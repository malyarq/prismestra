//! Local sensor descriptors and exact-scope resolution only; validation acquires no authority.
//! Modules are not read/pin-checked here. Only explicit save/load performs preference-file I/O.
use super::{ambient, document::Document, sensor_pci::Pci, strict_json};
use crate::{
    lcd::hid_policy,
    sensors::{amd, ddr5, nvidia},
    storage,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::Path,
};

pub const MAX_SOURCES: usize = 256;
pub const MAX_ROUTES: usize = 512;
pub const MAX_BYTES: usize = 256 * 1024;
fn identity(text: &str) -> Result<(), String> {
    if text.is_empty() || text.len() > 2048 || text.chars().any(char::is_control) {
        Err("Sensor identities require 1..2048 bytes without control characters".into())
    } else {
        Ok(())
    }
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn normalize_path(path: &str) -> String {
    path.replace('/', "\\").to_ascii_lowercase()
}
fn module_path(path: &str) -> Result<(), String> {
    let bytes = path.as_bytes();
    if !(4..=32760).contains(&bytes.len())
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || !matches!(bytes[2], b'/' | b'\\')
        || path.chars().any(char::is_control)
        || path[2..].contains([':', '<', '>', '"', '|', '?', '*'])
    {
        return Err("Sensor module requires an absolute local drive path without device/stream/wildcard aliases".into());
    }
    for part in path[3..].split(['/', '\\']) {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        let reserved = matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
        ) || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        });
        if part.is_empty() || matches!(part, "." | "..") || part.ends_with([' ', '.']) || reserved {
            return Err(
                "Sensor module path contains an empty, reserved or aliased component".into(),
            );
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    FanControl {
        pipe_name: String,
        expected_server: String,
        sensor_id: String,
    },
    Nvidia {
        pci: Pci,
    },
    Amd {
        vendor: [u8; 12],
        signature: u32,
        packages: u32,
        module_path: String,
    },
    Ddr5 {
        controller: [u64; 3],
        port: u8,
        address: u8,
        module_path: String,
    },
    Liquid {
        explicit_mi00_path: String,
    },
}
impl Target {
    pub fn nvidia_binding(&self) -> Result<nvidia::PciIdentity, String> {
        match self {
            Self::Nvidia { pci } => pci.identity(),
            _ => Err("Target is not NVIDIA".into()),
        }
    }
    pub fn amd_binding(&self) -> Result<amd::CpuIdentity, String> {
        match self {
            Self::Amd {
                vendor,
                signature,
                packages,
                ..
            } => {
                let value = amd::CpuIdentity {
                    vendor: *vendor,
                    signature: *signature,
                    packages: *packages,
                };
                value.validate()?;
                Ok(value)
            }
            _ => Err("Target is not AMD".into()),
        }
    }
    pub fn ddr5_binding(&self) -> Result<ddr5::Binding, String> {
        match self {
            Self::Ddr5 {
                controller,
                port,
                address,
                ..
            } => {
                let value = ddr5::Binding {
                    controller: *controller,
                    port: *port,
                    address: *address,
                };
                value.validate()?;
                Ok(value)
            }
            _ => Err("Target is not DDR5".into()),
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::FanControl {
                pipe_name,
                expected_server,
                sensor_id,
            } => {
                let tail = pipe_name
                    .strip_prefix(r"\\.\pipe\")
                    .ok_or("Only a local FanControl pipe is allowed")?;
                if tail.is_empty() || tail.contains(['\\', '/']) || pipe_name.len() > 256 {
                    return Err("Invalid FanControl pipe name".into());
                }
                identity(pipe_name)?;
                identity(sensor_id)?;
                module_path(expected_server)
            }
            Self::Nvidia { .. } => self.nvidia_binding().map(|_| ()),
            Self::Amd {
                module_path: path, ..
            } => {
                self.amd_binding()?;
                module_path(path)
            }
            Self::Ddr5 {
                module_path: path, ..
            } => {
                self.ddr5_binding()?;
                module_path(path)
            }
            Self::Liquid {
                explicit_mi00_path: path,
            } => {
                if path.len() > 4096 {
                    return Err("Liquid interface path exceeds its bound".into());
                }
                hid_policy::validate_path(hid_policy::Kind::Liquid, path)
            }
        }
    }
    /// Exactly the existing sensor_sources endpoint keys, excluding descriptor/module aliases.
    pub fn resource_keys(&self) -> Result<Vec<String>, String> {
        self.validate()?;
        Ok(vec![match self {
            // The cache RPC is read-only; each exact selected sensor owns its session.
            Self::FanControl {
                pipe_name,
                sensor_id,
                ..
            } => format!(
                "sensor:fancontrol:{}",
                hash(
                    &serde_json::to_vec(&(pipe_name.to_ascii_lowercase(), sensor_id))
                        .map_err(|e| e.to_string())?
                )
            ),
            Self::Nvidia { pci } => format!(
                "sensor:nvapi:{:08x}:{:08x}:{:08x}:{:08x}",
                pci.device_id, pci.subsystem_id, pci.revision_id, pci.extended_device_id
            ),
            Self::Amd { .. } => "sensor:amd-smn:primary".into(),
            Self::Ddr5 {
                controller,
                port,
                address,
                ..
            } => format!(
                "sensor:ddr5:{:016x}:{:016x}:{:016x}:{port}:{address:02x}",
                controller[0], controller[1], controller[2]
            ),
            Self::Liquid { explicit_mi00_path } => {
                format!("hid:{}", explicit_mi00_path.to_ascii_lowercase())
            }
        }])
    }
    /// Stable descriptor identity. Same endpoint with a different module/descriptor is a conflict.
    pub fn canonical_key(&self) -> Result<String, String> {
        self.validate()?;
        let mut normalized = self.clone();
        match &mut normalized {
            Self::FanControl {
                pipe_name,
                expected_server,
                ..
            } => {
                *pipe_name = pipe_name.to_ascii_lowercase();
                *expected_server = normalize_path(expected_server);
            }
            Self::Amd { module_path, .. } | Self::Ddr5 { module_path, .. } => {
                *module_path = normalize_path(module_path)
            }
            Self::Liquid { explicit_mi00_path } => {
                *explicit_mi00_path = explicit_mi00_path.to_ascii_lowercase()
            }
            Self::Nvidia { .. } => {}
        }
        let value = serde_json::to_value(normalized).map_err(|error| error.to_string())?;
        Ok(hash(
            &serde_json::to_vec(&value).map_err(|error| error.to_string())?,
        ))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub revision: u64,
    pub target: Target,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Scope {
    Lighting { identity: String, digest: String },
}
impl Scope {
    pub fn lighting(name: &str, document: &Document) -> Result<Self, String> {
        identity(name)?;
        document.temperatures.validate()?;
        let bytes = serde_json::to_vec(&(name, &document.temperatures))
            .map_err(|error| error.to_string())?;
        Ok(Self::Lighting {
            identity: name.into(),
            digest: hash(&bytes),
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        let (name, digest) = match self {
            Self::Lighting { identity, digest } => (identity, digest),
        };
        identity(name)?;
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Sensor scope digest must contain exactly 64 hex characters".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub scope: Scope,
    pub input: String,
    pub source: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub version: u32,
    pub sources: Vec<Source>,
    pub routes: Vec<Route>,
}
impl Default for Catalog {
    fn default() -> Self {
        Self {
            version: 1,
            sources: vec![],
            routes: vec![],
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub input: String,
    pub source_id: String,
    pub source_revision: u64,
    pub target: Target,
}
impl Catalog {
    /// Edit preferences atomically. This never resolves a provider or starts hardware I/O.
    pub fn set_source(&mut self, id: &str, target: Target) -> Result<(), String> {
        identity(id)?;
        target.validate()?;
        let mut next = self.clone();
        if let Some(source) = next.sources.iter_mut().find(|source| source.id == id) {
            if source.target.canonical_key()? == target.canonical_key()? {
                return self.validate();
            }
            source.revision = source
                .revision
                .checked_add(1)
                .ok_or("Sensor source revision exhausted")?;
            source.target = target;
        } else {
            next.sources.push(Source {
                id: id.into(),
                revision: 1,
                target,
            });
        }
        next.validate()?;
        // A descriptor replacement must not introduce conflicts into any saved scope.
        for scope in next
            .routes
            .iter()
            .map(|route| &route.scope)
            .collect::<BTreeSet<_>>()
        {
            let inputs = next
                .routes
                .iter()
                .filter(|route| &route.scope == scope)
                .map(|route| route.input.clone())
                .collect::<Vec<_>>();
            next.prepare(scope, &inputs)?;
        }
        *self = next;
        Ok(())
    }
    /// Replace only this exact document/profile scope; other saved scopes remain intact.
    pub fn set_routes(&mut self, scope: &Scope, inputs: &[(String, String)]) -> Result<(), String> {
        scope.validate()?;
        let mut next = self.clone();
        next.routes.retain(|route| &route.scope != scope);
        next.routes
            .extend(inputs.iter().map(|(input, source)| Route {
                scope: scope.clone(),
                input: input.clone(),
                source: source.clone(),
            }));
        next.prepare(
            scope,
            &inputs
                .iter()
                .map(|(input, _)| input.clone())
                .collect::<Vec<_>>(),
        )?;
        *self = next;
        Ok(())
    }
    pub fn remove_source(&mut self, id: &str) -> Result<(), String> {
        self.validate()?;
        if self.routes.iter().any(|route| route.source == id) {
            return Err("Release the source's saved routes before removing it".into());
        }
        let index = self
            .sources
            .iter()
            .position(|source| source.id == id)
            .ok_or("Unknown sensor source")?;
        self.sources.remove(index);
        Ok(())
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.sources.len() > MAX_SOURCES || self.routes.len() > MAX_ROUTES {
            return Err("Unsupported sensor catalog version or count limit".into());
        }
        let mut sources = BTreeSet::new();
        for source in &self.sources {
            identity(&source.id)?;
            if source.revision == 0 || !sources.insert(source.id.as_str()) {
                return Err("Sensor source revision must be nonzero and IDs unique".into());
            }
            source.target.validate()?;
        }
        let mut routes = BTreeSet::new();
        for route in &self.routes {
            route.scope.validate()?;
            if !ambient::key(&route.input)
                || !sources.contains(route.source.as_str())
                || !routes.insert((&route.scope, &route.input))
            {
                return Err("Invalid, duplicate or unbound sensor catalog route".into());
            }
        }
        if serde_json::to_vec(self)
            .map_err(|error| error.to_string())?
            .len()
            > MAX_BYTES
        {
            return Err("Sensor catalog exceeds 256 KiB".into());
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|error| error.to_string())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let mut value = strict_json::unique_json(bytes, MAX_BYTES)?;
        if let Some(routes) = value
            .get_mut("routes")
            .and_then(serde_json::Value::as_array_mut)
        {
            if routes.len() > MAX_ROUTES {
                return Err("Sensor catalog route count limit".into());
            }
            // Retire only old cooling consumers; preserve every source and lighting route.
            for route in routes.iter() {
                if route
                    .get("scope")
                    .and_then(|s| s.get("kind"))
                    .and_then(|k| k.as_str())
                    == Some("cooling")
                {
                    let object = route.as_object().ok_or("Invalid legacy sensor route")?;
                    if object.len() != 3
                        || !object.contains_key("input")
                        || !object.contains_key("source")
                    {
                        return Err("Invalid legacy sensor route".into());
                    }
                    let scope = route["scope"]
                        .as_object()
                        .ok_or("Invalid legacy sensor scope")?;
                    if scope.len() != 3 {
                        return Err("Unknown legacy sensor scope fields".into());
                    }
                    identity(
                        scope
                            .get("identity")
                            .and_then(|v| v.as_str())
                            .ok_or("Missing legacy identity")?,
                    )?;
                    let digest = scope
                        .get("digest")
                        .and_then(|v| v.as_str())
                        .ok_or("Missing legacy digest")?;
                    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err("Invalid legacy digest".into());
                    }
                    let input = route["input"].as_str().ok_or("Invalid legacy input")?;
                    if !ambient::key(input) {
                        return Err("Invalid legacy input".into());
                    }
                    identity(route["source"].as_str().ok_or("Invalid legacy source")?)?;
                }
            }
            routes.retain(|r| {
                r.get("scope")
                    .and_then(|s| s.get("kind"))
                    .and_then(|k| k.as_str())
                    != Some("cooling")
            });
        }
        let catalog: Self = serde_json::from_value(value).map_err(|error| error.to_string())?;
        catalog.validate()?;
        Ok(catalog)
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        storage::atomic_write(path, &self.encode()?, MAX_BYTES).map_err(|error| error.to_string())
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|error| error.to_string())?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES as u64 {
            return Err("Sensor catalog must be a bounded regular preference file".into());
        }
        let mut bytes = Vec::new();
        file.take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        Self::decode(&bytes)
    }
    /// Exact scope and input-set resolution; returns data only, never starts a source/factory.
    pub fn prepare(&self, scope: &Scope, inputs: &[String]) -> Result<Vec<Resolved>, String> {
        self.validate()?;
        scope.validate()?;
        if inputs.len() > MAX_SOURCES {
            return Err("Too many requested sensor inputs".into());
        }
        let mut requested = BTreeSet::new();
        for input in inputs {
            if !ambient::key(input) || !requested.insert(input.as_str()) {
                return Err("Invalid or duplicate requested sensor input".into());
            }
        }
        let scoped = self
            .routes
            .iter()
            .filter(|route| &route.scope == scope)
            .map(|route| (route.input.as_str(), route.source.as_str()))
            .collect::<BTreeMap<_, _>>();
        if scoped.len() != requested.len() || scoped.keys().any(|input| !requested.contains(input))
        {
            return Err("Sensor scope is stale, unbound or contains extra input routes".into());
        }
        let sources = self
            .sources
            .iter()
            .map(|source| (source.id.as_str(), source))
            .collect::<BTreeMap<_, _>>();
        let mut resources = BTreeMap::new();
        let mut result = Vec::with_capacity(inputs.len());
        for input in inputs {
            let source = sources[scoped[input.as_str()]];
            let descriptor = source.target.canonical_key()?;
            for resource in source.target.resource_keys()? {
                if let Some(previous) = resources.insert(resource, descriptor.clone())
                    && previous != descriptor
                {
                    return Err(
                        "Conflicting sensor descriptors claim the same physical resource".into(),
                    );
                }
            }
            result.push(Resolved {
                input: input.clone(),
                source_id: source.id.clone(),
                source_revision: source.revision,
                target: source.target.clone(),
            });
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
