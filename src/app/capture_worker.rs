//! Latest-intent capture owner. No Host, window, cooling or hardware output.
use super::ambient;
pub use crate::runtime::output::Phase;
use crate::runtime::{
    cadence_wait::{Signal, Timer},
    frame_feed,
};
use crate::{
    devices::Setup,
    lighting::{ambient::Rect, color::Rgb},
    runtime::output::Cancellation,
};
#[cfg(test)]
use std::time::Instant;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Intent {
    pub revision: u64,
    pub active: bool,
    pub suspended: bool,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub high_resolution_timer: bool,
    pub phase: Phase,
    pub generation: u64,
    pub last_accepted_revision: u64,
    /// Observed intent, never a hardware acknowledgement.
    pub processed_revision: u64,
    pub frame_revision: u64,
    pub frames: BTreeMap<String, Arc<[Rgb]>>,
    pub source_status: BTreeMap<String, ambient::SourceStatus>,
    pub error: Option<String>,
    pub replaced: u64,
}
struct State {
    latest: Option<Intent>,
    suspension: u64,
    snapshot: Snapshot,
}
struct Shared {
    state: Mutex<State>,
    wake: Signal,
    feed: frame_feed::Publisher,
    reader: frame_feed::Reader,
    cancel: Cancellation,
}
pub struct Worker {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    /// Only an explicit start invokes the supplied factory, on its owner thread.
    /// A Provider need not be Send: it is constructed, used and dropped there.
    pub fn start<F, P>(
        generation: u64,
        config: ambient::Config,
        setup: Setup,
        factory: F,
    ) -> Result<Self, String>
    where
        F: FnOnce(Cancellation) -> Result<P, String> + Send + 'static,
        P: ambient::Provider + 'static,
    {
        if generation == 0 {
            return Err("Capture generation must be nonzero".into());
        }
        config.validate(&setup)?;
        let statuses: BTreeMap<String, ambient::SourceStatus> = config
            .sources
            .iter()
            .map(|source| (source.key.clone(), ambient::SourceStatus::default()))
            .collect();
        let routes = config
            .routes
            .iter()
            .map(|r| {
                let source = config.sources.iter().find(|s| s.key == r.source).unwrap();
                (
                    r.device.clone(),
                    frame_feed::Route {
                        leds: r.zone.leds,
                        stale_after_ms: source.stale_after_ms,
                    },
                )
            })
            .collect();
        let (feed, reader) = frame_feed::channel(generation, routes)?;
        let timer = Timer::new()?;
        let high_resolution_timer = timer.precise();
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                latest: None,
                suspension: 0,
                snapshot: Snapshot {
                    high_resolution_timer,
                    phase: Phase::Connecting,
                    generation,
                    last_accepted_revision: 0,
                    processed_revision: 0,
                    frame_revision: 0,
                    frames: BTreeMap::new(),
                    source_status: statuses,
                    error: None,
                    replaced: 0,
                },
            }),
            wake: Signal::new()?,
            feed,
            reader,
            cancel: Cancellation::default(),
        });
        let owner = shared.clone();
        let thread = std::thread::Builder::new()
            .name("ambient-capture".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(&owner, config, setup, factory, timer)
                }));
                let mut state = owner.state.lock().unwrap_or_else(|e| e.into_inner());
                state.latest = None;
                match result {
                    Ok(Ok(())) => state.snapshot.phase = Phase::Stopped,
                    Ok(Err(error)) => {
                        state.snapshot.phase = Phase::Failed;
                        state.snapshot.error = Some(error.chars().take(512).collect());
                    }
                    Err(_) => {
                        state.snapshot.phase = Phase::Failed;
                        state.snapshot.error = Some("Capture worker panicked".into());
                    }
                }
                clear_frames(&mut state.snapshot);
                owner.feed.close();
                owner.cancel.cancel();
                owner.wake.notify();
            })
            .map_err(|e| format!("Start capture worker: {e}"))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }
    pub fn submit(&self, intent: Intent) -> Result<bool, String> {
        if intent.revision == 0 {
            return Err("Capture intent revision must be nonzero".into());
        }
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if self.shared.cancel.cancelled()
            || !matches!(state.snapshot.phase, Phase::Connecting | Phase::Ready)
        {
            return Ok(false);
        }
        if intent.revision <= state.snapshot.last_accepted_revision {
            return Err("Capture intent revision must strictly increase".into());
        }
        if state.latest.is_some()
            && state.snapshot.last_accepted_revision > state.snapshot.processed_revision
        {
            state.snapshot.replaced = state.snapshot.replaced.saturating_add(1);
        }
        if intent.suspended || !intent.active {
            // Persistent barrier survives replacement of this pending intent.
            state.suspension = intent.revision;
            self.shared.feed.clear();
            clear_frames(&mut state.snapshot);
            for status in state.snapshot.source_status.values_mut() {
                *status = ambient::SourceStatus::default();
            }
        }
        state.latest = Some(intent);
        state.snapshot.last_accepted_revision = intent.revision;
        self.shared.wake.notify();
        Ok(true)
    }
    pub fn frame_reader(&self) -> frame_feed::Reader {
        self.shared.reader.clone()
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
        state.latest = None;
        clear_frames(&mut state.snapshot);
        self.shared.feed.close();
        if matches!(state.snapshot.phase, Phase::Connecting | Phase::Ready) {
            state.snapshot.phase = Phase::Stopping;
        }
        self.shared.wake.notify();
    }
    pub fn reap(&mut self) -> bool {
        if self
            .thread
            .as_ref()
            .is_some_and(|thread| thread.is_finished())
        {
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
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
        let _ = self.reap();
    }
}
fn clear_frames(snapshot: &mut Snapshot) {
    if !snapshot.frames.is_empty() {
        snapshot.frames.clear();
        snapshot.frame_revision = snapshot.frame_revision.saturating_add(1);
    }
}
struct Cancellable<'a, P> {
    provider: &'a mut P,
    shared: &'a Shared,
    intent: Intent,
    suspension: u64,
}
impl<P: ambient::Provider> ambient::Provider for Cancellable<'_, P> {
    fn poll(
        &mut self,
        source: &str,
        crop: Option<Rect>,
        at: u64,
    ) -> Result<Option<crate::capture::Frame>, String> {
        if self.shared.cancel.cancelled() {
            return Err("Capture cancelled".into());
        }
        {
            let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.suspension != self.suspension
                || state.latest != Some(self.intent)
                || !self.intent.active
                || self.intent.suspended
            {
                return Err("Capture intent superseded".into());
            }
        }
        self.provider.poll(source, crop, at)
    }
}
fn run<F, P>(
    shared: &Shared,
    config: ambient::Config,
    setup: Setup,
    factory: F,
    timer: Timer,
) -> Result<(), String>
where
    F: FnOnce(Cancellation) -> Result<P, String>,
    P: ambient::Provider,
{
    if shared.cancel.cancelled() {
        return Ok(());
    }
    let mut controller = ambient::Controller::new(config.clone(), &setup)?;
    let mut provider = factory(shared.cancel.clone())?;
    {
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if shared.cancel.cancelled() {
            return Ok(());
        }
        state.snapshot.phase = Phase::Ready;
    }
    let mut handled = 0;
    let mut suspension = 0;
    let mut due_us: Option<u64> = None;
    let mut previous_us = 0;
    let mut seen = 0;
    loop {
        let (intent, barrier, previous_frames) = loop {
            let (ready, duration) = {
                let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                if shared.cancel.cancelled() {
                    return Ok(());
                }
                if let Some(intent) = state.latest {
                    let changed = intent.revision != handled || state.suspension != suspension;
                    let ticking = intent.active && !intent.suspended && due_us.is_some();
                    if changed
                        || (ticking
                            && due_us.is_some_and(|due| crate::clock::monotonic_us() >= due))
                    {
                        (
                            Some((intent, state.suspension, state.snapshot.frames.clone())),
                            None,
                        )
                    } else {
                        (
                            None,
                            if ticking {
                                due_us.map(|due| {
                                    Duration::from_micros(
                                        due.saturating_sub(crate::clock::monotonic_us()).max(1),
                                    )
                                })
                            } else {
                                None
                            },
                        )
                    }
                } else {
                    (None, None)
                }
            };
            if let Some(ready) = ready {
                break ready;
            }
            timer.wait(&shared.wake, &mut seen, duration)?;
        };
        if barrier != suspension {
            controller.reset();
            suspension = barrier;
        }
        handled = intent.revision;
        if intent.suspended {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.latest == Some(intent)
                && state.suspension == barrier
                && !shared.cancel.cancelled()
            {
                state.snapshot.processed_revision = intent.revision;
            }
            continue;
        }
        let now = crate::clock::monotonic_us();
        if now == 0 || now == u64::MAX || now < previous_us {
            return Err("Capture monotonic clock is invalid".into());
        }
        previous_us = now;
        controller.prepare(
            now,
            intent.active,
            &mut Cancellable {
                provider: &mut provider,
                shared,
                intent,
                suspension: barrier,
            },
        )?;
        if shared.cancel.cancelled() {
            return Ok(());
        }
        // Allocate/copy image output outside the mailbox lock. Unchanged device
        // buffers retain their Arc, including when other routes change.
        let changes: Vec<(String, Arc<[Rgb]>)> = controller
            .changes()
            .filter(|(id, colors)| {
                previous_frames
                    .get(*id)
                    .is_none_or(|old| old.as_ref() != *colors)
            })
            .map(|(id, colors)| (id.to_owned(), Arc::from(colors)))
            .collect();
        let statuses: BTreeMap<String, ambient::SourceStatus> = config
            .sources
            .iter()
            .filter_map(|s| {
                controller
                    .source_status(&s.key)
                    .map(|status| (s.key.clone(), status.clone()))
            })
            .collect();
        let mut colors = previous_frames;
        for (id, frame) in &changes {
            colors.insert(id.clone(), frame.clone());
        }
        let batch = config
            .routes
            .iter()
            .filter_map(|route| {
                let colors = colors.get(&route.device)?.clone();
                let observed_ms = if intent.active && route.enabled {
                    statuses.get(&route.source).and_then(|s| s.observed_ms)
                } else {
                    None
                };
                Some((
                    route.device.clone(),
                    frame_feed::Frame {
                        colors,
                        observed_ms,
                    },
                ))
            })
            .collect();
        {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.latest == Some(intent)
                && state.suspension == barrier
                && !shared.cancel.cancelled()
            {
                shared.feed.publish(batch, crate::clock::monotonic_ms())?;
                if !changes.is_empty() {
                    for (id, colors) in changes {
                        state.snapshot.frames.insert(id, colors);
                    }
                    state.snapshot.frame_revision = state.snapshot.frame_revision.saturating_add(1);
                }
                state.snapshot.source_status = statuses;
                state.snapshot.processed_revision = intent.revision;
            }
        }
        due_us = controller.next_due_us();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capture::{
            Backend, Frame,
            ambient::{Mode, OutputSettings, Settings, Zone},
        },
        devices::{Canvas, Device, Driver, Role},
        lighting::ambient::Image,
    };
    use std::{cell::Cell, rc::Rc, sync::mpsc};
    fn fixture() -> (ambient::Config, Setup) {
        let setup = Setup {
            version: 1,
            canvas: Canvas {
                width: 1,
                height: 1,
                points: vec![],
            },
            devices: vec![Device {
                id: "strip".into(),
                label: "Strip".into(),
                role: Role::Ambient,
                driver: Driver::SkydimoSerial {
                    model: "SK0801".into(),
                },
                leds: 2,
                rate_hz: 144,
                enabled: true,
            }],
        };
        let config = ambient::Config {
            sources: vec![ambient::Source {
                key: "monitor".into(),
                rate_hz: 144,
                crop: None,
                stale_after_ms: 1000,
            }],
            routes: vec![ambient::Route {
                device: "strip".into(),
                source: "monitor".into(),
                enabled: true,
                zone: Zone::linear(2, false, Mode::ScreenCopy).unwrap(),
                processing: Settings::default(),
                output: OutputSettings {
                    smoothness: 0,
                    ..OutputSettings::default()
                },
            }],
        };
        (config, setup)
    }
    struct Feed {
        polls: mpsc::Sender<()>,
        gate: Option<mpsc::Receiver<()>>,
        dropped: mpsc::Sender<std::thread::ThreadId>,
        owner: std::thread::ThreadId,
        nonsend: Rc<Cell<u8>>,
        mode: Arc<Mutex<u8>>,
    }
    impl Drop for Feed {
        fn drop(&mut self) {
            assert_eq!(self.owner, std::thread::current().id());
            let _ = self.dropped.send(self.owner);
        }
    }
    impl ambient::Provider for Feed {
        fn poll(&mut self, _: &str, crop: Option<Rect>, at: u64) -> Result<Option<Frame>, String> {
            assert_eq!(self.owner, std::thread::current().id());
            self.nonsend.set(self.nonsend.get().wrapping_add(1));
            let _ = self.polls.send(());
            if let Some(gate) = self.gate.take() {
                gate.recv_timeout(Duration::from_secs(3)).unwrap();
            }
            let mode = *self.mode.lock().unwrap();
            match mode {
                1 => return Err("synthetic source unavailable".into()),
                2 => panic!("synthetic capture panic"),
                _ => {}
            }
            let colors = if mode == 3 && self.nonsend.get() != 1 {
                vec![Rgb::new(0, 255, 0); 2]
            } else {
                vec![Rgb::new(255, 0, 0), Rgb::new(0, 0, 255)]
            };
            Ok(Some(Frame {
                image: Arc::new(Image::new(2, 1, colors).unwrap()),
                observed_ms: at,
                new_pixels: true,
                backend: Backend::Dxgi,
                manual_crop: crop,
            }))
        }
    }
    fn wait(worker: &Worker, predicate: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let snapshot = worker.snapshot();
            if predicate(&snapshot) {
                return snapshot;
            }
            assert!(Instant::now() < deadline, "{snapshot:?}");
            // The auto-reset wake belongs solely to its worker, never to a test observer.
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn finish(worker: &mut Worker) {
        worker.stop();
        wait(worker, |s| {
            matches!(s.phase, Phase::Stopped | Phase::Failed)
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        while !worker.reap() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(worker.is_reaped());
    }
    fn feed(
        polls: mpsc::Sender<()>,
        gate: Option<mpsc::Receiver<()>>,
        dropped: mpsc::Sender<std::thread::ThreadId>,
        mode: Arc<Mutex<u8>>,
    ) -> Feed {
        Feed {
            polls,
            gate,
            dropped,
            owner: std::thread::current().id(),
            nonsend: Rc::new(Cell::new(0)),
            mode,
        }
    }
    #[test]
    fn blocked_factory_latest_pause_and_resume_are_nonblocking_and_provider_is_not_send() {
        let (config, setup) = fixture();
        let (entered_tx, entered) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        let (poll_tx, polls) = mpsc::channel();
        let (drop_tx, drops) = mpsc::channel();
        let mut worker = Worker::start(1, config, setup, move |_| {
            entered_tx.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(3)).unwrap();
            Ok(feed(poll_tx, None, drop_tx, Arc::new(Mutex::new(0))))
        })
        .unwrap();
        entered.recv_timeout(Duration::from_secs(3)).unwrap();
        worker
            .submit(Intent {
                revision: 1,
                active: true,
                suspended: false,
            })
            .unwrap();
        worker
            .submit(Intent {
                revision: 2,
                active: true,
                suspended: true,
            })
            .unwrap();
        worker
            .submit(Intent {
                revision: 3,
                active: true,
                suspended: false,
            })
            .unwrap();
        assert_eq!(worker.snapshot().last_accepted_revision, 3);
        assert_eq!(worker.snapshot().replaced, 2);
        release_tx.send(()).unwrap();
        polls.recv_timeout(Duration::from_secs(3)).unwrap();
        let snapshot = wait(&worker, |s| {
            s.processed_revision == 3 && !s.frames.is_empty()
        });
        assert_ne!(snapshot.frames["strip"].as_ref(), [Rgb::default(); 2]);
        finish(&mut worker);
        assert_ne!(
            drops.recv_timeout(Duration::from_secs(3)).unwrap(),
            std::thread::current().id()
        );
    }
    #[test]
    fn blocked_poll_stop_does_not_publish_late_frame_or_release_provider_early() {
        let (config, setup) = fixture();
        let (poll_tx, polls) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        let (drop_tx, drops) = mpsc::channel();
        let mut worker = Worker::start(2, config, setup, move |_| {
            Ok(feed(
                poll_tx,
                Some(release),
                drop_tx,
                Arc::new(Mutex::new(0)),
            ))
        })
        .unwrap();
        worker
            .submit(Intent {
                revision: 1,
                active: true,
                suspended: false,
            })
            .unwrap();
        polls.recv_timeout(Duration::from_secs(3)).unwrap();
        worker.stop();
        assert_eq!(worker.snapshot().phase, Phase::Stopping);
        assert!(!worker.reap());
        assert!(drops.try_recv().is_err());
        assert!(
            !worker
                .submit(Intent {
                    revision: 2,
                    active: true,
                    suspended: false
                })
                .unwrap()
        );
        release_tx.send(()).unwrap();
        finish(&mut worker);
        assert!(worker.snapshot().frames.is_empty());
        drops.recv_timeout(Duration::from_secs(3)).unwrap();
    }
    #[test]
    fn pause_overwritten_during_blocked_poll_discards_that_capture_and_forces_fresh_poll() {
        let (config, setup) = fixture();
        let (poll_tx, polls) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        let (drop_tx, drops) = mpsc::channel();
        let mut worker = Worker::start(6, config, setup, move |_| {
            Ok(feed(
                poll_tx,
                Some(release),
                drop_tx,
                Arc::new(Mutex::new(3)),
            ))
        })
        .unwrap();
        worker
            .submit(Intent {
                revision: 1,
                active: true,
                suspended: false,
            })
            .unwrap();
        polls.recv_timeout(Duration::from_secs(3)).unwrap();
        worker
            .submit(Intent {
                revision: 2,
                active: true,
                suspended: true,
            })
            .unwrap();
        worker
            .submit(Intent {
                revision: 3,
                active: true,
                suspended: false,
            })
            .unwrap();
        assert!(worker.snapshot().frames.is_empty());
        assert_eq!(worker.snapshot().processed_revision, 0);
        release_tx.send(()).unwrap();
        polls.recv_timeout(Duration::from_secs(3)).unwrap();
        let fresh = wait(&worker, |snapshot| {
            snapshot.processed_revision == 3
                && snapshot
                    .frames
                    .get("strip")
                    .is_some_and(|colors| colors[0].g > colors[0].r)
        });
        assert!(fresh.source_status["monitor"].observed_ms.is_some());
        finish(&mut worker);
        drops.recv_timeout(Duration::from_secs(3)).unwrap();
    }
    #[test]
    fn inactive_and_suspended_do_not_poll_then_resume_replaces_cached_frame() {
        let (config, setup) = fixture();
        let (poll_tx, polls) = mpsc::channel();
        let (drop_tx, drops) = mpsc::channel();
        let mut worker = Worker::start(3, config, setup, move |_| {
            Ok(feed(poll_tx, None, drop_tx, Arc::new(Mutex::new(0))))
        })
        .unwrap();
        wait(&worker, |s| s.phase == Phase::Ready);
        assert!(polls.try_recv().is_err());
        worker
            .submit(Intent {
                revision: 1,
                active: false,
                suspended: false,
            })
            .unwrap();
        let black = wait(&worker, |s| s.processed_revision == 1);
        assert_eq!(black.frames["strip"].as_ref(), [Rgb::default(); 2]);
        assert!(polls.try_recv().is_err());
        worker
            .submit(Intent {
                revision: 2,
                active: true,
                suspended: true,
            })
            .unwrap();
        wait(&worker, |s| s.processed_revision == 2);
        assert!(worker.snapshot().frames.is_empty());
        assert!(polls.try_recv().is_err());
        worker
            .submit(Intent {
                revision: 3,
                active: true,
                suspended: false,
            })
            .unwrap();
        let fresh = wait(&worker, |s| {
            s.processed_revision == 3 && !s.frames.is_empty()
        });
        let old = fresh.frames["strip"].clone();
        let revision = fresh.frame_revision;
        worker
            .submit(Intent {
                revision: 4,
                active: true,
                suspended: false,
            })
            .unwrap();
        let same = wait(&worker, |s| s.processed_revision == 4);
        assert!(Arc::ptr_eq(&old, &same.frames["strip"]));
        assert_eq!(same.frame_revision, revision);
        worker
            .submit(Intent {
                revision: 5,
                active: true,
                suspended: true,
            })
            .unwrap();
        worker
            .submit(Intent {
                revision: 6,
                active: true,
                suspended: false,
            })
            .unwrap();
        let resumed = wait(&worker, |s| {
            s.processed_revision == 6 && !s.frames.is_empty()
        });
        assert!(!Arc::ptr_eq(&old, &resumed.frames["strip"]));
        finish(&mut worker);
        drops.recv_timeout(Duration::from_secs(3)).unwrap();
    }
    #[test]
    fn source_errors_recover_but_constructor_panic_and_stale_revisions_are_terminal_or_rejected() {
        let (config, setup) = fixture();
        let (poll_tx, _polls) = mpsc::channel();
        let (drop_tx, drops) = mpsc::channel();
        let mode = Arc::new(Mutex::new(1));
        let owner_mode = mode.clone();
        let mut worker = Worker::start(4, config.clone(), setup.clone(), move |_| {
            Ok(feed(poll_tx, None, drop_tx, owner_mode))
        })
        .unwrap();
        assert!(
            worker
                .submit(Intent {
                    revision: 0,
                    active: true,
                    suspended: false
                })
                .is_err()
        );
        worker
            .submit(Intent {
                revision: 1,
                active: true,
                suspended: false,
            })
            .unwrap();
        assert!(
            worker
                .submit(Intent {
                    revision: 1,
                    active: true,
                    suspended: false
                })
                .is_err()
        );
        wait(&worker, |s| s.source_status["monitor"].error.is_some());
        assert_eq!(worker.snapshot().phase, Phase::Ready);
        *mode.lock().unwrap() = 0;
        wait(&worker, |s| {
            s.source_status["monitor"].observed_ms.is_some()
                && s.source_status["monitor"].error.is_none()
        });
        *mode.lock().unwrap() = 2;
        wait(&worker, |s| s.phase == Phase::Failed);
        finish(&mut worker);
        drops.recv_timeout(Duration::from_secs(3)).unwrap();
        let mut failure = Worker::start(5, config, setup, |_| -> Result<Feed, String> {
            Err("synthetic constructor error".into())
        })
        .unwrap();
        let snapshot = wait(&failure, |s| s.phase == Phase::Failed);
        assert_eq!(
            snapshot.error.as_deref(),
            Some("synthetic constructor error")
        );
        finish(&mut failure);
        let (config, setup) = fixture();
        let mut panicked = Worker::start(7, config, setup, |_| -> Result<Feed, String> {
            panic!("synthetic factory panic");
        })
        .unwrap();
        let snapshot = wait(&panicked, |snapshot| snapshot.phase == Phase::Failed);
        assert_eq!(snapshot.error.as_deref(), Some("Capture worker panicked"));
        finish(&mut panicked);
    }
}
