use crate::crypto::{Protocol, random};
use anyhow::{Result, ensure};
use bytes::{Bytes, BytesMut};

pub const MAX_PACKET: usize = 4 * 1024 * 1024;

pub struct Splitter {
    protocol: Protocol,
    header: [u8; 4],
    header_len: usize,
    remaining: Option<usize>,
    encrypted: BytesMut,
    disabled: bool,
}

pub fn packet_length(protocol: Protocol, input: &[u8]) -> Result<Option<(usize, usize, bool)>> {
    if input.is_empty() {
        return Ok(None);
    }
    let (header, length, quick) = if protocol == Protocol::Abridged {
        let words = input[0] & 0x7f;
        if words == 0x7f {
            if input.len() < 4 {
                return Ok(None);
            }
            (
                4,
                u32::from_le_bytes([input[1], input[2], input[3], 0]) as usize * 4,
                input[0] & 0x80 != 0,
            )
        } else {
            (1, words as usize * 4, input[0] & 0x80 != 0)
        }
    } else {
        if input.len() < 4 {
            return Ok(None);
        }
        let value = u32::from_le_bytes(input[..4].try_into()?);
        (4, (value & 0x7fff_ffff) as usize, value & 0x8000_0000 != 0)
    };
    ensure!(
        length <= MAX_PACKET + 256,
        "MTProto packet exceeds supported size"
    );
    Ok(Some((header, length, quick)))
}

impl Splitter {
    pub fn new(protocol: Protocol) -> Self {
        Self {
            protocol,
            header: [0; 4],
            header_len: 0,
            remaining: None,
            encrypted: BytesMut::new(),
            disabled: false,
        }
    }

    pub fn push(&mut self, encrypted: &[u8], plain: &[u8]) -> Result<Vec<Bytes>> {
        ensure!(
            encrypted.len() == plain.len(),
            "ciphertext/plaintext length mismatch"
        );
        let mut offset = 0;
        let mut parts = Vec::new();
        while offset < encrypted.len() {
            if self.disabled {
                self.encrypted.extend_from_slice(&encrypted[offset..]);
                parts.push(self.encrypted.split().freeze());
                break;
            }
            if self.remaining.is_none() {
                let needed = if self.protocol == Protocol::Abridged
                    && (self.header_len == 0 || self.header[0] & 0x7f != 0x7f)
                {
                    1
                } else {
                    4
                };
                let count = (needed - self.header_len).min(plain.len() - offset);
                self.header[self.header_len..self.header_len + count]
                    .copy_from_slice(&plain[offset..offset + count]);
                self.encrypted
                    .extend_from_slice(&encrypted[offset..offset + count]);
                self.header_len += count;
                offset += count;
                let Some((_, length, _)) =
                    packet_length(self.protocol, &self.header[..self.header_len])?
                else {
                    continue;
                };
                if length == 0 {
                    self.disabled = true;
                    continue;
                }
                self.remaining = Some(length);
            }
            let remaining = self.remaining.unwrap_or(0);
            let count = remaining.min(encrypted.len() - offset);
            self.encrypted
                .extend_from_slice(&encrypted[offset..offset + count]);
            offset += count;
            self.remaining = Some(remaining - count);
            if count == remaining {
                parts.push(self.encrypted.split().freeze());
                self.header_len = 0;
                self.remaining = None;
            }
        }
        if self.disabled && !self.encrypted.is_empty() {
            parts.push(self.encrypted.split().freeze());
        }
        Ok(parts)
    }

    pub fn flush(&mut self) -> Bytes {
        self.header_len = 0;
        self.remaining = None;
        self.encrypted.split().freeze()
    }
}

pub fn strip_padding(protocol: Protocol, mut body: Bytes) -> Result<Bytes> {
    ensure!(
        body.len() >= 24 && body.len() <= MAX_PACKET + 256,
        "invalid HTTP MTProto packet size"
    );
    if protocol == Protocol::Padded {
        let (length, max_padding) = if body[..8] == [0; 8] {
            (
                20usize
                    .checked_add(u32::from_le_bytes(body[16..20].try_into()?) as usize)
                    .ok_or_else(|| anyhow::anyhow!("packet length overflow"))?,
                256,
            )
        } else {
            (24 + (body.len() - 24) / 16 * 16, 15)
        };
        ensure!(
            (24..=body.len()).contains(&length) && body.len() - length <= max_padding,
            "invalid MTProto padding"
        );
        body.truncate(length);
    }
    ensure!(
        body.len().is_multiple_of(4) && body.len() <= MAX_PACKET,
        "invalid MTProto alignment"
    );
    Ok(body)
}

pub fn encode_reply(protocol: Protocol, body: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        !body.is_empty() && body.len().is_multiple_of(4) && body.len() <= MAX_PACKET,
        "invalid MTProto reply size"
    );
    let mut result = Vec::with_capacity(body.len() + 8);
    if protocol == Protocol::Abridged {
        let words = body.len() / 4;
        if words < 0x7f {
            result.push(words as u8);
        } else {
            result.push(0x7f);
            result.extend_from_slice(&(words as u32).to_le_bytes()[..3]);
        }
        result.extend_from_slice(body);
    } else {
        let padding = if protocol == Protocol::Padded {
            (random::<1>()?[0] % 4) as usize
        } else {
            0
        };
        result.extend_from_slice(&((body.len() + padding) as u32).to_le_bytes());
        result.extend_from_slice(body);
        result.extend_from_slice(&random::<3>()?[..padding]);
    }
    Ok(result)
}
