//! Explicit, UI-neutral descriptor editing. No discovery, file reads or native calls.
use super::{
    sensor_bindings::{Source, Target},
    sensor_pci::Pci,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Nvidia,
    Amd,
    Ddr5,
    Liquid,
}
impl Family {
    pub const ALL: [Self; 4] = [Self::Nvidia, Self::Amd, Self::Ddr5, Self::Liquid];

    pub fn label(self) -> &'static str {
        match self {
            Self::Nvidia => "NVIDIA — ядро GPU",
            Self::Amd => "AMD — Tctl процессора",
            Self::Ddr5 => "DDR5 — температура SPD",
            Self::Liquid => "Жидкость — HID MI00",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub id: String,
    pub family: Family,
    pub values: [String; 6],
}
impl Draft {
    pub fn new(family: Family) -> Self {
        // No detected identities or paths are implied, including for AMD.
        Self {
            id: String::new(),
            family,
            values: std::array::from_fn(|_| String::new()),
        }
    }

    pub fn from_source(source: &Source) -> Self {
        let mut draft = Self::new(match &source.target {
            Target::Nvidia { .. } => Family::Nvidia,
            Target::Amd { .. } => Family::Amd,
            Target::Ddr5 { .. } => Family::Ddr5,
            Target::Liquid { .. } => Family::Liquid,
        });
        draft.id = source.id.clone();
        match &source.target {
            Target::Nvidia { pci } => {
                for (slot, value) in draft.values.iter_mut().zip([
                    pci.device_id,
                    pci.subsystem_id,
                    pci.revision_id,
                    pci.extended_device_id,
                ]) {
                    *slot = format!("{value:08x}");
                }
            }
            Target::Amd {
                vendor,
                signature,
                packages,
                module_path,
            } => {
                // Preserve every byte as a character; non-ASCII descriptors fail target().
                draft.values[0] = vendor.iter().map(|byte| char::from(*byte)).collect();
                draft.values[1] = format!("{signature:08x}");
                draft.values[2] = packages.to_string();
                draft.values[3] = module_path.clone();
            }
            Target::Ddr5 {
                controller,
                port,
                address,
                module_path,
            } => {
                for (slot, value) in draft.values.iter_mut().zip(controller) {
                    *slot = format!("{value:016x}");
                }
                draft.values[3] = port.to_string();
                draft.values[4] = format!("{address:02x}");
                draft.values[5] = module_path.clone();
            }
            Target::Liquid { explicit_mi00_path } => draft.values[0] = explicit_mi00_path.clone(),
        }
        draft
    }

    /// Empty labels indicate unused slots; nonempty unused values are rejected.
    pub fn labels(&self) -> [&'static str; 6] {
        match self.family {
            Family::Nvidia => [
                "PCI device ID (hex32)",
                "PCI subsystem ID (hex32)",
                "PCI revision ID (hex32)",
                "PCI extended ID (hex32)",
                "",
                "",
            ],
            Family::Amd => [
                "CPUID vendor (12 ASCII)",
                "CPUID signature (hex32)",
                "Сокетов CPU (десятичное)",
                "Модуль AMD (абс. путь)",
                "",
                "",
            ],
            Family::Ddr5 => [
                "Контроллер: слово 0 (hex64)",
                "Контроллер: слово 1 (hex64)",
                "Контроллер: слово 2 (hex64)",
                "Порт SMBus (десятичный)",
                "Адрес SPD (hex, 50–57)",
                "Модуль DDR5 (абс. путь)",
            ],
            Family::Liquid => ["HID-путь TT 264A:233C MI00", "", "", "", "", ""],
        }
    }

