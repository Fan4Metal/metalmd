//! The Licenses window (from About, as metalmpv's and metalamp's): the
//! program's own licence, the notices of the code ported from Markpad and
//! egui, of the Rust crates and fonts compiled into the exe, and of the
//! syntax definitions and colour themes two-face embeds. The texts are
//! embedded, so the exe carries them wherever it goes; the installer and
//! the portable archive ship them as files too. Long lines are wrapped at
//! words to the text's width beforehand ([`wrap`]): the lines are shown
//! with `show_rows`, which wants rows of one height.

use std::borrow::Cow;

use egui::{FontId, Key, Rect, RichText, Sense, pos2, vec2};

const METALMD: &str = include_str!("../../LICENSE");
/// The code ported from Markpad and egui.
const PORTED: &str = include_str!("../../THIRD_PARTY_LICENSES.md");
/// Written by tools/notices.py.
pub const NOTICES: &str = include_str!("../../licenses/THIRD_PARTY_NOTICES.txt");
/// Written by tools/notices.py through `tests::write_syntaxes_and_themes`.
pub const SYNTAXES: &str = include_str!("../../licenses/SYNTAXES_AND_THEMES.txt");

/// The window's content size in points.
const SIZE: egui::Vec2 = vec2(860.0, 540.0);
/// The width of the list on the left.
const LIST: f32 = 200.0;
const ROW: f32 = 15.0;
/// The font of the texts.
const FONT: f32 = 12.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Part {
    #[default]
    Program,
    Ported,
    Crates,
    Syntaxes,
}

impl Part {
    const ALL: [Part; 4] = [Part::Program, Part::Ported, Part::Crates, Part::Syntaxes];

    fn title(self) -> &'static str {
        match self {
            Part::Program => tr!("MetalMD: MIT", "MetalMD: MIT"),
            Part::Ported => tr!("Markpad and egui code", "Код Markpad и egui"),
            Part::Crates => tr!("Libraries and fonts", "Библиотеки и шрифты"),
            Part::Syntaxes => tr!("Syntaxes and themes", "Синтаксисы и темы"),
        }
    }

    fn text(self) -> &'static str {
        match self {
            Part::Program => METALMD,
            Part::Ported => PORTED,
            Part::Crates => NOTICES,
            Part::Syntaxes => SYNTAXES,
        }
    }
}

#[derive(Default)]
pub struct LicensesView {
    part: Part,
    /// The lines of the part shown, wrapped to `columns`.
    lines: Vec<Cow<'static, str>>,
    columns: usize,
}

impl LicensesView {
    /// Draws the window; whether it is to close (the cross, Esc, a click
    /// outside).
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let modal = egui::Modal::new(egui::Id::new("licenses")).show(ctx, |ui| {
            let size = super::modal_size(ctx, SIZE, 24.0);
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            let visuals = ui.visuals().clone();
            let list = Rect::from_min_size(rect.min, vec2(LIST, rect.height()));
            let mut list_ui = ui.new_child(egui::UiBuilder::new().max_rect(list));
            list_ui.label(
                RichText::new(tr!("Licenses", "Лицензии"))
                    .size(18.0)
                    .strong()
                    .color(visuals.strong_text_color()),
            );
            list_ui.add_space(8.0);
            for part in Part::ALL {
                if list_ui
                    .selectable_label(self.part == part, part.title())
                    .clicked()
                    && self.part != part
                {
                    self.part = part;
                    self.lines.clear();
                }
            }
            list_ui.add_space(12.0);
            list_ui.label(
                RichText::new(tr!(
                    "MetalMD is under the MIT License, without any warranty. The code ported from Markpad and egui, the libraries compiled into the program, the fonts and the syntax definitions keep their own licences, reproduced here.",
                    "MetalMD распространяется под лицензией MIT, без каких-либо гарантий. Код, перенесённый из Markpad и egui, библиотеки, вошедшие в программу, шрифты и определения синтаксисов сохраняют свои лицензии, приведённые здесь."
                ))
                .size(12.0)
                .color(visuals.weak_text_color()),
            );

            let text = Rect::from_min_max(pos2(list.right() + 12.0, rect.top() + 28.0), rect.max);
            let cross = Rect::from_min_size(pos2(rect.right() - 24.0, rect.top()), vec2(24.0, 24.0));
            let closed = super::close_cross(&mut ui.new_child(egui::UiBuilder::new().max_rect(cross)));
            ui.painter().rect(
                text,
                4.0,
                visuals.extreme_bg_color,
                visuals.widgets.noninteractive.bg_stroke,
                egui::StrokeKind::Inside,
            );
            let inner = text.shrink(6.0);
            // As many characters as fit beside the scroll bar.
            let font = FontId::monospace(FONT);
            let glyph = ui.fonts_mut(|f| f.glyph_width(&font, 'M'));
            let room = inner.width() - ui.spacing().scroll.bar_width - 8.0;
            let columns = ((room / glyph).floor() as usize).max(20);
            if self.lines.is_empty() || columns != self.columns {
                self.columns = columns;
                self.split();
            }
            let mut text_ui = ui.new_child(egui::UiBuilder::new().max_rect(inner));
            egui::ScrollArea::vertical()
                .id_salt(("licenses_text", self.part as u8))
                .auto_shrink([false, false])
                .show_rows(&mut text_ui, ROW, self.lines.len(), |ui, rows| {
                    for line in &self.lines[rows] {
                        ui.add(
                            egui::Label::new(
                                RichText::new(line.as_ref())
                                    .font(font.clone())
                                    .color(visuals.text_color()),
                            )
                            .extend(),
                        );
                    }
                });
            closed
        });
        modal.inner || modal.should_close() || ctx.input(|i| i.key_pressed(Key::Escape))
    }

    fn split(&mut self) {
        self.lines.clear();
        for line in self.part.text().lines() {
            wrap(line.trim_end_matches('\r'), self.columns, &mut self.lines);
        }
    }
}

