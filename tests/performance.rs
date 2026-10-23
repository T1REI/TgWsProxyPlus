use anyhow::{Result, bail};
use futures_util::FutureExt;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
    time::{Duration, Instant},
};
use tg_ws_proxy_plus::performance::{self, Kind, Metrics, Route};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::{sleep, timeout},
};

fn route(id: usize) -> Route {
    Route {
        dc: 2,
        media: true,
        test: false,
        kind: Kind::H2,
        endpoint: format!("relay{id}.example.org"),
    }
}

struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, SeqCst);
    }
}

#[tokio::test]
async fn races_are_staggered_bounded_and_losers_are_cancelled() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let polled = Arc::new(AtomicUsize::new(0));
    let attempts = (0..5).map(|index| {
        let active = active.clone();
        let peak = peak.clone();
        let polled = polled.clone();
        async move {
            polled.fetch_add(1, SeqCst);
            let count = active.fetch_add(1, SeqCst) + 1;
            peak.fetch_max(count, SeqCst);
            let _guard = Active(active);
            sleep(Duration::from_millis(if index == 1 { 5 } else { 500 })).await;
            Ok(index)
        }
        .boxed()
    });
    assert_eq!(
        performance::race(attempts, Duration::from_millis(10))
            .await
            .unwrap(),
        1
    );
    assert_eq!(peak.load(SeqCst), 2);
    assert_eq!(polled.load(SeqCst), 2);
    assert_eq!(active.load(SeqCst), 0);
}

#[tokio::test]
async fn failed_candidates_advance_without_waiting_for_stagger() {
    let attempts = (0..3).map(|index| {
        async move {
            if index < 2 {
                bail!("unreachable");
            }
            Ok(index)
        }
        .boxed()
    });
    assert_eq!(
        timeout(
            Duration::from_millis(500),
            performance::race(attempts, Duration::from_secs(10))
        )
        .await
        .unwrap()
        .unwrap(),
        2
    );
    let empty = Vec::<futures_util::future::BoxFuture<'static, Result<()>>>::new();
    assert!(
        performance::race(empty, Duration::from_millis(1))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancelling_selection_drops_all_pending_handshakes() {
    let active = Arc::new(AtomicUsize::new(0));
    let attempts = (0..4).map(|_| {
        let active = active.clone();
        async move {
            active.fetch_add(1, SeqCst);
            let _guard = Active(active);
            std::future::pending::<Result<()>>().await
        }
        .boxed()
    });
    assert!(
        timeout(
            Duration::from_millis(50),
            performance::race(attempts, Duration::from_millis(5))
        )
        .await
        .is_err()
    );
    assert_eq!(active.load(SeqCst), 0);
}

#[test]
fn route_history_ranks_latency_isolates_media_and_bounds_memory() {
    let metrics = Metrics::default();
    let fast = route(0);
    let slow = route(1);
    metrics.connected(&fast, Duration::from_millis(10));
    metrics.connected(&slow, Duration::from_millis(200));
    assert!(metrics.score(&fast) < metrics.score(&slow));
    metrics.failed(&fast);
    assert!(metrics.score(&fast).is_none());
    let mut other = fast.clone();
    other.media = false;
    assert!(metrics.score(&other).is_some());
    metrics.connected(&fast, Duration::from_millis(20));
    assert!(metrics.score(&fast).is_some());
    for i in 2..200 {
        metrics.connected(&route(i), Duration::from_millis(10));
    }
    assert_eq!(metrics.report().len(), 128);
    assert!(
        metrics
            .report()
            .iter()
            .all(|line| !line.contains("example.org"))
    );
}

#[tokio::test]
async fn stream_metrics_measure_delivered_bytes_and_release_active_session() {
    let metrics = Arc::new(Metrics::default());
    let id = route(0);
    metrics.connected(&id, Duration::from_millis(15));
    let (stream, mut peer) = tokio::io::duplex(256 * 1024);
    let mut measured = metrics.observe(id.clone(), Box::new(stream), Instant::now());
    measured.write_all(&vec![1; 64 * 1024]).await.unwrap();
    sleep(Duration::from_millis(20)).await;
    measured.write_all(&vec![2; 64 * 1024]).await.unwrap();
    let mut received = vec![0; 128 * 1024];
    peer.read_exact(&mut received).await.unwrap();
    assert_eq!(received[0], 1);
    assert_eq!(received[65536], 2);
    peer.write_all(b"request").await.unwrap();
    measured.read_exact(&mut [0; 7]).await.unwrap();
    drop(measured);
    let report = metrics.report().join("\n");
    assert!(
        report.contains("active=0") && report.contains("up=7") && report.contains("down=131072")
    );
    assert!(!report.contains("rate=0KiB/s"));
}
