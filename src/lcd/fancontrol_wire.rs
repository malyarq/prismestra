// SPDX-License-Identifier: GPL-2.0-or-later
//! Bounded cached-sensor protobuf wire only. No pipe, process, clock or device access.
//! The reply carries no acquisition timestamp; caller owns observation time and freshness.
use super::temperatures::{Reading, SensorRecord, filter_fan};
pub const METHOD: &str = "/SensorsRPC/GetAllSensors";
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
pub const MAX_TOTAL_BYTES: usize = 2 * MAX_MESSAGE_BYTES;
pub const MAX_PACKETS: usize = 64;
pub const MAX_ENTRIES: usize = 4096;
pub const IPC_TIMEOUT_MS: u64 = 2500;
pub type WireResult<T> = Result<T, String>;
fn error(text: &str) -> String {
    text.to_owned()
}
struct Proto<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Proto<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    fn empty(&self) -> bool {
        self.position == self.bytes.len()
    }
    fn varint(&mut self) -> WireResult<u64> {
        let mut value = 0;
        for i in 0..10 {
            let byte = *self
                .bytes
                .get(self.position)
                .ok_or_else(|| error("Truncated protobuf varint"))?;
            self.position += 1;
            if i == 9 && byte > 1 {
                return Err(error("Overflowing protobuf varint"));
            }
            value |= u64::from(byte & 0x7f) << (7 * i);
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(error("Malformed protobuf varint"))
    }
    fn take(&mut self, length: u64) -> WireResult<&'a [u8]> {
        if length > MAX_MESSAGE_BYTES as u64 || length > (self.bytes.len() - self.position) as u64 {
            return Err(error("Truncated/oversized protobuf field"));
        }
        let start = self.position;
        self.position += length as usize;
        Ok(&self.bytes[start..self.position])
    }
    fn blob(&mut self) -> WireResult<&'a [u8]> {
        let length = self.varint()?;
        self.take(length)
    }
    fn tag(&mut self) -> WireResult<u32> {
        let value = self.varint()?;
        if value > u32::MAX as u64 || value >> 3 == 0 {
            return Err(error("Invalid protobuf field tag"));
        }
        Ok(value as u32)
    }
    fn skip(&mut self, wire: u32) -> WireResult<()> {
        match wire {
            0 => {
                self.varint()?;
            }
            1 => {
                self.take(8)?;
            }
            2 => {
                self.blob()?;
            }
            5 => {
                self.take(4)?;
            }
            _ => return Err(error("Unsupported protobuf wire type")),
        }
        Ok(())
    }
}
fn wire(tag: u32, expected: u32) -> WireResult<()> {
    if tag & 7 != expected {
        Err(error("Unexpected protobuf wire type"))
    } else {
        Ok(())
    }
}
fn once(seen: &mut u32, field: u32) -> WireResult<()> {
    let bit = 1 << field;
    if *seen & bit != 0 {
        return Err(error("Ambiguous repeated protobuf scalar"));
    }
    *seen |= bit;
    Ok(())
}
fn text(bytes: &[u8]) -> WireResult<String> {
    if bytes.len() > 4096 {
        return Err(error("Oversized sensor text"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| error("Invalid UTF-8 in sensor metadata"))?;
    Ok(text.strip_prefix('﻿').unwrap_or(text).to_owned())
}
/// Parse all records, including omitted proto3 defaults. Type3 alone means temperature;
/// absent value stays value_present=false, while explicit IEEE754 zero remains present.
/// Unknown u64 enum values outside i32 are represented by -1, never accidentally type3.
pub fn parse_records(bytes: &[u8]) -> WireResult<Vec<SensorRecord>> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(error("Oversized FanControl reply"));
    }
    let mut reply = Proto::new(bytes);
    let mut records = Vec::new();
    while !reply.empty() {
        let tag = reply.tag()?;
        if tag >> 3 != 1 {
            reply.skip(tag & 7)?;
            continue;
        }
        wire(tag, 2)?;
        if records.len() >= MAX_ENTRIES {
            return Err(error("Too many FanControl sensors"));
        }
        let mut sensor = Proto::new(reply.blob()?);
        let mut seen = 0;
        let mut kind = 0u64;
        let mut reading = Reading {
            id: String::new(),
            name: String::new(),
            origin: String::new(),
            celsius: 0.0,
        };
        let mut value_present = false;
        while !sensor.empty() {
            let tag = sensor.tag()?;
            let field = tag >> 3;
            if (1..=5).contains(&field) {
                once(&mut seen, field)?
            }
            match field {
                1 => {
                    wire(tag, 2)?;
                    reading.id = text(sensor.blob()?)?
                }
                2 => {
                    wire(tag, 0)?;
                    kind = sensor.varint()?
                }
                3 => {
                    wire(tag, 2)?;
                    reading.name = text(sensor.blob()?)?
                }
                4 => {
                    wire(tag, 2)?;
                    reading.origin = text(sensor.blob()?)?
                }
                5 => {
                    wire(tag, 5)?;
                    let raw = sensor.take(4)?;
                    reading.celsius = f32::from_le_bytes(
                        raw.try_into().map_err(|_| error("Invalid float32 value"))?,
                    ) as f64;
                    value_present = true;
                }
                _ => sensor.skip(tag & 7)?,
            }
        }
        records.push(SensorRecord {
            reading,
            kind: i32::try_from(kind).unwrap_or(-1),
            value_present,
        });
    }
    Ok(records)
}
/// Filtering reuses the LCD source cache's kind/ID/DIMM/plausible-temperature rules.
/// No partial readings escape if a later record is malformed or ambiguously duplicated.
pub fn parse_payload(bytes: &[u8]) -> WireResult<Vec<Reading>> {
    filter_fan(parse_records(bytes)?)
}
#[derive(Default)]
struct PacketBudget {
    index: usize,
    total: usize,
}
impl PacketBudget {
    fn read(&mut self, read: &mut impl FnMut() -> WireResult<Vec<u8>>) -> WireResult<Vec<u8>> {
        if self.index >= MAX_PACKETS {
            return Err(error("Too many FanControl transport packets"));
        }
        let packet = read()?;
        self.index += 1;
        self.total = self
            .total
            .checked_add(packet.len())
            .ok_or_else(|| error("Oversized FanControl pipe reply"))?;
        if packet.len() > MAX_MESSAGE_BYTES || self.total > MAX_TOTAL_BYTES {
            return Err(error("Oversized FanControl pipe reply"));
        }
        Ok(packet)
    }
}
/// Fake complete message-mode packets, not arbitrary byte chunks. Unread entries
/// after successful terminal trailers are not consumed, matching the native callback.
pub fn parse_packets(packets: &[Vec<u8>]) -> WireResult<Vec<Reading>> {
    let mut index = 0;
    parse_reply(|| {
        let packet = packets
            .get(index)
            .ok_or_else(|| error("FanControl reply ended before trailers"))?;
        index += 1;
        if packet.len() > MAX_MESSAGE_BYTES {
            return Err(error("Oversized FanControl pipe reply"));
        }
        Ok(packet.clone())
    })
}
/// Pull complete message-mode packets only until successful terminal trailers.
/// A separate raw payload is one extra callback read; no read-to-EOF is performed.
/// Callback owns actual IO/deadline handling. This wrapper enforces consumed-byte
/// budgets and does not open a pipe, execute a provider, or add a host clock.
pub fn parse_reply(
    mut read_packet: impl FnMut() -> WireResult<Vec<u8>>,
) -> WireResult<Vec<Reading>> {
    let mut budget = PacketBudget::default();
    let mut payload = None;
    let mut headers_seen = false;
    loop {
        let packet = budget.read(&mut read_packet)?;
        let mut frames = Proto::new(&packet);
        if frames.empty() {
            return Err(error("Empty FanControl transport packet"));
        }
        while !frames.empty() {
            let mut transport = Proto::new(frames.blob()?);
            let mut kind = 0;
            let mut body = &[][..];
            while !transport.empty() {
                let tag = transport.tag()?;
                let field = tag >> 3;
                if (1..=5).contains(&field) {
                    if kind != 0 {
                        return Err(error("Ambiguous transport oneof"));
                    }
                    if field == 1 || field == 4 {
                        return Err(error("Unsupported server transport control"));
                    }
                    wire(tag, 2)?;
                    kind = field;
                    body = transport.blob()?;
                } else {
                    transport.skip(tag & 7)?
                }
            }
            match kind {
                2 => {
                    if headers_seen || payload.is_some() {
                        return Err(error("Unexpected FanControl headers"));
                    }
                    headers_seen = true;
                    let mut headers = Proto::new(body);
                    while !headers.empty() {
                        let tag = headers.tag()?;
                        headers.skip(tag & 7)?
                    }
                }
                3 => {
                    if payload.is_some() {
                        return Err(error("Multiple payloads in unary FanControl reply"));
                    }
                    let mut info = Proto::new(body);
                    let (mut seen, mut size, mut same_packet) = (0, 0, 0);
                    while !info.empty() {
                        let tag = info.tag()?;
                        let field = tag >> 3;
                        if field == 1 || field == 2 {
                            once(&mut seen, field)?;
                            wire(tag, 0)?;
                            if field == 1 {
                                size = info.varint()?
                            } else {
                                same_packet = info.varint()?
                            }
                        } else {
                            info.skip(tag & 7)?
                        }
                    }
                    if size > MAX_MESSAGE_BYTES as u64 || same_packet > 1 {
                        return Err(error("Unsupported/oversized FanControl payload"));
                    }
                    payload = Some(if same_packet != 0 {
                        frames.take(size)?.to_vec()
                    } else if size != 0 {
                        let raw = budget.read(&mut read_packet)?;
                        if raw.len() as u64 != size {
                            return Err(error("Separate payload size mismatch"));
                        }
                        raw
                    } else {
                        Vec::new()
                    });
                }
                5 => {
                    let mut trailers = Proto::new(body);
                    let (mut seen, mut status) = (0, 0u64);
                    let mut detail = String::new();
                    while !trailers.empty() {
                        let tag = trailers.tag()?;
                        match tag >> 3 {
                            2 => {
                                once(&mut seen, 2)?;
                                wire(tag, 0)?;
                                status = trailers.varint()?
                            }
                            3 => {
                                once(&mut seen, 3)?;
                                wire(tag, 2)?;
                                detail = text(trailers.blob()?)?
                            }
                            _ => trailers.skip(tag & 7)?,
                        }
                    }
                    if status != 0 {
                        return Err(format!("FanControl RPC status {status}: {detail}"));
                    }
                    if !frames.empty() || payload.is_none() {
                        return Err(error("Incomplete/ambiguous FanControl unary reply"));
                    }
                    return parse_payload(
                        payload
                            .as_deref()
                            .ok_or_else(|| error("Missing FanControl payload"))?,
                    );
                }
                _ => return Err(error("Unsupported FanControl transport message")),
            }
        }
    }
}
fn varint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut byte = (value & 127) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 128
        }
        out.push(byte);
        if value == 0 {
            return out;
        }
    }
}
fn blob(field: u32, bytes: &[u8]) -> Vec<u8> {
    let mut out = varint(u64::from(field << 3 | 2));
    out.extend(varint(bytes.len() as u64));
    out.extend_from_slice(bytes);
    out
}
fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = varint(body.len() as u64);
    out.extend_from_slice(body);
    out
}
/// The only reachable method is the fixed read-only GetAllSensors RPC. Deadline is
/// caller-supplied UTC milliseconds (source uses now+remaining from a 2500ms budget).
pub fn request(deadline_utc_ms: i64) -> WireResult<Vec<u8>> {
    if deadline_utc_ms <= 0 {
        return Err(error("Invalid FanControl deadline"));
    }
    let deadline = deadline_utc_ms as u64;
    let mut timestamp = vec![8];
    timestamp.extend(varint(deadline / 1000));
    timestamp.push(16);
    timestamp.extend(varint(deadline % 1000 * 1_000_000));
    let mut init = blob(1, METHOD.as_bytes());
    init.extend(blob(2, &timestamp));
    let mut out = frame(&blob(1, &init));
    out.extend(frame(&blob(2, &[])));
    out.extend(frame(&blob(3, &[16, 1])));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sensor(id: &[u8], kind: Option<u64>, value: Option<f32>) -> Vec<u8> {
        let mut body = blob(1, id);
        if let Some(kind) = kind {
            body.push(16);
            body.extend(varint(kind));
        }
        body.extend(blob(3, b"Synthetic temperature"));
        body.extend(blob(4, b"Synthetic origin"));
        if let Some(value) = value {
            body.push(45);
            body.extend(value.to_le_bytes());
        }
        blob(1, &body)
    }
    fn transport(kind: u32, body: &[u8]) -> Vec<u8> {
        frame(&blob(kind, body))
    }
    fn payload_packet(payload: &[u8], same: bool) -> Vec<u8> {
        let mut info = vec![8];
        info.extend(varint(payload.len() as u64));
        info.push(16);
        info.push(u8::from(same));
        let mut out = transport(3, &info);
        if same {
            out.extend_from_slice(payload)
        }
        out
    }
    fn valid() -> Vec<u8> {
        sensor(b"/synthetic/temp/0", Some(3), Some(63.5))
    }
    #[test]
    fn defaults_type_and_optional_float() {
        let empty = blob(1, &[]);
        let record = parse_records(&empty).unwrap().remove(0);
        assert_eq!(record.kind, 0);
        assert!(!record.value_present);
        assert_eq!(record.reading.celsius, 0.0);
        assert!(parse_payload(&empty).unwrap().is_empty());
        let mut payload = sensor(b"null", Some(3), None);
        payload.extend(sensor(b"zero", Some(3), Some(0.0)));
        payload.extend(sensor(b"omitted-kind", None, Some(80.0)));
        payload.extend(sensor(b"fan", Some(4), Some(80.0)));
        payload.extend(sensor(b"large-kind", Some(0x100000003), Some(80.0)));
        let readings = parse_payload(&payload).unwrap();
        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].id, "zero");
        assert_eq!(readings[0].celsius, 0.0);
    }
    #[test]
    fn values_ids_and_dimm() {
        let mut payload = Vec::new();
        for (id, value) in [
            ("nan", f32::NAN),
            ("inf", f32::INFINITY),
            ("high", 151.0),
            ("low", -21.0),
            ("minimum", -20.0),
            ("maximum", 150.0),
            ("/memory/dimm/1/temperature/0", 46.25),
            ("/memory/dimm/3/temperature/1", 0.25),
            ("/memory/dimm/x/temperature/0", 42.0),
        ] {
            payload.extend(sensor(id.as_bytes(), Some(3), Some(value)));
        }
        let r = parse_payload(&payload).unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[2].celsius, 46.25);
        let mut dup = valid();
        dup.extend(valid());
        assert!(parse_payload(&dup).is_err());
        let mut dup = sensor(b"same", Some(3), None);
        dup.extend(sensor(b"same", Some(3), Some(42.0)));
        assert!(parse_payload(&dup).is_err());
        assert!(parse_payload(&sensor(b"", Some(3), Some(40.0))).is_err());
    }
    #[test]
    fn strict_fields_utf8_and_unknowns() {
        assert!(parse_payload(&sensor(&[255], Some(4), None)).is_err());
        assert!(parse_payload(&sensor(&[0xc3], Some(3), Some(1.0))).is_err());
        assert!(parse_payload(&sensor(&vec![b'a'; 4097], Some(3), None)).is_err());
        let mut body = blob(1, b"one");
        body.extend(blob(1, b"two"));
        assert!(parse_records(&blob(1, &body)).is_err());
        let mut bytes = valid();
        bytes.extend(blob(99, b"future"));
        bytes.extend([0xa0, 6, 1]);
        bytes.extend([0xa9, 6, 0, 0, 0, 0, 0, 0, 0, 0]);
        bytes.extend([0xb5, 6, 0, 0, 0, 0]);
        assert_eq!(parse_payload(&bytes).unwrap().len(), 1);
        for bad in [
            vec![0],
            vec![0x0b],
            vec![0x9b, 6],
            vec![
                0x0a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 2,
            ],
            vec![0x0a, 0x80],
            vec![0x0a, 9, 1],
        ] {
            assert!(parse_payload(&bad).is_err());
        }
        let mut wrong = vec![16, 3];
        wrong.extend([42, 4, 0, 0, 0, 0]);
        assert!(parse_records(&blob(1, &wrong)).is_err());
    }
    #[test]
    fn message_record_and_packet_bounds() {
        assert!(parse_payload(&vec![0; MAX_MESSAGE_BYTES + 1]).is_err());
        let mut entries = Vec::new();
        for _ in 0..4096 {
            entries.extend([10, 0]);
        }
        assert_eq!(parse_records(&entries).unwrap().len(), 4096);
        entries.extend([10, 0]);
        assert!(parse_records(&entries).is_err());
        let mut read = || Ok(vec![0]);
        let mut reader = PacketBudget::default();
        for _ in 0..64 {
            reader.read(&mut read).unwrap();
        }
        assert!(reader.read(&mut read).is_err());
        let mut read = || Ok(vec![0; MAX_MESSAGE_BYTES]);
        let mut reader = PacketBudget::default();
        reader.read(&mut read).unwrap();
        reader.read(&mut read).unwrap();
        assert_eq!(reader.total, MAX_TOTAL_BYTES);
        assert!(reader.read(&mut read).is_err());
    }
    #[test]
    fn grouping_separate_raw_and_proto3_empty() {
        let payload = valid();
        let headers = transport(2, &[]);
        let trailers = transport(5, &[]);
        let mut all = headers.clone();
        all.extend(payload_packet(&payload, true));
        all.extend(&trailers);
        assert_eq!(parse_packets(&[all]).unwrap()[0].celsius, 63.5);
        assert_eq!(
            parse_packets(&[
                headers.clone(),
                payload_packet(&payload, true),
                trailers.clone()
            ])
            .unwrap()
            .len(),
            1
        );
        let mut outer = headers.clone();
        outer.extend(payload_packet(&payload, false));
        outer.extend(&trailers);
        assert_eq!(parse_packets(&[outer, payload.clone()]).unwrap().len(), 1);
        let mut empty = transport(3, &[]);
        empty.extend(&trailers);
        assert!(parse_packets(&[empty]).unwrap().is_empty());
        assert!(
            parse_packets(&[
                headers,
                payload_packet(&payload, false),
                vec![0],
                trailers.clone()
            ])
            .is_err()
        );
        assert!(parse_packets(&[payload_packet(&payload, true)]).is_err());
        assert!(parse_packets(&[trailers]).is_err());
    }
    #[test]
    fn transport_ambiguity_status_and_unknowns() {
        let payload = valid();
        let trailers = transport(5, &[]);
        let mut all = payload_packet(&payload, true);
        all.extend(payload_packet(&payload, true));
        all.extend(&trailers);
        assert!(parse_packets(&[all]).is_err());
        let mut all = payload_packet(&payload, true);
        all.extend(transport(5, &[16, 7]));
        assert!(parse_packets(&[all]).unwrap_err().contains("status 7"));
        let mut all = payload_packet(&payload, true);
        all.extend(transport(5, &[16, 0, 16, 0]));
        assert!(parse_packets(&[all]).is_err());
        let mut body = blob(2, &[]);
        body.extend(blob(3, &[]));
        assert!(parse_packets(&[frame(&body)]).is_err());
        for kind in [1, 4, 99] {
            assert!(parse_packets(&[transport(kind, &[])]).is_err());
        }
        let mut all = payload_packet(&payload, true);
        all.extend(&trailers);
        all.extend(transport(2, &[]));
        assert!(parse_packets(&[all]).is_err());
        let mut complete = payload_packet(&payload, true);
        complete.extend(&trailers);
        assert_eq!(parse_packets(&[complete, vec![255]]).unwrap().len(), 1);
    }
    #[test]
    fn request_allowlist_deadline() {
        let request = request(1700000000123).unwrap();
        assert!(
            request
                .windows(METHOD.len())
                .any(|w| w == METHOD.as_bytes())
        );
        assert!(request.ends_with(&[2, 18, 0, 4, 26, 2, 16, 1]));
        assert!(
            request
                .windows(11)
                .any(|w| w == [8, 0x80, 0xe2, 0xcf, 0xaa, 6, 16, 0xc0, 0xa9, 0xd3, 0x3a])
        );
        assert!(super::request(0).is_err());
        assert!(super::request(-1).is_err());
        assert!(super::request(i64::MAX).is_ok());
    }
}

