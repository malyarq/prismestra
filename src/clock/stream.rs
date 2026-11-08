//! Conservative observation-age bridge for authenticated IPC on one Windows system/boot.
//! QPC is comparable across processes; frequency is boot-fixed, not a boot ID or authority.
//! Microsoft documents a +/-1 counter-tick cross-thread ordering uncertainty. Windows Rust
//! Instant currently uses QPC. Integer ceil conversion plus one ms/tick margins only ages data.
//! Original local stamps must be sampled with this crate's monotonic_ms; no wall clocks.
//!
//! Sender captures common BEFORE local; receiver captures local BEFORE common. Scheduling
//! gaps make results older. Each source/generation needs its own Publisher/Receiver: equal
//! original stamps return their first mapping exactly, including after transport/cache reuse.
//! Errors never clip to zero/now or mint a replacement deadline. Consumer TTL remains its own
//! policy; this bridge accepts only 1..=2000ms. Cross-machine/reboot/authentication is external.
//!
//! Evidence: https://learn.microsoft.com/en-us/windows/win32/sysinfo/acquiring-high-resolution-time-stamps
//! https://learn.microsoft.com/en-us/windows/win32/api/profileapi/nf-profileapi-queryperformancefrequency
//! https://doc.rust-lang.org/std/time/struct.Instant.html#underlying-system-calls
use serde::{Deserialize, Serialize};
use std::fmt;

