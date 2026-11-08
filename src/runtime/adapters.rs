//! The same driver adapters run against native links and memory-only stand links.
use super::output::{Cancellation, RgbDriver, WriteResult};
use crate::{
    devices::{Device, Driver, Role},
    lighting::{color::Rgb, door},
    transport::{ble, ene, fusion2, serial, valkyrie},
};
use std::time::{Duration, Instant};

/// Both headers share a single controller, queue, endpoint lease and lifetime.
/// A frame is submitted only after both channel transfers finish successfully.
pub struct Fusion2<T: fusion2::Transport> {
    controller: fusion2::Controller<T>,
    headers: [usize; 2],
}
impl<T: fusion2::Transport> Fusion2<T> {
    pub fn new(transport: T, headers: [usize; 2]) -> Result<Self, String> {
        Ok(Self {
            controller: fusion2::Controller::connect(transport, headers)?,
            headers,
        })
    }
}
impl<T: fusion2::Transport> RgbDriver for Fusion2<T> {
    fn protocol(&self) -> Driver {
        Driver::Fusion2 {
            headers: self.headers,
        }
    }
    fn leds(&self) -> usize {
        self.headers.iter().sum()
    }
    fn write(&mut self, colors: &[Rgb], cancel: &Cancellation) -> Result<WriteResult, String> {
        if colors.len() != self.leds() {
            return Err("Fusion2 complete frame size changed".into());
        }
        if cancel.cancelled() {
            return Err("RGB output stopping".into());
        }
        self.controller.update(0, &colors[..self.headers[0]])?;
        if cancel.cancelled() {
            return Err("RGB output stopping between owned headers".into());
        }
        self.controller.update(1, &colors[self.headers[0]..])?;
        Ok(WriteResult::Submitted)
    }
}
pub struct Ene<B: ene::Bus> {
    controller: ene::Controller<B>,
}
impl<B: ene::Bus> Ene<B> {
    pub fn new(bus: B, address: u8) -> Result<Self, String> {
        Ok(Self {
            controller: ene::Controller::connect(bus, address)?,
        })
    }
}
impl<B: ene::Bus> RgbDriver for Ene<B> {
    fn protocol(&self) -> Driver {
        Driver::EneDram
    }
    fn leds(&self) -> usize {
        ene::LEDS
    }
    fn write(&mut self, colors: &[Rgb], cancel: &Cancellation) -> Result<WriteResult, String> {
        if cancel.cancelled() {
            return Err("RGB output stopping".into());
        }
        self.controller.frame(colors)?;
        Ok(WriteResult::Submitted)
    }
}
pub struct Valkyrie<T: valkyrie::Transport> {
    controller: valkyrie::Controller<T>,
    model: valkyrie::Model,
}
impl<T: valkyrie::Transport> Valkyrie<T> {
    pub fn new(
        model: valkyrie::Model,
        descriptor: &valkyrie::Descriptor,
        transport: T,
    ) -> Result<Self, String> {
        Ok(Self {
            controller: valkyrie::Controller::new(model, descriptor, transport)?,
            model,
        })
    }
}
impl<T: valkyrie::Transport> RgbDriver for Valkyrie<T> {
    fn protocol(&self) -> Driver {
        Driver::Valkyrie {
            model: self.model.id().into(),
        }
    }
    fn leds(&self) -> usize {
        self.model.leds()
    }
    fn write(&mut self, colors: &[Rgb], cancel: &Cancellation) -> Result<WriteResult, String> {
        if cancel.cancelled() {
            return Err("RGB output stopping".into());
        }
        self.controller.frame(colors).map_err(|e| e.to_string())?;
        Ok(WriteResult::Submitted)
    }
}
pub(crate) struct Control {
    cancel: Cancellation,
    start: Instant,
}
impl Control {
    pub(crate) fn new(cancel: Cancellation) -> Self {
        Self {
            cancel,
            start: Instant::now(),
        }
    }
}
impl ble::Control for Control {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
    fn cancelled(&self) -> bool {
        self.cancel.cancelled()
    }
    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration.min(ble::POLL));
    }
}
pub struct Serial<L: serial::Link> {
    controller: serial::Controller<L>,
    model: String,
    leds: usize,
}
impl<L: serial::Link> Serial<L> {
    pub fn new(
        controller: serial::Controller<L>,
        model: String,
        leds: usize,
    ) -> Result<Self, String> {
        Device {
            id: "serial".into(),
            label: "Serial".into(),
            role: Role::Canvas,
            driver: Driver::SkydimoSerial {
                model: model.clone(),
            },
            leds,
            rate_hz: 1,
            enabled: true,
        }
        .validate()?;
        Ok(Self {
            controller,
            model,
            leds,
        })
    }
}
impl<L: serial::Link> RgbDriver for Serial<L> {
    fn protocol(&self) -> Driver {
        Driver::SkydimoSerial {
            model: self.model.clone(),
        }
    }
    fn leds(&self) -> usize {
        self.leds
    }
    fn write(&mut self, colors: &[Rgb], cancel: &Cancellation) -> Result<WriteResult, String> {
        if cancel.cancelled() {
            return Err("RGB output stopping".into());
        }
        match self.controller.send(colors)? {
            serial::Submission::Submitted => Ok(WriteResult::Submitted),
            serial::Submission::Throttled { retry_us } => Ok(WriteResult::Deferred(
                Duration::from_micros(retry_us.saturating_sub(self.controller.now_us()?).max(1)),
            )),
        }
    }
}
pub struct Ble<L: ble::Link> {
    link: L,
    target: u64,
    brightness: i32,
    sequence: u8,
}
impl<L: ble::Link> Ble<L> {
    pub fn new(link: L, target: u64, brightness: i32) -> Result<Self, String> {
        if !door::allowed(target) || link.address()? != target || !(1..=100).contains(&brightness) {
            return Err("Invalid door binding/brightness".into());
        }
        Ok(Self {
            link,
            target,
            brightness,
            sequence: 0,
        })
    }
}
impl<L: ble::Link> RgbDriver for Ble<L> {
    fn protocol(&self) -> Driver {
        Driver::DoorBle
    }
    fn leds(&self) -> usize {
        20
    }
    fn write(&mut self, colors: &[Rgb], cancel: &Cancellation) -> Result<WriteResult, String> {
        if cancel.cancelled() {
            return Err("RGB output stopping".into());
        }
        if self.link.address()? != self.target || !self.link.connected()? {
            return Err("Door connection/identity changed".into());
        }
        let colors: &[Rgb; 20] = colors
            .try_into()
            .map_err(|_| "Door output requires20groups")?;
        let packet =
            door::frame_packet(colors, self.brightness, self.sequence).map_err(str::to_owned)?;
        if !door::fits(packet.len(), self.link.max_pdu()?) {
            return Err("Door packet exceeds ATT payload; no fragments sent".into());
        }
        self.link
            .write(&packet, &mut Control::new(cancel.clone()))?;
        self.sequence = self.sequence.wrapping_add(1);
        Ok(WriteResult::Submitted)
    }
}
