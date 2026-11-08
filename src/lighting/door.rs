// SPDX-License-Identifier: GPL-2.0-or-later
// Port of door-ble-plugin/Protocol.{cpp,h}.
use super::color::Rgb;
pub const COLOR_GROUPS: usize = 20;
pub const COLOR_PACKET_BYTES: usize = 95;
pub fn allowed(address: u64) -> bool {
    address > 0 && address < 0xffffffffffff
}
pub fn fits(length: usize, pdu: u32) -> bool {
    pdu > 3 && length <= (pdu - 3) as usize
}
pub fn off_packet(sequence: u8) -> Vec<u8> {
    vec![
        0, sequence, 0x80, 0, 0, 0x0d, 0x0e, 0x0b, 0x3b, 0x24, 0, 0, 0, 0, 0, 0, 0, 0x32, 0, 0,
        0x91,
    ]
}
pub fn color_packet(
    color: Rgb,
    brightness: i32,
    segments: i32,
    sequence: u8,
) -> Result<Vec<u8>, &'static str> {
    if !(1..=100).contains(&brightness) || !(1..=60).contains(&segments) {
        return Err("brightness or segment count out of range");
    }
    let (r, g, b) = (
        color.r as f64 / 255.0,
        color.g as f64 / 255.0,
        color.b as f64 / 255.0,
    );
    let high = r.max(g).max(b);
    let low = r.min(g).min(b);
    let delta = high - low;
    let mut hue = 0.0;
    if delta > 0.0 {
        hue = if high == r {
            (g - b) / delta + if g < b { 6.0 } else { 0.0 }
        } else if high == g {
            (b - r) / delta + 2.0
        } else {
            (r - g) / delta + 4.0
        };
        hue /= 6.0;
    }
    let h = ((hue * 360.0) as i32 / 2) as u8;
    let s = if high == 0.0 {
        0
    } else {
        (delta / high * 100.0) as u8
    };
    let v = (brightness * (high * 100.0) as i32 / 100).clamp(1, 100) as u8;
    let len = 7 + 4 * segments;
    let mut out = vec![
        0,
        sequence,
        0x80,
        0,
        (len >> 8) as u8,
        len as u8,
        (len + 1) as u8,
        0x0b,
        0xe1,
        3,
        0,
        segments as u8,
        0,
        0,
        segments as u8,
    ];
    for _ in 0..segments {
        out.extend_from_slice(&[0xa1, h, s, v]);
    }
    Ok(out)
}
pub fn frame_packet(
    colors: &[Rgb; COLOR_GROUPS],
    brightness: i32,
    sequence: u8,
) -> Result<Vec<u8>, &'static str> {
    if !(1..=100).contains(&brightness) {
        return Err("frame brightness out of range");
    }
    if colors.iter().all(|c| *c == Rgb::default()) {
        return Ok(off_packet(sequence));
    }
    let mut out = color_packet(Rgb::default(), brightness, COLOR_GROUPS as i32, sequence)?;
    for (i, c) in colors.iter().enumerate() {
        let packet = color_packet(*c, brightness, 1, sequence)?;
        out[15 + 4 * i..19 + 4 * i].copy_from_slice(&packet[15..19]);
    }
    Ok(out)
}
#[test]
fn packets() {
    assert_eq!(
        off_packet(7),
        vec![
            0, 7, 128, 0, 0, 13, 14, 11, 59, 36, 0, 0, 0, 0, 0, 0, 0, 50, 0, 0, 145
        ]
    );
    let p = color_packet(Rgb::new(255, 96, 16), 15, 20, 9).unwrap();
    assert_eq!(
        &p[..15],
        &[0, 9, 128, 0, 0, 87, 88, 11, 225, 3, 0, 20, 0, 0, 20]
    );
    assert_eq!(&p[15..19], &[161, 10, 93, 15]);
    assert_eq!(p.len(), 95);
    assert!(fits(95, 98));
    assert!(!fits(95, 97));
    assert!(!fits(0, 3));
    assert!(!allowed(0));
    assert!(!allowed(0xffffffffffff));
    assert!(color_packet(Rgb::default(), 0, 20, 0).is_err());
}
#[test]
fn black_semantics() {
    let mut c = [Rgb::default(); 20];
    assert_eq!(frame_packet(&c, 10, 1).unwrap(), off_packet(1));
    c[19] = Rgb::new(0, 0, 255);
    let p = frame_packet(&c, 10, 1).unwrap();
    assert_eq!(&p[15..19], &[161, 0, 0, 1]);
    assert_eq!(&p[91..], &[161, 120, 100, 10]);
}

#[test]
fn cpp_protocol_parity() {
    let data = include_bytes!("fixtures/protocol-fixtures.bin");
    let mut offset = 0;
    let mut count = 0;
    while offset < data.len() {
        let args = &data[offset..offset + 6];
        let n = data[offset + 6] as usize;
        let expected = &data[offset + 7..offset + 7 + n];
        assert_eq!(
            color_packet(
                Rgb::new(args[0], args[1], args[2]),
                args[3] as i32,
                args[4] as i32,
                args[5]
            )
            .unwrap(),
            expected,
            "fixture {count}"
        );
        offset += 7 + n;
        count += 1;
    }
    assert_eq!(count, 8192);
}
