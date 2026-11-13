use super::*;
use crate::{
    app::{assets::Store as Assets, config::Groups, document::Lcd},
    devices::{Canvas, Device, Driver, Point, Setup},
    runtime::output::{Phase, WriteResult},
};
use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Default)]
struct Link {
    calls: AtomicUsize,
    fail: AtomicBool,
    fail_open: AtomicBool,
    frames: Mutex<Vec<Vec<Rgb>>>,
}
struct RgbLink(Arc<Link>);
impl RgbDriver for RgbLink {
    fn protocol(&self) -> Driver {
        Driver::WledDdp
    }
    fn leds(&self) -> usize {
        1
    }
    fn write(&mut self, colors: &[Rgb], _: &Cancellation) -> Result<WriteResult, String> {
        if self.0.fail.load(Ordering::SeqCst) {
            return Err("Disconnected stand output".into());
        }
        self.0.frames.lock().unwrap().push(colors.to_vec());
        Ok(WriteResult::Submitted)
    }
}
fn factory(link: &Arc<Link>) -> RgbFactory {
    let link = link.clone();
    Arc::new(move |_| {
        link.calls.fetch_add(1, Ordering::SeqCst);
        if link.fail_open.load(Ordering::SeqCst) {
            return Err("Stand open failure".into());
        }
        Ok(Box::new(RgbLink(link.clone())))
    })
}
fn runtime() -> Runtime {
    let devices = ["a", "b"]
        .map(|id| Device {
            id: id.into(),
            label: id.into(),
            driver: Driver::WledDdp,
            role: Role::Canvas,
            leds: 1,
            enabled: true,
            rate_hz: 25,
        })
        .to_vec();
    Runtime::new(
        Document::from_lighting(Project {
            version: 1,
            setup: Setup {
                version: 1,
                devices,
                canvas: Canvas {
                    width: 2,
                    height: 1,
                    points: ["a", "b"]
                        .into_iter()
                        .enumerate()
                        .map(|(x, id)| Point {
                            device: id.into(),
                            led: 0,
                            x: x as u32,
                            y: 0,
                        })
                        .collect(),
                },
            },
            scene: crate::runtime::scene::example(),
            schedule: schedule::Config::default(),
            groups: Groups {
                pc: vec!["a".into(), "b".into()],
                door: vec![],
            },
        })
        .unwrap(),
        None,
    )
    .unwrap()
}
fn wait(mut check: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(4);
    while !check() {
        assert!(Instant::now() < until, "Recovery stand timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn tick(runtime: &mut Runtime, now: &mut u64) {
    *now += 50_000;
    runtime.tick(*now, 1_800_000_000).unwrap();
}
fn working(runtime: &mut Runtime, id: &str, now: &mut u64) {
    wait(|| {
        tick(runtime, now);
        runtime.host.status(id).unwrap().phase == Phase::Ready
            && runtime.host.status(id).unwrap().submitted > 0
    });
    tick(runtime, now);
}
fn disconnected(runtime: &mut Runtime, link: &Link, id: &str, now: &mut u64) {
    link.fail.store(true, Ordering::SeqCst);
    wait(|| {
        tick(runtime, now);
        runtime.host.status(id).unwrap().phase == Phase::Failed
    });
    tick(runtime, now);
}
fn drain(runtime: &mut Runtime) {
    runtime.stop();
    wait(|| runtime.drained());
}

#[test]
fn recovery_initial_failure_and_default_policy_remain_terminal() {
    let mut runtime = runtime();
    let link = Arc::new(Link::default());
    link.fail_open.store(true, Ordering::SeqCst);
    runtime.attach_rgb("a", vec![], factory(&link)).unwrap();
    runtime.enable_output_recovery("a").unwrap();
    assert!(runtime.enable_output_recovery("missing").is_err());
    wait(|| runtime.host.status("a").unwrap().phase == Phase::Failed);
    for now in [1, 2_000_000, 40_000_000] {
        runtime.tick(now, 1_800_000_000).unwrap();
    }
    assert_eq!(link.calls.load(Ordering::SeqCst), 1);
    drain(&mut runtime);

    let mut runtime = self::runtime();
    let link = Arc::new(Link::default());
    runtime.attach_rgb("a", vec![], factory(&link)).unwrap();
    let mut now = 0;
    working(&mut runtime, "a", &mut now);
    disconnected(&mut runtime, &link, "a", &mut now);
    runtime.tick(now + 40_000_000, 1_800_000_000).unwrap();
    assert_eq!(link.calls.load(Ordering::SeqCst), 1);
    drain(&mut runtime);
}

#[test]
fn recovery_reopens_only_failed_output_after_submission_and_restores_current_off() {
    let mut runtime = runtime();
    let a = Arc::new(Link::default());
    let b = Arc::new(Link::default());
    runtime.attach_rgb("a", vec![], factory(&a)).unwrap();
    runtime.attach_rgb("b", vec![], factory(&b)).unwrap();
    runtime.enable_registered_output_recovery();
    let mut now = 0;
    working(&mut runtime, "a", &mut now);
    working(&mut runtime, "b", &mut now);
    let healthy_generation = runtime.host.status("b").unwrap().generation;
    let failed_generation = runtime.host.status("a").unwrap().generation;
    disconnected(&mut runtime, &a, "a", &mut now);
    runtime.set_group(Target::Pc, false).unwrap();
    a.fail.store(false, Ordering::SeqCst);
    tick(&mut runtime, &mut now);
    assert_eq!(a.calls.load(Ordering::SeqCst), 1);
    now += 1_000_000;
    working(&mut runtime, "a", &mut now);
    assert_ne!(
        runtime.host.status("a").unwrap().generation,
        failed_generation
    );
    assert_eq!(a.calls.load(Ordering::SeqCst), 2);
    assert_eq!(b.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        runtime.host.status("b").unwrap().generation,
        healthy_generation
    );
    assert_eq!(
        a.frames.lock().unwrap().last().unwrap(),
        &vec![Rgb::default()]
    );
    drain(&mut runtime);
}

#[test]
fn recovery_has_three_attempts_even_when_each_replacement_submits_successfully() {
    let mut runtime = runtime();
    let link = Arc::new(Link::default());
    runtime.attach_rgb("a", vec![], factory(&link)).unwrap();
    runtime.enable_output_recovery("a").unwrap();
    let mut now = 0;
    working(&mut runtime, "a", &mut now);
    for delay in [1_000_000, 5_000_000, 15_000_000] {
        disconnected(&mut runtime, &link, "a", &mut now);
        let generation = runtime.host.status("a").unwrap().generation;
        link.fail.store(false, Ordering::SeqCst);
        now += delay;
        wait(|| {
            tick(&mut runtime, &mut now);
            let status = runtime.host.status("a").unwrap();
            status.generation != generation && status.submitted > 0
        });
        runtime.enable_output_recovery("a").unwrap(); // Idempotent, not a budget reset.
    }
    disconnected(&mut runtime, &link, "a", &mut now);
    link.fail.store(false, Ordering::SeqCst);
    now += 100_000_000;
    tick(&mut runtime, &mut now);
    assert_eq!(link.calls.load(Ordering::SeqCst), 4);
    assert_eq!(runtime.host.status("a").unwrap().phase, Phase::Failed);
    drain(&mut runtime);
}

#[derive(Default)]
struct Held {
    block: AtomicBool,
    entered: AtomicBool,
    release: AtomicBool,
    dropped: AtomicUsize,
    calls: AtomicUsize,
}
struct HeldDriver(Arc<Held>);
impl Drop for HeldDriver {
    fn drop(&mut self) {
        self.0.dropped.fetch_add(1, Ordering::SeqCst);
    }
}
impl RgbDriver for HeldDriver {
    fn protocol(&self) -> Driver {
        Driver::WledDdp
    }
    fn leds(&self) -> usize {
        1
    }
    fn write(&mut self, _: &[Rgb], cancel: &Cancellation) -> Result<WriteResult, String> {
        if self.0.block.load(Ordering::SeqCst) {
            self.0.entered.store(true, Ordering::SeqCst);
            while !self.0.release.load(Ordering::SeqCst) && !cancel.cancelled() {
                std::thread::sleep(Duration::from_millis(1));
            }
            return Err("Held stand I/O released".into());
        }
        Ok(WriteResult::Submitted)
    }
}

#[test]
fn recovery_waits_for_blocked_io_and_lease_release_before_reopening() {
    let mut runtime = runtime();
    let held = Arc::new(Held::default());
    let owned = held.clone();
    let key = format!("recovery-held:{}", std::process::id());
    runtime
        .attach_rgb(
            "a",
            vec![key.clone()],
            Arc::new(move |_| {
                let call = owned.calls.fetch_add(1, Ordering::SeqCst);
                if call > 0 {
                    assert!(owned.dropped.load(Ordering::SeqCst) > 0);
                }
                Ok(Box::new(HeldDriver(owned.clone())))
            }),
        )
        .unwrap();
    runtime.enable_output_recovery("a").unwrap();
    let mut now = 0;
    working(&mut runtime, "a", &mut now);
    held.block.store(true, Ordering::SeqCst);
    wait(|| {
        tick(&mut runtime, &mut now);
        held.entered.load(Ordering::SeqCst)
    });
    now += 40_000_000;
    tick(&mut runtime, &mut now);
    assert_eq!(held.calls.load(Ordering::SeqCst), 1);
    assert!(!runtime.host.output_reaped("a"));
    assert!(
        crate::runtime::output::Output::start_claimed(Driver::WledDdp, 1, 999, vec![key], |_| Err(
            "Should not open while held".into()
        ))
        .is_err()
    );
    held.block.store(false, Ordering::SeqCst);
    held.release.store(true, Ordering::SeqCst);
    wait(|| {
        tick(&mut runtime, &mut now);
        runtime.host.status("a").unwrap().phase == Phase::Failed
    });
    tick(&mut runtime, &mut now);
    now += 1_000_000;
    working(&mut runtime, "a", &mut now);
    assert_eq!(held.calls.load(Ordering::SeqCst), 2);
    drain(&mut runtime);
}

#[test]
fn recovery_pending_work_is_cancelled_by_suspend_and_configuration() {
    let mut runtime = runtime();
    let link = Arc::new(Link::default());
    runtime.attach_rgb("a", vec![], factory(&link)).unwrap();
    runtime.enable_output_recovery("a").unwrap();
    let mut now = 0;
    working(&mut runtime, "a", &mut now);
    disconnected(&mut runtime, &link, "a", &mut now);
    runtime.notify_power(true, now).unwrap();
    now += 40_000_000;
    tick(&mut runtime, &mut now);
    assert_eq!(link.calls.load(Ordering::SeqCst), 1);
    drain(&mut runtime);

    let mut runtime = self::runtime();
    let link = Arc::new(Link::default());
    runtime.attach_rgb("a", vec![], factory(&link)).unwrap();
    runtime.enable_output_recovery("a").unwrap();
    let mut now = 0;
    working(&mut runtime, "a", &mut now);
    disconnected(&mut runtime, &link, "a", &mut now);
    let mut document = runtime.document().clone();
    document.lighting.setup.devices[0].enabled = false;
    runtime.request_document(document).unwrap();
    now += 40_000_000;
    wait(|| {
        tick(&mut runtime, &mut now);
        !runtime.pending_configuration()
    });
    assert_eq!(link.calls.load(Ordering::SeqCst), 1);
    assert!(!runtime.output_recovery.contains_key("a"));
    drain(&mut runtime);
}

struct Reports(Arc<Link>);
impl crate::lcd::protocol::ReportTransport for Reports {
    fn now_ms(&self) -> u64 {
        crate::clock::monotonic_ms()
    }
    fn write_report(&mut self, _: &[u8; 1025], _: u64) -> Result<usize, String> {
        if self.0.fail.load(Ordering::SeqCst) {
            return Err("Disconnected stand LCD".into());
        }
        Ok(1025)
    }
    fn read_ack(&mut self, bytes: &mut [u8; 17], _: u64) -> Result<usize, String> {
        bytes.fill(0);
        Ok(17)
    }
}

#[test]
fn recovery_lcd_reopens_after_real_frame_and_republishes_current_off() {
    let mut runtime = runtime();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("work")
        .join(format!("recovery-lcd-{}", std::process::id()));
    let assets = Assets::new(dir.clone()).unwrap();
    let media = assets
        .put(
            include_bytes!("../../lcd/fixtures/two-frame.apng"),
            32 * 1024 * 1024,
        )
        .unwrap();
    let mut document = runtime.document().clone();
    document.lighting.setup.devices.push(Device {
        id: "lcd".into(),
        label: "LCD".into(),
        role: Role::Lcd,
        driver: Driver::TtLcd,
        leds: 0,
        rate_hz: 25,
        enabled: true,
    });
    document.lighting.groups.pc.push("lcd".into());
    document.lcd.push(Lcd {
        device: "lcd".into(),
        config: lcd::Config {
            enabled: true,
            media: Some(media),
            ..lcd::Config::default()
        },
    });
    runtime = Runtime::new(document, None).unwrap();
    let link = Arc::new(Link::default());
    let opened = link.clone();
    let root = dir.clone();
    runtime
        .attach_lcd(
            "lcd",
            vec![],
            Arc::new(move |config, _| {
                opened.calls.fetch_add(1, Ordering::SeqCst);
                Ok((
                    lcd::Controller::new(config, &Assets::new(root.clone())?)?,
                    Box::new(Reports(opened.clone())),
                ))
            }),
        )
        .unwrap();
    runtime.enable_output_recovery("lcd").unwrap();
    let mut now = 0;
    wait(|| {
        tick(&mut runtime, &mut now);
        runtime.lcd_status("lcd").unwrap().frames_sent > 0
    });
    tick(&mut runtime, &mut now);
    let generation = runtime.lcd_status("lcd").unwrap().generation;
    link.fail.store(true, Ordering::SeqCst);
    wait(|| {
        tick(&mut runtime, &mut now);
        runtime.lcd_status("lcd").unwrap().phase == lcd_worker::Phase::Failed
    });
    tick(&mut runtime, &mut now);
    runtime.set_group(Target::Pc, false).unwrap();
    link.fail.store(false, Ordering::SeqCst);
    now += 1_000_000;
    wait(|| {
        tick(&mut runtime, &mut now);
        let snapshot = runtime.lcd_status("lcd").unwrap();
        snapshot.generation != generation
            && snapshot.frames_sent > 0
            && snapshot.lcd_state == Some(lcd::State::Blanked)
    });
    assert_eq!(link.calls.load(Ordering::SeqCst), 2);
    drain(&mut runtime);
    drop(runtime);
    for file in std::fs::read_dir(&dir).unwrap() {
        std::fs::remove_file(file.unwrap().path()).unwrap();
    }
    std::fs::remove_dir(&dir).unwrap();
}
