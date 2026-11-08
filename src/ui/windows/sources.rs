//! Local preferences and observation UI. This module never creates a native resolver.
use super::*;
use crate::app::{
    sensor_bindings::{Catalog, Scope},
    sensor_form::{Draft, Family},
    sensor_sessions::{Registry, Subscription},
};
use std::sync::{Arc, Mutex};
mod bindings;
mod discovery;
pub(super) const MODULE_SELECT: i32 = bindings::MODULE;
pub(super) fn module_dialog(hwnd: HWND, state: &State) -> Result<(), String> {
    bindings::module_dialog(hwnd, state)
}

const MODE: i32 = 1100;
const LIST: i32 = 1101;
const ID: i32 = 1102;
const FAMILY: i32 = 1103;
const SAVE_SOURCE: i32 = 1104;
const NEW: i32 = 1105;
const REMOVE: i32 = 1106;
const RESET: i32 = 1107;
const SCOPE: i32 = 1108;
const ROUTE: i32 = 1109;
const BIND: i32 = 1110;
const RELEASE: i32 = 1111;
const INFO: i32 = 1112;
const ID_LABEL: i32 = 1113;
const FAMILY_LABEL: i32 = 1114;
const SCOPE_LABEL: i32 = 1115;
const ROUTE_LABEL: i32 = 1116;
const FIELD: i32 = 1120;
const LABEL: i32 = 1130;

