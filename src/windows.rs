use anyhow::{Context, Result, bail, ensure};
use std::{
    cell::{Cell, RefCell},
    ffi::OsStr,
    mem::{size_of, zeroed},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    rc::Rc,
    sync::{atomic::Ordering::Relaxed, mpsc},
    thread,
    time::{Duration, Instant},
};
use tg_ws_proxy_plus::{
    config::Config, crypto, logging, network::Network, routing::Routing, server, stats::Stats,
    updates,
};
use windows_sys::Win32::{
    Foundation::*,
    Globalization::*,
    Graphics::{Dwm::*, Gdi::*},
    System::{Console::*, DataExchange::*, LibraryLoader::*, Memory::*, Registry::*, Threading::*},
    UI::{HiDpi::*, Shell::*, WindowsAndMessaging::*},
};
use windows_sys::core::BOOL;
#[cfg(test)]
mod integration_tests;
mod settings;
mod telegram;
mod webview;
const CLASS: &str = "TgWsProxyPlusWindow";
const TRAY: u32 = WM_APP + 1;
const EVENT: u32 = WM_APP + 2;
const OPEN: usize = 1;
const COPY: usize = 2;
const RESTART: usize = 3;
const SETTINGS: usize = 4;
const LOGS: usize = 5;
const QUIT: usize = 6;
const DIAGNOSE: usize = 7;
const CONNECT: usize = 8;
fn review_build() -> bool {
    option_env!("TGWS_REVIEW_BUILD") == Some("1")
}
fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

pub fn attach_console() {
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

pub fn show_error(text: &str) {
    message(null_mut(), text, "TgWsProxy+", MB_OK | MB_ICONERROR);
}

fn message(owner: HWND, text: &str, title: &str, style: u32) -> i32 {
    unsafe { MessageBoxW(owner, wide(text).as_ptr(), wide(title).as_ptr(), style) }
}

fn open(target: impl AsRef<OsStr>) -> Result<()> {
    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            wide("open").as_ptr(),
            wide(target).as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    ensure!(
        result as usize > 32,
        "Windows cannot open the selected link or file"
    );
    Ok(())
}

fn clipboard(owner: HWND, value: &str) -> Result<()> {
    let data = wide(value);
    unsafe {
        ensure!(OpenClipboard(owner) != 0, "clipboard is busy");
        struct Clipboard;
        impl Drop for Clipboard {
            fn drop(&mut self) {
                unsafe {
                    CloseClipboard();
                }
            }
        }
        let _clipboard = Clipboard;
        ensure!(EmptyClipboard() != 0, "cannot empty clipboard");
        let allocation = GlobalAlloc(GMEM_MOVEABLE, data.len() * 2);
        ensure!(!allocation.is_null(), "clipboard allocation failed");
        let pointer = GlobalLock(allocation) as *mut u16;
        if pointer.is_null() {
            GlobalFree(allocation);
            bail!("clipboard allocation cannot be locked");
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), pointer, data.len());
        GlobalUnlock(allocation);
        if SetClipboardData(13, allocation).is_null() {
            GlobalFree(allocation);
            bail!("cannot set clipboard contents");
        }
    }
    Ok(())
}

fn autostart(enabled: bool, config_path: &Path) -> Result<()> {
    let name = wide("TgWsProxyPlus");
    unsafe {
        let mut key = null_mut();
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run").as_ptr(),
            0,
            null(),
            0,
            KEY_SET_VALUE,
            null(),
            &mut key,
            null_mut(),
        );
        ensure!(
            status == ERROR_SUCCESS,
            "cannot open autostart registry key: {status}"
        );
        let result = if enabled {
            let exe = std::env::current_exe()?;
            let command = wide(format!(
                "\"{}\" --tray --config \"{}\"",
                exe.display(),
                config_path.display()
            ));
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                command.as_ptr().cast(),
                (command.len() * 2) as u32,
            )
        } else {
            RegDeleteValueW(key, name.as_ptr())
        };
        RegCloseKey(key);
        ensure!(
            result == ERROR_SUCCESS || (!enabled && result == ERROR_FILE_NOT_FOUND),
            "cannot change autostart: {result}"
        );
    }
    Ok(())
}

struct ProxyThread {
    stats: std::sync::Arc<Stats>,
    metrics: std::sync::Arc<tg_ws_proxy_plus::performance::Metrics>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Drop for ProxyThread {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            log::error!("proxy thread panicked");
        }
    }
}

