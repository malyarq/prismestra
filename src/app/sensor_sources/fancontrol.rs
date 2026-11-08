//! FanControl cached temperature RPC only. A response time is not an acquisition timestamp.
#[cfg(windows)]
use crate::app::sensor_worker::Factory;
use crate::{
    app::sensor_worker::Provider, lcd::temperatures, runtime::output::Cancellation,
    sensors::Reading,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, Weak},
    time::{Duration, Instant},
};
const SNAPSHOT_TTL: Duration = Duration::from_millis(2000);
const WAIT_LIMIT: Duration = Duration::from_millis(2500);
type Fetch = Box<dyn FnMut() -> Result<Vec<temperatures::Reading>, String> + Send>;
type EndpointPool = Mutex<HashMap<(String, PathBuf), Weak<Shared>>>;

#[derive(Clone)]
struct Snapshot {
    observed_ms: u64,
    reusable_since: Instant,
    result: Result<Arc<Vec<temperatures::Reading>>, String>,
}
impl Snapshot {
    fn reusable(&self, now_ms: u64) -> bool {
        self.reusable_since.elapsed() < SNAPSHOT_TTL
            && (self.result.is_err()
                || now_ms.saturating_sub(self.observed_ms) < SNAPSHOT_TTL.as_millis() as u64)
    }
}
struct State {
    // None while the sole RPC is in flight; waiting consumers never open another pipe.
    fetch: Option<Fetch>,
    snapshot: Option<Snapshot>,
}
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}
impl Shared {
    fn new(fetch: Fetch) -> Self {
        Self {
            state: Mutex::new(State {
                fetch: Some(fetch),
                snapshot: None,
            }),
            wake: Condvar::new(),
        }
    }
    fn snapshot(&self, now_ms: u64, cancel: &Cancellation) -> Result<Snapshot, String> {
        let wait_started = Instant::now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| "FanControl snapshot lock poisoned")?;
        loop {
            if cancel.cancelled() {
                return Err("FanControl cache reading cancelled".into());
            }
            if let Some(snapshot) = state.snapshot.as_ref().filter(|s| s.reusable(now_ms)) {
                return Ok(snapshot.clone());
            }
            if let Some(mut fetch) = state.fetch.take() {
                let started = Instant::now();
                drop(state);
                // A cancelled caller still completes/publishes this read for other consumers.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&mut fetch))
                    .unwrap_or_else(|_| Err("FanControl cache query panicked".into()))
                    .and_then(|values| {
                        if started.elapsed() >= SNAPSHOT_TTL {
                            Err("FanControl snapshot expired during query".into())
                        } else {
                            Ok(Arc::new(values))
                        }
                    });
                let snapshot = Snapshot {
                    observed_ms: now_ms,
                    // Failed reads have no valid observation; coalesce their error for one TTL.
                    reusable_since: if result.is_ok() {
                        started
                    } else {
                        Instant::now()
                    },
                    result,
                };
                state = self
                    .state
                    .lock()
                    .map_err(|_| "FanControl snapshot lock poisoned")?;
                state.fetch = Some(fetch);
                state.snapshot = Some(snapshot.clone());
                self.wake.notify_all();
                return Ok(snapshot);
            }
            let left = WAIT_LIMIT
                .checked_sub(wait_started.elapsed())
                .ok_or("FanControl snapshot wait timeout")?;
            state = self
                .wake
                .wait_timeout(state, left.min(Duration::from_millis(10)))
                .map_err(|_| "FanControl snapshot lock poisoned")?
                .0;
        }
    }
}

fn shared_endpoint(
    pool: &EndpointPool,
    pipe: &str,
    server: PathBuf,
    fetch: Fetch,
) -> Result<Arc<Shared>, String> {
    let mut pool = pool
        .lock()
        .map_err(|_| "FanControl endpoint pool poisoned")?;
    pool.retain(|_, endpoint| endpoint.strong_count() > 0);
    // Match Target::canonical_key without changing the Policy used for verification.
    let server_key = server
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    let key = (pipe.to_ascii_lowercase(), PathBuf::from(server_key));
    if let Some(existing) = pool.get(&key).and_then(Weak::upgrade) {
        return Ok(existing);
    }
    let endpoint = Arc::new(Shared::new(fetch));
    pool.insert(key, Arc::downgrade(&endpoint));
    Ok(endpoint)
}

