// /*
// * RGB <--> HSV conversion in integer arithmetics, to be used on Windows.
// * Copyright (c) 2013 Martin Mitas
// *
// * Permission is hereby granted, free of charge, to any person obtaining
// * a copy of this software and associated documentation files (the "Software"),
// * to deal in the Software without restriction, including without limitation
// * the rights to use, copy, modify, merge, publish, distribute, sublicense,
// * and/or sell copies of the Software, and to permit persons to whom the
// * Software is furnished to do so, subject to the following conditions:
// *
// * The above copyright notice and this permission notice shall be included in
// * all copies or substantial portions of the Software.
// *
// * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
// * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
// * IN THE SOFTWARE.
// */
//
//
use super::color::Rgb;
#[derive(Clone, Copy, Debug)]
pub struct Hsv {
    pub hue: u32,
    pub saturation: u8,
    pub value: u8,
}
pub fn from_rgb(c: Rgb) -> Hsv {
    let (r, g, b) = (c.r as i32, c.g as i32, c.b as i32);
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    if d == 0 {
        return Hsv {
            hue: u32::MAX,
            saturation: 0,
            value: max as u8,
        };
    }
    let mut h = if r == max {
        (g - b) * 60 / d
    } else if g == max {
        (b - r) * 60 / d + 120
    } else {
        (r - g) * 60 / d + 240
    };
    if h < 0 {
        h += 360
    }
    Hsv {
        hue: h as u32,
        saturation: ((256 * d - 8) / max) as u8,
        value: max as u8,
    }
}
pub fn to_rgb(c: Hsv) -> Rgb {
    if c.saturation == 0 {
        return Rgb::new(c.value, c.value, c.value);
    }
    let (h, s, v) = ((c.hue % 360) as i32, c.saturation as i32, c.value as i32);
    let i = h / 60;
    let p = (256 * v - s * v) / 256;
    if i & 1 != 0 {
        let q = (256 * 60 * v - h * s * v + 60 * s * v * i) / (256 * 60);
        match i {
            1 => Rgb::new(q as u8, v as u8, p as u8),
            3 => Rgb::new(p as u8, q as u8, v as u8),
            _ => Rgb::new(v as u8, p as u8, q as u8),
        }
    } else {
        let t = (256 * 60 * v + h * s * v - 60 * s * v * (i + 1)) / (256 * 60);
        match i {
            0 => Rgb::new(v as u8, t as u8, p as u8),
            2 => Rgb::new(p as u8, v as u8, t as u8),
            _ => Rgb::new(t as u8, p as u8, v as u8),
        }
    }
}
