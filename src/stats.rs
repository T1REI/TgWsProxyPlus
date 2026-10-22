use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Default)]
pub struct Stats {
    pub total: AtomicU64,
    pub active: AtomicU64,
    pub bad: AtomicU64,
    pub ws: AtomicU64,
    pub h2: AtomicU64,
    pub tcp: AtomicU64,
    pub cf: AtomicU64,
    pub fronting: AtomicU64,
    pub errors: AtomicU64,
    pub pool_hits: AtomicU64,
    pub pool_misses: AtomicU64,
    pub up: AtomicU64,
    pub down: AtomicU64,
    pub h2_requests: AtomicU64,
    pub h2_connections: AtomicU64,
    pub replays: AtomicU64,
}

impl Stats {
    pub fn summary(&self) -> String {
        format!(
            "total={} active={} bad={} ws={} h2={} tcp={} cf={} front={} err={} pool={}/{} up={} down={} h2_req={} h2_tcp={} replays={}",
            self.total.load(Relaxed),
            self.active.load(Relaxed),
            self.bad.load(Relaxed),
            self.ws.load(Relaxed),
            self.h2.load(Relaxed),
            self.tcp.load(Relaxed),
            self.cf.load(Relaxed),
            self.fronting.load(Relaxed),
            self.errors.load(Relaxed),
            self.pool_hits.load(Relaxed),
            self.pool_hits.load(Relaxed) + self.pool_misses.load(Relaxed),
            self.up.load(Relaxed),
            self.down.load(Relaxed),
            self.h2_requests.load(Relaxed),
            self.h2_connections.load(Relaxed),
            self.replays.load(Relaxed)
        )
    }
}
