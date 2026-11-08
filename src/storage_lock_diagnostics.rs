//! Test-only, exact own-file diagnostics. Restart Manager query only: no shutdown/restart APIs.
use std::{
    os::windows::ffi::OsStrExt,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};
#[repr(C)]
struct Info {
    process: [u32; 3],
    app: [u16; 256],
    service: [u16; 64],
    kind: u32,
    status: u32,
    session: u32,
    restartable: i32,
}
const _: [(); 668] = [(); std::mem::size_of::<Info>()];
#[link(name = "rstrtmgr")]
unsafe extern "system" {
    fn RmStartSession(handle: *mut u32, flags: u32, key: *mut u16) -> u32;
    fn RmRegisterResources(
        handle: u32,
        files: u32,
        names: *const *const u16,
        processes: u32,
        ids: *const u8,
        services: u32,
        service_names: *const *const u16,
    ) -> u32;
    fn RmGetList(
        handle: u32,
        needed: *mut u32,
        count: *mut u32,
        info: *mut Info,
        reasons: *mut u32,
    ) -> u32;
    fn RmEndSession(handle: u32) -> u32;
}
struct Session(u32);
impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            RmEndSession(self.0);
        }
    }
}
fn query(path: &Path) -> serde_json::Value {
    let (mut handle, mut key) = (0, [0_u16; 33]);
    let code = unsafe { RmStartSession(&mut handle, 0, key.as_mut_ptr()) };
    if code != 0 {
        return serde_json::json!({"start_error":code});
    }
    let session = Session(handle);
    let normalized = path.to_string_lossy().replace('/', "\\");
    let path = std::ffi::OsStr::new(&normalized)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let files = [path.as_ptr()];
    let code = unsafe {
        RmRegisterResources(
            session.0,
            1,
            files.as_ptr(),
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
        )
    };
    if code != 0 {
        return serde_json::json!({"register_error":code});
    }
    let (mut needed, mut count, mut reasons) = (0, 0, 0);
    let code = unsafe {
        RmGetList(
            session.0,
            &mut needed,
            &mut count,
            std::ptr::null_mut(),
            &mut reasons,
        )
    };
    if ![0, 234].contains(&code) || needed > 32 {
        return serde_json::json!({"list_error":code,"needed":needed});
    }
    if needed == 0 {
        return serde_json::json!({"processes":[]});
    }
    let mut rows = (0..needed)
        .map(|_| unsafe { std::mem::zeroed::<Info>() })
        .collect::<Vec<_>>();
    count = needed;
    let code = unsafe {
        RmGetList(
            session.0,
            &mut needed,
            &mut count,
            rows.as_mut_ptr(),
            &mut reasons,
        )
    };
    if code != 0 {
        return serde_json::json!({"list_error":code,"needed":needed});
    }
    let processes=rows.into_iter().take(count as usize).map(|row|{
        let end=row.app.iter().position(|c|*c==0).unwrap_or(row.app.len());
        serde_json::json!({"pid":row.process[0],"application":String::from_utf16_lossy(&row.app[..end]),"kind":row.kind})
    }).collect::<Vec<_>>();
    serde_json::json!({"processes":processes})
}
fn allowed(path: &Path) -> bool {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let preview = root.join("work/app-preview-stand");
    let preview_fixture = path.file_name().is_some_and(|name| name == "journal.json")
        && path.parent().and_then(|parent| parent.parent()) == Some(preview.as_path())
        && path
            .parent()
            .and_then(|parent| parent.file_name())
            .is_some_and(|name| {
                let name = name.to_string_lossy();
                let parts = name.split('-').collect::<Vec<_>>();
                parts.len() == 2
                    && parts
                        .iter()
                        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            });
    path.starts_with(root.join("work/ite-durable"))
        && path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("roundtrip-"))
        || path.starts_with(root.join("work/lpc-durable"))
            && path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("combined-"))
        || preview_fixture
}
fn facts(path: &Path) -> serde_json::Value {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let Ok(metadata) = std::fs::metadata(path) else {
        return serde_json::json!({"metadata_unavailable":true});
    };
    let digest = (|| {
        if !metadata.is_file() || metadata.len() > 1024 * 1024 {
            return None;
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() <= 1024 * 1024).then(|| {
            Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        })
    })();
    serde_json::json!({"readonly":metadata.permissions().readonly(),"length":metadata.len(),"sha256":digest})
}
pub(super) fn record(path: &Path, staged: &Path, error: &std::io::Error) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    if !allowed(path) || !matches!(error.raw_os_error(), Some(5 | 32)) {
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let directory = root.join("work/storage-lock-diagnostics");
    if std::fs::create_dir_all(&directory).is_err() {
        return;
    }
    // Capture staged bytes before atomic_write's exact Temporary guard removes its own file.
    let value = serde_json::json!({"path":path,"staged_path":staged,"stage":"replace","os_error":error.raw_os_error(),"error":error.to_string(),"pid":std::process::id(),"unix_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis(),"destination":facts(path),"staged":facts(staged),"restart_manager":query(path),"staged_restart_manager":query(staged),"query_only":true});
    let name = format!(
        "{}-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let _ = std::fs::write(
        directory.join(name),
        serde_json::to_vec_pretty(&value).unwrap(),
    );
}

#[test]
fn exact_own_file_query_reports_a_known_open_handle_without_restart_actions() {
    use std::os::windows::fs::OpenOptionsExt;
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("work/ite-durable")
        .join(format!(
            "roundtrip-rm-{}-{}.json",
            std::process::id(),
            crate::clock::monotonic_us()
        ));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"original").unwrap();
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&path)
        .unwrap();
    assert_eq!(facts(&path)["length"], 8);
    assert!(
        facts(&path)["sha256"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64)
    );
    let found = query(&path);
    assert!(
        found["processes"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["pid"] == std::process::id())),
        "{found}"
    );
    assert!(crate::storage::atomic_write(&path, b"next", 32).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"original");
    drop(held);
}

#[test]
fn preview_diagnostics_only_accept_exact_numeric_own_fixture_paths() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(allowed(
        &root.join("work/app-preview-stand/123-4/journal.json")
    ));
    for suffix in [
        "work/app-preview-stand/journal.json",
        "work/app-preview-stand/123-4/private.json",
        "work/app-preview-stand/not-a-fixture/journal.json",
        "work/app-preview-stand/123-4/nested/journal.json",
        "work/app-preview-stand/123-/journal.json",
        "private/journal.json",
    ] {
        assert!(!allowed(&root.join(suffix)), "{suffix}");
    }
}
