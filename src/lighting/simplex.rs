// SPDX-License-Identifier: MIT
// Port of SimplexNoise.cpp, Copyright (c) 2014-2018 Sebastien Rombauts.
// Full license: fixtures/SimplexNoise-LICENSE.txt
const PERM: [u8; 256] = [
    151, 160, 137, 91, 90, 15, 131, 13, 201, 95, 96, 53, 194, 233, 7, 225, 140, 36, 103, 30, 69,
    142, 8, 99, 37, 240, 21, 10, 23, 190, 6, 148, 247, 120, 234, 75, 0, 26, 197, 62, 94, 252, 219,
    203, 117, 35, 11, 32, 57, 177, 33, 88, 237, 149, 56, 87, 174, 20, 125, 136, 171, 168, 68, 175,
    74, 165, 71, 134, 139, 48, 27, 166, 77, 146, 158, 231, 83, 111, 229, 122, 60, 211, 133, 230,
    220, 105, 92, 41, 55, 46, 245, 40, 244, 102, 143, 54, 65, 25, 63, 161, 1, 216, 80, 73, 209, 76,
    132, 187, 208, 89, 18, 169, 200, 196, 135, 130, 116, 188, 159, 86, 164, 100, 109, 198, 173,
    186, 3, 64, 52, 217, 226, 250, 124, 123, 5, 202, 38, 147, 118, 126, 255, 82, 85, 212, 207, 206,
    59, 227, 47, 16, 58, 17, 182, 189, 28, 42, 223, 183, 170, 213, 119, 248, 152, 2, 44, 154, 163,
    70, 221, 153, 101, 155, 167, 43, 172, 9, 129, 22, 39, 253, 19, 98, 108, 110, 79, 113, 224, 232,
    178, 185, 112, 104, 218, 246, 97, 228, 251, 34, 242, 193, 238, 210, 144, 12, 191, 179, 162,
    241, 81, 51, 145, 235, 249, 14, 239, 107, 49, 192, 214, 31, 181, 199, 106, 157, 184, 84, 204,
    176, 115, 121, 50, 45, 127, 4, 150, 254, 138, 236, 205, 93, 222, 114, 67, 29, 24, 72, 243, 141,
    128, 195, 78, 66, 215, 61, 156, 180,
];
fn hash(i: i32) -> i32 {
    PERM[i as u8 as usize] as i32
}
fn grad(hash: i32, x: f32, y: f32, z: f32) -> f32 {
    let h = hash & 15;
    let u = if h < 8 { x } else { y };
    let v = if h < 4 {
        y
    } else if h == 12 || h == 14 {
        x
    } else {
        z
    };
    (if h & 1 != 0 { -u } else { u }) + (if h & 2 != 0 { -v } else { v })
}
pub fn noise(x: f32, y: f32, z: f32) -> f32 {
    if [x, y, z]
        .iter()
        .any(|c| !c.is_finite() || c.abs() > 100_000_000.0)
    {
        return f32::NAN;
    }
    let s = (x + y + z) * (1.0f32 / 3.0);
    let (i, j, k) = (
        (x + s).floor() as i32,
        (y + s).floor() as i32,
        (z + s).floor() as i32,
    );
    let g = 1.0f32 / 6.0;
    let t = (i + j + k) as f32 * g;
    let (x0, y0, z0) = (x - (i as f32 - t), y - (j as f32 - t), z - (k as f32 - t));
    let (a, b) = if x0 >= y0 {
        if y0 >= z0 {
            ([1, 0, 0], [1, 1, 0])
        } else if x0 >= z0 {
            ([1, 0, 0], [1, 0, 1])
        } else {
            ([0, 0, 1], [1, 0, 1])
        }
    } else if y0 < z0 {
        ([0, 0, 1], [0, 1, 1])
    } else if x0 < z0 {
        ([0, 1, 0], [0, 1, 1])
    } else {
        ([0, 1, 0], [1, 1, 0])
    };
    let corner = |o: [i32; 3], n: f32| {
        let (x, y, z) = (
            x0 - o[0] as f32 + n * g,
            y0 - o[1] as f32 + n * g,
            z0 - o[2] as f32 + n * g,
        );
        let mut t = 0.6 - x * x - y * y - z * z;
        if t < 0.0 {
            0.0
        } else {
            t *= t;
            t * t * grad(hash(i + o[0] + hash(j + o[1] + hash(k + o[2]))), x, y, z)
        }
    };
    32.0 * (corner([0, 0, 0], 0.0) + corner(a, 1.0) + corner(b, 2.0) + corner([1, 1, 1], 3.0))
}
#[derive(Clone, Copy, Debug)]
pub struct Fractal {
    pub frequency: f32,
    pub amplitude: f32,
    pub lacunarity: f32,
    pub persistence: f32,
    pub octaves: u8,
}
impl Fractal {
    pub fn value(self, x: f32, y: f32, z: f32) -> f32 {
        let (mut sum, mut denom) = (0.0, 0.0);
        let (mut f, mut a) = (self.frequency, self.amplitude);
        for _ in 0..self.octaves {
            sum += a * noise(x * f, y * f, z * f);
            denom += a;
            f *= self.lacunarity;
            a *= self.persistence;
        }
        sum / denom
    }
}

#[test]
fn cpp_math_parity() {
    use super::{color::Rgb, hsv};
    let data = include_bytes!("fixtures/math-fixtures.bin");
    assert_eq!(data.len(), 4096 * 26);
    let noise = Fractal {
        frequency: 0.12,
        amplitude: 3.9,
        lacunarity: 0.75,
        persistence: 0.5,
        octaves: 2,
    };
    for (i, r) in data.as_chunks::<26>().0.iter().enumerate() {
        let u = |o| u32::from_le_bytes(r[o..o + 4].try_into().unwrap());
        let f = |o| f32::from_bits(u(o));
        let c = Rgb::new(
            ((i * 71) % 256) as u8,
            ((i * 113) % 256) as u8,
            ((i * 197) % 256) as u8,
        );
        let h = hsv::from_rgb(c);
        assert_eq!((h.hue, h.saturation, h.value), (u(0), r[4], r[5]));
        assert_eq!(hsv::to_rgb(h).native(), u(6));
        let actual = noise.value(f(10), f(14), f(18));
        assert!(
            (actual - f(22)).abs() < 0.000001,
            "noise {i}: {actual} != {}",
            f(22)
        );
    }
}
