//! Single-register AMD thermal executor over an explicitly injected PawnIO device.
//! The native caller validates CPU binding and holds Access_PCI before opening
//! and loading. No MSR operation or caller-selected register is exposed here.
use super::{
    pawnio,
    pawnio_adapter::{Device, GuardedMutex},
};
use crate::{runtime::output::Cancellation, sensors::amd::Module};

pub const PCI_MUTEX: &str = "Global\\Access_PCI";
pub const TEMPERATURE_REGISTER: u64 = 0x59800;
pub fn temperature_request() -> [u8; 40] {
    let mut bytes = [0; 40];
    bytes[..14].copy_from_slice(b"ioctl_read_smn");
    bytes[32..].copy_from_slice(&TEMPERATURE_REGISTER.to_le_bytes());
    bytes
}
pub(crate) struct VerifiedSmn<D: Device, M: GuardedMutex> {
    device: Option<D>,
    mutex: M,
    cancel: Cancellation,
    last_epoch: u64,
    failure: Option<String>,
}
impl<D: Device, M: GuardedMutex> VerifiedSmn<D, M> {
    pub(crate) fn connect(
        module: &Module,
        device: D,
        mutex: M,
        cancel: Cancellation,
    ) -> Result<Self, String> {
        let mut value = Self {
            device: Some(device),
            mutex,
            cancel,
            last_epoch: 0,
            failure: None,
        };
        let result = (|| {
            let epoch = value.mutex.epoch()?;
            if epoch == 0 || value.cancel.cancelled() {
                return Err("AMD module load lacks a live PCI guard".into());
            }
            let count = value.device.as_mut().unwrap().ioctl(
                pawnio::IOCTL_LOAD,
                module.bytes(),
                &mut [],
                &value.cancel,
            )?;
            if count != 0 {
                return Err("AMD module load returned unexpected bytes".into());
            }
            value.check_epoch(epoch)?;
            value.last_epoch = epoch;
            Ok(())
        })();
        match result {
            Ok(()) => Ok(value),
            Err(error) => Err(value.terminal(error)),
        }
    }
    fn terminal(&mut self, error: String) -> String {
        if self.failure.is_none() {
            self.failure = Some(error);
        }
        self.device = None;
        self.failure.clone().unwrap()
    }
    fn check_epoch(&self, epoch: u64) -> Result<(), String> {
        if self.cancel.cancelled() || epoch == 0 || self.mutex.epoch()? != epoch {
            Err("AMD PCI guard changed or operation cancelled".into())
        } else {
            Ok(())
        }
    }
    #[cfg(test)]
    pub(crate) fn read_tctl_raw(&mut self) -> Result<u32, String> {
        self.read_tctl_raw_checked(|| Ok(()))
    }
    pub(crate) fn read_tctl_raw_checked(
        &mut self,
        mut identity: impl FnMut() -> Result<(), String>,
    ) -> Result<u32, String> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if self.cancel.cancelled() {
            return Err(self.terminal("AMD sensor cancelled".into()));
        }
        let mutex = self.mutex.clone();
        // Failure to acquire has performed no IOCTL and may be retried later.
        let _guard = mutex.lock()?;
        let result = (|| {
            let epoch = self.mutex.epoch()?;
            if epoch <= self.last_epoch {
                return Err("AMD PCI guard epoch did not advance".into());
            }
            self.check_epoch(epoch)?;
            identity()?;
            let mut output = [0u8; 8];
            let count = self
                .device
                .as_mut()
                .ok_or("AMD sensor device is closed")?
                .ioctl(
                    pawnio::IOCTL_EXECUTE,
                    &temperature_request(),
                    &mut output,
                    &self.cancel,
                )?;
            self.check_epoch(epoch)?;
            identity()?;
            if count != 8 {
                return Err("AMD SMN read requires exactly eight successful output bytes".into());
            }
            let raw = u64::from_le_bytes(output);
            let raw = u32::try_from(raw).map_err(|_| "AMD SMN output exceeds a DWORD")?;
            self.last_epoch = epoch;
            Ok(raw)
        })();
        result.map_err(|error| self.terminal(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{sensors::amd, transport::ene_bus::SharedMutex};
    use std::{
        cell::Cell,
        rc::Rc,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };
    #[derive(Default)]
    struct LockState {
        held: Cell<bool>,
        epoch: Cell<u64>,
    }
    #[derive(Clone, Default)]
    struct Lock(Rc<LockState>);
    struct Guard(Lock);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.0.held.set(false);
        }
    }
    impl SharedMutex for Lock {
        type Guard<'a> = Guard;
        fn lock(&self) -> Result<Guard, String> {
            if self.0.held.replace(true) {
                return Err("lock busy; no I/O".into());
            }
            self.0.epoch.set(self.0.epoch.get() + 1);
            Ok(Guard(self.clone()))
        }
    }
    impl GuardedMutex for Lock {
        fn epoch(&self) -> Result<u64, String> {
            if self.0.held.get() {
                Ok(self.0.epoch.get())
            } else {
                Err("no guard".into())
            }
        }
    }
    #[derive(Default)]
    struct Record {
        calls: Mutex<Vec<(u32, Vec<u8>)>>,
        mode: AtomicUsize,
        drops: AtomicUsize,
    }
    struct Fake {
        lock: Lock,
        record: Arc<Record>,
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            self.record.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl Device for Fake {
        fn ioctl(
            &mut self,
            code: u32,
            input: &[u8],
            output: &mut [u8],
            _: &Cancellation,
        ) -> Result<usize, String> {
            assert!(self.lock.epoch().is_ok());
            assert!(output.iter().all(|v| *v == 0));
            self.record
                .calls
                .lock()
                .unwrap()
                .push((code, input.to_vec()));
            let mode = self.record.mode.load(Ordering::SeqCst);
            if code == pawnio::IOCTL_LOAD {
                assert!(output.is_empty());
                return if mode == 5 { Ok(1) } else { Ok(0) };
            }
            assert_eq!(code, pawnio::IOCTL_EXECUTE);
            assert_eq!(input, temperature_request());
            assert_eq!(output.len(), 8);
            if mode == 1 {
                return Err("injected I/O failure".into());
            }
            let value = if mode == 3 { u64::MAX } else { 800u64 << 21 };
            output.copy_from_slice(&value.to_le_bytes());
            if mode == 4 {
                self.lock.0.epoch.set(self.lock.0.epoch.get() + 1);
            }
            Ok(if mode == 2 { 0 } else { 8 })
        }
    }
    fn connect(record: Arc<Record>) -> (VerifiedSmn<Fake, Lock>, Lock) {
        connect_with_cancel(record, Cancellation::default())
    }
    fn connect_with_cancel(
        record: Arc<Record>,
        cancel: Cancellation,
    ) -> (VerifiedSmn<Fake, Lock>, Lock) {
        let lock = Lock::default();
        let guard = lock.lock().unwrap();
        let executor = VerifiedSmn::connect(
            &Module::synthetic_for_test(),
            Fake {
                lock: lock.clone(),
                record,
            },
            lock.clone(),
            cancel,
        )
        .unwrap();
        drop(guard);
        (executor, lock)
    }
    #[test]
    fn exact_single_offset_wire_and_guarded_load_then_fresh_transaction() {
        let record = Arc::new(Record::default());
        let (mut executor, lock) = connect(record.clone());
        assert!(!lock.0.held.get());
        assert_eq!(executor.read_tctl_raw().unwrap(), 800 << 21);
        assert!(!lock.0.held.get());
        assert_eq!(record.calls.lock().unwrap()[1].1, temperature_request());
        let bytes = temperature_request();
        assert_eq!(&bytes[..14], b"ioctl_read_smn");
        assert!(bytes[14..32].iter().all(|v| *v == 0));
        assert_eq!(u64::from_le_bytes(bytes[32..].try_into().unwrap()), 0x59800);
    }
    #[test]
    fn failed_count_width_epoch_or_io_closes_before_any_retry() {
        for mode in 1..=4 {
            let record = Arc::new(Record::default());
            let (mut executor, _) = connect(record.clone());
            record.mode.store(mode, Ordering::SeqCst);
            assert!(executor.read_tctl_raw().is_err());
            assert_eq!(record.drops.load(Ordering::SeqCst), 1);
            let calls = record.calls.lock().unwrap().len();
            assert!(executor.read_tctl_raw().is_err());
            assert_eq!(record.calls.lock().unwrap().len(), calls);
        }
    }
    #[test]
    fn load_requires_live_guard_and_exact_empty_reply() {
        for held in [false, true] {
            let record = Arc::new(Record::default());
            record.mode.store(5, Ordering::SeqCst);
            let lock = Lock::default();
            let _guard = held.then(|| lock.lock().unwrap());
            assert!(
                VerifiedSmn::connect(
                    &Module::synthetic_for_test(),
                    Fake {
                        lock: lock.clone(),
                        record: record.clone()
                    },
                    lock,
                    Cancellation::default()
                )
                .is_err()
            );
            assert_eq!(record.drops.load(Ordering::SeqCst), 1);
            assert_eq!(record.calls.lock().unwrap().len(), usize::from(held));
        }
    }
    #[test]
    fn busy_lock_does_no_io_and_identity_is_checked_under_guard_before_and_after_read() {
        let record = Arc::new(Record::default());
        let (mut executor, lock) = connect(record.clone());
        {
            let _guard = lock.lock().unwrap();
            assert!(executor.read_tctl_raw().is_err());
        }
        assert_eq!(record.calls.lock().unwrap().len(), 1);
        let checks = Cell::new(0);
        assert!(
            executor
                .read_tctl_raw_checked(|| {
                    assert!(lock.0.held.get());
                    checks.set(checks.get() + 1);
                    if checks.get() == 2 {
                        Err("changed CPU".into())
                    } else {
                        Ok(())
                    }
                })
                .is_err()
        );
        assert_eq!(checks.get(), 2);
        assert_eq!(record.drops.load(Ordering::SeqCst), 1);
        assert!(!lock.0.held.get());
    }
    #[test]
    fn cancelled_read_never_submits_execute() {
        let record = Arc::new(Record::default());
        let (mut executor, _) = connect(record.clone());
        executor.cancel.cancel_for_test();
        assert!(executor.read_tctl_raw().is_err());
        assert_eq!(record.calls.lock().unwrap().len(), 1);
    }
    fn binding() -> amd::CpuIdentity {
        amd::CpuIdentity {
            vendor: *b"AuthenticAMD",
            signature: 0x00a60f12,
            packages: 1,
        }
    }
    struct Backend(VerifiedSmn<Fake, Lock>);
    impl amd::Backend for Backend {
        fn identity(&mut self) -> Result<amd::CpuIdentity, String> {
            Ok(binding())
        }
        fn read_tctl_raw(&mut self) -> Result<u32, String> {
            self.0.read_tctl_raw()
        }
    }
    #[test]
    fn fake_device_full_tctl_provider_worker_runtime_chain_has_no_error_zero_or_hidden_reload() {
        use crate::{
            app::{
                ambient,
                config::{Groups, Project},
                document::Document,
                lcd,
                runtime::Runtime,
                sensors::{Config, Source, Value},
            },
            devices::{Canvas, Setup},
        };
        let document = Document {
            version: 1,
            lighting: Project {
                version: 1,
                setup: Setup {
                    version: 1,
                    devices: vec![],
                    canvas: Canvas {
                        width: 1,
                        height: 1,
                        points: vec![],
                    },
                },
                scene: crate::runtime::scene::example(),
                schedule: crate::schedule::Config::default(),
                groups: Groups {
                    pc: vec![],
                    door: vec![],
                },
            },
            ambient: ambient::Config::default(),
            lcd: vec![],
            temperatures: Config {
                sources: vec![Source {
                    key: "cpu".into(),
                    value: Value::Sensor {
                        input: "amd-slot".into(),
                    },
                }],
            },
        };
        let mut runtime = Runtime::new(document, None).unwrap();
        let record = Arc::new(Record::default());
        let shared = record.clone();
        runtime
            .attach_sensor(
                "amd-slot",
                vec![],
                Arc::new(move |cancel| {
                    let (executor, _) = connect_with_cancel(shared.clone(), cancel);
                    Ok(Box::new(amd::Provider::new(binding(), Backend(executor))?))
                }),
            )
            .unwrap();
        let wall = 1_793_900_000;
        let wait = |runtime: &mut Runtime, predicate: &dyn Fn(&Runtime) -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !predicate(runtime) {
                assert!(std::time::Instant::now() < deadline);
                runtime.tick(crate::clock::monotonic_us(), wall).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        };
        wait(&mut runtime, &|r| !r.temperature_samples().is_empty());
        assert_eq!(
            runtime.temperature_samples()[0].celsius,
            amd::decode_tctl(800 << 21)
        );
        record.mode.store(1, Ordering::SeqCst);
        wait(&mut runtime, &|r| {
            r.sensor_status("amd-slot").unwrap().error.is_some()
                && r.temperature_samples().is_empty()
        });
        assert_eq!(record.drops.load(Ordering::SeqCst), 1);
        let calls = record.calls.lock().unwrap().len();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        runtime.tick(crate::clock::monotonic_us(), wall).unwrap();
        assert_eq!(record.calls.lock().unwrap().len(), calls);
        assert!(
            runtime
                .accept_temperatures(
                    &[lcd::Sample {
                        key: "amd-slot".into(),
                        celsius: 0.,
                        observed_ms: crate::clock::monotonic_ms()
                    }],
                    crate::clock::monotonic_us()
                )
                .is_err()
        );
        runtime.stop();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !runtime.drained() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}
