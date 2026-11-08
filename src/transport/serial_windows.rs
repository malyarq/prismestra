//! Dormant native serial adapter: exact USB/COM binding, exclusive ownership,
//! only identification/RGB payloads. No test opens a physical port.
use super::serial::{self, Backend, Descriptor, Link};
use std::{
    mem::size_of,
    ptr::{null, null_mut},
};
use windows_sys::{
    Win32::{
        Devices::{Communication::*, DeviceAndDriverInstallation::*},
        Foundation::*,
        Storage::FileSystem::*,
        System::{
            IO::*,
            Registry::*,
            Threading::{CreateEventW, WaitForSingleObject},
        },
    },
    core::GUID,
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
        Err(error("Open serial handle"))
    } else {
        Ok(Handle(raw))
    }
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn text(s: &[u16]) -> Result<String, String> {
    let end = s
        .iter()
        .position(|x| *x == 0)
        .ok_or("Unterminated serial metadata")?;
    String::from_utf16(&s[..end]).map_err(|_| "Invalid serial metadata".into())
}
struct Devices(HDEVINFO);
impl Drop for Devices {
    fn drop(&mut self) {
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}
struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
const PORTS: GUID = GUID::from_u128(0x4d36e978_e325_11ce_bfc1_08002be10318);

/// Metadata-only discovery of present CH340 ports; no Moni-A probes.
pub fn descriptors() -> Result<Vec<Descriptor>, String> {
    let raw = unsafe { SetupDiGetClassDevsW(&PORTS, null(), null_mut(), DIGCF_PRESENT) };
    if raw == -1 {
        return Err(error("Enumerate COM devices"));
    }
    let list = Devices(raw);
    let mut result = Vec::new();
    for index in 0..4096 {
        let mut info = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        if unsafe { SetupDiEnumDeviceInfo(list.0, index, &mut info) } == 0 {
            if unsafe { GetLastError() } == ERROR_NO_MORE_ITEMS {
                return Ok(result);
            }
            return Err(error("Read COM device info"));
        }
        let mut ids = [0u16; 2048];
        let mut kind = 0;
        let mut needed = 0;
        if unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                list.0,
                &info,
                SPDRP_HARDWAREID,
                &mut kind,
                ids.as_mut_ptr().cast(),
                4096,
                &mut needed,
            )
        } == 0
        {
            if unsafe { GetLastError() } == ERROR_INVALID_DATA {
                continue;
            }
            return Err(error("Read COM hardware IDs"));
        }
        if kind != REG_MULTI_SZ || !(4..=4096).contains(&needed) || needed % 2 != 0 {
            return Err("Invalid COM hardware-ID metadata".into());
        }
        let units = &ids[..needed as usize / 2];
        if units[units.len() - 2..] != [0, 0] {
            return Err("Unterminated COM hardware IDs".into());
        }
        let matching = units
            .split(|x| *x == 0)
            .filter(|s| !s.is_empty())
            .map(String::from_utf16)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "Invalid COM hardware-ID UTF16")?
            .iter()
            .any(|id| serial::valid_usb_id(id));
        if !matching {
            continue;
        }
        let mut instance = [0u16; 1024];
        if unsafe {
            SetupDiGetDeviceInstanceIdW(list.0, &info, instance.as_mut_ptr(), 1024, &mut needed)
        } == 0
        {
            return Err(error("Read COM USB instance"));
        }
        let instance_id = text(&instance)?;
        if !serial::valid_usb_id(&instance_id) {
            return Err("COM instance does not match CH340 hardware ID".into());
        }
        let raw = unsafe {
            SetupDiOpenDevRegKey(
                list.0,
                &info,
                DICS_FLAG_GLOBAL,
                0,
                DIREG_DEV,
                KEY_QUERY_VALUE,
            )
        };
        if raw.is_null() || raw as isize == -1 {
            return Err(error("Open COM metadata key"));
        }
        let key = Key(raw);
        let mut port = [0u16; 128];
        let mut bytes = 256;
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                wide("PortName").as_ptr(),
                null(),
                &mut kind,
                port.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(format!("Read COM port name: Windows error{status}"));
        }
        if kind != REG_SZ || !(2..=256).contains(&bytes) || !bytes.is_multiple_of(2) {
            return Err("Invalid COM port-name metadata".into());
        }
        let port = text(&port[..bytes as usize / 2])?;
        serial::port_path(&port)?;
        result.push(Descriptor { port, instance_id });
    }
    Err("COM enumeration exceeds limit".into())
}

