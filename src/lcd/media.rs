//! Bounded media preparation, independent of HID and cooling interfaces.
use image::{
    AnimationDecoder, ImageDecoder, ImageFormat, ImageReader, Limits, Rgb, RgbImage, RgbaImage,
    imageops::{self, FilterType},
};
use std::{
    fs::File,
    io::{Cursor, Read},
    path::Path,
    sync::Arc,
};

pub const MAX_SOURCE: usize = 32 * 1024 * 1024;
pub const MAX_FRAMES: usize = 300;
pub const FRAME_BUDGET: usize = 20000;
pub struct Frame {
    pub jpeg: Arc<[u8]>,
    pub delay_ms: u32,
}
pub struct Media {
    pub frames: Vec<Frame>,
    pub thumbnail: RgbImage,
    pub source: Arc<[u8]>,
    pub rotation: i32,
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(256 * 1024 * 1024);
    limits
}
pub fn rotation(value: i32) -> Result<i32, String> {
    if value % 90 != 0 {
        Err("Rotation must be a multiple of 90 degrees".into())
    } else {
        Ok(value.rem_euclid(360))
    }
}
/// Geometry is independent of transport support. The actual TT protocol remains480x480.
pub fn canvas(
    source: &RgbaImage,
    width: u32,
    height: u32,
    circular: bool,
    angle: i32,
) -> Result<RgbImage, String> {
    let angle = rotation(angle)?;
    let (sw, sh) = source.dimensions();
    if !(128..=2048).contains(&width)
        || !(128..=2048).contains(&height)
        || sw == 0
        || sh == 0
        || sw > 4096
        || sh > 4096
    {
        return Err("Invalid media or canvas dimensions".into());
    }
    // Flatten transparency against black before interpolation, avoiding fringes.
    let opaque = RgbImage::from_fn(sw, sh, |x, y| {
        let p = source.get_pixel(x, y).0;
        Rgb([0, 1, 2].map(|i| ((u16::from(p[i]) * u16::from(p[3]) + 127) / 255) as u8))
    });
    let scale = (f64::from(width) / f64::from(sw)).min(f64::from(height) / f64::from(sh));
    let w = (f64::from(sw) * scale).round().clamp(1.0, f64::from(width)) as u32;
    let h = (f64::from(sh) * scale)
        .round()
        .clamp(1.0, f64::from(height)) as u32;
    let resized = imageops::resize(&opaque, w, h, FilterType::Lanczos3);
    let mut result = RgbImage::new(width, height);
    imageops::replace(
        &mut result,
        &resized,
        i64::from((width - w) / 2),
        i64::from((height - h) / 2),
    );
    // Rotate inside the same device rectangle, not a width/height-swapped canvas.
    if angle != 0 {
        let before = result;
        result = RgbImage::from_fn(width, height, |x, y| {
            let dx = f64::from(x) + 0.5 - f64::from(width) / 2.0;
            let dy = f64::from(y) + 0.5 - f64::from(height) / 2.0;
            let (sx, sy) = match angle {
                90 => (dy, -dx),
                180 => (-dx, -dy),
                _ => (-dy, dx),
            };
            let sx = (sx + f64::from(width) / 2.0).floor() as i64;
            let sy = (sy + f64::from(height) / 2.0).floor() as i64;
            if sx >= 0 && sy >= 0 && sx < i64::from(width) && sy < i64::from(height) {
                *before.get_pixel(sx as u32, sy as u32)
            } else {
                Rgb([0, 0, 0])
            }
        });
    }
    if circular {
        for (x, y, pixel) in result.enumerate_pixels_mut() {
            let dx = (f64::from(x) + 0.5 - f64::from(width) / 2.0) / (f64::from(width) / 2.0);
            let dy = (f64::from(y) + 0.5 - f64::from(height) / 2.0) / (f64::from(height) / 2.0);
            if dx * dx + dy * dy > 1.0 {
                *pixel = Rgb([0, 0, 0]);
            }
        }
    }
    Ok(result)
}

/// Baseline 4:4:4 at the native spatial resolution. Quality values preserve the
/// former compatibility ladder; bytes/quantization differ from the Windows WIC encoder.
pub fn encode_jpeg(frame: &RgbImage) -> Result<Vec<u8>, String> {
    if frame.dimensions() != (480, 480) {
        return Err("TT JPEG requires480x480".into());
    }
    for quality in [75, 60, 45, 30, 20, 12] {
        let mut bytes = Vec::new();
        let mut encoder = jpeg_encoder::Encoder::new(&mut bytes, quality);
        encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::F_1_1);
        encoder
            .encode(frame.as_raw(), 480, 480, jpeg_encoder::ColorType::Rgb)
            .map_err(|e| e.to_string())?;
        normalize_component_ids(&mut bytes)?;
        super::protocol::validate_jpeg(&bytes)?;
        if bytes.len() <= FRAME_BUDGET {
            return Ok(bytes);
        }
    }
    Err("Frame exceeds the20000-byte LCD compatibility budget".into())
}

