//! App-owned ambient fan-out. Providers are injected; construction never captures a desktop.
use crate::{
    capture::{
        self, Frame,
        ambient::{OutputSettings, Processor, Settings, Zone},
    },
    devices::{Role, Setup},
    lighting::{ambient::Rect, color::Rgb},
    runtime::Host,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Portable logical identity. The actual Windows monitor belongs in local bindings.
    pub key: String,
    pub rate_hz: u32,
    pub crop: Option<Rect>,
    pub stale_after_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub device: String,
    pub source: String,
    pub enabled: bool,
    pub zone: Zone,
    pub processing: Settings,
    pub output: OutputSettings,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub sources: Vec<Source>,
    pub routes: Vec<Route>,
}
pub(crate) fn key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
impl Config {
    pub fn validate(&self, setup: &Setup) -> Result<(), String> {
        setup.validate()?;
        if self.sources.len() > 16 || self.routes.len() > crate::devices::MAX_DEVICES {
            return Err("Ambient source/route limit exceeded".into());
        }
        let mut sources = BTreeSet::new();
        for s in &self.sources {
            if !key(&s.key)
                || !sources.insert(&s.key)
                || !(1..=144).contains(&s.rate_hz)
                || !(100..=60_000).contains(&s.stale_after_ms)
            {
                return Err("Invalid or duplicate ambient source".into());
            }
            if let Some(r) = s.crop {
                capture::valid_size(
                    r.width.try_into().map_err(|_| "Crop width overflow")?,
                    r.height.try_into().map_err(|_| "Crop height overflow")?,
                )?;
                if r.x.checked_add(r.width).is_none_or(|v| v > 16384)
                    || r.y.checked_add(r.height).is_none_or(|v| v > 16384)
                {
                    return Err("Ambient crop exceeds capture bounds".into());
                }
            }
        }
        let mut routed = BTreeSet::new();
        for r in &self.routes {
            let d = setup
                .devices
                .iter()
                .find(|d| d.id == r.device && d.role == Role::Ambient)
                .ok_or("Ambient route references a non-ambient device")?;
            if !routed.insert(&r.device) || !sources.contains(&r.source) || r.zone.leds != d.leds {
                return Err("Invalid/duplicate ambient routing or LED count".into());
            }
            let b = r.processing.borders;
            let c = r.processing.correction;
            if !(0..=100).contains(&b.black_threshold)
                || !(1..=200).contains(&b.stable_frames)
                || !(1..=2000).contains(&b.unknown_frames)
                || !(0.0..=3.0).contains(&c.saturation)
                || !(0.1..=4.0).contains(&c.gamma)
                || !(2000..=10000).contains(&c.kelvin)
                || c.gains.iter().any(|v| !(0.0..=2.0).contains(v))
                || r.output.smoothness > 100
            {
                return Err("Ambient processing setting outside its supported range".into());
            }
            Processor::new(r.zone.clone(), r.processing)?;
            Rgb::default()
                .adjusted(r.output.brightness, r.output.temperature, r.output.tint)
                .map_err(str::to_owned)?;
        }
        for d in setup
            .devices
            .iter()
            .filter(|d| d.enabled && d.role == Role::Ambient)
        {
            if !self.routes.iter().any(|r| r.device == d.id && r.enabled) {
                return Err("Enabled ambient device has no enabled route".into());
            }
        }
        Ok(())
    }
}
pub trait Provider {
    /// None means no observation, not a valid black frame. Cached static pixels may be
    /// returned as a fresh successful observation with new_pixels=false.
    fn poll(
        &mut self,
        source: &str,
        crop: Option<Rect>,
        requested_ms: u64,
    ) -> Result<Option<Frame>, String>;
}
impl<T: Provider + ?Sized> Provider for Box<T> {
    fn poll(
        &mut self,
        source: &str,
        crop: Option<Rect>,
        requested_ms: u64,
    ) -> Result<Option<Frame>, String> {
        (**self).poll(source, crop, requested_ms)
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceStatus {
    pub observed_ms: Option<u64>,
    pub error: Option<String>,
    pub polls: u64,
}
#[derive(Default)]
struct Cadence(Option<u64>);
impl Cadence {
    fn due(&mut self, now: u64, hz: u32) -> bool {
        let period = 1_000_000u64.div_ceil(u64::from(hz));
        if self.0.is_some_and(|due| now < due) {
            return false;
        }
        self.0 = Some(match self.0 {
            Some(due) => due.saturating_add(((now - due) / period + 1).saturating_mul(period)),
            None => now.saturating_add(period),
        });
        true
    }
}
struct Output {
    processor: Processor,
    previous: Vec<Rgb>,
    cadence: Cadence,
    hz: u32,
    cleared: bool,
    changed: bool,
}
pub struct Controller {
    config: Config,
    sources: BTreeMap<String, (SourceStatus, Cadence)>,
    outputs: Vec<Output>,
    last_us: Option<u64>,
    active: bool,
}
impl Controller {
    pub fn new(config: Config, setup: &Setup) -> Result<Self, String> {
        config.validate(setup)?;
        let outputs = config
            .routes
            .iter()
            .map(|r| {
                Ok(Output {
                    processor: Processor::new(r.zone.clone(), r.processing)?,
                    previous: vec![Rgb::default(); r.zone.leds],
                    cadence: Cadence::default(),
                    hz: setup
                        .devices
                        .iter()
                        .find(|d| d.id == r.device)
                        .unwrap()
                        .rate_hz,
                    cleared: false,
                    changed: false,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let sources = config
            .sources
            .iter()
            .map(|s| (s.key.clone(), (SourceStatus::default(), Cadence::default())))
            .collect();
        Ok(Self {
            config,
            sources,
            outputs,
            last_us: None,
            active: false,
        })
    }
    pub fn config(&self) -> &Config {
        &self.config
    }
    pub fn source_status(&self, key: &str) -> Option<&SourceStatus> {
        self.sources.get(key).map(|s| &s.0)
    }
    pub fn colors(&self, device: &str) -> Option<&[Rgb]> {
        self.config
            .routes
            .iter()
            .position(|r| r.device == device)
            .map(|i| self.outputs[i].previous.as_slice())
    }
    /// Reset capture/target history without invoking OS power APIs or writing a device.
    pub fn reset(&mut self) {
        for (status, cadence) in self.sources.values_mut() {
            *status = SourceStatus::default();
            *cadence = Cadence::default();
        }
        for o in &mut self.outputs {
            o.processor.reset();
            o.previous.fill(Rgb::default());
            o.cadence = Cadence::default();
            o.cleared = false;
        }
        self.active = false;
    }
    /// Updates Host intent only. Hardware submission remains Host.tick's responsibility.
    pub fn tick(
        &mut self,
        now_us: u64,
        active: bool,
        provider: &mut impl Provider,
        host: &mut Host,
    ) -> Result<usize, String> {
        // Reject a different live topology before polling a source or altering any route.
        for (r, o) in self.config.routes.iter().zip(&self.outputs) {
            if !host.setup().devices.iter().any(|d| {
                d.id == r.device
                    && d.role == Role::Ambient
                    && d.leds == o.previous.len()
                    && d.rate_hz == o.hz
            }) {
                return Err("Ambient host topology changed; rebuild controller".into());
            }
        }
        let count = self.prepare(now_us, active, provider)?;
        for (device, colors) in self.changes() {
            host.set_external(device, colors.to_vec())?;
        }
        Ok(count)
    }
    /// Process on a capture worker without sharing a Host across threads.
    /// Changes are borrowed until the next call and do not imply hardware submission.
    pub fn changes(&self) -> impl Iterator<Item = (&str, &[Rgb])> {
        self.config
            .routes
            .iter()
            .zip(&self.outputs)
            .filter(|(_, o)| o.changed)
            .map(|(r, o)| (r.device.as_str(), o.previous.as_slice()))
    }
    /// Earliest actual source/output/expiry deadline; the owner must not run a second cadence.
    pub fn next_due_us(&self) -> Option<u64> {
        if !self.active {
            return None;
        }
        let now = self.last_us.unwrap_or(0);
        let sources = self.config.sources.iter().filter(|s| {
            self.config
                .routes
                .iter()
                .any(|r| r.enabled && r.source == s.key)
        });
        let mut due = None;
        for source in sources {
            let (status, cadence) = &self.sources[&source.key];
            for next in [
                cadence.0,
                status
                    .observed_ms
                    .and_then(|at| {
                        at.checked_add(source.stale_after_ms)?
                            .checked_add(1)?
                            .checked_mul(1000)
                    })
                    .filter(|at| *at > now),
            ]
            .into_iter()
            .flatten()
            {
                due = Some(due.map_or(next, |old: u64| old.min(next)));
            }
        }
        for (route, output) in self.config.routes.iter().zip(&self.outputs) {
            if route.enabled
                && let Some(next) = output.cadence.0
            {
                due = Some(due.map_or(next, |old| old.min(next)));
            }
        }
        due
    }
    pub fn prepare(
        &mut self,
        now_us: u64,
        active: bool,
        provider: &mut impl Provider,
    ) -> Result<usize, String> {
        if self.last_us.is_some_and(|last| now_us < last) {
            return Err("Ambient clock moved backwards".into());
        }
        for o in &mut self.outputs {
            o.changed = false;
        }
        self.last_us = Some(now_us);
        if !active && self.active {
            self.reset();
        }
        self.active = active;
        let now_ms = now_us / 1000;
        if active {
            for s in &self.config.sources {
                if !self
                    .config
                    .routes
                    .iter()
                    .any(|r| r.enabled && r.source == s.key)
                {
                    continue;
                }
                let (status, cadence) = self.sources.get_mut(&s.key).unwrap();
                if !cadence.due(now_us, s.rate_hz) {
                    continue;
                }
                status.polls = status.polls.saturating_add(1);
                match provider.poll(&s.key, s.crop, now_ms).and_then(|frame| {
                    if let Some(f) = &frame {
                        if f.observed_ms > now_ms
                            || now_ms - f.observed_ms > s.stale_after_ms
                            || status.observed_ms.is_some_and(|at| f.observed_ms < at)
                            || f.manual_crop != s.crop
                            || f.image.width() > 320
                            || f.image.height() > 180
                        {
                            return Err(
                                "Ambient observation has invalid age, crop or dimensions".into()
                            );
                        }
                        for (r, o) in self
                            .config
                            .routes
                            .iter()
                            .zip(&mut self.outputs)
                            .filter(|(r, _)| r.enabled && r.source == s.key)
                        {
                            let _ = r;
                            o.processor.accept(f)?;
                        }
                    }
                    Ok(frame)
                }) {
                    Ok(Some(frame)) => {
                        status.observed_ms = Some(frame.observed_ms);
                        status.error = None;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        status.observed_ms = None;
                        status.error = Some(e.chars().take(512).collect());
                        for (r, o) in self.config.routes.iter().zip(&mut self.outputs) {
                            if r.source == s.key {
                                o.processor.reset();
                            }
                        }
                    }
                }
            }
        }
        let mut submitted = 0;
        for (r, o) in self.config.routes.iter().zip(&mut self.outputs) {
            let s = self
                .config
                .sources
                .iter()
                .find(|s| s.key == r.source)
                .unwrap();
            let observed = self.sources[&r.source].0.observed_ms;
            let valid = active
                && r.enabled
                && observed.is_some_and(|at| {
                    now_ms
                        .checked_sub(at)
                        .is_some_and(|age| age <= s.stale_after_ms)
                });
            if !valid {
                if !o.cleared {
                    o.previous.fill(Rgb::default());
                    o.processor.reset();
                    o.cadence = Cadence::default();
                    o.changed = true;
                    o.cleared = true;
                    submitted += 1;
                }
            } else if o.cadence.due(now_us, o.hz)
                && let Some(colors) = o.processor.render(&o.previous, r.output)?
            {
                o.previous = colors;
                o.changed = true;
                o.cleared = false;
                submitted += 1;
            }
        }
        Ok(submitted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capture::{Backend, ambient::Mode},
        devices::{Canvas, Device, Driver},
        lighting::ambient::Image,
    };
    use std::sync::Arc;
    #[test]
    fn next_deadline_clears_expired_input_before_a_slow_source_poll() {
        struct Pixels;
        impl Provider for Pixels {
            fn poll(
                &mut self,
                _: &str,
                crop: Option<Rect>,
                at: u64,
            ) -> Result<Option<Frame>, String> {
                Ok(Some(Frame {
                    image: std::sync::Arc::new(crate::lighting::ambient::Image::new(
                        1,
                        1,
                        vec![Rgb::new(100, 0, 0)],
                    )?),
                    observed_ms: at,
                    new_pixels: true,
                    backend: Backend::Dxgi,
                    manual_crop: crop,
                }))
            }
        }
        let (mut setup, mut config, _) = fixture();
        for source in &mut config.sources {
            source.rate_hz = 1;
            source.stale_after_ms = 100;
        }
        for device in &mut setup.devices {
            device.rate_hz = 1;
        }
        let mut controller = Controller::new(config, &setup).unwrap();
        assert_eq!(controller.next_due_us(), None);
        controller.prepare(1_000_000, true, &mut Pixels).unwrap();
        assert_eq!(controller.next_due_us(), Some(1_101_000));
        controller.prepare(1_101_000, true, &mut Pixels).unwrap();
        assert_eq!(controller.next_due_us(), Some(2_000_000));
        controller.prepare(1_102_000, false, &mut Pixels).unwrap();
        assert_eq!(controller.next_due_us(), None);
    }
    fn fixture() -> (Setup, Config, Host) {
        let setup = Setup {
            version: 1,
            canvas: Canvas {
                width: 2,
                height: 2,
                points: vec![],
            },
            devices: ["a", "b"]
                .into_iter()
                .map(|id| Device {
                    id: id.into(),
                    label: id.into(),
                    role: Role::Ambient,
                    driver: Driver::Unsupported {
                        protocol: "stand".into(),
                    },
                    leds: 2,
                    rate_hz: if id == "a" { 48 } else { 25 },
                    enabled: false,
                })
                .collect(),
        };
        let config = Config {
            sources: vec![Source {
                key: "monitor".into(),
                rate_hz: 60,
                crop: None,
                stale_after_ms: 100,
            }],
            routes: ["a", "b"]
                .into_iter()
                .map(|id| Route {
                    device: id.into(),
                    source: "monitor".into(),
                    enabled: true,
                    zone: Zone::linear(2, id == "b", Mode::ScreenCopy).unwrap(),
                    processing: Settings::default(),
                    output: OutputSettings {
                        smoothness: 0,
                        ..OutputSettings::default()
                    },
                })
                .collect(),
        };
        let host = Host::new(setup.clone(), crate::runtime::scene::example()).unwrap();
        (setup, config, host)
    }
    struct Feed {
        at: u64,
        calls: usize,
        mode: u8,
        image: Arc<Image>,
    }
    impl Feed {
        fn new() -> Self {
            Self {
                at: 0,
                calls: 0,
                mode: 0,
                image: Arc::new(
                    Image::new(2, 1, vec![Rgb::new(255, 0, 0), Rgb::new(0, 0, 255)]).unwrap(),
                ),
            }
        }
    }
    impl Provider for Feed {
        fn poll(&mut self, _: &str, crop: Option<Rect>, _: u64) -> Result<Option<Frame>, String> {
            self.calls += 1;
            match self.mode {
                1 => Ok(None),
                2 => Err("source offline".into()),
                _ => Ok(Some(Frame {
                    image: self.image.clone(),
                    observed_ms: self.at,
                    new_pixels: self.calls == 1,
                    backend: Backend::Dxgi,
                    manual_crop: crop,
                })),
            }
        }
    }
    #[test]
    fn one_capture_fans_out_with_separate_output_cadence_and_reverse() {
        let (setup, config, mut host) = fixture();
        let mut c = Controller::new(config, &setup).unwrap();
        let mut feed = Feed::new();
        assert_eq!(c.tick(0, true, &mut feed, &mut host).unwrap(), 2);
        assert_eq!(feed.calls, 1);
        assert_eq!(
            c.colors("a").unwrap(),
            [Rgb::new(255, 0, 0), Rgb::new(0, 0, 255)]
        );
        assert_eq!(
            c.colors("b").unwrap(),
            [Rgb::new(0, 0, 255), Rgb::new(255, 0, 0)]
        );
        feed.at = 20;
        assert_eq!(c.tick(20_000, true, &mut feed, &mut host).unwrap(), 0);
        assert_eq!(feed.calls, 2);
        assert_eq!(c.tick(21_000, true, &mut feed, &mut host).unwrap(), 1);
        assert_eq!(feed.calls, 2);
        feed.at = 40;
        assert_eq!(c.tick(40_000, true, &mut feed, &mut host).unwrap(), 1);
        assert_eq!(feed.calls, 3);
        assert!(host.requested("a").is_none()); // Host has no attached output.
    }
    #[test]
    fn stale_missing_error_and_future_frames_clear_once_then_recover() {
        let (setup, config, mut host) = fixture();
        let mut c = Controller::new(config, &setup).unwrap();
        let mut f = Feed::new();
        c.tick(0, true, &mut f, &mut host).unwrap();
        f.mode = 1;
        assert_eq!(c.tick(120_000, true, &mut f, &mut host).unwrap(), 2);
        assert_eq!(c.colors("a").unwrap(), [Rgb::default(); 2]);
        assert_eq!(c.tick(140_000, true, &mut f, &mut host).unwrap(), 0);
        f.mode = 0;
        f.at = 160;
        c.tick(160_000, true, &mut f, &mut host).unwrap();
        assert_ne!(c.colors("a").unwrap(), [Rgb::default(); 2]);
        f.mode = 2;
        c.tick(180_000, true, &mut f, &mut host).unwrap();
        assert_eq!(c.colors("a").unwrap(), [Rgb::default(); 2]);
        assert_eq!(
            c.source_status("monitor").unwrap().error.as_deref(),
            Some("source offline")
        );
        f.mode = 0;
        f.at = 500;
        c.tick(200_000, true, &mut f, &mut host).unwrap();
        assert!(c.source_status("monitor").unwrap().observed_ms.is_none());
        f.at = 220;
        c.tick(220_000, true, &mut f, &mut host).unwrap();
        assert!(c.source_status("monitor").unwrap().error.is_none());
    }
    #[test]
    fn inactive_has_no_capture_and_reenable_discards_old_smoothing_history() {
        let (setup, config, mut host) = fixture();
        let mut c = Controller::new(config, &setup).unwrap();
        let mut f = Feed::new();
        c.tick(0, true, &mut f, &mut host).unwrap();
        c.tick(1_000, false, &mut f, &mut host).unwrap();
        c.tick(200_000, false, &mut f, &mut host).unwrap();
        assert_eq!(f.calls, 1);
        assert_eq!(c.colors("b").unwrap(), [Rgb::default(); 2]);
        f.at = 201;
        c.tick(201_000, true, &mut f, &mut host).unwrap();
        assert_eq!(f.calls, 2);
        assert_ne!(c.colors("a").unwrap(), [Rgb::default(); 2]);
        assert!(c.tick(200_999, true, &mut f, &mut host).is_err());
        assert_eq!(f.calls, 2);
    }
    #[test]
    fn settings_roundtrip_and_bad_cross_references_are_rejected() {
        let (setup, config, _) = fixture();
        let bytes = serde_json::to_vec(&config).unwrap();
        assert_eq!(serde_json::from_slice::<Config>(&bytes).unwrap(), config);
        let mut bad = config.clone();
        bad.routes[0].source = "other".into();
        assert!(bad.validate(&setup).is_err());
        bad = config.clone();
        bad.routes[0].output.brightness = f32::NAN;
        assert!(bad.validate(&setup).is_err());
        bad = config.clone();
        bad.routes[0].processing.borders.stable_frames = 201;
        assert!(bad.validate(&setup).is_err());
        bad = config.clone();
        bad.routes.push(bad.routes[0].clone());
        assert!(bad.validate(&setup).is_err());
        bad = config.clone();
        bad.sources[0].crop = Some(Rect {
            x: usize::MAX,
            y: 0,
            width: 2,
            height: 1,
        });
        assert!(bad.validate(&setup).is_err());
        let mut value = serde_json::to_value(config).unwrap();
        value["sources"][0]["monitor_path"] = serde_json::json!("private");
        assert!(serde_json::from_value::<Config>(value).is_err());
    }
    #[test]
    fn wrong_host_is_rejected_before_source_poll() {
        let (setup, config, _) = fixture();
        let mut c = Controller::new(config, &setup).unwrap();
        let mut f = Feed::new();
        let mut other = setup.clone();
        other.devices[0].rate_hz = 49;
        let mut host = Host::new(other, crate::runtime::scene::example()).unwrap();
        assert!(c.tick(0, true, &mut f, &mut host).is_err());
        assert_eq!(f.calls, 0);
    }
}
