use super::*;
use crate::sensors::Reading;

fn stamp(counter: u64) -> Stamp {
    Stamp {
        counter,
        frequency: 1000,
    }
}
fn send(common: u64, local: u64) -> SendPair {
    SendPair::from_parts(stamp(common), local).unwrap()
}
fn receive(common: u64, local: u64) -> ReceivePair {
    ReceivePair::from_parts(local, stamp(common)).unwrap()
}
fn snapshot(sequence: u64) -> sensor_sessions::Snapshot {
    sensor_sessions::Snapshot {
        scope: Scope::Lighting {
            identity: "stand".into(),
            digest: "a".repeat(64),
        },
        subscription: 7,
        sequence,
        observations: ["a", "z"]
            .into_iter()
            .map(|id| sensor_sessions::Observation {
                input: format!("input-{id}"),
                source_id: id.into(),
                source_revision: 3,
                generation: 1,
                availability_epoch: 0,
                phase: Phase::Ready,
                reading: Some(Reading {
                    celsius: 42.0,
                    observed_ms: 1000,
                }),
                error: None,
            })
            .collect(),
    }
}
fn descriptor() -> Descriptor {
    Descriptor::from_snapshot(&snapshot(1), [7; 32]).unwrap()
}
fn frame(sequence: u64, counter: u64) -> Frame {
    Frame {
        version: 1,
        nonce: [7; 32],
        sequence,
        observations: (0..2)
            .map(|slot| Observation {
                slot,
                generation: 1,
                availability_epoch: 0,
                reading: Some(Value {
                    celsius: 42.0,
                    observed: stamp(counter),
                }),
            })
            .collect(),
    }
}

#[test]
fn original_age_cached_mapping_and_expiry_are_preserved() {
    let mut sender = Sender::new(descriptor()).unwrap();
    let mut receiver = Receiver::new(descriptor()).unwrap();
    let first = sender.frame(&snapshot(1), send(100000, 1000)).unwrap();
    assert_eq!(
        first.observations[0]
            .reading
            .as_ref()
            .unwrap()
            .observed
            .counter,
        99998
    );
    let batch = receiver.accept(&first, receive(100100, 2000)).unwrap();
    assert_eq!(batch.samples()[0].observed_ms, 1896);
    assert_eq!(batch.sequence(), 1);
    assert_eq!(batch.descriptor(), &descriptor());
    let repeated = sender.frame(&snapshot(2), send(100500, 1500)).unwrap();
    assert_eq!(first.observations, repeated.observations);
    let repeated = receiver.accept(&repeated, receive(100500, 2400)).unwrap();
    assert_eq!(repeated.samples()[0].observed_ms, 1896);
    let expired = sender.frame(&snapshot(3), send(102000, 3000)).unwrap();
    assert!(expired.observations.iter().all(|o| o.reading.is_none()));
    let absent = receiver.accept(&expired, receive(102100, 4000)).unwrap();
    assert!(absent.samples().is_empty());
    assert_eq!(absent.tokens().len(), 2);
}

#[test]
fn rejected_final_mapping_commits_neither_clock_nor_sequence() {
    let mut sender = Sender::new(descriptor()).unwrap();
    sender.frame(&snapshot(1), send(100000, 1000)).unwrap();
    let mut bad = snapshot(2);
    bad.observations[0].reading.as_mut().unwrap().observed_ms = 1400;
    bad.observations[1].reading.as_mut().unwrap().observed_ms = 1600;
    assert!(sender.frame(&bad, send(100500, 1500)).is_err());
    bad.observations[1].reading.as_mut().unwrap().observed_ms = 1400;
    assert!(sender.frame(&bad, send(100490, 1490)).is_ok());
    let mut receiver = Receiver::new(descriptor()).unwrap();
    receiver
        .accept(&frame(1, 100000), receive(100100, 2000))
        .unwrap();
    let mut bad = frame(2, 100400);
    bad.observations[1].reading.as_mut().unwrap().observed = stamp(100501);
    assert!(receiver.accept(&bad, receive(100500, 2400)).is_err());
    bad.observations[1].reading.as_mut().unwrap().observed = stamp(100400);
    assert!(receiver.accept(&bad, receive(100490, 2390)).is_ok());
}

