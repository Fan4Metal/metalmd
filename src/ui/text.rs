//! The Text panel: the font of the document's text, the font of code, the
//! text size and the colour of highlighted text. Changes apply at once, so
//! the document behind the panel shows them.

use crate::fonts::Family;
use crate::settings::Settings;
use crate::style::{
    BODY_SIZES, CONTENT_WIDTHS, DEFAULT_BODY_SIZE, DEFAULT_CONTENT_WIDTH, DEFAULT_FAMILIES,
    mark_presets,
};
use egui::{
    Align2, Color32, ComboBox, Id, PopupCloseBehavior, Response, Sense, Stroke, StrokeKind,
    TextEdit, Ui, vec2,
};
use std::sync::Arc;

/// What the reader changed in the panel.
pub enum TextChange {
    /// A family for text, or for code with `mono`; `None` for the default.
    Font {
        mono: bool,
        family: Option<Family>,
    },
    Size(f32),
    /// The widest text column, in points; the end of `CONTENT_WIDTHS` for
    /// the whole window.
    Width(f32),
    /// The highlight colour of the dark or the light theme; `None` for the
    /// theme's own.
    MarkColor {
        dark: bool,
        color: Option<Color32>,
    },
    /// Back to the default fonts, size and highlight colours.
    Reset,
}

#[derive(Default)]
pub struct TextPanel {
    pub open: bool,
    /// The window it is open in.
    pub window: u64,
    /// The system's font families, once listed (see `fonts::system_families`).
    pub families: Option<Arc<Vec<Family>>>,
    /// Being listed on a background thread.
    pub listing: bool,
    /// Typed into a list of families to narrow it.
    filter: String,
}

