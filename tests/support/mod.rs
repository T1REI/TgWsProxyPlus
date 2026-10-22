use ctr::cipher::StreamCipher;
use sha2::{Digest, Sha256};
use tg_ws_proxy_plus::crypto::{self, Cipher, ClientInit, Protocol};

pub fn client(protocol: Protocol) -> (ClientInit, Cipher, Cipher) {
    let secret = [0x12; 16];
    let mut handshake = crypto::random::<64>().unwrap();
    handshake[56..60].copy_from_slice(&protocol.tag());
    handshake[60..62].copy_from_slice(&(-2i16).to_le_bytes());
    let mut material = handshake[8..56].to_vec();
    let key = Sha256::digest([&material[..32], &secret].concat());
    let mut tx = crypto::cipher(&key, &material[32..]);
    let mut encrypted = handshake;
    tx.apply_keystream(&mut encrypted);
    handshake[56..].copy_from_slice(&encrypted[56..]);
    material.reverse();
    let key = Sha256::digest([&material[..32], &secret].concat());
    let rx = crypto::cipher(&key, &material[32..]);
    (
        crypto::accept_init(&handshake, &secret, false).unwrap(),
        tx,
        rx,
    )
}
