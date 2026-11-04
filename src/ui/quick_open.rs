//! Quick navigation among tabs, recent documents and the current vault
//! (Ctrl+K). Nothing here runs before the reader opens the dialog: the
//! vault is listed and the recent files are checked on threads of their
//! own then.
//!
//! A query is matched fuzzily, as in VS Code or Obsidian: each word of it
//! must appear in order, not necessarily together, in the document's name
//! (preferred) or in its folder and name. Letters at the start of a word
//! and letters in a row score higher; the matched ones are highlighted.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Color32, Context, FontId, Key, Modifiers, Rect, Sense, Ui, pos2, vec2};

use crate::style::Palette;

/// How many results are listed; the rest are only counted.
const LIMIT: usize = 100;
const ROW_HEIGHT: f32 = 42.0;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Source {
    Open,
    Recent,
    Vault,
}

/// What the reader chose in the dialog.
#[derive(Debug, PartialEq)]
pub enum Chosen {
    /// Open the document in this window, or switch to its tab.
    Open(PathBuf),
    /// Open the document in a window of its own (Ctrl+Enter, Ctrl+click).
    NewWindow(PathBuf),
    /// Take the document off the recent files (Shift+Delete or its cross);
    /// the dialog stays open.
    Forget(PathBuf),
}

/// What the worker threads find after the dialog has opened.
enum Found {
    /// Recent files that no longer exist.
    Missing(Vec<PathBuf>),
    /// The vault of the current document, if it is in one.
    Root(Option<PathBuf>),
    /// The vault's documents.
    Notes(Vec<PathBuf>),
}

struct Entry {
    path: PathBuf,
    source: Source,
    /// Listed in the vault too, so it stays when taken off the recent files.
    in_vault: bool,
    /// The file name without `.md`, as Obsidian shows notes.
    name: Vec<char>,
    /// Its folder: in the vault from the vault's own folder, else in full.
    folder: Vec<char>,
    /// `folder`, `/` and `name`, matched when a word is not in the name.
    full: Vec<char>,
    /// `name` and `full` folded char by char (`fold`), so that indices into
    /// them are indices into the text shown.
    lower_name: Vec<char>,
    lower_full: Vec<char>,
}

impl Entry {
    fn new(path: PathBuf, source: Source, root: Option<&Path>) -> Entry {
        let mut entry = Entry {
            path,
            source,
            in_vault: source == Source::Vault,
            name: Vec::new(),
            folder: Vec::new(),
            full: Vec::new(),
            lower_name: Vec::new(),
            lower_full: Vec::new(),
        };
        entry.describe(root);
        entry
    }

