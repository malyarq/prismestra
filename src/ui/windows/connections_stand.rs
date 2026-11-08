//! Actual owned controls/worker/files; no native metadata or output providers.
use super::*;
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
fn directory(state: &State) -> Result<PathBuf, String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let base = state
        .app
        .borrow()
        .as_ref()
        .ok_or("UI state is unavailable")?
        .state_dir
        .join("diagnostics/connections-stand");
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    for _ in 0..1024 {
        let leaf = base.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::create_dir(&leaf) {
            Ok(()) => return Ok(leaf),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("Cannot reserve connections stand directory".into())
}

pub(super) fn openrgb_controller() -> crate::transport::openrgb::ControllerInfo {
    use crate::transport::openrgb::{ControllerInfo, MatrixInfo, ZoneInfo};
    ControllerInfo {
        id: 7,
        name: "Software controller".into(),
        vendor: "Fixture".into(),
        serial: "not-hardware".into(),
        location: "software-only".into(),
        kind: "Keyboard".into(),
        leds: 3,
        led_names: vec!["a".into(), "b".into(), "c".into()],
        supports_direct: true,
        zones: vec![ZoneInfo {
            id: 0,
            name: "keys".into(),
            kind: "matrix".into(),
            leds: 3,
            reported_leds: 3,
            led_offset: 0,
            matrix: Some(MatrixInfo {
                width: 2,
                height: 2,
                led_indices: vec![Some(2), Some(0), None, Some(1)],
            }),
            reported_matrix: None,
            matrix_warning: None,
        }],
    }
}

pub(super) fn exercise_openrgb(state: &State) -> Result<(), String> {
    let root = directory(state)?;
    let (original, page, dirty, old) = {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        if a.native_lighting.is_some() {
            return Err("OpenRGB UI stand forbids native lighting".into());
        }
        let original = a.runtime.document().clone();
        let page = a.page;
        let dirty = a.dirty;
        let old = std::mem::replace(
            &mut a.connections,
            Editor::new(root.join("local.json"), Some(stand_factory())).activation(true, true),
        );
        let mut document = original.clone();
        document.lighting.setup.devices = vec![crate::devices::Device {
            id: "sdk-stand".into(),
            label: "OpenRGB fixture".into(),
            role: crate::devices::Role::Canvas,
            driver: crate::devices::Driver::OpenRgb,
            leds: 1,
            rate_hz: 25,
            enabled: true,
        }];
        document.lighting.setup.canvas = crate::devices::Canvas {
            width: 100,
            height: 80,
            points: vec![crate::devices::Point {
                device: "sdk-stand".into(),
                led: 0,
                x: 0,
                y: 0,
            }],
        };
        document.lighting.groups = crate::app::config::Groups {
            pc: vec!["sdk-stand".into()],
            door: vec![],
        };
        document.lcd.clear();
        document.ambient = Default::default();
        a.runtime
            .request_document_at(document, root.join("journal.json"))?;
        (original, page, dirty, old)
    };
    settle(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        a.page = Page::Connections;
        a.open_connections()?;
        a.layout(a.width, a.height);
        a.connections_command(SCAN, BN_CLICKED as u16)?;
    }
    settle(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        unsafe {
            SendMessageW(a.h(LIST), LB_SETCURSEL, 0, 0);
        }
        a.connections_command(LIST, LBN_SELCHANGE as u16)?;
        let revision = a.runtime.output_revision();
        a.connections_command(SAVE_BINDING, BN_CLICKED as u16)?;
        if a.runtime.output_revision() != revision
            || a.runtime.document().lighting.setup.devices[0].leds != 1
        {
            return Err("Saving OpenRGB metadata activated or changed the portable model".into());
        }
        a.connections_command(APPLY_OUTPUTS, BN_CLICKED as u16)?;
    }
    settle(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        let setup = &a.runtime.document().lighting.setup;
        if setup.devices[0].leds != 3
            || setup
                .canvas
                .points
                .iter()
                .map(|p| (p.x, p.y))
                .collect::<Vec<_>>()
                != vec![(99, 0), (99, 79), (0, 0)]
            || !a.dirty
            || a.native_lighting.is_some()
        {
            return Err(
                "OpenRGB UI did not apply exact discovered matrix through software outputs".into(),
            );
        }
        a.connections_command(REVERT_OUTPUTS, BN_CLICKED as u16)?;
    }
    settle(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        if a.runtime.document().lighting.setup.devices[0].leds != 1 {
            return Err(
                "OpenRGB UI reversion did not restore the previous document geometry".into(),
            );
        }
        a.connections_command(APPLY_OUTPUTS, BN_CLICKED as u16)?;
    }
    settle(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        let mut changed = a.runtime.document().clone();
        changed.lighting.scene.brightness = if changed.lighting.scene.brightness == 60 {
            59
        } else {
            60
        };
        changed.lighting.schedule.enabled = !changed.lighting.schedule.enabled;
        a.runtime
            .request_document_at(changed, root.join("edited.json"))?;
    }
    settle(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        let changed = a.runtime.document().clone();
        let error = a
            .connections_command(REVERT_OUTPUTS, BN_CLICKED as u16)
            .err()
            .ok_or("Revert discarded later profile edits")?;
        if !error.contains("профиль изменён")
            || a.connections.previous.is_some()
            || a.runtime.document() != &changed
            || a.runtime.pending_configuration()
        {
            return Err("Revert guard did not preserve scene/schedule edits".into());
        }
        let journal = journal_path(&a.state_dir, a.path.as_deref(), &original);
        a.runtime.request_document_at(original.clone(), journal)?;
    }
    settle(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        a.connections = old;
        a.page = page;
        a.dirty = dirty;
        a.reset_editor(0);
        a.attach_outputs()?;
        a.layout(a.width, a.height);
        if page == Page::Connections {
            a.open_connections()?;
        }
        a.show_page();
        if a.runtime.document() != &original {
            return Err("OpenRGB stand changed the original document".into());
        }
    }
    eprintln!(
        "OpenRGB UI stand PASS: read-only fixture discovery, selection, Save without activation, atomic geometry/outputs Apply and document reversion; no SDK connection or hardware outputs"
    );
    Ok(())
}
pub(super) fn exercise_home_assistant(state: &State) -> Result<(), String> {
    let root = directory(state)?;
    let (original, page, dirty, old) = {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        if a.native_lighting.is_some() {
            return Err("HA UI stand forbids native outputs".into());
        }
        let original = a.runtime.document().clone();
        let page = a.page;
        let dirty = a.dirty;
        let old = std::mem::replace(
            &mut a.connections,
            Editor::new(root.join("local.json"), Some(stand_factory())).activation(true, true),
        );
        let mut document = original.clone();
        document.lighting.setup.devices = vec![crate::devices::Device {
            id: "ha-stand".into(),
            label: "Software light".into(),
            role: crate::devices::Role::Canvas,
            driver: crate::devices::Driver::HomeAssistant,
            leds: 1,
            rate_hz: 1,
            enabled: true,
        }];
        document.lighting.setup.canvas = crate::devices::Canvas {
            width: 1,
            height: 1,
            points: vec![crate::devices::Point {
                device: "ha-stand".into(),
                led: 0,
                x: 0,
                y: 0,
            }],
        };
        document.lighting.groups = Groups {
            pc: vec!["ha-stand".into()],
            door: vec![],
        };
        document.lcd.clear();
        document.ambient = Default::default();
        a.runtime
            .request_document_at(document, root.join("journal.json"))?;
        (original, page, dirty, old)
    };
    settle(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        a.page = Page::Connections;
        a.open_connections()?;
        a.layout(a.width, a.height);
        a.set_text(ENDPOINT, "http://127.0.0.1:8123");
        a.connections_command(ENDPOINT, EN_CHANGE as u16)?;
        a.set_text(SERIAL, "fake.test-token");
        a.connections_command(SERIAL, EN_CHANGE as u16)?;
        if unsafe { SendMessageW(a.h(SERIAL), EM_GETPASSWORDCHAR, 0, 0) } != 0x2022 {
            return Err("HA token control is not masked".into());
        }
        if a.connections_command(SAVE_BINDING, BN_CLICKED as u16)
            .is_ok()
            || root.join("local.json").exists()
        {
            return Err("Unsaved token entered serializable bindings".into());
        }
        a.connections_command(SAVE_KEY, BN_CLICKED as u16)?;
        if !a.text(SERIAL).is_empty() || a.connections.stand_keys.len() != 1 {
            return Err("HA key save did not clear field/store fake-only key".into());
        }
        a.connections_command(SCAN, BN_CLICKED as u16)?;
    }
    settle(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        unsafe {
            SendMessageW(a.h(LIST), LB_SETCURSEL, 0, 0);
        }
        a.connections_command(LIST, LBN_SELCHANGE as u16)?;
        if !a.text(SERIAL).is_empty() {
            return Err("HA selection exposed a credential reference as a token".into());
        }
        let revision = a.runtime.output_revision();
        a.connections_command(SAVE_BINDING, BN_CLICKED as u16)?;
        let local = std::fs::read_to_string(root.join("local.json")).map_err(|e| e.to_string())?;
        let portable = serde_json::to_string(a.runtime.document()).map_err(|e| e.to_string())?;
        if local.contains("fake.test-token")
            || portable.contains("fake.test-token")
            || portable.contains("127.0.0.1:8123")
            || a.runtime.output_revision() != revision
        {
            return Err("HA binding save leaked a secret/endpoint or activated output".into());
        }
        a.connections_command(APPLY_OUTPUTS, BN_CLICKED as u16)?;
    }
    settle(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        if a.runtime.outputs_ready() != Ok(true) || a.native_lighting.is_some() {
            return Err("HA software Apply failed".into());
        }
        a.connections_command(REVERT_OUTPUTS, BN_CLICKED as u16)?;
    }
    settle(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        a.connections_command(REMOVE, BN_CLICKED as u16)?;
        a.connections_command(SAVE_BINDING, BN_CLICKED as u16)?;
        a.connections_command(REMOVE, BN_CLICKED as u16)?;
        a.connections_command(SAVE_BINDING, BN_CLICKED as u16)?;
        if !crate::app::lighting_outputs::Local::read_draft(
            &root.join("local.json"),
            a.runtime.document(),
        )?
        .devices
        .is_empty()
        {
            return Err("HA removed binding persisted".into());
        }
        let journal = journal_path(&a.state_dir, a.path.as_deref(), &original);
        a.runtime.request_document_at(original.clone(), journal)?;
    }
    settle(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        a.connections = old;
        a.page = page;
        a.dirty = dirty;
        a.reset_editor(0);
        a.attach_outputs()?;
        a.layout(a.width, a.height);
        if page == Page::Connections {
            a.open_connections()?;
        }
        a.show_page();
        if a.runtime.document() != &original {
            return Err("HA stand did not restore original profile".into());
        }
    }
    eprintln!(
        "Home Assistant UI stand PASS: masked key, independent fake credential save, discovery, secret-free binding save, software Apply/revert; no HTTP or Windows credential writes"
    );
    Ok(())
}

fn settle(state: &State) -> Result<(), String> {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let done = {
            let mut slot = state.app.borrow_mut();
            let a = slot.as_mut().unwrap();
            a.tick()?;
            !a.runtime.pending_configuration()
                && a.connections.scan.is_idle()
                && a.connections.pending.is_none()
                && !a.connections.watch_outputs
        };
        if done {
            return Ok(());
        }
        if Instant::now() >= end {
            return Err("Connections stand did not drain".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
pub(super) fn snapshot_scan(state: &State) -> Result<(), String> {
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        if !a.connections.scannable() {
            return Ok(());
        }
        a.connections_command(SCAN, BN_CLICKED as u16)?;
    }
    settle(state)
}
pub(super) fn exercise(_hwnd: HWND, state: &State) -> Result<(), String> {
    let root = directory(state)?;
    let file = root.join("local.json");
    let (original, page, old_editor, original_dirty) = {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        if a.native_lighting.is_some() {
            return Err("Connections stand forbids native lighting".into());
        }
        let original = a.runtime.document().clone();
        let page = a.page;
        let original_dirty = a.dirty;
        a.dirty = false;
        let old = std::mem::replace(
            &mut a.connections,
            Editor::new(file.clone(), Some(stand_factory())).activation(true, true),
        );
        let mut doc = original.clone();
        doc.lighting.setup.devices = vec![crate::devices::Device {
            id: "connection-stand".into(),
            label: "Контроллер стенда".into(),
            role: crate::devices::Role::Canvas,
            driver: crate::devices::Driver::Fusion2 { headers: [27, 27] },
            leds: 54,
            rate_hz: 20,
            enabled: true,
        }];
        doc.lighting.setup.canvas = crate::devices::Canvas {
            width: 54,
            height: 1,
            points: (0..54)
                .map(|led| crate::devices::Point {
                    device: "connection-stand".into(),
                    led,
                    x: led as u32,
                    y: 0,
                })
                .collect(),
        };
        doc.lighting.groups = Groups {
            pc: vec!["connection-stand".into()],
            door: vec![],
        };
        doc.ambient = Default::default();
        doc.lcd.clear();
        doc.temperatures = Default::default();
        a.runtime
            .request_document_at(doc, root.join("runtime.json"))?;
        (original, page, old, original_dirty)
    };
    settle(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        a.page = Page::Connections;
        a.reset_editor(0);
        a.layout(a.width, a.height);
        a.open_connections()?;
    }
    snapshot_scan(state)?;
    let before = {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        if a.connections
            .inventory
            .as_ref()
            .is_none_or(|i| i.candidates.len() != 1)
        {
            return Err(format!("Connections stand metadata missing: {}", a.status));
        }
        unsafe {
            SendMessageW(a.h(LIST), LB_SETCURSEL, 0, 0);
        }
        a.connections_command(LIST, LBN_SELCHANGE as u16)?;
        if a.guard_connections_exit().is_ok() {
            return Err("Unsaved binding was not guarded".into());
        }
        a.connections_command(SAVE_BINDING, BN_CLICKED as u16)?;
        if !file.exists() || a.connections.dirty() {
            return Err("Binding UI did not save and clear dirty state".into());
        }
        if crate::app::lighting_outputs::Local::read(&file, a.runtime.document())?
            .devices
            .get("connection-stand")
            != Some(&crate::devices::Binding::Hid {
                path: r"\\?\hid#vid_048d&pid_5702#stand#{4d1e55b2-f16f-11cf-88cb-001111000030}"
                    .into(),
            })
        {
            return Err("Binding UI saved a different physical target".into());
        }
        std::fs::read(&file).map_err(|e| e.to_string())?
    };
    let original_revision = state
        .app
        .borrow()
        .as_ref()
        .unwrap()
        .runtime
        .output_revision();
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        a.connections_command(APPLY_OUTPUTS, BN_CLICKED as u16)?;
    }
    settle(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        if a.runtime.output_revision() != original_revision + 1
            || a.runtime.outputs_ready() != Ok(true)
            || a.native_lighting.is_some()
            || a.dirty
        {
            return Err(
                "Connections GUI did not apply injected software outputs independently".into(),
            );
        }
    }
    // Exercise retry/cancel controls on a real failed replacement of our own journal.
    {
        use std::os::windows::fs::OpenOptionsExt;
        let blocker = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(root.join("runtime.json"))
            .map_err(|e| e.to_string())?;
        let revision = state
            .app
            .borrow()
            .as_ref()
            .unwrap()
            .runtime
            .output_revision();
        {
            let mut slot = state.app.borrow_mut();
            let a = slot.as_mut().unwrap();
            a.connections_command(APPLY_OUTPUTS, BN_CLICKED as u16)?;
        }
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let failed = {
                let mut slot = state.app.borrow_mut();
                let a = slot.as_mut().unwrap();
                let _ = a.tick();
                a.runtime.configuration_error().is_some()
            };
            if failed {
                break;
            }
            if Instant::now() >= end {
                return Err("GUI failed journal did not reach retry/cancel state".into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        {
            let mut slot = state.app.borrow_mut();
            let a = slot.as_mut().unwrap();
            let _ = a.tick();
            if a.runtime.output_revision() != revision
                || unsafe { IsWindowEnabled(a.h(APPLY_OUTPUTS)) } == 0
                || a.text(APPLY_OUTPUTS) != "Повторить"
            {
                return Err("GUI failed journal lost its retry action or changed outputs".into());
            }
            a.connections_command(RESET_BINDING, BN_CLICKED as u16)?;
        }
        drop(blocker);
        settle(state)?;
        if state
            .app
            .borrow()
            .as_ref()
            .unwrap()
            .runtime
            .output_revision()
            != revision
        {
            return Err("GUI cancellation committed the failed proposal".into());
        }
    }
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        // A real worker open failure must keep explicit reversion possible.
        a.connections.activation = Some(Activation {
            native: false,
            factory: std::sync::Arc::new(|_, doc, root| {
                let mut plan = super::super::super::stand::prepare(
                    doc,
                    root,
                    &super::super::super::stand::Feed::default(),
                )?;
                plan.rgb[0].2 =
                    std::sync::Arc::new(|_| Err("Injected GUI output open failure".into()));
                Ok(plan)
            }),
        });
        a.connections_command(APPLY_OUTPUTS, BN_CLICKED as u16)?;
    }
    settle(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        if a.runtime.outputs_ready().is_ok() || !a.status.contains("Сбой подключения")
        {
            return Err("GUI concealed the injected open failure".into());
        }
        a.connections_command(REVERT_OUTPUTS, BN_CLICKED as u16)?;
    }
    settle(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        if a.runtime.outputs_ready() != Ok(true)
            || a.connections.previous.is_some()
            || std::fs::read(&file).map_err(|e| e.to_string())? != before
        {
            return Err(
                "GUI reversion did not restore outputs or changed the saved binding".into(),
            );
        }
        a.connections.activation = Some(Activation {
            native: false,
            factory: std::sync::Arc::new(|_, doc, root| {
                super::super::super::stand::prepare(
                    doc,
                    root,
                    &super::super::super::stand::Feed::default(),
                )
            }),
        });
        a.set_text(ENDPOINT, "invalid HID path");
        a.connections_command(ENDPOINT, EN_CHANGE as u16)?;
        if a.connections_command(SAVE_BINDING, BN_CLICKED as u16)
            .is_ok()
            || std::fs::read(&file).map_err(|e| e.to_string())? != before
        {
            return Err("Invalid UI binding changed saved data".into());
        }
        a.connections_command(RESET_BINDING, BN_CLICKED as u16)?;
        let mut external = before.clone();
        external.push(b' ');
        std::fs::write(&file, &external).map_err(|e| e.to_string())?;
        a.set_text(
            ENDPOINT,
            r"\\?\hid#vid_048d&pid_5702#alternate#{4d1e55b2-f16f-11cf-88cb-001111000030}",
        );
        a.connections_command(ENDPOINT, EN_CHANGE as u16)?;
        if a.connections_command(SAVE_BINDING, BN_CLICKED as u16)
            .is_ok()
            || std::fs::read(&file).map_err(|e| e.to_string())? != external
        {
            return Err("UI overwrote an external binding edit".into());
        }
        a.connections_command(RESET_BINDING, BN_CLICKED as u16)?;
        a.connections_command(REMOVE, BN_CLICKED as u16)?;
        a.connections_command(SAVE_BINDING, BN_CLICKED as u16)?;
        if !a.text(ENDPOINT).is_empty()
            || !crate::app::lighting_outputs::Local::read_draft(&file, a.runtime.document())?
                .devices
                .is_empty()
        {
            return Err("Removed binding remained in the UI or file".into());
        }
        let revision = a.runtime.output_revision();
        if a.connections_command(APPLY_OUTPUTS, BN_CLICKED as u16)
            .is_ok()
            || a.runtime.output_revision() != revision
            || a.runtime.pending_configuration()
            || a.runtime.outputs_ready() != Ok(true)
        {
            return Err("Incomplete binding apply changed active outputs".into());
        }
        a.connections.scan.cancel();
        let journal = journal_path(&a.state_dir, a.path.as_deref(), &original);
        a.runtime.request_document_at(original.clone(), journal)?;
    }
    settle(state)?;
    {
        let mut slot = state.app.borrow_mut();
        let a = slot.as_mut().unwrap();
        if a.dirty {
            return Err("Private binding edits marked the portable profile dirty".into());
        }
        a.dirty = original_dirty;
        a.connections = old_editor;
        a.page = page;
        a.reset_editor(0);
        a.layout(a.width, a.height);
        if page == Page::Connections {
            a.open_connections()?;
        }
        a.show_page();
        if a.runtime.document() != &original {
            return Err("Connections stand changed the portable document".into());
        }
    }
    eprintln!(
        "Connections stand PASS: metadata selection, save, apply, journal failure/cancel, injected open failure, explicit reversion, invalid input, external edit and removal exercised through owned controls; only software providers"
    );
    Ok(())
}
