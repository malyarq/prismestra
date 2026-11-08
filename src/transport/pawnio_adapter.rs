//! Injected PawnIO policy only: no native device open, driver/module discovery,
//! installation, port selection, OS power/sleep changes or SMBus scan.
//! A freshly loaded VM uses the source-backed always-wait polling policy.
use super::{ene, ene_bus, pawnio};
use crate::runtime::output::Cancellation;

/// Must return the actual successful byte count. OS failure is Err, never output.
/// The caller supplies zeroed output storage and validates counts before parsing.
pub trait Device {
    fn ioctl(
        &mut self,
        code: u32,
        input: &[u8],
        output: &mut [u8],
        cancel: &Cancellation,
    ) -> Result<usize, String>;
}

/// Nonzero epoch only while the current thread owns the shared RAII lock.
/// Every acquisition receives a strictly newer epoch; clones share this state.
pub trait GuardedMutex: ene_bus::SharedMutex {
    fn epoch(&self) -> Result<u64, String>;
}

pub struct VerifiedPiix4<D: Device, M: GuardedMutex> {
    device: Option<D>,
    mutex: M,
    address: u8,
    cancel: Cancellation,
    identity: [u64; 3],
    epoch: u64,
    failed: Option<String>,
}

impl<D: Device, M: GuardedMutex> VerifiedPiix4<D, M> {
    /// The caller already holds the shared lock and opened this exact device
    /// under it. Piix4Module's production constructor verifies the pinned hash.
    pub fn connect(
        module: &pawnio::Piix4Module,
        device: D,
        mutex: M,
        address: u8,
        cancel: Cancellation,
    ) -> Result<Self, String> {
        if !ene::valid_address(address) {
            return Err("Explicit ENE address must be 0x70..=0x76".into());
        }
        let mut verified = Self {
            device: Some(device),
            mutex,
            address,
            cancel,
            identity: [0; 3],
            epoch: 0,
            failed: None,
        };
        let result = (|| {
            let epoch = verified.live_epoch()?;
            let count = verified.ioctl(pawnio::IOCTL_LOAD, module.bytes(), &mut [], epoch)?;
            if count != 0 {
                return Err("PawnIO module load returned unexpected bytes".into());
            }
            // This sets only the fresh VM's polling/wait policy, not OS sleep.
            let count = verified.ioctl(
                pawnio::IOCTL_EXECUTE,
                &pawnio::always_sleep_request(),
                &mut [],
                epoch,
            )?;
            if count != 0 {
                return Err("PawnIO VM wait policy returned unexpected bytes".into());
            }
            let identity = verified.read_identity(epoch)?;
            verified.check_primary_port(epoch)?;
            verified.identity = identity;
            verified.epoch = epoch;
            Ok(())
        })();
        match result {
            Ok(()) => Ok(verified),
            Err(error) => Err(verified.terminal(error)),
        }
    }
    pub fn identity(&self) -> &[u64; 3] {
        &self.identity
    }
    pub fn address(&self) -> u8 {
        self.address
    }
    pub fn failure(&self) -> Option<&str> {
        self.failed.as_deref()
    }

