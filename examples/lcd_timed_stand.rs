//! Accelerated monotonic timeline; no device or working FanControl connection.
use pc_control::lcd::{
    media,
    overlay::Renderer,
    player::{Overlay, OverlayValue, Player, State},
    protocol::{ACK_BYTES, REPORT_BYTES, ReportTransport},
    temperatures::{LIQUID_ID, RAM_MEAN_ID, Reading, Sources},
};
use std::{path::PathBuf, sync::Arc};
#[derive(Default)]
struct Fake {
    now: u64,
    writes: u64,
    acks: u64,
}
impl ReportTransport for Fake {
    fn now_ms(&self) -> u64 {
        self.now
    }
    fn write_report(&mut self, _: &[u8; REPORT_BYTES], _: u64) -> Result<usize, String> {
        self.now += 1;
        self.writes += 1;
        Ok(REPORT_BYTES)
    }
    fn read_ack(&mut self, reply: &mut [u8; ACK_BYTES], _: u64) -> Result<usize, String> {
        reply.fill(0);
        self.now += 1;
        self.acks += 1;
        Ok(ACK_BYTES)
    }
}
fn reading(id: &str, name: &str, value: f64) -> Reading {
    Reading {
        id: id.into(),
        name: name.into(),
        origin: "Synthetic stand".into(),
        celsius: value,
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        return Err("Usage: lcd_timed_stand <media> <stand-output-directory>".into());
    }
    let output = PathBuf::from(&args[2]);
    std::fs::create_dir_all(&output)?;
    let media = Arc::new(media::load(&PathBuf::from(&args[1]), 180, || false)?);
    let font = PathBuf::from(std::env::var_os("SystemRoot").ok_or("Windows system font required")?)
        .join("Fonts/seguibl.ttf");
    let mut p = Player::new(media, Renderer::new(std::fs::read(font)?)?)?;
    p.start()?;
    let mut t = Fake {
        now: 1000,
        ..Fake::default()
    };
    let mut sources = Sources::with_ram_ids(vec!["fixture/ram/a".into(), "fixture/ram/b".into()])?;
    let mut refresh = 1000;
    let mut stopped = false;
    let mut resumed = false;
    let mut blanked = false;
    let mut unblanked = false;
    let mut sent = 0u64;
    let mut overlays = 0u64;
    let mut no_overlay = 0u64;
    let mut black = 0u64;
    let mut stop_writes = None;
    let mut blank_writes = None;
    let mut max_overlay_age = 0u64;
    while t.now < 91000 {
        if t.now >= refresh {
            let frozen_all = (45000..53000).contains(&t.now);
            if !(10000..18000).contains(&t.now) && !frozen_all {
                sources.accept_fan(
                    Ok(vec![
                        reading("fixture/cpu", "CPU", 56.),
                        reading("fixture/gpu", "GPU", 48.),
                        reading("fixture/ram/a", "DIMM A", 40.),
                        reading("fixture/ram/b", "DIMM B", 44.),
                    ]),
                    t.now,
                );
            }
            if !frozen_all && sources.liquid_due(t.now) {
                sources.attempted_liquid(t.now);
                sources.accept_liquid(Ok(vec![reading(LIQUID_ID, "Liquid", 33.4)]), t.now);
            }
            refresh = t.now + 1000;
        }
        if t.now >= 30000 && !stopped {
            p.requests().stop();
            stopped = true;
        }
        if t.now >= 35000 && !resumed {
            p.start()?;
            resumed = true;
        }
        if t.now >= 60000 && !blanked {
            p.requests().blank();
            blanked = true;
        }
        if t.now >= 65000 && !unblanked {
            p.start()?;
            unblanked = true;
        }
        let snapshot = sources.combined(t.now);
        let values = ["fixture/cpu", "fixture/gpu", RAM_MEAN_ID, LIQUID_ID]
            .into_iter()
            .filter_map(|id| {
                let found: Vec<_> = snapshot.readings.iter().filter(|r| r.id == id).collect();
                if found.len() != 1 {
                    return None;
                }
                Some(OverlayValue {
                    id: id.into(),
                    label: found[0].name.clone(),
                    celsius: found[0].celsius,
                })
            })
            .collect();
        let observation = sources.oldest_observation(t.now);
        let overlay = Overlay::new(true, 5, observation, values)?;
        if let Some(frame) = p.poll(&overlay, &mut t)? {
            sent += 1;
            if frame.frame_index.is_none() {
                black += 1;
                assert_eq!(p.state(), &State::Blanked);
                blank_writes = Some(t.writes);
                assert!(
                    image::load_from_memory(p.preview())?
                        .into_rgb8()
                        .pixels()
                        .all(|pixel| pixel.0 == [0, 0, 0])
                );
            } else if frame.overlay_applied {
                overlays += 1;
                let age = t.now - observation.ok_or("Overlay had no fresh source at selection")?;
                max_overlay_age = max_overlay_age.max(age);
            } else {
                no_overlay += 1;
            }
        }
        if stopped && !resumed && p.state() == &State::Stopped {
            if let Some(count) = stop_writes {
                assert_eq!(count, t.writes);
            } else {
                stop_writes = Some(t.writes);
            }
        }
        if blanked && !unblanked && p.state() == &State::Blanked {
            assert_eq!(blank_writes, Some(t.writes));
        }
        // Timer wakes, not catch-up bursts. All time below is synthetic, not a benchmark.
        let due = p.next_due_ms().unwrap_or(refresh);
        t.now = due.min(refresh).max(t.now + 1);
    }
    assert_eq!(sent, t.acks);
    assert_eq!(black, 1);
    assert!(no_overlay > 0);
    assert!(overlays > 0);
    assert!(max_overlay_age <= 5000 + 32); // ACK finishes a little after snapshot selection.
    let report = serde_json::json!({"mode":"accelerated-synthetic-clock","timeline_ms":90000,
        "acknowledged_fake_frames":sent,"overlay_frames":overlays,"baseline_frames":no_overlay,
        "black_frames":black,"stop_gap_no_writes":stop_writes.is_some(),"blank_gap_no_writes":blank_writes.is_some(),
        "max_receive_age_at_ack_ms":max_overlay_age,"fake_packet_writes":t.writes,"hardware_writes":0,
        "physical_acceptance":false,"resource_benchmark":false});
    std::fs::write(
        output.join("timed-result.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{report}");
    Ok(())
}
