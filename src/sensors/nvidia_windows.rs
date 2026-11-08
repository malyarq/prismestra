//! Dormant Windows x64 GPU Core adapter over the vetted shared NVAPI context.
//! Only explicit open performs work; this adapter resolves only the thermal API
//! in addition to shared Initialize/Enum/PCI. No cooler/mutation interfaces.
use super::nvidia::{Backend, PciIdentity, TARGET_GPU, ThermalSettings};
use crate::transport::nvapi_windows::{Context, Handle};
type Thermal = unsafe extern "C" fn(Handle, i32, *mut ThermalSettings) -> i32;
pub struct NativeBackend {
    context: Context,
    thermal: Thermal,
}
impl NativeBackend {
    pub fn open(expected: PciIdentity) -> Result<Self, String> {
        Self::from_context(Context::open(expected)?)
    }
    fn from_context(context: Context) -> Result<Self, String> {
        let thermal = unsafe {
            std::mem::transmute::<*const std::ffi::c_void, Thermal>(context.required(0xe364_0a56)?)
        };
        Ok(Self { context, thermal })
    }
}
impl Backend for NativeBackend {
    fn identity(&mut self) -> Result<PciIdentity, String> {
        self.context.identity()
    }
    fn thermal_settings(&mut self, output: &mut ThermalSettings) -> Result<(), String> {
        let handle = self.context.handle()?;
        if *output != ThermalSettings::default() {
            return Err("NVAPI thermal output must start zeroed with version 2".into());
        }
        let code = unsafe { (self.thermal)(handle, TARGET_GPU, output) };
        if code != 0 {
            return Err(format!(
                "NvAPI_GPU_GetThermalSettings failed with NVAPI status {code}"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::nvapi_windows::tests as fake;
    #[test]
    fn sensor_adapter_resolves_only_thermal_after_shared_base_and_keeps_zeroed_output_policy() {
        let mut backend = NativeBackend::from_context(fake::fixture()).unwrap();
        assert_eq!(
            fake::lookups(),
            [0x0150_e828, 0xe5ac_921f, 0x2ddf_b66e, 0xe364_0a56]
        );
        assert_eq!(backend.identity().unwrap(), fake::expected());
        let mut output = ThermalSettings::default();
        backend.thermal_settings(&mut output).unwrap();
        assert_eq!(output.sensors[0].current_temperature, 42);
        assert_eq!(fake::thermal_calls(), 1);
        assert!(backend.thermal_settings(&mut output).is_err());
        assert_eq!(fake::thermal_calls(), 1);
        assert_eq!(
            fake::lookups(),
            [0x0150_e828, 0xe5ac_921f, 0x2ddf_b66e, 0xe364_0a56]
        );
    }
}
