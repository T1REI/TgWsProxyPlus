use crate::{
    crypto::{hex, random},
    network::Network,
    update_http,
};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::time::{sleep, timeout};

pub const REPOSITORY: &str = "T1REI/TgWsProxyPlus";
pub const MAX_DOWNLOAD: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub size: u64,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub html_url: String,
    pub assets: Vec<Asset>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
}

pub fn newer(latest: &str, current: &str) -> Result<bool> {
    Ok(
        semver::Version::parse(latest.trim_start_matches(['v', 'V']))?
            > semver::Version::parse(current.trim_start_matches(['v', 'V']))?,
    )
}

pub fn validate_release(release: &Release) -> Result<()> {
    let version = semver::Version::parse(release.tag_name.trim_start_matches(['v', 'V']))?;
    ensure!(
        !release.draft && !release.prerelease && version.pre.is_empty(),
        "release is not a stable published version"
    );
    ensure!(
        release.html_url
            == format!(
                "https://github.com/{REPOSITORY}/releases/tag/{}",
                release.tag_name
            ),
        "unexpected release page URL"
    );
    Ok(())
}

async fn request(
    network: &Network,
    host: &str,
    path: &str,
    limit: usize,
) -> Result<update_http::Response> {
    let mut last = None;
    for attempt in 0..3 {
        if attempt > 0 {
            sleep(Duration::from_secs(attempt)).await;
        }
        match update_http::get(network, host, path, limit).await {
            Ok(response) if response.status >= 500 => {
                last = Some(anyhow::anyhow!(
                    "GitHub is temporarily unavailable (HTTP {})",
                    response.status
                ));
            }
            Ok(response) => return Ok(response),
            Err(error) => last = Some(error),
        }
    }
    Err(last.context("update request failed")?)
}

pub async fn check(network: &Network) -> Result<Release> {
    timeout(Duration::from_secs(180), async {
        let response = request(
            network,
            "api.github.com",
            &format!("/repos/{REPOSITORY}/releases/latest"),
            1024 * 1024,
        )
        .await?;
        ensure!(
            response.status != 403 && response.status != 429,
            "GitHub rate limit or access restriction; try again later"
        );
        ensure!(
            response.status == 200,
            "GitHub release check returned HTTP {}",
            response.status
        );
        let release: Release = serde_json::from_slice(&response.body)?;
        validate_release(&release)?;
        Ok(release)
    })
    .await
    .context("update check timed out")?
}

pub fn verify_download(data: &[u8], expected: &str) -> Result<()> {
    let expected = expected
        .strip_prefix("sha256:")
        .context("release asset has no SHA-256 digest")?;
    ensure!(
        expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid SHA-256 digest"
    );
    ensure!(
        hex(&Sha256::digest(data)).eq_ignore_ascii_case(expected),
        "download SHA-256 mismatch"
    );
    Ok(())
}

pub fn verify_executable(data: &[u8], architecture: &str) -> Result<()> {
    ensure!(
        data.len() >= 64 && data.starts_with(b"MZ"),
        "update is not a Windows executable"
    );
    let pe = u32::from_le_bytes(data[60..64].try_into()?) as usize;
    let header = data
        .get(pe..pe.checked_add(26).context("invalid PE offset")?)
        .context("truncated PE header")?;
    ensure!(&header[..4] == b"PE\0\0", "invalid PE signature");
    let expected = match architecture {
        "x86_64" => 0x8664,
        "aarch64" => 0xaa64,
        _ => bail!("unsupported update architecture"),
    };
    ensure!(
        u16::from_le_bytes(header[4..6].try_into()?) == expected,
        "update has wrong architecture"
    );
    ensure!(
        u16::from_le_bytes(header[24..26].try_into()?) == 0x20b,
        "update is not a 64-bit executable"
    );
    Ok(())
}

pub fn select_asset(release: &Release) -> Result<&Asset> {
    validate_release(release)?;
    let expected = asset_name().context("unsupported update platform")?;
    let mut matches = release.assets.iter().filter(|asset| asset.name == expected);
    let asset = matches
        .next()
        .context("release has no executable for this platform")?;
    ensure!(
        matches.next().is_none(),
        "release has duplicate executables"
    );
    ensure!(
        asset.browser_download_url
            == format!(
                "https://github.com/{REPOSITORY}/releases/download/{}/{}",
                release.tag_name, expected
            ),
        "unexpected executable URL"
    );
    ensure!(
        asset.size > 0 && asset.size <= MAX_DOWNLOAD as u64,
        "invalid executable size"
    );
    let digest = asset
        .digest
        .as_deref()
        .context("release asset has no SHA-256 digest")?;
    let digest = digest
        .strip_prefix("sha256:")
        .context("invalid asset digest")?;
    ensure!(
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid asset digest"
    );
    Ok(asset)
}

