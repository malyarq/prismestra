//! Own read-only sensor capabilities; portable documents contain logical keys,
//! while physical provider bindings remain separate local state.
pub mod amd;
#[cfg(windows)]
pub(crate) mod amd_windows;
pub mod ddr5;
#[cfg(windows)]
pub(crate) mod ddr5_windows;
pub mod nvidia;
#[cfg(windows)]
pub mod nvidia_windows;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub celsius: f64,
    /// Conservative query-start time in the shared monotonic epoch, not a
    /// hardware acquisition timestamp and never refreshed by a cached read.
    pub observed_ms: u64,
}
impl Reading {
    pub fn is_fresh(self, now_ms: u64, max_age_ms: u64) -> bool {
        self.observed_ms > 0
            && self.celsius.is_finite()
            && now_ms
                .checked_sub(self.observed_ms)
                .is_some_and(|age| age <= max_age_ms)
    }
}
