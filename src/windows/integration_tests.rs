use super::*;
use webview2_com::{
    CapturePreviewCompletedHandler, ExecuteScriptCompletedHandler,
    Microsoft::Web::WebView2::Win32::*,
};
use windows::core::PCWSTR;

fn pump() {
    // SAFETY: this test owns its window and pumps only the current STA thread.
    unsafe {
        let mut msg: MSG = zeroed();
        while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    thread::sleep(Duration::from_millis(10));
}

#[track_caller]
fn receive<T>(receiver: mpsc::Receiver<T>) -> T {
    let started = Instant::now();
    loop {
        if let Ok(value) = receiver.try_recv() {
            return value;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "WebView2 callback timed out"
        );
        pump();
    }
}

fn core(ui: &RefCell<Ui>) -> ICoreWebView2 {
    let ui = ui.borrow();
    let view = ui.webview.borrow();
    match &*view {
        webview::State::Ready(view) => view.core(),
        _ => panic!("WebView2 not ready"),
    }
}

#[track_caller]
fn evaluate(ui: &RefCell<Ui>, script: &str) -> serde_json::Value {
    let (tx, rx) = mpsc::channel();
    // SAFETY: script and COM object belong to this STA. The callback copies its borrowed result.
    unsafe {
        core(ui)
            .ExecuteScript(
                PCWSTR(wide(script).as_ptr()),
                &ExecuteScriptCompletedHandler::create(Box::new(move |result, value| {
                    tx.send(result.map(|()| value).map_err(|e| e.to_string()))
                        .unwrap();
                    Ok(())
                })),
            )
            .unwrap();
    }
    let json = receive(rx).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn until(ui: &RefCell<Ui>, script: &str) {
    let started = Instant::now();
    while evaluate(ui, script) != serde_json::json!(true) {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "UI condition timed out: {script}"
        );
        pump();
    }
}

fn screenshot(ui: &RefCell<Ui>, name: &str) {
    evaluate(
        ui,
        "window.testPaintReady=false; requestAnimationFrame(()=>requestAnimationFrame(()=>{window.testPaintReady=true})); null",
    );
    until(ui, "window.testPaintReady === true");
    let path = std::env::current_dir()
        .unwrap()
        .join("target/ui-review")
        .join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let (tx, rx) = mpsc::channel();
    // SAFETY: the stream writes only to this test artifact; all interfaces remain on the STA.
    unsafe {
        let stream = windows::Win32::UI::Shell::SHCreateStreamOnFileEx(
            PCWSTR(wide(&path).as_ptr()),
            0x1001,
            0,
            true,
            None,
        )
        .unwrap();
        core(ui)
            .CapturePreview(
                COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
                &stream,
                &CapturePreviewCompletedHandler::create(Box::new(move |result| {
                    tx.send(result.map_err(|e| e.to_string())).unwrap();
                    Ok(())
                })),
            )
            .unwrap();
        receive(rx).unwrap();
    }
    assert!(path.metadata().unwrap().len() > 1024);
}

#[test]
#[ignore = "requires installed system WebView2 Runtime and a Windows desktop"]
fn system_webview_renders_and_saves_without_restarting_for_theme() {
    unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        )
        .ok()
        .unwrap();
        let directory = std::env::current_dir()
            .unwrap()
            .join("target/ui-review")
            .join(format!("session-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = Config::fresh().unwrap();
        config.port = listener.local_addr().unwrap().port();
        config.cfproxy = false;
        config.h2 = false;
        config.pool_size = 0;
        config.appearance = "dark".into();
        drop(listener);
        let path = directory.join("config.json");
        config.save(&path).unwrap();
        let ui = RefCell::new(Ui::new(config, path.clone(), None));
        let module = GetModuleHandleW(null());
        let class = wide(format!("TgWsProxyPlusWebViewTest{}", std::process::id()));
        let brush = CreateSolidBrush(0x001d190d);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: module,
            hbrBackground: brush,
            lpszClassName: class.as_ptr(),
            ..zeroed()
        };
        assert_ne!(RegisterClassW(&wc), 0);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("TgWsProxy+ — UI test").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            40,
            40,
            660,
            520,
            null_mut(),
            null_mut(),
            module,
            (&ui as *const RefCell<Ui>).cast(),
        );
        assert!(!hwnd.is_null());
        ui.borrow_mut().hwnd = hwnd;
        ui.borrow_mut().start().unwrap();
        assert!(matches!(
            *ui.borrow().webview.borrow(),
            webview::State::Empty
        ));
        ui.borrow_mut().show().unwrap();
        let started = Instant::now();
        loop {
            pump();
            let state = ui.borrow();
            if matches!(*state.webview.borrow(), webview::State::Ready(_)) && state.ready {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "WebView2 did not initialize: {}",
                state.status
            );
        }
        until(
            &ui,
            "document.querySelector('main h1')?.textContent === 'Прокси' && !document.querySelector('.proxy-error') && document.querySelector('.main-actions .primary')?.disabled === false",
        );
        screenshot(&ui, "overview.png");
        assert_eq!(
            evaluate(
                &ui,
                "document.documentElement.scrollWidth <= innerWidth && document.querySelector('main').scrollHeight <= document.querySelector('main').clientHeight"
            ),
            true,
            "home page should fit the compact window"
        );
        assert_eq!(
            evaluate(&ui, "document.body.innerText.includes('T1REI')"),
            false
        );
        for page in ["connection", "routes", "diagnostics"] {
            evaluate(
                &ui,
                &format!("document.querySelector('nav button[data-page={page}]').click(); null"),
            );
            until(
                &ui,
                &format!(
                    "document.querySelector('nav button[data-page={page}]').getAttribute('aria-current') === 'page'"
                ),
            );
            assert_eq!(
                evaluate(
                    &ui,
                    "document.querySelector('main').scrollWidth <= document.querySelector('main').clientWidth"
                ),
                true,
                "page {page} overflows horizontally"
            );
            screenshot(&ui, &format!("{page}.png"));
        }
        evaluate(
            &ui,
            "window.chrome.webview.addEventListener('message', e => { if(e.data.id === 9001) window.testReply = e.data; }); window.chrome.webview.postMessage({id:9001,command:'run_shell',data:'anything'}); null",
        );
        until(&ui, "window.testReply?.ok === false");
        assert_eq!(ui.borrow().generation, 1);
        evaluate(
            &ui,
            "document.querySelector('nav button[data-page=settings]').click(); null",
        );
        until(
            &ui,
            "document.querySelector('main h1')?.textContent === 'Настройки'",
        );
        evaluate(
            &ui,
            "const select = document.querySelector('select'); select.value='light'; select.dispatchEvent(new Event('change',{bubbles:true})); null",
        );
        until(
            &ui,
            "!!document.querySelector('.save-bar') && document.documentElement.dataset.theme === 'light'",
        );
        ui.borrow().publish();
        pump();
        assert_eq!(
            evaluate(&ui, "document.querySelector('select').value"),
            "light"
        );
        evaluate(
            &ui,
            "document.querySelector('.save-bar .primary').click(); null",
        );
        until(&ui, "!document.querySelector('.save-bar')");
        assert_eq!(Config::load(&path).unwrap().appearance, "light");
        assert_eq!(
            ui.borrow().generation,
            1,
            "theme changes restarted the proxy"
        );
        evaluate(
            &ui,
            "document.querySelector('.toast button')?.click(); null",
        );
        until(&ui, "!document.querySelector('.toast')");
        assert_eq!(
            evaluate(
                &ui,
                "document.querySelector('main').scrollHeight <= document.querySelector('main').clientHeight"
            ),
            true,
            "settings should fit the compact window"
        );
        screenshot(&ui, "settings-light.png");
        evaluate(
            &ui,
            "document.querySelector('nav button[data-page=connection]').click(); null",
        );
        until(
            &ui,
            "document.querySelector('main h1')?.textContent === 'Подключение'",
        );
        let original_port = ui.borrow().config.port;
        evaluate(
            &ui,
            "const port = document.querySelector('input[type=number]'); port.value='0'; port.dispatchEvent(new Event('input',{bubbles:true})); null",
        );
        until(&ui, "!!document.querySelector('.save-bar')");
        evaluate(
            &ui,
            "document.querySelector('.save-bar .primary').click(); null",
        );
        until(&ui, "!!document.querySelector('.toast.error')");
        assert_eq!(Config::load(&path).unwrap().port, original_port);
        assert_eq!(ui.borrow().generation, 1);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let new_port = listener.local_addr().unwrap().port();
        drop(listener);
        evaluate(
            &ui,
            &format!(
                "const nextPort = document.querySelector('input[type=number]'); nextPort.value='{new_port}'; nextPort.dispatchEvent(new Event('input',{{bubbles:true}})); null"
            ),
        );
        evaluate(
            &ui,
            "document.querySelector('.save-bar .primary').click(); null",
        );
        until(
            &ui,
            "!document.querySelector('.save-bar') && !document.querySelector('.proxy-error')",
        );
        assert_eq!(Config::load(&path).unwrap().port, new_port);
        assert_eq!(ui.borrow().generation, 2);
        assert!(std::net::TcpStream::connect(("127.0.0.1", original_port)).is_err());
        assert!(std::net::TcpStream::connect(("127.0.0.1", new_port)).is_ok());
        core(&ui)
            .Navigate(PCWSTR(wide("https://example.com/").as_ptr()))
            .unwrap();
        pump();
        assert_eq!(
            evaluate(&ui, "location.href"),
            "https://tgws.local/index.html"
        );
        SetWindowPos(hwnd, null_mut(), 0, 0, 580, 460, SWP_NOMOVE | SWP_NOZORDER);
        for page in [
            "overview",
            "connection",
            "routes",
            "diagnostics",
            "settings",
        ] {
            evaluate(
                &ui,
                &format!("document.querySelector('nav button[data-page={page}]').click(); null"),
            );
            until(
                &ui,
                &format!(
                    "document.querySelector('nav button[data-page={page}]').getAttribute('aria-current') === 'page'"
                ),
            );
            assert_eq!(
                evaluate(
                    &ui,
                    "document.documentElement.scrollWidth <= innerWidth && document.querySelector('main').scrollWidth <= document.querySelector('main').clientWidth"
                ),
                true,
                "page {page} overflows at minimum size"
            );
        }
        evaluate(
            &ui,
            "document.querySelector('nav button[data-page=overview]').click(); document.querySelector('.toast button')?.click(); null",
        );
        until(
            &ui,
            "document.querySelector('nav button[data-page=overview]').getAttribute('aria-current') === 'page'",
        );
        screenshot(&ui, "overview-small.png");
        let port = ui.borrow().config.port;
        SendMessageW(hwnd, WM_CLOSE, 0, 0);
        assert_eq!(IsWindowVisible(hwnd), 0);
        assert!(ui.borrow().ready);
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
        ui.borrow_mut().proxy.take();
        *ui.borrow().webview.borrow_mut() = webview::State::Empty;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        DestroyWindow(hwnd);
        if !ui.borrow().menu.is_null() {
            DestroyMenu(ui.borrow().menu);
        }
        DeleteObject(brush);
        UnregisterClassW(class.as_ptr(), module);
        drop(ui);
        windows::Win32::System::Com::CoUninitialize();
    }
}
