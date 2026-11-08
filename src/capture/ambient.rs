//! Ambient processing in the original order; render smoothing consumes the
//! controller's last output, including when no new desktop frame has arrived.
use super::{Frame, pixels};
use crate::lighting::{
    ambient::{BorderDetector, Correction, CropSettings, Image, Rect, correct},
    color::Rgb,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Average,
    ScreenCopy,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Zone {
    pub mode: Mode,
    pub width: usize,
    pub height: usize,
    /// Row-major cell -> LED. None is an unpopulated matrix cell.
    pub map: Vec<Option<usize>>,
    pub leds: usize,
    pub reverse: bool,
}
impl Zone {
    pub fn linear(leds: usize, reverse: bool, mode: Mode) -> Result<Self, String> {
        if !(1..=65536).contains(&leds) {
            return Err("Invalid ambient LED count".into());
        }
        let zone = Self {
            mode,
            width: leds,
            height: 1,
            map: (0..leds).map(Some).collect(),
            leds,
            reverse,
        };
        zone.validate()?;
        Ok(zone)
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.leds == 0
            || self.leds > 65536
            || self.width == 0
            || self.height == 0
            || self.width.checked_mul(self.height) != Some(self.map.len())
            || self.map.len() > 65536
        {
            return Err("Invalid ambient zone dimensions".into());
        }
        let mut used = std::collections::HashSet::new();
        for led in self.map.iter().flatten() {
            if *led >= self.leds || !used.insert(*led) {
                return Err("Out-of-range or duplicate ambient LED".into());
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub borders: CropSettings,
    pub correction: Correction,
    pub blur: i32,
}
impl Settings {
    fn validate(self) -> Result<(), String> {
        correct(Rgb::default(), self.correction).map_err(str::to_owned)?;
        if !(0..=10).contains(&self.blur) {
            return Err("Ambient blur must be0..10".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSettings {
    pub smoothness: u8,
    pub brightness: f32,
    pub temperature: i32,
    pub tint: i32,
}
impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            smoothness: 80,
            brightness: 1.0,
            temperature: 0,
            tint: 0,
        }
    }
}
pub struct Processor {
    zone: Zone,
    settings: Settings,
    detector: BorderDetector,
    source: Option<Arc<Image>>,
    crop: Option<Rect>,
    targets: Option<Vec<Option<Rgb>>>,
    dirty: bool,
    pub prepared_frames: u64,
}
impl Processor {
    pub fn new(zone: Zone, settings: Settings) -> Result<Self, String> {
        zone.validate()?;
        settings.validate()?;
        Ok(Self {
            zone,
            settings,
            detector: BorderDetector::default(),
            source: None,
            crop: None,
            targets: None,
            dirty: true,
            prepared_frames: 0,
        })
    }
    pub fn configure(&mut self, zone: Zone, settings: Settings) -> Result<(), String> {
        zone.validate()?;
        settings.validate()?;
        self.zone = zone;
        self.settings = settings;
        self.detector = BorderDetector::default();
        self.dirty = true;
        // No stale targets after a change, even if preparation later fails.
        self.targets = None;
        Ok(())
    }
    pub fn reset(&mut self) {
        self.detector = BorderDetector::default();
        self.source = None;
        self.crop = None;
        self.targets = None;
        self.dirty = true;
    }
    pub fn accept(&mut self, frame: &Frame) -> Result<(), String> {
        let image = &frame.image;
        if image.width() > 320 || image.height() > 180 {
            return Err("Expected bounded ambient thumbnail".into());
        }
        let (sw, sh) = fit(image.width(), image.height(), 160, 90);
        let sample = pixels::fast_scale(image, sw, sh)?;
        let crop = self
            .detector
            .update(
                &sample,
                image.width(),
                image.height(),
                self.settings.borders,
            )
            .map_err(str::to_owned)?;
        if !self.dirty
            && self.crop == Some(crop)
            && self
                .source
                .as_ref()
                .is_some_and(|old| Arc::ptr_eq(old, image))
        {
            return Ok(());
        }
        let prepared = (|| {
            let mut values = Vec::with_capacity(crop.width * crop.height);
            for row in crop.y..crop.y + crop.height {
                values.extend_from_slice(
                    &image.pixels()
                        [row * image.width() + crop.x..row * image.width() + crop.x + crop.width],
                );
            }
            let processed = Image::new(crop.width, crop.height, values)
                .map_err(str::to_owned)?
                .blur(self.settings.blur);
            if self.zone.mode == Mode::Average {
                let sample = pixels::smooth_scale(&processed, 1, 1)?;
                let color =
                    correct(sample.pixels()[0], self.settings.correction).map_err(str::to_owned)?;
                return Ok(vec![Some(color); self.zone.leds]);
            }
            let sample = pixels::fast_scale(&processed, self.zone.width, self.zone.height)?;
            let mut colors = vec![None; self.zone.leds];
            for (cell, led) in self.zone.map.iter().enumerate() {
                if let Some(led) = led {
                    let (x, y) = (cell % self.zone.width, cell / self.zone.width);
                    let x = if self.zone.reverse {
                        self.zone.width - x - 1
                    } else {
                        x
                    };
                    colors[*led] = Some(
                        correct(
                            sample.pixels()[y * self.zone.width + x],
                            self.settings.correction,
                        )
                        .map_err(str::to_owned)?,
                    );
                }
            }
            Ok::<_, String>(colors)
        })();
        let targets = match prepared {
            Ok(t) => t,
            Err(e) => {
                self.targets = None;
                self.dirty = true;
                return Err(e);
            }
        };
        self.targets = Some(targets);
        self.source = Some(image.clone());
        self.crop = Some(crop);
        self.dirty = false;
        self.prepared_frames = self.prepared_frames.saturating_add(1);
        Ok(())
    }
    /// None means no valid image, so the host must not submit a hardware update.
    pub fn render(
        &self,
        previous: &[Rgb],
        settings: OutputSettings,
    ) -> Result<Option<Vec<Rgb>>, String> {
        if previous.len() != self.zone.leds || settings.smoothness > 100 {
            return Err("Invalid ambient output dimensions/smoothness".into());
        }
        Rgb::default()
            .adjusted(settings.brightness, settings.temperature, settings.tint)
            .map_err(str::to_owned)?;
        let Some(targets) = &self.targets else {
            return Ok(None);
        };
        let fraction = (0.01f64 * f64::from(100 - settings.smoothness)) as f32;
        let mut result = previous.to_vec();
        for (led, target) in targets.iter().enumerate() {
            if let Some(target) = target {
                let previous = if self.zone.mode == Mode::Average {
                    previous[0]
                } else {
                    previous[led]
                };
                let lerp = |a: u8, b: u8| {
                    ((i32::from(b) - i32::from(a)) as f32 * fraction + f32::from(a)) as u8
                };
                let color = Rgb::new(
                    lerp(previous.r, target.r),
                    lerp(previous.g, target.g),
                    lerp(previous.b, target.b),
                );
                result[led] = color
                    .adjusted(settings.brightness, settings.temperature, settings.tint)
                    .map_err(str::to_owned)?;
            }
        }
        Ok(Some(result))
    }
}
fn fit(width: usize, height: usize, max_width: usize, max_height: usize) -> (usize, usize) {
    if max_height * width / height <= max_width {
        ((max_height * width / height).max(1), max_height)
    } else {
        (max_width, (max_width * height / width).max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::Backend;
    fn frame(pixels: Vec<Rgb>) -> Frame {
        Frame {
            image: Arc::new(Image::new(pixels.len(), 1, pixels).unwrap()),
            observed_ms: 1,
            new_pixels: true,
            backend: Backend::Dxgi,
            manual_crop: None,
        }
    }
    #[test]
    fn smoothing_uses_each_previous_controller_output_and_cached_targets() {
        let mut processor = Processor::new(
            Zone::linear(2, false, Mode::ScreenCopy).unwrap(),
            Settings::default(),
        )
        .unwrap();
        let frame = frame(vec![Rgb::new(200, 100, 50); 2]);
        assert!(
            processor
                .render(&[Rgb::default(); 2], OutputSettings::default())
                .unwrap()
                .is_none()
        );
        processor.accept(&frame).unwrap();
        processor.accept(&frame).unwrap();
        assert_eq!(processor.prepared_frames, 1);
        let settings = OutputSettings {
            smoothness: 50,
            ..Default::default()
        };
        let one = processor
            .render(&[Rgb::default(); 2], settings)
            .unwrap()
            .unwrap();
        assert_eq!(one, [Rgb::new(100, 50, 25); 2]);
        let two = processor.render(&one, settings).unwrap().unwrap();
        assert_eq!(two, [Rgb::new(150, 75, 37); 2]);
        processor.reset();
        assert!(processor.render(&two, settings).unwrap().is_none());
    }
    #[test]
    fn matrix_reverse_x_holes_and_validation_before_change() {
        let zone = Zone {
            mode: Mode::ScreenCopy,
            width: 2,
            height: 1,
            map: vec![Some(0), None],
            leds: 2,
            reverse: true,
        };
        let mut processor = Processor::new(zone.clone(), Settings::default()).unwrap();
        processor
            .accept(&frame(vec![Rgb::new(10, 0, 0), Rgb::new(200, 0, 0)]))
            .unwrap();
        let old = [Rgb::default(), Rgb::new(42, 42, 42)];
        let settings = OutputSettings {
            smoothness: 0,
            ..Default::default()
        };
        assert_eq!(
            processor.render(&old, settings).unwrap().unwrap(),
            [Rgb::new(200, 0, 0), old[1]]
        );
        let mut invalid = zone;
        invalid.map = vec![Some(0), Some(0)];
        assert!(processor.configure(invalid, Settings::default()).is_err());
        assert_eq!(
            processor.render(&old, settings).unwrap().unwrap(),
            [Rgb::new(200, 0, 0), old[1]]
        );
    }
    #[test]
    fn average_uses_led_zero_and_invalid_configuration_is_atomic() {
        let mut processor = Processor::new(
            Zone::linear(2, false, Mode::Average).unwrap(),
            Settings::default(),
        )
        .unwrap();
        processor
            .accept(&frame(vec![Rgb::new(100, 100, 100); 2]))
            .unwrap();
        let result = processor
            .render(
                &[Rgb::new(20, 20, 20), Rgb::new(200, 200, 200)],
                OutputSettings {
                    smoothness: 50,
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(result, [Rgb::new(60, 60, 60); 2]);
        assert!(Zone::linear(65537, false, Mode::ScreenCopy).is_err());
        assert!(Zone::linear(usize::MAX, false, Mode::ScreenCopy).is_err());
    }
}
