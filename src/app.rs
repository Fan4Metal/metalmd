//! The application: tabs, background loading, input and panels.

use crate::bench::Bench;
use crate::fs_text;
use crate::markdown::document::{Block, Document, Scripts};
use crate::markdown::highlight::Highlighter;
use crate::markdown::preprocess::{heading_anchor_id, is_markdown_path, percent_decode};
use crate::settings::{Settings, ThemeSetting, TocSide, same_path};
use crate::single_instance::Server;
use crate::style::{self, BODY_SIZE, FontChoice, Palette};
use crate::ui::about::About;
use crate::ui::diagram::DiagramRenderer;
use crate::ui::emoji::EmojiRenderer;
use crate::ui::find::FindState;
use crate::ui::home::{self, HomeAction};
use crate::ui::math::MathRenderer;
use crate::ui::toc;
use crate::ui::viewer::Pictures;
use crate::ui::viewer::{self, RenderCtx, ViewAction, ViewState};
use crate::watcher::FileWatcher;
use egui::{
    Align, CentralPanel, Color32, FontData, FontDefinitions, Frame, Id, Key, KeyboardShortcut,
    Layout, Margin, Modifiers, Panel, RichText, Sense, TextEdit, Theme, Ui, ViewportCommand,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Lets background threads ask for a repaint once the window exists.
pub type CtxCell = Arc<OnceLock<egui::Context>>;

pub fn request_repaint(cell: &CtxCell) {
    if let Some(ctx) = cell.get() {
        ctx.request_repaint();
    }
}

/// A document read and parsed off the UI thread.
pub struct Loaded {
    pub doc: Arc<Document>,
    pub encoding: String,
    pub lossy: bool,
}

pub enum AppMsg {
    Loaded {
        tab: u64,
        generation: u64,
        result: Result<Loaded, LoadError>,
    },
    OpenPaths(Vec<PathBuf>),
    FileChanged(PathBuf),
    Fonts(Vec<(String, FontData)>),
}

#[derive(Debug, Clone)]
pub struct LoadError {
    pub message: String,
    pub not_found: bool,
}

pub fn load_file(path: &Path) -> Result<Loaded, LoadError> {
    let decoded = fs_text::read_to_string_lossy(path).map_err(|e| LoadError {
        message: e.to_string(),
        not_found: e.kind() == std::io::ErrorKind::NotFound,
    })?;
    let doc = Document::parse(&decoded.content);
    Ok(Loaded {
        doc: Arc::new(doc),
        encoding: decoded.encoding,
        lossy: decoded.lossy,
    })
}

pub fn spawn_load(sender: Sender<AppMsg>, cell: CtxCell, tab: u64, generation: u64, path: PathBuf) {
    std::thread::Builder::new()
        .name("load".into())
        // comrak and the document builder recurse once per nesting level.
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let result = load_file(&path);
            let _ = sender.send(AppMsg::Loaded {
                tab,
                generation,
                result,
            });
            request_repaint(&cell);
        })
        .expect("spawn loader thread");
}

/// Absolute, symlink-resolved path without Windows' `\\?\` prefix.
pub fn normalize_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let canonical = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    strip_verbatim(canonical)
}

fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path
    }
}

struct Tab {
    id: u64,
    path: PathBuf,
    title: String,
    doc: Option<Arc<Document>>,
    error: Option<String>,
    loading: bool,
    generation: u64,
    encoding: String,
    lossy: bool,
    deleted: bool,
    view: ViewState,
    /// Anchor to scroll to once the document has loaded.
    pending_anchor: Option<String>,
    /// Reading position to restore after a reload.
    restore: Option<(usize, f32)>,
    /// Positions to return to with Alt+Left.
    back: Vec<(usize, f32)>,
}

impl Tab {
    fn new(id: u64, path: PathBuf) -> Tab {
        let title = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Tab {
            id,
            path,
            title,
            doc: None,
            error: None,
            loading: true,
            generation: 0,
            encoding: String::new(),
            lossy: false,
            deleted: false,
            view: ViewState::default(),
            pending_anchor: None,
            restore: None,
            back: Vec::new(),
        }
    }
}

/// What `main` prepared before the window existed.
pub struct Startup {
    /// Tabs whose loading has already started, with their ids.
    pub tabs: Vec<(u64, PathBuf)>,
    pub sender: Sender<AppMsg>,
    pub receiver: Receiver<AppMsg>,
    pub ctx_cell: CtxCell,
    pub settings: Settings,
    pub server: Option<Server>,
    pub bench: Option<Bench>,
}

