# DDR5 temperature: explicit PIIX4 binding

Source-derived implementation, tested with injected devices only. No DIMM, driver,
module, CPUID or native sensor factory was opened or executed.

## Pinned sources

RAMSPDToolkit-NDD 1.6.4 nuspec identifies repository commit
`198556be922df8aa870b0c37fd6e8b23cd02d0fd`:

- [DDR5Accessor.cs](https://github.com/Blacktempel/RAMSPDToolkit/blob/198556be922df8aa870b0c37fd6e8b23cd02d0fd/RAMSPDToolkit/SPD/DDR5Accessor.cs), lines129-220,283-328,374-391: hub, temperature and capability checks.
- [DDR5AccessorBase.cs](https://github.com/Blacktempel/RAMSPDToolkit/blob/198556be922df8aa870b0c37fd6e8b23cd02d0fd/RAMSPDToolkit/SPD/DDR5AccessorBase.cs), lines224-277: page selector and recovery delay.
- [SPDTemperatureConverter.cs](https://github.com/Blacktempel/RAMSPDToolkit/blob/198556be922df8aa870b0c37fd6e8b23cd02d0fd/RAMSPDToolkit/Utilities/SPDTemperatureConverter.cs), lines23-39: f32 conversion, sign bit0x1000, scale0.0625, no byte swap. Copyright2025 Florian K., MPL-2.0.
- [SMBusPawnIO.cs](https://github.com/Blacktempel/RAMSPDToolkit/blob/198556be922df8aa870b0c37fd6e8b23cd02d0fd/RAMSPDToolkit/I2CSMBus/SMBusPawnIO.cs), lines74-151: five-word request, BYTE_DATA2/WORD_DATA3.
- [SmbusPIIX4.p](https://github.com/namazso/PawnIO.Modules/blob/52a7e536dff3e53c96917a28caac5e0fa6510696/SmbusPIIX4.p), lines439-520,636-701: controller/port, auxiliary VM base, transfer and output policy.

Module pin:42404 bytes, SHA256
`91f9b4b1c39e3d399ce48477a89d8f6bd3e58a2241064a4daec3a1513dff56e5`.
Same verified module as the existing ENE boundary; no module binary is bundled here.
Local acquisition hashes/package metadata: `work/ddr5-source/manifest.json` and
`verify.py` (25 files). Source/code licensing and distribution still require the
overall product licensing audit; no public release readiness is implied.

## Implemented boundary

`ddr5::Binding` keeps all three controller words, port0/1 and one address50..57.
The high32 bits of controller[2] are subsystem identity, not revision.
Primary requires its existing port0; auxiliary1 changes only this VM's base toB20.
No physical port switch or scan is permitted. Before page selection the transaction
checks controller, port, hub00=51/01=18, capability05 and enabled1A=0. Only page0B=0
may be written, with validated recovery, readback and repeated hub checks.
WORD31 must return exactly8 bytes and fit u16. ENE70..76 policy is unchanged.

The shared SMBus guard covers the whole transaction, including recovery wait.
Cancellation, epoch, known conflicting writers and identity are rechecked at
boundaries; failure closes the device before unlock. Busy lock does no I/O.
The underlying SMBus module can write bus status/command and temporarily enable
PCI I/O decoding: this is not a zero-write hardware path.

Provider polling is1Hz, timestamp-preserving, with finite range-20..150C. Errors
clear cached data; a real raw0 remains valid0C. Clock/binding drift is terminal.
`attach_native_ddr5` is dormant and explicit; portable profiles contain no binding.

## Verification and remaining unknowns

5 pure-provider tests,13 transaction/fault/wire tests and a runtime integration test
exercise two providers -> RAM mean -> real LCD compositor/JPEG -> memory TT ACK.
The integration test verifies35C mean, failure removing mean, independent surviving
sensor and valid0C after detach/drain/reattach. Entire suite392 tests passes;
all-target Clippy passes with warnings denied (`work/ddr5-source/`). Scoped static
review found no proven P0-P2; it did not test hardware.

Native driver returned-length/ABI compatibility, physical DIMM-to-address pairing,
external programs obeying the same lock and actual device behavior remain UNKNOWN.
I801/NCT/IMC, DDR3/4, hidden-hub recovery and writes to sensor enable/thresholds/SPD
NVM are unsupported here. No guessed mapping or fallback is activated.
