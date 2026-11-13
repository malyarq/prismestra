//! Portable, transactional device/map edits. No bindings or device access.
use super::{
    document::{Document, Lcd},
    lcd,
};
use crate::{
    devices::{Device, Driver, MAX_POINTS, Point, Role},
    schedule::Target,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Line {
        x0: u32,
        y0: u32,
        x1: u32,
        y1: u32,
    },
    Grid {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        columns: usize,
        serpentine: bool,
    },
}
#[derive(Clone, Copy, Debug)]
enum Family {
    HomeAssistant,
    OpenRgb,
    Wled,
    Door,
    Lcd,
    Ene,
    Valkyrie(&'static str),
    Fusion([usize; 2]),
    Skydimo(&'static str),
}
#[derive(Clone, Copy, Debug)]
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    family: Family,
    leds: usize,
}
// Counts are the existing protocol capability tables, not discovered hardware.
static BASE_PRESETS: &[Preset] = &[
    Preset {
        id: "home-assistant-light",
        label: "Home Assistant · свет",
        family: Family::HomeAssistant,
        leds: 1,
    },
    Preset {
        id: "openrgb-controller",
        label: "OpenRGB · whole controller",
        family: Family::OpenRgb,
        leds: 1,
    },
    Preset {
        id: "wled-ddp",
        label: "WLED · DDP",
        family: Family::Wled,
        leds: 60,
    },
    Preset {
        id: "door20",
        label: "Surplife BLE · 20 групп",
        family: Family::Door,
        leds: 20,
    },
    Preset {
        id: "door153",
        label: "Surplife BLE · 153 LED",
        family: Family::Door,
        leds: crate::lighting::door::PIXEL_LEDS,
    },
    Preset {
        id: "tt-lcd",
        label: "TT LCD",
        family: Family::Lcd,
        leds: 0,
    },
    Preset {
        id: "ene8",
        label: "ENE DRAM · 8",
        family: Family::Ene,
        leds: 8,
    },
    Preset {
        id: "vk99",
        label: "Valkyrie VK99 · 102",
        family: Family::Valkyrie("vk99"),
        leds: 102,
    },
    Preset {
        id: "vk99-pro",
        label: "Valkyrie VK99 Pro · 98",
        family: Family::Valkyrie("vk99-pro"),
        leds: 98,
    },
    Preset {
        id: "fusion2-27-27",
        label: "Fusion2 · 27 + 27",
        family: Family::Fusion([27, 27]),
        leds: 54,
    },
];
static PRESETS: std::sync::LazyLock<Vec<Preset>> = std::sync::LazyLock::new(|| {
    let mut presets = BASE_PRESETS.to_vec();
    presets.extend(crate::transport::skydimo::MODELS.iter().map(|m| Preset {
        id: m.id,
        label: m.id,
        family: Family::Skydimo(m.id),
        leds: m.leds,
    }));
    presets
});
pub fn catalog() -> &'static [Preset] {
    &PRESETS
}
pub fn preset(id: &str) -> Result<&'static Preset, String> {
    PRESETS
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| "Неизвестный шаблон устройства".into())
}
impl Preset {
    pub fn target(&self) -> Target {
        if matches!(self.family, Family::Door) {
            Target::Door
        } else {
            Target::Pc
        }
    }
    pub fn device(&self, id: &str) -> Result<Device, String> {
        if self.leds == 0 && matches!(self.family, Family::Skydimo(_)) {
            return Err(
                "SK0410 требует явного числа LED (1..300); используйте add_device_with_leds".into(),
            );
        }
        self.device_with_leds(id, self.leds)
    }
    pub fn device_with_leds(&self, id: &str, leds: usize) -> Result<Device, String> {
        if leds != self.leds
            && !matches!(self.family, Family::Wled | Family::OpenRgb)
            && !matches!(self.family,Family::Skydimo(model) if crate::transport::skydimo::model(model).is_some_and(|m|m.resizable))
        {
            return Err("Этот шаблон имеет фиксированное число LED".into());
        }
        let driver = match self.family {
            Family::HomeAssistant => Driver::HomeAssistant,
            Family::OpenRgb => Driver::OpenRgb,
            Family::Wled => Driver::WledDdp,
            Family::Door => Driver::DoorBle,
            Family::Lcd => Driver::TtLcd,
            Family::Ene => Driver::EneDram,
            Family::Valkyrie(model) => Driver::Valkyrie {
                model: model.into(),
            },
            Family::Fusion(headers) => Driver::Fusion2 { headers },
            Family::Skydimo(model) => Driver::SkydimoSerial {
                model: model.into(),
            },
        };
        let d = Device {
            id: id.into(),
            label: self.label.into(),
            role: if matches!(self.family, Family::Lcd) {
                Role::Lcd
            } else {
                Role::Canvas
            },
            driver,
            leds,
            rate_hz: if matches!(self.family, Family::HomeAssistant) {
                1
            } else if matches!(self.family, Family::Door)
                && leds == crate::lighting::door::PIXEL_LEDS
            {
                10
            } else {
                25
            },
            enabled: false,
        };
        d.validate()?;
        Ok(d)
    }
}
pub fn target(document: &Document, id: &str) -> Target {
    if document.lighting.groups.door.iter().any(|v| v == id) {
        Target::Door
    } else {
        Target::Pc
    }
}

