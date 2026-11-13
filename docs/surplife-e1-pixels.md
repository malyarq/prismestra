# Surplife E1 pixel output

The BLE adapter has two explicit profile modes: `door_ble` with 20 LEDs retains the legacy E1 03 color-group command; the same driver with 153 LEDs uses E1 04 preview frames. The `door153` preset selects the latter at a requested 10 Hz. Existing profiles keep their previous mode. These are the two implemented counts, not a claim that every Surplife controller supports pixel output. Sources: [profile validation](../src/devices.rs), [presets](../src/app/device_setup.rs), [runtime dispatch](../src/runtime/adapters.rs).

## Protocol evidence

The frame layout was reconstructed from the Android/Dart code in [Surplife](https://play.google.com/store/apps/details?id=com.zennge.magichome2), package `com.zennge.magichome2`. The static specimen was obtained from a third-party mirror, SHA-256 `d5f6f43447a20234bb9aa354ded8c3e6ffdb14b6fbffde99e57868c819b34479`; its signing identity was not independently compared with an official APK. It was neither installed nor executed. No application binary or decompiled listing is included in this repository.

`command_protocol_lib/wifi&ble/symphony2.0_E1.dart`, `buildSmearHEXCommand` at `0x29a8404`, constructs this body:

```text
E1 04 isPreview mode direction density brightness pointCount_hi pointCount_lo
[flags HS_hi HS_lo value speed] ...
```

The implemented settings are preview=1, static mode=0, direction=1, density=80, pointCount=153. Each RGB record uses flags=0, `HS=(hue_degrees<<7)|saturation`, value and saturation in 0..100, speed=100. Header brightness is applied once; RGB scene intensity stays in each record. The complete body is 774 bytes. Source: [independent encoder and byte tests](../src/lighting/door.rs).

The app's `com.zengge.hagallbjarkan.protocol.zgble.LowerTransportLayerEncoder` implements V0 fragmentation: first header 8 bytes, continuation header 5 bytes, shared sequence, increasing fragment index, high bit on the last index. Its Android connection stores negotiated MTU minus four. Prismestra conservatively uses `min(MaxPduSize-4,255)`; at PDU 200 the writes are 196/196/196/196/18 bytes. Source: [fragment encoder](../src/lighting/door.rs), [Windows submission](../src/transport/ble_windows.rs).

E1 0D also contains a selected-index bitmask in the app, but it is not used by this adapter. Increasing the legacy E1 03 array to 153 was not accepted as proof of individual addressing; the observed marker was approximately a group of 6–8 LEDs. Source: [sanitized hardware observation](validation/surplife-153.json).

## Hardware and failure boundaries

A controller reporting product 0xAA, LED protocol version11 and one segment of 153 configured points accepted the E1 04 candidate. In a stationary marker0 test on 2026-11-13 the observer explicitly confirmed one physical LED. This establishes single-LED resolution on that kit; it is not an exhaustive optical test of all 153 positions, other firmware or other controllers. Source: [hardware observation](validation/surplife-153.json).

The encoder sends preview colors and the existing off command. It sends no LED-length, IC-type or firmware configuration commands. The host's preview flag does not independently establish the firmware's EEPROM behavior. All fragments are prepared before the first write; cancellation or a failed fragment aborts the frame without replaying its tail. An eight-second budget covers the whole frame, including in-flight waits. A black frame uses the existing off command. Source: [bounded frame submission and failure tests](../src/transport/ble.rs).

The 10 Hz preset is a requested output cadence, not a measured optical LED refresh rate. The mailbox keeps the latest frame when hardware is slower. Source: [output worker](../src/runtime/output.rs).
