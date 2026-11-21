//! The installed helper's only authority comes from the protected machine policy.
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsString, c_void},
    fs::{self, File},
    io::{self, Read},
    mem::{size_of, zeroed},
    net::{Ipv4Addr, SocketAddr},
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, PathBuf, Prefix},
    ptr::{addr_of, null, null_mut},
};
use windows_sys::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_INSUFFICIENT_BUFFER, GENERIC_ALL, GENERIC_WRITE, HANDLE,
            INVALID_HANDLE_VALUE, LUID, LocalFree,
        },
        Security::{
            ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
            Authorization::{ConvertSidToStringSidW, GetSecurityInfo, SE_FILE_OBJECT},
            CheckTokenMembership, CreateWellKnownSid, DACL_SECURITY_INFORMATION, DuplicateTokenEx,
            GetAce, GetTokenInformation, INHERIT_ONLY_ACE, OWNER_SECURITY_INFORMATION,
            PSECURITY_DESCRIPTOR, PSID, RevertToSelf, SECURITY_MAX_SID_SIZE, SecurityImpersonation,
            SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_DUPLICATE, TOKEN_IMPERSONATE,
            TOKEN_INFORMATION_CLASS, TOKEN_LINKED_TOKEN, TOKEN_MANDATORY_LABEL, TOKEN_OWNER,
            TOKEN_QUERY, TOKEN_STATISTICS, TOKEN_USER, TokenElevationType, TokenElevationTypeFull,
            TokenElevationTypeLimited, TokenImpersonation, TokenIntegrityLevel, TokenLinkedToken,
            TokenOwner, TokenSessionId, TokenStatistics, TokenUser, WinBuiltinAdministratorsSid,
            WinInteractiveSid,
        },
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CreateFileW, DELETE, FILE_APPEND_DATA,
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES,
            FILE_WRITE_DATA, FILE_WRITE_EA, GetDriveTypeW, GetFileInformationByHandle,
            OPEN_EXISTING, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
        },
        System::{
            Com::CoTaskMemFree,
            EventLog::{
                DeregisterEventSource, EVENTLOG_ERROR_TYPE, RegisterEventSourceW, ReportEventW,
            },
            SystemInformation::{GetSystemDirectoryW, GetSystemWindowsDirectoryW},
            Threading::{
                GetCurrentProcess, OpenProcessToken, PROCESS_NAME_NATIVE,
                QueryFullProcessImageNameW, SetThreadToken,
            },
        },
        UI::Shell::{
            FOLDERID_LocalAppData, FOLDERID_ProgramData, FOLDERID_ProgramFiles,
            KF_FLAG_DONT_VERIFY, SHGetKnownFolderPath,
        },
    },
    core::{GUID, w},
};

const POLICY_LIMIT: usize = 128 * 1024;
const MAX_FILES: usize = 1024;
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
const RUNTIME: &str = "components/openrgb/runtime";
const BACKEND: &str = "OpenRGB.exe";
const ADMINISTRATORS: &str = "S-1-5-32-544";
const SYSTEM: &str = "S-1-5-18";
const TRUSTED_INSTALLER: &str = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    version: u32,
    sid: String,
    endpoint: SocketAddr,
    app_sha256: String,
    runtime_sha256: BTreeMap<String, String>,
}

impl PolicyFile {
    fn validate(&self, sid: &str) -> Result<(), String> {
        let mut unique = BTreeSet::new();
        if self.version != 1
            || self.sid != sid
            || self.endpoint.ip() != Ipv4Addr::LOCALHOST
            || self.endpoint.port() == 0
            || !hash_shape(&self.app_sha256)
            || self.runtime_sha256.len() > MAX_FILES
            || !self.runtime_sha256.contains_key(BACKEND)
            || !self.runtime_sha256.iter().all(|(path, hash)| {
                valid_runtime_path(path)
                    && hash_shape(hash)
                    && unique.insert(path.to_ascii_lowercase())
            })
        {
            return Err("Invalid installed backend policy".into());
        }
        Ok(())
    }
}

fn hash_shape(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

fn valid_runtime_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|part| {
            !part.is_empty()
                && !part.ends_with('.')
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        })
}

