mod support;

use bytes::Bytes;
use ctr::cipher::StreamCipher;
use std::{sync::Arc, time::Duration};
use tg_ws_proxy_plus::{crypto::Protocol, framing, h2_transport::HttpLane, server, stats::Stats};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
    time::timeout,
};

struct Request {
    body: Bytes,
    respond: h2::server::SendResponse<Bytes>,
}
struct Harness {
    lane: Arc<HttpLane>,
    requests: mpsc::Receiver<Request>,
    driver: tokio::task::JoinHandle<()>,
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

async fn harness() -> Harness {
    let (client, remote) = tokio::io::duplex(8192);
    let (sender, requests) = mpsc::channel(32);
    let driver = tokio::spawn(async move {
        let mut connection = h2::server::handshake(remote).await.unwrap();
        let mut tasks = tokio::task::JoinSet::new();
        while let Some(request) = connection.accept().await {
            let Ok((request, respond)) = request else {
                break;
            };
            let sender = sender.clone();
            tasks.spawn(async move {
                let mut body = request.into_body();
                let mut buffer = Vec::new();
                while let Some(chunk) = body.data().await {
                    let Ok(chunk) = chunk else {
                        return;
                    };
                    buffer.extend_from_slice(&chunk);
                    body.flow_control().release_capacity(chunk.len()).unwrap();
                }
                let _ = sender
                    .send(Request {
                        body: buffer.into(),
                        respond,
                    })
                    .await;
            });
        }
    });
    let lane = HttpLane::from_stream(Box::new(client), "localhost", Arc::new(Stats::default()))
        .await
        .unwrap();
    Harness {
        lane,
        requests,
        driver,
    }
}

fn respond(request: &mut Request, status: u16, body: Bytes) {
    let response = http::Response::builder()
        .status(status)
        .header("content-type", "application/octet-stream")
        .header("content-length", body.len())
        .body(())
        .unwrap();
    let mut output = request
        .respond
        .send_response(response, body.is_empty())
        .unwrap();
    if !body.is_empty() {
        output.send_data(body, true).unwrap();
    }
}

#[tokio::test]
async fn stalled_encrypted_request_is_replayed_byte_for_byte() {
    timeout(Duration::from_secs(8), async {
        let mut harness = harness().await;
        let (mut peer, client) = tokio::io::duplex(4096);
        let (init, mut tx, mut rx) = support::client(Protocol::Intermediate);
        let lane = harness.lane.clone();
        let bridge = tokio::spawn(async move {
            server::bridge_h2(Box::new(client), lane, init, Arc::new(Stats::default())).await
        });
        let body = [9; 56];
        let mut frame = framing::encode_reply(Protocol::Intermediate, &body).unwrap();
        tx.apply_keystream(&mut frame);
        peer.write_all(&frame).await.unwrap();
        let original = harness.requests.recv().await.unwrap();
        assert_eq!(original.body.as_ref(), body);
        let mut replay = harness.requests.recv().await.unwrap();
        assert_eq!(replay.body, original.body);
        respond(&mut replay, 200, Bytes::from_static(&[5; 40]));
        let mut received = [0; 44];
        peer.read_exact(&mut received).await.unwrap();
        rx.apply_keystream(&mut received);
        assert_eq!(
            received.to_vec(),
            framing::encode_reply(Protocol::Intermediate, &[5; 40]).unwrap()
        );
        drop(peer);
        let _ = bridge.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn partially_received_response_is_never_replayed_or_reset() {
    timeout(Duration::from_secs(8), async {
        let mut harness = harness().await;
        let (mut peer, client) = tokio::io::duplex(4096);
        let (init, mut tx, mut rx) = support::client(Protocol::Intermediate);
        let lane = harness.lane.clone();
        let bridge = tokio::spawn(async move {
            server::bridge_h2(Box::new(client), lane, init, Arc::new(Stats::default())).await
        });
        let mut frame = framing::encode_reply(Protocol::Intermediate, &[9; 56]).unwrap();
        tx.apply_keystream(&mut frame);
        peer.write_all(&frame).await.unwrap();
        let mut original = harness.requests.recv().await.unwrap();
        let response = http::Response::builder()
            .status(200)
            .header("content-length", 40)
            .body(())
            .unwrap();
        let mut output = original.respond.send_response(response, false).unwrap();
        output
            .send_data(Bytes::from_static(&[5; 4]), false)
            .unwrap();
        tokio::time::sleep(Duration::from_millis(3500)).await;
        assert!(harness.requests.try_recv().is_err());
        output
            .send_data(Bytes::from_static(&[5; 36]), true)
            .unwrap();
        let mut received = [0; 44];
        peer.read_exact(&mut received).await.unwrap();
        rx.apply_keystream(&mut received);
        assert_eq!(&received[4..], &[5; 40]);
        drop(peer);
        let _ = bridge.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn negative_http_transport_error_is_delivered_encrypted_to_native_client() {
    timeout(Duration::from_secs(3), async {
        let mut harness = harness().await;
        let (mut peer, client) = tokio::io::duplex(4096);
        let (init, mut tx, mut rx) = support::client(Protocol::Intermediate);
        let lane = harness.lane.clone();
        let bridge = tokio::spawn(async move {
            server::bridge_h2(Box::new(client), lane, init, Arc::new(Stats::default())).await
        });
        let mut frame = framing::encode_reply(Protocol::Intermediate, &[9; 56]).unwrap();
        tx.apply_keystream(&mut frame);
        peer.write_all(&frame).await.unwrap();
        let mut original = harness.requests.recv().await.unwrap();
        respond(&mut original, 403, Bytes::new());
        let mut received = [0; 8];
        peer.read_exact(&mut received).await.unwrap();
        rx.apply_keystream(&mut received);
        assert_eq!(received[..4], 4u32.to_le_bytes());
        assert_eq!(received[4..], (-403i32).to_le_bytes());
        assert!(bridge.await.unwrap().is_err());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn channel_rotates_stalled_headers_to_make_room_for_new_uploads() {
    timeout(Duration::from_secs(12), async {
        let mut harness = harness().await;
        let (mut peer, client) = tokio::io::duplex(4096);
        let (init, mut tx, _rx) = support::client(Protocol::Intermediate);
        let lane = harness.lane.clone();
        let bridge = tokio::spawn(async move {
            server::bridge_h2(Box::new(client), lane, init, Arc::new(Stats::default())).await
        });
        for id in 0..9u8 {
            let mut body = [9; 56];
            body[24] = id;
            let mut frame = framing::encode_reply(Protocol::Intermediate, &body).unwrap();
            tx.apply_keystream(&mut frame);
            peer.write_all(&frame).await.unwrap();
        }
        let mut retained = Vec::new();
        loop {
            let request = harness.requests.recv().await.unwrap();
            let done = request.body[24] == 8;
            retained.push(request);
            if done {
                break;
            }
        }
        drop(peer);
        let _ = bridge.await.unwrap();
    })
    .await
    .unwrap();
}
