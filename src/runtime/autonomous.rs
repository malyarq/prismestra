//! Off-window RGB calculation; actual device I/O stays on the existing output workers.
use super::cadence_wait as wait;
use super::*;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};
#[derive(Clone, Debug)]
pub struct ClockStatus {
    pub running: bool,
    pub high_resolution: bool,
    pub error: Option<String>,
}
struct Clock {
    wake: Arc<wait::Signal>,
    stop: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    precise: bool,
    thread: Option<JoinHandle<()>>,
}
impl Clock {
    fn start(core: Arc<Mutex<Core>>) -> Result<Self, String> {
        let wake = Arc::new(wait::Signal::new()?);
        let timer = wait::Timer::new()?;
        let precise = timer.precise();
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let (signal, cancel, failure) = (wake.clone(), stop.clone(), error.clone());
        let thread = std::thread::Builder::new()
            .name("rgb-canvas".into())
            .spawn(move || {
                let result = (|| {
                    let mut seen = 0;
                    while !cancel.load(Ordering::Acquire) {
                        let due = {
                            let mut core = core.lock().unwrap_or_else(|e| e.into_inner());
                            if cancel.load(Ordering::Acquire) {
                                break;
                            }
                            core.reap();
                            if core.outputs.values().any(|o| !o.is_reaped()) {
                                core.tick(crate::clock::monotonic_us())?;
                                let mut due = core.scene_cadence.next;
                                if let Some(expiry) = core.feed_deadline(core.last_us.unwrap_or(0))
                                {
                                    due = Some(due.map_or(expiry, |v| v.min(expiry)));
                                }
                                for (id, cadence) in &core.cadences {
                                    if core.outputs.get(id).is_some_and(|o| !o.is_reaped())
                                        && let Some(next) = cadence.next
                                    {
                                        due = Some(due.map_or(next, |v| v.min(next)));
                                    }
                                }
                                due
                            } else {
                                None
                            }
                        };
                        let duration = due.map(|due| {
                            Duration::from_micros(
                                due.saturating_sub(crate::clock::monotonic_us()).max(1),
                            )
                        });
                        timer.wait(&signal, &mut seen, duration)?;
                    }
                    Ok::<_, String>(())
                })();
                if let Err(e) = result {
                    *failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                    core.lock().unwrap_or_else(|e| e.into_inner()).stop();
                }
            })
            .map_err(|e| format!("Start RGB canvas clock: {e}"))?;
        Ok(Self {
            wake,
            stop,
            error,
            precise,
            thread: Some(thread),
        })
    }
    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.wake.notify();
    }
    fn reap(&mut self) -> bool {
        if self.thread.as_ref().is_some_and(|t| t.is_finished())
            && let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            *self.error.lock().unwrap_or_else(|e| e.into_inner()) =
                Some("RGB canvas thread panicked".into());
        }
        self.thread.is_none()
    }
    fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}
