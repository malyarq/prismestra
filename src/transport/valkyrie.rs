// SPDX-License-Identifier: GPL-2.0-or-later
// Port of OpenRGB 1.1 ValkyrieKeyboardController (Nollie/Nuonuo, Bartholomew Ho, 2023/2024).
use crate::lighting::color::Rgb;
use std::time::Duration;

pub const REPORT_BYTES: usize = 65;
pub const MATRIX_WIDTH: usize = 22;
pub const MATRIX_HEIGHT: usize = 6;
pub const FEATURE_WRITES_PER_FRAME: usize = 10;
pub const FEATURE_READS_PER_FRAME: usize = 2;

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Action {
        Send([u8; 65]),
        Get([u8; 65]),
        Sleep(u64),
    }
    #[derive(Default)]
    struct Shared {
        actions: Vec<Action>,
        calls: usize,
        fail_at: Option<usize>,
        partial: bool,
        bad_id: bool,
    }
    struct Fake(Rc<RefCell<Shared>>);
    impl Transport for Fake {
        fn send_feature(&mut self, p: &[u8; 65]) -> Result<usize, String> {
            let mut s = self.0.borrow_mut();
            s.actions.push(Action::Send(*p));
            s.calls += 1;
            if s.fail_at == Some(s.calls) {
                if s.partial {
                    Ok(64)
                } else {
                    Err("injected failure".into())
                }
            } else {
                Ok(65)
            }
        }
        fn get_feature(&mut self, p: &mut [u8; 65]) -> Result<usize, String> {
            let mut s = self.0.borrow_mut();
            s.actions.push(Action::Get(*p));
            s.calls += 1;
            if s.fail_at == Some(s.calls) {
                if s.partial {
                    Ok(64)
                } else {
                    Err("injected failure".into())
                }
            } else {
                p.fill(0xa5);
                p[0] = if s.bad_id { 1 } else { 0 };
                Ok(65)
            }
        }
        fn sleep(&mut self, d: Duration) {
            self.0
                .borrow_mut()
                .actions
                .push(Action::Sleep(d.as_millis() as u64));
        }
    }
    fn descriptor(model: Model) -> Descriptor {
        Descriptor {
            path: format!(
                r"\\?\hid#vid_05ac&pid_024f&mi_{:02}&col01#SYNTHETIC#{{4d1e55b2-f16f-11cf-88cb-001111000030}}",
                model.interface()
            ),
            vid: 0x05ac,
            pid: 0x024f,
            interface: model.interface(),
            usage_page: 0xff13,
            usage: 1,
            feature_len: 65,
            feature_ids: vec![0],
            feature_caps_complete: true,
        }
    }
    struct Cursor<'a> {
        data: &'a [u8],
        pos: usize,
    }
    impl<'a> Cursor<'a> {
        fn take(&mut self, n: usize) -> &'a [u8] {
            let p = self.pos;
            self.pos += n;
            &self.data[p..self.pos]
        }
        fn byte(&mut self) -> u8 {
            self.take(1)[0]
        }
        fn u16(&mut self) -> u16 {
            u16::from_le_bytes(self.take(2).try_into().unwrap())
        }
        fn u32(&mut self) -> u32 {
            u32::from_le_bytes(self.take(4).try_into().unwrap())
        }
    }
    fn model(id: u8) -> Model {
        match id {
            0 => Model::Normal,
            1 => Model::Pro,
            _ => panic!("invalid fixture model"),
        }
    }
    #[test]
    fn actual_unchanged_cpp_oracle_transactions_and_matrices() {
        let data = include_bytes!("fixtures/valkyrie-native.bin");
        let mut c = Cursor { data, pos: 0 };
        assert_eq!(c.take(8), b"VK990001");
        for _ in 0..2 {
            let m = model(c.byte());
            assert_eq!(c.u16() as usize, m.leds());
            assert_eq!((c.u16(), c.u16()), (22, 6));
            for &p in m.matrix() {
                assert_eq!(c.u16(), p);
            }
        }
        let cases = c.u32();
        assert_eq!(cases, 64);
        for _ in 0..cases {
            let m = model(c.byte());
            let n = c.u16() as usize;
            assert_eq!(n, m.leds());
            let colors: Vec<_> = (0..n)
                .map(|_| {
                    let b = c.take(3);
                    Rgb::new(b[0], b[1], b[2])
                })
                .collect();
            let count = c.byte();
            assert_eq!(count, 12);
            let expected: Vec<_> = (0..count)
                .map(|_| {
                    let tag = c.byte();
                    let packet = c.take(65).try_into().unwrap();
                    match tag {
                        b'S' => Action::Send(packet),
                        b'G' => Action::Get(packet),
                        _ => panic!("unexpected action"),
                    }
                })
                .collect();
            let shared = Rc::new(RefCell::new(Shared::default()));
            let mut controller = Controller::new(m, &descriptor(m), Fake(shared.clone())).unwrap();
            assert_eq!(
                controller.frame(&colors).unwrap(),
                Counts {
                    write_attempts: 10,
                    read_attempts: 2,
                    writes: 10,
                    reads: 2,
                    bytes_written: 650,
                    bytes_read: 130
                }
            );
            let s = shared.borrow();
            assert_eq!(
                s.actions
                    .iter()
                    .filter(|a| !matches!(a, Action::Sleep(_)))
                    .cloned()
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(
                s.actions
                    .iter()
                    .filter_map(|a| if let Action::Sleep(n) = a {
                        Some(*n)
                    } else {
                        None
                    })
                    .collect::<Vec<_>>(),
                [vec![1; 12], vec![33]].concat()
            );
            assert_eq!(
                frame_packets(m, &colors).unwrap(),
                expected
                    .iter()
                    .filter_map(|a| if let Action::Send(p) = a {
                        Some(*p)
                    } else {
                        None
                    })
                    .collect::<Vec<_>>()
            );
        }
        assert_eq!(c.pos, data.len());
    }
    #[test]
    fn every_feature_failure_is_terminal_with_no_retries_or_late_termination() {
        for m in [Model::Normal, Model::Pro] {
            for partial in [false, true] {
                for fail in 1..=12 {
                    let shared = Rc::new(RefCell::new(Shared {
                        fail_at: Some(fail),
                        partial,
                        ..Default::default()
                    }));
                    let mut controller =
                        Controller::new(m, &descriptor(m), Fake(shared.clone())).unwrap();
                    let colors = vec![Rgb::new(1, 2, 3); m.leds()];
                    let e = controller.frame(&colors).unwrap_err();
                    assert!(controller.failed());
                    assert_eq!(e.counts.write_attempts + e.counts.read_attempts, fail);
                    assert_eq!(e.counts.writes + e.counts.reads, fail - 1);
                    assert_eq!(
                        e.counts.bytes_written + e.counts.bytes_read,
                        65 * (fail - 1) + if partial { 64 } else { 0 }
                    );
                    let expected = [
                        Stage::InitializeWrite,
                        Stage::InitializeRead,
                        Stage::Data(0),
                        Stage::Data(1),
                        Stage::Data(2),
                        Stage::Data(3),
                        Stage::Data(4),
                        Stage::Data(5),
                        Stage::Data(6),
                        Stage::TerminateClear,
                        Stage::TerminateWrite,
                        Stage::TerminateRead,
                    ];
                    assert_eq!(e.stage, expected[fail - 1]);
                    assert_eq!(shared.borrow().calls, fail);
                    let actions = shared.borrow().actions.len();
                    assert_eq!(controller.frame(&colors).unwrap_err().stage, Stage::Failed);
                    assert_eq!(shared.borrow().actions.len(), actions);
                }
            }
        }
    }
    #[test]
    fn descriptor_model_usage_and_complete_feature_caps_are_required() {
        for m in [Model::Normal, Model::Pro] {
            let d = descriptor(m);
            validate_descriptor(m, &d).unwrap();
            for field in 0..8 {
                let mut bad = d.clone();
                match field {
                    0 => bad.vid = 0,
                    1 => bad.pid = 0,
                    2 => bad.interface = 1,
                    3 => bad.usage_page = 0,
                    4 => bad.usage = 2,
                    5 => bad.feature_len = 64,
                    6 => bad.feature_caps_complete = false,
                    _ => bad.feature_ids = vec![1],
                };
                assert!(validate_descriptor(m, &bad).is_err());
            }
            let mut bad = d.clone();
            bad.feature_ids.clear();
            assert!(validate_descriptor(m, &bad).is_err());
            for path in [
                r"\\?\hid#vid_05ac&pid_024f&mi_02evil#x#x",
                r"\\?\hid#vid_05ac&pid_024f&mi_01#x#x",
                r"\\?\hid#vid_05ac&pid_024f&mi_02&c💡#x#x",
                r"\\.\hid#vid_05ac&pid_024f&mi_02#x#x",
            ] {
                assert!(validate_path(m, path).is_err());
            }
        }
    }
    #[test]
    fn invalid_frame_never_starts_io_and_wrong_read_id_is_terminal() {
        let m = Model::Normal;
        let shared = Rc::new(RefCell::new(Shared::default()));
        let mut c = Controller::new(m, &descriptor(m), Fake(shared.clone())).unwrap();
        assert_eq!(c.frame(&[]).unwrap_err().stage, Stage::Validate);
        assert!(!c.failed());
        assert_eq!(shared.borrow().calls, 0);
        shared.borrow_mut().bad_id = true;
        assert_eq!(
            c.frame(&vec![Rgb::default(); 102]).unwrap_err().stage,
            Stage::InitializeRead
        );
        assert!(c.failed());
        assert_eq!(shared.borrow().calls, 2);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    Normal,
    Pro,
}
impl Model {
    pub fn id(self) -> &'static str {
        match self {
            Self::Normal => "vk99",
            Self::Pro => "vk99-pro",
        }
    }
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "vk99" => Some(Self::Normal),
            "vk99-pro" => Some(Self::Pro),
            _ => None,
        }
    }
    pub fn leds(self) -> usize {
        match self {
            Self::Normal => 102,
            Self::Pro => 98,
        }
    }
    pub fn interface(self) -> i32 {
        match self {
            Self::Normal => 2,
            Self::Pro => 3,
        }
    }
    /// Row-major native LED indices; u16::MAX is a hole. Both matrices are 22x6.
    pub fn matrix(self) -> &'static [u16; 132] {
        match self {
            Self::Normal => &NORMAL_MATRIX,
            Self::Pro => &PRO_MATRIX,
        }
    }
    fn key_codes(self) -> &'static [u8] {
        match self {
            Self::Normal => &NORMAL_KEYS,
            Self::Pro => &PRO_KEYS,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub path: String,
    pub vid: u16,
    pub pid: u16,
    pub interface: i32,
    pub usage_page: u16,
    pub usage: u16,
    pub feature_len: usize,
    /// Every returned value/button capability's report ID. Incomplete/unknown caps are rejected.
    pub feature_ids: Vec<u8>,
    pub feature_caps_complete: bool,
}
pub fn validate_path(model: Model, path: &str) -> Result<(), String> {
    if path.len() > 4096 || path.chars().any(char::is_control) {
        return Err("Invalid Valkyrie HID path".into());
    }
    let mut parts = path.split('#');
    if !parts
        .next()
        .is_some_and(|s| s.eq_ignore_ascii_case(r"\\?\hid"))
    {
        return Err("Expected canonical HID interface path".into());
    }
    let hardware = parts.next().ok_or("Missing HID hardware tokens")?;
    let mut tokens = hardware.split('&');
    let expected = format!("mi_{:02}", model.interface());
    for token in ["vid_05ac", "pid_024f", &expected] {
        if !tokens.next().is_some_and(|s| s.eq_ignore_ascii_case(token)) {
            return Err("Valkyrie VID/PID/interface path mismatch".into());
        }
    }
    if let Some(collection) = tokens.next()
        && (collection.len() != 5
            || !collection.as_bytes()[..3].eq_ignore_ascii_case(b"col")
            || !collection.as_bytes()[3..].iter().all(u8::is_ascii_hexdigit)
            || tokens.next().is_some())
    {
        return Err("Unexpected HID collection suffix".into());
    }
    if !parts.next().is_some_and(|s| !s.is_empty())
        || !parts.next().is_some_and(|s| !s.is_empty())
        || parts.next().is_some()
    {
        return Err("Incomplete HID interface path".into());
    }
    Ok(())
}
pub fn validate_descriptor(model: Model, d: &Descriptor) -> Result<(), String> {
    validate_path(model, &d.path)?;
    if d.vid != 0x05ac
        || d.pid != 0x024f
        || d.interface != model.interface()
        || d.usage_page != 0xff13
        || d.usage != 1
    {
        return Err("Valkyrie model/interface/usage mismatch".into());
    }
    if d.feature_len != REPORT_BYTES
        || !d.feature_caps_complete
        || d.feature_ids.is_empty()
        || d.feature_ids.len() > 256
        || d.feature_ids.iter().any(|id| *id != 0)
    {
        return Err("Valkyrie requires complete 65-byte feature report ID zero caps".into());
    }
    // Source uses feature reports only. Input/output report sizes are not inferred requirements.
    Ok(())
}

