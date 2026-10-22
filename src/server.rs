use crate::{
    config::Config,
    crypto::{self, Cipher, ClientInit, RelayInit},
    fake_tls::{self, FakeTls},
    framing::Splitter,
    network::{Network, Stream, WRITE_TIMEOUT, WebSocket},
    routing::{Routing, Upstream},
    stats::Stats,
};
use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use ctr::cipher::StreamCipher;
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, atomic::Ordering::Relaxed},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, Semaphore},
    task::JoinSet,
    time::timeout,
};
use tokio_tungstenite::tungstenite::Message;

pub async fn run(config: Config, shutdown: impl Future<Output = ()>) -> Result<()> {
    config.validate()?;
    let listener = TcpListener::bind((config.host.as_str(), config.port))
        .await
        .with_context(|| {
            format!(
                "cannot listen on {}:{}; check the address and whether another proxy is running",
                config.host, config.port
            )
        })?;
    let network = Network::new(config.buf_kb * 1024)?;
    let routing = Routing::new(Arc::new(config), network, Arc::new(Stats::default()));
    serve(listener, routing, shutdown).await
}

pub async fn serve(
    listener: TcpListener,
    routing: Arc<Routing>,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let replay_cache = Arc::new(Mutex::new(BTreeMap::<[u8; 32], u64>::new()));
    let capacity = Arc::new(Semaphore::new(routing.config.max_connections));
    let mut clients = JoinSet::new();
    let mut background = JoinSet::new();
    background.spawn(routing.pool.clone().maintain());
    if routing.config.cfproxy {
        background.spawn(routing.clone().refresh_domains());
    }
    let mut stats_tick = tokio::time::interval(Duration::from_secs(60));
    stats_tick.tick().await;
    log::info!(
        "{} {} by {} listening on {}",
        crate::NAME,
        crate::VERSION,
        crate::AUTHOR,
        listener.local_addr()?
    );
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = stats_tick.tick() => log::info!("stats: {}", routing.stats.summary()),
            Some(result) = clients.join_next(), if !clients.is_empty() => { if let Err(error) = result { log::error!("client task failed: {error}"); } }
            Some(result) = background.join_next(), if !background.is_empty() => { if let Err(error) = result { log::error!("background task failed: {error}"); } }
            accepted = listener.accept() => {
                let (stream, peer) = match accepted { Ok(value) => value, Err(error) => { log::warn!("accept failed: {error}"); tokio::time::sleep(Duration::from_secs(1)).await; continue; } };
                let Ok(permit) = capacity.clone().try_acquire_owned() else { log::debug!("connection limit reached"); continue; };
                let routing = routing.clone(); let cache = replay_cache.clone();
                clients.spawn(async move {
                    let _permit = permit;
                    let stats = routing.stats.clone();
                    stats.total.fetch_add(1, Relaxed); stats.active.fetch_add(1, Relaxed);
                    struct Active(Arc<Stats>);
                    impl Drop for Active { fn drop(&mut self) { self.0.active.fetch_sub(1, Relaxed); } }
                    let _active = Active(stats);
                    if let Err(error) = handle_client(stream, &routing, &cache).await { log::debug!("client {peer}: {error:#}"); }
                });
            }
        }
    }
    clients.shutdown().await;
    background.shutdown().await;
    log::info!("stopped: {}", routing.stats.summary());
    Ok(())
}

async fn proxy_header(stream: &mut TcpStream) -> Result<()> {
    let mut line = Vec::new();
    while !line.ends_with(b"\r\n") {
        ensure!(line.len() < 108, "PROXY v1 header exceeds 108 bytes");
        line.push(stream.read_u8().await?);
    }
    let text = std::str::from_utf8(&line)?;
    let parts: Vec<_> = text.split_whitespace().collect();
    if parts == ["PROXY", "UNKNOWN"] {
        return Ok(());
    }
    ensure!(
        parts.len() == 6 && parts[0] == "PROXY" && matches!(parts[1], "TCP4" | "TCP6"),
        "invalid PROXY v1 header"
    );
    for value in &parts[2..4] {
        let ip = value.parse::<std::net::IpAddr>()?;
        ensure!(
            ip.is_ipv4() == (parts[1] == "TCP4"),
            "PROXY address family mismatch"
        );
    }
    parts[4].parse::<u16>()?;
    parts[5].parse::<u16>()?;
    Ok(())
}

