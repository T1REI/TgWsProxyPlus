use crate::{
    config::{Config, parse_dc_ips},
    network::{Network, WebSocket},
    stats::Stats,
};
use anyhow::Result;
use futures_util::{FutureExt, StreamExt};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};
use tokio::{sync::Mutex, task::JoinSet};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Key {
    Direct(u16, bool, bool),
    Worker(u16, bool),
}

struct Bucket {
    idle: VecDeque<(WebSocket, Instant)>,
    filling: bool,
    failures: u32,
    retry: Instant,
}

pub struct Pool {
    buckets: Mutex<BTreeMap<Key, Bucket>>,
    config: Arc<Config>,
    network: Arc<Network>,
    stats: Arc<Stats>,
    fronting_first: AtomicBool,
}

pub fn ws_domains(dc: u16, media: bool) -> Vec<String> {
    let dc = if dc == 203 { 2 } else { dc };
    let mut domains = Vec::new();
    if media {
        domains.push(format!("kws{dc}-1.web.telegram.org"));
    }
    domains.push(format!("kws{dc}.web.telegram.org"));
    domains
}

impl Pool {
    pub fn new(config: Arc<Config>, network: Arc<Network>, stats: Arc<Stats>) -> Arc<Self> {
        Arc::new(Self {
            buckets: Mutex::new(BTreeMap::new()),
            config,
            network,
            stats,
            fronting_first: AtomicBool::new(false),
        })
    }

    pub async fn take(&self, key: Key) -> Option<WebSocket> {
        let mut buckets = self.buckets.lock().await;
        let bucket = buckets.entry(key).or_insert_with(|| Bucket {
            idle: VecDeque::new(),
            filling: false,
            failures: 0,
            retry: Instant::now(),
        });
        while let Some((mut socket, created)) = bucket.idle.pop_front() {
            if created.elapsed() >= Duration::from_secs(100) {
                continue;
            }
            if socket.next().now_or_never().is_some() {
                continue;
            }
            self.stats.pool_hits.fetch_add(1, Relaxed);
            return Some(socket);
        }
        self.stats.pool_misses.fetch_add(1, Relaxed);
        None
    }

    pub async fn connect(&self, key: Key) -> Result<WebSocket> {
        match key {
            Key::Direct(dc, media, test) => {
                let redirects = parse_dc_ips(&self.config.dc_ip)?;
                let ip = redirects
                    .get(&dc)
                    .ok_or_else(|| anyhow::anyhow!("no direct WS mapping"))?
                    .to_string();
                let path = if test { "/apiws_test" } else { "/apiws" };
                for domain in ws_domains(dc, media) {
                    let first = self.fronting_first.load(Relaxed);
                    for fronting in [first, !first] {
                        match self
                            .network
                            .websocket(&ip, &domain, path, true, fronting)
                            .await
                        {
                            Ok(socket) => {
                                self.fronting_first.store(fronting, Relaxed);
                                if fronting {
                                    self.stats.fronting.fetch_add(1, Relaxed);
                                }
                                return Ok(socket);
                            }
                            Err(error) => {
                                self.stats.errors.fetch_add(1, Relaxed);
                                log::debug!("direct WS DC{dc}: {error}");
                            }
                        }
                    }
                }
                anyhow::bail!("direct WS routes unavailable for DC{dc}")
            }
            Key::Worker(dc, test) => {
                let ip = crate::routing::dc_ip(dc, test)
                    .ok_or_else(|| anyhow::anyhow!("unsupported Worker DC"))?;
                let path = format!("/apiws?dst={ip}&dc={dc}");
                for domain in self.config.worker_domains() {
                    match self
                        .network
                        .websocket(domain, domain, &path, !self.config.no_secure, false)
                        .await
                    {
                        Ok(socket) => return Ok(socket),
                        Err(error) => log::debug!("Worker DC{dc}: {error}"),
                    }
                }
                anyhow::bail!("Worker routes unavailable for DC{dc}")
            }
        }
    }

    pub async fn maintain(self: Arc<Self>) {
        let mut jobs = JoinSet::new();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if self.config.pool_size == 0 { continue; }
                    let Ok(redirects) = parse_dc_ips(&self.config.dc_ip) else { return; };
                    let mut keys = Vec::new();
                    for dc in redirects.keys() { for media in [false, true] { keys.push(Key::Direct(*dc, media, self.config.force_test_dc)); } }
                    if !self.config.worker_domains().is_empty() { for dc in [1,2,3,4,5,203] { if !redirects.contains_key(&dc) && (!self.config.force_test_dc || dc <= 3) { keys.push(Key::Worker(dc, self.config.force_test_dc)); } } }
                    let mut buckets = self.buckets.lock().await;
                    for key in keys {
                        let bucket = buckets.entry(key).or_insert_with(|| Bucket { idle: VecDeque::new(), filling: false, failures: 0, retry: Instant::now() });
                        bucket.idle.retain(|(_, created)| created.elapsed() < Duration::from_secs(100));
                        let target = if matches!(key, Key::Worker(..)) { 1 } else { self.config.pool_size };
                        if bucket.filling || bucket.idle.len() >= target || Instant::now() < bucket.retry { continue; }
                        bucket.filling = true;
                        let count = target - bucket.idle.len();
                        let pool = self.clone();
                        jobs.spawn(async move {
                            let mut connects = JoinSet::new();
                            for _ in 0..count { let pool = pool.clone(); connects.spawn(async move { pool.connect(key).await }); }
                            let mut ready = VecDeque::new();
                            while let Some(result) = connects.join_next().await { if let Ok(Ok(socket)) = result { ready.push_back((socket, Instant::now())); } }
                            let mut buckets = pool.buckets.lock().await;
                            if let Some(bucket) = buckets.get_mut(&key) {
                                bucket.filling = false;
                                if ready.is_empty() { bucket.failures = (bucket.failures + 1).min(13); bucket.retry = Instant::now() + Duration::from_secs((1u64 << (bucket.failures - 1)).min(3600)); }
                                else { bucket.failures = 0; bucket.idle.extend(ready); }
                            }
                        });
                    }
                }
                Some(result) = jobs.join_next(), if !jobs.is_empty() => { if let Err(error) = result { log::error!("pool task failed: {error}"); } }
            }
        }
    }
}
