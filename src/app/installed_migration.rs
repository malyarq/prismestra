//! Same-machine import of a previously saved Prismestra session.
use super::{
    assets::{self, Store},
    config::Project,
    document::{self, Document},
    installed::checked_local_path,
    lighting_outputs::Local,
    sensor_bindings,
};
use crate::{
    devices::Binding,
    storage,
    ui::catalog::{Catalog, draft},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::File,
    io::{self, Read},
    net::SocketAddr,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    ptr::null_mut,
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, HANDLE, LocalFree, WAIT_OBJECT_0,
            WAIT_TIMEOUT,
        },
        System::{
            Com::CoTaskMemFree,
            Threading::{
                GetCurrentProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
                PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
            },
        },
        UI::Shell::{
            CommandLineToArgvW, FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads,
            KF_FLAG_DONT_VERIFY, SHGetKnownFolderPath,
        },
    },
    core::GUID,
};

const RECEIPT: &str = "installed-migration.json";
const SOURCE_HINT: &str = "legacy-source.json";
const LEGACY_LAUNCHER_SHA: &str =
    "82def8e5478fa616d691ebda66d0277c8950cc1d092b0797f8e99218ce670318";
const LOCAL: &str = "lighting-local.json";
const INSTALLED_LOCAL: &str = "state/lighting-local.json";
const INSTALLED_SENSOR_CATALOG: &str = "state/sensor-sources.json";
const OPTIONAL_IMPORT_FILES: [(&str, usize); 3] = [
    ("sensor-sources.json", sensor_bindings::MAX_BYTES),
    ("scene-library.json", 256 * 1024),
    ("editor-scene-draft.json", draft::MAX_BYTES),
];
const MAX_DISCOVERY: usize = 128 * 1024;
const MAX_ASSETS: usize = 512 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MigrationOutcome {
    None,
    Imported {
        source_running: bool,
        used_bundled_backend: bool,
        external_uses_bundled_endpoint: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Source {
    profile: PathBuf,
    state: PathBuf,
}

impl Source {
    fn checked(profile: &Path, state: &Path) -> Result<Self, String> {
        let profile = checked_local_path(profile).map_err(err)?;
        let state = checked_local_path(state).map_err(err)?;
        if !profile.is_file() || !state.is_dir() {
            return Err("Legacy Prismestra profile and state paths are invalid".into());
        }
        Ok(Self { profile, state })
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u32,
    source: Source,
    before: BTreeMap<String, Option<String>>,
    after: BTreeMap<String, String>,
    complete: bool,
    source_running: bool,
    #[serde(default)]
    handover_completed: bool,
    used_bundled_backend: bool,
    external_uses_bundled_endpoint: bool,
}
impl Receipt {
    fn outcome(&self, source_running: bool) -> MigrationOutcome {
        MigrationOutcome::Imported {
            source_running,
            used_bundled_backend: self.used_bundled_backend,
            external_uses_bundled_endpoint: self.external_uses_bundled_endpoint,
        }
    }

    fn record_live_frontend(&mut self) -> bool {
        let changed = self.handover_completed || !self.source_running;
        self.handover_completed = false;
        self.source_running = true;
        changed
    }
}

/// Call with the installed profile's bootstrap lock held and its state directory present.
pub(crate) fn migrate_if_pristine(
    user_root: &Path,
    pristine_document: &Document,
    bundled_endpoint: SocketAddr,
) -> Result<MigrationOutcome, String> {
    let pending = read_receipt(user_root)?;
    if let Some(receipt) = &pending {
        if receipt.complete {
            // If process inventory fails, keep a prior observed writer suppressed.
            let running =
                running_sources().map_or(true, |sources| sources.contains(&receipt.source));
            return Ok(receipt.outcome(running));
        }
        let running = running_sources().map_or(receipt.source_running, |sources| {
            sources.contains(&receipt.source)
        });
        return apply(
            user_root,
            receipt.source.clone(),
            bundled_endpoint,
            running,
            pending,
        );
    }
    if !is_pristine(user_root, pristine_document)? {
        return Ok(MigrationOutcome::None);
    }
    let inventory = running_sources();
    let confirmed_inventory = inventory.is_ok();
    let active = inventory.unwrap_or_default();
    let mut candidates = if active.is_empty() {
        known_sources(user_root, LEGACY_LAUNCHER_SHA)?
    } else {
        let mut candidates = active.clone();
        candidates.extend(hinted_source(user_root, LEGACY_LAUNCHER_SHA));
        candidates
    };
    candidates.retain(|source| {
        !source.profile.starts_with(user_root) && !source.state.starts_with(user_root)
    });
    let Some(source) = unique(candidates)? else {
        return Ok(MigrationOutcome::None);
    };
    // A known-folder source can still be imported if CIM is unavailable, but
    // a missing process inventory cannot prove a previous writer has exited.
    let running = !confirmed_inventory || active.contains(&source);
    apply(user_root, source, bundled_endpoint, running, None)
}

/// Imports an explicitly identified same-machine profile under the same bootstrap lock.
#[cfg(test)]
pub(crate) fn migrate_from(
    user_root: &Path,
    pristine_document: &Document,
    bundled_endpoint: SocketAddr,
    profile: &Path,
    state: &Path,
    source_running: bool,
) -> Result<MigrationOutcome, String> {
    let receipt = read_receipt(user_root)?;
    if let Some(current) = &receipt {
        if current.complete {
            return Ok(MigrationOutcome::None);
        }
    } else if !is_pristine(user_root, pristine_document)? {
        return Ok(MigrationOutcome::None);
    }
    let source = Source::checked(profile, state)?;
    apply(user_root, source, bundled_endpoint, source_running, receipt)
}

fn unique(sources: BTreeSet<Source>) -> Result<Option<Source>, String> {
    if sources.len() > 1 {
        return Err(
            "Несколько сохранённых профилей Prismestra. Выберите профиль для переноса.".into(),
        );
    }
    Ok(sources.into_iter().next())
}

fn is_pristine(root: &Path, pristine: &Document) -> Result<bool, String> {
    if let Some(profile) = read_optional(&root.join("profile.json"), document::MAX_BYTES)?
        && Document::decode(&profile).as_ref() != Ok(pristine)
    {
        return Ok(false);
    }
    if let Some(local) = read_optional(&root.join("state").join(LOCAL), document::MAX_BYTES)? {
        let Ok(local) = serde_json::from_value::<Local>(super::strict_json::unique_json(
            &local,
            document::MAX_BYTES,
        )?) else {
            return Ok(false);
        };
        if local.version != 1
            || !local.devices.is_empty()
            || !local.monitors.monitors.is_empty()
            || local.door_brightness != 100
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn apply(
    root: &Path,
    source: Source,
    bundled: SocketAddr,
    source_running: bool,
    pending: Option<Receipt>,
) -> Result<MigrationOutcome, String> {
    if source.profile.starts_with(root) || source.state.starts_with(root) {
        return Err("The installed Prismestra data cannot be its own migration source".into());
    }
    let Some(profile) = read_optional(&source.profile, document::MAX_BYTES)? else {
        if let Some(receipt) = pending
            && let Some(outcome) = finish_written_import(root, receipt, source_running)?
        {
            return Ok(outcome);
        }
        return Err(format!(
            "Missing Prismestra file: {}",
            source.profile.display()
        ));
    };
    let (document, profile) = match Document::decode(&profile) {
        Ok(document) => (document, profile),
        Err(_) => {
            let document = Document::from_lighting(Project::decode(&profile)?)?;
            let canonical = document.encode()?;
            (document, canonical)
        }
    };
    if document.lighting.setup.devices.is_empty() {
        return Err("The legacy source does not contain a saved device map".into());
    }
    let local_path = source.state.join(LOCAL);
    checked_local_path(&local_path).map_err(err)?;
    let (mut local, local_bytes) = Local::read_draft_bytes(&local_path, &document)?;
    local.validate(&document)?;
    let owned_endpoint = owned_backend_endpoint(&source);
    let mut used_bundled_backend = false;
    let mut external_uses_bundled_endpoint = false;
    let mut rewritten = false;
    for binding in local.devices.values_mut() {
        if let Binding::OpenRgb { endpoint, .. } = binding {
            if Some(*endpoint) == owned_endpoint {
                rewritten |= *endpoint != bundled;
                *endpoint = bundled;
                used_bundled_backend = true;
            } else if *endpoint == bundled {
                external_uses_bundled_endpoint = true;
            }
        }
    }
    if used_bundled_backend && external_uses_bundled_endpoint {
        return Err("Legacy bindings use the same port as the new bundled backend for a separate OpenRGB server".into());
    }
    local.validate(&document)?;
    let local_bytes = if rewritten {
        serde_json::to_vec_pretty(&local).map_err(err)?
    } else {
        local_bytes
    };
    let mut files = BTreeMap::from([
        ("profile.json".to_owned(), profile),
        (INSTALLED_LOCAL.to_owned(), local_bytes),
    ]);
    let mut old_sensor_pin = None;
    for (name, limit) in OPTIONAL_IMPORT_FILES {
        let destination = format!("state/{name}");
        if pending
            .as_ref()
            .is_none_or(|receipt| !receipt.after.contains_key(&destination))
            && read_optional(&root.join(&destination), limit)?.is_some()
        {
            continue;
        }
        let path = source.state.join(name);
        let Some(bytes) = read_optional(&path, limit)? else {
            continue;
        };
        let bytes = match name {
            "sensor-sources.json" => {
                let original = fingerprint(&bytes);
                let rebased = rebase_sensor_scope(
                    bytes,
                    &source.profile,
                    &root.join("profile.json"),
                    &document,
                )?;
                if pending
                    .as_ref()
                    .and_then(|receipt| receipt.after.get(&destination))
                    == Some(&original)
                    && fingerprint(&rebased) != original
                {
                    old_sensor_pin = Some(original);
                }
                rebased
            }
            "scene-library.json" => {
                Catalog::load(&path, &document.lighting.scene)?;
                bytes
            }
            _ => {
                draft::Draft::decode(&bytes)?;
                bytes
            }
        };
        files.insert(destination, bytes);
    }
    let after: BTreeMap<String, String> = files
        .iter()
        .map(|(path, bytes)| (path.clone(), fingerprint(bytes)))
        .collect();
    let receipt = match pending {
        Some(mut receipt) => {
            if receipt.source != source
                || receipt.before.len() != files.len()
                || files.keys().any(|name| !receipt.before.contains_key(name))
            {
                return Err("Prismestra settings changed during an interrupted import".into());
            }
            if let Some(old_pin) = old_sensor_pin {
                receipt.after.insert(
                    INSTALLED_SENSOR_CATALOG.into(),
                    after[INSTALLED_SENSOR_CATALOG].clone(),
                );
                let current = read_optional(
                    &root.join(INSTALLED_SENSOR_CATALOG),
                    sensor_bindings::MAX_BYTES,
                )?
                .as_deref()
                .map(fingerprint);
                if current.as_ref() == Some(&old_pin) {
                    receipt
                        .before
                        .insert(INSTALLED_SENSOR_CATALOG.into(), current);
                }
            }
            if receipt.after != after {
                return Err("Prismestra settings changed during an interrupted import".into());
            }
            receipt
        }
        None => {
            let mut before = BTreeMap::new();
            for path in files.keys() {
                before.insert(
                    path.clone(),
                    read_optional(&root.join(path), document::MAX_BYTES)?
                        .as_deref()
                        .map(fingerprint),
                );
            }
            Receipt {
                version: 1,
                source,
                before,
                after,
                complete: false,
                source_running,
                handover_completed: false,
                used_bundled_backend,
                external_uses_bundled_endpoint,
            }
        }
    };
    for (path, after) in &receipt.after {
        let current = read_optional(&root.join(path), document::MAX_BYTES)?
            .as_deref()
            .map(fingerprint);
        if current.as_ref() != Some(after) && current != receipt.before[path] {
            return Err("Installed Prismestra settings changed during import".into());
        }
    }
    copy_assets(&receipt.source.state, &root.join("state"), &document)?;
    write_receipt(root, &receipt)?;
    // Commit the profile last, after all of the routes and referenced assets exist.
    for (path, bytes) in files
        .iter()
        .filter(|(path, _)| *path != "profile.json")
        .chain(files.get_key_value("profile.json"))
    {
        let destination = root.join(path);
        let current = read_optional(&destination, document::MAX_BYTES)?;
        if current.as_ref() != Some(bytes) {
            if current.as_deref().map(fingerprint) != receipt.before[path] {
                return Err("Installed Prismestra settings changed during import".into());
            }
            storage::atomic_write(&destination, bytes, document::MAX_BYTES).map_err(err)?;
        }
    }
    finish_import(root, receipt, source_running)
}

fn finish_import(
    root: &Path,
    mut receipt: Receipt,
    source_running: bool,
) -> Result<MigrationOutcome, String> {
    receipt.complete = true;
    receipt.source_running = source_running;
    write_receipt(root, &receipt)?;
    Ok(receipt.outcome(source_running))
}

fn finish_written_import(
    root: &Path,
    receipt: Receipt,
    source_running: bool,
) -> Result<Option<MigrationOutcome>, String> {
    // The source may disappear after the last write and before the receipt is
    // committed. Only a complete, verified target can finish without the source.
    if receipt.before.len() != receipt.after.len()
        || !receipt.after.contains_key("profile.json")
        || !receipt.after.contains_key(INSTALLED_LOCAL)
        || !receipt.after.keys().all(|path| {
            receipt.before.contains_key(path)
                && (matches!(path.as_str(), "profile.json" | INSTALLED_LOCAL)
                    || path.strip_prefix("state/").is_some_and(|name| {
                        OPTIONAL_IMPORT_FILES.iter().any(|(file, _)| *file == name)
                    }))
        })
    {
        return Err("Invalid Prismestra import receipt".into());
    }
    let mut profile = None;
    let mut sensor_catalog = None;
    for (path, expected) in &receipt.after {
        let Some(bytes) = read_optional(&root.join(path), document::MAX_BYTES)? else {
            return Ok(None);
        };
        if &fingerprint(&bytes) != expected {
            return Ok(None);
        }
        if path == "profile.json" {
            profile = Some(Document::decode(&bytes)?);
        } else if path == INSTALLED_SENSOR_CATALOG {
            sensor_catalog = Some((bytes, expected));
        }
    }
    let document = profile.ok_or("Missing imported Prismestra profile")?;
    if let Some((bytes, expected)) = sensor_catalog {
        let rebased = rebase_sensor_scope(
            bytes,
            &receipt.source.profile,
            &root.join("profile.json"),
            &document,
        )?;
        if &fingerprint(&rebased) != expected {
            return Err(
                "Restore the legacy Prismestra profile to finish migrating its temperature routes"
                    .into(),
            );
        }
    }
    let referenced = document.assets();
    if !referenced.is_empty() {
        let path = checked_local_path(&root.join("state/assets")).map_err(err)?;
        let assets = Store::new(path)?;
        let mut total = 0;
        for id in referenced {
            total += assets.read(id, assets::MAX_BYTES)?.len();
            if total > MAX_ASSETS {
                return Err("Imported Prismestra assets exceed 512 MiB".into());
            }
        }
    }
    finish_import(root, receipt, source_running).map(Some)
}

fn rebase_sensor_scope(
    bytes: Vec<u8>,
    source: &Path,
    target: &Path,
    document: &Document,
) -> Result<Vec<u8>, String> {
    use sensor_bindings::Scope;
    let mut catalog = sensor_bindings::Catalog::decode(&bytes)?;
    let inputs = document
        .temperatures
        .sources
        .iter()
        .filter_map(|source| match &source.value {
            super::sensors::Value::Sensor { input } => Some(input.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if inputs.is_empty() {
        return Ok(bytes);
    }

    let source = source.to_string_lossy();
    let plain = source.strip_prefix(r"\\?\").unwrap_or(&source);
    let target = Scope::lighting(&target.to_string_lossy(), document)?;
    if catalog.routes.iter().any(|route| route.scope == target) {
        return Ok(bytes);
    }
    let old_scopes = [source.as_ref(), plain]
        .into_iter()
        .map(|name| Scope::lighting(name, document))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let mut present = old_scopes
        .iter()
        .filter(|scope| catalog.routes.iter().any(|route| &route.scope == *scope));
    let Some(old) = present.next() else {
        return Ok(bytes);
    };
    if present.next().is_some() || catalog.prepare(old, &inputs).is_err() {
        return Ok(bytes);
    }
    for route in &mut catalog.routes {
        if &route.scope == old {
            route.scope = target.clone();
        }
    }
    catalog.encode()
}

fn copy_assets(source: &Path, target: &Path, document: &Document) -> Result<(), String> {
    let assets = document.assets();
    if assets.is_empty() {
        return Ok(());
    }
    let source = checked_local_path(&source.join("assets")).map_err(err)?;
    let source = Store::new(source)?;
    let target = Store::new(target.join("assets"))?;
    let mut total = 0;
    for id in assets {
        let bytes = source.read(id, assets::MAX_BYTES)?;
        total += bytes.len();
        if total > MAX_ASSETS {
            return Err("Legacy Prismestra assets exceed 512 MiB".into());
        }
        target.put(&bytes, assets::MAX_BYTES)?;
    }
    Ok(())
}

fn read_receipt(root: &Path) -> Result<Option<Receipt>, String> {
    let Some(bytes) = read_optional(&root.join(RECEIPT), MAX_DISCOVERY)? else {
        return Ok(None);
    };
    let value: Receipt = serde_json::from_slice(&bytes).map_err(err)?;
    if value.version != 1 {
        return Err("Unsupported Prismestra import receipt".into());
    }
    Ok(Some(value))
}

fn write_receipt(root: &Path, receipt: &Receipt) -> Result<(), String> {
    storage::atomic_write(
        &root.join(RECEIPT),
        &serde_json::to_vec(receipt).map_err(err)?,
        MAX_DISCOVERY,
    )
    .map_err(err)
}

fn read_optional(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, String> {
    let path = match checked_local_path(path) {
        Ok(path) => path,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(err(e)),
    };
    let file = File::open(path).map_err(err)?;
    let metadata = file.metadata().map_err(err)?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err("Expected a bounded regular Prismestra file".into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(err)?;
    if bytes.len() > limit {
        return Err("Prismestra file exceeds its bound".into());
    }
    Ok(Some(bytes))
}

fn err(value: impl std::fmt::Display) -> String {
    value.to_string()
}

fn fingerprint(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn image_matches(path: &Path, expected: &str) -> bool {
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let Ok(file) = File::open(path) else {
        return false;
    };
    let Ok(meta) = file.metadata() else {
        return false;
    };
    if !meta.is_file() || meta.len() > 128 * 1024 * 1024 {
        return false;
    }
    let mut read = file.take(128 * 1024 * 1024 + 1);
    let mut hash = Sha256::new();
    let mut buffer = [0; 32 * 1024];
    let mut total = 0;
    loop {
        let Ok(size) = read.read(&mut buffer) else {
            return false;
        };
        if size == 0 {
            break;
        }
        total += size;
        hash.update(&buffer[..size]);
    }
    let actual: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    total <= 128 * 1024 * 1024 && actual.eq_ignore_ascii_case(expected)
}

#[derive(Deserialize)]
struct LegacyBackend {
    version: u32,
    enabled: bool,
    graceful_shutdown: bool,
    endpoint: SocketAddr,
    executable: PathBuf,
    config_dir: PathBuf,
    expected_executable_sha256: String,
}

fn owned_backend_endpoint(source: &Source) -> Option<SocketAddr> {
    owned_backend(source).map(|config| config.endpoint)
}

fn owned_backend(source: &Source) -> Option<LegacyBackend> {
    let bytes = read_optional(&source.state.join("managed-openrgb.json"), 16_384).ok()??;
    let config: LegacyBackend = serde_json::from_slice(&bytes).ok()?;
    if config.version != 1
        || !config.enabled
        || !config.graceful_shutdown
        || !config.endpoint.ip().is_loopback()
        || config.endpoint.port() == 0
    {
        return None;
    }
    let profile_root = source.profile.parent()?;
    let install_root = if profile_root.file_name()?.eq_ignore_ascii_case("data") {
        profile_root.parent()?
    } else {
        profile_root
    };
    let image = checked_local_path(&config.executable).ok()?;
    let config_dir = checked_local_path(&config.config_dir).ok()?;
    let runtime = image.parent()?;
    let component = runtime.parent()?;
    if !image.starts_with(install_root.join("components/openrgb"))
        || !image.file_name()?.eq_ignore_ascii_case("OpenRGB.exe")
        || !runtime.file_name()?.eq_ignore_ascii_case("runtime")
        || !config_dir.starts_with(install_root)
        || !config_dir.is_dir()
    {
        return None;
    }
    let bytes = read_optional(&component.join("component-manifest.json"), MAX_DISCOVERY).ok()??;
    let manifest: serde_json::Value =
        super::strict_json::unique_json(&bytes, MAX_DISCOVERY).ok()?;
    if manifest["version"].as_u64()? != 1
        || !manifest["engine_sha256"]
            .as_str()?
            .eq_ignore_ascii_case(&config.expected_executable_sha256)
    {
        return None;
    }
    // Both the earlier private component build and the portable distribution
    // carry this image inventory, with different descriptive scope labels.
    let mut rows = manifest["files"]
        .as_array()?
        .iter()
        .filter(|row| row["path"].as_str() == Some("runtime/OpenRGB.exe"));
    let expected = rows.next()?;
    if rows.next().is_some()
        || expected["size"].as_u64()? != image.metadata().ok()?.len()
        || !expected["sha256"]
            .as_str()?
            .eq_ignore_ascii_case(&config.expected_executable_sha256)
        || !image_matches(&image, &config.expected_executable_sha256)
    {
        return None;
    }
    Some(LegacyBackend {
        executable: image,
        config_dir,
        ..config
    })
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct DrainIdentity {
    pid: u32,
    created: u64,
    executable: PathBuf,
}

#[derive(Deserialize)]
struct DrainRequest {
    version: u32,
    clock_domain: String,
    nonce: String,
    parent: DrainIdentity,
}

#[derive(Deserialize)]
struct DrainStatus {
    #[serde(flatten)]
    request: DrainRequest,
    helper: DrainIdentity,
    backend_pid: Option<u32>,
}

fn same_drain_owner(request: &DrainRequest, status: &DrainStatus) -> bool {
    const CLOCK: &str = "windows-unbiased-interrupt-ms-v1";
    request.version == 2
        && status.request.version == 2
        && request.clock_domain == CLOCK
        && status.request.clock_domain == CLOCK
        && !request.nonce.is_empty()
        && request.nonce.len() <= 128
        && request.nonce == status.request.nonce
        && request.parent == status.request.parent
        && request.parent.pid != 0
        && request.parent.created != 0
        && status.helper.pid != 0
        && status.helper.created != 0
        && status.helper.pid != request.parent.pid
        && status.helper.executable == request.parent.executable
}

fn read_drain_data<T: serde::de::DeserializeOwned>(
    directory: &Path,
    name: &str,
) -> Result<Option<T>, String> {
    read_optional(&directory.join(name), 16_384)?
        .map(|bytes| {
            let value = super::strict_json::unique_json(&bytes, 16_384)?;
            serde_json::from_value(value).map_err(err)
        })
        .transpose()
}

/// After the legacy frontend's normal exit, wait for its proven managed
/// processes. A successful drain is durable until a verified old frontend reappears.
pub(crate) fn wait_for_migrated_backend(root: &Path) -> Result<(), String> {
    let Some(mut receipt) = read_receipt(root)? else {
        return Ok(()); // A genuinely fresh installation has nothing to drain.
    };
    if !receipt.complete {
        return Err("Не завершён перенос прежнего профиля Prismestra.".into());
    }
    if receipt.handover_completed {
        return Ok(());
    }
    drain_backend(&receipt)?;
    receipt.handover_completed = true;
    receipt.source_running = false;
    write_receipt(root, &receipt)
}

fn drain_backend(receipt: &Receipt) -> Result<(), String> {
    let Some(config) = owned_backend(&receipt.source) else {
        return if receipt.used_bundled_backend {
            Err("Не удалось подтвердить прежний бэкенд OpenRGB. Новый вывод не запущен.".into())
        } else {
            Ok(())
        };
    };
    let until = Instant::now() + Duration::from_secs(35);
    let directory = &receipt.source.state;
    let request: Option<DrainRequest> = read_drain_data(directory, "owner-request.json")?;
    let status: Option<DrainStatus> = read_drain_data(directory, "helper-status.json")?;
    if let (Some(request), Some(status)) = (&request, &status) {
        if !same_drain_owner(request, status) || !verified_legacy_image(&request.parent.executable)
        {
            return Err("Не удалось подтвердить владельца прежнего помощника OpenRGB.".into());
        }
        let helper = checked_local_path(&status.helper.executable).map_err(err)?;
        // A newly relaunched old frontend must block the takeover.
        wait_for_legacy_image(
            request.parent.pid,
            &helper,
            Some(request.parent.created),
            Instant::now(),
        )?;
        wait_for_legacy_image(
            status.helper.pid,
            &helper,
            Some(status.helper.created),
            until,
        )?;
        if let Some(pid) = status.backend_pid {
            wait_for_legacy_image(pid, &config.executable, None, until)?;
        }
    } else if request.is_some() || status.is_some() {
        return Err("Статус прежнего помощника OpenRGB неполный. Его выход не подтверждён.".into());
    }
    // Also cover a direct backend or a helper that launched a new child after
    // the last status publication. A different executable may own this port.
    if let Some(pid) = super::managed_backend::listener_owner(config.endpoint)? {
        wait_for_legacy_image(pid, &config.executable, None, until)?;
    }
    Ok(())
}

struct DrainHandle(HANDLE);
impl Drop for DrainHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn wait_for_legacy_image(
    pid: u32,
    expected: &Path,
    creation: Option<u64>,
    until: Instant,
) -> Result<(), String> {
    if pid == 0 {
        return Err("Некорректный идентификатор прежнего процесса Prismestra.".into());
    }
    let process = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if process.is_null() {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            return Ok(()); // No live process with this PID.
        }
        return Err(format!(
            "Не удалось подтвердить выход прежнего процесса Prismestra: {error}"
        ));
    }
    let process = DrainHandle(process);
    let exited = || unsafe { WaitForSingleObject(process.0, 0) } == WAIT_OBJECT_0;
    if exited() {
        return Ok(());
    }
    let mut wide = vec![0; 32_768];
    let mut size = wide.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process.0, 0, wide.as_mut_ptr(), &mut size) } == 0 {
        let error = io::Error::last_os_error();
        return if exited() { Ok(()) } else { Err(err(error)) };
    }
    let image = PathBuf::from(OsString::from_wide(&wide[..size as usize]));
    let image = checked_local_path(&image).map_err(err)?;
    if image != expected {
        return Ok(()); // Stale PID or a different listener.
    }
    if let Some(expected_creation) = creation {
        let mut times = [FILETIME::default(); 4];
        let [created, exited, kernel, user] = &mut times;
        if unsafe { GetProcessTimes(process.0, created, exited, kernel, user) } == 0 {
            return Err(err(io::Error::last_os_error()));
        }
        let actual = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        if actual != expected_creation {
            return Ok(()); // Same image at a recycled process ID.
        }
    }
    let remaining = until.saturating_duration_since(Instant::now());
    match unsafe { WaitForSingleObject(process.0, remaining.as_millis() as u32) } {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => Err(
            "Прежняя Prismestra или её бэкенд OpenRGB ещё работает. Передача вывода отложена."
                .into(),
        ),
        _ => Err(err(io::Error::last_os_error())),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceHint {
    version: u32,
    launcher: PathBuf,
    executable: PathBuf,
    profile: PathBuf,
    state: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasePointer {
    executable: String,
    sha256: String,
    commit: String,
}

fn hinted_source(user_root: &Path, launcher_pin: &str) -> Option<Source> {
    let raw = read_optional(&user_root.join(SOURCE_HINT), 16_384).ok()??;
    let hint: SourceHint =
        serde_json::from_value(super::strict_json::unique_json(&raw, 16_384).ok()?).ok()?;
    if hint.version != 1 {
        return None;
    }
    let launcher = checked_local_path(&hint.launcher).ok()?;
    if !launcher
        .file_name()?
        .eq_ignore_ascii_case("Start Prismestra.ps1")
        || fingerprint(&read_optional(&launcher, MAX_DISCOVERY).ok()??) != launcher_pin
    {
        return None;
    }
    let root = launcher.parent()?;
    let pointer_bytes = read_optional(&root.join("live-release.json"), 4096).ok()??;
    let pointer: ReleasePointer =
        serde_json::from_value(super::strict_json::unique_json(&pointer_bytes, 4096).ok()?).ok()?;
    let parts: Vec<_> = pointer.executable.split('/').collect();
    let ["versions", release, "Prismestra", "Prismestra.exe"] = parts.as_slice() else {
        return None;
    };
    if !(7..=40).contains(&release.len())
        || pointer.commit.len() != 40
        || !pointer.commit.starts_with(release)
        || !pointer
            .commit
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        || !crate::devices::openrgb::valid_hash(&pointer.sha256)
    {
        return None;
    }
    let expected = checked_local_path(&root.join(&pointer.executable)).ok()?;
    let executable = checked_local_path(&hint.executable).ok()?;
    if executable != expected {
        return None;
    }
    let manifest = read_optional(
        &executable.parent()?.join("release-manifest.json"),
        MAX_DISCOVERY,
    )
    .ok()??;
    let manifest = super::strict_json::unique_json(&manifest, MAX_DISCOVERY).ok()?;
    if manifest["product"].as_str()? != "Prismestra"
        || manifest["commit"].as_str()? != pointer.commit
        || manifest["executable_sha256"].as_str()? != pointer.sha256
        || !image_matches(&executable, &pointer.sha256)
    {
        return None;
    }
    let source = Source::checked(&hint.profile, &hint.state).ok()?;
    let expected_source = Source::checked(&root.join("profile.json"), &root.join("state")).ok()?;
    if source != expected_source
        || source.profile.starts_with(user_root)
        || source.state.starts_with(user_root)
    {
        return None;
    }
    let mut confirmed = BTreeSet::new();
    add_known_source(&source.profile, &source.state, &mut confirmed).ok()?;
    confirmed.into_iter().next()
}

fn known_sources(user_root: &Path, launcher_pin: &str) -> Result<BTreeSet<Source>, String> {
    if let Some(source) = hinted_source(user_root, launcher_pin) {
        return Ok(BTreeSet::from([source]));
    }
    let mut candidates = BTreeSet::new();
    for folder in [&FOLDERID_Documents, &FOLDERID_Desktop, &FOLDERID_Downloads] {
        let Some(folder) = known_folder(folder).and_then(|path| checked_local_path(&path).ok())
        else {
            continue;
        };
        let portable = folder.join("Prismestra");
        for data in [portable.clone(), portable.join("data")] {
            add_known_source(
                &data.join("profile.json"),
                &data.join("state"),
                &mut candidates,
            )?;
        }
    }
    if let Some(local) = user_root.parent() {
        let stand = local.join("PcControl/Stand");
        add_known_source(&stand.join("profile.json"), &stand, &mut candidates)?;
    }
    Ok(candidates)
}

fn add_known_source(
    profile: &Path,
    state: &Path,
    candidates: &mut BTreeSet<Source>,
) -> Result<(), String> {
    let Some(raw) = read_optional(profile, document::MAX_BYTES)? else {
        return Ok(());
    };
    let map = Document::decode(&raw)
        .map(|doc| doc.lighting.setup.devices)
        .or_else(|_| Project::decode(&raw).map(|project| project.setup.devices))?;
    if !map.is_empty() {
        candidates.insert(Source::checked(profile, state)?);
    }
    Ok(())
}

fn known_folder(folder: &GUID) -> Option<PathBuf> {
    let mut raw = null_mut();
    let hr =
        unsafe { SHGetKnownFolderPath(folder, KF_FLAG_DONT_VERIFY as u32, null_mut(), &mut raw) };
    if hr < 0 || raw.is_null() {
        return None;
    }
    let path = unsafe { os_string(raw) };
    unsafe { CoTaskMemFree(raw.cast()) };
    Some(path.into())
}

unsafe fn os_string(raw: *const u16) -> OsString {
    let mut size = 0;
    while unsafe { *raw.add(size) } != 0 {
        size += 1;
    }
    OsString::from_wide(unsafe { std::slice::from_raw_parts(raw, size) })
}

struct RunningEntry {
    image: PathBuf,
    command: String,
    pid: u32,
    creation: u64,
}

#[path = "installed_process_inventory.rs"]
mod process_inventory;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LegacyProcess {
    pub pid: u32,
    pub executable: PathBuf,
    pub creation_filetime: u64,
}

/// Recheck only the imported source after committing. The caller must retain and
/// recheck the process handle before using this identity for a graceful handover.
pub(crate) fn active_process_for_migration(root: &Path) -> Result<Option<LegacyProcess>, String> {
    let Some(mut receipt) = read_receipt(root)? else {
        return Ok(None);
    };
    if !receipt.complete {
        return Ok(None);
    }
    let mut candidates = running_candidates()?
        .remove(&receipt.source)
        .unwrap_or_default();
    if candidates.len() > 1 {
        return Err("Multiple active Prismestra sessions use the same legacy source".into());
    }
    let process = candidates.pop();
    if process.is_some() && receipt.record_live_frontend() {
        // Record this before requesting the previous frontend's graceful exit.
        // If our process stops during handover, the next session must recheck the drain.
        write_receipt(root, &receipt)?;
    }
    Ok(process)
}

fn running_sources() -> Result<BTreeSet<Source>, String> {
    Ok(running_candidates()?.into_keys().collect())
}

fn running_candidates() -> Result<BTreeMap<Source, Vec<LegacyProcess>>, String> {
    verified_running_entries(process_inventory::query()?)
}

fn verified_running_entries(
    entries: Vec<RunningEntry>,
) -> Result<BTreeMap<Source, Vec<LegacyProcess>>, String> {
    let owner = super::installed_policy::process_user_sid(unsafe { GetCurrentProcess() })?;
    let mut result = BTreeMap::<Source, Vec<LegacyProcess>>::new();
    for entry in entries {
        if let Some((profile, state)) = legacy_args(&entry.command)
            && let Some(process) = verified_legacy_process(&entry, &owner)
            && let Ok(source) = Source::checked(&profile, &state)
        {
            result.entry(source).or_default().push(process);
        }
    }
    Ok(result)
}

fn verified_legacy_process(entry: &RunningEntry, owner: &str) -> Option<LegacyProcess> {
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            entry.pid,
        )
    };
    if handle.is_null() {
        return None;
    }
    let result = (|| {
        if super::installed_policy::process_user_sid(handle).ok()? != owner {
            return None;
        }
        let expected = checked_local_path(&entry.image).ok()?;
        let mut wide = vec![0; 32_768];
        let mut size = wide.len() as u32;
        if unsafe { QueryFullProcessImageNameW(handle, 0, wide.as_mut_ptr(), &mut size) } == 0 {
            return None;
        }
        let executable =
            checked_local_path(Path::new(&OsString::from_wide(&wide[..size as usize]))).ok()?;
        if executable != expected || !verified_legacy_image(&executable) {
            return None;
        }
        let mut times = [FILETIME::default(); 4];
        let [created, exited, kernel, user] = &mut times;
        if unsafe { GetProcessTimes(handle, created, exited, kernel, user) } == 0
            || unsafe { WaitForSingleObject(handle, 0) } != WAIT_TIMEOUT
        {
            return None;
        }
        let creation_filetime =
            (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        // CIM rounds timestamps while the process handle supplies exact FILETIME.
        if creation_filetime.abs_diff(entry.creation) > 10_000 {
            return None;
        }
        Some(LegacyProcess {
            pid: entry.pid,
            executable,
            creation_filetime,
        })
    })();
    unsafe { CloseHandle(handle) };
    result
}

fn verified_legacy_image(image: &Path) -> bool {
    let Ok(image) = checked_local_path(image) else {
        return false;
    };
    let Some(root) = image.parent() else {
        return false;
    };
    let Ok(Some(manifest)) = read_optional(&root.join("release-manifest.json"), MAX_DISCOVERY)
    else {
        return false;
    };
    let Ok(value) = super::strict_json::unique_json(&manifest, MAX_DISCOVERY) else {
        return false;
    };
    value["product"].as_str() == Some("Prismestra")
        && value["executable_sha256"]
            .as_str()
            .is_some_and(|pin| image_matches(&image, pin))
}

fn legacy_args(command: &str) -> Option<(PathBuf, PathBuf)> {
    let command: Vec<u16> = command.encode_utf16().chain([0]).collect();
    let mut argc = 0;
    let raw = unsafe { CommandLineToArgvW(command.as_ptr(), &mut argc) };
    if raw.is_null() {
        return None;
    }
    let args: Vec<OsString> = unsafe {
        std::slice::from_raw_parts(raw, argc as usize)
            .iter()
            .map(|arg| os_string(*arg))
            .collect()
    };
    unsafe { LocalFree(raw.cast()) };
    if args.is_empty()
        || args
            .iter()
            .any(|arg| arg == "--selftest" || arg == "--snapshot")
    {
        return None;
    }
    let path = |option: &str| {
        let mut matches = args[1..].windows(2).filter(|pair| pair[0] == option);
        let value = PathBuf::from(&matches.next()?[1]);
        (matches.next().is_none() && value.is_absolute()).then_some(value)
    };
    Some((path("--project")?, path("--state-dir")?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::config::Groups,
        devices::{Canvas, Device, Driver, Point, Role, Setup},
    };
    use std::{
        fs,
        io::Read,
        os::windows::process::CommandExt,
        process::{Command, Stdio},
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    #[test]
    #[ignore = "fixture process; launched only by the live migration inventory test"]
    fn legacy_inventory_child() {
        let ready = std::env::var_os("PRISMESTRA_MIGRATION_FIXTURE_READY").unwrap();
        fs::write(ready, b"ready").unwrap();
        let mut byte = [0];
        let _ = io::stdin().read(&mut byte);
    }

    struct InventoryChild(std::process::Child);

    impl Drop for InventoryChild {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(None)) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    #[test]
    fn native_inventory_verifies_live_legacy_source_and_ignores_exited_process() {
        let fixture = Fixture::with_legacy_folder("Старый профиль");
        fixture.populate(&document(true), "127.0.0.1:19424".parse().unwrap());
        let source_root = fixture.source.profile.parent().unwrap();
        let image = source_root.join("Prismestra.exe");
        fs::copy(std::env::current_exe().unwrap(), &image).unwrap();
        fs::write(
            source_root.join("release-manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "product": "Prismestra",
                "executable_sha256": fingerprint(&fs::read(&image).unwrap()),
            }))
            .unwrap(),
        )
        .unwrap();
        let ready = source_root.join("ready");
        let mut child = InventoryChild(
            Command::new(&image)
                .args([
                    "--exact",
                    "app::installed_migration::tests::legacy_inventory_child",
                    "--ignored",
                    "--",
                    "--project",
                ])
                .arg(&fixture.source.profile)
                .arg("--state-dir")
                .arg(&fixture.source.state)
                .env("PRISMESTRA_MIGRATION_FIXTURE_READY", &ready)
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(8);
        while !ready.exists() {
            assert!(child.0.try_wait().unwrap().is_none(), "fixture exited");
            assert!(Instant::now() < deadline, "fixture did not start");
            thread::sleep(Duration::from_millis(10));
        }

        let entries = process_inventory::query().unwrap();
        let entry = entries
            .iter()
            .find(|entry| entry.pid == child.0.id())
            .unwrap();
        let owner =
            super::super::installed_policy::process_user_sid(unsafe { GetCurrentProcess() })
                .unwrap();
        assert!(verified_legacy_process(entry, "S-1-0-0").is_none());
        let stale = RunningEntry {
            image: entry.image.clone(),
            command: entry.command.clone(),
            pid: entry.pid,
            creation: entry.creation - 20_000,
        };
        assert!(verified_legacy_process(&stale, &owner).is_none());
        let mut candidates = verified_running_entries(entries).unwrap();
        let expected = candidates.remove(&fixture.source).unwrap();
        assert_eq!(expected.len(), 1);
        assert_eq!(expected[0].pid, child.0.id());
        assert_eq!(expected[0].executable, image);

        fixture.migrate(true).unwrap();
        assert_eq!(
            active_process_for_migration(&fixture.target)
                .unwrap()
                .as_ref(),
            Some(&expected[0]),
        );
        drop(child.0.stdin.take());
        assert!(child.0.wait().unwrap().success());
        assert_eq!(active_process_for_migration(&fixture.target).unwrap(), None);
    }

    struct Fixture {
        root: PathBuf,
        target: PathBuf,
        source: Source,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_legacy_folder("old")
        }

        fn with_legacy_folder(folder: &str) -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "prismestra-migration-{}-{stamp}",
                std::process::id()
            ));
            let old = root.join(folder);
            let target = root.join("target");
            for path in [old.join("state"), target.join("state")] {
                fs::create_dir_all(path).unwrap();
            }
            let root = root.canonicalize().unwrap();
            let old = root.join(folder);
            Self {
                source: Source {
                    profile: old.join("profile.json"),
                    state: old.join("state"),
                },
                target: root.join("target"),
                root,
            }
        }

        fn migrate(&self, running: bool) -> Result<MigrationOutcome, String> {
            migrate_from(
                &self.target,
                &document(false),
                bundled(),
                &self.source.profile,
                &self.source.state,
                running,
            )
        }

        fn populate(&self, d: &Document, endpoint: SocketAddr) -> Vec<u8> {
            d.save(&self.source.profile).unwrap();
            let binding = Binding::OpenRgb {
                endpoint,
                identity: "a".repeat(64),
                topology: "b".repeat(64),
            };
            let local = Local {
                version: 1,
                devices: BTreeMap::from([("fixture".into(), binding)]),
                monitors: Default::default(),
                door_brightness: 100,
            };
            let bytes = serde_json::to_vec(&local).unwrap();
            fs::write(self.source.state.join(LOCAL), &bytes).unwrap();
            bytes
        }

        fn owned_backend(&self, endpoint: SocketAddr) -> PathBuf {
            let root = self.source.profile.parent().unwrap();
            let component = root.join("components/openrgb/pinned");
            let runtime = component.join("runtime");
            let config_dir = root.join("backend");
            fs::create_dir_all(&runtime).unwrap();
            fs::create_dir(&config_dir).unwrap();
            let image = runtime.join("OpenRGB.exe");
            fs::write(&image, b"owned fixture").unwrap();
            let hash = fingerprint(b"owned fixture");
            fs::write(
                component.join("component-manifest.json"),
                serde_json::to_vec(&serde_json::json!({
                    "version":1,"scope":"private component evaluation; not public release","engine_sha256":hash,
                    "files":[{"path":"runtime/OpenRGB.exe","size":b"owned fixture".len(),"sha256":hash}]
                }))
                .unwrap(),
            )
            .unwrap();
            fs::write(self.source.state.join("managed-openrgb.json"), serde_json::to_vec(&serde_json::json!({
                "version":1,"enabled":true,"graceful_shutdown":true,"endpoint":endpoint,"executable":image,
                "config_dir":config_dir,"expected_executable_sha256":hash
            })).unwrap()).unwrap();
            image
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn bundled() -> SocketAddr {
        "127.0.0.1:17422".parse().unwrap()
    }

    fn write_closed_source_hint(fixture: &Fixture) -> (PathBuf, PathBuf, String) {
        let legacy = fixture.source.profile.parent().unwrap();
        let launcher = legacy.join("Start Prismestra.ps1");
        let wrapper = b"synthetic legacy launcher; never executed";
        fs::write(&launcher, wrapper).unwrap();
        let relative = "versions/aaaaaaa/Prismestra/Prismestra.exe";
        let executable = legacy.join(relative);
        let release = executable.parent().unwrap();
        fs::create_dir_all(release).unwrap();
        let image = b"synthetic release image; never executed";
        fs::write(&executable, image).unwrap();
        let commit = "a".repeat(40);
        let sha256 = fingerprint(image);
        fs::write(
            legacy.join("live-release.json"),
            serde_json::to_vec(&serde_json::json!({
                "executable":relative, "sha256":sha256, "commit":commit
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            release.join("release-manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "product":"Prismestra", "executable_sha256":sha256, "commit":commit
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            fixture.target.join(SOURCE_HINT),
            serde_json::to_vec(&serde_json::json!({
                "version":1, "launcher":launcher, "executable":executable,
                "profile":fixture.source.profile, "state":fixture.source.state
            }))
            .unwrap(),
        )
        .unwrap();
        (launcher, executable, fingerprint(wrapper))
    }

    #[test]
    fn verified_hint_finds_a_closed_source_outside_standard_locations() {
        let fixture = Fixture::new();
        document(true).save(&fixture.source.profile).unwrap();
        let (launcher, executable, pin) = write_closed_source_hint(&fixture);
        assert_eq!(
            known_sources(&fixture.target, &pin).unwrap(),
            BTreeSet::from([fixture.source.clone()])
        );
        fs::write(&executable, b"different bytes").unwrap();
        assert!(hinted_source(&fixture.target, &pin).is_none());
        fs::write(&executable, b"synthetic release image; never executed").unwrap();
        fs::write(&launcher, b"unrecognized wrapper").unwrap();
        assert!(hinted_source(&fixture.target, &pin).is_none());
    }

    #[test]
    fn fresh_install_does_not_wait_for_a_legacy_backend() {
        let fixture = Fixture::new();
        wait_for_migrated_backend(&fixture.target).unwrap();
        assert!(!fixture.target.join(RECEIPT).exists());
    }

    #[test]
    fn confirmed_drain_survives_removed_source_but_reappearing_frontend_requires_recheck() {
        let fixture = Fixture::new();
        let mut receipt = Receipt {
            version: 1,
            source: fixture.source.clone(),
            before: BTreeMap::new(),
            after: BTreeMap::new(),
            complete: true,
            source_running: false,
            handover_completed: true,
            used_bundled_backend: true,
            external_uses_bundled_endpoint: false,
        };
        fs::remove_dir(&fixture.source.state).unwrap();
        write_receipt(&fixture.target, &receipt).unwrap();
        wait_for_migrated_backend(&fixture.target).unwrap();
        assert!(receipt.record_live_frontend());
        assert!(!receipt.record_live_frontend());
        write_receipt(&fixture.target, &receipt).unwrap();
        assert!(wait_for_migrated_backend(&fixture.target).is_err());
        assert!(
            !read_receipt(&fixture.target)
                .unwrap()
                .unwrap()
                .handover_completed
        );

        // A source that had no bundled backend can finish after its verified
        // frontend drains, without expecting managed OpenRGB control files.
        receipt.used_bundled_backend = false;
        write_receipt(&fixture.target, &receipt).unwrap();
        wait_for_migrated_backend(&fixture.target).unwrap();
        let committed = read_receipt(&fixture.target).unwrap().unwrap();
        assert!(committed.handover_completed);
        assert!(!committed.source_running);
    }

    #[test]
    fn drain_requires_matching_parent_generation_and_helper_image() {
        let identity = serde_json::json!({
            "pid":99, "created":123456, "executable":"C:/fixture/Prismestra.exe"
        });
        let request = serde_json::json!({
            "version":2,"clock_domain":"windows-unbiased-interrupt-ms-v1",
            "nonce":"99-1-0","parent":identity,"timestamp_ms":123
        });
        let request: DrainRequest = serde_json::from_value(request).unwrap();
        let status = serde_json::json!({
            "version":2,"clock_domain":"windows-unbiased-interrupt-ms-v1",
            "nonce":"99-1-0","parent":identity,
            "helper":{"pid":100,"created":123457,"executable":"C:/fixture/Prismestra.exe"},
            "backend_pid":101,"timestamp_ms":456,"ready":true,"ready_generation":1
        });
        let accepted: DrainStatus = serde_json::from_value(status.clone()).unwrap();
        assert!(same_drain_owner(&request, &accepted));
        for (path, changed) in [
            ("/nonce", serde_json::json!("old-owner")),
            ("/parent/created", serde_json::json!(123457)),
            ("/helper/pid", serde_json::json!(99)),
            (
                "/helper/executable",
                serde_json::json!("C:/foreign/OpenRGB.exe"),
            ),
            ("/clock_domain", serde_json::json!("wall-clock")),
        ] {
            let mut value = status.clone();
            *value.pointer_mut(path).unwrap() = changed;
            let disputed: DrainStatus = serde_json::from_value(value).unwrap();
            assert!(!same_drain_owner(&request, &disputed), "{path}");
        }
    }

    fn document(has_device: bool) -> Document {
        let mut document = Document::from_lighting(Project {
            version: 1,
            setup: Setup {
                version: 1,
                canvas: Canvas {
                    width: 14,
                    height: 8,
                    points: vec![],
                },
                devices: vec![],
            },
            scene: crate::runtime::scene::example(),
            schedule: Default::default(),
            groups: Groups {
                pc: vec![],
                door: vec![],
            },
        })
        .unwrap();
        if has_device {
            document.lighting.setup.devices.push(Device {
                id: "fixture".into(),
                label: "Known output".into(),
                role: Role::Canvas,
                driver: Driver::OpenRgb,
                leds: 1,
                rate_hz: 20,
                enabled: true,
            });
            document.lighting.setup.canvas.points.push(Point {
                device: "fixture".into(),
                led: 0,
                x: 9,
                y: 7,
            });
            document.lighting.groups.pc.push("fixture".into());
        }
        document.validate().unwrap();
        document
    }

    #[test]
    fn migrates_exact_saved_map_and_external_bindings_into_existing_empty_profile() {
        let fixture = Fixture::new();
        let doc = document(true);
        let previous = fixture.populate(&doc, "127.0.0.1:6742".parse().unwrap());
        document(false)
            .save(&fixture.target.join("profile.json"))
            .unwrap();
        let result = fixture.migrate(true).unwrap();
        assert_eq!(
            result,
            MigrationOutcome::Imported {
                source_running: true,
                used_bundled_backend: false,
                external_uses_bundled_endpoint: false
            }
        );
        assert_eq!(
            Document::load(&fixture.target.join("profile.json")).unwrap(),
            doc
        );
        assert_eq!(
            fs::read(fixture.target.join("state").join(LOCAL)).unwrap(),
            previous
        );
        assert_eq!(
            fs::read(fixture.source.state.join(LOCAL)).unwrap(),
            previous
        );
    }

    #[test]
    fn changes_endpoint_only_for_pinned_source_owned_component() {
        let fixture = Fixture::new();
        let source_sdk = "127.0.0.1:6742".parse().unwrap();
        let doc = document(true);
        let previous = fixture.populate(&doc, source_sdk);
        let image = fixture.owned_backend(source_sdk);
        assert_eq!(owned_backend_endpoint(&fixture.source), Some(source_sdk));
        fs::write(&image, b"unowned").unwrap();
        assert_eq!(owned_backend_endpoint(&fixture.source), None);
        fs::write(&image, b"owned fixture").unwrap();
        assert_eq!(
            fixture.migrate(false).unwrap(),
            MigrationOutcome::Imported {
                source_running: false,
                used_bundled_backend: true,
                external_uses_bundled_endpoint: false
            }
        );
        let local = Local::read(&fixture.target.join("state").join(LOCAL), &doc).unwrap();
        assert!(
            matches!(local.devices["fixture"], Binding::OpenRgb { endpoint, .. } if endpoint == bundled())
        );
        assert_eq!(
            fs::read(fixture.source.state.join(LOCAL)).unwrap(),
            previous
        );
    }

    #[test]
    fn preserves_an_edited_device_free_target_and_reports_external_port_collision() {
        let fixture = Fixture::new();
        fixture.populate(&document(true), bundled());
        let mut altered = document(false);
        altered.lighting.scene.brightness = 7;
        altered.save(&fixture.target.join("profile.json")).unwrap();
        assert_eq!(fixture.migrate(false).unwrap(), MigrationOutcome::None);
        assert_eq!(
            Document::load(&fixture.target.join("profile.json")).unwrap(),
            altered
        );
        document(false)
            .save(&fixture.target.join("profile.json"))
            .unwrap();
        assert_eq!(
            fixture.migrate(false).unwrap(),
            MigrationOutcome::Imported {
                source_running: false,
                used_bundled_backend: false,
                external_uses_bundled_endpoint: true
            }
        );
    }

    #[test]
    fn strict_candidate_selection_never_guesses_between_different_plans() {
        let fixture = Fixture::new();
        assert_eq!(unique(BTreeSet::new()).unwrap(), None);
        assert!(
            unique(BTreeSet::from([
                fixture.source.clone(),
                Source {
                    profile: fixture.root.join("different.json"),
                    state: fixture.source.state.clone(),
                }
            ]))
            .is_err()
        );
    }

    #[test]
    fn migrates_referenced_assets_and_sensor_catalog_without_copying_authority_files() {
        let fixture = Fixture::new();
        let mut doc = document(true);
        doc.lighting.setup.devices.push(Device {
            id: "lcd".into(),
            label: "Saved LCD".into(),
            role: Role::Lcd,
            driver: Driver::TtLcd,
            leds: 0,
            rate_hz: 25,
            enabled: false,
        });
        doc.lighting.groups.pc.push("lcd".into());
        let media = b"Verified referenced fixture media";
        let source_assets = Store::new(fixture.source.state.join("assets")).unwrap();
        let media_id = source_assets.put(media, assets::MAX_BYTES).unwrap();
        doc.lcd.push(document::Lcd {
            device: "lcd".into(),
            config: crate::app::lcd::Config {
                media: Some(media_id.clone()),
                ..Default::default()
            },
        });
        fixture.populate(&doc, "127.0.0.1:6742".parse().unwrap());
        let sensors = sensor_bindings::Catalog::default().encode().unwrap();
        fs::write(fixture.source.state.join("sensor-sources.json"), &sensors).unwrap();
        fs::write(
            fixture.source.state.join("backend-task.json"),
            b"legacy authority",
        )
        .unwrap();
        fixture.migrate(false).unwrap();
        let installed = fixture.target.join("state");
        assert_eq!(
            Store::new(installed.join("assets"))
                .unwrap()
                .read(&media_id, assets::MAX_BYTES)
                .unwrap(),
            media
        );
        assert_eq!(
            fs::read(installed.join("sensor-sources.json")).unwrap(),
            sensors
        );
        assert!(!installed.join("backend-task.json").exists());

        let mut pending = read_receipt(&fixture.target).unwrap().unwrap();
        pending.complete = false;
        write_receipt(&fixture.target, &pending).unwrap();
        fs::remove_file(&fixture.source.profile).unwrap();
        let retry = || {
            let pending = read_receipt(&fixture.target).unwrap().unwrap();
            apply(
                &fixture.target,
                pending.source.clone(),
                bundled(),
                false,
                Some(pending),
            )
        };

        // A final receipt cannot be recovered if a target differs from its pin.
        let installed_catalog = installed.join("sensor-sources.json");
        fs::write(&installed_catalog, b"{}").unwrap();
        assert!(retry().is_err());
        assert_eq!(fs::read(&installed_catalog).unwrap(), b"{}");
        assert!(!read_receipt(&fixture.target).unwrap().unwrap().complete);
        fs::write(&installed_catalog, sensors).unwrap();

        let installed_asset = installed.join("assets").join(media_id.as_str());
        fs::remove_file(&installed_asset).unwrap();
        assert!(retry().is_err());
        assert!(!read_receipt(&fixture.target).unwrap().unwrap().complete);
        fs::write(&installed_asset, media).unwrap();

        assert!(matches!(
            retry().unwrap(),
            MigrationOutcome::Imported { .. }
        ));
        let completed = read_receipt(&fixture.target).unwrap().unwrap();
        assert!(completed.complete);
        assert!(!completed.handover_completed);
        assert_eq!(completed.after, pending.after);
    }

    fn installed_temperature_scope_fixture() -> (
        Fixture,
        Document,
        Vec<String>,
        sensor_bindings::Scope,
        sensor_bindings::Scope,
        sensor_bindings::Catalog,
    ) {
        use crate::app::{
            sensor_bindings::{Catalog as SensorCatalog, Scope, Target},
            sensor_pci::Pci,
            sensors::{Source as TemperatureSource, Value},
        };
        let fixture = Fixture::with_legacy_folder("Старый профиль");
        let mut doc = document(true);
        let inputs = (0..5).map(|i| format!("sensor_{i}")).collect::<Vec<_>>();
        for input in &inputs {
            doc.temperatures.sources.push(TemperatureSource {
                key: format!("view_{input}"),
                value: Value::Sensor {
                    input: input.clone(),
                },
            });
        }
        fixture.populate(&doc, "127.0.0.1:6742".parse().unwrap());
        let old_identity = fixture.source.profile.to_string_lossy();
        let destination = fixture.target.join("profile.json");
        let new_identity = destination.to_string_lossy();
        let old_scope = Scope::lighting(&old_identity, &doc).unwrap();
        let new_scope = Scope::lighting(&new_identity, &doc).unwrap();
        let mut catalog = SensorCatalog::default();
        catalog
            .set_source(
                "physical/test-nvidia",
                Target::Nvidia {
                    pci: Pci {
                        device_id: 0x280310de,
                        subsystem_id: 0x12345678,
                        revision_id: 0xa1,
                        extended_device_id: 0x77,
                    },
                },
            )
            .unwrap();
        (fixture, doc, inputs, old_scope, new_scope, catalog)
    }

    #[test]
    fn installed_migration_rebases_only_the_exact_temperature_scope_from_closed_source_hint() {
        use sensor_bindings::{Catalog as SensorCatalog, Scope};
        let (fixture, doc, inputs, old_scope, new_scope, mut catalog) =
            installed_temperature_scope_fixture();
        let old_identity = fixture.source.profile.to_string_lossy();
        let foreign = Scope::lighting("another-profile", &doc).unwrap();
        let mut stale_doc = doc.clone();
        stale_doc.temperatures.sources.pop();
        let stale = Scope::lighting(&old_identity, &stale_doc).unwrap();
        assert_ne!(old_scope, new_scope);
        assert_ne!(old_scope, stale);
        let routes = inputs
            .iter()
            .map(|input| (input.clone(), "physical/test-nvidia".into()))
            .collect::<Vec<_>>();
        catalog.set_routes(&old_scope, &routes).unwrap();
        catalog.set_routes(&foreign, &routes[..1]).unwrap();
        catalog.set_routes(&stale, &routes[..4]).unwrap();
        let expected = catalog.prepare(&old_scope, &inputs).unwrap();
        assert_eq!(expected.len(), 5);
        assert!(catalog.prepare(&stale, &inputs).is_err());
        assert!(catalog.prepare(&new_scope, &inputs).is_err());
        let foreign_routes = catalog.prepare(&foreign, &inputs[..1]).unwrap();
        let original = catalog.encode().unwrap();
        let source_path = fixture.source.state.join("sensor-sources.json");
        fs::write(&source_path, &original).unwrap();

        let (_, _, pin) = write_closed_source_hint(&fixture);
        let source = unique(known_sources(&fixture.target, &pin).unwrap())
            .unwrap()
            .expect("the original-user hint must find the closed portable profile");
        assert_eq!(source, fixture.source);
        let profile = fs::read(&source.profile).unwrap();
        let local = fs::read(source.state.join(LOCAL)).unwrap();
        let migrate = || {
            migrate_from(
                &fixture.target,
                &document(false),
                bundled(),
                &source.profile,
                &source.state,
                false,
            )
            .unwrap()
        };
        assert!(matches!(
            migrate(),
            MigrationOutcome::Imported {
                source_running: false,
                used_bundled_backend: false,
                external_uses_bundled_endpoint: false,
            }
        ));
        assert_eq!(fs::read(&source.profile).unwrap(), profile);
        assert_eq!(fs::read(source.state.join(LOCAL)).unwrap(), local);
        assert_eq!(
            fs::read(fixture.target.join("profile.json")).unwrap(),
            profile
        );
        assert_eq!(
            fs::read(fixture.target.join("state").join(LOCAL)).unwrap(),
            local
        );
        let installed_path = fixture.target.join("state/sensor-sources.json");
        let installed = SensorCatalog::load(&installed_path).unwrap();
        assert_eq!(fs::read(&source_path).unwrap(), original);
        assert_eq!(installed.sources, catalog.sources);
        assert_eq!(
            installed.prepare(&foreign, &inputs[..1]).unwrap(),
            foreign_routes
        );
        assert!(installed.prepare(&stale, &inputs).is_err());
        let actual = installed
            .prepare(&new_scope, &inputs)
            .expect("five verified routes must resolve for the installed profile path");
        assert_eq!(actual, expected);
        let migrated_bytes = fs::read(&installed_path).unwrap();
        let receipt = read_receipt(&fixture.target).unwrap().unwrap();
        assert!(receipt.complete);
        assert_eq!(receipt.source, source);
        assert_eq!(receipt.after["profile.json"], fingerprint(&profile));
        assert_eq!(
            receipt.after[&format!("state/{LOCAL}")],
            fingerprint(&local)
        );
        assert_eq!(
            receipt.after["state/sensor-sources.json"],
            fingerprint(&migrated_bytes)
        );
        let receipt_path = fixture.target.join(RECEIPT);
        let receipt_bytes = fs::read(&receipt_path).unwrap();
        assert!(matches!(migrate(), MigrationOutcome::None));
        assert_eq!(fs::read(&receipt_path).unwrap(), receipt_bytes);
        assert_eq!(fs::read(&installed_path).unwrap(), migrated_bytes);
    }

    #[test]
    fn pending_preview6_receipt_rebases_temperature_routes_without_overwriting_edits() {
        let (fixture, _, inputs, old, new, mut catalog) = installed_temperature_scope_fixture();
        let routes = inputs
            .iter()
            .map(|input| (input.clone(), "physical/test-nvidia".into()))
            .collect::<Vec<_>>();
        catalog.set_routes(&old, &routes).unwrap();
        let old_catalog = catalog.encode().unwrap();
        let source_catalog = fixture.source.state.join("sensor-sources.json");
        let target_catalog = fixture.target.join(INSTALLED_SENSOR_CATALOG);
        fs::write(&source_catalog, &old_catalog).unwrap();
        fixture.migrate(false).unwrap();

        // Restore the receipt and files a previous version would have
        // written just before the final receipt update.
        let mut receipt = read_receipt(&fixture.target).unwrap().unwrap();
        receipt.complete = false;
        receipt
            .after
            .insert(INSTALLED_SENSOR_CATALOG.into(), fingerprint(&old_catalog));
        write_receipt(&fixture.target, &receipt).unwrap();
        let receipt_path = fixture.target.join(RECEIPT);
        let original_receipt = fs::read(&receipt_path).unwrap();
        fs::write(&target_catalog, &old_catalog).unwrap();

        let moved = fixture.source.profile.with_extension("offline");
        fs::rename(&fixture.source.profile, &moved).unwrap();
        assert!(
            apply(
                &fixture.target,
                fixture.source.clone(),
                bundled(),
                false,
                read_receipt(&fixture.target).unwrap()
            )
            .unwrap_err()
            .contains("temperature routes")
        );
        assert_eq!(fs::read(&receipt_path).unwrap(), original_receipt);
        assert_eq!(fs::read(&target_catalog).unwrap(), old_catalog);
        fs::rename(moved, &fixture.source.profile).unwrap();

        let changed = sensor_bindings::Catalog::default().encode().unwrap();
        fs::write(&target_catalog, &changed).unwrap();
        assert!(fixture.migrate(false).is_err());
        assert_eq!(fs::read(&target_catalog).unwrap(), changed);
        assert_eq!(fs::read(&receipt_path).unwrap(), original_receipt);
        fs::write(&target_catalog, &old_catalog).unwrap();

        assert!(matches!(
            fixture.migrate(false).unwrap(),
            MigrationOutcome::Imported { .. }
        ));
        let rebased = sensor_bindings::Catalog::load(&target_catalog).unwrap();
        assert_eq!(
            rebased.prepare(&new, &inputs).unwrap(),
            catalog.prepare(&old, &inputs).unwrap()
        );
        assert_eq!(fs::read(source_catalog).unwrap(), old_catalog);
        let completed = read_receipt(&fixture.target).unwrap().unwrap();
        assert!(completed.complete);
        let rebased_bytes = fs::read(&target_catalog).unwrap();
        assert_eq!(
            completed.after[INSTALLED_SENSOR_CATALOG],
            fingerprint(&rebased_bytes)
        );
        assert_eq!(
            completed.before[INSTALLED_SENSOR_CATALOG],
            Some(fingerprint(&old_catalog))
        );

        // A second interruption can leave the newer receipt on disk while
        // the installed catalog still contains the old, verified bytes.
        let mut interrupted = completed;
        interrupted.complete = false;
        write_receipt(&fixture.target, &interrupted).unwrap();
        fs::write(&target_catalog, &old_catalog).unwrap();
        assert!(matches!(
            fixture.migrate(false).unwrap(),
            MigrationOutcome::Imported { .. }
        ));
        let resumed = read_receipt(&fixture.target).unwrap().unwrap();
        assert!(resumed.complete);
        assert_eq!(resumed.before, interrupted.before);
        assert_eq!(resumed.after, interrupted.after);
        assert_eq!(fs::read(target_catalog).unwrap(), rebased_bytes);
    }

    #[test]
    fn sensor_scope_accepts_dos_path_but_preserves_conflicting_scopes() {
        use sensor_bindings::{Catalog as SensorCatalog, Scope};
        let (fixture, doc, inputs, old, new, mut catalog) = installed_temperature_scope_fixture();
        let source = fixture.source.profile.to_str().unwrap();
        let dos = Scope::lighting(source.strip_prefix(r"\\?\").unwrap(), &doc).unwrap();
        let installed = fixture.target.join("profile.json");
        let routes = inputs
            .iter()
            .map(|input| (input.clone(), "physical/test-nvidia".into()))
            .collect::<Vec<_>>();
        catalog.set_routes(&dos, &routes).unwrap();
        let original = catalog.encode().unwrap();
        let migrated =
            rebase_sensor_scope(original, &fixture.source.profile, &installed, &doc).unwrap();
        assert_eq!(
            SensorCatalog::decode(&migrated)
                .unwrap()
                .prepare(&new, &inputs),
            catalog.prepare(&dos, &inputs)
        );
        catalog.set_routes(&old, &routes[..1]).unwrap();
        let ambiguous = catalog.encode().unwrap();
        assert_eq!(
            rebase_sensor_scope(ambiguous.clone(), &fixture.source.profile, &installed, &doc)
                .unwrap(),
            ambiguous
        );
        catalog.set_routes(&new, &routes[..1]).unwrap();
        let existing = catalog.encode().unwrap();
        assert_eq!(
            rebase_sensor_scope(existing.clone(), &fixture.source.profile, &installed, &doc)
                .unwrap(),
            existing
        );
    }

    #[test]
    fn installed_migration_does_not_rebase_invalid_temperature_routes() {
        use sensor_bindings::Catalog as SensorCatalog;
        for incomplete in [true, false] {
            let (fixture, _, inputs, old_scope, new_scope, mut catalog) =
                installed_temperature_scope_fixture();
            let mut routes = inputs
                .iter()
                .map(|input| (input.clone(), "physical/test-nvidia".into()))
                .collect::<Vec<_>>();
            if incomplete {
                routes.pop();
            } else {
                routes.push(("extra_input".into(), "physical/test-nvidia".into()));
            }
            catalog.set_routes(&old_scope, &routes).unwrap();
            assert!(catalog.prepare(&old_scope, &inputs).is_err());
            let source = fixture.source.state.join("sensor-sources.json");
            let bytes = catalog.encode().unwrap();
            fs::write(&source, &bytes).unwrap();
            fixture.migrate(false).unwrap();
            assert_eq!(fs::read(&source).unwrap(), bytes);
            let installed =
                SensorCatalog::load(&fixture.target.join("state/sensor-sources.json")).unwrap();
            assert!(
                installed
                    .routes
                    .iter()
                    .all(|route| route.scope != new_scope)
            );
            assert!(installed.prepare(&new_scope, &inputs).is_err());
        }
    }

    #[test]
    fn keeps_legacy_and_versioned_scene_drafts_during_installation() {
        for versioned in [false, true] {
            let fixture = Fixture::new();
            let doc = document(true);
            fixture.populate(&doc, "127.0.0.1:6742".parse().unwrap());
            let mut edited = doc.lighting.scene.clone();
            edited.brightness = 37;
            let value = if versioned {
                serde_json::json!({
                    "version": 1,
                    "scene": edited,
                    "origin": {"id": "current", "name": "Saved", "scene": doc.lighting.scene}
                })
            } else {
                serde_json::to_value(edited).unwrap()
            };
            let bytes = serde_json::to_vec(&value).unwrap();
            let name = "editor-scene-draft.json";
            fs::write(fixture.source.state.join(name), &bytes).unwrap();
            fixture.migrate(false).unwrap();
            assert_eq!(
                fs::read(fixture.target.join("state").join(name)).unwrap(),
                bytes
            );
        }
    }

    #[test]
    fn extracts_only_absolute_unambiguous_source_paths_from_windows_argv() {
        let (profile, state) = legacy_args(r#""C:\Program Files\Prismestra.exe" --state-dir "C:\old tree\state" --project "C:\old tree\profile.json" --start-connected"#).unwrap();
        assert_eq!(profile, Path::new(r"C:\old tree\profile.json"));
        assert_eq!(state, Path::new(r"C:\old tree\state"));
        assert!(
            legacy_args(r"Prismestra.exe --state-dir C:\state --project relative.json").is_none()
        );
        assert!(
            legacy_args(
                r"Prismestra.exe --state-dir C:\state --project C:\a.json --project C:\b.json"
            )
            .is_none()
        );
    }
}
