//! Explicit DDR5 hub temperature transaction over the pinned PIIX4 module.
//! No scan, SPD data/threshold/enable writes or chipset port selection. Aux1
//! selects only this VM's base; primary's physical port must already be 0.
use super::{
    pawnio,
    pawnio_adapter::{Device, GuardedMutex},
};
use crate::{
    runtime::output::Cancellation,
    sensors::ddr5::{self, Binding},
};
use std::time::{Duration, Instant};

const BYTE_READS: &[u64] = &[0, 1, 5, 6, 0x0b, 0x1a];
const NAME: &[u8] = b"ioctl_smbus_xfer";
fn request(address: u8, read: bool, command: u8, kind: u8) -> [u8; 72] {
    let mut bytes = [0; 72];
    bytes[..NAME.len()].copy_from_slice(NAME);
    for (dst, word) in bytes[32..].as_chunks_mut::<8>().0.iter_mut().zip([
        u64::from(address),
        u64::from(read),
        u64::from(command),
        u64::from(kind),
        0,
    ]) {
        dst.copy_from_slice(&word.to_le_bytes());
    }
    bytes
}
/// Additional native request shape, independent of the ENE 0x70..0x76 policy.
pub(crate) fn permitted_wire(bytes: &[u8], output: usize) -> bool {
    if bytes.len() != 72
        || &bytes[..NAME.len()] != NAME
        || bytes[NAME.len()..32].iter().any(|b| *b != 0)
    {
        return false;
    }
    let w: Vec<_> = bytes[32..]
        .as_chunks::<8>()
        .0
        .iter()
        .map(|b| u64::from_le_bytes(*b))
        .collect();
    (0x50..=0x57).contains(&w[0])
        && w[4] == 0
        && match (w[1], w[2], w[3], output) {
            (1, reg, 2, 8) => BYTE_READS.contains(&reg),
            (1, 0x31, 3, 8) | (0, 0x0b, 2, 0) => true,
            _ => false,
        }
}
fn aux_request() -> [u8; 40] {
    let mut b = [0; 40];
    b[..20].copy_from_slice(b"ioctl_piix4_port_sel");
    b[32..].copy_from_slice(&1u64.to_le_bytes());
    b
}
/// Exact source units, rounded upward to microseconds; zero uses the toolkit's
/// 1ms default. Reserved encodings fail before any page selector write.
fn recovery_us(raw: u8) -> Result<u64, String> {
    let magnitude = match raw >> 4 {
        n @ 0..=10 => u64::from(n),
        11 => 50,
        12 => 100,
        13 => 200,
        14 => 500,
        _ => return Err("Reserved DDR5 recovery magnitude".into()),
    };
    let us = match raw & 3 {
        0 => magnitude.div_ceil(1000),
        1 => magnitude,
        2 => magnitude * 1000,
        _ => return Err("Reserved DDR5 recovery unit".into()),
    };
    Ok(if us == 0 { 1000 } else { us })
}
pub(crate) struct VerifiedDdr5<D: Device, M: GuardedMutex> {
    device: Option<D>,
    mutex: M,
    binding: Binding,
    cancel: Cancellation,
    epoch: u64,
    failure: Option<String>,
}
impl<D: Device, M: GuardedMutex> VerifiedDdr5<D, M> {
    /// The same caller-owned shared guard must cover native open and this load.
    pub(crate) fn connect(
        module: &pawnio::Piix4Module,
        device: D,
        mutex: M,
        binding: Binding,
        cancel: Cancellation,
    ) -> Result<Self, String> {
        binding.validate()?;
        let mut value = Self {
            device: Some(device),
            mutex,
            binding,
            cancel,
            epoch: 0,
            failure: None,
        };
        let result = (|| {
            let epoch = value.mutex.epoch()?;
            value.check(epoch)?;
            value.ioctl(pawnio::IOCTL_LOAD, module.bytes(), &mut [], epoch)?;
            value.ioctl(
                pawnio::IOCTL_EXECUTE,
                &pawnio::always_sleep_request(),
                &mut [],
                epoch,
            )?;
            let mut primary = binding.controller;
            primary[1] = 0x0b00;
            if value.identity(epoch)? != primary {
                return Err("DDR5 complete controller identity differs before port binding".into());
            }
            if binding.port == 1 {
                let old = value.one(&aux_request(), epoch)?;
                if !matches!(old, 0 | 2 | 3 | 4) {
                    return Err("DDR5 unexpected previous primary port".into());
                }
            }
            value.revalidate(epoch)?;
            value.epoch = epoch;
            Ok(())
        })();
        match result {
            Ok(()) => Ok(value),
            Err(e) => Err(value.terminal(e)),
        }
    }
    fn terminal(&mut self, error: String) -> String {
        if self.failure.is_none() {
            self.failure = Some(error.chars().take(512).collect());
        }
        self.device = None;
        self.failure.clone().unwrap()
    }
    fn check(&self, epoch: u64) -> Result<(), String> {
        if self.cancel.cancelled() || epoch == 0 || self.mutex.epoch()? != epoch {
            return Err("DDR5 shared SMBus guard changed or query cancelled".into());
        }
        Ok(())
    }
    fn ioctl(
        &mut self,
        code: u32,
        input: &[u8],
        output: &mut [u8],
        epoch: u64,
    ) -> Result<(), String> {
        self.check(epoch)?;
        output.fill(0);
        let count = self.device.as_mut().ok_or("DDR5 device closed")?.ioctl(
            code,
            input,
            output,
            &self.cancel,
        )?;
        self.check(epoch)?;
        if count != output.len() {
            return Err("DDR5 PawnIO returned an unexpected byte count".into());
        }
        Ok(())
    }
    fn one(&mut self, input: &[u8], epoch: u64) -> Result<u64, String> {
        let mut out = [0; 8];
        self.ioctl(pawnio::IOCTL_EXECUTE, input, &mut out, epoch)?;
        Ok(u64::from_le_bytes(out))
    }
    fn identity(&mut self, epoch: u64) -> Result<[u64; 3], String> {
        let mut out = [0; 24];
        self.ioctl(
            pawnio::IOCTL_EXECUTE,
            &pawnio::identity_request(),
            &mut out,
            epoch,
        )?;
        Ok(std::array::from_fn(|i| {
            u64::from_le_bytes(out[i * 8..i * 8 + 8].try_into().unwrap())
        }))
    }
    fn revalidate(&mut self, epoch: u64) -> Result<(), String> {
        if self.identity(epoch)? != self.binding.controller
            || self.one(&pawnio::query_port_request(), epoch)? != u64::from(self.binding.port)
        {
            return Err("DDR5 controller identity or selected port changed".into());
        }
        Ok(())
    }
    fn byte(&mut self, reg: u8, epoch: u64) -> Result<u8, String> {
        if !BYTE_READS.contains(&u64::from(reg)) {
            return Err("DDR5 byte register outside thermal subset".into());
        }
        u8::try_from(self.one(&request(self.binding.address, true, reg, 2), epoch)?)
            .map_err(|_| "DDR5 returned a non-byte register value".into())
    }
    fn hub(&mut self, epoch: u64) -> Result<(), String> {
        if self.byte(0, epoch)? != 0x51 || self.byte(1, epoch)? != 0x18 {
            return Err("DDR5 hub identity not confirmed; no recovery writes allowed".into());
        }
        if self.byte(5, epoch)? & 2 == 0 || self.byte(0x1a, epoch)? != 0 {
            return Err("DDR5 thermal sensor absent or disabled".into());
        }
        Ok(())
    }
    pub(crate) fn read_checked(
        &mut self,
        mut check_owner: impl FnMut() -> Result<(), String>,
    ) -> Result<u16, String> {
        if let Some(e) = &self.failure {
            return Err(e.clone());
        }
        if self.cancel.cancelled() {
            return Err(self.terminal("DDR5 query cancelled".into()));
        }
        let mutex = self.mutex.clone();
        let _guard = mutex.lock()?;
        let result = (|| {
            let epoch = mutex.epoch()?;
            if epoch <= self.epoch {
                return Err("DDR5 transaction requires a fresh SMBus guard epoch".into());
            }
            self.epoch = epoch;
            check_owner()?;
            self.revalidate(epoch)?;
            self.hub(epoch)?;
            let page = self.byte(0x0b, epoch)?;
            if page > 7 {
                return Err("DDR5 page register has unsupported extra bits".into());
            }
            if page != 0 {
                let delay = recovery_us(self.byte(6, epoch)?)?;
                check_owner()?;
                self.revalidate(epoch)?;
                self.ioctl(
                    pawnio::IOCTL_EXECUTE,
                    &request(self.binding.address, false, 0x0b, 2),
                    &mut [],
                    epoch,
                )?;
                let deadline = Instant::now() + Duration::from_micros(delay);
                while Instant::now() < deadline {
                    self.check(epoch)?;
                    std::thread::sleep(
                        deadline
                            .saturating_duration_since(Instant::now())
                            .min(Duration::from_millis(5)),
                    );
                }
                if self.byte(0x0b, epoch)? != 0 {
                    return Err("DDR5 page zero selection was not confirmed".into());
                }
                self.hub(epoch)?;
            }
            self.revalidate(epoch)?;
            let raw =
                u16::try_from(self.one(&request(self.binding.address, true, 0x31, 3), epoch)?)
                    .map_err(|_| "DDR5 returned a non-word temperature")?;
            self.revalidate(epoch)?;
            check_owner()?;
            Ok(raw)
        })();
        result.map_err(|e| self.terminal(e)) // Close before the guard is dropped.
    }
}
impl<D: Device, M: GuardedMutex> ddr5::Backend for VerifiedDdr5<D, M> {
    fn binding(&self) -> Binding {
        self.binding
    }
    fn read_temperature(&mut self) -> Result<u16, String> {
        self.read_checked(|| Ok(()))
    }
}

#[cfg(test)]
pub(crate) mod tests;