pub(super) struct InstalledPolicy {
    pub(super) state: PathBuf,
    pub(super) outbox: PathBuf,
    pub(super) config_dir: PathBuf,
    pub(super) backend: PathBuf,
    pub(super) backend_sha256: String,
    pub(super) endpoint: SocketAddr,
    pub(super) task_name: String,
}

impl InstalledPolicy {
    pub(super) fn client(state: &Path, install_dir: &Path) -> Result<Option<Self>, String> {
        let token = process_token(
            unsafe { GetCurrentProcess() },
            TOKEN_QUERY | TOKEN_DUPLICATE,
        )?;
        if elevation_type(&token)? != TokenElevationTypeLimited {
            return Ok(None);
        }
        let sid = token_sid(&token)?;
        let full = linked_token(&token)?;
        if elevation_type(&full)? != TokenElevationTypeFull
            || integrity(&full)? != 0x3000
            || token_sid(&full)? != sid
            || session(&token)? == 0
            || session(&token)? != session(&full)?
            || !is_interactive_medium(&impersonation_token(&token).map_err(error)?)?
        {
            return Ok(None);
        }
        let expected_state = current_state()?;
        let executable = std::env::current_exe().map_err(error)?;
        if expected_state != canonical_user_state_path(state)?
            || executable
                .parent()
                .map(fs::canonicalize)
                .transpose()
                .map_err(error)?
                != Some(fs::canonicalize(install_dir).map_err(error)?)
        {
            return Ok(None);
        }
        Self::load(&sid, expected_state, &executable)
    }

    pub(super) fn helper() -> Result<(Self, HelperAuthority), String> {
        let authority = HelperAuthority::current()?;
        let state = authority
            .with_limited(current_state)?
            .map_err(|err| authority_stage("resolve state under linked limited token", err))?;
        let policy = Self::load(&authority.sid, state, &authority.executable)?
            .ok_or("Installed backend policy is missing")?;
        for directory in [&policy.config_dir, &policy.outbox] {
            visit_tree(directory, &mut 0, &mut |_, _| Ok(()))?;
        }
        authority.set_creator_owner()?;
        Ok((policy, authority))
    }

    fn load(sid: &str, state: PathBuf, executable: &Path) -> Result<Option<Self>, String> {
        let program_data = known_folder(&FOLDERID_ProgramData)?;
        let program_files = known_folder(&FOLDERID_ProgramFiles)?;
        let policy_home = program_data.join("Prismestra").join("Policies").join(sid);
        let policy_path = policy_home.join("policy.json");
        if !protected_chain(&policy_path, &program_data, true)? {
            return Ok(None);
        }
        let policy = File::open(&policy_path).map_err(error)?;
        let mut bytes = Vec::new();
        policy
            .take(POLICY_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(error)?;
        let json = super::strict_json::unique_json(&bytes, POLICY_LIMIT)?;
        let policy: PolicyFile = serde_json::from_value(json).map_err(error)?;
        policy.validate(sid)?;

        let executable = checked_disk_path(executable)?;
        let install = executable
            .parent()
            .ok_or("Missing installed application directory")?;
        if !install.starts_with(&program_files) || install == program_files {
            return Err("Installed helper must run beneath Program Files".into());
        }
        protected_chain(&executable, &program_files, false)?;
        verify_hash(&executable, &policy.app_sha256)?;
        let runtime = install.join(RUNTIME);
        protected_chain(&runtime, &program_files, false)?;
        verify_runtime(&runtime, &policy.runtime_sha256)?;

        let config_dir = policy_home.join("config");
        let outbox = policy_home.join("outbox");
        for directory in [&config_dir, &outbox] {
            if !protected_entry(directory, false, 0)? {
                return Err(
                    "Installed backend policy requires config and outbox directories".into(),
                );
            }
        }
        Ok(Some(Self {
            state,
            outbox,
            config_dir,
            backend: runtime.join(BACKEND),
            backend_sha256: policy.runtime_sha256[BACKEND].clone(),
            endpoint: policy.endpoint,
            task_name: format!("\\Prismestra-Backend-{sid}"),
        }))
    }
}

pub(super) struct HelperAuthority {
    sid: String,
    session: u32,
    logon: LUID,
    executable: PathBuf,
    native_image: OsString,
    limited: Handle,
}

impl HelperAuthority {
    fn current() -> Result<Self, String> {
        let current = process_token(
            unsafe { GetCurrentProcess() },
            TOKEN_QUERY | TOKEN_DUPLICATE,
        )
        .map_err(|err| authority_stage("open helper process token", err))?;
        if !is_high_admin(&current)
            .map_err(|err| authority_stage("validate elevated helper token", err))?
        {
            return Err(
                "Installed backend helper requires a split-token elevated administrator".into(),
            );
        }
        let sid = token_sid(&current)?;
        let session = session(&current)?;
        let linked = linked_token(&current)
            .map_err(|err| authority_stage("get helper linked token", err))?;
        let limited = impersonation_token(&linked).map_err(|failure| {
            report_linked_token_failure(&failure);
            authority_stage("duplicate helper linked token", error(failure))
        })?;
        if session == 0
            || session != self::session(&limited)?
            || token_sid(&limited)? != sid
            || !is_interactive_medium(&limited)
                .map_err(|err| authority_stage("validate helper linked token", err))?
        {
            return Err(
                "Installed backend helper requires its own interactive limited token".into(),
            );
        }
        Ok(Self {
            sid,
            session,
            logon: token_value::<TOKEN_STATISTICS>(&limited, TokenStatistics)?.AuthenticationId,
            executable: std::env::current_exe().map_err(error)?,
            native_image: native_image(unsafe { GetCurrentProcess() })?,
            limited,
        })
    }