// jpeg-encoder0.7.1 uses component IDs0,1,2; the TT protocol requires1,2,3.
// Rename both frame and scan selectors, leaving coefficients/quantization untouched.
fn normalize_component_ids(bytes: &mut [u8]) -> Result<(), String> {
    let mut position = 2;
    let mut ids = None;
    while position + 3 < bytes.len() {
        if bytes[position] != 0xff {
            return Err("Invalid encoder marker".into());
        }
        position += 1;
        while position < bytes.len() && bytes[position] == 0xff {
            position += 1;
        }
        if position + 2 >= bytes.len() {
            return Err("Truncated encoder marker".into());
        }
        let marker = bytes[position];
        position += 1;
        let length = u16::from_be_bytes([bytes[position], bytes[position + 1]]) as usize;
        if length < 2 || position + length > bytes.len() {
            return Err("Invalid encoder segment".into());
        }
        if marker == 0xc0 {
            if ids.is_some() || length != 17 || bytes[position + 7] != 3 {
                return Err("Unexpected encoder frame".into());
            }
            let old = [
                bytes[position + 8],
                bytes[position + 11],
                bytes[position + 14],
            ];
            if old[0] == old[1] || old[0] == old[2] || old[1] == old[2] {
                return Err("Ambiguous encoder components".into());
            }
            for c in 0..3 {
                bytes[position + 8 + 3 * c] = c as u8 + 1;
            }
            ids = Some(old);
        } else if marker == 0xda {
            let ids = ids.ok_or("Missing encoder frame")?;
            if length != 12 || bytes[position + 2] != 3 {
                return Err("Unexpected encoder scan".into());
            }
            for c in 0..3 {
                if bytes[position + 3 + 2 * c] != ids[c] {
                    return Err("Encoder scan component mismatch".into());
                }
                bytes[position + 3 + 2 * c] = c as u8 + 1;
            }
            return Ok(());
        }
        position += length;
    }
    Err("Missing encoder scan".into())
}

