//! Thread-owned DXGI capture with named-monitor GDI fallback. None of the desktop
//! entrypoints are called by tests. WARP tests use synthetic textures only.
use super::{Backend, Counters, Frame, Monitor, pixels, valid_size, validate_name};
use crate::{
    clock::monotonic_ms,
    lighting::ambient::{Image, Rect},
};
use std::{marker::PhantomData, rc::Rc, sync::Arc, time::Instant};
use windows::{
    Win32::{
        Foundation::{HANDLE, HMODULE, LPARAM, RECT},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_UNKNOWN},
            Direct3D11::*,
            Dxgi::{Common::*, *},
            Gdi::*,
        },
        System::{StationsAndDesktops::*, Threading::GetCurrentThreadId},
    },
    core::{BOOL, Interface},
};

fn message(action: &str, error: impl std::fmt::Display) -> String {
    format!("{action}: {error}")
}
fn elapsed_us(start: Instant) -> u64 {
    start.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}
fn utf16_name(units: &[u16]) -> Result<String, String> {
    let end = units
        .iter()
        .position(|u| *u == 0)
        .ok_or("Unterminated Windows display name")?;
    String::from_utf16(&units[..end]).map_err(|_| "Invalid Windows display name".into())
}
unsafe extern "system" fn monitor_callback(
    monitor: HMONITOR,
    _: HDC,
    _: *mut RECT,
    state: LPARAM,
) -> BOOL {
    let result = unsafe { &mut *(state.0 as *mut Result<Vec<Monitor>, String>) };
    if result.is_err() {
        return BOOL(0);
    }
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if !unsafe { GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO) }.as_bool() {
        *result = Err(message("Get monitor info", std::io::Error::last_os_error()));
        return BOOL(0);
    }
    let rect = info.monitorInfo.rcMonitor;
    let item = (|| {
        let item = Monitor {
            device_name: utf16_name(&info.szDevice)?,
            left: rect.left,
            top: rect.top,
            width: u32::try_from(i64::from(rect.right) - i64::from(rect.left))
                .map_err(|_| "Invalid monitor width")?,
            height: u32::try_from(i64::from(rect.bottom) - i64::from(rect.top))
                .map_err(|_| "Invalid monitor height")?,
        };
        item.validate()?;
        Ok(item)
    })();
    match item {
        Ok(item) if result.as_ref().unwrap().len() < 256 => result.as_mut().unwrap().push(item),
        Ok(_) => *result = Err("Too many monitors".into()),
        Err(e) => *result = Err(e),
    }
    BOOL(result.is_ok().into())
}
pub fn monitors() -> Result<Vec<Monitor>, String> {
    let mut result: Result<Vec<Monitor>, String> = Ok(Vec::new());
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(monitor_callback),
            LPARAM(&mut result as *mut _ as isize),
        )
    };
    if !ok.as_bool() && result.is_ok() {
        return Err(message(
            "Enumerate monitors",
            std::io::Error::last_os_error(),
        ));
    }
    result
}
fn selected_monitor(name: &str) -> Result<Monitor, String> {
    let mut matching = monitors()?
        .into_iter()
        .filter(|m| m.device_name.eq_ignore_ascii_case(name));
    let selected = matching.next().ok_or("Selected display is missing")?;
    if matching.next().is_some() {
        return Err("Selected display is ambiguous".into());
    }
    Ok(selected)
}
fn desktop_accessible() -> bool {
    let Ok(input) =
        (unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) })
    else {
        return false;
    };
    let accessible = (|| {
        let thread = unsafe { GetThreadDesktop(GetCurrentThreadId()) }.ok()?;
        let mut input_name = [0u16; 256];
        let mut thread_name = [0u16; 256];
        let mut needed = 0;
        unsafe {
            GetUserObjectInformationW(
                HANDLE(input.0),
                UOI_NAME,
                Some(input_name.as_mut_ptr().cast()),
                512,
                Some(&mut needed),
            )
        }
        .ok()?;
        unsafe {
            GetUserObjectInformationW(
                HANDLE(thread.0),
                UOI_NAME,
                Some(thread_name.as_mut_ptr().cast()),
                512,
                Some(&mut needed),
            )
        }
        .ok()?;
        Some(utf16_name(&input_name).ok()? == utf16_name(&thread_name).ok()?)
    })()
    .unwrap_or(false);
    let _ = unsafe { CloseDesktop(input) };
    accessible
}