    pub fn target(&self) -> Result<Target, String> {
        if self.id.is_empty() || self.id.len() > 2048 || self.id.chars().any(char::is_control) {
            return Err("Имя источника: от 1 до 2048 байт без управляющих символов".into());
        }
        let labels = self.labels();
        for (label, value) in labels.iter().zip(&self.values) {
            if label.is_empty() && !value.is_empty() {
                return Err(
                    "Скрытые поля содержат данные другого типа источника; очистите форму".into(),
                );
            }
        }
        let hex = |index: usize, bits| number(self.values[index].as_str(), labels[index], 16, bits);
        let decimal =
            |index: usize, bits| number(self.values[index].as_str(), labels[index], 10, bits);
        let target = match self.family {
            Family::Nvidia => Target::Nvidia {
                pci: Pci {
                    device_id: hex(0, 32)? as u32,
                    subsystem_id: hex(1, 32)? as u32,
                    revision_id: hex(2, 32)? as u32,
                    extended_device_id: hex(3, 32)? as u32,
                },
            },
            Family::Amd => {
                let vendor = self.values[0].as_bytes();
                if vendor.len() != 12 || !vendor.is_ascii() {
                    return Err(
                        "CPUID vendor: требуется ровно 12 ASCII-байт без обрезки пробелов".into(),
                    );
                }
                Target::Amd {
                    vendor: vendor.try_into().expect("length checked"),
                    signature: hex(1, 32)? as u32,
                    packages: decimal(2, 32)? as u32,
                    module_path: self.module_path(3)?,
                }
            }
            Family::Ddr5 => Target::Ddr5 {
                controller: [hex(0, 64)?, hex(1, 64)?, hex(2, 64)?],
                port: decimal(3, 8)? as u8,
                address: hex(4, 8)? as u8,
                module_path: self.module_path(5)?,
            },
            Family::Liquid => {
                let path = &self.values[0];
                if path.is_empty() || path.len() > 4096 || path.trim() != path {
                    return Err("HID-путь MI00: от 1 до 4096 байт без внешних пробелов".into());
                }
                Target::Liquid {
                    explicit_mi00_path: path.clone(),
                }
            }
        };
        target.validate().map_err(|_| match self.family {
            Family::Nvidia => "NVIDIA: PCI device ID должен содержать vendor 10DE и ненулевой device ID, отличный от FFFF",
            Family::Amd => "AMD: нужны AuthenticAMD, один сокет, семейство CPUID 17h–1Ah и абсолютный путь без пустых, служебных или псевдонимных компонентов",
            Family::Ddr5 => "DDR5: нужны контроллер PIIX4 (слово 2: младшие 32 бита 790B1022), база B00 для порта 0 или B20 для порта 1, SPD 50–57 и допустимый абсолютный путь",
            Family::Liquid => "Жидкость: нужен точный HID-путь с VID 264A, PID 233C, MI00 и непустым экземпляром интерфейса",
        }.to_owned())?;
        Ok(target)
    }

    fn module_path(&self, index: usize) -> Result<String, String> {
        let path = &self.values[index];
        if !(4..=32760).contains(&path.len()) || path.trim() != path {
            return Err("Путь к модулю: от 4 до 32760 байт без внешних пробелов; нужен абсолютный путь локального диска".into());
        }
        Ok(path.clone())
    }
}

fn number(text: &str, label: &str, radix: u32, bits: u32) -> Result<u64, String> {
    let text = text.trim();
    let digits = if radix == 16 {
        text.strip_prefix("0x")
            .or_else(|| text.strip_prefix("0X"))
            .unwrap_or(text)
    } else {
        text
    };
    let max_digits = if radix == 16 {
        bits / 4
    } else if bits == 8 {
        3
    } else {
        10
    };
    if digits.is_empty()
        || digits.len() > max_digits as usize
        || !digits.bytes().all(|byte| {
            if radix == 16 {
                byte.is_ascii_hexdigit()
            } else {
                byte.is_ascii_digit()
            }
        })
    {
        return Err(format!(
            "{label}: введите {} число без знака, не более {max_digits} цифр",
            if radix == 16 {
                "шестнадцатеричное"
            } else {
                "десятичное"
            }
        ));
    }
    let value = u64::from_str_radix(digits, radix)
        .map_err(|_| format!("{label}: число не помещается в {bits} бит"))?;
    if bits < 64 && value >= (1_u64 << bits) {
        return Err(format!("{label}: число не помещается в {bits} бит"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests;
