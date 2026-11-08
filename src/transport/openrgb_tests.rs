// SPDX-License-Identifier: GPL-2.0-or-later
use super::*;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
};

fn u16v(b: &mut Vec<u8>, v: u16) {
    b.extend(v.to_le_bytes());
}
fn u32v(b: &mut Vec<u8>, v: u32) {
    b.extend(v.to_le_bytes());
}
fn string(b: &mut Vec<u8>, v: &str) {
    u16v(b, v.len() as u16 + 1);
    b.extend(v.as_bytes());
    b.push(0);
}
fn packet(s: &mut TcpStream, device: u32, id: u32, body: &[u8]) {
    let mut b = b"ORGB".to_vec();
    u32v(&mut b, device);
    u32v(&mut b, id);
    u32v(&mut b, body.len() as u32);
    b.extend(body);
    s.write_all(&b).unwrap();
}
fn read(s: &mut TcpStream) -> Option<(u32, u32, Vec<u8>)> {
    let mut h = [0; 16];
    s.read_exact(&mut h).ok()?;
    assert_eq!(&h[..4], b"ORGB");
    let n = u32::from_le_bytes(h[12..16].try_into().unwrap()) as usize;
    assert!(n <= 1024 * 1024);
    let mut b = vec![0; n];
    s.read_exact(&mut b).unwrap();
    Some((
        u32::from_le_bytes(h[4..8].try_into().unwrap()),
        u32::from_le_bytes(h[8..12].try_into().unwrap()),
        b,
    ))
}
fn ack(s: &mut TcpStream, device: u32, id: u32) {
    let mut b = vec![];
    u32v(&mut b, id);
    u32v(&mut b, 0);
    packet(s, device, 10, &b);
}
fn controller(serial: &str, future: bool) -> Vec<u8> {
    controller_matrices(serial, future, &[None], false)
}
fn controller_matrices(
    serial: &str,
    future: bool,
    matrices: &[Option<&[u32]>],
    effects_only: bool,
) -> Vec<u8> {
    let mut b = vec![0; 4];
    u32v(&mut b, if future { 99 } else { 14 });
    for value in [
        "Fixture",
        "Vendor",
        "Description",
        "v1",
        serial,
        "USB:fixture",
    ] {
        string(&mut b, value);
    }
    u16v(&mut b, 1);
    u32v(&mut b, u32::MAX); // no valid active mode must not panic
    string(&mut b, "Direct");
    for value in [
        0x30 | if future { 1 << 30 } else { 0 },
        0,
        0,
        0,
        100,
        7,
        0,
        0,
        0,
        if future { 99 } else { 0 },
        1,
    ] {
        u32v(&mut b, value);
    }
    u16v(&mut b, 0); // mode colors
    u16v(&mut b, matrices.len() as u16);
    for (zone, matrix) in matrices.iter().enumerate() {
        string(&mut b, "Zone");
        for value in [
            if future {
                99
            } else if matrix.is_some() {
                2
            } else {
                1
            },
            2,
            2,
            2,
        ] {
            u32v(&mut b, value);
        }
        if let Some(indices) = matrix {
            u16v(&mut b, (8 + 4 * indices.len()) as u16);
            let height = if indices.len() == 4 { 2 } else { 1 };
            u32v(&mut b, height);
            u32v(&mut b, indices.len() as u32 / height);
            for &index in *indices {
                u32v(&mut b, index);
            }
        } else {
            u16v(&mut b, 0);
        }
        u16v(&mut b, 0); // segments
        u32v(
            &mut b,
            (1 << 30) | if effects_only && zone == 0 { 1 } else { 0 },
        );
        u32v(&mut b, u32::MAX);
        u16v(&mut b, 0);
        string(&mut b, "Zone");
    }
    let outputs = matrices.len() * 2 - usize::from(effects_only);
    u16v(&mut b, outputs as u16);
    for index in 0..outputs {
        string(&mut b, &format!("LED{index}"));
    }
    u16v(&mut b, outputs as u16);
    b.resize(b.len() + outputs * 4, 0);
    u16v(&mut b, 0);
    u32v(&mut b, 1 << 30);
    string(&mut b, "{}");
    let n = b.len() as u32;
    b[..4].copy_from_slice(&n.to_le_bytes());
    b
}