impl Drop for Clock {
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
pub struct Host {
    core: Arc<Mutex<Core>>,
    setup: Arc<Setup>,
    scene: Scene,
    frame: Arc<FrameView>,
    clock: Option<Clock>,
    autonomous: bool,
}
impl Host {
    pub fn new(setup: Setup, scene: Scene) -> Result<Self, String> {
        let core = Core::new(setup, scene.clone())?;
        Ok(Self {
            setup: core.setup.clone(),
            scene,
            frame: core.frame.clone(),
            core: Arc::new(Mutex::new(core)),
            clock: None,
            autonomous: false,
        })
    }
    /// Explicit real-clock mode. Deterministic/manual callers remain thread-free by default.
    pub fn enable_autonomous(&mut self) -> Result<(), String> {
        if self
            .core
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_us
            .is_some_and(|last| last > crate::clock::monotonic_us())
        {
            return Err("Autonomous RGB requires the shared real monotonic clock".into());
        }
        self.start_clock()?;
        self.autonomous = true;
        Ok(())
    }
    fn start_clock(&mut self) -> Result<(), String> {
        if let Some(clock) = &mut self.clock
            && !clock.reap()
        {
            return if clock.stop.load(Ordering::Acquire) {
                Err("Previous RGB clock is still draining".into())
            } else {
                Ok(())
            };
        }
        let clock = Clock::start(self.core.clone())?;
        let feed = self
            .core
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .feed
            .clone();
        if let Some(feed) = feed {
            feed.subscribe(&clock.wake)?;
        }
        self.clock = Some(clock);
        Ok(())
    }
    fn notify(&self) {
        if let Some(clock) = &self.clock {
            clock.wake.notify();
        }
    }
    pub fn clock_status(&self) -> Option<ClockStatus> {
        self.clock.as_ref().map(|c| ClockStatus {
            running: c.thread.as_ref().is_some_and(|t| !t.is_finished()),
            high_resolution: c.precise,
            error: c.error(),
        })
    }
    pub fn setup(&self) -> &Setup {
        &self.setup
    }
    pub fn scene(&self) -> &Scene {
        &self.scene
    }
    pub fn preview(&self) -> &[Rgb] {
        &self.frame.preview
    }
    /// Latest UI snapshot; the autonomous thread does not borrow UI-owned data.
    pub fn frame(&self, id: &str) -> Option<&[Rgb]> {
        self.frame.frames.get(id).map(Vec::as_slice)
    }
    pub fn requested(&self, id: &str) -> Option<Ticket> {
        self.core
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .requested(id)
    }
    pub fn status(&self, id: &str) -> Option<Snapshot> {
        self.core
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status(id)
    }
    pub fn configure_scene(&mut self, scene: Scene) -> Result<(), String> {
        self.core
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .configure_scene(scene.clone())?;
        self.scene = scene;
        self.notify();
        Ok(())
    }
    pub fn replace_setup(&mut self, setup: Setup) -> Result<(), String> {
        self.reap();
        if self.clock.as_mut().is_some_and(|c| !c.reap()) {
            return Err("Stop and drain RGB clock before replacing devices".into());
        }
        let mut core = self.core.lock().unwrap_or_else(|e| e.into_inner());
        core.replace_setup(setup)?;
        self.setup = core.setup.clone();
        self.frame = core.frame.clone();
        Ok(())
    }
    pub fn attach<F>(&mut self, id: &str, factory: F) -> Result<(), String>
    where
        F: FnOnce(Cancellation) -> Result<Box<dyn RgbDriver>, String> + Send + 'static,
    {
        self.attach_claimed(id, Vec::new(), factory)
    }
    pub(crate) fn attach_claimed<F>(
        &mut self,
        id: &str,
        keys: Vec<String>,
        factory: F,
    ) -> Result<(), String>
    where
        F: FnOnce(Cancellation) -> Result<Box<dyn RgbDriver>, String> + Send + 'static,
    {
        let already_running = self.clock.as_ref().is_some_and(|c| {
            !c.stop.load(Ordering::Acquire) && c.thread.as_ref().is_some_and(|t| !t.is_finished())
        });
        if self.autonomous {
            self.start_clock()?;
        }
        let attached = self
            .core
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .attach_claimed(id, keys, factory);
        if attached.is_err()
            && !already_running
            && let Some(clock) = &self.clock
        {
            // A rejected replacement must not leave a fresh idle clock blocking drain.
            clock.stop();
        }
        attached?;
        self.notify();
        Ok(())
    }
    pub fn mute(&mut self, id: &str, off: bool) -> Result<(), String> {
        self.core
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .mute(id, off)?;
        self.notify();
        Ok(())
    }
    pub fn external_feed_generation(&self) -> Option<u64> {
        self.core
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .feed
            .as_ref()
            .map(|r| r.snapshot().generation)
    }
    pub fn bind_external_feed(&mut self, feed: frame_feed::Reader) -> Result<(), String> {
        self.core
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .validate_feed(&feed)?;
        if let Some(clock) = &self.clock {
            feed.subscribe(&clock.wake)?;
        }
        self.core
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .bind_feed(feed)?;
        self.notify();
        Ok(())
    }
    pub fn clear_external_feed(&mut self) {
        self.core
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear_feed();
        self.notify();
    }
    pub fn set_external(&mut self, id: &str, colors: Vec<Rgb>) -> Result<(), String> {
        self.core
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_external(id, colors)?;
        self.notify();
        Ok(())
    }
    fn try_core(&self) -> Option<std::sync::MutexGuard<'_, Core>> {
        match self.core.try_lock() {
            Ok(core) => Some(core),
            Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }
    pub fn tick(&mut self, now_us: u64) -> Result<usize, String> {
        if self.autonomous {
            if let Some(error) = self.clock.as_ref().and_then(Clock::error) {
                return Err(error);
            }
            let frame = if let Some(mut core) = self.try_core() {
                // Empty/offline setups still provide a manual preview; there is no active I/O.
                if !core.outputs.values().any(|o| !o.is_reaped()) {
                    core.tick(crate::clock::monotonic_us())?;
                }
                Some(core.frame.clone())
            } else {
                None
            };
            if let Some(frame) = frame {
                self.frame = frame;
            }
            Ok(0)
        } else {
            let mut core = self.core.lock().unwrap_or_else(|e| e.into_inner());
            let submitted = core.tick(now_us)?;
            self.frame = core.frame.clone();
            Ok(submitted)
        }
    }
    pub fn stop(&self) {
        if let Some(clock) = &self.clock {
            clock.stop();
        }
        self.core.lock().unwrap_or_else(|e| e.into_inner()).stop();
    }
    pub fn reap(&mut self) {
        if let Some(clock) = &mut self.clock
            && clock.reap()
            && clock.error().is_some()
        {
            self.core.lock().unwrap_or_else(|e| e.into_inner()).stop();
        }
        if let Some(mut core) = self.try_core() {
            core.reap();
        }
    }
    pub fn drained(&mut self) -> bool {
        self.reap();
        if self.clock.as_mut().is_some_and(|c| !c.reap()) {
            return false;
        }
        self.try_core().is_some_and(|mut c| c.drained())
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.stop();
        self.clock.take();
        self.reap();
    }
}
#[cfg(test)]
mod tests;
