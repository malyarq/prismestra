// SPDX-License-Identifier: GPL-2.0-or-later
// Pure step math from Effects/{Breathing,RainbowWave,NoiseMap}.
use super::{
    color::Rgb,
    hsv::{self, Hsv},
    simplex::Fractal,
};
// The native effect uses this decimal literal; full PI changes frame parity.
#[allow(clippy::approx_constant)]
const NATIVE_BREATHING_PI: f64 = 3.14159;

#[derive(Clone, Copy, Debug)]
pub struct Playback {
    pub speed: i32,
    pub fps: u32,
    pub brightness: f32,
    pub temperature: i32,
    pub tint: i32,
}
impl Playback {
    fn validate(self, min: i32, max: i32) -> Result<(), &'static str> {
        if !(min..=max).contains(&self.speed) || !(1..=144).contains(&self.fps) {
            return Err("invalid playback");
        };
        Rgb::default().adjusted(self.brightness, self.temperature, self.tint)?;
        Ok(())
    }
    fn adjust(self, c: Rgb) -> Result<Rgb, &'static str> {
        c.adjusted(self.brightness, self.temperature, self.tint)
    }
}
pub struct Breathing {
    progress: f32,
    current: Hsv,
    index: usize,
}
impl Default for Breathing {
    fn default() -> Self {
        Self {
            progress: NATIVE_BREATHING_PI as f32,
            current: Hsv {
                hue: 0,
                saturation: 0,
                value: 0,
            },
            index: 0,
        }
    }
}
impl Breathing {
    pub fn step(&mut self, p: Playback, palette: &[Rgb]) -> Result<Rgb, &'static str> {
        p.validate(10, 200)?;
        if palette.is_empty() || palette.len() > 8 {
            return Err("invalid palette");
        }
        self.progress =
            ((self.progress as f64) + ((p.speed as f64 / 100.0) / (p.fps as f32) as f64)) as f32;
        if self.progress as f64 >= NATIVE_BREATHING_PI {
            self.progress = (self.progress as f64 - NATIVE_BREATHING_PI) as f32;
            if self.index >= palette.len() {
                self.index = 0
            }
            self.current = hsv::from_rgb(palette[self.index]);
            self.index = (self.index + 1) % palette.len();
        }
        self.current.value = ((self.progress.sin() as f64).powi(3) * 255.0) as u8;
        p.adjust(hsv::to_rgb(self.current))
    }
}
#[derive(Default)]
pub struct RainbowWave {
    progress: f32,
}
impl RainbowWave {
    /// Matrix callers use column count, replicate columns through their explicit map.
    pub fn step(
        &mut self,
        p: Playback,
        width: i32,
        positions: usize,
        reverse: bool,
        single: bool,
    ) -> Result<Vec<Rgb>, &'static str> {
        p.validate(1, 100)?;
        if !(1..=50).contains(&width) || positions > 1_000_000 {
            return Err("invalid rainbow geometry");
        }
        let mut out = Vec::with_capacity(positions);
        for i in 0..positions {
            let pos = if single {
                0
            } else if reverse {
                positions - 1 - i
            } else {
                i
            };
            let hue = ((self.progress + pos as f32) * width as f32) as u32;
            out.push(p.adjust(hsv::to_rgb(Hsv {
                hue,
                saturation: 255,
                value: 255,
            }))?);
        }
        if self.progress < 360.0 {
            self.progress += p.speed as f32 / p.fps as f32
        } else {
            self.progress = 0.0
        }
        Ok(out)
    }
}
// Keep the 300-byte LUT inline: palette edits and frame sampling need no heap allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy)]
pub enum NoiseColors {
    Rainbow,
    InverseRainbow,
    Gradient([Rgb; 100]),
}
impl NoiseColors {
    pub fn from_palette(palette: &[Rgb]) -> Result<Self, &'static str> {
        Ok(Self::Gradient(super::gradient::noise_palette_lut(palette)?))
    }
}
pub struct NoiseMap {
    progress: f64,
    noise: Fractal,
    motion: u8,
    motion_speed: u8,
    pub colors: NoiseColors,
}
impl NoiseMap {
    /// Palette editor updates retain playback progress. Invalid edits change nothing.
    pub fn set_palette(&mut self, palette: &[Rgb]) -> Result<(), &'static str> {
        let colors = NoiseColors::from_palette(palette)?;
        self.colors = colors;
        Ok(())
    }

    pub fn new(
        noise: Fractal,
        motion: u8,
        motion_speed: u8,
        colors: NoiseColors,
    ) -> Result<Self, &'static str> {
        if motion > 3 || motion_speed > 99 || noise.octaves == 0 || noise.octaves > 20 {
            return Err("invalid noise settings");
        }
        for (v, max) in [
            (noise.frequency, 0.5),
            (noise.amplitude, 5.0),
            (noise.lacunarity, 5.0),
            (noise.persistence, 5.0),
        ] {
            if !v.is_finite() || v < 0.000099 || v > max + 0.000001 {
                return Err("invalid fractal parameter");
            }
        }
        Ok(Self {
            progress: 0.0,
            noise,
            motion,
            motion_speed,
            colors,
        })
    }
    pub fn step(
        &mut self,
        p: Playback,
        coordinates: &[(u32, u32)],
    ) -> Result<Vec<Rgb>, &'static str> {
        p.validate(1, 100)?;
        if coordinates.len() > 1_000_000 {
            return Err("oversized noise frame");
        };
        let shift = (self.motion_speed as f64 * self.progress) as f32;
        let (xs, ys) = match self.motion {
            0 => (0.0, shift),
            1 => (0.0, -shift),
            2 => (shift, 0.0),
            _ => (-shift, 0.0),
        };
        let mut out = Vec::with_capacity(coordinates.len());
        for &(x, y) in coordinates {
            let value = self
                .noise
                .value(x as f32 + xs, y as f32 + ys, self.progress as f32);
            if !value.is_finite() {
                return Err("noise coordinate exceeds supported arithmetic range");
            }
            let frac = (1.0 + value) * 0.5;
            let c = match &self.colors {
                NoiseColors::Rainbow => hsv::to_rgb(Hsv {
                    hue: (360.0 * frac) as u32,
                    saturation: 255,
                    value: 255,
                }),
                NoiseColors::InverseRainbow => hsv::to_rgb(Hsv {
                    hue: (360.0 - 360.0 * frac) as u32,
                    saturation: 255,
                    value: 255,
                }),
                NoiseColors::Gradient(lut) => super::gradient::sample_noise_lut(lut, frac)?,
            };
            out.push(p.adjust(c)?)
        }
        self.progress += 0.1 * p.speed as f32 as f64 / p.fps as f32 as f64;
        Ok(out)
    }
}
#[test]
fn patterns() {
    let p = Playback {
        speed: 100,
        fps: 60,
        brightness: 1.0,
        temperature: 0,
        tint: 0,
    };
    let mut breathing = Breathing::default();
    let mut max = 0;
    for _ in 0..200 {
        max = max.max(breathing.step(p, &[Rgb::new(255, 0, 0)]).unwrap().r)
    }
    assert!(max > 250);
    let mut rainbow = RainbowWave::default();
    assert_eq!(
        rainbow.step(p, 10, 1, false, true).unwrap(),
        vec![Rgb::new(255, 0, 0)]
    );
    let noise = Fractal {
        frequency: 0.12,
        amplitude: 3.9,
        lacunarity: 0.75,
        persistence: 0.5,
        octaves: 2,
    };
    let mut n = NoiseMap::new(noise, 0, 0, NoiseColors::Rainbow).unwrap();
    assert_eq!(
        n.step(p, &[(0, 0)]).unwrap(),
        vec![hsv::to_rgb(Hsv {
            hue: 180,
            saturation: 255,
            value: 255
        })]
    );
}

