//! Loopback SDK6 fixture. Packet layout matches the transport's independent SDK fixtures;
//! no installed OpenRGB process or physical device is contacted.
use std::{
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
pub type Calls = Arc<Mutex<Vec<(u32, u32, Vec<u8>)>>>;
pub struct Peer {
    pub endpoint: SocketAddr,
    pub calls: Calls,
    stop: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<TcpStream>>>,
    paused: Arc<AtomicBool>,
    changed_identity: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
fn word(out: &mut Vec<u8>, n: u16) {
    out.extend(n.to_le_bytes());
}
fn dword(out: &mut Vec<u8>, n: u32) {
    out.extend(n.to_le_bytes());
}
fn text(out: &mut Vec<u8>, s: &str) {
    word(out, s.len() as u16 + 1);
    out.extend(s.as_bytes());
    out.push(0);
}
fn controller(kind: u32, zone_name: &str, serial: &str, zone_size: u32) -> Vec<u8> {
    let mut out = vec![0; 4];
    dword(&mut out, kind);
    for value in [
        "Fixture",
        "Prismestra",
        "Loopback only",
        "v1",
        serial,
        "fixture-location",
    ] {
        text(&mut out, value);
    }
    word(&mut out, 1);
    dword(&mut out, u32::MAX);
    text(&mut out, "Direct");
    for value in [0x30, 0, 0, 0, 100, 7, 0, 0, 0, 0, 1] {
        dword(&mut out, value);
    }
    word(&mut out, 0);
    word(&mut out, 1);
    text(&mut out, zone_name);
    for value in [1, zone_size, zone_size, zone_size] {
        dword(&mut out, value);
    }
    word(&mut out, 0);
    word(&mut out, 0);
    dword(&mut out, 1 << 30);
    dword(&mut out, u32::MAX);
    word(&mut out, 0);
    text(&mut out, zone_name);
    word(&mut out, 2);
    text(&mut out, "LED0");
    text(&mut out, "LED1");
    word(&mut out, 2);
    out.extend([0; 8]);
    word(&mut out, 0);
    dword(&mut out, 1 << 30);
    text(&mut out, "{}");
    let size = out.len() as u32;
    out[..4].copy_from_slice(&size.to_le_bytes());
    out
}
fn packet(stream: &mut TcpStream, device: u32, kind: u32, body: &[u8]) -> std::io::Result<()> {
    let mut bytes = b"ORGB".to_vec();
    dword(&mut bytes, device);
    dword(&mut bytes, kind);
    dword(&mut bytes, body.len() as u32);
    bytes.extend(body);
    stream.write_all(&bytes)
}
impl Peer {
    #[allow(dead_code)]
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Release);
        if paused && let Some(socket) = self.socket.lock().unwrap().as_ref() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    #[allow(dead_code)]
    pub fn change_identity(&self, changed: bool) {
        self.changed_identity.store(changed, Ordering::Release);
    }

    pub fn new(duplicate: bool) -> Self {
        Self::start(duplicate, false, false, None, 0)
    }
    pub fn virtual_controller() -> Self {
        Self::start(false, true, false, None, 0)
    }
    pub fn becomes_ambiguous_on_activation() -> Self {
        Self::start(false, false, true, None, 0)
    }
    pub fn with_invalid_neighbor(serial: &'static str) -> Self {
        Self::start(false, false, false, Some(serial), 0)
    }
    #[allow(dead_code)]
    pub fn disconnect_inventory(sessions: usize) -> Self {
        Self::start(false, false, false, None, sessions)
    }
    fn start(
        duplicate: bool,
        virtual_controller: bool,
        late_duplicate: bool,
        invalid_neighbor: Option<&'static str>,
        disconnect_inventory: usize,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let socket = Arc::new(Mutex::new(None));
        let paused = Arc::new(AtomicBool::new(false));
        let changed_identity = Arc::new(AtomicBool::new(false));
        let calls: Calls = Arc::new(Mutex::new(vec![]));
        let (done, current, saved) = (stop.clone(), socket.clone(), calls.clone());
        let (reject, changed) = (paused.clone(), changed_identity.clone());
        let thread = thread::spawn(move || {
            let mut connections = 0;
            while !done.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                connections += 1;
                let mut inventory_requests = 0;
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                {
                    let mut current = current.lock().unwrap();
                    if reject.load(Ordering::Acquire) {
                        continue;
                    }
                    *current = Some(stream.try_clone().unwrap());
                }
                loop {
                    let mut header = [0; 16];
                    if stream.read_exact(&mut header).is_err() {
                        break;
                    }
                    assert_eq!(&header[..4], b"ORGB");
                    let device = u32::from_le_bytes(header[4..8].try_into().unwrap());
                    let kind = u32::from_le_bytes(header[8..12].try_into().unwrap());
                    let size = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
                    assert!(size <= 1024 * 1024);
                    let mut body = vec![0; size];
                    if stream.read_exact(&mut body).is_err() {
                        break;
                    }
                    saved.lock().unwrap().push((device, kind, body));
                    let result = match kind {
                        40 => packet(&mut stream, device, kind, &6u32.to_le_bytes()),
                        52 => packet(&mut stream, device, 53, &(1u32 | 1 << 30).to_le_bytes()),
                        0 => {
                            if connections <= disconnect_inventory {
                                break;
                            }
                            inventory_requests += 1;
                            let duplicate = duplicate
                                || invalid_neighbor.is_some()
                                || (late_duplicate && connections == 2 && inventory_requests > 1);
                            let mut ids = vec![];
                            dword(&mut ids, if duplicate { 2 } else { 1 });
                            dword(&mut ids, if connections == 1 { 7 } else { 8 });
                            if duplicate {
                                dword(&mut ids, 9);
                            }
                            packet(&mut stream, device, kind, &ids)
                        }
                        1 if device == 9 && invalid_neighbor.is_some() => packet(
                            &mut stream,
                            device,
                            kind,
                            &controller(14, "Broken zone", invalid_neighbor.unwrap(), 3),
                        ),
                        1 => packet(
                            &mut stream,
                            device,
                            kind,
                            &controller(
                                if virtual_controller { 13 } else { 14 },
                                if late_duplicate && device == 9 {
                                    "Different zone"
                                } else {
                                    "Zone"
                                },
                                if changed.load(Ordering::Acquire) {
                                    "DIFFERENT-SERIAL"
                                } else {
                                    "FAKE-SERIAL"
                                },
                                2,
                            ),
                        ),
                        1101 | 1050 => Ok(()),
                        _ => panic!("Unexpected SDK packet {kind}"),
                    };
                    if result.is_err() {
                        break;
                    }
                    let mut ack = vec![];
                    dword(&mut ack, kind);
                    dword(&mut ack, 0);
                    if packet(&mut stream, device, 10, &ack).is_err() {
                        break;
                    }
                }
                *current.lock().unwrap() = None;
            }
        });
        Self {
            endpoint,
            calls,
            stop,
            socket,
            paused,
            changed_identity,
            thread: Some(thread),
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(socket) = self.socket.lock().unwrap().as_ref() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
