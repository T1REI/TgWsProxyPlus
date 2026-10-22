use ctr::cipher::StreamCipher;
use serde::Deserialize;
use tg_ws_proxy_plus::{
    crypto::{self, Protocol},
    fake_tls,
    framing::{self, Splitter},
};

#[test]
fn splitter_handles_quick_ack_flags_and_zero_length_passthrough() {
    for protocol in [Protocol::Abridged, Protocol::Intermediate, Protocol::Padded] {
        let mut frame = framing::encode_reply(protocol, &[7; 128]).unwrap();
        if protocol == Protocol::Abridged {
            frame[0] |= 0x80;
        } else {
            frame[3] |= 0x80;
        }
        let mut splitter = Splitter::new(protocol);
        let encrypted = vec![0xa5; frame.len()];
        assert_eq!(
            splitter.push(&encrypted, &frame).unwrap()[0].as_ref(),
            encrypted
        );
        let zero = if protocol == Protocol::Abridged {
            vec![0x80]
        } else {
            vec![0, 0, 0, 0x80]
        };
        assert_eq!(splitter.push(&zero, &zero).unwrap()[0].as_ref(), zero);
        assert_eq!(
            splitter.push(&[0xff; 8], &[0xff; 8]).unwrap()[0].as_ref(),
            &[0xff; 8]
        );
    }
}

#[derive(Deserialize)]
struct Reference {
    vectors: Vec<Vector>,
    tls: Tls,
}
#[derive(Deserialize)]
struct Vector {
    secret: String,
    handshake: String,
    dc: u16,
    media: bool,
    test: bool,
    tag: String,
    plain: String,
    client_up: String,
    client_down: String,
    relay: String,
    relay_up: String,
    relay_down: String,
}
#[derive(Deserialize)]
struct Tls {
    secret: String,
    hello: String,
    response: String,
    timestamp: u64,
}
fn bytes(s: &str) -> Vec<u8> {
    crypto::unhex(s).unwrap()
}
fn reference() -> Reference {
    serde_json::from_str(include_str!("fixtures/python-reference.json")).unwrap()
}

#[test]
fn matches_original_python_handshakes_and_both_cipher_directions() {
    for vector in reference().vectors {
        let handshake: [u8; 64] = bytes(&vector.handshake).try_into().unwrap();
        let secret = bytes(&vector.secret).try_into().unwrap();
        let mut init = crypto::accept_init(&handshake, &secret, false).unwrap();
        assert_eq!(
            (init.dc, init.media, init.test),
            (vector.dc, vector.media, vector.test)
        );
        assert_eq!(init.protocol.tag().to_vec(), bytes(&vector.tag));
        let mut up = bytes(&vector.client_up);
        init.decrypt.apply_keystream(&mut up);
        assert_eq!(up, bytes(&vector.plain));
        let mut down = bytes(&vector.plain);
        down.reverse();
        init.encrypt.apply_keystream(&mut down);
        assert_eq!(down, bytes(&vector.client_down));
        let relay = bytes(&vector.relay);
        let mut tx = crypto::cipher(&relay[8..40], &relay[40..56]);
        tx.apply_keystream(&mut [0; 64]);
        tx.apply_keystream(&mut up);
        assert_eq!(up, bytes(&vector.relay_up));
        let mut reversed = relay[8..56].to_vec();
        reversed.reverse();
        let mut rx = crypto::cipher(&reversed[..32], &reversed[32..]);
        let mut received = bytes(&vector.relay_down);
        rx.apply_keystream(&mut received);
        let mut expected = bytes(&vector.plain);
        expected.reverse();
        assert_eq!(received, expected);
        assert!(crypto::accept_init(&handshake, &[0xff; 16], false).is_err());
    }
}

#[test]
fn generated_relay_init_is_standard_obfuscation_without_proxy_secret() {
    for protocol in [Protocol::Abridged, Protocol::Intermediate, Protocol::Padded] {
        for media in [false, true] {
            let mut relay = crypto::relay_init(protocol, 4, media).unwrap();
            let mut decrypt = crypto::cipher(&relay.bytes[8..40], &relay.bytes[40..56]);
            let mut plain = relay.bytes;
            decrypt.apply_keystream(&mut plain);
            assert_eq!(plain[56..60], protocol.tag());
            assert_eq!(
                i16::from_le_bytes(plain[60..62].try_into().unwrap()),
                if media { -4 } else { 4 }
            );
            let mut tx = vec![0x55; 1003];
            relay.encrypt.apply_keystream(&mut tx);
            decrypt.apply_keystream(&mut tx);
            assert_eq!(tx, vec![0x55; 1003]);
            assert_ne!(relay.bytes[0], 0xef);
            assert_ne!(relay.bytes[4..8], [0; 4]);
        }
    }
}

