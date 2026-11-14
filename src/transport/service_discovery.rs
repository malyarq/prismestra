// SPDX-License-Identifier: GPL-2.0-or-later
//! Explicit, short-lived DNS-SD browse; advertisements are untrusted hints.
//! No IP-range scan, HTTP request, device control, credentials or registration.
//! References checked 2026-11-13:
//! https://github.com/wled/WLED/blob/main/wled00/wled.cpp (wled/tcp, mac TXT)
//! https://contributors.home-assistant.io/core/internals/instance-discovery/
//! https://docs.rs/mdns-sd/0.21.7/mdns_sd/struct.ServiceDaemon.html

use mdns_sd::{DaemonStatus, ResolvedService, ServiceDaemon, ServiceEvent};
use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

pub const WLED_SERVICE: &str = "_wled._tcp.local.";
pub const HOME_ASSISTANT_SERVICE: &str = "_home-assistant._tcp.local.";
pub const BROWSE_DURATION: Duration = Duration::from_secs(3);
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
pub const MAX_RECORDS: usize = 128;
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const MAX_EVENTS: usize = 4096;
const MAX_ADDRESSES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Wled,
    HomeAssistant,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub kind: Kind,
    /// DNS-SD full instance, preserved for display; case-insensitive for dedup.
    pub instance: String,
    /// Unauthenticated identity hint: WLED MAC, HA UUID, or full instance fallback.
    pub identity: String,
    /// SRV port + literal IP only. TXT URLs never supply routing or credentials.
    /// HTTP candidate; TLS/reverse-proxy installations may require manual setup.
    pub http_endpoint: String,
    pub address: SocketAddr,
}
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scan {
    pub records: Vec<Record>,
    pub truncated: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Cancelled,
    Daemon,
    /// Shutdown was requested but not confirmed within SHUTDOWN_TIMEOUT.
    Shutdown,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LAN discovery: {self:?}")
    }
}
impl std::error::Error for Error {}

/// Browse only the two supported services for at most three seconds or until
/// `deadline`, whichever comes first. A pre-cancelled/expired request opens no
/// sockets. Cancellation is polled at <=20ms intervals between bounded batches;
/// callbacks and OS scheduling must be prompt. No HTTP requests are performed.
///
/// Both browses are stopped and daemon shutdown acknowledged before success or
/// ordinary error return. Cleanup waits at most one additional second. If it
/// cannot confirm shutdown, returns Error::Shutdown, never a successful scan;
/// mdns-sd exposes no thread join/abort, so a stuck OS/library cannot be forcibly
/// terminated here. No daemon handle or long-lived task is retained by the app.
pub fn discover(deadline: Instant, mut cancelled: impl FnMut() -> bool) -> Result<Scan, Error> {
    if cancelled() {
        return Err(Error::Cancelled);
    }
    if Instant::now() >= deadline {
        return Ok(Scan::default());
    }
    let until = deadline.min(Instant::now() + BROWSE_DURATION);
    let daemon = DaemonGuard(ServiceDaemon::new().map_err(|_| Error::Daemon)?);
    let result = (|| {
        let receivers = [
            daemon.0.browse(WLED_SERVICE).map_err(|_| Error::Daemon)?,
            daemon
                .0
                .browse(HOME_ASSISTANT_SERVICE)
                .map_err(|_| Error::Daemon)?,
        ];
        let mut records = BTreeMap::new();
        let mut truncated = false;
        let mut events = 0;
        'browse: while Instant::now() < until {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            for receiver in &receivers {
                for _ in 0..32 {
                    if cancelled() {
                        return Err(Error::Cancelled);
                    }
                    if Instant::now() >= until {
                        break 'browse;
                    }
                    let event = match receiver.try_recv() {
                        Ok(event) => event,
                        Err(mdns_sd::TryRecvError::Empty) => break,
                        Err(mdns_sd::TryRecvError::Disconnected) => return Err(Error::Daemon),
                    };
                    events += 1;
                    if events > MAX_EVENTS {
                        truncated = true;
                        break 'browse;
                    }
                    match event {
                        ServiceEvent::ServiceResolved(service) => {
                            for record in records_from_service(&service) {
                                let key = (record.kind, record.address);
                                if records.contains_key(&key) || records.len() < MAX_RECORDS {
                                    records.insert(key, record);
                                } else {
                                    truncated = true;
                                }
                            }
                        }
                        ServiceEvent::ServiceRemoved(_, instance) => {
                            records.retain(|_, r: &mut Record| {
                                !r.instance.eq_ignore_ascii_case(&instance)
                            });
                        }
                        _ => (),
                    }
                }
            }
            std::thread::sleep(POLL_INTERVAL.min(until.saturating_duration_since(Instant::now())));
        }
        if cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(Scan {
            records: records.into_values().collect(),
            truncated,
        })
    })();
    daemon.finish()?;
    result
}

