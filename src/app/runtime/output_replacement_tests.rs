use super::*;
fn body_plan(factory: RgbFactory, key: &str) -> OutputPlan {
    OutputPlan {
        rgb: vec![("body".into(), vec![key.into()], factory)],
        ..Default::default()
    }
}
#[test]
fn output_replacement_validates_before_drain_cancels_and_explicitly_reverts_failed_open() {
    let stand = Stand::new();
    let mut runtime = stand.runtime(true);
    runtime.set_group(Target::Door, false).unwrap();
    let original = runtime.document().clone();
    let gate = ConfigurationGate::new();
    let held = gate.0.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let base = rgb_factory(
        &original,
        "body",
        Arc::new(Record::default()),
        calls.clone(),
    );
    let (entered, waiting) = std::sync::mpsc::channel();
    runtime
        .attach_rgb(
            "body",
            vec![],
            Arc::new(move |cancel| {
                entered.send(()).unwrap();
                let (lock, wake) = &*held;
                let mut open = lock.lock().unwrap();
                while !*open {
                    open = wake.wait(open).unwrap();
                }
                base(cancel)
            }),
        )
        .unwrap();
    waiting.recv_timeout(Duration::from_secs(1)).unwrap();
    let next_calls = Arc::new(AtomicUsize::new(0));
    let next = rgb_factory(
        &original,
        "body",
        Arc::new(Record::default()),
        next_calls.clone(),
    );
    let mut invalid = body_plan(next.clone(), "stand:output-plan:duplicate");
    invalid.rgb.push((
        "door".into(),
        vec!["stand:output-plan:duplicate".into()],
        next.clone(),
    ));
    assert!(runtime.request_outputs(invalid).is_err());
    assert!(!runtime.pending_configuration());
    assert_eq!(
        runtime.host().status("body").unwrap().phase,
        crate::runtime::output::Phase::Connecting
    );
    runtime
        .request_outputs(body_plan(next.clone(), "stand:output-plan:new"))
        .unwrap();
    runtime.tick(now(), wall()).unwrap();
    assert!(runtime.pending_configuration());
    assert_eq!(runtime.output_revision(), 0);
    assert_eq!(next_calls.load(Ordering::SeqCst), 0);
    runtime.cancel_configuration().unwrap();
    gate.open();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        !runtime.pending_configuration() && runtime.outputs_ready() == Ok(true)
    });
    assert_eq!(runtime.output_revision(), 0);
    assert_eq!(next_calls.load(Ordering::SeqCst), 0);
    let previous = runtime.output_plan();
    let revision = runtime
        .request_outputs(body_plan(next, "stand:output-plan:new"))
        .unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        runtime.output_revision() == revision && runtime.outputs_ready() == Ok(true)
    });
    assert_eq!(next_calls.load(Ordering::SeqCst), 1);
    assert!(!runtime.group_on(Target::Door));
    let revision = runtime
        .request_outputs(body_plan(
            Arc::new(|_| Err("Injected replacement open failure".into())),
            "stand:output-plan:failed",
        ))
        .unwrap();
    wait(|| {
        let _ = runtime.tick(now(), wall());
        runtime.output_revision() == revision && runtime.outputs_ready().is_err()
    });
    let revision = runtime.request_outputs(previous).unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        runtime.output_revision() == revision && runtime.outputs_ready() == Ok(true)
    });
    assert_eq!(runtime.document(), &original);
    assert!(!runtime.group_on(Target::Door));
    assert_eq!(next_calls.load(Ordering::SeqCst), 1);
    assert!(calls.load(Ordering::SeqCst) >= 2);
}
#[cfg(windows)]
#[test]
fn output_replacement_persistence_failure_retains_old_factories_until_retry_or_cancel() {
    use std::os::windows::fs::OpenOptionsExt;
    let stand = Stand::new();
    let mut runtime = stand.runtime(true);
    let old_calls = Arc::new(AtomicUsize::new(0));
    runtime
        .attach_rgb(
            "body",
            vec![],
            rgb_factory(
                &stand.document,
                "body",
                Arc::new(Record::default()),
                old_calls.clone(),
            ),
        )
        .unwrap();
    wait(|| runtime.outputs_ready() == Ok(true));
    let before = fs::read(stand.root.join("journal.json")).unwrap();
    let blocker = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(stand.root.join("journal.json"))
        .unwrap();
    let new_calls = Arc::new(AtomicUsize::new(0));
    let factory = rgb_factory(
        &stand.document,
        "body",
        Arc::new(Record::default()),
        new_calls.clone(),
    );
    runtime
        .request_outputs(body_plan(factory, "stand:output-plan:persistence"))
        .unwrap();
    wait(|| {
        let _ = runtime.tick(now(), wall());
        runtime.configuration_error().is_some()
    });
    assert_eq!(runtime.output_revision(), 0);
    assert_eq!(new_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fs::read(stand.root.join("journal.json")).unwrap(), before);
    runtime.cancel_configuration().unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        !runtime.pending_configuration() && runtime.outputs_ready() == Ok(true)
    });
    assert_eq!(runtime.output_revision(), 0);
    assert_eq!(new_calls.load(Ordering::SeqCst), 0);
    assert_eq!(old_calls.load(Ordering::SeqCst), 2);
    drop(blocker);
}

