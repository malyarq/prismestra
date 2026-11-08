//! Network outputs use the same endpoint lease, latest-frame queue and stop/drain as USB.
use super::output::{Cancellation, RgbDriver, WriteResult};
use crate::{devices::Driver, lighting::color::Rgb, transport::wled};
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

pub struct Wled {
    client: wled::Client,
    leds: usize,
    period: Duration,
    next: Option<Instant>,
    rgb: Vec<u8>,
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
