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
fn verified_wled_checks_identity_before_any_udp_output_and_adopts_geometry() {
    use pc_control::app::{
        config::{Groups, Project},
        device_setup,
        document::Document,
        lighting_bindings::discovery,
    };
    use pc_control::runtime::output::Cancellation;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
    let http = format!("http://{}", tcp.local_addr().unwrap());
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    udp.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let ddp = udp.local_addr().unwrap();
    let peer = std::thread::spawn(move || {
        for count in [3, 4] {
            let (mut stream, _) = tcp.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
                assert!(bytes.len() < 8192);
            }
            assert!(bytes.starts_with(b"GET /json/info HTTP/1.1"));
            let body =
                format!(r#"{{"mac":"02aabbccddee","name":"Fixture","leds":{{"count":{count}}}}}"#);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    let before = Document::from_lighting(Project {
        version: 1,
        schedule: Default::default(),
        setup: setup(),
        scene: scene(),
        groups: Groups {
            pc: vec!["wled".into()],
            door: vec![],
        },
    })
    .unwrap();
    let target = discovery::Target::Device(before.lighting.setup.devices[0].clone());
    let inventory = discovery::wled_factory(ddp, http)(target, Cancellation::default()).unwrap();
    let candidate = &inventory.candidates[0];
    let after =
        device_setup::adopt_wled(&before, "wled", candidate.wled.as_ref().unwrap()).unwrap();
    assert_eq!(before.lighting.setup.devices[0].leds, 2);
    assert_eq!(after.lighting.setup.devices[0].leds, 3);
    assert_eq!(after.lighting.setup.canvas.points.len(), 3);
    let same = device_setup::adopt_wled(&after, "wled", candidate.wled.as_ref().unwrap()).unwrap();
    assert_eq!(same, after);
    let binding = candidate
        .draft
        .binding(&after.lighting.setup.devices[0])
        .unwrap();
    let bindings = [("wled".into(), binding)].into();
    let (_, factory) = native::bound_factory(&after.lighting.setup, "wled", &bindings, 15).unwrap();
    assert!(
        factory(Cancellation::default()).is_err(),
        "changed count must reject activation"
    );
    let mut packet = [0; 64];
    assert!(
        udp.recv(&mut packet).is_err(),
        "metadata queries or failed activation must not send RGB"
    );
    peer.join().unwrap();
}
#[test]
fn discovered_identity_survives_save_output_and_rejects_a_replaced_controller() {
    use pc_control::app::{
        config::{Groups, Project},
        device_setup,
        document::Document,
        lighting_bindings::discovery,
    };
    use pc_control::runtime::output::{Cancellation, Phase};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
    tcp.set_nonblocking(true).unwrap();
    let http = format!("http://{}", tcp.local_addr().unwrap());
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    udp.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let ddp = udp.local_addr().unwrap();
    let peer = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        for mac in ["02aabbccddee", "02aabbccddee", "02aabbccddff"] {
            let mut stream = loop {
                match tcp.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "missing metadata request");
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("metadata fixture: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() < 8192);
            }
            assert!(request.starts_with(b"GET /json/info HTTP/1.1"));
            let body = format!(r#"{{"mac":"{mac}","leds":{{"count":3}}}}"#);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    let before = Document::from_lighting(Project {
        version: 1,
        setup: setup(),
        scene: scene(),
        schedule: Default::default(),
        groups: Groups {
            pc: vec!["wled".into()],
            door: vec![],
        },
    })
    .unwrap();
    let inventory = discovery::wled_factory(ddp, http)(
        discovery::Target::Device(before.lighting.setup.devices[0].clone()),
        Cancellation::default(),
    )
    .unwrap();
    let candidate = &inventory.candidates[0];
    let adopted =
        device_setup::adopt_wled(&before, "wled", candidate.wled.as_ref().unwrap()).unwrap();
    let document = Document::decode(&adopted.encode().unwrap()).unwrap();
    let setup = &document.lighting.setup;
    let bindings = [(
        "wled".into(),
        candidate.draft.binding(&setup.devices[0]).unwrap(),
    )]
    .into();
    let saved = pc_control::devices::encode_bindings(setup, &bindings).unwrap();
    let bindings = pc_control::devices::decode_bindings(setup, &saved).unwrap();
    let mut host = Host::new(setup.clone(), document.lighting.scene.clone()).unwrap();
    native::attach_bound(&mut host, "wled", &bindings, 15).unwrap();
    host.tick(0).unwrap();
    let mut packet = [0; 64];
    udp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let size = udp.recv(&mut packet).unwrap();
    let expected = host
        .frame("wled")
        .unwrap()
        .iter()
        .flat_map(|c| [c.r, c.g, c.b])
        .collect::<Vec<_>>();
    assert_eq!(size, 19);
    assert_eq!(&packet[10..size], expected);

    // Another address for the same MAC cannot create a second physical owner.
    let mut alias = bindings.clone();
    let other = UdpSocket::bind("127.0.0.1:0").unwrap();
    if let Binding::WledDdp { endpoint, .. } = alias.get_mut("wled").unwrap() {
        *endpoint = other.local_addr().unwrap();
    }
    let mut second = Host::new(setup.clone(), scene()).unwrap();
    assert!(native::attach_bound(&mut second, "wled", &alias, 15).is_err());
    host.stop();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !host.drained() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    // Reopening uses saved identity, not just the old IP or unchanged LED count.
    native::attach_bound(&mut second, "wled", &bindings, 15).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while second.status("wled").unwrap().phase != Phase::Failed {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let status = second.status("wled").unwrap();
    assert_eq!(status.submitted, 0);
    assert!(status.error.unwrap().contains("identity changed"));
    udp.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    assert!(
        udp.recv(&mut packet).is_err(),
        "replacement must receive no RGB"
    );
    second.stop();
    while !second.drained() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    peer.join().unwrap();
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
                controller: None,
                endpoint: "127.0.0.1:4048".parse().unwrap(),
            },
        ),
        (
            "second".into(),
            Binding::WledDdp {
                controller: None,
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
                controller: None,
                endpoint: address.parse().unwrap()
            }
            .validate(&setup.devices[0])
            .is_err()
        );
    }
}
