use anyhow::{Context, Result, bail, ensure};
use std::{
    cell::{Cell, RefCell},
    ffi::OsStr,
    mem::{size_of, zeroed},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::mpsc,
    thread,
};
use tg_ws_proxy_plus::{
    config::{Config, normalize_domains},
    crypto, logging,
    network::Network,
    routing::Routing,
    server,
    stats::Stats,
    updates,
};
use windows_sys::Win32::{
    Foundation::*,
    Globalization::*,
    Graphics::{Dwm::*, Gdi::*},
    System::{Console::*, DataExchange::*, LibraryLoader::*, Memory::*, Registry::*, Threading::*},
    UI::{Controls::*, HiDpi::*, Shell::*, WindowsAndMessaging::*},
};
use windows_sys::core::BOOL;

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
const UPDATES: usize = 8;
const ABOUT: usize = 9;
const SAVE: usize = 10;
const CANCEL: usize = 11;
const GENERATE: usize = 12;
const PAGE: usize = 20;

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

fn text(hwnd: HWND) -> String {
    unsafe {
        let length = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut data = vec![0; length + 1];
        let count = GetWindowTextW(hwnd, data.as_mut_ptr(), data.len() as i32).max(0) as usize;
        String::from_utf16_lossy(&data[..count])
    }
}

fn set_text(hwnd: HWND, value: &str) {
    unsafe {
        SetWindowTextW(hwnd, wide(value).as_ptr());
    }
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
    Error(String),
    Status(String),
    Message(String),
    UpdateChecked(std::result::Result<updates::Release, String>, bool),
    Install(std::result::Result<tg_ws_proxy_plus::update_install::Prepared, String>),
}
enum Kind {
    Text,
    Number,
    Decimal,
    Check,
    Lines,
    Domains,
    Choice,
}
struct Field {
    key: &'static str,
    handle: HWND,
    page: usize,
    kind: Kind,
}

struct Ui {
    hwnd: HWND,
    config: Config,
    path: PathBuf,
    proxy: Option<ProxyThread>,
    fields: Vec<Field>,
    labels: Vec<(HWND, usize)>,
    status: HWND,
    sender: mpsc::Sender<Event>,
    receiver: mpsc::Receiver<Event>,
    menu: HMENU,
    icon: HICON,
    brush: HBRUSH,
    dark: bool,
    page: usize,
    dpi: i32,
    font: HFONT,
    update_busy: Cell<bool>,
    update_ready: Option<PathBuf>,
    prompted_version: Option<String>,
}

