//! Actual Runtime coordinator/worker/ledger tests; no windows, physical providers or OS power calls.
use super::*;
use crate::app::runtime::coordinator::Coordinator;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32};

fn runtime(stand: &Stand) -> Runtime {
    Runtime::new(
        stand.document.clone(),
        Some(stand.root.join("coordinator.json")),
    )
    .unwrap()
}
fn finish(owner: &mut Coordinator) {
    owner.stop();
    wait(|| owner.drained());
    assert!(!owner.status().running);
}
#[test]
fn coordinator_views_are_immutable_reuse_documents_and_gate_recovery_on_stop() {
    let stand = Stand::new();
    let mut owner = Coordinator::start(runtime(&stand)).unwrap();
    let original = owner.view();
    owner
        .command(|r, _| r.set_group(Target::Pc, false))
        .unwrap();
    let changed = owner.view();
    assert!(original.desired.pc);
    assert!(!changed.desired.pc);
    assert!(Arc::ptr_eq(&original.document, &changed.document));
    assert!(
        owner
            .recovery::<()>(|_| panic!("Live recovery must not run"))
            .is_err()
    );
    owner
        .command(|r, _| {
            let mut scene = r.project().scene.clone();
            scene.brightness = 7;
            r.set_scene(scene)
        })
        .unwrap();
    let next = owner.view();
    assert_eq!(next.document.lighting.scene.brightness, 7);
    assert_ne!(original.document.lighting.scene.brightness, 7);
    assert!(!Arc::ptr_eq(&original.document, &next.document));
    finish(&mut owner);
    assert!(!owner.view().temperature_ready);
    assert!(owner.recovery(|r| r.set_group(Target::Pc, true)).is_err());
}
struct Temperature(Arc<AtomicU32>);
impl sensor_worker::Provider for Temperature {
    fn poll(&mut self, now: u64) -> Result<Option<crate::sensors::Reading>, String> {
        let value = self.0.load(Ordering::SeqCst);
        Ok((value != u32::MAX).then_some(crate::sensors::Reading {
            celsius: f64::from(value),
            observed_ms: now,
        }))
    }
    fn poll_interval_ms(&self) -> u64 {
        100
    }
}
#[test]
fn coordinator_advances_schedule_sensor_mean_and_lcd_without_window_ticks() {
    let mut stand = Stand::new();
    stand.document.lighting.schedule.enabled = true;
    stand.document.temperatures.sources = vec![
        sensors::Source {
            key: "ram-a".into(),
            value: sensors::Value::Sensor { input: "a".into() },
        },
        sensors::Source {
            key: "ram-b".into(),
            value: sensors::Value::Sensor { input: "b".into() },
        },
        sensors::Source {
            key: "ram".into(),
            value: sensors::Value::Mean {
                members: vec!["ram-a".into(), "ram-b".into()],
            },
        },
    ];
    let mut engine = runtime(&stand);
    engine
        .attach_lcd("lcd", vec![], stand.lcd_factory())
        .unwrap();
    let first = Arc::new(AtomicU32::new(30));
    for (id, value) in [("a", first.clone()), ("b", Arc::new(AtomicU32::new(50)))] {
        engine
            .attach_sensor(
                id,
                vec![],
                Arc::new(move |_| Ok(Box::new(Temperature(value.clone())))),
            )
            .unwrap();
    }
    let wall = Arc::new(AtomicI64::new(at("2026-11-06T06:59:59Z"))); // Moscow09:59:59
    let clock = wall.clone();
    let mut owner =
        Coordinator::start_with_wall(engine, move || Ok(clock.load(Ordering::SeqCst))).unwrap();
    wait(|| {
        owner.read(|r| {
            r.temperature_samples()
                .iter()
                .any(|s| s.key == "ram" && s.celsius == 40.)
                && !r.group_on(Target::Pc)
        })
    });
    wall.store(at("2026-11-06T07:00:00Z"), Ordering::SeqCst);
    wait(|| owner.read(|r| r.group_on(Target::Pc) && !r.group_on(Target::Door)));
    first.store(70, Ordering::SeqCst);
    wait(|| {
        owner.read(|r| {
            r.temperature_samples()
                .iter()
                .any(|s| s.key == "ram" && s.celsius == 60.)
                && r.lcd_status("lcd")
                    .is_some_and(|s| s.processed_revision == r.revision && s.frames_sent > 0)
        })
    });
    first.store(u32::MAX, Ordering::SeqCst);
    wait(|| {
        owner.read(|r| {
            !r.temperature_samples()
                .iter()
                .any(|s| s.key == "ram" || s.key == "ram-a")
        })
    });
    // Repeated real coordinator ticks do not duplicate a durable schedule attempt.
    let ledger = fs::read(stand.root.join("coordinator.json")).unwrap();
    std::thread::sleep(Duration::from_millis(130));
    assert_eq!(
        fs::read(stand.root.join("coordinator.json")).unwrap(),
        ledger
    );
    assert!(owner.status().ticks > 3);
    assert!(!owner.status().fatal);
    finish(&mut owner);
    let ticks = owner.status().ticks;
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(owner.status().ticks, ticks);
}
#[test]
fn coordinator_preserves_configuration_fault_and_explicit_retry() {
    let stand = Stand::new();
    let engine = runtime(&stand);
    let journal = stand.root.join("coordinator.json");
    let mut owner = Coordinator::start(engine).unwrap();
    wait(|| owner.status().ticks > 0);
    fs::remove_file(&journal).unwrap();
    fs::create_dir(&journal).unwrap();
    let previous = owner.read(|r| r.project().scene.brightness);
    owner
        .command(|r, _| {
            let mut next = r.document().clone();
            next.lighting.scene.brightness = if previous == 42 { 43 } else { 42 };
            r.request_document(next)
        })
        .unwrap();
    wait(|| owner.read(|r| r.configuration_error().is_some()));
    assert_eq!(owner.read(|r| r.project().scene.brightness), previous);
    assert!(owner.status().running);
    assert!(!owner.status().fatal);
    fs::remove_dir(&journal).unwrap();
    owner.command(|r, _| r.retry_configuration()).unwrap();
    wait(|| owner.read(|r| !r.pending_configuration() && r.project().scene.brightness != previous));
    assert!(journal.is_file());
    finish(&mut owner);
}
#[test]
fn coordinator_failure_stops_engine_and_rejects_further_commands() {
    let stand = Stand::new();
    let fail = Arc::new(AtomicBool::new(false));
    let clock = fail.clone();
    let mut owner = Coordinator::start_with_wall(runtime(&stand), move || {
        if clock.load(Ordering::SeqCst) {
            Err("Injected wall clock failure".into())
        } else {
            Ok(wall())
        }
    })
    .unwrap();
    wait(|| owner.status().ticks > 0);
    fail.store(true, Ordering::SeqCst);
    wait(|| !owner.status().running);
    assert!(owner.status().fatal);
    assert!(
        owner
            .status()
            .error
            .unwrap()
            .contains("Injected wall clock")
    );
    assert!(owner.command(|r, _| r.set_group(Target::Pc, true)).is_err());
    wait(|| owner.drained());

    let mut owner = Coordinator::start(runtime(&stand)).unwrap();
    let result: Result<(), String> = owner.command(|_, _| panic!("Injected UI command panic"));
    assert!(result.is_err());
    wait(|| !owner.status().running);
    assert!(owner.status().fatal);
    finish(&mut owner);
}

