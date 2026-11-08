//! Actual timed software output, with no UI ticks and no native device factories.
use pc_control::{
    devices::{Canvas, Device, Driver, Point, Role, Setup},
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
struct Sink(Arc<Mutex<Vec<Instant>>>);
impl RgbDriver for Sink {
    fn protocol(&self) -> Driver {
        Driver::DoorBle
    }
    fn leds(&self) -> usize {
        20
    }
    fn write(&mut self, _: &[Rgb], _: &Cancellation) -> Result<WriteResult, String> {
        let mut times = self.0.lock().unwrap();
        if times.len() < 4096 {
            times.push(Instant::now());
        }
        Ok(WriteResult::Submitted)
    }
}
fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 1 {
        return Err("Usage: rgb_cadence_stand NEW_OUTPUT_DIRECTORY".into());
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
                label: "Memory sink, not Bluetooth".into(),
                role: Role::Canvas,
                driver: Driver::DoorBle,
                leds: 20,
                rate_hz: hz,
                enabled: true,
            }],
            canvas: Canvas {
                width: 20,
                height: 1,
                points: (0..20)
                    .map(|led| Point {
                        device: "software-door".into(),
                        led,
                        x: led as u32,
                        y: 0,
                    })
                    .collect(),
            },
        };
        let mut host = Host::new(setup, scene)?;
        host.enable_autonomous()?;
        let trace = Arc::new(Mutex::new(Vec::new()));
        let writes = trace.clone();
        host.attach("software-door", move |_| Ok(Box::new(Sink(writes))))?;
        let start = Instant::now() + Duration::from_millis(200);
        let end = start + Duration::from_secs(2);
        // No Host::tick/Runtime::tick/window message processing here.
        std::thread::sleep(Duration::from_millis(2200));
        let times = trace
            .lock()
            .unwrap()
            .iter()
            .copied()
            .filter(|t| *t >= start && *t < end)
            .collect::<Vec<_>>();
        host.stop();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !host.drained() {
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
            .map(|p| p[1].duration_since(p[0]).as_secs_f64() * 1000.)
            .collect::<Vec<_>>();
        periods.sort_by(f64::total_cmp);
        let percentile = |p: f64| {
            periods
                .get(((periods.len().saturating_sub(1)) as f64 * p).round() as usize)
                .copied()
        };
        cases.push(serde_json::json!({"requested_hz":hz,"window_seconds":2,"submitted_memory_frames":times.len(),"observed_hz":times.len() as f64/2.,"period_ms_median":percentile(0.5),"period_ms_p95":percentile(0.95),"period_ms_max":periods.last(),"private_high_resolution_timer":clock.high_resolution,"clock_running_after_drain":clock.running,"ui_ticks":0}));
    }
    let report = serde_json::json!({"scope":"20-LED software sink; does not exercise native BLE or any other hardware transport","hardware_writes":0,"resource_comparison":false,"cases":cases});
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
