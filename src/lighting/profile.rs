// SPDX-License-Identifier: GPL-2.0-or-later
//! Pure JSON composition/validation from SceneEffects.cpp; never applies a scene.
use super::{
    color::Rgb,
    scene::{Controls, Effect},
};
use serde_json::{Value, json};
pub type ProfileResult<T> = Result<T, String>;
const MAX_BYTES: usize = 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SceneIndices {
    pub main: usize,
    pub ambient: Option<usize>,
}
fn fail(message: &str) -> String {
    message.to_owned()
}
fn field<'a>(v: &'a Value, key: &str) -> ProfileResult<&'a Value> {
    v.get(key).ok_or_else(|| format!("Missing {key}"))
}
fn string(v: &Value) -> ProfileResult<&str> {
    v.as_str().ok_or_else(|| fail("Expected string"))
}
fn string_or<'a>(v: &'a Value, key: &str, default: &'a str) -> ProfileResult<&'a str> {
    match v.get(key) {
        Some(value) => string(value),
        None => Ok(default),
    }
}
fn boolean_or(v: &Value, key: &str, default: bool) -> ProfileResult<bool> {
    match v.get(key) {
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("Invalid boolean {key}")),
        None => Ok(default),
    }
}
fn integer(v: &Value, min: i64, max: i64, label: &str) -> ProfileResult<i32> {
    let n = v.as_i64().ok_or_else(|| format!("Invalid {label}"))?;
    if n < min || n > max {
        return Err(format!("Invalid {label}"));
    }
    Ok(n as i32)
}
fn integer_or(v: &Value, key: &str, min: i64, max: i64, default: i32) -> ProfileResult<i32> {
    match v.get(key) {
        Some(x) => integer(x, min, max, key),
        None => Ok(default),
    }
}
fn bounded(v: &Value) -> ProfileResult<()> {
    if serde_json::to_vec(v).map_err(|e| e.to_string())?.len() > MAX_BYTES {
        Err(fail("Oversized scene profile"))
    } else {
        Ok(())
    }
}
fn effect_from_name(name: &str) -> ProfileResult<Effect> {
    match name {
        "NoiseMap" => Ok(Effect::NoiseMap),
        "Breathing" => Ok(Effect::Breathing),
        "RainbowWave" => Ok(Effect::RainbowWave),
        _ => Err(fail("Unsupported scene effect class")),
    }
}
fn effect_name(effect: Effect) -> &'static str {
    match effect {
        Effect::NoiseMap => "NoiseMap",
        Effect::Breathing => "Breathing",
        Effect::RainbowWave => "RainbowWave",
    }
}
/// Native indices: exactly one explicit main, legacy main, or sole eligible effect.
pub fn indices(profile: &Value) -> ProfileResult<SceneIndices> {
    bounded(profile)?;
    let effects = profile
        .as_object()
        .and_then(|p| p.get("Effects"))
        .and_then(Value::as_array)
        .filter(|e| e.len() <= 32)
        .ok_or_else(|| fail("Invalid/bounded Effects profile required"))?;
    let (mut main, mut ambient) = (None, None);
    let mut ordinary = Vec::new();
    for (i, effect) in effects.iter().enumerate() {
        if !effect.is_object() {
            return Err(fail("Invalid effect record"));
        }
        let kind = string_or(effect, "EffectClassName", "")?;
        if kind == "Ambient" {
            if ambient.replace(i).is_some() {
                return Err(fail("Ambiguous Ambient effect"));
            }
            continue;
        }
        if effect_from_name(kind).is_err() {
            continue;
        }
        let Some(zones) = effect
            .get("ControllerZones")
            .and_then(Value::as_array)
            .filter(|z| !z.is_empty() && z.len() <= 256)
        else {
            continue;
        };
        if boolean_or(effect, "SelectAll", false)? {
            continue;
        }
        ordinary.push(i);
        let mut primary = string_or(effect, "SceneRole", "")? == "main";
        for zone in zones {
            if !zone.is_object() {
                return Err(fail("Invalid scene zone"));
            }
            if string_or(zone, "name", "")? == "Desk-Campfire" {
                // nlohmann value<int> accepts numeric route indices, truncating floats.
                let zone_index = match zone.get("zone_idx") {
                    None => -1,
                    Some(v) => v
                        .as_f64()
                        .filter(|n| n.is_finite() && *n >= i32::MIN as f64 && *n <= i32::MAX as f64)
                        .ok_or_else(|| fail("Invalid legacy zone index"))?
                        as i32,
                };
                if zones.len() != 1
                    || string_or(zone, "vendor", "")? != "OpenRGB Visual Map Plugin"
                    || string_or(zone, "serial", "")? != "VISUAL_MAP_VISUAL_CONTROLLER_SERIAL"
                    || zone_index != 0
                    || boolean_or(zone, "is_segment", true)?
                {
                    return Err(fail("Invalid legacy Desk-Campfire route"));
                }
                primary = true;
            }
        }
        if primary && main.replace(i).is_some() {
            return Err(fail("Ambiguous main scene effect"));
        }
    }
    if main.is_none() && ordinary.len() == 1 {
        main = Some(ordinary[0])
    }
    Ok(SceneIndices {
        main: main.ok_or_else(|| fail("Choose exactly one main RGB scene group"))?,
        ambient,
    })
}
pub fn safe_pattern(pattern: &Value) -> ProfileResult<()> {
    let kind = string(field(pattern, "EffectClassName")?)?;
    integer(field(pattern, "FPS")?, 1, 144, "effect FPS")?;
    integer_or(pattern, "Temperature", -100, 100, 0)?;
    integer_or(pattern, "Tint", -100, 100, 0)?;
    integer_or(
        pattern,
        "Slider2Val",
        if kind == "RainbowWave" { 1 } else { 0 },
        if kind == "RainbowWave" { 50 } else { 100000 },
        1,
    )?;
    if string_or(pattern, "CustomName", "")?.len() > 256 {
        return Err(fail("Effect name too long"));
    }
    for key in ["RandomColors", "AllowOnlyFirst", "AutoStart"] {
        boolean_or(pattern, key, false)?;
    }
    if let Some(colors) = pattern.get("UserColors").filter(|v| !v.is_null()) {
        let colors = colors
            .as_array()
            .filter(|v| v.len() <= 8)
            .ok_or_else(|| fail("Invalid native palette"))?;
        for c in colors {
            integer(c, 0, 0xffffff, "native palette color")?;
        }
    }
    let empty = json!({});
    let custom = pattern.get("CustomSettings").unwrap_or(&empty);
    if !custom.is_object() {
        return Err(fail("Invalid custom settings"));
    }
    if kind == "NoiseMap" {
        for key in ["frequency", "amplitude", "lacunarity", "persistence"] {
            if let Some(value) = custom.get(key) {
                let value = value
                    .as_f64()
                    .ok_or_else(|| fail("Invalid noise parameter"))?;
                let max = if key == "frequency" { 0.5 } else { 5.0 };
                if !value.is_finite() || value < 0.000099 || value > max + 0.000001 {
                    return Err(fail("Noise parameter outside native UI range"));
                }
            }
        }
        if let Some(v) = custom.get("octaves") {
            integer(v, 1, 20, "noise octaves")?;
        }
        if let Some(v) = custom.get("motion") {
            integer(v, 0, 3, "noise motion")?;
        }
        if let Some(v) = custom.get("motion_speed") {
            integer(v, 0, 99, "noise motion speed")?;
        }
    }
    Ok(())
}
pub fn controls_from_json(controls: &Value) -> ProfileResult<Controls> {
    let fields = controls
        .as_object()
        .filter(|v| v.len() == 5)
        .ok_or_else(|| fail("Exactly five scene controls required"))?;
    let effect = effect_from_name(string(field(controls, "effect_type")?)?)?;
    let brightness = integer(field(controls, "brightness")?, 0, 100, "brightness")? as u8;
    let speed = integer(field(controls, "speed")?, 0, 100, "normalized speed")? as u8;
    let screen_brightness = integer(
        field(controls, "screen_brightness")?,
        0,
        100,
        "screen brightness",
    )? as u8;
    let colors = fields
        .get("colors")
        .and_then(Value::as_array)
        .filter(|c| !c.is_empty() && c.len() <= 8)
        .ok_or_else(|| fail("One to eight colors required"))?
        .iter()
        .map(|c| integer(c, 0, 0xffffff, "palette color").map(|c| Rgb::from_rrggbb(c as u32)))
        .collect::<ProfileResult<Vec<_>>>()?;
    Ok(Controls {
        effect,
        brightness,
        speed,
        colors,
        screen_brightness,
    })
}
pub fn controls_to_json(controls: &Controls) -> Value {
    json!({"effect_type":effect_name(controls.effect),"brightness":controls.brightness,"speed":controls.speed,"colors":controls.colors.iter().map(|c|c.rrggbb()).collect::<Vec<_>>(),"screen_brightness":controls.screen_brightness})
}
pub fn read_controls(profile: &Value) -> ProfileResult<Controls> {
    let position = indices(profile)?;
    let main = &profile["Effects"][position.main];
    let effect = effect_from_name(string(field(main, "EffectClassName")?)?)?;
    let mut colors = Vec::new();
    if let Some(native) = main
        .get("CustomSettings")
        .and_then(|v| v.get("colors"))
        .and_then(Value::as_array)
    {
        for c in native {
            colors.push(Rgb::from_native(
                integer(c, 0, 0xffffff, "palette color")? as u32
            ));
        }
    }
    if colors.is_empty() {
        colors = [0xff5500, 0xffc800, 0xc80000]
            .into_iter()
            .map(Rgb::from_rrggbb)
            .collect()
    }
    if colors.len() > 8 {
        return Err(fail("Scene supports at most eight colors"));
    }
    let (lo, hi) = effect.limits();
    let speed = effect
        .normalized_speed(integer(
            field(main, "Speed")?,
            lo as i64,
            hi as i64,
            "native speed",
        )?)
        .map_err(fail)? as u8;
    Ok(Controls {
        effect,
        brightness: integer(field(main, "Brightness")?, 0, 100, "brightness")? as u8,
        speed,
        colors,
        screen_brightness: match position.ambient {
            Some(i) => integer(
                field(&profile["Effects"][i], "Brightness")?,
                0,
                100,
                "screen brightness",
            )? as u8,
            None => 0,
        },
    })
}
pub fn read_controls_json(profile: &Value) -> ProfileResult<Value> {
    read_controls(profile).map(|c| controls_to_json(&c))
}
pub fn compose_profile(
    profile: &Value,
    controls: &Value,
    noise_template: Option<&Value>,
) -> ProfileResult<Value> {
    // Check profile selection before control parsing, matching native error ordering.
    indices(profile)?;
    let controls = controls_from_json(controls)?;
    compose_with_controls(profile, &controls, noise_template)
}
pub fn compose_with_controls(
    profile: &Value,
    controls: &Controls,
    noise_template: Option<&Value>,
) -> ProfileResult<Value> {
    let position = indices(profile)?;
    controls.validate().map_err(fail)?;
    let mut result = profile.clone();
    let main = &mut result["Effects"][position.main];
    if controls.effect == Effect::NoiseMap
        && string(field(main, "EffectClassName")?)? != "NoiseMap"
        && let Some(template) = noise_template.filter(|v| v.is_object())
    {
        main["CustomSettings"] = template.clone();
    }
    if !main.get("CustomSettings").is_some_and(Value::is_object) {
        main["CustomSettings"] = json!({});
    }
    main["EffectClassName"] = json!(effect_name(controls.effect));
    main["Brightness"] = json!(controls.brightness);
    main["Speed"] = json!(
        controls
            .effect
            .native_speed(controls.speed as i32)
            .map_err(fail)?
    );
    main["RandomColors"] = json!(false);
    main["CustomSettings"]["colors"] = json!(
        controls
            .colors
            .iter()
            .map(|c| c.native())
            .collect::<Vec<_>>()
    );
    if controls.effect == Effect::NoiseMap {
        main["CustomSettings"]["colors_choice"] = json!(2);
    }
    safe_pattern(main)?;
    if let Some(i) = position.ambient {
        result["Effects"][i]["Brightness"] = json!(controls.screen_brightness);
    }
    Ok(result)
}
/// nlohmann JSON compares integer/float numbers by numeric value, including hidden fields.
fn native_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => {
            a == b || ((a.is_f64() || b.is_f64()) && a.as_f64() == b.as_f64())
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| native_equal(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| native_equal(a, b)))
        }
        _ => a == b,
    }
}
pub fn validate_profile(
    current: &Value,
    candidate: &Value,
    noise_template: Option<&Value>,
) -> ProfileResult<()> {
    bounded(candidate)?;
    let controls = read_controls(candidate)?;
    let expected = compose_with_controls(current, &controls, noise_template)?;
    if !native_equal(&expected, candidate) {
        return Err(fail(
            "Scene candidate changes unowned state, route, timing or unsupported controls",
        ));
    }
    safe_pattern(&candidate["Effects"][indices(candidate)?.main])
}
pub fn catalogue() -> Value {
    json!([{"effect_type":"NoiseMap","name":"NoiseMap","min_speed":1,"max_speed":100,"supports_colors":true},{"effect_type":"Breathing","name":"Breathing","min_speed":10,"max_speed":200,"supports_colors":true},{"effect_type":"RainbowWave","name":"RainbowWave","min_speed":1,"max_speed":100,"supports_colors":false}])
}

