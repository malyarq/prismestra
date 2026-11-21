#![cfg(windows)]
#[path = "support/openrgb_peer.rs"]
mod openrgb_peer;
use openrgb_peer::Peer;
use pc_control::{
    app::{
        config::{Groups, Project},
        device_setup,
        document::Document,
        lighting_bindings::{Draft, discovery},
        runtime::{OutputPlan, Runtime},
    },
    devices::{Binding, Bindings, Canvas, Device, Driver, Point, Role, Setup},
    lighting::color::Rgb,
    runtime::{native, output::Cancellation, scene},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
fn document() -> Document {
    let setup = Setup {
        version: 1,
        devices: vec![Device {
            id: "rgb".into(),
            label: "OpenRGB controller".into(),
            role: Role::Canvas,
            driver: Driver::OpenRgb,
            leds: 1,
            rate_hz: 25,
            enabled: true,
        }],
        canvas: Canvas {
            width: 100,
            height: 80,
            points: vec![Point {
                device: "rgb".into(),
                led: 0,
                x: 0,
                y: 0,
            }],
        },
    };
    let scene = scene::Scene {
        effect: pc_control::lighting::scene::Effect::NoiseMap,
        colors: vec![0xff8800],
        brightness: 60,
        speed: 8,
        fps: 25,
        temperature: 0,
        tint: 0,
        noise: scene::NoiseSettings {
            frequency: 0.055,
            amplitude: 1.0,
            lacunarity: 0.75,
            persistence: 0.4,
            octaves: 2,
            motion: 0,
            motion_speed: 1,
        },
        noise_palette: scene::NoisePalette::Custom,
        rainbow_width: 10,
    };
    Document::from_lighting(Project {
        version: 1,
        setup,
        scene,
        schedule: Default::default(),
        groups: Groups {
            pc: vec!["rgb".into()],
            door: vec![],
        },
    })
    .unwrap()
}
fn wait(mut ready: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(4);
    while !ready() {
        assert!(Instant::now() < until, "loopback integration timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn tick(engine: &mut Runtime) {
    engine
        .tick(
            pc_control::clock::monotonic_us(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        )
        .unwrap();
}
#[test]
fn manual_openrgb_retry_preserves_wled_and_revalidates_the_saved_controller() {
    use pc_control::app::runtime::OpenRgbRetryStatus;
    use pc_control::runtime::output::Phase;
    use std::net::UdpSocket;

    let peer = Peer::new(false);
    let before = document();
    let inventory = discovery::openrgb_factory(peer.endpoint)(
        discovery::Target::Device(before.lighting.setup.devices[0].clone()),
        Cancellation::default(),
    )
    .unwrap();
    let candidate = &inventory.candidates[0];
    let mut document =
        device_setup::adopt_openrgb(&before, "rgb", candidate.controller.as_ref().unwrap())
            .unwrap();
    let wled = UdpSocket::bind("127.0.0.1:0").unwrap();
    wled.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let setup = &mut document.lighting.setup;
    setup.devices.push(Device {
        id: "wled".into(),
        label: "Loopback WLED".into(),
        driver: Driver::WledDdp,
        leds: 1,
        ..setup.devices[0].clone()
    });
    setup.canvas.points.push(Point {
        device: "wled".into(),
        led: 0,
        x: 0,
        y: 1,
    });
    document.lighting.groups.pc.push("wled".into());
    let bindings: Bindings = [
        (
            "rgb".into(),
            candidate.draft.binding(&setup.devices[0]).unwrap(),
        ),
        (
            "wled".into(),
            Binding::WledDdp {
                endpoint: wled.local_addr().unwrap(),
                controller: None,
            },
        ),
    ]
    .into();
    let mut engine = Runtime::new(document.clone(), None).unwrap();
    for id in ["rgb", "wled"] {
        let (keys, factory) =
            native::bound_factory(&document.lighting.setup, id, &bindings, 15).unwrap();
        engine.attach_rgb(id, keys, factory).unwrap();
        engine.enable_output_recovery(id).unwrap();
    }
    let step = |engine: &mut Runtime, time| engine.tick(time, 1_800_000_000).unwrap();
    let mut now = 0;
    wait(|| {
        now += 50_000;
        step(&mut engine, now);
        ["rgb", "wled"]
            .iter()
            .all(|id| engine.host().status(id).unwrap().submitted > 0)
    });
    let mut datagram = [0; 64];
    assert_eq!(wled.recv(&mut datagram).unwrap(), 13);
    let wled_start = engine.host().status("wled").unwrap();
    let direct = || {
        peer.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.1 == 1101)
            .count()
    };
    assert_eq!(direct(), 1);
    assert!(engine.openrgb_retry_ticket("rgb").is_none());

    peer.set_paused(true);
    now += 50_000;
    wait(|| {
        step(&mut engine, now);
        engine.host().status("rgb").unwrap().phase == Phase::Failed
    });
    for delay in [1_000_000, 5_000_000, 15_000_000] {
        step(&mut engine, now);
        let generation = engine.host().status("rgb").unwrap().generation;
        now += delay;
        wait(|| {
            step(&mut engine, now);
            let snapshot = engine.host().status("rgb").unwrap();
            snapshot.generation != generation && snapshot.phase == Phase::Failed
        });
    }
    step(&mut engine, now);
    peer.set_paused(false);
    now += 60_000_000;
    step(&mut engine, now);
    assert_eq!(direct(), 1);
    let ticket = engine.openrgb_retry_ticket("rgb").unwrap();
    assert!(!engine.request_openrgb_retry("wled", ticket));
    assert!(engine.request_openrgb_retry("rgb", ticket));
    assert_eq!(
        engine.openrgb_retry_status("rgb", ticket),
        OpenRgbRetryStatus::Pending
    );
    assert!(!engine.request_openrgb_retry("rgb", ticket));
    assert!(engine.cancel_openrgb_retry("rgb", ticket));
    assert_eq!(
        engine.openrgb_retry_status("rgb", ticket),
        OpenRgbRetryStatus::Cancelled
    );
    step(&mut engine, now);
    assert!(!engine.request_openrgb_retry("rgb", ticket));
    let next = engine.openrgb_retry_ticket("rgb").unwrap();
    assert_eq!(next.generation, ticket.generation);
    assert_ne!(next, ticket);

    let calls_before_foreign_controller = peer.calls.lock().unwrap().len();
    peer.change_identity(true);
    assert!(engine.request_openrgb_retry("rgb", next));
    wait(|| {
        step(&mut engine, now);
        engine
            .openrgb_retry_ticket("rgb")
            .is_some_and(|ticket| ticket.generation != next.generation)
    });
    assert_eq!(
        direct(),
        1,
        "A different controller must not receive Direct"
    );
    {
        let calls = peer.calls.lock().unwrap();
        let foreign_attempt = &calls[calls_before_foreign_controller..];
        assert!(
            foreign_attempt
                .iter()
                .any(|(id, packet, _)| *id == 8 && *packet == 1),
            "The mismatched controller must actually be read"
        );
        assert!(
            foreign_attempt
                .iter()
                .all(|(_, packet, _)| !matches!(packet, 1101 | 1050)),
            "A different controller must not receive Direct or an RGB frame"
        );
    }
    assert!(
        matches!(engine.openrgb_retry_status("rgb", next), OpenRgbRetryStatus::Failed(error) if !error.is_empty())
    );
    assert_eq!(engine.host().status("rgb").unwrap().submitted, 0);
    assert!(!engine.request_openrgb_retry("rgb", ticket));

    peer.change_identity(false);
    let (keys, _) = native::bound_factory(&document.lighting.setup, "rgb", &bindings, 15).unwrap();
    let mut lease_holder = Runtime::new(document.clone(), None).unwrap();
    wait(|| {
        step(&mut engine, now);
        lease_holder
            .attach_rgb(
                "rgb",
                keys.clone(),
                std::sync::Arc::new(|cancel| {
                    while !cancel.cancelled() {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err("fixture lease released".into())
                }),
            )
            .is_ok()
    });
    let ticket = engine.openrgb_retry_ticket("rgb").unwrap();
    let before_busy_retry = peer.calls.lock().unwrap().len();
    assert!(engine.request_openrgb_retry("rgb", ticket));
    wait(|| {
        step(&mut engine, now);
        !matches!(
            engine.openrgb_retry_status("rgb", ticket),
            OpenRgbRetryStatus::Pending
        )
    });
    let result = engine.openrgb_retry_status("rgb", ticket);
    assert!(
        matches!(&result, OpenRgbRetryStatus::Failed(error) if error.contains("still owned by another output")),
        "{result:?}"
    );
    assert_eq!(peer.calls.lock().unwrap().len(), before_busy_retry);
    assert!(!engine.request_openrgb_retry("rgb", ticket));
    lease_holder.stop();
    wait(|| lease_holder.drained());

    let stale = engine.openrgb_retry_ticket("rgb").unwrap();
    assert!(engine.request_openrgb_retry("rgb", stale));
    let mut scene = document.lighting.scene.clone();
    scene.colors = vec![0x00ff00];
    engine.set_scene(scene).unwrap();
    step(&mut engine, now);
    assert_eq!(
        engine.openrgb_retry_status("rgb", stale),
        OpenRgbRetryStatus::Cancelled
    );
    assert_eq!(direct(), 1);
    let ticket = engine.openrgb_retry_ticket("rgb").unwrap();
    assert!(engine.request_openrgb_retry("rgb", ticket));
    wait(|| {
        now += 50_000;
        step(&mut engine, now);
        engine.host().status("rgb").unwrap().submitted > 0
    });
    assert_eq!(direct(), 2);
    assert_eq!(
        engine.openrgb_retry_status("rgb", ticket),
        OpenRgbRetryStatus::Submitted
    );
    let calls = peer.calls.lock().unwrap();
    let (_, _, frame) = calls.iter().rev().find(|call| call.1 == 1050).unwrap();
    assert!(
        frame[7] > frame[6] && frame[7] > frame[8],
        "Expected current green scene"
    );
    drop(calls);
    let modes_before = direct();
    let (keys, factory) =
        native::bound_factory(&document.lighting.setup, "rgb", &bindings, 15).unwrap();
    let mut competitor = Runtime::new(document.clone(), None).unwrap();
    assert!(
        competitor
            .attach_rgb("rgb", keys, factory)
            .unwrap_err()
            .contains("still owned by another output")
    );
    assert_eq!(direct(), modes_before);
    competitor.stop();
    let wled_end = engine.host().status("wled").unwrap();
    assert_eq!(wled_end.generation, wled_start.generation);
    assert!(wled_end.submitted > wled_start.submitted);
    assert_eq!(wled.recv(&mut datagram).unwrap(), 13);

    peer.set_paused(true);
    wait(|| {
        now += 50_000;
        step(&mut engine, now);
        engine.openrgb_retry_ticket("rgb").is_some()
    });
    let ticket = engine.openrgb_retry_ticket("rgb").unwrap();
    assert!(engine.request_openrgb_retry("rgb", ticket));
    engine.stop();
    assert!(!engine.cancel_openrgb_retry("rgb", ticket));
    peer.set_paused(false);
    wait(|| engine.drained());
    assert_eq!(direct(), 2);
}

#[test]
fn discovery_id_reorder_geometry_and_explicit_apply_reach_the_real_sdk_adapter() {
    let peer = Peer::new(false);
    let before = document();
    let inventory = discovery::openrgb_factory(peer.endpoint)(
        discovery::Target::Device(before.lighting.setup.devices[0].clone()),
        Cancellation::default(),
    )
    .unwrap();
    assert_eq!(inventory.candidates.len(), 1);
    let candidate = &inventory.candidates[0];
    assert!(
        peer.calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c.1 != 1101 && c.1 != 1050),
        "Discovery must never activate Direct or send RGB"
    );
    let after = device_setup::adopt_openrgb(&before, "rgb", candidate.controller.as_ref().unwrap())
        .unwrap();
    assert_eq!(before.lighting.setup.devices[0].leds, 1);
    assert_eq!(after.lighting.setup.devices[0].leds, 2);
    assert_eq!(
        after
            .lighting
            .setup
            .canvas
            .points
            .iter()
            .map(|p| p.x)
            .collect::<Vec<_>>(),
        vec![0, 99]
    );
    let binding = candidate
        .draft
        .binding(&after.lighting.setup.devices[0])
        .unwrap();
    assert_eq!(Draft::from_binding(&binding), candidate.draft);
    let bindings: Bindings = [("rgb".into(), binding)].into();
    let (keys, factory) =
        native::bound_factory(&after.lighting.setup, "rgb", &bindings, 15).unwrap();
    assert!(peer.calls.lock().unwrap().iter().all(|c| c.1 != 1101));
    let mut engine = Runtime::new(before.clone(), None).unwrap();
    engine
        .request_outputs_for(
            after.clone(),
            OutputPlan {
                rgb: vec![("rgb".into(), keys, factory)],
                ..Default::default()
            },
        )
        .unwrap();
    wait(|| {
        tick(&mut engine);
        peer.calls.lock().unwrap().iter().any(|c| c.1 == 1050)
    });
    assert_eq!(engine.document(), &after);
    {
        let calls = peer.calls.lock().unwrap();
        let modes = calls.iter().filter(|c| c.1 == 1101).collect::<Vec<_>>();
        assert_eq!(modes.len(), 1);
        assert_eq!(
            modes[0].0, 8,
            "A numeric ID saved from discovery would target the wrong controller"
        );
        let frame = calls.iter().find(|c| c.1 == 1050).unwrap();
        assert_eq!(frame.0, 8);
        assert_eq!(frame.2.len(), 14);
        assert_eq!(&frame.2[4..6], &2u16.to_le_bytes());
    }
    engine.stop();
    wait(|| engine.drained());
    let portable = String::from_utf8(after.encode().unwrap()).unwrap();
    assert!(!portable.contains(&peer.endpoint.to_string()));
}
#[test]
fn ambiguous_or_stale_topology_is_rejected_before_direct_mode() {
    let peer = Peer::new(true);
    let document = document();
    let inventory = discovery::openrgb_factory(peer.endpoint)(
        discovery::Target::Device(document.lighting.setup.devices[0].clone()),
        Cancellation::default(),
    )
    .unwrap();
    assert!(inventory.candidates.is_empty());
    assert!(inventory.issues.iter().any(|i| i.contains("неоднозначная")));
    assert!(
        peer.calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c.1 != 1101 && c.1 != 1050)
    );
    let peer = Peer::new(false);
    let inventory = discovery::openrgb_factory(peer.endpoint)(
        discovery::Target::Device(document.lighting.setup.devices[0].clone()),
        Cancellation::default(),
    )
    .unwrap();
    let candidate = &inventory.candidates[0];
    let after =
        device_setup::adopt_openrgb(&document, "rgb", candidate.controller.as_ref().unwrap())
            .unwrap();
    let mut binding = candidate
        .draft
        .binding(&after.lighting.setup.devices[0])
        .unwrap();
    let Binding::OpenRgb { topology, .. } = &mut binding else {
        panic!()
    };
    *topology = "0".repeat(64);
    let bindings: Bindings = [("rgb".into(), binding)].into();
    let (_, factory) = native::bound_factory(&after.lighting.setup, "rgb", &bindings, 15).unwrap();
    assert!(factory(Cancellation::default()).is_err());
    assert!(
        peer.calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c.1 != 1101 && c.1 != 1050)
    );
}

#[test]
fn late_duplicate_with_different_topology_blocks_direct_mode() {
    let peer = Peer::becomes_ambiguous_on_activation();
    let before = document();
    let inventory = discovery::openrgb_factory(peer.endpoint)(
        discovery::Target::Device(before.lighting.setup.devices[0].clone()),
        Cancellation::default(),
    )
    .unwrap();
    let [candidate] = inventory.candidates.as_slice() else {
        panic!("discovery should see one controller");
    };
    let after = device_setup::adopt_openrgb(&before, "rgb", candidate.controller.as_ref().unwrap())
        .unwrap();
    let binding = candidate
        .draft
        .binding(&after.lighting.setup.devices[0])
        .unwrap();
    let bindings: Bindings = [("rgb".into(), binding)].into();
    let (_, factory) = native::bound_factory(&after.lighting.setup, "rgb", &bindings, 15).unwrap();

    let result = factory(Cancellation::default());
    {
        let calls = peer.calls.lock().unwrap();
        assert!(calls.iter().any(|c| c.0 == 9 && c.1 == 1));
        assert!(
            calls.iter().all(|c| c.1 != 1101 && c.1 != 1050),
            "ambiguous physical ownership must block all lighting writes"
        );
    }
    assert!(result.err().unwrap().contains("ambiguous"));

    let cancel = Cancellation::default();
    let mut output = factory(cancel.clone()).unwrap();
    output
        .write(
            &[Rgb { r: 1, g: 2, b: 3 }, Rgb { r: 4, g: 5, b: 6 }],
            &cancel,
        )
        .unwrap();
    let calls = peer.calls.lock().unwrap();
    assert_eq!(calls.iter().filter(|c| c.1 == 1101).count(), 1);
    let frame = calls.iter().find(|c| c.0 == 8 && c.1 == 1050).unwrap();
    assert_eq!(&frame.2[6..], &[1, 2, 3, 0, 4, 5, 6, 0]);
}

#[test]
fn invalid_geometry_only_excludes_the_affected_physical_identity() {
    let peer = Peer::with_invalid_neighbor("INVALID-SERIAL");
    let before = document();
    let inventory = discovery::openrgb_factory(peer.endpoint)(
        discovery::Target::Device(before.lighting.setup.devices[0].clone()),
        Cancellation::default(),
    )
    .unwrap();
    let [candidate] = inventory.candidates.as_slice() else {
        panic!("the controller with valid geometry should remain visible");
    };
    let controller = candidate.controller.as_ref().unwrap();
    assert_eq!(controller.serial, "FAKE-SERIAL");
    assert_eq!(controller.leds, 2);
    assert!(
        inventory
            .issues
            .iter()
            .any(|issue| issue.contains("invalid geometry"))
    );
    let after = device_setup::adopt_openrgb(&before, "rgb", controller).unwrap();
    let binding = candidate
        .draft
        .binding(&after.lighting.setup.devices[0])
        .unwrap();
    let bindings: Bindings = [("rgb".into(), binding.clone())].into();
    let (_, factory) = native::bound_factory(&after.lighting.setup, "rgb", &bindings, 15).unwrap();
    let cancel = Cancellation::default();
    let mut output = factory(cancel.clone()).unwrap();
    output
        .write(&[Rgb { r: 1, g: 2, b: 3 }; 2], &cancel)
        .unwrap();
    assert!(
        peer.calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.0 == 8 && c.1 == 1050)
    );

    let duplicate = Peer::with_invalid_neighbor("FAKE-SERIAL");
    let duplicate_inventory = discovery::openrgb_factory(duplicate.endpoint)(
        discovery::Target::Device(before.lighting.setup.devices[0].clone()),
        Cancellation::default(),
    )
    .unwrap();
    assert!(duplicate_inventory.candidates.is_empty());
    assert!(
        duplicate_inventory
            .issues
            .iter()
            .any(|issue| issue.contains("неоднозначная"))
    );
    let Binding::OpenRgb {
        identity, topology, ..
    } = binding
    else {
        panic!("expected an OpenRGB binding");
    };
    let bindings: Bindings = [(
        "rgb".into(),
        Binding::OpenRgb {
            endpoint: duplicate.endpoint,
            identity,
            topology,
        },
    )]
    .into();
    let (_, factory) = native::bound_factory(&after.lighting.setup, "rgb", &bindings, 15).unwrap();
    assert!(
        factory(Cancellation::default())
            .err()
            .unwrap()
            .contains("ambiguous")
    );
    assert!(
        duplicate
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c.1 != 1101 && c.1 != 1050)
    );
}

