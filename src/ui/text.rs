//! The Text panel: the font of the document's text, the font of code and
//! the text size. Changes apply at once, so the document behind the panel
//! shows them.

use crate::fonts::Family;
use crate::settings::Settings;
use crate::style::{BODY_SIZES, DEFAULT_BODY_SIZE, DEFAULT_FAMILIES};
use egui::{Align2, ComboBox, Id, PopupCloseBehavior, TextEdit, Ui, vec2};
use std::sync::Arc;

/// What the reader changed in the panel.
pub enum TextChange {
    /// A family for text, or for code with `mono`; `None` for the default.
    Font {
        mono: bool,
        family: Option<Family>,
    },
    Size(f32),
    /// Back to the default fonts and size.
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
                    });
                ui.add_space(4.0);
                let default = settings.font.is_none()
                    && settings.mono_font.is_none()
                    && settings.text_size == DEFAULT_BODY_SIZE;
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
