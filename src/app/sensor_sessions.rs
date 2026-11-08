//! Parent-owned shared read-only sessions. Descriptors are data; a resolver is an explicit capability.
//! Observation timestamps belong to this process. They must not be sent as child-local timestamps.
use super::{
    sensor_bindings::{Resolved, Scope, Target},
    sensor_worker::{self, Factory, Worker},
};
use crate::{runtime::output::Phase, sensors::Reading};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
const MAX_SESSIONS: usize = 256;
const MAX_SUBSCRIPTIONS: usize = 256;

pub trait Resolver {
    /// Build a dormant factory only. Registry preflight occurs before this call.
    fn resolve(&mut self, target: &Target) -> Result<Factory, String>;
}
/// Explicit native activation at an operator-selected start, never deserialized from a document.
#[cfg(windows)]
pub struct NativeResolver {
    _private: (),
}
#[cfg(windows)]
impl NativeResolver {
    pub fn explicit_user_start() -> Self {
        Self { _private: () }
    }
}
#[cfg(windows)]
impl Resolver for NativeResolver {
    fn resolve(&mut self, target: &Target) -> Result<Factory, String> {
        super::sensor_sources::native_factory(target)
    }
}
pub struct Subscription {
    owner: Arc<Mutex<BTreeSet<u64>>>,
    id: u64,
    active: Arc<AtomicBool>,
    generations: BTreeSet<u64>,
}
impl Subscription {
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
    pub fn revoke(&self) {
        if self.active.swap(false, Ordering::AcqRel) {
            self.owner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(self.id);
        }
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.revoke();
    }
}
struct Consumer {
    scope: Scope,
    routes: Vec<(Resolved, String)>,
    sequence: u64,
    active: Arc<AtomicBool>,
}
struct Session {
    target: Target,
    keys: Vec<String>,
    worker: Worker,
    consumers: BTreeSet<u64>,
    stopping: bool,
}
#[derive(Clone, Debug)]
pub struct Observation {
    pub input: String,
    pub source_id: String,
    pub source_revision: u64,
    pub generation: u64,
    pub availability_epoch: u64,
    pub phase: Phase,
    pub reading: Option<Reading>,
    pub error: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub scope: Scope,
    pub subscription: u64,
    pub sequence: u64,
    pub observations: Vec<Observation>,
}
impl Snapshot {
    /// Same-process numeric projection; consumers still enforce their own freshness policy.
    /// This drops generation/availability metadata; discontinuity-aware consumers
    /// must inspect the full observations first.
    pub fn readings(&self) -> Vec<super::lcd::Sample> {
        self.observations
            .iter()
            .filter_map(|o| {
                if o.phase != Phase::Ready {
                    return None;
                }
                o.reading.map(|r| super::lcd::Sample {
                    key: o.input.clone(),
                    celsius: r.celsius,
                    observed_ms: r.observed_ms,
                })
            })
            .collect()
    }
}
#[derive(Clone, Debug)]
pub struct Status {
    pub target: Target,
    pub consumers: usize,
    pub stopping: bool,
    pub worker: sensor_worker::Snapshot,
}
pub struct Registry {
    release: Arc<Mutex<BTreeSet<u64>>>,
    sessions: BTreeMap<String, Session>,
    consumers: BTreeMap<u64, Consumer>,
    next_subscription: u64,
    next_generation: u64,
    stopped: bool,
}
/// Waits only for readers stopped by this release, never for unrelated subscribers.
pub struct Retirement {
    owner: Arc<Mutex<BTreeSet<u64>>>,
    generations: BTreeSet<u64>,
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            release: Arc::new(Mutex::new(BTreeSet::new())),
            sessions: BTreeMap::new(),
            consumers: BTreeMap::new(),
            next_subscription: 1,
            next_generation: 1,
            stopped: false,
        }
    }
}
impl Registry {
    fn release_consumer(&mut self, id: u64) {
        if let Some(consumer) = self.consumers.remove(&id) {
            consumer.active.store(false, Ordering::Release);
            for key in consumer
                .routes
                .into_iter()
                .map(|(_, key)| key)
                .collect::<BTreeSet<_>>()
            {
                if let Some(session) = self.sessions.get_mut(&key) {
                    session.consumers.remove(&id);
                    if session.consumers.is_empty() {
                        session.stopping = true;
                        session.worker.stop();
                    }
                }
            }
        }
    }
    pub fn poll(&mut self) {
        let released = std::mem::take(&mut *self.release.lock().unwrap_or_else(|e| e.into_inner()));
        for id in released {
            self.release_consumer(id);
        }
        self.sessions
            .retain(|_, session| !(session.stopping && session.worker.reap()));
    }
    /// All route/resource conflicts are checked before resolving or starting any new factory.
    /// Partial thread-start failures cancel and retain new readers until their actual drain.
    pub fn subscribe(
        &mut self,
        scope: Scope,
        routes: Vec<Resolved>,
        resolver: &mut dyn Resolver,
    ) -> Result<Subscription, String> {
        self.poll();
        scope.validate()?;
        if self.stopped {
            return Err("Sensor registry has stopped".into());
        }
        if routes.is_empty() || routes.len() > 256 || self.consumers.len() >= MAX_SUBSCRIPTIONS {
            return Err("Invalid sensor subscription size".into());
        }
        let mut inputs = BTreeSet::new();
        let mut targets = BTreeMap::new();
        let mut mapped = vec![];
        let mut source_ids = BTreeMap::new();
        for route in routes {
            if !super::ambient::key(&route.input)
                || route.source_id.is_empty()
                || route.source_id.len() > 2048
                || route.source_id.chars().any(char::is_control)
                || route.source_revision == 0
                || !inputs.insert(route.input.clone())
            {
                return Err("Invalid sensor subscription route".into());
            }
            route.target.validate()?;
            let key = route.target.canonical_key()?;
            let keys = route.target.resource_keys()?;
            let identity = (route.source_revision, key.clone());
            if source_ids
                .get(&route.source_id)
                .is_some_and(|known| known != &identity)
            {
                return Err("Conflicting sensor source identity/revision".into());
            }
            source_ids.insert(route.source_id.clone(), identity);
            if let Some(session) = self.sessions.get(&key)
                && (session.stopping
                    || matches!(
                        session.worker.snapshot().phase,
                        Phase::Stopping | Phase::Stopped | Phase::Failed
                    ))
            {
                return Err("Previous sensor session must be released and drained first".into());
            }
            targets
                .entry(key.clone())
                .or_insert((route.target.clone(), keys));
            mapped.push((route, key));
        }
        let new = targets
            .keys()
            .filter(|key| !self.sessions.contains_key(*key))
            .count();
        if self.sessions.len() + new > MAX_SESSIONS {
            return Err("Sensor session limit exceeded".into());
        }
        let mut reservations: BTreeMap<String, String> = BTreeMap::new();
        for (key, session) in &self.sessions {
            for resource in &session.keys {
                reservations.insert(resource.clone(), key.clone());
            }
        }
        for (key, (_, keys)) in &targets {
            for resource in keys {
                if reservations.get(resource).is_some_and(|owner| owner != key) {
                    return Err("Incompatible sensor descriptors share an endpoint".into());
                }
                reservations.insert(resource.clone(), key.clone());
            }
        }
        let id = self.next_subscription;
        let next_id = id
            .checked_add(1)
            .ok_or("Sensor subscription identity exhausted")?;
        let next_generation = self
            .next_generation
            .checked_add(new as u64)
            .ok_or("Sensor generation exhausted")?;
        let mut prepared = vec![];
        for (key, (target, keys)) in targets {
            if !self.sessions.contains_key(&key) {
                let factory = resolver.resolve(&target)?;
                prepared.push((key, target, keys, factory));
            }
        }
        let mut started = vec![];
        for (index, (key, target, keys, factory)) in prepared.into_iter().enumerate() {
            match Worker::start_claimed(self.next_generation + index as u64, keys.clone(), factory)
            {
                Ok(worker) => {
                    self.sessions.insert(
                        key.clone(),
                        Session {
                            target,
                            keys,
                            worker,
                            consumers: BTreeSet::new(),
                            stopping: false,
                        },
                    );
                    started.push(key);
                }
                Err(error) => {
                    self.next_generation = next_generation;
                    for key in started {
                        let session = self.sessions.get_mut(&key).unwrap();
                        session.stopping = true;
                        session.worker.stop();
                    }
                    return Err(error);
                }
            }
        }
        self.next_generation = next_generation;
        self.next_subscription = next_id;
        for (_, key) in &mapped {
            self.sessions.get_mut(key).unwrap().consumers.insert(id);
        }
        let active = Arc::new(AtomicBool::new(true));
        let generations = mapped
            .iter()
            .map(|(_, key)| self.sessions[key].worker.snapshot().generation)
            .collect();
        self.consumers.insert(
            id,
            Consumer {
                scope,
                routes: mapped,
                sequence: 0,
                active: active.clone(),
            },
        );
        Ok(Subscription {
            owner: self.release.clone(),
            id,
            active,
            generations,
        })
    }
    fn check(&self, subscription: &Subscription) -> Result<(), String> {
        if !Arc::ptr_eq(&self.release, &subscription.owner)
            || !subscription.active.load(Ordering::Acquire)
            || !self.consumers.contains_key(&subscription.id)
        {
            return Err("Unknown or released sensor subscription".into());
        }
        Ok(())
    }
    pub fn snapshot(&mut self, subscription: &Subscription) -> Result<Snapshot, String> {
        self.poll();
        self.check(subscription)?;
        let consumer = self.consumers.get_mut(&subscription.id).unwrap();
        let sequence = consumer
            .sequence
            .checked_add(1)
            .ok_or("Sensor observation sequence exhausted")?;
        let mut captured = BTreeMap::new();
        let mut observations = vec![];
        for (route, key) in &consumer.routes {
            let snapshot = captured
                .entry(key.clone())
                .or_insert_with(|| self.sessions[key].worker.snapshot());
            observations.push(Observation {
                input: route.input.clone(),
                source_id: route.source_id.clone(),
                source_revision: route.source_revision,
                generation: snapshot.generation,
                availability_epoch: snapshot.availability_epoch,
                phase: snapshot.phase,
                reading: snapshot.reading,
                error: snapshot.error.clone(),
            });
        }
        consumer.sequence = sequence;
        Ok(Snapshot {
            scope: consumer.scope.clone(),
            subscription: subscription.id,
            sequence,
            observations,
        })
    }
    pub fn unsubscribe(&mut self, subscription: &Subscription) -> Result<(), String> {
        self.poll();
        self.check(subscription)?;
        self.release_consumer(subscription.id);
        Ok(())
    }
    pub fn retire(&mut self, subscription: &Subscription) -> Result<Retirement, String> {
        if !Arc::ptr_eq(&self.release, &subscription.owner) {
            return Err("Unknown registry subscription owner".into());
        }
        subscription.revoke();
        self.release_consumer(subscription.id);
        self.poll();
        let generations = self
            .sessions
            .values()
            .filter(|s| s.stopping)
            .map(|s| s.worker.snapshot().generation)
            .filter(|g| subscription.generations.contains(g))
            .collect();
        Ok(Retirement {
            owner: self.release.clone(),
            generations,
        })
    }
    pub fn retirement_drained(&mut self, retirement: &Retirement) -> Result<bool, String> {
        if !Arc::ptr_eq(&self.release, &retirement.owner) {
            return Err("Unknown registry retirement owner".into());
        }
        self.poll();
        Ok(!self.sessions.values().any(|s| {
            retirement
                .generations
                .contains(&s.worker.snapshot().generation)
        }))
    }
    pub fn status(&self) -> Vec<Status> {
        self.sessions
            .values()
            .map(|s| Status {
                target: s.target.clone(),
                consumers: s.consumers.len(),
                stopping: s.stopping,
                worker: s.worker.snapshot(),
            })
            .collect()
    }
    pub fn stop(&mut self) {
        self.stopped = true;
        for consumer in self.consumers.values() {
            consumer.active.store(false, Ordering::Release);
        }
        self.consumers.clear();
        for session in self.sessions.values_mut() {
            session.consumers.clear();
            session.stopping = true;
            session.worker.stop();
        }
    }
    pub fn drained(&mut self) -> bool {
        self.poll();
        self.sessions.is_empty()
    }
}
impl Drop for Registry {
    fn drop(&mut self) {
        self.stop();
        self.poll();
    }
}

#[cfg(test)]
mod tests;
