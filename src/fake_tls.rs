use crate::crypto::random;
use anyhow::{Result, ensure};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub struct Hello {
    pub random: [u8; 32],
    pub session: [u8; 32],
    pub timestamp: u32,
}

pub fn digest(secret: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts every key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

pub fn verify_hello(data: &[u8], secret: &[u8; 16], now: u64) -> Result<Hello> {
    ensure!(
        data.len() >= 76 && data[0] == 0x16 && data[5] == 1,
        "invalid Fake TLS ClientHello"
    );
    ensure!(
        usize::from(u16::from_be_bytes([data[3], data[4]])) + 5 == data.len(),
        "invalid TLS record length"
    );
    ensure!(data[43] == 32, "invalid TLS session ID length");
    let random: [u8; 32] = data[11..43].try_into()?;
    let mut zeroed = data.to_vec();
    zeroed[11..43].fill(0);
    let mut mac = Hmac::<Sha256>::new_from_slice(secret)?;
    mac.update(&zeroed);
    mac.verify_truncated_left(&random[..28])
        .map_err(|_| anyhow::anyhow!("Fake TLS authentication failed"))?;
    let expected = digest(secret, &zeroed);
    let mut time = [0; 4];
    for i in 0..4 {
        time[i] = random[28 + i] ^ expected[28 + i];
    }
    let timestamp = u32::from_le_bytes(time);
    ensure!(
        now.abs_diff(timestamp as u64) <= 120,
        "expired Fake TLS ClientHello"
    );
    Ok(Hello {
        random,
        session: data[44..76].try_into()?,
        timestamp,
    })
}

pub fn server_hello(secret: &[u8; 16], hello: &Hello) -> Result<Vec<u8>> {
    let mut result = vec![0x16, 3, 3, 0, 0x7a, 2, 0, 0, 0x76, 3, 3];
    result.extend_from_slice(&[0; 32]);
    result.push(32);
    result.extend_from_slice(&hello.session);
    result.extend_from_slice(&[0x13, 1, 0, 0, 0x2e, 0, 0x33, 0, 0x24, 0, 0x1d, 0, 0x20]);
    result.extend_from_slice(&random::<32>()?);
    result.extend_from_slice(&[0, 0x2b, 0, 2, 3, 4]);
    result.extend_from_slice(&[0x14, 3, 3, 0, 1, 1]);
    let size = 1900 + u16::from_le_bytes(random::<2>()?) as usize % 201;
    result.extend_from_slice(&[0x17, 3, 3]);
    result.extend_from_slice(&(size as u16).to_be_bytes());
    let mut padding = vec![0; size];
    getrandom::fill(&mut padding)?;
    result.extend_from_slice(&padding);
    let mut signed = hello.random.to_vec();
    signed.extend_from_slice(&result);
    result[11..43].copy_from_slice(&digest(secret, &signed));
    Ok(result)
}

pub fn wrap_records(data: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(data.len() + data.len().div_ceil(16384) * 5);
    for chunk in data.chunks(16384) {
        output.extend_from_slice(&[0x17, 3, 3]);
        output.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
        output.extend_from_slice(chunk);
    }
    output
}

pub struct FakeTls<S> {
    inner: S,
    header: [u8; 5],
    header_read: usize,
    remaining: usize,
    ccs: bool,
    outgoing: Vec<u8>,
    written: usize,
}

impl<S> FakeTls<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            header: [0; 5],
            header_read: 0,
            remaining: 0,
            ccs: false,
            outgoing: Vec::new(),
            written: 0,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for FakeTls<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            if this.remaining == 0 {
                while this.header_read < 5 {
                    let mut part = ReadBuf::new(&mut this.header[this.header_read..]);
                    std::task::ready!(Pin::new(&mut this.inner).poll_read(cx, &mut part))?;
                    let n = part.filled().len();
                    if n == 0 {
                        return Poll::Ready(if this.header_read == 0 {
                            Ok(())
                        } else {
                            Err(io::Error::from(io::ErrorKind::UnexpectedEof))
                        });
                    }
                    this.header_read += n;
                }
                this.remaining = u16::from_be_bytes([this.header[3], this.header[4]]) as usize;
                this.header_read = 0;
                if this.header[0] == 0x15 {
                    return Poll::Ready(Ok(()));
                }
                if this.header[1..3] != [3, 3]
                    || !matches!(this.header[0], 0x14 | 0x17)
                    || this.remaining == 0
                    || this.remaining > 18432
                {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid Fake TLS record",
                    )));
                }
                this.ccs = this.header[0] == 0x14;
                if this.ccs && this.remaining != 1 {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid TLS CCS",
                    )));
                }
            }
            if this.ccs {
                let mut ccs = [0];
                let mut part = ReadBuf::new(&mut ccs);
                std::task::ready!(Pin::new(&mut this.inner).poll_read(cx, &mut part))?;
                if part.filled() != [1] {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid TLS CCS payload",
                    )));
                }
                this.remaining = 0;
                continue;
            }
            let count = this.remaining.min(buf.remaining());
            let mut part = ReadBuf::new(buf.initialize_unfilled_to(count));
            std::task::ready!(Pin::new(&mut this.inner).poll_read(cx, &mut part))?;
            let n = part.filled().len();
            if n == 0 {
                return Poll::Ready(Err(io::Error::from(io::ErrorKind::UnexpectedEof)));
            }
            this.remaining -= n;
            buf.advance(n);
            return Poll::Ready(Ok(()));
        }
    }
}

impl<S: AsyncWrite + Unpin> FakeTls<S> {
    fn drain(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.written < self.outgoing.len() {
            let n = std::task::ready!(
                Pin::new(&mut self.inner).poll_write(cx, &self.outgoing[self.written..])
            )?;
            if n == 0 {
                return Poll::Ready(Err(io::Error::from(io::ErrorKind::WriteZero)));
            }
            self.written += n;
        }
        self.outgoing.clear();
        self.written = 0;
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for FakeTls<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        std::task::ready!(this.drain(cx))?;
        let size = buf.len().min(16384);
        this.outgoing = wrap_records(&buf[..size]);
        Poll::Ready(Ok(size))
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        std::task::ready!(this.drain(cx))?;
        Pin::new(&mut this.inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        std::task::ready!(this.drain(cx))?;
        std::task::ready!(Pin::new(&mut this.inner).poll_flush(cx))?;
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}
