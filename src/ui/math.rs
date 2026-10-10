//! TeX formulas, typeset by RaTeX (a port of KaTeX) off the UI thread.
//!
//! The worker lays a formula out, writes it as SVG with the glyph outlines
//! of the embedded KaTeX fonts, and rasterises it with resvg at the pixel
//! size it is shown at (see [`super::render_cache`]). The size of a formula
//! in ems does not depend on that pixel size.

use super::render_cache::{Metrics, PAD_PX, RenderCache, Rendered, State};
use egui::{Color32, Context, TextureOptions};

#[derive(Clone)]
pub struct MathRenderer {
    cache: RenderCache<Metrics>,
}

impl MathRenderer {
    pub fn new(repaint: impl Fn() + Send + Sync + 'static) -> MathRenderer {
        MathRenderer {
            cache: RenderCache::new("math", render, TextureOptions::LINEAR, repaint),
        }
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
    ) -> State<Metrics> {
        let variant = (u32::from_be_bytes(color.to_array()) as u64) << 1 | display as u64;
        self.cache.get(ctx, tex, variant, ppem)
    }
}

/// Lays `tex` out and rasterises it at `ppem` pixels per em; `variant` is
/// the display flag and the color, as [`MathRenderer::get`] packs them.
fn render(tex: &str, variant: u64, ppem: f32) -> Result<Rendered<Metrics>, String> {
    let display = variant & 1 == 1;
    let color = ((variant >> 1) as u32).to_be_bytes();
    render_tex(tex, display, color, ppem)
}

fn render_tex(
    tex: &str,
    display: bool,
    color: [u8; 4],
    ppem: f32,
) -> Result<Rendered<Metrics>, String> {
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
    let metrics = Metrics {
        width: list.width as f32,
        height: list.height as f32,
        depth: list.depth as f32,
    };
    let svg_options = ratex_svg::SvgOptions {
        font_size: ppem as f64,
        padding: PAD_PX as f64,
        embed_glyphs: true,
        ..Default::default()
    };
    let svg = ratex_svg::render_to_svg(&list, &svg_options);
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default())
        .map_err(|e| e.to_string())?;
    // The SVG gives its size in `pt`, which usvg turns into 4/3 as many
    // pixels; its view box is in the pixels wanted.
    let width = metrics.width * ppem + 2.0 * PAD_PX;
    let height = (metrics.height + metrics.depth) * ppem + 2.0 * PAD_PX;
    let scale = width / tree.size().width();
    let image = super::render_cache::rasterize(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        [width.ceil() as u32, height.ceil() as u32],
    )?;
    Ok(Rendered {
        image,
        meta: metrics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formulas_get_a_size_and_pixels() {
        let rendered = render_tex(r"\frac{a}{b} + x^2", false, [0, 0, 0, 255], 20.0).unwrap();
        let m = &rendered.meta;
        assert!(m.width > 1.0 && m.height > 0.5 && m.depth > 0.1);
        let [w, h] = rendered.image.size;
        assert_eq!(w, (m.width * 20.0 + 4.0).ceil() as usize);
        assert_eq!(h, ((m.height + m.depth) * 20.0 + 4.0).ceil() as usize);
        assert!(rendered.image.pixels.iter().any(|p| p.a() > 200));
    }

    #[test]
    fn the_variant_carries_display_and_color() {
        let color = Color32::from_rgb(1, 2, 3);
        let variant = (u32::from_be_bytes(color.to_array()) as u64) << 1 | 1;
        assert_eq!(variant & 1, 1);
        assert_eq!(((variant >> 1) as u32).to_be_bytes(), [1, 2, 3, 255]);
    }

    #[test]
    fn bad_tex_is_an_error() {
        assert!(render_tex(r"\frac{a}{", false, [0, 0, 0, 255], 20.0).is_err());
        assert!(render_tex(r"\nosuchcommand", true, [0, 0, 0, 255], 20.0).is_err());
        assert!(render_tex("  ", true, [0, 0, 0, 255], 20.0).is_err());
    }
}
