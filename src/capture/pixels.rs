// SPDX-License-Identifier: GPL-2.0-or-later
// Opaque specialization of Qt 6.8.3 qimagescale.cpp (BSD-2-Clause AND Imlib2).
// Copyright (C) 2016 The Qt Company Ltd.
// Copyright (C) 2004, 2005 Daniel M. Duley, Carsten Haitzler and various contributors,
// Willem Monsuwe <willem@stack.nl>.
// Original notices are retained below. The up/up interpolation follows Qt's x86 SSE2 order.
use crate::lighting::{
    ambient::{Image, Rect},
    color::Rgb,
};

const MAX_BYTES: usize = 256 * 1024 * 1024;
#[cfg(target_arch = "x86_64")]
#[path = "pixels_sse41.rs"]
mod sse41;

/// Consume a borrowed BGRA8 mapping without copying the full desktop or retaining its memory.
pub fn thumbnail(
    bytes: &[u8],
    width: u32,
    height: u32,
    pitch: usize,
    crop: Option<Rect>,
) -> Result<Image, String> {
    let (w, h) = (width as usize, height as usize);
    if !(1..=16384).contains(&w)
        || !(1..=16384).contains(&h)
        || w.checked_mul(h)
            .and_then(|n| n.checked_mul(4))
            .is_none_or(|n| n > MAX_BYTES)
    {
        return Err("capture dimensions exceed limits".into());
    }
    let row = w * 4;
    let end = pitch.checked_mul(h - 1).and_then(|n| n.checked_add(row));
    if pitch < row || end.is_none_or(|n| n > bytes.len() || n > MAX_BYTES) {
        return Err("invalid capture pitch or buffer length".into());
    }
    let c = crop.unwrap_or(Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    });
    if c.width == 0
        || c.height == 0
        || c.x.checked_add(c.width).is_none_or(|n| n > w)
        || c.y.checked_add(c.height).is_none_or(|n| n > h)
    {
        return Err("capture crop must fit source".into());
    }
    // QSize::scaled floors the dependent dimension; QImage::scaled clamps it to one.
    let (dw, dh) = if 180 * c.width / c.height <= 320 {
        ((180 * c.width / c.height).max(1), 180)
    } else {
        (320, (320 * c.height / c.width).max(1))
    };
    #[cfg(target_arch = "x86_64")]
    if dw < c.width && dh < c.height && std::is_x86_feature_detected!("sse4.1") {
        // Layout/crop were bounded above; only this down/down specialization is SIMD.
        return unsafe { sse41::thumbnail(bytes, pitch, c, dw, dh) };
    }
    let read = |x: usize, y: usize| {
        let p = (y + c.y) * pitch + (x + c.x) * 4;
        [bytes[p + 2] as u64, bytes[p + 1] as u64, bytes[p] as u64]
    };
    smooth(c.width, c.height, dw, dh, read)
}

/// Smooth RGB32 scaling of a bounded processing image (including Ambient's 1x1 average).
pub fn smooth_scale(image: &Image, width: usize, height: usize) -> Result<Image, String> {
    if width == 0
        || height == 0
        || width.checked_mul(height).is_none_or(|n| n > 65536)
        || image
            .width()
            .checked_mul(image.height())
            .is_none_or(|n| n > 65536)
        || image.width() > 16384
        || image.height() > 16384
    {
        return Err("smooth scale dimensions exceed limits".into());
    }
    smooth(image.width(), image.height(), width, height, |x, y| {
        let p = image.pixels()[y * image.width() + x];
        [p.r as u64, p.g as u64, p.b as u64]
    })
}

