//! Portable lighting profile + referenced opaque assets. No local bindings,
//! automatic extraction, native factories or executable content installation.
use super::{
    assets::{self, AssetId, Store},
    document::{self, Document},
    strict_json,
};
use crate::{devices::MAX_DEVICES, storage};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Cursor, Read, Seek, SeekFrom, Write},
    path::Path,
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

pub const MAX_MEDIA_BYTES: usize = 128 * 1024 * 1024;
const MAX_LEGACY_COOLING: usize = 4 * 1024 * 1024;
const MAX_MANIFEST: usize = document::MAX_BYTES + MAX_LEGACY_COOLING + 4096;
pub const MAX_ARCHIVE_BYTES: usize = MAX_MEDIA_BYTES + MAX_MANIFEST + 1024 * 1024;
const MAX_ENTRIES: usize = MAX_DEVICES * 2 + 1;
const PROFILE: &str = "profile.json";
fn asset_limit(document: &Document, id: &AssetId) -> usize {
    if document
        .lcd
        .iter()
        .any(|lcd| lcd.config.font.as_ref() == Some(id))
    {
        8 * 1024 * 1024
    } else {
        assets::MAX_BYTES
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    version: u32,
    document: Document,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cooling: Option<serde_json::Value>,
}
#[derive(Debug, PartialEq)]
pub struct Profile {
    pub document: Document,
}
pub struct Loaded {
    document: Document,
    assets: Vec<(AssetId, Vec<u8>)>,
}
impl Loaded {
    pub fn document(&self) -> &Document {
        &self.document
    }
    pub fn asset_count(&self) -> usize {
        self.assets.len()
    }
    /// All package bytes/hashes are checked before this can be called. Store's
    /// immutable publication never replaces existing content, even if corrupt.
    /// An I/O failure may leave verified, unreferenced blobs; no profile is applied.
    pub fn install(self, store: &Store) -> Result<Profile, String> {
        for (id, bytes) in self.assets {
            if store.put(&bytes, assets::MAX_BYTES)? != id {
                return Err("Imported asset identity changed".into());
            }
        }
        Ok(Profile {
            document: self.document,
        })
    }
}
pub fn export(path: &Path, document: &Document, store: &Store) -> Result<usize, String> {
    document.encode()?;
    let manifest = serde_json::to_vec(&Manifest {
        format: "pc-control-profile".into(),
        version: 1,
        document: document.clone(),
        cooling: None,
    })
    .map_err(|e| e.to_string())?;
    if manifest.len() > MAX_MANIFEST {
        return Err("Portable profile manifest exceeds size limit".into());
    }
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o600);
    zip.start_file(PROFILE, options)
        .map_err(|e| e.to_string())?;
    zip.write_all(&manifest).map_err(|e| e.to_string())?;
    let refs = document.assets();
    let mut total = 0usize;
    for id in &refs {
        let bytes = store.read(id, asset_limit(document, id))?;
        total = total
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_MEDIA_BYTES)
            .ok_or("Portable assets exceed 128 MiB")?;
        zip.start_file(format!("assets/{}", id.as_str()), options)
            .map_err(|e| e.to_string())?;
        zip.write_all(&bytes).map_err(|e| e.to_string())?;
    }
    let bytes = zip.finish().map_err(|e| e.to_string())?.into_inner();
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err("Portable archive exceeds size limit".into());
    }
    storage::atomic_write(path, &bytes, MAX_ARCHIVE_BYTES).map_err(|e| e.to_string())?;
    Ok(refs.len())
}

