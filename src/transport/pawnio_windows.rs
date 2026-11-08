//! Dormant native PIIX4 owner. No driver install/start, scan, clock or port writes.
use super::{
    ene,
    ene_bus::{self, SharedMutex},
    pawnio,
    pawnio_adapter::{self, Device, GuardedMutex},
};
use crate::runtime::output::Cancellation;
use std::{
    cell::Cell,
    path::Path,
    ptr::{null, null_mut},
    rc::Rc,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    },
    System::{
        IO::DeviceIoControl,
        Threading::{CreateMutexW, GetCurrentThreadId, ReleaseMutex, WaitForSingleObject},
    },
};
const DEVICE_PATH: &str = r"\\?\GLOBALROOT\Device\PawnIO";
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn owned(raw: HANDLE) -> Result<Handle, String> {
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        Err(format!(
            "Open PawnIO object: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(Handle(raw))
    }
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
struct MutexState {
    handle: Handle,
    owner_thread: u32,
    held: Cell<bool>,
    epoch: Cell<u64>,
    poisoned: Cell<bool>,
    cancel: Cancellation,
}
/// Thread-bound clones share bookkeeping, while the named OS object also
/// serializes independent output workers and cooperating external applications.
#[derive(Clone)]
pub(crate) struct NamedMutex(Rc<MutexState>);
impl NamedMutex {
    pub(crate) fn named(name: &str, cancel: Cancellation) -> Result<Self, String> {
        if name.is_empty() || name.contains('\0') {
            return Err("Invalid SMBus mutex name".into());
        }
        let name = wide(name);
        let handle = owned(unsafe { CreateMutexW(null(), 0, name.as_ptr()) })?;
        Ok(Self(Rc::new(MutexState {
            handle,
            owner_thread: unsafe { GetCurrentThreadId() },
            held: Cell::new(false),
            epoch: Cell::new(0),
            poisoned: Cell::new(false),
            cancel,
        })))
    }
}
pub(crate) struct Guard<'a>(&'a NamedMutex);
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        let s = &self.0.0;
        if unsafe { ReleaseMutex(s.handle.0) } == 0 {
            s.poisoned.set(true);
        }
        s.held.set(false);
    }
}
impl SharedMutex for NamedMutex {
    type Guard<'a> = Guard<'a>;
    fn lock(&self) -> Result<Guard<'_>, String> {
        let s = &self.0;
        if s.poisoned.get() || s.held.get() || unsafe { GetCurrentThreadId() } != s.owner_thread {
            return Err("SMBus mutex poisoned, recursive or on wrong thread".into());
        }
        let start = Instant::now();
        loop {
            let remaining = Duration::from_millis(250).saturating_sub(start.elapsed());
            if s.cancel.cancelled() || remaining.is_zero() {
                return Err("SMBus mutex cancelled/timed out".into());
            }
            let status = unsafe {
                WaitForSingleObject(s.handle.0, remaining.as_millis().clamp(1, 25) as u32)
            };
            match status {
                WAIT_OBJECT_0 => {
                    let Some(epoch) = s.epoch.get().checked_add(1) else {
                        unsafe {
                            ReleaseMutex(s.handle.0);
                        }
                        s.poisoned.set(true);
                        return Err("SMBus mutex epoch exhausted".into());
                    };
                    s.epoch.set(epoch);
                    s.held.set(true);
                    return Ok(Guard(self));
                }
                WAIT_TIMEOUT => {}
                WAIT_ABANDONED => {
                    // WAIT_ABANDONED transfers ownership too. Release it, but do
                    // not use a bus left in an unknown state by another owner.
                    unsafe {
                        ReleaseMutex(s.handle.0);
                    }
                    s.poisoned.set(true);
                    return Err("SMBus mutex was abandoned; no hardware operation allowed".into());
                }
                _ => {
                    return Err(format!(
                        "Wait for SMBus mutex: {}",
                        std::io::Error::last_os_error()
                    ));
                }
            }
        }
    }
}
impl GuardedMutex for NamedMutex {
    fn epoch(&self) -> Result<u64, String> {
        let s = &self.0;
        if !s.held.get()
            || s.poisoned.get()
            || unsafe { GetCurrentThreadId() } != s.owner_thread
            || s.cancel.cancelled()
        {
            return Err("Current thread does not own a live SMBus guard".into());
        }
        Ok(s.epoch.get())
    }
}
pub(crate) struct NativeDevice {
    handle: Handle,
}
impl NativeDevice {
    pub(crate) fn open() -> Result<Self, String> {
        let path = wide(DEVICE_PATH);
        let handle = owned(unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        })?;
        Ok(Self { handle })
    }
}
impl NativeDevice {
    fn transfer(
        &mut self,
        code: u32,
        input: &[u8],
        output: &mut [u8],
        cancel: &Cancellation,
    ) -> Result<usize, String> {
        output.fill(0);
        // Input and output are borrowed until this synchronous function returns.
        // The starter immediately calls FFI; all request memory survives drain.
        let count = unsafe {
            super::windows_io::run(self.handle.0, cancel, |op| {
                DeviceIoControl(
                    self.handle.0,
                    code,
                    input.as_ptr().cast(),
                    input.len() as u32,
                    if output.is_empty() {
                        null_mut()
                    } else {
                        output.as_mut_ptr().cast()
                    },
                    output.len() as u32,
                    null_mut(),
                    op,
                )
            })
        }?;
        if count > output.len() {
            return Err("PawnIO output count exceeds capacity".into());
        }
        Ok(count)
    }
}