pub struct App {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    sender: Sender<AppMsg>,
    receiver: Receiver<AppMsg>,
    ctx_cell: CtxCell,
    settings: Settings,
    settings_dirty: bool,
    highlighter: Highlighter,
    pictures: Pictures,
    base_fonts: FontDefinitions,
    fonts: FontChoice,
    fonts_generation: u64,
    fallback_scripts: Scripts,
    find: FindState,
    find_jump: bool,
    watcher: Option<FileWatcher>,
    frame: u64,
    bench: Option<Bench>,
    title: String,
    about: About,
    start_icon: Option<egui::TextureHandle>,
    /// The window, for its caption (see `follow_theme`).
    hwnd: Option<isize>,
    /// The theme of the last frame.
    shown_theme: Option<Theme>,
    /// Repaint the window caption on this frame.
    refresh_caption: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, startup: Startup) -> App {
        let ctx = cc.egui_ctx.clone();
        let _ = startup.ctx_cell.set(ctx.clone());
        let mut bench = startup.bench;
        if let Some(b) = bench.as_mut() {
            b.mark("app_new");
        }

        style::apply_visuals(&ctx);
        ctx.set_theme(startup.settings.theme.preference());
        // The window is still hidden (eframe shows it after the first
        // frame), so a caption set now is shown from the start; set after
        // egui's first frame, Windows 10 would keep the caption of the
        // Windows theme. Following Windows, winit has already done it.
        let hwnd = crate::caption::hwnd(cc);
        if let Some(hwnd) = hwnd {
            crate::placement::keep_on_screen(hwnd);
            match startup.settings.theme {
                ThemeSetting::Dark => crate::caption::set_dark(hwnd, true),
                ThemeSetting::Light => crate::caption::set_dark(hwnd, false),
                ThemeSetting::System => {}
            }
        }
        ctx.set_zoom_factor(startup.settings.zoom);
        // A wheel notch scrolls 100 points instead of egui's 40, as far as
        // Chromium scrolls with the default three lines per notch.
        ctx.options_mut(|o| o.input_options.line_scroll_speed = 100.0);
        let (base_fonts, fonts) = style::load_fonts();
        ctx.set_fonts(base_fonts.clone());
        egui_extras::install_image_loaders(&ctx);
        crate::ui::animation::ApngLoader::install(&ctx);
        crate::ui::svg::SvgLoader::install(&ctx);
        // Last: it stands in front of the loaders above.
        crate::ui::limit::LimitLoader::install(&ctx);
        ctx.add_plugin(crate::ui::copy::CopyPlugin);
        if let Some(b) = bench.as_mut() {
            b.mark("fonts_ready");
        }

        if let Some(server) = startup.server {
            let sender = startup.sender.clone();
            let cell = startup.ctx_cell.clone();
            server.spawn(move |paths| {
                let _ = sender.send(AppMsg::OpenPaths(paths));
                request_repaint(&cell);
            });
        }

        let highlighter = {
            let cell = startup.ctx_cell.clone();
            Highlighter::new(move || request_repaint(&cell))
        };
        let pictures = {
            let math_cell = startup.ctx_cell.clone();
            let emoji_cell = startup.ctx_cell.clone();
            let diagram_cell = startup.ctx_cell.clone();
            Pictures {
                math: MathRenderer::new(move || request_repaint(&math_cell)),
                emoji: EmojiRenderer::new(move || request_repaint(&emoji_cell)),
                diagrams: DiagramRenderer::new(move || request_repaint(&diagram_cell)),
            }
        };

        let next_id = startup
            .tabs
            .iter()
            .map(|(id, _)| *id + 1)
            .max()
            .unwrap_or(1);
        let tabs: Vec<Tab> = startup
            .tabs
            .into_iter()
            .map(|(id, path)| Tab::new(id, path))
            .collect();
        let active = tabs.len().saturating_sub(1);

        App {
            tabs,
            active,
            next_id,
            sender: startup.sender,
            receiver: startup.receiver,
            ctx_cell: startup.ctx_cell,
            settings: startup.settings,
            settings_dirty: false,
            highlighter,
            pictures,
            base_fonts,
            fonts,
            fonts_generation: 0,
            fallback_scripts: Scripts::default(),
            find: FindState::default(),
            find_jump: false,
            watcher: None,
            frame: 0,
            bench,
            title: String::new(),
            about: About::default(),
            start_icon: None,
            hwnd,
            shown_theme: None,
            refresh_caption: false,
        }
    }

    /// Bring the window caption in line with the theme. egui passes a new
    /// theme to the window at the end of the frame it changed in, and
    /// Windows 10 shows it in the caption only at the next repaint of the
    /// frame, so the frame after the change sets the caption and asks for
    /// that repaint. This also covers Windows switching its theme while
    /// the app follows it.
    fn follow_theme(&mut self, ctx: &egui::Context) {
        let theme = ctx.theme();
        if std::mem::take(&mut self.refresh_caption)
            && let Some(hwnd) = self.hwnd
        {
            crate::caption::set_dark(hwnd, theme == Theme::Dark);
            crate::caption::refresh(hwnd);
        }
        if self.shown_theme.is_some_and(|t| t != theme) {
            self.refresh_caption = true;
            ctx.request_repaint();
        }
        self.shown_theme = Some(theme);
    }

    fn open_paths(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>, focus_window: bool) {
        for path in paths {
            let (path, anchor) = (normalize_path(&path), None);
            self.open_path(path, anchor);
        }
        if focus_window {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
    }

    /// Opens `path` in a tab (or activates its tab) and scrolls to `anchor`.
    fn open_path(&mut self, path: PathBuf, anchor: Option<String>) {
        if let Some(index) = self.tabs.iter().position(|t| same_path(&t.path, &path)) {
            self.active = index;
            if let Some(anchor) = anchor {
                let tab = &mut self.tabs[index];
                match tab
                    .doc
                    .as_ref()
                    .and_then(|doc| resolve_anchor(doc, &anchor))
                {
                    Some(block) => tab.view.jump_to(block, 8.0),
                    None => tab.pending_anchor = Some(anchor),
                }
            }
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        let mut tab = Tab::new(id, path.clone());
        tab.pending_anchor = anchor;
        spawn_load(
            self.sender.clone(),
            self.ctx_cell.clone(),
            id,
            0,
            path.clone(),
        );
        if let Some(watcher) = self.watcher.as_mut() {
            watcher.watch(&path);
        }
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(index);
        if let Some(watcher) = self.watcher.as_mut() {
            watcher.unwatch(&tab.path);
        }
        if self.active > index || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.find.invalidate();
    }

    fn reload(&mut self, index: usize) {
        let tab = &mut self.tabs[index];
        tab.generation += 1;
        tab.loading = tab.doc.is_none();
        if tab.restore.is_none() {
            tab.restore = tab.view.reading_position();
        }
        spawn_load(
            self.sender.clone(),
            self.ctx_cell.clone(),
            tab.id,
            tab.generation,
            tab.path.clone(),
        );
    }

    fn process_messages(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.receiver.try_recv() {
            self.handle_message(ctx, message);
        }
    }

    fn handle_message(&mut self, ctx: &egui::Context, message: AppMsg) {
        match message {
            AppMsg::Loaded {
                tab,
                generation,
                result,
            } => {
                let Some(index) = self.tabs.iter().position(|t| t.id == tab) else {
                    return;
                };
                if self.tabs[index].generation != generation {
                    return;
                }
                let tab = &mut self.tabs[index];
                tab.loading = false;
                match result {
                    Ok(loaded) => {
                        let scripts = loaded.doc.scripts;
                        let has_code = loaded.doc.has_code;
                        if let Some(anchor) = tab.pending_anchor.take() {
                            if let Some(block) = resolve_anchor(&loaded.doc, &anchor) {
                                tab.view.jump_to(block, 8.0);
                            }
                        } else if let Some((block, offset)) = tab.restore.take() {
                            let last = loaded.doc.blocks.len().saturating_sub(1);
                            tab.view.jump_to(block.min(last), offset);
                        }
                        tab.doc = Some(loaded.doc);
                        tab.encoding = loaded.encoding;
                        tab.lossy = loaded.lossy;
                        tab.error = None;
                        tab.deleted = false;
                        let path = tab.path.clone();
                        self.settings.add_recent(&path);
                        self.settings_dirty = true;
                        self.find.invalidate();
                        self.request_fallback_fonts(scripts);
                        if has_code && self.frame > 1 {
                            self.highlighter.warm_up();
                        }
                        if let Some(bench) = self.bench.as_mut() {
                            bench.mark_once("doc_ready");
                        }
                    }
                    Err(error) => {
                        if tab.doc.is_some() {
                            tab.deleted = error.not_found;
                        } else {
                            tab.error = Some(error.message);
                            if error.not_found {
                                let path = tab.path.clone();
                                self.settings.remove_recent(&path);
                                self.settings_dirty = true;
                            }
                        }
                    }
                }
            }
            AppMsg::OpenPaths(paths) => self.open_paths(ctx, paths, true),
            AppMsg::FileChanged(path) => {
                let indices: Vec<usize> = self
                    .tabs
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| same_path(&t.path, &path))
                    .map(|(i, _)| i)
                    .collect();
                for index in indices {
                    self.reload(index);
                }
            }
            AppMsg::Fonts(extra) => {
                if !extra.is_empty() {
                    self.base_fonts = style::with_fallbacks(self.base_fonts.clone(), &extra);
                    ctx.set_fonts(self.base_fonts.clone());
                    self.fonts_generation += 1;
                }
            }
        }
    }

    /// Loads fonts for scripts egui's bundled fonts lack, once per script.
    fn request_fallback_fonts(&mut self, scripts: Scripts) {
        let wanted = Scripts {
            han_or_kana: scripts.han_or_kana && !self.fallback_scripts.han_or_kana,
            hangul: scripts.hangul && !self.fallback_scripts.hangul,
        };
        if wanted == Scripts::default() {
            return;
        }
        self.fallback_scripts.han_or_kana |= wanted.han_or_kana;
        self.fallback_scripts.hangul |= wanted.hangul;
        let sender = self.sender.clone();
        let cell = self.ctx_cell.clone();
        std::thread::spawn(move || {
            let fonts = style::load_cjk_fonts(wanted);
            let _ = sender.send(AppMsg::Fonts(fonts));
            request_repaint(&cell);
        });
    }

    /// The first frame waits briefly for the document so the window opens
    /// with it rather than empty. Loading started before the window did.
    fn wait_for_first_document(&mut self, ctx: &egui::Context) {
        let deadline = Instant::now() + Duration::from_millis(1500);
        while self.tabs.get(self.active).is_some_and(|t| t.loading) {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.receiver.recv_timeout(left) {
                Ok(message) => self.handle_message(ctx, message),
                Err(_) => break,
            }
        }
    }

    fn start_background_services(&mut self) {
        let sender = self.sender.clone();
        let cell = self.ctx_cell.clone();
        if let Ok(mut watcher) = FileWatcher::new(move |path| {
            let _ = sender.send(AppMsg::FileChanged(path));
            request_repaint(&cell);
        }) {
            for tab in &self.tabs {
                watcher.watch(&tab.path);
            }
            self.watcher = Some(watcher);
        }
        if self
            .tabs
            .iter()
            .any(|t| t.doc.as_ref().is_some_and(|d| d.has_code))
        {
            self.highlighter.warm_up();
        }
    }

    fn open_dialog(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("Markdown", &["md", "markdown", "mdown", "mkd", "txt"])
            .add_filter(tr!("All files", "Все файлы"), &["*"]);
        if let Some(dir) = self.tabs.get(self.active).and_then(|t| t.path.parent()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(paths) = dialog.pick_files() {
            self.open_paths(ctx, paths, false);
        }
    }

    fn handle_action(&mut self, ctx: &egui::Context, action: ViewAction) {
        let ViewAction::OpenLink(target) = action;
        let target = target.trim().to_owned();
        if let Some(fragment) = target.strip_prefix('#') {
            let Some(tab) = self.tabs.get_mut(self.active) else {
                return;
            };
            let Some(doc) = tab.doc.as_ref() else { return };
            if let Some(block) = resolve_anchor(doc, fragment) {
                if let Some(position) = tab.view.reading_position() {
                    tab.back.push(position);
                }
                tab.view.jump_to(block, 8.0);
            }
            return;
        }
        if has_url_scheme(&target) {
            let _ = open::that_detached(&target);
            return;
        }
        // A path relative to the current document.
        let (path_part, fragment) = match target.split_once('#') {
            Some((p, f)) => (p, Some(f.to_owned())),
            None => (target.as_str(), None),
        };
        let path_part = path_part.split('?').next().unwrap_or(path_part);
        let decoded = percent_decode(path_part);
        let base = self
            .tabs
            .get(self.active)
            .and_then(|t| t.path.parent().map(Path::to_path_buf));
        let candidate = Path::new(&decoded);
        let resolved = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else if let Some(base) = base {
            base.join(candidate)
        } else {
            return;
        };
        if is_markdown_path(&decoded) {
            self.open_path(normalize_path(&resolved), fragment);
            let _ = ctx;
        } else if resolved.exists() {
            let _ = open::that_detached(&resolved);
        }
    }

    fn go_back(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active)
            && let Some((block, offset)) = tab.back.pop()
        {
            tab.view.jump_to(block, offset);
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let command = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let command_shift = |key| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key);
        let plain = |key| KeyboardShortcut::new(Modifiers::NONE, key);
        let shift = |key| KeyboardShortcut::new(Modifiers::SHIFT, key);
        let consume = |shortcut: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&shortcut));

        if consume(plain(Key::F1)) {
            self.about.toggle();
        }
        // The dialog takes the keyboard (Enter and Esc close it).
        if self.about.open {
            return;
        }
        if consume(command(Key::F)) {
            if self.find.open {
                self.find.request_focus = true;
            } else {
                self.find.open();
            }
        }
        if consume(command(Key::O)) {
            self.open_dialog(ctx);
        }
        if consume(command(Key::W)) && !self.tabs.is_empty() {
            self.close_tab(self.active);
        }
        let tab_count = self.tabs.len();
        if tab_count > 0 {
            if consume(command_shift(Key::Tab)) || consume(command(Key::PageUp)) {
                self.active = (self.active + tab_count - 1) % tab_count;
                self.find.invalidate();
            }
            if consume(command(Key::Tab)) || consume(command(Key::PageDown)) {
                self.active = (self.active + 1) % tab_count;
                self.find.invalidate();
            }
        }
        if consume(command_shift(Key::O)) {
            self.settings.toc_visible = !self.settings.toc_visible;
            self.settings_dirty = true;
        }
        if (consume(plain(Key::F5)) || consume(command(Key::R))) && self.active < self.tabs.len() {
            self.reload(self.active);
        }
        if self.find.open {
            if consume(plain(Key::F3)) {
                self.find.step(true);
                self.find_jump = true;
            }
            if consume(shift(Key::F3)) {
                self.find.step(false);
                self.find_jump = true;
            }
            if consume(plain(Key::Escape)) {
                self.find.close();
            }
        }
        if consume(KeyboardShortcut::new(Modifiers::ALT, Key::ArrowLeft))
            || ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Extra1))
        {
            self.go_back();
        }

        // Reading keys, unless a text field has the keyboard.
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        // Chromium's steps: 87.5 % of the view for a page, 40 px for a line.
        let page = (tab.view.viewport_height * 0.875).max(40.0);
        let line = BODY_SIZE * 2.5;
        ctx.input_mut(|i| {
            let mut delta = 0.0;
            if i.consume_key(Modifiers::NONE, Key::PageDown)
                || i.consume_key(Modifiers::NONE, Key::Space)
            {
                delta -= page;
            }
            if i.consume_key(Modifiers::NONE, Key::PageUp)
                || i.consume_key(Modifiers::SHIFT, Key::Space)
            {
                delta += page;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                delta -= line;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                delta += line;
            }
            if i.consume_key(Modifiers::NONE, Key::Home)
                || i.consume_key(Modifiers::COMMAND, Key::Home)
            {
                delta = 1.0e7;
            }
            if i.consume_key(Modifiers::NONE, Key::End)
                || i.consume_key(Modifiers::COMMAND, Key::End)
            {
                delta = -1.0e7;
            }
            tab.view.scroll_delta += delta;
        });
    }

    fn tab_bar(&mut self, ui: &mut Ui, palette: &Palette) {
        let mut activate = None;
        let mut close = None;
        let mut open_dialog = false;
        Panel::top("tab-bar")
            .frame(
                Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .inner_margin(Margin::symmetric(8, 5)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    // File actions first, where opening one adds its tab.
                    if ui
                        .button(RichText::new(tr!("Open", "Открыть")).size(13.0))
                        .on_hover_text(tr!("Open file (Ctrl+O)", "Открыть файл (Ctrl+O)"))
                        .clicked()
                    {
                        open_dialog = true;
                    }
                    ui.separator();
                    for (i, tab) in self.tabs.iter().enumerate() {
                        let selected = i == self.active;
                        let mut title = RichText::new(&tab.title).size(13.5);
                        if tab.deleted {
                            title = title.strikethrough();
                        }
                        let response = ui
                            .selectable_label(selected, title)
                            .on_hover_text(tab.path.display().to_string());
                        if response.clicked() {
                            activate = Some(i);
                        }
                        if response.middle_clicked() {
                            close = Some(i);
                        }
                        let close_button =
                            egui::Button::new(RichText::new("×").size(14.0).color(palette.muted))
                                .frame(false)
                                .small();
                        if ui
                            .add(close_button)
                            .on_hover_text(tr!("Close (Ctrl+W)", "Закрыть (Ctrl+W)"))
                            .clicked()
                        {
                            close = Some(i);
                        }
                        ui.add_space(6.0);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        if crate::ui::about_button(ui).clicked() {
                            self.about.toggle();
                        }
                        let theme = self.settings.theme;
                        if crate::ui::theme_button(ui, theme).clicked() {
                            self.settings.theme = theme.next();
                            ui.ctx().set_theme(self.settings.theme.preference());
                            self.settings_dirty = true;
                        }
                        let toc = crate::ui::toc_button(ui, self.settings.toc_visible);
                        if toc.clicked() {
                            self.settings.toc_visible = !self.settings.toc_visible;
                            self.settings_dirty = true;
                        }
                        if crate::ui::toc_side_menu(&toc, &mut self.settings.toc_side) {
                            // Choosing a side shows the contents there.
                            self.settings.toc_visible = true;
                            self.settings_dirty = true;
                        }
                        // Left of the icons, so it doesn't split them up.
                        let zoom = ui.ctx().zoom_factor();
                        if (zoom - 1.0).abs() > 0.01
                            && ui
                                .button(RichText::new(format!("{:.0}%", zoom * 100.0)).size(13.0))
                                .on_hover_text(tr!(
                                    "Reset zoom (Ctrl+0)",
                                    "Сбросить масштаб (Ctrl+0)"
                                ))
                                .clicked()
                        {
                            ui.ctx().set_zoom_factor(1.0);
                        }
                        if let Some(tab) = self.tabs.get(self.active) {
                            let mut status = Vec::new();
                            if tab.deleted {
                                status.push(tr!("file deleted", "файл удалён").to_owned());
                            }
                            if tab.lossy {
                                status.push(
                                    tr!("decoding errors", "ошибки декодирования").to_owned(),
                                );
                            }
                            if !tab.encoding.is_empty() && tab.encoding != fs_text::UTF8_LABEL {
                                status.push(tab.encoding.clone());
                            }
                            if !status.is_empty() {
                                ui.label(
                                    RichText::new(status.join(" · "))
                                        .size(12.0)
                                        .color(palette.muted),
                                );
                            }
                        }
                    });
                });
            });
        if let Some(i) = activate
            && i != self.active
        {
            self.active = i;
            self.find.invalidate();
        }
        if let Some(i) = close {
            self.close_tab(i);
        }
        if open_dialog {
            let ctx = ui.ctx().clone();
            self.open_dialog(&ctx);
        }
    }

    fn find_bar(&mut self, ui: &mut Ui, palette: &Palette) {
        if !self.find.open {
            return;
        }
        let doc = self.tabs.get(self.active).and_then(|t| t.doc.clone());
        Panel::top("find-bar")
            .frame(
                Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .inner_margin(Margin::symmetric(10, 5)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let input = ui.add(
                        TextEdit::singleline(&mut self.find.query)
                            .id(Id::new("find-input"))
                            .hint_text(tr!("Find in document", "Найти в документе"))
                            .desired_width(280.0),
                    );
                    if self.find.request_focus {
                        input.request_focus();
                        self.find.request_focus = false;
                    }
                    if input.changed() {
                        self.find.current = 0;
                        self.find_jump = true;
                    }
                    if input.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        let forward = !ui.input(|i| i.modifiers.shift);
                        self.find.step(forward);
                        self.find_jump = true;
                        input.request_focus();
                    }
                    if let Some(doc) = doc.as_ref() {
                        self.find.refresh(doc);
                    }
                    let count = self.find.matches.len();
                    let status = if self.find.query.is_empty() {
                        String::new()
                    } else if count == 0 {
                        tr!("No results", "Не найдено").to_owned()
                    } else {
                        let current = self.find.current + 1;
                        tr!(
                            format!("{current} of {count}"),
                            format!("{current} из {count}")
                        )
                    };
                    ui.label(RichText::new(status).size(12.5).color(palette.muted));
                    if ui
                        .button("↑")
                        .on_hover_text(tr!("Previous (Shift+F3)", "Предыдущее (Shift+F3)"))
                        .clicked()
                    {
                        self.find.step(false);
                        self.find_jump = true;
                    }
                    if ui
                        .button("↓")
                        .on_hover_text(tr!("Next (F3, Enter)", "Следующее (F3, Enter)"))
                        .clicked()
                    {
                        self.find.step(true);
                        self.find_jump = true;
                    }
                    if ui
                        .button("×")
                        .on_hover_text(tr!("Close (Esc)", "Закрыть (Esc)"))
                        .clicked()
                    {
                        self.find.close();
                    }
                });
            });
    }

    /// Scrolls the active tab to the current search match.
    fn jump_to_find_match(&mut self) {
        if !std::mem::take(&mut self.find_jump) {
            return;
        }
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let Some(doc) = tab.doc.as_ref() else { return };
        self.find.refresh(doc);
        let Some(m) = self.find.current_match() else {
            return;
        };
        let mut offset = tab.view.viewport_height * 0.3;
        if let Some(Block::Code { id, code, .. }) = doc.blocks.get(m.block)
            && *id == m.id
        {
            let line = code[..m.range.start.min(code.len())].matches('\n').count();
            offset -= line as f32 * (BODY_SIZE * 0.875 * 1.45).round();
        }
        tab.view.jump_to(m.block, offset);
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match self.tabs.get(self.active) {
            Some(tab) => format!("{} — MetalMD", tab.title),
            None => "MetalMD".to_owned(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    fn remember_window(&mut self, ctx: &egui::Context) {
        // egui reports the window in zoomed points, and the next start
        // creates it before the zoom is set: kept unzoomed, it would shrink
        // by the zoom at each start.
        let zoom = ctx.zoom_factor();
        let geometry = ctx.input(|i| {
            let viewport = i.viewport();
            let outer = viewport.outer_rect?;
            let inner = viewport.inner_rect?;
            Some(crate::settings::WindowGeometry {
                x: outer.min.x * zoom,
                y: outer.min.y * zoom,
                width: inner.width() * zoom,
                height: inner.height() * zoom,
                maximized: viewport.maximized.unwrap_or(false),
            })
        });
        if let Some(mut geometry) = geometry {
            if geometry.maximized {
                // Keep the restored size, not the maximized one.
                if let Some(previous) = self.settings.window {
                    geometry.x = previous.x;
                    geometry.y = previous.y;
                    geometry.width = previous.width;
                    geometry.height = previous.height;
                }
            }
            if geometry.width > 100.0 && geometry.height > 100.0 {
                self.settings.window = Some(geometry);
            }
        }
    }

    fn save_settings_if_needed(&mut self) {
        if std::mem::take(&mut self.settings_dirty) {
            let settings = self.settings.clone();
            std::thread::spawn(move || settings.save());
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.frame += 1;
        if self.frame == 1 {
            if let Some(b) = self.bench.as_mut() {
                b.mark("first_frame_start");
            }
            // The fonts set in `new` are live now.
            self.fonts.measure_ascents(&ctx);
            self.wait_for_first_document(&ctx);
        }
        if self.frame == 2 {
            self.start_background_services();
        }
        self.process_messages(&ctx);

        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        if !dropped.is_empty() {
            self.open_paths(&ctx, dropped, false);
        }
        self.handle_shortcuts(&ctx);

        let palette = Palette::new(ctx.theme() == Theme::Dark);
        self.tab_bar(ui, &palette);
        self.find_bar(ui, &palette);
        self.jump_to_find_match();

        let active_doc = self.tabs.get(self.active).and_then(|t| t.doc.clone());
        if self.settings.toc_visible
            && let (Some(doc), Some(tab)) = (active_doc.as_ref(), self.tabs.get_mut(self.active))
            && !doc.headings.is_empty()
        {
            let panel = match self.settings.toc_side {
                TocSide::Left => Panel::left("toc"),
                TocSide::Right => Panel::right("toc"),
            };
            panel
                .resizable(true)
                .default_size(250.0)
                .size_range(140.0..=480.0)
                .frame(
                    Frame::new()
                        .fill(ui.visuals().panel_fill)
                        .inner_margin(Margin::symmetric(10, 4)),
                )
                .show(ui, |ui| {
                    // Right-clicking the panel outside the headings moves it.
                    let background =
                        ui.interact(ui.max_rect(), Id::new("toc-background"), Sense::click());
                    if crate::ui::toc_side_menu(&background, &mut self.settings.toc_side) {
                        self.settings_dirty = true;
                    }
                    if let Some(index) = toc::show(ui, doc, &tab.view, &palette) {
                        let block = doc.headings[index].block;
                        if let Some(position) = tab.view.reading_position() {
                            tab.back.push(position);
                        }
                        tab.view.jump_to(block, 8.0);
                    }
                });
        }

        let mut actions = Vec::new();
        let mut home_action = None;
        let mut retry = false;
        CentralPanel::no_frame()
            .frame(Frame::new().fill(ui.visuals().panel_fill))
            .show(ui, |ui| match self.tabs.get_mut(self.active) {
                None => {
                    home_action =
                        home::show(ui, &self.settings.recent, &palette, &mut self.start_icon)
                }
                Some(tab) => {
                    if let Some(doc) = tab.doc.clone() {
                        let base_dir = tab.path.parent().map(Path::to_path_buf);
                        let mut rc = RenderCtx::new(
                            &palette,
                            &self.fonts,
                            self.fonts_generation,
                            &self.highlighter,
                            &self.pictures,
                            base_dir.as_deref(),
                            &self.find,
                        );
                        viewer::show_document(
                            ui,
                            &doc,
                            &mut tab.view,
                            &mut rc,
                            Id::new(("doc", tab.id)),
                        );
                        actions = std::mem::take(&mut rc.actions);
                    } else if let Some(error) = &tab.error {
                        ui.vertical_centered(|ui| {
                            ui.add_space(80.0);
                            ui.label(
                                RichText::new(tr!(
                                    "Could not open the file",
                                    "Не удалось открыть файл"
                                ))
                                .size(18.0),
                            );
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(tab.path.display().to_string()).color(palette.muted),
                            );
                            ui.label(RichText::new(error).color(palette.muted));
                            ui.add_space(12.0);
                            retry = ui.button(tr!("Retry", "Повторить")).clicked();
                        });
                    } else {
                        ui.vertical_centered(|ui| {
                            ui.add_space(80.0);
                            ui.spinner();
                        });
                    }
                }
            });
        let lang = self.settings.language;
        self.about.show(&ctx, &mut self.settings.language);
        if self.settings.language != lang {
            self.settings_dirty = true;
        }
        for action in actions {
            self.handle_action(&ctx, action);
        }
        if retry {
            self.tabs[self.active].error = None;
            self.tabs[self.active].loading = true;
            self.reload(self.active);
        }
        match home_action {
            Some(HomeAction::Open(path)) => self.open_paths(&ctx, vec![path], false),
            Some(HomeAction::ClearRecent) => {
                self.settings.recent.clear();
                self.settings_dirty = true;
            }
            None => {}
        }

        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                Id::new("drop-overlay"),
            ));
            let rect = ctx.content_rect();
            painter.rect_filled(rect, 0.0, Color32::from_black_alpha(120));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                tr!("Drop to open", "Отпустите, чтобы открыть"),
                egui::FontId::proportional(26.0),
                Color32::WHITE,
            );
        }

        self.update_title(&ctx);
        self.follow_theme(&ctx);
        let zoom = ctx.zoom_factor();
        if (zoom - self.settings.zoom).abs() > 0.001 {
            self.settings.zoom = zoom;
            self.settings_dirty = true;
        }
        self.remember_window(&ctx);
        self.save_settings_if_needed();

        if let Some(bench) = self.bench.as_mut() {
            let painted_doc =
                self.tabs.get(self.active).is_some_and(|t| t.doc.is_some()) || self.tabs.is_empty();
            if painted_doc {
                bench.mark_once("doc_frame_end");
            }
            if bench.frames_after_doc(painted_doc) >= 2 {
                bench.finish();
                ctx.send_viewport_cmd(ViewportCommand::Close);
            } else {
                ctx.request_repaint();
            }
        }
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.bench.is_none() {
            self.settings.save();
        }
    }
}

