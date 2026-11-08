use super::*;
#[test]
fn persisted_identity_key_format_keeps_exact_cpu_and_controller_fields() {
    assert_eq!(
        key(&Target::Amd {
            vendor: *b"AuthenticAMD",
            signature: 0x00800f12,
            packages: 1,
            module_path: String::new()
        }),
        "amd:41757468656e746963414d44:00800f12:1"
    );
    assert_eq!(
        key(&Target::Ddr5 {
            controller: [1, 2, 0x11223344790b1022],
            port: 1,
            address: 0x57,
            module_path: String::new()
        }),
        "ddr5:0000000000000001:0000000000000002:11223344790b1022:1:57"
    );
}
use std::{
    sync::{Condvar, Mutex, mpsc},
    time::{Duration, Instant},
};
fn pci() -> nvidia::PciIdentity {
    nvidia::PciIdentity {
        device_id: 0x248410de,
        subsystem_id: 7,
        revision_id: 1,
        extended_device_id: 9,
    }
}
fn cpu() -> amd::CpuIdentity {
    amd::CpuIdentity {
        vendor: *b"AuthenticAMD",
        signature: 0x00800f10,
        packages: 1,
    }
}
fn binding() -> ddr5::Binding {
    ddr5::Binding {
        controller: [
            u64::from_le_bytes(*b"PIIX4\0\0\0"),
            0xb00,
            0x12340000790b1022,
        ],
        port: 0,
        address: 0x50,
    }
}
fn hid(instance: &str) -> Descriptor {
    Descriptor {
        path: format!(r"\\?\hid#vid_264a&pid_233c&mi_00#{instance}#{{synthetic}}"),
        vid: 0x264a,
        pid: 0x233c,
        usage_page: 0xff06,
        usage: 1,
        input_len: 441,
        output_len: 441,
        feature_len: 0,
        input_buttons: 0,
        output_buttons: 0,
        input_ids: vec![0],
        output_ids: vec![0],
    }
}
fn metadata() -> Metadata {
    Metadata {
        nvidia: Ok(vec![pci()]),
        cpu: Ok(cpu()),
        ddr5: Ok(vec![binding()]),
        liquid: Ok(vec![hid("one")]),
    }
}
fn empty() -> Inventory {
    Inventory::default()
}
fn finish(w: &mut Worker) -> Outcome {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(o) = w.poll() {
            return o;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}
#[test]
fn four_families_and_required_explicit_modules() {
    let inventory = collect(metadata()).unwrap();
    assert_eq!(inventory.candidates.len(), 4);
    assert!(inventory.issues.is_empty());
    inventory.validate().unwrap();
    for c in inventory.candidates {
        let d = c.draft.unwrap();
        match d.family {
            Family::Amd => {
                assert!(d.values[3].is_empty());
                assert!(d.target().is_err());
            }
            Family::Ddr5 => {
                assert!(d.values[5].is_empty());
                assert!(d.target().is_err());
            }
            _ => {
                d.target().unwrap();
            }
        }
        assert!(c.detail.contains("не подтверждают"));
    }
}
#[test]
fn unsupported_identity_and_partial_family_errors_are_visible() {
    for bad in [
        amd::CpuIdentity {
            vendor: *b"GenuineIntel",
            ..cpu()
        },
        amd::CpuIdentity {
            packages: 2,
            ..cpu()
        },
        amd::CpuIdentity {
            signature: 0x600,
            ..cpu()
        },
    ] {
        let mut m = metadata();
        m.cpu = Ok(bad);
        m.nvidia = Err("enumeration failed\n".repeat(1000));
        let i = collect(m).unwrap();
        assert_eq!(i.issues.len(), 1);
        assert!(i.issues[0].len() <= 512);
        assert!(!i.issues[0].contains('\n'));
        let c = i
            .candidates
            .iter()
            .find(|c| c.label.starts_with("CPU"))
            .unwrap();
        assert!(c.draft.is_none());
        let reason = if bad.vendor != *b"AuthenticAMD" {
            "производителя CPU"
        } else if bad.packages != 1 {
            "сокет"
        } else {
            "AMD family"
        };
        assert!(c.detail.contains(reason), "{}", c.detail);
    }
    let m = Metadata {
        nvidia: Err("nv".into()),
        cpu: Err("cpu".into()),
        ddr5: Err("ddr".into()),
        liquid: Err("hid".into()),
    };
    assert!(collect(m).is_err());
    let mut m = metadata();
    m.ddr5.as_mut().unwrap()[0].controller[2] = 0;
    let i = collect(m).unwrap();
    assert!(
        i.candidates
            .iter()
            .find(|c| c.label.starts_with("DDR5"))
            .unwrap()
            .draft
            .is_none()
    );
}
#[test]
fn duplicate_exact_identities_and_case_aliases_cannot_be_selected() {
    let mut m = metadata();
    m.nvidia = Ok(vec![pci(), pci()]);
    m.ddr5 = Ok(vec![binding(), binding()]);
    let one = hid("one");
    let mut alias = one.clone();
    alias.path = alias.path.to_ascii_uppercase();
    m.liquid = Ok(vec![one, alias]);
    let i = collect(m).unwrap();
    assert_eq!(i.candidates.len(), 4);
    for c in i.candidates.iter().filter(|c| !c.label.starts_with("CPU")) {
        assert!(c.draft.is_none());
        assert!(c.detail.contains("2 записей"));
    }
}
#[test]
fn caps_and_full_set_selection_policy_are_preserved() {
    let mut m = metadata();
    let good = hid("good");
    let mut bad = hid("bad");
    bad.input_len = 1;
    m.liquid = Ok(vec![good, bad]);
    let i = collect(m).unwrap();
    assert!(
        i.candidates
            .iter()
            .filter(|c| c.label.starts_with("Жидкость"))
            .all(|c| c.draft.is_none() && c.detail.contains("report length"))
    );
    let mut m = metadata();
    let mut other = hid("other");
    other.path = other.path.replace("mi_00", "mi_01");
    m.liquid = Ok(vec![other]);
    assert!(
        collect(m)
            .unwrap()
            .candidates
            .iter()
            .find(|c| c.label.starts_with("Жидкость"))
            .unwrap()
            .draft
            .is_none()
    );
    let mut m = metadata();
    m.liquid.as_mut().unwrap()[0].input_ids = vec![0; 129];
    assert!(
        collect(m)
            .unwrap()
            .candidates
            .iter()
            .find(|c| c.label.starts_with("Жидкость"))
            .unwrap()
            .draft
            .is_none()
    );
}
#[test]
fn stable_exact_identity_is_independent_of_order_and_retains_all_words() {
    let mut m = metadata();
    let mut second = pci();
    second.extended_device_id += 1;
    m.nvidia = Ok(vec![pci(), second]);
    let a = collect(m).unwrap();
    let mut m = metadata();
    m.nvidia = Ok(vec![second, pci()]);
    let b = collect(m).unwrap();
    assert_eq!(a, b);
    let mut m = metadata();
    m.ddr5.as_mut().unwrap()[0].controller[2] ^= 1 << 63;
    let changed = collect(m).unwrap();
    assert_ne!(
        a.candidates
            .iter()
            .find(|c| c.label.starts_with("DDR5"))
            .unwrap()
            .id,
        changed
            .candidates
            .iter()
            .find(|c| c.label.starts_with("DDR5"))
            .unwrap()
            .id
    );
    let mut m = metadata();
    m.liquid.as_mut().unwrap()[0].path = m.liquid.as_ref().unwrap()[0].path.to_ascii_uppercase();
    let upper = collect(m).unwrap();
    assert_eq!(
        b.candidates
            .iter()
            .find(|c| c.label.starts_with("Жидкость"))
            .unwrap()
            .id,
        upper
            .candidates
            .iter()
            .find(|c| c.label.starts_with("Жидкость"))
            .unwrap()
            .id
    );
}
#[test]
fn bounds_and_malformed_factory_output_are_rejected() {
    let mut m = metadata();
    m.nvidia = Ok(vec![pci(); MAX_FAMILY + 1]);
    assert!(collect(m).is_err());
    let mut m = metadata();
    m.nvidia = Ok(vec![pci(); 64]);
    m.ddr5 = Ok(vec![binding(); 64]);
    assert!(collect(m).is_err());
    let mut m = metadata();
    m.liquid.as_mut().unwrap()[0].path = "x".repeat(4097);
    assert!(collect(m).is_err());
    let original = collect(metadata()).unwrap();
    for bad in 0..7 {
        let mut i = original.clone();
        match bad {
            0 => i.candidates.push(i.candidates[0].clone()),
            1 => i.candidates[0].label = "x".repeat(513),
            2 => i.candidates[0].detail = "\n".into(),
            3 => i.candidates[0].draft.as_mut().unwrap().id = "wrong".into(),
            4 => i.candidates[0].id = "wrong".into(),
            5 => i.issues = vec!["issue".into(); 5],
            _ => {
                let d = i
                    .candidates
                    .iter_mut()
                    .find_map(|c| c.draft.as_mut().filter(|d| d.family == Family::Amd))
                    .unwrap();
                d.values[3] = r"C:\unsolicited.dll".into();
            }
        }
        assert!(i.validate().is_err(), "case {bad}");
        let mut w = Worker::default();
        w.start(Arc::new(move |_| Ok(i.clone()))).unwrap();
        assert!(matches!(
            finish(&mut w),
            Outcome::Failed { generation: 1, .. }
        ));
        assert!(w.is_idle());
    }
}
#[test]
fn worker_cancellation_keeps_blocked_thread_owned_until_terminal_reap() {
    let (entered_tx, entered) = mpsc::channel();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let g = gate.clone();
    let caller = std::thread::current().id();
    let factory: Factory = Arc::new(move |cancel| {
        assert_ne!(caller, std::thread::current().id());
        entered_tx.send(()).unwrap();
        let guard = g.0.lock().unwrap();
        drop(g.1.wait_while(guard, |open| !*open).unwrap());
        assert!(cancel.cancelled());
        Ok(empty())
    });
    let mut w = Worker::default();
    assert_eq!(w.start(factory).unwrap(), 1);
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    w.cancel();
    let now = Instant::now();
    assert!(w.poll().is_none());
    assert!(now.elapsed() < Duration::from_millis(100));
    assert!(!w.is_idle());
    assert!(w.start(Arc::new(|_| Ok(empty()))).is_err());
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    assert!(matches!(
        finish(&mut w),
        Outcome::Cancelled { generation: 1 }
    ));
    assert!(w.is_idle());
    assert!(w.poll().is_none());
    assert_eq!(w.start(Arc::new(|_| Ok(empty()))).unwrap(), 2);
    assert!(matches!(
        finish(&mut w),
        Outcome::Ready { generation: 2, .. }
    ));
}
#[test]
fn failures_panics_and_generation_exhaustion_are_terminal() {
    for panic in [false, true] {
        let mut w = Worker::default();
        w.start(Arc::new(move |_| {
            if panic {
                panic!("synthetic");
            }
            Err("bad\n".repeat(1000))
        }))
        .unwrap();
        match finish(&mut w) {
            Outcome::Failed { generation, error } => {
                assert_eq!(generation, 1);
                assert!(error.len() <= 512);
                assert!(!error.contains('\n'));
            }
            other => panic!("{other:?}"),
        }
        assert!(w.is_idle());
    }
    let mut w = Worker {
        generation: u64::MAX,
        task: None,
    };
    assert!(w.start(Arc::new(|_| Ok(empty()))).is_err());
    assert!(w.is_idle());
}

#[test]
fn finished_thread_must_be_reaped_and_drop_only_cancels_blocked_work() {
    let mut w = Worker::default();
    w.start(Arc::new(|_| Ok(empty()))).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !w.task.as_ref().unwrap().thread.is_finished() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(!w.is_idle());
    assert!(w.start(Arc::new(|_| Ok(empty()))).is_err());
    assert!(matches!(
        w.poll(),
        Some(Outcome::Ready { generation: 1, .. })
    ));
    assert!(w.is_idle());

    let (entered_tx, entered) = mpsc::channel();
    let (done_tx, done) = mpsc::channel();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let g = gate.clone();
    w.start(Arc::new(move |cancel| {
        entered_tx.send(()).unwrap();
        let guard = g.0.lock().unwrap();
        drop(g.1.wait_while(guard, |open| !*open).unwrap());
        done_tx.send(cancel.cancelled()).unwrap();
        Ok(empty())
    }))
    .unwrap();
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    let now = Instant::now();
    drop(w);
    assert!(now.elapsed() < Duration::from_millis(100));
    assert!(done.try_recv().is_err());
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    assert!(done.recv_timeout(Duration::from_secs(2)).unwrap());
}