fn device(
    adapter: Option<&IDXGIAdapter1>,
    driver: D3D_DRIVER_TYPE,
) -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let adapter = adapter
        .map(|a| a.cast::<IDXGIAdapter>())
        .transpose()
        .map_err(|e| message("Get D3D adapter", e))?;
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            adapter.as_ref(),
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|e| message("Create D3D device", e))?;
    Ok((
        device.ok_or("Missing D3D device")?,
        context.ok_or("Missing D3D context")?,
    ))
}
struct Readback {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    staging: Option<ID3D11Texture2D>,
    cached: Option<(Option<Rect>, Arc<Image>)>,
}
impl Readback {
    fn copy(&mut self, texture: &ID3D11Texture2D) -> Result<(), String> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        valid_size(desc.Width, desc.Height)?;
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
            || desc.SampleDesc.Count != 1
            || desc.ArraySize != 1
            || desc.MipLevels != 1
        {
            return Err("Unsupported capture texture format/layout".into());
        }
        let mut old = D3D11_TEXTURE2D_DESC::default();
        if let Some(staging) = &self.staging {
            unsafe { staging.GetDesc(&mut old) };
        }
        if old.Width != desc.Width || old.Height != desc.Height {
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            let mut staging = None;
            unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut staging)) }
                .map_err(|e| message("Create capture staging texture", e))?;
            self.staging = Some(staging.ok_or("Missing capture staging texture")?);
            self.cached = None;
        }
        unsafe {
            self.context
                .CopyResource(self.staging.as_ref().unwrap(), texture)
        };
        Ok(())
    }
    fn image(&mut self, crop: Option<Rect>, counters: &mut Counters) -> Result<Arc<Image>, String> {
        let texture = self.staging.as_ref().ok_or("No captured texture")?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        let start = Instant::now();
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context
                .Map(texture, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }
        .map_err(|e| message("Map capture texture", e))?;
        counters.readback_us = counters.readback_us.saturating_add(elapsed_us(start));
        struct Unmap<'a>(&'a ID3D11DeviceContext, &'a ID3D11Texture2D);
        impl Drop for Unmap<'_> {
            fn drop(&mut self) {
                unsafe { self.0.Unmap(self.1, 0) };
            }
        }
        let _unmap = Unmap(&self.context, texture);
        let start = Instant::now();
        let result = (|| {
            let pitch = mapped.RowPitch as usize;
            let len = (desc.Height as usize)
                .checked_sub(1)
                .and_then(|h| h.checked_mul(pitch))
                .and_then(|v| v.checked_add(desc.Width as usize * 4))
                .ok_or("Capture row length overflow")?;
            if mapped.pData.is_null() || pitch < desc.Width as usize * 4 || len > 512 * 1024 * 1024
            {
                return Err("Invalid mapped capture layout".into());
            }
            let source = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), len) };
            pixels::thumbnail(source, desc.Width, desc.Height, pitch, crop).map(Arc::new)
        })();
        counters.scale_us = counters.scale_us.saturating_add(elapsed_us(start));
        let image = result?;
        self.cached = Some((crop, image.clone()));
        Ok(image)
    }
    fn cached(&self, crop: Option<Rect>) -> Option<Arc<Image>> {
        self.cached
            .as_ref()
            .filter(|c| c.0 == crop)
            .map(|c| c.1.clone())
    }
}
struct DuplicationFrame<'a>(&'a IDXGIOutputDuplication, bool);
impl DuplicationFrame<'_> {
    fn finish(mut self) -> Result<(), String> {
        self.1 = false;
        unsafe { self.0.ReleaseFrame() }.map_err(|e| message("Release desktop frame", e))
    }
}
impl Drop for DuplicationFrame<'_> {
    fn drop(&mut self) {
        if self.1 {
            let _ = unsafe { self.0.ReleaseFrame() };
        }
    }
}
struct Dxgi {
    duplication: IDXGIOutputDuplication,
    readback: Readback,
}
impl Dxgi {
    fn new(name: &str) -> Result<Self, String> {
        let factory: IDXGIFactory1 =
            unsafe { CreateDXGIFactory1() }.map_err(|e| message("Create DXGI factory", e))?;
        for ai in 0..256 {
            let adapter = match unsafe { factory.EnumAdapters1(ai) } {
                Ok(a) => a,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(message("Enumerate DXGI adapter", e)),
            };
            for oi in 0..256 {
                let output = match unsafe { adapter.EnumOutputs(oi) } {
                    Ok(o) => o,
                    Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(e) => return Err(message("Enumerate DXGI output", e)),
                };
                let desc =
                    unsafe { output.GetDesc() }.map_err(|e| message("Read DXGI output", e))?;
                if !desc.AttachedToDesktop.as_bool()
                    || !utf16_name(&desc.DeviceName)?.eq_ignore_ascii_case(name)
                {
                    continue;
                }
                if desc.Rotation != DXGI_MODE_ROTATION_IDENTITY
                    && desc.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED
                {
                    return Err("Rotated display requires GDI fallback".into());
                }
                if let Ok(output6) = output.cast::<IDXGIOutput6>() {
                    let desc1 = unsafe { output6.GetDesc1() }
                        .map_err(|e| message("Read output color space", e))?;
                    if desc1.ColorSpace != DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709 {
                        return Err("HDR display requires GDI fallback".into());
                    }
                }
                let (device, context) = device(Some(&adapter), D3D_DRIVER_TYPE_UNKNOWN)?;
                let output1: IDXGIOutput1 = output
                    .cast()
                    .map_err(|e| message("Get duplication output", e))?;
                let duplication = unsafe { output1.DuplicateOutput(&device) }
                    .map_err(|e| message("Duplicate output", e))?;
                let desc = unsafe { duplication.GetDesc() };
                valid_size(desc.ModeDesc.Width, desc.ModeDesc.Height)?;
                if desc.ModeDesc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
                    || (desc.Rotation != DXGI_MODE_ROTATION_IDENTITY
                        && desc.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED)
                {
                    return Err("Unsupported duplication format/rotation".into());
                }
                return Ok(Self {
                    duplication,
                    readback: Readback {
                        device,
                        context,
                        staging: None,
                        cached: None,
                    },
                });
            }
        }
        Err("Exact DXGI display not found".into())
    }
    fn poll(
        &mut self,
        crop: Option<Rect>,
        counters: &mut Counters,
    ) -> Result<Option<(Arc<Image>, bool)>, String> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        let acquired = match unsafe {
            self.duplication
                .AcquireNextFrame(0, &mut info, &mut resource)
        } {
            Ok(()) => true,
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => false,
            Err(e) => return Err(message("Acquire desktop frame", e)),
        };
        let guard = acquired.then(|| DuplicationFrame(&self.duplication, true));
        let new_pixels = acquired && (info.LastPresentTime != 0 || self.readback.staging.is_none());
        if new_pixels {
            let texture: ID3D11Texture2D = resource
                .ok_or("Missing desktop resource")?
                .cast()
                .map_err(|e| message("Get desktop texture", e))?;
            self.readback.copy(&texture)?;
            // New pixels must invalidate the old thumbnail even at equal dimensions.
            self.readback.cached = None;
            counters.source_readbacks = counters.source_readbacks.saturating_add(1);
        }
        let image = if let Some(image) = self.readback.cached(crop) {
            Some(image)
        } else if self.readback.staging.is_some() {
            Some(self.readback.image(crop, counters)?)
        } else {
            None
        };
        if let Some(guard) = guard {
            guard.finish()?;
        }
        Ok(image.map(|image| (image, new_pixels)))
    }
}