/// `line` in pieces of at most `columns` characters, broken after spaces
/// where it can be (else inside a word); the pieces after the first keep
/// the line's indent.
fn wrap(line: &'static str, columns: usize, out: &mut Vec<Cow<'static, str>>) {
    if line.chars().count() <= columns {
        out.push(Cow::Borrowed(line));
        return;
    }
    let indent = line
        .chars()
        .take_while(|c| *c == ' ')
        .count()
        .min(columns / 2);
    let mut rest = line;
    let mut first = true;
    while !rest.is_empty() {
        let room = if first { columns } else { columns - indent };
        // The byte where the piece of `room` characters ends.
        let end = rest.char_indices().nth(room).map_or(rest.len(), |(i, _)| i);
        let cut = if end == rest.len() {
            end
        } else {
            // After the last space, if it leaves something on the line.
            match rest[..end].rfind(' ') {
                Some(space) if !rest[..space].trim().is_empty() => space + 1,
                _ => end,
            }
        };
        let piece = rest[..cut].trim_end();
        out.push(if first {
            Cow::Borrowed(piece)
        } else {
            Cow::Owned(format!("{}{piece}", " ".repeat(indent)))
        });
        rest = rest[cut..].trim_start_matches(' ');
        first = false;
    }
}

/// `licenses/SYNTAXES_AND_THEMES.txt` as it should be for the two-face
/// compiled in: its listing of the licences of the syntax definitions and
/// colour themes, as plain text.
#[cfg(test)]
fn syntaxes_and_themes() -> String {
    use std::fmt::Write;

    let listing = two_face::acknowledgement::listing();
    let mut out = String::new();
    writeln!(out, "Syntax definitions and colour themes used by MetalMD").unwrap();
    writeln!(out, "{}", "=".repeat(78)).unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "Code is highlighted with the syntax definitions and colour themes that the"
    )
    .unwrap();
    writeln!(
        out,
        "two-face crate embeds, collected by the bat project from Sublime Text"
    )
    .unwrap();
    writeln!(
        out,
        "packages. They have licences of their own, reproduced below as two-face"
    )
    .unwrap();
    writeln!(
        out,
        "lists them (two_face::acknowledgement::listing()). The complete listing of"
    )
    .unwrap();
    writeln!(out, "this version of two-face is at").unwrap();
    writeln!(out, "{}", two_face::acknowledgement::url()).unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "Written by tools/notices.py (ui::licenses::tests::write_syntaxes_and_themes)."
    )
    .unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "Most of the code for generating both theme and syntax dumps along with the"
    )
    .unwrap();
    writeln!(
        out,
        "curation of said themes and syntaxes is taken from the bat project,"
    )
    .unwrap();
    writeln!(out, "https://github.com/sharkdp/bat.").unwrap();
    for (title, licenses) in [
        ("Syntaxes", listing.for_syntaxes()),
        ("Themes", listing.for_themes()),
    ] {
        writeln!(out).unwrap();
        writeln!(out, "{}", "=".repeat(78)).unwrap();
        writeln!(out, "{title}").unwrap();
        writeln!(out, "{}", "=".repeat(78)).unwrap();
        for license in licenses {
            let path = license.rel_path.to_string_lossy().replace('\\', "/");
            writeln!(out).unwrap();
            writeln!(out, "--- {path} ---").unwrap();
            for line in license.text.trim().lines() {
                writeln!(out, "{}", line.trim_end()).unwrap();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tools/notices.py's hash of Cargo.lock: without carriage returns and
    /// without this package's own version (a release changes it).
    fn lock_hash(lock: &str) -> String {
        let lock = lock.replace('\r', "");
        let lines: Vec<&str> = lock.split('\n').collect();
        let kept: Vec<&str> = lines
            .iter()
            .enumerate()
            .filter(|&(i, line)| {
                !(i > 0 && lines[i - 1] == "name = \"metalmd\"" && line.starts_with("version = "))
            })
            .map(|(_, line)| *line)
            .collect();
        let mut h: u64 = 0xCBF2_9CE4_8422_2325;
        for b in kept.join("\n").bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
        format!("{h:016x}")
    }

    #[test]
    fn notices_follow_cargo_lock() {
        let lock =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.lock")).unwrap();
        let hash = lock_hash(&lock);
        assert!(
            NOTICES.contains(&format!("Cargo.lock FNV-1a 64: {hash}")),
            "licenses/THIRD_PARTY_NOTICES.txt is older than Cargo.lock: run python tools/notices.py"
        );
    }

    #[test]
    fn own_version_leaves_the_hash() {
        let lock = |v: &str| format!("[[package]]\r\nname = \"metalmd\"\r\nversion = \"{v}\"\r\n");
        assert_eq!(lock_hash(&lock("0.1.0-dev")), lock_hash(&lock("0.1.0")));
        let other = |v: &str| format!("[[package]]\nname = \"egui\"\nversion = \"{v}\"\n");
        assert_ne!(lock_hash(&other("0.36.1")), lock_hash(&other("0.36.2")));
    }

    #[test]
    fn syntaxes_follow_two_face() {
        assert!(
            SYNTAXES.replace('\r', "") == syntaxes_and_themes(),
            "licenses/SYNTAXES_AND_THEMES.txt differs from two-face's listing: run python tools/notices.py"
        );
    }

    /// Writes `licenses/SYNTAXES_AND_THEMES.txt`; tools/notices.py runs it.
    #[test]
    #[ignore]
    fn write_syntaxes_and_themes() {
        std::fs::write(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/licenses/SYNTAXES_AND_THEMES.txt"
            ),
            syntaxes_and_themes(),
        )
        .unwrap();
    }

    #[test]
    fn long_lines_wrap_at_words() {
        let mut out = Vec::new();
        wrap("short", 10, &mut out);
        wrap("  one two three four", 10, &mut out);
        wrap("abcdefghijklmnop", 6, &mut out);
        assert_eq!(
            out,
            [
                "short",
                "  one two",
                "  three",
                "  four",
                "abcdef",
                "ghijkl",
                "mnop"
            ]
        );
        // Every line of the texts fits.
        let mut all = Vec::new();
        for text in [METALMD, PORTED, NOTICES, SYNTAXES] {
            for line in text.lines() {
                wrap(line.trim_end_matches('\r'), 80, &mut all);
            }
        }
        assert!(all.iter().all(|l| l.chars().count() <= 80));
    }

    #[test]
    fn texts_are_there() {
        assert!(METALMD.contains("MIT License"));
        assert!(PORTED.contains("Markpad") && PORTED.contains("BSD 3-Clause"));
        assert!(NOTICES.contains("egui") && NOTICES.contains("OFL"));
        assert!(NOTICES.contains("KaTeX") && NOTICES.contains("WebView2"));
        assert!(SYNTAXES.contains("Syntaxes") && SYNTAXES.contains("Themes"));
    }
}
