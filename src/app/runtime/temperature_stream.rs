//! Explicit observation context only. Installing a stream creates no physical reader or capability.
use super::*;
use crate::app::sensor_stream::{Batch, Descriptor};

pub(super) struct State {
    descriptor: Descriptor,
    inputs: BTreeSet<String>,
    sequence: u64,
    pub(super) samples: Vec<lcd::Sample>,
    pub(super) tokens: BTreeMap<String, (u64, u64)>,
}
impl State {
    pub(super) fn contains(&self, input: &str) -> bool {
        self.inputs.contains(input)
    }
}
impl Runtime {
    /// Bound before a temperature stream starts. Replacing a context requires a new stopped/drained runtime.
    pub fn configure_temperature_stream(&mut self, descriptor: &Descriptor) -> Result<(), String> {
        self.usable()?;
        descriptor.validate()?;
        if self.temperature_stream.is_some() {
            return Err("Observation context is already installed".into());
        }
        let configured = Self::temperature_inputs(&self.document);
        let inputs = descriptor.inputs().into_iter().collect::<BTreeSet<_>>();
        if inputs.iter().any(|input| {
            !configured.contains(input)
                || self.registry_owns(input)
                || self.sensors.contains_key(input)
                || self.sensor_specs.contains_key(input)
                || self.injected_inputs.iter().any(|s| s.key == *input)
        }) {
            return Err(
                "Stream inputs must be configured and have no other temperature owner".into(),
            );
        }
        self.temperature_stream = Some(State {
            descriptor: descriptor.clone(),
            inputs,
            sequence: 0,
            samples: vec![],
            tokens: BTreeMap::new(),
        });
        Ok(())
    }

    pub fn accept_temperature_stream(&mut self, batch: &Batch, now_us: u64) -> Result<(), String> {
        self.usable()?;
        let stream = self
            .temperature_stream
            .as_ref()
            .ok_or("No observation context installed")?;
        let samples = batch.samples();
        let tokens = batch.tokens();
        if stream.descriptor != *batch.descriptor()
            || batch.sequence() <= stream.sequence
            || tokens.keys().cloned().collect::<BTreeSet<_>>() != stream.inputs
            || samples.iter().any(|s| {
                !stream.inputs.contains(&s.key)
                    || s.observed_ms == 0
                    || s.observed_ms > now_us / 1000
                    || !s.celsius.is_finite()
                    || !(-20.0..=150.0).contains(&s.celsius)
            })
            || tokens.iter().any(|(key, &(generation, epoch))| {
                generation == 0
                    || stream
                        .tokens
                        .get(key)
                        .is_some_and(|&(old_generation, old_epoch)| {
                            generation < old_generation
                                || (generation == old_generation && epoch < old_epoch)
                        })
            })
        {
            return Err(
                "Observation context, sequence, membership or source lineage mismatch".into(),
            );
        }
        // Validate the entire prospective numeric batch before changing the live stream.
        let mut prospective = self.temperature_observations();
        prospective.retain(|s| !stream.inputs.contains(&s.key));
        prospective.extend(samples.iter().cloned());
        self.document
            .temperatures
            .select(now_us / 1000, &prospective)?;
        self.observe(now_us)?;
        let stream = self.temperature_stream.as_mut().unwrap();
        stream.samples = samples;
        stream.tokens = tokens;
        stream.sequence = batch.sequence();
        // Revocation is immediate on accepted discontinuity, not deferred to a cadence tick.
        self.rebuild_temperatures(now_us / 1000)
    }
}
