//! Mermaid diagrams (```` ```mermaid ```` blocks), drawn by mermaid-svg off
//! the UI thread.
//!
//! mermaid-svg lays the diagram out (measuring text by character widths, so
//! without fonts) and writes SVG, which resvg rasterises with the system
//! fonts of [`super::svg`] at the scale it is shown at (see
//! [`super::render_cache`]): its own size in points, or less when the page
//! is narrower.

use super::render_cache::{Picture, RenderCache, Rendered, State};
use egui::{Context, TextureOptions, Vec2};
use std::sync::Arc;

/// A diagram ready to draw; `meta` is its own size in points.
pub type Diagram = Picture<Vec2>;

#[derive(Clone)]
pub struct DiagramRenderer {
    cache: RenderCache<Vec2>,
}

impl DiagramRenderer {
    pub fn new(repaint: impl Fn() + Send + Sync + 'static) -> DiagramRenderer {
        DiagramRenderer {
            cache: RenderCache::new("diagram", render, TextureOptions::LINEAR, repaint),
        }
    }

    /// The diagram for `source` in the dark or light theme, its texture made
    /// at `scale` pixels per point of its own size or on its way there.
    pub fn get(&self, ctx: &Context, source: &str, dark: bool, scale: f32) -> State<Vec2> {
        self.cache.get(ctx, source, dark as u64, scale)
    }

    /// The diagram as last drawn, without asking for one.
    pub fn peek(&self, source: &str, dark: bool) -> Option<Arc<Diagram>> {
        self.cache.peek(source, dark as u64)
    }
}

fn render(source: &str, dark: u64, scale: f32) -> Result<Rendered<Vec2>, String> {
    let mut theme = if dark == 1 {
        mermaid_svg::Theme::dark()
    } else {
        mermaid_svg::Theme::default()
    };
    // A fixed size rather than `width="100%"`; the viewer fits it to the page.
    theme.responsive = false;
    let svg = mermaid_svg::render_with(source, &theme).map_err(|e| e.to_string())?;
    let tree =
        resvg::usvg::Tree::from_str(&svg, super::svg::text_options()).map_err(|e| e.to_string())?;
    let size = Vec2::new(tree.size().width(), tree.size().height());
    let pixels = [
        (size.x * scale).ceil() as u32,
        (size.y * scale).ceil() as u32,
    ];
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
    let image = super::render_cache::rasterize(&tree, transform, pixels)?;
    Ok(Rendered { image, meta: size })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flowchart_is_drawn_at_its_size() {
        let source = "flowchart LR\n    A[Начало] --> B{Ветвь}\n    B --> C[Конец]\n";
        let rendered = render(source, 0, 2.0).unwrap();
        let size = rendered.meta;
        assert!(size.x > 100.0 && size.y > 20.0, "{size:?}");
        let [w, h] = rendered.image.size;
        assert_eq!(
            [w, h],
            [
                (size.x * 2.0).ceil() as usize,
                (size.y * 2.0).ceil() as usize
            ]
        );
        assert!(rendered.image.pixels.iter().filter(|p| p.a() > 200).count() > 100);
    }

    #[test]
    fn every_sample_diagram_renders_in_both_themes() {
        // A checkout on Windows can have CRLF line ends.
        let sample = include_str!("../../samples/markdown-syntax.md").replace("\r\n", "\n");
        let mut count = 0;
        for block in sample.split("```mermaid\n").skip(1) {
            let source = block.split("```").next().unwrap();
            for dark in [0, 1] {
                render(source, dark, 1.0).unwrap_or_else(|e| panic!("{e}: {source}"));
            }
            count += 1;
        }
        assert!(count >= 4, "{count}");
    }

    #[test]
    fn nonsense_is_an_error() {
        assert!(render("not a diagram at all", 0, 1.0).is_err());
    }
}
