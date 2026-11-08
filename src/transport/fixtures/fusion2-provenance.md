# Fusion2 IT5702 direct-RGB subset

The production module is `src/transport/fusion2.rs`. Its checked-in `fusion2-native.tsv` fixture is synthetic CPU data, not captured device data. No real HID path, chip identity, firmware dump, user configuration or hardware traffic is included.

## Source and bounded scope

Source directory relative to repository root:

`../rgb-openrgb-2026-11-05/core-source-1.1/Controllers/GigabyteRGBFusion2USBController/`

Exact SHA-256 hashes:

| File | SHA-256 |
| --- | --- |
| `GigabyteRGBFusion2USBController.cpp` | `00dbd33a58003114087f709c9e6e1d64408307d3ac75cc801b2ccb4ae1d473ad` |
| `GigabyteRGBFusion2USBController.h` | `ec4a58a9ddb9bef1c9dc4907c160960aeea607bf83a8ff86d56d0dba6d831a7c` |
| `GigabyteFusion2USB_Devices.h` | `1c59c99a646da9b4005076c83267e9e3657be4e1f27ee2cd9eaaf08aebb27546` |
| `GigabyteFusion2USB_Devices.cpp` | `6ca12cd3b8d7b986f9cca5a80779e42891e3d0fb03a0bcac3985ec59c45620ad` |
| `GigabyteRGBFusion2USBControllerDetect.cpp` | `3dd88151bf5ac7678cce3ede6df51dfa80ade3599626ab8290a3d4f044b47e49` |
| `RGBController_GigabyteRGBFusion2USB.cpp` | `c5cbabb8e6ae1f92feb2975a871c7dd66eac46b28762c53d63e814438cfcc202` |

The device table's `b650_eagle_ax_device` selects `it5702_7_device`, device number 0. That layout maps its two ARGB zones to `HDR_D_LED1` (index 5) and `HDR_D_LED2` (index 6), plus `common_led_c_5_zone` at index 4. The first ARGB zone is named `ARGB_V2_1/ARGB_V2_3` in the source, reflecting physical headers sharing a controller zone. ARGB zones use the source IT57xx maximum of 256 LEDs. This port deliberately supports only this two-header IT5702 family; it does not copy the full motherboard/model table.

The detector registers VID `048D`, PID `5702`, usage page `FF89`, usage `00CC`. Although it defines interface constant 0, this detector registration does not use it: the required HID interface number is **UNKNOWN**. Source calls feature send/get with total buffer length **64**, including report ID `CC` at byte 0. Input/output report capabilities are **UNKNOWN**, and neither API is used here. Native HID capability checks, explicit path ownership and OS timeout handling belong to the host adapter.

## Protocol and intentional safety differences

Source methods: `RefreshHardwareInfo`, `SetLedCount`, `SetStripBuiltinEffectState`, `SetStripColors`, `SendCCReport`, `SendPacket`; structures: `IT8297Report`, `PktRGB`.

`query(&mut Transport)` sends `CC 60 00 00` padded to 64 bytes, then reads a 64-byte feature report requested with ID `CC`. It parses the packed little-endian identity fields, six length nibbles, and existing per-header R/G/B byte positions. It sends no lighting-mode/count/color command. Invalid/off/nonpermutation calibration, bad report ID, unexpected controller number, invalid division codes, and short/oversized transfers fail closed.

`Controller::connect(transport, [count0,count1])` owns both ARGB channels. After the same query, it sends `CC 34` only if either owned count division changes, preserving the other four division nibbles. It then sends `CC 32 03` and requests the source's 50 ms settling delay. The mask 3 disables built-in effects for the two ARGB channels; LED_C is outside that mask. One-header-only ownership is not implemented because the source query does not expose the previous direct/built-in mode mask.

The native constructor also disables LampArray when supported, resets effect registers `20..27`, applies every zone, and disables Beat. Those global operations are **deliberately omitted** because they could affect LED_C. No reset, hardware-effect/apply, Beat/LampArray, calibration-write, persistent-lighting, flash, Gen2 scan or firmware commands are exposed by the driver's transaction path.

`update(0|1, colors)` sends only the selected header's `58`/`59` packets. Each report carries at most 19 LEDs, a byte offset in little-endian bytes 2..3, payload byte count at byte 4, and calibrated channels starting at byte 5. The native implementation reuses the packet buffer, so unused final-packet bytes retain previous-chunk data; the fixture and Rust builder preserve that exact behavior. The other channel's submitted frame and cached intent remain unchanged. LED_C is never addressed by this transaction sequence.

Every send/get requires exactly 64 bytes. There is no source-backed device-level frame ACK or RGB readback: completion means feature-transport submission only. A partial/failed frame makes the controller terminal, drops its own transport, preserves prior successful frame metadata, and performs no automatic retry/reset/blackout. `stop` only drops its transport.

## Actual C++ oracle and checks

The local reproduction bundle is `work/fusion2-protocol/`: builder, `oracle.cpp`, HID/RGB mocks, byte-identical source snapshots, `source-sha256.json`, standalone Rust harness and `check.log`. This local bundle is ignored by Git; ordinary Rust tests require only the checked-in public fixture.

The oracle includes the actual unchanged controller `.cpp` and header, including its real calibration lookup and packet methods. Only HID I/O and RGB helper types are mocked; all feature requests remain in memory. The mock native constructor runs against synthetic information. The runner retains its actual query packet but discards the deliberately excluded global constructor initialization writes, then calls real `SetLedCount` and `SetStripBuiltinEffectState(-1,false)` to obtain the bounded initialization reference. Direct packet references call actual `SetStripColors`.

Commands:

```powershell
python ./work/fusion2-protocol/build_oracle.py
rustc --edition 2024 --test ./work/fusion2-protocol/standalone.rs -o ./work/fusion2-protocol/tests.exe
& ./work/fusion2-protocol/tests.exe --nocapture
```

The C++ builder uses MSVC C++17; the original source emits existing anonymous-struct/conversion warnings. Fixture SHA-256: `32e7e4d5140f9726b1ffecd61d2c3a5a6c735c499b45e2c73c90cee718380bbc` (53,652 bytes).

Fixture records: six synthetic identity reports (`I`), six bounded initialization sequences (`N`), and **108** direct packet sequences (`P`: six calibration permutations times two channels times nine counts: 1,19,20,27,32,33,64,65,256). RGB at index i is `(37*i+17,73*i+29,101*i+43) mod 256`. All reports are complete uppercase hex, preserving every byte.

Standalone result: **7 tests passed** (six Fusion2 tests plus existing color encoding test). Checks compare complete native packets and safe initialization order, all permutations/header/boundaries, identity fields, unchanged unowned count nibbles, independent channel updates, no LED_C/global/persistent command, short/error terminal behavior, handle drop, invalid initialization and packet bounds, and isolated read-only query.

Remaining **UNKNOWN**: physical response, LED appearance/direction, the actual host's HID capabilities, and whether a device currently in a global LampArray/Beat state accepts direct output without a global toggle. The safe initialization variant has not been tested on real hardware; source/CPU equivalence of its selected operations does not prove complete native-constructor equivalence or physical readiness.
