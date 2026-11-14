//! Opt-in bounded recovery for explicitly bound outputs, including initial failures.
use super::*;

#[derive(Default)]
pub(super) struct Recovery {
    attempts: usize,
    failed_generation: Option<u64>,
    due_us: Option<u64>,
}
impl Recovery {
    fn observe(&mut self, generation: u64, failed: bool, now: u64) {
        if failed && self.failed_generation != Some(generation) {
            self.failed_generation = Some(generation);
            self.due_us = [1_000_000, 5_000_000, 15_000_000]
                .get(self.attempts)
                .map(|delay| now.saturating_add(*delay));
        }
    }
    fn take_attempt(&mut self, now: u64, reaped: bool) -> bool {
        if !reaped || !self.due_us.is_some_and(|due| now >= due) {
            return false;
        }
        self.attempts += 1;
        self.due_us = None;
        // Also bound synchronous start failures (e.g. another owner acquired the lease).
        self.failed_generation = None;
        true
    }
}

impl Runtime {
    /// Enable at most three reconnects (1/5/15s), including initial connection failures.
    /// Success does not replenish the budget. Budget is renewed only by
    /// an explicit configuration/refresh/power epoch, never by a flapping light.
    /// Errors are untyped, so even auth/topology failures use this finite budget.
    pub fn enable_output_recovery(&mut self, id: &str) -> Result<(), String> {
        if !self.rgb_specs.contains_key(id) && !self.lcd_specs.contains_key(id) {
            return Err("Output recovery requires a registered output".into());
        }
        self.output_recovery.entry(id.to_owned()).or_default();
        Ok(())
    }

    /// Call after native activation or after a committed output-plan transaction.
    pub fn enable_registered_output_recovery(&mut self) {
        for id in self.rgb_specs.keys().chain(self.lcd_specs.keys()) {
            self.output_recovery.entry(id.clone()).or_default();
        }
    }

    pub(super) fn reset_output_recovery(&mut self, id: &str) {
        if let Some(state) = self.output_recovery.get_mut(id) {
            *state = Recovery::default();
        }
    }

    pub(super) fn recover_failed_outputs(&mut self, now: u64) -> Result<(), String> {
        if self.stopped
            || self.suspended
            || self.pending.is_some()
            || self.power.pending()
            || self.power.error().is_some()
            || self.refresh.is_some()
        {
            return Ok(());
        }
        let mut rgb = Vec::new();
        let mut lcd = Vec::new();
        for (id, state) in &mut self.output_recovery {
            if let Some(spec) = self.rgb_specs.get(id) {
                let Some(snapshot) = self.host.status(id) else {
                    continue;
                };
                let failed = snapshot.phase == crate::runtime::output::Phase::Failed;
                state.observe(snapshot.generation, failed, now);
                if failed && state.take_attempt(now, self.host.output_reaped(id)) {
                    rgb.push((id.clone(), spec.keys.clone(), spec.factory.clone()));
                }
            } else if let (Some(spec), Some(worker)) = (self.lcd_specs.get(id), self.lcd.get(id)) {
                let snapshot = worker.snapshot();
                let failed = snapshot.phase == lcd_worker::Phase::Failed;
                state.observe(snapshot.generation, failed, now);
                if failed && state.take_attempt(now, worker.is_reaped()) {
                    lcd.push((id.clone(), spec.keys.clone(), spec.factory.clone()));
                }
            }
        }
        for (id, keys, factory) in rgb {
            if let Err(error) = self.host.attach_claimed(&id, keys, move |c| factory(c)) {
                self.error = Some(error);
            }
            self.bump_view();
        }
        for (id, keys, factory) in lcd {
            let config = self
                .document
                .lcd
                .iter()
                .find(|entry| entry.device == id)
                .ok_or("Missing LCD configuration during recovery")?
                .config
                .clone();
            let generation = self.next_generation()?;
            match lcd_worker::Worker::start_claimed(generation, keys, move |cancel| {
                let (controller, transport) = factory(config.clone(), cancel)?;
                if controller.config() != &config {
                    return Err("LCD factory configuration changed during recovery".into());
                }
                Ok((controller, transport))
            }) {
                Ok(worker) => {
                    self.lcd.insert(id, worker);
                }
                Err(error) => {
                    self.error = Some(error);
                }
            }
            self.bump_view();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Recovery;

    #[test]
    fn recovery_delays_and_budget_also_bound_failed_reconnection_factories() {
        let mut state = Recovery::default();
        state.observe(1, false, 0);
        assert!(!state.take_attempt(u64::MAX, true));
        let mut now = 0;
        for (generation, delay) in [(1, 1_000_000), (2, 5_000_000), (3, 15_000_000)] {
            state.observe(generation, true, now);
            assert!(!state.take_attempt(now + delay - 1, true));
            assert!(!state.take_attempt(now + delay, false));
            now += delay;
            assert!(state.take_attempt(now, true));
        }
        state.observe(4, true, now);
        assert!(!state.take_attempt(u64::MAX, true));
    }
}
