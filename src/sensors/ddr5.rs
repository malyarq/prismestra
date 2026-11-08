//! Pure DDR5 SPD temperature conversion and explicit local binding policy.
//! Conversion follows work/ddr5-source/SPDTemperatureConverter.cs (MPL-2.0,
//! Copyright (c) 2025 Florian K.), CheckAndConvertTemperature. Input is already
//! host-endian u16: no byte swap and no masking of unrelated register bits.
use super::Reading;

pub const POLL_INTERVAL_MS: u64 = 1000;

/// All controller words are binding identity. The high half of controller[2]
/// is the subsystem identity, not a revision or an ignorable field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub controller: [u64; 3],
    pub port: u8,
    pub address: u8,
}
impl Binding {
    pub fn validate(self) -> Result<(), String> {
        let base = match self.port {
            0 => 0x0b00,
            1 => 0x0b20,
            _ => return Err("DDR5 binding port must be 0 or 1".into()),
        };
        if self.controller[0] != u64::from_le_bytes(*b"PIIX4\0\0\0")
            || self.controller[1] != base
            || self.controller[2] as u32 != 0x790b1022
            || !(0x50..=0x57).contains(&self.address)
        {
            return Err(
                "DDR5 requires an explicit supported PIIX4 controller, port and SPD address".into(),
            );
        }
        Ok(())
    }
}
pub trait Backend {
    /// Pure identity query; construction must not read hardware.
    fn binding(&self) -> Binding;
    fn read_temperature(&mut self) -> Result<u16, String>;
}