struct HeldSensor {
    gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
    entered: Arc<AtomicBool>,
    dropped: Arc<AtomicBool>,
    owner: std::rc::Rc<std::thread::ThreadId>,
}
impl sensor_worker::Provider for HeldSensor {
    fn poll(&mut self, at: u64) -> Result<Option<crate::sensors::Reading>, String> {
        assert_eq!(*self.owner, std::thread::current().id());
        self.entered.store(true, Ordering::SeqCst);
        let (lock, wake) = &*self.gate;
        let (open, _) = wake
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(2), |open| !*open)
            .unwrap();
        if !*open {
            return Err("Own sensor gate timed out".into());
        }
        Ok(Some(crate::sensors::Reading {
            celsius: 88.,
            observed_ms: at,
        }))
    }
}
impl Drop for HeldSensor {
    fn drop(&mut self) {
        assert_eq!(*self.owner, std::thread::current().id());
        self.dropped.store(true, Ordering::SeqCst);
    }
}
#[test]
fn coordinator_thread_exit_does_not_release_a_blocked_sensor_or_its_lease() {
    let mut stand = Stand::new();
    stand.document.temperatures.sources = vec![sensors::Source {
        key: "ram".into(),
        value: sensors::Value::Sensor {
            input: "slot".into(),
        },
    }];
    let gate = ConfigurationGate::new();
    let entered = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let mut engine = runtime(&stand);
    let key = format!("stand:coordinator-held:{}", stand.root.display());
    let (source_gate, source_entered, source_dropped) =
        (gate.0.clone(), entered.clone(), dropped.clone());
    engine
        .attach_sensor(
            "slot",
            vec![key.clone()],
            Arc::new(move |_| {
                Ok(Box::new(HeldSensor {
                    gate: source_gate.clone(),
                    entered: source_entered.clone(),
                    dropped: source_dropped.clone(),
                    owner: std::rc::Rc::new(std::thread::current().id()),
                }))
            }),
        )
        .unwrap();
    let mut owner = Coordinator::start(engine).unwrap();
    wait(|| entered.load(Ordering::SeqCst));
    owner.stop();
    wait(|| !owner.status().running);
    assert!(!owner.drained());
    assert!(!dropped.load(Ordering::SeqCst));
    assert!(crate::runtime::output::Lease::acquire(vec![key.clone()]).is_err());
    gate.open();
    wait(|| owner.drained());
    assert!(dropped.load(Ordering::SeqCst));
    assert!(crate::runtime::output::Lease::acquire(vec![key]).is_ok());
    assert!(owner.read(|r| r.temperature_samples().is_empty()));
}
