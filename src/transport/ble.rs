// SPDX-License-Identifier: GPL-2.0-or-later
// Port of door-ble-plugin/BleWorker.{cpp,h}; this module never touches hardware.
use crate::lighting::{color::Rgb, door};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

pub const POLL: Duration = Duration::from_millis(25);
pub const TIMEOUT: Duration = Duration::from_secs(8);
/// Only the audited door protocol commands may cross either transport boundary.
pub fn valid_packet(packet: &[u8]) -> bool {
    match packet.len() {
        21 => packet == door::off_packet(packet[1]),
        95 => {
            packet[..15]
                == [
                    0, packet[1], 0x80, 0, 0, 87, 88, 0x0b, 0xe1, 3, 0, 20, 0, 0, 20,
                ]
                && packet[15..].as_chunks::<4>().0.iter().all(|c| {
                    c[0] == 0xa1 && c[1] <= 179 && c[2] <= 100 && (1..=100).contains(&c[3])
                })
        }
        _ => false,
    }
}
pub trait Control {
    fn now(&self) -> Duration;
    fn cancelled(&self) -> bool;
    fn sleep(&mut self, duration: Duration);
    /// Fakes advance their clock; production wakes immediately on send/stop.
    fn idle(&mut self, _wait: &mut dyn FnMut(Duration)) {
        self.sleep(POLL);
    }
}
pub struct SystemControl {
    start: Instant,
}
impl Default for SystemControl {
    fn default() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}
