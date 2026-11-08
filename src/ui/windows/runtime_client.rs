//! Window-side data and short commands. No engine lock survives a call into native UI.
use crate::app::{
    document::Document,
    runtime::{
        OutputPlan, Runtime,
        coordinator::{Coordinator, View},
    },
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

enum Owner {
    Manual(Box<Runtime>),
    Background(Coordinator),
}

pub(super) struct Client {
    owner: Owner,
    view: Arc<View>,
    fatal: Option<String>,
    reported_error: Option<String>,
}
macro_rules! command {
    ($name:ident($($arg:ident:$ty:ty),* $(,)?) -> $result:ty) => {
        pub(super) fn $name(&mut self, $($arg:$ty),*) -> Result<$result,String> {
            self.with(|r,_| r.$name($($arg),*))
        }
    };
}
impl Client {
    pub(super) fn new(runtime: Runtime, background: bool) -> Result<Self, String> {
        let view = View::capture(&runtime, None);
        let owner = if background {
            Owner::Background(Coordinator::start(runtime)?)
        } else {
            Owner::Manual(Box::new(runtime))
        };
        let mut value = Self {
            owner,
            view,
            fatal: None,
            reported_error: None,
        };
        value.refresh();
        Ok(value)
    }
    pub(super) fn background(&self) -> bool {
        matches!(self.owner, Owner::Background(_))
    }
    pub(super) fn owner_ticks(&self) -> Option<u64> {
        match &self.owner {
            Owner::Manual(_) => None,
            Owner::Background(c) => Some(c.status().ticks),
        }
    }
    pub(super) fn read<R>(&self, f: impl FnOnce(&Runtime) -> R) -> R {
        match &self.owner {
            Owner::Manual(r) => f(r),
            Owner::Background(c) => c.read(f),
        }
    }
    pub(super) fn with<R>(
        &mut self,
        f: impl FnOnce(&mut Runtime, u64) -> Result<R, String>,
    ) -> Result<R, String> {
        let result = match &mut self.owner {
            Owner::Manual(r) => f(r, crate::clock::monotonic_us()),
            Owner::Background(c) => c.command(f),
        };
        self.refresh();
        result
    }
    fn recovery<R>(
        &mut self,
        f: impl FnOnce(&mut Runtime) -> Result<R, String>,
    ) -> Result<R, String> {
        let result = match &mut self.owner {
            Owner::Manual(r) => f(r),
            Owner::Background(c) => c.recovery(f),
        };
        self.refresh();
        result
    }
    pub(super) fn refresh(&mut self) {
        match &self.owner {
            Owner::Manual(r) => self.view = View::capture(r, Some(&self.view)),
            Owner::Background(c) => {
                self.view = c.view();
                let s = c.status();
                self.fatal = if s.fatal { s.error } else { None };
            }
        }
    }
    pub(super) fn maintenance<R>(
        &mut self,
        f: impl FnOnce(&mut Runtime, u64) -> Result<R, String>,
    ) -> Result<R, String> {
        if self.fatal.is_some() {
            self.recovery(|r| f(r, crate::clock::monotonic_us()))
        } else {
            self.with(f)
        }
    }
    pub(super) fn tick(&mut self, now: u64, wall: i64) -> Result<(), String> {
        let result = if let Owner::Manual(r) = &mut self.owner {
            r.tick(now, wall)
        } else {
            Ok(())
        };
        self.refresh();
        if let Err(error) = result {
            self.reported_error = self.error().map(str::to_owned);
            return Err(error);
        }
        if let Some(error) = &self.fatal {
            return Err(error.clone());
        }
        // A failed background tick may have completed before the window polls it.
        let error = self.error().map(str::to_owned);
        if error != self.reported_error {
            self.reported_error = error.clone();
            if let Some(error) = error {
                return Err(error);
            }
        }
        Ok(())
    }
    pub(super) fn stop(&mut self) {
        match &mut self.owner {
            Owner::Manual(r) => r.stop(),
            Owner::Background(c) => c.stop(),
        };
        self.refresh();
    }
    pub(super) fn drained(&mut self) -> bool {
        match &mut self.owner {
            Owner::Manual(r) => r.drained(),
            Owner::Background(c) => c.drained(),
        }
    }
    pub(super) fn request_shutdown(
        &mut self,
        s: &mut crate::app::shutdown::Shutdown,
    ) -> Result<(), String> {
        if !s.active() {
            self.stop();
        }
        self.recovery(|r| {
            s.request(r);
            Ok(())
        })
    }
    pub(super) fn poll_shutdown(
        &mut self,
        s: &mut crate::app::shutdown::Shutdown,
    ) -> Result<(), String> {
        self.recovery(|r| {
            s.poll(r);
            Ok(())
        })
    }

    pub(super) fn document(&self) -> &Document {
        &self.view.document
    }
    pub(super) fn requested_document(&self) -> &Document {
        &self.view.requested
    }
    pub(super) fn project(&self) -> &crate::app::config::Project {
        &self.view.document.lighting
    }
    pub(super) fn colors(&self) -> &[crate::lighting::color::Rgb] {
        &self.view.colors
    }
    pub(super) fn temperature_samples(&self) -> &[crate::app::lcd::Sample] {
        &self.view.samples
    }
    pub(super) fn group_on(&self, t: crate::schedule::Target) -> bool {
        match t {
            crate::schedule::Target::Pc => self.view.desired.pc,
            crate::schedule::Target::Door => self.view.desired.door,
        }
    }
    pub(super) fn view_revision(&self) -> u64 {
        self.view.revision
    }
    pub(super) fn pending_configuration(&self) -> bool {
        self.view.pending
    }
    pub(super) fn configuration_error(&self) -> Option<&str> {
        self.view.configuration_error.as_deref()
    }
    pub(super) fn error(&self) -> Option<&str> {
        self.fatal.as_deref().or(self.view.error.as_deref())
    }
    pub(super) fn autonomous_rgb(&self) -> bool {
        self.view.autonomous_rgb
    }
    pub(super) fn temperature_input_ready(&self) -> bool {
        self.view.temperature_ready
    }
    pub(super) fn lcd_status(&self, id: &str) -> Option<crate::app::lcd_worker::Snapshot> {
        self.read(|r| r.lcd_status(id))
    }
    pub(super) fn capture_status(&self) -> Option<crate::app::capture_worker::Snapshot> {
        self.read(Runtime::capture_status)
    }

    pub(super) fn rgb_status(&self, id: &str) -> Option<crate::runtime::output::Snapshot> {
        self.read(|r| r.host().status(id))
    }
    pub(super) fn output_plan(&self) -> OutputPlan {
        self.read(Runtime::output_plan)
    }
    pub(super) fn output_revision(&self) -> u64 {
        self.read(Runtime::output_revision)
    }
    pub(super) fn outputs_ready(&self) -> Result<bool, String> {
        self.read(Runtime::outputs_ready)
    }
    pub(super) fn save(&self, path: &Path) -> Result<(), String> {
        self.read(|r| r.save(path))
    }
    pub(super) fn export_journal_for(&self, doc: &Document, path: PathBuf) -> Result<(), String> {
        self.read(|r| r.export_journal_for(doc, path))
    }
    pub(super) fn accept_temperatures(
        &mut self,
        samples: &[crate::app::lcd::Sample],
        _: u64,
    ) -> Result<(), String> {
        self.with(|r, now| r.accept_temperatures(samples, now))
    }
    pub(super) fn refresh_devices(&mut self, _: u64) -> Result<(), String> {
        self.with(|r, now| r.refresh_devices(now))
    }
    pub(super) fn notify_power(&mut self, suspended: bool, _: u64) -> Result<(), String> {
        self.with(|r, now| r.notify_power(suspended, now))
    }

    command!(set_group(t: crate::schedule::Target, on: bool) -> ());
    command!(set_scene(scene: crate::runtime::scene::Scene) -> ());
    command!(configure_schedule(config: crate::schedule::Config) -> ());
    command!(request_document_at(document: Document, path: PathBuf) -> ());
    command!(request_outputs_for(document: Document, plan: OutputPlan) -> u64);
    command!(relocate_journal(path: PathBuf) -> ());
    command!(retry_configuration()->());
    command!(cancel_configuration()->());
}

#[cfg(test)]
mod tests {
    use super::*;
    fn engine() -> Runtime {
        Runtime::new(
            Document::from_lighting(crate::app::config::Project {
                version: 1,
                setup: crate::devices::Setup {
                    version: 1,
                    devices: vec![],
                    canvas: crate::devices::Canvas {
                        width: 1,
                        height: 1,
                        points: vec![],
                    },
                },
                scene: crate::runtime::scene::example(),
                schedule: Default::default(),
                groups: crate::app::config::Groups {
                    pc: vec![],
                    door: vec![],
                },
            })
            .unwrap(),
            None,
        )
        .unwrap()
    }
    fn wait(mut ready: impl FnMut() -> bool) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !ready() {
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    #[test]
    fn window_client_manual_snapshot_and_background_clock_do_not_mix() {
        let mut manual = Client::new(engine(), false).unwrap();
        assert_eq!(manual.owner_ticks(), None);
        assert!(!manual.autonomous_rgb());
        manual.tick(1, 1_800_000_000).unwrap();
        manual.stop();
        assert!(manual.drained());
        let mut live = Client::new(engine(), true).unwrap();
        wait(|| live.owner_ticks().unwrap() > 0);
        // Window timestamps are not admitted to the background clock.
        live.tick(u64::MAX, i64::MIN).unwrap();
        assert!(live.background());
        assert!(live.autonomous_rgb());
        live.stop();
        wait(|| live.drained());
    }
    #[test]
    fn window_client_fatal_owner_can_still_poll_and_complete_shutdown() {
        let mut live = Client::new(engine(), true).unwrap();
        let result: Result<(), String> = live.with(|_, _| panic!("Injected window command panic"));
        assert!(result.is_err());
        assert!(live.error().is_some());
        live.maintenance(|r, _| {
            assert!(!r.temperature_input_ready());
            Ok(())
        })
        .unwrap();
        assert!(live.set_group(crate::schedule::Target::Pc, true).is_err());
        assert!(
            live.maintenance(|r, _| r.set_group(crate::schedule::Target::Pc, true))
                .is_err()
        );
        let mut shutdown = crate::app::shutdown::Shutdown::default();
        live.request_shutdown(&mut shutdown).unwrap();
        wait(|| {
            live.poll_shutdown(&mut shutdown).unwrap();
            live.drained() && shutdown.phase() == crate::app::shutdown::Phase::Ready
        });
    }
    #[test]
    fn window_client_reports_a_background_persistence_failure_once() {
        let parent =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("work/window-client-error-stand");
        std::fs::create_dir_all(&parent).unwrap();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let blocked = parent.join(format!("{}-{nonce}", std::process::id()));
        std::fs::create_dir(&blocked).unwrap();
        let mut live = Client::new(engine(), true).unwrap();
        let mut next = live.document().clone();
        next.lighting.scene.brightness = 7;
        live.request_document_at(next, blocked.clone()).unwrap();
        wait(|| live.read(|r| r.configuration_error().is_some()));
        assert!(
            live.tick(crate::clock::monotonic_us(), 1_800_000_000)
                .is_err()
        );
        assert!(live.configuration_error().is_some());
        assert!(
            live.tick(crate::clock::monotonic_us(), 1_800_000_000)
                .is_ok()
        );
        assert_ne!(live.project().scene.brightness, 7);
        live.stop();
        wait(|| live.drained());
        std::fs::remove_dir(&blocked).unwrap();
    }
}
