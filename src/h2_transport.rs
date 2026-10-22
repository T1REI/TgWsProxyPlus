use crate::{
    network::{Network, Stream},
    stats::Stats,
};
use anyhow::{Context, Result, ensure};
use bytes::{Buf, Bytes};
use h2::client::SendRequest;
use http::{HeaderMap, Method, Request};
use std::{
    future::poll_fn,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering::Relaxed},
    },
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
    time::timeout,
};

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(65);

pub struct Response {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Bytes,
    _memory: OwnedSemaphorePermit,
}

struct Driver(JoinHandle<()>);
impl Drop for Driver {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub struct HttpLane {
    host: String,
    sender: SendRequest<Bytes>,
    _driver: Driver,
    requests: Semaphore,
    memory: Arc<Semaphore>,
    stats: Arc<Stats>,
}

impl HttpLane {
    pub async fn connect(network: &Network, host: &str, stats: Arc<Stats>) -> Result<Arc<Self>> {
        let stream = network.h2_stream(host).await?;
        stats.h2_connections.fetch_add(1, Relaxed);
        Self::from_stream(stream, host, stats).await
    }

    pub async fn from_stream(stream: Stream, host: &str, stats: Arc<Stats>) -> Result<Arc<Self>> {
        let (sender, connection) = h2::client::Builder::new()
            .enable_push(false)
            .initial_window_size(256 * 1024)
            .initial_connection_window_size(4 * 1024 * 1024)
            .max_concurrent_streams(64)
            .max_header_list_size(32 * 1024)
            .handshake(stream)
            .await?;
        let driver = tokio::spawn(async move {
            if let Err(error) = connection.await {
                log::debug!("H2 connection ended: {error}");
            }
        });
        Ok(Arc::new(Self {
            host: host.into(),
            sender,
            _driver: Driver(driver),
            requests: Semaphore::new(64),
            memory: Arc::new(Semaphore::new(32 * 1024 * 1024)),
            stats,
        }))
    }

    pub async fn preflight(&self) -> Result<()> {
        let reply = self.request(Method::HEAD, "/api", Bytes::new(), 0).await?;
        ensure!(
            (200..300).contains(&reply.status) || matches!(reply.status, 405 | 501),
            "H2 preflight returned HTTP {}",
            reply.status
        );
        Ok(())
    }

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        body: Bytes,
        limit: usize,
    ) -> Result<Response> {
        self.request_traced(method, path, body, limit, None).await
    }

    pub async fn request_traced(
        &self,
        method: Method,
        path: &str,
        body: Bytes,
        limit: usize,
        progress: Option<Arc<AtomicU8>>,
    ) -> Result<Response> {
        timeout(
            REQUEST_TIMEOUT,
            self.perform(method, path, body, limit, progress),
        )
        .await
        .context("HTTP/2 request timed out")?
    }

    async fn perform(
        &self,
        method: Method,
        path: &str,
        mut body: Bytes,
        limit: usize,
        progress: Option<Arc<AtomicU8>>,
    ) -> Result<Response> {
        let _request = self.requests.acquire().await?;
        ensure!(
            body.len() + limit <= 32 * 1024 * 1024,
            "HTTP request exceeds lane memory bound"
        );
        let memory = self
            .memory
            .clone()
            .acquire_many_owned((body.len() + limit) as u32)
            .await?;
        let head = method == Method::HEAD;
        let request = Request::builder()
            .method(method)
            .uri(format!("https://{}{path}", self.host))
            .header("content-type", "application/octet-stream")
            .header("accept-encoding", "identity")
            .header(
                "user-agent",
                concat!("TgWsProxyPlus/", env!("CARGO_PKG_VERSION")),
            )
            .header("content-length", body.len())
            .body(())?;
        let mut sender = self.sender.clone().ready().await?;
        let (response, mut send) = sender.send_request(request, body.is_empty())?;
        while !body.is_empty() {
            send.reserve_capacity(body.len());
            let capacity = if send.capacity() > 0 {
                send.capacity()
            } else {
                poll_fn(|cx| send.poll_capacity(cx))
                    .await
                    .context("H2 upload stream closed")??
            };
            if capacity == 0 {
                continue;
            }
            let count = capacity.min(body.len()).min(64 * 1024);
            let chunk = body.split_to(count);
            send.send_data(chunk, body.is_empty())?;
        }
        if let Some(progress) = &progress {
            progress.store(1, Relaxed);
        }
        self.stats.h2_requests.fetch_add(1, Relaxed);
        let response = response.await?;
        if let Some(progress) = &progress {
            progress.store(2, Relaxed);
        }
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        if !head && let Some(length) = headers.get("content-length") {
            ensure!(
                length.to_str()?.parse::<usize>()? <= limit,
                "HTTP response exceeds size limit"
            );
        }
        let mut stream = response.into_body();
        let mut buffer = Vec::new();
        while let Some(chunk) = stream.data().await {
            let chunk = chunk?;
            ensure!(
                buffer.len() + chunk.len() <= limit,
                "HTTP response exceeds size limit"
            );
            buffer.extend_from_slice(&chunk);
            stream.flow_control().release_capacity(chunk.remaining())?;
        }
        if !head && let Some(length) = headers.get("content-length") {
            ensure!(
                length.to_str()?.parse::<usize>()? == buffer.len(),
                "incomplete HTTP response"
            );
        }
        if let Some(progress) = &progress {
            progress.store(3, Relaxed);
        }
        Ok(Response {
            status,
            headers,
            body: buffer.into(),
            _memory: memory,
        })
    }
}

pub async fn get(network: &Network, host: &str, path: &str, limit: usize) -> Result<Response> {
    let lane = timeout(
        Duration::from_secs(10),
        HttpLane::connect(network, host, Arc::new(Stats::default())),
    )
    .await??;
    lane.request(Method::GET, path, Bytes::new(), limit).await
}