#[test]
fn cpp_step_parity() {
    let data = include_bytes!("fixtures/patterns-fixtures.bin");
    let mut cursor = 0;
    let mut next = || {
        let v = u32::from_le_bytes(data[cursor..cursor + 4].try_into().unwrap());
        cursor += 4;
        v
    };
    for speed in [10, 100, 200] {
        for fps in [10, 48, 60, 144] {
            let mut effect = Breathing::default();
            let p = Playback {
                speed,
                fps,
                brightness: 1.0,
                temperature: 0,
                tint: 0,
            };
            for frame in 0..1000 {
                let c = effect
                    .step(
                        p,
                        &[
                            Rgb::from_native(0x0055ff),
                            Rgb::from_native(0x00c8ff),
                            Rgb::from_native(0x0000c8),
                        ],
                    )
                    .unwrap();
                assert_eq!(
                    c.native(),
                    next(),
                    "breathing speed={speed} fps={fps} frame={frame}"
                );
            }
        }
    }
    for speed in [1, 40, 100] {
        for fps in [10, 48, 60, 144] {
            let mut effect = RainbowWave::default();
            let p = Playback {
                speed,
                fps,
                brightness: 1.0,
                temperature: 0,
                tint: 0,
            };
            for frame in 0..1000 {
                let row = effect.step(p, 10, 8, false, false).unwrap();
                for c in row {
                    assert_eq!(
                        c.native(),
                        next(),
                        "rainbow speed={speed} fps={fps} frame={frame}"
                    );
                }
            }
        }
    }
    assert_eq!(cursor, data.len());
}

