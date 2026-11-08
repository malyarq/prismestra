use super::*;
use crate::{
    devices::{Canvas, Device, Driver},
    runtime::output::{Cancellation, RgbDriver, WriteResult},
};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};
fn routes() -> BTreeMap<String, Route> {
    BTreeMap::from([
        (
            "a".into(),
            Route {
                leds: 150,
                stale_after_ms: 100,
            },
        ),
        (
            "b".into(),
            Route {
                leds: 150,
                stale_after_ms: 1000,
            },
        ),
    ])
}
fn frame(color: Rgb, at: u64) -> Frame {
    Frame {
        colors: vec![color; 150].into(),
        observed_ms: Some(at),
    }
}
#[test]
fn latest_slot_rejects_bad_frames_and_replay_without_mutating_last_good_snapshot() {
    assert!(channel(0, routes()).is_err());
    let (writer, reader) = channel(1, routes()).unwrap();
    let first = frame(Rgb::new(100, 0, 0), 1000);
    let colors = first.colors.clone();
    writer
        .publish(BTreeMap::from([("a".into(), first)]), 1000)
        .unwrap();
    let old = reader.snapshot();
    assert!(Arc::ptr_eq(&old.frames["a"].colors, &colors));
    for bad in [
        BTreeMap::from([("unknown".into(), frame(Rgb::default(), 1000))]),
        BTreeMap::from([(
            "a".into(),
            Frame {
                colors: vec![Rgb::default(); 1].into(),
                observed_ms: Some(1000),
            },
        )]),
        BTreeMap::from([("a".into(), frame(Rgb::default(), 2000))]),
        BTreeMap::from([("a".into(), frame(Rgb::default(), 999))]),
    ] {
        assert!(writer.publish(bad, 1001).is_err());
        assert!(Arc::ptr_eq(&old, &reader.snapshot()));
    }
    for at in 1001..2001 {
        writer
            .publish(
                BTreeMap::from([("a".into(), frame(Rgb::new(0, 100, 0), at))]),
                at,
            )
            .unwrap();
    }
    assert_eq!(reader.snapshot().revision, 1001);
    assert_eq!(reader.snapshot().frames.len(), 1);
    assert_eq!(old.frames["a"].colors[0], Rgb::new(100, 0, 0));
    writer.clear();
    assert!(reader.snapshot().frames.is_empty());
    assert!(
        writer
            .publish(
                BTreeMap::from([("a".into(), frame(Rgb::default(), 1000))]),
                2001
            )
            .is_err()
    );
    drop(writer);
    assert!(reader.snapshot().closed);
}
struct Sink(mpsc::Sender<Vec<Rgb>>);
impl RgbDriver for Sink {
    fn protocol(&self) -> Driver {
        Driver::SkydimoSerial {
            model: "SK0410".into(),
        }
    }
    fn leds(&self) -> usize {
        150
    }
    fn write(&mut self, c: &[Rgb], _: &Cancellation) -> Result<WriteResult, String> {
        self.0.send(c.to_vec()).map_err(|e| e.to_string())?;
        Ok(WriteResult::Submitted)
    }
}
fn setup() -> Setup {
    Setup {
        version: 1,
        canvas: Canvas {
            width: 1,
            height: 1,
            points: vec![],
        },
        devices: ["a", "b"]
            .into_iter()
            .map(|id| Device {
                id: id.into(),
                label: id.into(),
                role: Role::Ambient,
                driver: Driver::SkydimoSerial {
                    model: "SK0410".into(),
                },
                leds: 150,
                rate_hz: 25,
                enabled: true,
            })
            .collect(),
    }
}
#[test]
fn host_checks_per_route_age_and_old_publishers_cannot_cross_generation_replacement() {
    let mut host = Host::new(setup(), scene::example()).unwrap();
    let (a, rx_a) = mpsc::channel();
    let (b, rx_b) = mpsc::channel();
    host.attach("a", move |_| Ok(Box::new(Sink(a)))).unwrap();
    host.attach("b", move |_| Ok(Box::new(Sink(b)))).unwrap();
    let (old, reader) = channel(1, routes()).unwrap();
    old.publish(
        BTreeMap::from([
            ("a".into(), frame(Rgb::new(100, 0, 0), 1000)),
            ("b".into(), frame(Rgb::new(0, 100, 0), 1000)),
        ]),
        1000,
    )
    .unwrap();
    host.bind_external_feed(reader).unwrap();
    let (_, invalid) = channel(
        9,
        BTreeMap::from([(
            "a".into(),
            Route {
                leds: 1,
                stale_after_ms: 100,
            },
        )]),
    )
    .unwrap();
    assert!(host.bind_external_feed(invalid).is_err());
    assert_eq!(host.external_feed_generation(), Some(1));
    host.tick(1_000_000).unwrap();
    assert_eq!(
        rx_a.recv_timeout(Duration::from_secs(2)).unwrap()[0],
        Rgb::new(100, 0, 0)
    );
    assert_eq!(
        rx_b.recv_timeout(Duration::from_secs(2)).unwrap()[0],
        Rgb::new(0, 100, 0)
    );
    host.tick(1_101_000).unwrap();
    assert_eq!(
        rx_a.recv_timeout(Duration::from_secs(2)).unwrap()[0],
        Rgb::default()
    );
    assert_eq!(
        rx_b.recv_timeout(Duration::from_secs(2)).unwrap()[0],
        Rgb::new(0, 100, 0)
    );
    let (next, reader) = channel(2, routes()).unwrap();
    next.publish(
        BTreeMap::from([("a".into(), frame(Rgb::new(0, 0, 100), 1200))]),
        1200,
    )
    .unwrap();
    host.bind_external_feed(reader).unwrap();
    old.publish(
        BTreeMap::from([("a".into(), frame(Rgb::new(200, 0, 0), 1200))]),
        1200,
    )
    .unwrap();
    host.tick(1_200_000).unwrap();
    assert_eq!(host.external_feed_generation(), Some(2));
    assert_eq!(
        rx_a.recv_timeout(Duration::from_secs(2)).unwrap()[0],
        Rgb::new(0, 0, 100)
    );
    assert_eq!(
        rx_b.recv_timeout(Duration::from_secs(2)).unwrap()[0],
        Rgb::default()
    );
    drop(old);
    drop(next);
    host.tick(1_201_000).unwrap();
    assert_eq!(
        rx_a.recv_timeout(Duration::from_secs(2)).unwrap()[0],
        Rgb::default()
    );
    host.stop();
    let end = Instant::now() + Duration::from_secs(2);
    while !host.drained() {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(1));
    }
}