fn smooth(
    sw: usize,
    sh: usize,
    dw: usize,
    dh: usize,
    read: impl Fn(usize, usize) -> [u64; 3],
) -> Result<Image, String> {
    let ax = axis(sw, dw);
    let ay = axis(sh, dh);
    let mut out = Vec::with_capacity(dw * dh);
    for y in &ay {
        for x in &ax {
            let rgb = match (dw >= sw, dh >= sh) {
                (true, true) => {
                    // SSE2 interpolates vertically first, truncating before horizontal interpolation.
                    let l = mix(
                        read(x.pos, y.pos),
                        read(x.pos, (y.pos + 1).min(sh - 1)),
                        y.first,
                        8,
                    );
                    let r = mix(
                        read((x.pos + 1).min(sw - 1), y.pos),
                        read((x.pos + 1).min(sw - 1), (y.pos + 1).min(sh - 1)),
                        y.first,
                        8,
                    );
                    mix(l, r, x.first, 8)
                }
                (true, false) => {
                    let l = area(y, |k| read(x.pos, k));
                    let r = area(y, |k| read((x.pos + 1).min(sw - 1), k));
                    mix(l, r, x.first, 8).map(|v| v >> 14)
                }
                (false, true) => {
                    let t = area(x, |k| read(k, y.pos));
                    let b = area(x, |k| read(k, (y.pos + 1).min(sh - 1)));
                    mix(t, b, y.first, 8).map(|v| v >> 14)
                }
                (false, false) => {
                    area(y, |j| area(x, |i| read(i, j)).map(|v| v >> 4)).map(|v| v >> 24)
                }
            };
            out.push(Rgb::new(rgb[0] as u8, rgb[1] as u8, rgb[2] as u8));
        }
    }
    Image::new(dw, dh, out).map_err(str::to_owned)
}

struct Axis {
    pos: usize,
    first: u64,
    step: u64,
}
fn axis(source: usize, dest: usize) -> Vec<Axis> {
    let up = dest >= source;
    let mut val = if up {
        (32768 * source / dest) as i64 - 32768
    } else {
        0
    };
    let inc = ((source as i64) << 16) / dest as i64;
    let cp = (dest * 16384).div_ceil(source) as u64;
    (0..dest)
        .map(|_| {
            let raw = val >> 16;
            let pos = raw.max(0) as usize;
            let first = if up {
                if raw < 0 || pos >= source - 1 {
                    0
                } else {
                    ((val >> 8) & 255) as u64
                }
            } else {
                ((65536 - (val & 65535)) as u64 * cp) >> 16
            };
            val += inc;
            Axis {
                pos,
                first,
                step: cp,
            }
        })
        .collect()
}
fn mix(a: [u64; 3], b: [u64; 3], weight: u64, shift: u32) -> [u64; 3] {
    std::array::from_fn(|i| (a[i] * (256 - weight) + b[i] * weight) >> shift)
}
fn area(a: &Axis, read: impl Fn(usize) -> [u64; 3]) -> [u64; 3] {
    let mut sum = read(a.pos).map(|v| v * a.first);
    let mut left = 16384 - a.first;
    let mut p = a.pos;
    while left > a.step {
        p += 1;
        let c = read(p);
        for i in 0..3 {
            sum[i] += c[i] * a.step;
        }
        left -= a.step;
    }
    // Qt reads the final sample even for zero weight; avoid that unnecessary boundary access.
    if left > 0 {
        let c = read(p + 1);
        for i in 0..3 {
            sum[i] += c[i] * left;
        }
    }
    sum
}