pub fn download_url(url: &str) -> Result<http::Uri> {
    let uri: http::Uri = url.parse()?;
    ensure!(
        uri.scheme_str() == Some("https")
            && uri.port_u16().is_none()
            && !uri.authority().is_some_and(|a| a.as_str().contains('@')),
        "update downloads require standard HTTPS"
    );
    let host = uri.host().context("download URL has no host")?;
    ensure!(
        host == "github.com" || host.ends_with(".githubusercontent.com"),
        "unexpected update download host"
    );
    Ok(uri)
}

pub async fn download(network: &Network, asset: &Asset, directory: &Path) -> Result<PathBuf> {
    let digest = asset
        .digest
        .as_deref()
        .context("release asset has no SHA-256 digest")?;
    timeout(Duration::from_secs(240), async {
        let mut url = asset.browser_download_url.clone();
        for redirects in 0..=5 {
            let uri = download_url(&url)?;
            let host = uri.host().context("missing download host")?;
            let response = request(
                network,
                host,
                uri.path_and_query().map_or("/", |p| p.as_str()),
                MAX_DOWNLOAD,
            )
            .await?;
            if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                ensure!(redirects < 5, "too many update redirects");
                let location = response
                    .headers
                    .get("location")
                    .context("redirect has no Location")?
                    .to_str()?;
                url = if location.starts_with('/') && !location.starts_with("//") {
                    format!("https://{host}{location}")
                } else {
                    location.into()
                };
                continue;
            }
            ensure!(
                response.status == 200,
                "update download HTTP {}",
                response.status
            );
            ensure!(
                response.body.len() as u64 == asset.size,
                "update download size mismatch"
            );
            verify_download(&response.body, digest)?;
            if cfg!(windows) {
                verify_executable(&response.body, std::env::consts::ARCH)?;
            }
            std::fs::create_dir_all(directory)?;
            let path = directory.join(format!("TgWsProxyPlus-update-{}.exe", hex(&random::<8>()?)));
            let result = (|| -> Result<()> {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&path)?;
                file.write_all(&response.body)?;
                file.sync_all()?;
                Ok(())
            })();
            if let Err(error) = result {
                let _ = std::fs::remove_file(&path);
                return Err(error);
            }
            return Ok(path);
        }
        bail!("too many update redirects")
    })
    .await
    .context("update download timed out")?
}

pub fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("TgWsProxyPlus-windows-x86_64.exe"),
        ("windows", "aarch64") => Some("TgWsProxyPlus-windows-aarch64.exe"),
        ("linux", "x86_64") => Some("TgWsProxyPlus-linux-x86_64"),
        ("macos", "x86_64") => Some("TgWsProxyPlus-macos-x86_64"),
        ("macos", "aarch64") => Some("TgWsProxyPlus-macos-aarch64"),
        _ => None,
    }
}

pub fn replace_binary(
    staged: &Path,
    target: &Path,
    launch: impl FnOnce() -> Result<()>,
) -> Result<PathBuf> {
    let parent = target
        .parent()
        .context("executable has no parent directory")?;
    ensure!(
        staged.canonicalize()?.parent() == Some(parent.canonicalize()?.as_path()),
        "update must be staged beside the executable"
    );
    ensure!(
        staged.canonicalize()? != target.canonicalize()?,
        "staged file is the current executable"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(staged, std::fs::Permissions::from_mode(0o755))?;
    }
    let backup = parent.join(format!(
        "TgWsProxyPlus-old-{}{}",
        hex(&random::<8>()?),
        if cfg!(windows) { ".exe" } else { "" }
    ));
    replace_binary_with_backup(staged, target, &backup, launch)?;
    Ok(backup)
}

pub fn replace_binary_with_backup(
    staged: &Path,
    target: &Path,
    backup: &Path,
    launch: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let parent = target
        .parent()
        .context("executable has no parent directory")?
        .canonicalize()?;
    ensure!(
        staged.canonicalize()?.parent() == Some(parent.as_path()),
        "update must be staged beside the executable"
    );
    ensure!(
        backup
            .parent()
            .context("backup has no parent")?
            .canonicalize()?
            == parent
            && !backup.exists(),
        "invalid backup path"
    );
    ensure!(
        staged.canonicalize()? != target.canonicalize()?,
        "staged file is the current executable"
    );
    std::fs::rename(target, backup).context("cannot move current executable to backup")?;
    let result = (|| {
        std::fs::rename(staged, target).context("cannot install the verified update")?;
        launch()
    })();
    if let Err(error) = result {
        if target.exists() {
            std::fs::rename(target, staged).context("cannot move failed update out of the way")?;
        }
        std::fs::rename(backup, target).context("cannot restore the previous executable")?;
        return Err(error.context("update failed; previous executable restored"));
    }
    Ok(())
}
