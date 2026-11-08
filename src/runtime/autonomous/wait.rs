//! Private wake event + one-shot timer, never a system timer-resolution change.
use std::time::Duration;

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{Foundation::WAIT_OBJECT_0, System::Threading::*};
    pub struct Signal(OwnedHandle);
    impl Signal {
        pub fn new() -> Result<Self, String> {
            let raw = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
            if raw.is_null() {
                return Err(format!(
                    "Create RGB wake event: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(Self(unsafe { OwnedHandle::from_raw_handle(raw) }))
        }
        pub fn notify(&self) {
            unsafe {
                SetEvent(self.0.as_raw_handle());
            }
        }
    }
    pub struct Timer {
        handle: OwnedHandle,
        precise: bool,
    }
    impl Timer {
        pub fn new() -> Result<Self, String> {
            let mut raw = unsafe {
                CreateWaitableTimerExW(std::ptr::null(), std::ptr::null(), 0x2, TIMER_ALL_ACCESS)
            };
            let precise = !raw.is_null();
            if raw.is_null() {
                raw = unsafe {
                    CreateWaitableTimerExW(std::ptr::null(), std::ptr::null(), 0, TIMER_ALL_ACCESS)
                };
            }
            if raw.is_null() {
                return Err(format!(
                    "Create RGB timer: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(Self {
                handle: unsafe { OwnedHandle::from_raw_handle(raw) },
                precise,
            })
        }
        pub fn precise(&self) -> bool {
            self.precise
        }
        pub fn wait(
            &self,
            signal: &Signal,
            _seen: &mut u64,
            duration: Option<Duration>,
        ) -> Result<(), String> {
            let handles = [signal.0.as_raw_handle(), self.handle.as_raw_handle()];
            let count = if let Some(duration) = duration {
                let due = -i64::try_from(duration.as_nanos().div_ceil(100).max(1))
                    .map_err(|_| "RGB timer interval overflow")?;
                if unsafe {
                    SetWaitableTimerEx(
                        handles[1],
                        &due,
                        0,
                        None,
                        std::ptr::null(),
                        std::ptr::null(),
                        0,
                    )
                } == 0
                {
                    return Err(format!(
                        "Arm RGB timer: {}",
                        std::io::Error::last_os_error()
                    ));
                }
                2
            } else {
                1
            };
            match unsafe { WaitForMultipleObjects(count, handles.as_ptr(), 0, INFINITE) } {
                WAIT_OBJECT_0 => Ok(()),
                v if count == 2 && v == WAIT_OBJECT_0 + 1 => Ok(()),
                _ => Err(format!(
                    "Wait for RGB cadence: {}",
                    std::io::Error::last_os_error()
                )),
            }
        }
    }
}
#[cfg(not(windows))]
mod platform {
    use super::*;
    use std::sync::{Condvar, Mutex};
    pub struct Signal {
        version: Mutex<u64>,
        wake: Condvar,
    }
    impl Signal {
        pub fn new() -> Result<Self, String> {
            Ok(Self {
                version: Mutex::new(0),
                wake: Condvar::new(),
            })
        }
        pub fn notify(&self) {
            let mut v = self.version.lock().unwrap_or_else(|e| e.into_inner());
            *v = v.wrapping_add(1);
            self.wake.notify_one();
        }
    }
    pub struct Timer;
    impl Timer {
        pub fn new() -> Result<Self, String> {
            Ok(Self)
        }
        pub fn precise(&self) -> bool {
            false
        }
        pub fn wait(
            &self,
            signal: &Signal,
            seen: &mut u64,
            duration: Option<Duration>,
        ) -> Result<(), String> {
            let mut version = signal.version.lock().unwrap_or_else(|e| e.into_inner());
            if *version == *seen {
                version = if let Some(duration) = duration {
                    signal
                        .wake
                        .wait_timeout(version, duration)
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                } else {
                    signal.wake.wait(version).unwrap_or_else(|e| e.into_inner())
                };
            }
            *seen = *version;
            Ok(())
        }
    }
}
pub(crate) use platform::{Signal, Timer};
