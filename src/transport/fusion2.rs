// SPDX-License-Identifier: GPL-2.0-or-later
//! Bounded IT5702 two-ARGB-header direct RGB subset of OpenRGB Fusion2 USB.
//! No global reset, LED_C effects, calibration writes, flash or Gen2 commands.
use crate::lighting::color::Rgb;

pub const VID: u16 = 0x048d;
pub const PID: u16 = 0x5702;
pub const USAGE_PAGE: u16 = 0xff89;
pub const USAGE: u16 = 0x00cc;
pub const REPORT_BYTES: usize = 64;
pub const MAX_LEDS: usize = 256;
pub type Report = [u8; REPORT_BYTES];

/// Family/path preflight only; a native opener must still revalidate attributes,
/// usage and complete feature caps. The source does not require a fixed MI.
pub fn validate_path(path: &str) -> Result<(), String> {
    if path.len() > 4096
        || path.chars().any(char::is_control)
        || !path
            .get(..8)
            .is_some_and(|v| v.eq_ignore_ascii_case(r"\\?\hid#"))
    {
        return Err("Expected canonical Fusion2 HID path".into());
    }
    let mut parts = path.split('#');
    parts.next();
    let mut tokens = parts
        .next()
        .ok_or("Missing HID hardware tokens")?
        .split('&');
    for expected in ["vid_048d", "pid_5702"] {
        if !tokens
            .next()
            .is_some_and(|s| s.eq_ignore_ascii_case(expected))
        {
            return Err("Fusion2 VID/PID mismatch".into());
        }
    }
    let mut mi_seen = false;
    let mut col_seen = false;
    for token in tokens {
        let bytes = token.as_bytes();
        if bytes.len() == 5
            && bytes[..3].eq_ignore_ascii_case(b"mi_")
            && !mi_seen
            && !col_seen
            && bytes[3..].iter().all(u8::is_ascii_hexdigit)
        {
            mi_seen = true;
        } else if bytes.len() == 5
            && bytes[..3].eq_ignore_ascii_case(b"col")
            && !col_seen
            && bytes[3..].iter().all(u8::is_ascii_hexdigit)
        {
            col_seen = true;
        } else {
            return Err("Unexpected Fusion2 HID interface/collection token".into());
        }
    }
    if !parts.next().is_some_and(|s| !s.is_empty())
        || !parts.next().is_some_and(|s| !s.is_empty())
        || parts.next().is_some()
    {
        return Err("Incomplete Fusion2 HID interface path".into());
    }
    Ok(())
}

/// Feature reports only. Implementations must bound OS operations and release
/// their owned handle on Drop. Return the actual complete transfer length.
pub trait Transport {
    fn feature_send(&mut self, report: &Report) -> Result<usize, String>;
    fn feature_get(&mut self, report: &mut Report) -> Result<usize, String>;
    fn wait_ms(&mut self, milliseconds: u32);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub product: u8,
    pub device_num: u8,
    pub firmware: u32,
    pub description: String,
    pub chip_id: u32,
    pub support_flags: u8,
    pub count_codes: [u8; 6],
    /// Per-header byte positions of R, G, B, read from hardware (never written).
    pub orders: [[usize; 3]; 2],
}

pub fn command(opcode: u8, a: u8, b: u8) -> Result<Report, String> {
    // Deliberately small allow-list: no arbitrary calibration/persistent commands.
    if !matches!(opcode, 0x60 | 0x32 | 0x34) {
        return Err("Fusion2 command outside direct RGB subset".into());
    }
    let mut report = [0; REPORT_BYTES];
    report[..4].copy_from_slice(&[0xcc, opcode, a, b]);
    Ok(report)
}

