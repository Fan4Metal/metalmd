//! Pictures made from document text off the UI thread: formulas and
//! diagrams.
//!
//! A worker turns a source (TeX, Mermaid) into an image at the scale it is
//! shown at, plus whatever the viewer needs to lay it out (its size), which
//! does not depend on the scale. After a zoom or a resize the old texture is
//! drawn scaled until the new one arrives. Until the first one arrives the
//! viewer shows the source as code.

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

struct Entry<M> {
    picture: Option<Arc<Picture<M>>>,
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
            image: None,
            pending_scale: None,
            error: None,
        }
    }
}

struct Inner<M> {
    cache: Mutex<HashMap<Key, Entry<M>>>,
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
                cache: Mutex::new(HashMap::new()),
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
        cache.get(&key(source, variant))?.picture.clone()
    }

    /// The picture of `source` in `variant`, its texture made at `scale` or
    /// on its way there.
    pub fn get(&self, ctx: &Context, source: &str, variant: u64, scale: f32) -> State<M> {
        let key = key(source, variant);
        // Small changes of scale (a resize by a few points) keep the texture.
        let scale = (scale * 32.0).round() / 32.0;
        let mut cache = self.inner.cache.lock().expect("render cache lock");
        let entry = cache.entry(key).or_default();
        if let Some((rendered, at)) = entry.image.take() {
            let texture =
                ctx.load_texture(self.inner.name, rendered.image, self.inner.texture_options);
            entry.picture = Some(Arc::new(Picture {
                meta: rendered.meta,
                texture,
                scale: at,
            }));
        }
        if let Some(error) = &entry.error {
            return State::Failed(Arc::clone(error));
        }
        let picture = entry.picture.clone();
        if entry.pending_scale != Some(scale) {
            entry.pending_scale = Some(scale);
            drop(cache);
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
            .get(&job.key)
            .and_then(|e| e.pending_scale);
        if wanted != Some(job.scale) {
            continue;
        }
        let result = (inner.render)(&job.source, job.key.1, job.scale);
        let mut cache = inner.cache.lock().expect("render cache lock");
        let entry = cache.entry(job.key).or_default();
        match result {
            Ok(rendered) => entry.image = Some((rendered, job.scale)),
            Err(error) => entry.error = Some(error.into()),
        }
        drop(cache);
        (inner.repaint)();
    }
}

/// Rasterises `tree` scaled by `scale` into an image `size` pixels large.
pub fn rasterize(
    tree: &resvg::usvg::Tree,
    scale: f32,
    size: [u32; 2],
) -> Result<ColorImage, String> {
    const MAX_SIDE: u32 = 8192;
    let [w, h] = size;
    if !(1..=MAX_SIDE).contains(&w) || !(1..=MAX_SIDE).contains(&h) {
        return Err(format!("picture too large ({w}x{h} px)"));
    }
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).ok_or("bad picture size")?;
    resvg::render(
        tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(ColorImage::from_rgba_premultiplied(
        [w as usize, h as usize],
        pixmap.data(),
    ))
}
