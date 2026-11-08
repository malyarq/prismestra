use super::*;
mod coordinator_tests;
#[path = "registry_ingress_tests.rs"]
mod registry_ingress_tests;
use crate::{
    app::{
        ambient,
        assets::Store as Assets,
        config::{Groups, Project},
        document::Lcd,
        sensors,
    },
    capture::{
        Backend, Frame,
        ambient::{Mode, OutputSettings, Settings, Zone},
    },
    devices::{Canvas, Device, Driver, Point, Setup},
    lcd::protocol::{ACK_BYTES, REPORT_BYTES, ReportTransport},
    lighting::{
        ambient::{Image, Rect},
        color::Rgb,
    },
    runtime::output::WriteResult,
};
use std::{
    fs,
    ops::{Deref, DerefMut},
    sync::{
        Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
fn gpu_binding() -> crate::sensors::nvidia::PciIdentity {
    crate::sensors::nvidia::PciIdentity {
        device_id: 0x2684_10de,
        subsystem_id: 1,
        revision_id: 1,
        extended_device_id: 1,
    }
}
struct NvTemperature(Arc<std::sync::atomic::AtomicI32>);
impl crate::sensors::nvidia::Backend for NvTemperature {
    fn identity(&mut self) -> Result<crate::sensors::nvidia::PciIdentity, String> {
        Ok(gpu_binding())
    }
    fn thermal_settings(
        &mut self,
        output: &mut crate::sensors::nvidia::ThermalSettings,
    ) -> Result<(), String> {
        let temperature = self.0.load(Ordering::SeqCst);
        if temperature == i32::MIN {
            return Err("injected GPU read failure".into());
        }
        output.count = 1;
        output.sensors[0].target = crate::sensors::nvidia::TARGET_GPU;
        output.sensors[0].current_temperature = temperature;
        Ok(())
    }
}
fn gpu_factory(
    temperature: Arc<std::sync::atomic::AtomicI32>,
    calls: Arc<AtomicUsize>,
) -> sensor_worker::Factory {
    Arc::new(move |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(crate::sensors::nvidia::Provider::new(
            gpu_binding(),
            NvTemperature(temperature.clone()),
        )?))
    })
}

const LIQUID_PATH: &str = r"\\?\hid#vid_264a&pid_233c&mi_00#selected-liquid";
const DISPLAY_PATH: &str = r"\\?\hid#vid_264a&pid_233c&mi_01#selected-display";
#[derive(Default)]
struct TtState {
    liquid_path: String,
    display_path: String,
    mode: AtomicUsize,
    opens: AtomicUsize,
    closes: AtomicUsize,
    reads: AtomicUsize,
    writes: AtomicUsize,
    display_packets: AtomicUsize,
    blocked: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}