    fn terminal(&mut self, error: String) -> String {
        if self.failed.is_none() {
            self.failed = Some(error);
        }
        self.device = None;
        self.failed.clone().unwrap()
    }
    fn live_epoch(&self) -> Result<u64, String> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        if self.cancel.cancelled() {
            return Err("PawnIO operation cancelled".into());
        }
        let epoch = self.mutex.epoch()?;
        if epoch == 0 || epoch < self.epoch {
            return Err("PawnIO shared lock is absent or epoch regressed".into());
        }
        Ok(epoch)
    }
    fn check_epoch(&self, expected: u64) -> Result<(), String> {
        if self.live_epoch()? != expected {
            return Err("PawnIO lock epoch changed during operation".into());
        }
        Ok(())
    }
    fn ioctl(
        &mut self,
        code: u32,
        input: &[u8],
        output: &mut [u8],
        epoch: u64,
    ) -> Result<usize, String> {
        self.check_epoch(epoch)?;
        output.fill(0);
        let count = self
            .device
            .as_mut()
            .ok_or("PawnIO device is closed")?
            .ioctl(code, input, output, &self.cancel)?;
        self.check_epoch(epoch)?;
        if count > output.len() {
            return Err("PawnIO returned count exceeds output capacity".into());
        }
        Ok(count)
    }
    fn read_identity(&mut self, epoch: u64) -> Result<[u64; 3], String> {
        let mut output = [0; 24];
        let count = self.ioctl(
            pawnio::IOCTL_EXECUTE,
            &pawnio::identity_request(),
            &mut output,
            epoch,
        )?;
        let words = pawnio::decode_words(&output[..count], 3, 3)?;
        let identity: [u64; 3] = words
            .try_into()
            .map_err(|_| "PawnIO identity requires exactly three words")?;
        if identity[0] != u64::from_le_bytes(*b"PIIX4\0\0\0")
            || identity[1] != 0x0b00
            || identity[2] as u32 != 0x790b1022
        {
            return Err("PawnIO identity is not the pinned primary PIIX4 provider".into());
        }
        Ok(identity)
    }
    fn check_primary_port(&mut self, epoch: u64) -> Result<(), String> {
        let mut output = [0; 8];
        let count = self.ioctl(
            pawnio::IOCTL_EXECUTE,
            &pawnio::query_port_request(),
            &mut output,
            epoch,
        )?;
        let words = pawnio::decode_words(&output[..count], 1, 1)?;
        if words[0] != 0 {
            return Err("PawnIO primary PIIX4 port is no longer selected".into());
        }
        Ok(())
    }
}

