# Valkyrie RGB source and finite oracle evidence

Rust `transport/valkyrie.rs` ports only full-frame RGB from neighboring OpenRGB 1.1 `Controllers/ValkyrieKeyboardController`. Original authors: Nollie (Nuonuo), Bartholomew Ho (imnotmental); GPL-2.0-or-later. Version-specific source and fixture hashes are in [valkyrie-sources.json](valkyrie-sources.json).

| Model ID | Source detector | Interface | LEDs | Matrix |
|---|---|---:|---:|---|
| `vk99` | Valkyrie VK99 | 2 | 102 | 22x6 |
| `vk99-pro` | Valkyrie VK99 Pro | 3 | 98 | 22x6 |

Both source registrations require VID05AC/PID024F, usage page FF13, usage1 (`ValkyrieKeyboardControllerDetect.cpp`). These are shared family identifiers, not proof of a particular physical keyboard. The source defaults all non3 interfaces to Normal; Rust deliberately accepts only exact interface2/3 and matching HID path MI02/MI03, rejecting unknown interfaces.

## Wire contract

The unchanged source uses **feature reports only**, each 65 bytes including report ID0. No `hid_write`, input-report read, firmware, macro or persistent-mode operation is implemented. Input/output report lengths are not source-derived requirements and are intentionally not invented.

One RGB frame in `ValkyrieKeyboardController.cpp::SendColors`:

1. Initialize SET_FEATURE: zero-filled, bytes1=04,2=20,9=08; sleep1ms; GET_FEATURE with zero-filled buffer/reportID0; sleep1ms.
2. Seven SET_FEATURE data chunks, 16 groups per full chunk. Each group is `[native_key_code,R,G,B]` starting at byte1; final chunk has six groups for Normal or two for Pro; all remaining bytes are zero. Sleep1ms after each chunk.
3. SET_FEATURE all zeros; sleep1ms. SET_FEATURE with bytes1=04,2=02; sleep1ms. GET_FEATURE with zero-filled buffer; sleep1ms, then sleep33ms.

Successful totals: **10 SET_FEATURE/650 bytes, 2 GET_FEATURE/130 bytes**, 12 requested sleeps of1ms plus33ms. Requested delay sum45ms is source behavior, not a measured hardware throughput or guaranteed timing.

Rust requires exact model-sized RGB input and complete matching descriptor caps: feature length65, every returned feature report ID0, complete nonempty cap list. Unknown/incomplete caps fail closed. The source ignores HID return values; Rust checks each result/count and report ID, abandons the frame on any error/short result, and permanently rejects subsequent frames on that controller. It does not invent an ACK payload; response bytes1..64 are not interpreted. Transport Drop owns handle cleanup; partial frames are not retried or followed by best-effort termination.

## Actual-source oracle

[valkyrie-oracle.cpp](valkyrie-oracle.cpp) compiles the **unchanged actual** `ValkyrieKeyboardController.cpp` and `RGBController_ValkyrieKeyboard.cpp`, plus actual `RGBControllerKeyNames.cpp`. Memory-only `hid_send_feature_report`/`hid_get_feature_report` mocks record all65bytes and call order. Minimal controller-container/StringUtils stubs provide construction/storage; their RGB byte helpers match `RGBControllerInterface.h` low-byte-red definitions. No driver packet builder, key mapping, matrix or detector algorithm is rewritten into the oracle. The real wrapper constructs both matrix layouts and executes `DeviceUpdateLEDs`.

Public [valkyrie-native.bin](valkyrie-native.bin), **70,698 bytes**, has no physical path, serial, MAC, captured traffic or unit identity. It contains only synthetic colors, model tags, actual-source matrices and mock transactions. SHA256 `9c5e17c6adab6cd74c57d6d9a15ca7084aa0aa939d25bf22bdd442eff54fcc8f`.

Format, little endian: magic8 `VK990001`; two model records `(model:u8,leds:u16,width:u16,height:u16,cells:u16[132])`, holeFFFF; case_count:u32; each case `(model:u8,leds:u16,RGB:u8[leds*3],action_count:u8,(kind:u8,packet:u8[65])[action_count])`, kinds S/G. Model0=Normal,1=Pro. There are64 cases (32/model), with black, white, isolated RGB channels, gray gradient, distinct per-LED RGB, alternating colors, first/last impulses and deterministic xorshift colors.

Rust tests compare **640 SET_FEATURE packets and128 GET_FEATURE request buffers** (49,920 report bytes) and all264 matrix cells exactly. All64 controller transactions and pure packet-builder outputs match. Fake transport separately covers error and short result at every one of12 operation positions for both models (48 injected failures), correct stage/count reporting, terminal no-retry behavior, invalid frame zero-I/O, invalid read report ID and descriptor gates. Five standalone test functions pass, including the existing color-encoding test.

Reproduction scripts/stubs/logs are retained in ignored `work/valkyrie-protocol`: `Build-Oracle.ps1`, `mocks`, `extract-arrays.py`, `check.rs`, `native-results.txt`, `test-results.txt`. MSVC native oracle then `rustc --edition=2024 --test check.rs -o check.exe; ./check.exe`; no root Cargo invocation. Original source sleeps are retained; no device APIs are linked by the mocks.

This is finite source/CPU evidence. Actual HID report caps, driver blocking/permissions, physical model/key appearance, native adapter, hot unplug and hardware acceptance remain UNKNOWN/unperformed. Merely sharing VID/PID is insufficient to open or write a device.
