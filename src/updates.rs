use crate::{
    crypto::{hex, random},
    h2_transport,
    network::Network,
};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::time::timeout;

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub digest: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub html_url: String,
    pub assets: Vec<Asset>,
}

pub fn validate_repository(value: &str) -> Result<()> {
    let parts: Vec<_> = value.split('/').collect();
    ensure!(
        parts.len() == 2
            && parts.iter().all(|s| !s.is_empty()
                && *s != "."
                && *s != ".."
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))),
        "update_repository must be owner/repository"
    );
    Ok(())
}

pub fn newer(latest: &str, current: &str) -> Result<bool> {
    Ok(
        semver::Version::parse(latest.trim_start_matches(['v', 'V']))?
            > semver::Version::parse(current.trim_start_matches(['v', 'V']))?,
    )
}

pub async fn check(network: &Network, repository: &str) -> Result<Release> {
    ensure!(
        !repository.is_empty(),
        "update_repository is not configured; specify the Rust project's GitHub repository"
    );
    validate_repository(repository)?;
    let path = format!("/repos/{repository}/releases/latest");
    let response = timeout(
        Duration::from_secs(15),
        h2_transport::get(network, "api.github.com", &path, 1024 * 1024),
    )
    .await??;
    ensure!(
        response.status == 200,
        "GitHub release check returned HTTP {}",
        response.status
    );
    let release: Release = serde_json::from_slice(&response.body)?;
    semver::Version::parse(release.tag_name.trim_start_matches(['v', 'V']))?;
    ensure!(
        release
            .html_url
            .starts_with(&format!("https://github.com/{repository}/releases/")),
        "unexpected release page URL"
    );
    Ok(release)
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

pub async fn download(network: &Network, asset: &Asset, directory: &Path) -> Result<PathBuf> {
    let digest = asset
        .digest
        .as_deref()
        .context("release asset has no SHA-256 digest")?;
    let mut url = asset.browser_download_url.clone();
    for _ in 0..5 {
        let uri: http::Uri = url.parse()?;
        ensure!(
            uri.scheme_str() == Some("https"),
            "update downloads require HTTPS"
        );
        let host = uri.host().context("download URL has no host")?;
        ensure!(
            host == "github.com" || host.ends_with(".githubusercontent.com"),
            "unexpected update download host"
        );
        let response = h2_transport::get(
            network,
            host,
            uri.path_and_query().map_or("/", |p| p.as_str()),
            16 * 1024 * 1024,
        )
        .await?;
        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            url = response
                .headers
                .get("location")
                .context("redirect has no Location")?
                .to_str()?
                .into();
            continue;
        }
        ensure!(
            response.status == 200,
            "update download HTTP {}",
            response.status
        );
        verify_download(&response.body, digest)?;
        std::fs::create_dir_all(directory)?;
        let path = directory.join(format!("TgWsProxyPlus-update-{}.exe", hex(&random::<8>()?)));
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        file.write_all(&response.body)?;
        file.sync_all()?;
        return Ok(path);
    }
    bail!("too many update redirects")
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
    std::fs::rename(target, &backup).context("cannot move current executable to backup")?;
    let result = (|| {
        std::fs::rename(staged, target).context("cannot install the verified update")?;
        launch()
    })();
    if let Err(error) = result {
        if target.exists() {
            std::fs::rename(target, staged).context("cannot move failed update out of the way")?;
        }
        std::fs::rename(&backup, target).context("cannot restore the previous executable")?;
        return Err(error.context("update failed; previous executable restored"));
    }
    Ok(backup)
}
