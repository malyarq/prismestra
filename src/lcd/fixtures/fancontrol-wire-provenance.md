# Synthetic FanControl wire corpus

`fancontrol-native.bin` is actual pure C++ output from original TemperatureSensors.cpp, using TT_SENSOR_TEST and TT_LCD_OFFLINE_TRANSPORT with QtCore6.8.3. No IPC, installed process, hardware IDs, private profiles or user paths in public source/fixtures. `fancontrol-wire-oracle.cpp` takes FANCONTROL_SENSOR_HEADER macro and links original source. Generation details/hashes: work/fancontrol-wire/REPORT.md and manifest.json.

Format: all integers LE. `FWN1` + count:u32(60). Each op:u8:0=request,1=payload,2=packets. Request input deadline:i64; output ok:u8 + blob(requestbytes or native error). Payload input blob; packet input count:u32 + blobs. Then snapshot output ok:u8 + blob(native error UTF8) + readingcount:u32; each reading has blob(idUTF8),blob(nameUTF8),blob(originUTF8),celsius:f64LE. Every blob is length:u32 + bytes.

Contains 7synthetic request deadlines,33payload and20packet-group cases. Native outcomes are immutable.59cases match Rust successful request/reading bytes and acceptance; case59 is explicitly stronger Rust UTF8 rejection: actual Qt buffers an incomplete final UTF8 trailer detail without error. Rust rejects it. Other metadata error wording can differ due reuse of strict shared filter_fan. Known separate DIMM-LF regex-vs-strict-suffix difference is documented in work REPORT.

Coverage includes defaults/type3/noValue-vs-zero, finite temperatures, metadata/duplicate fields and IDs, unknown ordinary fields, groups, malformed protobuf, coalesced/separate unary packets, separate raw payload, trailers/status and fixed request deadline format. No physical acquisition timestamp, IPC-provider acceptance or exhaustive protobuf parity is claimed.
