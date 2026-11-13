//! Private binding authoring; scan and save are deliberately separate from activation.
use super::*;
use crate::app::lighting_bindings::{
    Draft, Workspace,
    discovery::{self, Target as BindingTarget},
};
#[path = "connections_stand.rs"]
mod stand;
pub(super) fn exercise(hwnd: HWND, state: &State) -> Result<(), String> {
    stand::exercise(hwnd, state)?;
    stand::exercise_openrgb(state)?;
    stand::exercise_home_assistant(state)
}
pub(super) fn snapshot_scan(state: &State) -> Result<(), String> {
    stand::snapshot_scan(state)
}
const TARGET_LABEL: i32 = 1800;
const TARGET: i32 = 1801;
const SCAN: i32 = 1802;
const LIST: i32 = 1803;
const NOTE: i32 = 1804;
const ENDPOINT_LABEL: i32 = 1805;
const ENDPOINT: i32 = 1806;
const IDENTITY_LABEL: i32 = 1807;
const IDENTITY: i32 = 1808;
const SERIAL_LABEL: i32 = 1809;
const SERIAL: i32 = 1810;
const SAVE_BINDING: i32 = 1811;
const RESET_BINDING: i32 = 1812;
const REMOVE: i32 = 1813;
const PRUNE: i32 = 1814;
const PATH: i32 = 1815;
const RESULT_LABEL: i32 = 1816;
const PATH_LABEL: i32 = 1817;
const APPLY_OUTPUTS: i32 = 1818;
const REVERT_OUTPUTS: i32 = 1819;
const ACTIVE_STATUS: i32 = 1820;
const SAVE_KEY: i32 = 1821;
type PlanFactory = std::sync::Arc<
    dyn Fn(
            &crate::app::lighting_outputs::Local,
            &Document,
            &std::path::Path,
        ) -> Result<crate::app::runtime::OutputPlan, String>
        + Send
        + Sync,
>;
struct Activation {
    factory: PlanFactory,
    native: bool,
}
struct Previous {
    document: Document,
    applied_document: Document,
    plan: crate::app::runtime::OutputPlan,
    local: Option<crate::app::lighting_outputs::Local>,
}
struct Pending {
    reverting: bool,
    revision: u64,
    local: Option<crate::app::lighting_outputs::Local>,
    previous: Previous,
}

