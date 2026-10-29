//! The picture viewer: a picture of the document shown on its own, over
//! the window, as the reader's Image Viewer plugin for Obsidian shows one.
//!
//! It goes through all the document's pictures (images that are not links
//! and diagrams, in document order): ← and →, the ‹ › buttons, the strip
//! of thumbnails along the bottom, or the wheel in the "navigate" mode;
//! from the last it goes on to the first. The top row shows "3 / 12" and
//! the picture's name. A picture opens at the start zoom
//! (`StartZoom`); the wheel (or Ctrl+wheel in the "navigate" mode), `+`
//! and `-` zoom about the pointer from a fifth of that to twelve times it,
//! `0` goes back to it, a double click switches between it and twice it,
//! and a picture larger than the window is moved by dragging. Esc or a
//! click beside the picture closes it. The options are in its context
//! menu (`ViewerSettings`).
//!
//! A diagram is drawn again at the scale it is shown at, under a variant
//! of its own in the render cache, so the document behind keeps its own
//! texture.

use super::viewer::Pictures;
use crate::settings::{StartZoom, ThumbShape, ViewerSettings, WheelMode};
use crate::style::Palette;
use egui::{
    Align, Color32, CursorIcon, Id, Order, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2, pos2, vec2,
};

/// What is shown.
#[derive(Clone, Debug, PartialEq)]
pub enum Shown {
    /// An image, by the URI the viewer loads it from.
    Image(String),
    /// A Mermaid diagram, by its source.
    Diagram(String),
}

/// One picture of the document, with the name the top row shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub shown: Shown,
    pub name: String,
}

/// The plugin's defaults: one wheel notch or key zooms by this factor,
/// within these multiples of the start zoom.
const STEP: f32 = 1.15;
const MIN_ZOOM: f32 = 0.2;
const MAX_ZOOM: f32 = 12.0;
/// The backdrop's opacity (the plugin's 0.92).
const BACKDROP: u8 = 235;
const TOP_BAR: f32 = 44.0;
/// The range of a thumbnail's height (`ViewerSettings::thumb_height`);
/// its width follows `ThumbShape`: the plugin's 84:64 across or upright,
/// or square.
const THUMB_HEIGHTS: std::ops::RangeInclusive<u16> = 40..=160;
const THUMB_ASPECT: f32 = 84.0 / 64.0;
const THUMB_GAP: f32 = 8.0;
const STRIP_PADDING: Vec2 = vec2(14.0, 10.0);

pub struct Lightbox {
    items: Vec<Item>,
    index: usize,
    /// Times its own size; `None` at the start zoom.
    zoom: Option<f32>,
    /// From the centre of the stage to the centre of the picture.
    offset: Vec2,
    /// Wheel notches towards the next picture, in the "navigate" mode.
    wheel: f32,
    /// The current thumbnail is to be scrolled into view.
    reveal_thumb: bool,
}

impl Lightbox {
    /// Shows `items[index]`; `items` must not be empty.
    pub fn new(items: Vec<Item>, index: usize) -> Lightbox {
        Lightbox {
            index: index.min(items.len().saturating_sub(1)),
            items,
            zoom: None,
            offset: Vec2::ZERO,
            wheel: 0.0,
            reveal_thumb: true,
        }
    }

    /// The picture `step` places on, going round from the last to the
    /// first, at the start zoom.
    fn go(&mut self, step: isize) {
        let count = self.items.len() as isize;
        if count == 0 {
            return;
        }
        self.index = (self.index as isize + step).rem_euclid(count) as usize;
        self.zoom = None;
        self.offset = Vec2::ZERO;
        self.wheel = 0.0;
        self.reveal_thumb = true;
    }