impl Device for NativeDevice {
    fn ioctl(
        &mut self,
        code: u32,
        input: &[u8],
        output: &mut [u8],
        cancel: &Cancellation,
    ) -> Result<usize, String> {
        let shape = match code {
            pawnio::IOCTL_LOAD => {
                !input.is_empty() && input.len() <= pawnio::MAX_MODULE_BYTES && output.is_empty()
            }
            pawnio::IOCTL_EXECUTE => {
                (input.len() == 40 || input.len() == 104)
                    && output.len() <= 40
                    && output.len().is_multiple_of(8)
                    || super::ddr5_bus::permitted_wire(input, output.len())
            }
            _ => false,
        };
        if !shape {
            return Err("PawnIO request outside bounded LOAD/EXECUTE shape".into());
        }
        self.transfer(code, input, output, cancel)
    }
}

pub(crate) fn connect_bound(
    module_path: &Path,
    address: u8,
    cancel: Cancellation,
) -> Result<impl ene::Bus + use<>, String> {
    if !ene::valid_address(address) || cancel.cancelled() {
        return Err("Invalid/cancelled ENE binding".into());
    }
    // Do not compete with an existing RGB owner. Never terminate that owner.
    check_writers()?;
    let module = pawnio::Piix4Module::read(module_path)?;
    let mutex = NamedMutex::named(pawnio::SHARED_MUTEX, cancel.clone())?;
    let executor = with_guard(&mutex, check_writers, || {
        let device = NativeDevice::open()?;
        pawnio_adapter::VerifiedPiix4::connect(&module, device, mutex.clone(), address, cancel)
    })?;
    // Exact pinned bytecode implements BLOCK_DATA5 with length1..32. An actual
    // transaction error is still terminal, never interpreted as no progress.
    ene_bus::EneBus::new(
        address,
        executor,
        mutex,
        ene_bus::BlockCapability::VerifiedThreeByteWrites,
    )
}

