//! Received-value handling only. FanControl IPC and MI00 access are not implemented here.
//! Receiving a cached snapshot does not prove the physical sensor's acquisition time.
use std::collections::{HashMap, HashSet};
pub const RAM_MEAN_ID: &str = "/memory/average/temperature/0";
pub const LIQUID_ID: &str = "/tt264a233c/liquid/temperature/0";
pub const TTL_MS: u64 = 5000;
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub id: String,
    pub name: String,
    pub origin: String,
    pub celsius: f64,
}
pub struct SensorRecord {
    pub reading: Reading,
    pub kind: i32,
    pub value_present: bool,
}
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub readings: Vec<Reading>,
    pub error: String,
}

fn valid(value: f64) -> bool {
    value.is_finite() && (-20.0..=150.0).contains(&value)
}
pub fn filter_fan(records: Vec<SensorRecord>) -> Result<Vec<Reading>, String> {
    if records.len() > 4096 {
        return Err("Too many sensor records".into());
    }
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    for record in records {
        if record.kind != 3 {
            continue;
        }
        let reading = record.reading;
        if [&reading.id, &reading.name, &reading.origin]
            .iter()
            .any(|s| s.len() > 4096)
        {
            return Err("Sensor string too long".into());
        }
        if reading.id.starts_with("/memory/dimm/") {
            let suffix = &reading.id[13..];
            let Some(number) = suffix.strip_suffix("/temperature/0") else {
                continue;
            };
            if number.is_empty() || !number.bytes().all(|c| c.is_ascii_digit()) {
                continue;
            }
        }
        if reading.id.is_empty() || !seen.insert(reading.id.clone()) {
            return Err("Empty or duplicate temperature ID".into());
        }
        if record.value_present && valid(reading.celsius) {
            output.push(reading);
        }
    }
    Ok(output)
}
fn fresh(observed: Option<u64>, now: u64) -> bool {
    observed.is_some_and(|at| at > 0 && now >= at && now - at <= TTL_MS)
}
#[derive(Default)]
pub struct Sources {
    fan: Snapshot,
    liquid: Snapshot,
    fan_at: Option<u64>,
    liquid_at: Option<u64>,
    last_liquid_attempt: Option<u64>,
    ram_ids: Vec<String>,
}
impl Sources {
    pub fn with_ram_ids(ids: Vec<String>) -> Result<Self, String> {
        let mut sources = Self::default();
        sources.set_ram_ids(ids)?;
        Ok(sources)
    }
    pub fn ram_ids(&self) -> &[String] {
        &self.ram_ids
    }
    pub fn set_ram_ids(&mut self, ids: Vec<String>) -> Result<(), String> {
        let mut seen = HashSet::new();
        if ids.len() > 4096
            || ids
                .iter()
                .any(|id| id.is_empty() || id.len() > 4096 || !seen.insert(id))
        {
            return Err("Invalid or duplicate RAM mean source ID".into());
        }
        self.ram_ids = ids;
        Ok(())
    }
    pub fn accept_fan(&mut self, result: Result<Vec<Reading>, String>, received_ms: u64) {
        let result = result.and_then(|readings| {
            filter_fan(
                readings
                    .into_iter()
                    .map(|reading| SensorRecord {
                        reading,
                        kind: 3,
                        value_present: true,
                    })
                    .collect(),
            )
        });
        let (snapshot, at) = accepted(result, received_ms);
        self.fan = snapshot;
        self.fan_at = at;
    }
    pub fn accept_liquid(&mut self, result: Result<Vec<Reading>, String>, received_ms: u64) {
        let result = result.and_then(|readings| {
            if readings.len() > 1
                || readings.iter().any(|r| {
                    r.id != LIQUID_ID
                        || !r.celsius.is_finite()
                        || !(1.0..100.0).contains(&r.celsius)
                })
            {
                Err("Invalid liquid snapshot".into())
            } else {
                Ok(readings)
            }
        });
        let (snapshot, at) = accepted(result, received_ms);
        self.liquid = snapshot;
        self.liquid_at = at;
    }
    pub fn liquid_due(&self, now_ms: u64) -> bool {
        self.last_liquid_attempt
            .is_none_or(|at| now_ms.checked_sub(at).is_some_and(|age| age >= 2000))
    }
    pub fn attempted_liquid(&mut self, now_ms: u64) {
        self.last_liquid_attempt = Some(now_ms);
    }
    pub fn combined(&self, now_ms: u64) -> Snapshot {
        let mut readings = Vec::new();
        if fresh(self.fan_at, now_ms) {
            readings.extend(self.fan.readings.iter().cloned());
            let mut seen = HashSet::new();
            let mut sum = 0.;
            let mut complete = !self.ram_ids.is_empty() && self.ram_ids.len() <= 4096;
            let values: HashMap<_, _> = self
                .fan
                .readings
                .iter()
                .map(|r| (r.id.as_str(), r.celsius))
                .collect();
            let ids = if complete {
                self.ram_ids.as_slice()
            } else {
                &[]
            };
            for id in ids {
                if !seen.insert(id) {
                    complete = false;
                    break;
                }
                let Some(value) = values.get(id.as_str()) else {
                    complete = false;
                    break;
                };
                sum += value;
            }
            if complete && sum.is_finite() {
                readings.push(Reading {
                    id: RAM_MEAN_ID.into(),
                    name: "RAM".into(),
                    origin: format!(
                        "Derived arithmetic mean (not a physical sensor) of selected sources: {}",
                        self.ram_ids.join(" + ")
                    ),
                    celsius: sum / self.ram_ids.len() as f64,
                });
            }
        }
        if fresh(self.liquid_at, now_ms) {
            readings.extend(self.liquid.readings.iter().cloned());
        }
        let error = [
            ("FanControl", &self.fan.error),
            ("TT MI00", &self.liquid.error),
        ]
        .into_iter()
        .filter(|(_, error)| !error.is_empty())
        .map(|(source, error)| format!("{source}: {error}"))
        .collect::<Vec<_>>()
        .join("; ");
        Snapshot { readings, error }
    }
    pub fn oldest_observation(&self, now_ms: u64) -> Option<u64> {
        [(self.fan_at, &self.fan), (self.liquid_at, &self.liquid)]
            .into_iter()
            .filter(|(at, snapshot)| fresh(*at, now_ms) && !snapshot.readings.is_empty())
            .filter_map(|(at, _)| at)
            .min()
    }
}
fn accepted(result: Result<Vec<Reading>, String>, now: u64) -> (Snapshot, Option<u64>) {
    match result {
        Ok(readings) => (
            Snapshot {
                readings,
                error: String::new(),
            },
            Some(now),
        ),
        Err(error) => (
            Snapshot {
                readings: Vec::new(),
                error,
            },
            None,
        ),
    }
}
/// Bytes for the known status request, not permission to send it to a device.
/// It uses a HID write despite obtaining a temperature; all real access stays disabled.
pub fn liquid_status_request() -> [u8; 441] {
    let mut bytes = [0; 441];
    bytes[1..5].copy_from_slice(&[0x82, 1, 0, 0x80]);
    bytes
}
pub fn parse_liquid(bytes: &[u8]) -> Result<f64, &'static str> {
    if bytes.len() != 441 || bytes[..5] != [0, 0x82, 1, 0, 0x80] {
        return Err("Invalid liquid status report");
    }
    let value = f64::from(u16::from_le_bytes([bytes[5], bytes[6]])) / 100.;
    if !(1.0..100.0).contains(&value) {
        return Err("Liquid status temperature outside1..100C");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reading(id: &str, value: f64) -> Reading {
        Reading {
            id: id.into(),
            name: String::new(),
            origin: String::new(),
            celsius: value,
        }
    }
    #[test]
    fn full_ram_mean_independent_freshness_and_error_replacement() {
        let mut s = Sources {
            ram_ids: vec!["A".into(), "B".into()],
            ..Sources::default()
        };
        s.accept_fan(Ok(vec![reading("A", 40.), reading("B", 44.)]), 1000);
        s.accept_liquid(Ok(vec![reading(LIQUID_ID, 33.)]), 1000);
        assert_eq!(
            s.combined(6000)
                .readings
                .iter()
                .find(|r| r.id == RAM_MEAN_ID)
                .unwrap()
                .celsius,
            42.
        );
        assert!(s.combined(6001).readings.is_empty());
        assert!(s.combined(999).readings.is_empty());
        s.accept_fan(Ok(vec![reading("A", 0.), reading("B", 44.)]), 6000);
        assert!(!s.combined(6001).readings.iter().any(|r| r.id == LIQUID_ID));
        assert_eq!(s.oldest_observation(6001), Some(6000));
        s.accept_fan(Err("offline".into()), 6002);
        assert!(s.combined(6003).readings.is_empty());
        assert!(s.combined(6003).error.contains("FanControl: offline"));
    }
    #[test]
    fn no_partial_or_duplicate_ram_average() {
        let mut s = Sources {
            ram_ids: vec!["A".into(), "B".into()],
            ..Sources::default()
        };
        s.accept_fan(Ok(vec![reading("A", 40.)]), 1000);
        assert!(
            !s.combined(1000)
                .readings
                .iter()
                .any(|r| r.id == RAM_MEAN_ID)
        );
        s.ram_ids = vec!["A".into(), "A".into()];
        assert!(
            !s.combined(1000)
                .readings
                .iter()
                .any(|r| r.id == RAM_MEAN_ID)
        );
    }
    #[test]
    fn sensor_filter_excludes_spd_metadata_but_keeps_zero_and_boundary() {
        let entries = [
            ("/memory/dimm/1/temperature/1", 42.),
            ("/memory/dimm/1/temperature/0", 0.),
            ("X", -20.),
            ("Y", 150.),
            ("Z", 150.01),
        ]
        .into_iter()
        .map(|(id, v)| SensorRecord {
            reading: reading(id, v),
            kind: 3,
            value_present: true,
        })
        .collect();
        let values = filter_fan(entries).unwrap();
        assert_eq!(values.len(), 3);
        assert_eq!(values[0].celsius, 0.);
        assert!(
            filter_fan(vec![
                SensorRecord {
                    reading: reading("A", f64::NAN),
                    kind: 3,
                    value_present: false
                },
                SensorRecord {
                    reading: reading("A", 40.),
                    kind: 3,
                    value_present: true
                }
            ])
            .is_err()
        );
    }
    #[test]
    fn liquid_status_bounds_and_retry_cadence() {
        let mut bytes = liquid_status_request();
        for (raw, expected) in [(100, Some(1.)), (9999, Some(99.99)), (10000, None)] {
            bytes[5..7].copy_from_slice(&u16::to_le_bytes(raw));
            assert_eq!(parse_liquid(&bytes).ok(), expected);
        }
        assert!(parse_liquid(&bytes[..440]).is_err());
        bytes[1] = 0;
        assert!(parse_liquid(&bytes).is_err());
        let mut s = Sources::default();
        assert!(s.liquid_due(1000));
        s.attempted_liquid(1000);
        assert!(!s.liquid_due(2999));
        assert!(s.liquid_due(3000));
        assert!(!s.liquid_due(999));
    }
    #[test]
    fn invalid_ram_selection_preserves_previous_configuration() {
        let mut sources = Sources::with_ram_ids(vec!["A".into(), "B".into()]).unwrap();
        assert!(sources.set_ram_ids(vec!["A".into(), "A".into()]).is_err());
        assert_eq!(sources.ram_ids(), &["A".to_owned(), "B".to_owned()]);
        assert!(sources.set_ram_ids(vec![String::new()]).is_err());
        sources.set_ram_ids(Vec::new()).unwrap();
        assert!(sources.ram_ids().is_empty());
    }
}
