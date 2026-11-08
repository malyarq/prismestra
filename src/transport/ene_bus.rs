//! Stateful, address-bound ENE boundary over an injected primary-PIIX4 executor.
//! No driver/module discovery, native calls, port selection or clock changes.
use super::{ene, pawnio};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

/// The native owner must verify provider/module pins and primary-port ownership
/// under the same shared lock before executing. Output must contain only actual
/// successful bytes (maximum 40); errors must never be converted into output.
/// Native output storage must be zero-initialized before the OS operation.
pub trait Executor {
    fn smbus_transfer(&mut self, transfer: &pawnio::Transfer) -> Result<Vec<u8>, String>;
}

/// Clones must refer to the same global `pawnio::SHARED_MUTEX` lock. The guard
/// releases it on Drop. Acquisition must be bounded; abandonment is a failure.
pub trait SharedMutex: Clone {
    type Guard<'a>
    where
        Self: 'a;
    fn lock(&self) -> Result<Self::Guard<'_>, String>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockCapability {
    /// No block operation is attempted; fallback is provably transfer-free.
    ByteOnly,
    /// Explicit caller attestation of verified three-byte block support.
    /// The pinned module's support remains UNKNOWN until separately verified.
    VerifiedThreeByteWrites,
}

pub struct EneBus<E: Executor, M: SharedMutex> {
    executor: Option<E>,
    mutex: M,
    address: u8,
    capability: BlockCapability,
    active: bool,
    pointer: Option<u16>,
    failed: Option<String>,
}

impl<E: Executor, M: SharedMutex> EneBus<E, M> {
    pub fn new(
        address: u8,
        executor: E,
        mutex: M,
        capability: BlockCapability,
    ) -> Result<Self, String> {
        if !ene::valid_address(address) {
            return Err("Explicit ENE address must be 0x70..=0x76".into());
        }
        Ok(Self {
            executor: Some(executor),
            mutex,
            address,
            capability,
            active: false,
            pointer: None,
            failed: None,
        })
    }
    pub fn address(&self) -> u8 {
        self.address
    }
    pub fn failure(&self) -> Option<&str> {
        self.failed.as_deref()
    }

    fn terminal(&mut self, error: String) -> String {
        self.pointer = None;
        if self.failed.is_none() {
            self.failed = Some(error);
        }
        self.executor = None;
        self.failed.clone().unwrap()
    }
    fn check(&self, address: u8) -> Result<(), String> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        if !self.active || self.executor.is_none() {
            return Err("ENE operation requires an active transaction".into());
        }
        if address != self.address {
            return Err("ENE address differs from explicit binding".into());
        }
        Ok(())
    }
    fn execute(
        &mut self,
        transfer: pawnio::Transfer,
        minimum_words: usize,
    ) -> Result<Vec<u64>, String> {
        // Transfer constructs all nine input words with zero union/padding.
        let result = self
            .executor
            .as_mut()
            .ok_or("ENE executor is closed")?
            .smbus_transfer(&transfer);
        let words = result.and_then(|bytes| pawnio::decode_words(&bytes, minimum_words, 5));
        words.map_err(|error| self.terminal(error))
    }
    fn read_register(register: u16) -> bool {
        matches!(register, 0x1000..=0x100f | 0x1c00..=0x1c3f)
    }
    fn rgb_register(register: u16) -> bool {
        (0x8100..=0x8117).contains(&register)
    }
}

