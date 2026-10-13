//! The application: windows, tabs, background loading, input and panels.

use crate::bench::Bench;
use crate::fonts::{Family, FontSpec};
use crate::fs_text;
use crate::markdown::document::{Block, Document, Scripts, Task};
use crate::markdown::highlight::Highlighter;
use crate::markdown::preprocess::{heading_anchor_id, is_markdown_path, percent_decode};
use crate::settings::{Settings, ThemeSetting, TocSide, same_path};
use crate::single_instance::Server;
use crate::style::{self, FontChoice, Palette, body_size};
use crate::ui::about::About;
use crate::ui::confirm::{ConfirmOpen, LinkTarget};
use crate::ui::diagram::DiagramRenderer;
use crate::ui::emoji::EmojiRenderer;
use crate::ui::find::FindState;
use crate::ui::home::{self, HomeAction};
use crate::ui::math::MathRenderer;
use crate::ui::text::{TextChange, TextPanel};
use crate::ui::toc;
use crate::ui::viewer::Pictures;
use crate::ui::viewer::{self, RenderCtx, ViewAction, ViewState};
use crate::watcher::FileWatcher;
use egui::{
    Align, CentralPanel, Color32, CursorIcon, FontData, FontDefinitions, FontId, Frame, Id, Key,
    KeyboardShortcut, LayerId, Layout, Margin, Modifiers, Order, Panel, PointerButton, Pos2, Rect,
    RichText, Sense, Stroke, StrokeKind, TextEdit, Theme, Ui, Vec2, ViewportBuilder,
    ViewportCommand, ViewportId, vec2,
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
    /// A task list item was ticked in this file, or could not be.
    TaskWritten {
        path: PathBuf,
        result: Result<(), String>,
    },
    Fonts(Vec<(String, FontData)>),
    /// The system's font families, for the Text panel.
    FontList(Arc<Vec<Family>>),
    /// A family chosen in the Text panel, resolved to its faces (`None` if
    /// it has none that can be used). `request` tells a late answer from
    /// the last one asked for.
    FontResolved {
        mono: bool,
        request: u64,
        spec: Option<Box<FontSpec>>,
    },
}

#[derive(Debug, Clone)]
pub struct LoadError {
    pub message: String,
    pub not_found: bool,
    /// The path is a folder, which Windows reports as "access denied".
    pub is_dir: bool,
}

pub fn load_file(path: &Path) -> Result<Loaded, LoadError> {
    let decoded = fs_text::read_to_string_lossy(path).map_err(|e| LoadError {
        message: e.to_string(),
        not_found: e.kind() == std::io::ErrorKind::NotFound,
        is_dir: path.is_dir(),
    })?;
    let doc = Document::parse(&decoded.content);
    Ok(Loaded {
        doc: Arc::new(doc),
        encoding: decoded.encoding,
        lossy: decoded.lossy,
    })
}

/// Windows narrower than this, in points, show no table of contents.
const TOC_MIN_WINDOW: f32 = 560.0;

/// The smallest window, in points.
const MIN_WINDOW_SIZE: [f32; 2] = [420.0, 300.0];

/// How long the window rests after a move or resize before it is saved.
const WINDOW_SAVE_DELAY: Duration = Duration::from_secs(1);

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

/// The absolute path, `.` and `..` resolved, without Windows' `\\?\`
/// prefix. Not `canonicalize`: it turns a mapped drive (`Z:\a.md`) into the
/// share behind it, shown in the title and the recent files, and its `\\?\`
/// form, once stripped, would lose paths over 260 characters (std adds the
/// prefix itself where a path needs it). As in qview.
pub fn normalize_path(path: &Path) -> PathBuf {
    strip_verbatim(std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
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
    /// Why ticking a task list item failed, until the next load.
    write_error: Option<String>,
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
            write_error: None,
            view: ViewState::default(),
            pending_anchor: None,
            restore: None,
            back: Vec::new(),
        }
    }
}

/// A window of the app with its tabs. The first one is the root viewport;
/// a tab dragged out of a window gets a window of its own, an egui viewport
/// that lives as long as the root does (when the root is closed while such
/// windows exist, it takes one of them over instead, see `adopt_window`).
struct Window {
    /// Unique for the run; the viewport id is made from it.
    id: u64,
    viewport: ViewportId,
    tabs: Vec<Tab>,
    active: usize,
    find: FindState,
    find_jump: bool,
    /// The caption shown.
    title: String,
    hwnd: Option<isize>,
    /// Asks before a link starts a program or reaches another computer.
    confirm: ConfirmOpen,
    /// How a detached window is made; `None` for the root.
    builder: Option<ViewportBuilder>,
    /// Its viewport has been shown, so its native window exists.
    shown: bool,
    /// Its frame and its content on the screen, in points, from the last
    /// frame it drew.
    outer: Option<Rect>,
    inner: Option<Rect>,
    /// Where the first tab starts, from the window's left edge, so a
    /// window made for a dragged tab puts that tab under the pointer.
    tab_lead: f32,
    /// Its close button was pressed (detached windows only).
    close_requested: bool,
}

impl Window {
    fn new(id: u64, builder: Option<ViewportBuilder>) -> Window {
        let viewport = match builder {
            None => ViewportId::ROOT,
            Some(_) => ViewportId::from_hash_of(("metalmd-window", id)),
        };
        Window {
            id,
            viewport,
            tabs: Vec::new(),
            active: 0,
            find: FindState::default(),
            find_jump: false,
            title: String::new(),
            hwnd: None,
            confirm: ConfirmOpen::default(),
            builder,
            shown: false,
            outer: None,
            inner: None,
            tab_lead: 44.0,
            close_requested: false,
        }
    }

    fn is_root(&self) -> bool {
        self.viewport == ViewportId::ROOT
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Takes tab `index` out, keeping the active tab in range.
    fn remove_tab(&mut self, index: usize) -> Tab {
        let tab = self.tabs.remove(index);
        if self.active > index || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.find.invalidate();
        tab
    }

    /// Puts `tab` at the end and shows it.
    fn add_tab(&mut self, tab: Tab) {
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.find.invalidate();
    }

    /// Moves tab `from` to position `to`, the active tab staying the same.
    fn move_tab(&mut self, from: usize, to: usize) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        if self.active == from {
            self.active = to;
        } else if from < self.active && self.active <= to {
            self.active -= 1;
        } else if to <= self.active && self.active < from {
            self.active += 1;
        }
    }
}

