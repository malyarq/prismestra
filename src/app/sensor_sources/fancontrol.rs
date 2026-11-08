//! FanControl cached temperature RPC only. A response time is not an acquisition timestamp.
#[cfg(windows)]
use crate::app::sensor_worker::Factory;
use crate::{
    app::sensor_worker::Provider, lcd::temperatures, runtime::output::Cancellation,
    sensors::Reading,
};
type Fetch = Box<dyn FnMut() -> Result<Vec<temperatures::Reading>, String>>;
struct Cached {
    sensor_id: String,
    cancel: Cancellation,
    fetch: Fetch,
}
impl Provider for Cached {
    fn poll_interval_ms(&self) -> u64 {
        2000
    }
    fn poll(&mut self, now_ms: u64) -> Result<Option<Reading>, String> {
        if self.cancel.cancelled() {
            return Err("FanControl cache reading cancelled".into());
        }
        let snapshot = (self.fetch)()?;
        if self.cancel.cancelled() {
            return Err("FanControl cache reading cancelled".into());
        }
        let mut matching = snapshot.into_iter().filter(|r| r.id == self.sensor_id);
        let reading = matching.next();
        if matching.next().is_some() {
            return Err("Ambiguous FanControl temperature ID".into());
        }
        reading
            .map(|r| {
                if !r.celsius.is_finite() || !(-20.0..=150.0).contains(&r.celsius) {
                    return Err("Invalid FanControl cached temperature".into());
                }
                Ok(Reading {
                    celsius: r.celsius,
                    observed_ms: now_ms,
                })
            })
            .transpose()
    }
}
#[cfg(windows)]
pub(super) fn factory(
    pipe_name: &str,
    expected_server: &str,
    sensor_id: &str,
) -> Result<Factory, String> {
    use crate::{app::sensor_bindings::Target, lcd::fancontrol_pipe::Policy};
    Target::FanControl {
        pipe_name: pipe_name.into(),
        expected_server: expected_server.into(),
        sensor_id: sensor_id.into(),
    }
    .validate()?;
    let pipe = pipe_name.to_owned();
    let server = std::path::PathBuf::from(expected_server);
    let id = sensor_id.to_owned();
    Ok(std::sync::Arc::new(move |cancel| {
        if cancel.cancelled() {
            return Err("FanControl activation cancelled".into());
        }
        // No pipe is opened until the worker polls, after Registry lease/preflight.
        let policy = Policy::new(&pipe, server.clone(), 2500)?;
        Ok(Box::new(Cached {
            sensor_id: id.clone(),
            cancel,
            fetch: Box::new(move || crate::lcd::fancontrol_pipe::read_temperatures(&policy)),
        }))
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn reading(id: &str, value: f64) -> temperatures::Reading {
        temperatures::Reading {
            id: id.into(),
            name: "cached".into(),
            origin: "fixture".into(),
            celsius: value,
        }
    }
    #[test]
    fn exact_id_missing_duplicate_invalid_and_cancellation_never_guess_a_temperature() {
        let cancel = Cancellation::default();
        let mut provider = Cached {
            sensor_id: "cpu".into(),
            cancel: cancel.clone(),
            fetch: Box::new(|| Ok(vec![reading("cpu", 42.0), reading("gpu", 80.0)])),
        };
        assert_eq!(provider.poll_interval_ms(), 2000);
        assert_eq!(
            provider.poll(1234).unwrap(),
            Some(Reading {
                celsius: 42.0,
                observed_ms: 1234
            })
        );
        provider.fetch = Box::new(|| Ok(vec![reading("gpu", 80.0)]));
        assert!(provider.poll(1235).unwrap().is_none());
        provider.fetch = Box::new(|| Ok(vec![reading("cpu", 40.0), reading("cpu", 41.0)]));
        assert!(provider.poll(1236).is_err());
        provider.fetch = Box::new(|| Ok(vec![reading("cpu", f64::NAN)]));
        assert!(provider.poll(1237).is_err());
        cancel.cancel_for_test();
        provider.fetch = Box::new(|| panic!("cancelled reader performed RPC"));
        assert!(provider.poll(1238).is_err());
    }
    #[test]
    fn cancellation_during_cache_fetch_discards_the_reply() {
        let cancel = Cancellation::default();
        let during = cancel.clone();
        let mut provider = Cached {
            sensor_id: "cpu".into(),
            cancel,
            fetch: Box::new(move || {
                during.cancel_for_test();
                Ok(vec![reading("cpu", 42.0)])
            }),
        };
        assert!(provider.poll(1234).is_err());
    }
}
