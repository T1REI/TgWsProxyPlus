#[path = "support/baseline_framing.rs"]
mod baseline;
mod support;
use ctr::cipher::StreamCipher;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicIsize, Ordering::Relaxed},
    time::Instant,
};
use tg_ws_proxy_plus::{
    crypto::{self, Protocol},
    framing,
    network::Stream,
    server,
    stats::Stats,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Allocator;
static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let live = LIVE.fetch_add(layout.size() as isize, Relaxed) + layout.size() as isize;
            PEAK.fetch_max(live, Relaxed);
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as isize, Relaxed);
        unsafe { System.dealloc(pointer, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

#[test]
#[ignore = "manual allocation benchmark; run alone with --test-threads=1"]
fn packet_buffer_allocations() {
    let mut plain = vec![7; 4 * 1024 * 1024 + 4];
    plain[..4].copy_from_slice(&(4u32 * 1024 * 1024).to_le_bytes());
    let cipher = vec![9; plain.len()];
    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    {
        let mut splitter = baseline::Splitter::new(Protocol::Intermediate);
        for (encrypted, plain) in cipher.chunks(32768).zip(plain.chunks(32768)) {
            drop(splitter.push(encrypted, plain).unwrap());
        }
        assert!(splitter.flush().is_empty());
    }
    let old = PEAK.load(Relaxed) - before;
    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    {
        let mut splitter = framing::Splitter::new(Protocol::Intermediate);
        for (encrypted, plain) in cipher.chunks(32768).zip(plain.chunks(32768)) {
            drop(splitter.push(encrypted, plain).unwrap());
        }
        assert!(splitter.flush().is_empty());
    }
    let new = PEAK.load(Relaxed) - before;
    println!("packet_buffer_peak old={old} new={new} saved={}", old - new);
    assert!(new < old);
}

async fn baseline_forward(
    mut read: tokio::io::ReadHalf<Stream>,
    mut write: tokio::io::WriteHalf<Stream>,
    mut decrypt: crypto::Cipher,
    mut encrypt: crypto::Cipher,
) -> anyhow::Result<()> {
    let mut buffer = vec![0; 65536];
    loop {
        let n = read.read(&mut buffer).await?;
        if n == 0 {
            write.shutdown().await?;
            return Ok(());
        }
        decrypt.apply_keystream(&mut buffer[..n]);
        encrypt.apply_keystream(&mut buffer[..n]);
        write.write_all(&buffer[..n]).await?;
        write.flush().await?;
    }
}

async fn transfer(old: bool) -> f64 {
    const TOTAL: usize = 32 * 1024 * 1024;
    let (client, proxy_client) = tokio::io::duplex(256 * 1024);
    let (proxy_remote, mut remote) = tokio::io::duplex(256 * 1024);
    let (init, mut encrypt, mut decrypt) = support::client(Protocol::Intermediate);
    let relay = crypto::relay_init(init.protocol, 2, true).unwrap();
    let mut remote_rx = crypto::cipher(&relay.bytes[8..40], &relay.bytes[40..56]);
    remote_rx.apply_keystream(&mut [0; 64]);
    let mut reverse = relay.bytes[8..56].to_vec();
    reverse.reverse();
    let mut remote_tx = crypto::cipher(&reverse[..32], &reverse[32..]);
    let proxy = tokio::spawn(async move {
        if old {
            let (read, write) = tokio::io::split(Box::new(proxy_client) as Stream);
            let (remote_read, remote_write) = tokio::io::split(Box::new(proxy_remote) as Stream);
            tokio::select! {r=baseline_forward(read,remote_write,init.decrypt,relay.encrypt)=>r,r=baseline_forward(remote_read,write,relay.decrypt,init.encrypt)=>r}
        } else {
            server::bridge_tcp(
                Box::new(proxy_client),
                Box::new(proxy_remote),
                init,
                relay,
                &Stats::default(),
            )
            .await
        }
    });
    let (done, keep) = tokio::sync::oneshot::channel();
    let remote_task = tokio::spawn(async move {
        let mut buffer = vec![0; 65536];
        let mut received = 0;
        while received < TOTAL {
            let n = remote.read(&mut buffer).await.unwrap();
            assert!(n > 0);
            received += n;
            remote_rx.apply_keystream(&mut buffer[..n]);
            assert!(buffer[..n].iter().all(|&b| b == 0x5a));
            remote_tx.apply_keystream(&mut buffer[..n]);
            remote.write_all(&buffer[..n]).await.unwrap();
        }
        let _ = keep.await;
    });
    let (mut read, mut write) = tokio::io::split(client);
    let began = Instant::now();
    let upload = async {
        let mut buffer = vec![0; 32768];
        for _ in 0..TOTAL / 32768 {
            buffer.fill(0x5a);
            encrypt.apply_keystream(&mut buffer);
            write.write_all(&buffer).await.unwrap();
        }
    };
    let download = async {
        let mut buffer = vec![0; 65536];
        let mut received = 0;
        while received < TOTAL {
            let n = read.read(&mut buffer).await.unwrap();
            assert!(n > 0);
            decrypt.apply_keystream(&mut buffer[..n]);
            assert!(buffer[..n].iter().all(|&b| b == 0x5a));
            received += n;
        }
    };
    tokio::join!(upload, download);
    let seconds = began.elapsed().as_secs_f64();
    drop((read, write));
    let _ = done.send(());
    remote_task.await.unwrap();
    let _ = proxy.await;
    64.0 / seconds
}

#[tokio::test]
#[ignore = "manual loopback throughput benchmark; run in release mode"]
async fn encrypted_transfer_throughput() {
    let mut old = Vec::new();
    let mut new = Vec::new();
    for _ in 0..5 {
        old.push(transfer(true).await);
        new.push(transfer(false).await);
    }
    old.sort_by(f64::total_cmp);
    new.sort_by(f64::total_cmp);
    println!(
        "encrypted_duplex_MiB_s old={:.2} new={:.2} change_pct={:.2}",
        old[2],
        new[2],
        (new[2] / old[2] - 1.0) * 100.0
    );
}