#[test]
fn output_replacement_round_trips_lcd_and_capture_and_stop_never_opens_queued_factories() {
    let stand = Stand::new();
    let mut runtime = stand.runtime(false);
    let calls = Arc::new(AtomicUsize::new(0));
    let mut plan = body_plan(
        rgb_factory(
            &stand.document,
            "body",
            Arc::new(Record::default()),
            calls.clone(),
        ),
        "stand:output-plan:all",
    );
    plan.lcd.push(("lcd".into(), vec![], stand.lcd_factory()));
    plan.capture = Some(Arc::new(|_| Ok(Box::new(Pixels))));
    let revision = runtime.request_outputs(plan).unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        runtime.output_revision() == revision && runtime.outputs_ready() == Ok(true)
    });
    assert!(runtime.capture_status().is_some());
    assert!(runtime.lcd_status("lcd").is_some());
    let original = runtime.output_plan();
    runtime.request_outputs(OutputPlan::default()).unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        !runtime.pending_configuration()
    });
    assert!(runtime.capture_status().is_none());
    assert!(runtime.lcd_status("lcd").is_none());
    let revision = runtime.request_outputs(original).unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        runtime.output_revision() == revision && runtime.outputs_ready() == Ok(true)
    });
    assert!(runtime.capture_status().is_some());
    assert!(runtime.lcd_status("lcd").is_some());
    let queued = Arc::new(AtomicUsize::new(0));
    runtime
        .request_outputs(body_plan(
            rgb_factory(
                &stand.document,
                "body",
                Arc::new(Record::default()),
                queued.clone(),
            ),
            "stand:output-plan:stop",
        ))
        .unwrap();
    runtime.stop();
    wait(|| runtime.drained());
    assert_eq!(queued.load(Ordering::SeqCst), 0);
    assert!(runtime.tick(now(), wall()).is_err());
}

#[test]
fn autonomous_runtime_survives_configuration_and_simulated_resume_without_os_power_calls() {
    let stand = Stand::new();
    let mut runtime = stand.runtime(false);
    runtime.enable_autonomous_rgb().unwrap();
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
    wait(|| {
        runtime
            .host()
            .status("body")
            .is_some_and(|s| s.submitted > 1)
    });
    let before = runtime.host().status("body").unwrap().submitted;
    std::thread::sleep(Duration::from_millis(100));
    assert!(runtime.host().status("body").unwrap().submitted > before);
    let mut next = runtime.document().clone();
    next.lighting.scene.brightness = 31;
    runtime.request_document(next).unwrap();
    wait(|| {
        runtime.tick(now(), wall()).unwrap();
        !runtime.pending_configuration() && runtime.outputs_ready() == Ok(true)
    });
    assert!(runtime.autonomous_rgb());
    assert!(runtime.host().clock_status().unwrap().running);
    assert_eq!(runtime.host().scene().brightness, 31);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let base = now();
    runtime.notify_power(true, base).unwrap();
    runtime.notify_power(false, base + 1).unwrap();
    wait(|| runtime.drained());
    runtime.tick(base + 7_002_000, wall()).unwrap();
    wait(|| {
        runtime.tick(base + 7_003_000, wall()).unwrap();
        !runtime.power.pending()
    });
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(runtime.host().clock_status().unwrap().running);
    assert!(runtime.power.error().is_none());
}
