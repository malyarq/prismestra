# Compatibility

This is an implementation map, not a list of physically certified devices. Last reviewed: 2026-11-08. Read the source and test links for the exact scope.

| Path | Implemented scope | Software evidence | Physical acceptance |
| --- | --- | --- | --- |
| OpenRGB SDK6 | Explicit server, bounded inventory, identity/topology checks, direct colors; setup integration in progress | [Adapter](../src/transport/openrgb.rs), [fake-server tests](../src/transport/openrgb_tests.rs), [patches](../vendor/openrgb2/LOCAL-CHANGES.md) | Pending for this adapter; upstream device support is not our own test result |
| WLED DDP | Explicit unicast endpoint, bounded RGB8 datagrams, cancellation/deadline checks; setup integration in progress | [Transport](../src/transport/wled.rs), [UDP tests](../src/transport/wled_tests.rs) | Pending; UDP submission has no delivery acknowledgment |
| Home Assistant | Explicit HTTP(S) endpoint and token, light inventory, bound whole-light color or off; setup and credential storage integration in progress | [Transport](../src/transport/home_assistant.rs), [fake-server tests](../src/transport/home_assistant_tests.rs) | Pending; a service response does not confirm physical state |
| BLE | Selected LEDnetWF-style protocol; no generic Bluetooth RGB support | [BLE transport](../src/transport/ble.rs), [Windows transport](../src/transport/ble_windows.rs) | Device/revision acceptance pending in the new application |
| LCD | Thermaltake-family packet path, media composition and temperature overlays | [LCD module](../src/lcd), [protocol provenance](../src/lcd/fixtures/protocol-provenance.md) | Device/revision acceptance pending in the new application |
| Selected native RGB | Skydimo, Fusion2, ENE and Valkyrie protocol implementations | [Transports and fixtures](../src/transport), [native adapters](../src/runtime/native.rs) | Per-device acceptance pending; these names do not imply vendor-wide support |
| Windows Dynamic Lighting / LampArray | Planned | No direct adapter in this source preview | Unsupported here |

## What counts as support

Record the exact model, hardware revision, firmware, OS, connection mode and backend. For each operation, distinguish:

1. Implemented protocol and parser behavior.
2. Tests with fixtures or a simulated server.
3. An accessible setup and activation path in the application.
4. A physical device check, with observable results and limitations.

Successful compilation does not complete the other stages. A report should say whether discovery, static color, animated frames, stopping, reconnecting and returning ownership were tested. Do not infer untested operations from one successful color change.

## Output boundaries

- The OpenRGB adapter requires SDK6. Inventory does not initialize hardware. Explicit activation can select a direct mode. Limits and topology handling are documented in its [source](../src/transport/openrgb.rs) and [patch record](../vendor/openrgb2/LOCAL-CHANGES.md).
- WLED is an outbound DDP transport. Configure the endpoint and capacity explicitly. There is no claim of automatic discovery or remote confirmation.
- Home Assistant is intended for slower whole-light updates, not per-pixel realtime effects. Tokens belong in local credential storage, outside portable scenes.
- Read-only temperatures do not grant fan or pump control. The application does not replace Fan Control.

Device reports are welcome through the [device report form](https://github.com/malyarq/prismestra/issues/new?template=device.yml).
