// Copyright (C) 2021 The Qt Company Ltd.
// SPDX-License-Identifier: GPL-2.0-only OR GPL-3.0-only
// Opaque RGB specialization of Qt 6.8.3 QGradientCache/fixed-point raster fetch.
// See fixtures/qt-gradient-sources.md for official source functions and licenses.
use super::color::Rgb;
const SIZE: usize = 1024;
fn channels(c: Rgb) -> [u32; 3] {
    [c.r, c.g, c.b].map(|c| c as u32 * 257)
}
fn rgb(c: [u32; 3]) -> Rgb {
    let c = c.map(|v| {
        let v = v + 128;
        ((v - (v >> 8)) >> 8) as u8
    });
    Rgb::new(c[0], c[1], c[2])
}
/// Reproduce NoiseMap GenerateGradient for 1..=8 opaque colours, 100x1 RGB32,
/// no transform, PadSpread, default ColorInterpolation, Qt 6.8.3 raster engine.
/// Stops use native float32 i*(1/n), with the final stop before 1.0.
pub fn noise_palette_lut(palette: &[Rgb]) -> Result<[Rgb; 100], &'static str> {
    if palette.is_empty() || palette.len() > 8 {
        return Err("palette must contain one to eight colors");
    }
    let n = palette.len();
    let mut table = [[0u32; 3]; SIZE];
    let stops: Vec<f64> = (0..n)
        .map(|i| (i as f32 * (1.0f32 / n as f32)) as f64)
        .collect();
    if n == 1 {
        table.fill(channels(palette[0]));
    } else if n == 2 {
        let first = channels(palette[0]);
        let second = channels(palette[1]);
        let end = (stops[1] * 1023.0).round() as usize;
        table[0] = first;
        let mut value = first.map(|v| ((v as i64) << 16) + 32768);
        let delta = std::array::from_fn::<_, 3, _>(|ch| {
            ((second[ch] as f64 - first[ch] as f64) * 65536.0 / end as f64).round() as i64
        });
        for row in table.iter_mut().take(end).skip(1) {
            for ch in 0..3 {
                value[ch] += delta[ch];
                row[ch] = (value[ch] >> 16) as u32;
            }
        }
        table[end..].fill(second);
    } else {
        table[0] = channels(palette[0]);
        let incr = 1.0 / 1024.0;
        let mut dpos = 1.5 * incr;
        let mut pos = 1;
        let mut current = 0;
        let end = stops[n - 1];
        let mut factor = 256.0 / (stops[1] - stops[0]);
        let mut t = dpos * factor;
        let mut dt = incr * factor;
        while dpos < end {
            let dist = t.round() as u32;
            let a = channels(palette[current]);
            let b = channels(palette[current + 1]);
            table[pos] =
                std::array::from_fn(|ch| ((a[ch] * (256 - dist)) >> 8) + ((b[ch] * dist) >> 8));
            pos += 1;
            dpos += incr;
            if dpos >= end {
                break;
            }
            t += dt;
            let before = current;
            while dpos > stops[current + 1] {
                current += 1
            }
            if current != before {
                factor = 256.0 / (stops[current + 1] - stops[current]);
                t = (dpos - stops[current]) * factor;
                dt = incr * factor;
            }
        }
        table[pos..].fill(channels(palette[n - 1]));
    }
    let mut out = [Rgb::default(); 100];
    let mut fixed = (0.005f64 * 1023.0 * 256.0) as usize;
    let inc = (0.01f64 * 1023.0 * 256.0) as usize;
    for c in &mut out {
        *c = rgb(table[((fixed + 128) >> 8).min(1023)]);
        fixed += inc;
    }
    Ok(out)
}
#[test]
fn native_gradient_baseline() {
    let expected = include_bytes!("fixtures/gradient-fixtures.bin");
    for n in 1..=8 {
        let palette: Vec<_> = (0..n)
            .map(|i| {
                Rgb::new(
                    ((i * 71 + 255) % 256) as u8,
                    ((i * 113 + 85) % 256) as u8,
                    ((i * 197) % 256) as u8,
                )
            })
            .collect();
        let lut = noise_palette_lut(&palette).unwrap();
        for (x, c) in lut.iter().enumerate() {
            let offset = (n - 1) * 300 + x * 3;
            assert_eq!(
                *c,
                Rgb::new(expected[offset], expected[offset + 1], expected[offset + 2]),
                "palette {n} x{x}"
            );
        }
    }
}
#[test]
fn palette_boundaries() {
    assert!(noise_palette_lut(&[]).is_err());
    assert!(noise_palette_lut(&[Rgb::default(); 9]).is_err());
    let c = Rgb::new(128, 20, 255);
    assert_eq!(noise_palette_lut(&[c]).unwrap(), [c; 100]);
}

#[test]
fn native_arbitrary_palettes() {
    let data = include_bytes!("fixtures/gradient-palettes-fixtures.bin");
    let mut offset = 0;
    let mut cases = 0;
    let mut maximum = 0;
    while offset < data.len() {
        let n = data[offset] as usize;
        offset += 1;
        let palette: Vec<_> = data[offset..offset + 3 * n]
            .as_chunks::<3>()
            .0
            .iter()
            .map(|c| Rgb::new(c[0], c[1], c[2]))
            .collect();
        offset += 3 * n;
        let expected = &data[offset..offset + 300];
        offset += 300;
        let lut = noise_palette_lut(&palette).unwrap();
        for (x, (c, expect)) in lut
            .iter()
            .zip(expected.as_chunks::<3>().0.iter())
            .enumerate()
        {
            for (actual, reference) in [c.r, c.g, c.b].iter().zip(expect) {
                let error = actual.abs_diff(*reference);
                maximum = maximum.max(error);
                assert_eq!(error, 0, "case {cases} n{n} x{x} palette {palette:?}");
            }
        }
        cases += 1;
    }
    assert_eq!(cases, 8192);
    assert_eq!(maximum, 0);
}

/// Native truncation, with explicit errors for undefined QImage::pixelColor bounds.
pub fn sample_noise_lut(lut: &[Rgb; 100], fraction: f32) -> Result<Rgb, &'static str> {
    if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
        return Err("invalid noise gradient fraction");
    }
    let index = ((1.0 - fraction) * 100.0) as usize;
    lut.get(index)
        .copied()
        .ok_or("native gradient coordinate outside 100 pixels")
}
#[test]
fn sampling_bounds() {
    let lut = [Rgb::new(1, 2, 3); 100];
    assert_eq!(sample_noise_lut(&lut, 1.0), Ok(lut[0]));
    assert!(sample_noise_lut(&lut, 0.0).is_err());
    assert!(sample_noise_lut(&lut, -0.01).is_err());
    assert!(sample_noise_lut(&lut, 1.01).is_err());
    assert!(sample_noise_lut(&lut, f32::NAN).is_err());
}
