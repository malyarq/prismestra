//! Explicit local lighting activation. Loading a portable profile never opens hardware.
use super::{ambient_windows, assets, document::Document, lcd, runtime::*};
use crate::devices::{self, Binding, Bindings, Device, Role};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Private machine bindings; deliberately separate from the portable document.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Local {
    pub version: u8,
    pub devices: Bindings,
    pub monitors: ambient_windows::Bindings,
    pub door_brightness: i32,
}
impl Local {
    pub fn read(path: &Path, document: &Document) -> Result<Self, String> {
        let local = Self::read_draft(path, document)?;
        local.validate(document)?;
        Ok(local)
    }
    pub fn read_draft(path: &Path, document: &Document) -> Result<Self, String> {
        Ok(Self::read_draft_bytes(path, document)?.0)
    }
    pub(crate) fn read_draft_bytes(
        path: &Path,
        document: &Document,
    ) -> Result<(Self, Vec<u8>), String> {
        let absolute = std::path::absolute(path).map_err(|e| e.to_string())?;
        use std::path::{Component, Prefix};
        if !matches!(absolute.components().next(),Some(Component::Prefix(p)) if matches!(p.kind(),Prefix::Disk(_)|Prefix::VerbatimDisk(_)))
        {
            return Err("Lighting bindings require a local disk path".into());
        }
        let path = absolute.as_path();
        let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        use std::os::windows::fs::MetadataExt;
        if !metadata.is_file()
            || metadata.file_attributes() & 0x400 != 0
            || metadata.len() > devices::MAX_JSON_BYTES as u64
        {
            return Err("Lighting bindings must be a bounded regular local file".into());
        }
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|e| e.to_string())?
            .take(devices::MAX_JSON_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let value = super::strict_json::unique_json(&bytes, devices::MAX_JSON_BYTES)?;
        let local: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        local.validate_draft(document)?;
        Ok((local, bytes))
    }
    /// Authoring permits missing/orphan bindings but never activates them.
    /// Full validate remains mandatory at the native activation boundary.
    pub fn validate_draft(&self, document: &Document) -> Result<(), String> {
        document.validate()?;
        if self.version != 1
            || !(1..=100).contains(&self.door_brightness)
            || self.devices.len() > devices::MAX_DEVICES
            || self.monitors.monitors.len() > 16
        {
            return Err("Invalid local lighting draft bounds".into());
        }
        for (id, binding) in &self.devices {
            if id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
                || serde_json::to_vec(binding)
                    .map_err(|e| e.to_string())?
                    .len()
                    > 16 * 1024
            {
                return Err("Invalid local binding identity or size".into());
            }
        }
        // Drafts may retain a previous device family's binding for explicit
        // repair. Only full validate can ever grant activation preflight.
        let mut seen = std::collections::BTreeSet::new();
        for monitor in &self.monitors.monitors {
            if !super::ambient::key(&monitor.source) || !seen.insert(&monitor.source) {
                return Err("Invalid/duplicate monitor draft source".into());
            }
            crate::capture::validate_name(&monitor.device_name)?;
        }
        let monitors = ambient_windows::Bindings {
            monitors: self
                .monitors
                .monitors
                .iter()
                .filter(|m| document.ambient.sources.iter().any(|s| s.key == m.source))
                .cloned()
                .collect(),
        };
        monitors.validate_partial(&document.ambient)
    }
    pub fn validate(&self, document: &Document) -> Result<(), String> {
        self.validate_draft(document)?;
        document.validate()?;
        if self.version != 1 || !(1..=100).contains(&self.door_brightness) {
            return Err("Lighting bindings version/brightness rejected".into());
        }
        devices::validate_active_bindings(&document.lighting.setup, &self.devices)?;
        self.monitors.validate(&document.ambient)
    }
    /// Native constructors remain deferred until Runtime starts the owning thread.
    /// No software fallback: missing/mismatched bindings fail the whole preflight.
    pub fn prepare(&self, document: &Document, asset_root: &Path) -> Result<OutputPlan, String> {
        prepare_with(
            self,
            document,
            asset_root,
            native_rgb,
            native_lcd,
            native_capture,
        )
    }
    pub fn attach_missing(&self, runtime: &mut Runtime, asset_root: &Path) -> Result<(), String> {
        attach_with(
            self,
            runtime,
            asset_root,
            native_rgb,
            native_lcd,
            native_capture,
        )
    }
}

