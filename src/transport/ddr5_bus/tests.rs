use super::*;
use crate::transport::ene_bus::SharedMutex;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU16, Ordering},
    },
};

#[derive(Clone, Debug)]
struct Call {
    code: u32,
    input: Vec<u8>,
    output: usize,
    epoch: u64,
    at: Instant,
}
#[derive(Clone, Copy)]
enum Fault {
    Error,
    Short(usize),
    Epoch,
    Cancel,
}
struct State {
    held: bool,
    busy: bool,
    epoch: u64,
    acquisitions: usize,
    unlocks: usize,
    calls: Vec<Call>,
    closes: Vec<(bool, usize, usize)>,
    controller: [u64; 3],
    physical_port: u64,
    aux: bool,
    bytes: BTreeMap<u8, u64>,
    temperature: u64,
    address: u8,
    shared_temperature: Option<Arc<AtomicU16>>,
    shared_failure: Option<Arc<AtomicBool>>,
    apply_page_write: bool,
    fault: Option<(usize, Fault)>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            held: false,
            busy: false,
            epoch: 0,
            acquisitions: 0,
            unlocks: 0,
            calls: vec![],
            closes: vec![],
            controller: binding(0).controller,
            physical_port: 0,
            aux: false,
            bytes: BTreeMap::from([
                (0, 0x51),
                (1, 0x18),
                (5, 2),
                (6, 0x11),
                (0x0b, 0),
                (0x1a, 0),
            ]),
            temperature: 0x0190,
            address: 0x50,
            shared_temperature: None,
            shared_failure: None,
            apply_page_write: true,
            fault: None,
        }
    }
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
        if s.busy || s.held {
            return Err("synthetic shared bus busy".into());
        }
        s.held = true;
        s.epoch += 1;
        s.acquisitions += 1;
        Ok(Guard(self.0.clone()))
    }
}
impl GuardedMutex for Mutex {
    fn epoch(&self) -> Result<u64, String> {
        let s = self.0.borrow();
        Ok(if s.held { s.epoch } else { 0 })
    }
}
struct FakeDevice(Rc<RefCell<State>>);
impl Drop for FakeDevice {
    fn drop(&mut self) {
        let mut s = self.0.borrow_mut();
        let record = (s.held, s.calls.len(), s.unlocks);
        s.closes.push(record);
    }
}
fn words(input: &[u8]) -> Vec<u64> {
    assert!(input.len() >= 32);
    assert!((input.len() - 32).is_multiple_of(8));
    input[32..]
        .as_chunks::<8>()
        .0
        .iter()
        .map(|b| u64::from_le_bytes(*b))
        .collect()
}
fn name(input: &[u8]) -> &[u8] {
    let end = input[..32].iter().position(|b| *b == 0).unwrap_or(32);
    assert!(input[end..32].iter().all(|b| *b == 0));
    &input[..end]
}
fn store(output: &mut [u8], values: &[u64]) {
    assert_eq!(output.len(), values.len() * 8);
    for (dst, value) in output.as_chunks_mut::<8>().0.iter_mut().zip(values) {
        dst.copy_from_slice(&value.to_le_bytes());
    }
}
impl Device for FakeDevice {
    fn ioctl(
        &mut self,
        code: u32,
        input: &[u8],
        output: &mut [u8],
        cancel: &Cancellation,
    ) -> Result<usize, String> {
        let mut s = self.0.borrow_mut();
        assert!(s.held, "all native I/O must have the cloned live guard");
        assert!(s.epoch > 0);
        assert!(!cancel.cancelled());
        assert!(output.iter().all(|b| *b == 0));
        let epoch = s.epoch;
        s.calls.push(Call {
            code,
            input: input.to_vec(),
            output: output.len(),
            epoch,
            at: Instant::now(),
        });
        let fault = s
            .fault
            .filter(|(at, _)| *at == s.calls.len())
            .map(|(_, f)| f);
        if matches!(fault, Some(Fault::Error)) {
            return Err("synthetic transfer failure".into());
        }
        match code {
            pawnio::IOCTL_LOAD => {
                assert_eq!(input, pawnio::Piix4Module::synthetic_for_test().bytes());
                assert!(output.is_empty());
            }
            pawnio::IOCTL_EXECUTE => match name(input) {
                b"ioctl_set_sleep_mode" => {
                    assert_eq!(input, pawnio::always_sleep_request());
                    assert!(output.is_empty());
                }
                b"ioctl_identity" => {
                    assert_eq!(input, pawnio::identity_request());
                    let mut identity = s.controller;
                    if s.aux {
                        identity[1] = 0xb20;
                    }
                    store(output, &identity);
                }
                b"ioctl_piix4_port_sel" => {
                    assert_eq!(input.len(), 40);
                    let w = words(input);
                    assert_eq!(output.len(), 8);
                    match w[0] {
                        u64::MAX => store(output, &[if s.aux { 1 } else { s.physical_port }]),
                        1 => {
                            assert!(!s.aux, "aux is selected only once on this fresh VM");
                            let old = s.physical_port;
                            s.aux = true;
                            store(output, &[old]);
                        }
                        port => panic!("physical port write {port} must never be issued"),
                    }
                }
                b"ioctl_smbus_xfer" => {
                    assert!(permitted_wire(input, output.len()));
                    let w = words(input);
                    assert_eq!(input.len(), 72);
                    assert_eq!(w.len(), 5);
                    assert_eq!(w[4], 0);
                    assert_eq!(w[0], u64::from(s.address));
                    match (w[1], w[2], w[3]) {
                        (1, 0x31, 3) => {
                            if s.shared_failure
                                .as_ref()
                                .is_some_and(|v| v.load(Ordering::Acquire))
                            {
                                return Err("synthetic shared DDR5 temperature failure".into());
                            }
                            let t = s
                                .shared_temperature
                                .as_ref()
                                .map_or(s.temperature, |v| u64::from(v.load(Ordering::Acquire)));
                            store(output, &[t]);
                        }
                        (1, reg, 2) => {
                            let value = *s
                                .bytes
                                .get(&(reg as u8))
                                .expect("byte register is allowlisted");
                            store(output, &[value]);
                        }
                        (0, 0x0b, 2) => {
                            assert!(output.is_empty());
                            if s.apply_page_write {
                                s.bytes.insert(0x0b, 0);
                            }
                        }
                        other => panic!("unexpected DDR5 operation {other:?}"),
                    }
                }
                other => panic!("unexpected export {other:?}"),
            },
            other => panic!("unexpected IOCTL {other:08x}"),
        }
        match fault {
            Some(Fault::Short(count)) => return Ok(count),
            Some(Fault::Epoch) => s.epoch += 1,
            Some(Fault::Cancel) => cancel.cancel(),
            _ => {}
        }
        Ok(output.len())
    }
}
fn binding(port: u8) -> Binding {
    Binding {
        controller: [
            u64::from_le_bytes(*b"PIIX4\0\0\0"),
            if port == 0 { 0xb00 } else { 0xb20 },
            0x12345678_790b1022,
        ],
        port,
        address: 0x50,
    }
}
type Bus = VerifiedDdr5<FakeDevice, Mutex>;
fn connect(state: Rc<RefCell<State>>, port: u8, cancel: Cancellation) -> Result<Bus, String> {
    let mutex = Mutex(state.clone());
    let _guard = mutex.lock()?;
    VerifiedDdr5::connect(
        &pawnio::Piix4Module::synthetic_for_test(),
        FakeDevice(state),
        mutex.clone(),
        binding(port),
        cancel,
    )
}
fn fixture() -> (Bus, Rc<RefCell<State>>, Cancellation) {
    let state = Rc::new(RefCell::new(State::default()));
    let cancel = Cancellation::default();
    let bus = connect(state.clone(), 0, cancel.clone()).unwrap();
    (bus, state, cancel)
}
/// Reuses the typed fake bus for runtime -> sampled readings -> LCD tests.
/// Rc/guards/devices are created only inside the actual sensor owner thread.
pub(crate) fn sensor_factory(
    raw: Arc<AtomicU16>,
    fail: Arc<AtomicBool>,
    address: u8,
) -> crate::app::sensor_worker::Factory {
    struct Sensor(ddr5::Provider<Bus>);
    impl crate::app::sensor_worker::Provider for Sensor {
        fn poll(&mut self, now_ms: u64) -> Result<Option<crate::sensors::Reading>, String> {
            self.0.poll(now_ms)
        }
    }
    Arc::new(move |cancel| {
        let state = Rc::new(RefCell::new(State {
            address,
            shared_temperature: Some(raw.clone()),
            shared_failure: Some(fail.clone()),
            ..State::default()
        }));
        let mutex = Mutex(state.clone());
        let mut explicit = binding(0);
        explicit.address = address;
        let bus = {
            let _guard = mutex.lock()?;
            VerifiedDdr5::connect(
                &pawnio::Piix4Module::synthetic_for_test(),
                FakeDevice(state),
                mutex.clone(),
                explicit,
                cancel,
            )?
        };
        Ok(Box::new(Sensor(ddr5::Provider::new(explicit, bus)?)))
    })
}
fn transfers(state: &State) -> Vec<&Call> {
    state
        .calls
        .iter()
        .filter(|c| c.code == pawnio::IOCTL_EXECUTE && name(&c.input) == NAME)
        .collect()
}
fn writes(state: &State) -> Vec<&Call> {
    transfers(state)
        .into_iter()
        .filter(|c| words(&c.input)[1] == 0)
        .collect()
}
fn temperature_reads(state: &State) -> usize {
    transfers(state)
        .iter()
        .filter(|c| words(&c.input)[2] == 0x31)
        .count()
}
fn assert_terminal(bus: &mut Bus, state: &Rc<RefCell<State>>) {
    assert!(bus.failure.is_some());
    assert!(bus.device.is_none());
    let s = state.borrow();
    let count = s.calls.len();
    assert_eq!(s.closes.len(), 1);
    assert!(s.closes[0].0, "close must precede guard drop");
    assert_eq!(s.closes[0].1, count);
    assert!(s.closes[0].2 < s.unlocks);
    drop(s);
    assert!(bus.read_checked(|| Ok(())).is_err());
    assert_eq!(state.borrow().calls.len(), count);
}