type Captured = Arc<Mutex<Vec<(u32, u32, Vec<u8>)>>>;
fn server(reorder_after: usize, future: bool) -> (SocketAddr, Captured, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = listener.local_addr().unwrap();
    let calls: Captured = Arc::new(Mutex::new(vec![]));
    let saved = calls.clone();
    let handle = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
        let mut scans = 0;
        while let Some((device, id, b)) = read(&mut s) {
            saved.lock().unwrap().push((device, id, b.clone()));
            match id {
                40 => {
                    assert_eq!(b, 6u32.to_le_bytes());
                    packet(&mut s, device, id, &6u32.to_le_bytes());
                }
                52 => {
                    assert_eq!(b, 1u32.to_le_bytes());
                    packet(&mut s, device, 53, &(1u32 | 1 << 30).to_le_bytes());
                }
                0 => {
                    scans += 1;
                    let mut ids = vec![];
                    u32v(&mut ids, 1);
                    u32v(&mut ids, if scans > reorder_after { 8 } else { 7 });
                    packet(&mut s, device, id, &ids);
                }
                1 => {
                    assert!(device == 7 || device == 8);
                    packet(&mut s, device, id, &controller("SERIAL", future));
                }
                1101 | 1050 => (),
                _ => panic!("Unexpected write {id}"),
            }
            ack(&mut s, device, id);
            if reorder_after == 2 && id == 1101 {
                packet(&mut s, 0, 100, &[]);
            }
        }
    });
    (endpoint, calls, handle)
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(3)
}

#[test]
fn virtual_controller_flag_is_recognized_even_with_a_physical_device_type() {
    let mut payload = controller("SERIAL", false);
    // SDK controller flags precede the final length-prefixed "{}" configuration.
    let offset = payload.len() - (4 + 2 + 3);
    payload[offset..offset + 4].copy_from_slice(&(1u32 << 2).to_le_bytes());
    let (addr, calls, server) = matrix_server(vec![payload]);
    let mut session = OpenRgbSession::connect(addr, deadline(), &|| false).unwrap();
    let inventory = session.scan(deadline(), &|| false).unwrap();
    assert_eq!(inventory[0].kind, "Virtual");
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, id, _)| matches!(id, 1101 | 1050))
    );
    drop(session);
    server.join().unwrap();
}

#[test]
fn inventory_is_read_only_and_tolerates_future_values() {
    let (addr, calls, h) = server(usize::MAX, true);
    let mut session = OpenRgbSession::connect(addr, deadline(), &|| false).unwrap();
    let inventory = session.scan(deadline(), &|| false).unwrap();
    assert_eq!(inventory[0].kind, "Unrecognized(99)");
    assert_eq!(inventory[0].zones[0].kind, "Unrecognized(99)");
    assert_eq!(inventory[0].led_names, ["LED0", "LED1"]);
    drop(session);
    h.join().unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.1)
            .collect::<Vec<_>>(),
        [40, 52, 0, 1]
    );
}

#[test]
fn explicit_open_sends_one_max_brightness_mode_then_exact_rgb_frame() {
    let (addr, calls, h) = server(usize::MAX, false);
    let mut session = OpenRgbSession::connect(addr, deadline(), &|| false).unwrap();
    let info = session.scan(deadline(), &|| false).unwrap().remove(0);
    assert_eq!(info.kind, "Storage");
    session.open_output(&info, deadline(), &|| false).unwrap();
    session
        .submit(&[[1, 2, 3], [240, 128, 64]], deadline(), &|| false)
        .unwrap();
    assert!(session.submit(&[[1, 2, 3]], deadline(), &|| false).is_err());
    drop(session);
    h.join().unwrap();
    let calls = calls.lock().unwrap();
    let modes = calls.iter().filter(|c| c.1 == 1101).collect::<Vec<_>>();
    assert_eq!(modes.len(), 1);
    // size, mode index, name length + Direct NUL, flags/speed limits/brightness limits/current.
    assert_eq!(
        u32::from_le_bytes(modes[0].2[37..41].try_into().unwrap()),
        100
    );
    let frame = calls.iter().find(|c| c.1 == 1050).unwrap();
    assert_eq!(frame.0, 7);
    assert_eq!(frame.2, [14, 0, 0, 0, 2, 0, 1, 2, 3, 0, 240, 128, 64, 0]);
}

#[test]
fn identity_change_blocks_mode_and_discards_connection() {
    let (addr, calls, h) = server(1, false);
    let mut session = OpenRgbSession::connect(addr, deadline(), &|| false).unwrap();
    let info = session.scan(deadline(), &|| false).unwrap().remove(0);
    assert!(
        session
            .open_output(&info, deadline(), &|| false)
            .unwrap_err()
            .contains("identity")
    );
    assert!(
        session
            .scan(deadline(), &|| false)
            .unwrap_err()
            .contains("discarded")
    );
    drop(session);
    h.join().unwrap();
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.1 == 1101 || c.1 == 1050)
    );
}