impl Ui {
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
            unsafe {
                PostMessageW(hwnd as HWND, EVENT, 0, 0);
            }
        }
    }

    fn start(&mut self) -> Result<()> {
        self.proxy.take();
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
                        Event::Status(format!("{}:{}", routing.config.host, routing.config.port)),
                    );
                    server::serve(listener, routing, async {
                        let _ = stopped.await;
                    })
                    .await
                })
            })();
            if let Err(error) = result {
                Self::post(&tx, hwnd, Event::Error(format!("{error:#}")));
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

    fn control(
        &self,
        class: &str,
        label: &str,
        style: u32,
        bounds: [i32; 4],
        id: usize,
    ) -> Result<HWND> {
        let [x, y, width, height] = bounds;
        unsafe {
            let hwnd = CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(label).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                x * self.dpi / 96,
                y * self.dpi / 96,
                width * self.dpi / 96,
                height * self.dpi / 96,
                self.hwnd,
                id as HMENU,
                GetModuleHandleW(null()),
                null(),
            );
            ensure!(!hwnd.is_null(), "cannot create native control");
            SendMessageW(hwnd, WM_SETFONT, self.font as usize, 1);
            if self.dark {
                SetWindowTheme(hwnd, wide("DarkMode_Explorer").as_ptr(), null());
            }
            Ok(hwnd)
        }
    }

    fn field(
        &mut self,
        key: &'static str,
        ru: &str,
        en: &str,
        page: usize,
        row: i32,
        kind: Kind,
    ) -> Result<()> {
        let y = 82 + row * 45;
        let label = self.control("STATIC", self.tr(ru, en), 0, [24, y + 4, 240, 26], 0)?;
        self.labels.push((label, page));
        let value = serde_json::to_value(&self.config)?;
        let value = &value[key];
        let (class, style, height) = match kind {
            Kind::Check => ("BUTTON", BS_AUTOCHECKBOX as u32 | WS_TABSTOP, 26),
            Kind::Choice => (
                "COMBOBOX",
                CBS_DROPDOWNLIST as u32 | WS_TABSTOP | WS_VSCROLL,
                150,
            ),
            Kind::Lines => (
                "EDIT",
                WS_BORDER | WS_TABSTOP | ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | WS_VSCROLL,
                62,
            ),
            _ => ("EDIT", WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32, 27),
        };
        let initial = match kind {
            Kind::Lines | Kind::Domains => value
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(if matches!(kind, Kind::Lines) {
                            "\r\n"
                        } else {
                            ", "
                        })
                })
                .unwrap_or_default(),
            Kind::Check | Kind::Choice => String::new(),
            _ => value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string()),
        };
        let handle = self.control(
            class,
            &initial,
            style,
            [270, y, 395, height],
            100 + self.fields.len(),
        )?;
        unsafe {
            if matches!(kind, Kind::Check) {
                SendMessageW(
                    handle,
                    BM_SETCHECK,
                    if value.as_bool().unwrap_or(false) {
                        BST_CHECKED
                    } else {
                        BST_UNCHECKED
                    } as usize,
                    0,
                );
            } else if matches!(kind, Kind::Choice) {
                let choices: &[&str] = if key == "language" {
                    &["ru", "en", "auto"]
                } else {
                    &["auto", "dark", "light"]
                };
                for choice in choices {
                    SendMessageW(handle, CB_ADDSTRING, 0, wide(choice).as_ptr() as isize);
                }
                SendMessageW(
                    handle,
                    CB_SETCURSEL,
                    choices
                        .iter()
                        .position(|s| Some(*s) == value.as_str())
                        .unwrap_or(0),
                    0,
                );
            } else {
                SendMessageW(handle, EM_SETLIMITTEXT, 65536, 0);
            }
        }
        self.fields.push(Field {
            key,
            handle,
            page,
            kind,
        });
        Ok(())
    }

    fn build(&mut self) -> Result<()> {
        self.control(
            "STATIC",
            "TgWsProxy+  /  TgWsProxyPlus     •     T1REI",
            0,
            [24, 16, 640, 25],
            0,
        )?;
        for (index, label) in [
            self.tr("Соединение", "Connection"),
            self.tr("Маршруты", "Routes"),
            self.tr("Дополнительно", "Advanced"),
            self.tr("Быстродействие", "Performance"),
        ]
        .into_iter()
        .enumerate()
        {
            self.control(
                "BUTTON",
                label,
                WS_TABSTOP,
                [24 + index as i32 * 162, 45, 154, 28],
                PAGE + index,
            )?;
        }
        self.field(
            "host",
            "Адрес прослушивания",
            "Listen address",
            0,
            0,
            Kind::Text,
        )?;
        self.field("port", "Порт", "Port", 0, 1, Kind::Number)?;
        self.field(
            "secret",
            "Секрет (32 hex-символа)",
            "Secret (32 hex characters)",
            0,
            2,
            Kind::Text,
        )?;
        self.control(
            "BUTTON",
            self.tr("Новый секрет", "New secret"),
            WS_TABSTOP,
            [270, 220, 190, 28],
            GENERATE,
        )?;
        self.field(
            "fake_tls_domain",
            "Домен Fake TLS (необязательно)",
            "Fake TLS domain (optional)",
            0,
            4,
            Kind::Text,
        )?;
        self.field(
            "proxy_protocol",
            "Заголовок PROXY v1",
            "PROXY v1 header",
            0,
            5,
            Kind::Check,
        )?;
        self.field(
            "autostart",
            "Автозапуск при входе в Windows",
            "Start at Windows sign-in",
            0,
            6,
            Kind::Check,
        )?;
        let instructions = self.control(
            "STATIC",
            self.tr(
                "1. Сохраните настройки.\r\n2. Нажмите «Открыть Telegram» и включите прокси.\r\n\r\nЗакрытие окна оставляет приложение в трее.\r\nПолный выход: меню значка в трее → «Выход».",
                "1. Save your settings.\r\n2. Click Open Telegram and enable the proxy.\r\n\r\nClosing this window keeps the app in the tray.\r\nTo quit: right-click the tray icon → Exit.",
            ),
            0, [24, 420, 640, 140], 0,
        )?;
        self.labels.push((instructions, 0));
        self.field(
            "dc_ip",
            "DC → IP (по одной строке)",
            "DC → IP (one per line)",
            1,
            0,
            Kind::Lines,
        )?;
        self.field(
            "pool_size",
            "Пул WebSocket на DC",
            "WebSocket pool per DC",
            1,
            2,
            Kind::Number,
        )?;
        self.field(
            "cfproxy",
            "Cloudflare fallback",
            "Cloudflare fallback",
            1,
            3,
            Kind::Check,
        )?;
        self.field(
            "cfproxy_user_domain_enabled",
            "Собственные CF-домены",
            "Use custom CF domains",
            1,
            4,
            Kind::Check,
        )?;
        self.field(
            "cfproxy_user_domain",
            "CF-домены через запятую",
            "CF domains, comma separated",
            1,
            5,
            Kind::Domains,
        )?;
        self.field(
            "cfproxy_worker_enabled",
            "Cloudflare Worker",
            "Cloudflare Worker",
            1,
            6,
            Kind::Check,
        )?;
        self.field(
            "cfproxy_worker_domain",
            "Worker-домены через запятую",
            "Worker domains, comma separated",
            1,
            7,
            Kind::Domains,
        )?;
        self.field(
            "h2",
            "HTTP/2 для медиа",
            "HTTP/2 for media",
            1,
            8,
            Kind::Check,
        )?;
        self.field(
            "no_secure",
            "HTTP вместо HTTPS для CF",
            "HTTP instead of HTTPS for CF",
            1,
            9,
            Kind::Check,
        )?;
        self.field(
            "force_test_dc",
            "Только тестовые DC Telegram",
            "Only Telegram test DCs",
            1,
            10,
            Kind::Check,
        )?;
        self.field(
            "buf_kb",
            "Буфер, КиБ (0 — авто)",
            "Buffer, KiB (0 = auto)",
            2,
            0,
            Kind::Number,
        )?;
        self.field(
            "max_connections",
            "Максимум подключений",
            "Maximum connections",
            2,
            1,
            Kind::Number,
        )?;
        self.field(
            "verbose",
            "Подробный журнал",
            "Verbose logging",
            2,
            2,
            Kind::Check,
        )?;
        self.field(
            "log_max_mb",
            "Размер журнала, МиБ",
            "Log size, MiB",
            2,
            3,
            Kind::Decimal,
        )?;
        self.field(
            "log_backups",
            "Число архивов журнала",
            "Log backups",
            2,
            4,
            Kind::Number,
        )?;
        self.field(
            "check_updates",
            "Проверять обновления",
            "Check for updates",
            2,
            5,
            Kind::Check,
        )?;
        self.field(
            "language",
            "Язык интерфейса",
            "Interface language",
            2,
            6,
            Kind::Choice,
        )?;
        self.field("appearance", "Оформление", "Appearance", 2, 7, Kind::Choice)?;
        self.field(
            "h2_max_connections",
            "H2-соединений на сервер",
            "H2 connections per server",
            3,
            0,
            Kind::Number,
        )?;
        self.field(
            "h2_stream_window_kb",
            "H2-окно потока, КиБ",
            "H2 stream window, KiB",
            3,
            1,
            Kind::Number,
        )?;
        self.field(
            "h2_connection_window_kb",
            "H2-окно соединения, КиБ",
            "H2 connection window, KiB",
            3,
            2,
            Kind::Number,
        )?;
        let note = self.control("STATIC", self.tr("Маршруты выбираются по времени подключения, скорости и ошибкам.\r\nПулы растут по спросу и сокращаются в простое.\r\n\r\nТекущие измерения доступны через «Диагностика» в меню трея.\r\nОбщий бюджет тел HTTP/2: 32 МиБ на приложение.", "Routes adapt to connection time, throughput and errors.\r\nPools grow with demand and shrink when idle.\r\n\r\nOpen Diagnostics from the tray menu for current measurements.\r\nHTTP/2 body budget: 32 MiB for the application."), 0, [24, 280, 640, 160], 0)?;
        self.labels.push((note, 3));
        self.status = self.control(
            "STATIC",
            self.tr("Прокси запускается…", "Proxy starting…"),
            0,
            [24, 590, 640, 25],
            0,
        )?;
        self.control(
            "BUTTON",
            self.tr("Открыть Telegram", "Open Telegram"),
            WS_TABSTOP,
            [24, 625, 155, 33],
            OPEN,
        )?;
        self.control(
            "BUTTON",
            self.tr("Копировать ссылку", "Copy link"),
            WS_TABSTOP,
            [190, 625, 155, 33],
            COPY,
        )?;
        self.control(
            "BUTTON",
            self.tr("Сохранить", "Save"),
            WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
            [390, 625, 130, 33],
            SAVE,
        )?;
        self.control(
            "BUTTON",
            self.tr("Закрыть", "Close"),
            WS_TABSTOP,
            [535, 625, 130, 33],
            CANCEL,
        )?;
        self.show_page(0);
        self.rebuild_menu()?;
        Ok(())
    }

    fn show_page(&mut self, page: usize) {
        self.page = page;
        unsafe {
            for field in &self.fields {
                ShowWindow(
                    field.handle,
                    if field.page == page { SW_SHOW } else { SW_HIDE },
                );
            }
            for &(label, owner) in &self.labels {
                ShowWindow(label, if owner == page { SW_SHOW } else { SW_HIDE });
            }
            let generate = GetDlgItem(self.hwnd, GENERATE as i32);
            ShowWindow(generate, if page == 0 { SW_SHOW } else { SW_HIDE });
            InvalidateRect(self.hwnd, null(), 1);
        }
    }

    fn rebuild_menu(&mut self) -> Result<()> {
        unsafe {
            if !self.menu.is_null() {
                DestroyMenu(self.menu);
            }
            self.menu = CreatePopupMenu();
            ensure!(!self.menu.is_null(), "cannot create tray menu");
            for (id, label) in [
                (OPEN, self.tr("Открыть в Telegram", "Open in Telegram")),
                (COPY, self.tr("Скопировать ссылку", "Copy link")),
                (RESTART, self.tr("Перезапустить прокси", "Restart proxy")),
                (SETTINGS, self.tr("Настройки…", "Settings…")),
                (
                    DIAGNOSE,
                    self.tr("Проверить подключения", "Test connectivity"),
                ),
                (LOGS, self.tr("Открыть журнал", "Open log")),
                (UPDATES, self.tr("Проверить обновления", "Check updates")),
                (ABOUT, self.tr("О программе", "About")),
                (QUIT, self.tr("Выход", "Exit")),
            ] {
                AppendMenuW(self.menu, MF_STRING, id, wide(label).as_ptr());
            }
        }
        Ok(())
    }

    fn save(&mut self) -> Result<()> {
        let mut value = serde_json::to_value(&self.config)?;
        for field in &self.fields {
            let input = text(field.handle);
            value[field.key] =
                match field.kind {
                    Kind::Text | Kind::Choice => serde_json::Value::String(input.trim().into()),
                    Kind::Number => serde_json::json!(input.trim().parse::<u64>().with_context(
                        || format!("{}: expected a nonnegative integer", field.key)
                    )?),
                    Kind::Decimal => serde_json::json!(
                        input
                            .trim()
                            .parse::<f64>()
                            .with_context(|| format!("{}: expected a number", field.key))?
                    ),
                    Kind::Check => serde_json::Value::Bool(
                        unsafe { SendMessageW(field.handle, BM_GETCHECK, 0, 0) }
                            == BST_CHECKED as isize,
                    ),
                    Kind::Lines => serde_json::json!(
                        input
                            .lines()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                    ),
                    Kind::Domains => serde_json::json!(normalize_domains(&[input])),
                };
        }
        let config: Config = serde_json::from_value(value)?;
        config.validate()?;
        if config.autostart != self.config.autostart {
            autostart(config.autostart, &self.path)?;
        }
        if let Err(error) = config.save(&self.path) {
            if config.autostart != self.config.autostart {
                let _ = autostart(self.config.autostart, &self.path);
            }
            return Err(error);
        }
        let interface_changed =
            config.language != self.config.language || config.appearance != self.config.appearance;
        self.config = config;
        logging::configure(
            Some(&self.path.with_file_name("TgWsProxyPlus.log")),
            self.config.log_max_mb,
            self.config.log_backups,
            self.config.verbose,
            false,
        )?;
        self.start()?;
        if interface_changed {
            message(
                self.hwnd,
                self.tr(
                    "Язык и оформление изменятся после перезапуска приложения.",
                    "Language and appearance will change after restarting the application.",
                ),
                "TgWsProxy+",
                MB_OK,
            );
        }
        unsafe {
            ShowWindow(self.hwnd, SW_HIDE);
        }
        Ok(())
    }

    fn background(&self, update: bool, manual: bool) -> Result<()> {
        if update && self.update_busy.replace(true) {
            return Ok(());
        }
        let config = self.config.clone();
        let live = self
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
        let spawned = thread::Builder::new()
            .name(if update { "updates" } else { "diagnostics" }.into())
            .spawn(move || {
                let result = (|| -> Result<Event> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(async {
                        if update {
                            let network = Network::new(0)?;
                            Ok(Event::UpdateChecked(
                                updates::check(&network).await.map_err(|e| format!("{e:#}")),
                                manual,
                            ))
                        } else {
                            let mut report = live;
                            report.push("--- Connection probes ---".into());
                            report.extend(server::diagnose(config).await?);
                            Ok(Event::Message(report.join("\n")))
                        }
                    })
                })();
                let event = match result {
                    Ok(event) => event,
                    Err(error) if update => Event::UpdateChecked(Err(format!("{error:#}")), manual),
                    Err(error) => Event::Error(format!("{error:#}")),
                };
                Self::post(&tx, hwnd, event);
            });
        if let Err(error) = spawned {
            if update {
                self.update_busy.set(false);
            }
            return Err(error.into());
        }
        Ok(())
    }

    fn command(&mut self, command: usize) -> Result<()> {
        match command {
            OPEN => open(self.config.link())?,
            COPY => clipboard(self.hwnd, &self.config.link())?,
            RESTART => self.start()?,
            LOGS => open(self.path.with_file_name("TgWsProxyPlus.log"))?,
            DIAGNOSE => self.background(false, true)?,
            UPDATES => self.background(true, true)?,
            SAVE => self.save()?,
            GENERATE => {
                if let Some(field) = self.fields.iter().find(|f| f.key == "secret") {
                    set_text(field.handle, &crypto::hex(&crypto::random::<16>()?));
                }
            }
            SETTINGS => unsafe {
                ShowWindow(self.hwnd, SW_RESTORE);
                SetForegroundWindow(self.hwnd);
            },
            CANCEL => {
                let current = serde_json::to_value(&self.config)?;
                for field in &self.fields {
                    let value = &current[field.key];
                    match field.kind {
                        Kind::Check => unsafe {
                            SendMessageW(
                                field.handle,
                                BM_SETCHECK,
                                if value.as_bool().unwrap_or(false) {
                                    BST_CHECKED
                                } else {
                                    BST_UNCHECKED
                                } as usize,
                                0,
                            );
                        },
                        Kind::Choice => {
                            let choices: &[&str] = if field.key == "language" {
                                &["ru", "en", "auto"]
                            } else {
                                &["auto", "dark", "light"]
                            };
                            unsafe {
                                SendMessageW(
                                    field.handle,
                                    CB_SETCURSEL,
                                    choices
                                        .iter()
                                        .position(|s| Some(*s) == value.as_str())
                                        .unwrap_or(0),
                                    0,
                                );
                            }
                        }
                        Kind::Lines | Kind::Domains => set_text(
                            field.handle,
                            &value
                                .as_array()
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|v| v.as_str())
                                        .collect::<Vec<_>>()
                                        .join(if matches!(field.kind, Kind::Lines) {
                                            "\r\n"
                                        } else {
                                            ", "
                                        })
                                })
                                .unwrap_or_default(),
                        ),
                        _ => set_text(
                            field.handle,
                            &value
                                .as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| value.to_string()),
                        ),
                    }
                }
                unsafe {
                    ShowWindow(self.hwnd, SW_HIDE);
                }
            }
            ABOUT => {
                message(
                    self.hwnd,
                    &format!(
                        "TgWsProxy+ / TgWsProxyPlus {}\nАвтор Rust-версии / Rust author: T1REI\n\nBased on Flowseal/tg-ws-proxy\nMIT License © 2026 Flowseal; © 2026 T1REI",
                        tg_ws_proxy_plus::VERSION
                    ),
                    self.tr("О программе", "About"),
                    MB_OK | MB_ICONINFORMATION,
                );
            }
            QUIT => {
                self.proxy.take();
                unsafe {
                    PostQuitMessage(0);
                }
            }
            id if (PAGE..PAGE + 4).contains(&id) => self.show_page(id - PAGE),
            _ => {}
        }
        Ok(())
    }

    fn download_update(&self, release: updates::Release) -> Result<()> {
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
        set_text(
            self.status,
            self.tr(
                "Загрузка и проверка обновления…",
                "Downloading and verifying update…",
            ),
        );
        Ok(())
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_CLOSE || (msg == WM_SYSCOMMAND && w & 0xfff0 == SC_CLOSE as usize) {
            ShowWindow(hwnd, SW_HIDE);
            PostMessageW(hwnd, WM_COMMAND, CANCEL, 0);
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
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<Ui>;
        if pointer.is_null() {
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let Ok(mut ui) = (*pointer).try_borrow_mut() else {
            return DefWindowProcW(hwnd, msg, w, l);
        };
        match msg {
            WM_TIMER if w == 1001 => {
                if ui.config.check_updates
                    && let Err(error) = ui.background(true, false)
                {
                    log::warn!("update worker: {error:#}");
                }
                return 0;
            }
            WM_QUERYENDSESSION => return 1,
            WM_ENDSESSION if w != 0 => {
                ui.proxy.take();
                PostQuitMessage(0);
                return 0;
            }
            WM_COMMAND => {
                if w >> 16 == 0 {
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
                } else if l as u32 == WM_LBUTTONDBLCLK {
                    ShowWindow(hwnd, SW_RESTORE);
                    SetForegroundWindow(hwnd);
                }
                return 0;
            }
            EVENT => {
                while let Ok(event) = ui.receiver.try_recv() {
                    match event {
                        Event::Error(error) => {
                            set_text(
                                ui.status,
                                ui.tr("Ошибка — подробности в журнале", "Error — see log"),
                            );
                            log::error!("{error}");
                            message(hwnd, &error, "TgWsProxy+", MB_OK | MB_ICONERROR);
                        }
                        Event::Status(status) => {
                            set_text(
                                ui.status,
                                &format!(
                                    "{}  {status}",
                                    ui.tr("Прокси работает:", "Proxy listening:")
                                ),
                            );
                            if let Some(path) = ui.update_ready.take()
                                && let Err(error) =
                                    tg_ws_proxy_plus::update_install::confirm_ready(&path)
                            {
                                log::error!("cannot confirm update startup: {error:#}");
                                PostQuitMessage(1);
                            }
                        }
                        Event::Message(text) => {
                            if !text.is_empty() {
                                message(hwnd, &text, "TgWsProxy+", MB_OK | MB_ICONINFORMATION);
                            }
                        }
                        Event::UpdateChecked(result, manual) => {
                            ui.update_busy.set(false);
                            match result {
                                Ok(release) => {
                                    let is_new = updates::newer(
                                        &release.tag_name,
                                        tg_ws_proxy_plus::VERSION,
                                    )
                                    .unwrap_or(false);
                                    if is_new
                                        && (manual
                                            || ui.prompted_version.as_ref()
                                                != Some(&release.tag_name))
                                    {
                                        ui.prompted_version = Some(release.tag_name.clone());
                                        if message(
                                            hwnd,
                                            &format!(
                                                "{} {}\n{}",
                                                ui.tr("Доступна версия", "Version available:"),
                                                release.tag_name,
                                                ui.tr(
                                                    "Скачать и установить обновление?",
                                                    "Download and install the update?"
                                                )
                                            ),
                                            "TgWsProxy+",
                                            MB_YESNO | MB_ICONINFORMATION,
                                        ) == IDYES
                                            && let Err(error) = ui.download_update(release)
                                        {
                                            show_error(&format!("{error:#}"));
                                        }
                                    } else if manual && !is_new {
                                        message(
                                            hwnd,
                                            ui.tr(
                                                "Установлена актуальная версия.",
                                                "The application is up to date.",
                                            ),
                                            "TgWsProxy+",
                                            MB_OK | MB_ICONINFORMATION,
                                        );
                                    }
                                }
                                Err(error) => {
                                    log::warn!("update check failed: {error}");
                                    if manual {
                                        message(hwnd, &error, "TgWsProxy+", MB_OK | MB_ICONERROR);
                                    }
                                }
                            }
                        }
                        Event::Install(result) => {
                            ui.update_busy.set(false);
                            let result = result.map_err(anyhow::Error::msg).and_then(|prepared| {
                                tg_ws_proxy_plus::update_install::handoff(prepared, &ui.path)
                            });
                            match result {
                                Ok(()) => {
                                    ui.proxy.take();
                                    PostQuitMessage(0);
                                }
                                Err(error) => {
                                    log::error!("update installation failed: {error:#}");
                                    message(
                                        hwnd,
                                        &format!("{error:#}"),
                                        "TgWsProxy+",
                                        MB_OK | MB_ICONERROR,
                                    );
                                }
                            }
                        }
                    }
                }
                return 0;
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
                let hdc = w as HDC;
                SetTextColor(hdc, if ui.dark { 0x00efefef } else { 0x00242424 });
                SetBkColor(hdc, if ui.dark { 0x00242424 } else { 0x00ffffff });
                return ui.brush as isize;
            }
            WM_ERASEBKGND => {
                let mut rect = zeroed();
                GetClientRect(hwnd, &mut rect);
                FillRect(w as HDC, &rect, ui.brush);
                return 1;
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
    let first_run_marker = path.with_file_name(".first-run-done");
    let first_run = first_run || !first_run_marker.exists();
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
                ShowWindow(hwnd, SW_RESTORE);
                SetForegroundWindow(hwnd);
            }
            return Ok(());
        }
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE);
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
        let class_name = wide(CLASS);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: module,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hIcon: icon,
            hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
            lpszClassName: class_name.as_ptr(),
            ..zeroed()
        };
        ensure!(
            RegisterClassW(&wc) != 0,
            "cannot register application window"
        );
        let (sender, receiver) = mpsc::channel();
        let mut light = 1u32;
        let mut size = 4u32;
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize").as_ptr(),
            wide("AppsUseLightTheme").as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut light as *mut u32).cast(),
            &mut size,
        );
        let dark = config.appearance == "dark" || (config.appearance == "auto" && light == 0);
        let dpi = GetDpiForSystem().max(96) as i32;
        let font = CreateFontW(
            -9 * dpi / 72,
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            DEFAULT_PITCH as u32,
            wide("Segoe UI").as_ptr(),
        );
        let ui = Box::new(RefCell::new(Ui {
            hwnd: null_mut(),
            config,
            path,
            proxy: None,
            fields: Vec::new(),
            labels: Vec::new(),
            status: null_mut(),
            sender,
            receiver,
            menu: null_mut(),
            icon,
            brush: CreateSolidBrush(if dark { 0x00242424 } else { 0x00ffffff }),
            dark,
            page: 0,
            dpi,
            font,
            update_busy: Cell::new(false),
            update_ready,
            prompted_version: None,
        }));
        let hwnd = CreateWindowExW(
            WS_EX_APPWINDOW,
            wide(CLASS).as_ptr(),
            wide("TgWsProxy+ — TgWsProxyPlus").as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            710 * dpi / 96,
            715 * dpi / 96,
            null_mut(),
            null_mut(),
            module,
            (&*ui as *const RefCell<Ui>).cast(),
        );
        ensure!(!hwnd.is_null(), "cannot create settings window");
        let dark_title: BOOL = if dark { 1 } else { 0 };
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            (&dark_title as *const BOOL).cast(),
            size_of::<BOOL>() as u32,
        );
        {
            let mut state = ui.borrow_mut();
            state.hwnd = hwnd;
            SetClassLongPtrW(hwnd, GCLP_HBRBACKGROUND, state.brush as isize);
            state.build()?;
            state.start()?;
        }
        let mut tray: NOTIFYICONDATAW = zeroed();
        tray.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        tray.hWnd = hwnd;
        tray.uID = 1;
        tray.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        tray.uCallbackMessage = TRAY;
        tray.hIcon = icon;
        let tip = wide("TgWsProxy+ • T1REI");
        tray.szTip[..tip.len()].copy_from_slice(&tip);
        ensure!(
            Shell_NotifyIconW(NIM_ADD, &tray) != 0,
            "cannot create system tray icon"
        );
        let restart_taskbar = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        if first_run {
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
            std::fs::write(&first_run_marker, b"TgWsProxyPlus\n")?;
        }
        if SetTimer(hwnd, 1001, 6 * 60 * 60 * 1000, None) == 0 {
            log::warn!("cannot schedule periodic update checks");
        }
        if ui.borrow().config.check_updates
            && let Err(error) = ui.borrow().background(true, false)
        {
            log::warn!("update worker: {error:#}");
        }
        let mut message: MSG = zeroed();
        loop {
            let result = GetMessageW(&mut message, null_mut(), 0, 0);
            if result <= 0 {
                break;
            }
            if message.message == restart_taskbar {
                Shell_NotifyIconW(NIM_ADD, &tray);
            }
            if IsDialogMessageW(hwnd, &message) == 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        KillTimer(hwnd, 1001);
        ui.borrow_mut().proxy.take();
        Shell_NotifyIconW(NIM_DELETE, &tray);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        DestroyWindow(hwnd);
        let state = ui.borrow();
        if !state.menu.is_null() {
            DestroyMenu(state.menu);
        }
        DestroyIcon(state.icon);
        DeleteObject(state.brush);
        DeleteObject(state.font);
        drop(state);
        drop(_instance);
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
            let (sender, receiver) = mpsc::channel();
            let ui = RefCell::new(Ui {
                hwnd: null_mut(),
                config: Config::fresh().unwrap(),
                path: PathBuf::new(),
                proxy: None,
                fields: Vec::new(),
                labels: Vec::new(),
                status: null_mut(),
                sender,
                receiver,
                menu: null_mut(),
                icon: null_mut(),
                brush: null_mut(),
                dark: false,
                page: 0,
                dpi: 96,
                font: null_mut(),
                update_busy: Cell::new(false),
                update_ready: None,
                prompted_version: None,
            });
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
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DestroyWindow(hwnd);
            let mut msg: MSG = zeroed();
            PeekMessageW(&mut msg, null_mut(), WM_QUIT, WM_QUIT, PM_REMOVE);
            UnregisterClassW(class.as_ptr(), module);
        }
    }
}
