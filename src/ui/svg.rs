//! SVG images, replacing egui_extras' SVG loader.
//!
//! That loader takes only URIs ending in `.svg` and draws no text. README
//! badges (`https://img.shields.io/badge/…`) are SVG with text behind an
//! extensionless URL, so they showed as their alt text. This loader also
//! takes any URI whose bytes are SVG, and gives resvg a few system fonts,
//! read the first time an SVG has text.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use egui::load::{BytesPoll, ImageLoadResult, ImageLoader, ImagePoll, LoadError, SizeHint};
use egui::{ColorImage, Context, Vec2};
use resvg::tiny_skia::Pixmap;
use resvg::usvg::{Options, Transform, Tree};

/// Faces for badge text (`Verdana,Geneva,DejaVu Sans,sans-serif`) and the
/// generic families. usvg falls back to any loaded face for other names.
#[cfg(windows)]
const FONT_FILES: &[&str] = &[
    "verdana.ttf",
    "verdanab.ttf",
    "verdanai.ttf",
    "arial.ttf",
    "arialbd.ttf",
    "ariali.ttf",
    "segoeui.ttf",
    "segoeuib.ttf",
    "times.ttf",
    "cour.ttf",
    "consola.ttf",
];
#[cfg(target_os = "macos")]
const FONT_FILES: &[&str] = &[
    "/System/Library/Fonts/Supplemental/Verdana.ttf",
    "/System/Library/Fonts/Supplemental/Verdana Bold.ttf",
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
    "/System/Library/Fonts/Supplemental/Times New Roman.ttf",
    "/System/Library/Fonts/Supplemental/Courier New.ttf",
];
#[cfg(not(any(windows, target_os = "macos")))]
const FONT_FILES: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
];

fn text_options() -> &'static Options<'static> {
    static OPTIONS: OnceLock<Options<'static>> = OnceLock::new();
    OPTIONS.get_or_init(|| {
        let mut options = Options::default();
        let db = options.fontdb_mut();
        for name in FONT_FILES {
            if let Ok(bytes) = std::fs::read(crate::style::font_path(name)) {
                db.load_font_data(bytes);
            }
        }
        if cfg!(not(any(windows, target_os = "macos"))) {
            db.set_sans_serif_family("DejaVu Sans");
            db.set_monospace_family("DejaVu Sans Mono");
        }
        options
    })
}

fn plain_options() -> &'static Options<'static> {
    static OPTIONS: OnceLock<Options<'static>> = OnceLock::new();
    OPTIONS.get_or_init(Options::default)
}

/// SVG by its media type or by its markup: an `<svg` element near the start
/// of something that begins like XML.
pub fn is_svg(bytes: &[u8], mime: Option<&str>) -> bool {
    if mime.is_some_and(|m| m.contains("image/svg")) {
        return true;
    }
    let head = &bytes[..bytes.len().min(4096)];
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    let start = head.iter().position(|b| !b.is_ascii_whitespace());
    start.is_some_and(|i| head[i] == b'<') && head.windows(4).any(|w| w == b"<svg")
}

/// Extensions the other loaders own. Skipping them keeps this loader from
/// fetching bytes it will never decode, and from failing frame URIs
/// (`x.gif#3`) that only the animated-image loaders understand.
fn is_other_image(uri: &str) -> bool {
    ["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico"]
        .iter()
        .any(|ext| egui::load::has_extension(uri, ext))
        || egui::decode_animated_image_uri(uri).is_ok()
}

fn rasterize(bytes: &[u8], hint: SizeHint) -> Result<ColorImage, String> {
    let has_text = bytes.windows(5).any(|w| w == b"<text");
    let options = if has_text {
        text_options()
    } else {
        plain_options()
    };
    let tree = Tree::from_data(bytes, options).map_err(|e| e.to_string())?;
    let source = Vec2::new(tree.size().width(), tree.size().height());
    let size = match hint {
        SizeHint::Size {
            width,
            height,
            maintain_aspect_ratio: true,
        } => {
            let size = source * (width as f32 / source.x);
            if size.y > height as f32 {
                size * (height as f32 / size.y)
            } else {
                size
            }
        }
        SizeHint::Size { width, height, .. } => Vec2::new(width as f32, height as f32),
        SizeHint::Width(w) => source * (w as f32 / source.x),
        SizeHint::Height(h) => source * (h as f32 / source.y),
        SizeHint::Scale(scale) => source * scale.into_inner(),
    }
    .round();
    let (w, h) = (size.x as u32, size.y as u32);
    let mut pixmap = Pixmap::new(w, h).ok_or_else(|| format!("Bad SVG size {w}x{h}"))?;
    resvg::render(
        &tree,
        Transform::from_scale(w as f32 / source.x, h as f32 / source.y),
        &mut pixmap.as_mut(),
    );
    Ok(
        ColorImage::from_rgba_premultiplied([w as usize, h as usize], pixmap.data())
            .with_source_size(source),
    )
}