/// Qt FastTransformation sampling for an owned processing image, without aspect preservation.
pub fn fast_scale(image: &Image, width: usize, height: usize) -> Result<Image, String> {
    if width == 0
        || height == 0
        || width.checked_mul(height).is_none_or(|n| n > 65536)
        || image.width() > 16384
        || image.height() > 16384
    {
        return Err("fast scale dimensions exceed limits".into());
    }
    let mut pixels = Vec::with_capacity(width * height);
    // qblendfunctions_p.h: 16.16 increment, ceil(initial half-step)-1 biases exact ties down.
    let xstep = image.width() * 65536 / width;
    let ystep = image.height() * 65536 / height;
    let xstart = (image.width() * 32768).div_ceil(width) - 1;
    let ystart = (image.height() * 32768).div_ceil(height) - 1;
    for y in 0..height {
        let sy = ((ystart + y * ystep) >> 16).min(image.height() - 1);
        for x in 0..width {
            let sx = ((xstart + x * xstep) >> 16).min(image.width() - 1);
            pixels.push(image.pixels()[sy * image.width() + sx]);
        }
    }
    Image::new(width, height, pixels).map_err(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar_thumbnail(
        bytes: &[u8],
        width: u32,
        height: u32,
        pitch: usize,
        crop: Option<Rect>,
    ) -> Image {
        let c = crop.unwrap_or(Rect {
            x: 0,
            y: 0,
            width: width as usize,
            height: height as usize,
        });
        let (dw, dh) = if 180 * c.width / c.height <= 320 {
            ((180 * c.width / c.height).max(1), 180)
        } else {
            (320, (320 * c.height / c.width).max(1))
        };
        smooth(c.width, c.height, dw, dh, |x, y| {
            let p = (y + c.y) * pitch + (x + c.x) * 4;
            [
                u64::from(bytes[p + 2]),
                u64::from(bytes[p + 1]),
                u64::from(bytes[p]),
            ]
        })
        .unwrap()
    }

    #[test]
    fn seeded_bgra_downscale_matches_scalar_with_crops_pitch_and_alpha() {
        let mut seed = 0x2e6d_9a31_a59c_487bu64;
        let mut cases = vec![
            (321, 181, 0, None),
            (640, 360, 7, None),
            (641, 361, 13, None),
            (1921, 1081, 19, None),
            (3840, 2160, 64, None),
            (4096, 2304, 17, None),
            (16384, 7, 3, None),
            (7, 16384, 9, None),
            (
                3840,
                2160,
                31,
                Some(Rect {
                    x: 13,
                    y: 17,
                    width: 3799,
                    height: 2097,
                }),
            ),
            (
                2000,
                1200,
                1,
                Some(Rect {
                    x: 1543,
                    y: 901,
                    width: 457,
                    height: 299,
                }),
            ),
        ];
        for _ in 0..24 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let w = 321 + (seed as u32 % 900);
            let h = 181 + ((seed >> 32) as u32 % 700);
            cases.push((
                w,
                h,
                (seed as usize >> 16) % 35,
                Some(Rect {
                    x: 1,
                    y: 1,
                    width: w as usize - 2,
                    height: h as usize - 2,
                }),
            ));
        }
        for (w, h, padding, crop) in cases {
            let pitch = w as usize * 4 + padding;
            // Last row deliberately has no trailing padding: wide loads must not overread.
            let mut bytes = vec![0u8; (h as usize - 1) * pitch + w as usize * 4];
            for value in &mut bytes {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                *value = (seed >> 32) as u8;
            }
            let scalar = scalar_thumbnail(&bytes, w, h, pitch, crop);
            let actual = thumbnail(&bytes, w, h, pitch, crop).unwrap();
            assert_eq!(actual, scalar, "{w}x{h} pitch{pitch} crop{crop:?}");
            for y in 0..h as usize {
                for x in 0..w as usize {
                    bytes[y * pitch + x * 4 + 3] ^= 255;
                }
            }
            assert_eq!(
                thumbnail(&bytes, w, h, pitch, crop).unwrap(),
                scalar,
                "alpha affected {w}x{h}"
            );
            assert!(thumbnail(&bytes[..bytes.len() - 1], w, h, pitch, crop).is_err());
        }
    }

    #[test]
    fn mixed_equal_and_up_thumbnail_remain_scalar_exact() {
        for (w, h) in [(1, 1), (500, 1), (1, 500), (320, 180), (31, 17)] {
            let pitch = w as usize * 4 + 3;
            let bytes: Vec<_> = (0..(h as usize - 1) * pitch + w as usize * 4)
                .map(|i| (i.wrapping_mul(37) % 251) as u8)
                .collect();
            assert_eq!(
                thumbnail(&bytes, w, h, pitch, None).unwrap(),
                scalar_thumbnail(&bytes, w, h, pitch, None)
            );
        }
    }

    #[test]
    fn padded_last_row_and_alpha_are_independent_of_owned_output() {
        let mut bytes = vec![1, 2, 3, 0, 99, 99, 99, 99, 1, 2, 3, 77];
        let image = thumbnail(&bytes, 1, 2, 8, None).unwrap();
        bytes.fill(255);
        assert_eq!((image.width(), image.height()), (90, 180));
        assert!(image.pixels().iter().all(|p| *p == Rgb::new(3, 2, 1)));
    }

    #[test]
    fn rejects_untrusted_layout_and_crop() {
        for (w, h, pitch) in [
            (0, 1, 4),
            (16385, 1, 4),
            (1, 2, usize::MAX),
            (1, 1, 3),
            (16384, 16384, 65536),
        ] {
            assert!(thumbnail(&[0; 4], w, h, pitch, None).is_err());
        }
        for crop in [
            Rect::default(),
            Rect {
                x: usize::MAX,
                y: 0,
                width: 2,
                height: 1,
            },
            Rect {
                x: 0,
                y: 1,
                width: 1,
                height: 1,
            },
        ] {
            assert!(thumbnail(&[0; 4], 1, 1, 4, Some(crop)).is_err());
        }
    }

    #[test]
    fn fast_ties_follow_qt_lower_pixel() {
        let image = Image::new(2, 1, vec![Rgb::new(1, 2, 3), Rgb::new(4, 5, 6)]).unwrap();
        assert_eq!(
            fast_scale(&image, 1, 1).unwrap().pixels(),
            &[Rgb::new(1, 2, 3)]
        );
        assert_eq!(
            smooth_scale(&image, 1, 1).unwrap().pixels(),
            &[Rgb::new(2, 3, 4)]
        );
        assert!(fast_scale(&image, 257, 256).is_err());
        assert!(smooth_scale(&image, 257, 256).is_err());
    }
}
/*
 * Copyright (C) 2004, 2005 Daniel M. Duley
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions
 * are met:
 *
 * 1. Redistributions of source code must retain the above copyright
 *    notice, this list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright
 *    notice, this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 *
 * THIS SOFTWARE IS PROVIDED BY THE AUTHOR ``AS IS'' AND ANY EXPRESS OR
 * IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES
 * OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED.
 * IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY DIRECT, INDIRECT,
 * INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT
 * NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
 * DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
 * THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
 * (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF
 * THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 *
 */

/* OTHER CREDITS:
 *
 * This is the normal smoothscale method, based on Imlib2's smoothscale.
 *
 * Originally I took the algorithm used in NetPBM and Qt and added MMX/3dnow
 * optimizations. It ran in about 1/2 the time as Qt. Then I ported Imlib's
 * C algorithm and it ran at about the same speed as my MMX optimized one...
 * Finally I ported Imlib's MMX version and it ran in less than half the
 * time as my MMX algorithm, (taking only a quarter of the time Qt does).
 * After further optimization it seems to run at around 1/6th.
 *
 * Changes include formatting, namespaces and other C++'ings, removal of old
 * #ifdef'ed code, and removal of unneeded border calculation code.
 * Later the code has been refactored, an SSE4.1 optimizated path have been
 * added instead of the removed MMX assembler, and scaling of clipped area
 * removed, and an RGBA64 version written
 *
 * Imlib2 is (C) Carsten Haitzler and various contributors. The MMX code
 * is by Willem Monsuwe <willem@stack.nl>. All other modifications are
 * (C) Daniel M. Duley.
 */
