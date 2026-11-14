//! Named native scenes. This catalog never contains bindings or credentials.
use crate::{lighting::scene::Effect, runtime::scene::Scene, storage};
use serde::{Deserialize, Serialize};
use std::{fs::File, io::Read, path::Path};

const MAX_BYTES: usize = 256 * 1024;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub scene: Scene,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    version: u32,
    pub entries: Vec<Entry>,
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name == name.trim()
        && name.chars().count() <= 128
        && !name.chars().any(char::is_control)
}
impl Catalog {
    pub fn seeded(current: &Scene) -> Self {
        let mut entries = vec![Entry {
            id: "current".into(),
            name: "Моя сцена".into(),
            scene: current.clone(),
        }];
        for (id, name, effect) in [
            ("noise", "Мягкий свет", Effect::NoiseMap),
            ("breathing", "Дыхание", Effect::Breathing),
            ("rainbow", "Радуга", Effect::RainbowWave),
        ] {
            let mut scene = current.clone();
            scene.effect = effect;
            let (lo, hi) = effect.limits();
            scene.speed = scene.speed.clamp(lo, hi);
            entries.push(Entry {
                id: id.into(),
                name: name.into(),
                scene,
            });
        }
        Self {
            version: 1,
            entries,
        }
    }
    fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.entries.len() > 100 {
            return Err("Некорректная библиотека сцен".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for entry in &self.entries {
            if !valid_name(&entry.name)
                || entry.id.is_empty()
                || entry.id.len() > 80
                || !entry
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                || !ids.insert(&entry.id)
            {
                return Err("Некорректное имя или идентификатор сцены".into());
            }
            entry.scene.validate()?;
        }
        Ok(())
    }
    pub fn load(path: &Path, current: &Scene) -> Result<Self, String> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::seeded(current)),
            Err(e) => return Err(e.to_string()),
        };
        if file.metadata().map_err(|e| e.to_string())?.len() > MAX_BYTES as u64 {
            return Err("Библиотека сцен слишком большая".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let catalog: Self = serde_json::from_slice(&bytes)
            .map_err(|e| format!("Не удалось прочитать библиотеку сцен: {e}"))?;
        catalog.validate()?;
        Ok(catalog)
    }
    pub fn save_entry(
        &mut self,
        path: &Path,
        id: Option<&str>,
        name: String,
        scene: Scene,
    ) -> Result<String, String> {
        if !valid_name(&name) {
            return Err("Введите название от 1 до 128 символов без управляющих знаков".into());
        }
        scene.validate()?;
        let mut next = self.clone();
        let id = if let Some(id) = id {
            let entry = next
                .entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or("Сцена не найдена")?;
            entry.name = name;
            entry.scene = scene;
            id.to_owned()
        } else {
            if next.entries.len() >= 100 {
                return Err("В библиотеке может быть до 100 сцен".into());
            }
            let mut n = 1;
            while next.entries.iter().any(|e| e.id == format!("scene-{n}")) {
                n += 1;
            }
            let id = format!("scene-{n}");
            next.entries.push(Entry {
                id: id.clone(),
                name,
                scene,
            });
            id
        };
        next.persist(path)?;
        *self = next;
        Ok(id)
    }
    pub fn remove(&mut self, path: &Path, id: &str) -> Result<(), String> {
        let mut next = self.clone();
        let index = next
            .entries
            .iter()
            .position(|e| e.id == id)
            .ok_or("Сцена не найдена")?;
        next.entries.remove(index);
        next.persist(path)?;
        *self = next;
        Ok(())
    }
    fn persist(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        storage::atomic_write(path, &bytes, MAX_BYTES).map_err(|e| e.to_string())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_save_keeps_memory_and_native_values() {
        let scene = crate::runtime::scene::example();
        let mut catalog = Catalog::seeded(&scene);
        assert_eq!(catalog.entries[0].scene, scene);
        let original = serde_json::to_string(&catalog).unwrap();
        assert!(
            catalog
                .save_entry(Path::new(""), None, "bad\tname".into(), scene.clone())
                .is_err()
        );
        assert!(
            catalog
                .save_entry(Path::new(""), None, "Valid".into(), scene)
                .is_err()
        );
        assert_eq!(serde_json::to_string(&catalog).unwrap(), original);
    }
}