    pub(super) fn validate_parent(&self, process: HANDLE, image: &Path) -> Result<(), String> {
        if image != self.executable || native_image(process)? != self.native_image {
            return Err(
                "Installed backend helper owner image differs from its installed executable".into(),
            );
        }
        let token = process_token(process, TOKEN_QUERY | TOKEN_DUPLICATE)
            .map_err(|err| authority_stage("open owner process token", err))?;
        let logon = token_value::<TOKEN_STATISTICS>(&token, TokenStatistics)
            .map_err(|err| authority_stage("query owner logon", err))?
            .AuthenticationId;
        if token_sid(&token)? != self.sid
            || session(&token)? != self.session
            || logon.HighPart != self.logon.HighPart
            || logon.LowPart != self.logon.LowPart
            || !is_interactive_medium(
                &impersonation_token(&token)
                    .map_err(|err| authority_stage("duplicate owner process token", error(err)))?,
            )
            .map_err(|err| authority_stage("validate owner process token", err))?
        {
            return Err(
                "Installed backend helper owner has a different user, logon or session".into(),
            );
        }
        Ok(())
    }

    pub(super) fn with_limited<T>(&self, read: impl FnOnce() -> T) -> Result<T, String> {
        if unsafe { SetThreadToken(null(), self.limited.0) } == 0 {
            return Err(error(format_args!(
                "SetThreadToken(linked limited token): {}",
                io::Error::last_os_error()
            )));
        }
        struct Revert;
        impl Drop for Revert {
            fn drop(&mut self) {
                if unsafe { RevertToSelf() } == 0 {
                    std::process::abort();
                }
            }
        }
        let _revert = Revert;
        Ok(read())
    }

    fn set_creator_owner(&self) -> Result<(), String> {
        let token = process_token(unsafe { GetCurrentProcess() }, TOKEN_ADJUST_DEFAULT)?;
        let mut sid = well_known_sid(WinBuiltinAdministratorsSid)?;
        let owner = TOKEN_OWNER {
            Owner: sid.as_mut_ptr().cast(),
        };
        if unsafe {
            SetTokenInformation(
                token.0,
                TokenOwner,
                addr_of!(owner).cast(),
                size_of::<TOKEN_OWNER>() as u32,
            )
        } == 0
        {
            return Err(error(format_args!(
                "SetTokenInformation(TokenOwner): {}",
                io::Error::last_os_error()
            )));
        }
        Ok(())
    }
}

fn native_image(process: HANDLE) -> Result<OsString, String> {
    let mut buffer = vec![0u16; 32768];
    let mut length = buffer.len() as u32;
    if unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_NATIVE,
            buffer.as_mut_ptr(),
            &mut length,
        )
    } == 0
    {
        return Err(error(io::Error::last_os_error()));
    }
    Ok(OsString::from_wide(&buffer[..length as usize]))
}

