//! Editable private bindings. Saving never opens an output or changes the running Runtime.
use super::{document::Document, lighting_outputs::Local};
use crate::devices::{Binding, Device, Driver};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
pub mod discovery;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub endpoint: String,
    pub identity: String,
    pub serial: String,
}
impl Draft {
    pub fn from_binding(binding: &Binding) -> Self {
        match binding {
            Binding::WledDdp { endpoint } => Self {
                endpoint: endpoint.to_string(),
                ..Self::default()
            },
            Binding::Ble { address } => Self {
                endpoint: format!("{address:012X}"),
                ..Self::default()
            },
            Binding::Hid { path } => Self {
                endpoint: path.clone(),
                ..Self::default()
            },
            Binding::Serial {
                port,
                instance_id,
                serial,
            } => Self {
                endpoint: port.clone(),
                identity: instance_id.clone(),
                serial: serial.clone(),
            },
            Binding::EnePiix4 {
                module_path,
                address,
            } => Self {
                endpoint: module_path.clone(),
                identity: format!("0x{address:02X}"),
                serial: String::new(),
            },
        }
    }
    pub fn binding(&self, device: &Device) -> Result<Binding, String> {
        if [&self.endpoint, &self.identity, &self.serial]
            .iter()
            .any(|s| s.len() > 4096 || s.chars().any(char::is_control))
        {
            return Err(
                "Поле подключения слишком длинное или содержит управляющие символы.".into(),
            );
        }
        let endpoint = self.endpoint.trim().to_owned();
        let binding = match device.driver {
            Driver::WledDdp => Binding::WledDdp {
                endpoint: crate::devices::canonical_endpoint(
                    endpoint
                        .parse()
                        .map_err(|_| "Введите IP:порт DDP, например 192.168.1.20:4048")?,
                ),
            },
            Driver::DoorBle => {
                let mac = endpoint.replace([':', '-'], "");
                if mac.len() != 12 || !mac.bytes().all(|c| c.is_ascii_hexdigit()) {
                    return Err("MAC должен содержать 12 шестнадцатеричных цифр.".into());
                }
                Binding::Ble {
                    address: u64::from_str_radix(&mac, 16).map_err(|e| e.to_string())?,
                }
            }
            Driver::Fusion2 { .. } | Driver::Valkyrie { .. } | Driver::TtLcd => {
                Binding::Hid { path: endpoint }
            }
            Driver::SkydimoSerial { .. } => Binding::Serial {
                port: endpoint.to_ascii_uppercase(),
                instance_id: self.identity.trim().into(),
                serial: self.serial.trim().to_ascii_uppercase(),
            },
            Driver::EneDram => {
                let address = self.identity.trim();
                let address = address
                    .strip_prefix("0x")
                    .or_else(|| address.strip_prefix("0X"))
                    .unwrap_or(address);
                Binding::EnePiix4 {
                    module_path: endpoint,
                    address: u8::from_str_radix(address, 16)
                        .map_err(|_| "Адрес RAM должен быть 0x70..0x76")?,
                }
            }
            Driver::Unsupported { .. } => {
                return Err("Этот протокол пока не поддерживается.".into());
            }
        };
        binding.validate(device)?;
        Ok(binding)
    }
}

