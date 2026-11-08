//! Portable scene, device map and schedule document, with no local bindings.
//! This version does not export LCD/capture/cooling profiles or runtime state.
use crate::{devices, runtime::scene::Scene, schedule, storage};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs::File, io::Read, path::Path};

pub const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Groups {
    pub pc: Vec<String>,
    pub door: Vec<String>,
}
impl Groups {
    pub fn target_ids(&self, target: schedule::Target) -> &[String] {
        match target {
            schedule::Target::Pc => &self.pc,
            schedule::Target::Door => &self.door,
        }
    }
    fn validate(&self, setup: &devices::Setup) -> Result<(), String> {
        if self.pc.len().saturating_add(self.door.len()) > devices::MAX_DEVICES {
            return Err("Project groups exceed 256 devices".into());
        }
        let expected: BTreeSet<&str> = setup
            .devices
            .iter()
            .map(|device| device.id.as_str())
            .collect();
        let mut assigned = BTreeSet::new();
        for id in self.pc.iter().chain(&self.door) {
            if !expected.contains(id.as_str()) || !assigned.insert(id.as_str()) {
                return Err("Project groups contain an unknown or duplicate device".into());
            }
        }
        if assigned != expected {
            return Err(
                "Project groups must cover every device, including disabled and LCD devices".into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub version: u32,
    pub setup: devices::Setup,
    pub scene: Scene,
    pub schedule: schedule::Config,
    pub groups: Groups,
}
impl Project {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("Unsupported project version".into());
        }
        self.setup.validate()?;
        self.scene.validate()?;
        schedule::Schedule::new(self.schedule.clone())?;
        self.groups.validate(&self.setup)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err("Project JSON exceeds 4 MiB".into());
        }
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        let project: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        project.validate()?;
        Ok(project)
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_BYTES {
            return Err("Project JSON exceeds 4 MiB".into());
        }
        Ok(bytes)
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|error| error.to_string())?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES as u64 {
            return Err("Project must be a regular file of at most 4 MiB".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        Self::decode(&bytes)
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        storage::atomic_write(path, &self.encode()?, MAX_BYTES).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::{Canvas, Device, Driver, Point, Role};
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    fn project() -> Project {
        let devices = vec![
            Device {
                id: "strip".into(),
                label: "Strip".into(),
                role: Role::Canvas,
                driver: Driver::SkydimoSerial {
                    model: "SK0801".into(),
                },
                leds: 2,
                rate_hz: 25,
                enabled: true,
            },
            Device {
                id: "door".into(),
                label: "Door".into(),
                role: Role::Canvas,
                driver: Driver::DoorBle,
                leds: 20,
                rate_hz: 25,
                enabled: false,
            },
            Device {
                id: "lcd".into(),
                label: "LCD".into(),
                role: Role::Lcd,
                driver: Driver::TtLcd,
                leds: 0,
                rate_hz: 25,
                enabled: false,
            },
        ];
        let points = devices
            .iter()
            .filter(|device| device.role == Role::Canvas)
            .flat_map(|device| {
                (0..device.leds).map(move |led| Point {
                    device: device.id.clone(),
                    led,
                    x: led as u32,
                    y: 0,
                })
            })
            .collect();
        Project {
            version: 1,
            setup: devices::Setup {
                version: 1,
                devices,
                canvas: Canvas {
                    width: 20,
                    height: 1,
                    points,
                },
            },
            scene: crate::runtime::scene::example(),
            schedule: schedule::Config::default(),
            groups: Groups {
                pc: vec!["strip".into(), "lcd".into()],
                door: vec!["door".into()],
            },
        }
    }
    #[test]
    fn roundtrip_bom_and_validation_leave_original_document_unchanged() {
        let project = project();
        let before = project.clone();
        project.validate().unwrap();
        let bytes = project.encode().unwrap();
        let original_bytes = bytes.clone();
        assert_eq!(Project::decode(&bytes).unwrap(), project);
        let mut bom = b"\xef\xbb\xbf".to_vec();
        bom.extend_from_slice(&bytes);
        assert_eq!(Project::decode(&bom).unwrap(), project);
        assert_eq!(project, before);
        assert_eq!(bytes, original_bytes);
        assert_eq!(
            project.groups.target_ids(schedule::Target::Pc),
            &["strip".to_owned(), "lcd".to_owned()]
        );
        assert_eq!(
            project.groups.target_ids(schedule::Target::Door),
            &["door".to_owned()]
        );
    }
    #[test]
    fn groups_cover_all_disabled_and_lcd_devices_exactly_once() {
        for case in 0..7 {
            let mut project = project();
            match case {
                0 => {
                    project.groups.pc.pop();
                } // Disabled LCD cannot disappear.
                1 => project.groups.door.clear(), // Disabled RGB cannot disappear.
                2 => project.groups.pc.push("strip".into()),
                3 => project.groups.door.push("strip".into()),
                4 => project.groups.pc[0] = "unknown".into(),
                5 => project.groups.door = vec!["door".into(); 257],
                _ => project.groups.pc[0] = String::new(),
            }
            assert!(project.validate().is_err(), "case {case}");
        }
        let mut project = project();
        project.groups.pc.append(&mut project.groups.door);
        project.validate().unwrap();
        project.setup.devices.clear();
        project.setup.canvas.points.clear();
        project.groups.pc.clear();
        project.validate().unwrap();
    }
    #[test]
    fn nested_validation_versions_and_byte_bounds_are_enforced() {
        for case in 0..6 {
            let mut project = project();
            match case {
                0 => project.version = 2,
                1 => {
                    project.setup.canvas.points.pop();
                }
                2 => project.scene.brightness = 101,
                3 => project.schedule.timezone = "Unknown/Zone".into(),
                4 => project.schedule.times = [600, 600, 120],
                _ => project.schedule.times[0] = 1440,
            }
            assert!(project.validate().is_err(), "case {case}");
            assert!(project.encode().is_err());
        }
        assert!(Project::decode(&vec![b' '; MAX_BYTES + 1]).is_err());
        assert!(Project::decode(b"\xef\xbb\xbf{}").is_err());
    }
    #[test]
    fn private_and_future_unknown_fields_are_rejected_instead_of_silently_dropped() {
        for path in [
            vec!["bindings"],
            vec!["physical_address"],
            vec!["lcd_profile"],
            vec!["capture"],
            vec!["cooling"],
            vec!["setup", "bindings"],
            vec!["scene", "device_path"],
            vec!["schedule", "hardware"],
            vec!["groups", "local_paths"],
        ] {
            let mut value = serde_json::to_value(project()).unwrap();
            let mut target = &mut value;
            for key in &path[..path.len() - 1] {
                target = &mut target[*key];
            }
            target[path[path.len() - 1]] = serde_json::json!("PRIVATE_BINDING_MARKER");
            assert!(
                Project::decode(&serde_json::to_vec(&value).unwrap()).is_err(),
                "{path:?}"
            );
        }
        for field in ["port", "instance_id", "serial", "address", "path"] {
            let mut value = serde_json::to_value(project()).unwrap();
            value["setup"]["devices"][0][field] = serde_json::json!("PRIVATE_BINDING_MARKER");
            assert!(Project::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        let encoded = String::from_utf8(project().encode().unwrap()).unwrap();
        for field in ["bindings", "instance_id", "physical_address", "device_path"] {
            assert!(!encoded.contains(field));
        }
    }
    struct Temporary(PathBuf);
    impl Temporary {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("work/app-config-stand")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Temporary {
        fn drop(&mut self) {
            let _ = fs::remove_file(self.0.join("project.json"));
            let _ = fs::remove_dir(&self.0);
        }
    }
    #[test]
    fn atomic_reload_failed_validation_and_failed_replace_preserve_previous_good_file() {
        let directory = Temporary::new();
        let path = directory.0.join("project.json");
        let first = project();
        first.save(&path).unwrap();
        assert_eq!(Project::load(&path).unwrap(), first);
        let mut second = first.clone();
        second.scene.brightness = 31;
        second.save(&path).unwrap();
        assert_eq!(Project::load(&path).unwrap(), second);
        let old = fs::read(&path).unwrap();
        let mut invalid = second.clone();
        invalid.groups.pc.clear();
        assert!(invalid.save(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), old);
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
        assert!(
            second
                .save(&directory.0.join("absent/project.json"))
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), old);
        assert!(Project::load(&directory.0).is_err());
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let held = fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&path)
                .unwrap();
            assert!(first.save(&path).is_err());
            drop(held);
            assert_eq!(fs::read(&path).unwrap(), old);
            assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
        }
    }
}
