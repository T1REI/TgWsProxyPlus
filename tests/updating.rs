use std::{fs, path::PathBuf, process::Command, time::Duration};
use tg_ws_proxy_plus::{config::Config, crypto, update_http, update_install, updates};

fn directory() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "tgws-updater-{}",
        crypto::hex(&crypto::random::<12>().unwrap())
    ));
    fs::create_dir(&path).unwrap();
    path
}

#[tokio::test]
async fn http_framing_handles_chunked_fixed_and_eof_bodies() {
    for wire in [
        &b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2;ext=yes\r\nhe\r\n3\r\nllo\r\n0\r\nX-End: yes\r\n\r\n"[..],
        &b"HTTP/1.0 200 OK\r\n\r\nhello"[..],
    ] {
        let response = update_http::read_response(wire, 5).await.unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"hello");
    }
    let response = update_http::read_response(
        &b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\n\r\n"[..],
        5,
    )
    .await
    .unwrap();
    assert_eq!(response.status, 302);
    assert_eq!(response.headers["location"], "/next");
}

#[tokio::test]
async fn http_framing_rejects_ambiguous_truncated_and_oversized_messages() {
    for wire in [
        "HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nhello!",
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhi",
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 5\r\n\r\nhello",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 0\r\n\r\n0\r\n\r\n",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\nhello",
        "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\r\nhello",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\nhello!\r\n0\r\n\r\n",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nhi",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhiXX0\r\n\r\n",
        "HTTP/1.1 200 OK\r\n\r\nhello!",
        "HTTP/1.1 200 OK\n\nhello",
        "HTTP/2 200 OK\r\n\r\nhello",
    ] {
        assert!(
            update_http::read_response(wire.as_bytes(), 5)
                .await
                .is_err(),
            "{wire}"
        );
    }
    let oversized = format!("HTTP/1.1 200 OK\r\nX-Large: {}\r\n\r\n", "x".repeat(32768));
    assert!(
        update_http::read_response(oversized.as_bytes(), 5)
            .await
            .is_err()
    );
    let trailers = format!(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nX-Large: {}\r\n\r\n",
        "x".repeat(4096)
    );
    assert!(
        update_http::read_response(trailers.as_bytes(), 5)
            .await
            .is_err()
    );
}

fn release() -> updates::Release {
    let name = updates::asset_name().unwrap();
    serde_json::from_value(serde_json::json!({
        "tag_name": "v1.2.3", "html_url": "https://github.com/T1REI/TgWsProxyPlus/releases/tag/v1.2.3",
        "assets": [{"name": name, "size": 12345, "digest": format!("sha256:{}", "a".repeat(64)),
            "browser_download_url": format!("https://github.com/T1REI/TgWsProxyPlus/releases/download/v1.2.3/{name}")}]
    })).unwrap()
}

#[test]
fn only_stable_official_release_and_matching_executable_are_accepted() {
    let good = release();
    updates::select_asset(&good).unwrap();
    for invalid in 0..9 {
        let mut bad = good.clone();
        match invalid {
            0 => bad.draft = true,
            1 => bad.prerelease = true,
            2 => bad.html_url.push_str("?other"),
            3 => {
                bad.assets[0].browser_download_url = bad.assets[0]
                    .browser_download_url
                    .replace("v1.2.3", "v1.2.2")
            }
            4 => bad.assets[0].size = 0,
            5 => bad.assets[0].size = updates::MAX_DOWNLOAD as u64 + 1,
            6 => bad.assets[0].digest = None,
            7 => bad.assets[0].digest = Some("sha256:bad".into()),
            8 => bad.assets.push(bad.assets[0].clone()),
            _ => unreachable!(),
        }
        assert!(updates::select_asset(&bad).is_err(), "case {invalid}");
    }
    let mut bad = good;
    bad.tag_name = "v2.0.0-rc.1".into();
    bad.html_url = format!(
        "https://github.com/{}/releases/tag/{}",
        updates::REPOSITORY,
        bad.tag_name
    );
    assert!(updates::validate_release(&bad).is_err());
}

#[test]
fn download_redirects_require_github_https() {
    for url in [
        "https://github.com/a",
        "https://release-assets.githubusercontent.com/a?token=public",
    ] {
        updates::download_url(url).unwrap();
    }
    for url in [
        "http://github.com/a",
        "https://github.com:443/a",
        "https://user@github.com/a",
        "https://github.com.evil.org/a",
        "https://evilgithubusercontent.com/a",
        "file:///app.exe",
        "//github.com/a",
    ] {
        assert!(updates::download_url(url).is_err(), "{url}");
    }
}

