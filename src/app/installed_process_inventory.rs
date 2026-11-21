//! Bounded local WMI process inventory for installed profile migration.
use super::{MAX_DISCOVERY, RunningEntry};
use chrono::{FixedOffset, NaiveDateTime};
use std::{
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::System::{
        Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoSetProxyBlanket, CoUninitialize, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
        },
        Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE},
        Variant::{VARIANT, VT_BSTR},
        Wmi::{
            IWbemClassObject, IWbemLocator, WBEM_FLAG_CONNECT_USE_MAX_WAIT, WBEM_FLAG_FORWARD_ONLY,
            WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_S_FALSE, WBEM_S_NO_ERROR, WbemLocator,
        },
    },
    core::{BSTR, IUnknown, PCWSTR, Param, w},
};

const TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PROCESSES: usize = 24;
static WMI_WORKER: Mutex<()> = Mutex::new(());

pub(super) fn query() -> Result<Vec<RunningEntry>, String> {
    let deadline = Instant::now() + TIMEOUT;
    let (send, receive) = mpsc::sync_channel(1);
    // Connecting can take longer than an enumerator poll. Keep that COM call off
    // the UI thread and allow only one outstanding query if a provider stalls.
    thread::Builder::new()
        .name("legacy-process-inventory".into())
        .spawn(move || {
            let result = match WMI_WORKER.try_lock() {
                Ok(_worker) => collect(deadline),
                Err(_) => Err("Prismestra process inventory is already running".into()),
            };
            let _ = send.send(result);
        })
        .map_err(|error| error.to_string())?;
    receive
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| "Prismestra process inventory timed out or stopped".to_owned())?
}

struct Apartment;

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn secure_proxy(proxy: impl Param<IUnknown>) -> windows::core::Result<()> {
    unsafe {
        CoSetProxyBlanket(
            proxy,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            PCWSTR::null(),
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )
    }
}

fn collect(deadline: Instant) -> Result<Vec<RunningEntry>, String> {
    let err = |error: windows::core::Error| format!("Prismestra process inventory: {error}");
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }.map_err(err)?;
    let _apartment = Apartment;
    let locator: IWbemLocator =
        unsafe { CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER) }.map_err(err)?;
    let empty = BSTR::new();
    let service = unsafe {
        locator.ConnectServer(
            &BSTR::from("ROOT\\CIMV2"),
            &empty,
            &empty,
            &empty,
            WBEM_FLAG_CONNECT_USE_MAX_WAIT.0,
            &empty,
            None,
        )
    }
    .map_err(err)?;
    secure_proxy(&service).map_err(err)?;
    let rows = unsafe {
        service.ExecQuery(
            &BSTR::from("WQL"),
            &BSTR::from(
                "SELECT ProcessId, ExecutablePath, CommandLine, CreationDate FROM Win32_Process \
                 WHERE Name='Prismestra.exe' OR Name='pc-control-ui.exe'",
            ),
            WBEM_FLAG_RETURN_IMMEDIATELY | WBEM_FLAG_FORWARD_ONLY,
            None,
        )
    }
    .map_err(err)?;
    secure_proxy(&rows).map_err(err)?;
    let mut entries = Vec::new();
    let mut bytes = 0;
    for _ in 0..MAX_PROCESSES {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("Prismestra process inventory timed out".into());
        }
        let mut objects = [None];
        let mut count = 0;
        let status = unsafe { rows.Next(remaining.as_millis() as i32, &mut objects, &mut count) };
        if status.0 == WBEM_S_FALSE.0 && count == 0 {
            break;
        }
        if status.0 != WBEM_S_NO_ERROR.0 || count != 1 {
            return Err(format!(
                "Prismestra process inventory enumeration failed: {status:?}"
            ));
        }
        let object = objects[0]
            .as_ref()
            .ok_or("Prismestra process inventory returned no object")?;
        if let Some(entry) = entry(object) {
            bytes += entry.image.as_os_str().len() + entry.command.len();
            if bytes > MAX_DISCOVERY {
                return Err("Prismestra process inventory is too large".into());
            }
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn property(object: &IWbemClassObject, name: PCWSTR) -> Option<VARIANT> {
    let mut value = VARIANT::default();
    unsafe { object.Get(name, 0, &mut value, None, None) }.ok()?;
    Some(value)
}

fn text(object: &IWbemClassObject, name: PCWSTR) -> Option<String> {
    let value = property(object, name)?;
    if value.vt() != VT_BSTR {
        return None;
    }
    // The tag above selects the BSTR member; VARIANT owns and clears it.
    let string = unsafe { &*value.Anonymous.Anonymous.Anonymous.bstrVal };
    if string.is_empty() || string.contains(&0) {
        return None;
    }
    String::from_utf16(string).ok()
}

fn entry(object: &IWbemClassObject) -> Option<RunningEntry> {
    let command = text(object, w!("CommandLine"))?;
    if !command.contains("--project") || !command.contains("--state-dir") {
        return None;
    }
    Some(RunningEntry {
        image: text(object, w!("ExecutablePath"))?.into(),
        command,
        pid: u32::try_from(&property(object, w!("ProcessId"))?).ok()?,
        creation: creation_filetime(&text(object, w!("CreationDate"))?)?,
    })
}

fn creation_filetime(date: &str) -> Option<u64> {
    // A CIM datetime carries a signed offset in minutes, not HHMM.
    let [local, offset] = [date.get(..21)?, date.get(21..)?];
    if offset.len() != 4
        || !matches!(offset.as_bytes()[0], b'+' | b'-')
        || !offset.as_bytes()[1..].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let local = NaiveDateTime::parse_from_str(local, "%Y%m%d%H%M%S%.6f").ok()?;
    let offset = FixedOffset::east_opt(offset.parse::<i32>().ok()? * 60)?;
    let utc = local
        .and_local_timezone(offset)
        .single()?
        .timestamp_micros();
    let micros_since_1601 = utc.checked_add(11_644_473_600_000_000)?;
    u64::try_from(micros_since_1601).ok()?.checked_mul(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cim_creation_time_preserves_microseconds_and_signed_minute_offset() {
        let epoch = 116_444_736_000_000_000;
        assert_eq!(
            creation_filetime("19700101053000.123456+330"),
            Some(epoch + 1_234_560)
        );
        assert_eq!(
            creation_filetime("19691231203000.123456-210"),
            Some(epoch + 1_234_560)
        );
        assert_eq!(creation_filetime("19700101000000.******+000"), None);
        assert_eq!(creation_filetime("19700101000000.000000+*00"), None);
        assert_eq!(creation_filetime("16001231235959.999999+000"), None);
    }
}
