use super::*;
use serde_json::json;
#[test]
fn legacy_catalog_keeps_sources_and_lighting_routes_without_cooling_consumers() {
    let original = catalog();
    let mut legacy = serde_json::to_value(&original).unwrap();
    legacy["routes"].as_array_mut().unwrap().push(json!({
        "scope": {"kind":"cooling", "identity":"old-profile", "digest":"a".repeat(64)},
        "input":"old-input", "source":"physical/one"
    }));
    let bytes = serde_json::to_vec(&legacy).unwrap();
    let migrated = Catalog::decode(&bytes).unwrap();
    assert_eq!(migrated, original);
    assert_eq!(migrated.sources[0].revision, 9);
    assert_eq!(
        Catalog::decode(&migrated.encode().unwrap()).unwrap(),
        original
    );
    legacy["routes"][1]["scope"]["unexpected"] = true.into();
    assert!(Catalog::decode(&serde_json::to_vec(&legacy).unwrap()).is_err());
    assert!(Catalog::decode(br#"{"version":1,"sources":[],"routes":[{"scope":{"kind":"cooling","identity":"a","identity":"b","digest":"x"}}]}"#).is_err());
}
fn nvidia() -> Target {
    Target::Nvidia {
        pci: Pci {
            device_id: 0x280310de,
            subsystem_id: 0x12345678,
            revision_id: 0xa1,
            extended_device_id: 0x77,
        },
    }
}
fn amd(path: &str) -> Target {
    Target::Amd {
        vendor: *b"AuthenticAMD",
        signature: 0x00800f12,
        packages: 1,
        module_path: path.into(),
    }
}
fn ddr5(path: &str) -> Target {
    Target::Ddr5 {
        controller: [
            u64::from_le_bytes(*b"PIIX4\0\0\0"),
            0xb00,
            0x11223344790b1022,
        ],
        port: 0,
        address: 0x50,
        module_path: path.into(),
    }
}
fn liquid() -> Target {
    Target::Liquid {
        explicit_mi00_path: r"\\?\hid#vid_264a&pid_233c&mi_00#explicit-instance#{guid}".into(),
    }
}
fn document() -> Document {
    use crate::{
        app::config::{Groups, Project},
        devices::{Canvas, Setup},
        runtime::scene,
        schedule,
    };
    let mut result = Document::from_lighting(Project {
        version: 1,
        setup: Setup {
            version: 1,
            devices: vec![],
            canvas: Canvas {
                width: 1,
                height: 1,
                points: vec![],
            },
        },
        scene: scene::example(),
        schedule: schedule::Config::default(),
        groups: Groups {
            pc: vec![],
            door: vec![],
        },
    })
    .unwrap();
    result
        .temperatures
        .sources
        .push(super::super::sensors::Source {
            key: "cpu_view".into(),
            value: super::super::sensors::Value::Sensor {
                input: "cpu".into(),
            },
        });
    result.validate().unwrap();
    result
}

fn scope() -> Scope {
    Scope::lighting("document-A", &document()).unwrap()
}
fn catalog() -> Catalog {
    let scope = scope();
    Catalog {
        version: 1,
        sources: vec![Source {
            id: "physical/one".into(),
            revision: 9,
            target: nvidia(),
        }],
        routes: vec![Route {
            scope,
            input: "cpu".into(),
            source: "physical/one".into(),
        }],
    }
}
#[test]
fn preference_edits_preserve_other_scopes_and_round_trip_without_authority() {
    let mut c = catalog();
    let other = Scope::lighting("other", &document()).unwrap();
    c.set_routes(&other, &[("gpu".into(), "physical/one".into())])
        .unwrap();
    c.set_source("second", amd(r"C:\drivers\amd.dll")).unwrap();
    c.set_routes(&scope(), &[("cpu".into(), "second".into())])
        .unwrap();
    assert_eq!(
        c.prepare(&other, &["gpu".into()]).unwrap()[0].source_id,
        "physical/one"
    );
    assert_eq!(
        c.prepare(&scope(), &["cpu".into()]).unwrap()[0].source_id,
        "second"
    );
    assert_eq!(Catalog::decode(&c.encode().unwrap()).unwrap(), c);
    assert!(c.remove_source("second").is_err());
    c.set_routes(&scope(), &[]).unwrap();
    c.remove_source("second").unwrap();
    assert_eq!(c.routes.len(), 1);
}
#[test]
fn failed_scope_edits_are_atomic_and_do_not_rebind_stale_scopes() {
    let mut c = catalog();
    let original = c.clone();
    for routes in [
        vec![("cpu".into(), "missing".into())],
        vec![
            ("cpu".into(), "physical/one".into()),
            ("cpu".into(), "physical/one".into()),
        ],
    ] {
        assert!(c.set_routes(&scope(), &routes).is_err());
        assert_eq!(c, original);
    }
    let mut changed = document();
    changed.temperatures.sources[0].key = "different".into();
    let stale = Scope::lighting("document-A", &changed).unwrap();
    assert!(c.prepare(&stale, &["cpu".into()]).is_err());
    assert_eq!(c, original);
}
#[test]
fn source_replacement_increments_lineage_and_rejects_conflicts_atomically() {
    let mut c = catalog();
    c.set_source("second", amd(r"C:\drivers\amd.dll")).unwrap();
    c.set_routes(
        &scope(),
        &[
            ("cpu".into(), "physical/one".into()),
            ("ram".into(), "second".into()),
        ],
    )
    .unwrap();
    let mut alternate = nvidia();
    if let Target::Nvidia { pci } = &mut alternate {
        pci.extended_device_id += 1;
    }
    c.set_source("physical/one", alternate.clone()).unwrap();
    assert_eq!(c.sources[0].revision, 10);
    c.set_source("physical/one", alternate).unwrap();
    assert_eq!(c.sources[0].revision, 10);
    c.set_source("physical/one", amd(r"C:\drivers\amd.dll"))
        .unwrap();
    assert_eq!(c.sources[0].revision, 11);
    let original = c.clone();
    assert!(
        c.set_source("second", amd(r"C:\drivers\different.dll"))
            .is_err()
    );
    assert_eq!(c, original);
    c.sources[0].revision = u64::MAX;
    let original = c.clone();
    assert!(c.set_source("physical/one", nvidia()).is_err());
    assert_eq!(c, original);
}
#[test]
fn sensor_bindings_four_descriptors_match_existing_pure_bindings_and_exact_endpoint_keys() {
    let nv = nvidia();
    assert_eq!(
        nv.resource_keys().unwrap(),
        ["sensor:nvapi:280310de:12345678:000000a1:00000077"]
    );
    assert_eq!(nv.nvidia_binding().unwrap().device_id, 0x280310de);
    let amd = amd("Q:/never-read/pinned.sys");
    assert_eq!(amd.resource_keys().unwrap(), ["sensor:amd-smn:primary"]);
    assert_eq!(amd.amd_binding().unwrap().display_family(), 0x17);
    let ram = ddr5("Q:/never-read/pinned.sys");
    assert_eq!(
        ram.resource_keys().unwrap(),
        ["sensor:ddr5:0000003458494950:0000000000000b00:11223344790b1022:0:50"]
    );
    assert_eq!(ram.ddr5_binding().unwrap().address, 0x50);
    let liquid = liquid();
    assert_eq!(
        liquid.resource_keys().unwrap(),
        [r"hid:\\?\hid#vid_264a&pid_233c&mi_00#explicit-instance#{guid}"]
    );
    for target in [nv, amd, ram, liquid] {
        let key = target.canonical_key().unwrap();
        assert_eq!(key.len(), 64);
        assert!(
            key.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
    }
}
#[test]
fn sensor_bindings_module_paths_are_lexical_local_only_and_validation_performs_no_source_io() {
    for path in ["Q:/does-not-exist/pinned.sys", r"C:\Modules\driver.dll"] {
        amd(path).validate().unwrap();
        ddr5(path).validate().unwrap();
    }
    for path in [
        "relative.sys",
        "C:relative.sys",
        r"\\server\share\module.sys",
        r"\\?\C:\module.sys",
        "C:/a:stream",
        "C:/a/../m.sys",
        "C:/a./m.sys",
        "C:/a /m.sys",
        "C:/NUL.sys",
        "C:/COM1.dll",
        "C:/LPT¹.sys",
        "C:/a//m.sys",
        "C:/a/",
        "C:/a/*.sys",
        "C:/a/|m.sys",
        "C:/a\0m.sys",
    ] {
        assert!(amd(path).validate().is_err(), "{path:?}");
    }
    let mut target = nvidia();
    if let Target::Nvidia { pci } = &mut target {
        pci.device_id = 0;
    }
    assert!(target.validate().is_err());
    let mut target = amd("C:/module.sys");
    if let Target::Amd { packages, .. } = &mut target {
        *packages = 2;
    }
    assert!(target.validate().is_err());
    let mut target = ddr5("C:/module.sys");
    if let Target::Ddr5 { address, .. } = &mut target {
        *address = 0x58;
    }
    assert!(target.validate().is_err());
    for path in [
        r"\\?\hid#vid_264a&pid_233c&mi_01#x#guid",
        r"\\?\hid#vid_0000&pid_233c&mi_00#x#guid",
        "C:/fake-mi00",
    ] {
        assert!(
            Target::Liquid {
                explicit_mi00_path: path.into()
            }
            .validate()
            .is_err()
        );
    }
}
#[test]
fn sensor_bindings_canonical_descriptor_normalizes_case_slashes_but_never_merges_different_modules()
{
    let a = amd("C:/Modules/PIN.sys");
    let alias = amd(r"c:\modules\pin.SYS");
    let other = amd("C:/elsewhere/PIN.sys");
    assert_eq!(a.canonical_key().unwrap(), alias.canonical_key().unwrap());
    assert_eq!(a.resource_keys().unwrap(), other.resource_keys().unwrap());
    assert_ne!(a.canonical_key().unwrap(), other.canonical_key().unwrap());
    let mut upper = liquid();
    if let Target::Liquid { explicit_mi00_path } = &mut upper {
        *explicit_mi00_path = explicit_mi00_path.to_ascii_uppercase();
    }
    assert_eq!(
        upper.canonical_key().unwrap(),
        liquid().canonical_key().unwrap()
    );
    let mut catalog = catalog();
    let scope = scope();
    catalog.sources[0].target = a;
    catalog.sources.push(Source {
        id: "second".into(),
        revision: 1,
        target: other,
    });
    catalog.routes.push(Route {
        scope: scope.clone(),
        input: "other".into(),
        source: "second".into(),
    });
    catalog.validate().unwrap();
    assert!(
        catalog
            .prepare(&scope, &["cpu".into(), "other".into()])
            .is_err()
    );
    catalog.sources[1].target = alias;
    assert_eq!(
        catalog
            .prepare(&scope, &["cpu".into(), "other".into()])
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn sensor_bindings_scope_lighting_changes_only_with_temperature_config_or_identity() {
    let base = document();
    let original = Scope::lighting("A", &base).unwrap();
    let mut scene = base.clone();
    scene.lighting.scene.colors = vec![0x123456];
    scene.lighting.scene.brightness = 77;
    assert_eq!(Scope::lighting("A", &scene).unwrap(), original);
    assert_ne!(Scope::lighting("B", &base).unwrap(), original);
    scene.temperatures.sources[0].value = super::super::sensors::Value::Sensor {
        input: "other".into(),
    };
    assert_ne!(Scope::lighting("A", &scene).unwrap(), original);
    assert!(Scope::lighting("", &document()).is_err());
    assert!(
        Scope::Lighting {
            identity: "A".into(),
            digest: "x".repeat(64)
        }
        .validate()
        .is_err()
    );
}
#[test]
fn sensor_bindings_prepare_requires_exact_scope_and_complete_input_set_preserves_request_order_and_revision()
 {
    let mut catalog = catalog();
    let scope = scope();
    catalog.routes.push(Route {
        scope: scope.clone(),
        input: "alias".into(),
        source: "physical/one".into(),
    });
    let result = catalog
        .prepare(&scope, &["alias".into(), "cpu".into()])
        .unwrap();
    assert_eq!(result[0].input, "alias");
    assert_eq!(result[1].input, "cpu");
    assert_eq!(result[0].source_id, "physical/one");
    assert_eq!(result[0].source_revision, 9);
    assert_eq!(result[0].target, result[1].target);
    assert!(catalog.prepare(&scope, &["cpu".into()]).is_err());
    assert!(
        catalog
            .prepare(&scope, &["cpu".into(), "cpu".into()])
            .is_err()
    );
    assert!(catalog.prepare(&scope, &[]).is_err());
    let stale = Scope::Lighting {
        identity: "document-A".into(),
        digest: "0".repeat(64),
    };
    assert!(catalog.prepare(&stale, &["cpu".into()]).is_err());
    let foreign = Scope::Lighting {
        identity: "document-A".into(),
        digest: match &scope {
            Scope::Lighting { digest, .. } => digest.clone(),
        },
    };
    assert!(catalog.prepare(&foreign, &["cpu".into()]).is_err());
    assert!(Catalog::default().prepare(&scope, &[]).unwrap().is_empty());
    let other = Scope::lighting("unused-saved-scope", &document()).unwrap();
    catalog.routes.push(Route {
        scope: other,
        input: "unused".into(),
        source: "physical/one".into(),
    });
    assert!(
        catalog
            .prepare(&scope, &["cpu".into(), "alias".into()])
            .is_ok()
    );
}
#[test]
fn sensor_bindings_strict_unique_json_schema_versions_unknown_sources_and_duplicate_routes() {
    let catalog = catalog();
    let encoded = catalog.encode().unwrap();
    assert_eq!(Catalog::decode(&encoded).unwrap(), catalog);
    let text = String::from_utf8(encoded.clone()).unwrap();
    let duplicate = text.replacen("\"version\":1", "\"version\":1,\"version\":1", 1);
    assert!(Catalog::decode(duplicate.as_bytes()).is_err());
    for modify in [0, 1, 2, 3, 4, 5, 6] {
        let mut value = serde_json::to_value(&catalog).unwrap();
        match modify {
            0 => value["version"] = json!(0),
            1 => value["version"] = json!(2),
            2 => value["native_grant"] = json!(true),
            3 => value["sources"][0]["target"]["native"] = json!(true),
            4 => value["routes"][0]["scope"]["extra"] = json!(true),
            5 => value["sources"][0]["revision"] = json!(0),
            _ => value["routes"][0]["source"] = json!("missing"),
        };
        assert!(Catalog::decode(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    let mut duplicate = catalog.clone();
    duplicate.sources.push(duplicate.sources[0].clone());
    assert!(duplicate.validate().is_err());
    let mut duplicate = catalog.clone();
    duplicate.routes.push(duplicate.routes[0].clone());
    assert!(duplicate.validate().is_err());
    assert!(Catalog::decode(&vec![b' '; MAX_BYTES + 1]).is_err());
    assert!(Catalog::decode(b"{\"version\":1,\"sources\":[],\"routes\":[]} trailing").is_err());
}
#[test]
fn sensor_bindings_count_byte_and_identity_caps_allow_many_aliases_without_extra_targets() {
    let scope = scope();
    let other = Scope::lighting("other", &document()).unwrap();
    let mut catalog = catalog();
    catalog.sources = (0..256)
        .map(|index| Source {
            id: format!("source-{index}"),
            revision: 1,
            target: nvidia(),
        })
        .collect();
    catalog.routes = [scope.clone(), other]
        .into_iter()
        .flat_map(|scope| {
            (0..256).map(move |index| Route {
                scope: scope.clone(),
                input: format!("input_{index}"),
                source: "source-0".into(),
            })
        })
        .collect();
    catalog.validate().unwrap();
    let inputs = (0..256)
        .map(|index| format!("input_{index}"))
        .collect::<Vec<_>>();
    assert_eq!(catalog.prepare(&scope, &inputs).unwrap().len(), 256);
    let mut too_many = catalog.clone();
    too_many.sources.push(Source {
        id: "overflow".into(),
        revision: 1,
        target: nvidia(),
    });
    assert!(too_many.validate().is_err());
    let mut too_many = catalog.clone();
    too_many.routes.push(Route {
        scope: scope.clone(),
        input: "extra".into(),
        source: "source-0".into(),
    });
    assert!(too_many.validate().is_err());
    let mut large = catalog;
    for source in &mut large.sources {
        source.id = format!("{}{}", source.id, "x".repeat(2000));
    }
    large.routes.clear();
    assert!(large.validate().is_err());
    let mut bad = Source {
        id: "a\n".into(),
        revision: 1,
        target: nvidia(),
    };
    let mut catalog = Catalog::default();
    catalog.sources.push(bad.clone());
    assert!(catalog.validate().is_err());
    bad.id = "x".repeat(2049);
    catalog.sources = vec![bad];
    assert!(catalog.validate().is_err());
}
#[test]
fn sensor_bindings_explicit_own_preference_file_roundtrip_does_not_open_module_targets() {
    let catalog = Catalog {
        version: 1,
        sources: vec![Source {
            id: "amd".into(),
            revision: 1,
            target: amd("Q:/never-opened/pinned.sys"),
        }],
        routes: vec![],
    };
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("work")
        .join(format!(
            "sensor-catalog-own-test-{}.json",
            std::process::id()
        ));
    assert!(!path.exists());
    catalog.save(&path).unwrap();
    let loaded = Catalog::load(&path);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(loaded.unwrap(), catalog);
}
