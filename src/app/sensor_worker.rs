//! Read-only sensor ownership. Factories run only after an explicit start;
//! portable configuration contains neither these factories nor endpoint keys.
pub use crate::runtime::output::Phase;
use crate::{
    runtime::output::{Cancellation, Lease},
    sensors::Reading,
};
use std::{
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::Duration,
};

pub trait Provider {
    fn poll(&mut self, now_ms: u64) -> Result<Option<Reading>, String>;
    /// Minimum idle time after an attempt completes, including failed attempts.
    /// Read once on the owner thread before any poll; permitted range 100..=60000.
    fn poll_interval_ms(&self) -> u64 {
        1000
    }
}
pub type Factory = Arc<dyn Fn(Cancellation) -> Result<Box<dyn Provider>, String> + Send + Sync>;

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub phase: Phase,
    pub generation: u64,
    /// Advances only when a snapshot field changes; never a physical ACK.
    pub revision: u64,
    /// Advances on every observed loss, even if a consumer misses the unavailable snapshot.
    pub availability_epoch: u64,
    pub polls: u64,
    pub reading: Option<Reading>,
    pub error: Option<String>,
}
struct Shared {
    snapshot: Mutex<Snapshot>,
    wake: Condvar,
    cancel: Cancellation,
}
pub struct Worker {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}
fn message(error: String) -> String {
    error.chars().take(512).collect()
}
fn validate_reading(r: Reading, now: u64) -> Result<Reading, String> {
    if r.observed_ms == 0 || r.observed_ms > now {
        return Err("Invalid sensor observation timestamp".into());
    }
    if !r.celsius.is_finite() || !(-20.0..=150.0).contains(&r.celsius) {
        return Err("Invalid sensor temperature".into());
    }
    Ok(r)
}
fn update(shared: &Shared, edit: impl FnOnce(&mut Snapshot)) {
    let mut snapshot = shared.snapshot.lock().unwrap_or_else(|e| e.into_inner());
    let before = snapshot.clone();
    edit(&mut snapshot);
    if before.phase == Phase::Ready
        && before.reading.is_some()
        && (snapshot.phase != Phase::Ready || snapshot.reading.is_none())
    {
        if let Some(epoch) = snapshot.availability_epoch.checked_add(1) {
            snapshot.availability_epoch = epoch;
        } else {
            snapshot.phase = Phase::Failed;
            snapshot.reading = None;
            snapshot.error = Some("Sensor availability epoch exhausted".into());
            shared.cancel.cancel();
        }
    }
    if *snapshot != before {
        snapshot.revision = snapshot.revision.saturating_add(1);
        shared.wake.notify_all();
    }
}
impl Worker {
    pub fn start(generation: u64, factory: Factory) -> Result<Self, String> {
        Self::start_claimed(generation, vec![], factory)
    }
    pub(crate) fn start_claimed(
        generation: u64,
        keys: Vec<String>,
        factory: Factory,
    ) -> Result<Self, String> {
        Self::start_inner(generation, keys, factory, None)
    }
    fn start_inner(
        generation: u64,
        keys: Vec<String>,
        factory: Factory,
        test_interval_override: Option<Duration>,
    ) -> Result<Self, String> {
        if generation == 0 {
            return Err("Sensor generation must be nonzero".into());
        }
        let lease = Lease::acquire(keys)?;
        let shared = Arc::new(Shared {
            snapshot: Mutex::new(Snapshot {
                phase: Phase::Connecting,
                generation,
                revision: 1,
                availability_epoch: 1,
                polls: 0,
                reading: None,
                error: None,
            }),
            wake: Condvar::new(),
            cancel: Cancellation::default(),
        });
        let owner = shared.clone();
        let thread = std::thread::Builder::new()
            .name("read-only-sensor".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(&owner, factory, test_interval_override)
                }));
                // Provider is destroyed by run/unwind before releasing its actual endpoint.
                drop(lease);
                update(&owner, |s| {
                    s.reading = None;
                    match result {
                        Ok(Ok(())) => s.phase = Phase::Stopped,
                        Ok(Err(error)) => {
                            s.phase = if owner.cancel.cancelled() {
                                Phase::Stopped
                            } else {
                                Phase::Failed
                            };
                            s.error = Some(message(error));
                        }
                        Err(_) => {
                            s.phase = Phase::Failed;
                            s.error = Some("Sensor worker panicked".into());
                        }
                    }
                });
                owner.cancel.cancel();
                owner.wake.notify_all();
            })
            .map_err(|e| format!("Start sensor worker: {e}"))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }
    pub fn snapshot(&self) -> Snapshot {
        self.shared
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn stop(&self) {
        self.shared.cancel.cancel();
        update(&self.shared, |s| {
            s.reading = None;
            if matches!(s.phase, Phase::Connecting | Phase::Ready) {
                s.phase = Phase::Stopping;
            }
        });
        self.shared.wake.notify_all();
    }
    /// True only once the actual thread has destroyed its provider and exited.
    pub fn drained(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }
    pub fn reap(&mut self) -> bool {
        if self.thread.as_ref().is_some_and(|t| t.is_finished())
            && let Some(t) = self.thread.take()
        {
            let _ = t.join();
        }
        self.thread.is_none()
    }
    pub fn is_reaped(&self) -> bool {
        self.thread.is_none()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
        let _ = self.reap();
    }
}
fn run(
    shared: &Shared,
    factory: Factory,
    test_interval_override: Option<Duration>,
) -> Result<(), String> {
    if shared.cancel.cancelled() {
        return Ok(());
    }
    let mut provider = factory(shared.cancel.clone())?;
    if shared.cancel.cancelled() {
        return Ok(());
    }
    let interval_ms = provider.poll_interval_ms();
    if !(100..=60_000).contains(&interval_ms) {
        return Err("Sensor poll interval must be 100..=60000 ms".into());
    }
    let interval = Duration::from_millis(interval_ms);
    // Production has no clock/rate override. Fast cadence is private test-only input.
    #[cfg(test)]
    let interval = test_interval_override.unwrap_or(interval);
    #[cfg(not(test))]
    let _ = test_interval_override;
    update(shared, |s| {
        if !shared.cancel.cancelled() {
            s.phase = Phase::Ready;
        }
    });
    loop {
        // The snapshot lock also orders stop against publishing an in-flight result.
        {
            let mut s = shared.snapshot.lock().unwrap_or_else(|e| e.into_inner());
            if shared.cancel.cancelled() {
                return Ok(());
            }
            s.polls = s.polls.saturating_add(1);
            s.revision = s.revision.saturating_add(1);
            shared.wake.notify_all();
        }
        let now = crate::clock::monotonic_ms();
        let result = provider
            .poll(now)
            .and_then(|r| r.map(|r| validate_reading(r, now)).transpose());
        update(shared, |s| {
            if shared.cancel.cancelled() {
                s.reading = None;
                return;
            }
            match result {
                Ok(Some(r)) => {
                    s.reading = Some(r);
                    s.error = None;
                }
                Ok(None) => {
                    s.reading = None;
                    s.error = None;
                }
                Err(error) => {
                    s.reading = None;
                    s.error = Some(message(error));
                }
            }
        });
        let s = shared.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        if shared.cancel.cancelled() {
            return Ok(());
        }
        // Spurious wakes never increase the rate. Slow I/O also gets a full idle interval.
        let (_guard, _) = shared
            .wake
            .wait_timeout_while(s, interval, |_| !shared.cancel.cancelled())
            .unwrap_or_else(|e| e.into_inner());
        if shared.cancel.cancelled() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        rc::Rc,
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        time::Instant,
    };
    fn wait(worker: &Worker, predicate: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let s = worker
            .shared
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (s, t) = worker
            .shared
            .wake
            .wait_timeout_while(s, Duration::from_secs(3), |s| !predicate(s))
            .unwrap_or_else(|e| e.into_inner());
        assert!(!t.timed_out() || predicate(&s), "timed out: {s:?}");
        s.clone()
    }
    fn reap(worker: &mut Worker) {
        wait(worker, |s| {
            matches!(s.phase, Phase::Stopped | Phase::Failed)
        });
        if let Some(t) = worker.thread.take() {
            t.join().unwrap();
        }
        assert!(worker.is_reaped());
    }
    struct Bound {
        owner: std::thread::ThreadId,
        not_send: Rc<Cell<usize>>,
        drop_tx: mpsc::Sender<std::thread::ThreadId>,
    }
    impl Provider for Bound {
        fn poll(&mut self, now: u64) -> Result<Option<Reading>, String> {
            assert_eq!(self.owner, std::thread::current().id());
            self.not_send.set(self.not_send.get() + 1);
            Ok(Some(Reading {
                celsius: 42.0,
                observed_ms: now,
            }))
        }
    }
    impl Drop for Bound {
        fn drop(&mut self) {
            assert_eq!(self.owner, std::thread::current().id());
            let _ = self.drop_tx.send(self.owner);
        }
    }
    #[test]
    fn non_send_provider_lives_and_dies_on_owner_and_idle_does_not_poll() {
        let (tx, rx) = mpsc::channel();
        let caller = std::thread::current().id();
        let factory: Factory = Arc::new(move |_| {
            Ok(Box::new(Bound {
                owner: std::thread::current().id(),
                not_send: Rc::new(Cell::new(0)),
                drop_tx: tx.clone(),
            }))
        });
        let mut w = Worker::start(1, factory).unwrap();
        let s = wait(&w, |s| s.reading.is_some());
        assert_eq!(s.polls, 1);
        let state = w.shared.snapshot.lock().unwrap();
        let (state, _) = w
            .shared
            .wake
            .wait_timeout_while(state, Duration::from_millis(30), |s| s.polls == 1)
            .unwrap();
        assert_eq!(state.polls, 1);
        drop(state);
        w.stop();
        assert!(w.snapshot().reading.is_none());
        reap(&mut w);
        assert_ne!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), caller);
    }
    struct Blocked {
        entered: mpsc::Sender<()>,
        gate: Arc<(Mutex<bool>, Condvar)>,
        dropped: mpsc::Sender<()>,
    }
    impl Provider for Blocked {
        fn poll(&mut self, now: u64) -> Result<Option<Reading>, String> {
            self.entered.send(()).unwrap();
            let (m, c) = &*self.gate;
            let g = m.lock().unwrap();
            drop(c.wait_while(g, |open| !*open).unwrap());
            Ok(Some(Reading {
                celsius: 35.0,
                observed_ms: now,
            }))
        }
    }
    impl Drop for Blocked {
        fn drop(&mut self) {
            let _ = self.dropped.send(());
        }
    }
    #[test]
    fn drop_keeps_lease_until_blocking_io_and_provider_destroyed() {
        let (entered_tx, entered) = mpsc::channel();
        let (drop_tx, dropped) = mpsc::channel();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let g = gate.clone();
        let key = "sensor-worker-blocked-drop".to_string();
        let factory: Factory = Arc::new(move |_| {
            Ok(Box::new(Blocked {
                entered: entered_tx.clone(),
                gate: g.clone(),
                dropped: drop_tx.clone(),
            }))
        });
        let w = Worker::start_claimed(2, vec![key.clone()], factory).unwrap();
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        w.stop();
        assert!(w.snapshot().reading.is_none());
        assert!(!w.drained());
        assert!(Lease::acquire(vec![key.clone()]).is_err());
        let shared = w.shared.clone();
        let start = Instant::now();
        drop(w);
        assert!(start.elapsed() < Duration::from_millis(100));
        assert!(Lease::acquire(vec![key.clone()]).is_err());
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        dropped.recv_timeout(Duration::from_secs(2)).unwrap();
        let s = shared.snapshot.lock().unwrap();
        let (s, t) = shared
            .wake
            .wait_timeout_while(s, Duration::from_secs(2), |s| s.phase != Phase::Stopped)
            .unwrap();
        assert!(!t.timed_out());
        assert!(s.reading.is_none());
        drop(s);
        drop(Lease::acquire(vec![key]).unwrap());
    }
    #[test]
    fn factory_failure_and_panic_are_terminal_without_recreation() {
        for panic in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let c = calls.clone();
            let factory: Factory = Arc::new(move |_| {
                c.fetch_add(1, Ordering::SeqCst);
                if panic {
                    panic!("synthetic factory panic");
                }
                Err("synthetic factory error".into())
            });
            let mut w = Worker::start(3, factory).unwrap();
            let s = wait(&w, |s| s.phase == Phase::Failed);
            assert!(s.error.is_some());
            assert_eq!(s.polls, 0);
            reap(&mut w);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }
    struct Script {
        next: usize,
        step: mpsc::Sender<usize>,
        advance: Arc<(Mutex<usize>, Condvar)>,
    }
    impl Provider for Script {
        fn poll(&mut self, now: u64) -> Result<Option<Reading>, String> {
            let i = self.next;
            self.next += 1;
            self.step.send(i).unwrap();
            let (m, c) = &*self.advance;
            let g = m.lock().unwrap();
            drop(c.wait_while(g, |n| *n <= i).unwrap());
            match i {
                0 => Ok(Some(Reading {
                    celsius: 30.0,
                    observed_ms: now,
                })),
                1 => Err("read error".into()),
                2 => Ok(Some(Reading {
                    celsius: 31.0,
                    observed_ms: 1,
                })),
                3 => Ok(None),
                4 => Ok(Some(Reading {
                    celsius: 151.0,
                    observed_ms: now,
                })),
                5 => Ok(Some(Reading {
                    celsius: 40.0,
                    observed_ms: now + 1,
                })),
                _ => panic!("synthetic poll panic"),
            }
        }
    }
    #[test]
    fn read_failures_recover_missing_clears_and_cached_timestamp_is_not_freshened() {
        let (tx, rx) = mpsc::channel();
        let advance = Arc::new((Mutex::new(0), Condvar::new()));
        let a = advance.clone();
        let factory: Factory = Arc::new(move |_| {
            Ok(Box::new(Script {
                next: 0,
                step: tx.clone(),
                advance: a.clone(),
            }))
        });
        let mut w =
            Worker::start_inner(4, vec![], factory, Some(Duration::from_millis(5))).unwrap();
        for i in 0..7 {
            assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), i);
            *advance.0.lock().unwrap() = i + 1;
            advance.1.notify_all();
            let s = wait(&w, |s| match i {
                0 => s.reading.is_some(),
                1 => s.error.as_deref() == Some("read error"),
                2 => s.reading.is_some_and(|r| r.celsius == 31.0),
                3 => s.reading.is_none() && s.error.is_none(),
                4 => s.error.as_deref() == Some("Invalid sensor temperature"),
                5 => s.error.as_deref() == Some("Invalid sensor observation timestamp"),
                _ => s.phase == Phase::Failed,
            });
            if i == 2 {
                let r = s.reading.unwrap();
                assert_eq!(r.observed_ms, 1);
                assert!(!r.is_fresh(10_000, 1000));
            }
            if i == 1 || (3..=5).contains(&i) {
                assert!(s.reading.is_none());
            }
        }
        reap(&mut w);
        assert!(w.snapshot().reading.is_none());
    }
    #[test]
    fn readings_keep_original_time_and_validate_finite_range_and_nonfuture_time() {
        for celsius in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -20.01, 150.01] {
            assert!(
                validate_reading(
                    Reading {
                        celsius,
                        observed_ms: 1
                    },
                    10
                )
                .is_err()
            );
        }
        for observed_ms in [0, 11] {
            assert!(
                validate_reading(
                    Reading {
                        celsius: 30.0,
                        observed_ms
                    },
                    10
                )
                .is_err()
            );
        }
        for celsius in [-20.0, 150.0] {
            let r = Reading {
                celsius,
                observed_ms: 1,
            };
            assert_eq!(validate_reading(r, 10).unwrap(), r);
        }
        assert!(Worker::start(0, Arc::new(|_| Err("must not run".into()))).is_err());
    }
    struct Cadenced {
        interval: u64,
        interval_reads: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
        attempts: mpsc::Sender<Instant>,
    }
    impl Provider for Cadenced {
        fn poll_interval_ms(&self) -> u64 {
            self.interval_reads.fetch_add(1, Ordering::SeqCst);
            self.interval
        }
        fn poll(&mut self, _: u64) -> Result<Option<Reading>, String> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
            self.attempts.send(Instant::now()).unwrap();
            if attempt == 0 {
                Err("synthetic failed attempt".into())
            } else {
                Ok(None)
            }
        }
    }
    #[test]
    fn provider_two_second_cadence_is_read_once_and_honored_after_failure() {
        let (tx, rx) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let intervals = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let i = intervals.clone();
        let factory: Factory = Arc::new(move |_| {
            Ok(Box::new(Cadenced {
                interval: 2000,
                interval_reads: i.clone(),
                calls: c.clone(),
                attempts: tx.clone(),
            }))
        });
        let mut w = Worker::start(5, factory).unwrap();
        let first = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        wait(&w, |s| {
            s.error.as_deref() == Some("synthetic failed attempt")
        });
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(1100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let second = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(second.duration_since(first) >= Duration::from_millis(2000));
        assert_eq!(intervals.load(Ordering::SeqCst), 1);
        w.stop();
        reap(&mut w);
    }
    #[test]
    fn default_interval_is_one_second_and_invalid_intervals_never_poll() {
        let (tx, _) = mpsc::channel();
        let default = Bound {
            owner: std::thread::current().id(),
            not_send: Rc::new(Cell::new(0)),
            drop_tx: tx,
        };
        assert_eq!(default.poll_interval_ms(), 1000);
        for interval in [0, 99, 60_001, u64::MAX] {
            let (tx, _) = mpsc::channel();
            let calls = Arc::new(AtomicUsize::new(0));
            let intervals = Arc::new(AtomicUsize::new(0));
            let c = calls.clone();
            let i = intervals.clone();
            let factory: Factory = Arc::new(move |_| {
                Ok(Box::new(Cadenced {
                    interval,
                    interval_reads: i.clone(),
                    calls: c.clone(),
                    attempts: tx.clone(),
                }))
            });
            let mut w = Worker::start(6, factory).unwrap();
            let s = wait(&w, |s| s.phase == Phase::Failed);
            assert_eq!(
                s.error.as_deref(),
                Some("Sensor poll interval must be 100..=60000 ms")
            );
            assert_eq!(s.polls, 0);
            assert!(s.reading.is_none());
            reap(&mut w);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(intervals.load(Ordering::SeqCst), 1);
        }
    }
}