struct DaemonGuard(ServiceDaemon);
impl DaemonGuard {
    fn finish(&self) -> Result<(), Error> {
        let _ = self.0.stop_browse(WLED_SERVICE);
        let _ = self.0.stop_browse(HOME_ASSISTANT_SERVICE);
        let until = Instant::now() + SHUTDOWN_TIMEOUT;
        loop {
            match self.0.shutdown() {
                Ok(status) => {
                    return match status
                        .recv_timeout(until.saturating_duration_since(Instant::now()))
                    {
                        Ok(DaemonStatus::Shutdown) => Ok(()),
                        _ => Err(Error::Shutdown),
                    };
                }
                Err(mdns_sd::Error::DaemonShutdown) => return Ok(()),
                Err(mdns_sd::Error::Again) if Instant::now() < until => {
                    std::thread::sleep(POLL_INTERVAL)
                }
                _ => return Err(Error::Shutdown),
            }
        }
    }
}
impl Drop for DaemonGuard {
    fn drop(&mut self) {
        // Also request exit if a caller's cancellation callback panics.
        let _ = self.0.shutdown();
    }
}

/// Pure conversion, suitable for tests; does not resolve or connect anywhere.
/// Invalid records/TXT are discarded. IP endpoints are hints until validated by
/// the corresponding HTTP client. Link-local IPv6 is omitted because endpoint
/// clients currently do not support the required interface scope/zone IDs.
pub fn records_from_service(service: &ResolvedService) -> Vec<Record> {
    let kind = if service.ty_domain.eq_ignore_ascii_case(WLED_SERVICE) {
        Kind::Wled
    } else if service
        .ty_domain
        .eq_ignore_ascii_case(HOME_ASSISTANT_SERVICE)
    {
        Kind::HomeAssistant
    } else {
        return Vec::new();
    };
    if !valid_instance(&service.fullname, &service.ty_domain)
        || service.port == 0
        || service.addresses.len() > MAX_ADDRESSES
        || !valid_txt(service)
    {
        return Vec::new();
    }
    if kind == Kind::HomeAssistant && service.get_property("landingpage").is_some() {
        return Vec::new();
    }
    let identity = match kind {
        Kind::Wled => match hex_property(service, "mac", 12) {
            Ok(Some(mac))
                if mac != "000000000000" && u8::from_str_radix(&mac[..2], 16).unwrap() & 1 == 0 =>
            {
                format!("wled:{mac}")
            }
            Ok(None) => format!("mdns:{}", service.fullname.to_ascii_lowercase()),
            _ => return Vec::new(),
        },
        Kind::HomeAssistant => match hex_property(service, "uuid", 32) {
            Ok(Some(uuid)) => format!("home-assistant:{uuid}"),
            Ok(None) => format!("mdns:{}", service.fullname.to_ascii_lowercase()),
            _ => return Vec::new(),
        },
    };
    let addresses: BTreeSet<_> = service
        .addresses
        .iter()
        .map(|address| address.to_ip_addr())
        .filter(|ip| valid_address(*ip))
        .map(|ip| SocketAddr::new(ip, service.port))
        .collect();
    addresses
        .into_iter()
        .map(|address| Record {
            kind,
            instance: service.fullname.clone(),
            identity: identity.clone(),
            http_endpoint: format!("http://{address}"),
            address,
        })
        .collect()
}
fn valid_instance(fullname: &str, ty_domain: &str) -> bool {
    if fullname.len() > 255
        || fullname.len() <= ty_domain.len() + 1
        || fullname.chars().any(char::is_control)
    {
        return false;
    }
    let suffix = fullname.len() - ty_domain.len();
    let Some((instance, suffix)) = fullname.get(..suffix).zip(fullname.get(suffix..)) else {
        return false;
    };
    suffix.eq_ignore_ascii_case(ty_domain)
        && instance.ends_with('.')
        && (1..=63).contains(&(instance.len() - 1))
        && !instance[..instance.len() - 1].trim().is_empty()
}
fn valid_address(ip: IpAddr) -> bool {
    if ip.is_unspecified() || ip.is_multicast() || ip.is_loopback() {
        return false;
    }
    match ip {
        IpAddr::V4(ip) => !ip.is_broadcast() && !ip.is_link_local(),
        IpAddr::V6(ip) => !ip.is_unicast_link_local() && ip.to_ipv4_mapped().is_none(),
    }
}
fn valid_txt(service: &ResolvedService) -> bool {
    let mut count = 0;
    let mut size = 0;
    for property in service.get_properties().iter() {
        count += 1;
        let key = property.key();
        let value = property.val().unwrap_or_default();
        size += key.len() + value.len();
        if count > 32
            || size > 4096
            || key.is_empty()
            || key.len() > 63
            || !key.bytes().all(|b| (33..=126).contains(&b) && b != b'=')
            || value.len() > 230
        {
            return false;
        }
    }
    true
}
fn hex_property(service: &ResolvedService, key: &str, length: usize) -> Result<Option<String>, ()> {
    match service.get_property_val(key) {
        None => Ok(None),
        Some(Some(bytes)) if bytes.len() == length && bytes.iter().all(u8::is_ascii_hexdigit) => {
            Ok(Some(
                String::from_utf8(bytes.to_ascii_lowercase()).map_err(|_| ())?,
            ))
        }
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdns_sd::ServiceInfo;
    fn service(kind: &str, properties: &[(&str, &str)]) -> ResolvedService {
        ServiceInfo::new(kind, "Desk", "desk.local.", "192.168.1.50", 80, properties)
            .unwrap()
            .as_resolved_service()
    }
    #[test]
    fn converts_only_supported_services_and_literal_routes() {
        let s = service(
            WLED_SERVICE,
            &[
                ("mac", "AABBCCDDEEFF"),
                ("internal_url", "http://user:secret@attacker/"),
            ],
        );
        let records = records_from_service(&s);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, Kind::Wled);
        assert_eq!(records[0].identity, "wled:aabbccddeeff");
        assert_eq!(records[0].http_endpoint, "http://192.168.1.50:80");
        let mut s = service(
            HOME_ASSISTANT_SERVICE,
            &[("uuid", "0123456789abcdef0123456789abcdef")],
        );
        s.port = 8123;
        let records = records_from_service(&s);
        assert_eq!(
            records[0].identity,
            "home-assistant:0123456789abcdef0123456789abcdef"
        );
        assert_eq!(records[0].http_endpoint, "http://192.168.1.50:8123");
        assert!(records_from_service(&service("_other._tcp.local.", &[])).is_empty());
    }
    #[test]
    fn validates_untrusted_txt_and_instances() {
        for mac in [
            "",
            "000000000000",
            "ffffffffffff",
            "aabbccddeeg0",
            "aa:bb:cc:dd:ee:ff",
        ] {
            assert!(records_from_service(&service(WLED_SERVICE, &[("mac", mac)])).is_empty());
        }
        assert!(
            records_from_service(&service(HOME_ASSISTANT_SERVICE, &[("uuid", "bad")])).is_empty()
        );
        assert!(
            records_from_service(&service(HOME_ASSISTANT_SERVICE, &[("landingpage", "True")]))
                .is_empty()
        );
        let long = "x".repeat(231);
        assert!(records_from_service(&service(WLED_SERVICE, &[("ignored", &long)])).is_empty());
        let fallback = records_from_service(&service(WLED_SERVICE, &[]));
        assert_eq!(fallback[0].identity, "mdns:desk._wled._tcp.local.");
        for name in [
            "_wled._tcp.local.",
            "._wled._tcp.local.",
            "bad\n._wled._tcp.local.",
            "Desk._other._tcp.local.",
        ] {
            let mut s = service(WLED_SERVICE, &[]);
            s.fullname = name.into();
            assert!(records_from_service(&s).is_empty());
        }
    }
    #[test]
    fn validates_addresses_and_preserves_ipv6_brackets() {
        let mut s = service(WLED_SERVICE, &[]);
        s.addresses = [
            "0.0.0.0",
            "127.0.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "169.254.1.1",
            "::",
            "::1",
            "ff02::1",
            "fe80::1",
            "::ffff:192.168.1.5",
            "fd00::123",
        ]
        .into_iter()
        .map(|ip| ip.parse::<IpAddr>().unwrap().into())
        .collect();
        let records = records_from_service(&s);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].http_endpoint, "http://[fd00::123]:80");
        s.port = 0;
        assert!(records_from_service(&s).is_empty());
    }
    #[test]
    fn pre_cancel_and_expired_request_do_not_start_daemon() {
        assert_eq!(
            discover(Instant::now() + BROWSE_DURATION, || true),
            Err(Error::Cancelled)
        );
        assert_eq!(discover(Instant::now(), || false), Ok(Scan::default()));
    }
}