pub(super) fn is_control(id: i32) -> bool {
    (TARGET_LABEL..=SAVE_KEY).contains(&id)
}
pub(super) struct Editor {
    activation: Option<Activation>,
    pending: Option<Pending>,
    previous: Option<Previous>,
    watch_outputs: bool,
    stand_keys: BTreeMap<String, String>,
    path: PathBuf,
    factory: Option<discovery::Factory>,
    pub(super) scan: discovery::Worker,
    workspace: Option<Workspace>,
    targets: Vec<BindingTarget>,
    selected: usize,
    inventory: Option<discovery::Inventory>,
    scan_endpoint: Option<String>,
    controllers: BTreeMap<String, crate::transport::openrgb::ControllerInfo>,
    wled_controllers: BTreeMap<String, crate::transport::wled_info::Info>,
    surplife_points: BTreeMap<String, (u64, usize)>,
    edited: bool,
    removed: bool,
    syncing: bool,
}
impl Editor {
    pub(super) fn new(path: PathBuf, factory: Option<discovery::Factory>) -> Self {
        Self {
            activation: None,
            pending: None,
            previous: None,
            watch_outputs: false,
            stand_keys: BTreeMap::new(),
            path,
            factory,
            scan: Default::default(),
            workspace: None,
            targets: vec![],
            selected: 0,
            inventory: None,
            scan_endpoint: None,
            controllers: BTreeMap::new(),
            wled_controllers: BTreeMap::new(),
            surplife_points: BTreeMap::new(),
            edited: false,
            removed: false,
            syncing: false,
        }
    }
    pub(super) fn activation(mut self, diagnostic: bool, selftest: bool) -> Self {
        if selftest {
            self.activation = Some(Activation {
                native: false,
                factory: std::sync::Arc::new(|_, doc, root| {
                    super::super::stand::prepare(doc, root, &super::super::stand::Feed::default())
                }),
            });
        } else if !diagnostic {
            self.activation = Some(Activation {
                native: true,
                factory: std::sync::Arc::new(|local, doc, root| local.prepare(doc, root)),
            });
        }
        self
    }
    pub(super) fn stopped(&mut self) {
        self.pending = None;
        self.previous = None;
        self.watch_outputs = false;
    }
    fn current(&self) -> Option<&BindingTarget> {
        self.targets.get(self.selected)
    }
    fn scannable(&self) -> bool {
        matches!(self.current(), Some(BindingTarget::Monitor(_)))
            || matches!(self.current(),Some(BindingTarget::Device(d)) if matches!(d.driver,crate::devices::Driver::HomeAssistant|crate::devices::Driver::OpenRgb|crate::devices::Driver::WledDdp|crate::devices::Driver::SurplifeE1Pixels|crate::devices::Driver::Fusion2{..}|crate::devices::Driver::Valkyrie{..}|crate::devices::Driver::TtLcd|crate::devices::Driver::SkydimoSerial{..}))
    }
    fn home_assistant(&self) -> bool {
        matches!(self.current(),Some(BindingTarget::Device(d)) if d.driver==crate::devices::Driver::HomeAssistant)
    }
    fn wled(&self) -> bool {
        matches!(self.current(),Some(BindingTarget::Device(d)) if d.driver==crate::devices::Driver::WledDdp)
    }
    fn key(&self, id: &str) -> Result<String, String> {
        match self.activation.as_ref().map(|a| a.native) {
            Some(true) => crate::credentials_windows::read(id)
                .map_err(|e| e.to_string())?
                .ok_or("Сначала сохраните ключ Home Assistant.".into()),
            Some(false) => self
                .stand_keys
                .get(id)
                .cloned()
                .ok_or("Сначала сохраните тестовый ключ.".into()),
            None => Err("Ключи в диагностическом снимке недоступны.".into()),
        }
    }
    fn dirty(&self) -> bool {
        self.edited || self.workspace.as_ref().is_some_and(|w| w.dirty)
    }
}
impl App {
    pub(super) fn build_connections(&mut self, parent: HWND) -> Result<(), String> {
        self.control(parent, ACTIVE_STATUS, "STATIC", "", 0)?;
        self.control(
            parent,
            TARGET_LABEL,
            "STATIC",
            "Устройство или источник экрана",
            0,
        )?;
        self.control(
            parent,
            TARGET,
            "COMBOBOX",
            "",
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
        )?;
        self.control(
            parent,
            SCAN,
            "BUTTON",
            "Найти устройства",
            BS_PUSHBUTTON as u32,
        )?;
        self.control(parent, RESULT_LABEL, "STATIC", "Результаты поиска", 0)?;
        self.control(
            parent,
            LIST,
            "LISTBOX",
            "",
            LBS_NOTIFY as u32 | WS_VSCROLL | WS_BORDER,
        )?;
        for (label, field, text) in [
            (ENDPOINT_LABEL, ENDPOINT, "Подключение"),
            (IDENTITY_LABEL, IDENTITY, "Дополнительно"),
            (SERIAL_LABEL, SERIAL, "ID контроллера (HEX)"),
        ] {
            self.control(parent, label, "STATIC", text, 0)?;
            self.control(parent, field, "EDIT", "", WS_BORDER | ES_AUTOHSCROLL as u32)?;
            unsafe {
                SendMessageW(self.h(field), EM_SETLIMITTEXT, 4096, 0);
            }
        }
        for (id, label) in [
            (SAVE_KEY, "Сохранить ключ"),
            (SAVE_BINDING, "Сохранить"),
            (RESET_BINDING, "Отменить правки"),
            (REMOVE, "Удалить привязку"),
            (PRUNE, "Убрать отсутствующие"),
            (APPLY_OUTPUTS, "Применить"),
            (REVERT_OUTPUTS, "Вернуть"),
        ] {
            self.control(parent, id, "BUTTON", label, BS_PUSHBUTTON as u32)?;
        }
        self.control(parent, PATH_LABEL, "STATIC", "Локальный файл", 0)?;
        self.control(
            parent,
            PATH,
            "EDIT",
            "",
            ES_READONLY as u32 | ES_AUTOHSCROLL as u32 | WS_BORDER,
        )?;
        self.control(
            parent,
            NOTE,
            "EDIT",
            "",
            ES_READONLY as u32 | ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | WS_VSCROLL,
        )?;
        Ok(())
    }
    pub(super) fn connection_visible(&self, id: i32) -> bool {
        if self.page != Page::Connections {
            return false;
        }
        match id {
            ENDPOINT | ENDPOINT_LABEL => self.connections.current().is_some(),
            IDENTITY | IDENTITY_LABEL => {
                matches!(self.connections.current(),Some(BindingTarget::Device(d)) if matches!(d.driver,crate::devices::Driver::HomeAssistant|crate::devices::Driver::WledDdp|crate::devices::Driver::SkydimoSerial{..}|crate::devices::Driver::EneDram))
            }
            SAVE_KEY => self.connections.home_assistant(),
            SERIAL | SERIAL_LABEL => {
                matches!(self.connections.current(),Some(BindingTarget::Device(d)) if matches!(d.driver,crate::devices::Driver::HomeAssistant|crate::devices::Driver::WledDdp|crate::devices::Driver::SkydimoSerial{..}))
            }
            _ => true,
        }
    }
    pub(super) fn connections_layout(&self, w: i32, h: i32) {
        let r = right_panel(w);
        let left = r - CONTENT_LEFT - 24;
        self.pos(ACTIVE_STATUS, CONTENT_LEFT, 174, w - CONTENT_LEFT - 24, 18);
        self.pos(TARGET_LABEL, CONTENT_LEFT, 194, left, 22);
        self.pos(TARGET, CONTENT_LEFT, 220, left, 220);
        self.pos(SCAN, r, 220, 280, 30);
        self.pos(RESULT_LABEL, CONTENT_LEFT, 246, left, 20);
        self.pos(LIST, CONTENT_LEFT, 268, left, (h - 416).max(120));
        self.pos(NOTE, CONTENT_LEFT, h - 140, left, 64);
        for (label, field, y) in [
            (ENDPOINT_LABEL, ENDPOINT, 262),
            (IDENTITY_LABEL, IDENTITY, 316),
            (SERIAL_LABEL, SERIAL, 370),
        ] {
            self.pos(label, r, y, 280, 22);
            self.pos(field, r, y + 24, 280, 26);
        }
        if self.connections.home_assistant() {
            self.pos(SERIAL, r, 394, 136, 26);
        }
        self.pos(SAVE_KEY, r + 144, 394, 136, 26);
        self.pos(SAVE_BINDING, r, 430, 132, 28);
        self.pos(RESET_BINDING, r + 144, 430, 136, 28);
        self.pos(REMOVE, r, 466, 280, 28);
        self.pos(PRUNE, r, 502, 280, 28);
        self.pos(APPLY_OUTPUTS, r, 538, 132, 28);
        self.pos(REVERT_OUTPUTS, r + 144, 538, 136, 28);
        self.pos(PATH_LABEL, CONTENT_LEFT, h - 71, 110, 22);
        self.pos(
            PATH,
            CONTENT_LEFT + 116,
            h - 71,
            w - CONTENT_LEFT - 24 - 116,
            22,
        );
    }
    pub(super) fn guard_connections_close(&self) -> Result<(), String> {
        if self.page == Page::Connections && self.connections.dirty() {
            Err("Сохраните локальные привязки или нажмите «Отменить правки».".into())
        } else {
            Ok(())
        }
    }
    pub(super) fn guard_connections_exit(&self) -> Result<(), String> {
        if self.page == Page::Connections && self.connections.pending.is_some() {
            Err("Дождитесь применения подключений или отмените его.".into())
        } else if self.page == Page::Connections && self.connections.dirty() {
            Err("Сохраните локальные привязки или нажмите «Отменить правки».".into())
        } else {
            Ok(())
        }
    }
    pub(super) fn open_connections(&mut self) -> Result<(), String> {
        if self.connections.dirty() {
            return Err("Сначала сохраните или отмените правки подключения.".into());
        }
        if !self.connections.scan.is_idle() {
            return Err("Дождитесь завершения поиска устройств.".into());
        }
        self.connections.workspace = Some(Workspace::open(
            &self.connections.path,
            self.runtime.document(),
        )?);
        self.connections.targets = self
            .runtime
            .document()
            .lighting
            .setup
            .devices
            .iter()
            .cloned()
            .map(BindingTarget::Device)
            .chain(
                self.runtime
                    .document()
                    .ambient
                    .sources
                    .iter()
                    .map(|s| BindingTarget::Monitor(s.key.clone())),
            )
            .collect();
        self.connections.selected = self
            .connections
            .selected
            .min(self.connections.targets.len().saturating_sub(1));
        unsafe {
            SendMessageW(self.h(TARGET), CB_RESETCONTENT, 0, 0);
        }
        for target in &self.connections.targets {
            let label = match target {
                BindingTarget::Device(d) => format!("{} · {}", d.label, d.id),
                BindingTarget::Monitor(key) => format!("Экран · {key}"),
            };
            self.add(TARGET, &label);
        }
        unsafe {
            SendMessageW(self.h(TARGET), CB_SETCURSEL, self.connections.selected, 0);
        }
        self.load_connection_fields();
        Ok(())
    }
    fn load_connection_fields(&mut self) {
        let draft = self
            .connections
            .workspace
            .as_ref()
            .and_then(|w| match self.connections.current()? {
                BindingTarget::Device(d) => w.local.devices.get(&d.id).map(Draft::from_binding),
                BindingTarget::Monitor(key) => w
                    .local
                    .monitors
                    .monitors
                    .iter()
                    .find(|m| &m.source == key)
                    .map(|m| Draft {
                        endpoint: m.device_name.clone(),
                        ..Default::default()
                    }),
            })
            .unwrap_or_default();
        self.connections.syncing = true;
        unsafe {
            SendMessageW(
                self.h(SERIAL),
                EM_SETPASSWORDCHAR,
                if self.connections.home_assistant() {
                    0x2022
                } else {
                    0
                },
                0,
            );
        }
        self.set_text(
            SERIAL_LABEL,
            if self.connections.home_assistant() {
                "Новый ключ (пусто = сохранённый)"
            } else {
                if self.connections.wled() {
                    "HTTP WLED (пусто = порт 80)"
                } else {
                    "ID контроллера (HEX)"
                }
            },
        );
        self.set_text(ENDPOINT, &draft.endpoint);
        self.set_text(IDENTITY, &draft.identity);
        self.set_text(
            SERIAL,
            if self.connections.home_assistant() {
                ""
            } else {
                &draft.serial
            },
        );
        self.connections.syncing = false;
        self.connections.edited = false;
        self.connections.removed = false;
        self.connections.inventory = None;
        unsafe {
            SendMessageW(self.h(LIST), LB_RESETCONTENT, 0, 0);
        }
        let label = match self.connections.current() {
            Some(BindingTarget::Monitor(_)) => r"Экран Windows (например, \\.\DISPLAY1)",
            Some(BindingTarget::Device(d)) => match d.driver {
                crate::devices::Driver::HomeAssistant => "Адрес Home Assistant (http://IP:8123)",
                crate::devices::Driver::OpenRgb => "SDK-сервер OpenRGB (127.0.0.1:6742)",
                crate::devices::Driver::WledDdp => "IP:порт WLED DDP (обычно 4048)",
                crate::devices::Driver::DoorBle | crate::devices::Driver::SurplifeE1Pixels => {
                    "MAC Bluetooth-контроллера"
                }
                crate::devices::Driver::EneDram => "Проверенный модуль PawnIO",
                crate::devices::Driver::SkydimoSerial { .. } => "COM-порт контроллера",
                _ => "Путь интерфейса HID",
            },
            None => "Подключение",
        };
        self.set_text(ENDPOINT_LABEL, label);
        self.set_text(IDENTITY_LABEL,if self.connections.home_assistant(){"Светильник (light.office)"}else if self.connections.wled(){"Идентичность WLED (из поиска)"}else if matches!(self.connections.current(),Some(BindingTarget::Device(d)) if matches!(d.driver,crate::devices::Driver::EneDram)){"Адрес RAM (0x70..0x76)"}else{"USB instance (из метаданных)"});
        self.refresh_connections();
        self.connections_layout(self.width, self.height);
        self.show_page();
    }
    pub(super) fn refresh_connections(&self) {
        self.set_text(PATH, &self.connections.path.to_string_lossy());
        let n = self.connections.workspace.as_ref().map_or(0, |w| {
            w.orphan_ids(self.runtime.document()).len()
                + w.local
                    .monitors
                    .monitors
                    .iter()
                    .filter(|m| {
                        !self
                            .runtime
                            .document()
                            .ambient
                            .sources
                            .iter()
                            .any(|s| s.key == m.source)
                    })
                    .count()
        });
        let note = if !self.connections.scan.is_idle() {
            if matches!(self.connections.current(),Some(BindingTarget::Device(d)) if d.driver == crate::devices::Driver::SurplifeE1Pixels)
            {
                "Проверка Surplife: подключение по BLE и чтение возможностей; цвет и конфигурация не меняются.".into()
            } else {
                "Поиск устройств… RGB-команды не отправляются.".into()
            }
        } else if self.connections.targets.is_empty() {
            format!(
                "Сначала добавьте устройство на вкладке «Устройства» или источник экрана. Привязок удалённых источников: {n}."
            )
        } else if let Some(inventory) = &self.connections.inventory {
            format!(
                "Найдено: {}. Выберите результат, сохраните привязку и нажмите «Применить». {}",
                inventory.candidates.len(),
                inventory.issues.join(" ")
            )
        } else {
            let hint = match self.connections.current() {
                Some(BindingTarget::Device(d)) => match d.driver {
                    crate::devices::Driver::HomeAssistant => {
                        "Пустой адрес: найти сервер в сети. Затем сохраните ключ и найдите светильник. Вывод до 1 Гц."
                    }
                    crate::devices::Driver::OpenRgb => {
                        "Поиск читает SDK; «Применить» включает Direct."
                    }
                    crate::devices::Driver::WledDdp => {
                        "Пустой адрес: найти WLED в сети. Можно указать IP:4048 вручную. Поиск читает число LED и идентичность."
                    }
                    crate::devices::Driver::DoorBle | crate::devices::Driver::SurplifeE1Pixels => {
                        "Укажите MAC-адрес ленты. Автоматический поиск для этого протокола не поддерживается."
                    }
                    crate::devices::Driver::EneDram => {
                        "Укажите проверенный модуль доступа и адрес. Сканирование шины здесь не выполняется."
                    }
                    crate::devices::Driver::SkydimoSerial { .. } => {
                        "Поиск показывает USB-порты CH340, но не подтверждает модель ленты. Укажите также известный HEX ID контроллера."
                    }
                    _ => {
                        "Найдите устройство и выберите результат или укажите известный путь вручную."
                    }
                },
                _ => "Найдите монитор и выберите его из списка.",
            };
            format!(
                "{hint} Сохранение не переключает работающие устройства. Привязок удалённых источников: {n}."
            )
        };
        self.set_text(NOTE, &note);
        let active = if self.connections.pending.is_some() {
            "Сейчас: меняются подключения подсветки."
        } else if let Some(local) = &self.native_lighting {
            let same = self.connections.workspace.as_ref().is_some_and(|saved| {
                match self.connections.current() {
                    Some(BindingTarget::Device(d)) => {
                        local.devices.get(&d.id) == saved.local.devices.get(&d.id)
                    }
                    Some(BindingTarget::Monitor(key)) => {
                        local
                            .monitors
                            .monitors
                            .iter()
                            .find(|m| &m.source == key)
                            .map(|m| &m.device_name)
                            == saved
                                .local
                                .monitors
                                .monitors
                                .iter()
                                .find(|m| &m.source == key)
                                .map(|m| &m.device_name)
                    }
                    None => true,
                }
            });
            if same {
                "Сейчас: аппаратный набор. Выбранная привязка совпадает с сохранённой."
            } else {
                "Сейчас: аппаратный набор. Сохранённая привязка отличается от применённой."
            }
        } else {
            "Сейчас: программный предпросмотр; физические выходы не подключены."
        };
        self.set_text(ACTIVE_STATUS, active);
        let switching = self.connections.pending.is_some();
        let ready = self.connections.workspace.is_some() && !switching;
        self.set_text(
            APPLY_OUTPUTS,
            if switching {
                "Повторить"
            } else {
                "Применить"
            },
        );
        self.set_text(
            RESET_BINDING,
            if switching {
                "Отменить"
            } else {
                "Отменить правки"
            },
        );
        let target = self.connections.current().is_some();
        for (id, on) in [
            (
                SCAN,
                ready
                    && target
                    && self.connections.factory.is_some()
                    && self.connections.scannable()
                    && self.connections.scan.is_idle(),
            ),
            (TARGET, ready),
            (LIST, ready),
            (ENDPOINT, ready),
            (IDENTITY, ready),
            (SERIAL, ready),
            (SAVE_KEY, ready && self.connections.activation.is_some()),
            (SAVE_BINDING, ready && self.connections.dirty()),
            (RESET_BINDING, ready || switching),
            (
                APPLY_OUTPUTS,
                self.connections.activation.is_some()
                    && if switching {
                        self.runtime.configuration_error().is_some()
                    } else {
                        ready
                            && !self.connections.dirty()
                            && self.connections.scan.is_idle()
                            && !self.runtime.pending_configuration()
                            && self.pending_open.is_none()
                    },
            ),
            (
                REVERT_OUTPUTS,
                !switching
                    && ready
                    && self.connections.previous.is_some()
                    && !self.connections.dirty()
                    && self.connections.scan.is_idle()
                    && !self.runtime.pending_configuration(),
            ),
            (REMOVE, ready && target),
            (PRUNE, ready && n > 0),
        ] {
            unsafe {
                EnableWindow(self.h(id), i32::from(on));
            }
        }
    }
    pub(super) fn connections_command(&mut self, id: i32, notice: u16) -> Result<bool, String> {
        if !is_control(id) {
            return Ok(false);
        }
        if self.page != Page::Connections {
            return Err("Подключения открыты на другой странице.".into());
        }
        if self.connections.pending.is_some() && !matches!(id, APPLY_OUTPUTS | RESET_BINDING) {
            return Err("Дождитесь применения подключений или отмените его.".into());
        }
        match id {
            SAVE_KEY if notice == BN_CLICKED as u16 => {
                if !self.connections.home_assistant() {
                    return Err("Ключ относится только к Home Assistant.".into());
                }
                let endpoint =
                    crate::devices::home_assistant::canonical_endpoint(self.text(ENDPOINT).trim())?;
                let key = crate::devices::home_assistant::credential_id(&endpoint)?;
                let token = self.text(SERIAL);
                if token.len() > crate::credentials_windows::MAX_TOKEN_BYTES {
                    return Err("Ключ превышает предел хранилища Windows (2560 байт).".into());
                }
                crate::transport::home_assistant::Client::new(
                    crate::transport::home_assistant::Endpoint::parse(&endpoint)
                        .map_err(|e| e.to_string())?,
                    token.clone(),
                )
                .map_err(|e| e.to_string())?;
                match self.connections.activation.as_ref().map(|a| a.native) {
                    Some(true) => {
                        crate::credentials_windows::save(&key, &token).map_err(|e| e.to_string())?
                    }
                    Some(false) => {
                        self.connections.stand_keys.insert(key, token);
                    }
                    None => return Err("Хранилище отключено в снимке.".into()),
                }
                self.connections.syncing = true;
                self.set_text(SERIAL, "");
                self.connections.syncing = false;
                self.status = "Ключ сохранён отдельно от профиля. Для работающего выхода примените подключения заново.".into();
            }
            APPLY_OUTPUTS if notice == BN_CLICKED as u16 => {
                if self.connections.pending.is_some() {
                    self.runtime.retry_configuration()?;
                } else {
                    self.apply_connections(false)?;
                }
            }
            REVERT_OUTPUTS if notice == BN_CLICKED as u16 => {
                self.apply_connections(true)?;
            }
            ENDPOINT | IDENTITY | SERIAL if notice == EN_CHANGE as u16 => {
                if !self.connections.syncing {
                    self.connections.edited = true;
                    self.connections.removed = false;
                }
            }
            TARGET if notice == CBN_SELCHANGE as u16 => {
                if self.connections.dirty() || !self.connections.scan.is_idle() {
                    unsafe {
                        SendMessageW(self.h(TARGET), CB_SETCURSEL, self.connections.selected, 0);
                    }
                    return Err("Сначала сохраните/отмените правки и дождитесь поиска.".into());
                }
                let selected = unsafe { SendMessageW(self.h(TARGET), CB_GETCURSEL, 0, 0) };
                if selected >= 0 && (selected as usize) < self.connections.targets.len() {
                    self.connections.selected = selected as usize;
                    self.load_connection_fields();
                }
            }
            SCAN if notice == BN_CLICKED as u16 => {
                if !self.connections.scannable() {
                    return Err("Для этого семейства подключение задаётся вручную.".into());
                }
                let target = self
                    .connections
                    .current()
                    .cloned()
                    .ok_or("Не выбрано устройство")?;
                let factory = self
                    .connections
                    .factory
                    .clone()
                    .ok_or("Аппаратный поиск в этом снимке отключён.")?;
                if self.connections.home_assistant() && !self.text(ENDPOINT).trim().is_empty() {
                    if !self.text(SERIAL).is_empty() {
                        return Err("Сначала сохраните новый ключ.".into());
                    }
                    let key =
                        crate::devices::home_assistant::credential_id(self.text(ENDPOINT).trim())?;
                    self.connections.key(&key)?;
                }
                let network = matches!(&target,BindingTarget::Device(d) if matches!(d.driver,crate::devices::Driver::OpenRgb|crate::devices::Driver::HomeAssistant|crate::devices::Driver::WledDdp|crate::devices::Driver::SurplifeE1Pixels));
                let factory = if network
                    && self
                        .connections
                        .activation
                        .as_ref()
                        .is_some_and(|a| a.native)
                {
                    let endpoint = self.text(ENDPOINT);
                    if matches!(&target, BindingTarget::Device(d) if d.driver == crate::devices::Driver::SurplifeE1Pixels)
                    {
                        let binding = Draft {
                            endpoint: endpoint.clone(),
                            ..Default::default()
                        }
                        .binding(match &target {
                            BindingTarget::Device(d) => d,
                            _ => unreachable!(),
                        })?;
                        let crate::devices::Binding::Ble { address } = binding else {
                            unreachable!()
                        };
                        self.connections.scan_endpoint = Some(endpoint);
                        discovery::surplife_factory(address)
                    } else if endpoint.trim().is_empty()
                        && (self.connections.home_assistant() || self.connections.wled())
                    {
                        self.connections.scan_endpoint = Some(endpoint);
                        discovery::network_factory()
                    } else if self.connections.home_assistant() {
                        if !self.text(SERIAL).is_empty() {
                            return Err("Сначала сохраните новый ключ.".into());
                        }
                        let key = crate::devices::home_assistant::credential_id(endpoint.trim())?;
                        let token = self.connections.key(&key)?;
                        self.connections.scan_endpoint = Some(endpoint.clone());
                        discovery::home_assistant_factory(endpoint, token)?
                    } else if self.connections.wled() {
                        let endpoint: std::net::SocketAddr = endpoint
                            .trim()
                            .parse()
                            .map_err(|_| "Введите IP:порт DDP, например 192.168.1.20:4048")?;
                        let endpoint = crate::devices::canonical_endpoint(endpoint);
                        let http = self.text(SERIAL);
                        let http = if http.trim().is_empty() {
                            format!("http://{}", std::net::SocketAddr::new(endpoint.ip(), 80))
                        } else {
                            http.trim().to_owned()
                        };
                        self.connections.scan_endpoint = Some(self.text(ENDPOINT));
                        discovery::wled_factory(endpoint, http)
                    } else {
                        let endpoint = if endpoint.trim().is_empty() {
                            "127.0.0.1:6742"
                        } else {
                            endpoint.trim()
                        };
                        let endpoint = crate::devices::canonical_endpoint(
                            endpoint
                                .parse()
                                .map_err(|_| "Введите IP:порт SDK-сервера OpenRGB")?,
                        );
                        self.connections.scan_endpoint = Some(self.text(ENDPOINT));
                        discovery::openrgb_factory(endpoint)
                    }
                } else {
                    self.connections.scan_endpoint = None;
                    factory
                };
                self.connections.scan.start(target, factory)?;
                if network {
                    self.connections.edited = false;
                }
                self.connections.inventory = None;
                unsafe {
                    SendMessageW(self.h(LIST), LB_RESETCONTENT, 0, 0);
                }
            }
            LIST if notice == LBN_SELCHANGE as u16 => {
                if self.connections.edited {
                    unsafe {
                        SendMessageW(self.h(LIST), LB_SETCURSEL, usize::MAX, 0);
                    }
                    return Err("Сначала сохраните или отмените введённые значения.".into());
                }
                let selected = unsafe { SendMessageW(self.h(LIST), LB_GETCURSEL, 0, 0) };
                let draft = self
                    .connections
                    .inventory
                    .as_ref()
                    .and_then(|v| {
                        usize::try_from(selected)
                            .ok()
                            .and_then(|i| v.candidates.get(i))
                    })
                    .cloned()
                    .ok_or("Результат поиска не выбран")?;
                if let Some(controller) = &draft.controller
                    && let Some(BindingTarget::Device(device)) = self.connections.current().cloned()
                {
                    self.connections
                        .controllers
                        .insert(device.id, controller.clone());
                }
                if let Some(info) = &draft.wled
                    && let Some(BindingTarget::Device(device)) = self.connections.current().cloned()
                {
                    self.connections
                        .wled_controllers
                        .insert(device.id, info.clone());
                }
                if let Some(BindingTarget::Device(device)) = self.connections.current().cloned()
                    && device.driver == crate::devices::Driver::SurplifeE1Pixels
                {
                    let address = u64::from_str_radix(&draft.draft.endpoint, 16)
                        .map_err(|_| "Invalid Surplife address")?;
                    let points = draft
                        .draft
                        .serial
                        .parse()
                        .map_err(|_| "Invalid Surplife point count")?;
                    self.connections
                        .surplife_points
                        .insert(device.id, (address, points));
                }
                let draft = draft.draft;
                self.connections.syncing = true;
                self.set_text(ENDPOINT, &draft.endpoint);
                self.set_text(IDENTITY, &draft.identity);
                self.set_text(
                    SERIAL,
                    if self.connections.home_assistant() {
                        ""
                    } else {
                        &draft.serial
                    },
                );
                self.connections.syncing = false;
                self.connections.edited = true;
                self.connections.removed = false;
            }
            SAVE_BINDING if notice == BN_CLICKED as u16 => {
                if self.connections.home_assistant() && !self.text(SERIAL).is_empty() {
                    return Err("Сначала сохраните новый ключ отдельно.".into());
                }
                let serial = if self.connections.removed {
                    String::new()
                } else if self.connections.home_assistant() {
                    crate::devices::home_assistant::credential_id(self.text(ENDPOINT).trim())?
                } else {
                    self.text(SERIAL)
                };
                let draft = Draft {
                    endpoint: self.text(ENDPOINT),
                    identity: self.text(IDENTITY),
                    serial,
                };
                let target = self.connections.current().cloned();
                let workspace = self
                    .connections
                    .workspace
                    .as_mut()
                    .ok_or("Файл привязок не открыт")?;
                if !self.connections.removed
                    && [&draft.endpoint, &draft.identity, &draft.serial]
                        .iter()
                        .any(|s| !s.is_empty())
                {
                    match target {
                        Some(BindingTarget::Device(d)) => {
                            workspace.assign(self.runtime.document(), &d, &draft)?
                        }
                        Some(BindingTarget::Monitor(key)) => workspace.assign_monitor(
                            self.runtime.document(),
                            &key,
                            draft.endpoint.trim(),
                        )?,
                        None => return Err("Не выбран источник привязки".into()),
                    }
                }
                workspace.save(self.runtime.document())?;
                self.connections.edited = false;
                self.connections.removed = false;
                self.load_connection_fields();
                self.status="Привязки сохранены локально. Текущие подключения не переключались; новый файл применяется при аппаратном запуске.".into();
            }
            RESET_BINDING if notice == BN_CLICKED as u16 => {
                if self.connections.pending.is_some() {
                    self.runtime.cancel_configuration()?;
                    self.status = "Отменяю применение подключений…".into();
                    self.refresh_connections();
                    return Ok(true);
                }
                self.connections.scan.cancel();
                if !self.connections.scan.is_idle() {
                    return Err("Поиск отменяется; нажмите ещё раз после завершения.".into());
                }
                self.connections.edited = false;
                if let Some(w) = &mut self.connections.workspace {
                    w.dirty = false;
                }
                self.open_connections()?;
            }
            REMOVE if notice == BN_CLICKED as u16 => {
                let target = self
                    .connections
                    .current()
                    .cloned()
                    .ok_or("Устройство не выбрано")?;
                let w = self
                    .connections
                    .workspace
                    .as_mut()
                    .ok_or("Файл не открыт")?;
                match target {
                    BindingTarget::Device(d) => w.remove(&d.id),
                    BindingTarget::Monitor(key) => w.remove_monitor(&key),
                }
                self.connections.removed = true;
                self.connections.edited = true;
                self.status =
                    "Удаление привязки в черновике. Нажмите «Сохранить», чтобы записать файл."
                        .into();
            }
            PRUNE if notice == BN_CLICKED as u16 => {
                if let Some(w) = &mut self.connections.workspace {
                    w.prune_orphans(self.runtime.document());
                }
                self.status =
                    "Отсутствующие привязки убраны из черновика. Файл изменится после сохранения."
                        .into();
            }
            _ => {}
        }
        self.refresh_connections();
        Ok(true)
    }
    fn apply_connections(&mut self, revert: bool) -> Result<(), String> {
        if self.connections.dirty()
            || !self.connections.scan.is_idle()
            || self.runtime.pending_configuration()
            || self.pending_open.is_some()
        {
            return Err("Сохраните или отмените правки и дождитесь текущих операций.".into());
        }
        if revert
            && self
                .connections
                .previous
                .as_ref()
                .is_some_and(|old| self.runtime.document() != &old.applied_document)
        {
            self.connections.previous = None;
            self.refresh_connections();
            return Err("После применения профиль изменён. Возврат отменён, чтобы сохранить новые настройки; выберите и примените подключения заново.".into());
        }
        let activation = self
            .connections
            .activation
            .as_ref()
            .ok_or("Применение к устройствам в этом снимке отключено")?;
        let (document, plan, local) = if revert {
            let old = self
                .connections
                .previous
                .as_ref()
                .ok_or("Нет предыдущего подключения")?;
            (old.document.clone(), old.plan.clone(), old.local.clone())
        } else {
            let mut document = self.runtime.document().clone();
            let local =
                crate::app::lighting_outputs::Local::read_draft(&self.connections.path, &document)?;
            for (id, binding) in &local.devices {
                if let crate::devices::Binding::OpenRgb {
                    identity, topology, ..
                } = binding
                    && let Some(controller) = self.connections.controllers.get(id)
                    && &crate::devices::openrgb::identity(controller) == identity
                    && &crate::devices::openrgb::topology(controller) == topology
                {
                    document = crate::app::device_setup::adopt_openrgb(&document, id, controller)?;
                }
                if let crate::devices::Binding::WledDdp {
                    controller: Some(controller),
                    ..
                } = binding
                    && let Some(info) = self.connections.wled_controllers.get(id)
                    && info.stable_id() == controller.identity
                {
                    document = crate::app::device_setup::adopt_wled(&document, id, info)?;
                }
                if let crate::devices::Binding::Ble { address } = binding
                    && let Some((known, points)) = self.connections.surplife_points.get(id)
                    && known == address
                {
                    document = crate::app::device_setup::adopt_surplife(&document, id, *points)?;
                }
            }
            local.validate(&document)?;
            let plan = (activation.factory)(&local, &document, &self.asset_root)?;
            (document, plan, activation.native.then_some(local))
        };
        let previous = Previous {
            applied_document: document.clone(),
            document: self.runtime.document().clone(),
            plan: self.runtime.output_plan(),
            local: self.native_lighting.clone(),
        };
        let revision = self.runtime.request_outputs_for(document, plan)?;
        self.connections.pending = Some(Pending {
            reverting: revert,
            revision,
            local,
            previous,
        });
        self.connections.watch_outputs = false;
        self.status = "Смена подключений: ожидаю завершения прежних потоков подсветки.".into();
        Ok(())
    }
    pub(super) fn connection_switching(&self) -> bool {
        self.connections.pending.is_some()
    }
    pub(super) fn poll_connection_switch(&mut self) -> bool {
        if self.shutdown.active() {
            return false;
        }
        let previous_status = self.status.clone();
        if self.connections.pending.is_some() {
            if self.runtime.pending_configuration() {
                if let Some(error) = self.runtime.configuration_error() {
                    self.status = format!(
                        "Подключения не применены: {error}. Повторите или отмените применение."
                    );
                }
                self.refresh_connections();
                return previous_status != self.status;
            }
            let pending = self.connections.pending.take().unwrap();
            if self.runtime.output_revision() == pending.revision {
                if self.runtime.document() != &pending.previous.document {
                    self.dirty = true;
                    self.context_revision = self.context_revision.wrapping_add(1);
                    self.reset_editor(0);
                }
                self.native_lighting = pending.local;
                self.connections.previous = if pending.reverting {
                    None
                } else {
                    Some(pending.previous)
                };
                self.connections.watch_outputs = true;
                self.status =
                    "Подключения выбраны; ожидаю открытия устройств. Файл привязок не изменён."
                        .into();
                if self.native_lighting.is_some()
                    && let Err(error) = self.runtime.with(|runtime, _| {
                        runtime.enable_registered_output_recovery();
                        Ok(())
                    })
                {
                    self.status =
                        format!("Подключения выбраны, восстановление не включено: {error}");
                }
                unsafe {
                    SetWindowTextW(
                        GetParent(self.h(PC_ON)),
                        wide(if self.native_lighting.is_some() {
                            "Prismestra · Подсветка устройств"
                        } else {
                            "Prismestra · Предпросмотр"
                        })
                        .as_ptr(),
                    );
                }
                self.set_text(
                    PC_ON,
                    if self.native_lighting.is_some() {
                        "Подсветка ПК"
                    } else {
                        "ПК в предпросмотре"
                    },
                );
                self.set_text(
                    DOOR_ON,
                    if self.native_lighting.is_some() {
                        "Лента у двери"
                    } else {
                        "Лента в предпросмотре"
                    },
                );
                self.set_text(
                    SCHED_ON,
                    if self.native_lighting.is_some() {
                        "Включить расписание подсветки"
                    } else {
                        "Включить расписание предпросмотра"
                    },
                );
                self.set_text(
                    APPLY_SCHED,
                    if self.native_lighting.is_some() {
                        "Применить к устройствам"
                    } else {
                        "Применить к предпросмотру"
                    },
                );
            } else {
                self.connections.watch_outputs = true;
                self.status="Применение отменено. Восстанавливаются прежние подключения; файл привязок не изменён.".into();
            }
            self.refresh_connections();
            self.show_page();
        }
        if self.connections.watch_outputs {
            match self.runtime.outputs_ready() {
                Ok(false) => {}
                Ok(true) => {
                    self.connections.watch_outputs = false;
                    self.status="Подключения открыты. Проверяйте передачу и вид устройств; «Вернуть» восстановит предыдущий набор без изменения файла.".into();
                }
                Err(error) => {
                    self.connections.watch_outputs = false;
                    self.status = format!(
                        "Сбой подключения: {error}. Можно исправить привязки и применить снова либо нажать «Вернуть». Файл не перезаписан."
                    );
                }
            }
        }
        previous_status != self.status
    }
    pub(super) fn poll_connections(&mut self) -> bool {
        let Some((target, result)) = self.connections.scan.poll() else {
            return false;
        };
        if self
            .connections
            .scan_endpoint
            .take()
            .is_some_and(|endpoint| endpoint != self.text(ENDPOINT))
            || self.connections.current() != Some(&target)
        {
            return false;
        }
        match result {
            Ok(inventory) => {
                unsafe {
                    SendMessageW(self.h(LIST), LB_RESETCONTENT, 0, 0);
                }
                for c in &inventory.candidates {
                    unsafe {
                        SendMessageW(
                            self.h(LIST),
                            LB_ADDSTRING,
                            0,
                            wide(&c.label).as_ptr() as isize,
                        );
                    }
                }
                self.connections.inventory = Some(inventory);
            }
            Err(e) => {
                self.status = e;
                self.connections.inventory = None;
            }
        }
        self.refresh_connections();
        true
    }
}
pub(super) fn stand_factory() -> discovery::Factory {
    std::sync::Arc::new(|target, _| {
        let mut inventory = discovery::Inventory::empty(target.clone());
        if matches!(&target,BindingTarget::Device(d) if d.driver==crate::devices::Driver::OpenRgb) {
            let controller = stand::openrgb_controller();
            inventory.candidates.push(discovery::Candidate {
                wled: None,
                controller: Some(controller.clone()),
                label: "Программный OpenRGB · 3 LED · не оборудование".into(),
                draft: Draft {
                    endpoint: "127.0.0.1:6742".into(),
                    identity: crate::devices::openrgb::identity(&controller),
                    serial: crate::devices::openrgb::topology(&controller),
                },
            });
            return Ok(inventory);
        }
        let draft = match &target {
            BindingTarget::Device(d) if d.driver == crate::devices::Driver::HomeAssistant => {
                Some(Draft {
                    endpoint: "http://127.0.0.1:8123".into(),
                    identity: "light.fixture".into(),
                    serial: crate::devices::home_assistant::credential_id("http://127.0.0.1:8123")?,
                })
            }
            BindingTarget::Device(d)
                if matches!(d.driver, crate::devices::Driver::Fusion2 { .. }) =>
            {
                Some(Draft {
                    endpoint:
                        r"\\?\hid#vid_048d&pid_5702#stand#{4d1e55b2-f16f-11cf-88cb-001111000030}"
                            .into(),
                    ..Default::default()
                })
            }
            BindingTarget::Monitor(_) => Some(Draft {
                endpoint: r"\\.\DISPLAY42".into(),
                ..Default::default()
            }),
            _ => None,
        };
        if let Some(draft) = draft {
            inventory.candidates.push(discovery::Candidate {
                wled: None,
                controller: None,
                label: "Программный пример · не обнаруженное оборудование".into(),
                draft,
            });
        } else {
            inventory
                .issues
                .push("Для этого семейства в данном стенде нет примера.".into());
        }
        Ok(inventory)
    })
}

#[cfg(test)]
mod activation_tests {
    use super::*;
    #[test]
    fn normal_ui_can_explicitly_apply_native_bindings_but_diagnostics_cannot() {
        let normal = Editor::new(PathBuf::from("unused.json"), None).activation(false, false);
        assert!(normal.activation.as_ref().is_some_and(|a| a.native));
        let snapshot = Editor::new(PathBuf::from("unused.json"), None).activation(true, false);
        assert!(snapshot.activation.is_none());
        let stand = Editor::new(PathBuf::from("unused.json"), None).activation(true, true);
        assert!(stand.activation.as_ref().is_some_and(|a| !a.native));
    }
}
