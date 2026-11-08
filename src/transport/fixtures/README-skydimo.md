# Skydimo native fixture provenance

`skydimo-native.tsv` is public synthetic test data produced by compiling the **actual unchanged** OpenRGB Skydimo driver with memory-only serial mocks. It contains no machine identifiers or captured hardware data. The Rust module tests use this checked-in fixture directly; they do not require the ignored `work` directory or a C++ compiler.

Upstream source snapshot, relative to repository root:

`../rgb-openrgb-2026-11-05/core-source-1.1/Controllers/SkydimoSerialController/SkydimoSerialController.{cpp,h}`

SHA-256 of exact source bytes:

| Source | SHA-256 |
| --- | --- |
| `.cpp` | `1642d5b4b9748bf99d334c8af45adb253b8f6c37dd31cdc55d18b736432cdbab` |
| `.h` | `110c7bb2ae52f870d966af6335b1bbc0eb308d52955a39abf85ce904f19dcfd1` |

The local reproducibility bundle is `work/skydimo-protocol/`: `build_oracle.py`, `oracle.cpp`, `mocks/`, exact `oracle-source/` snapshots, original full `oracle.tsv`, hashes, and standalone Rust harness. These local work artifacts are ignored by Git. The runner includes the actual `.cpp`, thereby executing its table, `QueryInfo`, `ConfigureOutput`, `SetMatrixCell`, and `WriteFrame`; no driver algorithm is rewritten as the oracle. Mock writes cap each write at seven bytes; all reads and writes remain in memory.

Generate the fixture from the original source, then run the portable Rust test:

```powershell
python ./work/skydimo-protocol/build_oracle.py
cargo test transport::skydimo
```

The local builder uses MSVC C++17 through `C:/Program Files/Microsoft Visual Studio/18/Community/VC/Auxiliary/Build/vcvars64.bat`, with a process-local minimal PATH to avoid an overlong inherited environment. It records and refuses source/fixture drift. Initial complete native output SHA-256: `84c7f72a0f685175a7ab7cdf5463656f897df6f2fea67a6904690a8c299f2845`.

TSV schema:

The final empty matrix field for strip/resizable models is a semantic trailing tab. `.gitattributes` preserves this fixture byte-for-byte and permits that empty final column.

- `M`: ID, layout enum integer, initial/min/max LED counts, resizable flag, width, height, comma-separated cell positions in increasing native LED-index order. This is a lossless sparse encoding of the full row-major map; the Rust test reconstructs every cell including holes. There are **42** native model records.
- `P`: count, full uppercase hexadecimal packet. Counts: **1, 2, 51, 77, 150, 253, 255, 256, 257, 300, 1024**. Input channels at index `i`: `(37*i+17, 73*i+29, 101*i+43) mod 256`.
- `I`: hexadecimal synthetic response, success flag, model, serial. Seven cases cover casing, default serial, binary serial/NUL/comma bytes, CR/LF trimming, short/wrong-prefix rejection.

Deliberate Rust validation differences: raw response at most 63 bytes; exactly six ASCII alphanumeric model characters with SK prefix, followed only by an optional immediate comma and binary serial; frames require 1..=1024 colors. Valid unknown IDs parse but model lookup returns `None`. These stricter rejection boundaries have separate Rust assertions rather than a claim of C++ equivalence.

This proves finite protocol/layout behavior in CPU tests. It does not establish real serial transport, device discovery, physical LED direction, or visual appearance.
