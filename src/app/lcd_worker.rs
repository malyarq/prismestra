//! Independent LCD owner thread. Only an explicitly invoked injected factory can acquire I/O.
use super::lcd::{self, Controller};
use crate::{
    lcd::protocol::{ACK_BYTES, REPORT_BYTES, ReportTransport},
    runtime::output::{Cancellation, Lease},
};
use std::{
    collections::BTreeSet,
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Intent {
    pub revision: u64,
    pub active: bool,
    pub suspended: bool,
    pub samples: Arc<[lcd::Sample]>,
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
    /// Ready means worker resources exist, not a physical-device connection acknowledgement.
    pub phase: Phase,
    pub generation: u64,
    pub last_accepted_revision: u64,
    /// Latest consumed intent, not acknowledgement of visible pixels.
    pub processed_revision: u64,
    pub frames_sent: u64,
    pub preview: Arc<[u8]>,
    pub lcd_state: Option<lcd::State>,
    pub error: Option<String>,
    pub replaced: u64,
}
struct State {
    pending: Option<Intent>,
    suspension_epoch: u64,
    snapshot: Snapshot,
}
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    cancel: Cancellation,
}
pub struct Worker {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

fn validate(intent: &Intent) -> Result<(), String> {
    if intent.revision == 0 || intent.samples.len() > 4096 {
        return Err("LCD intent requires a nonzero revision and at most 4096 samples".into());
    }
    let mut keys = BTreeSet::new();
    for sample in intent.samples.iter() {
        if sample.key.is_empty()
            || sample.key.len() > 128
            || !sample
                .key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            || !keys.insert(sample.key.as_str())
        {
            return Err("LCD intent contains an invalid or ambiguous logical sample key".into());
        }
    }
    Ok(())
}
fn error(mut error: String) -> String {
    let mut end = error.len().min(8192);
    while !error.is_char_boundary(end) {
        end -= 1;
    }
    error.truncate(end);
    error
}
impl Worker {
    pub fn start<T, F>(generation: u64, factory: F) -> Result<Self, String>
    where
        T: ReportTransport + 'static,
        F: FnOnce(Cancellation) -> Result<(Controller, T), String> + Send + 'static,
    {
        Self::start_claimed(generation, vec![], factory)
    }
    pub(crate) fn start_claimed<T, F>(
        generation: u64,
        keys: Vec<String>,
        factory: F,
    ) -> Result<Self, String>
    where
        T: ReportTransport + 'static,
        F: FnOnce(Cancellation) -> Result<(Controller, T), String> + Send + 'static,
    {
        if generation == 0 {
            return Err("LCD worker generation must be nonzero".into());
        }
        let lease = Lease::acquire(keys)?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                pending: None,
                suspension_epoch: 0,
                snapshot: Snapshot {
                    phase: Phase::Connecting,
                    generation,
                    last_accepted_revision: 0,
                    processed_revision: 0,
                    frames_sent: 0,
                    preview: Arc::from([]),
                    lcd_state: None,
                    error: None,
                    replaced: 0,
                },
            }),
            wake: Condvar::new(),
            cancel: Cancellation::default(),
        });
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("lcd-output".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(&worker, factory)
                }));
                // run's controller/transport have been destroyed, even on unwind, before release.
                drop(lease);
                let mut state = worker.state.lock().unwrap_or_else(|e| e.into_inner());
                state.pending = None;
                match result {
                    Ok(Ok(())) => state.snapshot.phase = Phase::Stopped,
                    Ok(Err(message)) => {
                        state.snapshot.phase = if worker.cancel.cancelled() {
                            Phase::Stopped
                        } else {
                            Phase::Failed
                        };
                        state.snapshot.error = Some(error(message));
                    }
                    Err(_) => {
                        state.snapshot.phase = Phase::Failed;
                        state.snapshot.error = Some("LCD output worker panicked".into());
                    }
                }
                worker.cancel.cancel();
                worker.wake.notify_all();
            })
            .map_err(|e| format!("Start LCD output thread: {e}"))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }
    pub fn submit(&self, intent: Intent) -> Result<bool, String> {
        validate(&intent)?;
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if intent.revision <= state.snapshot.last_accepted_revision {
            return Err("LCD intent revision must strictly increase".into());
        }
        if self.shared.cancel.cancelled()
            || !matches!(state.snapshot.phase, Phase::Connecting | Phase::Ready)
        {
            return Ok(false);
        }
        let epoch = if intent.suspended {
            state
                .suspension_epoch
                .checked_add(1)
                .ok_or("LCD suspension epoch exhausted")?
        } else {
            state.suspension_epoch
        };
        state.suspension_epoch = epoch;
        state.snapshot.last_accepted_revision = intent.revision;
        if state.pending.replace(intent).is_some() {
            state.snapshot.replaced = state.snapshot.replaced.saturating_add(1);
        }
        self.shared.wake.notify_all();
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
        self.shared.cancel.cancel();
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pending = None;
        if matches!(state.snapshot.phase, Phase::Connecting | Phase::Ready) {
            state.snapshot.phase = Phase::Stopping;
        }
        self.shared.wake.notify_all();
    }
    /// GUI-safe reclamation: never joins an unfinished I/O operation.
    pub fn reap(&mut self) -> bool {
        if self
            .thread
            .as_ref()
            .is_some_and(|thread| thread.is_finished())
            && let Some(thread) = self.thread.take()
        {
            let _ = thread.join();
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

struct Guarded<'a, T> {
    transport: &'a mut T,
    cancel: &'a Cancellation,
}
impl<T: ReportTransport> ReportTransport for Guarded<'_, T> {
    fn now_ms(&self) -> u64 {
        self.transport.now_ms()
    }
    fn write_report(&mut self, packet: &[u8; REPORT_BYTES], timeout: u64) -> Result<usize, String> {
        if self.cancel.cancelled() {
            return Err("LCD worker cancelled between reports".into());
        }
        self.transport.write_report(packet, timeout)
    }
    fn read_ack(&mut self, response: &mut [u8; ACK_BYTES], timeout: u64) -> Result<usize, String> {
        if self.cancel.cancelled() {
            return Err("LCD worker cancelled before ACK read".into());
        }
        self.transport.read_ack(response, timeout)
    }
}
fn publish(shared: &Shared, controller: &Controller, revision: u64) {
    let frames = controller.frames_sent();
    let changed = shared
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .snapshot
        .frames_sent
        != frames;
    // Compress/copy payload outside the intent mailbox lock.
    let preview = changed.then(|| Arc::from(controller.preview()));
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    state.snapshot.processed_revision = state.snapshot.processed_revision.max(revision);
    if let Some(preview) = preview {
        state.snapshot.preview = preview;
    }
    state.snapshot.frames_sent = frames;
    state.snapshot.lcd_state = Some(match controller.state() {
        lcd::State::Failed(message) => lcd::State::Failed(error(message)),
        other => other,
    });
    shared.wake.notify_all();
}
fn clock<T: ReportTransport>(transport: &T, last: &mut Option<u64>) -> Result<u64, String> {
    let now = transport.now_ms();
    if last.is_some_and(|old| now < old) {
        return Err("LCD worker transport clock moved backwards".into());
    }
    *last = Some(now);
    Ok(now)
}
fn wait(shared: &Shared, epoch: u64, duration: Option<Duration>) {
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    if shared.cancel.cancelled() || state.pending.is_some() || state.suspension_epoch != epoch {
        return;
    }
    if let Some(duration) = duration {
        if !duration.is_zero() {
            drop(
                shared
                    .wake
                    .wait_timeout(state, duration)
                    .unwrap_or_else(|e| e.into_inner()),
            );
        }
    } else {
        while !shared.cancel.cancelled()
            && state.pending.is_none()
            && state.suspension_epoch == epoch
        {
            state = shared.wake.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }
}
fn run<T, F>(shared: &Arc<Shared>, factory: F) -> Result<(), String>
where
    T: ReportTransport,
    F: FnOnce(Cancellation) -> Result<(Controller, T), String>,
{
    if shared.cancel.cancelled() {
        return Ok(());
    }
    // T can be !Send: construction, all calls and destruction occur right here.
    let (mut controller, mut transport) = factory(shared.cancel.clone())?;
    if shared.cancel.cancelled() {
        return Ok(());
    }
    {
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if shared.cancel.cancelled() {
            return Ok(());
        }
        state.snapshot.phase = Phase::Ready;
        state.snapshot.lcd_state = Some(controller.state());
        shared.wake.notify_all();
    }
    let mut current: Option<Intent> = None;
    let mut epoch = 0;
    let mut last_clock = None;
    loop {
        let (pending, next_epoch) = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if shared.cancel.cancelled() {
                return Ok(());
            }
            (state.pending.take(), state.suspension_epoch)
        };
        let barrier = next_epoch != epoch;
        if barrier {
            controller.suspend();
            epoch = next_epoch;
        }
        let changed = pending.is_some() || barrier;
        if let Some(intent) = pending {
            current = Some(intent);
        }
        let Some(intent) = &current else {
            wait(shared, epoch, None);
            continue;
        };
        if changed {
            if intent.suspended {
                if !barrier {
                    controller.suspend();
                }
            } else {
                controller.resume();
            }
        }
        if intent.suspended {
            publish(shared, &controller, intent.revision);
            wait(shared, epoch, None);
            continue;
        }
        let poll = match controller.state() {
            lcd::State::Failed(message) => return Err(message),
            lcd::State::Disabled => false,
            lcd::State::Stopped => intent.active || changed,
            lcd::State::Blanked => intent.active,
            lcd::State::Suspended => changed,
            lcd::State::Running => {
                if !intent.active {
                    changed
                } else {
                    let now = clock(&transport, &mut last_clock)?;
                    controller.next_due_ms().is_none_or(|due| now >= due)
                }
            }
        };
        if poll {
            clock(&transport, &mut last_clock)?;
            let result = controller.poll(
                &mut Guarded {
                    transport: &mut transport,
                    cancel: &shared.cancel,
                },
                intent.active,
                &intent.samples,
            );
            let completed_clock = if result.is_ok() {
                clock(&transport, &mut last_clock).map(|_| ())
            } else {
                Ok(())
            };
            publish(shared, &controller, intent.revision);
            result?;
            completed_clock?;
        } else if changed {
            publish(shared, &controller, intent.revision);
        }
        if shared.cancel.cancelled() {
            return Ok(());
        }
        {
            let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.pending.is_some() || state.suspension_epoch != epoch {
                continue;
            }
        }
        let duration = if intent.active && controller.state() == lcd::State::Running {
            let now = clock(&transport, &mut last_clock)?;
            Some(
                Duration::from_millis(controller.next_due_ms().unwrap_or(now).saturating_sub(now))
                    .min(Duration::from_secs(10)),
            )
        } else {
            None
        };
        wait(shared, epoch, duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::assets::Store;
    use std::{
        fs,
        path::{Path, PathBuf},
        rc::Rc,
        sync::atomic::{AtomicU64, Ordering},
        thread::ThreadId,
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Stand {
        root: PathBuf,
        config: lcd::Config,
    }
    impl Stand {
        fn new() -> Self {
            let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("work/app-lcd-worker-stand");
            fs::create_dir_all(&parent).unwrap();
            let root = loop {
                let p = parent.join(format!(
                    "{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&p) {
                    Ok(()) => break p,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => panic!("{e}"),
                }
            };
            let store = Store::new(root.join("assets")).unwrap();
            let id = store
                .put(
                    include_bytes!("../lcd/fixtures/two-frame.apng"),
                    crate::lcd::media::MAX_SOURCE,
                )
                .unwrap();
            Self {
                root,
                config: lcd::Config {
                    enabled: true,
                    media: Some(id),
                    ..Default::default()
                },
            }
        }
        fn factory(
            &self,
            log: Arc<Log>,
            gate: Option<Arc<Gate>>,
            fail: bool,
        ) -> impl FnOnce(Cancellation) -> Result<(Controller, Fake), String> + Send + 'static
        {
            let root = self.root.join("assets");
            let config = self.config.clone();
            move |_| {
                let store = Store::new(root)?;
                let controller = Controller::new(config, &store)?;
                Ok((controller, Fake::new(log, gate, fail)))
            }
        }
    }
    impl Drop for Stand {
        fn drop(&mut self) {
            let dir = self.root.join("assets");
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let _ = fs::remove_file(entry.path());
                }
            }
            let _ = fs::remove_dir(dir);
            let _ = fs::remove_dir(&self.root);
        }
    }
    #[derive(Default)]
    struct Gate {
        open: Mutex<bool>,
        wake: Condvar,
    }
    impl Gate {
        fn release(&self) {
            *self.open.lock().unwrap() = true;
            self.wake.notify_all();
        }
        fn wait(&self) {
            let guard = self.open.lock().unwrap();
            let (guard, _) = self
                .wake
                .wait_timeout_while(guard, Duration::from_secs(3), |open| !*open)
                .unwrap();
            assert!(*guard, "Stand gate was not released");
        }
    }
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    struct Observed {
        clocks: u64,
        writes: u64,
        acks: u64,
        created: Option<ThreadId>,
        dropped: Option<ThreadId>,
        factory: Option<ThreadId>,
        blocked: bool,
    }
    struct Log {
        state: Mutex<Observed>,
        wake: Condvar,
        now: AtomicU64,
    }
    impl Default for Log {
        fn default() -> Self {
            Self {
                state: Mutex::new(Observed::default()),
                wake: Condvar::new(),
                now: AtomicU64::new(1000),
            }
        }
    }
    impl Log {
        fn observe(&self) -> Observed {
            self.state.lock().unwrap().clone()
        }
        fn until(&self, condition: impl Fn(&Observed) -> bool) {
            let s = self.state.lock().unwrap();
            let (s, _) = self
                .wake
                .wait_timeout_while(s, Duration::from_secs(3), |s| !condition(s))
                .unwrap();
            assert!(condition(&s), "Stand observation timed out: {s:?}");
        }
        fn idle(&self) {
            let before = self.observe();
            let s = self.state.lock().unwrap();
            let (s, timeout) = self
                .wake
                .wait_timeout_while(s, Duration::from_millis(30), |s| *s == before)
                .unwrap();
            assert!(timeout.timed_out());
            assert_eq!(*s, before);
        }
    }
    struct Fake {
        log: Arc<Log>,
        block: Option<Arc<Gate>>,
        fail: bool,
        owner: ThreadId,
        _not_send: Rc<()>,
    }
    impl Fake {
        fn new(log: Arc<Log>, block: Option<Arc<Gate>>, fail: bool) -> Self {
            let owner = std::thread::current().id();
            log.state.lock().unwrap().created = Some(owner);
            log.wake.notify_all();
            Self {
                log,
                block,
                fail,
                owner,
                _not_send: Rc::new(()),
            }
        }
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            assert_eq!(std::thread::current().id(), self.owner);
            self.log.state.lock().unwrap().dropped = Some(self.owner);
            self.log.wake.notify_all();
        }
    }
    impl ReportTransport for Fake {
        fn now_ms(&self) -> u64 {
            assert_eq!(std::thread::current().id(), self.owner);
            self.log.state.lock().unwrap().clocks += 1;
            self.log.wake.notify_all();
            self.log.now.load(Ordering::Acquire)
        }
        fn write_report(
            &mut self,
            report: &[u8; REPORT_BYTES],
            timeout: u64,
        ) -> Result<usize, String> {
            assert_eq!(std::thread::current().id(), self.owner);
            assert_eq!(timeout, 250);
            assert_eq!((report[0], report[1]), (0, 8));
            self.log.now.fetch_add(1, Ordering::AcqRel);
            let first = {
                let mut s = self.log.state.lock().unwrap();
                s.writes += 1;
                let first = s.writes == 1;
                if first && self.block.is_some() {
                    s.blocked = true;
                }
                self.log.wake.notify_all();
                first
            };
            if first && let Some(gate) = self.block.take() {
                gate.wait();
            }
            Ok(REPORT_BYTES)
        }
        fn read_ack(
            &mut self,
            response: &mut [u8; ACK_BYTES],
            timeout: u64,
        ) -> Result<usize, String> {
            assert_eq!(std::thread::current().id(), self.owner);
            assert_eq!(timeout, 250);
            self.log.now.fetch_add(1, Ordering::AcqRel);
            self.log.state.lock().unwrap().acks += 1;
            self.log.wake.notify_all();
            if self.fail {
                return Err("Injected LCD ACK failure".into());
            }
            response.fill(0xa5);
            response[0] = 0;
            Ok(ACK_BYTES)
        }
    }
    fn intent(revision: u64, active: bool, suspended: bool) -> Intent {
        Intent {
            revision,
            active,
            suspended,
            samples: Arc::from([]),
        }
    }
    fn until(shared: &Shared, condition: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let s = shared.state.lock().unwrap();
        let (s, _) = shared
            .wake
            .wait_timeout_while(s, Duration::from_secs(3), |s| !condition(&s.snapshot))
            .unwrap();
        assert!(
            condition(&s.snapshot),
            "LCD snapshot timed out: {:?}",
            s.snapshot
        );
        s.snapshot.clone()
    }
    fn finish(worker: &mut Worker) {
        worker.stop();
        until(&worker.shared, |s| {
            matches!(s.phase, Phase::Stopped | Phase::Failed)
        });
        // Only the stand joins, after bounded observation of complete resource disposal.
        if let Some(thread) = worker.thread.take() {
            thread.join().unwrap();
        }
        assert!(worker.is_reaped());
    }
    fn endpoint() -> String {
        format!(
            "synthetic-lcd-worker-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn blocked_factory_latest_mailbox_blank_suspend_idle_and_thread_local_transport() {
        let stand = Stand::new();
        let log = Arc::new(Log::default());
        let gate = Arc::new(Gate::default());
        let base = stand.factory(log.clone(), None, false);
        let held = gate.clone();
        let entered = log.clone();
        let mut worker = Worker::start(1, move |cancel| {
            entered.state.lock().unwrap().factory = Some(std::thread::current().id());
            entered.wake.notify_all();
            held.wait();
            base(cancel)
        })
        .unwrap();
        log.until(|s| s.factory.is_some());
        assert_eq!(worker.snapshot().phase, Phase::Connecting);
        worker.submit(intent(1, true, false)).unwrap();
        worker.submit(intent(2, true, true)).unwrap();
        worker.submit(intent(3, false, false)).unwrap();
        assert_eq!(worker.snapshot().replaced, 2);
        gate.release();
        let snapshot = until(&worker.shared, |s| {
            s.processed_revision == 3 && s.frames_sent == 1
        });
        assert_eq!(snapshot.last_accepted_revision, 3);
        assert_eq!(snapshot.lcd_state, Some(lcd::State::Blanked));
        assert!(
            image::load_from_memory(&snapshot.preview)
                .unwrap()
                .into_rgb8()
                .pixels()
                .all(|p| p.0 == [0, 0, 0])
        );
        log.idle();
        worker.submit(intent(4, true, true)).unwrap();
        until(&worker.shared, |s| {
            s.processed_revision == 4 && s.lcd_state == Some(lcd::State::Suspended)
        });
        log.idle();
        worker.submit(intent(5, true, false)).unwrap();
        until(&worker.shared, |s| {
            s.processed_revision == 5 && s.frames_sent == 2
        });
        finish(&mut worker);
        let seen = log.observe();
        assert_eq!(seen.created, seen.dropped);
        assert_ne!(seen.created, Some(std::thread::current().id()));
    }
    #[test]
    fn overwritten_suspend_is_a_barrier_after_inflight_ack_not_a_lost_flag() {
        let stand = Stand::new();
        let log = Arc::new(Log::default());
        let gate = Arc::new(Gate::default());
        let mut worker =
            Worker::start(2, stand.factory(log.clone(), Some(gate.clone()), false)).unwrap();
        worker.submit(intent(1, true, false)).unwrap();
        log.until(|s| s.blocked);
        worker.submit(intent(2, true, true)).unwrap();
        worker.submit(intent(3, true, false)).unwrap();
        assert_eq!(worker.snapshot().replaced, 1);
        gate.release();
        let snapshot = until(&worker.shared, |s| {
            s.processed_revision == 3 && s.frames_sent == 2
        });
        let media = crate::lcd::media::prepare(
            Arc::from(include_bytes!("../lcd/fixtures/two-frame.apng").as_slice()),
            0,
            || false,
        )
        .unwrap();
        // A reset resumes from source frame0; losing suspend would leave frame1 due in the future.
        assert_eq!(snapshot.preview.as_ref(), media.frames[0].jpeg.as_ref());
        assert_eq!(log.observe().acks, 2);
        finish(&mut worker);
    }
    #[test]
    fn dropped_worker_retains_endpoint_until_blocked_report_drains_and_transport_drops() {
        let stand = Stand::new();
        let log = Arc::new(Log::default());
        let gate = Arc::new(Gate::default());
        let key = endpoint();
        let mut worker = Worker::start_claimed(
            3,
            vec![key.clone()],
            stand.factory(log.clone(), Some(gate.clone()), false),
        )
        .unwrap();
        worker.submit(intent(1, true, false)).unwrap();
        log.until(|s| s.blocked);
        let shared = worker.shared.clone();
        worker.stop();
        assert!(!worker.reap());
        drop(worker);
        assert!(Lease::acquire(vec![key.clone()]).is_err());
        gate.release();
        let stopped = until(&shared, |s| s.phase == Phase::Stopped);
        assert_eq!(stopped.frames_sent, 0);
        assert_eq!((log.observe().writes, log.observe().acks), (1, 0));
        let seen = log.observe();
        assert_eq!(seen.created, seen.dropped);
        let replacement = Lease::acquire(vec![key]).unwrap();
        drop(replacement);
    }
    #[test]
    fn stopping_blocked_factory_does_not_release_lease_or_run_its_transport() {
        let stand = Stand::new();
        let log = Arc::new(Log::default());
        let gate = Arc::new(Gate::default());
        let key = endpoint();
        let base = stand.factory(log.clone(), None, false);
        let held = gate.clone();
        let entered = log.clone();
        let worker = Worker::start_claimed(4, vec![key.clone()], move |cancel| {
            entered.state.lock().unwrap().factory = Some(std::thread::current().id());
            entered.wake.notify_all();
            held.wait();
            base(cancel)
        })
        .unwrap();
        log.until(|s| s.factory.is_some());
        let shared = worker.shared.clone();
        drop(worker);
        assert!(Lease::acquire(vec![key.clone()]).is_err());
        gate.release();
        until(&shared, |s| s.phase == Phase::Stopped);
        let seen = log.observe();
        assert_eq!((seen.writes, seen.acks, seen.clocks), (0, 0, 0));
        assert_eq!(seen.created, seen.dropped);
        drop(Lease::acquire(vec![key]).unwrap());
    }
    #[test]
    fn revisions_validation_no_intent_idle_and_terminal_failure_or_panic() {
        let stand = Stand::new();
        let log = Arc::new(Log::default());
        let mut worker = Worker::start(5, stand.factory(log.clone(), None, true)).unwrap();
        until(&worker.shared, |s| s.phase == Phase::Ready);
        log.idle();
        assert_eq!(log.observe().clocks, 0);
        assert!(worker.submit(intent(0, true, false)).is_err());
        let mut bad = intent(1, true, false);
        bad.samples = Arc::from([lcd::Sample {
            key: "local/path".into(),
            celsius: 40.,
            observed_ms: 1,
        }]);
        assert!(worker.submit(bad).is_err());
        assert_eq!(worker.snapshot().last_accepted_revision, 0);
        worker.submit(intent(1, true, false)).unwrap();
        assert!(worker.submit(intent(1, true, false)).is_err());
        let failed = until(&worker.shared, |s| s.phase == Phase::Failed);
        assert_eq!(failed.frames_sent, 0);
        assert!(failed.preview.is_empty());
        assert!(matches!(failed.lcd_state, Some(lcd::State::Failed(_))));
        assert!(!worker.submit(intent(2, true, false)).unwrap());
        finish(&mut worker);
        let mut panic = Worker::start::<Fake, _>(6, |_| panic!("Injected factory panic")).unwrap();
        let failed = until(&panic.shared, |s| s.phase == Phase::Failed);
        assert!(failed.error.unwrap().contains("panicked"));
        finish(&mut panic);
        assert!(
            Worker::start::<Fake, _>(0, |_| panic!("Invalid generation must not invoke factory"))
                .is_err()
        );
    }
}
