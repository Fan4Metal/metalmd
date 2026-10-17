//! A picture of the document shown on its own, over the window: an image
//! or a diagram clicked in the text.
//!
//! It opens fitted to the window (never enlarged), the wheel zooms about
//! the pointer, a drag moves it, a double click switches between fitted and
//! its own size, and Esc or a click beside it closes it. A diagram is drawn
//! again at the scale it is shown at, under a variant of its own in the
//! render cache, so the document behind keeps its own texture.

use super::viewer::Pictures;
use crate::style::Palette;
use egui::{
    Color32, CursorIcon, Id, Order, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2, pos2, vec2,
};

/// What is shown.
#[derive(Clone, Debug, PartialEq)]
pub enum Shown {
    /// An image, by the URI the viewer loads it from.
    Image(String),
    /// A Mermaid diagram, by its source.
    Diagram(String),
}

/// One wheel notch zooms by this factor.
const NOTCH: f32 = 1.2;
/// Room left around a fitted picture, in points.
const MARGIN: f32 = 32.0;

pub struct Lightbox {
    shown: Shown,
    /// Times its own size; `None` while fitted to the window.
    zoom: Option<f32>,
    /// From the centre of the window to the centre of the picture.
    offset: Vec2,
}

impl Lightbox {
    pub fn new(shown: Shown) -> Lightbox {
        Lightbox {
            shown,
            zoom: None,
            offset: Vec2::ZERO,
        }
    }

    /// Draws it over the window and follows the pointer and keys; `false`
    /// once it is closed.
    pub fn show(&mut self, ctx: &egui::Context, pictures: &Pictures, palette: &Palette) -> bool {
        let screen = ctx.content_rect();
        let area = egui::Area::new(Id::new("lightbox"))
            .order(Order::Foreground)
            .fixed_pos(screen.min)
            .interactable(true);
        let mut open = true;
        area.show(ctx, |ui| {
            ui.set_min_size(screen.size());
            let backdrop = ui.interact(screen, Id::new("lightbox-backdrop"), Sense::click());
            ui.painter()
                .rect_filled(screen, 0.0, Color32::from_black_alpha(210));

            let Some(natural) = self.natural_size(ui, pictures, palette.dark) else {
                ui.painter().text(
                    screen.center(),
                    egui::Align2::CENTER_CENTER,
                    "…",
                    egui::FontId::proportional(28.0),
                    Color32::WHITE,
                );
                open = !backdrop.clicked();
                return;
            };
            let room = (screen.size() - Vec2::splat(2.0 * MARGIN)).max(Vec2::splat(16.0));
            let fit = (room.x / natural.x).min(room.y / natural.y).min(1.0);
            let mut zoom = self.zoom.unwrap_or(fit);
            let mut rect = Rect::from_center_size(screen.center() + self.offset, natural * zoom);
            let picture = ui.interact(rect, Id::new("lightbox-picture"), Sense::click_and_drag());

            // The wheel zooms about the pointer.
            let notches = wheel_notches(ui);
            if notches != 0.0
                && let Some(pointer) = ui.input(|i| i.pointer.hover_pos())
            {
                let wanted = (zoom * NOTCH.powf(notches)).clamp(fit.min(1.0) * 0.25, 16.0);
                self.zoom_about(pointer, rect, zoom, wanted, screen);
                zoom = wanted;
            }
            if picture.double_clicked() {
                let pointer = picture.interact_pointer_pos().unwrap_or(rect.center());
                if self.zoom.is_some() {
                    self.zoom = None;
                    self.offset = Vec2::ZERO;
                    zoom = fit;
                } else {
                    let wanted = if (zoom - 1.0).abs() < 0.01 { 2.0 } else { 1.0 };
                    self.zoom_about(pointer, rect, zoom, wanted, screen);
                    zoom = wanted;
                }
            }
            if picture.dragged() {
                self.offset += picture.drag_delta();
                self.zoom = Some(zoom);
            }
            if ui.input(|i| i.key_pressed(egui::Key::Num0)) {
                self.zoom = None;
                self.offset = Vec2::ZERO;
                zoom = fit;
            }
            rect = Rect::from_center_size(screen.center() + self.offset, natural * zoom);

            // On the page's colour: diagrams and many images are drawn on
            // a transparent ground.
            ui.painter().rect_filled(rect, 2.0, palette.page);
            self.paint(ui, pictures, palette.dark, rect, natural, zoom);
            ui.painter().rect_stroke(
                rect,
                2.0,
                Stroke::new(1.0, Color32::from_white_alpha(40)),
                StrokeKind::Outside,
            );
            if picture.hovered() {
                ui.ctx().set_cursor_icon(if picture.dragged() {
                    CursorIcon::Grabbing
                } else {
                    CursorIcon::Grab
                });
            }

            let hint = tr!(
                format!(
                    "{:.0} %  ·  wheel: zoom  ·  double click: 100 % or fit  ·  0: fit  ·  Esc: close",
                    zoom * 100.0
                ),
                format!(
                    "{:.0} %  ·  колесо: масштаб  ·  двойной щелчок: 100 % или по окну  ·  0: по окну  ·  Esc: закрыть",
                    zoom * 100.0
                )
            );
            ui.painter().text(
                pos2(screen.center().x, screen.bottom() - 14.0),
                egui::Align2::CENTER_CENTER,
                hint,
                egui::FontId::proportional(12.5),
                Color32::from_white_alpha(200),
            );
            let close = Rect::from_min_size(pos2(screen.right() - 40.0, screen.top() + 8.0), vec2(32.0, 32.0));
            let close_button = ui.interact(close, Id::new("lightbox-close"), Sense::click());
            let c = close.center();
            let color = if close_button.hovered() {
                Color32::WHITE
            } else {
                Color32::from_white_alpha(180)
            };
            for d in [vec2(7.0, 7.0), vec2(7.0, -7.0)] {
                ui.painter()
                    .line_segment([c - d, c + d], Stroke::new(2.0, color));
            }
            let close_button = close_button.on_hover_text(tr!("Close (Esc)", "Закрыть (Esc)"));
            if close_button.clicked() || (backdrop.clicked() && !picture.hovered()) {
                open = false;
            }
        });
        open
    }

