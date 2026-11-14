use super::*;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::{self, JoinHandle};
use std::time::Duration;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(4)
}
fn response(status: &str, body: &[u8]) -> Vec<u8> {
    let mut response = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    response.extend_from_slice(body);
    response
}
fn server(responses: Vec<Vec<u8>>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/ha", listener.local_addr().unwrap());
    let thread = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in responses {
            let until = deadline();
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until =>
                    {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(e) => panic!("loopback accept: {e}"),
                }
            };
            // Accepted Windows sockets can inherit the listener's nonblocking mode.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut data = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let n = stream.read(&mut buffer).unwrap();
                assert_ne!(n, 0);
                data.extend_from_slice(&buffer[..n]);
                assert!(data.len() < 32_768);
                if let Some(end) = data.windows(4).position(|s| s == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&data[..end]);
                    let length: usize = header
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(key, _)| key.eq_ignore_ascii_case("Content-Length"))
                                .map(|(_, n)| n.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if data.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(data).unwrap());
            let _ = stream.write_all(&response);
        }
        requests
    });
    (url, thread)
}
fn client(url: &str) -> Client {
    Client::new(Endpoint::parse(url).unwrap(), "fake.test-token".to_owned()).unwrap()
}

#[test]
fn validates_urls_tokens_and_entity_names_without_io() {
    for url in [
        "ftp://host",
        "http://",
        "http://user:pass@host",
        "http://host?x",
        "http://host/#x",
        "http://host:0",
        "http://host:65536",
        "http://host:abc",
        "http://host/../x",
        "http://host/%2f",
        "http://host\\evil",
        "http://host/\r\nx",
        "http://host/é",
    ] {
        assert!(Endpoint::parse(url).is_err(), "{url:?}");
    }
    let endpoint = Endpoint::parse("https://host/base/").unwrap();
    assert_eq!(endpoint.port, 443);
    assert_eq!(Endpoint::parse("http://host").unwrap().port, 80);
    assert_eq!(
        Endpoint::parse("http://host:8123").unwrap().port,
        DEFAULT_PORT
    );
    assert_eq!(
        Endpoint::parse("https://host:8123").unwrap().port,
        DEFAULT_PORT
    );
    assert!(endpoint.secure);
    assert_eq!(endpoint.base_path, "/base");
    assert_eq!(Endpoint::parse("http://[::1]:1234").unwrap().port, 1234);
    for token in ["", "secret\r\nHeader: bad", "two words", "\0"] {
        assert!(Client::new(Endpoint::parse("http://127.0.0.1:9").unwrap(), token.into()).is_err());
    }
    for entity in [
        "light.",
        "switch.room",
        "light.a/b",
        "light.a,b",
        "light.A",
        "light.a\n",
    ] {
        assert!(!valid_entity(entity));
    }
    assert!(valid_entity("light.room_2"));
}

#[test]
fn metadata_is_read_only_and_filters_lights() {
    let body = br#"[{"entity_id":"sensor.temp","state":"19"},{"entity_id":"light.room","state":"on","attributes":{"friendly_name":"Room","supported_color_modes":["rgb"]}},{"entity_id":"light.bad/path"}]"#;
    let (url, server) = server(vec![response("200 OK", body)]);
    let lights = client(&url).lights(deadline(), || false).unwrap();
    assert_eq!(
        lights,
        vec![Light {
            entity_id: "light.room".into(),
            name: Some("Room".into()),
            state: Some("on".into()),
            supported_color_modes: vec!["rgb".into()]
        }]
    );
    let requests = server.join().unwrap();
    assert!(requests[0].starts_with("GET /ha/api/states HTTP/1.1\r\n"));
    assert!(requests[0].contains("Authorization: Bearer fake.test-token\r\n"));
}

