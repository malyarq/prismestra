//! Unified lighting coordinator. Factories are explicit capabilities; loading a
//! document never constructs a native backend. Slow capture/LCD/RGB own separate threads.
use super::{capture_worker, config::Project, document::Document, lcd, lcd_worker, sensor_worker};
use crate::{
    devices::Role,
    lifecycle::{Action, PowerCoordinator},
    lighting::color::Rgb,
    runtime::{
        Host,
        output::{Cancellation, RgbDriver},
    },
    schedule::{self, Schedule, Target},
    storage,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};
const LIMIT: usize = 1024 * 1024;
const SCOPE: &str = "pc-control.runtime-desired";
const ACCEPTED: &str = "Desired state persisted; transport completion is tracked separately";
pub mod coordinator;
#[cfg(windows)]
mod managed_backend;
mod outputs;
mod recovery;
pub use recovery::{OpenRgbRetryStatus, OpenRgbRetryTicket};
mod registry_input;
#[cfg(windows)]
pub(crate) use outputs::OutputActivity;
pub use outputs::{LcdOutput, OutputPlan, RgbOutput};
mod temperature_stream;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Desired {
    pub pc: bool,
    pub door: bool,
}
impl Desired {
    fn on(self, target: Target) -> bool {
        match target {
            Target::Pc => self.pc,
            Target::Door => self.door,
        }
    }
    fn set(&mut self, target: Target, on: bool) {
        match target {
            Target::Pc => self.pc = on,
            Target::Door => self.door = on,
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    scope: String,
    fingerprint: String,
    desired: Desired,
    ledger: serde_json::Value,
}
struct Store {
    path: Option<PathBuf>,
    fingerprint: String,
}
impl Store {
    fn new(path: Option<PathBuf>, document: &Document) -> Result<Self, String> {
        let bytes = serde_json::to_vec(&(&document.lighting.setup, &document.lighting.groups))
            .map_err(|e| e.to_string())?;
        let fingerprint = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(Self { path, fingerprint })
    }
    fn load(&self) -> Result<Option<(Schedule, Desired)>, String> {
        let Some(path) = &self.path else {
            return Ok(None);
        };
        let meta = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("Runtime journal metadata: {e}")),
        };
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > LIMIT as u64 {
            return Err("Runtime journal must be a bounded regular file".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err("Runtime journal cannot be a reparse point".into());
            }
        }
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|e| format!("Runtime journal open: {e}"))?
            .take(LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("Runtime journal read: {e}"))?;
        if bytes.len() > LIMIT {
            return Err("Runtime journal exceeds 1 MiB".into());
        }
        let j: Journal =
            serde_json::from_slice(&bytes).map_err(|e| format!("Runtime journal decode: {e}"))?;
        if j.version != 1 || j.scope != SCOPE || j.fingerprint != self.fingerprint {
            return Err("Runtime journal scope/version/topology mismatch".into());
        }
        let ledger = serde_json::to_vec(&j.ledger).map_err(|e| e.to_string())?;
        let schedule = Schedule::restore(&ledger)?;
        for target in [Target::Pc, Target::Door] {
            if let Some(a) = schedule.attempt(target) {
                match a.state {
                    schedule::AttemptState::Attempting if a.error.is_empty() => {}
                    schedule::AttemptState::Submitted if a.error == ACCEPTED => {}
                    _ => return Err("Runtime journal has an unknown desired-state outcome".into()),
                }
            }
        }
        Ok(Some((schedule, j.desired)))
    }
    fn persist(&self, desired: Desired, ledger: &[u8]) -> Result<(), String> {
        let j = Journal {
            version: 1,
            scope: SCOPE.into(),
            fingerprint: self.fingerprint.clone(),
            desired,
            ledger: serde_json::from_slice(ledger).map_err(|e| e.to_string())?,
        };
        let bytes = serde_json::to_vec(&j).map_err(|e| e.to_string())?;
        if bytes.len() > LIMIT {
            return Err("Runtime journal exceeds 1 MiB".into());
        }
        if let Some(path) = &self.path {
            storage::atomic_write(path, &bytes, LIMIT)
                .map_err(|e| format!("Runtime journal atomic replace: {e}"))?;
        }
        Ok(())
    }
}

type RgbOpen = dyn Fn(Cancellation) -> Result<Box<dyn RgbDriver>, String> + Send + Sync;
pub type RgbFactory = Arc<RgbOpen>;
pub type LcdTransport = Box<dyn crate::lcd::protocol::ReportTransport>;
pub type LcdFactory = Arc<
    dyn Fn(lcd::Config, Cancellation) -> Result<(lcd::Controller, LcdTransport), String>
        + Send
        + Sync,
>;
pub type CaptureFactory =
    Arc<dyn Fn(Cancellation) -> Result<Box<dyn super::ambient::Provider>, String> + Send + Sync>;
struct RgbSpec {
    keys: Vec<String>,
    factory: RgbFactory,
    action_required_error: Option<String>,
}
struct LcdSpec {
    keys: Vec<String>,
    factory: LcdFactory,
}
struct SensorSpec {
    keys: Vec<String>,
    factory: sensor_worker::Factory,
}

enum ConfigurationPhase {
    Draining,
    Fault(String),
    Cancelling,
}
struct Configuration {
    outputs: Option<OutputPlan>,
    live_rgb: Option<LiveRgbPlan>,
    #[cfg(windows)]
    startup_managed_ids: Option<BTreeSet<String>>,
    document: Document,
    phase: ConfigurationPhase,
    journal_override: Option<PathBuf>,
}
#[derive(Clone)]
struct LiveRgbPlan {
    retained: BTreeSet<String>,
    retiring: BTreeSet<String>,
    added_lcd: BTreeSet<String>,
}
#[derive(PartialEq, Eq)]
struct RgbView {
    phase: crate::runtime::output::Phase,
    generation: u64,
    received: u64,
    replaced: u64,
    submitted: u64,
    deferred: u64,
    last: Option<crate::runtime::output::Ticket>,
    error: Option<String>,
}
#[derive(PartialEq, Eq)]
struct LcdView {
    phase: lcd_worker::Phase,
    generation: u64,
    accepted: u64,
    processed: u64,
    frames: u64,
    replaced: u64,
    state: Option<lcd::State>,
    error: Option<String>,
}
#[derive(PartialEq, Eq)]
struct CaptureView {
    phase: capture_worker::Phase,
    generation: u64,
    accepted: u64,
    processed: u64,
    frame: u64,
    replaced: u64,
    sources: BTreeMap<String, super::ambient::SourceStatus>,
    error: Option<String>,
}

