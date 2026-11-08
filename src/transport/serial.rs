//! Skydimo connection/identity and bounded frame submission, injectable for a stand.
use super::skydimo::{self, Identity};
use crate::lighting::color::Rgb;

pub const BAUD: u64 = 115200;
pub const QUERY: &[u8; 6] = b"Moni-A";
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub port: String,
    pub instance_id: String,
}
#[derive(Clone, Debug)]
pub struct Binding {
    pub descriptor: Descriptor,
    pub expected: Identity,
    pub leds: usize,
}
pub fn port_path(port: &str) -> Result<String, String> {
    let suffix = port
        .strip_prefix("COM")
        .ok_or("Expected canonical COM port")?;
    if suffix.is_empty()
        || suffix.starts_with('0')
        || suffix.len() > 4
        || !suffix.bytes().all(|b| b.is_ascii_digit())
        || !suffix.parse::<u32>().is_ok_and(|n| (1..=4096).contains(&n))
    {
        return Err("Invalid COM port number".into());
    }
    Ok(format!(r"\\.\{port}"))
}
pub fn valid_usb_id(value: &str) -> bool {
    let prefix = r"USB\VID_1A86&PID_7523";
    value
        .get(..prefix.len())
        .is_some_and(|s| s.eq_ignore_ascii_case(prefix))
        && value
            .as_bytes()
            .get(prefix.len())
            .is_none_or(|b| *b == b'&' || *b == b'\\')
        && value.len() <= 1024
        && !value.chars().any(char::is_control)
}
impl Binding {
    pub fn validate(&self) -> Result<(), String> {
        port_path(&self.descriptor.port)?;
        if !valid_usb_id(&self.descriptor.instance_id)
            || self.descriptor.instance_id.split('\\').count() != 3
            || self.descriptor.instance_id.ends_with('\\')
        {
            return Err("Invalid CH340 USB instance binding".into());
        }
        let model =
            skydimo::model(&self.expected.model).ok_or("Unsupported Skydimo serial model")?;
        if self.leds == 0
            || (model.leds != 0 && self.leds != model.leds)
            || self.leds > model.max_leds
        {
            return Err("LED count does not match Skydimo model capabilities".into());
        }
        if self.expected.serial.is_empty()
            || self.expected.serial.len() > 114
            || !self.expected.serial.len().is_multiple_of(2)
            || !self
                .expected
                .serial
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_lowercase())
        {
            return Err("Invalid Skydimo serial identity".into());
        }
        Ok(())
    }
}
pub trait Link {
    fn now_us(&self) -> u64;
    fn flush_rx(&mut self) -> Result<(), String>;
    fn write(&mut self, bytes: &[u8], timeout_ms: u32) -> Result<usize, String>;
    fn read(&mut self, bytes: &mut [u8], timeout_ms: u32) -> Result<usize, String>;
}
pub trait Backend {
    type Link: Link;
    /// Metadata only; never send identification queries while enumerating ports.
    fn descriptors(&mut self) -> Result<Vec<Descriptor>, String>;
    fn open_exclusive(&mut self, descriptor: &Descriptor) -> Result<Self::Link, String>;
}
fn select(ports: &[Descriptor], target: &Descriptor) -> Result<(), String> {
    let matches: Vec<_> = ports.iter().filter(|d| d.port == target.port).collect();
    if matches.len() != 1 || matches[0] != target {
        return Err("Serial target missing, changed or ambiguous".into());
    }
    Ok(())
}
pub fn connect<B: Backend>(
    backend: &mut B,
    binding: &Binding,
) -> Result<Controller<B::Link>, String> {
    binding.validate()?;
    select(&backend.descriptors()?, &binding.descriptor)?;
    let mut link = backend.open_exclusive(&binding.descriptor)?;
    select(&backend.descriptors()?, &binding.descriptor)?;
    link.flush_rx()?;
    let start = link.now_us();
    if link.write(QUERY, 250)? != QUERY.len() {
        return Err("Short serial identification query".into());
    }
    let mut response = Vec::new();
    for _ in 0..64 {
        let elapsed = link
            .now_us()
            .checked_sub(start)
            .ok_or("Serial clock moved backwards")?;
        if elapsed >= 2_500_000 {
            return Err("Serial identification timed out".into());
        }
        let timeout = ((2_500_000 - elapsed).div_ceil(1000).min(250)) as u32;
        let mut chunk = [0u8; 64];
        let count = link.read(&mut chunk, timeout)?;
        if count > chunk.len() || response.len().saturating_add(count) > 63 {
            return Err("Serial identification exceeds63bytes".into());
        }
        response.extend_from_slice(&chunk[..count]);
        // Serial bytes after the comma are binary. CR/LF at a chunk boundary
        // can be identity data, so terminate on bounded read silence instead.
        if response.len() == 63 || (count == 0 && !response.is_empty()) {
            break;
        }
    }
    if link
        .now_us()
        .checked_sub(start)
        .is_none_or(|e| e >= 2_500_000)
    {
        return Err("Serial identification timed out".into());
    }
    if skydimo::parse_identity(&response)? != binding.expected {
        return Err("Skydimo model or serial identity changed".into());
    }
    Ok(Controller {
        link: Some(link),
        leds: binding.leds,
        failed: None,
        next_us: 0,
        last_us: start,
        submitted: 0,
    })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submission {
    Submitted,
    Throttled { retry_us: u64 },
}
pub struct Controller<L: Link> {
    link: Option<L>,
    leds: usize,
    failed: Option<String>,
    next_us: u64,
    last_us: u64,
    submitted: u64,
}
impl<L: Link> Controller<L> {
    pub fn now_us(&self) -> Result<u64, String> {
        Ok(self
            .link
            .as_ref()
            .ok_or("Serial output is stopped")?
            .now_us())
    }
    pub fn submitted(&self) -> u64 {
        self.submitted
    }
    pub fn failure(&self) -> Option<&str> {
        self.failed.as_deref()
    }
    /// Releasing ownership never sends an implicit black frame.
    pub fn stop(&mut self) {
        self.link = None;
    }
    pub fn send(&mut self, colors: &[Rgb]) -> Result<Submission, String> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        if colors.len() != self.leds {
            return Err("Skydimo frame LED count changed".into());
        }
        let packet = skydimo::packet(colors)?;
        let link = self.link.as_mut().ok_or("Serial output is stopped")?;
        let start = link.now_us();
        if start < self.last_us {
            self.failed = Some("Serial clock moved backwards".into());
            self.link = None;
            return Err(self.failed.clone().unwrap());
        }
        self.last_us = start;
        if start < self.next_us {
            return Ok(Submission::Throttled {
                retry_us: self.next_us,
            });
        }
        let result = (|| {
            let mut offset = 0;
            while offset < packet.len() {
                if link
                    .now_us()
                    .checked_sub(start)
                    .is_none_or(|e| e > 1_000_000)
                {
                    return Err("Serial frame exceeded1second".into());
                }
                let count = link.write(&packet[offset..], 250)?;
                if count == 0 || count > packet.len() - offset {
                    return Err("Invalid serial write count".into());
                }
                offset += count;
            }
            if link
                .now_us()
                .checked_sub(start)
                .is_none_or(|e| e > 1_000_000)
            {
                return Err("Serial frame exceeded1second".into());
            }
            Ok::<_, String>(())
        })();
        if let Err(error) = result {
            self.failed = Some(error.clone());
            self.link = None;
            return Err(error);
        }
        self.submitted = self.submitted.saturating_add(1);
        self.last_us = link.now_us();
        // 8N1: ten wire bits/byte. Completion means submitted, not physical LED ACK.
        self.next_us = link
            .now_us()
            .saturating_add((packet.len() as u64 * 10 * 1_000_000).div_ceil(BAUD));
        Ok(Submission::Submitted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };
    #[derive(Default)]
    struct State {
        log: Vec<&'static str>,
        now: u64,
        writes: Vec<Vec<u8>>,
        zero: bool,
        limit: Option<usize>,
    }
    struct FakeLink {
        state: Arc<Mutex<State>>,
        responses: VecDeque<Vec<u8>>,
    }
    impl Drop for FakeLink {
        fn drop(&mut self) {
            self.state.lock().unwrap().log.push("close");
        }
    }
    impl Link for FakeLink {
        fn now_us(&self) -> u64 {
            self.state.lock().unwrap().now
        }
        fn flush_rx(&mut self) -> Result<(), String> {
            self.state.lock().unwrap().log.push("flush");
            Ok(())
        }
        fn write(&mut self, b: &[u8], _: u32) -> Result<usize, String> {
            let mut s = self.state.lock().unwrap();
            s.log.push("write");
            if s.zero {
                return Ok(0);
            };
            let count = b.len().min(s.limit.unwrap_or(b.len()));
            s.writes.push(b[..count].to_vec());
            Ok(count)
        }
        fn read(&mut self, b: &mut [u8], _: u32) -> Result<usize, String> {
            let r = self.responses.pop_front().unwrap_or_default();
            b[..r.len()].copy_from_slice(&r);
            Ok(r.len())
        }
    }
    struct Fake {
        state: Arc<Mutex<State>>,
        target: Descriptor,
        changed: bool,
        reads: usize,
        response: VecDeque<Vec<u8>>,
    }
    impl Backend for Fake {
        type Link = FakeLink;
        fn descriptors(&mut self) -> Result<Vec<Descriptor>, String> {
            self.reads += 1;
            let mut d = self.target.clone();
            if self.changed && self.reads > 1 {
                d.instance_id.push('x');
            }
            Ok(vec![d])
        }
        fn open_exclusive(&mut self, _: &Descriptor) -> Result<FakeLink, String> {
            self.state.lock().unwrap().log.push("open-exclusive");
            Ok(FakeLink {
                state: self.state.clone(),
                responses: self.response.clone(),
            })
        }
    }
    fn setup() -> (Fake, Binding) {
        let descriptor = Descriptor {
            port: "COM7".into(),
            instance_id: r"USB\VID_1A86&PID_7523\STAND".into(),
        };
        let b = Binding {
            descriptor: descriptor.clone(),
            expected: Identity {
                model: "SK0132".into(),
                serial: "000000".into(),
            },
            leds: 77,
        };
        (
            Fake {
                state: Arc::default(),
                target: descriptor,
                changed: false,
                reads: 0,
                response: VecDeque::from([b"SK0".to_vec(), b"132\r\n".to_vec()]),
            },
            b,
        )
    }
    #[test]
    fn identity_revalidation_no_write_on_change() {
        let (mut f, b) = setup();
        f.changed = true;
        assert!(connect(&mut f, &b).is_err());
        let s = f.state.lock().unwrap();
        assert!(s.writes.is_empty());
        assert_eq!(s.log, ["open-exclusive", "close"]);
    }
    #[test]
    fn exact_query_frame_throttle_and_release() {
        let (mut f, b) = setup();
        let mut c = connect(&mut f, &b).unwrap();
        assert_eq!(
            c.send(&vec![Rgb::default(); 77]).unwrap(),
            Submission::Submitted
        );
        assert_eq!(c.submitted(), 1);
        assert_eq!(
            c.send(&vec![Rgb::default(); 77]).unwrap(),
            Submission::Throttled { retry_us: 20573 }
        );
        f.state.lock().unwrap().now = 20833;
        assert_eq!(
            c.send(&vec![Rgb::default(); 77]).unwrap(),
            Submission::Submitted
        );
        c.stop();
        let s = f.state.lock().unwrap();
        assert_eq!(s.writes[0], QUERY);
        assert_eq!(s.writes.len(), 3);
        assert_eq!(s.log.last(), Some(&"close"));
    }
    #[test]
    fn failures_are_terminal_and_identity_mismatch_gets_no_frame() {
        let (mut f, b) = setup();
        f.response = VecDeque::from([b"SK0127\r\n".to_vec()]);
        assert!(connect(&mut f, &b).is_err());
        assert_eq!(f.state.lock().unwrap().writes.len(), 1);
        let (mut f, b) = setup();
        let mut c = connect(&mut f, &b).unwrap();
        f.state.lock().unwrap().zero = true;
        assert!(c.send(&vec![Rgb::default(); 77]).is_err());
        assert_eq!(c.submitted(), 0);
        assert!(c.failure().is_some());
    }
    #[test]
    fn explicit_port_and_binding_bounds() {
        for port in ["COM0", "COM01", "COM4097", "com1", r"\\.\COM1", "COM1/evil"] {
            assert!(port_path(port).is_err());
        }
        assert_eq!(port_path("COM256").unwrap(), r"\\.\COM256");
        assert!(!valid_usb_id(r"USB\VID_1A86&PID_75230\x"));
        let (_, mut b) = setup();
        b.leds = 76;
        assert!(b.validate().is_err());
    }
    #[test]
    fn partial_writes_preserve_packet_order_and_clock_regression_stops() {
        let (mut f, b) = setup();
        let mut c = connect(&mut f, &b).unwrap();
        f.state.lock().unwrap().limit = Some(13);
        let colors = vec![Rgb::new(1, 2, 3); 77];
        c.send(&colors).unwrap();
        let s = f.state.lock().unwrap();
        let bytes: Vec<u8> = s.writes.iter().skip(1).flatten().copied().collect();
        assert_eq!(bytes, skydimo::packet(&colors).unwrap());
        drop(s);
        f.state.lock().unwrap().now = 30000;
        c.send(&colors).unwrap();
        f.state.lock().unwrap().now = 29999;
        assert!(c.send(&colors).is_err());
        assert!(c.failure().is_some());
    }
    #[test]
    fn binary_serial_cr_at_chunk_boundary_waits_for_silence() {
        let (mut f, mut b) = setup();
        b.expected.serial = "010D02".into();
        f.response = VecDeque::from([b"SK0132,\x01\r".to_vec(), b"\x02\r\n".to_vec(), Vec::new()]);
        assert!(connect(&mut f, &b).is_ok());
        let (mut f, b) = setup();
        f.response = b"SK0132\r\n".iter().map(|b| vec![*b]).collect();
        assert!(connect(&mut f, &b).is_ok());
    }
}
