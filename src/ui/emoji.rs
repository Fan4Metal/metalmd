//! Colour emoji. egui draws every glyph in one colour, so an emoji (a span
//! the parser marked `Style::EMOJI`) is drawn as a picture instead: the
//! worker sets it in the system's colour emoji font with resvg, which reads
//! COLR (Segoe UI Emoji), CBDT (Noto Color Emoji) and sbix (Apple Color
//! Emoji), at the pixel size it is shown at. The font is read with the
//! first emoji. Until then, or if the font lacks the emoji, it is text.

use super::render_cache::{Metrics, PAD_PX, RenderCache, Rendered, State, rasterize};
use egui::{Color32, ColorImage, Context, TextureOptions};
use resvg::tiny_skia::Transform;
use resvg::usvg::{Node, Options, Tree};
use std::sync::OnceLock;

#[cfg(windows)]
const FONT_FILES: &[&str] = &["seguiemj.ttf"];
#[cfg(target_os = "macos")]
const FONT_FILES: &[&str] = &["/System/Library/Fonts/Apple Color Emoji.ttc"];
#[cfg(not(any(windows, target_os = "macos")))]
const FONT_FILES: &[&str] = &[
    "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/google-noto-emoji/NotoColorEmoji.ttf",
];

/// resvg options holding the emoji font alone, so that no other font
/// stands in for an emoji it lacks, and the font's family name; `None`
/// without one.
fn font() -> Option<&'static (Options<'static>, String)> {
    static FONT: OnceLock<Option<(Options<'static>, String)>> = OnceLock::new();
    FONT.get_or_init(|| {
        let bytes = FONT_FILES
            .iter()
            .find_map(|name| std::fs::read(crate::style::font_path(name)).ok())?;
        let mut options = Options::default();
        let db = options.fontdb_mut();
        db.load_font_data(bytes);
        let family = db.faces().next()?.families.first()?.0.clone();
        Some((options, family))
    })
    .as_ref()
}

#[derive(Clone)]
pub struct EmojiRenderer {
    cache: RenderCache<Metrics>,
}

impl EmojiRenderer {
    pub fn new(repaint: impl Fn() + Send + Sync + 'static) -> EmojiRenderer {
        EmojiRenderer {
            cache: RenderCache::new("emoji", render, TextureOptions::LINEAR, repaint),
        }
    }

    /// The picture of `emoji` for `ppem` pixels per em, or on its way
    /// there; `color` is for the few glyphs drawn in the colour of text.
    pub fn get(&self, ctx: &Context, emoji: &str, color: Color32, ppem: f32) -> State<Metrics> {
        let variant = u32::from_be_bytes(color.to_array()) as u64;
        self.cache.get(ctx, emoji, variant, ppem)
    }
}

/// Sets `emoji` at `ppem` pixels per em in the colour `variant` (as
/// [`EmojiRenderer::get`] packs it). Its box is as wide as its advance (or
/// its ink, if wider) and as tall as its ink.
fn render(emoji: &str, variant: u64, ppem: f32) -> Result<Rendered<Metrics>, String> {
    let (options, family) = font().ok_or("no colour emoji font")?;
    let [r, g, b, a] = (variant as u32).to_be_bytes();
    // A space after the emoji marks where its advance ends: the shaper
    // gives the hidden parts of a sequence (U+FE0F, joiners) no advance,
    // which the font's own advances of their glyphs do not show.
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><text xml:space="preserve" font-family="{}" font-size="{ppem}" fill="rgb({r},{g},{b})" fill-opacity="{}">{} </text></svg>"#,
        escape(family),
        a as f32 / 255.0,
        escape(emoji),
    );
    let tree = Tree::from_str(&svg, options).map_err(|e| e.to_string())?;
    let advance = advance(&tree).ok_or("not in the emoji font")?;
    // usvg's box of text is the line's, and that of a colour glyph is in
    // font units, so the ink is found in pixels: the emoji is drawn with
    // room around it, then again in its box. The text starts at x = 0 with
    // its baseline at y = 0.
    let room = (ppem * 1.5).ceil();
    let canvas = rasterize(
        &tree,
        Transform::from_translate(room, room),
        [(advance + 2.0 * room).ceil() as u32, (2.0 * room) as u32],
    )?;
    let [_, top, right, bottom] = ink(&canvas).ok_or("emoji without ink")?;
    let above = (room - top as f32).max(0.0);
    let below = (bottom as f32 - room).max(0.0);
    // Ink past the advance takes room too: without flags Segoe UI Emoji
    // draws a pair of regional indicators as letters wider than that.
    let right = advance.max(right as f32 - room);
    let image = rasterize(
        &tree,
        Transform::from_translate(PAD_PX, PAD_PX + above),
        [
            (right + 2.0 * PAD_PX).ceil() as u32,
            (above + below + 2.0 * PAD_PX).ceil() as u32,
        ],
    )?;
    let meta = Metrics {
        width: right / ppem,
        height: above / ppem,
        depth: below / ppem,
    };
    Ok(Rendered { image, meta })
}

