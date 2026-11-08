//! GDI preview drawing only. The window owner supplies a buffered client HDC.
use super::{CONTENT_LEFT, Page, View, right_panel};
use std::ptr::null_mut;
use windows_sys::Win32::{
    Foundation::{COLORREF, RECT},
    Graphics::Gdi::*,
};

const BACKGROUND: COLORREF = rgb(248, 247, 244);
const HEADER: COLORREF = rgb(253, 252, 249);
const SIDEBAR: COLORREF = rgb(240, 239, 235);
const TEXT: COLORREF = rgb(39, 42, 45);
const MUTED: COLORREF = rgb(86, 91, 96);
const LINE: COLORREF = rgb(215, 213, 206);
const CANVAS: COLORREF = rgb(27, 29, 31);
const WARM: COLORREF = rgb(164, 85, 43);
const DOOR: COLORREF = rgb(72, 110, 100);
const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | ((g as u32) << 8) | ((b as u32) << 16)
}

struct Font(HFONT);
impl Drop for Font {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.0);
        }
    }
}
struct Pen(HPEN);
impl Drop for Pen {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.0);
        }
    }
}
struct SavedDc {
    hdc: HDC,
    level: i32,
}
impl SavedDc {
    fn new(hdc: HDC) -> Result<Self, String> {
        let level = unsafe { SaveDC(hdc) };
        if level == 0 {
            return Err("GDI SaveDC failed".into());
        }
        Ok(Self { hdc, level })
    }
    fn restore(mut self) -> Result<(), String> {
        let level = self.level;
        self.level = 0;
        if unsafe { RestoreDC(self.hdc, level) } == 0 {
            return Err("GDI RestoreDC failed".into());
        }
        Ok(())
    }
}
impl Drop for SavedDc {
    fn drop(&mut self) {
        if self.level != 0 {
            unsafe {
                RestoreDC(self.hdc, self.level);
            }
        }
    }
}

