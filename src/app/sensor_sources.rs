//! Explicit source adapters. Importing this module never opens a device or DLL.
use super::sensor_worker;
use crate::sensors::{Reading, nvidia};
use crate::sensors::{amd, ddr5};
use crate::{
    lcd::{hid_adapter, hid_policy},
    runtime::output::Cancellation,
};

struct Liquid<B: hid_adapter::Backend> {
    backend: B,
    target: String,
    cancel: Cancellation,
}
impl<B: hid_adapter::Backend> sensor_worker::Provider for Liquid<B> {
    fn poll_interval_ms(&self) -> u64 {
        2000
    }
    fn poll(&mut self, now_ms: u64) -> Result<Option<Reading>, String> {
        if self.cancel.cancelled() {
            return Err("Liquid query cancelled".into());
        }
        // MI00 is opened exclusively for this single 441-byte status exchange.
        // Its local link is dropped by read_liquid on both success and error.
        let reading = hid_adapter::read_liquid(&mut self.backend, Some(&self.target), false)?;
        if self.cancel.cancelled() {
            return Err("Liquid query cancelled".into());
        }
        Ok(Some(Reading {
            celsius: reading.celsius,
            observed_ms: now_ms,
        }))
    }
}

/// MI00 is an explicitly selected local input, independent of any MI01 LCD.
/// Never derive a path or physical pairing by replacing an interface suffix.
pub fn attach_liquid<B, F>(
    runtime: &mut super::runtime::Runtime,
    input: &str,
    target: &str,
    backend: F,
) -> Result<(), String>
where
    B: hid_adapter::Backend + 'static,
    F: Fn(Cancellation) -> Result<B, String> + Send + Sync + 'static,
{
    let factory = liquid_factory(target, backend)?;
    runtime.attach_sensor(
        input,
        vec![format!("hid:{}", target.to_ascii_lowercase())],
        factory,
    )
}

/// Dormant protocol factory, also used by isolated fake HID backends.
pub fn liquid_factory<B, F>(target: &str, backend: F) -> Result<sensor_worker::Factory, String>
where
    B: hid_adapter::Backend + 'static,
    F: Fn(Cancellation) -> Result<B, String> + Send + Sync + 'static,
{
    hid_policy::validate_path(hid_policy::Kind::Liquid, target)?;
    let target = target.to_owned();
    Ok(std::sync::Arc::new(move |cancel| {
        if cancel.cancelled() {
            return Err("Liquid activation cancelled".into());
        }
        Ok(Box::new(Liquid {
            backend: backend(cancel.clone())?,
            target: target.clone(),
            cancel,
        }))
    }))
}

/// Dormant native activation. No default UI, document load or test calls this.
#[cfg(windows)]
pub fn attach_native_liquid(
    runtime: &mut super::runtime::Runtime,
    input: &str,
    target: &str,
) -> Result<(), String> {
    attach_native_target(
        runtime,
        input,
        super::sensor_bindings::Target::Liquid {
            explicit_mi00_path: target.into(),
        },
    )
}

impl<B: nvidia::Backend> sensor_worker::Provider for nvidia::Provider<B> {
    fn poll(&mut self, now_ms: u64) -> Result<Option<Reading>, String> {
        nvidia::Provider::poll(self, now_ms)
    }
}
impl<B: amd::Backend> sensor_worker::Provider for amd::Provider<B> {
    fn poll(&mut self, now_ms: u64) -> Result<Option<Reading>, String> {
        amd::Provider::poll(self, now_ms)
    }
}
impl<B: ddr5::Backend> sensor_worker::Provider for ddr5::Provider<B> {
    fn poll_interval_ms(&self) -> u64 {
        ddr5::POLL_INTERVAL_MS
    }
    fn poll(&mut self, now_ms: u64) -> Result<Option<Reading>, String> {
        ddr5::Provider::poll(self, now_ms)
    }
}
/// Explicit local capability only. The stand UI never invokes native factories.
#[cfg(windows)]
pub fn attach_native_ddr5(
    runtime: &mut super::runtime::Runtime,
    input: &str,
    binding: ddr5::Binding,
    module: &std::path::Path,
) -> Result<(), String> {
    attach_native_target(
        runtime,
        input,
        super::sensor_bindings::Target::Ddr5 {
            controller: binding.controller,
            port: binding.port,
            address: binding.address,
            module_path: module
                .to_str()
                .ok_or("DDR5 module path must be Unicode")?
                .into(),
        },
    )
}

/// No default UI/test invokes the dormant CPUID/topology/kernel capability.
#[cfg(windows)]
pub fn attach_native_amd(
    runtime: &mut super::runtime::Runtime,
    input: &str,
    binding: amd::CpuIdentity,
    module: &std::path::Path,
) -> Result<(), String> {
    attach_native_target(
        runtime,
        input,
        super::sensor_bindings::Target::Amd {
            vendor: binding.vendor,
            signature: binding.signature,
            packages: binding.packages,
            module_path: module
                .to_str()
                .ok_or("AMD module path must be Unicode")?
                .into(),
        },
    )
}

/// Native capability activation is explicit and owner-thread-bound. This helper
/// is not invoked by the stand UI, document loading, discovery or tests.
#[cfg(windows)]
pub fn attach_nvidia(
    runtime: &mut super::runtime::Runtime,
    input: &str,
    binding: nvidia::PciIdentity,
) -> Result<(), String> {
    attach_native_target(
        runtime,
        input,
        super::sensor_bindings::Target::Nvidia {
            pci: binding.into(),
        },
    )
}
#[cfg(windows)]
fn attach_native_target(
    runtime: &mut super::runtime::Runtime,
    input: &str,
    target: super::sensor_bindings::Target,
) -> Result<(), String> {
    let keys = target.resource_keys()?;
    runtime.attach_sensor(input, keys, native_factory(&target)?)
}
/// Only constructs a dormant factory. Native handles/modules are opened on its owner thread.
#[cfg(windows)]
pub(crate) fn native_factory(
    target: &super::sensor_bindings::Target,
) -> Result<sensor_worker::Factory, String> {
    use super::sensor_bindings::Target;
    target.validate()?;
    match target {
        Target::Liquid { explicit_mi00_path } => liquid_factory(explicit_mi00_path, |_| {
            Ok(crate::lcd::hid_windows::NativeBackend)
        }),
        Target::Amd { module_path, .. } => {
            let binding = target.amd_binding()?;
            let module = std::path::PathBuf::from(module_path);
            Ok(std::sync::Arc::new(move |cancel| {
                let backend =
                    crate::sensors::amd_windows::NativeBackend::open(binding, &module, cancel)?;
                Ok(Box::new(amd::Provider::new(binding, backend)?))
            }))
        }
        Target::Ddr5 { module_path, .. } => {
            let binding = target.ddr5_binding()?;
            let module = std::path::PathBuf::from(module_path);
            Ok(std::sync::Arc::new(move |cancel| {
                let backend =
                    crate::sensors::ddr5_windows::NativeBackend::open(binding, &module, cancel)?;
                Ok(Box::new(ddr5::Provider::new(binding, backend)?))
            }))
        }
        Target::Nvidia { .. } => {
            let binding = target.nvidia_binding()?;
            Ok(std::sync::Arc::new(move |cancel| {
                if cancel.cancelled() {
                    return Err("Sensor activation cancelled".into());
                }
                let backend = crate::sensors::nvidia_windows::NativeBackend::open(binding)?;
                if cancel.cancelled() {
                    return Err("Sensor activation cancelled".into());
                }
                Ok(Box::new(nvidia::Provider::new(binding, backend)?))
            }))
        }
    }
}
