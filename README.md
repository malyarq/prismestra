# Prismestra

Local lighting orchestration for Windows.

Prismestra brings PC lighting, unusual BLE devices, LCD displays and home lighting into one scene. The project uses existing device backends where possible and keeps device support explicit.

**Status: early development.** This repository is a source preview, not a claim that every device or connection mode works.

## What is here

- A Rust scene engine with a shared spatial canvas, effects and schedules.
- Windows desktop capture and independently paced output workers.
- GIF/image composition and temperature overlays for a Thermaltake LCD family.
- Native adapters for selected HID, serial, BLE and memory-lighting protocols.
- Portable scenes and assets separated from local hardware bindings.
- Explicit activation, cancellation and device ownership checks.

The current development work adds a general OpenRGB backend and network lighting transports. Their protocol tests and hardware acceptance are tracked separately.

Prismestra does not control fans or pumps. Keep your existing cooling software.

## Build

Use a current stable Rust toolchain on Windows:

```powershell
cargo build --release --locked --bin pc-control-ui
.\target\release\pc-control-ui.exe
```

The internal crate and executable names are currently `pc-control` and `pc-control-ui`; Prismestra is the product name.

The normal first launch is a preview. Saving a local connection does not activate hardware; applying a supported connection is a separate action.

## Compatibility

Support is recorded per protocol family, device revision, firmware and connection mode. USB, a wireless dongle and Bluetooth on the same keyboard can expose different capabilities.

- Existing backend support does not mean every device was tested by this project.
- Protocol fixtures and fake servers validate software behavior, not physical colors or hardware timing.
- Unknown devices remain unsupported until a safe adapter is available.
- A successful SDK, UDP or HTTP submission is not proof of physical output.

Device controllers, local addresses and credentials belong to local configuration, not exported scenes.

## Development

```powershell
cargo check --locked --all-targets
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Hardware tests are separate. Never run a test that changes cooling, firmware or an unknown device protocol just to make a software test pass.

## License and upstream work

Original Prismestra source is licensed under GPL-2.0-or-later unless a file states otherwise. Third-party components retain their own licenses and notices; a distributed combined build must satisfy all applicable terms.

OpenRGB is an independent upstream project. Prismestra is not affiliated with OpenRGB, SignalRGB or hardware vendors.

