use crate::{
    crypto::{Cipher, ClientInit, Protocol},
    framing::{self, MAX_PACKET},
    h2_transport::{HttpLane, Response},
    network::{Stream, WRITE_TIMEOUT},
    stats::Stats,
};
use anyhow::{Context, Result, bail, ensure};
use bytes::Bytes;
use ctr::cipher::StreamCipher;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
    task::{AbortHandle, Id, JoinSet},
    time::timeout,
};

async fn read_packet<R: AsyncRead + Unpin>(
    reader: &mut R,
    decrypt: &mut Cipher,
    protocol: Protocol,
) -> Result<Bytes> {
    let mut header = [0; 4];
    let initial = if protocol == Protocol::Abridged { 1 } else { 4 };
    reader.read_exact(&mut header[..initial]).await?;
    decrypt.apply_keystream(&mut header[..initial]);
    if initial == 1 && header[0] & 0x7f == 0x7f {
        reader.read_exact(&mut header[1..]).await?;
        decrypt.apply_keystream(&mut header[1..]);
    }
    let (_, length, _) =
        framing::packet_length(protocol, &header)?.context("incomplete MTProto header")?;
    ensure!(length >= 24, "HTTP MTProto packet too short");
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    decrypt.apply_keystream(&mut body);
    framing::strip_padding(protocol, body.into())
}

struct History {
    id: u64,
    body: Bytes,
    started: Instant,
    last_attempt: Instant,
    attempts: u8,
    retired: bool,
    reply: Option<Bytes>,
}

struct Pending {
    packet: u64,
    replay: bool,
    bytes: usize,
    started: Instant,
    progress: Arc<AtomicU8>,
    abort: AbortHandle,
}

impl Pending {
    fn waiting_headers(&self, age: Duration) -> bool {
        self.progress.load(Relaxed) == 1 && self.started.elapsed() >= age
    }
}

fn request(
    jobs: &mut JoinSet<Result<Response>>,
    pending: &mut BTreeMap<Id, Pending>,
    lane: Arc<HttpLane>,
    body: Bytes,
    packet: u64,
    replay: bool,
) {
    let progress = Arc::new(AtomicU8::new(0));
    let trace = progress.clone();
    let length = body.len();
    let abort = jobs.spawn(async move {
        lane.request_traced(http::Method::POST, "/api", body, MAX_PACKET, Some(trace))
            .await
    });
    pending.insert(
        abort.id(),
        Pending {
            packet,
            replay,
            bytes: length,
            started: Instant::now(),
            progress,
            abort,
        },
    );
}

pub fn transport_error(response: &Response) -> Option<i32> {
    if matches!(response.status, 403 | 404 | 429 | 444) {
        return Some(-(response.status as i32));
    }
    if response.status == 200 && response.body.len() == 4 {
        let code = i32::from_le_bytes(response.body[..4].try_into().expect("four-byte body"));
        if code < 0 {
            return Some(code);
        }
    }
    None
}

