# Compatibility

The table separates implementations, software tests and hardware observations. Connection implementation reviewed: 2026-11-13. Hardware observations remain revision-specific. See [reusable connections](universal-connections.md) for discovery, capability checks, profile compatibility and bounded recovery.

| Path | Implemented scope | Software evidence | Hardware observations |
| --- | --- | --- | --- |
| OpenRGB SDK6 | Explicit server, read-only discovery, complete-controller geometry, stable local binding, explicit Apply and direct colors | [Adapter](../src/transport/openrgb.rs), [packet tests](../src/transport/openrgb_tests.rs), [integration tests](../tests/openrgb_pipeline.rs), [patches](../vendor/openrgb2/LOCAL-CHANGES.md) | Inventory and transport submissions checked on one [local kit](validation/windows-local-kit.json); visible color acceptance pending |
| WLED DDP | LAN discovery or explicit endpoint, reported LED count and saved identity, paced RGB8 output | [Metadata](../src/transport/wled_info.rs), [transport](../src/transport/wled.rs), [integration tests](../tests/wled_pipeline.rs) | Pending; UDP submission has no delivery acknowledgment |
| Home Assistant | LAN server discovery, HTTP(S) light discovery, protected token, capability-aware whole-light commands, at most 1 Hz with dedup | [Transport](../src/transport/home_assistant.rs), [capabilities](../src/devices/home_assistant.rs), [binding tests](../tests/home_assistant_bindings.rs), [worker tests](../src/runtime/network_ha_tests.rs) | Pending; a service response does not confirm physical state |
| BLE | Legacy 20/153 profiles; new Surplife E1 family validates model and reported single-segment point count before variable output; no generic Bluetooth RGB support | [Connection limits](universal-connections.md), [protocol](surplife-e1-pixels.md), [BLE transport](../src/transport/ble.rs) | One selected kit on previous implementation: marker 0 confirmed one physical LED. New capability handshake and other counts/controllers require separate acceptance. [Observation](validation/surplife-153.json) |
| LCD | Thermaltake-family packet path, media composition and temperature overlays | [LCD module](../src/lcd), [protocol provenance](../src/lcd/fixtures/protocol-provenance.md) | JPEG acknowledgments and MI00 temperature reading checked on one [TT 264A:233C](validation/windows-local-kit.json); visible pixels and overlay acceptance pending |
| Selected native RGB | Skydimo, Fusion2, ENE and Valkyrie protocol implementations | [Transports and fixtures](../src/transport), [native adapters](../src/runtime/native.rs) | Per-device acceptance pending; these names do not imply vendor-wide support |
| Windows Dynamic Lighting / LampArray | Planned | No direct adapter in this source preview | Unsupported here |

The packaged Windows preview at `76e91606b992` also completed a 327-second full-kit run: editor close/reopen, all seven output counters, and natural tray Exit with restoration. The [revision-specific application record](validation/windows-preview1.json) includes its resource sample and limits. Physical colors and LCD pixels remain unobserved.

## What counts as support

Record the exact model, hardware revision, firmware, OS, connection mode and backend. For each operation, distinguish:

1. Implemented protocol and parser behavior.
2. Tests with fixtures or a simulated server.
3. An accessible setup and activation path in the application.
4. A physical device check, with observable results and limitations.

Successful compilation does not complete the other stages. A report should say whether discovery, static color, animated frames, stopping, reconnecting and returning ownership were tested. Do not infer untested operations from one successful color change.

## Output boundaries

- The OpenRGB adapter requires SDK6. Inventory does not initialize hardware. Explicit activation can select a direct mode. Limits and topology handling are documented in its [source](../src/transport/openrgb.rs) and [patch record](../vendor/openrgb2/LOCAL-CHANGES.md).
- Virtual controllers are excluded. The same physical identity cannot be activated through multiple SDK addresses. Identical ambiguous metadata on separate servers is conservatively rejected until the physical namespace can be distinguished. A full controller needs a complete point map; synthetic fallback positions are not physical calibration.
- WLED can discover advertised LAN controllers or query an explicit endpoint, read configured LED count and save identity. Verified bindings recheck identity/count before DDP activation; legacy explicit DDP-only bindings remain supported. There is no remote confirmation of visible pixels.
- Home Assistant is intended for slower whole-light updates, not per-pixel realtime effects. Tokens belong in local credential storage, outside portable scenes.
- Home Assistant keys are shared by lights on the same canonical server endpoint and stored in [Windows Credential Manager](../src/credentials_windows.rs). Removing one light preserves the server key. After changing a key, Apply creates a new worker and reads it again. Missing credentials or server rejection can be reported after previous outputs have drained; Return is an explicit recovery action.
- Read-only temperatures do not grant fan or pump control. The application does not replace Fan Control.

Device reports are welcome through the [device report form](https://github.com/malyarq/prismestra/issues/new?template=device.yml).