/// Full native wire sequence: initialize, seven data chunks, zero flush, terminate.
pub fn frame_packets(model: Model, colors: &[Rgb]) -> Result<Vec<[u8; REPORT_BYTES]>, String> {
    if colors.len() != model.leds() {
        return Err("Valkyrie frame must contain exactly the model LED count".into());
    }
    let mut result = Vec::with_capacity(FEATURE_WRITES_PER_FRAME);
    let mut initialize = [0; REPORT_BYTES];
    initialize[1] = 4;
    initialize[2] = 0x20;
    initialize[9] = 8;
    result.push(initialize);
    for (chunk, keys) in model.key_codes().chunks(16).enumerate() {
        let mut packet = [0; REPORT_BYTES];
        for (j, &key) in keys.iter().enumerate() {
            let c = colors[chunk * 16 + j];
            packet[j * 4 + 1..j * 4 + 5].copy_from_slice(&[key, c.r, c.g, c.b]);
        }
        result.push(packet);
    }
    result.push([0; REPORT_BYTES]);
    let mut terminate = [0; REPORT_BYTES];
    terminate[1] = 4;
    terminate[2] = 2;
    result.push(terminate);
    Ok(result)
}

pub trait Transport {
    fn send_feature(&mut self, packet: &[u8; REPORT_BYTES]) -> Result<usize, String>;
    /// Buffer starts zeroed, with report ID zero. No undocumented ACK body is assumed.
    fn get_feature(&mut self, packet: &mut [u8; REPORT_BYTES]) -> Result<usize, String>;
    fn sleep(&mut self, duration: Duration);
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub write_attempts: usize,
    pub read_attempts: usize,
    pub writes: usize,
    pub reads: usize,
    pub bytes_written: usize,
    pub bytes_read: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Validate,
    InitializeWrite,
    InitializeRead,
    Data(u8),
    TerminateClear,
    TerminateWrite,
    TerminateRead,
    Failed,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameError {
    pub stage: Stage,
    pub counts: Counts,
    pub message: String,
}
impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Valkyrie {:?}: {}", self.stage, self.message)
    }
}
impl std::error::Error for FrameError {}
pub struct Controller<T: Transport> {
    model: Model,
    transport: T,
    failed: bool,
}
impl<T: Transport> Controller<T> {
    /// Descriptor validation performs no I/O. The native transport owns its handle and closes on Drop.
    pub fn new(model: Model, descriptor: &Descriptor, transport: T) -> Result<Self, String> {
        validate_descriptor(model, descriptor)?;
        Ok(Self {
            model,
            transport,
            failed: false,
        })
    }
    pub fn failed(&self) -> bool {
        self.failed
    }
    pub fn frame(&mut self, colors: &[Rgb]) -> Result<Counts, FrameError> {
        let mut counts = Counts::default();
        if self.failed {
            return Err(FrameError {
                stage: Stage::Failed,
                counts,
                message: "Transport is terminal; create a new validated connection".into(),
            });
        }
        let packets = frame_packets(self.model, colors).map_err(|message| FrameError {
            stage: Stage::Validate,
            counts,
            message,
        })?;
        self.write(&packets[0], Stage::InitializeWrite, &mut counts)?;
        self.pause(1);
        self.read(Stage::InitializeRead, &mut counts)?;
        self.pause(1);
        for i in 0..7 {
            self.write(&packets[1 + i], Stage::Data(i as u8), &mut counts)?;
            self.pause(1);
        }
        self.write(&packets[8], Stage::TerminateClear, &mut counts)?;
        self.pause(1);
        self.write(&packets[9], Stage::TerminateWrite, &mut counts)?;
        self.pause(1);
        self.read(Stage::TerminateRead, &mut counts)?;
        self.pause(1);
        self.pause(33);
        Ok(counts)
    }
    fn pause(&mut self, ms: u64) {
        self.transport.sleep(Duration::from_millis(ms));
    }
    fn write(
        &mut self,
        p: &[u8; REPORT_BYTES],
        stage: Stage,
        c: &mut Counts,
    ) -> Result<(), FrameError> {
        c.write_attempts += 1;
        let result = self.transport.send_feature(p);
        if let Ok(n) = result.as_ref() {
            c.bytes_written += (*n).min(REPORT_BYTES);
            if *n == REPORT_BYTES {
                c.writes += 1;
                return Ok(());
            }
        }
        self.failed = true;
        Err(FrameError {
            stage,
            counts: *c,
            message: match result {
                Ok(n) => format!("Incomplete feature write: {n}/65; frame abandoned, no retry"),
                Err(e) => e,
            },
        })
    }
    fn read(&mut self, stage: Stage, c: &mut Counts) -> Result<(), FrameError> {
        c.read_attempts += 1;
        let mut packet = [0; REPORT_BYTES];
        let result = self.transport.get_feature(&mut packet);
        if let Ok(n) = result.as_ref() {
            c.bytes_read += (*n).min(REPORT_BYTES);
            if *n == REPORT_BYTES && packet[0] == 0 {
                c.reads += 1;
                return Ok(());
            }
        }
        self.failed = true;
        Err(FrameError {
            stage,
            counts: *c,
            message: match result {
                Ok(n) => format!(
                    "Invalid feature read: {n}/65 or report ID mismatch; frame abandoned, no retry"
                ),
                Err(e) => e,
            },
        })
    }
}

