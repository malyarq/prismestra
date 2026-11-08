//! Logical temperature selection for LCD overlays.
//! Inputs are explicitly injected observations, never hardware discovery or fabricated values.
use super::lcd::Sample;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
const MAX: usize = 256;
const TTL: u64 = 5000;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Value {
    /// A portable logical input slot. Physical sensor/provider IDs stay in local bindings.
    Sensor { input: String },
    /// All members must be direct logical sensors and fresh; missing members suppress the mean.
    Mean { members: Vec<String> },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub key: String,
    pub value: Value,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub sources: Vec<Source>,
}
impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if self.sources.len() > MAX {
            return Err("Temperature source limit exceeded".into());
        }
        let mut keys = BTreeSet::new();
        for s in &self.sources {
            if !super::ambient::key(&s.key) || !keys.insert(&s.key) {
                return Err("Invalid/duplicate temperature source key".into());
            }
            if let Value::Sensor { input } = &s.value
                && !super::ambient::key(input)
            {
                return Err("Invalid logical temperature input".into());
            }
        }
        for s in &self.sources {
            if let Value::Mean { members } = &s.value {
                let mut unique = BTreeSet::new();
                let mut inputs = BTreeSet::new();
                if members.is_empty()
                    || members.len() > MAX
                    || members.iter().any(|key| {
                        !unique.insert(key)
                            || !self.sources.iter().any(|s| {
                                s.key == *key
                                    && match &s.value {
                                        Value::Sensor { input } => inputs.insert(input),
                                        Value::Mean { .. } => false,
                                    }
                            })
                    })
                {
                    return Err("Temperature mean requires unique direct sensor members".into());
                }
            }
        }
        Ok(())
    }
    /// A snapshot is atomic: ambiguous duplicate input slots reject the entire snapshot.
    /// Invalid, stale or future temperatures are omitted; no old or partial mean is retained.
    pub fn select(&self, now_ms: u64, inputs: &[Sample]) -> Result<Vec<Sample>, String> {
        self.validate()?;
        if inputs.len() > 4096 {
            return Err("Temperature observation limit exceeded".into());
        }
        let mut by_input = BTreeMap::new();
        for input in inputs {
            if !super::ambient::key(&input.key) || by_input.insert(&input.key, input).is_some() {
                return Err("Invalid/duplicate temperature observation key".into());
            }
        }
        let mut direct = BTreeMap::new();
        for s in &self.sources {
            if let Value::Sensor { input } = &s.value
                && let Some(value) = by_input.get(input)
                && value.celsius.is_finite()
                && (-20.0..=150.0).contains(&value.celsius)
                && value.observed_ms != 0
                && now_ms
                    .checked_sub(value.observed_ms)
                    .is_some_and(|age| age <= TTL)
            {
                direct.insert(s.key.as_str(), *value);
            }
        }
        let mut result = Vec::with_capacity(self.sources.len());
        for s in &self.sources {
            match &s.value {
                Value::Sensor { .. } => {
                    if let Some(value) = direct.get(s.key.as_str()) {
                        result.push(Sample {
                            key: s.key.clone(),
                            celsius: value.celsius,
                            observed_ms: value.observed_ms,
                        });
                    }
                }
                Value::Mean { members } => {
                    let values = members
                        .iter()
                        .map(|m| direct.get(m.as_str()).copied())
                        .collect::<Option<Vec<_>>>();
                    if let Some(values) = values {
                        result.push(Sample {
                            key: s.key.clone(),
                            celsius: values.iter().map(|v| v.celsius).sum::<f64>()
                                / values.len() as f64,
                            observed_ms: values.iter().map(|v| v.observed_ms).min().unwrap(),
                        });
                    }
                }
            }
        }
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        Config {
            sources: vec![
                Source {
                    key: "ram-a".into(),
                    value: Value::Sensor {
                        input: "slot-a".into(),
                    },
                },
                Source {
                    key: "ram-b".into(),
                    value: Value::Sensor {
                        input: "slot-b".into(),
                    },
                },
                Source {
                    key: "ram".into(),
                    value: Value::Mean {
                        members: vec!["ram-a".into(), "ram-b".into()],
                    },
                },
            ],
        }
    }
    fn inputs() -> Vec<Sample> {
        vec![
            Sample {
                key: "slot-a".into(),
                celsius: 40.0,
                observed_ms: 2000,
            },
            Sample {
                key: "slot-b".into(),
                celsius: 50.0,
                observed_ms: 3000,
            },
        ]
    }
    #[test]
    fn ram_mean_requires_every_member_and_preserves_oldest_age() {
        let c = config();
        let mut input = inputs();
        let values = c.select(6000, &input).unwrap();
        assert_eq!(values.len(), 3);
        assert_eq!(values[2].key, "ram");
        assert_eq!(values[2].celsius, 45.0);
        assert_eq!(values[2].observed_ms, 2000);
        assert_eq!(c.select(7001, &input).unwrap().len(), 1);
        input[1].celsius = f64::NAN;
        assert_eq!(c.select(6000, &input).unwrap().len(), 1);
        input[1].celsius = 50.;
        input[1].observed_ms = 6001;
        assert_eq!(c.select(6000, &input).unwrap().len(), 1);
    }
    #[test]
    fn ambiguity_cycles_nested_means_and_duplicate_members_are_rejected() {
        let c = config();
        let mut i = inputs();
        i.push(i[0].clone());
        assert!(c.select(6000, &i).is_err());
        let mut bad = c.clone();
        bad.sources[2].value = Value::Mean {
            members: vec!["ram".into()],
        };
        assert!(bad.validate().is_err());
        bad.sources[2].value = Value::Mean {
            members: vec!["ram-a".into(), "ram-a".into()],
        };
        assert!(bad.validate().is_err());
        bad.sources[0].key = "/physical/path".into();
        assert!(bad.validate().is_err());
        assert_eq!(
            serde_json::from_slice::<Config>(&serde_json::to_vec(&c).unwrap()).unwrap(),
            c
        );
    }
}
