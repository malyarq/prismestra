//! Embedded WebView2 transport. COM callbacks only enqueue bounded messages.
use std::{cell::RefCell, collections::VecDeque, path::Path, rc::Rc, sync::mpsc};
use webview2_com::{
    AddScriptToExecuteOnDocumentCreatedCompletedHandler,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    DownloadStartingEventHandler, Microsoft::Web::WebView2::Win32::*,
    NavigationStartingEventHandler, NewWindowRequestedEventHandler,
    PermissionRequestedEventHandler, WebMessageReceivedEventHandler,
    WebResourceRequestedEventHandler, take_pwstr, wait_with_pump,
};
use windows::{
    Win32::{
        Foundation::{E_POINTER, HWND, RECT},
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
        UI::Shell::SHCreateMemStream,
    },
    core::{HSTRING, Interface, PCWSTR, PWSTR, w},
};

const ORIGIN: &str = "http://prismestra.localhost";
const MAX_MESSAGES: usize = 32;
const MAX_MESSAGE_BYTES: usize = 65_536;
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'none'; object-src 'none'; base-uri 'none'; frame-src 'none'; frame-ancestors 'none'; form-action 'none'";
type Assets = fn(&str) -> Option<(&'static str, &'static [u8])>;

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub struct WebShell {
    core: ICoreWebView2,
    controller: ICoreWebView2Controller,
    _environment: ICoreWebView2Environment,
    messages: Rc<RefCell<VecDeque<String>>>,
    _apartment: Apartment,
}
impl Drop for WebShell {
    fn drop(&mut self) {
        let _ = unsafe { self.controller.Close() };
    }
}