struct TtBackend(Arc<TtState>);
struct TtLink {
    descriptor: crate::lcd::hid_policy::Descriptor,
    state: Arc<TtState>,
    liquid: bool,
    written: bool,
    read: bool,
}
fn tt_state(root: &Path) -> Arc<TtState> {
    let tag = root.file_name().unwrap().to_string_lossy();
    Arc::new(TtState {
        liquid_path: format!("{LIQUID_PATH}-{tag}"),
        display_path: format!("{DISPLAY_PATH}-{tag}"),
        ..TtState::default()
    })
}
fn tt_descriptor(
    state: &TtState,
    kind: crate::lcd::hid_policy::Kind,
) -> crate::lcd::hid_policy::Descriptor {
    use crate::lcd::hid_policy::{Descriptor, Kind};
    let liquid = kind == Kind::Liquid;
    Descriptor {
        path: if liquid {
            state.liquid_path.clone()
        } else {
            state.display_path.clone()
        },
        vid: 0x264a,
        pid: 0x233c,
        usage_page: 0xff06,
        usage: 1,
        input_len: if liquid { 441 } else { 17 },
        output_len: if liquid { 441 } else { 1025 },
        feature_len: 0,
        input_buttons: 0,
        output_buttons: 0,
        input_ids: vec![0],
        output_ids: vec![0],
    }
}
impl crate::lcd::hid_adapter::Backend for TtBackend {
    type Link = TtLink;
    fn process_names(&mut self) -> Result<Vec<String>, String> {
        Ok(if self.0.mode.load(Ordering::SeqCst) == 3 {
            vec!["TTRGBPlus.exe".into()]
        } else {
            vec![]
        })
    }
    fn candidates(
        &mut self,
        kind: crate::lcd::hid_policy::Kind,
    ) -> Result<Vec<crate::lcd::hid_policy::Descriptor>, String> {
        let selected = tt_descriptor(&self.0, kind);
        let mut other = selected.clone();
        other.path.push_str("-other");
        Ok(vec![other, selected])
    }
    fn open(
        &mut self,
        kind: crate::lcd::hid_policy::Kind,
        path: &str,
    ) -> Result<Self::Link, String> {
        let descriptor = tt_descriptor(&self.0, kind);
        if descriptor.path != path {
            return Err("Unexpected explicit interface".into());
        }
        let liquid = kind == crate::lcd::hid_policy::Kind::Liquid;
        if liquid {
            self.0.opens.fetch_add(1, Ordering::SeqCst);
        }
        Ok(TtLink {
            descriptor,
            state: self.0.clone(),
            liquid,
            written: false,
            read: false,
        })
    }
}
impl Drop for TtLink {
    fn drop(&mut self) {
        if self.liquid {
            self.state.closes.fetch_add(1, Ordering::SeqCst);
        }
    }
}
impl crate::lcd::hid_adapter::Reports for TtLink {
    fn now_ms(&self) -> u64 {
        crate::clock::monotonic_ms()
    }
    fn descriptor(&mut self) -> Result<crate::lcd::hid_policy::Descriptor, String> {
        Ok(self.descriptor.clone())
    }
    fn write(&mut self, bytes: &[u8], _: u32) -> Result<usize, String> {
        if self.liquid {
            if self.written || bytes != crate::lcd::temperatures::liquid_status_request() {
                return Err("One status write per MI00 link".into());
            }
            self.written = true;
            self.state.writes.fetch_add(1, Ordering::SeqCst);
        } else {
            self.state.display_packets.fetch_add(1, Ordering::SeqCst);
        }
        Ok(bytes.len())
    }
    fn read(&mut self, bytes: &mut [u8], _: u32) -> Result<usize, String> {
        bytes.fill(0);
        if self.liquid {
            if !self.written || self.read || bytes.len() != 441 {
                return Err("One status read per MI00 link".into());
            }
            self.read = true;
            self.state.reads.fetch_add(1, Ordering::SeqCst);
            let blocked = self.state.blocked.lock().unwrap().take();
            if let Some(release) = blocked {
                release.recv().map_err(|e| e.to_string())?;
            }
            bytes[..5].copy_from_slice(&[0, 0x82, 1, 0, 0x80]);
            bytes[5..7].copy_from_slice(&3340u16.to_le_bytes());
            match self.state.mode.load(Ordering::SeqCst) {
                1 => bytes[1] = 0,
                2 => return Ok(440),
                _ => {}
            }
        }
        Ok(bytes.len())
    }
}
struct Stand {
    root: PathBuf,
    document: Document,
}
impl Stand {
    fn new() -> Self {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("work/app-runtime-stand");
        fs::create_dir_all(&parent).unwrap();
        let root = parent.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let assets = Assets::new(root.join("assets")).unwrap();
        let media = assets
            .put(
                include_bytes!("../../lcd/fixtures/two-frame.apng"),
                32 * 1024 * 1024,
            )
            .unwrap();
        let devices = vec![
            Device {
                id: "body".into(),
                label: "Body".into(),
                role: Role::Canvas,
                driver: Driver::EneDram,
                leds: 8,
                rate_hz: 48,
                enabled: true,
            },
            Device {
                id: "door".into(),
                label: "Door".into(),
                role: Role::Canvas,
                driver: Driver::DoorBle,
                leds: 20,
                rate_hz: 25,
                enabled: true,
            },
            Device {
                id: "ambient".into(),
                label: "Ambient".into(),
                role: Role::Ambient,
                driver: Driver::SkydimoSerial {
                    model: "SK0410".into(),
                },
                leds: 150,
                rate_hz: 48,
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
        let mut points = Vec::new();
        for (id, n, offset) in [("body", 8, 0), ("door", 20, 8)] {
            for led in 0..n {
                points.push(Point {
                    device: id.into(),
                    led,
                    x: (offset + led) as u32,
                    y: 0,
                });
            }
        }
        let setup = Setup {
            version: 1,
            devices,
            canvas: Canvas {
                width: 28,
                height: 1,
                points,
            },
        };
        let ambient = ambient::Config {
            sources: vec![ambient::Source {
                key: "screen".into(),
                rate_hz: 60,
                crop: None,
                stale_after_ms: 2000,
            }],
            routes: vec![ambient::Route {
                device: "ambient".into(),
                source: "screen".into(),
                enabled: true,
                zone: Zone::linear(150, false, Mode::ScreenCopy).unwrap(),
                processing: Settings::default(),
                output: OutputSettings {
                    smoothness: 0,
                    ..OutputSettings::default()
                },
            }],
        };
        let document = Document {
            version: 1,
            lighting: Project {
                version: 1,
                setup,
                scene: crate::runtime::scene::example(),
                schedule: schedule::Config::default(),
                groups: Groups {
                    pc: vec!["body".into(), "ambient".into(), "lcd".into()],
                    door: vec!["door".into()],
                },
            },
            ambient,
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
        document.validate().unwrap();
        Self { root, document }
    }
    fn runtime(&self, persistent: bool) -> Guard {
        Guard(
            Runtime::new(
                self.document.clone(),
                persistent.then(|| self.root.join("journal.json")),
            )
            .unwrap(),
        )
    }
    fn lcd_factory(&self) -> LcdFactory {
        let root = self.root.join("assets");
        Arc::new(move |config, _| {
            let store = Assets::new(root.clone())?;
            Ok((lcd::Controller::new(config, &store)?, Box::new(Reports)))
        })
    }
}
impl Drop for Stand {
    fn drop(&mut self) {
        if let Ok(entries) = fs::read_dir(self.root.join("assets")) {
            for entry in entries.flatten() {
                let _ = fs::remove_file(entry.path());
            }
        }
        let _ = fs::remove_dir(self.root.join("assets"));
        if let Ok(entries) = fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let _ = fs::remove_file(entry.path());
                let _ = fs::remove_dir(entry.path());
            }
        }
        let _ = fs::remove_dir(&self.root);
    }
}
struct Guard(Runtime);
impl Deref for Guard {
    type Target = Runtime;
    fn deref(&self) -> &Runtime {
        &self.0
    }
}
impl DerefMut for Guard {
    fn deref_mut(&mut self) -> &mut Runtime {
        &mut self.0
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.stop();
        let until = Instant::now() + Duration::from_secs(3);
        while !self.drained() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
fn wait(mut condition: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(Instant::now() < until, "Owned stand worker timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn at(value: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(value)
        .unwrap()
        .timestamp()
}
fn wall() -> i64 {
    at("2026-11-06T18:00:00Z")
}

#[cfg(windows)]
#[test]
fn typed_ddr5_transactions_feed_ram_mean_and_real_lcd_then_remove_failed_input() {
    use crate::transport::ddr5_bus::tests::sensor_factory;
    use std::sync::atomic::{AtomicBool, AtomicU16};
    let mut stand = Stand::new();
    stand.document.temperatures.sources = vec![
        sensors::Source {
            key: "ram-a".into(),
            value: sensors::Value::Sensor {
                input: "spd-a".into(),
            },
        },
        sensors::Source {
            key: "ram-b".into(),
            value: sensors::Value::Sensor {
                input: "spd-b".into(),
            },
        },
        sensors::Source {
            key: "ram".into(),
            value: sensors::Value::Mean {
                members: vec!["ram-a".into(), "ram-b".into()],
            },
        },
    ];
    // Private local font is read only; no hardware or proprietary font is
    // distributed. Actual Player/compositor/JPEG/TT reports remain in the path.
    let font = fs::read("C:/Windows/Fonts/segoeui.ttf").unwrap();
    let store = Assets::new(stand.root.join("assets")).unwrap();
    stand.document.lcd[0].config.font = Some(store.put(&font, 8 * 1024 * 1024).unwrap());
    stand.document.lcd[0].config.overlay_enabled = true;
    stand.document.lcd[0].config.sources = vec![lcd::OverlaySource {
        key: "ram".into(),
        label: "RAM".into(),
    }];
    let mut runtime = stand.runtime(false);
    runtime
        .attach_lcd("lcd", vec![], stand.lcd_factory())
        .unwrap();
    let first = Arc::new(AtomicU16::new(0x0190)); // 25C, little-endian WORD
    let second = Arc::new(AtomicU16::new(0x02d0)); // 45C
    let fail = Arc::new(AtomicBool::new(false));
    let a = sensor_factory(first.clone(), fail.clone(), 0x50);
    let b = sensor_factory(second, Arc::new(AtomicBool::new(false)), 0x51);
    runtime.attach_sensor("spd-a", vec![], a.clone()).unwrap();
    runtime.attach_sensor("spd-b", vec![], b).unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        runtime
            .temperature_samples()
            .iter()
            .any(|s| s.key == "ram" && s.celsius == 35.)
    });
    let revision = runtime.revision;
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        runtime.lcd_status("lcd").is_some_and(|s| {
            s.frames_sent > 0 && s.processed_revision >= revision && !s.preview.is_empty()
        })
    });
    let jpeg = runtime.lcd_status("lcd").unwrap().preview;
    let decoded = image::load_from_memory(&jpeg).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (480, 480));
    fail.store(true, Ordering::SeqCst);
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        runtime.sensor_status("spd-a").unwrap().error.is_some()
            && runtime
                .temperature_samples()
                .iter()
                .all(|s| s.key != "ram-a" && s.key != "ram")
    });
    assert!(
        runtime
            .temperature_samples()
            .iter()
            .any(|s| s.key == "ram-b" && s.celsius == 45.)
    );
    runtime.detach_sensor("spd-a").unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        runtime.sensor_status("spd-a").is_none()
    });
    fail.store(false, Ordering::SeqCst);
    first.store(0, Ordering::SeqCst); // legitimate zero, not error fallback
    runtime.attach_sensor("spd-a", vec![], a).unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        runtime
            .temperature_samples()
            .iter()
            .any(|s| s.key == "ram" && s.celsius == 22.5)
    });
}
fn now() -> u64 {
    crate::clock::monotonic_us()
}
#[derive(Default)]
struct Record(Mutex<Vec<Vec<Rgb>>>);
impl Record {
    fn last(&self) -> Option<Vec<Rgb>> {
        self.0.lock().unwrap().last().cloned()
    }
}
struct RgbDriverFake {
    protocol: Driver,
    leds: usize,
    record: Arc<Record>,
}
impl RgbDriver for RgbDriverFake {
    fn protocol(&self) -> Driver {
        self.protocol.clone()
    }
    fn leds(&self) -> usize {
        self.leds
    }
    fn write(&mut self, colors: &[Rgb], _: &Cancellation) -> Result<WriteResult, String> {
        let mut frames = self.record.0.lock().unwrap();
        if frames.len() == 8 {
            frames.remove(0);
        }
        frames.push(colors.to_vec());
        Ok(WriteResult::Submitted)
    }
}
fn rgb_factory(
    document: &Document,
    id: &str,
    record: Arc<Record>,
    calls: Arc<AtomicUsize>,
) -> RgbFactory {
    let d = document
        .lighting
        .setup
        .devices
        .iter()
        .find(|d| d.id == id)
        .unwrap()
        .clone();
    Arc::new(move |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(RgbDriverFake {
            protocol: d.driver.clone(),
            leds: d.leds,
            record: record.clone(),
        }))
    })
}
struct Reports;
impl ReportTransport for Reports {
    fn now_ms(&self) -> u64 {
        crate::clock::monotonic_ms()
    }
    fn write_report(&mut self, report: &[u8; REPORT_BYTES], _: u64) -> Result<usize, String> {
        assert_eq!((report[0], report[1]), (0, 8));
        Ok(REPORT_BYTES)
    }
    fn read_ack(&mut self, response: &mut [u8; ACK_BYTES], _: u64) -> Result<usize, String> {
        response.fill(0);
        Ok(ACK_BYTES)
    }
}
struct Pixels;
impl ambient::Provider for Pixels {
    fn poll(
        &mut self,
        _: &str,
        crop: Option<Rect>,
        requested_ms: u64,
    ) -> Result<Option<Frame>, String> {
        Ok(Some(Frame {
            image: Arc::new(Image::new(1, 1, vec![Rgb::new(120, 60, 20)]).unwrap()),
            observed_ms: requested_ms,
            new_pixels: true,
            backend: Backend::Dxgi,
            manual_crop: crop,
        }))
    }
}
#[test]
fn actual_workers_share_scene_capture_and_lcd_without_blocking_coordinator() {
    let stand = Stand::new();
    let mut runtime = stand.runtime(false);
    let body = Arc::new(Record::default());
    let ambient = Arc::new(Record::default());
    let calls = Arc::new(AtomicUsize::new(0));
    runtime
        .attach_rgb(
            "body",
            vec![],
            rgb_factory(&stand.document, "body", body.clone(), calls.clone()),
        )
        .unwrap();
    runtime
        .attach_rgb(
            "ambient",
            vec![],
            rgb_factory(&stand.document, "ambient", ambient.clone(), calls),
        )
        .unwrap();
    runtime
        .attach_lcd("lcd", vec![], stand.lcd_factory())
        .unwrap();
    runtime
        .attach_capture(Arc::new(|_| Ok(Box::new(Pixels))))
        .unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        body.last().is_some()
            && ambient
                .last()
                .is_some_and(|v| v.iter().all(|c| *c == Rgb::new(120, 60, 20)))
            && runtime.lcd_status("lcd").unwrap().frames_sent > 0
    });
    assert_eq!(ambient.last().unwrap(), vec![Rgb::new(120, 60, 20); 150]);
    let lcd = runtime.lcd_status("lcd").unwrap();
    assert!(!lcd.preview.is_empty());
    assert_eq!(image::load_from_memory(&lcd.preview).unwrap().width(), 480);
    runtime.set_group(Target::Pc, false).unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        body.last()
            .is_some_and(|v| v.iter().all(|c| *c == Rgb::default()))
            && runtime.lcd_status("lcd").unwrap().lcd_state == Some(lcd::State::Blanked)
    });
    assert_eq!(
        runtime.desired(),
        Desired {
            pc: false,
            door: true
        }
    );
    runtime.stop();
    wait(|| runtime.drained());
}
#[test]
fn journal_commits_intent_before_effect_and_restore_does_not_replay_old_event() {
    let mut stand = Stand::new();
    stand.document.lighting.schedule.enabled = true;
    {
        let mut runtime = stand.runtime(true);
        runtime.tick(now(), wall()).unwrap();
        runtime.set_group(Target::Pc, false).unwrap();
    }
    let mut runtime = stand.runtime(true);
    runtime.tick(now(), wall() + 1).unwrap();
    assert!(!runtime.desired().pc);
    runtime.tick(now(), at("2026-11-07T11:00:00Z")).unwrap();
    assert_eq!(
        runtime.desired(),
        Desired {
            pc: true,
            door: false
        }
    );
    fs::rename(
        stand.root.join("journal.json"),
        stand.root.join("previous.json"),
    )
    .unwrap();
    fs::create_dir(stand.root.join("journal.json")).unwrap();
    let desired = runtime.desired();
    assert!(
        runtime
            .set_group(Target::Pc, false)
            .unwrap_err()
            .contains("Runtime journal atomic replace")
    );
    assert_eq!(runtime.desired(), desired);
}
#[test]
fn synthetic_power_refresh_reopens_owners_only_after_drain_and_keeps_current_manual_intent() {
    let stand = Stand::new();
    let mut runtime = stand.runtime(false);
    let calls = Arc::new(AtomicUsize::new(0));
    runtime
        .attach_rgb(
            "body",
            vec![],
            rgb_factory(
                &stand.document,
                "body",
                Arc::new(Record::default()),
                calls.clone(),
            ),
        )
        .unwrap();
    wait(|| calls.load(Ordering::SeqCst) == 1);
    let base = now();
    runtime.notify_power(true, base).unwrap();
    runtime.notify_power(false, base + 1000).unwrap();
    runtime.set_group(Target::Pc, false).unwrap();
    wait(|| runtime.drained());
    runtime.tick(base + 6_000_000, wall()).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    runtime.tick(base + 7_002_000, wall()).unwrap();
    wait(|| calls.load(Ordering::SeqCst) == 2);
    wait(|| {
        runtime.tick(base + 7_003_000, wall()).unwrap();
        !runtime.power.pending()
    });
    assert!(!runtime.desired().pc);
    assert!(runtime.power.error().is_none());
}
#[test]
fn no_outputs_power_cycle_completes_and_restores_current_scene_without_os_actions() {
    let stand = Stand::new();
    let mut runtime = stand.runtime(false);
    let base = now();
    runtime.notify_power(true, base).unwrap();
    runtime.notify_power(false, base + 1).unwrap();
    let mut scene = runtime.document().lighting.scene.clone();
    scene.brightness = 31;
    runtime.set_scene(scene).unwrap();
    runtime.set_group(Target::Door, false).unwrap();
    for offset in [7_001_000, 7_002_000, 7_003_000] {
        runtime.tick(base + offset, wall()).unwrap();
    }
    assert!(!runtime.power.pending());
    assert_eq!(runtime.host().scene().brightness, 31);
    assert!(!runtime.desired().door);
    assert!(runtime.tick(base + 1, wall()).is_err());
}
#[test]
fn runtime_does_not_mistake_preview_journal_or_new_topology_for_its_own() {
    let stand = Stand::new();
    let path = stand.root.join("journal.json");
    {
        let _runtime = stand.runtime(true);
    }
    let original = fs::read(&path).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
    value["scope"] = serde_json::json!("pc-control.preview");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(Runtime::new(stand.document.clone(), Some(path.clone())).is_err());
    fs::write(&path, original).unwrap();
    let mut other = stand.document.clone();
    other.lighting.groups.door = vec![];
    other.lighting.groups.pc.push("door".into());
    assert!(Runtime::new(other, Some(path)).is_err());
}
#[test]
fn stopped_runtime_does_not_accept_new_factories_or_dispatch() {
    let stand = Stand::new();
    let mut runtime = stand.runtime(false);
    runtime.stop();
    let calls = Arc::new(AtomicUsize::new(0));
    assert!(
        runtime
            .attach_rgb(
                "body",
                vec![],
                rgb_factory(
                    &stand.document,
                    "body",
                    Arc::new(Record::default()),
                    calls.clone()
                )
            )
            .is_err()
    );
    assert!(runtime.tick(now(), wall()).is_err());
    assert!(runtime.set_group(Target::Pc, false).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[test]
fn runtime_selects_logical_ram_and_invalid_snapshot_does_not_replace_previous_samples() {
    let mut stand = Stand::new();
    stand.document.temperatures = sensors::Config {
        sources: vec![
            sensors::Source {
                key: "a".into(),
                value: sensors::Value::Sensor {
                    input: "slot-a".into(),
                },
            },
            sensors::Source {
                key: "b".into(),
                value: sensors::Value::Sensor {
                    input: "slot-b".into(),
                },
            },
            sensors::Source {
                key: "ram".into(),
                value: sensors::Value::Mean {
                    members: vec!["a".into(), "b".into()],
                },
            },
        ],
    };
    let mut runtime = stand.runtime(false);
    let input = [
        lcd::Sample {
            key: "slot-a".into(),
            celsius: 40.,
            observed_ms: 1000,
        },
        lcd::Sample {
            key: "slot-b".into(),
            celsius: 50.,
            observed_ms: 2000,
        },
    ];
    runtime.accept_temperatures(&input, 3_000_000).unwrap();
    assert_eq!(runtime.samples.len(), 3);
    assert_eq!(runtime.samples[2].celsius, 45.);
    let revision = runtime.revision;
    assert!(
        runtime
            .accept_temperatures(&[input[0].clone(), input[0].clone()], 3_000_000)
            .is_err()
    );
    assert_eq!(runtime.samples.len(), 3);
    assert_eq!(runtime.revision, revision);
    runtime.accept_temperatures(&input, 6_001_000).unwrap();
    assert!(runtime.samples.iter().all(|s| s.key != "ram"));
}
#[test]
fn owned_gpu_samples_merge_with_manual_inputs_and_cannot_be_overwritten_by_injection() {
    let mut stand = Stand::new();
    stand.document.temperatures.sources = vec![
        sensors::Source {
            key: "gpu".into(),
            value: sensors::Value::Sensor {
                input: "gpu-slot".into(),
            },
        },
        sensors::Source {
            key: "ram".into(),
            value: sensors::Value::Sensor {
                input: "ram-slot".into(),
            },
        },
        sensors::Source {
            key: "mean".into(),
            value: sensors::Value::Mean {
                members: vec!["gpu".into(), "ram".into()],
            },
        },
    ];
    let mut runtime = stand.runtime(false);
    let temperature = Arc::new(std::sync::atomic::AtomicI32::new(42));
    runtime
        .attach_sensor(
            "gpu-slot",
            vec![],
            gpu_factory(temperature.clone(), Arc::new(AtomicUsize::new(0))),
        )
        .unwrap();
    let observed = now();
    runtime
        .accept_temperatures(
            &[lcd::Sample {
                key: "ram-slot".into(),
                celsius: 40.,
                observed_ms: observed / 1000,
            }],
            observed,
        )
        .unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        runtime.temperature_samples().len() == 3
    });
    assert_eq!(
        runtime
            .temperature_samples()
            .iter()
            .find(|s| s.key == "mean")
            .unwrap()
            .celsius,
        41.
    );
    let before = runtime.temperature_samples().to_vec();
    assert!(
        runtime
            .accept_temperatures(
                &[lcd::Sample {
                    key: "gpu-slot".into(),
                    celsius: 1.,
                    observed_ms: now() / 1000
                }],
                now()
            )
            .is_err()
    );
    assert_eq!(runtime.temperature_samples(), before);
    let mut bounded_inputs = (0..4095)
        .map(|n| lcd::Sample {
            key: format!("unused-{n}"),
            celsius: 1.,
            observed_ms: now() / 1000,
        })
        .collect::<Vec<_>>();
    bounded_inputs.push(lcd::Sample {
        key: "ram-slot".into(),
        celsius: 40.,
        observed_ms: now() / 1000,
    });
    runtime.accept_temperatures(&bounded_inputs, now()).unwrap();
    assert_eq!(runtime.injected_inputs.len(), 1);
    assert!(
        runtime
            .attach_sensor(
                "gpu-slot",
                vec![],
                gpu_factory(temperature.clone(), Arc::new(AtomicUsize::new(0)))
            )
            .is_err()
    );
    temperature.store(i32::MIN, Ordering::SeqCst);
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        runtime.sensor_status("gpu-slot").unwrap().error.is_some()
            && runtime.temperature_samples().iter().all(|s| s.key == "ram")
    });
    assert_eq!(
        runtime
            .temperature_samples()
            .iter()
            .map(|s| s.key.as_str())
            .collect::<Vec<_>>(),
        ["ram"]
    );
    runtime.tick(now() + 6_000_000, wall()).unwrap();
    assert!(runtime.temperature_samples().is_empty());
}

