//! Pictures made from document text off the UI thread: formulas, emoji and
//! diagrams.
//!
//! A worker turns a source (TeX, an emoji, Mermaid) into an image at the
//! scale it is shown at, plus whatever the viewer needs to lay it out (its
//! size), which does not depend on the scale. After a zoom or a resize the
//! old texture is drawn scaled until the new one arrives. Until the first
//! one arrives the viewer shows the source as code or text.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use egui::{ColorImage, Context, TextureHandle, TextureOptions};

/// What a worker makes of a source.
pub struct Rendered<M> {
    pub image: ColorImage,
    /// Layout information independent of the scale.
    pub meta: M,
}

/// A picture ready to draw.
pub struct Picture<M> {
    pub meta: M,
    pub texture: TextureHandle,
    /// The scale `texture` was made at.
    pub scale: f32,
}

pub enum State<M> {
    Ready(Arc<Picture<M>>),
    Pending,
    Failed(Arc<str>),
}

/// Makes the picture of a source in a variant (style, colors) at a scale.
pub type RenderFn<M> = fn(&str, u64, f32) -> Result<Rendered<M>, String>;

/// The source's hash and the variant.
type Key = (u64, u64);

struct Job {
    key: Key,
    source: String,
    scale: f32,
}

/// Bytes of textures kept. Past this, pictures not asked for in the last
/// [`MIN_IDLE`] seconds are dropped, those asked for longest ago first, and
/// made again if shown again. Nothing asked for more recently goes: the
/// pictures of blocks still being measured off screen are asked for only
/// every few frames, and dropping them would have them made again forever.
const MAX_BYTES: usize = 256 * 1024 * 1024;
const MIN_IDLE: f64 = 30.0;

struct Entry<M> {
    picture: Option<Arc<Picture<M>>>,
    /// Bytes of `picture`'s texture.
    bytes: usize,
    /// When it was last asked for, in egui's seconds.
    last_used: f64,
    /// The image made for `pending_scale`, not yet a texture.
    image: Option<(Rendered<M>, f32)>,
    /// A rendering at this scale is queued or done.
    pending_scale: Option<f32>,
    error: Option<Arc<str>>,
}

impl<M> Default for Entry<M> {
    fn default() -> Self {
        Entry {
            picture: None,
            bytes: 0,
            last_used: 0.0,
            image: None,
            pending_scale: None,
            error: None,
        }
    }
}

struct Cache<M> {
    entries: HashMap<Key, Entry<M>>,
    /// The sum of the entries' `bytes`.
    bytes: usize,
}

impl<M> Cache<M> {
    /// If over [`MAX_BYTES`], drops the pictures idle for [`MIN_IDLE`]
    /// seconds, those asked for longest ago first, down to three quarters.
    fn evict(&mut self, now: f64) {
        if self.bytes <= MAX_BYTES {
            return;
        }
        let mut by_age: Vec<(f64, Key)> = self
            .entries
            .iter()
            .filter(|(_, e)| e.bytes > 0 && now - e.last_used >= MIN_IDLE)
            .map(|(k, e)| (e.last_used, *k))
            .collect();
        by_age.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        for (_, key) in by_age {
            if self.bytes <= MAX_BYTES * 3 / 4 {
                break;
            }
            if let Some(entry) = self.entries.remove(&key) {
                self.bytes -= entry.bytes;
            }
        }
    }
}

struct Inner<M> {
    cache: Mutex<Cache<M>>,
    sender: Mutex<Option<Sender<Job>>>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    render: RenderFn<M>,
    name: &'static str,
    texture_options: TextureOptions,
}

pub struct RenderCache<M> {
    inner: Arc<Inner<M>>,
}

