use tg_ws_proxy_plus::{
    cli::Args,
    config::{Config, normalize_domains, parse_dc_ips, valid_domain},
    crypto, logging,
    routing::{Domains, dc_ip, decode_domain},
    updates,
};

fn args(values: &[&str]) -> Args {
    Args::parse(values.iter().map(|s| s.to_string())).unwrap()
}

#[test]
fn upstream_configuration_roundtrips_and_preserves_unknown_keys() {
    let mut config = Config::fresh().unwrap();
    config
        .extra
        .insert("future_option".into(), serde_json::json!([1, 2, 3]));
    let mut value = serde_json::to_value(&config).unwrap();
    value["cfproxy_user_domain"] = serde_json::json!("EXAMPLE.org;example.org,other.example.net");
    let parsed: Config = serde_json::from_value(value).unwrap();
    parsed.validate().unwrap();
    assert_eq!(
        parsed.cfproxy_user_domain,
        ["example.org", "other.example.net"]
    );
    assert_eq!(parsed.extra["future_option"], serde_json::json!([1, 2, 3]));
    assert_eq!(serde_json::to_value(&parsed).unwrap()["port"], 1443);
}

#[test]
fn invalid_secrets_network_addresses_and_resource_limits_are_rejected() {
    let base = Config::fresh().unwrap();
    for (key, invalid) in [
        ("secret", serde_json::json!("xx".repeat(16))),
        ("secret", serde_json::json!("0".repeat(31))),
        ("host", serde_json::json!("example.org")),
        ("port", serde_json::json!(0)),
        ("buf_kb", serde_json::json!(1)),
        ("pool_size", serde_json::json!(33)),
        ("h2_max_connections", serde_json::json!(4)),
        ("h2_stream_window_kb", serde_json::json!(0)),
        ("h2_connection_window_kb", serde_json::json!(32)),
        ("log_backups", serde_json::json!(0)),
        ("max_connections", serde_json::json!(0)),
        ("log_max_mb", serde_json::json!(-1)),
        (
            "fake_tls_domain",
            serde_json::json!("example.org\r\nHeader: injected"),
        ),
    ] {
        let mut value = serde_json::to_value(&base).unwrap();
        value[key] = invalid;
        assert!(
            serde_json::from_value::<Config>(value)
                .unwrap()
                .validate()
                .is_err(),
            "{key}"
        );
    }
}

#[test]
fn cli_overrides_defaults_and_repeated_routes_with_upstream_semantics() {
    let mut config = Config::fresh().unwrap();
    args(&[
        "--dc-ip",
        "4:149.154.167.220",
        "--cfproxy-domain",
        "EXAMPLE.org;example.org",
        "--cfproxy-worker-domain",
        "relay.example.org",
        "--no-h2",
        "--pool-size",
        "0",
    ])
    .apply(&mut config)
    .unwrap();
    assert_eq!(config.dc_ip, ["4:149.154.167.220"]);
    assert_eq!(config.pool_size, 0);
    assert_eq!(config.cfproxy_user_domain, ["example.org"]);
    assert!(!config.h2_enabled());
    assert_eq!(config.worker_domains(), ["relay.example.org"]);
    args(&["--dc-ip", "--no-cfproxy"])
        .apply(&mut config)
        .unwrap();
    assert!(config.dc_ip.is_empty());
    assert!(!config.cfproxy);
    assert!(Args::parse(["--port".into()]).is_err());
    assert!(Args::parse(["--unknown".into()]).is_err());
    assert!(Args::parse(["--console".into(), "--tray".into()]).is_err());
}

#[test]
fn domain_and_dc_validation_blocks_urls_control_characters_and_invalid_ips() {
    for domain in [
        "example.org",
        "sub-domain.example.co.uk",
        "xn--e1afmkfd.xn--p1ai",
    ] {
        assert!(valid_domain(domain));
    }
    for domain in [
        "https://example.org",
        ".example.org",
        "example.org.",
        "a..org",
        "-a.org",
        "a-.org",
        "x.123",
        "x",
        "x.org\n",
        "127.0.0.1",
        "пример.рф",
    ] {
        assert!(!valid_domain(domain), "{domain}");
    }
    assert_eq!(
        normalize_domains(&["A.ORG; a.org,b.org c.org".into()]),
        ["a.org", "b.org", "c.org"]
    );
    for mapping in [
        "2:127.1",
        "2:999.0.0.1",
        "2:no",
        "x:127.0.0.1",
        "0:127.0.0.1",
        "2",
    ] {
        assert!(parse_dc_ips(&[mapping.into()]).is_err());
    }
}

