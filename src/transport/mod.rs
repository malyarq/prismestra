//! Explicitly bound outputs. Native adapters are opt-in and never used by stand tests.
pub(crate) mod amd_smn;
pub mod ble;
#[cfg(windows)]
pub mod ble_windows;
pub(crate) mod ddr5_bus;
pub mod ene;
pub mod ene_bus;
pub mod feature_hid;
#[cfg(windows)]
pub mod feature_hid_windows;
pub mod fusion2;
#[cfg(windows)]
pub mod home_assistant;
#[cfg(windows)]
pub(crate) mod nvapi_windows;
pub mod openrgb;
pub mod pawnio;
pub mod pawnio_adapter;
#[cfg(windows)]
pub mod pawnio_windows;
pub mod serial;
#[cfg(windows)]
pub mod serial_windows;
pub mod service_discovery;
pub mod skydimo;
pub mod valkyrie;
#[cfg(windows)]
mod windows_io;
pub mod wled;
pub mod wled_info;
