use crate::{
    config::{Config, valid_domain},
    crypto::ClientInit,
    h2_transport::{self, HttpLane},
    network::{Network, WebSocket},
    pool::{Key, Pool},
    stats::Stats,
};
use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::Ordering::Relaxed},
    time::{Duration, Instant},
};
use tokio::{
    net::TcpStream,
    sync::{Mutex, Semaphore},
    time::timeout,
};

pub fn dc_ip(dc: u16, test: bool) -> Option<&'static str> {
    match (test, dc) {
        (true, 1) => Some("149.154.175.10"),
        (true, 2) => Some("149.154.167.40"),
        (true, 3) => Some("149.154.175.117"),
        (false, 1) => Some("149.154.175.50"),
        (false, 2) => Some("149.154.167.51"),
        (false, 3) => Some("149.154.175.100"),
        (false, 4) => Some("149.154.167.91"),
        (false, 5) => Some("149.154.171.5"),
        (false, 203) => Some("91.105.192.100"),
        _ => None,
    }
}

pub fn decode_domain(domain: &str) -> String {
    let Some(prefix) = domain.strip_suffix(".com") else {
        return domain.into();
    };
    let shift = prefix.bytes().filter(|b| b.is_ascii_alphabetic()).count() as u8 % 26;
    let mut result: String = prefix
        .bytes()
        .map(|b| {
            if b.is_ascii_lowercase() {
                (b'a' + (b - b'a' + 26 - shift) % 26) as char
            } else {
                b as char
            }
        })
        .collect();
    result.push_str(".co.uk");
    result
}

pub struct Domains {
    pub list: Vec<String>,
    preferred: BTreeMap<u16, String>,
}
impl Domains {
    pub fn new(config: &Config) -> Self {
        let list = if config.cfproxy_user_domain_enabled && !config.cfproxy_user_domain.is_empty() {
            config.cfproxy_user_domain.clone()
        } else {
            include_str!("../assets/cfproxy-domains.txt")
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty() && !s.starts_with('#'))
                .map(decode_domain)
                .filter(|s| valid_domain(s))
                .collect()
        };
        Self {
            list,
            preferred: BTreeMap::new(),
        }
    }

    pub fn ordered(&self, dc: u16) -> Vec<String> {
        let mut list = self.list.clone();
        if let Ok(random) = crate::crypto::random::<8>() {
            let seed = u64::from_le_bytes(random) as usize;
            if !list.is_empty() {
                let len = list.len();
                list.rotate_left(seed % len);
            }
        }
        if let Some(preferred) = self.preferred.get(&dc)
            && let Some(index) = list.iter().position(|s| s == preferred)
        {
            list.swap(0, index);
        }
        list
    }
    pub fn prefer(&mut self, dc: u16, domain: &str) {
        self.preferred.insert(dc, domain.into());
    }
    pub fn replace(&mut self, domains: Vec<String>) {
        self.preferred.retain(|_, domain| domains.contains(domain));
        self.list = domains;
    }
}

pub enum Upstream {
    WebSocket { socket: Box<WebSocket>, split: bool },
    Tcp(TcpStream),
    H2(Arc<HttpLane>),
}

struct LaneState {
    lane: Option<Arc<HttpLane>>,
    retry: Instant,
}
struct TcpState {
    semaphore: Arc<Semaphore>,
    failures: u32,
    retry: Instant,
}

pub struct Routing {
    pub config: Arc<Config>,
    pub network: Arc<Network>,
    pub stats: Arc<Stats>,
    pub pool: Arc<Pool>,
    pub domains: Mutex<Domains>,
    lanes: Mutex<BTreeMap<String, Arc<Mutex<LaneState>>>>,
    tcp: Mutex<BTreeMap<(u16, bool), TcpState>>,
}

impl Routing {
    pub fn new(config: Arc<Config>, network: Arc<Network>, stats: Arc<Stats>) -> Arc<Self> {
        Arc::new(Self {
            pool: Pool::new(config.clone(), network.clone(), stats.clone()),
            domains: Mutex::new(Domains::new(&config)),
            config,
            network,
            stats,
            lanes: Mutex::new(BTreeMap::new()),
            tcp: Mutex::new(BTreeMap::new()),
        })
    }

