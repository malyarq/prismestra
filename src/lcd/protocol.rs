// SPDX-License-Identifier: GPL-2.0-or-later
//! Pure TT LCD JPEG framing and injected transport state; no device access.
//! JPEG validation is structural only: it does not decode entropy or prove pixels.
pub const PAYLOAD: usize = 1020;
pub const REPORT_BYTES: usize = 1025;
pub const MAX_FRAME: usize = PAYLOAD * 255;
pub const ACK_BYTES: usize = 17;
pub const FRAME_DEADLINE_MS: u64 = 3000;
pub const REPORT_DEADLINE_MS: u64 = 250;
/// Exact structural gate from native Protocol.cpp, including its early SOS return.
pub fn validate_jpeg(j: &[u8]) -> Result<(), &'static str> {
    if j.len() < 4
        || j.len() > MAX_FRAME
        || !j.starts_with(&[0xff, 0xd8])
        || !j.ends_with(&[0xff, 0xd9])
    {
        return Err("JPEG empty, truncated or exceeds 260100 bytes");
    }
    let mut sof = false;
    let mut p = 2;
    while p + 3 < j.len() {
        if j[p] != 0xff {
            return Err("Malformed JPEG marker");
        }
        p += 1;
        while p < j.len() && j[p] == 0xff {
            p += 1
        }
        if p + 2 >= j.len() {
            return Err("Truncated JPEG marker");
        }
        let marker = j[p];
        p += 1;
        let len = u16::from_be_bytes([j[p], j[p + 1]]) as usize;
        if len < 2 || p + len > j.len() {
            return Err("Truncated JPEG segment");
        }
        if marker == 0xc0 {
            if sof
                || len != 17
                || j[p + 2] != 8
                || j[p + 3] != 1
                || j[p + 4] != 224
                || j[p + 5] != 1
                || j[p + 6] != 224
                || j[p + 7] != 3
            {
                return Err("JPEG must be baseline RGB 480x480");
            }
            for c in 0..3 {
                if j[p + 8 + 3 * c] != (c + 1) as u8 || j[p + 9 + 3 * c] != 0x11 {
                    return Err("JPEG must have 4:4:4 sampling and component IDs 1,2,3");
                }
            }
            sof = true;
        } else if (0xc1..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
            return Err("Non-baseline JPEG is unsupported");
        }
        if marker == 0xda {
            return if sof {
                Ok(())
            } else {
                Err("JPEG has no baseline frame header")
            };
        }
        p += len;
    }
    Err("JPEG has no scan")
}
/// Packet framing does not imply JPEG validity, matching native tt::packet.
pub fn packet_count(frame: &[u8]) -> Result<usize, &'static str> {
    if frame.is_empty() || frame.len() > MAX_FRAME {
        Err("LCD frame empty or oversized")
    } else {
        Ok(frame.len().div_ceil(PAYLOAD))
    }
}
pub fn packet(frame: &[u8], index: usize) -> Result<[u8; REPORT_BYTES], &'static str> {
    let count = packet_count(frame)?;
    if index >= count {
        return Err("LCD packet index out of range");
    }
    let mut out = [0u8; REPORT_BYTES];
    out[1] = 0x08;
    out[2] = if index == 0 {
        count as u8
    } else {
        ((index - 1) % 0x15 + 1) as u8
    };
    out[4] = if index == 0 { 0x80 } else { 0 };
    let start = index * PAYLOAD;
    let n = PAYLOAD.min(frame.len() - start);
    out[5..5 + n].copy_from_slice(&frame[start..start + n]);
    Ok(out)
}
/// The source establishes only full 17-byte read and report ID zero.
/// Other ACK bytes are deliberately uninterpreted, not an invented command token.
pub fn validate_ack(response: &[u8]) -> Result<(), &'static str> {
    if response.len() != ACK_BYTES {
        return Err("Short LCD report; stream stopped");
    }
    if response[0] != 0 {
        return Err("Unexpected LCD input report ID");
    }
    Ok(())
}
/// Injected clock and reports. No implementation here opens HID or owns a real device.
/// Implementations must honor the timeout argument; elapsed includes caller composition.
pub trait ReportTransport {
    fn now_ms(&self) -> u64;
    fn write_report(
        &mut self,
        report: &[u8; REPORT_BYTES],
        timeout_ms: u64,
    ) -> Result<usize, String>;
    fn read_ack(
        &mut self,
        response: &mut [u8; ACK_BYTES],
        timeout_ms: u64,
    ) -> Result<usize, String>;
}
/// Owning trait objects are constructed on the I/O thread; this adds no Send assumption.
impl<T: ReportTransport + ?Sized> ReportTransport for Box<T> {
    fn now_ms(&self) -> u64 {
        (**self).now_ms()
    }
    fn write_report(
        &mut self,
        report: &[u8; REPORT_BYTES],
        timeout_ms: u64,
    ) -> Result<usize, String> {
        (**self).write_report(report, timeout_ms)
    }
    fn read_ack(
        &mut self,
        response: &mut [u8; ACK_BYTES],
        timeout_ms: u64,
    ) -> Result<usize, String> {
        (**self).read_ack(response, timeout_ms)
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum StreamState {
    #[default]
    Ready,
    BlankAcked,
    Stopped(String),
}
#[derive(Debug, Default)]
pub struct Sender {
    state: StreamState,
    frames_sent: u64,
    latest_frame: Vec<u8>,
}
impl Sender {
    pub fn state(&self) -> StreamState {
        self.state.clone()
    }
    pub fn frames_sent(&self) -> u64 {
        self.frames_sent
    }
    pub fn latest_frame(&self) -> &[u8] {
        &self.latest_frame
    }
    /// True means an ACK for caller-supplied blank JPEG, not proof its pixels are black.
    pub fn blank_acked(&self) -> bool {
        self.state == StreamState::BlankAcked
    }
    /// Finish a complete frame and ACK before publishing software progress. Failures stop
    /// the sender; already delivered packets cannot be undone or treated as a full frame.
    pub fn send_frame<T: ReportTransport>(
        &mut self,
        jpeg: &[u8],
        blank: bool,
        work_started_ms: u64,
        transport: &mut T,
    ) -> Result<(), String> {
        if self.state != StreamState::Ready {
            return Err("LCD sender is no longer ready".into());
        }
        let result: Result<(), String> = (|| {
            validate_jpeg(jpeg)?;
            for index in 0..packet_count(jpeg)? {
                let elapsed = transport
                    .now_ms()
                    .checked_sub(work_started_ms)
                    .ok_or("Invalid monotonic LCD clock")?;
                // Native tests >3000 only before writes; the last write/ACK can finish later.
                if elapsed > FRAME_DEADLINE_MS {
                    return Err("LCD frame exceeded 3 seconds; stream stopped".into());
                }
                let report = packet(jpeg, index)?;
                if transport.write_report(&report, REPORT_DEADLINE_MS)? != REPORT_BYTES {
                    return Err("Short LCD report; stream stopped".into());
                }
            }
            let mut response = [0u8; ACK_BYTES];
            if transport.read_ack(&mut response, REPORT_DEADLINE_MS)? != ACK_BYTES {
                return Err("Short LCD report; stream stopped".into());
            }
            validate_ack(&response)?;
            Ok(())
        })();
        if let Err(error) = result {
            self.state = StreamState::Stopped(error.clone());
            return Err(error);
        }
        self.frames_sent += 1;
        self.latest_frame = jpeg.to_vec();
        if blank {
            self.state = StreamState::BlankAcked
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn structural_frame(length: usize) -> Vec<u8> {
        let mut frame = vec![
            0xff, 0xd8, 0xff, 0xc0, 0, 17, 8, 1, 224, 1, 224, 3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11,
            0, 0xff, 0xda, 0, 2,
        ];
        frame.resize(length.max(27) - 2, 0x5a);
        frame.extend_from_slice(&[0xff, 0xd9]);
        frame
    }
    #[test]
    fn jpeg_structural_limits() {
        let frame = structural_frame(27);
        assert!(validate_jpeg(&frame).is_ok());
        assert!(validate_jpeg(&structural_frame(MAX_FRAME)).is_ok());
        assert!(validate_jpeg(&structural_frame(MAX_FRAME + 1)).is_err());
        for (offset, value) in [
            (3, 0xc2),
            (6, 12),
            (8, 223),
            (10, 223),
            (11, 1),
            (12, 0),
            (13, 0x22),
        ] {
            let mut bad = frame.clone();
            bad[offset] = value;
            assert!(validate_jpeg(&bad).is_err(), "offset {offset}");
        }
        assert!(validate_jpeg(&frame[..frame.len() - 1]).is_err());
        let mut duplicate = frame[..21].to_vec();
        duplicate.extend_from_slice(&frame[2..]);
        assert!(validate_jpeg(&duplicate).is_err());
        let mut scan = vec![0xff, 0xd8];
        scan.extend_from_slice(&frame[21..]);
        assert!(validate_jpeg(&scan).is_err());
    }
    #[test]
    fn framing_edges_and_cycle() {
        assert!(packet(&[], 0).is_err());
        assert!(packet(&vec![0; MAX_FRAME + 1], 0).is_err());
        assert!(packet(&[1], usize::MAX).is_err());
        let data: Vec<_> = (0..MAX_FRAME).map(|i| (i % 251) as u8).collect();
        let first = packet(&data, 0).unwrap();
        assert_eq!(&first[..5], &[0, 8, 255, 0, 128]);
        assert_eq!(&first[5..], &data[..PAYLOAD]);
        for (index, sequence) in [(1, 1), (21, 21), (22, 1), (254, 2)] {
            let p = packet(&data, index).unwrap();
            assert_eq!(&p[..5], &[0, 8, sequence, 0, 0]);
        }
        let last = packet(&[1, 2, 3], 0).unwrap();
        assert_eq!(&last[5..8], &[1, 2, 3]);
        assert!(last[8..].iter().all(|v| *v == 0));
    }
    #[derive(Default)]
    struct Fake {
        clock: u64,
        write_ms: u64,
        read_ms: u64,
        writes: usize,
        reads: usize,
        fail_write: Option<usize>,
        short_write: bool,
        short_ack: bool,
        ack_id: u8,
        fail_ack: bool,
    }
    impl ReportTransport for Fake {
        fn now_ms(&self) -> u64 {
            self.clock
        }
        fn write_report(&mut self, _: &[u8; REPORT_BYTES], timeout: u64) -> Result<usize, String> {
            assert_eq!(timeout, 250);
            self.writes += 1;
            if self.fail_write == Some(self.writes) {
                return Err("Injected write failure".into());
            }
            if self.write_ms > timeout {
                return Err("Injected write timeout".into());
            }
            self.clock += self.write_ms;
            Ok(if self.short_write {
                REPORT_BYTES - 1
            } else {
                REPORT_BYTES
            })
        }
        fn read_ack(
            &mut self,
            response: &mut [u8; ACK_BYTES],
            timeout: u64,
        ) -> Result<usize, String> {
            assert_eq!(timeout, 250);
            self.reads += 1;
            if self.fail_ack {
                return Err("Injected ACK failure".into());
            }
            if self.read_ms > timeout {
                return Err("Injected ACK timeout".into());
            }
            self.clock += self.read_ms;
            response.fill(0xa5);
            response[0] = self.ack_id;
            Ok(if self.short_ack {
                ACK_BYTES - 1
            } else {
                ACK_BYTES
            })
        }
    }
    #[test]
    fn commit_only_after_ack_and_blank() {
        let frame = structural_frame(2050);
        let mut sender = Sender::default();
        let mut fake = Fake::default();
        sender.send_frame(&frame, false, 0, &mut fake).unwrap();
        assert_eq!(fake.writes, 3);
        assert_eq!(fake.reads, 1);
        assert_eq!(sender.frames_sent(), 1);
        assert_eq!(sender.latest_frame(), frame);
        assert!(!sender.blank_acked());
        sender.send_frame(&frame, true, 0, &mut fake).unwrap();
        assert_eq!(sender.frames_sent(), 2);
        assert!(sender.blank_acked());
        assert!(sender.send_frame(&frame, false, 0, &mut fake).is_err());
        assert_eq!(fake.writes, 6);
    }
    #[test]
    fn failures_never_publish_partial_progress() {
        let frame = structural_frame(2050);
        for mode in 0..7 {
            let mut sender = Sender::default();
            let mut fake = Fake::default();
            sender.send_frame(&frame, false, 0, &mut fake).unwrap();
            match mode {
                0 => fake.fail_write = Some(fake.writes + 2),
                1 => fake.short_write = true,
                2 => fake.fail_ack = true,
                3 => fake.short_ack = true,
                4 => fake.ack_id = 1,
                5 => fake.write_ms = 251,
                _ => fake.read_ms = 251,
            }
            assert!(
                sender
                    .send_frame(&structural_frame(3090), true, 0, &mut fake)
                    .is_err()
            );
            assert_eq!(sender.frames_sent(), 1);
            assert_eq!(sender.latest_frame(), frame);
            assert!(!sender.blank_acked());
            let writes = fake.writes;
            assert!(sender.send_frame(&frame, false, 0, &mut fake).is_err());
            assert_eq!(fake.writes, writes);
        }
    }
    #[test]
    fn native_deadline_position() {
        let frame = structural_frame(13 * PAYLOAD);
        let mut fake = Fake {
            write_ms: 250,
            read_ms: 250,
            ..Fake::default()
        };
        let mut sender = Sender::default();
        sender.send_frame(&frame, false, 0, &mut fake).unwrap();
        assert_eq!(fake.clock, 3500);
        assert_eq!(sender.frames_sent(), 1);
        let frame = structural_frame(14 * PAYLOAD);
        let mut fake = Fake {
            write_ms: 250,
            ..Fake::default()
        };
        let mut sender = Sender::default();
        assert!(sender.send_frame(&frame, false, 0, &mut fake).is_err());
        assert_eq!(fake.writes, 13);
        assert_eq!(fake.reads, 0);
        assert_eq!(sender.frames_sent(), 0);
        let mut sender = Sender::default();
        let mut fake = Fake {
            clock: 3001,
            ..Fake::default()
        };
        assert!(
            sender
                .send_frame(&structural_frame(27), false, 0, &mut fake)
                .is_err()
        );
        assert_eq!(fake.writes, 0);
    }
    #[test]
    fn ack_contract_only_known_bytes() {
        assert!(validate_ack(&[0; 17]).is_ok());
        let mut ack = [255; 17];
        ack[0] = 0;
        assert!(validate_ack(&ack).is_ok());
        assert!(validate_ack(&ack[..16]).is_err());
        ack[0] = 1;
        assert!(validate_ack(&ack).is_err());
    }
}

#[cfg(test)]
mod native_tests {
    use super::*;
    struct Reader<'a> {
        data: &'a [u8],
        offset: usize,
    }
    impl<'a> Reader<'a> {
        fn take(&mut self, n: usize) -> &'a [u8] {
            let out = &self.data[self.offset..self.offset + n];
            self.offset += n;
            out
        }
        fn u8(&mut self) -> u8 {
            self.take(1)[0]
        }
        fn u16(&mut self) -> usize {
            u16::from_le_bytes(self.take(2).try_into().unwrap()) as usize
        }
        fn u32(&mut self) -> usize {
            u32::from_le_bytes(self.take(4).try_into().unwrap()) as usize
        }
    }
    #[test]
    fn actual_cpp_protocol_parity() {
        let mut r = Reader {
            data: include_bytes!("fixtures/protocol-native.bin"),
            offset: 0,
        };
        assert_eq!(r.take(4), b"LCP1");
        assert_eq!(r.u32(), 512);
        for case in 0..512 {
            let length = r.u32();
            let index = r.u32();
            let seed = r.u32() as u32;
            let expected_length = r.u16();
            let expected = r.take(expected_length);
            let frame: Vec<_> = (0..length)
                .map(|i| ((i as u32 * 31).wrapping_add(seed) & 255) as u8)
                .collect();
            let actual = packet(&frame, index);
            if expected_length == 0 {
                assert!(actual.is_err(), "packet {case}")
            } else {
                assert_eq!(actual.unwrap().as_slice(), expected, "packet {case}");
            }
        }
        assert_eq!(r.u32(), 169);
        for case in 0..169 {
            let length = r.u32();
            let jpeg = r.take(length);
            let ok = r.u8() != 0;
            let message_length = r.u16();
            let message = std::str::from_utf8(r.take(message_length)).unwrap();
            let actual = validate_jpeg(jpeg);
            assert_eq!(actual.is_ok(), ok, "JPEG {case}");
            if let Err(error) = actual {
                assert_eq!(error, message, "JPEG error {case}");
            }
        }
        assert_eq!(r.offset, r.data.len());
    }
    #[test]
    fn invalid_frame_and_clock_never_touch_transport() {
        struct NoIo;
        impl ReportTransport for NoIo {
            fn now_ms(&self) -> u64 {
                0
            }
            fn write_report(&mut self, _: &[u8; REPORT_BYTES], _: u64) -> Result<usize, String> {
                panic!("unexpected I/O")
            }
            fn read_ack(&mut self, _: &mut [u8; ACK_BYTES], _: u64) -> Result<usize, String> {
                panic!("unexpected I/O")
            }
        }
        let mut sender = Sender::default();
        assert!(sender.send_frame(&[], false, 0, &mut NoIo).is_err());
        assert_eq!(sender.frames_sent(), 0);
        let mut sender = Sender::default();
        let structural = [
            0xff, 0xd8, 0xff, 0xc0, 0, 17, 8, 1, 224, 1, 224, 3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11,
            0, 0xff, 0xda, 0, 2, 0xff, 0xd9,
        ];
        assert_eq!(
            sender.send_frame(&structural, false, 1, &mut NoIo),
            Err("Invalid monotonic LCD clock".into())
        );
    }
}
