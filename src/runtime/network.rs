//! Network outputs use the same endpoint lease, latest-frame queue and stop/drain as USB.
use super::output::{Cancellation, RgbDriver, WriteResult};
use crate::{devices::Driver, lighting::color::Rgb, transport::wled};
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

#[cfg(windows)]
pub struct HomeAssistant {
    client: crate::transport::home_assistant::Client,
    last: Option<[u8; 3]>,
    next: Option<Instant>,
}
#[cfg(windows)]
impl HomeAssistant {
    pub fn connect(
        endpoint: &str,
        entity: &str,
        credential_id: &str,
        cancel: &Cancellation,
    ) -> Result<Self, String> {
        use crate::devices::home_assistant as metadata;
        if !metadata::valid_entity(entity) || metadata::credential_id(endpoint)? != credential_id {
            return Err("Invalid Home Assistant binding".into());
        }
        if cancel.cancelled() {
            return Err("Home Assistant output stopping".into());
        }
        let token = crate::credentials_windows::read(credential_id)
            .map_err(|e| e.to_string())?
            .ok_or("Home Assistant key is missing; save it before Apply")?;
        Self::with_token(endpoint, entity, token, cancel)
    }
    fn with_token(
        endpoint: &str,
        entity: &str,
        token: String,
        cancel: &Cancellation,
    ) -> Result<Self, String> {
        use crate::transport::home_assistant::{Client, Endpoint};
        let mut client = Client::new(Endpoint::parse(endpoint).map_err(|e| e.to_string())?, token)
            .map_err(|e| e.to_string())?;
        client
            .bind_light(entity, Instant::now() + Duration::from_secs(3), || {
                cancel.cancelled()
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            last: None,
            next: None,
        })
    }
}
#[cfg(windows)]
impl RgbDriver for HomeAssistant {
    fn protocol(&self) -> Driver {
        Driver::HomeAssistant
    }
    fn leds(&self) -> usize {
        1
    }
    fn write(&mut self, colors: &[Rgb], cancel: &Cancellation) -> Result<WriteResult, String> {
        if cancel.cancelled() {
            return Err("Home Assistant output stopping".into());
        }
        let [color] = colors else {
            return Err("Home Assistant requires one whole-light color".into());
        };
        let rgb = [color.r, color.g, color.b];
        if self.last == Some(rgb) {
            return Ok(WriteResult::Unchanged);
        }
        let now = Instant::now();
        if let Some(next) = self.next
            && next > now
        {
            return Ok(WriteResult::Deferred(next - now));
        }
        self.client
            .set_color(rgb, now + Duration::from_secs(3), || cancel.cancelled())
            .map_err(|e| e.to_string())?;
        self.last = Some(rgb);
        self.next = Some(Instant::now() + Duration::from_secs(1));
        Ok(WriteResult::Submitted)
    }
}

pub struct Wled {
    client: wled::Client,
    leds: usize,
    period: Duration,
    next: Option<Instant>,
    rgb: Vec<u8>,
}