/// Bound the central-directory allocation *before* the ZIP library parses it.
/// Our small Stored ZIP profile needs neither Zip64, multi-disk, comments nor
/// self-extracting prefixes. This is a profile loader, not a general ZIP tool.
fn envelope(file: &mut File) -> Result<(), String> {
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    if !(22..=MAX_ARCHIVE_BYTES as u64).contains(&size) {
        return Err("Portable archive size is outside its limit".into());
    }
    file.seek(SeekFrom::End(-22)).map_err(|e| e.to_string())?;
    let mut end = [0u8; 22];
    file.read_exact(&mut end).map_err(|e| e.to_string())?;
    let word = |i| u16::from_le_bytes([end[i], end[i + 1]]);
    let dword = |i| u32::from_le_bytes(end[i..i + 4].try_into().unwrap());
    let entries = usize::from(word(10));
    let cd_size = u64::from(dword(12));
    let cd_offset = u64::from(dword(16));
    if &end[..4] != b"PK\x05\x06"
        || word(4) != 0
        || word(6) != 0
        || word(8) != word(10)
        || word(20) != 0
        || !(1..=MAX_ENTRIES).contains(&entries)
        || cd_size < (entries * 46) as u64
        || cd_size > (MAX_ENTRIES * 512) as u64
        || cd_offset + cd_size != size - 22
    {
        return Err("Unsupported or oversized portable ZIP directory".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).map_err(|e| e.to_string())?;
    if &magic != b"PK\x03\x04" {
        return Err("Portable ZIP has an unexpected prefix".into());
    }
    // zip-rs can otherwise skip a final EOCD pointing to non-CDFH garbage and
    // fall back to an earlier (potentially Zip64) directory before our bounds.
    file.seek(SeekFrom::Start(cd_offset))
        .map_err(|e| e.to_string())?;
    file.read_exact(&mut magic).map_err(|e| e.to_string())?;
    if &magic != b"PK\x01\x02" {
        return Err("Portable ZIP directory signature does not match its bounded EOCD".into());
    }
    file.rewind().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load(path: &Path) -> Result<Loaded, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Portable archive must be a regular file".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // Open reparse point itself, not its target.
        options.share_mode(1); // Shared reads only while validating this file.
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("Portable archive cannot be a reparse point".into());
        }
    }
    let mut file = options.open(path).map_err(|e| e.to_string())?;
    let opened = file.metadata().map_err(|e| e.to_string())?;
    if !opened.is_file() {
        return Err("Portable archive is not a regular file".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if opened.file_attributes() & 0x400 != 0 {
            return Err("Portable archive changed into a reparse point".into());
        }
    }
    envelope(&mut file)?;
    let mut zip = ZipArchive::new(file).map_err(|e| e.to_string())?;
    if zip.len() > MAX_ENTRIES || zip.has_duplicate_names() {
        return Err("Portable archive has duplicate or excessive entries".into());
    }
    zip.validate_header_consistency()
        .map_err(|e| e.to_string())?;
    let mut manifest = None;
    let mut content = Vec::new();
    let mut names = BTreeSet::new();
    let mut total = 0usize;
    for i in 0..zip.len() {
        let entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().map_err(|e| e.to_string())?.into_owned();
        if !names.insert(name.clone())
            || entry.is_dir()
            || entry.is_symlink()
            || entry.encrypted()
            || entry.compression() != CompressionMethod::Stored
            || entry.compressed_size() != entry.size()
            || entry
                .unix_mode()
                .is_some_and(|m| !matches!(m & 0o170000, 0 | 0o100000))
        {
            return Err("Unsupported portable archive entry".into());
        }
        let id = if name == PROFILE {
            None
        } else {
            Some(AssetId::try_from(
                name.strip_prefix("assets/")
                    .ok_or("Unexpected file in portable archive")?
                    .to_owned(),
            )?)
        };
        let limit = if id.is_some() {
            assets::MAX_BYTES
        } else {
            MAX_MANIFEST
        };
        let size = usize::try_from(entry.size()).map_err(|_| "Portable entry size overflow")?;
        if size > limit {
            return Err("Portable entry exceeds its size limit".into());
        }
        if id.is_some() {
            total = total
                .checked_add(size)
                .filter(|n| *n <= MAX_MEDIA_BYTES)
                .ok_or("Portable assets exceed 128 MiB")?;
        }
        let mut bytes = Vec::new();
        entry
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() != size {
            return Err("Portable entry length changed".into());
        }
        if let Some(id) = id {
            if assets::digest(&bytes) != id {
                return Err("Portable asset SHA-256 mismatch".into());
            }
            content.push((id, bytes));
        } else {
            let value: Manifest =
                serde_json::from_value(strict_json::unique_json(&bytes, MAX_MANIFEST)?)
                    .map_err(|e| e.to_string())?;
            if value.format != "pc-control-profile"
                || !matches!(
                    (value.version, value.cooling.is_some()),
                    (1, false) | (2, true)
                )
            {
                return Err("Unsupported portable profile format/version".into());
            }
            value.document.encode()?;
            // Legacy v2 payload is inert import-only data. Never activate or export it.
            if let Some(legacy) = &value.cooling
                && (!legacy.is_object()
                    || serde_json::to_vec(legacy).map_err(|e| e.to_string())?.len()
                        > MAX_LEGACY_COOLING)
            {
                return Err("Legacy cooling payload exceeds its object/size limit".into());
            }
            manifest = Some(value.document);
        }
    }
    let document = manifest.ok_or("Portable profile is missing")?;
    let expected: BTreeSet<_> = document
        .assets()
        .iter()
        .map(|id| format!("assets/{}", id.as_str()))
        .chain(std::iter::once(PROFILE.into()))
        .collect();
    if names != expected {
        return Err("Portable archive assets do not exactly match profile references".into());
    }
    for (id, bytes) in &content {
        if bytes.len() > asset_limit(&document, id) {
            return Err("Portable font exceeds 8 MiB".into());
        }
    }
    Ok(Loaded {
        document,
        assets: content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_v2_import_preserves_document_assets_and_original_archive() {
        let root = root();
        let store = Store::new(root.join("assets")).unwrap();
        let media = b"opaque legacy media transfer";
        let id = store.put(media, assets::MAX_BYTES).unwrap();
        let mut document = document();
        document.lcd[0].config.media = Some(id.clone());
        let mut value = serde_json::json!({
            "format":"pc-control-profile", "version":2, "document":document,
            "cooling":{"profile":{"FanControl":{"Controls":[]}}, "routes":[]}
        });
        let name = format!("assets/{}", id.as_str());
        let original = raw(&[
            (PROFILE, &serde_json::to_vec(&value).unwrap()),
            (&name, media),
        ]);
        let path = root.join("legacy.zip");
        fs::write(&path, &original).unwrap();
        let loaded = load(&path).unwrap().install(&store).unwrap();
        assert_eq!(loaded.document, document);
        assert_eq!(store.read(&id, assets::MAX_BYTES).unwrap(), media);
        let exported = root.join("lighting.zip");
        export(&exported, &loaded.document, &store).unwrap();
        let mut zip = ZipArchive::new(File::open(&exported).unwrap()).unwrap();
        let mut encoded = vec![];
        zip.by_name(PROFILE)
            .unwrap()
            .read_to_end(&mut encoded)
            .unwrap();
        let output: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(output["version"], 1);
        assert!(output.get("cooling").is_none());
        assert_eq!(fs::read(&path).unwrap(), original);
        value["version"] = 1.into();
        fs::write(
            &path,
            raw(&[
                (PROFILE, &serde_json::to_vec(&value).unwrap()),
                (&name, media),
            ]),
        )
        .unwrap();
        assert!(load(&path).is_err());
        value["version"] = 2.into();
        value["cooling"] = serde_json::json!({"large":"x".repeat(MAX_LEGACY_COOLING)});
        fs::write(
            &path,
            raw(&[
                (PROFILE, &serde_json::to_vec(&value).unwrap()),
                (&name, media),
            ]),
        )
        .unwrap();
        assert!(load(&path).is_err());
    }
    use crate::{
        app::{
            config::{Groups, Project},
            document::Lcd,
            lcd,
        },
        devices::{Canvas, Setup},
    };
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn root() -> PathBuf {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("work/bundle-stand");
        fs::create_dir_all(&parent).unwrap();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        for _ in 0..32 {
            let p = parent.join(format!(
                "{}-{nonce}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&p) {
                Ok(()) => return p,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("Create isolated bundle fixture: {e}"),
            }
        }
        panic!("Cannot reserve a fresh bundle fixture")
    }
    fn document() -> Document {
        let device = crate::app::device_setup::preset("tt-lcd")
            .unwrap()
            .device("lcd")
            .unwrap();
        let mut d = Document::from_lighting(Project {
            version: 1,
            setup: Setup {
                version: 1,
                devices: vec![device],
                canvas: Canvas {
                    width: 100,
                    height: 100,
                    points: vec![],
                },
            },
            scene: crate::runtime::scene::example(),
            schedule: Default::default(),
            groups: Groups {
                pc: vec!["lcd".into()],
                door: vec![],
            },
        })
        .unwrap();
        d.lcd.push(Lcd {
            device: "lcd".into(),
            config: lcd::Config::default(),
        });
        d
    }
    fn manifest(d: &Document) -> Vec<u8> {
        serde_json::to_vec(&Manifest {
            format: "pc-control-profile".into(),
            version: 1,
            document: d.clone(),
            cooling: None,
        })
        .unwrap()
    }
    fn raw(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut z = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            z.start_file(
                *name,
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .unwrap();
            z.write_all(bytes).unwrap();
        }
        z.finish().unwrap().into_inner()
    }
    #[test]
    fn referenced_assets_only_survive_a_fresh_store_and_real_media_decode() {
        let p = root();
        let first = Store::new(p.join("first")).unwrap();
        let mut gif = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
            for rgba in [[255, 64, 0, 255], [128, 16, 0, 255]] {
                encoder
                    .encode_frame(image::Frame::from_parts(
                        image::RgbaImage::from_pixel(2, 2, image::Rgba(rgba)),
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(100, 1),
                    ))
                    .unwrap();
            }
        }
        let media = first.put(&gif, assets::MAX_BYTES).unwrap();
        let font = first
            .put(b"opaque-font-transfer-only", assets::MAX_BYTES)
            .unwrap();
        first
            .put(b"unreferenced-local-content", assets::MAX_BYTES)
            .unwrap();
        let mut d = document();
        d.lcd[0].config.media = Some(media.clone());
        d.lcd[0].config.font = Some(font.clone());
        d.lcd[0].config.enabled = true;
        d.lighting.setup.devices[0].enabled = true;
        let path = p.join("profile.zip");
        assert_eq!(export(&path, &d, &first).unwrap(), 2);
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.asset_count(), 2);
        assert_eq!(loaded.document(), &d);
        let second = Store::new(p.join("second")).unwrap();
        let restored = loaded.install(&second).unwrap();
        let restored = restored.document;
        assert_eq!(second.read(&media, assets::MAX_BYTES).unwrap(), gif);
        assert_eq!(
            second.read(&font, assets::MAX_BYTES).unwrap(),
            b"opaque-font-transfer-only"
        );
        assert_eq!(fs::read_dir(p.join("second")).unwrap().count(), 2);
        // Media is parsed by the actual bounded LCD controller. The opaque
        // font above is not claimed to render; overlay is explicitly disabled.
        let player = lcd::Controller::new(restored.lcd[0].config.clone(), &second).unwrap();
        assert_eq!(player.config(), &d.lcd[0].config);
        assert_eq!(load(&path).unwrap().install(&second).unwrap().document, d); // idempotent
    }
    #[test]
    fn corrupt_or_missing_sources_never_replace_a_previous_export() {
        let p = root();
        let store = Store::new(p.join("assets")).unwrap();
        let id = store.put(b"source", assets::MAX_BYTES).unwrap();
        let mut d = document();
        d.lcd[0].config.media = Some(id.clone());
        let path = p.join("profile.zip");
        fs::write(&path, b"previous-export").unwrap();
        fs::write(p.join("assets").join(id.as_str()), b"broken").unwrap();
        assert!(export(&path, &d, &store).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"previous-export");
        fs::remove_file(p.join("assets").join(id.as_str())).unwrap();
        assert!(export(&path, &d, &store).is_err());
        assert_eq!(fs::read(path).unwrap(), b"previous-export");
    }
    #[test]
    fn rejects_unsafe_extra_missing_or_hash_mismatched_entries_without_extraction() {
        let p = root();
        let mut d = document();
        let bytes = b"stand-content";
        let id = assets::digest(bytes);
        d.lcd[0].config.media = Some(id.clone());
        let m = manifest(&d);
        let name = format!("assets/{}", id.as_str());
        let cases = [
            raw(&[(PROFILE, &m)]),
            raw(&[(PROFILE, &m), ("../escape", bytes)]),
            raw(&[(PROFILE, &m), (&name, b"wrong")]),
            raw(&[(PROFILE, &m), (&name, bytes), ("extra", bytes)]),
        ];
        for (i, archive) in cases.iter().enumerate() {
            let path = p.join(format!("bad-{i}.zip"));
            fs::write(&path, archive).unwrap();
            assert!(load(&path).is_err());
        }
        assert!(!p.join("escape").exists());
        assert!(!p.parent().unwrap().join("escape").exists());
    }
    #[test]
    fn rejects_crc_corruption_oversized_directory_and_local_header_mismatch() {
        let p = root();
        let mut d = document();
        let content = b"unique-payload-for-corruption-test";
        let id = assets::digest(content);
        d.lcd[0].config.media = Some(id.clone());
        let name = format!("assets/{}", id.as_str());
        let original = raw(&[(PROFILE, &manifest(&d)), (&name, content)]);
        let mut crc = original.clone();
        let at = crc
            .windows(content.len())
            .position(|v| v == content)
            .unwrap();
        crc[at] ^= 1;
        let mut huge = original.clone();
        let end = huge.len() - 22;
        huge[end + 8..end + 12].copy_from_slice(&[255, 255, 255, 255]);
        let mut local = original.clone();
        local[30] ^= 1; // profile local filename, not central
        for (i, archive) in [crc, huge, local, original[..original.len() - 1].to_vec()]
            .iter()
            .enumerate()
        {
            let path = p.join(format!("bad-{i}.zip"));
            fs::write(&path, archive).unwrap();
            assert!(load(&path).is_err());
        }
    }
    #[test]
    fn rejects_a_fake_final_directory_instead_of_falling_back_to_an_earlier_zip() {
        let p = root();
        let d = document();
        let mut bytes = raw(&[(PROFILE, &manifest(&d))]);
        bytes.extend_from_slice(&[b'J'; 46]);
        let offset = (bytes.len() - 46) as u32;
        let mut fake = [0u8; 22];
        fake[..4].copy_from_slice(b"PK\x05\x06");
        fake[8..10].copy_from_slice(&1u16.to_le_bytes());
        fake[10..12].copy_from_slice(&1u16.to_le_bytes());
        fake[12..16].copy_from_slice(&46u32.to_le_bytes());
        fake[16..20].copy_from_slice(&offset.to_le_bytes());
        bytes.extend_from_slice(&fake);
        let path = p.join("fallback.zip");
        fs::write(&path, bytes).unwrap();
        let error = match load(&path) {
            Ok(_) => panic!("Accepted fallback directory"),
            Err(e) => e,
        };
        assert!(error.contains("directory signature"), "{error}");
    }
    #[test]
    fn duplicate_names_and_conflicting_existing_assets_are_not_replaced() {
        let p = root();
        let mut d = document();
        let a = assets::digest(b"first");
        let b = assets::digest(b"second");
        d.lcd[0].config.media = Some(a.clone());
        d.lcd[0].config.font = Some(b.clone());
        let name_a = format!("assets/{}", a.as_str());
        let name_b = format!("assets/{}", b.as_str());
        let valid = raw(&[
            (PROFILE, &manifest(&d)),
            (&name_a, b"first"),
            (&name_b, b"second"),
        ]);
        let mut duplicate = valid.clone();
        let positions: Vec<_> = duplicate
            .windows(name_b.len())
            .enumerate()
            .filter_map(|(i, bytes)| (bytes == name_b.as_bytes()).then_some(i))
            .collect();
        assert_eq!(positions.len(), 2); // local + central header
        for i in positions {
            duplicate[i..i + name_a.len()].copy_from_slice(name_a.as_bytes());
        }
        let path = p.join("profile.zip");
        fs::write(&path, duplicate).unwrap();
        assert!(load(&path).err().unwrap().contains("duplicate"));
        fs::write(&path, valid).unwrap();
        let target = Store::new(p.join("target")).unwrap();
        fs::write(
            p.join("target").join(a.as_str()),
            b"existing-corrupt-content",
        )
        .unwrap();
        assert!(load(&path).unwrap().install(&target).is_err());
        assert_eq!(
            fs::read(p.join("target").join(a.as_str())).unwrap(),
            b"existing-corrupt-content"
        );
    }
    #[test]
    fn font_size_limit_is_enforced_on_export_and_import() {
        let p = root();
        let store = Store::new(p.join("assets")).unwrap();
        let bytes = vec![0; 8 * 1024 * 1024 + 1];
        let id = store.put(&bytes, assets::MAX_BYTES).unwrap();
        let mut d = document();
        d.lcd[0].config.font = Some(id.clone());
        let path = p.join("font.zip");
        assert!(export(&path, &d, &store).is_err());
        assert!(!path.exists());
        fs::write(
            &path,
            raw(&[
                (PROFILE, &manifest(&d)),
                (&format!("assets/{}", id.as_str()), &bytes),
            ]),
        )
        .unwrap();
        assert!(load(&path).err().unwrap().contains("font exceeds"));
    }
    #[test]
    fn empty_media_package_roundtrips_and_unsupported_version_is_rejected() {
        let p = root();
        let s = Store::new(p.join("assets")).unwrap();
        let d = document();
        let path = p.join("empty.zip");
        assert_eq!(export(&path, &d, &s).unwrap(), 0);
        assert_eq!(load(&path).unwrap().document(), &d);
        let mut m: serde_json::Value = serde_json::from_slice(&manifest(&d)).unwrap();
        m["version"] = 2.into();
        fs::write(&path, raw(&[(PROFILE, &serde_json::to_vec(&m).unwrap())])).unwrap();
        assert!(load(&path).is_err());
    }
}