#[test]
fn aes256_ctr_matches_nist_sp800_38a_across_partial_blocks() {
    let key = bytes("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4");
    let iv = bytes("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff");
    let mut data = bytes("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51");
    let mut cipher = crypto::cipher(&key, &iv);
    for part in data.chunks_mut(3) {
        cipher.apply_keystream(part);
    }
    assert_eq!(
        crypto::hex(&data),
        "601ec313775789a5b7a7f504bbf3d228f443e3ca4d62b59aca84e990cacaf5c5"
    );
}

#[test]
fn fake_tls_authenticates_python_hello_and_binds_server_response() {
    let vector = reference().tls;
    let data = bytes(&vector.hello);
    let secret = bytes(&vector.secret).try_into().unwrap();
    let hello = fake_tls::verify_hello(&data, &secret, vector.timestamp).unwrap();
    assert_eq!(hello.timestamp as u64, vector.timestamp);
    assert_eq!(hello.session, std::array::from_fn(|i| i as u8));
    assert!(fake_tls::verify_hello(&data, &[7; 16], vector.timestamp).is_err());
    assert!(fake_tls::verify_hello(&data, &secret, vector.timestamp + 121).is_err());
    let mut tampered = data.clone();
    tampered[300] ^= 1;
    assert!(fake_tls::verify_hello(&tampered, &secret, vector.timestamp).is_err());
    for response in [
        bytes(&vector.response),
        fake_tls::server_hello(&secret, &hello).unwrap(),
    ] {
        assert_eq!(response[44..76], hello.session);
        let signature = response[11..43].to_vec();
        let mut unsigned = response;
        unsigned[11..43].fill(0);
        let mut signed = hello.random.to_vec();
        signed.extend(unsigned);
        assert_eq!(signature, fake_tls::digest(&secret, &signed));
    }
}

#[test]
fn splitter_preserves_packets_at_every_header_and_payload_boundary() {
    for protocol in [Protocol::Abridged, Protocol::Intermediate, Protocol::Padded] {
        let first = framing::encode_reply(protocol, &vec![9; 1024]).unwrap();
        let second = framing::encode_reply(protocol, &[1; 24]).unwrap();
        let plain = [first.clone(), second.clone()].concat();
        let encrypted: Vec<_> = plain.iter().map(|b| b ^ 0x5a).collect();
        for chunk_size in [1, 2, 3, 4, 5, 31, 1024, 4096] {
            let mut splitter = Splitter::new(protocol);
            let mut packets = Vec::new();
            for (encrypted, plain) in encrypted.chunks(chunk_size).zip(plain.chunks(chunk_size)) {
                packets.extend(splitter.push(encrypted, plain).unwrap());
            }
            assert_eq!(packets.len(), 2);
            assert_eq!(packets[0].len(), first.len());
            assert_eq!(packets[1].len(), second.len());
            assert_eq!(packets.concat(), encrypted);
            assert!(splitter.flush().is_empty());
        }
    }
}

#[test]
fn framing_rejects_oversized_lengths_and_strips_only_valid_padding() {
    assert!(framing::packet_length(Protocol::Intermediate, &u32::MAX.to_le_bytes()).is_err());
    assert!(framing::packet_length(Protocol::Abridged, &[0x7f, 0xff, 0xff, 0xff]).is_err());
    let mut plain = vec![0; 44];
    plain[16..20].copy_from_slice(&4u32.to_le_bytes());
    assert_eq!(
        framing::strip_padding(Protocol::Padded, plain.into())
            .unwrap()
            .len(),
        24
    );
    assert!(framing::strip_padding(Protocol::Intermediate, vec![0; 25].into()).is_err());
    for protocol in [Protocol::Abridged, Protocol::Intermediate, Protocol::Padded] {
        let mut splitter = Splitter::new(protocol);
        assert!(splitter.push(&[1, 2, 3], &[1, 2, 3]).unwrap().is_empty());
        assert_eq!(splitter.flush().as_ref(), &[1, 2, 3]);
        assert!(splitter.flush().is_empty());
    }
}