#[test]
fn inventory_notification_before_frame_blocks_rgb_without_rescan() {
    let (addr, calls, h) = server(2, false);
    let mut session = OpenRgbSession::connect(addr, deadline(), &|| false).unwrap();
    let info = session.scan(deadline(), &|| false).unwrap().remove(0);
    session.open_output(&info, deadline(), &|| false).unwrap();
    thread::sleep(Duration::from_millis(20));
    assert!(
        session
            .submit(&[[1, 2, 3]; 2], deadline(), &|| false)
            .is_err()
    );
    drop(session);
    h.join().unwrap();
    assert!(!calls.lock().unwrap().iter().any(|c| c.1 == 1050));
}

#[test]
fn malicious_counts_strings_and_geometry_are_bounded() {
    for payload in [
        {
            let mut b = vec![];
            u32v(&mut b, u32::MAX);
            b
        },
        {
            let mut b = vec![0; 4];
            u32v(&mut b, 14);
            u16v(&mut b, 4097);
            b.resize(b.len() + 4097, 0);
            b
        },
        {
            let mut b = controller("SERIAL", false);
            b.truncate(20);
            b
        },
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let h = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            while let Some((device, id, _)) = read(&mut s) {
                match id {
                    40 => packet(&mut s, device, id, &6u32.to_le_bytes()),
                    52 => packet(&mut s, device, 53, &1u32.to_le_bytes()),
                    0 if payload.len() == 4 => {
                        packet(&mut s, device, id, &payload);
                        ack(&mut s, device, id);
                        break;
                    }
                    0 => {
                        let mut b = vec![];
                        u32v(&mut b, 1);
                        u32v(&mut b, 7);
                        packet(&mut s, device, id, &b);
                    }
                    1 => {
                        packet(&mut s, device, id, &payload);
                        ack(&mut s, device, id);
                        break;
                    }
                    _ => panic!("unexpected write"),
                }
                ack(&mut s, device, id);
            }
        });
        let mut session = OpenRgbSession::connect(addr, deadline(), &|| false).unwrap();
        assert!(session.scan(deadline(), &|| false).is_err());
        drop(session);
        h.join().unwrap();
    }
}

#[test]
fn malicious_packet_size_and_truncation_fail_without_panic() {
    for size in [u32::MAX, 4] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let h = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            read(&mut s).unwrap();
            let mut b = b"ORGB".to_vec();
            u32v(&mut b, 0);
            u32v(&mut b, 40);
            u32v(&mut b, size);
            s.write_all(&b).unwrap(); // close without payload
        });
        assert!(OpenRgbSession::connect(addr, deadline(), &|| false).is_err());
        h.join().unwrap();
    }
}

#[test]
fn timeout_and_cancellation_discard_connection() {
    for cancel in [false, true] {
        let (addr, _calls, h) = server(usize::MAX, false);
        let mut session = OpenRgbSession::connect(addr, deadline(), &|| false).unwrap();
        let stop = Instant::now();
        let result = session.scan(if cancel { deadline() } else { stop }, &|| cancel);
        assert!(result.is_err());
        assert!(
            session
                .scan(deadline(), &|| false)
                .unwrap_err()
                .contains("discarded")
        );
        assert!(stop.elapsed() < Duration::from_secs(1));
        drop(session);
        h.join().unwrap();
    }
}

#[test]
fn cancellation_while_waiting_for_reply_closes_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let h = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        for expected in [40, 52] {
            let (device, id, _) = read(&mut s).unwrap();
            assert_eq!(id, expected);
            packet(
                &mut s,
                device,
                if id == 52 { 53 } else { id },
                &if id == 52 { 1u32 } else { 6u32 }.to_le_bytes(),
            );
            ack(&mut s, device, id);
        }
        assert_eq!(read(&mut s).unwrap().1, 0);
        let mut byte = [0; 1];
        assert_eq!(s.read(&mut byte).unwrap(), 0);
    });
    let mut session = OpenRgbSession::connect(addr, deadline(), &|| false).unwrap();
    let start = Instant::now();
    let error = session
        .scan(deadline(), &|| start.elapsed() > Duration::from_millis(40))
        .unwrap_err();
    assert!(error.contains("cancelled"));
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(session.scan(deadline(), &|| false).is_err());
    drop(session);
    h.join().unwrap();
}

