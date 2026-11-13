use pc_control::devices::*;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

fn setup() -> Setup {
    let devices = vec![
        Device {
            id: "screen".into(),
            label: "Экран".into(),
            role: Role::Canvas,
            driver: Driver::SkydimoSerial {
                model: "SK0132".into(),
            },
            leds: 77,
            rate_hz: 60,
            enabled: true,
        },
        Device {
            id: "door".into(),
            label: "Дверь".into(),
            role: Role::Canvas,
            driver: Driver::DoorBle,
            leds: 20,
            rate_hz: 30,
            enabled: false,
        },
        Device {
            id: "ambient".into(),
            label: "Ambient".into(),
            role: Role::Ambient,
            driver: Driver::SkydimoSerial {
                model: "SK0410".into(),
            },
            leds: 150,
            rate_hz: 144,
            enabled: false,
        },
        Device {
            id: "lcd".into(),
            label: "LCD".into(),
            role: Role::Lcd,
            driver: Driver::TtLcd,
            leds: 0,
            rate_hz: 48,
            enabled: true,
        },
    ];
    let points = devices
        .iter()
        .filter(|d| d.role == Role::Canvas)
        .flat_map(|d| {
            (0..d.leds).map(move |led| Point {
                device: d.id.clone(),
                led,
                x: led as u32,
                y: 0,
            })
        })
        .collect();
    Setup {
        version: 1,
        devices,
        canvas: Canvas {
            width: 100,
            height: 10,
            points,
        },
    }
}

