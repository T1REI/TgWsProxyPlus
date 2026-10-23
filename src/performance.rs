use crate::network::Stream;
use anyhow::{Result, bail};
use futures_util::{StreamExt, future::BoxFuture, stream::FuturesUnordered};
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    time::sleep,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Direct,
    Worker,
    H2,
    WebSocket,
    Tcp,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Direct => "Telegram WS",
            Self::Worker => "Worker WS",
            Self::H2 => "CF H2",
            Self::WebSocket => "CF WS",
            Self::Tcp => "TCP",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Route {
    pub dc: u16,
    pub media: bool,
    pub test: bool,
    pub kind: Kind,
    pub endpoint: String,
}

struct Entry {
    route: Route,
    connect_ms: u64,
    first_ms: u64,
    kib_s: u64,
    failures: u32,
    errors: u64,
    successes: u64,
    active: u32,
    down: u64,
    up: u64,
    retry: Instant,
    touched: Instant,
}

#[derive(Default)]
pub struct Metrics(Mutex<Vec<Entry>>);

fn average(old: u64, sample: u64) -> u64 {
    if old == 0 {
        sample
    } else {
        (old * 3 + sample) / 4
    }
}

impl Metrics {
    fn update(&self, route: &Route, action: impl FnOnce(&mut Entry)) {
        let Ok(mut entries) = self.0.lock() else {
            return;
        };
        if !entries.iter().any(|entry| entry.route == *route) {
            if entries.len() == 128 {
                let Some(index) = entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.active == 0)
                    .min_by_key(|(_, e)| e.touched)
                    .map(|(i, _)| i)
                else {
                    return;
                };
                entries.swap_remove(index);
            }
            entries.push(Entry {
                route: route.clone(),
                connect_ms: 0,
                first_ms: 0,
                kib_s: 0,
                failures: 0,
                errors: 0,
                successes: 0,
                active: 0,
                down: 0,
                up: 0,
                retry: Instant::now(),
                touched: Instant::now(),
            });
        }
        if let Some(entry) = entries.iter_mut().find(|e| e.route == *route) {
            entry.touched = Instant::now();
            action(entry);
        }
    }

    pub fn connected(&self, route: &Route, elapsed: Duration) {
        self.update(route, |e| {
            e.connect_ms = average(e.connect_ms, elapsed.as_millis().max(1) as u64);
            e.failures = 0;
            e.retry = Instant::now();
            e.successes += 1;
        });
    }

    pub fn failed(&self, route: &Route) {
        self.update(route, |e| {
            e.errors += 1;
            e.failures = (e.failures + 1).min(6);
            e.retry = Instant::now() + Duration::from_secs((1 << e.failures).min(60));
        });
    }

    pub fn known(&self, route: &Route) -> bool {
        self.0
            .lock()
            .is_ok_and(|entries| entries.iter().any(|e| e.route == *route && e.successes > 0))
    }

    pub fn score(&self, route: &Route) -> Option<u64> {
        let default = match route.kind {
            Kind::Direct => 100,
            Kind::Worker => 150,
            Kind::H2 => 200,
            Kind::WebSocket => 250,
            Kind::Tcp => 300,
        };
        let Ok(entries) = self.0.lock() else {
            return Some(default);
        };
        let Some(e) = entries.iter().find(|e| e.route == *route) else {
            return Some(default);
        };
        if Instant::now() < e.retry {
            return None;
        }
        let transfer = if route.media && e.kib_s > 0 {
            256_000 / e.kib_s.max(1)
        } else {
            0
        };
        Some(
            e.connect_ms.max(1)
                + e.first_ms.min(2000) / 4
                + transfer
                + u64::from(e.active) * 25
                + e.errors * 500 / (e.errors + e.successes).max(1),
        )
    }

    pub fn observe(self: &Arc<Self>, route: Route, inner: Stream, started: Instant) -> Stream {
        self.update(&route, |e| e.active += 1);
        Box::new(Observed {
            inner,
            metrics: self.clone(),
            route,
            started,
            first: None,
            last: None,
            active_time: Duration::ZERO,
            burst_bytes: 0,
            up: 0,
            down: 0,
            reported_up: 0,
            reported_down: 0,
            reported_at: Instant::now(),
        })
    }

    pub fn report(&self) -> Vec<String> {
        let Ok(entries) = self.0.lock() else {
            return vec![];
        };
        entries.iter().filter(|e| e.successes > 0).map(|e| format!("DC{} media={} {} {}: connect={}ms first={}ms rate={}KiB/s active={} connects={} errors={} up={} down={}", e.route.dc, e.route.media, e.route.kind.name(), crate::logging::censor(&e.route.endpoint), e.connect_ms, e.first_ms, e.kib_s, e.active, e.successes, e.errors, e.up, e.down)).collect()
    }
}