async fn client_stream(
    mut stream: TcpStream,
    routing: &Routing,
    cache: &Mutex<BTreeMap<[u8; 32], u64>>,
) -> Result<Option<(Stream, [u8; 64])>> {
    routing.network.configure(&stream)?;
    let config = &routing.config;
    if config.proxy_protocol {
        timeout(Duration::from_secs(10), proxy_header(&mut stream)).await??;
    }
    let first = timeout(Duration::from_secs(10), stream.read_u8()).await??;
    if config.fake_tls_domain.is_empty() {
        let mut init = [0; 64];
        init[0] = first;
        timeout(Duration::from_secs(10), stream.read_exact(&mut init[1..])).await??;
        return Ok(Some((Box::new(stream), init)));
    }
    if first != 0x16 {
        let response = format!(
            "HTTP/1.1 301 Moved Permanently\r\nLocation: https://{}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            config.fake_tls_domain
        );
        timeout(WRITE_TIMEOUT, stream.write_all(response.as_bytes())).await??;
        return Ok(None);
    }
    let mut header = [0x16, 0, 0, 0, 0];
    timeout(Duration::from_secs(10), stream.read_exact(&mut header[1..])).await??;
    let length = u16::from_be_bytes([header[3], header[4]]) as usize;
    ensure!(length <= 18432, "oversized TLS ClientHello");
    let mut hello = header.to_vec();
    hello.resize(5 + length, 0);
    timeout(Duration::from_secs(10), stream.read_exact(&mut hello[5..])).await??;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let secret = config.secret_bytes()?;
    let verified = fake_tls::verify_hello(&hello, &secret, now);
    let Ok(hello_verified) = verified else {
        let mut remote = routing.network.tcp(&config.fake_tls_domain, 443).await?;
        timeout(WRITE_TIMEOUT, remote.write_all(&hello)).await??;
        timeout(
            Duration::from_secs(120),
            tokio::io::copy_bidirectional(&mut stream, &mut remote),
        )
        .await??;
        return Ok(None);
    };
    {
        let mut cache = cache.lock().await;
        cache.retain(|_, timestamp| now.abs_diff(*timestamp) <= 120);
        ensure!(
            !cache.contains_key(&hello_verified.random),
            "replayed Fake TLS ClientHello"
        );
        ensure!(cache.len() < 4096, "Fake TLS replay cache is full");
        cache.insert(hello_verified.random, hello_verified.timestamp as u64);
    }
    timeout(
        WRITE_TIMEOUT,
        stream.write_all(&fake_tls::server_hello(&secret, &hello_verified)?),
    )
    .await??;
    let mut tls = FakeTls::new(stream);
    let mut init = [0; 64];
    timeout(Duration::from_secs(10), tls.read_exact(&mut init)).await??;
    Ok(Some((Box::new(tls), init)))
}

async fn handle_client(
    stream: TcpStream,
    routing: &Routing,
    cache: &Mutex<BTreeMap<[u8; 32], u64>>,
) -> Result<()> {
    let Some((stream, init)) = client_stream(stream, routing, cache).await? else {
        return Ok(());
    };
    let init = match crypto::accept_init(
        &init,
        &routing.config.secret_bytes()?,
        routing.config.force_test_dc,
    ) {
        Ok(init) => init,
        Err(error) => {
            routing.stats.bad.fetch_add(1, Relaxed);
            return Err(error);
        }
    };
    let relay = crypto::relay_init(init.protocol, init.dc, init.media)?;
    let upstream = routing.choose(&init).await?;
    match upstream {
        Upstream::Tcp(mut remote) => {
            timeout(WRITE_TIMEOUT, remote.write_all(&relay.bytes)).await??;
            bridge_tcp(stream, Box::new(remote), init, relay, &routing.stats).await
        }
        Upstream::WebSocket { socket, split } => {
            bridge_ws(stream, *socket, init, relay, split, &routing.stats).await
        }
        Upstream::H2(lane) => {
            let result = bridge_h2(stream, lane.clone(), init, routing.stats.clone()).await;
            if result.is_err() {
                routing.invalidate_h2(&lane).await;
            }
            result
        }
    }
}

async fn forward<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    mut reader: R,
    mut writer: W,
    mut decrypt: Cipher,
    mut encrypt: Cipher,
    count: &std::sync::atomic::AtomicU64,
) -> Result<()> {
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let n = reader.read(&mut buffer).await?;
        if n == 0 {
            writer.shutdown().await?;
            return Ok(());
        }
        decrypt.apply_keystream(&mut buffer[..n]);
        encrypt.apply_keystream(&mut buffer[..n]);
        timeout(WRITE_TIMEOUT, writer.write_all(&buffer[..n])).await??;
        timeout(WRITE_TIMEOUT, writer.flush()).await??;
        count.fetch_add(n as u64, Relaxed);
    }
}

pub async fn bridge_tcp(
    client: Stream,
    remote: Stream,
    init: ClientInit,
    relay: RelayInit,
    stats: &Stats,
) -> Result<()> {
    let (reader, writer) = tokio::io::split(client);
    let (remote_reader, remote_writer) = tokio::io::split(remote);
    tokio::select! {
        result = forward(reader, remote_writer, init.decrypt, relay.encrypt, &stats.up) => result,
        result = forward(remote_reader, writer, relay.decrypt, init.encrypt, &stats.down) => result,
    }
}

