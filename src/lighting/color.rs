// SPDX-License-Identifier: GPL-2.0-or-later
/// Channel order matches OpenRGB's low-byte red encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}
impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
    pub const fn native(self) -> u32 {
        self.r as u32 | ((self.g as u32) << 8) | ((self.b as u32) << 16)
    }
    pub const fn from_native(v: u32) -> Self {
        Self::new(v as u8, (v >> 8) as u8, (v >> 16) as u8)
    }
    pub const fn from_rrggbb(v: u32) -> Self {
        Self::new((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }
    pub const fn rrggbb(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | self.b as u32
    }
    /// C++ ColorUtils::apply_adjustments truncates before brightness.
    pub fn adjusted(
        self,
        brightness: f32,
        temperature: i32,
        tint: i32,
    ) -> Result<Self, &'static str> {
        if !brightness.is_finite()
            || !(0.0..=1.0).contains(&brightness)
            || !(-100..=100).contains(&temperature)
            || !(-100..=100).contains(&tint)
        {
            return Err("invalid adjustments");
        }
        let ch = |c: u8, a: f64| (((c as f64 * a) as i32).clamp(0, 255) as f32 * brightness) as u8;
        Ok(Self::new(
            ch(self.r, 1.0 + temperature as f64 / 255.0),
            ch(self.g, 1.0 + tint as f64 / 255.0),
            ch(self.b, 1.0 - temperature as f64 / 255.0),
        ))
    }
}
#[test]
fn encodings() {
    let c = Rgb::new(255, 96, 16);
    assert_eq!(c.native(), 0x1060ff);
    assert_eq!(c.rrggbb(), 0xff6010);
    assert_eq!(Rgb::from_native(c.native()), c);
    assert_eq!(c.adjusted(0.5, 0, 0).unwrap(), Rgb::new(127, 48, 8));
}