fn current_state() -> Result<PathBuf, String> {
    let local = known_folder(&FOLDERID_LocalAppData)?;
    canonical_user_state_path(&local.join("Prismestra").join("state"))
}

fn canonical_user_state_path(path: &Path) -> Result<PathBuf, String> {
    let path = checked_disk_path(path)?;
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor).map_err(error)?;
        use std::os::windows::fs::MetadataExt;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(
                "Installed backend control directory must be local and reparse-free".into(),
            );
        }
    }
    fs::canonicalize(path).map_err(error)
}

fn error(e: impl std::fmt::Display) -> String {
    format!("Installed backend authority: {e}")
}

fn authority_stage(stage: &str, error: String) -> String {
    format!(
        "Installed backend authority ({stage}): {}",
        error
            .strip_prefix("Installed backend authority: ")
            .unwrap_or(&error)
    )
}

fn known_folder(id: &GUID) -> Result<PathBuf, String> {
    let mut raw = null_mut();
    let result =
        unsafe { SHGetKnownFolderPath(id, KF_FLAG_DONT_VERIFY as u32, null_mut(), &mut raw) };
    if result < 0 || raw.is_null() {
        if !raw.is_null() {
            unsafe { CoTaskMemFree(raw.cast()) };
        }
        return Err(error(format!(
            "Windows Known Folder lookup failed: {result:#x}"
        )));
    }
    let path = unsafe { from_wide(raw) };
    unsafe { CoTaskMemFree(raw.cast()) };
    checked_disk_path(Path::new(&path))
}

unsafe fn from_wide(wide: *const u16) -> OsString {
    let mut len = 0;
    while unsafe { *wide.add(len) } != 0 {
        len += 1;
    }
    OsString::from_wide(unsafe { std::slice::from_raw_parts(wide, len) })
}

fn checked_disk_path(path: &Path) -> Result<PathBuf, String> {
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Err(error("expected a local disk path"));
    };
    let (Prefix::Disk(letter) | Prefix::VerbatimDisk(letter)) = prefix.kind() else {
        return Err(error("network and device paths are forbidden"));
    };
    let Some(Component::RootDir) = components.next() else {
        return Err(error("expected an absolute local disk path"));
    };
    let drive = [u16::from(letter), b':' as u16, b'\\' as u16, 0];
    if unsafe { GetDriveTypeW(drive.as_ptr()) } != 3 {
        return Err(error("expected a fixed local disk"));
    }
    let mut result = PathBuf::from(OsString::from_wide(&drive[..3]));
    for part in components {
        let Component::Normal(part) = part else {
            return Err(error("expected a normalized local disk path"));
        };
        let wide: Vec<_> = part.encode_wide().collect();
        if wide.iter().any(|b| matches!(*b, 0 | 58))
            || wide.last().is_some_and(|b| matches!(*b, 32 | 46))
        {
            return Err(error("filesystem aliases and streams are forbidden"));
        }
        result.push(part);
    }
    Ok(result)
}

fn protected_chain(path: &Path, system_root: &Path, optional: bool) -> Result<bool, String> {
    let path = checked_disk_path(path)?;
    let ancestors: Vec<_> = path.ancestors().collect();
    for ancestor in ancestors.into_iter().rev() {
        if optional {
            match fs::symlink_metadata(ancestor) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(e) => return Err(error(e)),
                Ok(_) => {}
            }
        }
        let system_path = system_root.starts_with(ancestor);
        let shared_data = ancestor == system_root
            && system_root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("ProgramData"));
        let exception = if shared_data {
            FILE_WRITE_DATA | FILE_APPEND_DATA | FILE_WRITE_EA | FILE_WRITE_ATTRIBUTES
        } else if ancestor.parent().is_none() {
            FILE_APPEND_DATA // Windows volume root permits creation of unrelated directories.
        } else {
            0
        };
        let directory = protected_entry(ancestor, system_path, exception)?;
        if !directory && ancestor != path {
            return Err(error("protected path ancestor is not a directory"));
        }
        // A stock ProgramData grants Users 0x116 on the directory itself.
        // Its protected Microsoft child keeps it nonempty, which forbids
        // assigning a reparse point. Neither path grants DELETE/DELETE_CHILD.
        if shared_data && !protected_entry(&ancestor.join("Microsoft"), true, 0)? {
            return Err(error("expected a protected Windows ProgramData anchor"));
        }
    }
    Ok(true)
}

