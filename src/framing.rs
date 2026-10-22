use crate::crypto::{Protocol, random};
use anyhow::{Result, ensure};
use bytes::{Buf, Bytes, BytesMut};

pub const MAX_PACKET: usize = 4 * 1024 * 1024;

pub struct Splitter {
    protocol: Protocol,
    plain: BytesMut,
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
            plain: BytesMut::new(),
            encrypted: BytesMut::new(),
            disabled: false,
        }
    }

    pub fn push(&mut self, encrypted: &[u8], plain: &[u8]) -> Result<Vec<Bytes>> {
        ensure!(
            encrypted.len() == plain.len(),
            "ciphertext/plaintext length mismatch"
        );
        if encrypted.is_empty() {
            return Ok(Vec::new());
        }
        if self.disabled {
            return Ok(vec![Bytes::copy_from_slice(encrypted)]);
        }
        self.plain.extend_from_slice(plain);
        self.encrypted.extend_from_slice(encrypted);
        let mut parts = Vec::new();
        loop {
            let Some((header, length, _)) = packet_length(self.protocol, &self.plain)? else {
                break;
            };
            if length == 0 {
                self.disabled = true;
                self.plain.clear();
                parts.push(self.encrypted.split().freeze());
                break;
            }
            if self.plain.len() < header + length {
                break;
            }
            self.plain.advance(header + length);
            parts.push(self.encrypted.split_to(header + length).freeze());
        }
        Ok(parts)
    }

    pub fn flush(&mut self) -> Bytes {
        self.plain.clear();
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
