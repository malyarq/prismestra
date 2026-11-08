//! Bounded latest-frame mailbox for blocking RGB transports. A driver is
//! constructed, used and destroyed on its own thread; rendering never waits for I/O.
use crate::{devices::Driver, lighting::color::Rgb};
use std::{
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

static ENDPOINTS: OnceLock<Mutex<std::collections::BTreeSet<String>>> = OnceLock::new();
pub(crate) struct Lease(Vec<String>);
impl Lease {
    pub(crate) fn acquire(keys: Vec<String>) -> Result<Self, String> {
        if keys.len() > 4
            || keys.iter().any(|k| k.is_empty() || k.len() > 4100)
            || keys.iter().collect::<std::collections::BTreeSet<_>>().len() != keys.len()
        {
            return Err("Invalid output endpoint lease".into());
        }
        let mut active = ENDPOINTS
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if keys.iter().any(|k| active.contains(k)) {
            return Err("Hardware endpoint is still owned by another output".into());
        }
        active.extend(keys.iter().cloned());
        Ok(Self(keys))
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut active = ENDPOINTS
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for key in &self.0 {
            active.remove(key);
        }
    }
}

#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    #[cfg(test)]
    pub(crate) fn cancel_for_test(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub generation: u64,
    pub sequence: u64,
}
pub enum WriteResult {
    Submitted,
    Deferred(Duration),
}
pub trait RgbDriver {
    fn protocol(&self) -> Driver;
    fn leds(&self) -> usize;
    /// Submitted is a transport completion, never a physical LED acknowledgement.
    fn write(&mut self, colors: &[Rgb], cancel: &Cancellation) -> Result<WriteResult, String>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Connecting,
    Ready,
    Stopping,
    Stopped,
    Failed,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub phase: Phase,
    pub generation: u64,
    pub received: u64,
    pub replaced: u64,
    pub submitted: u64,
    pub deferred: u64,
    pub last_submitted: Option<Ticket>,
    pub error: Option<String>,
}
struct Frame {
    ticket: Ticket,
    colors: Vec<Rgb>,
}
struct State {
    pending: Option<Frame>,
    last_sequence: u64,
    snapshot: Snapshot,
}
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    cancel: Cancellation,
}
pub struct Output {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    leds: usize,
}
impl Output {
    /// This is the only function here that starts work. Factory may connect a
    /// device; pass fake factories on a stand. No driver is opened at import.
    pub fn start<F>(
        protocol: Driver,
        leds: usize,
        generation: u64,
        factory: F,
    ) -> Result<Self, String>
    where
        F: FnOnce(Cancellation) -> Result<Box<dyn RgbDriver>, String> + Send + 'static,
    {
        Self::start_claimed(protocol, leds, generation, Vec::new(), factory)
    }
    pub(crate) fn start_claimed<F>(
        protocol: Driver,
        leds: usize,
        generation: u64,
        keys: Vec<String>,
        factory: F,
    ) -> Result<Self, String>
    where
        F: FnOnce(Cancellation) -> Result<Box<dyn RgbDriver>, String> + Send + 'static,
    {
        if leds == 0
            || leds > 65536
            || generation == 0
            || matches!(protocol, Driver::Unsupported { .. } | Driver::TtLcd)
        {
            return Err("Invalid RGB worker capabilities".into());
        }
        crate::devices::Device {
            id: "output".into(),
            label: "Output".into(),
            role: crate::devices::Role::Canvas,
            driver: protocol.clone(),
            leds,
            rate_hz: 1,
            enabled: true,
        }
        .validate()?;
        let lease = Lease::acquire(keys)?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                pending: None,
                last_sequence: 0,
                snapshot: Snapshot {
                    phase: Phase::Connecting,
                    generation,
                    received: 0,
                    replaced: 0,
                    submitted: 0,
                    deferred: 0,
                    last_submitted: None,
                    error: None,
                },
            }),
            wake: Condvar::new(),
            cancel: Cancellation(Arc::new(AtomicBool::new(false))),
        });
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("rgb-output".into())
            .spawn(move || {
                // Ownership outlives a dropped Host until the worker destroys its driver.
                let _endpoint_lease = lease;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(&worker, protocol, leds, factory)
                }));
                let mut state = worker.state.lock().unwrap_or_else(|e| e.into_inner());
                state.pending = None;
                match result {
                    Ok(Ok(())) => state.snapshot.phase = Phase::Stopped,
                    Ok(Err(error)) => {
                        state.snapshot.phase = Phase::Failed;
                        state.snapshot.error = Some(error);
                    }
                    Err(_) => {
                        state.snapshot.phase = Phase::Failed;
                        state.snapshot.error = Some("RGB output worker panicked".into());
                    }
                }
                worker.cancel.0.store(true, Ordering::Release);
                worker.wake.notify_all();
            })
            .map_err(|e| format!("Start RGB output thread: {e}"))?;
        Ok(Self {
            shared,
            thread: Some(thread),
            leds,
        })
    }
    pub fn submit(&self, ticket: Ticket, colors: &[Rgb]) -> Result<bool, String> {
        if colors.len() != self.leds || ticket.sequence == 0 {
            return Err("RGB frame dimensions/ticket mismatch".into());
        }
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if ticket.generation != state.snapshot.generation {
            return Err("Stale RGB generation".into());
        }
        if self.shared.cancel.cancelled()
            || !matches!(state.snapshot.phase, Phase::Connecting | Phase::Ready)
        {
            return Ok(false);
        }
        // Sequence order is checked against the last accepted request, including
        // one already taken by the worker, not just the pending slot.
        if ticket.sequence <= state.last_sequence {
            return Err("RGB ticket sequence must increase".into());
        }
        state.last_sequence = ticket.sequence;
        state.snapshot.received = state.snapshot.received.saturating_add(1);
        if state
            .pending
            .replace(Frame {
                ticket,
                colors: colors.to_vec(),
            })
            .is_some()
        {
            state.snapshot.replaced = state.snapshot.replaced.saturating_add(1);
        }
        self.shared.wake.notify_one();
        Ok(true)
    }
    pub fn snapshot(&self) -> Snapshot {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot
            .clone()
    }
    pub fn stop(&self) {
        self.shared.cancel.0.store(true, Ordering::Release);
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pending = None;
        if matches!(state.snapshot.phase, Phase::Connecting | Phase::Ready) {
            state.snapshot.phase = Phase::Stopping;
        }
        self.shared.wake.notify_all();
    }
    /// Nonblocking reclamation for a GUI timer. Stopping is not Stopped while an
    /// operation still owns a device; callers must keep its lease until joined.
    pub fn reap(&mut self) -> bool {
        if self.thread.as_ref().is_some_and(|t| t.is_finished()) {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            true
        } else {
            self.thread.is_none()
        }
    }
    pub fn is_reaped(&self) -> bool {
        self.thread.is_none()
    }
    #[cfg(test)]
    fn join(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        self.stop();
        let _ = self.reap();
    }
}
fn run<F>(shared: &Arc<Shared>, protocol: Driver, leds: usize, factory: F) -> Result<(), String>
where
    F: FnOnce(Cancellation) -> Result<Box<dyn RgbDriver>, String>,
{
    if shared.cancel.cancelled() {
        return Ok(());
    }
    let mut driver = factory(shared.cancel.clone())?;
    if driver.protocol() != protocol || driver.leds() != leds {
        return Err("RGB driver capabilities changed during connection".into());
    }
    if shared.cancel.cancelled() {
        return Ok(());
    }
    {
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if shared.cancel.cancelled() {
            return Ok(());
        }
        state.snapshot.phase = Phase::Ready;
    }
    let mut retry = Instant::now();
    loop {
        let frame = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if shared.cancel.cancelled() {
                    return Ok(());
                }
                let now = Instant::now();
                if now >= retry
                    && let Some(frame) = state.pending.take()
                {
                    break frame;
                }
                let wait = if now < retry {
                    retry.duration_since(now).min(Duration::from_millis(250))
                } else {
                    Duration::from_millis(250)
                };
                state = shared
                    .wake
                    .wait_timeout(state, wait)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        };
        if shared.cancel.cancelled() {
            return Ok(());
        }
        match driver.write(&frame.colors, &shared.cancel)? {
            WriteResult::Submitted => {
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                state.snapshot.submitted = state.snapshot.submitted.saturating_add(1);
                state.snapshot.last_submitted = Some(frame.ticket);
            }
            WriteResult::Deferred(delay) => {
                if delay.is_zero() || delay > Duration::from_secs(30) {
                    return Err("Invalid RGB driver retry delay".into());
                }
                retry = Instant::now() + delay;
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                state.snapshot.deferred = state.snapshot.deferred.saturating_add(1);
                if state.pending.is_none() {
                    state.pending = Some(frame);
                } else {
                    state.snapshot.replaced = state.snapshot.replaced.saturating_add(1);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    struct Fake {
        entered: mpsc::Sender<Vec<Rgb>>,
        release: mpsc::Receiver<()>,
        dropped: Arc<AtomicBool>,
    }
    impl RgbDriver for Fake {
        fn protocol(&self) -> Driver {
            Driver::DoorBle
        }
        fn leds(&self) -> usize {
            20
        }
        fn write(&mut self, c: &[Rgb], _: &Cancellation) -> Result<WriteResult, String> {
            self.entered.send(c.to_vec()).unwrap();
            self.release
                .recv_timeout(Duration::from_secs(2))
                .map_err(|e| e.to_string())?;
            Ok(WriteResult::Submitted)
        }
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }
    #[test]
    fn endpoint_ownership_outlives_dropped_host_handle() {
        let (tx, rx) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let dropped = Arc::new(AtomicBool::new(false));
        let observed = dropped.clone();
        let keys = vec!["stand:drop-lease".to_owned()];
        let mut output = Output::start_claimed(Driver::DoorBle, 20, 1, keys.clone(), move |_| {
            Ok(Box::new(Fake {
                entered: tx,
                release: wait,
                dropped: observed,
            }))
        })
        .unwrap();
        output
            .submit(
                Ticket {
                    generation: 1,
                    sequence: 1,
                },
                &[Rgb::default(); 20],
            )
            .unwrap();
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let join = output.thread.take().unwrap();
        drop(output);
        assert!(Lease::acquire(keys.clone()).is_err());
        release.send(()).unwrap();
        join.join().unwrap();
        assert!(dropped.load(Ordering::Acquire));
        assert!(Lease::acquire(keys).is_ok());
    }
    #[test]
    fn slow_output_keeps_only_latest_frame_and_stop_is_drained() {
        let (tx, rx) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let dropped = Arc::new(AtomicBool::new(false));
        let observed = dropped.clone();
        let mut output = Output::start(Driver::DoorBle, 20, 7, move |_| {
            Ok(Box::new(Fake {
                entered: tx,
                release: wait,
                dropped: observed,
            }))
        })
        .unwrap();
        let ticket = |sequence| Ticket {
            generation: 7,
            sequence,
        };
        output.submit(ticket(1), &[Rgb::new(1, 0, 0); 20]).unwrap();
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        output.submit(ticket(2), &[Rgb::new(2, 0, 0); 20]).unwrap();
        output.submit(ticket(3), &[Rgb::new(3, 0, 0); 20]).unwrap();
        assert!(
            output
                .submit(
                    Ticket {
                        generation: 6,
                        sequence: 4
                    },
                    &[Rgb::default(); 20]
                )
                .is_err()
        );
        release.send(()).unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            [Rgb::new(3, 0, 0); 20]
        );
        output.stop();
        assert_eq!(output.snapshot().phase, Phase::Stopping);
        release.send(()).unwrap();
        output.join();
        let s = output.snapshot();
        assert_eq!(s.submitted, 2);
        assert_eq!(s.replaced, 1);
        assert_eq!(s.last_submitted, Some(ticket(3)));
        assert!(dropped.load(Ordering::Acquire));
    }
}
