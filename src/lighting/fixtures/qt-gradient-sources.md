# Qt gradient source evidence

Rust `gradient.rs` specializes the opaque, transform-free NoiseMap gradient; full Qt rendering is not imported. Original copyrights and GPL-2.0-only OR GPL-3.0-only option are retained in the source header.

Official version-pinned sources:

- [qpaintengine_raster.cpp v6.8.3](https://github.com/qt/qtbase/blob/v6.8.3/src/gui/painting/qpaintengine_raster.cpp#L3936): QGradientCache::generateGradientColorTable, lines3936–4112. This includes separate stopCount2 fixed16 path, and general interpolated256 path with dpos starting at1.5/1024.
- [qdrawhelper.cpp v6.8.3](https://github.com/qt/qtbase/blob/v6.8.3/src/gui/painting/qdrawhelper.cpp#L2989): FIXPT_BITS8; qt_gradient_pixel_fixed; qt_fetch_linear_gradient_template lines3094–3159. NoiseMap 100x1 rect starts at x+.5; fetch truncates the initial/fixed increments separately, then nearest-index lookup.
- [qdrawhelper_p.h v6.8.3](https://github.com/qt/qtbase/blob/v6.8.3/src/gui/painting/qdrawhelper_p.h): GRADIENT_STOPTABLE_SIZE1024 and QRgba64 interpolate256 (each multiplyAlpha256 is truncated before addition).
- Local version-matched SDK `qt-sdk/6.8.3/msvc2022_64/include/QtGui/qrgba64.h`: QRgba64::toArgb32/div_257 for final16→8 conversion. `include/QtGui/6.8.3/QtGui/private/qdrawhelper_p.h` supplies multiplyAlpha256/interpolate256. These installed headers were read directly.
- Our source behavior: neighboring effects-source/Effects/NoiseMap/NoiseMap.cpp::GenerateGradient: float32 color_step=1.f/n, float32 i*color_step then qreal; stops end at(n-1)/n; PadSpread; QPainter paints RGB32 image100x1, default ColorInterpolation.

Native oracle `gradient-palettes-oracle.cpp` performs the same actual Qt QPainter operation for8192 deterministic palettes with sizes1..8. Fixtures record n:u8 + RGBu8[n] + RGBu8[100]. Cases include repeated identical colors, black/white alternation, all-black/all-white, individual channels, greyscale and xorshift random colors. `gradient::native_arbitrary_palettes` asserts all819200 pixels, all2457600 channel comparisons exactly; maximum observed channel error0. Additionally original8 deterministic palettes/800pixels remain tested. This finite coverage plus direct source port does not claim exhaustive testing of all16M colors per stop or other Qt versions/transforms/alpha.
