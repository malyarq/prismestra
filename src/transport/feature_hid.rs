//! Policy and injectable feature IO. Native entrypoints are dormant in stand tests.
use super::{fusion2, valkyrie};
use crate::{lighting::color::Rgb, runtime::output::Cancellation};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Fusion2 { headers: [usize; 2] },
    Valkyrie(valkyrie::Model),
}
impl Family {
    pub fn logical_len(self) -> usize {
        match self {
            Self::Fusion2 { .. } => 64,
            Self::Valkyrie(_) => 65,
        }
    }
    pub fn report_id(self) -> u8 {
        match self {
            Self::Fusion2 { .. } => 0xcc,
            Self::Valkyrie(_) => 0,
        }
    }
    pub fn validate_path(self, path: &str) -> Result<(), String> {
        match self {
            Self::Fusion2 { headers } => {
                fusion2::count_code(headers[0])?;
                fusion2::count_code(headers[1])?;
                fusion2::validate_path(path)
            }
            Self::Valkyrie(m) => valkyrie::validate_path(m, path),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub path: String,
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub feature_len: usize,
    pub feature_ids: Vec<u8>,
    pub feature_caps_complete: bool,
}
pub fn validate(family: Family, d: &Descriptor) -> Result<(), String> {
    family.validate_path(&d.path)?;
    match family {
        Family::Valkyrie(model) => valkyrie::validate_descriptor(
            model,
            &valkyrie::Descriptor {
                path: d.path.clone(),
                vid: d.vid,
                pid: d.pid,
                interface: model.interface(),
                usage_page: d.usage_page,
                usage: d.usage,
                feature_len: d.feature_len,
                feature_ids: d.feature_ids.clone(),
                feature_caps_complete: d.feature_caps_complete,
            },
        ),
        Family::Fusion2 { .. } => {
            // hidapi pads SET to the descriptor maximum. Do not guess that the
            // device maximum equals its64-byte logical controller packet.
            if (d.vid, d.pid, d.usage_page, d.usage)
                != (
                    fusion2::VID,
                    fusion2::PID,
                    fusion2::USAGE_PAGE,
                    fusion2::USAGE,
                )
                || !(64..=4096).contains(&d.feature_len)
                || !d.feature_caps_complete
                || d.feature_ids.is_empty()
                || d.feature_ids.len() > 256
                || d.feature_ids.iter().any(|id| *id != 0xcc)
            {
                return Err("Unsupported Fusion2 feature attributes/caps".into());
            }
            Ok(())
        }
    }
}
pub fn guard_processes(names: &[String]) -> Result<(), String> {
    const WRITERS: &[&str] = &[
        "openrgb.exe",
        "signalrgb.exe",
        "signalrgblauncher.exe",
        "rgbfusion.exe",
        "gcc.exe",
        "ledkeeper2.exe",
    ];
    if names
        .iter()
        .any(|n| WRITERS.iter().any(|w| n.eq_ignore_ascii_case(w)))
    {
        return Err("Another known RGB writer is running; it was left untouched".into());
    }
    Ok(())
}
pub trait Reports {
    fn descriptor(&mut self) -> Result<Descriptor, String>;
    /// SET: successful class completion has Information=0. GET: actual bytes,
    /// excluding the leading zero report ID as in hidapi's Windows backend.
    fn transfer(
        &mut self,
        get: bool,
        bytes: &mut [u8],
        cancel: &Cancellation,
    ) -> Result<usize, String>;
    fn sleep(&mut self, duration: Duration);
}
pub trait Backend {
    type Link: Reports;
    fn process_names(&mut self) -> Result<Vec<String>, String>;
    fn inspect(&mut self, family: Family, path: &str) -> Result<Descriptor, String>;
    fn open_exclusive(&mut self, family: Family, path: &str) -> Result<Self::Link, String>;
}
pub struct Verified<L: Reports> {
    link: Option<L>,
    descriptor: Descriptor,
    family: Family,
    cancel: Cancellation,
    wire: Vec<u8>,
    vk_template: Option<Vec<[u8; 65]>>,
    vk_step: usize,
    fusion_identity: Option<fusion2::Identity>,
    fusion_query: bool,
    fusion_direct: bool,
}
pub fn open<B: Backend>(
    backend: &mut B,
    family: Family,
    path: &str,
    cancel: Cancellation,
) -> Result<Verified<B::Link>, String> {
    family.validate_path(path)?;
    if cancel.cancelled() {
        return Err("RGB output stopping".into());
    }
    guard_processes(&backend.process_names()?)?;
    let selected = backend.inspect(family, path)?;
    validate(family, &selected)?;
    if !selected.path.eq_ignore_ascii_case(path) {
        return Err("HID inspected path changed".into());
    }
    let mut link = backend.open_exclusive(family, path)?;
    let actual = link.descriptor()?;
    validate(family, &actual)?;
    if actual != selected {
        return Err("Feature HID descriptor changed at open".into());
    }
    guard_processes(&backend.process_names()?)?;
    if cancel.cancelled() {
        return Err("RGB output stopping".into());
    }
    let vk_template = match family {
        Family::Valkyrie(model) => Some(valkyrie::frame_packets(
            model,
            &vec![Rgb::default(); model.leds()],
        )?),
        _ => None,
    };
    let wire = vec![0; actual.feature_len];
    Ok(Verified {
        link: Some(link),
        descriptor: actual,
        family,
        cancel,
        wire,
        vk_template,
        vk_step: 0,
        fusion_identity: None,
        fusion_query: false,
        fusion_direct: false,
    })
}
impl<L: Reports> Verified<L> {
    pub fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }
    fn allowed(&self, get: bool, packet: &[u8]) -> Result<(), String> {
        if packet.len() != self.family.logical_len() || packet[0] != self.family.report_id() {
            return Err("Feature report family/length mismatch".into());
        }
        match self.family {
            Family::Valkyrie(model) => {
                let expects_get = matches!(self.vk_step, 1 | 11);
                if expects_get != get {
                    return Err("Valkyrie feature transaction out of order".into());
                }
                if get {
                    return if packet.iter().all(|b| *b == 0) {
                        Ok(())
                    } else {
                        Err("Nonzero Valkyrie feature GET request".into())
                    };
                }
                let template = self.vk_template.as_ref().unwrap();
                let index = match self.vk_step {
                    0 => 0,
                    2..=8 => self.vk_step - 1,
                    9 => 8,
                    10 => 9,
                    _ => return Err("Invalid keyboard phase".into()),
                };
                if (1..=7).contains(&index) {
                    let mut masked: [u8; 65] = packet.try_into().unwrap();
                    for key in 0..16 {
                        masked[key * 4 + 2..key * 4 + 5].fill(0);
                    }
                    if masked != template[index] {
                        return Err("Valkyrie RGB key map changed".into());
                    }
                    // Padding after the final real key must be zero, not arbitrary data.
                    let valid = (model.leds() - (index - 1) * 16).min(16);
                    if packet[1 + valid * 4..].iter().any(|b| *b != 0) {
                        return Err("Nonzero keyboard tail".into());
                    }
                } else if packet != template[index] {
                    return Err("Unsupported keyboard control report".into());
                }
                Ok(())
            }
            Family::Fusion2 { headers } => {
                if get {
                    return if self.fusion_query && packet[1..].iter().all(|b| *b == 0) {
                        Ok(())
                    } else {
                        Err("Fusion2 GET without identity query".into())
                    };
                }
                let report: &fusion2::Report = packet.try_into().unwrap();
                match report[1] {
                    0x60 if *report == fusion2::command(0x60, 0, 0)? => Ok(()),
                    0x34 => {
                        let identity = self
                            .fusion_identity
                            .as_ref()
                            .ok_or("Count update before identity")?;
                        if fusion2::count_report(identity, headers)?.as_ref() != Some(report) {
                            return Err(
                                "Count report differs from owned header configuration".into()
                            );
                        }
                        Ok(())
                    }
                    0x32 if self.fusion_identity.is_some()
                        && *report == fusion2::direct_report() =>
                    {
                        Ok(())
                    }
                    0x58 | 0x59 if self.fusion_direct => {
                        let capacity = headers[(report[1] - 0x58) as usize] * 3;
                        let offset = u16::from_le_bytes([report[2], report[3]]) as usize;
                        if offset >= capacity
                            || !offset.is_multiple_of(57)
                            || usize::from(report[4]) != (capacity - offset).min(57)
                            || report[62..].iter().any(|b| *b != 0)
                        {
                            return Err("Fusion2 data span outside owned header".into());
                        }
                        Ok(())
                    }
                    _ => Err("Unsupported/unowned Fusion2 feature command".into()),
                }
            }
        }
    }
    fn transfer(&mut self, get: bool, packet: &mut [u8]) -> Result<usize, String> {
        if self.link.is_none() {
            return Err("Feature HID link is terminal".into());
        }
        let result = (|| {
            if self.cancel.cancelled() {
                return Err("RGB output stopping".into());
            }
            self.allowed(get, packet)?;
            let wire = &mut self.wire;
            wire.fill(0);
            wire[..packet.len()].copy_from_slice(packet);
            let count = self
                .link
                .as_mut()
                .unwrap()
                .transfer(get, wire, &self.cancel)?;
            if get {
                let actual = count
                    .checked_add(usize::from(self.family.report_id() == 0))
                    .ok_or("Feature size overflow")?;
                if actual < packet.len()
                    || actual > wire.len()
                    || wire[0] != self.family.report_id()
                {
                    return Err("Short/oversize/wrong-ID feature response".into());
                }
                packet.copy_from_slice(&wire[..packet.len()]);
            } else if count != 0 {
                return Err("Unexpected SET_FEATURE class Information count".into());
            }
            match self.family {
                Family::Valkyrie(_) => {
                    self.vk_step = (self.vk_step + 1) % 12;
                }
                Family::Fusion2 { .. } => {
                    if get {
                        let report = (&*packet).try_into().unwrap();
                        self.fusion_identity = Some(fusion2::parse_identity(report)?);
                        self.fusion_query = false;
                    } else if packet[1] == 0x60 {
                        self.fusion_query = true;
                    } else if packet[1] == 0x32 {
                        self.fusion_direct = true;
                    }
                }
            }
            Ok(packet.len())
        })();
        if result.is_err() {
            self.link = None;
        }
        result
    }
}
impl<L: Reports> fusion2::Transport for Verified<L> {
    fn feature_send(&mut self, p: &fusion2::Report) -> Result<usize, String> {
        let mut report = *p;
        self.transfer(false, &mut report)
    }
    fn feature_get(&mut self, p: &mut fusion2::Report) -> Result<usize, String> {
        self.transfer(true, p)
    }
    fn wait_ms(&mut self, milliseconds: u32) {
        if let Some(link) = self.link.as_mut() {
            link.sleep(Duration::from_millis(u64::from(milliseconds)));
        }
    }
}
impl<L: Reports> valkyrie::Transport for Verified<L> {
    fn send_feature(&mut self, p: &[u8; 65]) -> Result<usize, String> {
        let mut report = *p;
        self.transfer(false, &mut report)
    }
    fn get_feature(&mut self, p: &mut [u8; 65]) -> Result<usize, String> {
        self.transfer(true, p)
    }
    fn sleep(&mut self, duration: Duration) {
        if let Some(link) = self.link.as_mut() {
            link.sleep(duration);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct State {
        calls: usize,
        fail_at: Option<usize>,
        bad_count: Option<usize>,
        bad_id: bool,
        closed: bool,
        changed: bool,
        late_writer: bool,
        process_calls: usize,
        opens: usize,
        operations: Vec<(bool, Vec<u8>, usize)>,
    }
    struct Fake {
        d: Descriptor,
        s: Arc<Mutex<State>>,
    }
    struct Link {
        d: Descriptor,
        s: Arc<Mutex<State>>,
    }
    impl Drop for Link {
        fn drop(&mut self) {
            self.s.lock().unwrap().closed = true;
        }
    }
    fn identity() -> fusion2::Report {
        let hex = include_str!("fixtures/fusion2-native.tsv")
            .lines()
            .next()
            .unwrap()
            .split('\t')
            .nth(2)
            .unwrap();
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap()
    }
    fn fixture(family: Family, len: usize) -> (Fake, Arc<Mutex<State>>) {
        let (path, vid, pid, page, usage) = match family {
            Family::Fusion2 { .. } => (
                r"\\?\hid#vid_048d&pid_5702#stand#{guid}".into(),
                0x048d,
                0x5702,
                0xff89,
                0xcc,
            ),
            Family::Valkyrie(m) => (
                format!(
                    r"\\?\hid#vid_05ac&pid_024f&mi_{:02}#stand#{{guid}}",
                    m.interface()
                ),
                0x05ac,
                0x024f,
                0xff13,
                1,
            ),
        };
        let d = Descriptor {
            path,
            vid,
            pid,
            usage_page: page,
            usage,
            feature_len: len,
            feature_ids: vec![family.report_id()],
            feature_caps_complete: true,
        };
        let s = Arc::new(Mutex::new(State::default()));
        (Fake { d, s: s.clone() }, s)
    }
    impl Backend for Fake {
        type Link = Link;
        fn process_names(&mut self) -> Result<Vec<String>, String> {
            let mut s = self.s.lock().unwrap();
            s.process_calls += 1;
            Ok(if s.late_writer && s.process_calls >= 2 {
                vec!["OpenRGB.exe".into()]
            } else {
                vec![]
            })
        }
        fn inspect(&mut self, _: Family, _: &str) -> Result<Descriptor, String> {
            Ok(self.d.clone())
        }
        fn open_exclusive(&mut self, _: Family, _: &str) -> Result<Link, String> {
            self.s.lock().unwrap().opens += 1;
            Ok(Link {
                d: self.d.clone(),
                s: self.s.clone(),
            })
        }
    }
    impl Reports for Link {
        fn descriptor(&mut self) -> Result<Descriptor, String> {
            let mut d = self.d.clone();
            if self.s.lock().unwrap().changed {
                d.usage = 0;
            }
            Ok(d)
        }
        fn transfer(&mut self, get: bool, b: &mut [u8], _: &Cancellation) -> Result<usize, String> {
            let mut s = self.s.lock().unwrap();
            s.calls += 1;
            s.operations.push((get, b.to_vec(), b.as_ptr() as usize));
            if s.fail_at == Some(s.calls) {
                return Err("Injected IOCTL failure".into());
            }
            if get {
                if self.d.vid == 0x048d {
                    b[..64].copy_from_slice(&identity());
                } else {
                    b.fill(0);
                }
                if s.bad_id {
                    b[0] ^= 1;
                }
            }
            Ok(s.bad_count.unwrap_or(if get {
                b.len() - usize::from(self.d.feature_ids[0] == 0)
            } else {
                0
            }))
        }
        fn sleep(&mut self, _: Duration) {}
    }
    fn opened(f: &mut Fake, family: Family) -> Verified<Link> {
        let path = f.d.path.clone();
        open(f, family, &path, Cancellation::default()).unwrap()
    }
    fn vk_descriptor(d: &Descriptor, m: valkyrie::Model) -> valkyrie::Descriptor {
        valkyrie::Descriptor {
            path: d.path.clone(),
            vid: d.vid,
            pid: d.pid,
            interface: m.interface(),
            usage_page: d.usage_page,
            usage: d.usage,
            feature_len: d.feature_len,
            feature_ids: d.feature_ids.clone(),
            feature_caps_complete: d.feature_caps_complete,
        }
    }
    #[test]
    fn both_keyboards_run_exact_controller_sequence_and_reuse_one_wire_buffer() {
        for model in [valkyrie::Model::Normal, valkyrie::Model::Pro] {
            let family = Family::Valkyrie(model);
            let (mut f, s) = fixture(family, 65);
            let d = vk_descriptor(&f.d, model);
            let mut c = valkyrie::Controller::new(model, &d, opened(&mut f, family)).unwrap();
            let colors: Vec<_> = (0..model.leds())
                .map(|i| Rgb::new(i as u8, 93, 201))
                .collect();
            c.frame(&colors).unwrap();
            c.frame(&colors).unwrap();
            drop(c);
            let s = s.lock().unwrap();
            assert!(s.closed);
            assert_eq!(s.calls, 24);
            assert!(s.operations.iter().all(|op| op.2 == s.operations[0].2));
            let sets: Vec<_> = s
                .operations
                .iter()
                .filter(|op| !op.0)
                .take(10)
                .map(|op| op.1.clone())
                .collect();
            assert_eq!(
                sets,
                valkyrie::frame_packets(model, &colors)
                    .unwrap()
                    .into_iter()
                    .map(|p| p.to_vec())
                    .collect::<Vec<_>>()
            );
        }
    }
    #[test]
    fn fusion_descriptor_padding_and_nonzero_report_id_counts() {
        for len in [64, 65, 96, 4096] {
            let family = Family::Fusion2 { headers: [27, 27] };
            let (mut f, s) = fixture(family, len);
            let mut c = fusion2::Controller::connect(opened(&mut f, family), [27, 27]).unwrap();
            c.update(0, &[Rgb::new(7, 8, 9); 27]).unwrap();
            c.update(1, &[Rgb::new(1, 2, 3); 27]).unwrap();
            drop(c);
            let s = s.lock().unwrap();
            assert!(s.closed);
            assert!(
                s.operations
                    .iter()
                    .all(|op| op.1.len() == len && op.1[64..].iter().all(|b| *b == 0))
            );
        }
    }
    #[test]
    fn every_keyboard_io_failure_is_terminal_and_releases_raw_link() {
        let model = valkyrie::Model::Normal;
        let family = Family::Valkyrie(model);
        for failed in 1..=12 {
            let (mut f, s) = fixture(family, 65);
            s.lock().unwrap().fail_at = Some(failed);
            let d = vk_descriptor(&f.d, model);
            let mut c = valkyrie::Controller::new(model, &d, opened(&mut f, family)).unwrap();
            assert!(c.frame(&[Rgb::default(); 102]).is_err());
            assert!(c.frame(&[Rgb::default(); 102]).is_err());
            let s = s.lock().unwrap();
            assert_eq!(s.calls, failed);
            assert!(s.closed);
        }
    }
    #[test]
    fn wrong_id_short_overcapacity_and_set_information_are_terminal() {
        use valkyrie::Transport;
        let model = valkyrie::Model::Normal;
        let family = Family::Valkyrie(model);
        let init = valkyrie::frame_packets(model, &[Rgb::default(); 102]).unwrap()[0];
        for count in [0, 63, 65, usize::MAX] {
            let (mut f, s) = fixture(family, 65);
            let mut v = opened(&mut f, family);
            v.send_feature(&init).unwrap();
            s.lock().unwrap().bad_count = Some(count);
            assert!(v.get_feature(&mut [0; 65]).is_err());
            assert!(s.lock().unwrap().closed);
        }
        let (mut f, s) = fixture(family, 65);
        let mut v = opened(&mut f, family);
        v.send_feature(&init).unwrap();
        s.lock().unwrap().bad_id = true;
        assert!(v.get_feature(&mut [0; 65]).is_err());
        let (mut f, s) = fixture(family, 65);
        let mut v = opened(&mut f, family);
        s.lock().unwrap().bad_count = Some(1);
        assert!(v.send_feature(&init).is_err());
        assert!(s.lock().unwrap().closed);
    }
    #[test]
    fn preflight_and_revalidation_stop_before_reports() {
        let family = Family::Valkyrie(valkyrie::Model::Normal);
        for fault in 0..7 {
            let (mut f, s) = fixture(family, 65);
            match fault {
                0 => f.d.feature_caps_complete = false,
                1 => f.d.feature_ids = vec![],
                2 => f.d.feature_ids.push(1),
                3 => f.d.feature_len = 64,
                4 => f.d.vid = 0,
                5 => s.lock().unwrap().changed = true,
                _ => s.lock().unwrap().late_writer = true,
            };
            let path = f.d.path.clone();
            assert!(open(&mut f, family, &path, Cancellation::default()).is_err());
            assert_eq!(s.lock().unwrap().calls, 0);
        }
        let (mut f, s) = fixture(family, 65);
        let token = Cancellation::default();
        token.cancel_for_test();
        let path = f.d.path.clone();
        assert!(open(&mut f, family, &path, token).is_err());
        assert_eq!(s.lock().unwrap().opens, 0);
    }
    #[test]
    fn unsupported_commands_or_cancel_never_reach_raw_transport() {
        use fusion2::Transport;
        let family = Family::Fusion2 { headers: [27, 27] };
        let (mut f, s) = fixture(family, 64);
        let mut v = opened(&mut f, family);
        let mut reset = [0; 64];
        reset[0] = 0xcc;
        reset[1] = 0x20;
        assert!(v.feature_send(&reset).is_err());
        assert_eq!(s.lock().unwrap().calls, 0);
        assert!(s.lock().unwrap().closed);
        let (mut f, s) = fixture(family, 64);
        let mut v = opened(&mut f, family);
        assert!(
            v.feature_get(&mut {
                let mut p = [0; 64];
                p[0] = 0xcc;
                p
            })
            .is_err()
        );
        assert_eq!(s.lock().unwrap().calls, 0);
        let (mut f, s) = fixture(family, 64);
        let token = Cancellation::default();
        let path = f.d.path.clone();
        let mut v = open(&mut f, family, &path, token.clone()).unwrap();
        token.cancel_for_test();
        assert!(
            v.feature_send(&fusion2::command(0x60, 0, 0).unwrap())
                .is_err()
        );
        assert_eq!(s.lock().unwrap().calls, 0);
    }
}