/// A tab being dragged by its title.
struct TabDrag {
    /// The window it is in.
    window: u64,
    tab: u64,
    /// The pointer's offset from the tab's top left corner.
    grab: Vec2,
    /// Where the pointer was last seen, on the screen in points.
    screen_pos: Option<Pos2>,
}

/// What a tab's context menu asked for.
enum TabMenu {
    Detach(usize),
    ToRoot(usize),
    CloseOthers(usize),
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
    /// The root window first, then the detached ones.
    windows: Vec<Window>,
    /// The window whose frame is being drawn.
    current: usize,
    /// The window that last had the focus: files handed over by another
    /// launch open there.
    focused: u64,
    next_window_id: u64,
    next_id: u64,
    drag: Option<TabDrag>,
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
    /// The fallback fonts loaded for the scripts of documents, kept to be
    /// added again when the reader chooses other fonts.
    extra_fonts: Vec<(String, Arc<FontData>)>,
    /// Fonts were set this frame: measure them at the start of the next,
    /// when they are live.
    remeasure_fonts: bool,
    /// The number of the last font asked for in the Text panel.
    font_request: u64,
    text_panel: TextPanel,
    watcher: Option<FileWatcher>,
    frame: u64,
    bench: Option<Bench>,
    about: About,
    /// The window About was opened from.
    about_window: u64,
    start_icon: Option<egui::TextureHandle>,
    /// The theme of the last frame.
    shown_theme: Option<Theme>,
    /// Repaint the window captions on this frame.
    refresh_caption: bool,
    /// When the window last moved or changed size, until that is saved.
    window_moved: Option<Instant>,
    /// Ctrl+wheel notches not yet turned into zoom steps.
    wheel_zoom: f32,
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
        style::set_body_size(startup.settings.text_size);
        let (base_fonts, fonts) = style::load_fonts(
            startup.settings.font.as_ref(),
            startup.settings.mono_font.as_ref(),
        );
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
        let mut root = Window::new(0, None);
        root.hwnd = hwnd;
        root.shown = true;
        root.tabs = startup
            .tabs
            .into_iter()
            .map(|(id, path)| Tab::new(id, path))
            .collect();
        root.active = root.tabs.len().saturating_sub(1);