impl TextPanel {
    /// Draws the panel while it is open, in the top right corner under the
    /// toolbar; it closes with its cross (Esc is handled with the other
    /// keys).
    pub fn show(&mut self, ctx: &egui::Context, settings: &Settings) -> Option<TextChange> {
        if !self.open {
            return None;
        }
        let mut open = true;
        let mut change = None;
        // egui titles windows in the heading size, too large for a panel.
        egui::Window::new(egui::RichText::new(tr!("Text", "Текст")).size(15.0))
            .id(Id::new(("text-panel", self.window)))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::RIGHT_TOP, vec2(-10.0, 44.0))
            .show(ctx, |ui| {
                egui::Grid::new("text-panel-grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label(tr!("Text font", "Шрифт текста"));
                        let body = settings.font.as_ref().map(|f| f.family.as_str());
                        change = change.take().or(self.font_list(ui, false, body));
                        ui.end_row();

                        ui.label(tr!("Code font", "Шрифт кода"));
                        let mono = settings.mono_font.as_ref().map(|f| f.family.as_str());
                        change = change.take().or(self.font_list(ui, true, mono));
                        ui.end_row();

                        ui.label(tr!("Text size", "Размер текста"));
                        let mut size = settings.text_size;
                        let slider = egui::Slider::new(&mut size, BODY_SIZES)
                            .step_by(1.0)
                            .fixed_decimals(0)
                            .suffix(tr!(" pt", " пт"));
                        if ui.add(slider).changed() {
                            change = Some(TextChange::Size(size));
                        }
                        ui.end_row();

                        ui.label(tr!("Text width", "Ширина текста"));
                        let mut width = settings.text_width;
                        let widest = *CONTENT_WIDTHS.end();
                        let slider = egui::Slider::new(&mut width, CONTENT_WIDTHS)
                            .step_by(40.0)
                            .custom_formatter(move |value, _| {
                                if value >= widest as f64 {
                                    tr!("whole window", "во всё окно").to_owned()
                                } else {
                                    tr!(format!("{value:.0} pt"), format!("{value:.0} пт"))
                                }
                            })
                            .custom_parser(|text| {
                                let digits: String =
                                    text.chars().filter(char::is_ascii_digit).collect();
                                digits.parse().ok()
                            });
                        if ui.add(slider).changed() {
                            change = Some(TextChange::Width(width));
                        }
                        ui.end_row();

                        ui.label(tr!("Highlight colour", "Цвет выделения"));
                        let dark = ctx.theme() == egui::Theme::Dark;
                        if let Some(color) = mark_colors(ui, settings, dark) {
                            change = Some(TextChange::MarkColor { dark, color });
                        }
                        ui.end_row();
                    });
                ui.add_space(4.0);
                let default = settings.font.is_none()
                    && settings.mono_font.is_none()
                    && settings.text_size == DEFAULT_BODY_SIZE
                    && settings.text_width == DEFAULT_CONTENT_WIDTH
                    && settings.mark_color.is_none()
                    && settings.mark_color_dark.is_none();
                if ui
                    .add_enabled(!default, egui::Button::new(tr!("Reset", "Сбросить")))
                    .clicked()
                {
                    change = Some(TextChange::Reset);
                }
            });
        if !open {
            self.open = false;
        }
        change
    }

    /// The list of families for text, or for code with `mono` (only the
    /// monospaced ones). `current` is the chosen family, `None` for the
    /// default.
    fn font_list(&mut self, ui: &mut Ui, mono: bool, current: Option<&str>) -> Option<TextChange> {
        let default_name = if mono {
            DEFAULT_FAMILIES.1
        } else {
            DEFAULT_FAMILIES.0
        };
        let default_label = tr!(
            format!("Default ({default_name})"),
            format!("По умолчанию ({default_name})")
        );
        let shown = current.map_or_else(|| default_label.clone(), str::to_owned);
        let mut change = None;
        ComboBox::from_id_salt(("font-list", mono))
            .selected_text(shown)
            .width(260.0)
            .height(380.0)
            // Typing into the search field must not close it.
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .show_ui(ui, |ui| {
                let search = ui.add(
                    TextEdit::singleline(&mut self.filter)
                        .hint_text(tr!("Search", "Поиск"))
                        .desired_width(f32::INFINITY),
                );
                if !search.has_focus() && ui.memory(|m| m.focused().is_none()) {
                    search.request_focus();
                }
                ui.separator();
                let mut chosen = None;
                if ui
                    .selectable_label(current.is_none(), &default_label)
                    .clicked()
                {
                    chosen = Some(None);
                }
                match &self.families {
                    None => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.weak(tr!("Reading the fonts…", "Чтение шрифтов…"));
                        });
                    }
                    Some(families) => {
                        let needle = self.filter.to_lowercase();
                        let shown = families.iter().filter(|f| {
                            (!mono || f.monospaced)
                                && (needle.is_empty() || f.name.to_lowercase().contains(&needle))
                        });
                        for family in shown {
                            let selected =
                                current.is_some_and(|c| c.eq_ignore_ascii_case(&family.name));
                            if ui.selectable_label(selected, &family.name).clicked() {
                                chosen = Some(Some(family.clone()));
                            }
                        }
                    }
                }
                if let Some(family) = chosen {
                    change = Some(TextChange::Font { mono, family });
                    self.filter.clear();
                    ui.close();
                }
            });
        change
    }
}

/// The highlight colours of the theme (`dark` or light) as swatches, the
/// chosen one outlined, then a button for a colour of one's own. Gives the
/// colour clicked: `Some(None)` for the theme's own.
fn mark_colors(ui: &mut Ui, settings: &Settings, dark: bool) -> Option<Option<Color32>> {
    let presets = mark_presets(dark);
    let current = settings.mark_color(dark).unwrap_or(presets[0].0);
    let mut chosen = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (i, (color, name)) in presets.into_iter().enumerate() {
            if swatch(ui, color, color == current)
                .on_hover_text(name)
                .clicked()
            {
                chosen = Some((i > 0).then_some(color));
            }
        }
        let mut rgb = [current.r(), current.g(), current.b()];
        let custom = ui
            .color_edit_button_srgb(&mut rgb)
            .on_hover_text(tr!("Another colour", "Другой цвет"));
        if custom.changed() {
            chosen = Some(Some(Color32::from_rgb(rgb[0], rgb[1], rgb[2])));
        }
    });
    chosen
}

/// A square of `color` to click, outlined when `selected`.
fn swatch(ui: &mut Ui, color: Color32, selected: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
    let visuals = ui.visuals();
    let stroke = if selected {
        Stroke::new(2.0, visuals.strong_text_color())
    } else if response.hovered() {
        Stroke::new(1.0, visuals.widgets.hovered.fg_stroke.color)
    } else {
        Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color)
    };
    ui.painter().rect_filled(rect, 4.0, color);
    ui.painter()
        .rect_stroke(rect, 4.0, stroke, StrokeKind::Inside);
    response
}
