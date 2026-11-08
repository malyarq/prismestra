//! Offline renderer/report replay. This executable cannot open a HID device.
use pc_control::lcd::{
    compositor::Compositor,
    media,
    overlay::Renderer,
    protocol::{ACK_BYTES, REPORT_BYTES, ReportTransport, Sender},
};
use std::{path::PathBuf, sync::Arc};
#[derive(Default)]
struct FakeTransport {
    now: u64,
    writes: u64,
    acks: u64,
}
impl ReportTransport for FakeTransport {
    fn now_ms(&self) -> u64 {
        self.now
    }
    fn write_report(&mut self, _: &[u8; REPORT_BYTES], _: u64) -> Result<usize, String> {
        self.now += 1;
        self.writes += 1;
        Ok(REPORT_BYTES)
    }
    fn read_ack(&mut self, response: &mut [u8; ACK_BYTES], _: u64) -> Result<usize, String> {
        response.fill(0);
        self.now += 1;
        self.acks += 1;
        Ok(ACK_BYTES)
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        return Err(
            "Usage: cargo run --release --example lcd_stand -- <media> <stand-output-directory>"
                .into(),
        );
    }
    let output = PathBuf::from(&args[2]);
    std::fs::create_dir_all(&output)?;
    let media = Arc::new(media::load(&PathBuf::from(&args[1]), 180, || false)?);
    media.thumbnail.save(output.join("source-canvas.png"))?;
    let font = PathBuf::from(std::env::var_os("SystemRoot").ok_or("Windows system font required")?)
        .join("Fonts/seguibl.ttf");
    let mut compositor = Compositor::new(media.clone(), Renderer::new(std::fs::read(font)?)?);
    let mut sender = Sender::default();
    let mut transport = FakeTransport::default();
    let mut hits = 0;
    let mut max_bytes = 0;
    for pass in 0..2 {
        compositor.begin_loop();
        for i in 0..media.frames.len() {
            let label = ["CPU", "GPU", "RAM", "Liquid"][i * 4 / media.frames.len()];
            // Explicitly fictional values: this stand reads no physical sensors.
            let value = match label {
                "CPU" => 56.0,
                "GPU" => 48.0,
                "RAM" => 42.0,
                _ => 33.4,
            };
            let frame = compositor.compose(i, label, value)?;
            if !frame.applied || frame.source_fallback {
                return Err("Overlay failed or used JPEG fallback".into());
            }
            hits += u64::from(frame.hit);
            max_bytes = max_bytes.max(frame.jpeg.len());
            let started = transport.now;
            sender.send_frame(&frame.jpeg, false, started, &mut transport)?;
            if pass == 0
                && (i == 0
                    || i == media.frames.len() / 4
                    || i == media.frames.len() / 2
                    || i == media.frames.len() * 3 / 4)
            {
                // Device data is rotated180. Preview is in physical viewing orientation.
                let frame = image::load_from_memory(&frame.jpeg)?.into_rgb8();
                image::imageops::rotate180(&frame)
                    .save(output.join(format!("view-{label}.png")))?;
            }
        }
    }
    let blank = media::encode_jpeg(&image::RgbImage::new(480, 480))?;
    let started = transport.now;
    sender.send_frame(&blank, true, started, &mut transport)?;
    let report = serde_json::json!({"mode":"offline-fake-only","source_frames":media.frames.len(),
        "acknowledged_fake_frames":sender.frames_sent(),"fake_packet_writes":transport.writes,"fake_acks":transport.acks,
        "blank_fake_acked":sender.blank_acked(),"source_decodes":compositor.decoded,"overlay_encodes":compositor.encoded,
        "cache_hits":hits,"cache_entries":compositor.cache_entries(),"cache_cost_bytes":compositor.cache_cost(),
        "max_frame_bytes":max_bytes,"hardware_writes":0,"temperatures":"synthetic, not sensor observations",
        "physical_acceptance":false});
    std::fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{report}");
    Ok(())
}