pub struct Painter {
    dpi: u32,
    body: Font,
    heading: Font,
    grid: Pen,
    point_edge: Pen,
}
impl Painter {
    pub fn new(dpi: u32) -> Result<Self, String> {
        if dpi == 0 {
            return Err("DPI must be nonzero".into());
        }
        let font = |dip: i32, weight: i32| -> Result<Font, String> {
            let face: Vec<u16> = "Segoe UI\0".encode_utf16().collect();
            let handle = unsafe {
                CreateFontW(
                    -scaled(dpi, dip).max(1),
                    0,
                    0,
                    0,
                    weight,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET as u32,
                    OUT_DEFAULT_PRECIS as u32,
                    CLIP_DEFAULT_PRECIS as u32,
                    CLEARTYPE_QUALITY as u32,
                    DEFAULT_PITCH as u32,
                    face.as_ptr(),
                )
            };
            if handle.is_null() {
                return Err("Cannot create Segoe UI font".into());
            }
            Ok(Font(handle))
        };
        let pen = |color| -> Result<Pen, String> {
            let handle = unsafe { CreatePen(PS_SOLID, scaled(dpi, 1).max(1), color) };
            if handle.is_null() {
                return Err("Cannot create GDI pen".into());
            }
            Ok(Pen(handle))
        };
        Ok(Self {
            dpi,
            body: font(14, 400)?,
            heading: font(22, 600)?,
            grid: pen(rgb(48, 51, 54))?,
            point_edge: pen(rgb(123, 127, 130))?,
        })
    }
    /// The Painter must outlive any native controls borrowing this font.
    pub fn body_font(&self) -> HFONT {
        self.body.0
    }
    fn px(&self, dip: i32) -> i32 {
        scaled(self.dpi, dip)
    }
    fn rect(&self, x: i32, y: i32, width: i32, height: i32) -> RECT {
        RECT {
            left: self.px(x),
            top: self.px(y),
            right: self.px(x.saturating_add(width)),
            bottom: self.px(y.saturating_add(height)),
        }
    }
    fn text(
        &self,
        hdc: HDC,
        value: &str,
        mut rect: RECT,
        heading: bool,
        color: COLORREF,
        multiline: bool,
    ) -> Result<(), String> {
        if value.is_empty() || rect.right <= rect.left || rect.bottom <= rect.top {
            return Ok(());
        }
        select(hdc, if heading { self.heading.0 } else { self.body.0 })?;
        let text: Vec<u16> = value.encode_utf16().take(4096).collect();
        let flags = DT_NOPREFIX
            | DT_END_ELLIPSIS
            | if multiline {
                DT_WORDBREAK
            } else {
                DT_SINGLELINE | DT_VCENTER
            };
        if unsafe { SetTextColor(hdc, color) } == CLR_INVALID {
            return Err("GDI SetTextColor failed".into());
        }
        if unsafe { DrawTextW(hdc, text.as_ptr(), text.len() as i32, &mut rect, flags) } == 0 {
            return Err("GDI DrawText failed".into());
        }
        Ok(())
    }
    pub(crate) fn paint(
        &self,
        hdc: HDC,
        width_px: i32,
        height_px: i32,
        view: &View<'_>,
    ) -> Result<(), String> {
        if hdc.is_null() || width_px <= 0 || height_px <= 0 {
            return Err("Invalid preview drawing surface".into());
        }
        let saved = SavedDc::new(hdc)?;
        if unsafe { IntersectClipRect(hdc, 0, 0, width_px, height_px) } == 0 {
            return Err("GDI client clipping failed".into());
        }
        if unsafe { SetBkMode(hdc, TRANSPARENT as i32) } == 0 {
            return Err("GDI transparent text mode failed".into());
        }
        let width =
            ((i64::from(width_px) * 96) / i64::from(self.dpi)).min(i64::from(i32::MAX)) as i32;
        let height =
            ((i64::from(height_px) * 96) / i64::from(self.dpi)).min(i64::from(i32::MAX)) as i32;
        fill(
            hdc,
            RECT {
                left: 0,
                top: 0,
                right: width_px,
                bottom: height_px,
            },
            BACKGROUND,
        )?;
        fill(hdc, self.rect(0, 0, width, 88), HEADER)?;
        fill(hdc, self.rect(0, 88, 164, height - 88), SIDEBAR)?;
        fill(hdc, self.rect(0, 87, width, 1), LINE)?;
        fill(hdc, self.rect(164, 88, 1, height - 88), LINE)?;
        self.text(
            hdc,
            "Prismestra",
            self.rect(24, 16, width - 48, 32),
            true,
            TEXT,
            false,
        )?;
        self.text(
            hdc,
            if view.native_lighting {
                "Аппаратная подсветка · проверяйте статусы устройств"
            } else {
                "Предпросмотр · аппаратная подсветка выключена"
            },
            self.rect(24, 52, width - 48, 22),
            false,
            MUTED,
            false,
        )?;
        match view.page {
            Page::Scene => self.scene(hdc, width, height, view)?,
            Page::Schedule => self.schedule(hdc, width, height, view)?,
            Page::Devices
            | Page::Connections
            | Page::Ambient
            | Page::Lcd
            | Page::Temperatures
            | Page::Sources => {
                self.properties(hdc, width, view)?;
            }
        }
        fill(hdc, self.rect(0, height - 45, width, 1), LINE)?;
        let status = if view.dirty {
            format!("* {} · Есть несохранённые изменения", view.status)
        } else {
            view.status.to_owned()
        };
        self.text(
            hdc,
            &status,
            self.rect(16, height - 34, width - 32, 24),
            false,
            TEXT,
            false,
        )?;
        saved.restore()
    }

