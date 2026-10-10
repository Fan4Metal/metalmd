//! Animated images beyond what egui does on its own.
//!
//! egui animates GIF and WebP only when the URI's path ends in `.gif` or
//! `.webp`: it then asks the loaders for `uri#frame`. Images whose URI hides
//! the format (`demo.gif?raw=true`, GitHub's extensionless
//! `user-attachments/assets/…` links) showed only their first frame, and
//! APNG had no loader at all. For those, `frame_uri` sniffs the loaded bytes
//! and picks the frame itself; egui_extras' GIF and WebP loaders accept any
//! `uri#frame`, and `ApngLoader` handles the PNG case.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::load::{BytesPoll, ImageLoadResult, ImageLoader, ImagePoll, LoadError, SizeHint};
use egui::{ColorImage, Context, FrameDurations, Id};
use image::AnimationDecoder as _;
use image::codecs::png::PngDecoder;

/// Browsers show frames with a delay of 10 ms or less for 100 ms; many GIFs
/// rely on it. egui would show such a GIF as a still image.
const MIN_DELAY: Duration = Duration::from_millis(11);
const DEFAULT_DELAY: Duration = Duration::from_millis(100);

/// The URI to hand to `egui::Image` for `uri`. Returns `uri#frame` for an
/// animated image that egui does not recognise by extension, once its bytes
/// are loaded; `None` means use `uri` as it is.
pub fn frame_uri(ctx: &Context, uri: &str) -> Option<String> {
    fix_delays(ctx, uri);
    if egui::load::has_extension(uri, "gif") || egui::load::has_extension(uri, "webp") {
        return None;
    }
    let verdict_id = Id::new(("qmd-animated", uri));
    let animated = match ctx.data(|d| d.get_temp::<bool>(verdict_id)) {
        Some(animated) => animated,
        None => {
            let Ok(BytesPoll::Ready { bytes, .. }) = ctx.try_load_bytes(uri) else {
                return None;
            };
            let animated = egui::has_gif_magic_header(&bytes)
                || egui::has_webp_header(&bytes)
                || is_apng(&bytes);
            ctx.data_mut(|d| d.insert_temp(verdict_id, animated));
            animated
        }
    };
    animated.then(|| format!("{uri}#{}", frame_index(ctx, uri)))
}

/// Rewrites the frame delays a loader stored for `uri` the way browsers
/// read them. Runs before egui picks a frame for `.gif` and `.webp` URIs.
fn fix_delays(ctx: &Context, uri: &str) {
    let id = Id::new(uri);
    let Some(durations) = ctx.data(|d| d.get_temp::<FrameDurations>(id)) else {
        return;
    };
    if durations.all().len() > 1 && durations.all().any(|d| *d < MIN_DELAY) {
        let fixed = FrameDurations::new(durations.all().map(|d| browser_delay(*d)).collect());
        ctx.data_mut(|d| d.insert_temp(id, fixed));
    }
}

fn browser_delay(delay: Duration) -> Duration {
    if delay < MIN_DELAY {
        DEFAULT_DELAY
    } else {
        delay
    }
}

/// The frame to show now, as egui computes it for its own animated images,
/// and a repaint request for when it changes. A single frame never repaints.
fn frame_index(ctx: &Context, uri: &str) -> usize {
    let Some(durations) = ctx.data(|d| d.get_temp::<FrameDurations>(Id::new(uri))) else {
        return 0;
    };
    if durations.all().len() < 2 {
        return 0;
    }
    let total: u128 = durations.all().map(Duration::as_millis).sum();
    let now = ctx.input(|i| Duration::from_secs_f64(i.time)).as_millis();
    let pos = now % total.max(1);
    let mut end = 0;
    for (index, delay) in durations.all().enumerate() {
        end += delay.as_millis();
        if pos < end {
            ctx.request_repaint_after(Duration::from_millis((end - pos) as u64));
            return index;
        }
    }
    0
}

/// A PNG with an `acTL` chunk before its image data.
pub fn is_apng(bytes: &[u8]) -> bool {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if !bytes.starts_with(SIGNATURE) {
        return false;
    }
    let mut pos = SIGNATURE.len();
    while let Some(header) = bytes.get(pos..pos + 8) {
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        match &header[4..8] {
            b"acTL" => return true,
            b"IDAT" | b"IEND" => return false,
            _ => pos = pos.saturating_add(12).saturating_add(len),
        }
    }
    false
}