pub const MAX_AGE_MS: u64 = 2000;
// Explicit representation/product bound, not a claim about all future QPC implementations.
pub const MAX_FREQUENCY: u64 = 1_000_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    pub counter: u64,
    pub frequency: u64,
}
impl Stamp {
    /// Conservative time since a same-boot QPC stamp; includes cross-thread tick/rounding margins.
    /// Unlike observation mapping this can report an already expired diagnostic packet.
    pub fn age_ms_at(self, now: Self) -> Result<u64, Error> {
        self.validate()?;
        now.validate()?;
        if self.frequency != now.frequency {
            return Err(Error::DomainMismatch);
        }
        let ticks = now
            .counter
            .checked_sub(self.counter)
            .ok_or(Error::Future)?
            .checked_add(1)
            .ok_or(Error::Overflow)?;
        ceil_product(ticks, 1000, self.frequency)?
            .checked_add(1)
            .ok_or(Error::Overflow)
    }
    pub fn validate(self) -> Result<(), Error> {
        if self.counter == 0
            || self.counter > i64::MAX as u64
            || self.frequency == 0
            || self.frequency > MAX_FREQUENCY
        {
            return Err(Error::InvalidStamp);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unavailable,
    InvalidStamp,
    InvalidLocal,
    InvalidMaxAge,
    DomainMismatch,
    ClockRegressed,
    ObservationRegressed,
    Future,
    Expired,
    Underflow,
    Overflow,
}
impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "Common QPC clock unavailable",
            Self::InvalidStamp => "Invalid common-domain counter/frequency",
            Self::InvalidLocal => "Invalid process-local timestamp",
            Self::InvalidMaxAge => "Stream maximum age must be 1..2000 ms",
            Self::DomainMismatch => "Common clock frequency/domain mismatch",
            Self::ClockRegressed => "Stream clock pair regressed",
            Self::ObservationRegressed => "Original observation timestamp regressed",
            Self::Future => "Original observation is in the future",
            Self::Expired => "Original observation has expired",
            Self::Underflow => "Observation predates a representable local/common epoch",
            Self::Overflow => "Clock conversion overflow",
        })
    }
}
impl std::error::Error for Error {}
fn local(value: u64) -> Result<(), Error> {
    if value == 0 || value > i64::MAX as u64 {
        Err(Error::InvalidLocal)
    } else {
        Ok(())
    }
}
fn maximum(age: u64) -> Result<(), Error> {
    if (1..=MAX_AGE_MS).contains(&age) {
        Ok(())
    } else {
        Err(Error::InvalidMaxAge)
    }
}
fn ceil_product(value: u64, multiplier: u64, divisor: u64) -> Result<u64, Error> {
    let value = u128::from(value)
        .checked_mul(u128::from(multiplier))
        .ok_or(Error::Overflow)?;
    let value = value
        .checked_add(u128::from(divisor) - 1)
        .ok_or(Error::Overflow)?
        / u128::from(divisor);
    value.try_into().map_err(|_| Error::Overflow)
}
#[derive(Clone, Copy, Debug)]
pub struct SendPair {
    common_before: Stamp,
    local_after_ms: u64,
}
impl SendPair {
    /// Call from the sender thread; the acquisition order is part of the conservative proof.
    pub fn capture() -> Result<Self, Error> {
        let common = super::common_stamp()?;
        Self::from_parts(common, super::monotonic_ms())
    }
    /// Injected ordered pair; caller must really have sampled common first, local second.
    pub fn from_parts(common_before: Stamp, local_after_ms: u64) -> Result<Self, Error> {
        common_before.validate()?;
        local(local_after_ms)?;
        Ok(Self {
            common_before,
            local_after_ms,
        })
    }
}
#[derive(Clone, Copy, Debug)]
pub struct ReceivePair {
    local_before_ms: u64,
    common_after: Stamp,
}
impl ReceivePair {
    pub fn capture() -> Result<Self, Error> {
        let before = super::monotonic_ms();
        Self::from_parts(before, super::common_stamp()?)
    }
    /// Injected ordered pair; caller must really have sampled local first, common second.
    pub fn from_parts(local_before_ms: u64, common_after: Stamp) -> Result<Self, Error> {
        common_after.validate()?;
        local(local_before_ms)?;
        Ok(Self {
            local_before_ms,
            common_after,
        })
    }
}
#[derive(Clone, Default)]
struct PairTracker {
    previous: Option<(u64, Stamp)>,
}
impl PairTracker {
    fn observe(&mut self, local_ms: u64, common: Stamp) -> Result<(), Error> {
        if let Some((previous_local, previous_common)) = self.previous {
            if common.frequency != previous_common.frequency {
                return Err(Error::DomainMismatch);
            }
            if local_ms < previous_local || common.counter < previous_common.counter {
                return Err(Error::ClockRegressed);
            }
        }
        // Track all valid pairs, even when the associated source value is future/expired.
        self.previous = Some((local_ms, common));
        Ok(())
    }
}
#[derive(Clone, Default)]
pub struct Publisher {
    pairs: PairTracker,
    cached: Option<(u64, Stamp)>,
}
impl Publisher {
    pub fn map(
        &mut self,
        observed_ms: u64,
        pair: SendPair,
        max_age_ms: u64,
    ) -> Result<Stamp, Error> {
        maximum(max_age_ms)?;
        self.pairs
            .observe(pair.local_after_ms, pair.common_before)?;
        local(observed_ms)?;
        let age = pair
            .local_after_ms
            .checked_sub(observed_ms)
            .ok_or(Error::Future)?;
        if age >= max_age_ms {
            return Err(Error::Expired);
        }
        if let Some((previous, stamp)) = self.cached {
            if observed_ms < previous {
                return Err(Error::ObservationRegressed);
            }
            if observed_ms == previous {
                return Ok(stamp);
            }
        }
        // floor(local elapsed) has <1ms uncertainty; ceil and an extra QPC tick age safely.
        let delta = ceil_product(
            age.checked_add(1).ok_or(Error::Overflow)?,
            pair.common_before.frequency,
            1000,
        )?
        .checked_add(1)
        .ok_or(Error::Overflow)?;
        let counter = pair
            .common_before
            .counter
            .checked_sub(delta)
            .filter(|counter| *counter != 0)
            .ok_or(Error::Underflow)?;
        let stamp = Stamp {
            counter,
            frequency: pair.common_before.frequency,
        };
        stamp.validate()?;
        if self
            .cached
            .is_some_and(|(_, previous)| counter < previous.counter)
        {
            return Err(Error::ObservationRegressed);
        }
        self.cached = Some((observed_ms, stamp));
        Ok(stamp)
    }
}
#[derive(Clone, Default)]
pub struct Receiver {
    pairs: PairTracker,
    cached: Option<(Stamp, u64)>,
}
impl Receiver {
    pub fn map(&mut self, stamp: Stamp, pair: ReceivePair, max_age_ms: u64) -> Result<u64, Error> {
        maximum(max_age_ms)?;
        self.pairs
            .observe(pair.local_before_ms, pair.common_after)?;
        stamp.validate()?;
        if stamp.frequency != pair.common_after.frequency {
            return Err(Error::DomainMismatch);
        }
        let delta = pair
            .common_after
            .counter
            .checked_sub(stamp.counter)
            .ok_or(Error::Future)?
            .checked_add(1)
            .ok_or(Error::Overflow)?;
        let age = ceil_product(delta, 1000, stamp.frequency)?
            .checked_add(1)
            .ok_or(Error::Overflow)?;
        if age >= max_age_ms {
            return Err(Error::Expired);
        }
        if let Some((previous, observed_ms)) = self.cached {
            if stamp.counter < previous.counter {
                return Err(Error::ObservationRegressed);
            }
            if stamp.counter == previous.counter {
                if pair
                    .local_before_ms
                    .checked_sub(observed_ms)
                    .is_none_or(|age| age >= max_age_ms)
                {
                    return Err(Error::Expired);
                }
                return Ok(observed_ms);
            }
        }
        let observed = pair
            .local_before_ms
            .checked_sub(age)
            .filter(|observed| *observed != 0)
            .ok_or(Error::Underflow)?;
        if self.cached.is_some_and(|(_, previous)| observed < previous) {
            return Err(Error::ObservationRegressed);
        }
        self.cached = Some((stamp, observed));
        Ok(observed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostic_age_accounts_for_delivery_rounding_and_invalid_domains() {
        let sent = Stamp {
            counter: 1000,
            frequency: 1000,
        };
        assert_eq!(sent.age_ms_at(sent).unwrap(), 2);
        assert_eq!(
            sent.age_ms_at(Stamp {
                counter: 2500,
                ..sent
            })
            .unwrap(),
            1502
        );
        assert_eq!(
            sent.age_ms_at(Stamp {
                counter: 999,
                ..sent
            }),
            Err(Error::Future)
        );
        assert_eq!(
            sent.age_ms_at(Stamp {
                frequency: 2000,
                ..sent
            }),
            Err(Error::DomainMismatch)
        );
        assert_eq!(
            Stamp { counter: 0, ..sent }.age_ms_at(sent),
            Err(Error::InvalidStamp)
        );
        assert_eq!(
            Stamp {
                counter: 1,
                frequency: 1
            }
            .age_ms_at(Stamp {
                counter: i64::MAX as u64,
                frequency: 1
            }),
            Err(Error::Overflow)
        );
    }
    #[cfg(windows)]
    #[test]
    #[ignore = "invoked only by the isolated two-process clock test"]
    fn clock_stream_child() {
        use std::io::{BufRead, Write};
        assert_eq!(
            std::env::var("PC_CONTROL_CLOCK_STAND_CHILD").as_deref(),
            Ok("1")
        );
        super::super::monotonic_ms();
        std::thread::sleep(std::time::Duration::from_millis(100));
        println!("CLOCK_READY");
        std::io::stdout().flush().unwrap();
        let mut lines = std::io::stdin().lock().lines();
        let mut receiver = Receiver::default();
        let mut first = None;
        for _ in 0..2 {
            let line = lines.next().unwrap().unwrap();
            assert!(line.len() < 256);
            let stamp: Stamp = serde_json::from_str(&line).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(120));
            let before = super::super::common_stamp().unwrap();
            let local = receiver
                .map(stamp, ReceivePair::capture().unwrap(), 2000)
                .unwrap();
            let age = super::super::monotonic_ms() - local;
            let original_age = (u128::from(before.counter - stamp.counter) * 1000
                / u128::from(stamp.frequency)) as u64;
            assert!(age >= original_age, "delivery must not make data younger");
            if let Some(first) = first {
                assert_eq!(local, first);
            } else {
                first = Some(local);
            }
            println!("CLOCK_AGE {age}");
            std::io::stdout().flush().unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn clock_stream_two_process_delivery_preserves_original_age_and_cached_stamp() {
        use std::{
            io::{BufRead, BufReader, Write},
            os::windows::process::CommandExt,
            process::{Child, Command, Stdio},
            sync::mpsc,
            time::{Duration, Instant},
        };
        struct Owned(Child);
        impl Drop for Owned {
            fn drop(&mut self) {
                if !matches!(self.0.try_wait(), Ok(Some(_))) {
                    let _ = self.0.kill();
                }
                let _ = self.0.wait();
            }
        }
        let mut child = Owned(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "clock::stream::tests::clock_stream_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("PC_CONTROL_CLOCK_STAND_CHILD", "1")
                .creation_flags(0x08000000)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let output = child.0.stdout.take().unwrap();
        let (send, receive) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if send.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let wait_line = |prefix: &str| {
            let end = Instant::now() + Duration::from_secs(5);
            loop {
                let line = receive
                    .recv_timeout(end.saturating_duration_since(Instant::now()))
                    .unwrap();
                if line.starts_with(prefix) {
                    break line;
                }
            }
        };
        wait_line("CLOCK_READY");
        let original = super::super::monotonic_ms();
        let stamp = Publisher::default()
            .map(original, SendPair::capture().unwrap(), 2000)
            .unwrap();
        let line = serde_json::to_string(&stamp).unwrap() + "\n";
        std::thread::sleep(Duration::from_millis(80));
        child
            .0
            .stdin
            .as_mut()
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
        let first: u64 = wait_line("CLOCK_AGE ")
            .split_once(' ')
            .unwrap()
            .1
            .parse()
            .unwrap();
        assert!(first >= 200);
        std::thread::sleep(Duration::from_millis(80));
        child
            .0
            .stdin
            .as_mut()
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
        let second: u64 = wait_line("CLOCK_AGE ")
            .split_once(' ')
            .unwrap()
            .1
            .parse()
            .unwrap();
        assert!(second >= first + 200);
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < end, "own clock stand child did not exit");
            std::thread::sleep(Duration::from_millis(2));
        }
        reader.join().unwrap();
    }
    fn stamp(counter: u64, frequency: u64) -> Stamp {
        Stamp { counter, frequency }
    }
    fn send(common: u64, local: u64, frequency: u64) -> SendPair {
        SendPair::from_parts(stamp(common, frequency), local).unwrap()
    }
    fn receive(local: u64, common: u64, frequency: u64) -> ReceivePair {
        ReceivePair::from_parts(local, stamp(common, frequency)).unwrap()
    }
    #[test]
    fn clock_stream_rounding_and_delayed_pairs_are_conservative_across_different_local_epochs() {
        let mut publisher = Publisher::default();
        let original = publisher
            .map(4900, send(1_000_000, 5000, 10_000), 2000)
            .unwrap();
        assert_eq!(original.counter, 998_989);
        let mut receiver = Receiver::default();
        let observed = receiver
            .map(original, receive(9000, 1_001_000, 10_000), 2000)
            .unwrap();
        assert_eq!(observed, 8797);
        assert!(9000 - observed >= 201);
        // A larger sampling gap between common-before and local-after only makes the source older.
        let delayed = Publisher::default()
            .map(4900, send(1_000_000, 5010, 10_000), 2000)
            .unwrap();
        assert!(delayed.counter < original.counter);
        let delayed_local = Receiver::default()
            .map(original, receive(8990, 1_001_000, 10_000), 2000)
            .unwrap();
        assert!(delayed_local < observed);
    }
    #[test]
    fn clock_stream_cached_original_stamps_never_renew_in_sender_or_receiver() {
        let mut publisher = Publisher::default();
        let first = publisher
            .map(1000, send(100_000, 1000, 10_000), 2000)
            .unwrap();
        assert_eq!(
            publisher
                .map(1000, send(110_000, 2000, 10_000), 2000)
                .unwrap(),
            first
        );
        assert_eq!(
            publisher.map(1000, send(120_000, 3000, 10_000), 2000),
            Err(Error::Expired)
        );
        let mut receiver = Receiver::default();
        let mapped = receiver
            .map(first, receive(5000, 100_100, 10_000), 2000)
            .unwrap();
        assert_eq!(
            receiver
                .map(first, receive(5500, 105_000, 10_000), 2000)
                .unwrap(),
            mapped
        );
        assert_eq!(
            receiver.map(first, receive(7000, 120_100, 10_000), 2000),
            Err(Error::Expired)
        );
    }
    #[test]
    fn clock_stream_rejects_future_regressed_domain_and_underflow_without_clipping() {
        assert_eq!(
            Publisher::default().map(1001, send(100_000, 1000, 10_000), 2000),
            Err(Error::Future)
        );
        assert_eq!(
            Publisher::default().map(1, send(1, 1, 10_000), 2000),
            Err(Error::Underflow)
        );
        assert_eq!(
            Receiver::default().map(stamp(101, 10_000), receive(500, 100, 10_000), 2000),
            Err(Error::Future)
        );
        assert_eq!(
            Receiver::default().map(stamp(100, 10_000), receive(1, 101, 10_000), 2000),
            Err(Error::Underflow)
        );
        let mut publisher = Publisher::default();
        publisher
            .map(1000, send(100_000, 1000, 10_000), 2000)
            .unwrap();
        assert_eq!(
            publisher.map(1000, send(100_001, 999, 10_000), 2000),
            Err(Error::ClockRegressed)
        );
        assert_eq!(
            publisher.map(1000, send(99_999, 1001, 10_000), 2000),
            Err(Error::ClockRegressed)
        );
        assert_eq!(
            publisher.map(999, send(100_001, 1001, 10_000), 2000),
            Err(Error::ObservationRegressed)
        );
        assert_eq!(
            publisher.map(1001, send(100_002, 1002, 20_000), 2000),
            Err(Error::DomainMismatch)
        );
        let mut receiver = Receiver::default();
        receiver
            .map(stamp(100_000, 10_000), receive(1000, 100_100, 10_000), 2000)
            .unwrap();
        assert_eq!(
            receiver.map(stamp(99_999, 10_000), receive(1001, 100_101, 10_000), 2000),
            Err(Error::ObservationRegressed)
        );
        assert_eq!(
            receiver.map(stamp(100_001, 20_000), receive(1002, 100_102, 10_000), 2000),
            Err(Error::DomainMismatch)
        );
    }
    #[test]
    fn clock_stream_invalid_old_and_overflow_inputs_do_not_wrap_or_restamp() {
        for invalid in [
            stamp(0, 1),
            stamp(1, 0),
            stamp(1, MAX_FREQUENCY + 1),
            stamp(u64::MAX, 1),
        ] {
            assert!(invalid.validate().is_err());
        }
        assert!(SendPair::from_parts(stamp(1, 1), 0).is_err());
        assert!(ReceivePair::from_parts(u64::MAX, stamp(1, 1)).is_err());
        assert_eq!(
            Publisher::default().map(1, send(100_000, 2001, 10_000), 2000),
            Err(Error::Expired)
        );
        assert_eq!(
            Receiver::default().map(stamp(1, 1), receive(5000, stamp_max(), 1), 2000),
            Err(Error::Overflow)
        );
        assert_eq!(
            Publisher::default().map(1, send(100_000, 1, 10_000), 0),
            Err(Error::InvalidMaxAge)
        );
        assert!(
            serde_json::from_str::<Stamp>(r#"{"counter":1,"frequency":1000,"now":2}"#).is_err()
        );
        let mut publisher = Publisher::default();
        assert_eq!(
            publisher.map(2000, send(100_000, 1000, 10_000), 2000),
            Err(Error::Future)
        );
        assert_eq!(
            publisher.map(900, send(99_000, 900, 10_000), 2000),
            Err(Error::ClockRegressed)
        );
    }
    fn stamp_max() -> u64 {
        i64::MAX as u64
    }
    #[test]
    fn clock_stream_quantized_epoch_grid_never_extends_the_original_deadline() {
        // Unit is 0.1ms; model event times, millisecond floor/+1 clocks, and both capture gaps.
        let frequency = 10_000;
        for event in 1000..1010 {
            for elapsed in [0, 1, 9, 10, 19, 100, 1000] {
                for gap in [0, 1, 9, 25] {
                    let common_event = 100_000 + event;
                    let sender_epoch = 900;
                    let source_local = (event - sender_epoch) / 10 + 1;
                    let common_before = common_event + elapsed;
                    let local_after = (event + elapsed + gap - sender_epoch) / 10 + 1;
                    let wire = Publisher::default()
                        .map(
                            source_local,
                            send(common_before, local_after, frequency),
                            2000,
                        )
                        .unwrap();
                    assert!(wire.counter <= common_event);
                    let child_epoch = 800;
                    let child_before = event + elapsed + gap + 100;
                    let local_before = (child_before - child_epoch) / 10 + 1;
                    let received = Receiver::default()
                        .map(
                            wire,
                            receive(local_before, 100_000 + child_before + gap, frequency),
                            2000,
                        )
                        .unwrap();
                    let original_child = (event - child_epoch) / 10 + 1;
                    assert!(received <= original_child);
                    assert!(received + 2000 <= original_child + 2000);
                }
            }
        }
    }
    #[cfg(windows)]
    #[test]
    fn clock_stream_native_qpc_and_existing_local_clock_map_conservatively_without_replacement() {
        let _ = super::super::monotonic_ms();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let observed = super::super::monotonic_ms();
        let mut publisher = Publisher::default();
        let wire = publisher
            .map(observed, SendPair::capture().unwrap(), 2000)
            .unwrap();
        let mut receiver = Receiver::default();
        let local = receiver
            .map(wire, ReceivePair::capture().unwrap(), 2000)
            .unwrap();
        assert!(local <= observed);
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(
            publisher
                .map(observed, SendPair::capture().unwrap(), 2000)
                .unwrap(),
            wire
        );
        assert_eq!(
            receiver
                .map(wire, ReceivePair::capture().unwrap(), 2000)
                .unwrap(),
            local
        );
    }
}
