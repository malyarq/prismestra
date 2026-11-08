//! Application-owned read subscriptions; the optional resolver is an explicit code capability.
//! No native resolver or synthetic default is created by this UI.
use super::*;
use crate::app::sensor_bindings::Resolved;
pub(super) const START: i32 = 1150;
pub(super) const STOP: i32 = 1151;
pub(super) const STATE: i32 = 1152;
pub(super) const MODULE: i32 = 1153;
pub(super) struct Active {
    pub(super) scope: Scope,
    pub(super) routes: Vec<Resolved>,
}
impl Active {
    fn matches(&self, catalog: &Catalog, scope: &Scope, inputs: &[String]) -> bool {
        self.scope == *scope
            && catalog
                .prepare(scope, inputs)
                .is_ok_and(|routes| routes == self.routes)
    }
}
impl App {
    pub(super) fn build_source_bindings(&mut self, parent: HWND) -> Result<(), String> {
        for (id, text) in [
            (START, "Запустить чтение"),
            (STOP, "Остановить"),
            (MODULE, "Файл…"),
        ] {
            self.control(parent, id, "BUTTON", text, BS_PUSHBUTTON as u32)?;
        }
        self.control(
            parent,
            STATE,
            "EDIT",
            "",
            (ES_MULTILINE | ES_READONLY) as u32 | WS_VSCROLL,
        )?;
        Ok(())
    }
    pub(super) fn source_bindings_layout(&self, w: i32, _h: i32) {
        let right = CONTENT_LEFT + 250;
        let width = w - right - 24;
        self.pos(START, right, 366, 250, 32);
        self.pos(STOP, right + 260, 366, (width - 260).max(110), 32);
        self.pos(STATE, right, 412, width, 112);
        if let Some(slot) = match self.sources.draft.family {
            Family::Amd => Some(3),
            Family::Ddr5 => Some(5),
            _ => None,
        } {
            self.pos(FIELD + slot, right + 208, 308 + slot * 30, width - 320, 26);
            self.pos(MODULE, right + width - 104, 308 + slot * 30, 104, 26);
        }
        self.refresh_source_binding_state();
    }
    pub(super) fn refresh_source_binding_state(&self) {
        if !self.widgets.contains_key(&STATE) {
            return;
        }
        let active = self.sources.lighting_active.as_ref();
        let running = self.sources.lighting.is_some();
        let mut text = if let Some(active) = active {
            let same = self.source_scope_inputs().is_ok_and(|(scope, inputs)| {
                active.matches(&self.sources.catalog, &scope, &inputs)
            });
            if same {
                format!(
                    "Активных входов: {}. Привязки совпадают с сохранёнными. {}",
                    active.routes.len(),
                    "Чтение обслуживает LCD и логические температуры."
                )
            } else {
                "Активные привязки отличаются от сохранённых. Для применения остановите эту задачу и запустите её снова.".into()
            }
        } else if self.sources.resolver.is_none() {
            "Чтение не разрешено в режиме предпросмотра. Сохранение профиля и привязок не включает оборудование.".into()
        } else {
            "Чтение не запущено. Будут использованы только явные привязки текущего профиля; недостающие показания не подставляются.".into()
        };
        if let Some(active) = active {
            let selected =
                usize::try_from(unsafe { SendMessageW(self.h(LIST), LB_GETCURSEL, 0, 0) })
                    .ok()
                    .and_then(|i| self.sources.inputs.get(i));
            let route = selected
                .and_then(|input| active.routes.iter().find(|r| r.input == *input))
                .or_else(|| active.routes.first());
            if let Some(route) = route {
                let registry = self
                    .sources
                    .registry
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let status = registry
                    .status()
                    .into_iter()
                    .find(|s| s.target == route.target);
                let label = route.input.chars().take(80).collect::<String>();
                let details = status
                    .map(|s| {
                        let value = s
                            .worker
                            .reading
                            .map(|r| {
                                format!(
                                    "{:.1} °C, возраст {} мс",
                                    r.celsius,
                                    crate::clock::monotonic_ms().saturating_sub(r.observed_ms)
                                )
                            })
                            .unwrap_or("нет показания".into());
                        let error = s
                            .worker
                            .error
                            .as_deref()
                            .map(|e| {
                                format!(
                                    "; {}",
                                    e.chars()
                                        .filter(|c| !c.is_control())
                                        .take(160)
                                        .collect::<String>()
                                )
                            })
                            .unwrap_or_default();
                        format!("{label}: {value}; потребителей {}{error}", s.consumers)
                    })
                    .unwrap_or(format!("{label}: reader ещё не доступен."));
                text.push_str("\r\n");
                text.push_str(&details);
            }
        }

        let mut format_rect = RECT::default();
        unsafe {
            SendMessageW(
                self.h(STATE),
                EM_GETRECT,
                0,
                (&mut format_rect as *mut RECT) as isize,
            );
        }
        let text = self.wrap_control_text(
            STATE,
            &text,
            (format_rect.right - format_rect.left - px(8, self.dpi)).max(1),
        );
        if self.text(STATE) != text {
            let first = unsafe { SendMessageW(self.h(STATE), EM_GETFIRSTVISIBLELINE, 0, 0) };
            let (mut start, mut end) = (0u32, 0u32);
            unsafe {
                SendMessageW(
                    self.h(STATE),
                    EM_GETSEL,
                    (&mut start as *mut u32) as usize,
                    (&mut end as *mut u32) as isize,
                );
            }
            self.set_text(STATE, &text);
            // Live age updates must not reset the user's scroll/copy selection.
            unsafe {
                SendMessageW(self.h(STATE), EM_SETSEL, start as usize, end as isize);
                let current = SendMessageW(self.h(STATE), EM_GETFIRSTVISIBLELINE, 0, 0);
                SendMessageW(self.h(STATE), EM_LINESCROLL, 0, first - current);
            }
        }
        self.set_text(START, "Читать для LCD");
        self.set_text(STOP, "Остановить");
        unsafe {
            EnableWindow(
                self.h(START),
                i32::from(
                    !running
                        && self.sources.resolver.is_some()
                        && !self.shutdown.active()
                        && self.runtime.temperature_input_ready(),
                ),
            );
            EnableWindow(self.h(STOP), i32::from(running && !self.shutdown.active()));
        }
    }
    fn start_bound_sources(&mut self) -> Result<(), String> {
        if self.shutdown.active()
            || self.pending_open.is_some()
            || !self.runtime.temperature_input_ready()
        {
            return Err("Дождитесь завершения перехода профиля или обновления устройств.".into());
        }
        let (scope, inputs) = self.source_scope_inputs()?;
        if inputs.is_empty() {
            return Err("В этом профиле нет внешних температурных входов.".into());
        }
        let routes = self.sources.catalog.prepare(&scope, &inputs)?;

        if self.sources.lighting.is_some() {
            return Err("Чтение этого профиля уже запущено.".into());
        }
        let resolver = self
            .sources
            .resolver
            .as_deref_mut()
            .ok_or("Чтение датчиков не разрешено.")?;
        let registry = self.sources.registry.clone();
        let subscription = self.runtime.with(|r, now| {
            r.validate_registry_input(&scope, &routes.iter().map(|r| r.input.clone()).collect())?;
            let subscription = Arc::new(
                registry
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .subscribe(scope.clone(), routes.clone(), resolver)?,
            );
            if let Err(error) = r.bind_registry_input(registry.clone(), subscription.clone(), now) {
                subscription.revoke();
                registry.lock().unwrap_or_else(|e| e.into_inner()).poll();
                return Err(error);
            }
            Ok(subscription)
        })?;
        self.sources.lighting = Some(subscription);
        self.sources.lighting_active = Some(Active { scope, routes });
        self.status =
            "Чтение привязанных источников запущено; привязки зафиксированы до остановки.".into();

        Ok(())
    }
    pub(super) fn source_bindings_command(&mut self, id: i32, notice: u16) -> Result<bool, String> {
        if ![START, STOP].contains(&id) {
            return Ok(false);
        }
        if !self.source_routes_mode() || notice != BN_CLICKED as u16 {
            return Ok(true);
        }
        match id {
            START => self.start_bound_sources()?,
            STOP => {
                self.sources.release_lighting();
                self.runtime.with(|r, _| self.sources.poll(r))?;

                self.status =
                    "Остановка запрошена. Другие подписчики общего reader продолжают работу."
                        .into();
            }
            _ => unreachable!(),
        }
        self.refresh_source_binding_state();
        Ok(true)
    }
}
pub(super) fn module_dialog(hwnd: HWND, state: &State) -> Result<(), String> {
    let draft = {
        let slot = state.app.try_borrow().map_err(|_| "UI занято")?;
        let a = slot.as_ref().ok_or("UI не создан")?;
        if a.shutdown.active()
            || a.page != Page::Sources
            || a.source_routes_mode()
            || a.source_discovery_mode()
            || !matches!(a.read_source_draft().family, Family::Amd | Family::Ddr5)
        {
            return Err("Откройте форму датчика AMD или DDR5.".into());
        }
        a.read_source_draft()
    };
    let Some(path) = file_dialog(
        hwnd,
        false,
        "Модули PawnIO (*.bin)\0*.bin\0Все файлы\0*.*\0\0",
        "bin",
    )?
    else {
        return Ok(());
    };
    let edited = crate::app::sensor_modules::select(&draft, &path)?;
    let mut slot = state.app.try_borrow_mut().map_err(|_| "UI занято")?;
    let a = slot.as_mut().ok_or("UI не создан")?;
    if a.shutdown.active()
        || a.page != Page::Sources
        || a.source_routes_mode()
        || a.source_discovery_mode()
        || a.read_source_draft() != draft
    {
        return Err("Форма изменилась, выбранный модуль не применён.".into());
    }
    a.sources.draft = edited;
    a.fill_source_draft();
    a.status =
        "Размер и SHA-256 модуля проверены. Сохраните датчик; драйвер и модуль не запускались."
            .into();
    Ok(())
}
