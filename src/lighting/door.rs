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
pub const PIXEL_LEDS: usize = 153;

/// E104 preview body: statically recovered from Surplife and one-LED marker verified.
/// Brightness belongs only in the header; record values retain scene RGB intensity.
pub fn pixel_payload(colors: &[Rgb], brightness: i32) -> Result<Vec<u8>, &'static str> {
    if colors.len() != PIXEL_LEDS || !(1..=100).contains(&brightness) {
        return Err("pixel frame requires 153 colors and brightness 1..100");
    }
    let mut out = vec![0xe1, 4, 1, 0, 1, 80, brightness as u8, 0, PIXEL_LEDS as u8];
    for color in colors {
        let (r, g, b) = (
            color.r as f64 / 255.0,
            color.g as f64 / 255.0,
            color.b as f64 / 255.0,
        );
        let high = r.max(g).max(b);
        let delta = high - r.min(g).min(b);
        let hue = if delta == 0.0 {
            0.0
        } else if high == r {
            ((g - b) / delta).rem_euclid(6.0) * 60.0
        } else if high == g {
            ((b - r) / delta + 2.0) * 60.0
        } else {
            ((r - g) / delta + 4.0) * 60.0
        };
        let h = (hue as u16).min(359);
        let saturation = if high == 0.0 {
            0
        } else {
            (delta / high * 100.0) as u16
        };
        let hs = (h << 7) | saturation;
        out.extend_from_slice(&[0, (hs >> 8) as u8, hs as u8, (high * 100.0) as u8, 100]);
    }
    Ok(out)
}

/// V0 transport uses an eight-byte first header, then five-byte continuation headers.
pub fn pixel_fragments(
    colors: &[Rgb],
    brightness: i32,
    sequence: u8,
    pdu: u32,
) -> Result<Vec<Vec<u8>>, &'static str> {
    let payload = pixel_payload(colors, brightness)?;
    if colors.iter().all(|color| *color == Rgb::default()) {
        let off = off_packet(sequence);
        return if fits(off.len(), pdu) {
            Ok(vec![off])
        } else {
            Err("off packet exceeds negotiated ATT payload")
        };
    }
    let maximum = pdu.saturating_sub(4).min(255) as usize;
    if maximum < 9 {
        return Err("ATT payload too small for pixel fragments");
    }
    let length = payload.len();
    let first = maximum - 8;
    let mut packet = vec![
        0x40,
        sequence,
        0,
        0,
        (length >> 8) as u8,
        length as u8,
        (first + 1) as u8,
        0x0b,
    ];
    packet.extend_from_slice(&payload[..first]);
    let mut packets = vec![packet];
    let mut offset = first;
    let mut index = 1u16;
    while offset < length {
        let end = (offset + maximum - 5).min(length);
        let control = index | if end == length { 0x8000 } else { 0 };
        let mut packet = vec![
            0x40,
            sequence,
            (control >> 8) as u8,
            control as u8,
            (end - offset) as u8,
        ];
        packet.extend_from_slice(&payload[offset..end]);
        packets.push(packet);
        index += 1;
        offset = end;
    }
    if packets
        .iter()
        .any(|packet| packet.len() > maximum || !fits(packet.len(), pdu))
    {
        return Err("pixel fragment exceeds negotiated ATT payload");
    }
    Ok(packets)
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

#[test]
fn pixel_color_records_preserve_intensity_and_header_brightness() {
    let mut colors = [Rgb::default(); PIXEL_LEDS];
    colors[0] = Rgb::new(255, 0, 0);
    colors[1] = Rgb::new(0, 255, 0);
    colors[2] = Rgb::new(0, 0, 255);
    colors[3] = Rgb::new(128, 128, 128);
    let payload = pixel_payload(&colors, 15).unwrap();
    assert_eq!(payload.len(), 774);
    assert_eq!(&payload[..9], &[0xe1, 4, 1, 0, 1, 80, 15, 0, 153]);
    assert_eq!(&payload[9..14], &[0, 0, 100, 100, 100]);
    assert_eq!(&payload[14..19], &[0, 60, 100, 100, 100]);
    assert_eq!(&payload[19..24], &[0, 120, 100, 100, 100]);
    assert_eq!(&payload[24..29], &[0, 0, 0, 50, 100]);
    assert_eq!(&payload[29..34], &[0, 0, 0, 0, 100]);
    let full = pixel_payload(&colors, 100).unwrap();
    assert_eq!(&payload[9..], &full[9..]);
    assert!(pixel_payload(&colors[..152], 15).is_err());
    assert!(pixel_payload(&colors, 0).is_err());
    assert!(pixel_payload(&colors, 101).is_err());
}

#[test]
fn pixel_fragments_reassemble_in_order_with_one_sequence() {
    let colors = [Rgb::new(0, 255, 0); PIXEL_LEDS];
    let expected = pixel_payload(&colors, 15).unwrap();
    let packets = pixel_fragments(&colors, 15, 255, 200).unwrap();
    assert_eq!(
        packets.iter().map(Vec::len).collect::<Vec<_>>(),
        [196, 196, 196, 196, 18]
    );
    assert_eq!(&packets[0][..8], &[0x40, 255, 0, 0, 3, 6, 189, 0x0b]);
    for pdu in [13, 24, 68, 200, 259, u32::MAX] {
        let packets = pixel_fragments(&colors, 15, 255, pdu).unwrap();
        let mut body = packets[0][8..].to_vec();
        let maximum = pdu.saturating_sub(4).min(255) as usize;
        for (index, packet) in packets.iter().enumerate() {
            assert!(packet.len() <= maximum);
            assert!(fits(packet.len(), pdu));
            assert_eq!(packet[0], 0x40);
            assert_eq!(packet[1], 255);
            let control = u16::from_be_bytes([packet[2], packet[3]]);
            assert_eq!(control & 0x7fff, index as u16);
            assert_eq!(control & 0x8000 != 0, index == packets.len() - 1);
            if index > 0 {
                assert_eq!(packet[4] as usize, packet.len() - 5);
                body.extend_from_slice(&packet[5..]);
            }
        }
        assert_eq!(body, expected);
    }
    for pdu in [0, 3, 4, 12] {
        assert!(pixel_fragments(&colors, 15, 1, pdu).is_err());
    }
}

#[test]
fn black_pixel_frame_preserves_existing_off_packet() {
    assert_eq!(
        pixel_fragments(&[Rgb::default(); PIXEL_LEDS], 15, 9, 24).unwrap(),
        [off_packet(9)]
    );
    assert!(pixel_fragments(&[Rgb::default(); PIXEL_LEDS], 15, 9, 23).is_err());
}
