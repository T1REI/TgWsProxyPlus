#[cfg(all(windows, feature = "desktop"))]
use crate::{
    crypto::{hex, random},
    updates,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
#[cfg(all(windows, feature = "desktop"))]
use std::io::Write;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

pub struct Prepared {
    pub path: PathBuf,
    pub digest: String,
    pub version: String,
}

#[derive(Serialize, Deserialize)]
pub struct Plan {
    pub target: PathBuf,
    pub staged: PathBuf,
    pub config: PathBuf,
    pub digest: String,
    pub version: String,
    pub nonce: String,
    pub parent: u32,
    pub helper_pid: u32,
}

impl Plan {
    pub fn path(&self, suffix: &str) -> Result<PathBuf> {
        ensure!(
            self.nonce.len() == 32 && self.nonce.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid update identifier"
        );
        Ok(self
            .target
            .parent()
            .context("executable has no parent")?
            .join(format!("TgWsProxyPlus-install-{}{suffix}", self.nonce)))
    }

    pub fn read(path: &Path) -> Result<Self> {
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take(32 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 32 * 1024, "update plan exceeds limit");
        let plan: Self = serde_json::from_slice(&bytes)?;
        ensure!(
            path.canonicalize()? == plan.path(".json")?.canonicalize()?,
            "invalid update plan location"
        );
        let parent = plan
            .target
            .parent()
            .context("executable has no parent")?
            .canonicalize()?;
        ensure!(
            plan.staged
                .parent()
                .context("staged file has no parent")?
                .canonicalize()?
                == parent,
            "update staged outside executable directory"
        );
        ensure!(
            plan.target.file_name() != plan.staged.file_name(),
            "update targets itself"
        );
        semver::Version::parse(plan.version.trim_start_matches(['v', 'V']))?;
        Ok(plan)
    }

    #[cfg(all(windows, feature = "desktop"))]
    fn write(&self, path: &Path) -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)?;
        file.write_all(&serde_json::to_vec(self)?)?;
        file.sync_all()?;
        Ok(())
    }
}

#[cfg(all(windows, feature = "desktop"))]
fn wait_process(pid: u32, milliseconds: u32) -> Result<()> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, WAIT_OBJECT_0},
        System::Threading::{OpenProcess, WaitForSingleObject},
    };
    ensure!(pid != std::process::id(), "updater cannot wait for itself");
    unsafe {
        let handle = OpenProcess(0x00100000, 0, pid);
        if handle.is_null() {
            ensure!(
                GetLastError() == ERROR_INVALID_PARAMETER,
                "cannot open update process"
            );
            return Ok(());
        }
        let result = WaitForSingleObject(handle, milliseconds);
        CloseHandle(handle);
        ensure!(
            result == WAIT_OBJECT_0,
            "update process did not exit in time"
        );
    }
    Ok(())
}

#[cfg(all(windows, feature = "desktop"))]
fn command(path: &Path) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut command = std::process::Command::new(path);
    command.creation_flags(0x08000000);
    command
}

#[cfg(all(windows, feature = "desktop"))]
pub fn handoff(prepared: Prepared, config: &Path) -> Result<()> {
    let target = std::env::current_exe()?.canonicalize()?;
    let plan = Plan {
        target,
        staged: prepared.path.canonicalize()?,
        config: config.canonicalize()?,
        digest: prepared.digest,
        version: prepared.version,
        nonce: hex(&random::<16>()?),
        parent: std::process::id(),
        helper_pid: 0,
    };
    let path = plan.path(".json")?;
    let helper = plan.path(".exe")?;
    let result = (|| -> Result<()> {
        updates::verify_download(&fs::read(&plan.staged)?, &plan.digest)?;
        plan.write(&path)?;
        fs::copy(&plan.target, &helper)?;
        fs::OpenOptions::new()
            .write(true)
            .open(&helper)?
            .sync_all()?;
        command(&helper).arg("--apply-update").arg(&path).spawn()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(helper);
    }
    result
}

pub fn await_ready(
    child: &mut std::process::Child,
    path: &Path,
    nonce: &str,
    deadline: std::time::Duration,
) -> Result<()> {
    let started = std::time::Instant::now();
    let result = (|| {
        while started.elapsed() < deadline {
            ensure!(
                child.try_wait()?.is_none(),
                "updated application exited during startup"
            );
            if fs::read(path).is_ok_and(|bytes| bytes == nonce.as_bytes()) {
                std::thread::sleep(std::time::Duration::from_millis(500));
                ensure!(
                    child.try_wait()?.is_none(),
                    "updated application exited after startup"
                );
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        anyhow::bail!("updated application did not confirm startup");
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(all(windows, feature = "desktop"))]
pub fn apply(path: &Path) -> Result<()> {
    let mut plan = Plan::read(path)?;
    ensure!(
        std::env::current_exe()?.canonicalize()? == plan.path(".exe")?.canonicalize()?,
        "invalid updater executable"
    );
    wait_process(plan.parent, 60000)?;
    let result = (|| -> Result<()> {
        let bytes = fs::read(&plan.staged)?;
        updates::verify_download(&bytes, &plan.digest)?;
        updates::verify_executable(&bytes, std::env::consts::ARCH)?;
        let mut probe = command(&plan.staged)
            .arg("--version")
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        let began = std::time::Instant::now();
        while probe.try_wait()?.is_none() {
            if began.elapsed() >= std::time::Duration::from_secs(5) {
                let _ = probe.kill();
                let _ = probe.wait();
                anyhow::bail!("update version check timed out");
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let output = probe.wait_with_output()?;
        ensure!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains(&format!(
                    " {} ",
                    plan.version.trim_start_matches(['v', 'V'])
                )),
            "update version does not match the release"
        );
        plan.helper_pid = std::process::id();
        let mut file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)?;
        file.write_all(&serde_json::to_vec(&plan)?)?;
        file.sync_all()?;
        drop(file);
        updates::replace_binary_with_backup(
            &plan.staged,
            &plan.target,
            &plan.path(".backup.exe")?,
            || {
                let mut child = command(&plan.target)
                    .arg("--tray")
                    .arg("--config")
                    .arg(&plan.config)
                    .arg("--update-ready")
                    .arg(path)
                    .spawn()?;
                await_ready(
                    &mut child,
                    &plan.path(".ready")?,
                    &plan.nonce,
                    std::time::Duration::from_secs(30),
                )
            },
        )
    })();
    if result.is_err() && plan.target.is_file() {
        let _ = command(&plan.target)
            .arg("--tray")
            .arg("--config")
            .arg(&plan.config)
            .spawn();
    }
    result
}

#[cfg(all(windows, feature = "desktop"))]
pub fn confirm_ready(path: &Path) -> Result<()> {
    let plan = Plan::read(path)?;
    ensure!(
        std::env::current_exe()?.canonicalize()? == plan.target.canonicalize()?,
        "unexpected updated executable"
    );
    ensure!(
        plan.version.trim_start_matches(['v', 'V']) == env!("CARGO_PKG_VERSION"),
        "unexpected updated version"
    );
    let ready = plan.path(".ready")?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&ready)?;
    file.write_all(plan.nonce.as_bytes())?;
    file.sync_all()?;
    let path = path.to_path_buf();
    std::thread::Builder::new()
        .name("update-cleanup".into())
        .spawn(move || {
            if wait_process(plan.helper_pid, 60000).is_ok() {
                if let Ok(helper) = plan.path(".exe") {
                    let _ = fs::remove_file(helper);
                }
                let _ = fs::remove_file(path);
                let _ = fs::remove_file(ready);
            }
        })?;
    Ok(())
}