#[test]
fn expired_wire_stamps_still_enforce_regression_and_allow_recovery() {
    let mut receiver = Receiver::new(descriptor()).unwrap();
    receiver
        .accept(&frame(1, 100000), receive(100100, 2000))
        .unwrap();
    assert!(
        receiver
            .accept(&frame(2, 102000), receive(105000, 6900))
            .unwrap()
            .samples()
            .is_empty()
    );
    assert!(
        receiver
            .accept(&frame(3, 101500), receive(105100, 7000))
            .is_err()
    );
    assert_eq!(
        receiver
            .accept(&frame(3, 105000), receive(105100, 7000))
            .unwrap()
            .samples()
            .len(),
        2
    );
    let mut sender = Sender::new(descriptor()).unwrap();
    sender.frame(&snapshot(1), send(100000, 1000)).unwrap();
    let mut old = snapshot(2);
    for o in &mut old.observations {
        o.reading.as_mut().unwrap().observed_ms = 3000;
    }
    assert!(
        sender.frame(&old, send(106000, 7000)).unwrap().observations[0]
            .reading
            .is_none()
    );
    old.sequence = 3;
    for o in &mut old.observations {
        o.reading.as_mut().unwrap().observed_ms = 2900;
    }
    assert!(sender.frame(&old, send(106100, 7100)).is_err());
}

#[test]
fn metadata_rollback_is_atomic_generation_reset_is_explicit() {
    let mut receiver = Receiver::new(descriptor()).unwrap();
    let mut first = frame(1, 100000);
    for o in &mut first.observations {
        o.availability_epoch = 5;
    }
    let original = receiver.accept(&first, receive(100100, 2000)).unwrap();
    let mut bad = first.clone();
    bad.sequence = 2;
    bad.observations[0].availability_epoch = 6;
    bad.observations[1].availability_epoch = 4;
    assert!(receiver.accept(&bad, receive(100200, 2100)).is_err());
    bad.observations[1].availability_epoch = 6;
    let good = receiver.accept(&bad, receive(100200, 2100)).unwrap();
    assert_eq!(good.samples(), original.samples()); // epoch does not reset mapping
    let mut next = frame(3, 100100);
    for o in &mut next.observations {
        o.generation = 2;
    }
    assert!(receiver.accept(&next, receive(100300, 2200)).is_ok());
    let mut rollback = frame(4, 100200);
    for o in &mut rollback.observations {
        o.reading = None;
    }
    assert!(receiver.accept(&rollback, receive(100400, 2300)).is_err());
}

#[test]
fn aliases_must_share_metadata_and_effective_reading() {
    let mut source = snapshot(1);
    let mut alias = source.observations[0].clone();
    alias.input = "alias".into();
    source.observations.push(alias);
    let d = Descriptor::from_snapshot(&source, [7; 32]).unwrap();
    let mut sender = Sender::new(d.clone()).unwrap();
    source.observations[2].availability_epoch = 1;
    assert!(sender.frame(&source, send(100000, 1000)).is_err());
    source.observations[2].availability_epoch = 0;
    source.observations[2].phase = Phase::Failed;
    assert!(sender.frame(&source, send(100000, 1000)).is_err());
    source.observations[2].phase = Phase::Ready;
    let good = sender.frame(&source, send(100000, 1000)).unwrap();
    let mut receiver = Receiver::new(d).unwrap();
    let mut bad = good.clone();
    bad.observations[2].reading.as_mut().unwrap().celsius = 43.0;
    assert!(receiver.accept(&bad, receive(100100, 2000)).is_err());
    assert_eq!(
        receiver
            .accept(&good, receive(100100, 2000))
            .unwrap()
            .samples()
            .len(),
        3
    );
}

