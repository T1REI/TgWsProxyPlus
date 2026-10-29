use anyhow::Result;
use std::{cell::RefCell, path::Path, rc::Rc, sync::mpsc};
use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::{
    Win32::{
        Foundation::{E_FAIL, HWND, RECT},
        UI::Shell::SHCreateMemStream,
    },
    core::{Interface, PCWSTR, PWSTR},
};

use super::{EVENT, Event, wide};

include!(concat!(env!("OUT_DIR"), "/ui_assets.rs"));
const ORIGIN: &str = "https://tgws.local";
const DOCUMENT: &str = "https://tgws.local/index.html";

#[derive(Default)]
pub enum State {
    #[default]
    Empty,
    Loading,
    Ready(WebView),
    Failed(String),
}

pub struct WebView {
    controller: ICoreWebView2Controller,
    core: ICoreWebView2,
}

impl Drop for WebView {
    fn drop(&mut self) {
        // SAFETY: the controller is owned and released on its creating STA thread.
        unsafe {
            let _ = self.controller.Close();
        }
    }
}

fn wake(hwnd: usize) {
    // SAFETY: posting does not dereference the handle; it fails harmlessly after shutdown.
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(hwnd as _, EVENT, 0, 0);
    }
}

pub fn create(
    hwnd: usize,
    directory: &Path,
    state: Rc<RefCell<State>>,
    sender: mpsc::Sender<Event>,
) -> Result<()> {
    *state.borrow_mut() = State::Loading;
    std::fs::create_dir_all(directory)?;
    let profile = wide(directory);
    let completion = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
        move |result, environment| {
            let outcome = (|| -> windows::core::Result<()> {
                result?;
                let environment = environment.ok_or_else(|| windows::core::Error::from(E_FAIL))?;
                let controller_environment = environment.clone();
                let controller_state = state.clone();
                let sender = sender.clone();
                let completion = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
                    move |result, controller| {
                        let outcome = (|| -> windows::core::Result<WebView> {
                            result?;
                            let controller =
                                controller.ok_or_else(|| windows::core::Error::from(E_FAIL))?;
                            WebView::configure(
                                hwnd,
                                controller,
                                &controller_environment,
                                sender.clone(),
                            )
                        })();
                        *controller_state.borrow_mut() = match outcome {
                            Ok(view) => State::Ready(view),
                            Err(error) => State::Failed(error.to_string()),
                        };
                        wake(hwnd);
                        Ok(())
                    },
                ));
                // SAFETY: parent HWND belongs to this STA, callbacks retain their captured state.
                unsafe {
                    environment.CreateCoreWebView2Controller(HWND(hwnd as _), &completion)?;
                }
                Ok(())
            })();
            if let Err(error) = outcome {
                *state.borrow_mut() = State::Failed(error.to_string());
                wake(hwnd);
            }
            Ok(())
        },
    ));
    // SAFETY: strings live for this call; the runtime retains the completion interface.
    unsafe {
        CreateCoreWebView2EnvironmentWithOptions(
            PCWSTR::null(),
            PCWSTR(profile.as_ptr()),
            None,
            &completion,
        )?;
    }
    Ok(())
}

impl WebView {
    #[cfg(test)]
    pub fn core(&self) -> ICoreWebView2 {
        self.core.clone()
    }

    fn configure(
        hwnd: usize,
        controller: ICoreWebView2Controller,
        environment: &ICoreWebView2Environment,
        sender: mpsc::Sender<Event>,
    ) -> windows::core::Result<Self> {
        // SAFETY: all interfaces are used on the creating STA and output strings use COM allocation guards.
        unsafe {
            let core = controller.CoreWebView2()?;
            let settings = core.Settings()?;
            settings.SetAreDevToolsEnabled(false)?;
            settings.SetAreDefaultContextMenusEnabled(false)?;
            settings.SetIsStatusBarEnabled(false)?;
            settings.SetIsBuiltInErrorPageEnabled(false)?;
            let mut token = 0;
            core.AddWebResourceRequestedFilter(
                PCWSTR(wide("*").as_ptr()),
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
            )?;
            let environment = environment.clone();
            core.add_WebResourceRequested(&WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let request = args.Request()?;
                    let mut uri = PWSTR::null();
                    request.Uri(&mut uri)?;
                    let uri = CoTaskMemPWSTR::from(uri).to_string();
                    let resource = uri.strip_prefix(ORIGIN).and_then(|path| {
                        let path = path.split('?').next().unwrap_or(path);
                        let path = if path == "/" { "/index.html" } else { path };
                        ASSETS.iter().find(|(name, _, _)| *name == path)
                    });
                    let (code, reason, mime, body) = match resource {
                        Some((_, mime, data)) => (200, "OK", *mime, *data),
                        None => (404, "Not Found", "text/plain", &b"Not found"[..]),
                    };
                    let stream = SHCreateMemStream(Some(body)).ok_or_else(|| windows::core::Error::from(E_FAIL))?;
                    let headers = wide(format!("Content-Type: {mime}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n"));
                    let response = environment.CreateWebResourceResponse(&stream, code, PCWSTR(wide(reason).as_ptr()), PCWSTR(headers.as_ptr()))?;
                    args.SetResponse(&response)?;
                }
                Ok(())
            })), &mut token)?;
            core.add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        let mut uri = PWSTR::null();
                        args.Uri(&mut uri)?;
                        if CoTaskMemPWSTR::from(uri).to_string() != DOCUMENT {
                            args.SetCancel(true)?;
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            core.add_NewWindowRequested(
                &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        args.SetHandled(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            core.add_PermissionRequested(
                &PermissionRequestedEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        args.SetState(COREWEBVIEW2_PERMISSION_STATE_DENY)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            core.add_WebMessageReceived(
                &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        let mut source = PWSTR::null();
                        args.Source(&mut source)?;
                        if CoTaskMemPWSTR::from(source).to_string() != DOCUMENT {
                            return Ok(());
                        }
                        let mut json = PWSTR::null();
                        args.WebMessageAsJson(&mut json)?;
                        let json = CoTaskMemPWSTR::from(json).to_string();
                        if json.len() <= 65536 && sender.send(Event::Command(json)).is_ok() {
                            wake(hwnd);
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            if let Ok(controller2) = controller.cast::<ICoreWebView2Controller2>() {
                controller2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR {
                    A: 255,
                    R: 13,
                    G: 25,
                    B: 29,
                })?;
            }
            let view = Self { controller, core };
            view.resize(hwnd)?;
            view.visible(true)?;
            view.core.Navigate(PCWSTR(wide(DOCUMENT).as_ptr()))?;
            Ok(view)
        }
    }

    pub fn resize(&self, hwnd: usize) -> windows::core::Result<()> {
        let mut rect = windows_sys::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: valid parent window, stack-allocated rectangle, STA controller.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd as _, &mut rect);
            self.controller.SetBounds(RECT {
                left: 0,
                top: 0,
                right: rect.right,
                bottom: rect.bottom,
            })
        }
    }

    pub fn visible(&self, value: bool) -> windows::core::Result<()> {
        // SAFETY: STA controller.
        unsafe { self.controller.SetIsVisible(value) }
    }

    pub fn send(&self, value: &serde_json::Value) -> Result<()> {
        let json = wide(serde_json::to_string(value)?);
        // SAFETY: JSON string lives throughout this synchronous COM call.
        unsafe {
            self.core.PostWebMessageAsJson(PCWSTR(json.as_ptr()))?;
        }
        Ok(())
    }
}
