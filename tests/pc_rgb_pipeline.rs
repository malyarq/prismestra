//! Shared Host -> real packet controller -> memory-only transport. No device APIs.
use pc_control::{
    devices::{self, Binding, Bindings, Canvas, Device, Driver, Point, Role, Setup},
    lighting::{color::Rgb, door},
    runtime::{Host, adapters, scene::Scene},
    transport::{ble, ene, fusion2, valkyrie},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
fn device(id: &str, driver: Driver, leds: usize) -> Device {
    Device {
        id: id.into(),
        label: id.into(),
        role: Role::Canvas,
        driver,
        leds,
        rate_hz: 48,
        enabled: true,
    }
}
fn setup() -> Setup {
    let devices = vec![
        device("ram-a", Driver::EneDram, 8),
        device("ram-b", Driver::EneDram, 8),
        device("board", Driver::Fusion2 { headers: [27, 27] }, 54),
        device(
            "keys",
            Driver::Valkyrie {
                model: "vk99".into(),
            },
            102,
        ),
        device("door", Driver::DoorBle, 20),
    ];
    let mut points = vec![];
    for d in &devices {
        for led in 0..d.leds {
            let n = points.len() as u32;
            points.push(Point {
                device: d.id.clone(),
                led,
                x: n % 100,
                y: n / 100,
            });
        }
    }
    Setup {
        version: 1,
        devices,
        canvas: Canvas {
            width: 100,
            height: 64,
            points,
        },
    }
}
fn scene() -> Scene {
    Scene::from_native_noise(&serde_json::json!({"EffectClassName":"NoiseMap","FPS":25,"Speed":8,"Brightness":60,"Temperature":0,"Tint":0,"Slider2Val":1,
        "CustomSettings":{"frequency":0.055,"amplitude":1.0,"lacunarity":0.75,"persistence":0.4,"octaves":2,"motion":0,"motion_speed":1,"colors_choice":2,"colors":[274,1341695]}})).unwrap()
}
fn wait_submitted(host: &Host, count: u64) {
    let until = Instant::now() + Duration::from_secs(2);
    while !host
        .setup()
        .devices
        .iter()
        .all(|d| host.status(&d.id).is_some_and(|s| s.submitted >= count))
    {
        assert!(
            Instant::now() < until,
            "worker failed or timed out: {:?}",
            host.setup()
                .devices
                .iter()
                .map(|d| host.status(&d.id))
                .collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn stop(host: &mut Host) {
    host.stop();
    let until = Instant::now() + Duration::from_secs(2);
    while !host.drained() {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[derive(Default)]
struct Capture {
    reports: Vec<Vec<u8>>,
    closed: bool,
    fail_opcode: Option<u8>,
}
struct Fusion(Arc<Mutex<Capture>>);
impl Drop for Fusion {
    fn drop(&mut self) {
        self.0.lock().unwrap().closed = true;
    }
}
fn identity() -> fusion2::Report {
    let hex = include_str!("../src/transport/fixtures/fusion2-native.tsv")
        .lines()
        .next()
        .unwrap()
        .split('\t')
        .nth(2)
        .unwrap();
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    bytes.try_into().unwrap()
}
impl fusion2::Transport for Fusion {
    fn feature_send(&mut self, p: &fusion2::Report) -> Result<usize, String> {
        let mut state = self.0.lock().unwrap();
        state.reports.push(p.to_vec());
        if state.fail_opcode == Some(p[1]) {
            return Err("Injected second-header error".into());
        }
        Ok(p.len())
    }
    fn feature_get(&mut self, p: &mut fusion2::Report) -> Result<usize, String> {
        *p = identity();
        Ok(p.len())
    }
    fn wait_ms(&mut self, _: u32) {}
}
struct Keyboard(Arc<Mutex<Capture>>);

struct Door(Arc<Mutex<Capture>>);
impl Drop for Door {
    fn drop(&mut self) {
        self.0.lock().unwrap().closed = true;
    }
}
impl ble::Link for Door {
    fn address(&self) -> Result<u64, String> {
        Ok(1)
    }
    fn connected(&self) -> Result<bool, String> {
        Ok(true)
    }
    fn max_pdu(&self) -> Result<u32, String> {
        Ok(98)
    }
    fn write(&mut self, p: &[u8], control: &mut dyn ble::Control) -> Result<(), String> {
        if control.cancelled() {
            return Err("Stopped fake BLE".into());
        }
        assert!(ble::valid_packet(p));
        self.0.lock().unwrap().reports.push(p.to_vec());
        Ok(())
    }
}
impl Drop for Keyboard {
    fn drop(&mut self) {
        self.0.lock().unwrap().closed = true;
    }
}
impl valkyrie::Transport for Keyboard {
    fn send_feature(&mut self, p: &[u8; 65]) -> Result<usize, String> {
        self.0.lock().unwrap().reports.push(p.to_vec());
        Ok(p.len())
    }
    fn get_feature(&mut self, p: &mut [u8; 65]) -> Result<usize, String> {
        p.fill(0);
        Ok(p.len())
    }
    fn sleep(&mut self, _: Duration) {}
}
#[derive(Default)]
struct RamState {
    memory: BTreeMap<u16, u8>,
    pointer: u16,
    closed: bool,
    transactions: u64,
}
struct Ram(Arc<Mutex<RamState>>);
fn ram() -> Arc<Mutex<RamState>> {
    let mut s = RamState::default();
    for (i, b) in ene::VERSION.bytes().enumerate() {
        s.memory.insert(0x1000 + i as u16, b);
    }
    s.memory.insert(0x1c02, 8);
    s.memory.insert(0x1c03, 8);
    s.memory.insert(0x1c13, 0x8a);
    Arc::new(Mutex::new(s))
}
impl Drop for Ram {
    fn drop(&mut self) {
        self.0.lock().unwrap().closed = true;
    }
}
impl ene::Bus for Ram {
    fn transaction<R>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<R, String>,
    ) -> Result<R, String> {
        self.0.lock().unwrap().transactions += 1;
        f(self)
    }
    fn write_word(&mut self, _: u8, c: u8, v: u16) -> Result<(), String> {
        assert_eq!(c, 0);
        self.0.lock().unwrap().pointer = v.swap_bytes();
        Ok(())
    }
    fn read_byte(&mut self, _: u8, c: u8) -> Result<u8, String> {
        assert_eq!(c, 0x81);
        let s = self.0.lock().unwrap();
        Ok(*s.memory.get(&s.pointer).unwrap_or(&0))
    }
    fn write_byte(&mut self, _: u8, c: u8, v: u8) -> Result<(), String> {
        assert_eq!(c, 1);
        let mut s = self.0.lock().unwrap();
        let p = s.pointer;
        s.memory.insert(p, v);
        s.pointer += 1;
        Ok(())
    }
    fn write_block(&mut self, _: u8, c: u8, b: &[u8]) -> Result<ene::BlockWrite, String> {
        assert_eq!(c, 3);
        let mut s = self.0.lock().unwrap();
        for b in b {
            let p = s.pointer;
            s.memory.insert(p, *b);
            s.pointer += 1;
        }
        Ok(ene::BlockWrite::Written)
    }
}
fn keyboard_descriptor() -> valkyrie::Descriptor {
    valkyrie::Descriptor {
        path: r"\\?\hid#vid_05ac&pid_024f&mi_02#stand#{guid}".into(),
        vid: 0x05ac,
        pid: 0x024f,
        interface: 2,
        usage_page: 0xff13,
        usage: 1,
        feature_len: 65,
        feature_ids: vec![0],
        feature_caps_complete: true,
    }
}
#[test]
fn all_pc_outputs_receive_shared_canvas_then_black_and_release() {
    let mut host = Host::new(setup(), scene()).unwrap();
    let a = ram();
    let b = ram();
    for (id, address, state) in [("ram-a", 0x71, a.clone()), ("ram-b", 0x73, b.clone())] {
        host.attach(id, move |_| {
            Ok(Box::new(adapters::Ene::new(Ram(state), address)?))
        })
        .unwrap();
    }
    let board = Arc::new(Mutex::new(Capture::default()));
    let board_io = board.clone();
    host.attach("board", move |_| {
        Ok(Box::new(adapters::Fusion2::new(
            Fusion(board_io),
            [27, 27],
        )?))
    })
    .unwrap();
    let keys = Arc::new(Mutex::new(Capture::default()));
    let keys_io = keys.clone();
    host.attach("keys", move |_| {
        Ok(Box::new(adapters::Valkyrie::new(
            valkyrie::Model::Normal,
            &keyboard_descriptor(),
            Keyboard(keys_io),
        )?))
    })
    .unwrap();
    let door = Arc::new(Mutex::new(Capture::default()));
    let door_io = door.clone();
    host.attach("door", move |_| {
        Ok(Box::new(adapters::Ble::new(Door(door_io), 1, 15)?))
    })
    .unwrap();
    assert_eq!(host.setup().canvas.points.len(), 192);
    assert_eq!(host.tick(0).unwrap(), 5);
    wait_submitted(&host, 1);
    for (id, s) in [("ram-a", &a), ("ram-b", &b)] {
        let s = s.lock().unwrap();
        for (i, c) in host.frame(id).unwrap().iter().enumerate() {
            for (j, v) in [c.r, c.b, c.g].iter().enumerate() {
                assert_eq!(s.memory[&(0x8100 + (i * 3 + j) as u16)], *v);
            }
        }
        assert_eq!(s.memory[&0x8020], 1);
        assert_eq!(s.memory[&0x80a0], 1);
    }
    let board_colors = host.frame("board").unwrap();
    let order = fusion2::parse_identity(&identity()).unwrap().orders;
    let expected: Vec<Vec<u8>> = fusion2::packets(0, &board_colors[..27], order[0])
        .unwrap()
        .into_iter()
        .chain(fusion2::packets(1, &board_colors[27..], order[1]).unwrap())
        .map(|p| p.to_vec())
        .collect();
    let reports = board.lock().unwrap().reports.clone();
    assert_eq!(&reports[reports.len() - 4..], &expected);
    assert!(
        reports
            .iter()
            .all(|p| matches!(p[1], 0x60 | 0x34 | 0x32 | 0x58 | 0x59))
    );
    assert_eq!(
        keys.lock().unwrap().reports,
        valkyrie::frame_packets(valkyrie::Model::Normal, host.frame("keys").unwrap())
            .unwrap()
            .into_iter()
            .map(|p| p.to_vec())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        door.lock().unwrap().reports,
        [
            door::frame_packet(host.frame("door").unwrap().try_into().unwrap(), 15, 0)
                .unwrap()
                .to_vec()
        ]
    );
    for id in ["ram-a", "ram-b", "board", "keys", "door"] {
        host.mute(id, true).unwrap();
    }
    host.tick(1).unwrap();
    wait_submitted(&host, 2);
    stop(&mut host);
    for state in [&a, &b] {
        let s = state.lock().unwrap();
        assert!(s.closed);
        assert_eq!(s.transactions, 4);
        for reg in 0x8100..0x8118 {
            assert_eq!(s.memory[&reg], 0);
        }
    }
    assert!(board.lock().unwrap().closed);
    assert!(keys.lock().unwrap().closed);
    let key_reports = keys.lock().unwrap().reports.clone();
    let black_keys: Vec<Vec<u8>> =
        valkyrie::frame_packets(valkyrie::Model::Normal, &[Rgb::default(); 102])
            .unwrap()
            .into_iter()
            .map(|p| p.to_vec())
            .collect();
    assert_eq!(&key_reports[key_reports.len() - 10..], &black_keys);
    assert!(door.lock().unwrap().closed);
    assert_eq!(
        door.lock().unwrap().reports.last().unwrap(),
        &door::off_packet(1).to_vec()
    );
    let reports = board.lock().unwrap().reports.clone();
    let expected: Vec<Vec<u8>> = fusion2::packets(0, &[Rgb::default(); 27], order[0])
        .unwrap()
        .into_iter()
        .chain(fusion2::packets(1, &[Rgb::default(); 27], order[1]).unwrap())
        .map(|p| p.to_vec())
        .collect();
    assert_eq!(&reports[reports.len() - 4..], &expected);
}

#[test]
fn portable_native_protocols_and_local_bindings_roundtrip_without_activation() {
    let setup = setup();
    let mut bindings = Bindings::new();
    bindings.insert("door".into(), Binding::Ble { address: 1 });
    for (id, address) in [("ram-a", 0x71), ("ram-b", 0x73)] {
        bindings.insert(
            id.into(),
            Binding::EnePiix4 {
                module_path: r"C:\stand\SmbusPIIX4.bin".into(),
                address,
            },
        );
    }
    bindings.insert(
        "board".into(),
        Binding::Hid {
            path: r"\\?\hid#vid_048d&pid_5702&mi_00#stand#{guid}".into(),
        },
    );
    bindings.insert(
        "keys".into(),
        Binding::Hid {
            path: keyboard_descriptor().path,
        },
    );
    devices::validate_active_bindings(&setup, &bindings).unwrap();
    assert_eq!(
        devices::decode_bindings(
            &setup,
            &devices::encode_bindings(&setup, &bindings).unwrap()
        )
        .unwrap(),
        bindings
    );
    let portable = setup.encode().unwrap();
    assert!(!String::from_utf8_lossy(&portable).contains("SmbusPIIX4.bin"));
    assert_eq!(Setup::decode(&portable).unwrap(), setup);
    let mut bad = bindings.clone();
    bad.insert("ram-b".into(), bad["ram-a"].clone());
    assert!(devices::validate_bindings(&setup, &bad).is_err());
    for path in [
        r"\\server\module.bin",
        r"\\?\GLOBALROOT\Device\PawnIO",
        "relative.bin",
        r"C:\stand\module.bin:stream",
    ] {
        let mut bad = bindings.clone();
        bad.insert(
            "ram-a".into(),
            Binding::EnePiix4 {
                module_path: path.into(),
                address: 0x71,
            },
        );
        assert!(devices::validate_bindings(&setup, &bad).is_err());
    }
    for path in [
        r"\\?\hid#vid_048d&pid_5702&mi_00&mi_01#stand#{guid}",
        r"\\?\hid#vid_048d&pid_5703#stand#{guid}",
    ] {
        let mut bad = bindings.clone();
        bad.insert("board".into(), Binding::Hid { path: path.into() });
        assert!(devices::validate_bindings(&setup, &bad).is_err());
    }
}

#[test]
fn second_header_failure_never_reports_a_complete_frame_or_retries() {
    use pc_control::runtime::output::Phase;
    let mut setup = setup();
    setup.devices.retain(|d| d.id == "board");
    setup.canvas.points.retain(|p| p.device == "board");
    let mut host = Host::new(setup, scene()).unwrap();
    let state = Arc::new(Mutex::new(Capture {
        fail_opcode: Some(0x59),
        ..Capture::default()
    }));
    let io = state.clone();
    host.attach("board", move |_| {
        Ok(Box::new(adapters::Fusion2::new(Fusion(io), [27, 27])?))
    })
    .unwrap();
    host.tick(0).unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while host.status("board").unwrap().phase != Phase::Failed {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(1));
    }
    stop(&mut host);
    let snapshot = host.status("board").unwrap();
    assert_eq!(snapshot.submitted, 0);
    assert!(snapshot.error.unwrap().contains("second-header"));
    let state = state.lock().unwrap();
    assert!(state.closed);
    assert_eq!(state.reports.last().unwrap()[1], 0x59);
    assert_eq!(state.reports.iter().filter(|p| p[1] == 0x58).count(), 2);
    assert_eq!(state.reports.iter().filter(|p| p[1] == 0x59).count(), 1);
}

#[test]
fn native_driver_role_counts_and_protocol_families_are_explicit() {
    for (driver, leds) in [
        (Driver::Fusion2 { headers: [0, 27] }, 27),
        (Driver::Fusion2 { headers: [257, 27] }, 284),
        (Driver::Fusion2 { headers: [27, 27] }, 53),
        (Driver::EneDram, 9),
        (
            Driver::Valkyrie {
                model: "vk99".into(),
            },
            98,
        ),
        (
            Driver::Valkyrie {
                model: "guessed-sonix".into(),
            },
            102,
        ),
    ] {
        assert!(device("bad", driver, leds).validate().is_err());
    }
    for driver in [
        Driver::Fusion2 { headers: [27, 27] },
        Driver::EneDram,
        Driver::Valkyrie {
            model: "vk99".into(),
        },
    ] {
        let mut d = device("bad", driver, 8);
        d.role = Role::Ambient;
        assert!(d.validate().is_err());
    }
    for path in [
        r"\\?\hid#vid_048d&pid_5702#stand#{guid}",
        r"\\?\hid#vid_048d&pid_5702&mi_03&col01#stand#{guid}",
    ] {
        fusion2::validate_path(path).unwrap();
    }
    for address in [0x50, 0x6f, 0x77, 0xff] {
        assert!(
            Binding::EnePiix4 {
                module_path: r"C:\stand\module.bin".into(),
                address
            }
            .validate(&device("ram", Driver::EneDram, 8))
            .is_err()
        );
    }
}
