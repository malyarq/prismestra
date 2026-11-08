//! Explicit, immutable logical migration; hardware bindings are deliberately outside this API.
use crate::devices::{Device, Driver, Role, Setup};

/// Merge two explicitly selected disabled canvas devices into Fusion2 header0/header1.
/// Argument order defines headers; original device order only defines the insertion position.
/// Every point retains its position in the vector and its world coordinates.
pub fn merge_fusion2(
    setup: &Setup,
    header0: &str,
    header1: &str,
    target_id: &str,
    label: &str,
    rate_hz: u32,
) -> Result<Setup, String> {
    setup.validate()?;
    if header0 == header1 {
        return Err("Fusion2 headers require two distinct source device IDs".into());
    }
    let first = setup
        .devices
        .iter()
        .position(|d| d.id == header0)
        .ok_or("Fusion2 header0 source device is missing")?;
    let second = setup
        .devices
        .iter()
        .position(|d| d.id == header1)
        .ok_or("Fusion2 header1 source device is missing")?;
    let a = &setup.devices[first];
    let b = &setup.devices[second];
    if [a, b]
        .iter()
        .any(|d| d.enabled || d.role != Role::Canvas || d.leds == 0)
    {
        return Err("Fusion2 sources must be disabled, nonempty canvas devices".into());
    }
    if setup
        .devices
        .iter()
        .any(|d| d.id == target_id && d.id != header0 && d.id != header1)
    {
        return Err("Fusion2 target ID belongs to another device".into());
    }
    let leds = a
        .leds
        .checked_add(b.leds)
        .ok_or("Fusion2 LED count overflow")?;
    let merged = Device {
        id: target_id.to_owned(),
        label: label.to_owned(),
        role: Role::Canvas,
        driver: Driver::Fusion2 {
            headers: [a.leds, b.leds],
        },
        leds,
        rate_hz,
        enabled: false,
    };
    merged.validate()?;
    let mut result = setup.clone();
    result.devices.clear();
    let insertion = first.min(second);
    for (index, device) in setup.devices.iter().enumerate() {
        if index == insertion {
            result.devices.push(merged.clone());
        }
        if device.id != header0 && device.id != header1 {
            result.devices.push(device.clone());
        }
    }
    for point in &mut result.canvas.points {
        if point.device == header0 {
            point.device = target_id.to_owned();
        } else if point.device == header1 {
            point.device = target_id.to_owned();
            point.led = point
                .led
                .checked_add(a.leds)
                .ok_or("Fusion2 LED index overflow")?;
        }
    }
    result.validate()?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::{Canvas, Point};

    fn device(id: &str, leds: usize) -> Device {
        Device {
            id: id.into(),
            label: id.into(),
            role: Role::Canvas,
            driver: Driver::Unsupported {
                protocol: "synthetic".into(),
            },
            leds,
            rate_hz: 25,
            enabled: false,
        }
    }
    fn point(device: &str, led: usize, x: u32, y: u32) -> Point {
        Point {
            device: device.into(),
            led,
            x,
            y,
        }
    }
    fn fixture() -> Setup {
        let mut ambient = device("ambient", 0);
        ambient.role = Role::Ambient;
        let mut lcd = device("lcd", 0);
        lcd.role = Role::Lcd;
        lcd.driver = Driver::TtLcd;
        lcd.enabled = true;
        let s = Setup {
            version: 1,
            devices: vec![
                ambient,
                device("b", 3),
                device("other", 1),
                lcd,
                device("a", 2),
            ],
            canvas: Canvas {
                width: 20,
                height: 20,
                points: vec![
                    point("b", 2, 7, 8),
                    point("other", 0, 2, 4),
                    point("a", 1, 1, 3),
                    point("b", 0, 6, 9),
                    point("a", 0, 1, 2),
                    point("b", 1, 8, 9),
                ],
            },
        };
        s.validate().unwrap();
        s
    }
    fn unchanged_error(s: &Setup, a: &str, b: &str, id: &str, label: &str, rate: u32) {
        let before = s.clone();
        assert!(merge_fusion2(s, a, b, id, label, rate).is_err());
        assert_eq!(*s, before);
    }

    #[test]
    fn nonadjacent_sources_preserve_geometry_order_outputs_and_argument_header_order() {
        let original = fixture();
        let before = original.clone();
        let result = merge_fusion2(&original, "a", "b", "combined", "Two headers", 30).unwrap();
        assert_eq!(original, before);
        assert_eq!(
            result
                .devices
                .iter()
                .map(|d| d.id.as_str())
                .collect::<Vec<_>>(),
            ["ambient", "combined", "other", "lcd"]
        );
        assert_eq!(result.devices[0], original.devices[0]);
        assert_eq!(result.devices[2], original.devices[2]);
        assert_eq!(result.devices[3], original.devices[3]);
        let d = &result.devices[1];
        assert_eq!(d.driver, Driver::Fusion2 { headers: [2, 3] });
        assert_eq!((d.leds, d.rate_hz, d.enabled), (5, 30, false));
        assert_eq!(d.label, "Two headers");
        assert_eq!(
            (result.version, result.canvas.width, result.canvas.height),
            (1, 20, 20)
        );
        assert_eq!(result.canvas.points.len(), original.canvas.points.len());
        for (old, new) in original.canvas.points.iter().zip(&result.canvas.points) {
            assert_eq!((old.x, old.y), (new.x, new.y));
            match old.device.as_str() {
                "a" => assert_eq!((new.device.as_str(), new.led), ("combined", old.led)),
                "b" => assert_eq!((new.device.as_str(), new.led), ("combined", old.led + 2)),
                _ => assert_eq!(new, old),
            }
        }
        result.validate().unwrap();
        let reverse =
            merge_fusion2(&original, "b", "a", "combined", "Reversed arguments", 25).unwrap();
        assert_eq!(
            reverse.devices[1].driver,
            Driver::Fusion2 { headers: [3, 2] }
        );
        for (old, new) in original.canvas.points.iter().zip(&reverse.canvas.points) {
            if old.device == "a" {
                assert_eq!(new.led, old.led + 3);
            } else {
                assert_eq!(new.led, old.led);
            }
        }
    }

    #[test]
    fn either_source_id_can_be_reused_but_a_third_id_cannot() {
        let s = fixture();
        for id in ["a", "b"] {
            let result = merge_fusion2(&s, "a", "b", id, "Combined", 25).unwrap();
            assert_eq!(result.devices[1].id, id);
            assert_eq!(
                result
                    .canvas
                    .points
                    .iter()
                    .filter(|p| p.device == id)
                    .count(),
                5
            );
        }
        unchanged_error(&s, "a", "b", "other", "Combined", 25);
        unchanged_error(&s, "a", "b", "lcd", "Combined", 25);
    }

    #[test]
    fn rejects_invalid_ids_source_states_metadata_and_invalid_input_without_mutation() {
        let s = fixture();
        for (a, b) in [("a", "a"), ("missing", "b"), ("a", "missing"), ("", "b")] {
            unchanged_error(&s, a, b, "combined", "Combined", 25);
        }
        for id in ["", "bad id", "bad/id"] {
            unchanged_error(&s, "a", "b", id, "Combined", 25);
        }
        for label in ["", " ", "bad\nlabel"] {
            unchanged_error(&s, "a", "b", "combined", label, 25);
        }
        for rate in [0, 145] {
            unchanged_error(&s, "a", "b", "combined", "Combined", rate);
        }
        let mut active = s.clone();
        active.devices[4].driver = Driver::Fusion2 { headers: [1, 1] };
        active.devices[4].enabled = true;
        active.validate().unwrap();
        unchanged_error(&active, "a", "b", "combined", "Combined", 25);
        let mut ambient = s.clone();
        ambient.devices[4].role = Role::Ambient;
        ambient.canvas.points.retain(|p| p.device != "a");
        ambient.validate().unwrap();
        unchanged_error(&ambient, "a", "b", "combined", "Combined", 25);
        let mut invalid = s.clone();
        invalid.canvas.points[0].led = 99;
        unchanged_error(&invalid, "a", "b", "combined", "Combined", 25);
    }

    #[test]
    fn header_count_bounds_are_validated_without_truncation() {
        for (count, valid) in [(0, false), (1, true), (256, true), (257, false)] {
            let mut s = Setup {
                version: 1,
                devices: vec![device("a", count), device("b", 1)],
                canvas: Canvas {
                    width: 512,
                    height: 1,
                    points: vec![],
                },
            };
            s.canvas
                .points
                .extend((0..count).map(|led| point("a", led, led as u32, 0)));
            s.canvas.points.push(point("b", 0, 1, 0));
            s.validate().unwrap();
            let before = s.clone();
            let result = merge_fusion2(&s, "a", "b", "combined", "Combined", 25);
            assert_eq!(result.is_ok(), valid);
            assert_eq!(s, before);
            if let Ok(result) = result {
                assert_eq!(result.devices[0].leds, count + 1);
                assert_eq!(result.canvas.points.last().unwrap().led, count);
            }
        }
    }
}