impl Control for SystemControl {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
    fn cancelled(&self) -> bool {
        false
    }
    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
    fn idle(&mut self, wait: &mut dyn FnMut(Duration)) {
        wait(Duration::from_millis(250));
    }
}
/// Each asynchronous operation gets its own eight-second budget, as in bounded().
pub fn check_wait(control: &dyn Control, started: Duration) -> Result<(), String> {
    if control.cancelled() || control.now().saturating_sub(started) >= TIMEOUT {
        Err("Operation cancelled or timed out; disconnected".into())
    } else {
        Ok(())
    }
}
pub fn validate_service(success: bool, count: u32) -> Result<(), String> {
    if success && count == 1 {
        Ok(())
    } else {
        Err("FFFF service unavailable or ambiguous".into())
    }
}
pub fn validate_characteristic(
    success: bool,
    count: u32,
    write_without_response: bool,
) -> Result<(), String> {
    if !success || count != 1 {
        return Err("FF01 characteristic unavailable or ambiguous".into());
    }
    if !write_without_response {
        return Err("FF01 has no write-without-response support".into());
    }
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Status(String),
    Connected { max_write: u32 },
    Sent,
}
pub trait Backend {
    type Link: Link;
    fn connect(
        &mut self,
        target: u64,
        control: &mut dyn Control,
        emit: &mut dyn FnMut(Event),
    ) -> Result<Self::Link, String>;
}
/// Drop must close owned resources. Writes must be one complete WriteWithoutResponse operation.
pub trait Link {
    fn address(&self) -> Result<u64, String>;
    fn connected(&self) -> Result<bool, String>;
    fn max_pdu(&self) -> Result<u32, String>;
    fn write(&mut self, packet: &[u8], control: &mut dyn Control) -> Result<(), String>;
}
#[derive(Default)]
struct State {
    running: bool,
    pending: Option<Vec<u8>>,
}
#[derive(Clone)]
pub struct Sender {
    state: Arc<Mutex<State>>,
    wake: Arc<Condvar>,
    stop: Arc<AtomicBool>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submission {
    Accepted,
    Busy,
    NotRunning,
    InvalidPacket,
}
impl Sender {
    /// BleWorker::send is FIRST pending wins: never overwrite or queue a second pending packet.
    pub fn send(&self, packet: Vec<u8>) -> Submission {
        if !valid_packet(&packet) {
            return Submission::InvalidPacket;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.running || self.stop.load(Ordering::Acquire) {
            return Submission::NotRunning;
        }
        if state.pending.is_some() {
            return Submission::Busy;
        }
        state.pending = Some(packet);
        self.wake.notify_one();
        Submission::Accepted
    }
    pub fn frame(
        &self,
        colors: &[Rgb; door::COLOR_GROUPS],
        brightness: i32,
        sequence: u8,
    ) -> Result<Submission, String> {
        Ok(self.send(door::frame_packet(colors, brightness, sequence).map_err(str::to_owned)?))
    }
    pub fn off(&self, sequence: u8) -> Submission {
        self.send(door::off_packet(sequence))
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.state.lock().unwrap_or_else(|e| e.into_inner()).pending = None;
        self.wake.notify_all();
    }
    pub fn is_running(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).running
    }
}
pub struct Worker<B: Backend> {
    backend: B,
    target: u64,
    sender: Sender,
}
impl<B: Backend> Worker<B> {
    pub fn new(backend: B, target: u64) -> Result<(Self, Sender), String> {
        if !door::allowed(target) {
            return Err("Target not allowed".into());
        }
        let sender = Sender {
            state: Arc::new(Mutex::new(State::default())),
            wake: Arc::new(Condvar::new()),
            stop: Arc::new(AtomicBool::new(false)),
        };
        Ok((
            Self {
                backend,
                target,
                sender: sender.clone(),
            },
            sender,
        ))
    }
    pub fn set_target(&mut self, target: u64) -> bool {
        if self.sender.is_running()
            || self.sender.stop.load(Ordering::Acquire)
            || !door::allowed(target)
        {
            return false;
        }
        self.target = target;
        true
    }
    /// Single-use blocking entrypoint. After stop/exit create a new Worker to reconnect.
    /// Caller chooses thread; no threads or connections start at construction.
    pub fn run(
        &mut self,
        control: &mut dyn Control,
        emit: &mut dyn FnMut(Event),
    ) -> Result<(), String> {
        {
            let mut state = self.sender.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.running {
                return Err("BLE worker already running".into());
            }
            if self.sender.stop.load(Ordering::Acquire) {
                return Err("BLE worker stopped; create a new worker to reconnect".into());
            }
            state.running = true;
        }
        let _finish = Finish(self.sender.clone());
        let mut control = WorkerControl {
            inner: control,
            stop: self.sender.stop.clone(),
        };
        let result = (|| {
            if !door::allowed(self.target) {
                return Err("Target not allowed".into());
            }
            if control.cancelled() {
                return Err("Operation cancelled or timed out; disconnected".into());
            }
            let mut link = self.backend.connect(self.target, &mut control, emit)?;
            if link.address()? != self.target {
                return Err("Fixed target unavailable after cache lookup".into());
            }
            emit(Event::Connected {
                max_write: link.max_pdu()?.saturating_sub(3),
            });
            while !control.cancelled() {
                if !link.connected()? {
                    return Err("BLE link lost; disconnected".into());
                }
                let packet = self
                    .sender
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pending
                    .take();
                let Some(packet) = packet else {
                    control.idle(&mut |duration| {
                        let state = self.sender.state.lock().unwrap_or_else(|e| e.into_inner());
                        if state.pending.is_none() && !self.sender.stop.load(Ordering::Acquire) {
                            let _guard = self
                                .sender
                                .wake
                                .wait_timeout(state, duration.min(Duration::from_millis(250)))
                                .unwrap_or_else(|e| e.into_inner());
                        }
                    });
                    continue;
                };
                if control.cancelled() {
                    break;
                }
                if !link.connected()? {
                    return Err("BLE link lost; disconnected".into());
                }
                if !door::fits(packet.len(), link.max_pdu()?) {
                    return Err("Packet exceeds negotiated ATT payload; no fragments sent".into());
                }
                link.write(&packet, &mut control)?;
                emit(Event::Sent);
            }
            Ok(())
        })();
        emit(Event::Status(
            result
                .as_ref()
                .err()
                .cloned()
                .unwrap_or_else(|| "Disconnected".into()),
        ));
        result
    }
}
struct Finish(Sender);
impl Drop for Finish {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::Release);
        let mut s = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        s.running = false;
        s.pending = None;
    }
}
struct WorkerControl<'a> {
    inner: &'a mut dyn Control,
    stop: Arc<AtomicBool>,
}
impl Control for WorkerControl<'_> {
    fn now(&self) -> Duration {
        self.inner.now()
    }
    fn cancelled(&self) -> bool {
        self.stop.load(Ordering::Acquire) || self.inner.cancelled()
    }
    fn sleep(&mut self, duration: Duration) {
        self.inner.sleep(duration);
    }
    fn idle(&mut self, wait: &mut dyn FnMut(Duration)) {
        self.inner.idle(wait);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Clock {
        time: Duration,
        cancel_at: Duration,
    }
    impl Control for Clock {
        fn now(&self) -> Duration {
            self.time
        }
        fn cancelled(&self) -> bool {
            self.time >= self.cancel_at
        }
        fn sleep(&mut self, d: Duration) {
            assert!(d <= POLL);
            self.time += d;
        }
    }
    struct Fake {
        address: u64,
        pdu: u32,
        live: bool,
        fail: bool,
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
        dropped: Arc<AtomicBool>,
        sender: Option<Sender>,
        connects: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Backend for Fake {
        type Link = Self;
        fn connect(
            &mut self,
            _: u64,
            _: &mut dyn Control,
            _: &mut dyn FnMut(Event),
        ) -> Result<Self, String> {
            self.connects.fetch_add(1, Ordering::Relaxed);
            Ok(Self {
                address: self.address,
                pdu: self.pdu,
                live: self.live,
                fail: self.fail,
                writes: self.writes.clone(),
                dropped: self.dropped.clone(),
                sender: self.sender.clone(),
                connects: self.connects.clone(),
            })
        }
    }
    impl Link for Fake {
        fn address(&self) -> Result<u64, String> {
            Ok(self.address)
        }
        fn max_pdu(&self) -> Result<u32, String> {
            Ok(self.pdu)
        }
        fn connected(&self) -> Result<bool, String> {
            Ok(self.live)
        }
        fn write(&mut self, p: &[u8], c: &mut dyn Control) -> Result<(), String> {
            self.writes.lock().unwrap().push(p.to_vec());
            if self.fail {
                return Err("GATT write failed; disconnected".into());
            }
            if let Some(s) = &self.sender {
                assert_eq!(s.off(3), Submission::Accepted);
                assert_eq!(s.off(4), Submission::Busy);
            }
            c.sleep(POLL);
            Ok(())
        }
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }
    type Fixture = (
        Worker<Fake>,
        Sender,
        Arc<Mutex<Vec<Vec<u8>>>>,
        Arc<AtomicBool>,
    );
    fn fixture(pdu: u32, live: bool, fail: bool) -> Fixture {
        let writes = Arc::new(Mutex::new(vec![]));
        let dropped = Arc::new(AtomicBool::new(false));
        let (w, s) = Worker::new(
            Fake {
                address: 1,
                pdu,
                live,
                fail,
                writes: writes.clone(),
                dropped: dropped.clone(),
                sender: None,
                connects: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            },
            1,
        )
        .unwrap();
        (w, s, writes, dropped)
    }
    #[test]
    fn production_idle_wakes_on_submission_and_stop_with_fake_link() {
        let (mut worker, sender, writes, dropped) = fixture(98, true, false);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let stop = sender.clone();
        let thread = std::thread::spawn(move || {
            let result = worker.run(&mut SystemControl::default(), &mut |event| match event {
                Event::Connected { .. } => {
                    let _ = ready_tx.send(());
                }
                Event::Sent => stop.stop(),
                _ => {}
            });
            let _ = done_tx.send(result);
        });
        let ready = ready_rx.recv_timeout(Duration::from_secs(2));
        if ready.is_ok() {
            assert_eq!(sender.off(17), Submission::Accepted);
        }
        let done = done_rx.recv_timeout(Duration::from_secs(2));
        sender.stop();
        thread.join().unwrap();
        ready.unwrap();
        done.unwrap().unwrap();
        assert_eq!(*writes.lock().unwrap(), [door::off_packet(17)]);
        assert!(dropped.load(Ordering::Acquire));
    }
    #[test]
    fn pending_is_first_not_latest_and_inflight_allows_one_next() {
        let (mut w, s, writes, dropped) = fixture(98, true, false);
        w.backend.sender = Some(s.clone());
        assert_eq!(s.off(0), Submission::NotRunning);
        let mut c = Clock {
            time: Duration::ZERO,
            cancel_at: POLL * 2,
        };
        w.run(&mut c, &mut |e| {
            if matches!(e, Event::Connected { .. }) {
                assert_eq!(s.off(1), Submission::Accepted);
                assert_eq!(s.off(2), Submission::Busy);
            }
        })
        .unwrap();
        let packets = writes.lock().unwrap();
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0], door::off_packet(1));
        assert_eq!(packets[1], door::off_packet(3));
        assert!(dropped.load(Ordering::Acquire));
        assert!(!s.is_running());
        assert_eq!(s.off(5), Submission::NotRunning);
    }
    #[test]
    fn pdu_error_never_fragments_and_is_terminal() {
        let (mut w, s, writes, dropped) = fixture(97, true, false);
        let mut c = Clock {
            time: Duration::ZERO,
            cancel_at: TIMEOUT,
        };
        let e = w
            .run(&mut c, &mut |e| {
                if matches!(e, Event::Connected { .. }) {
                    assert_eq!(
                        s.frame(&[Rgb::new(1, 2, 3); door::COLOR_GROUPS], 10, 1)
                            .unwrap(),
                        Submission::Accepted
                    );
                }
            })
            .unwrap_err();
        assert!(e.contains("no fragments"));
        assert!(writes.lock().unwrap().is_empty());
        assert!(dropped.load(Ordering::Acquire));
        assert!(!s.is_running());
    }
    #[test]
    fn full_frame_at_exact_pdu_boundary_is_one_write() {
        let (mut w, s, writes, _) = fixture(98, true, false);
        let mut c = Clock {
            time: Duration::ZERO,
            cancel_at: POLL,
        };
        let colors = [Rgb::new(12, 34, 56); door::COLOR_GROUPS];
        w.run(&mut c, &mut |e| {
            if matches!(e, Event::Connected { .. }) {
                assert_eq!(s.frame(&colors, 10, 7).unwrap(), Submission::Accepted);
            }
        })
        .unwrap();
        assert_eq!(
            *writes.lock().unwrap(),
            vec![door::frame_packet(&colors, 10, 7).unwrap()]
        );
        assert_eq!(s.send(vec![0; 96]), Submission::InvalidPacket);
    }
    #[test]
    fn loss_write_error_stop_and_exact_target() {
        for (live, fail) in [(false, false), (true, true)] {
            let (mut w, s, writes, _) = fixture(98, live, fail);
            let mut c = Clock {
                time: Duration::ZERO,
                cancel_at: TIMEOUT,
            };
            assert!(
                w.run(&mut c, &mut |e| if matches!(e, Event::Connected { .. }) {
                    s.off(1);
                })
                .is_err()
            );
            assert!(writes.lock().unwrap().len() <= 1);
            assert!(!s.is_running());
        }
        let (mut w, s, writes, _) = fixture(98, true, false);
        let mut c = Clock {
            time: Duration::ZERO,
            cancel_at: TIMEOUT,
        };
        w.run(&mut c, &mut |e| {
            if matches!(e, Event::Connected { .. }) {
                s.off(1);
                s.stop();
            }
        })
        .unwrap();
        assert!(writes.lock().unwrap().is_empty());
        let (mut w, _, _, _) = fixture(98, true, false);
        w.backend.address = 2;
        assert!(w.run(&mut c, &mut |_| {}).is_err());
    }
    #[test]
    fn stop_before_run_is_terminal_and_never_connects() {
        let (mut w, s, writes, _) = fixture(98, true, false);
        let connects = w.backend.connects.clone();
        let mut c = Clock {
            time: Duration::ZERO,
            cancel_at: TIMEOUT,
        };
        s.stop();
        for _ in 0..2 {
            assert!(w.run(&mut c, &mut |_| {}).is_err());
        }
        assert_eq!(connects.load(Ordering::Relaxed), 0);
        assert!(writes.lock().unwrap().is_empty());
        assert!(!w.set_target(2));
        assert_eq!(s.off(1), Submission::NotRunning);
    }
    #[test]
    fn malformed_commands_cannot_enter_pending_slot() {
        let packet = door::frame_packet(&[Rgb::new(1, 2, 3); door::COLOR_GROUPS], 10, 1).unwrap();
        assert!(valid_packet(&packet));
        for (index, value) in [
            (0, 1),
            (2, 0),
            (11, 19),
            (15, 0),
            (16, 180),
            (17, 101),
            (18, 0),
            (18, 101),
            (94, 0),
        ] {
            let mut bad = packet.clone();
            bad[index] = value;
            assert!(!valid_packet(&bad));
        }
        let mut off = door::off_packet(1);
        off[8] ^= 1;
        assert!(!valid_packet(&off));
        let (mut w, s, writes, _) = fixture(98, true, false);
        let mut c = Clock {
            time: Duration::ZERO,
            cancel_at: POLL,
        };
        w.run(&mut c, &mut |e| {
            if matches!(e, Event::Connected { .. }) {
                assert_eq!(s.send(vec![0; 95]), Submission::InvalidPacket);
                assert_eq!(s.send(off.clone()), Submission::InvalidPacket);
                assert_eq!(s.off(9), Submission::Accepted);
            }
        })
        .unwrap();
        assert_eq!(*writes.lock().unwrap(), vec![door::off_packet(9)]);
    }
    #[test]
    fn discovery_and_budget_guards() {
        for n in [0, 2] {
            assert!(validate_service(true, n).is_err());
            assert!(validate_characteristic(true, n, true).is_err());
        }
        assert!(validate_service(false, 1).is_err());
        assert!(validate_characteristic(true, 1, false).is_err());
        assert!(validate_characteristic(true, 1, true).is_ok());
        let mut c = Clock {
            time: Duration::ZERO,
            cancel_at: Duration::MAX,
        };
        for _ in 0..320 {
            assert!(check_wait(&c, Duration::ZERO).is_ok());
            c.sleep(POLL);
        }
        assert!(check_wait(&c, Duration::ZERO).is_err());
        assert!(Worker::new(fixture(98, true, false).0.backend, 0).is_err());
    }
}
