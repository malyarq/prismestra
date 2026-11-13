//! Native editor and persistent session. Hardware activation is an explicit user action.
use super::{CONTENT_LEFT, MIN_HEIGHT, MIN_WIDTH, Page, View, paint::Painter, right_panel};
use crate::{
    app::{
        config::{Groups, Project},
        document::Document,
        runtime::Runtime,
    },
    devices::Setup,
    lighting::scene::Effect,
    runtime::scene::{NoisePalette, NoiseSettings, Scene},
    schedule::{self, Target},
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    ffi::OsString,
    fs::File,
    io::Read,
    mem::size_of,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    rc::Rc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::Dialogs::*,
        Controls::*,
        HiDpi::*,
        Input::KeyboardAndMouse::{EnableWindow, IsWindowEnabled},
        WindowsAndMessaging::*,
    },
};
const TBM_GETPOS: u32 = WM_USER;
mod brand_icon;
mod connections;
mod modern;
mod native_forms;
mod resident;
mod runtime_client;
mod web_shell;
const MODERN_RETURN: i32 = 109;
mod sources;
use runtime_client::Client;
const CLASS: &str = "PrismestraWindow";
const NAV_SCENE: i32 = 101;
const NAV_DEVICES: i32 = 102;
const NAV_SCHEDULE: i32 = 103;
const NAV_AMBIENT: i32 = 104;
const NAV_LCD: i32 = 105;
const NAV_TEMPS: i32 = 106;
const NAV_SOURCES: i32 = 108;
const NAV_CONNECTIONS: i32 = 114;
const MODAL_COMMANDS: &[i32] = &[
    OPEN,
    SAVE,
    EXPORT,
    COLOR_EDIT,
    E_FILE,
    sources::MODULE_SELECT,
];
const E_TARGET: i32 = 701;
const E_LIST: i32 = 702;
const E_HINT: i32 = 703;
const E_VALUE: i32 = 704;
const E_CHOICE: i32 = 705;
const E_SET: i32 = 706;
const E_APPLY: i32 = 707;
const E_RESET: i32 = 708;
const E_RETRY: i32 = 709;
const E_FILE: i32 = 710;
const E_ADD: i32 = 711;
const E_REMOVE: i32 = 712;
const E_TEST: i32 = 713;
const E_PRESET: i32 = 714;
const E_NEW_LABEL: i32 = 715;
const E_LED_COUNT: i32 = 716;
const E_LED_LABEL: i32 = 717;
const E_ROUTE_DEVICE: i32 = 718;
const E_ROUTE_SOURCE: i32 = 719;
const OPEN: i32 = 110;
const SAVE: i32 = 111;
const EXPORT: i32 = 112;
const EFFECT: i32 = 201;
const BRIGHT: i32 = 202;
const SPEED: i32 = 203;
const FPS: i32 = 204;
const COLOR_INDEX: i32 = 205;
const COLOR_EDIT: i32 = 206;
const PC_ON: i32 = 207;
const DOOR_ON: i32 = 208;
const SCHED_ON: i32 = 401;
const SCHED_PAUSE: i32 = 402;
const TIME_PC: i32 = 403;
const TIME_DOOR: i32 = 404;
const TIME_OFF: i32 = 405;
const TIMEZONE: i32 = 406;
const APPLY_SCHED: i32 = 407;
const FIRST_LABEL: i32 = 600;
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn last(action: &str) -> String {
    format!("{action}: {}", std::io::Error::last_os_error())
}
fn px(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi) + 48) / 96) as i32
}
fn wall() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs().min(i64::MAX as u64) as i64)
}
#[derive(Default)]
struct Options {
    native_lighting: Option<PathBuf>,
    start_connected: bool,
    project: Option<PathBuf>,
    setup: Option<PathBuf>,
    scene: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    snapshot: Option<PathBuf>,
    dpi: Option<u32>,
    size: Option<(i32, i32)>,
    page: Option<Page>,
    allow_sensor_discovery: bool,
    source_discovery_page: bool,
    selftest: bool,
    classic_editor: bool,
}
fn options(args: Vec<OsString>) -> Result<Options, String> {
    let mut o = Options::default();
    let mut it = args.into_iter();
    while let Some(key) = it.next() {
        let key = key.to_str().ok_or("Invalid option encoding")?;
        if key == "--start-connected" {
            if o.start_connected {
                return Err("Duplicate start-connected option".into());
            }
            o.start_connected = true;
            continue;
        }
        if key == "--classic-editor" {
            o.classic_editor = true;
            continue;
        }
        if key == "--selftest" {
            o.selftest = true;
            continue;
        }

        if key == "--allow-sensor-discovery" {
            o.allow_sensor_discovery = true;
            continue;
        }
        let value = it.next().ok_or_else(|| format!("Missing value for{key}"))?;
        if value.to_str().is_some_and(|v| v.starts_with("--")) {
            return Err(format!("Missing value for{key}"));
        }
        match key {
            "--native-lighting" => {
                if o.native_lighting.is_some() {
                    return Err("Duplicate native lighting option".into());
                }
                o.native_lighting = Some(value.into());
            }
            "--project" => o.project = Some(value.into()),
            "--setup" => o.setup = Some(value.into()),
            "--scene" => o.scene = Some(value.into()),
            "--state-dir" => o.state_dir = Some(value.into()),
            "--snapshot" => o.snapshot = Some(value.into()),
            "--dpi" => {
                let dpi = value
                    .to_str()
                    .ok_or("Invalid DPI")?
                    .parse::<u32>()
                    .map_err(|_| "Invalid DPI")?;
                if !(96..=384).contains(&dpi) {
                    return Err("DPI must be96..384".into());
                }
                o.dpi = Some(dpi);
            }
            "--size" => {
                let text = value.to_str().ok_or("Invalid size")?;
                let (w, h) = text.split_once('x').ok_or("ExpectedWIDTHxHEIGHT")?;
                let (w, h) = (
                    w.parse::<i32>().map_err(|_| "Invalid width")?,
                    h.parse::<i32>().map_err(|_| "Invalid height")?,
                );
                if !(960..=4096).contains(&w) || !(640..=2160).contains(&h) {
                    return Err("Preview size outside960..4096x640..2160DIP".into());
                }
                o.size = Some((w, h));
            }

            "--page" => {
                o.page = Some(match value.to_str() {
                    Some("scene") => Page::Scene,
                    Some("devices") => Page::Devices,
                    Some("schedule") => Page::Schedule,
                    Some("ambient") => Page::Ambient,
                    Some("lcd") => Page::Lcd,
                    Some("temperatures") => Page::Temperatures,
                    Some("sources") => Page::Sources,
                    Some("source-discovery") => {
                        o.source_discovery_page = true;
                        Page::Sources
                    }
                    Some("connections") => Page::Connections,
                    _ => {
                        return Err(
                            "Page must be scene/devices/connections/schedule/ambient/lcd/temperatures/sources/source-discovery"
                                .into(),
                        );
                    }
                })
            }
            _ => {
                return Err(format!("Unknown option{key}"));
            }
        }
    }
    if o.project.is_some() && (o.setup.is_some() || o.scene.is_some()) {
        return Err("Useproject ORsetup+scene".into());
    }
    if o.start_connected && (o.project.is_none() || o.native_lighting.is_some()) {
        return Err("Use --start-connected with --project and saved local connections".into());
    }
    if (o.native_lighting.is_some() || o.start_connected) && (o.selftest || o.snapshot.is_some()) {
        return Err("Native lighting is forbidden in snapshots and selftests".into());
    }
    if o.allow_sensor_discovery && (o.selftest || o.snapshot.is_some()) {
        return Err("Native discovery is forbidden in snapshots and selftests".into());
    }

    Ok(o)
}
fn journal_path(root: &Path, profile: Option<&Path>, document: &Document) -> PathBuf {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    if let Some(profile) = profile {
        let path = std::fs::canonicalize(profile).unwrap_or_else(|_| profile.to_path_buf());
        let name = path.to_string_lossy().to_lowercase();
        digest.update(name.as_bytes());
    }
    digest.update([0]);
    digest.update(serde_json::to_vec(&document.lighting.setup).expect("serializable setup"));
    digest.update(serde_json::to_vec(&document.lighting.groups).expect("serializable groups"));
    let hash = digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    root.join(format!("runtime-{hash}.json"))
}
fn wrap_words(text: &str, fits: impl Fn(&str) -> bool) -> String {
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if line.is_empty() {
                word.into()
            } else {
                format!("{line} {word}")
            };
            if !line.is_empty() && !fits(&candidate) {
                lines.push(std::mem::take(&mut line));
                line.push_str(word);
            } else {
                line = candidate;
            }
        }
        lines.push(line);
    }
    lines.join("\r\n")
}
fn rates(current: u32) -> Vec<u32> {
    let mut values = vec![15, 25, 30, 48, 60, 144];
    if !values.contains(&current) {
        values.push(current);
        values.sort_unstable();
    }
    values
}
fn default_scene() -> Scene {
    Scene {
        effect: Effect::NoiseMap,
        colors: vec![0x120100, 0x480500, 0x961801, 0xe64005, 0xff7814],
        brightness: 60,
        speed: 8,
        fps: 25,
        temperature: 0,
        tint: 0,
        noise: NoiseSettings {
            frequency: 0.055,
            amplitude: 1.0,
            lacunarity: 0.75,
            persistence: 0.4,
            octaves: 2,
            motion: 0,
            motion_speed: 1,
        },
        noise_palette: NoisePalette::Custom,
        rainbow_width: 10,
    }
}
fn load_project(path: &Path) -> Result<(Project, Option<Document>), String> {
    match Document::load(path) {
        Ok(document) => Ok((document.lighting.clone(), Some(document))),
        Err(document_error) => Project::load(path)
            .map(|p| (p, None))
            .map_err(|project_error| {
                format!("Неверный документ: {document_error}; {project_error}")
            }),
    }
}
fn load_profile(path: &Path, asset_root: &Path) -> Result<crate::app::bundle::Profile, String> {
    let mut prefix = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(4)
        .read_to_end(&mut prefix)
        .map_err(|e| e.to_string())?;
    if prefix == b"PK\x03\x04"
        || path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
    {
        let loaded = crate::app::bundle::load(path)?;
        let store = crate::app::assets::Store::new(asset_root.to_owned())?;
        loaded.install(&store)
    } else {
        let (p, document) = load_project(path)?;
        match document {
            Some(d) => Ok(crate::app::bundle::Profile { document: d }),
            None => Ok(crate::app::bundle::Profile {
                document: Document::from_lighting(p)?,
            }),
        }
    }
}
#[cfg(test)]
fn save_project(path: &Path, project: &Project, document: Option<&Document>) -> Result<(), String> {
    if let Some(document) = document {
        let mut next = document.clone();
        next.lighting = project.clone();
        next.save(path)
    } else {
        project.save(path)
    }
}
fn project(o: &Options) -> Result<(Project, Option<Document>), String> {
    if let Some(path) = &o.project {
        return load_project(path);
    }
    let setup = if let Some(path) = &o.setup {
        Setup::load(path)?
    } else {
        Setup {
            version: 1,
            devices: vec![],
            canvas: crate::devices::Canvas {
                width: 140,
                height: 80,
                points: vec![],
            },
        }
    };
    let scene = if let Some(path) = &o.scene {
        let mut bytes = vec![];
        File::open(path)
            .map_err(|e| e.to_string())?
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err("Scene exceeds4MiB".into());
        }
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())?
    } else {
        default_scene()
    };
    let mut groups = Groups {
        pc: vec![],
        door: vec![],
    };
    for d in &setup.devices {
        if matches!(
            d.driver,
            crate::devices::Driver::DoorBle | crate::devices::Driver::SurplifeE1Pixels
        ) {
            groups.door.push(d.id.clone());
        } else {
            groups.pc.push(d.id.clone());
        }
    }
    let p = Project {
        version: 1,
        setup,
        scene,
        schedule: schedule::Config::default(),
        groups,
    };
    p.validate()?;
    Ok((p, None))
}
struct Bitmap {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    width: i32,
    height: i32,
}
impl Bitmap {
    fn new(width: i32, height: i32) -> Result<Self, String> {
        if width <= 0 || height <= 0 || i64::from(width) * i64::from(height) > 32 * 1024 * 1024 {
            return Err("UI buffer size outside bounds".into());
        }
        let dc = unsafe { CreateCompatibleDC(null_mut()) };
        if dc.is_null() {
            return Err(last("Create UI buffer DC"));
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = null_mut();
        let bitmap =
            unsafe { CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0) };
        if bitmap.is_null() || bits.is_null() {
            unsafe {
                DeleteDC(dc);
            }
            return Err(last("Create UI bitmap"));
        }
        let old = unsafe { SelectObject(dc, bitmap) };
        if old.is_null() || old as isize == -1 {
            unsafe {
                DeleteObject(bitmap);
                DeleteDC(dc);
            }
            return Err(last("Select UI bitmap"));
        }
        Ok(Self {
            dc,
            bitmap,
            old,
            bits: bits.cast(),
            width,
            height,
        })
    }
    fn save(&self, path: &Path) -> Result<(), String> {
        if unsafe { GdiFlush() } == 0 {
            return Err(last("Finish snapshot drawing"));
        }
        if path.exists() {
            return Err("Snapshot output already exists".into());
        }
        let raw = unsafe {
            std::slice::from_raw_parts(self.bits, self.width as usize * self.height as usize * 4)
        };
        let mut image = image::RgbImage::new(self.width as u32, self.height as u32);
        for (p, b) in image.pixels_mut().zip(raw.as_chunks::<4>().0) {
            *p = image::Rgb([b[2], b[1], b[0]]);
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        image
            .write_to(&mut file, image::ImageFormat::Png)
            .map_err(|e| e.to_string())
    }
}
impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}
struct App {
    web_mode: bool,
    connections: connections::Editor,
    native_lighting: Option<crate::app::lighting_outputs::Local>,
    sources: sources::Editor,
    runtime: Client,
    asset_root: PathBuf,
    feed: super::stand::Feed,
    editor: Option<super::editor::Editor>,
    editor_dirty: bool,
    detail: String,
    was_pending: bool,
    context_revision: u64,
    lcd_image: Vec<u8>,
    lcd_image_size: (u32, u32),
    lcd_frame: (u64, u64),
    ambient_image: Vec<crate::lighting::color::Rgb>,
    ambient_frame: (u64, u64),
    last_status: Instant,
    pending_open: Option<(Runtime, PathBuf)>,
    shutdown: crate::app::shutdown::Shutdown,
    painter: Painter,
    widgets: BTreeMap<i32, HWND>,
    page: Page,
    dpi: u32,
    width: i32,
    height: i32,
    buffer: Option<Bitmap>,
    dirty: bool,
    status: String,
    path: Option<PathBuf>,
    state_dir: PathBuf,
    groups: [bool; 2],
}
impl App {
    fn attach_outputs(&mut self) -> Result<(), String> {
        if let Some(local) = &self.native_lighting {
            self.runtime
                .with(|r, _| local.attach_missing(r, &self.asset_root))
        } else {
            self.runtime
                .with(|r, _| super::stand::attach_missing(r, &self.asset_root, &self.feed))
        }
    }
    fn view(&self) -> View<'_> {
        View {
            native_lighting: self.native_lighting.is_some(),
            project: self.runtime.project(),
            colors: self.runtime.colors(),
            page: self.page,
            status: &self.status,
            dirty: self.dirty,
            pc_on: self.runtime.group_on(Target::Pc),
            door_on: self.runtime.group_on(Target::Door),
            detail: if self.page == Page::Connections {
                "Локальные привязки хранятся отдельно от профиля. Сохранение не включает устройства."
            } else if self.page == Page::Sources {
                "Локальные датчики · сохранение настроек не запускает оборудование"
            } else {
                &self.detail
            },
            lcd_preview: if self.lcd_image.is_empty() {
                None
            } else {
                Some(super::Surface {
                    width: self.lcd_image_size.0,
                    height: self.lcd_image_size.1,
                    bgra: &self.lcd_image,
                })
            },
            ambient_colors: (!self.ambient_image.is_empty())
                .then_some(self.ambient_image.as_slice()),
            lcd_hint: match self
                .runtime
                .document()
                .lcd
                .get(self.editor.as_ref().map_or(0, |e| e.index))
            {
                Some(entry) if entry.config.media.is_none() => "Выберите анимацию",
                Some(entry) if !entry.config.enabled => "Включите воспроизведение",
                Some(entry)
                    if self
                        .runtime
                        .lcd_status(&entry.device)
                        .is_some_and(|s| s.error.is_some()) =>
                {
                    "Нет кадра.\nПроверьте файл."
                }
                Some(_) => "Подготовка кадра…",
                None => "LCD не добавлен",
            },
        }
    }
    fn combo_index(&self, id: i32) -> Option<usize> {
        let index = unsafe { SendMessageW(self.h(id), CB_GETCURSEL, 0, 0) };
        (index >= 0).then_some(index as usize)
    }
    fn control(
        &mut self,
        parent: HWND,
        id: i32,
        class: &str,
        text: &str,
        style: u32,
    ) -> Result<(), String> {
        if self.widgets.contains_key(&id) {
            return Err(format!("Duplicate native control ID: {id}"));
        }
        let handle = unsafe {
            CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | style,
                0,
                0,
                10,
                10,
                parent,
                id as usize as HMENU,
                GetModuleHandleW(null()),
                null(),
            )
        };
        if handle.is_null() {
            return Err(last("Create native control"));
        }
        unsafe {
            SendMessageW(handle, WM_SETFONT, self.painter.body_font() as usize, 1);
        }
        self.widgets.insert(id, handle);
        Ok(())
    }
    fn build(&mut self, parent: HWND) -> Result<(), String> {
        self.control(
            parent,
            MODERN_RETURN,
            "BUTTON",
            "Новый интерфейс",
            BS_PUSHBUTTON as u32,
        )?;
        for (id, text) in [
            (NAV_SCENE, "Сцена"),
            (NAV_DEVICES, "Устройства"),
            (NAV_CONNECTIONS, "Подключения"),
            (NAV_SCHEDULE, "Расписание"),
            (NAV_AMBIENT, "Подсветка экрана"),
            (NAV_LCD, "LCD"),
            (NAV_TEMPS, "Температуры"),
            (NAV_SOURCES, "Датчики"),
        ] {
            self.control(
                parent,
                id,
                "BUTTON",
                text,
                BS_AUTORADIOBUTTON as u32
                    | BS_PUSHLIKE as u32
                    | if id == NAV_SCENE { WS_GROUP } else { 0 },
            )?;
        }
        self.control(parent, OPEN, "BUTTON", "Открыть…", BS_PUSHBUTTON as u32)?;
        self.control(parent, SAVE, "BUTTON", "Сохранить…", BS_PUSHBUTTON as u32)?;
        self.control(
            parent,
            EXPORT,
            "BUTTON",
            "Экспорт ZIP…",
            BS_PUSHBUTTON as u32,
        )?;
        self.control(
            parent,
            EFFECT,
            "COMBOBOX",
            "",
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
        )?;
        for s in ["Мягкие угли / шум", "Дыхание", "Радуга"] {
            self.add(EFFECT, s);
        }
        self.control(
            parent,
            BRIGHT,
            "msctls_trackbar32",
            "",
            TBS_HORZ | TBS_NOTICKS,
        )?;
        self.control(
            parent,
            SPEED,
            "msctls_trackbar32",
            "",
            TBS_HORZ | TBS_NOTICKS,
        )?;
        self.control(
            parent,
            FPS,
            "COMBOBOX",
            "",
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
        )?;
        for n in [15, 25, 30, 48, 60, 144] {
            self.add(FPS, &format!("{n} кадров/с"));
        }
        self.control(
            parent,
            COLOR_INDEX,
            "COMBOBOX",
            "",
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
        )?;
        self.control(
            parent,
            COLOR_EDIT,
            "BUTTON",
            "Изменить…",
            BS_PUSHBUTTON as u32,
        )?;
        self.control(
            parent,
            PC_ON,
            "BUTTON",
            if self.native_lighting.is_some() {
                "Подсветка ПК"
            } else {
                "ПК в предпросмотре"
            },
            BS_AUTOCHECKBOX as u32,
        )?;
        self.control(
            parent,
            DOOR_ON,
            "BUTTON",
            if self.native_lighting.is_some() {
                "Лента у двери"
            } else {
                "Лента в предпросмотре"
            },
            BS_AUTOCHECKBOX as u32,
        )?;
        self.control(
            parent,
            SCHED_ON,
            "BUTTON",
            if self.native_lighting.is_some() {
                "Включить расписание подсветки"
            } else {
                "Включить расписание предпросмотра"
            },
            BS_AUTOCHECKBOX as u32,
        )?;
        self.control(
            parent,
            SCHED_PAUSE,
            "BUTTON",
            "Приостановить расписание",
            BS_AUTOCHECKBOX as u32,
        )?;
        for (id, text) in [
            (TIME_PC, "10:00"),
            (TIME_DOOR, "17:00"),
            (TIME_OFF, "02:00"),
            (TIMEZONE, "Europe/Moscow"),
        ] {
            self.control(parent, id, "EDIT", text, WS_BORDER | ES_AUTOHSCROLL as u32)?;
        }
        self.control(
            parent,
            APPLY_SCHED,
            "BUTTON",
            if self.native_lighting.is_some() {
                "Применить к устройствам"
            } else {
                "Применить к предпросмотру"
            },
            BS_PUSHBUTTON as u32,
        )?;
        for (i, text) in [
            "Режим",
            "Яркость",
            "Скорость",
            "Частота",
            "Цвет палитры",
            "ПК включается",
            "Лента включается",
            "Всё выключается",
            "Часовой пояс",
        ]
        .into_iter()
        .enumerate()
        {
            self.control(parent, FIRST_LABEL + i as i32, "STATIC", text, 0)?;
            unsafe {
                SetWindowLongPtrW(
                    self.h(FIRST_LABEL + i as i32),
                    GWL_STYLE,
                    GetWindowLongPtrW(self.h(FIRST_LABEL + i as i32), GWL_STYLE)
                        & !(WS_TABSTOP as isize),
                );
            }
        }
        self.build_editor(parent)?;
        self.build_sources(parent)?;
        self.build_connections(parent)?;
        self.sync();
        Ok(())
    }
    fn h(&self, id: i32) -> HWND {
        if self.widgets.is_empty() {
            null_mut()
        } else {
            self.widgets[&id]
        }
    }
    fn add(&self, id: i32, text: &str) {
        unsafe {
            SendMessageW(self.h(id), CB_ADDSTRING, 0, wide(text).as_ptr() as isize);
        }
    }
    fn set_text(&self, id: i32, text: &str) {
        unsafe {
            SetWindowTextW(self.h(id), wide(text).as_ptr());
        }
    }
    fn checked(&self, id: i32) -> bool {
        unsafe { SendMessageW(self.h(id), BM_GETCHECK, 0, 0) == BST_CHECKED as isize }
    }
    fn set_check(&self, id: i32, checked: bool) {
        unsafe {
            SendMessageW(
                self.h(id),
                BM_SETCHECK,
                if checked { BST_CHECKED } else { BST_UNCHECKED } as usize,
                0,
            );
        }
    }
    fn text(&self, id: i32) -> String {
        let mut b = [0u16; 512];
        let count = unsafe { GetWindowTextW(self.h(id), b.as_mut_ptr(), b.len() as i32) };
        String::from_utf16_lossy(&b[..count.max(0) as usize])
    }
    fn sync(&mut self) {
        let p = self.runtime.project();
        let index = match p.scene.effect {
            Effect::NoiseMap => 0,
            Effect::Breathing => 1,
            Effect::RainbowWave => 2,
        };
        unsafe {
            SendMessageW(self.h(EFFECT), CB_SETCURSEL, index, 0);
            SendMessageW(self.h(BRIGHT), TBM_SETRANGE, 0, 100 << 16);
            SendMessageW(self.h(BRIGHT), TBM_SETPOS, 1, p.scene.brightness as isize);
            SendMessageW(self.h(SPEED), TBM_SETRANGE, 0, 100 << 16);
            SendMessageW(
                self.h(SPEED),
                TBM_SETPOS,
                1,
                p.scene.effect.normalized_speed(p.scene.speed).unwrap_or(0) as isize,
            );
        }
        self.set_text(
            FIRST_LABEL + 1,
            &format!("Яркость · {}%", p.scene.brightness),
        );
        self.set_text(FIRST_LABEL + 2, &format!("Скорость · {}", p.scene.speed));
        let rates = rates(p.scene.fps);
        let color_index = unsafe { SendMessageW(self.h(COLOR_INDEX), CB_GETCURSEL, 0, 0) }.max(0);
        unsafe {
            SendMessageW(self.h(FPS), CB_RESETCONTENT, 0, 0);
        }
        for n in &rates {
            self.add(FPS, &format!("{n} кадров/с"));
        }
        unsafe {
            SendMessageW(
                self.h(FPS),
                CB_SETCURSEL,
                rates.iter().position(|n| *n == p.scene.fps).unwrap_or(1),
                0,
            );
            SendMessageW(self.h(COLOR_INDEX), CB_RESETCONTENT, 0, 0);
        }
        for (i, c) in p.scene.colors.iter().enumerate() {
            self.add(COLOR_INDEX, &format!("Цвет {} · #{c:06X}", i + 1));
        }
        unsafe {
            SendMessageW(
                self.h(COLOR_INDEX),
                CB_SETCURSEL,
                (color_index as usize).min(p.scene.colors.len() - 1),
                0,
            );
        }
        self.groups = [
            self.runtime.group_on(Target::Pc),
            self.runtime.group_on(Target::Door),
        ];
        self.set_check(PC_ON, self.groups[0]);
        self.set_check(DOOR_ON, self.groups[1]);
        self.set_check(SCHED_ON, p.schedule.enabled);
        self.set_check(SCHED_PAUSE, p.schedule.paused);
        for (id, minute) in [TIME_PC, TIME_DOOR, TIME_OFF]
            .into_iter()
            .zip(p.schedule.times)
        {
            self.set_text(id, &format!("{:02}:{:02}", minute / 60, minute % 60));
        }
        self.set_text(TIMEZONE, &p.schedule.timezone);
        self.refresh_devices();
    }
    fn pos(&self, id: i32, x: i32, y: i32, w: i32, h: i32) {
        unsafe {
            MoveWindow(
                self.h(id),
                px(x, self.dpi),
                px(y, self.dpi),
                px(w, self.dpi),
                px(h, self.dpi),
                1,
            );
        }
    }
    fn layout(&mut self, width: i32, height: i32) {
        self.width = width;
        self.height = height;
        self.buffer = None;
        let (w, h) = (
            (i64::from(width) * 96 / i64::from(self.dpi)) as i32,
            (i64::from(height) * 96 / i64::from(self.dpi)) as i32,
        );
        for (id, y) in [
            (NAV_SCENE, 112),
            (NAV_DEVICES, 146),
            (NAV_CONNECTIONS, 180),
            (NAV_SCHEDULE, 214),
            (NAV_AMBIENT, 248),
            (NAV_LCD, 282),
            (NAV_TEMPS, 316),
            (NAV_SOURCES, 350),
        ] {
            self.pos(id, 16, y, 132, 32);
        }
        self.pos(MODERN_RETURN, 16, 72, 156, 32);
        self.pos(OPEN, 16, h - 132, 132, 32);
        self.pos(SAVE, 16, h - 92, 132, 32);
        self.pos(EXPORT, 16, h - 172, 132, 32);
        // Native list rows need breathing room independently of the font.
        // Reapply on DPI changes as well as the initial layout.
        unsafe {
            SendMessageW(
                self.h(E_LIST),
                LB_SETITEMHEIGHT,
                0,
                px(26, self.dpi) as isize,
            );
        }
        let r = right_panel(w);
        for (id, y) in [(EFFECT, 166), (FPS, 392), (COLOR_INDEX, 464)] {
            self.pos(id, r, y, if id == COLOR_INDEX { 144 } else { 280 }, 250);
        }
        self.pos(COLOR_EDIT, r + 152, 464, 128, 30);
        self.pos(BRIGHT, r, 244, 280, 28);
        self.pos(SPEED, r, 318, 280, 28);
        self.pos(PC_ON, r, 522, 280, 28);
        self.pos(DOOR_ON, r, 554, 280, 28);
        for (i, y) in [142, 220, 294, 368, 440].into_iter().enumerate() {
            self.pos(FIRST_LABEL + i as i32, r, y, 280, 22);
        }
        self.pos(SCHED_ON, CONTENT_LEFT, 190, 450, 28);
        self.pos(SCHED_PAUSE, CONTENT_LEFT, 228, 450, 28);
        for (i, id) in [TIME_PC, TIME_DOOR, TIME_OFF, TIMEZONE]
            .into_iter()
            .enumerate()
        {
            let y = 278 + i as i32 * 44;
            self.pos(FIRST_LABEL + 5 + i as i32, CONTENT_LEFT, y, 180, 24);
            self.pos(
                id,
                CONTENT_LEFT + 196,
                y,
                if id == TIMEZONE { 280 } else { 110 },
                28,
            );
        }
        self.pos(APPLY_SCHED, CONTENT_LEFT + 510, 254, 230, 34);
        self.editor_layout(w, h);
        self.sources_layout(w, h);
        self.connections_layout(w, h);
        self.show_page();
    }
    fn show_page(&self) {
        if self.web_mode {
            for hwnd in self.widgets.values() {
                unsafe {
                    ShowWindow(*hwnd, SW_HIDE);
                }
            }
            return;
        }
        for id in [
            NAV_SCENE,
            NAV_DEVICES,
            NAV_SCHEDULE,
            NAV_AMBIENT,
            NAV_LCD,
            NAV_TEMPS,
            NAV_SOURCES,
            NAV_CONNECTIONS,
        ] {
            self.set_check(
                id,
                (id == NAV_SCENE && self.page == Page::Scene)
                    || (id == NAV_DEVICES && self.page == Page::Devices)
                    || (id == NAV_SCHEDULE && self.page == Page::Schedule)
                    || (id == NAV_AMBIENT && self.page == Page::Ambient)
                    || (id == NAV_LCD && self.page == Page::Lcd)
                    || (id == NAV_TEMPS && self.page == Page::Temperatures)
                    || (id == NAV_SOURCES && self.page == Page::Sources)
                    || (id == NAV_CONNECTIONS && self.page == Page::Connections),
            );
        }
        for (id, hwnd) in &self.widgets {
            let show = if connections::is_control(*id) {
                self.connection_visible(*id)
            } else if sources::is_control(*id) {
                self.source_visible(*id)
            } else if (E_TARGET..=E_ROUTE_SOURCE).contains(id) {
                let settings = self.settings_page();
                settings
                    && match *id {
                        E_CHOICE => self.selected_field().is_some_and(|f| !f.choices.is_empty()),
                        E_VALUE => self.selected_field().is_none_or(|f| f.choices.is_empty()),
                        E_FILE => self.selected_field().is_some_and(|f| f.file),
                        E_ADD => true,
                        E_REMOVE => matches!(
                            self.page,
                            Page::Temperatures | Page::Devices | Page::Ambient
                        ),
                        E_NEW_LABEL => matches!(self.page, Page::Devices | Page::Ambient),
                        E_ROUTE_DEVICE | E_ROUTE_SOURCE => self.page == Page::Ambient,
                        E_PRESET | E_LED_COUNT | E_LED_LABEL => self.page == Page::Devices,
                        E_TEST => matches!(self.page, Page::Ambient | Page::Temperatures),
                        _ => true,
                    }
            } else if (FIRST_LABEL..FIRST_LABEL + 9).contains(id) {
                if *id < FIRST_LABEL + 5 {
                    self.page == Page::Scene
                } else {
                    self.page == Page::Schedule
                }
            } else if (200..300).contains(id) {
                self.page == Page::Scene
            } else if (400..500).contains(id) {
                self.page == Page::Schedule
            } else {
                true
            };
            unsafe {
                ShowWindow(*hwnd, if show { SW_SHOWNA } else { SW_HIDE });
            }
        }
        if self.shutdown.active() {
            for hwnd in self.widgets.values() {
                unsafe {
                    EnableWindow(*hwnd, 0);
                }
            }
        }
    }

    fn request_close(&mut self) {
        if !self.widgets.is_empty()
            && let Err(error) = self.guard_connections_close()
        {
            self.status = error;
            self.show_page();
            return;
        }
        if !self.widgets.is_empty()
            && let Err(error) = self.guard_sources_exit()
        {
            self.status = error;
            self.show_page();
            return;
        }

        self.begin_shutdown();
    }
    fn begin_shutdown(&mut self) {
        self.sources.release_lighting();
        self.connections.scan.cancel();
        self.sources.scan.cancel();
        if let Some((runtime, _)) = &mut self.pending_open {
            runtime.stop();
        }
        if let Err(error) = self.runtime.request_shutdown(&mut self.shutdown) {
            self.status = error;
            return;
        }
        self.status = self.shutdown.message();
        self.show_page();
    }
    fn close_ready(&mut self) -> bool {
        self.shutdown.phase() == crate::app::shutdown::Phase::Ready
            && self.runtime.drained()
            && self.sources.scan.is_idle()
            && self.connections.scan.is_idle()
            && self.sources.drained()
            && self.pending_open.as_mut().is_none_or(|(r, _)| r.drained())
    }
    fn build_editor(&mut self, parent: HWND) -> Result<(), String> {
        self.control(
            parent,
            E_TARGET,
            "COMBOBOX",
            "",
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
        )?;
        self.control(
            parent,
            E_LIST,
            "LISTBOX",
            "",
            WS_BORDER | WS_VSCROLL | LBS_NOTIFY as u32,
        )?;
        self.control(parent, E_HINT, "STATIC", "", 0)?;
        self.control(parent, E_NEW_LABEL, "STATIC", "Новое устройство", 0)?;
        for id in [E_ROUTE_DEVICE, E_ROUTE_SOURCE] {
            self.control(
                parent,
                id,
                "COMBOBOX",
                "",
                CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
            )?;
        }
        self.control(
            parent,
            E_PRESET,
            "COMBOBOX",
            "",
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
        )?;
        for preset in crate::app::device_setup::catalog() {
            self.add(E_PRESET, preset.label);
        }
        self.control(parent, E_LED_LABEL, "STATIC", "LED", 0)?;
        self.control(
            parent,
            E_LED_COUNT,
            "EDIT",
            "",
            WS_BORDER | ES_NUMBER as u32 | ES_AUTOHSCROLL as u32,
        )?;
        unsafe {
            SendMessageW(self.h(E_PRESET), CB_SETCURSEL, 0, 0);
        }
        self.control(
            parent,
            E_VALUE,
            "EDIT",
            "",
            WS_BORDER | ES_AUTOHSCROLL as u32,
        )?;
        self.control(
            parent,
            E_CHOICE,
            "COMBOBOX",
            "",
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
        )?;
        for (id, text) in [
            (E_SET, "Изменить"),
            (E_APPLY, "Применить"),
            (E_RESET, "Сбросить"),
            (E_RETRY, "Повторить"),
            (E_FILE, "Выбрать файл…"),
            (E_ADD, "Добавить"),
            (E_REMOVE, "Удалить"),
            (E_TEST, "Тестовый кадр"),
        ] {
            self.control(parent, id, "BUTTON", text, BS_PUSHBUTTON as u32)?;
        }
        unsafe {
            for id in [E_HINT, E_NEW_LABEL, E_LED_LABEL] {
                let h = self.h(id);
                SetWindowLongPtrW(
                    h,
                    GWL_STYLE,
                    GetWindowLongPtrW(h, GWL_STYLE) & !(WS_TABSTOP as isize),
                );
            }
        }
        self.reset_editor(0);
        Ok(())
    }
    fn settings_page(&self) -> bool {
        matches!(
            self.page,
            Page::Devices | Page::Ambient | Page::Lcd | Page::Temperatures
        )
    }
    fn reset_editor(&mut self, index: usize) {
        self.lcd_image.clear();
        self.ambient_image.clear();
        self.lcd_frame = (0, 0);
        self.ambient_frame = (0, 0);
        self.editor = if self.settings_page() {
            Some(super::editor::Editor {
                document: self.runtime.document().clone(),
                page: self.page,
                index,
            })
        } else {
            None
        };
        self.editor_dirty = false;
        self.refresh_editor(true);
    }
    fn selected_field(&self) -> Option<super::editor::Field> {
        let row = unsafe { SendMessageW(self.h(E_LIST), LB_GETCURSEL, 0, 0) };
        self.editor.as_ref()?.fields().get(row as usize).cloned()
    }
    fn refresh_editor(&mut self, targets: bool) {
        self.detail = match self.page {
            Page::Ambient if self.editor.as_ref().is_some_and(|e| e.document.ambient.routes.is_empty()
                && e.ambient_candidates().is_empty()) =>
                "Добавьте ленту в «Устройствах» и выберите назначение «От экрана».",
            Page::Ambient => "Источник здесь логический. Предпросмотр получает только явно включённый тестовый кадр.",
            Page::Lcd if self.editor.as_ref().is_some_and(|e| e.document.lcd.is_empty()) =>
                "Добавьте TT LCD кнопкой ниже, затем выберите анимацию.",
            Page::Lcd => "Анимация и панель температур. Передача выполняется в памяти, без доступа к помпе.",
            Page::Temperatures => "Логические источники. Тестовые показания задаются отдельной кнопкой и действуют 5 секунд.",
            _ => "",
        }.into();
        self.refresh_devices();
        if targets {
            if self.page == Page::Ambient {
                unsafe {
                    SendMessageW(self.h(E_ROUTE_DEVICE), CB_RESETCONTENT, 0, 0);
                    SendMessageW(self.h(E_ROUTE_SOURCE), CB_RESETCONTENT, 0, 0);
                }
                self.add(E_ROUTE_SOURCE, "Новый источник (без привязки)");
                if let Some(e) = &self.editor {
                    for (_, label) in e.ambient_candidates() {
                        self.add(E_ROUTE_DEVICE, &label);
                    }
                    for source in &e.document.ambient.sources {
                        self.add(E_ROUTE_SOURCE, &source.key);
                    }
                }
                unsafe {
                    SendMessageW(self.h(E_ROUTE_DEVICE), CB_SETCURSEL, 0, 0);
                    SendMessageW(self.h(E_ROUTE_SOURCE), CB_SETCURSEL, 0, 0);
                }
            }
            unsafe {
                SendMessageW(self.h(E_TARGET), CB_RESETCONTENT, 0, 0);
            }
            if let Some(e) = &self.editor {
                for name in super::editor::Editor::targets(&e.document, self.page) {
                    self.add(E_TARGET, &name);
                }
                unsafe {
                    SendMessageW(self.h(E_TARGET), CB_SETCURSEL, e.index, 0);
                }
            }
        }
        let row = unsafe { SendMessageW(self.h(E_LIST), LB_GETCURSEL, 0, 0) }.max(0) as usize;
        unsafe {
            SendMessageW(self.h(E_LIST), LB_RESETCONTENT, 0, 0);
        }
        let fields = self.editor.as_ref().map(|e| e.fields()).unwrap_or_default();
        for f in &fields {
            let value = f
                .choices
                .iter()
                .find(|(_, v)| v == &f.value)
                .map_or(f.value.as_str(), |(label, _)| label.as_str());
            let text = format!("{}: {}", f.label, value);
            unsafe {
                SendMessageW(
                    self.h(E_LIST),
                    LB_ADDSTRING,
                    0,
                    wide(&text).as_ptr() as isize,
                );
            }
        }
        if !fields.is_empty() {
            unsafe {
                SendMessageW(self.h(E_LIST), LB_SETCURSEL, row.min(fields.len() - 1), 0);
            }
        }
        self.refresh_field();
    }
    fn refresh_field(&self) {
        unsafe {
            SendMessageW(self.h(E_CHOICE), CB_RESETCONTENT, 0, 0);
        }
        if let Some(f) = self.selected_field() {
            self.set_text(E_HINT, &self.wrap_hint(&format!("{}\n{}", f.label, f.hint)));
            self.set_text(E_VALUE, &f.value);
            for (label, _) in &f.choices {
                self.add(E_CHOICE, label);
            }
            if let Some(index) = f.choices.iter().position(|(_, v)| v == &f.value) {
                unsafe {
                    SendMessageW(self.h(E_CHOICE), CB_SETCURSEL, index, 0);
                }
            }
        } else {
            let hint = match self.page {
                Page::Ambient => "Выберите связь с экраном или добавьте новую слева.",
                Page::Lcd => "Выберите LCD или добавьте новый кнопкой слева.",
                _ => "Выберите устройство или источник.",
            };
            self.set_text(E_HINT, &self.wrap_hint(hint));
            self.set_text(E_VALUE, "");
        }
        self.editor_buttons();
        self.show_page();
    }
    fn wrap_hint(&self, text: &str) -> String {
        self.wrap_control_text(E_HINT, text, px(274, self.dpi))
    }
    fn wrap_control_text(&self, id: i32, text: &str, max: i32) -> String {
        let hwnd = self.h(id);
        let hdc = unsafe { GetDC(hwnd) };
        if hdc.is_null() {
            return text.into();
        }
        let old = unsafe { SelectObject(hdc, self.painter.body_font()) };
        let result = wrap_words(text, |line| {
            let wide = wide(line);
            let mut size = SIZE::default();
            unsafe {
                GetTextExtentPoint32W(hdc, wide.as_ptr(), (wide.len() - 1) as i32, &mut size) != 0
                    && size.cx <= max
            }
        });
        unsafe {
            SelectObject(hdc, old);
            ReleaseDC(hwnd, hdc);
        }
        result
    }
    fn editor_buttons(&self) {
        let pending = self.runtime.pending_configuration() || self.pending_open.is_some();
        let field = self.selected_field();
        for (id, enabled) in [
            (E_SET, field.as_ref().is_some_and(|f| !f.file) && !pending),
            (E_FILE, field.as_ref().is_some_and(|f| f.file) && !pending),
            (E_APPLY, self.editor_dirty && !pending),
            (E_RESET, self.editor_dirty || pending),
            (
                E_RETRY,
                self.runtime.configuration_error().is_some() || self.runtime.error().is_some(),
            ),
            (E_TARGET, !pending),
            (E_PRESET, !pending),
            (E_ROUTE_DEVICE, !pending),
            (E_ROUTE_SOURCE, !pending),
            (E_LED_COUNT, !pending),
            (E_VALUE, !pending),
            (E_CHOICE, !pending),
            (E_LIST, !pending),
            (
                E_ADD,
                !pending
                    && (self.page != Page::Ambient
                        || unsafe {
                            SendMessageW(self.h(E_ROUTE_DEVICE), CB_GETCURSEL, 0, 0) >= 0
                        }),
            ),
            (
                E_REMOVE,
                !pending
                    && self.editor.as_ref().is_some_and(|e| {
                        if self.page == Page::Devices {
                            e.document.lighting.setup.devices.get(e.index).is_some()
                        } else if self.page == Page::Ambient {
                            e.document.ambient.routes.get(e.index).is_some()
                        } else {
                            e.document.temperatures.sources.get(e.index).is_some()
                        }
                    }),
            ),
        ] {
            unsafe {
                EnableWindow(self.h(id), enabled as i32);
            }
        }
    }
    fn editor_layout(&self, w: i32, h: i32) {
        let r = right_panel(w);
        let left = r - CONTENT_LEFT - 24;
        self.pos(E_TARGET, CONTENT_LEFT, 184, left, 240);
        self.pos(E_LIST, CONTENT_LEFT, 228, left, h - 228 - 130);
        let lcd = self.page == Page::Lcd;
        self.pos(E_HINT, r, if lcd { 380 } else { 228 }, 280, 76);
        for id in [E_VALUE, E_CHOICE] {
            self.pos(
                id,
                r,
                if lcd { 464 } else { 320 },
                280,
                if id == E_CHOICE { 240 } else { 30 },
            );
        }
        self.pos(E_SET, r, if lcd { 504 } else { 364 }, 280, 32);
        self.pos(E_FILE, r, 544, 280, 32);
        self.pos(E_APPLY, CONTENT_LEFT, h - 92, 128, 32);
        self.pos(E_RESET, CONTENT_LEFT + 140, h - 92, 120, 32);
        self.pos(E_RETRY, CONTENT_LEFT + 272, h - 92, 120, 32);
        self.pos(E_ADD, CONTENT_LEFT, 184, 128, 32);
        self.pos(E_REMOVE, CONTENT_LEFT + 140, 184, 120, 32);
        if self.page == Page::Temperatures {
            self.pos(E_TARGET, CONTENT_LEFT, 184, left - 272, 240);
            self.pos(E_ADD, r, 184, 132, 32);
            self.pos(E_REMOVE, r + 144, 184, 136, 32);
        }
        if self.page == Page::Devices {
            self.set_text(E_NEW_LABEL, "Новое устройство");
            self.pos(E_REMOVE, r, 184, 280, 32);
            self.pos(E_NEW_LABEL, r, 410, 210, 24);
            self.pos(E_LED_LABEL, r + 222, 410, 58, 24);
            self.pos(E_PRESET, r, 438, 210, 240);
            self.pos(E_LED_COUNT, r + 222, 438, 58, 30);
            self.pos(E_ADD, r, 478, 280, 32);
        }
        self.set_text(
            E_ADD,
            if lcd {
                "Добавить TT LCD"
            } else {
                "Добавить"
            },
        );
        self.set_text(
            E_REMOVE,
            if self.page == Page::Ambient {
                "Удалить связь"
            } else {
                "Удалить"
            },
        );
        if self.page == Page::Ambient {
            self.pos(E_LIST, CONTENT_LEFT, 228, left, h - 228 - 218);
            self.pos(E_NEW_LABEL, CONTENT_LEFT, h - 208, left, 22);
            self.set_text(E_NEW_LABEL, "Добавить связь с экраном");
            self.pos(E_ROUTE_DEVICE, CONTENT_LEFT, h - 180, left, 240);
            self.pos(E_ROUTE_SOURCE, CONTENT_LEFT, h - 140, left - 140, 240);
            self.pos(E_ADD, CONTENT_LEFT + left - 128, h - 140, 128, 32);
            self.pos(E_REMOVE, r, 184, 280, 32);
        } else if lcd {
            self.pos(E_LIST, CONTENT_LEFT, 228, left, h - 228 - 180);
            self.pos(E_ADD, CONTENT_LEFT, h - 140, 190, 32);
        }
        self.pos(
            E_TEST,
            r,
            if self.page == Page::Ambient {
                h - 92
            } else {
                430
            },
            280,
            32,
        );
        self.set_text(
            E_TEST,
            if self.page == Page::Temperatures {
                "Подать тестовые 40°C"
            } else {
                if self.native_lighting.is_some() {
                    "Проверить источник экрана"
                } else {
                    "Показать тестовый кадр"
                }
            },
        );
    }
    fn editor_command(&mut self, id: i32, notice: u16) -> Result<bool, String> {
        if self.runtime.pending_configuration() && ![E_RESET, E_RETRY].contains(&id) {
            return Err("Дождитесь применения параметров или нажмите «Сбросить».".into());
        }
        match id {
            E_ROUTE_DEVICE | E_ROUTE_SOURCE if notice == CBN_SELCHANGE as u16 => {
                self.editor_buttons()
            }
            E_PRESET if notice == CBN_SELCHANGE as u16 => self.set_text(E_LED_COUNT, ""),
            E_TARGET if notice == CBN_SELCHANGE as u16 => {
                let index = unsafe { SendMessageW(self.h(E_TARGET), CB_GETCURSEL, 0, 0) };
                if let Some(editor) = &mut self.editor {
                    editor.index = index.max(0) as usize;
                }
                self.lcd_image.clear();
                self.ambient_image.clear();
                self.lcd_frame = (0, 0);
                self.ambient_frame = (0, 0);
                self.refresh_editor(true);
            }
            E_LIST if notice == LBN_SELCHANGE as u16 => self.refresh_field(),
            E_SET => {
                let f = self.selected_field().ok_or("Выберите параметр")?;
                let value = if f.choices.is_empty() {
                    self.text(E_VALUE)
                } else {
                    let index = unsafe { SendMessageW(self.h(E_CHOICE), CB_GETCURSEL, 0, 0) };
                    f.choices
                        .get(index as usize)
                        .ok_or("Выберите значение")?
                        .1
                        .clone()
                };
                self.editor
                    .as_mut()
                    .ok_or("Нет редактора")?
                    .set(f.key, &value)?;
                self.editor_dirty = true;
                self.dirty = true;
                self.status = "Параметр изменён. Нажмите «Применить».".into();
                self.refresh_editor(false);
            }
            E_APPLY => {
                let next = self
                    .editor
                    .as_ref()
                    .ok_or("Нет параметров")?
                    .merged(self.runtime.document())?;
                if let Some(local) = &self.native_lighting {
                    local.validate(&next)?;
                }
                let journal = journal_path(&self.state_dir, self.path.as_deref(), &next);
                self.runtime.request_document_at(next, journal)?;
                self.was_pending = self.runtime.pending_configuration();
                self.editor_dirty = false;
                self.dirty = true;
                self.status = "Проверка и применение параметров…".into();
                self.editor_buttons();
            }
            E_RESET => {
                if self.runtime.pending_configuration() {
                    self.runtime.cancel_configuration()?;
                } else {
                    let index = self.editor.as_ref().map_or(0, |e| e.index);
                    self.reset_editor(index);
                }
                self.status = "Изменения параметров отменены.".into();
            }
            E_RETRY => {
                if self.runtime.pending_configuration() {
                    self.runtime.retry_configuration()?;
                } else {
                    self.runtime.refresh_devices(crate::clock::monotonic_us())?;
                }
                self.status = "Повторная проверка подключений…".into();
            }
            E_ADD => {
                let selected = unsafe { SendMessageW(self.h(E_PRESET), CB_GETCURSEL, 0, 0) };
                let led_count = self.text(E_LED_COUNT);
                let route_device =
                    unsafe { SendMessageW(self.h(E_ROUTE_DEVICE), CB_GETCURSEL, 0, 0) };
                let route_source =
                    unsafe { SendMessageW(self.h(E_ROUTE_SOURCE), CB_GETCURSEL, 0, 0) };
                let editor = self.editor.as_mut().ok_or("Нет редактора")?;
                if self.page == Page::Ambient {
                    let device = editor
                        .ambient_candidates()
                        .get(route_device as usize)
                        .ok_or(
                            "Добавьте экранную ленту в разделе «Устройства» и выберите её здесь",
                        )?
                        .0
                        .clone();
                    let source = if route_source == 0 {
                        None
                    } else {
                        Some(
                            editor
                                .document
                                .ambient
                                .sources
                                .get((route_source - 1) as usize)
                                .ok_or("Выберите источник экрана")?
                                .key
                                .clone(),
                        )
                    };
                    editor.add_ambient_route(&device, source.as_deref())?;
                    self.editor_dirty = true;
                    self.dirty = true;
                    self.status =
                        "Связь добавлена выключенной. Настройте её и нажмите «Применить».".into();
                    self.refresh_editor(true);
                    return Ok(true);
                }
                if self.page == Page::Lcd {
                    editor.add_lcd_device()?;
                    self.editor_dirty = true;
                    self.dirty = true;
                    self.status =
                        "LCD добавлен выключенным. Выберите анимацию и примените настройки.".into();
                    self.refresh_editor(true);
                    return Ok(true);
                }
                if self.page == Page::Devices {
                    let preset = crate::app::device_setup::catalog()
                        .get(selected as usize)
                        .ok_or("Выберите поддерживаемый протокол")?;
                    if led_count.trim().is_empty() {
                        editor.add_device(preset.id)?;
                    } else {
                        editor.add_device_with_leds(
                            preset.id,
                            led_count.trim().parse().map_err(|_| "Введите число LED")?,
                        )?;
                    }
                    self.editor_dirty = true;
                    self.dirty = true;
                    self.refresh_editor(true);
                    return Ok(true);
                }
                let mut n = 1;
                while editor
                    .document
                    .temperatures
                    .sources
                    .iter()
                    .any(|s| s.key == format!("sensor{n}"))
                {
                    n += 1;
                }
                editor
                    .document
                    .temperatures
                    .sources
                    .push(crate::app::sensors::Source {
                        key: format!("sensor{n}"),
                        value: crate::app::sensors::Value::Sensor {
                            input: format!("input{n}"),
                        },
                    });
                editor.index = editor.document.temperatures.sources.len() - 1;
                self.editor_dirty = true;
                self.dirty = true;
                self.refresh_editor(true);
            }
            E_REMOVE => {
                let editor = self.editor.as_mut().ok_or("Нет редактора")?;
                if self.page == Page::Ambient {
                    editor.remove_ambient_route()?;
                    self.editor_dirty = true;
                    self.dirty = true;
                    self.status =
                        "Связь удалена в черновике; устройство выключено, источник сохранён."
                            .into();
                    self.refresh_editor(true);
                    return Ok(true);
                }
                if self.page == Page::Devices {
                    editor.remove_device()?;
                    self.editor_dirty = true;
                    self.dirty = true;
                    self.refresh_editor(true);
                    return Ok(true);
                }
                if editor.index >= editor.document.temperatures.sources.len() {
                    return Err("Выберите источник".into());
                }
                editor.document.temperatures.sources.remove(editor.index);
                editor.index = editor.index.saturating_sub(1);
                self.editor_dirty = true;
                self.dirty = true;
                self.refresh_editor(true);
            }
            E_TEST => {
                if self.page == Page::Ambient {
                    self.feed.enable();
                    self.attach_outputs()?;
                    self.status = if self.native_lighting.is_some() {
                        "Захват выбранного экрана запрошен; проверьте статус источника.".into()
                    } else {
                        "Включён синтетический кадр. Рабочий стол не захватывается.".into()
                    };
                } else {
                    if self.native_lighting.is_some() {
                        return Err(
                            "Тестовые температуры доступны только на программном стенде.".into(),
                        );
                    }
                    let now = crate::clock::monotonic_us();
                    let mut keys = std::collections::BTreeSet::new();
                    let inputs = self
                        .runtime
                        .document()
                        .temperatures
                        .sources
                        .iter()
                        .filter_map(|s| {
                            if let crate::app::sensors::Value::Sensor { input } = &s.value {
                                keys.insert(input.clone()).then(|| crate::app::lcd::Sample {
                                    key: input.clone(),
                                    celsius: 40.,
                                    observed_ms: now / 1000,
                                })
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>();
                    self.runtime.accept_temperatures(&inputs, now)?;
                    self.status = "Поданы явно тестовые 40°C; они устареют через 5 секунд.".into();
                }
            }
            E_FILE => return Ok(false),
            _ => return Ok(false),
        }
        Ok(true)
    }
    fn save(&mut self, path: &Path) -> Result<(), String> {
        if self.editor_dirty {
            let document = self
                .editor
                .as_ref()
                .ok_or("Нет черновика")?
                .merged(self.runtime.document())?;
            document.save(path)?;
            self.runtime.export_journal_for(
                &document,
                journal_path(&self.state_dir, Some(path), &document),
            )
        } else {
            self.runtime.save(path)?;
            self.runtime.relocate_journal(journal_path(
                &self.state_dir,
                Some(path),
                self.runtime.requested_document(),
            ))
        }
    }
    fn export_profile(&self, path: &Path) -> Result<usize, String> {
        if self.pending_open.is_some() || self.runtime.pending_configuration() {
            return Err("Дождитесь применения или открытия профиля.".into());
        }
        let document = if self.editor_dirty {
            self.editor
                .as_ref()
                .ok_or("Нет черновика")?
                .merged(self.runtime.document())?
        } else {
            self.runtime.document().clone()
        };
        let store = crate::app::assets::Store::new(self.asset_root.clone())?;
        crate::app::bundle::export(path, &document, &store)
    }
    fn open_profile(&mut self, path: &Path) -> Result<(), String> {
        if self.pending_open.is_some() || self.runtime.pending_configuration() {
            return Err("Дождитесь применения или открытия профиля.".into());
        }
        let imported = load_profile(path, &self.asset_root)?;
        let document = imported.document;
        if let Some(local) = &self.native_lighting {
            local.validate(&document)?;
        }
        let journal = journal_path(&self.state_dir, Some(path), &document);
        let mut candidate = Runtime::new(document, Some(journal))?;
        if self.runtime.autonomous_rgb() {
            candidate.enable_autonomous_rgb()?;
        }
        self.sources.release_lighting();
        self.runtime.stop();
        self.pending_open = Some((candidate, path.to_owned()));
        self.status = "Открытие профиля: ожидаю завершения текущих операций…".into();
        Ok(())
    }
    fn refresh_devices(&mut self) {
        if self.page != Page::Devices {
            return;
        }
        let selected = self
            .editor
            .as_ref()
            .and_then(|e| e.document.lighting.setup.devices.get(e.index));
        self.detail = selected.map_or_else(
            || "Добавьте устройство из списка поддерживаемых протоколов.".into(),
            |device| {
                let applied = self
                    .runtime
                    .project()
                    .setup
                    .devices
                    .iter()
                    .find(|d| d.id == device.id);
                let Some(applied) = applied else {
                    return "Новое устройство в черновике. Примените параметры.".into();
                };
                if !applied.enabled {
                    return "Выключено в применённом профиле. Аппаратные выходы не используются."
                        .into();
                }
                if applied.role == crate::devices::Role::Lcd {
                    self.runtime.lcd_status(&applied.id).map_or_else(
                        || "LCD: не запущен".into(),
                        |s| {
                            s.error.map_or_else(
                                || format!("LCD: передано кадров {}", s.frames_sent),
                                |e| format!("Ошибка LCD: {e}"),
                            )
                        },
                    )
                } else {
                    self.runtime.rgb_status(&applied.id).map_or_else(
                        || "RGB: не запущен".into(),
                        |s| {
                            s.error.map_or_else(
                                || format!("RGB: передано кадров {}", s.submitted),
                                |e| format!("Ошибка RGB: {e}"),
                            )
                        },
                    )
                }
            },
        );
    }
    fn changed_scene(&mut self, scene: Scene) -> Result<(), String> {
        self.runtime.set_scene(scene)?;
        self.dirty = true;
        self.status = "Сцена изменена и ещё не сохранена.".into();
        self.sync();
        Ok(())
    }
    fn command(&mut self, id: i32, notice: u16) -> Result<(), String> {
        if matches!(
            id,
            NAV_SCENE
                | NAV_DEVICES
                | NAV_SCHEDULE
                | NAV_AMBIENT
                | NAV_LCD
                | NAV_TEMPS
                | NAV_SOURCES
                | OPEN
        ) {
            self.guard_connections_exit()?;
        }
        if self.shutdown.active() {
            return Ok(());
        }
        if self.pending_open.is_some() {
            return Err("Ожидается завершение предыдущих операций.".into());
        }
        if [
            NAV_SCENE,
            NAV_DEVICES,
            NAV_SCHEDULE,
            NAV_AMBIENT,
            NAV_LCD,
            NAV_TEMPS,
            NAV_SOURCES,
            NAV_CONNECTIONS,
            OPEN,
        ]
        .contains(&id)
        {
            self.guard_sources_exit()?;
        }
        if self.connections_command(id, notice)? || self.sources_command(id, notice)? {
            return Ok(());
        }
        if (E_TARGET..=E_ROUTE_SOURCE).contains(&id) && self.editor_command(id, notice)? {
            return Ok(());
        }

        match id {
            NAV_SCENE | NAV_DEVICES | NAV_SCHEDULE | NAV_AMBIENT | NAV_LCD | NAV_TEMPS
            | NAV_SOURCES | NAV_CONNECTIONS => {
                if self.editor_dirty || self.runtime.pending_configuration() {
                    self.show_page();
                    return Err("Сначала примените или сбросьте параметры.".into());
                }
                self.page = match id {
                    NAV_SCENE => Page::Scene,
                    NAV_DEVICES => Page::Devices,
                    NAV_SCHEDULE => Page::Schedule,
                    NAV_AMBIENT => Page::Ambient,
                    NAV_LCD => Page::Lcd,
                    NAV_SOURCES => Page::Sources,
                    NAV_CONNECTIONS => Page::Connections,
                    _ => Page::Temperatures,
                };
                self.lcd_image.clear();
                self.ambient_image.clear();
                self.lcd_frame = (0, 0);
                self.ambient_frame = (0, 0);
                self.reset_editor(0);

                if self.page == Page::Sources {
                    self.refresh_sources();
                }
                self.layout(self.width, self.height);
                if self.page == Page::Connections {
                    self.open_connections()?;
                }
                self.refresh_devices();
                self.show_page();
            }
            EFFECT if notice == CBN_SELCHANGE as u16 => {
                let mut s = self.runtime.project().scene.clone();
                s.effect = match unsafe { SendMessageW(self.h(EFFECT), CB_GETCURSEL, 0, 0) } {
                    1 => Effect::Breathing,
                    2 => Effect::RainbowWave,
                    _ => Effect::NoiseMap,
                };
                let (lo, hi) = s.effect.limits();
                s.speed = s.speed.clamp(lo, hi);
                self.changed_scene(s)?;
            }
            FPS if notice == CBN_SELCHANGE as u16 => {
                let index = unsafe { SendMessageW(self.h(FPS), CB_GETCURSEL, 0, 0) };
                let mut s = self.runtime.project().scene.clone();
                s.fps = *rates(s.fps)
                    .get(index as usize)
                    .ok_or("Не выбрана частота")?;
                self.changed_scene(s)?;
            }
            PC_ON | DOOR_ON => {
                let target = if id == PC_ON {
                    Target::Pc
                } else {
                    Target::Door
                };
                let result = self.runtime.set_group(target, self.checked(id));
                self.set_check(id, self.runtime.group_on(target));
                result?;
                self.status = if self.native_lighting.is_some() {
                    "Состояние группы изменено. Проверьте статусы устройств.".into()
                } else {
                    "Видимость изменена только в предпросмотре.".into()
                };
            }
            APPLY_SCHED => {
                let mut c = self.runtime.project().schedule.clone();
                c.enabled = self.checked(SCHED_ON);
                c.paused = self.checked(SCHED_PAUSE);
                c.timezone = self.text(TIMEZONE);
                for (i, id) in [TIME_PC, TIME_DOOR, TIME_OFF].into_iter().enumerate() {
                    c.times[i] = parse_time(&self.text(id))?;
                }
                self.runtime.configure_schedule(c)?;
                self.dirty = true;
                self.status = if self.native_lighting.is_some() {
                    "Расписание обновлено для подключённых устройств.".into()
                } else {
                    "Расписание применяется к предпросмотру. Устройства не подключены.".into()
                };
                self.sync();
            }
            _ => {}
        }
        Ok(())
    }
    fn slider(&mut self, handle: HWND) -> Result<(), String> {
        if self.shutdown.active() {
            return Ok(());
        }
        let mut s = self.runtime.project().scene.clone();
        let value = unsafe { SendMessageW(handle, TBM_GETPOS, 0, 0) } as i32;
        if handle == self.h(BRIGHT) {
            s.brightness = u8::try_from(value).map_err(|_| "Недопустимая яркость")?;
        } else if handle == self.h(SPEED) {
            s.speed = s.effect.native_speed(value).map_err(str::to_owned)?;
        } else {
            return Ok(());
        }
        self.changed_scene(s)
    }
    fn tick(&mut self) -> Result<bool, String> {
        self.runtime.refresh();
        self.runtime.maintenance(|r, _| self.sources.poll(r))?;
        if self.sources.last_status.elapsed() >= std::time::Duration::from_millis(500) {
            self.refresh_source_status();
            self.sources.last_status = Instant::now();
        }
        let scan_changed =
            self.poll_source_discovery() | self.poll_connections() | self.poll_connection_switch();
        if self.shutdown.active() {
            let previous = self.status.clone();
            self.runtime.poll_shutdown(&mut self.shutdown)?;
            self.status = self.shutdown.message();
            if self.shutdown.phase() == crate::app::shutdown::Phase::Ready
                && (!self.sources.scan.is_idle() || !self.connections.scan.is_idle())
            {
                self.status = "Ожидаю завершения поиска метаданных устройств…".into();
            }
            self.show_page();
            return Ok(previous != self.status || scan_changed);
        }
        if self.pending_open.is_some() {
            if !self.runtime.drained() {
                return Ok(scan_changed);
            }
            let (runtime, path) = self.pending_open.take().unwrap();
            self.sources.release_lighting();
            self.runtime = Client::new(runtime, self.runtime.background())?;
            self.context_revision = self.context_revision.wrapping_add(1);
            self.attach_outputs()?;
            self.path = Some(path);
            self.dirty = false;
            self.was_pending = false;
            self.reset_editor(0);
            self.sync();
            self.status = "Профиль открыт. Управление оборудованием выключено.".into();
            if self.page == Page::Sources {
                self.refresh_sources();
            }
        }
        let previous = self.runtime.view_revision();
        let connection_switching = self.connection_switching();
        self.runtime.tick(crate::clock::monotonic_us(), wall())?;
        let mut changed = self.runtime.view_revision() != previous || scan_changed;
        changed |= self.poll_connection_switch();
        if self.was_pending && !self.runtime.pending_configuration() && !connection_switching {
            self.context_revision = self.context_revision.wrapping_add(1);
            self.attach_outputs()?;
            let index = self.editor.as_ref().map_or(0, |e| e.index);
            self.reset_editor(index);
            self.sync();
            self.status = if self.native_lighting.is_some() {
                "Параметры обновлены. Проверяйте статусы передачи устройств.".into()
            } else {
                "Параметры обновлены на программном стенде.".into()
            };
            changed = true;
        }
        self.was_pending = self.runtime.pending_configuration();
        if let Some(error) = self.runtime.configuration_error() {
            self.status = format!("Не удалось применить: {error}");
            self.editor_buttons();
        }
        let groups = [
            self.runtime.group_on(Target::Pc),
            self.runtime.group_on(Target::Door),
        ];
        if groups != self.groups {
            self.groups = groups;
            self.set_check(PC_ON, groups[0]);
            self.set_check(DOOR_ON, groups[1]);
            changed = true;
        }
        if self.widgets.is_empty() {
            return Ok(changed);
        }
        if self.page == Page::Lcd
            && let Some(entry) = self
                .runtime
                .document()
                .lcd
                .get(self.editor.as_ref().map_or(0, |e| e.index))
            && let Some(snapshot) = self.runtime.lcd_status(&entry.device)
        {
            let frame = (snapshot.generation, snapshot.frames_sent);
            if snapshot.error.is_none() && frame != self.lcd_frame && !snapshot.preview.is_empty() {
                let image = image::load_from_memory(&snapshot.preview)
                    .map_err(|e| e.to_string())?
                    .into_rgba8();
                self.lcd_image_size = (image.width(), image.height());
                self.lcd_image = image.into_raw();
                for pixel in self.lcd_image.as_chunks_mut::<4>().0 {
                    pixel.swap(0, 2);
                }
                self.lcd_frame = frame;
                changed = true;
            }
            if let Some(error) = snapshot.error {
                changed |= !self.lcd_image.is_empty();
                self.lcd_image.clear();
                self.lcd_frame = (0, 0);
                self.detail = format!("Предпросмотр недоступен: {error}");
            }
        }
        if self.page == Page::Ambient
            && let Some(snapshot) = self.runtime.capture_status()
        {
            let frame = (snapshot.generation, snapshot.frame_revision);
            if frame != self.ambient_frame {
                self.ambient_image = self
                    .editor
                    .as_ref()
                    .and_then(|e| e.document.ambient.routes.get(e.index))
                    .and_then(|r| snapshot.frames.get(&r.device))
                    .map_or_else(Vec::new, |v| v.to_vec());
                self.ambient_frame = frame;
                changed = true;
            }
        }
        if self.page == Page::Temperatures
            && let Some(source) = self
                .editor
                .as_ref()
                .and_then(|e| e.document.temperatures.sources.get(e.index))
        {
            let now = crate::clock::monotonic_ms();
            self.detail = self
                .runtime
                .temperature_samples()
                .iter()
                .find(|s| {
                    s.key == source.key
                        && now
                            .checked_sub(s.observed_ms)
                            .is_some_and(|age| age <= 5000)
                })
                .map_or_else(
                    || {
                        format!(
                            "{}: нет свежих показаний. Тестовая кнопка задаёт 40°C на 5 секунд.",
                            source.key
                        )
                    },
                    |s| format!("{}: {:.1}°C · источник температуры", source.key, s.celsius),
                );
        }
        if self.page == Page::Devices
            && self.last_status.elapsed() >= std::time::Duration::from_millis(500)
        {
            self.refresh_devices();
            self.last_status = Instant::now();
            changed = true;
        }
        Ok(changed)
    }
    fn render(&mut self, target: HDC) -> Result<(), String> {
        if self.width <= 0 || self.height <= 0 {
            return Ok(());
        }
        if self.buffer.is_none() {
            self.buffer = Some(Bitmap::new(self.width, self.height)?);
        }
        let buffer = self.buffer.as_ref().unwrap();
        self.painter
            .paint(buffer.dc, self.width, self.height, &self.view())?;
        if unsafe {
            BitBlt(
                target,
                0,
                0,
                self.width,
                self.height,
                buffer.dc,
                0,
                0,
                SRCCOPY,
            )
        } == 0
        {
            return Err(last("Present UI frame"));
        }
        Ok(())
    }
}
fn parse_time(s: &str) -> Result<u16, String> {
    let (h, m) = s
        .trim()
        .split_once(':')
        .ok_or("Время должно быть в формате ЧЧ:ММ")?;
    let (h, m) = (
        h.parse::<u16>().map_err(|_| "Некорректный час")?,
        m.parse::<u16>().map_err(|_| "Некорректные минуты")?,
    );
    if h >= 24 || m >= 60 {
        return Err("Время вне диапазона00:00–23:59".into());
    }
    Ok(h * 60 + m)
}
struct State {
    icon: brand_icon::Icon,
    modern_enabled: bool,
    webview: RefCell<Option<web_shell::WebShell>>,
    modern_session: RefCell<Option<modern::Session>>,
    app: RefCell<Option<App>>,
    snapshot: bool,
    resident: Cell<bool>,
    editor_window: Rc<Cell<HWND>>,
    shell_window: Cell<HWND>,
    session_action: Cell<resident::Action>,
    tray_enabled: Cell<bool>,
    pending_power: Cell<resident::PowerIntent>,
}
fn dialog(hwnd: HWND, save: bool) -> Result<Option<PathBuf>, String> {
    profile_dialog(hwnd, save, false)
}
fn profile_dialog(hwnd: HWND, save: bool, bundle: bool) -> Result<Option<PathBuf>, String> {
    let filter = if bundle {
        "Профиль с GIF и шрифтами (*.zip)\0*.zip\0\0"
    } else if save {
        "Профиль без файлов медиа (*.json)\0*.json\0\0"
    } else {
        "Профили (*.json;*.zip)\0*.json;*.zip\0\0"
    };
    file_dialog(hwnd, save, filter, if bundle { "zip" } else { "json" })
}
fn file_dialog(
    hwnd: HWND,
    save: bool,
    filter: &str,
    extension: &str,
) -> Result<Option<PathBuf>, String> {
    let mut file = [0u16; 32768];
    let filter = wide(filter);
    let mut o = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: filter.as_ptr(),
        lpstrFile: file.as_mut_ptr(),
        nMaxFile: file.len() as u32,
        lpstrDefExt: null(),
        Flags: OFN_NOCHANGEDIR
            | OFN_PATHMUSTEXIST
            | if save {
                OFN_OVERWRITEPROMPT
            } else {
                OFN_FILEMUSTEXIST
            },
        ..Default::default()
    };
    let ext = wide(extension);
    o.lpstrDefExt = ext.as_ptr();
    let okay = unsafe {
        if save {
            GetSaveFileNameW(&mut o)
        } else {
            GetOpenFileNameW(&mut o)
        }
    };
    if okay == 0 {
        let e = unsafe { CommDlgExtendedError() };
        if e == 0 {
            return Ok(None);
        }
        return Err(format!("Ошибка диалога файла0x{e:x}"));
    }
    let end = file
        .iter()
        .position(|c| *c == 0)
        .ok_or("Незавершённый путь файла")?;
    Ok(Some(PathBuf::from(
        String::from_utf16(&file[..end]).map_err(|_| "Путь неUTF16")?,
    )))
}
fn save_dialog(hwnd: HWND, state: &State) -> Result<bool, String> {
    let Some(path) = dialog(hwnd, true)? else {
        return Ok(false);
    };
    let mut slot = state.app.try_borrow_mut().map_err(|_| "UI занято")?;
    let app = slot.as_mut().ok_or("UI не создан")?;
    app.save(&path)?;
    app.path = Some(path);
    app.dirty = false;
    app.status = "Профиль сохранён.".into();
    Ok(true)
}
fn confirm_discard(hwnd: HWND, state: &State) -> Result<bool, String> {
    let dirty = state
        .app
        .try_borrow()
        .map_err(|_| "UI занято")?
        .as_ref()
        .is_some_and(|a| a.dirty);
    if !dirty {
        return Ok(true);
    }
    let answer = unsafe {
        MessageBoxW(
            hwnd,
            wide("Сохранить изменённый профиль?").as_ptr(),
            wide("Есть несохранённые изменения").as_ptr(),
            MB_YESNOCANCEL | MB_ICONQUESTION,
        )
    };
    match answer {
        IDYES => save_dialog(hwnd, state),
        IDNO => Ok(true),
        _ => Ok(false),
    }
}
fn asset_dialog(hwnd: HWND, font: bool) -> Result<Option<PathBuf>, String> {
    let mut file = [0u16; 32768];
    let filter = wide(if font {
        "Шрифт (*.ttf;*.otf)\0*.ttf;*.otf\0Все файлы\0*.*\0\0"
    } else {
        "Анимация / изображение\0*.gif;*.png;*.apng;*.jpg;*.jpeg;*.bmp\0Все файлы\0*.*\0\0"
    });
    let mut o = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: filter.as_ptr(),
        lpstrFile: file.as_mut_ptr(),
        nMaxFile: file.len() as u32,
        Flags: OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST | OFN_FILEMUSTEXIST,
        ..Default::default()
    };
    if unsafe { GetOpenFileNameW(&mut o) } == 0 {
        let e = unsafe { CommDlgExtendedError() };
        if e == 0 {
            return Ok(None);
        }
        return Err(format!("Не удалось выбрать файл: {e:x}"));
    }
    let end = file.iter().position(|c| *c == 0).ok_or("Неверный путь")?;
    Ok(Some(PathBuf::from(
        String::from_utf16(&file[..end]).map_err(|_| "Неверный путь")?,
    )))
}
fn import_asset(app: &mut App, path: &Path, key: &str) -> Result<(), String> {
    let limit = if key == "font" {
        8 * 1024 * 1024
    } else {
        crate::lcd::media::MAX_SOURCE
    };
    let file = File::open(path).map_err(|e| e.to_string())?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > limit as u64 {
        return Err("Файл слишком большой или не является обычным файлом".into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let id = crate::app::assets::Store::new(app.asset_root.clone())?.put(&bytes, limit)?;
    let editor = app.editor.as_mut().ok_or("Нет редактора LCD")?;
    let config = &mut editor
        .document
        .lcd
        .get_mut(editor.index)
        .ok_or("Выберите LCD")?
        .config;
    match key {
        "font" => config.font = Some(id),
        "media" => config.media = Some(id),
        _ => return Err("Выберите анимацию или шрифт".into()),
    };
    app.editor_dirty = true;
    app.dirty = true;
    app.status = "Файл добавлен в профиль. Нажмите «Применить».".into();
    app.refresh_editor(false);
    Ok(())
}
fn modal_command(hwnd: HWND, state: &State, id: i32) -> Result<(), String> {
    if id == sources::MODULE_SELECT {
        return sources::module_dialog(hwnd, state);
    }
    {
        let slot = state.app.try_borrow().map_err(|_| "UI занято")?;
        let a = slot.as_ref().ok_or("UI не создан")?;
        if a.pending_open.is_some() || (id != SAVE && a.runtime.pending_configuration()) {
            return Err("Дождитесь применения параметров".into());
        }
    }
    if id == E_FILE {
        let (key, index, device, revision) = {
            let slot = state.app.try_borrow().map_err(|_| "UI занято")?;
            let a = slot.as_ref().ok_or("UI не создан")?;
            if a.runtime.pending_configuration() || a.pending_open.is_some() {
                return Err("Дождитесь применения параметров".into());
            }
            let field = a
                .selected_field()
                .filter(|f| f.file)
                .ok_or("Выберите анимацию или шрифт")?;
            let e = a.editor.as_ref().ok_or("Нет редактора")?;
            (
                field.key,
                e.index,
                e.document
                    .lcd
                    .get(e.index)
                    .ok_or("Выберите LCD")?
                    .device
                    .clone(),
                a.context_revision,
            )
        };
        let Some(path) = asset_dialog(hwnd, key == "font")? else {
            return Ok(());
        };
        let mut slot = state.app.try_borrow_mut().map_err(|_| "UI занято")?;
        let a = slot.as_mut().ok_or("UI не создан")?;
        if a.context_revision != revision
            || a.pending_open.is_some()
            || a.runtime.pending_configuration()
            || a.page != Page::Lcd
            || a.editor.as_ref().is_none_or(|e| {
                e.index != index || e.document.lcd.get(index).is_none_or(|l| l.device != device)
            })
        {
            return Err("Выбор LCD изменился. Повторите выбор файла.".into());
        }
        import_asset(a, &path, key)?;
        return Ok(());
    }
    if id == SAVE {
        save_dialog(hwnd, state)?;
        return Ok(());
    }
    if id == EXPORT {
        let Some(path) = profile_dialog(hwnd, true, true)? else {
            return Ok(());
        };
        let mut slot = state.app.try_borrow_mut().map_err(|_| "UI занято")?;
        let a = slot.as_mut().ok_or("UI не создан")?;
        let count = a.export_profile(&path)?;
        a.status = format!(
            "Экспортирована копия профиля и файлов медиа: {count}. Привязки к оборудованию не включены."
        );
        return Ok(());
    }
    if id == OPEN {
        if !confirm_discard(hwnd, state)? {
            return Ok(());
        }
        let Some(path) = dialog(hwnd, false)? else {
            return Ok(());
        };
        let mut slot = state.app.try_borrow_mut().map_err(|_| "UI занято")?;
        let a = slot.as_mut().ok_or("UI не создан")?;
        return a.open_profile(&path);
    }
    if id == COLOR_EDIT {
        let (index, rgb, revision) = {
            let slot = state.app.try_borrow().map_err(|_| "UI занято")?;
            let a = slot.as_ref().ok_or("UI не создан")?;
            let index = unsafe { SendMessageW(a.h(COLOR_INDEX), CB_GETCURSEL, 0, 0) };
            let rgb = *a
                .runtime
                .project()
                .scene
                .colors
                .get(index as usize)
                .ok_or("Не выбран цвет")?;
            (index as usize, rgb, a.context_revision)
        };
        let mut custom = [0u32; 16];
        let mut color = CHOOSECOLORW {
            lStructSize: size_of::<CHOOSECOLORW>() as u32,
            hwndOwner: hwnd,
            rgbResult: (rgb >> 16) | (rgb & 0xff00) | ((rgb & 0xff) << 16),
            lpCustColors: custom.as_mut_ptr(),
            Flags: CC_FULLOPEN | CC_RGBINIT,
            ..Default::default()
        };
        if unsafe { ChooseColorW(&mut color) } != 0 {
            let mut slot = state.app.try_borrow_mut().map_err(|_| "UI занято")?;
            let a = slot.as_mut().ok_or("UI не создан")?;
            if a.context_revision != revision
                || a.pending_open.is_some()
                || a.runtime.pending_configuration()
                || a.runtime.project().scene.colors.get(index) != Some(&rgb)
            {
                return Err("Профиль или палитра изменились. Повторите выбор цвета.".into());
            }
            let mut s = a.runtime.project().scene.clone();
            let c = color.rgbResult;
            s.colors[index] = (c >> 16) | (c & 0xff00) | ((c & 0xff) << 16);
            a.changed_scene(s)?;
        }
    }
    Ok(())
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    std::panic::catch_unwind(|| unsafe { dispatch(hwnd, msg, w, l) }).unwrap_or_else(|_| {
        if msg == WM_CLOSE {
            0
        } else {
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
    })
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
        if state.editor_window.get() == hwnd {
            state.editor_window.set(null_mut());
        }
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        }
        return unsafe { DefWindowProcW(hwnd, msg, w, l) };
    }
    // The editor can exist briefly before its permanent shell has been created.
    if msg == WM_POWERBROADCAST && state.resident.get() && state.shell_window.get().is_null() {
        if matches!(w as u32, PBT_APMSUSPEND | PBT_APMRESUMEAUTOMATIC)
            && let Err(error) = resident::power_message(state, w as u32 == PBT_APMSUSPEND)
        {
            resident::report(state, error);
        }
        return 1;
    }
    if msg == WM_DESTROY {
        modern::close(state);
        if let Ok(mut slot) = state.app.try_borrow_mut()
            && let Some(app) = slot.as_mut()
        {
            app.widgets.clear();
            app.buffer = None;
            app.lcd_image = vec![];
            app.ambient_image = vec![];
            app.lcd_frame = (0, 0);
            app.ambient_frame = (0, 0);
        }
        unsafe {
            KillTimer(hwnd, 1);
            if !state.resident.get() {
                PostQuitMessage(0);
            }
        }
        return 0;
    }
    if msg == WM_CLOSE {
        if state.resident.get() {
            if state.session_action.get() == resident::Action::None
                && let Err(error) = resident::close_editor(state)
            {
                modern::close_failed(state, &error);
                resident::report(state, error);
            }
            return 0;
        }
        let active = match state.app.try_borrow() {
            Ok(slot) => slot.as_ref().is_some_and(|a| a.shutdown.active()),
            Err(_) => return 0,
        };
        if !state.snapshot && !active {
            match confirm_discard(hwnd, state) {
                Ok(true) => {}
                Ok(false) => return 0,
                Err(e) => {
                    if let Ok(mut slot) = state.app.try_borrow_mut()
                        && let Some(a) = slot.as_mut()
                    {
                        a.status = e;
                    }
                    unsafe {
                        InvalidateRect(hwnd, null(), 0);
                    }
                    return 0;
                }
            }
        }
        let ready = if let Ok(mut slot) = state.app.try_borrow_mut() {
            if let Some(a) = slot.as_mut() {
                a.request_close();
                a.close_ready()
            } else {
                false
            }
        } else {
            false
        };
        unsafe {
            if ready {
                DestroyWindow(hwnd);
            } else {
                InvalidateRect(hwnd, null(), 0);
            }
        }
        return 0;
    }
    if msg == WM_GETMINMAXINFO {
        if state.snapshot {
            // Off-screen fixtures have a fixed, validated client size and no
            // interactive resizing. Do not inherit the host monitor's minimum.
            return unsafe { DefWindowProcW(hwnd, msg, w, l) };
        }
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let content_dpi = state
            .app
            .try_borrow()
            .ok()
            .and_then(|slot| slot.as_ref().map(|a| a.dpi))
            .unwrap_or(dpi);
        let info = unsafe { &mut *(l as *mut MINMAXINFO) };
        // The minimum describes usable content, not the outer frame. Otherwise
        // resizing below the editor's required client height clips its actions.
        let mut minimum = RECT {
            left: 0,
            top: 0,
            right: px(MIN_WIDTH, content_dpi),
            bottom: px(MIN_HEIGHT, content_dpi),
        };
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
        let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        if unsafe { AdjustWindowRectExForDpi(&mut minimum, style, 0, ex_style, dpi) } != 0 {
            info.ptMinTrackSize = POINT {
                x: minimum.right - minimum.left,
                y: minimum.bottom - minimum.top,
            };
        }
        return 0;
    }
    if msg == WM_ERASEBKGND {
        return 1;
    }
    if msg == WM_PRINT {
        return unsafe { DefWindowProcW(hwnd, msg, w, l) };
    }
    if msg == WM_PAINT {
        let mut ps = PAINTSTRUCT::default();
        let dc = unsafe { BeginPaint(hwnd, &mut ps) };
        if let Ok(mut slot) = state.app.try_borrow_mut()
            && let Some(a) = slot.as_mut()
            && !a.web_mode
            && let Err(e) = a.render(dc)
        {
            a.status = e;
        }
        unsafe {
            EndPaint(hwnd, &ps);
        }
        return 0;
    }
    if msg == WM_PRINTCLIENT {
        if let Ok(slot) = state.app.try_borrow()
            && let Some(a) = slot.as_ref()
        {
            let _ = a.painter.paint(w as HDC, a.width, a.height, &a.view());
        }
        return 0;
    }
    if msg == WM_SIZE {
        modern::resize(state, (l & 0xffff) as i32, ((l >> 16) & 0xffff) as i32);
    }
    if msg == WM_TIMER {
        modern::maintain(state);
    }
    if msg == WM_COMMAND && (w & 0xffff) as i32 == MODERN_RETURN {
        if let Err(error) = modern::return_to_editor(state) {
            resident::report(state, error);
        }
        return 0;
    }
    if msg == WM_COMMAND && MODAL_COMMANDS.contains(&((w & 0xffff) as i32)) {
        if state.app.try_borrow().map_or(true, |slot| {
            slot.as_ref().is_some_and(|a| a.shutdown.active())
        }) {
            return 0;
        }
        let result = modal_command(hwnd, state, (w & 0xffff) as i32);
        if let Err(e) = result
            && let Ok(mut slot) = state.app.try_borrow_mut()
            && let Some(a) = slot.as_mut()
        {
            a.status = e;
        }
        unsafe {
            InvalidateRect(hwnd, null(), 0);
        }
        return 0;
    }
    if msg == WM_CTLCOLORSTATIC {
        unsafe {
            SetBkMode(w as HDC, TRANSPARENT as i32);
            SetTextColor(w as HDC, 0x2b2723);
        }
        unsafe {
            SetDCBrushColor(w as HDC, 0x00f4f7f8);
        }
        return unsafe { GetStockObject(DC_BRUSH) } as isize;
    }
    // Snapshot fixtures deliberately render an explicit logical DPI. Moving
    // their owned window off-screen must not replace it with the host monitor's
    // DPI. Interactive windows still follow WM_DPICHANGED normally below.
    if msg == WM_DPICHANGED && state.snapshot {
        return 0;
    }
    if let Ok(mut slot) = state.app.try_borrow_mut()
        && let Some(a) = slot.as_mut()
    {
        let result = match msg {
            WM_SIZE => {
                a.layout((l & 0xffff) as i32, ((l >> 16) & 0xffff) as i32);
                Ok(())
            }
            WM_COMMAND => a.command((w & 0xffff) as i32, ((w >> 16) & 0xffff) as u16),
            WM_HSCROLL => a.slider(l as HWND),
            WM_TIMER => match a.tick() {
                Ok(changed) => {
                    if !state.resident.get() && a.close_ready() {
                        unsafe {
                            PostMessageW(hwnd, WM_CLOSE, 0, 0);
                        }
                    }
                    if changed {
                        unsafe {
                            InvalidateRect(hwnd, null(), 0);
                        }
                    }
                    Ok(())
                }
                Err(e) => Err(e),
            },
            WM_POWERBROADCAST => {
                if !state.resident.get()
                    && !a.shutdown.active()
                    && (w as u32 == PBT_APMSUSPEND || w as u32 == PBT_APMRESUMEAUTOMATIC)
                {
                    a.runtime
                        .notify_power(w as u32 == PBT_APMSUSPEND, crate::clock::monotonic_us())
                } else {
                    Ok(())
                }
            }
            WM_DPICHANGED => {
                let dpi = (w & 0xffff) as u32;
                match Painter::new(dpi) {
                    Ok(painter) => {
                        let old = std::mem::replace(&mut a.painter, painter);
                        a.dpi = dpi;
                        for h in a.widgets.values() {
                            unsafe {
                                SendMessageW(*h, WM_SETFONT, a.painter.body_font() as usize, 1);
                            }
                        }
                        drop(old);
                        let r = unsafe { &*(l as *const RECT) };
                        unsafe {
                            SetWindowPos(
                                hwnd,
                                null_mut(),
                                r.left,
                                r.top,
                                r.right - r.left,
                                r.bottom - r.top,
                                SWP_NOACTIVATE | SWP_NOZORDER,
                            );
                            let mut client = RECT::default();
                            GetClientRect(hwnd, &mut client);
                            a.layout(client.right, client.bottom);
                        }
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            _ => {
                // Default handling can synchronously reenter dispatch (e.g. SC_CLOSE).
                drop(slot);
                return unsafe { DefWindowProcW(hwnd, msg, w, l) };
            }
        };
        if let Err(e) = result {
            a.status = e;
        }
        if msg != WM_TIMER {
            unsafe {
                InvalidateRect(hwnd, null(), 0);
            }
        }
        return if msg == WM_POWERBROADCAST { 1 } else { 0 };
    }
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}

struct Window(Rc<Cell<HWND>>);
impl Drop for Window {
    fn drop(&mut self) {
        let hwnd = self.0.get();
        if !hwnd.is_null() && unsafe { IsWindow(hwnd) } != 0 {
            unsafe {
                DestroyWindow(hwnd);
            }
        }
    }
}
pub fn run(args: Vec<OsString>) -> Result<(), String> {
    let diagnostic = args.iter().any(|a| a == "--snapshot" || a == "--selftest");
    let error_dir = args
        .windows(2)
        .find(|pair| pair[0] == "--state-dir")
        .map(|pair| PathBuf::from(&pair[1]));
    let result = run_inner(args);
    if !diagnostic && let Err(error) = &result {
        if let Some(dir) = error_dir {
            let _ = std::fs::write(dir.join("startup-error.txt"), error);
        }
        let message: String = error.chars().take(1000).collect();
        unsafe {
            MessageBoxW(
                null_mut(),
                wide(&message).as_ptr(),
                wide("Не удалось запустить программу").as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    result
}
fn native_sources_allowed(snapshot: bool, selftest: bool) -> bool {
    !snapshot && !selftest
}
fn run_inner(args: Vec<OsString>) -> Result<(), String> {
    let mut o = options(args)?;
    let state_dir = if let Some(path) = &o.state_dir {
        path.clone()
    } else if o.selftest || o.snapshot.is_some() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        std::env::temp_dir().join(format!(
            "prismestra-diagnostic-{}-{stamp}",
            std::process::id()
        ))
    } else {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or("LOCALAPPDATA unavailable; use--state-dir")?
            .join("PcControl")
            .join("Stand")
    };
    let state_dir = if o.native_lighting.is_some() {
        state_dir.join("Lighting")
    } else {
        state_dir
    };
    std::fs::create_dir_all(&state_dir).map_err(|e| e.to_string())?;
    if o.start_connected {
        o.native_lighting = Some(state_dir.join("lighting-local.json"));
    }
    if o.selftest && o.snapshot.is_none() {
        o.snapshot = Some(state_dir.join("diagnostics/selftest.png"));
    }
    let resident_mode = o.snapshot.is_none() && !o.selftest;
    let _single_instance = if resident_mode {
        Some(resident::Instance::acquire(&state_dir)?)
    } else {
        None
    };
    // This process setting does not change Windows display resolution or other apps.
    if unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) } == 0 {
        return Err(last("Enable sharp per-monitor native UI"));
    }
    let dpi = o
        .dpi
        .unwrap_or_else(|| unsafe { GetDpiForSystem() }.max(96));
    let asset_root = std::fs::canonicalize(&state_dir)
        .map_err(|e| e.to_string())?
        .join("assets");
    let imported = if let Some(path) = &o.project {
        load_profile(path, &asset_root)?
    } else {
        let (project, document) = project(&o)?;
        crate::app::bundle::Profile {
            document: document.unwrap_or(Document::from_lighting(project)?),
        }
    };
    let document = imported.document;
    let native_lighting = o
        .native_lighting
        .as_ref()
        .map(|path| crate::app::lighting_outputs::Local::read(path, &document))
        .transpose()?;
    let feed = super::stand::Feed::default();
    let journal = journal_path(&state_dir, o.project.as_deref(), &document);
    let mut runtime = Runtime::new(document, Some(journal))?;
    if o.snapshot.is_none() || o.selftest {
        runtime.enable_autonomous_rgb()?;
    }
    if let Some(local) = &native_lighting {
        local.attach_missing(&mut runtime, &asset_root)?;
    } else {
        super::stand::attach_missing(&mut runtime, &asset_root, &feed)?;
    }
    runtime.tick(crate::clock::monotonic_us(), wall())?;
    let painter = Painter::new(dpi)?;
    let init = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_BAR_CLASSES,
    };
    if unsafe { InitCommonControlsEx(&init) } == 0 {
        return Err(last("Initialize native controls"));
    }
    let state = Box::new(State {
        icon: brand_icon::Icon::new()?,
        modern_enabled: !o.classic_editor && resident_mode,
        webview: RefCell::new(None),
        modern_session: RefCell::new(None),
        app: RefCell::new(None),
        snapshot: o.snapshot.is_some(),
        resident: Cell::new(resident_mode),
        editor_window: Rc::new(Cell::new(null_mut())),
        shell_window: Cell::new(null_mut()),
        session_action: Cell::new(resident::Action::None),
        tray_enabled: Cell::new(false),
        pending_power: Cell::new(resident::PowerIntent::default()),
    });
    let instance = unsafe { GetModuleHandleW(null()) };
    let class = wide(CLASS);
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(proc),
        hInstance: instance,
        hIcon: state.icon.handle(),
        hIconSm: state.icon.handle(),
        hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
        hbrBackground: unsafe { GetStockObject(WHITE_BRUSH) } as HBRUSH,
        lpszClassName: class.as_ptr(),
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&wc) } == 0 {
        return Err(last("Register preview window"));
    }
    let (w, h) = o.size.unwrap_or((1120, 760));
    // Snapshot images contain client content only. A borderless owned window
    // keeps an explicitly requested test DPI independent of host frame metrics;
    // the actual controls, layout, renderer and event path remain the same.
    let style = if o.snapshot.is_some() {
        WS_POPUP
    } else {
        WS_OVERLAPPEDWINDOW
    } | WS_CLIPCHILDREN;
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: px(w, dpi),
        bottom: px(h, dpi),
    };
    if unsafe { AdjustWindowRectExForDpi(&mut rect, style, 0, WS_EX_CONTROLPARENT, dpi) } == 0 {
        return Err(last("Size native UI"));
    }
    let raw = unsafe {
        CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class.as_ptr(),
            wide(if native_lighting.is_some() {
                "Prismestra · Подсветка устройств"
            } else {
                "Prismestra · Предпросмотр"
            })
            .as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            rect.right - rect.left,
            rect.bottom - rect.top,
            null_mut(),
            null_mut(),
            instance,
            (&*state as *const State).cast(),
        )
    };
    if raw.is_null() {
        return Err(last("Create preview window"));
    }
    state.editor_window.set(raw);
    let window = Window(state.editor_window.clone());
    let mut app = App {
        web_mode: false,
        connections: connections::Editor::new(
            o.native_lighting
                .clone()
                .unwrap_or_else(|| state_dir.join("lighting-local.json")),
            if o.selftest {
                Some(connections::stand_factory())
            } else if o.snapshot.is_some() {
                None
            } else {
                Some(crate::app::lighting_bindings::discovery::native_factory())
            },
        )
        .activation(o.snapshot.is_some(), o.selftest),
        native_lighting,
        sources: sources::Editor::load(state_dir.join("sensor-sources.json")),
        runtime: Client::new(runtime, o.snapshot.is_none() || o.selftest)?,
        asset_root,
        feed,
        editor: None,
        editor_dirty: false,
        detail: String::new(),
        was_pending: false,
        context_revision: 1,
        lcd_image: vec![],
        lcd_image_size: (0, 0),
        lcd_frame: (0, 0),
        ambient_image: vec![],
        ambient_frame: (0, 0),
        last_status: Instant::now(),
        pending_open: None,
        shutdown: crate::app::shutdown::Shutdown::default(),
        painter,
        widgets: BTreeMap::new(),
        page: o.page.unwrap_or(Page::Scene),
        dpi,
        width: px(w, dpi),
        height: px(h, dpi),
        buffer: None,
        dirty: false,
        status: if o.native_lighting.is_some() {
            "Аппаратная подсветка · проверьте статусы устройств.".into()
        } else {
            "Сцена, карта и расписание · Предпросмотр без подключения устройств".into()
        },
        path: o.project.clone(),
        state_dir,
        groups: [true, true],
    };
    let built = app.build(raw).and_then(|_| {
        if app.page == Page::Connections {
            app.open_connections()?;
        }

        // Preview grants capabilities only; connected startup explicitly starts saved routes.
        let native_sources = native_sources_allowed(o.snapshot.is_some(), o.selftest);
        app.sources.native_allowed = native_sources;
        app.sources.scan_factory = (native_sources || o.allow_sensor_discovery)
            .then(crate::app::sensor_discovery_windows::native_factory);
        if app.sources.scan_factory.is_some() {
            app.sources.scan_message =
                "Поиск выполняется только по кнопке; vendor API и HID metadata не опрашивались."
                    .into();
        }
        if o.source_discovery_page {
            app.select_source_discovery();
        }
        if o.start_connected && !app.source_scope_inputs()?.1.is_empty() {
            app.start_bound_sources()?;
        }
        Ok(())
    });
    // Keep fonts alive until the parent and any partially-created controls
    // are destroyed, including a control-construction failure.
    state.app.replace(Some(app));
    built?;
    let _shell = if resident_mode {
        Some(resident::Shell::create(&state, true)?)
    } else {
        None
    };
    let mut client = RECT::default();
    unsafe {
        GetClientRect(raw, &mut client);
    }
    state
        .app
        .borrow_mut()
        .as_mut()
        .unwrap()
        .layout(client.right, client.bottom);
    if let Some(path) = &o.snapshot {
        // EDIT/COMBOBOX skip portions of WM_PRINT while an ancestor is hidden.
        // Realize only this owned test window off-screen, without activation.
        if unsafe {
            SetWindowPos(
                raw,
                null_mut(),
                -32000,
                -32000,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSIZE | SWP_SHOWWINDOW,
            )
        } == 0
        {
            return Err(last("Realize isolated snapshot window"));
        }
        unsafe {
            UpdateWindow(raw);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        if o.selftest {
            selftest(raw, &state)?;
        }
        let raw = state.editor_window.get();
        {
            let mut slot = state.app.borrow_mut();
            let app = slot.as_mut().unwrap();
            for _ in 0..40 {
                app.tick()?;
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            // GIF/font preparation is asynchronous. Capture the first observed
            // frame (or a real error), rather than an arbitrary 200ms loading
            // placeholder. Interactive startup remains nonblocking.
            let expected_lcd = (app.page == Page::Lcd)
                .then(|| {
                    app.runtime
                        .document()
                        .lcd
                        .get(app.editor.as_ref().map_or(0, |e| e.index))
                        .filter(|lcd| {
                            lcd.config.enabled
                                && app
                                    .runtime
                                    .document()
                                    .lighting
                                    .setup
                                    .devices
                                    .iter()
                                    .any(|d| d.id == lcd.device && d.enabled)
                        })
                        .map(|lcd| lcd.device.clone())
                })
                .flatten();
            if let Some(id) = expected_lcd {
                let until = Instant::now() + std::time::Duration::from_secs(3);
                while app.lcd_image.is_empty()
                    && !app
                        .runtime
                        .lcd_status(&id)
                        .is_some_and(|s| s.error.is_some())
                {
                    if Instant::now() >= until {
                        return Err("Software LCD snapshot did not reach a frame or error".into());
                    }
                    app.tick()?;
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }
        let bitmap = Bitmap::new(client.right, client.bottom)?;
        {
            let slot = state.app.borrow();
            let a = slot.as_ref().unwrap();
            a.painter
                .paint(bitmap.dc, client.right, client.bottom, &a.view())?;
            for handle in a.widgets.values() {
                if unsafe { GetWindowLongPtrW(*handle, GWL_STYLE) } & WS_VISIBLE as isize == 0 {
                    continue;
                }
                let mut rect = RECT::default();
                unsafe {
                    GetWindowRect(*handle, &mut rect);
                }
                let mut origin = POINT {
                    x: rect.left,
                    y: rect.top,
                };
                unsafe {
                    ScreenToClient(raw, &mut origin);
                }
                let saved = unsafe { SaveDC(bitmap.dc) };
                if saved == 0 {
                    return Err(last("Save snapshot context"));
                }
                unsafe {
                    SetViewportOrgEx(bitmap.dc, origin.x, origin.y, null_mut());
                    IntersectClipRect(
                        bitmap.dc,
                        0,
                        0,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                    );
                    SendMessageW(
                        *handle,
                        WM_PRINTCLIENT,
                        bitmap.dc as usize,
                        (PRF_CLIENT | PRF_NONCLIENT | PRF_CHILDREN | PRF_ERASEBKGND) as isize,
                    );
                    RestoreDC(bitmap.dc, saved);
                }
            }
        }
        bitmap.save(path)?;
        let background_runtime = state.app.borrow().as_ref().unwrap().runtime.background();
        let autonomous_rgb = state
            .app
            .borrow()
            .as_ref()
            .unwrap()
            .runtime
            .autonomous_rgb();
        if o.selftest {
            if !background_runtime {
                return Err("GUI transitions lost the background Runtime".into());
            }
            if !state
                .app
                .borrow()
                .as_ref()
                .unwrap()
                .runtime
                .autonomous_rgb()
            {
                return Err("GUI transitions lost the autonomous RGB engine".into());
            }
        }
        println!(
            "{}",
            serde_json::json!({"snapshot":path,"width":client.right,"height":client.bottom,"dpi":dpi,"mode":"software_stand","native_output_factories":0,"autonomous_rgb":autonomous_rgb,"background_runtime":background_runtime,"selftest":o.selftest})
        );
        unsafe {
            DestroyWindow(raw);
        }
        drop(window);
        return Ok(());
    }
    // WebView2 initialization pumps messages before ShowWindow. Keep the existing
    // editor timer live so completion callbacks cannot strand the nested pump.
    if unsafe { SetTimer(raw, 1, 16, None) } == 0 {
        return Err(last("Start preview refresh"));
    }
    if let Err(error) = modern::open(&state)
        && let Some(app) = state.app.borrow_mut().as_mut()
    {
        app.web_mode = false;
        app.status = format!("Новый интерфейс недоступен: {error}");
        app.show_page();
    }
    unsafe {
        ShowWindow(raw, SW_SHOWNORMAL);
        UpdateWindow(raw);
    }
    let mut msg = MSG::default();
    loop {
        let got = unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) };
        if got == 0 {
            break;
        }
        if got < 0 {
            return Err(last("Read native UI message"));
        }
        let value_enter = msg.message == WM_KEYDOWN
            && msg.wParam == 13
            && state
                .app
                .borrow()
                .as_ref()
                .is_some_and(|a| msg.hwnd == a.h(E_VALUE));
        if value_enter {
            unsafe {
                SendMessageW(state.editor_window.get(), WM_COMMAND, E_SET as usize, 0);
            }
            continue;
        }
        let editor = state.editor_window.get();
        if editor.is_null() || unsafe { IsDialogMessageW(editor, &msg) } == 0 {
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    drop(window);
    Ok(())
}
fn selftest(hwnd: HWND, state: &State) -> Result<(), String> {
    {
        let slot = state.app.borrow();
        let app = slot.as_ref().unwrap();
        let start = app
            .runtime
            .owner_ticks()
            .ok_or("GUI selftest requires a background Runtime")?;
        // No App tick, message pumping or engine guard during this interval.
        std::thread::sleep(std::time::Duration::from_millis(160));
        if app.runtime.owner_ticks().unwrap() <= start {
            return Err("Runtime stopped progressing without window ticks".into());
        }
    }
    println!(
        "Background Runtime stand PASS: advances while the window does not tick; no engine lock retained by UI data"
    );
    if !state
        .app
        .borrow()
        .as_ref()
        .unwrap()
        .runtime
        .autonomous_rgb()
    {
        return Err("GUI selftest must exercise the autonomous RGB engine".into());
    }
    {
        let slot = state.app.borrow();
        let a = slot.as_ref().unwrap();
        let mut rect = RECT::default();
        if unsafe { GetWindowRect(a.h(SAVE), &mut rect) } == 0 {
            return Err(last("Measure save action"));
        }
        let mut bottom = POINT {
            x: rect.left,
            y: rect.bottom,
        };
        unsafe {
            ScreenToClient(hwnd, &mut bottom);
        }
        if bottom.y > a.height - px(45, a.dpi) {
            return Err("Save action overlaps the status bar".into());
        }
        if unsafe { SendMessageW(a.h(E_LIST), LB_GETITEMHEIGHT, 0, 0) } != px(26, a.dpi) as isize {
            return Err("Property row height does not follow DPI".into());
        }
    }
    let original_page = state.app.borrow().as_ref().unwrap().page;
    let (bright, effect) = {
        let slot = state.app.borrow();
        let a = slot.as_ref().unwrap();
        (a.h(BRIGHT), a.h(EFFECT))
    };
    unsafe {
        SendMessageW(bright, TBM_SETPOS, 1, 42);
        SendMessageW(hwnd, WM_HSCROLL, 0, bright as isize);
        SendMessageW(effect, CB_SETCURSEL, 1, 0);
        SendMessageW(
            hwnd,
            WM_COMMAND,
            EFFECT as usize | ((CBN_SELCHANGE as usize) << 16),
            effect as isize,
        );
    }
    {
        let slot = state.app.borrow();
        let a = slot.as_ref().unwrap();
        if a.runtime.project().scene.brightness != 42
            || a.runtime.project().scene.effect != Effect::Breathing
        {
            return Err("Native scene control self-test failed".into());
        }
    }
    let file = {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        let path = a.state_dir.join("ui-selftest-project.json");
        a.save(&path)?;
        path
    };
    let (reloaded, _) = load_project(&file)?;
    if reloaded.scene.brightness != 42 {
        return Err("Native project persistence self-test failed".into());
    }
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        let parent = a
            .state_dir
            .join(format!("failed-journal-{}", crate::clock::monotonic_us()));
        std::fs::create_dir(&parent).map_err(|e| e.to_string())?;
        let journal = parent.join("journal.json");
        let temporary = Runtime::new(a.runtime.document().clone(), Some(journal.clone()))?;
        std::fs::remove_file(&journal).map_err(|e| e.to_string())?;
        std::fs::create_dir(&journal).map_err(|e| e.to_string())?;
        let temporary = Client::new(temporary, a.runtime.background())?;
        let prior = std::mem::replace(&mut a.runtime, temporary);
        let enabled = a.runtime.group_on(Target::Pc);
        a.set_check(PC_ON, !enabled);
        let failed = a.command(PC_ON, BN_CLICKED as u16).is_err();
        let consistent = a.checked(PC_ON) == enabled && a.runtime.group_on(Target::Pc) == enabled;
        a.runtime = prior;
        a.sync();
        std::fs::remove_dir(&journal).map_err(|e| e.to_string())?;
        std::fs::remove_dir(&parent).map_err(|e| e.to_string())?;
        if !failed || !consistent {
            return Err("Native checkbox rollback self-test failed".into());
        }
    }
    let has_ambient = !state
        .app
        .borrow()
        .as_ref()
        .unwrap()
        .runtime
        .document()
        .ambient
        .routes
        .is_empty();
    if has_ambient {
        native_edit(hwnd, state, NAV_AMBIENT, "gamma", "1.8")?;
        let slot = state.app.borrow();
        if slot.as_ref().unwrap().runtime.document().ambient.routes[0]
            .processing
            .correction
            .gamma
            != 1.8
        {
            return Err("Native ambient apply self-test failed".into());
        }
        drop(slot);
        unsafe {
            SendMessageW(hwnd, WM_COMMAND, E_TEST as usize, 0);
        }
    }
    let has_lcd = !state
        .app
        .borrow()
        .as_ref()
        .unwrap()
        .runtime
        .document()
        .lcd
        .is_empty();
    if has_lcd {
        native_edit(hwnd, state, NAV_LCD, "rotation", "90")?;
        if state.app.borrow().as_ref().unwrap().runtime.document().lcd[0]
            .config
            .rotation
            != 90
        {
            return Err("Native LCD apply self-test failed".into());
        }
    }
    let has_temperatures = !state
        .app
        .borrow()
        .as_ref()
        .unwrap()
        .runtime
        .document()
        .temperatures
        .sources
        .is_empty();
    if has_temperatures {
        unsafe {
            SendMessageW(hwnd, WM_COMMAND, NAV_TEMPS as usize, 0);
            SendMessageW(hwnd, WM_COMMAND, E_TEST as usize, 0);
        }
    }
    native_device_setup(hwnd, state)?;
    connections::exercise(hwnd, state)?;
    native_ambient_and_lcd_setup(hwnd, state)?;
    native_portable_profile(hwnd, state)?;
    sources::exercise(state)?;
    let nav = match original_page {
        Page::Scene => NAV_SCENE,
        Page::Devices => NAV_DEVICES,
        Page::Schedule => NAV_SCHEDULE,
        Page::Ambient => NAV_AMBIENT,
        Page::Lcd => NAV_LCD,
        Page::Temperatures => NAV_TEMPS,
        Page::Sources => NAV_SOURCES,
        Page::Connections => NAV_CONNECTIONS,
    };
    unsafe {
        SendMessageW(hwnd, WM_COMMAND, nav as usize, 0);
    }
    if original_page == Page::Connections {
        connections::snapshot_scan(state)?;
    }
    resident::exercise(state)?;
    Ok(())
}

fn native_edit(hwnd: HWND, state: &State, nav: i32, key: &str, value: &str) -> Result<(), String> {
    unsafe {
        SendMessageW(hwnd, WM_COMMAND, nav as usize, 0);
    }
    native_edit_current(hwnd, state, key, value)
}
fn native_edit_current(hwnd: HWND, state: &State, key: &str, value: &str) -> Result<(), String> {
    let (list, value_edit, choice, index, choice_index) = {
        let slot = state.app.borrow();
        let a = slot.as_ref().unwrap();
        let fields = a
            .editor
            .as_ref()
            .ok_or("Settings page did not open")?
            .fields();
        let index = fields
            .iter()
            .position(|f| f.key == key)
            .ok_or("Native property missing")?;
        (
            a.h(E_LIST),
            a.h(E_VALUE),
            a.h(E_CHOICE),
            index,
            fields[index].choices.iter().position(|(_, v)| v == value),
        )
    };
    unsafe {
        SendMessageW(list, LB_SETCURSEL, index, 0);
        SendMessageW(
            hwnd,
            WM_COMMAND,
            E_LIST as usize | ((LBN_SELCHANGE as usize) << 16),
            list as isize,
        );
        if let Some(index) = choice_index {
            SendMessageW(choice, CB_SETCURSEL, index, 0);
        } else {
            SetWindowTextW(value_edit, wide(value).as_ptr());
        }
        SendMessageW(hwnd, WM_COMMAND, E_SET as usize, 0);
        SendMessageW(hwnd, WM_COMMAND, E_APPLY as usize, 0);
    }
    native_wait_configuration(state)
}
fn native_wait_configuration(state: &State) -> Result<(), String> {
    let until = Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        a.tick()?;
        if let Some(error) = a.runtime.configuration_error() {
            return Err(error.into());
        }
        if !a.runtime.pending_configuration() && a.pending_open.is_none() {
            return Ok(());
        }
        drop(slot);
        if Instant::now() >= until {
            return Err("Native configuration did not drain".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// Create/apply/save/remove through the real controls and Runtime. Only injected
/// stand factories are available in this process; no desktop or device access.
fn native_ambient_and_lcd_setup(hwnd: HWND, state: &State) -> Result<(), String> {
    let original = state
        .app
        .borrow()
        .as_ref()
        .unwrap()
        .runtime
        .document()
        .clone();
    unsafe {
        SendMessageW(hwnd, WM_COMMAND, NAV_DEVICES as usize, 0);
    }
    {
        let slot = state.app.borrow();
        let a = slot.as_ref().unwrap();
        let preset = crate::app::device_setup::catalog()
            .iter()
            .position(|p| p.id == "SK0410")
            .ok_or("Ambient native preset missing")?;
        unsafe {
            SendMessageW(a.h(E_PRESET), CB_SETCURSEL, preset, 0);
            SetWindowTextW(a.h(E_LED_COUNT), wide("150").as_ptr());
        }
    }
    unsafe {
        SendMessageW(hwnd, WM_COMMAND, E_ADD as usize, 0);
        SendMessageW(hwnd, WM_COMMAND, E_APPLY as usize, 0);
    }
    native_wait_configuration(state)?;
    let id = state
        .app
        .borrow()
        .as_ref()
        .unwrap()
        .runtime
        .document()
        .lighting
        .setup
        .devices
        .last()
        .ok_or("Ambient native device missing")?
        .id
        .clone();
    // The catalog starts RGB strips on the shared canvas. Explicitly select
    // the screen role through the same control a first-time user uses.
    native_edit_current(hwnd, state, "role", "ambient")?;
    unsafe {
        SendMessageW(hwnd, WM_COMMAND, NAV_AMBIENT as usize, 0);
    }
    {
        let slot = state.app.borrow();
        let a = slot.as_ref().unwrap();
        let candidate = a
            .editor
            .as_ref()
            .unwrap()
            .ambient_candidates()
            .iter()
            .position(|(device, _)| device == &id)
            .ok_or("New ambient candidate not offered")?;
        unsafe {
            SendMessageW(a.h(E_ROUTE_DEVICE), CB_SETCURSEL, candidate, 0);
            SendMessageW(a.h(E_ROUTE_SOURCE), CB_SETCURSEL, 0, 0);
        }
    }
    unsafe {
        SendMessageW(hwnd, WM_COMMAND, E_ADD as usize, 0);
        SendMessageW(hwnd, WM_COMMAND, E_APPLY as usize, 0);
    }
    native_wait_configuration(state)?;
    let source = {
        let slot = state.app.borrow();
        let a = slot.as_ref().unwrap();
        let route = a
            .runtime
            .document()
            .ambient
            .routes
            .iter()
            .find(|r| r.device == id)
            .ok_or("Native route creation did not apply")?;
        if route.enabled || route.zone.leds != 150 {
            return Err("New ambient route was enabled or had incorrect geometry".into());
        }
        route.source.clone()
    };
    native_edit_current(hwnd, state, "enabled", "true")?;
    {
        let slot = state.app.borrow();
        let a = slot.as_ref().unwrap();
        if !a
            .runtime
            .document()
            .lighting
            .setup
            .devices
            .iter()
            .any(|d| d.id == id && d.enabled)
        {
            return Err("Ambient toggle did not enable its output".into());
        }
    }
    unsafe {
        SendMessageW(hwnd, WM_COMMAND, NAV_AMBIENT as usize, 0);
    }
    {
        let slot = state.app.borrow();
        let a = slot.as_ref().unwrap();
        let i = a
            .runtime
            .document()
            .ambient
            .routes
            .iter()
            .position(|r| r.device == id)
            .unwrap();
        unsafe {
            SendMessageW(a.h(E_TARGET), CB_SETCURSEL, i, 0);
        }
    }
    unsafe {
        SendMessageW(
            hwnd,
            WM_COMMAND,
            E_TARGET as usize | ((CBN_SELCHANGE as usize) << 16),
            0,
        );
        SendMessageW(hwnd, WM_COMMAND, E_TEST as usize, 0);
    }
    let until = Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        a.tick()?;
        if a.runtime
            .capture_status()
            .is_some_and(|s| s.frames.get(&id).is_some_and(|f| f.len() == 150))
        {
            let path = a.state_dir.join("ui-created-ambient.json");
            a.save(&path)?;
            let saved = Document::load(&path)?;
            if !saved
                .ambient
                .routes
                .iter()
                .any(|r| r.device == id && r.source == source && r.enabled)
            {
                return Err("Created ambient route did not survive save/open".into());
            }
            break;
        }
        drop(slot);
        if Instant::now() >= until {
            return Err("Created ambient route produced no synthetic frame".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    unsafe {
        SendMessageW(hwnd, WM_COMMAND, E_REMOVE as usize, 0);
        SendMessageW(hwnd, WM_COMMAND, E_APPLY as usize, 0);
    }
    native_wait_configuration(state)?;
    {
        let slot = state.app.borrow();
        let d = slot.as_ref().unwrap().runtime.document();
        if d.ambient.routes.iter().any(|r| r.device == id)
            || !d.ambient.sources.iter().any(|s| s.key == source)
            || d.lighting
                .setup
                .devices
                .iter()
                .any(|d| d.id == id && d.enabled)
        {
            return Err("Removing ambient route damaged source or left output enabled".into());
        }
    }
    unsafe {
        SendMessageW(hwnd, WM_COMMAND, NAV_LCD as usize, 0);
        SendMessageW(hwnd, WM_COMMAND, E_ADD as usize, 0);
        SendMessageW(hwnd, WM_COMMAND, E_APPLY as usize, 0);
    }
    native_wait_configuration(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        let lcd = a
            .runtime
            .document()
            .lcd
            .last()
            .ok_or("Native LCD not created")?;
        if a.runtime.document().lcd.len() != original.lcd.len() + 1
            || lcd.config.enabled
            || lcd.config.media.is_some()
        {
            return Err("Native LCD creation changed the safe initial state".into());
        }
        // Restore this test's exact document through Runtime's drain/apply path.
        let journal = journal_path(&a.state_dir, a.path.as_deref(), &original);
        a.runtime.request_document_at(original.clone(), journal)?;
        a.was_pending = a.runtime.pending_configuration();
    }
    native_wait_configuration(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        if a.runtime.document() != &original {
            return Err("Native setup stand did not restore its document".into());
        }
        a.reset_editor(0);
    }
    Ok(())
}
/// Same file-operation handlers as the dialogs, with isolated explicit paths.
/// The system font is read only for this private stand and never committed.
fn native_portable_profile(hwnd: HWND, state: &State) -> Result<(), String> {
    let (original, old_assets, root) = {
        let slot = state.app.borrow();
        let a = slot.as_ref().unwrap();
        (
            a.runtime.document().clone(),
            a.asset_root.clone(),
            std::fs::canonicalize(&a.state_dir).map_err(|e| e.to_string())?,
        )
    };
    let (project, _) = project(&Options::default())?;
    let mut editor = super::editor::Editor {
        document: Document::from_lighting(project)?,
        page: Page::Lcd,
        index: 0,
    };
    editor.add_lcd_device()?;
    editor
        .document
        .temperatures
        .sources
        .push(crate::app::sensors::Source {
            key: "cpu".into(),
            value: crate::app::sensors::Value::Sensor {
                input: "stand-cpu".into(),
            },
        });
    editor.document.lcd[0]
        .config
        .sources
        .push(crate::app::lcd::OverlaySource {
            key: "cpu".into(),
            label: "CPU".into(),
        });
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        let journal = journal_path(&a.state_dir, None, &editor.document);
        a.runtime.request_document_at(editor.document, journal)?;
        a.was_pending = a.runtime.pending_configuration();
    }
    native_wait_configuration(state)?;
    unsafe {
        SendMessageW(hwnd, WM_COMMAND, NAV_LCD as usize, 0);
    }
    let gif_path = root.join("portable-source.gif");
    {
        let file = File::create(&gif_path).map_err(|e| e.to_string())?;
        let mut encoder = image::codecs::gif::GifEncoder::new(file);
        for rgba in [[255, 64, 0, 255], [128, 16, 0, 255]] {
            encoder
                .encode_frame(image::Frame::from_parts(
                    image::RgbaImage::from_pixel(2, 2, image::Rgba(rgba)),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(100, 1),
                ))
                .map_err(|e| e.to_string())?;
        }
    }
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        import_asset(a, &gif_path, "media")?;
        import_asset(a, Path::new("C:/Windows/Fonts/segoeui.ttf"), "font")?;
    }
    native_edit_current(hwnd, state, "overlay", "true")?;
    native_edit_current(hwnd, state, "enabled", "true")?;
    let archive = root.join("portable-profile.zip");
    let expected = {
        let slot = state.app.borrow();
        let a = slot.as_ref().unwrap();
        if a.export_profile(&archive)? != 2 {
            return Err("Portable export did not include GIF and font".into());
        }
        a.runtime.document().clone()
    };
    let fresh_assets = root.join("portable-fresh-assets");
    let alias = root.join("portable-renamed.json");
    std::fs::copy(&archive, &alias).map_err(|e| e.to_string())?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        let bad = root.join("portable-invalid.zip");
        std::fs::write(&bad, b"invalid-zip").map_err(|e| e.to_string())?;
        if a.open_profile(&bad).is_ok()
            || a.pending_open.is_some()
            || a.runtime.document() != &expected
        {
            return Err("Invalid package changed the active profile".into());
        }
        a.asset_root = fresh_assets.clone();
        a.open_profile(&alias)?; // Recognize content even after a filename change.
    }
    native_wait_configuration(state)?;
    let until = Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        let now = crate::clock::monotonic_us();
        a.runtime.accept_temperatures(
            &[crate::app::lcd::Sample {
                key: "stand-cpu".into(),
                celsius: 44.5,
                observed_ms: now / 1000,
            }],
            now,
        )?;
        a.tick()?;
        if a.runtime.document() != &expected {
            return Err("Imported portable document differs".into());
        }
        if a.runtime
            .lcd_status(&expected.lcd[0].device)
            .is_some_and(|s| s.frames_sent > 0 && !s.preview.is_empty())
        {
            if std::fs::read_dir(&fresh_assets)
                .map_err(|e| e.to_string())?
                .count()
                != 2
            {
                return Err("Portable import installed unexpected files".into());
            }
            let store = crate::app::assets::Store::new(fresh_assets.clone())?;
            let font = store.read(
                expected.lcd[0].config.font.as_ref().unwrap(),
                8 * 1024 * 1024,
            )?;
            crate::lcd::overlay::Renderer::new(font)?;
            break;
        }
        drop(slot);
        if Instant::now() >= until {
            return Err("Imported GIF/font produced no software LCD frame".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        a.asset_root = old_assets;
        let journal = journal_path(&a.state_dir, a.path.as_deref(), &original);
        a.runtime.request_document_at(original.clone(), journal)?;
        a.was_pending = a.runtime.pending_configuration();
    }
    native_wait_configuration(state)?;
    let mut slot = state.app.borrow_mut();
    let a = slot.as_mut().unwrap();
    if a.runtime.document() != &original {
        return Err("Portable stand failed to restore its document".into());
    }
    a.reset_editor(0);
    Ok(())
}
fn native_device_setup(hwnd: HWND, state: &State) -> Result<(), String> {
    unsafe {
        SendMessageW(hwnd, WM_COMMAND, NAV_DEVICES as usize, 0);
    }
    let original = state
        .app
        .borrow()
        .as_ref()
        .unwrap()
        .runtime
        .document()
        .clone();
    for (preset, count) in [("door20", ""), ("SK0410", "150")] {
        let index = crate::app::device_setup::catalog()
            .iter()
            .position(|p| p.id == preset)
            .ok_or("Native preset missing")?;
        {
            let slot = state.app.borrow();
            let a = slot.as_ref().unwrap();
            unsafe {
                SendMessageW(a.h(E_PRESET), CB_SETCURSEL, index, 0);
                SetWindowTextW(a.h(E_LED_COUNT), wide(count).as_ptr());
            }
        }
        unsafe {
            SendMessageW(hwnd, WM_COMMAND, E_ADD as usize, 0);
            SendMessageW(hwnd, WM_COMMAND, E_APPLY as usize, 0);
        }
        native_wait_configuration(state)?;
        {
            let slot = state.app.borrow();
            let d = slot.as_ref().unwrap().runtime.document();
            if d.lighting.setup.devices.len() != original.lighting.setup.devices.len() + 1 {
                return Err("Native add device failed".into());
            }
            let added = d
                .lighting
                .setup
                .devices
                .last()
                .ok_or("New device missing")?;
            if added.enabled || (!count.is_empty() && added.leds != 150) {
                return Err("New device state or explicit LED count mismatch".into());
            }
        }
        unsafe {
            SendMessageW(hwnd, WM_COMMAND, E_REMOVE as usize, 0);
            SendMessageW(hwnd, WM_COMMAND, E_APPLY as usize, 0);
        }
        native_wait_configuration(state)?;
        if state.app.borrow().as_ref().unwrap().runtime.document() != &original {
            return Err("Native remove device changed unrelated configuration".into());
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connected_startup_requires_a_profile_and_excludes_diagnostics_and_legacy_mode() {
        assert!(!options(vec![]).unwrap().start_connected);
        assert!(options(vec!["--start-connected".into()]).is_err());
        let args = vec![
            "--start-connected".into(),
            "--project".into(),
            "profile.json".into(),
        ];
        assert!(options(args.clone()).unwrap().start_connected);
        for tail in [
            vec!["--selftest"],
            vec!["--snapshot", "screen.png"],
            vec!["--native-lighting", "local.json"],
            vec!["--start-connected"],
        ] {
            let mut rejected = args.clone();
            rejected.extend(tail.into_iter().map(OsString::from));
            assert!(options(rejected).is_err());
        }
    }
    #[test]
    fn native_lighting_requires_explicit_private_file_and_cannot_run_in_capture_tests() {
        for flag in ["--allow-nvidia-discovery", "--allow-sensor-discovery"] {
            assert!(options(vec![flag.into(), "--selftest".into()]).is_err());
            assert!(options(vec![flag.into(), "--snapshot".into(), "screen.png".into()]).is_err());
        }
        assert!(options(vec![]).unwrap().native_lighting.is_none());
        assert!(options(vec!["--native-lighting".into()]).is_err());
        assert!(options(vec!["--native-lighting".into(), "--selftest".into()]).is_err());
        let enabled = options(vec!["--native-lighting".into(), "local.json".into()]).unwrap();
        assert_eq!(enabled.native_lighting, Some(PathBuf::from("local.json")));
        for tail in [
            vec!["--selftest"],
            vec!["--snapshot", "screen.png"],
            vec!["--native-lighting", "other.json"],
        ] {
            let mut args = vec!["--native-lighting".into(), "local.json".into()];
            args.extend(tail.into_iter().map(OsString::from));
            assert!(options(args).is_err());
        }
    }

    #[test]
    fn discovery_page_selection_does_not_grant_native_metadata_access() {
        let preview = options(vec!["--page".into(), "source-discovery".into()]).unwrap();
        assert_eq!(preview.page, Some(Page::Sources));
        assert!(preview.source_discovery_page);
        assert!(!preview.allow_sensor_discovery);
        let explicit = options(vec![
            "--allow-sensor-discovery".into(),
            "--page".into(),
            "source-discovery".into(),
        ])
        .unwrap();
        assert!(explicit.allow_sensor_discovery);
    }
    #[test]
    fn journal_key_separates_unsaved_topology_but_not_scene() {
        let (p, _) = project(&Options::default()).unwrap();
        let mut d = Document::from_lighting(p).unwrap();
        let root = Path::new("stand");
        let before = journal_path(root, None, &d);
        d.lighting.scene.brightness = 42;
        assert_eq!(before, journal_path(root, None, &d));
        d.lighting.setup.canvas.width += 1;
        assert_ne!(before, journal_path(root, None, &d));
        assert_ne!(
            journal_path(root, Some(Path::new("a.json")), &d),
            journal_path(root, Some(Path::new("b.json")), &d)
        );
    }
    #[test]
    fn hints_wrap_at_words_and_keep_paragraphs() {
        assert_eq!(
            wrap_words("Один два три\nЧетыре пять", |s| s
                .chars()
                .count()
                <= 8),
            "Один два\r\nтри\r\nЧетыре\r\nпять"
        );
    }
    #[test]
    fn time_inputs_are_explicit_and_bounded() {
        for (s, m) in [
            ("00:00", 0),
            ("10:00", 600),
            ("17:00", 1020),
            ("02:00", 120),
            ("23:59", 1439),
        ] {
            assert_eq!(parse_time(s).unwrap(), m);
        }
        for s in ["24:00", "10:60", "-1:10", "now", "", ":"] {
            assert!(parse_time(s).is_err());
        }
    }
    #[test]
    fn snapshot_options_cannot_enable_hardware() {
        assert!(options(vec!["--allow-sensor-discovery".into(), "--selftest".into()]).is_err());
        assert!(
            options(vec![
                "--allow-sensor-discovery".into(),
                "--snapshot".into(),
                "never-created.png".into()
            ])
            .is_err()
        );
        assert!(
            options(vec!["--allow-sensor-discovery".into()])
                .unwrap()
                .allow_sensor_discovery
        );
        assert!(options(vec!["--native".into(), "yes".into()]).is_err());
        assert!(options(vec!["--dpi".into(), "0".into()]).is_err());
        assert!(options(vec!["--size".into(), "2x2".into()]).is_err());
        let o = options(vec![
            "--page".into(),
            "schedule".into(),
            "--dpi".into(),
            "144".into(),
        ])
        .unwrap();
        assert_eq!(o.page, Some(Page::Schedule));
        assert_eq!(o.dpi, Some(144));
    }
    #[test]
    fn uncommon_valid_rate_is_displayed_without_rounding() {
        let values = rates(49);
        assert_eq!(values, [15, 25, 30, 48, 49, 60, 144]);
        assert_eq!(rates(48), [15, 25, 30, 48, 60, 144]);
    }
    #[test]
    fn editing_scene_keeps_extended_lighting_document_fields() {
        let (project, _) = project(&Options::default()).unwrap();
        let mut document = Document::from_lighting(project.clone()).unwrap();
        document
            .temperatures
            .sources
            .push(crate::app::sensors::Source {
                key: "cpu".into(),
                value: crate::app::sensors::Value::Sensor {
                    input: "package".into(),
                },
            });
        let mut edited = project;
        edited.scene.brightness = 31;
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("work/app-lighting-stand");
        std::fs::create_dir_all(&parent).unwrap();
        let path = parent.join(format!(
            "ui-document-{}-{}.json",
            std::process::id(),
            crate::clock::monotonic_us()
        ));
        save_project(&path, &edited, Some(&document)).unwrap();
        let (read, extended) = load_project(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(read.scene.brightness, 31);
        assert_eq!(extended.unwrap().temperatures, document.temperatures);
        assert_eq!(document.lighting.scene.brightness, 60);
    }
}

#[cfg(test)]
mod sensor_access_tests {
    #[test]
    fn only_ordinary_mode_grants_dormant_native_source_capabilities() {
        assert!(super::native_sources_allowed(false, false));
        for (snapshot, selftest) in [(true, false), (false, true), (true, true)] {
            assert!(!super::native_sources_allowed(snapshot, selftest));
        }
    }
}
