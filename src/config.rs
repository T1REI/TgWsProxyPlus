use crate::crypto::{hex, random, unhex};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub secret: String,
    pub dc_ip: Vec<String>,
    pub buf_kb: usize,
    pub pool_size: usize,
    pub max_connections: usize,
    pub cfproxy: bool,
    pub h2: bool,
    pub h2_max_connections: usize,
    pub h2_stream_window_kb: usize,
    pub h2_connection_window_kb: usize,
    pub cfproxy_user_domain_enabled: bool,
    #[serde(deserialize_with = "domain_list")]
    pub cfproxy_user_domain: Vec<String>,
    pub cfproxy_worker_enabled: bool,
    #[serde(deserialize_with = "domain_list")]
    pub cfproxy_worker_domain: Vec<String>,
    pub no_secure: bool,
    pub fake_tls_domain: String,
    pub proxy_protocol: bool,
    pub force_test_dc: bool,
    pub verbose: bool,
    pub log_max_mb: f64,
    pub log_backups: usize,
    pub check_updates: bool,
    pub update_repository: String,
    pub autostart: bool,
    pub appearance: String,
    pub language: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 1443,
            secret: String::new(),
            dc_ip: vec!["2:149.154.167.220".into(), "4:149.154.167.220".into()],
            buf_kb: 0,
            pool_size: 4,
            max_connections: 512,
            cfproxy: true,
            h2: true,
            h2_max_connections: 2,
            h2_stream_window_kb: 1024,
            h2_connection_window_kb: 4096,
            cfproxy_user_domain_enabled: false,
            cfproxy_user_domain: vec![],
            cfproxy_worker_enabled: false,
            cfproxy_worker_domain: vec![],
            no_secure: false,
            fake_tls_domain: String::new(),
            proxy_protocol: false,
            force_test_dc: false,
            verbose: false,
            log_max_mb: 5.0,
            log_backups: 1,
            check_updates: true,
            update_repository: "T1REI/TgWsProxyPlus".into(),
            autostart: false,
            appearance: "auto".into(),
            language: "ru".into(),
            extra: BTreeMap::new(),
        }
    }
}

fn domain_list<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum List {
        Text(String),
        Items(Vec<String>),
    }
    let items = match List::deserialize(deserializer)? {
        List::Text(s) => vec![s],
        List::Items(v) => v,
    };
    Ok(normalize_domains(&items))
}

pub fn normalize_domains(items: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    for item in items
        .iter()
        .flat_map(|s| s.split(|c: char| c.is_whitespace() || c == ',' || c == ';'))
    {
        let item = item.to_ascii_lowercase();
        if !item.is_empty() && !result.contains(&item) {
            result.push(item);
        }
    }
    result
}

