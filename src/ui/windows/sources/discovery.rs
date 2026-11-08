//! Explicit metadata selection. Adopting a candidate edits a draft only.
use super::*;
use crate::app::sensor_discovery::{self, Outcome};
pub(super) const SCAN: i32 = 1140;
pub(super) const CANCEL: i32 = 1141;
pub(super) const ADOPT: i32 = 1142;
pub(super) const DETAIL: i32 = 1143;
pub(super) fn is_control(id: i32) -> bool {
    (SCAN..=DETAIL).contains(&id)
}
impl App {
    pub(super) fn build_source_discovery(&mut self, parent: HWND) -> Result<(), String> {
        for (id, text) in [
            (SCAN, "Найти датчики"),
            (CANCEL, "Отменить поиск"),
            (ADOPT, "Настроить выбранный"),
        ] {
            self.control(parent, id, "BUTTON", text, BS_PUSHBUTTON as u32)?;
        }
        self.control(
            parent,
            DETAIL,
            "EDIT",
            "",
            ES_MULTILINE as u32
                | ES_READONLY as u32
                | ES_AUTOVSCROLL as u32
                | WS_VSCROLL
                | WS_BORDER,
        )?;
        unsafe {
            SendMessageW(self.h(DETAIL), EM_SETLIMITTEXT, 32760, 0);
        }
        Ok(())
    }
    pub(super) fn source_discovery_layout(&self, w: i32, h: i32) {
        let right = CONTENT_LEFT + 250;
        let width = w - right - 24;
        self.pos(SCAN, CONTENT_LEFT, h - 104, 226, 32);
        self.pos(INFO, right, 182, width, 38);
        self.pos(DETAIL, right, 230, width, (h - 350).max(180));
        self.pos(ADOPT, right, h - 104, 220, 32);
        self.pos(CANCEL, right + 230, h - 104, (width - 230).max(100), 32);
    }
    pub(in super::super) fn select_source_discovery(&mut self) {
        unsafe {
            SendMessageW(self.h(MODE), CB_SETCURSEL, 2, 0);
        }
        self.refresh_sources();
        self.sources_layout(
            (i64::from(self.width) * 96 / i64::from(self.dpi)) as i32,
            (i64::from(self.height) * 96 / i64::from(self.dpi)) as i32,
        );
        self.show_page();
    }
    pub(super) fn refresh_source_discovery(&self) {
        if let Some(inventory) = &self.sources.inventory {
            for candidate in &inventory.candidates {
                let state = match candidate.draft.as_ref() {
                    None => "Не поддержан",
                    Some(d) if d.target().is_err() => "Нужен модуль",
                    Some(_) => "Точный ID",
                };
                unsafe {
                    SendMessageW(
                        self.h(LIST),
                        LB_ADDSTRING,
                        0,
                        wide(&format!("{state} · {}", candidate.label)).as_ptr() as isize,
                    );
                }
            }
            unsafe {
                SendMessageW(self.h(LIST), LB_SETCURSEL, 0, 0);
            }
        }
        self.refresh_source_discovery_details();
    }
    pub(super) fn refresh_source_discovery_details(&self) {
        let caption = self.wrap_control_text(
            INFO,
            &self.sources.scan_message,
            px(
                ((i64::from(self.width) * 96 / i64::from(self.dpi)) as i32 - CONTENT_LEFT - 282)
                    .max(1),
                self.dpi,
            ),
        );
        if self.text(INFO) != caption {
            self.set_text(INFO, &caption);
        }
        let index = usize::try_from(unsafe { SendMessageW(self.h(LIST), LB_GETCURSEL, 0, 0) }).ok();
        let candidate = self
            .sources
            .inventory
            .as_ref()
            .and_then(|i| index.and_then(|n| i.candidates.get(n)));
        let mut detail = String::new();
        if let Some(candidate) = candidate {
            detail.push_str(&format!("{}\r\n{}\r\n", candidate.label, candidate.detail));
            if let Some(draft) = &candidate.draft {
                for (label, value) in draft.labels().iter().zip(&draft.values) {
                    if !label.is_empty() {
                        detail.push_str(&format!(
                            "\r\n{label}: {}",
                            if value.is_empty() {
                                "не выбран"
                            } else {
                                value
                            }
                        ));
                    }
                }
                detail.push_str("\r\n\r\n«Настроить» перенесёт метаданные в форму. Сохранение и чтение выполняются отдельно.");
            }
        } else if self.sources.inventory.is_none() {
            detail.push_str(&format!("{}\r\n\r\n", self.sources.scan_message));
            detail.push_str("Поиск получает идентичность устройства, а не температуру.\r\n\r\nNVIDIA: требуется vendor API.\r\nCPU: CPUID и топология Windows; Tctl поддержан только для одного AMD package, family 17h–1Ah.\r\nTT Liquid: точный MI00 и полные HID caps.\r\nDDR5: автоматическая проба SMBus не входит в этот поиск; адреса не подставляются.\r\n\r\nЧужие программы, охлаждение и подсветка не переключаются.");
        } else {
            detail.push_str("В этом поиске нет подходящих кандидатов.");
        }
        if let Some(inventory) = &self.sources.inventory
            && !inventory.issues.is_empty()
        {
            detail.push_str("\r\n\r\nОграничения и ошибки поиска:");
            for issue in &inventory.issues {
                detail.push_str(&format!("\r\n• {issue}"));
            }
        }
        let width =
            (i64::from(self.width) * 96 / i64::from(self.dpi)) as i32 - CONTENT_LEFT - 250 - 48;
        let detail = self.wrap_control_text(
            DETAIL,
            &detail.replace("\r\n", "\n"),
            px(width.max(1), self.dpi),
        );
        if self.text(DETAIL) != detail {
            self.set_text(DETAIL, &detail);
        }
        unsafe {
            EnableWindow(
                self.h(SCAN),
                i32::from(
                    self.sources.scan.is_idle()
                        && self.sources.scan_factory.is_some()
                        && !self.shutdown.active(),
                ),
            );
            EnableWindow(
                self.h(CANCEL),
                i32::from(!self.sources.scan.is_idle() && !self.shutdown.active()),
            );
            EnableWindow(
                self.h(ADOPT),
                i32::from(
                    self.sources.scan.is_idle()
                        && candidate.is_some_and(|c| c.draft.is_some())
                        && !self.shutdown.active(),
                ),
            );
        }
    }
    pub(in super::super) fn poll_source_discovery(&mut self) -> bool {
        let Some(outcome) = self.sources.scan.poll() else {
            return false;
        };
        match outcome {
            Outcome::Ready {
                generation: _,
                inventory,
            } => {
                self.sources.scan_message = format!(
                    "Найдено кандидатов: {}. Это метаданные; чтение температур не проверялось.",
                    inventory.candidates.len()
                );
                self.sources.inventory = Some(inventory);
            }
            Outcome::Failed {
                generation: _,
                error,
            } => {
                self.sources.inventory = None;
                self.sources.scan_message = format!("Поиск не завершён: {error}");
            }
            Outcome::Cancelled { generation: _ } => {
                self.sources.inventory = None;
                self.sources.scan_message =
                    "Поиск отменён. Устройства и настройки не изменялись.".into();
            }
        }
        if self.page == Page::Sources && self.source_discovery_mode() {
            self.refresh_sources();
            self.show_page();
        }
        true
    }
    pub(super) fn source_discovery_command(
        &mut self,
        id: i32,
        notice: u16,
    ) -> Result<bool, String> {
        if !self.source_discovery_mode() {
            return Ok(false);
        }
        match id {
            LIST if notice == LBN_SELCHANGE as u16 => self.refresh_source_discovery_details(),
            SCAN if notice == BN_CLICKED as u16 => {
                let factory =
                    self.sources.scan_factory.clone().ok_or(
                        "Поиск недоступен в режиме предпросмотра. Добавьте датчик вручную.",
                    )?;
                self.sources.scan.start(factory)?;
                self.sources.inventory = None;
                self.sources.scan_message = "Ищу метаданные устройств…".into();
                self.refresh_sources();
            }
            CANCEL if notice == BN_CLICKED as u16 => {
                self.sources.scan.cancel();
                self.sources.scan_message =
                    "Отмена: ожидаю завершения уже начатого запроса…".into();
                self.refresh_source_discovery_details();
            }
            ADOPT if notice == BN_CLICKED as u16 => {
                if !self.sources.scan.is_idle() {
                    return Err("Дождитесь завершения поиска.".into());
                }
                let index =
                    usize::try_from(unsafe { SendMessageW(self.h(LIST), LB_GETCURSEL, 0, 0) })
                        .map_err(|_| "Выберите устройство.")?;
                let draft = self
                    .sources
                    .inventory
                    .as_ref()
                    .and_then(|i| i.candidates.get(index))
                    .and_then(|c| c.draft.clone())
                    .ok_or("Этот кандидат не поддерживается или неоднозначен.")?;
                if self
                    .sources
                    .catalog
                    .sources
                    .iter()
                    .any(|s| s.id == draft.id)
                {
                    return Err("Такой идентификатор уже сохранён. Выберите его в разделе «Датчики»; поиск не заменяет сохранённые настройки.".into());
                }
                self.sources.selected = None;
                self.sources.baseline = Draft::new(draft.family);
                self.sources.draft = draft;
                unsafe {
                    SendMessageW(self.h(MODE), CB_SETCURSEL, 0, 0);
                }
                self.refresh_sources();
                self.layout(self.width, self.height);
                self.status="Метаданные перенесены в форму. Проверьте поля и сохраните датчик; чтение не запускалось.".into();
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

fn fixture() -> Result<sensor_discovery::Inventory, String> {
    use crate::{
        lcd::hid_policy::Descriptor,
        sensors::{amd, ddr5, nvidia},
    };
    sensor_discovery::collect(sensor_discovery::Metadata {
        nvidia: Ok(vec![nvidia::PciIdentity {
            device_id: 0x123410de,
            subsystem_id: 7,
            revision_id: 1,
            extended_device_id: 9,
        }]),
        cpu: Ok(amd::CpuIdentity {
            vendor: *b"GenuineIntel",
            signature: 0x000006a0,
            packages: 1,
        }),
        ddr5: Ok(vec![ddr5::Binding {
            controller: [
                u64::from_le_bytes(*b"PIIX4\0\0\0"),
                0xb00,
                0x11223344790b1022,
            ],
            port: 0,
            address: 0x50,
        }]),
        liquid: Ok(vec![Descriptor {
            path: r"\\?\hid#vid_264a&pid_233c&mi_00#stand-liquid#{synthetic}".into(),
            vid: 0x264a,
            pid: 0x233c,
            usage_page: 0xff06,
            usage: 1,
            input_len: 441,
            output_len: 441,
            feature_len: 0,
            input_buttons: 0,
            output_buttons: 0,
            input_ids: vec![0],
            output_ids: vec![0],
        }]),
    })
}
pub(super) fn exercise(a: &mut App) -> Result<(), String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let old_factory = a.sources.scan_factory.take();
    a.page = Page::Sources;
    a.sources.scan_factory = Some(Arc::new(|_| fixture()));
    a.select_source_discovery();
    let catalog = a.sources.catalog.clone();
    let stored = std::fs::read(&a.sources.path).map_err(|e| e.to_string())?;
    a.sources_command(SCAN, BN_CLICKED as u16)?;
    let end = Instant::now() + std::time::Duration::from_secs(3);
    while !a.sources.scan.is_idle() {
        a.poll_source_discovery();
        if Instant::now() >= end {
            return Err("Source discovery UI did not reap metadata worker".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let inventory = a
        .sources
        .inventory
        .as_ref()
        .ok_or("Source inventory missing")?;
    let unsupported = inventory
        .candidates
        .iter()
        .position(|c| c.draft.is_none())
        .ok_or("Unsupported CPU not shown")?;
    let gpu = inventory
        .candidates
        .iter()
        .position(|c| c.draft.as_ref().is_some_and(|d| d.family == Family::Nvidia))
        .ok_or("Detected GPU missing")?;
    unsafe {
        SendMessageW(a.h(LIST), LB_SETCURSEL, unsupported, 0);
    }
    if a.sources_command(ADOPT, BN_CLICKED as u16).is_ok() {
        return Err("Unsupported source was adopted".into());
    }
    unsafe {
        SendMessageW(a.h(LIST), LB_SETCURSEL, gpu, 0);
    }
    a.sources_command(ADOPT, BN_CLICKED as u16)?;
    if a.sources.catalog != catalog
        || std::fs::read(&a.sources.path).map_err(|e| e.to_string())? != stored
        || !a.sources.registry.lock().unwrap().status().is_empty()
        || a.guard_sources_exit().is_ok()
    {
        return Err("Metadata adoption saved, activated or lost its dirty draft".into());
    }
    a.sources_command(SAVE_SOURCE, BN_CLICKED as u16)?;
    let saved = a.sources.catalog.clone();
    let stored = std::fs::read(&a.sources.path).map_err(|e| e.to_string())?;
    a.select_source_discovery();
    unsafe {
        SendMessageW(a.h(LIST), LB_SETCURSEL, gpu, 0);
    }
    if a.sources_command(ADOPT, BN_CLICKED as u16).is_ok()
        || a.sources.catalog != saved
        || std::fs::read(&a.sources.path).map_err(|e| e.to_string())? != stored
    {
        return Err("Discovery replaced saved source preferences".into());
    }
    unsafe {
        SendMessageW(a.h(MODE), CB_SETCURSEL, 0, 0);
    }
    a.refresh_sources();
    a.sources_command(REMOVE, BN_CLICKED as u16)?;
    if a.sources.catalog != catalog {
        return Err("Discovery source removal changed unrelated preferences".into());
    }
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let gate = release.clone();
    let active = entered.clone();
    a.sources.scan_factory = Some(Arc::new(move |_| {
        active.store(true, Ordering::Release);
        let end = Instant::now() + std::time::Duration::from_secs(3);
        while !gate.load(Ordering::Acquire) {
            if Instant::now() >= end {
                return Err("Fixed metadata gate timed out".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        fixture()
    }));
    a.select_source_discovery();
    a.sources_command(SCAN, BN_CLICKED as u16)?;
    let end = Instant::now() + std::time::Duration::from_secs(2);
    while !entered.load(Ordering::Acquire) {
        if Instant::now() >= end {
            return Err("Fixed metadata call did not enter".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    a.sources_command(CANCEL, BN_CLICKED as u16)?;
    let rejected = a.sources_command(SCAN, BN_CLICKED as u16).is_err()
        && !a.sources.scan.is_idle()
        && !a.poll_source_discovery();
    release.store(true, Ordering::Release);
    while !a.sources.scan.is_idle() {
        a.poll_source_discovery();
        if Instant::now() >= end {
            return Err("Cancelled source scan did not drain".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    if !rejected || a.sources.inventory.is_some() || !a.sources.scan_message.contains("отменён")
    {
        return Err(
            "Cancelled source scan released its barrier or published stale inventory".into(),
        );
    }
    a.sources.inventory = Some(fixture()?);
    a.sources.scan_message =
        "Стенд: метаданные четырёх кандидатов. Реальное оборудование не опрашивалось.".into();
    a.sources.scan_factory = old_factory;
    a.refresh_sources();
    Ok(())
}
