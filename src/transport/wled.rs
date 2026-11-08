// SPDX-License-Identifier: GPL-2.0-or-later
//! Explicit unicast RGB8 DDP output, without discovery or a background worker.
//!
//! Protocol references (independent implementation, checked 2026-11-08):
//! https://kno.wled.ge/interfaces/ddp/ (default UDP port 4048; no timecodes)
//! https://github.com/wled/WLED/blob/main/wled00/udp.cpp (`realtimeBroadcast`)
//! https://github.com/wled/WLED/blob/main/wled00/src/dependencies/e131/ESPAsyncE131.h
//! Header is version 1, RGB24, display ID 1, big-endian byte offset and length.
//! Sequence advances per datagram through 1..=15; only the final packet has PUSH.
//! UDP submission is not receiver acknowledgement or physical output confirmation.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::time::Instant;

pub const DEFAULT_PORT: u16 = 4048;
pub const MAX_FRAME_LEDS: usize = 65_535;
// 1210 UDP bytes + 8 UDP + 40 IPv6 = 1258, below IPv6's minimum MTU (1280).
// A smaller path MTU/tunnel may still prevent transmission; no delivery is promised.
pub const PACKET_RGB_BYTES: usize = 1200;
const HEADER_BYTES: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameSubmission {
    pub datagrams: usize,
    pub rgb_bytes: usize,
}

pub struct Client {
    socket: UdpSocket,
    target: SocketAddr,
    max_leds: usize,
    sequence: u8,
}

impl Client {
    /// Bind an ephemeral local socket to exactly one explicit unicast destination.
    /// `max_leds` is the configured receiver capacity, in 1..=MAX_FRAME_LEDS.
    /// This does not query or verify the receiver. Non-default ports support
    /// explicit forwarding/test endpoints; normal WLED uses DEFAULT_PORT.
    pub fn new(target: SocketAddr, max_leds: usize) -> io::Result<Self> {
        validate_target(target)?;
        if !(1..=MAX_FRAME_LEDS).contains(&max_leds) {
            return Err(invalid("invalid WLED LED capacity"));
        }
        let local = match target.ip() {
            IpAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
            IpAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
        };
        let socket = UdpSocket::bind(local)?;
        socket.set_broadcast(false)?;
        socket.set_nonblocking(true)?;
        socket.connect(target)?;
        Ok(Self {
            socket,
            target,
            max_leds,
            sequence: 1,
        })
    }

    pub fn target(&self) -> SocketAddr {
        self.target
    }

    /// Validate the complete frame before sending anything. Callers own scheduling.
    /// Check cancellation and the absolute deadline before every nonblocking send;
    /// callback must itself be prompt. WouldBlock and other socket errors return
    /// immediately, without retries. Cancellation/errors may leave a partial frame
    /// at the receiver, which a later successful frame replaces. No blackout or
    /// receiver reset is sent. An in-flight OS send cannot be recalled.
    pub fn send_frame(
        &mut self,
        rgb: &[u8],
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> io::Result<FrameSubmission> {
        validate_frame(rgb, self.max_leds)?;
        let mut packet = [0u8; HEADER_BYTES + PACKET_RGB_BYTES];
        let mut datagrams = 0;
        for offset in (0..rgb.len()).step_by(PACKET_RGB_BYTES) {
            if cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "WLED frame cancelled",
                ));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "WLED frame deadline expired",
                ));
            }
            let size = encode_packet(rgb, offset, self.sequence, &mut packet);
            // Reserve the number even if the OS reports failure: never reuse a
            // possibly submitted packet's sequence on the next frame.
            self.sequence = next_sequence(self.sequence);
            if self.socket.send(&packet[..size])? != size {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "incomplete WLED UDP datagram",
                ));
            }
            datagrams += 1;
        }
        Ok(FrameSubmission {
            datagrams,
            rgb_bytes: rgb.len(),
        })
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn validate_target(target: SocketAddr) -> io::Result<()> {
    let valid = match target.ip() {
        IpAddr::V4(ip) => !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast(),
        // Reject mapped IPv4 to keep IPv4 destination checks unambiguous.
        IpAddr::V6(ip) => {
            !ip.is_unspecified() && !ip.is_multicast() && ip.to_ipv4_mapped().is_none()
        }
    };
    if target.port() == 0 || !valid {
        return Err(invalid(
            "WLED requires an explicit unicast address and nonzero port",
        ));
    }
    Ok(())
}

fn validate_frame(rgb: &[u8], max_leds: usize) -> io::Result<()> {
    if rgb.is_empty() || rgb.len() % 3 != 0 || rgb.len() / 3 > max_leds {
        return Err(invalid(
            "WLED frame must contain 1..=configured capacity RGB8 pixels",
        ));
    }
    Ok(())
}

fn next_sequence(sequence: u8) -> u8 {
    if sequence == 15 { 1 } else { sequence + 1 }
}

fn encode_packet(
    rgb: &[u8],
    offset: usize,
    sequence: u8,
    out: &mut [u8; HEADER_BYTES + PACKET_RGB_BYTES],
) -> usize {
    let length = (rgb.len() - offset).min(PACKET_RGB_BYTES);
    out[0] = if offset + length == rgb.len() {
        0x41
    } else {
        0x40
    };
    out[1] = sequence;
    out[2] = 0x0b;
    out[3] = 1;
    out[4..8].copy_from_slice(&(offset as u32).to_be_bytes());
    out[8..10].copy_from_slice(&(length as u16).to_be_bytes());
    out[10..10 + length].copy_from_slice(&rgb[offset..offset + length]);
    HEADER_BYTES + length
}

#[cfg(test)]
#[path = "wled_tests.rs"]
mod tests;