fn matrix_server(payloads: Vec<Vec<u8>>) -> (SocketAddr, Captured, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let calls: Captured = Arc::new(Mutex::new(vec![]));
    let saved = calls.clone();
    let h = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut descriptors = payloads.into_iter();
        while let Some((device, id, body)) = read(&mut s) {
            saved.lock().unwrap().push((device, id, body));
            match id {
                40 => packet(&mut s, device, id, &6u32.to_le_bytes()),
                52 => packet(&mut s, device, 53, &1u32.to_le_bytes()),
                0 => {
                    let mut ids = vec![];
                    u32v(&mut ids, 1);
                    u32v(&mut ids, 7);
                    packet(&mut s, device, id, &ids);
                }
                1 => packet(
                    &mut s,
                    device,
                    id,
                    &descriptors.next().expect("unexpected descriptor read"),
                ),
                1101 | 1050 => (),
                _ => panic!("unexpected write"),
            }
            ack(&mut s, device, id);
        }
    });
    (addr, calls, h)
}

#[test]
fn matrix_holes_and_zone_offsets_preserve_controller_indices() {
    let cells = [0, u32::MAX, 1, u32::MAX];
    let decorative = [0, u32::MAX, u32::MAX, u32::MAX];
    let payload = controller_matrices("SERIAL", false, &[Some(&cells), Some(&decorative)], false);
    let (addr, _, h) = matrix_server(vec![payload]);
    let mut session = OpenRgbSession::connect(addr, deadline(), &|| false).unwrap();
    let info = session.scan(deadline(), &|| false).unwrap().remove(0);
    let first = info.zones[0].matrix.as_ref().unwrap();
    assert_eq!((first.width, first.height), (2, 2));
    assert_eq!(first.led_indices, [Some(0), None, Some(1), None]);
    assert_eq!(info.zones[1].led_offset, 2);
    assert_eq!(
        info.zones[1].matrix.as_ref().unwrap().led_indices,
        [Some(2), None, None, None]
    );
    assert_eq!(info.leds, 4); // Decorative LED3 remains available outside the matrix.
    drop(session);
    h.join().unwrap();
}

#[test]
fn invalid_matrix_indices_and_duplicates_reject_inventory() {
    for cells in [[0, u32::MAX, 2, u32::MAX], [0, u32::MAX, 0, u32::MAX]] {
        let payload = controller_matrices("SERIAL", false, &[Some(&cells)], false);
        let (addr, calls, h) = matrix_server(vec![payload]);
        let mut session = OpenRgbSession::connect(addr, deadline(), &|| false).unwrap();
        assert!(
            session
                .scan(deadline(), &|| false)
                .unwrap_err()
                .contains("geometry")
        );
        drop(session);
        h.join().unwrap();
        assert!(
            !calls
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.1 == 1101 || c.1 == 1050)
        );
    }
}

#[test]
fn same_counts_with_changed_matrix_reject_output_open() {
    let before = [0, u32::MAX, 1, u32::MAX];
    let after = [1, u32::MAX, 0, u32::MAX];
    let (addr, calls, h) = matrix_server(vec![
        controller_matrices("SERIAL", false, &[Some(&before)], false),
        controller_matrices("SERIAL", false, &[Some(&after)], false),
    ]);
    let mut session = OpenRgbSession::connect(addr, deadline(), &|| false).unwrap();
    let info = session.scan(deadline(), &|| false).unwrap().remove(0);
    assert!(
        session
            .open_output(&info, deadline(), &|| false)
            .unwrap_err()
            .contains("identity")
    );
    drop(session);
    h.join().unwrap();
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.1 == 1101 || c.1 == 1050)
    );
}

#[test]
fn effects_only_virtual_matrix_uses_truthful_fallback_and_physical_offsets() {
    let cells = [0, u32::MAX, 1, u32::MAX];
    let payload = controller_matrices("SERIAL", false, &[Some(&cells), Some(&cells)], true);
    let (addr, _, h) = matrix_server(vec![payload]);
    let mut session = OpenRgbSession::connect(addr, deadline(), &|| false).unwrap();
    let info = session.scan(deadline(), &|| false).unwrap().remove(0);
    assert_eq!((info.zones[0].leds, info.zones[0].reported_leds), (1, 2));
    assert!(info.zones[0].matrix.is_none());
    assert!(info.zones[0].matrix_warning.is_some());
    assert_eq!(
        info.zones[0]
            .reported_matrix
            .as_ref()
            .unwrap()
            .zone_local_indices,
        cells
    );
    assert_eq!(info.zones[1].led_offset, 1);
    assert_eq!(
        info.zones[1].matrix.as_ref().unwrap().led_indices,
        [Some(1), None, Some(2), None]
    );
    drop(session);
    h.join().unwrap();
}
