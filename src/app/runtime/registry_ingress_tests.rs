//! Actual Registry -> Coordinator -> Runtime -> LCD. Injected read-only providers only.
use super::*;
use crate::app::{
    runtime::coordinator::Coordinator,
    sensor_bindings::{Resolved, Scope, Target},
    sensor_sessions::{Registry, Resolver, Subscription},
};
use std::sync::atomic::{AtomicBool, AtomicI32};
pub(super) struct Value {
    pub(super) value: Arc<AtomicI32>,
    pub(super) closes: Arc<AtomicUsize>,
    pub(super) thread: std::thread::ThreadId,
}
impl sensor_worker::Provider for Value {
    fn poll_interval_ms(&self) -> u64 {
        100
    }
    fn poll(&mut self, at: u64) -> Result<Option<crate::sensors::Reading>, String> {
        assert_eq!(self.thread, std::thread::current().id());
        let value = self.value.load(Ordering::SeqCst);
        Ok((value != i32::MIN).then_some(crate::sensors::Reading {
            celsius: f64::from(value),
            observed_ms: at,
        }))
    }
}
impl Drop for Value {
    fn drop(&mut self) {
        assert_eq!(self.thread, std::thread::current().id());
        self.closes.fetch_add(1, Ordering::SeqCst);
    }
}
struct Resolve(sensor_worker::Factory);
impl Resolver for Resolve {
    fn resolve(&mut self, _: &Target) -> Result<sensor_worker::Factory, String> {
        Ok(self.0.clone())
    }
}
pub(super) fn subscribe(
    document: &Document,
    factory: sensor_worker::Factory,
) -> (Arc<Mutex<Registry>>, Arc<Subscription>, Target) {
    let mut pci: crate::app::sensor_pci::Pci = gpu_binding().into();
    pci.subsystem_id = 0xf0000000 + NEXT.fetch_add(1, Ordering::SeqCst) as u32;
    let target = Target::Nvidia { pci };
    let registry = Arc::new(Mutex::new(Registry::default()));
    let routes = Runtime::temperature_inputs(document)
        .into_iter()
        .map(|input| Resolved {
            input,
            source_id: "registry-ingress-source".into(),
            source_revision: 1,
            target: target.clone(),
        })
        .collect();
    let token = registry
        .lock()
        .unwrap()
        .subscribe(
            Scope::lighting("registry-ingress", document).unwrap(),
            routes,
            &mut Resolve(factory),
        )
        .unwrap();
    (registry, Arc::new(token), target)
}
fn config(stand: &mut Stand) {
    stand.document.temperatures.sources = vec![sensors::Source {
        key: "gpu".into(),
        value: sensors::Value::Sensor {
            input: "slot".into(),
        },
    }];
}
fn finish(owner: &mut Coordinator) {
    owner.stop();
    wait(|| owner.drained());
}
#[cfg(windows)]
#[test]
fn registry_ingress_keeps_real_lcd_overlay_fresh_without_ui_and_preserves_other_subscribers() {
    let mut stand = Stand::new();
    config(&mut stand);
    let assets = Assets::new(stand.root.join("assets")).unwrap();
    let font = fs::read("C:/Windows/Fonts/segoeui.ttf").unwrap();
    let mut png = std::io::Cursor::new(vec![]);
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(16, 16, image::Rgb([80, 10, 0])))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    stand.document.lcd[0].config.media = Some(assets.put(png.get_ref(), 32 * 1024 * 1024).unwrap());
    stand.document.lcd[0].config.font = Some(assets.put(&font, 8 * 1024 * 1024).unwrap());
    stand.document.lcd[0].config.overlay_enabled = true;
    stand.document.lcd[0].config.sources = vec![lcd::OverlaySource {
        key: "gpu".into(),
        label: "GPU".into(),
    }];
    let expected = |temperature: Option<f64>| {
        let mut transport = Reports;
        let samples = temperature
            .map(|celsius| lcd::Sample {
                key: "gpu".into(),
                celsius,
                observed_ms: transport.now_ms(),
            })
            .into_iter()
            .collect::<Vec<_>>();
        let mut reference =
            lcd::Controller::new(stand.document.lcd[0].config.clone(), &assets).unwrap();
        reference.poll(&mut transport, true, &samples).unwrap();
        reference.preview().to_vec()
    };
    let (jpeg42, jpeg67, without_card) = (expected(Some(42.)), expected(Some(67.)), expected(None));
    assert!(
        !jpeg42.is_empty() && jpeg42 != jpeg67 && jpeg67 != without_card,
        "Reference overlay fixtures must differ"
    );
    let value = Arc::new(AtomicI32::new(42));
    let closes = Arc::new(AtomicUsize::new(0));
    let opens = Arc::new(AtomicUsize::new(0));
    let (v, c, o) = (value.clone(), closes.clone(), opens.clone());
    let factory: sensor_worker::Factory = Arc::new(move |_| {
        o.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Value {
            value: v.clone(),
            closes: c.clone(),
            thread: std::thread::current().id(),
        }))
    });
    let (registry, token, target) = subscribe(&stand.document, factory.clone());
    let other = registry
        .lock()
        .unwrap()
        .subscribe(
            Scope::Lighting {
                identity: "other-reader-scope".into(),
                digest: "a".repeat(64),
            },
            vec![Resolved {
                input: "other".into(),
                source_id: "registry-ingress-source".into(),
                source_revision: 1,
                target,
            }],
            &mut Resolve(factory),
        )
        .unwrap();
    let mut engine = Runtime::new(stand.document.clone(), None).unwrap();
    engine
        .attach_lcd("lcd", vec![], stand.lcd_factory())
        .unwrap();
    engine
        .bind_registry_input(
            registry.clone(),
            token.clone(),
            crate::clock::monotonic_us(),
        )
        .unwrap();
    let mut owner = Coordinator::start(engine).unwrap();
    let selected = |r: &Runtime, t: f64, jpeg: &[u8]| {
        r.temperature_samples()
            .iter()
            .any(|s| s.key == "gpu" && s.celsius == t)
            && r.lcd_status("lcd")
                .is_some_and(|s| s.preview.as_ref() == jpeg && s.frames_sent > 0)
    };
    wait(|| owner.read(|r| selected(r, 42., &jpeg42)));
    let first = owner.read(|r| r.temperature_samples()[0].observed_ms);
    std::thread::sleep(Duration::from_millis(5200)); // Exceeds the numeric projection's five-second TTL.
    wait(|| {
        owner.read(|r| selected(r, 42., &jpeg42) && r.temperature_samples()[0].observed_ms > first)
    });
    value.store(67, Ordering::SeqCst);
    wait(|| owner.read(|r| selected(r, 67., &jpeg67)));
    value.store(i32::MIN, Ordering::SeqCst);
    wait(|| {
        owner.read(|r| {
            r.temperature_samples().is_empty()
                && r.lcd_status("lcd")
                    .is_some_and(|s| s.preview.as_ref() == without_card)
        })
    });
    value.store(42, Ordering::SeqCst);
    wait(|| owner.read(|r| selected(r, 42., &jpeg42)));
    token.revoke(); // No Editor poll or Runtime/window tick from this test.
    wait(|| {
        owner.read(|r| r.registry_subscription().is_none() && r.temperature_samples().is_empty())
    });
    finish(&mut owner);
    assert!(owner.read(|r| r.error().is_none()));
    assert!(other.active());
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(closes.load(Ordering::SeqCst), 0);
    other.revoke();
    wait(|| registry.lock().unwrap().drained());
    assert_eq!(closes.load(Ordering::SeqCst), 1);
}
struct Held {
    gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
    entered: Arc<AtomicBool>,
    closes: Arc<AtomicUsize>,
    marker: std::rc::Rc<std::thread::ThreadId>,
    warm: Option<f64>,
}
impl sensor_worker::Provider for Held {
    fn poll(&mut self, at: u64) -> Result<Option<crate::sensors::Reading>, String> {
        assert_eq!(*self.marker, std::thread::current().id());
        if let Some(celsius) = self.warm.take() {
            return Ok(Some(crate::sensors::Reading {
                celsius,
                observed_ms: at,
            }));
        }
        self.entered.store(true, Ordering::SeqCst);
        let (lock, wake) = &*self.gate;
        let (open, _) = wake
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(3), |v| !*v)
            .unwrap();
        if !*open {
            return Err("Own registry source gate timed out".into());
        }
        Ok(Some(crate::sensors::Reading {
            celsius: 99.,
            observed_ms: at,
        }))
    }
}
impl Drop for Held {
    fn drop(&mut self) {
        assert_eq!(*self.marker, std::thread::current().id());
        self.closes.fetch_add(1, Ordering::SeqCst);
    }
}
#[test]
fn registry_ingress_retirement_keeps_last_blocked_reader_lease_after_ui_already_polled_release() {
    let mut stand = Stand::new();
    config(&mut stand);
    let gate = ConfigurationGate::new();
    let entered = Arc::new(AtomicBool::new(false));
    let closes = Arc::new(AtomicUsize::new(0));
    let (g, e, c) = (gate.0.clone(), entered.clone(), closes.clone());
    let (registry, token, target) = subscribe(
        &stand.document,
        Arc::new(move |_| {
            Ok(Box::new(Held {
                gate: g.clone(),
                entered: e.clone(),
                closes: c.clone(),
                marker: std::rc::Rc::new(std::thread::current().id()),
                warm: None,
            }))
        }),
    );
    let mut engine = Runtime::new(stand.document.clone(), None).unwrap();
    engine
        .bind_registry_input(
            registry.clone(),
            token.clone(),
            crate::clock::monotonic_us(),
        )
        .unwrap();
    let mut owner = Coordinator::start(engine).unwrap();
    wait(|| entered.load(Ordering::SeqCst));
    token.revoke();
    registry.lock().unwrap().poll();
    owner.stop();
    wait(|| !owner.status().running);
    assert!(!owner.drained());
    assert_eq!(closes.load(Ordering::SeqCst), 0);
    assert!(crate::runtime::output::Lease::acquire(target.resource_keys().unwrap()).is_err());
    gate.open();
    wait(|| owner.drained());
    assert_eq!(closes.load(Ordering::SeqCst), 1);
    assert!(crate::runtime::output::Lease::acquire(target.resource_keys().unwrap()).is_ok());
    assert!(owner.read(|r| r.temperature_samples().is_empty()));
}
#[test]
fn registry_ingress_rejects_foreign_ownership_and_retires_a_changed_temperature_scope() {
    let mut stand = Stand::new();
    config(&mut stand);
    let closes = Arc::new(AtomicUsize::new(0));
    let c = closes.clone();
    let (registry, token, _) = subscribe(
        &stand.document,
        Arc::new(move |_| {
            Ok(Box::new(Value {
                value: Arc::new(AtomicI32::new(42)),
                closes: c.clone(),
                thread: std::thread::current().id(),
            }))
        }),
    );
    let mut engine = Runtime::new(stand.document.clone(), None).unwrap();
    assert!(
        engine
            .bind_registry_input(
                Arc::new(Mutex::new(Registry::default())),
                token.clone(),
                crate::clock::monotonic_us()
            )
            .is_err()
    );
    engine
        .bind_registry_input(
            registry.clone(),
            token.clone(),
            crate::clock::monotonic_us(),
        )
        .unwrap();
    assert!(
        engine
            .accept_temperatures(&[], crate::clock::monotonic_us())
            .is_err()
    );
    assert!(
        engine
            .attach_sensor(
                "slot",
                vec![],
                Arc::new(|_| panic!("Must not create a second reader"))
            )
            .is_err()
    );
    let mut owner = Coordinator::start(engine).unwrap();
    wait(|| owner.read(|r| !r.temperature_samples().is_empty()));
    owner
        .command(|r, _| {
            let mut next = r.document().clone();
            next.temperatures.sources[0].value = sensors::Value::Sensor {
                input: "renamed-slot".into(),
            };
            r.request_document(next)
        })
        .unwrap();
    wait(|| owner.read(|r| !r.pending_configuration() && r.registry_subscription().is_none()));
    assert!(!token.active());
    assert!(owner.read(|r| r.temperature_samples().is_empty()));
    finish(&mut owner);
    assert_eq!(closes.load(Ordering::SeqCst), 1);
}