fn protected_entry(path: &Path, system_path: bool, exception: u32) -> Result<bool, String> {
    let wide: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
    let file = unsafe {
        CreateFileW(
            wide.as_ptr(),
            READ_CONTROL | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if file == INVALID_HANDLE_VALUE {
        return Err(error(format!(
            "{}: {}",
            path.display(),
            io::Error::last_os_error()
        )));
    }
    let file = Handle(file);
    let mut attributes: BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
    if unsafe { GetFileInformationByHandle(file.0, &mut attributes) } == 0
        || attributes.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(error(format!("invalid protected path: {}", path.display())));
    }
    let mut owner: PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    let result = unsafe {
        GetSecurityInfo(
            file.0,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    let _descriptor = LocalAllocation(descriptor.cast());
    if result != 0 || owner.is_null() || dacl.is_null() {
        return Err(error(format!(
            "missing protected ACL on {}",
            path.display()
        )));
    }
    if !privileged_sid(&sid_string(owner)?, system_path) {
        return Err(error(format!("untrusted owner of {}", path.display())));
    }
    let dangerous = (DELETE
        | WRITE_DAC
        | WRITE_OWNER
        | FILE_DELETE_CHILD
        | FILE_WRITE_ATTRIBUTES
        | FILE_WRITE_EA
        | FILE_WRITE_DATA
        | FILE_APPEND_DATA
        | GENERIC_WRITE
        | GENERIC_ALL)
        & !exception;
    for index in 0..u32::from(unsafe { (*dacl).AceCount }) {
        let mut ace: *mut c_void = null_mut();
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
            return Err(error("unable to read protected ACL entry"));
        }
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        // System ancestors may have inheritance-only entries. Their descendants
        // are checked individually, including inheritance on private directories.
        if system_path && u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0 {
            continue;
        }
        match header.AceType {
            1 => {} // ACCESS_DENIED_ACE_TYPE
            0 if usize::from(header.AceSize) >= size_of::<ACCESS_ALLOWED_ACE>() => {
                let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
                let sid = addr_of!(ace.SidStart).cast_mut().cast();
                if ace.Mask & dangerous != 0 && !privileged_sid(&sid_string(sid)?, system_path) {
                    return Err(error(format!(
                        "unprivileged write grants on {}",
                        path.display()
                    )));
                }
            }
            _ => {
                return Err(error(format!(
                    "unsupported ACL entry on {}",
                    path.display()
                )));
            }
        }
    }
    Ok(attributes.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0)
}

pub(super) fn verify_status(path: &Path) -> Result<(), String> {
    if protected_entry(path, false, 0)? {
        return Err(error("expected an installed helper status file"));
    }
    Ok(())
}

fn privileged_sid(sid: &str, system_path: bool) -> bool {
    sid == ADMINISTRATORS || sid == SYSTEM || (system_path && sid == TRUSTED_INSTALLER)
}

fn visit_tree(
    directory: &Path,
    count: &mut usize,
    visit: &mut impl FnMut(&Path, bool) -> Result<(), String>,
) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(error)? {
        *count += 1;
        if *count > MAX_FILES {
            return Err(error("protected tree has too many entries"));
        }
        let entry = entry.map_err(error)?.path();
        let is_directory = protected_entry(&entry, false, 0)?;
        visit(&entry, is_directory)?;
        if is_directory {
            visit_tree(&entry, count, visit)?;
        }
    }
    Ok(())
}

fn verify_runtime(runtime: &Path, expected: &BTreeMap<String, String>) -> Result<(), String> {
    let mut actual = 0;
    visit_tree(runtime, &mut 0, &mut |path, directory| {
        let relative = path.strip_prefix(runtime).map_err(error)?;
        let key = relative
            .to_str()
            .ok_or("Non-Unicode installed runtime path")?
            .replace('\\', "/");
        if directory {
            let prefix = format!("{key}/");
            if !expected.keys().any(|entry| entry.starts_with(&prefix)) {
                return Err(error(format!("unexpected runtime directory: {key}")));
            }
        } else {
            let hash = expected
                .get(&key)
                .ok_or_else(|| error(format!("unexpected runtime file: {key}")))?;
            verify_hash(path, hash)?;
            actual += 1;
        }
        Ok(())
    })?;
    if actual != expected.len() {
        return Err(error("installed runtime is missing pinned files"));
    }
    Ok(())
}

fn verify_hash(path: &Path, expected: &str) -> Result<(), String> {
    let mut file = File::open(path).map_err(error)?;
    let metadata = file.metadata().map_err(error)?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return Err(error("expected a bounded installed file"));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let length = file.read(&mut buffer).map_err(error)?;
        if length == 0 {
            break;
        }
        hash.update(&buffer[..length]);
    }
    let actual: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(error(format!("SHA-256 mismatch: {}", path.display())));
    }
    Ok(())
}

