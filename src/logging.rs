use anyhow::Result;
use log::{LevelFilter, Log, Metadata, Record};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering::Relaxed},
    },
    time::{SystemTime, UNIX_EPOCH},
};

struct FileSink {
    path: PathBuf,
    file: Option<File>,
    limit: u64,
    backups: usize,
    size: u64,
}
static LOGGER: Logger = Logger {
    file: Mutex::new(None),
    console: AtomicBool::new(true),
};
struct Logger {
    file: Mutex<Option<FileSink>>,
    console: AtomicBool,
}

fn backup(path: &Path, n: usize) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(format!(".{n}"));
    value.into()
}

impl FileSink {
    fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        if self.size + data.len() as u64 > self.limit {
            self.file.take();
            let oldest = backup(&self.path, self.backups);
            if oldest.exists() {
                fs::remove_file(oldest)?;
            }
            for n in (1..self.backups).rev() {
                let from = backup(&self.path, n);
                if from.exists() {
                    fs::rename(from, backup(&self.path, n + 1))?;
                }
            }
            if self.path.exists() {
                fs::rename(&self.path, backup(&self.path, 1))?;
            }
            self.size = 0;
        }
        if self.file.is_none() {
            self.file = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?,
            );
        }
        if let Some(file) = &mut self.file {
            file.write_all(data)?;
            file.flush()?;
        }
        self.size += data.len() as u64;
        Ok(())
    }
}

pub fn censor(message: &str) -> String {
    let mut result = String::with_capacity(message.len());
    let mut token = String::new();
    let flush = |token: &mut String, result: &mut String| {
        if crate::config::valid_domain(token)
            && token != "telegram.org"
            && !token.ends_with(".telegram.org")
            && !token.ends_with(".log")
        {
            let labels: Vec<_> = token.split('.').collect();
            for (index, label) in labels.iter().enumerate() {
                if index > 0 {
                    result.push('.');
                }
                if index == labels.len() - 1 {
                    result.push_str(label);
                } else {
                    let keep = label.len() / 2;
                    result.push_str(&label[..keep]);
                    result.extend(std::iter::repeat_n('*', label.len() - keep));
                }
            }
        } else {
            result.push_str(token);
        }
        token.clear();
    };
    for c in message.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
            token.push(c);
        } else {
            flush(&mut token, &mut result);
            result.push(c);
        }
    }
    flush(&mut token, &mut result);
    result
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }
    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata())
            || !(record.target().starts_with("tg_ws_proxy_plus")
                || record.target().starts_with("TgWsProxyPlus"))
        {
            return;
        }
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let line = format!(
            "{:02}:{:02}:{:02} UTC {:5} {}\n",
            seconds / 3600 % 24,
            seconds / 60 % 60,
            seconds % 60,
            record.level(),
            censor(&record.args().to_string())
        );
        if self.console.load(Relaxed) {
            eprint!("{line}");
        }
        if let Ok(mut sink) = self.file.lock()
            && let Some(sink) = &mut *sink
            && let Err(error) = sink.write(line.as_bytes())
        {
            eprintln!("log file error: {error}");
        }
    }
    fn flush(&self) {
        if let Ok(mut sink) = self.file.lock()
            && let Some(sink) = &mut *sink
            && let Some(file) = &mut sink.file
        {
            let _ = file.flush();
        }
    }
}

pub fn install(
    path: Option<&Path>,
    max_mb: f64,
    backups: usize,
    verbose: bool,
    console: bool,
) -> Result<()> {
    configure(path, max_mb, backups, verbose, console)?;
    log::set_logger(&LOGGER).map_err(|error| anyhow::anyhow!("cannot install logger: {error}"))?;
    Ok(())
}

pub fn configure(
    path: Option<&Path>,
    max_mb: f64,
    backups: usize,
    verbose: bool,
    console: bool,
) -> Result<()> {
    if let Some(path) = path {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let size = file.metadata()?.len();
        *LOGGER
            .file
            .lock()
            .map_err(|_| anyhow::anyhow!("logger lock poisoned"))? = Some(FileSink {
            path: path.into(),
            file: Some(file),
            limit: ((max_mb * 1024.0 * 1024.0) as u64).max(32768),
            backups: backups.clamp(1, 20),
            size,
        });
    } else {
        *LOGGER
            .file
            .lock()
            .map_err(|_| anyhow::anyhow!("logger lock poisoned"))? = None;
    }
    LOGGER.console.store(console, Relaxed);
    log::set_max_level(if verbose {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    });
    Ok(())
}
