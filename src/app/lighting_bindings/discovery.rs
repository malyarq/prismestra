//! Explicit discovery: metadata only; selected Surplife capacity uses a bounded BLE query.
use super::Draft;
use crate::{
    devices::{Device, Driver},
    runtime::output::Cancellation,
};
use std::{sync::Arc, thread::JoinHandle};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Device(Device),
    Monitor(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub wled: Option<crate::transport::wled_info::Info>,
    pub controller: Option<crate::transport::openrgb::ControllerInfo>,
    pub label: String,
    pub draft: Draft,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inventory {
    pub target: Target,
    pub candidates: Vec<Candidate>,
    pub issues: Vec<String>,
}
impl Inventory {
    pub fn empty(target: Target) -> Self {
        Self {
            target,
            candidates: vec![],
            issues: vec![],
        }
    }
    fn validate(&self, target: &Target) -> Result<(), String> {
        if &self.target != target
            || self.candidates.len() > 128
            || self.issues.len() > 128
            || self.issues.iter().any(|s| s.len() > 8192)
        {
            return Err("Invalid lighting metadata inventory".into());
        }
        let mut keys = std::collections::BTreeSet::new();
        for c in &self.candidates {
            if let Some(info) = &c.wled
                && (!matches!(target, Target::Device(d) if d.driver == Driver::WledDdp)
                    || c.draft.identity != info.stable_id()
                    || info.led_count == 0)
            {
                return Err("WLED candidate metadata does not match its binding".into());
            }
            if let Some(controller) = &c.controller {
                if !matches!(target,Target::Device(d) if d.driver==Driver::OpenRgb)
                    || crate::devices::openrgb::identity(controller) != c.draft.identity
                    || crate::devices::openrgb::topology(controller) != c.draft.serial
                {
                    return Err("OpenRGB candidate metadata does not match its binding".into());
                }
                crate::devices::openrgb::positions(controller)?;
            }
            if c.label.is_empty()
                || c.label.len() > 1024
                || c.label.chars().any(char::is_control)
                || [&c.draft.endpoint, &c.draft.identity, &c.draft.serial]
                    .iter()
                    .any(|s| s.len() > 4096 || s.chars().any(char::is_control))
                || !keys.insert((
                    c.draft.endpoint.to_ascii_lowercase(),
                    c.draft.identity.to_ascii_lowercase(),
                ))
            {
                return Err("Invalid/duplicate lighting metadata candidate".into());
            }
            match target {
                Target::Monitor(_) => crate::capture::validate_name(&c.draft.endpoint)?,
                Target::Device(d)
                    if d.driver == Driver::HomeAssistant
                        && c.draft.identity.is_empty()
                        && c.draft.serial.is_empty() =>
                {
                    crate::devices::home_assistant::canonical_endpoint(&c.draft.endpoint)?;
                }
                Target::Device(d) if matches!(d.driver, Driver::SkydimoSerial { .. }) => {
                    crate::transport::serial::port_path(&c.draft.endpoint)?;
                    if !crate::transport::serial::valid_usb_id(&c.draft.identity) {
                        return Err("Invalid CH340 metadata identity".into());
                    }
                }
                Target::Device(d) => {
                    c.draft.binding(d)?;
                }
            }
        }
        Ok(())
    }
}
pub type Factory = Arc<dyn Fn(Target, Cancellation) -> Result<Inventory, String> + Send + Sync>;
struct Task {
    target: Target,
    cancel: Cancellation,
    thread: JoinHandle<Result<Inventory, String>>,
}
#[derive(Default)]
pub struct Worker {
    task: Option<Task>,
}
impl Worker {
    pub fn start(&mut self, target: Target, factory: Factory) -> Result<(), String> {
        if self.task.is_some() {
            return Err("Предыдущий поиск ещё завершается.".into());
        }
        match &target {
            Target::Device(d) => d.validate()?,
            Target::Monitor(key) if !crate::app::ambient::key(key) => {
                return Err("Invalid monitor source key".into());
            }
            _ => {}
        }
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let requested = target.clone();
        let thread = std::thread::Builder::new()
            .name("lighting-metadata".into())
            .spawn(move || {
                if token.cancelled() {
                    return Err("Поиск отменён.".into());
                }
                let inventory = factory(requested.clone(), token.clone())?;
                if token.cancelled() {
                    return Err("Поиск отменён.".into());
                }
                inventory.validate(&requested)?;
                Ok(inventory)
            })
            .map_err(|e| e.to_string())?;
        self.task = Some(Task {
            target,
            cancel,
            thread,
        });
        Ok(())
    }
    pub fn cancel(&self) {
        if let Some(task) = &self.task {
            task.cancel.cancel();
        }
    }
    pub fn is_idle(&self) -> bool {
        self.task.is_none()
    }
    pub fn poll(&mut self) -> Option<(Target, Result<Inventory, String>)> {
        if !self.task.as_ref()?.thread.is_finished() {
            return None;
        }
        let task = self.task.take().unwrap();
        let result = task
            .thread
            .join()
            .unwrap_or_else(|_| Err("Поиск устройств завершился с ошибкой.".into()));
        Some((task.target, result))
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub fn native_factory() -> Factory {
    Arc::new(native)
}
/// Read-only entity inventory using an explicitly saved local credential.
#[cfg(windows)]
pub fn home_assistant_factory(endpoint: String, token: String) -> Result<Factory, String> {
    use crate::{
        devices::home_assistant as metadata,
        transport::home_assistant::{Client, Endpoint},
    };
    let endpoint = metadata::canonical_endpoint(&endpoint)?;
    let credential = metadata::credential_id(&endpoint)?;
    // Validate credentials without issuing a request. Errors contain no token.
    Client::new(
        Endpoint::parse(&endpoint).map_err(|e| e.to_string())?,
        token.clone(),
    )
    .map_err(|e| e.to_string())?;
    Ok(Arc::new(move |target, cancel| {
        if !matches!(&target, Target::Device(d) if d.driver==Driver::HomeAssistant) {
            return Err("Home Assistant discovery target changed".into());
        }
        let client = Client::new(
            Endpoint::parse(&endpoint).map_err(|e| e.to_string())?,
            token.clone(),
        )
        .map_err(|e| e.to_string())?;
        let lights = client
            .lights(
                std::time::Instant::now() + std::time::Duration::from_secs(3),
                || cancel.cancelled(),
            )
            .map_err(|e| e.to_string())?;
        let mut inventory = Inventory::empty(target);
        if lights.len() > 128 {
            inventory
                .issues
                .push("Показаны первые 128 светильников.".into());
        }
        for light in lights.into_iter().take(128) {
            if !metadata::valid_entity(&light.entity_id) {
                continue;
            }
            let Some(capability) = light.capability() else {
                inventory.issues.push(format!(
                    "{}: неизвестные возможности светильника.",
                    light.entity_id
                ));
                continue;
            };
            if !matches!(light.state.as_deref(), Some("on" | "off")) {
                inventory
                    .issues
                    .push(format!("{}: светильник недоступен.", light.entity_id));
                continue;
            }
            let name = light
                .name
                .unwrap_or_else(|| light.entity_id.clone())
                .chars()
                .filter(|c| !c.is_control())
                .take(100)
                .collect::<String>();
            let modes = capability.label();
            inventory.candidates.push(Candidate {
                wled: None,
                controller: None,
                label: format!("{name} · {} · {modes}", light.entity_id),
                draft: Draft {
                    endpoint: endpoint.clone(),
                    identity: light.entity_id,
                    serial: credential.clone(),
                },
            });
        }
        Ok(inventory)
    }))
}
/// Explicit SDK metadata only. The server address is local preferences, never portable geometry.
pub fn openrgb_factory(endpoint: std::net::SocketAddr) -> Factory {
    Arc::new(move |target, cancel| {
        use crate::{devices::openrgb as metadata, transport::openrgb::OpenRgbSession};
        let Target::Device(device) = &target else {
            return Err("OpenRGB discovery needs a controller device".into());
        };
        if device.driver != Driver::OpenRgb {
            return Err("OpenRGB discovery target changed".into());
        }
        let deadline = || std::time::Instant::now() + std::time::Duration::from_secs(3);
        let cancelled = || cancel.cancelled();
        let mut session = OpenRgbSession::connect(endpoint, deadline(), &cancelled)?;
        let controllers = session.scan(deadline(), &cancelled)?;
        let mut counts = std::collections::BTreeMap::new();
        for c in &controllers {
            *counts.entry(metadata::identity(c)).or_insert(0usize) += 1;
        }
        let mut inventory = Inventory::empty(target.clone());
        for controller in controllers {
            let identity = metadata::identity(&controller);
            let name = controller
                .name
                .chars()
                .filter(|c| !c.is_control())
                .take(80)
                .collect::<String>();
            if counts[&identity] != 1 {
                inventory.issues.push(format!(
                    "{name}: неоднозначная идентичность; вывод запрещён."
                ));
                continue;
            }
            if controller.kind.eq_ignore_ascii_case("virtual") {
                inventory.issues.push(format!("{name}: виртуальный контроллер исключён, чтобы не дублировать физические выходы."));
                continue;
            }
            if controller.leds == 0 || !controller.supports_direct {
                inventory
                    .issues
                    .push(format!("{name}: нет полного Direct-вывода."));
                continue;
            }
            metadata::positions(&controller)?;
            inventory.candidates.push(Candidate {
                wled: None,
                controller: Some(controller.clone()),
                label: format!("{name} · {} LED · {endpoint}", controller.leds),
                draft: Draft {
                    endpoint: endpoint.to_string(),
                    identity,
                    serial: metadata::topology(&controller),
                },
            });
        }
        inventory.issues.truncate(127);
        inventory.issues.push("Выбирайте физический контроллер; общий виртуальный холст вместе с его физическими выходами создаёт конфликт владения.".into());
        inventory.validate(&target)?;
        Ok(inventory)
    })
}
/// Read-only WLED metadata; DDP and HTTP ports are intentionally separate.
#[cfg(windows)]
pub fn wled_factory(endpoint: std::net::SocketAddr, http_endpoint: String) -> Factory {
    wled_factory_until(endpoint, http_endpoint, None)
}
#[cfg(windows)]
fn wled_factory_until(
    endpoint: std::net::SocketAddr,
    http_endpoint: String,
    until: Option<std::time::Instant>,
) -> Factory {
    Arc::new(move |target, cancel| {
        if !matches!(&target, Target::Device(d) if d.driver == Driver::WledDdp) {
            return Err("WLED discovery target changed".into());
        }
        let info = crate::transport::wled_info::Endpoint::parse(&http_endpoint)
            .map_err(|e| e.to_string())?
            .query(
                until
                    .unwrap_or_else(|| {
                        std::time::Instant::now() + std::time::Duration::from_secs(3)
                    })
                    .min(std::time::Instant::now() + std::time::Duration::from_secs(3)),
                || cancel.cancelled(),
            )
            .map_err(|e| e.to_string())?;
        let controller = crate::devices::wled::Controller {
            http_endpoint: http_endpoint.clone(),
            identity: info.stable_id(),
        };
        controller.validate()?;
        let name = info.name.as_deref().unwrap_or("WLED");
        let mut inventory = Inventory::empty(target);
        inventory.candidates.push(Candidate {
            controller: None,
            label: format!("{name} · {} LED · {endpoint}", info.led_count),
            draft: Draft {
                endpoint: endpoint.to_string(),
                identity: controller.identity,
                serial: controller.http_endpoint,
            },
            wled: Some(info),
        });
        Ok(inventory)
    })
}
/// A finite LAN browse. HA server selection precedes authentication and light discovery.
#[cfg(windows)]
pub fn network_factory() -> Factory {
    Arc::new(move |target, cancel| {
        use crate::transport::service_discovery::{self, Kind};
        let kind = match &target {
            Target::Device(d) if d.driver == Driver::WledDdp => Kind::Wled,
            Target::Device(d) if d.driver == Driver::HomeAssistant => Kind::HomeAssistant,
            _ => return Err("Network discovery target changed".into()),
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        let scan = service_discovery::discover(deadline, || cancel.cancelled())
            .map_err(|e| e.to_string())?;
        let mut inventory = Inventory::empty(target.clone());
        for record in scan.records.into_iter().filter(|r| r.kind == kind).take(16) {
            if cancel.cancelled() {
                return Err("Поиск отменён.".into());
            }
            if std::time::Instant::now() >= deadline {
                inventory
                    .issues
                    .push("Время поиска истекло; повторите для остальных устройств.".into());
                break;
            }
            if kind == Kind::Wled {
                let endpoint = std::net::SocketAddr::new(
                    record.address.ip(),
                    crate::transport::wled::DEFAULT_PORT,
                );
                match wled_factory_until(endpoint, record.http_endpoint, Some(deadline))(
                    target.clone(),
                    cancel.clone(),
                ) {
                    Ok(found) => {
                        for candidate in found.candidates {
                            if !inventory
                                .candidates
                                .iter()
                                .any(|c| c.draft.identity == candidate.draft.identity)
                            {
                                inventory.candidates.push(candidate);
                            }
                        }
                    }
                    Err(_) => inventory.issues.push(format!(
                        "{}: WLED не подтвердил метаданные; можно указать адрес вручную.",
                        record.address
                    )),
                }
            } else {
                inventory.candidates.push(Candidate {
                    controller: None,
                    wled: None,
                    label: format!("Home Assistant · {}", record.address),
                    draft: Draft {
                        endpoint: record.http_endpoint,
                        ..Default::default()
                    },
                });
            }
        }
        if scan.truncated {
            inventory
                .issues
                .push("Часть сетевых объявлений не показана.".into());
        }
        if kind == Kind::HomeAssistant {
            inventory.issues.push("Выберите сервер, сохраните ключ и повторите поиск светильников. TLS/прокси может потребовать исправить адрес.".into());
        }
        if inventory.candidates.is_empty() {
            inventory.issues.push("Устройств не найдено. Укажите адрес вручную; mDNS может быть отключён или недоступен между сетями.".into());
        }
        Ok(inventory)
    })
}
#[cfg(windows)]
pub fn surplife_factory(address: u64) -> Factory {
    Arc::new(move |target, cancel| {
        use crate::transport::ble::{Backend, Link};
        if !matches!(&target, Target::Device(d) if d.driver == Driver::SurplifeE1Pixels) {
            return Err("Surplife discovery target changed".into());
        }
        let _lease = crate::runtime::output::Lease::acquire(vec![format!("ble:{address}")])
            .map_err(|_| "Контроллер уже занят выводом. Отключите его привязку перед проверкой возможностей.")?;
        let mut control = crate::runtime::adapters::Control::new(cancel);
        let mut link = crate::transport::ble_windows::WindowsBackend.connect(
            address,
            &mut control,
            &mut |_| {},
        )?;
        let points = link.pixel_capacity(&mut control)?;
        let mut inventory = Inventory::empty(target);
        inventory.candidates.push(Candidate {
            controller: None,
            wled: None,
            label: format!("Surplife E1 · {points} точек · {address:012X}"),
            draft: Draft {
                endpoint: format!("{address:012X}"),
                serial: points.to_string(),
                ..Default::default()
            },
        });
        Ok(inventory)
    })
}
fn native(target: Target, cancel: Cancellation) -> Result<Inventory, String> {
    use crate::{
        lcd::{hid_adapter::Backend as _, hid_policy::Kind},
        transport::{
            feature_hid::{Backend as _, Family},
            serial::Backend as _,
        },
    };
    let mut out = Inventory::empty(target.clone());
    if cancel.cancelled() {
        return Err("Поиск отменён.".into());
    }
    match &target {
        Target::Monitor(_) => {
            for m in crate::capture::windows::monitors()? {
                out.candidates.push(Candidate {
                    wled: None,
                    controller: None,
                    label: format!("{} · {}×{}", m.device_name, m.width, m.height),
                    draft: Draft {
                        endpoint: m.device_name,
                        ..Default::default()
                    },
                });
            }
        }
        Target::Device(d) => {
            d.validate()?;
            match &d.driver {
                Driver::TtLcd=>{
                    for descriptor in crate::lcd::hid_windows::NativeBackend.candidates(Kind::Display)? {
                        if let Err(e)=crate::lcd::hid_policy::validate(Kind::Display,&descriptor){out.issues.push(e);continue;}
                        out.candidates.push(Candidate{wled: None,controller: None,label:format!("TT display MI01 · {}",descriptor.path.chars().take(100).collect::<String>()),draft:Draft{endpoint:descriptor.path,..Default::default()}});
                    }
                }
                Driver::Fusion2{..} | Driver::Valkyrie{..}=>{
                    let family=match &d.driver {
                        Driver::Fusion2{headers}=>Family::Fusion2{headers:*headers},
                        Driver::Valkyrie{model}=>Family::Valkyrie(crate::transport::valkyrie::Model::from_id(model).ok_or("Неизвестная модель клавиатуры")?),
                        _=>unreachable!(),
                    };
                    for path in crate::lcd::hid_windows::metadata_paths(|p|!cancel.cancelled() && family.validate_path(p).is_ok())? {
                        if cancel.cancelled(){return Err("Поиск отменён.".into());}
                        match crate::transport::feature_hid_windows::NativeBackend.inspect(family,&path).and_then(|descriptor|{crate::transport::feature_hid::validate(family,&descriptor)?;Ok(descriptor)}) {
                            Ok(descriptor)=>out.candidates.push(Candidate{wled: None,controller: None,label:format!("USB HID · {}",descriptor.path.chars().take(100).collect::<String>()),draft:Draft{endpoint:descriptor.path,..Default::default()}}),
                            Err(e)=>out.issues.push(e),
                        }
                    }
                }
                Driver::SkydimoSerial{..}=>{
                    for descriptor in crate::transport::serial_windows::WindowsBackend.descriptors()? {
                        if !crate::transport::serial::valid_usb_id(&descriptor.instance_id){continue;}
                        out.candidates.push(Candidate{wled: None,controller: None,label:format!("{} · CH340, Skydimo ещё не подтверждён",descriptor.port),draft:Draft{endpoint:descriptor.port,identity:descriptor.instance_id,serial:String::new()}});
                    }
                    out.issues.push("Порты не открывались. Модель и HEX ID контроллера требуют отдельной проверки; CH340 не означает Skydimo.".into());
                }
                Driver::HomeAssistant=>out.issues.push("Укажите адрес Home Assistant и сохраните ключ, затем найдите светильники.".into()),
                Driver::OpenRgb=>out.issues.push("Укажите SDK-сервер OpenRGB и выполните отдельный read-only поиск контроллеров.".into()),
                Driver::WledDdp=>out.issues.push("WLED: укажите известный IP и порт DDP (обычно 4048). Автопоиск не отправляет команды; UDP-передача не подтверждает физический вывод.".into()),
                Driver::DoorBle | Driver::SurplifeE1Pixels=>out.issues.push("Введите MAC известного совместимого контроллера. Для Surplife E1 перед выводом проверяется модель и число точек; сохранение не включает подсветку.".into()),
                Driver::EneDram=>out.issues.push("Автопоиск ENE/SMBus не выполняется. Нужны путь проверенного модуля PawnIO и адрес 0x70..0x76.".into()),
                Driver::Unsupported{..}=>out.issues.push("Этот протокол пока не поддерживается.".into()),
            }
        }
    }
    out.validate(&target)?;
    Ok(out)
}
