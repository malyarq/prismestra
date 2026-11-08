# Native PIIX4 owner: exact-module evidence and stand scope

`pawnio_adapter.rs` validates the injected LOAD/EXECUTE protocol; `pawnio_windows.rs` implements the dormant native device/global mutex and explicit runtime factory. `windows_io.rs` shares request completion/drain with feature HID. No installed PawnIO device, module load, physical SMBus or production global mutex was invoked during this stage. Native code is compiled and connected to `attach_bound`, not physically accepted.

## Exact pinned module

The input is the existing42404-byte `SmbusPIIX4.bin` with SHA-256 `91f9b4b1c39e3d399ce48477a89d8f6bd3e58a2241064a4daec3a1513dff56e5`. The binary is not redistributed. Public `pawnio-amx-proof.py` reads it as data, verifies size/hash/header and emits only focused static instruction traces. No AMX instructions are executed or emulated. Reproduction: `python src/transport/fixtures/pawnio-amx-proof.py <existing-module-file>`. Root result is saved in ignored `work/pawnio-module-provenance/trace.txt`; first attempt caught a decoder assertion mismatch(names75/counts74), corrected by explicitly excluding variable opcode74/casetbl from linear decoding. It is not a general AMX verifier.

Header layout and opcode/relative-transfer semantics were checked against primary [PawnIO loader](https://github.com/namazso/PawnIO/blob/master/PawnIO/src/amx_loader.h#L309) and [PawnPP AMX definitions](https://github.com/namazso/PawnPP/blob/master/amx.h#L503), read2026-11-06. Header starts at file516, AMX64 magicF1E1/file+AMXversion11/defsize8; COD600/DAT37096; absolute offset is1116+CODaddress. The following addresses are relative to COD. Exact release/commit attribution and installed-driver hash remain UNKNOWN; current master alone is not evidence of this pin's behavior.

| Exact bytes / region | Derived contract |
| --- | --- |
| Public entries6438(port),6818(identity),75F8(SMBus),1008(wait policy) | Names/addresses come from its own public table. |
| Port6468–6488,6500–6520;6620–6680 | Requires input1/output1; accepts signed port−1..4. |
| Primary5558;5AD8–5B18 | `port==-1` jumps to cleanup5E60 before virtual_write_byte/native13 at5D10. Wrapper5F38 aux−1 returns without switching its global base. Query can still clear status through busy-check2258/nativeio_out_byte23E8. |
| Identity6828–6838;68E0–6900 | Input count guard is disabled; input0 or1 accepted. Output requires3. |
| Main8DC8–8E88 -> init1358–2258; helper750–788 | Init uses PCI/MMIO/port reads and map/unmap/debug natives, no port selector, io_out_byte, virtual_write_byte, pci_config_write_word or indirect invoke. Initial VM baseDAT+240=B00. LOAD/main and query−1 do not switch selection; this does not prove general driver safety. |
| SMBus case table8930; value5 at8988 ->8278; helper45F0–4758 | BLOCK_DATA5 accepts input≥9; block length1..32 includes3. Native selects VerifiedThreeByteWrites on this pin; an actual block error is terminal, not evidence of no progress. |
| Wait policy1008; helperD20–D68; initialDAT+216=0(file93C4) | Input1, values0..2. Output is unused, no prior mode returned. Setter calls a helper which only stores the argument in this VM's DAT+216. Root sets2 as local OpenRGB does, with empty output/count0; this is polling wait behavior, not OS sleep/power or another program's cooling settings. |

Craft evidence `ev_e95458292951cc62d229a1f81293d7cc5b0d9c3ad95f019e54e35316eb837fb9` independently records the exact80-byte query branch at file5F34 in this SHA; interpreted with the static trace/header as above. The byte-match tool does not itself prove control flow or runtime compatibility. The bundle is preserved locally at `work/pawnio-module-provenance/craft-evidence.json`.

## Native ownership and validation

The native path is fixed to `\\?\GLOBALROOT\Device\PawnIO`, not a user-supplied device namespace: [Microsoft NT/GLOBALROOT naming contract](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file#nt-namespaces). Its read/write access, share7 and async opening correspond to the local PawnIOLib wrapper ABI in adjacent `pawnio-provenance.md`. No driver installation, service start, signature/security setting, clock adjustment, address scan, port_sel0 or reset command is implemented.

Known RGB-writer checks run before file hashing, under the acquired global mutex immediately before open/LOAD, and after initialization. A detected writer is left running; the new path stops, dropping its result before releasing the guard. This narrows startup races and does not claim to identify every future external writer. The exact mutex is `Global\Access_SMBUS.HTP.Method`; thread-bound Rc bookkeeping prevents moving its guard across threads, rejects recursion/abandonment and publishes a newer epoch per acquisition. Acquisition is nominal250ms; the entire ENE transaction holds its guard. There is no hard total transaction-duration guarantee.

LOAD uses only a production `Piix4Module` hash-verified image. Then the new VM receives wait policy2, identity must be exactly3words with PIIX4/IO baseB00/PCIvendor1022-device790B, and query−1 must return port0. A new lock epoch revalidates full identity and port before any SMBus request; the same epoch avoids repeated checks for each pointer/data pair. Epoch/cancellation are checked around each call. Every buffer is zeroed and actual successful byte counts are validated. ENE pointer/value/address restrictions remain in `ene_bus`; invalid/ambiguous I/O closes the executor without retry or port changes. BYTE_DATA high bits beyond255 are rejected; installed-driver behavior is not asserted from the module alone.

Common overlapped completion retains request memory through cancellation/drain. As already documented for HID, the nominal250ms deadline is not a hard bound on a broken kernel, which must finish/cancel before buffers can be freed. Source-private callbacks immediately return FFI and do not panic after submission.

## Verification

Final237tests passed in debug/release (224unit+7devices+4PCpipeline+2runtimepipeline), all-target Clippy-Dwarnings passed; logs and final source hashes are in `work/pawnio-native-stand`. Eight provider tests include complete fake Device->VerifiedPiix4->EneBus->ENEController color/black/drop flows for71/73. Five Windows tests use only their own Local named mutexes or a freshly created delete-on-close temporary file: clone/epoch/recursion, contention, abandoned/cancelled lock, process recheck ordering/drop-before-unlock, and expected IOCTL errors on that file. Existing own-pipe completion/cancel tests also pass through the shared helper.

Fake module images are available only under cfg(test), only to fake Device tests; they are not valid/loadable production modules. No successful physical kernel LOAD/EXECUTE, production-name lock, hardware identity/caps, measured FPS or CPU/RAM savings is claimed. Full native UI/App lifecycle, sensors/cooling, setup/discovery and packaging acceptance remain separate unfinished requirements.