    fn describe(&mut self, root: Option<&Path>) {
        self.name = display_name(&self.path).chars().collect();
        self.folder = display_folder(&self.path, root).chars().collect();
        self.full = self.folder.clone();
        if !self.full.is_empty() {
            self.full.push('/');
        }
        self.full.extend(&self.name);
        self.lower_name = self.name.iter().map(|&c| fold(c)).collect();
        self.lower_full = self.full.iter().map(|&c| fold(c)).collect();
    }
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

/// One char lowercased to one char, so that positions stay put; `\` is `/`.
fn fold(c: char) -> char {
    if c == '\\' {
        '/'
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

fn display_name(path: &Path) -> String {
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    let cut = name.len().saturating_sub(3);
    if cut > 0 && name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(".md") {
        name[..cut].to_owned()
    } else {
        name.into_owned()
    }
}

/// The folder of `path`; inside the vault at `root` from the vault's own
/// folder (`Vault/Notes`), which says where in it a note is.
fn display_folder(path: &Path, root: Option<&Path>) -> String {
    let parent = path.parent().unwrap_or(Path::new(""));
    if let Some(root) = root
        && let Ok(relative) = parent.strip_prefix(root)
    {
        let mut folder = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        for part in relative.components() {
            folder.push('/');
            folder.push_str(&part.as_os_str().to_string_lossy());
        }
        return folder;
    }
    parent.display().to_string()
}

// Scores of the fuzzy match, after fzf's: every matched letter gets
// `MATCH`, one at the start of a word more, the first letter of the query
// twice that; letters in a row get `CONSECUTIVE`, skipped letters cost.
const MATCH: i32 = 16;
const BOUNDARY: i32 = 8;
const CAMEL: i32 = 6;
const CONSECUTIVE: i32 = 5;
const GAP_START: i32 = 3;
const GAP_EXTEND: i32 = 1;
/// For a word found in the name rather than only in the path.
const NAME_BONUS: i32 = 24;
const NONE: i32 = i32::MIN / 2;

/// The tables of `fuzzy`, kept between calls.
#[derive(Default)]
struct Scratch {
    score: Vec<i32>,
    from: Vec<u32>,
}

fn bonus(text: &[char], j: usize) -> i32 {
    let Some(prev) = j.checked_sub(1).map(|p| text[p]) else {
        return BOUNDARY + 2;
    };
    let cur = text[j];
    if matches!(
        prev,
        '/' | '\\' | ' ' | '-' | '_' | '.' | ',' | '(' | '[' | '+'
    ) {
        BOUNDARY
    } else if (prev.is_lowercase() && cur.is_uppercase())
        || (!prev.is_numeric() && cur.is_numeric())
    {
        CAMEL
    } else {
        0
    }
}

/// The best way to find `query` (folded) in `text`, `lower` being `text`
/// folded: its score and the indices of the chars it takes, or `None` when
/// `query` is not a subsequence of it. `score[i][j]` is the best score of
/// the first `i + 1` chars of the query with the last at `j`.
fn fuzzy(
    query: &[char],
    text: &[char],
    lower: &[char],
    scratch: &mut Scratch,
) -> Option<(i32, Vec<usize>)> {
    let (m, n) = (query.len(), lower.len());
    if m == 0 {
        return Some((0, Vec::new()));
    }
    let mut rest = lower.iter();
    if !query.iter().all(|q| rest.any(|c| c == q)) {
        return None;
    }
    let Scratch { score, from } = scratch;
    score.clear();
    score.resize(m * n, NONE);
    from.clear();
    from.resize(m * n, u32::MAX);
    for (i, &letter) in query.iter().enumerate() {
        let row = i * n;
        // The best previous letter at least two chars back, less the gap.
        let (mut gap, mut gap_from) = (NONE, u32::MAX);
        for j in i..n {
            if i > 0 && j >= 2 {
                gap -= GAP_EXTEND;
                let skipped = score[row - n + j - 2] - GAP_START;
                if skipped > gap {
                    gap = skipped;
                    gap_from = (j - 2) as u32;
                }
            }
            if lower[j] != letter {
                continue;
            }
            let own = MATCH + bonus(text, j) * if i == 0 { 2 } else { 1 };
            if i == 0 {
                score[row + j] = own;
                continue;
            }
            let diagonal = score[row - n + j - 1] + CONSECUTIVE;
            let (best, prev) = if diagonal >= gap {
                (diagonal, (j - 1) as u32)
            } else {
                (gap, gap_from)
            };
            if best > NONE / 2 {
                score[row + j] = best + own;
                from[row + j] = prev;
            }
        }
    }
    let last = (m - 1) * n;
    let (mut end, mut best) = (0, NONE);
    for j in m - 1..n {
        if score[last + j] > best {
            (end, best) = (j, score[last + j]);
        }
    }
    if best <= NONE / 2 {
        return None;
    }
    let mut hits = vec![0; m];
    for i in (0..m).rev() {
        hits[i] = end;
        if i > 0 {
            end = from[i * n + end] as usize;
        }
    }
    Some((best, hits))
}

struct Match {
    entry: usize,
    score: i32,
    name_hits: Vec<usize>,
    folder_hits: Vec<usize>,
}

/// How `entry` matches every word of the query, if it does.
fn score(index: usize, entry: &Entry, terms: &[Vec<char>], scratch: &mut Scratch) -> Option<Match> {
    let mut found = Match {
        entry: index,
        score: 0,
        name_hits: Vec::new(),
        folder_hits: Vec::new(),
    };
    let name_start = entry.full.len() - entry.name.len();
    for term in terms {
        if let Some((score, hits)) = fuzzy(term, &entry.name, &entry.lower_name, scratch) {
            found.score += score + NAME_BONUS;
            found.name_hits.extend(hits);
        } else {
            let (score, hits) = fuzzy(term, &entry.full, &entry.lower_full, scratch)?;
            found.score += score;
            for hit in hits {
                if hit >= name_start {
                    found.name_hits.push(hit - name_start);
                } else if hit < entry.folder.len() {
                    found.folder_hits.push(hit);
                }
            }
        }
    }
    Some(found)
}

/// The query's words, folded; `.md` at the end of one is dropped, since
/// names are shown without it.
fn terms(query: &str) -> Vec<Vec<char>> {
    query
        .split_whitespace()
        .map(|word| {
            let mut term: Vec<char> = word.chars().map(fold).collect();
            if term.len() > 3 && term.ends_with(&['.', 'm', 'd']) {
                term.truncate(term.len() - 3);
            }
            term
        })
        .collect()
}

/// Presses of `key` with exactly `modifiers`, taken out of the input so
/// that the query field does not see them (Home and End move in the list,
/// Shift+Home still selects in the field).
fn take_key(input: &mut egui::InputState, modifiers: Modifiers, key: Key) -> usize {
    let mut count = 0;
    input.events.retain(|event| {
        let hit = matches!(
            event,
            egui::Event::Key { key: k, pressed: true, modifiers: m, .. }
                if *k == key && m.matches_exact(modifiers)
        );
        count += usize::from(hit);
        !hit
    });
    count
}

#[derive(Default)]
struct Keys {
    up: usize,
    down: usize,
    page_up: usize,
    page_down: usize,
    home: bool,
    end: bool,
    enter: bool,
    new_window: bool,
    forget: bool,
}

#[derive(Default)]
pub struct QuickOpen {
    pub open: bool,
    query: String,
    entries: Vec<Entry>,
    keys: HashSet<String>,
    /// The vault of the document shown when the dialog opened.
    root: Option<PathBuf>,
    matches: Vec<Match>,
    /// Matches past `LIMIT`, not listed.
    more: usize,
    selected: usize,
    /// Scroll the selected row into view.
    reveal: bool,
    focus: bool,
    found: Option<Receiver<Found>>,
    /// The vault is being listed.
    listing: bool,
    error: Option<String>,
    scratch: Scratch,
}

impl QuickOpen {
    /// Opens the dialog with `tabs` (in the order to list them), `recent`
    /// and, when `folder` is in a vault, the vault's documents once listed.
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
        let (sender, receiver) = channel();
        let recent = recent.to_vec();
        let mut started = spawn("quick-open-recent", sender.clone(), ctx, move |send| {
            send(Found::Missing(
                recent.into_iter().filter(|path| !path.exists()).collect(),
            ));
        });
        if let Some(folder) = folder.map(Path::to_owned) {
            started &= spawn("quick-open-vault", sender, ctx, move |send| {
                let root = crate::vault::root_of(&folder);
                let in_vault = root.is_some();
                send(Found::Root(root));
                if in_vault {
                    send(Found::Notes(crate::vault::note_paths(&folder)));
                }
            });
        }
        self.found = Some(receiver);
        if !started {
            self.error = Some(
                tr!(
                    "Could not list the vault. Tabs and recent files are still available.",
                    "Не удалось прочитать хранилище. Вкладки и недавние файлы доступны."
                )
                .to_owned(),
            );
        }
    }

    /// Moves the selection one row down (`forward`) or up, round the ends:
    /// Ctrl+K and Ctrl+Shift+K while the dialog is open.
    pub fn step(&mut self, forward: bool) {
        let n = self.matches.len();
        if n > 0 {
            self.selected = if forward {
                (self.selected + 1) % n
            } else {
                (self.selected + n - 1) % n
            };
            self.reveal = true;
        }
    }

    fn add(&mut self, paths: impl IntoIterator<Item = PathBuf>, source: Source) {
        for path in paths {
            if self.keys.insert(path_key(&path)) {
                self.entries
                    .push(Entry::new(path, source, self.root.as_deref()));
            }
        }
    }

    fn filter(&mut self) {
        let terms = terms(&self.query);
        let entries = &self.entries;
        let mut matches: Vec<Match> = entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| score(index, entry, &terms, &mut self.scratch))
            .collect();
        // Without a query the order is the one given: tabs read last first,
        // recent files, the vault by path. With one, the best matches come
        // first; of equal ones open tabs, then the shorter path.
        if !terms.is_empty() {
            matches.sort_by(|a, b| {
                let (ea, eb) = (&entries[a.entry], &entries[b.entry]);
                b.score
                    .cmp(&a.score)
                    .then(ea.source.cmp(&eb.source))
                    .then(ea.full.len().cmp(&eb.full.len()))
                    .then(a.entry.cmp(&b.entry))
            });
        }
        self.more = matches.len().saturating_sub(LIMIT);
        matches.truncate(LIMIT);
        self.matches = matches;
        self.selected = 0;
        self.reveal = true;
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.matches
            .get(self.selected)
            .map(|m| &self.entries[m.entry])
    }

    /// Filters again after the entries changed, keeping the row selected.
    fn refilter(&mut self) {
        let selected = self.selected_entry().map(|e| e.path.clone());
        let row = self.selected;
        self.filter();
        self.selected = match selected {
            Some(path) => self
                .matches
                .iter()
                .position(|m| self.entries[m.entry].path == path)
                .unwrap_or(row.min(self.matches.len().saturating_sub(1))),
            None => 0,
        };
    }

    /// Takes in what the worker threads found.
    fn poll(&mut self) {
        let Some(receiver) = self.found.take() else {
            return;
        };
        let mut changed = false;
        loop {
            match receiver.try_recv() {
                Ok(Found::Missing(paths)) => {
                    let gone: HashSet<String> = paths.iter().map(|p| path_key(p)).collect();
                    self.entries.retain(|e| {
                        e.source != Source::Recent || !gone.contains(&path_key(&e.path))
                    });
                    changed = true;
                }
                Ok(Found::Root(root)) => {
                    self.listing = root.is_some();
                    if root.is_some() {
                        self.root = root;
                        for entry in &mut self.entries {
                            entry.describe(self.root.as_deref());
                        }
                        changed = true;
                    }
                }
                Ok(Found::Notes(paths)) => {
                    self.listing = false;
                    self.add_notes(paths);
                    changed = true;
                }
                Err(TryRecvError::Empty) => {
                    self.found = Some(receiver);
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    if std::mem::take(&mut self.listing) {
                        self.error = Some(
                            tr!(
                                "Could not list the vault.",
                                "Не удалось прочитать хранилище."
                            )
                            .to_owned(),
                        );
                    }
                    break;
                }
            }
        }
        if changed {
            self.refilter();
        }
    }

    fn add_notes(&mut self, paths: Vec<PathBuf>) {
        let known: HashMap<String, usize> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (path_key(&e.path), i))
            .collect();
        let first = self.entries.len();
        for path in paths {
            let key = path_key(&path);
            if let Some(&i) = known.get(&key) {
                self.entries[i].in_vault = true;
            } else if self.keys.insert(key) {
                self.entries
                    .push(Entry::new(path, Source::Vault, self.root.as_deref()));
            }
        }
        self.entries[first..].sort_by(|a, b| a.lower_full.cmp(&b.lower_full));
    }

