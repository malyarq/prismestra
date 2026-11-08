//! Dormant explicit PIIX4 DDR5 binding. No default UI, tests or profile import
//! invokes this capability; no driver installation or hardware discovery here.
use super::ddr5::{Backend, Binding};
use crate::{
    runtime::output::Cancellation,
    transport::{
        ddr5_bus::VerifiedDdr5,
        ene_bus::SharedMutex,
        pawnio,
        pawnio_windows::{self, NamedMutex, NativeDevice},
    },
};
use std::path::Path;
pub(crate) struct NativeBackend {
    bus: VerifiedDdr5<NativeDevice, NamedMutex>,
}
impl NativeBackend {
    pub(crate) fn open(
        binding: Binding,
        module_path: &Path,
        cancel: Cancellation,
    ) -> Result<Self, String> {
        binding.validate()?;
        if !module_path.is_absolute() {
            return Err("DDR5 module path must be absolute".into());
        }
        let module = pawnio::Piix4Module::read(module_path)?;
        pawnio_windows::check_writers()?;
        let mutex = NamedMutex::named(pawnio::SHARED_MUTEX, cancel.clone())?;
        let bus = {
            let _guard = mutex.lock()?;
            pawnio_windows::check_writers()?;
            let bus = VerifiedDdr5::connect(
                &module,
                NativeDevice::open()?,
                mutex.clone(),
                binding,
                cancel,
            )?;
            pawnio_windows::check_writers()?;
            bus
        };
        Ok(Self { bus })
    }
}
impl Backend for NativeBackend {
    fn binding(&self) -> Binding {
        self.bus.binding()
    }
    fn read_temperature(&mut self) -> Result<u16, String> {
        self.bus.read_checked(pawnio_windows::check_writers)
    }
}
