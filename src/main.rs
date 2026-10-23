#![cfg_attr(all(windows, feature = "desktop"), windows_subsystem = "windows")]

use anyhow::{Context, Result};
use tg_ws_proxy_plus::{
    cli::{Args, HELP},
    config::{Config, app_dir},
    logging,
    network::Network,
    server, updates,
};

#[cfg(all(windows, feature = "desktop"))]
mod windows;

fn execute() -> Result<()> {
    #[cfg(all(windows, feature = "desktop"))]
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--apply-update")) {
        let path = std::env::args_os().nth(2).context("missing update plan")?;
        return tg_ws_proxy_plus::update_install::apply(std::path::Path::new(&path));
    }
    let args = Args::parse(std::env::args().skip(1))?;
    #[cfg(all(windows, feature = "desktop"))]
    windows::attach_console();
    if args.help {
        print!("{HELP}");
        return Ok(());
    }
    if args.version {
        println!(
            "TgWsProxy+ / TgWsProxyPlus {} — T1REI",
            tg_ws_proxy_plus::VERSION
        );
        return Ok(());
    }
    if let Some(path) = &args.init {
        anyhow::ensure!(
            !path.exists(),
            "configuration already exists: {}",
            path.display()
        );
        let config = Config::fresh()?;
        config.save(path)?;
        println!("Created {}", path.display());
        return Ok(());
    }
    let path = match &args.path {
        Some(path) => path.clone(),
        None if args.portable => std::env::current_exe()?
            .parent()
            .context("executable has no parent")?
            .join("TgWsProxyPlus-data/config.json"),
        None => app_dir()?.join("config.json"),
    };
    let first_run = !path.exists();
    let mut config = if first_run {
        if args.path.is_some() {
            anyhow::bail!(
                "configuration does not exist; create it with --init-config {}",
                path.display()
            );
        }
        let config = Config::fresh()?;
        config.save(&path)?;
        config
    } else {
        Config::load(&path)?
    };
    args.apply(&mut config)?;
    if args.link {
        println!("{}", config.link());
        return Ok(());
    }
    if args.check {
        println!("Configuration OK");
        return Ok(());
    }
    let tray = !args.console
        && !args.diagnose
        && !args.updates
        && (args.tray || cfg!(all(windows, feature = "desktop")));
    let log_path = args
        .log
        .clone()
        .or_else(|| tray.then(|| path.with_file_name("TgWsProxyPlus.log")));
    logging::install(
        log_path.as_deref(),
        config.log_max_mb,
        config.log_backups,
        config.verbose,
        !tray,
    )?;
    #[cfg(all(windows, feature = "desktop"))]
    if tray {
        return windows::run(config, path, first_run || args.settings, args.update_ready);
    }
    if tray {
        anyhow::bail!("native tray requires the Windows desktop build; use --console");
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        if args.diagnose {
            for line in server::diagnose(config).await? {
                println!("{line}");
            }
            return Ok(());
        }
        if args.updates {
            let network = Network::new(config.buf_kb * 1024)?;
            let release = updates::check(&network).await?;
            println!(
                "Latest: {}\nNewer: {}\n{}",
                release.tag_name,
                updates::newer(&release.tag_name, tg_ws_proxy_plus::VERSION)?,
                release.html_url
            );
            return Ok(());
        }
        println!("{}", config.link());
        server::run(config, shutdown_signal()).await
    })
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    result = tokio::signal::ctrl_c() => if let Err(error) = result { log::error!("shutdown signal error: {error}"); },
                    _ = terminate.recv() => {}
                }
                return;
            }
            Err(error) => log::error!("SIGTERM handler: {error}"),
        }
    }
    if let Err(error) = tokio::signal::ctrl_c().await {
        log::error!("shutdown signal error: {error}");
    }
}

fn main() {
    if let Err(error) = execute().context("TgWsProxyPlus") {
        eprintln!("{error:#}");
        #[cfg(all(windows, feature = "desktop"))]
        if std::env::args().len() == 1
            || std::env::args()
                .any(|arg| arg == "--tray" || arg == "--settings" || arg == "--apply-update")
        {
            windows::show_error(&format!("{error:#}"));
        }
        std::process::exit(1);
    }
}
