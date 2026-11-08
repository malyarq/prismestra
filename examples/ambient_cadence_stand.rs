//! Timed software capture→frame feed→autonomous RGB, without UI ticks/native capture or devices.
use pc_control::{
    app::{ambient, capture_worker},
    capture::{
        Backend, Frame,
        ambient::{Mode, OutputSettings, Settings, Zone},
    },
    devices::{Canvas, Device, Driver, Role, Setup},
    lighting::ambient::{Image, Rect},
    lighting::{color::Rgb, scene::Effect},
    runtime::{
        Host,
        output::{Cancellation, RgbDriver, WriteResult},
        scene::{NoisePalette, NoiseSettings, Scene},
    },
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
struct Pixels(Arc<Mutex<Vec<Instant>>>);
impl ambient::Provider for Pixels {
    fn poll(&mut self, _: &str, crop: Option<Rect>, at: u64) -> Result<Option<Frame>, String> {
        let counter = {
            let mut polls = self.0.lock().unwrap();
            let counter = polls.len();
            if counter < 4096 {
                polls.push(Instant::now());
            }
            counter
        };
        let color = if counter % 2 == 0 {
            Rgb::new(255, 0, 0)
        } else {
            Rgb::new(0, 255, 0)
        };
        Ok(Some(Frame {
            image: Arc::new(Image::new(1, 1, vec![color])?),
            observed_ms: at,
            new_pixels: true,
            backend: Backend::Dxgi,
            manual_crop: crop,
        }))
    }
}
struct Sink(Arc<Mutex<Vec<(Instant, Rgb)>>>);
impl RgbDriver for Sink {
    fn protocol(&self) -> Driver {
        Driver::SkydimoSerial {
            model: "SK0410".into(),
        }
    }
    fn leds(&self) -> usize {
        150
    }
    fn write(&mut self, colors: &[Rgb], _: &Cancellation) -> Result<WriteResult, String> {
        let mut times = self.0.lock().unwrap();
        if times.len() < 4096 {
            times.push((Instant::now(), colors[0]));
        }
        Ok(WriteResult::Submitted)
    }
}
fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 1 {
        return Err("Usage: ambient_cadence_stand NEW_OUTPUT_DIRECTORY".into());
    }
    let out = PathBuf::from(&args[0]);
    std::fs::create_dir(&out).map_err(|e| e.to_string())?;
    let mut cases = vec![];
    for hz in [25, 49, 60, 144] {
        let scene = Scene {
            effect: Effect::NoiseMap,
            colors: vec![0x120100, 0xe64005, 0xff7814],
            brightness: 60,
            speed: 8,
            fps: hz,
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
        };
        let setup = Setup {
            version: 1,
            devices: vec![Device {
                id: "software-door".into(),
                label: "Memory sink, not serial".into(),
                role: Role::Ambient,
                driver: Driver::SkydimoSerial {
                    model: "SK0410".into(),
                },
                leds: 150,
                rate_hz: hz,
                enabled: true,
            }],
            canvas: Canvas {
                width: 1,
                height: 1,
                points: vec![],
            },
        };
        let config = ambient::Config {
            sources: vec![ambient::Source {
                key: "software".into(),
                rate_hz: hz,
                crop: None,
                stale_after_ms: 500,
            }],
            routes: vec![ambient::Route {
                device: "software-door".into(),
                source: "software".into(),
                enabled: true,
                zone: Zone::linear(150, false, Mode::ScreenCopy)?,
                processing: Settings::default(),
                output: OutputSettings {
                    smoothness: 0,
                    ..Default::default()
                },
            }],
        };
        let polls = Arc::new(Mutex::new(Vec::new()));
        let sampled = polls.clone();
        let mut capture =
            capture_worker::Worker::start(u64::from(hz), config, setup.clone(), move |_| {
                Ok(Pixels(sampled))
            })?;
        let mut host = Host::new(setup, scene)?;
        host.enable_autonomous()?;
        let trace = Arc::new(Mutex::new(Vec::new()));
        let writes = trace.clone();
        host.attach("software-door", move |_| Ok(Box::new(Sink(writes))))?;
        host.bind_external_feed(capture.frame_reader())?;
        capture.submit(capture_worker::Intent {
            revision: 1,
            active: true,
            suspended: false,
        })?;
        std::thread::sleep(Duration::from_millis(200));
        let start = Instant::now();
        let end = start + Duration::from_secs(2);
        // No Host::tick/Runtime::tick/window message processing here.
        std::thread::sleep(Duration::from_secs(2));
        let measured_polls = polls
            .lock()
            .unwrap()
            .iter()
            .filter(|at| **at >= start && **at < end)
            .count();
        let times = trace
            .lock()
            .unwrap()
            .iter()
            .copied()
            .filter(|t| t.0 >= start && t.0 < end)
            .collect::<Vec<_>>();
        host.stop();
        capture.stop();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !host.drained() || !capture.reap() {
            if Instant::now() >= deadline {
                return Err("Own software outputs did not drain".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let clock = host.clock_status().ok_or("Missing autonomous clock")?;
        if let Some(error) = clock.error {
            return Err(error);
        }
        let mut periods = times
            .windows(2)
            .map(|p| p[1].0.duration_since(p[0].0).as_secs_f64() * 1000.)
            .collect::<Vec<_>>();
        periods.sort_by(f64::total_cmp);
        let percentile = |p: f64| {
            periods
                .get(((periods.len().saturating_sub(1)) as f64 * p).round() as usize)
                .copied()
        };
        let changed = times.windows(2).filter(|p| p[0].1 != p[1].1).count();
        cases.push(serde_json::json!({"requested_hz":hz,"capture_polls":measured_polls,"capture_polls_per_second":measured_polls as f64/2.,"changed_rgb_frames":changed,"capture_high_resolution_timer":capture.snapshot().high_resolution_timer,"window_seconds":2,"submitted_memory_frames":times.len(),"observed_hz":times.len() as f64/2.,"period_ms_median":percentile(0.5),"period_ms_p95":percentile(0.95),"period_ms_max":periods.last(),"private_high_resolution_timer":clock.high_resolution,"clock_running_after_drain":clock.running,"ui_ticks":0}));
    }
    let report = serde_json::json!({"scope":"150-LED software sink and synthetic image provider; no native desktop capture, serial or other hardware I/O","hardware_writes":0,"resource_comparison":false,"cases":cases});
    let bytes = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(out.join("report.json"), &bytes).map_err(|e| e.to_string())?;
    println!("{}", String::from_utf8_lossy(&bytes));
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