#[test]
fn door_pixel_preset_is_explicit_and_preserves_legacy_profiles() {
    let pixels = pc_control::app::device_setup::preset("door153")
        .unwrap()
        .device("strip")
        .unwrap();
    assert_eq!(pixels.leds, 153);
    assert_eq!(pixels.rate_hz, 10);
    assert!(!pixels.enabled);
    assert_eq!(pixels.driver, Driver::DoorBle);
    pixels.validate().unwrap();
    let legacy = pc_control::app::device_setup::preset("door20")
        .unwrap()
        .device("strip")
        .unwrap();
    assert_eq!(legacy.leds, 20);
    legacy.validate().unwrap();
    let mut invalid = pixels;
    invalid.leds = 152;
    assert!(invalid.validate().is_err());
    invalid.leds = 153;
    invalid.role = Role::Ambient;
    assert!(invalid.validate().is_err());
}
fn bindings() -> Bindings {
    BTreeMap::from([
        (
            "screen".into(),
            Binding::Serial {
                port: "COM123".into(),
                instance_id: r"USB\VID_1A86&PID_7523\SYNTHETIC_PRIVATE".into(),
                serial: "00A1FF".into(),
            },
        ),
        (
            "door".into(),
            Binding::Ble {
                address: 0x123456789abc,
            },
        ),
        (
            "lcd".into(),
            Binding::Hid {
                path: r"\\?\hid#vid_264a&pid_233c&mi_01#SYNTHETIC_PRIVATE#{fake}".into(),
            },
        ),
    ])
}
#[test]
fn roundtrip_and_explicit_privacy_boundary() {
    let s = setup();
    let b = bindings();
    s.validate().unwrap();
    validate_active_bindings(&s, &b).unwrap();
    let json = s.encode().unwrap();
    assert_eq!(Setup::decode(&json).unwrap(), s);
    let text = String::from_utf8(json).unwrap();
    for private in [
        "COM123",
        "SYNTHETIC_PRIVATE",
        "00A1FF",
        "123456789abc",
        "instance_id",
        "address",
        "path",
        "bindings",
    ] {
        assert!(!text.contains(private), "{private}");
    }
    assert_eq!(
        decode_bindings(&s, &encode_bindings(&s, &b).unwrap()).unwrap(),
        b
    );
    validate_bindings(&s, &Bindings::new()).unwrap();
    assert!(validate_active_bindings(&s, &Bindings::new()).is_err());
}
#[test]
fn geometry_rejects_partial_duplicate_unknown_and_wrong_role() {
    for case in 0..10 {
        let mut s = setup();
        match case {
            0 => {
                s.canvas.points.pop();
            }
            1 => s.canvas.points[0] = s.canvas.points[1].clone(),
            2 => s.canvas.points[0].device = "absent".into(),
            3 => s.canvas.points[0].device = "ambient".into(),
            4 => s.canvas.points[0].led = 77,
            5 => s.canvas.points[0].x = 100,
            6 => s.canvas.points[0].y = 10,
            7 => s.devices[1].id = "screen".into(),
            8 => s.canvas.width = 0,
            _ => s.canvas.height = 8193,
        }
        assert!(s.validate().is_err(), "case {case}");
    }
    let mut s = setup();
    s.devices[0].enabled = false;
    s.validate().unwrap();
    // Coordinates may coincide; destinations remain unique.
    for point in &mut s.canvas.points {
        point.x = 0;
        point.y = 0;
    }
    s.validate().unwrap();
}
#[test]
fn capabilities_bounds_and_unsupported_never_active() {
    let original = setup();
    for case in 0..15 {
        let mut s = original.clone();
        match case {
            0 => s.version = 2,
            1 => s.devices[0].rate_hz = 0,
            2 => s.devices[0].rate_hz = 145,
            3 => s.devices[0].id = "a".repeat(65),
            4 => s.devices[0].id = "устройство".into(),
            5 => s.devices[0].label = "a".repeat(129),
            6 => {
                s.devices[0].driver = Driver::SkydimoSerial {
                    model: "SKZZZZ".into(),
                }
            }
            7 => s.devices[0].leds = 76,
            8 => s.devices[1].role = Role::Ambient,
            9 => s.devices[1].leds = 19,
            10 => s.devices[2].leds = 301,
            11 => s.devices[2].leds = 0,
            12 => s.devices[3].leds = 1,
            13 => s.devices[3].role = Role::Canvas,
            _ => {
                s.devices[0].driver = Driver::Unsupported {
                    protocol: "unknown".into(),
                }
            }
        }
        assert!(s.validate().is_err(), "case {case}");
    }
    let mut s = original.clone();
    s.devices[0].enabled = false;
    s.devices[0].driver = Driver::Unsupported {
        protocol: "explicit unknown".into(),
    };
    s.validate().unwrap();
    assert!(bindings()["screen"].validate(&s.devices[0]).is_err());
    let mut excessive = original.clone();
    excessive.devices = vec![original.devices[3].clone(); 257];
    assert!(excessive.validate().is_err());
    let mut excessive = original;
    excessive.canvas.points = vec![excessive.canvas.points[0].clone(); 65537];
    assert!(excessive.validate().is_err());
}
#[test]
fn exact_binding_policy_and_duplicate_hardware_rejection() {
    let s = setup();
    for case in 0..9 {
        let mut b = bindings();
        match case {
            0 => {
                b.insert("screen".into(), Binding::Ble { address: 1 });
            }
            1 => {
                b.insert("unknown".into(), Binding::Ble { address: 1 });
            }
            2 => {
                if let Binding::Serial { port, .. } = b.get_mut("screen").unwrap() {
                    *port = "COM0123".into();
                }
            }
            3 => {
                if let Binding::Serial { instance_id, .. } = b.get_mut("screen").unwrap() {
                    *instance_id = r"USB\VID_0000&PID_7523\test".into();
                }
            }
            4 => {
                if let Binding::Serial { serial, .. } = b.get_mut("screen").unwrap() {
                    *serial = "a1".into();
                }
            }
            5 => {
                b.insert("door".into(), Binding::Ble { address: 0 });
            }
            6 => {
                b.insert(
                    "door".into(),
                    Binding::Ble {
                        address: 0xffffffffffff,
                    },
                );
            }
            7 => {
                b.insert(
                    "lcd".into(),
                    Binding::Hid {
                        path: r"\\?\hid#vid_264a&pid_233c&mi_00#fake#{fake}".into(),
                    },
                );
            }
            _ => {
                b.insert(
                    "lcd".into(),
                    Binding::Hid {
                        path: "C:/fake.dll".into(),
                    },
                );
            }
        }
        assert!(validate_bindings(&s, &b).is_err(), "case {case}");
    }
    let mut s = s;
    let mut b = bindings();
    s.devices[2].leds = 77;
    s.devices[2].driver = s.devices[0].driver.clone();
    b.insert("ambient".into(), b["screen"].clone());
    assert!(validate_bindings(&s, &b).is_err());
}
#[test]
fn strict_json_and_transactional_replace() {
    let mut s = setup();
    let before = s.clone();
    let mut bad = s.clone();
    bad.canvas.points.pop();
    assert!(s.replace(bad).is_err());
    assert_eq!(s, before);
    let mut good = s.clone();
    good.devices[0].label = "Changed".into();
    s.replace(good.clone()).unwrap();
    assert_eq!(s, good);
    let mut json = serde_json::to_value(&before).unwrap();
    json["bindings"] = serde_json::json!({});
    assert!(Setup::decode(&serde_json::to_vec(&json).unwrap()).is_err());
    let mut json = serde_json::to_value(&before).unwrap();
    json["devices"][0]["driver"]["port"] = "COM123".into();
    assert!(Setup::decode(&serde_json::to_vec(&json).unwrap()).is_err());
    assert!(Setup::decode(&vec![b' '; MAX_JSON_BYTES + 1]).is_err());
    assert!(
        decode_bindings(
            &before,
            br#"{"lcd":{"type":"hid","path":"fake","extra":1}}"#
        )
        .is_err()
    );
    assert!(
        decode_bindings(
            &before,
            br#"{"door":{"type":"ble","address":1},"door":{"type":"ble","address":2}}"#
        )
        .is_err()
    );
}
#[test]
fn inclusive_geometry_and_definition_bounds() {
    let mut s = setup();
    s.canvas.width = MAX_AXIS;
    s.canvas.height = MAX_AXIS;
    s.devices[0].label = "я".repeat(128);
    s.devices[0].id = "a".repeat(64);
    for p in &mut s.canvas.points {
        if p.device == "screen" {
            p.device = s.devices[0].id.clone();
        }
        p.x = MAX_AXIS - 1;
        p.y = MAX_AXIS - 1;
    }
    s.validate().unwrap();
    let unknown = Device {
        id: "unknown".into(),
        label: "Unimplemented geometry".into(),
        role: Role::Canvas,
        driver: Driver::Unsupported {
            protocol: "unsupported".into(),
        },
        leds: MAX_POINTS,
        rate_hz: 1,
        enabled: false,
    };
    let mut s = Setup {
        version: 1,
        devices: vec![unknown],
        canvas: Canvas {
            width: 1,
            height: 1,
            points: (0..MAX_POINTS)
                .map(|led| Point {
                    device: "unknown".into(),
                    led,
                    x: 0,
                    y: 0,
                })
                .collect(),
        },
    };
    s.validate().unwrap();
    s.devices = (0..MAX_DEVICES)
        .map(|i| Device {
            id: format!("lcd-{i}"),
            label: "LCD".into(),
            role: Role::Lcd,
            driver: Driver::TtLcd,
            leds: 0,
            rate_hz: 1,
            enabled: false,
        })
        .collect();
    s.canvas.points.clear();
    s.validate().unwrap();
}
#[test]
fn atomic_save_reload_and_failed_proposal_preserves_file() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("work")
        .join("devices-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("setup.json");
    let mut s = setup();
    s.save(&path).unwrap();
    assert_eq!(Setup::load(&path).unwrap(), s);
    s.devices[0].label = "Replacement".into();
    s.save(&path).unwrap();
    assert_eq!(Setup::load(&path).unwrap(), s);
    let bytes = std::fs::read(&path).unwrap();
    s.canvas.points.pop();
    assert!(s.save(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(dir).unwrap();
}
