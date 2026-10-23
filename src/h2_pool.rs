use crate::h2_transport::HttpLane;
use anyhow::Result;
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub struct Session(pub(crate) Arc<HttpLane>);
impl Session {
    pub fn lane(&self) -> &Arc<HttpLane> {
        &self.0
    }
    fn new(lane: Arc<HttpLane>) -> Self {
        lane.add_session();
        Self(lane)
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.0.remove_session();
    }
}

struct State {
    lanes: Vec<Arc<HttpLane>>,
    connecting: bool,
    retry: Instant,
    used: Instant,
}
struct Connecting(Arc<Mutex<State>>);
impl Drop for Connecting {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.lock() {
            state.connecting = false;
        }
    }
}

#[derive(Default)]
pub struct LanePool(Mutex<BTreeMap<String, Arc<Mutex<State>>>>);

impl LanePool {
    pub async fn acquire<F, Fut>(&self, host: &str, limit: usize, connect: F) -> Result<Session>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Arc<HttpLane>>>,
    {
        let state = {
            let mut hosts = self
                .0
                .lock()
                .map_err(|_| anyhow::anyhow!("H2 pool lock poisoned"))?;
            hosts
                .entry(host.into())
                .or_insert_with(|| {
                    Arc::new(Mutex::new(State {
                        lanes: vec![],
                        connecting: false,
                        retry: Instant::now(),
                        used: Instant::now(),
                    }))
                })
                .clone()
        };
        let fallback = {
            let mut s = state
                .lock()
                .map_err(|_| anyhow::anyhow!("H2 lane lock poisoned"))?;
            s.lanes.retain(|lane| !lane.closed());
            s.used = Instant::now();
            let best = s
                .lanes
                .iter()
                .min_by_key(|lane| (lane.sessions(), lane.load()))
                .cloned();
            let needs_connection =
                s.lanes.len() < limit && best.as_ref().is_none_or(|lane| lane.sessions() > 0);
            if !needs_connection || s.connecting || Instant::now() < s.retry {
                return best
                    .map(Session::new)
                    .ok_or_else(|| anyhow::anyhow!("H2 route connecting or in cooldown"));
            }
            s.connecting = true;
            best
        };
        let _connecting = Connecting(state.clone());
        match connect().await {
            Ok(lane) => {
                let mut s = state
                    .lock()
                    .map_err(|_| anyhow::anyhow!("H2 lane lock poisoned"))?;
                s.lanes.push(lane.clone());
                s.retry = Instant::now();
                Ok(Session::new(lane))
            }
            Err(error) => {
                if let Ok(mut s) = state.lock() {
                    s.retry = Instant::now() + Duration::from_secs(30);
                }
                if let Some(lane) = fallback {
                    Ok(Session::new(lane))
                } else {
                    Err(error)
                }
            }
        }
    }

    pub fn prune(&self) {
        let Ok(mut hosts) = self.0.lock() else {
            return;
        };
        hosts.retain(|_, state| {
            let Ok(mut s) = state.lock() else {
                return false;
            };
            let idle = s.used.elapsed() >= Duration::from_secs(60);
            s.lanes
                .retain(|lane| !lane.closed() && (!idle || lane.sessions() > 0));
            s.connecting || !s.lanes.is_empty() || s.used.elapsed() < Duration::from_secs(120)
        });
    }

    pub fn invalidate(&self, lane: &Arc<HttpLane>) {
        let Ok(hosts) = self.0.lock() else {
            return;
        };
        for state in hosts.values() {
            if let Ok(mut s) = state.lock() {
                s.lanes.retain(|value| !Arc::ptr_eq(value, lane));
            }
        }
    }
}