#[test]
fn binds_once_then_posts_exact_entity_color_and_off_without_inventory() {
    let (url, server) = server(vec![
        response("200 OK", br#"{"entity_id":"light.room","state":"on","attributes":{"supported_color_modes":["rgb"]}}"#),
        response("200 OK", b"[]"),
        response("200 OK", b"[]"),
    ]);
    let mut client = client(&url);
    assert_eq!(
        client.set_color([1, 2, 3], deadline(), || false),
        Err(Error::InvalidInput)
    );
    client
        .bind_light("light.room", deadline(), || false)
        .unwrap();
    client.set_color([64, 32, 0], deadline(), || false).unwrap();
    client.set_color([0, 0, 0], deadline(), || false).unwrap();
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("GET /ha/api/states/light.room HTTP/1.1"));
    assert!(requests[1].starts_with("POST /ha/api/services/light/turn_on HTTP/1.1"));
    let body: Value = serde_json::from_str(requests[1].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(
        body,
        json!({"entity_id":"light.room", "rgb_color":[255,128,0], "brightness":64})
    );
    assert!(requests[2].starts_with("POST /ha/api/services/light/turn_off HTTP/1.1"));
    assert_eq!(
        serde_json::from_str::<Value>(requests[2].split_once("\r\n\r\n").unwrap().1).unwrap(),
        json!({"entity_id":"light.room"})
    );
}

#[test]
fn binding_rejects_unavailable_and_unknown_capabilities_without_posting() {
    for (state, modes, expected) in [
        ("unavailable", json!(["rgb"]), Error::EntityUnavailable),
        ("unknown", json!(["rgb"]), Error::EntityUnavailable),
        ("on", json!(null), Error::UnsupportedCapabilities),
        ("on", json!(["rgb", 42]), Error::UnsupportedCapabilities),
        ("on", json!(["future"]), Error::UnsupportedCapabilities),
        ("bad-state", json!(["rgb"]), Error::InvalidResponse),
    ] {
        let body = serde_json::to_vec(&json!({"entity_id":"light.room", "state":state, "attributes":{"supported_color_modes":modes}})).unwrap();
        let (url, server) = server(vec![response("200 OK", &body)]);
        let mut client = client(&url);
        assert_eq!(
            client.bind_light("light.room", deadline(), || false),
            Err(expected)
        );
        assert_eq!(
            client.set_color([1, 2, 3], deadline(), || false),
            Err(Error::InvalidInput)
        );
        assert!(client.entity.is_none() && client.capability.is_none());
        assert_eq!(server.join().unwrap().len(), 1);
    }
}

#[test]
fn output_uses_bound_capabilities_for_all_supported_modes() {
    for mode in [
        "rgb",
        "hs",
        "xy",
        "rgbw",
        "rgbww",
        "brightness",
        "color_temp",
        "onoff",
    ] {
        let state = serde_json::to_vec(&json!({"entity_id":"light.room", "state":"off", "attributes":{"supported_color_modes":[mode]}})).unwrap();
        let (url, server) = server(vec![response("200 OK", &state), response("200 OK", b"[]")]);
        let mut client = client(&url);
        client
            .bind_light("light.room", deadline(), || false)
            .unwrap();
        client.set_color([64, 32, 0], deadline(), || false).unwrap();
        let requests = server.join().unwrap();
        let body: Value =
            serde_json::from_str(requests[1].split_once("\r\n\r\n").unwrap().1).unwrap();
        let mut expected = json!({"entity_id":"light.room"});
        if mode != "onoff" {
            expected["brightness"] = json!(64);
        }
        if matches!(mode, "rgb" | "hs" | "xy" | "rgbw" | "rgbww") {
            expected["rgb_color"] = json!([255, 128, 0]);
        }
        assert_eq!(body, expected, "{mode}");
    }
}

#[test]
fn authentication_missing_entity_and_http_failures_are_distinct_and_redacted() {
    for (status, expected) in [
        ("401 Unauthorized", Error::Authentication),
        ("404 Not Found", Error::MissingEntity),
        ("503 Unavailable", Error::Http(503)),
    ] {
        let (url, server) = server(vec![response(status, b"fake.test-token")]);
        let mut client = client(&url);
        let error = client
            .bind_light("light.room", deadline(), || false)
            .unwrap_err();
        assert_eq!(error, expected);
        assert!(!format!("{error:?} {error}").contains("fake.test-token"));
        assert!(client.entity.is_none());
        server.join().unwrap();
    }
}

#[test]
fn failed_rebinding_clears_old_target_and_invalid_json_is_not_echoed() {
    let (url, server) = server(vec![
        response("200 OK", br#"{"entity_id":"light.room","state":"on","attributes":{"supported_color_modes":["rgb"]}}"#),
        response("200 OK", b"fake.test-token-not-json"),
    ]);
    let mut client = client(&url);
    client
        .bind_light("light.room", deadline(), || false)
        .unwrap();
    assert_eq!(
        client.bind_light("light.new", deadline(), || false),
        Err(Error::InvalidResponse)
    );
    assert_eq!(
        client.set_color([1, 2, 3], deadline(), || false),
        Err(Error::InvalidInput)
    );
    server.join().unwrap();
}

#[test]
fn rejects_declared_and_streamed_oversize_bodies_and_headers() {
    let declared = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        MAX_RESPONSE_BYTES + 1
    )
    .into_bytes();
    let (url, server) = server(vec![declared]);
    assert_eq!(
        client(&url).lights(deadline(), || false),
        Err(Error::ResponseTooLarge)
    );
    server.join().unwrap();
    let mut streamed = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
    streamed.resize(streamed.len() + MAX_RESPONSE_BYTES + 1, b' ');
    let (url, server) = self::server(vec![streamed]);
    assert_eq!(
        client(&url).lights(deadline(), || false),
        Err(Error::ResponseTooLarge)
    );
    server.join().unwrap();
    let oversized_headers = format!(
        "HTTP/1.1 200 OK\r\nX-Large: {}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]",
        "a".repeat(MAX_HEADER_BYTES as usize + 1024)
    )
    .into_bytes();
    let (url, server) = self::server(vec![oversized_headers]);
    assert!(matches!(
        client(&url).lights(deadline(), || false),
        Err(Error::WinHttp(_))
    ));
    server.join().unwrap();
}

#[test]
fn redirects_are_not_followed() {
    let destination = TcpListener::bind("127.0.0.1:0").unwrap();
    destination.set_nonblocking(true).unwrap();
    let redirect = format!("HTTP/1.1 302 Found\r\nLocation: http://{}/steal\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", destination.local_addr().unwrap()).into_bytes();
    let (url, server) = server(vec![redirect]);
    assert_eq!(
        client(&url).lights(deadline(), || false),
        Err(Error::Http(302))
    );
    server.join().unwrap();
    assert_eq!(
        destination.accept().err().unwrap().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn cancellation_and_expired_deadline_do_not_connect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let client = client(&format!("http://{}", listener.local_addr().unwrap()));
    assert_eq!(client.lights(deadline(), || true), Err(Error::Cancelled));
    assert_eq!(
        client.lights(Instant::now(), || false),
        Err(Error::Deadline)
    );
    assert_eq!(
        listener.accept().err().unwrap().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn cancellation_and_deadline_are_checked_after_blocking_io() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    for cancel in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let cancelled = Arc::new(AtomicBool::new(false));
        let server_cancelled = Arc::clone(&cancelled);
        let server = thread::spawn(move || {
            let until = deadline();
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until =>
                    {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(e) => panic!("loopback accept: {e}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut request = [0u8; 16_384];
            assert!(stream.read(&mut request).unwrap() > 0);
            thread::sleep(Duration::from_millis(150));
            server_cancelled.store(cancel, Ordering::Relaxed);
            let _ = stream.write_all(&response("200 OK", b"[]"));
        });
        let until = if cancel {
            deadline()
        } else {
            Instant::now() + Duration::from_millis(100)
        };
        let error = client(&url)
            .lights(until, || cancelled.load(Ordering::Relaxed))
            .unwrap_err();
        assert_eq!(
            error,
            if cancel {
                Error::Cancelled
            } else {
                Error::Deadline
            }
        );
        server.join().unwrap();
    }
}
