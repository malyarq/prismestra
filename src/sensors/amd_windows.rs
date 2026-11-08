//! Dormant AMD thermal capability. No native function is called by import or tests.
use super::amd::{Backend, CpuIdentity, Module};
use crate::{
    runtime::output::Cancellation,
    transport::{
        amd_smn::{PCI_MUTEX, VerifiedSmn},
        ene_bus::SharedMutex,
        pawnio_windows::{NamedMutex, NativeDevice},
    },
};
use std::{mem::size_of, path::Path};
use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError},
    System::SystemInformation::{
        GROUP_AFFINITY, GetLogicalProcessorInformationEx, PROCESSOR_RELATIONSHIP,
        RelationProcessorPackage, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
    },
};

fn package_count(bytes: &[u8]) -> Result<u32, String> {
    if bytes.is_empty() || bytes.len() > 1024 * 1024 {
        return Err("Invalid CPU topology buffer".into());
    }
    let mut offset = 0;
    let mut count = 0u32;
    while offset < bytes.len() {
        let header = bytes
            .get(offset..offset + 8)
            .ok_or("Truncated CPU topology record")?;
        let kind = u32::from_ne_bytes(header[..4].try_into().unwrap());
        let size = u32::from_ne_bytes(header[4..].try_into().unwrap()) as usize;
        if kind != RelationProcessorPackage as u32 || size < 8 || size > bytes.len() - offset {
            return Err("Unexpected CPU topology record".into());
        }
        let record = &bytes[offset..offset + size];
        let payload = std::mem::offset_of!(SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX, Anonymous);
        let count_offset = payload + std::mem::offset_of!(PROCESSOR_RELATIONSHIP, GroupCount);
        let group_count = u16::from_ne_bytes(
            record
                .get(count_offset..count_offset + 2)
                .ok_or("CPU topology lacks group count")?
                .try_into()
                .unwrap(),
        ) as usize;
        let mask_offset = payload + std::mem::offset_of!(PROCESSOR_RELATIONSHIP, GroupMask);
        let required = group_count
            .checked_mul(size_of::<GROUP_AFFINITY>())
            .and_then(|n| n.checked_add(mask_offset))
            .ok_or("CPU topology group size overflow")?;
        if group_count == 0 || required > size {
            return Err("CPU topology group mask is truncated".into());
        }
        let mut groups = std::collections::BTreeSet::new();
        for group in record[mask_offset..required]
            .as_chunks::<{ size_of::<GROUP_AFFINITY>() }>()
            .0
        {
            let mask = usize::from_ne_bytes(group[..size_of::<usize>()].try_into().unwrap());
            let at = std::mem::offset_of!(GROUP_AFFINITY, Group);
            let id = u16::from_ne_bytes(group[at..at + 2].try_into().unwrap());
            if mask == 0 || !groups.insert(id) {
                return Err("CPU topology has empty or duplicate processor group".into());
            }
        }
        offset += size;
        count = count.checked_add(1).ok_or("CPU package count overflow")?;
    }
    Ok(count)
}
fn native_package_count() -> Result<u32, String> {
    let mut length = 0;
    let first = unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorPackage,
            std::ptr::null_mut(),
            &mut length,
        )
    };
    if first != 0 || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
        return Err("Cannot obtain bounded CPU package topology size".into());
    }
    for _ in 0..3 {
        if !(8..=1024 * 1024).contains(&length) {
            return Err("CPU topology exceeds bounded size".into());
        }
        let capacity = length as usize;
        let mut storage = vec![0u64; capacity.div_ceil(size_of::<u64>())];
        let ok = unsafe {
            GetLogicalProcessorInformationEx(
                RelationProcessorPackage,
                storage.as_mut_ptr().cast(),
                &mut length,
            )
        };
        if ok == 0 {
            if unsafe { GetLastError() } == ERROR_INSUFFICIENT_BUFFER {
                continue;
            }
            return Err("CPU package topology query failed".into());
        }
        if length as usize > capacity {
            return Err("CPU topology returned beyond capacity".into());
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), length as usize) };
        return package_count(bytes);
    }
    Err("CPU topology changed during bounded query".into())
}
/// CPUID and OS package metadata only; no PawnIO module or SMN reads.
pub(crate) fn metadata_identity() -> Result<CpuIdentity, String> {
    #[cfg(target_arch = "x86_64")]
    {
        // Explicit native activation only; fake tests never invoke this function.
        let root = std::arch::x86_64::__cpuid(0);
        if root.eax < 1 {
            return Err("CPU lacks identity leaf".into());
        }
        let mut vendor = [0u8; 12];
        vendor[..4].copy_from_slice(&root.ebx.to_le_bytes());
        vendor[4..8].copy_from_slice(&root.edx.to_le_bytes());
        vendor[8..].copy_from_slice(&root.ecx.to_le_bytes());
        let leaf = std::arch::x86_64::__cpuid(1);
        let value = CpuIdentity {
            vendor,
            signature: leaf.eax,
            packages: native_package_count()?,
        };
        Ok(value)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        Err("Native AMD Tctl requires Windows x64".into())
    }
}
fn identity() -> Result<CpuIdentity, String> {
    let value = metadata_identity()?;
    value.validate()?;
    Ok(value)
}
pub(crate) struct NativeBackend {
    binding: CpuIdentity,
    executor: VerifiedSmn<NativeDevice, NamedMutex>,
}
impl NativeBackend {
    pub(crate) fn open(
        binding: CpuIdentity,
        module: &Path,
        cancel: Cancellation,
    ) -> Result<Self, String> {
        binding.validate()?;
        if cancel.cancelled() || !module.is_absolute() {
            return Err("AMD capability requires an absolute module path and live request".into());
        }
        if identity()? != binding {
            return Err("CPU identity differs from explicit binding".into());
        }
        let module = Module::read(module)?;
        let mutex = NamedMutex::named(PCI_MUTEX, cancel.clone())?;
        let executor = {
            let _guard = mutex.lock()?;
            if identity()? != binding {
                return Err("CPU identity changed before module load".into());
            }
            let executor =
                VerifiedSmn::connect(&module, NativeDevice::open()?, mutex.clone(), cancel)?;
            if identity()? != binding {
                return Err("CPU identity changed during module load".into());
            }
            executor
        };
        Ok(Self { binding, executor })
    }
}
impl Backend for NativeBackend {
    fn identity(&mut self) -> Result<CpuIdentity, String> {
        identity()
    }
    fn read_tctl_raw(&mut self) -> Result<u32, String> {
        let expected = self.binding;
        self.executor.read_tctl_raw_checked(|| {
            if identity()? != expected {
                Err("CPU identity changed under the PCI guard".into())
            } else {
                Ok(())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(kind: u32, size: u32) -> Vec<u8> {
        let mut b = vec![0; size as usize];
        b[..4].copy_from_slice(&kind.to_ne_bytes());
        b[4..8].copy_from_slice(&size.to_ne_bytes());
        b
    }
    #[test]
    fn only_synthetic_topology_is_parsed_no_native_identity_query() {
        let payload = std::mem::offset_of!(SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX, Anonymous);
        let mut one = record(
            RelationProcessorPackage as u32,
            (payload + size_of::<PROCESSOR_RELATIONSHIP>()) as u32,
        );
        let count = payload + std::mem::offset_of!(PROCESSOR_RELATIONSHIP, GroupCount);
        one[count..count + 2].copy_from_slice(&1u16.to_ne_bytes());
        one[payload + std::mem::offset_of!(PROCESSOR_RELATIONSHIP, GroupMask)] = 1;
        assert_eq!(package_count(&one).unwrap(), 1);
        assert_eq!(package_count(&[one.clone(), one].concat()).unwrap(), 2);
        for bytes in [
            vec![],
            vec![0; 7],
            record(0, 8),
            record(RelationProcessorPackage as u32, 8)[..7].to_vec(),
        ] {
            assert!(package_count(&bytes).is_err());
        }
        let mut short = record(RelationProcessorPackage as u32, 16);
        short[4..8].copy_from_slice(&17u32.to_ne_bytes());
        assert!(package_count(&short).is_err());
    }
}
