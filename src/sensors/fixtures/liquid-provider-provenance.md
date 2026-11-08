# Liquid: repeated observation through explicit MI00

This is a source-backed adapter over the existing verified TT status reader. No hardware, native helper, HID enumeration or data open was executed. Default UI remains the software stand.

## Local source pins

Paths relative to the repository root, under `../rgb-openrgb-2026-11-05/tt-lcd-plugin/`:

| File | SHA-256 |
| --- | --- |
| LiquidTemperature.cpp | 055f889e8118a40df922f8b0b0d7639ae637d307ac0c4e2123d1670b33de96d5 |
| LiquidTemperature.h | ec4e1d2c68b22b922ac8ed05e10698fb2a9e9d2fa663df04cc7a2d8d11c94b70 |
| Plugin.cpp | b9f51fcd36c4175f0b88636d2c36d699a0d48c7f07563bde634c47b1c782a0fe |
| HidTransport.cpp | b29510a24dc64f9c3aa9e7287d071f7aa0e13037a280f85d01c5804fc5cd215a |

`LiquidTemperature.cpp:106–119` opens a new exclusive overlapped MI00 handle for each single status exchange; the local RAII object at lines17–20 closes it on return/error. `Plugin.cpp:51–55` and `LiquidTemperature.h:19` gate attempts by at least2000ms from the previous attempt's start, including failures. `HidTransport.cpp:135–143` separately retains the MI01 LCD handle. Repeated queries on one MI00 handle are not established by this source; the existing Rust native one-shot flags remain unchanged.

`LiquidTemperature.cpp:97–102` and Rust `lcd::temperatures` define the441-byte request/response `00 82 01 00 80`, LE-u16 bytes5–6 divided by100 and `1 <= Celsius < 100`. There is no source-backed extra flush, nonce, second read or command to add. Existing codec/native differential fixtures remain applicable; this change does not modify the wire format.

## Rust integration

`app::sensor_sources::attach_liquid` requires an explicit validated MI00 path and existing logical input. A common `hid:<ASCII-lowercase path>` lease is held by the actual sensor owner thread, including between one-shot opens. The backend is constructed there and need not be Send. Each poll uses `hid_adapter::read_liquid(Some(target), false)`, retaining its process/caps/identity guards and one-shot resource lifetime.

The source adapter requests2000ms through `Provider::poll_interval_ms`. Worker validates this metadata once in100..60000ms and waits after each completed attempt; thus it is conservatively at least the source's minimum start-to-start gap. NVIDIA and other providers retain the default1000ms. There is no alternating not-due `None` which would clear a still-fresh observation. Query-start monotonic time is retained as an observation time; it is not a hardware acquisition timestamp.

`attach_native_liquid` is dormant and is never invoked by document loading, default UI or tests. MI01 is not opened by this helper. The two paths are not derived from one another. Neither the original source nor the current Descriptor establishes common Container ID, parent or serial: physical pairing to a particular LCD must not be claimed.

## Stand evidence and limits

`app::runtime::tests` exercises real policy selection among two synthetic valid candidates, the actual441-byte request/parser and sensorworker/runtime selection. Each test has unique synthetic endpoints. Cases cover two-second polling without timestamp refresh, one write/read/open/close per attempt, bad header clearing and recovery, external-writer guard before open, wrong-interface rejection, case-folded lease conflict, and blocked MI00 read while the actual LCD Controller/MI01 adapter continues JPEG/ACK frames. Detach rejects the late result and releases the status link. No physical USB interface was used.

Physical simultaneous MI00/MI01 operation, wall-clock driver latency, and the freshness of a firmware/driver response without a nonce remain UNKNOWN. The existing overlapped adapter must drain kernel I/O before freeing its buffers; nominal timeouts cannot promise bounded completion under a broken driver. A same-shaped response is not proof of a newly acquired physical temperature. CPU/RAM/ITE providers, cooling control/failsafe and local-binding UI remain separate work.