    fn properties(&self, hdc: HDC, width: i32, view: &View<'_>) -> Result<(), String> {
        let title = match view.page {
            Page::Devices => "Устройства и расположение",
            Page::Ambient => "Подсветка экрана",
            Page::Lcd => "LCD",
            Page::Temperatures => "Температуры и источники",
            Page::Sources => "Датчики и привязки",
            Page::Connections => "Подключения устройств",
            _ => return Ok(()),
        };
        self.text(
            hdc,
            title,
            self.rect(CONTENT_LEFT, 110, width - CONTENT_LEFT - 24, 32),
            true,
            TEXT,
            false,
        )?;
        self.text(
            hdc,
            view.detail,
            self.rect(CONTENT_LEFT, 148, width - CONTENT_LEFT - 24, 24),
            false,
            MUTED,
            false,
        )?;
        // The native target/property lists, accessible descriptions, value
        // editors and action buttons are drawn by their actual child controls.
        match view.page {
            Page::Ambient => self.ambient_preview(hdc, right_panel(width), view.ambient_colors),
            Page::Lcd => self.lcd_preview(
                hdc,
                right_panel(width),
                view.lcd_preview.as_ref(),
                view.lcd_hint,
            ),
            _ => Ok(()),
        }
    }

    fn ambient_preview(
        &self,
        hdc: HDC,
        x: i32,
        colors: Option<&[crate::lighting::color::Rgb]>,
    ) -> Result<(), String> {
        let frame = self.rect(x, 430, 280, 50);
        fill(hdc, frame, LINE)?;
        let inner = self.rect(x + 1, 431, 278, 48);
        fill(hdc, inner, HEADER)?;
        let Some(colors) = colors.filter(|colors| !colors.is_empty()) else {
            self.text(
                hdc,
                "Кадр ещё не получен",
                self.rect(x + 10, 436, 260, 36),
                false,
                MUTED,
                false,
            )?;
            return self.text(
                hdc,
                "Цвета появятся после получения кадра.",
                self.rect(x, 490, 280, 40),
                false,
                MUTED,
                true,
            );
        };
        if colors.len() > 65_536 {
            return Err("Ambient preview exceeds LED bound".into());
        }
        let count = colors.len().min(128);
        let width = i64::from(inner.right - inner.left);
        for index in 0..count {
            let selected = if count == 1 {
                0
            } else {
                index * (colors.len() - 1) / (count - 1)
            };
            let color = colors[selected];
            let left = inner.left + (width * index as i64 / count as i64) as i32;
            let right = inner.left + (width * (index + 1) as i64 / count as i64) as i32;
            fill(
                hdc,
                RECT {
                    left,
                    top: inner.top,
                    right,
                    bottom: inner.bottom,
                },
                rgb(color.r, color.g, color.b),
            )?;
        }
        let caption = if colors.len() > count {
            format!("Предпросмотр · {} точек, выборка", colors.len())
        } else {
            format!("Предпросмотр · {} точек", colors.len())
        };
        self.text(
            hdc,
            &caption,
            self.rect(x, 490, 280, 40),
            false,
            MUTED,
            true,
        )
    }

