// SPDX-License-Identifier: GPL-2.0-or-later
// Port of OpenRGB 1.1 SkydimoSerialController (Bartholomew Ho, 2026).
use crate::lighting::color::Rgb;

pub const BAUD_RATE: u32 = 115_200;
pub const QUERY: &[u8; 6] = b"Moni-A";
pub const MAX_RESPONSE_BYTES: usize = 63;
pub const MAX_FRAME_LEDS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub model: String,
    pub serial: String,
}

/// Decode one complete response; serial bytes are binary, including embedded NULs.
/// Reject malformed model fields rather than inheriting C++'s permissive suffix handling.
pub fn parse_identity(bytes: &[u8]) -> Result<Identity, String> {
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err("Skydimo identity exceeds 63 bytes".into());
    }
    let end = bytes
        .iter()
        .rposition(|b| *b != b'\r' && *b != b'\n')
        .map_or(0, |i| i + 1);
    let bytes = &bytes[..end];
    if bytes.len() < 6
        || !bytes[..2].eq_ignore_ascii_case(b"SK")
        || !bytes[..6].iter().all(u8::is_ascii_alphanumeric)
        || (bytes.len() > 6 && bytes[6] != b',')
    {
        return Err("invalid Skydimo identity model field".into());
    }
    let model = String::from_utf8(bytes[..6].to_ascii_uppercase()).map_err(|e| e.to_string())?;
    let mut serial = String::new();
    for byte in bytes.get(7..).unwrap_or_default() {
        use std::fmt::Write;
        write!(&mut serial, "{byte:02X}").map_err(|e| e.to_string())?;
    }
    if serial.is_empty() {
        serial = "000000".into();
    }
    Ok(Identity { model, serial })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Layout {
    None = 0,
    Strip = 1,
    Perimeter3 = 2,
    Perimeter4 = 3,
    Sides2 = 4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Model {
    pub id: &'static str,
    pub layout: Layout,
    pub zones: [usize; 4],
    pub leds: usize,
    pub min_leds: usize,
    pub max_leds: usize,
    pub resizable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Matrix {
    pub width: usize,
    pub height: usize,
    /// Row-major cells contain native wire-order LED indices; None is a hole.
    pub cells: Vec<Option<usize>>,
}

impl Model {
    pub fn matrix(&self) -> Result<Option<Matrix>, String> {
        if self.leds > MAX_FRAME_LEDS || self.zones.iter().any(|&z| z > MAX_FRAME_LEDS) {
            return Err("Skydimo model exceeds supported dimensions".into());
        }
        if matches!(self.layout, Layout::None | Layout::Strip) {
            return Ok(None);
        }
        let [z1, z2, z3, z4] = self.zones;
        if z1 + z2 + z3 + z4 != self.leds {
            return Err("Skydimo zone count does not match LED count".into());
        }
        let (width, height) = match self.layout {
            Layout::Perimeter4 => (z2.max(z4) + 2, z1.max(z3) + 2),
            Layout::Perimeter3 => (z2 + 2, z1.max(z3) + 1),
            Layout::Sides2 => {
                let height = z1.max(z2) + 2;
                (3.max((16 * height + 4) / 9), height)
            }
            _ => unreachable!(),
        };
        let mut cells = vec![None; width * height];
        let mut index = 0;
        let mut put = |y: usize, x: usize| -> Result<(), String> {
            let cell = cells
                .get_mut(y * width + x)
                .ok_or("Skydimo matrix cell outside bounds")?;
            if x >= width || y >= height || cell.is_some() || index >= self.leds {
                return Err("invalid Skydimo matrix placement".into());
            }
            *cell = Some(index);
            index += 1;
            Ok(())
        };
        let start_y = if self.layout == Layout::Perimeter3 {
            height - 1
        } else {
            height - 2
        };
        let first_x = if self.layout == Layout::Sides2 {
            0
        } else {
            width - 1
        };
        for y in (1..=start_y).rev().take(z1) {
            put(y, first_x)?;
        }
        if self.layout == Layout::Sides2 {
            for y in (1..=height - 2).take(z2) {
                put(y, width - 1)?;
            }
        } else {
            for x in (1..=width - 2).rev().take(z2) {
                put(0, x)?;
            }
        }
        for y in (1..=start_y).take(z3) {
            put(y, 0)?;
        }
        if self.layout == Layout::Perimeter4 {
            for x in (1..=width - 2).take(z4) {
                put(height - 1, x)?;
            }
        }
        if index != self.leds {
            return Err("incomplete Skydimo matrix".into());
        }
        Ok(Some(Matrix {
            width,
            height,
            cells,
        }))
    }
}

const fn fixed(id: &'static str, layout: Layout, zones: [usize; 4], leds: usize) -> Model {
    Model {
        id,
        layout,
        zones,
        leds,
        min_leds: leds,
        max_leds: leds,
        resizable: false,
    }
}

pub const MODELS: &[Model] = &[
    fixed("SK0121", Layout::Perimeter3, [13, 25, 13, 0], 51),
    fixed("SK0124", Layout::Perimeter3, [14, 26, 14, 0], 54),
    fixed("SK0127", Layout::Perimeter3, [17, 31, 17, 0], 65),
    fixed("SK0132", Layout::Perimeter3, [20, 37, 20, 0], 77),
    fixed("SK0134", Layout::Perimeter3, [15, 41, 15, 0], 71),
    fixed("SK0149", Layout::Perimeter3, [19, 69, 19, 0], 107),
    fixed("SK0201", Layout::Sides2, [20, 20, 0, 0], 40),
    fixed("SK0202", Layout::Sides2, [30, 30, 0, 0], 60),
    fixed("SK0204", Layout::Sides2, [25, 25, 0, 0], 50),
    fixed("SK0402", Layout::Strip, [72, 0, 0, 0], 72),
    fixed("SK0403", Layout::Strip, [96, 0, 0, 0], 96),
    fixed("SK0404", Layout::Strip, [144, 0, 0, 0], 144),
    Model {
        id: "SK0410",
        layout: Layout::None,
        zones: [0; 4],
        leds: 0,
        min_leds: 0,
        max_leds: 300,
        resizable: true,
    },
    fixed("SK0801", Layout::Strip, [2, 0, 0, 0], 2),
    fixed("SK0802", Layout::Strip, [18, 0, 0, 0], 18),
    fixed("SK0803", Layout::Strip, [10, 0, 0, 0], 10),
    fixed("SK0901", Layout::Strip, [14, 0, 0, 0], 14),
    fixed("SK0E01", Layout::Strip, [16, 0, 0, 0], 16),
    fixed("SK0F01", Layout::Sides2, [29, 29, 0, 0], 58),
    fixed("SK0F02", Layout::Sides2, [25, 25, 0, 0], 50),
    fixed("SK0H01", Layout::Strip, [2, 0, 0, 0], 2),
    fixed("SK0H02", Layout::Strip, [4, 0, 0, 0], 4),
    fixed("SK0K01", Layout::Strip, [120, 0, 0, 0], 120),
    fixed("SK0K02", Layout::Strip, [15, 0, 0, 0], 15),
    fixed("SK0L21", Layout::Perimeter4, [13, 25, 13, 25], 76),
    fixed("SK0L24", Layout::Perimeter4, [14, 26, 14, 26], 80),
    fixed("SK0L27", Layout::Perimeter4, [17, 31, 17, 31], 96),
    fixed("SK0L32", Layout::Perimeter4, [20, 37, 20, 37], 114),
    fixed("SK0L34", Layout::Perimeter4, [15, 41, 15, 41], 112),
    fixed("SK0M01", Layout::Strip, [24, 0, 0, 0], 24),
    fixed("SK0N01", Layout::Strip, [256, 0, 0, 0], 256),
    fixed("SK0N02", Layout::Strip, [1024, 0, 0, 0], 1024),
    fixed("SK0N03", Layout::Strip, [253, 0, 0, 0], 253),
    fixed("SK0S01", Layout::Strip, [32, 0, 0, 0], 32),
    fixed("SKA124", Layout::Perimeter3, [18, 34, 18, 0], 70),
    fixed("SKA127", Layout::Perimeter3, [20, 41, 20, 0], 81),
    fixed("SKA132", Layout::Perimeter3, [25, 45, 25, 0], 95),
    fixed("SKA134", Layout::Perimeter3, [21, 51, 21, 0], 93),
    fixed("SKB124", Layout::Perimeter4, [18, 34, 18, 34], 104),
    fixed("SKB127", Layout::Perimeter4, [20, 41, 20, 41], 122),
    fixed("SKB132", Layout::Perimeter4, [25, 44, 25, 44], 138),
    fixed("SKB134", Layout::Perimeter4, [21, 50, 21, 50], 142),
];

pub fn model(id: &str) -> Option<Model> {
    MODELS.iter().copied().find(|entry| entry.id == id)
}

/// One complete Ada frame, with a big-endian last-index field and RGB channel order.
pub fn packet(colors: &[Rgb]) -> Result<Vec<u8>, String> {
    if colors.is_empty() || colors.len() > MAX_FRAME_LEDS {
        return Err("Skydimo frame needs 1..=1024 LEDs".into());
    }
    let mut bytes = Vec::with_capacity(6 + 3 * colors.len());
    bytes.extend_from_slice(b"Ada\0");
    bytes.extend_from_slice(&((colors.len() - 1) as u16).to_be_bytes());
    for color in colors {
        bytes.extend_from_slice(&[color.r, color.g, color.b]);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_controller_corpus() {
        let mut models = 0;
        let mut frames = 0;
        let mut identities = 0;
        let unhex = |s: &str| -> Vec<u8> {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect()
        };
        for line in include_str!("fixtures/skydimo-native.tsv").lines() {
            let f: Vec<_> = line.split('\t').collect();
            match f[0] {
                "M" => {
                    models += 1;
                    let m = model(f[1]).unwrap();
                    let n = |i: usize| f[i].parse::<usize>().unwrap();
                    assert_eq!(
                        (
                            m.layout as usize,
                            m.leds,
                            m.min_leds,
                            m.max_leds,
                            m.resizable
                        ),
                        (n(2), n(3), n(4), n(5), n(6) != 0),
                        "{}",
                        m.id
                    );
                    let matrix = m.matrix().unwrap();
                    assert_eq!(
                        matrix.as_ref().map_or((0, 0), |m| (m.width, m.height)),
                        (n(7), n(8)),
                        "{}",
                        m.id
                    );
                    let actual = matrix.map_or_else(Vec::new, |m| {
                        m.cells
                            .into_iter()
                            .map(|c| c.map_or(u32::MAX, |c| c as u32))
                            .collect::<Vec<_>>()
                    });
                    let positions: Vec<usize> = f[9]
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(|s| s.parse().unwrap())
                        .collect();
                    let mut expected = vec![u32::MAX; n(7) * n(8)];
                    for (led, position) in positions.into_iter().enumerate() {
                        assert_eq!(expected[position], u32::MAX);
                        expected[position] = led as u32;
                    }
                    assert_eq!(actual, expected, "{}", m.id);
                    let mut mapped: Vec<_> =
                        actual.into_iter().filter(|&c| c != u32::MAX).collect();
                    mapped.sort_unstable();
                    if !mapped.is_empty() {
                        assert_eq!(mapped, (0..m.leds as u32).collect::<Vec<_>>());
                    }
                }
                "P" => {
                    frames += 1;
                    let count = f[1].parse::<usize>().unwrap();
                    let colors: Vec<_> = (0..count)
                        .map(|i| {
                            Rgb::new(
                                (i * 37 + 17) as u8,
                                (i * 73 + 29) as u8,
                                (i * 101 + 43) as u8,
                            )
                        })
                        .collect();
                    assert_eq!(packet(&colors).unwrap(), unhex(f[2]), "count {count}");
                }
                "I" => {
                    identities += 1;
                    let parsed = parse_identity(&unhex(f[1]));
                    if f[2] == "1" {
                        assert_eq!(
                            parsed.unwrap(),
                            Identity {
                                model: f[3].into(),
                                serial: f[4].into()
                            }
                        );
                    } else {
                        assert!(parsed.is_err());
                    }
                }
                _ => panic!("unknown corpus record"),
            }
        }
        assert_eq!((models, frames, identities), (42, 11, 7));
    }

    #[test]
    fn strict_bounds_and_wire_order() {
        for bytes in [
            b"".as_slice(),
            b"SK013",
            b"SK01322",
            b"SK,132",
            b"SK\xFF132",
            b"SK0132junk,abc",
            b"SK0132\0",
        ] {
            assert!(parse_identity(bytes).is_err(), "{bytes:?}");
        }
        let mut bounded = b"SK0132,".to_vec();
        bounded.extend_from_slice(&[0x80; 56]);
        assert!(parse_identity(&bounded).is_ok());
        bounded.push(b'\n');
        assert!(parse_identity(&bounded).is_err());
        assert_eq!(parse_identity(b"skzzzz").unwrap().model, "SKZZZZ");
        assert!(model("SKZZZZ").is_none());
        assert!(packet(&[]).is_err());
        assert!(packet(&vec![Rgb::default(); 1025]).is_err());
        assert_eq!(
            packet(&[Rgb::new(1, 2, 3)]).unwrap(),
            b"Ada\0\0\0\x01\x02\x03"
        );
        let matrix = model("SK0132").unwrap().matrix().unwrap().unwrap();
        assert_eq!((matrix.width, matrix.height), (39, 21));
        assert_eq!(matrix.cells[20 * 39 + 38], Some(0));
        assert_eq!(matrix.cells[39 + 38], Some(19));
        assert_eq!(matrix.cells[37], Some(20));
        assert_eq!(matrix.cells[1], Some(56));
        assert_eq!(matrix.cells[39], Some(57));
        assert_eq!(matrix.cells[20 * 39], Some(76));
    }
}