pub fn valid_domain(domain: &str) -> bool {
    let labels: Vec<_> = domain.split('.').collect();
    domain.is_ascii()
        && domain.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|s| {
            !s.is_empty()
                && s.len() <= 63
                && !s.starts_with('-')
                && !s.ends_with('-')
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        && labels
            .last()
            .is_some_and(|s| s.len() >= 2 && s.bytes().any(|b| b.is_ascii_alphabetic()))
}

pub fn parse_dc_ips(items: &[String]) -> Result<BTreeMap<u16, Ipv4Addr>> {
    let mut result = BTreeMap::new();
    for item in items {
        let (dc, ip) = item.split_once(':').context("DC mapping must be DC:IPv4")?;
        let dc = dc.parse::<u16>().context("invalid DC number")?;
        ensure!(matches!(dc, 1..=5 | 203), "unsupported DC {dc}");
        result.insert(
            dc,
            ip.parse()
                .with_context(|| format!("invalid IPv4 for DC{dc}"))?,
        );
    }
    Ok(result)
}

impl Config {
    pub fn fresh() -> Result<Self> {
        Ok(Self {
            secret: hex(&random::<16>()?),
            ..Self::default()
        })
    }

    pub fn validate(&self) -> Result<()> {
        self.host
            .parse::<IpAddr>()
            .context("host must be an IPv4 or IPv6 address")?;
        ensure!(self.port > 0, "port must be between 1 and 65535");
        self.secret_bytes()?;
        parse_dc_ips(&self.dc_ip)?;
        ensure!(
            self.buf_kb == 0 || (4..=4096).contains(&self.buf_kb),
            "buf_kb must be 0 (automatic) or 4..4096"
        );
        ensure!(self.pool_size <= 32, "pool_size must be 0..32");
        ensure!(
            (1..=3).contains(&self.h2_max_connections),
            "h2_max_connections must be 1..3"
        );
        ensure!(
            (64..=4096).contains(&self.h2_stream_window_kb),
            "h2_stream_window_kb must be 64..4096"
        );
        ensure!(
            (self.h2_stream_window_kb..=16384).contains(&self.h2_connection_window_kb),
            "h2_connection_window_kb must be at least the stream window and at most 16384"
        );
        ensure!(
            (1..=16384).contains(&self.max_connections),
            "max_connections must be 1..16384"
        );
        ensure!(
            self.log_max_mb.is_finite() && (0.03125..=1024.0).contains(&self.log_max_mb),
            "log_max_mb must be 0.03125..1024"
        );
        ensure!(
            (1..=20).contains(&self.log_backups),
            "log_backups must be 1..20"
        );
        for domain in self
            .cfproxy_user_domain
            .iter()
            .chain(&self.cfproxy_worker_domain)
        {
            ensure!(valid_domain(domain), "invalid domain: {domain}");
        }
        ensure!(
            self.fake_tls_domain.is_empty() || valid_domain(&self.fake_tls_domain),
            "invalid Fake TLS domain"
        );
        ensure!(
            ["ru", "en", "auto"].contains(&self.language.as_str()),
            "language must be ru, en or auto"
        );
        ensure!(
            ["auto", "dark", "light"].contains(&self.appearance.as_str()),
            "appearance must be auto, dark or light"
        );
        if !self.update_repository.is_empty() {
            crate::updates::validate_repository(&self.update_repository)?;
        }
        Ok(())
    }

    pub fn secret_bytes(&self) -> Result<[u8; 16]> {
        ensure!(
            self.secret.len() == 32,
            "secret must contain exactly 32 hexadecimal characters"
        );
        unhex(&self.secret)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid secret"))
    }

    pub fn h2_enabled(&self) -> bool {
        self.h2 && self.cfproxy && !self.no_secure && !self.force_test_dc
    }
    pub fn worker_domains(&self) -> &[String] {
        if self.cfproxy_worker_enabled {
            &self.cfproxy_worker_domain
        } else {
            &[]
        }
    }

    pub fn link(&self) -> String {
        let host = match self.host.as_str() {
            "0.0.0.0" => "127.0.0.1",
            "::" => "::1",
            host => host,
        };
        let secret = if self.fake_tls_domain.is_empty() {
            format!("dd{}", self.secret)
        } else {
            format!("ee{}{}", self.secret, hex(self.fake_tls_domain.as_bytes()))
        };
        format!(
            "tg://proxy?server={}&port={}&secret={secret}",
            host.replace(':', "%3A"),
            self.port
        )
    }

    pub fn load(path: &Path) -> Result<Self> {
        let data = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
        ensure!(data.len() <= 1024 * 1024, "configuration exceeds 1 MiB");
        let config: Self = serde_json::from_slice(&data).context("invalid configuration JSON")?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".config-{}.tmp", hex(&random::<8>()?)));
        let result = (|| {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&serde_json::to_vec_pretty(self)?)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

pub fn app_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let parent = exe.parent().context("executable has no parent directory")?;
    if parent.join("TgWsProxyPlus.portable").exists()
        || parent.join("TgWsProxyPlus-data/config.json").is_file()
    {
        return Ok(parent.join("TgWsProxyPlus-data"));
    }
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base =
        std::env::var_os("HOME").map(|p| PathBuf::from(p).join("Library/Application Support"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")));
    Ok(base
        .context("cannot locate user configuration directory; use --config PATH")?
        .join("TgWsProxyPlus"))
}
