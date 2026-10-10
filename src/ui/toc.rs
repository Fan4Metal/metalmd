//! Table of contents side panel.

use crate::markdown::document::Document;
use crate::style::Palette;
use crate::ui::viewer::{ViewState, block_top};
use egui::{CursorIcon, Label, RichText, ScrollArea, Sense, Ui};

/// Index of the heading the reader is in: the last one that starts at or
/// above the top of the view. At the end of the document, where the last
/// sections can never reach the top, the last heading on screen counts.
pub fn active_heading(doc: &Document, view: &ViewState) -> Option<usize> {
    let content_end = view.offsets.last().copied().unwrap_or(0.0);
    let at_bottom =
        view.viewport_top > 1.0 && view.viewport_top + view.viewport_height >= content_end - 1.0;
    let top = if at_bottom {
        view.viewport_top + view.viewport_height - 40.0
    } else {
        view.viewport_top + 12.0
    };
    let mut active = None;
    for (i, heading) in doc.headings.iter().enumerate() {
        match block_top(view, heading.block) {
            Some(y) if y <= top => active = Some(i),
            Some(_) => break,
            None => break,
        }
    }
    active.or(if doc.headings.is_empty() {
        None
    } else {
        Some(0)
    })
}

/// Draws the outline. Returns the heading that was clicked.
pub fn show(ui: &mut Ui, doc: &Document, view: &ViewState, palette: &Palette) -> Option<usize> {
    let mut clicked = None;
    ui.add_space(10.0);
    ui.label(
        RichText::new(tr!("CONTENTS", "СОДЕРЖАНИЕ"))
            .size(11.0)
            .color(palette.muted),
    );
    ui.add_space(6.0);
    if doc.headings.is_empty() {
        ui.label(RichText::new(tr!("No headings", "Нет заголовков")).color(palette.muted));
        return None;
    }
    let min_level = doc.headings.iter().map(|h| h.level).min().unwrap_or(1);
    let active = active_heading(doc, view);
    // Keep the active entry in view when it changes, so the outline follows
    // the reader through a long document.
    let last_id = egui::Id::new("toc-last-active");
    let changed = ui.data(|d| d.get_temp::<Option<usize>>(last_id)) != Some(active);
    if changed {
        ui.data_mut(|d| d.insert_temp(last_id, active));
    }
    ScrollArea::vertical()
        .id_salt("toc-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (i, heading) in doc.headings.iter().enumerate() {
                let indent = f32::from(heading.level.saturating_sub(min_level)) * 12.0;
                let is_active = active == Some(i);
                let size = if heading.level <= min_level {
                    13.5
                } else {
                    13.0
                };
                let mut text = RichText::new(&heading.text).size(size);
                text = if is_active {
                    text.color(palette.accent).strong()
                } else if heading.level > min_level + 1 {
                    text.color(palette.muted)
                } else {
                    text
                };
                ui.horizontal(|ui| {
                    ui.add_space(indent + 4.0);
                    let response = ui.add(Label::new(text).truncate().sense(Sense::click()));
                    if response.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if is_active && changed {
                        response.scroll_to_me(Some(egui::Align::Center));
                    }
                    if is_active {
                        let rect = response.rect;
                        ui.painter().vline(
                            rect.left() - indent - 4.0,
                            rect.y_range(),
                            egui::Stroke::new(2.0, palette.accent),
                        );
                    }
                    if response.clicked() {
                        clicked = Some(i);
                    }
                });
            }
        });
    clicked
}
