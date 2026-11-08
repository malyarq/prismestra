// SPDX-License-Identifier: GPL-2.0-or-later
// Typed control math from effects-source/SceneEffects.cpp; no JSON profile mutation.
use super::color::Rgb;
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    NoiseMap,
    Breathing,
    RainbowWave,
}
impl Effect {
    pub fn limits(self) -> (i32, i32) {
        match self {
            Self::Breathing => (10, 200),
            _ => (1, 100),
        }
    }
    pub fn native_speed(self, normalized: i32) -> Result<i32, &'static str> {
        if !(0..=100).contains(&normalized) {
            return Err("invalid speed");
        };
        let (lo, hi) = self.limits();
        Ok(lo + (normalized as f64 * (hi - lo) as f64 / 100.0).round() as i32)
    }
    pub fn normalized_speed(self, native: i32) -> Result<i32, &'static str> {
        let (lo, hi) = self.limits();
        if !(lo..=hi).contains(&native) {
            return Err("invalid native speed");
        };
        Ok((100.0 * (native - lo) as f64 / (hi - lo) as f64).round() as i32)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Controls {
    pub effect: Effect,
    pub brightness: u8,
    pub speed: u8,
    pub colors: Vec<Rgb>,
    pub screen_brightness: u8,
}
impl Controls {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.brightness > 100
            || self.speed > 100
            || self.screen_brightness > 100
            || self.colors.is_empty()
            || self.colors.len() > 8
        {
            Err("invalid scene controls")
        } else {
            Ok(())
        }
    }
}
#[test]
fn speed() {
    assert_eq!(Effect::Breathing.native_speed(50), Ok(105));
    assert_eq!(Effect::NoiseMap.native_speed(50), Ok(51));
    assert_eq!(Effect::RainbowWave.normalized_speed(51), Ok(51));
    assert!(Effect::Breathing.native_speed(101).is_err());
}