    /// Takes `path` off the list after it was taken off the recent files;
    /// a note of the vault stays as one.
    fn forget(&mut self, path: &Path) {
        let Some(i) = self.entries.iter().position(|e| e.path == path) else {
            return;
        };
        if self.entries[i].in_vault {
            self.entries[i].source = Source::Vault;
        } else {
            self.entries.remove(i);
            self.keys.remove(&path_key(path));
        }
        self.refilter();
    }

    fn close(&mut self) {
        *self = Self::default();
    }

    fn list_height(ctx: &Context) -> f32 {
        (ctx.content_rect().height() - 170.0).clamp(ROW_HEIGHT * 2.0, 440.0)
    }

    pub fn show(&mut self, ctx: &Context, palette: &Palette) -> Option<Chosen> {
        if !self.open {
            return None;
        }
        self.poll();
        let page = ((Self::list_height(ctx) / ROW_HEIGHT) as usize).max(1);
        let keys = ctx.input_mut(|input| Keys {
            up: take_key(input, Modifiers::NONE, Key::ArrowUp),
            down: take_key(input, Modifiers::NONE, Key::ArrowDown),
            page_up: take_key(input, Modifiers::NONE, Key::PageUp),
            page_down: take_key(input, Modifiers::NONE, Key::PageDown),
            home: take_key(input, Modifiers::NONE, Key::Home) > 0,
            end: take_key(input, Modifiers::NONE, Key::End) > 0,
            enter: take_key(input, Modifiers::NONE, Key::Enter) > 0,
            new_window: take_key(input, Modifiers::COMMAND, Key::Enter) > 0,
            forget: take_key(input, Modifiers::SHIFT, Key::Delete) > 0,
        });
        let n = self.matches.len();
        if n > 0 {
            for _ in 0..keys.down {
                self.step(true);
            }
            for _ in 0..keys.up {
                self.step(false);
            }
            let last = n - 1;
            let mut to = None;
            if keys.page_down > 0 {
                to = Some((self.selected + page * keys.page_down).min(last));
            }
            if keys.page_up > 0 {
                to = Some(self.selected.saturating_sub(page * keys.page_up));
            }
            if keys.home {
                to = Some(0);
            }
            if keys.end {
                to = Some(last);
            }
            if let Some(to) = to {
                self.selected = to;
                self.reveal = true;
            }
        }
        let mut chosen = None;
        if let Some(entry) = self.selected_entry() {
            if keys.new_window {
                chosen = Some(Chosen::NewWindow(entry.path.clone()));
            } else if keys.enter {
                chosen = Some(Chosen::Open(entry.path.clone()));
            } else if keys.forget && entry.source == Source::Recent {
                chosen = Some(Chosen::Forget(entry.path.clone()));
            }
        }
        let width = (ctx.content_rect().width() - 48.0).clamp(180.0, 620.0);
        let modal = egui::Modal::new(egui::Id::new("quick-open")).show(ctx, |ui| {
            ui.set_width(width);
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .id_salt("quick-open-query")
                    .hint_text(tr!(
                        "Go to a document: name or folder…",
                        "Перейти к документу: имя или папка…"
                    ))
                    .margin(egui::Margin::symmetric(8, 6))
                    .desired_width(f32::INFINITY),
            );
            if std::mem::take(&mut self.focus) {
                response.request_focus();
            }
            if response.changed() {
                self.filter();
            }
            ui.add_space(6.0);
            let moved = ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
            egui::ScrollArea::vertical()
                .id_salt("quick-open-results")
                .max_height(Self::list_height(ctx))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    if self.matches.is_empty() {
                        ui.add_space(8.0);
                        ui.weak(if self.entries.is_empty() && self.listing {
                            tr!("Looking for documents…", "Поиск документов…")
                        } else {
                            tr!("No matching documents", "Подходящих документов нет")
                        });
                        ui.add_space(8.0);
                    }
                    let mut hovered = None;
                    for position in 0..self.matches.len() {
                        let selected = position == self.selected;
                        match self.row(ui, position, selected, palette) {
                            Row::Hovered if moved => hovered = Some(position),
                            Row::Clicked(new_window) => {
                                let path = self.entries[self.matches[position].entry].path.clone();
                                chosen = Some(if new_window {
                                    Chosen::NewWindow(path)
                                } else {
                                    Chosen::Open(path)
                                });
                            }
                            Row::Forget => {
                                let path = self.entries[self.matches[position].entry].path.clone();
                                chosen = Some(Chosen::Forget(path));
                                self.focus = true;
                            }
                            _ => {}
                        }
                    }
                    if let Some(position) = hovered {
                        self.selected = position;
                    }
                    if self.more > 0 {
                        ui.add_space(6.0);
                        ui.weak(format!(
                            "{} {} — {}",
                            tr!("More:", "Ещё:"),
                            self.more,
                            tr!("refine the search", "уточните запрос")
                        ));
                    }
                });
            self.reveal = false;
            if self.listing {
                ui.add_space(4.0);
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
            ui.add_space(6.0);
            self.hints(ui, palette);
        });
        match &chosen {
            Some(Chosen::Forget(path)) => self.forget(path),
            Some(_) => self.close(),
            None if modal.should_close() => self.close(),
            None => {}
        }
        chosen
    }

    fn hints(&self, ui: &mut Ui, palette: &Palette) {
        let mut hints = vec![
            ("↑↓", tr!("select", "выбрать")),
            ("Enter", tr!("open", "открыть")),
            ("Ctrl+Enter", tr!("in a new window", "в новом окне")),
        ];
        if self
            .selected_entry()
            .is_some_and(|e| e.source == Source::Recent)
        {
            hints.push(("Shift+Del", tr!("forget", "убрать из недавних")));
        }
        hints.push(("Esc", tr!("close", "закрыть")));
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(14.0, 2.0);
            let font = FontId::proportional(11.0);
            for (keys, what) in hints {
                let mut job = LayoutJob::default();
                job.append(keys, 0.0, TextFormat::simple(font.clone(), palette.text));
                job.append(what, 4.0, TextFormat::simple(font.clone(), palette.muted));
                ui.label(job);
            }
        });
    }

    /// One result: the name over its folder, matched letters highlighted,
    /// and where it comes from on the right; a recent file under the pointer
    /// has a cross there that takes it off the recent files.
    fn row(&self, ui: &mut Ui, position: usize, selected: bool, palette: &Palette) -> Row {
        let found = &self.matches[position];
        let entry = &self.entries[found.entry];
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
        if selected && self.reveal {
            response.scroll_to_me(None);
        }
        if !ui.is_rect_visible(rect) {
            return Row::Idle;
        }
        let under_pointer = ui.rect_contains_pointer(rect);
        let painter = ui.painter();
        if selected {
            painter.rect_filled(
                rect,
                4.0,
                ui.visuals().selection.bg_fill.gamma_multiply(0.6),
            );
            painter.rect_filled(
                Rect::from_min_size(rect.min + vec2(0.0, 6.0), vec2(3.0, rect.height() - 12.0)),
                1.5,
                palette.accent,
            );
        } else if under_pointer {
            painter.rect_filled(rect, 4.0, ui.visuals().widgets.hovered.weak_bg_fill);
        }
        let inner = rect.shrink2(vec2(12.0, 4.0));

        let mut row = if response.clicked() {
            Row::Clicked(ui.input(|i| i.modifiers.command))
        } else if under_pointer {
            Row::Hovered
        } else {
            Row::Idle
        };
        // The source on the right, or the cross of a recent file.
        let side = if entry.source == Source::Recent && under_pointer {
            let cross = Rect::from_center_size(
                pos2(inner.right() - 10.0, rect.center().y),
                vec2(22.0, 22.0),
            );
            let response = ui
                .interact(
                    cross,
                    ui.id().with(("quick-open-forget", found.entry)),
                    Sense::click(),
                )
                .on_hover_text(tr!(
                    "Remove from recent files (Shift+Delete)",
                    "Убрать из недавних (Shift+Delete)"
                ));
            let visuals = ui.style().interact(&response);
            if response.hovered() {
                ui.painter().rect_filled(cross, 3.0, visuals.bg_fill);
            }
            let (c, r) = (cross.center(), 4.0);
            let stroke = egui::Stroke::new(1.3, visuals.fg_stroke.color);
            ui.painter()
                .line_segment([c + vec2(-r, -r), c + vec2(r, r)], stroke);
            ui.painter()
                .line_segment([c + vec2(-r, r), c + vec2(r, -r)], stroke);
            if response.clicked() {
                row = Row::Forget;
            }
            cross.width()
        } else {
            let (label, color) = match entry.source {
                Source::Open => (tr!("Tab", "Вкладка"), palette.accent),
                Source::Recent => (tr!("Recent", "Недавний"), palette.muted),
                Source::Vault => (tr!("Vault", "Хранилище"), palette.muted),
            };
            let galley =
                ui.painter()
                    .layout_no_wrap(label.to_owned(), FontId::proportional(11.0), color);
            let badge = Rect::from_min_size(
                pos2(
                    inner.right() - galley.size().x - 12.0,
                    rect.center().y - 9.0,
                ),
                vec2(galley.size().x + 12.0, 18.0),
            );
            ui.painter().rect_filled(badge, 9.0, palette.code_bg);
            ui.painter()
                .galley(badge.center() - galley.size() / 2.0, galley, color);
            badge.width()
        };
        let text_width = (inner.width() - side - 10.0).max(20.0);
        let name = highlighted(
            &entry.name,
            &found.name_hits,
            FontId::proportional(14.0),
            palette.text,
            palette.accent,
            text_width,
        );
        let folder = highlighted(
            &entry.folder,
            &found.folder_hits,
            FontId::proportional(11.5),
            palette.muted,
            palette.accent,
            text_width,
        );
        let (name, folder) = ui.fonts_mut(|f| (f.layout_job(name), f.layout_job(folder)));
        let painter = ui.painter();
        painter.galley(inner.left_top() + vec2(0.0, 1.0), name, palette.text);
        painter.galley(
            pos2(inner.left(), inner.top() + 20.0),
            folder,
            palette.muted,
        );
        row
    }
}

