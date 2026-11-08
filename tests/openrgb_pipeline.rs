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
