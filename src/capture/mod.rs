//! Screen capture has no hardware-output capabilities. Native sources are opt-in;
//! tests feed owned synthetic pixels or software D3D textures, never the desktop.
pub mod ambient;
pub mod pixels;
#[cfg(windows)]
pub mod windows;

use crate::lighting::ambient::{Image, Rect};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    /// Exact Windows GDI source identity; not a friendly name or array index.
    pub device_name: String,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}
impl Monitor {
    pub fn validate(&self) -> Result<(), String> {
        validate_name(&self.device_name)?;
        valid_size(self.width, self.height)
    }
}
pub fn validate_name(name: &str) -> Result<(), String> {
    let suffix = name.get(11..).filter(|_| {
        name.get(..11)
            .is_some_and(|p| p.eq_ignore_ascii_case(r"\\.\DISPLAY"))
    });
    if !suffix.is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 6
            && s.bytes().all(|b| b.is_ascii_digit())
            && s.parse::<u32>().is_ok_and(|n| n != 0)
    }) {
        return Err("Expected an explicit Windows DISPLAY source identity".into());
    }
    Ok(())
}
pub fn valid_size(width: u32, height: u32) -> Result<(), String> {
    if width == 0
        || height == 0
        || width > 16384
        || height > 16384
        || u64::from(width) * u64::from(height) * 4 > 256 * 1024 * 1024
    {
        return Err("Capture image exceeds dimension or 256MiB limit".into());
    }
    Ok(())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Dxgi,
    Gdi,
    /// Explicit software test pixels; never a desktop observation.
    Synthetic,
}
#[derive(Clone, Debug)]
pub struct Frame {
    pub image: Arc<Image>,
    /// Capture poll time, NOT proof of a new desktop presentation.
    pub observed_ms: u64,
    pub new_pixels: bool,
    pub backend: Backend,
    pub manual_crop: Option<Rect>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub frames: u64,
    pub source_readbacks: u64,
    pub readback_us: u64,
    pub scale_us: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_and_size_bounds() {
        assert!(validate_name(r"\\.\DISPLAY1").is_ok());
        assert!(validate_name(r"\\.\display32").is_ok());
        for bad in [
            "",
            "M32UC",
            r"\\.\DISPLAY0",
            r"\\.\DISPLAY1x",
            r"\\.\DISPLAY1\x",
        ] {
            assert!(validate_name(bad).is_err());
        }
        assert!(valid_size(16384, 4096).is_ok());
        assert!(valid_size(16384, 4097).is_err());
        assert!(valid_size(0, 1).is_err());
    }
}