fn native_rgb(local: &Local, document: &Document, device: &Device) -> Result<RgbFactory, String> {
    Ok(crate::runtime::native::bound_factory(
        &document.lighting.setup,
        &device.id,
        &local.devices,
        local.door_brightness,
    )?
    .1)
}
fn native_lcd(binding: &Binding, root: PathBuf) -> Result<LcdFactory, String> {
    let Binding::Hid { path } = binding else {
        return Err("LCD requires an exact HID path".into());
    };
    let path = path.clone();
    Ok(Arc::new(move |config, cancel| {
        if cancel.cancelled() {
            return Err("LCD activation cancelled".into());
        }
        let store = assets::Store::new(root.clone())?;
        let controller = lcd::Controller::new(config, &store)?;
        let display = crate::lcd::hid_adapter::open_display(
            &mut crate::lcd::hid_windows::NativeBackend,
            &path,
        )?;
        if cancel.cancelled() {
            return Err("LCD activation cancelled".into());
        }
        Ok((controller, Box::new(display) as LcdTransport))
    }))
}
fn native_capture(local: &Local, document: &Document) -> Result<CaptureFactory, String> {
    let bindings = local.monitors.clone();
    let config = document.ambient.clone();
    Ok(Arc::new(move |cancel| {
        if cancel.cancelled() {
            return Err("Capture activation cancelled".into());
        }
        Ok(Box::new(ambient_windows::Native::new(&bindings, &config)?)
            as Box<dyn super::ambient::Provider>)
    }))
}

fn attach_with(
    local: &Local,
    runtime: &mut Runtime,
    asset_root: &Path,
    rgb: impl FnMut(&Local, &Document, &Device) -> Result<RgbFactory, String>,
    lcd: impl FnMut(&Binding, PathBuf) -> Result<LcdFactory, String>,
    capture: impl FnMut(&Local, &Document) -> Result<CaptureFactory, String>,
) -> Result<(), String> {
    let plan = prepare_with(local, runtime.document(), asset_root, rgb, lcd, capture)?;
    for (id, keys, factory) in plan.rgb {
        if runtime.host().status(&id).is_none() {
            runtime.attach_rgb(&id, keys, factory)?;
        }
    }
    for (id, keys, factory) in plan.lcd {
        if runtime.lcd_status(&id).is_none() {
            runtime.attach_lcd(&id, keys, factory)?;
        }
    }
    if runtime.capture_status().is_none()
        && let Some(factory) = plan.capture
    {
        runtime.attach_capture(factory)?;
    }
    Ok(())
}
fn prepare_with(
    local: &Local,
    document: &Document,
    asset_root: &Path,
    mut rgb: impl FnMut(&Local, &Document, &Device) -> Result<RgbFactory, String>,
    mut lcd: impl FnMut(&Binding, PathBuf) -> Result<LcdFactory, String>,
    mut capture: impl FnMut(&Local, &Document) -> Result<CaptureFactory, String>,
) -> Result<OutputPlan, String> {
    local.validate(document)?;
    if !asset_root.is_absolute() {
        return Err("Lighting asset root must be absolute".into());
    }
    let mut plan = OutputPlan::default();
    for device in document.lighting.setup.devices.iter().filter(|d| d.enabled) {
        let binding = &local.devices[&device.id];
        if device.role == Role::Lcd {
            if document
                .lcd
                .iter()
                .any(|l| l.device == device.id && l.config.enabled)
            {
                plan.lcd.push((
                    device.id.clone(),
                    binding.endpoint_keys(),
                    lcd(binding, asset_root.to_owned())?,
                ));
            }
        } else {
            plan.rgb.push((
                device.id.clone(),
                binding.endpoint_keys(),
                rgb(local, document, device)?,
            ));
        }
    }
    if document.ambient.routes.iter().any(|r| r.enabled) {
        plan.capture = Some(capture(local, document)?);
    }
    plan.validate(document)?;
    Ok(plan)
}

#[cfg(test)]
mod tests;
