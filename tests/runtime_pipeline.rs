use pc_control::{
    devices::{Canvas, Device, Driver, Point, Role, Setup},
    lighting::{color::Rgb, door, scene::Effect},
    runtime::{
        Host, adapters,
        output::Cancellation,
        scene::{NoisePalette, NoiseSettings, Scene},
    },
    transport::ble,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
struct Link {
    packets: mpsc::Sender<Vec<u8>>,
    closed: Arc<AtomicBool>,
}
impl ble::Link for Link {
    fn address(&self) -> Result<u64, String> {
        Ok(1)
    }
    fn connected(&self) -> Result<bool, String> {
        Ok(true)
    }
    fn max_pdu(&self) -> Result<u32, String> {
        Ok(98)
    }
    fn write(&mut self, bytes: &[u8], control: &mut dyn ble::Control) -> Result<(), String> {
        if control.cancelled() {
            return Err("Cancelled fake link".into());
        }
        assert!(ble::valid_packet(bytes));
        self.packets.send(bytes.to_vec()).map_err(|e| e.to_string())
    }
}
impl Drop for Link {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
    }
}
fn scene() -> Scene {
    Scene {
        effect: Effect::NoiseMap,
        colors: vec![0x441100, 0xff8800],
        brightness: 60,
        speed: 8,
        fps: 48,
        temperature: 0,
        tint: 0,
        noise: NoiseSettings {
            frequency: 0.055,
            amplitude: 1.0,
            lacunarity: 0.75,
            persistence: 0.4,
            octaves: 2,
            motion: 0,
            motion_speed: 1,
        },
        noise_palette: NoisePalette::Custom,
        rainbow_width: 10,
    }
}
#[test]
fn shared_scene_routes_to_real_door_codec_using_memory_only_link() {
    let setup = Setup {
        version: 1,
        devices: vec![Device {
            id: "door".into(),
            label: "Door".into(),
            role: Role::Canvas,
            driver: Driver::DoorBle,
            leds: 20,
            rate_hz: 10,
            enabled: true,
        }],
        canvas: Canvas {
            width: 140,
            height: 80,
            points: (0..20)
                .map(|i| Point {
                    device: "door".into(),
                    led: i,
                    x: 100 + i as u32,
                    y: 60,
                })
                .collect(),
        },
    };
    let mut host = Host::new(setup, scene()).unwrap();
    let (tx, rx) = mpsc::channel();
    let closed = Arc::new(AtomicBool::new(false));
    let observed = closed.clone();
    host.attach("door", move |_: Cancellation| {
        Ok(Box::new(adapters::Ble::new(
            Link {
                packets: tx,
                closed: observed,
            },
            1,
            15,
        )?))
    })
    .unwrap();
    host.tick(0).unwrap();
    let packet = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let colors: &[Rgb; 20] = host.frame("door").unwrap().try_into().unwrap();
    assert_eq!(packet, door::frame_packet(colors, 15, 0).unwrap());
    host.mute("door", true).unwrap();
    host.tick(1).unwrap();
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        door::off_packet(1)
    );
    host.stop();
    let until = Instant::now() + Duration::from_secs(2);
    while !host.drained() && Instant::now() < until {
        std::thread::yield_now();
    }
    assert!(host.drained());
    assert!(closed.load(Ordering::Acquire));
}
#[test]
fn native_scene_import_preserves_speed_and_packed_palette() {
    let native = serde_json::json!({"EffectClassName":"NoiseMap","FPS":25,"Speed":8,"Brightness":60,"Temperature":0,"Tint":0,"Slider2Val":1,
        "CustomSettings":{"frequency":0.055,"amplitude":1.0,"lacunarity":0.75,"persistence":0.4,"octaves":2,"motion":0,"motion_speed":1,"colors_choice":2,"colors":[274,1341695]}});
    let imported = Scene::from_native_noise(&native).unwrap();
    assert_eq!(imported.speed, 8);
    assert_eq!(imported.fps, 25);
    assert_eq!(imported.colors, [0x120100, 0xff7814]);
    let serialized = serde_json::to_vec(&imported).unwrap();
    assert_eq!(
        serde_json::from_slice::<Scene>(&serialized).unwrap(),
        imported
    );
}
