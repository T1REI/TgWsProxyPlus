use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tg_ws_proxy_plus::{
    config::Config, crypto, fake_tls, network::Network, routing::Routing, server, stats::Stats,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

#[tokio::test]
async fn proxy_protocol_valid_header_reaches_authentication_and_bad_header_is_rejected() {
    timeout(Duration::from_secs(5), async {
        let config = Config {
            proxy_protocol: true,
            cfproxy: false,
            pool_size: 0,
            ..Config::fresh().unwrap()
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let stats = Arc::new(Stats::default());
        let routing = Routing::new(Arc::new(config), Network::new(4096).unwrap(), stats.clone());
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server::serve(listener, routing, async {
            let _ = stopped.await;
        }));
        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(b"PROXY TCP4 192.0.2.1 127.0.0.1 1234 1443\r\n")
            .await
            .unwrap();
        client.write_all(&[0; 64]).await.unwrap();
        assert_eq!(client.read(&mut [0; 1]).await.unwrap(), 0);
        assert_eq!(stats.bad.load(std::sync::atomic::Ordering::Relaxed), 1);
        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(b"PROXY TCP4 invalid 127.0.0.1 1234 1443\r\n")
            .await
            .unwrap();
        assert_eq!(client.read(&mut [0; 1]).await.unwrap(), 0);
        assert_eq!(stats.bad.load(std::sync::atomic::Ordering::Relaxed), 1);
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn fake_tls_listener_authenticates_hello_rejects_replay_and_redirects_http() {
    timeout(Duration::from_secs(5), async {
        let reference: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/python-reference.json")).unwrap();
        let mut hello = crypto::unhex(reference["tls"]["hello"].as_str().unwrap()).unwrap();
        let secret = crypto::unhex(reference["tls"]["secret"].as_str().unwrap()).unwrap();
        hello[11..43].fill(0);
        let mut signature = fake_tls::digest(&secret, &hello);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32;
        for (byte, time) in signature[28..].iter_mut().zip(now.to_le_bytes()) {
            *byte ^= time;
        }
        hello[11..43].copy_from_slice(&signature);
        let config = Config {
            secret: crypto::hex(&secret),
            fake_tls_domain: "example.org".into(),
            cfproxy: false,
            pool_size: 0,
            ..Config::fresh().unwrap()
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let routing = Routing::new(
            Arc::new(config),
            Network::new(4096).unwrap(),
            Arc::new(Stats::default()),
        );
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server::serve(listener, routing, async {
            let _ = stopped.await;
        }));
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(&hello).await.unwrap();
        let mut header = [0; 5];
        client.read_exact(&mut header).await.unwrap();
        assert_eq!(header[..3], [0x16, 3, 3]);
        let mut body = vec![0; u16::from_be_bytes([header[3], header[4]]) as usize];
        client.read_exact(&mut body).await.unwrap();
        assert_eq!(body[39..71], hello[44..76]);
        let mut replay = TcpStream::connect(address).await.unwrap();
        replay.write_all(&hello).await.unwrap();
        assert_eq!(replay.read(&mut [0; 1]).await.unwrap(), 0);
        let mut http = TcpStream::connect(address).await.unwrap();
        http.write_all(b"G").await.unwrap();
        let mut redirect = String::new();
        http.read_to_string(&mut redirect).await.unwrap();
        assert!(redirect.starts_with("HTTP/1.1 301"));
        assert!(redirect.contains("Location: https://example.org/"));
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}
