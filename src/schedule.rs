//! Daily desired states, coalesced after sleep, with durable claims before dispatch.
//! No OS power calls, clocks, threads or hardware access. The host supplies time/storage.
use chrono::{DateTime, Days, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub paused: bool,
    pub timezone: String,
    /// Local minutes since midnight: PC on, door on, all lighting off.
    pub times: [u16; 3],
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            paused: false,
            timezone: "Europe/Moscow".into(),
            times: [600, 1020, 120],
        }
    }
}
impl Config {
    fn zone(&self) -> Result<Tz, String> {
        if self.times.iter().any(|t| *t >= 1440)
            || self.times[0] == self.times[1]
            || self.times[0] == self.times[2]
            || self.times[1] == self.times[2]
        {
            return Err("Schedule times must be distinct local minutes in 0..1440".into());
        }
        self.timezone
            .parse()
            .map_err(|_| "Unknown IANA timezone".into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    Pc,
    Door,
}
impl Target {
    fn index(self) -> usize {
        match self {
            Self::Pc => 0,
            Self::Door => 1,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub occurrence: i64,
    pub on: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttemptState {
    Attempting,
    Submitted,
    Failed,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub event: Event,
    pub count: u8,
    pub state: AttemptState,
    pub retry_at: Option<i64>,
    pub error: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Durable {
    version: u8,
    config: Config,
    seen: [i64; 2],
    attempts: [Option<Attempt>; 2],
}
#[derive(Clone)]
pub struct Schedule {
    durable: Durable,
    zone: Tz,
    observed: Option<i64>,
    reconcile: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dispatch {
    pub target: Target,
    pub event: Event,
    pub attempt: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Submitted,
    Failed,
    Unknown,
}

// IANA conversions handle DST; a fold runs once at its earliest instant. A gap
// moves to the first valid minute within that day. A skipped date has no event.
fn occurrence(zone: Tz, date: NaiveDate, minute: u16) -> Option<i64> {
    for m in minute..1440 {
        let time = NaiveTime::from_hms_opt(u32::from(m / 60), u32::from(m % 60), 0)?;
        match zone.from_local_datetime(&date.and_time(time)) {
            LocalResult::Single(at) | LocalResult::Ambiguous(at, _) => return Some(at.timestamp()),
            LocalResult::None => {}
        }
    }
    None
}
fn instant(now: i64) -> Result<DateTime<Utc>, String> {
    DateTime::from_timestamp(now, 0)
        .filter(|_| now > 0)
        .ok_or_else(|| "Timestamp must be a representable positive Unix second".into())
}

impl Schedule {
    pub fn new(config: Config) -> Result<Self, String> {
        let zone = config.zone()?;
        Ok(Self {
            durable: Durable {
                version: 1,
                config,
                seen: [0; 2],
                attempts: [None, None],
            },
            zone,
            observed: None,
            reconcile: true,
        })
    }
    pub fn config(&self) -> &Config {
        &self.durable.config
    }
    pub fn attempt(&self, target: Target) -> Option<&Attempt> {
        self.durable.attempts[target.index()].as_ref()
    }
    pub fn reconcile_after_refresh(&mut self) {
        self.reconcile = true;
    }
    /// Only an explicit user retry clears an uncertain result. In-flight operations
    /// must first finish or report Unknown; no automatic duplicate dispatch.
    pub fn retry_current(
        &mut self,
        target: Target,
        persist: &mut impl FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        let i = target.index();
        if self.durable.attempts[i]
            .as_ref()
            .is_some_and(|a| a.state == AttemptState::Attempting)
        {
            return Err("Operation is still in flight".into());
        }
        let mut next = self.clone();
        next.durable.attempts[i] = None;
        next.durable.seen[i] = 0;
        if let Err(error) = persist(&next.encoded()?) {
            self.durable.config.enabled = false;
            return Err(error);
        }
        *self = next;
        Ok(())
    }
    pub fn encoded(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&self.durable).map_err(|e| e.to_string())
    }
    pub fn restore(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 262144 {
            return Err("Schedule exceeds 256 KiB".into());
        }
        let durable: Durable = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if durable.version != 1 || durable.seen.iter().any(|t| *t < 0) {
            return Err("Unsupported schedule version or negative watermark".into());
        }
        let zone = durable.config.zone()?;
        for (i, attempt) in durable.attempts.iter().enumerate() {
            if let Some(attempt) = attempt {
                if !(1..=3).contains(&attempt.count)
                    || attempt.event.occurrence <= 0
                    || attempt.event.occurrence > durable.seen[i]
                    || attempt.error.len() > 8192
                    || attempt
                        .retry_at
                        .is_some_and(|at| at <= attempt.event.occurrence)
                    || (attempt.state != AttemptState::Failed && attempt.retry_at.is_some())
                    || (attempt.count == 3 && attempt.retry_at.is_some())
                {
                    return Err("Invalid schedule attempt state".into());
                }
                instant(attempt.event.occurrence)?;
                if let Some(at) = attempt.retry_at {
                    instant(at)?;
                }
            }
        }
        Ok(Self {
            durable,
            zone,
            observed: None,
            reconcile: true,
        })
    }
    pub fn latest(&self, now: i64, target: Target) -> Result<Event, String> {
        let date = instant(now)?.with_timezone(&self.zone).date_naive();
        let mut result = None;
        // Extra lookback includes a whole skipped local date in IANA transitions.
        for days in 0..=3 {
            let date = date
                .checked_sub_days(Days::new(days))
                .ok_or("Calendar underflow")?;
            for index in [target.index(), 2] {
                if let Some(at) = occurrence(self.zone, date, self.durable.config.times[index])
                    && at > 0
                    && at <= now
                    && result.is_none_or(|e: Event| {
                        at > e.occurrence || (at == e.occurrence && index == 2)
                    })
                {
                    result = Some(Event {
                        occurrence: at,
                        on: index != 2,
                    });
                }
            }
        }
        result.ok_or_else(|| "No preceding schedule event".into())
    }
    pub fn next(&self, now: i64) -> Result<i64, String> {
        let date = instant(now)?.with_timezone(&self.zone).date_naive();
        let mut next = None;
        for days in 0..=3 {
            let date = date
                .checked_add_days(Days::new(days))
                .ok_or("Calendar overflow")?;
            for minute in self.durable.config.times {
                if let Some(at) = occurrence(self.zone, date, minute)
                    && at > now
                    && next.is_none_or(|n| at < n)
                {
                    next = Some(at);
                }
            }
        }
        next.ok_or_else(|| "No following schedule event".into())
    }

    /// Persist the whole next state successfully before exposing any dispatch.
    /// A persistence error disables further scheduling in this process.
    pub fn prepare(
        &mut self,
        now: i64,
        persist: &mut impl FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<Vec<Dispatch>, String> {
        instant(now)?;
        // Retain the last enabled observation through pause, so a corrected
        // future clock cannot hide watermarks and suppress days of events.
        if !self.durable.config.enabled || self.durable.config.paused {
            return Ok(Vec::new());
        }
        let mut next = self.clone();
        if next
            .observed
            .is_none_or(|last| last > now.saturating_add(30))
            && next.durable.seen.iter().any(|seen| *seen > now)
        {
            next.durable.seen = [0; 2];
            next.durable.attempts = [None, None];
            next.reconcile = true;
        }
        next.observed = Some(now);
        let mut output = Vec::new();
        for target in [Target::Pc, Target::Door] {
            let i = target.index();
            let event = next.latest(now, target)?;
            let previous = next.durable.attempts[i]
                .as_ref()
                .filter(|a| a.event == event);
            // An interrupted or uncertain command is never blindly retried after restart.
            let uncertain = previous.is_some_and(|a| {
                matches!(a.state, AttemptState::Attempting | AttemptState::Unknown)
            });
            let retry = previous.is_some_and(|a| {
                a.state == AttemptState::Failed
                    && a.count < 3
                    && a.retry_at.is_some_and(|at| at <= now)
            });
            let due = next.reconcile || event.occurrence > next.durable.seen[i];
            if uncertain || (!due && !retry) {
                continue;
            }
            // Startup reconcile must respect a persisted failed attempt's backoff/limit.
            if previous.is_some_and(|a| a.state == AttemptState::Failed) && !retry {
                continue;
            }
            let count = if retry {
                previous.map_or(1, |a| a.count + 1)
            } else {
                1
            };
            next.durable.seen[i] = next.durable.seen[i].max(event.occurrence);
            next.durable.attempts[i] = Some(Attempt {
                event,
                count,
                state: AttemptState::Attempting,
                retry_at: None,
                error: String::new(),
            });
            output.push(Dispatch {
                target,
                event,
                attempt: count,
            });
        }
        next.reconcile = false;
        if !output.is_empty()
            && let Err(error) = persist(&next.encoded()?)
        {
            self.durable.config.enabled = false;
            return Err(error);
        }
        *self = next;
        Ok(output)
    }
    pub fn finish(
        &mut self,
        dispatch: Dispatch,
        now: i64,
        outcome: Outcome,
        error: &str,
        persist: &mut impl FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        instant(now)?;
        if error.len() > 8192 || now < dispatch.event.occurrence {
            return Err("Invalid outcome metadata".into());
        }
        let mut next = self.clone();
        let attempt = next.durable.attempts[dispatch.target.index()]
            .as_mut()
            .ok_or("No matching attempt")?;
        if attempt.event != dispatch.event
            || attempt.count != dispatch.attempt
            || attempt.state != AttemptState::Attempting
        {
            return Err("Stale or duplicate schedule outcome".into());
        }
        attempt.state = match outcome {
            Outcome::Submitted => AttemptState::Submitted,
            Outcome::Failed => AttemptState::Failed,
            Outcome::Unknown => AttemptState::Unknown,
        };
        attempt.error = error.into();
        attempt.retry_at = if outcome == Outcome::Failed && attempt.count < 3 {
            Some(
                now.checked_add(if attempt.count == 1 { 30 } else { 60 })
                    .ok_or("Retry timestamp overflow")?,
            )
        } else {
            None
        };
        if let Err(error) = persist(&next.encoded()?) {
            self.durable.config.enabled = false;
            return Err(error);
        }
        *self = next;
        Ok(())
    }
    /// Host persists a config edit atomically. A manual lighting change need not call this:
    /// it remains in effect until the next due event, as in the prior native application.
    pub fn configure(
        &mut self,
        config: Config,
        persist: &mut impl FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        let zone = config.zone()?;
        let mut next = self.clone();
        if config.timezone != next.durable.config.timezone
            || config.times != next.durable.config.times
        {
            next.durable.seen = [0; 2];
            next.durable.attempts = [None, None];
        }
        next.reconcile = config.enabled && !config.paused && (config != next.durable.config);
        next.durable.config = config;
        next.zone = zone;
        if let Err(error) = persist(&next.encoded()?) {
            self.durable.config.enabled = false;
            return Err(error);
        }
        *self = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn at(value: &str) -> i64 {
        DateTime::parse_from_rfc3339(value).unwrap().timestamp()
    }
    fn enabled() -> Schedule {
        Schedule::new(Config {
            enabled: true,
            ..Config::default()
        })
        .unwrap()
    }
    fn store(_: &[u8]) -> Result<(), String> {
        Ok(())
    }
    #[test]
    fn moscow_defaults_midnight_and_sleep_coalescing() {
        let now = at("2026-11-05T18:05:00+03:00");
        let mut s = Schedule::new(Config::default()).unwrap();
        assert!(s.prepare(now, &mut store).unwrap().is_empty());
        let mut s = enabled();
        let events = s.prepare(now, &mut store).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].event,
            Event {
                occurrence: at("2026-11-05T10:00:00+03:00"),
                on: true
            }
        );
        assert_eq!(
            events[1].event,
            Event {
                occurrence: at("2026-11-05T17:00:00+03:00"),
                on: true
            }
        );
        for d in events {
            s.finish(d, now, Outcome::Submitted, "", &mut store)
                .unwrap();
        }
        assert!(s.prepare(now, &mut store).unwrap().is_empty());
        let events = s
            .prepare(at("2026-11-06T02:00:00+03:00"), &mut store)
            .unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|d| !d.event.on));
        let wake = at("2026-11-08T18:00:00+03:00");
        assert!(
            s.prepare(wake, &mut store)
                .unwrap()
                .iter()
                .all(|d| d.event.on)
        );
        assert_eq!(s.next(wake).unwrap(), at("2026-11-09T02:00:00+03:00"));
    }
    #[test]
    fn durable_claim_precedes_dispatch_and_no_blind_replay_after_crash() {
        let now = at("2026-11-05T18:05:00+03:00");
        let mut s = enabled();
        let mut bytes = Vec::new();
        let events = s
            .prepare(now, &mut |b| {
                bytes = b.to_vec();
                Ok(())
            })
            .unwrap();
        assert_eq!(events.len(), 2);
        let mut restart = Schedule::restore(&bytes).unwrap();
        assert!(restart.prepare(now, &mut store).unwrap().is_empty());
        s.finish(events[0], now, Outcome::Unknown, "timeout", &mut store)
            .unwrap();
        assert!(s.prepare(now + 60, &mut store).unwrap().is_empty());
        assert!(
            s.finish(events[0], now, Outcome::Submitted, "", &mut store)
                .is_err()
        );
    }
    #[test]
    fn persistence_failure_prevents_all_dispatch() {
        let mut s = enabled();
        assert!(
            s.prepare(at("2026-11-05T18:00:00+03:00"), &mut |_| Err(
                "disk full".into()
            ))
            .is_err()
        );
        assert!(!s.config().enabled);
        assert!(
            s.prepare(at("2026-11-05T18:01:00+03:00"), &mut store)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn bounded_retry_backoff_survives_restart() {
        let now = at("2026-11-05T18:05:00+03:00");
        let mut s = enabled();
        let events = s.prepare(now, &mut store).unwrap();
        s.finish(events[1], now, Outcome::Submitted, "", &mut store)
            .unwrap();
        s.finish(events[0], now, Outcome::Failed, "offline", &mut store)
            .unwrap();
        let mut s = Schedule::restore(&s.encoded().unwrap()).unwrap();
        let before = s.prepare(now + 29, &mut store).unwrap();
        assert!(before.iter().all(|d| d.target != Target::Pc));
        let retry = s.prepare(now + 30, &mut store).unwrap();
        assert_eq!(retry.len(), 1);
        assert_eq!(retry[0].attempt, 2);
        s.finish(retry[0], now + 30, Outcome::Failed, "offline", &mut store)
            .unwrap();
        assert!(s.prepare(now + 89, &mut store).unwrap().is_empty());
        let retry = s.prepare(now + 90, &mut store).unwrap();
        assert_eq!(retry[0].attempt, 3);
        s.finish(retry[0], now + 90, Outcome::Failed, "offline", &mut store)
            .unwrap();
        assert!(s.prepare(now + 300, &mut store).unwrap().is_empty());
    }
    #[test]
    fn clock_correction_clears_future_watermark() {
        let mut s = enabled();
        s.prepare(at("2026-11-10T18:00:00+03:00"), &mut store)
            .unwrap();
        let events = s
            .prepare(at("2026-11-08T03:00:00+03:00"), &mut store)
            .unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|d| !d.event.on));
        assert!(
            s.prepare(at("2026-11-08T10:00:00+03:00"), &mut store)
                .unwrap()
                .iter()
                .any(|d| d.target == Target::Pc && d.event.on)
        );
    }
    #[test]
    fn dst_gap_fold_and_skipped_date() {
        let ny: Tz = "America/New_York".parse().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
        assert_eq!(
            occurrence(ny, date, 150),
            Some(at("2026-03-08T03:00:00-04:00"))
        );
        let date = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        assert_eq!(
            occurrence(ny, date, 90),
            Some(at("2026-11-01T01:30:00-04:00"))
        );
        let apia: Tz = "Pacific/Apia".parse().unwrap();
        assert_eq!(
            occurrence(apia, NaiveDate::from_ymd_opt(2011, 12, 30).unwrap(), 600),
            None
        );
    }
    #[test]
    fn reject_invalid_import_and_duplicate_times() {
        assert!(
            Schedule::new(Config {
                times: [1, 1, 2],
                ..Config::default()
            })
            .is_err()
        );
        assert!(
            Schedule::new(Config {
                timezone: "Fake/Zone".into(),
                ..Config::default()
            })
            .is_err()
        );
        assert!(Schedule::restore(&vec![0; 262145]).is_err());
        let s = enabled();
        let mut value: serde_json::Value = serde_json::from_slice(&s.encoded().unwrap()).unwrap();
        value["version"] = 2.into();
        assert!(Schedule::restore(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    #[test]
    fn pause_and_explicit_retry_preserve_unknown_until_requested() {
        let now = at("2026-11-05T18:00:00+03:00");
        let mut s = enabled();
        let dispatch = s.prepare(now, &mut store).unwrap();
        assert!(s.retry_current(Target::Pc, &mut store).is_err());
        s.finish(dispatch[0], now, Outcome::Unknown, "timeout", &mut store)
            .unwrap();
        s.finish(dispatch[1], now, Outcome::Submitted, "", &mut store)
            .unwrap();
        s.reconcile_after_refresh();
        let reconcile = s.prepare(now + 1, &mut store).unwrap();
        assert!(reconcile.iter().all(|d| d.target != Target::Pc));
        let mut config = s.config().clone();
        config.paused = true;
        s.configure(config.clone(), &mut store).unwrap();
        assert!(s.prepare(now + 2, &mut store).unwrap().is_empty());
        s.retry_current(Target::Pc, &mut store).unwrap();
        assert!(s.prepare(now + 3, &mut store).unwrap().is_empty());
        config.paused = false;
        s.configure(config, &mut store).unwrap();
        let retry = s.prepare(now + 4, &mut store).unwrap();
        assert_eq!(retry.len(), 1);
        assert_eq!(retry[0].target, Target::Pc);
        assert_eq!(retry[0].attempt, 1);
    }
    #[test]
    fn dst_collision_prefers_all_off_and_fold_does_not_repeat() {
        let mut s = Schedule::new(Config {
            enabled: true,
            timezone: "America/New_York".into(),
            times: [180, 1020, 150],
            ..Config::default()
        })
        .unwrap();
        let event = s
            .latest(at("2026-03-08T03:00:00-04:00"), Target::Pc)
            .unwrap();
        assert!(!event.on);
        s.configure(
            Config {
                enabled: true,
                timezone: "America/New_York".into(),
                times: [90, 1020, 120],
                ..Config::default()
            },
            &mut store,
        )
        .unwrap();
        let first = s
            .prepare(at("2026-11-01T01:45:00-04:00"), &mut store)
            .unwrap();
        for d in first {
            s.finish(
                d,
                at("2026-11-01T01:45:00-04:00"),
                Outcome::Submitted,
                "",
                &mut store,
            )
            .unwrap();
        }
        assert!(
            s.prepare(at("2026-11-01T01:45:00-05:00"), &mut store)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn failed_outcome_persistence_disables_scheduler_and_keeps_claim() {
        let now = at("2026-11-05T18:00:00+03:00");
        let mut s = enabled();
        let d = s.prepare(now, &mut store).unwrap()[0];
        assert!(
            s.finish(d, now, Outcome::Failed, "offline", &mut |_| Err(
                "disk full".into()
            ))
            .is_err()
        );
        assert!(!s.config().enabled);
        assert_eq!(
            s.attempt(Target::Pc).unwrap().state,
            AttemptState::Attempting
        );
        assert!(s.prepare(now + 300, &mut store).unwrap().is_empty());
    }
    #[test]
    fn correction_while_paused_does_not_hide_future_watermark() {
        let mut s = enabled();
        s.prepare(at("2026-11-10T18:00:00+03:00"), &mut store)
            .unwrap();
        let mut config = s.config().clone();
        config.paused = true;
        s.configure(config.clone(), &mut store).unwrap();
        let now = at("2026-11-08T03:00:00+03:00");
        assert!(s.prepare(now, &mut store).unwrap().is_empty());
        config.paused = false;
        s.configure(config, &mut store).unwrap();
        let events = s.prepare(now, &mut store).unwrap();
        assert!(events.iter().all(|d| !d.event.on));
        for d in events {
            s.finish(d, now, Outcome::Submitted, "", &mut store)
                .unwrap();
        }
        assert!(
            s.prepare(at("2026-11-08T10:00:00+03:00"), &mut store)
                .unwrap()
                .iter()
                .any(|d| d.target == Target::Pc && d.event.on)
        );
    }
    #[test]
    fn native_qt_schedule_differential_4096_cases() {
        let bytes = include_bytes!("fixtures/schedule-native.bin");
        assert_eq!(&bytes[..4], b"SCH1");
        let count = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        assert_eq!(count, 4096);
        assert_eq!(bytes.len(), 8 + 40 * count);
        for (i, row) in bytes[8..].as_chunks::<40>().0.iter().enumerate() {
            let now = i64::from_le_bytes(row[0..8].try_into().unwrap());
            let times = [8, 10, 12]
                .map(|offset| u16::from_le_bytes(row[offset..offset + 2].try_into().unwrap()));
            let schedule = Schedule::new(Config {
                enabled: true,
                times,
                ..Config::default()
            })
            .unwrap();
            let pc = Event {
                occurrence: i64::from_le_bytes(row[14..22].try_into().unwrap()),
                on: row[22] != 0,
            };
            let door = Event {
                occurrence: i64::from_le_bytes(row[23..31].try_into().unwrap()),
                on: row[31] != 0,
            };
            assert_eq!(schedule.latest(now, Target::Pc).unwrap(), pc, "case{i}pc");
            assert_eq!(
                schedule.latest(now, Target::Door).unwrap(),
                door,
                "case{i}door"
            );
            assert_eq!(
                schedule.next(now).unwrap(),
                i64::from_le_bytes(row[32..40].try_into().unwrap()),
                "case{i}next"
            );
        }
    }
}
