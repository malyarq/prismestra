// SPDX-License-Identifier: GPL-2.0-or-later
//! BGRA down/down specialization of the scalar Qt integer ordering in pixels.rs.
use super::{Axis, Image, Rect, Rgb, axis};
use std::arch::x86_64::*;

/// Caller must establish SSE4.1, a valid bounded BGRA layout/crop, and strict
/// downscaling on both axes. Every memory read remains a safe four-byte slice:
/// crop is inside the validated rows, and axis/area select the same in-range
/// source coordinates as the scalar oracle. No SIMD load can overread a row.
#[target_feature(enable = "sse4.1")]
pub(super) unsafe fn thumbnail(
    bytes: &[u8],
    pitch: usize,
    crop: Rect,
    width: usize,
    height: usize,
) -> Result<Image, String> {
    let xs = axis(crop.width, width);
    let ys = axis(crop.height, height);
    let mut output = Vec::with_capacity(width * height);
    for y in &ys {
        for x in &xs {
            // Horizontal weights total16384, max sum255<<14. Shift before
            // vertical exactly as scalar does; its max255<<24 fits unsignedu32.
            let first = unsafe { horizontal(bytes, pitch, crop, y.pos, x) };
            let mut sum =
                _mm_mullo_epi32(_mm_srli_epi32::<4>(first), _mm_set1_epi32(y.first as i32));
            let mut left = 16384 - y.first;
            let mut row = y.pos;
            let step = _mm_set1_epi32(y.step as i32);
            while left > y.step {
                row += 1;
                let value = unsafe { horizontal(bytes, pitch, crop, row, x) };
                sum = _mm_add_epi32(sum, _mm_mullo_epi32(_mm_srli_epi32::<4>(value), step));
                left -= y.step;
            }
            if left > 0 {
                let value = unsafe { horizontal(bytes, pitch, crop, row + 1, x) };
                sum = _mm_add_epi32(
                    sum,
                    _mm_mullo_epi32(_mm_srli_epi32::<4>(value), _mm_set1_epi32(left as i32)),
                );
            }
            let rgb = _mm_srli_epi32::<24>(sum);
            output.push(Rgb::new(
                _mm_extract_epi32::<2>(rgb) as u8,
                _mm_extract_epi32::<1>(rgb) as u8,
                _mm_extract_epi32::<0>(rgb) as u8,
            ));
        }
    }
    Image::new(width, height, output).map_err(str::to_owned)
}

#[target_feature(enable = "sse4.1")]
unsafe fn horizontal(bytes: &[u8], pitch: usize, crop: Rect, y: usize, x: &Axis) -> __m128i {
    let start = (crop.y + y) * pitch + crop.x * 4;
    let row = &bytes[start..start + crop.width * 4];
    let mut sum = _mm_mullo_epi32(unsafe { pixel(row, x.pos) }, _mm_set1_epi32(x.first as i32));
    let mut left = 16384 - x.first;
    let mut column = x.pos;
    let step = _mm_set1_epi32(x.step as i32);
    while left > x.step {
        column += 1;
        sum = _mm_add_epi32(sum, _mm_mullo_epi32(unsafe { pixel(row, column) }, step));
        left -= x.step;
    }
    if left > 0 {
        sum = _mm_add_epi32(
            sum,
            _mm_mullo_epi32(
                unsafe { pixel(row, column + 1) },
                _mm_set1_epi32(left as i32),
            ),
        );
    }
    sum
}

#[target_feature(enable = "sse4.1")]
unsafe fn pixel(row: &[u8], x: usize) -> __m128i {
    let offset = x * 4;
    let bgra = u32::from_le_bytes(row[offset..offset + 4].try_into().unwrap());
    // Alpha gets its own independent lane and is discarded; it cannot affect RGB.
    _mm_cvtepu8_epi32(_mm_cvtsi32_si128(bgra as i32))
}