#[test]
fn sensor_specs_follow_logical_inputs_across_atomic_document_commit() {
    let mut stand = Stand::new();
    stand.document.temperatures.sources = vec![sensors::Source {
        key: "gpu".into(),
        value: sensors::Value::Sensor {
            input: "gpu-slot".into(),
        },
    }];
    let mut runtime = stand.runtime(false);
    let calls = Arc::new(AtomicUsize::new(0));
    runtime
        .attach_sensor(
            "gpu-slot",
            vec![],
            gpu_factory(
                Arc::new(std::sync::atomic::AtomicI32::new(42)),
                calls.clone(),
            ),
        )
        .unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        !runtime.temperature_samples().is_empty()
    });
    let mut renamed = runtime.document().clone();
    renamed.temperatures.sources[0].key = "new-gpu".into();
    runtime.request_document(renamed).unwrap();
    assert!(runtime.temperature_samples().is_empty());
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        !runtime.pending_configuration()
            && calls.load(Ordering::SeqCst) == 2
            && runtime
                .temperature_samples()
                .iter()
                .any(|s| s.key == "new-gpu")
    });
    let mut removed = runtime.document().clone();
    removed.temperatures.sources.clear();
    runtime.request_document(removed).unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        !runtime.pending_configuration()
    });
    assert!(runtime.sensors.is_empty());
    assert!(runtime.sensor_specs.is_empty());
    assert!(runtime.temperature_samples().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
#[test]
fn liquid_reopens_one_shot_mi00_on_two_second_cadence_and_clears_failed_values() {
    let mut stand = Stand::new();
    stand.document.temperatures.sources = vec![sensors::Source {
        key: "liquid".into(),
        value: sensors::Value::Sensor {
            input: "liquid-slot".into(),
        },
    }];
    let mut runtime = stand.runtime(false);
    let state = tt_state(&stand.root);
    let source = state.clone();
    super::super::sensor_sources::attach_liquid(
        &mut runtime,
        "liquid-slot",
        &state.liquid_path,
        move |_| Ok(TtBackend(source.clone())),
    )
    .unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        !runtime.temperature_samples().is_empty()
    });
    let first = runtime.temperature_samples()[0].clone();
    assert_eq!(first.celsius, 33.4);
    assert_eq!(state.opens.load(Ordering::SeqCst), 1);
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    std::thread::sleep(Duration::from_millis(1100));
    runtime.tick(now(), wall()).unwrap();
    assert_eq!(runtime.temperature_samples(), std::slice::from_ref(&first));
    assert_eq!(state.opens.load(Ordering::SeqCst), 1);
    state.mode.store(1, Ordering::SeqCst);
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        runtime
            .sensor_status("liquid-slot")
            .unwrap()
            .error
            .is_some()
            && runtime.temperature_samples().is_empty()
    });
    assert_eq!(
        state.closes.load(Ordering::SeqCst),
        state.opens.load(Ordering::SeqCst)
    );
    state.mode.store(0, Ordering::SeqCst);
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        runtime
            .temperature_samples()
            .first()
            .is_some_and(|s| s.observed_ms > first.observed_ms)
    });
    assert_eq!(runtime.temperature_samples()[0].celsius, 33.4);
    assert_eq!(
        state.writes.load(Ordering::SeqCst),
        state.reads.load(Ordering::SeqCst)
    );
}

