pub struct BinaryProtocol;
impl tokio_tungstenite::tungstenite::handshake::server::Callback for BinaryProtocol {
    fn on_request(
        self,
        request: &tokio_tungstenite::tungstenite::handshake::server::Request,
        mut response: tokio_tungstenite::tungstenite::handshake::server::Response,
    ) -> Result<
        tokio_tungstenite::tungstenite::handshake::server::Response,
        tokio_tungstenite::tungstenite::handshake::server::ErrorResponse,
    > {
        assert_eq!(
            request.headers().get("Sec-WebSocket-Protocol").unwrap(),
            "binary"
        );
        response
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "binary".parse().unwrap());
        Ok(response)
    }
}

mod support;

use bytes::Bytes;
use ctr::cipher::StreamCipher;
use futures_util::{SinkExt, StreamExt};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::Relaxed},
    },
    time::Duration,
};
use tg_ws_proxy_plus::{
    config::Config,
    crypto::{self, Protocol},
    fake_tls::FakeTls,
    framing,
    h2_transport::HttpLane,
    network::{Network, websocket_handshake},
    routing::Routing,
    server,
    stats::Stats,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

#[tokio::test]
async fn tcp_bridge_translates_both_cipher_streams_with_backpressure() {
    timeout(Duration::from_secs(5), async {
        let (mut client, proxy_client) = tokio::io::duplex(127);
        let (proxy_remote, mut remote) = tokio::io::duplex(113);
        let (init, mut tx, mut rx) = support::client(Protocol::Padded);
        let relay = crypto::relay_init(init.protocol, 2, true).unwrap();
        let mut remote_rx = crypto::cipher(&relay.bytes[8..40], &relay.bytes[40..56]);
        remote_rx.apply_keystream(&mut [0; 64]);
        let mut reverse = relay.bytes[8..56].to_vec();
        reverse.reverse();
        let mut remote_tx = crypto::cipher(&reverse[..32], &reverse[32..]);
        let proxy = tokio::spawn(async move {
            server::bridge_tcp(
                Box::new(proxy_client),
                Box::new(proxy_remote),
                init,
                relay,
                &Stats::default(),
            )
            .await
        });
        let body: Vec<_> = (0..200_000).map(|i| (i % 251) as u8).collect();
        let expected = body.clone();
        let remote_task = tokio::spawn(async move {
            let mut received = vec![0; expected.len()];
            remote.read_exact(&mut received).await.unwrap();
            remote_rx.apply_keystream(&mut received);
            assert_eq!(received, expected);
            received.reverse();
            remote_tx.apply_keystream(&mut received);
            remote.write_all(&received).await.unwrap();
            remote
        });
        let mut wire = body.clone();
        tx.apply_keystream(&mut wire);
        client.write_all(&wire).await.unwrap();
        client.read_exact(&mut wire).await.unwrap();
        rx.apply_keystream(&mut wire);
        let mut expected = body;
        expected.reverse();
        assert_eq!(wire, expected);
        drop(client);
        drop(remote_task.await.unwrap());
        proxy.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn websocket_bridge_preserves_packet_boundaries_and_handles_ping() {
    timeout(Duration::from_secs(5), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (mut client, proxy_client) = tokio::io::duplex(1024);
        let (init, mut tx, mut rx) = support::client(Protocol::Intermediate);
        let relay = crypto::relay_init(init.protocol, 2, true).unwrap();
        let (done, keep) = tokio::sync::oneshot::channel::<()>();
        let remote_task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_hdr_async(tcp, BinaryProtocol)
                .await
                .unwrap();
            let init = ws.next().await.unwrap().unwrap().into_data();
            assert_eq!(init.len(), 64);
            let mut decrypt = crypto::cipher(&init[8..40], &init[40..56]);
            decrypt.apply_keystream(&mut [0; 64]);
            let mut reverse = init[8..56].to_vec();
            reverse.reverse();
            let mut encrypt = crypto::cipher(&reverse[..32], &reverse[32..]);
            ws.send(tokio_tungstenite::tungstenite::Message::Ping(
                Bytes::from_static(b"ping"),
            ))
            .await
            .unwrap();
            let mut packets = Vec::new();
            while packets.len() < 2 {
                let message = ws.next().await.unwrap().unwrap();
                if let tokio_tungstenite::tungstenite::Message::Binary(data) = message {
                    let mut plain = data.to_vec();
                    decrypt.apply_keystream(&mut plain);
                    packets.push(plain);
                }
            }
            assert_eq!(
                packets[0],
                framing::encode_reply(Protocol::Intermediate, &[1; 24]).unwrap()
            );
            assert_eq!(
                packets[1],
                framing::encode_reply(Protocol::Intermediate, &vec![2; 80_000]).unwrap()
            );
            let mut reply = framing::encode_reply(Protocol::Intermediate, &[3; 28]).unwrap();
            encrypt.apply_keystream(&mut reply);
            ws.send(tokio_tungstenite::tungstenite::Message::Binary(
                reply.into(),
            ))
            .await
            .unwrap();
            let _ = keep.await;
        });
        let tcp = TcpStream::connect(address).await.unwrap();
        let ws = websocket_handshake(Box::new(tcp), "localhost", "/apiws", false)
            .await
            .unwrap();
        let proxy = tokio::spawn(async move {
            server::bridge_ws(
                Box::new(proxy_client),
                ws,
                init,
                relay,
                true,
                &Stats::default(),
            )
            .await
        });
        let mut wire = [
            framing::encode_reply(Protocol::Intermediate, &[1; 24]).unwrap(),
            framing::encode_reply(Protocol::Intermediate, &vec![2; 80_000]).unwrap(),
        ]
        .concat();
        tx.apply_keystream(&mut wire);
        for part in wire.chunks(7) {
            client.write_all(part).await.unwrap();
        }
        let mut reply = vec![0; 32];
        client.read_exact(&mut reply).await.unwrap();
        rx.apply_keystream(&mut reply);
        assert_eq!(
            reply,
            framing::encode_reply(Protocol::Intermediate, &[3; 28]).unwrap()
        );
        drop(client);
        let _ = done.send(());
        remote_task.await.unwrap();
        let _ = proxy.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn fake_tls_stream_survives_one_byte_reads_ccs_and_record_boundaries() {
    timeout(Duration::from_secs(5), async {
        let (wire_a, wire_b) = tokio::io::duplex(13);
        let mut tls_a = FakeTls::new(wire_a);
        let mut tls_b = FakeTls::new(wire_b);
        let expected = vec![0x75; 40_000];
        let body = expected.clone();
        let sender = tokio::spawn(async move {
            tls_a.write_all(&body).await.unwrap();
            tls_a.flush().await.unwrap();
            tls_a
        });
        let mut received = vec![0; expected.len()];
        for byte in &mut received {
            tls_b.read_exact(std::slice::from_mut(byte)).await.unwrap();
        }
        assert_eq!(received, expected);
        drop(sender.await.unwrap());
        let (mut raw, wire) = tokio::io::duplex(16);
        let sender = tokio::spawn(async move {
            raw.write_all(b"\x14\x03\x03\0\x01\x01\x17\x03\x03\0\x03abc")
                .await
                .unwrap();
        });
        let mut tls = FakeTls::new(wire);
        let mut data = [0; 3];
        tls.read_exact(&mut data).await.unwrap();
        assert_eq!(&data, b"abc");
        sender.await.unwrap();
    })
    .await
    .unwrap();
}

async fn h2_echo(active: Arc<AtomicUsize>) -> (Arc<HttpLane>, tokio::task::JoinHandle<()>) {
    let (client, server) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
        let mut connection = h2::server::handshake(server).await.unwrap();
        let mut tasks = tokio::task::JoinSet::new();
        while let Some(request) = connection.accept().await {
            let (request, mut respond) = request.unwrap();
            let count = active.clone();
            tasks.spawn(async move {
                count.fetch_add(1, Relaxed);
                let mut incoming = request.into_body();
                let mut data = Vec::new();
                while let Some(part) = incoming.data().await {
                    let part = part.unwrap();
                    data.extend_from_slice(&part);
                    incoming
                        .flow_control()
                        .release_capacity(part.len())
                        .unwrap();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
                let response = http::Response::builder()
                    .status(200)
                    .header("content-type", "application/octet-stream")
                    .header("content-length", data.len())
                    .body(())
                    .unwrap();
                let mut output = respond.send_response(response, data.is_empty()).unwrap();
                let mut data = Bytes::from(data);
                while !data.is_empty() {
                    output.reserve_capacity(data.len());
                    let capacity = if output.capacity() > 0 {
                        output.capacity()
                    } else {
                        std::future::poll_fn(|cx| output.poll_capacity(cx))
                            .await
                            .unwrap()
                            .unwrap()
                    };
                    let n = capacity.min(data.len()).min(16 * 1024);
                    if n == 0 {
                        continue;
                    }
                    let part = data.split_to(n);
                    output.send_data(part, data.is_empty()).unwrap();
                }
            });
        }
    });
    let lane = HttpLane::from_stream(Box::new(client), "localhost", Arc::new(Stats::default()))
        .await
        .unwrap();
    (lane, server)
}

#[tokio::test]
async fn h2_multiplexes_large_bodies_on_a_single_connection() {
    timeout(Duration::from_secs(10), async {
        let active = Arc::new(AtomicUsize::new(0));
        let (lane, server) = h2_echo(active.clone()).await;
        let mut jobs = tokio::task::JoinSet::new();
        for value in 1..=4u8 {
            let lane = lane.clone();
            jobs.spawn(async move {
                let data = Bytes::from(vec![value; 300_000]);
                let response = lane
                    .request(http::Method::POST, "/api", data.clone(), 400_000)
                    .await
                    .unwrap();
                assert_eq!(response.status, 200);
                assert_eq!(response.body, data);
            });
        }
        while let Some(result) = jobs.join_next().await {
            result.unwrap();
        }
        assert_eq!(active.load(Relaxed), 4);
        drop(lane);
        server.abort();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn h2_bridge_converts_obfuscated_padded_transport_to_http_and_back() {
    timeout(Duration::from_secs(5), async {
        let (lane, remote) = h2_echo(Arc::new(AtomicUsize::new(0))).await;
        let (mut client, proxy_client) = tokio::io::duplex(1024);
        let (init, mut tx, mut rx) = support::client(Protocol::Padded);
        let bridge = tokio::spawn(async move {
            server::bridge_h2(
                Box::new(proxy_client),
                lane,
                init,
                Arc::new(Stats::default()),
            )
            .await
        });
        let mut body = vec![5; 24 + 16 * 100];
        body[..8].fill(0);
        let length = body.len() as u32 - 20;
        body[16..20].copy_from_slice(&length.to_le_bytes());
        let mut packet = framing::encode_reply(Protocol::Padded, &body).unwrap();
        tx.apply_keystream(&mut packet);
        client.write_all(&packet).await.unwrap();
        let mut header = [0; 4];
        client.read_exact(&mut header).await.unwrap();
        rx.apply_keystream(&mut header);
        let mut response = vec![0; u32::from_le_bytes(header) as usize];
        client.read_exact(&mut response).await.unwrap();
        rx.apply_keystream(&mut response);
        assert_eq!(
            framing::strip_padding(Protocol::Padded, response.into()).unwrap(),
            body
        );
        drop(client);
        let _ = bridge.await.unwrap();
        remote.abort();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn listener_rejects_wrong_secret_and_stops_clients_on_shutdown() {
    timeout(Duration::from_secs(5), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut config = Config::fresh().unwrap();
        config.cfproxy = false;
        config.pool_size = 0;
        let stats = Arc::new(Stats::default());
        let routing = Routing::new(Arc::new(config), Network::new(4096).unwrap(), stats.clone());
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            server::serve(listener, routing, async {
                let _ = stopped.await;
            })
            .await
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(&[0; 64]).await.unwrap();
        let mut data = [0];
        assert_eq!(client.read(&mut data).await.unwrap(), 0);
        assert_eq!(stats.bad.load(Relaxed), 1);
        let mut idle_client = TcpStream::connect(address).await.unwrap();
        idle_client.write_all(&[1]).await.unwrap();
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
        assert_eq!(stats.active.load(Relaxed), 0);
        match idle_client.read(&mut data).await {
            Ok(n) => assert_eq!(n, 0),
            Err(error) => assert!(matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            )),
        }
    })
    .await
    .unwrap();
}
