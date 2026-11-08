//! Dormant, owner-thread-bound System32 NVAPI context. No work occurs on import.
//! Base discovery resolves Initialize, physical enumeration and full PCI identity
//! only. Explicit adapters own their additional interface allowlists.
use crate::sensors::nvidia::PciIdentity;
use std::{
    ffi::{OsString, c_void},
    marker::PhantomData,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::MetadataExt,
    },
    path::{Path, PathBuf},
    ptr::null_mut,
    rc::Rc,
    thread::ThreadId,
};
use windows_sys::Win32::{
    Foundation::{FreeLibrary, HMODULE},
    System::LibraryLoader::{
        GetModuleFileNameW, GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    },
};
const MAX_GPUS: usize = 64;
pub(crate) type Handle = *mut c_void;
pub(crate) type QueryInterface = unsafe extern "C" fn(u32) -> *const c_void;
type Initialize = unsafe extern "C" fn() -> i32;
type Enumerate = unsafe extern "C" fn(*mut Handle, *mut i32) -> i32;
type Pci = unsafe extern "C" fn(Handle, *mut u32, *mut u32, *mut u32, *mut u32) -> i32;
type PhysicalIdentities = Vec<(Handle, PciIdentity)>;
struct Library(HMODULE);
impl Drop for Library {
    fn drop(&mut self) {
        unsafe {
            FreeLibrary(self.0);
        }
    }
}

