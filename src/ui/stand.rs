//! Explicit software stand factories for the real Runtime. No native backends.
//! RGB submissions and TT ACKs below are memory-only, never physical evidence.
use crate::{
    app::{ambient, assets::Store, lcd, runtime::Runtime},
    capture::{Backend, Frame},
    devices::{Driver, Role},
    lcd::protocol::{self, ACK_BYTES, REPORT_BYTES, ReportTransport},
    lighting::{
        ambient::{Image, Rect},
        color::Rgb,
    },
    runtime::output::{Cancellation, RgbDriver, WriteResult},
};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Default)]
pub struct Feed(Arc<AtomicBool>);
impl Feed {
    pub fn enable(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn enabled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Attach missing owners only. Some(Failed) deliberately requires an explicit
/// Runtime retry, rather than an automatic connection loop on every UI timer.
pub fn attach_missing(runtime: &mut Runtime, asset_root: &Path, feed: &Feed) -> Result<(), String> {
    let plan = prepare(runtime.document(), asset_root, feed)?;
    for (id, keys, factory) in plan.rgb {
        if runtime.host().status(&id).is_none() {
            runtime.attach_rgb(&id, keys, factory)?;
        }
    }
    for (id, keys, factory) in plan.lcd {
        if runtime.lcd_status(&id).is_none() {
            runtime.attach_lcd(&id, keys, factory)?;
        }
    }
    if runtime.capture_status().is_none()
        && let Some(factory) = plan.capture
    {
        runtime.attach_capture(factory)?;
    }
    Ok(())
}
pub(super) fn prepare(
    document: &crate::app::document::Document,
    asset_root: &Path,
    feed: &Feed,
) -> Result<crate::app::runtime::OutputPlan, String> {
    if !asset_root.is_absolute() {
        return Err("Software stand asset root must be absolute".into());
    }
    let mut plan = crate::app::runtime::OutputPlan::default();
    for device in document
        .lighting
        .setup
        .devices
        .iter()
        .filter(|d| d.enabled && d.role != Role::Lcd)
        .cloned()
    {
        plan.rgb.push((
            device.id.clone(),
            vec![],
            Arc::new(move |cancel| {
                if cancel.cancelled() {
                    return Err("Software RGB construction cancelled".into());
                }
                Ok(Box::new(RgbSink {
                    driver: device.driver.clone(),
                    colors: vec![Rgb::default(); device.leds],
                }) as Box<dyn RgbDriver>)
            }),
        ));
    }
    for lcd in document.lcd.iter().filter(|l| {
        l.config.enabled
            && document
                .lighting
                .setup
                .devices
                .iter()
                .any(|d| d.id == l.device && d.enabled && d.role == Role::Lcd)
    }) {
        let root = asset_root.to_owned();
        plan.lcd.push((
            lcd.device.clone(),
            vec![],
            Arc::new(move |config, cancel| {
                if cancel.cancelled() {
                    return Err("Software LCD construction cancelled".into());
                }
                let store = Store::new(root.clone())?;
                let controller = lcd::Controller::new(config, &store)?;
                if cancel.cancelled() {
                    return Err("Software LCD construction cancelled".into());
                }
                Ok((
                    controller,
                    Box::new(LcdSink::new(cancel)) as Box<dyn ReportTransport>,
                ))
            }),
        ));
    }
    if feed.enabled() {
        plan.capture = Some(Arc::new(|cancel| {
            Ok(Box::new(Synthetic::new(cancel)) as Box<dyn ambient::Provider>)
        }));
    }
    plan.validate(document)?;
    Ok(plan)
}

struct RgbSink {
    driver: Driver,
    colors: Vec<Rgb>,
}
impl RgbDriver for RgbSink {
    fn protocol(&self) -> Driver {
        self.driver.clone()
    }
    fn leds(&self) -> usize {
        self.colors.len()
    }
    fn write(&mut self, colors: &[Rgb], cancel: &Cancellation) -> Result<WriteResult, String> {
        if cancel.cancelled() {
            return Err("Software RGB submission cancelled".into());
        }
        if colors.len() != self.colors.len() {
            return Err("Software RGB LED count mismatch".into());
        }
        self.colors.copy_from_slice(colors);
        Ok(WriteResult::Submitted)
    }
}

pub(crate) struct LcdSink {
    cancel: Cancellation,
    count: Option<usize>,
    next: usize,
    jpeg: Vec<u8>,
    failed: bool,
}
impl LcdSink {
    pub(crate) fn new(cancel: Cancellation) -> Self {
        Self {
            cancel,
            count: None,
            next: 0,
            jpeg: vec![],
            failed: false,
        }
    }
    fn usable(&self, timeout: u64) -> Result<(), String> {
        if self.failed
            || self.cancel.cancelled()
            || timeout == 0
            || timeout > protocol::REPORT_DEADLINE_MS
        {
            return Err("Software LCD transport stopped, cancelled or invalid timeout".into());
        }
        Ok(())
    }
}
impl ReportTransport for LcdSink {
    fn now_ms(&self) -> u64 {
        crate::clock::monotonic_ms()
    }
    fn write_report(&mut self, report: &[u8; REPORT_BYTES], timeout: u64) -> Result<usize, String> {
        let result = (|| {
            self.usable(timeout)?;
            if report[0] != 0 || report[1] != 8 || report[3] != 0 {
                return Err("Invalid software TT report header".into());
            }
            if self.count.is_none() {
                if report[2] == 0 || report[4] != 0x80 {
                    return Err("Invalid software TT first report".into());
                }
                self.count = Some(usize::from(report[2]));
            } else if report[4] != 0 || report[2] != (((self.next - 1) % 0x15 + 1) as u8) {
                return Err("Invalid software TT continuation report".into());
            }
            if self.next >= self.count.unwrap()
                || self.jpeg.len() + protocol::PAYLOAD > protocol::MAX_FRAME
            {
                return Err("Software TT frame has excess reports".into());
            }
            self.jpeg.extend_from_slice(&report[5..]);
            self.next += 1;
            Ok(REPORT_BYTES)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn read_ack(&mut self, response: &mut [u8; ACK_BYTES], timeout: u64) -> Result<usize, String> {
        let result = (|| {
            self.usable(timeout)?;
            if self.count != Some(self.next) {
                return Err("Software TT ACK requested before a complete frame".into());
            }
            let length = self
                .jpeg
                .iter()
                .rposition(|&byte| byte != 0)
                .map_or(0, |i| i + 1);
            protocol::validate_jpeg(&self.jpeg[..length]).map_err(str::to_owned)?;
            // Exactly the source's 17-byte/report-ID-zero ACK contract. This is
            // fabricated by a software sink, not a response from a physical LCD.
            response.fill(0);
            protocol::validate_ack(response).map_err(str::to_owned)?;
            self.count = None;
            self.next = 0;
            self.jpeg.clear();
            Ok(ACK_BYTES)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}

struct Cached {
    crop: Rect,
    image: Arc<Image>,
}
struct Synthetic {
    cancel: Cancellation,
    cache: BTreeMap<String, Cached>,
}
impl Synthetic {
    fn new(cancel: Cancellation) -> Self {
        Self {
            cancel,
            cache: BTreeMap::new(),
        }
    }
}
impl ambient::Provider for Synthetic {
    fn poll(
        &mut self,
        source: &str,
        crop: Option<Rect>,
        requested_ms: u64,
    ) -> Result<Option<Frame>, String> {
        if self.cancel.cancelled() {
            return Err("Synthetic source cancelled".into());
        }
        if !ambient::key(source) {
            return Err("Invalid synthetic source key".into());
        }
        let area = crop.unwrap_or(Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        });
        if area.width == 0
            || area.height == 0
            || area.x.checked_add(area.width).is_none_or(|x| x > 1920)
            || area.y.checked_add(area.height).is_none_or(|y| y > 1080)
        {
            return Err("Synthetic crop must lie within the virtual 1920×1080 desktop".into());
        }
        let first = self
            .cache
            .get(source)
            .is_none_or(|cached| cached.crop != area);
        if first {
            if !self.cache.contains_key(source) && self.cache.len() >= 16 {
                return Err("Synthetic source cache exceeds 16 sources".into());
            }
            let scale = (320.0 / area.width as f64)
                .min(180.0 / area.height as f64)
                .min(1.0);
            let width = (area.width as f64 * scale).round().max(1.) as usize;
            let height = (area.height as f64 * scale).round().max(1.) as usize;
            let mut pixels = Vec::with_capacity(width * height);
            for y in 0..height {
                for x in 0..width {
                    let world_x =
                        area.x + ((2 * x + 1) * area.width / (2 * width)).min(area.width - 1);
                    let world_y =
                        area.y + ((2 * y + 1) * area.height / (2 * height)).min(area.height - 1);
                    pixels.push(Rgb::new(
                        (world_x * 255 / 1919) as u8,
                        (world_y * 255 / 1079) as u8,
                        ((world_x / 120 + world_y / 120) % 2 * 160 + 48) as u8,
                    ));
                }
            }
            self.cache.insert(
                source.to_owned(),
                Cached {
                    crop: area,
                    image: Arc::new(Image::new(width, height, pixels).map_err(str::to_owned)?),
                },
            );
        }
        Ok(Some(Frame {
            image: self.cache[source].image.clone(),
            observed_ms: requested_ms,
            new_pixels: first,
            backend: Backend::Synthetic,
            manual_crop: crop,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::lcd_worker::Phase,
        app::{
            assets::AssetId,
            config::{Groups, Project},
            document::{Document, Lcd},
            sensors,
        },
        capture::ambient::{Mode, OutputSettings, Settings, Zone},
        devices::{Canvas, Device, Point, Setup},
        schedule,
    };
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::AtomicU64,
        time::{Duration, Instant},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Stand {
        runtime: Runtime,
        root: PathBuf,
    }
    impl Stand {
        fn new(missing_media: bool) -> Self {
            let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("work/ui-stand");
            fs::create_dir_all(&parent).unwrap();
            let root = parent.join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let store = Store::new(root.join("assets")).unwrap();
            let media = if missing_media {
                AssetId::try_from("f".repeat(64)).unwrap()
            } else {
                store
                    .put(
                        include_bytes!("../lcd/fixtures/two-frame.apng"),
                        32 * 1024 * 1024,
                    )
                    .unwrap()
            };
            let devices = vec![
                Device {
                    id: "rgb".into(),
                    label: "RGB".into(),
                    role: Role::Canvas,
                    driver: Driver::SkydimoSerial {
                        model: "SK0801".into(),
                    },
                    leds: 2,
                    rate_hz: 25,
                    enabled: true,
                },
                Device {
                    id: "ambient".into(),
                    label: "Ambient".into(),
                    role: Role::Ambient,
                    driver: Driver::SkydimoSerial {
                        model: "SK0801".into(),
                    },
                    leds: 2,
                    rate_hz: 25,
                    enabled: true,
                },
                Device {
                    id: "lcd".into(),
                    label: "LCD".into(),
                    role: Role::Lcd,
                    driver: Driver::TtLcd,
                    leds: 0,
                    rate_hz: 25,
                    enabled: true,
                },
            ];
            let setup = Setup {
                version: 1,
                devices,
                canvas: Canvas {
                    width: 2,
                    height: 1,
                    points: (0..2)
                        .map(|led| Point {
                            device: "rgb".into(),
                            led,
                            x: led as u32,
                            y: 0,
                        })
                        .collect(),
                },
            };
            let document = Document {
                version: 1,
                lighting: Project {
                    version: 1,
                    setup,
                    scene: crate::runtime::scene::example(),
                    schedule: schedule::Config::default(),
                    groups: Groups {
                        pc: vec!["rgb".into(), "ambient".into(), "lcd".into()],
                        door: vec![],
                    },
                },
                ambient: ambient::Config {
                    sources: vec![ambient::Source {
                        key: "screen".into(),
                        rate_hz: 25,
                        crop: None,
                        stale_after_ms: 1000,
                    }],
                    routes: vec![ambient::Route {
                        device: "ambient".into(),
                        source: "screen".into(),
                        enabled: true,
                        zone: Zone::linear(2, false, Mode::ScreenCopy).unwrap(),
                        processing: Settings::default(),
                        output: OutputSettings {
                            smoothness: 0,
                            ..OutputSettings::default()
                        },
                    }],
                },
                lcd: vec![Lcd {
                    device: "lcd".into(),
                    config: lcd::Config {
                        enabled: true,
                        media: Some(media),
                        ..lcd::Config::default()
                    },
                }],
                temperatures: sensors::Config::default(),
            };
            Self {
                runtime: Runtime::new(document, None).unwrap(),
                root,
            }
        }
        fn wait(&mut self, condition: impl Fn(&Runtime) -> bool) {
            let until = Instant::now() + Duration::from_secs(5);
            while !condition(&self.runtime) {
                assert!(Instant::now() < until, "software stand timed out");
                self.runtime
                    .tick(crate::clock::monotonic_us(), 1_793_900_000)
                    .unwrap();
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
    impl Drop for Stand {
        fn drop(&mut self) {
            self.runtime.stop();
            let until = Instant::now() + Duration::from_secs(5);
            while !self.runtime.drained() {
                if Instant::now() >= until {
                    return;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            if let Ok(entries) = fs::read_dir(self.root.join("assets")) {
                for entry in entries.flatten() {
                    let _ = fs::remove_file(entry.path());
                }
            }
            let _ = fs::remove_dir(self.root.join("assets"));
            let _ = fs::remove_dir(&self.root);
        }
    }
    #[test]
    fn actual_runtime_uses_memory_rgb_tt_jpeg_ack_and_explicit_synthetic_feed() {
        let mut stand = Stand::new(false);
        let feed = Feed::default();
        assert!(!feed.enabled());
        let clone = feed.clone();
        let assets = stand.root.join("assets");
        attach_missing(&mut stand.runtime, &assets, &feed).unwrap();
        assert!(stand.runtime.capture_status().is_none());
        stand.wait(|runtime| {
            runtime
                .host()
                .status("rgb")
                .is_some_and(|s| s.submitted > 0)
                && runtime.lcd_status("lcd").is_some_and(|s| s.frames_sent > 0)
        });
        let preview = stand.runtime.lcd_status("lcd").unwrap().preview;
        protocol::validate_jpeg(&preview).unwrap();
        let decoded = image::load_from_memory(&preview).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (480, 480));
        let rgb_generation = stand.runtime.host().status("rgb").unwrap().generation;
        let lcd_generation = stand.runtime.lcd_status("lcd").unwrap().generation;
        clone.enable();
        assert!(feed.enabled());
        attach_missing(&mut stand.runtime, &assets, &feed).unwrap();
        stand.wait(|runtime| {
            runtime.capture_status().is_some_and(|s| {
                s.source_status["screen"].observed_ms.is_some()
                    && s.frames
                        .get("ambient")
                        .is_some_and(|colors| colors.iter().any(|c| *c != Rgb::default()))
            })
        });
        assert_eq!(
            stand.runtime.host().status("rgb").unwrap().generation,
            rgb_generation
        );
        assert_eq!(
            stand.runtime.lcd_status("lcd").unwrap().generation,
            lcd_generation
        );
    }
    #[test]
    fn missing_lcd_media_fails_once_without_automatic_retry() {
        let mut stand = Stand::new(true);
        let assets = stand.root.join("assets");
        let feed = Feed::default();
        attach_missing(&mut stand.runtime, &assets, &feed).unwrap();
        stand.wait(|runtime| {
            runtime
                .lcd_status("lcd")
                .is_some_and(|s| s.phase == Phase::Failed)
        });
        let failed = stand.runtime.lcd_status("lcd").unwrap();
        assert!(failed.error.is_some());
        for _ in 0..3 {
            attach_missing(&mut stand.runtime, &assets, &feed).unwrap();
        }
        let after = stand.runtime.lcd_status("lcd").unwrap();
        assert_eq!(after.generation, failed.generation);
        assert_eq!(after.phase, Phase::Failed);
        assert!(stand.runtime.capture_status().is_none());
    }
    #[test]
    fn synthetic_pixels_are_bounded_crop_aware_and_cached_with_truthful_backend() {
        use ambient::Provider;
        let mut source = Synthetic::new(Cancellation::default());
        let first = source.poll("screen", None, 10).unwrap().unwrap();
        let same = source.poll("screen", None, 20).unwrap().unwrap();
        assert_eq!(first.backend, Backend::Synthetic);
        assert_eq!((first.image.width(), first.image.height()), (320, 180));
        assert!(first.new_pixels);
        assert!(!same.new_pixels);
        assert!(Arc::ptr_eq(&first.image, &same.image));
        assert_eq!(same.observed_ms, 20);
        let crop = Rect {
            x: 960,
            y: 0,
            width: 960,
            height: 1080,
        };
        let cropped = source.poll("screen", Some(crop), 30).unwrap().unwrap();
        assert_eq!((cropped.image.width(), cropped.image.height()), (160, 180));
        assert_eq!(cropped.manual_crop, Some(crop));
        assert!(cropped.image.pixels()[0].r > first.image.pixels()[0].r);
        assert!(cropped.new_pixels);
        assert!(
            source
                .poll(
                    "screen",
                    Some(Rect {
                        x: 1920,
                        y: 0,
                        width: 1,
                        height: 1
                    }),
                    40
                )
                .is_err()
        );
        assert!(
            source
                .poll(
                    "screen",
                    Some(Rect {
                        x: usize::MAX,
                        y: 0,
                        width: 2,
                        height: 2
                    }),
                    40
                )
                .is_err()
        );
        let token = Cancellation::default();
        let mut cancelled = Synthetic::new(token.clone());
        token.cancel_for_test();
        assert!(cancelled.poll("screen", None, 1).is_err());
    }
    #[test]
    fn software_tt_transport_rejects_bad_reports_and_requires_complete_valid_jpeg() {
        let mut sink = LcdSink::new(Cancellation::default());
        let mut report = [0; REPORT_BYTES];
        report[0] = 1;
        assert!(sink.write_report(&report, 250).is_err());
        assert!(sink.read_ack(&mut [0; ACK_BYTES], 250).is_err());
        let mut sink = LcdSink::new(Cancellation::default());
        assert!(sink.read_ack(&mut [0; ACK_BYTES], 250).is_err());
        let mut sink = LcdSink::new(Cancellation::default());
        let malformed = protocol::packet(&[1, 2, 3], 0).unwrap();
        assert_eq!(sink.write_report(&malformed, 250).unwrap(), REPORT_BYTES);
        assert!(sink.read_ack(&mut [0; ACK_BYTES], 250).is_err());
    }
}
