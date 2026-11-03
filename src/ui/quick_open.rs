//! Quick navigation among tabs, recent documents and the current vault.
//! The vault is requested only when the reader opens this dialog.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::{Context, Key, Modifiers};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    Open,
    Recent,
    Vault,
}

struct Entry {
    path: PathBuf,
    name: String,
    lower_name: String,
    lower_path: String,
    source: Source,
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

#[derive(Default)]
pub struct QuickOpen {
    pub open: bool,
    query: String,
    entries: Vec<Entry>,
    keys: HashSet<String>,
    matches: Vec<usize>,
    selected: usize,
    focus: bool,
    listing: Option<Receiver<Vec<PathBuf>>>,
    error: Option<String>,
}

impl QuickOpen {
    pub fn start(
        &mut self,
        tabs: impl IntoIterator<Item = PathBuf>,
        recent: &[PathBuf],
        folder: Option<&Path>,
        ctx: &Context,
    ) {
        self.focus = true;
        if self.open {
            return;
        }
        *self = Self {
            open: true,
            focus: true,
            ..Self::default()
        };
        self.add(tabs, Source::Open);
        self.add(recent.iter().cloned(), Source::Recent);
        self.filter();
        if let Some(folder) = folder {
            let folder = folder.to_owned();
            let ctx = ctx.clone();
            let viewport = ctx.viewport_id();
            let (sender, receiver) = channel();
            match std::thread::Builder::new()
                .name("quick-open".into())
                .spawn(move || {
                    let notes = crate::vault::note_paths(&folder);
                    if sender.send(notes).is_ok() {
                        ctx.request_repaint_of(viewport);
                    }
                }) {
                Ok(_) => self.listing = Some(receiver),
                Err(_) => {
                    self.error = Some(
                        tr!(
                            "Could not list the vault. Tabs and recent files are still available.",
                            "Не удалось прочитать хранилище. Вкладки и недавние файлы доступны."
                        )
                        .to_owned(),
                    )
                }
            }
        }
    }

    fn add(&mut self, paths: impl IntoIterator<Item = PathBuf>, source: Source) {
        for path in paths {
            let lower_path = path_key(&path);
            if !self.keys.insert(lower_path.clone()) {
                continue;
            }
            let name = path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned();
            self.entries.push(Entry {
                path,
                lower_name: name.to_lowercase(),
                name,
                lower_path,
                source,
            });
        }
    }