struct Gdi {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut std::ffi::c_void,
    size: (u32, u32),
}
impl Default for Gdi {
    fn default() -> Self {
        Self {
            dc: HDC::default(),
            bitmap: HBITMAP::default(),
            previous: HGDIOBJ::default(),
            bits: std::ptr::null_mut(),
            size: (0, 0),
        }
    }
}
impl Drop for Gdi {
    fn drop(&mut self) {
        unsafe {
            if !self.dc.0.is_null() && !self.previous.0.is_null() {
                SelectObject(self.dc, self.previous);
            }
            if !self.bitmap.0.is_null() {
                let _ = DeleteObject(self.bitmap.into());
            }
            if !self.dc.0.is_null() {
                let _ = DeleteDC(self.dc);
            }
        }
    }
}
impl Gdi {
    fn poll(
        &mut self,
        monitor: &Monitor,
        crop: Option<Rect>,
        counters: &mut Counters,
    ) -> Result<Arc<Image>, String> {
        monitor.validate()?;
        let display = unsafe { GetDC(None) };
        if display.0.is_null() {
            return Err("Cannot get display DC".into());
        }
        struct Release(HDC);
        impl Drop for Release {
            fn drop(&mut self) {
                unsafe {
                    ReleaseDC(None, self.0);
                }
            }
        }
        let _release = Release(display);
        if self.size != (monitor.width, monitor.height) {
            *self = Self::default();
            self.dc = unsafe { CreateCompatibleDC(Some(display)) };
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: monitor.width as i32,
                    biHeight: -(monitor.height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            self.bitmap = unsafe {
                CreateDIBSection(
                    Some(display),
                    &info,
                    DIB_RGB_COLORS,
                    &mut self.bits,
                    None,
                    0,
                )
            }
            .map_err(|e| message("Create capture bitmap", e))?;
            if self.dc.0.is_null() || self.bits.is_null() {
                return Err("Cannot create capture DC/bitmap".into());
            }
            self.previous = unsafe { SelectObject(self.dc, self.bitmap.into()) };
            if self.previous.0.is_null() || self.previous.0 as isize == -1 {
                self.previous = HGDIOBJ::default();
                return Err("Cannot select capture bitmap".into());
            }
            self.size = (monitor.width, monitor.height);
        }
        let start = Instant::now();
        unsafe {
            BitBlt(
                self.dc,
                0,
                0,
                monitor.width as i32,
                monitor.height as i32,
                Some(display),
                monitor.left,
                monitor.top,
                SRCCOPY,
            )
        }
        .map_err(|e| message("Capture display bitmap", e))?;
        if !unsafe { GdiFlush() }.as_bool() {
            return Err("Cannot flush display capture".into());
        }
        counters.readback_us = counters.readback_us.saturating_add(elapsed_us(start));
        counters.source_readbacks = counters.source_readbacks.saturating_add(1);
        let start = Instant::now();
        let len = monitor.width as usize * monitor.height as usize * 4;
        let bytes = unsafe { std::slice::from_raw_parts(self.bits.cast::<u8>(), len) };
        let image = pixels::thumbnail(
            bytes,
            monitor.width,
            monitor.height,
            monitor.width as usize * 4,
            crop,
        )
        .map(Arc::new);
        counters.scale_us = counters.scale_us.saturating_add(elapsed_us(start));
        image
    }
}

/// Construct/poll/drop on one capture thread. No background thread is implicitly started.
pub struct Source {
    name: String,
    dxgi: Option<Dxgi>,
    gdi: Gdi,
    retry_at_ms: u64,
    counters: Counters,
    last_dxgi_error: Option<String>,
    _thread: PhantomData<Rc<()>>,
}
impl Source {
    pub fn new(device_name: String) -> Result<Self, String> {
        validate_name(&device_name)?;
        Ok(Self {
            name: device_name,
            dxgi: None,
            gdi: Gdi::default(),
            retry_at_ms: 0,
            counters: Counters::default(),
            last_dxgi_error: None,
            _thread: PhantomData,
        })
    }
    pub fn counters(&self) -> Counters {
        self.counters
    }
    pub fn last_dxgi_error(&self) -> Option<&str> {
        self.last_dxgi_error.as_deref()
    }
    pub fn reset(&mut self) {
        self.dxgi = None;
        self.gdi = Gdi::default();
        self.retry_at_ms = 0;
    }
    pub fn poll(&mut self, crop: Option<Rect>) -> Result<Option<Frame>, String> {
        if !desktop_accessible() {
            self.reset();
            return Err("Input desktop is inaccessible; no frame published".into());
        }
        let now = monotonic_ms();
        if self.dxgi.is_none() && now >= self.retry_at_ms {
            if let Err(e) = selected_monitor(&self.name) {
                self.reset();
                return Err(e);
            }
            match Dxgi::new(&self.name) {
                Ok(dxgi) => {
                    self.dxgi = Some(dxgi);
                    self.gdi = Gdi::default();
                    self.last_dxgi_error = None;
                }
                Err(e) => self.last_dxgi_error = Some(e),
            }
            self.retry_at_ms = now.saturating_add(2000);
        }
        let mut result = None;
        if let Some(dxgi) = self.dxgi.as_mut() {
            match dxgi.poll(crop, &mut self.counters) {
                Ok(frame) => {
                    result = frame.map(|(image, new_pixels)| (image, new_pixels, Backend::Dxgi))
                }
                Err(e) => {
                    self.dxgi = None;
                    self.last_dxgi_error = Some(e);
                    self.retry_at_ms = now.saturating_add(2000);
                }
            }
        }
        if self.dxgi.is_none() {
            // Refresh exact identity before every fallback BitBlt: never read
            // a different monitor left behind at the old desktop coordinates.
            let monitor = match selected_monitor(&self.name) {
                Ok(m) => m,
                Err(e) => {
                    self.reset();
                    return Err(e);
                }
            };
            result = Some((
                self.gdi.poll(&monitor, crop, &mut self.counters)?,
                true,
                Backend::Gdi,
            ));
        }
        if !desktop_accessible() {
            self.reset();
            return Err("Desktop changed during capture; frame discarded".into());
        }
        if let Some((image, new_pixels, backend)) = result {
            self.counters.frames = self.counters.frames.saturating_add(1);
            Ok(Some(Frame {
                image,
                new_pixels,
                backend,
                observed_ms: monotonic_ms(),
                manual_crop: crop,
            }))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
    #[test]
    fn software_texture_readback_and_cache_without_desktop_access() {
        let (device, context) = device(None, D3D_DRIVER_TYPE_WARP).unwrap();
        let desc = D3D11_TEXTURE2D_DESC {
            Width: 8,
            Height: 4,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            ..Default::default()
        };
        let bytes: Vec<u8> = (0..32).flat_map(|i| [30, 60, 120, i as u8]).collect();
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: bytes.as_ptr().cast(),
            SysMemPitch: 32,
            SysMemSlicePitch: 128,
        };
        let mut texture = None;
        unsafe { device.CreateTexture2D(&desc, Some(&init), Some(&mut texture)) }.unwrap();
        let mut readback = Readback {
            device,
            context,
            staging: None,
            cached: None,
        };
        let mut counters = Counters::default();
        readback.copy(&texture.unwrap()).unwrap();
        let full = readback.image(None, &mut counters).unwrap();
        assert!(
            full.pixels()
                .iter()
                .all(|p| *p == crate::lighting::color::Rgb::new(120, 60, 30))
        );
        assert!(Arc::ptr_eq(&full, &readback.cached(None).unwrap()));
        let crop = Rect {
            x: 2,
            y: 1,
            width: 4,
            height: 2,
        };
        assert!(readback.cached(Some(crop)).is_none());
        let recropped = readback.image(Some(crop), &mut counters).unwrap();
        assert!(
            recropped
                .pixels()
                .iter()
                .all(|p| *p == crate::lighting::color::Rgb::new(120, 60, 30))
        );
        assert_eq!(counters.source_readbacks, 0); // copy() is not a presentation counter
    }
}
