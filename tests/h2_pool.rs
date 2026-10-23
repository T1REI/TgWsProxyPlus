use bytes::Bytes;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
    time::Duration,
};
use tg_ws_proxy_plus::{
    h2_pool::LanePool,
    h2_transport::{HttpLane, Settings},
    stats::Stats,
};
use tokio::{sync::Semaphore, time::timeout};

struct Peer(tokio::task::JoinHandle<()>);
impl Drop for Peer {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn lane(memory: Arc<Semaphore>) -> (Arc<HttpLane>, Peer) {
    let (client, remote) = tokio::io::duplex(65536);
    let task = tokio::spawn(async move {
        let mut connection = h2::server::handshake(remote).await.unwrap();
        while let Some(request) = connection.accept().await {
            let Ok((_, mut respond)) = request else {
                break;
            };
            if respond
                .send_response(http::Response::new(()), true)
                .is_err()
            {
                break;
            }
        }
    });
    let lane = HttpLane::with_settings(
        Box::new(client),
        "localhost",
        Arc::new(Stats::default()),
        Settings::default(),
        memory,
    )
    .await
    .unwrap();
    (lane, Peer(task))
}

#[tokio::test]
async fn new_sessions_balance_across_bounded_connections_and_reuse_idle_lanes() {
    let pool = LanePool::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let (a, _peer_a) = lane(Arc::new(Semaphore::new(1024))).await;
    let (b, _peer_b) = lane(Arc::new(Semaphore::new(1024))).await;
    let first = pool
        .acquire("localhost", 2, || async {
            calls.fetch_add(1, SeqCst);
            Ok(a.clone())
        })
        .await
        .unwrap();
    let second = pool
        .acquire("localhost", 2, || async {
            calls.fetch_add(1, SeqCst);
            Ok(b.clone())
        })
        .await
        .unwrap();
    assert!(!Arc::ptr_eq(first.lane(), second.lane()));
    let third = pool
        .acquire("localhost", 2, || async {
            panic!("connection limit exceeded")
        })
        .await
        .unwrap();
    assert_eq!(calls.load(SeqCst), 2);
    assert_eq!(a.sessions() + b.sessions(), 3);
    drop(second);
    let next = pool
        .acquire("localhost", 2, || async {
            panic!("idle connection ignored")
        })
        .await
        .unwrap();
    assert!(Arc::ptr_eq(next.lane(), &b));
    drop((first, third, next));
    assert_eq!(a.sessions() + b.sessions(), 0);
}

#[tokio::test]
async fn cancellation_clears_connecting_state_and_failed_expansion_keeps_working_lane() {
    let pool = LanePool::default();
    assert!(
        timeout(
            Duration::from_millis(30),
            pool.acquire("localhost", 2, std::future::pending)
        )
        .await
        .is_err()
    );
    let (a, _peer) = lane(Arc::new(Semaphore::new(1024))).await;
    let first = pool
        .acquire("localhost", 2, || async { Ok(a.clone()) })
        .await
        .unwrap();
    let second = pool
        .acquire("localhost", 2, || async {
            anyhow::bail!("secondary unavailable")
        })
        .await
        .unwrap();
    assert!(Arc::ptr_eq(first.lane(), second.lane()));
    pool.invalidate(&a);
    assert!(
        pool.acquire("localhost", 2, || async { anyhow::bail!("offline") })
            .await
            .is_err()
    );
    assert_eq!(a.sessions(), 2);
    drop((first, second));
    assert_eq!(a.sessions(), 0);
}

#[tokio::test]
async fn response_memory_budget_is_shared_between_independent_h2_connections() {
    let memory = Arc::new(Semaphore::new(4096));
    let (a, _peer_a) = lane(memory.clone()).await;
    let (b, _peer_b) = lane(memory.clone()).await;
    let first = a
        .request(http::Method::GET, "/", Bytes::new(), 4096)
        .await
        .unwrap();
    let second =
        tokio::spawn(async move { b.request(http::Method::GET, "/", Bytes::new(), 4096).await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!second.is_finished());
    drop(first);
    timeout(Duration::from_secs(1), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(memory.available_permits(), 4096);
}
