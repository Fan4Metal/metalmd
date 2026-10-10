//! The About dialog: icon, name, version, a short description, the author,
//! the license, where the settings are kept and the Markpad credit.

use egui::{Key, TextureHandle};

const MARKPAD_URL: &str = "https://github.com/sftwrdotdev/Markpad";

#[derive(Default)]
pub struct About {
    pub open: bool,
    /// Opened this frame: the key that opened it must not close it.
    pub fresh: bool,
    icon: Option<TextureHandle>,
}

impl About {
    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.fresh = self.open;
    }

    /// Draws the dialog while it is open; closes it on the cross, Esc,
    /// Enter or a click outside (F1 is handled with the other shortcuts).
    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        const ICON: f32 = 64.0;
        const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
        const LICENSE: &str = env!("CARGO_PKG_LICENSE");
        /// Cargo joins several authors with `:`.
        const AUTHORS: &str = env!("CARGO_PKG_AUTHORS");
        let fresh = std::mem::take(&mut self.fresh);
        let icon = super::app_icon(ctx, ICON, &mut self.icon);
        let modal = egui::Modal::new(egui::Id::new("about")).show(ctx, |ui| {
            ui.set_width(340.0);
            // The cross in the top right corner, over the centred content
            // (a child Ui takes no room in the layout).
            let corner = egui::Rect::from_min_size(
                egui::pos2(ui.max_rect().right() - 24.0, ui.cursor().top()),
                egui::vec2(24.0, 24.0),
            );
            let closed =
                super::close_cross(&mut ui.new_child(egui::UiBuilder::new().max_rect(corner)));
            ui.vertical_centered(|ui| {
                ui.add_space(4.0);
                ui.image((icon.id(), egui::vec2(ICON, ICON)));
                ui.add_space(6.0);
                ui.heading("MetalMD");
                ui.label(format!("Version {}", crate::VERSION));
                ui.add_space(8.0);
                ui.label("A fast Markdown viewer for Windows.");
                ui.add_space(8.0);
                if !AUTHORS.is_empty() {
                    ui.label(format!("Author: {}", AUTHORS.replace(':', ", ")));
                }
                if !REPOSITORY.is_empty() && ui.link("Homepage").on_hover_text(REPOSITORY).clicked()
                {
                    let _ = open::that_detached(REPOSITORY);
                }
                if !LICENSE.is_empty() {
                    ui.weak(format!("{LICENSE} License"));
                }
                let stored = crate::settings::location();
                let place = ui.weak(if stored.portable {
                    "Settings: next to the program (portable)"
                } else {
                    "Settings: in the user profile"
                });
                match &stored.file {
                    Some(path) => place.on_hover_text(path.display().to_string()),
                    None => place.on_hover_text("Settings are not saved"),
                };
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.weak("Markdown support is ported from");
                if ui
                    .link("Markpad by Alec Ames")
                    .on_hover_text(format!("{MARKPAD_URL}\nBSD 3-Clause License"))
                    .clicked()
                {
                    let _ = open::that_detached(MARKPAD_URL);
                }
                ui.add_space(4.0);
            });
            closed
        });
        let closed = modal.inner
            || (!fresh
                && (ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter))
                    || modal.should_close()));
        if closed {
            self.open = false;
        }
    }
}