    fn lcd_preview(
        &self,
        hdc: HDC,
        x: i32,
        surface: Option<&super::Surface<'_>>,
        hint: &str,
    ) -> Result<(), String> {
        fill(hdc, self.rect(x, 184, 180, 180), LINE)?;
        let inner = self.rect(x + 1, 185, 178, 178);
        let Some(surface) = surface else {
            fill(hdc, inner, HEADER)?;
            return self.text(
                hdc,
                hint,
                self.rect(x + 12, 236, 156, 80),
                false,
                MUTED,
                true,
            );
        };
        if surface.width == 0 || surface.height == 0 || surface.width > 480 || surface.height > 480
        {
            return Err("LCD preview dimensions must be 1..=480".into());
        }
        let bytes = (surface.width as usize)
            .checked_mul(surface.height as usize)
            .and_then(|size| size.checked_mul(4))
            .ok_or("LCD preview size overflow")?;
        if surface.bgra.len() != bytes {
            return Err("LCD preview requires an exact BGRA buffer".into());
        }
        fill(hdc, inner, rgb(0, 0, 0))?;
        let scale = (f64::from(inner.right - inner.left) / f64::from(surface.width))
            .min(f64::from(inner.bottom - inner.top) / f64::from(surface.height));
        let width = (f64::from(surface.width) * scale).round().max(1.) as i32;
        let height = (f64::from(surface.height) * scale).round().max(1.) as i32;
        let left = inner.left + (inner.right - inner.left - width) / 2;
        let top = inner.top + (inner.bottom - inner.top - height) / 2;
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: surface.width as i32,
                biHeight: -(surface.height as i32), // Existing top-down BGRA; alpha is not synthesized.
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: bytes as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let saved = SavedDc::new(hdc)?;
        if unsafe { IntersectClipRect(hdc, inner.left, inner.top, inner.right, inner.bottom) } == 0
        {
            return Err("GDI LCD preview clipping failed".into());
        }
        if unsafe { SetStretchBltMode(hdc, COLORONCOLOR) } == 0 {
            return Err("GDI LCD stretch mode failed".into());
        }
        let copied = unsafe {
            StretchDIBits(
                hdc,
                left,
                top,
                width,
                height,
                0,
                0,
                surface.width as i32,
                surface.height as i32,
                surface.bgra.as_ptr().cast(),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            )
        };
        if copied == 0 || copied == GDI_ERROR {
            return Err("GDI LCD preview copy failed".into());
        }
        saved.restore()
    }

    fn scene(&self, hdc: HDC, width: i32, height: i32, view: &View<'_>) -> Result<(), String> {
        let right = right_panel(width);
        let content_width = right - CONTENT_LEFT - 24;
        self.text(
            hdc,
            "Сцена",
            self.rect(CONTENT_LEFT, 110, content_width, 32),
            true,
            TEXT,
            false,
        )?;
        let area = self.rect(CONTENT_LEFT, 164, content_width, height - 164 - 180);
        if area.right > area.left && area.bottom > area.top {
            self.canvas(hdc, area, view)?;
        }
        let canvas = &view.project.setup.canvas;
        self.text(
            hdc,
            &format!(
                "Схема {} × {} · точек: {} · палитра",
                canvas.width,
                canvas.height,
                canvas.points.len()
            ),
            self.rect(CONTENT_LEFT, height - 172, content_width, 24),
            false,
            MUTED,
            false,
        )?;
        for (index, &color) in view.project.scene.colors.iter().take(8).enumerate() {
            let x = CONTENT_LEFT + index as i32 * 36;
            if x + 28 > right - 24 {
                break;
            }
            fill(hdc, self.rect(x, height - 140, 28, 24), LINE)?;
            fill(
                hdc,
                self.rect(x + 1, height - 139, 26, 22),
                rgb((color >> 16) as u8, (color >> 8) as u8, color as u8),
            )?;
        }
        let on = |value| if value { "вкл." } else { "выкл." };
        self.text(
            hdc,
            &format!(
                "ПК: {} · дверь: {}. {}",
                on(view.pc_on),
                on(view.door_on),
                if view.native_lighting {
                    "Аппаратный вывод · статусы передачи на вкладке устройств."
                } else {
                    "Изменения видны только в предпросмотре."
                }
            ),
            self.rect(CONTENT_LEFT, height - 92, content_width, 40),
            false,
            MUTED,
            true,
        )?;
        Ok(())
    }

