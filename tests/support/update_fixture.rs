use std::{fs, path::Path, time::Duration};
use tg_ws_proxy_plus::update_install::{self, Prepared};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--version") => println!("TgWsProxyPlus {} fixture", tg_ws_proxy_plus::VERSION),
        Some("--apply-update") => {
            if let Err(error) = update_install::apply(Path::new(&args[2])) {
                fs::write(
                    std::env::current_exe()?.with_file_name("install-error.txt"),
                    format!("{error:#}"),
                )?;
            }
        }
        Some("--parent") => update_install::handoff(
            Prepared {
                path: (&args[2]).into(),
                digest: args[4].clone(),
                version: tg_ws_proxy_plus::VERSION.into(),
            },
            Path::new(&args[3]),
        )?,
        Some("--tray") => {
            let root = std::env::current_exe()?.parent().unwrap().to_path_buf();
            if let Some(index) = args.iter().position(|arg| arg == "--update-ready") {
                if cfg!(fail_start) {
                    std::process::exit(7);
                }
                update_install::confirm_ready(Path::new(&args[index + 1]))?;
                fs::write(root.join("healthy.txt"), b"ready")?;
                std::thread::sleep(Duration::from_secs(3));
            } else {
                fs::write(root.join("rollback.txt"), b"restored")?;
            }
        }
        _ => return Err("unexpected fixture role".into()),
    }
    Ok(())
}
