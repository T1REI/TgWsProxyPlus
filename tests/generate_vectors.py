import hashlib
import json
import pathlib
import sys

from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "upstream"))
from proxy.tg_ws_proxy import _try_handshake, _generate_relay_init, _build_crypto_ctx
from proxy.fake_tls import build_server_hello, verify_client_hello


def stream(key, iv):
    return Cipher(algorithms.AES(key), modes.CTR(iv)).encryptor()


def generate():
    secret = bytes.fromhex("00112233445566778899aabbccddeeff")
    vectors = []
    for tag in (b"\xef" * 4, b"\xee" * 4, b"\xdd" * 4):
        for index in (1, -2, 203, 10003, -10002):
            raw = bytearray(hashlib.shake_256(tag + str(index).encode()).digest(64))
            raw[56:60] = tag
            raw[60:62] = index.to_bytes(2, "little", signed=True)
            key = hashlib.sha256(bytes(raw[8:40]) + secret).digest()
            tx = stream(key, bytes(raw[40:56]))
            encrypted = tx.update(bytes(raw))
            handshake = bytes(raw[:56]) + encrypted[56:]
            dc, media, actual_tag, material = _try_handshake(handshake, secret)
            assert (dc, media, actual_tag) == (abs(index), index < 0, tag)
            relay = _generate_relay_init(tag, -2 if media else 2)
            context = _build_crypto_ctx(material, secret, relay)
            plain = bytes(range(256)) * 3
            client_up = tx.update(plain)
            assert context.clt_dec.update(client_up) == plain
            relay_up = context.tg_enc.update(plain)
            reverse = relay[8:56][::-1]
            remote_tx = stream(reverse[:32], reverse[32:])
            relay_down = remote_tx.update(plain[::-1])
            assert context.tg_dec.update(relay_down) == plain[::-1]
            client_down = context.clt_enc.update(plain[::-1])
            vectors.append(dict(secret=secret.hex(), handshake=handshake.hex(), dc=abs(index) % 10000,
                                media=media, test=abs(index) >= 10000, tag=tag.hex(),
                                plain=plain.hex(), client_up=client_up.hex(), client_down=client_down.hex(),
                                relay=relay.hex(), relay_up=relay_up.hex(), relay_down=relay_down.hex()))
    hello = bytearray(517)
    hello[:11] = b"\x16\x03\x01\x02\x00\x01\x00\x01\xfc\x03\x03"
    hello[43] = 32
    hello[44:76] = bytes(range(32))
    import hmac
    signature = bytearray(hmac.digest(secret, hello, "sha256"))
    timestamp = 1780000000
    for i, byte in enumerate(timestamp.to_bytes(4, "little")):
        signature[28 + i] ^= byte
    hello[11:43] = signature
    import unittest.mock
    with unittest.mock.patch("proxy.fake_tls.time.time", return_value=timestamp):
        assert verify_client_hello(hello, secret) is not None
    response = build_server_hello(secret, bytes(signature), bytes(range(32)))
    output = dict(upstream_commit="307a384ab7f522e34bb1a045884d0bd90f40bee5", vectors=vectors,
                  tls=dict(secret=secret.hex(), hello=hello.hex(), response=response.hex(), timestamp=timestamp))
    directory = ROOT / "tests" / "fixtures"
    directory.mkdir(exist_ok=True)
    (directory / "python-reference.json").write_text(json.dumps(output, indent=2) + "\n", encoding="utf-8")
    print(f"Generated {len(vectors)} upstream crypto vectors and Fake TLS reference")


if __name__ == "__main__":
    generate()
