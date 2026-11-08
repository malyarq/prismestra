//! One shared animation state for all canvas points; native step math lives in lighting.
use crate::{
    devices::Canvas,
    lighting::{
        color::Rgb,
        patterns::{Breathing, NoiseColors, NoiseMap, Playback, RainbowWave},
        scene::Effect,
        simplex::Fractal,
    },
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoiseSettings {
    pub frequency: f32,
    pub amplitude: f32,
    pub lacunarity: f32,
    pub persistence: f32,
    pub octaves: u8,
    pub motion: u8,
    pub motion_speed: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoisePalette {
    Custom,
    Rainbow,
    InverseRainbow,
}
impl NoiseSettings {
    fn fractal(&self) -> Fractal {
        Fractal {
            frequency: self.frequency,
            amplitude: self.amplitude,
            lacunarity: self.lacunarity,
            persistence: self.persistence,
            octaves: self.octaves,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    pub effect: Effect,
    /// Portable RRGGBB values, independent of OpenRGB's low-byte red encoding.
    pub colors: Vec<u32>,
    pub brightness: u8,
    /// Native effect speed, preserving imported values without a percentage round trip.
    pub speed: i32,
    pub fps: u32,
    pub temperature: i32,
    pub tint: i32,
    pub noise: NoiseSettings,
    pub noise_palette: NoisePalette,
    pub rainbow_width: i32,
}
impl Scene {
    /// Import only the supported native NoiseMap subset, without retaining any
    /// controller identities or changing native speed through percentage controls.
    pub fn from_native_noise(pattern: &serde_json::Value) -> Result<Self, String> {
        use serde_json::Value;
        crate::lighting::profile::safe_pattern(pattern)?;
        if pattern["EffectClassName"] != "NoiseMap"
            || pattern["RandomColors"].as_bool().unwrap_or(false)
            || pattern["AllowOnlyFirst"].as_bool().unwrap_or(false)
        {
            return Err("Unsupported native scene behavior".into());
        }
        let int = |v: &Value, key: &str| {
            v[key]
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .ok_or_else(|| format!("Missing/invalid native scene {key}"))
        };
        let custom = &pattern["CustomSettings"];
        let float = |key: &str| {
            custom[key]
                .as_f64()
                .map(|v| v as f32)
                .ok_or_else(|| format!("Missing native noise {key}"))
        };
        let colors = custom["colors"]
            .as_array()
            .ok_or("Missing native noise palette")?
            .iter()
            .map(|v| {
                v.as_u64()
                    .filter(|v| *v <= 0xffffff)
                    .map(|v| Rgb::from_native(v as u32).rrggbb())
                    .ok_or_else(|| "Invalid native palette color".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let to_u8 = |v: i32| {
            u8::try_from(v).map_err(|_| "Native scene value outside byte range".to_owned())
        };
        let scene = Self {
            effect: Effect::NoiseMap,
            colors,
            brightness: to_u8(int(pattern, "Brightness")?)?,
            speed: int(pattern, "Speed")?,
            fps: u32::try_from(int(pattern, "FPS")?).map_err(|_| "Invalid native FPS")?,
            temperature: int(pattern, "Temperature")?,
            tint: int(pattern, "Tint")?,
            noise: NoiseSettings {
                frequency: float("frequency")?,
                amplitude: float("amplitude")?,
                lacunarity: float("lacunarity")?,
                persistence: float("persistence")?,
                octaves: to_u8(int(custom, "octaves")?)?,
                motion: to_u8(int(custom, "motion")?)?,
                motion_speed: to_u8(int(custom, "motion_speed")?)?,
            },
            noise_palette: match int(custom, "colors_choice")? {
                0 => NoisePalette::Rainbow,
                1 => NoisePalette::InverseRainbow,
                2 => NoisePalette::Custom,
                _ => return Err("Unknown native noise color mode".into()),
            },
            rainbow_width: int(pattern, "Slider2Val")?,
        };
        scene.validate()?;
        Ok(scene)
    }
    pub fn validate(&self) -> Result<(), String> {
        let (lo, hi) = self.effect.limits();
        if !(lo..=hi).contains(&self.speed)
            || !(1..=144).contains(&self.fps)
            || self.brightness > 100
            || self.colors.is_empty()
            || self.colors.len() > 8
            || self.colors.iter().any(|c| *c > 0xffffff)
            || !(1..=50).contains(&self.rainbow_width)
        {
            return Err("Invalid scene settings".into());
        }
        Rgb::default()
            .adjusted(self.brightness as f32 / 100., self.temperature, self.tint)
            .map_err(str::to_owned)?;
        NoiseMap::new(
            self.noise.fractal(),
            self.noise.motion,
            self.noise.motion_speed,
            NoiseColors::from_palette(&self.palette()).map_err(str::to_owned)?,
        )
        .map_err(str::to_owned)?;
        Ok(())
    }
    fn palette(&self) -> Vec<Rgb> {
        self.colors.iter().map(|c| Rgb::from_rrggbb(*c)).collect()
    }
    fn noise_colors(&self) -> Result<NoiseColors, String> {
        match self.noise_palette {
            NoisePalette::Custom => {
                NoiseColors::from_palette(&self.palette()).map_err(str::to_owned)
            }
            NoisePalette::Rainbow => Ok(NoiseColors::Rainbow),
            NoisePalette::InverseRainbow => Ok(NoiseColors::InverseRainbow),
        }
    }
    fn playback(&self) -> Playback {
        Playback {
            speed: self.speed,
            fps: self.fps,
            brightness: self.brightness as f32 / 100.,
            temperature: self.temperature,
            tint: self.tint,
        }
    }
}
// One state per host, not per LED: retain its inline palette LUT without another allocation.
#[allow(clippy::large_enum_variant)]
enum State {
    Noise(NoiseMap),
    Breathing(Breathing),
    Rainbow(RainbowWave),
}
pub struct Renderer {
    scene: Scene,
    state: State,
}
impl Renderer {
    pub fn new(scene: Scene) -> Result<Self, String> {
        scene.validate()?;
        let state = match scene.effect {
            Effect::NoiseMap => State::Noise(
                NoiseMap::new(
                    scene.noise.fractal(),
                    scene.noise.motion,
                    scene.noise.motion_speed,
                    scene.noise_colors()?,
                )
                .map_err(str::to_owned)?,
            ),
            Effect::Breathing => State::Breathing(Breathing::default()),
            Effect::RainbowWave => State::Rainbow(RainbowWave::default()),
        };
        Ok(Self { scene, state })
    }
    pub fn scene(&self) -> &Scene {
        &self.scene
    }
    /// Palette/speed/brightness edits retain progress; structural noise/effect changes restart it.
    pub fn configure(&mut self, scene: Scene) -> Result<(), String> {
        scene.validate()?;
        if scene.effect != self.scene.effect || scene.noise != self.scene.noise {
            *self = Self::new(scene)?;
        } else {
            if (scene.colors != self.scene.colors
                || scene.noise_palette != self.scene.noise_palette)
                && let State::Noise(noise) = &mut self.state
            {
                noise.colors = scene.noise_colors()?;
            }
            self.scene = scene;
        }
        Ok(())
    }
    pub fn step(&mut self, canvas: &Canvas) -> Result<Vec<Rgb>, String> {
        if canvas.width == 0
            || canvas.height == 0
            || canvas.points.len() > 65536
            || canvas
                .points
                .iter()
                .any(|p| p.x >= canvas.width || p.y >= canvas.height)
        {
            return Err("Invalid render canvas".into());
        }
        let p = self.scene.playback();
        match &mut self.state {
            State::Noise(noise) => noise
                .step(
                    p,
                    &canvas.points.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>(),
                )
                .map_err(str::to_owned),
            State::Breathing(breathing) => {
                let color = breathing
                    .step(p, &self.scene.palette())
                    .map_err(str::to_owned)?;
                Ok(vec![color; canvas.points.len()])
            }
            State::Rainbow(rainbow) => {
                let columns = rainbow
                    .step(
                        p,
                        self.scene.rainbow_width,
                        canvas.width as usize,
                        false,
                        false,
                    )
                    .map_err(str::to_owned)?;
                Ok(canvas
                    .points
                    .iter()
                    .map(|point| columns[point.x as usize])
                    .collect())
            }
        }
    }
}
#[cfg(test)]
pub(crate) fn example() -> Scene {
    Scene {
        effect: Effect::NoiseMap,
        colors: vec![0x120100, 0x480500, 0x961801, 0xe64005, 0xff7814],
        brightness: 60,
        speed: 8,
        fps: 25,
        temperature: 0,
        tint: 0,
        noise: NoiseSettings {
            frequency: 0.055,
            amplitude: 1.,
            lacunarity: 0.75,
            persistence: 0.4,
            octaves: 2,
            motion: 0,
            motion_speed: 1,
        },
        noise_palette: NoisePalette::Custom,
        rainbow_width: 10,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::Point;
    #[test]
    fn one_state_shared_positions_and_invalid_edit_preserves_progress() {
        let canvas = Canvas {
            width: 20,
            height: 20,
            points: vec![
                Point {
                    device: "a".into(),
                    led: 0,
                    x: 2,
                    y: 3,
                },
                Point {
                    device: "b".into(),
                    led: 0,
                    x: 2,
                    y: 3,
                },
            ],
        };
        let mut a = Renderer::new(example()).unwrap();
        let mut b = Renderer::new(example()).unwrap();
        assert_eq!(a.step(&canvas).unwrap(), b.step(&canvas).unwrap());
        let mut invalid = example();
        invalid.noise.frequency = f32::NAN;
        assert!(a.configure(invalid).is_err());
        let frame = a.step(&canvas).unwrap();
        assert_eq!(frame, b.step(&canvas).unwrap());
        assert_eq!(frame[0], frame[1]);
    }
}
