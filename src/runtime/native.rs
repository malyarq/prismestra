//! Explicit factories only: no native activation until called on an output worker.
use super::{
    adapters::{self, Control},
    output::{Cancellation, RgbDriver},
};
use crate::transport::{
    ble, ble_windows, feature_hid, feature_hid_windows, pawnio_windows, serial, serial_windows,
    valkyrie,
};
/// Explicit user activation from already validated local bindings. Does not
/// infer replacements or guess a protocol from a label. No caller in stand mode.
pub type Factory =
    std::sync::Arc<dyn Fn(Cancellation) -> Result<Box<dyn RgbDriver>, String> + Send + Sync>;

pub fn attach_bound(
    host: &mut super::Host,
    id: &str,
    bindings: &crate::devices::Bindings,
    door_brightness: i32,
) -> Result<(), String> {
    let (keys, factory) = bound_factory(host.setup(), id, bindings, door_brightness)?;
    host.attach_claimed(id, keys, move |cancel| factory(cancel))
}

/// Build a repeatable deferred factory for Runtime retry/resume. Validation
/// performs no device enumeration/open or driver/WinRT calls.
pub fn bound_factory(
    setup: &crate::devices::Setup,
    id: &str,
    bindings: &crate::devices::Bindings,
    door_brightness: i32,
) -> Result<(Vec<String>, Factory), String> {
    use crate::devices::{Binding, Driver, validate_bindings};
    use std::sync::Arc;
    validate_bindings(setup, bindings)?;
    let device = setup
        .devices
        .iter()
        .find(|d| d.id == id)
        .ok_or("Unknown logical device")?
        .clone();
    if !device.enabled {
        return Err("Device is disabled".into());
    }
    let binding = bindings
        .get(id)
        .ok_or("Device has no explicit local binding")?
        .clone();
    let keys = binding.endpoint_keys();
    let factory: Factory = match (device.driver, binding) {
        (
            Driver::HomeAssistant,
            Binding::HomeAssistant {
                endpoint,
                entity,
                credential_id,
            },
        ) => Arc::new(move |cancel| {
            Ok(Box::new(super::network::HomeAssistant::connect(
                &endpoint,
                &entity,
                &credential_id,
                &cancel,
            )?))
        }),
        (
            Driver::OpenRgb,
            Binding::OpenRgb {
                endpoint,
                identity,
                topology,
            },
        ) => Arc::new(move |cancel| {
            Ok(Box::new(super::network::OpenRgb::connect(
                endpoint,
                &identity,
                &topology,
                device.leds,
                &cancel,
            )?))
        }),
        (
            Driver::WledDdp,
            Binding::WledDdp {
                endpoint,
                controller,
            },
        ) => Arc::new(move |cancel| {
            if cancel.cancelled() {
                return Err("RGB output stopping".into());
            }
            if let Some(controller) = &controller {
                let info = crate::transport::wled_info::Endpoint::parse(&controller.http_endpoint)
                    .map_err(|e| e.to_string())?
                    .query(
                        std::time::Instant::now() + std::time::Duration::from_secs(3),
                        || cancel.cancelled(),
                    )
                    .map_err(|e| e.to_string())?;
                controller.verify(&info, device.leds)?;
            }
            Ok(Box::new(super::network::Wled::connect(
                endpoint,
                device.leds,
            )?))
        }),
        (
            Driver::SkydimoSerial { model },
            Binding::Serial {
                port,
                instance_id,
                serial: identity,
            },
        ) => {
            let binding = serial::Binding {
                descriptor: serial::Descriptor { port, instance_id },
                expected: crate::transport::skydimo::Identity {
                    model,
                    serial: identity,
                },
                leds: device.leds,
            };
            Arc::new(move |cancel| serial(binding.clone())(cancel))
        }
        (Driver::DoorBle, Binding::Ble { address }) => {
            if !(1..=100).contains(&door_brightness) {
                return Err("Invalid door brightness".into());
            }
            let leds = device.leds;
            Arc::new(move |cancel| door(address, door_brightness, leds)(cancel))
        }
        (Driver::SurplifeE1Pixels, Binding::Ble { address }) => Arc::new(move |cancel| {
            use ble::Backend;
            let link = ble_windows::WindowsBackend.connect(
                address,
                &mut Control::new(cancel.clone()),
                &mut |_| {},
            )?;
            Ok(Box::new(adapters::Ble::with_e1(
                link,
                address,
                door_brightness,
                device.leds,
                &cancel,
            )?))
        }),
        (Driver::Fusion2 { headers }, Binding::Hid { path }) => Arc::new(move |cancel| {
            let verified = feature_hid::open(
                &mut feature_hid_windows::NativeBackend,
                feature_hid::Family::Fusion2 { headers },
                &path,
                cancel,
            )?;
            Ok(Box::new(adapters::Fusion2::new(verified, headers)?))
        }),
        (Driver::Valkyrie { model }, Binding::Hid { path }) => {
            let model = valkyrie::Model::from_id(&model).ok_or("Unsupported keyboard model")?;
            Arc::new(move |cancel| {
                let verified = feature_hid::open(
                    &mut feature_hid_windows::NativeBackend,
                    feature_hid::Family::Valkyrie(model),
                    &path,
                    cancel,
                )?;
                let d = verified.descriptor();
                let descriptor = valkyrie::Descriptor {
                    path: d.path.clone(),
                    vid: d.vid,
                    pid: d.pid,
                    interface: model.interface(),
                    usage_page: d.usage_page,
                    usage: d.usage,
                    feature_len: d.feature_len,
                    feature_ids: d.feature_ids.clone(),
                    feature_caps_complete: d.feature_caps_complete,
                };
                Ok(Box::new(adapters::Valkyrie::new(
                    model,
                    &descriptor,
                    verified,
                )?))
            })
        }
        (
            Driver::EneDram,
            Binding::EnePiix4 {
                module_path,
                address,
            },
        ) => Arc::new(move |cancel| {
            let bus =
                pawnio_windows::connect_bound(std::path::Path::new(&module_path), address, cancel)?;
            Ok(Box::new(adapters::Ene::new(bus, address)?))
        }),
        _ => {
            return Err(
                "This device requires an unsupported or separate non-RGB controller".into(),
            );
        }
    };
    Ok((keys, factory))
}
pub(crate) fn serial(
    binding: serial::Binding,
) -> impl FnOnce(Cancellation) -> Result<Box<dyn RgbDriver>, String> + Send + 'static {
    move |cancel| {
        if cancel.cancelled() {
            return Err("RGB output stopping".into());
        }
        let controller = serial::connect(&mut serial_windows::WindowsBackend, &binding)?;
        Ok(Box::new(adapters::Serial::new(
            controller,
            binding.expected.model,
            binding.leds,
        )?))
    }
}
pub(crate) fn door(
    address: u64,
    brightness: i32,
    leds: usize,
) -> impl FnOnce(Cancellation) -> Result<Box<dyn RgbDriver>, String> + Send + 'static {
    move |cancel| {
        use ble::Backend;
        if !crate::lighting::door::allowed(address)
            || !(1..=100).contains(&brightness)
            || ![
                crate::lighting::door::COLOR_GROUPS,
                crate::lighting::door::PIXEL_LEDS,
            ]
            .contains(&leds)
        {
            return Err("Invalid door binding/brightness".into());
        }
        let link =
            ble_windows::WindowsBackend.connect(address, &mut Control::new(cancel), &mut |_| {})?;
        Ok(Box::new(adapters::Ble::with_leds(
            link, address, brightness, leds,
        )?))
    }
}
