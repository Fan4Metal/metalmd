//! TeX formulas, typeset by RaTeX (a port of KaTeX) off the UI thread.
//!
//! The worker lays a formula out, writes it as SVG with the glyph outlines
//! of the embedded KaTeX fonts, and rasterises it with resvg at the pixel
//! size it is shown at. The size of a formula in ems does not depend on that
//! pixel size, so after a zoom the old texture is drawn scaled until the new
//! one arrives. Until the first one arrives the viewer shows the TeX as code.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use egui::{Color32, ColorImage, Context, TextureHandle, TextureOptions};

/// Space around the formula in the image, in pixels, so that antialiased
/// edges are not cut off.
const PAD_PX: f32 = 2.0;

/// Images larger than this many pixels on a side are not made; the formula
/// is shown as code.
const MAX_SIDE_PX: f32 = 8192.0;

/// A formula ready to draw.
pub struct Formula {
    /// Width, height above the baseline and depth below it, in ems.
    pub width: f32,
    pub height: f32,
    pub depth: f32,
    /// Covers the formula plus [`PAD_PX`] on each side at `ppem` pixels per
    /// em.
    pub texture: TextureHandle,
    pub ppem: f32,
}

impl Formula {
    /// Draws the formula at `em` points per em, its left end at `left` and
    /// its baseline at `baseline`. The texture's corner goes on a pixel, so
    /// that at the size it was made for each texel is one pixel.
    pub fn paint(&self, painter: &egui::Painter, left: f32, baseline: f32, em: f32) {
        let ppp = painter.pixels_per_point();
        let scale = em / self.ppem;
        let corner = egui::pos2(
            left - PAD_PX * scale,
            baseline - self.height * em - PAD_PX * scale,
        );
        let corner = egui::pos2(
            (corner.x * ppp).round() / ppp,
            (corner.y * ppp).round() / ppp,
        );
        let [w, h] = self.texture.size();
        let rect =
            egui::Rect::from_min_size(corner, egui::vec2(w as f32 * scale, h as f32 * scale));
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.image(self.texture.id(), rect, uv, Color32::WHITE);
    }

    /// The formula's box at `em` points per em, without the padding.
    pub fn size(&self, em: f32) -> egui::Vec2 {
        egui::vec2(self.width * em, (self.height + self.depth) * em)
    }
}

pub enum MathState {
    Ready(Arc<Formula>),
    Pending,
    Failed(Arc<str>),
}

/// What a formula looks like: its TeX, display or inline style, and color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key {
    tex: u64,
    display: bool,
    color: [u8; 4],
}

impl Key {
    fn new(tex: &str, display: bool, color: Color32) -> Key {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        tex.hash(&mut hasher);
        Key {
            tex: hasher.finish(),
            display,
            color: color.to_array(),
        }
    }
}

struct Job {
    key: Key,
    tex: String,
    ppem: f32,
}

struct Rendered {
    image: ColorImage,
    width: f32,
    height: f32,
    depth: f32,
}

#[derive(Default)]
struct Entry {
    formula: Option<Arc<Formula>>,
    /// The image made for `pending_ppem`, not yet a texture.
    image: Option<(Rendered, f32)>,
    /// A rendering at this size is queued or done.
    pending_ppem: Option<f32>,
    error: Option<Arc<str>>,
}

type RepaintFn = Arc<dyn Fn() + Send + Sync>;

struct Inner {
    cache: Mutex<HashMap<Key, Entry>>,
    sender: Mutex<Option<Sender<Job>>>,
    repaint: RepaintFn,
}

#[derive(Clone)]
pub struct MathRenderer {
    inner: Arc<Inner>,
}

impl MathRenderer {
    pub fn new(repaint: impl Fn() + Send + Sync + 'static) -> MathRenderer {
        MathRenderer {
            inner: Arc::new(Inner {
                cache: Mutex::new(HashMap::new()),
                sender: Mutex::new(None),
                repaint: Arc::new(repaint),
            }),
        }
    }

    fn sender(&self) -> Sender<Job> {
        let mut guard = self.inner.sender.lock().expect("math lock");
        if let Some(sender) = guard.as_ref() {
            return sender.clone();
        }
        let (sender, receiver) = channel::<Job>();
        let inner = Arc::clone(&self.inner);
        std::thread::Builder::new()
            .name("math".into())
            // Deeply nested TeX recurses deeply.
            .stack_size(16 * 1024 * 1024)
            .spawn(move || worker(receiver, inner))
            .expect("spawn math thread");
        *guard = Some(sender.clone());
        sender
    }

