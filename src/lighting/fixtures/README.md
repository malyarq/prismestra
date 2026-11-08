# Public offline fixtures

All samples are synthetic RGB/math data, no hardware identifiers, paths or personal data. Tests include synthetic *.bin bytes and JSON fixtures; no executables.

protocol-oracle.cpp uses original door Protocol.cpp. math-oracle.cpp links original OpenRGB hsv.cpp and SimplexNoise.cpp. patterns-oracle.cpp copies the small Breathing/RainbowWave step arithmetic verbatim from neighboring Effects sources and links original hsv.cpp. noise-oracle.cpp copies NoiseMap step/GetColor arithmetic and links original hsv.cpp/SimplexNoise.cpp. gradient-oracle.cpp reproduces GenerateGradient using native Qt6.8.3 QPainter. Sources are compiled from workspace with MSVC C++17; Qt oracle additionally needs /Zc:__cplusplus /permissive- and QtCore/QtGui. Run oracle executables in a temporary work directory; they emit named *-fixtures.bin there. noise oracle reads gradient-fixtures.bin in its working directory.

RGB fixtures use OpenRGB 0xBBGGRR. Integer/float records are little-endian: math records26bytes = HSV hue u32, saturation u8, value u8, backRGB u32, x/y/z/expectednoise f32. patterns/noise records are packed u32 RGB. Gradient records RGB u8 triplets, palettes1..8 each100px. Protocol records = RGB/brightness/segments/sequence six u8 + length u8 + packet bytes.

Oracle code adapted from GPL-2.0-or-later OpenRGB Effects and MIT HSV/Simplex sources. See HSV header notice and SimplexNoise-LICENSE.txt. Parity bounds are documented in work/lighting-port/RESULT.md.

Added gradient-palettes-fixtures.bin:8192records, each n:u8 + palette RGBu8[n] + output RGBu8[100]. Source gradient-palettes-oracle.cpp uses actual native Qt6.8.3 QPainter. Rust generator source evidence and coverage: qt-gradient-sources.md.

scene-profile-native.json is the byte-for-byte extracted JSON formerly embedded in profile.rs::native_cpp_differential. It contains one synthetic base profile and 29 read/compose/validate cases, including expected native JSON/results. Provenance: work/scene-profile/oracle.cpp compiles the original neighboring rgb-openrgb-2026-11-05/effects-source/SceneEffects.cpp with SCENE_EFFECTS_LOGIC_ONLY; raw native outputs are work/scene-profile/native-fixtures.json and native-validation-fixtures.json. The public fixture has no dependency on ignored work. Coverage is finite 29 cases: effect transitions, speed limits, RGB order, noise template, malformed fields, unowned changes and numeric equality; it does not establish GUI/provider/hardware acceptance or exhaust every JSON input.
