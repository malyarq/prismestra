use super::*;
use crate::devices::{Canvas, Device, Driver, Point};
use crate::runtime::output::WriteResult;
use std::{sync::atomic::AtomicU64, time::Instant};
static NEXT: AtomicU64 = AtomicU64::new(0);
fn setup() -> Setup {
    Setup {
        version: 1,
        devices: vec![Device {
            id: "door".into(),
            label: "Software door".into(),
            role: Role::Canvas,
            driver: Driver::DoorBle,
            leds: 20,
            rate_hz: 49,
            enabled: true,
        }],
        canvas: Canvas {
            width: 20,
            height: 1,
            points: (0..20)
                .map(|led| Point {
                    device: "door".into(),
                    led,
                    x: led as u32,
                    y: 0,
                })
                .collect(),
        },
    }
}
#[derive(Default)]
struct Frames(Mutex<Vec<Vec<Rgb>>>);
struct Sink(Arc<Frames>);
impl RgbDriver for Sink {
    fn protocol(&self) -> Driver {
        Driver::DoorBle
    }
    fn leds(&self) -> usize {
        20
    }
    fn write(&mut self, colors: &[Rgb], _: &Cancellation) -> Result<WriteResult, String> {
        let mut frames = self.0.0.lock().unwrap();
        if frames.len() < 512 {
            frames.push(colors.to_vec());
        }
        Ok(WriteResult::Submitted)
    }
}
fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(Instant::now() < end, "Autonomous software stand timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn attach(host: &mut Host, frames: Arc<Frames>) {
    host.attach("door", move |_| Ok(Box::new(Sink(frames))))
        .unwrap();
}
#[test]
fn canvas_advances_without_ui_ticks_and_keeps_the_original_pixels_and_mute() {
    let setup = setup();
    let mut scene = scene::example();
    scene.fps = 49;
    let mut reference = Renderer::new(scene.clone()).unwrap();
    let references = (0..200)
        .map(|_| reference.step(&setup.canvas).unwrap())
        .collect::<Vec<_>>();
    let mut host = Host::new(setup, scene).unwrap();
    host.enable_autonomous().unwrap();
    let frames = Arc::new(Frames::default());
    attach(&mut host, frames.clone());
    // Deliberately no Host/Runtime/window tick while actual output workers run.
    std::thread::sleep(Duration::from_millis(250));
    let rendered = frames.0.lock().unwrap().clone();
    assert!(rendered.len() >= 3, "No independent canvas progress");
    let mut index = 0;
    for frame in rendered {
        let offset = references[index..]
            .iter()
            .position(|p| *p == frame)
            .expect("Background changed deterministic scene pixels");
        index += offset;
    }
    wait(|| {
        host.tick(crate::clock::monotonic_us()).unwrap();
        !host.preview().is_empty()
    });
    assert_eq!(host.frame("door").unwrap(), host.preview());
    host.mute("door", true).unwrap();
    wait(|| {
        frames
            .0
            .lock()
            .unwrap()
            .last()
            .is_some_and(|v| v.iter().all(|c| *c == Rgb::default()))
    });
    host.stop();
    wait(|| host.drained());
    let count = frames.0.lock().unwrap().len();
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(frames.0.lock().unwrap().len(), count);
    assert!(!host.clock_status().unwrap().running);
}
#[test]
fn clock_drain_does_not_release_a_blocked_io_owner_and_explicit_attach_restarts_it() {
    let mut host = Host::new(setup(), scene::example()).unwrap();
    host.enable_autonomous().unwrap();
    let key = format!(
        "stand:autonomous:{}:{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let (entered, ready) = std::sync::mpsc::channel();
    let (release, gate) = std::sync::mpsc::channel();
    host.attach_claimed("door", vec![key.clone()], move |_| {
        entered.send(()).unwrap();
        gate.recv_timeout(Duration::from_secs(2))
            .map_err(|e| e.to_string())?;
        Ok(Box::new(Sink(Arc::new(Frames::default()))))
    })
    .unwrap();
    ready.recv_timeout(Duration::from_secs(1)).unwrap();
    host.stop();
    assert!(!host.drained());
    wait(|| {
        host.reap();
        !host.clock_status().unwrap().running
    });
    assert!(
        host.attach("door", move |_| panic!(
            "Rejected replacement opened a new driver"
        ))
        .is_err()
    );
    assert!(output::Lease::acquire(vec![key.clone()]).is_err());
    release.send(()).unwrap();
    wait(|| host.drained());
    assert!(output::Lease::acquire(vec![key]).is_ok());
    host.replace_setup(setup()).unwrap();
    let frames = Arc::new(Frames::default());
    attach(&mut host, frames.clone());
    wait(|| !frames.0.lock().unwrap().is_empty());
    assert!(host.clock_status().unwrap().running);
    host.stop();
    wait(|| host.drained());
}
#[test]
fn autonomous_failure_is_visible_and_outputs_drain_before_repair() {
    let mut host = Host::new(setup(), scene::example()).unwrap();
    host.enable_autonomous().unwrap();
    let frames = Arc::new(Frames::default());
    attach(&mut host, frames.clone());
    wait(|| !frames.0.lock().unwrap().is_empty());
    {
        let mut core = host.core.lock().unwrap();
        let mut invalid = (*core.setup).clone();
        invalid.canvas.width = 0;
        core.setup = Arc::new(invalid);
        core.scene_cadence = Cadence::default();
    }
    host.notify();
    wait(|| host.clock_status().unwrap().error.is_some());
    assert!(host.tick(crate::clock::monotonic_us()).is_err());
    wait(|| host.drained());
    host.replace_setup(setup()).unwrap();
    let next = Arc::new(Frames::default());
    attach(&mut host, next.clone());
    wait(|| !next.0.lock().unwrap().is_empty());
    assert!(host.clock_status().unwrap().error.is_none());
    host.stop();
    wait(|| host.drained());
}