pub fn load(path: &Path, angle: i32, cancelled: impl Fn() -> bool) -> Result<Media, String> {
    let mut bytes = Vec::new();
    let file = File::open(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Media must be a regular file".into());
    }
    file.take((MAX_SOURCE + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    prepare(bytes.into(), angle, cancelled)
}

pub fn prepare(
    source: Arc<[u8]>,
    angle: i32,
    cancelled: impl Fn() -> bool,
) -> Result<Media, String> {
    let angle = rotation(angle)?;
    if source.is_empty() || source.len() > MAX_SOURCE {
        return Err("Media must be1..32MiB".into());
    }
    if cancelled() {
        return Err("Media preparation cancelled".into());
    }
    let mut frames = Vec::new();
    let mut thumbnail = None;
    let mut total = 0;
    let mut append = |pixels: RgbaImage, delay_ms: u32| -> Result<(), String> {
        if cancelled() {
            return Err("Media preparation cancelled".into());
        }
        if frames.len() >= MAX_FRAMES {
            return Err("Animation exceeds300frames".into());
        }
        let frame = canvas(&pixels, 480, 480, false, angle)?;
        let jpeg = encode_jpeg(&frame)?;
        total += jpeg.len();
        if total > MAX_SOURCE {
            return Err("Encoded animation exceeds32MiB".into());
        }
        if thumbnail.is_none() {
            thumbnail = Some(frame);
        }
        frames.push(Frame {
            jpeg: jpeg.into(),
            delay_ms: delay_ms.clamp(50, 10000),
        });
        Ok(())
    };
    for frame in decoded_frames(source.clone())? {
        let (pixels, delay) = frame?;
        append(pixels, delay)?;
    }
    if cancelled() {
        return Err("Media preparation cancelled".into());
    }
    Ok(Media {
        frames,
        thumbnail: thumbnail.ok_or("Animation contains no frames")?,
        source,
        rotation: angle,
    })
}

pub(crate) type DecodedFrames = Box<dyn Iterator<Item = Result<(RgbaImage, u32), String>>>;
fn animation_frames(frames: image::Frames<'static>) -> DecodedFrames {
    Box::new(frames.map(|frame| {
        let frame = frame.map_err(|e| e.to_string())?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        Ok((
            frame.into_buffer(),
            numerator.checked_div(denominator).unwrap_or(50),
        ))
    }))
}
pub(crate) fn decoded_frames(source: Arc<[u8]>) -> Result<DecodedFrames, String> {
    let format = image::guess_format(&source).map_err(|e| e.to_string())?;
    match format {
        ImageFormat::Gif => {
            let mut decoder = image::codecs::gif::GifDecoder::new(Cursor::new(source))
                .map_err(|e| e.to_string())?;
            decoder.set_limits(limits()).map_err(|e| e.to_string())?;
            // Streaming iterator handles disposal/transparency. Do not collect RGBA frames.
            Ok(animation_frames(decoder.into_frames()))
        }
        ImageFormat::Png => {
            let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(source))
                .map_err(|e| e.to_string())?;
            decoder.set_limits(limits()).map_err(|e| e.to_string())?;
            if decoder.is_apng().map_err(|e| e.to_string())? {
                Ok(animation_frames(
                    decoder.apng().map_err(|e| e.to_string())?.into_frames(),
                ))
            } else {
                let image =
                    image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
                Ok(Box::new(std::iter::once(Ok((image.into_rgba8(), 50)))))
            }
        }
        ImageFormat::Jpeg | ImageFormat::Bmp => {
            let mut reader = ImageReader::with_format(Cursor::new(source), format);
            reader.limits(limits());
            let image = reader.decode().map_err(|e| e.to_string())?;
            Ok(Box::new(std::iter::once(Ok((image.into_rgba8(), 50)))))
        }
        _ => Err("Unsupported media format: currently GIF/PNG/JPEG/BMP".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> RgbaImage {
        RgbaImage::from_fn(480, 480, |x, y| {
            image::Rgba([
                if x < 240 { 255 } else { 0 },
                if y < 240 { 255 } else { 0 },
                0,
                255,
            ])
        })
    }
    #[test]
    fn right_angle_rotation_is_pixel_exact_and_alpha_is_black() {
        let source = source();
        let a = canvas(&source, 480, 480, false, 0).unwrap();
        assert_eq!(
            canvas(&source, 480, 480, false, 90).unwrap(),
            imageops::rotate90(&a)
        );
        assert_eq!(
            canvas(&source, 480, 480, false, -180).unwrap(),
            imageops::rotate180(&a)
        );
        let transparent = RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 0]));
        assert!(
            canvas(&transparent, 128, 128, false, 0)
                .unwrap()
                .pixels()
                .all(|p| p.0 == [0, 0, 0])
        );
    }
    #[test]
    fn canvas_dimensions_aspect_borders_and_circle() {
        let source = RgbaImage::from_pixel(400, 200, image::Rgba([255, 255, 255, 255]));
        let fitted = canvas(&source, 480, 480, false, 0).unwrap();
        assert_eq!(fitted.get_pixel(240, 0).0, [0, 0, 0]);
        assert_eq!(fitted.get_pixel(240, 240).0, [255, 255, 255]);
        let square = RgbaImage::from_pixel(128, 128, image::Rgba([255, 255, 255, 255]));
        assert_eq!(
            canvas(&square, 128, 128, true, 0)
                .unwrap()
                .get_pixel(0, 0)
                .0,
            [0, 0, 0]
        );
        assert!(canvas(&square, 127, 128, false, 0).is_err());
        assert!(canvas(&square, 128, 128, false, 45).is_err());
    }
    #[test]
    fn encoded_jpeg_remains_native_resolution_and_under_budget() {
        let frame = canvas(&source(), 480, 480, false, 0).unwrap();
        let bytes = encode_jpeg(&frame).unwrap();
        assert!(bytes.len() <= FRAME_BUDGET);
        assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 480);
        assert_eq!(&bytes[..2], &[0xff, 0xd8]);
        assert_eq!(&bytes[bytes.len() - 2..], &[0xff, 0xd9]);
    }
    #[test]
    fn component_renaming_preserves_decoded_pixels() {
        let frame = canvas(&source(), 480, 480, false, 0).unwrap();
        let mut bytes = Vec::new();
        let mut encoder = jpeg_encoder::Encoder::new(&mut bytes, 75);
        encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::F_1_1);
        encoder
            .encode(frame.as_raw(), 480, 480, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        let before = image::load_from_memory(&bytes).unwrap().into_rgb8();
        normalize_component_ids(&mut bytes).unwrap();
        super::super::protocol::validate_jpeg(&bytes).unwrap();
        assert_eq!(image::load_from_memory(&bytes).unwrap().into_rgb8(), before);
        assert!(normalize_component_ids(&mut [0xff, 0xd8, 0xff, 0xd9]).is_err());
    }
    #[test]
    fn animated_gif_delays_source_retention_and_cancellation() {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            for delay in [10, 100, 12000] {
                encoder
                    .encode_frame(image::Frame::from_parts(
                        RgbaImage::from_pixel(8, 8, image::Rgba([255, 0, 0, 255])),
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(delay, 1),
                    ))
                    .unwrap();
            }
        }
        let media = prepare(bytes.clone().into(), 180, || false).unwrap();
        assert_eq!(
            media.frames.iter().map(|f| f.delay_ms).collect::<Vec<_>>(),
            vec![50, 100, 10000]
        );
        assert_eq!(media.source.as_ref(), bytes);
        assert_eq!(media.frames.len(), 3);
        assert!(prepare(bytes.into(), 0, || true).is_err());
        assert!(prepare(Arc::from([]), 0, || false).is_err());
    }
    #[test]
    fn animated_png_is_not_silently_flattened_to_first_frame() {
        let bytes: Arc<[u8]> = Arc::from(include_bytes!("fixtures/two-frame.apng").as_slice());
        let decoded: Vec<_> = decoded_frames(bytes.clone())
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].1, 100);
        assert_eq!(decoded[1].1, 200);
        assert_eq!(decoded[0].0.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(decoded[1].0.get_pixel(0, 0).0, [0, 0, 255, 255]);
        assert_eq!(prepare(bytes, 0, || false).unwrap().frames.len(), 2);
    }
}