#[test]
fn liquid_explicit_interface_and_writer_guard_do_not_infer_lcd_pairing_or_bypass_ownership() {
    let mut stand = Stand::new();
    stand.document.temperatures.sources = vec![sensors::Source {
        key: "liquid".into(),
        value: sensors::Value::Sensor {
            input: "liquid-slot".into(),
        },
    }];
    let mut runtime = stand.runtime(false);
    let mut other = stand.runtime(false);
    let state = tt_state(&stand.root);
    state.mode.store(3, Ordering::SeqCst);
    let source = state.clone();
    assert!(
        super::super::sensor_sources::attach_liquid(
            &mut runtime,
            "liquid-slot",
            &state.display_path,
            move |_| Ok(TtBackend(source.clone()))
        )
        .is_err()
    );
    let source = state.clone();
    super::super::sensor_sources::attach_liquid(
        &mut runtime,
        "liquid-slot",
        &state.liquid_path,
        move |_| Ok(TtBackend(source.clone())),
    )
    .unwrap();
    wait(|| {
        runtime
            .sensor_status("liquid-slot")
            .unwrap()
            .error
            .is_some()
    });
    assert_eq!(state.opens.load(Ordering::SeqCst), 0);
    let source = state.clone();
    assert!(
        super::super::sensor_sources::attach_liquid(
            &mut other,
            "liquid-slot",
            &state.liquid_path.to_uppercase(),
            move |_| Ok(TtBackend(source.clone()))
        )
        .is_err()
    );
    state.mode.store(0, Ordering::SeqCst);
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        !runtime.temperature_samples().is_empty()
    });
    assert_eq!(state.opens.load(Ordering::SeqCst), 1);
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
}

#[test]
fn blocked_mi00_status_read_does_not_delay_mi01_lcd_frames_and_late_result_is_discarded() {
    let mut stand = Stand::new();
    stand.document.temperatures.sources = vec![sensors::Source {
        key: "liquid".into(),
        value: sensors::Value::Sensor {
            input: "liquid-slot".into(),
        },
    }];
    let mut runtime = stand.runtime(false);
    let state = tt_state(&stand.root);
    let (send, receive) = std::sync::mpsc::channel();
    *state.blocked.lock().unwrap() = Some(receive);
    let source = state.clone();
    super::super::sensor_sources::attach_liquid(
        &mut runtime,
        "liquid-slot",
        &state.liquid_path,
        move |_| Ok(TtBackend(source.clone())),
    )
    .unwrap();
    let display = state.clone();
    let display_path = state.display_path.clone();
    let root = stand.root.join("assets");
    runtime
        .attach_lcd(
            "lcd",
            vec![format!("hid:{}", display_path.to_ascii_lowercase())],
            Arc::new(move |config, _| {
                let mut backend = TtBackend(display.clone());
                let transport = crate::lcd::hid_adapter::open_display(&mut backend, &display_path)?;
                Ok((
                    lcd::Controller::new(config, &Assets::new(root.clone())?)?,
                    Box::new(transport),
                ))
            }),
        )
        .unwrap();
    wait(|| state.reads.load(Ordering::SeqCst) == 1);
    let first = runtime.lcd_status("lcd").unwrap().frames_sent;
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        runtime.lcd_status("lcd").unwrap().frames_sent > first + 2
    });
    assert_eq!(state.closes.load(Ordering::SeqCst), 0);
    runtime.detach_sensor("liquid-slot").unwrap();
    send.send(()).unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        runtime.sensor_status("liquid-slot").is_none()
    });
    assert!(runtime.temperature_samples().is_empty());
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    assert!(state.display_packets.load(Ordering::SeqCst) > 0);
    assert_eq!(
        runtime.lcd_status("lcd").unwrap().phase,
        lcd_worker::Phase::Ready
    );
}
#[test]
fn failed_optional_sensor_does_not_break_lighting_resume_or_retry_itself() {
    let mut stand = Stand::new();
    stand.document.temperatures.sources = vec![sensors::Source {
        key: "gpu".into(),
        value: sensors::Value::Sensor {
            input: "gpu-slot".into(),
        },
    }];
    let mut runtime = stand.runtime(false);
    let calls = Arc::new(AtomicUsize::new(0));
    let started = calls.clone();
    runtime
        .attach_sensor(
            "gpu-slot",
            vec![],
            Arc::new(move |_| {
                started.fetch_add(1, Ordering::SeqCst);
                Err("injected unavailable optional GPU".into())
            }),
        )
        .unwrap();
    wait(|| {
        runtime.sensor_status("gpu-slot").unwrap().phase == crate::runtime::output::Phase::Failed
    });
    runtime.tick(now(), wall()).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let base = now();
    runtime.notify_power(true, base).unwrap();
    wait(|| runtime.drained());
    runtime.notify_power(false, base + 1000).unwrap();
    let mut clock = base + 7_002_000;
    wait(|| {
        clock += 1000;
        runtime.tick(clock, wall()).unwrap();
        !runtime.power.pending()
            && calls.load(Ordering::SeqCst) == 2
            && runtime.sensor_status("gpu-slot").unwrap().phase
                == crate::runtime::output::Phase::Failed
    });
    assert!(runtime.power.error().is_none());
    assert!(runtime.temperature_samples().is_empty());
    for _ in 0..8 {
        clock += 1000;
        runtime.tick(clock, wall()).unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
#[test]
fn detaching_a_blocked_reader_keeps_its_input_and_endpoint_owned_until_actual_drain() {
    struct Blocked {
        release: std::sync::mpsc::Receiver<()>,
        entered: Arc<AtomicUsize>,
    }
    impl sensor_worker::Provider for Blocked {
        fn poll(&mut self, at: u64) -> Result<Option<crate::sensors::Reading>, String> {
            self.entered.fetch_add(1, Ordering::SeqCst);
            self.release.recv().map_err(|e| e.to_string())?;
            Ok(Some(crate::sensors::Reading {
                celsius: 42.,
                observed_ms: at,
            }))
        }
    }
    let mut stand = Stand::new();
    stand.document.temperatures.sources = vec![sensors::Source {
        key: "gpu".into(),
        value: sensors::Value::Sensor {
            input: "gpu-slot".into(),
        },
    }];
    let mut runtime = stand.runtime(false);
    let mut other = stand.runtime(false);
    let entered = Arc::new(AtomicUsize::new(0));
    let (send, receive) = std::sync::mpsc::channel();
    let receive = Arc::new(Mutex::new(Some(receive)));
    let seen = entered.clone();
    let key = format!("sensor-stand:{}", stand.root.display());
    runtime
        .attach_sensor(
            "gpu-slot",
            vec![key.clone()],
            Arc::new(move |_| {
                Ok(Box::new(Blocked {
                    release: receive.lock().unwrap().take().ok_or("factory reused")?,
                    entered: seen.clone(),
                }))
            }),
        )
        .unwrap();
    wait(|| entered.load(Ordering::SeqCst) == 1);
    runtime.detach_sensor("gpu-slot").unwrap();
    assert!(runtime.temperature_samples().is_empty());
    assert!(
        runtime
            .accept_temperatures(
                &[lcd::Sample {
                    key: "gpu-slot".into(),
                    celsius: 99.,
                    observed_ms: now() / 1000
                }],
                now()
            )
            .is_err()
    );
    assert!(!runtime.drained());
    let fresh = gpu_factory(
        Arc::new(std::sync::atomic::AtomicI32::new(43)),
        Arc::new(AtomicUsize::new(0)),
    );
    assert!(
        runtime
            .attach_sensor("gpu-slot", vec![key.clone()], fresh.clone())
            .is_err()
    );
    assert!(
        other
            .attach_sensor("gpu-slot", vec![key.clone()], fresh.clone())
            .is_err()
    );
    send.send(()).unwrap();
    wait(|| runtime.drained());
    assert!(runtime.sensor_status("gpu-slot").is_none());
    assert!(runtime.temperature_samples().is_empty());
    runtime.attach_sensor("gpu-slot", vec![key], fresh).unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        runtime
            .temperature_samples()
            .first()
            .is_some_and(|s| s.celsius == 43.)
    });
}