        App {
            windows: vec![root],
            current: 0,
            focused: 0,
            next_window_id: 1,
            next_id,
            drag: None,
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
            extra_fonts: Vec::new(),
            remeasure_fonts: false,
            font_request: 0,
            text_panel: TextPanel::default(),
            watcher: None,
            frame: 0,
            bench,
            about: About::default(),
            about_window: 0,
            start_icon: None,
            shown_theme: None,
            refresh_caption: false,
            window_moved: None,
            wheel_zoom: 0.0,
        }
    }

    /// The window being drawn.
    fn win(&mut self) -> &mut Window {
        &mut self.windows[self.current]
    }

    fn window_index(&self, id: u64) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }

    /// The window and position of tab `id`.
    fn find_tab(&self, id: u64) -> Option<(usize, usize)> {
        self.windows
            .iter()
            .enumerate()
            .find_map(|(w, window)| window.tab_index(id).map(|t| (w, t)))
    }

    fn all_tabs(&self) -> impl Iterator<Item = &Tab> {
        self.windows.iter().flat_map(|w| w.tabs.iter())
    }

    /// Bring the window captions in line with the theme. egui passes a new
    /// theme to the window at the end of the frame it changed in, and
    /// Windows 10 shows it in the caption only at the next repaint of the
    /// frame, so the frame after the change sets the caption and asks for
    /// that repaint. This also covers Windows switching its theme while
    /// the app follows it.
    fn follow_theme(&mut self, ctx: &egui::Context) {
        let theme = ctx.theme();
        if std::mem::take(&mut self.refresh_caption) {
            for hwnd in self.windows.iter().filter_map(|w| w.hwnd) {
                crate::caption::set_dark(hwnd, theme == Theme::Dark);
                crate::caption::refresh(hwnd);
            }
        }
        if self.shown_theme.is_some_and(|t| t != theme) {
            self.refresh_caption = true;
            ctx.request_repaint();
        }
        self.shown_theme = Some(theme);
    }

    /// Finds the native windows of viewports shown for the first time, for
    /// their captions. eframe gives no handle for a child viewport, so they
    /// are the visible top-level windows of this thread not seen before.
    fn find_new_windows(&mut self, ctx: &egui::Context) {
        if !self.windows.iter().any(|w| w.shown && w.hwnd.is_none()) {
            return;
        }
        let known: Vec<isize> = self.windows.iter().filter_map(|w| w.hwnd).collect();
        let mut fresh = crate::caption::thread_windows()
            .into_iter()
            .filter(|h| !known.contains(h));
        let dark = ctx.theme() == Theme::Dark;
        for window in self
            .windows
            .iter_mut()
            .filter(|w| w.shown && w.hwnd.is_none())
        {
            let Some(hwnd) = fresh.next() else { break };
            window.hwnd = Some(hwnd);
            crate::caption::set_dark(hwnd, dark);
            crate::caption::refresh(hwnd);
        }
    }

    /// Opens `paths` in the current window.
    fn open_paths(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>, focus_window: bool) {
        for path in paths {
            let (path, anchor) = (normalize_path(&path), None);
            self.open_path(path, anchor);
        }
        if focus_window {
            let viewport = self.win().viewport;
            ctx.send_viewport_cmd_to(viewport, ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd_to(viewport, ViewportCommand::Focus);
        }
    }

    /// Opens `path` in a tab of the current window (or activates its tab,
    /// in whichever window it is) and scrolls to `anchor`.
    fn open_path(&mut self, path: PathBuf, anchor: Option<String>) {
        let open = self.windows.iter().enumerate().find_map(|(w, window)| {
            window
                .tabs
                .iter()
                .position(|t| same_path(&t.path, &path))
                .map(|t| (w, t))
        });
        if let Some((w, index)) = open {
            let window = &mut self.windows[w];
            window.active = index;
            window.find.invalidate();
            if let Some(anchor) = anchor {
                let tab = &mut window.tabs[index];
                match tab
                    .doc
                    .as_ref()
                    .and_then(|doc| resolve_anchor(doc, &anchor))
                {
                    Some(block) => tab.view.jump_to(block, 8.0),
                    None => tab.pending_anchor = Some(anchor),
                }
            }
            if w != self.current
                && let Some(ctx) = self.ctx_cell.get()
            {
                ctx.send_viewport_cmd_to(window.viewport, ViewportCommand::Focus);
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
        self.win().add_tab(tab);
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.win().tabs.len() {
            return;
        }
        let tab = self.win().remove_tab(index);
        self.drop_tab(tab);
    }

    /// Closes every tab of the current window but tab `index`.
    fn close_other_tabs(&mut self, index: usize) {
        let window = self.win();
        if index >= window.tabs.len() {
            return;
        }
        let kept = window.tabs.remove(index);
        let closed = std::mem::replace(&mut window.tabs, vec![kept]);
        window.active = 0;
        window.find.invalidate();
        for tab in closed {
            self.drop_tab(tab);
        }
    }

    /// Lets go of a tab taken out of its window.
    fn drop_tab(&mut self, tab: Tab) {
        if let Some(watcher) = self.watcher.as_mut() {
            watcher.unwatch(&tab.path);
        }
        self.forget_images(&tab);
    }

    /// Frees the images of a closed tab that no other tab shows: egui keeps
    /// every image it has loaded, decoded and as a texture, until told.
    fn forget_images(&self, closed: &Tab) {
        let Some(ctx) = self.ctx_cell.get() else {
            return;
        };
        if self.all_tabs().next().is_none() {
            ctx.forget_all_images();
            return;
        }
        let uris = |tab: &Tab| -> Vec<String> {
            let Some(doc) = tab.doc.as_ref() else {
                return Vec::new();
            };
            doc.images
                .iter()
                .filter_map(|src| viewer::image_uri(src, tab.path.parent()))
                .collect()
        };
        let kept: std::collections::HashSet<String> = self.all_tabs().flat_map(uris).collect();
        for uri in uris(closed) {
            if !kept.contains(&uri) {
                ctx.forget_image(&uri);
            }
        }
    }

    /// A reload the reader asked for: the folder is watched anew too, as a
    /// dead watch may be why the change was missed.
    fn reload_by_hand(&mut self, index: usize) {
        let current = self.current;
        if let (Some(watcher), Some(tab)) =
            (self.watcher.as_mut(), self.windows[current].tabs.get(index))
        {
            watcher.refresh(&tab.path);
        }
        self.reload(current, index);
    }

    fn reload(&mut self, window: usize, index: usize) {
        let tab = &mut self.windows[window].tabs[index];
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

    /// Reloads every tab showing the file at `path`.
    fn reload_path(&mut self, path: &Path) {
        let mut changed = Vec::new();
        for (w, window) in self.windows.iter().enumerate() {
            for (t, tab) in window.tabs.iter().enumerate() {
                if same_path(&tab.path, path) {
                    changed.push((w, t));
                }
            }
        }
        for (w, t) in changed {
            self.reload(w, t);
        }
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
                let Some((w, index)) = self.find_tab(tab) else {
                    return;
                };
                if self.windows[w].tabs[index].generation != generation {
                    return;
                }
                let tab = &mut self.windows[w].tabs[index];
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
                        tab.write_error = None;
                        let path = tab.path.clone();
                        self.windows[w].find.invalidate();
                        self.settings.add_recent(&path);
                        self.settings_dirty = true;
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
                            // Worded here: the first loads start before the
                            // interface language is known.
                            tab.error = Some(if error.is_dir {
                                tr!("It is a folder, not a file.", "Это папка, а не файл.")
                                    .to_owned()
                            } else {
                                error.message
                            });
                            if error.not_found {
                                let path = tab.path.clone();
                                self.settings.remove_recent(&path);
                                self.settings_dirty = true;
                            }
                        }
                    }
                }
            }
            AppMsg::OpenPaths(paths) => {
                // Into the window the reader used last.
                let current = self.current;
                self.current = self.window_index(self.focused).unwrap_or(0);
                self.open_paths(ctx, paths, true);
                self.current = current;
            }
            AppMsg::FileChanged(path) => self.reload_path(&path),
            // Reloaded at once rather than when the watcher notices.
            AppMsg::TaskWritten { path, result } => match result {
                Ok(()) => self.reload_path(&path),
                Err(error) => {
                    for window in &mut self.windows {
                        for tab in &mut window.tabs {
                            if same_path(&tab.path, &path) {
                                tab.write_error = Some(error.clone());
                            }
                        }
                    }
                }
            },
            AppMsg::Fonts(extra) => {
                if !extra.is_empty() {
                    let extra: Vec<(String, Arc<FontData>)> = extra
                        .into_iter()
                        .map(|(key, data)| (key, Arc::new(data)))
                        .collect();
                    self.base_fonts = style::with_fallbacks(self.base_fonts.clone(), &extra);
                    self.extra_fonts.extend(extra);
                    ctx.set_fonts(self.base_fonts.clone());
                    self.fonts_generation += 1;
                }
            }
            AppMsg::FontList(families) => {
                self.text_panel.families = Some(families);
                self.text_panel.listing = false;
            }
            AppMsg::FontResolved {
                mono,
                request,
                spec,
            } => {
                if request != self.font_request || spec.is_none() {
                    return;
                }
                let spec = spec.map(|spec| *spec);
                if mono {
                    self.settings.mono_font = spec;
                } else {
                    self.settings.font = spec;
                }
                self.settings_dirty = true;
                self.reload_fonts(ctx);
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
        while self.windows[0].active_tab().is_some_and(|t| t.loading) {
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
            for tab in self.all_tabs() {
                watcher.watch(&tab.path);
            }
            self.watcher = Some(watcher);
        }
        if self
            .all_tabs()
            .any(|t| t.doc.as_ref().is_some_and(|d| d.has_code))
        {
            self.highlighter.warm_up();
        }
    }

    fn open_dialog(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("Markdown", &["md", "markdown", "mdown", "mkd", "txt"])
            .add_filter(tr!("All files", "Все файлы"), &["*"]);
        if let Some(dir) = self.win().active_tab().and_then(|t| t.path.parent()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(paths) = dialog.pick_files() {
            self.open_paths(ctx, paths, false);
        }
    }

    fn handle_action(&mut self, ctx: &egui::Context, action: ViewAction) {
        let target = match action {
            ViewAction::OpenLink(target) => target,
            ViewAction::ToggleTask(task) => {
                if let Some(path) = self.win().active_tab().map(|t| t.path.clone()) {
                    self.toggle_task(path, task);
                }
                return;
            }
        };
        let target = target.trim().to_owned();
        if let Some(fragment) = target.strip_prefix('#') {
            let Some(tab) = self.win().active_tab_mut() else {
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
        let file_url = file_url_path(&target);
        let target = file_url.unwrap_or(target);
        if has_url_scheme(&target) {
            self.open_link_target(LinkTarget::Url(target));
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
            .win()
            .active_tab()
            .and_then(|t| t.path.parent().map(Path::to_path_buf));
        let candidate = Path::new(&decoded);
        let resolved = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else if let Some(base) = base {
            base.join(candidate)
        } else {
            return;
        };
        let _ = ctx;
        self.open_link_target(if is_markdown_path(&decoded) {
            LinkTarget::Markdown(resolved, fragment)
        } else {
            LinkTarget::File(resolved)
        });
    }

    /// Ticks or clears a task list item in the file, off the UI thread: the
    /// file may be on a slow disk or another computer.
    fn toggle_task(&self, path: PathBuf, task: Task) {
        let sender = self.sender.clone();
        let cell = self.ctx_cell.clone();
        std::thread::spawn(move || {
            let result = crate::tasks::toggle(&path, task);
            let _ = sender.send(AppMsg::TaskWritten { path, result });
            request_repaint(&cell);
        });
    }

    /// Opens a link's target, or asks first if it may run something or
    /// reach another computer.
    fn open_link_target(&mut self, target: LinkTarget) {
        let window = self.win();
        let document = window.active_tab().map(|t| t.path.as_path());
        match crate::ui::confirm::risk(&target, document) {
            Some(risk) => window.confirm.ask(target, risk),
            None => self.open_target(target),
        }
    }

    fn open_target(&mut self, target: LinkTarget) {
        match target {
            LinkTarget::Markdown(path, fragment) => self.open_path(normalize_path(&path), fragment),
            // Off the UI thread: the shell can take a while, and a path on
            // an unreachable computer blocks for the network's timeout.
            LinkTarget::Url(url) => {
                std::thread::spawn(move || open::that_detached(&url));
            }
            LinkTarget::File(path) => {
                std::thread::spawn(move || {
                    if path.exists() {
                        let _ = open::that_detached(&path);
                    }
                });
            }
        }
    }

    fn go_back(&mut self) {
        if let Some(tab) = self.win().active_tab_mut()
            && let Some((block, offset)) = tab.back.pop()
        {
            tab.view.jump_to(block, offset);
        }
    }

    /// Ctrl+wheel zooms by Ctrl+= and Ctrl+−'s 10 % a notch, as browsers do.
    /// egui turns the wheel into a smooth `zoom_delta` instead, but every
    /// change of zoom measures the blocks and draws the pictures anew.
    /// Touchpads send fractions of a notch, which add up.
    fn zoom_with_wheel(&mut self, ctx: &egui::Context) {
        let (command, notches) = ctx.input(|i| {
            let notches: f32 = i
                .events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                        ..
                    } if modifiers.command => Some(match unit {
                        egui::MouseWheelUnit::Point => delta.y / 50.0,
                        egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page => delta.y,
                    }),
                    _ => None,
                })
                .sum();
            (i.modifiers.command, notches)
        });
        if !command {
            self.wheel_zoom = 0.0;
            return;
        }
        self.wheel_zoom += notches;
        while self.wheel_zoom >= 1.0 {
            self.wheel_zoom -= 1.0;
            egui::gui_zoom::zoom_in(ctx);
        }
        while self.wheel_zoom <= -1.0 {
            self.wheel_zoom += 1.0;
            egui::gui_zoom::zoom_out(ctx);
        }
    }

    /// Builds the fonts anew from the settings, keeping the fallbacks
    /// loaded for documents. They are live from the next frame, which
    /// measures them (`remeasure_fonts`).
    fn reload_fonts(&mut self, ctx: &egui::Context) {
        let (defs, choice) = style::load_fonts(
            self.settings.font.as_ref(),
            self.settings.mono_font.as_ref(),
        );
        self.base_fonts = style::with_fallbacks(defs, &self.extra_fonts);
        ctx.set_fonts(self.base_fonts.clone());
        self.fonts = choice;
        self.remeasure_fonts = true;
        ctx.request_repaint();
    }

    /// Opens or closes the Text panel in the current window, and lists the
    /// system's fonts the first time.
    fn toggle_text_panel(&mut self) {
        let id = self.win().id;
        let panel = &mut self.text_panel;
        panel.open = !(panel.open && panel.window == id);
        panel.window = id;
        if panel.open && panel.families.is_none() && !panel.listing {
            panel.listing = true;
            let sender = self.sender.clone();
            let cell = self.ctx_cell.clone();
            std::thread::spawn(move || {
                let families = crate::fonts::system_families();
                let _ = sender.send(AppMsg::FontList(Arc::new(families)));
                request_repaint(&cell);
            });
        }
    }

    fn apply_text_change(&mut self, ctx: &egui::Context, change: TextChange) {
        match change {
            TextChange::Font { mono, family } => {
                self.font_request += 1;
                match family {
                    None => {
                        if mono {
                            self.settings.mono_font = None;
                        } else {
                            self.settings.font = None;
                        }
                        self.settings_dirty = true;
                        self.reload_fonts(ctx);
                    }
                    // Its files are read to find its axes: off the UI thread.
                    Some(family) => {
                        let request = self.font_request;
                        let sender = self.sender.clone();
                        let cell = self.ctx_cell.clone();
                        std::thread::spawn(move || {
                            let spec = crate::fonts::resolve(&family).map(Box::new);
                            let _ = sender.send(AppMsg::FontResolved {
                                mono,
                                request,
                                spec,
                            });
                            request_repaint(&cell);
                        });
                    }
                }
            }
            TextChange::Size(size) => {
                style::set_body_size(size);
                self.settings.text_size = style::body_size();
                self.settings_dirty = true;
                self.fonts_generation += 1;
            }
            TextChange::Reset => {
                self.font_request += 1;
                self.settings.font = None;
                self.settings.mono_font = None;
                style::set_body_size(style::DEFAULT_BODY_SIZE);
                self.settings.text_size = style::DEFAULT_BODY_SIZE;
                self.settings_dirty = true;
                self.reload_fonts(ctx);
            }
        }
    }

    /// About is open in the current window.
    fn about_here(&self) -> bool {
        self.about.open && self.about_window == self.windows[self.current].id
    }

    fn toggle_about(&mut self) {
        let id = self.win().id;
        if self.about.open && self.about_window != id {
            // Open in another window: bring it here.
            self.about.open = false;
        }
        self.about.toggle();
        self.about_window = id;
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let command = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let command_shift = |key| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key);
        let plain = |key| KeyboardShortcut::new(Modifiers::NONE, key);
        let shift = |key| KeyboardShortcut::new(Modifiers::SHIFT, key);
        let consume = |shortcut: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&shortcut));

        self.zoom_with_wheel(ctx);
        if consume(plain(Key::F1)) {
            self.toggle_about();
        }
        // A dialog takes the keyboard (Enter and Esc close it).
        if self.about_here() || self.win().confirm.is_open() {
            return;
        }
        if self.text_panel.open
            && self.text_panel.window == self.win().id
            && consume(plain(Key::Escape))
        {
            self.text_panel.open = false;
        }
        if consume(command(Key::F)) {
            let find = &mut self.win().find;
            if find.open {
                find.request_focus = true;
            } else {
                find.open();
            }
        }
        if consume(command(Key::O)) {
            self.open_dialog(ctx);
        }
        if consume(command(Key::W)) && !self.win().tabs.is_empty() {
            let active = self.win().active;
            self.close_tab(active);
        }
        let tab_count = self.win().tabs.len();
        if tab_count > 0 {
            if consume(command_shift(Key::Tab)) || consume(command(Key::PageUp)) {
                let window = self.win();
                window.active = (window.active + tab_count - 1) % tab_count;
                window.find.invalidate();
            }
            if consume(command(Key::Tab)) || consume(command(Key::PageDown)) {
                let window = self.win();
                window.active = (window.active + 1) % tab_count;
                window.find.invalidate();
            }
        }
        if consume(command_shift(Key::O)) {
            self.settings.toc_visible = !self.settings.toc_visible;
            self.settings_dirty = true;
        }
        if (consume(plain(Key::F5)) || consume(command(Key::R))) && self.win().active < tab_count {
            let active = self.win().active;
            self.reload_by_hand(active);
        }
        if self.win().find.open {
            if consume(plain(Key::F3)) {
                let window = self.win();
                window.find.step(true);
                window.find_jump = true;
            }
            if consume(shift(Key::F3)) {
                let window = self.win();
                window.find.step(false);
                window.find_jump = true;
            }
            if consume(plain(Key::Escape)) {
                self.win().find.close();
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
        let Some(tab) = self.win().active_tab_mut() else {
            return;
        };
        // Chromium's steps: 87.5 % of the view for a page, 40 px for a line.
        let page = (tab.view.viewport_height * 0.875).max(40.0);
        let line = body_size() * 2.5;
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
        let mut menu_action = None;
        let cur = self.current;
        let (wid, is_root) = (self.windows[cur].id, self.windows[cur].is_root());
        Panel::top(Id::new(("tab-bar", wid)))
            .frame(
                Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .inner_margin(Margin::symmetric(8, 5)),
            )
            .show(ui, |ui| {
                let bar = ui.max_rect();
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    // File actions first, where opening one adds its tab.
                    if crate::ui::open_button(ui).clicked() {
                        open_dialog = true;
                    }
                    ui.separator();
                    let tab_count = self.windows[cur].tabs.len();
                    let active = self.windows[cur].active;
                    let mut rects = Vec::with_capacity(tab_count);
                    let mut drag_start = None;
                    for i in 0..tab_count {
                        let tab = &self.windows[cur].tabs[i];
                        let (tab_id, path_text) = (tab.id, tab.path.display().to_string());
                        let mut title = RichText::new(&tab.title).size(13.5);
                        if tab.deleted {
                            title = title.strikethrough();
                        }
                        // Under the tab's own id, so a drag follows the tab
                        // when it is moved along the bar.
                        let response = ui
                            .push_id(tab_id, |ui| {
                                ui.add(
                                    egui::Button::selectable(i == active, title)
                                        .sense(Sense::click_and_drag()),
                                )
                            })
                            .inner
                            .on_hover_text(path_text);
                        rects.push(response.rect);
                        if response.clicked() {
                            activate = Some(i);
                        }
                        if response.middle_clicked() {
                            close = Some(i);
                        }
                        if response.drag_started_by(PointerButton::Primary) {
                            activate = Some(i);
                            // Where the button went down, not where the
                            // pointer is now that egui calls it a drag.
                            let grab = ui
                                .input(|i| i.pointer.press_origin())
                                .map_or(Vec2::ZERO, |p| p - response.rect.min);
                            drag_start = Some((tab_id, grab));
                        }
                        response.context_menu(|ui| {
                            if tab_count > 1
                                && ui
                                    .button(tr!("Move to a new window", "В новое окно"))
                                    .clicked()
                            {
                                menu_action = Some(TabMenu::Detach(i));
                                ui.close();
                            }
                            if !is_root
                                && ui
                                    .button(tr!("Move to the main window", "В главное окно"))
                                    .clicked()
                            {
                                menu_action = Some(TabMenu::ToRoot(i));
                                ui.close();
                            }
                            if ui.button(tr!("Close", "Закрыть")).clicked() {
                                close = Some(i);
                                ui.close();
                            }
                            ui.separator();
                            if ui
                                .add_enabled(
                                    tab_count > 1,
                                    egui::Button::new(tr!("Close others", "Закрыть другие")),
                                )
                                .clicked()
                            {
                                menu_action = Some(TabMenu::CloseOthers(i));
                                ui.close();
                            }
                        });
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
                    if let Some(first) = rects.first() {
                        self.windows[cur].tab_lead = first.left() - bar.left();
                    }
                    if let Some((tab, grab)) = drag_start {
                        self.drag = Some(TabDrag {
                            window: wid,
                            tab,
                            grab,
                            screen_pos: None,
                        });
                    }
                    self.drag_along_bar(ui, palette, bar, &rects);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        if crate::ui::about_button(ui).clicked() {
                            self.toggle_about();
                        }
                        let theme = self.settings.theme;
                        if crate::ui::theme_button(ui, theme).clicked() {
                            self.settings.theme = theme.next();
                            ui.ctx().set_theme(self.settings.theme.preference());
                            self.settings_dirty = true;
                        }
                        let text_open = self.text_panel.open && self.text_panel.window == wid;
                        if crate::ui::text_button(ui, text_open).clicked() {
                            self.toggle_text_panel();
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
                        if let Some(tab) = self.windows[cur].active_tab() {
                            let mut status = Vec::new();
                            if tab.deleted {
                                status.push(tr!("file deleted", "файл удалён").to_owned());
                            }
                            if tab.write_error.is_some() {
                                status.push(tr!("not saved", "не сохранено").to_owned());
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
                                let label = ui.label(
                                    RichText::new(status.join(" · "))
                                        .size(12.0)
                                        .color(palette.muted),
                                );
                                if let Some(error) = &tab.write_error {
                                    label.on_hover_text(format!(
                                        "{}
{error}",
                                        tr!(
                                            "The task list item was not ticked:",
                                            "Пункт списка задач не отмечен:"
                                        )
                                    ));
                                }
                            }
                        }
                    });
                });
            });
        if let Some(i) = activate
            && i != self.win().active
        {
            let window = self.win();
            window.active = i;
            window.find.invalidate();
        }
        if let Some(i) = close {
            self.close_tab(i);
        }
        match menu_action {
            Some(TabMenu::Detach(i)) => self.detach_tab(cur, i, None),
            Some(TabMenu::ToRoot(i)) => {
                self.move_tab_to_window(cur, i, 0);
                let root = self.windows[0].viewport;
                ui.ctx().send_viewport_cmd_to(root, ViewportCommand::Focus);
            }
            Some(TabMenu::CloseOthers(i)) => self.close_other_tabs(i),
            None => {}
        }
        if open_dialog {
            let ctx = ui.ctx().clone();
            self.open_dialog(&ctx);
        }
    }

    /// While a tab of this window is dragged: along the bar it changes
    /// places with its neighbours, away from it a ghost of its title
    /// follows the pointer. `rects` are the tabs' places in the bar.
    fn drag_along_bar(&mut self, ui: &mut Ui, palette: &Palette, bar: Rect, rects: &[Rect]) {
        let cur = self.current;
        let Some(drag) = self.drag.as_ref() else {
            return;
        };
        if drag.window != self.windows[cur].id {
            return;
        }
        let Some(from) = self.windows[cur].tab_index(drag.tab) else {
            return;
        };
        let Some(pointer) = ui.ctx().pointer_latest_pos() else {
            return;
        };
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        if bar.y_range().contains(pointer.y) {
            // Its place is after every other tab whose middle is left of
            // the pointer.
            let to = rects
                .iter()
                .enumerate()
                .filter(|(i, r)| *i != from && r.center().x < pointer.x)
                .count();
            self.windows[cur].move_tab(from, to);
            return;
        }
        let title = self.windows[cur].tabs[from].title.clone();
        let grab = drag.grab;
        let painter = ui
            .ctx()
            .layer_painter(LayerId::new(Order::Tooltip, Id::new(("tab-drag", cur))));
        let galley = painter.layout_no_wrap(title, FontId::proportional(13.5), palette.text);
        let rect = Rect::from_min_size(pointer - grab, galley.size() + vec2(16.0, 8.0));
        painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
        painter.rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0, palette.muted),
            StrokeKind::Inside,
        );
        painter.galley(rect.min + vec2(8.0, 4.0), galley, palette.text);
    }

    /// Follows a drag of this window's tab until the button is released,
    /// then drops the tab where the pointer is: into another window, or
    /// into a new one when outside every window.
    fn track_drag(&mut self, ctx: &egui::Context) {
        let cur = self.current;
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        if drag.window != self.windows[cur].id {
            return;
        }
        // The pointer on the screen: Windows keeps sending its position
        // to the window the button went down in, even outside it.
        let on_screen = ctx.input(|i| {
            let pos = i.pointer.latest_pos()?;
            let inner = i.viewport().inner_rect?;
            Some(inner.min + pos.to_vec2())
        });
        if on_screen.is_some() {
            drag.screen_pos = on_screen;
        }
        if ctx.input(|i| i.pointer.any_down()) {
            ctx.request_repaint();
            return;
        }
        let Some(drag) = self.drag.take() else { return };
        let Some(from) = self.windows[cur].tab_index(drag.tab) else {
            return;
        };
        let Some(pos) = drag.screen_pos else { return };
        if self.windows[cur].outer.is_some_and(|r| r.contains(pos)) {
            // Dropped in its own window: it was moved along the bar.
            return;
        }
        let target = self
            .windows
            .iter()
            .position(|w| w.id != drag.window && w.outer.is_some_and(|r| r.contains(pos)));
        if let Some(to) = target {
            self.move_tab_to_window(cur, from, to);
            ctx.send_viewport_cmd_to(self.windows[to].viewport, ViewportCommand::Focus);
        } else if self.windows[cur].tabs.len() > 1 {
            self.detach_tab(cur, from, Some(pos - drag.grab));
        }
    }

    /// Moves tab `index` of window `from` to the end of window `to`.
    fn move_tab_to_window(&mut self, from: usize, index: usize, to: usize) {
        if from == to || index >= self.windows[from].tabs.len() {
            return;
        }
        let tab = self.windows[from].remove_tab(index);
        self.windows[to].add_tab(tab);
    }

    /// Gives tab `index` of window `from` a window of its own, the size of
    /// the one it leaves. With `tab_pos`, the top left corner of the tab
    /// on the screen, the new window is placed so that the tab lies there;
    /// otherwise it is offset from the window it leaves.
    fn detach_tab(&mut self, from: usize, index: usize, tab_pos: Option<Pos2>) {
        let source = &self.windows[from];
        if index >= source.tabs.len() || source.tabs.len() < 2 {
            return;
        }
        let size = source
            .inner
            .map(|r| r.size())
            .unwrap_or(vec2(980.0, 780.0))
            .max(MIN_WINDOW_SIZE.into());
        // The window's frame: caption above the content, border left of it.
        let frame = match (source.inner, source.outer) {
            (Some(inner), Some(outer)) => inner.min - outer.min,
            _ => vec2(8.0, 31.0),
        };
        let position = match tab_pos {
            Some(tab) => tab - frame - vec2(source.tab_lead, 5.0),
            None => source
                .outer
                .map_or(Pos2::new(100.0, 100.0), |o| o.min + vec2(40.0, 40.0)),
        };
        let id = self.next_window_id;
        self.next_window_id += 1;
        let builder = ViewportBuilder::default()
            .with_title("MetalMD")
            .with_inner_size(size)
            .with_position(position)
            .with_min_inner_size(MIN_WINDOW_SIZE)
            .with_drag_and_drop(true)
            .with_icon(crate::window_icon());
        let mut window = Window::new(id, Some(builder));
        let tab = self.windows[from].remove_tab(index);
        window.add_tab(tab);
        self.windows.push(window);
        self.focused = id;
    }

    /// The root window was told to close while detached windows exist: its
    /// tabs close, and it takes the place and the tabs of the window the
    /// reader used last, which goes away. Child viewports cannot outlive
    /// the root, so this keeps that window open as the reader expects.
    fn adopt_window(&mut self, ctx: &egui::Context, root_maximized: bool) {
        let child = self
            .window_index(self.focused)
            .filter(|&i| i != 0)
            .unwrap_or(1);
        let closed = std::mem::take(&mut self.windows[0].tabs);
        for tab in closed {
            self.drop_tab(tab);
        }
        let tabs = std::mem::take(&mut self.windows[child].tabs);
        let active = self.windows[child].active;
        let (outer, inner) = (self.windows[child].outer, self.windows[child].inner);
        self.windows[child].close_requested = true;
        let root = &mut self.windows[0];
        root.tabs = tabs;
        root.active = active;
        root.find.invalidate();
        self.focused = root.id;
        if root_maximized {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
        }
        if let Some(outer) = outer {
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(outer.min));
        }
        if let Some(inner) = inner {
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(inner.size()));
        }
    }

    /// Closes the detached windows whose button was pressed or that have
    /// no tab left.
    fn close_windows(&mut self) {
        let mut i = 1;
        while i < self.windows.len() {
            if !self.windows[i].close_requested && !self.windows[i].tabs.is_empty() {
                i += 1;
                continue;
            }
            let window = self.windows.remove(i);
            for tab in window.tabs {
                self.drop_tab(tab);
            }
            if self.focused == window.id {
                self.focused = self.windows[0].id;
            }
            if self.drag.as_ref().is_some_and(|d| d.window == window.id) {
                self.drag = None;
            }
        }
    }

    fn find_bar(&mut self, ui: &mut Ui, palette: &Palette) {
        let cur = self.current;
        if !self.windows[cur].find.open {
            return;
        }
        let wid = self.windows[cur].id;
        let doc = self.windows[cur].active_tab().and_then(|t| t.doc.clone());
        let find_bar = Id::new(("find-bar", wid));
        let find_input = Id::new(("find-input", wid));
        Panel::top(find_bar)
            .frame(
                Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .inner_margin(Margin::symmetric(10, 5)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let window = &mut self.windows[cur];
                    let input = ui.add(
                        TextEdit::singleline(&mut window.find.query)
                            .id(find_input)
                            .hint_text(tr!("Find in document", "Найти в документе"))
                            .desired_width(280.0),
                    );
                    if window.find.request_focus {
                        input.request_focus();
                        window.find.request_focus = false;
                    }
                    if input.changed() {
                        window.find.current = 0;
                        window.find_jump = true;
                    }
                    if input.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        let forward = !ui.input(|i| i.modifiers.shift);
                        window.find.step(forward);
                        window.find_jump = true;
                        input.request_focus();
                    }
                    if let Some(doc) = doc.as_ref() {
                        window.find.refresh(doc);
                    }
                    let count = window.find.matches.len();
                    let status = if window.find.query.is_empty() {
                        String::new()
                    } else if count == 0 {
                        tr!("No results", "Не найдено").to_owned()
                    } else {
                        let current = window.find.current + 1;
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
                        window.find.step(false);
                        window.find_jump = true;
                    }
                    if ui
                        .button("↓")
                        .on_hover_text(tr!("Next (F3, Enter)", "Следующее (F3, Enter)"))
                        .clicked()
                    {
                        window.find.step(true);
                        window.find_jump = true;
                    }
                    if ui
                        .button("×")
                        .on_hover_text(tr!("Close (Esc)", "Закрыть (Esc)"))
                        .clicked()
                    {
                        window.find.close();
                    }
                });
            });
    }

    /// Scrolls the active tab to the current search match.
    fn jump_to_find_match(&mut self) {
        let window = self.win();
        if !std::mem::take(&mut window.find_jump) {
            return;
        }
        let active = window.active;
        let Some(tab) = window.tabs.get_mut(active) else {
            return;
        };
        let Some(doc) = tab.doc.as_ref() else { return };
        window.find.refresh(doc);
        let Some(m) = window.find.current_match() else {
            return;
        };
        let mut offset = tab.view.viewport_height * 0.3;
        if let Some(Block::Code { id, code, .. }) = doc.blocks.get(m.block)
            && *id == m.id
        {
            let line = code[..m.range.start.min(code.len())].matches('\n').count();
            offset -= line as f32 * (body_size() * 0.875 * 1.45).round();
        }
        tab.view.jump_to(m.block, offset);
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let window = self.win();
        let title = match window.active_tab() {
            Some(tab) => format!("{} — MetalMD", tab.title),
            None => "MetalMD".to_owned(),
        };
        if title != window.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            window.title = title;
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
            if geometry.width > 100.0
                && geometry.height > 100.0
                && self.settings.window != Some(geometry)
            {
                self.settings.window = Some(geometry);
                self.window_moved = Some(Instant::now());
            }
        }
        // Saved once the window has rested for a second, not at every
        // frame of a drag, and not only at exit, which a crash or a logoff
        // may skip.
        if let Some(moved) = self.window_moved {
            if moved.elapsed() >= WINDOW_SAVE_DELAY {
                self.window_moved = None;
                self.settings_dirty = true;
            } else {
                ctx.request_repaint_after(WINDOW_SAVE_DELAY);
            }
        }
    }

    fn save_settings_if_needed(&mut self) {
        if std::mem::take(&mut self.settings_dirty) {
            self.settings.save_in_background();
        }
    }

    /// One frame of the current window.
    fn window_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let cur = self.current;
        let (inner, outer, focused, close_requested, maximized) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.inner_rect,
                v.outer_rect,
                v.focused.unwrap_or(false),
                v.close_requested(),
                v.maximized.unwrap_or(false),
            )
        });
        {
            let window = &mut self.windows[cur];
            window.inner = inner;
            window.outer = outer;
            if focused {
                self.focused = window.id;
            }
        }
        if close_requested {
            if cur == 0 {
                if self.windows.len() > 1 {
                    ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                    self.adopt_window(&ctx, maximized);
                }
            } else {
                self.windows[cur].close_requested = true;
            }
        }

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
        self.track_drag(&ctx);
        self.handle_shortcuts(&ctx);

        let palette = Palette::new(ctx.theme() == Theme::Dark);
        self.tab_bar(ui, &palette);
        self.find_bar(ui, &palette);
        self.jump_to_find_match();

        let wid = self.windows[cur].id;
        let active_doc = self.windows[cur].active_tab().and_then(|t| t.doc.clone());
        // In a narrow window the contents take at most a third of it, and
        // none below `TOC_MIN_WINDOW`: the text needs the room more. The
        // setting stays, so a wider window shows them again.
        let window_width = ui.available_width();
        if self.settings.toc_visible
            && window_width >= TOC_MIN_WINDOW
            && let (Some(doc), Some(tab)) =
                (active_doc.as_ref(), self.windows[cur].active_tab_mut())
            && !doc.headings.is_empty()
        {
            let widest = (window_width * 0.35).clamp(140.0, 480.0);
            let panel = match self.settings.toc_side {
                TocSide::Left => Panel::left(Id::new(("toc", wid))),
                TocSide::Right => Panel::right(Id::new(("toc", wid))),
            };
            panel
                .resizable(true)
                .default_size(250.0_f32.min(widest))
                .size_range(140.0..=widest)
                .frame(
                    Frame::new()
                        .fill(ui.visuals().panel_fill)
                        .inner_margin(Margin::symmetric(10, 4)),
                )
                .show(ui, |ui| {
                    // Right-clicking the panel outside the headings moves it.
                    let background = ui.interact(
                        ui.max_rect(),
                        Id::new(("toc-background", wid)),
                        Sense::click(),
                    );
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
        let Window {
            tabs, active, find, ..
        } = &mut self.windows[cur];
        let active_tab = tabs.get_mut(*active);
        CentralPanel::no_frame()
            .frame(Frame::new().fill(ui.visuals().panel_fill))
            .show(ui, |ui| match active_tab {
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
                            find,
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
        if self.about_here() {
            let lang = self.settings.language;
            self.about.show(&ctx, &mut self.settings.language);
            if self.settings.language != lang {
                self.settings_dirty = true;
            }
        }
        if self.text_panel.open
            && self.text_panel.window == wid
            && let Some(change) = self.text_panel.show(&ctx, &self.settings)
        {
            self.apply_text_change(&ctx, change);
        }
        if let Some(target) = self.windows[cur].confirm.show(&ctx) {
            self.open_target(target);
        }
        for action in actions {
            self.handle_action(&ctx, action);
        }
        if retry {
            let window = self.win();
            let active = window.active;
            window.tabs[active].error = None;
            window.tabs[active].loading = true;
            self.reload_by_hand(active);
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
            let painter = ctx.layer_painter(LayerId::new(
                Order::Foreground,
                Id::new(("drop-overlay", wid)),
            ));
            let rect = ctx.content_rect();
            painter.rect_filled(rect, 0.0, Color32::from_black_alpha(120));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                tr!("Drop to open", "Отпустите, чтобы открыть"),
                FontId::proportional(26.0),
                Color32::WHITE,
            );
        }

        self.update_title(&ctx);
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
        if std::mem::take(&mut self.remeasure_fonts) {
            // The fonts set last frame are live now; every block is
            // measured again with them.
            self.fonts.measure_ascents(&ctx);
            self.fonts_generation += 1;
        }
        self.process_messages(&ctx);

        self.current = 0;
        self.window_ui(ui);
        // The detached windows, each in a viewport of its own. A window
        // made during this frame (a tab dragged out) is shown at once.
        let mut i = 1;
        while i < self.windows.len() {
            self.current = i;
            let viewport = self.windows[i].viewport;
            let builder = self.windows[i].builder.clone().unwrap_or_default();
            ctx.show_viewport_immediate(viewport, builder, |ui, _class| self.window_ui(ui));
            self.windows[i].shown = true;
            i += 1;
        }
        self.current = 0;
        self.close_windows();
        self.find_new_windows(&ctx);

        self.follow_theme(&ctx);
        let zoom = ctx.zoom_factor();
        if (zoom - self.settings.zoom).abs() > 0.001 {
            self.settings.zoom = zoom;
            self.settings_dirty = true;
        }
        self.remember_window(&ctx);
        // A benchmark run leaves the settings and recent files alone.
        if self.bench.is_none() {
            self.save_settings_if_needed();
        }

        if let Some(bench) = self.bench.as_mut() {
            // The document, or the error in its place when it cannot be read.
            let root = &self.windows[0];
            let painted_doc = root
                .active_tab()
                .is_some_and(|t| t.doc.is_some() || t.error.is_some())
                || root.tabs.is_empty();
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

/// The path a `file:` URL names (`file:///C:/a.md` → `C:/a.md`,
/// `file://host/share/a.md` → `//host/share/a.md`), percent-decoded.
fn file_url_path(target: &str) -> Option<String> {
    let scheme = target.get(..5)?;
    if !scheme.eq_ignore_ascii_case("file:") {
        return None;
    }
    let rest = &target[5..];
    let path = match rest.strip_prefix("///") {
        Some(local) => local.to_owned(),
        None if rest.starts_with("//") => rest.to_owned(),
        None => rest.trim_start_matches('/').to_owned(),
    };
    Some(percent_decode(&path))
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
    fn file_urls_become_paths() {
        assert_eq!(
            file_url_path("file:///C:/My%20Docs/a.md").as_deref(),
            Some("C:/My Docs/a.md")
        );
        assert_eq!(
            file_url_path("FILE://host/share/a.exe").as_deref(),
            Some("//host/share/a.exe")
        );
        assert_eq!(file_url_path("file:/C:/a.png").as_deref(), Some("C:/a.png"));
        assert_eq!(file_url_path("https://example.com"), None);
        assert_eq!(file_url_path("fil"), None);
    }

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

    #[cfg(windows)]
    #[test]
    fn paths_are_made_absolute_and_keep_their_drive() {
        assert_eq!(
            normalize_path(Path::new(r"Z:\docs\..\a.md")),
            PathBuf::from(r"Z:\a.md")
        );
        assert_eq!(
            normalize_path(Path::new(r"C:\a\.\b.md")),
            PathBuf::from(r"C:\a\b.md")
        );
        assert!(normalize_path(Path::new("a.md")).is_absolute());
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

    fn window_with_tabs(n: u64) -> Window {
        let mut window = Window::new(1, Some(ViewportBuilder::default()));
        for id in 0..n {
            window
                .tabs
                .push(Tab::new(id, PathBuf::from(format!("{id}.md"))));
        }
        window
    }

    fn ids(window: &Window) -> Vec<u64> {
        window.tabs.iter().map(|t| t.id).collect()
    }

    #[test]
    fn moving_a_tab_along_the_bar_keeps_the_active_one() {
        let mut window = window_with_tabs(4);
        window.active = 0;
        window.move_tab(0, 2);
        assert_eq!(ids(&window), [1, 2, 0, 3]);
        assert_eq!(window.active, 2);
        // Another tab is active and gets pushed along.
        window.active = 1;
        window.move_tab(3, 0);
        assert_eq!(ids(&window), [3, 1, 2, 0]);
        assert_eq!(window.active, 2);
        window.move_tab(0, 3);
        assert_eq!(ids(&window), [1, 2, 0, 3]);
        assert_eq!(window.active, 1);
    }

    #[test]
    fn removing_a_tab_keeps_the_active_one_in_range() {
        let mut window = window_with_tabs(3);
        window.active = 2;
        window.remove_tab(2);
        assert_eq!(window.active, 1);
        window.active = 1;
        window.remove_tab(0);
        assert_eq!(window.active, 0);
        assert_eq!(ids(&window), [1]);
    }
}
