use std::{
    path::Path,
    process::{Command, Output},
};
use tg_ws_proxy_plus::{config::Config, crypto};

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_TgWsProxyPlus"))
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn executable_reports_version_help_and_invalid_arguments() {
    let version = run(&["--version"]);
    assert!(version.status.success());
    let text = String::from_utf8(version.stdout).unwrap();
    assert!(text.contains("TgWsProxyPlus") && text.contains("T1REI"));
    let help = run(&["--help"]);
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("--diagnose")
    );
    let invalid = run(&["--not-an-option"]);
    assert!(!invalid.status.success());
    assert!(
        String::from_utf8(invalid.stderr)
            .unwrap()
            .contains("unknown argument")
    );
}

#[test]
fn executable_creates_valid_config_and_preserves_it_under_cli_overrides() {
    let directory = std::env::temp_dir().join(format!(
        "tgws-cli-{}",
        crypto::hex(&crypto::random::<8>().unwrap())
    ));
    let path = directory.join("config.json");
    let path_text = Path::to_str(&path).unwrap();
    assert!(run(&["--init-config", path_text]).status.success());
    let original = std::fs::read(&path).unwrap();
    assert!(Config::load(&path).is_ok());
    assert!(
        run(&["--config", path_text, "--check-config"])
            .status
            .success()
    );
    let link = run(&["--config", path_text, "--port", "18445", "--link"]);
    assert!(link.status.success());
    assert!(
        String::from_utf8(link.stdout)
            .unwrap()
            .contains("port=18445")
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(!run(&["--init-config", path_text]).status.success());
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}