pub fn parse_identity(report: &Report) -> Result<Identity, String> {
    if report[0] != 0xcc || report[2] != 0 {
        return Err("Unexpected Fusion2 report ID or controller number".into());
    }
    let read32 = |offset| u32::from_le_bytes(report[offset..offset + 4].try_into().unwrap());
    let mut count_codes = [0; 6];
    for (index, code) in count_codes.iter_mut().enumerate() {
        *code = (report[8 + index / 2] >> (4 * (index % 2))) & 15;
        if *code > 4 {
            return Err("Invalid Fusion2 LED division code".into());
        }
    }
    let mut orders = [[0; 3]; 2];
    for (index, order) in orders.iter_mut().enumerate() {
        let raw = read32(44 + index * 4);
        *order = [
            ((raw >> 16) & 255) as usize,
            ((raw >> 8) & 255) as usize,
            (raw & 255) as usize,
        ];
        if raw >> 24 != 0
            || order.iter().any(|&p| p > 2)
            || order[0] == order[1]
            || order[0] == order[2]
            || order[1] == order[2]
        {
            return Err("Unsupported/off/invalid Fusion2 ARGB calibration".into());
        }
    }
    let raw_name = &report[12..40];
    let raw_name = &raw_name[..raw_name
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(raw_name.len())];
    if !raw_name.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        return Err("Invalid Fusion2 product description".into());
    }
    Ok(Identity {
        product: report[1],
        device_num: report[2],
        firmware: read32(4),
        description: String::from_utf8(raw_name.to_vec()).map_err(|e| e.to_string())?,
        chip_id: read32(56),
        support_flags: report[11],
        count_codes,
        orders,
    })
}

pub fn count_code(count: usize) -> Result<u8, String> {
    match count {
        1..=32 => Ok(0),
        33..=64 => Ok(1),
        65..=MAX_LEDS => Ok(2),
        _ => Err("IT5702 header requires 1..=256 LEDs".into()),
    }
}

/// Preserve all count nibbles except the two explicitly owned ARGB headers.
pub fn count_report(identity: &Identity, counts: [usize; 2]) -> Result<Option<Report>, String> {
    let mut codes = identity.count_codes;
    codes[0] = count_code(counts[0])?;
    codes[1] = count_code(counts[1])?;
    if codes.iter().any(|&c| c > 4) {
        return Err("Invalid count code".into());
    }
    if codes == identity.count_codes {
        return Ok(None);
    }
    let mut report = command(0x34, codes[0] | codes[1] << 4, codes[2] | codes[3] << 4)?;
    report[4] = codes[4] | codes[5] << 4;
    Ok(Some(report))
}

pub fn direct_report() -> Report {
    // Source SetStripBuiltinEffectState(-1,false), default two-header PID family.
    command(0x32, 0x03, 0).unwrap()
}

pub fn packets(channel: u8, colors: &[Rgb], order: [usize; 3]) -> Result<Vec<Report>, String> {
    count_code(colors.len())?;
    if channel > 1
        || order.iter().any(|&p| p > 2)
        || order[0] == order[1]
        || order[0] == order[2]
        || order[1] == order[2]
    {
        return Err("Invalid Fusion2 ARGB header or byte order".into());
    }
    let mut packet = [0; REPORT_BYTES];
    packet[0] = 0xcc;
    packet[1] = 0x58 + channel;
    let mut reports = Vec::with_capacity(colors.len().div_ceil(19));
    for (chunk, colors) in colors.chunks(19).enumerate() {
        packet[2..4].copy_from_slice(&((chunk * 19 * 3) as u16).to_le_bytes());
        packet[4] = (colors.len() * 3) as u8;
        for (index, color) in colors.iter().enumerate() {
            for (offset, value) in order.into_iter().zip([color.r, color.g, color.b]) {
                packet[5 + index * 3 + offset] = value;
            }
        }
        // Native PktRGB reuses its payload: unused final-packet bytes retain
        // previous-chunk data. bcount alone defines valid payload bytes.
        reports.push(packet);
    }
    Ok(reports)
}

pub struct Controller<T: Transport> {
    transport: Option<T>,
    identity: Identity,
    counts: [usize; 2],
    last_frames: [Option<Vec<Rgb>>; 2],
    failed: Option<String>,
    submitted: [u64; 2],
}

fn send<T: Transport>(transport: &mut T, report: &Report) -> Result<(), String> {
    if transport.feature_send(report)? != REPORT_BYTES {
        return Err("Short Fusion2 feature send".into());
    }
    Ok(())
}

