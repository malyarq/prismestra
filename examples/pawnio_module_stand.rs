//! File-only image inspection. Never opens a kernel, HID or SMBus device.
use pc_control::transport::pawnio::{PIIX4_SHA256, Piix4Module};
fn main() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .ok_or("Expected exactly one local module file path")?;
    if args.next().is_some() {
        return Err("Expected exactly one local module file path".into());
    }
    let module = Piix4Module::read(std::path::Path::new(&path))?;
    println!(
        "{}",
        serde_json::json!({"sha256":PIIX4_SHA256,"bytes":module.bytes().len(),"module_loaded":false,"kernel_device_opens":0,"hardware_writes":0})
    );
    Ok(())
}
