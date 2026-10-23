use bytes::Bytes;
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tg_ws_proxy_plus::{
    h2_transport::{HttpLane, Settings},
    stats::Stats,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf},
    sync::Semaphore,
    time::{Sleep, sleep, timeout},
};

struct DelayedControl {
    inner: DuplexStream,
    delay: Pin<Box<Sleep>>,
    restart: bool,
}
impl AsyncRead for DelayedControl {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.restart {
            self.delay
                .as_mut()
                .reset(tokio::time::Instant::now() + Duration::from_millis(30));
            self.restart = false;
        }
        if self.delay.as_mut().poll(cx).is_pending() {
            return Poll::Pending;
        }
        let before = buffer.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buffer);
        if buffer.filled().len() > before {
            self.restart = true;
        }
        result
    }
}
impl AsyncWrite for DelayedControl {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buffer)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

async fn download(window: u32) -> f64 {
    let (client, remote) = tokio::io::duplex(4 * 1024 * 1024);
    let server = tokio::spawn(async move {
        let remote = DelayedControl {
            inner: remote,
            delay: Box::pin(sleep(Duration::from_millis(30))),
            restart: false,
        };
        let mut connection = h2::server::handshake(remote).await.unwrap();
        let mut tasks = tokio::task::JoinSet::new();
        while let Some(request) = connection.accept().await {
            let (_, mut respond) = request.unwrap();
            tasks.spawn(async move {
                let response = http::Response::builder()
                    .header("content-length", 4 * 1024 * 1024)
                    .body(())
                    .unwrap();
                let mut send = respond.send_response(response, false).unwrap();
                let mut body = Bytes::from(vec![0x33; 4 * 1024 * 1024]);
                while !body.is_empty() {
                    send.reserve_capacity(body.len());
                    let capacity = if send.capacity() > 0 {
                        send.capacity()
                    } else {
                        poll_fn(|cx| send.poll_capacity(cx)).await.unwrap().unwrap()
                    };
                    if capacity == 0 {
                        continue;
                    }
                    let count = capacity.min(body.len());
                    let chunk = body.split_to(count);
                    send.send_data(chunk, body.is_empty()).unwrap();
                }
            });
        }
    });
    let began = Instant::now();
    let lane = HttpLane::with_settings(
        Box::new(client),
        "localhost",
        Arc::new(Stats::default()),
        Settings {
            stream_window: window,
            connection_window: 4 * 1024 * 1024,
        },
        Arc::new(Semaphore::new(32 * 1024 * 1024)),
    )
    .await
    .unwrap();
    let response = lane
        .request(http::Method::GET, "/", Bytes::new(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let elapsed = began.elapsed().as_secs_f64();
    assert_eq!(response.body.len(), 4 * 1024 * 1024);
    assert!(response.body.iter().all(|&byte| byte == 0x33));
    drop(lane);
    server.abort();
    let _ = server.await;
    elapsed
}

#[tokio::test]
#[ignore = "manual H2 benchmark with 30ms delay on control reads; run in release mode"]
async fn receive_windows_with_delayed_control_traffic() {
    timeout(Duration::from_secs(20), async {
        let mut old = Vec::new();
        let mut new = Vec::new();
        for _ in 0..3 {
            old.push(download(256 * 1024).await);
            new.push(download(1024 * 1024).await);
        }
        old.sort_by(f64::total_cmp);
        new.sort_by(f64::total_cmp);
        println!(
            "h2_4MiB_30ms_control_ms old={:.1} new={:.1} speedup={:.2}",
            old[1] * 1000.0,
            new[1] * 1000.0,
            old[1] / new[1]
        );
    })
    .await
    .unwrap();
}