#[cfg(windows)]
#[test]
fn gpu_backend_through_worker_logical_selection_and_lcd_matches_actual_overlay_jpeg() {
    let mut stand = Stand::new();
    let assets = Assets::new(stand.root.join("assets")).unwrap();
    let font =
        fs::read(PathBuf::from(std::env::var_os("WINDIR").unwrap()).join("Fonts/seguibl.ttf"))
            .unwrap();
    let font = assets.put(&font, 8 * 1024 * 1024).unwrap();
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(16, 16, image::Rgb([80, 10, 0])))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    stand.document.lcd[0].config.media = Some(assets.put(png.get_ref(), 32 * 1024 * 1024).unwrap());
    stand.document.lcd[0].config.font = Some(font);
    stand.document.lcd[0].config.overlay_enabled = true;
    stand.document.lcd[0].config.sources = vec![lcd::OverlaySource {
        key: "gpu".into(),
        label: "GPU".into(),
    }];
    stand.document.temperatures.sources = vec![sensors::Source {
        key: "gpu".into(),
        value: sensors::Value::Sensor {
            input: "gpu-slot".into(),
        },
    }];
    let mut runtime = stand.runtime(false);
    runtime
        .attach_sensor(
            "gpu-slot",
            vec![],
            gpu_factory(
                Arc::new(std::sync::atomic::AtomicI32::new(42)),
                Arc::new(AtomicUsize::new(0)),
            ),
        )
        .unwrap();
    runtime
        .attach_lcd("lcd", vec![], stand.lcd_factory())
        .unwrap();
    let mut expected = lcd::Controller::new(stand.document.lcd[0].config.clone(), &assets).unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        if runtime.temperature_samples().is_empty() {
            return false;
        }
        expected
            .poll(&mut Reports, true, runtime.temperature_samples())
            .unwrap();
        let snapshot = runtime.lcd_status("lcd").unwrap();
        snapshot.processed_revision >= runtime.revision
            && snapshot.preview.as_ref() == expected.preview()
    });
    assert!(!expected.preview().is_empty());
    let wrong = [lcd::Sample {
        key: "gpu".into(),
        celsius: 99.,
        observed_ms: now() / 1000,
    }];
    let mut expected = lcd::Controller::new(stand.document.lcd[0].config.clone(), &assets).unwrap();
    expected.poll(&mut Reports, true, &wrong).unwrap();
    assert_ne!(
        runtime.lcd_status("lcd").unwrap().preview.as_ref(),
        expected.preview()
    );
}
#[test]
fn resume_without_suspend_drains_old_owner_and_duplicate_resume_keeps_refresh_generation() {
    let stand = Stand::new();
    let mut runtime = stand.runtime(false);
    let calls = Arc::new(AtomicUsize::new(0));
    runtime
        .attach_rgb(
            "body",
            vec![],
            rgb_factory(
                &stand.document,
                "body",
                Arc::new(Record::default()),
                calls.clone(),
            ),
        )
        .unwrap();
    wait(|| calls.load(Ordering::SeqCst) == 1);
    let base = now();
    runtime.notify_power(false, base).unwrap();
    wait(|| runtime.drained());
    runtime.tick(base + 7_001_000, wall()).unwrap();
    let generation = runtime.refresh;
    // Let workers publish readiness without ticking the power coordinator again.
    wait(|| runtime.outputs_ready() == Ok(true));
    assert!(runtime.power.pending());
    assert!(!runtime.temperature_input_ready());
    let document = runtime.document().clone();
    let outputs = runtime.output_plan();
    assert!(runtime.request_outputs_for(document, outputs).is_err());
    runtime.notify_power(false, base + 7_002_000).unwrap();
    assert_eq!(runtime.refresh, generation);
    wait(|| {
        runtime.tick(base + 7_003_000, wall()).unwrap();
        !runtime.power.pending()
    });
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(runtime.temperature_input_ready());
    let document = runtime.document().clone();
    let outputs = runtime.output_plan();
    runtime.request_outputs_for(document, outputs).unwrap();
}
#[test]
fn terminal_capture_invalidation_removes_old_host_color() {
    use std::sync::atomic::AtomicBool;
    struct FailingPixels(Arc<AtomicBool>);
    impl ambient::Provider for FailingPixels {
        fn poll(
            &mut self,
            source: &str,
            crop: Option<Rect>,
            requested_ms: u64,
        ) -> Result<Option<Frame>, String> {
            assert!(
                !self.0.load(Ordering::SeqCst),
                "Injected terminal capture panic"
            );
            Pixels.poll(source, crop, requested_ms)
        }
    }
    let stand = Stand::new();
    let mut runtime = stand.runtime(false);
    let record = Arc::new(Record::default());
    runtime
        .attach_rgb(
            "ambient",
            vec![],
            rgb_factory(
                &stand.document,
                "ambient",
                record.clone(),
                Arc::new(AtomicUsize::new(0)),
            ),
        )
        .unwrap();
    let failed = Arc::new(AtomicBool::new(false));
    let flag = failed.clone();
    runtime
        .attach_capture(Arc::new(move |_| Ok(Box::new(FailingPixels(flag.clone())))))
        .unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        record
            .last()
            .is_some_and(|v| v.iter().any(|c| *c != Rgb::default()))
    });
    failed.store(true, Ordering::SeqCst);
    wait(|| runtime.capture_status().unwrap().phase == capture_worker::Phase::Failed);
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        record
            .last()
            .is_some_and(|v| v.iter().all(|c| *c == Rgb::default()))
    });
    let snapshot = runtime.capture_status().unwrap();
    assert!(snapshot.frames.is_empty());
    assert!(snapshot.error.is_some());
}
#[test]
fn temperature_revisions_do_not_cancel_an_unchanged_capture_intent() {
    use std::sync::{Condvar, atomic::AtomicBool};
    struct Release(Arc<(Mutex<bool>, Condvar)>);
    impl Release {
        fn open(&self) {
            let (lock, wake) = &*self.0;
            *lock.lock().unwrap() = true;
            wake.notify_all();
        }
    }
    impl Drop for Release {
        fn drop(&mut self) {
            self.open();
        }
    }
    struct Gated {
        gate: Arc<(Mutex<bool>, Condvar)>,
        entered: Arc<AtomicBool>,
    }
    impl ambient::Provider for Gated {
        fn poll(
            &mut self,
            source: &str,
            crop: Option<Rect>,
            requested_ms: u64,
        ) -> Result<Option<Frame>, String> {
            self.entered.store(true, Ordering::SeqCst);
            let (lock, wake) = &*self.gate;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = wake.wait(open).unwrap();
            }
            drop(open);
            Pixels.poll(source, crop, requested_ms)
        }
    }
    let stand = Stand::new();
    let mut runtime = stand.runtime(false);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let release = Release(gate.clone());
    let entered = Arc::new(AtomicBool::new(false));
    let observed = entered.clone();
    runtime
        .attach_capture(Arc::new(move |_| {
            Ok(Box::new(Gated {
                gate: gate.clone(),
                entered: observed.clone(),
            }))
        }))
        .unwrap();
    runtime.tick(now(), wall()).unwrap();
    wait(|| entered.load(Ordering::SeqCst));
    let accepted = runtime.capture_status().unwrap().last_accepted_revision;
    for _ in 0..64 {
        runtime.accept_temperatures(&[], now()).unwrap();
        runtime.tick(now(), wall()).unwrap();
    }
    assert_eq!(
        runtime.capture_status().unwrap().last_accepted_revision,
        accepted
    );
    release.open();
    wait(|| runtime.capture_status().unwrap().frame_revision > 0);
    assert!(
        runtime.capture_status().unwrap().frames["ambient"]
            .iter()
            .any(|c| *c != Rgb::default())
    );
}
#[test]
fn refresh_does_not_invoke_replacement_factory_while_old_factory_still_owns_its_thread() {
    use std::sync::Condvar;
    struct Release(Arc<(Mutex<bool>, Condvar)>);
    impl Release {
        fn open(&self) {
            let (lock, wake) = &*self.0;
            *lock.lock().unwrap() = true;
            wake.notify_all();
        }
    }
    impl Drop for Release {
        fn drop(&mut self) {
            self.open();
        }
    }
    let stand = Stand::new();
    let mut runtime = stand.runtime(false);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let release = Release(gate.clone());
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = calls.clone();
    runtime
        .attach_capture(Arc::new(move |_| {
            entered.fetch_add(1, Ordering::SeqCst);
            let (lock, wake) = &*gate;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = wake.wait(open).unwrap();
            }
            Ok(Box::new(Pixels))
        }))
        .unwrap();
    wait(|| calls.load(Ordering::SeqCst) == 1);
    let base = now();
    runtime.refresh_devices(base).unwrap();
    runtime.tick(base + 8_000_000, wall()).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!runtime.drained());
    release.open();
    wait(|| runtime.drained());
    runtime.tick(base + 8_001_000, wall()).unwrap();
    wait(|| calls.load(Ordering::SeqCst) == 2);
    wait(|| {
        runtime.tick(base + 8_002_000, wall()).unwrap();
        !runtime.power.pending()
    });
}
#[test]
fn failed_revalidation_requires_explicit_refresh_before_another_factory_attempt() {
    let stand = Stand::new();
    let mut runtime = stand.runtime(false);
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = calls.clone();
    runtime
        .attach_capture(Arc::new(move |_| {
            if entered.fetch_add(1, Ordering::SeqCst) == 1 {
                return Err("Injected refresh failure".into());
            }
            Ok(Box::new(Pixels))
        }))
        .unwrap();
    wait(|| calls.load(Ordering::SeqCst) == 1);
    let base = now();
    runtime.refresh_devices(base).unwrap();
    wait(|| runtime.drained());
    runtime.tick(base + 7_001_000, wall()).unwrap();
    wait(|| runtime.capture_status().unwrap().phase == capture_worker::Phase::Failed);
    runtime.tick(base + 7_002_000, wall()).unwrap();
    assert!(runtime.error().is_some());
    for _ in 0..4 {
        runtime.tick(base + 7_003_000, wall()).unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    runtime.refresh_devices(base + 8_000_000).unwrap();
    wait(|| runtime.drained());
    runtime.tick(base + 16_000_000, wall()).unwrap();
    wait(|| calls.load(Ordering::SeqCst) == 3);
    wait(|| {
        runtime.tick(base + 16_001_000, wall()).unwrap();
        !runtime.power.pending()
    });
    assert!(runtime.error().is_none());
}

struct ConfigurationGate(Arc<(Mutex<bool>, std::sync::Condvar)>);
impl ConfigurationGate {
    fn new() -> Self {
        Self(Arc::new((Mutex::new(false), std::sync::Condvar::new())))
    }
    fn open(&self) {
        let (lock, wake) = &*self.0;
        *lock.lock().unwrap() = true;
        wake.notify_all();
    }
}
impl Drop for ConfigurationGate {
    fn drop(&mut self) {
        self.open();
    }
}

#[test]
fn lighting_shutdown_waits_for_a_blocked_sensor_factory_without_releasing_its_lease() {
    use crate::app::shutdown::{Phase, Shutdown};
    let mut stand = Stand::new();
    stand.document.temperatures.sources = vec![sensors::Source {
        key: "gpu".into(),
        value: sensors::Value::Sensor {
            input: "shutdown-gpu".into(),
        },
    }];
    let mut runtime = stand.runtime(false);
    let mut other = stand.runtime(false);
    let gate = ConfigurationGate::new();
    let waiting = gate.0.clone();
    let entered = Arc::new(AtomicUsize::new(0));
    let observed = entered.clone();
    let key = format!("shutdown-sensor:{}", stand.root.display());
    runtime
        .attach_sensor(
            "shutdown-gpu",
            vec![key.clone()],
            Arc::new(move |_| {
                observed.fetch_add(1, Ordering::SeqCst);
                let (lock, wake) = &*waiting;
                let mut open = lock.lock().unwrap();
                while !*open {
                    open = wake.wait(open).unwrap();
                }
                Ok(Box::new(crate::sensors::nvidia::Provider::new(
                    gpu_binding(),
                    NvTemperature(Arc::new(std::sync::atomic::AtomicI32::new(42))),
                )?))
            }),
        )
        .unwrap();
    wait(|| entered.load(Ordering::SeqCst) == 1);
    let mut close = Shutdown::default();
    for _ in 0..3 {
        close.request(&mut runtime);
        assert_eq!(close.phase(), Phase::Waiting);
    }
    let fresh = gpu_factory(
        Arc::new(std::sync::atomic::AtomicI32::new(43)),
        Arc::new(AtomicUsize::new(0)),
    );
    assert!(
        other
            .attach_sensor("shutdown-gpu", vec![key.clone()], fresh.clone())
            .is_err()
    );
    gate.open();
    wait(|| {
        close.poll(&mut runtime);
        close.phase() == Phase::Ready
    });
    other
        .attach_sensor("shutdown-gpu", vec![key], fresh)
        .unwrap();
    other.stop();
    wait(|| other.drained());
}

#[test]
fn queued_document_waits_for_actual_drain_then_preserves_intent_and_passes_new_lcd_config() {
    let stand = Stand::new();
    let mut runtime = stand.runtime(true);
    let calls = Arc::new(AtomicUsize::new(0));
    runtime
        .attach_rgb(
            "body",
            vec![],
            rgb_factory(
                &stand.document,
                "body",
                Arc::new(Record::default()),
                calls.clone(),
            ),
        )
        .unwrap();
    let configs = Arc::new(Mutex::new(Vec::<lcd::Config>::new()));
    let saved = configs.clone();
    let assets = stand.root.join("assets");
    runtime
        .attach_lcd(
            "lcd",
            vec![],
            Arc::new(move |config, _| {
                saved.lock().unwrap().push(config.clone());
                let store = Assets::new(assets.clone())?;
                Ok((lcd::Controller::new(config, &store)?, Box::new(Reports)))
            }),
        )
        .unwrap();
    let gate = ConfigurationGate::new();
    let held = gate.0.clone();
    let captures = Arc::new(AtomicUsize::new(0));
    let started = captures.clone();
    runtime
        .attach_capture(Arc::new(move |_| {
            started.fetch_add(1, Ordering::SeqCst);
            let (lock, wake) = &*held;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = wake.wait(open).unwrap();
            }
            Ok(Box::new(Pixels))
        }))
        .unwrap();
    wait(|| {
        calls.load(Ordering::SeqCst) == 1
            && captures.load(Ordering::SeqCst) == 1
            && configs.lock().unwrap().len() == 1
    });
    runtime.tick(now(), wall()).unwrap();
    runtime.set_group(Target::Pc, false).unwrap();
    let prior = runtime.document().clone();
    let colors = runtime.colors().to_vec();
    let journal = fs::read(stand.root.join("journal.json")).unwrap();
    let mut candidate = prior.clone();
    candidate.lighting.setup.canvas.width = 40;
    candidate.lighting.setup.canvas.points[0].x = 35;
    candidate.lighting.scene.brightness = 35;
    candidate.lcd[0].config.rotation = 90;
    runtime.request_document(candidate.clone()).unwrap();
    assert!(runtime.pending_configuration());
    assert!(runtime.request_document(candidate.clone()).is_err());
    runtime.tick(now(), wall()).unwrap();
    assert_eq!(runtime.document(), &prior);
    assert_eq!(runtime.project(), &prior.lighting);
    assert_eq!(runtime.colors(), colors);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(fs::read(stand.root.join("journal.json")).unwrap(), journal);
    runtime.save(&stand.root.join("proposal.json")).unwrap();
    assert_eq!(
        Document::load(&stand.root.join("proposal.json")).unwrap(),
        candidate
    );
    gate.open();
    wait(|| runtime.drained());
    runtime.tick(now(), wall()).unwrap();
    assert!(!runtime.pending_configuration());
    assert_eq!(runtime.document(), &candidate);
    assert!(!runtime.group_on(Target::Pc));
    assert!(runtime.group_on(Target::Door));
    wait(|| {
        calls.load(Ordering::SeqCst) == 2
            && captures.load(Ordering::SeqCst) == 2
            && configs.lock().unwrap().len() == 2
    });
    assert_eq!(configs.lock().unwrap()[1], candidate.lcd[0].config);
    assert!(
        runtime
            .colors()
            .iter()
            .zip(&runtime.point_targets)
            .all(|(color, target)| *target != Target::Pc || *color == Rgb::default())
    );
}

