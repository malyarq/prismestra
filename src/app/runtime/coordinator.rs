//! Single off-window Runtime owner. Hardware capabilities still require explicit factories.
use super::{Desired, Document, Runtime, lcd};
use crate::lighting::color::Rgb;
use std::{
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const PERIOD: Duration = Duration::from_millis(50);

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub running: bool,
    pub ticks: u64,
    pub error: Option<String>,
    pub fatal: bool,
}
/// Immutable UI data; no engine/transport handles and no borrowed mutex guards.
pub struct View {
    pub document: Arc<Document>,
    pub requested: Arc<Document>,
    pub colors: Arc<[Rgb]>,
    pub samples: Arc<[lcd::Sample]>,
    pub desired: Desired,
    pub revision: u64,
    pub pending: bool,
    pub configuration_error: Option<String>,
    pub error: Option<String>,
    pub temperature_ready: bool,
    pub autonomous_rgb: bool,
    document_revision: u64,
}
impl View {
    pub(crate) fn capture(runtime: &Runtime, old: Option<&Self>) -> Arc<Self> {
        let document = old
            .filter(|v| {
                v.document_revision == runtime.document_revision
                    || v.document.as_ref() == runtime.document()
            })
            .map(|v| v.document.clone())
            .unwrap_or_else(|| Arc::new(runtime.document().clone()));
        let pending = runtime.pending_configuration();
        let requested = if pending {
            old.filter(|v| v.pending && v.requested.as_ref() == runtime.requested_document())
                .map(|v| v.requested.clone())
                .unwrap_or_else(|| Arc::new(runtime.requested_document().clone()))
        } else {
            document.clone()
        };
        let colors = old
            .filter(|v| v.revision == runtime.view_revision())
            .map(|v| v.colors.clone())
            .unwrap_or_else(|| Arc::from(runtime.colors()));
        Arc::new(Self {
            document,
            requested,
            colors,
            samples: runtime.samples.clone(),
            desired: runtime.desired(),
            revision: runtime.view_revision(),
            pending,
            configuration_error: runtime.configuration_error().map(str::to_owned),
            error: runtime.error().map(str::to_owned),
            temperature_ready: runtime.temperature_input_ready(),
            autonomous_rgb: runtime.autonomous_rgb(),
            document_revision: runtime.document_revision,
        })
    }
}
struct State {
    runtime: Runtime,
    status: Status,
    revision: u64,
    view: Arc<View>,
}
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}
pub struct Coordinator {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}
fn message(error: String) -> String {
    error.chars().take(512).collect()
}
fn wall() -> Result<i64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "Coordinator wall clock predates UNIX epoch".to_string())?
        .as_secs()
        .try_into()
        .map_err(|_| "Coordinator wall clock overflow".to_string())
}
impl Coordinator {
    /// Takes ownership; no backend discovery or physical factory is created here.
    pub fn start(runtime: Runtime) -> Result<Self, String> {
        Self::start_with_wall(runtime, wall)
    }
    pub(super) fn start_with_wall(
        mut runtime: Runtime,
        wall: impl Fn() -> Result<i64, String> + Send + 'static,
    ) -> Result<Self, String> {
        if runtime.stopped
            || runtime
                .last_us
                .is_some_and(|at| at > crate::clock::monotonic_us())
        {
            return Err("Coordinator requires a running Runtime on the process clock".into());
        }
        runtime.enable_autonomous_rgb()?;
        let view = View::capture(&runtime, None);
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                runtime,
                status: Status {
                    running: true,
                    ..Status::default()
                },
                revision: 0,
                view,
            }),
            wake: Condvar::new(),
        });
        let owner = shared.clone();
        let thread = std::thread::Builder::new()
            .name("pc-coordinator".into())
            .spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&owner, wall)));
                let mut state = owner.state.lock().unwrap_or_else(|e| e.into_inner());
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        state.status.error = Some(message(error));
                        state.status.fatal = true;
                    }
                    Err(_) => {
                        state.status.error =
                            Some("Runtime coordinator panicked; outputs stopped".into());
                        state.status.fatal = true;
                    }
                }
                if state.status.fatal {
                    state.runtime.stop();
                }
                state.status.running = false;
                state.view = View::capture(&state.runtime, Some(&state.view));
                owner.wake.notify_all();
            })
            .map_err(|e| format!("Start Runtime coordinator: {e}"))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }
    /// Borrowed engine data cannot escape the closure or outlive the owner lock.
    /// Do not re-enter this Coordinator from a read/command closure.
    pub fn read<R>(&self, read: impl FnOnce(&Runtime) -> R) -> R {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        read(&state.runtime)
    }
    /// Clock is sampled under the same lock as the command, preventing timestamp races.
    /// Keep commands bounded: file dialogs, rendering and waiting belong outside this closure.
    pub fn command<R>(
        &self,
        edit: impl FnOnce(&mut Runtime, u64) -> Result<R, String>,
    ) -> Result<R, String> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.status.fatal {
            return Err(state
                .status
                .error
                .clone()
                .unwrap_or("Coordinator failed".into()));
        }
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            edit(&mut state.runtime, crate::clock::monotonic_us())
        })) {
            Ok(result) => result,
            Err(_) => {
                let error = "Runtime command panicked; outputs stopped".to_string();
                state.status.fatal = true;
                state.status.error = Some(error.clone());
                state.runtime.stop();
                Err(error)
            }
        };
        state.revision = state.revision.wrapping_add(1);
        state.view = View::capture(&state.runtime, Some(&state.view));
        self.shared.wake.notify_all();
        result
    }
    pub fn status(&self) -> Status {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .clone()
    }
    pub fn view(&self) -> Arc<View> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .view
            .clone()
    }
    /// Only stopped engines admit recovery operations; no ordinary control may restart them.
    pub(crate) fn recovery<R>(
        &self,
        edit: impl FnOnce(&mut Runtime) -> Result<R, String>,
    ) -> Result<R, String> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.runtime.stopped {
            return Err("Recovery requires a stopped Runtime".into());
        }
        let result = edit(&mut state.runtime);
        state.view = View::capture(&state.runtime, Some(&state.view));
        result
    }
    pub fn stop(&self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.runtime.stop();
        state.view = View::capture(&state.runtime, Some(&state.view));
        state.revision = state.revision.wrapping_add(1);
        self.shared.wake.notify_all();
    }

    pub fn drained(&mut self) -> bool {
        if self.thread.as_ref().is_some_and(|t| t.is_finished()) {
            let _ = self.thread.take().unwrap().join();
        }
        let engines = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .runtime
            .drained();
        self.thread.is_none() && engines
    }
}
impl Drop for Coordinator {
    fn drop(&mut self) {
        self.stop();
        // Runtime tick contains coordination only; device calls remain on their own owners.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn run(shared: &Shared, wall: impl Fn() -> Result<i64, String>) -> Result<(), String> {
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if state.runtime.stopped {
            return Ok(());
        }
        let revision = state.revision;
        let now = crate::clock::monotonic_us();
        if now == u64::MAX || state.runtime.last_us.is_some_and(|at| at > now) {
            return Err("Coordinator process clock moved backwards or overflowed".into());
        }
        // Recoverable persistence/configuration errors stay visible. Keep servicing output workers.
        state.status.error = state.runtime.tick(now, wall()?).err().map(message);
        state.status.ticks = state.status.ticks.saturating_add(1);
        state.view = View::capture(&state.runtime, Some(&state.view));
        state = shared
            .wake
            .wait_timeout_while(state, PERIOD, |s| {
                !s.runtime.stopped && s.revision == revision
            })
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}