#[test]
fn page_zero_read_and_legitimate_zero_are_distinct_from_missing_or_error() {
    let (mut bus, state, _) = fixture();
    assert_eq!(state.borrow().calls.len(), 5);
    let mut owners = 0;
    assert_eq!(
        bus.read_checked(|| {
            owners += 1;
            Ok(())
        })
        .unwrap(),
        0x0190
    );
    assert_eq!(owners, 2);
    {
        let s = state.borrow();
        assert_eq!(s.calls.len(), 17);
        assert!(writes(&s).is_empty());
        assert_eq!(temperature_reads(&s), 1);
        assert_eq!(s.acquisitions, 2);
        assert_eq!(s.unlocks, 2);
        assert!(!s.held);
        assert!(s.calls[5..].iter().all(|c| c.epoch == 2));
        let regs = transfers(&s)
            .iter()
            .map(|c| words(&c.input)[2])
            .collect::<Vec<_>>();
        assert_eq!(regs, [0, 1, 5, 0x1a, 0x0b, 0x31]);
    }
    state.borrow_mut().temperature = 0;
    assert_eq!(bus.read_checked(|| Ok(())).unwrap(), 0);
    assert!(bus.failure.is_none());
    assert_eq!(state.borrow().calls.len(), 29);
    assert_eq!(temperature_reads(&state.borrow()), 2);
}

