//! Portable lighting document. Physical/provider bindings remain separate.
use super::{ambient, assets::AssetId, config::Project, lcd, sensors};
use crate::{devices::Role, storage};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs::File, io::Read, path::Path};
pub const MAX_BYTES: usize = 4 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lcd {
    pub device: String,
    pub config: lcd::Config,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub version: u32,
    pub lighting: Project,
    pub ambient: ambient::Config,
    pub lcd: Vec<Lcd>,
    pub temperatures: sensors::Config,
}
impl Document {
    pub fn from_lighting(lighting: Project) -> Result<Self, String> {
        let value = Self {
            version: 1,
            lighting,
            ambient: ambient::Config::default(),
            lcd: vec![],
            temperatures: sensors::Config::default(),
        };
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("Unsupported lighting document version".into());
        }
        self.lighting.validate()?;
        self.ambient.validate(&self.lighting.setup)?;
        self.temperatures.validate()?;
        if self.lcd.len() > crate::devices::MAX_DEVICES {
            return Err("LCD document limit exceeded".into());
        }
        let mut devices = BTreeSet::new();
        for lcd in &self.lcd {
            if !devices.insert(&lcd.device)
                || !self
                    .lighting
                    .setup
                    .devices
                    .iter()
                    .any(|d| d.id == lcd.device && d.role == Role::Lcd)
            {
                return Err("Duplicate or non-LCD document device".into());
            }
            lcd.config.validate()?;
            for source in &lcd.config.sources {
                if !self
                    .temperatures
                    .sources
                    .iter()
                    .any(|s| s.key == source.key)
                {
                    return Err("LCD overlay references an unknown logical temperature".into());
                }
            }
        }
        for d in self
            .lighting
            .setup
            .devices
            .iter()
            .filter(|d| d.enabled && d.role == Role::Lcd)
        {
            if !self
                .lcd
                .iter()
                .any(|l| l.device == d.id && l.config.enabled)
            {
                return Err("Enabled LCD has no enabled media configuration".into());
            }
        }
        Ok(())
    }
    /// Unique content references only. A profile carries neither source paths nor asset bytes.
    pub fn assets(&self) -> Vec<&AssetId> {
        let mut result = Vec::new();
        let mut used = BTreeSet::new();
        for lcd in &self.lcd {
            for id in [&lcd.config.media, &lcd.config.font].into_iter().flatten() {
                if used.insert(id.as_str()) {
                    result.push(id);
                }
            }
        }
        result
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_BYTES {
            return Err("Lighting document exceeds 4 MiB".into());
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err("Lighting document exceeds 4 MiB".into());
        }
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        let value: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        value.validate()?;
        Ok(value)
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        storage::atomic_write(path, &self.encode()?, MAX_BYTES).map_err(|e| e.to_string())
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() || meta.len() > MAX_BYTES as u64 {
            return Err("Lighting document must be a bounded regular file".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        Self::decode(&bytes)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::config::Groups,
        devices::{Canvas, Device, Driver, Setup},
        schedule,
    };
    fn document() -> Document {
        let setup = Setup {
            version: 1,
            devices: vec![Device {
                id: "lcd".into(),
                label: "LCD".into(),
                role: Role::Lcd,
                driver: Driver::TtLcd,
                leds: 0,
                rate_hz: 25,
                enabled: false,
            }],
            canvas: Canvas {
                width: 1,
                height: 1,
                points: vec![],
            },
        };
        Document::from_lighting(Project {
            version: 1,
            setup,
            scene: crate::runtime::scene::example(),
            schedule: schedule::Config::default(),
            groups: Groups {
                pc: vec!["lcd".into()],
                door: vec![],
            },
        })
        .unwrap()
    }
    #[test]
    fn document_is_strict_and_enabled_devices_require_real_configuration() {
        let d = document();
        let bytes = d.encode().unwrap();
        assert_eq!(Document::decode(&bytes).unwrap(), d);
        let mut bad = d.clone();
        bad.lighting.setup.devices[0].enabled = true;
        assert!(bad.validate().is_err());
        bad = d.clone();
        bad.lcd.push(Lcd {
            device: "lcd".into(),
            config: lcd::Config {
                enabled: true,
                ..Default::default()
            },
        });
        assert!(bad.validate().is_err());
        let mut value = serde_json::to_value(&d).unwrap();
        value["local_bindings"] = serde_json::json!({"path":"private"});
        assert!(serde_json::from_value::<Document>(value).is_err());
    }
    #[test]
    fn assets_are_deduplicated_and_sensor_references_checked() {
        let mut d = document();
        let id = AssetId::try_from("a".repeat(64)).unwrap();
        let mut c = lcd::Config {
            media: Some(id.clone()),
            font: Some(id),
            ..Default::default()
        };
        d.lcd.push(Lcd {
            device: "lcd".into(),
            config: c.clone(),
        });
        assert_eq!(d.assets().len(), 1);
        d.validate().unwrap();
        c.sources.push(lcd::OverlaySource {
            key: "cpu".into(),
            label: "CPU".into(),
        });
        d.lcd[0].config = c;
        assert!(d.validate().is_err());
        d.temperatures.sources.push(sensors::Source {
            key: "cpu".into(),
            value: sensors::Value::Sensor {
                input: "package".into(),
            },
        });
        d.validate().unwrap();
        d.lcd.push(d.lcd[0].clone());
        assert!(d.validate().is_err());
    }
}