    fn canvas(&self, hdc: HDC, area: RECT, view: &View<'_>) -> Result<(), String> {
        fill(hdc, area, CANVAS)?;
        let saved = SavedDc::new(hdc)?;
        if unsafe { IntersectClipRect(hdc, area.left, area.top, area.right, area.bottom) } == 0 {
            return Err("GDI canvas clipping failed".into());
        }
        let canvas = &view.project.setup.canvas;
        if canvas.points.is_empty() {
            self.text(
                hdc,
                "В проекте пока нет точек.\nОткройте сохранённую сцену со схемой устройств.",
                RECT {
                    left: area.left + self.px(24),
                    top: area.top + (area.bottom - area.top) / 2 - self.px(30),
                    right: area.right - self.px(24),
                    bottom: area.bottom - self.px(16),
                },
                false,
                rgb(222, 224, 226),
                true,
            )?;
            return saved.restore();
        }
        if canvas.width == 0
            || canvas.height == 0
            || canvas.width > 8192
            || canvas.height > 8192
            || canvas.points.len() > 65_536
        {
            return Err("Invalid preview canvas dimensions".into());
        }
        if view.colors.len() != canvas.points.len() {
            self.text(
                hdc,
                "Кадр предпросмотра ещё не рассчитан.",
                RECT {
                    left: area.left + self.px(24),
                    top: area.top + self.px(24),
                    right: area.right - self.px(24),
                    bottom: area.bottom - self.px(24),
                },
                false,
                rgb(222, 224, 226),
                true,
            )?;
            return saved.restore();
        }
        let inset = self.px(20);
        let available_width = (area.right - area.left - 2 * inset).max(1);
        let available_height = (area.bottom - area.top - 2 * inset).max(1);
        let scale = (f64::from(available_width) / f64::from(canvas.width))
            .min(f64::from(available_height) / f64::from(canvas.height));
        let left = f64::from(area.left)
            + (f64::from(area.right - area.left) - f64::from(canvas.width) * scale) / 2.;
        let top = f64::from(area.top)
            + (f64::from(area.bottom - area.top) - f64::from(canvas.height) * scale) / 2.;
        if canvas.width >= 4 && canvas.height >= 4 {
            select(hdc, self.grid.0)?;
            for x in (0..=canvas.width).step_by(canvas.width.div_ceil(12) as usize) {
                let px = (left + f64::from(x) * scale).round() as i32;
                line(
                    hdc,
                    px,
                    top.round() as i32,
                    px,
                    (top + f64::from(canvas.height) * scale).round() as i32,
                )?;
            }
            for y in (0..=canvas.height).step_by(canvas.height.div_ceil(10) as usize) {
                let py = (top + f64::from(y) * scale).round() as i32;
                line(
                    hdc,
                    left.round() as i32,
                    py,
                    (left + f64::from(canvas.width) * scale).round() as i32,
                    py,
                )?;
            }
        }
        select(hdc, self.point_edge.0)?;
        let brush = unsafe { GetStockObject(DC_BRUSH) };
        select(hdc, brush)?;
        let radius = (scale * 0.35)
            .round()
            .clamp(f64::from(self.px(2).max(1)), f64::from(self.px(5).max(1)))
            as i32;
        for (point, color) in canvas.points.iter().zip(view.colors) {
            if point.x >= canvas.width || point.y >= canvas.height {
                return Err("Preview point outside canvas".into());
            }
            let x = (left + (f64::from(point.x) + 0.5) * scale).round() as i32;
            let y = (top + (f64::from(point.y) + 0.5) * scale).round() as i32;
            if unsafe { SetDCBrushColor(hdc, rgb(color.r, color.g, color.b)) } == CLR_INVALID {
                return Err("GDI point brush color failed".into());
            }
            if unsafe { Ellipse(hdc, x - radius, y - radius, x + radius + 1, y + radius + 1) } == 0
            {
                return Err("GDI preview point failed".into());
            }
        }
        saved.restore()
    }