#[test]
fn nonzero_page_recovers_only_selector_zero_then_rechecks_hub_and_temperature() {
    for (recovery, min_delay) in [(0x11, 1), (0, 1000)] {
        let (mut bus, state, _) = fixture();
        state.borrow_mut().bytes.insert(0x0b, 3);
        state.borrow_mut().bytes.insert(6, recovery);
        let mut owners = 0;
        assert_eq!(
            bus.read_checked(|| {
                owners += 1;
                Ok(())
            })
            .unwrap(),
            0x0190
        );
        assert_eq!(owners, 3);
        let s = state.borrow();
        assert_eq!(s.calls.len(), 26);
        assert_eq!(temperature_reads(&s), 1);
        let w = writes(&s);
        assert_eq!(w.len(), 1);
        assert_eq!(words(&w[0].input), [0x50, 0, 0x0b, 2, 0]);
        assert_eq!(w[0].output, 0);
        let after = s
            .calls
            .iter()
            .skip_while(|c| !std::ptr::eq(*c, w[0]))
            .nth(1)
            .unwrap();
        assert_eq!(words(&after.input)[2], 0x0b);
        assert!(after.at.duration_since(w[0].at) >= Duration::from_micros(min_delay));
        let regs = transfers(&s)
            .iter()
            .map(|c| words(&c.input)[2])
            .collect::<Vec<_>>();
        assert_eq!(
            regs,
            [0, 1, 5, 0x1a, 0x0b, 6, 0x0b, 0x0b, 0, 1, 5, 0x1a, 0x31]
        );
        assert!(s.closes.is_empty());
        assert_eq!(s.physical_port, 0);
    }
}

