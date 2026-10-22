mod support;

use anyhow::{Result, ensure};
use ctr::cipher::StreamCipher;
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tg_ws_proxy_plus::{
    config::Config,
    crypto::{self, Protocol},
    framing,
    network::Network,
    pool::{Key, Pool},
    routing, server,
    stats::Stats,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::timeout,
};

async fn anonymous_probe(websocket: bool) -> Result<()> {
    timeout(Duration::from_secs(40), async {
        let network = Network::new(256 * 1024)?;
        let stats = Arc::new(Stats::default());
        let (mut peer, client) = tokio::io::duplex(8192);
        let (init, mut tx, mut rx) = support::client(Protocol::Intermediate);
        let relay = crypto::relay_init(Protocol::Intermediate, 2, false)?;
        let bridge = if websocket {
            let pool = Pool::new(Arc::new(Config::fresh()?), network.clone(), stats.clone());
            let socket = pool.connect(Key::Direct(2, false, false)).await?;
            tokio::spawn(async move {
                server::bridge_ws(Box::new(client), socket, init, relay, true, &stats).await
            })
        } else {
            let mut remote = network.tcp(routing::dc_ip(2, false).unwrap(), 443).await?;
            remote.write_all(&relay.bytes).await?;
            tokio::spawn(async move {
                server::bridge_tcp(Box::new(client), Box::new(remote), init, relay, &stats).await
            })
        };
        let nonce = crypto::random::<16>()?;
        let mut body = vec![0; 8];
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?;
        let id = ((now.as_secs() << 32)
            | (u64::from(now.subsec_nanos()) * (1u64 << 32) / 1_000_000_000))
            & !3;
        body.extend_from_slice(&id.to_le_bytes());
        body.extend_from_slice(&20u32.to_le_bytes());
        body.extend_from_slice(&0xbe7e8ef1u32.to_le_bytes());
        body.extend_from_slice(&nonce);
        let mut encoded = framing::encode_reply(Protocol::Intermediate, &body)?;
        tx.apply_keystream(&mut encoded);
        peer.write_all(&encoded).await?;
        let mut header = [0; 4];
        peer.read_exact(&mut header).await?;
        rx.apply_keystream(&mut header);
        let size = u32::from_le_bytes(header) as usize;
        ensure!(
            (40..=4096).contains(&size),
            "unexpected Telegram response length {size}"
        );
        let mut reply = vec![0; size];
        peer.read_exact(&mut reply).await?;
        rx.apply_keystream(&mut reply);
        ensure!(reply[..8] == [0; 8], "unexpected auth_key_id");
        ensure!(
            reply[20..24] == 0x05162463u32.to_le_bytes(),
            "expected resPQ"
        );
        ensure!(reply[24..40] == nonce, "Telegram nonce mismatch");
        drop(peer);
        bridge.abort();
        let _ = bridge.await;
        Ok(())
    })
    .await?
}

#[tokio::test]
#[ignore = "requires access to Telegram DC2; sends anonymous req_pq_multi only"]
async fn telegram_tcp_returns_matching_res_pq() -> Result<()> {
    anonymous_probe(false).await
}

#[tokio::test]
#[ignore = "requires access to Telegram WebSocket DC2; sends anonymous req_pq_multi only"]
async fn telegram_websocket_returns_matching_res_pq() -> Result<()> {
    anonymous_probe(true).await
}
