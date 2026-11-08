//! AMD family 17h..1Ah Tctl only. No CPUID, driver or module execution here.
//! Pinned LHM Amd17Cpu.cs lines 285..312 supply the f32 conversion. Legacy
//! model-specific Tdie offsets, CCDs, MSRs and other CPU sensors are outside scope.
use super::Reading;
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

pub const MODULE_BYTES: usize = 10_652;
pub const MODULE_SHA256: &str = "dae74615761b78bdf064dfb3e136252ddcc6fc727d88f14738d0e5800d427a91";
pub const POLL_INTERVAL_MS: u64 = 1000;
/// Content-pinned container. Runtime acceptance checks the exact image hash;
/// the separate static RSA proof does not claim the installed driver's policy.
pub struct Module {
    bytes: Vec<u8>,
}
impl Module {
    pub fn read(path: &Path) -> Result<Self, String> {
        let metadata =
            std::fs::symlink_metadata(path).map_err(|e| format!("AMD module metadata: {e}"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("AMD module must be a regular file".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("AMD module reparse points are not accepted".into());
            }
        }
        let file = File::open(path).map_err(|e| format!("Open AMD module: {e}"))?;
        let metadata = file
            .metadata()
            .map_err(|e| format!("AMD module metadata: {e}"))?;
        if !metadata.is_file() || metadata.len() != MODULE_BYTES as u64 {
            return Err("AMD module size differs from pin".into());
        }
        let mut bytes = Vec::with_capacity(MODULE_BYTES);
        file.take((MODULE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("Read AMD module: {e}"))?;
        Self::from_bytes(&bytes)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != MODULE_BYTES {
            return Err("AMD module size differs from pin".into());
        }
        let digest = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        if digest != MODULE_SHA256 {
            return Err("AMD module SHA256 differs from pin".into());
        }
        Ok(Self {
            bytes: bytes.to_vec(),
        })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    /// Fake Device tests only. Never supply this to a native module loader.
    #[cfg(test)]
    pub(crate) fn synthetic_for_test() -> Self {
        Self {
            bytes: vec![0; MODULE_BYTES],
        }
    }
}

/// Explicit local CPUID/package binding, never serialized into a portable file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuIdentity {
    pub vendor: [u8; 12],
    pub signature: u32,
    pub packages: u32,
}
impl CpuIdentity {
    pub fn display_family(self) -> u32 {
        let base = (self.signature >> 8) & 15;
        if base == 15 {
            base + ((self.signature >> 20) & 255)
        } else {
            base
        }
    }
    pub fn validate(self) -> Result<(), String> {
        if self.vendor != *b"AuthenticAMD"
            || self.packages != 1
            || !(0x17..=0x1a).contains(&self.display_family())
        {
            return Err(
                "AMD Tctl requires an explicit single-package AuthenticAMD family 17h..1Ah binding"
                    .into(),
            );
        }
        Ok(())
    }
}
pub trait Backend {
    fn identity(&mut self) -> Result<CpuIdentity, String>;
    /// The native executor owns the literal SMN 0x59800 allowlist and PCI lock.
    fn read_tctl_raw(&mut self) -> Result<u32, String>;
}
/// All raw codes are decodable. Provider policy separately rejects temperatures
/// outside -20..150 C; zero raw is a genuine 0 C, never a substitute for an error.
pub fn decode_tctl(raw: u32) -> f64 {
    let temperature = (raw >> 21) * 125;
    let mut t = (temperature as f32) * 0.001_f32;
    if raw & 0x80000 != 0 || raw & 0x30000 == 0x30000 {
        t += -49.0_f32;
    }
    f64::from(t)
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
    binding: CpuIdentity,
    last_clock: Option<u64>,
    next_poll: Option<u64>,
    status: Status,
}
impl<B: Backend> Provider<B> {
    pub fn new(binding: CpuIdentity, mut backend: B) -> Result<Self, String> {
        binding.validate()?;
        let identity = backend.identity()?;
        identity.validate()?;
        if identity != binding {
            return Err("Selected AMD CPU identity differs from binding".into());
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
            .filter(|r| !self.status.terminal && r.is_fresh(now_ms, max_age_ms))
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
    /// Errors consume the cadence budget. A not-due poll returns None and does
    /// not refresh a cached timestamp or touch the backend.
    pub fn poll(&mut self, now_ms: u64) -> Result<Option<Reading>, String> {
        if self.status.terminal {
            return Err(self
                .status
                .error
                .clone()
                .unwrap_or_else(|| "AMD CPU sensor is stopped".into()));
        }
        if now_ms == 0 || self.last_clock.is_some_and(|last| now_ms < last) {
            return Err(self.error("AMD CPU sensor monotonic clock is invalid".into(), true));
        }
        self.last_clock = Some(now_ms);
        if self.next_poll.is_some_and(|due| now_ms < due) {
            return Ok(None);
        }
        let Some(next) = now_ms.checked_add(POLL_INTERVAL_MS) else {
            return Err(self.error("AMD CPU sensor cadence clock overflow".into(), true));
        };
        self.next_poll = Some(next);
        self.status.last_attempt_ms = Some(now_ms);
        let identity = match self.backend.as_mut().unwrap().identity() {
            Ok(identity) => identity,
            Err(error) => return Err(self.error(error, true)),
        };
        if identity.validate().is_err() || identity != self.binding {
            return Err(self.error("Selected AMD CPU identity changed".into(), true));
        }
        let raw = match self.backend.as_mut().unwrap().read_tctl_raw() {
            Ok(raw) => raw,
            Err(error) => return Err(self.error(error, false)),
        };
        let celsius = decode_tctl(raw);
        if !(-20.0..=150.0).contains(&celsius) {
            return Err(self.error("AMD Tctl outside supported range".into(), false));
        }
        let reading = Reading {
            celsius,
            observed_ms: now_ms,
        };
        self.status.reading = Some(reading);
        self.status.error = None;
        Ok(Some(reading))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    fn binding() -> CpuIdentity {
        CpuIdentity {
            vendor: *b"AuthenticAMD",
            signature: 0x00a6_0f12,
            packages: 1,
        }
    }
    struct State {
        identity: CpuIdentity,
        raw: u32,
        fail: bool,
        identity_fail: bool,
        reads: usize,
        identities: usize,
        drops: usize,
    }
    struct Fake(Rc<RefCell<State>>);
    impl Backend for Fake {
        fn identity(&mut self) -> Result<CpuIdentity, String> {
            let mut s = self.0.borrow_mut();
            s.identities += 1;
            if s.identity_fail {
                Err("identity unavailable".into())
            } else {
                Ok(s.identity)
            }
        }
        fn read_tctl_raw(&mut self) -> Result<u32, String> {
            let mut s = self.0.borrow_mut();
            s.reads += 1;
            if s.fail {
                Err("synthetic SMN read failure".into())
            } else {
                Ok(s.raw)
            }
        }
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            self.0.borrow_mut().drops += 1;
        }
    }
    fn fixture() -> (Provider<Fake>, Rc<RefCell<State>>) {
        let state = Rc::new(RefCell::new(State {
            identity: binding(),
            raw: 400 << 21,
            fail: false,
            identity_fail: false,
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
    fn full_cpp_float32_oracle_matches_every_code_and_flag_combination() {
        let bits = include_bytes!("fixtures/amd-tctl-f32-bits.bin");
        assert_eq!(bits.len(), 2048 * 8 * 4);
        for code in 0u32..2048 {
            for flags in 0u32..8 {
                let raw = (code << 21) | ((flags & 1) << 19) | (((flags >> 1) & 3) << 16);
                let index = (code as usize * 8 + flags as usize) * 4;
                let expected = u32::from_le_bytes(bits[index..index + 4].try_into().unwrap());
                assert_eq!(
                    (decode_tctl(raw) as f32).to_bits(),
                    expected,
                    "raw {raw:08x}"
                );
                assert_eq!(decode_tctl(raw), f64::from(f32::from_bits(expected)));
            }
        }
        // Other register bits do not participate in the pinned expression.
        assert_eq!(decode_tctl(0), 0.0);
        assert_eq!(decode_tctl(0x0014_ffff), 0.0);
    }
    #[test]
    fn cadence_failures_zero_and_old_observations_are_distinct() {
        let (mut p, s) = fixture();
        let first = p.poll(1).unwrap().unwrap();
        assert_eq!(first.observed_ms, 1);
        assert_eq!(first.celsius, decode_tctl(400 << 21));
        assert_eq!(p.poll(1000).unwrap(), None);
        assert_eq!(s.borrow().reads, 1);
        assert_eq!(s.borrow().identities, 2);
        assert_eq!(p.snapshot().reading, Some(first));
        assert!(p.latest(1002, 1000).is_none());
        s.borrow_mut().fail = true;
        assert!(p.poll(1001).is_err());
        assert!(p.snapshot().reading.is_none());
        assert_eq!(p.poll(2000).unwrap(), None);
        assert_eq!(s.borrow().reads, 2);
        s.borrow_mut().fail = false;
        s.borrow_mut().raw = 0;
        assert_eq!(p.poll(2001).unwrap().unwrap().celsius, 0.0);
        s.borrow_mut().raw = 2047 << 21;
        assert!(p.poll(3001).is_err());
        assert!(p.snapshot().reading.is_none());
        assert!(!p.snapshot().terminal);
        s.borrow_mut().raw = 0x80000;
        assert!(p.poll(4001).is_err());
        assert!(p.snapshot().reading.is_none());
        s.borrow_mut().raw = 400 << 21;
        assert!(p.poll(5001).unwrap().is_some());
        assert!(p.snapshot().error.is_none());
        p.stop();
        assert!(p.poll(6001).is_err());
        assert_eq!(s.borrow().drops, 1);
    }
    #[test]
    fn identity_clock_and_overflow_fail_closed_before_raw_read() {
        for identity_error in [false, true] {
            let (mut p, s) = fixture();
            p.poll(10).unwrap();
            if identity_error {
                s.borrow_mut().identity_fail = true;
            } else {
                s.borrow_mut().identity.signature ^= 1;
            }
            assert!(p.poll(1010).is_err());
            assert!(p.snapshot().terminal);
            assert!(p.snapshot().reading.is_none());
            assert_eq!(s.borrow().reads, 1);
            assert_eq!(s.borrow().drops, 1);
            assert!(p.poll(2010).is_err());
            assert_eq!(s.borrow().reads, 1);
        }
        for now in [0, u64::MAX] {
            let (mut p, s) = fixture();
            assert!(p.poll(now).is_err());
            assert!(p.snapshot().terminal);
            assert_eq!(s.borrow().reads, 0);
        }
        let (mut p, s) = fixture();
        p.poll(100).unwrap();
        assert!(p.poll(99).is_err());
        assert!(p.snapshot().terminal);
        assert_eq!(s.borrow().reads, 1);
    }
    #[test]
    fn cpu_binding_rejects_wrong_vendor_family_packages_and_raw_signature() {
        for family in 0x17..=0x1a {
            let mut b = binding();
            b.signature = 0xf00 | ((family - 15) << 20);
            assert_eq!(b.display_family(), family);
            b.validate().unwrap();
        }
        let b = binding();
        let mut wrong = b;
        wrong.vendor = *b"GenuineIntel";
        assert!(wrong.validate().is_err());
        for packages in [0, 2, u32::MAX] {
            let mut wrong = b;
            wrong.packages = packages;
            assert!(wrong.validate().is_err());
        }
        for signature in [0, 0x0006_0000, 0x0070_0f00, 0x00c0_0f00] {
            let mut wrong = b;
            wrong.signature = signature;
            assert!(wrong.validate().is_err());
        }
        let (_, s) = fixture();
        let mut wrong = b;
        wrong.signature ^= 1;
        assert!(Provider::new(wrong, Fake(s.clone())).is_err());
        assert_eq!(s.borrow().reads, 0);
    }
    #[test]
    fn unpinned_module_inputs_are_rejected_without_execution() {
        assert!(Module::from_bytes(&[]).is_err());
        assert!(Module::from_bytes(&vec![0; MODULE_BYTES - 1]).is_err());
        assert!(Module::from_bytes(&vec![0; MODULE_BYTES + 1]).is_err());
        let synthetic = Module::synthetic_for_test();
        assert_eq!(synthetic.bytes().len(), MODULE_BYTES);
        assert!(Module::from_bytes(synthetic.bytes()).is_err());
        assert!(Module::read(Path::new(env!("CARGO_MANIFEST_DIR"))).is_err());
        // Optional saved source is read only; tests never require private source data.
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("work/amd-cpu-source/AMDFamily17.bin");
        if source.is_file() {
            let module = Module::read(&source).unwrap();
            let mut altered = module.bytes().to_vec();
            altered[MODULE_BYTES - 1] ^= 1;
            assert!(Module::from_bytes(&altered).is_err());
        }
    }
}
