use crate::{
    config::{Config, valid_domain},
    crypto::ClientInit,
    h2_pool::{LanePool, Session},
    h2_transport::{self, HttpLane, Settings},
    network::{Network, WebSocket},
    performance::{self, Kind, Metrics, Route},
    pool::{Key, Pool},
    stats::Stats,
};
use anyhow::{Context, Result};
use futures_util::FutureExt;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
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

pub struct Selection {
    pub upstream: Upstream,
    pub route: Route,
    pub elapsed: Duration,
    pub session: Option<Session>,
}

pub struct Routing {
    pub config: Arc<Config>,
    pub network: Arc<Network>,
    pub stats: Arc<Stats>,
    pub pool: Arc<Pool>,
    pub domains: Mutex<Domains>,
    pub metrics: Arc<Metrics>,
    lanes: LanePool,
    memory: Arc<Semaphore>,
    selections: AtomicU64,
    handshakes: Semaphore,
}

impl Routing {
    pub fn new(config: Arc<Config>, network: Arc<Network>, stats: Arc<Stats>) -> Arc<Self> {
        Arc::new(Self {
            pool: Pool::new(config.clone(), network.clone(), stats.clone()),
            domains: Mutex::new(Domains::new(&config)),
            config,
            network,
            stats,
            metrics: Arc::new(Metrics::default()),
            lanes: LanePool::default(),
            memory: Arc::new(Semaphore::new(32 * 1024 * 1024)),
            selections: AtomicU64::new(0),
            handshakes: Semaphore::new(8),
        })
    }

    pub async fn choose(&self, init: &ClientInit) -> Result<Selection> {
        let mut candidates = Vec::new();
        let mut domain_order = Vec::new();
        let route = |kind, endpoint: String| Route {
            dc: init.dc,
            media: init.media,
            test: init.test,
            kind,
            endpoint,
        };
        if self.config.pool_size > 0
            && crate::config::parse_dc_ips(&self.config.dc_ip)?.contains_key(&init.dc)
        {
            candidates.push(route(Kind::Direct, "telegram".into()));
        }
        if !self.config.worker_domains().is_empty() && dc_ip(init.dc, init.test).is_some() {
            candidates.push(route(Kind::Worker, "workers".into()));
        }
        if self.config.cfproxy && !init.test {
            for base in self
                .domains
                .lock()
                .await
                .ordered(init.dc)
                .into_iter()
                .take(12)
            {
                let host = format!("kws{}.{}", init.dc, base);
                domain_order.push(host.clone());
                if init.media && self.config.h2_enabled() {
                    candidates.push(route(Kind::H2, host.clone()));
                }
                candidates.push(route(Kind::WebSocket, host));
            }
        }
        if let Some(ip) = dc_ip(init.dc, init.test) {
            candidates.push(route(Kind::Tcp, ip.into()));
        }
        let mut candidates: Vec<_> = candidates
            .into_iter()
            .filter_map(|route| {
                let score = self.metrics.score(&route)?;
                let offset = if self.metrics.known(&route) {
                    0
                } else {
                    domain_order
                        .iter()
                        .position(|host| *host == route.endpoint)
                        .unwrap_or(0) as u64
                        * 200
                };
                Some((score.saturating_add(offset), route))
            })
            .collect();
        candidates.sort_unstable_by_key(|(score, _)| *score);
        let fallback = candidates
            .iter()
            .position(|(_, route)| route.kind == Kind::Tcp)
            .map(|index| candidates.remove(index).1);
        let sequence = self.selections.fetch_add(1, Relaxed);
        if sequence % 16 == 15 && candidates.len() > 2 {
            let index = 2 + (sequence as usize / 16) % (candidates.len() - 2);
            candidates.swap(1, index);
        }
        let started = Instant::now();
        let attempt = |route: Route| {
            async move {
                let begin = Instant::now();
                match timeout(Duration::from_secs(8), self.connect_route(&route)).await {
                    Ok(Ok((upstream, session))) => {
                        self.metrics.connected(&route, begin.elapsed());
                        Ok(Selection {
                            upstream,
                            route,
                            elapsed: started.elapsed(),
                            session,
                        })
                    }
                    result => {
                        self.metrics.failed(&route);
                        self.stats.errors.fetch_add(1, Relaxed);
                        match result {
                            Ok(Err(error)) => Err(error),
                            _ => anyhow::bail!("route connection timeout"),
                        }
                    }
                }
            }
            .boxed()
        };
        let selection = timeout(Duration::from_secs(16), async {
            match performance::race(
                candidates.into_iter().map(|(_, route)| attempt(route)),
                Duration::from_millis(200),
            )
            .await
            {
                Ok(selection) => Ok(selection),
                Err(error) => match fallback {
                    Some(route) => attempt(route).await,
                    None => Err(error),
                },
            }
        })
        .await
        .context("route selection timed out")??;
        if matches!(selection.route.kind, Kind::H2 | Kind::WebSocket)
            && let Some((_, base)) = selection.route.endpoint.split_once('.')
        {
            self.domains.lock().await.prefer(init.dc, base);
        }
        match selection.route.kind {
            Kind::Direct => {
                self.stats.ws.fetch_add(1, Relaxed);
            }
            Kind::H2 => {
                self.stats.h2.fetch_add(1, Relaxed);
                self.stats.cf.fetch_add(1, Relaxed);
            }
            Kind::Worker | Kind::WebSocket => {
                self.stats.cf.fetch_add(1, Relaxed);
            }
            Kind::Tcp => {
                self.stats.tcp.fetch_add(1, Relaxed);
            }
        }
        log::info!(
            "route DC{} media={} -> {} connect={}ms",
            init.dc,
            init.media,
            selection.route.kind.name(),
            selection.elapsed.as_millis()
        );
        Ok(selection)
    }

