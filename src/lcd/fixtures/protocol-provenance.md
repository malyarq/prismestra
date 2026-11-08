# Public LCD protocol fixtures

`protocol-native.bin` is synthetic original-native output, not an image-ready media file and not private device data. Tests must not treat structurally accepted JPEG examples as decoded JPEGs.

Generation: `protocol-oracle.cpp` includes the original `tt-lcd-plugin/Protocol.h` through `LCD_PROTOCOL_HEADER` macro, links original `Protocol.cpp`, QtCore only. It does not include HidTransport, open devices or start GUI. Private work evidence: `work/lcd-protocol/{REPORT.md,manifest.json,driver.cpp}`. No runtime test dependency on work.

Format: all integer little-endian. `LCP1` + packetcount:u32(512). Each packet record: frame_length:u32,index:u32,seed:u32,native_report_length:u16, then report bytes(0or1025). Frame input byte[i] is low8bits of uint32(i*31+seed), with wrappingaddition. First255cases use MAX_FRAME260100 and index0..254. More boundary/randomcases include invalid requests.

Then jpegcount:u32(169), each record jpeg_length:u32 + jpegbytes + ok:u8 + error_length:u16 + nativeUTF8errorbytes. Contains structurally plausible synthetic baseline480x4804:4:4 headers, deliberately invalid cases, and size boundary cases. Original code checks structure and returns on SOS without validating entropy.

Bounds: MAX_FRAME260100, payload1020, report1025 includingID0. Packet oracle checks entire native bytes, JPEG oracle checks outcome/error text. No source/fixture proves hardwareframe delivery, fullJPEGdecoding, arbitraryLCDcompatibility orACKpayloadmeaning. Existing ACKgate knows only length17/reportID0.
