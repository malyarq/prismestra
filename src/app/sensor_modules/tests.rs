use super::*;
use crate::app::sensor_bindings::{Source, Target};

fn draft(family: Family) -> Draft {
    let target = match family {
        Family::Amd => Target::Amd {
            vendor: *b"AuthenticAMD",
            signature: 0x00800f10,
            packages: 1,
            module_path: String::new(),
        },
        Family::Ddr5 => Target::Ddr5 {
            controller: [
                u64::from_le_bytes(*b"PIIX4\0\0\0"),
                0xb00,
                0x12340000790b1022,
            ],
            port: 0,
            address: 0x50,
            module_path: String::new(),
        },
        _ => return Draft::new(family),
    };
    Draft::from_source(&Source {
        id: "selected sensor".into(),
        revision: 1,
        target,
    })
}

#[test]
fn other_families_and_relative_paths_are_rejected_without_mutation() {
    for family in Family::ALL {
        let original = draft(family);
        let before = original.clone();
        assert!(select(&original, Path::new("relative-module.bin")).is_err());
        assert_eq!(original, before);
        if matches!(family, Family::Nvidia | Family::Liquid) {
            assert!(
                select(&original, Path::new(env!("CARGO_MANIFEST_DIR")))
                    .unwrap_err()
                    .contains("только для AMD или DDR5")
            );
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "pc-control-module-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, bytes).unwrap();
            path
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            // Only direct files created by this test; never recursively remove a directory.
            for entry in fs::read_dir(&self.0).unwrap() {
                fs::remove_file(entry.unwrap().path()).unwrap();
            }
            fs::remove_dir(&self.0).unwrap();
        }
    }
    fn fixture(family: Family) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(match family {
            Family::Amd => "work/amd-cpu-source/AMDFamily17.bin",
            Family::Ddr5 => "work/ddr5-source/lhm-SmbusPIIX4.bin",
            _ => unreachable!(),
        })
    }
    #[test]
    fn non_utf8_selected_path_is_rejected_without_lossy_conversion() {
        use std::os::windows::ffi::OsStringExt;
        let path = PathBuf::from(std::ffi::OsString::from_wide(&[
            b'C' as u16,
            b':' as u16,
            b'\\' as u16,
            0xd800,
        ]));
        for family in [Family::Amd, Family::Ddr5] {
            let d = draft(family);
            let before = d.clone();
            assert!(select(&d, &path).unwrap_err().contains("UTF-8"));
            assert_eq!(d, before);
        }
    }
    #[test]
    fn complete_target_is_validated_before_selected_file_is_accessed() {
        let missing =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("work/nonexistent-selected-module.bin");
        for family in [Family::Amd, Family::Ddr5] {
            let mut d = draft(family);
            d.values[0] = "wrong identity".into();
            let before = d.clone();
            let mut expected = d.clone();
            expected.values[if family == Family::Amd { 3 } else { 5 }] =
                missing.to_str().unwrap().into();
            assert_eq!(
                select(&d, &missing).unwrap_err(),
                expected.target().unwrap_err()
            );
            assert_eq!(d, before);
        }
    }
    #[test]
    fn aliases_missing_files_directories_and_unpinned_bytes_never_edit_original() {
        let temp = Temp::new();
        let malformed = temp.file("malformed.bin", b"not a module");
        let exact_size = temp.file("same-size-wrong-hash.bin", &vec![0; amd::MODULE_BYTES]);
        let too_large = temp.file(
            "oversized.bin",
            &vec![0; crate::transport::pawnio::MAX_MODULE_BYTES + 1],
        );
        for family in [Family::Amd, Family::Ddr5] {
            let d = draft(family);
            let before = d.clone();
            for path in [
                &temp.0,
                &malformed,
                &exact_size,
                &too_large,
                &temp.0.join("missing.bin"),
                &temp.0.join("..\\alias.bin"),
                &temp.0.join("NUL.bin"),
            ] {
                assert!(select(&d, path).is_err(), "{family:?} {path:?}");
                assert_eq!(d, before);
            }
        }
    }
    #[test]
    fn local_pinned_fixtures_are_accepted_and_only_exact_path_slot_changes() {
        let temp = Temp::new();
        for family in [Family::Amd, Family::Ddr5] {
            let source = fixture(family);
            // Saved source is optional in fresh checkouts, as in provider tests.
            if !source.is_file() {
                continue;
            }
            let bytes = match family {
                Family::Amd => amd::Module::read(&source).unwrap().bytes().to_vec(),
                Family::Ddr5 => Piix4Module::read(&source).unwrap().bytes().to_vec(),
                _ => unreachable!(),
            };
            assert!(bytes.len() <= crate::transport::pawnio::MAX_MODULE_BYTES);
            let path = temp.file(
                if family == Family::Amd {
                    "AMD выбранный.bin"
                } else {
                    "DDR5 выбранный.bin"
                },
                &bytes,
            );
            let d = draft(family);
            let before = d.clone();
            let edited = select(&d, &path).unwrap();
            let mut expected = before.clone();
            expected.values[if family == Family::Amd { 3 } else { 5 }] =
                path.to_str().unwrap().into();
            assert_eq!(edited, expected);
            assert_eq!(d, before);
            edited.target().unwrap();
            let mut corrupted = bytes;
            corrupted[0] ^= 1;
            let invalid = temp.file(
                if family == Family::Amd {
                    "amd-corrupt.bin"
                } else {
                    "ddr5-corrupt.bin"
                },
                &corrupted,
            );
            assert!(select(&d, &invalid).is_err());
            assert_eq!(d, before);
        }
    }
    #[test]
    fn symlinks_to_pinned_files_are_rejected_if_test_account_can_create_them() {
        let temp = Temp::new();
        for family in [Family::Amd, Family::Ddr5] {
            let source = fixture(family);
            if !source.is_file() {
                continue;
            }
            let path = temp.0.join(if family == Family::Amd {
                "amd-link.bin"
            } else {
                "ddr5-link.bin"
            });
            if let Err(e) = std::os::windows::fs::symlink_file(&source, &path) {
                if e.raw_os_error() == Some(1314)
                    || e.kind() == std::io::ErrorKind::PermissionDenied
                {
                    continue;
                }
                panic!("create test symlink: {e}");
            }
            let d = draft(family);
            let before = d.clone();
            assert!(select(&d, &path).is_err());
            assert_eq!(d, before);
        }
    }
}
