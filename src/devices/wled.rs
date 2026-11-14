//! Saved WLED identity is separate from portable scene geometry.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Controller {
    pub http_endpoint: String,
    pub identity: String,
}
impl Controller {
    pub fn validate(&self) -> Result<(), String> {
        crate::transport::wled_info::Endpoint::parse(&self.http_endpoint)
            .map_err(|_| "Invalid WLED HTTP endpoint")?;
        let mac = self
            .identity
            .strip_prefix("wled:")
            .ok_or("Invalid WLED identity")?;
        if mac.len() != 12
            || !mac
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || mac == "000000000000"
            || mac == "ffffffffffff"
        {
            return Err("Invalid WLED identity".into());
        }
        Ok(())
    }
    pub fn verify(
        &self,
        info: &crate::transport::wled_info::Info,
        leds: usize,
    ) -> Result<(), String> {
        self.validate()?;
        if info.stable_id() != self.identity {
            return Err("WLED controller identity changed; rediscover before Apply".into());
        }
        if info.led_count != leds {
            return Err(format!(
                "WLED LED count changed to {}; rediscover and apply the new geometry",
                info.led_count
            ));
        }
        Ok(())
    }
    pub fn validate_target(&self, target: std::net::SocketAddr) -> Result<(), String> {
        self.validate()?;
        let http = crate::transport::wled_info::Endpoint::parse(&self.http_endpoint)
            .map_err(|e| e.to_string())?;
        if http.address().map(|ip| ip.to_canonical()) != Some(target.ip().to_canonical()) {
            return Err("WLED HTTP metadata and DDP output must address the same IP".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn controller() -> Controller {
        Controller {
            http_endpoint: "http://192.168.1.20:80".into(),
            identity: "wled:02aabbccddee".into(),
        }
    }
    #[test]
    fn metadata_must_match_identity_capacity_and_ddp_host() {
        let info = crate::transport::wled_info::parse_info(
            br#"{"mac":"02aabbccddee","leds":{"count":300}}"#,
        )
        .unwrap();
        assert!(controller().verify(&info, 300).is_ok());
        assert!(controller().verify(&info, 153).is_err());
        let mut other = controller();
        other.identity = "wled:02aabbccddef".into();
        assert!(other.verify(&info, 300).is_err());
        assert!(
            controller()
                .validate_target("192.168.1.20:4048".parse().unwrap())
                .is_ok()
        );
        assert!(
            controller()
                .validate_target("192.168.1.21:4048".parse().unwrap())
                .is_err()
        );
    }
    #[test]
    fn legacy_binding_round_trip_and_verified_identity_keys() {
        use crate::devices::Binding;
        let old = r#"{"type":"wled_ddp","endpoint":"192.168.1.20:4048"}"#;
        let binding: Binding = serde_json::from_str(old).unwrap();
        assert_eq!(serde_json::to_string(&binding).unwrap(), old);
        let verified = Binding::WledDdp {
            endpoint: "192.168.1.20:4048".parse().unwrap(),
            controller: Some(controller()),
        };
        assert!(
            verified
                .endpoint_keys()
                .contains(&"wled:02aabbccddee".into())
        );
        assert_eq!(
            verified,
            serde_json::from_str(&serde_json::to_string(&verified).unwrap()).unwrap()
        );
    }
}
