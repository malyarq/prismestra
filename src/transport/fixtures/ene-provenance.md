# ENE native C++ transaction oracle

`ene-native.json` contains only synthetic memory-bus transactions. It was emitted by the actual unchanged OpenRGB ENE controller and I2C/SMBus interface sources compiled with a memory SMBus mock. No PawnIO DLL, driver, kernel, real SMBus adapter, SPD data, user configuration or live device is accessed.

## Sources

Directory relative to repository root:

`../rgb-openrgb-2026-11-05/core-source-1.1/Controllers/ENESMBusController/`

| Exact source file | SHA-256 |
| --- | --- |
| `ENESMBusController.cpp` | `d7719b7ba1e4e62789f40ae8878ba1324fd64d2b871315bf540788cc457aa17d` |
| `ENESMBusController.h` | `da599c5fd3ab26e56b44f80ef8df2519e4fe4189d282e0d8bc4a90cda41cbf32` |
| `ENESMBusInterface/ENESMBusInterface.h` | `802feb83a1b44a2085eb7243023a0460fd67ce147e8cd779fadf8f4dc29aa256` |
| `ENESMBusInterface/ENESMBusInterface_i2c_smbus.cpp` | `a1beee34a39d061343b39a8a76122ceaa8390e72093e3cdfd2583a595406b239` |
| `ENESMBusInterface/ENESMBusInterface_i2c_smbus.h` | `f324f95fff1367cc3795e163b3adef72861857bd4ed35accd19083477de130b1` |

The local reproduction bundle in ignored `work/ene-native/` contains byte-identical snapshots, `oracle.cpp`, memory SMBus/RGB/logging mocks, `build_oracle.py`, `check_fixture.py`, source hashes and `check.json`. The native algorithm is included directly, not rewritten in the runner. The Python checker validates the emitted fixture's internal consistency; it is not the native reference implementation.

## Cases and source behavior

The synthetic device version is `AUDA0-E6K5-0101`. Its 64-byte configuration is zero except offsets `02=08`, `03=08`, `13=8A`: eight LEDs, one eight-LED DRAM zone. The source selects direct-color base `8100` and the V1 channel table. Constructor traces include all **16 name reads plus 64 configuration reads**, with each read preceded by an address-selector word operation. Public records cover SMBus device addresses `71` and `73` hex.

For each address and **32 deterministic eight-LED RGB patterns**, the runner calls the actual `SetDirect(1)` then `SetAllColorsDirect`. Each pattern is exercised with successful three-byte block writes and with block writes returning `-1` without making progress: **128 frame cases** total. Pattern 0 is black, pattern 1 white; remaining channels at LED i are `(pattern*53+i*37+17, pattern*79+i*73+29, pattern*113+i*101+43) mod 256`. Explicit `rgb_hex` inputs are stored in every record.

Source `SetDirect(1)` writes `8020=01` and `80A0=01`, a four-operation word/byte transaction. Source `SetAllColorsDirect` arranges each LED in **R,B,G** order, then uses `GetMaxBlock()==3`. Each successful frame has eight selector-word/block pairs (16 operations). Byte fallback has eight selector-word/failed-block/three-byte groups (40 operations). Full 24-byte final register images and final direct/apply values are recorded.

The mock asserts every target is one of the two synthetic addresses. Register reads are limited to `1000..100F` and `1C00..1C3F`; data writes are limited to `8020`, `80A0`, `8100..8117`. Before/after assertions cover the entire 65,536-byte memory image, ensuring all other bytes remain untouched. The runner never calls mode save, effect writes, slot/address remapping, fan, SPD or discovery operations.

## JSON schema

Top-level fields: `schema=1`, `name`, `config_hex`, `constructors`, `frames`.

Constructor record: `address`, `trace`, `version`, `leds`, `zone_leds`, `zone_name`.

Frame record: `address`, `pattern`, `block_supported`, `rgb_hex`, `direct_trace`, `frame_trace`, `final_hex`, `direct_value`, `apply_value`.

Trace operations use decimal integers and hexadecimal byte strings:

- `["W", address, command, value]`: SMBus word write; source command 0 selects the ENE register using its byte-swapped address.
- `["R", address, command, result]`: byte-data read; source command `129` (`81` hex).
- `["Y", address, command, value]`: byte-data write; source command 1, with memory pointer auto-increment.
- `["B", address, command, "HEXBYTES", result]`: block-data write; source command 3 and result 0 or -1. In this mock -1 means unsupported with no bytes committed and no pointer progress.

The upstream interface falls back after **any** block failure and does not reset its pointer before byte fallback. That behavior is not a safe reference for partial failures. A stricter Rust port may fall back only on an explicit Unsupported result, reset the pointer, and stop on other failures. Compare exact successful-block traces against the oracle; compare fallback **final images** when the stricter port adds pointer resets. Do not require unsafe failure behavior to match the native source.

## Reproduction and checks

```powershell
python ./work/ene-native/build_oracle.py
python ./work/ene-native/check_fixture.py
```

The builder uses MSVC C++17 with a process-local minimal PATH. It preserves existing source snapshots/fixture on drift. Existing source conversion/`strcpy` warnings are retained. Fixture SHA-256: `1f4a7ece47ab7c7bdee9d465d85af82a0e79ebf89272d95755d6f6242eb5a2e8` (109,892 bytes).

Finite checks: two complete constructor traces (320 operations); 128 direct/frame cases (4,096 operations), 512 successful block operations, 512 explicit unsupported block operations; 64 address/pattern pairs with equal block/fallback final images. This is source/CPU proof only. Root owns `src/transport/ene.rs` and its Rust differential tests; this oracle task does not claim those tests, real bus behavior, runtime or physical LED acceptance.