    async fn connect_route(&self, route: &Route) -> Result<(Upstream, Option<Session>)> {
        let _permit = self.handshakes.acquire().await?;
        let upstream = match route.kind {
            Kind::Direct | Kind::Worker => {
                let key = if route.kind == Kind::Direct {
                    Key::Direct(route.dc, route.media, route.test)
                } else {
                    Key::Worker(route.dc, route.test)
                };
                let socket = if self.config.pool_size > 0 {
                    self.pool.take(key).await
                } else {
                    None
                };
                let socket = match socket {
                    Some(socket) => socket,
                    None => self.pool.connect(key).await?,
                };
                Upstream::WebSocket {
                    socket: Box::new(socket),
                    split: route.kind == Kind::Direct,
                }
            }
            Kind::WebSocket => Upstream::WebSocket {
                socket: Box::new(
                    self.network
                        .websocket(
                            &route.endpoint,
                            &route.endpoint,
                            "/apiws",
                            !self.config.no_secure,
                            false,
                        )
                        .await?,
                ),
                split: true,
            },
            Kind::Tcp => Upstream::Tcp(self.network.tcp(&route.endpoint, 443).await?),
            Kind::H2 => {
                let session = self
                    .lanes
                    .acquire(&route.endpoint, self.config.h2_max_connections, || async {
                        let stream = self.network.h2_stream(&route.endpoint).await?;
                        let settings = Settings {
                            stream_window: (self.config.h2_stream_window_kb * 1024) as u32,
                            connection_window: (self.config.h2_connection_window_kb * 1024) as u32,
                        };
                        let lane = HttpLane::with_settings(
                            stream,
                            &route.endpoint,
                            self.stats.clone(),
                            settings,
                            self.memory.clone(),
                        )
                        .await?;
                        lane.preflight().await?;
                        self.stats.h2_connections.fetch_add(1, Relaxed);
                        Ok(lane)
                    })
                    .await?;
                return Ok((Upstream::H2(session.0.clone()), Some(session)));
            }
        };
        Ok((upstream, None))
    }

    pub async fn invalidate_h2(&self, lane: &Arc<HttpLane>) {
        if lane.closed() {
            self.lanes.invalidate(lane);
        }
    }

    pub fn report(&self) -> Vec<String> {
        self.metrics.report()
    }

    pub async fn maintain(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        loop {
            tick.tick().await;
            self.lanes.prune();
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
