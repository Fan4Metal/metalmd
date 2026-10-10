//! The start page, shown when no document is open: the icon, the name, the
//! version and what to do, then the recent files.

use crate::style::Palette;
use egui::{CursorIcon, Label, RichText, ScrollArea, Sense, TextureHandle, Ui};
use std::path::{Path, PathBuf};

const ICON: f32 = 112.0;

pub enum HomeAction {
    Open(PathBuf),
}

pub fn show(
    ui: &mut Ui,
    recent: &[PathBuf],
    palette: &Palette,
    icon_cache: &mut Option<TextureHandle>,
) -> Option<HomeAction> {
    let mut action = None;
    let icon = super::app_icon(ui.ctx(), ICON, icon_cache);
    ScrollArea::vertical()
        .id_salt("home-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                // The icon's bottom edge at the middle of the window, as in
                // qview and Disk Flashlight.
                ui.add_space((ui.available_height() * 0.5 - ICON).max(16.0));
                ui.image((icon.id(), egui::vec2(ICON, ICON)));
                ui.add_space(14.0);
                ui.label(RichText::new("qmarkdown").size(30.0).strong());
                ui.weak(format!("Version {}", crate::VERSION));
                ui.add_space(18.0);
                ui.label("Open a Markdown file with Ctrl+O or drop it here");
                if !recent.is_empty() {
                    ui.add_space(32.0);
                    ui.set_max_width(520.0);
                    ui.label(RichText::new("RECENT").size(11.0).color(palette.muted));
                    ui.add_space(6.0);
                    for path in recent {
                        if let Some(clicked) = recent_entry(ui, path, palette) {
                            action = Some(HomeAction::Open(clicked));
                        }
                    }
                    ui.add_space(16.0);
                }
            });
        });
    action
}

/// One recent file: its name as a link, its folder below. The path when
/// clicked.
fn recent_entry(ui: &mut Ui, path: &Path, palette: &Palette) -> Option<PathBuf> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let dir = path
        .parent()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    let response = ui
        .add(Label::new(RichText::new(name).size(14.0).color(palette.link)).sense(Sense::click()))
        .on_hover_text(path.display().to_string());
    if response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.add(Label::new(RichText::new(dir).size(11.0).color(palette.muted)).truncate());
    ui.add_space(4.0);
    response.clicked().then(|| path.to_path_buf())
}