pub(super) fn windows_dirs() -> Result<(PathBuf, PathBuf), String> {
    fn directory(get: unsafe extern "system" fn(*mut u16, u32) -> u32) -> Result<PathBuf, String> {
        let mut buffer = [0u16; 32768];
        let length = unsafe { get(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        if length == 0 || length >= buffer.len() {
            return Err(error(io::Error::last_os_error()));
        }
        checked_disk_path(Path::new(&OsString::from_wide(&buffer[..length])))
    }
    Ok((
        directory(GetSystemWindowsDirectoryW)?,
        directory(GetSystemDirectoryW)?,
    ))
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0) };
        }
    }
}

fn process_token(process: HANDLE, access: u32) -> Result<Handle, String> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(process, access, &mut token) } == 0 {
        return Err(error(format_args!(
            "OpenProcessToken: {}",
            io::Error::last_os_error()
        )));
    }
    Ok(Handle(token))
}

pub(super) fn process_user_sid(process: HANDLE) -> Result<String, String> {
    token_sid(&process_token(process, TOKEN_QUERY)?)
}

fn token_data(token: &Handle, class: TOKEN_INFORMATION_CLASS) -> Result<Vec<usize>, String> {
    let mut length = 0;
    if unsafe { GetTokenInformation(token.0, class, null_mut(), 0, &mut length) } == 0 {
        let failure = io::Error::last_os_error();
        if failure.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
            return Err(error(format_args!(
                "GetTokenInformation(class {class}, size): {failure}"
            )));
        }
    }
    if length == 0 || length > 65536 {
        return Err(error("invalid Windows token information size"));
    }
    let mut data = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token.0,
            class,
            data.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err(error(format_args!(
            "GetTokenInformation(class {class}, data): {}",
            io::Error::last_os_error()
        )));
    }
    Ok(data)
}

fn token_value<T: Copy>(token: &Handle, class: TOKEN_INFORMATION_CLASS) -> Result<T, String> {
    let data = token_data(token, class)?;
    if data.len() * size_of::<usize>() < size_of::<T>() {
        return Err(error("unexpected Windows token information size"));
    }
    Ok(unsafe { data.as_ptr().cast::<T>().read() })
}

fn sid_string(sid: PSID) -> Result<String, String> {
    let mut text = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 || text.is_null() {
        return Err(error(io::Error::last_os_error()));
    }
    let _text = LocalAllocation(text.cast());
    unsafe { from_wide(text) }
        .into_string()
        .map_err(|_| error("invalid SID"))
}

fn token_sid(token: &Handle) -> Result<String, String> {
    let data = token_data(token, TokenUser)?;
    sid_string(unsafe { (*data.as_ptr().cast::<TOKEN_USER>()).User.Sid })
}