/// The box of the pixels of `image` that are not transparent: left, top,
/// right, bottom, the last two past the end.
fn ink(image: &ColorImage) -> Option<[usize; 4]> {
    let width = image.size[0];
    let mut rect: Option<[usize; 4]> = None;
    for (i, _) in image.pixels.iter().enumerate().filter(|(_, p)| p.a() > 0) {
        let (x, y) = (i % width, i / width);
        let r = rect.get_or_insert([x, y, x + 1, y + 1]);
        r[0] = r[0].min(x);
        r[1] = r[1].min(y);
        r[2] = r[2].max(x + 1);
        r[3] = r[3].max(y + 1);
    }
    rect
}

/// Where the trailing space is set: the emoji's advance, in pixels; `None`
/// if the font has no glyph for one of the characters.
fn advance(tree: &Tree) -> Option<f32> {
    let mut glyphs = Vec::new();
    for node in tree.root().children() {
        if let Node::Text(text) = node {
            glyphs.extend(text.layouted().iter().flat_map(|s| &s.positioned_glyphs));
        }
    }
    if glyphs.len() < 2 || glyphs.iter().any(|g| g.id.0 == 0) {
        return None;
    }
    Some(glyphs.last()?.transform().tx)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLACK: u64 = 0x0000_00ff;

    #[test]
    fn emoji_are_drawn_in_colour() {
        if font().is_none() {
            return;
        }
        let rendered = render("✅", BLACK, 32.0).unwrap();
        let m = &rendered.meta;
        assert!(m.width > 0.8 && m.width < 1.6, "advance {}", m.width);
        assert!(m.height > 0.5 && m.height < 1.2, "height {}", m.height);
        let [w, h] = rendered.image.size;
        assert!(w >= (m.width * 32.0 + 4.0) as usize);
        assert_eq!(h, ((m.height + m.depth) * 32.0 + 4.0).ceil() as usize);
        let coloured = rendered
            .image
            .pixels
            .iter()
            .filter(|p| p.a() > 200 && (p.g() as i32 - p.r() as i32).abs() > 60)
            .count();
        assert!(coloured > 50, "only {coloured} coloured pixels");
    }

    #[test]
    fn nothing_is_cut_off() {
        if font().is_none() {
            return;
        }
        for emoji in ["✅", "🚀", "👍🏽", "🇺🇦", "1️⃣"] {
            let rendered = render(emoji, BLACK, 23.0).unwrap();
            let [w, h] = rendered.image.size;
            let [left, top, right, bottom] = ink(&rendered.image).unwrap();
            assert!(left >= 1 && top >= 1, "{emoji}: {left}, {top}");
            assert!(
                right < w && bottom < h,
                "{emoji}: {right}/{w}, {bottom}/{h}"
            );
            let m = &rendered.meta;
            assert!(right as f32 <= m.width * 23.0 + PAD_PX + 1.0, "{emoji}");
        }
    }

    #[test]
    fn a_joined_sequence_is_one_glyph() {
        if font().is_none() {
            return;
        }
        let one = render("👩", BLACK, 32.0).unwrap().meta.width;
        let joined = render("👩‍💻", BLACK, 32.0).unwrap().meta.width;
        assert!((one - joined).abs() < 0.01, "{one} vs {joined}");
        let heart = render("❤", BLACK, 32.0).unwrap().meta.width;
        let selected = render("❤\u{FE0F}", BLACK, 32.0).unwrap().meta.width;
        assert!((heart - selected).abs() < 0.01, "{heart} vs {selected}");
    }

    #[test]
    fn what_the_font_lacks_is_an_error() {
        if font().is_none() {
            return;
        }
        assert!(render("\u{F0000}", BLACK, 32.0).is_err());
    }
}