pub async fn bridge_h2(
    client: Stream,
    lane: Arc<HttpLane>,
    init: ClientInit,
    stats: Arc<Stats>,
) -> Result<()> {
    let protocol = init.protocol;
    let (mut reader, mut writer) = tokio::io::split(client);
    let mut decrypt = init.decrypt;
    let mut encrypt = init.encrypt;
    let (tx, mut rx) = mpsc::channel(2);
    let upload = async {
        loop {
            let body = match read_packet(&mut reader, &mut decrypt, protocol).await {
                Ok(body) => body,
                Err(error) => break Err::<(), _>(error),
            };
            if let Err(error) = tx.send(body).await {
                break Err(error.into());
            }
        }
    };
    let dispatcher = async {
        let mut jobs = JoinSet::new();
        let mut pending = BTreeMap::<Id, Pending>::new();
        let mut history = VecDeque::<History>::new();
        let mut next = 0u64;
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        let mut last_progress = Instant::now();
        loop {
            let pending_bytes: usize = pending.values().map(|v| v.bytes).sum();
            tokio::select! {
                Some(body) = rx.recv(), if pending.len() < 8 && pending_bytes <= 4 * 1024 * 1024 => {
                    next += 1;
                    stats.up.fetch_add(body.len() as u64, Relaxed);
                    if body[..8] != [0; 8] && body.len() <= 64 * 1024 {
                        if history.back().is_some_and(|v| v.body[..8] != body[..8]) { history.clear(); }
                        history.push_back(History { id: next, body: body.clone(), started: Instant::now(), last_attempt: Instant::now() - Duration::from_secs(2), attempts: 0, retired: false, reply: None });
                        while history.len() > 16 || history.iter().map(|v| v.body.len()).sum::<usize>() > 64 * 1024 { history.pop_front(); }
                    }
                    request(&mut jobs, &mut pending, lane.clone(), body, next, false);
                }
                Some(result) = jobs.join_next_with_id(), if !jobs.is_empty() => {
                    let (id, result) = match result {
                        Ok(value) => value,
                        Err(error) if error.is_cancelled() => { pending.remove(&error.id()); continue; }
                        Err(error) => return Err(error.into()),
                    };
                    let Some(request) = pending.remove(&id) else { continue; };
                    let response = result?;
                    if let Some(code) = transport_error(&response) {
                        let mut encoded = framing::encode_reply(protocol, &code.to_le_bytes())?; encrypt.apply_keystream(&mut encoded);
                        timeout(WRITE_TIMEOUT, writer.write_all(&encoded)).await??; timeout(WRITE_TIMEOUT, writer.flush()).await??;
                        bail!("MTProto endpoint error {code}");
                    }
                    ensure!(response.status == 200, "HTTP MTProto returned {}", response.status);
                    ensure!(!response.headers.get("content-type").is_some_and(|v| v.to_str().unwrap_or("").to_ascii_lowercase().contains("text/html")), "HTTP endpoint returned HTML");
                    if !response.body.is_empty() {
                        if let Some(entry) = history.iter_mut().find(|v| v.id == request.packet) {
                            let fingerprint = response.body.slice(..response.body.len().min(24));
                            if entry.reply.as_ref() != Some(&fingerprint) { entry.reply = Some(Bytes::copy_from_slice(&fingerprint)); entry.attempts = 0; }
                        }
                        let mut encoded = framing::encode_reply(protocol, &response.body)?; encrypt.apply_keystream(&mut encoded);
                        timeout(WRITE_TIMEOUT, writer.write_all(&encoded)).await??; timeout(WRITE_TIMEOUT, writer.flush()).await??;
                        stats.down.fetch_add(response.body.len() as u64, Relaxed);
                    }
                    last_progress = Instant::now();
                }
                _ = tick.tick() => {
                    let upload_waiting = (!rx.is_empty() && (pending.len() >= 8 || pending_bytes > 4 * 1024 * 1024))
                        || pending.values().any(|request| request.progress.load(Relaxed) == 0);
                    let candidate = pending.iter().find(|(_, request)| {
                        request.waiting_headers(if request.replay { Duration::from_secs(30) } else { Duration::from_secs(3) })
                            && (request.replay || (upload_waiting && history.iter().any(|v| v.id == request.packet && v.started.elapsed() <= Duration::from_secs(30))))
                    }).map(|(id, _)| *id);
                    if let Some(id) = candidate && let Some(request) = pending.remove(&id) {
                        request.abort.abort();
                        if let Some(entry) = history.iter_mut().find(|v| v.id == request.packet) { entry.retired = true; }
                    }
                    if upload_waiting || pending.values().filter(|v| v.replay).count() >= 2 || pending.len() >= 8 || pending_bytes > 8 * 1024 * 1024 { continue; }
                    let idle = pending.is_empty() && last_progress.elapsed() >= Duration::from_secs(1);
                    let last_id = history.back().map(|v| v.id);
                    let candidate = history.iter_mut().find(|entry| {
                        let related: Vec<_> = pending.values().filter(|v| v.packet == entry.id).collect();
                        entry.attempts < 3 && entry.started.elapsed() <= Duration::from_secs(30) && entry.last_attempt.elapsed() >= Duration::from_secs(2)
                            && !related.iter().any(|v| v.replay || v.progress.load(Relaxed) >= 2)
                            && (entry.retired || related.iter().any(|v| v.waiting_headers(Duration::from_secs(3))) || (idle && Some(entry.id) == last_id))
                    });
                    if let Some(entry) = candidate {
                        entry.attempts += 1; entry.last_attempt = Instant::now();
                        stats.replays.fetch_add(1, Relaxed);
                        request(&mut jobs, &mut pending, lane.clone(), entry.body.clone(), entry.id, true);
                    }
                }
            }
        }
    };
    tokio::select! { result = upload => result, result = dispatcher => result }
}
