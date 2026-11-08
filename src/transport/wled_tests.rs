use super::*;
use std::time::Duration;

fn receiver() -> UdpSocket {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    socket
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(2)
}

fn receive(socket: &UdpSocket) -> Vec<u8> {
    let mut buffer = [0; 2048];
    let size = socket.recv(&mut buffer).unwrap();
    buffer[..size].to_vec()
}

fn assert_empty(socket: &UdpSocket) {
    let error = socket.recv(&mut [0; 2048]).unwrap_err();
    assert!(matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ));
}

#[test]
fn encoder_header_is_exact_and_big_endian() {
    let rgb = [1, 2, 3, 254, 253, 252];
    let mut buffer = [0; HEADER_BYTES + PACKET_RGB_BYTES];
    let size = encode_packet(&rgb, 0, 15, &mut buffer);
    assert_eq!(
        &buffer[..size],
        &[0x41, 15, 0x0b, 1, 0, 0, 0, 0, 0, 6, 1, 2, 3, 254, 253, 252]
    );
    let rgb = vec![7; 66_000];
    let size = encode_packet(&rgb, 65_700, 2, &mut buffer);
    assert_eq!(&buffer[..10], &[0x41, 2, 0x0b, 1, 0, 1, 0, 164, 1, 44]);
    assert_eq!(size, 310);
}

#[test]
fn validates_targets_and_capacity_before_socket_use() {
    for address in [
        "0.0.0.0:4048",
        "255.255.255.255:4048",
        "224.0.0.1:4048",
        "[::]:4048",
        "[ff02::1]:4048",
        "[::ffff:127.0.0.1]:4048",
        "127.0.0.1:0",
    ] {
        assert_eq!(
            Client::new(address.parse().unwrap(), 10)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
    for capacity in [0, MAX_FRAME_LEDS + 1] {
        assert!(Client::new("127.0.0.1:4048".parse().unwrap(), capacity).is_err());
    }
    assert!(validate_target("[::1]:4048".parse().unwrap()).is_ok());
}

#[test]
fn invalid_frames_send_nothing_and_do_not_consume_sequence() {
    let socket = receiver();
    let mut client = Client::new(socket.local_addr().unwrap(), 2).unwrap();
    for rgb in [&[][..], &[1][..], &[1, 2][..], &[0; 9][..]] {
        assert_eq!(
            client
                .send_frame(rgb, deadline(), || false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
    assert_empty(&socket);
    assert_eq!(client.sequence, 1);
    assert!(validate_frame(&vec![0; MAX_FRAME_LEDS * 3], MAX_FRAME_LEDS).is_ok());
    assert!(validate_frame(&vec![0; (MAX_FRAME_LEDS + 1) * 3], MAX_FRAME_LEDS).is_err());
}

#[test]
fn loopback_reassembles_chunks_flags_offsets_sequence_and_destination() {
    let socket = receiver();
    let other = receiver();
    let mut client = Client::new(socket.local_addr().unwrap(), 1000).unwrap();
    assert_eq!(client.target(), socket.local_addr().unwrap());
    let rgb: Vec<u8> = (0..3000).map(|n| (n % 251) as u8).collect();
    assert_eq!(
        client.send_frame(&rgb, deadline(), || false).unwrap(),
        FrameSubmission {
            datagrams: 3,
            rgb_bytes: 3000
        }
    );
    let mut reconstructed = Vec::new();
    for index in 0..3 {
        let packet = receive(&socket);
        let expected_len = if index == 2 { 600 } else { 1200 };
        assert_eq!(packet.len(), 10 + expected_len);
        assert_eq!(
            &packet[..4],
            &[
                if index == 2 { 0x41 } else { 0x40 },
                index as u8 + 1,
                0x0b,
                1
            ]
        );
        assert_eq!(
            u32::from_be_bytes(packet[4..8].try_into().unwrap()) as usize,
            index * 1200
        );
        assert_eq!(
            u16::from_be_bytes(packet[8..10].try_into().unwrap()) as usize,
            expected_len
        );
        reconstructed.extend_from_slice(&packet[10..]);
    }
    assert_eq!(reconstructed, rgb);
    assert_empty(&socket);
    assert_empty(&other);
    client.send_frame(&[9, 8, 7], deadline(), || false).unwrap();
    assert_eq!(&receive(&socket)[..4], &[0x41, 4, 0x0b, 1]);
}

#[test]
fn exact_chunk_boundary_and_sequence_wrap() {
    let socket = receiver();
    let mut client = Client::new(socket.local_addr().unwrap(), 800).unwrap();
    client.sequence = 15;
    client.send_frame(&[4; 2400], deadline(), || false).unwrap();
    assert_eq!(&receive(&socket)[..4], &[0x40, 15, 0x0b, 1]);
    assert_eq!(&receive(&socket)[..4], &[0x41, 1, 0x0b, 1]);
    assert_eq!(client.sequence, 2);
    assert_empty(&socket);
}

#[test]
fn cancellation_and_deadline_prevent_push_after_partial_frame() {
    let socket = receiver();
    let mut client = Client::new(socket.local_addr().unwrap(), 800).unwrap();
    assert_eq!(
        client
            .send_frame(&[1; 3], deadline(), || true)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(
        client
            .send_frame(&[1; 3], Instant::now(), || false)
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert_empty(&socket);
    let mut calls = 0;
    let error = client
        .send_frame(&[1; 2400], deadline(), || {
            calls += 1;
            calls == 2
        })
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(&receive(&socket)[..4], &[0x40, 1, 0x0b, 1]);
    assert_empty(&socket);
    assert_eq!(client.sequence, 2);
    let mut calls = 0;
    let error = client
        .send_frame(
            &[1; 2400],
            Instant::now() + Duration::from_millis(15),
            || {
                calls += 1;
                if calls == 2 {
                    std::thread::sleep(Duration::from_millis(30));
                }
                false
            },
        )
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(receive(&socket)[0], 0x40);
    assert_empty(&socket);
}