    pub async fn choose(&self, init: &ClientInit) -> Result<Upstream> {
        let dc = init.dc;
        if self.config.pool_size > 0
            && let Some(socket) = self.pool.take(Key::Direct(dc, init.media, init.test)).await
        {
            self.stats.ws.fetch_add(1, Relaxed);
            log::info!("route DC{dc} media={} -> Telegram WS", init.media);
            return Ok(Upstream::WebSocket {
                socket: Box::new(socket),
                split: true,
            });
        }
        if !self.config.worker_domains().is_empty() && dc_ip(dc, init.test).is_some() {
            let key = Key::Worker(dc, init.test);
            let pooled = if self.config.pool_size > 0 {
                self.pool.take(key).await
            } else {
                None
            };
            let result = match pooled {
                Some(socket) => Ok(socket),
                None => self.pool.connect(key).await,
            };
            if let Ok(socket) = result {
                self.stats.cf.fetch_add(1, Relaxed);
                log::info!("route DC{dc} -> Worker WS");
                return Ok(Upstream::WebSocket {
                    socket: Box::new(socket),
                    split: false,
                });
            }
        }
        if self.config.cfproxy && !init.test {
            let domains = self.domains.lock().await.ordered(dc);
            if init.media && self.config.h2_enabled() {
                match timeout(Duration::from_secs(8), self.h2_lane(dc, &domains)).await {
                    Ok(Ok(lane)) => {
                        self.stats.h2.fetch_add(1, Relaxed);
                        self.stats.cf.fetch_add(1, Relaxed);
                        log::info!("route DC{dc} media -> CF H2");
                        return Ok(Upstream::H2(lane));
                    }
                    _ => log::debug!("DC{dc} H2 unavailable, trying WS"),
                }
            }
            for base in domains {
                let domain = format!("kws{dc}.{base}");
                match self
                    .network
                    .websocket(&domain, &domain, "/apiws", !self.config.no_secure, false)
                    .await
                {
                    Ok(socket) => {
                        self.domains.lock().await.prefer(dc, &base);
                        self.stats.cf.fetch_add(1, Relaxed);
                        log::info!("route DC{dc} -> CF WS");
                        return Ok(Upstream::WebSocket {
                            socket: Box::new(socket),
                            split: true,
                        });
                    }
                    Err(error) => {
                        self.stats.errors.fetch_add(1, Relaxed);
                        log::debug!("CF WS DC{dc}: {error}");
                    }
                }
            }
        }
        let ip = dc_ip(dc, init.test).context("no fallback IP for this DC")?;
        let semaphore = {
            let mut states = self.tcp.lock().await;
            let state = states.entry((dc, init.test)).or_insert_with(|| TcpState {
                semaphore: Arc::new(Semaphore::new(1)),
                failures: 0,
                retry: Instant::now(),
            });
            if Instant::now() < state.retry {
                bail!("TCP route in backoff for DC{dc}");
            }
            state.semaphore.clone()
        };
        let _lease = semaphore
            .try_acquire()
            .context("TCP route is already connecting")?;
        match self.network.tcp(ip, 443).await {
            Ok(stream) => {
                if let Some(state) = self.tcp.lock().await.get_mut(&(dc, init.test)) {
                    state.failures = 0;
                    state.retry = Instant::now();
                }
                self.stats.tcp.fetch_add(1, Relaxed);
                log::info!("route DC{dc} test={} -> TCP", init.test);
                Ok(Upstream::Tcp(stream))
            }
            Err(error) => {
                if let Some(state) = self.tcp.lock().await.get_mut(&(dc, init.test)) {
                    state.failures = (state.failures + 1).min(8);
                    state.retry = Instant::now()
                        + Duration::from_secs((30u64 << (state.failures - 1)).min(3600));
                }
                Err(error)
            }
        }
    }

    async fn h2_lane(&self, dc: u16, domains: &[String]) -> Result<Arc<HttpLane>> {
        for base in domains {
            let host = format!("kws{dc}.{base}");
            let state = {
                let mut lanes = self.lanes.lock().await;
                lanes
                    .entry(host.clone())
                    .or_insert_with(|| {
                        Arc::new(Mutex::new(LaneState {
                            lane: None,
                            retry: Instant::now(),
                        }))
                    })
                    .clone()
            };
            let mut state = state.lock().await;
            if Instant::now() < state.retry {
                continue;
            }
            if let Some(lane) = &state.lane {
                return Ok(lane.clone());
            }
            match HttpLane::connect(&self.network, &host, self.stats.clone()).await {
                Ok(lane) => match lane.preflight().await {
                    Ok(()) => {
                        state.lane = Some(lane.clone());
                        self.domains.lock().await.prefer(dc, base);
                        return Ok(lane);
                    }
                    Err(error) => log::debug!("H2 preflight: {error}"),
                },
                Err(error) => log::debug!("H2 connect: {error}"),
            }
            state.retry = Instant::now() + Duration::from_secs(30);
        }
        bail!("no available H2 route")
    }

    pub async fn invalidate_h2(&self, lane: &Arc<HttpLane>) {
        let states: Vec<_> = self.lanes.lock().await.values().cloned().collect();
        for state in states {
            let mut state = state.lock().await;
            if state.lane.as_ref().is_some_and(|v| Arc::ptr_eq(v, lane)) {
                state.lane = None;
                state.retry = Instant::now() + Duration::from_secs(30);
            }
        }
    }

    pub async fn refresh_domains(self: Arc<Self>) {
        if self.config.cfproxy_user_domain_enabled {
            return;
        }
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tick.tick().await;
            let result = h2_transport::get(
                &self.network,
                "raw.githubusercontent.com",
                "/Flowseal/tg-ws-proxy/main/.github/cfproxy-domains.txt",
                64 * 1024,
            )
            .await;
            match result {
                Ok(response) if response.status == 200 => {
                    let text = String::from_utf8_lossy(&response.body);
                    let mut domains: Vec<_> = text
                        .lines()
                        .map(str::trim)
                        .filter(|s| !s.is_empty() && !s.starts_with('#'))
                        .map(decode_domain)
                        .filter(|s| valid_domain(s))
                        .collect();
                    domains.sort();
                    domains.dedup();
                    if domains.len() >= 3 {
                        self.domains.lock().await.replace(domains);
                        log::info!("CF domain list refreshed");
                    } else {
                        log::warn!("CF domain refresh rejected: fewer than three valid domains");
                    }
                }
                _ => log::warn!("CF domain refresh unavailable; keeping existing list"),
            }
        }
    }
}
