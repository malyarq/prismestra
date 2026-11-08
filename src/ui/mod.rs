//! Native operation surface. Initial UI is an explicit scene/map/schedule preview.
pub mod editor;
#[cfg(windows)]
pub mod paint;
pub mod stand;
#[cfg(windows)]
pub mod windows;
use crate::{app::config::Project, lighting::color::Rgb};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Scene,
    Devices,
    Connections,
    Schedule,
    Ambient,
    Lcd,
    Temperatures,
    Sources,
}
pub struct View<'a> {
    pub native_lighting: bool,
    pub project: &'a Project,
    pub colors: &'a [Rgb],
    pub page: Page,
    pub status: &'a str,
    pub dirty: bool,
    pub pc_on: bool,
    pub door_on: bool,
    pub detail: &'a str,
    pub lcd_preview: Option<Surface<'a>>,
    pub lcd_hint: &'a str,
    pub ambient_colors: Option<&'a [Rgb]>,
}
pub struct Surface<'a> {
    pub width: u32,
    pub height: u32,
    pub bgra: &'a [u8],
}
pub const MIN_WIDTH: i32 = 960;
pub const MIN_HEIGHT: i32 = 640;
pub const CONTENT_LEFT: i32 = 188;
pub fn right_panel(width_dip: i32) -> i32 {
    width_dip - 304
}