type Frames = Result<Arc<Vec<Arc<ColorImage>>>, String>;

/// Loads `uri#frame` for animated PNGs. `frame_uri` is what asks for it; any
/// other URI is left to egui_extras' loaders.
#[derive(Default)]
pub struct ApngLoader {
    cache: Mutex<HashMap<String, Frames>>,
}

impl ApngLoader {
    pub const ID: &'static str = egui::generate_loader_id!(ApngLoader);

    pub fn install(ctx: &Context) {
        if !ctx.is_loader_installed(Self::ID) {
            ctx.add_image_loader(Arc::new(Self::default()));
        }
    }
}

fn decode_apng(bytes: &[u8]) -> Result<(Vec<Arc<ColorImage>>, FrameDurations), String> {
    let error = |e: image::ImageError| format!("Failed to decode APNG: {e}");
    let decoder = PngDecoder::new(Cursor::new(bytes)).map_err(error)?;
    let mut images = Vec::new();
    let mut delays = Vec::new();
    for frame in decoder.apng().map_err(error)?.into_frames() {
        let frame = frame.map_err(error)?;
        delays.push(browser_delay(frame.delay().into()));
        let buffer = frame.buffer();
        images.push(Arc::new(ColorImage::from_rgba_unmultiplied(
            [buffer.width() as usize, buffer.height() as usize],
            buffer.as_raw(),
        )));
    }
    if images.is_empty() {
        return Err("APNG has no frames".to_owned());
    }
    Ok((images, FrameDurations::new(delays)))
}

impl ImageLoader for ApngLoader {
    fn id(&self) -> &str {
        Self::ID
    }

    fn load(&self, ctx: &Context, frame_uri: &str, _: SizeHint) -> ImageLoadResult {
        let (uri, index) =
            egui::decode_animated_image_uri(frame_uri).map_err(|_| LoadError::NotSupported)?;
        let frame = |frames: &[Arc<ColorImage>]| ImagePoll::Ready {
            image: Arc::clone(&frames[index % frames.len()]),
        };
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = cache.get(uri) {
            return match entry {
                Ok(frames) => Ok(frame(frames)),
                Err(e) => Err(LoadError::Loading(e.clone())),
            };
        }
        match ctx.try_load_bytes(uri)? {
            BytesPoll::Pending { size } => Ok(ImagePoll::Pending { size }),
            BytesPoll::Ready { bytes, .. } => {
                if !is_apng(&bytes) {
                    return Err(LoadError::NotSupported);
                }
                match decode_apng(&bytes) {
                    Ok((frames, durations)) => {
                        ctx.data_mut(|d| d.insert_temp(Id::new(uri), durations));
                        let frames = Arc::new(frames);
                        let poll = frame(&frames);
                        cache.insert(uri.to_owned(), Ok(frames));
                        Ok(poll)
                    }
                    Err(e) => {
                        cache.insert(uri.to_owned(), Err(e.clone()));
                        Err(LoadError::Loading(e))
                    }
                }
            }
        }
    }

    fn forget(&self, uri: &str) {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(uri);
    }

    fn forget_all(&self) {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    fn byte_size(&self) -> usize {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .values()
            .map(|entry| match entry {
                Ok(frames) => frames.iter().map(|f| f.pixels.len() * 4).sum(),
                Err(e) => e.len(),
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&[0; 4]); // CRC, not checked
        out
    }

    fn png(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, data) in chunks {
            out.extend(chunk(kind, data));
        }
        out
    }

    #[test]
    fn apng_is_told_apart_from_png() {
        let ihdr = [0u8; 13];
        let actl = [0u8; 8];
        assert!(is_apng(&png(&[
            (b"IHDR", &ihdr),
            (b"acTL", &actl),
            (b"IDAT", &[])
        ])));
        assert!(!is_apng(&png(&[
            (b"IHDR", &ihdr),
            (b"IDAT", &[]),
            (b"acTL", &actl)
        ])));
        assert!(!is_apng(&png(&[(b"IHDR", &ihdr)])));
        assert!(!is_apng(b"GIF89a"));
    }

    #[test]
    fn tiny_delays_play_at_browser_speed() {
        assert_eq!(browser_delay(Duration::ZERO), DEFAULT_DELAY);
        assert_eq!(browser_delay(Duration::from_millis(10)), DEFAULT_DELAY);
        assert_eq!(
            browser_delay(Duration::from_millis(40)),
            Duration::from_millis(40)
        );
    }
}