pub(super) struct Editor {
    catalog: Catalog,
    load_error: Option<String>,
    path: PathBuf,
    pub(super) registry: Arc<Mutex<Registry>>,
    lighting: Option<Arc<Subscription>>,
    draft: Draft,
    baseline: Draft,
    selected: Option<usize>,
    inputs: Vec<String>,
    pub(super) last_status: Instant,
    pub(super) scan: crate::app::sensor_discovery::Worker,
    pub(super) scan_factory: Option<crate::app::sensor_discovery::Factory>,
    inventory: Option<crate::app::sensor_discovery::Inventory>,
    pub(super) scan_message: String,
    resolver: Option<Box<dyn crate::app::sensor_sessions::Resolver>>,
    lighting_active: Option<bindings::Active>,
}
impl Editor {
    pub(super) fn load(path: PathBuf) -> Self {
        let loaded = if path.exists() {
            Catalog::load(&path)
        } else {
            Ok(Catalog::default())
        };
        let (catalog, load_error) = match loaded {
            Ok(c) => (c, None),
            Err(e) => (Catalog::default(), Some(e)),
        };
        let selected = (!catalog.sources.is_empty()).then_some(0);
        let draft = catalog
            .sources
            .first()
            .map(Draft::from_source)
            .unwrap_or_else(|| Draft::new(Family::Nvidia));
        Self {
            catalog,
            load_error,
            path,
            registry: Arc::new(Mutex::new(Registry::default())),
            lighting: None,
            baseline: draft.clone(),
            draft,
            selected,
            inputs: vec![],
            last_status: Instant::now(),
            scan: Default::default(),
            scan_factory: None,
            inventory: None,
            scan_message: "Поиск оборудования недоступен в режиме предпросмотра. Можно добавить датчик вручную.".into(),
            resolver:None,lighting_active:None,
        }
    }
    fn save(&mut self, next: Catalog) -> Result<(), String> {
        if let Some(error) = &self.load_error {
            return Err(format!(
                "Каталог не загружен: {error}. Исходный файл сохранён; исправьте его и откройте приложение заново."
            ));
        }
        next.save(&self.path)?;
        self.catalog = next;
        Ok(())
    }
    pub(super) fn release_lighting(&mut self) {
        if let Some(subscription) = self.lighting.take() {
            subscription.revoke();
        }
        self.lighting_active = None;
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .poll();
    }
    pub(super) fn poll(&mut self, runtime: &mut Runtime) -> Result<(), String> {
        if self.lighting.as_ref().is_some_and(|s| !s.active()) {
            self.lighting = None;
            self.lighting_active = None;
        }
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .poll();
        if !runtime.temperature_input_ready() {
            return Ok(());
        }
        if let Some(token) = &self.lighting {
            runtime.bind_registry_input(
                self.registry.clone(),
                token.clone(),
                crate::clock::monotonic_us(),
            )?;
        } else {
            runtime.release_registry_input(crate::clock::monotonic_us())?;
        }
        Ok(())
    }
    pub(super) fn drained(&mut self) -> bool {
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drained()
    }
}
pub(super) fn is_control(id: i32) -> bool {
    (MODE..=bindings::MODULE).contains(&id)
}
impl App {
    pub(super) fn build_sources(&mut self, parent: HWND) -> Result<(), String> {
        for (id, class, text, style) in [
            (MODE, "COMBOBOX", "", CBS_DROPDOWNLIST as u32),
            (
                LIST,
                "LISTBOX",
                "",
                LBS_NOTIFY as u32 | WS_VSCROLL | WS_BORDER,
            ),
            (ID_LABEL, "STATIC", "Имя датчика", 0),
            (ID, "EDIT", "", ES_AUTOHSCROLL as u32 | WS_BORDER),
            (FAMILY_LABEL, "STATIC", "Семейство", 0),
            (FAMILY, "COMBOBOX", "", CBS_DROPDOWNLIST as u32),
            (SCOPE_LABEL, "STATIC", "Профиль", 0),
            (SCOPE, "COMBOBOX", "", CBS_DROPDOWNLIST as u32),
            (ROUTE_LABEL, "STATIC", "Физический датчик", 0),
            (ROUTE, "COMBOBOX", "", CBS_DROPDOWNLIST as u32 | WS_VSCROLL),
            (
                SAVE_SOURCE,
                "BUTTON",
                "Сохранить датчик",
                BS_PUSHBUTTON as u32,
            ),
            (NEW, "BUTTON", "Новый", BS_PUSHBUTTON as u32),
            (REMOVE, "BUTTON", "Удалить", BS_PUSHBUTTON as u32),
            (RESET, "BUTTON", "Сбросить правки", BS_PUSHBUTTON as u32),
            (BIND, "BUTTON", "Привязать", BS_PUSHBUTTON as u32),
            (RELEASE, "BUTTON", "Снять привязку", BS_PUSHBUTTON as u32),
            (INFO, "STATIC", "", 0),
        ] {
            self.control(parent, id, class, text, style)?;
        }
        for i in 0..6 {
            self.control(parent, LABEL + i, "STATIC", "", 0)?;
            self.control(
                parent,
                FIELD + i,
                "EDIT",
                "",
                ES_AUTOHSCROLL as u32 | WS_BORDER,
            )?;
            unsafe {
                SendMessageW(self.h(FIELD + i), EM_SETLIMITTEXT, 32760, 0);
            }
        }
        unsafe {
            SendMessageW(self.h(ID), EM_SETLIMITTEXT, 2048, 0);
        }
        for text in ["Датчики", "Привязки", "Обнаружение"] {
            self.add(MODE, text);
        }
        for family in Family::ALL {
            self.add(FAMILY, family.label());
        }
        self.add(SCOPE, "Подсветка и LCD");
        for id in [MODE, FAMILY, SCOPE] {
            unsafe {
                SendMessageW(self.h(id), CB_SETCURSEL, 0, 0);
            }
        }
        for id in [
            INFO,
            ID_LABEL,
            FAMILY_LABEL,
            SCOPE_LABEL,
            ROUTE_LABEL,
            LABEL,
            LABEL + 1,
            LABEL + 2,
            LABEL + 3,
            LABEL + 4,
            LABEL + 5,
        ] {
            unsafe {
                SetWindowLongPtrW(
                    self.h(id),
                    GWL_STYLE,
                    GetWindowLongPtrW(self.h(id), GWL_STYLE) & !(WS_TABSTOP as isize),
                );
            }
        }
        self.build_source_discovery(parent)?;
        self.build_source_bindings(parent)?;
        self.refresh_sources();
        Ok(())
    }
    fn source_routes_mode(&self) -> bool {
        self.combo_index(MODE) == Some(1)
    }
    fn source_discovery_mode(&self) -> bool {
        self.combo_index(MODE) == Some(2)
    }
    fn source_scope_inputs(&self) -> Result<(Scope, Vec<String>), String> {
        let name = self
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or("unsaved-lighting".into());
        let scope = Scope::lighting(&name, self.runtime.document())?;
        let inputs = self
            .runtime
            .document()
            .temperatures
            .sources
            .iter()
            .filter_map(|s| match &s.value {
                crate::app::sensors::Value::Sensor { input } => Some(input.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let inputs = inputs
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok((scope, inputs))
    }
    fn read_source_draft(&self) -> Draft {
        Draft {
            id: self.text(ID),
            family: Family::ALL[self.combo_index(FAMILY).unwrap_or(0).min(3)],
            values: std::array::from_fn(|i| self.text(FIELD + i as i32)),
        }
    }
    pub(super) fn guard_sources_exit(&self) -> Result<(), String> {
        if self.page == Page::Sources && !self.source_routes_mode() && !self.source_discovery_mode()
        {
            let current = self.read_source_draft();
            let saved = &self.sources.baseline;
            if current.id != saved.id
                || current.family != saved.family
                || current.values != saved.values
            {
                return Err("Сначала сохраните датчик или нажмите «Сбросить правки».".into());
            }
        }
        Ok(())
    }
    fn fill_source_draft(&self) {
        let draft = &self.sources.draft;
        self.set_text(ID, &draft.id);
        unsafe {
            SendMessageW(
                self.h(FAMILY),
                CB_SETCURSEL,
                Family::ALL
                    .iter()
                    .position(|f| *f == draft.family)
                    .unwrap_or(0),
                0,
            );
        }
        for (i, label) in draft.labels().iter().enumerate() {
            self.set_text(LABEL + i as i32, label);
            self.set_text(FIELD + i as i32, &draft.values[i]);
        }
        self.sources_layout(
            (i64::from(self.width) * 96 / i64::from(self.dpi)) as i32,
            (i64::from(self.height) * 96 / i64::from(self.dpi)) as i32,
        );
    }
    pub(super) fn refresh_sources(&mut self) {
        if !self.widgets.contains_key(&MODE) {
            return;
        }
        unsafe {
            SendMessageW(self.h(LIST), LB_RESETCONTENT, 0, 0);
            SendMessageW(self.h(ROUTE), CB_RESETCONTENT, 0, 0);
        }
        if self.source_discovery_mode() {
            self.refresh_source_discovery();
            return;
        }
        if self.source_routes_mode() {
            self.sources.inputs = self
                .source_scope_inputs()
                .map(|(_, inputs)| inputs)
                .unwrap_or_default();
            for input in &self.sources.inputs {
                let source = self
                    .source_scope_inputs()
                    .ok()
                    .and_then(|(scope, _)| {
                        self.sources
                            .catalog
                            .routes
                            .iter()
                            .find(|r| r.scope == scope && r.input == *input)
                    })
                    .map(|r| r.source.as_str())
                    .unwrap_or("не привязан");
                unsafe {
                    SendMessageW(
                        self.h(LIST),
                        LB_ADDSTRING,
                        0,
                        wide(&format!("{input} · {source}")).as_ptr() as isize,
                    );
                }
            }
            for source in &self.sources.catalog.sources {
                self.add(ROUTE, &source.id);
            }
            unsafe {
                SendMessageW(self.h(LIST), LB_SETCURSEL, 0, 0);
                SendMessageW(self.h(ROUTE), CB_SETCURSEL, 0, 0);
            }
        } else {
            for source in &self.sources.catalog.sources {
                unsafe {
                    SendMessageW(
                        self.h(LIST),
                        LB_ADDSTRING,
                        0,
                        wide(&format!("{} · v{}", source.id, source.revision)).as_ptr() as isize,
                    );
                }
            }
            if let Some(index) = self.sources.selected {
                unsafe {
                    SendMessageW(self.h(LIST), LB_SETCURSEL, index, 0);
                }
            }
            self.fill_source_draft();
        }
        self.refresh_source_status();
    }
    pub(super) fn refresh_source_status(&self) {
        if self.page != Page::Sources || !self.widgets.contains_key(&INFO) {
            return;
        }
        if self.source_discovery_mode() {
            self.refresh_source_discovery_details();
            return;
        }
        self.refresh_source_binding_state();
        let text = if let Some(error) = &self.sources.load_error {
            format!("Каталог недоступен: {error}. Файл не перезаписывается.")
        } else if self.source_routes_mode() {
            match self.source_scope_inputs() {
                Err(error)=>error,
                Ok((scope,inputs))=>match self.sources.catalog.prepare(&scope,&inputs) {
                    Ok(_)=>format!("Все {} входов привязаны к текущей версии профиля. Сохранение привязок не запускает оборудование.",inputs.len()),
                    Err(_)=>"Привяжите каждый вход этого профиля. После изменения профиля старые привязки автоматически не переносятся.".into(),
                }
            }
        } else if let Some(source) = self
            .sources
            .selected
            .and_then(|i| self.sources.catalog.sources.get(i))
        {
            let registry = self
                .sources
                .registry
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match registry.status().into_iter().find(|s|s.target==source.target) {
                Some(status)=> {
                    let value=status.worker.reading.map(|r|format!("{:.1} °C · возраст {} мс",r.celsius,crate::clock::monotonic_ms().saturating_sub(r.observed_ms))).unwrap_or("нет показания".into());
                    format!("{:?} · {value} · потребителей {}{}",status.worker.phase,status.consumers,status.worker.error.as_ref().map(|e|format!(" · {e}")).unwrap_or_default())
                }
                None=>"Чтение не запущено. Физическая совместимость этого датчика пока не подтверждена.".into(),
            }
        } else {
            "Укажите точную идентичность датчика. Автопоиск и загрузка драйверов здесь не выполняются.".into()
        };
        let width_dip = (i64::from(self.width) * 96 / i64::from(self.dpi)) as i32;
        let text = self.wrap_control_text(
            INFO,
            &text,
            px((width_dip - CONTENT_LEFT - 250 - 32).max(1), self.dpi),
        );
        if self.text(INFO) != text {
            self.set_text(INFO, &text);
        }
    }
    pub(super) fn source_visible(&self, id: i32) -> bool {
        if self.page != Page::Sources {
            return false;
        }
        if [MODE, LIST, INFO].contains(&id) {
            return true;
        }
        if self.source_discovery_mode() {
            return discovery::is_control(id);
        }
        if self.source_routes_mode() {
            return [
                SCOPE,
                SCOPE_LABEL,
                ROUTE,
                ROUTE_LABEL,
                BIND,
                RELEASE,
                bindings::START,
                bindings::STOP,
                bindings::STATE,
            ]
            .contains(&id);
        }
        if id == bindings::MODULE {
            return matches!(self.sources.draft.family, Family::Amd | Family::Ddr5);
        }
        if (LABEL..LABEL + 6).contains(&id) {
            return !self.sources.draft.labels()[(id - LABEL) as usize].is_empty();
        }
        if (FIELD..FIELD + 6).contains(&id) {
            return !self.sources.draft.labels()[(id - FIELD) as usize].is_empty();
        }
        [
            ID,
            FAMILY,
            ID_LABEL,
            FAMILY_LABEL,
            SAVE_SOURCE,
            NEW,
            REMOVE,
            RESET,
        ]
        .contains(&id)
    }
    pub(super) fn sources_layout(&self, w: i32, h: i32) {
        let left = CONTENT_LEFT;
        let right = left + 250;
        let width = w - right - 24;
        self.pos(MODE, left, 182, 226, 180);
        self.pos(LIST, left, 222, 226, (h - 338).max(160));
        self.pos(NEW, left, h - 104, 106, 30);
        self.pos(REMOVE, left + 114, h - 104, 112, 30);
        for (id, y) in [(ID, 202), (FAMILY, 262)] {
            self.pos(id, right, y, width, if id == FAMILY { 180 } else { 28 });
        }
        self.pos(ID_LABEL, right, 180, width, 20);
        self.pos(FAMILY_LABEL, right, 240, width, 20);
        for i in 0..6 {
            let y = 308 + i * 30;
            self.pos(LABEL + i, right, y, 200, 24);
            self.pos(FIELD + i, right + 208, y, width - 208, 26);
        }
        self.pos(SAVE_SOURCE, right, 492, 180, 32);
        self.pos(RESET, right + 190, 492, 180, 32);
        self.pos(SCOPE_LABEL, right, 180, width, 20);
        self.pos(SCOPE, right, 202, width, 180);
        self.pos(ROUTE_LABEL, right, 248, width, 20);
        self.pos(ROUTE, right, 270, width, 200);
        self.pos(BIND, right, 314, 150, 32);
        self.pos(RELEASE, right + 160, 314, 180, 32);
        self.pos(INFO, right, 536, width, (h - 584).max(40));
        self.source_bindings_layout(w, h);
        if self.source_discovery_mode() {
            self.source_discovery_layout(w, h);
        }
    }
    pub(super) fn sources_command(&mut self, id: i32, notice: u16) -> Result<bool, String> {
        if !is_control(id) {
            return Ok(false);
        }
        if self.page != Page::Sources {
            return Ok(true);
        }
        if self.source_discovery_command(id, notice)? {
            return Ok(true);
        }
        if self.source_bindings_command(id, notice)? {
            return Ok(true);
        }
        match id {
            MODE if notice == CBN_SELCHANGE as u16 => {
                // The old mode may contain an unsaved source form.
                if (self.source_routes_mode() || self.source_discovery_mode())
                    && let Err(error) = self.guard_source_form()
                {
                    unsafe {
                        SendMessageW(self.h(MODE), CB_SETCURSEL, 0, 0);
                    }
                    return Err(error);
                }
                self.refresh_sources();
                self.sources_layout(
                    (i64::from(self.width) * 96 / i64::from(self.dpi)) as i32,
                    (i64::from(self.height) * 96 / i64::from(self.dpi)) as i32,
                );
                self.show_page();
            }
            SCOPE if notice == CBN_SELCHANGE as u16 => self.refresh_sources(),
            LIST if notice == LBN_SELCHANGE as u16
                && !self.source_routes_mode()
                && !self.source_discovery_mode() =>
            {
                let index = unsafe { SendMessageW(self.h(LIST), LB_GETCURSEL, 0, 0) };
                if let Err(error) = self.guard_source_form() {
                    unsafe {
                        SendMessageW(
                            self.h(LIST),
                            LB_SETCURSEL,
                            self.sources.selected.unwrap_or(usize::MAX),
                            0,
                        );
                    }
                    return Err(error);
                }
                if let Some(source) = usize::try_from(index)
                    .ok()
                    .and_then(|i| self.sources.catalog.sources.get(i))
                {
                    self.sources.draft = Draft::from_source(source);
                    self.sources.baseline = self.sources.draft.clone();
                    self.sources.selected = Some(index as usize);
                    self.fill_source_draft();
                    self.show_page();
                }
            }
            FAMILY if notice == CBN_SELCHANGE as u16 => {
                let id = self.text(ID);
                self.sources.draft =
                    Draft::new(Family::ALL[self.combo_index(FAMILY).unwrap_or(0).min(3)]);
                self.sources.draft.id = id;
                self.fill_source_draft();
                self.show_page();
            }
            NEW if notice == BN_CLICKED as u16 => {
                self.guard_source_form()?;
                self.sources.selected = None;
                self.sources.draft = Draft::new(Family::Nvidia);
                self.sources.baseline = self.sources.draft.clone();
                self.refresh_sources();
                self.show_page();
            }
            RESET if notice == BN_CLICKED as u16 => {
                self.sources.draft = self
                    .sources
                    .selected
                    .and_then(|i| self.sources.catalog.sources.get(i))
                    .map(Draft::from_source)
                    .unwrap_or_else(|| Draft::new(Family::Nvidia));
                self.sources.baseline = self.sources.draft.clone();
                self.fill_source_draft();
                self.show_page();
            }
            SAVE_SOURCE if notice == BN_CLICKED as u16 => {
                let draft = self.read_source_draft();
                let mut catalog = self.sources.catalog.clone();
                if self.sources.selected.is_none()
                    && catalog.sources.iter().any(|s| s.id == draft.id)
                {
                    return Err("Это имя уже занято. Выберите существующий датчик для изменения или задайте новое имя.".into());
                }
                if self
                    .sources
                    .selected
                    .is_some_and(|i| catalog.sources[i].id != draft.id)
                {
                    return Err("Имя существующего датчика не меняется. Для нового имени создайте новый датчик.".into());
                }
                catalog.set_source(&draft.id, draft.target()?)?;
                self.sources.save(catalog)?;
                self.sources.selected = self
                    .sources
                    .catalog
                    .sources
                    .iter()
                    .position(|s| s.id == draft.id);
                self.sources.draft = draft;
                self.sources.baseline = self.sources.draft.clone();
                self.refresh_sources();
                self.status = "Датчик сохранён локально. Оборудование не запускалось.".into();
            }
            REMOVE if notice == BN_CLICKED as u16 => {
                self.guard_source_form()?;
                let index = self.sources.selected.ok_or("Выберите датчик.")?;
                let mut catalog = self.sources.catalog.clone();
                let id = catalog.sources[index].id.clone();
                catalog.remove_source(&id)?;
                self.sources.save(catalog)?;
                self.sources.selected = None;
                self.sources.draft = Draft::new(Family::Nvidia);
                self.sources.baseline = self.sources.draft.clone();
                self.refresh_sources();
                self.show_page();
            }
            BIND | RELEASE if notice == BN_CLICKED as u16 => {
                let (scope, current_inputs) = self.source_scope_inputs()?;
                let index =
                    usize::try_from(unsafe { SendMessageW(self.h(LIST), LB_GETCURSEL, 0, 0) })
                        .map_err(|_| "Выберите логический вход.")?;
                let input = self
                    .sources
                    .inputs
                    .get(index)
                    .ok_or("Выберите логический вход.")?
                    .clone();
                if !current_inputs.contains(&input) {
                    return Err(
                        "Профиль изменился. Заново откройте список привязок перед сохранением."
                            .into(),
                    );
                }
                let mut routes = self
                    .sources
                    .catalog
                    .routes
                    .iter()
                    .filter(|r| r.scope == scope && r.input != input)
                    .map(|r| (r.input.clone(), r.source.clone()))
                    .collect::<Vec<_>>();
                if id == BIND {
                    let source = self
                        .combo_index(ROUTE)
                        .and_then(|i| self.sources.catalog.sources.get(i))
                        .ok_or("Добавьте и выберите физический датчик.")?;
                    routes.push((input, source.id.clone()));
                }
                let mut catalog = self.sources.catalog.clone();
                catalog.set_routes(&scope, &routes)?;
                self.sources.save(catalog)?;
                self.refresh_sources();
                self.status = "Привязки сохранены. Действующие подписки не изменялись.".into();
            }
            _ => {}
        }
        Ok(true)
    }
    fn guard_source_form(&self) -> Result<(), String> {
        let current = self.read_source_draft();
        let saved = &self.sources.baseline;
        if current.id != saved.id
            || current.family != saved.family
            || current.values != saved.values
        {
            return Err("Сохраните датчик или сбросьте правки перед переходом.".into());
        }
        Ok(())
    }
}

pub(super) fn exercise(state: &State) -> Result<(), String> {
    use crate::app::{
        sensor_sessions::Resolver,
        sensor_worker::{Factory, Provider},
    };
    struct Fake;
    impl Provider for Fake {
        fn poll_interval_ms(&self) -> u64 {
            100
        }
        fn poll(&mut self, now: u64) -> Result<Option<crate::sensors::Reading>, String> {
            Ok(Some(crate::sensors::Reading {
                celsius: 42.,
                observed_ms: now,
            }))
        }
    }
    struct Resolve;
    impl Resolver for Resolve {
        fn resolve(&mut self, _: &crate::app::sensor_bindings::Target) -> Result<Factory, String> {
            Ok(Arc::new(|_| Ok(Box::new(Fake))))
        }
    }
    let mut slot = state.app.borrow_mut();
    let a = slot.as_mut().unwrap();
    let original_mode = a.combo_index(MODE).unwrap_or(0);
    a.command(NAV_SOURCES, BN_CLICKED as u16)?;
    unsafe {
        SendMessageW(a.h(MODE), CB_SETCURSEL, 0, 0);
    }
    let original = a.sources.catalog.clone();
    a.sources.selected = None;
    a.sources.draft = Draft::new(Family::Nvidia);
    a.sources.baseline = a.sources.draft.clone();
    a.refresh_sources();
    a.set_text(ID, "selftest/gpu");
    for (i, value) in ["123410de", "7", "1", "9"].iter().enumerate() {
        a.set_text(FIELD + i as i32, value);
    }
    if a.command(NAV_SCENE, BN_CLICKED as u16).is_ok() {
        return Err("Unsaved source form allowed navigation".into());
    }
    a.sources_command(SAVE_SOURCE, BN_CLICKED as u16)?;
    if Catalog::load(&a.sources.path)? != a.sources.catalog {
        return Err("Source form did not persist".into());
    }
    let selected = a
        .sources
        .selected
        .ok_or("Source selection missing after save")?;
    let source = a.sources.catalog.sources[selected].clone();
    let stored = std::fs::read(&a.sources.path).map_err(|e| e.to_string())?;
    a.sources_command(NEW, BN_CLICKED as u16)?;
    a.set_text(ID, &source.id);
    for (i, value) in ["123410de", "7", "1", "a"].iter().enumerate() {
        a.set_text(FIELD + i as i32, value);
    }
    if a.sources_command(SAVE_SOURCE, BN_CLICKED as u16).is_ok()
        || a.sources.catalog.sources[selected] != source
        || std::fs::read(&a.sources.path).map_err(|e| e.to_string())? != stored
    {
        return Err("New source overwrote an existing descriptor or preference file".into());
    }
    a.sources_command(RESET, BN_CLICKED as u16)?;
    unsafe {
        SendMessageW(a.h(LIST), LB_SETCURSEL, selected, 0);
    }
    a.sources_command(LIST, LBN_SELCHANGE as u16)?;
    unsafe {
        SendMessageW(a.h(FAMILY), CB_SETCURSEL, 1, 0);
    }
    a.sources_command(FAMILY, CBN_SELCHANGE as u16)?;
    if a.guard_sources_exit().is_ok() {
        return Err("Family change lost its dirty state".into());
    }
    a.sources_command(RESET, BN_CLICKED as u16)?;
    let mut document = a.runtime.document().clone();
    document
        .temperatures
        .sources
        .push(crate::app::sensors::Source {
            key: "ui-source-test".into(),
            value: crate::app::sensors::Value::Sensor {
                input: "ui-test-input".into(),
            },
        });
    let fixture = Client::new(Runtime::new(document, None)?, a.runtime.background())?;
    let mut prior = std::mem::replace(&mut a.runtime, fixture);
    unsafe {
        SendMessageW(a.h(MODE), CB_SETCURSEL, 1, 0);
    }
    a.sources_command(MODE, CBN_SELCHANGE as u16)?;
    let index = a
        .sources
        .inputs
        .iter()
        .position(|i| i == "ui-test-input")
        .ok_or("Source input not listed")?;
    unsafe {
        SendMessageW(a.h(LIST), LB_SETCURSEL, index, 0);
        SendMessageW(a.h(ROUTE), CB_SETCURSEL, selected, 0);
    }
    a.sources_command(BIND, BN_CLICKED as u16)?;
    let (scope, _) = a.source_scope_inputs()?;
    let mut catalog = a.sources.catalog.clone();
    if catalog.remove_source(&source.id).is_ok() {
        return Err("Bound source removal accepted".into());
    }
    // Keep this test's one route exact, independent of other fixture temperatures.
    catalog.set_routes(&scope, &[("ui-test-input".into(), source.id.clone())])?;
    let routes = catalog.prepare(&scope, &["ui-test-input".into()])?;
    let mut resolver = Resolve;
    let mut registry = a.sources.registry.lock().unwrap();
    let light = registry.subscribe(scope.clone(), routes.clone(), &mut resolver)?;
    let other_scope = Scope::Lighting {
        identity: "selftest-reader".into(),
        digest: "0".repeat(64),
    };
    let other = registry.subscribe(other_scope, routes, &mut resolver)?;
    drop(registry);
    a.sources.lighting = Some(Arc::new(light));
    let end = Instant::now() + std::time::Duration::from_secs(2);
    loop {
        a.runtime.with(|r, _| a.sources.poll(r))?;
        if a.runtime
            .temperature_samples()
            .iter()
            .any(|r| r.key == "ui-source-test" && r.celsius == 42.)
        {
            break;
        }
        if Instant::now() >= end {
            return Err("Shared source did not reach App Runtime".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let old_path = a.path.clone();
    let mut next_document = a.runtime.document().clone();
    let next_source = next_document
        .temperatures
        .sources
        .iter_mut()
        .find(|s| s.key == "ui-source-test")
        .ok_or("Fixture source missing")?;
    next_source.key = "ui-source-next".into();
    next_source.value = crate::app::sensors::Value::Sensor {
        input: "ui-test-next".into(),
    };
    let next_path = a.state_dir.join("source-switch-selftest.json");
    next_document.save(&next_path)?;
    a.open_profile(&next_path)?;
    if a.sources.lighting.is_some() {
        return Err("Profile opening retained its old lighting subscription".into());
    }
    let end = Instant::now() + std::time::Duration::from_secs(3);
    while a.pending_open.is_some() {
        a.tick()?;
        if Instant::now() >= end {
            return Err("Source-enabled profile switch did not finish".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let next_index = a
        .sources
        .inputs
        .iter()
        .position(|input| input == "ui-test-next")
        .ok_or("Profile switch did not refresh source input list")?;
    if a.sources
        .inputs
        .iter()
        .any(|input| input == "ui-test-input")
    {
        return Err("Old source input remained in the new profile editor".into());
    }
    unsafe {
        SendMessageW(a.h(LIST), LB_SETCURSEL, next_index, 0);
        SendMessageW(a.h(ROUTE), CB_SETCURSEL, selected, 0);
    }
    let before = a.sources.catalog.clone();
    let stored = std::fs::read(&a.sources.path).map_err(|e| e.to_string())?;
    a.sources.inputs[next_index] = "ui-test-stale".into();
    if a.sources_command(BIND, BN_CLICKED as u16).is_ok()
        || a.sources.catalog != before
        || std::fs::read(&a.sources.path).map_err(|e| e.to_string())? != stored
    {
        return Err("Cached stale input was saved into the new profile".into());
    }
    a.refresh_sources();
    unsafe {
        SendMessageW(a.h(LIST), LB_SETCURSEL, next_index, 0);
        SendMessageW(a.h(ROUTE), CB_SETCURSEL, selected, 0);
    }
    a.sources_command(BIND, BN_CLICKED as u16)?;
    let (scope, _) = a.source_scope_inputs()?;
    if !a
        .sources
        .catalog
        .routes
        .iter()
        .any(|r| r.scope == scope && r.input == "ui-test-next")
        || a.sources
            .catalog
            .routes
            .iter()
            .any(|r| r.scope == scope && r.input == "ui-test-input")
    {
        return Err("New profile received the wrong source binding".into());
    }
    unsafe {
        SendMessageW(a.h(LIST), LB_SETCURSEL, next_index, 0);
    }
    a.sources_command(RELEASE, BN_CLICKED as u16)?;
    let sessions = a.sources.registry.lock().unwrap().status();
    if sessions.len() != 1 || sessions[0].consumers != 1 || sessions[0].stopping {
        return Err("Lighting release stopped the other consumer".into());
    }
    drop(other);
    let end = Instant::now() + std::time::Duration::from_secs(2);
    while !a.sources.drained() {
        if Instant::now() >= end {
            return Err("UI registry did not drain".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    std::mem::swap(&mut a.runtime, &mut prior);
    a.path = old_path;
    prior.stop();
    let end = Instant::now() + std::time::Duration::from_secs(2);
    while !prior.drained() {
        if Instant::now() >= end {
            return Err("UI source fixture runtime did not drain".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    a.sources.save(original)?;
    a.sources.selected = None;
    a.sources.draft = Draft::new(Family::Nvidia);
    a.sources.baseline = a.sources.draft.clone();
    unsafe {
        SendMessageW(a.h(MODE), CB_SETCURSEL, 0, 0);
    }
    a.refresh_sources();
    a.command(NAV_SCENE, BN_CLICKED as u16)?;
    // Corrupt preferences must be preserved rather than replaced with an empty catalog.
    let corrupt = a.state_dir.join("source-invalid-selftest.json");
    std::fs::write(&corrupt, b"{invalid").map_err(|e| e.to_string())?;
    let mut failed = Editor::load(corrupt.clone());
    if failed.save(Catalog::default()).is_ok()
        || std::fs::read(&corrupt).map_err(|e| e.to_string())? != b"{invalid"
    {
        return Err("Corrupt source catalog was overwritten".into());
    }
    std::fs::remove_file(corrupt).map_err(|e| e.to_string())?;
    discovery::exercise(a)?;
    unsafe {
        SendMessageW(a.h(MODE), CB_SETCURSEL, original_mode, 0);
    }
    a.refresh_sources();
    Ok(())
}
