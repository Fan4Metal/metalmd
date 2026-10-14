//! The About dialog: icon, name, version, a short description, the author,
//! the license, where the settings are kept, the interface language, the
//! update check and the Markpad credit.

use crate::i18n::LangChoice;
use crate::update::{Status, Updates, release_url, version_of};
use egui::{Key, TextureHandle, Ui};

const MARKPAD_URL: &str = "https://github.com/sftwrdotdev/Markpad";

/// Width of the language list, enough for its longest entry.
const LANG_WIDTH: f32 = 220.0;

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
    /// A language picked in it is applied at once and left in `lang`.
    pub fn show(&mut self, ctx: &egui::Context, lang: &mut LangChoice, updates: &mut Updates) {
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
                ui.label(tr!(
                    format!("Version {}", crate::VERSION),
                    format!("Версия {}", crate::VERSION)
                ));
                ui.add_space(8.0);
                ui.label(tr!(
                    "A fast Markdown viewer for Windows.",
                    "Быстрый просмотрщик Markdown для Windows."
                ));
                ui.add_space(8.0);
                if !AUTHORS.is_empty() {
                    let authors = AUTHORS.replace(':', ", ");
                    ui.label(tr!(
                        format!("Author: {authors}"),
                        format!("Автор: {authors}")
                    ));
                }
                if !REPOSITORY.is_empty()
                    && ui
                        .link(tr!("Homepage", "Домашняя страница"))
                        .on_hover_text(REPOSITORY)
                        .clicked()
                {
                    let _ = open::that_detached(REPOSITORY);
                }
                if !LICENSE.is_empty() {
                    ui.weak(tr!(
                        format!("{LICENSE} License"),
                        format!("Лицензия {LICENSE}")
                    ));
                }
                let stored = crate::settings::location();
                let place = ui.weak(if stored.portable {
                    tr!(
                        "Settings: next to the program (portable)",
                        "Настройки: рядом с программой (портативно)"
                    )
                } else {
                    tr!(
                        "Settings: in the user profile",
                        "Настройки: в профиле пользователя"
                    )
                });
                match &stored.file {
                    Some(path) => place.on_hover_text(path.display().to_string()),
                    None => place
                        .on_hover_text(tr!("Settings are not saved", "Настройки не сохраняются")),
                };
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                // The label over the list, both centred; a fixed width
                // keeps the list from jumping when the language changes.
                ui.weak(tr!("Interface language", "Язык интерфейса"));
                let mut choice = *lang;
                // A combo box lays itself out left to right, ignoring the
                // centring: indented by hand (`width` is its outer width).
                ui.horizontal(|ui| {
                    ui.add_space(((ui.available_width() - LANG_WIDTH) / 2.0).max(0.0));
                    egui::ComboBox::from_id_salt("language")
                        .selected_text(choice.label())
                        .width(LANG_WIDTH)
                        .show_ui(ui, |ui| {
                            for c in LangChoice::ALL {
                                ui.selectable_value(&mut choice, c, c.label());
                            }
                        });
                });
                if choice != *lang {
                    *lang = choice;
                    crate::i18n::set_lang(choice.resolve());
                }
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                update_section(ui, updates);
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.weak(tr!(
                    "Markdown support is ported from",
                    "Поддержка Markdown перенесена из"
                ));
                if ui
                    .link(tr!("Markpad by Alec Ames", "Markpad (автор Alec Ames)"))
                    .on_hover_text(format!(
                        "{MARKPAD_URL}\n{}",
                        tr!("BSD 3-Clause License", "Лицензия BSD 3-Clause")
                    ))
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

/// The update check: the start-up option, a button to check now and the
/// outcome.
fn update_section(ui: &mut Ui, updates: &mut Updates) {
    let before = updates.enabled;
    ui.checkbox(
        &mut updates.enabled,
        tr!(
            "Check for updates at start-up (once a day)",
            "Проверять обновления при запуске (раз в сутки)"
        ),
    )
    .on_hover_text(tr!(
        "Asks api.github.com for the latest release; nothing else is sent, and nothing is downloaded",
        "Запрашивает у api.github.com последний выпуск; больше ничего не отправляется и не скачивается"
    ));
    if updates.enabled && !before {
        updates.start_if_due(ui.ctx());
    }
    let checking = updates.status == Status::Checking;
    if ui
        .add_enabled(
            !checking,
            egui::Button::new(tr!("Check now", "Проверить сейчас")),
        )
        .clicked()
    {
        updates.start(ui.ctx());
    }
    match &updates.status {
        Status::Unknown => {}
        Status::Checking => {
            ui.horizontal(|ui| {
                // Centred by hand, as the language list above.
                ui.add_space(((ui.available_width() - 110.0) / 2.0).max(0.0));
                ui.add(egui::Spinner::new());
                ui.weak(tr!("Checking…", "Проверка…"));
            });
        }
        Status::UpToDate => {
            ui.weak(tr!(
                "This is the latest version",
                "Установлена последняя версия"
            ));
        }
        Status::Newer(tag) => {
            let version = version_of(tag);
            let url = release_url(tag);
            if ui
                .link(tr!(
                    format!("Version {version} is available"),
                    format!("Доступна версия {version}")
                ))
                .on_hover_text(url.as_str())
                .clicked()
            {
                let _ = open::that_detached(&url);
            }
        }
        Status::Failed(e) => {
            ui.weak(tr!(
                "Could not check for updates",
                "Не удалось проверить обновления"
            ))
            .on_hover_text(e);
        }
    }
}
