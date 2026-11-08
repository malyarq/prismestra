//! Dormant, opt-in identity discovery. NVIDIA loads its vendor API; TT opens
//! metadata-only HID handles. No thermal query, data handle, SMBus probe or setter.
use super::sensor_discovery::{self, Factory, Inventory, Metadata};
use crate::{
    lcd::{
        hid_adapter::Backend,
        hid_policy::{Descriptor, Kind},
    },
    runtime::output::Cancellation,
    sensors::{amd::CpuIdentity, nvidia::PciIdentity},
};
use std::sync::Arc;

trait Sources {
    fn nvidia(&mut self) -> Result<Vec<PciIdentity>, String>;
    fn cpu(&mut self) -> Result<CpuIdentity, String>;
    fn liquid(&mut self) -> Result<Vec<Descriptor>, String>;
}
struct Native;
impl Sources for Native {
    fn nvidia(&mut self) -> Result<Vec<PciIdentity>, String> {
        crate::transport::nvapi_windows::Context::enumerate_identities()
    }
    fn cpu(&mut self) -> Result<CpuIdentity, String> {
        crate::sensors::amd_windows::metadata_identity()
    }
    fn liquid(&mut self) -> Result<Vec<Descriptor>, String> {
        crate::lcd::hid_windows::NativeBackend.candidates(Kind::Liquid)
    }
}
fn scan(source: &mut impl Sources, cancel: &Cancellation) -> Result<Inventory, String> {
    let check = || {
        if cancel.cancelled() {
            Err("Поиск датчиков отменён".to_string())
        } else {
            Ok(())
        }
    };
    check()?;
    let nvidia = source.nvidia();
    check()?;
    let cpu = source.cpu();
    check()?;
    let liquid = source.liquid();
    check()?;
    sensor_discovery::collect(Metadata {
        nvidia,cpu,liquid,
        ddr5:Err("Автопоиск DDR5 требует отдельного разрешения на SMBus и проверенного модуля; адреса не угадываются.".into()),
    })
}
/// Merely constructs a capability. Calling the factory performs identity queries.
pub fn native_factory() -> Factory {
    Arc::new(|cancel| scan(&mut Native, &cancel))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        calls: Vec<&'static str>,
        cancel: Option<Cancellation>,
    }
    impl Sources for Fake {
        fn nvidia(&mut self) -> Result<Vec<PciIdentity>, String> {
            self.calls.push("nvidia");
            if let Some(cancel) = &self.cancel {
                cancel.cancel();
            }
            Err("fixed NVAPI unavailable".into())
        }
        fn cpu(&mut self) -> Result<CpuIdentity, String> {
            self.calls.push("cpu");
            Ok(CpuIdentity {
                vendor: *b"AuthenticAMD",
                signature: 0x00800f12,
                packages: 1,
            })
        }
        fn liquid(&mut self) -> Result<Vec<Descriptor>, String> {
            self.calls.push("liquid");
            Ok(vec![])
        }
    }
    #[test]
    fn partial_metadata_failure_does_not_hide_cpu_or_guess_ddr5_addresses() {
        let mut fake = Fake {
            calls: vec![],
            cancel: None,
        };
        let report = scan(&mut fake, &Cancellation::default()).unwrap();
        assert_eq!(fake.calls, ["nvidia", "cpu", "liquid"]);
        assert_eq!(report.candidates.len(), 1);
        let draft = report.candidates[0].draft.as_ref().unwrap();
        assert_eq!(draft.family, crate::app::sensor_form::Family::Amd);
        assert!(draft.values[3].is_empty());
        assert!(
            report
                .issues
                .iter()
                .any(|i| i.contains("fixed NVAPI unavailable"))
        );
        assert!(report.issues.iter().any(|i| i.contains("DDR5")));
    }
    #[test]
    fn cancellation_before_and_between_families_never_starts_the_next_api() {
        let cancel = Cancellation::default();
        cancel.cancel();
        let mut fake = Fake {
            calls: vec![],
            cancel: None,
        };
        assert!(scan(&mut fake, &cancel).is_err());
        assert!(fake.calls.is_empty());
        let cancel = Cancellation::default();
        fake.cancel = Some(cancel.clone());
        assert!(scan(&mut fake, &cancel).is_err());
        assert_eq!(fake.calls, ["nvidia"]);
    }
}
