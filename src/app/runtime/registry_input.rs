//! Explicit same-process registry ingress. No resolver, native factory or window dependency.
use super::*;
use crate::app::{
    sensor_bindings::Scope,
    sensor_sessions::{Registry, Retirement, Snapshot, Subscription},
};
use std::sync::Mutex;

pub(super) struct Input {
    registry: Arc<Mutex<Registry>>,
    pub(super) subscription: Arc<Subscription>,
    pub(super) config: super::super::sensors::Config,
    pub(super) tokens: BTreeMap<String, (u64, u64)>,
    pub(super) readings: Vec<lcd::Sample>,
    inputs: BTreeSet<String>,
    generation: u64,
    workers: BTreeMap<String, u64>,
}
pub(super) struct Retired {
    registry: Arc<Mutex<Registry>>,
    receipt: Retirement,
}
impl Runtime {
    pub fn registry_subscription(&self) -> Option<u64> {
        self.registry_input
            .as_ref()
            .filter(|i| i.subscription.active())
            .map(|i| i.subscription.id())
    }
    pub(super) fn registry_owns(&self, input: &str) -> bool {
        self.registry_input
            .as_ref()
            .is_some_and(|i| i.inputs.contains(input))
    }
    /// Preflight under the Runtime command lock, before starting a new registry reader.
    pub fn validate_registry_input(
        &self,
        scope: &Scope,
        inputs: &BTreeSet<String>,
    ) -> Result<(), String> {
        self.usable()?;
        let valid_scope = match scope {
            Scope::Lighting { identity, .. } => {
                Scope::lighting(identity, &self.document)? == *scope
            }
        };
        let configured = Self::temperature_inputs(&self.document);
        if !valid_scope
            || inputs.is_empty()
            || inputs.len() > 256
            || inputs.iter().any(|key| {
                !configured.contains(key)
                    || self.sensor_specs.contains_key(key)
                    || self.sensors.contains_key(key)
                    || self
                        .temperature_stream
                        .as_ref()
                        .is_some_and(|s| s.contains(key))
            })
        {
            return Err("Registry subscription does not match this lighting context or has another input owner".into());
        }
        if self.retired_registry.len() >= 256 || self.generation == u64::MAX {
            return Err("Registry input ownership capacity exhausted".into());
        }
        Ok(())
    }
    /// The subscription is an already-authorized capability, not a portable descriptor.
    pub fn bind_registry_input(
        &mut self,
        registry: Arc<Mutex<Registry>>,
        subscription: Arc<Subscription>,
        now_us: u64,
    ) -> Result<(), String> {
        self.usable()?;
        if !subscription.active() {
            return Err("Registry subscription has been revoked".into());
        }
        if self.registry_input.as_ref().is_some_and(|i| {
            Arc::ptr_eq(&i.registry, &registry) && Arc::ptr_eq(&i.subscription, &subscription)
        }) {
            return self.reconcile_registry(now_us);
        }
        self.reap_registry()?;
        if self.retired_registry.len() >= 256 {
            return Err("Too many pending registry retirements".into());
        }
        let snapshot = registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot(&subscription)?;
        let inputs = snapshot
            .observations
            .iter()
            .map(|o| o.input.clone())
            .collect::<BTreeSet<_>>();
        self.validate_registry_input(&snapshot.scope, &inputs)?;
        self.document
            .temperatures
            .select(now_us / 1000, &snapshot.readings())?;
        self.observe(now_us)?;
        let generation = self.next_generation()?;
        self.release_registry_input(now_us)?;
        self.injected_inputs.clear();
        self.registry_input = Some(Input {
            registry,
            subscription,
            config: self.document.temperatures.clone(),
            tokens: BTreeMap::new(),
            readings: vec![],
            inputs,
            generation,
            workers: snapshot
                .observations
                .iter()
                .map(|o| (o.input.clone(), o.generation))
                .collect(),
        });
        self.accept_registry_snapshot(snapshot, now_us)
    }
    pub fn release_registry_input(&mut self, now_us: u64) -> Result<(), String> {
        let Some(input) = self.registry_input.take() else {
            return Ok(());
        };
        let result = input
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retire(&input.subscription);
        let receipt = match result {
            Ok(receipt) => receipt,
            Err(error) => {
                self.registry_input = Some(input);
                return Err(error);
            }
        };
        self.retired_registry.push(Retired {
            registry: input.registry,
            receipt,
        });
        self.injected_inputs.clear();
        // Revocation precedes any replacement data, including two fresh scopes with the same numbers.
        self.rebuild_temperatures(now_us / 1000)?;
        self.reap_registry()
    }
    pub(super) fn reconcile_registry(&mut self, now_us: u64) -> Result<(), String> {
        if self
            .registry_input
            .as_ref()
            .is_some_and(|i| !i.subscription.active() || i.config != self.document.temperatures)
        {
            self.release_registry_input(now_us)?;
        }
        Ok(())
    }
    pub(super) fn registry_tick(&mut self, now_us: u64) -> Result<(), String> {
        self.reap_registry()?;
        self.reconcile_registry(now_us)?;
        let Some(input) = &self.registry_input else {
            return Ok(());
        };
        let snapshot = input
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot(&input.subscription);
        match snapshot {
            Ok(snapshot) => self.accept_registry_snapshot(snapshot, now_us),
            Err(error) => {
                let revoked = self
                    .registry_input
                    .as_ref()
                    .is_some_and(|i| !i.subscription.active());
                self.release_registry_input(now_us)?;
                if revoked { Ok(()) } else { Err(error) }
            }
        }
    }
    fn accept_registry_snapshot(&mut self, snapshot: Snapshot, now_us: u64) -> Result<(), String> {
        let readings = snapshot.readings();
        self.document
            .temperatures
            .select(now_us / 1000, &readings)?;
        let Some(input) = &self.registry_input else {
            return Err("Registry input is not attached".into());
        };
        if !input.subscription.active() {
            return self.release_registry_input(now_us);
        }
        if snapshot.observations.len() != input.workers.len()
            || snapshot
                .observations
                .iter()
                .any(|o| input.workers.get(&o.input) != Some(&o.generation))
        {
            self.release_registry_input(now_us)?;
            return Err("Registry input producer lineage changed without a new binding".into());
        }
        let input = self.registry_input.as_mut().unwrap();
        input.tokens = snapshot
            .observations
            .iter()
            // Worker generations are local to each Registry. Bindings share Runtime's namespace.
            .map(|o| (o.input.clone(), (input.generation, o.availability_epoch)))
            .collect();
        input.readings = readings;
        if self.temperature_input_ready() {
            self.rebuild_temperatures(now_us / 1000)?;
        }
        Ok(())
    }
    fn reap_registry(&mut self) -> Result<(), String> {
        let mut error = None;
        self.retired_registry.retain(|r| {
            match r
                .registry
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retirement_drained(&r.receipt)
            {
                Ok(drained) => !drained,
                Err(e) => {
                    error = Some(e);
                    true
                }
            }
        });
        error.map_or(Ok(()), Err)
    }
    pub(super) fn registry_drained(&mut self) -> bool {
        match self.reap_registry() {
            Ok(()) => self.registry_input.is_none() && self.retired_registry.is_empty(),
            Err(error) => {
                self.error = Some(error);
                false
            }
        }
    }
}
