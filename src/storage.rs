//! Bounded same-directory replacement for application-owned configuration files.
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
#[cfg(all(test, windows))]
#[path = "storage_lock_diagnostics.rs"]
mod lock_diagnostics;

struct Temporary(Option<PathBuf>);
impl Drop for Temporary {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8], max_bytes: usize) -> io::Result<()> {
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Configuration exceeds limit",
        ));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Explicit parent directory required",
            )
        })?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Filename required"))?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Destination must be a regular file",
            ));
        }
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    for _ in 0..32 {
        let mut temporary_name = name.to_os_string();
        temporary_name.push(format!(
            ".save-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let temporary = parent.join(temporary_name);
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let mut guard = Temporary(Some(temporary.clone()));
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace(&temporary, path).inspect_err(|_error| {
            #[cfg(all(test, windows))]
            lock_diagnostics::record(path, &temporary, _error);
        })?;
        guard.0 = None;
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Cannot reserve configuration temporary file",
    ))
}

#[cfg(windows)]
fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(source: *const u16, destination: *const u16, flags: u32) -> i32;
    }
    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::Prefix};
        // Rust's file APIs support long paths, but the raw MoveFileExW call
        // needs an explicit extended path without relying on machine policy.
        // absolute resolves Win32 relative/dot syntax without following links.
        let absolute = std::path::absolute(path)?;
        let mut encoded = OsString::new();
        match absolute.components().next() {
            Some(std::path::Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::VerbatimDisk(_) | Prefix::VerbatimUNC(_, _) | Prefix::Verbatim(_) => {
                    encoded.push(absolute.as_os_str());
                }
                Prefix::Disk(_) => {
                    encoded.push(r"\\?\");
                    encoded.push(absolute.as_os_str());
                }
                Prefix::UNC(_, _) => {
                    encoded.push(r"\\?\UNC\");
                    let wide: Vec<_> = absolute.as_os_str().encode_wide().skip(2).collect();
                    encoded.push(OsString::from_wide(&wide));
                }
                _ => return Err(io::Error::other("Unsupported configuration path prefix")),
            },
            _ => {
                return Err(io::Error::other(
                    "Missing absolute configuration path prefix",
                ));
            }
        }
        let mut value: Vec<_> = encoded.encode_wide().collect();
        if value.contains(&0) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"));
        }
        value.push(0);
        Ok(value)
    }
    let source = wide(source)?;
    let destination = wide(destination)?;
    // Same-directory source. No copy, deferred-at-reboot, registry or power flags.
    // https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-movefileexw
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0x1 | 0x8) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
#[cfg(not(windows))]
fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)?;
    fs::File::open(
        destination
            .parent()
            .ok_or_else(|| io::Error::other("Missing parent"))?,
    )?
    .sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    // Isolated application-owned test files only; no user configuration or devices.
    fn fixture() -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("work/storage-stand")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    #[test]
    fn replacement_and_bounded_failure_preserve_previous_file() {
        let dir = fixture();
        let path = dir.join("настройки.json");
        atomic_write(&path, b"old", 16).unwrap();
        atomic_write(&path, b"new", 16).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert!(atomic_write(&path, b"too long", 3).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
    #[test]
    fn reject_directory_and_missing_parent_without_creating_debris() {
        let dir = fixture();
        let target = dir.join("directory");
        fs::create_dir(&target).unwrap();
        assert!(atomic_write(&target, b"x", 16).is_err());
        assert!(atomic_write(&dir.join("absent/target.json"), b"x", 16).is_err());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir(target).unwrap();
        fs::remove_dir(dir).unwrap();
    }
    #[test]
    fn actual_file_claim_before_schedule_dispatch_survives_restore() {
        use crate::schedule::{Config, Schedule};
        let dir = fixture();
        let path = dir.join("schedule.json");
        let mut schedule = Schedule::new(Config {
            enabled: true,
            ..Config::default()
        })
        .unwrap();
        let mut persist =
            |bytes: &[u8]| atomic_write(&path, bytes, 262144).map_err(|e| e.to_string());
        assert_eq!(schedule.prepare(1793890800, &mut persist).unwrap().len(), 2);
        let mut restarted = Schedule::restore(&fs::read(&path).unwrap()).unwrap();
        assert!(
            restarted
                .prepare(1793890801, &mut persist)
                .unwrap()
                .is_empty()
        );
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn long_windows_destination_and_temporary_paths_replace_without_global_policy() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = fixture();
        let parent = root.join("a".repeat(110));
        let dir = parent.join("b".repeat(110));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("journal.json");
        assert!(path.as_os_str().len() > 260);
        atomic_write(&path, b"first", 32).unwrap();
        atomic_write(&path, b"second", 32).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        atomic_write(&fs::canonicalize(&path).unwrap(), b"third", 32).unwrap();
        let held = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert!(atomic_write(&path, b"blocked", 32).is_err());
        drop(held);
        assert_eq!(fs::read(&path).unwrap(), b"third");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
        fs::remove_dir(parent).unwrap();
        fs::remove_dir(root).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn locked_destination_preserves_previous_bytes_and_cleans_temp() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = fixture();
        let path = dir.join("locked.json");
        fs::write(&path, b"previous").unwrap();
        let held = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert!(atomic_write(&path, b"replacement", 32).is_err());
        drop(held);
        assert_eq!(fs::read(&path).unwrap(), b"previous");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
