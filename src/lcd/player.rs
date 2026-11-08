//! Worker-owned timed playback. The host supplies an owned transport and monotonic clock.
//! No sleeps, OS power calls or HID discovery occur here.
use super::{
    compositor::Compositor,
    media::{self, Media},
    overlay::Renderer,
    protocol::{ReportTransport, Sender},
};
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};

const STOP: u8 = 1;
const BLANK: u8 = 2;
const CLOSED: u8 = 128;
#[derive(Clone)]
pub struct Requests(Arc<AtomicU8>);
impl Default for Requests {
    fn default() -> Self {
        Self(Arc::new(AtomicU8::new(CLOSED)))
    }
}
impl Requests {
    fn open() -> Self {
        Self(Arc::new(AtomicU8::new(0)))
    }
    fn request(&self, flag: u8) {
        let mut state = self.0.load(Ordering::Acquire);
        while state & CLOSED == 0 {
            match self.0.compare_exchange_weak(
                state,
                state | flag,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => state = current,
            }
        }
    }
    fn close(&self) {
        self.0.store(CLOSED, Ordering::Release);
    }
    pub fn stop(&self) {
        self.request(STOP);
    }
    pub fn blank(&self) {
        self.request(BLANK);
    }
    pub fn pending(&self) -> bool {
        self.0.load(Ordering::Acquire) & (STOP | BLANK) != 0
    }
}
#[derive(Clone)]
pub struct OverlayValue {
    pub id: String,
    pub label: String,
    pub celsius: f64,
}
#[derive(Clone)]
pub struct Overlay {
    enabled: bool,
    interval_seconds: u8,
    observed_ms: Option<u64>,
    readings: Vec<OverlayValue>,
}
impl Default for Overlay {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_seconds: 5,
            observed_ms: None,
            readings: Vec::new(),
        }
    }
}
impl Overlay {
    pub fn new(
        enabled: bool,
        interval_seconds: u8,
        observed_ms: Option<u64>,
        readings: Vec<OverlayValue>,
    ) -> Result<Self, String> {
        if !(2..=60).contains(&interval_seconds) || readings.len() > 4096 {
            return Err("Invalid LCD overlay configuration".into());
        }
        let mut ids = HashSet::new();
        for value in &readings {
            if value.id.is_empty()
                || value.id.len() > 4096
                || !ids.insert(&value.id)
                || value.label.len() > 16384
                || value.label.trim().is_empty()
                || !value.celsius.is_finite()
                || !(-20.0..=150.0).contains(&value.celsius)
            {
                return Err("Invalid or ambiguous LCD overlay reading".into());
            }
        }
        Ok(Self {
            enabled,
            interval_seconds,
            observed_ms,
            readings,
        })
    }
    fn selected(&self, now_ms: u64, elapsed: u64) -> Option<&OverlayValue> {
        let received = self.observed_ms?;
        if !self.enabled
            || self.readings.is_empty()
            || received == 0
            || now_ms < received
            || now_ms - received > 5000
        {
            return None;
        }
        let index =
            (elapsed / (u64::from(self.interval_seconds) * 1000)) % self.readings.len() as u64;
        self.readings.get(index as usize)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Stopped,
    Running,
    Blanked,
    Failed(String),
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    pub overlay_frames: u64,
    pub overlay_failures: u64,
    pub overruns: u64,
    pub work_max_ms: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sent {
    pub frame_index: Option<usize>,
    pub overlay_applied: bool,
}
pub struct Player {
    media: Arc<Media>,
    compositor: Compositor,
    sender: Sender,
    requests: Requests,
    state: State,
    started_ms: Option<u64>,
    last_seen_ms: Option<u64>,
    next_due: Option<u64>,
    frame_index: usize,
    source_aligned: bool,
    blank_pending: bool,
    counters: Counters,
}
impl Player {
    pub fn new(media: Arc<Media>, renderer: Renderer) -> Result<Self, String> {
        Self::configured(media, Some(renderer))
    }
    /// GIF-only playback does not need or load a font.
    pub fn without_overlay(media: Arc<Media>) -> Result<Self, String> {
        Self::configured(media, None)
    }
    fn configured(media: Arc<Media>, renderer: Option<Renderer>) -> Result<Self, String> {
        if media.frames.is_empty() || media.frames.len() > media::MAX_FRAMES {
            return Err("No validated LCD media".into());
        }
        for frame in &media.frames {
            super::protocol::validate_jpeg(&frame.jpeg)?;
            if !(50..=10000).contains(&frame.delay_ms) {
                return Err("Invalid LCD frame delay".into());
            }
        }
        Ok(Self {
            compositor: match renderer {
                Some(renderer) => Compositor::new(media.clone(), renderer),
                None => Compositor::without_overlay(media.clone()),
            },
            media,
            sender: Sender::default(),
            requests: Requests::default(),
            state: State::Stopped,
            started_ms: None,
            last_seen_ms: None,
            next_due: None,
            frame_index: 0,
            source_aligned: true,
            blank_pending: false,
            counters: Counters::default(),
        })
    }
    pub fn state(&self) -> &State {
        &self.state
    }
    pub fn requests(&self) -> Requests {
        self.requests.clone()
    }
    pub fn counters(&self) -> Counters {
        self.counters
    }
    pub fn frames_sent(&self) -> u64 {
        self.sender.frames_sent()
    }
    pub fn preview(&self) -> &[u8] {
        self.sender.latest_frame()
    }
    pub fn next_due_ms(&self) -> Option<u64> {
        if self.blank_pending {
            self.last_seen_ms
        } else {
            self.next_due
        }
    }
    /// Explicit start/resume. A failed transport requires a new player after revalidation.
    /// Counters are per run; the bounded compositor cache is retained across clean resume.
    pub fn start(&mut self) -> Result<(), String> {
        if matches!(self.state, State::Failed(_)) {
            return Err("Revalidate and reopen the LCD transport after failure".into());
        }
        if self.state == State::Running {
            return Ok(());
        }
        self.sender = Sender::default();
        // Old callbacks retain a closed handle and cannot stop a later run.
        self.requests.close();
        self.requests = Requests::open();
        self.state = State::Running;
        self.started_ms = None;
        self.last_seen_ms = None;
        self.next_due = None;
        self.frame_index = 0;
        self.source_aligned = true;
        self.blank_pending = false;
        self.counters = Counters::default();
        self.compositor.begin_loop();
        Ok(())
    }
    fn process_requests(&mut self) {
        let flags = self.requests.0.swap(0, Ordering::AcqRel);
        if flags & STOP != 0 {
            self.state = State::Stopped;
            self.blank_pending = false;
            self.next_due = None;
            self.requests.close();
            self.compositor.begin_loop();
        } else if flags & BLANK != 0 {
            self.blank_pending = true;
        }
    }
    fn failure<T>(&mut self, error: impl Into<String>) -> Result<T, String> {
        let error = error.into();
        self.state = State::Failed(error.clone());
        self.next_due = None;
        self.requests.close();
        self.compositor.begin_loop();
        Err(error)
    }
    /// At most one frame per poll; late wakeups do not send catch-up bursts.
    /// Requests arriving during composition/I/O take effect after that frame's ACK.
    pub fn poll<T: ReportTransport>(
        &mut self,
        overlay: &Overlay,
        transport: &mut T,
    ) -> Result<Option<Sent>, String> {
        if self.state != State::Running {
            return Ok(None);
        }
        self.process_requests();
        if self.state != State::Running {
            return Ok(None);
        }
        let now = transport.now_ms();
        if self.last_seen_ms.is_some_and(|last| now < last) {
            return self.failure("Invalid monotonic LCD clock");
        }
        self.last_seen_ms = Some(now);
        let started = *self.started_ms.get_or_insert(now);
        if !self.blank_pending && self.next_due.is_some_and(|due| now < due) {
            return Ok(None);
        }
        if self.blank_pending {
            let blank = match media::encode_jpeg(&image::RgbImage::new(480, 480)) {
                Ok(jpeg) => jpeg,
                Err(error) => return self.failure(error),
            };
            if let Err(error) = self.sender.send_frame(&blank, true, now, transport) {
                return self.failure(error);
            }
            let pending = self.requests.0.swap(CLOSED, Ordering::AcqRel);
            self.state = if pending & STOP != 0 {
                State::Stopped
            } else {
                State::Blanked
            };
            self.blank_pending = false;
            self.next_due = None;
            self.compositor.begin_loop();
            self.last_seen_ms = Some(transport.now_ms());
            return Ok(Some(Sent {
                frame_index: None,
                overlay_applied: false,
            }));
        }
        let selected = overlay.selected(now, now - started);
        if selected.is_some() && !self.source_aligned {
            self.frame_index = 0;
            self.source_aligned = true;
            self.compositor.begin_loop();
        }
        let index = self.frame_index;
        let frame = &self.media.frames[index];
        let jpeg = frame.jpeg.clone();
        let Some(due) = now.checked_add(u64::from(frame.delay_ms)) else {
            return self.failure("LCD deadline overflow");
        };
        let delay = frame.delay_ms;
        let mut applied = false;
        let jpeg = if let Some(value) = selected {
            match self.compositor.compose(index, &value.label, value.celsius) {
                Ok(output) => {
                    applied = output.applied;
                    if !applied {
                        self.counters.overlay_failures += 1;
                    }
                    output.jpeg
                }
                Err(_) => {
                    self.counters.overlay_failures += 1;
                    jpeg
                }
            }
        } else {
            // Inactive/stale overlays cannot use the decoder until a fresh loop.
            // Drop its large working buffers now, even for very long animations.
            if self.source_aligned {
                self.compositor.begin_loop();
            }
            self.source_aligned = false;
            jpeg
        };
        if let Err(error) = self.sender.send_frame(&jpeg, false, now, transport) {
            return self.failure(error);
        }
        if applied {
            self.counters.overlay_frames += 1;
        }
        let finished = transport.now_ms();
        let Some(work_ms) = finished.checked_sub(now) else {
            return self.failure("Invalid monotonic LCD completion clock");
        };
        self.last_seen_ms = Some(finished);
        self.counters.work_max_ms = self.counters.work_max_ms.max(work_ms);
        if work_ms > u64::from(delay) {
            self.counters.overruns += 1;
        }
        self.frame_index += 1;
        if self.frame_index == self.media.frames.len() {
            self.frame_index = 0;
            self.source_aligned = true;
            self.compositor.begin_loop();
        }
        self.next_due = Some(due);
        self.process_requests();
        Ok(Some(Sent {
            frame_index: Some(index),
            overlay_applied: applied,
        }))
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::super::protocol::{ACK_BYTES, REPORT_BYTES};
    use super::*;
    struct Fake {
        now: u64,
        step: u64,
        writes: u64,
        acks: u64,
        fail: bool,
        on_ack: Option<Requests>,
    }
    impl Default for Fake {
        fn default() -> Self {
            Self {
                now: 1000,
                step: 1,
                writes: 0,
                acks: 0,
                fail: false,
                on_ack: None,
            }
        }
    }
    impl ReportTransport for Fake {
        fn now_ms(&self) -> u64 {
            self.now
        }
        fn write_report(&mut self, _: &[u8; REPORT_BYTES], _: u64) -> Result<usize, String> {
            self.now += self.step;
            self.writes += 1;
            Ok(REPORT_BYTES)
        }
        fn read_ack(&mut self, response: &mut [u8; ACK_BYTES], _: u64) -> Result<usize, String> {
            self.now += self.step;
            self.acks += 1;
            if self.fail {
                return Err("Injected ACK failure".into());
            }
            if let Some(request) = self.on_ack.take() {
                request.stop();
            }
            response.fill(0);
            Ok(ACK_BYTES)
        }
    }
    fn player() -> Player {
        let bytes: Arc<[u8]> = Arc::from(include_bytes!("fixtures/two-frame.apng").as_slice());
        let media = Arc::new(media::prepare(bytes, 0, || false).unwrap());
        let font = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("Fonts/seguibl.ttf");
        Player::new(media, Renderer::new(std::fs::read(font).unwrap()).unwrap()).unwrap()
    }
    fn overlay(at: u64) -> Overlay {
        Overlay::new(
            true,
            2,
            Some(at),
            ["CPU", "GPU"]
                .into_iter()
                .map(|label| OverlayValue {
                    id: label.into(),
                    label: label.into(),
                    celsius: 42.,
                })
                .collect(),
        )
        .unwrap()
    }
    #[test]
    fn cadence_overrun_no_catchup_and_clean_resume() {
        let mut p = player();
        let mut t = Fake::default();
        p.start().unwrap();
        assert_eq!(
            p.poll(&Overlay::default(), &mut t)
                .unwrap()
                .unwrap()
                .frame_index,
            Some(0)
        );
        assert!(p.poll(&Overlay::default(), &mut t).unwrap().is_none());
        t.now = 1100;
        assert_eq!(
            p.poll(&Overlay::default(), &mut t)
                .unwrap()
                .unwrap()
                .frame_index,
            Some(1)
        );
        t.now = 10000;
        assert!(p.poll(&Overlay::default(), &mut t).unwrap().is_some());
        assert!(p.poll(&Overlay::default(), &mut t).unwrap().is_none());
        p.requests().stop();
        let writes = t.writes;
        assert!(p.poll(&Overlay::default(), &mut t).unwrap().is_none());
        assert_eq!(t.writes, writes);
        assert_eq!(p.state(), &State::Stopped);
        p.start().unwrap();
        assert_eq!(p.frames_sent(), 0);
        t.step = 60;
        p.poll(&Overlay::default(), &mut t).unwrap();
        assert_eq!(p.counters().overruns, 1);
    }
    #[test]
    fn stale_overlay_recovers_at_loop_start_and_future_data_is_not_fresh() {
        let mut p = player();
        let mut t = Fake::default();
        p.start().unwrap();
        assert!(
            !p.poll(&overlay(2000), &mut t)
                .unwrap()
                .unwrap()
                .overlay_applied
        );
        t.now = 1100;
        let output = p.poll(&overlay(1100), &mut t).unwrap().unwrap();
        assert!(output.overlay_applied);
        assert_eq!(output.frame_index, Some(0));
        t.now = 7000;
        assert!(
            !p.poll(&overlay(1100), &mut t)
                .unwrap()
                .unwrap()
                .overlay_applied
        );
        assert!(Overlay::new(true, 0, Some(1000), Vec::new()).is_err());
    }
    #[test]
    fn stop_during_ack_drains_frame_and_blank_is_not_a_fake_software_flag() {
        let mut p = player();
        let mut t = Fake::default();
        p.start().unwrap();
        t.on_ack = Some(p.requests());
        assert!(p.poll(&Overlay::default(), &mut t).unwrap().is_some());
        assert_eq!(p.frames_sent(), 1);
        assert_eq!(p.state(), &State::Stopped);
        p.start().unwrap();
        p.requests().blank();
        let output = p.poll(&Overlay::default(), &mut t).unwrap().unwrap();
        assert_eq!(output.frame_index, None);
        assert_eq!(p.state(), &State::Blanked);
        assert!(
            image::load_from_memory(p.preview())
                .unwrap()
                .into_rgb8()
                .pixels()
                .all(|pixel| pixel.0 == [0, 0, 0])
        );
        let writes = t.writes;
        assert!(p.poll(&Overlay::default(), &mut t).unwrap().is_none());
        assert_eq!(t.writes, writes);
    }
    #[test]
    fn failed_ack_latches_failure_without_publishing_frame_or_blind_resume() {
        let mut p = player();
        let mut t = Fake {
            fail: true,
            ..Fake::default()
        };
        p.start().unwrap();
        p.requests().blank();
        assert!(p.poll(&Overlay::default(), &mut t).is_err());
        assert_eq!(p.frames_sent(), 0);
        assert!(p.preview().is_empty());
        assert!(matches!(p.state(), State::Failed(_)));
        assert!(p.start().is_err());
        let writes = t.writes;
        assert!(p.poll(&Overlay::default(), &mut t).unwrap().is_none());
        assert_eq!(writes, t.writes);
    }
    #[test]
    fn stale_requests_do_not_cross_runs_and_close_after_blank_ack() {
        let mut p = player();
        let mut t = Fake::default();
        p.start().unwrap();
        let old = p.requests();
        old.stop();
        p.poll(&Overlay::default(), &mut t).unwrap();
        assert!(!old.pending());
        p.start().unwrap();
        old.stop();
        old.blank();
        assert!(!old.pending());
        assert!(p.poll(&Overlay::default(), &mut t).unwrap().is_some());
        assert_eq!(p.state(), &State::Running);
        let current = p.requests();
        current.blank();
        t.on_ack = Some(current.clone());
        p.poll(&Overlay::default(), &mut t).unwrap();
        assert_eq!(p.state(), &State::Stopped);
        assert!(!current.pending());
        assert!(p.sender.blank_acked());
    }
    #[test]
    fn carousel_uses_elapsed_time_and_never_treats_future_receive_as_fresh() {
        let values = overlay(1000);
        assert_eq!(values.selected(1000, 0).unwrap().label, "CPU");
        assert_eq!(values.selected(3000, 2000).unwrap().label, "GPU");
        assert_eq!(values.selected(5000, 4000).unwrap().label, "CPU");
        assert!(values.selected(999, 0).is_none());
        assert!(values.selected(6001, 5001).is_none());
    }
    #[test]
    fn backwards_clock_does_not_write_another_frame() {
        let mut p = player();
        let mut t = Fake::default();
        p.start().unwrap();
        p.poll(&Overlay::default(), &mut t).unwrap();
        let writes = t.writes;
        t.now = 999;
        assert!(p.poll(&Overlay::default(), &mut t).is_err());
        assert_eq!(t.writes, writes);
        assert_eq!(p.frames_sent(), 1);
        assert!(matches!(p.state(), State::Failed(_)));
    }
}
