use crate::config::{Config, normalize_domains};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;

pub const HELP: &str = "TgWsProxy+ / TgWsProxyPlus — T1REI\n\
Usage: TgWsProxyPlus [--console | --tray | --settings] [options]\n\
  --config PATH          JSON configuration (compatible with upstream tray keys)\n\
  --init-config PATH     Write a new configuration with a random secret and exit\n\
  --host IP --port N --secret HEX\n\
  --dc-ip [DC:IP]        Repeatable; without value clears direct WS routes\n\
  --pool-size N          Warm connections per DC (0 disables direct WS)\n\
  --buf-kb N             Socket buffer in KiB; 0 uses OS autotuning (default)\n\
  --max-connections N\n\
  --cfproxy-domain NAME  Repeatable, comma/space separated\n\
  --cfproxy-worker-domain NAME\n\
  --no-cfproxy --no-h2 --no-secure --force-test-dc\n\
  --fake-tls-domain NAME --proxy-protocol\n\
  --log-file PATH --log-max-mb N --log-backups N --verbose\n\
  --link                 Print the Telegram connection link and exit\n\
  --check-config         Validate configuration and exit\n\
  --diagnose             Check configured upstream routes and exit\n\
  --check-updates        Check official TgWsProxyPlus releases\n\
  --version --help\n\
Windows defaults to the native tray; other systems default to console.\n";

#[derive(Default)]
pub struct Args {
    pub path: Option<PathBuf>,
    pub init: Option<PathBuf>,
    pub log: Option<PathBuf>,
    pub console: bool,
    pub tray: bool,
    pub link: bool,
    pub check: bool,
    pub diagnose: bool,
    pub updates: bool,
    pub help: bool,
    pub version: bool,
    pub settings: bool,
    pub portable: bool,
    pub update_ready: Option<PathBuf>,
    overrides: Vec<(String, String)>,
}

impl Args {
    pub fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut args = arguments.into_iter().peekable();
        let mut result = Self::default();
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--console" => result.console = true,
                "--tray" => result.tray = true,
                "--settings" => {
                    result.tray = true;
                    result.settings = true;
                }
                "--portable" => result.portable = true,
                "--link" => result.link = true,
                "--check-config" => result.check = true,
                "--diagnose" => result.diagnose = true,
                "--check-updates" => result.updates = true,
                "--help" | "-h" => result.help = true,
                "--version" | "-V" => result.version = true,
                "--config" | "--init-config" | "--log-file" | "--update-ready" => {
                    let path = PathBuf::from(
                        args.next()
                            .with_context(|| format!("missing value for {flag}"))?,
                    );
                    match flag.as_str() {
                        "--config" => result.path = Some(path),
                        "--init-config" => result.init = Some(path),
                        "--update-ready" => result.update_ready = Some(path),
                        _ => result.log = Some(path),
                    }
                }
                "--dc-ip" => {
                    let value = if args.peek().is_some_and(|s| !s.starts_with('-')) {
                        args.next().unwrap_or_default()
                    } else {
                        String::new()
                    };
                    result.overrides.push((flag, value));
                }
                "--no-cfproxy" | "--no-h2" | "--no-secure" | "--force-test-dc"
                | "--proxy-protocol" | "--verbose" | "-v" | "--cfproxy-h2-media" => {
                    result.overrides.push((flag, String::new()))
                }
                "--host"
                | "--port"
                | "--secret"
                | "--pool-size"
                | "--buf-kb"
                | "--max-connections"
                | "--cfproxy-domain"
                | "--cfproxy-worker-domain"
                | "--fake-tls-domain"
                | "--log-max-mb"
                | "--log-backups" => {
                    let value = args
                        .next()
                        .with_context(|| format!("missing value for {flag}"))?;
                    result.overrides.push((flag, value));
                }
                _ => bail!("unknown argument: {flag}; use --help"),
            }
        }
        if result.console && result.tray {
            bail!("--console and --tray cannot be combined");
        }
        Ok(result)
    }

    pub fn apply(&self, config: &mut Config) -> Result<()> {
        let mut dc_cleared = false;
        let mut cf_cleared = false;
        let mut worker_cleared = false;
        for (key, value) in &self.overrides {
            match key.as_str() {
                "--host" => config.host = value.clone(),
                "--port" => config.port = value.parse().context("invalid port")?,
                "--secret" => config.secret = value.clone(),
                "--pool-size" => config.pool_size = value.parse()?,
                "--buf-kb" => config.buf_kb = value.parse()?,
                "--max-connections" => config.max_connections = value.parse()?,
                "--log-max-mb" => config.log_max_mb = value.parse()?,
                "--log-backups" => config.log_backups = value.parse()?,
                "--fake-tls-domain" => config.fake_tls_domain = value.clone(),
                "--no-cfproxy" => config.cfproxy = false,
                "--no-h2" => config.h2 = false,
                "--cfproxy-h2-media" => config.h2 = true,
                "--no-secure" => config.no_secure = true,
                "--force-test-dc" => config.force_test_dc = true,
                "--proxy-protocol" => config.proxy_protocol = true,
                "--verbose" | "-v" => config.verbose = true,
                "--dc-ip" => {
                    if !dc_cleared {
                        config.dc_ip.clear();
                        dc_cleared = true;
                    }
                    if !value.is_empty() {
                        config.dc_ip.push(value.clone());
                    }
                }
                "--cfproxy-domain" => {
                    if !cf_cleared {
                        config.cfproxy_user_domain.clear();
                        cf_cleared = true;
                    }
                    config.cfproxy_user_domain_enabled = true;
                    config.cfproxy_user_domain.push(value.clone());
                }
                "--cfproxy-worker-domain" => {
                    if !worker_cleared {
                        config.cfproxy_worker_domain.clear();
                        worker_cleared = true;
                    }
                    config.cfproxy_worker_enabled = true;
                    config.cfproxy_worker_domain.push(value.clone());
                }
                _ => unreachable!("validated command line option"),
            }
        }
        config.cfproxy_user_domain = normalize_domains(&config.cfproxy_user_domain);
        config.cfproxy_worker_domain = normalize_domains(&config.cfproxy_worker_domain);
        config.validate()
    }
}
