use super::*;
use crate::{
    app::config::{Groups, Project},
    devices::{Canvas, Point, Role, Setup},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(1);
fn document() -> Document {
    Document::from_lighting(Project {
        version: 1,
        setup: Setup {
            version: 1,
            devices: vec![device("door")],
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
        },
        scene: crate::runtime::scene::example(),
        schedule: Default::default(),
        groups: Groups {
            pc: vec![],
            door: vec!["door".into()],
        },
    })
    .unwrap()
}
fn device(id: &str) -> Device {
    Device {
        id: id.into(),
        label: id.into(),
        role: Role::Canvas,
        driver: Driver::DoorBle,
        leds: 20,
        rate_hz: 20,
        enabled: true,
    }
}
fn path() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("work")
        .join(format!(
            "lighting-binding-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir(&p).unwrap();
    p.join("local.json")
}
#[test]
fn workspace_saves_partial_and_complete_bindings_without_touching_the_portable_document() {
    let doc = document();
    let before = doc.clone();
    let path = path();
    let mut w = Workspace::open(&path, &doc).unwrap();
    w.save(&doc).unwrap();
    assert!(Local::read(&path, &doc).is_err());
    w.assign(
        &doc,
        &doc.lighting.setup.devices[0],
        &Draft {
            endpoint: "11:22:33:44:55:66".into(),
            ..Default::default()
        },
    )
    .unwrap();
    w.save(&doc).unwrap();
    let full = Local::read(&path, &doc).unwrap();
    assert_eq!(
        full.devices["door"],
        Binding::Ble {
            address: 0x112233445566
        }
    );
    assert!(!w.dirty);
    assert_eq!(doc, before);
}
#[test]
fn drafts_preserve_orphans_and_stale_family_until_explicit_repair_and_prune() {
    let doc = document();
    let path = path();
    let mut w = Workspace::open(&path, &doc).unwrap();
    w.local.devices.insert(
        "door".into(),
        Binding::Hid {
            path: "old-family-path".into(),
        },
    );
    w.local
        .devices
        .insert("removed".into(), Binding::Ble { address: 0 });
    w.save(&doc).unwrap();
    let old = std::fs::read(&path).unwrap();
    let mut w = Workspace::open(&path, &doc).unwrap();
    assert_eq!(w.orphan_ids(&doc), ["removed"]);
    assert!(Local::read(&path, &doc).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), old);
    w.assign(
        &doc,
        &doc.lighting.setup.devices[0],
        &Draft {
            endpoint: "112233445566".into(),
            ..Default::default()
        },
    )
    .unwrap();
    w.prune_orphans(&doc);
    w.save(&doc).unwrap();
    assert!(Local::read(&path, &doc).is_ok());
}
#[test]
fn changed_file_and_wrong_device_keep_user_draft_and_external_bytes() {
    let doc = document();
    let path = path();
    let mut w = Workspace::open(&path, &doc).unwrap();
    w.save(&doc).unwrap();
    w.assign(
        &doc,
        &doc.lighting.setup.devices[0],
        &Draft {
            endpoint: "112233445566".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let mut external = std::fs::read(&path).unwrap();
    external.push(b' ');
    std::fs::write(&path, &external).unwrap();
    assert!(w.save(&doc).is_err());
    assert!(w.dirty);
    assert_eq!(std::fs::read(&path).unwrap(), external);
    let mut stale = device("door");
    stale.label = "changed".into();
    assert!(
        w.assign(
            &doc,
            &stale,
            &Draft {
                endpoint: "112233445566".into(),
                ..Default::default()
            }
        )
        .is_err()
    );
}
#[test]
fn manual_fields_validate_family_and_reject_duplicate_physical_output() {
    let doc = document();
    let mut w = Workspace::open(&path(), &doc).unwrap();
    for endpoint in ["0", "FFFFFFFFFFFF", "11:22:33:44:55:GG", "112233445566\n"] {
        assert!(
            w.assign(
                &doc,
                &device("door"),
                &Draft {
                    endpoint: endpoint.into(),
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    w.local.devices.insert(
        "other".into(),
        Binding::Ble {
            address: 0x112233445566,
        },
    );
    assert!(
        w.assign(
            &doc,
            &device("door"),
            &Draft {
                endpoint: "112233445566".into(),
                ..Default::default()
            }
        )
        .is_err()
    );
    let mut ram = device("ram");
    ram.driver = Driver::EneDram;
    ram.leds = 8;
    let b = Binding::EnePiix4 {
        module_path: r"C:\stand\Piix4.bin".into(),
        address: 0x70,
    };
    assert_eq!(Draft::from_binding(&b).binding(&ram).unwrap(), b);
}
#[test]
fn metadata_scan_is_explicit_validated_and_cancelled_before_reuse() {
    use discovery::*;
    let target = Target::Device(device("door"));
    let mut worker = Worker::default();
    let (ready, waiting) = std::sync::mpsc::channel();
    let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let signal = release.clone();
    let factory: Factory = Arc::new(move |target, _| {
        ready.send(()).unwrap();
        let (lock, cv) = &*signal;
        let mut yes = lock.lock().unwrap();
        while !*yes {
            yes = cv.wait(yes).unwrap();
        }
        Ok(Inventory::empty(target))
    });
    worker.start(target.clone(), factory.clone()).unwrap();
    waiting.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(worker.start(target.clone(), factory).is_err());
    assert!(worker.poll().is_none());
    worker.cancel();
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    let end = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some((_, result)) = worker.poll() {
            assert!(result.is_err());
            break;
        }
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(worker.is_idle());
    worker
        .start(
            target,
            Arc::new(|_, _| Ok(Inventory::empty(Target::Monitor("wrong".into())))),
        )
        .unwrap();
    loop {
        if let Some((_, result)) = worker.poll() {
            assert!(result.is_err());
            break;
        }
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn monitor_replacement_and_removed_source_preserve_the_portable_profile() {
    let mut doc = document();
    doc.ambient.sources.push(crate::app::ambient::Source {
        key: "main".into(),
        rate_hz: 25,
        crop: None,
        stale_after_ms: 1000,
    });
    let before = doc.clone();
    let path = path();
    let mut w = Workspace::open(&path, &doc).unwrap();
    assert!(w.assign_monitor(&doc, "unknown", r"\\.\DISPLAY1").is_err());
    assert!(w.assign_monitor(&doc, "main", "not-a-monitor").is_err());
    w.assign_monitor(&doc, "main", r"\\.\DISPLAY1").unwrap();
    w.save(&doc).unwrap();
    w.assign_monitor(&doc, "main", r"\\.\DISPLAY2").unwrap();
    w.save(&doc).unwrap();
    assert_eq!(w.local.monitors.monitors.len(), 1);
    assert_eq!(w.local.monitors.monitors[0].device_name, r"\\.\DISPLAY2");
    assert_eq!(doc, before);
    doc.ambient.sources.clear();
    let mut w = Workspace::open(&path, &doc).unwrap();
    assert_eq!(w.local.monitors.monitors.len(), 1);
    assert!(Local::read(&path, &doc).is_err());
    w.prune_orphans(&doc);
    w.save(&doc).unwrap();
    assert!(w.local.monitors.monitors.is_empty());
    w.assign_monitor(&before, "main", r"\\.\DISPLAY3").unwrap();
    w.remove_monitor("main");
    assert!(w.local.monitors.monitors.is_empty());
}
