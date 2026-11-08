//! Policy-first adapters with injectable discovery and report IO; no native calls here.
use super::{
    hid_policy::{self, Descriptor, Kind},
    protocol::{ACK_BYTES, REPORT_BYTES, ReportTransport},
    temperatures::{self, LIQUID_ID, Reading},
};
pub trait Reports {
    fn now_ms(&self) -> u64;
    fn descriptor(&mut self) -> Result<Descriptor, String>;
    fn write(&mut self, bytes: &[u8], timeout_ms: u32) -> Result<usize, String>;
    fn read(&mut self, bytes: &mut [u8], timeout_ms: u32) -> Result<usize, String>;
}
pub trait Backend {
    type Link: Reports;
    fn process_names(&mut self) -> Result<Vec<String>, String>;
    fn candidates(&mut self, kind: Kind) -> Result<Vec<Descriptor>, String>;
    /// Native implementations must open data exclusively; metadata uses no data access.
    fn open(&mut self, kind: Kind, path: &str) -> Result<Self::Link, String>;
}
pub fn allowed_write(kind: Kind, bytes: &[u8]) -> Result<(), String> {
    match kind {
        Kind::Liquid if bytes == temperatures::liquid_status_request() => Ok(()),
        Kind::Display
            if bytes.len() == REPORT_BYTES
                && bytes[0] == 0
                && bytes[1] == 8
                && bytes[3] == 0
                && (bytes[4] == 0 || bytes[4] == 0x80)
                && bytes[2] != 0 =>
        {
            Ok(())
        }
        _ => Err("Report is not an allowed TT display/status operation".into()),
    }
}
fn open_verified<B: Backend>(
    backend: &mut B,
    kind: Kind,
    target: Option<&str>,
    auto: bool,
) -> Result<B::Link, String> {
    hid_policy::guard_processes(&backend.process_names()?)?;
    let candidates = backend.candidates(kind)?;
    let selected = hid_policy::select(kind, &candidates, target, auto)?;
    let mut link = backend.open(kind, &selected.path)?;
    let actual = link.descriptor()?;
    hid_policy::validate(kind, &actual)?;
    if actual != selected {
        return Err("TT descriptor changed between selection and data open".into());
    }
    hid_policy::guard_processes(&backend.process_names()?)?;
    Ok(link)
}
pub struct Display<L: Reports> {
    link: L,
}
pub fn open_display<B: Backend>(backend: &mut B, target: &str) -> Result<Display<B::Link>, String> {
    Ok(Display {
        link: open_verified(backend, Kind::Display, Some(target), false)?,
    })
}
impl<L: Reports> ReportTransport for Display<L> {
    fn now_ms(&self) -> u64 {
        self.link.now_ms()
    }
    fn write_report(
        &mut self,
        bytes: &[u8; REPORT_BYTES],
        timeout_ms: u64,
    ) -> Result<usize, String> {
        allowed_write(Kind::Display, bytes)?;
        if !(1..=250).contains(&timeout_ms) {
            return Err("Invalid TT HID write timeout".into());
        }
        self.link.write(
            bytes,
            u32::try_from(timeout_ms).map_err(|_| "Invalid HID write timeout")?,
        )
    }
    fn read_ack(&mut self, bytes: &mut [u8; ACK_BYTES], timeout_ms: u64) -> Result<usize, String> {
        if !(1..=250).contains(&timeout_ms) {
            return Err("Invalid TT HID read timeout".into());
        }
        self.link.read(
            bytes,
            u32::try_from(timeout_ms).map_err(|_| "Invalid HID read timeout")?,
        )
    }
}
pub fn read_liquid<B: Backend>(
    backend: &mut B,
    target: Option<&str>,
    allow_unique_auto: bool,
) -> Result<Reading, String> {
    let mut link = open_verified(backend, Kind::Liquid, target, allow_unique_auto)?;
    let outgoing = temperatures::liquid_status_request();
    allowed_write(Kind::Liquid, &outgoing)?;
    if link.write(&outgoing, 250)? != outgoing.len() {
        return Err("Short liquid status write".into());
    }
    let mut incoming = [0u8; 441];
    if link.read(&mut incoming, 250)? != incoming.len() {
        return Err("Short liquid status read".into());
    }
    let celsius = temperatures::parse_liquid(&incoming)?;
    Ok(Reading {
        id: LIQUID_ID.into(),
        name: "Liquid".into(),
        origin: "TTLCD MI00 0x82".into(),
        celsius,
    })
}