enum Row {
    Idle,
    Hovered,
    /// With Ctrl held.
    Clicked(bool),
    Forget,
}

/// `text` on one line of `width`, cut with an ellipsis, the chars at
/// `hits` in `hit` colour.
fn highlighted(
    text: &[char],
    hits: &[usize],
    font: FontId,
    color: Color32,
    hit: Color32,
    width: f32,
) -> LayoutJob {
    let mut marked = vec![false; text.len()];
    for &i in hits {
        if let Some(mark) = marked.get_mut(i) {
            *mark = true;
        }
    }
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: width,
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        },
        ..Default::default()
    };
    let mut start = 0;
    while start < text.len() {
        let on = marked[start];
        let end = (start..text.len())
            .find(|&i| marked[i] != on)
            .unwrap_or(text.len());
        let piece: String = text[start..end].iter().collect();
        let format = TextFormat::simple(font.clone(), if on { hit } else { color });
        job.append(&piece, 0.0, format);
        start = end;
    }
    job
}

/// Runs `work` on a thread named `name`; whatever it sends repaints the
/// viewport of `ctx`. False when the thread could not be started.
fn spawn(
    name: &str,
    sender: Sender<Found>,
    ctx: &Context,
    work: impl FnOnce(&dyn Fn(Found)) + Send + 'static,
) -> bool {
    let ctx = ctx.clone();
    let viewport = ctx.viewport_id();
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            work(&|found| {
                if sender.send(found).is_ok() {
                    ctx.request_repaint_of(viewport);
                }
            })
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(dialog: &QuickOpen) -> Vec<String> {
        dialog
            .matches
            .iter()
            .map(|m| dialog.entries[m.entry].name.iter().collect())
            .collect()
    }

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    fn hits(query: &str, text: &str) -> Option<Vec<usize>> {
        let text = chars(text);
        let lower: Vec<char> = text.iter().map(|&c| fold(c)).collect();
        fuzzy(&chars(query), &text, &lower, &mut Scratch::default()).map(|(_, hits)| hits)
    }

    #[test]
    fn fuzzy_match_prefers_word_starts_and_runs() {
        assert_eq!(
            hits("mdsyn", "markdown-syntax"),
            Some(vec![0, 4, 9, 10, 11])
        );
        // "op" starting "open", not the one inside "top".
        assert_eq!(hits("op", "top_open"), Some(vec![4, 5]));
        assert_eq!(hits("qo", "quick_open"), Some(vec![0, 6]));
        assert_eq!(hits("rm", "ReadMe"), Some(vec![0, 4]));
        assert_eq!(hits("cba", "abc"), None);
    }

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
        assert_eq!(names(&dialog), ["Мир", "Привет мир", "other"]);
        dialog.query = "notes привет".into();
        dialog.filter();
        assert_eq!(names(&dialog), ["Привет мир"]);
        dialog.query = "привет.md".into();
        dialog.filter();
        assert_eq!(names(&dialog), ["Привет мир"]);
        dialog.query = "missing".into();
        dialog.filter();
        assert!(dialog.matches.is_empty());
    }

    #[test]
    fn a_close_match_beats_a_scattered_one_and_hits_are_shown() {
        let mut dialog = QuickOpen::default();
        dialog.add(
            [
                PathBuf::from("C:/a/other pen notes.md"),
                PathBuf::from("C:/a/open.md"),
            ],
            Source::Vault,
        );
        dialog.query = "open".into();
        dialog.filter();
        assert_eq!(names(&dialog), ["open", "other pen notes"]);
        assert_eq!(dialog.matches[0].name_hits, [0, 1, 2, 3]);
        // A word found only in the folder is highlighted there.
        dialog.query = "a pen".into();
        dialog.filter();
        let found = &dialog.matches[0];
        assert_eq!(found.folder_hits.len() + found.name_hits.len(), 4);
    }

    #[test]
    fn notes_are_shown_from_the_vault_folder_without_md() {
        let root = Path::new("C:/Vault");
        let entry = Entry::new(
            PathBuf::from("C:/Vault/Notes/Daily/Today.md"),
            Source::Vault,
            Some(root),
        );
        assert_eq!(entry.name.iter().collect::<String>(), "Today");
        assert_eq!(entry.folder.iter().collect::<String>(), "Vault/Notes/Daily");
        let outside = Entry::new(
            PathBuf::from("D:/docs/readme.markdown"),
            Source::Recent,
            Some(root),
        );
        assert_eq!(outside.name.iter().collect::<String>(), "readme.markdown");
        assert_eq!(
            outside.folder.iter().collect::<String>(),
            Path::new("D:/docs").display().to_string()
        );
    }

    fn palette() -> Palette {
        Palette::new(false, crate::settings::DarkStyle::Grey)
    }

    fn render(ctx: &Context, dialog: &mut QuickOpen, events: Vec<egui::Event>) -> Option<Chosen> {
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
            |_| chosen = dialog.show(ctx, &palette()),
        )
        .drop_without_applying_deltas();
        chosen
    }

    fn key_with(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn key(key: Key) -> egui::Event {
        key_with(key, Modifiers::NONE)
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
        assert_eq!(chosen, Some(Chosen::Open(paths[1].clone())));
        assert!(!dialog.open);
        dialog.start(paths.clone(), &[], None, &ctx);
        render(&ctx, &mut dialog, vec![]);
        render(&ctx, &mut dialog, vec![egui::Event::Text("second".into())]);
        assert_eq!(dialog.matches.len(), 1);
        assert!(render(&ctx, &mut dialog, vec![key(Key::Escape)]).is_none());
        assert!(!dialog.open);
    }

    #[test]
    fn list_keys_move_and_ctrl_enter_asks_for_a_new_window() {
        let ctx = Context::default();
        let paths: Vec<_> = (0..30)
            .map(|i| PathBuf::from(format!("C:/notes/{i:02}.md")))
            .collect();
        let mut dialog = QuickOpen::default();
        dialog.start(paths.clone(), &[], None, &ctx);
        render(&ctx, &mut dialog, vec![]);
        render(&ctx, &mut dialog, vec![key(Key::End)]);
        assert_eq!(dialog.selected, 29);
        render(&ctx, &mut dialog, vec![key(Key::PageUp)]);
        assert!(dialog.selected < 29 && dialog.selected > 0);
        render(&ctx, &mut dialog, vec![key(Key::Home)]);
        assert_eq!(dialog.selected, 0);
        // Wrapping round the top, as Ctrl+Shift+K does.
        dialog.step(false);
        assert_eq!(dialog.selected, 29);
        let ctrl = Modifiers::CTRL | Modifiers::COMMAND;
        let chosen = render(&ctx, &mut dialog, vec![key_with(Key::Enter, ctrl)]);
        assert_eq!(chosen, Some(Chosen::NewWindow(paths[29].clone())));
        assert!(!dialog.open);
    }

    #[test]
    fn missing_recent_files_are_hidden_and_forgotten_ones_dropped() {
        let ctx = Context::default();
        let mut dialog = QuickOpen::default();
        let tab = PathBuf::from("C:/notes/tab.md");
        let gone = PathBuf::from("C:/notes/gone.md");
        let kept = PathBuf::from("C:/notes/kept.md");
        let note = PathBuf::from("C:/notes/note.md");
        dialog.start([tab.clone()], &[], None, &ctx);
        dialog.add([gone.clone(), kept.clone(), note.clone()], Source::Recent);
        dialog.filter();
        let (sender, receiver) = channel();
        dialog.found = Some(receiver);
        sender.send(Found::Missing(vec![gone, tab])).unwrap();
        sender.send(Found::Notes(vec![note.clone()])).unwrap();
        render(&ctx, &mut dialog, vec![]);
        // An open tab stays even when its file is gone.
        assert_eq!(names(&dialog), ["tab", "kept", "note"]);
        render(&ctx, &mut dialog, vec![key(Key::ArrowDown)]);
        let chosen = render(
            &ctx,
            &mut dialog,
            vec![key_with(Key::Delete, Modifiers::SHIFT)],
        );
        assert_eq!(chosen, Some(Chosen::Forget(kept)));
        assert!(dialog.open);
        assert_eq!(names(&dialog), ["tab", "note"]);
        assert_eq!(dialog.selected, 1);
        // A note of the vault stays, as one.
        render(
            &ctx,
            &mut dialog,
            vec![key_with(Key::Delete, Modifiers::SHIFT)],
        );
        assert_eq!(names(&dialog), ["tab", "note"]);
        assert_eq!(
            dialog.entries[dialog.matches[1].entry].source,
            Source::Vault
        );
        // Shift+Delete on a tab does nothing.
        render(&ctx, &mut dialog, vec![key(Key::Home)]);
        assert!(
            render(
                &ctx,
                &mut dialog,
                vec![key_with(Key::Delete, Modifiers::SHIFT)]
            )
            .is_none()
        );
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
        dialog.found = Some(receiver);
        sender
            .send(Found::Notes(vec![PathBuf::from("C:/vault/third.md")]))
            .unwrap();
        let chosen = render(&ctx, &mut dialog, vec![key(Key::Enter)]);
        assert_eq!(chosen, Some(Chosen::Open(paths[1].clone())));
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
