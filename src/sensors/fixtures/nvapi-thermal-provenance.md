# Own NVIDIA GPU Core provider: source and CPU evidence

Scope: read-only GPU Core through an injected backend and a dormant Windows x64 NVAPI adapter. This is not FanControl cached IPC, full GPU telemetry, CPU/RAM sensing, or cooling control. No native backend/DLL was invoked to produce this evidence.

## Exact local sources

Relative to repository root:

`../rgb-openrgb-2026-11-05/core-source-1.1/dependencies/NVFC/`

| File | SHA-256 |
| --- | --- |
| `nvapi.h` | `1cd791d34799fcea408bc12bf4e1cc7662fa792c7a37fa3e91470e48597a2f41` |
| `nvapi.cpp` | `250c9a02509cf9e6387a8ff22ec9c12e806ed4b6b77f555a05c6244055636f13` |

Relevant source:

- `nvapi.h:193,213`: signed target enum, GPU=1/MEMORY=2/ALL=15, thermal V2 structure with three sensors. Raw enum fields use Rust i32 to avoid invalid-enum undefined behavior before validation.
- `nvapi.cpp:61`: actual thermal constructor zeroes the entire structure and sets `NV_STRUCT_VERSION(...,2)`.
- `nvapi.cpp:112,121,186,229,282..307`: C function-pointer signatures and interface IDs. Only Initialize `0150E828`, EnumPhysicalGPUs `E5AC921F`, GetPCIIdentifiers `2DDFB66E`, GetThermalSettings `E3640A56` are resolved by this port.
- `nvapi.h:954`: thermal selector is a target, not array index; the native port requests GPU=1.
- `nvapi.cpp:489,564`: thermal/PCI wrappers. An absent symbol returns an error rather than valid data.
- `i2c_smbus/Windows/i2c_smbus_nvapi.cpp:168..197` in the same core source tree: 64-handle enumeration storage and PCI vendor/device word layout. Referenced only for ABI bounds/identity; no I2C functionality is ported or called.

These copied source files do not contain an independently established license grant for every NVFC element. No broad third-party redistribution/license claim is made here; packaging review remains separate.

## Actual CPU oracle

```powershell
python ./src/sensors/fixtures/make-nvapi-thermal-fixture.py
```

The builder verifies both source SHA-256 pins, includes the actual header, extracts the exact thermal constructor definition from the actual `.cpp`, and compiles `nvapi-thermal-oracle.cpp` with MSVC C++17. It does not compile/link the NVAPI loader or any vendor DLL. The resulting executable instantiates only that structure and prints layout, enum values and every initialization byte. Generated compiler files live in a checked, temporary child of this fixture directory and are removed afterward. System sandbox TEMP was unwritable during the initial attempt; this fixture-local temporary directory was used instead.

Observed CPU ABI: size **68**, alignment **4**; each sensor **20** bytes. Outer offsets version/count/sensor: **0/4/8**. Sensor offsets controller/min/max/current/target: **0/4/8/12/16**. Initialized version **0x00020044**, count/sensors all zero. `nvapi-thermal-native.json` SHA-256: `ebaae4b3e61077bbb75b0fb4941a7864b220d167f70301bb5b6b9033a9ccd93c`.

The Rust inline check compares all offsets, sizes, version, GPU target and all initialized bytes against this checked-in native fixture. Other fake checks cover zero as a valid temperature, bounds, absent/duplicate targets, count overflow, wrong version/identity, I/O failure without parsing bad data, monotonic/overflow rejection, one-second retry cadence and stale cached timestamps. Rust tests are not claimed executed by this source task; root owns the integrated test run.

## Rust policy

`sensors::Reading` carries Celsius and the conservative query-start monotonic timestamp. `is_fresh(now,max_age)` requires a nonzero observation, finite temperature and a nonnegative age within the caller's limit. The caller chooses freshness TTL; there is no fabricated hardware acquisition timestamp.

`nvidia::Provider<Backend>` takes a separately supplied `PciIdentity`, verifies it at construction and before each due thermal call, and polls at most once per 1000 ms, including failures. A not-due poll returns None; `latest` may expose a still-fresh previous observation with its original timestamp. Missing GPU target returns None; errors clear old readings. Backward/zero/overflowing clocks and changed identity close the owned backend. Ordinary read/parse errors may recover only at the next eligible poll. Output is freshly zeroed/versioned before every backend call; error outputs are never parsed.

PCI identity includes all four source fields and deliberately has no serde implementation. Physical identifiers/module locations do not enter portable profiles through this module. Duplicate full PCI matches are rejected; there is no first-GPU/index-zero fallback. This PCI tuple can be ambiguous for identical boards, in which case the user must choose a future stronger supported binding rather than this implementation guessing.

## Dormant native adapter and remaining limits

Only explicit `nvidia_windows::NativeBackend::open(expected_identity)` performs native work. It rejects invalid PCI bindings before loading; obtains the system directory from Windows; requires a regular, non-reparse System32 directory/DLL; loads the absolute `nvapi64.dll` with `LOAD_LIBRARY_SEARCH_SYSTEM32`; verifies the loaded module's canonical path; and retains its own HMODULE/function pointers until Drop. It is owner-thread-bound and !Send. All NVAPI outputs start zeroed; nonzero statuses are errors, counts/null/duplicate handles fail closed, and each runtime identity read uses the selected original handle. No automatic handle re-enumeration/fallback is performed after a failure.

The path checks trust the OS-managed System32 location; they are not Authenticode or a driver-binary hash pin. Hardware/driver compatibility, individual calls' wall time/cancellation behavior, and native initialization/global teardown behavior remain **UNKNOWN**. Drop balances only this object's LoadLibrary reference through FreeLibrary; NvAPI_Unload is outside the requested four-function allowlist. No clock/power/fan/voltage/I2C setters or driver installation APIs are exposed. GPU memory/hotspot/fractional-temperature parity and all other cooling providers remain separate work.