impl<E: Executor, M: SharedMutex> ene::Bus for EneBus<E, M> {
    fn transaction<R>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<R, String>,
    ) -> Result<R, String> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        if self.active {
            return Err("Nested ENE transactions are forbidden".into());
        }
        let mutex = self.mutex.clone();
        let guard = match mutex.lock() {
            Ok(guard) => guard,
            Err(error) => return Err(self.terminal(error)),
        };
        self.active = true;
        self.pointer = None;
        // Catch only to invalidate state and close this executor while the lock
        // is still held, then resume the original panic. No unsafe/self aliasing.
        let result = catch_unwind(AssertUnwindSafe(|| operation(self)));
        self.active = false;
        self.pointer = None;
        let result = match result {
            Ok(Ok(value)) if self.failed.is_none() => Ok(value),
            Ok(Ok(_)) => Err(self.failed.clone().unwrap()),
            Ok(Err(error)) => Err(self.terminal(error)),
            Err(payload) => {
                self.terminal("ENE transaction panicked; executor closed".into());
                drop(guard);
                resume_unwind(payload)
            }
        };
        drop(guard);
        result
    }

    fn write_word(&mut self, address: u8, command: u8, value: u16) -> Result<(), String> {
        self.pointer = None;
        self.check(address)?;
        if command != 0 {
            return Err("Only ENE selector word command 0 is permitted".into());
        }
        let register = value.swap_bytes();
        let transfer = pawnio::Transfer::select_register(address, register)?;
        self.execute(transfer, 0)?;
        self.pointer = Some(register);
        Ok(())
    }

    fn read_byte(&mut self, address: u8, command: u8) -> Result<u8, String> {
        let pointer = self.pointer.take();
        self.check(address)?;
        if command != 0x81 || !pointer.is_some_and(Self::read_register) {
            return Err("ENE read requires an identity/config selector and command 0x81".into());
        }
        let words = self.execute(pawnio::Transfer::read_selected_byte(address)?, 1)?;
        // Intentional strict validation: upper bits are not silently truncated.
        // Whether the pinned native ABI guarantees these bits zero is UNKNOWN.
        u8::try_from(words[0]).map_err(|_| self.terminal("ENE BYTE_DATA result exceeds 255".into()))
    }

    fn write_byte(&mut self, address: u8, command: u8, value: u8) -> Result<(), String> {
        let pointer = self.pointer.take();
        self.check(address)?;
        let register = pointer.ok_or("ENE byte write has no selected register")?;
        if command != 1
            || !(Self::rgb_register(register)
                || (matches!(register, 0x8020 | 0x80a0) && value == 1))
        {
            return Err("ENE byte write outside direct mode/apply=1 or RGB registers".into());
        }
        self.execute(pawnio::Transfer::write_selected_byte(address, value)?, 0)?;
        if Self::rgb_register(register) && register < 0x8117 {
            self.pointer = Some(register + 1);
        }
        Ok(())
    }

    fn write_block(
        &mut self,
        address: u8,
        command: u8,
        bytes: &[u8],
    ) -> Result<ene::BlockWrite, String> {
        let pointer = self.pointer.take();
        self.check(address)?;
        let register = pointer.ok_or("ENE RGB block has no selected register")?;
        if command != 3
            || bytes.len() != 3
            || !Self::rgb_register(register)
            || register > 0x8115
            || !(register - 0x8100).is_multiple_of(3)
        {
            return Err(
                "ENE block must be one aligned three-byte RGB value in 0x8100..0x8117".into(),
            );
        }
        if self.capability == BlockCapability::ByteOnly {
            // No execute call, no inferred support and no possible byte transfer.
            return Ok(ene::BlockWrite::UnsupportedWithoutTransfer);
        }
        let rgb = bytes.try_into().map_err(|_| "Invalid RGB block size")?;
        self.execute(pawnio::Transfer::write_selected_rgb(address, rgb)?, 0)?;
        if register < 0x8115 {
            self.pointer = Some(register + 3);
        }
        Ok(ene::BlockWrite::Written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lighting::color::Rgb, transport::ene::Bus};
    use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
    #[derive(Default)]
    struct State {
        held: bool,
        locks: usize,
        unlocks: usize,
        calls: usize,
        drops: usize,
        pointer: u16,
        memory: BTreeMap<u16, u8>,
        failure: bool,
        response: Option<Vec<u8>>,
        panic: bool,
        lock_fail: bool,
    }
    #[derive(Clone)]
    struct Mutex(Rc<RefCell<State>>);
    struct Guard(Rc<RefCell<State>>);
    impl Drop for Guard {
        fn drop(&mut self) {
            let mut s = self.0.borrow_mut();
            assert!(s.held);
            s.held = false;
            s.unlocks += 1;
        }
    }
    impl SharedMutex for Mutex {
        type Guard<'a> = Guard;
        fn lock(&self) -> Result<Guard, String> {
            let mut s = self.0.borrow_mut();
            if s.lock_fail {
                return Err("synthetic lock error".into());
            }
            assert!(!s.held);
            s.held = true;
            s.locks += 1;
            Ok(Guard(self.0.clone()))
        }
    }
    struct Fake(Rc<RefCell<State>>);
    impl Drop for Fake {
        fn drop(&mut self) {
            self.0.borrow_mut().drops += 1;
        }
    }
    impl Executor for Fake {
        fn smbus_transfer(&mut self, t: &pawnio::Transfer) -> Result<Vec<u8>, String> {
            let mut s = self.0.borrow_mut();
            assert!(s.held);
            s.calls += 1;
            if s.panic {
                drop(s);
                panic!("synthetic executor panic");
            }
            if s.failure {
                return Err("ambiguous transfer failure".into());
            }
            if let Some(response) = &s.response {
                return Ok(response.clone());
            }
            let w = t.words();
            assert!((0x70..=0x76).contains(&w[0]));
            assert!(w[5..].iter().all(|&v| v == 0));
            match (w[1], w[2], w[3]) {
                (0, 0, 3) => {
                    s.pointer = (w[4] as u16).swap_bytes();
                    Ok(vec![])
                }
                (1, 0x81, 2) => {
                    assert_eq!(w[4], 0);
                    Ok(u64::from(*s.memory.get(&s.pointer).unwrap_or(&0))
                        .to_le_bytes()
                        .to_vec())
                }
                (0, 1, 2) => {
                    let p = s.pointer;
                    s.memory.insert(p, w[4] as u8);
                    s.pointer += 1;
                    Ok(vec![])
                }
                (0, 3, 5) => {
                    let b = w[4].to_le_bytes();
                    assert_eq!(b[0], 3);
                    assert!(b[4..].iter().all(|&v| v == 0));
                    for value in &b[1..4] {
                        let p = s.pointer;
                        s.memory.insert(p, *value);
                        s.pointer += 1;
                    }
                    Ok(vec![])
                }
                _ => panic!("unexpected transfer"),
            }
        }
    }
    type TestBus = EneBus<Fake, Mutex>;
    fn fixture(block: BlockCapability) -> (TestBus, Rc<RefCell<State>>) {
        let state = Rc::new(RefCell::new(State::default()));
        for (i, b) in ene::VERSION.bytes().enumerate() {
            state.borrow_mut().memory.insert(0x1000 + i as u16, b);
        }
        for (r, b) in [(0x1c02, 8), (0x1c03, 8), (0x1c13, 0x8a)] {
            state.borrow_mut().memory.insert(r, b);
        }
        (
            EneBus::new(0x71, Fake(state.clone()), Mutex(state.clone()), block).unwrap(),
            state,
        )
    }
    fn select(bus: &mut TestBus, r: u16) -> Result<(), String> {
        bus.write_word(0x71, 0, r.swap_bytes())
    }
    #[test]
    fn actual_controller_connect_and_frame_both_capabilities_lock_entire_transactions() {
        for block in [
            BlockCapability::ByteOnly,
            BlockCapability::VerifiedThreeByteWrites,
        ] {
            let (bus, state) = fixture(block);
            let mut c = ene::Controller::connect(bus, 0x71).unwrap();
            c.frame(&[Rgb::new(0xaa, 2, 3); 8]).unwrap();
            let s = state.borrow();
            for i in 0..8 {
                assert_eq!(
                    (
                        s.memory[&(0x8100 + i * 3)],
                        s.memory[&(0x8101 + i * 3)],
                        s.memory[&(0x8102 + i * 3)]
                    ),
                    (0xaa, 3, 2)
                );
            }
            assert_eq!((s.memory[&0x8020], s.memory[&0x80a0]), (1, 1));
            assert_eq!((s.locks, s.unlocks), (3, 3));
            assert!(!s.held);
            assert_eq!(
                s.calls,
                if block == BlockCapability::ByteOnly {
                    204
                } else {
                    180
                }
            );
        }
    }
    #[test]
    fn no_outside_or_nested_or_cross_transaction_pointer_use() {
        let (mut b, s) = fixture(BlockCapability::ByteOnly);
        assert!(select(&mut b, 0x1000).is_err());
        assert!(b.read_byte(0x71, 0x81).is_err());
        assert!(b.write_byte(0x71, 1, 1).is_err());
        assert!(b.write_block(0x71, 3, &[1, 2, 3]).is_err());
        assert_eq!(s.borrow().calls, 0);
        b.transaction(|b| {
            assert!(s.borrow().held);
            assert!(b.transaction(|_| Ok(())).is_err());
            select(b, 0x1000)?;
            assert!(s.borrow().held);
            assert_eq!(b.read_byte(0x71, 0x81)?, b'A');
            select(b, 0x8100)
        })
        .unwrap();
        assert!(b.transaction(|b| b.write_byte(0x71, 1, 1)).is_err());
        assert_eq!(s.borrow().calls, 3);
        assert!(!s.borrow().held);
    }
    #[test]
    fn rejects_bad_command_direction_address_register_value_length_and_overshoot_before_execute() {
        for bad in 0..17 {
            let (mut b, s) = fixture(BlockCapability::VerifiedThreeByteWrites);
            b.transaction(|b| {
                let selected = if bad == 12 {
                    0x8115
                } else if bad == 13 {
                    0x8116
                } else if bad == 14 {
                    0x8117
                } else {
                    0x1000
                };
                select(b, selected)?;
                let before = s.borrow().calls;
                let result = match bad {
                    0 => b.write_word(0x71, 1, 0x0010),
                    1 => b.write_word(0x72, 0, 0x0010),
                    2 => select(b, 0x80f9),
                    3 => select(b, 0x0050),
                    4 => select(b, 0xffff),
                    5 => b.read_byte(0x71, 1).map(|_| ()),
                    6 => b.read_byte(0x72, 0x81).map(|_| ()),
                    7 => b.write_byte(0x71, 1, 1),
                    8 => b.write_block(0x71, 3, &[1, 2, 3]).map(|_| ()),
                    9 => {
                        select(b, 0x8020)?;
                        let n = s.borrow().calls;
                        assert!(b.write_byte(0x71, 1, 0).is_err());
                        assert_eq!(s.borrow().calls, n);
                        return Ok(());
                    }
                    10 => {
                        select(b, 0x80a0)?;
                        let n = s.borrow().calls;
                        assert!(b.write_byte(0x71, 1, 0xaa).is_err());
                        assert_eq!(s.borrow().calls, n);
                        return Ok(());
                    }
                    11 => b.write_byte(0x71, 0x81, 1),
                    12 => b.write_block(0x71, 3, &[1, 2, 3, 4]).map(|_| ()),
                    13 => b.write_block(0x71, 3, &[1, 2, 3]).map(|_| ()),
                    14 => {
                        b.write_byte(0x71, 1, 7)?;
                        let n = s.borrow().calls;
                        assert!(b.write_byte(0x71, 1, 8).is_err());
                        assert_eq!(s.borrow().calls, n);
                        return Ok(());
                    }
                    15 => b.write_block(0x71, 3, &[]).map(|_| ()),
                    _ => b.write_block(0x71, 1, &[1, 2, 3]).map(|_| ()),
                };
                assert!(result.is_err(), "case {bad}");
                assert_eq!(s.borrow().calls, before);
                Ok(())
            })
            .unwrap();
        }
        let (_, s) = fixture(BlockCapability::ByteOnly);
        for address in [0, 0x50, 0x6f, 0x77, 255] {
            assert!(
                EneBus::new(
                    address,
                    Fake(s.clone()),
                    Mutex(s.clone()),
                    BlockCapability::ByteOnly
                )
                .is_err()
            );
        }
    }
    #[test]
    fn explicit_byte_only_block_never_executes_but_ambiguous_failure_never_falls_back() {
        let (mut b, s) = fixture(BlockCapability::ByteOnly);
        b.transaction(|b| {
            select(b, 0x8100)?;
            let n = s.borrow().calls;
            assert_eq!(
                b.write_block(0x71, 3, &[1, 3, 2])?,
                ene::BlockWrite::UnsupportedWithoutTransfer
            );
            assert_eq!(s.borrow().calls, n);
            Ok(())
        })
        .unwrap();
        let (mut b, s) = fixture(BlockCapability::VerifiedThreeByteWrites);
        assert!(
            b.transaction(|b| {
                select(b, 0x8100)?;
                s.borrow_mut().failure = true;
                b.write_block(0x71, 3, &[1, 3, 2])
            })
            .is_err()
        );
        assert!(b.failure().is_some());
        assert_eq!(s.borrow().drops, 1);
        let n = s.borrow().calls;
        assert!(b.transaction(|_| Ok(())).is_err());
        assert_eq!(s.borrow().calls, n);
        assert!(!s.borrow().held);
    }
    #[test]
    fn strict_actual_counts_and_high_read_bits_are_terminal_no_failed_read_255() {
        for bytes in [
            vec![],
            vec![0; 7],
            vec![0; 9],
            vec![0; 48],
            256u64.to_le_bytes().to_vec(),
        ] {
            let (mut b, s) = fixture(BlockCapability::ByteOnly);
            assert!(
                b.transaction(|b| {
                    select(b, 0x1000)?;
                    s.borrow_mut().response = Some(bytes);
                    b.read_byte(0x71, 0x81)
                })
                .is_err()
            );
            assert_eq!(s.borrow().drops, 1);
            assert!(b.failure().is_some());
            assert!(!s.borrow().held);
        }
        let (mut b, s) = fixture(BlockCapability::ByteOnly);
        assert!(
            b.transaction(|b| {
                select(b, 0x1000)?;
                s.borrow_mut().failure = true;
                b.read_byte(0x71, 0x81)
            })
            .is_err()
        );
        assert_eq!(s.borrow().drops, 1);
        for count in [0, 8, 16, 24, 32, 40] {
            let (mut b, s) = fixture(BlockCapability::ByteOnly);
            s.borrow_mut().response = Some(vec![0; count]);
            assert!(b.transaction(|b| select(b, 0x1000)).is_ok());
        }
        for count in [1, 7, 9, 39, 41, 48] {
            let (mut b, s) = fixture(BlockCapability::ByteOnly);
            s.borrow_mut().response = Some(vec![0; count]);
            assert!(b.transaction(|b| select(b, 0x1000)).is_err());
            assert_eq!(s.borrow().drops, 1);
        }
    }
    #[test]
    fn panic_and_lock_failure_release_and_close_without_retry() {
        let (mut b, s) = fixture(BlockCapability::ByteOnly);
        let result = catch_unwind(AssertUnwindSafe(|| {
            b.transaction(|b| {
                select(b, 0x1000)?;
                s.borrow_mut().panic = true;
                b.read_byte(0x71, 0x81)
            })
        }));
        assert!(result.is_err());
        assert_eq!(
            (s.borrow().locks, s.borrow().unlocks, s.borrow().drops),
            (1, 1, 1)
        );
        assert!(!s.borrow().held);
        assert!(b.failure().is_some());
        let n = s.borrow().calls;
        assert!(b.transaction(|_| Ok(())).is_err());
        assert_eq!(s.borrow().calls, n);
        let (mut b, s) = fixture(BlockCapability::ByteOnly);
        s.borrow_mut().lock_fail = true;
        assert!(b.transaction(|_| Ok(())).is_err());
        assert_eq!((s.borrow().calls, s.borrow().drops), (0, 1));
    }
}