pub struct OpenRgb {
    session: crate::transport::openrgb::OpenRgbSession,
    leds: usize,
    rgb: Vec<[u8; 3]>,
    next: Option<Instant>,
}
impl OpenRgb {
    pub fn connect(
        endpoint: SocketAddr,
        identity: &str,
        topology: &str,
        leds: usize,
        cancel: &Cancellation,
    ) -> Result<Self, String> {
        use crate::{devices::openrgb as metadata, transport::openrgb::OpenRgbSession};
        let deadline = || Instant::now() + Duration::from_secs(3);
        let cancelled = || cancel.cancelled();
        let mut session = OpenRgbSession::connect(endpoint, deadline(), &cancelled)?;
        let inventory = session.scan(deadline(), &cancelled)?;
        let mut matching = inventory
            .iter()
            .filter(|c| metadata::identity(c) == identity);
        let controller = matching
            .next()
            .ok_or("OpenRGB controller identity is no longer available; rediscover before Apply")?;
        if matching.next().is_some() {
            return Err("OpenRGB controller identity is ambiguous; no output activated".into());
        }
        if controller.kind.eq_ignore_ascii_case("virtual") {
            return Err("OpenRGB virtual controllers cannot own physical lighting outputs".into());
        }
        if controller.leds != leds || metadata::topology(controller) != topology {
            return Err(format!(
                "OpenRGB topology changed ({} LEDs); update device/map and rediscover before Apply",
                controller.leds
            ));
        }
        session.open_output(controller, deadline(), &cancelled)?;
        Ok(Self {
            session,
            leds,
            rgb: vec![[0; 3]; leds],
            next: None,
        })
    }
}
impl RgbDriver for OpenRgb {
    fn protocol(&self) -> Driver {
        Driver::OpenRgb
    }
    fn leds(&self) -> usize {
        self.leds
    }
    fn write(&mut self, colors: &[Rgb], cancel: &Cancellation) -> Result<WriteResult, String> {
        if cancel.cancelled() {
            return Err("OpenRGB output stopping".into());
        }
        if colors.len() != self.leds {
            return Err("OpenRGB requires the complete controller frame".into());
        }
        let now = Instant::now();
        if let Some(next) = self.next
            && next > now
        {
            return Ok(WriteResult::Deferred(next - now));
        }
        for (color, rgb) in colors.iter().zip(&mut self.rgb) {
            *rgb = [color.r, color.g, color.b];
        }
        self.session
            .submit(&self.rgb, now + Duration::from_secs(3), &|| {
                cancel.cancelled()
            })?;
        self.next = Some(Instant::now() + Duration::from_micros(16_667));
        Ok(WriteResult::Submitted)
    }
}
impl Wled {
    /// Opens only the explicitly selected UDP endpoint; does not probe the LAN.
    pub fn connect(endpoint: SocketAddr, leds: usize) -> Result<Self, String> {
        if !(1..=wled::MAX_FRAME_LEDS).contains(&leds) {
            return Err("WLED endpoint capacity limit exceeded".into());
        }
        // Host controls the requested device cadence; this independent ceiling
        // stays correct when a document changes its requested frame rate.
        let rate_hz = 60usize.min(1_000_000 / (leds * 3)) as u64;
        Ok(Self {
            client: wled::Client::new(crate::devices::canonical_endpoint(endpoint), leds)
                .map_err(|e| e.to_string())?,
            leds,
            period: Duration::from_micros(1_000_000u64.div_ceil(rate_hz)),
            next: None,
            rgb: vec![0; leds * 3],
        })
    }
}
impl RgbDriver for Wled {
    fn protocol(&self) -> Driver {
        Driver::WledDdp
    }
    fn leds(&self) -> usize {
        self.leds
    }
    fn write(&mut self, colors: &[Rgb], cancel: &Cancellation) -> Result<WriteResult, String> {
        if cancel.cancelled() {
            return Err("WLED output stopping".into());
        }
        if colors.len() != self.leds {
            return Err("WLED complete frame size changed".into());
        }
        let now = Instant::now();
        if let Some(next) = self.next
            && next > now
        {
            return Ok(WriteResult::Deferred(next - now));
        }
        for (color, bytes) in colors
            .iter()
            .zip(self.rgb.as_chunks_mut::<3>().0.iter_mut())
        {
            bytes.copy_from_slice(&[color.r, color.g, color.b]);
        }
        self.client
            .send_frame(&self.rgb, now + Duration::from_millis(250), || {
                cancel.cancelled()
            })
            .map_err(|e| e.to_string())?;
        self.next = Some(Instant::now() + self.period);
        Ok(WriteResult::Submitted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cadence_invalid_frame_and_cancellation_do_not_send_extra_packets() {
        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver.set_nonblocking(true).unwrap();
        let mut driver = Wled::connect(receiver.local_addr().unwrap(), 1).unwrap();
        let cancel = Cancellation::default();
        assert!(driver.write(&[], &cancel).is_err());
        let pixel = [Rgb { r: 1, g: 2, b: 3 }];
        assert!(matches!(
            driver.write(&pixel, &cancel).unwrap(),
            WriteResult::Submitted
        ));
        let mut packet = [0; 32];
        assert_eq!(receiver.recv(&mut packet).unwrap(), 13);
        driver.next = Some(Instant::now() + Duration::from_secs(1));
        assert!(matches!(
            driver.write(&pixel, &cancel).unwrap(),
            WriteResult::Deferred(_)
        ));
        cancel.cancel_for_test();
        assert!(driver.write(&pixel, &cancel).is_err());
        assert!(receiver.recv(&mut packet).is_err());
    }
}

#[cfg(all(test, windows))]
#[path = "network_ha_tests.rs"]
mod ha_tests;
