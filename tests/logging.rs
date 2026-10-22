use tg_ws_proxy_plus::{crypto, logging};

#[test]
fn rotating_logs_remain_bounded_with_at_least_one_backup() {
    let dir = std::env::temp_dir().join(format!(
        "tgws-logs-{}",
        crypto::hex(&crypto::random::<8>().unwrap())
    ));
    std::fs::create_dir(&dir).unwrap();
    let file = dir.join("proxy.log");
    logging::install(Some(&file), 0.03125, 0, false, false).unwrap();
    for _ in 0..2000 {
        log::info!(target: "tg_ws_proxy_plus", "{} relay.private.example.org", "a".repeat(100));
    }
    log::logger().flush();
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|v| v.unwrap().path())
        .collect();
    assert_eq!(names.len(), 2);
    for path in names {
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.len() <= 32768);
        assert!(!contents.contains("relay.private.example.org"));
    }
    logging::configure(None, 0.03125, 1, false, false).unwrap();
    std::fs::remove_file(file).unwrap();
    std::fs::remove_file(dir.join("proxy.log.1")).unwrap();
    std::fs::remove_dir(dir).unwrap();
}