#[cfg(test)]
mod tests {
    use super::*;
    fn base() -> Value {
        json!({"version":2,"future":"preserve","Effects":[{"EffectClassName":"NoiseMap","SceneRole":"main","Speed":8,"Brightness":60,"FPS":25,"CustomName":"Synthetic fire","AutoStart":true,"SelectAll":false,"UserColors":[],"RandomColors":false,"ControllerZones":[{"name":"Synthetic route","unowned":"route-extra"}],"CustomSettings":{"frequency":0.055,"colors_choice":2,"colors":[255,65280],"unowned":123}},{"EffectClassName":"Ambient","Brightness":80,"FPS":48,"CustomSettings":{"framerate":60,"gamma":1.0}}]})
    }
    #[test]
    fn native_rgb_and_owned_changes() {
        let base = base();
        let before = base.clone();
        let mut c = read_controls_json(&base).unwrap();
        assert_eq!(c["colors"][0], 0xff0000);
        c["brightness"] = json!(71);
        c["speed"] = json!(37);
        c["colors"] = json!([0x102030]);
        let result = compose_profile(&base, &c, None).unwrap();
        validate_profile(&base, &result, None).unwrap();
        assert_eq!(
            result["Effects"][0]["CustomSettings"]["colors"][0],
            0x302010
        );
        assert_eq!(result["Effects"][0]["CustomSettings"]["unowned"], 123);
        assert_eq!(
            result["Effects"][0]["ControllerZones"],
            base["Effects"][0]["ControllerZones"]
        );
        assert_eq!(result["Effects"][0]["FPS"], 25);
        assert_eq!(result["Effects"][1], base["Effects"][1]);
        assert_eq!(base, before);
    }
    #[test]
    fn all_effect_speed_roundtrips_and_template() {
        let base = base();
        let mut c = read_controls_json(&base).unwrap();
        for effect in ["NoiseMap", "Breathing", "RainbowWave"] {
            c["effect_type"] = json!(effect);
            for speed in 0..=100 {
                c["speed"] = json!(speed);
                let composed = compose_profile(&base, &c, None).unwrap();
                validate_profile(&base, &composed, None).unwrap();
                assert_eq!(
                    compose_profile(&base, &read_controls_json(&composed).unwrap(), None).unwrap(),
                    composed
                );
            }
        }
        c["effect_type"] = json!("Breathing");
        let b = compose_profile(&base, &c, None).unwrap();
        c["effect_type"] = json!("NoiseMap");
        let template = json!({"frequency":0.2,"amplitude":2.0,"hidden":true});
        let noise = compose_profile(&b, &c, Some(&template)).unwrap();
        assert_eq!(noise["Effects"][0]["CustomSettings"]["frequency"], 0.2);
        assert_eq!(noise["Effects"][0]["CustomSettings"]["hidden"], true);
        assert!(
            noise["Effects"][0]["CustomSettings"]
                .get("unowned")
                .is_none()
        );
        validate_profile(&b, &noise, Some(&template)).unwrap();
        assert!(validate_profile(&b, &noise, None).is_err());
    }
    #[test]
    fn malformed_control_types() {
        let base = base();
        let controls = read_controls_json(&base).unwrap();
        for key in ["brightness", "speed", "screen_brightness"] {
            for value in [json!(-1), json!(101), json!(1.0), json!(true), Value::Null] {
                let mut c = controls.clone();
                c[key] = value;
                assert!(compose_profile(&base, &c, None).is_err());
            }
        }
        for palette in [
            json!([]),
            json!([-1]),
            json!([16777216]),
            json!([1.0]),
            json!([true]),
            json!([1, 2, 3, 4, 5, 6, 7, 8, 9]),
            json!(null),
        ] {
            let mut c = controls.clone();
            c["colors"] = palette;
            assert!(compose_profile(&base, &c, None).is_err());
        }
        let mut c = controls.clone();
        c["extra"] = json!(1);
        assert!(compose_profile(&base, &c, None).is_err());
        c = controls;
        c["effect_type"] = json!("Ambient");
        assert!(compose_profile(&base, &c, None).is_err());
    }
    #[test]
    fn ambiguity_and_legacy_routes() {
        let mut b = base();
        let extra = b["Effects"][0].clone();
        b["Effects"].as_array_mut().unwrap().push(extra);
        assert!(indices(&b).is_err());
        b = base();
        b["Effects"]
            .as_array_mut()
            .unwrap()
            .push(json!({"EffectClassName":"Ambient"}));
        assert!(indices(&b).is_err());
        b = base();
        b["Effects"][0].as_object_mut().unwrap().remove("SceneRole");
        assert_eq!(indices(&b).unwrap().main, 0);
        let route = json!({"name":"Desk-Campfire","vendor":"OpenRGB Visual Map Plugin","serial":"VISUAL_MAP_VISUAL_CONTROLLER_SERIAL","zone_idx":0,"is_segment":false});
        b["Effects"][0]["ControllerZones"] = json!([route]);
        assert!(indices(&b).is_ok());
        b["Effects"][0]["ControllerZones"][0]["vendor"] = json!("Wrong");
        assert!(indices(&b).is_err());
        b = base();
        b["Effects"][0]["SelectAll"] = json!(true);
        assert!(indices(&b).is_err());
        b["Effects"][0]["SelectAll"] = json!(0);
        assert!(indices(&b).is_err());
    }
    #[test]
    fn unowned_mutations_rejected() {
        let b = base();
        let composed = compose_profile(&b, &read_controls_json(&b).unwrap(), None).unwrap();
        for path in [
            "/Effects/0/FPS",
            "/Effects/0/AutoStart",
            "/Effects/0/ControllerZones/0/name",
            "/Effects/0/CustomSettings/frequency",
            "/Effects/1/FPS",
            "/Effects/1/CustomSettings/framerate",
            "/future",
        ] {
            let mut candidate = composed.clone();
            *candidate.pointer_mut(path).unwrap() = json!(99);
            assert!(validate_profile(&b, &candidate, None).is_err(), "{path}");
        }
        let mut candidate = composed.clone();
        candidate["Effects"][0]["CustomSettings"]["unowned"] = json!(123.0);
        assert!(validate_profile(&b, &candidate, None).is_ok());
        assert!(!native_equal(
            &json!(9007199254740992u64),
            &json!(9007199254740993u64)
        ));
    }
    #[test]
    fn safe_pattern_rejects_hidden_invalid_values() {
        let b = base();
        let c = read_controls_json(&b).unwrap();
        for (key, value) in [
            ("FPS", json!(0)),
            ("FPS", json!(145)),
            ("Temperature", json!(101)),
            ("Tint", json!(1.0)),
            ("AllowOnlyFirst", json!(1)),
            ("AutoStart", Value::Null),
            ("UserColors", json!([16777216])),
            ("CustomName", json!("x".repeat(257))),
        ] {
            let mut bad = b.clone();
            bad["Effects"][0][key] = value;
            assert!(compose_profile(&bad, &c, None).is_err(), "{key}");
        }
        for (key, value) in [
            ("frequency", json!(0.6)),
            ("amplitude", json!(-1)),
            ("octaves", json!(0)),
            ("motion", json!(4)),
            ("motion_speed", json!(100)),
            ("persistence", json!("1")),
        ] {
            let mut bad = b.clone();
            bad["Effects"][0]["CustomSettings"][key] = value;
            assert!(compose_profile(&bad, &c, None).is_err(), "{key}");
        }
        let mut bad = b.clone();
        bad["Effects"][0]["CustomSettings"] = json!(false);
        assert!(compose_profile(&bad, &c, None).is_ok());
    }
    #[test]
    fn structural_bounds_and_defaults() {
        assert!(indices(&json!({"Effects":[]})).is_err());
        let b = base();
        let mut huge = b.clone();
        huge["large"] = json!("x".repeat(MAX_BYTES));
        assert!(indices(&huge).is_err());
        let mut too_many = b.clone();
        too_many["Effects"] = json!(vec![json!({}); 33]);
        assert!(indices(&too_many).is_err());
        let mut bad = b.clone();
        bad["Effects"][0]["ControllerZones"] = json!(vec![json!({}); 257]);
        assert!(indices(&bad).is_err());
        let mut default = b;
        default["Effects"].as_array_mut().unwrap().pop();
        default["Effects"][0]["CustomSettings"]["colors"] = json!([]);
        let c = read_controls(&default).unwrap();
        assert_eq!(c.colors[0], Rgb::from_rrggbb(0xff5500));
        assert_eq!(c.screen_brightness, 0);
    }
}

#[test]
fn native_cpp_differential() {
    let fixtures: Value =
        serde_json::from_str(include_str!("fixtures/scene-profile-native.json")).unwrap();
    let cases = fixtures["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 29);
    for (i, case) in cases.iter().enumerate() {
        let profile = case.get("profile").unwrap_or(&fixtures["base"]);
        let template = case.get("template");
        if case["op"] == "validate" {
            assert_eq!(
                validate_profile(profile, &case["candidate"], template).is_ok(),
                case["ok"].as_bool().unwrap(),
                "native validation case {i}"
            );
            continue;
        }
        let result = if case["op"] == "read" {
            read_controls_json(profile)
        } else {
            compose_profile(profile, &case["controls"], template)
        };
        assert_eq!(
            result.is_ok(),
            case["ok"].as_bool().unwrap(),
            "native error case {i}: {result:?}"
        );
        if let Ok(actual) = result {
            assert_eq!(actual, case["expected"], "native JSON case {i}");
        }
    }
    assert_eq!(catalogue().as_array().unwrap().len(), 3);
}