fn session(token: &Handle) -> Result<u32, String> {
    token_value(token, TokenSessionId)
}

fn elevation_type(token: &Handle) -> Result<i32, String> {
    token_value(token, TokenElevationType)
}

fn integrity(token: &Handle) -> Result<u32, String> {
    let data = token_data(token, TokenIntegrityLevel)?;
    let sid = sid_string(unsafe { (*data.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid })?;
    sid.strip_prefix("S-1-16-")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| error("unexpected integrity SID"))
}

fn linked_token(token: &Handle) -> Result<Handle, String> {
    let linked = token_value::<TOKEN_LINKED_TOKEN>(token, TokenLinkedToken)?;
    Ok(Handle(linked.LinkedToken))
}

struct DuplicationError {
    failure: io::Error,
    token_type: Option<i32>,
    impersonation_level: Option<i32>,
}

impl std::fmt::Display for DuplicationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DuplicateTokenEx(SecurityImpersonation, ")?;
        match (self.token_type, self.impersonation_level) {
            (_, Some(level)) => write!(f, "source impersonation level {level}")?,
            (Some(kind), None) => write!(f, "source token type {kind}")?,
            (None, None) => f.write_str("source token type unavailable")?,
        }
        write!(f, "): {}", self.failure)
    }
}

impl DuplicationError {
    fn event_data(&self) -> String {
        let number = |value: Option<i32>| value.map_or_else(|| "unknown".into(), |n| n.to_string());
        format!(
            "stage=duplicate_helper_linked_token api=DuplicateTokenEx win32={} token_type={} impersonation_level={}",
            number(self.failure.raw_os_error()),
            number(self.token_type),
            number(self.impersonation_level),
        )
    }
}

fn report_linked_token_failure(failure: &DuplicationError) {
    // The scheduled GUI executable may start without stderr. This fixed event
    // carries numeric token metadata only and does not need a profile path.
    let source = unsafe { RegisterEventSourceW(null(), w!("Prismestra Installed Backend")) };
    if source.is_null() {
        return;
    }
    let data: Vec<u16> = failure.event_data().encode_utf16().chain([0]).collect();
    let strings = [data.as_ptr()];
    unsafe {
        ReportEventW(
            source,
            EVENTLOG_ERROR_TYPE,
            0,
            1,
            null_mut(),
            1,
            0,
            strings.as_ptr(),
            null(),
        );
        DeregisterEventSource(source);
    }
}

fn impersonation_token(token: &Handle) -> Result<Handle, DuplicationError> {
    let mut result = null_mut();
    if unsafe {
        DuplicateTokenEx(
            token.0,
            TOKEN_QUERY | TOKEN_IMPERSONATE,
            null(),
            SecurityImpersonation,
            TokenImpersonation,
            &mut result,
        )
    } == 0
    {
        let failure = io::Error::last_os_error();
        let stats = token_value::<TOKEN_STATISTICS>(token, TokenStatistics).ok();
        return Err(DuplicationError {
            failure,
            token_type: stats.map(|stats| stats.TokenType),
            impersonation_level: stats
                .filter(|stats| stats.TokenType == TokenImpersonation)
                .map(|stats| stats.ImpersonationLevel),
        });
    }
    Ok(Handle(result))
}

fn well_known_sid(
    kind: i32,
) -> Result<[u32; (SECURITY_MAX_SID_SIZE as usize).div_ceil(4)], String> {
    let mut sid = [0u32; (SECURITY_MAX_SID_SIZE as usize).div_ceil(4)];
    let mut size = SECURITY_MAX_SID_SIZE;
    if unsafe { CreateWellKnownSid(kind, null_mut(), sid.as_mut_ptr().cast(), &mut size) } == 0 {
        return Err(error(io::Error::last_os_error()));
    }
    Ok(sid)
}

fn member(token: &Handle, kind: i32) -> Result<bool, String> {
    let mut sid = well_known_sid(kind)?;
    let mut result = 0;
    if unsafe { CheckTokenMembership(token.0, sid.as_mut_ptr().cast(), &mut result) } == 0 {
        return Err(error(format_args!(
            "CheckTokenMembership(well-known SID type {kind}): {}",
            io::Error::last_os_error()
        )));
    }
    Ok(result != 0)
}

