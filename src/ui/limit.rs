//! Images larger than the GPU takes. egui_glow asserts that a texture's side
//! is at most `max_texture_side` (16384 on many GPUs, 8192 on some), and with
//! `panic = "abort"` a long screenshot in a README would end the app. This
//! loader stands in front of the others and halves such an image, averaging
//! texels, until it fits; its size in points stays the original one, so it
//! is drawn as large as before, only less sharp.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::load::{ImageLoadResult, ImageLoader, ImagePoll, LoadError, SizeHint};
use egui::{Color32, ColorImage, Context};

type Loader = Arc<dyn ImageLoader + Send + Sync + 'static>;
/// Reduced images by URI, with the image each was reduced from.
type Reduced = HashMap<String, (Arc<ColorImage>, Arc<ColorImage>)>;

pub struct LimitLoader {
    /// The loaders installed before this one, most recent last.
    inner: Vec<Loader>,
    reduced: Mutex<Reduced>,
}

impl LimitLoader {
    pub const ID: &'static str = egui::generate_loader_id!(LimitLoader);

    /// Installs the loader in front of every image loader installed so far,
    /// so it must come last.
    pub fn install(ctx: &Context) {
        if ctx.is_loader_installed(Self::ID) {
            return;
        }
        let inner = ctx.loaders().image.lock().clone();
        ctx.add_image_loader(Arc::new(LimitLoader {
            inner,
            reduced: Mutex::default(),
        }));
    }

    fn reduced(&self) -> std::sync::MutexGuard<'_, Reduced> {
        self.reduced.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl ImageLoader for LimitLoader {
    fn id(&self) -> &str {
        Self::ID
    }

    fn load(&self, ctx: &Context, uri: &str, hint: SizeHint) -> ImageLoadResult {
        // As `Context::try_load_image` does: the most recent loader first.
        // If none takes it, egui tries them itself and reports the error.
        let mut result = Err(LoadError::NotSupported);
        for loader in self.inner.iter().rev() {
            match loader.load(ctx, uri, hint) {
                Err(LoadError::NotSupported) => {}
                other => {
                    result = other;
                    break;
                }
            }
        }
        let image = match result {
            Ok(ImagePoll::Ready { image }) => image,
            other => return other,
        };
        let max_side = ctx.input(|i| i.max_texture_side);
        if image.size[0] <= max_side && image.size[1] <= max_side {
            return Ok(ImagePoll::Ready { image });
        }
        let mut reduced = self.reduced();
        if let Some((source, small)) = reduced.get(uri)
            && Arc::ptr_eq(source, &image)
        {
            return Ok(ImagePoll::Ready {
                image: Arc::clone(small),
            });
        }
        let small = Arc::new(fit(&image, max_side));
        reduced.insert(uri.to_owned(), (image, Arc::clone(&small)));
        Ok(ImagePoll::Ready { image: small })
    }

    fn forget(&self, uri: &str) {
        self.reduced().remove(uri);
    }

    fn forget_all(&self) {
        self.reduced().clear();
    }

    fn byte_size(&self) -> usize {
        self.reduced()
            .values()
            .map(|(_, small)| small.pixels.len() * 4)
            .sum()
    }
}

/// `image` halved until neither side exceeds `max_side`, with the same
/// size in points.
fn fit(image: &ColorImage, max_side: usize) -> ColorImage {
    let max_side = max_side.max(1);
    let [width, height] = image.size;
    let mut factor = 1;
    while width.div_ceil(factor) > max_side || height.div_ceil(factor) > max_side {
        factor *= 2;
    }
    let (w, h) = (width.div_ceil(factor), height.div_ceil(factor));
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        let rows = y * factor..((y + 1) * factor).min(height);
        for x in 0..w {
            let columns = x * factor..((x + 1) * factor).min(width);
            let mut sum = [0u64; 4];
            for row in rows.clone() {
                for texel in &image.pixels[row * width + columns.start..row * width + columns.end] {
                    for (s, c) in sum.iter_mut().zip(texel.to_array()) {
                        *s += u64::from(c);
                    }
                }
            }
            let count = (rows.len() * columns.len()) as u64;
            let [r, g, b, a] = sum.map(|s| (s / count) as u8);
            // The texels are premultiplied, so their average is too.
            pixels.push(Color32::from_rgba_premultiplied(r, g, b, a));
        }
    }
    ColorImage {
        size: [w, h],
        source_size: image.source_size,
        pixels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_images_are_halved_until_they_fit() {
        let mut image = ColorImage::filled([10, 3], Color32::WHITE);
        image.pixels[0] = Color32::BLACK;
        let small = fit(&image, 4);
        // 10 → 5 → 3 texels wide; the size in points is kept.
        assert_eq!(small.size, [3, 1]);
        assert_eq!(small.source_size, image.source_size);
        // The first texel averages one black and eleven white ones.
        let first = small.pixels[0].to_array();
        assert!(first[0] > 200 && first[0] < 255, "{first:?}");
        assert_eq!(first[3], 255);
    }
}
