// SPDX-License-Identifier: GPL-2.0-or-later
//! Bounded ENE AUDA0-E6K5-0101 RGB-only register subset. Logical reads write
//! a register pointer; none of these functions may be called on live hardware in a stand.
use crate::lighting::color::Rgb;
pub const VERSION: &str = "AUDA0-E6K5-0101";
pub const LEDS: usize = 8;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockWrite {
    Written,
    UnsupportedWithoutTransfer,
}
pub trait Bus: Sized {
    /// Native backends hold the shared SMBus mutex over the entire closure,
    /// including ENE pointer+read/write pairs; never lock only one half.
    fn transaction<R>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<R, String>,
    ) -> Result<R, String>;
    fn write_word(&mut self, address: u8, command: u8, value: u16) -> Result<(), String>;
    fn read_byte(&mut self, address: u8, command: u8) -> Result<u8, String>;
    fn write_byte(&mut self, address: u8, command: u8, value: u8) -> Result<(), String>;
    /// UnsupportedWithoutTransfer must prove no bytes were transferred. An
    /// unknown/partial failure is Err and must never trigger automatic fallback.
    fn write_block(&mut self, address: u8, command: u8, bytes: &[u8])
    -> Result<BlockWrite, String>;
}
pub fn valid_address(address: u8) -> bool {
    (0x70..=0x76).contains(&address)
}
fn pointer<B: Bus>(bus: &mut B, address: u8, register: u16) -> Result<(), String> {
    bus.write_word(address, 0, register.swap_bytes())
}
fn read<B: Bus>(bus: &mut B, address: u8, register: u16) -> Result<u8, String> {
    pointer(bus, address, register)?;
    bus.read_byte(address, 0x81)
}
fn write<B: Bus>(bus: &mut B, address: u8, register: u16, value: u8) -> Result<(), String> {
    pointer(bus, address, register)?;
    bus.write_byte(address, 1, value)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub version: String,
    pub config: [u8; 64],
}
impl Identity {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != VERSION || self.config[2] != LEDS as u8 {
            return Err("Unsupported ENE version or LED count".into());
        }
        let mut total = 0usize;
        for i in 0..8 {
            let count = usize::from(self.config[3 + i]);
            if count > 0 && !matches!(self.config[0x13 + i], 0x8a | 5 | 0x0e) {
                return Err("ENE configuration contains a non-DRAM zone".into());
            }
            total += count;
        }
        if total != LEDS {
            return Err("ENE zone count mismatch".into());
        }
        Ok(())
    }
}
pub struct Controller<B: Bus> {
    bus: Option<B>,
    address: u8,
    identity: Identity,
    error: Option<String>,
    submitted: u64,
}
impl<B: Bus> Controller<B> {
    pub fn connect(mut bus: B, address: u8) -> Result<Self, String> {
        if !valid_address(address) {
            return Err(
                "ENE target must be an explicit existing address0x70..0x76; remap0x77 is forbidden"
                    .into(),
            );
        }
        let identity = bus.transaction(|bus| {
            let mut name = [0u8; 16];
            for (i, c) in name.iter_mut().enumerate() {
                *c = read(bus, address, 0x1000 + i as u16)?;
            }
            name[15] = 0;
            let end = name.iter().position(|b| *b == 0).unwrap_or(16);
            let version = String::from_utf8(name[..end].to_vec())
                .map_err(|_| "Invalid ENE version string")?;
            let mut config = [0; 64];
            for (i, c) in config.iter_mut().enumerate() {
                *c = read(bus, address, 0x1c00 + i as u16)?;
            }
            let identity = Identity { version, config };
            identity.validate()?;
            Ok(identity)
        })?;
        bus.transaction(|bus| {
            write(bus, address, 0x8020, 1)?;
            write(bus, address, 0x80a0, 1)
        })?;
        Ok(Self {
            bus: Some(bus),
            address,
            identity,
            error: None,
            submitted: 0,
        })
    }
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    pub fn failure(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub fn submitted(&self) -> u64 {
        self.submitted
    }
    pub fn stop(&mut self) {
        self.bus = None;
    }
    pub fn frame(&mut self, colors: &[Rgb]) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        if colors.len() != LEDS {
            return Err("ENE frame requires8LEDs".into());
        }
        let bus = self.bus.as_mut().ok_or("ENE output is stopped")?;
        let address = self.address;
        let result = bus.transaction(|bus| {
            for (i, color) in colors.iter().enumerate() {
                let register = 0x8100 + (i * 3) as u16;
                let bytes = [color.r, color.b, color.g];
                pointer(bus, address, register)?;
                if bus.write_block(address, 3, &bytes)? == BlockWrite::UnsupportedWithoutTransfer {
                    // Re-establish the pointer before byte fallback, never continue
                    // after an ambiguous failure at a potentially advanced pointer.
                    pointer(bus, address, register)?;
                    for byte in bytes {
                        bus.write_byte(address, 1, byte)?;
                    }
                }
            }
            Ok(())
        });
        if let Err(error) = result {
            self.error = Some(error.clone());
            self.bus = None;
            return Err(error);
        }
        self.submitted = self.submitted.saturating_add(1);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
    #[derive(Default)]
    struct State {
        memory: BTreeMap<u16, u8>,
        pointer: u16,
        writes: Vec<(u8, u16)>,
        rgb_writes: usize,
        fail: bool,
        unsupported: bool,
        closed: bool,
    }
    struct Fake(Rc<RefCell<State>>);
    impl Drop for Fake {
        fn drop(&mut self) {
            self.0.borrow_mut().closed = true;
        }
    }
    impl Bus for Fake {
        fn transaction<R>(
            &mut self,
            operation: impl FnOnce(&mut Self) -> Result<R, String>,
        ) -> Result<R, String> {
            operation(self)
        }
        fn write_word(&mut self, a: u8, c: u8, v: u16) -> Result<(), String> {
            assert!(valid_address(a));
            assert_eq!(c, 0);
            let mut s = self.0.borrow_mut();
            s.writes.push((c, v));
            s.pointer = v.swap_bytes();
            Ok(())
        }
        fn read_byte(&mut self, _: u8, c: u8) -> Result<u8, String> {
            assert_eq!(c, 0x81);
            let s = self.0.borrow();
            Ok(*s.memory.get(&s.pointer).unwrap_or(&0))
        }
        fn write_byte(&mut self, _: u8, c: u8, v: u8) -> Result<(), String> {
            assert_eq!(c, 1);
            let mut s = self.0.borrow_mut();
            let p = s.pointer;
            s.memory.insert(p, v);
            s.pointer += 1;
            Ok(())
        }
        fn write_block(&mut self, _: u8, c: u8, b: &[u8]) -> Result<BlockWrite, String> {
            assert_eq!(c, 3);
            let mut s = self.0.borrow_mut();
            s.rgb_writes += 1;
            if s.fail {
                return Err("Ambiguous block failure".into());
            }
            if s.unsupported {
                return Ok(BlockWrite::UnsupportedWithoutTransfer);
            }
            let p = s.pointer;
            for (i, b) in b.iter().enumerate() {
                s.memory.insert(p + i as u16, *b);
            }
            s.pointer += b.len() as u16;
            Ok(BlockWrite::Written)
        }
    }
    fn state() -> Rc<RefCell<State>> {
        let mut s = State::default();
        for (i, b) in VERSION.bytes().enumerate() {
            s.memory.insert(0x1000 + i as u16, b);
        }
        s.memory.insert(0x1c02, 8);
        s.memory.insert(0x1c03, 8);
        s.memory.insert(0x1c13, 0x8a);
        Rc::new(RefCell::new(s))
    }
    #[test]
    fn identity_gates_before_rgb_mode() {
        let s = state();
        s.borrow_mut().memory.insert(0x1c02, 9);
        assert!(Controller::connect(Fake(s.clone()), 0x71).is_err());
        assert!(!s.borrow().memory.contains_key(&0x8020));
        assert!(s.borrow().closed);
    }
    #[test]
    fn direct_colors_rbg_and_explicit_fallback() {
        for fallback in [false, true] {
            let s = state();
            s.borrow_mut().unsupported = fallback;
            let mut c = Controller::connect(Fake(s.clone()), 0x73).unwrap();
            c.frame(&[Rgb::new(1, 2, 3); 8]).unwrap();
            let s = s.borrow();
            for i in 0..8 {
                assert_eq!(
                    (
                        s.memory[&(0x8100 + 3 * i)],
                        s.memory[&(0x8101 + 3 * i)],
                        s.memory[&(0x8102 + 3 * i)]
                    ),
                    (1, 3, 2)
                );
            }
            assert_eq!(s.memory[&0x80a0], 1);
        }
    }
    #[test]
    fn ambiguous_failure_is_terminal_no_fallback_or_retry() {
        let s = state();
        let mut c = Controller::connect(Fake(s.clone()), 0x71).unwrap();
        s.borrow_mut().fail = true;
        assert!(c.frame(&[Rgb::default(); 8]).is_err());
        let n = s.borrow().rgb_writes;
        assert!(c.frame(&[Rgb::default(); 8]).is_err());
        assert_eq!(s.borrow().rgb_writes, n);
        assert!(s.borrow().closed);
    }

    /// The input is emitted by the unchanged C++ driver, not by this Rust port.
    /// Replay consumes every request and supplies only the recorded read values.
    struct Replay(Rc<RefCell<std::collections::VecDeque<serde_json::Value>>>);
    impl Replay {
        fn next(&self, prefix: serde_json::Value) -> serde_json::Value {
            let op = self
                .0
                .borrow_mut()
                .pop_front()
                .expect("unexpected extra I/O");
            let actual = op.as_array().unwrap();
            let expected = prefix.as_array().unwrap();
            assert_eq!(&actual[..expected.len()], expected.as_slice());
            op
        }
    }
    impl Bus for Replay {
        fn transaction<R>(
            &mut self,
            f: impl FnOnce(&mut Self) -> Result<R, String>,
        ) -> Result<R, String> {
            f(self)
        }
        fn write_word(&mut self, a: u8, c: u8, v: u16) -> Result<(), String> {
            self.next(serde_json::json!(["W", a, c, v]));
            Ok(())
        }
        fn read_byte(&mut self, a: u8, c: u8) -> Result<u8, String> {
            Ok(self.next(serde_json::json!(["R", a, c]))[3]
                .as_u64()
                .unwrap() as u8)
        }
        fn write_byte(&mut self, a: u8, c: u8, v: u8) -> Result<(), String> {
            self.next(serde_json::json!(["Y", a, c, v]));
            Ok(())
        }
        fn write_block(&mut self, a: u8, c: u8, b: &[u8]) -> Result<BlockWrite, String> {
            let hex: String = b.iter().map(|v| format!("{v:02X}")).collect();
            let op = self.next(serde_json::json!(["B", a, c, hex]));
            match op[4].as_i64().unwrap() {
                0 => Ok(BlockWrite::Written),
                -1 => Ok(BlockWrite::UnsupportedWithoutTransfer),
                _ => panic!("unexpected native result"),
            }
        }
    }
    #[test]
    fn unchanged_cpp_constructor_direct_and_all_frame_transactions() {
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/ene-native.json")).unwrap();
        let frames = corpus["frames"].as_array().unwrap();
        assert_eq!(frames.len(), 128);
        for frame in frames {
            let address = frame["address"].as_u64().unwrap() as u8;
            let ctor = corpus["constructors"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["address"] == frame["address"])
                .unwrap();
            let mut ops: std::collections::VecDeque<_> =
                ctor["trace"].as_array().unwrap().iter().cloned().collect();
            ops.extend(frame["direct_trace"].as_array().unwrap().iter().cloned());
            let mut last_pointer = None;
            for op in frame["frame_trace"].as_array().unwrap() {
                if op[0] == "W" {
                    last_pointer = Some(op.clone());
                }
                ops.push_back(op.clone());
                if op[0] == "B" && op[4] == -1 {
                    // Intentional safety change: explicitly unsupported/no-progress
                    // fallback starts from a fresh selector instead of trusting it.
                    ops.push_back(last_pointer.clone().unwrap());
                }
            }
            let shared = Rc::new(RefCell::new(ops));
            let mut controller = Controller::connect(Replay(shared.clone()), address).unwrap();
            let hex = frame["rgb_hex"].as_str().unwrap();
            assert_eq!(hex.len(), 48);
            let bytes: Vec<u8> = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            let colors: Vec<Rgb> = bytes
                .as_chunks::<3>()
                .0
                .iter()
                .map(|b| Rgb::new(b[0], b[1], b[2]))
                .collect();
            controller.frame(&colors).unwrap();
            assert!(shared.borrow().is_empty());
            assert_eq!(controller.submitted(), 1);
        }
    }
}
