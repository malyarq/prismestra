#![cfg(windows)]
use pc_control::{
    devices::{Binding, Bindings, Canvas, Device, Driver, Point, Role, Setup},
    runtime::{Host, native, scene},
};
use std::{
    net::UdpSocket,
    time::{Duration, Instant},
};

fn scene() -> scene::Scene {
    scene::Scene {
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
    }
}
fn setup() -> Setup {
    Setup {
        version: 1,
        devices: vec![Device {
            id: "wled".into(),
            label: "Test WLED".into(),
            role: Role::Canvas,
            driver: Driver::WledDdp,
            leds: 2,
            rate_hz: 25,
            enabled: true,
        }],
        canvas: Canvas {
            width: 2,
            height: 1,
            points: (0..2)
                .map(|n| Point {
                    device: "wled".into(),
                    led: n,
                    x: n as u32,
                    y: 0,
                })
                .collect(),
        },
    }
}
#[test]
fn private_binding_apply_scene_udp_black_and_lease_drain_use_the_ordinary_factory() {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let endpoint = socket.local_addr().unwrap();
    let setup = setup();
    let draft = pc_control::app::lighting_bindings::Draft {
        endpoint: endpoint.to_string(),
        ..Default::default()
    };
    let bindings: Bindings = [("wled".into(), draft.binding(&setup.devices[0]).unwrap())].into();
    assert_eq!(
        pc_control::app::lighting_bindings::Draft::from_binding(&bindings["wled"]),
        draft
    );
    assert_eq!(
        pc_control::app::device_setup::preset("wled-ddp")
            .unwrap()
            .device_with_leds("new-strip", 401)
            .unwrap()
            .leds,
        401
    );
    let portable = String::from_utf8(setup.encode().unwrap()).unwrap();
    assert!(!portable.contains(&endpoint.to_string()));
    let encoded = pc_control::devices::encode_bindings(&setup, &bindings).unwrap();
    assert_eq!(
        pc_control::devices::decode_bindings(&setup, &encoded).unwrap(),
        bindings
    );
    let (_keys, _factory) = native::bound_factory(&setup, "wled", &bindings, 15).unwrap();
    socket.set_nonblocking(true).unwrap();
    assert!(
        socket.recv(&mut [0; 64]).is_err(),
        "Creating a deferred factory must not send commands"
    );
    socket.set_nonblocking(false).unwrap();
    let mut host = Host::new(setup.clone(), scene()).unwrap();
    native::attach_bound(&mut host, "wled", &bindings, 15).unwrap();
    let mut second = Host::new(setup, scene()).unwrap();
    assert!(native::attach_bound(&mut second, "wled", &bindings, 15).is_err());
    host.tick(0).unwrap();
    let mut packet = [0; 64];
    let size = socket
        .recv(&mut packet)
        .unwrap_or_else(|e| panic!("{e}; output: {:?}", host.status("wled")));
    let expected = host
        .frame("wled")
        .unwrap()
        .iter()
        .flat_map(|c| [c.r, c.g, c.b])
        .collect::<Vec<_>>();
    assert_eq!(size, 16);
    assert_eq!(&packet[10..size], &expected);
    host.mute("wled", true).unwrap();
    host.tick(1).unwrap();
    let size = socket.recv(&mut packet).unwrap();
    assert_eq!(&packet[10..size], &[0; 6]);
    host.stop();
    let end = Instant::now() + Duration::from_secs(2);
    while !host.drained() && Instant::now() < end {
        std::thread::yield_now();
    }
    assert!(host.drained());
    native::attach_bound(&mut second, "wled", &bindings, 15).unwrap();
    second.stop();
    let end = Instant::now() + Duration::from_secs(2);
    while !second.drained() && Instant::now() < end {
        std::thread::yield_now();
    }
    assert!(second.drained());
}

#[test]
fn invalid_capacity_rate_and_duplicate_canonical_endpoint_cannot_activate() {
    let mut setup = setup();
    setup.devices[0].rate_hz = 61;
    assert!(setup.validate().is_err());
    setup.devices[0].rate_hz = 25;
    let mut second = setup.devices[0].clone();
    second.id = "second".into();
    setup.devices.push(second);
    setup.canvas.points.extend((0..2).map(|n| Point {
        device: "second".into(),
        led: n,
        x: n as u32,
        y: 0,
    }));
    let bindings: Bindings = [
        (
            "wled".into(),
            Binding::WledDdp {
                endpoint: "127.0.0.1:4048".parse().unwrap(),
            },
        ),
        (
            "second".into(),
            Binding::WledDdp {
                endpoint: "[::ffff:127.0.0.1]:4048".parse().unwrap(),
            },
        ),
    ]
    .into();
    assert!(pc_control::devices::validate_bindings(&setup, &bindings).is_err());
    for address in [
        "0.0.0.0:4048",
        "255.255.255.255:4048",
        "224.0.0.1:4048",
        "127.0.0.1:0",
    ] {
        assert!(
            Binding::WledDdp {
                endpoint: address.parse().unwrap()
            }
            .validate(&setup.devices[0])
            .is_err()
        );
    }
}