#[test]
fn failed_configuration_persistence_retains_applied_model_without_automatic_retry_and_can_retry_or_cancel()
 {
    let stand = Stand::new();
    let mut runtime = stand.runtime(true);
    let calls = Arc::new(AtomicUsize::new(0));
    runtime
        .attach_rgb(
            "body",
            vec![],
            rgb_factory(
                &stand.document,
                "body",
                Arc::new(Record::default()),
                calls.clone(),
            ),
        )
        .unwrap();
    wait(|| calls.load(Ordering::SeqCst) == 1);
    runtime.tick(now(), wall()).unwrap();
    runtime.set_group(Target::Door, false).unwrap();
    let applied = runtime.document().clone();
    let scene = runtime.host.scene().clone();
    let geometry = runtime.host.setup().clone();
    let colors = runtime.colors().to_vec();
    let ledger = runtime.schedule.encoded().unwrap();
    let fingerprint = runtime.store.fingerprint.clone();
    let mut candidate = applied.clone();
    candidate.lighting.scene.brightness = 34;
    candidate.lighting.setup.canvas.width = 40;
    let path = stand.root.join("journal.json");
    let old = fs::read(&path).unwrap();
    runtime.request_document(candidate.clone()).unwrap();
    wait(|| runtime.drained());
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(runtime.tick(now(), wall()).is_err());
    assert!(runtime.configuration_error().is_some());
    assert!(runtime.pending_configuration());
    assert_eq!(runtime.document(), &applied);
    assert_eq!(runtime.host.scene(), &scene);
    assert_eq!(runtime.host.setup(), &geometry);
    assert_eq!(runtime.colors(), colors);
    assert_eq!(runtime.store.fingerprint, fingerprint);
    assert_eq!(runtime.schedule.encoded().unwrap(), ledger);
    assert!(!runtime.group_on(Target::Door));
    fs::remove_dir(&path).unwrap();
    fs::write(&path, &old).unwrap();
    for _ in 0..3 {
        runtime.tick(now(), wall()).unwrap();
    }
    assert_eq!(runtime.document(), &applied);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(fs::read(&path).unwrap(), old);
    runtime.retry_configuration().unwrap();
    runtime.tick(now(), wall()).unwrap();
    assert_eq!(runtime.document(), &candidate);
    assert!(!runtime.pending_configuration());
    wait(|| calls.load(Ordering::SeqCst) == 2);
    let committed = runtime.document().clone();
    let mut second = committed.clone();
    second.lighting.scene.brightness = 79;
    runtime.request_document(second).unwrap();
    wait(|| runtime.drained());
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(runtime.tick(now(), wall()).is_err());
    runtime.cancel_configuration().unwrap();
    runtime.tick(now(), wall()).unwrap();
    assert!(!runtime.pending_configuration());
    assert_eq!(runtime.document(), &committed);
    assert!(!runtime.group_on(Target::Door));
    wait(|| calls.load(Ordering::SeqCst) == 3);
}

