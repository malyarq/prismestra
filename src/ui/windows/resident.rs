//! One native session owns the Runtime; editor HWNDs are disposable.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    os::windows::fs::OpenOptionsExt,
    sync::atomic::{AtomicU32, Ordering},
};
use windows_sys::Win32::{
    Security::*,
    System::Threading::{GetCurrentProcess, OpenProcessToken},
    UI::Shell::*,
};
const CLASS: &str = "PrismestraSessionShell";
const CALLBACK: u32 = WM_APP + 31;
const OPEN: u32 = 1;
const STOP: u32 = 2;
const EXIT: u32 = 3;
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) enum Action {
    #[default]
    None,
    Stop,
    Exit,
}
/// The OS file handle supplies exclusion even after a crash; a stale filename does not lock.
pub(super) struct Instance {
    _file: File,
}
impl Instance {
    pub(super) fn acquire(state_dir: &Path) -> Result<Self, String> {
        let mut raw = null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
            return Err(last("Read session identity"));
        }
        struct Token(HANDLE);
        impl Drop for Token {
            fn drop(&mut self) {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
        let token = Token(raw);
        let mut needed = 0;
        unsafe {
            GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut needed);
        }
        if !(size_of::<TOKEN_USER>()..=4096).contains(&(needed as usize)) {
            return Err("Invalid session identity size".into());
        }
        let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(last("Read user identity"));
        }
        let user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
        let length = unsafe { GetLengthSid(user.User.Sid) } as usize;
        if !(8..=1024).contains(&length) {
            return Err("Invalid user SID size".into());
        }
        let sid = unsafe { std::slice::from_raw_parts(user.User.Sid.cast::<u8>(), length) };
        let hash = Sha256::digest(sid)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let mut session = 0u32;
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenSessionId,
                (&mut session as *mut u32).cast(),
                4,
                &mut needed,
            )
        } == 0
        {
            return Err(last("Read logon session"));
        }
        let path = state_dir.join(format!(".prismestra-session-{hash}-{session}.lock"));
        let file=OpenOptions::new().read(true).write(true).create(true).truncate(false).share_mode(0).open(path).map_err(|e|{
            if e.raw_os_error()==Some(ERROR_SHARING_VIOLATION as i32){"Prismestra уже работает с этой папкой настроек. Откройте её через значок в трее.".into()}else{format!("Session lock: {e}")}
        })?;
        Ok(Self { _file: file })
    }
}
pub(super) struct Shell {
    hwnd: HWND,
    tray: bool,
}
impl Shell {
    pub(super) fn create(state: &State, tray: bool) -> Result<Self, String> {
        let instance = unsafe { GetModuleHandleW(null()) };
        let class = wide(CLASS);
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..Default::default()
        };
        if unsafe { RegisterClassExW(&wc) } == 0
            && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
        {
            return Err(last("Register session shell"));
        }
        // Hidden top-level window, deliberately not HWND_MESSAGE: receives OS power events.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class.as_ptr(),
                wide("Prismestra Session").as_ptr(),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                instance,
                (state as *const State).cast(),
            )
        };
        if hwnd.is_null() {
            return Err(last("Create session shell"));
        }
        state.shell_window.set(hwnd);
        state.tray_enabled.set(tray);
        let shell = Self { hwnd, tray };
        if tray {
            TASKBAR_CREATED.store(
                unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) },
                Ordering::Relaxed,
            );
            tray_add(hwnd)?;
        }
        if unsafe { SetTimer(hwnd, 1, 50, None) } == 0 {
            return Err(last("Start session maintenance"));
        }
        Ok(shell)
    }
}
impl Drop for Shell {
    fn drop(&mut self) {
        if self.tray {
            let data = tray_data(self.hwnd);
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &data);
            }
        }
        unsafe {
            DestroyWindow(self.hwnd);
        }
    }
}
fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: CALLBACK,
        hIcon: unsafe { LoadIconW(null_mut(), IDI_APPLICATION) },
        ..Default::default()
    };
    for (out, input) in data
        .szTip
        .iter_mut()
        .zip("Prismestra · подсветка и температуры".encode_utf16())
    {
        *out = input;
    }
    data
}
fn tray_add(hwnd: HWND) -> Result<(), String> {
    let data = tray_data(hwnd);
    if unsafe { Shell_NotifyIconW(NIM_ADD, &data) } == 0 {
        Err(last("Create tray icon"))
    } else {
        Ok(())
    }
}
pub(super) fn report(state: &State, error: String) {
    if let Ok(mut slot) = state.app.try_borrow_mut()
        && let Some(a) = slot.as_mut()
    {
        a.status = error;
    }
    let hwnd = state.editor_window.get();
    if !hwnd.is_null() {
        unsafe {
            InvalidateRect(hwnd, null(), 0);
        }
    }
}
pub(super) fn close_editor(state: &State) -> Result<(), String> {
    let hwnd = state.editor_window.get();
    if hwnd.is_null() {
        return Ok(());
    }
    {
        let slot = state.app.try_borrow().map_err(|_| "Редактор занят")?;
        let a = slot.as_ref().ok_or("Нет сессии")?;
        a.guard_connections_close()?;
        a.guard_sources_exit()?;
        if a.editor_dirty {
            return Err(
                "Примените или сбросьте правки параметров перед закрытием редактора.".into(),
            );
        }
    }
    // Native teardown can reenter the window procedure; do not retain a RefCell borrow.
    let mut app = state.app.borrow_mut().take().ok_or("Нет сессии")?;
    let destroyed = unsafe { DestroyWindow(hwnd) };
    if destroyed != 0 {
        app.widgets.clear();
        app.buffer = None;
        app.lcd_image = vec![];
        app.ambient_image = vec![];
        app.lcd_frame = (0, 0);
        app.ambient_frame = (0, 0);
        app.status =
            "Редактор закрыт; подсветка, расписание и чтение датчиков продолжаются.".into();
    }
    state.app.replace(Some(app));
    if destroyed == 0 {
        Err(last("Close editor"))
    } else {
        Ok(())
    }
}
pub(super) fn open_editor(state: &State) -> Result<(), String> {
    let existing = state.editor_window.get();
    if !existing.is_null() {
        if !state.snapshot {
            unsafe {
                ShowWindow(existing, SW_RESTORE);
                SetForegroundWindow(existing);
            }
        }
        return Ok(());
    }
    let mut app = state.app.borrow_mut().take().ok_or("Нет сессии")?;
    let result = (|| {
        let old_dpi = app.dpi;
        let dpi = if state.snapshot {
            old_dpi
        } else {
            unsafe { GetDpiForSystem() }.max(96)
        };
        let width = app.width * 96 / old_dpi as i32;
        let height = app.height * 96 / old_dpi as i32;
        let (width, height) = if state.snapshot {
            (width, height)
        } else {
            (width.max(MIN_WIDTH), height.max(MIN_HEIGHT))
        };
        app.painter = Painter::new(dpi)?;
        app.dpi = dpi;
        let style = if state.snapshot {
            WS_POPUP
        } else {
            WS_OVERLAPPEDWINDOW
        } | WS_CLIPCHILDREN;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: px(width, dpi),
            bottom: px(height, dpi),
        };
        if unsafe { AdjustWindowRectExForDpi(&mut rect, style, 0, WS_EX_CONTROLPARENT, dpi) } == 0 {
            return Err(last("Size editor"));
        }
        let title = if app.native_lighting.is_some() {
            "Prismestra · Подсветка устройств"
        } else {
            "Prismestra · Предпросмотр"
        };
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_CONTROLPARENT,
                wide(super::CLASS).as_ptr(),
                wide(title).as_ptr(),
                style,
                if state.snapshot {
                    -32000
                } else {
                    CW_USEDEFAULT
                },
                if state.snapshot {
                    -32000
                } else {
                    CW_USEDEFAULT
                },
                rect.right - rect.left,
                rect.bottom - rect.top,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                (state as *const State).cast(),
            )
        };
        if hwnd.is_null() {
            return Err(last("Reopen editor"));
        }
        state.editor_window.set(hwnd);
        let built = app.build(hwnd).and_then(|_| {
            if app.page == Page::Connections {
                if app.connections.scan.is_idle() {
                    app.open_connections()?;
                } else {
                    app.page = Page::Scene;
                }
            }
            let mut client = RECT::default();
            if unsafe { GetClientRect(hwnd, &mut client) } == 0 {
                return Err(last("Measure editor"));
            }
            app.layout(client.right, client.bottom);
            Ok(())
        });
        if let Err(error) = built {
            unsafe {
                DestroyWindow(hwnd);
            }
            app.widgets.clear();
            return Err(error);
        }
        if !state.snapshot && unsafe { SetTimer(hwnd, 1, 16, None) } == 0 {
            unsafe {
                DestroyWindow(hwnd);
            }
            app.widgets.clear();
            return Err(last("Start editor timer"));
        }
        Ok(hwnd)
    })();
    state.app.replace(Some(app));
    let hwnd = result?;
    unsafe {
        if state.snapshot {
            SetWindowPos(
                hwnd,
                null_mut(),
                -32000,
                -32000,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
        } else {
            ShowWindow(hwnd, SW_SHOWNORMAL);
            UpdateWindow(hwnd);
        }
    }
    Ok(())
}
fn request(state: &State, action: Action, confirm: bool) -> Result<(), String> {
    if state.session_action.get() != Action::None {
        return Ok(());
    }
    if action == Action::Exit && confirm && !confirm_discard(state.editor_window.get(), state)? {
        return Ok(());
    }
    let mut slot = state.app.borrow_mut();
    let app = slot.as_mut().ok_or("Нет сессии")?;
    if action == Action::Exit && !app.widgets.is_empty() {
        app.guard_connections_close()?;
        app.guard_sources_exit()?;
        if app.editor_dirty {
            return Err("Примените или сбросьте правки перед выходом.".into());
        }
    }
    app.begin_shutdown();
    if app.shutdown.active() {
        state.session_action.set(action);
    }
    Ok(())
}
fn finish_stop(app: &mut App) -> Result<(), String> {
    let document = app.runtime.document().clone();
    let journal = journal_path(&app.state_dir, app.path.as_deref(), &document);
    let mut runtime = Runtime::new(document, Some(journal))?;
    runtime.enable_autonomous_rgb()?;
    runtime.set_group(Target::Pc, false)?;
    runtime.set_group(Target::Door, false)?;
    app.runtime = Client::new(runtime, true)?;
    app.pending_open = None;
    app.was_pending = false;
    app.native_lighting = None;
    app.connections.stopped();
    app.shutdown = Default::default();
    app.groups = [false, false];
    app.lcd_image = vec![];
    app.ambient_image = vec![];
    app.lcd_frame = (0, 0);
    app.ambient_frame = (0, 0);
    app.status="Работа остановлена. Устройства освобождены; для запуска выберите подключения и нажмите «Применить».".into();
    app.set_check(PC_ON, false);
    app.set_check(DOOR_ON, false);
    app.refresh_connections();
    app.show_page();
    Ok(())
}
fn maintain(state: &State) -> Result<(), String> {
    let mut slot = state.app.try_borrow_mut().map_err(|_| "Сессия занята")?;
    let app = slot.as_mut().ok_or("Нет сессии")?;
    if state.editor_window.get().is_null() || state.session_action.get() != Action::None {
        app.tick()?;
    }
    if state.session_action.get() != Action::None && app.close_ready() {
        match state.session_action.get() {
            Action::Stop => {
                finish_stop(app)?;
                state.session_action.set(Action::None);
            }
            Action::Exit => {
                drop(slot);
                let editor = state.editor_window.get();
                if !editor.is_null() {
                    unsafe {
                        DestroyWindow(editor);
                    }
                }
                unsafe {
                    PostQuitMessage(0);
                }
                return Ok(());
            }
            Action::None => {}
        }
    }
    let editor = state.editor_window.get();
    if !editor.is_null() && state.session_action.get() != Action::None {
        unsafe {
            InvalidateRect(editor, null(), 0);
        }
    }
    Ok(())
}
fn menu(state: &State, hwnd: HWND) {
    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        return;
    }
    for (id, text) in [
        (OPEN, "Открыть редактор"),
        (STOP, "Остановить работу"),
        (EXIT, "Выйти"),
    ] {
        unsafe {
            AppendMenuW(menu, MF_STRING, id as usize, wide(text).as_ptr());
        }
    }
    let mut point = POINT::default();
    unsafe {
        GetCursorPos(&mut point);
        SetForegroundWindow(hwnd);
    }
    let id = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            0,
            hwnd,
            null(),
        )
    };
    unsafe {
        DestroyMenu(menu);
        PostMessageW(hwnd, WM_NULL, 0, 0);
    }
    let result = match id as u32 {
        OPEN => open_editor(state),
        STOP => request(state, Action::Stop, false),
        EXIT => request(state, Action::Exit, true),
        _ => Ok(()),
    };
    if let Err(error) = result {
        report(state, error);
    }
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    std::panic::catch_unwind(|| unsafe { dispatch(hwnd, msg, w, l) }).unwrap_or(0)
}
unsafe fn dispatch(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let create = unsafe { &*(l as *const CREATESTRUCTW) };
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        return 1;
    }
    let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const State;
    if raw.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, w, l) };
    }
    let state = unsafe { &*raw };
    if msg == WM_NCDESTROY {
        if state.shell_window.get() == hwnd {
            state.shell_window.set(null_mut());
        }
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        }
        return unsafe { DefWindowProcW(hwnd, msg, w, l) };
    }
    if msg == WM_DESTROY {
        unsafe {
            KillTimer(hwnd, 1);
        }
        return 0;
    }
    if msg == TASKBAR_CREATED.load(Ordering::Relaxed) && msg != 0 && state.tray_enabled.get() {
        if let Err(e) = tray_add(hwnd) {
            report(state, e);
        }
        return 0;
    }
    let result = match msg {
        WM_TIMER => maintain(state),
        WM_CLOSE => request(state, Action::Exit, true),
        CALLBACK => {
            match l as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                    if let Err(e) = open_editor(state) {
                        report(state, e);
                    }
                }
                WM_RBUTTONUP => menu(state, hwnd),
                _ => {}
            }
            Ok(())
        }
        WM_POWERBROADCAST => {
            if let Ok(mut slot) = state.app.try_borrow_mut()
                && let Some(a) = slot.as_mut()
                && !a.shutdown.active()
                && matches!(w as u32, PBT_APMSUSPEND | PBT_APMRESUMEAUTOMATIC)
            {
                a.runtime
                    .notify_power(w as u32 == PBT_APMSUSPEND, crate::clock::monotonic_us())
            } else {
                Ok(())
            }
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, w, l) },
    };
    if let Err(e) = result {
        report(state, e);
    }
    if msg == WM_POWERBROADCAST { 1 } else { 0 }
}
fn settle(state: &State, label: &str, done: impl Fn(&App) -> bool) -> Result<(), String> {
    let end = Instant::now() + std::time::Duration::from_secs(12);
    loop {
        // The stand does not pump global UI messages. Simulate only our editor timer.
        if !state.editor_window.get().is_null() && state.session_action.get() == Action::None {
            state.app.borrow_mut().as_mut().unwrap().tick()?;
        }
        maintain(state)?;
        {
            let slot = state.app.borrow();
            if done(slot.as_ref().unwrap()) {
                return Ok(());
            }
        }
        if Instant::now() >= end {
            let slot = state.app.borrow();
            let a = slot.as_ref().unwrap();
            return Err(format!(
                "Resident {label}: pending={}, ready={:?}, shutdown={:?}, temps={}, status={}",
                a.runtime.pending_configuration(),
                a.runtime.outputs_ready(),
                a.shutdown.phase(),
                a.runtime.temperature_samples().len(),
                a.status
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
pub(super) fn exercise(state: &State) -> Result<(), String> {
    use crate::app::{
        sensor_bindings::{Resolved, Scope, Target as SensorTarget},
        sensor_sessions::Resolver,
        sensor_worker::{Factory, Provider},
    };
    use std::sync::{Arc, atomic::AtomicUsize};
    struct Sample;
    impl Provider for Sample {
        fn poll_interval_ms(&self) -> u64 {
            100
        }
        fn poll(&mut self, now: u64) -> Result<Option<crate::sensors::Reading>, String> {
            Ok(Some(crate::sensors::Reading {
                celsius: 42.0,
                observed_ms: now,
            }))
        }
    }
    struct Fake(Arc<AtomicUsize>);
    impl Resolver for Fake {
        fn resolve(&mut self, _: &SensorTarget) -> Result<Factory, String> {
            let count = self.0.clone();
            Ok(Arc::new(move |_| {
                count.fetch_add(1, Ordering::Relaxed);
                Ok(Box::new(Sample))
            }))
        }
    }
    if !state.snapshot || state.resident.get() {
        return Err("Resident stand requires isolated diagnostic mode".into());
    }
    let (original, prior_plan, plan, page, dirty, registry) = {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        if a.native_lighting.is_some() || a.sources.native_allowed || a.sources.lighting.is_some() {
            return Err("Resident stand forbids native output/source capabilities".into());
        }
        let original = a.runtime.document().clone();
        let prior_plan = a.runtime.output_plan();
        let page = a.page;
        let dirty = a.dirty;
        let mut doc = original.clone();
        doc.lighting.setup = Setup {
            version: 1,
            devices: vec![crate::devices::Device {
                id: "resident-rgb".into(),
                label: "Resident software RGB".into(),
                role: crate::devices::Role::Canvas,
                driver: crate::devices::Driver::WledDdp,
                leds: 1,
                rate_hz: 25,
                enabled: true,
            }],
            canvas: crate::devices::Canvas {
                width: 1,
                height: 1,
                points: vec![crate::devices::Point {
                    device: "resident-rgb".into(),
                    led: 0,
                    x: 0,
                    y: 0,
                }],
            },
        };
        doc.lighting.groups = Groups {
            pc: vec!["resident-rgb".into()],
            door: vec![],
        };
        doc.lcd.clear();
        doc.ambient = Default::default();
        let mut plan = super::super::stand::prepare(&doc, &a.asset_root, &a.feed)?;
        plan.rgb[0].1 = vec!["resident-ui-fixture:rgb".into()];
        doc.temperatures.sources.push(crate::app::sensors::Source {
            key: "resident-temp".into(),
            value: crate::app::sensors::Value::Sensor {
                input: "resident-input".into(),
            },
        });
        a.runtime.request_outputs_for(doc, plan.clone())?;
        (
            original,
            prior_plan,
            plan,
            page,
            dirty,
            a.sources.registry.clone(),
        )
    };
    state.resident.set(true);
    let shell = Shell::create(state, false)?;
    settle(state, "initial-output", |a| {
        !a.runtime.pending_configuration() && a.runtime.outputs_ready() == Ok(true)
    })?;
    let starts = Arc::new(AtomicUsize::new(0));
    let subscription = {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        let scope = Scope::lighting("resident-ui-stand", a.runtime.document())?;
        let route = Resolved {
            input: "resident-input".into(),
            source_id: "resident-fake".into(),
            source_revision: 1,
            target: SensorTarget::FanControl {
                pipe_name: r"\\.\pipe\resident-fixture-no-server".into(),
                expected_server: r"C:\resident-fixture-no-server\FanControl.exe".into(),
                sensor_id: "cpu".into(),
            },
        };
        let token = Arc::new(registry.lock().unwrap().subscribe(
            scope,
            vec![route],
            &mut Fake(starts.clone()),
        )?);
        a.sources.lighting = Some(token.clone());
        a.page = Page::Scene;
        a.reset_editor(0);
        a.runtime.set_group(Target::Pc, true)?;
        a.runtime.set_group(Target::Door, true)?;
        token
    };
    settle(state, "first-sensor", |a| {
        a.runtime
            .temperature_samples()
            .iter()
            .any(|s| s.key == "resident-temp" && s.celsius == 42.0)
    })?;
    let (revision, ticks, sensor_generation, rgb_before, rgb_id) = {
        let slot = state.app.borrow();
        let a = slot.as_ref().unwrap();
        let rgb_id = plan
            .rgb
            .first()
            .map(|(id, _, _)| id.clone())
            .ok_or("Resident fixture needs a software RGB output")?;
        let sent = a
            .runtime
            .read(|r| r.host().status(&rgb_id).map(|s| s.submitted))
            .ok_or("Software RGB output missing")?;
        (
            a.runtime.output_revision(),
            a.runtime.owner_ticks().unwrap(),
            registry.lock().unwrap().status()[0].worker.generation,
            sent,
            rgb_id,
        )
    };
    for _ in 0..2 {
        close_editor(state)?;
        if !state.editor_window.get().is_null() {
            return Err("Close retained editor HWND".into());
        }
        if crate::runtime::output::Lease::acquire(vec!["resident-ui-fixture:rgb".into()]).is_ok() {
            return Err("Editor close released the active RGB endpoint lease".into());
        }

        {
            let slot = state.app.borrow();
            let a = slot.as_ref().unwrap();
            if !a.widgets.is_empty()
                || a.buffer.is_some()
                || !a.lcd_image.is_empty()
                || !a.ambient_image.is_empty()
            {
                return Err("Editor surface was retained after destruction".into());
            }
        }
        let end = Instant::now() + std::time::Duration::from_millis(180);
        while Instant::now() < end {
            maintain(state)?;
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        if !subscription.active()
            || starts.load(Ordering::Relaxed) != 1
            || registry.lock().unwrap().status()[0].worker.generation != sensor_generation
        {
            return Err("Editor close restarted/revoked shared sensor ownership".into());
        }
        open_editor(state)?;
        let slot = state.app.borrow();
        let a = slot.as_ref().unwrap();
        if a.runtime.output_revision() != revision
            || !Arc::ptr_eq(&a.sources.registry, &registry)
            || a.runtime.owner_ticks().unwrap() <= ticks
            || !a
                .runtime
                .temperature_samples()
                .iter()
                .any(|s| s.key == "resident-temp")
        {
            return Err("Reopen replaced Runtime or lost read-only source state".into());
        }
    }
    {
        let slot = state.app.borrow();
        let a = slot.as_ref().unwrap();
        if a.runtime
            .read(|r| r.host().status(&rgb_id).unwrap().submitted)
            <= rgb_before
        {
            return Err("RGB output stopped when editor was absent".into());
        }
    }
    let pending_revision = {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        let mut next = a.runtime.document().clone();
        next.lighting.scene.brightness = if next.lighting.scene.brightness == 60 {
            59
        } else {
            60
        };
        let next_plan = a.runtime.output_plan();
        let revision = a.runtime.request_outputs_for(next, next_plan)?;
        a.was_pending = true;
        revision
    };
    close_editor(state)?;
    settle(state, "closed-editor-transaction", |a| {
        !a.runtime.pending_configuration()
            && !a.was_pending
            && a.runtime.output_revision() == pending_revision
            && a.runtime.outputs_ready() == Ok(true)
    })?;
    if !subscription.active() || starts.load(Ordering::Relaxed) != 1 {
        return Err(
            "Window-free output transaction revoked/restarted unrelated sensor ownership".into(),
        );
    }
    open_editor(state)?;
    // Simulate only our shell's power messages; never call OS power APIs.
    unsafe {
        SendMessageW(shell.hwnd, WM_POWERBROADCAST, PBT_APMSUSPEND as usize, 0);
        SendMessageW(
            shell.hwnd,
            WM_POWERBROADCAST,
            PBT_APMRESUMEAUTOMATIC as usize,
            0,
        );
    }
    settle(state, "simulated-resume", |a| {
        a.runtime.outputs_ready() == Ok(true)
    })?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        let doc = a.runtime.document().clone();
        let outputs = a.runtime.output_plan();
        a.runtime.request_outputs_for(doc, outputs)?;
        a.was_pending = true;
    }
    close_editor(state)?;
    request(state, Action::Stop, false)?;
    settle(state, "stop", |a| {
        state.session_action.get() == Action::None && !a.shutdown.active()
    })?;
    let idle_until = Instant::now() + std::time::Duration::from_millis(100);
    while Instant::now() < idle_until {
        maintain(state)?;
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    {
        let slot = state.app.borrow();
        let a = slot.as_ref().unwrap();
        if a.was_pending
            || subscription.active()
            || !a.runtime.output_plan().rgb.is_empty()
            || !a.runtime.output_plan().lcd.is_empty()
            || a.runtime.output_plan().capture.is_some()
            || a.native_lighting.is_some()
        {
            return Err("Tray Stop retained outputs/subscription".into());
        }
    }
    if crate::runtime::output::Lease::acquire(vec!["resident-ui-fixture:rgb".into()]).is_err() {
        return Err("Tray Stop did not release the drained RGB endpoint lease".into());
    }
    open_editor(state)?;
    // Exit must wait for drain, destroy the editor and post quit only on this UI thread.
    request(state, Action::Exit, false)?;
    settle(state, "exit", |a| {
        a.shutdown.phase() == crate::app::shutdown::Phase::Ready
            && state.editor_window.get().is_null()
    })?;
    // WM_QUIT is generated at low priority. Drain this stand thread's own queue,
    // just as GetMessage does in ordinary mode, rather than filtering out pending paints.
    let end = Instant::now() + std::time::Duration::from_secs(1);
    loop {
        let mut message = MSG::default();
        if unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } != 0 {
            if message.message == WM_QUIT {
                break;
            }
            if message.hwnd == shell.hwnd {
                unsafe {
                    DispatchMessageW(&message);
                }
            }
        } else {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if Instant::now() >= end {
            return Err("Session Exit did not post quit after drain".into());
        }
    }
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        finish_stop(a)?;
        a.runtime
            .request_outputs_for(original.clone(), prior_plan)?;
        a.page = page;
        a.dirty = dirty;
    }
    state.session_action.set(Action::None);
    settle(state, "restore", |a| {
        !a.runtime.pending_configuration() && a.runtime.outputs_ready() == Ok(true)
    })?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        a.reset_editor(0);
        if a.runtime.document() != &original {
            return Err("Resident stand did not restore original profile".into());
        }
    }
    open_editor(state)?;
    drop(shell);
    state.tray_enabled.set(false);
    state.resident.set(false);
    eprintln!(
        "Resident session stand PASS: owned hidden top-level shell, editor destroy/reopen twice, same Runtime/output revision/Registry, one shared reader, live software RGB while editor absent, simulated shell power events, Stop drain, Exit drain and quit; no tray icon, native hardware or OS power operations"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn instance_is_exclusive_for_same_state_and_released_by_the_handle() {
        let dir = std::env::temp_dir().join(format!("prismestra-instance-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = Instance::acquire(&dir).unwrap();
        assert!(Instance::acquire(&dir).is_err());
        drop(first);
        assert!(Instance::acquire(&dir).is_ok());
        // The filename may remain; only the OS handle determines ownership.
        assert!(Instance::acquire(&dir).is_ok());
    }
}
