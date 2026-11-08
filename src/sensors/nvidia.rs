//! GPU Core only. The ABI comes from the saved NVFC sources; no native calls here.
use super::Reading;

pub const TARGET_GPU: i32 = 1;
pub const THERMAL_VERSION: u32 = (2 << 16) | 68;
pub const POLL_INTERVAL_MS: u64 = 1000;

/// Local physical binding. Deliberately does not derive Serialize/Deserialize.
/// These are all four values returned by NvAPI_GPU_GetPCIIdentifiers, not an
/// enumeration index. Identical matches remain ambiguous and must be refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciIdentity {
    pub device_id: u32,
    pub subsystem_id: u32,
    pub revision_id: u32,
    pub extended_device_id: u32,
}
impl PciIdentity {
    pub fn validate(self) -> Result<(), String> {
        let device = self.device_id >> 16;
        if self.device_id & 0xffff != 0x10de || matches!(device, 0 | 0xffff) {
            return Err("An explicit NVIDIA PCI identity is required".into());
        }
        Ok(())
    }
}

/// Raw signed enum fields avoid undefined behavior before validating FFI output.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThermalSensor {
    pub controller: i32,
    pub default_min: i32,
    pub default_max: i32,
    pub current_temperature: i32,
    pub target: i32,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThermalSettings {
    pub version: u32,
    pub count: u32,
    pub sensors: [ThermalSensor; 3],
}
impl Default for ThermalSettings {
    fn default() -> Self {
        Self {
            version: THERMAL_VERSION,
            count: 0,
            sensors: [ThermalSensor::default(); 3],
        }
    }
}
const _: [(); 68] = [(); std::mem::size_of::<ThermalSettings>()];
const _: [(); 20] = [(); std::mem::size_of::<ThermalSensor>()];

pub trait Backend {
    fn identity(&mut self) -> Result<PciIdentity, String>;
    /// Success only; callers must never parse output after an error status.
    fn thermal_settings(&mut self, output: &mut ThermalSettings) -> Result<(), String>;
}

