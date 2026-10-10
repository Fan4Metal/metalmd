//! User interface pieces.

pub mod about;
pub mod animation;
pub mod diagram;
pub mod emoji;
pub mod find;
pub mod home;
pub mod math;
pub mod render_cache;
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

/// A button with a 14-point icon that `paint` draws around the given centre
/// in the button's text colour; the last argument is the button's fill, for
/// cut-outs. `selected` gives it the look of a toggle that is on.
fn icon_button(
    ui: &mut Ui,
    salt: &str,
    selected: bool,
    tip: &str,
    paint: impl FnOnce(&egui::Painter, egui::Pos2, egui::Color32, egui::Color32),
) -> Response {
    let icon = ui.id().with(salt);
    let resp = egui::Button::new(egui::Atom::custom(icon, egui::vec2(14.0, 14.0)))
        .selected(selected)
        .atom_ui(ui);
    let response = resp.response.clone().on_hover_text(tip);
    if let Some(rect) = resp.rect(icon) {
        let visuals = ui.style().interact_selectable(&response, selected);
        paint(
            ui.painter(),
            rect.center(),
            visuals.text_color(),
            visuals.weak_bg_fill,
        );
    }
    response
}

/// Shows or hides the table of contents: an outline of a heading and two
/// indented subheadings, highlighted while the panel is shown.
pub fn toc_button(ui: &mut Ui, visible: bool) -> Response {
    use egui::{Stroke, vec2};
    let tip = if visible {
        tr!(
            "Hide the table of contents (Ctrl+Shift+O)",
            "Скрыть содержание (Ctrl+Shift+O)"
        )
    } else {
        tr!(
            "Show the table of contents (Ctrl+Shift+O)",
            "Показать содержание (Ctrl+Shift+O)"
        )
    };
    icon_button(ui, "toc_icon", visible, tip, |painter, c, color, _| {
        for (y, indent) in [(-4.0, 0.0), (0.0, 3.0), (4.0, 3.0)] {
            painter.circle_filled(c + vec2(-5.0 + indent, y), 1.1, color);
            painter.line_segment(
                [c + vec2(-2.5 + indent, y), c + vec2(6.0, y)],
                Stroke::new(1.3, color),
            );
        }
    })
}

/// Opens the About window: an "i" in a circle.
pub fn about_button(ui: &mut Ui) -> Response {
    use egui::{Stroke, vec2};
    icon_button(
        ui,
        "about_icon",
        false,
        tr!("About MetalMD (F1)", "О программе MetalMD (F1)"),
        |painter, c, color, _| {
            painter.circle_stroke(c, 6.0, Stroke::new(1.3, color));
            painter.circle_filled(c + vec2(0.0, -2.8), 0.95, color);
            painter.line_segment(
                [c + vec2(0.0, -0.6), c + vec2(0.0, 3.4)],
                Stroke::new(1.4, color),
            );
        },
    )
}

/// The theme switch: a half-filled circle while following Windows, a sun
/// for the light theme, a moon for the dark one; a click moves on to the
/// next of the three.
pub fn theme_button(ui: &mut Ui, theme: ThemeSetting) -> Response {
    let tip = match theme {
        ThemeSetting::System => tr!(
            "Theme: as in Windows (click: light)",
            "Тема: как в Windows (щелчок: светлая)"
        ),
        ThemeSetting::Light => tr!(
            "Theme: light (click: dark)",
            "Тема: светлая (щелчок: тёмная)"
        ),
        ThemeSetting::Dark => tr!(
            "Theme: dark (click: as in Windows)",
            "Тема: тёмная (щелчок: как в Windows)"
        ),
    };
    icon_button(ui, "theme_icon", false, tip, |painter, c, color, fill| {
        paint_theme_icon(painter, c, color, fill, theme);
    })
}

fn paint_theme_icon(
    painter: &egui::Painter,
    c: egui::Pos2,
    color: egui::Color32,
    fill: egui::Color32,
    theme: ThemeSetting,
) {
    use egui::{Shape, Stroke, pos2, vec2};
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
    response.on_hover_text(tr!("Close", "Закрыть")).clicked()
}
