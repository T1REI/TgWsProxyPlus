use crate::{h2_transport, network::Network};
use anyhow::{Context, Result, ensure};
use http::{HeaderMap, HeaderName, HeaderValue};
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    time::timeout,
};

pub struct Response {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

async fn line<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
    budget: &mut usize,
) -> Result<Vec<u8>> {
    ensure!(*budget > 0, "HTTP headers exceed limit");
    let mut bytes = Vec::new();
    reader
        .take(*budget as u64)
        .read_until(b'\n', &mut bytes)
        .await?;
    ensure!(bytes.ends_with(b"\r\n"), "incomplete HTTP header");
    *budget -= bytes.len();
    bytes.truncate(bytes.len() - 2);
    Ok(bytes)
}

pub async fn read_response<R: AsyncRead + Unpin>(stream: R, limit: usize) -> Result<Response> {
    let mut reader = BufReader::new(stream);
    let mut budget = 32 * 1024;
    let first = line(&mut reader, &mut budget).await?;
    let text = std::str::from_utf8(&first)?;
    let mut parts = text.split(' ');
    ensure!(
        matches!(parts.next(), Some("HTTP/1.0" | "HTTP/1.1")),
        "invalid HTTP version"
    );
    let status: u16 = parts.next().context("missing HTTP status")?.parse()?;
    ensure!((200..600).contains(&status), "unexpected HTTP status");
    let mut headers = HeaderMap::new();
    loop {
        let bytes = line(&mut reader, &mut budget).await?;
        if bytes.is_empty() {
            break;
        }
        let colon = bytes
            .iter()
            .position(|b| *b == b':')
            .context("invalid HTTP header")?;
        let name = HeaderName::from_bytes(&bytes[..colon])?;
        let value = HeaderValue::from_bytes(bytes[colon + 1..].trim_ascii())?;
        headers.append(name, value);
    }
    let mut body = Vec::new();
    ensure!(
        headers.get_all("content-encoding").iter().count() <= 1,
        "duplicate Content-Encoding"
    );
    if let Some(encoding) = headers.get("content-encoding") {
        ensure!(
            encoding == "identity",
            "unexpected compressed update response"
        );
    }
    let lengths: Vec<_> = headers.get_all("content-length").iter().collect();
    ensure!(lengths.len() <= 1, "duplicate Content-Length");
    ensure!(
        headers.get_all("transfer-encoding").iter().count() <= 1,
        "duplicate Transfer-Encoding"
    );
    if let Some(encoding) = headers.get("transfer-encoding") {
        ensure!(
            lengths.is_empty() && encoding == "chunked",
            "unsupported HTTP transfer encoding"
        );
        loop {
            let mut chunk_budget = 1024;
            let bytes = line(&mut reader, &mut chunk_budget).await?;
            let text = std::str::from_utf8(&bytes)?
                .split(';')
                .next()
                .context("empty chunk size")?;
            let length = usize::from_str_radix(text, 16)?;
            ensure!(
                length <= limit.saturating_sub(body.len()),
                "HTTP response exceeds size limit"
            );
            if length == 0 {
                let mut trailer_budget = 4096;
                while !line(&mut reader, &mut trailer_budget).await?.is_empty() {}
                break;
            }
            let old = body.len();
            body.resize(old + length, 0);
            reader.read_exact(&mut body[old..]).await?;
            let mut end = [0; 2];
            reader.read_exact(&mut end).await?;
            ensure!(end == *b"\r\n", "invalid HTTP chunk terminator");
        }
    } else if let Some(length) = lengths.first() {
        let length: usize = length.to_str()?.parse()?;
        ensure!(length <= limit, "HTTP response exceeds size limit");
        body.resize(length, 0);
        reader.read_exact(&mut body).await?;
    } else {
        reader.take(limit as u64 + 1).read_to_end(&mut body).await?;
        ensure!(body.len() <= limit, "HTTP response exceeds size limit");
    }
    Ok(Response {
        status,
        headers,
        body,
    })
}

pub async fn get(network: &Network, host: &str, path: &str, limit: usize) -> Result<Response> {
    if let Ok(Ok(response)) = timeout(
        Duration::from_secs(20),
        h2_transport::get(network, host, path, limit),
    )
    .await
    {
        return Ok(Response {
            status: response.status,
            headers: response.headers.clone(),
            body: response.body.to_vec(),
        });
    }
    timeout(Duration::from_secs(40), async {
        ensure!(path.starts_with('/') && !path.contains(['\r', '\n']), "invalid update path");
        let mut stream = network.stream(host, 443, host, true, false).await?;
        let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: TgWsProxyPlus/{}\r\nAccept: application/vnd.github+json\r\nAccept-Encoding: identity\r\nX-GitHub-Api-Version: 2022-11-28\r\nConnection: close\r\n\r\n", env!("CARGO_PKG_VERSION"));
        stream.write_all(request.as_bytes()).await?;
        stream.flush().await?;
        read_response(stream, limit).await
    }).await.context("update HTTP request timed out")?
}
