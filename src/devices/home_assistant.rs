//! Non-secret endpoint/entity identity. Tokens are never part of these descriptors.
use sha2::{Digest, Sha256};

/// Output supported by the advertised HA modes, never inferred from a light ID.
/// https://developers.home-assistant.io/docs/core/entity/light/#color-modes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightCapability {
    Color,
    Brightness,
    ColorTemperature,
    OnOff,
}

impl LightCapability {
    pub fn from_modes(modes: &[String]) -> Option<Self> {
        if modes.is_empty() || modes.len() > 32 {
            return None;
        }
        if modes.len() == 1 {
            match modes[0].as_str() {
                "onoff" => return Some(Self::OnOff),
                "brightness" => return Some(Self::Brightness),
                "color_temp" => return Some(Self::ColorTemperature),
                _ => (),
            }
        }
        let mut color = false;
        for mode in modes {
            match mode.as_str() {
                "rgb" | "hs" | "xy" | "rgbw" | "rgbww" => color = true,
                "color_temp" | "white" => (),
                _ => return None,
            }
        }
        color.then_some(Self::Color)
    }

    pub fn from_attributes(attributes: &serde_json::Value) -> Option<Self> {
        let modes = attributes["supported_color_modes"].as_array()?;
        if modes.len() > 32 {
            return None;
        }
        let modes = modes
            .iter()
            .map(|mode| mode.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?;
        Self::from_modes(&modes)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Color => "Color + brightness",
            Self::Brightness => "Brightness only",
            Self::ColorTemperature => "Color temperature; brightness output only",
            Self::OnOff => "On/off only",
        }
    }

    /// HA translates rgb_color into the entity's supported color format.
    /// Color-temperature-only lights keep their temperature; no RGB mapping.
    pub fn command(self, entity: &str, rgb: [u8; 3]) -> (&'static str, serde_json::Value) {
        let peak = *rgb.iter().max().unwrap();
        let mut data = serde_json::json!({"entity_id": entity});
        if peak == 0 {
            return ("turn_off", data);
        }
        if self != Self::OnOff {
            data["brightness"] = peak.into();
        }
        if self == Self::Color {
            let normalized =
                rgb.map(|c| ((u16::from(c) * 255 + u16::from(peak) / 2) / u16::from(peak)) as u8);
            data["rgb_color"] = serde_json::json!(normalized);
        }
        ("turn_on", data)
    }
}