#[test]
fn unknown_hub_missing_capability_disabled_or_extra_page_bits_never_write() {
    for (reg, value) in [
        (0, 0),
        (1, 0),
        (0, 0x151),
        (5, 0),
        (0x1a, 1),
        (0x1a, 0x100),
        (0x0b, 8),
        (0x0b, 0x80),
    ] {
        let (mut bus, state, _) = fixture();
        state.borrow_mut().bytes.insert(0x0b, 3);
        state.borrow_mut().bytes.insert(reg, value);
        assert!(bus.read_checked(|| Ok(())).is_err());
        assert!(writes(&state.borrow()).is_empty());
        assert_eq!(temperature_reads(&state.borrow()), 0);
        assert_terminal(&mut bus, &state);
    }
    for recovery in [0x03, 0x13, 0xf0, 0xff] {
        let (mut bus, state, _) = fixture();
        state.borrow_mut().bytes.insert(0x0b, 3);
        state.borrow_mut().bytes.insert(6, recovery);
        assert!(bus.read_checked(|| Ok(())).is_err());
        assert!(writes(&state.borrow()).is_empty());
        assert_eq!(temperature_reads(&state.borrow()), 0);
        assert_terminal(&mut bus, &state);
    }
}

#[test]
fn failed_page_write_and_unconfirmed_readback_never_reach_temperature() {
    for fail_write in [true, false] {
        let (mut bus, state, _) = fixture();
        state.borrow_mut().bytes.insert(0x0b, 3);
        if fail_write {
            state.borrow_mut().fault = Some((16, Fault::Error));
        } else {
            state.borrow_mut().apply_page_write = false;
        }
        assert!(bus.read_checked(|| Ok(())).is_err());
        let s = state.borrow();
        assert_eq!(writes(&s).len(), 1);
        assert_eq!(temperature_reads(&s), 0);
        assert_eq!(s.calls.len(), if fail_write { 16 } else { 17 });
        drop(s);
        assert_terminal(&mut bus, &state);
    }
    // A readback transport error is separate from a successful nonzero reply.
    let (mut bus, state, _) = fixture();
    state.borrow_mut().bytes.insert(0x0b, 3);
    state.borrow_mut().fault = Some((17, Fault::Error));
    assert!(bus.read_checked(|| Ok(())).is_err());
    assert_eq!(temperature_reads(&state.borrow()), 0);
    assert_terminal(&mut bus, &state);
}

#[test]
fn truncated_or_wide_results_fail_closed_without_retry() {
    for (absolute, count) in [(6, 16), (7, 0), (8, 7), (15, 7), (15, 9)] {
        let (mut bus, state, _) = fixture();
        state.borrow_mut().fault = Some((absolute, Fault::Short(count)));
        assert!(bus.read_checked(|| Ok(())).is_err());
        assert_eq!(state.borrow().calls.len(), absolute);
        assert_terminal(&mut bus, &state);
    }
    let (mut bus, state, _) = fixture();
    state.borrow_mut().temperature = 0x1_0190;
    assert!(bus.read_checked(|| Ok(())).is_err());
    assert_eq!(state.borrow().calls.len(), 15);
    assert_terminal(&mut bus, &state);
}

#[test]
fn full_identity_or_port_drift_and_owner_refusal_prevent_page_writes() {
    for drift in 0..5 {
        let (mut bus, state, _) = fixture();
        state.borrow_mut().bytes.insert(0x0b, 3);
        match drift {
            0 => state.borrow_mut().controller[0] ^= 1,
            1 => state.borrow_mut().controller[1] = 0xb20,
            2 => state.borrow_mut().controller[2] ^= 1,
            3 => state.borrow_mut().controller[2] ^= 1 << 32,
            _ => state.borrow_mut().physical_port = 2,
        }
        assert!(bus.read_checked(|| Ok(())).is_err());
        let s = state.borrow();
        assert!(transfers(&s).is_empty());
        assert!(writes(&s).is_empty());
        assert_eq!(s.calls.len(), if drift == 4 { 7 } else { 6 });
        drop(s);
        assert_terminal(&mut bus, &state);
    }
    // Owner can revoke permission at the last boundary before the only write.
    let (mut bus, state, _) = fixture();
    state.borrow_mut().bytes.insert(0x0b, 3);
    let mut checks = 0;
    assert!(
        bus.read_checked(|| {
            checks += 1;
            if checks == 2 {
                Err("owner changed".into())
            } else {
                Ok(())
            }
        })
        .is_err()
    );
    assert!(writes(&state.borrow()).is_empty());
    assert_eq!(temperature_reads(&state.borrow()), 0);
    assert_terminal(&mut bus, &state);
}