    fn filter(&mut self) {
        let query = self.query.replace('\\', "/").to_lowercase();
        let terms: Vec<_> = query.split_whitespace().collect();
        let mut matches: Vec<_> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let score = if terms.is_empty() || entry.lower_name.starts_with(query.trim()) {
                    0
                } else if terms.iter().all(|term| entry.lower_name.contains(term)) {
                    1
                } else if terms.iter().all(|term| entry.lower_path.contains(term)) {
                    2
                } else {
                    return None;
                };
                Some((score, entry.source, index))
            })
            .collect();
        // Preserve tab and recent-file order when there is no query. For a
        // query, names win over folder matches; open tabs win tied matches.
        matches.sort_unstable();
        self.matches = matches
            .into_iter()
            .take(50)
            .map(|(_, _, index)| index)
            .collect();
        self.selected = 0;
    }

    fn close(&mut self) {
        *self = Self::default();
    }

    pub fn show(&mut self, ctx: &Context) -> Option<PathBuf> {
        if !self.open {
            return None;
        }
        if let Some(receiver) = &self.listing {
            match receiver.try_recv() {
                Ok(paths) => {
                    // Keep the selected path when asynchronous results arrive.
                    let selected = self
                        .matches
                        .get(self.selected)
                        .map(|&i| self.entries[i].path.clone());
                    self.add(paths, Source::Vault);
                    self.filter();
                    if let Some(selected) = selected {
                        self.selected = self
                            .matches
                            .iter()
                            .position(|&i| self.entries[i].path == selected)
                            .unwrap_or(0);
                    }
                    self.listing = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.listing = None;
                    self.error = Some(
                        tr!(
                            "Could not list the vault.",
                            "Не удалось прочитать хранилище."
                        )
                        .to_owned(),
                    );
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        let (up, down, enter) = ctx.input_mut(|input| {
            (
                input.consume_key(Modifiers::NONE, Key::ArrowUp),
                input.consume_key(Modifiers::NONE, Key::ArrowDown),
                input.consume_key(Modifiers::NONE, Key::Enter),
            )
        });
        if !self.matches.is_empty() {
            if down {
                self.selected = (self.selected + 1) % self.matches.len();
            }
            if up {
                self.selected = (self.selected + self.matches.len() - 1) % self.matches.len();
            }
        }
        let mut chosen = None;
        let modal = egui::Modal::new(egui::Id::new("quick-open")).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 48.0).clamp(180.0, 560.0));
            ui.heading(tr!("Quick open", "Быстрый переход"));
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .id_salt("quick-open-query")
                    .hint_text(tr!("File name or path…", "Имя файла или путь…"))
                    .desired_width(f32::INFINITY),
            );
            if std::mem::take(&mut self.focus) {
                response.request_focus();
            }
            let filter_changed = response.changed();
            if filter_changed {
                self.filter();
            }
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .id_salt("quick-open-results")
                .max_height((ctx.content_rect().height() - 190.0).clamp(80.0, 360.0))
                .show(ui, |ui| {
                    if self.matches.is_empty() {
                        ui.weak(tr!("No matching documents", "Подходящих документов нет"));
                    }
                    for (position, &index) in self.matches.iter().enumerate() {
                        let entry = &self.entries[index];
                        let source = match entry.source {
                            Source::Open => tr!("Open", "Открыт"),
                            Source::Recent => tr!("Recent", "Недавний"),
                            Source::Vault => tr!("Vault", "Хранилище"),
                        };
                        let label = format!(
                            "{}  ·  {source}\n{}",
                            entry.name,
                            entry.path.parent().unwrap_or(Path::new("")).display()
                        );
                        let response = ui.add_sized(
                            [ui.available_width(), 44.0],
                            egui::Button::new(label)
                                .selected(position == self.selected)
                                .wrap(),
                        );
                        if (up || down || filter_changed) && position == self.selected {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }
                        if response.clicked() {
                            chosen = Some(entry.path.clone());
                        }
                    }
                });
            if self.listing.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak(tr!(
                        "Looking for notes in the vault…",
                        "Поиск заметок в хранилище…"
                    ));
                });
            }
            if let Some(error) = &self.error {
                ui.weak(error);
            }
            ui.weak(tr!(
                "↑ ↓ Select   Enter Open   Esc Close",
                "↑ ↓ Выбрать   Enter Открыть   Esc Закрыть"
            ));
        });
        if enter {
            chosen = self
                .matches
                .get(self.selected)
                .map(|&i| self.entries[i].path.clone());
        }
        if chosen.is_some() || modal.should_close() {
            self.close();
        }
        chosen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_are_deduplicated_and_rank_names_before_folders() {
        let mut dialog = QuickOpen::default();
        dialog.add([PathBuf::from("C:/notes/Мир.md")], Source::Open);
        dialog.add([PathBuf::from("c:\\notes\\МИР.md")], Source::Recent);
        dialog.add(
            [
                PathBuf::from("C:/notes/мир/other.md"),
                PathBuf::from("C:/notes/Привет мир.md"),
            ],
            Source::Vault,
        );
        assert_eq!(dialog.entries.len(), 3);
        dialog.query = "МИР".into();
        dialog.filter();
        let names: Vec<_> = dialog
            .matches
            .iter()
            .map(|&i| dialog.entries[i].name.as_str())
            .collect();
        assert_eq!(names, ["Мир.md", "Привет мир.md", "other.md"]);
        dialog.query = "notes привет".into();
        dialog.filter();
        assert_eq!(dialog.matches.len(), 1);
        dialog.query = "missing".into();
        dialog.filter();
        assert!(dialog.matches.is_empty());
    }

    fn render(ctx: &Context, dialog: &mut QuickOpen, events: Vec<egui::Event>) -> Option<PathBuf> {
        let mut chosen = None;
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                events,
                ..Default::default()
            },
            |_| chosen = dialog.show(ctx),
        )
        .drop_without_applying_deltas();
        chosen
    }

    fn key(key: Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    #[test]
    fn keyboard_filters_selects_and_cancels_without_opening_a_file() {
        let ctx = Context::default();
        let paths = [
            PathBuf::from("C:/notes/first.md"),
            PathBuf::from("C:/notes/second.md"),
        ];
        let mut dialog = QuickOpen::default();
        dialog.start(paths.clone(), &[], None, &ctx);
        render(&ctx, &mut dialog, vec![]);
        let chosen = render(
            &ctx,
            &mut dialog,
            vec![key(Key::ArrowDown), key(Key::Enter)],
        );
        assert_eq!(chosen, Some(paths[1].clone()));
        assert!(!dialog.open);
        dialog.start(paths.clone(), &[], None, &ctx);
        render(&ctx, &mut dialog, vec![]);
        render(&ctx, &mut dialog, vec![egui::Event::Text("second".into())]);
        assert_eq!(dialog.matches.len(), 1);
        assert!(render(&ctx, &mut dialog, vec![key(Key::Escape)]).is_none());
        assert!(!dialog.open);
    }

    #[test]
    fn asynchronous_notes_keep_the_keyboard_selection() {
        let ctx = Context::default();
        let paths = [PathBuf::from("C:/first.md"), PathBuf::from("C:/second.md")];
        let mut dialog = QuickOpen::default();
        dialog.start(paths.clone(), &[], None, &ctx);
        render(&ctx, &mut dialog, vec![]);
        render(&ctx, &mut dialog, vec![key(Key::ArrowDown)]);
        let (sender, receiver) = channel();
        dialog.listing = Some(receiver);
        sender
            .send(vec![PathBuf::from("C:/vault/third.md")])
            .unwrap();
        let chosen = render(&ctx, &mut dialog, vec![key(Key::Enter)]);
        assert_eq!(chosen, Some(paths[1].clone()));
    }

    #[test]
    fn clicking_the_backdrop_cancels_without_opening_a_document() {
        let ctx = Context::default();
        let mut dialog = QuickOpen::default();
        dialog.start([PathBuf::from("C:/notes/first.md")], &[], None, &ctx);
        // The first Area pass only measures it; click after it is visible.
        render(&ctx, &mut dialog, vec![]);
        render(&ctx, &mut dialog, vec![]);
        let pos = egui::pos2(2.0, 2.0);
        for pressed in [true, false] {
            assert!(
                render(
                    &ctx,
                    &mut dialog,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: Modifiers::NONE
                        },
                    ]
                )
                .is_none()
            );
        }
        assert!(!dialog.open);
    }
}