pub fn canonical_endpoint(url: &str) -> Result<String, String> {
    if url.len() > 2048
        || !url.is_ascii()
        || url.bytes().any(|b| b <= 32 || b == 127)
        || url.contains(['@', '?', '#', '\\', '%'])
    {
        return Err("Invalid Home Assistant endpoint".into());
    }
    #[cfg(windows)]
    crate::transport::home_assistant::Endpoint::parse(url)
        .map_err(|_| "Invalid Home Assistant endpoint")?;
    let (scheme, rest) = url
        .split_once("://")
        .ok_or("Home Assistant requires http(s) URL")?;
    let default = match scheme {
        "http" => 80,
        "https" => 443,
        _ => return Err("Home Assistant requires http(s) URL".into()),
    };
    let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    if path.split('/').any(|part| matches!(part, "." | "..")) {
        return Err("Invalid Home Assistant base path".into());
    }
    let (host, port) = if let Some(ip) = authority.strip_prefix('[') {
        let (ip, tail) = ip.split_once(']').ok_or("Invalid IPv6 authority")?;
        let ip: std::net::IpAddr = ip.parse().map_err(|_| "Invalid IPv6 address")?;
        let port = if tail.is_empty() {
            default
        } else {
            tail.strip_prefix(':')
                .ok_or("Invalid port")?
                .parse::<u16>()
                .map_err(|_| "Invalid port")?
        };
        (
            match ip.to_canonical() {
                std::net::IpAddr::V4(ip) => ip.to_string(),
                std::net::IpAddr::V6(ip) => format!("[{ip}]"),
            },
            port,
        )
    } else {
        let (host, port) = authority
            .rsplit_once(':')
            .map_or(Ok((authority, default)), |(h, p)| {
                p.parse::<u16>().map(|p| (h, p)).map_err(|_| "Invalid port")
            })?;
        if host.is_empty()
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        {
            return Err("Invalid Home Assistant host".into());
        }
        (host.trim_end_matches('.').to_ascii_lowercase(), port)
    };
    if host.is_empty() || port == 0 {
        return Err("Invalid Home Assistant authority".into());
    }
    let port = if port == default {
        String::new()
    } else {
        format!(":{port}")
    };
    let path = path.trim_end_matches('/');
    Ok(format!(
        "{scheme}://{host}{port}{}",
        if path.is_empty() {
            String::new()
        } else {
            format!("/{path}")
        }
    ))
}
pub fn credential_id(endpoint: &str) -> Result<String, String> {
    let endpoint = canonical_endpoint(endpoint)?;
    Ok(
        Sha256::digest(format!("Prismestra/HomeAssistant/v1\0{endpoint}"))
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}
pub fn valid_entity(entity: &str) -> bool {
    entity.len() <= 255
        && entity.strip_prefix("light.").is_some_and(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn capabilities_validate_modes_and_do_not_infer_color() {
        for mode in ["rgb", "hs", "xy", "rgbw", "rgbww"] {
            assert_eq!(
                LightCapability::from_attributes(&json!({"supported_color_modes":[mode]})),
                Some(LightCapability::Color)
            );
        }
        for modes in [
            json!([]),
            json!(["unknown"]),
            json!(["future_mode"]),
            json!(["rgb", 7]),
            json!(["brightness", "rgb"]),
            json!(["onoff", "color_temp"]),
            json!(["white"]),
            json!(["color_temp", "white"]),
            json!("rgb"),
            json!(null),
        ] {
            assert_eq!(
                LightCapability::from_attributes(&json!({"supported_color_modes":modes})),
                None,
                "{modes}"
            );
        }
        assert_eq!(LightCapability::from_attributes(&json!({})), None);
        assert_eq!(
            LightCapability::from_attributes(
                &json!({"supported_color_modes":["color_temp","rgb","white"]})
            ),
            Some(LightCapability::Color)
        );
    }

    #[test]
    fn commands_respect_color_brightness_and_onoff_capabilities() {
        use LightCapability::*;
        for capability in [Color, Brightness, ColorTemperature, OnOff] {
            assert_eq!(
                capability.command("light.room", [0; 3]),
                ("turn_off", json!({"entity_id":"light.room"}))
            );
            let (service, body) = capability.command("light.room", [64, 32, 0]);
            assert_eq!(service, "turn_on");
            let mut expected = json!({"entity_id":"light.room"});
            if capability != OnOff {
                expected["brightness"] = json!(64);
            }
            if capability == Color {
                expected["rgb_color"] = json!([255, 128, 0]);
            }
            assert_eq!(body, expected);
        }
    }
    #[test]
    fn equivalent_authorities_share_credentials_but_paths_and_protocols_do_not() {
        assert_eq!(
            credential_id("http://EXAMPLE.test:80/ha/").unwrap(),
            credential_id("http://example.test/ha").unwrap()
        );
        assert_ne!(
            credential_id("https://example.test/HA").unwrap(),
            credential_id("https://example.test/ha").unwrap()
        );
        assert_ne!(
            credential_id("http://example.test").unwrap(),
            credential_id("https://example.test").unwrap()
        );
        assert_eq!(
            credential_id("http://[::ffff:127.0.0.1]:8123").unwrap(),
            credential_id("http://127.0.0.1:8123/").unwrap()
        );
        for url in [
            "http://token@example.test",
            "file:///secret",
            "https://example.test/?token=x",
            "http://example.test/../x",
            "http://example.test:0",
        ] {
            assert!(credential_id(url).is_err());
        }
    }
    #[test]
    fn only_whole_light_entities_are_valid() {
        assert!(valid_entity("light.office_1"));
        for entity in [
            "switch.office",
            "climate.office",
            "light.",
            "light.x/y",
            "light.X",
            "light.a.b",
        ] {
            assert!(!valid_entity(entity));
        }
    }
}
