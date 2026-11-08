// SPDX-License-Identifier: GPL-2.0-or-later
//! Pure descriptor policy from native TT display/liquid source, without enumeration.
//! Metadata must come from successful complete caps queries; this is not device proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Display,
    Liquid,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub path: String,
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub input_len: u16,
    pub output_len: u16,
    pub feature_len: u16,
    pub input_buttons: u16,
    pub output_buttons: u16,
    /// One entry per successfully returned HIDP_VALUE_CAPS; no omitted caps allowed.
    pub input_ids: Vec<u8>,
    pub output_ids: Vec<u8>,
}
impl Kind {
    fn path_fragment(self) -> &'static str {
        match self {
            Self::Display => "vid_264a&pid_233c&mi_01",
            Self::Liquid => "vid_264a&pid_233c&mi_00",
        }
    }
    fn lengths(self) -> (u16, u16) {
        match self {
            Self::Display => (17, 1025),
            Self::Liquid => (441, 441),
        }
    }
}
fn fail(message: &str) -> String {
    message.to_owned()
}
fn starts_ascii_case(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|v| v.eq_ignore_ascii_case(prefix))
}
fn matches_hint(kind: Kind, path: &str) -> bool {
    path.as_bytes()
        .windows(kind.path_fragment().len())
        .any(|w| w.eq_ignore_ascii_case(kind.path_fragment().as_bytes()))
}
fn path_gate(kind: Kind, path: &str) -> Result<(), String> {
    if path.chars().any(char::is_control) || !starts_ascii_case(path, r"\\?\hid#") {
        return Err(fail(
            "Expected a canonical HID interface path without control characters",
        ));
    }
    let mut parts = path.split('#');
    parts.next();
    let hardware_id = parts
        .next()
        .ok_or_else(|| fail("Missing HID hardware ID"))?;
    let mut tokens = hardware_id.split('&');
    for expected in kind.path_fragment().split('&') {
        if !tokens
            .next()
            .is_some_and(|token| token.eq_ignore_ascii_case(expected))
        {
            return Err(fail("Unexpected TT VID/PID/MI path tokens"));
        }
    }
    // HID may expose a collection token after the exact MI, but not another MI suffix.
    if let Some(collection) = tokens.next()
        && (collection.len() != 5
            || !starts_ascii_case(collection, "col")
            || !collection.as_bytes()[3..].iter().all(u8::is_ascii_hexdigit)
            || tokens.next().is_some())
    {
        return Err(fail("Unexpected HID collection/path suffix"));
    }
    if !parts.next().is_some_and(|instance| !instance.is_empty()) {
        return Err(fail("Missing HID interface instance"));
    }
    Ok(())
}
fn zero_reports(ids: &[u8]) -> bool {
    (1..=128).contains(&ids.len()) && ids.iter().all(|id| *id == 0)
}
/// Path-only preflight before opening a data handle; caps must still be validated.
pub fn validate_path(kind: Kind, path: &str) -> Result<(), String> {
    path_gate(kind, path)
}
pub fn validate(kind: Kind, d: &Descriptor) -> Result<(), String> {
    path_gate(kind, &d.path)?;
    if d.vid != 0x264a || d.pid != 0x233c {
        return Err(fail("TT VID/PID attribute mismatch"));
    }
    if d.usage_page != 0xff06 || d.usage != 1 {
        return Err(fail("TT HID usage mismatch"));
    }
    let (input, output) = kind.lengths();
    if d.input_len != input || d.output_len != output || d.feature_len != 0 {
        return Err(fail("TT HID report length/feature mismatch"));
    }
    if d.input_buttons != 0 || d.output_buttons != 0 {
        return Err(fail("Unexpected TT HID button caps"));
    }
    if !zero_reports(&d.input_ids) || !zero_reports(&d.output_ids) {
        return Err(fail(
            "TT value caps require 1..128 entries with report ID zero",
        ));
    }
    Ok(())
}
/// Explicit targets compare case-sensitively, matching native LCD target equality.
/// Any invalid descriptor with a native matching-MI hint aborts the selection, even
/// if another descriptor validates. This deliberately tightens native LCD skipping.
/// Auto-selection is opt-in and requires one valid candidate; never pick the first.
pub fn select(
    kind: Kind,
    candidates: &[Descriptor],
    target: Option<&str>,
    allow_unique_auto: bool,
) -> Result<Descriptor, String> {
    if let Some(target) = target {
        path_gate(kind, target)?;
    } else if !allow_unique_auto {
        return Err(fail("An explicit TT HID target is required"));
    }
    let mut eligible = Vec::new();
    for candidate in candidates {
        if matches_hint(kind, &candidate.path) {
            validate(kind, candidate)?;
            eligible.push(candidate);
        } else if target.is_some_and(|target| target == candidate.path) {
            return Err(fail("Selected target is not the expected TT HID interface"));
        }
    }
    let mut selected = None;
    for candidate in eligible {
        if target.is_none_or(|target| target == candidate.path)
            && selected.replace(candidate).is_some()
        {
            return Err(fail("Ambiguous TT HID interface selection"));
        }
    }
    selected
        .cloned()
        .ok_or_else(|| fail("Requested TT HID interface is unavailable; fallback is forbidden"))
}
/// Complete process basenames must be supplied by the host. Enumeration failures are
/// host errors, not an empty successful inventory. No process is queried here.
pub fn guard_processes(names: &[String]) -> Result<(), String> {
    if names
        .iter()
        .any(|name| starts_ascii_case(name, "TTRGBPlus") || starts_ascii_case(name, "LibHWInfo"))
    {
        return Err(fail(
            "Liquid query blocked: TT RGB Plus / LibHWInfo is running",
        ));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn descriptor(kind: Kind, instance: &str) -> Descriptor {
        let (input_len, output_len) = kind.lengths();
        Descriptor {
            path: format!(
                r"\\?\hid#{}#{}#{{synthetic-interface}}",
                kind.path_fragment(),
                instance
            ),
            vid: 0x264a,
            pid: 0x233c,
            usage_page: 0xff06,
            usage: 1,
            input_len,
            output_len,
            feature_len: 0,
            input_buttons: 0,
            output_buttons: 0,
            input_ids: vec![0],
            output_ids: vec![0],
        }
    }
    #[test]
    fn exact_caps_for_both_interfaces() {
        for kind in [Kind::Display, Kind::Liquid] {
            let d = descriptor(kind, "one");
            assert!(validate(kind, &d).is_ok());
            let other = if kind == Kind::Display {
                Kind::Liquid
            } else {
                Kind::Display
            };
            assert!(validate(other, &d).is_err());
            for field in 0..9 {
                let mut bad = d.clone();
                match field {
                    0 => bad.vid = 0,
                    1 => bad.pid = 0,
                    2 => bad.usage_page = 1,
                    3 => bad.usage = 2,
                    4 => bad.input_len -= 1,
                    5 => bad.output_len -= 1,
                    6 => bad.feature_len = 1,
                    7 => bad.input_buttons = 1,
                    _ => bad.output_buttons = 1,
                }
                assert!(validate(kind, &bad).is_err(), "field{field}");
            }
        }
    }
    #[test]
    fn report_caps_bounds_and_all_ids() {
        for kind in [Kind::Display, Kind::Liquid] {
            for input in [true, false] {
                for count in [0, 129] {
                    let mut bad = descriptor(kind, "one");
                    if input {
                        bad.input_ids = vec![0; count]
                    } else {
                        bad.output_ids = vec![0; count]
                    }
                    assert!(validate(kind, &bad).is_err());
                }
                let mut d = descriptor(kind, "one");
                d.input_ids = vec![0; 128];
                d.output_ids = vec![0; 128];
                assert!(validate(kind, &d).is_ok());
                if input {
                    d.input_ids[127] = 1
                } else {
                    d.output_ids[127] = 1
                }
                assert!(validate(kind, &d).is_err());
            }
        }
    }
    #[test]
    fn paths_tokens_collections_and_case() {
        let kind = Kind::Display;
        let d = descriptor(kind, "one");
        let mut upper = d.clone();
        upper.path = upper.path.to_ascii_uppercase();
        assert!(validate(kind, &upper).is_ok());
        let mut collection = d.clone();
        collection.path = collection.path.replace("mi_01#", "mi_01&col01#");
        assert!(validate(kind, &collection).is_ok());
        for path in [
            "unknown".to_owned(),
            d.path.replace("mi_01", "mi_010"),
            d.path.replace("mi_01", "mi_00"),
            d.path.replace("vid_264a", "vid_ffff"),
            d.path.replace("mi_01#", "mi_01&mi_00#"),
            format!("{}\0suffix", d.path),
            format!("{}\nsuffix", d.path),
            r"\\?\hid#vid_264a&pid_233c&mi_01#".to_owned(),
        ] {
            let mut bad = d.clone();
            bad.path = path;
            assert!(validate(kind, &bad).is_err(), "{}", bad.path);
        }
    }
    #[test]
    fn explicit_and_opt_in_singleton() {
        for kind in [Kind::Display, Kind::Liquid] {
            let a = descriptor(kind, "one");
            let b = descriptor(kind, "two");
            let candidates = vec![a.clone(), b.clone()];
            let before = candidates.clone();
            assert_eq!(select(kind, &candidates, Some(&a.path), false).unwrap(), a);
            assert!(select(kind, &candidates, None, true).is_err());
            assert!(select(kind, std::slice::from_ref(&a), None, false).is_err());
            assert_eq!(
                select(kind, std::slice::from_ref(&a), None, true).unwrap(),
                a
            );
            assert!(select(kind, &[], Some(&a.path), true).is_err());
            assert!(select(kind, std::slice::from_ref(&b), Some(&a.path), true).is_err());
            assert!(select(kind, &[a.clone(), a.clone()], Some(&a.path), false).is_err());
            assert!(select(kind, &candidates, Some(""), true).is_err());
            assert_eq!(candidates, before);
        }
    }
    #[test]
    fn matching_invalid_never_falls_back() {
        for kind in [Kind::Display, Kind::Liquid] {
            let good = descriptor(kind, "good");
            let mut bad = descriptor(kind, "bad");
            bad.output_len = 0;
            for candidates in [
                vec![good.clone(), bad.clone()],
                vec![bad.clone(), good.clone()],
            ] {
                assert!(select(kind, &candidates, None, true).is_err());
                assert!(select(kind, &candidates, Some(&good.path), false).is_err());
            }
            let mut prefix_spoof = descriptor(kind, "spoof");
            prefix_spoof.path = prefix_spoof
                .path
                .replace("mi_01#", "mi_010#")
                .replace("mi_00#", "mi_000#");
            assert!(select(kind, &[good, prefix_spoof], None, true).is_err());
        }
    }
    #[test]
    fn unrelated_interface_and_changed_caps() {
        let good = descriptor(Kind::Display, "one");
        let other = descriptor(Kind::Liquid, "other");
        assert_eq!(
            select(
                Kind::Display,
                &[good.clone(), other],
                Some(&good.path),
                false
            )
            .unwrap(),
            good
        );
        let mut changed = good.clone();
        changed.input_len = 441;
        changed.output_len = 441;
        assert!(validate(Kind::Display, &changed).is_err());
        assert!(select(Kind::Display, &[changed], Some(&good.path), true).is_err());
        let mut upper = good.clone();
        upper.path = upper.path.to_ascii_uppercase();
        assert!(select(Kind::Display, &[upper], Some(&good.path), true).is_err());
    }
    #[test]
    fn process_prefix_case_insensitive() {
        for name in [
            "TTRGBPlus.exe",
            "ttrgbplus",
            "TtRgBpLuS_Service.exe",
            "LibHWInfo.exe",
            "LIBHWINFO64.exe",
            "libhwinfowrapper",
        ] {
            assert!(
                guard_processes(&["safe.exe".into(), name.into()]).is_err(),
                "{name}"
            );
        }
        assert!(guard_processes(&[]).is_ok());
        assert!(
            guard_processes(&[
                "explorer.exe".into(),
                "OtherTTRGBPlus.exe".into(),
                "myLibHWInfo.exe".into()
            ])
            .is_ok()
        );
    }
}