#[test]
fn cancellation_or_epoch_drift_after_every_io_stops_the_transaction() {
    for fault in [Fault::Cancel, Fault::Epoch] {
        for transaction_io in 1..=12 {
            let (mut bus, state, _) = fixture();
            let at = 5 + transaction_io;
            state.borrow_mut().fault = Some((at, fault));
            assert!(bus.read_checked(|| Ok(())).is_err());
            assert_eq!(state.borrow().calls.len(), at);
            assert_terminal(&mut bus, &state);
        }
    }
    let (mut bus, state, cancel) = fixture();
    cancel.cancel();
    assert!(bus.read_checked(|| Ok(())).is_err());
    assert_eq!(state.borrow().calls.len(), 5);
    assert_eq!(state.borrow().acquisitions, 1);
    assert!(bus.device.is_none());
}

#[test]
fn connect_requires_live_guard_and_checks_cancel_epoch_and_counts_on_load_and_queries() {
    let state = Rc::new(RefCell::new(State::default()));
    assert!(
        VerifiedDdr5::connect(
            &pawnio::Piix4Module::synthetic_for_test(),
            FakeDevice(state.clone()),
            Mutex(state.clone()),
            binding(0),
            Cancellation::default()
        )
        .is_err()
    );
    assert!(state.borrow().calls.is_empty());
    assert_eq!(state.borrow().closes.len(), 1);
    for fault in [Fault::Cancel, Fault::Epoch, Fault::Error] {
        for at in 1..=5 {
            let state = Rc::new(RefCell::new(State {
                fault: Some((at, fault)),
                ..State::default()
            }));
            assert!(connect(state.clone(), 0, Cancellation::default()).is_err());
            let s = state.borrow();
            assert_eq!(s.calls.len(), at);
            assert_eq!(s.closes.len(), 1);
            assert!(s.closes[0].0);
            assert_eq!(s.closes[0].2, 0);
            assert_eq!(s.unlocks, 1);
        }
    }
    for (at, count) in [(1, 1), (2, 1), (3, 16), (4, 16), (5, 0)] {
        let state = Rc::new(RefCell::new(State {
            fault: Some((at, Fault::Short(count))),
            ..State::default()
        }));
        assert!(connect(state.clone(), 0, Cancellation::default()).is_err());
        assert_eq!(state.borrow().calls.len(), at);
        assert!(state.borrow().closes[0].0);
    }
}

#[test]
fn page_recovery_checks_cancellation_and_epoch_at_every_io_and_final_owner_check() {
    for fault in [Fault::Cancel, Fault::Epoch] {
        for transaction_io in 1..=21 {
            let (mut bus, state, _) = fixture();
            state.borrow_mut().bytes.insert(0x0b, 3);
            let at = 5 + transaction_io;
            state.borrow_mut().fault = Some((at, fault));
            assert!(bus.read_checked(|| Ok(())).is_err());
            assert_eq!(state.borrow().calls.len(), at);
            assert_terminal(&mut bus, &state);
        }
    }
    let (mut bus, state, _) = fixture();
    let mut owner_checks = 0;
    assert!(
        bus.read_checked(|| {
            owner_checks += 1;
            if owner_checks == 2 {
                Err("owner changed after read".into())
            } else {
                Ok(())
            }
        })
        .is_err()
    );
    assert_eq!(temperature_reads(&state.borrow()), 1);
    assert_terminal(&mut bus, &state);
}

#[test]
fn shared_sensor_factory_uses_real_typed_provider_with_no_cached_value_after_failure() {
    let raw = Arc::new(AtomicU16::new(0x0190));
    let fail = Arc::new(AtomicBool::new(false));
    let factory = sensor_factory(raw.clone(), fail.clone(), 0x51);
    let mut provider = factory(Cancellation::default()).unwrap();
    assert_eq!(provider.poll(1).unwrap().unwrap().celsius, 25.0);
    raw.store(0x1ff0, Ordering::Release);
    assert_eq!(provider.poll(1001).unwrap().unwrap().celsius, -1.0);
    fail.store(true, Ordering::Release);
    assert!(provider.poll(2001).is_err());
    fail.store(false, Ordering::Release);
    assert!(
        provider.poll(3001).is_err(),
        "typed transfer failure remains terminal until explicit new factory"
    );
    let mut fresh = factory(Cancellation::default()).unwrap();
    assert_eq!(fresh.poll(4001).unwrap().unwrap().celsius, -1.0);
}