#[test]
fn changed_capability_or_capture_source_requires_rebind_and_new_temperature_sources_clear_cached_samples()
 {
    let mut stand = Stand::new();
    stand.document.temperatures.sources = vec![sensors::Source {
        key: "cpu".into(),
        value: sensors::Value::Sensor {
            input: "slot".into(),
        },
    }];
    let mut runtime = stand.runtime(false);
    let calls = Arc::new(AtomicUsize::new(0));
    let captures = Arc::new(AtomicUsize::new(0));
    runtime
        .attach_rgb(
            "body",
            vec![],
            rgb_factory(
                &stand.document,
                "body",
                Arc::new(Record::default()),
                calls.clone(),
            ),
        )
        .unwrap();
    runtime
        .attach_lcd("lcd", vec![], stand.lcd_factory())
        .unwrap();
    let started = captures.clone();
    runtime
        .attach_capture(Arc::new(move |_| {
            started.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Pixels))
        }))
        .unwrap();
    wait(|| calls.load(Ordering::SeqCst) == 1 && captures.load(Ordering::SeqCst) == 1);
    let observed = now();
    runtime
        .accept_temperatures(
            &[lcd::Sample {
                key: "slot".into(),
                celsius: 42.,
                observed_ms: observed / 1000,
            }],
            observed,
        )
        .unwrap();
    assert_eq!(runtime.temperature_samples().len(), 1);
    let mut candidate = runtime.document().clone();
    candidate.lighting.setup.devices[0].driver = Driver::Fusion2 { headers: [4, 4] };
    candidate.ambient.sources[0].key = "replacement".into();
    candidate.ambient.routes[0].source = "replacement".into();
    candidate.temperatures.sources = vec![sensors::Source {
        key: "gpu".into(),
        value: sensors::Value::Sensor {
            input: "new-slot".into(),
        },
    }];
    runtime.request_document(candidate.clone()).unwrap();
    wait(|| runtime.drained());
    runtime.tick(now(), wall()).unwrap();
    assert_eq!(runtime.document(), &candidate);
    assert!(!runtime.rgb_specs.contains_key("body"));
    assert!(runtime.host.status("body").is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(runtime.capture_factory.is_none());
    assert!(runtime.capture_status().is_none());
    assert!(runtime.host.external_feed_generation().is_none());
    assert_eq!(captures.load(Ordering::SeqCst), 1);
    assert!(runtime.temperature_samples().is_empty());
    assert!(runtime.lcd_specs.contains_key("lcd"));
    assert!(runtime.lcd_status("lcd").is_some());
}

#[test]
fn invalid_document_is_rejected_before_stop_and_pending_does_not_allow_applied_scene_edits() {
    let stand = Stand::new();
    let mut runtime = stand.runtime(false);
    let calls = Arc::new(AtomicUsize::new(0));
    runtime
        .attach_rgb(
            "body",
            vec![],
            rgb_factory(
                &stand.document,
                "body",
                Arc::new(Record::default()),
                calls.clone(),
            ),
        )
        .unwrap();
    wait(|| runtime.host.status("body").unwrap().phase == crate::runtime::output::Phase::Ready);
    let mut invalid = runtime.document().clone();
    invalid.lcd[0].config.media = None;
    let revision = runtime.view_revision();
    assert!(runtime.request_document(invalid).is_err());
    assert!(!runtime.pending_configuration());
    assert_eq!(runtime.view_revision(), revision);
    assert_eq!(
        runtime.host.status("body").unwrap().phase,
        crate::runtime::output::Phase::Ready
    );
    let mut candidate = runtime.document().clone();
    candidate.lighting.scene.brightness = 20;
    runtime.request_document(candidate).unwrap();
    let mut scene = runtime.project().scene.clone();
    scene.brightness = 40;
    assert!(runtime.set_scene(scene).is_err());
    assert!(
        runtime
            .configure_schedule(schedule::Config::default())
            .is_err()
    );
    runtime.cancel_configuration().unwrap();
    wait(|| runtime.drained());
    runtime.tick(now(), wall()).unwrap();
    assert!(!runtime.pending_configuration());
    runtime.notify_power(true, now()).unwrap();
    assert!(runtime.request_document(stand.document.clone()).is_err());
}

#[test]
fn reusable_canvas_view_revision_masks_groups_and_observes_cached_temperature_expiry() {
    let mut stand = Stand::new();
    stand.document.temperatures.sources = vec![sensors::Source {
        key: "cpu".into(),
        value: sensors::Value::Sensor {
            input: "slot".into(),
        },
    }];
    let mut runtime = stand.runtime(false);
    let base = now();
    runtime.tick(base, wall()).unwrap();
    assert_eq!(runtime.project().scene.speed, 8);
    assert_eq!(runtime.colors(), runtime.host.preview());
    let address = runtime.colors().as_ptr();
    let before = runtime.view_revision();
    runtime.set_group(Target::Pc, false).unwrap();
    assert_ne!(runtime.view_revision(), before);
    let unchanged = runtime.view_revision();
    runtime.set_group(Target::Pc, false).unwrap();
    assert_eq!(runtime.view_revision(), unchanged);
    for (point, color) in runtime
        .project()
        .setup
        .canvas
        .points
        .iter()
        .zip(runtime.colors())
    {
        if runtime.project().groups.pc.contains(&point.device) {
            assert_eq!(*color, Rgb::default());
        }
    }
    runtime.set_group(Target::Door, false).unwrap();
    runtime
        .accept_temperatures(
            &[lcd::Sample {
                key: "slot".into(),
                celsius: 42.,
                observed_ms: base / 1000,
            }],
            base,
        )
        .unwrap();
    runtime.tick(base + 1000, wall()).unwrap();
    let before = runtime.view_revision();
    runtime.tick(base + 2000, wall()).unwrap();
    assert_eq!(runtime.view_revision(), before);
    runtime.tick(base + 5_002_000, wall()).unwrap();
    assert_ne!(runtime.view_revision(), before);
    assert_eq!(runtime.colors().as_ptr(), address);
    assert!(runtime.temperature_samples().is_empty());
    assert!(runtime.colors().iter().all(|c| *c == Rgb::default()));
}

