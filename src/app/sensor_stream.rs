//! Parent registry -> child observation codec. Metadata and original age only, never targets.
//! Atomic validation clones clock state; no clock acquisition, native activation or I/O here.
use super::{strict_json::unique_json, lcd, sensor_bindings::Scope, sensor_sessions};
use crate::{
    clock::stream::{self, ReceivePair, SendPair, Stamp},
    runtime::output::Phase,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub const MAX_BYTES: usize = 65536;
pub const MAX_SLOTS: usize = 256;
fn name(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 2048 || value.chars().any(char::is_control) {
        Err("Invalid stream identity".into())
    } else {
        Ok(())
    }
}
fn header(version: u32, nonce: &[u8; 32]) -> Result<(), String> {
    if version != 1 || nonce.iter().all(|byte| *byte == 0) {
        Err("Unsupported stream version or empty launch nonce".into())
    } else {
        Ok(())
    }
}
fn finite(value: f64) -> Result<(), String> {
    if value.is_finite() && (-20.0..=150.0).contains(&value) {
        Ok(())
    } else {
        Err("Invalid stream temperature".into())
    }
}
fn bounded<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("Sensor stream exceeds 65536 bytes".into());
    }
    Ok(bytes)
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slot {
    pub input: String,
    pub source_id: String,
    pub source_revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub version: u32,
    pub nonce: [u8; 32],
    pub scope: Scope,
    pub subscription: u64,
    pub slots: Vec<Slot>,
}
impl Descriptor {
    pub fn from_snapshot(
        snapshot: &sensor_sessions::Snapshot,
        nonce: [u8; 32],
    ) -> Result<Self, String> {
        let descriptor = Self {
            version: 1,
            nonce,
            scope: snapshot.scope.clone(),
            subscription: snapshot.subscription,
            slots: snapshot
                .observations
                .iter()
                .map(|observation| Slot {
                    input: observation.input.clone(),
                    source_id: observation.source_id.clone(),
                    source_revision: observation.source_revision,
                })
                .collect(),
        };
        descriptor.validate()?;
        Ok(descriptor)
    }
    pub fn validate(&self) -> Result<(), String> {
        header(self.version, &self.nonce)?;
        self.scope.validate()?;
        if self.subscription == 0 || self.slots.is_empty() || self.slots.len() > MAX_SLOTS {
            return Err("Invalid stream descriptor membership".into());
        }
        let mut inputs = BTreeSet::new();
        let mut sources = BTreeMap::new();
        for slot in &self.slots {
            name(&slot.input)?;
            name(&slot.source_id)?;
            if slot.source_revision == 0 || !inputs.insert(&slot.input) {
                return Err("Invalid or duplicate stream slot/input".into());
            }
            if let Some(previous) = sources.insert(&slot.source_id, slot.source_revision)
                && previous != slot.source_revision
            {
                return Err("Aliases disagree on source revision".into());
            }
        }
        bounded(self)?;
        Ok(())
    }
    pub fn inputs(&self) -> Vec<String> {
        self.slots.iter().map(|slot| slot.input.clone()).collect()
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        bounded(self)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let value = unique_json(bytes, MAX_BYTES)?;
        let descriptor: Self = serde_json::from_value(value).map_err(|error| error.to_string())?;
        descriptor.validate()?;
        Ok(descriptor)
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Value {
    pub celsius: f64,
    pub observed: Stamp,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub slot: u16,
    pub generation: u64,
    pub availability_epoch: u64,
    pub reading: Option<Value>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub version: u32,
    pub nonce: [u8; 32],
    pub sequence: u64,
    pub observations: Vec<Observation>,
}
impl Frame {
    pub fn validate(&self) -> Result<(), String> {
        header(self.version, &self.nonce)?;
        if self.sequence == 0 || self.observations.is_empty() || self.observations.len() > MAX_SLOTS
        {
            return Err("Invalid stream frame sequence/count".into());
        }
        let mut slots = BTreeSet::new();
        for observation in &self.observations {
            if observation.slot as usize >= MAX_SLOTS
                || observation.generation == 0
                || !slots.insert(observation.slot)
            {
                return Err("Invalid or duplicate stream observation slot/generation".into());
            }
            if let Some(value) = &observation.reading {
                finite(value.celsius)?;
                value
                    .observed
                    .validate()
                    .map_err(|error| error.to_string())?;
            }
        }
        bounded(self)?;
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        bounded(self)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let value = unique_json(bytes, MAX_BYTES)?;
        let frame: Self = serde_json::from_value(value).map_err(|error| error.to_string())?;
        frame.validate()?;
        Ok(frame)
    }
}
fn metadata(
    previous_generation: u64,
    previous_epoch: u64,
    generation: u64,
    epoch: u64,
) -> Result<bool, String> {
    if generation == 0
        || generation < previous_generation
        || (generation == previous_generation && epoch < previous_epoch)
    {
        return Err("Sensor generation/availability epoch rolled back".into());
    }
    Ok(generation != previous_generation)
}
fn unavailable<T>(result: Result<T, stream::Error>) -> Result<Option<T>, String> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(stream::Error::Expired | stream::Error::Underflow) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}
#[derive(Clone, Default)]
struct Sending {
    generation: u64,
    epoch: u64,
    last: Option<u64>,
    clock: stream::Publisher,
}
pub struct Sender {
    descriptor: Descriptor,
    sequence: u64,
    sources: BTreeMap<String, Sending>,
}
impl Sender {
    pub fn new(descriptor: Descriptor) -> Result<Self, String> {
        descriptor.validate()?;
        Ok(Self {
            descriptor,
            sequence: 0,
            sources: BTreeMap::new(),
        })
    }
    pub fn frame(
        &mut self,
        snapshot: &sensor_sessions::Snapshot,
        pair: SendPair,
    ) -> Result<Frame, String> {
        if snapshot.scope != self.descriptor.scope
            || snapshot.subscription != self.descriptor.subscription
            || snapshot.sequence == 0
            || snapshot.sequence <= self.sequence
            || snapshot.observations.len() != self.descriptor.slots.len()
        {
            return Err("Registry snapshot scope/subscription/sequence/membership mismatch".into());
        }
        let mut by_input = BTreeMap::new();
        for observation in &snapshot.observations {
            if by_input
                .insert(observation.input.as_str(), observation)
                .is_some()
            {
                return Err("Duplicate registry input".into());
            }
        }
        let mut raw = BTreeMap::new();
        for slot in &self.descriptor.slots {
            let observation = by_input
                .get(slot.input.as_str())
                .ok_or("Registry input missing")?;
            if observation.source_id != slot.source_id
                || observation.source_revision != slot.source_revision
                || observation.generation == 0
            {
                return Err("Registry source identity/revision/generation mismatch".into());
            }
            let reading = if observation.phase == Phase::Ready {
                observation
                    .reading
                    .map(|reading| (reading.celsius, reading.observed_ms))
            } else {
                None
            };
            if let Some((celsius, _)) = reading {
                finite(celsius)?;
            }
            let tuple = (
                observation.generation,
                observation.availability_epoch,
                reading,
            );
            if let Some(previous) = raw.insert(slot.source_id.clone(), tuple)
                && previous != tuple
            {
                return Err("Registry aliases disagree on source observation".into());
            }
        }
        let mut sources = self.sources.clone();
        let mut values = BTreeMap::new();
        for (id, (generation, epoch, reading)) in raw {
            let state = sources.entry(id.clone()).or_default();
            if metadata(state.generation, state.epoch, generation, epoch)? {
                *state = Sending::default();
            }
            state.generation = generation;
            state.epoch = epoch;
            let value = if let Some((celsius, observed)) = reading {
                if state.last.is_some_and(|last| observed < last) {
                    return Err("Original registry observation regressed".into());
                }
                let mapped = unavailable(state.clock.map(observed, pair, stream::MAX_AGE_MS))?;
                state.last = Some(observed);
                mapped.map(|observed| Value { celsius, observed })
            } else {
                if let Some(last) = state.last {
                    unavailable(state.clock.map(last, pair, stream::MAX_AGE_MS))?;
                }
                None
            };
            values.insert(id, (generation, epoch, value));
        }
        let frame = Frame {
            version: 1,
            nonce: self.descriptor.nonce,
            sequence: snapshot.sequence,
            observations: self
                .descriptor
                .slots
                .iter()
                .enumerate()
                .map(|(index, slot)| {
                    let (generation, epoch, reading) = &values[&slot.source_id];
                    Observation {
                        slot: index as u16,
                        generation: *generation,
                        availability_epoch: *epoch,
                        reading: reading.clone(),
                    }
                })
                .collect(),
        };
        frame.validate()?;
        self.sources = sources;
        self.sequence = snapshot.sequence;
        Ok(frame)
    }
}
#[derive(Clone, Default)]
struct Receiving {
    generation: u64,
    epoch: u64,
    last: Option<Stamp>,
    clock: stream::Receiver,
}
pub struct Receiver {
    descriptor: Descriptor,
    sequence: u64,
    sources: BTreeMap<String, Receiving>,
}
/// Constructed only by a successful complete receive; values retain child-local original age.
#[derive(Clone, Debug)]
pub struct Batch {
    descriptor: Descriptor,
    sequence: u64,
    samples: Vec<lcd::Sample>,
    tokens: BTreeMap<String, (u64, u64)>,
}
impl Batch {
    pub fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }
    pub fn samples(&self) -> Vec<lcd::Sample> {
        self.samples.clone()
    }
    pub fn tokens(&self) -> BTreeMap<String, (u64, u64)> {
        self.tokens.clone()
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}
impl Receiver {
    pub fn new(descriptor: Descriptor) -> Result<Self, String> {
        descriptor.validate()?;
        Ok(Self {
            descriptor,
            sequence: 0,
            sources: BTreeMap::new(),
        })
    }
    pub fn accept(&mut self, frame: &Frame, pair: ReceivePair) -> Result<Batch, String> {
        frame.validate()?;
        if frame.nonce != self.descriptor.nonce
            || frame.sequence <= self.sequence
            || frame.observations.len() != self.descriptor.slots.len()
        {
            return Err("Stream nonce/sequence/membership mismatch".into());
        }
        let mut raw = BTreeMap::new();
        for observation in &frame.observations {
            let slot = self
                .descriptor
                .slots
                .get(observation.slot as usize)
                .ok_or("Stream slot is outside launch descriptor")?;
            let tuple = (
                observation.generation,
                observation.availability_epoch,
                observation.reading.clone(),
            );
            if let Some(previous) = raw.insert(slot.source_id.clone(), tuple.clone())
                && previous != tuple
            {
                return Err("Wire aliases disagree on source observation".into());
            }
        }
        let mut sources = self.sources.clone();
        let mut values = BTreeMap::new();
        for (id, (generation, epoch, reading)) in raw {
            let state = sources.entry(id.clone()).or_default();
            if metadata(state.generation, state.epoch, generation, epoch)? {
                *state = Receiving::default();
            }
            state.generation = generation;
            state.epoch = epoch;
            let sample = if let Some(value) = reading {
                if let Some(previous) = state.last
                    && (value.observed.frequency != previous.frequency
                        || value.observed.counter < previous.counter)
                {
                    return Err("Wire timestamp domain/regression mismatch".into());
                }
                let mapped =
                    unavailable(state.clock.map(value.observed, pair, stream::MAX_AGE_MS))?;
                state.last = Some(value.observed);
                mapped.map(|observed_ms| (value.celsius, observed_ms))
            } else {
                if let Some(last) = state.last {
                    unavailable(state.clock.map(last, pair, stream::MAX_AGE_MS))?;
                }
                None
            };
            values.insert(id, (generation, epoch, sample));
        }
        let mut samples = vec![];
        let mut tokens = BTreeMap::new();
        for slot in &self.descriptor.slots {
            let (generation, epoch, sample) = values[&slot.source_id];
            tokens.insert(slot.input.clone(), (generation, epoch));
            if let Some((celsius, observed_ms)) = sample {
                samples.push(lcd::Sample {
                    key: slot.input.clone(),
                    celsius,
                    observed_ms,
                });
            }
        }
        let batch = Batch {
            descriptor: self.descriptor.clone(),
            sequence: frame.sequence,
            samples,
            tokens,
        };
        self.sources = sources;
        self.sequence = frame.sequence;
        Ok(batch)
    }
}
#[cfg(test)]
mod tests;