pub struct Runtime {
    document: Document,
    document_revision: u64,
    host: Host,
    autonomous_rgb: bool,
    store: Store,
    schedule: Schedule,
    desired: Desired,
    lcd: BTreeMap<String, lcd_worker::Worker>,
    capture: Option<capture_worker::Worker>,
    sensors: BTreeMap<String, sensor_worker::Worker>,
    sensor_specs: BTreeMap<String, SensorSpec>,
    sensor_stamps: BTreeMap<String, (u64, u64)>,
    injected_inputs: Vec<lcd::Sample>,
    registry_input: Option<registry_input::Input>,
    retired_registry: Vec<registry_input::Retired>,
    temperature_stream: Option<temperature_stream::State>,
    temperature_due_ms: Option<u64>,
    output_revision: u64,
    rgb_specs: BTreeMap<String, RgbSpec>,
    lcd_specs: BTreeMap<String, LcdSpec>,
    output_recovery: BTreeMap<String, recovery::Recovery>,
    lcd_arrivals: BTreeSet<String>,
    capture_factory: Option<CaptureFactory>,
    samples: Arc<[lcd::Sample]>,
    revision: u64,
    generation: u64,
    capture_active: Option<bool>,
    last_us: Option<u64>,
    last_wall: Option<i64>,
    power: PowerCoordinator,
    refresh: Option<u64>,
    suspended: bool,
    stopped: bool,
    refresh_started: bool,
    retry_action_required_on_refresh: bool,
    error: Option<String>,
    pending: Option<Configuration>,
    colors: Vec<Rgb>,
    point_targets: Vec<Target>,
    view_revision: u64,
    rgb_view: BTreeMap<String, RgbView>,
    lcd_view: BTreeMap<String, LcdView>,
    capture_view: Option<CaptureView>,
    view_error: Option<String>,
    sample_freshness: Vec<bool>,
    #[cfg(windows)]
    managed_backend: Option<super::backend_helper::Backend>,
    #[cfg(windows)]
    managed_startup_guard: Option<managed_backend::StartupGuard>,
    #[cfg(windows)]
    openrgb_policies: managed_backend::OpenRgbPolicies,
    #[cfg(windows)]
    managed_backend_ids: BTreeSet<String>,
    #[cfg(windows)]
    managed_backend_generation: u64,
    #[cfg(windows)]
    managed_backend_detail: Option<String>,
}
impl Runtime {
    pub fn new(document: Document, journal: Option<PathBuf>) -> Result<Self, String> {
        document.validate()?;
        let store = Store::new(journal, &document)?;
        let (mut schedule, desired) = store.load()?.unwrap_or((
            Schedule::new(document.lighting.schedule.clone())?,
            Desired {
                pc: true,
                door: true,
            },
        ));
        // The document config is authoritative. Same-config restore preserves manual
        // intent, rather than replaying an already-accepted event over it at startup.
        schedule.configure(document.lighting.schedule.clone(), &mut |ledger| {
            store.persist(desired, ledger)
        })?;
        let host = Host::new(
            document.lighting.setup.clone(),
            document.lighting.scene.clone(),
        )?;
        let point_targets = Self::point_targets(&document);
        let colors = vec![Rgb::default(); document.lighting.setup.canvas.points.len()];
        let mut value = Self {
            document,
            document_revision: 1,
            host,
            autonomous_rgb: false,
            store,
            schedule,
            desired,
            lcd: BTreeMap::new(),
            capture: None,
            sensors: BTreeMap::new(),
            sensor_specs: BTreeMap::new(),
            sensor_stamps: BTreeMap::new(),
            injected_inputs: vec![],
            registry_input: None,
            retired_registry: vec![],
            temperature_stream: None,
            temperature_due_ms: None,
            output_revision: 0,
            rgb_specs: BTreeMap::new(),
            lcd_specs: BTreeMap::new(),
            output_recovery: BTreeMap::new(),
            lcd_arrivals: BTreeSet::new(),
            capture_factory: None,
            samples: Arc::from([]),
            revision: 1,
            generation: 0,
            capture_active: None,
            last_us: None,
            last_wall: None,
            power: PowerCoordinator::default(),
            refresh: None,
            suspended: false,
            stopped: false,
            refresh_started: false,
            retry_action_required_on_refresh: false,
            error: None,
            pending: None,
            colors,
            point_targets,
            view_revision: 1,
            rgb_view: BTreeMap::new(),
            lcd_view: BTreeMap::new(),
            capture_view: None,
            view_error: None,
            sample_freshness: vec![],
            #[cfg(windows)]
            managed_backend: None,
            #[cfg(windows)]
            managed_startup_guard: None,
            #[cfg(windows)]
            openrgb_policies: managed_backend::OpenRgbPolicies::default(),
            #[cfg(windows)]
            managed_backend_ids: BTreeSet::new(),
            #[cfg(windows)]
            managed_backend_generation: 0,
            #[cfg(windows)]
            managed_backend_detail: None,
        };
        value.apply_mutes()?;
        Ok(value)
    }
    pub fn document(&self) -> &Document {
        &self.document
    }
    pub fn project(&self) -> &Project {
        &self.document.lighting
    }
    pub fn group_on(&self, target: Target) -> bool {
        self.desired.on(target)
    }
    /// Canvas-point order, masked for current PC/door intent, never physical telemetry.
    pub fn colors(&self) -> &[Rgb] {
        &self.colors
    }
    pub fn view_revision(&self) -> u64 {
        self.view_revision
    }
    pub fn temperature_samples(&self) -> &[lcd::Sample] {
        &self.samples
    }
    /// Observation ingress pauses during configuration, power refresh and shutdown.
    pub fn temperature_input_ready(&self) -> bool {
        self.usable().is_ok()
    }
    pub fn pending_configuration(&self) -> bool {
        self.pending.is_some()
    }
    pub fn configuration_error(&self) -> Option<&str> {
        self.pending.as_ref().and_then(|p| {
            if let ConfigurationPhase::Fault(error) = &p.phase {
                Some(error.as_str())
            } else {
                None
            }
        })
    }
    pub fn requested_document(&self) -> &Document {
        self.pending
            .as_ref()
            .map_or(&self.document, |p| &p.document)
    }
    pub fn host(&self) -> &Host {
        &self.host
    }
    pub fn desired(&self) -> Desired {
        self.desired
    }
    pub fn error(&self) -> Option<&str> {
        self.configuration_error()
            .or(self.error.as_deref())
            .or(self.power.error())
    }
    pub fn schedule_active(&self) -> bool {
        self.schedule.config().enabled && !self.schedule.config().paused
    }
    pub fn lcd_status(&self, id: &str) -> Option<lcd_worker::Snapshot> {
        self.lcd.get(id).map(lcd_worker::Worker::snapshot)
    }
    pub fn capture_status(&self) -> Option<capture_worker::Snapshot> {
        self.capture.as_ref().map(capture_worker::Worker::snapshot)
    }
    fn bump(&mut self) -> Result<(), String> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("Runtime intent revision exhausted")?;
        self.publish_capture_control()
    }
    fn bump_view(&mut self) {
        self.view_revision = self.view_revision.wrapping_add(1);
    }
    fn point_targets(document: &Document) -> Vec<Target> {
        document
            .lighting
            .setup
            .canvas
            .points
            .iter()
            .map(|p| {
                if document.lighting.groups.door.contains(&p.device) {
                    Target::Door
                } else {
                    Target::Pc
                }
            })
            .collect()
    }
    fn mask_view(&mut self) -> bool {
        let mut changed = false;
        for ((value, target), source) in self.colors.iter_mut().zip(&self.point_targets).zip(
            self.host
                .preview()
                .iter()
                .copied()
                .chain(std::iter::repeat(Rgb::default())),
        ) {
            let next = if self.desired.on(*target) {
                source
            } else {
                Rgb::default()
            };
            changed |= *value != next;
            *value = next;
        }
        changed
    }
    fn update_view(&mut self, now_us: u64) {
        let mut changed = self.mask_view();
        changed |= self.sample_freshness.len() != self.samples.len();
        self.sample_freshness.resize(self.samples.len(), false);
        for (sample, was_fresh) in self.samples.iter().zip(&mut self.sample_freshness) {
            let now = now_us / 1000;
            let fresh = sample.observed_ms > 0
                && sample.observed_ms <= now
                && now - sample.observed_ms <= 5000;
            changed |= *was_fresh != fresh;
            *was_fresh = fresh;
        }
        for id in self.rgb_specs.keys() {
            if let Some(s) = self.host.status(id) {
                let view = RgbView {
                    phase: s.phase,
                    generation: s.generation,
                    received: s.received,
                    replaced: s.replaced,
                    submitted: s.submitted,
                    deferred: s.deferred,
                    last: s.last_submitted,
                    error: s.error,
                };
                if self.rgb_view.get(id) != Some(&view) {
                    self.rgb_view.insert(id.clone(), view);
                    changed = true;
                }
            } else {
                changed |= self.rgb_view.remove(id).is_some();
            }
        }
        for (id, worker) in &self.lcd {
            let s = worker.snapshot();
            let view = LcdView {
                phase: s.phase,
                generation: s.generation,
                accepted: s.last_accepted_revision,
                processed: s.processed_revision,
                frames: s.frames_sent,
                replaced: s.replaced,
                state: s.lcd_state,
                error: s.error,
            };
            if self.lcd_view.get(id) != Some(&view) {
                self.lcd_view.insert(id.clone(), view);
                changed = true;
            }
        }
        let capture = self.capture.as_ref().map(|w| {
            let s = w.snapshot();
            CaptureView {
                phase: s.phase,
                generation: s.generation,
                accepted: s.last_accepted_revision,
                processed: s.processed_revision,
                frame: s.frame_revision,
                replaced: s.replaced,
                sources: s.source_status,
                error: s.error,
            }
        });
        if capture != self.capture_view {
            self.capture_view = capture;
            changed = true;
        }
        let error = self.error().map(str::to_owned);
        if error != self.view_error {
            self.view_error = error;
            changed = true;
        }
        if changed {
            self.bump_view();
        }
    }
    /// Normal interactive mode: RGB no longer needs UI ticks to advance its canvas.
    pub fn enable_autonomous_rgb(&mut self) -> Result<(), String> {
        self.usable()?;
        self.host.enable_autonomous()?;
        self.autonomous_rgb = true;
        Ok(())
    }
    pub fn autonomous_rgb(&self) -> bool {
        self.autonomous_rgb
    }
    /// Validate first, then stop only channels owned by this coordinator. No waiting here.
    pub fn request_document(&mut self, document: Document) -> Result<(), String> {
        self.request_document_to(document, None, None)
    }
    /// A candidate topology claims its own journal only after successful persistence.
    pub fn request_document_at(
        &mut self,
        document: Document,
        journal: PathBuf,
    ) -> Result<(), String> {
        Self::journal_path(&journal)?;
        self.request_document_to(document, Some(journal), None)
    }
    fn journal_path(path: &Path) -> Result<(), String> {
        if path.file_name().is_none()
            || path
                .parent()
                .is_none_or(|parent| parent.as_os_str().is_empty())
        {
            return Err("Journal requires an explicit parent directory and filename".into());
        }
        Ok(())
    }
    fn request_document_to(
        &mut self,
        document: Document,
        journal_override: Option<PathBuf>,
        outputs: Option<OutputPlan>,
    ) -> Result<(), String> {
        document.validate()?;
        if self.stopped
            || self.suspended
            || self.power.pending()
            || self.refresh.is_some()
            || self.pending.is_some()
        {
            return Err(
                "Runtime cannot queue configuration during stop, suspend or another transition"
                    .into(),
            );
        }

        if outputs.is_none()
            && document == self.document
            && self.power.error().is_none()
            && journal_override
                .as_ref()
                .is_none_or(|path| Some(path) == self.store.path.as_ref())
        {
            return Ok(());
        }
        let live_rgb = outputs
            .as_ref()
            .and_then(|plan| self.live_rgb_plan(&document, plan));
        if let Some(live) = &live_rgb {
            self.host.stop_rgb_outputs(&live.retiring);
        } else {
            self.stop_channels();
        }
        self.pending = Some(Configuration {
            outputs,
            live_rgb,
            #[cfg(windows)]
            startup_managed_ids: None,
            document,
            phase: ConfigurationPhase::Draining,
            journal_override,
        });
        self.error = None;
        self.bump_view();
        Ok(())
    }

    fn live_rgb_plan(&self, document: &Document, outputs: &OutputPlan) -> Option<LiveRgbPlan> {
        // RGB layout and route changes can leave the other owners running
        // when they require no rebuild.
        if self.power.error().is_some()
            || self.capture_factory.is_some()
            || self.capture.is_some()
            || outputs.capture.is_some()
            || !self.sensor_specs.is_empty()
            || !self.sensors.is_empty()
            || !self.document.ambient.routes.is_empty()
        {
            return None;
        }
        let mut layout = self.document.clone();
        layout.lighting.setup = document.lighting.setup.clone();
        layout.lighting.groups = document.lighting.groups.clone();
        // First-run probes can discover a new LCD after RGB already started.
        // Preserve the live layout only for appended LCD definitions.
        if document.lcd.starts_with(&layout.lcd) {
            layout.lcd.clone_from(&document.lcd);
        }
        if layout != *document {
            return None;
        }
        // An unchanged LCD with its own recovery may keep waiting while other routes change.
        if self.lcd.keys().any(|id| !self.lcd_specs.contains_key(id))
            || self.lcd_specs.iter().any(|(id, previous)| {
                !outputs.lcd.iter().any(|(next, keys, factory)| {
                    next == id && previous.keys == *keys && Arc::ptr_eq(&previous.factory, factory)
                }) || !self.device_unchanged(document, id)
                    || (!self.output_recovery.contains_key(id)
                        && self.lcd.get(id).is_none_or(|worker| {
                            !matches!(
                                worker.snapshot().phase,
                                lcd_worker::Phase::Connecting | lcd_worker::Phase::Ready
                            )
                        }))
            })
        {
            return None;
        }
        let added_lcd = outputs
            .lcd
            .iter()
            .filter(|(id, _, _)| !self.lcd_specs.contains_key(id))
            .map(|(id, _, _)| id.clone())
            .collect();
        let retained: BTreeSet<_> = outputs
            .rgb
            .iter()
            .filter_map(|(id, keys, factory)| {
                let previous = self.rgb_specs.get(id)?;
                let old = self
                    .document
                    .lighting
                    .setup
                    .devices
                    .iter()
                    .find(|device| device.id == *id)?;
                let new = document
                    .lighting
                    .setup
                    .devices
                    .iter()
                    .find(|device| device.id == *id)?;
                (old.driver == new.driver
                    && old.leds == new.leds
                    && old.role == Role::Canvas
                    && new.role == Role::Canvas
                    && previous.keys == *keys
                    && Arc::ptr_eq(&previous.factory, factory)
                    && self.host.status(id).is_some_and(|snapshot| {
                        use crate::runtime::output::Phase;
                        matches!(snapshot.phase, Phase::Connecting | Phase::Ready)
                    }))
                .then(|| id.clone())
            })
            .collect();
        if retained.is_empty() && self.lcd.is_empty() {
            return None;
        }
        let retiring = self
            .rgb_specs
            .keys()
            .filter(|id| !retained.contains(*id))
            .cloned()
            .collect();
        Some(LiveRgbPlan {
            retained,
            retiring,
            added_lcd,
        })
    }

    fn device_unchanged(&self, document: &Document, id: &str) -> bool {
        self.document
            .lighting
            .setup
            .devices
            .iter()
            .find(|device| device.id == id)
            == document
                .lighting
                .setup
                .devices
                .iter()
                .find(|device| device.id == id)
    }
    /// Save As storage barrier. Never changes a model or starts/restarts a factory.
    /// Pending saves retain the applied journal until that proposal actually commits.
    pub fn relocate_journal(&mut self, path: PathBuf) -> Result<(), String> {
        Self::journal_path(&path)?;
        if let Some(pending) = &self.pending {
            self.export_journal_for(&pending.document, path.clone())?;
            self.pending.as_mut().unwrap().journal_override = Some(path);
        } else {
            let store = Store::new(Some(path), &self.document)?;
            store.persist(self.desired, &self.schedule.encoded()?)?;
            self.store = store;
        }
        self.bump_view();
        Ok(())
    }
    /// Export intent for a saved editor draft without queuing/applying it or relocating this runtime.
    pub fn export_journal_for(&self, document: &Document, path: PathBuf) -> Result<(), String> {
        document.validate()?;
        Self::journal_path(&path)?;
        let store = Store::new(Some(path), document)?;
        let mut schedule = self.schedule.clone();
        schedule.configure(document.lighting.schedule.clone(), &mut |ledger| {
            store.persist(self.desired, ledger)
        })
    }
    pub fn retry_configuration(&mut self) -> Result<(), String> {
        if self.stopped || self.suspended || self.power.pending() {
            return Err("Runtime cannot retry configuration during stop or power refresh".into());
        }
        let pending = self.pending.as_mut().ok_or("No pending configuration")?;
        if !matches!(pending.phase, ConfigurationPhase::Fault(_)) {
            return Err("Pending configuration has not failed".into());
        }
        pending.phase = ConfigurationPhase::Draining;
        let live = pending.live_rgb.is_some();
        self.error = None;
        if !live {
            self.stop_channels();
        }
        self.bump_view();
        Ok(())
    }
    pub fn cancel_configuration(&mut self) -> Result<(), String> {
        if self.stopped || self.suspended || self.power.pending() {
            return Err("Runtime cannot cancel configuration during stop or power refresh".into());
        }
        let pending = self.pending.as_mut().ok_or("No pending configuration")?;
        pending.phase = ConfigurationPhase::Cancelling;
        let live = pending.live_rgb.is_some();
        self.error = None;
        if !live {
            self.stop_channels();
        }
        self.bump_view();
        Ok(())
    }
    /// Called only on timer/refresh boundaries; persistence failure retains the entire old model.
    fn configuration_tick(&mut self, restart: bool) -> Result<bool, String> {
        let Some(pending) = &self.pending else {
            return Ok(false);
        };
        let live = pending.live_rgb.clone();
        if matches!(pending.phase, ConfigurationPhase::Fault(_)) {
            return Ok(false);
        }
        let drained = if let Some(live) = &live {
            self.host.rgb_outputs_drained(&live.retiring)
        } else {
            self.lighting_drained()
        };
        if !drained {
            return Ok(false);
        }
        if matches!(
            self.pending.as_ref().unwrap().phase,
            ConfigurationPhase::Cancelling
        ) {
            // The old model/intent were already committed; cancellation discards only the proposal.
            self.pending = None;
            self.error = None;
            self.capture = None;
            if live.is_none() {
                self.lcd.clear();
            }

            self.sensors.clear();

            self.capture_active = None;
            self.clear_capture_intent()?;
            self.bump()?;
            self.bump_view();
        } else {
            let prepared = (|| {
                let document = &self.pending.as_ref().unwrap().document;
                let host = if live.is_some() {
                    None
                } else {
                    let mut host = Host::new(
                        document.lighting.setup.clone(),
                        document.lighting.scene.clone(),
                    )?;
                    for d in &document.lighting.setup.devices {
                        if d.role != Role::Lcd {
                            let target = if document.lighting.groups.door.contains(&d.id) {
                                Target::Door
                            } else {
                                Target::Pc
                            };
                            host.mute(&d.id, !self.desired.on(target))?;
                        }
                    }
                    if self.autonomous_rgb {
                        host.enable_autonomous()?;
                    }
                    Some(host)
                };
                let path = self
                    .pending
                    .as_ref()
                    .unwrap()
                    .journal_override
                    .clone()
                    .or_else(|| self.store.path.clone());
                let store = Store::new(path, document)?;
                let mut schedule = self.schedule.clone();
                let revision = self
                    .revision
                    .checked_add(1)
                    .ok_or("Runtime intent revision exhausted")?;
                let targets = Self::point_targets(document);
                let colors = vec![Rgb::default(); targets.len()];
                schedule.configure(document.lighting.schedule.clone(), &mut |ledger| {
                    store.persist(self.desired, ledger)
                })?;
                Ok::<_, String>((host, store, schedule, revision, targets, colors))
            })();
            let (host, store, schedule, revision, targets, colors) = match prepared {
                Ok(prepared) => prepared,
                Err(error) => {
                    self.pending.as_mut().unwrap().phase = ConfigurationPhase::Fault(error.clone());
                    self.bump_view();
                    return Err(error);
                }
            };
            let pending = self.pending.take().unwrap();
            let outputs = pending.outputs;
            let preserve_action_failures = outputs.is_none();
            #[cfg(windows)]
            let preserve_action_failures =
                preserve_action_failures || pending.startup_managed_ids.is_some();
            let proposal = pending.document;
            let old = &self.document.lighting.setup.devices;
            let new = &proposal.lighting.setup.devices;
            self.rgb_specs.retain(|id,_| {
                let a=old.iter().find(|d|&d.id==id);let b=new.iter().find(|d|&d.id==id);
                matches!((a,b),(Some(a),Some(b)) if a.enabled&&b.enabled&&a.driver==b.driver&&a.leds==b.leds&&b.role!=Role::Lcd)
            });
            self.lcd_specs.retain(|id,_| {
                let a=old.iter().find(|d|&d.id==id);let b=new.iter().find(|d|&d.id==id);
                matches!((a,b),(Some(a),Some(b)) if a.enabled&&b.enabled&&a.role==Role::Lcd&&b.role==Role::Lcd&&a.driver==b.driver)
            });
            let old_keys = self
                .document
                .ambient
                .sources
                .iter()
                .map(|s| s.key.clone())
                .collect::<BTreeSet<_>>();
            let new_keys = proposal
                .ambient
                .sources
                .iter()
                .map(|s| s.key.clone())
                .collect::<BTreeSet<_>>();
            if old_keys != new_keys {
                self.capture_factory = None;
            }
            if self.document.temperatures.sources != proposal.temperatures.sources {
                self.samples = Arc::from([]);
                self.sample_freshness.clear();
            }
            let inputs = Self::temperature_inputs(&proposal);
            self.sensor_specs.retain(|input, _| inputs.contains(input));
            if preserve_action_failures {
                for (id, spec) in &mut self.rgb_specs {
                    if let Some(error) = self
                        .host
                        .status(id)
                        .filter(|snapshot| snapshot.requires_action)
                        .and_then(|snapshot| snapshot.error)
                    {
                        spec.action_required_error = Some(error);
                    }
                }
            }
            if let Some(outputs) = outputs {
                self.rgb_specs = outputs
                    .rgb
                    .into_iter()
                    .map(|(id, keys, factory)| {
                        // Startup discovery can replace the factory of an unchanged binding.
                        let action_required_error = if preserve_action_failures && !keys.is_empty()
                        {
                            self.rgb_specs
                                .get(&id)
                                .filter(|previous| previous.keys == keys)
                                .and_then(|previous| previous.action_required_error.clone())
                        } else {
                            None
                        };
                        (
                            id,
                            RgbSpec {
                                keys,
                                factory,
                                action_required_error,
                            },
                        )
                    })
                    .collect();
                self.lcd_specs = outputs
                    .lcd
                    .into_iter()
                    .map(|(id, keys, factory)| (id, LcdSpec { keys, factory }))
                    .collect();
                self.capture_factory = outputs.capture;
                self.output_revision += 1;
            }
            self.output_recovery
                .retain(|id, _| self.rgb_specs.contains_key(id) || self.lcd_specs.contains_key(id));
            if let Some(live) = &live {
                for id in &live.retiring {
                    self.reset_output_recovery(id);
                    if let Some(recovery) = self.output_recovery.get_mut(id) {
                        recovery.start_error = None;
                    }
                }
            }
            #[cfg(windows)]
            if let Some(ids) = pending.startup_managed_ids {
                self.managed_backend_ids = ids;
                self.enable_registered_output_recovery();
            }
            self.document = proposal;
            self.document_revision = self.document_revision.wrapping_add(1);
            if let Some(host) = host {
                self.host = host;
            } else if let Some(live) = &live {
                let setup = self.document.lighting.setup.clone();
                let muted = setup
                    .devices
                    .iter()
                    .filter(|device| device.role != Role::Lcd)
                    .map(|device| (device.id.clone(), !self.desired.on(self.target(&device.id))))
                    .collect();
                self.host.install_rgb_setup(setup, &live.retiring, muted);
            }
            self.store = store;
            self.schedule = schedule;
            self.revision = revision;
            self.point_targets = targets;
            self.colors = colors;
            if live.is_none() {
                self.lcd.clear();
                self.lcd_view.clear();
            }

            self.sensors.clear();

            self.capture = None;
            self.capture_active = None;
            self.last_wall = None;
            self.error = None;
            self.rgb_view.clear();
            self.capture_view = None;
            self.bump_view();
        }
        if self.power.error().is_some() && !self.power.pending() {
            // Failed refresh has no completion outstanding after drain. Preserve diagnostics,
            // but let an explicit committed edit/cancel recover instead of remaining gated forever.
            let mut power = PowerCoordinator::default();
            power.suspends = self.power.suspends;
            power.resumes = self.power.resumes;
            power.coalesced = self.power.coalesced;
            self.power = power;
            self.refresh = None;
            self.refresh_started = false;
        }
        if restart {
            if let Some(live) = &live {
                let rgb = self.restart_registered_rgb(Some(live), false);
                let lcd = self.restart_registered_lcd(Some(&live.added_lcd));
                rgb.and(lcd)?;
                self.bump()?;
            } else if let Err(error) = self.restart_registered(false) {
                self.stop_channels();
                return Err(error);
            }
        }
        Ok(true)
    }
    fn next_generation(&mut self) -> Result<u64, String> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("Runtime generation exhausted")?;
        Ok(self.generation)
    }
    fn target(&self, id: &str) -> Target {
        if self.document.lighting.groups.door.iter().any(|d| d == id) {
            Target::Door
        } else {
            Target::Pc
        }
    }
    fn apply_mutes(&mut self) -> Result<(), String> {
        for d in &self.document.lighting.setup.devices {
            if d.role != Role::Lcd {
                let off = !self.desired.on(self.target(&d.id));
                self.host.mute(&d.id, off)?;
            }
        }
        Ok(())
    }
    fn observe(&mut self, now: u64) -> Result<(), String> {
        if self.last_us.is_some_and(|at| now < at) {
            return Err("Runtime monotonic clock moved backwards".into());
        }
        self.last_us = Some(now);
        Ok(())
    }
    fn usable(&self) -> Result<(), String> {
        if self.stopped {
            return Err("Runtime has stopped".into());
        }
        if self.suspended || self.power.pending() || self.power.error().is_some() {
            return Err("Runtime is waiting for device refresh".into());
        }
        if self.pending.is_some() {
            return Err("A configuration is already pending; finish or cancel it first".into());
        }
        Ok(())
    }
    pub fn set_group(&mut self, target: Target, on: bool) -> Result<(), String> {
        if self.stopped {
            return Err("Runtime has stopped".into());
        }
        if self.desired.on(target) == on {
            return Ok(());
        }
        let mut desired = self.desired;
        desired.set(target, on);
        self.store.persist(desired, &self.schedule.encoded()?)?;
        self.desired = desired;
        self.apply_mutes()?;
        self.mask_view();
        self.bump_view();
        self.bump()
    }
    pub fn set_scene(&mut self, scene: crate::runtime::scene::Scene) -> Result<(), String> {
        scene.validate()?;
        if self.stopped {
            return Err("Runtime has stopped".into());
        }
        if self.pending.is_some() {
            return Err(
                "Finish or cancel pending configuration before editing the applied scene".into(),
            );
        }
        self.host.configure_scene(scene.clone())?;
        self.document.lighting.scene = scene;
        self.document_revision = self.document_revision.wrapping_add(1);
        self.bump_view();
        Ok(())
    }
    pub fn configure_schedule(&mut self, config: schedule::Config) -> Result<(), String> {
        if self.stopped {
            return Err("Runtime has stopped".into());
        }
        if self.pending.is_some() {
            return Err(
                "Finish or cancel pending configuration before editing the applied schedule".into(),
            );
        }
        Schedule::new(config.clone())?;
        self.schedule.configure(config.clone(), &mut |ledger| {
            self.store.persist(self.desired, ledger)
        })?;
        self.document.lighting.schedule = config;
        self.document_revision = self.document_revision.wrapping_add(1);
        self.last_wall = None;
        self.bump_view();
        Ok(())
    }
    pub fn accept_temperatures(
        &mut self,
        inputs: &[lcd::Sample],
        now_us: u64,
    ) -> Result<(), String> {
        self.usable()?;
        if self.registry_input.is_some() {
            return Err("Temperature observations belong to the attached registry".into());
        }
        self.apply_temperature_inputs(inputs, now_us)
    }
    fn apply_temperature_inputs(
        &mut self,
        inputs: &[lcd::Sample],
        now_us: u64,
    ) -> Result<(), String> {
        self.observe(now_us)?;
        self.document.temperatures.select(now_us / 1000, inputs)?;
        if inputs.iter().any(|s| {
            self.sensor_specs.contains_key(&s.key)
                || self.sensors.contains_key(&s.key)
                || self
                    .temperature_stream
                    .as_ref()
                    .is_some_and(|stream| stream.contains(&s.key))
        }) {
            return Err("Temperature input belongs to an attached sensor".into());
        }
        let configured = Self::temperature_inputs(&self.document);
        self.injected_inputs = inputs
            .iter()
            .filter(|s| configured.contains(&s.key))
            .cloned()
            .collect();
        self.rebuild_temperatures(now_us / 1000)
    }
    pub fn attach_sensor(
        &mut self,
        input: &str,
        keys: Vec<String>,
        factory: sensor_worker::Factory,
    ) -> Result<(), String> {
        self.usable()?;
        if self.registry_owns(input) {
            return Err("Temperature input belongs to the attached registry".into());
        }
        if !Self::temperature_inputs(&self.document).contains(input) {
            return Err("Sensor must bind an existing logical temperature input".into());
        }
        if self.sensors.contains_key(input) || self.sensor_specs.contains_key(input) {
            return Err("Temperature input already has an owner; use explicit refresh".into());
        }
        if self
            .temperature_stream
            .as_ref()
            .is_some_and(|stream| stream.contains(input))
        {
            return Err("Temperature input belongs to a configured observation stream".into());
        }
        let generation = self.next_generation()?;
        let worker =
            sensor_worker::Worker::start_claimed(generation, keys.clone(), factory.clone())?;
        self.injected_inputs.retain(|s| s.key != input);
        self.sensors.insert(input.into(), worker);
        self.sensor_specs
            .insert(input.into(), SensorSpec { keys, factory });
        self.sensor_stamps.clear();
        self.rebuild_temperatures(self.last_us.unwrap_or(1) / 1000)?;
        self.bump_view();
        Ok(())
    }
    pub fn sensor_status(&self, input: &str) -> Option<sensor_worker::Snapshot> {
        self.sensors.get(input).map(|w| w.snapshot())
    }
    /// Retire the owned reader without blocking on its current platform call.
    /// Rebinding remains blocked until tick/drained reaps the actual old thread.
    pub fn detach_sensor(&mut self, input: &str) -> Result<(), String> {
        self.usable()?;
        self.sensors
            .get(input)
            .ok_or("Temperature input has no sensor owner")?
            .stop();
        self.sensor_specs.remove(input);
        self.sensor_stamps.remove(input);
        self.rebuild_temperatures(self.last_us.unwrap_or(1) / 1000)?;
        self.bump_view();
        Ok(())
    }
    fn temperature_inputs(document: &Document) -> BTreeSet<String> {
        document
            .temperatures
            .sources
            .iter()
            .filter_map(|s| match &s.value {
                super::sensors::Value::Sensor { input } => Some(input.clone()),
                _ => None,
            })
            .collect()
    }
    fn rebuild_temperatures(&mut self, now_ms: u64) -> Result<(), String> {
        self.injected_inputs.retain(|s| {
            now_ms
                .checked_sub(s.observed_ms)
                .is_some_and(|age| age <= 5000)
        });
        let inputs = self.temperature_observations();
        let selected = self.document.temperatures.select(now_ms, &inputs)?;
        self.temperature_due_ms = selected
            .iter()
            .filter_map(|s| s.observed_ms.checked_add(5001))
            .min();
        if self.samples.as_ref() != selected.as_slice() {
            self.samples = Arc::from(selected);
            self.bump_view();
            self.bump()?;
        }
        Ok(())
    }
    fn temperature_observations(&self) -> Vec<lcd::Sample> {
        self.temperature_observation_batch().0
    }
    fn temperature_observation_batch(&self) -> (Vec<lcd::Sample>, BTreeMap<String, (u64, u64)>) {
        let mut inputs = self.injected_inputs.clone();
        let mut tokens = BTreeMap::new();
        if let Some(registry) = &self.registry_input {
            if registry.subscription.active() && registry.config == self.document.temperatures {
                tokens.extend(registry.tokens.clone());
                inputs.extend(registry.readings.iter().cloned());
            } else {
                inputs.clear();
            }
        }
        if let Some(stream) = &self.temperature_stream {
            inputs.extend(stream.samples.iter().cloned());
            tokens.extend(stream.tokens.clone());
        }
        for (key, worker) in &self.sensors {
            let snapshot = worker.snapshot();
            tokens.insert(
                key.clone(),
                (snapshot.generation, snapshot.availability_epoch),
            );
            if snapshot.phase == crate::runtime::output::Phase::Ready
                && let Some(reading) = snapshot.reading
            {
                inputs.push(lcd::Sample {
                    key: key.clone(),
                    celsius: reading.celsius,
                    observed_ms: reading.observed_ms,
                });
            }
        }
        (inputs, tokens)
    }
    fn temperatures_tick(&mut self, now_ms: u64) -> Result<(), String> {
        let mut changed = self.temperature_due_ms.is_some_and(|due| now_ms >= due);
        for (input, worker) in &self.sensors {
            let s = worker.snapshot();
            let stamp = (s.generation, s.revision);
            if self.sensor_stamps.get(input) != Some(&stamp) {
                self.sensor_stamps.insert(input.clone(), stamp);
                changed = true;
            }
        }
        if changed {
            self.bump_view();
            self.rebuild_temperatures(now_ms)?;
        }
        Ok(())
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.requested_document().save(path)
    }

    /// Injected factories are capabilities, not discovery. Pass only stand factories
    /// until physical activation is explicitly authorized. Canonical native keys must
    /// come from validated local bindings; leases are held by actual I/O threads.
    pub fn attach_rgb(
        &mut self,
        id: &str,
        keys: Vec<String>,
        factory: RgbFactory,
    ) -> Result<(), String> {
        self.usable()?;
        #[cfg(windows)]
        let factory = self.guard_managed_rgb(id, factory);
        let f = factory.clone();
        self.host.attach_claimed(id, keys.clone(), move |c| f(c))?;
        self.rgb_specs.insert(
            id.into(),
            RgbSpec {
                keys,
                factory,
                action_required_error: None,
            },
        );
        self.reset_output_recovery(id);
        self.bump_view();
        Ok(())
    }
    pub fn attach_lcd(
        &mut self,
        id: &str,
        keys: Vec<String>,
        factory: LcdFactory,
    ) -> Result<(), String> {
        self.usable()?;
        if !self
            .document
            .lighting
            .setup
            .devices
            .iter()
            .any(|d| d.id == id && d.role == Role::Lcd && d.enabled)
        {
            return Err("LCD device is missing or disabled".into());
        }
        if let Some(worker) = self.lcd.get_mut(id)
            && !worker.reap()
        {
            return Err("Previous LCD owner is still draining".into());
        }
        let config = self
            .document
            .lcd
            .iter()
            .find(|l| l.device == id)
            .ok_or("Missing LCD configuration")?
            .config
            .clone();
        let generation = self.next_generation()?;
        let f = factory.clone();
        let worker = lcd_worker::Worker::start_claimed(generation, keys.clone(), move |cancel| {
            let (controller, transport) = f(config.clone(), cancel)?;
            if controller.config() != &config {
                return Err("LCD factory configuration changed".into());
            }
            Ok((controller, transport))
        })?;
        self.lcd.insert(id.into(), worker);
        self.lcd_specs.insert(id.into(), LcdSpec { keys, factory });
        self.reset_output_recovery(id);
        self.bump_view();
        self.bump()
    }
    pub fn attach_capture(&mut self, factory: CaptureFactory) -> Result<(), String> {
        self.usable()?;
        if let Some(worker) = &mut self.capture
            && !worker.reap()
        {
            return Err("Previous capture owner is still draining".into());
        }
        let generation = self.next_generation()?;
        let f = factory.clone();
        self.capture = Some(capture_worker::Worker::start(
            generation,
            self.document.ambient.clone(),
            self.document.lighting.setup.clone(),
            move |c| f(c),
        )?);
        self.capture_factory = Some(factory);
        self.capture_active = None;
        self.clear_capture_intent()?;
        self.host
            .bind_external_feed(self.capture.as_ref().unwrap().frame_reader())?;
        self.bump_view();
        self.bump()
    }
    fn clear_capture_intent(&mut self) -> Result<(), String> {
        self.host.clear_external_feed();
        for route in &self.document.ambient.routes {
            self.host.set_external(
                &route.device,
                vec![crate::lighting::color::Rgb::default(); route.zone.leds],
            )?;
        }
        Ok(())
    }
    fn stop_channels(&mut self) {
        self.lcd_arrivals.clear();
        for recovery in self.output_recovery.values_mut() {
            *recovery = recovery::Recovery::default();
        }
        self.injected_inputs.clear();
        self.samples = Arc::from([]);
        self.sensor_stamps.clear();
        self.temperature_due_ms = None;
        self.host.stop();
        for w in self.lcd.values() {
            w.stop();
        }
        if let Some(w) = &self.capture {
            w.stop();
        }

        for worker in self.sensors.values() {
            worker.stop();
        }
    }
    pub fn stop(&mut self) {
        self.stopped = true;
        #[cfg(windows)]
        if let Some(backend) = &self.managed_backend {
            backend.stop_restarting();
        }
        if let Err(error) = self.release_registry_input(crate::clock::monotonic_us()) {
            self.error = Some(error);
        }
        self.stop_channels();
        self.bump_view();
    }
    pub fn drained(&mut self) -> bool {
        let lighting = self.lighting_drained();
        let sensors = self.sensors.values_mut().all(|w| w.reap());
        let registry = self.registry_drained();
        lighting && sensors && registry
    }
    fn lighting_drained(&mut self) -> bool {
        let rgb_drained = self.host.drained();
        self.other_channels_drained() && rgb_drained
    }
    fn other_channels_drained(&mut self) -> bool {
        let mut drained = true;
        for w in self.lcd.values_mut() {
            drained = w.reap() && drained;
        }
        if let Some(w) = &mut self.capture {
            drained = w.reap() && drained;
        }
        for worker in self.sensors.values_mut() {
            let reaped = worker.reap();
            drained = reaped && drained;
        }
        self.sensors
            .retain(|input, worker| self.sensor_specs.contains_key(input) || !worker.is_reaped());
        drained
    }
    fn resume_owners_drained(&mut self) -> bool {
        if self.pending.is_some() || self.retry_action_required_on_refresh {
            return self.lighting_drained();
        }
        self.host.clock_drained()
            && self.other_channels_drained()
            && self.host.rgb_outputs_drained(
                self.rgb_specs
                    .keys()
                    .filter(|id| !self.output_recovery.contains_key(*id)),
            )
    }
    /// Notifications only; this never calls Windows suspend/reboot/shutdown APIs.
    pub fn notify_power(&mut self, suspended: bool, now_us: u64) -> Result<(), String> {
        self.lighting_power(suspended, now_us)
    }
    fn lighting_power(&mut self, suspended: bool, now_us: u64) -> Result<(), String> {
        if self.stopped {
            return Err("Runtime has stopped".into());
        }
        self.observe(now_us)?;
        let resumes = self.power.resumes;
        self.power
            .notify(suspended, now_us / 1000)
            .map_err(str::to_owned)?;
        if !suspended && self.power.resumes == resumes {
            return Ok(());
        }
        self.suspended = suspended;
        self.bump_view();
        self.refresh = None;
        self.refresh_started = false;
        self.retry_action_required_on_refresh = false;
        // A resume can be delivered without a preceding suspend notification.
        // Its old owners must still drain before new factories are invoked.
        if suspended || self.power.resumes != resumes {
            if let Some(pending) = &mut self.pending {
                pending.live_rgb = None;
            }
            self.stop_channels();
            for d in &self.document.lighting.setup.devices {
                if d.role == Role::Ambient {
                    self.host.set_external(
                        &d.id,
                        vec![crate::lighting::color::Rgb::default(); d.leds],
                    )?;
                }
            }
        }
        self.bump()
    }
    /// Explicit retry/refresh of owned lighting channels, not an OS power operation.
    pub fn refresh_devices(&mut self, now_us: u64) -> Result<(), String> {
        self.lighting_power(true, now_us)?;
        self.lighting_power(false, now_us)?;
        self.retry_action_required_on_refresh = true;
        self.error = None;
        Ok(())
    }
    fn rgb_requires_action(&self, id: &str) -> bool {
        self.rgb_specs
            .get(id)
            .is_some_and(|spec| spec.action_required_error.is_some())
            || self
                .host
                .status(id)
                .is_some_and(|status| status.requires_action)
    }

    fn restart_registered_rgb(
        &mut self,
        live: Option<&LiveRgbPlan>,
        retry_action_required: bool,
    ) -> Result<(), String> {
        if retry_action_required {
            for spec in self.rgb_specs.values_mut() {
                spec.action_required_error = None;
            }
        }
        let rgb = self
            .rgb_specs
            .iter()
            .filter(|(id, _)| {
                live.is_none_or(|plan| !plan.retained.contains(*id))
                    && (retry_action_required || !self.rgb_requires_action(id))
            })
            .map(|(id, s)| (id.clone(), s.keys.clone(), s.factory.clone()))
            .collect::<Vec<_>>();
        let mut first_error = None;
        for (id, keys, f) in rgb {
            let result = if self.refresh_started
                && self.output_recovery.contains_key(&id)
                && !self.host.rgb_outputs_drained(std::iter::once(&id))
            {
                // Recovery waits for this worker to reap before it retries the same binding.
                Err("Previous RGB owner is still stopping".into())
            } else {
                self.host.attach_claimed(&id, keys, move |c| f(c))
            };
            if let Err(error) = self.handle_output_start(&id, result) {
                if live.is_none() {
                    return Err(error);
                }
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn restart_registered(&mut self, retry_action_required: bool) -> Result<(), String> {
        // Every reopened owner revalidates its binding. A recoverable RGB
        // owner that is still stopping is deferred to its own recovery.
        self.restart_registered_rgb(None, retry_action_required)?;
        self.restart_registered_lcd(None)?;
        if let Some(f) = self.capture_factory.clone() {
            let generation = self.next_generation()?;
            self.capture = Some(capture_worker::Worker::start(
                generation,
                self.document.ambient.clone(),
                self.document.lighting.setup.clone(),
                move |c| f(c),
            )?);
            self.capture_active = None;
            self.clear_capture_intent()?;
            self.host
                .bind_external_feed(self.capture.as_ref().unwrap().frame_reader())?;
        }
        let sensors = self
            .sensor_specs
            .iter()
            .map(|(input, s)| (input.clone(), s.keys.clone(), s.factory.clone()))
            .collect::<Vec<_>>();
        for (input, keys, factory) in sensors {
            if self.sensors.get(&input).is_some_and(|w| !w.is_reaped()) {
                continue;
            }
            let generation = self.next_generation()?;
            self.sensors.insert(
                input,
                sensor_worker::Worker::start_claimed(generation, keys, factory)?,
            );
        }
        self.sensor_stamps.clear();
        self.bump()
    }

    fn restart_registered_lcd(&mut self, added: Option<&BTreeSet<String>>) -> Result<(), String> {
        let lcd = self
            .lcd_specs
            .iter()
            .filter(|(id, _)| added.is_none_or(|ids| ids.contains(*id)))
            .map(|(id, s)| (id.clone(), s.keys.clone(), s.factory.clone()))
            .collect::<Vec<_>>();
        for (id, keys, f) in lcd {
            let result = self.start_registered_lcd(&id, keys, f);
            self.handle_output_start(&id, result)?;
        }
        Ok(())
    }
    fn ready(&self, recovers_independently: impl Fn(&str) -> bool) -> Result<bool, String> {
        let mut ready = true;
        if let Some(error) = self
            .output_recovery
            .iter()
            .filter(|(id, _)| !recovers_independently(id))
            .find_map(|(_, state)| state.start_error.as_ref())
        {
            return Err(error.clone());
        }
        // A failed synchronous start may leave the old stopped worker as the
        // only snapshot. Its committed route still has a retry budget.
        let retry_start = |id: &str| {
            recovers_independently(id)
                && self
                    .output_recovery
                    .get(id)
                    .is_some_and(|state| state.start_error.is_some())
        };
        // This is lighting-output readiness. Optional temperature readers may
        // connect/fail independently; absence is not a temperature reading.
        for (id, spec) in &self.rgb_specs {
            if let Some(error) = &spec.action_required_error {
                if recovers_independently(id) {
                    continue;
                }
                return Err(error.clone());
            }
            if retry_start(id) {
                continue;
            }
            let s = self
                .host
                .status(id)
                .ok_or("RGB owner missing after refresh")?;
            match s.phase {
                crate::runtime::output::Phase::Ready => {}
                crate::runtime::output::Phase::Connecting
                | crate::runtime::output::Phase::Failed
                    if recovers_independently(id) => {}
                crate::runtime::output::Phase::Connecting => ready = false,
                _ => return Err(s.error.unwrap_or_else(|| "RGB refresh stopped".into())),
            }
        }
        for (id, worker) in &self.lcd {
            if retry_start(id) {
                continue;
            }
            let s = worker.snapshot();
            match s.phase {
                lcd_worker::Phase::Ready => {}
                lcd_worker::Phase::Connecting | lcd_worker::Phase::Failed
                    if recovers_independently(id) => {}
                lcd_worker::Phase::Connecting => ready = false,
                _ => return Err(s.error.unwrap_or_else(|| "LCD refresh stopped".into())),
            }
        }
        if let Some(w) = &self.capture {
            let s = w.snapshot();
            match s.phase {
                capture_worker::Phase::Ready => {}
                capture_worker::Phase::Connecting => ready = false,
                _ => return Err(s.error.unwrap_or_else(|| "Capture refresh stopped".into())),
            }
        }
        Ok(ready)
    }
    fn power_tick(&mut self, now: u64) -> Result<bool, String> {
        if self.suspended {
            return Ok(false);
        }
        if let Some(generation) = self.refresh {
            self.power
                .poll(now / 1000, false, self.schedule_active())
                .map_err(str::to_owned)?;
            if self.power.error().is_some() {
                self.refresh = None;
                self.stop_channels();
                return Ok(false);
            }
            // Healthy outputs can submit while others connect, retry, or wait
            // for the user to resolve an authorization failure.
            match self
                .ready(|id| self.output_recovery.contains_key(id) || self.rgb_requires_action(id))
            {
                Ok(true) => {
                    self.power
                        .refreshed(generation, now / 1000, Ok(()))
                        .map_err(str::to_owned)?;
                    self.refresh = None;
                }
                Ok(false) => {}
                Err(e) => {
                    self.power
                        .refreshed(generation, now / 1000, Err(&e))
                        .map_err(str::to_owned)?;
                    self.refresh = None;
                    self.stop_channels();
                }
            }
        }
        let busy = !self.refresh_started && self.power.pending() && !self.resume_owners_drained();
        for _ in 0..2 {
            match self
                .power
                .poll(now / 1000, busy, self.schedule_active())
                .map_err(str::to_owned)?
            {
                Some(Action::Refresh { generation }) => {
                    self.refresh_started = true;
                    let retry_action_required =
                        std::mem::take(&mut self.retry_action_required_on_refresh);
                    let configuring = self.pending.is_some();
                    let restart = if configuring {
                        match self.configuration_tick(false) {
                            Ok(true) => self.restart_registered(retry_action_required),
                            Ok(false) => Err(self
                                .configuration_error()
                                .unwrap_or("Configuration is still draining")
                                .to_owned()),
                            Err(error) => Err(error),
                        }
                    } else {
                        self.restart_registered(retry_action_required)
                    };
                    if let Err(e) = restart {
                        self.stop_channels();
                        self.power
                            .refreshed(generation, now / 1000, Err(&e))
                            .map_err(str::to_owned)?;
                        if configuring {
                            return Err(e);
                        }
                    } else {
                        self.refresh = Some(generation);
                    }
                    break;
                }
                Some(Action::ReconcileSchedule { .. }) | Some(Action::RestoreUserIntent { .. }) => {
                    self.last_wall = None;
                    self.refresh_started = false;
                    self.bump()?;
                }
                None => break,
            }
        }
        Ok(!self.power.pending() && self.power.error().is_none())
    }
    fn schedule_tick(&mut self, now: i64) -> Result<(), String> {
        let dispatch = self
            .schedule
            .prepare(now, &mut |ledger| self.store.persist(self.desired, ledger))?;
        for d in dispatch {
            let mut desired = self.desired;
            desired.set(d.target, d.event.on);
            self.schedule.finish(
                d,
                now,
                schedule::Outcome::Submitted,
                ACCEPTED,
                &mut |ledger| self.store.persist(desired, ledger),
            )?;
            self.desired = desired;
            self.apply_mutes()?;
            self.mask_view();
            self.bump_view();
            self.bump()?;
        }
        Ok(())
    }
    fn publish(&mut self) -> Result<(), String> {
        for (id, w) in &self.lcd {
            let s = w.snapshot();
            if s.last_accepted_revision < self.revision
                && matches!(
                    s.phase,
                    lcd_worker::Phase::Connecting | lcd_worker::Phase::Ready
                )
            {
                w.submit(lcd_worker::Intent {
                    revision: self.revision,
                    active: self.desired.on(self.target(id)),
                    suspended: false,
                    samples: self.samples.clone(),
                })?;
            }
        }
        self.publish_capture_control()
    }
    fn publish_capture_control(&mut self) -> Result<(), String> {
        if let Some(w) = &self.capture {
            let active = self.document.lighting.setup.devices.iter().any(|d| {
                d.enabled && d.role == Role::Ambient && self.desired.on(self.target(&d.id))
            });
            let s = w.snapshot();
            if self.capture_active != Some(active)
                && matches!(
                    s.phase,
                    capture_worker::Phase::Connecting | capture_worker::Phase::Ready
                )
            {
                let accepted = w.submit(capture_worker::Intent {
                    revision: self.revision,
                    active,
                    suspended: false,
                })?;
                if accepted {
                    self.capture_active = Some(active);
                }
            }
        }
        Ok(())
    }
    pub fn tick(&mut self, now_us: u64, unix_seconds: i64) -> Result<(), String> {
        if self.stopped {
            return Err("Runtime has stopped".into());
        }
        if unix_seconds <= 0
            || chrono::DateTime::<chrono::Utc>::from_timestamp(unix_seconds, 0).is_none()
        {
            return Err("Invalid runtime wall clock".into());
        }
        self.observe(now_us)?;
        #[cfg(windows)]
        self.poll_managed_backend();
        let registry_result = self.registry_tick(now_us);
        let result: Result<(), String> = (|| {
            registry_result?;
            self.host.reap();
            for w in self.lcd.values_mut() {
                w.reap();
            }
            if let Some(w) = &mut self.capture {
                w.reap();
            }
            for worker in self.sensors.values_mut() {
                worker.reap();
            }
            self.sensors.retain(|input, worker| {
                self.sensor_specs.contains_key(input) || !worker.is_reaped()
            });
            if self.pending.is_some() && !self.suspended && !self.power.pending() {
                self.configuration_tick(true)?;
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.live_rgb.is_none())
                {
                    return Ok(());
                }
            }
            if !self.power_tick(now_us)? {
                return Ok(());
            }
            self.reconcile_registry(now_us)?;
            self.temperatures_tick(now_us / 1000)?;
            if self.last_wall != Some(unix_seconds) {
                self.schedule_tick(unix_seconds)?;
                self.last_wall = Some(unix_seconds);
            }
            self.recover_failed_outputs(now_us)?;
            self.publish()?;
            self.host.tick(now_us)?;
            Ok(())
        })();
        if let Err(e) = &result {
            self.error = Some(e.clone());
        }
        self.update_view(now_us);
        result
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stopped = true;
        let _ = self.release_registry_input(crate::clock::monotonic_us());
        self.stop_channels();
    }
}

#[cfg(test)]
#[path = "runtime/recovery_tests.rs"]
mod recovery_tests;
#[cfg(test)]
mod tests;
