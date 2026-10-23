use anyhow::{Context, Result, ensure};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    time::timeout,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{client::IntoClientRequest, protocol::WebSocketConfig},
};

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
pub type Stream = Box<dyn Io>;
pub type WebSocket = WebSocketStream<Stream>;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct Network {
    tls: tokio_native_tls::TlsConnector,
    fronting: tokio_native_tls::TlsConnector,
    h2: tokio_native_tls::TlsConnector,
    pub buffer: usize,
}

impl Network {
    pub fn new(buffer: usize) -> Result<Arc<Self>> {
        let tls = native_tls::TlsConnector::builder().build()?;
        let fronting = native_tls::TlsConnector::builder()
            .danger_accept_invalid_hostnames(true)
            .build()?;
        let h2 = native_tls::TlsConnector::builder()
            .request_alpns(&["h2"])
            .build()?;
        Ok(Arc::new(Self {
            tls: tls.into(),
            fronting: fronting.into(),
            h2: h2.into(),
            buffer,
        }))
    }

    pub fn configure(&self, stream: &TcpStream) -> Result<()> {
        stream.set_nodelay(true)?;
        let socket = socket2::SockRef::from(stream);
        if self.buffer > 0 {
            socket.set_recv_buffer_size(self.buffer)?;
            socket.set_send_buffer_size(self.buffer)?;
        }
        socket
            .set_tcp_keepalive(&socket2::TcpKeepalive::new().with_time(Duration::from_secs(60)))?;
        Ok(())
    }

    pub async fn tcp(&self, host: &str, port: u16) -> Result<TcpStream> {
        let stream = timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
            .await
            .context("TCP connect timed out")??;
        self.configure(&stream)?;
        Ok(stream)
    }

    pub async fn stream(
        &self,
        host: &str,
        port: u16,
        sni: &str,
        secure: bool,
        fronting: bool,
    ) -> Result<Stream> {
        let tcp = self.tcp(host, port).await?;
        if !secure {
            return Ok(Box::new(tcp));
        }
        let connector = if fronting { &self.fronting } else { &self.tls };
        Ok(Box::new(
            timeout(CONNECT_TIMEOUT, connector.connect(sni, tcp))
                .await
                .context("TLS handshake timed out")??,
        ))
    }

    pub async fn h2_stream(&self, host: &str) -> Result<Stream> {
        let tcp = self.tcp(host, 443).await?;
        let stream = timeout(CONNECT_TIMEOUT, self.h2.connect(host, tcp))
            .await
            .context("H2 TLS handshake timed out")??;
        ensure!(
            stream.get_ref().negotiated_alpn()?.as_deref() == Some(b"h2"),
            "endpoint did not negotiate HTTP/2"
        );
        Ok(Box::new(stream))
    }

    pub async fn websocket(
        &self,
        host: &str,
        domain: &str,
        path: &str,
        secure: bool,
        fronting: bool,
    ) -> Result<WebSocket> {
        let sni = if fronting { "sprinthost.ru" } else { domain };
        timeout(CONNECT_TIMEOUT, async {
            let stream = self
                .stream(host, if secure { 443 } else { 80 }, sni, secure, fronting)
                .await?;
            websocket_handshake(stream, domain, path, secure).await
        })
        .await
        .context("WebSocket setup timed out")?
    }
}

pub async fn websocket_handshake(
    stream: Stream,
    domain: &str,
    path: &str,
    secure: bool,
) -> Result<WebSocket> {
    ensure!(
        path.starts_with('/') && !path.contains(['\r', '\n']),
        "invalid WebSocket path"
    );
    let mut request =
        format!("{}://{domain}{path}", if secure { "wss" } else { "ws" }).into_client_request()?;
    if !path.starts_with("/apiws?dst=") {
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "binary".parse()?);
    }
    let limits = WebSocketConfig::default()
        .max_message_size(Some(16 * 1024 * 1024))
        .max_frame_size(Some(16 * 1024 * 1024));
    let (socket, _) =
        tokio_tungstenite::client_async_with_config(request, stream, Some(limits)).await?;
    Ok(socket)
}