fn status(code: i32, operation: &str) -> Result<(), String> {
    if code != 0 {
        return Err(format!("{operation} failed with NVAPI status {code}"));
    }
    Ok(())
}
fn ordinary(path: &Path, directory: bool) -> Result<(), String> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|e| format!("NVAPI path metadata: {e}"))?;
    if metadata.file_type().is_symlink()
        || metadata.file_attributes() & 0x400 != 0
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err("NVAPI requires a regular System32 DLL without reparse points".into());
    }
    Ok(())
}
fn system_dll() -> Result<PathBuf, String> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemDirectoryW(buffer: *mut u16, capacity: u32) -> u32;
    }
    let mut buffer = vec![0u16; 32_768];
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err("Cannot obtain a bounded Windows system directory".into());
    }
    let directory = PathBuf::from(OsString::from_wide(&buffer[..length]));
    if !directory.is_absolute() {
        return Err("Windows system directory is not absolute".into());
    }
    ordinary(&directory, true)?;
    let path = directory.join("nvapi64.dll");
    ordinary(&path, false)?;
    Ok(path)
}
fn load() -> Result<Library, String> {
    let path = system_dll()?;
    let expected = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err("NUL in NVAPI module path".into());
    }
    wide.push(0);
    // Exact absolute System32 path, restricted dependency search. No CWD/PATH fallback.
    let module = unsafe { LoadLibraryExW(wide.as_ptr(), null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
    if module.is_null() {
        return Err(format!(
            "Load System32 NVAPI: {}",
            std::io::Error::last_os_error()
        ));
    }
    let library = Library(module);
    let mut actual = vec![0u16; 32_768];
    let length =
        unsafe { GetModuleFileNameW(library.0, actual.as_mut_ptr(), actual.len() as u32) } as usize;
    if length == 0 || length >= actual.len() {
        return Err("Cannot verify loaded NVAPI module path".into());
    }
    let actual = PathBuf::from(OsString::from_wide(&actual[..length]));
    ordinary(&actual, false)?;
    let actual = std::fs::canonicalize(actual).map_err(|e| e.to_string())?;
    let actual_text = actual
        .to_str()
        .ok_or("Loaded NVAPI path has invalid Unicode")?;
    let expected_text = expected
        .to_str()
        .ok_or("System32 NVAPI path has invalid Unicode")?;
    if !actual_text.eq_ignore_ascii_case(expected_text) {
        return Err("Loaded NVAPI module differs from the vetted System32 path".into());
    }
    Ok(library)
}
fn pci(function: Pci, handle: Handle) -> Result<PciIdentity, String> {
    let mut result = PciIdentity {
        device_id: 0,
        subsystem_id: 0,
        revision_id: 0,
        extended_device_id: 0,
    };
    status(
        unsafe {
            function(
                handle,
                &mut result.device_id,
                &mut result.subsystem_id,
                &mut result.revision_id,
                &mut result.extended_device_id,
            )
        },
        "NvAPI_GPU_GetPCIIdentifiers",
    )?;
    result.validate()?;
    Ok(result)
}

fn loaded_query(library: &Library) -> Result<QueryInterface, String> {
    let entry = unsafe { GetProcAddress(library.0, c"nvapi_QueryInterface".as_ptr().cast()) }
        .ok_or("NVAPI QueryInterface export is missing")?;
    Ok(unsafe {
        std::mem::transmute::<unsafe extern "system" fn() -> isize, QueryInterface>(entry)
    })
}
// The caller retains the supplying library throughout resolution and calls.
unsafe fn enumerate_query(query: QueryInterface) -> Result<(Pci, PhysicalIdentities), String> {
    let resolve = |id| -> Result<*const c_void, String> {
        let address = unsafe { query(id) };
        if address.is_null() {
            Err(format!(
                "Required NVAPI interface 0x{id:08x} is unavailable"
            ))
        } else {
            Ok(address)
        }
    };
    let initialize: Initialize =
        unsafe { std::mem::transmute::<*const c_void, Initialize>(resolve(0x0150_e828)?) };
    let enumerate: Enumerate =
        unsafe { std::mem::transmute::<*const c_void, Enumerate>(resolve(0xe5ac_921f)?) };
    let identify: Pci = unsafe { std::mem::transmute::<*const c_void, Pci>(resolve(0x2ddf_b66e)?) };
    status(unsafe { initialize() }, "NvAPI_Initialize")?;
    let mut handles = [null_mut(); MAX_GPUS];
    let mut count = 0i32;
    status(
        unsafe { enumerate(handles.as_mut_ptr(), &mut count) },
        "NvAPI_EnumPhysicalGPUs",
    )?;
    if count < 0 || count as usize > MAX_GPUS {
        return Err("NVAPI returned an invalid physical GPU count".into());
    }
    let mut identities = Vec::with_capacity(count as usize);
    for (index, &handle) in handles[..count as usize].iter().enumerate() {
        if handle.is_null() || handles[..index].contains(&handle) {
            return Err("NVAPI returned null or duplicate physical GPU handles".into());
        }
        identities.push((handle, pci(identify, handle)?));
    }
    Ok((identify, identities))
}

/// Retains this object's DLL reference; never invokes global NVAPI_Unload.
/// Rc phantom prevents both Send and Sync, including destruction on another thread.
pub(crate) struct Context {
    query: QueryInterface,
    pci: Pci,
    selected: Handle,
    owner: ThreadId,
    _thread_bound: PhantomData<Rc<()>>,
    _library: Option<Library>,
}
impl Context {
    #[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
    pub(crate) fn open(expected: PciIdentity) -> Result<Self, String> {
        expected.validate()?;
        let library = load()?;
        let query = loaded_query(&library)?;
        unsafe { Self::from_parts(expected, query, Some(library)) }
    }
    #[cfg(not(all(target_arch = "x86_64", target_pointer_width = "64")))]
    pub(crate) fn open(_expected: PciIdentity) -> Result<Self, String> {
        Err("Native NVAPI context supports Windows x64 only".into())
    }
    #[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
    pub(crate) fn enumerate_identities() -> Result<Vec<PciIdentity>, String> {
        let library = load()?;
        let query = loaded_query(&library)?;
        let (_, items) = unsafe { enumerate_query(query) }?;
        Ok(items.into_iter().map(|(_, identity)| identity).collect())
    }
    #[cfg(not(all(target_arch = "x86_64", target_pointer_width = "64")))]
    pub(crate) fn enumerate_identities() -> Result<Vec<PciIdentity>, String> {
        Err("Native NVAPI context supports Windows x64 only".into())
    }
    /// # Safety
    /// Only trusted fake ABI functions with valid lifetimes; no vendor DLL/device functions.
    #[cfg(test)]
    pub(crate) unsafe fn enumerate_from_query(
        query: QueryInterface,
    ) -> Result<Vec<PciIdentity>, String> {
        let (_, items) = unsafe { enumerate_query(query) }?;
        Ok(items.into_iter().map(|(_, identity)| identity).collect())
    }
    /// Construct an owner-thread context without a library loader, for fake ABI tests.
    ///
    /// # Safety
    /// Caller must provide only trusted fake functions with the exact requested
    /// C signatures and valid lifetime for the entire Context. Query and all
    /// returned pointers must never reference a vendor DLL or real native device.
    #[cfg(test)]
    pub(crate) unsafe fn from_query(
        expected: PciIdentity,
        query: QueryInterface,
    ) -> Result<Self, String> {
        unsafe { Self::from_parts(expected, query, None) }
    }
    unsafe fn from_parts(
        expected: PciIdentity,
        query: QueryInterface,
        library: Option<Library>,
    ) -> Result<Self, String> {
        expected.validate()?;
        let (identify, identities) = unsafe { enumerate_query(query) }?;
        let mut selected = None;
        for (handle, identity) in identities {
            if identity == expected && selected.replace(handle).is_some() {
                return Err(
                    "Selected NVIDIA PCI identity is ambiguous; no GPU-index fallback".into(),
                );
            }
        }
        Ok(Self {
            query,
            pci: identify,
            selected: selected.ok_or("Selected NVIDIA PCI identity was not found")?,
            owner: std::thread::current().id(),
            _thread_bound: PhantomData,
            _library: library,
        })
    }
    fn owner(&self) -> Result<(), String> {
        if self.owner != std::thread::current().id() {
            return Err("NVAPI context used outside its owner thread".into());
        }
        Ok(())
    }
    pub(crate) fn optional(&self, id: u32) -> Result<Option<*const c_void>, String> {
        self.owner()?;
        let address = unsafe { (self.query)(id) };
        Ok((!address.is_null()).then_some(address))
    }
    pub(crate) fn required(&self, id: u32) -> Result<*const c_void, String> {
        self.optional(id)?
            .ok_or_else(|| format!("Required NVAPI interface 0x{id:08x} is unavailable"))
    }
    pub(crate) fn identity(&self) -> Result<PciIdentity, String> {
        self.owner()?;
        pci(self.pci, self.selected)
    }
    pub(crate) fn handle(&self) -> Result<Handle, String> {
        self.owner()?;
        Ok(self.selected)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::sensors::nvidia::{TARGET_GPU, ThermalSettings};
    use std::{
        cell::RefCell,
        collections::{BTreeMap, BTreeSet},
    };
    const BASE: [u32; 3] = [0x0150_e828, 0xe5ac_921f, 0x2ddf_b66e];
    struct State {
        handles: Vec<usize>,
        count: i32,
        identities: BTreeMap<usize, PciIdentity>,
        lookups: Vec<u32>,
        missing: BTreeSet<u32>,
        initialize_status: i32,
        enumerate_status: i32,
        pci_status: i32,
        initializes: usize,
        enumerations: usize,
        pci_calls: Vec<usize>,
        thermal_calls: usize,
    }
    pub(crate) fn expected() -> PciIdentity {
        PciIdentity {
            device_id: 0x2222_10de,
            subsystem_id: 7,
            revision_id: 3,
            extended_device_id: 11,
        }
    }
    impl Default for State {
        fn default() -> Self {
            let mut other = expected();
            other.device_id = 0x1111_10de;
            Self {
                handles: vec![1, 2],
                count: 2,
                identities: BTreeMap::from([(1, other), (2, expected())]),
                lookups: vec![],
                missing: BTreeSet::new(),
                initialize_status: 0,
                enumerate_status: 0,
                pci_status: 0,
                initializes: 0,
                enumerations: 0,
                pci_calls: vec![],
                thermal_calls: 0,
            }
        }
    }
    thread_local! {static STATE:RefCell<State>=RefCell::new(State::default());}
    unsafe extern "C" fn initialize() -> i32 {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.initializes += 1;
            s.initialize_status
        })
    }
    unsafe extern "C" fn enumerate(handles: *mut Handle, count: *mut i32) -> i32 {
        assert!(!handles.is_null() && !count.is_null());
        unsafe {
            assert_eq!(*count, 0);
            assert!(
                std::slice::from_raw_parts(handles, MAX_GPUS)
                    .iter()
                    .all(|h| h.is_null())
            );
        }
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.enumerations += 1;
            for (i, handle) in s.handles.iter().take(MAX_GPUS).enumerate() {
                unsafe {
                    handles.add(i).write(*handle as Handle);
                }
            }
            unsafe {
                count.write(s.count);
            }
            s.enumerate_status
        })
    }
    unsafe extern "C" fn identify(
        handle: Handle,
        device: *mut u32,
        subsystem: *mut u32,
        revision: *mut u32,
        extended: *mut u32,
    ) -> i32 {
        assert!(
            !device.is_null() && !subsystem.is_null() && !revision.is_null() && !extended.is_null()
        );
        unsafe {
            assert_eq!((*device, *subsystem, *revision, *extended), (0, 0, 0, 0));
        }
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.pci_calls.push(handle as usize);
            let Some(value) = s.identities.get(&(handle as usize)).copied() else {
                return -12;
            };
            unsafe {
                device.write(value.device_id);
                subsystem.write(value.subsystem_id);
                revision.write(value.revision_id);
                extended.write(value.extended_device_id);
            }
            s.pci_status
        })
    }
    unsafe extern "C" fn thermal(handle: Handle, target: i32, out: *mut ThermalSettings) -> i32 {
        assert_eq!(handle as usize, 2);
        assert_eq!(target, TARGET_GPU);
        assert!(!out.is_null());
        unsafe {
            assert_eq!(*out, ThermalSettings::default());
            (*out).count = 1;
            (*out).sensors[0].target = TARGET_GPU;
            (*out).sensors[0].current_temperature = 42;
        }
        STATE.with(|s| s.borrow_mut().thermal_calls += 1);
        0
    }
    unsafe extern "C" fn query(id: u32) -> *const c_void {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.lookups.push(id);
            if s.missing.contains(&id) {
                return std::ptr::null();
            }
            match id {
                0x0150_e828 => initialize as *const c_void,
                0xe5ac_921f => enumerate as *const c_void,
                0x2ddf_b66e => identify as *const c_void,
                0xe364_0a56 => thermal as *const c_void,
                _ => std::ptr::null(),
            }
        })
    }
    fn reset() {
        STATE.with(|s| *s.borrow_mut() = State::default());
    }
    #[test]
    fn discovery_empty_and_duplicate_identities_remain_read_only_without_index_selection() {
        reset();
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.count = 0;
            s.handles.clear();
        });
        assert!(
            unsafe { Context::enumerate_from_query(query) }
                .unwrap()
                .is_empty()
        );
        assert_eq!(lookups(), BASE);
        reset();
        STATE.with(|s| s.borrow_mut().identities.insert(1, expected()));
        assert_eq!(
            unsafe { Context::enumerate_from_query(query) }.unwrap(),
            vec![expected(), expected()]
        );
        assert!(build().is_err());
    }
    #[test]
    fn discovery_rejects_corrupt_handles_counts_and_pci_failure() {
        for count in [-1, 65] {
            reset();
            STATE.with(|s| s.borrow_mut().count = count);
            assert!(unsafe { Context::enumerate_from_query(query) }.is_err());
        }
        for handles in [vec![0, 2], vec![1, 1]] {
            reset();
            STATE.with(|s| s.borrow_mut().handles = handles);
            assert!(unsafe { Context::enumerate_from_query(query) }.is_err());
        }
        reset();
        STATE.with(|s| s.borrow_mut().pci_status = -8);
        assert!(unsafe { Context::enumerate_from_query(query) }.is_err());
    }
    fn build() -> Result<Context, String> {
        unsafe { Context::from_query(expected(), query) }
    }
    pub(crate) fn fixture() -> Context {
        reset();
        build().unwrap()
    }
    pub(crate) fn lookups() -> Vec<u32> {
        STATE.with(|s| s.borrow().lookups.clone())
    }
    pub(crate) fn thermal_calls() -> usize {
        STATE.with(|s| s.borrow().thermal_calls)
    }

    #[test]
    fn base_context_selects_complete_pci_identity_without_index_zero_or_extra_api_lookup() {
        let context = fixture();
        assert!(context._library.is_none());
        assert_eq!(context.handle().unwrap() as usize, 2);
        assert_eq!(context.identity().unwrap(), expected());
        assert_eq!(lookups(), BASE);
        STATE.with(|s| {
            let s = s.borrow();
            assert_eq!((s.initializes, s.enumerations), (1, 1));
            assert_eq!(s.pci_calls, [1, 2, 2]);
        });
        for field in 0..4 {
            reset();
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                let id = s.identities.get_mut(&2).unwrap();
                match field {
                    0 => id.device_id ^= 1 << 16,
                    1 => id.subsystem_id ^= 1,
                    2 => id.revision_id ^= 1,
                    _ => id.extended_device_id ^= 1,
                }
            });
            assert!(build().is_err());
        }
    }
    #[test]
    fn invalid_counts_null_duplicate_ambiguous_or_missing_handles_fail_closed() {
        for count in [-1, 0, 65] {
            reset();
            STATE.with(|s| s.borrow_mut().count = count);
            assert!(build().is_err());
            STATE.with(|s| assert!(s.borrow().pci_calls.is_empty()));
        }
        for handles in [vec![0, 2], vec![1, 1], vec![1]] {
            reset();
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                s.count = handles.len() as i32;
                s.handles = handles;
            });
            assert!(build().is_err());
        }
        reset();
        STATE.with(|s| s.borrow_mut().identities.insert(1, expected()));
        assert!(build().is_err());
        // The full capacity is valid, with the requested GPU at the last slot.
        reset();
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.handles = (1..=64).collect();
            s.count = 64;
            s.identities.clear();
            for h in 1..=64 {
                let mut id = expected();
                id.subsystem_id = h as u32;
                s.identities.insert(h, id);
            }
            s.identities.insert(64, expected());
            s.identities.get_mut(&7).unwrap().subsystem_id = 99;
        });
        let context = build().unwrap();
        assert_eq!(context.handle().unwrap() as usize, 64);
    }
    #[test]
    fn invalid_binding_missing_base_functions_and_failed_status_never_select_a_gpu() {
        reset();
        let mut invalid = expected();
        invalid.device_id = 0;
        assert!(unsafe { Context::from_query(invalid, query) }.is_err());
        assert!(lookups().is_empty());
        for id in BASE {
            reset();
            STATE.with(|s| s.borrow_mut().missing.insert(id));
            assert!(build().is_err());
            STATE.with(|s| assert_eq!((s.borrow().initializes, s.borrow().enumerations), (0, 0)));
        }
        for failure in 0..3 {
            reset();
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                match failure {
                    0 => s.initialize_status = -1,
                    1 => s.enumerate_status = -2,
                    _ => s.pci_status = -3,
                }
            });
            assert!(build().is_err());
        }
    }
    #[test]
    fn identity_reads_selected_original_handle_without_reenumeration_or_fallback() {
        let context = fixture();
        let mut drift = expected();
        drift.revision_id += 1;
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.identities.insert(2, drift);
            s.identities.insert(1, expected());
        });
        assert_eq!(context.identity().unwrap(), drift);
        assert_eq!(context.handle().unwrap() as usize, 2);
        STATE.with(|s| assert_eq!(s.borrow().enumerations, 1));
        STATE.with(|s| s.borrow_mut().pci_status = -9);
        assert!(context.identity().is_err());
        assert_eq!(context.handle().unwrap() as usize, 2);
    }
    #[test]
    fn optional_required_and_owner_thread_guard_do_not_perform_hidden_lookups() {
        let mut context = fixture();
        assert_eq!(context.optional(0xdead_beef).unwrap(), None);
        assert!(context.required(0xdead_beef).is_err());
        assert_eq!(
            lookups(),
            [BASE[0], BASE[1], BASE[2], 0xdead_beef, 0xdead_beef]
        );
        // Do not unsafely send Context: assign a different ID locally to exercise
        // the runtime check while Rc phantom enforces the safe Rust type boundary.
        context.owner = std::thread::spawn(|| std::thread::current().id())
            .join()
            .unwrap();
        let calls = lookups();
        let pci = STATE.with(|s| s.borrow().pci_calls.len());
        assert!(context.handle().is_err());
        assert!(context.identity().is_err());
        assert!(context.optional(0xe364_0a56).is_err());
        assert!(context.required(0xe364_0a56).is_err());
        assert_eq!(lookups(), calls);
        assert_eq!(STATE.with(|s| s.borrow().pci_calls.len()), pci);
        context.owner = std::thread::current().id();
    }
}
