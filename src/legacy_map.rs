//! Read-only VisualMap CUSTOM geometry import. No hardware identity is copied into Setup.
use crate::devices::{
    Canvas, Device, Driver, MAX_AXIS, MAX_DEVICES, MAX_JSON_BYTES, MAX_POINTS, Point, Role, Setup,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// Explicit, reviewed zone metadata; never infer a native protocol from a display name.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZoneBinding {
    pub leds: usize,
    pub driver: Driver,
    pub rate_hz: u32,
}

fn integer(value: &Value, key: &str) -> Result<u32, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| format!("Missing or invalid unsigned integer: {key}"))
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a Value, String> {
    value
        .get(key)
        .ok_or_else(|| format!("Missing VisualMap field: {key}"))
}

/// Each binding corresponds to one ctrl_zones entry, in the original order.
/// Output is disabled until separately reviewed and bound; coordinates/LED indices are unchanged.
pub fn import_visual_map(input: &str, bindings: &[ZoneBinding]) -> Result<Setup, String> {
    if input.len() > MAX_JSON_BYTES {
        return Err("VisualMap exceeds 4 MiB".into());
    }
    let root: Value = serde_json::from_str(input.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("Invalid VisualMap JSON: {e}"))?;
    let grid = required(&root, "grid_settings")?;
    let (width, height) = (integer(grid, "w")?, integer(grid, "h")?);
    if !(1..=MAX_AXIS).contains(&width) || !(1..=MAX_AXIS).contains(&height) {
        return Err("VisualMap grid exceeds setup bounds".into());
    }
    let zones = required(&root, "ctrl_zones")?
        .as_array()
        .ok_or("ctrl_zones must be an array")?;
    if zones.is_empty() || zones.len() > MAX_DEVICES || zones.len() != bindings.len() {
        return Err("One explicit binding is required for every VisualMap zone".into());
    }
    let mut result = Setup {
        version: 1,
        devices: vec![],
        canvas: Canvas {
            width,
            height,
            points: vec![],
        },
    };
    let mut identities = BTreeSet::new();
    for (index, (zone, binding)) in zones.iter().zip(bindings).enumerate() {
        if binding.leds == 0
            || binding.leds > MAX_POINTS
            || result
                .canvas
                .points
                .len()
                .checked_add(binding.leds)
                .is_none_or(|n| n > MAX_POINTS)
        {
            return Err("VisualMap LED count exceeds setup bounds".into());
        }
        let controller = required(zone, "controller")?;
        let mut identity = Vec::new();
        for field in [
            "name",
            "vendor",
            "description",
            "version",
            "serial",
            "location",
        ] {
            identity.push(
                required(controller, field)?
                    .as_str()
                    .ok_or("Controller identity fields must be strings")?
                    .to_owned(),
            );
        }
        let zone_index = integer(zone, "zone_idx")?;
        // Segments need original controller offsets; without them importing would guess addressing.
        if zone.get("is_segment").is_some_and(|v| v != false)
            || zone
                .get("segment_idx")
                .is_some_and(|v| v.as_i64() != Some(-1))
        {
            return Err(
                "Segment addressing requires unavailable original controller offsets".into(),
            );
        }
        if !identities.insert((identity, zone_index)) {
            return Err("Duplicate legacy controller/zone binding".into());
        }
        if let Some(version) = zone.get("binding_version")
            && version.as_u64() != Some(1)
        {
            return Err("Unsupported VisualMap binding version".into());
        }
        if let Some(expected) = zone.get("expected_led_count")
            && expected.as_u64() != Some(binding.leds as u64)
        {
            return Err("Legacy expected LED count differs from reviewed binding".into());
        }
        let settings = required(zone, "settings")?;
        if integer(settings, "shape")? != 2 || required(settings, "scale")?.as_f64() != Some(1.0) {
            return Err(
                "Only unscaled CUSTOM geometry can be imported without changing LED coverage"
                    .into(),
            );
        }
        let (ox, oy) = (integer(settings, "x")?, integer(settings, "y")?);
        // LedRouting::BuildCells CUSTOM uses led_num directly; reverse and led_spacing do not apply.
        required(settings, "reverse")?
            .as_bool()
            .ok_or("reverse must be boolean")?;
        let shape = required(settings, "custom_shape")?;
        let (sw, sh) = (integer(shape, "w")?, integer(shape, "h")?);
        if sw == 0 || sh == 0 || sw > MAX_AXIS || sh > MAX_AXIS {
            return Err("Invalid custom shape bounds".into());
        }
        let positions = required(shape, "led_positions")?
            .as_array()
            .ok_or("led_positions must be an array")?;
        if positions.len() != binding.leds {
            return Err("Custom shape does not map every reviewed LED".into());
        }
        let id = format!("canvas-{:02}", index + 1);
        let mut seen = BTreeSet::new();
        for position in positions {
            let led = integer(position, "led_num")? as usize;
            let (x, y) = (integer(position, "x")?, integer(position, "y")?);
            if led >= binding.leds || !seen.insert(led) || x >= sw || y >= sh {
                return Err("Invalid, missing or duplicate custom LED index/coordinate".into());
            }
            let x = x.checked_add(ox).ok_or("VisualMap x overflow")?;
            let y = y.checked_add(oy).ok_or("VisualMap y overflow")?;
            if x >= width || y >= height {
                return Err("Custom LED lies outside canvas".into());
            }
            result.canvas.points.push(Point {
                device: id.clone(),
                led,
                x,
                y,
            });
        }
        result.devices.push(Device {
            id,
            label: format!("Canvas channel {}", index + 1),
            role: Role::Canvas,
            driver: binding.driver.clone(),
            leds: binding.leds,
            rate_hz: binding.rate_hz,
            enabled: false,
        });
    }
    result.validate()?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn source() -> Value {
        json!({"grid_settings":{"w":140,"h":80},"ctrl_zones":[{"controller":{"name":"test","vendor":"test","description":"test","version":"1","serial":"PRIVATE","location":"PRIVATE"},"zone_idx":0,"settings":{"shape":2,"x":0,"y":0,"scale":1.0,"reverse":false,"custom_shape":{"w":140,"h":80,"led_positions":[{"led_num":1,"x":134,"y":8},{"led_num":0,"x":102,"y":28}]}}}]})
    }
    fn binding() -> Vec<ZoneBinding> {
        vec![ZoneBinding {
            leds: 2,
            driver: Driver::Unsupported {
                protocol: "legacy-openrgb".into(),
            },
            rate_hz: 25,
        }]
    }
    fn import(v: &Value) -> Result<Setup, String> {
        import_visual_map(&v.to_string(), &binding())
    }
    #[test]
    fn preserves_array_order_indices_coordinates_and_hides_identity() {
        let s = import(&source()).unwrap();
        assert_eq!((s.canvas.width, s.canvas.height), (140, 80));
        assert_eq!(
            (
                s.canvas.points[0].led,
                s.canvas.points[0].x,
                s.canvas.points[0].y
            ),
            (1, 134, 8)
        );
        assert_eq!(
            (
                s.canvas.points[1].led,
                s.canvas.points[1].x,
                s.canvas.points[1].y
            ),
            (0, 102, 28)
        );
        assert_eq!(s.devices[0].id, "canvas-01");
        assert!(!s.devices[0].enabled);
        assert!(!serde_json::to_string(&s).unwrap().contains("PRIVATE"));
    }
    #[test]
    fn rejects_ambiguous_or_incomplete_led_addressing() {
        for value in [json!(1), json!(2), json!(-1), json!(0.5), json!("0")] {
            let mut v = source();
            v["ctrl_zones"][0]["settings"]["custom_shape"]["led_positions"][1]["led_num"] = value;
            assert!(import(&v).is_err());
        }
        let mut v = source();
        v["ctrl_zones"][0]["settings"]["custom_shape"]["led_positions"]
            .as_array_mut()
            .unwrap()
            .pop();
        assert!(import(&v).is_err());
        let mut v = source();
        let duplicate = v["ctrl_zones"][0].clone();
        v["ctrl_zones"].as_array_mut().unwrap().push(duplicate);
        let mut b = binding();
        b.extend(binding());
        assert!(import_visual_map(&v.to_string(), &b).is_err());
    }
    #[test]
    fn rejects_unrepresentable_geometry_and_unknown_segments() {
        for field in ["shape", "scale"] {
            let mut v = source();
            v["ctrl_zones"][0]["settings"][field] = json!(3);
            assert!(import(&v).is_err());
        }
        let mut v = source();
        v["ctrl_zones"][0]["is_segment"] = json!(true);
        assert!(import(&v).is_err());
        let mut v = source();
        v["ctrl_zones"][0]["settings"]["x"] = json!(u32::MAX);
        assert!(import(&v).is_err());
        let mut v = source();
        v["ctrl_zones"][0]["settings"]["custom_shape"]["led_positions"][0]["x"] = json!(140);
        assert!(import(&v).is_err());
        let mut v = source();
        v["ctrl_zones"][0]["expected_led_count"] = json!(3);
        assert!(import(&v).is_err());
    }
    #[test]
    fn custom_reverse_does_not_reverse_addresses_and_offsets_are_exact() {
        let mut v = source();
        v["ctrl_zones"][0]["settings"]["reverse"] = json!(true);
        v["grid_settings"]["w"] = json!(200);
        v["ctrl_zones"][0]["settings"]["x"] = json!(2);
        let s = import(&v).unwrap();
        assert_eq!((s.canvas.points[0].led, s.canvas.points[0].x), (1, 136));
        assert!(import_visual_map(&v.to_string(), &[]).is_err());
        assert!(import_visual_map("{}", &binding()).is_err());
    }
}
