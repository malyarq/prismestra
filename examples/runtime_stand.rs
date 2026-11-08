//! Offline shared-canvas render: no discovery, bindings, threads or device factories.
use pc_control::{
    devices::Setup,
    runtime::{Host, scene::Scene},
};
use std::{fs, io::Read, path::PathBuf};
fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("Usage: runtime_stand SETUP_JSON SCENE_JSON OUTPUT_DIR".into());
    }
    let setup = Setup::load(&PathBuf::from(&args[0]))?;
    let mut bytes = Vec::new();
    fs::File::open(&args[1])
        .map_err(|e| e.to_string())?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("Scene file exceeds4MiB".into());
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let scene: Scene = if let Some(pattern) = value.get("noise_map") {
        Scene::from_native_noise(pattern)?
    } else {
        serde_json::from_value(value).map_err(|e| e.to_string())?
    };
    scene.validate()?;
    let mut host = Host::new(setup, scene)?;
    let fps = host.scene().fps;
    for index in 0..=fps * 5 {
        host.tick(u64::from(index) * 1_000_000 / u64::from(fps))?;
    }
    let setup = host.setup();
    let scale = 4.0f64.min(1024.0 / f64::from(setup.canvas.width.max(setup.canvas.height)));
    let mut image = image::RgbImage::from_pixel(
        (f64::from(setup.canvas.width) * scale).ceil() as u32,
        (f64::from(setup.canvas.height) * scale).ceil() as u32,
        image::Rgb([12, 14, 18]),
    );
    for (point, color) in setup.canvas.points.iter().zip(host.preview()) {
        let x = (f64::from(point.x) * scale) as u32;
        let y = (f64::from(point.y) * scale) as u32;
        for dy in 0..3 {
            for dx in 0..3 {
                image.put_pixel(
                    (x + dx).min(image.width() - 1),
                    (y + dy).min(image.height() - 1),
                    image::Rgb([color.r, color.g, color.b]),
                );
            }
        }
    }
    let out = PathBuf::from(&args[2]);
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    image
        .save(out.join("canvas.png"))
        .map_err(|e| e.to_string())?;
    fs::write(
        out.join("scene.json"),
        serde_json::to_vec_pretty(host.scene()).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let result = serde_json::json!({"points":setup.canvas.points.len(),"devices":setup.devices.len(),"canvas":[setup.canvas.width,setup.canvas.height],"scene_fps":fps,
        "timeline_us":5_000_000,"stand_hardware_factories_invoked":0,"frame_lengths":setup.devices.iter().map(|d|serde_json::json!({"id":d.id,"leds":host.frame(&d.id).map_or(0,|v|v.len())})).collect::<Vec<_>>(),"benchmark":false});
    fs::write(
        out.join("result.json"),
        serde_json::to_vec_pretty(&result).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!("{result}");
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
