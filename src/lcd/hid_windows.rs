//! Native implementation, dormant until explicitly called by a future host.
//! Tests do not enumerate or open physical devices. No feature/configuration calls exist.
use super::{
    hid_adapter::{Backend, Reports, allowed_write},
    hid_policy::{self, Descriptor, Kind},
};
use std::{
    mem::{offset_of, size_of},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Devices::{DeviceAndDriverInstallation::*, HumanInterfaceDevice::*},
    Foundation::*,
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        ReadFile, WriteFile,
    },
    System::{
        Diagnostics::ToolHelp::*,
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        Threading::{CreateEventW, WaitForSingleObject},
    },
};
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
fn handle(raw: HANDLE) -> Result<Handle, String> {
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        Err(error("Open TT HID handle"))
    } else {
        Ok(Handle(raw))
    }
}
fn wide(value: &str) -> Result<Vec<u16>, String> {
    if value.contains('\0') {
        return Err("NUL in HID path".into());
    }
    Ok(value.encode_utf16().chain(Some(0)).collect())
}
struct DeviceList(HDEVINFO);
impl Drop for DeviceList {
    fn drop(&mut self) {
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}
struct Preparsed(PHIDP_PREPARSED_DATA);
impl Drop for Preparsed {
    fn drop(&mut self) {
        unsafe {
            HidD_FreePreparsedData(self.0);
        }
    }
}
fn value_ids(
    pp: PHIDP_PREPARSED_DATA,
    kind: HIDP_REPORT_TYPE,
    count: u16,
) -> Result<Vec<u8>, String> {
    if !(1..=128).contains(&count) {
        return Err("Unexpected TT HID value caps count".into());
    }
    let mut values = vec![HIDP_VALUE_CAPS::default(); usize::from(count)];
    let mut returned = count;
    if unsafe { HidP_GetValueCaps(kind, values.as_mut_ptr(), &mut returned, pp) }
        != HIDP_STATUS_SUCCESS
        || returned != count
    {
        return Err("Incomplete TT HID value caps query".into());
    }
    Ok(values.into_iter().map(|v| v.ReportID).collect())
}
fn inspect(device: HANDLE, path: &str) -> Result<Descriptor, String> {
    let mut attrs = HIDD_ATTRIBUTES {
        Size: size_of::<HIDD_ATTRIBUTES>() as u32,
        ..HIDD_ATTRIBUTES::default()
    };
    if !unsafe { HidD_GetAttributes(device, &mut attrs) } {
        return Err(error("Read TT HID attributes"));
    }
    let mut raw = 0;
    if !unsafe { HidD_GetPreparsedData(device, &mut raw) } || raw == 0 {
        return Err(error("Read TT HID preparsed data"));
    }
    let pp = Preparsed(raw);
    let mut caps = HIDP_CAPS::default();
    if unsafe { HidP_GetCaps(pp.0, &mut caps) } != HIDP_STATUS_SUCCESS {
        return Err("Read TT HID caps failed".into());
    }
    Ok(Descriptor {
        path: path.into(),
        vid: attrs.VendorID,
        pid: attrs.ProductID,
        usage_page: caps.UsagePage,
        usage: caps.Usage,
        input_len: caps.InputReportByteLength,
        output_len: caps.OutputReportByteLength,
        feature_len: caps.FeatureReportByteLength,
        input_buttons: caps.NumberInputButtonCaps,
        output_buttons: caps.NumberOutputButtonCaps,
        input_ids: value_ids(pp.0, HidP_Input, caps.NumberInputValueCaps)?,
        output_ids: value_ids(pp.0, HidP_Output, caps.NumberOutputValueCaps)?,
    })
}
// Vec<u64> supplies alignment for the native detail header. DevicePath starts at
// offsetof(DevicePath), not cbSize; on x64 those are4 and8 respectively.
fn detail_path(buffer: &[u64], returned_bytes: usize) -> Result<String, String> {
    let offset = offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
    if returned_bytes > std::mem::size_of_val(buffer)
        || returned_bytes < size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>()
        || returned_bytes < offset + 2
        || !(returned_bytes - offset).is_multiple_of(2)
    {
        return Err("Invalid HID detail buffer size".into());
    }
    let path = unsafe {
        std::slice::from_raw_parts(
            buffer.as_ptr().cast::<u8>().add(offset).cast::<u16>(),
            (returned_bytes - offset) / 2,
        )
    };
    let end = path
        .iter()
        .position(|c| *c == 0)
        .ok_or("Unterminated HID interface path")?;
    if end == 0 {
        return Err("Empty HID interface path".into());
    }
    String::from_utf16(&path[..end]).map_err(|_| "Invalid UTF16 HID interface path".into())
}
fn matching_hint(kind: Kind, path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.contains(match kind {
        Kind::Display => "vid_264a&pid_233c&mi_01",
        Kind::Liquid => "vid_264a&pid_233c&mi_00",
    })
}
pub(crate) fn metadata_paths(mut accept: impl FnMut(&str) -> bool) -> Result<Vec<String>, String> {
    let mut guid = windows_sys::core::GUID::default();
    unsafe {
        HidD_GetHidGuid(&mut guid);
    }
    let raw = unsafe {
        SetupDiGetClassDevsW(
            &guid,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if raw == INVALID_HANDLE_VALUE as HDEVINFO {
        return Err(error("Enumerate HID interfaces"));
    }
    let list = DeviceList(raw);
    let mut candidates = Vec::new();
    for index in 0..16384 {
        let mut data = SP_DEVICE_INTERFACE_DATA {
            cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..SP_DEVICE_INTERFACE_DATA::default()
        };
        if unsafe { SetupDiEnumDeviceInterfaces(list.0, null(), &guid, index, &mut data) } == 0 {
            if unsafe { GetLastError() } == ERROR_NO_MORE_ITEMS {
                return Ok(candidates);
            }
            return Err(error("Read HID interface list"));
        }
        let mut required = 0;
        let first = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                list.0,
                &data,
                null_mut(),
                0,
                &mut required,
                null_mut(),
            )
        };
        if first != 0 || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            return Err(error("Read HID detail size"));
        }
        if (required as usize) < size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() || required > 65536
        {
            return Err("Unsupported HID detail size".into());
        }
        let mut storage = vec![0u64; (required as usize).div_ceil(8)];
        let detail = storage
            .as_mut_ptr()
            .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
        unsafe {
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        }
        let mut returned = required;
        if unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                list.0,
                &data,
                detail,
                required,
                &mut returned,
                null_mut(),
            )
        } == 0
        {
            return Err(error("Read HID interface detail"));
        }
        let path = detail_path(&storage, returned as usize)?;
        if !accept(&path) {
            continue;
        }
        if candidates.len() >= 128 {
            return Err("Matching HID metadata exceeds128devices".into());
        }
        candidates.push(path);
    }
    Err("HID interface list exceeds limit".into())
}
fn enumerate(kind: Kind) -> Result<Vec<Descriptor>, String> {
    let mut candidates = Vec::new();
    for path in metadata_paths(|path| matching_hint(kind, path))? {
        let name = wide(&path)?;
        let query = handle(unsafe {
            CreateFileW(
                name.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        })?;
        candidates.push(inspect(query.0, &path)?);
    }
    Ok(candidates)
}
fn process_names() -> Result<Vec<String>, String> {
    let snapshot = handle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) })?;
    let mut process = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..PROCESSENTRY32W::default()
    };
    if unsafe { Process32FirstW(snapshot.0, &mut process) } == 0 {
        return Err(error("Read TT process guard"));
    }
    let mut names = Vec::new();
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
            return Err("Process list exceeds TT guard limit".into());
        }
        if unsafe { Process32NextW(snapshot.0, &mut process) } == 0 {
            if unsafe { GetLastError() } == ERROR_NO_MORE_FILES {
                return Ok(names);
            }
            return Err(error("Complete TT process guard"));
        }
    }
}
pub struct NativeBackend;
pub struct NativeLink {
    handle: Handle,
    path: String,
    kind: Kind,
    checked_processes: bool,
    failed: bool,
    status_written: bool,
    status_read: bool,
}
impl Backend for NativeBackend {
    type Link = NativeLink;
    fn process_names(&mut self) -> Result<Vec<String>, String> {
        process_names()
    }
    fn candidates(&mut self, kind: Kind) -> Result<Vec<Descriptor>, String> {
        enumerate(kind)
    }
    fn open(&mut self, kind: Kind, path: &str) -> Result<NativeLink, String> {
        hid_policy::validate_path(kind, path)?;
        // Exclusive for both interfaces: existing writers are not stopped or interfered with.
        let name = wide(path)?;
        let handle = handle(unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        })?;
        let descriptor = inspect(handle.0, path)?;
        hid_policy::validate(kind, &descriptor)?;
        Ok(NativeLink {
            handle,
            path: path.into(),
            kind,
            checked_processes: false,
            failed: false,
            status_written: false,
            status_read: false,
        })
    }
}
impl NativeLink {
    fn transfer(
        &mut self,
        bytes: &mut [u8],
        write: bool,
        timeout_ms: u32,
    ) -> Result<usize, String> {
        if self.failed {
            return Err("TT HID link is stopped after an I/O error".into());
        }
        if !(1..=250).contains(&timeout_ms) {
            return Err("TT HID I/O timeout must be1..250ms".into());
        }
        let expected = match (self.kind, write) {
            (Kind::Display, true) => 1025,
            (Kind::Display, false) => 17,
            _ => 441,
        };
        if bytes.len() != expected {
            return Err("TT HID report length mismatch".into());
        }
        let result = (|| {
            let event = handle(unsafe { CreateEventW(null(), 1, 0, null()) })?;
            let mut op = OVERLAPPED {
                hEvent: event.0,
                ..OVERLAPPED::default()
            };
            let mut count = 0;
            let started = unsafe {
                if write {
                    WriteFile(
                        self.handle.0,
                        bytes.as_ptr(),
                        bytes.len() as u32,
                        null_mut(),
                        &mut op,
                    )
                } else {
                    ReadFile(
                        self.handle.0,
                        bytes.as_mut_ptr(),
                        bytes.len() as u32,
                        null_mut(),
                        &mut op,
                    )
                }
            };
            if started == 0 {
                if unsafe { GetLastError() } != ERROR_IO_PENDING {
                    return Err(error("Start TT HID report"));
                }
                if unsafe { WaitForSingleObject(event.0, timeout_ms) } != WAIT_OBJECT_0 {
                    unsafe {
                        CancelIoEx(self.handle.0, &op);
                        GetOverlappedResult(self.handle.0, &op, &mut count, 1);
                    }
                    return Err("TT HID I/O timeout; cancellation completed".into());
                }
            }
            // Both immediate and pending overlapped completions report their
            // byte count here, rather than through the synchronous out pointer.
            if unsafe { GetOverlappedResult(self.handle.0, &op, &mut count, 0) } == 0 {
                return Err(error("Complete TT HID report"));
            }
            if count as usize != bytes.len() {
                return Err("Short TT HID report".into());
            }
            if !write && bytes[0] != 0 {
                return Err("Unexpected TT HID input report ID".into());
            }
            Ok(count as usize)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}
impl Reports for NativeLink {
    fn now_ms(&self) -> u64 {
        crate::clock::monotonic_ms()
    }
    fn descriptor(&mut self) -> Result<Descriptor, String> {
        inspect(self.handle.0, &self.path)
    }
    fn write(&mut self, bytes: &[u8], timeout_ms: u32) -> Result<usize, String> {
        allowed_write(self.kind, bytes)?;
        if !self.checked_processes {
            hid_policy::guard_processes(&process_names()?)?;
            self.checked_processes = true;
        }
        if self.kind == Kind::Liquid && self.status_written {
            return Err("Liquid link permits one status request only".into());
        }
        let mut report = bytes.to_vec();
        let count = self.transfer(&mut report, true, timeout_ms)?;
        if self.kind == Kind::Liquid {
            self.status_written = true;
        }
        Ok(count)
    }
    fn read(&mut self, bytes: &mut [u8], timeout_ms: u32) -> Result<usize, String> {
        if self.kind == Kind::Liquid && (!self.status_written || self.status_read) {
            return Err("Liquid link expects one response to its status request".into());
        }
        let count = self.transfer(bytes, false, timeout_ms)?;
        if self.kind == Kind::Liquid {
            self.status_read = true;
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            atomic::{AtomicU64, Ordering},
            mpsc,
        },
        time::Duration,
    };
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::*;

    // Exercise the actual overlapped transfer on owned pipes, never on HID.
    // The descriptor/command policy is tested separately with fake Backends.
    fn pipe_link(
        reply: Vec<u8>,
        delay: Duration,
        exchange: bool,
    ) -> (NativeLink, mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = format!(
            r"\\.\pipe\pc-control-hid-stand-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let name = wide(&path).unwrap();
        let server = handle(unsafe {
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
        let event = handle(unsafe { CreateEventW(null(), 1, 0, null()) }).unwrap();
        let mut op = OVERLAPPED {
            hEvent: event.0,
            ..OVERLAPPED::default()
        };
        let connected = unsafe { ConnectNamedPipe(server.0, &mut op) };
        assert!(connected != 0 || unsafe { GetLastError() } == ERROR_IO_PENDING);
        let client = handle(unsafe {
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
        // Move a numeric handle into the thread; ownership is transferred once.
        let raw = server.0 as usize;
        std::mem::forget(server);
        let (done, finished) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut server = NativeLink {
                handle: Handle(raw as HANDLE),
                path: String::new(),
                kind: Kind::Display,
                checked_processes: true,
                failed: false,
                status_written: false,
                status_read: false,
            };
            if exchange {
                // Server reads the 1025-byte output report using an independent
                // overlapped operation (NativeLink's read shape is 17 bytes).
                let event = handle(unsafe { CreateEventW(null(), 1, 0, null()) }).unwrap();
                let mut op = OVERLAPPED {
                    hEvent: event.0,
                    ..OVERLAPPED::default()
                };
                let mut input = [0u8; 1025];
                let started = unsafe {
                    ReadFile(
                        server.handle.0,
                        input.as_mut_ptr(),
                        1025,
                        null_mut(),
                        &mut op,
                    )
                };
                assert!(started != 0 || unsafe { GetLastError() } == ERROR_IO_PENDING);
                let mut count = 0;
                assert_ne!(
                    unsafe { GetOverlappedResult(server.handle.0, &op, &mut count, 1) },
                    0
                );
                assert_eq!(count, 1025);
                assert_eq!(input[1], 8);
            }
            std::thread::sleep(delay);
            if !reply.is_empty() {
                let event = handle(unsafe { CreateEventW(null(), 1, 0, null()) }).unwrap();
                let mut op = OVERLAPPED {
                    hEvent: event.0,
                    ..OVERLAPPED::default()
                };
                let started = unsafe {
                    WriteFile(
                        server.handle.0,
                        reply.as_ptr(),
                        reply.len() as u32,
                        null_mut(),
                        &mut op,
                    )
                };
                assert!(started != 0 || unsafe { GetLastError() } == ERROR_IO_PENDING);
                let mut count = 0;
                assert_ne!(
                    unsafe { GetOverlappedResult(server.handle.0, &op, &mut count, 1) },
                    0
                );
                assert_eq!(count as usize, reply.len());
            }
            let _ = finished.recv_timeout(Duration::from_secs(2));
            // Keep the endpoint alive until the client's read has completed.
            server.failed = true;
        });
        (
            NativeLink {
                handle: client,
                path,
                kind: Kind::Display,
                checked_processes: true,
                failed: false,
                status_written: false,
                status_read: false,
            },
            done,
            thread,
        )
    }

    #[test]
    fn owned_pipe_reports_and_ack_use_native_overlapped_io() {
        let (mut link, done, thread) = pipe_link(vec![0; 17], Duration::ZERO, true);
        let mut report = [0; 1025];
        report[1] = 8;
        report[2] = 1;
        assert_eq!(link.write(&report, 250).unwrap(), 1025);
        assert_eq!(link.read(&mut [0; 17], 250).unwrap(), 17);
        drop(done);
        thread.join().unwrap();
    }

    #[test]
    fn owned_pipe_short_report_and_bad_id_stop_link() {
        for reply in [vec![0; 16], vec![1; 17]] {
            let (mut link, done, thread) = pipe_link(reply, Duration::ZERO, false);
            assert!(link.read(&mut [0; 17], 250).is_err());
            assert!(link.failed);
            assert!(
                link.read(&mut [0; 17], 250)
                    .unwrap_err()
                    .contains("stopped")
            );
            drop(done);
            thread.join().unwrap();
        }
    }

    #[test]
    fn owned_pipe_pending_read_is_cancelled_and_drained() {
        let (mut link, done, thread) = pipe_link(Vec::new(), Duration::ZERO, false);
        assert!(
            link.read(&mut [0; 17], 10)
                .unwrap_err()
                .contains("cancellation completed")
        );
        assert!(link.failed);
        drop(done);
        thread.join().unwrap();
    }

    #[test]
    fn detail_buffer_alignment_offset_and_termination() {
        let path = r"\\?\hid#vid_264a&pid_233c&mi_01#stand";
        let units: Vec<_> = path.encode_utf16().chain(Some(0)).collect();
        let offset = offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
        let size = offset + units.len() * 2;
        let mut bytes = vec![0u64; size.div_ceil(8)];
        let raw = bytes.as_mut_ptr().cast::<u8>();
        for (i, unit) in units.iter().enumerate() {
            unsafe {
                std::ptr::write_unaligned(raw.add(offset + i * 2).cast::<u16>(), *unit);
            }
        }
        assert_eq!(detail_path(&bytes, size).unwrap(), path);
        assert!(detail_path(&bytes, size - 2).is_err());
        assert!(detail_path(&bytes, usize::MAX).is_err());
        assert!(detail_path(&bytes, offset + 1).is_err());
    }
}