enum Event {
    Error(u64, String),
    Status(u64, String),
    Diagnostics(std::result::Result<String, String>),
    Command(String),
    UpdateChecked(std::result::Result<updates::Release, String>, bool),
    Install(std::result::Result<tg_ws_proxy_plus::update_install::Prepared, String>),
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: u64,
    command: String,
    #[serde(default)]
    data: serde_json::Value,
}

struct Ui {
    hwnd: HWND,
    config: Config,
    path: PathBuf,
    proxy: Option<ProxyThread>,
    sender: mpsc::Sender<Event>,
    receiver: mpsc::Receiver<Event>,
    menu: HMENU,
    icon: HICON,
    brush: HBRUSH,
    webview: Rc<RefCell<webview::State>>,
    telegram: telegram::Launcher,
    update_busy: Cell<bool>,
    prompted_version: Option<String>,
    update_status: String,
    ready: bool,
    status: String,
    diagnostics: String,
    diagnosing: bool,
    generation: u64,
    started: Instant,
    last_click: Option<Instant>,
    update_ready: Option<PathBuf>,
}

impl Ui {
    fn new(config: Config, path: PathBuf, update_ready: Option<PathBuf>) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            hwnd: null_mut(),
            config,
            path,
            proxy: None,
            sender,
            receiver,
            menu: null_mut(),
            icon: null_mut(),
            brush: null_mut(),
            webview: Rc::new(RefCell::new(webview::State::Empty)),
            telegram: telegram::Launcher::new(),
            update_busy: Cell::new(false),
            prompted_version: None,
            update_status: String::new(),
            ready: false,
            status: String::new(),
            diagnostics: String::new(),
            diagnosing: false,
            generation: 0,
            started: Instant::now(),
            last_click: None,
            update_ready,
        }
    }
    fn ru(&self) -> bool {
        if self.config.language == "auto" {
            unsafe { GetUserDefaultUILanguage() & 0x03ff == 0x19 }
        } else {
            self.config.language != "en"
        }
    }
    fn tr<'a>(&self, ru: &'a str, en: &'a str) -> &'a str {
        if self.ru() { ru } else { en }
    }
    fn post(sender: &mpsc::Sender<Event>, hwnd: usize, event: Event) {
        if sender.send(event).is_ok() {
            // SAFETY: posting to a live or recently closed HWND does not dereference it.
            unsafe {
                PostMessageW(hwnd as HWND, EVENT, 0, 0);
            }
        }
    }
    fn start(&mut self) -> Result<()> {
        self.proxy.take();
        self.generation += 1;
        let generation = self.generation;
        self.ready = false;
        self.status = self.tr("Запускаем прокси…", "Starting proxy…").into();
        self.started = std::time::Instant::now();
        let config = self.config.clone();
        let tx = self.sender.clone();
        let hwnd = self.hwnd as usize;
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let metrics = std::sync::Arc::new(tg_ws_proxy_plus::performance::Metrics::default());
        let worker_metrics = metrics.clone();
        let stats = std::sync::Arc::new(Stats::default());
        let worker_stats = stats.clone();
        let thread = thread::Builder::new().name("proxy".into()).spawn(move || {
            let result = (|| -> Result<()> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(async {
                    let listener =
                        tokio::net::TcpListener::bind((config.host.as_str(), config.port))
                            .await
                            .context("cannot listen; check the address and port")?;
                    let network = Network::new(config.buf_kb * 1024)?;
                    let mut routing =
                        Routing::new(std::sync::Arc::new(config), network, worker_stats);
                    if let Some(routing) = std::sync::Arc::get_mut(&mut routing) {
                        routing.metrics = worker_metrics;
                    }
                    Self::post(
                        &tx,
                        hwnd,
                        Event::Status(
                            generation,
                            format!("{}:{}", routing.config.host, routing.config.port),
                        ),
                    );
                    server::serve(listener, routing, async {
                        let _ = stopped.await;
                    })
                    .await
                })
            })();
            if let Err(error) = result {
                Self::post(&tx, hwnd, Event::Error(generation, format!("{error:#}")));
            }
        })?;
        self.proxy = Some(ProxyThread {
            stats,
            metrics,
            stop: Some(stop),
            thread: Some(thread),
        });
        Ok(())
    }

    fn rebuild_menu(&mut self) -> Result<()> {
        unsafe {
            if !self.menu.is_null() {
                DestroyMenu(self.menu);
            }
            self.menu = CreatePopupMenu();
            ensure!(!self.menu.is_null(), "cannot create tray menu");
            for (id, label) in [
                (OPEN, self.tr("Открыть Telegram", "Open Telegram")),
                (
                    CONNECT,
                    self.tr("Подключить прокси в Telegram", "Connect proxy in Telegram"),
                ),
                (COPY, self.tr("Скопировать ссылку", "Copy link")),
                (SETTINGS, self.tr("Открыть TgWsProxy+", "Open TgWsProxy+")),
                (DIAGNOSE, self.tr("Диагностика", "Diagnostics")),
                (LOGS, self.tr("Открыть журнал", "Open log")),
                (RESTART, self.tr("Перезапустить прокси", "Restart proxy")),
                (QUIT, self.tr("Выход", "Quit")),
            ] {
                AppendMenuW(self.menu, MF_STRING, id, wide(label).as_ptr());
            }
        }
        Ok(())
    }
    fn show(&mut self) -> Result<()> {
        unsafe {
            ShowWindow(self.hwnd, SW_RESTORE);
            SetForegroundWindow(self.hwnd);
        }
        let create = matches!(
            *self.webview.borrow(),
            webview::State::Empty | webview::State::Failed(_)
        );
        if create {
            if let Err(error) = webview::create(
                self.hwnd as usize,
                &self.path.with_file_name("WebView2"),
                self.webview.clone(),
                self.sender.clone(),
            ) {
                *self.webview.borrow_mut() = webview::State::Failed(error.to_string());
                self.report_webview_error();
            }
        } else if let webview::State::Ready(view) = &*self.webview.borrow() {
            view.visible(true)?;
        }
        self.publish();
        Ok(())
    }
    fn report_webview_error(&mut self) {
        let error = if let webview::State::Failed(error) = &*self.webview.borrow() {
            Some(error.clone())
        } else {
            None
        };
        if let Some(error) = error {
            *self.webview.borrow_mut() = webview::State::Empty;
            log::error!("WebView2: {error}");
            message(self.hwnd, &format!("{}\n\n{error}\n\nhttps://developer.microsoft.com/microsoft-edge/webview2/", self.tr("Не удалось открыть интерфейс. Установи Microsoft Edge WebView2 Runtime. Прокси продолжает работать в трее.", "Cannot open the interface. Install Microsoft Edge WebView2 Runtime. The proxy continues running in the tray.")), "TgWsProxy+", MB_OK | MB_ICONERROR);
        }
    }
    fn system_dark() -> bool {
        let mut light = 0u32;
        let mut size = 4u32;
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize").as_ptr(),
                wide("AppsUseLightTheme").as_ptr(),
                RRF_RT_REG_DWORD,
                null_mut(),
                (&mut light as *mut u32).cast(),
                &mut size,
            );
        }
        light == 0
    }
    fn snapshot(&self) -> serde_json::Value {
        let mut stats = serde_json::json!({});
        let mut routes = Vec::new();
        if let Some(proxy) = &self.proxy {
            let s = &proxy.stats;
            stats = serde_json::json!({ "active": s.active.load(Relaxed), "total": s.total.load(Relaxed), "up": s.up.load(Relaxed), "down": s.down.load(Relaxed), "errors": s.errors.load(Relaxed), "ws": s.ws.load(Relaxed), "h2": s.h2.load(Relaxed), "tcp": s.tcp.load(Relaxed) });
            routes = proxy.metrics.snapshot();
        }
        serde_json::json!({ "version": tg_ws_proxy_plus::VERSION, "config": self.config,
            "ready": self.ready, "status": self.status, "uptime": self.started.elapsed().as_secs(),
            "stats": stats, "routes": routes, "system_dark": Self::system_dark(),
            "system_language": if unsafe { GetUserDefaultUILanguage() & 0x03ff == 0x19 } { "ru" } else { "en" },
            "diagnostics": self.diagnostics, "diagnosing": self.diagnosing, "update_status": self.update_status, "review": review_build() })
    }
    fn publish(&self) {
        if unsafe { IsWindowVisible(self.hwnd) } == 0 {
            return;
        }
        if let webview::State::Ready(view) = &*self.webview.borrow()
            && let Err(error) =
                view.send(&serde_json::json!({ "event": "state", "data": self.snapshot() }))
        {
            log::warn!("UI snapshot: {error:#}");
        }
    }
    fn save(&mut self, value: serde_json::Value) -> Result<()> {
        let next = settings::parse(value, &self.config)?;
        let restart = settings::network_changed(&self.config, &next)?;
        let autostart_changed = next.autostart != self.config.autostart;
        if autostart_changed {
            autostart(next.autostart, &self.path)?;
        }
        if let Err(error) = next.save(&self.path) {
            if autostart_changed {
                let _ = autostart(self.config.autostart, &self.path);
            }
            return Err(error);
        }
        self.config = next;
        if let Err(error) = logging::configure(
            Some(&self.path.with_file_name("TgWsProxyPlus.log")),
            self.config.log_max_mb,
            self.config.log_backups,
            self.config.verbose,
            false,
        ) {
            log::error!("logging configuration: {error:#}");
        }
        self.rebuild_menu()?;
        if restart {
            self.start()?;
        }
        Ok(())
    }
    fn diagnose(&mut self) -> Result<()> {
        if self.diagnosing {
            return Ok(());
        }
        let config = self.config.clone();
        let mut live = self
            .proxy
            .as_ref()
            .map(|p| {
                let mut lines = vec![p.stats.summary()];
                lines.extend(p.metrics.report());
                lines
            })
            .unwrap_or_default();
        let tx = self.sender.clone();
        let hwnd = self.hwnd as usize;
        thread::Builder::new()
            .name("diagnostics".into())
            .spawn(move || {
                let result = (|| -> Result<String> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    live.push("--- Connection probes ---".into());
                    live.extend(runtime.block_on(server::diagnose(config))?);
                    Ok(live.join("\n"))
                })();
                Self::post(
                    &tx,
                    hwnd,
                    Event::Diagnostics(result.map_err(|error| format!("{error:#}"))),
                );
            })?;
        self.diagnosing = true;
        self.diagnostics.clear();
        Ok(())
    }
    fn command(&mut self, command: usize) -> Result<()> {
        match command {
            OPEN => {
                if self
                    .last_click
                    .is_none_or(|last| last.elapsed() > Duration::from_millis(400))
                {
                    self.last_click = Some(Instant::now());
                    let started = Instant::now();
                    self.telegram.open()?;
                    log::debug!(
                        "Telegram launch dispatched in {} ms",
                        started.elapsed().as_millis()
                    );
                }
            }
            CONNECT => {
                ensure!(
                    self.ready,
                    "{}",
                    self.tr("Прокси ещё не готов", "Proxy is not ready yet")
                );
                open(self.config.link())?;
            }
            COPY => clipboard(self.hwnd, &self.config.link())?,
            RESTART => self.start()?,
            SETTINGS => self.show()?,
            LOGS => open(self.path.with_file_name("TgWsProxyPlus.log"))?,
            DIAGNOSE => {
                self.show()?;
                self.diagnose()?;
            }
            QUIT => {
                self.proxy.take();
                unsafe {
                    PostQuitMessage(0);
                }
            }
            _ => {}
        }
        self.publish();
        Ok(())
    }
    fn invoke(&mut self, request: Request) -> Result<serde_json::Value> {
        match request.command.as_str() {
            "state" => return Ok(self.snapshot()),
            "save" => {
                self.save(request.data)?;
                return Ok(self.snapshot());
            }
            "telegram" => self.command(OPEN)?,
            "connect" => self.command(CONNECT)?,
            "copy" => self.command(COPY)?,
            "logs" => self.command(LOGS)?,
            "restart" => self.command(RESTART)?,
            "diagnose" => self.diagnose()?,
            "updates" => self.check_updates(true)?,
            "copy_diagnostics" => clipboard(self.hwnd, &self.diagnostics)?,
            "secret" => return Ok(serde_json::json!(crypto::hex(&crypto::random::<16>()?))),
            "repository" => open("https://github.com/T1REI/TgWsProxyPlus")?,
            "hide" => unsafe {
                ShowWindow(self.hwnd, SW_HIDE);
            },
            "minimize" => unsafe {
                ShowWindow(self.hwnd, SW_MINIMIZE);
            },
            "maximize" => unsafe {
                ShowWindow(
                    self.hwnd,
                    if IsZoomed(self.hwnd) != 0 {
                        SW_RESTORE
                    } else {
                        SW_MAXIMIZE
                    },
                );
            },
            "drag" => unsafe {
                windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
                let mut point: POINT = zeroed();
                GetCursorPos(&mut point);
                let position =
                    ((point.x as u32 & 0xffff) | ((point.y as u32 & 0xffff) << 16)) as isize;
                PostMessageW(self.hwnd, WM_NCLBUTTONDOWN, HTCAPTION as usize, position);
            },
            _ => bail!("unsupported UI command"),
        }
        self.publish();
        Ok(serde_json::Value::Null)
    }

    fn check_updates(&mut self, manual: bool) -> Result<()> {
        ensure!(!review_build(), "Updates are disabled in this review build");
        if self.update_busy.replace(true) {
            return Ok(());
        }
        let tx = self.sender.clone();
        let hwnd = self.hwnd as usize;
        let result = thread::Builder::new()
            .name("updates".into())
            .spawn(move || {
                let result = (|| -> Result<updates::Release> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    let network = Network::new(0)?;
                    runtime.block_on(updates::check(&network))
                })();
                Self::post(
                    &tx,
                    hwnd,
                    Event::UpdateChecked(result.map_err(|error| format!("{error:#}")), manual),
                );
            });
        if let Err(error) = result {
            self.update_busy.set(false);
            return Err(error.into());
        }
        self.update_status = self
            .tr("Проверяем обновления…", "Checking for updates…")
            .into();
        Ok(())
    }
    fn download_update(&mut self, release: updates::Release) -> Result<()> {
        let asset = updates::select_asset(&release)?.clone();
        let digest = asset.digest.clone().context("missing update digest")?;
        let directory = std::env::current_exe()?
            .parent()
            .context("executable has no parent")?
            .to_path_buf();
        if self.update_busy.replace(true) {
            return Ok(());
        }
        let tx = self.sender.clone();
        let hwnd = self.hwnd as usize;
        let spawned = thread::Builder::new()
            .name("update-download".into())
            .spawn(move || {
                let result = (|| -> Result<tg_ws_proxy_plus::update_install::Prepared> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    let network = Network::new(0)?;
                    let path = runtime.block_on(updates::download(&network, &asset, &directory))?;
                    Ok(tg_ws_proxy_plus::update_install::Prepared {
                        path,
                        digest,
                        version: release.tag_name,
                    })
                })();
                Self::post(
                    &tx,
                    hwnd,
                    Event::Install(result.map_err(|e| format!("{e:#}"))),
                );
            });
        if let Err(error) = spawned {
            self.update_busy.set(false);
            return Err(error.into());
        }
        self.update_status = self
            .tr(
                "Загрузка и проверка обновления…",
                "Downloading and verifying update…",
            )
            .into();
        Ok(())
    }
    fn events(&mut self) {
        self.report_webview_error();
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Event::Status(generation, address) if generation == self.generation => {
                    self.ready = true;
                    self.status = address;
                    if let Some(path) = self.update_ready.take()
                        && let Err(error) = tg_ws_proxy_plus::update_install::confirm_ready(&path)
                    {
                        log::error!("cannot confirm update startup: {error:#}");
                        unsafe {
                            PostQuitMessage(1);
                        }
                    }
                }
                Event::Error(generation, error) if generation == self.generation => {
                    self.ready = false;
                    self.status = error.clone();
                    log::error!("{error}");
                }
                Event::Diagnostics(result) => {
                    self.diagnosing = false;
                    self.diagnostics = result.unwrap_or_else(|error| error);
                }
                Event::UpdateChecked(result, manual) => {
                    self.update_busy.set(false);
                    match result {
                        Ok(release) => {
                            self.update_status =
                                self.tr("Проверка завершена", "Check complete").into();
                            let is_new =
                                updates::newer(&release.tag_name, tg_ws_proxy_plus::VERSION)
                                    .unwrap_or(false);
                            if is_new
                                && (manual
                                    || self.prompted_version.as_ref() != Some(&release.tag_name))
                            {
                                self.prompted_version = Some(release.tag_name.clone());
                                if message(
                                    self.hwnd,
                                    &format!(
                                        "{} {}\n{}",
                                        self.tr("Доступна версия", "Version available:"),
                                        release.tag_name,
                                        self.tr(
                                            "Скачать и установить обновление?",
                                            "Download and install the update?"
                                        )
                                    ),
                                    "TgWsProxy+",
                                    MB_YESNO | MB_ICONINFORMATION,
                                ) == IDYES
                                    && let Err(error) = self.download_update(release)
                                {
                                    show_error(&format!("{error:#}"));
                                }
                            } else if manual && !is_new {
                                message(
                                    self.hwnd,
                                    self.tr(
                                        "Установлена актуальная версия.",
                                        "The application is up to date.",
                                    ),
                                    "TgWsProxy+",
                                    MB_OK | MB_ICONINFORMATION,
                                );
                            }
                        }
                        Err(error) => {
                            self.update_status = error.clone();
                            log::warn!("update check failed: {error}");
                            if manual {
                                message(self.hwnd, &error, "TgWsProxy+", MB_OK | MB_ICONERROR);
                            }
                        }
                    }
                }
                Event::Install(result) => {
                    self.update_busy.set(false);
                    let result = result.map_err(anyhow::Error::msg).and_then(|prepared| {
                        tg_ws_proxy_plus::update_install::handoff(prepared, &self.path)
                    });
                    match result {
                        Ok(()) => {
                            self.proxy.take();
                            unsafe {
                                PostQuitMessage(0);
                            }
                        }
                        Err(error) => {
                            log::error!("update installation failed: {error:#}");
                            message(
                                self.hwnd,
                                &format!("{error:#}"),
                                "TgWsProxy+",
                                MB_OK | MB_ICONERROR,
                            );
                        }
                    }
                }
                Event::Command(json) => match serde_json::from_str::<Request>(&json) {
                    Ok(request) => {
                        let id = request.id;
                        let response = match self.invoke(request) {
                            Ok(data) => serde_json::json!({ "id": id, "ok": true, "data": data }),
                            Err(error) => {
                                serde_json::json!({ "id": id, "ok": false, "error": format!("{error:#}") })
                            }
                        };
                        if let webview::State::Ready(view) = &*self.webview.borrow()
                            && let Err(error) = view.send(&response)
                        {
                            log::warn!("UI response: {error:#}");
                        }
                    }
                    Err(error) => log::warn!("Invalid UI request: {error}"),
                },
                _ => {}
            }
        }
        self.publish();
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_CLOSE || (msg == WM_SYSCOMMAND && w & 0xfff0 == SC_CLOSE as usize) {
            ShowWindow(hwnd, SW_HIDE);
            return 0;
        }
        if msg == WM_NCCALCSIZE {
            return 0;
        }
        if msg == WM_NCCREATE {
            let creation = &*(l as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, creation.lpCreateParams as isize);
        }
        if msg == WM_DESTROY {
            PostQuitMessage(0);
            return 0;
        }
        if msg == WM_GETMINMAXINFO {
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            let info = &mut *(l as *mut MINMAXINFO);
            info.ptMinTrackSize = POINT {
                x: 580 * dpi / 96,
                y: 460 * dpi / 96,
            };
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut monitor_info: MONITORINFO = zeroed();
            monitor_info.cbSize = size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(monitor, &mut monitor_info) != 0 {
                info.ptMaxPosition = POINT {
                    x: monitor_info.rcWork.left - monitor_info.rcMonitor.left,
                    y: monitor_info.rcWork.top - monitor_info.rcMonitor.top,
                };
                info.ptMaxSize = POINT {
                    x: monitor_info.rcWork.right - monitor_info.rcWork.left,
                    y: monitor_info.rcWork.bottom - monitor_info.rcWork.top,
                };
            }
            return 0;
        }
        if msg == WM_NCHITTEST && IsZoomed(hwnd) == 0 {
            let mut rect: RECT = zeroed();
            GetWindowRect(hwnd, &mut rect);
            let x = (l as u32 & 0xffff) as i16 as i32;
            let y = ((l as u32 >> 16) & 0xffff) as i16 as i32;
            let border = 6 * GetDpiForWindow(hwnd).max(96) as i32 / 96;
            let left = x < rect.left + border;
            let right = x >= rect.right - border;
            let top = y < rect.top + border;
            let bottom = y >= rect.bottom - border;
            let hit = match (left, right, top, bottom) {
                (true, _, true, _) => HTTOPLEFT,
                (_, true, true, _) => HTTOPRIGHT,
                (true, _, _, true) => HTBOTTOMLEFT,
                (_, true, _, true) => HTBOTTOMRIGHT,
                (true, _, _, _) => HTLEFT,
                (_, true, _, _) => HTRIGHT,
                (_, _, true, _) => HTTOP,
                (_, _, _, true) => HTBOTTOM,
                _ => HTCLIENT,
            };
            return hit as isize;
        }
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<Ui>;
        if pointer.is_null() {
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let Ok(mut ui) = (*pointer).try_borrow_mut() else {
            return DefWindowProcW(hwnd, msg, w, l);
        };
        match msg {
            WM_TIMER => {
                if w == 1002
                    && !review_build()
                    && ui.config.check_updates
                    && let Err(error) = ui.check_updates(false)
                {
                    log::warn!("update worker: {error:#}");
                }
                ui.publish();
                return 0;
            }
            WM_QUERYENDSESSION => return 1,
            WM_ENDSESSION if w != 0 => {
                ui.proxy.take();
                PostQuitMessage(0);
                return 0;
            }
            WM_SIZE => {
                if let webview::State::Ready(view) = &*ui.webview.borrow() {
                    let _ = view.resize(hwnd as usize);
                    let _ = view.visible(w != SIZE_MINIMIZED as usize);
                }
                return 0;
            }
            WM_SHOWWINDOW => {
                if let webview::State::Ready(view) = &*ui.webview.borrow() {
                    let _ = view.visible(w != 0);
                }
            }
            WM_DPICHANGED => {
                let rect = &*(l as *const RECT);
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                return 0;
            }
            WM_COMMAND if w >> 16 == 0 => {
                if let Err(error) = ui.command(w & 0xffff) {
                    message(
                        hwnd,
                        &format!("{error:#}"),
                        "TgWsProxy+",
                        MB_OK | MB_ICONERROR,
                    );
                }
                return 0;
            }
            TRAY => {
                if l as u32 == WM_RBUTTONUP || l as u32 == WM_CONTEXTMENU {
                    let mut point = POINT { x: 0, y: 0 };
                    GetCursorPos(&mut point);
                    SetForegroundWindow(hwnd);
                    let id = TrackPopupMenu(
                        ui.menu,
                        TPM_RETURNCMD | TPM_RIGHTBUTTON,
                        point.x,
                        point.y,
                        0,
                        hwnd,
                        null(),
                    );
                    if id != 0
                        && let Err(error) = ui.command(id as usize)
                    {
                        message(
                            hwnd,
                            &format!("{error:#}"),
                            "TgWsProxy+",
                            MB_OK | MB_ICONERROR,
                        );
                    }
                    PostMessageW(hwnd, WM_NULL, 0, 0);
                } else if l as u32 == WM_LBUTTONUP
                    && let Err(error) = ui.command(OPEN)
                {
                    message(
                        hwnd,
                        &format!("{error:#}"),
                        "TgWsProxy+",
                        MB_OK | MB_ICONERROR,
                    );
                }
                return 0;
            }
            EVENT => {
                ui.events();
                return 0;
            }
            _ => {}
        }
        drop(ui);
        DefWindowProcW(hwnd, msg, w, l)
    }
}

