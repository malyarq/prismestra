//! Approved D1 mark, rasterized directly for the window and tray icon.
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIcon, DestroyIcon, HICON};

const SIZE: usize = 32;

pub(super) struct Icon(HICON);
impl Icon {
    pub(super) fn new() -> Result<Self, String> {
        let (mask, pixels) = raster()?;
        let icon = unsafe {
            CreateIcon(
                std::ptr::null_mut(),
                SIZE as i32,
                SIZE as i32,
                1,
                32,
                mask.as_ptr(),
                pixels.as_ptr(),
            )
        };
        if icon.is_null() {
            Err(format!(
                "Create brand icon: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(Self(icon))
        }
    }
    pub(super) fn handle(&self) -> HICON {
        self.0
    }
}
impl Drop for Icon {
    fn drop(&mut self) {
        unsafe {
            DestroyIcon(self.0);
        }
    }
}

fn raster() -> Result<([u8; SIZE * SIZE / 8], Vec<u8>), String> {
    let mut pixmap = Pixmap::new(SIZE as u32, SIZE as u32).ok_or("Allocate brand icon")?;
    let mut path = PathBuilder::new();
    // M4 6H36L24 17Z; M4 10L24 21L17 33Z; M4 16L13 34H4Z.
    for polygon in [
        &[(4., 6.), (36., 6.), (24., 17.)][..],
        &[(4., 10.), (24., 21.), (17., 33.)][..],
        &[(4., 16.), (13., 34.), (4., 34.)][..],
    ] {
        path.move_to(polygon[0].0, polygon[0].1);
        for &(x, y) in &polygon[1..] {
            path.line_to(x, y);
        }
        path.close();
    }
    let path = path.finish().ok_or("Build brand icon path")?;
    let mut paint = Paint::default();
    paint.set_color_rgba8(0xe9, 0xed, 0xf1, 255);
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::from_scale(SIZE as f32 / 40., SIZE as f32 / 40.),
        None,
    );
    let mut pixels = pixmap.take(); // tiny-skia stores premultiplied RGBA, top row first.
    let mut mask = [0u8; SIZE * SIZE / 8]; // 32px rows are already DWORD aligned.
    for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        pixel.swap(0, 2); // CreateIcon consumes premultiplied BGRA, also top row first.
        if pixel[3] == 0 {
            mask[index / 8] |= 0x80 >> (index % 8);
        }
    }
    Ok((mask, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn approved_mark_has_premultiplied_color_and_transparency() {
        let (mask, pixels) = raster().unwrap();
        assert_eq!(pixels.len(), SIZE * SIZE * 4);
        assert_eq!(&pixels[(6 * SIZE + 8) * 4..][..4], &[0xf1, 0xed, 0xe9, 255]);
        assert_eq!(&pixels[..4], &[0, 0, 0, 0]);
        for (index, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
            assert!(pixel[..3].iter().all(|channel| *channel <= pixel[3]));
            assert_eq!(mask[index / 8] & (0x80 >> (index % 8)) != 0, pixel[3] == 0);
        }
        assert!(!Icon::new().unwrap().handle().is_null());
    }
}