impl<M> Clone for RenderCache<M> {
    fn clone(&self) -> Self {
        RenderCache {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<M: Send + Sync + 'static> RenderCache<M> {
    /// A cache whose worker thread, `name`, starts with the first request.
    pub fn new(
        name: &'static str,
        render: RenderFn<M>,
        texture_options: TextureOptions,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> RenderCache<M> {
        RenderCache {
            inner: Arc::new(Inner {
                cache: Mutex::new(Cache {
                    entries: HashMap::new(),
                    bytes: 0,
                }),
                sender: Mutex::new(None),
                repaint: Arc::new(repaint),
                render,
                name,
                texture_options,
            }),
        }
    }

    fn sender(&self) -> Sender<Job> {
        let mut guard = self.inner.sender.lock().expect("render cache lock");
        if let Some(sender) = guard.as_ref() {
            return sender.clone();
        }
        let (sender, receiver) = channel::<Job>();
        let inner = Arc::clone(&self.inner);
        std::thread::Builder::new()
            .name(self.inner.name.into())
            // Deeply nested sources recurse deeply.
            .stack_size(16 * 1024 * 1024)
            .spawn(move || worker(receiver, inner))
            .expect("spawn render thread");
        *guard = Some(sender.clone());
        sender
    }

    /// The picture of `source` in `variant` as last made, without asking
    /// for one.
    pub fn peek(&self, source: &str, variant: u64) -> Option<Arc<Picture<M>>> {
        let cache = self.inner.cache.lock().expect("render cache lock");
        cache.entries.get(&key(source, variant))?.picture.clone()
    }

    /// The picture of `source` in `variant`, its texture made at `scale` or
    /// on its way there.
    pub fn get(&self, ctx: &Context, source: &str, variant: u64, scale: f32) -> State<M> {
        let key = key(source, variant);
        // Small changes of scale (a resize by a few points) keep the texture.
        let scale = (scale * 32.0).round() / 32.0;
        let now = ctx.input(|i| i.time);
        let mut guard = self.inner.cache.lock().expect("render cache lock");
        let cache = &mut *guard;
        let entry = cache.entries.entry(key).or_default();
        entry.last_used = now;
        if let Some((rendered, at)) = entry.image.take() {
            let [w, h] = rendered.image.size;
            if w.max(h) > ctx.input(|i| i.max_texture_side) {
                // More than the GPU takes: egui_glow would assert, which
                // ends the app.
                entry.error = Some(format!("picture too large ({w}x{h} px)").into());
            } else {
                let bytes = rendered.image.pixels.len() * 4;
                let texture =
                    ctx.load_texture(self.inner.name, rendered.image, self.inner.texture_options);
                entry.picture = Some(Arc::new(Picture {
                    meta: rendered.meta,
                    texture,
                    scale: at,
                }));
                cache.bytes = cache.bytes - entry.bytes + bytes;
                entry.bytes = bytes;
            }
        }
        if let Some(error) = &entry.error {
            return State::Failed(Arc::clone(error));
        }
        let picture = entry.picture.clone();
        let pending_scale = entry.pending_scale;
        entry.pending_scale = Some(scale);
        if cache.bytes > MAX_BYTES {
            cache.evict(now);
        }
        if pending_scale != Some(scale) {
            drop(guard);
            let _ = self.sender().send(Job {
                key,
                source: source.to_owned(),
                scale,
            });
        }
        match picture {
            Some(picture) => State::Ready(picture),
            None => State::Pending,
        }
    }
}

fn key(source: &str, variant: u64) -> Key {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    (hasher.finish(), variant)
}

fn worker<M>(receiver: Receiver<Job>, inner: Arc<Inner<M>>) {
    while let Ok(job) = receiver.recv() {
        // A later request for another scale makes this one moot.
        let wanted = inner
            .cache
            .lock()
            .expect("render cache lock")
            .entries
            .get(&job.key)
            .and_then(|e| e.pending_scale);
        if wanted != Some(job.scale) {
            continue;
        }
        let result = (inner.render)(&job.source, job.key.1, job.scale);
        let mut cache = inner.cache.lock().expect("render cache lock");
        // Dropped from the cache while it was being made.
        let Some(entry) = cache.entries.get_mut(&job.key) else {
            continue;
        };
        match result {
            Ok(rendered) => entry.image = Some((rendered, job.scale)),
            Err(error) => entry.error = Some(error.into()),
        }
        drop(cache);
        (inner.repaint)();
    }
}

/// Space around a picture set in text, in pixels, so that antialiased edges
/// are not cut off.
pub const PAD_PX: f32 = 2.0;

/// The box of a picture set in text (a formula, an emoji): width, height
/// above the baseline and depth below it, in ems.
pub struct Metrics {
    pub width: f32,
    pub height: f32,
    pub depth: f32,
}

/// A picture set in text: its texture's corner is [`PAD_PX`] left of and
/// above its box at `scale` pixels per em, and may reach past the box on
/// the right and bottom.
impl Picture<Metrics> {
    /// Draws the picture at `em` points per em, its left end at `left` and
    /// its baseline at `baseline`. The texture's corner goes on a pixel, so
    /// that at the size it was made for each texel is one pixel.
    pub fn paint(&self, painter: &egui::Painter, left: f32, baseline: f32, em: f32) {
        let ppp = painter.pixels_per_point();
        let scale = em / self.scale;
        let corner = egui::pos2(
            left - PAD_PX * scale,
            baseline - self.meta.height * em - PAD_PX * scale,
        );
        let corner = egui::pos2(
            (corner.x * ppp).round() / ppp,
            (corner.y * ppp).round() / ppp,
        );
        let [w, h] = self.texture.size();
        let rect =
            egui::Rect::from_min_size(corner, egui::vec2(w as f32 * scale, h as f32 * scale));
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.image(self.texture.id(), rect, uv, egui::Color32::WHITE);
    }

    /// The picture's box at `em` points per em, without the padding.
    pub fn size(&self, em: f32) -> egui::Vec2 {
        egui::vec2(
            self.meta.width * em,
            (self.meta.height + self.meta.depth) * em,
        )
    }

    /// Height above the baseline at `em` points per em.
    pub fn ascent(&self, em: f32) -> f32 {
        self.meta.height * em
    }
}

/// Rasterises `tree` moved by `transform` into an image `size` pixels large.
pub fn rasterize(
    tree: &resvg::usvg::Tree,
    transform: resvg::tiny_skia::Transform,
    size: [u32; 2],
) -> Result<ColorImage, String> {
    const MAX_SIDE: u32 = 8192;
    let [w, h] = size;
    if !(1..=MAX_SIDE).contains(&w) || !(1..=MAX_SIDE).contains(&h) {
        return Err(format!("picture too large ({w}x{h} px)"));
    }
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).ok_or("bad picture size")?;
    resvg::render(tree, transform, &mut pixmap.as_mut());
    Ok(ColorImage::from_rgba_premultiplied(
        [w as usize, h as usize],
        pixmap.data(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB64: usize = 64 * 1024 * 1024;

    fn cache(entries: impl Iterator<Item = (f64, usize)>) -> Cache<()> {
        let mut cache = Cache {
            entries: HashMap::new(),
            bytes: 0,
        };
        for (i, (last_used, bytes)) in entries.enumerate() {
            let entry = Entry {
                last_used,
                bytes,
                ..Entry::default()
            };
            cache.bytes += bytes;
            cache.entries.insert((i as u64, 0), entry);
        }
        cache
    }

    #[test]
    fn idle_pictures_go_longest_idle_first_when_over_the_budget() {
        // Ten 64 MB pictures, the last asked for at 0, 1, … 9 s.
        let mut cache = cache((0..10).map(|i| (i as f64, MB64)));
        cache.evict(100.0);
        assert_eq!(cache.bytes, 3 * MB64);
        assert!(cache.entries.values().all(|e| e.last_used >= 7.0));
    }

    #[test]
    fn recent_pictures_stay_even_over_the_budget() {
        let mut cache = cache((0..10).map(|i| (i as f64, MB64)));
        cache.evict(20.0);
        assert_eq!(cache.entries.len(), 10);
    }

    #[test]
    fn many_small_pictures_are_no_reason_to_drop_any() {
        let mut cache = cache((0..20_000).map(|_| (0.0, 4096)));
        cache.evict(1000.0);
        assert_eq!(cache.entries.len(), 20_000);
    }
}
