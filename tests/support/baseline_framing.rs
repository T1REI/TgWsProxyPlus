use anyhow::{Result, ensure};
use bytes::{Buf, Bytes, BytesMut};
use tg_ws_proxy_plus::crypto::Protocol;

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
