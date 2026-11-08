// SPDX-License-Identifier: GPL-2.0-or-later
// Explicitly invoked Windows backend. Construction/import alone performs no WinRT/device calls.
use super::ble::{self, Backend, Control, Event, Link};
use std::{
    marker::PhantomData,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use windows::{
    Devices::Bluetooth::{
        Advertisement::{
            BluetoothLEAdvertisementReceivedEventArgs, BluetoothLEAdvertisementWatcher,
            BluetoothLEAdvertisementWatcherStatus, BluetoothLEScanningMode,
        },
        BluetoothCacheMode, BluetoothConnectionStatus, BluetoothLEDevice,
        GenericAttributeProfile::{
            GattCharacteristic, GattCharacteristicProperties, GattCommunicationStatus,
            GattDeviceService, GattSession, GattWriteOption,
        },
    },
    Foundation::TypedEventHandler,
    Storage::Streams::DataWriter,
    Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
    core::GUID,
};

fn error(e: windows::core::Error) -> String {
    format!("WinRT error: {e}; disconnected")
}
fn uuid(short: u16) -> GUID {
    GUID::from_u128((short as u128) << 96 | 0x00001000800000805f9b34fb)
}

// Macro preserves each operation's concrete projected type without requiring windows-future publicly.
macro_rules! bounded {
    ($operation:expr, $control:expr) => {{
        let started = $control.now();
        ble::check_wait($control, started)?;
        let operation = $operation.map_err(error)?;
        loop {
            let status = match operation.Status() {
                Ok(s) => s,
                Err(e) => {
                    let _ = operation.Cancel();
                    return Err(error(e));
                }
            };
            if status.0 != 0 {
                break;
            }
            if let Err(e) = ble::check_wait($control, started) {
                let _ = operation.Cancel();
                return Err(e);
            }
            $control.sleep(ble::POLL);
        }
        operation.GetResults()
    }};
}

struct Apartment(PhantomData<Rc<()>>);
impl Apartment {
    fn new() -> Result<Self, String> {
        // Balanced on the same thread, including an S_FALSE nested initialization.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(error)?;
        Ok(Self(PhantomData))
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}

#[derive(Default)]
pub struct WindowsBackend;
pub struct WindowsLink {
    characteristic: Option<GattCharacteristic>,
    service: Option<GattDeviceService>,
    session: Option<GattSession>,
    device: Option<BluetoothLEDevice>,
    _apartment: Apartment,
}
impl Drop for WindowsLink {
    fn drop(&mut self) {
        self.characteristic.take();
        if let Some(s) = self.service.take() {
            let _ = s.Close();
        }
        if let Some(s) = self.session.take() {
            let _ = s.Close();
        }
        if let Some(d) = self.device.take() {
            let _ = d.Close();
        }
    }
}
struct Discovery {
    watcher: BluetoothLEAdvertisementWatcher,
    token: Option<i64>,
}
impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.watcher.Stop();
        if let Some(t) = self.token.take() {
            let _ = self.watcher.RemoveReceived(t);
        }
    }
}
fn discover(target: u64, control: &mut dyn Control) -> Result<(), String> {
    let seen = Arc::new(AtomicBool::new(false));
    let watcher = BluetoothLEAdvertisementWatcher::new().map_err(error)?;
    let mut scan = Discovery {
        watcher,
        token: None,
    };
    scan.watcher
        .SetScanningMode(BluetoothLEScanningMode::Passive)
        .map_err(error)?;
    let callback_seen = seen.clone();
    let callback = TypedEventHandler::<
        BluetoothLEAdvertisementWatcher,
        BluetoothLEAdvertisementReceivedEventArgs,
    >::new(move |_, args| {
        // Callback owns only target + shared flag, never worker or stack; safe after revocation.
        if let Some(args) = args.as_ref()
            && args.BluetoothAddress().ok() == Some(target)
        {
            callback_seen.store(true, Ordering::Release);
        }
        Ok(())
    });
    scan.token = Some(scan.watcher.Received(&callback).map_err(error)?);
    let started = control.now();
    ble::check_wait(control, started)?;
    scan.watcher.Start().map_err(error)?;
    while !seen.load(Ordering::Acquire) {
        if control.cancelled() {
            return Err("Fixed-target discovery cancelled; disconnected".into());
        }
        if control.now().saturating_sub(started) >= ble::TIMEOUT {
            return Err("Fixed-target discovery timed out after 8 seconds; disconnected".into());
        }
        if scan.watcher.Status().map_err(error)? == BluetoothLEAdvertisementWatcherStatus::Aborted {
            return Err("Fixed-target passive discovery aborted; disconnected".into());
        }
        control.sleep(ble::POLL);
    }
    if control.cancelled() {
        return Err("Fixed-target discovery cancelled; disconnected".into());
    }
    Ok(())
}
fn lookup(target: u64, control: &mut dyn Control) -> Result<Option<BluetoothLEDevice>, String> {
    ble::check_wait(control, control.now())?;
    let operation = BluetoothLEDevice::FromBluetoothAddressAsync(target).map_err(error)?;
    match bounded!(Ok::<_, windows::core::Error>(operation.clone()), control) {
        Ok(d) => Ok(Some(d)),
        // windows-core 0.62 Type::from_abi(null) returns Error::empty (code S_OK).
        // Only a completed operation with that projection error is a cache miss.
        Err(e) if e.code().0 == 0 && operation.Status().ok().is_some_and(|s| s.0 == 1) => Ok(None),
        Err(e) => Err(error(e)),
    }
}
impl Backend for WindowsBackend {
    type Link = WindowsLink;
    fn connect(
        &mut self,
        target: u64,
        control: &mut dyn Control,
        emit: &mut dyn FnMut(Event),
    ) -> Result<WindowsLink, String> {
        if !crate::lighting::door::allowed(target) {
            return Err("Target not allowed".into());
        }
        ble::check_wait(control, control.now())?;
        let mut link = WindowsLink {
            characteristic: None,
            service: None,
            session: None,
            device: None,
            _apartment: Apartment::new()?,
        };
        link.device = lookup(target, control)?;
        if link.device.is_none() {
            emit(Event::Status(
                "Fixed target absent from Windows BLE cache; passive discovery (up to 8 seconds)"
                    .into(),
            ));
            discover(target, control)?;
            emit(Event::Status(
                "Fixed target discovered; retrying Windows BLE address cache once".into(),
            ));
            link.device = lookup(target, control)?;
        }
        let Some(device) = link.device.as_ref() else {
            return Err("Fixed target unavailable after cache lookup".into());
        };
        if device.BluetoothAddress().map_err(error)? != target {
            return Err("Fixed target unavailable after cache lookup".into());
        }
        let id = device.BluetoothDeviceId().map_err(error)?;
        link.session = Some(bounded!(GattSession::FromDeviceIdAsync(&id), control).map_err(error)?);
        let services = bounded!(
            device.GetGattServicesForUuidWithCacheModeAsync(
                uuid(0xffff),
                BluetoothCacheMode::Uncached
            ),
            control
        )
        .map_err(error)?;
        let service_list = services.Services().map_err(error)?;
        ble::validate_service(
            services.Status().map_err(error)? == GattCommunicationStatus::Success,
            service_list.Size().map_err(error)?,
        )?;
        link.service = Some(service_list.GetAt(0).map_err(error)?);
        let chars = bounded!(
            link.service
                .as_ref()
                .unwrap()
                .GetCharacteristicsForUuidWithCacheModeAsync(
                    uuid(0xff01),
                    BluetoothCacheMode::Uncached
                ),
            control
        )
        .map_err(error)?;
        let list = chars.Characteristics().map_err(error)?;
        let count = list.Size().map_err(error)?;
        // Validate uniqueness before indexing. No fallback to an arbitrary service/characteristic.
        ble::validate_characteristic(
            chars.Status().map_err(error)? == GattCommunicationStatus::Success,
            count,
            true,
        )?;
        let characteristic = list.GetAt(0).map_err(error)?;
        ble::validate_characteristic(
            true,
            1,
            (characteristic.CharacteristicProperties().map_err(error)?
                & GattCharacteristicProperties::WriteWithoutResponse)
                != GattCharacteristicProperties::None,
        )?;
        link.characteristic = Some(characteristic);
        Ok(link)
    }
}
impl Link for WindowsLink {
    fn address(&self) -> Result<u64, String> {
        self.device
            .as_ref()
            .unwrap()
            .BluetoothAddress()
            .map_err(error)
    }
    fn connected(&self) -> Result<bool, String> {
        Ok(self
            .device
            .as_ref()
            .unwrap()
            .ConnectionStatus()
            .map_err(error)?
            == BluetoothConnectionStatus::Connected)
    }
    fn max_pdu(&self) -> Result<u32, String> {
        Ok(self.session.as_ref().unwrap().MaxPduSize().map_err(error)? as u32)
    }
    fn write(&mut self, packet: &[u8], control: &mut dyn Control) -> Result<(), String> {
        if !ble::valid_packet(packet) {
            return Err("Packet is not an audited door command; no bytes sent".into());
        }
        if control.cancelled() {
            return Err("Operation cancelled or timed out; disconnected".into());
        }
        if !self.connected()? {
            return Err("BLE link lost; disconnected".into());
        }
        if !crate::lighting::door::fits(packet.len(), self.max_pdu()?) {
            return Err("Packet exceeds negotiated ATT payload; no fragments sent".into());
        }
        let writer = DataWriter::new().map_err(error)?;
        let buffer = (|| {
            writer.WriteBytes(packet)?;
            writer.DetachBuffer()
        })();
        let _ = writer.Close();
        let buffer = buffer.map_err(error)?;
        let result = bounded!(
            self.characteristic
                .as_ref()
                .unwrap()
                .WriteValueWithResultAndOptionAsync(&buffer, GattWriteOption::WriteWithoutResponse),
            control
        )
        .map_err(error)?;
        if result.Status().map_err(error)? != GattCommunicationStatus::Success {
            return Err("GATT write failed; disconnected".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    struct Clock {
        time: Duration,
        cancel: bool,
    }
    impl Control for Clock {
        fn now(&self) -> Duration {
            self.time
        }
        fn cancelled(&self) -> bool {
            self.cancel
        }
        fn sleep(&mut self, d: Duration) {
            assert_eq!(d, ble::POLL);
            self.time += d;
        }
    }
    struct Status(i32);
    struct FakeOperation {
        cancelled: Arc<AtomicBool>,
        completed: bool,
    }
    #[allow(non_snake_case)]
    impl FakeOperation {
        fn Status(&self) -> windows::core::Result<Status> {
            Ok(Status(if self.completed { 1 } else { 0 }))
        }
        fn Cancel(&self) -> windows::core::Result<()> {
            self.cancelled.store(true, Ordering::Release);
            Ok(())
        }
        fn GetResults(&self) -> windows::core::Result<()> {
            assert!(self.completed);
            Ok(())
        }
    }
    fn wait(operation: FakeOperation, control: &mut dyn Control) -> Result<(), String> {
        bounded!(Ok::<_, windows::core::Error>(operation), control).map_err(error)
    }
    #[test]
    fn native_wait_macro_is_bounded_and_cancels_without_any_winrt_operation() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut clock = Clock {
            time: Duration::ZERO,
            cancel: false,
        };
        assert!(
            wait(
                FakeOperation {
                    cancelled: cancelled.clone(),
                    completed: false
                },
                &mut clock
            )
            .is_err()
        );
        assert_eq!(clock.time, ble::TIMEOUT);
        assert!(cancelled.load(Ordering::Acquire));
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut clock = Clock {
            time: Duration::ZERO,
            cancel: false,
        };
        wait(
            FakeOperation {
                cancelled: cancelled.clone(),
                completed: true,
            },
            &mut clock,
        )
        .unwrap();
        assert_eq!(clock.time, Duration::ZERO);
        assert!(!cancelled.load(Ordering::Acquire));
    }
    #[test]
    fn cancellation_of_pending_operation_is_polled_in_25_ms() {
        struct CancelClock(Duration);
        impl Control for CancelClock {
            fn now(&self) -> Duration {
                self.0
            }
            fn cancelled(&self) -> bool {
                self.0 >= ble::POLL
            }
            fn sleep(&mut self, d: Duration) {
                self.0 += d;
            }
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut clock = CancelClock(Duration::ZERO);
        assert!(
            wait(
                FakeOperation {
                    cancelled: cancelled.clone(),
                    completed: false
                },
                &mut clock
            )
            .is_err()
        );
        assert_eq!(clock.0, ble::POLL);
        assert!(cancelled.load(Ordering::Acquire));
    }
    #[test]
    fn null_projection_and_endpoint_uuid_are_source_verified_without_activation() {
        let missing = unsafe {
            <BluetoothLEDevice as windows::core::Type<BluetoothLEDevice>>::from_abi(
                std::ptr::null_mut(),
            )
        };
        assert_eq!(missing.unwrap_err().code().0, 0);
        assert_eq!(
            uuid(0xffff),
            GUID::from_u128(0x0000ffff00001000800000805f9b34fb)
        );
        assert_eq!(
            uuid(0xff01),
            GUID::from_u128(0x0000ff0100001000800000805f9b34fb)
        );
    }
}