    fn schedule(&self, hdc: HDC, width: i32, _height: i32, view: &View<'_>) -> Result<(), String> {
        self.text(
            hdc,
            "Расписание",
            self.rect(CONTENT_LEFT, 110, width - CONTENT_LEFT - 24, 32),
            true,
            TEXT,
            false,
        )?;
        self.text(
            hdc,
            "Суточные правила проекта. Системное время и работающие устройства не меняются.",
            self.rect(CONTENT_LEFT, 148, width - CONTENT_LEFT - 24, 24),
            false,
            MUTED,
            false,
        )?;
        let schedule = &view.project.schedule;
        let x = CONTENT_LEFT + 64;
        let plot_width = width - x - 24;
        if plot_width <= 0 {
            return Ok(());
        }
        for (start, y, color, label) in [
            (schedule.times[0], 454, WARM, "ПК"),
            (schedule.times[1], 482, DOOR, "Дверь"),
        ] {
            self.text(
                hdc,
                label,
                self.rect(CONTENT_LEFT, y - 4, 56, 24),
                false,
                TEXT,
                false,
            )?;
            fill(hdc, self.rect(x, y, plot_width, 14), LINE)?;
            let end = schedule.times[2];
            let segment = |from: u16, to: u16| -> Result<(), String> {
                let left = x + (i64::from(plot_width) * i64::from(from) / 1440) as i32;
                let right = x + (i64::from(plot_width) * i64::from(to) / 1440) as i32;
                fill(hdc, self.rect(left, y, right - left, 14), color)
            };
            if start < end {
                segment(start, end)?;
            } else {
                segment(0, end)?;
                segment(start, 1440)?;
            }
        }
        for hour in [0, 6, 12, 18, 24] {
            let offset = (i64::from(plot_width) * i64::from(hour) / 24) as i32;
            self.text(
                hdc,
                &format!("{hour:02}:00"),
                self.rect((x + offset - 18).clamp(x, width - 70), 510, 46, 20),
                false,
                MUTED,
                false,
            )?;
        }
        let time = |minute: u16| format!("{:02}:{:02}", minute / 60, minute % 60);
        self.text(
            hdc,
            &format!(
                "ПК {} · дверь {} · всё выключается {}",
                time(schedule.times[0]),
                time(schedule.times[1]),
                time(schedule.times[2])
            ),
            self.rect(CONTENT_LEFT, 540, width - CONTENT_LEFT - 24, 22),
            false,
            TEXT,
            false,
        )?;
        let state = if !schedule.enabled {
            "выключено"
        } else if schedule.paused {
            "на паузе"
        } else {
            if view.native_lighting {
                "включено для устройств"
            } else {
                "включено для предпросмотра"
            }
        };
        self.text(
            hdc,
            &format!("Часовой пояс: {} · расписание {}", schedule.timezone, state),
            self.rect(CONTENT_LEFT, 568, width - CONTENT_LEFT - 24, 22),
            false,
            MUTED,
            false,
        )
    }
}

fn scaled(dpi: u32, dip: i32) -> i32 {
    ((i64::from(dip) * i64::from(dpi) + 48) / 96).clamp(i64::from(i32::MIN), i64::from(i32::MAX))
        as i32
}
fn select(hdc: HDC, object: HGDIOBJ) -> Result<(), String> {
    let previous = unsafe { SelectObject(hdc, object) };
    if previous.is_null() || previous as isize == -1 {
        return Err("GDI SelectObject failed".into());
    }
    Ok(())
}
fn fill(hdc: HDC, rect: RECT, color: COLORREF) -> Result<(), String> {
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return Ok(());
    }
    let brush = unsafe { GetStockObject(DC_BRUSH) };
    if brush.is_null() || unsafe { SetDCBrushColor(hdc, color) } == CLR_INVALID {
        return Err("GDI fill brush failed".into());
    }
    if unsafe { FillRect(hdc, &rect, brush) } == 0 {
        return Err("GDI FillRect failed".into());
    }
    Ok(())
}
fn line(hdc: HDC, x1: i32, y1: i32, x2: i32, y2: i32) -> Result<(), String> {
    if unsafe { MoveToEx(hdc, x1, y1, null_mut()) } == 0 || unsafe { LineTo(hdc, x2, y2) } == 0 {
        return Err("GDI line failed".into());
    }
    Ok(())
}