// Exact source lookup tables; regenerated by work/valkyrie-protocol/extract-arrays.py.
const NORMAL_KEYS: [u8; 102] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 119, 112, 115, 117, 120, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31, 103, 116, 32, 33, 34, 122, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47,
    48, 49, 67, 118, 50, 51, 52, 123, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 85, 121, 68,
    69, 70, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 101, 86, 87, 88, 106, 91, 92, 93, 94,
    95, 96, 98, 99, 100, 102, 104, 105,
];
const PRO_KEYS: [u8; 98] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 119, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30,
    31, 103, 116, 32, 33, 34, 122, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 67, 118, 50,
    51, 52, 123, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 85, 121, 68, 69, 70, 73, 74, 75,
    76, 77, 78, 79, 80, 81, 82, 83, 84, 101, 86, 87, 88, 106, 91, 92, 93, 94, 95, 96, 98, 99, 100,
    102, 104, 105,
];
const NORMAL_MATRIX: [u16; 132] = [
    0, 65535, 1, 2, 3, 4, 65535, 5, 6, 7, 8, 65535, 9, 10, 11, 12, 65535, 13, 14, 15, 16, 17, 18,
    19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 65535, 65535, 65535, 32, 33, 34, 35, 36,
    37, 65535, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 65535, 65535, 51, 52, 53, 54,
    55, 56, 65535, 65535, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 65535, 67, 68, 65535, 69, 70, 71,
    72, 65535, 73, 65535, 65535, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 65535, 65535, 85,
    65535, 86, 87, 88, 89, 90, 91, 92, 65535, 65535, 65535, 65535, 93, 65535, 65535, 65535, 65535,
    94, 95, 96, 97, 98, 99, 65535, 100, 101, 65535,
];
const PRO_MATRIX: [u16; 132] = [
    0, 65535, 1, 2, 3, 4, 65535, 5, 6, 7, 8, 65535, 9, 10, 11, 12, 65535, 13, 65535, 65535, 65535,
    65535, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 65535, 65535, 65535, 28, 29, 30,
    31, 32, 33, 65535, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 65535, 65535, 47, 48,
    49, 50, 51, 52, 65535, 65535, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 65535, 63, 64, 65535, 65,
    66, 67, 68, 65535, 69, 65535, 65535, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 65535, 65535,
    81, 65535, 82, 83, 84, 85, 86, 87, 88, 65535, 65535, 65535, 65535, 89, 65535, 65535, 65535,
    65535, 90, 91, 92, 93, 94, 95, 65535, 96, 97, 65535,
];