fn is_high_admin(token: &Handle) -> Result<bool, String> {
    Ok(elevation_type(token)? == TokenElevationTypeFull
        && integrity(token)? == 0x3000
        && session(token)? != 0
        && member(
            &impersonation_token(token).map_err(error)?,
            WinBuiltinAdministratorsSid,
        )?)
}

fn is_interactive_medium(token: &Handle) -> Result<bool, String> {
    Ok(elevation_type(token)? == TokenElevationTypeLimited
        && (0x2000..0x3000).contains(&integrity(token)?)
        && member(token, WinInteractiveSid)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Security::SecurityIdentification;

    #[test]
    fn identification_token_cannot_be_used_for_limited_file_access() {
        let token = process_token(
            unsafe { GetCurrentProcess() },
            TOKEN_QUERY | TOKEN_DUPLICATE,
        )
        .unwrap();
        let mut identification = null_mut();
        assert_ne!(
            unsafe {
                DuplicateTokenEx(
                    token.0,
                    TOKEN_QUERY | TOKEN_DUPLICATE,
                    null(),
                    SecurityIdentification,
                    TokenImpersonation,
                    &mut identification,
                )
            },
            0,
            "{}",
            io::Error::last_os_error()
        );
        let failure = impersonation_token(&Handle(identification)).err().unwrap();
        assert!(
            failure
                .to_string()
                .contains("DuplicateTokenEx(SecurityImpersonation, source impersonation level 1)")
        );
        assert_eq!(failure.failure.raw_os_error(), Some(1346));
        assert_eq!(
            failure.event_data(),
            "stage=duplicate_helper_linked_token api=DuplicateTokenEx win32=1346 token_type=2 impersonation_level=1"
        );
    }

    #[test]
    fn policy_accepts_only_exact_sid_ipv4_loopback_and_unambiguous_runtime_pins() {
        let mut file = PolicyFile {
            version: 1,
            sid: "S-1-5-21-42".into(),
            endpoint: "127.0.0.1:6742".parse().unwrap(),
            app_sha256: "a".repeat(64),
            runtime_sha256: [(BACKEND.into(), "b".repeat(64))].into(),
        };
        assert!(file.validate("S-1-5-21-42").is_ok());
        assert!(file.validate("S-1-5-21-43").is_err());
        file.endpoint = "[::1]:6742".parse().unwrap();
        assert!(file.validate(&file.sid).is_err());
        file.endpoint = "127.0.0.1:6742".parse().unwrap();
        file.runtime_sha256
            .insert("openrgb.exe".into(), "c".repeat(64));
        assert!(file.validate(&file.sid).is_err());
        for name in [
            "../Qt.dll",
            "/Qt.dll",
            "Qt.dll:stream",
            "plugins\\Qt.dll",
            "Qt.dll.",
        ] {
            assert!(!valid_runtime_path(name), "{name}");
        }
        assert!(valid_runtime_path("plugins/platforms/qwindows.dll"));
    }

    #[test]
    fn windows_ancestor_rights_are_never_granted_to_ordinary_owners() {
        assert!(privileged_sid(SYSTEM, false));
        assert!(privileged_sid(ADMINISTRATORS, false));
        assert!(privileged_sid(TRUSTED_INSTALLER, true));
        assert!(!privileged_sid(TRUSTED_INSTALLER, false));
        assert!(!privileged_sid("S-1-5-32-545", true));
    }

    #[test]
    fn windows_known_folders_are_resolved_and_checked_read_only() {
        let paths = [FOLDERID_ProgramData, FOLDERID_ProgramFiles];
        for id in paths {
            let path = known_folder(&id).unwrap();
            // Managed policy must reject unfamiliar system ACLs. Read-only test
            // never attempts to normalize a customized Windows installation.
            assert!(path.is_dir());
            assert!(checked_disk_path(&path).is_ok());
            assert!(protected_chain(&path, &path, false).unwrap());
        }
    }
}
