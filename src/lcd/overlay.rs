//! Compact188x76 card with vector icons. Independent Rust rasterizer, not Qt pixel parity.
//! Font bytes are supplied locally by the host; proprietary system fonts are not bundled.
use image::{
    Rgb, RgbImage, RgbaImage,
    imageops::{self, FilterType},
};
use std::collections::HashMap;
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

pub struct Renderer {
    font: fontdue::Font,
    glyphs: HashMap<char, (fontdue::Metrics, Vec<u8>)>,
}
fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> tiny_skia::Path {
    let mut p = PathBuilder::new();
    let k = r * 0.5522848;
    p.move_to(x + r, y);
    p.line_to(x + w - r, y);
    p.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    p.line_to(x + w, y + h - r);
    p.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    p.line_to(x + r, y + h);
    p.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    p.line_to(x, y + r);
    p.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    p.close();
    p.finish().unwrap()
}
fn line(p: &mut PathBuilder, x: f32, y: f32, x2: f32, y2: f32) {
    p.move_to(x, y);
    p.line_to(x2, y2);
}
fn icon(label: &str) -> tiny_skia::Path {
    let mut p = PathBuilder::new();
    if label.starts_with("CPU") {
        p.push_path(&rounded(5., 5., 16., 16., 2.));
        p.push_rect(tiny_skia::Rect::from_xywh(9., 9., 8., 8.).unwrap());
        for n in [8., 13., 18.] {
            line(&mut p, n, 1., n, 5.);
            line(&mut p, n, 21., n, 25.);
            line(&mut p, 1., n, 5., n);
            line(&mut p, 21., n, 25., n);
        }
    } else if label.contains("GPU") {
        p.push_path(&rounded(1., 6., 24., 15., 2.));
        p.push_circle(8., 13.5, 3.5);
        p.push_circle(18., 13.5, 3.5);
        line(&mut p, 5., 21., 5., 24.);
        line(&mut p, 8., 24., 19., 24.);
    } else if label.starts_with("RAM") {
        p.push_path(&rounded(1., 6., 24., 13., 1.5));
        for x in [4., 10., 16.] {
            p.push_path(&rounded(x, 9., 5., 6., 0.5));
        }
        for x in [4., 7., 10., 13., 16., 19., 22.] {
            line(&mut p, x, 19., x, 22.);
        }
    } else if label == "Жидкость" || label.to_lowercase().contains("liquid") {
        p.move_to(13., 1.);
        p.cubic_to(10., 7., 4., 12., 4., 17.);
        p.cubic_to(4., 28., 22., 28., 22., 17.);
        p.cubic_to(22., 12., 16., 7., 13., 1.);
        p.move_to(8.1, 17.2);
        p.cubic_to(8.3, 20.5, 10., 22., 13., 22.);
    } else {
        p.push_path(&rounded(10., 2., 6., 16., 3.));
        p.push_circle(13., 20., 5.);
        line(&mut p, 13., 7., 13., 19.);
    }
    p.finish().unwrap()
}
impl Renderer {
    pub fn new(font_bytes: Vec<u8>) -> Result<Self, String> {
        if font_bytes.is_empty() || font_bytes.len() > 32 * 1024 * 1024 {
            return Err("Invalid font size".into());
        }
        let font = fontdue::Font::from_bytes(font_bytes, fontdue::FontSettings::default())
            .map_err(str::to_owned)?;
        if font.horizontal_line_metrics(22.).is_none() {
            return Err("Font has no horizontal metrics".into());
        }
        Ok(Self {
            font,
            glyphs: HashMap::new(),
        })
    }
    fn width(&self, text: &str) -> f32 {
        text.chars()
            .map(|c| self.font.metrics(c, 22.).advance_width)
            .sum()
    }
    fn title(&self, label: &str) -> String {
        if self.width(label) <= 118. {
            return label.into();
        }
        let ellipsis = self.width("…");
        let mut result = String::new();
        let mut width = 0.;
        for c in label.chars() {
            let next = self.font.metrics(c, 22.).advance_width;
            if width + next + ellipsis > 118. {
                break;
            }
            result.push(c);
            width += next;
        }
        result.push('…');
        result
    }
    fn text(&mut self, pixmap: &mut Pixmap, text: &str, y: i32) {
        let mut x = 208. + (118. - self.width(text)) / 2.;
        let metrics = self.font.horizontal_line_metrics(22.).unwrap();
        let baseline = y as f32 + (34. - (metrics.ascent - metrics.descent)) / 2. + metrics.ascent;
        for c in text.chars() {
            if !self.glyphs.contains_key(&c) {
                if self.glyphs.len() >= 256 {
                    self.glyphs.clear();
                }
                self.glyphs.insert(c, self.font.rasterize(c, 22.));
            }
            let (metrics, coverage) = &self.glyphs[&c];
            let gx = x.round() as i32 + metrics.xmin;
            let gy = baseline.round() as i32 - metrics.ymin - metrics.height as i32;
            for dy in 0..metrics.height {
                for dx in 0..metrics.width {
                    let px = gx + dx as i32;
                    let py = gy + dy as i32;
                    if !(208..326).contains(&px) || !(y..y + 34).contains(&py) {
                        continue;
                    }
                    let a = u16::from(coverage[dy * metrics.width + dx]);
                    let offset = ((py * 480 + px) * 4) as usize;
                    let out = &mut pixmap.data_mut()[offset..offset + 4];
                    for channel in out.iter_mut().take(3) {
                        *channel = (a + (u16::from(*channel) * (255 - a) + 127) / 255) as u8;
                    }
                    out[3] = (a + (u16::from(out[3]) * (255 - a) + 127) / 255) as u8;
                }
            }
            x += metrics.advance_width;
        }
    }
    pub fn render(
        &mut self,
        background: &RgbImage,
        angle: i32,
        label: &str,
        celsius: f64,
    ) -> Result<RgbImage, String> {
        let (width, height) = background.dimensions();
        if !(128..=2048).contains(&width) || !(128..=2048).contains(&height) {
            return Err("Invalid background dimensions".into());
        }
        let Ok(angle) = super::media::rotation(angle) else {
            return Ok(background.clone());
        };
        let name = label.split_whitespace().collect::<Vec<_>>().join(" ");
        if !celsius.is_finite()
            || !(-20.0..=150.0).contains(&celsius)
            || name.is_empty()
            || name.encode_utf16().count() > 4096
        {
            return Ok(background.clone());
        }
        let mut view = match angle {
            90 => imageops::rotate270(background),
            180 => imageops::rotate180(background),
            270 => imageops::rotate90(background),
            _ => background.clone(),
        };
        let mut layer = Pixmap::new(480, 480).ok_or("Cannot allocate overlay")?;
        let panel = rounded(146., 350., 188., 76., 10.);
        let mut paint = Paint::default();
        paint.set_color_rgba8(16, 16, 14, 202);
        layer.fill_path(
            &panel,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
        paint.set_color_rgba8(255, 247, 231, 32);
        layer.stroke_path(
            &panel,
            &paint,
            &Stroke {
                width: 1.,
                ..Stroke::default()
            },
            Transform::identity(),
            None,
        );
        paint.set_color_rgba8(255, 255, 255, 255);
        let transform = Transform::from_row(48. / 26., 0., 0., 48. / 26., 154., 364.);
        layer.stroke_path(
            &icon(&name),
            &paint,
            &Stroke {
                width: 1.8,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            },
            transform,
            None,
        );
        self.text(&mut layer, &self.title(&name), 354);
        // Actual Qt QString fixed0 uses ties away from zero and suppresses -0.
        let reading = format!("{}°C", celsius.round() as i32);
        self.text(&mut layer, &reading, 388);
        let rgba = RgbaImage::from_raw(480, 480, layer.take()).ok_or("Invalid overlay raster")?;
        let rgba = if view.dimensions() == (480, 480) {
            rgba
        } else {
            imageops::resize(&rgba, view.width(), view.height(), FilterType::Triangle)
        };
        // Layer channels are premultiplied. Never multiply source color by alpha twice.
        for (x, y, pixel) in view.enumerate_pixels_mut() {
            let source = rgba.get_pixel(x, y).0;
            let a = u16::from(source[3]);
            *pixel = Rgb([0, 1, 2].map(|i| {
                (u16::from(source[i]) + (u16::from(pixel[i]) * (255 - a) + 127) / 255).min(255)
                    as u8
            }));
        }
        Ok(match angle {
            90 => imageops::rotate90(&view),
            180 => imageops::rotate180(&view),
            270 => imageops::rotate270(&view),
            _ => view,
        })
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    fn renderer() -> Renderer {
        let path = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("Fonts/seguibl.ttf");
        Renderer::new(std::fs::read(path).unwrap()).unwrap()
    }
    #[test]
    fn compact_icons_and_invalid_values_preserve_background() {
        let mut r = renderer();
        let image = RgbImage::from_pixel(480, 480, Rgb([100, 40, 10]));
        for label in ["CPU", "GPU", "RAM", "Liquid"] {
            let result = r.render(&image, 0, label, 42.).unwrap();
            assert_eq!(result.get_pixel(100, 100), image.get_pixel(100, 100));
            assert_ne!(result, image);
            assert_eq!(r.render(&image, 0, label, f64::NAN).unwrap(), image);
        }
        assert_eq!(r.render(&image, 45, "CPU", 42.).unwrap(), image);
        assert_eq!(r.render(&image, 0, "  ", 42.).unwrap(), image);
    }
    #[test]
    fn rotations_preserve_viewing_orientation_and_preview_geometry() {
        let mut r = renderer();
        let image = RgbImage::from_pixel(480, 480, Rgb([100, 40, 10]));
        let normal = r.render(&image, 0, "Liquid", 33.4).unwrap();
        for angle in [90, 180, 270] {
            let rotated = match angle {
                90 => imageops::rotate90(&image),
                180 => imageops::rotate180(&image),
                _ => imageops::rotate270(&image),
            };
            let output = r.render(&rotated, angle, "Liquid", 33.4).unwrap();
            let expected = match angle {
                90 => imageops::rotate90(&normal),
                180 => imageops::rotate180(&normal),
                _ => imageops::rotate270(&normal),
            };
            assert_eq!(output, expected);
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("work/lcd-stand");
        std::fs::create_dir_all(&path).unwrap();
        let mut grid = RgbImage::new(960, 960);
        for (i, label) in ["CPU", "GPU", "RAM", "Liquid"].iter().enumerate() {
            let output = r.render(&image, 0, label, 33.4 + i as f64 * 10.).unwrap();
            imageops::replace(
                &mut grid,
                &output,
                (i % 2 * 480) as i64,
                (i / 2 * 480) as i64,
            );
        }
        grid.save(path.join("overlay-icons.png")).unwrap();
    }
}
