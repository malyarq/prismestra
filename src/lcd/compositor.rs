//! Worker-owned compositor: compressed source, streaming decode, bounded JPEG LRU.
use super::{
    media::{self, DecodedFrames, Media},
    overlay::Renderer,
};
use std::{collections::VecDeque, sync::Arc};
const BUDGET: usize = 4 * 1024 * 1024;
const MIN_COST: usize = 16 * 1024;
struct Entry {
    index: usize,
    label: String,
    temperature: i32,
    jpeg: Arc<[u8]>,
    cost: usize,
}
pub struct Output {
    pub jpeg: Arc<[u8]>,
    pub applied: bool,
    pub hit: bool,
    pub source_fallback: bool,
}
pub struct Compositor {
    media: Arc<Media>,
    renderer: Option<Renderer>,
    source: Option<DecodedFrames>,
    next_source: usize,
    cache: VecDeque<Entry>,
    cost: usize,
    pub decoded: u64,
    pub encoded: u64,
}
impl Compositor {
    pub fn new(media: Arc<Media>, renderer: Renderer) -> Self {
        Self::configured(media, Some(renderer))
    }
    pub fn without_overlay(media: Arc<Media>) -> Self {
        Self::configured(media, None)
    }
    fn configured(media: Arc<Media>, renderer: Option<Renderer>) -> Self {
        Self {
            media,
            renderer,
            source: None,
            next_source: 0,
            cache: VecDeque::new(),
            cost: 0,
            decoded: 0,
            encoded: 0,
        }
    }
    pub fn begin_loop(&mut self) {
        // Drop decoded working buffers immediately; initialize lazily on active overlay.
        self.source = None;
        self.next_source = 0;
    }
    pub fn cache_cost(&self) -> usize {
        self.cost
    }
    pub fn cache_entries(&self) -> usize {
        self.cache.len()
    }
    pub fn compose(&mut self, index: usize, label: &str, celsius: f64) -> Result<Output, String> {
        let frame = self
            .media
            .frames
            .get(index)
            .ok_or("Frame index out of bounds")?;
        let baseline = frame.jpeg.clone();
        let mut output = Output {
            jpeg: baseline.clone(),
            applied: false,
            hit: false,
            source_fallback: false,
        };
        let name = label.split_whitespace().collect::<Vec<_>>().join(" ");
        let cacheable = celsius.is_finite()
            && (-20.0..=150.0).contains(&celsius)
            && !name.is_empty()
            && name.encode_utf16().count() <= 4096;
        let single = self.media.frames.len() == 1;
        if !single && (self.source.is_none() || index < self.next_source) {
            self.begin_loop();
            self.source = media::decoded_frames(self.media.source.clone()).ok();
        }
        let mut original = None;
        while !single && self.next_source <= index {
            self.next_source += 1;
            self.decoded = self.decoded.saturating_add(1);
            original = self
                .source
                .as_mut()
                .and_then(|decoder| decoder.next())
                .and_then(Result::ok)
                .map(|(image, _)| image);
            if original.is_none() {
                self.next_source = index + 1;
                break;
            }
        }
        output.source_fallback = !single && original.is_none();
        let temperature = celsius.round() as i32;
        if cacheable
            && !output.source_fallback
            && let Some(position) = self
                .cache
                .iter()
                .position(|e| e.index == index && e.label == name && e.temperature == temperature)
        {
            let entry = self.cache.remove(position).unwrap();
            output.jpeg = entry.jpeg.clone();
            output.hit = true;
            output.applied = true;
            self.cache.push_back(entry);
            return Ok(output);
        }
        if !cacheable {
            return Ok(output);
        }
        let background = if single {
            // Loader already rendered the immutable first frame without JPEG generation.
            self.media.thumbnail.clone()
        } else if let Some(original) = original {
            media::canvas(&original, 480, 480, false, self.media.rotation)?
        } else {
            image::load_from_memory(&baseline)
                .map_err(|e| e.to_string())?
                .into_rgb8()
        };
        let composed = self
            .renderer
            .as_mut()
            .ok_or("LCD overlay renderer is not configured")?
            .render(&background, self.media.rotation, label, celsius)?;
        self.encoded = self.encoded.saturating_add(1);
        let Ok(jpeg) = media::encode_jpeg(&composed) else {
            return Ok(output);
        };
        output.jpeg = jpeg.into();
        output.applied = true;
        if !output.source_fallback {
            let cost = (output.jpeg.len() + name.len() * 2 + 256).max(MIN_COST);
            while self.cost + cost > BUDGET {
                let Some(evicted) = self.cache.pop_front() else {
                    break;
                };
                self.cost -= evicted.cost;
            }
            if cost <= BUDGET {
                self.cache.push_back(Entry {
                    index,
                    label: name,
                    temperature,
                    jpeg: output.jpeg.clone(),
                    cost,
                });
                self.cost += cost;
            }
        }
        Ok(output)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    fn fixture() -> Compositor {
        let mut source = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut source);
            for color in [[80, 30, 10, 255], [100, 40, 10, 255]] {
                encoder
                    .encode_frame(image::Frame::new(image::RgbaImage::from_pixel(
                        8,
                        8,
                        image::Rgba(color),
                    )))
                    .unwrap();
            }
        }
        let media = Arc::new(media::prepare(source.into(), 180, || false).unwrap());
        let font = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("Fonts/seguibl.ttf");
        Compositor::new(media, Renderer::new(std::fs::read(font).unwrap()).unwrap())
    }
    #[test]
    fn loop_cache_advances_decoder_and_temperature_change_is_one_frame() {
        let mut c = fixture();
        c.begin_loop();
        assert!(!c.compose(0, "CPU", 42.4).unwrap().hit);
        assert!(!c.compose(1, "CPU", 42.4).unwrap().hit);
        let encoded = c.encoded;
        c.begin_loop();
        assert!(c.compose(0, "CPU", 42.3).unwrap().hit);
        assert_eq!(c.encoded, encoded);
        assert_eq!(c.decoded, 3);
        let output = c.compose(1, "CPU", 43.5).unwrap();
        assert!(!output.hit);
        assert!(output.applied);
        assert_eq!(c.decoded, 4);
        assert_eq!(c.encoded, encoded + 1);
        assert!(c.cache_cost() <= BUDGET);
        assert_eq!(c.cache_entries(), 3);
        assert!(c.compose(2, "CPU", 42.).is_err());
    }
    #[test]
    fn invalid_inputs_keep_original_and_do_not_cache() {
        let mut c = fixture();
        let original = c.media.frames[0].jpeg.clone();
        let output = c.compose(0, "CPU", f64::NAN).unwrap();
        assert!(!output.applied);
        assert_eq!(output.jpeg, original);
        assert_eq!(c.cache_entries(), 0);
        let a = c.compose(0, "CPU", 20.5).unwrap();
        assert!(a.applied);
        assert!(c.compose(0, "CPU", 21.).unwrap().hit);
    }
    #[test]
    fn single_frame_uses_validated_thumbnail_and_releases_loop_decoder() {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            12,
            12,
            image::Rgba([80, 30, 10, 255]),
        ))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
        let media = Arc::new(media::prepare(bytes.into_inner().into(), 180, || false).unwrap());
        let font = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("Fonts/seguibl.ttf");
        let mut c = Compositor::new(media, Renderer::new(std::fs::read(font).unwrap()).unwrap());
        c.begin_loop();
        assert!(c.source.is_none());
        assert!(c.compose(0, "CPU", 42.).unwrap().applied);
        for _ in 0..10 {
            c.begin_loop();
            assert!(c.compose(0, "CPU", 42.).unwrap().hit);
        }
        assert_eq!(c.decoded, 0);
        assert_eq!(c.encoded, 1);
        let mut animated = fixture();
        animated.compose(0, "CPU", 42.).unwrap();
        assert!(animated.source.is_some());
        animated.begin_loop();
        assert!(animated.source.is_none());
        assert_eq!(animated.cache_entries(), 1);
    }
}