    /// Draws it over the window and follows the pointer and keys. Returns
    /// whether it is still open and whether `options` changed (its menu).
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        pictures: &Pictures,
        palette: &Palette,
        options: &mut ViewerSettings,
    ) -> (bool, bool) {
        let before = *options;
        let screen = ctx.content_rect();
        // Over the document and its windows, under the menus (its own
        // context menu among them), which are `Foreground`.
        let area = egui::Area::new(Id::new("lightbox"))
            .order(Order::Middle)
            .fixed_pos(screen.min)
            .interactable(true);
        let mut open = !self.items.is_empty();
        area.show(ctx, |ui| {
            if self.items.is_empty() {
                return;
            }
            ui.set_min_size(screen.size());
            let backdrop = ui.interact(screen, Id::new("lightbox-backdrop"), Sense::click());
            // A click that closes the menu must not close the viewer too.
            // (`Popup::default_response_id` is the response's id + "popup".)
            let menu_open =
                egui::Popup::is_id_open(ctx, egui::Popup::default_response_id(&backdrop))
                    || egui::Popup::is_id_open(ctx, Id::new("lightbox-picture").with("popup"));
            ui.painter()
                .rect_filled(screen, 0.0, Color32::from_black_alpha(BACKDROP));

            let many = self.items.len() > 1;
            let strip = (options.thumbnails && many).then(|| {
                Rect::from_min_max(
                    pos2(
                        screen.left(),
                        screen.bottom() - thumb_height(options) - 2.0 * STRIP_PADDING.y,
                    ),
                    screen.max,
                )
            });
            let stage = Rect::from_min_max(
                pos2(screen.left(), screen.top() + TOP_BAR),
                pos2(screen.right(), strip.map_or(screen.bottom(), |s| s.top())),
            );

            // Keys: ← → go, + - 0 zoom.
            let (left, right, plus, minus, zero) = ui.input_mut(|i| {
                let none = egui::Modifiers::NONE;
                let plus_text = i
                    .events
                    .iter()
                    .any(|e| matches!(e, egui::Event::Text(t) if t == "+"));
                (
                    i.consume_key(none, egui::Key::ArrowLeft),
                    i.consume_key(none, egui::Key::ArrowRight),
                    i.consume_key(none, egui::Key::Plus)
                        || i.consume_key(none, egui::Key::Equals)
                        || plus_text,
                    i.consume_key(none, egui::Key::Minus),
                    i.consume_key(none, egui::Key::Num0),
                )
            });
            if many && left {
                self.go(-1);
            }
            if many && right {
                self.go(1);
            }

            let pointer = ui.input(|i| i.pointer.hover_pos());
            let over_strip = pointer.is_some_and(|p| strip.is_some_and(|s| s.contains(p)));
            let (notches, ctrl) = if over_strip {
                (0.0, false)
            } else {
                wheel_notches(ui)
            };
            let navigate = options.wheel == WheelMode::Navigate && !ctrl;
            if notches != 0.0 && navigate && many {
                // One notch, one picture; a touchpad's fractions add up.
                if self.wheel.signum() != (-notches).signum() {
                    self.wheel = 0.0;
                }
                self.wheel -= notches;
                if self.wheel >= 1.0 {
                    self.go(1);
                } else if self.wheel <= -1.0 {
                    self.go(-1);
                }
            }

            let shown = self.items[self.index].shown.clone();
            let mut on_picture = None;
            match natural_size(ui, &shown, pictures, palette.dark) {
                None => {
                    ui.painter().text(
                        stage.center(),
                        egui::Align2::CENTER_CENTER,
                        "…",
                        egui::FontId::proportional(28.0),
                        Color32::WHITE,
                    );
                }
                Some(natural) => {
                    let base = start_scale(options.start_zoom, stage.size(), natural);
                    let (least, most) = (base * MIN_ZOOM, base * MAX_ZOOM);
                    let mut zoom = self.zoom.unwrap_or(base);
                    let rect = Rect::from_center_size(stage.center() + self.offset, natural * zoom);
                    let mut wanted = None;
                    if notches != 0.0 && !navigate {
                        wanted =
                            Some((zoom * STEP.powf(notches), pointer.unwrap_or(stage.center())));
                    }
                    if plus {
                        wanted = Some((zoom * STEP, stage.center()));
                    }
                    if minus {
                        wanted = Some((zoom / STEP, stage.center()));
                    }
                    let picture = ui.interact(
                        rect.intersect(stage),
                        Id::new("lightbox-picture"),
                        Sense::click_and_drag(),
                    );
                    if picture.double_clicked() {
                        if (zoom - base).abs() > base * 1e-3 {
                            self.zoom = None;
                            self.offset = Vec2::ZERO;
                            zoom = base;
                        } else {
                            wanted = Some((base * 2.0, stage.center()));
                        }
                    }
                    if zero {
                        self.zoom = None;
                        self.offset = Vec2::ZERO;
                        zoom = base;
                        wanted = None;
                    }
                    if let Some((target, about)) = wanted {
                        let target = target.clamp(least, most);
                        self.zoom_about(about, rect, zoom, target, stage);
                        zoom = target;
                    }
                    // Only a picture larger than the stage moves.
                    let size = natural * zoom;
                    let pannable = size.x > stage.width() + 1.0 || size.y > stage.height() + 1.0;
                    if pannable && picture.dragged() {
                        self.offset += picture.drag_delta();
                        self.zoom = Some(zoom);
                    }
                    if !pannable {
                        self.offset = Vec2::ZERO;
                    }
                    let rect = Rect::from_center_size(stage.center() + self.offset, size);

                    let mut stage_ui = ui.new_child(egui::UiBuilder::new().max_rect(stage));
                    stage_ui.set_clip_rect(stage.intersect(ui.clip_rect()));
                    // On the page's colour: diagrams and many images are
                    // drawn on a transparent ground.
                    stage_ui.painter().rect_filled(rect, 2.0, palette.page);
                    paint(
                        &stage_ui,
                        &shown,
                        pictures,
                        palette.dark,
                        rect,
                        natural,
                        zoom,
                    );
                    stage_ui.painter().rect_stroke(
                        rect,
                        2.0,
                        Stroke::new(1.0, Color32::from_white_alpha(40)),
                        StrokeKind::Outside,
                    );
                    if picture.hovered() {
                        ui.ctx().set_cursor_icon(if !pannable {
                            CursorIcon::ZoomIn
                        } else if picture.dragged() {
                            CursorIcon::Grabbing
                        } else {
                            CursorIcon::Grab
                        });
                    }
                    on_picture = Some(picture);
                }
            }

            if many && options.arrows {
                for (step, left_side) in [(-1, true), (1, false)] {
                    let x = if left_side {
                        stage.left() + 16.0
                    } else {
                        stage.right() - 16.0 - 48.0
                    };
                    let rect =
                        Rect::from_min_size(pos2(x, stage.center().y - 32.0), vec2(48.0, 64.0));
                    let response = ui
                        .interact(rect, Id::new(("lightbox-arrow", step)), Sense::click())
                        .on_hover_text(if left_side {
                            tr!("Previous (←)", "Предыдущая (←)")
                        } else {
                            tr!("Next (→)", "Следующая (→)")
                        });
                    paint_arrow(ui, rect, left_side, response.hovered());
                    if response.clicked() {
                        self.go(step);
                    }
                }
            }

            self.top_row(ui, screen);
            if let Some(strip) = strip {
                self.thumbnails(ui, strip, options, pictures, palette);
            }

            let mut changed = options_menu(&backdrop, options);
            if let Some(picture) = &on_picture {
                changed |= options_menu(picture, options);
            }
            if changed {
                // Applied at once: the start zoom anew.
                self.zoom = None;
                self.offset = Vec2::ZERO;
            }
            if backdrop.clicked() && !menu_open {
                open = false;
            }
        });
        (open, *options != before)
    }

    /// "3 / 12" on the left, the picture's name in the middle.
    fn top_row(&self, ui: &egui::Ui, screen: Rect) {
        let y = screen.top() + TOP_BAR / 2.0;
        let color = Color32::from_white_alpha(217);
        let font = egui::FontId::proportional(14.0);
        if self.items.len() > 1 {
            ui.painter().text(
                pos2(screen.left() + 14.0, y),
                egui::Align2::LEFT_CENTER,
                format!("{} / {}", self.index + 1, self.items.len()),
                font.clone(),
                color,
            );
        }
        let name = &self.items[self.index].name;
        let mut job = egui::text::LayoutJob::simple_singleline(name.clone(), font, color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(screen.width() * 0.6);
        let galley = ui.fonts_mut(|f| f.layout_job(job));
        let pos = pos2(
            screen.center().x - galley.size().x / 2.0,
            y - galley.size().y / 2.0,
        );
        ui.painter().galley(pos, galley, color);
    }

    /// The strip of thumbnails: every picture at one size, cropped to fill
    /// it, the current one bright and framed; centred while they fit.
    fn thumbnails(
        &mut self,
        ui: &mut egui::Ui,
        strip: Rect,
        options: &ViewerSettings,
        pictures: &Pictures,
        palette: &Palette,
    ) {
        let height = thumb_height(options);
        let thumb = vec2(
            match options.thumb_shape {
                ThumbShape::Landscape => (height * THUMB_ASPECT).round(),
                ThumbShape::Portrait => (height / THUMB_ASPECT).round(),
                ThumbShape::Square => height,
            },
            height,
        );
        let inner = strip.shrink2(STRIP_PADDING);
        let mut strip_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::left_to_right(Align::Center)),
        );
        let count = self.items.len();
        let total = count as f32 * thumb.x + (count - 1) as f32 * THUMB_GAP;
        let mut go_to = None;
        egui::ScrollArea::horizontal()
            .id_salt("lightbox-thumbs")
            .auto_shrink([false, true])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(&mut strip_ui, |ui| {
                ui.spacing_mut().item_spacing.x = THUMB_GAP;
                if total < inner.width() {
                    ui.add_space((inner.width() - total) / 2.0 - THUMB_GAP);
                }
                for (i, item) in self.items.iter().enumerate() {
                    let (rect, response) = ui.allocate_exact_size(thumb, Sense::click());
                    let current = i == self.index;
                    let alpha = if current {
                        1.0
                    } else if response.hovered() {
                        0.85
                    } else {
                        0.55
                    };
                    ui.painter()
                        .rect_filled(rect, 6.0, Color32::from_white_alpha(13));
                    paint_thumbnail(ui, &item.shown, rect, alpha, pictures, palette.dark);
                    if current {
                        ui.painter().rect_stroke(
                            rect,
                            6.0,
                            Stroke::new(2.0, palette.accent),
                            StrokeKind::Inside,
                        );
                        if self.reveal_thumb {
                            response.scroll_to_me(Some(Align::Center));
                        }
                    }
                    if response.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if response.on_hover_text(&item.name).clicked() {
                        go_to = Some(i);
                    }
                }
            });
        self.reveal_thumb = false;
        if let Some(i) = go_to {
            self.go(i as isize - self.index as isize);
        }
    }

    /// Zooms from `zoom` to `wanted`, keeping the point under `about`
    /// where it is.
    fn zoom_about(&mut self, about: Pos2, rect: Rect, zoom: f32, wanted: f32, stage: Rect) {
        let from_centre = about - rect.center();
        let centre = about - from_centre * (wanted / zoom);
        self.offset = centre - stage.center();
        self.zoom = Some(wanted);
    }
}