#[test]
fn lock_busy_is_transfer_free_nonterminal_and_next_fresh_guard_can_succeed() {
    let (mut bus, state, _) = fixture();
    state.borrow_mut().busy = true;
    assert!(bus.read_checked(|| Ok(())).is_err());
    assert_eq!(state.borrow().calls.len(), 5);
    assert!(bus.failure.is_none());
    assert!(bus.device.is_some());
    assert!(state.borrow().closes.is_empty());
    state.borrow_mut().busy = false;
    assert_eq!(bus.read_checked(|| Ok(())).unwrap(), 0x0190);
    // A caller-held old guard is neither recursively acquired nor reused.
    let mutex = Mutex(state.clone());
    let guard = mutex.lock().unwrap();
    let count = state.borrow().calls.len();
    assert!(bus.read_checked(|| Ok(())).is_err());
    assert_eq!(state.borrow().calls.len(), count);
    assert!(bus.failure.is_none());
    drop(guard);
    assert!(bus.read_checked(|| Ok(())).is_ok());
}

#[test]
fn auxiliary_selection_changes_only_local_vm_base_and_never_writes_physical_port() {
    for physical_port in [0, 2, 3, 4] {
        let state = Rc::new(RefCell::new(State {
            physical_port,
            ..State::default()
        }));
        let mut bus = connect(state.clone(), 1, Cancellation::default()).unwrap();
        assert_eq!(bus.read_checked(|| Ok(())).unwrap(), 0x0190);
        let s = state.borrow();
        assert_eq!(s.physical_port, physical_port);
        assert!(s.aux);
        assert_eq!(s.controller[1], 0xb00);
        let selections = s
            .calls
            .iter()
            .filter(|c| {
                c.code == pawnio::IOCTL_EXECUTE && name(&c.input) == b"ioctl_piix4_port_sel"
            })
            .map(|c| words(&c.input)[0])
            .filter(|v| *v != u64::MAX)
            .collect::<Vec<_>>();
        assert_eq!(selections, [1]);
    }
    for (port, physical) in [(0, 1), (0, 2), (0, 4), (1, 1), (1, 5)] {
        let state = Rc::new(RefCell::new(State {
            physical_port: physical,
            ..State::default()
        }));
        assert!(connect(state.clone(), port, Cancellation::default()).is_err());
        let s = state.borrow();
        assert!(writes(&s).is_empty());
        assert_eq!(temperature_reads(&s), 0);
        assert_eq!(s.physical_port, physical);
        assert_eq!(s.closes.len(), 1);
        assert!(s.closes[0].0);
    }
    let state = Rc::new(RefCell::new(State::default()));
    state.borrow_mut().controller[1] = 0xb20;
    assert!(connect(state.clone(), 1, Cancellation::default()).is_err());
    assert!(!state.borrow().aux);
}

#[test]
fn permitted_wire_accepts_only_spd_thermal_subset_with_exact_lengths() {
    for address in 0x50..=0x57 {
        for reg in [0, 1, 5, 6, 0x0b, 0x1a] {
            assert!(permitted_wire(&request(address, true, reg, 2), 8));
        }
        assert!(permitted_wire(&request(address, true, 0x31, 3), 8));
        assert!(permitted_wire(&request(address, false, 0x0b, 2), 0));
    }
    for address in [0, 0x4f, 0x58, 0x70, 0x76] {
        assert!(!permitted_wire(&request(address, true, 0x31, 3), 8));
        assert!(!permitted_wire(&request(address, false, 0x0b, 2), 0));
    }
    for (read, reg, kind, out) in [
        (false, 0x1a, 2, 0),
        (false, 0x31, 3, 0),
        (false, 0x0b, 2, 8),
        (true, 0x31, 2, 8),
        (true, 0, 3, 8),
        (true, 0x30, 3, 8),
        (true, 5, 2, 0),
        (true, 5, 2, 7),
        (true, 5, 2, 9),
    ] {
        assert!(!permitted_wire(&request(0x50, read, reg, kind), out));
    }
    let valid = request(0x50, true, 0x31, 3);
    assert!(!permitted_wire(&valid[..71], 8));
    let mut long = valid.to_vec();
    long.push(0);
    assert!(!permitted_wire(&long, 8));
    for index in [0, 20, 31, 39, 40, 56, 64, 71] {
        let mut invalid = valid;
        invalid[index] ^= 0x80;
        assert!(!permitted_wire(&invalid, 8), "byte {index}");
    }
}
