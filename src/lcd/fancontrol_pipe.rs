//! Optional cached-temperature interoperability, not an own hardware sensor provider.
//! No caller in the application invokes this adapter yet. Tests contact only owned fake pipes.
use super::{fancontrol_wire as wire, temperatures::Reading};
use std::{
    path::{Component, Path, PathBuf, Prefix},
    ptr::{null, null_mut},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, ReadFile, SECURITY_IDENTIFICATION,
        SECURITY_SQOS_PRESENT, WriteFile,
    },
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        Pipes::*,
        Threading::{
            CreateEventW, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW, WaitForSingleObject,
        },
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
fn handle(raw: HANDLE) -> Result<Handle, String> {
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(Handle(raw))
    }
}
fn error(operation: &str) -> String {
    format!("{operation}: {}", std::io::Error::last_os_error())
}
fn wide(value: &std::ffi::OsStr) -> Result<Vec<u16>, String> {
    use std::os::windows::ffi::OsStrExt;
    let mut value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err("NUL in Windows path".into());
    }
    value.push(0);
    Ok(value)
}
fn local_file(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || !matches!(path.components().next(),Some(Component::Prefix(prefix))
        if matches!(prefix.kind(),Prefix::Disk(_)|Prefix::VerbatimDisk(_)))
    {
        return Err("Expected server executable must be on a local drive".into());
    }
    Ok(())
}
pub struct Policy {
    name: Vec<u16>,
    expected_server: PathBuf,
    timeout_ms: u32,
}
impl Policy {
    /// A local pipe and explicit executable identity. This grants no configuration RPC access.
    pub fn new(pipe_name: &str, expected_server: PathBuf, timeout_ms: u32) -> Result<Self, String> {
        let tail = pipe_name
            .strip_prefix(r"\\.\pipe\")
            .ok_or("Only local named pipes are allowed")?;
        if tail.is_empty()
            || tail.contains(['\\', '/', '\0'])
            || pipe_name.encode_utf16().count() > 256
            || !(1..=2500).contains(&timeout_ms)
        {
            return Err("Invalid local FanControl pipe policy".into());
        }
        local_file(&expected_server)?;
        Ok(Self {
            name: wide(std::ffi::OsStr::new(pipe_name))?,
            expected_server,
            timeout_ms,
        })
    }
}
struct Connection {
    pipe: Handle,
    start: Instant,
    timeout_ms: u32,
    total: usize,
    reads: usize,
}
impl Connection {
    fn remaining(&self) -> Result<u32, String> {
        let elapsed = self.start.elapsed().as_millis();
        if elapsed >= u128::from(self.timeout_ms) {
            return Err("FanControl IPC timeout".into());
        }
        Ok(self.timeout_ms - elapsed as u32)
    }
    fn verify(&self, expected: &Path) -> Result<(), String> {
        let mut pid = 0;
        if unsafe { GetNamedPipeServerProcessId(self.pipe.0, &mut pid) } == 0 || pid == 0 {
            return Err(error("Identify pipe server"));
        }
        let process = handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })?;
        let mut image = vec![0u16; 32768];
        let mut length = image.len() as u32;
        if unsafe { QueryFullProcessImageNameW(process.0, 0, image.as_mut_ptr(), &mut length) } == 0
        {
            return Err(error("Read pipe server executable"));
        }
        use std::os::windows::ffi::OsStringExt;
        let actual = PathBuf::from(std::ffi::OsString::from_wide(&image[..length as usize]));
        local_file(&actual)?;
        let actual = actual.canonicalize().map_err(|e| e.to_string())?;
        if actual != expected {
            return Err("FanControl pipe belongs to an unexpected executable".into());
        }
        let mut flags = 0;
        if unsafe { GetNamedPipeInfo(self.pipe.0, &mut flags, null_mut(), null_mut(), null_mut()) }
            == 0
        {
            return Err(error("Read pipe type"));
        }
        if flags & PIPE_TYPE_MESSAGE == 0 {
            return Err("Message-mode FanControl pipe required".into());
        }
        let mode = PIPE_READMODE_MESSAGE;
        if unsafe { SetNamedPipeHandleState(self.pipe.0, &mode, null(), null()) } == 0 {
            return Err(error("Set pipe read mode"));
        }
        Ok(())
    }
    fn transfer(&self, bytes: &mut [u8], write: bool) -> Result<(usize, bool), String> {
        let timeout = self.remaining()?;
        let event = handle(unsafe { CreateEventW(null(), 1, 0, null()) })?;
        // OVERLAPPED is a Windows POD whose initial state is zero except hEvent.
        let mut op: OVERLAPPED = unsafe { std::mem::zeroed() };
        op.hEvent = event.0;
        let mut count = 0;
        let started = unsafe {
            if write {
                WriteFile(
                    self.pipe.0,
                    bytes.as_ptr().cast(),
                    bytes.len() as u32,
                    &mut count,
                    &mut op,
                )
            } else {
                ReadFile(
                    self.pipe.0,
                    bytes.as_mut_ptr().cast(),
                    bytes.len() as u32,
                    &mut count,
                    &mut op,
                )
            }
        };
        let status = if started != 0 {
            ERROR_SUCCESS
        } else {
            unsafe { GetLastError() }
        };
        if ![ERROR_SUCCESS, ERROR_MORE_DATA, ERROR_IO_PENDING].contains(&status) {
            return Err(error("Start FanControl pipe I/O"));
        }
        if status == ERROR_IO_PENDING
            && unsafe { WaitForSingleObject(event.0, timeout) } != WAIT_OBJECT_0
        {
            // Buffer/event/OVERLAPPED remain alive through cancellation completion.
            unsafe {
                CancelIoEx(self.pipe.0, &op);
                GetOverlappedResult(self.pipe.0, &op, &mut count, 1);
            }
            return Err("FanControl IPC timeout; pending I/O cancelled".into());
        }
        let complete = unsafe { GetOverlappedResult(self.pipe.0, &op, &mut count, 0) };
        let status = if complete != 0 {
            ERROR_SUCCESS
        } else {
            unsafe { GetLastError() }
        };
        if ![ERROR_SUCCESS, ERROR_MORE_DATA].contains(&status) {
            return Err(error("Complete FanControl pipe I/O"));
        }
        if count as usize > bytes.len()
            || (!write && count == 0)
            || (write && (status != ERROR_SUCCESS || count as usize != bytes.len()))
        {
            return Err("Invalid/short FanControl pipe I/O".into());
        }
        Ok((count as usize, status == ERROR_MORE_DATA))
    }
    fn read_message(&mut self) -> Result<Vec<u8>, String> {
        let mut packet = Vec::new();
        loop {
            self.reads += 1;
            if self.reads > 256 {
                return Err("Too many FanControl pipe reads".into());
            }
            let mut chunk = [0u8; 16384];
            let (count, more) = self.transfer(&mut chunk, false)?;
            self.total += count;
            if self.total > wire::MAX_TOTAL_BYTES || packet.len() + count > wire::MAX_MESSAGE_BYTES
            {
                return Err("Oversized FanControl pipe reply".into());
            }
            packet.extend_from_slice(&chunk[..count]);
            if !more {
                return Ok(packet);
            }
        }
    }
}
/// The sole RPC is GetAllSensors. The nominal I/O budget includes connection/metadata.
/// Cancellation completion can take longer; this is not an absolute wall-clock guarantee.
pub fn read_temperatures(policy: &Policy) -> Result<Vec<Reading>, String> {
    let start = Instant::now();
    let expected = policy
        .expected_server
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let pipe = loop {
        let left = u128::from(policy.timeout_ms)
            .checked_sub(start.elapsed().as_millis())
            .filter(|t| *t > 0)
            .ok_or("FanControl pipe connection timeout")? as u32;
        let raw = unsafe {
            CreateFileW(
                policy.name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                null_mut(),
            )
        };
        if raw != INVALID_HANDLE_VALUE {
            break handle(raw)?;
        }
        if unsafe { GetLastError() } != ERROR_PIPE_BUSY {
            return Err(error("Open local FanControl pipe"));
        }
        if unsafe { WaitNamedPipeW(policy.name.as_ptr(), left.min(100)) } == 0
            && unsafe { GetLastError() } != ERROR_SEM_TIMEOUT
        {
            return Err(error("Wait for FanControl pipe"));
        }
    };
    let mut connection = Connection {
        pipe,
        start,
        timeout_ms: policy.timeout_ms,
        total: 0,
        reads: 0,
    };
    connection.verify(&expected)?;
    let utc_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis();
    let deadline = i64::try_from(utc_ms + u128::from(connection.remaining()?))
        .map_err(|_| "RPC deadline overflow")?;
    let mut outgoing = wire::request(deadline)?;
    connection.transfer(&mut outgoing, true)?;
    let readings = wire::parse_reply(|| connection.read_message())?;
    connection.remaining()?;
    Ok(readings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    };
    use std::time::Duration;
    use windows_sys::Win32::Storage::FileSystem::{FlushFileBuffers, PIPE_ACCESS_DUPLEX};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn empty_reply(padding: usize) -> Vec<u8> {
        fn varint(mut n: usize) -> Vec<u8> {
            let mut b = Vec::new();
            loop {
                let part = (n & 127) as u8;
                n >>= 7;
                b.push(part | if n != 0 { 128 } else { 0 });
                if n == 0 {
                    return b;
                }
            }
        }
        let mut body = vec![0x1a, 0];
        if padding > 0 {
            body.extend(varint((99 << 3) | 2));
            body.extend(varint(padding));
            body.resize(body.len() + padding, 0);
        }
        let mut bytes = varint(body.len());
        bytes.extend(body);
        bytes.extend([2, 0x2a, 0]);
        bytes
    }
    fn server(
        reply: Vec<u8>,
        delay: Duration,
    ) -> (String, Arc<AtomicU64>, std::thread::JoinHandle<()>) {
        server_kind(reply, delay, true)
    }
    fn server_kind(
        reply: Vec<u8>,
        delay: Duration,
        message_mode: bool,
    ) -> (String, Arc<AtomicU64>, std::thread::JoinHandle<()>) {
        let name = format!(
            r"\\.\pipe\pc-control-stand-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let count = Arc::new(AtomicU64::new(0));
        let observed = count.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let n = name.clone();
        let thread = std::thread::spawn(move || {
            let name = wide(std::ffi::OsStr::new(&n)).unwrap();
            let pipe = handle(unsafe {
                CreateNamedPipeW(
                    name.as_ptr(),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                    (if message_mode {
                        PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE
                    } else {
                        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE
                    }) | PIPE_WAIT
                        | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    65536,
                    65536,
                    3000,
                    null(),
                )
            })
            .unwrap();
            let event = handle(unsafe { CreateEventW(null(), 1, 0, null()) }).unwrap();
            let mut op: OVERLAPPED = unsafe { std::mem::zeroed() };
            op.hEvent = event.0;
            let connected = unsafe { ConnectNamedPipe(pipe.0, &mut op) };
            let status = if connected != 0 {
                ERROR_SUCCESS
            } else {
                unsafe { GetLastError() }
            };
            ready_tx.send(()).unwrap();
            if status == ERROR_IO_PENDING {
                if unsafe { WaitForSingleObject(event.0, 3000) } != WAIT_OBJECT_0 {
                    let mut bytes = 0;
                    unsafe {
                        CancelIoEx(pipe.0, &op);
                        GetOverlappedResult(pipe.0, &op, &mut bytes, 1);
                    }
                    return;
                }
            } else if status != ERROR_SUCCESS && status != ERROR_PIPE_CONNECTED {
                return;
            }
            let connection = Connection {
                pipe,
                start: Instant::now(),
                timeout_ms: 2500,
                total: 0,
                reads: 0,
            };
            let mut request = [0u8; 4096];
            let Ok((size, _)) = connection.transfer(&mut request, false) else {
                return;
            };
            assert!(
                request[..size]
                    .windows(wire::METHOD.len())
                    .any(|part| part == wire::METHOD.as_bytes())
            );
            observed.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(delay);
            let mut reply = reply;
            if connection.transfer(&mut reply, true).is_ok() {
                unsafe {
                    FlushFileBuffers(connection.pipe.0);
                }
            }
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        (name, count, thread)
    }
    #[test]
    fn own_server_executable_verified_and_message_chunks_reassembled() {
        let (name, count, thread) = server(empty_reply(20000), Duration::ZERO);
        let policy = Policy::new(&name, std::env::current_exe().unwrap(), 2500).unwrap();
        let result = read_temperatures(&policy);
        thread.join().unwrap();
        assert_eq!(result.unwrap(), Vec::new());
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn unexpected_executable_gets_no_rpc_write() {
        let (name, count, thread) = server(empty_reply(0), Duration::ZERO);
        let expected =
            PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe");
        let policy = Policy::new(&name, expected, 2500).unwrap();
        let result = read_temperatures(&policy);
        thread.join().unwrap();
        assert!(result.unwrap_err().contains("unexpected executable"));
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn timeout_cancels_and_drains_owned_pending_read() {
        let (name, count, thread) = server(empty_reply(0), Duration::from_millis(180));
        let policy = Policy::new(&name, std::env::current_exe().unwrap(), 100).unwrap();
        let result = read_temperatures(&policy);
        thread.join().unwrap();
        assert!(result.unwrap_err().contains("timeout"));
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn remote_pipe_and_remote_server_path_are_rejected_before_io() {
        assert!(
            Policy::new(
                r"\\remote\pipe\FanControl",
                std::env::current_exe().unwrap(),
                2500
            )
            .is_err()
        );
        assert!(
            Policy::new(
                r"\\.\pipe\FanControl",
                PathBuf::from(r"\\remote\server.exe"),
                2500
            )
            .is_err()
        );
    }
    #[test]
    fn byte_mode_is_rejected_before_rpc_write() {
        let (name, count, thread) = server_kind(empty_reply(0), Duration::ZERO, false);
        let policy = Policy::new(&name, std::env::current_exe().unwrap(), 2500).unwrap();
        let result = read_temperatures(&policy);
        thread.join().unwrap();
        assert!(result.unwrap_err().contains("Message-mode"));
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn actual_pipe_returns_typed_zero_temperature_without_inventing_missing_values() {
        fn blob(field: u8, value: &[u8]) -> Vec<u8> {
            assert!(value.len() < 128);
            let mut out = vec![(field << 3) | 2, value.len() as u8];
            out.extend(value);
            out
        }
        let mut sensor = blob(1, b"fixture/cpu/temp0");
        sensor.extend([16, 3]);
        sensor.extend(blob(3, b"CPU"));
        sensor.push(45);
        sensor.extend(0f32.to_le_bytes());
        let payload = blob(1, &sensor);
        let info = vec![8, payload.len() as u8, 16, 1];
        let transport = blob(3, &info);
        let mut reply = vec![transport.len() as u8];
        reply.extend(transport);
        reply.extend(payload);
        reply.extend([2, 0x2a, 0]);
        let (name, _, thread) = server(reply, Duration::ZERO);
        let policy = Policy::new(&name, std::env::current_exe().unwrap(), 2500).unwrap();
        let values = read_temperatures(&policy);
        thread.join().unwrap();
        let values = values.unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].id, "fixture/cpu/temp0");
        assert_eq!(values[0].celsius, 0.);
    }
}