#[test]
fn committed_document_survives_terminal_new_lcd_factory_error_without_rollback_or_automatic_retry()
{
    let stand = Stand::new();
    let mut runtime = stand.runtime(true);
    let calls = Arc::new(AtomicUsize::new(0));
    let started = calls.clone();
    let assets = stand.root.join("assets");
    runtime
        .attach_lcd(
            "lcd",
            vec![],
            Arc::new(move |config, _| {
                started.fetch_add(1, Ordering::SeqCst);
                if config.rotation == 90 {
                    return Err("Injected new LCD configuration failure".into());
                }
                let store = Assets::new(assets.clone())?;
                Ok((lcd::Controller::new(config, &store)?, Box::new(Reports)))
            }),
        )
        .unwrap();
    wait(|| calls.load(Ordering::SeqCst) == 1);
    let mut candidate = runtime.document().clone();
    candidate.lcd[0].config.rotation = 90;
    runtime.request_document(candidate.clone()).unwrap();
    wait(|| runtime.drained());
    runtime.tick(now(), wall()).unwrap();
    wait(|| runtime.lcd_status("lcd").unwrap().phase == lcd_worker::Phase::Failed);
    for _ in 0..3 {
        runtime.tick(now(), wall()).unwrap();
    }
    assert_eq!(runtime.document(), &candidate);
    assert!(!runtime.pending_configuration());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn explicit_configuration_can_recover_prior_power_error_after_drain() {
    let stand = Stand::new();
    let mut runtime = stand.runtime(false);
    let calls = Arc::new(AtomicUsize::new(0));
    let started = calls.clone();
    runtime
        .attach_capture(Arc::new(move |_| {
            if started.fetch_add(1, Ordering::SeqCst) == 1 {
                return Err("Injected old power refresh failure".into());
            }
            Ok(Box::new(Pixels))
        }))
        .unwrap();
    wait(|| calls.load(Ordering::SeqCst) == 1);
    let base = now();
    runtime.refresh_devices(base).unwrap();
    wait(|| runtime.drained());
    runtime.tick(base + 7_001_000, wall()).unwrap();
    wait(|| runtime.capture_status().unwrap().phase == capture_worker::Phase::Failed);
    runtime.tick(base + 7_002_000, wall()).unwrap();
    assert!(runtime.power.error().is_some());
    let mut candidate = runtime.document().clone();
    candidate.lighting.scene.brightness = 44;
    candidate.ambient.sources[0].key = "rebound".into();
    candidate.ambient.routes[0].source = "rebound".into();
    runtime.request_document(candidate.clone()).unwrap();
    wait(|| runtime.drained());
    runtime.tick(base + 7_003_000, wall()).unwrap();
    assert_eq!(runtime.document(), &candidate);
    assert!(runtime.power.error().is_none());
    assert!(!runtime.power.pending());
    assert!(runtime.error().is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn topology_commit_uses_candidate_journal_and_leaves_old_profile_reopenable() {
    let stand = Stand::new();
    let a = stand.root.join("journal.json");
    let b = stand.root.join("journal-b.json");
    let mut runtime = stand.runtime(true);
    runtime.set_group(Target::Pc, false).unwrap();
    runtime.set_group(Target::Door, false).unwrap();
    let original = runtime.document().clone();
    let a_bytes = fs::read(&a).unwrap();
    let mut candidate = original.clone();
    candidate.lighting.setup.devices[0].rate_hz = 25;
    runtime
        .request_document_at(candidate.clone(), b.clone())
        .unwrap();
    assert_eq!(runtime.store.path, Some(a.clone()));
    assert!(!b.exists());
    runtime.tick(now(), wall()).unwrap();
    assert_eq!(runtime.document(), &candidate);
    assert_eq!(runtime.store.path, Some(b.clone()));
    assert_eq!(fs::read(&a).unwrap(), a_bytes);
    let reopened_a = Runtime::new(original, Some(a)).unwrap();
    assert!(!reopened_a.group_on(Target::Pc));
    let reopened_b = Runtime::new(candidate, Some(b)).unwrap();
    assert!(!reopened_b.group_on(Target::Pc));
    assert!(!reopened_b.group_on(Target::Door));
}

#[test]
fn failed_candidate_journal_cannot_switch_applied_path_and_cancel_keeps_old_store() {
    let stand = Stand::new();
    let a = stand.root.join("journal.json");
    let bad = stand.root.join("blocked-journal");
    fs::create_dir(&bad).unwrap();
    let mut runtime = stand.runtime(true);
    runtime.set_group(Target::Door, false).unwrap();
    let original = runtime.document().clone();
    let a_bytes = fs::read(&a).unwrap();
    let mut candidate = original.clone();
    candidate.lighting.setup.devices[0].rate_hz = 25;
    runtime.request_document_at(candidate, bad).unwrap();
    assert!(runtime.tick(now(), wall()).is_err());
    assert_eq!(runtime.document(), &original);
    assert_eq!(runtime.store.path, Some(a.clone()));
    assert_eq!(fs::read(&a).unwrap(), a_bytes);
    assert!(runtime.configuration_error().is_some());
    runtime.cancel_configuration().unwrap();
    runtime.tick(now(), wall()).unwrap();
    assert_eq!(runtime.store.path, Some(a));
    assert_eq!(runtime.document(), &original);
    assert!(!runtime.group_on(Target::Door));
}

#[test]
fn pending_save_as_writes_only_candidate_intent_and_can_be_reopened_before_application() {
    let stand = Stand::new();
    let a = stand.root.join("journal.json");
    let queued = stand.root.join("queued.json");
    let saved = stand.root.join("saved-journal.json");
    let profile = stand.root.join("saved-document.json");
    let mut runtime = stand.runtime(true);
    runtime.set_group(Target::Pc, false).unwrap();
    let original = runtime.document().clone();
    let a_bytes = fs::read(&a).unwrap();
    let ledger = runtime.schedule.encoded().unwrap();
    let mut candidate = original.clone();
    candidate.lighting.setup.devices[0].rate_hz = 25;
    candidate.lighting.schedule.paused = true;
    runtime
        .request_document_at(candidate.clone(), queued.clone())
        .unwrap();
    runtime.save(&profile).unwrap();
    runtime.relocate_journal(saved.clone()).unwrap();
    assert_eq!(runtime.document(), &original);
    assert_eq!(runtime.store.path, Some(a.clone()));
    assert!(runtime.pending_configuration());
    assert_eq!(runtime.schedule.encoded().unwrap(), ledger);
    assert_eq!(fs::read(&a).unwrap(), a_bytes);
    assert!(!queued.exists());
    let reopened = Runtime::new(Document::load(&profile).unwrap(), Some(saved.clone())).unwrap();
    assert_eq!(reopened.document(), &candidate);
    assert!(!reopened.group_on(Target::Pc));
    runtime.tick(now(), wall()).unwrap();
    assert_eq!(runtime.store.path, Some(saved));
    assert_eq!(runtime.document(), &candidate);
    assert_eq!(fs::read(&a).unwrap(), a_bytes);
}

#[test]
fn normal_save_as_and_failed_pending_relocation_switch_only_after_success() {
    let stand = Stand::new();
    let a = stand.root.join("journal.json");
    let saved = stand.root.join("save-as.json");
    let bad = stand.root.join("blocked");
    fs::create_dir(&bad).unwrap();
    let mut runtime = stand.runtime(true);
    runtime.set_group(Target::Pc, false).unwrap();
    let a_bytes = fs::read(&a).unwrap();
    assert!(runtime.relocate_journal(bad.clone()).is_err());
    assert_eq!(runtime.store.path, Some(a.clone()));
    runtime.relocate_journal(saved.clone()).unwrap();
    assert_eq!(runtime.store.path, Some(saved.clone()));
    assert_eq!(fs::read(&a).unwrap(), a_bytes);
    let reopened = Runtime::new(runtime.document().clone(), Some(saved.clone())).unwrap();
    assert!(!reopened.group_on(Target::Pc));
    let mut candidate = runtime.document().clone();
    candidate.lighting.setup.devices[0].rate_hz = 25;
    let target = stand.root.join("candidate.json");
    runtime
        .request_document_at(candidate.clone(), target.clone())
        .unwrap();
    assert!(runtime.relocate_journal(bad).is_err());
    assert_eq!(runtime.store.path, Some(saved));
    assert_eq!(
        runtime.pending.as_ref().unwrap().journal_override,
        Some(target.clone())
    );
    assert!(!target.exists());
    runtime.cancel_configuration().unwrap();
    runtime.tick(now(), wall()).unwrap();
    assert!(!runtime.pending_configuration());
    assert!(!target.exists());
}

#[test]
fn editor_draft_export_preserves_intent_without_queuing_applying_or_relocating_runtime() {
    let stand = Stand::new();
    let a = stand.root.join("journal.json");
    let b = stand.root.join("draft-journal.json");
    let profile = stand.root.join("draft.json");
    let mut runtime = stand.runtime(true);
    runtime.set_group(Target::Pc, false).unwrap();
    let original = runtime.document().clone();
    let ledger = runtime.schedule.encoded().unwrap();
    let a_bytes = fs::read(&a).unwrap();
    let revision = runtime.view_revision();
    let mut draft = original.clone();
    draft.lighting.setup.devices[0].rate_hz = 25;
    draft.lighting.scene.brightness = 7;
    draft.lighting.schedule.times = [660, 1080, 180];
    draft.save(&profile).unwrap();
    runtime.export_journal_for(&draft, b.clone()).unwrap();
    assert_eq!(runtime.document(), &original);
    assert_eq!(runtime.store.path, Some(a.clone()));
    assert!(!runtime.pending_configuration());
    assert_eq!(runtime.schedule.encoded().unwrap(), ledger);
    assert_eq!(runtime.view_revision(), revision);
    assert_eq!(fs::read(&a).unwrap(), a_bytes);
    let reopened = Runtime::new(Document::load(&profile).unwrap(), Some(b)).unwrap();
    assert_eq!(reopened.document(), &draft);
    assert!(!reopened.group_on(Target::Pc));
    let invalid = stand.root.join("invalid-draft-journal.json");
    draft.lighting.setup.canvas.points.clear();
    assert!(runtime.export_journal_for(&draft, invalid.clone()).is_err());
    assert!(!invalid.exists());
    assert_eq!(runtime.document(), &original);
}

#[path = "stream_input_tests.rs"]
mod stream_input_tests;

#[path = "ambient_feed_tests.rs"]
mod ambient_feed_tests;
#[path = "output_replacement_tests.rs"]
mod output_replacement_tests;