struct Observed {
    inner: Stream,
    metrics: Arc<Metrics>,
    route: Route,
    started: Instant,
    first: Option<Duration>,
    last: Option<Instant>,
    active_time: Duration,
    burst_bytes: u64,
    up: u64,
    down: u64,
    reported_up: u64,
    reported_down: u64,
    reported_at: Instant,
}

impl Observed {
    fn publish(&mut self) {
        let millis = self.active_time.as_millis() as u64;
        self.metrics.update(&self.route, |e| {
            e.up += self.up - self.reported_up;
            e.down += self.down - self.reported_down;
            if self.burst_bytes >= 64 * 1024 && millis >= 10 {
                e.kib_s = average(
                    e.kib_s,
                    self.burst_bytes.saturating_mul(1000) / 1024 / millis,
                );
            }
        });
        self.reported_up = self.up;
        self.reported_down = self.down;
        self.reported_at = Instant::now();
        self.active_time = Duration::ZERO;
        self.burst_bytes = 0;
    }
    fn received(&mut self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        let now = Instant::now();
        let first = self.first.is_none();
        self.first.get_or_insert(now - self.started);
        if let Some(previous) = self.last {
            let elapsed = now - previous;
            if elapsed <= Duration::from_secs(1) {
                self.active_time += elapsed;
                self.burst_bytes += bytes as u64;
            }
        }
        self.last = Some(now);
        self.down += bytes as u64;
        if self.reported_at.elapsed() >= Duration::from_secs(1) {
            self.publish();
        }
        if first {
            self.metrics.update(&self.route, |e| {
                e.first_ms = average(
                    e.first_ms,
                    self.first.unwrap_or_default().as_millis() as u64,
                )
            });
        }
    }
}

impl AsyncRead for Observed {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        self.up += (buf.filled().len() - before) as u64;
        if buf.filled().len() > before && self.reported_at.elapsed() >= Duration::from_secs(1) {
            self.publish();
        }
        result
    }
}
impl AsyncWrite for Observed {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = result {
            self.received(n);
        }
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
impl Drop for Observed {
    fn drop(&mut self) {
        self.publish();
        self.metrics
            .update(&self.route, |e| e.active = e.active.saturating_sub(1));
    }
}

pub async fn race<'a, T: 'a>(
    attempts: impl IntoIterator<Item = BoxFuture<'a, Result<T>>>,
    delay: Duration,
) -> Result<T> {
    let mut waiting = attempts.into_iter();
    let mut running = FuturesUnordered::new();
    if let Some(first) = waiting.next() {
        running.push(first);
    }
    let timer = sleep(delay);
    tokio::pin!(timer);
    let mut exhausted = false;
    let mut last_error = None;
    loop {
        tokio::select! {
            result = running.next(), if !running.is_empty() => {
                match result { Some(Ok(value)) => return Ok(value), Some(Err(error)) => last_error = Some(error), None => {} }
                if let Some(next) = waiting.next() { running.push(next); } else { exhausted = true; }
                timer.as_mut().reset(tokio::time::Instant::now() + delay);
            }
            _ = &mut timer, if !exhausted && running.len() < 2 => {
                if let Some(next) = waiting.next() { running.push(next); } else { exhausted = true; }
                timer.as_mut().reset(tokio::time::Instant::now() + delay);
            }
            else => break,
        }
    }
    if let Some(error) = last_error {
        Err(error)
    } else {
        bail!("no available routes; retry after cooldown")
    }
}