struct Cached {
    sensor_id: String,
    cancel: Cancellation,
    shared: Arc<Shared>,
}
impl Provider for Cached {
    fn poll_interval_ms(&self) -> u64 {
        2000
    }
    fn poll(&mut self, now_ms: u64) -> Result<Option<Reading>, String> {
        if self.cancel.cancelled() {
            return Err("FanControl cache reading cancelled".into());
        }
        let snapshot = self.shared.snapshot(now_ms, &self.cancel)?;
        if self.cancel.cancelled() {
            return Err("FanControl cache reading cancelled".into());
        }
        let values = snapshot.result?;
        let mut matching = values.iter().filter(|r| r.id == self.sensor_id);
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
                    // A worker timestamps before waiting; never advance a shared observation.
                    observed_ms: snapshot.observed_ms.min(now_ms),
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
        static ENDPOINTS: std::sync::OnceLock<EndpointPool> = std::sync::OnceLock::new();
        let shared = shared_endpoint(
            ENDPOINTS.get_or_init(|| Mutex::new(HashMap::new())),
            &pipe,
            server.clone(),
            Box::new(move || crate::lcd::fancontrol_pipe::read_temperatures(&policy)),
        )?;
        Ok(Box::new(Cached {
            sensor_id: id.clone(),
            cancel,
            shared,
        }))
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Barrier,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    fn reading(id: &str, value: f64) -> temperatures::Reading {
        temperatures::Reading {
            id: id.into(),
            name: "cached".into(),
            origin: "fixture".into(),
            celsius: value,
        }
    }
    fn shared(
        fetch: impl FnMut() -> Result<Vec<temperatures::Reading>, String> + Send + 'static,
    ) -> Arc<Shared> {
        Arc::new(Shared::new(Box::new(fetch)))
    }
    #[test]
    fn exact_id_missing_duplicate_invalid_and_cancellation_never_guess_a_temperature() {
        let cancel = Cancellation::default();
        let mut provider = Cached {
            sensor_id: "cpu".into(),
            cancel: cancel.clone(),
            shared: shared(|| Ok(vec![reading("cpu", 42.0), reading("gpu", 80.0)])),
        };
        assert_eq!(provider.poll_interval_ms(), 2000);
        assert_eq!(
            provider.poll(1234).unwrap(),
            Some(Reading {
                celsius: 42.0,
                observed_ms: 1234
            })
        );
        provider.shared = shared(|| Ok(vec![reading("gpu", 80.0)]));
        assert!(provider.poll(1235).unwrap().is_none());
        provider.shared = shared(|| Ok(vec![reading("cpu", 40.0), reading("cpu", 41.0)]));
        assert!(provider.poll(1236).is_err());
        provider.shared = shared(|| Ok(vec![reading("cpu", f64::NAN)]));
        assert!(provider.poll(1237).is_err());
        cancel.cancel_for_test();
        provider.shared = shared(|| panic!("cancelled reader performed RPC"));
        assert!(provider.poll(1238).is_err());
    }
    #[test]
    fn cancellation_during_cache_fetch_discards_the_reply() {
        let cancel = Cancellation::default();
        let during = cancel.clone();
        let mut provider = Cached {
            sensor_id: "cpu".into(),
            cancel,
            shared: shared(move || {
                during.cancel_for_test();
                Ok(vec![reading("cpu", 42.0)])
            }),
        };
        assert!(provider.poll(1234).is_err());
    }

    #[test]
    fn four_logical_sensors_share_one_endpoint_query_and_original_observation() {
        let pool = Mutex::new(HashMap::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(5));
        let mut threads = vec![];
        for (id, value) in [("cpu", 42.), ("gpu", 52.), ("dimm1", 32.), ("dimm3", 33.)] {
            let calls = calls.clone();
            let source = shared_endpoint(
                &pool,
                "pipe",
                PathBuf::from("owner.exe"),
                Box::new(move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(vec![
                        reading("cpu", 42.),
                        reading("gpu", 52.),
                        reading("dimm1", 32.),
                        reading("dimm3", 33.),
                    ])
                }),
            )
            .unwrap();
            let start = start.clone();
            threads.push(std::thread::spawn(move || {
                let mut provider = Cached {
                    sensor_id: id.into(),
                    cancel: Cancellation::default(),
                    shared: source,
                };
                start.wait();
                assert_eq!(
                    provider.poll(1000).unwrap(),
                    Some(Reading {
                        celsius: value,
                        observed_ms: 1000
                    })
                );
                assert_eq!(provider.poll(1500).unwrap().unwrap().observed_ms, 1000);
            }));
        }
        start.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ttl_refreshes_once_and_a_failed_refresh_never_returns_the_old_temperature() {
        let calls = Arc::new(AtomicUsize::new(0));
        let fetch_calls = calls.clone();
        let source = shared(move || match fetch_calls.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(vec![reading("cpu", 42.)]),
            1 => Err("verified endpoint read failed".into()),
            _ => Ok(vec![reading("cpu", 43.)]),
        });
        let mut provider = Cached {
            sensor_id: "cpu".into(),
            cancel: Cancellation::default(),
            shared: source.clone(),
        };
        assert_eq!(provider.poll(1000).unwrap().unwrap().observed_ms, 1000);
        assert_eq!(provider.poll(2999).unwrap().unwrap().observed_ms, 1000);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            provider
                .poll(3000)
                .unwrap_err()
                .contains("endpoint read failed")
        );
        assert!(
            provider
                .poll(3001)
                .unwrap_err()
                .contains("endpoint read failed")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        source
            .state
            .lock()
            .unwrap()
            .snapshot
            .as_mut()
            .unwrap()
            .reusable_since = Instant::now() - SNAPSHOT_TTL;
        assert_eq!(
            provider.poll(5000).unwrap(),
            Some(Reading {
                celsius: 43.,
                observed_ms: 5000
            })
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn cancelled_rpc_owner_still_publishes_for_another_live_consumer() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let source = shared(move || {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            Ok(vec![reading("cpu", 42.), reading("gpu", 52.)])
        });
        let cancel = Cancellation::default();
        let mut owner = Cached {
            sensor_id: "cpu".into(),
            cancel: cancel.clone(),
            shared: source.clone(),
        };
        let owner = std::thread::spawn(move || owner.poll(1000));
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        cancel.cancel_for_test();
        let mut other = Cached {
            sensor_id: "gpu".into(),
            cancel: Cancellation::default(),
            shared: source,
        };
        let other = std::thread::spawn(move || other.poll(1001));
        release_tx.send(()).unwrap();
        assert!(owner.join().unwrap().is_err());
        assert_eq!(
            other.join().unwrap().unwrap(),
            Some(Reading {
                celsius: 52.,
                observed_ms: 1000
            })
        );
    }

    #[test]
    fn cancelled_waiter_leaves_the_in_flight_read_available_to_its_owner() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let source = shared(move || {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            Ok(vec![reading("cpu", 42.)])
        });
        let mut owner = Cached {
            sensor_id: "cpu".into(),
            cancel: Cancellation::default(),
            shared: source.clone(),
        };
        let owner = std::thread::spawn(move || owner.poll(1000));
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let cancel = Cancellation::default();
        let mut waiter = Cached {
            sensor_id: "cpu".into(),
            cancel: cancel.clone(),
            shared: source,
        };
        let (waiting_tx, waiting_rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            waiting_tx.send(()).unwrap();
            waiter.poll(1001)
        });
        waiting_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        cancel.cancel_for_test();
        assert!(waiter.join().unwrap().is_err());
        assert!(!owner.is_finished());
        release_tx.send(()).unwrap();
        assert_eq!(owner.join().unwrap().unwrap().unwrap().celsius, 42.);
    }

    #[test]
    fn endpoint_case_aliases_share_one_blocked_query_but_different_servers_do_not() {
        let pool = Mutex::new(HashMap::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let fetch_calls = calls.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first = shared_endpoint(
            &pool,
            r"\\.\pipe\FanControl",
            PathBuf::from(r"C:\Tools\FanControl.exe"),
            Box::new(move || {
                fetch_calls.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                Ok(vec![reading("cpu", 42.), reading("gpu", 52.)])
            }),
        )
        .unwrap();
        let same = shared_endpoint(
            &pool,
            r"\\.\pipe\FANCONTROL",
            PathBuf::from("c:/TOOLS/fancontrol.exe"),
            Box::new(|| panic!("reused endpoint performed second RPC")),
        )
        .unwrap();
        let different = shared_endpoint(
            &pool,
            r"\\.\pipe\FanControl",
            PathBuf::from(r"C:\Other\FanControl.exe"),
            Box::new(|| Ok(vec![])),
        )
        .unwrap();
        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &different));
        let mut cpu = Cached {
            sensor_id: "cpu".into(),
            cancel: Cancellation::default(),
            shared: first,
        };
        let cpu = std::thread::spawn(move || cpu.poll(1000));
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let mut gpu = Cached {
            sensor_id: "gpu".into(),
            cancel: Cancellation::default(),
            shared: same,
        };
        let gpu = std::thread::spawn(move || gpu.poll(1001));
        assert!(!cpu.is_finished());
        release_tx.send(()).unwrap();
        assert_eq!(cpu.join().unwrap().unwrap().unwrap().celsius, 42.);
        assert_eq!(
            gpu.join().unwrap().unwrap().unwrap(),
            Reading {
                celsius: 52.,
                observed_ms: 1000
            }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