#[test]
fn sender_binds_exact_registry_scope_subscription_and_sources() {
    let mut sender = Sender::new(descriptor()).unwrap();
    for edit in 0..5 {
        let mut bad = snapshot(1);
        match edit {
            0 => {
                bad.scope = Scope::Lighting {
                    identity: "different-context".into(),
                    digest: "a".repeat(64),
                }
            }
            1 => bad.subscription = 8,
            2 => bad.observations[1].source_id = "other".into(),
            3 => bad.observations[1].source_revision = 4,
            _ => bad.observations[1].input = "input-a".into(),
        }
        assert!(sender.frame(&bad, send(100000, 1000)).is_err());
    }
    let good = sender.frame(&snapshot(1), send(100000, 1000)).unwrap();
    assert!(sender.frame(&snapshot(1), send(100000, 1000)).is_err());
    let mut receiver = Receiver::new(descriptor()).unwrap();
    for edit in 0..6 {
        let mut bad = good.clone();
        match edit {
            0 => bad.nonce = [8; 32],
            1 => bad.version = 2,
            2 => {
                bad.observations.pop();
            }
            3 => bad.observations[1].slot = 0,
            4 => bad.observations[1].slot = 2,
            _ => bad.observations[1].reading.as_mut().unwrap().celsius = f64::NAN,
        }
        assert!(receiver.accept(&bad, receive(100100, 2000)).is_err());
    }
    receiver.accept(&good, receive(100100, 2000)).unwrap();
    assert!(receiver.accept(&good, receive(100100, 2000)).is_err());
}

#[test]
fn strict_wire_and_bounded_descriptors() {
    let d = descriptor();
    assert_eq!(Descriptor::decode(&d.encode().unwrap()).unwrap(), d);
    let f = frame(1, 100000);
    assert_eq!(Frame::decode(&f.encode().unwrap()).unwrap(), f);
    let encoded = String::from_utf8(f.encode().unwrap()).unwrap();
    assert!(
        Frame::decode(
            encoded
                .replacen("\"version\":1", "\"version\":1,\"version\":1", 1)
                .as_bytes()
        )
        .is_err()
    );
    assert!(
        Frame::decode(
            encoded
                .replacen("\"slot\":0", "\"slot\":0,\"extra\":0", 1)
                .as_bytes()
        )
        .is_err()
    );
    assert!(Frame::decode(&vec![b' '; MAX_BYTES + 1]).is_err());
    for edit in 0..6 {
        let mut bad = d.clone();
        match edit {
            0 => bad.nonce = [0; 32],
            1 => bad.subscription = 0,
            2 => bad.slots[1].input = bad.slots[0].input.clone(),
            3 => {
                bad.slots[1].source_id = "a".into();
                bad.slots[1].source_revision = 4;
            }
            4 => bad.slots[0].input = "x".repeat(2049),
            _ => {
                bad.slots = (0..257)
                    .map(|i| Slot {
                        input: i.to_string(),
                        source_id: "a".into(),
                        source_revision: 3,
                    })
                    .collect()
            }
        }
        assert!(bad.validate().is_err());
    }
    let mut too_large = d.clone();
    too_large.slots = (0..40)
        .map(|i| Slot {
            input: format!("{i}{}", "x".repeat(2000)),
            source_id: "y".repeat(2048),
            source_revision: 1,
        })
        .collect();
    assert!(too_large.validate().is_err());
    let mut largest = frame(u64::MAX, u64::MAX / 2);
    largest.observations = (0..256)
        .map(|slot| Observation {
            slot,
            generation: u64::MAX,
            availability_epoch: u64::MAX,
            reading: Some(Value {
                celsius: 150.0,
                observed: Stamp {
                    counter: i64::MAX as u64,
                    frequency: 1_000_000_000_000,
                },
            }),
        })
        .collect();
    assert!(largest.encode().unwrap().len() <= MAX_BYTES);
}

#[test]
fn domain_future_and_underflow_are_fail_closed() {
    let mut receiver = Receiver::new(descriptor()).unwrap();
    assert!(
        receiver
            .accept(&frame(1, 100101), receive(100100, 2000))
            .is_err()
    );
    let mut domain = frame(1, 100000);
    domain.observations[1]
        .reading
        .as_mut()
        .unwrap()
        .observed
        .frequency = 1001;
    assert!(receiver.accept(&domain, receive(100100, 2000)).is_err());
    assert!(
        receiver
            .accept(&frame(1, 100000), receive(100100, 10))
            .unwrap()
            .samples()
            .is_empty()
    );
    let mut sender = Sender::new(descriptor()).unwrap();
    assert!(
        sender
            .frame(&snapshot(1), send(1, 1000))
            .unwrap()
            .observations
            .iter()
            .all(|o| o.reading.is_none())
    );
}
