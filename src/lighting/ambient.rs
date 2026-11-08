// SPDX-License-Identifier: GPL-2.0-or-later
// Port of AmbientProcessing.h; caller supplies a bounded processing image.
use super::color::Rgb;
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Correction {
    pub saturation: f64,
    pub gamma: f64,
    pub kelvin: i32,
    pub gains: [f64; 3],
}
impl Default for Correction {
    fn default() -> Self {
        Self {
            saturation: 1.0,
            gamma: 1.0,
            kelvin: 6500,
            gains: [1.0; 3],
        }
    }
}
pub fn kelvin_scales(kelvin: i32) -> [f64; 3] {
    let t = kelvin.clamp(2000, 10000) as f64 / 100.0;
    let r = if t <= 66.0 {
        255.0
    } else {
        329.698727446 * (t - 60.0).powf(-0.1332047592)
    };
    let g = if t <= 66.0 {
        99.4708025861 * t.ln() - 161.1195681661
    } else {
        288.1221695283 * (t - 60.0).powf(-0.0755148492)
    };
    let b = if t >= 66.0 {
        255.0
    } else {
        138.5177312231 * (t - 10.0).ln() - 305.0447927307
    };
    [r, g, b].map(|c| c.clamp(0.0, 255.0) / 255.0)
}
pub fn correct(color: Rgb, s: Correction) -> Result<Rgb, &'static str> {
    if !s.saturation.is_finite() || !s.gamma.is_finite() || s.gains.iter().any(|x| !x.is_finite()) {
        return Err("non-finite correction");
    }
    let c = [color.r, color.g, color.b].map(|c| c as f64 / 255.0);
    let luma = 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
    let neutral = kelvin_scales(6500);
    let scales = kelvin_scales(s.kelvin);
    let mut out = [0; 3];
    for i in 0..3 {
        let v = (luma + (c[i] - luma) * s.saturation.clamp(0.0, 3.0)).clamp(0.0, 1.0);
        let v = (v * scales[i] / neutral[i]).clamp(0.0, 1.0);
        let v = v.powf(s.gamma.clamp(0.1, 4.0)) * s.gains[i].clamp(0.0, 2.0);
        out[i] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    Ok(Rgb::new(out[0], out[1], out[2]))
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    width: usize,
    height: usize,
    pixels: Vec<Rgb>,
}
impl Image {
    pub fn new(width: usize, height: usize, pixels: Vec<Rgb>) -> Result<Self, &'static str> {
        if width == 0 || height == 0 || width.checked_mul(height) != Some(pixels.len()) {
            return Err("invalid image dimensions");
        };
        Ok(Self {
            width,
            height,
            pixels,
        })
    }
    pub fn width(&self) -> usize {
        self.width
    }
    pub fn height(&self) -> usize {
        self.height
    }
    pub fn pixels(&self) -> &[Rgb] {
        &self.pixels
    }
    pub fn blur(&self, radius: i32) -> Self {
        let r = radius.clamp(0, 10) as usize;
        if r == 0 {
            return self.clone();
        };
        let mut horizontal = self.clone();
        let mut out = self.clone();
        for y in 0..self.height {
            for x in 0..self.width {
                horizontal.pixels[y * self.width + x] = average(
                    (x.saturating_sub(r)..=x.saturating_add(r).min(self.width - 1))
                        .map(|k| self.pixels[y * self.width + k]),
                );
            }
        }
        for y in 0..self.height {
            for x in 0..self.width {
                out.pixels[y * self.width + x] = average(
                    (y.saturating_sub(r)..=y.saturating_add(r).min(self.height - 1))
                        .map(|k| horizontal.pixels[k * self.width + x]),
                );
            }
        }
        out
    }
}
fn average(pixels: impl Iterator<Item = Rgb>) -> Rgb {
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    for c in pixels {
        r += c.r as u64;
        g += c.g as u64;
        b += c.b as u64;
        n += 1;
    }
    Rgb::new((r / n) as u8, (g / n) as u8, (b / n) as u8)
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CropSettings {
    pub enabled: bool,
    pub black_threshold: i32,
    pub stable_frames: i32,
    pub unknown_frames: i32,
}
impl Default for CropSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            black_threshold: 5,
            stable_frames: 50,
            unknown_frames: 600,
        }
    }
}
#[derive(Default)]
pub struct BorderDetector {
    size: (usize, usize),
    current: Rect,
    pending: Option<Rect>,
    count: u32,
    unknown: u32,
}
impl BorderDetector {
    /// Same crop state machine as C++; sample must be the caller's <=160x90 thumbnail.
    /// Qt thumbnail scaling itself is deliberately outside this pure math function.
    pub fn update(
        &mut self,
        sample: &Image,
        frame_width: usize,
        frame_height: usize,
        s: CropSettings,
    ) -> Result<Rect, &'static str> {
        if frame_width == 0
            || frame_height == 0
            || frame_width > i32::MAX as usize
            || frame_height > i32::MAX as usize
            || sample.width > 160
            || sample.height > 90
        {
            return Err("invalid crop dimensions");
        }
        let full = Rect {
            x: 0,
            y: 0,
            width: frame_width,
            height: frame_height,
        };
        if self.size != (frame_width, frame_height) {
            *self = Self::default();
            self.size = (frame_width, frame_height);
            self.current = full;
        }
        if !s.enabled {
            *self = Self::default();
            return Ok(full);
        }
        let threshold = (s.black_threshold.clamp(0, 100) * 255 / 100) as u8;
        let lit = |x: usize, y: usize| {
            let c = sample.pixels[y * sample.width + x];
            c.r > threshold || c.g > threshold || c.b > threshold
        };
        let row_black = |y| !(0..sample.width).any(|x| lit(x, y));
        let col_black = |x| !(0..sample.height).any(|y| lit(x, y));
        let mut top = 0;
        while top < sample.height && row_black(top) {
            top += 1;
        }
        if top == sample.height {
            self.pending = None;
            self.count = 0;
            self.unknown = self.unknown.saturating_add(1);
            if self.unknown >= s.unknown_frames.clamp(1, 2000) as u32 {
                self.current = full
            }
            return Ok(self.current);
        }
        self.unknown = 0;
        let (mut bottom, mut left, mut right) = (sample.height - 1, 0, sample.width - 1);
        while bottom > top && row_black(bottom) {
            bottom -= 1
        }
        while left <= right && col_black(left) {
            left += 1
        }
        while right > left && col_black(right) {
            right -= 1
        }
        if right - left + 1 < sample.width / 3 || bottom - top + 1 < sample.height / 3 {
            self.pending = None;
            self.count = 0;
            return Ok(self.current);
        }
        let x = left * frame_width / sample.width;
        let y = top * frame_height / sample.height;
        let detected = Rect {
            x,
            y,
            width: (right + 1) * frame_width / sample.width - x,
            height: (bottom + 1) * frame_height / sample.height - y,
        };
        if self.pending != Some(detected) {
            self.pending = Some(detected);
            self.count = 1
        } else {
            self.count = self.count.saturating_add(1)
        }
        if self.count >= s.stable_frames.clamp(1, 200) as u32 {
            self.current = detected;
        }
        Ok(self.current)
    }
}
#[test]
fn correction() {
    let c = Rgb::new(45, 128, 201);
    assert_eq!(correct(c, Correction::default()), Ok(c));
    let s = Correction {
        saturation: 0.0,
        ..Correction::default()
    };
    assert_eq!(correct(c, s), Ok(Rgb::new(112, 112, 112)));
    assert!(
        correct(
            c,
            Correction {
                gamma: f64::NAN,
                ..Correction::default()
            }
        )
        .is_err()
    );
    assert_eq!(kelvin_scales(100), kelvin_scales(2000));
}
#[test]
fn box_blur() {
    let image = Image::new(
        3,
        1,
        vec![Rgb::default(), Rgb::new(255, 0, 0), Rgb::default()],
    )
    .unwrap();
    assert_eq!(
        image.blur(1).pixels,
        vec![Rgb::new(127, 0, 0), Rgb::new(85, 0, 0), Rgb::new(127, 0, 0)]
    );
    assert!(Image::new(2, 2, vec![]).is_err());
}
#[test]
fn crop_stability() {
    let mut d = BorderDetector::default();
    let mut p = vec![Rgb::default(); 16];
    for y in 1..3 {
        for x in 0..4 {
            p[y * 4 + x] = Rgb::new(255, 255, 255);
        }
    }
    let image = Image::new(4, 4, p).unwrap();
    let s = CropSettings {
        enabled: true,
        stable_frames: 2,
        unknown_frames: 2,
        ..CropSettings::default()
    };
    assert_eq!(d.update(&image, 400, 400, s).unwrap().height, 400);
    assert_eq!(
        d.update(&image, 400, 400, s).unwrap(),
        Rect {
            x: 0,
            y: 100,
            width: 400,
            height: 200
        }
    );
    let black = Image::new(4, 4, vec![Rgb::default(); 16]).unwrap();
    assert_eq!(d.update(&black, 400, 400, s).unwrap().height, 200);
    assert_eq!(d.update(&black, 400, 400, s).unwrap().height, 400);
}