/// The thumbnails' height in points, within [`THUMB_HEIGHTS`] whatever
/// the settings file says.
fn thumb_height(options: &ViewerSettings) -> f32 {
    f32::from(
        options
            .thumb_height
            .clamp(*THUMB_HEIGHTS.start(), *THUMB_HEIGHTS.end()),
    )
}

/// The scale a picture of `natural` size opens at in a stage of `room`.
fn start_scale(mode: StartZoom, room: Vec2, natural: Vec2) -> f32 {
    let (w, h) = (room.x / natural.x, room.y / natural.y);
    match mode {
        StartZoom::FitDown => w.min(h).min(1.0),
        StartZoom::Fit => w.min(h),
        StartZoom::Height => h,
        StartZoom::Width => w,
        StartZoom::Actual => 1.0,
    }
    .max(1e-3)
}

/// The viewer's context menu: its options. Returns whether one changed.
fn options_menu(response: &egui::Response, options: &mut ViewerSettings) -> bool {
    let before = *options;
    super::menu::context_menu(response, |ui| {
        ui.checkbox(&mut options.thumbnails, tr!("Thumbnails", "Миниатюры"));
        ui.add_enabled_ui(options.thumbnails, |ui| {
            for (shape, label) in [
                (ThumbShape::Landscape, tr!("Landscape", "Альбомные")),
                (ThumbShape::Portrait, tr!("Portrait", "Портретные")),
                (ThumbShape::Square, tr!("Square", "Квадратные")),
            ] {
                ui.horizontal(|ui| {
                    // Under the checkbox's text.
                    ui.add_space(ui.spacing().icon_width);
                    if ui
                        .radio_value(&mut options.thumb_shape, shape, label)
                        .clicked()
                    {
                        ui.close();
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.add_space(ui.spacing().icon_width);
                ui.label(tr!("Height", "Высота"));
                ui.add(
                    egui::Slider::new(&mut options.thumb_height, THUMB_HEIGHTS)
                        .suffix(tr!(" pt", " пт")),
                );
            });
        });
        ui.checkbox(&mut options.arrows, tr!("Arrows ‹ ›", "Стрелки ‹ ›"));
        ui.separator();
        ui.weak(tr!("Mouse wheel", "Колесо мыши"));
        for (mode, label) in [
            (WheelMode::Zoom, tr!("Zooms", "Масштабирует")),
            (
                WheelMode::Navigate,
                tr!(
                    "Goes to the next picture (Ctrl: zooms)",
                    "Листает картинки (с Ctrl — масштабирует)"
                ),
            ),
        ] {
            if ui.radio_value(&mut options.wheel, mode, label).clicked() {
                ui.close();
            }
        }
        ui.separator();
        ui.weak(tr!("Opens", "Открывается"));
        for (mode, label) in [
            (
                StartZoom::FitDown,
                tr!("Fitted, not enlarged", "По окну, без увеличения"),
            ),
            (StartZoom::Fit, tr!("Fitted", "По окну")),
            (StartZoom::Height, tr!("Filling the height", "По высоте")),
            (StartZoom::Width, tr!("Filling the width", "По ширине")),
            (StartZoom::Actual, tr!("At 100 %", "В 100 %")),
        ] {
            if ui
                .radio_value(&mut options.start_zoom, mode, label)
                .clicked()
            {
                ui.close();
            }
        }
    });
    *options != before
}

/// A ‹ or › button: a rounded light square with a chevron.
fn paint_arrow(ui: &egui::Ui, rect: Rect, left: bool, hovered: bool) {
    let fill = Color32::from_white_alpha(if hovered { 56 } else { 20 });
    ui.painter().rect_filled(rect, 6.0, fill);
    let c = rect.center();
    let (dx, dy) = (5.0, 10.0);
    let tip = if left { -dx } else { dx };
    let points = vec![
        pos2(c.x - tip, c.y - dy),
        pos2(c.x + tip, c.y),
        pos2(c.x - tip, c.y + dy),
    ];
    ui.painter()
        .add(egui::Shape::line(points, Stroke::new(2.5, Color32::WHITE)));
}

/// A thumbnail: the picture cropped to fill `rect`, at `alpha`.
fn paint_thumbnail(
    ui: &egui::Ui,
    shown: &Shown,
    rect: Rect,
    alpha: f32,
    pictures: &Pictures,
    dark: bool,
) {
    let tint = Color32::WHITE.gamma_multiply(alpha);
    let (texture, size) = match shown {
        Shown::Image(uri) => {
            let uri = super::animation::frame_uri(ui.ctx(), uri).unwrap_or_else(|| uri.clone());
            match egui::Image::new(uri)
                .fit_to_original_size(1.0)
                .load_for_size(ui.ctx(), Vec2::INFINITY)
            {
                Ok(egui::load::TexturePoll::Ready { texture }) => (texture.id, texture.size),
                _ => return,
            }
        }
        Shown::Diagram(source) => match pictures.diagrams.peek(source, dark) {
            Some(diagram) => {
                let [w, h] = diagram.texture.size();
                (diagram.texture.id(), vec2(w as f32, h as f32))
            }
            None => return,
        },
    };
    if size.x <= 0.0 || size.y <= 0.0 {
        return;
    }
    // Cropped to the thumbnail's shape, from the middle.
    let (picture, frame) = (size.x / size.y, rect.width() / rect.height());
    let uv = if picture > frame {
        let w = frame / picture;
        Rect::from_min_max(pos2((1.0 - w) / 2.0, 0.0), pos2((1.0 + w) / 2.0, 1.0))
    } else {
        let h = picture / frame;
        Rect::from_min_max(pos2(0.0, (1.0 - h) / 2.0), pos2(1.0, (1.0 + h) / 2.0))
    };
    egui::Image::new((texture, rect.size()))
        .uv(uv)
        .tint(tint)
        .corner_radius(6)
        .paint_at(ui, rect);
}

/// The picture's own size in points, once it is loaded.
fn natural_size(ui: &egui::Ui, shown: &Shown, pictures: &Pictures, dark: bool) -> Option<Vec2> {
    match shown {
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
    ui: &egui::Ui,
    shown: &Shown,
    pictures: &Pictures,
    dark: bool,
    rect: Rect,
    natural: Vec2,
    zoom: f32,
) {
    match shown {
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

/// Wheel notches turned this frame, up positive (a touchpad's points count
/// as fractions of one), and whether Ctrl was held.
fn wheel_notches(ui: &egui::Ui) -> (f32, bool) {
    ui.input(|i| {
        let mut notches = 0.0;
        let mut ctrl = false;
        for event in &i.events {
            if let egui::Event::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } = event
            {
                notches += match unit {
                    egui::MouseWheelUnit::Point => delta.y / 50.0,
                    egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page => delta.y,
                };
                ctrl |= modifiers.command || modifiers.ctrl;
            }
        }
        (notches, ctrl)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_zoom_modes() {
        let room = vec2(800.0, 600.0);
        let big = vec2(1600.0, 600.0);
        let small = vec2(200.0, 100.0);
        assert_eq!(start_scale(StartZoom::FitDown, room, big), 0.5);
        assert_eq!(start_scale(StartZoom::FitDown, room, small), 1.0);
        assert_eq!(start_scale(StartZoom::Fit, room, small), 4.0);
        assert_eq!(start_scale(StartZoom::Height, room, big), 1.0);
        assert_eq!(start_scale(StartZoom::Width, room, big), 0.5);
        assert_eq!(start_scale(StartZoom::Actual, room, big), 1.0);
    }

    #[test]
    fn thumbnail_height_stays_in_range() {
        let height = |h| {
            thumb_height(&ViewerSettings {
                thumb_height: h,
                ..ViewerSettings::default()
            })
        };
        assert_eq!(height(96), 96.0);
        assert_eq!(height(10), 40.0);
        assert_eq!(height(500), 160.0);
    }

    #[test]
    fn going_round_the_pictures() {
        let item = |n: &str| Item {
            shown: Shown::Image(n.into()),
            name: n.into(),
        };
        let mut lightbox = Lightbox::new(vec![item("a"), item("b"), item("c")], 2);
        lightbox.zoom = Some(3.0);
        lightbox.go(1);
        assert_eq!(lightbox.index, 0);
        assert_eq!(lightbox.zoom, None, "each picture at the start zoom");
        lightbox.go(-1);
        assert_eq!(lightbox.index, 2);
        lightbox.go(-4);
        assert_eq!(lightbox.index, 1);
    }
}
