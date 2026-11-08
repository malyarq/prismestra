//! Explicit local monitor bindings. Not constructed by stand tests or the preview UI.
//! Construct/poll/drop on the same capture thread; no implicit monitor enumeration.
use super::ambient::{self, Provider};
use crate::{
    capture::{self, Frame, windows::Source},
    lighting::ambient::Rect,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub source: String,
    pub device_name: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bindings {
    pub monitors: Vec<Binding>,
}
impl Bindings {
    pub fn validate(&self, config: &ambient::Config) -> Result<(), String> {
        self.validate_partial(config)?;
        for route in config.routes.iter().filter(|r| r.enabled) {
            if !self.monitors.iter().any(|m| m.source == route.source) {
                return Err("Enabled ambient route has no local monitor binding".into());
            }
        }
        Ok(())
    }
    pub fn validate_partial(&self, config: &ambient::Config) -> Result<(), String> {
        if self.monitors.len() > 16 {
            return Err("Monitor binding limit exceeded".into());
        }
        let mut keys = BTreeSet::new();
        for m in &self.monitors {
            if !keys.insert(&m.source) || !config.sources.iter().any(|s| s.key == m.source) {
                return Err("Unknown/duplicate monitor source binding".into());
            }
            capture::validate_name(&m.device_name)?;
        }
        Ok(())
    }
}
pub struct Native {
    sources: BTreeMap<String, Source>,
}
impl Native {
    pub fn new(bindings: &Bindings, config: &ambient::Config) -> Result<Self, String> {
        bindings.validate(config)?;
        let sources = bindings
            .monitors
            .iter()
            .map(|m| Ok((m.source.clone(), Source::new(m.device_name.clone())?)))
            .collect::<Result<_, String>>()?;
        Ok(Self { sources })
    }
}
impl Provider for Native {
    fn poll(
        &mut self,
        source: &str,
        crop: Option<Rect>,
        requested_ms: u64,
    ) -> Result<Option<Frame>, String> {
        let frame = self
            .sources
            .get_mut(source)
            .ok_or("Monitor source is not locally bound")?
            .poll(crop)?;
        // Use the request's start in the caller's monotonic domain. A capture that
        // completes later is conservatively older, never stamped beyond tick's now.
        Ok(frame.map(|mut f| {
            f.observed_ms = requested_ms;
            f
        }))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_bindings_are_explicit_and_validation_never_constructs_capture() {
        let config = ambient::Config {
            sources: vec![ambient::Source {
                key: "main".into(),
                rate_hz: 60,
                crop: None,
                stale_after_ms: 2000,
            }],
            routes: vec![],
        };
        let mut b = Bindings {
            monitors: vec![Binding {
                source: "main".into(),
                device_name: r"\\.\DISPLAY1".into(),
            }],
        };
        b.validate(&config).unwrap();
        b.monitors.push(b.monitors[0].clone());
        assert!(b.validate(&config).is_err());
        b.monitors.pop();
        b.monitors[0].device_name = "first monitor".into();
        assert!(b.validate(&config).is_err());
        b.monitors[0].device_name = r"\\.\DISPLAY2".into();
        b.monitors[0].source = "missing".into();
        assert!(b.validate(&config).is_err());
    }
}