#[test]
fn executable_validation_rejects_corrupt_and_wrong_architecture_files() {
    let mut pe = vec![0; 128];
    pe[..2].copy_from_slice(b"MZ");
    pe[60..64].copy_from_slice(&64u32.to_le_bytes());
    pe[64..68].copy_from_slice(b"PE\0\0");
    pe[68..70].copy_from_slice(&0x8664u16.to_le_bytes());
    pe[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
    updates::verify_executable(&pe, "x86_64").unwrap();
    assert!(updates::verify_executable(&pe, "aarch64").is_err());
    assert!(updates::verify_executable(&pe[..70], "x86_64").is_err());
    pe[60..64].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(updates::verify_executable(&pe, "x86_64").is_err());
    assert!(updates::verify_executable(b"<html>Access denied</html>", "x86_64").is_err());
}

#[test]
fn old_repository_setting_is_discarded_without_losing_other_configuration() {
    let dir = directory();
    let path = dir.join("config.json");
    let mut value = serde_json::to_value(Config::fresh().unwrap()).unwrap();
    value["update_repository"] = serde_json::json!("invalid/repo/path");
    value["future_key"] = serde_json::json!(42);
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let config = Config::load(&path).unwrap();
    config.save(&path).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(saved.get("update_repository").is_none());
    assert_eq!(saved["future_key"], 42);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore]
fn readiness_child_fixture() {
    let Ok(mode) = std::env::var("TGWS_READY_MODE") else {
        return;
    };
    if mode == "exit" {
        return;
    }
    let path = std::env::var_os("TGWS_READY_PATH").unwrap();
    fs::write(path, if mode == "ready" { "expected" } else { "wrong" }).unwrap();
    std::thread::sleep(Duration::from_secs(20));
}

#[test]
fn installation_waits_for_readiness_and_stops_only_its_failed_child() {
    let dir = directory();
    for mode in ["ready", "wrong", "exit"] {
        let path = dir.join(mode);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "readiness_child_fixture", "--ignored"])
            .env("TGWS_READY_MODE", mode)
            .env("TGWS_READY_PATH", &path)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let result = update_install::await_ready(
            &mut child,
            &path,
            "expected",
            if mode == "wrong" {
                Duration::from_millis(500)
            } else {
                Duration::from_secs(10)
            },
        );
        assert_eq!(result.is_ok(), mode == "ready", "{mode}");
        if mode == "ready" {
            assert!(child.try_wait().unwrap().is_none());
            child.kill().unwrap();
            child.wait().unwrap();
        } else {
            assert!(child.try_wait().unwrap().is_some());
        }
    }
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
#[ignore]
async fn official_github_release_download_passes_size_hash_and_format_checks() {
    let network = tg_ws_proxy_plus::network::Network::new(0).unwrap();
    let release = updates::check(&network).await.unwrap();
    let asset = updates::select_asset(&release).unwrap();
    let dir = directory();
    let path = updates::download(&network, asset, &dir).await.unwrap();
    assert_eq!(fs::metadata(&path).unwrap().len(), asset.size);
    updates::verify_download(&fs::read(&path).unwrap(), asset.digest.as_deref().unwrap()).unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
#[ignore]
async fn official_github_api_is_accessible_over_http11() {
    use tokio::io::AsyncWriteExt;
    let network = tg_ws_proxy_plus::network::Network::new(0).unwrap();
    tokio::time::timeout(Duration::from_secs(40), async {
        let mut stream = network.stream("api.github.com", 443, "api.github.com", true, false).await.unwrap();
        stream.write_all(b"GET /repos/T1REI/TgWsProxyPlus/releases/latest HTTP/1.1\r\nHost: api.github.com\r\nUser-Agent: TgWsProxyPlus-tests\r\nAccept-Encoding: identity\r\nX-GitHub-Api-Version: 2022-11-28\r\nConnection: close\r\n\r\n").await.unwrap();
        let response = update_http::read_response(stream, 1024 * 1024).await.unwrap();
        assert_eq!(response.status, 200);
        let release: updates::Release = serde_json::from_slice(&response.body).unwrap();
        updates::select_asset(&release).unwrap();
    }).await.unwrap();
}
