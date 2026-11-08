//! One explicit metadata scan. No RGB reports, serial queries, BLE connects or bus scans.
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
                        out.candidates.push(Candidate{label:format!("TT display MI01 · {}",descriptor.path.chars().take(100).collect::<String>()),draft:Draft{endpoint:descriptor.path,..Default::default()}});
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
                            Ok(descriptor)=>out.candidates.push(Candidate{label:format!("USB HID · {}",descriptor.path.chars().take(100).collect::<String>()),draft:Draft{endpoint:descriptor.path,..Default::default()}}),
                            Err(e)=>out.issues.push(e),
                        }
                    }
                }
                Driver::SkydimoSerial{..}=>{
                    for descriptor in crate::transport::serial_windows::WindowsBackend.descriptors()? {
                        if !crate::transport::serial::valid_usb_id(&descriptor.instance_id){continue;}
                        out.candidates.push(Candidate{label:format!("{} · CH340, Skydimo ещё не подтверждён",descriptor.port),draft:Draft{endpoint:descriptor.port,identity:descriptor.instance_id,serial:String::new()}});
                    }
                    out.issues.push("Порты не открывались. Модель и HEX ID контроллера требуют отдельной проверки; CH340 не означает Skydimo.".into());
                }
                Driver::WledDdp=>out.issues.push("WLED: укажите известный IP и порт DDP (обычно 4048). Автопоиск не отправляет команды; UDP-передача не подтверждает физический вывод.".into()),
                Driver::DoorBle=>out.issues.push("Автопоиск BLE пока не реализован. Введите MAC известного совместимого контроллера; подключение не выполняется при сохранении.".into()),
                Driver::EneDram=>out.issues.push("Автопоиск ENE/SMBus не выполняется. Нужны путь проверенного модуля PawnIO и адрес 0x70..0x76.".into()),
                Driver::Unsupported{..}=>out.issues.push("Этот протокол пока не поддерживается.".into()),
            }
        }
    }
    out.validate(&target)?;
    Ok(out)
}