pub struct WindowsBackend;
pub struct WindowsLink {
    handle: Handle,
    failed: bool,
    /// Exact remaining bytes after a short write; arbitrary continuations are rejected.
    continuation: Vec<u8>,
}
impl Backend for WindowsBackend {
    type Link = WindowsLink;
    fn descriptors(&mut self) -> Result<Vec<Descriptor>, String> {
        descriptors()
    }
    fn open_exclusive(&mut self, descriptor: &Descriptor) -> Result<WindowsLink, String> {
        let matches: Vec<_> = descriptors()?
            .into_iter()
            .filter(|p| p.port == descriptor.port)
            .collect();
        if matches.len() != 1 || matches[0] != *descriptor {
            return Err("COM target changed before open".into());
        }
        let path = wide(&serial::port_path(&descriptor.port)?);
        let handle = handle(unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        })?;
        let mut dcb = DCB {
            DCBlength: size_of::<DCB>() as u32,
            ..Default::default()
        };
        if unsafe { GetCommState(handle.0, &mut dcb) } == 0 {
            return Err(error("Read serial state"));
        }
        // 115200 8N1, binary mode, no flow control, DTR/RTS disabled, as source.
        dcb.BaudRate = serial::BAUD as u32;
        dcb.ByteSize = 8;
        dcb.Parity = NOPARITY;
        dcb.StopBits = ONESTOPBIT;
        dcb._bitfield = 1;
        if unsafe { SetCommState(handle.0, &dcb) } == 0 {
            return Err(error("Set serial state"));
        }
        let timeouts = COMMTIMEOUTS {
            ReadIntervalTimeout: u32::MAX,
            ReadTotalTimeoutMultiplier: u32::MAX,
            ReadTotalTimeoutConstant: 100,
            WriteTotalTimeoutMultiplier: 0,
            WriteTotalTimeoutConstant: 200,
        };
        if unsafe { SetCommTimeouts(handle.0, &timeouts) } == 0 {
            return Err(error("Set serial timeouts"));
        }
        Ok(WindowsLink {
            handle,
            failed: false,
            continuation: Vec::new(),
        })
    }
}
fn allowed(bytes: &[u8]) -> bool {
    if bytes == serial::QUERY {
        return true;
    }
    bytes.len() >= 9
        && bytes.len() <= 3078
        && &bytes[..4] == b"Ada\0"
        && bytes.len() == 6 + (usize::from(u16::from_be_bytes([bytes[4], bytes[5]])) + 1) * 3
}
impl WindowsLink {
    fn transfer(&mut self, bytes: &mut [u8], write: bool, timeout: u32) -> Result<usize, String> {
        if self.failed {
            return Err("Serial link stopped after I/O error".into());
        }
        if !(1..=250).contains(&timeout)
            || bytes.is_empty()
            || bytes.len() > 3078
            || (!write && bytes.len() > 64)
        {
            return Err("Invalid serial I/O bounds".into());
        }
        let result = (|| {
            let event = handle(unsafe { CreateEventW(null(), 1, 0, null()) })?;
            let mut op = OVERLAPPED {
                hEvent: event.0,
                ..Default::default()
            };
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
            let mut count = 0;
            if started == 0 {
                if unsafe { GetLastError() } != ERROR_IO_PENDING {
                    return Err(error("Start serial I/O"));
                }
                if unsafe { WaitForSingleObject(event.0, timeout) } != WAIT_OBJECT_0 {
                    unsafe {
                        CancelIoEx(self.handle.0, &op);
                        GetOverlappedResult(self.handle.0, &op, &mut count, 1);
                    }
                    return Err("Serial I/O timeout; cancellation drained".into());
                }
            }
            if unsafe { GetOverlappedResult(self.handle.0, &op, &mut count, 0) } == 0 {
                return Err(error("Complete serial I/O"));
            }
            if count as usize > bytes.len() {
                return Err("Invalid serial completion length".into());
            }
            Ok(count as usize)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}
impl Link for WindowsLink {
    fn now_us(&self) -> u64 {
        crate::clock::monotonic_us()
    }
    fn flush_rx(&mut self) -> Result<(), String> {
        if self.failed {
            return Err("Serial link stopped".into());
        }
        if unsafe { PurgeComm(self.handle.0, PURGE_RXABORT | PURGE_RXCLEAR) } == 0 {
            self.failed = true;
            return Err(error("Flush serial input"));
        }
        Ok(())
    }
    fn write(&mut self, bytes: &[u8], timeout: u32) -> Result<usize, String> {
        if !(if self.continuation.is_empty() {
            allowed(bytes)
        } else {
            bytes == self.continuation
        }) {
            return Err("Serial payload is not identification/RGB/known continuation".into());
        }
        let mut copy = bytes.to_vec();
        let count = self.transfer(&mut copy, true, timeout)?;
        self.continuation = bytes[count..].to_vec();
        Ok(count)
    }
    fn read(&mut self, bytes: &mut [u8], timeout: u32) -> Result<usize, String> {
        self.transfer(bytes, false, timeout)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    #[test]
    fn whitelist_rejects_empty_and_inconsistent_frames() {
        assert!(allowed(serial::QUERY));
        assert!(allowed(&[65, 100, 97, 0, 0, 0, 1, 2, 3]));
        for bytes in [
            &b""[..],
            &b"Ada\0\0\0"[..],
            &b"Moni-B"[..],
            &[65, 100, 97, 0, 0, 2, 1, 2, 3][..],
        ] {
            assert!(!allowed(bytes));
        }
    }
    #[test]
    fn overlapped_transfer_uses_only_an_owned_delete_on_close_file() {
        let path = std::env::temp_dir().join(format!(
            "pc-control-serial-stand-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let handle = handle(unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                CREATE_NEW,
                FILE_FLAG_OVERLAPPED | FILE_FLAG_DELETE_ON_CLOSE,
                null_mut(),
            )
        })
        .unwrap();
        let mut link = WindowsLink {
            handle,
            failed: false,
            continuation: Vec::new(),
        };
        assert!(link.write(b"OTHER!", 250).is_err());
        assert_eq!(link.write(serial::QUERY, 250).unwrap(), 6);
        let mut read = [0; 6];
        assert_eq!(link.read(&mut read, 250).unwrap(), 6);
        assert_eq!(&read, serial::QUERY);
        assert!(!link.failed);
        drop(link);
        assert!(!path.exists());
    }
}