pub async fn bridge_ws(
    client: Stream,
    mut socket: WebSocket,
    mut init: ClientInit,
    mut relay: RelayInit,
    split: bool,
    stats: &Stats,
) -> Result<()> {
    timeout(
        WRITE_TIMEOUT,
        socket.send(Message::Binary(Bytes::copy_from_slice(&relay.bytes))),
    )
    .await??;
    let (mut sink, mut source) = socket.split();
    let (mut reader, mut writer) = tokio::io::split(client);
    let upload = async {
        let mut buffer = vec![0; 64 * 1024];
        let mut splitter = Splitter::new(init.protocol);
        loop {
            let n = reader.read(&mut buffer).await?;
            if n == 0 {
                let tail = splitter.flush();
                if !tail.is_empty() {
                    timeout(WRITE_TIMEOUT, sink.send(Message::Binary(tail))).await??;
                }
                timeout(WRITE_TIMEOUT, sink.close()).await??;
                return Ok::<_, anyhow::Error>(());
            }
            init.decrypt.apply_keystream(&mut buffer[..n]);
            let plain = if split {
                buffer[..n].to_vec()
            } else {
                Vec::new()
            };
            relay.encrypt.apply_keystream(&mut buffer[..n]);
            let parts = if split {
                splitter.push(&buffer[..n], &plain)?
            } else {
                vec![Bytes::copy_from_slice(&buffer[..n])]
            };
            for part in parts {
                timeout(WRITE_TIMEOUT, sink.feed(Message::Binary(part))).await??;
            }
            timeout(WRITE_TIMEOUT, sink.flush()).await??;
            stats.up.fetch_add(n as u64, Relaxed);
        }
    };
    let download = async {
        while let Some(message) = source.next().await {
            let data = match message? {
                Message::Binary(data) => data,
                Message::Text(data) => Bytes::copy_from_slice(data.as_bytes()),
                Message::Close(_) => break,
                _ => continue,
            };
            let mut data = data.to_vec();
            relay.decrypt.apply_keystream(&mut data);
            init.encrypt.apply_keystream(&mut data);
            timeout(WRITE_TIMEOUT, writer.write_all(&data)).await??;
            timeout(WRITE_TIMEOUT, writer.flush()).await??;
            stats.down.fetch_add(data.len() as u64, Relaxed);
        }
        Ok::<_, anyhow::Error>(())
    };
    tokio::select! { result = upload => result, result = download => result }
}

pub use crate::h2_channel::{bridge_h2, transport_error};

pub async fn diagnose(config: Config) -> Result<Vec<String>> {
    config.validate()?;
    let network = Network::new(config.buf_kb * 1024)?;
    let routing = Routing::new(Arc::new(config), network, Arc::new(Stats::default()));
    let redirects = crate::config::parse_dc_ips(&routing.config.dc_ip)?;
    let mut jobs = JoinSet::new();
    fn probe(
        jobs: &mut JoinSet<(String, bool)>,
        name: String,
        action: impl Future<Output = Result<()>> + Send + 'static,
    ) {
        jobs.spawn(async move {
            let ok = matches!(timeout(Duration::from_secs(15), action).await, Ok(Ok(())));
            (name, ok)
        });
    }
    for dc in [1, 2, 3, 4, 5, 203] {
        let test = routing.config.force_test_dc;
        if test && dc > 3 {
            continue;
        }
        if redirects.contains_key(&dc) && routing.config.pool_size > 0 {
            let r = routing.clone();
            probe(&mut jobs, format!("DC{dc:03} direct WS"), async move {
                r.pool
                    .connect(crate::pool::Key::Direct(dc, false, test))
                    .await
                    .map(|_| ())
            });
        }
        if !routing.config.worker_domains().is_empty() {
            let r = routing.clone();
            probe(&mut jobs, format!("DC{dc:03} Worker WS"), async move {
                r.pool
                    .connect(crate::pool::Key::Worker(dc, test))
                    .await
                    .map(|_| ())
            });
        }
        if routing.config.cfproxy && !test {
            let domains = routing.domains.lock().await.ordered(dc);
            let r = routing.clone();
            let ws_domains = domains.clone();
            probe(&mut jobs, format!("DC{dc:03} CF WS"), async move {
                for base in ws_domains {
                    let host = format!("kws{dc}.{base}");
                    if r.network
                        .websocket(&host, &host, "/apiws", !r.config.no_secure, false)
                        .await
                        .is_ok()
                    {
                        return Ok(());
                    }
                }
                anyhow::bail!("no CF WebSocket route")
            });
            if routing.config.h2_enabled() {
                let r = routing.clone();
                probe(&mut jobs, format!("DC{dc:03} CF H2"), async move {
                    for base in domains {
                        let host = format!("kws{dc}.{base}");
                        if let Ok(lane) = crate::h2_transport::HttpLane::connect(
                            &r.network,
                            &host,
                            r.stats.clone(),
                        )
                        .await
                            && lane.preflight().await.is_ok()
                        {
                            return Ok(());
                        }
                    }
                    anyhow::bail!("no CF HTTP/2 route")
                });
            }
        }
        if let Some(ip) = crate::routing::dc_ip(dc, test) {
            let r = routing.clone();
            probe(&mut jobs, format!("DC{dc:03} TCP"), async move {
                r.network.tcp(ip, 443).await.map(|_| ())
            });
        }
    }
    let mut results = Vec::new();
    while let Some(result) = jobs.join_next().await {
        let (name, ok) = result?;
        results.push(format!("{name}: {}", if ok { "OK" } else { "unavailable" }));
    }
    results.sort();
    Ok(results)
}
