//! User interface pieces.

pub mod about;
pub mod animation;
pub mod confirm;
pub mod copy;
pub mod diagram;
pub mod emoji;
pub mod find;
pub mod home;
pub mod licenses;
pub mod lightbox;
pub mod limit;
pub mod math;
pub mod menu;
pub mod render_cache;
pub mod selection;
pub mod svg;
pub mod text;
pub mod toc;
pub mod viewer;

use crate::settings::{DarkStyle, ThemeSetting, TocSide};
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
            "Hide the table of contents (Ctrl+Shift+O)
Right-click: left or right side",
            "Скрыть содержание (Ctrl+Shift+O)
Правый щелчок: слева или справа"
        )
    } else {
        tr!(
            "Show the table of contents (Ctrl+Shift+O)
Right-click: left or right side",
            "Показать содержание (Ctrl+Shift+O)
Правый щелчок: слева или справа"
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

/// Opens the Text panel (fonts and text size): a large and a small "A",
/// drawn in lines like the other icons, with a rounded apex and round ends.
pub fn text_button(ui: &mut Ui, open: bool) -> Response {
    use egui::{Stroke, pos2};
    let tip = tr!("Fonts and text size", "Шрифты и размер текста");
    icon_button(ui, "text_icon", open, tip, |painter, c, color, _| {
        let stroke = Stroke::new(1.3, color);
        let at = |x: f32, y: f32| pos2(c.x + x, c.y + y);
        // (apex, left foot, right foot, crossbar height)
        for (apex, left, right, bar) in
            [((-3.0, -6.0), -7.0, 1.0, 2.0), ((4.5, -0.5), 2.0, 7.0, 3.8)]
        {
            let foot = 6.0;
            let legs = [at(left, foot), at(apex.0, apex.1), at(right, foot)];
            round_line(painter, rounded(&legs, &[0.0, 1.5, 0.0]), stroke);
            // Where the bar meets the legs.
            let t = (foot - bar) / (foot - apex.1);
            round_line(
                painter,
                vec![
                    at(left + (apex.0 - left) * t, bar),
                    at(right + (apex.0 - right) * t, bar),
                ],
                stroke,
            );
        }
    })
}

/// The context menu of the table of contents and its button: the side of
/// the window it is on. Returns `true` when the side changed.
pub fn toc_side_menu(response: &Response, side: &mut TocSide) -> bool {
    let before = *side;
    menu::context_menu(response, |ui| {
        for (value, label) in [
            (
                TocSide::Left,
                tr!("Contents on the left", "Содержание слева"),
            ),
            (
                TocSide::Right,
                tr!("Contents on the right", "Содержание справа"),
            ),
        ] {
            if ui.radio_value(side, value, label).clicked() {
                ui.close();
            }
        }
    });
    *side != before
}

/// The context menu of the theme button: which of the two dark themes the
/// dark one is. Returns `true` when it changed.
pub fn dark_style_menu(response: &Response, style: &mut DarkStyle) -> bool {
    let before = *style;
    menu::context_menu(response, |ui| {
        for (value, label) in [
            (
                DarkStyle::Grey,
                tr!("Dark theme: grey", "Тёмная тема: серая"),
            ),
            (
                DarkStyle::Black,
                tr!("Dark theme: black", "Тёмная тема: чёрная"),
            ),
        ] {
            if ui.radio_value(style, value, label).clicked() {
                ui.close();
            }
        }
    });
    *style != before
}

/// Opens files: the outline of an open folder, its tab on the top left and
/// its front flap leaning forward, with rounded corners.
pub fn open_button(ui: &mut Ui) -> Response {
    use egui::{Stroke, pos2};
    icon_button(
        ui,
        "open_icon",
        false,
        tr!("Open file (Ctrl+O)", "Открыть файл (Ctrl+O)"),
        |painter, c, color, _| {
            let at = |points: &[(f32, f32)]| -> Vec<egui::Pos2> {
                points
                    .iter()
                    .map(|&(x, y)| pos2(c.x + x, c.y + y))
                    .collect()
            };
            let stroke = Stroke::new(1.3, color);
            // The back with its tab, from where its rounded bottom left
            // corner meets the front flap to where the flap's top is.
            let back = at(&[
                (-4.5, 5.0),
                (-6.5, 5.0),
                (-6.5, -5.0),
                (-2.0, -5.0),
                (-0.5, -3.0),
                (5.0, -3.0),
                (5.0, -0.5),
            ]);
            round_line(
                painter,
                rounded(&back, &[0.0, 2.0, 2.0, 1.2, 1.2, 2.0, 0.0]),
                stroke,
            );
            // The front flap, opened forward: a parallelogram leaning right,
            // sharp only where it leaves the back's corner.
            let flap = at(&[
                (-4.5, 5.0),
                (-2.0, -0.5),
                (7.0, -0.5),
                (4.5, 5.0),
                (-4.5, 5.0),
            ]);
            round_line(painter, rounded(&flap, &[0.0, 1.5, 1.5, 1.5, 0.0]), stroke);
        },
    )
}

/// What the print button's menu asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintChoice {
    Print,
    Pdf,
}

/// Prints the document or saves it as PDF, from the menu a click opens: a
/// printer, its paper going in at the top and coming out at the front.
pub fn print_button(ui: &mut Ui, enabled: bool) -> Option<PrintChoice> {
    use egui::{Rect, Stroke, StrokeKind, pos2};
    let response = ui
        .add_enabled_ui(enabled, |ui| {
            icon_button(
                ui,
                "print_icon",
                false,
                tr!("Print or save as PDF", "Печать или сохранение в PDF"),
                |painter, c, color, fill| {
                    let stroke = Stroke::new(1.3, color);
                    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| {
                        Rect::from_min_max(pos2(c.x + x0, c.y + y0), pos2(c.x + x1, c.y + y1))
                    };
                    // The paper going in, behind the body.
                    painter.rect_stroke(
                        rect(-4.0, -6.5, 4.0, -1.0),
                        1.0,
                        stroke,
                        StrokeKind::Middle,
                    );
                    let body = rect(-6.5, -2.5, 6.5, 3.5);
                    painter.rect_filled(body, 2.0, fill);
                    painter.rect_stroke(body, 2.0, stroke, StrokeKind::Middle);
                    // The printed sheet over the body's front.
                    let sheet = rect(-4.0, 1.0, 4.0, 6.5);
                    painter.rect_filled(sheet, 1.0, fill);
                    painter.rect_stroke(sheet, 1.0, stroke, StrokeKind::Middle);
                    painter.circle_filled(pos2(c.x + 4.3, c.y - 0.5), 0.8, color);
                },
            )
        })
        .inner;
    let mut choice = None;
    egui::Popup::menu(&response).show(|ui| {
        let print = egui::Button::new(tr!("Print…", "Печать…")).shortcut_text("Ctrl+P");
        if ui.add(print).clicked() {
            choice = Some(PrintChoice::Print);
        }
        let pdf = egui::Button::new(tr!("Save as PDF…", "Сохранить как PDF…"))
            .shortcut_text("Ctrl+Shift+S");
        if ui.add(pdf).clicked() {
            choice = Some(PrintChoice::Pdf);
        }
    });
    choice
}

/// The line through `points` with each corner cut `radius[i]` along both
/// its sides (less on a short side) and bridged by a curve, so that a line
/// icon looks drawn with a round pen; a radius of 0 keeps the point as it
/// is, as for the ends. egui joins lines with sharp mitres, which look
/// rough at 14 points.
fn rounded(points: &[egui::Pos2], radius: &[f32]) -> Vec<egui::Pos2> {
    const STEPS: usize = 6;
    let n = points.len();
    let mut out = Vec::with_capacity(n * (STEPS + 1));
    for (i, (&p, &radius)) in points.iter().zip(radius).enumerate() {
        if radius <= 0.0 || i == 0 || i == n - 1 {
            out.push(p);
            continue;
        }
        let (prev, next) = (points[i - 1], points[i + 1]);
        let r = radius
            .min((prev - p).length() / 2.0)
            .min((next - p).length() / 2.0);
        let start = p + (prev - p).normalized() * r;
        let end = p + (next - p).normalized() * r;
        // A quadratic Bézier with the corner as its control point.
        for k in 0..=STEPS {
            let t = k as f32 / STEPS as f32;
            let u = 1.0 - t;
            out.push(quadratic(start, p, end, u, t));
        }
    }
    out
}

/// The point at `t` (with `u = 1 - t`) of the quadratic Bézier `a`, `b`, `c`.
fn quadratic(a: egui::Pos2, b: egui::Pos2, c: egui::Pos2, u: f32, t: f32) -> egui::Pos2 {
    (a.to_vec2() * (u * u) + b.to_vec2() * (2.0 * u * t) + c.to_vec2() * (t * t)).to_pos2()
}

/// An open line with round ends: egui ends a stroke flat.
fn round_line(painter: &egui::Painter, points: Vec<egui::Pos2>, stroke: egui::Stroke) {
    for end in [points.first(), points.last()].into_iter().flatten() {
        painter.circle_filled(*end, stroke.width / 2.0, stroke.color);
    }
    painter.add(egui::epaint::PathShape::line(points, stroke));
}

/// A newer release: its version with a download arrow (an arrow onto a
/// tray) in the link colour; a click opens its release page. Shown left of
/// About only while `Updates::newer` is `Some`, as in qview.
pub fn update_button(ui: &mut Ui, tag: &str) -> Response {
    use egui::{Stroke, vec2};
    let version = crate::update::version_of(tag);
    let id = ui.id().with("update_icon");
    let resp =
        egui::Button::new((egui::Atom::custom(id, egui::vec2(14.0, 14.0)), version)).atom_ui(ui);
    let response = resp.response.clone().on_hover_text(tr!(
        format!("Version {version} is available: open its release page"),
        format!("Доступна версия {version}: открыть страницу выпуска")
    ));
    if let Some(rect) = resp.rect(id) {
        let (c, painter) = (rect.center(), ui.painter());
        let stroke = Stroke::new(1.5, ui.visuals().hyperlink_color);
        let at = |x: f32, y: f32| c + vec2(x, y);
        round_line(painter, vec![at(0.0, -5.5), at(0.0, 2.5)], stroke);
        let head = [at(-4.0, -1.5), at(0.0, 2.5), at(4.0, -1.5)];
        round_line(painter, rounded(&head, &[0.0, 1.0, 0.0]), stroke);
        round_line(painter, vec![at(-5.5, 5.5), at(5.5, 5.5)], stroke);
    }
    response
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

/// The Copy button of a code block, filling `rect`: two overlapping sheets,
/// or a tick for a moment after a copy.
pub fn copy_button(ui: &mut Ui, rect: egui::Rect, copied: bool) -> Response {
    use egui::{Rect, Stroke, StrokeKind, pos2, vec2};
    let response = ui.put(
        rect,
        egui::Button::new("").min_size(rect.size()).corner_radius(4),
    );
    let visuals = ui.style().interact(&response);
    let (c, painter) = (response.rect.center(), ui.painter());
    let stroke = Stroke::new(1.3, visuals.text_color());
    if copied {
        let tick = [(-5.0, 0.5), (-1.5, 4.0), (5.0, -3.5)].map(|(x, y)| pos2(c.x + x, c.y + y));
        painter.add(egui::epaint::PathShape::line(tick.to_vec(), stroke));
    } else {
        let sheet = |x: f32, y: f32| Rect::from_min_size(c + vec2(x, y), vec2(8.0, 10.0));
        painter.rect_stroke(sheet(-6.0, -7.0), 1.5, stroke, StrokeKind::Middle);
        // The front sheet hides the back one where they overlap.
        painter.rect_filled(sheet(-2.0, -3.0), 1.5, visuals.weak_bg_fill);
        painter.rect_stroke(sheet(-2.0, -3.0), 1.5, stroke, StrokeKind::Middle);
    }
    let tip = if copied {
        tr!("Copied", "Скопировано")
    } else {
        tr!("Copy", "Копировать")
    };
    response.on_hover_text(tip)
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

/// The size of a modal of `wanted` points, cut to the window less
/// `margin` on each side.
pub fn modal_size(ctx: &egui::Context, wanted: egui::Vec2, margin: f32) -> egui::Vec2 {
    let room = ctx.content_rect().size() - egui::vec2(2.0 * margin, 2.0 * margin);
    wanted.min(room).max(egui::vec2(200.0, 160.0))
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