pub fn run(
    config: Config,
    path: PathBuf,
    first_run: bool,
    update_ready: Option<PathBuf>,
) -> Result<()> {
    let marker = path.with_file_name(".first-run-done");
    let first_run = first_run || !marker.exists();
    unsafe {
        let lock = CreateMutexW(null(), 0, wide("Local\\TgWsProxyPlus").as_ptr());
        ensure!(!lock.is_null(), "cannot acquire single-instance mutex");
        struct Instance(HANDLE);
        impl Drop for Instance {
            fn drop(&mut self) {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
        let _instance = Instance(lock);
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let hwnd = FindWindowW(wide(CLASS).as_ptr(), null());
            if !hwnd.is_null() {
                PostMessageW(hwnd, WM_COMMAND, SETTINGS, 0);
            }
            return Ok(());
        }
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        )
        .ok()?;
        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                unsafe {
                    windows::Win32::System::Com::CoUninitialize();
                }
            }
        }
        let _com = Com;
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let module = GetModuleHandleW(null());
        let icon = LoadImageW(
            module,
            std::ptr::without_provenance::<u16>(1),
            IMAGE_ICON,
            GetSystemMetrics(SM_CXICON),
            GetSystemMetrics(SM_CYICON),
            LR_DEFAULTCOLOR,
        ) as HICON;
        ensure!(!icon.is_null(), "cannot load application icon resource");
        let brush = CreateSolidBrush(0x001d190d);
        let class = wide(CLASS);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: module,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hIcon: icon,
            hbrBackground: brush,
            lpszClassName: class.as_ptr(),
            ..zeroed()
        };
        ensure!(
            RegisterClassW(&wc) != 0,
            "cannot register application window"
        );
        let ui = Box::new(RefCell::new(Ui::new(config, path, update_ready)));
        let dpi = GetDpiForSystem().max(96) as i32;
        let mut work: RECT = zeroed();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut work as *mut RECT).cast(), 0);
        let width = (660 * dpi / 96).min((work.right - work.left - 32 * dpi / 96).max(580));
        let height = (520 * dpi / 96).min((work.bottom - work.top - 32 * dpi / 96).max(460));
        let hwnd = CreateWindowExW(
            WS_EX_APPWINDOW,
            class.as_ptr(),
            wide("TgWsProxy+").as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            work.left + (work.right - work.left - width) / 2,
            work.top + (work.bottom - work.top - height) / 2,
            width,
            height,
            null_mut(),
            null_mut(),
            module,
            (&*ui as *const RefCell<Ui>).cast(),
        );
        ensure!(!hwnd.is_null(), "cannot create application window");
        let dark_title: BOOL = 1;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            (&dark_title as *const BOOL).cast(),
            size_of::<BOOL>() as u32,
        );
        {
            let mut state = ui.borrow_mut();
            state.hwnd = hwnd;
            state.icon = icon;
            state.brush = brush;
            state.start()?;
            state.rebuild_menu()?;
        }
        let mut tray: NOTIFYICONDATAW = zeroed();
        tray.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        tray.hWnd = hwnd;
        tray.uID = 1;
        tray.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        tray.uCallbackMessage = TRAY;
        tray.hIcon = icon;
        let tip = wide("TgWsProxy+ · ЛКМ: Telegram · ПКМ: меню");
        tray.szTip[..tip.len()].copy_from_slice(&tip);
        ensure!(
            Shell_NotifyIconW(NIM_ADD, &tray) != 0,
            "cannot create tray icon"
        );
        let taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        if first_run {
            ui.borrow_mut().show()?;
            std::fs::write(marker, b"TgWsProxyPlus\n")?;
        }
        SetTimer(hwnd, 1001, 1000, None);
        if !review_build() {
            SetTimer(hwnd, 1002, 6 * 60 * 60 * 1000, None);
            let should_check = ui.borrow().config.check_updates;
            if should_check && let Err(error) = ui.borrow_mut().check_updates(false) {
                log::warn!("update worker: {error:#}");
            }
        }
        let mut msg: MSG = zeroed();
        loop {
            let result = GetMessageW(&mut msg, null_mut(), 0, 0);
            if result <= 0 {
                break;
            }
            if msg.message == taskbar_created {
                Shell_NotifyIconW(NIM_ADD, &tray);
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        KillTimer(hwnd, 1001);
        KillTimer(hwnd, 1002);
        ui.borrow_mut().proxy.take();
        *ui.borrow().webview.borrow_mut() = webview::State::Empty;
        Shell_NotifyIconW(NIM_DELETE, &tray);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        DestroyWindow(hwnd);
        DestroyMenu(ui.borrow().menu);
        DestroyIcon(icon);
        DeleteObject(brush);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_settings_hides_window_even_during_reentrant_dispatch() {
        unsafe {
            let class = wide(format!("TgWsProxyPlusCloseTest{}", std::process::id()));
            let module = GetModuleHandleW(null());
            let wc = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: module,
                lpszClassName: class.as_ptr(),
                ..zeroed()
            };
            assert_ne!(RegisterClassW(&wc), 0);
            let ui = RefCell::new(Ui::new(Config::fresh().unwrap(), PathBuf::new(), None));
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                -32000,
                -32000,
                100,
                100,
                null_mut(),
                null_mut(),
                module,
                (&ui as *const RefCell<Ui>).cast(),
            );
            assert!(!hwnd.is_null());
            ui.borrow_mut().hwnd = hwnd;
            let opens = Rc::new(Cell::new(0));
            ui.borrow_mut().telegram = telegram::Launcher::recording(opens.clone());
            for borrowed in [false, true] {
                for (message, parameter) in [(WM_SYSCOMMAND, SC_CLOSE as usize), (WM_CLOSE, 0)] {
                    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    assert_ne!(IsWindowVisible(hwnd), 0);
                    let held = borrowed.then(|| ui.borrow_mut());
                    SendMessageW(hwnd, message, parameter, 0);
                    assert_ne!(IsWindow(hwnd), 0);
                    assert_eq!(IsWindowVisible(hwnd), 0);
                    drop(held);
                    let mut msg: MSG = zeroed();
                    while PeekMessageW(&mut msg, hwnd, 0, 0, PM_REMOVE) != 0 {
                        DispatchMessageW(&msg);
                    }
                }
            }
            SendMessageW(hwnd, TRAY, 0, WM_LBUTTONUP as isize);
            assert_eq!(opens.get(), 1, "single tray click must open Telegram");
            SendMessageW(hwnd, TRAY, 0, WM_LBUTTONDBLCLK as isize);
            SendMessageW(hwnd, TRAY, 0, WM_LBUTTONUP as isize);
            assert_eq!(opens.get(), 1, "double click must not open Telegram twice");
            assert_eq!(
                IsWindowVisible(hwnd),
                0,
                "tray click must not reveal settings"
            );
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DestroyWindow(hwnd);
            let mut msg: MSG = zeroed();
            PeekMessageW(&mut msg, null_mut(), WM_QUIT, WM_QUIT, PM_REMOVE);
            UnregisterClassW(class.as_ptr(), module);
        }
    }
}
