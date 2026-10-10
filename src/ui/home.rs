//! The page shown when no document is open.

use crate::style::Palette;
use egui::{CursorIcon, Label, RichText, Sense, Ui};
use std::path::{Path, PathBuf};

pub enum HomeAction {
    OpenDialog,
    Open(PathBuf),
}

pub fn show(ui: &mut Ui, recent: &[PathBuf], palette: &Palette) -> Option<HomeAction> {
    let mut action = None;
    ui.vertical_centered(|ui| {
        ui.set_max_width(520.0);
        ui.add_space((ui.available_height() * 0.18).max(24.0));
        ui.label(RichText::new("qmarkdown").size(30.0).strong());
        ui.add_space(4.0);
        ui.label(RichText::new("Fast Markdown viewer").color(palette.muted));
        ui.add_space(24.0);
        if ui
            .button(RichText::new("Open file…  (Ctrl+O)").size(15.0))
            .clicked()
        {
            action = Some(HomeAction::OpenDialog);
        }
        ui.add_space(8.0);
        ui.label(
            RichText::new("or drop a .md file onto this window")
                .size(12.0)
                .color(palette.muted),
        );
        if !recent.is_empty() {
            ui.add_space(28.0);
            ui.label(RichText::new("RECENT").size(11.0).color(palette.muted));
            ui.add_space(6.0);
            for path in recent {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let dir = path
                    .parent()
                    .map(Path::display)
                    .map(|d| d.to_string())
                    .unwrap_or_default();
                let response = ui.add(
                    Label::new(RichText::new(name).size(14.0).color(palette.link))
                        .sense(Sense::click()),
                );
                let response = response.on_hover_text(path.display().to_string());
                if response.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                if response.clicked() {
                    action = Some(HomeAction::Open(path.clone()));
                }
                ui.add(Label::new(RichText::new(dir).size(11.0).color(palette.muted)).truncate());
                ui.add_space(4.0);
            }
        }
    });
    action
}
