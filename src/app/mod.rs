//! Application-owned preview/project integration. Native output backends remain
//! separate; a preview controller cannot acquire them by loading a project.
pub mod ambient;
#[cfg(windows)]
pub mod ambient_windows;
pub mod assets;
pub mod bundle;
pub mod capture_worker;
pub mod config;
pub mod device_setup;
pub mod document;
pub mod lcd;
pub mod lcd_worker;
#[cfg(windows)]
pub mod lighting_bindings;
#[cfg(windows)]
pub mod lighting_outputs;
pub mod preview;
pub mod runtime;
pub mod sensor_bindings;
pub mod sensor_discovery;
#[cfg(windows)]
pub mod sensor_discovery_windows;
pub mod sensor_form;
pub mod sensor_modules;
pub mod sensor_pci;
pub mod sensor_sessions;
pub mod sensor_sources;
pub mod sensor_stream;
pub mod sensor_worker;
pub mod sensors;
pub mod shutdown;
pub(crate) mod strict_json;
