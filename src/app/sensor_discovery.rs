//! Bounded, metadata-only classification. No enumeration, module reads or sensor I/O.
use super::{
    sensor_pci::Pci,
    sensor_bindings::{Source, Target},
    sensor_form::{Draft, Family},
};
use crate::{
    lcd::hid_policy::{self, Descriptor, Kind},
    runtime::output::Cancellation,
    sensors::{amd, ddr5, nvidia},
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    thread::JoinHandle,
};

pub const MAX_FAMILY: usize = 64;
pub const MAX_TOTAL: usize = 128;
const DISPLAY_BYTES: usize = 512;
const NOTICE: &str = "Метаданные не подтверждают протокол датчика или физическую совместимость.";
pub struct Metadata {
    pub nvidia: Result<Vec<nvidia::PciIdentity>, String>,
    pub cpu: Result<amd::CpuIdentity, String>,
    pub ddr5: Result<Vec<ddr5::Binding>, String>,
    pub liquid: Result<Vec<Descriptor>, String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub label: String,
    pub draft: Option<Draft>,
    pub detail: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    pub candidates: Vec<Candidate>,
    pub issues: Vec<String>,
}
fn display(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        let c = if c.is_control() { ' ' } else { c };
        if out.len() + c.len_utf8() > DISPLAY_BYTES {
            break;
        }
        out.push(c);
    }
    out
}
fn key(target: &Target) -> String {
    // Generated IDs are persisted: use an explicit numeric format, not Debug output.
    match target {
        Target::Nvidia { pci } => format!(
            "nvidia:{:08x}:{:08x}:{:08x}:{:08x}",
            pci.device_id, pci.subsystem_id, pci.revision_id, pci.extended_device_id
        ),
        Target::Amd {
            vendor,
            signature,
            packages,
            ..
        } => {
            let vendor = vendor
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            format!("amd:{vendor}:{signature:08x}:{packages}")
        }
        Target::Ddr5 {
            controller,
            port,
            address,
            ..
        } => format!(
            "ddr5:{:016x}:{:016x}:{:016x}:{port}:{address:02x}",
            controller[0], controller[1], controller[2]
        ),
        Target::Liquid { explicit_mi00_path } => {
            format!("liquid:{}", explicit_mi00_path.to_ascii_lowercase())
        }
    }
}
fn id(key: &str) -> String {
    let digest: String = Sha256::digest(key.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sensor-{digest}")
}
fn draft(target: Target, id: String) -> Draft {
    Draft::from_source(&Source {
        id,
        revision: 1,
        target,
    })
}
impl Inventory {
    /// Reject malformed factory output before it reaches the UI, including altered drafts.
    pub fn validate(&self) -> Result<(), String> {
        if self.candidates.len() > MAX_TOTAL || self.issues.len() > 4 {
            return Err("Sensor inventory count limit exceeded".into());
        }
        let text =
            |s: &str| !s.is_empty() && s.len() <= DISPLAY_BYTES && !s.chars().any(char::is_control);
        if self.issues.iter().any(|s| !text(s)) {
            return Err("Invalid inventory issue".into());
        }
        let mut ids = BTreeSet::new();
        let mut families = [0_usize; 4];
        for c in &self.candidates {
            if c.id.len() != 71
                || !c.id.starts_with("sensor-")
                || !c.id[7..]
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                || !ids.insert(&c.id)
                || !text(&c.label)
                || !text(&c.detail)
            {
                return Err("Invalid inventory candidate identity or display text".into());
            }
            if let Some(d) = &c.draft {
                if d.id != c.id {
                    return Err("Candidate and draft IDs differ".into());
                }
                if d.values
                    .iter()
                    .any(|v| v.len() > 4096 || v.chars().any(char::is_control))
                {
                    return Err("Unbounded or invalid inventory draft field".into());
                }
                let family = match d.family {
                    Family::Nvidia => 0,
                    Family::Amd => 1,
                    Family::Ddr5 => 2,
                    Family::Liquid => 3,
                };
                families[family] += 1;
                if families[family] > MAX_FAMILY {
                    return Err("Inventory family count limit exceeded".into());
                }
                let target = match d.family {
                    Family::Amd => {
                        let vendor: [u8; 12] = d.values[0]
                            .as_bytes()
                            .try_into()
                            .map_err(|_| "Invalid CPU vendor")?;
                        let signature = u32::from_str_radix(&d.values[1], 16)
                            .map_err(|_| "Invalid CPU signature")?;
                        let packages = d.values[2]
                            .parse()
                            .map_err(|_| "Invalid CPU package count")?;
                        amd::CpuIdentity {
                            vendor,
                            signature,
                            packages,
                        }
                        .validate()?;
                        Target::Amd {
                            vendor,
                            signature,
                            packages,
                            module_path: String::new(),
                        }
                    }
                    Family::Ddr5 => {
                        let mut controller = [0; 3];
                        for (word, value) in controller.iter_mut().zip(&d.values) {
                            *word = u64::from_str_radix(value, 16)
                                .map_err(|_| "Invalid DDR5 controller")?;
                        }
                        let port = d.values[3].parse().map_err(|_| "Invalid DDR5 port")?;
                        let address = u8::from_str_radix(&d.values[4], 16)
                            .map_err(|_| "Invalid DDR5 address")?;
                        ddr5::Binding {
                            controller,
                            port,
                            address,
                        }
                        .validate()?;
                        Target::Ddr5 {
                            controller,
                            port,
                            address,
                            module_path: String::new(),
                        }
                    }
                    _ => d.target()?,
                };
                if id(&key(&target)) != c.id || draft(target, c.id.clone()) != *d {
                    return Err(
                        "Inventory draft is noncanonical or contains an unsolicited module path"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }
}
pub fn collect(metadata: Metadata) -> Result<Inventory, String> {
    let counts = [
        metadata.nvidia.as_ref().map_or(0, Vec::len),
        usize::from(metadata.cpu.is_ok()),
        metadata.ddr5.as_ref().map_or(0, Vec::len),
        metadata.liquid.as_ref().map_or(0, Vec::len),
    ];
    if counts.iter().any(|n| *n > MAX_FAMILY) || counts.iter().sum::<usize>() > MAX_TOTAL {
        return Err("Sensor metadata count limit exceeded".into());
    }
    if metadata.liquid.as_ref().is_ok_and(|ds| {
        ds.iter()
            .any(|d| d.path.len() > 4096 || d.input_ids.len() > 4096 || d.output_ids.len() > 4096)
    }) {
        return Err("HID metadata size limit exceeded".into());
    }
    let mut inventory = Inventory::default();
    let mut grouped: BTreeMap<String, (Candidate, usize)> = BTreeMap::new();
    let mut insert =
        |target: Target, validation: Result<(), String>, label: String, module: bool| {
            let key = key(&target);
            let identity = id(&key);
            let (draft, detail) = match validation {
                Ok(()) => (
                    Some(draft(target, identity.clone())),
                    format!(
                        "{} {NOTICE}",
                        if module {
                            "Нужно явно выбрать проверяемый по pin модуль; путь не задан."
                        } else {
                            "Идентичность поддерживается политикой."
                        }
                    ),
                ),
                Err(error) => (None, format!("Не поддерживается: {error}. {NOTICE}")),
            };
            let entry = grouped.entry(key).or_insert((
                Candidate {
                    id: identity,
                    label: display(&label),
                    draft,
                    detail: display(&detail),
                },
                0,
            ));
            entry.1 += 1;
        };
    let mut successes = 0;
    match metadata.nvidia {
        Ok(values) => {
            successes += 1;
            for p in values {
                insert(
                    Target::Nvidia { pci: Pci::from(p) },
                    p.validate(),
                    format!("NVIDIA {:08x} / {:08x}", p.device_id, p.subsystem_id),
                    false,
                );
            }
        }
        Err(e) => inventory.issues.push(display(&format!("NVIDIA: {e}"))),
    }
    match metadata.cpu {
        Ok(p) => {
            successes += 1;
            insert(
                Target::Amd {
                    vendor: p.vendor,
                    signature: p.signature,
                    packages: p.packages,
                    module_path: String::new(),
                },
                p.validate().map_err(|_| {
                    if p.vendor != *b"AuthenticAMD" {
                        "Для этого производителя CPU чтение температуры пока не реализовано; доступен AMD Tctl семейства 17h–1Ah.".into()
                    } else if p.packages != 1 {
                        format!("Требуется один процессорный сокет; обнаружено {}.",p.packages)
                    } else {
                        format!("AMD family {:X}h не поддерживается; реализован Tctl семейства 17h–1Ah.",p.display_family())
                    }
                }),
                format!(
                    "CPU {} · family {:X}h · сокетов {}",
                    if p.vendor==*b"AuthenticAMD"{"AMD".into()}else if p.vendor==*b"GenuineIntel"{"Intel".into()}else{String::from_utf8_lossy(&p.vendor)},
                    p.display_family(),
                    p.packages
                ),
                true,
            );
        }
        Err(e) => inventory.issues.push(display(&format!("CPU: {e}"))),
    }
    match metadata.ddr5 {
        Ok(values) => {
            successes += 1;
            for b in values {
                insert(
                    Target::Ddr5 {
                        controller: b.controller,
                        port: b.port,
                        address: b.address,
                        module_path: String::new(),
                    },
                    b.validate(),
                    format!(
                        "DDR5 порт {}, SPD {:02x}, {:016x}",
                        b.port, b.address, b.controller[2]
                    ),
                    true,
                );
            }
        }
        Err(e) => inventory.issues.push(display(&format!("DDR5: {e}"))),
    }
    match metadata.liquid {
        Ok(values) => {
            successes += 1;
            for d in &values {
                let valid = hid_policy::validate(Kind::Liquid, d).and_then(|_| {
                    hid_policy::select(Kind::Liquid, &values, Some(&d.path), false).map(|_| ())
                });
                insert(
                    Target::Liquid {
                        explicit_mi00_path: d.path.clone(),
                    },
                    valid,
                    format!("Жидкость {}", d.path),
                    false,
                );
            }
        }
        Err(e) => inventory.issues.push(display(&format!("Жидкость: {e}"))),
    }
    if successes == 0 {
        return Err(display(&inventory.issues.join("; ")));
    }
    for (_, (mut c, count)) in grouped {
        if count > 1 {
            c.draft = None;
            c.detail = display(&format!(
                "Неоднозначная идентичность: {count} записей одного endpoint. Выбор запрещён. {NOTICE}"
            ));
        }
        inventory.candidates.push(c);
    }
    inventory.validate()?;
    Ok(inventory)
}

pub type Factory = Arc<dyn Fn(Cancellation) -> Result<Inventory, String> + Send + Sync>;
#[derive(Debug)]
pub enum Outcome {
    Ready {
        generation: u64,
        inventory: Inventory,
    },
    Failed {
        generation: u64,
        error: String,
    },
    Cancelled {
        generation: u64,
    },
}
struct Task {
    generation: u64,
    cancel: Cancellation,
    thread: JoinHandle<Result<Inventory, String>>,
}
#[derive(Default)]
pub struct Worker {
    generation: u64,
    task: Option<Task>,
}
impl Worker {
    pub fn start(&mut self, factory: Factory) -> Result<u64, String> {
        if self.task.is_some() {
            return Err("A sensor scan is already in flight".into());
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or("Sensor discovery generation exhausted")?;
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("sensor-discovery".into())
            .spawn(move || {
                if token.cancelled() {
                    return Err("Sensor discovery cancelled".into());
                }
                let result = factory(token.clone());
                if token.cancelled() {
                    return Err("Sensor discovery cancelled".into());
                }
                let inventory = result?;
                inventory.validate()?;
                Ok(inventory)
            })
            .map_err(|e| e.to_string())?;
        self.task = Some(Task {
            generation,
            cancel,
            thread,
        });
        self.generation = generation;
        Ok(generation)
    }
    pub fn cancel(&self) {
        if let Some(t) = &self.task {
            t.cancel.cancel();
        }
    }
    pub fn is_idle(&self) -> bool {
        self.task.is_none()
    }
    /// Cancellation is cooperative; keep the close barrier until the actual thread exits.
    pub fn poll(&mut self) -> Option<Outcome> {
        if !self.task.as_ref().is_some_and(|t| t.thread.is_finished()) {
            return None;
        }
        let t = self.task.take().unwrap();
        let result = t.thread.join();
        let generation = t.generation;
        if t.cancel.cancelled() {
            return Some(Outcome::Cancelled { generation });
        }
        Some(match result {
            Ok(Ok(inventory)) => Outcome::Ready {
                generation,
                inventory,
            },
            Ok(Err(error)) => Outcome::Failed {
                generation,
                error: display(&error),
            },
            Err(_) => Outcome::Failed {
                generation,
                error: "Sensor discovery worker panicked".into(),
            },
        })
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
        let _ = self.poll();
    }
}
#[cfg(test)]
mod tests;