pub struct Workspace {
    pub local: Local,
    pub dirty: bool,
    path: PathBuf,
    expected: Option<Vec<u8>>,
}
impl Workspace {
    pub fn open(path: &Path, document: &Document) -> Result<Self, String> {
        let path = std::path::absolute(path).map_err(|e| e.to_string())?;
        use std::path::{Component, Prefix};
        if !matches!(path.components().next(),Some(Component::Prefix(p)) if matches!(p.kind(),Prefix::Disk(_)|Prefix::VerbatimDisk(_)))
        {
            return Err("Привязки должны храниться на локальном диске.".into());
        }
        let (local, expected) = match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                let (local, bytes) = Local::read_draft_bytes(&path, document)?;
                (local, Some(bytes))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
                Local {
                    version: 1,
                    devices: BTreeMap::new(),
                    monitors: Default::default(),
                    door_brightness: 15,
                },
                None,
            ),
            Err(e) => return Err(e.to_string()),
        };
        Ok(Self {
            local,
            dirty: false,
            path,
            expected,
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn assign(
        &mut self,
        document: &Document,
        device: &Device,
        draft: &Draft,
    ) -> Result<(), String> {
        if document
            .lighting
            .setup
            .devices
            .iter()
            .find(|d| d.id == device.id)
            != Some(device)
        {
            return Err(
                "Устройство изменилось после открытия подключения. Откройте его заново.".into(),
            );
        }
        let binding = draft.binding(device)?;
        let keys = binding.endpoint_keys();
        if self.local.devices.iter().any(|(id, b)| {
            id != &device.id && b.endpoint_keys().iter().any(|key| keys.contains(key))
        }) {
            return Err("Этот физический выход уже привязан к другому устройству.".into());
        }
        let mut next = self.local.clone();
        next.devices.insert(device.id.clone(), binding);
        next.validate_draft(document)?;
        self.local = next;
        self.dirty = true;
        Ok(())
    }
    pub fn remove(&mut self, id: &str) {
        self.dirty |= self.local.devices.remove(id).is_some();
    }
    pub fn assign_monitor(
        &mut self,
        document: &Document,
        source: &str,
        name: &str,
    ) -> Result<(), String> {
        if !document.ambient.sources.iter().any(|s| s.key == source) {
            return Err("Логический источник экрана изменился.".into());
        }
        crate::capture::validate_name(name)?;
        let mut next = self.local.clone();
        next.monitors.monitors.retain(|m| m.source != source);
        next.monitors
            .monitors
            .push(super::ambient_windows::Binding {
                source: source.into(),
                device_name: name.into(),
            });
        next.validate_draft(document)?;
        self.local = next;
        self.dirty = true;
        Ok(())
    }
    pub fn remove_monitor(&mut self, source: &str) {
        let n = self.local.monitors.monitors.len();
        self.local.monitors.monitors.retain(|m| m.source != source);
        self.dirty |= n != self.local.monitors.monitors.len();
    }
    pub fn orphan_ids(&self, document: &Document) -> Vec<String> {
        self.local
            .devices
            .keys()
            .filter(|id| !document.lighting.setup.devices.iter().any(|d| &d.id == *id))
            .cloned()
            .collect()
    }
    pub fn prune_orphans(&mut self, document: &Document) {
        let before = self.local.devices.len() + self.local.monitors.monitors.len();
        self.local
            .devices
            .retain(|id, _| document.lighting.setup.devices.iter().any(|d| &d.id == id));
        self.local
            .monitors
            .monitors
            .retain(|m| document.ambient.sources.iter().any(|s| s.key == m.source));
        self.dirty |= before != self.local.devices.len() + self.local.monitors.monitors.len();
    }
    pub fn save(&mut self, document: &Document) -> Result<(), String> {
        self.local.validate_draft(document)?;
        let current = match std::fs::symlink_metadata(&self.path) {
            Ok(_) => Some(Local::read_draft_bytes(&self.path, document)?.1),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.to_string()),
        };
        if current != self.expected {
            return Err("Файл привязок изменён другой программой. Перечитайте его; правки сохранены в форме.".into());
        }
        let bytes = serde_json::to_vec_pretty(&self.local).map_err(|e| e.to_string())?;
        crate::storage::atomic_write(&self.path, &bytes, crate::devices::MAX_JSON_BYTES)
            .map_err(|e| e.to_string())?;
        if Local::read_draft_bytes(&self.path, document)?.1 != bytes {
            return Err("Не удалось подтвердить сохранённые привязки.".into());
        }
        self.expected = Some(bytes);
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
