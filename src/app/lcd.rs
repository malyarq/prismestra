//! Asset-backed LCD playback with an explicitly supplied transport; never opens a device.
use super::assets::{AssetId, Store};
use crate::lcd::{media, overlay, player, protocol::ReportTransport};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};

const FONT_LIMIT: usize = 8 * 1024 * 1024;
const MAX_SOURCES: usize = 32;
const MAX_SAMPLES: usize = 4096;
const TTL_MS: u64 = 5000;

fn key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
fn label(value: &str) -> bool {
    !value.trim().is_empty() && value.chars().count() <= 128 && !value.chars().any(char::is_control)
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlaySource {
    pub key: String,
    pub label: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub media: Option<AssetId>,
    pub font: Option<AssetId>,
    pub rotation: i32,
    pub overlay_enabled: bool,
    pub interval_seconds: u8,
    pub sources: Vec<OverlaySource>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            media: None,
            font: None,
            rotation: 0,
            overlay_enabled: false,
            interval_seconds: 5,
            sources: vec![],
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<(), String> {
        media::rotation(self.rotation)?;
        if !(2..=60).contains(&self.interval_seconds) || self.sources.len() > MAX_SOURCES {
            return Err(
                "LCD requires interval 2..60 seconds and at most 32 overlay sources".into(),
            );
        }
        let mut selected = BTreeSet::new();
        for source in &self.sources {
            if !key(&source.key) || !label(&source.label) || !selected.insert(source.key.as_str()) {
                return Err(
                    "LCD overlay sources require unique portable logical keys and bounded labels"
                        .into(),
                );
            }
        }
        if self.enabled && self.media.is_none() {
            return Err("Enabled LCD requires a media asset".into());
        }
        if self.overlay_enabled && (self.font.is_none() || self.sources.is_empty()) {
            return Err("Enabled LCD overlay requires a font asset and selected sources".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub key: String,
    pub celsius: f64,
    pub observed_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Disabled,
    Suspended,
    Stopped,
    Running,
    Blanked,
    Failed(String),
}

/// Preserve configuration order, omit unavailable data, and never manufacture a temperature.
fn readings(
    config: &Config,
    samples: &[Sample],
    now: u64,
) -> Result<(Option<u64>, Vec<player::OverlayValue>), String> {
    if samples.len() > MAX_SAMPLES {
        return Err("LCD sample batch exceeds 4096 readings".into());
    }
    let mut seen = BTreeSet::new();
    for sample in samples {
        if !key(&sample.key) || !seen.insert(sample.key.as_str()) {
            return Err("Invalid or ambiguous LCD logical sample key".into());
        }
    }
    let mut oldest = None;
    let mut values = Vec::with_capacity(config.sources.len());
    for source in &config.sources {
        let Some(sample) = samples.iter().find(|sample| sample.key == source.key) else {
            continue;
        };
        if !sample.celsius.is_finite()
            || !(-20.0..=150.0).contains(&sample.celsius)
            || sample.observed_ms == 0
            || sample.observed_ms > now
            || now - sample.observed_ms > TTL_MS
        {
            continue;
        }
        oldest = Some(oldest.map_or(sample.observed_ms, |at: u64| at.min(sample.observed_ms)));
        values.push(player::OverlayValue {
            id: source.key.clone(),
            label: source.label.clone(),
            celsius: sample.celsius,
        });
    }
    Ok((oldest, values))
}

pub struct Controller {
    config: Config,
    player: Option<player::Player>,
    suspended: bool,
    stop_pending: bool,
    off_done: bool,
    frames_sent: u64,
    preview: Vec<u8>,
}
impl Controller {
    pub fn new(config: Config, store: &Store) -> Result<Self, String> {
        config.validate()?;
        let player = if config.enabled {
            let source: Arc<[u8]> = Arc::from(store.read(
                config.media.as_ref().ok_or("Missing LCD media")?,
                media::MAX_SOURCE,
            )?);
            let prepared = Arc::new(media::prepare(source, config.rotation, || false)?);
            Some(if config.overlay_enabled {
                let font = store.read(
                    config.font.as_ref().ok_or("Missing LCD overlay font")?,
                    FONT_LIMIT,
                )?;
                player::Player::new(prepared, overlay::Renderer::new(font)?)?
            } else {
                // Optional unselected font references do not cause any read or parse.
                player::Player::without_overlay(prepared)?
            })
        } else {
            None
        };
        Ok(Self {
            config,
            player,
            suspended: false,
            stop_pending: false,
            off_done: false,
            frames_sent: 0,
            preview: vec![],
        })
    }
    pub fn config(&self) -> &Config {
        &self.config
    }
    pub fn state(&self) -> State {
        match self.player.as_ref().map(player::Player::state) {
            None => State::Disabled,
            Some(player::State::Failed(error)) => State::Failed(error.clone()),
            _ if self.suspended => State::Suspended,
            _ if self.stop_pending => State::Stopped,
            Some(player::State::Stopped) => State::Stopped,
            Some(player::State::Running) => State::Running,
            Some(player::State::Blanked) => State::Blanked,
        }
    }
    /// Last complete ACKed JPEG across clean restarts, not physical display confirmation.
    pub fn preview(&self) -> &[u8] {
        &self.preview
    }
    /// Lifetime complete-frame count, including blank frames; never counts partial writes.
    pub fn frames_sent(&self) -> u64 {
        self.frames_sent
    }
    pub fn next_due_ms(&self) -> Option<u64> {
        if self.suspended || self.stop_pending {
            None
        } else {
            self.player.as_ref().and_then(player::Player::next_due_ms)
        }
    }
    /// Request stop only. The suspended flag prevents even clock/transport polling hereafter.
    pub fn suspend(&mut self) {
        self.suspended = true;
        if let Some(player) = &self.player {
            player.requests().stop();
            self.stop_pending = true;
        }
    }
    pub fn resume(&mut self) {
        self.suspended = false;
    }
    pub fn poll<T: ReportTransport>(
        &mut self,
        transport: &mut T,
        active: bool,
        samples: &[Sample],
    ) -> Result<Option<player::Sent>, String> {
        let Some(player) = &mut self.player else {
            return Ok(None);
        };
        if let player::State::Failed(error) = player.state() {
            return Err(error.clone());
        }
        if self.suspended {
            return Ok(None);
        }
        if self.stop_pending {
            // Player processes queued STOP before reading its injected clock or issuing any reports.
            if player.state() == &player::State::Running {
                player.poll(&player::Overlay::default(), transport)?;
            }
            self.stop_pending = false;
        }
        let overlay = if active && self.config.overlay_enabled {
            let (oldest, values) = readings(&self.config, samples, transport.now_ms())?;
            player::Overlay::new(true, self.config.interval_seconds, oldest, values)?
        } else {
            player::Overlay::default()
        };
        if !active && self.off_done {
            return Ok(None);
        }
        if matches!(
            player.state(),
            player::State::Stopped | player::State::Blanked
        ) {
            player.start()?;
        }
        if active {
            self.off_done = false;
        } else {
            player.requests().blank();
        }
        let sent = player.poll(&overlay, transport)?;
        if let Some(sent) = sent {
            self.frames_sent = self.frames_sent.saturating_add(1);
            self.preview.clear();
            self.preview.extend_from_slice(player.preview());
            if !active && sent.frame_index.is_none() {
                self.off_done = true;
            }
        }
        Ok(sent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lcd::protocol::{ACK_BYTES, REPORT_BYTES, REPORT_DEADLINE_MS};
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Stand {
        root: PathBuf,
        store: Store,
    }
    impl Stand {
        fn new() -> Self {
            let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("work/app-lcd-stand");
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
            Self { root, store }
        }
        fn config(&self) -> Config {
            Config {
                enabled: true,
                media: Some(
                    self.store
                        .put(
                            include_bytes!("../lcd/fixtures/two-frame.apng"),
                            media::MAX_SOURCE,
                        )
                        .unwrap(),
                ),
                ..Default::default()
            }
        }
    }
    impl Drop for Stand {
        fn drop(&mut self) {
            let assets = self.root.join("assets");
            if let Ok(entries) = fs::read_dir(&assets) {
                for entry in entries.flatten() {
                    let _ = fs::remove_file(entry.path());
                }
            }
            let _ = fs::remove_dir(assets);
            let _ = fs::remove_dir(&self.root);
        }
    }
    struct Fake {
        now: u64,
        writes: usize,
        acks: usize,
        fail_ack: bool,
        reports: Vec<[u8; REPORT_BYTES]>,
    }
    impl Default for Fake {
        fn default() -> Self {
            Self {
                now: 1000,
                writes: 0,
                acks: 0,
                fail_ack: false,
                reports: vec![],
            }
        }
    }
    impl ReportTransport for Fake {
        fn now_ms(&self) -> u64 {
            self.now
        }
        fn write_report(
            &mut self,
            report: &[u8; REPORT_BYTES],
            timeout: u64,
        ) -> Result<usize, String> {
            assert_eq!(timeout, REPORT_DEADLINE_MS);
            assert_eq!((report[0], report[1]), (0, 8));
            self.writes += 1;
            self.now += 1;
            self.reports.push(*report);
            Ok(REPORT_BYTES)
        }
        fn read_ack(
            &mut self,
            response: &mut [u8; ACK_BYTES],
            timeout: u64,
        ) -> Result<usize, String> {
            assert_eq!(timeout, REPORT_DEADLINE_MS);
            self.now += 1;
            self.acks += 1;
            if self.fail_ack {
                return Err("Injected ACK error".into());
            }
            response.fill(0xa5);
            response[0] = 0;
            Ok(ACK_BYTES)
        }
    }
    #[test]
    fn clean_media_run_real_blank_once_and_clean_resume_use_actual_player() {
        let stand = Stand::new();
        let mut c = Controller::new(stand.config(), &stand.store).unwrap();
        let mut t = Fake::default();
        assert_eq!(c.state(), State::Stopped);
        assert!(c.preview().is_empty());
        let sent = c.poll(&mut t, true, &[]).unwrap().unwrap();
        assert_eq!(sent.frame_index, Some(0));
        assert!(!sent.overlay_applied);
        assert_eq!(c.frames_sent(), 1);
        assert_eq!(t.acks, 1);
        crate::lcd::protocol::validate_jpeg(c.preview()).unwrap();
        assert!(c.poll(&mut t, true, &[]).unwrap().is_none());
        let off = c.poll(&mut t, false, &[]).unwrap().unwrap();
        assert_eq!(off.frame_index, None);
        assert_eq!(c.state(), State::Blanked);
        assert_eq!(c.frames_sent(), 2);
        assert!(
            image::load_from_memory(c.preview())
                .unwrap()
                .into_rgb8()
                .pixels()
                .all(|p| p.0 == [0, 0, 0])
        );
        let writes = t.writes;
        assert!(c.poll(&mut t, false, &[]).unwrap().is_none());
        assert_eq!(t.writes, writes);
        let resumed = c.poll(&mut t, true, &[]).unwrap().unwrap();
        assert_eq!(resumed.frame_index, Some(0));
        assert_eq!(c.frames_sent(), 3);
        assert_eq!(c.state(), State::Running);
    }
    #[test]
    fn first_off_really_sends_black_instead_of_only_setting_a_flag() {
        let stand = Stand::new();
        let mut c = Controller::new(stand.config(), &stand.store).unwrap();
        let mut t = Fake::default();
        assert_eq!(
            c.poll(&mut t, false, &[]).unwrap().unwrap().frame_index,
            None
        );
        assert!(t.writes > 0);
        assert_eq!(t.acks, 1);
        assert_eq!(c.frames_sent(), 1);
        assert_eq!(c.state(), State::Blanked);
        let writes = t.writes;
        for _ in 0..3 {
            assert!(c.poll(&mut t, false, &[]).unwrap().is_none());
        }
        assert_eq!(t.writes, writes);
    }
    #[test]
    fn suspend_never_polls_hardware_and_resume_drains_stop_before_restarting() {
        struct Never;
        impl ReportTransport for Never {
            fn now_ms(&self) -> u64 {
                panic!("Suspended controller must not poll its clock")
            }
            fn write_report(&mut self, _: &[u8; REPORT_BYTES], _: u64) -> Result<usize, String> {
                panic!("No suspended writes")
            }
            fn read_ack(&mut self, _: &mut [u8; ACK_BYTES], _: u64) -> Result<usize, String> {
                panic!("No suspended reads")
            }
        }
        let stand = Stand::new();
        let mut c = Controller::new(stand.config(), &stand.store).unwrap();
        let mut t = Fake::default();
        c.poll(&mut t, true, &[]).unwrap();
        let before = c.preview().to_vec();
        c.suspend();
        assert_eq!(c.state(), State::Suspended);
        assert!(c.next_due_ms().is_none());
        assert!(c.poll(&mut Never, true, &[]).unwrap().is_none());
        assert!(c.poll(&mut Never, false, &[]).unwrap().is_none());
        assert_eq!(c.preview(), before);
        c.resume();
        assert_eq!(
            c.poll(&mut t, true, &[]).unwrap().unwrap().frame_index,
            Some(0)
        );
        assert_eq!(c.frames_sent(), 2);
        c.suspend();
        c.resume();
        assert_eq!(
            c.poll(&mut t, false, &[]).unwrap().unwrap().frame_index,
            None
        );
        assert_eq!(c.state(), State::Blanked);
    }
    #[test]
    fn failed_ack_is_terminal_and_keeps_only_last_completed_preview() {
        let stand = Stand::new();
        let mut c = Controller::new(stand.config(), &stand.store).unwrap();
        let mut t = Fake::default();
        c.poll(&mut t, true, &[]).unwrap();
        let before = c.preview().to_vec();
        t.now = c.next_due_ms().unwrap();
        t.fail_ack = true;
        assert!(c.poll(&mut t, true, &[]).is_err());
        assert!(matches!(c.state(), State::Failed(_)));
        assert_eq!(c.frames_sent(), 1);
        assert_eq!(c.preview(), before);
        let writes = t.writes;
        let acks = t.acks;
        c.suspend();
        c.resume();
        for active in [true, false] {
            assert!(c.poll(&mut t, active, &[]).is_err());
        }
        assert_eq!((t.writes, t.acks), (writes, acks));
    }
    #[test]
    fn ordered_readings_filter_invalid_missing_stale_future_and_ambiguous_samples() {
        let cfg = Config {
            sources: vec![
                OverlaySource {
                    key: "cpu".into(),
                    label: "CPU".into(),
                },
                OverlaySource {
                    key: "gpu".into(),
                    label: "GPU".into(),
                },
                OverlaySource {
                    key: "missing".into(),
                    label: "Missing".into(),
                },
            ],
            ..Config::default()
        };
        let sample = |key: &str, celsius, observed_ms| Sample {
            key: key.into(),
            celsius,
            observed_ms,
        };
        let samples = [sample("gpu", 51., 6000), sample("cpu", 0., 9000)];
        let (oldest, values) = readings(&cfg, &samples, 10000).unwrap();
        assert_eq!(oldest, Some(6000));
        assert_eq!(
            values.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            ["cpu", "gpu"]
        );
        assert_eq!(values[0].celsius, 0.);
        for s in [
            sample("cpu", f64::NAN, 9000),
            sample("cpu", 151., 9000),
            sample("cpu", 40., 10001),
            sample("cpu", 40., 0),
            sample("cpu", 40., 4999),
        ] {
            let (at, values) = readings(&cfg, &[s], 10000).unwrap();
            assert!(at.is_none());
            assert!(values.is_empty());
        }
        assert!(
            readings(
                &cfg,
                &[sample("cpu", 40., 9000), sample("cpu", 41., 9001)],
                10000
            )
            .is_err()
        );
        assert!(readings(&cfg, &[sample(r"HID\local-path", 40., 9000)], 10000).is_err());
    }
    #[test]
    fn strict_configuration_disabled_no_decode_and_unselected_font_no_read() {
        let stand = Stand::new();
        let mut cfg = Config::default();
        cfg.media = Some(AssetId::try_from("0".repeat(64)).unwrap());
        cfg.font = cfg.media.clone();
        let mut c = Controller::new(cfg.clone(), &stand.store).unwrap();
        let mut t = Fake::default();
        assert_eq!(c.state(), State::Disabled);
        assert!(c.poll(&mut t, true, &[]).unwrap().is_none());
        assert_eq!(t.writes, 0);
        let mut enabled = stand.config();
        enabled.font = cfg.font.clone();
        assert!(Controller::new(enabled, &stand.store).is_ok());
        let invalid = stand
            .store
            .put(b"not a media file", media::MAX_SOURCE)
            .unwrap();
        cfg.enabled = true;
        cfg.media = Some(invalid);
        assert!(Controller::new(cfg, &stand.store).is_err());
        for case in 0..8 {
            let mut cfg = Config::default();
            match case {
                0 => cfg.enabled = true,
                1 => cfg.rotation = 45,
                2 => cfg.interval_seconds = 1,
                3 => cfg.overlay_enabled = true,
                4 => {
                    cfg.sources = vec![
                        OverlaySource {
                            key: "cpu".into(),
                            label: "CPU".into()
                        };
                        2
                    ]
                }
                5 => {
                    cfg.sources = vec![OverlaySource {
                        key: r"C:\local".into(),
                        label: "CPU".into(),
                    }]
                }
                6 => {
                    cfg.sources = vec![OverlaySource {
                        key: "cpu".into(),
                        label: "bad\nlabel".into(),
                    }]
                }
                _ => {
                    cfg.sources = (0..33)
                        .map(|i| OverlaySource {
                            key: format!("k{i}"),
                            label: "CPU".into(),
                        })
                        .collect()
                }
            }
            assert!(cfg.validate().is_err());
        }
        let cfg = Config {
            rotation: -90,
            ..Config::default()
        };
        cfg.validate().unwrap();
        let mut json = serde_json::to_value(&cfg).unwrap();
        json["local_path"] = serde_json::json!("PRIVATE");
        assert!(serde_json::from_value::<Config>(json).is_err());
        let mut json = serde_json::to_value(&cfg).unwrap();
        json["media"] = serde_json::json!("A".repeat(64));
        assert!(serde_json::from_value::<Config>(json).is_err());
    }
    #[cfg(windows)]
    #[test]
    fn real_overlay_uses_only_fresh_selected_samples_and_missing_data_has_no_card() {
        let stand = Stand::new();
        let mut cfg = stand.config();
        let font = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("Fonts/seguibl.ttf");
        cfg.font = Some(
            stand
                .store
                .put(&fs::read(font).unwrap(), FONT_LIMIT)
                .unwrap(),
        );
        cfg.overlay_enabled = true;
        cfg.sources = vec![
            OverlaySource {
                key: "cpu".into(),
                label: "CPU".into(),
            },
            OverlaySource {
                key: "gpu".into(),
                label: "GPU".into(),
            },
        ];
        let mut c = Controller::new(cfg, &stand.store).unwrap();
        let mut t = Fake::default();
        let samples = [
            Sample {
                key: "gpu".into(),
                celsius: 50.,
                observed_ms: 1000,
            },
            Sample {
                key: "cpu".into(),
                celsius: 42.,
                observed_ms: 1000,
            },
        ];
        assert!(
            c.poll(&mut t, true, &samples)
                .unwrap()
                .unwrap()
                .overlay_applied
        );
        let writes = t.writes;
        let duplicate = [samples[0].clone(), samples[0].clone()];
        assert!(c.poll(&mut t, true, &duplicate).is_err());
        assert_eq!(t.writes, writes);
        t.now = 7000;
        assert!(
            !c.poll(&mut t, true, &samples)
                .unwrap()
                .unwrap()
                .overlay_applied
        );
        t.now = c.next_due_ms().unwrap();
        assert!(!c.poll(&mut t, true, &[]).unwrap().unwrap().overlay_applied);
        t.now = c.next_due_ms().unwrap();
        let future = [Sample {
            key: "cpu".into(),
            celsius: 42.,
            observed_ms: t.now + 1,
        }];
        assert!(
            !c.poll(&mut t, true, &future)
                .unwrap()
                .unwrap()
                .overlay_applied
        );
    }
    #[cfg(windows)]
    #[test]
    fn logical_ram_mean_reaches_real_overlay_and_expires_without_a_partial_value() {
        use crate::app::sensors::{Config as Sensors, Source, Value};
        let stand = Stand::new();
        let mut cfg = stand.config();
        let font = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("Fonts/seguibl.ttf");
        cfg.font = Some(
            stand
                .store
                .put(&fs::read(font).unwrap(), FONT_LIMIT)
                .unwrap(),
        );
        cfg.overlay_enabled = true;
        cfg.sources = vec![OverlaySource {
            key: "ram".into(),
            label: "RAM".into(),
        }];
        let sensors = Sensors {
            sources: vec![
                Source {
                    key: "ram-a".into(),
                    value: Value::Sensor {
                        input: "slot-a".into(),
                    },
                },
                Source {
                    key: "ram-b".into(),
                    value: Value::Sensor {
                        input: "slot-b".into(),
                    },
                },
                Source {
                    key: "ram".into(),
                    value: Value::Mean {
                        members: vec!["ram-a".into(), "ram-b".into()],
                    },
                },
            ],
        };
        let inputs = [
            Sample {
                key: "slot-a".into(),
                celsius: 40.,
                observed_ms: 1000,
            },
            Sample {
                key: "slot-b".into(),
                celsius: 50.,
                observed_ms: 2000,
            },
        ];
        let mut t = Fake {
            now: 2000,
            ..Fake::default()
        };
        let mut controller = Controller::new(cfg, &stand.store).unwrap();
        let selected = sensors.select(t.now, &inputs).unwrap();
        assert_eq!(selected[2].celsius, 45.);
        assert!(
            controller
                .poll(&mut t, true, &selected)
                .unwrap()
                .unwrap()
                .overlay_applied
        );
        let image = image::load_from_memory(controller.preview()).unwrap();
        assert_eq!((image.width(), image.height()), (480, 480));
        t.now = 6500;
        let selected = sensors.select(t.now, &inputs).unwrap();
        assert!(selected.iter().all(|s| s.key != "ram"));
        assert!(
            !controller
                .poll(&mut t, true, &selected)
                .unwrap()
                .unwrap()
                .overlay_applied
        );
        assert_eq!(controller.frames_sent(), 2);
    }
}
