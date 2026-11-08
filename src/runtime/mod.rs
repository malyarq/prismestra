//! Application frame host: geometry, one shared animation, independent I/O workers.
pub mod adapters;
mod autonomous;
#[path = "autonomous/wait.rs"]
pub(crate) mod cadence_wait;
pub mod frame_feed;
#[cfg(windows)]
pub mod native;
pub mod network;
pub mod output;
pub mod scene;
use crate::{
    devices::{Role, Setup},
    lighting::color::Rgb,
};
pub use autonomous::{ClockStatus, Host};
use output::{Cancellation, Output, RgbDriver, Snapshot, Ticket};
use scene::{Renderer, Scene};
use std::collections::BTreeMap;

#[derive(Default)]
struct Cadence {
    next: Option<u64>,
}
impl Cadence {
    fn due(&mut self, now: u64, hz: u32) -> bool {
        let period = 1_000_000u64.div_ceil(u64::from(hz));
        match self.next {
            None => {
                self.next = Some(now.saturating_add(period));
                true
            }
            Some(next) if now >= next => {
                // Never catch up by sending a burst after a late tick.
                self.next =
                    Some(next.saturating_add(((now - next) / period + 1).saturating_mul(period)));
                true
            }
            _ => false,
        }
    }
}
#[derive(Default)]
struct FrameView {
    preview: Vec<Rgb>,
    frames: BTreeMap<String, Vec<Rgb>>,
}
struct Core {
    setup: std::sync::Arc<Setup>,
    renderer: Renderer,
    outputs: BTreeMap<String, Output>,
    endpoint_leases: BTreeMap<String, Vec<String>>,
    cadences: BTreeMap<String, Cadence>,
    scene_cadence: Cadence,
    frame: std::sync::Arc<FrameView>,
    external: BTreeMap<String, std::sync::Arc<[Rgb]>>,
    feed: Option<frame_feed::Reader>,
    generation: u64,
    sequence: u64,
    last_us: Option<u64>,
    muted: BTreeMap<String, bool>,
    requested: BTreeMap<String, Ticket>,
}
impl Core {
    pub fn new(setup: Setup, scene: Scene) -> Result<Self, String> {
        setup.validate()?;
        let renderer = Renderer::new(scene)?;
        Ok(Self {
            setup: std::sync::Arc::new(setup),
            renderer,
            outputs: BTreeMap::new(),
            endpoint_leases: BTreeMap::new(),
            cadences: BTreeMap::new(),
            scene_cadence: Cadence::default(),
            frame: Default::default(),
            external: BTreeMap::new(),
            feed: None,
            generation: 0,
            sequence: 0,
            last_us: None,
            muted: BTreeMap::new(),
            requested: BTreeMap::new(),
        })
    }
    pub fn requested(&self, id: &str) -> Option<Ticket> {
        self.requested.get(id).copied()
    }
    pub fn status(&self, id: &str) -> Option<Snapshot> {
        self.outputs.get(id).map(Output::snapshot)
    }
    pub fn configure_scene(&mut self, scene: Scene) -> Result<(), String> {
        self.renderer.configure(scene)?;
        self.scene_cadence = Cadence::default();
        Ok(())
    }
    /// A replacement never takes over while the prior generation still owns I/O.
    pub fn replace_setup(&mut self, setup: Setup) -> Result<(), String> {
        setup.validate()?;
        self.reap();
        if self.outputs.values().any(|o| !o.is_reaped()) {
            return Err("Stop and drain RGB outputs before replacing devices".into());
        }
        self.setup = std::sync::Arc::new(setup);
        self.outputs.clear();
        self.endpoint_leases.clear();
        self.cadences.clear();
        self.frame = Default::default();
        self.external.clear();
        self.feed = None;
        self.muted.clear();
        self.requested.clear();

        self.scene_cadence = Cadence::default();
        Ok(())
    }
    /// Native callers reserve canonical endpoint keys across binding snapshots.
    pub(crate) fn attach_claimed<F>(
        &mut self,
        id: &str,
        keys: Vec<String>,
        factory: F,
    ) -> Result<(), String>
    where
        F: FnOnce(Cancellation) -> Result<Box<dyn RgbDriver>, String> + Send + 'static,
    {
        self.reap();
        if keys.len() > 4 || keys.iter().any(|k| k.is_empty() || k.len() > 4100) {
            return Err("Invalid output endpoint lease".into());
        }
        if keys.iter().collect::<std::collections::BTreeSet<_>>().len() != keys.len()
            || keys.iter().any(|key| {
                self.endpoint_leases
                    .values()
                    .any(|active| active.contains(key))
            })
        {
            return Err(
                "Hardware endpoint is already owned by an active or draining output".into(),
            );
        }
        let device = self
            .setup
            .devices
            .iter()
            .find(|d| d.id == id)
            .ok_or("Unknown logical device")?;
        if !device.enabled || device.role == Role::Lcd {
            return Err("Device is disabled or is not an RGB output".into());
        }
        if self.outputs.get(id).is_some_and(|o| !o.is_reaped()) {
            return Err("RGB device already owns an output generation".into());
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or("RGB generation exhausted")?;
        let output = Output::start_claimed(
            device.driver.clone(),
            device.leds,
            generation,
            keys.clone(),
            factory,
        )?;
        self.generation = generation;
        self.outputs.insert(id.to_owned(), output);
        self.endpoint_leases.insert(id.to_owned(), keys);
        self.cadences.remove(id);
        self.requested.remove(id);
        Ok(())
    }
    pub fn mute(&mut self, id: &str, off: bool) -> Result<(), String> {
        if !self
            .setup
            .devices
            .iter()
            .any(|d| d.id == id && d.role != Role::Lcd)
        {
            return Err("Unknown RGB device".into());
        }
        self.muted.insert(id.to_owned(), off);
        self.cadences.remove(id);
        Ok(())
    }
    /// Ambient preparation is independent of canvas animation and may run at its
    /// own capture/render cadence. Missing input is no-write, not synthetic black.
    pub fn set_external(&mut self, id: &str, colors: Vec<Rgb>) -> Result<(), String> {
        let device = self
            .setup
            .devices
            .iter()
            .find(|d| d.id == id && d.role == Role::Ambient)
            .ok_or("Unknown ambient device")?;
        if colors.len() != device.leds {
            return Err("Ambient output size mismatch".into());
        }
        self.external.insert(id.to_owned(), colors.into());
        Ok(())
    }
    fn validate_feed(&self, feed: &frame_feed::Reader) -> Result<(), String> {
        if feed.routes().iter().any(|(id, r)| {
            !self
                .setup
                .devices
                .iter()
                .any(|d| &d.id == id && d.role == Role::Ambient && d.leds == r.leds)
        }) {
            return Err("External feed does not match ambient devices".into());
        }
        Ok(())
    }
    fn bind_feed(&mut self, feed: frame_feed::Reader) -> Result<(), String> {
        self.validate_feed(&feed)?;
        self.clear_feed();
        self.feed = Some(feed);
        Ok(())
    }
    fn clear_feed(&mut self) {
        if let Some(feed) = self.feed.take() {
            for (id, r) in feed.routes() {
                self.external
                    .insert(id.clone(), vec![Rgb::default(); r.leds].into());
                self.cadences.remove(id);
            }
        }
    }
    fn refresh_feed(&mut self, now_us: u64) -> Result<(), String> {
        let Some(feed) = &self.feed else {
            return Ok(());
        };
        let batch = feed.snapshot();
        let now = now_us / 1000;
        for (id, route) in feed.routes() {
            let frame = batch.frames.get(id).filter(|f| {
                !batch.closed
                    && f.observed_ms.is_some_and(|at| {
                        now.checked_sub(at)
                            .is_some_and(|age| age <= route.stale_after_ms)
                    })
            });
            if let Some(frame) = frame {
                self.external.insert(id.clone(), frame.colors.clone());
            } else if self
                .external
                .get(id)
                .is_none_or(|colors| colors.iter().any(|c| *c != Rgb::default()))
            {
                self.external
                    .insert(id.clone(), vec![Rgb::default(); route.leds].into());
                self.cadences.remove(id);
            }
        }
        Ok(())
    }
    fn feed_deadline(&self, now_us: u64) -> Option<u64> {
        let feed = self.feed.as_ref()?;
        let batch = feed.snapshot();
        if batch.closed {
            return None;
        }
        feed.routes()
            .iter()
            .filter_map(|(id, r)| {
                batch
                    .frames
                    .get(id)?
                    .observed_ms?
                    .checked_add(r.stale_after_ms)?
                    .checked_add(1)?
                    .checked_mul(1000)
            })
            .filter(|at| *at > now_us)
            .min()
    }
    pub fn tick(&mut self, now_us: u64) -> Result<usize, String> {
        if self.last_us.is_some_and(|last| now_us < last) {
            return Err("Host monotonic clock moved backwards".into());
        }
        self.last_us = Some(now_us);
        self.refresh_feed(now_us)?;
        self.reap();
        if self.scene_cadence.due(now_us, self.renderer.scene().fps) {
            let preview = self.renderer.step(&self.setup.canvas)?;
            let mut frames = BTreeMap::new();
            for device in self.setup.devices.iter().filter(|d| d.role == Role::Canvas) {
                frames.insert(device.id.clone(), vec![Rgb::default(); device.leds]);
            }
            for (point, color) in self.setup.canvas.points.iter().zip(&preview) {
                frames
                    .get_mut(&point.device)
                    .ok_or("Canvas device disappeared")?[point.led] = *color;
            }
            self.frame = std::sync::Arc::new(FrameView { preview, frames });
        }
        let mut accepted = 0;
        for device in self
            .setup
            .devices
            .iter()
            .filter(|d| d.enabled && d.role != Role::Lcd)
        {
            let Some(output) = self.outputs.get(&device.id) else {
                continue;
            };
            if output.is_reaped() {
                continue;
            }
            if !self
                .cadences
                .entry(device.id.clone())
                .or_default()
                .due(now_us, device.rate_hz)
            {
                continue;
            }
            let source = if device.role == Role::Ambient {
                self.external.get(&device.id).map(|v| v.as_ref())
            } else {
                self.frame.frames.get(&device.id).map(|v| v.as_slice())
            };
            let off = self.muted.get(&device.id) == Some(&true);
            let black;
            let colors = if off {
                black = vec![Rgb::default(); device.leds];
                &black
            } else if let Some(source) = source {
                source
            } else {
                continue;
            };
            let sequence = self
                .sequence
                .checked_add(1)
                .ok_or("RGB frame sequence exhausted")?;
            let ticket = Ticket {
                generation: output.snapshot().generation,
                sequence,
            };
            if output.submit(ticket, colors)? {
                self.sequence = sequence;
                self.requested.insert(device.id.clone(), ticket);
                accepted += 1;
            }
        }
        Ok(accepted)
    }
    pub fn stop(&self) {
        for output in self.outputs.values() {
            output.stop();
        }
    }
    pub fn reap(&mut self) {
        for (id, output) in self.outputs.iter_mut() {
            if output.reap() {
                self.endpoint_leases.remove(id);
            }
        }
    }
    pub fn drained(&mut self) -> bool {
        self.reap();
        self.outputs.values().all(Output::is_reaped)
    }
}
impl Drop for Core {
    fn drop(&mut self) {
        self.stop();
        self.reap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::{Canvas, Device, Driver, Point};
    use output::WriteResult;
    use std::{sync::mpsc, time::Duration};
    struct Fake(mpsc::Sender<Vec<Rgb>>);
    impl RgbDriver for Fake {
        fn protocol(&self) -> Driver {
            Driver::DoorBle
        }
        fn leds(&self) -> usize {
            20
        }
        fn write(&mut self, c: &[Rgb], _: &Cancellation) -> Result<WriteResult, String> {
            self.0.send(c.to_vec()).map_err(|e| e.to_string())?;
            Ok(WriteResult::Submitted)
        }
    }
    fn setup() -> Setup {
        Setup {
            version: 1,
            devices: vec![Device {
                id: "door".into(),
                label: "Door".into(),
                role: Role::Canvas,
                driver: Driver::DoorBle,
                leds: 20,
                rate_hz: 10,
                enabled: true,
            }],
            canvas: Canvas {
                width: 20,
                height: 10,
                points: (0..20)
                    .map(|i| Point {
                        device: "door".into(),
                        led: i,
                        x: i as u32,
                        y: 5,
                    })
                    .collect(),
            },
        }
    }
    #[test]
    fn endpoint_lease_survives_stop_until_inflight_driver_is_reaped() {
        struct Slow {
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        }
        impl RgbDriver for Slow {
            fn protocol(&self) -> Driver {
                Driver::DoorBle
            }
            fn leds(&self) -> usize {
                20
            }
            fn write(&mut self, _: &[Rgb], _: &Cancellation) -> Result<WriteResult, String> {
                self.entered.send(()).unwrap();
                self.release
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|e| e.to_string())?;
                Ok(WriteResult::Submitted)
            }
        }
        let mut s = setup();
        let mut second = s.devices[0].clone();
        second.id = "other".into();
        s.devices.push(second);
        let mut points = s.canvas.points.clone();
        for point in &mut points {
            point.device = "other".into();
        }
        s.canvas.points.extend(points);
        let mut host = Host::new(s, scene::example()).unwrap();
        let (entered, ready) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        host.attach_claimed("door", vec!["ble:1".into()], move |_| {
            Ok(Box::new(Slow {
                entered,
                release: wait,
            }))
        })
        .unwrap();
        host.tick(0).unwrap();
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        host.stop();
        let attempted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = attempted.clone();
        let (tx, _rx) = mpsc::channel();
        assert!(
            host.attach_claimed("other", vec!["ble:1".into()], move |_| {
                observed.store(true, std::sync::atomic::Ordering::Release);
                Ok(Box::new(Fake(tx)))
            })
            .is_err()
        );
        assert!(!attempted.load(std::sync::atomic::Ordering::Acquire));
        release.send(()).unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while !host.drained() && std::time::Instant::now() < until {
            std::thread::yield_now();
        }
        assert!(host.drained());
        let (tx, _rx) = mpsc::channel();
        host.attach_claimed("other", vec!["ble:1".into()], move |_| {
            Ok(Box::new(Fake(tx)))
        })
        .unwrap();
        host.stop();
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while !host.drained() && std::time::Instant::now() < until {
            std::thread::yield_now();
        }
        assert!(host.drained());
    }
    #[test]
    fn scene_to_device_frame_and_safe_generation_replacement() {
        let mut host = Host::new(setup(), scene::example()).unwrap();
        let (tx, rx) = mpsc::channel();
        host.attach("door", move |_| Ok(Box::new(Fake(tx))))
            .unwrap();
        assert_eq!(host.tick(0).unwrap(), 1);
        let colors = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(colors, host.frame("door").unwrap());
        assert!(host.replace_setup(setup()).is_err());
        assert_eq!(host.tick(99_999).unwrap(), 0);
        assert_eq!(host.tick(100_000).unwrap(), 1);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        host.mute("door", true).unwrap();
        host.tick(100_001).unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            [Rgb::default(); 20]
        );
        assert!(host.tick(100_000).is_err());
        host.stop();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !host.drained() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(host.drained());
        host.replace_setup(setup()).unwrap();
    }
    #[test]
    fn cadence_drops_late_work_instead_of_burst() {
        let mut c = Cadence::default();
        assert!(c.due(0, 48));
        assert!(!c.due(20833, 48));
        assert!(c.due(20834, 48));
        assert!(c.due(10_000_000, 48));
        assert!(!c.due(10_000_000, 48));
    }
}
