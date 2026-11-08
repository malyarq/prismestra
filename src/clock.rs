//! Shared process epoch for capture, playback and received sensor timestamps.
use std::{sync::OnceLock, time::Instant};
static START: OnceLock<Instant> = OnceLock::new();
pub fn monotonic_us() -> u64 {
    START
        .get_or_init(Instant::now)
        .elapsed()
        .as_micros()
        .saturating_add(1000)
        .try_into()
        .unwrap_or(u64::MAX)
}
pub fn monotonic_ms() -> u64 {
    // Zero is reserved for missing observations in imported source contracts.
    START
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .saturating_add(1)
        .try_into()
        .unwrap_or(u64::MAX)
}

/// Conservative IPC timestamp conversion; the process-local epoch above is unchanged.
pub mod stream;

/// OS common counter, comparable only on the same Windows system/boot (not UTC or authentication).
#[cfg(windows)]
pub fn common_stamp() -> Result<stream::Stamp, stream::Error> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn QueryPerformanceCounter(counter: *mut i64) -> i32;
        fn QueryPerformanceFrequency(frequency: *mut i64) -> i32;
    }
    static FREQUENCY: OnceLock<Result<u64, stream::Error>> = OnceLock::new();
    let frequency = (*FREQUENCY.get_or_init(|| {
        let mut value = 0_i64;
        if unsafe { QueryPerformanceFrequency(&mut value) } == 0 || value <= 0 {
            return Err(stream::Error::Unavailable);
        }
        let frequency = value as u64;
        stream::Stamp {
            counter: 1,
            frequency,
        }
        .validate()?;
        Ok(frequency)
    }))?;
    let mut counter = 0_i64;
    if unsafe { QueryPerformanceCounter(&mut counter) } == 0 || counter <= 0 {
        return Err(stream::Error::Unavailable);
    }
    let stamp = stream::Stamp {
        counter: counter as u64,
        frequency,
    };
    stamp.validate()?;
    Ok(stamp)
}
#[cfg(not(windows))]
pub fn common_stamp() -> Result<stream::Stamp, stream::Error> {
    Err(stream::Error::Unavailable)
}