#[cfg(test)]
mod tests {
    use super::super::protocol;
    use super::*;
    use std::sync::{Arc, Mutex};
    fn descriptor(kind: Kind) -> Descriptor {
        let (mi, input, output) = match kind {
            Kind::Display => ("01", 17, 1025),
            Kind::Liquid => ("00", 441, 441),
        };
        Descriptor {
            path: format!(r"\\?\hid#vid_264a&pid_233c&mi_{mi}#fixture"),
            vid: 0x264a,
            pid: 0x233c,
            usage_page: 0xff06,
            usage: 1,
            input_len: input,
            output_len: output,
            feature_len: 0,
            input_buttons: 0,
            output_buttons: 0,
            input_ids: vec![0],
            output_ids: vec![0],
        }
    }
    struct Fake {
        kind: Kind,
        log: Arc<Mutex<Vec<&'static str>>>,
        changed: bool,
        late_process: bool,
        guards: u8,
    }
    struct Link {
        d: Descriptor,
        log: Arc<Mutex<Vec<&'static str>>>,
    }
    impl Drop for Link {
        fn drop(&mut self) {
            self.log.lock().unwrap().push("close");
        }
    }
    impl Reports for Link {
        fn now_ms(&self) -> u64 {
            1
        }
        fn descriptor(&mut self) -> Result<Descriptor, String> {
            self.log.lock().unwrap().push("revalidate");
            Ok(self.d.clone())
        }
        fn write(&mut self, bytes: &[u8], _: u32) -> Result<usize, String> {
            self.log.lock().unwrap().push("write");
            Ok(bytes.len())
        }
        fn read(&mut self, bytes: &mut [u8], _: u32) -> Result<usize, String> {
            self.log.lock().unwrap().push("read");
            bytes.fill(0);
            if bytes.len() == 441 {
                bytes[..5].copy_from_slice(&[0, 0x82, 1, 0, 0x80]);
                bytes[5..7].copy_from_slice(&3340u16.to_le_bytes());
            }
            Ok(bytes.len())
        }
    }
    impl Backend for Fake {
        type Link = Link;
        fn process_names(&mut self) -> Result<Vec<String>, String> {
            self.log.lock().unwrap().push("guard");
            self.guards += 1;
            Ok(if self.late_process && self.guards >= 2 {
                vec!["TTRGBPlus.exe".into()]
            } else {
                Vec::new()
            })
        }
        fn candidates(&mut self, _: Kind) -> Result<Vec<Descriptor>, String> {
            self.log.lock().unwrap().push("enumerate");
            Ok(vec![descriptor(self.kind)])
        }
        fn open(&mut self, _: Kind, _: &str) -> Result<Link, String> {
            self.log.lock().unwrap().push("open-exclusive");
            let mut d = descriptor(self.kind);
            if self.changed {
                d.feature_len = 1;
            }
            Ok(Link {
                d,
                log: self.log.clone(),
            })
        }
    }
    fn backend(kind: Kind) -> Fake {
        Fake {
            kind,
            log: Arc::default(),
            changed: false,
            late_process: false,
            guards: 0,
        }
    }
    #[test]
    fn liquid_status_has_exact_two_io_operations_after_both_guards() {
        let mut b = backend(Kind::Liquid);
        let value = read_liquid(&mut b, None, true).unwrap();
        assert_eq!(value.celsius, 33.4);
        assert_eq!(
            *b.log.lock().unwrap(),
            vec![
                "guard",
                "enumerate",
                "open-exclusive",
                "revalidate",
                "guard",
                "write",
                "read",
                "close"
            ]
        );
    }
    #[test]
    fn changed_caps_and_late_conflict_close_without_report_io() {
        for changed in [true, false] {
            let mut b = backend(Kind::Liquid);
            b.changed = changed;
            b.late_process = !changed;
            assert!(read_liquid(&mut b, None, true).is_err());
            let log = b.log.lock().unwrap();
            assert!(log.contains(&"close"));
            assert!(!log.contains(&"write"));
            assert!(!log.contains(&"read"));
        }
    }
    #[test]
    fn target_mismatch_and_wrong_commands_are_rejected_before_data() {
        let mut b = backend(Kind::Liquid);
        assert!(read_liquid(&mut b, Some(&descriptor(Kind::Display).path), false).is_err());
        assert!(!b.log.lock().unwrap().contains(&"open-exclusive"));
        let mut status = temperatures::liquid_status_request();
        status[1] = 0x83;
        assert!(allowed_write(Kind::Liquid, &status).is_err());
        let mut lcd = [0; REPORT_BYTES];
        lcd[1] = 8;
        lcd[2] = 1;
        lcd[4] = 0x80;
        assert!(allowed_write(Kind::Display, &lcd).is_ok());
        lcd[1] = 0x82;
        assert!(allowed_write(Kind::Display, &lcd).is_err());
    }
    #[test]
    fn display_transport_preserves_report_sizes_and_ack_validation() {
        let mut b = backend(Kind::Display);
        let path = descriptor(Kind::Display).path;
        let mut display = open_display(&mut b, &path).unwrap();
        let frame = protocol::packet(&[1, 2, 3], 0).unwrap();
        assert_eq!(display.write_report(&frame, 250).unwrap(), REPORT_BYTES);
        let mut ack = [0; ACK_BYTES];
        assert_eq!(display.read_ack(&mut ack, 250).unwrap(), ACK_BYTES);
        protocol::validate_ack(&ack).unwrap();
    }
}
