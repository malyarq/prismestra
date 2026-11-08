//! Serializable identity for read-only NVIDIA temperature sources.
use serde::{Deserialize, Serialize};
use crate::sensors::nvidia::PciIdentity;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pci {
    pub device_id: u32,
    pub subsystem_id: u32,
    pub revision_id: u32,
    pub extended_device_id: u32,
}
impl Pci {
    pub fn identity(self) -> Result<PciIdentity, String> {
        let identity = PciIdentity {
            device_id: self.device_id,
            subsystem_id: self.subsystem_id,
            revision_id: self.revision_id,
            extended_device_id: self.extended_device_id,
        };
        identity.validate()?;
        Ok(identity)
    }
}
impl From<PciIdentity> for Pci {
    fn from(p: PciIdentity) -> Self {
        Self {
            device_id: p.device_id,
            subsystem_id: p.subsystem_id,
            revision_id: p.revision_id,
            extended_device_id: p.extended_device_id,
        }
    }
}