impl<D: Device, M: GuardedMutex> ene_bus::Executor for VerifiedPiix4<D, M> {
    fn smbus_transfer(&mut self, transfer: &pawnio::Transfer) -> Result<Vec<u8>, String> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        let result = (|| {
            let epoch = self.live_epoch()?;
            if transfer.words()[0] != u64::from(self.address) {
                return Err("PawnIO SMBus target differs from explicit ENE binding".into());
            }
            if epoch != self.epoch {
                if self.read_identity(epoch)? != self.identity {
                    return Err("PawnIO complete provider identity changed".into());
                }
                self.check_primary_port(epoch)?;
                self.epoch = epoch;
            }
            let mut output = [0; 40];
            let count = self.ioctl(
                pawnio::IOCTL_EXECUTE,
                &transfer.encode(),
                &mut output,
                epoch,
            )?;
            // This layer accepts source-supported 0..5 returned words. ENE Bus
            // separately requires a returned word for reads and a byte <=255.
            pawnio::decode_words(&output[..count], 0, 5)?;
            Ok(output[..count].to_vec())
        })();
        result.map_err(|error| self.terminal(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::ene_bus::{Executor, SharedMutex};
    use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

    #[derive(Clone, Debug)]
    struct Call {
        code: u32,
        input: Vec<u8>,
        capacity: usize,
    }
    #[derive(Clone)]
    enum Reply {
        Bytes(Vec<u8>, usize),
        Error,
    }
    struct State {
        held: bool,
        epoch: u64,
        drops: usize,
        calls: Vec<Call>,
        replies: BTreeMap<usize, Reply>,
        identity: [u64; 3],
        port: u64,
        cancel_at: Option<usize>,
        epoch_after: Option<(usize, u64)>,
        epoch_error: bool,
    }
    impl Default for State {
        fn default() -> Self {
            Self {
                held: false,
                epoch: 0,
                drops: 0,
                calls: vec![],
                replies: BTreeMap::new(),
                identity: [
                    u64::from_le_bytes(*b"PIIX4\0\0\0"),
                    0xb00,
                    0x01234567790b1022,
                ],
                port: 0,
                cancel_at: None,
                epoch_after: None,
                epoch_error: false,
            }
        }
    }
    #[derive(Clone)]
    struct Mutex(Rc<RefCell<State>>);
    struct Guard(Rc<RefCell<State>>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.borrow_mut().held = false;
        }
    }
    impl SharedMutex for Mutex {
        type Guard<'a> = Guard;
        fn lock(&self) -> Result<Guard, String> {
            let mut s = self.0.borrow_mut();
            assert!(!s.held);
            s.held = true;
            s.epoch += 1;
            Ok(Guard(self.0.clone()))
        }
    }
    impl GuardedMutex for Mutex {
        fn epoch(&self) -> Result<u64, String> {
            let s = self.0.borrow();
            if s.epoch_error {
                Err("synthetic epoch error".into())
            } else {
                Ok(if s.held { s.epoch } else { 0 })
            }
        }
    }
    struct Fake(Rc<RefCell<State>>);
    impl Drop for Fake {
        fn drop(&mut self) {
            self.0.borrow_mut().drops += 1;
        }
    }
    fn words(values: &[u64]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }
    fn function(input: &[u8]) -> String {
        let name = &input[..32];
        String::from_utf8(name[..name.iter().position(|&b| b == 0).unwrap_or(32)].to_vec()).unwrap()
    }
    impl Device for Fake {
        fn ioctl(
            &mut self,
            code: u32,
            input: &[u8],
            output: &mut [u8],
            cancel: &Cancellation,
        ) -> Result<usize, String> {
            let mut s = self.0.borrow_mut();
            assert!(s.held);
            assert!(!cancel.cancelled());
            assert!(output.iter().all(|&b| b == 0));
            let index = s.calls.len();
            s.calls.push(Call {
                code,
                input: input.to_vec(),
                capacity: output.len(),
            });
            if s.cancel_at == Some(index) {
                cancel.cancel_for_test();
            }
            if let Some((at, epoch)) = s.epoch_after
                && index == at
            {
                s.epoch = epoch;
            }
            if let Some(reply) = s.replies.get(&index).cloned() {
                return match reply {
                    Reply::Error => Err("synthetic IO failure".into()),
                    Reply::Bytes(bytes, count) => {
                        let n = bytes.len().min(output.len());
                        output[..n].copy_from_slice(&bytes[..n]);
                        Ok(count)
                    }
                };
            }
            if code == pawnio::IOCTL_LOAD {
                assert!(output.is_empty());
                return Ok(0);
            }
            assert_eq!(code, pawnio::IOCTL_EXECUTE);
            let bytes = match function(input).as_str() {
                "ioctl_set_sleep_mode" => {
                    assert_eq!(input, pawnio::always_sleep_request());
                    assert_eq!(&input[32..], &2u64.to_le_bytes());
                    assert!(output.is_empty());
                    vec![]
                }
                "ioctl_identity" => {
                    assert_eq!(input, pawnio::identity_request());
                    words(&s.identity)
                }
                "ioctl_piix4_port_sel" => {
                    assert_eq!(input, pawnio::query_port_request());
                    assert_eq!(&input[32..], &u64::MAX.to_le_bytes());
                    words(&[s.port])
                }
                "ioctl_smbus_xfer" => {
                    assert_eq!(input.len(), 104);
                    assert_eq!(output.len(), 40);
                    vec![]
                }
                _ => panic!("forbidden native function"),
            };
            assert!(bytes.len() <= output.len());
            output[..bytes.len()].copy_from_slice(&bytes);
            Ok(bytes.len())
        }
    }
    fn state() -> (Rc<RefCell<State>>, Mutex) {
        let s = Rc::new(RefCell::new(State::default()));
        (s.clone(), Mutex(s))
    }
    fn connect(
        s: Rc<RefCell<State>>,
        m: Mutex,
        cancel: Cancellation,
    ) -> Result<VerifiedPiix4<Fake, Mutex>, String> {
        VerifiedPiix4::connect(
            &pawnio::Piix4Module::synthetic_for_test(),
            Fake(s),
            m,
            0x71,
            cancel,
        )
    }
    fn transfer() -> pawnio::Transfer {
        pawnio::Transfer::select_register(0x71, 0x1000).unwrap()
    }

    #[test]
    fn exact_load_wait_policy_identity_query_then_transfer_and_per_epoch_revalidation() {
        let (s, m) = state();
        let module = pawnio::Piix4Module::synthetic_for_test();
        let lock = m.clone();
        let guard = lock.lock().unwrap();
        let mut v = VerifiedPiix4::connect(
            &module,
            Fake(s.clone()),
            m.clone(),
            0x71,
            Cancellation::default(),
        )
        .unwrap();
        assert_eq!(v.identity(), &s.borrow().identity);
        assert_eq!(v.address(), 0x71);
        v.smbus_transfer(&transfer()).unwrap();
        v.smbus_transfer(&transfer()).unwrap();
        {
            let s = s.borrow();
            assert_eq!(s.calls.len(), 6);
            assert_eq!(s.calls[0].code, pawnio::IOCTL_LOAD);
            assert_eq!(s.calls[0].input, module.bytes());
            assert_eq!(
                s.calls.iter().map(|c| c.capacity).collect::<Vec<_>>(),
                vec![0, 0, 24, 8, 40, 40]
            );
        }
        drop(guard);
        let _guard = lock.lock().unwrap();
        v.smbus_transfer(&transfer()).unwrap();
        let s = s.borrow();
        assert_eq!(s.calls.len(), 9);
        assert_eq!(function(&s.calls[6].input), "ioctl_identity");
        assert_eq!(function(&s.calls[7].input), "ioctl_piix4_port_sel");
        assert_eq!(function(&s.calls[8].input), "ioctl_smbus_xfer");
    }
    #[test]
    fn polling_policy_failure_or_nonzero_count_stops_before_identity_and_smbus() {
        for reply in [Reply::Error, Reply::Bytes(vec![], 1)] {
            let (s, m) = state();
            let lock = m.clone();
            let _guard = lock.lock().unwrap();
            s.borrow_mut().replies.insert(1, reply);
            assert!(connect(s.clone(), m, Cancellation::default()).is_err());
            let s = s.borrow();
            assert_eq!(s.drops, 1);
            assert_eq!(s.calls.len(), 2);
            assert_eq!(s.calls[0].code, pawnio::IOCTL_LOAD);
            assert_eq!(function(&s.calls[1].input), "ioctl_set_sleep_mode");
            assert_eq!(s.calls[1].input, pawnio::always_sleep_request());
            assert_eq!(s.calls[1].capacity, 0);
        }
    }
    #[test]
    fn no_load_without_guard_address_or_cancellation_preflight() {
        for case in 0..4 {
            let (s, m) = state();
            let lock = m.clone();
            let guard = if case == 0 {
                None
            } else {
                Some(lock.lock().unwrap())
            };
            let cancel = Cancellation::default();
            if case == 1 {
                cancel.cancel_for_test();
            }
            if case == 3 {
                s.borrow_mut().epoch_error = true;
            }
            let address = if case == 2 { 0x77 } else { 0x71 };
            assert!(
                VerifiedPiix4::connect(
                    &pawnio::Piix4Module::synthetic_for_test(),
                    Fake(s.clone()),
                    m,
                    address,
                    cancel
                )
                .is_err()
            );
            assert!(s.borrow().calls.is_empty());
            assert_eq!(s.borrow().drops, 1);
            drop(guard);
        }
    }
    #[test]
    fn failed_load_bad_identity_and_nonprimary_port_stop_before_smbus() {
        for case in 0..12 {
            let (s, m) = state();
            let lock = m.clone();
            let _guard = lock.lock().unwrap();
            match case {
                0 => {
                    s.borrow_mut().replies.insert(0, Reply::Error);
                }
                1 => {
                    s.borrow_mut().replies.insert(0, Reply::Bytes(vec![], 1));
                }
                2 => {
                    s.borrow_mut()
                        .replies
                        .insert(2, Reply::Bytes(vec![0; 16], 16));
                }
                3 => {
                    s.borrow_mut()
                        .replies
                        .insert(2, Reply::Bytes(vec![0; 24], 25));
                }
                4 => s.borrow_mut().identity[0] = 0,
                5 => s.borrow_mut().identity[1] = 0xb01,
                6 => s.borrow_mut().identity[2] = 0x790b1023,
                7 => s.borrow_mut().port = 1,
                8 => {
                    s.borrow_mut().replies.insert(3, Reply::Bytes(vec![], 0));
                }
                9 => {
                    s.borrow_mut()
                        .replies
                        .insert(3, Reply::Bytes(vec![0; 8], 9));
                }
                10 => {
                    s.borrow_mut().cancel_at = Some(0);
                }
                _ => {
                    s.borrow_mut().epoch_after = Some((0, 0));
                }
            }
            assert!(
                connect(s.clone(), m, Cancellation::default()).is_err(),
                "case{case}"
            );
            assert_eq!(s.borrow().drops, 1);
            assert!(
                s.borrow()
                    .calls
                    .iter()
                    .all(|c| c.code == pawnio::IOCTL_LOAD
                        || function(&c.input) != "ioctl_smbus_xfer")
            );
        }
    }
    #[test]
    fn new_epoch_checks_full_identity_port_and_regression_without_xfer() {
        for case in 0..5 {
            let (s, m) = state();
            let lock = m.clone();
            let guard = lock.lock().unwrap();
            let mut v = connect(s.clone(), m, Cancellation::default()).unwrap();
            drop(guard);
            let _guard = lock.lock().unwrap();
            match case {
                0 => s.borrow_mut().identity[2] ^= 1 << 63,
                1 => s.borrow_mut().port = 4,
                2 => s.borrow_mut().epoch = 0,
                3 => {
                    s.borrow_mut().replies.insert(4, Reply::Error);
                }
                _ => s.borrow_mut().held = false,
            }
            assert!(v.smbus_transfer(&transfer()).is_err());
            assert!(v.failure().is_some());
            assert_eq!(s.borrow().drops, 1);
            let n = s.borrow().calls.len();
            assert!(v.smbus_transfer(&transfer()).is_err());
            assert_eq!(s.borrow().calls.len(), n);
            assert!(
                s.borrow()
                    .calls
                    .iter()
                    .all(|c| c.code == pawnio::IOCTL_LOAD
                        || function(&c.input) != "ioctl_smbus_xfer")
            );
        }
        let (s, m) = state();
        s.borrow_mut().epoch = 2;
        let lock = m.clone();
        let _guard = lock.lock().unwrap();
        let mut v = connect(s.clone(), m, Cancellation::default()).unwrap();
        s.borrow_mut().epoch = 2; // A positive epoch regresses from cached 3.
        assert!(v.smbus_transfer(&transfer()).is_err());
        assert_eq!(s.borrow().calls.len(), 4);
        assert_eq!(s.borrow().drops, 1);
    }
    #[test]
    fn transfer_failure_invalid_count_address_and_cancel_are_terminal_without_retry() {
        for case in 0..10 {
            let (s, m) = state();
            let lock = m.clone();
            let _guard = lock.lock().unwrap();
            let cancel = Cancellation::default();
            let mut v = connect(s.clone(), m, cancel.clone()).unwrap();
            match case {
                0 => {
                    s.borrow_mut().replies.insert(4, Reply::Error);
                }
                1 => {
                    s.borrow_mut()
                        .replies
                        .insert(4, Reply::Bytes(vec![255; 40], 41));
                }
                2 => {
                    s.borrow_mut()
                        .replies
                        .insert(4, Reply::Bytes(vec![255; 7], 7));
                }
                3 => cancel.cancel_for_test(),
                4 => s.borrow_mut().cancel_at = Some(4),
                5 => s.borrow_mut().held = false,
                6 => s.borrow_mut().epoch_after = Some((4, 2)),
                7 => s.borrow_mut().epoch_error = true,
                8 => {
                    s.borrow_mut().epoch = 2;
                    s.borrow_mut()
                        .replies
                        .insert(5, Reply::Bytes(vec![0; 8], 7));
                }
                _ => {}
            }
            let request = if case == 9 {
                pawnio::Transfer::select_register(0x73, 0x1000).unwrap()
            } else {
                transfer()
            };
            assert!(v.smbus_transfer(&request).is_err(), "case{case}");
            assert!(v.failure().is_some());
            assert_eq!(s.borrow().drops, 1);
            let n = s.borrow().calls.len();
            assert!(v.smbus_transfer(&transfer()).is_err());
            assert_eq!(s.borrow().calls.len(), n);
            if [3, 5, 7, 9].contains(&case) {
                assert_eq!(n, 4);
            }
        }
    }
    #[test]
    fn successful_transfer_actual_byte_count_is_preserved_and_buffers_start_zero() {
        for count in [0, 8, 16, 24, 32, 40] {
            let (s, m) = state();
            let lock = m.clone();
            let _guard = lock.lock().unwrap();
            let mut v = connect(s.clone(), m, Cancellation::default()).unwrap();
            s.borrow_mut()
                .replies
                .insert(4, Reply::Bytes(vec![0x5a; count], count));
            assert_eq!(v.smbus_transfer(&transfer()).unwrap(), vec![0x5a; count]);
            assert!(v.failure().is_none());
        }
    }

    #[test]
    fn complete_verified_piix4_ene_bus_ram_chain_color_black_and_drop() {
        use crate::lighting::color::Rgb;
        struct Ram {
            registers: BTreeMap<u16, u8>,
            pointer: u16,
            epochs: BTreeMap<u64, Vec<String>>,
        }
        struct RamDevice {
            policy: Fake,
            ram: Rc<RefCell<Ram>>,
            address: u8,
        }
        impl Device for RamDevice {
            fn ioctl(
                &mut self,
                code: u32,
                input: &[u8],
                output: &mut [u8],
                cancel: &Cancellation,
            ) -> Result<usize, String> {
                let state = self.policy.0.borrow();
                assert!(state.held);
                let epoch = state.epoch;
                drop(state);
                let name = if code == pawnio::IOCTL_LOAD {
                    "LOAD".into()
                } else {
                    function(input)
                };
                self.ram
                    .borrow_mut()
                    .epochs
                    .entry(epoch)
                    .or_default()
                    .push(name.clone());
                // Reuse the policy fake's strict load/identity/query handling,
                // output-zero checks, call recording and owned-device Drop.
                let count = self.policy.ioctl(code, input, output, cancel)?;
                if name != "ioctl_smbus_xfer" {
                    return Ok(count);
                }
                assert_eq!(input.len(), 104);
                let words: Vec<u64> = input[32..]
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|bytes| u64::from_le_bytes(*bytes))
                    .collect();
                assert_eq!(words.len(), 9);
                assert_eq!(words[0], u64::from(self.address));
                assert!(words[5..].iter().all(|&word| word == 0));
                let mut ram = self.ram.borrow_mut();
                match (words[1], words[2], words[3]) {
                    (0, 0, 3) => {
                        assert!(words[4] <= u64::from(u16::MAX));
                        let register = (words[4] as u16).swap_bytes();
                        assert!(
                            matches!(register, 0x1000..=0x100f | 0x1c00..=0x1c3f | 0x8020 | 0x80a0 | 0x8100..=0x8117)
                        );
                        ram.pointer = register;
                        Ok(0)
                    }
                    (1, 0x81, 2) => {
                        assert_eq!(words[4], 0);
                        assert!(matches!(ram.pointer, 0x1000..=0x100f | 0x1c00..=0x1c3f));
                        let value = u64::from(ram.registers[&ram.pointer]);
                        output[..8].copy_from_slice(&value.to_le_bytes());
                        Ok(8)
                    }
                    (0, 1, 2) => {
                        assert!(words[4] <= 255);
                        let register = ram.pointer;
                        assert!(matches!(register, 0x8020 | 0x80a0 | 0x8100..=0x8117));
                        if matches!(register, 0x8020 | 0x80a0) {
                            assert_eq!(words[4], 1);
                        }
                        ram.registers.insert(register, words[4] as u8);
                        ram.pointer += 1;
                        Ok(0)
                    }
                    (0, 3, 5) => {
                        let bytes = words[4].to_le_bytes();
                        assert_eq!(bytes[0], 3);
                        assert!(bytes[4..].iter().all(|&byte| byte == 0));
                        assert!((0x8100..=0x8115).contains(&ram.pointer));
                        assert!((ram.pointer - 0x8100).is_multiple_of(3));
                        for &value in &bytes[1..4] {
                            let register = ram.pointer;
                            ram.registers.insert(register, value);
                            ram.pointer += 1;
                        }
                        Ok(0)
                    }
                    _ => panic!("unexpected RAM SMBus transfer"),
                }
            }
        }
        for address in [0x71, 0x73] {
            let (state, mutex) = state();
            let mut registers = BTreeMap::new();
            for register in (0x1000..=0x100f).chain(0x1c00..=0x1c3f) {
                registers.insert(register, 0);
            }
            for (offset, byte) in ene::VERSION.bytes().enumerate() {
                registers.insert(0x1000 + offset as u16, byte);
            }
            registers.extend([(0x1c02, 8), (0x1c03, 8), (0x1c13, 0x8a)]);
            let ram = Rc::new(RefCell::new(Ram {
                registers,
                pointer: 0,
                epochs: BTreeMap::new(),
            }));
            let lock = mutex.clone();
            let guard = lock.lock().unwrap();
            let device = RamDevice {
                policy: Fake(state.clone()),
                ram: ram.clone(),
                address,
            };
            let verified = VerifiedPiix4::connect(
                &pawnio::Piix4Module::synthetic_for_test(),
                device,
                mutex.clone(),
                address,
                Cancellation::default(),
            )
            .unwrap();
            drop(guard);
            let bus = ene_bus::EneBus::new(
                address,
                verified,
                mutex,
                ene_bus::BlockCapability::VerifiedThreeByteWrites,
            )
            .unwrap();
            let mut controller = ene::Controller::connect(bus, address).unwrap();
            assert_eq!(controller.identity().version, ene::VERSION);
            let colors: Vec<Rgb> = (0..8)
                .map(|i| Rgb::new(1 + i * 17, 2 + i * 29, 255 - i * 13))
                .collect();
            controller.frame(&colors).unwrap();
            for (index, color) in colors.iter().enumerate() {
                let register = 0x8100 + index as u16 * 3;
                let ram = ram.borrow();
                assert_eq!(
                    [
                        ram.registers[&register],
                        ram.registers[&(register + 1)],
                        ram.registers[&(register + 2)]
                    ],
                    [color.r, color.b, color.g]
                );
            }
            controller.frame(&[Rgb::default(); 8]).unwrap();
            assert_eq!(controller.submitted(), 2);
            assert!(controller.failure().is_none());
            {
                let ram = ram.borrow();
                assert!((0x8100..=0x8117).all(|register| ram.registers[&register] == 0));
                assert_eq!((ram.registers[&0x8020], ram.registers[&0x80a0]), (1, 1));
                for (epoch, calls) in &ram.epochs {
                    if *epoch == 1 {
                        assert_eq!(
                            calls,
                            &[
                                "LOAD",
                                "ioctl_set_sleep_mode",
                                "ioctl_identity",
                                "ioctl_piix4_port_sel"
                            ]
                        );
                    } else {
                        assert_eq!(&calls[..2], &["ioctl_identity", "ioctl_piix4_port_sel"]);
                        assert!(calls[2..].iter().all(|call| call == "ioctl_smbus_xfer"));
                    }
                }
                assert_eq!(ram.epochs.len(), 5); // Open, identity, direct mode, color, black.
            }
            assert!(!state.borrow().held);
            let calls = state.borrow().calls.len();
            drop(controller);
            assert_eq!(state.borrow().drops, 1);
            assert_eq!(state.borrow().calls.len(), calls);
            assert!(!state.borrow().held);
        }
    }
}
