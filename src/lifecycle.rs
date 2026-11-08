//! Pure handling of notifications supplied by a future native host.
//! This module cannot suspend, resume, restart or shut down the computer.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    None,
    Settling(u64),
    Refreshing(u64),
    Ready,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Refresh { generation: u64 },
    ReconcileSchedule { generation: u64 },
    RestoreUserIntent { generation: u64 },
}

pub struct PowerCoordinator {
    suspended: bool,
    handled: bool,
    generation: u64,
    pending: Pending,
    last_time: Option<u64>,
    error: Option<String>,
    pub suspends: u64,
    pub resumes: u64,
    pub coalesced: u64,
}
impl Default for PowerCoordinator {
    fn default() -> Self {
        Self {
            suspended: false,
            handled: false,
            generation: 0,
            pending: Pending::None,
            last_time: None,
            error: None,
            suspends: 0,
            resumes: 0,
            coalesced: 0,
        }
    }
}
impl PowerCoordinator {
    fn observe(&mut self, now_ms: u64) -> Result<(), &'static str> {
        if self.last_time.is_some_and(|last| now_ms < last) {
            return Err("Monotonic clock moved backwards");
        }
        self.last_time = Some(now_ms);
        Ok(())
    }
    pub fn notify(&mut self, suspended: bool, now_ms: u64) -> Result<(), &'static str> {
        self.observe(now_ms)?;
        if suspended {
            self.suspended = true;
            self.handled = false;
            self.pending = Pending::None;
            self.generation = self.generation.wrapping_add(1);
            self.suspends = self.suspends.saturating_add(1);
        } else if self.handled {
            self.coalesced = self.coalesced.saturating_add(1);
        } else {
            self.suspended = false;
            self.handled = true;
            self.error = None;
            self.resumes = self.resumes.saturating_add(1);
            self.pending = Pending::Settling(now_ms.saturating_add(7000));
        }
        Ok(())
    }
    /// Called by the host's timer; no timers or worker threads are created here.
    /// busy covers staged operations and in-flight transport work to preserve user edits.
    pub fn poll(
        &mut self,
        now_ms: u64,
        busy: bool,
        schedule_active: bool,
    ) -> Result<Option<Action>, &'static str> {
        self.observe(now_ms)?;
        if self.suspended || busy {
            return Ok(None);
        }
        match self.pending {
            Pending::Settling(until) if now_ms >= until => {
                self.pending = Pending::Refreshing(now_ms.saturating_add(30000));
                Ok(Some(Action::Refresh {
                    generation: self.generation,
                }))
            }
            Pending::Refreshing(deadline) if now_ms >= deadline => {
                self.pending = Pending::None;
                self.error =
                    Some("Device refresh timed out; no lighting state was submitted".into());
                Ok(None)
            }
            Pending::Ready => {
                self.pending = Pending::None;
                Ok(Some(if schedule_active {
                    Action::ReconcileSchedule {
                        generation: self.generation,
                    }
                } else {
                    Action::RestoreUserIntent {
                        generation: self.generation,
                    }
                }))
            }
            _ => Ok(None),
        }
    }
    pub fn refreshed(
        &mut self,
        generation: u64,
        now_ms: u64,
        result: Result<(), &str>,
    ) -> Result<(), &'static str> {
        self.observe(now_ms)?;
        let Pending::Refreshing(deadline) = self.pending else {
            return Err("No pending refresh");
        };
        if self.suspended || generation != self.generation || now_ms >= deadline {
            return Err("Stale refresh completion");
        }
        match result {
            Ok(()) => self.pending = Pending::Ready,
            Err(error) => {
                if error.len() > 8192 {
                    return Err("Refresh error too large");
                }
                self.pending = Pending::None;
                self.error = Some(error.into());
            }
        }
        Ok(())
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub fn permits(&self, generation: u64) -> bool {
        !self.suspended && generation == self.generation
    }
    pub fn pending(&self) -> bool {
        self.pending != Pending::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resume_waits_for_settle_and_busy_then_refresh_before_reconcile() {
        let mut p = PowerCoordinator::default();
        p.notify(true, 0).unwrap();
        p.notify(false, 1000).unwrap();
        p.notify(false, 1100).unwrap();
        assert_eq!(p.coalesced, 1);
        assert_eq!(p.poll(7999, false, true).unwrap(), None);
        assert_eq!(p.poll(8000, true, true).unwrap(), None);
        let Some(Action::Refresh { generation }) = p.poll(8250, false, true).unwrap() else {
            panic!("Missing refresh")
        };
        assert_eq!(p.poll(8300, false, true).unwrap(), None);
        p.refreshed(generation, 8500, Ok(())).unwrap();
        assert_eq!(
            p.poll(8500, false, true).unwrap(),
            Some(Action::ReconcileSchedule { generation })
        );
        assert!(!p.pending());
        assert_eq!(p.poll(9000, false, true).unwrap(), None);
    }
    #[test]
    fn resuspend_invalidates_late_callback_and_old_dispatch() {
        let mut p = PowerCoordinator::default();
        p.notify(false, 0).unwrap();
        let Some(Action::Refresh { generation }) = p.poll(7000, false, true).unwrap() else {
            panic!()
        };
        p.notify(true, 7100).unwrap();
        assert!(!p.permits(generation));
        assert!(p.refreshed(generation, 7200, Ok(())).is_err());
        assert_eq!(p.poll(8000, false, true).unwrap(), None);
        p.notify(false, 9000).unwrap();
        let Some(Action::Refresh {
            generation: new_generation,
        }) = p.poll(16000, false, false).unwrap()
        else {
            panic!()
        };
        assert_ne!(new_generation, generation);
        p.refreshed(new_generation, 16100, Ok(())).unwrap();
        assert_eq!(
            p.poll(16100, false, false).unwrap(),
            Some(Action::RestoreUserIntent {
                generation: new_generation
            })
        );
    }
    #[test]
    fn timeout_and_refresh_error_never_submit_unverified_state() {
        let mut p = PowerCoordinator::default();
        p.notify(false, 0).unwrap();
        p.poll(7000, false, true).unwrap();
        assert_eq!(p.poll(37000, false, true).unwrap(), None);
        assert!(p.error().unwrap().contains("timed out"));
        assert!(!p.pending());
        p.notify(true, 38000).unwrap();
        p.notify(false, 39000).unwrap();
        let Some(Action::Refresh { generation }) = p.poll(46000, false, true).unwrap() else {
            panic!()
        };
        p.refreshed(generation, 47000, Err("Device missing"))
            .unwrap();
        assert_eq!(p.poll(47000, false, true).unwrap(), None);
        assert_eq!(p.error(), Some("Device missing"));
    }
    #[test]
    fn backwards_monotonic_time_is_rejected() {
        let mut p = PowerCoordinator::default();
        p.notify(true, 100).unwrap();
        assert!(p.notify(false, 99).is_err());
        assert!(!p.permits(1));
    }
}
