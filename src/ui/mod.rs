//! User interface pieces.

pub mod about;
pub mod animation;
pub mod find;
pub mod home;
pub mod svg;
pub mod toc;
pub mod viewer;

use crate::settings::ThemeSetting;
use egui::{Response, TextureHandle, TextureOptions, Ui};

/// The app icon as a texture for `points` wide, from the smallest embedded
/// size (64, 128, 256 px) that covers it at the display's pixel density;
/// kept in `cache` and replaced when that density changes.
pub fn app_icon(
    ctx: &egui::Context,
    points: f32,
    cache: &mut Option<TextureHandle>,
) -> TextureHandle {
    let px = (points * ctx.pixels_per_point()).round() as u32;
    let size = [64u32, 128, 256]
        .into_iter()
        .find(|&s| s >= px)
        .unwrap_or(256);
    let n = size as usize;
    if cache.as_ref().is_none_or(|t| t.size() != [n, n]) {
        let image = egui::ColorImage::from_rgba_unmultiplied([n, n], crate::embedded_icon(size));
        let options = TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear));
        *cache = Some(ctx.load_texture(format!("app_icon_{size}"), image, options));
    }
    cache.clone().expect("set above")
}

/// The theme switch: a half-filled circle while following Windows, a sun
/// for the light theme, a moon for the dark one; a click moves on to the
/// next of the three.
pub fn theme_button(ui: &mut Ui, theme: ThemeSetting) -> Response {
    use egui::{Shape, Stroke, pos2, vec2};
    let icon = ui.id().with("theme_icon");
    let resp = egui::Button::new(egui::Atom::custom(icon, vec2(14.0, 14.0))).atom_ui(ui);
    let tip = match theme {
        ThemeSetting::System => "Theme: as in Windows (click: light)",
        ThemeSetting::Light => "Theme: light (click: dark)",
        ThemeSetting::Dark => "Theme: dark (click: as in Windows)",
    };
    let response = resp.response.clone().on_hover_text(tip);
    let Some(rect) = resp.rect(icon) else {
        return response;
    };
    let visuals = ui.style().interact(&response);
    let (color, fill) = (visuals.text_color(), visuals.weak_bg_fill);
    let painter = ui.painter();
    let c = rect.center();
    match theme {
        ThemeSetting::System => {
            let r = 5.5;
            painter.circle_stroke(c, r, Stroke::new(1.3, color));
            // Left half filled: a convex half disc.
            let half: Vec<_> = (0..=16)
                .map(|i| {
                    let a = std::f32::consts::PI * (0.5 + i as f32 / 16.0);
                    c + vec2(r * a.cos(), -r * a.sin())
                })
                .collect();
            painter.add(Shape::convex_polygon(half, color, Stroke::NONE));
        }
        ThemeSetting::Light => {
            painter.circle_filled(c, 3.2, color);
            for i in 0..8 {
                let a = std::f32::consts::TAU * i as f32 / 8.0;
                let d = vec2(a.cos(), a.sin());
                painter.line_segment([c + d * 5.0, c + d * 6.8], Stroke::new(1.3, color));
            }
        }
        ThemeSetting::Dark => {
            // A crescent: a disc with another in the button's colour over
            // its upper right.
            painter.circle_filled(c, 6.0, color);
            painter.circle_filled(pos2(c.x + 3.0, c.y - 2.2), 5.0, fill);
        }
    }
    response
}

/// A cross that closes a dialog; true when clicked.
pub fn close_cross(ui: &mut Ui) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::click());
    let visuals = ui.style().interact(&response);
    if response.hovered() {
        ui.painter().rect_filled(rect, 3.0, visuals.bg_fill);
    }
    let (c, r) = (rect.center(), 5.0);
    let stroke = egui::Stroke::new(1.5, visuals.fg_stroke.color);
    ui.painter()
        .line_segment([c + egui::vec2(-r, -r), c + egui::vec2(r, r)], stroke);
    ui.painter()
        .line_segment([c + egui::vec2(-r, r), c + egui::vec2(r, -r)], stroke);
    response.on_hover_text("Close").clicked()
}