pub(crate) fn check_writers() -> Result<(), String> {
    let names =
        super::feature_hid::Backend::process_names(&mut super::feature_hid_windows::NativeBackend)?;
    super::feature_hid::guard_processes(&names)
}
/// Recheck after the file/hash/lock wait and after initialization. A result
/// closes before the guard on a late conflict; no existing process is stopped.
fn with_guard<M: SharedMutex, T>(
    mutex: &M,
    mut check: impl FnMut() -> Result<(), String>,
    body: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let _guard = mutex.lock()?;
    check()?;
    let value = body()?;
    check()?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    };
    fn stand_name() -> String {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        format!(
            "Local\\pc-control-mutex-stand-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }
    #[test]
    fn writer_rechecks_run_under_guard_and_close_late_result_before_unlock() {
        let mutex = NamedMutex::named(&stand_name(), Cancellation::default()).unwrap();
        let opened = Cell::new(false);
        let result = with_guard(
            &mutex,
            || {
                assert!(mutex.epoch().is_ok());
                Err("writer started while waiting".into())
            },
            || {
                opened.set(true);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!opened.get());
        assert!(mutex.epoch().is_err());
        struct Probe<'a> {
            mutex: &'a NamedMutex,
            closed: &'a Cell<bool>,
        }
        impl Drop for Probe<'_> {
            fn drop(&mut self) {
                assert!(self.mutex.epoch().is_ok());
                self.closed.set(true);
            }
        }
        let checks = Cell::new(0);
        let closed = Cell::new(false);
        let result = with_guard(
            &mutex,
            || {
                checks.set(checks.get() + 1);
                assert!(mutex.epoch().is_ok());
                if checks.get() == 2 {
                    Err("late writer".into())
                } else {
                    Ok(())
                }
            },
            || {
                Ok(Probe {
                    mutex: &mutex,
                    closed: &closed,
                })
            },
        );
        assert!(result.is_err());
        assert!(closed.get());
        assert!(mutex.epoch().is_err());
        with_guard(
            &mutex,
            || {
                assert!(mutex.epoch().is_ok());
                Ok(())
            },
            || Ok(()),
        )
        .unwrap();
        assert!(mutex.epoch().is_err());
    }
    #[test]
    fn raw_ioctl_errors_only_touch_a_new_delete_on_close_stand_file() {
        use windows_sys::Win32::Storage::FileSystem::{
            CREATE_NEW, FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_DELETE_ON_CLOSE,
        };
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "pc-control-pawnio-stand-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let path_w = wide(path.to_str().unwrap());
        let handle = owned(unsafe {
            CreateFileW(
                path_w.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                CREATE_NEW,
                FILE_FLAG_OVERLAPPED | FILE_FLAG_DELETE_ON_CLOSE | FILE_ATTRIBUTE_TEMPORARY,
                null_mut(),
            )
        })
        .unwrap();
        let mut device = NativeDevice { handle };
        let token = Cancellation::default();
        assert!(
            device
                .ioctl(pawnio::IOCTL_LOAD, b"fake input", &mut [], &token)
                .is_err()
        );
        let mut output = [0xaa; 24];
        assert!(
            device
                .ioctl(
                    pawnio::IOCTL_EXECUTE,
                    &pawnio::identity_request(),
                    &mut output,
                    &token
                )
                .is_err()
        );
        assert_eq!(output, [0; 24]);
        assert!(device.ioctl(0, b"fake", &mut [], &token).is_err());
        assert!(
            device
                .ioctl(
                    pawnio::IOCTL_LOAD,
                    &vec![0; pawnio::MAX_MODULE_BYTES + 1],
                    &mut [],
                    &token
                )
                .is_err()
        );
        drop(device);
        assert!(!path.exists());
    }
    #[test]
    fn own_named_mutex_epochs_clone_recursion_and_release() {
        let mutex = NamedMutex::named(&stand_name(), Cancellation::default()).unwrap();
        let clone = mutex.clone();
        assert!(mutex.epoch().is_err());
        let guard = mutex.lock().unwrap();
        assert_eq!(clone.epoch().unwrap(), 1);
        assert!(clone.lock().is_err());
        drop(guard);
        assert!(clone.epoch().is_err());
        let guard = clone.lock().unwrap();
        assert_eq!(mutex.epoch().unwrap(), 2);
        drop(guard);
    }
    #[test]
    fn independent_handles_contend_only_on_our_own_stand_object() {
        let name = stand_name();
        let mutex = NamedMutex::named(&name, Cancellation::default()).unwrap();
        let guard = mutex.lock().unwrap();
        let thread = std::thread::spawn(move || {
            let other = NamedMutex::named(&name, Cancellation::default()).unwrap();
            let start = Instant::now();
            assert!(other.lock().is_err());
            assert!(start.elapsed() < Duration::from_secs(2));
        });
        thread.join().unwrap();
        drop(guard);
        assert!(mutex.lock().is_ok());
    }
    #[test]
    fn abandoned_or_cancelled_own_mutex_never_grants_a_guard() {
        let name = stand_name();
        let mutex = NamedMutex::named(&name, Cancellation::default()).unwrap();
        let (ready, seen) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let other = NamedMutex::named(&name, Cancellation::default()).unwrap();
            let guard = other.lock().unwrap();
            std::mem::forget(guard);
            ready.send(()).unwrap();
        });
        seen.recv_timeout(Duration::from_secs(1)).unwrap();
        thread.join().unwrap();
        let error = mutex.lock().err().unwrap();
        assert!(error.contains("abandoned"));
        assert!(mutex.lock().is_err());
        let token = Cancellation::default();
        token.cancel_for_test();
        let cancelled = NamedMutex::named(&stand_name(), token).unwrap();
        assert!(cancelled.lock().is_err());
    }
}