pub fn parse(settings: &ThermalSettings) -> Result<Option<f64>, String> {
    if settings.version != THERMAL_VERSION || settings.count > 3 {
        return Err("Invalid NVAPI thermal version or sensor count".into());
    }
    let mut core = None;
    for sensor in settings
        .sensors
        .iter()
        .take(settings.count as usize)
        .filter(|sensor| sensor.target == TARGET_GPU)
    {
        if core.replace(sensor.current_temperature).is_some() {
            return Err("Ambiguous NVAPI GPU Core target".into());
        }
    }
    match core {
        Some(value) if (-20..=150).contains(&value) => Ok(Some(f64::from(value))),
        Some(_) => Err("NVAPI GPU Core temperature outside supported range".into()),
        None => Ok(None),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub reading: Option<Reading>,
    pub last_attempt_ms: Option<u64>,
    pub error: Option<String>,
    pub terminal: bool,
}
pub struct Provider<B: Backend> {
    backend: Option<B>,
    binding: PciIdentity,
    last_clock: Option<u64>,
    next_poll: Option<u64>,
    status: Status,
}
impl<B: Backend> Provider<B> {
    pub fn new(binding: PciIdentity, mut backend: B) -> Result<Self, String> {
        binding.validate()?;
        if backend.identity()? != binding {
            return Err("Selected NVIDIA PCI identity differs from binding".into());
        }
        Ok(Self {
            backend: Some(backend),
            binding,
            last_clock: None,
            next_poll: None,
            status: Status {
                reading: None,
                last_attempt_ms: None,
                error: None,
                terminal: false,
            },
        })
    }
    pub fn snapshot(&self) -> Status {
        self.status.clone()
    }
    pub fn latest(&self, now_ms: u64, max_age_ms: u64) -> Option<Reading> {
        self.status
            .reading
            .filter(|reading| !self.status.terminal && reading.is_fresh(now_ms, max_age_ms))
    }
    pub fn stop(&mut self) {
        self.backend = None;
        self.status.reading = None;
        self.status.terminal = true;
    }
    fn error(&mut self, error: String, terminal: bool) -> String {
        self.status.reading = None;
        let error: String = error.chars().take(512).collect();
        self.status.error = Some(error.clone());
        if terminal {
            self.stop();
        }
        error
    }
    /// Not-due/missing data returns None, never a freshly timestamped cached value.
    /// Read failures clear old data and consume the one-second retry budget.
    pub fn poll(&mut self, now_ms: u64) -> Result<Option<Reading>, String> {
        if self.status.terminal {
            return Err(self
                .status
                .error
                .clone()
                .unwrap_or_else(|| "GPU sensor is stopped".into()));
        }
        if now_ms == 0 || self.last_clock.is_some_and(|last| now_ms < last) {
            return Err(self.error("GPU sensor monotonic clock is invalid".into(), true));
        }
        self.last_clock = Some(now_ms);
        if self.next_poll.is_some_and(|due| now_ms < due) {
            return Ok(None);
        }
        let Some(next) = now_ms.checked_add(POLL_INTERVAL_MS) else {
            return Err(self.error("GPU sensor cadence clock overflow".into(), true));
        };
        self.next_poll = Some(next);
        self.status.last_attempt_ms = Some(now_ms);
        let identity = match self
            .backend
            .as_mut()
            .ok_or("GPU sensor backend is closed")?
            .identity()
        {
            Ok(identity) => identity,
            Err(error) => return Err(self.error(error, false)),
        };
        if identity != self.binding {
            return Err(self.error("Selected NVIDIA PCI identity changed".into(), true));
        }
        let mut settings = ThermalSettings::default();
        if let Err(error) = self
            .backend
            .as_mut()
            .unwrap()
            .thermal_settings(&mut settings)
        {
            return Err(self.error(error, false));
        }
        let value = parse(&settings).map_err(|error| self.error(error, false))?;
        let reading = value.map(|celsius| Reading {
            celsius,
            observed_ms: now_ms,
        });
        self.status.reading = reading;
        self.status.error = None;
        Ok(reading)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    fn binding() -> PciIdentity {
        PciIdentity {
            device_id: 0x1234_10de,
            subsystem_id: 7,
            revision_id: 1,
            extended_device_id: 0,
        }
    }
    fn thermal(value: i32) -> ThermalSettings {
        let mut settings = ThermalSettings {
            count: 1,
            ..ThermalSettings::default()
        };
        settings.sensors[0] = ThermalSensor {
            controller: 1,
            default_min: -20,
            default_max: 150,
            current_temperature: value,
            target: TARGET_GPU,
        };
        settings
    }
    struct State {
        identity: PciIdentity,
        output: ThermalSettings,
        fail: bool,
        reads: usize,
        identities: usize,
        drops: usize,
    }
    struct Fake(Rc<RefCell<State>>);
    impl Drop for Fake {
        fn drop(&mut self) {
            self.0.borrow_mut().drops += 1;
        }
    }
    impl Backend for Fake {
        fn identity(&mut self) -> Result<PciIdentity, String> {
            let mut state = self.0.borrow_mut();
            state.identities += 1;
            Ok(state.identity)
        }
        fn thermal_settings(&mut self, output: &mut ThermalSettings) -> Result<(), String> {
            assert_eq!(*output, ThermalSettings::default());
            let mut state = self.0.borrow_mut();
            state.reads += 1;
            *output = state.output;
            if state.fail {
                Err("synthetic NVAPI status error".into())
            } else {
                Ok(())
            }
        }
    }
    fn fixture() -> (Provider<Fake>, Rc<RefCell<State>>) {
        let state = Rc::new(RefCell::new(State {
            identity: binding(),
            output: thermal(42),
            fail: false,
            reads: 0,
            identities: 0,
            drops: 0,
        }));
        (
            Provider::new(binding(), Fake(state.clone())).unwrap(),
            state,
        )
    }
    #[test]
    fn original_cpp_thermal_layout_constructor_and_targets() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/nvapi-thermal-native.json")).unwrap();
        assert_eq!(oracle["size"], std::mem::size_of::<ThermalSettings>());
        assert_eq!(oracle["alignment"], std::mem::align_of::<ThermalSettings>());
        assert_eq!(oracle["sensor_size"], std::mem::size_of::<ThermalSensor>());
        for (key, value) in [
            (
                "offset_version",
                std::mem::offset_of!(ThermalSettings, version),
            ),
            ("offset_count", std::mem::offset_of!(ThermalSettings, count)),
            (
                "offset_sensor",
                std::mem::offset_of!(ThermalSettings, sensors),
            ),
            (
                "offset_controller",
                std::mem::offset_of!(ThermalSensor, controller),
            ),
            (
                "offset_min",
                std::mem::offset_of!(ThermalSensor, default_min),
            ),
            (
                "offset_max",
                std::mem::offset_of!(ThermalSensor, default_max),
            ),
            (
                "offset_temperature",
                std::mem::offset_of!(ThermalSensor, current_temperature),
            ),
            ("offset_target", std::mem::offset_of!(ThermalSensor, target)),
        ] {
            assert_eq!(oracle[key], value);
        }
        assert_eq!(oracle["version"], THERMAL_VERSION);
        assert_eq!(oracle["gpu_target"], TARGET_GPU);
        let settings = ThermalSettings::default();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&settings.version.to_le_bytes());
        bytes.extend_from_slice(&settings.count.to_le_bytes());
        for sensor in settings.sensors {
            for value in [
                sensor.controller,
                sensor.default_min,
                sensor.default_max,
                sensor.current_temperature,
                sensor.target,
            ] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(oracle["bytes_hex"], hex);
    }
    #[test]
    fn target_uniqueness_version_count_missing_and_temperature_bounds() {
        for temperature in [-20, 0, 150] {
            assert_eq!(
                parse(&thermal(temperature)).unwrap(),
                Some(f64::from(temperature))
            );
        }
        for temperature in [-21, 151, i32::MAX] {
            assert!(parse(&thermal(temperature)).is_err());
        }
        let mut data = thermal(42);
        data.count = 4;
        assert!(parse(&data).is_err());
        data.count = u32::MAX;
        assert!(parse(&data).is_err());
        data = thermal(42);
        data.version ^= 1;
        assert!(parse(&data).is_err());
        data = thermal(42);
        data.count = 2;
        data.sensors[1] = data.sensors[0];
        assert!(parse(&data).is_err());
        data = thermal(42);
        data.sensors[0].target = 2;
        assert_eq!(parse(&data).unwrap(), None);
        data.count = 0;
        assert_eq!(parse(&data).unwrap(), None);
    }
    #[test]
    fn bounded_one_hz_zero_valid_and_cached_time_is_never_refreshed() {
        let (mut provider, state) = fixture();
        state.borrow_mut().output = thermal(0);
        let first = provider.poll(1000).unwrap().unwrap();
        assert_eq!(
            first,
            Reading {
                celsius: 0.,
                observed_ms: 1000
            }
        );
        assert!(provider.poll(1999).unwrap().is_none());
        assert_eq!(state.borrow().reads, 1);
        assert_eq!(provider.latest(1999, 1000).unwrap().observed_ms, 1000);
        assert!(provider.latest(2001, 1000).is_none());
        assert!(provider.latest(999, 1000).is_none());
        assert_eq!(provider.poll(2000).unwrap().unwrap().observed_ms, 2000);
        assert_eq!(state.borrow().reads, 2);
    }
    #[test]
    fn failed_read_is_not_zero_old_value_or_fresh_cache_and_retries_are_throttled() {
        let (mut provider, state) = fixture();
        provider.poll(1).unwrap();
        state.borrow_mut().fail = true;
        state.borrow_mut().output = thermal(255);
        assert!(provider.poll(1001).is_err());
        assert!(provider.snapshot().reading.is_none());
        assert!(provider.latest(1001, 5000).is_none());
        assert!(provider.poll(1500).unwrap().is_none());
        assert_eq!(state.borrow().reads, 2);
        state.borrow_mut().fail = false;
        state.borrow_mut().output = thermal(39);
        assert_eq!(provider.poll(2001).unwrap().unwrap().celsius, 39.);
        assert!(provider.snapshot().error.is_none());
    }
    #[test]
    fn identity_and_bad_clock_close_owned_backend_without_fallback() {
        let (mut provider, state) = fixture();
        state.borrow_mut().identity.subsystem_id ^= 1;
        assert!(provider.poll(1).is_err());
        assert!(provider.snapshot().terminal);
        assert_eq!(state.borrow().drops, 1);
        assert_eq!(state.borrow().reads, 0);
        assert!(provider.poll(2000).is_err());
        assert_eq!(state.borrow().reads, 0);
        for clock in [0, u64::MAX] {
            let (mut provider, state) = fixture();
            assert!(provider.poll(clock).is_err());
            assert_eq!(state.borrow().drops, 1);
            assert_eq!(state.borrow().reads, 0);
        }
        let (mut provider, state) = fixture();
        provider.poll(100).unwrap();
        assert!(provider.poll(99).is_err());
        assert_eq!(state.borrow().drops, 1);
        let state = Rc::new(RefCell::new(State {
            identity: PciIdentity {
                subsystem_id: 99,
                ..binding()
            },
            output: thermal(1),
            fail: false,
            reads: 0,
            identities: 0,
            drops: 0,
        }));
        assert!(Provider::new(binding(), Fake(state.clone())).is_err());
        assert_eq!(state.borrow().drops, 1);
    }
}