/// Read-only information query; no lighting mode, count or color writes.
pub fn query<T: Transport>(transport: &mut T) -> Result<Identity, String> {
    send(transport, &command(0x60, 0, 0)?)?;
    let mut report = [0; REPORT_BYTES];
    report[0] = 0xcc;
    if transport.feature_get(&mut report)? != REPORT_BYTES {
        return Err("Short Fusion2 identity feature read".into());
    }
    parse_identity(&report)
}

impl<T: Transport> Controller<T> {
    /// Takes exclusive ownership of both ARGB headers. The enclosing native
    /// adapter must already pin VID/PID/usage/path; this function never enumerates.
    pub fn connect(mut transport: T, counts: [usize; 2]) -> Result<Self, String> {
        count_code(counts[0])?;
        count_code(counts[1])?;
        let identity = query(&mut transport)?;
        // Unlike the native constructor, do not ResetController/ApplyFF or
        // toggle global Beat/LampArray: those could alter unowned LED_C output.
        if let Some(report) = count_report(&identity, counts)? {
            send(&mut transport, &report)?;
        }
        send(&mut transport, &direct_report())?;
        transport.wait_ms(50);
        Ok(Self {
            transport: Some(transport),
            identity,
            counts,
            last_frames: [None, None],
            failed: None,
            submitted: [0, 0],
        })
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    pub fn counts(&self) -> [usize; 2] {
        self.counts
    }
    pub fn failure(&self) -> Option<&str> {
        self.failed.as_deref()
    }
    pub fn submitted(&self) -> [u64; 2] {
        self.submitted
    }
    pub fn last_frame(&self, channel: u8) -> Option<&[Rgb]> {
        self.last_frames.get(channel as usize)?.as_deref()
    }
    /// Stop only drops this controller's transport; no implicit blackout/reset.
    pub fn stop(&mut self) {
        self.transport = None;
    }

    /// Updates one owned channel without rewriting the other's colors or LED_C.
    /// Success means accepted by the feature transport, not physical LED ACK.
    pub fn update(&mut self, channel: u8, colors: &[Rgb]) -> Result<(), String> {
        if let Some(failed) = &self.failed {
            return Err(failed.clone());
        }
        let index = channel as usize;
        if self.counts.get(index) != Some(&colors.len()) {
            return Err("Fusion2 channel/count changed".into());
        }
        let reports = packets(channel, colors, self.identity.orders[index])?;
        let transport = self.transport.as_mut().ok_or("Fusion2 output is stopped")?;
        for report in reports {
            if let Err(error) = send(transport, &report) {
                self.failed = Some(error.clone());
                self.transport = None;
                return Err(error);
            }
        }
        self.last_frames[index] = Some(colors.to_vec());
        self.submitted[index] = self.submitted[index].saturating_add(1);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    fn unhex(s: &str) -> Report {
        let bytes: Vec<_> = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect();
        bytes.try_into().unwrap()
    }
    fn colors(count: usize) -> Vec<Rgb> {
        (0..count)
            .map(|i| {
                Rgb::new(
                    (i * 37 + 17) as u8,
                    (i * 73 + 29) as u8,
                    (i * 101 + 43) as u8,
                )
            })
            .collect()
    }
    fn identities() -> Vec<Report> {
        include_str!("fixtures/fusion2-native.tsv")
            .lines()
            .filter_map(|line| {
                let f: Vec<_> = line.split('\t').collect();
                (f[0] == "I").then(|| unhex(f[2]))
            })
            .collect()
    }
    #[derive(Default)]
    struct State {
        sent: Vec<Report>,
        waits: Vec<u32>,
        reads: usize,
        drop_count: usize,
        fail_at: Option<usize>,
        short_at: Option<usize>,
        read_len: Option<usize>,
    }
    struct Mock {
        state: Rc<RefCell<State>>,
        info: Report,
    }
    impl Drop for Mock {
        fn drop(&mut self) {
            self.state.borrow_mut().drop_count += 1;
        }
    }
    impl Transport for Mock {
        fn feature_send(&mut self, report: &Report) -> Result<usize, String> {
            let mut s = self.state.borrow_mut();
            let index = s.sent.len();
            s.sent.push(*report);
            if s.fail_at == Some(index) {
                Err("synthetic transport error".into())
            } else if s.short_at == Some(index) {
                Ok(63)
            } else {
                Ok(64)
            }
        }
        fn feature_get(&mut self, report: &mut Report) -> Result<usize, String> {
            assert_eq!(report[0], 0xcc);
            *report = self.info;
            let mut s = self.state.borrow_mut();
            s.reads += 1;
            Ok(s.read_len.unwrap_or(64))
        }
        fn wait_ms(&mut self, ms: u32) {
            self.state.borrow_mut().waits.push(ms);
        }
    }
    fn mock(info: Report) -> (Mock, Rc<RefCell<State>>) {
        let state = Rc::new(RefCell::new(State::default()));
        (
            Mock {
                state: state.clone(),
                info,
            },
            state,
        )
    }
    #[test]
    fn actual_cpp_corpus_all_calibrations_headers_and_packet_boundaries() {
        let infos = identities();
        assert_eq!(infos.len(), 6);
        let mut identity_cases = 0;
        let mut init_cases = 0;
        let mut packet_cases = 0;
        for line in include_str!("fixtures/fusion2-native.tsv").lines() {
            let f: Vec<_> = line.split('\t').collect();
            let permutation = f[1].parse::<usize>().unwrap();
            let id = parse_identity(&infos[permutation]).unwrap();
            match f[0] {
                "I" => {
                    identity_cases += 1;
                    assert_eq!(id.firmware, 0x04030201);
                    assert_eq!(id.chip_id, 0x12345678);
                    assert_eq!(id.description, "SYNTHETIC IT5702");
                    assert_eq!(id.count_codes, [1, 1, 3, 4, 2, 0]);
                }
                "N" => {
                    init_cases += 1;
                    let (mock, state) = mock(infos[permutation]);
                    let _controller = Controller::connect(mock, [27, 27]).unwrap();
                    let expected: Vec<Report> = f[2..].iter().map(|s| unhex(s)).collect();
                    assert_eq!(state.borrow().sent, expected);
                    assert_eq!(state.borrow().waits, vec![50]);
                    assert_eq!(state.borrow().reads, 1);
                }
                "P" => {
                    packet_cases += 1;
                    let channel = f[2].parse::<u8>().unwrap();
                    let count = f[3].parse::<usize>().unwrap();
                    let expected: Vec<Report> = f[4..].iter().map(|s| unhex(s)).collect();
                    assert_eq!(
                        packets(channel, &colors(count), id.orders[channel as usize]).unwrap(),
                        expected,
                        "permutation{permutation} channel{channel} count{count}"
                    );
                }
                _ => panic!("Unexpected oracle record"),
            }
        }
        assert_eq!((identity_cases, init_cases, packet_cases), (6, 6, 108));
    }
    #[test]
    fn two_channels_preserve_each_other_and_never_address_led_c() {
        let (mock, state) = mock(identities()[0]);
        let mut c = Controller::connect(mock, [27, 27]).unwrap();
        let a = colors(27);
        let b = vec![Rgb::new(4, 5, 6); 27];
        c.update(0, &a).unwrap();
        c.update(1, &b).unwrap();
        assert_eq!(c.last_frame(0), Some(a.as_slice()));
        assert_eq!(c.last_frame(1), Some(b.as_slice()));
        let before = state.borrow().sent.len();
        c.update(0, &[Rgb::new(7, 8, 9); 27]).unwrap();
        assert_eq!(c.last_frame(1), Some(b.as_slice()));
        assert!(state.borrow().sent[before..].iter().all(|p| p[1] == 0x58));
        assert!(
            state
                .borrow()
                .sent
                .iter()
                .all(|p| matches!(p[1], 0x60 | 0x34 | 0x32 | 0x58 | 0x59))
        );
        assert_eq!(c.submitted(), [2, 1]);
        let sent = state.borrow().sent.len();
        c.stop();
        assert_eq!(state.borrow().drop_count, 1);
        assert!(c.update(0, &a).is_err());
        assert_eq!(state.borrow().sent.len(), sent);
    }
    #[test]
    fn partial_frame_failure_is_terminal_and_releases_only_owned_transport() {
        for short in [false, true] {
            let (mock, state) = mock(identities()[0]);
            let mut c = Controller::connect(mock, [27, 27]).unwrap();
            let a = colors(27);
            c.update(0, &a).unwrap();
            let fail_at = state.borrow().sent.len() + 1;
            if short {
                state.borrow_mut().short_at = Some(fail_at);
            } else {
                state.borrow_mut().fail_at = Some(fail_at);
            }
            assert!(c.update(1, &a).is_err());
            assert!(c.failure().is_some());
            assert_eq!(state.borrow().drop_count, 1);
            assert_eq!(c.last_frame(0), Some(a.as_slice()));
            assert_eq!(c.last_frame(1), None);
            assert_eq!(c.submitted(), [1, 0]);
            let sent = state.borrow().sent.len();
            assert!(c.update(0, &a).is_err());
            assert_eq!(state.borrow().sent.len(), sent);
        }
    }
    #[test]
    fn failed_or_invalid_initialization_stops_before_later_writes() {
        for fail_at in 0..3 {
            let (mock, state) = mock(identities()[0]);
            state.borrow_mut().short_at = Some(fail_at);
            assert!(Controller::connect(mock, [27, 27]).is_err());
            assert_eq!(state.borrow().sent.len(), fail_at + 1);
            assert_eq!(state.borrow().drop_count, 1);
        }
        for read_len in [0, 63, 65] {
            let (mock, state) = mock(identities()[0]);
            state.borrow_mut().read_len = Some(read_len);
            assert!(Controller::connect(mock, [27, 27]).is_err());
            assert_eq!(state.borrow().sent.len(), 1);
            assert_eq!(state.borrow().drop_count, 1);
        }
        for offset in [0, 2, 8, 44] {
            let mut info = identities()[0];
            info[offset] = 0xff;
            let (mock, state) = mock(info);
            assert!(Controller::connect(mock, [27, 27]).is_err());
            assert_eq!(state.borrow().sent.len(), 1);
        }
        let (mock, state) = mock(identities()[0]);
        assert!(Controller::connect(mock, [0, 27]).is_err());
        assert!(state.borrow().sent.is_empty());
    }
    #[test]
    fn strict_packet_and_count_bounds_no_persistent_or_calibration_commands() {
        let id = parse_identity(&identities()[0]).unwrap();
        for count in [0, 257, usize::MAX] {
            assert!(count_code(count).is_err());
        }
        for (count, expected) in [(1, 0), (32, 0), (33, 1), (64, 1), (65, 2), (256, 2)] {
            assert_eq!(count_code(count).unwrap(), expected);
        }
        assert!(count_report(&id, [64, 64]).unwrap().is_none());
        let report = count_report(&id, [256, 33]).unwrap().unwrap();
        assert_eq!(&report[..5], &[0xcc, 0x34, 0x12, 0x43, 0x02]);
        for opcode in [0x20, 0x28, 0x31, 0x33, 0x38, 0x47, 0x48, 0x5e, 0x61] {
            assert!(command(opcode, 0, 0).is_err());
        }
        assert!(packets(2, &colors(27), [0, 1, 2]).is_err());
        assert!(packets(0, &[], [0, 1, 2]).is_err());
        assert!(packets(0, &colors(27), [0, 0, 0]).is_err());
    }
    #[test]
    fn standalone_query_is_read_only_and_does_not_release_callers_transport() {
        let (mut mock, state) = mock(identities()[0]);
        query(&mut mock).unwrap();
        assert_eq!(state.borrow().sent, vec![command(0x60, 0, 0).unwrap()]);
        assert_eq!(state.borrow().reads, 1);
        assert!(state.borrow().waits.is_empty());
        assert_eq!(state.borrow().drop_count, 0);
    }
}
