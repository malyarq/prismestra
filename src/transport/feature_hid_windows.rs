//! Native feature HID, activated only by explicit runtime factories. Stand tests
//! never inspect/open a HID path. Requests retain buffers/OVERLAPPED until drained.
use super::feature_hid::{Backend, Descriptor, Family, Reports};
use crate::runtime::output::Cancellation;
use std::{
    mem::size_of,
    ptr::{null, null_mut},
    time::Duration,
};
use windows_sys::Win32::{
    Devices::HumanInterfaceDevice::*,
    Foundation::*,
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    },
    System::{
        Diagnostics::ToolHelp::*,
        IO::{DeviceIoControl, OVERLAPPED},
    },
};

// Windows SDK10.0.26100 shared/hidclass.h:135-158, devioctl.h:
// FILE_DEVICE_KEYBOARD=11, function100, METHOD_IN/OUT_DIRECT=1/2, ANY_ACCESS=0.
const SET_FEATURE: u32 = (11 << 16) | (100 << 2) | 1;
const GET_FEATURE: u32 = (11 << 16) | (100 << 2) | 2;
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn error(action: &str) -> String {
    format!("{action}: {}", std::io::Error::last_os_error())
}
fn owned(raw: HANDLE) -> Result<Handle, String> {
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        Err(error("Open feature HID handle"))
    } else {
        Ok(Handle(raw))
    }
}
fn wide(path: &str) -> Result<Vec<u16>, String> {
    if path.contains('\0') {
        return Err("NUL in HID path".into());
    }
    Ok(path.encode_utf16().chain(Some(0)).collect())
}
struct Preparsed(PHIDP_PREPARSED_DATA);
impl Drop for Preparsed {
    fn drop(&mut self) {
        unsafe {
            HidD_FreePreparsedData(self.0);
        }
    }
}
fn inspect(handle: HANDLE, path: &str) -> Result<Descriptor, String> {
    let mut attrs = HIDD_ATTRIBUTES {
        Size: size_of::<HIDD_ATTRIBUTES>() as u32,
        ..Default::default()
    };
    if !unsafe { HidD_GetAttributes(handle, &mut attrs) } {
        return Err(error("Get feature HID attributes"));
    }
    let mut raw = 0;
    if !unsafe { HidD_GetPreparsedData(handle, &mut raw) } || raw == 0 {
        return Err(error("Get feature HID preparsed data"));
    }
    let pp = Preparsed(raw);
    let mut caps = HIDP_CAPS::default();
    if unsafe { HidP_GetCaps(pp.0, &mut caps) } != HIDP_STATUS_SUCCESS {
        return Err("Get feature HID caps failed".into());
    }
    let mut ids = vec![];
    let value_count = caps.NumberFeatureValueCaps;
    if value_count > 128 || caps.NumberFeatureButtonCaps > 128 {
        return Err("Feature caps count exceeds bounds".into());
    }
    if value_count > 0 {
        let mut values = vec![HIDP_VALUE_CAPS::default(); usize::from(value_count)];
        let mut count = value_count;
        if unsafe { HidP_GetValueCaps(HidP_Feature, values.as_mut_ptr(), &mut count, pp.0) }
            != HIDP_STATUS_SUCCESS
            || count != value_count
        {
            return Err("Incomplete feature value caps".into());
        }
        ids.extend(values.into_iter().map(|v| v.ReportID));
    }
    let button_count = caps.NumberFeatureButtonCaps;
    if button_count > 0 {
        let mut buttons = vec![HIDP_BUTTON_CAPS::default(); usize::from(button_count)];
        let mut count = button_count;
        if unsafe { HidP_GetButtonCaps(HidP_Feature, buttons.as_mut_ptr(), &mut count, pp.0) }
            != HIDP_STATUS_SUCCESS
            || count != button_count
        {
            return Err("Incomplete feature button caps".into());
        }
        ids.extend(buttons.into_iter().map(|v| v.ReportID));
    }
    Ok(Descriptor {
        path: path.into(),
        vid: attrs.VendorID,
        pid: attrs.ProductID,
        usage_page: caps.UsagePage,
        usage: caps.Usage,
        feature_len: usize::from(caps.FeatureReportByteLength),
        feature_ids: ids,
        feature_caps_complete: true,
    })
}
fn process_names() -> Result<Vec<String>, String> {
    let snapshot = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) })?;
    let mut process = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    if unsafe { Process32FirstW(snapshot.0, &mut process) } == 0 {
        return Err(error("Read RGB process guard"));
    }
    let mut names = vec![];
    loop {
        let end = process
            .szExeFile
            .iter()
            .position(|c| *c == 0)
            .ok_or("Unterminated process name")?;
        names.push(
            String::from_utf16(&process.szExeFile[..end]).map_err(|_| "Invalid process name")?,
        );
        if names.len() > 32768 {
            return Err("Process guard exceeds bounds".into());
        }
        if unsafe { Process32NextW(snapshot.0, &mut process) } == 0 {
            if unsafe { GetLastError() } == ERROR_NO_MORE_FILES {
                return Ok(names);
            }
            return Err(error("Complete RGB process guard"));
        }
    }
}
pub(crate) struct NativeBackend;
pub(crate) struct NativeLink {
    handle: Handle,
    path: String,
}
impl Backend for NativeBackend {
    type Link = NativeLink;
    fn process_names(&mut self) -> Result<Vec<String>, String> {
        process_names()
    }
    fn inspect(&mut self, family: Family, path: &str) -> Result<Descriptor, String> {
        family.validate_path(path)?;
        let path_w = wide(path)?;
        let handle = owned(unsafe {
            CreateFileW(
                path_w.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        })?;
        inspect(handle.0, path)
    }
    fn open_exclusive(&mut self, family: Family, path: &str) -> Result<NativeLink, String> {
        family.validate_path(path)?;
        let path_w = wide(path)?;
        let handle = owned(unsafe {
            CreateFileW(
                path_w.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        })?;
        Ok(NativeLink {
            handle,
            path: path.into(),
        })
    }
}
/// Nominal250ms. Cancellation always drains the exact request before its memory
/// can die. A broken kernel driver may make that final drain unbounded.
fn ioctl(
    handle: HANDLE,
    code: u32,
    bytes: &mut [u8],
    get: bool,
    cancel: &Cancellation,
) -> Result<usize, String> {
    overlapped(handle, bytes, cancel, |bytes, op| unsafe {
        DeviceIoControl(
            handle,
            code,
            bytes.as_ptr().cast(),
            bytes.len() as u32,
            if get {
                bytes.as_mut_ptr().cast()
            } else {
                null_mut()
            },
            if get { bytes.len() as u32 } else { 0 },
            null_mut(),
            op,
        )
    })
}
/// Internal start callbacks must not retain pointers or panic after submission.
/// The common wait/drain path is also exercised with owned stand pipes, not HID.
fn overlapped(
    handle: HANDLE,
    bytes: &mut [u8],
    cancel: &Cancellation,
    begin: impl FnOnce(&mut [u8], *mut OVERLAPPED) -> windows_sys::core::BOOL,
) -> Result<usize, String> {
    if bytes.is_empty() || bytes.len() > 4096 {
        return Err("Invalid feature transfer buffer".into());
    }
    // The borrowed slice remains alive for this synchronous call; all internal
    // starters immediately call an OS API and cannot panic after submission.
    unsafe { super::windows_io::run(handle, cancel, |op| begin(bytes, op)) }
}
impl Reports for NativeLink {
    fn descriptor(&mut self) -> Result<Descriptor, String> {
        inspect(self.handle.0, &self.path)
    }
    fn transfer(
        &mut self,
        get: bool,
        bytes: &mut [u8],
        cancel: &Cancellation,
    ) -> Result<usize, String> {
        ioctl(
            self.handle.0,
            if get { GET_FEATURE } else { SET_FEATURE },
            bytes,
            get,
            cancel,
        )
    }
    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration.min(Duration::from_millis(250)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    };
    use std::time::Instant;
    use windows_sys::Win32::{
        Storage::FileSystem::{PIPE_ACCESS_DUPLEX, ReadFile, WriteFile},
        System::Pipes::*,
        System::{IO::GetOverlappedResult, Threading::CreateEventW},
    };
    enum ServerMode {
        Reply,
        DelayedReply,
        Wait,
        None,
    }
    fn pair(
        mode: ServerMode,
    ) -> (
        Handle,
        mpsc::Sender<()>,
        mpsc::Receiver<()>,
        std::thread::JoinHandle<()>,
    ) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = format!(
            r"\\.\pipe\pc-control-feature-stand-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let name = wide(&path).unwrap();
        let server = owned(unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4096,
                4096,
                1000,
                null(),
            )
        })
        .unwrap();
        let event = owned(unsafe { CreateEventW(null(), 1, 0, null()) }).unwrap();
        let mut op = OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        };
        let connected = unsafe { ConnectNamedPipe(server.0, &mut op) };
        assert!(connected != 0 || unsafe { GetLastError() } == ERROR_IO_PENDING);
        let client = owned(unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        })
        .unwrap();
        let mut count = 0;
        assert_ne!(
            unsafe { GetOverlappedResult(server.0, &op, &mut count, 1) },
            0
        );
        let read_mode = PIPE_READMODE_MESSAGE;
        assert_ne!(
            unsafe { SetNamedPipeHandleState(client.0, &read_mode, null(), null()) },
            0
        );
        // Exactly one owner moves into the stand thread; no process is spawned.
        let raw = server.0 as usize;
        std::mem::forget(server);
        let (done, finish) = mpsc::channel();
        let (seen, input) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let server = Handle(raw as HANDLE);
            let cancel = Cancellation::default();
            if !matches!(mode, ServerMode::None) {
                let mut request = [0; 65];
                let count = overlapped(server.0, &mut request, &cancel, |b, op| unsafe {
                    ReadFile(server.0, b.as_mut_ptr(), b.len() as u32, null_mut(), op)
                })
                .unwrap();
                assert_eq!(count, 65);
                assert_eq!(request, [3; 65]);
                seen.send(()).unwrap();
                if matches!(mode, ServerMode::Reply | ServerMode::DelayedReply) {
                    if matches!(mode, ServerMode::DelayedReply) {
                        std::thread::sleep(Duration::from_millis(60));
                    }
                    let mut response = [9; 65];
                    assert_eq!(
                        overlapped(server.0, &mut response, &cancel, |b, op| unsafe {
                            WriteFile(server.0, b.as_ptr(), b.len() as u32, null_mut(), op)
                        })
                        .unwrap(),
                        65
                    );
                }
            }
            // Bounded even if an assertion fails on the client.
            let _ = finish.recv_timeout(Duration::from_secs(2));
        });
        (client, done, input, thread)
    }
    fn transceive(
        client: &Handle,
        cancel: &Cancellation,
        buffer: &mut [u8],
    ) -> Result<usize, String> {
        overlapped(client.0, buffer, cancel, |b, op| unsafe {
            TransactNamedPipe(
                client.0,
                b.as_ptr().cast(),
                b.len() as u32,
                b.as_mut_ptr().cast(),
                b.len() as u32,
                null_mut(),
                op,
            )
        })
    }
    #[test]
    fn owned_pipe_exchange_exercises_shared_overlapped_completion() {
        let (client, done, seen, thread) = pair(ServerMode::Reply);
        let mut buffer = [3; 65];
        assert_eq!(
            transceive(&client, &Cancellation::default(), &mut buffer).unwrap(),
            65
        );
        seen.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(buffer, [9; 65]);
        done.send(()).unwrap();
        thread.join().unwrap();
    }
    #[test]
    fn owned_pipe_early_event_requires_real_completion_or_cancellation_drain() {
        for case in 0..3 {
            let (client, done, seen, thread) = pair(if case == 0 {
                ServerMode::DelayedReply
            } else {
                ServerMode::Wait
            });
            let token = Cancellation::default();
            let cancel_thread = if case == 2 {
                let token = token.clone();
                Some(std::thread::spawn(move || {
                    seen.recv_timeout(Duration::from_secs(1)).unwrap();
                    token.cancel_for_test();
                }))
            } else {
                None
            };
            let mut buffer = [3; 65];
            let mut pending = false;
            let mut signalled = false;
            let started = Instant::now();
            let result = overlapped(client.0, &mut buffer, &token, |b, op| unsafe {
                let begun = TransactNamedPipe(
                    client.0,
                    b.as_ptr().cast(),
                    b.len() as u32,
                    b.as_mut_ptr().cast(),
                    b.len() as u32,
                    null_mut(),
                    op,
                );
                let error = GetLastError();
                pending = begun == 0 && error == ERROR_IO_PENDING;
                if pending {
                    signalled = windows_sys::Win32::System::Threading::SetEvent((*op).hEvent) != 0;
                }
                // Preserve the immediate OS result expected by the starter contract.
                SetLastError(error);
                begun
            });
            // Join the stand owners before assertions so failure leaves no work behind.
            let _ = done.send(());
            thread.join().unwrap();
            if let Some(cancel_thread) = cancel_thread {
                cancel_thread.join().unwrap();
            }
            assert!(pending && signalled);
            assert!(started.elapsed() < Duration::from_secs(2));
            if case == 0 {
                assert_eq!(result.unwrap(), 65);
                assert_eq!(buffer, [9; 65]);
                assert!(started.elapsed() >= Duration::from_millis(60));
            } else {
                assert!(result.unwrap_err().contains("drained"));
                if case == 1 {
                    assert!(started.elapsed() >= Duration::from_millis(250));
                }
            }
        }
    }
    #[test]
    fn owned_pipe_pending_timeout_and_active_cancel_are_drained() {
        for active in [false, true] {
            let (client, done, seen, thread) = pair(ServerMode::Wait);
            let token = Cancellation::default();
            let cancel_thread = if active {
                let token = token.clone();
                Some(std::thread::spawn(move || {
                    seen.recv_timeout(Duration::from_secs(1)).unwrap();
                    token.cancel_for_test();
                }))
            } else {
                None
            };
            let mut buffer = [3; 65];
            let started = Instant::now();
            let error = transceive(&client, &token, &mut buffer).unwrap_err();
            assert!(error.contains("drained"));
            assert!(started.elapsed() < Duration::from_secs(2));
            if let Some(cancel_thread) = cancel_thread {
                cancel_thread.join().unwrap();
            }
            done.send(()).unwrap();
            thread.join().unwrap();
        }
    }
    #[test]
    fn feature_ioctl_immediate_errors_only_use_an_owned_pipe() {
        let (client, done, _, thread) = pair(ServerMode::None);
        for (get, code) in [(false, SET_FEATURE), (true, GET_FEATURE)] {
            let mut bytes = [0; 65];
            assert!(ioctl(client.0, code, &mut bytes, get, &Cancellation::default()).is_err());
        }
        done.send(()).unwrap();
        thread.join().unwrap();
    }
}
