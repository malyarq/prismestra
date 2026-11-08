//! Private controller identity: SDK numeric IDs are only live lookup hints.
use crate::transport::openrgb::ControllerInfo;
use serde_json::json;
use sha2::{Digest, Sha256};

fn digest(value: serde_json::Value) -> String {
    Sha256::digest(serde_json::to_vec(&value).expect("bounded controller metadata"))
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect()
}
/// Physical ownership deliberately excludes mutable topology and SDK ordering.
pub fn identity(controller: &ControllerInfo) -> String {
    digest(json!([
        controller.name,
        controller.vendor,
        controller.serial,
        controller.location,
        controller.kind
    ]))
}
pub fn topology(controller: &ControllerInfo) -> String {
    let zones = controller
        .zones
        .iter()
        .map(|z| {
            json!([
                z.id,
                z.name,
                z.kind,
                z.leds,
                z.reported_leds,
                z.led_offset,
                z.matrix
                    .as_ref()
                    .map(|m| json!([m.width, m.height, m.led_indices])),
                z.reported_matrix.as_ref().map(|m| json!([
                    m.width,
                    m.height,
                    m.zone_local_indices
                ]))
            ])
        })
        .collect::<Vec<_>>();
    digest(json!([
        controller.leds,
        controller.led_names,
        zones,
        controller.supports_direct
    ]))
}
pub fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// One complete controller; unmapped/virtual LEDs get an explicit fallback row.
/// Coordinates are local grid cells, ready to scale into the portable canvas.
pub fn positions(controller: &ControllerInfo) -> Result<Vec<(usize, usize)>, String> {
    if controller.leds == 0 || controller.leds > crate::transport::openrgb::MAX_LEDS {
        return Err("OpenRGB LED count exceeds geometry limits".into());
    }
    let mut points = vec![None; controller.leds];
    let mut top = 0usize;
    for zone in &controller.zones {
        if let Some(matrix) = &zone.matrix {
            if matrix.width == 0
                || matrix.height == 0
                || matrix.width.checked_mul(matrix.height) != Some(matrix.led_indices.len())
            {
                return Err("Invalid OpenRGB matrix size".into());
            }
            for (cell, index) in matrix.led_indices.iter().enumerate() {
                if let Some(index) = index {
                    let slot = points
                        .get_mut(*index)
                        .ok_or("OpenRGB matrix index exceeds controller")?;
                    if slot
                        .replace((cell % matrix.width, top + cell / matrix.width))
                        .is_some()
                    {
                        return Err("Duplicate OpenRGB matrix LED".into());
                    }
                }
            }
            top = top
                .checked_add(matrix.height + 1)
                .ok_or("OpenRGB geometry overflow")?;
        }
    }
    let mut orphan = 0;
    for point in &mut points {
        if point.is_none() {
            *point = Some((orphan, top));
            orphan += 1;
        }
    }
    Ok(points.into_iter().map(Option::unwrap).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::openrgb::{MatrixInfo, ZoneInfo};
    fn controller() -> ControllerInfo {
        ControllerInfo {
            id: 1,
            name: "controller".into(),
            vendor: "vendor".into(),
            serial: "serial".into(),
            location: "usb-location".into(),
            kind: "keyboard".into(),
            leds: 3,
            led_names: vec!["a".into(), "b".into(), "c".into()],
            supports_direct: true,
            zones: vec![ZoneInfo {
                id: 0,
                name: "keys".into(),
                kind: "matrix".into(),
                leds: 3,
                reported_leds: 3,
                led_offset: 0,
                matrix: Some(MatrixInfo {
                    width: 2,
                    height: 2,
                    led_indices: vec![Some(2), None, Some(0), None],
                }),
                reported_matrix: None,
                matrix_warning: None,
            }],
        }
    }
    #[test]
    fn numeric_ids_do_not_change_ownership_but_identity_and_topology_changes_are_detected() {
        let original = controller();
        let mut changed = original.clone();
        changed.id = 9;
        assert_eq!(identity(&original), identity(&changed));
        assert_eq!(topology(&original), topology(&changed));
        changed.led_names[0] = "replacement".into();
        assert_eq!(identity(&original), identity(&changed));
        assert_ne!(topology(&original), topology(&changed));
        changed.serial = "different".into();
        assert_ne!(identity(&original), identity(&changed));
        assert!(valid_hash(&identity(&original)));
        assert!(!valid_hash(&"A".repeat(64)));
    }
    #[test]
    fn matrix_holes_and_unmapped_leds_keep_exact_controller_indices() {
        let mut c = controller();
        assert_eq!(positions(&c).unwrap(), vec![(0, 1), (0, 3), (0, 0)]);
        c.zones[0].matrix.as_mut().unwrap().led_indices[1] = Some(2);
        assert!(positions(&c).is_err());
        c.zones[0].matrix = None;
        assert_eq!(positions(&c).unwrap(), vec![(0, 0), (1, 0), (2, 0)]);
    }
}
