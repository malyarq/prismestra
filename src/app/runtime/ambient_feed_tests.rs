//! Actual capture→bounded slot→autonomous Host→memory RGB output; no UI ticks/native I/O.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicU8};
#[derive(Default)]
struct Trace(Mutex<Vec<Rgb>>);
struct Sink(Arc<Trace>);
impl RgbDriver for Sink {
    fn protocol(&self) -> Driver {
        Driver::SkydimoSerial {
            model: "SK0410".into(),
        }
    }
    fn leds(&self) -> usize {
        150
    }
    fn write(&mut self, c: &[Rgb], _: &Cancellation) -> Result<WriteResult, String> {
        let mut v = self.0.0.lock().unwrap();
        if v.len() < 1024 {
            v.push(c[0]);
        }
        Ok(WriteResult::Submitted)
    }
}
fn red(c: &Rgb) -> bool {
    c.r > 0 && c.g == 0 && c.b == 0
}
fn green(c: &Rgb) -> bool {
    c.g > 0 && c.r == 0 && c.b == 0
}
fn blue(c: &Rgb) -> bool {
    c.b > 0 && c.r == 0 && c.g == 0
}
fn image(color: Rgb, crop: Option<Rect>, at: u64) -> Frame {
    Frame {
        image: Arc::new(Image::new(1, 1, vec![color]).unwrap()),
        observed_ms: at,
        new_pixels: true,
        backend: Backend::Dxgi,
        manual_crop: crop,
    }
}
fn runtime(stand: &mut Stand, ttl: u64) -> (Guard, Arc<Trace>) {
    stand.document.ambient.sources[0].rate_hz = 49;
    stand.document.ambient.sources[0].stale_after_ms = ttl;
    stand
        .document
        .lighting
        .setup
        .devices
        .iter_mut()
        .find(|d| d.id == "ambient")
        .unwrap()
        .rate_hz = 49;
    let mut runtime = stand.runtime(false);
    runtime.enable_autonomous_rgb().unwrap();
    let trace = Arc::new(Trace::default());
    let sink = trace.clone();
    runtime
        .attach_rgb(
            "ambient",
            vec![],
            Arc::new(move |_| Ok(Box::new(Sink(sink.clone())))),
        )
        .unwrap();
    (runtime, trace)
}
struct Variable(Arc<AtomicU8>, Arc<AtomicUsize>);
impl ambient::Provider for Variable {
    fn poll(&mut self, _: &str, crop: Option<Rect>, at: u64) -> Result<Option<Frame>, String> {
        self.1.fetch_add(1, Ordering::SeqCst);
        let color = if self.0.load(Ordering::SeqCst) == 0 {
            Rgb::new(255, 0, 0)
        } else {
            Rgb::new(0, 255, 0)
        };
        Ok(Some(image(color, crop, at)))
    }
}
#[test]
fn capture_and_color_changes_continue_without_any_runtime_or_window_ticks() {
    let mut stand = Stand::new();
    let (mut runtime, trace) = runtime(&mut stand, 500);
    let value = Arc::new(AtomicU8::new(0));
    let polls = Arc::new(AtomicUsize::new(0));
    let (v, p) = (value.clone(), polls.clone());
    runtime
        .attach_capture(Arc::new(move |_| {
            Ok(Box::new(Variable(v.clone(), p.clone())))
        }))
        .unwrap();
    let generation = runtime.capture_status().unwrap().generation;
    assert_eq!(runtime.host.external_feed_generation(), Some(generation));
    wait(|| trace.0.lock().unwrap().iter().any(red));
    value.store(1, Ordering::SeqCst);
    wait(|| trace.0.lock().unwrap().iter().any(green));
    runtime.set_group(Target::Pc, false).unwrap();
    wait(|| trace.0.lock().unwrap().last() == Some(&Rgb::default()));
    assert!(
        runtime
            .capture_status()
            .unwrap()
            .frames
            .values()
            .all(|frame| frame.iter().all(|c| *c == Rgb::default()))
    );
    std::thread::sleep(Duration::from_millis(30));
    let paused = polls.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(polls.load(Ordering::SeqCst), paused);
    runtime.set_group(Target::Pc, true).unwrap();
    wait(|| trace.0.lock().unwrap().last().is_some_and(green));
    runtime.stop();
    wait(|| runtime.drained());
    let end = trace.0.lock().unwrap().len();
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(trace.0.lock().unwrap().len(), end);
}
struct Blocked {
    calls: usize,
    gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
    entered: Arc<AtomicBool>,
    green: Arc<AtomicBool>,
    polls: Arc<AtomicUsize>,
}
impl ambient::Provider for Blocked {
    fn poll(&mut self, _: &str, crop: Option<Rect>, at: u64) -> Result<Option<Frame>, String> {
        self.calls += 1;
        self.polls.fetch_add(1, Ordering::SeqCst);
        if self.calls == 1 {
            return Ok(Some(image(Rgb::new(255, 0, 0), crop, at)));
        }
        if self.calls == 2 {
            self.entered.store(true, Ordering::SeqCst);
            let (lock, wake) = &*self.gate;
            let (open, timed) = wake
                .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(2), |open| !*open)
                .unwrap();
            if !*open && timed.timed_out() {
                return Err("Own capture gate timed out".into());
            }
            return Ok(Some(image(Rgb::new(0, 0, 255), crop, at)));
        }
        if self.green.load(Ordering::SeqCst) {
            Ok(Some(image(Rgb::new(0, 255, 0), crop, at)))
        } else {
            Ok(None)
        }
    }
}
fn attach_blocked(
    runtime: &mut Runtime,
    gate: &ConfigurationGate,
    entered: Arc<AtomicBool>,
    allow: Arc<AtomicBool>,
    polls: Arc<AtomicUsize>,
) {
    let gate = gate.0.clone();
    runtime
        .attach_capture(Arc::new(move |_| {
            Ok(Box::new(Blocked {
                calls: 0,
                gate: gate.clone(),
                entered: entered.clone(),
                green: allow.clone(),
                polls: polls.clone(),
            }))
        }))
        .unwrap();
}
#[test]
fn blocked_capture_expires_to_black_without_ui_and_late_expired_pixels_never_reach_rgb() {
    let mut stand = Stand::new();
    let (mut runtime, trace) = runtime(&mut stand, 150);
    let gate = ConfigurationGate::new();
    let entered = Arc::new(AtomicBool::new(false));
    let allow = Arc::new(AtomicBool::new(false));
    attach_blocked(
        &mut runtime,
        &gate,
        entered.clone(),
        allow.clone(),
        Arc::new(AtomicUsize::new(0)),
    );
    wait(|| trace.0.lock().unwrap().iter().any(red));
    wait(|| entered.load(Ordering::SeqCst));
    std::thread::sleep(Duration::from_millis(210));
    wait(|| trace.0.lock().unwrap().last() == Some(&Rgb::default()));
    assert_eq!(
        runtime.capture_status().unwrap().phase,
        capture_worker::Phase::Ready
    );
    allow.store(true, Ordering::SeqCst);
    gate.open();
    wait(|| trace.0.lock().unwrap().iter().any(green));
    assert!(!trace.0.lock().unwrap().iter().any(blue));
    runtime.stop();
    wait(|| runtime.drained());
}
#[test]
fn coalesced_disable_enable_discards_blocked_cache_even_while_that_cache_is_fresh() {
    let mut stand = Stand::new();
    let (mut runtime, trace) = runtime(&mut stand, 1000);
    let gate = ConfigurationGate::new();
    let entered = Arc::new(AtomicBool::new(false));
    let allow = Arc::new(AtomicBool::new(false));
    let polls = Arc::new(AtomicUsize::new(0));
    attach_blocked(
        &mut runtime,
        &gate,
        entered.clone(),
        allow.clone(),
        polls.clone(),
    );
    wait(|| trace.0.lock().unwrap().iter().any(red));
    wait(|| entered.load(Ordering::SeqCst));
    runtime.set_group(Target::Pc, false).unwrap();
    runtime.set_group(Target::Pc, true).unwrap();
    gate.open();
    wait(|| polls.load(Ordering::SeqCst) >= 4);
    assert!(!trace.0.lock().unwrap().iter().any(blue));
    assert_eq!(trace.0.lock().unwrap().last(), Some(&Rgb::default()));
    allow.store(true, Ordering::SeqCst);
    wait(|| trace.0.lock().unwrap().iter().any(green));
    runtime.stop();
    wait(|| runtime.drained());
}
