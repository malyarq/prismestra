use super::*;

fn fixtures() -> [Target; 4] {
    [
        Target::Nvidia {
            pci: Pci {
                device_id: 0x280310de,
                subsystem_id: 0x12345678,
                revision_id: 0xa1,
                extended_device_id: u32::MAX,
            },
        },
        Target::Amd {
            vendor: *b"AuthenticAMD",
            signature: 0x00800f12,
            packages: 1,
            module_path: r"C:\Synthetic\AMD.dll".into(),
        },
        Target::Ddr5 {
            controller: [
                u64::from_le_bytes(*b"PIIX4\0\0\0"),
                0xb00,
                0x11223344790b1022,
            ],
            port: 0,
            address: 0x50,
            module_path: r"C:\Synthetic\DDR5.dll".into(),
        },
        Target::Liquid {
            explicit_mi00_path: r"\\?\hid#vid_264a&pid_233c&mi_00#Synthetic-instance#{guid}".into(),
        },
    ]
}
fn draft(index: usize) -> Draft {
    Draft::from_source(&Source {
        id: "synthetic/source".into(),
        revision: 7,
        target: fixtures()[index].clone(),
    })
}

#[test]
fn all_families_round_trip_without_discovery_or_module_reads() {
    for (index, target) in fixtures().into_iter().enumerate() {
        let form = draft(index);
        assert_eq!(form.family, Family::ALL[index]);
        assert_eq!(form.id, "synthetic/source");
        assert_eq!(form.target().unwrap(), target);
        assert!(!form.family.label().is_empty());
    }
    // Paths are intentionally synthetic; target() must not check file/device existence.
    for family in Family::ALL {
        let form = Draft::new(family);
        assert!(form.id.is_empty());
        assert!(form.values.iter().all(String::is_empty));
        assert!(form.target().is_err());
    }
}

#[test]
fn numeric_syntax_and_boundaries() {
    let mut form = draft(0);
    form.values[0] = " 0X280310de ".into();
    form.values[1] = "ffffffff".into();
    assert!(form.target().is_ok());
    for malformed in [
        "",
        "0x",
        "+1",
        "-1",
        "1_0",
        "ff gg",
        "１０",
        "100000000",
        "0x100000000",
        "0x+1",
    ] {
        form.values[1] = malformed.into();
        assert!(form.target().is_err(), "accepted {malformed:?}");
    }
    let mut ddr = draft(2);
    ddr.values[2] = "ffffffff790b1022".into();
    assert!(ddr.target().is_ok());
    ddr.values[2] = "10000000000000000".into();
    assert!(ddr.target().is_err());
    for malformed in ["256", "+0", "0x0", "-0", "", "1.0"] {
        let mut ddr = draft(2);
        ddr.values[3] = malformed.into();
        assert!(ddr.target().is_err());
    }
    for malformed in ["100", "-50", "gg", ""] {
        let mut ddr = draft(2);
        ddr.values[4] = malformed.into();
        assert!(ddr.target().is_err());
    }
    assert!(number("4294967295", "test", 10, 32).is_ok());
    assert!(number("4294967296", "test", 10, 32).is_err());
}

#[test]
fn rejects_missing_mismatched_and_non_ascii_descriptors() {
    for index in 0..4 {
        let form = draft(index);
        for (slot, label) in form.labels().iter().enumerate() {
            if !label.is_empty() {
                let mut missing = form.clone();
                missing.values[slot].clear();
                assert!(missing.target().is_err(), "family {index} slot {slot}");
            }
        }
    }
    for vendor in [
        "AuthenticAM",
        "AuthenticAMD ",
        "AuthenticAMД",
        "GenuineIntel",
    ] {
        let mut form = draft(1);
        form.values[0] = vendor.into();
        assert!(form.target().is_err());
    }
    for (index, slot, value) in [
        (0, 0, "28031022"),
        (1, 1, "00000600"),
        (1, 2, "2"),
        (2, 0, "0"),
        (2, 1, "b20"),
        (2, 3, "2"),
        (2, 4, "58"),
    ] {
        let mut form = draft(index);
        form.values[slot] = value.into();
        assert!(form.target().is_err());
    }
    let mut form = draft(3);
    form.values[0] = form.values[0].replace("mi_00", "mi_01");
    assert!(form.target().is_err());
}

#[test]
fn paths_and_ids_are_bounded_and_never_silently_trimmed() {
    for (index, slot) in [(1, 3), (2, 5), (3, 0)] {
        let mut form = draft(index);
        let original = form.values[slot].clone();
        form.values[slot] = format!(" {original}");
        assert!(form.target().is_err());
        form.values[slot] = format!("{original} ");
        assert!(form.target().is_err());
        form.values[slot] = "x".repeat(if index == 3 { 4097 } else { 32761 });
        assert!(form.target().is_err());
    }
    for invalid in [
        r"relative.dll",
        r"C:\path\..\amd.dll",
        r"C:\NUL.dll",
        r"C:\path\amd.dll:stream",
        r"C:\path\*.dll",
    ] {
        let mut form = draft(1);
        form.values[3] = invalid.into();
        assert!(form.target().is_err());
    }
    let mut form = draft(0);
    form.id = "  source with spaces  ".into();
    assert!(form.target().is_ok());
    assert_eq!(form.id, "  source with spaces  ");
    for invalid in [String::new(), "x".repeat(2049), "name\n".into()] {
        form.id = invalid;
        assert!(form.target().is_err());
    }
}

#[test]
fn hidden_values_cannot_survive_a_family_change() {
    for index in [0, 1, 3] {
        let form = draft(index);
        for (slot, label) in form.labels().iter().enumerate() {
            if label.is_empty() {
                let mut stale = form.clone();
                stale.values[slot] = " ".into();
                assert!(stale.target().unwrap_err().contains("Скрытые"));
            }
        }
    }
    let mut form = draft(2);
    form.family = Family::Liquid;
    assert!(form.target().unwrap_err().contains("Скрытые"));
}