#[test]
fn telegram_links_handle_fake_tls_and_ipv6() {
    let mut config = Config::fresh().unwrap();
    assert!(config.link().contains("secret=dd"));
    config.host = "::".into();
    config.fake_tls_domain = "example.org".into();
    assert!(config.link().contains("server=%3A%3A1"));
    assert!(config.link().ends_with(&crypto::hex(b"example.org")));
    assert!(config.link().contains("secret=ee"));
}

#[test]
fn fallback_domains_and_test_dc_routes_are_consistent() {
    let config = Config::fresh().unwrap();
    let mut domains = Domains::new(&config);
    assert!(domains.list.len() >= 3);
    assert!(domains.list.iter().all(|s| valid_domain(s)));
    assert!(decode_domain("virkgj.com").ends_with(".co.uk"));
    let selected = domains.list[0].clone();
    domains.prefer(2, &selected);
    assert_eq!(domains.ordered(2)[0], selected);
    domains.replace(vec!["example.org".into()]);
    assert_eq!(domains.ordered(2), ["example.org"]);
    assert_eq!(dc_ip(2, true), Some("149.154.167.40"));
    assert!(dc_ip(203, true).is_none());
}

#[test]
fn update_downloads_require_matching_sha256_and_use_semver() {
    assert!(updates::newer("v1.10.0", "1.9.0").unwrap());
    assert!(!updates::newer("1.0.0-rc.1", "1.0.0").unwrap());
    assert!(updates::newer("garbage", "1.0.0").is_err());
    assert!(
        updates::verify_download(
            b"abc",
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        )
        .is_ok()
    );
    assert!(
        updates::verify_download(
            b"tampered",
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        )
        .is_err()
    );
    assert!(updates::verify_download(b"abc", "").is_err());
    assert_eq!(updates::REPOSITORY, "T1REI/TgWsProxyPlus");
}

#[test]
fn domain_censor_preserves_telegram_domains_and_hides_custom_routes() {
    let censored = logging::censor("kws2.telegram.org relay.private.example.org 127.0.0.1");
    assert!(censored.contains("kws2.telegram.org"));
    assert!(censored.contains("127.0.0.1"));
    assert!(!censored.contains("relay.private.example.org"));
}

#[test]
fn atomic_config_save_keeps_previous_file_when_validation_fails() {
    let directory = std::env::temp_dir().join(format!(
        "tgws-config-{}",
        crypto::hex(&crypto::random::<8>().unwrap())
    ));
    let path = directory.join("config.json");
    let mut config = Config::fresh().unwrap();
    config.save(&path).unwrap();
    let original = std::fs::read(&path).unwrap();
    config.port = 0;
    assert!(config.save(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(Config::load(&path).is_ok());
    config.port = 1555;
    config.save(&path).unwrap();
    assert_eq!(Config::load(&path).unwrap().port, 1555);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn binary_update_rolls_back_after_failed_relaunch() {
    let directory = std::env::temp_dir().join(format!(
        "tgws-update-{}",
        crypto::hex(&crypto::random::<8>().unwrap())
    ));
    std::fs::create_dir(&directory).unwrap();
    let target = directory.join("app.exe");
    let staged = directory.join("staged.exe");
    std::fs::write(&target, b"old executable").unwrap();
    std::fs::write(&staged, b"verified new executable").unwrap();
    assert!(updates::replace_binary(&staged, &target, || anyhow::bail!("cannot launch")).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"old executable");
    let backup = updates::replace_binary(&staged, &target, || Ok(())).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"verified new executable");
    assert_eq!(std::fs::read(&backup).unwrap(), b"old executable");
    assert!(updates::replace_binary(&target, &target, || Ok(())).is_err());
    for path in [target, backup] {
        std::fs::remove_file(path).unwrap();
    }
    std::fs::remove_dir(directory).unwrap();
}