impl WebShell {
    /// Call on the parent window's UI thread, with its timer pumping, and drop before destroying HWND.
    pub fn new(
        hwnd: windows_sys::Win32::Foundation::HWND,
        state_dir: &Path,
        assets: Assets,
    ) -> Result<Self, String> {
        if hwnd.is_null() {
            return Err("WebView parent HWND is null".into());
        }
        let mut version = PWSTR::null();
        unsafe { GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut version) }
            .map_err(|error| {
                format!("Для нового интерфейса нужен Microsoft Edge WebView2 Runtime: {error}")
            })?;
        if !supported_version(&take_pwstr(version)) {
            return Err("Обновите Microsoft Edge WebView2 Runtime до версии 105 или новее".into());
        }
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() }.map_err(error)?;
        let apartment = Apartment;
        let profile = state_dir.join("webview-editor");
        std::fs::create_dir_all(&profile).map_err(|e| format!("WebView profile: {e}"))?;
        let environment = create_environment(&profile)?;
        let controller = create_controller(&environment, HWND(hwnd))?;
        let core = match unsafe { controller.CoreWebView2() } {
            Ok(core) => core,
            Err(e) => {
                let _ = unsafe { controller.Close() };
                return Err(error(e));
            }
        };
        // Own the controller before configuring callbacks, so every error path closes it.
        let shell = Self {
            core,
            controller,
            _environment: environment,
            messages: Rc::new(RefCell::new(VecDeque::new())),
            _apartment: apartment,
        };
        shell.configure(assets).map_err(error)?;
        Ok(shell)
    }

    fn configure(&self, assets: Assets) -> windows::core::Result<()> {
        unsafe {
            let settings = self.core.Settings()?;
            settings.SetIsScriptEnabled(true)?;
            settings.SetIsWebMessageEnabled(true)?;
            settings.SetAreDevToolsEnabled(false)?;
            settings.SetAreDefaultContextMenusEnabled(false)?;
            settings.SetAreDefaultScriptDialogsEnabled(false)?;
            settings.SetAreHostObjectsAllowed(false)?;
            settings.SetIsStatusBarEnabled(false)?;
            settings.SetIsZoomControlEnabled(false)?;
            settings.SetIsBuiltInErrorPageEnabled(false)?;
            if let Ok(settings3) = settings.cast::<ICoreWebView2Settings3>() {
                settings3.SetAreBrowserAcceleratorKeysEnabled(false)?;
            }
            if let Ok(settings4) = settings.cast::<ICoreWebView2Settings4>() {
                settings4.SetIsPasswordAutosaveEnabled(false)?;
                settings4.SetIsGeneralAutofillEnabled(false)?;
            }
            if let Ok(controller4) = self.controller.cast::<ICoreWebView2Controller4>() {
                controller4.SetAllowExternalDrop(false)?;
            }
            let mut token = 0;
            self.core.add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        args.SetCancel(true)?;
                        let mut uri = PWSTR::null();
                        args.Uri(&mut uri)?;
                        if same_origin(&take_pwstr(uri)) {
                            args.SetCancel(false)?;
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            self.core.add_FrameNavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        args.SetCancel(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            self.core.add_NewWindowRequested(
                &NewWindowRequestedEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        args.SetHandled(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            self.core.add_PermissionRequested(
                &PermissionRequestedEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        args.SetState(COREWEBVIEW2_PERMISSION_STATE_DENY)?;
                        if let Ok(args2) = args.cast::<ICoreWebView2PermissionRequestedEventArgs2>()
                        {
                            args2.SetHandled(true)?;
                        }
                        if let Ok(args3) = args.cast::<ICoreWebView2PermissionRequestedEventArgs3>()
                        {
                            args3.SetSavesInProfile(false)?;
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            // DownloadStarting has been available since Runtime 86 (minimum supported here: 105).
            self.core.cast::<ICoreWebView2_4>()?.add_DownloadStarting(
                &DownloadStartingEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        args.SetCancel(true)?;
                        args.SetHandled(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let environment = self._environment.clone();
            self.core.add_WebResourceRequested(
                &WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else {
                        return Ok(());
                    };
                    // Deny first: failures while reading request metadata must not fall through to the network.
                    let denied = environment.CreateWebResourceResponse(
                        None,
                        403,
                        w!("Forbidden"),
                        &HSTRING::from(headers("text/plain; charset=utf-8")),
                    )?;
                    args.SetResponse(&denied)?;
                    let request = args.Request()?;
                    let mut uri = PWSTR::null();
                    request.Uri(&mut uri)?;
                    let uri = take_pwstr(uri);
                    let mut method = PWSTR::null();
                    request.Method(&mut method)?;
                    let method = take_pwstr(method);
                    if let Some((mime, bytes)) = requested_asset(&uri, &method, assets) {
                        let stream = SHCreateMemStream(Some(bytes))
                            .ok_or_else(|| windows::core::Error::from(E_POINTER))?;
                        let response = environment.CreateWebResourceResponse(
                            &stream,
                            200,
                            w!("OK"),
                            &HSTRING::from(headers(mime)),
                        )?;
                        args.SetResponse(&response)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            if let Ok(core22) = self.core.cast::<ICoreWebView2_22>() {
                core22.AddWebResourceRequestedFilterWithRequestSourceKinds(
                    w!("*"),
                    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
                    COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
                )?;
            } else {
                // Older runtimes use the frame filter; CSP default-src 'none' also disallows workers.
                self.core.AddWebResourceRequestedFilter(
                    w!("*"),
                    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
                )?;
            }
            let messages = Rc::clone(&self.messages);
            self.core.add_WebMessageReceived(
                &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else {
                        return Ok(());
                    };
                    let mut source = PWSTR::null();
                    args.Source(&mut source)?;
                    if same_origin(&take_pwstr(source)) {
                        let mut body = PWSTR::null();
                        args.TryGetWebMessageAsString(&mut body)?;
                        let body = take_pwstr(body);
                        if let Ok(mut queue) = messages.try_borrow_mut() {
                            enqueue(&mut queue, body);
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        let core = self.core.clone();
        AddScriptToExecuteOnDocumentCreatedCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| unsafe {
                core.AddScriptToExecuteOnDocumentCreated(w!("Object.defineProperty(window,'ipc',{value:Object.freeze({postMessage:s=>window.chrome.webview.postMessage(s)})});"), &handler).map_err(Into::into)
            }), Box::new(|result, _| result),
        ).map_err(|e| windows::core::Error::new(windows::core::HRESULT(0x80004005u32 as i32), e.to_string()))?;
        unsafe {
            self.controller.SetIsVisible(true)?;
            self.core
                .Navigate(&HSTRING::from(format!("{ORIGIN}/index.html")))
        }
    }

    pub fn resize(&self, width: i32, height: i32) -> Result<(), String> {
        unsafe {
            self.controller.SetBounds(RECT {
                left: 0,
                top: 0,
                right: width.max(0),
                bottom: height.max(0),
            })
        }
        .map_err(error)
    }
    pub fn send(&self, value: &serde_json::Value) -> Result<(), String> {
        let json = serde_json::to_string(value).map_err(|e| format!("WebView message: {e}"))?;
        unsafe {
            self.core.ExecuteScript(
                &HSTRING::from(format!("window.prismestraReceive({json});")),
                None,
            )
        }
        .map_err(error)
    }
    pub fn drain(&self) -> Vec<String> {
        self.messages.borrow_mut().drain(..).collect()
    }
    pub fn set_visible(&self, visible: bool) -> Result<(), String> {
        unsafe { self.controller.SetIsVisible(visible) }.map_err(error)
    }
    pub fn focus(&self) -> Result<(), String> {
        unsafe {
            self.controller
                .MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)
        }
        .map_err(error)
    }
}

fn error(error: windows::core::Error) -> String {
    format!("WebView2 editor: {error}")
}
fn supported_version(version: &str) -> bool {
    version
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major >= 105)
}
fn create_environment(profile: &Path) -> Result<ICoreWebView2Environment, String> {
    let (tx, rx) = mpsc::channel();
    unsafe {
        CreateCoreWebView2EnvironmentWithOptions(
            PCWSTR::null(),
            &HSTRING::from(profile.as_os_str()),
            None,
            &CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
                move |result, environment| {
                    let result = result.and_then(|()| {
                        environment.ok_or_else(|| windows::core::Error::from(E_POINTER))
                    });
                    let _ = tx.send(result);
                    Ok(())
                },
            )),
        )
    }
    .map_err(error)?;
    wait_with_pump(rx)
        .map_err(|e| format!("Create WebView environment: {e}"))?
        .map_err(error)
}
fn create_controller(
    environment: &ICoreWebView2Environment,
    hwnd: HWND,
) -> Result<ICoreWebView2Controller, String> {
    let (tx, rx) = mpsc::channel();
    unsafe {
        environment.CreateCoreWebView2Controller(
            hwnd,
            &CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
                move |result, controller| {
                    let result = result.and_then(|()| {
                        controller.ok_or_else(|| windows::core::Error::from(E_POINTER))
                    });
                    let _ = tx.send(result);
                    Ok(())
                },
            )),
        )
    }
    .map_err(error)?;
    wait_with_pump(rx)
        .map_err(|e| format!("Create WebView controller: {e}"))?
        .map_err(error)
}
fn same_origin(url: &str) -> bool {
    url.strip_prefix(ORIGIN)
        .is_some_and(|tail| tail.is_empty() || tail.starts_with(['/', '?', '#']))
}
fn enqueue(queue: &mut VecDeque<String>, body: String) {
    if body.len() <= MAX_MESSAGE_BYTES && queue.len() < MAX_MESSAGES {
        queue.push_back(body);
    }
}
fn requested_asset(
    url: &str,
    method: &str,
    assets: Assets,
) -> Option<(&'static str, &'static [u8])> {
    let path = url.strip_prefix(ORIGIN)?;
    if method != "GET"
        || url.len() > 4096
        || !path.starts_with('/')
        || path.contains(['?', '#', '\\', '%'])
        || path.split('/').any(|p| p == "." || p == "..")
    {
        return None;
    }
    assets(path)
}
fn headers(mime: &str) -> String {
    format!(
        "Content-Type: {mime}\r\nContent-Security-Policy: {CSP}\r\nX-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origin_is_exact_and_queue_is_bounded() {
        assert!(same_origin("http://prismestra.localhost/index.html#scene"));
        for url in [
            "https://prismestra.localhost/",
            "http://prismestra.localhost.evil/",
            "http://prismestra.localhost:80/",
            "http://user@prismestra.localhost/",
            "http://prismestra.localhost\\evil/",
            "file:///index.html",
        ] {
            assert!(!same_origin(url), "{url}");
        }
        let mut queue = VecDeque::new();
        enqueue(&mut queue, "x".repeat(MAX_MESSAGE_BYTES + 1));
        assert!(queue.is_empty());
        for _ in 0..MAX_MESSAGES + 1 {
            enqueue(&mut queue, "x".repeat(MAX_MESSAGE_BYTES));
        }
        assert_eq!(queue.len(), MAX_MESSAGES);
    }
    #[test]
    fn only_exact_known_get_assets_are_served_with_csp() {
        fn asset(path: &str) -> Option<(&'static str, &'static [u8])> {
            (path == "/index.html").then_some(("text/html", b"editor"))
        }
        assert!(requested_asset("http://prismestra.localhost/index.html", "GET", asset).is_some());
        for (method, url) in [
            ("POST", "http://prismestra.localhost/index.html"),
            ("GET", "http://prismestra.localhost.evil/index.html"),
            ("GET", "https://prismestra.localhost/index.html"),
            ("GET", "http://prismestra.localhost:80/index.html"),
            ("GET", "http://prismestra.localhost/index.html?secret"),
            ("GET", "http://prismestra.localhost/../index.html"),
            ("GET", "http://prismestra.localhost/%69ndex.html"),
            ("GET", "file:///index.html"),
            ("GET", "http://prismestra.localhost/missing"),
        ] {
            assert!(
                requested_asset(url, method, asset).is_none(),
                "{method} {url}"
            );
        }
        assert!(headers("text/html").contains(&format!("Content-Security-Policy: {CSP}\r\n")));
    }
    #[test]
    fn runtime_version_requires_105_or_newer() {
        for version in ["105.0.0.0", "142.0.3595.65"] {
            assert!(supported_version(version));
        }
        for version in ["104.0.0.0", "", "bad.105", "-1.0.0.0"] {
            assert!(!supported_version(version));
        }
    }
}