/// Explicit Apply may adopt a newly discovered controller's complete geometry.
/// An existing same-sized user layout is retained; this never opens a transport.
pub fn adopt_openrgb(
    document: &Document,
    id: &str,
    controller: &crate::transport::openrgb::ControllerInfo,
) -> Result<Document, String> {
    let mut device = document
        .lighting
        .setup
        .devices
        .iter()
        .find(|d| d.id == id)
        .ok_or("OpenRGB device was removed")?
        .clone();
    if controller.kind.eq_ignore_ascii_case("virtual") {
        return Err("OpenRGB virtual controllers are excluded".into());
    }
    if device.driver != Driver::OpenRgb {
        return Err("OpenRGB device family changed".into());
    }
    if device.leds == controller.leds {
        return Ok(document.clone());
    }
    if device.role != Role::Canvas {
        return Err("OpenRGB LED count changed: update the ambient route before Apply".into());
    }
    let points = crate::devices::openrgb::positions(controller)?;
    device.leds = controller.leds;
    let canvas = &document.lighting.setup.canvas;
    let mut next = replace(
        document,
        id,
        device,
        target(document, id),
        Some(Layout::Line {
            x0: 0,
            y0: 0,
            x1: canvas.width - 1,
            y1: 0,
        }),
    )?;
    let xmax = points.iter().map(|p| p.0).max().unwrap_or(0).max(1) as u64;
    let ymax = points.iter().map(|p| p.1).max().unwrap_or(0).max(1) as u64;
    next.lighting.setup.canvas.points.retain(|p| p.device != id);
    next.lighting
        .setup
        .canvas
        .points
        .extend(points.into_iter().enumerate().map(|(led, (x, y))| Point {
            device: id.into(),
            led,
            x: ((x as u64 * u64::from(canvas.width - 1)) / xmax) as u32,
            y: ((y as u64 * u64::from(canvas.height - 1)) / ymax) as u32,
        }));
    next.validate()?;
    Ok(next)
}
fn group(document: &mut Document, id: &str, next: Target) {
    if target(document, id) == next
        && (document
            .lighting
            .groups
            .pc
            .iter()
            .chain(&document.lighting.groups.door)
            .any(|v| v == id))
    {
        return;
    }
    document.lighting.groups.pc.retain(|v| v != id);
    document.lighting.groups.door.retain(|v| v != id);
    match next {
        Target::Pc => &mut document.lighting.groups.pc,
        Target::Door => &mut document.lighting.groups.door,
    }
    .push(id.into());
}
fn positions(document: &Document, device: &Device, layout: Layout) -> Result<Vec<Point>, String> {
    if device.role != Role::Canvas || device.leds == 0 || device.leds > MAX_POINTS {
        return Err("Карта требует Canvas с 1..=65536 LED".into());
    }
    let (cw, ch) = (
        document.lighting.setup.canvas.width,
        document.lighting.setup.canvas.height,
    );
    let lerp = |a: u32, b: u32, i: usize, n: usize| {
        if n <= 1 {
            a
        } else {
            ((a as u64 * (n - 1 - i) as u64 + b as u64 * i as u64) / (n - 1) as u64) as u32
        }
    };
    let mut result = Vec::with_capacity(device.leds);
    match layout {
        Layout::Line { x0, y0, x1, y1 } => {
            if x0 >= cw || x1 >= cw || y0 >= ch || y1 >= ch {
                return Err("Линия выходит за холст".into());
            }
            for led in 0..device.leds {
                result.push(Point {
                    device: device.id.clone(),
                    led,
                    x: lerp(x0, x1, led, device.leds),
                    y: lerp(y0, y1, led, device.leds),
                });
            }
        }
        Layout::Grid {
            x,
            y,
            width,
            height,
            columns,
            serpentine,
        } => {
            if width == 0
                || height == 0
                || columns == 0
                || columns > device.leds
                || x.checked_add(width).is_none_or(|v| v > cw)
                || y.checked_add(height).is_none_or(|v| v > ch)
            {
                return Err("Некорректные границы или число столбцов сетки".into());
            }
            let rows = device.leds.div_ceil(columns);
            for led in 0..device.leds {
                let row = led / columns;
                let col = led % columns;
                let col = if serpentine && row % 2 == 1 {
                    columns - 1 - col
                } else {
                    col
                };
                result.push(Point {
                    device: device.id.clone(),
                    led,
                    x: lerp(x, x + width - 1, col, columns),
                    y: lerp(y, y + height - 1, row, rows),
                });
            }
        }
    }
    Ok(result)
}
fn install_points(document: &mut Document, id: &str, points: Vec<Point>, same_count: bool) {
    if same_count {
        for p in &mut document.lighting.setup.canvas.points {
            if p.device == id {
                p.x = points[p.led].x;
                p.y = points[p.led].y;
            }
        }
    } else {
        let old = std::mem::take(&mut document.lighting.setup.canvas.points);
        let mut insert = Some(points);
        for p in old {
            if p.device == id {
                if let Some(points) = insert.take() {
                    document.lighting.setup.canvas.points.extend(points);
                }
            } else {
                document.lighting.setup.canvas.points.push(p);
            }
        }
        if let Some(points) = insert {
            document.lighting.setup.canvas.points.extend(points);
        }
    }
}
pub fn add(
    document: &Document,
    device: Device,
    target: Target,
    layout: Option<Layout>,
) -> Result<Document, String> {
    document.validate()?;
    device.validate()?;
    if document
        .lighting
        .setup
        .devices
        .iter()
        .any(|d| d.id == device.id)
    {
        return Err("ID устройства уже существует".into());
    }
    let points = match (device.role, layout) {
        (Role::Canvas, Some(l)) => positions(document, &device, l)?,
        (Role::Canvas, None) => return Err("Для Canvas нужна явная карта".into()),
        (_, Some(_)) => return Err("Карта доступна только Canvas".into()),
        _ => vec![],
    };
    let mut next = document.clone();
    group(&mut next, &device.id, target);
    if device.role == Role::Lcd {
        next.lcd.push(Lcd {
            device: device.id.clone(),
            config: lcd::Config::default(),
        });
    }
    next.lighting.setup.canvas.points.extend(points);
    next.lighting.setup.devices.push(device);
    next.validate()?;
    Ok(next)
}
pub fn replace(
    document: &Document,
    id: &str,
    device: Device,
    target: Target,
    layout: Option<Layout>,
) -> Result<Document, String> {
    document.validate()?;
    device.validate()?;
    let index = document
        .lighting
        .setup
        .devices
        .iter()
        .position(|d| d.id == id)
        .ok_or("Устройство не найдено")?;
    if device.id != id {
        return Err("Замена должна сохранить ID".into());
    }
    let old = &document.lighting.setup.devices[index];
    let same = old.role == Role::Canvas && device.role == Role::Canvas && old.leds == device.leds;
    let points = match (device.role, layout) {
        (Role::Canvas, Some(l)) => Some(positions(document, &device, l)?),
        (Role::Canvas, None) if same => None,
        (Role::Canvas, None) => {
            return Err("Новое число LED требует явной карты: replace_line".into());
        }
        (_, Some(_)) => return Err("Карта доступна только Canvas".into()),
        _ => Some(vec![]),
    };
    let mut next = document.clone();
    group(&mut next, id, target);
    if device.role != Role::Ambient {
        next.ambient.routes.retain(|r| r.device != id);
    }
    if device.role != Role::Lcd {
        next.lcd.retain(|r| r.device != id);
    } else if let Some(lcd) = next.lcd.iter_mut().find(|r| r.device == id) {
        if old.enabled != device.enabled {
            lcd.config.enabled = device.enabled;
        }
    } else {
        next.lcd.push(Lcd {
            device: id.into(),
            config: lcd::Config::default(),
        });
    }
    if let Some(points) = points {
        install_points(&mut next, id, points, same);
    }
    next.lighting.setup.devices[index] = device;
    next.validate()?;
    Ok(next)
}
pub fn remove(document: &Document, id: &str) -> Result<Document, String> {
    document.validate()?;
    if !document.lighting.setup.devices.iter().any(|d| d.id == id) {
        return Err("Устройство не найдено".into());
    }
    let mut next = document.clone();
    next.lighting.setup.devices.retain(|d| d.id != id);
    next.lighting.setup.canvas.points.retain(|p| p.device != id);
    next.lighting.groups.pc.retain(|v| v != id);
    next.lighting.groups.door.retain(|v| v != id);
    next.lcd.retain(|r| r.device != id);
    next.ambient.routes.retain(|r| r.device != id);
    next.validate()?;
    Ok(next)
}
pub fn relayout(document: &Document, id: &str, layout: Layout) -> Result<Document, String> {
    document.validate()?;
    let d = document
        .lighting
        .setup
        .devices
        .iter()
        .find(|d| d.id == id)
        .ok_or("Устройство не найдено")?;
    let points = positions(document, d, layout)?;
    let mut next = document.clone();
    install_points(&mut next, id, points, true);
    next.validate()?;
    Ok(next)
}
pub fn move_point(
    document: &Document,
    id: &str,
    led: usize,
    x: u32,
    y: u32,
) -> Result<Document, String> {
    document.validate()?;
    let mut next = document.clone();
    let p = next
        .lighting
        .setup
        .canvas
        .points
        .iter_mut()
        .find(|p| p.device == id && p.led == led)
        .ok_or("LED Canvas не найден")?;
    p.x = x;
    p.y = y;
    next.validate()?;
    Ok(next)
}