/// The top-level block an anchor (`#setup`, `Setup`, `fn-1`) points at.
pub fn resolve_anchor(doc: &Document, anchor: &str) -> Option<usize> {
    let decoded = percent_decode(anchor);
    doc.anchors
        .get(decoded.as_str())
        .or_else(|| doc.anchors.get(&decoded.to_lowercase()))
        .or_else(|| doc.anchors.get(&heading_anchor_id(&decoded)))
        .copied()
}

/// `https:`, `mailto:` and the like, but not a Windows drive letter.
fn has_url_scheme(target: &str) -> bool {
    match target.split_once(':') {
        Some((scheme, _)) => {
            scheme.len() > 1
                && scheme
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_schemes_are_told_apart_from_drive_letters() {
        assert!(has_url_scheme("https://example.com"));
        assert!(has_url_scheme("mailto:a@b.c"));
        assert!(!has_url_scheme(r"C:\docs\a.md"));
        assert!(!has_url_scheme("docs/a.md"));
        assert!(!has_url_scheme("a.md#x:y"));
    }

    #[test]
    fn anchors_resolve_by_id_text_and_encoding() {
        let doc = Document::parse("# Intro\n\n## Getting Started\n\n## 1. 概述\n");
        assert_eq!(resolve_anchor(&doc, "getting-started"), Some(1));
        assert_eq!(resolve_anchor(&doc, "Getting Started"), Some(1));
        assert_eq!(resolve_anchor(&doc, "1-%E6%A6%82%E8%BF%B0"), Some(2));
        assert_eq!(resolve_anchor(&doc, "missing"), None);
    }

    #[test]
    fn verbatim_prefixes_are_stripped() {
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\a\b.md")),
            PathBuf::from(r"C:\a\b.md")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\UNC\srv\share\a.md")),
            PathBuf::from(r"\\srv\share\a.md")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from("/tmp/a.md")),
            PathBuf::from("/tmp/a.md")
        );
    }
}
