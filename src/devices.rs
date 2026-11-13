//! Portable logical geometry and separate, explicitly local hardware bindings.
use crate::{
    lcd::hid_policy,
    lighting::door,
    storage,
    transport::{ene, fusion2, serial, skydimo, valkyrie},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::Path,
};

pub mod home_assistant;
pub mod openrgb;

pub const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_DEVICES: usize = 256;
pub const MAX_POINTS: usize = 65_536;
pub const MAX_AXIS: u32 = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Canvas,
    Ambient,
    Lcd,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Driver {
    SkydimoSerial { model: String },
    DoorBle,
    WledDdp,
    OpenRgb,
    HomeAssistant,
    TtLcd,
    Fusion2 { headers: [usize; 2] },
    EneDram,
    Valkyrie { model: String },
    Unsupported { protocol: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub id: String,
    pub label: String,
    pub role: Role,
    pub driver: Driver,
    pub leds: usize,
    /// Requested cadence; transport capacity may impose a lower effective rate.
    pub rate_hz: u32,
    pub enabled: bool,
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
fn label(value: &str) -> bool {
    !value.trim().is_empty() && value.chars().count() <= 128 && !value.chars().any(char::is_control)
}
impl Device {
    pub fn validate(&self) -> Result<(), String> {
        if !identifier(&self.id)
            || !label(&self.label)
            || !(1..=144).contains(&self.rate_hz)
            || self.leds > MAX_POINTS
        {
            return Err("Invalid device ID, label, rate or LED bound".into());
        }
        match &self.driver {
            Driver::HomeAssistant => {
                if !matches!(self.role, Role::Canvas | Role::Ambient)
                    || self.leds != 1
                    || self.rate_hz != 1
                {
                    return Err("Home Assistant controls one whole light at 1 Hz".into());
                }
            }
            Driver::OpenRgb => {
                if !matches!(self.role, Role::Canvas | Role::Ambient)
                    || !(1..=8192).contains(&self.leds)
                    || self.rate_hz > 60
                {
                    return Err(
                        "OpenRGB requires a complete 1..8192 LED controller and at most 60 Hz"
                            .into(),
                    );
                }
            }
            Driver::WledDdp => {
                if !matches!(self.role, Role::Canvas | Role::Ambient)
                    || !(1..=65_535).contains(&self.leds)
                    || self.rate_hz > 60
                    || self
                        .leds
                        .saturating_mul(3)
                        .saturating_mul(self.rate_hz as usize)
                        > 1_000_000
                {
                    return Err(
                        "WLED requires 1..65535 LEDs, at most 60 Hz and 1 MB/s per endpoint".into(),
                    );
                }
            }
            Driver::Fusion2 { headers } => {
                fusion2::count_code(headers[0])?;
                fusion2::count_code(headers[1])?;
                if self.role != Role::Canvas || self.leds != headers[0] + headers[1] {
                    return Err(
                        "Fusion2 requires both owned ARGB headers on one canvas device".into(),
                    );
                }
            }
            Driver::EneDram if self.role != Role::Canvas || self.leds != ene::LEDS => {
                return Err("ENE DRAM requires exactly8canvas LEDs".into());
            }
            Driver::Valkyrie { model } => {
                let model = valkyrie::Model::from_id(model).ok_or("Unsupported Valkyrie model")?;
                if self.role != Role::Canvas || self.leds != model.leds() {
                    return Err("Valkyrie role or model LED count mismatch".into());
                }
            }
            Driver::SkydimoSerial { model } => {
                let m = skydimo::model(model).ok_or("Unsupported Skydimo model")?;
                if !matches!(self.role, Role::Canvas | Role::Ambient)
                    || self.leds == 0
                    || self.leds < m.min_leds
                    || self.leds > m.max_leds
                    || (!m.resizable && self.leds != m.leds)
                {
                    return Err("Skydimo role or LED count mismatch".into());
                }
            }
            Driver::DoorBle
                if self.role != Role::Canvas
                    || ![door::COLOR_GROUPS, door::PIXEL_LEDS].contains(&self.leds) =>
            {
                return Err("Door BLE requires 20 legacy groups or 153 E1 preview pixels".into());
            }
            Driver::TtLcd if self.role != Role::Lcd || self.leds != 0 => {
                return Err("TT LCD requires LCD role and zero RGB LEDs".into());
            }
            Driver::Unsupported { protocol }
                if !label(protocol)
                    || self.enabled
                    || (self.role == Role::Lcd && self.leds != 0) =>
            {
                return Err(
                    "Unsupported protocol must be explicit, disabled and role-consistent".into(),
                );
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub device: String,
    pub led: usize,
    pub x: u32,
    pub y: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub points: Vec<Point>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setup {
    pub version: u32,
    pub devices: Vec<Device>,
    pub canvas: Canvas,
}

impl Setup {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || self.devices.len() > MAX_DEVICES
            || self.canvas.points.len() > MAX_POINTS
            || !(1..=MAX_AXIS).contains(&self.canvas.width)
            || !(1..=MAX_AXIS).contains(&self.canvas.height)
        {
            return Err("Unsupported setup version or geometry bounds".into());
        }
        let mut devices = BTreeMap::new();
        let mut expected = 0usize;
        for device in &self.devices {
            device.validate()?;
            if devices.insert(device.id.as_str(), device).is_some() {
                return Err("Duplicate logical device ID".into());
            }
            if device.role == Role::Canvas {
                expected = expected
                    .checked_add(device.leds)
                    .ok_or("Canvas LED count overflow")?;
            }
        }
        if expected > MAX_POINTS || self.canvas.points.len() != expected {
            return Err("Canvas must map every canvas-role LED, including disabled devices".into());
        }
        let mut destinations = BTreeSet::new();
        for point in &self.canvas.points {
            let device = devices
                .get(point.device.as_str())
                .ok_or("Canvas references unknown device")?;
            if device.role != Role::Canvas
                || point.led >= device.leds
                || point.x >= self.canvas.width
                || point.y >= self.canvas.height
                || !destinations.insert((point.device.as_str(), point.led))
            {
                return Err("Invalid or duplicate canvas LED destination".into());
            }
        }
        Ok(())
    }

    /// An invalid proposal leaves the current setup untouched.
    pub fn replace(&mut self, proposal: Setup) -> Result<(), String> {
        proposal.validate()?;
        *self = proposal;
        Ok(())
    }

    /// Logical JSON only: this type has no hardware address/path/serial fields.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        bounded_encode(self)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_JSON_BYTES {
            return Err("Setup JSON exceeds 4 MiB".into());
        }
        let setup: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        setup.validate()?;
        Ok(setup)
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        storage::atomic_write(path, &self.encode()?, MAX_JSON_BYTES).map_err(|e| e.to_string())
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        Self::decode(&bounded_read(path)?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Binding {
    HomeAssistant {
        endpoint: String,
        entity: String,
        credential_id: String,
    },
    OpenRgb {
        endpoint: std::net::SocketAddr,
        identity: String,
        topology: String,
    },
    WledDdp {
        endpoint: std::net::SocketAddr,
    },
    Serial {
        port: String,
        instance_id: String,
        serial: String,
    },
    Ble {
        address: u64,
    },
    Hid {
        path: String,
    },
    /// Explicit existing ENE target on primary PIIX4. This never scans the bus.
    EnePiix4 {
        module_path: String,
        address: u8,
    },
}
pub type Bindings = BTreeMap<String, Binding>;

pub(crate) fn canonical_endpoint(endpoint: std::net::SocketAddr) -> std::net::SocketAddr {
    match endpoint {
        std::net::SocketAddr::V4(_) => endpoint,
        std::net::SocketAddr::V6(v6) if v6.ip().to_ipv4_mapped().is_some() => {
            std::net::SocketAddr::new(endpoint.ip().to_canonical(), endpoint.port())
        }
        _ => endpoint,
    }
}

impl Binding {
    pub(crate) fn endpoint_keys(&self) -> Vec<String> {
        match self {
            Self::HomeAssistant {
                endpoint, entity, ..
            } => vec![format!(
                "ha:{}:{entity}",
                home_assistant::canonical_endpoint(endpoint).unwrap_or_else(|_| endpoint.clone())
            )],
            // Until the SDK exposes a verified server namespace, identical
            // physical metadata across server addresses is conservatively one owner.
            Self::OpenRgb { identity, .. } => vec![format!("openrgb:{identity}")],
            Self::WledDdp { endpoint } => vec![format!("wled:{}", canonical_endpoint(*endpoint))],
            Self::Serial {
                port, instance_id, ..
            } => vec![
                format!("serial:{port}"),
                format!("usb:{}", instance_id.to_ascii_uppercase()),
            ],
            Self::Ble { address } => vec![format!("ble:{address}")],
            Self::Hid { path } => vec![format!("hid:{}", path.to_ascii_lowercase())],
            Self::EnePiix4 { address, .. } => vec![format!("ene-piix4:0:{address}")],
        }
    }
    pub fn validate(&self, device: &Device) -> Result<(), String> {
        device.validate()?;
        match (self, &device.driver) {
            (
                Self::HomeAssistant {
                    endpoint,
                    entity,
                    credential_id,
                },
                Driver::HomeAssistant,
            ) => {
                if !home_assistant::valid_entity(entity)
                    || &home_assistant::credential_id(endpoint)? != credential_id
                {
                    return Err("Home Assistant binding requires a light entity and its own endpoint credential reference".into());
                }
                Ok(())
            }

            (
                Self::OpenRgb {
                    endpoint,
                    identity,
                    topology,
                },
                Driver::OpenRgb,
            ) => {
                let ip = endpoint.ip().to_canonical();
                if endpoint.port() == 0
                    || ip.is_unspecified()
                    || ip.is_multicast()
                    || ip == std::net::IpAddr::V4(std::net::Ipv4Addr::BROADCAST)
                    || !openrgb::valid_hash(identity)
                    || !openrgb::valid_hash(topology)
                {
                    return Err(
                        "Select an unambiguous OpenRGB controller from its server inventory".into(),
                    );
                }
                Ok(())
            }

            (Self::WledDdp { endpoint }, Driver::WledDdp) => {
                let ip = endpoint.ip().to_canonical();
                if endpoint.port() == 0
                    || ip.is_unspecified()
                    || ip.is_multicast()
                    || ip == std::net::IpAddr::V4(std::net::Ipv4Addr::BROADCAST)
                {
                    return Err("WLED requires an explicit unicast IP and nonzero UDP port".into());
                }
                Ok(())
            }

            (
                Self::Serial {
                    port,
                    instance_id,
                    serial: identity,
                },
                Driver::SkydimoSerial { model },
            ) => serial::Binding {
                descriptor: serial::Descriptor {
                    port: port.clone(),
                    instance_id: instance_id.clone(),
                },
                expected: skydimo::Identity {
                    model: model.clone(),
                    serial: identity.clone(),
                },
                leds: device.leds,
            }
            .validate(),
            (Self::Ble { address }, Driver::DoorBle) if door::allowed(*address) => Ok(()),
            (Self::Hid { path }, Driver::TtLcd) if path.len() <= 4096 => {
                hid_policy::validate_path(hid_policy::Kind::Display, path)
            }
            (Self::Hid { path }, Driver::Fusion2 { .. }) => fusion2::validate_path(path),
            (Self::Hid { path }, Driver::Valkyrie { model }) => valkyrie::validate_path(
                valkyrie::Model::from_id(model).ok_or("Unknown keyboard model")?,
                path,
            ),
            (
                Self::EnePiix4 {
                    module_path,
                    address,
                },
                Driver::EneDram,
            ) => {
                let bytes = module_path.as_bytes();
                if !ene::valid_address(*address)
                    || module_path.len() > 4096
                    || bytes.len() < 4
                    || !bytes[0].is_ascii_alphabetic()
                    || bytes[1] != b':'
                    || !matches!(bytes[2], b'/' | b'\\')
                    || module_path[2..].contains(':')
                    || module_path.chars().any(char::is_control)
                {
                    return Err(
                        "ENE binding requires an explicit local module file and address0x70..0x76"
                            .into(),
                    );
                }
                Ok(())
            }
            _ => Err("Binding does not match supported device driver".into()),
        }
    }
}

/// Missing bindings are allowed during configuration; every provided target must match.
pub fn validate_bindings(setup: &Setup, bindings: &Bindings) -> Result<(), String> {
    setup.validate()?;
    if bindings.len() > MAX_DEVICES {
        return Err("Too many local bindings".into());
    }
    let mut endpoints = BTreeSet::new();
    for (id, binding) in bindings {
        let device = setup
            .devices
            .iter()
            .find(|d| &d.id == id)
            .ok_or("Binding references unknown device")?;
        binding.validate(device)?;
        for key in binding.endpoint_keys() {
            if !endpoints.insert(key) {
                return Err("Hardware endpoint bound to multiple logical devices".into());
            }
        }
    }
    Ok(())
}

/// Preflight for activation; performs no discovery and opens no handle.
pub fn validate_active_bindings(setup: &Setup, bindings: &Bindings) -> Result<(), String> {
    validate_bindings(setup, bindings)?;
    if setup
        .devices
        .iter()
        .any(|d| d.enabled && !bindings.contains_key(&d.id))
    {
        return Err("Enabled device has no explicit local binding".into());
    }
    Ok(())
}

/// Explicit private serialization, never included by Setup::encode/save.
pub fn encode_bindings(setup: &Setup, bindings: &Bindings) -> Result<Vec<u8>, String> {
    validate_bindings(setup, bindings)?;
    bounded_encode(bindings)
}
pub fn decode_bindings(setup: &Setup, bytes: &[u8]) -> Result<Bindings, String> {
    if bytes.len() > MAX_JSON_BYTES {
        return Err("Bindings JSON exceeds 4 MiB".into());
    }
    // A duplicate JSON key must not silently replace an earlier hardware target.
    struct UniqueBindings(Bindings);
    impl<'de> Deserialize<'de> for UniqueBindings {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct UniqueMap;
            impl<'de> serde::de::Visitor<'de> for UniqueMap {
                type Value = UniqueBindings;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("unique logical device IDs mapped to local bindings")
                }
                fn visit_map<M: serde::de::MapAccess<'de>>(
                    self,
                    mut map: M,
                ) -> Result<Self::Value, M::Error> {
                    let mut bindings = Bindings::new();
                    while let Some((id, binding)) = map.next_entry::<String, Binding>()? {
                        if bindings.len() >= MAX_DEVICES
                            || !identifier(&id)
                            || bindings.insert(id, binding).is_some()
                        {
                            return Err(serde::de::Error::custom(
                                "Invalid, duplicate or excessive binding IDs",
                            ));
                        }
                    }
                    Ok(UniqueBindings(bindings))
                }
            }
            deserializer.deserialize_map(UniqueMap)
        }
    }
    let UniqueBindings(bindings) = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    validate_bindings(setup, &bindings)?;
    Ok(bindings)
}
fn bounded_encode(value: &impl Serialize) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_JSON_BYTES {
        return Err("JSON exceeds 4 MiB".into());
    }
    Ok(bytes)
}
fn bounded_read(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Setup must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_JSON_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_JSON_BYTES {
        return Err("Setup JSON exceeds 4 MiB".into());
    }
    Ok(bytes)
}
