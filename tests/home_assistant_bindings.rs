#![cfg(windows)]
use pc_control::{
    app::{device_setup, lighting_bindings::Draft},
    devices::{Binding, Bindings, Canvas, Point, Setup, home_assistant, validate_bindings},
};
#[test]
fn whole_light_preset_local_credentials_and_endpoint_ownership_are_validated() {
    let preset = device_setup::catalog()
        .iter()
        .find(|p| p.id == "home-assistant-light")
        .unwrap();
    let device = preset.device("office").unwrap();
    assert_eq!((device.leds, device.rate_hz), (1, 1));
    assert!(preset.device_with_leds("office", 2).is_err());
    let mut invalid = device.clone();
    invalid.rate_hz = 2;
    assert!(invalid.validate().is_err());
    let endpoint = "http://HA.example.test:80/ha/";
    let draft = Draft {
        endpoint: endpoint.into(),
        identity: "light.office".into(),
        serial: home_assistant::credential_id(endpoint).unwrap(),
    };
    let binding = draft.binding(&device).unwrap();
    assert_eq!(
        Draft::from_binding(&binding).endpoint,
        "http://ha.example.test/ha"
    );
    let mut wrong = draft.clone();
    wrong.serial = home_assistant::credential_id("http://other.example.test").unwrap();
    assert!(wrong.binding(&device).is_err());
    let mut other = device.clone();
    other.id = "other".into();
    let setup = Setup {
        version: 1,
        devices: vec![device, other],
        canvas: Canvas {
            width: 2,
            height: 1,
            points: vec![
                Point {
                    device: "office".into(),
                    led: 0,
                    x: 0,
                    y: 0,
                },
                Point {
                    device: "other".into(),
                    led: 0,
                    x: 1,
                    y: 0,
                },
            ],
        },
    };
    let mut bindings: Bindings = [
        ("office".into(), binding.clone()),
        ("other".into(), binding),
    ]
    .into();
    assert!(validate_bindings(&setup, &bindings).is_err());
    if let Binding::HomeAssistant { entity, .. } = bindings.get_mut("other").unwrap() {
        *entity = "light.other".into();
    }
    assert!(validate_bindings(&setup, &bindings).is_ok());
}
