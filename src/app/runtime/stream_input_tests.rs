use super::*;
use crate::app::{
    sensor_bindings::Scope,
    sensor_sessions::{Observation, Snapshot},
    sensor_stream::{Descriptor, Receiver, Sender},
};
use crate::clock::stream::{ReceivePair, SendPair, Stamp};

#[test]
fn stream_context_replay_and_competing_provider_cannot_replace_accepted_runtime_data() {
    let mut stand = Stand::new();
    stand.document.temperatures.sources = vec![sensors::Source {
        key: "cpu".into(),
        value: sensors::Value::Sensor {
            input: "wire-slot".into(),
        },
    }];
    let mut runtime = Runtime::new(stand.document.clone(), None).unwrap();
    let mut snapshot = Snapshot {
        scope: Scope::lighting("runtime-stream", &stand.document).unwrap(),
        subscription: 1,
        sequence: 1,
        observations: vec![Observation {
            input: "wire-slot".into(),
            source_id: "parent-reader".into(),
            source_revision: 1,
            generation: 1,
            availability_epoch: 0,
            phase: sensor_worker::Phase::Ready,
            reading: Some(crate::sensors::Reading {
                celsius: 42.,
                observed_ms: 1000,
            }),
            error: None,
        }],
    };
    let descriptor = Descriptor::from_snapshot(&snapshot, [1; 32]).unwrap();
    let mut sender = Sender::new(descriptor.clone()).unwrap();
    let mut receiver = Receiver::new(descriptor.clone()).unwrap();
    runtime.configure_temperature_stream(&descriptor).unwrap();
    let stamp = |counter| Stamp {
        counter,
        frequency: 1000,
    };
    let frame = sender
        .frame(
            &snapshot,
            SendPair::from_parts(stamp(100000), 1000).unwrap(),
        )
        .unwrap();
    let batch = receiver
        .accept(
            &frame,
            ReceivePair::from_parts(9000, stamp(100010)).unwrap(),
        )
        .unwrap();
    runtime
        .accept_temperature_stream(&batch, 9_000_000)
        .unwrap();
    let accepted = runtime.temperature_samples().to_vec();
    let last = runtime.last_us;
    assert_eq!(accepted[0].celsius, 42.);
    assert!(accepted[0].observed_ms < 9000);
    assert!(
        runtime
            .accept_temperature_stream(&batch, 9_001_000)
            .is_err()
    );
    assert_eq!(runtime.last_us, last);
    assert_eq!(runtime.temperature_samples(), accepted);
    let mut foreign = descriptor.clone();
    foreign.nonce = [2; 32];
    let mut wrong = Receiver::new(foreign).unwrap();
    let mut foreign_frame = frame;
    foreign_frame.nonce = [2; 32];
    foreign_frame.sequence = 2;
    let foreign_batch = wrong
        .accept(
            &foreign_frame,
            ReceivePair::from_parts(9100, stamp(100110)).unwrap(),
        )
        .unwrap();
    assert!(
        runtime
            .accept_temperature_stream(&foreign_batch, 9_100_000)
            .is_err()
    );
    assert_eq!(runtime.last_us, last);
    assert_eq!(runtime.temperature_samples(), accepted);
    assert!(runtime.configure_temperature_stream(&descriptor).is_err());
    assert!(
        runtime
            .attach_sensor(
                "wire-slot",
                vec![],
                Arc::new(|_| panic!("Must reject before factory"))
            )
            .is_err()
    );
    assert!(
        runtime
            .accept_temperatures(
                &[lcd::Sample {
                    key: "wire-slot".into(),
                    celsius: 99.,
                    observed_ms: 9000
                }],
                9_001_000
            )
            .is_err()
    );
    assert_eq!(runtime.temperature_samples(), accepted);
    snapshot.sequence = 2;
    snapshot.observations[0].availability_epoch = 1;
    snapshot.observations[0].reading = None;
    let frame = sender
        .frame(
            &snapshot,
            SendPair::from_parts(stamp(100200), 1200).unwrap(),
        )
        .unwrap();
    let batch = receiver
        .accept(
            &frame,
            ReceivePair::from_parts(9200, stamp(100210)).unwrap(),
        )
        .unwrap();
    runtime
        .accept_temperature_stream(&batch, 9_200_000)
        .unwrap();
    assert!(runtime.temperature_samples().is_empty());
    assert_eq!(
        runtime.temperature_stream.as_ref().unwrap().tokens["wire-slot"],
        (1, 1)
    );
    runtime.stop();
    assert!(runtime.drained());
}