#[test]
fn callback_stops_at_terminal_trailers() {
    let payload = [10, 0];
    let info = vec![8, 2, 16, 1];
    let mut packet = frame(&blob(3, &info));
    packet.extend(payload);
    packet.extend(frame(&blob(5, &[])));
    let mut calls = 0;
    let readings = parse_reply(|| {
        calls += 1;
        assert_eq!(calls, 1, "must not read to EOF");
        Ok(packet.clone())
    })
    .unwrap();
    assert!(readings.is_empty());
    assert_eq!(calls, 1);
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
            let slice = &self.data[self.offset..self.offset + n];
            self.offset += n;
            slice
        }
        fn u8(&mut self) -> u8 {
            self.take(1)[0]
        }
        fn u32(&mut self) -> usize {
            u32::from_le_bytes(self.take(4).try_into().unwrap()) as usize
        }
        fn blob(&mut self) -> &'a [u8] {
            let n = self.u32();
            self.take(n)
        }
        fn string(&mut self) -> String {
            std::str::from_utf8(self.blob()).unwrap().to_owned()
        }
    }
    #[test]
    fn actual_cpp_differential() {
        let mut r = Reader {
            data: include_bytes!("fixtures/fancontrol-native.bin"),
            offset: 0,
        };
        assert_eq!(r.take(4), b"FWN1");
        assert_eq!(r.u32(), 60);
        let mut matched = 0;
        for case in 0..60 {
            let op = r.u8();
            if op == 0 {
                let deadline = i64::from_le_bytes(r.take(8).try_into().unwrap());
                let native_ok = r.u8() != 0;
                let expected = r.blob();
                let actual = request(deadline);
                assert_eq!(actual.is_ok(), native_ok);
                match actual {
                    Ok(bytes) => assert_eq!(bytes, expected),
                    Err(error) => assert_eq!(error.as_bytes(), expected),
                }
                matched += 1;
                continue;
            }
            let actual = if op == 1 {
                parse_payload(r.blob())
            } else {
                let n = r.u32();
                let packets: Vec<_> = (0..n).map(|_| r.blob().to_vec()).collect();
                parse_packets(&packets)
            };
            let native_ok = r.u8() != 0;
            let _native_error = r.string();
            let count = r.u32();
            let expected: Vec<_> = (0..count)
                .map(|_| Reading {
                    id: r.string(),
                    name: r.string(),
                    origin: r.string(),
                    celsius: f64::from_le_bytes(r.take(8).try_into().unwrap()),
                })
                .collect();
            if case == 59 {
                assert!(native_ok, "Qt buffers incomplete UTF8 detail without error");
                assert_eq!(actual.unwrap_err(), "Invalid UTF-8 in sensor metadata");
                continue;
            }
            assert_eq!(actual.is_ok(), native_ok, "native case {case}: {actual:?}");
            if let Ok(actual) = actual {
                assert_eq!(actual, expected, "native readings {case}");
            }
            matched += 1;
        }
        assert_eq!(matched, 59);
        assert_eq!(r.offset, r.data.len());
    }
}
