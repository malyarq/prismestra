use super::*;
use crate::{
    app::config::{Groups, Project},
    devices::{Canvas, Driver, Point, Setup},
    lighting::color::Rgb,
    runtime::output::{Cancellation, RgbDriver, WriteResult},
};
use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(1);
fn fixture(count: usize) -> (Document, Local) {
    let devices: Vec<_> = (0..count)
        .map(|i| Device {
            id: format!("door{i}"),
            label: format!("Door{i}"),
            role: Role::Canvas,
            driver: Driver::DoorBle,
            leds: 20,
            rate_hz: 20,
            enabled: true,
        })
        .collect();
    let points = devices
        .iter()
        .flat_map(|d| {
            (0..20).map(move |led| Point {
                device: d.id.clone(),
                led,
                x: led as u32,
                y: 0,
            })
        })
        .collect();
    let local = Local {
        version: 1,
        devices: devices
            .iter()
            .map(|d| {
                (
                    d.id.clone(),
                    Binding::Ble {
                        address: 0x1100000000 + NEXT.fetch_add(1, Ordering::Relaxed),
                    },
                )
            })
            .collect(),
        monitors: Default::default(),
        door_brightness: 15,
    };
    let document = Document::from_lighting(Project {
        version: 1,
        setup: Setup {
            version: 1,
            devices: devices.clone(),
            canvas: Canvas {
                width: 20,
                height: 1,
                points,
            },
        },
        scene: crate::runtime::scene::example(),
        schedule: Default::default(),
        groups: Groups {
            pc: vec![],
            door: devices.iter().map(|d| d.id.clone()).collect(),
        },
    })
    .unwrap();
    (document, local)
}
struct Sink(Arc<Mutex<Vec<Vec<Rgb>>>>);
impl RgbDriver for Sink {
    fn protocol(&self) -> Driver {
        Driver::DoorBle
    }
    fn leds(&self) -> usize {
        20
    }
    fn write(&mut self, colors: &[Rgb], _: &Cancellation) -> Result<WriteResult, String> {
        self.0.lock().unwrap().push(colors.to_vec());
        Ok(WriteResult::Submitted)
    }
}
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("work")
}
#[test]
fn lighting_binding_plan_reaches_runtime_scene_worker_and_drains_without_native_io() {
    let (document, local) = fixture(1);
    let writes = Arc::new(Mutex::new(Vec::new()));
    let opened = Arc::new(AtomicUsize::new(0));
    let mut runtime = Runtime::new(document, None).unwrap();
    let factory: RgbFactory = {
        let opened = opened.clone();
        let writes = writes.clone();
        Arc::new(move |_| {
            opened.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Sink(writes.clone())))
        })
    };
    attach_with(
        &local,
        &mut runtime,
        &root(),
        |_, _, _| Ok(factory.clone()),
        |_, _| panic!("Unexpected LCD"),
        |_, _| panic!("Unexpected capture"),
    )
    .unwrap();
    runtime
        .set_group(crate::schedule::Target::Door, true)
        .unwrap();
    let end = Instant::now() + Duration::from_secs(2);
    while writes.lock().unwrap().is_empty() {
        runtime
            .tick(crate::clock::monotonic_us(), 1794100000)
            .unwrap();
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(opened.load(Ordering::SeqCst), 1);
    assert_eq!(writes.lock().unwrap()[0].len(), 20);
    runtime.stop();
    while !runtime.drained() {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
    // Deferred native factory construction itself must never contact BLE.
    assert!(
        crate::runtime::native::bound_factory(
            &runtime.document().lighting.setup,
            "door0",
            &local.devices,
            15
        )
        .is_ok()
    );
}
#[test]
fn complete_preflight_rejects_missing_binding_or_late_factory_error_before_any_open() {
    let (document, mut local) = fixture(2);
    let opened = Arc::new(AtomicUsize::new(0));
    let factory: RgbFactory = {
        let opened = opened.clone();
        Arc::new(move |_| {
            opened.fetch_add(1, Ordering::SeqCst);
            Err("Not opened".into())
        })
    };
    let mut runtime = Runtime::new(document, None).unwrap();
    let mut n = 0;
    assert!(
        attach_with(
            &local,
            &mut runtime,
            &root(),
            |_, _, _| {
                n += 1;
                if n == 2 {
                    Err("Invalid second factory".into())
                } else {
                    Ok(factory.clone())
                }
            },
            |_, _| panic!(),
            |_, _| panic!()
        )
        .is_err()
    );
    assert!(runtime.host().status("door0").is_none());
    local.devices = BTreeMap::new();
    assert!(
        attach_with(
            &local,
            &mut runtime,
            &root(),
            |_, _, _| panic!("Validation must precede resolver"),
            |_, _| panic!(),
            |_, _| panic!()
        )
        .is_err()
    );
    assert_eq!(opened.load(Ordering::SeqCst), 0);
}
#[test]
fn local_lighting_file_rejects_duplicate_fields_and_is_not_part_of_portable_document() {
    let (document, local) = fixture(1);
    let path = root().join(format!(
        "lighting-local-{}-{}.json",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let bytes = serde_json::to_vec(&local).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert!(Local::read(&path, &document).is_ok());
    let duplicate = String::from_utf8(bytes).unwrap().replacen(
        "\"version\":1",
        "\"version\":1,\"version\":1",
        1,
    );
    std::fs::write(&path, duplicate).unwrap();
    assert!(Local::read(&path, &document).is_err());
    std::fs::remove_file(path).unwrap();
    let portable = serde_json::to_string(&document).unwrap();
    assert!(!portable.contains("door_brightness"));
    assert!(!portable.contains("address"));
}
