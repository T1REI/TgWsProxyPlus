use aes::Aes256;
use anyhow::{Result, bail, ensure};
use ctr::cipher::{KeyIvInit, StreamCipher};
use sha2::{Digest, Sha256};

pub type Cipher = ctr::Ctr128BE<Aes256>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protocol {
    Abridged,
    Intermediate,
    Padded,
}

impl Protocol {
    pub fn tag(self) -> [u8; 4] {
        [match self {
            Self::Abridged => 0xef,
            Self::Intermediate => 0xee,
            Self::Padded => 0xdd,
        }; 4]
    }

    pub fn from_tag(tag: &[u8]) -> Result<Self> {
        match tag {
            [0xef, 0xef, 0xef, 0xef] => Ok(Self::Abridged),
            [0xee, 0xee, 0xee, 0xee] => Ok(Self::Intermediate),
            [0xdd, 0xdd, 0xdd, 0xdd] => Ok(Self::Padded),
            _ => bail!("invalid MTProto handshake (secret or protocol)"),
        }
    }
}

pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut data = [0; N];
    getrandom::fill(&mut data)?;
    Ok(data)
}

pub fn hex(data: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut result = String::with_capacity(data.len() * 2);
    for &byte in data {
        result.push(DIGITS[(byte >> 4) as usize] as char);
        result.push(DIGITS[(byte & 15) as usize] as char);
    }
    result
}

pub fn unhex(value: &str) -> Result<Vec<u8>> {
    ensure!(
        value.len().is_multiple_of(2) && value.is_ascii(),
        "invalid hexadecimal value"
    );
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digit = |b: u8| {
                (b as char)
                    .to_digit(16)
                    .ok_or_else(|| anyhow::anyhow!("invalid hexadecimal value"))
            };
            Ok(((digit(pair[0])? << 4) | digit(pair[1])?) as u8)
        })
        .collect()
}

pub fn cipher(key: &[u8], iv: &[u8]) -> Cipher {
    Cipher::new(
        key.try_into().expect("32-byte internal key"),
        iv.try_into().expect("16-byte internal IV"),
    )
}

fn secret_cipher(material: &[u8; 48], secret: &[u8; 16]) -> Cipher {
    let mut hash = Sha256::new();
    hash.update(&material[..32]);
    hash.update(secret);
    cipher(&hash.finalize(), &material[32..])
}

pub struct ClientInit {
    pub dc: u16,
    pub media: bool,
    pub test: bool,
    pub protocol: Protocol,
    pub decrypt: Cipher,
    pub encrypt: Cipher,
}

pub fn accept_init(init: &[u8; 64], secret: &[u8; 16], force_test: bool) -> Result<ClientInit> {
    let material: [u8; 48] = init[8..56].try_into()?;
    let mut decrypt = secret_cipher(&material, secret);
    let mut plain = *init;
    decrypt.apply_keystream(&mut plain);
    let protocol = Protocol::from_tag(&plain[56..60])?;
    let index = i16::from_le_bytes(plain[60..62].try_into()?);
    let mut dc = index.unsigned_abs();
    let test = force_test || dc >= 10000;
    if dc >= 10000 {
        dc -= 10000;
    }
    ensure!(
        matches!(dc, 1..=5 | 203) && (!test || dc <= 3),
        "unsupported Telegram DC {dc}"
    );
    let mut reverse = material;
    reverse.reverse();
    Ok(ClientInit {
        dc,
        media: index < 0,
        test,
        protocol,
        decrypt,
        encrypt: secret_cipher(&reverse, secret),
    })
}

pub struct RelayInit {
    pub bytes: [u8; 64],
    pub encrypt: Cipher,
    pub decrypt: Cipher,
}

pub fn relay_init(protocol: Protocol, dc: u16, media: bool) -> Result<RelayInit> {
    let mut bytes = loop {
        let data = random::<64>()?;
        if data[0] != 0xef
            && data[4..8] != [0; 4]
            && ![
                b"HEAD",
                b"POST",
                b"GET ",
                b"OPTI",
                &[0xee; 4],
                &[0xdd; 4],
                &[0x16, 3, 1, 2],
            ]
            .iter()
            .any(|p| data[..4] == **p)
        {
            break data;
        }
    };
    let mut encrypt = cipher(&bytes[8..40], &bytes[40..56]);
    let mut reverse: [u8; 48] = bytes[8..56].try_into()?;
    reverse.reverse();
    let decrypt = cipher(&reverse[..32], &reverse[32..]);
    bytes[56..60].copy_from_slice(&protocol.tag());
    let index = if media { -(dc as i16) } else { dc as i16 };
    bytes[60..62].copy_from_slice(&index.to_le_bytes());
    let mut encrypted = bytes;
    encrypt.apply_keystream(&mut encrypted);
    bytes[56..64].copy_from_slice(&encrypted[56..64]);
    Ok(RelayInit {
        bytes,
        encrypt,
        decrypt,
    })
}
