# AMD Tctl: exact image, arithmetic and dormant executor

No CPUID/topology query, PawnIO device open, module/driver load or physical PCI/SMN operation was performed. Tests use fake Device/Mutex/provider and synthetic topology. The arithmetic oracle is an independently built CPU-only program, not the module or a vendor DLL.

## Pins and source contract

LibreHardwareMonitor commit `2808d80564b6761e1b01f42fabfd966ed695fcbc`:

- [AmdFamily17.cs](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/blob/2808d80564b6761e1b01f42fabfd966ed695fcbc/LibreHardwareMonitorLib/PawnIo/AmdFamily17.cs), SHA256 `d47cbc1e8469dd7e1b8b6cbbd798c298df62ca59229de0f0d7163ac869d6f970`: literal `ioctl_read_smn`, one offset/one returned64-bit cell.
- [Amd17Cpu.cs](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/blob/2808d80564b6761e1b01f42fabfd966ed695fcbc/LibreHardwareMonitorLib/Hardware/Cpu/Amd17Cpu.cs), SHA256 `8a6dba06cd584c91043f13bc4555fb43ec095534c80ceb00afa31e62d2b082e3`: register59800, integer `(raw>>21)*125`, float32 multiply by0.001, then float32 addition of-49 when RANGE_SEL bit19 or both TJ_SEL bits17:16 are set. Tctl is distinct from legacy model-specific Tdie corrections. Baseline Ryzen7700 uses the no-extra-offset Tctl/Tdie case; actual local CPUID was not queried.

PawnIO.Modules release0.2.11, commit `52a7e536dff3e53c96917a28caac5e0fa6510696`: [AMDFamily17.p](https://github.com/namazso/PawnIO.Modules/blob/52a7e536dff3e53c96917a28caac5e0fa6510696/AMDFamily17.p), SHA256 `8a1808a2e51e184f96d17362991ae900c65a1a23135a68c256ec6759e8297bb9`. Official [release ZIP](https://github.com/namazso/PawnIO.Modules/releases/tag/0.2.11) digest matched GitHub asset metadata. Extracted AMDFamily17.bin is10652bytes, SHA256 `dae74615761b78bdf064dfb3e136252ddcc6fc727d88f14738d0e5800d427a91`; module bytes are not bundled here.

The operation requires `Global\Access_PCI`. It reads PCI BDF00:00.0/vendor1022, writes SMN **index**0x60 and reads data0x64. This is a temperature-reading capability with a necessary pointer write, not absence of hardware writes. No fan/MSR/write export is exposed by the Rust thermal executor.

## Exact binary proofs without execution

`amd-module-proof.py <dedicated-source-directory>` reads the pinned image and `vm.cpp` (PawnIO commit `9d52965895588b0ffa3703b72eec75ba19f4ccc0`, SHA256 `bb56cb7da7da602da1413cbac27a00a2063cb7707c70d15d0f65a74ff1ae4bb1`). It verifies RSA4096/PKCS#1v1.5/SHA256 against the pinned `k_pubkey_namazso_2023` BCRYPT_RSA1 blob, exponent65537, key-blob SHA256 `724af6ac42e7a4cbbbe3e4a5a0117af0fd98364a72c2aee40972228838400e59`. Five tamper/truncation checks reject modified input. Result: `amd-module-rsa-proof.json`. This proves that exact signature against that key, not current installed-driver trust policy.

The same script statically traverses direct control flow and callees, without execution/emulation. AMX64 magicF1E1/version11, payload516, COD280/DAT9856. Main/CIP21a0 reaches298 instructions and only get_arch/cpuid/debug_print natives, with AMD/x64/family17h..1Ah gates. ReadSMN entry1ad8 reaches134 instructions: exact counts1/1, AMD vendor guard, index60/data64, early status returns; no reachable MSR write and no unresolved branches. Fixed-pin operand checks and limitations are in `amd-module-cfg-proof.json`; driver callback/native implementations are outside this proof.

Craft Anything independently observed the selected176-byte indexed-call block exactly once at file0x21CC in this image: evidence `ev_6be5ef70b2572075b8468fea2acca59d5d03c3543902632cf22e41c4ce03d6d1`, full result in ignored `work/amd-runtime-stand/craft-evidence.json`. The initial80-byte function prefix also occurs at another export and is not unique. These are file-byte observations; the separate script supplies CFG interpretation, and neither observation executes the module.

`amd-tctl-make.py` and `amd-tctl-oracle.cpp` reproduce the pinned float32 arithmetic for16384 code/flag combinations. `amd-tctl-f32-bits.bin` SHA256 `2d04cec24495a97de554333528531749064d29e48b81ae2f343286b6451484a0`; Rust compares every result bit. Runtime provider rejects observations outside-20..150C, errors do not produce zero, and a successful raw zero is separately tested as0C.

## Native policy and stand scope

The dormant native backend takes a separate non-serialized vendor/raw-CPUID-signature/single-package binding. On Windowsx64 it checks topology/identity before module load and under the PCI guard before/after readings. It opens only the existing PawnIO device, never installs/starts a driver. A module must pass the exact image hash. The common bounded overlapped executor is reused without changing its accepted LOAD/EXECUTE shapes; previous RGB policy remains unchanged.

The thermal request is a fixed40-byte literal `ioctl_read_smn(0x59800)`. Read result must be exactly8 successful bytes and fit a DWORD. Each read requires a newer live mutex epoch. IO/count/epoch/identity faults close the owned device before leaving the guard, and later calls cannot silently resubmit. Lock failure has issued no IOCTL and may recover later. Source provider keeps1Hz cadence, original timestamps, and clears missing/failed data. Only explicit `attach_native_amd` creates this native capability; no document/default UI/test invokes it.

Tests cover fake LOAD/read/close order, absent guard, wrong output counts/high bits, changed epoch/identity, cancellation, busy lock without I/O, and the full fake Device→verified executor→AMD provider→sensorworker→Runtime logical CPU chain. Kernel errors clear data and do not trigger hidden reloads. Synthetic Windows topology records are parsed with SDK offsets, bounded groups/masks and aligned storage; native topology/CPUID functions are not called by tests.

Installed-driver compatibility/latency, firmware values, multi-package systems, legacy Tdie offsets, CCDs, Intel/other CPU families and physical acceptance remain unproven or unsupported. DDR5/ITE/cooling control/failsafe are separate work. No CPU/RAM savings are claimed.
