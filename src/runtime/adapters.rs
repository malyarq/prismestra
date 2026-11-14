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
    protocol: Driver,
    target: u64,
    brightness: i32,
    sequence: u8,
    leds: usize,
}
impl<L: ble::Link> Ble<L> {
    pub fn new(link: L, target: u64, brightness: i32) -> Result<Self, String> {
        Self::with_leds(link, target, brightness, door::COLOR_GROUPS)
    }
    pub fn with_leds(link: L, target: u64, brightness: i32, leds: usize) -> Result<Self, String> {
        if !door::allowed(target)
            || link.address()? != target
            || !(1..=100).contains(&brightness)
            || ![door::COLOR_GROUPS, door::PIXEL_LEDS].contains(&leds)
        {
            return Err("Invalid door binding/brightness".into());
        }
        Ok(Self {
            link,
            protocol: Driver::DoorBle,
            target,
            brightness,
            sequence: 0,
            leds,
        })
    }
    pub fn with_e1(
        mut link: L,
        target: u64,
        brightness: i32,
        leds: usize,
        cancel: &Cancellation,
    ) -> Result<Self, String> {
        if !door::allowed(target)
            || link.address()? != target
            || !(1..=100).contains(&brightness)
            || !(1..=door::MAX_PIXEL_POINTS).contains(&leds)
        {
            return Err("Invalid Surplife E1 binding/capacity".into());
        }
        let actual = link.pixel_capacity(&mut Control::new(cancel.clone()))?;
        if actual != leds {
            return Err(format!(
                "Surplife reports {actual} points; update the device geometry before Apply"
            ));
        }
        Ok(Self {
            link,
            protocol: Driver::SurplifeE1Pixels,
            target,
            brightness,
            sequence: 0,
            leds,
        })
    }
}
impl<L: ble::Link> RgbDriver for Ble<L> {
    fn protocol(&self) -> Driver {
        self.protocol.clone()
    }
    fn leds(&self) -> usize {
        self.leds
    }
    fn write(&mut self, colors: &[Rgb], cancel: &Cancellation) -> Result<WriteResult, String> {
        if cancel.cancelled() {
            return Err("RGB output stopping".into());
        }
        if self.link.address()? != self.target || !self.link.connected()? {
            return Err("Door connection/identity changed".into());
        }
        if colors.len() != self.leds {
            return Err("Door frame does not match its configured LED count".into());
        }
        if self.protocol == Driver::SurplifeE1Pixels {
            self.link.write_e1_pixels(
                colors,
                self.brightness,
                self.sequence,
                &mut Control::new(cancel.clone()),
            )?;
            self.sequence = self.sequence.wrapping_add(1);
            return Ok(WriteResult::Submitted);
        }
        if self.leds == door::PIXEL_LEDS {
            self.link.write_pixels(
                colors,
                self.brightness,
                self.sequence,
                &mut Control::new(cancel.clone()),
            )?;
            self.sequence = self.sequence.wrapping_add(1);
            return Ok(WriteResult::Submitted);
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

#[cfg(test)]
mod ble_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Record {
        capacity: usize,
        legacy: usize,
        pixels: Vec<u8>,
        fail: bool,
    }
    struct Link(Arc<Mutex<Record>>);
    impl ble::Link for Link {
        fn pixel_capacity(&mut self, _: &mut dyn ble::Control) -> Result<usize, String> {
            Ok(self.0.lock().unwrap().capacity)
        }
        fn write_e1_pixels(
            &mut self,
            colors: &[Rgb],
            _: i32,
            sequence: u8,
            _: &mut dyn ble::Control,
        ) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            if colors.len() != state.capacity {
                return Err("Capacity changed".into());
            }
            state.pixels.push(sequence);
            Ok(())
        }
        fn address(&self) -> Result<u64, String> {
            Ok(1)
        }
        fn connected(&self) -> Result<bool, String> {
            Ok(true)
        }
        fn max_pdu(&self) -> Result<u32, String> {
            Ok(200)
        }
        fn write(&mut self, packet: &[u8], _: &mut dyn ble::Control) -> Result<(), String> {
            assert!(ble::valid_packet(packet));
            self.0.lock().unwrap().legacy += 1;
            Ok(())
        }
        fn write_pixels(
            &mut self,
            colors: &[Rgb],
            _: i32,
            sequence: u8,
            _: &mut dyn ble::Control,
        ) -> Result<(), String> {
            assert_eq!(colors.len(), door::PIXEL_LEDS);
            let mut state = self.0.lock().unwrap();
            state.pixels.push(sequence);
            if state.fail {
                Err("Incomplete pixel frame".into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn ble_pixel_dispatch_checks_length_cancel_and_complete_submission_before_sequence() {
        let state = Arc::new(Mutex::new(Record::default()));
        let mut pixels = Ble::with_leds(Link(state.clone()), 1, 100, door::PIXEL_LEDS).unwrap();
        assert_eq!(pixels.leds(), 153);
        let colors = vec![Rgb { r: 0, g: 255, b: 0 }; 153];
        let cancel = Cancellation::default();
        assert!(pixels.write(&colors[..152], &cancel).is_err());
        assert!(state.lock().unwrap().pixels.is_empty());
        state.lock().unwrap().fail = true;
        assert!(pixels.write(&colors, &cancel).is_err());
        state.lock().unwrap().fail = false;
        pixels.write(&colors, &cancel).unwrap();
        pixels.write(&colors, &cancel).unwrap();
        assert_eq!(state.lock().unwrap().pixels, vec![0, 0, 1]);
        cancel.cancel_for_test();
        assert!(pixels.write(&colors, &cancel).is_err());
        assert_eq!(state.lock().unwrap().pixels.len(), 3);
        let mut legacy = Ble::new(Link(state.clone()), 1, 100).unwrap();
        legacy
            .write(&colors[..20], &Cancellation::default())
            .unwrap();
        assert_eq!(state.lock().unwrap().legacy, 1);
        assert!(Ble::with_leds(Link(state), 1, 100, 152).is_err());
    }
    #[test]
    fn e1_requires_reported_capacity_before_any_output() {
        let state = Arc::new(Mutex::new(Record {
            capacity: 152,
            ..Default::default()
        }));
        let cancel = Cancellation::default();
        assert!(Ble::with_e1(Link(state.clone()), 1, 60, 153, &cancel).is_err());
        assert!(state.lock().unwrap().pixels.is_empty());
        let mut driver = Ble::with_e1(Link(state.clone()), 1, 60, 152, &cancel).unwrap();
        assert_eq!(driver.protocol(), Driver::SurplifeE1Pixels);
        assert!(driver.write(&vec![Rgb::default(); 153], &cancel).is_err());
        driver.write(&vec![Rgb::default(); 152], &cancel).unwrap();
        assert_eq!(state.lock().unwrap().pixels, vec![0]);
    }
}