struct Entry {
    last_used: u64,
    result: Result<Arc<ColorImage>, String>,
}

#[derive(Default)]
struct State {
    pass: u64,
    cache: HashMap<String, HashMap<SizeHint, Entry>>,
}

#[derive(Default)]
pub struct SvgLoader {
    state: Mutex<State>,
}

impl SvgLoader {
    pub const ID: &'static str = egui::generate_loader_id!(SvgLoader);

    pub fn install(ctx: &Context) {
        if !ctx.is_loader_installed(Self::ID) {
            ctx.add_image_loader(Arc::new(Self::default()));
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl ImageLoader for SvgLoader {
    fn id(&self) -> &str {
        Self::ID
    }

    fn load(&self, ctx: &Context, uri: &str, hint: SizeHint) -> ImageLoadResult {
        if is_other_image(uri) {
            return Err(LoadError::NotSupported);
        }
        let mut state = self.state();
        let pass = state.pass;
        if let Some(entry) = state.cache.get_mut(uri).and_then(|b| b.get_mut(&hint)) {
            entry.last_used = pass;
            return match &entry.result {
                Ok(image) => Ok(ImagePoll::Ready {
                    image: Arc::clone(image),
                }),
                Err(e) => Err(LoadError::Loading(e.clone())),
            };
        }
        let named_svg = egui::load::has_extension(uri, "svg");
        let (bytes, mime) = match ctx.try_load_bytes(uri) {
            Ok(BytesPoll::Ready { bytes, mime, .. }) => (bytes, mime),
            Ok(BytesPoll::Pending { size }) => return Ok(ImagePoll::Pending { size }),
            // Not ours to report unless the name says SVG.
            Err(e) if named_svg => return Err(e),
            Err(_) => return Err(LoadError::NotSupported),
        };
        if !named_svg && !is_svg(&bytes, mime.as_deref()) {
            return Err(LoadError::NotSupported);
        }
        let result = rasterize(&bytes, hint).map(Arc::new);
        let poll = match &result {
            Ok(image) => Ok(ImagePoll::Ready {
                image: Arc::clone(image),
            }),
            Err(e) => Err(LoadError::Loading(e.clone())),
        };
        state.cache.entry(uri.to_owned()).or_default().insert(
            hint,
            Entry {
                last_used: pass,
                result,
            },
        );
        poll
    }

    fn forget(&self, uri: &str) {
        self.state().cache.remove(uri);
    }

    fn forget_all(&self) {
        self.state().cache.clear();
    }

    fn byte_size(&self) -> usize {
        self.state()
            .cache
            .values()
            .flat_map(HashMap::values)
            .map(|entry| match &entry.result {
                Ok(image) => image.pixels.len() * 4,
                Err(e) => e.len(),
            })
            .sum()
    }

    /// Drops sizes not drawn last pass when one URI has several, as when a
    /// window resize re-rasterizes an SVG at each width.
    fn end_pass(&self, pass: u64) {
        let mut state = self.state();
        state.pass = pass;
        state.cache.retain(|_, sizes| {
            if sizes.len() > 1 {
                sizes.retain(|_, entry| pass <= entry.last_used + 1);
            }
            !sizes.is_empty()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_is_recognised_by_markup_or_media_type() {
        assert!(is_svg(b"<svg xmlns='http://www.w3.org/2000/svg'/>", None));
        assert!(is_svg(b"\xEF\xBB\xBF\n<?xml version='1.0'?>\n<svg/>", None));
        assert!(is_svg(b"anything", Some("image/svg+xml; charset=utf-8")));
        assert!(!is_svg(b"\x89PNG\r\n\x1a\n", Some("image/png")));
        assert!(!is_svg(b"<html><body></body></html>", None));
    }

    #[test]
    fn frame_and_raster_uris_are_left_to_other_loaders() {
        assert!(is_other_image("file:///C:/a/ball.gif#3"));
        assert!(is_other_image("https://x/y/pic.PNG"));
        assert!(!is_other_image(
            "https://img.shields.io/badge/Windows-10%2B-0078D6"
        ));
        assert!(!is_other_image("file:///C:/a/logo.svg"));
    }

    #[test]
    fn badge_text_is_drawn() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="20">
            <text x="2" y="15" font-family="Verdana,Geneva,DejaVu Sans,sans-serif"
                  font-size="11" fill="#000">release</text></svg>"##;
        let image = rasterize(svg, SizeHint::Scale(1.0.into())).unwrap();
        assert_eq!(image.size, [60, 20]);
        let inked = image.pixels.iter().filter(|p| p.a() > 0).count();
        assert!(inked > 20, "text should leave pixels, got {inked}");
    }
}
