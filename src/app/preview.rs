//! Map/scene/schedule preview only. No output factories, native bindings, capture or sensors.
//! Group states and journal outcomes are preview intent, never device connection/write ACKs.
use super::config::{Groups, Project};
use crate::{
    lifecycle::{Action, PowerCoordinator},
    lighting::color::Rgb,
    runtime::{Host, scene::Scene},
    schedule::{self, AttemptState, Schedule, Target},
    storage,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

const JOURNAL_LIMIT: usize = 1024 * 1024;
const SCOPE: &str = "pc-control.preview";
const PREVIEW_RESULT: &str = "Preview desired-state only; no hardware output";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    Preview,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Desired {
    pc: bool,
    door: bool,
}
impl Desired {
    fn get(self, target: Target) -> bool {
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Preview,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewRecord {
    target: Target,
    event: schedule::Event,
    attempt: u8,
    outcome: Outcome,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    scope: String,
    mode: Mode,
    setup_groups_sha256: String,
    target_ids: Groups,
    desired: Desired,
    outcomes: [Option<PreviewRecord>; 2],
    ledger: serde_json::Value,
}
fn index(target: Target) -> usize {
    match target {
        Target::Pc => 0,
        Target::Door => 1,
    }
}
fn valid_time(now: i64) -> bool {
    now > 0 && chrono::DateTime::<chrono::Utc>::from_timestamp(now, 0).is_some()
}
fn fingerprint(project: &Project) -> Result<String, String> {
    let bytes =
        serde_json::to_vec(&(&project.setup, &project.groups)).map_err(|e| e.to_string())?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
fn persist(
    path: Option<&Path>,
    fingerprint: &str,
    groups: &Groups,
    desired: Desired,
    outcomes: [Option<PreviewRecord>; 2],
    ledger: &[u8],
) -> Result<(), String> {
    let journal = Journal {
        version: 1,
        scope: SCOPE.into(),
        mode: Mode::Preview,
        setup_groups_sha256: fingerprint.into(),
        target_ids: groups.clone(),
        desired,
        outcomes,
        ledger: serde_json::from_slice(ledger).map_err(|e| e.to_string())?,
    };
    let bytes = serde_json::to_vec(&journal).map_err(|e| e.to_string())?;
    if bytes.len() > JOURNAL_LIMIT {
        return Err("Preview journal exceeds 1 MiB".into());
    }
    // None is explicitly session memory, not a claim of on-disk or physical durability.
    if let Some(path) = path {
        storage::atomic_write(path, &bytes, JOURNAL_LIMIT).map_err(|e| e.to_string())?;
    }
    Ok(())
}
type Restored = (Schedule, Desired, [Option<PreviewRecord>; 2]);
fn restore(path: &Path, fingerprint: &str, groups: &Groups) -> Result<Option<Restored>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > JOURNAL_LIMIT as u64
    {
        return Err("Preview journal must be a regular file of at most 1 MiB".into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(JOURNAL_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > JOURNAL_LIMIT {
        return Err("Preview journal exceeds 1 MiB".into());
    }
    let journal: Journal = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if journal.version != 1
        || journal.scope != SCOPE
        || journal.mode != Mode::Preview
        || journal.setup_groups_sha256 != fingerprint
        || journal.target_ids != *groups
    {
        return Err("Preview journal version, scope or setup/groups fingerprint mismatch".into());
    }
    let schedule =
        Schedule::restore(&serde_json::to_vec(&journal.ledger).map_err(|e| e.to_string())?)?;
    for target in [Target::Pc, Target::Door] {
        let record = journal.outcomes[index(target)];
        if record.is_some_and(|r| {
            r.target != target
                || r.outcome != Outcome::Preview
                || !(1..=3).contains(&r.attempt)
                || !valid_time(r.event.occurrence)
        }) {
            return Err("Invalid preview target/outcome metadata".into());
        }
        if let Some(attempt) = schedule.attempt(target) {
            match attempt.state {
                AttemptState::Attempting if attempt.error.is_empty() => {}
                AttemptState::Submitted
                    if attempt.error == PREVIEW_RESULT
                        && record.is_some_and(|r| {
                            r.event == attempt.event && r.attempt == attempt.count
                        }) => {}
                _ => return Err("Non-preview completion in preview journal".into()),
            }
        }
    }
    Ok(Some((schedule, journal.desired, journal.outcomes)))
}

pub struct Preview {
    project: Project,
    host: Host,
    schedule: Schedule,
    journal_path: Option<PathBuf>,
    fingerprint: String,
    desired: Desired,
    outcomes: [Option<PreviewRecord>; 2],
    point_targets: Vec<Target>,
    colors: Vec<Rgb>,
    next_render: Option<u64>,
    last_mono: Option<u64>,
    last_schedule_unix: Option<i64>,
    power: PowerCoordinator,
    suspended: bool,
    dirty: bool,
}
impl Preview {
    pub fn new(project: Project, journal_path: Option<PathBuf>) -> Result<Self, String> {
        project.validate()?;
        let fingerprint = fingerprint(&project)?;
        let loaded = journal_path
            .as_deref()
            .map(|path| restore(path, &fingerprint, &project.groups))
            .transpose()?
            .flatten();
        let (mut schedule, desired, outcomes) = loaded.unwrap_or((
            Schedule::new(project.schedule.clone())?,
            Desired {
                pc: true,
                door: true,
            },
            [None, None],
        ));
        let mut write = |ledger: &[u8]| {
            persist(
                journal_path.as_deref(),
                &fingerprint,
                &project.groups,
                desired,
                outcomes,
                ledger,
            )
        };
        // The supplied Project scene/config are authoritative, including explicit File Open.
        // No hardware needs startup replay. Same-config configure clears only ephemeral reconcile.
        schedule.configure(project.schedule.clone(), &mut write)?;
        let host = Host::new(project.setup.clone(), project.scene.clone())?;
        let point_targets = project
            .setup
            .canvas
            .points
            .iter()
            .map(|point| {
                if project.groups.pc.contains(&point.device) {
                    Target::Pc
                } else {
                    Target::Door
                }
            })
            .collect();
        let colors = vec![Rgb::default(); project.setup.canvas.points.len()];
        Ok(Self {
            project,
            host,
            schedule,
            journal_path,
            fingerprint,
            desired,
            outcomes,
            point_targets,
            colors,
            next_render: None,
            last_mono: None,
            last_schedule_unix: None,
            power: PowerCoordinator::default(),
            suspended: false,
            dirty: true,
        })
    }
    pub fn project(&self) -> &Project {
        &self.project
    }
    /// In exact canvas.points order, including disabled devices; masked preview only.
    pub fn colors(&self) -> &[Rgb] {
        &self.colors
    }
    /// Desired preview visibility, never device power/connection telemetry.
    pub fn group_on(&self, target: Target) -> bool {
        self.desired.get(target)
    }
    /// May be false after a persistence failure while the unchanged project still requests enabled.
    pub fn schedule_active(&self) -> bool {
        self.schedule.config().enabled && !self.schedule.config().paused
    }
    fn persist_current(&self, desired: Desired) -> Result<(), String> {
        persist(
            self.journal_path.as_deref(),
            &self.fingerprint,
            &self.project.groups,
            desired,
            self.outcomes,
            &self.schedule.encoded()?,
        )
    }
    pub fn set_scene(&mut self, scene: Scene) -> Result<(), String> {
        scene.validate()?;
        if scene == self.project.scene {
            return Ok(());
        }
        // Journal is a successful storage barrier only; unsaved scenes never override File Open.
        self.persist_current(self.desired)?;
        self.host.configure_scene(scene.clone())?;
        self.project.scene = scene;
        self.next_render = None;
        self.dirty = true;
        Ok(())
    }
    pub fn set_group(&mut self, target: Target, on: bool) -> Result<(), String> {
        if self.desired.get(target) == on {
            return Ok(());
        }
        let mut desired = self.desired;
        desired.set(target, on);
        self.persist_current(desired)?;
        self.desired = desired;
        self.mask();
        self.dirty = true;
        Ok(())
    }
    pub fn configure_schedule(&mut self, config: schedule::Config) -> Result<(), String> {
        Schedule::new(config.clone())?;
        let mut next = self.schedule.clone();
        let result = next.configure(config.clone(), &mut |ledger| {
            persist(
                self.journal_path.as_deref(),
                &self.fingerprint,
                &self.project.groups,
                self.desired,
                self.outcomes,
                ledger,
            )
        });
        self.schedule = next; // Core disables itself on persistence failure; never dispatch through it.
        result?;
        self.project.schedule = config;
        self.last_schedule_unix = None;
        self.dirty = true;
        Ok(())
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.project.save(path)
    }
    fn observe(&mut self, mono_us: u64) -> Result<(), String> {
        if self.last_mono.is_some_and(|last| mono_us < last) {
            return Err("Preview monotonic clock moved backwards".into());
        }
        self.last_mono = Some(mono_us);
        Ok(())
    }
    /// Synthetic preview lifecycle only. No native power APIs, device refresh or hardware ACK.
    pub fn notify_power(&mut self, suspended: bool, mono_us: u64) -> Result<(), String> {
        self.observe(mono_us)?;
        self.power
            .notify(suspended, mono_us / 1000)
            .map_err(str::to_owned)?;
        self.suspended = suspended;
        self.dirty = true;
        Ok(())
    }
    fn mask(&mut self) -> bool {
        let mut changed = false;
        for ((output, target), source) in self.colors.iter_mut().zip(&self.point_targets).zip(
            self.host
                .preview()
                .iter()
                .copied()
                .chain(std::iter::repeat(Rgb::default())),
        ) {
            let next = if self.desired.get(*target) {
                source
            } else {
                Rgb::default()
            };
            changed |= *output != next;
            *output = next;
        }
        changed
    }
    fn run_schedule(&mut self, now: i64) -> Result<bool, String> {
        let path = self.journal_path.as_deref();
        let fingerprint = &self.fingerprint;
        let groups = &self.project.groups;
        let desired = self.desired;
        let outcomes = self.outcomes;
        let dispatches = self.schedule.prepare(now, &mut |ledger| {
            persist(path, fingerprint, groups, desired, outcomes, ledger)
        })?;
        let mut changed = false;
        for dispatch in dispatches {
            let mut desired = self.desired;
            desired.set(dispatch.target, dispatch.event.on);
            let mut outcomes = self.outcomes;
            outcomes[index(dispatch.target)] = Some(PreviewRecord {
                target: dispatch.target,
                event: dispatch.event,
                attempt: dispatch.attempt,
                outcome: Outcome::Preview,
            });
            self.schedule.finish(
                dispatch,
                now,
                schedule::Outcome::Submitted,
                PREVIEW_RESULT,
                &mut |ledger| persist(path, fingerprint, groups, desired, outcomes, ledger),
            )?;
            // Desired state and its preview ledger are committed together before view mutation.
            self.desired = desired;
            self.outcomes = outcomes;
            changed = true;
        }
        Ok(changed)
    }
    pub fn tick(&mut self, mono_us: u64, unix_seconds: i64) -> Result<bool, String> {
        if !valid_time(unix_seconds) {
            return Err("Preview requires an injected positive representable Unix time".into());
        }
        self.observe(mono_us)?;
        for _ in 0..2 {
            let active = self.schedule_active();
            match self
                .power
                .poll(mono_us / 1000, false, active)
                .map_err(str::to_owned)?
            {
                Some(Action::Refresh { generation }) => {
                    // Only this already-owned preview model is validated; no device discovery.
                    let result = self.project.validate();
                    self.power
                        .refreshed(
                            generation,
                            mono_us / 1000,
                            result.as_ref().map(|_| ()).map_err(String::as_str),
                        )
                        .map_err(str::to_owned)?;
                    result?;
                    self.next_render = None;
                }
                Some(Action::ReconcileSchedule { .. }) => {
                    // Prepare due events normally. Do not replay old submitted events over manual edits.
                    self.last_schedule_unix = None;
                    break;
                }
                Some(Action::RestoreUserIntent { .. }) => {
                    self.next_render = None;
                    break;
                }
                None => break,
            }
        }
        if self.suspended || self.power.pending() {
            return Ok(std::mem::take(&mut self.dirty));
        }
        if self.last_schedule_unix != Some(unix_seconds) {
            match self.run_schedule(unix_seconds) {
                Ok(changed) => self.dirty |= changed,
                Err(error) => {
                    self.mask();
                    return Err(error);
                }
            }
            self.last_schedule_unix = Some(unix_seconds);
        }
        if self.next_render.is_none_or(|due| mono_us >= due) {
            self.host.tick(mono_us)?;
            let period = 1_000_000u64.div_ceil(u64::from(self.project.scene.fps));
            self.next_render = Some(match self.next_render {
                Some(due) => {
                    due.saturating_add(((mono_us - due) / period + 1).saturating_mul(period))
                }
                None => mono_us.saturating_add(period),
            });
        }
        self.dirty |= self.mask();
        Ok(std::mem::take(&mut self.dirty))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::{Canvas, Device, Driver, Point, Role, Setup};
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    fn project(enabled: bool) -> Project {
        let make = |id: &str, leds: usize, role: Role| Device {
            id: id.into(),
            label: id.into(),
            role,
            driver: Driver::Unsupported {
                protocol: "synthetic preview".into(),
            },
            leds,
            rate_hz: 25,
            enabled: false,
        };
        Project {
            version: 1,
            setup: Setup {
                version: 1,
                devices: vec![
                    make("pc-a", 2, Role::Canvas),
                    make("door", 2, Role::Canvas),
                    make("pc-b", 1, Role::Canvas),
                    make("ambient", 0, Role::Ambient),
                    make("lcd", 0, Role::Lcd),
                ],
                canvas: Canvas {
                    width: 5,
                    height: 1,
                    points: vec![
                        Point {
                            device: "door".into(),
                            led: 1,
                            x: 4,
                            y: 0,
                        },
                        Point {
                            device: "pc-a".into(),
                            led: 1,
                            x: 3,
                            y: 0,
                        },
                        Point {
                            device: "pc-b".into(),
                            led: 0,
                            x: 1,
                            y: 0,
                        },
                        Point {
                            device: "door".into(),
                            led: 0,
                            x: 0,
                            y: 0,
                        },
                        Point {
                            device: "pc-a".into(),
                            led: 0,
                            x: 2,
                            y: 0,
                        },
                    ],
                },
            },
            scene: crate::runtime::scene::example(),
            schedule: schedule::Config {
                enabled,
                timezone: "UTC".into(),
                ..Default::default()
            },
            groups: Groups {
                pc: vec!["pc-a".into(), "pc-b".into(), "ambient".into(), "lcd".into()],
                door: vec!["door".into()],
            },
        }
    }
    fn at(value: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(value)
            .unwrap()
            .timestamp()
    }
    fn evening() -> i64 {
        at("2026-11-06T18:00:00Z")
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Stand(PathBuf);
    impl Stand {
        fn new() -> Self {
            let p = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("work/app-preview-stand")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> PathBuf {
            self.0.join("journal.json")
        }
        fn block(&self) {
            fs::rename(self.path(), self.0.join("previous.json")).unwrap();
            fs::create_dir(self.path()).unwrap();
        }
    }
    impl Drop for Stand {
        fn drop(&mut self) {
            for name in ["journal.json", "previous.json", "project.json"] {
                let p = self.0.join(name);
                let _ = fs::remove_file(&p);
                let _ = fs::remove_dir(&p);
            }
            let _ = fs::remove_dir(&self.0);
        }
    }
    #[test]
    fn real_host_native_speed_cadence_point_order_and_reused_view_mask() {
        let source = project(false);
        let mut expected = Host::new(source.setup.clone(), source.scene.clone()).unwrap();
        let mut p = Preview::new(source, None).unwrap();
        let address = p.colors().as_ptr();
        assert!(p.tick(0, evening()).unwrap());
        expected.tick(0).unwrap();
        assert_eq!(p.colors(), expected.preview());
        assert_eq!(p.project().scene.speed, 8);
        assert_eq!(p.colors().len(), 5);
        assert!(!p.tick(1_000, evening()).unwrap());
        assert!(!p.tick(39_999, evening()).unwrap());
        p.tick(40_000, evening()).unwrap();
        expected.tick(40_000).unwrap();
        assert_eq!(p.colors(), expected.preview());
        p.set_group(Target::Pc, false).unwrap();
        for (i, target) in p.point_targets.iter().enumerate() {
            assert_eq!(
                p.colors()[i],
                if *target == Target::Pc {
                    Rgb::default()
                } else {
                    expected.preview()[i]
                }
            );
        }
        p.tick(80_000, evening()).unwrap();
        expected.tick(80_000).unwrap();
        p.set_group(Target::Pc, true).unwrap();
        assert_eq!(p.colors(), expected.preview());
        assert_eq!(p.colors().as_ptr(), address);
        for device in &p.project.setup.devices {
            assert!(p.host.status(&device.id).is_none());
            assert!(p.host.requested(&device.id).is_none());
        }
    }
    #[test]
    fn failed_storage_precedes_manual_scene_visibility_and_configuration_changes() {
        let stand = Stand::new();
        let mut p = Preview::new(project(true), Some(stand.path())).unwrap();
        p.tick(0, evening()).unwrap();
        let prior = p.project().clone();
        let colors = p.colors().to_vec();
        let old = fs::read(stand.path()).unwrap();
        stand.block();
        assert!(p.set_group(Target::Pc, false).is_err());
        assert!(p.group_on(Target::Pc));
        assert_eq!(p.colors(), colors);
        let mut scene = prior.scene.clone();
        scene.brightness = 1;
        assert!(p.set_scene(scene).is_err());
        assert_eq!(p.project(), &prior);
        assert_eq!(p.host.scene(), &prior.scene);
        assert_eq!(p.colors(), colors);
        let mut config = prior.schedule.clone();
        config.paused = true;
        assert!(p.configure_schedule(config).is_err());
        assert_eq!(p.project(), &prior);
        assert!(!p.schedule_active());
        assert_eq!(p.colors(), colors);
        assert_eq!(fs::read(stand.0.join("previous.json")).unwrap(), old);
    }
    #[test]
    fn durable_preview_results_and_manual_intent_survive_without_duplicate_startup_dispatch() {
        let stand = Stand::new();
        let source = project(true);
        let mut p = Preview::new(source.clone(), Some(stand.path())).unwrap();
        p.tick(0, evening()).unwrap();
        assert!(p.group_on(Target::Pc) && p.group_on(Target::Door));
        p.set_group(Target::Pc, false).unwrap();
        let records = p.outcomes;
        let bytes = fs::read(stand.path()).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["mode"], "preview");
        assert_eq!(value["scope"], SCOPE);
        assert_eq!(value["outcomes"][0]["outcome"], "preview");
        assert!(value.get("scene").is_none());
        assert!(value.get("bindings").is_none());
        p.tick(40_000, evening()).unwrap();
        assert_eq!(fs::read(stand.path()).unwrap(), bytes);
        drop(p);
        let mut restarted = Preview::new(source, Some(stand.path())).unwrap();
        let restored = fs::read(stand.path()).unwrap();
        restarted.tick(0, evening() + 1).unwrap();
        assert!(!restarted.group_on(Target::Pc));
        assert_eq!(restarted.outcomes, records);
        assert_eq!(fs::read(stand.path()).unwrap(), restored);
        restarted.tick(40_000, at("2026-11-07T11:00:00Z")).unwrap();
        assert!(restarted.group_on(Target::Pc));
        assert!(!restarted.group_on(Target::Door));
    }
    #[test]
    fn project_open_is_authoritative_and_journal_cannot_cross_map_groups_or_scope() {
        let stand = Stand::new();
        let source = project(false);
        let mut p = Preview::new(source.clone(), Some(stand.path())).unwrap();
        p.set_group(Target::Door, false).unwrap();
        let mut draft = source.scene.clone();
        draft.brightness = 2;
        p.set_scene(draft).unwrap();
        drop(p);
        let mut opened = source.clone();
        opened.scene.brightness = 95;
        opened.schedule.paused = true;
        let loaded = Preview::new(opened.clone(), Some(stand.path())).unwrap();
        assert_eq!(loaded.project(), &opened);
        assert!(!loaded.group_on(Target::Door));
        drop(loaded);
        let good = fs::read(stand.path()).unwrap();
        let mut other = opened.clone();
        other.setup.canvas.points[0].x = 3;
        assert!(Preview::new(other, Some(stand.path())).is_err());
        assert_eq!(fs::read(stand.path()).unwrap(), good);
        let value: serde_json::Value = serde_json::from_slice(&good).unwrap();
        for case in 0..6 {
            let mut bad = value.clone();
            match case {
                0 => bad["version"] = serde_json::json!(2),
                1 => bad["scope"] = serde_json::json!("physical-runtime"),
                2 => bad["mode"] = serde_json::json!("live"),
                3 => bad["target_ids"]["pc"][0] = serde_json::json!("unknown"),
                4 => bad["bindings"] = serde_json::json!({"door":"PRIVATE"}),
                _ => bad["setup_groups_sha256"] = serde_json::json!("bad"),
            }
            let bytes = serde_json::to_vec(&bad).unwrap();
            fs::write(stand.path(), &bytes).unwrap();
            assert!(Preview::new(opened.clone(), Some(stand.path())).is_err());
            assert_eq!(fs::read(stand.path()).unwrap(), bytes);
        }
    }
    #[test]
    fn schedule_prepare_storage_failure_cannot_apply_uncommitted_desire() {
        let stand = Stand::new();
        let mut p = Preview::new(project(true), Some(stand.path())).unwrap();
        stand.block();
        let colors = p.colors().to_vec();
        assert!(p.tick(0, at("2026-11-07T03:00:00Z")).is_err());
        assert!(p.group_on(Target::Pc) && p.group_on(Target::Door));
        assert_eq!(p.colors(), colors);
        assert!(!p.schedule_active());
        assert!(p.project().schedule.enabled);
        assert!(p.outcomes.iter().all(Option::is_none));
    }
    #[test]
    fn preview_journal_rejects_oversize_and_physical_completion_labels() {
        let stand = Stand::new();
        let source = project(true);
        let mut p = Preview::new(source.clone(), Some(stand.path())).unwrap();
        p.tick(0, evening()).unwrap();
        drop(p);
        let good: serde_json::Value =
            serde_json::from_slice(&fs::read(stand.path()).unwrap()).unwrap();
        for case in 0..3 {
            let mut bad = good.clone();
            match case {
                0 => bad["ledger"]["attempts"][0]["error"] = serde_json::json!("Physical ACK"),
                1 => bad["outcomes"][0]["outcome"] = serde_json::json!("submitted"),
                _ => bad["outcomes"][0]["target"] = serde_json::json!("Door"),
            }
            fs::write(stand.path(), serde_json::to_vec(&bad).unwrap()).unwrap();
            assert!(Preview::new(source.clone(), Some(stand.path())).is_err());
        }
        fs::write(stand.path(), vec![b' '; JOURNAL_LIMIT + 1]).unwrap();
        assert!(Preview::new(source, Some(stand.path())).is_err());
    }
    #[test]
    fn interrupted_preview_claim_is_restored_without_automatic_replay() {
        let stand = Stand::new();
        let source = project(true);
        let mut p = Preview::new(source.clone(), Some(stand.path())).unwrap();
        let path = stand.path();
        let fingerprint = p.fingerprint.clone();
        let groups = p.project.groups.clone();
        let desired = p.desired;
        let outcomes = p.outcomes;
        let claims = p
            .schedule
            .prepare(evening(), &mut |ledger| {
                persist(
                    Some(&path),
                    &fingerprint,
                    &groups,
                    desired,
                    outcomes,
                    ledger,
                )
            })
            .unwrap();
        assert_eq!(claims.len(), 2);
        drop(p);
        let mut restarted = Preview::new(source, Some(path.clone())).unwrap();
        let before = fs::read(&path).unwrap();
        restarted.tick(0, evening() + 1).unwrap();
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(restarted.outcomes.iter().all(Option::is_none));
        for target in [Target::Pc, Target::Door] {
            assert_eq!(
                restarted.schedule.attempt(target).unwrap().state,
                AttemptState::Attempting
            );
        }
    }
    #[test]
    fn injected_clock_regression_is_checked_and_core_corrects_future_wall_watermarks_once() {
        let mut p = Preview::new(project(true), None).unwrap();
        p.tick(1_000, evening()).unwrap();
        let colors = p.colors().to_vec();
        assert!(p.tick(999, evening()).is_err());
        assert_eq!(p.colors(), colors);
        assert!(p.notify_power(true, 999).is_err());
        assert!(!p.suspended);
        assert!(p.tick(1_000, i64::MAX).is_err());
        let corrected = at("2026-11-06T03:00:00Z");
        p.tick(2_000, corrected).unwrap();
        assert!(!p.group_on(Target::Pc) && !p.group_on(Target::Door));
        let records = p.outcomes;
        p.tick(3_000, corrected).unwrap();
        assert_eq!(p.outcomes, records);
    }
    #[test]
    fn synthetic_resume_settles_and_uses_current_intent_not_suspend_snapshot() {
        let mut p = Preview::new(project(true), None).unwrap();
        p.tick(0, evening()).unwrap();
        p.notify_power(true, 1_000_000).unwrap();
        let host = p.host.preview().to_vec();
        p.tick(2_000_000, evening() + 1).unwrap();
        assert_eq!(p.host.preview(), host);
        p.notify_power(false, 3_000_000).unwrap();
        p.set_group(Target::Pc, false).unwrap();
        p.tick(9_999_999, evening() + 2).unwrap();
        assert_eq!(p.host.preview(), host);
        assert!(!p.group_on(Target::Pc));
        p.tick(10_000_000, evening() + 3).unwrap();
        assert!(!p.group_on(Target::Pc));
        assert_eq!(p.power.resumes, 1);
        p.notify_power(false, 10_000_001).unwrap();
        assert_eq!(p.power.coalesced, 1);
        // A genuinely later due event still controls preview, unlike replaying the old event.
        p.tick(10_040_000, at("2026-11-07T11:00:00Z")).unwrap();
        assert!(p.group_on(Target::Pc));
        assert!(!p.group_on(Target::Door));
        for device in &p.project.setup.devices {
            assert!(p.host.status(&device.id).is_none());
        }
    }
    #[test]
    fn memory_mode_and_invalid_edits_do_not_claim_or_create_disk_state() {
        let stand = Stand::new();
        let mut p = Preview::new(project(false), None).unwrap();
        p.tick(0, evening()).unwrap();
        let prior = p.project().clone();
        let mut bad = prior.scene.clone();
        bad.speed = 0;
        assert!(p.set_scene(bad).is_err());
        assert_eq!(p.project(), &prior);
        let mut invalid = prior.schedule.clone();
        invalid.timezone = "UNKNOWN/ZONE".into();
        assert!(p.configure_schedule(invalid).is_err());
        assert_eq!(p.project(), &prior);
        p.set_group(Target::Door, false).unwrap();
        assert!(p.journal_path.is_none());
        assert_eq!(fs::read_dir(&stand.0).unwrap().count(), 0);
    }
}