#[test]
fn virtual_controllers_are_excluded_even_if_a_private_binding_is_forged() {
    let peer = Peer::virtual_controller();
    let mut document = document();
    let inventory = discovery::openrgb_factory(peer.endpoint)(
        discovery::Target::Device(document.lighting.setup.devices[0].clone()),
        Cancellation::default(),
    )
    .unwrap();
    assert!(inventory.candidates.is_empty());
    assert!(
        inventory
            .issues
            .iter()
            .any(|issue| issue.contains("виртуальный контроллер исключён"))
    );
    use pc_control::transport::openrgb::OpenRgbSession;
    let deadline = || Instant::now() + Duration::from_secs(3);
    let mut session = OpenRgbSession::connect(peer.endpoint, deadline(), &|| false).unwrap();
    let info = session.scan(deadline(), &|| false).unwrap().remove(0);
    drop(session);
    document.lighting.setup.devices[0].leds = info.leds;
    document.lighting.setup.canvas.points = (0..info.leds)
        .map(|n| Point {
            device: "rgb".into(),
            led: n,
            x: n as u32,
            y: 0,
        })
        .collect();
    let binding = Binding::OpenRgb {
        endpoint: peer.endpoint,
        identity: pc_control::devices::openrgb::identity(&info),
        topology: pc_control::devices::openrgb::topology(&info),
    };
    let bindings: Bindings = [("rgb".into(), binding)].into();
    let (_, factory) =
        native::bound_factory(&document.lighting.setup, "rgb", &bindings, 15).unwrap();
    let error = factory(Cancellation::default()).err().unwrap();
    assert!(error.contains("virtual controllers"));
    assert!(
        peer.calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c.1 != 1101 && c.1 != 1050)
    );
}

#[test]
fn identical_physical_identity_on_two_server_addresses_is_rejected_before_factory() {
    use pc_control::devices::{Bindings, validate_bindings};
    let mut doc = document();
    let mut second = doc.lighting.setup.devices[0].clone();
    second.id = "second".into();
    doc.lighting.setup.devices.push(second);
    doc.lighting.setup.canvas.points.push(Point {
        device: "second".into(),
        led: 0,
        x: 1,
        y: 0,
    });
    let binding = |endpoint: &str| Binding::OpenRgb {
        endpoint: endpoint.parse().unwrap(),
        identity: "a".repeat(64),
        topology: "b".repeat(64),
    };
    let bindings: Bindings = [
        ("rgb".into(), binding("127.0.0.1:6742")),
        ("second".into(), binding("192.168.1.10:6742")),
    ]
    .into();
    assert!(validate_bindings(&doc.lighting.setup, &bindings).is_err());
    assert!(
        pc_control::runtime::native::bound_factory(&doc.lighting.setup, "second", &bindings, 15)
            .is_err()
    );
}