#[test]
fn cpp_noise_parity() {
    let data = include_bytes!("fixtures/noise-fixtures.bin");
    let gradients = include_bytes!("fixtures/gradient-fixtures.bin");
    let mut lut = [Rgb::default(); 100];
    for (i, c) in gradients[600..900].as_chunks::<3>().0.iter().enumerate() {
        lut[i] = Rgb::new(c[0], c[1], c[2])
    }
    let mut cursor = 0;
    let coords: Vec<_> = (0..4).flat_map(|y| (0..8).map(move |x| (x, y))).collect();
    for choice in 0..3 {
        for motion in 0..4 {
            let noise = Fractal {
                frequency: 0.12,
                amplitude: 3.9,
                lacunarity: 0.75,
                persistence: 0.5,
                octaves: 2,
            };
            let colors = match choice {
                0 => NoiseColors::Rainbow,
                1 => NoiseColors::InverseRainbow,
                _ => NoiseColors::Gradient(lut),
            };
            let mut effect = NoiseMap::new(noise, motion, 7, colors).unwrap();
            let p = Playback {
                speed: 40,
                fps: 48,
                brightness: 1.0,
                temperature: 0,
                tint: 0,
            };
            for frame in 0..200 {
                for c in effect.step(p, &coords).unwrap() {
                    let expected = u32::from_le_bytes(data[cursor..cursor + 4].try_into().unwrap());
                    cursor += 4;
                    assert_eq!(
                        c.native(),
                        expected,
                        "choice={choice} motion={motion} frame={frame}"
                    );
                }
            }
        }
    }
    assert_eq!(cursor, data.len());
}

#[test]
fn palette_edit_preserves_playback() {
    let f = Fractal {
        frequency: 0.12,
        amplitude: 3.9,
        lacunarity: 0.75,
        persistence: 0.5,
        octaves: 2,
    };
    let palette = [Rgb::new(255, 0, 0), Rgb::new(0, 0, 255)];
    let mut a = NoiseMap::new(f, 0, 7, NoiseColors::from_palette(&palette).unwrap()).unwrap();
    let mut b = NoiseMap::new(f, 0, 7, NoiseColors::from_palette(&palette).unwrap()).unwrap();
    let p = Playback {
        speed: 40,
        fps: 48,
        brightness: 1.0,
        temperature: 0,
        tint: 0,
    };
    let coords = [(0, 0), (1, 2), (7, 3)];
    a.step(p, &coords).unwrap();
    b.step(p, &coords).unwrap();
    let progress = a.progress;
    assert!(a.set_palette(&[]).is_err());
    assert_eq!(a.progress, progress);
    a.set_palette(&palette).unwrap();
    assert_eq!(a.progress, progress);
    assert_eq!(a.step(p, &coords).unwrap(), b.step(p, &coords).unwrap());
    let c = Rgb::new(29, 45, 199);
    a.set_palette(&[c]).unwrap();
    assert_eq!(a.step(p, &coords).unwrap(), vec![c; 3]);
}