    /// The formula for `tex` in `color`, its texture made for `ppem` pixels
    /// per em or on its way there.
    pub fn get(
        &self,
        ctx: &Context,
        tex: &str,
        display: bool,
        color: Color32,
        ppem: f32,
    ) -> MathState {
        let key = Key::new(tex, display, color);
        let ppem = (ppem * 4.0).round() / 4.0;
        let mut cache = self.inner.cache.lock().expect("math lock");
        let entry = cache.entry(key).or_default();
        if let Some((rendered, at)) = entry.image.take() {
            let texture = ctx.load_texture("math", rendered.image, TextureOptions::LINEAR);
            entry.formula = Some(Arc::new(Formula {
                width: rendered.width,
                height: rendered.height,
                depth: rendered.depth,
                texture,
                ppem: at,
            }));
        }
        if let Some(error) = &entry.error {
            return MathState::Failed(Arc::clone(error));
        }
        if entry.pending_ppem != Some(ppem) {
            entry.pending_ppem = Some(ppem);
            drop(cache);
            let _ = self.sender().send(Job {
                key,
                tex: tex.to_owned(),
                ppem,
            });
            cache = self.inner.cache.lock().expect("math lock");
        }
        match cache.get(&key).and_then(|e| e.formula.clone()) {
            Some(formula) => MathState::Ready(formula),
            None => MathState::Pending,
        }
    }
}

fn worker(receiver: Receiver<Job>, inner: Arc<Inner>) {
    while let Ok(job) = receiver.recv() {
        // A later request for another size makes this one moot.
        let wanted = inner
            .cache
            .lock()
            .expect("math lock")
            .get(&job.key)
            .and_then(|e| e.pending_ppem);
        if wanted != Some(job.ppem) {
            continue;
        }
        let result = render(&job.tex, job.key.display, job.key.color, job.ppem);
        let mut cache = inner.cache.lock().expect("math lock");
        let entry = cache.entry(job.key).or_default();
        match result {
            Ok(rendered) => entry.image = Some((rendered, job.ppem)),
            Err(error) => entry.error = Some(error.into()),
        }
        drop(cache);
        (inner.repaint)();
    }
}

/// Lays `tex` out and rasterises it at `ppem` pixels per em.
fn render(tex: &str, display: bool, color: [u8; 4], ppem: f32) -> Result<Rendered, String> {
    use ratex_layout::LayoutOptions;
    use ratex_types::math_style::MathStyle;

    let tex = tex.trim();
    if tex.is_empty() {
        return Err("empty formula".to_owned());
    }
    let nodes = ratex_parser::parse(tex).map_err(|e| e.to_string())?;
    let [r, g, b, a] = color.map(|c| c as f32 / 255.0);
    let options = LayoutOptions {
        style: if display {
            MathStyle::Display
        } else {
            MathStyle::Text
        },
        color: ratex_types::color::Color { r, g, b, a },
        ..LayoutOptions::default()
    };
    let list = ratex_layout::to_display_list(&ratex_layout::layout(&nodes, &options));
    let (width, height, depth) = (list.width as f32, list.height as f32, list.depth as f32);
    let svg_options = ratex_svg::SvgOptions {
        font_size: ppem as f64,
        padding: PAD_PX as f64,
        embed_glyphs: true,
        ..Default::default()
    };
    let svg = ratex_svg::render_to_svg(&list, &svg_options);
    let w = (width * ppem + 2.0 * PAD_PX).ceil();
    let h = ((height + depth) * ppem + 2.0 * PAD_PX).ceil();
    if !(1.0..=MAX_SIDE_PX).contains(&w) || !(1.0..=MAX_SIDE_PX).contains(&h) {
        return Err(format!("formula too large ({w}x{h} px)"));
    }
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default())
        .map_err(|e| e.to_string())?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w as u32, h as u32).ok_or("bad formula size")?;
    // The SVG gives its size in `pt`, which usvg turns into 4/3 as many
    // pixels; its view box is in the pixels wanted.
    let scale = (width * ppem + 2.0 * PAD_PX) / tree.size().width();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(Rendered {
        image: ColorImage::from_rgba_premultiplied([w as usize, h as usize], pixmap.data()),
        width,
        height,
        depth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formulas_get_a_size_and_pixels() {
        let rendered = render(r"\frac{a}{b} + x^2", false, [0, 0, 0, 255], 20.0).unwrap();
        assert!(rendered.width > 1.0 && rendered.height > 0.5 && rendered.depth > 0.1);
        let [w, h] = rendered.image.size;
        assert_eq!(w, (rendered.width * 20.0 + 4.0).ceil() as usize);
        assert_eq!(
            h,
            ((rendered.height + rendered.depth) * 20.0 + 4.0).ceil() as usize
        );
        assert!(rendered.image.pixels.iter().any(|p| p.a() > 200));
    }

    #[test]
    fn bad_tex_is_an_error() {
        assert!(render(r"\frac{a}{", false, [0, 0, 0, 255], 20.0).is_err());
        assert!(render(r"\nosuchcommand", true, [0, 0, 0, 255], 20.0).is_err());
        assert!(render("  ", true, [0, 0, 0, 255], 20.0).is_err());
    }
}
