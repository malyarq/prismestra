#![cfg(windows)]
use pc_control::app::{
    sensor_bindings::{Catalog, Source, Target},
    sensor_form::{Draft, Family},
    sensor_sessions::{NativeResolver, Resolver},
};
use pc_control::runtime::output::Cancellation;
fn target() -> Target {
    Target::FanControl {
        pipe_name: r"\\.\pipe\Prismestra-stand-no-server".into(),
        expected_server: r"C:\Prismestra-stand-missing\FanControl.exe".into(),
        sensor_id: "/cpu/package".into(),
    }
}
#[test]
fn local_exact_cached_sensor_identity_is_scoped_and_preferences_roundtrip_without_io() {
    let t = target();
    t.validate().unwrap();
    let mut alias = t.clone();
    if let Target::FanControl {
        pipe_name,
        expected_server,
        ..
    } = &mut alias
    {
        *pipe_name = pipe_name.to_ascii_lowercase();
        *expected_server = expected_server.to_ascii_uppercase().replace('\\', "/");
    }
    assert_eq!(t.canonical_key().unwrap(), alias.canonical_key().unwrap());
    assert_eq!(t.resource_keys().unwrap(), alias.resource_keys().unwrap());
    let source = Source {
        id: "cached-cpu".into(),
        revision: 1,
        target: t.clone(),
    };
    let draft = Draft::from_source(&source);
    assert_eq!(draft.family, Family::FanControl);
    assert_eq!(draft.target().unwrap(), t);
    let catalog = Catalog {
        version: 1,
        sources: vec![source],
        routes: vec![],
    };
    catalog.validate().unwrap();
    let serialized = serde_json::to_string(&catalog).unwrap();
    let decoded: Catalog = serde_json::from_str(&serialized).unwrap();
    assert_eq!(decoded, catalog);
    for (pipe, server, id) in [
        (r"\\remote\pipe\x", r"C:\FanControl.exe", "cpu"),
        (r"\\.\pipe\x", r"..\FanControl.exe", "cpu"),
        (r"\\.\pipe\x", r"C:\FanControl.exe", ""),
    ] {
        assert!(
            Target::FanControl {
                pipe_name: pipe.into(),
                expected_server: server.into(),
                sensor_id: id.into()
            }
            .validate()
            .is_err()
        );
    }
    let mut other = t.clone();
    if let Target::FanControl { sensor_id, .. } = &mut other {
        *sensor_id = "/gpu/core".into();
    }
    assert_ne!(t.resource_keys().unwrap(), other.resource_keys().unwrap());
}
#[test]
fn native_resolve_and_provider_creation_do_not_open_pipe_or_require_a_running_server() {
    let mut resolver = NativeResolver::explicit_user_start();
    let factory = resolver.resolve(&target()).unwrap();
    let provider = factory(Cancellation::default()).unwrap();
    assert_eq!(provider.poll_interval_ms(), 2000);
}
