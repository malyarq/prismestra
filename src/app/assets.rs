//! Local opaque media/font content, addressed and verified by SHA-256.
//! No import, download, decoder, bundle installation or hardware behavior.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, Metadata, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub const MAX_BYTES: usize = 32 * 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct AssetId(String);
impl TryFrom<String> for AssetId {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("Asset ID must be exactly 64 lowercase SHA-256 hex characters".into());
        }
        Ok(Self(value))
    }
}
impl AssetId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub struct Store {
    root: PathBuf,
}
impl Store {
    /// Creates only the final missing directory; its explicit parent must exist.
    pub fn new(root: PathBuf) -> Result<Self, String> {
        if !root.is_absolute() {
            return Err("Asset root must be an explicit absolute path".into());
        }
        #[cfg(windows)]
        {
            use std::path::{Component, Prefix};
            if !matches!(root.components().next(), Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
            {
                return Err(
                    "Asset root must be on an explicit local disk, not a network or device path"
                        .into(),
                );
            }
        }
        match fs::symlink_metadata(&root) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::create_dir(&root) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
            Err(error) => return Err(error.to_string()),
        }
        let store = Self { root };
        store.check_root()?;
        Ok(store)
    }
    fn check_root(&self) -> Result<(), String> {
        let metadata = fs::symlink_metadata(&self.root).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || is_link(&metadata) {
            return Err("Asset root must be a regular directory without reparse points".into());
        }
        Ok(())
    }
    pub fn put(&self, bytes: &[u8], limit: usize) -> Result<AssetId, String> {
        let limit = limit.min(MAX_BYTES);
        if bytes.len() > limit {
            return Err("Asset exceeds caller limit or 32 MiB cap".into());
        }
        self.check_root()?;
        let id = digest(bytes);
        let target = self.root.join(id.as_str());
        match fs::symlink_metadata(&target) {
            Ok(_) => {
                self.read(&id, limit)?;
                return Ok(id);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        // Publish without replacement so a concurrent writer's destination,
        // including a corrupt file, is never silently overwritten.
        for _ in 0..32 {
            let stage = self.root.join(format!(
                ".{}.stage-{}-{}",
                id.as_str(),
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let mut reserved = match OpenOptions::new().write(true).create_new(true).open(&stage) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!("Reserve asset stage {}: {error}", stage.display()));
                }
            };
            let temporary = Temporary(stage);
            // Write through the reserved handle; replacing an empty stage adds an unnecessary move.
            let staged = reserved
                .write_all(bytes)
                .map_err(|error| format!("Write asset stage {}: {error}", temporary.0.display()))
                .and_then(|()| {
                    reserved.sync_all().map_err(|error| {
                        format!("Sync asset stage {}: {error}", temporary.0.display())
                    })
                });
            drop(reserved);
            staged?;
            self.check_root()?;
            match publish(&temporary.0, &target) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(format!("Atomic asset publication failed: {error}")),
            }
            self.read(&id, limit)?;
            return Ok(id);
        }
        Err("Cannot reserve an asset staging file".into())
    }
    pub fn read(&self, id: &AssetId, limit: usize) -> Result<Vec<u8>, String> {
        self.check_root()?;
        let path = self.root.join(id.as_str());
        let limit = limit.min(MAX_BYTES);
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        check_asset(&metadata, limit)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT.
        }
        let file = options.open(&path).map_err(|error| error.to_string())?;
        check_asset(&file.metadata().map_err(|error| error.to_string())?, limit)?;
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > limit {
            return Err("Asset grew beyond its read limit".into());
        }
        if digest(&bytes) != *id {
            return Err("Asset content SHA-256 does not match its ID".into());
        }
        Ok(bytes)
    }
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
#[cfg(windows)]
fn publish(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(source: *const u16, destination: *const u16, flags: u32) -> i32;
    }
    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let mut value: Vec<u16> = path.as_os_str().encode_wide().collect();
        if value.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NUL in asset path",
            ));
        }
        value.push(0);
        Ok(value)
    }
    let source = wide(source)?;
    let destination = wide(destination)?;
    // Same-directory move; WRITE_THROUGH only. In particular, never include
    // REPLACE_EXISTING, COPY_ALLOWED or DELAY_UNTIL_REBOOT.
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0x8) } != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if matches!(error.raw_os_error(), Some(80) | Some(183)) {
        return Err(io::Error::from(io::ErrorKind::AlreadyExists));
    }
    Err(error)
}
#[cfg(not(windows))]
fn publish(source: &Path, destination: &Path) -> io::Result<()> {
    // Portable create-if-absent publication; non-Windows filesystems must support
    // hard links. An unsupported filesystem fails closed instead of replacing.
    fs::hard_link(source, destination)
}
fn is_link(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        } // FILE_ATTRIBUTE_REPARSE_POINT.
    }
    false
}
fn check_asset(metadata: &Metadata, limit: usize) -> Result<(), String> {
    if !metadata.is_file() || is_link(metadata) || metadata.len() > limit as u64 {
        return Err("Asset must be a bounded regular file without reparse points".into());
    }
    Ok(())
}
pub(crate) fn digest(bytes: &[u8]) -> AssetId {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut hex = String::with_capacity(64);
    for &byte in Sha256::digest(bytes).iter() {
        hex.push(HEX[(byte >> 4) as usize] as char);
        hex.push(HEX[(byte & 15) as usize] as char);
    }
    AssetId(hex)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TemporaryRoot(PathBuf);
    impl TemporaryRoot {
        fn new() -> Self {
            let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("work/assets-stand");
            fs::create_dir_all(&parent).unwrap();
            let root = parent.join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn store(&self) -> Store {
            Store::new(self.0.join("assets")).unwrap()
        }
    }
    impl Drop for TemporaryRoot {
        fn drop(&mut self) {
            let assets = self.0.join("assets");
            if let Ok(entries) = fs::read_dir(&assets) {
                for entry in entries.flatten() {
                    let _ = fs::remove_file(entry.path());
                }
                let _ = fs::remove_dir(&assets);
            }
            let _ = fs::remove_file(self.0.join("file"));
            let _ = fs::remove_dir(&self.0);
        }
    }
    #[test]
    fn strict_id_serde_blocks_traversal_and_noncanonical_hashes() {
        let valid = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let id = AssetId::try_from(valid.to_owned()).unwrap();
        assert_eq!(id.as_str(), valid);
        assert_eq!(serde_json::to_string(&id).unwrap(), format!("\"{valid}\""));
        assert_eq!(
            serde_json::from_str::<AssetId>(&format!("\"{valid}\"")).unwrap(),
            id
        );
        for invalid in [
            String::new(),
            "a".repeat(63),
            "a".repeat(65),
            valid.to_uppercase(),
            "../asset".into(),
            r"C:\asset".into(),
            "g".repeat(64),
            format!("{}/", "a".repeat(63)),
        ] {
            assert!(AssetId::try_from(invalid.clone()).is_err());
            assert!(
                serde_json::from_str::<AssetId>(&serde_json::to_string(&invalid).unwrap()).is_err()
            );
        }
    }
    #[test]
    fn put_hash_read_deduplicate_and_atomic_limit_failure_preserve_good_file() {
        let root = TemporaryRoot::new();
        let store = root.store();
        let id = store.put(b"abc", 32).unwrap();
        assert_eq!(
            id.as_str(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(store.read(&id, 3).unwrap(), b"abc");
        assert_eq!(store.put(b"abc", 3).unwrap(), id);
        assert!(store.put(b"abc", 2).is_err());
        assert!(store.read(&id, 2).is_err());
        assert_eq!(store.read(&id, 32).unwrap(), b"abc");
        assert_eq!(fs::read_dir(&store.root).unwrap().count(), 1);
        let empty = store.put(b"", 0).unwrap();
        assert!(store.read(&empty, 0).unwrap().is_empty());
        assert_eq!(fs::read_dir(&store.root).unwrap().count(), 2);
    }
    #[test]
    fn tampering_is_detected_on_read_and_put_without_overwriting_corruption() {
        let root = TemporaryRoot::new();
        let store = root.store();
        let id = store.put(b"abc", 100).unwrap();
        let path = store.root.join(id.as_str());
        fs::write(&path, b"bad").unwrap();
        assert!(store.read(&id, 100).is_err());
        assert!(store.put(b"abc", 100).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"bad");
        assert_eq!(fs::read_dir(&store.root).unwrap().count(), 1);
    }
    #[test]
    fn concurrent_identical_publications_deduplicate_without_stage_debris() {
        let root = TemporaryRoot::new();
        let store = root.store();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let threads = (0..8)
            .map(|_| {
                let directory = store.root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let store = Store::new(directory).unwrap();
                    barrier.wait();
                    let id = store.put(b"shared asset", 100).unwrap();
                    assert_eq!(store.read(&id, 100).unwrap(), b"shared asset");
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(fs::read_dir(&store.root).unwrap().count(), 1);
    }
    #[test]
    fn publication_collision_preserves_target_and_staged_bytes() {
        let root = TemporaryRoot::new();
        let store = root.store();
        let id = store.put(b"abc", 100).unwrap();
        let target = store.root.join(id.as_str());
        let stage = Temporary(store.root.join(".collision-stage"));
        fs::write(&stage.0, b"staged").unwrap();
        assert_eq!(
            publish(&stage.0, &target).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&target).unwrap(), b"abc");
        assert_eq!(fs::read(&stage.0).unwrap(), b"staged");
        fs::write(&target, b"bad").unwrap();
        assert_eq!(
            publish(&stage.0, &target).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&target).unwrap(), b"bad");
        assert_eq!(fs::read(&stage.0).unwrap(), b"staged");
        assert!(store.put(b"abc", 100).is_err());
        drop(stage);
        assert_eq!(fs::read_dir(&store.root).unwrap().count(), 1);
    }
    #[test]
    fn independent_hard_cap_and_nonregular_paths_are_rejected() {
        let root = TemporaryRoot::new();
        let store = root.store();
        assert!(store.put(&vec![0; MAX_BYTES + 1], usize::MAX).is_err());
        assert_eq!(fs::read_dir(&store.root).unwrap().count(), 0);
        let id = store.put(b"abc", 100).unwrap();
        let path = store.root.join(id.as_str());
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_BYTES as u64 + 1)
            .unwrap();
        assert!(store.read(&id, usize::MAX).is_err());
        assert!(store.put(b"abc", 100).is_err());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(store.read(&id, 100).is_err());
        assert!(store.put(b"abc", 100).is_err());
        fs::remove_dir(&path).unwrap();
        let file = root.0.join("file");
        fs::write(&file, b"keep").unwrap();
        assert!(Store::new(file.clone()).is_err());
        assert_eq!(fs::read(&file).unwrap(), b"keep");
        assert!(Store::new(root.0.join("missing-parent/assets")).is_err());
        assert!(!root.0.join("missing-parent").exists());
        assert!(Store::new("relative-assets".into()).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn symlink_assets_roots_and_replaced_roots_are_rejected() {
        use std::os::unix::fs::symlink;
        let root = TemporaryRoot::new();
        let store = root.store();
        let id = store.put(b"abc", 100).unwrap();
        let path = store.root.join(id.as_str());
        fs::remove_file(&path).unwrap();
        let outside = root.0.join("file");
        fs::write(&outside, b"abc").unwrap();
        symlink(&outside, &path).unwrap();
        assert!(store.read(&id, 100).is_err());
        assert!(store.put(b"abc", 100).is_err());
        fs::remove_file(&path).unwrap();
        fs::remove_dir(&store.root).unwrap();
        symlink(&root.0, &store.root).unwrap();
        assert!(Store::new(store.root.clone()).is_err());
        assert!(store.read(&id, 100).is_err());
        assert!(store.put(b"abc", 100).is_err());
        fs::remove_file(&store.root).unwrap();
        assert_eq!(fs::read(&outside).unwrap(), b"abc");
    }
}
