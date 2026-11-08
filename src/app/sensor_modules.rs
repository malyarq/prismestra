//! Explicit pinned-file selection only; never loads or executes a module.
use super::sensor_form::{Draft, Family};
use crate::{sensors::amd, transport::pawnio::Piix4Module};
use std::path::Path;

/// Return an edited copy only after the complete target and selected file pass.
/// The runtime must revalidate the file when opening its actual provider.
pub fn select(draft: &Draft, path: &Path) -> Result<Draft, String> {
    let slot = match draft.family {
        Family::Amd => 3,
        Family::Ddr5 => 5,
        _ => return Err("Модуль можно выбрать только для AMD или DDR5".into()),
    };
    let selected = path.to_str().ok_or("Путь к модулю должен быть UTF-8")?;
    if !path.is_absolute() {
        return Err("Выберите абсолютный путь к модулю".into());
    }
    let mut edited = draft.clone();
    edited.values[slot] = selected.to_owned();
    edited.target()?;

    // PIIX4's existing reader checks content; selection also refuses file aliases.
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("Метаданные выбранного модуля: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Выбранный модуль должен быть обычным файлом без символических ссылок".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("Reparse point для выбранного модуля запрещён".into());
        }
    }
    match draft.family {
        Family::Amd => {
            amd::Module::read(path)?;
        }
        Family::Ddr5 => {
            Piix4Module::read(path)?;
        }
        _ => unreachable!("family checked before accessing the file"),
    }
    Ok(edited)
}

#[cfg(test)]
mod tests;