    /// Zooms from `zoom` to `wanted`, keeping the point under `pointer`
    /// where it is.
    fn zoom_about(&mut self, pointer: Pos2, rect: Rect, zoom: f32, wanted: f32, screen: Rect) {
        let from_centre = pointer - rect.center();
        let centre = pointer - from_centre * (wanted / zoom);
        self.offset = centre - screen.center();
        self.zoom = Some(wanted);
    }

    /// The picture's own size in points, once it is loaded.
    fn natural_size(&self, ui: &egui::Ui, pictures: &Pictures, dark: bool) -> Option<Vec2> {
        match &self.shown {
            Shown::Image(uri) => {
                let uri = super::animation::frame_uri(ui.ctx(), uri).unwrap_or_else(|| uri.clone());
                egui::Image::new(uri)
                    .fit_to_original_size(1.0)
                    .load_and_calc_size(ui, Vec2::splat(f32::INFINITY))
                    .filter(|size| size.x > 0.0 && size.y > 0.0)
            }
            Shown::Diagram(source) => pictures
                .diagrams
                .peek(source, dark)
                .map(|diagram| diagram.meta),
        }
    }

    fn paint(
        &self,
        ui: &egui::Ui,
        pictures: &Pictures,
        dark: bool,
        rect: Rect,
        natural: Vec2,
        zoom: f32,
    ) {
        match &self.shown {
            Shown::Image(uri) => {
                let uri = super::animation::frame_uri(ui.ctx(), uri).unwrap_or_else(|| uri.clone());
                egui::Image::new(uri)
                    .fit_to_exact_size(rect.size())
                    .paint_at(ui, rect);
            }
            Shown::Diagram(source) => {
                let ppp = ui.ctx().pixels_per_point();
                // Within the texture size every GPU takes.
                let largest = 8192.0 / natural.x.max(natural.y);
                let scale = (zoom * ppp).min(largest);
                let ctx = ui.ctx();
                let diagram = match pictures.diagrams.get_zoomed(ctx, source, dark, scale) {
                    super::render_cache::State::Ready(diagram) => Some(diagram),
                    _ => pictures
                        .diagrams
                        .peek_zoomed(source, dark)
                        .or_else(|| pictures.diagrams.peek(source, dark)),
                };
                if let Some(diagram) = diagram {
                    let [w, h] = diagram.texture.size();
                    let shown = Rect::from_min_size(
                        rect.min,
                        vec2(w as f32, h as f32) * (zoom / diagram.scale),
                    );
                    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                    ui.painter()
                        .with_clip_rect(rect.intersect(ui.clip_rect()))
                        .image(diagram.texture.id(), shown, uv, Color32::WHITE);
                }
            }
        }
    }
}

/// Wheel notches turned this frame, up positive; a touchpad's points count
/// as fractions of one.
fn wheel_notches(ui: &egui::Ui) -> f32 {
    ui.input(|i| {
        i.events
            .iter()
            .filter_map(|event| match event {
                egui::Event::MouseWheel { unit, delta, .. } => Some(match unit {
                    egui::MouseWheelUnit::Point => delta.y / 50.0,
                    egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page => delta.y,
                }),
                _ => None,
            })
            .sum()
    })
}