#[test]
fn registry_ingress_replacement_with_equal_generation_cannot_adopt_a_late_old_reader() {
    let mut stand = Stand::new();
    config(&mut stand);
    let gate = ConfigurationGate::new();
    let entered = Arc::new(AtomicBool::new(false));
    let closed = Arc::new(AtomicUsize::new(0));
    let (g, e, c) = (gate.0.clone(), entered.clone(), closed.clone());
    let (old, old_token, old_target) = subscribe(
        &stand.document,
        Arc::new(move |_| {
            Ok(Box::new(Held {
                gate: g.clone(),
                entered: e.clone(),
                closes: c.clone(),
                marker: std::rc::Rc::new(std::thread::current().id()),
                warm: Some(42.),
            }))
        }),
    );
    let mut engine = Runtime::new(stand.document.clone(), None).unwrap();
    engine
        .bind_registry_input(old.clone(), old_token.clone(), crate::clock::monotonic_us())
        .unwrap();
    let mut owner = Coordinator::start(engine).unwrap();
    wait(|| owner.read(|r| r.temperature_samples().iter().any(|s| s.celsius == 42.)));
    wait(|| entered.load(Ordering::SeqCst));
    let old_lineage = owner.read(|r| r.temperature_observation_batch().1["slot"].0);
    let new_closed = Arc::new(AtomicUsize::new(0));
    let c = new_closed.clone();
    let (new, new_token, _) = subscribe(
        &stand.document,
        Arc::new(move |_| {
            Ok(Box::new(Value {
                value: Arc::new(AtomicI32::new(67)),
                closes: c.clone(),
                thread: std::thread::current().id(),
            }))
        }),
    );
    assert_eq!(
        old.lock().unwrap().status()[0].worker.generation,
        new.lock().unwrap().status()[0].worker.generation
    );
    owner
        .command(|r, now| r.bind_registry_input(new.clone(), new_token.clone(), now))
        .unwrap();
    assert_ne!(
        owner.read(|r| r.temperature_observation_batch().1["slot"].0),
        old_lineage
    );
    wait(|| owner.read(|r| r.temperature_samples().iter().any(|s| s.celsius == 67.)));
    assert!(!old_token.active());
    assert!(crate::runtime::output::Lease::acquire(old_target.resource_keys().unwrap()).is_err());
    gate.open();
    wait(|| closed.load(Ordering::SeqCst) == 1);
    std::thread::sleep(Duration::from_millis(80));
    assert!(
        owner.read(|r| r.temperature_samples().iter().any(|s| s.celsius == 67.)
            && !r.temperature_samples().iter().any(|s| s.celsius == 99.))
    );
    finish(&mut owner);
    assert_eq!(new_closed.load(Ordering::SeqCst), 1);
}
