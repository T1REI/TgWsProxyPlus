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
use tokio::{
    sync::{Mutex, Notify},
    task::JoinSet,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Key {
    Direct(u16, bool, bool),
    Worker(u16, bool),
}

struct Bucket {
    idle: VecDeque<(WebSocket, Instant)>,
    filling: usize,
    used: Instant,
    demand: usize,
    failures: u32,
    retry: Instant,
}

impl Bucket {
    fn target(&mut self, limit: usize) -> usize {
        self.idle
            .retain(|(_, created)| created.elapsed() < Duration::from_secs(100));
        if self.used.elapsed() >= Duration::from_secs(30) {
            self.demand = 1;
            self.idle.truncate(1);
        }
        if self.used.elapsed() >= Duration::from_secs(120) {
            self.demand = 0;
            self.idle.clear();
        }
        self.demand.min(limit)
    }
}

pub struct Pool {
    buckets: Mutex<BTreeMap<Key, Bucket>>,
    config: Arc<Config>,
    network: Arc<Network>,
    stats: Arc<Stats>,
    fronting_first: AtomicBool,
    wake: Notify,
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
            wake: Notify::new(),
        })
    }

    pub async fn take(&self, key: Key) -> Option<WebSocket> {
        let mut buckets = self.buckets.lock().await;
        let bucket = buckets.entry(key).or_insert_with(|| Bucket {
            idle: VecDeque::new(),
            filling: 0,
            used: Instant::now(),
            demand: 1,
            failures: 0,
            retry: Instant::now(),
        });
        bucket.used = Instant::now();
        bucket.demand = (bucket.demand + 1).min(self.config.pool_size);
        self.wake.notify_one();
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
        if self.config.pool_size == 0 {
            return;
        }
        let Ok(redirects) = parse_dc_ips(&self.config.dc_ip) else {
            return;
        };
        {
            let mut buckets = self.buckets.lock().await;
            let mut keys = Vec::new();
            for dc in redirects.keys() {
                for media in [false, true] {
                    keys.push(Key::Direct(*dc, media, self.config.force_test_dc));
                }
            }
            if !self.config.worker_domains().is_empty() {
                for dc in [1, 2, 3, 4, 5, 203] {
                    if !redirects.contains_key(&dc) && (!self.config.force_test_dc || dc <= 3) {
                        keys.push(Key::Worker(dc, self.config.force_test_dc));
                    }
                }
            }
            for key in keys {
                buckets.entry(key).or_insert_with(|| Bucket {
                    idle: VecDeque::new(),
                    filling: 0,
                    failures: 0,
                    retry: Instant::now(),
                    used: Instant::now(),
                    demand: 1,
                });
            }
        }
        let mut jobs = JoinSet::new();
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        loop {
            {
                let mut buckets = self.buckets.lock().await;
                for (key, bucket) in buckets.iter_mut() {
                    let target = bucket.target(self.config.pool_size);
                    while jobs.len() < 4
                        && bucket.idle.len() + bucket.filling < target
                        && Instant::now() >= bucket.retry
                    {
                        bucket.filling += 1;
                        let pool = self.clone();
                        let key = *key;
                        jobs.spawn(async move { (key, pool.connect(key).await) });
                    }
                }
            }
            tokio::select! {
                _ = tick.tick() => {},
                _ = self.wake.notified() => {},
                Some(result) = jobs.join_next(), if !jobs.is_empty() => {
                    match result {
                        Ok((key, result)) => self.completed(key, result).await,
                        Err(error) => log::error!("pool task failed: {error}"),
                    }
                }
            }
        }
    }

    async fn completed(&self, key: Key, result: Result<WebSocket>) {
        let mut buckets = self.buckets.lock().await;
        if let Some(bucket) = buckets.get_mut(&key) {
            bucket.filling = bucket.filling.saturating_sub(1);
            match result {
                Ok(socket) => {
                    bucket.failures = 0;
                    bucket.retry = Instant::now();
                    bucket.idle.push_back((socket, Instant::now()));
                }
                Err(_) => {
                    bucket.failures = (bucket.failures + 1).min(13);
                    bucket.retry = Instant::now()
                        + Duration::from_secs((1u64 << (bucket.failures - 1)).min(3600));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};

    #[tokio::test]
    async fn ready_socket_is_available_while_another_connection_is_still_pending() {
        let config = Arc::new(Config::fresh().unwrap());
        let pool = Pool::new(
            config,
            Network::new(4096).unwrap(),
            Arc::new(Stats::default()),
        );
        let key = Key::Direct(2, true, false);
        assert!(pool.take(key).await.is_none());
        {
            let mut buckets = pool.buckets.lock().await;
            buckets.get_mut(&key).unwrap().filling = 2;
        }
        let (stream, _peer) = tokio::io::duplex(1024);
        let socket = WebSocketStream::from_raw_socket(
            Box::new(stream) as crate::network::Stream,
            Role::Client,
            None,
        )
        .await;
        pool.completed(key, Ok(socket)).await;
        assert!(pool.take(key).await.is_some());
        assert_eq!(pool.buckets.lock().await[&key].filling, 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), pool.wake.notified())
                .await
                .is_ok()
        );
    }

    #[test]
    fn warm_pool_grows_on_demand_and_stops_reconnecting_when_idle() {
        let mut bucket = Bucket {
            idle: VecDeque::new(),
            filling: 0,
            used: Instant::now(),
            demand: 1,
            failures: 0,
            retry: Instant::now(),
        };
        assert_eq!(bucket.target(4), 1);
        bucket.demand = 4;
        assert_eq!(bucket.target(4), 4);
        bucket.used = Instant::now() - Duration::from_secs(31);
        assert_eq!(bucket.target(4), 1);
        bucket.used = Instant::now() - Duration::from_secs(121);
        assert_eq!(bucket.target(4), 0);
    }
}
