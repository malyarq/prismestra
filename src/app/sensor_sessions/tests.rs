use super::*;
use crate::{app::sensor_pci::Pci, sensors::nvidia};
use std::{
    sync::{
        Condvar,
        atomic::{AtomicI32, AtomicU32, AtomicUsize},
    },
    time::{Duration, Instant},
};
static NEXT: AtomicU32 = AtomicU32::new(1);
fn target() -> Target {
    Target::Nvidia {
        pci: Pci {
            device_id: 0x280310de,
            subsystem_id: 0x60000000 + NEXT.fetch_add(1, Ordering::SeqCst),
            revision_id: 1,
            extended_device_id: 1,
        },
    }
}
fn scope(other: bool) -> Scope {
    if other {
        Scope::Lighting {
            identity: "other-lighting-stand".into(),
            digest: "b".repeat(64),
        }
    } else {
        Scope::Lighting {
            identity: "own-lcd-stand".into(),
            digest: "a".repeat(64),
        }
    }
}
fn route(input: &str, target: &Target) -> Resolved {
    Resolved {
        input: input.into(),
        source_id: "own-source".into(),
        source_revision: 1,
        target: target.clone(),
    }
}
fn wait(mut predicate: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(Instant::now() < end, "Shared sensor fixture deadline");
        std::thread::sleep(Duration::from_millis(2));
    }
}
struct Gate(Arc<(Mutex<bool>, Condvar)>);
impl Gate {
    fn open(&self) {
        let (lock, wake) = &*self.0;
        *lock.lock().unwrap() = true;
        wake.notify_all();
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        self.open();
    }
}
struct Backend {
    binding: nvidia::PciIdentity,
    mode: Arc<AtomicI32>,
    polls: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    gate: Option<Arc<(Mutex<bool>, Condvar)>>,
}
impl Drop for Backend {
    fn drop(&mut self) {
        self.closes.fetch_add(1, Ordering::SeqCst);
    }
}
impl nvidia::Backend for Backend {
    fn identity(&mut self) -> Result<nvidia::PciIdentity, String> {
        Ok(self.binding)
    }
    fn thermal_settings(&mut self, output: &mut nvidia::ThermalSettings) -> Result<(), String> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            let (lock, wake) = &**gate;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = wake.wait(open).unwrap();
            }
        }
        let value = self.mode.load(Ordering::SeqCst);
        if value == i32::MIN {
            return Err("Injected shared reader loss".into());
        }
        *output = nvidia::ThermalSettings::default();
        output.count = 1;
        output.sensors[0].target = nvidia::TARGET_GPU;
        output.sensors[0].current_temperature = value;
        Ok(())
    }
}
struct FakeResolver {
    resolved: usize,
    opens: Arc<AtomicUsize>,
    polls: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    mode: Arc<AtomicI32>,
    gate: Option<Arc<(Mutex<bool>, Condvar)>>,
}
impl Default for FakeResolver {
    fn default() -> Self {
        Self {
            resolved: 0,
            opens: Arc::new(AtomicUsize::new(0)),
            polls: Arc::new(AtomicUsize::new(0)),
            closes: Arc::new(AtomicUsize::new(0)),
            mode: Arc::new(AtomicI32::new(65)),
            gate: None,
        }
    }
}
impl Resolver for FakeResolver {
    fn resolve(&mut self, target: &Target) -> Result<Factory, String> {
        self.resolved += 1;
        let binding = target.nvidia_binding()?;
        let opens = self.opens.clone();
        let polls = self.polls.clone();
        let closes = self.closes.clone();
        let mode = self.mode.clone();
        let gate = self.gate.clone();
        Ok(Arc::new(move |_| {
            opens.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(nvidia::Provider::new(
                binding,
                Backend {
                    binding,
                    mode: mode.clone(),
                    polls: polls.clone(),
                    closes: closes.clone(),
                    gate: gate.clone(),
                },
            )?))
        }))
    }
}
#[test]
fn two_scopes_and_aliases_share_one_protocol_reader_without_restamping() {
    let target = target();
    let mut resolver = FakeResolver::default();
    let mut registry = Registry::default();
    let lcd = registry
        .subscribe(
            scope(false),
            vec![route("gpu-lcd", &target), route("gpu-alias", &target)],
            &mut resolver,
        )
        .unwrap();
    let other = registry
        .subscribe(
            scope(true),
            vec![route("gpu-other", &target)],
            &mut resolver,
        )
        .unwrap();
    wait(|| registry.snapshot(&lcd).unwrap().readings().len() == 2);
    let a = registry.snapshot(&lcd).unwrap();
    let b = registry.snapshot(&other).unwrap();
    let again = registry.snapshot(&lcd).unwrap();
    assert_eq!(resolver.resolved, 1);
    assert_eq!(resolver.opens.load(Ordering::SeqCst), 1);
    assert_eq!(registry.status().len(), 1);
    assert_eq!(a.scope, scope(false));
    assert_eq!(b.scope, scope(true));
    assert!(again.sequence > a.sequence);
    for observation in a
        .observations
        .iter()
        .chain(&b.observations)
        .chain(&again.observations)
    {
        assert_eq!(observation.reading, a.observations[0].reading);
        assert_eq!(observation.generation, a.observations[0].generation);
    }
    registry.unsubscribe(&lcd).unwrap();
    assert!(registry.snapshot(&lcd).is_err());
    assert_eq!(registry.status()[0].consumers, 1);
    assert_eq!(
        registry.snapshot(&other).unwrap().readings()[0].celsius,
        65.0
    );
    drop(other);
    wait(|| registry.drained());
    assert_eq!(resolver.closes.load(Ordering::SeqCst), 1);
}
#[test]
fn missed_unavailable_snapshot_is_carried_by_availability_epoch() {
    let target = target();
    let mut resolver = FakeResolver::default();
    let mut registry = Registry::default();
    let subscription = registry
        .subscribe(scope(true), vec![route("cpu", &target)], &mut resolver)
        .unwrap();
    wait(|| {
        !registry
            .snapshot(&subscription)
            .unwrap()
            .readings()
            .is_empty()
    });
    let first = registry.snapshot(&subscription).unwrap();
    resolver.mode.store(i32::MIN, Ordering::SeqCst);
    wait(|| registry.status()[0].worker.reading.is_none());
    resolver.mode.store(70, Ordering::SeqCst);
    wait(|| {
        registry.status()[0]
            .worker
            .reading
            .is_some_and(|r| r.celsius == 70.0)
    });
    let recovered = registry.snapshot(&subscription).unwrap();
    assert_eq!(
        recovered.observations[0].generation,
        first.observations[0].generation
    );
    assert!(
        recovered.observations[0].availability_epoch > first.observations[0].availability_epoch
    );
    assert_eq!(recovered.readings()[0].celsius, 70.0);
    drop(subscription);
    wait(|| registry.drained());
}
#[test]
fn blocked_reader_retains_endpoint_and_rebind_waits_for_real_drain() {
    let target = target();
    let gate = Gate(Arc::new((Mutex::new(false), Condvar::new())));
    let mut resolver = FakeResolver {
        gate: Some(gate.0.clone()),
        ..Default::default()
    };
    let mut registry = Registry::default();
    let subscription = registry
        .subscribe(scope(false), vec![route("gpu", &target)], &mut resolver)
        .unwrap();
    wait(|| resolver.polls.load(Ordering::SeqCst) == 1);
    let generation = registry.status()[0].worker.generation;
    registry.unsubscribe(&subscription).unwrap();
    assert!(!registry.drained());
    assert!(
        registry
            .subscribe(scope(true), vec![route("gpu", &target)], &mut resolver)
            .is_err()
    );
    assert_eq!(resolver.resolved, 1);
    gate.open();
    wait(|| registry.drained());
    assert_eq!(resolver.closes.load(Ordering::SeqCst), 1);
    let next = registry
        .subscribe(scope(true), vec![route("gpu", &target)], &mut resolver)
        .unwrap();
    wait(|| !registry.snapshot(&next).unwrap().readings().is_empty());
    assert!(registry.snapshot(&next).unwrap().observations[0].generation > generation);
    drop(next);
    wait(|| registry.drained());
    assert_eq!(resolver.opens.load(Ordering::SeqCst), 2);
}
#[test]
fn batch_conflicts_and_foreign_tokens_cannot_open_or_stop_other_sessions() {
    let target = target();
    let mut resolver = FakeResolver::default();
    let mut registry = Registry::default();
    let mut foreign = Registry::default();
    assert!(
        registry
            .subscribe(
                scope(false),
                vec![route("same", &target), route("same", &target)],
                &mut resolver
            )
            .is_err()
    );
    assert_eq!(resolver.resolved, 0);
    let a = Target::Amd {
        vendor: *b"AuthenticAMD",
        signature: 0x00800f12,
        packages: 1,
        module_path: "C:/own/a.bin".into(),
    };
    let mut b = a.clone();
    if let Target::Amd { module_path, .. } = &mut b {
        *module_path = "C:/own/b.bin".into();
    }
    assert!(
        registry
            .subscribe(
                scope(false),
                vec![route("a", &a), route("b", &b)],
                &mut resolver
            )
            .is_err()
    );
    assert_eq!(resolver.resolved, 0);
    let subscription = registry
        .subscribe(scope(false), vec![route("gpu", &target)], &mut resolver)
        .unwrap();
    assert!(foreign.snapshot(&subscription).is_err());
    assert!(foreign.unsubscribe(&subscription).is_err());
    assert!(subscription.active.load(Ordering::Acquire));
    drop(subscription);
    wait(|| registry.drained());
    registry.stop();
    assert!(
        registry
            .subscribe(scope(false), vec![route("gpu", &target)], &mut resolver)
            .is_err()
    );
}
#[test]
fn partial_start_failure_cancels_started_threads_without_publishing_subscription() {
    let mut targets = [target(), target()];
    targets.sort_by_key(|target| target.canonical_key().unwrap());
    let held = crate::runtime::output::Lease::acquire(targets[1].resource_keys().unwrap()).unwrap();
    let gate = Gate(Arc::new((Mutex::new(false), Condvar::new())));
    let mut resolver = FakeResolver {
        gate: Some(gate.0.clone()),
        ..Default::default()
    };
    let mut registry = Registry::default();
    let mut second = route("b", &targets[1]);
    second.source_id = "second-source".into();
    assert!(
        registry
            .subscribe(
                scope(false),
                vec![route("a", &targets[0]), second],
                &mut resolver
            )
            .is_err()
    );
    assert!(registry.consumers.is_empty());
    assert_eq!(registry.status().len(), 1);
    assert!(registry.status()[0].stopping);
    gate.open();
    drop(held);
    wait(|| registry.drained());
    assert_eq!(
        resolver.opens.load(Ordering::SeqCst),
        resolver.closes.load(Ordering::SeqCst)
    );
}