/// Source conversion over the entire u16 domain. Range policy belongs to Provider.
pub fn decode_temperature(raw: u16) -> f64 {
    let temperature = if raw & 0x1000 != 0 {
        f32::from(raw & !0x1000) * 0.0625_f32 - 256.0_f32
    } else {
        f32::from(raw) * 0.0625_f32
    };
    f64::from(temperature)
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
    binding: Binding,
    last_clock: Option<u64>,
    next_poll: Option<u64>,
    status: Status,
}
impl<B: Backend> Provider<B> {
    /// Only validates the explicit binding and backend's pure identity; no reads.
    pub fn new(binding: Binding, backend: B) -> Result<Self, String> {
        binding.validate()?;
        if backend.binding() != binding {
            return Err("Selected DDR5 binding differs from backend".into());
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
    /// Not-due returns None without touching the backend or refreshing a cached
    /// observation. Every attempted read, including failure, consumes 1 second.
    pub fn poll(&mut self, now_ms: u64) -> Result<Option<Reading>, String> {
        if self.status.terminal {
            return Err(self
                .status
                .error
                .clone()
                .unwrap_or_else(|| "DDR5 sensor is stopped".into()));
        }
        if now_ms == 0 || self.last_clock.is_some_and(|last| now_ms < last) {
            return Err(self.error("DDR5 sensor monotonic clock is invalid".into(), true));
        }
        self.last_clock = Some(now_ms);
        if self.next_poll.is_some_and(|due| now_ms < due) {
            return Ok(None);
        }
        let Some(next) = now_ms.checked_add(POLL_INTERVAL_MS) else {
            return Err(self.error("DDR5 sensor cadence clock overflow".into(), true));
        };
        self.next_poll = Some(next);
        self.status.last_attempt_ms = Some(now_ms);
        if self.backend.as_ref().unwrap().binding() != self.binding {
            return Err(self.error("Selected DDR5 binding changed".into(), true));
        }
        let raw = match self.backend.as_mut().unwrap().read_temperature() {
            Ok(raw) => raw,
            Err(error) => return Err(self.error(error, false)),
        };
        let celsius = decode_temperature(raw);
        if !(-20.0..=150.0).contains(&celsius) {
            return Err(self.error("DDR5 SPD temperature outside supported range".into(), false));
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
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };
    fn binding() -> Binding {
        Binding {
            controller: [
                u64::from_le_bytes(*b"PIIX4\0\0\0"),
                0x0b00,
                0x12345678_790b1022,
            ],
            port: 0,
            address: 0x50,
        }
    }
    struct State {
        binding: Binding,
        raw: u16,
        fail: bool,
        binding_queries: Cell<usize>,
        reads: usize,
        drops: usize,
    }
    struct Fake(Rc<RefCell<State>>);
    impl Backend for Fake {
        fn binding(&self) -> Binding {
            let s = self.0.borrow();
            s.binding_queries.set(s.binding_queries.get() + 1);
            s.binding
        }
        fn read_temperature(&mut self) -> Result<u16, String> {
            let mut s = self.0.borrow_mut();
            s.reads += 1;
            if s.fail {
                Err("synthetic SPD read failure".into())
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
        let s = Rc::new(RefCell::new(State {
            binding: binding(),
            raw: 0x0190,
            fail: false,
            binding_queries: Cell::new(0),
            reads: 0,
            drops: 0,
        }));
        (Provider::new(binding(), Fake(s.clone())).unwrap(), s)
    }
    #[test]
    fn source_conversion_endian_sign_and_unmasked_bits() {
        assert_eq!(decode_temperature(0x0190), 25.0);
        assert_eq!(decode_temperature(0x1ff0), -1.0);
        assert_eq!(decode_temperature(0), 0.0);
        assert_eq!(decode_temperature(0x1000), -256.0);
        assert_eq!(decode_temperature(0x0fff), 255.9375);
        assert_eq!(decode_temperature(0x1fff), -0.0625);
        assert_eq!(decode_temperature(0x2000), 512.0);
        assert_eq!(decode_temperature(0xffff), 3583.9375);
        // Exhaustive algebraic check: all source results are exact multiples
        // of 1/16, so integer sixteenths independently express the same branch.
        for raw in 0..=u16::MAX {
            let units = if raw & 0x1000 != 0 {
                i32::from(raw & !0x1000) - 4096
            } else {
                i32::from(raw)
            };
            assert_eq!(decode_temperature(raw), f64::from(units) / 16.0);
        }
    }
    #[test]
    fn binding_requires_exact_controller_base_port_and_address() {
        let b = binding();
        b.validate().unwrap();
        let mut second = b;
        second.port = 1;
        second.controller[1] = 0x0b20;
        second.validate().unwrap();
        for address in 0x50..=0x57 {
            let mut valid = b;
            valid.address = address;
            valid.validate().unwrap();
        }
        for address in [0, 0x4f, 0x58, 0xff] {
            let mut wrong = b;
            wrong.address = address;
            assert!(wrong.validate().is_err());
        }
        for port in [2, 255] {
            let mut wrong = b;
            wrong.port = port;
            assert!(wrong.validate().is_err());
        }
        for (word, value) in [
            (0, 0),
            (0, b.controller[0] ^ (1 << 63)),
            (1, 0x0b20),
            (1, 0x100000b00),
            (2, 0x12345678_790b8086),
        ] {
            let mut wrong = b;
            wrong.controller[word] = value;
            assert!(wrong.validate().is_err());
        }
        // High controller[2] is accepted as subsystem data but compared in full.
        let (_, s) = fixture();
        let mut other = b;
        other.controller[2] ^= 1 << 32;
        other.validate().unwrap();
        assert!(Provider::new(other, Fake(s.clone())).is_err());
        assert_eq!(s.borrow().reads, 0);
    }
    #[test]
    fn constructor_and_not_due_never_read_or_refresh_cached_observation() {
        let (mut p, s) = fixture();
        assert_eq!(s.borrow().reads, 0);
        assert_eq!(p.snapshot().last_attempt_ms, None);
        let first = p.poll(1).unwrap().unwrap();
        assert_eq!(
            first,
            Reading {
                celsius: 25.0,
                observed_ms: 1
            }
        );
        let queries = s.borrow().binding_queries.get();
        assert_eq!(p.poll(1000).unwrap(), None);
        assert_eq!(p.poll(1000).unwrap(), None);
        assert_eq!(s.borrow().reads, 1);
        assert_eq!(s.borrow().binding_queries.get(), queries);
        assert_eq!(p.snapshot().reading, Some(first));
        assert_eq!(p.latest(1001, 1000), Some(first));
        assert!(p.latest(1002, 1000).is_none());
        assert!(p.latest(0, 1000).is_none());
        assert_eq!(p.poll(1001).unwrap().unwrap().observed_ms, 1001);
        assert_eq!(s.borrow().reads, 2);
    }
    #[test]
    fn failures_clear_old_data_consume_cadence_and_do_not_invent_zero() {
        let (mut p, s) = fixture();
        p.poll(10).unwrap();
        s.borrow_mut().fail = true;
        assert!(p.poll(1010).is_err());
        assert!(p.snapshot().reading.is_none());
        assert!(!p.snapshot().terminal);
        assert!(p.latest(1010, 5000).is_none());
        assert_eq!(p.poll(2009).unwrap(), None);
        assert_eq!(s.borrow().reads, 2);
        s.borrow_mut().fail = false;
        s.borrow_mut().raw = 0;
        assert_eq!(p.poll(2010).unwrap().unwrap().celsius, 0.0);
        assert!(p.snapshot().error.is_none());
        for (now, raw) in [(3010, 0x2000), (4010, 0x1ebf), (5010, 0x0961)] {
            s.borrow_mut().raw = raw;
            assert!(p.poll(now).is_err());
            assert!(p.snapshot().reading.is_none());
            assert!(!p.snapshot().terminal);
        }
        for (now, raw, expected) in [(6010, 0x1ec0, -20.0), (7010, 0x0960, 150.0)] {
            s.borrow_mut().raw = raw;
            assert_eq!(p.poll(now).unwrap().unwrap().celsius, expected);
        }
        p.stop();
        assert!(p.snapshot().reading.is_none());
        assert!(p.poll(8010).is_err());
        assert_eq!(s.borrow().drops, 1);
    }
    #[test]
    fn binding_change_and_bad_clocks_are_terminal_before_read() {
        let (mut p, s) = fixture();
        p.poll(100).unwrap();
        s.borrow_mut().binding.controller[2] ^= 1 << 32;
        assert_eq!(p.poll(1099).unwrap(), None);
        assert!(p.poll(1100).is_err());
        assert!(p.snapshot().terminal);
        assert!(p.snapshot().reading.is_none());
        assert_eq!(s.borrow().reads, 1);
        assert_eq!(s.borrow().drops, 1);
        assert!(p.poll(2100).is_err());
        assert_eq!(s.borrow().reads, 1);
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
}
