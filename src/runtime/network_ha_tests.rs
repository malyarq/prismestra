//! Own loopback-only fixtures. No credential-store or physical-light writes.
use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

fn start_peer(
    bodies: Vec<(&'static str, &'static str)>,
) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/ha", listener.local_addr().unwrap());
    let task = thread::spawn(move || {
        let mut requests = vec![];
        for (status, body) in bodies {
            let end = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < end =>
                    {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(e) => panic!("fixture accept: {e}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut data = vec![];
            let mut buffer = [0; 4096];
            loop {
                let n = stream.read(&mut buffer).unwrap();
                assert_ne!(n, 0);
                data.extend_from_slice(&buffer[..n]);
                assert!(data.len() < 32768);
                if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&data[..end]);
                    let len = header
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                        .map_or(0, |(_, v)| v.trim().parse::<usize>().unwrap());
                    if data.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(data).unwrap());
            write!(stream,"HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
        requests
    });
    (url, task)
}
#[test]
fn ha_deduplicates_limits_rate_and_posts_off_only_when_due() {
    let (url, peer) = start_peer(vec![
        ("200 OK", r#"{"entity_id":"light.room"}"#),
        ("200 OK", "[]"),
        ("200 OK", "[]"),
    ]);
    let cancel = Cancellation::default();
    let mut driver =
        HomeAssistant::with_token(&url, "light.room", "fake.test-token".into(), &cancel).unwrap();
    let color = [Rgb { r: 90, g: 30, b: 0 }];
    let black = [Rgb::default()];
    assert!(driver.write(&[], &cancel).is_err());
    assert!(matches!(
        driver.write(&color, &cancel).unwrap(),
        WriteResult::Submitted
    ));
    assert!(matches!(
        driver.write(&color, &cancel).unwrap(),
        WriteResult::Unchanged
    ));
    assert!(matches!(
        driver.write(&black, &cancel).unwrap(),
        WriteResult::Deferred(_)
    ));
    driver.next = Some(Instant::now());
    assert!(matches!(
        driver.write(&black, &cancel).unwrap(),
        WriteResult::Submitted
    ));
    assert!(matches!(
        driver.write(&black, &cancel).unwrap(),
        WriteResult::Unchanged
    ));
    cancel.cancel_for_test();
    assert!(driver.write(&black, &cancel).is_err());
    let requests = peer.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("GET /ha/api/states/light.room "));
    assert!(requests[1].starts_with("POST /ha/api/services/light/turn_on "));
    let body: serde_json::Value =
        serde_json::from_str(requests[1].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(
        body,
        serde_json::json!({"entity_id":"light.room","brightness":90,"rgb_color":[255,85,0]})
    );
    assert!(requests[2].starts_with("POST /ha/api/services/light/turn_off "));
}
#[test]
fn ha_discovery_is_read_only_and_errors_do_not_expose_credentials() {
    use crate::app::lighting_bindings::discovery::{self, Target};
    let (url, peer) = start_peer(vec![(
        "200 OK",
        r#"[{"entity_id":"light.room","attributes":{"friendly_name":"Room","supported_color_modes":["rgb"]}},{"entity_id":"sensor.temp"}]"#,
    )]);
    let d = crate::devices::Device {
        id: "ha".into(),
        label: "Light".into(),
        driver: Driver::HomeAssistant,
        role: crate::devices::Role::Canvas,
        leds: 1,
        rate_hz: 1,
        enabled: true,
    };
    let factory = discovery::home_assistant_factory(url.clone(), "fake.test-token".into()).unwrap();
    let inventory = factory(Target::Device(d), Cancellation::default()).unwrap();
    assert_eq!(inventory.candidates.len(), 1);
    let draft = &inventory.candidates[0].draft;
    assert_eq!(draft.identity, "light.room");
    assert_eq!(
        draft.serial,
        crate::devices::home_assistant::credential_id(&url).unwrap()
    );
    assert!(!format!("{inventory:?}").contains("fake.test-token"));
    let requests = peer.join().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET /ha/api/states "));
    let (url, peer) = start_peer(vec![("401 Unauthorized", "secret response")]);
    let error = HomeAssistant::with_token(
        &url,
        "light.room",
        "fake.test-token".into(),
        &Cancellation::default(),
    )
    .err()
    .unwrap();
    assert!(!error.contains("fake.test-token") && !error.contains("secret response"));
    peer.join().unwrap();
}
