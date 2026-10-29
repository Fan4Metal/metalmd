//! The application: windows, tabs, background loading, input and panels.

use crate::bench::Bench;
use crate::edit::{Change, Written};
use crate::fonts::{Family, FontSpec};
use crate::fs_text;
use crate::markdown::document::{Block, Document, Scripts};
use crate::markdown::highlight::Highlighter;
use crate::markdown::preprocess::{heading_anchor_id, is_markdown_path, percent_decode};
use crate::markdown::source::Selected;
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
use crate::update::Updates;
use crate::watcher::FileWatcher;
use egui::{
    Align, CentralPanel, Color32, CursorIcon, FontData, FontDefinitions, FontId, Frame, Id, Key,
    KeyboardShortcut, LayerId, Layout, Margin, Modifiers, Order, Panel, PointerButton, Pos2, Rect,
    RichText, Sense, Stroke, StrokeKind, TextEdit, Theme, Ui, Vec2, ViewportBuilder,
    ViewportCommand, ViewportId, vec2,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// A change to the document of tab `tab`. Changes are written one at a
/// time in order, so an undo finds the file its change left.
pub struct EditJob {
    tab: u64,
    path: PathBuf,
    change: Change,
    /// Set for an undo: the digest the file must still have.
    expect: Option<u64>,
}

/// Work on a document's file, done in order on one thread.
enum FileJob {
    Edit(EditJob),
    /// Find a selection's Markdown, to copy it.
    Markdown {
        tab: u64,
        path: PathBuf,
        selection: Vec<Selected>,
    },
}

/// What could not be done to a tab's file, until the next load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failed {
    Tick,
    Highlight,
    Copy,
    Editor,
}

impl Failed {
    fn of(change: &Change) -> Failed {
        match change {
            Change::Tick(_) => Failed::Tick,
            Change::Highlight(_) | Change::Unhighlight(_) | Change::Restore(_) => Failed::Highlight,
        }
    }

    /// The word in the status line.
    fn status(self) -> &'static str {
        match self {
            Failed::Tick | Failed::Highlight => tr!("not saved", "не сохранено"),
            Failed::Copy => tr!("not copied", "не скопировано"),
            Failed::Editor => tr!("editor not started", "редактор не запущен"),
        }
    }

    /// What it says on hover, before the reason.
    fn what(self) -> &'static str {
        match self {
            Failed::Tick => tr!(
                "The task list item was not ticked:",
                "Пункт списка задач не отмечен:"
            ),
            Failed::Highlight => tr!(
                "The highlight was not saved:",
                "Выделение цветом не сохранено:"
            ),
            Failed::Copy => tr!("The Markdown was not copied:", "Markdown не скопирован:"),
            Failed::Editor => tr!(
                "The editor could not be started:",
                "Не удалось запустить редактор:"
            ),
        }
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
    /// A change was written to this file, or could not be.
    Written {
        job: EditJob,
        result: Result<Option<Written>, String>,
    },
    /// The Markdown of a selection in this tab, to copy.
    Markdown {
        tab: u64,
        result: Result<String, String>,
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
    /// A print or PDF asked for in window `window` is done.
    Printed {
        window: u64,
        pdf: bool,
        result: Result<crate::print::Outcome, String>,
    },
}

/// What printing says in the status line of the window it was asked in.
struct PrintStatus {
    window: u64,
    text: &'static str,
    /// On hover.
    detail: Option<String>,
    /// A PDF to open with a click.
    pdf: Option<PathBuf>,
    /// Shown until then; `None` while printing.
    until: Option<Instant>,
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
    let mut doc = Document::parse(&decoded.content);
    crate::markdown::embed::fill(&mut doc, path);
    Ok(Loaded {
        doc: Arc::new(doc),
        encoding: decoded.encoding,
        lossy: decoded.lossy,
    })
}

/// The Markdown of `selection` in the file at `path`, read again.
fn copy_markdown(path: &Path, selection: &[Selected]) -> Result<String, String> {
    let decoded = fs_text::read_to_string_lossy(path).map_err(|e| e.to_string())?;
    crate::markdown::source::markdown(&decoded.content, selection).map_err(|p| p.message())
}

/// Windows narrower than this, in points, show no table of contents.
const TOC_MIN_WINDOW: f32 = 560.0;

/// The Editor submenu of a tab: the editors found, the one in use
/// checked (with the program chosen by hand among them), and Other
/// Program… to choose one.
fn editor_menu(ui: &mut Ui, settings: &Settings) -> Option<TabMenu> {
    let current = crate::editor::editor(settings.editor.as_deref());
    let mut list = crate::editor::installed().to_vec();
    if !list.iter().any(|e| same_path(&e.exe, &current)) {
        list.insert(
            0,
            crate::editor::Editor {
                name: crate::editor::name(&current),
                exe: current.clone(),
            },
        );
    }
    let mut action = None;
    for editor in list {
        let chosen = same_path(&editor.exe, &current);
        if ui
            .radio(chosen, &editor.name)
            .on_hover_text(editor.exe.display().to_string())
            .clicked()
        {
            action = Some(TabMenu::SetEditor(editor.exe));
        }
    }
    ui.separator();
    if ui
        .button(tr!("Other program…", "Другая программа…"))
        .clicked()
    {
        action = Some(TabMenu::ChooseEditor);
    }
    action
}

/// Closed tabs Ctrl+Shift+T can open again.
const MAX_CLOSED_TABS: usize = 25;

/// The smallest window, in points.
const MIN_WINDOW_SIZE: [f32; 2] = [420.0, 300.0];

/// How long the window rests after a move or resize before it is saved.
const WINDOW_SAVE_DELAY: Duration = Duration::from_secs(1);

/// Frames a window paints, cloaked, before it is seen, and the longest the
/// main window stays cloaked.
const PAINTED_TO_SHOW: u64 = 3;
const UNCLOAK_TIMEOUT: Duration = Duration::from_secs(1);
/// Frames a detached window's native window is looked for before it is
/// shown as it is.
const LOOK_FRAMES: u32 = 10;

/// The title a detached window is made with, hidden, until it is found
/// (`App::appear`): unique, as two windows may show the same document.
fn hidden_title(id: u64) -> String {
    format!("MetalMD window {id}")
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
    /// What could not be done to the file and why, until the next load.
    failed: Option<(Failed, String)>,
    /// Changes made here, last last: each as the change that undoes it and
    /// the digest of the file it left.
    undo: Vec<(Change, u64)>,
    view: ViewState,
    /// Anchor to scroll to once the document has loaded.
    pending_anchor: Option<String>,
    /// Reading position to restore after a reload.
    restore: Option<(usize, f32)>,
    /// Positions to return to with Alt+Left.
    back: Vec<(usize, f32)>,
    /// Positions gone back from, to return to with Alt+Right.
    forward: Vec<(usize, f32)>,
    /// Reopened from the last run's session: if its file is gone, the tab
    /// closes instead of showing the error.
    restored: bool,
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
            failed: None,
            undo: Vec::new(),
            view: ViewState::default(),
            pending_anchor: None,
            restore: None,
            back: Vec::new(),
            forward: Vec::new(),
            restored: false,
        }
    }
}

impl Tab {
    /// Jumps to the top of `block` from a link or the contents, so that
    /// Alt+Left comes back here; the way forward starts anew.
    fn jump_from_here(&mut self, block: usize) {
        if let Some(position) = self.view.reading_position() {
            self.back.push(position);
        }
        self.forward.clear();
        self.view.jump_to(block, 8.0);
    }

    /// Goes one step back (`forward` false) or forward along the jumps.
    fn step(&mut self, forward: bool) {
        let (from, to) = if forward {
            (&mut self.forward, &mut self.back)
        } else {
            (&mut self.back, &mut self.forward)
        };
        let Some((block, offset)) = from.pop() else {
            return;
        };
        if let Some(position) = self.view.reading_position() {
            to.push(position);
        }
        self.view.jump_to(block, offset);
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
    /// A picture of the document shown on its own over the window.
    lightbox: Option<crate::ui::lightbox::Lightbox>,
    /// How a detached window is made; `None` for the root.
    builder: Option<ViewportBuilder>,
    /// How far a detached window is from being seen.
    appear: Appear,
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
            lightbox: None,
            appear: match builder {
                None => Appear::Seen,
                Some(_) => Appear::Looking(0),
            },
            builder,
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

/// A detached window on its way to the screen. Windows shows a new window
/// white until its first frame reaches DWM, so it is made hidden under
/// `hidden_title`, found by that title, cloaked, given its caption and
/// title, shown, and uncloaked once it has painted a few frames
/// (`App::appear`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Appear {
    /// Its native window not found yet: the frames looked.
    Looking(u32),
    /// Shown, cloaked if it was found: the frames painted since.
    Painting(u64),
    Seen,
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
    Edit(usize),
    /// Use this editor from now on.
    SetEditor(PathBuf),
    /// Ask for the editor's program.
    ChooseEditor,
    Detach(usize),
    ToRoot(usize),
    CloseOthers(usize),
}

/// What `main` prepared before the window existed.
pub struct Startup {
    /// The tabs with their ids; the loading of those not in `deferred` has
    /// already started.
    pub tabs: Vec<(u64, PathBuf)>,
    /// The tab shown.
    pub active: usize,
    /// Tabs to load from the second frame on.
    pub deferred: Vec<(u64, PathBuf)>,
    /// The tabs are the last run's session.
    pub restored: bool,
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
    /// The thread that changes documents and finds the Markdown of
    /// selections, started by the first job.
    file_worker: Option<Sender<FileJob>>,
    /// Another launch handed over files: bring the window they open in to
    /// the front. Set by the single-instance thread and taken in `logic`,
    /// which runs while the window is minimized too.
    raise: Arc<AtomicBool>,
    updates: Updates,
    /// Restored tabs whose loading waits for the second frame.
    deferred: Vec<(u64, PathBuf)>,
    /// Closed tabs' files, the last closed last, for Ctrl+Shift+T.
    closed_tabs: Vec<PathBuf>,
    /// Printing under way, or how the last one went.
    print_status: Option<PrintStatus>,
    /// The right button's press held back until its release.
    right_click: crate::ui::menu::RightClick,
    /// The main window is cloaked until it has painted a few frames (else
    /// Windows showed it white before its first frame reached DWM): the
    /// time it is shown anyway, the frame it was cloaked in, maximized.
    cloak: Option<(Instant, u64, bool)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, startup: Startup) -> App {
        let ctx = cc.egui_ctx.clone();
        let _ = startup.ctx_cell.set(ctx.clone());
        let mut bench = startup.bench;
        if let Some(b) = bench.as_mut() {
            b.mark("app_new");
        }

        style::apply_visuals(&ctx, startup.settings.dark_style);
        ctx.set_theme(startup.settings.theme.preference());
        // The window is still hidden (eframe shows it after the first
        // frame), so a caption set now is shown from the start; set after
        // egui's first frame, Windows 10 would keep the caption of the
        // Windows theme. Following Windows, winit has already done it.
        let hwnd = crate::caption::hwnd(cc);
        let mut cloak = None;
        if let Some(hwnd) = hwnd {
            // Cloaked until it has painted a few frames (`uncloak`), so it
            // is not seen white before the first one, nor while it is
            // being maximized.
            let maximized = startup.settings.window.is_some_and(|w| w.maximized);
            crate::caption::cloak(hwnd, true);
            cloak = Some((
                Instant::now() + UNCLOAK_TIMEOUT,
                ctx.cumulative_frame_nr(),
                maximized,
            ));
            crate::placement::keep_on_screen(hwnd);
            if maximized {
                crate::placement::show_maximized(hwnd);
            }
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
        style::set_content_width(startup.settings.text_width);
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
        ctx.add_plugin(crate::ui::selection::TextSelection::default());
        if let Some(b) = bench.as_mut() {
            b.mark("fonts_ready");
        }

        let raise = Arc::new(AtomicBool::new(false));
        if let Some(server) = startup.server {
            let sender = startup.sender.clone();
            let cell = startup.ctx_cell.clone();
            let raise = Arc::clone(&raise);
            server.spawn(move |paths| {
                let _ = sender.send(AppMsg::OpenPaths(paths));
                raise.store(true, Ordering::Release);
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
        root.tabs = startup
            .tabs
            .into_iter()
            .map(|(id, path)| Tab {
                restored: startup.restored,
                ..Tab::new(id, path)
            })
            .collect();
        root.active = startup.active.min(root.tabs.len().saturating_sub(1));

        // Checked, if due, from the second frame on.
        let updates = Updates::new(
            startup.settings.check_updates,
            startup.settings.last_update_check,
        );
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
            file_worker: None,
            raise,
            updates,
            deferred: startup.deferred,
            closed_tabs: Vec::new(),
            print_status: None,
            right_click: Default::default(),
            cloak,
        }
    }

    /// Shows the main window once it has painted `PAINTED_TO_SHOW` frames
    /// and, if it is to be, is maximized; after `UNCLOAK_TIMEOUT` anyway.
    fn uncloak(&mut self, ctx: &egui::Context) {
        let Some((until, frame, maximized)) = self.cloak else {
            return;
        };
        let ready = ctx.cumulative_frame_nr() >= frame + PAINTED_TO_SHOW
            && (!maximized || ctx.input(|i| i.viewport().maximized == Some(true)));
        if ready || Instant::now() >= until {
            if let Some(hwnd) = self.windows[0].hwnd {
                crate::caption::cloak(hwnd, false);
            }
            self.cloak = None;
        } else {
            ctx.request_repaint();
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

    /// Brings the current detached window to the screen (`Appear`), at the
    /// start of each of its frames (`ctx` is its viewport's). eframe gives
    /// no handle for a child viewport, so its window is found by the title
    /// it is made with; once found it is cloaked and gets the dark caption
    /// while still hidden, then its real title, and is shown. Not found
    /// within `LOOK_FRAMES`, it is shown as it is.
    fn appear(&mut self, ctx: &egui::Context) {
        let more = || {
            ctx.request_repaint();
            ctx.request_repaint_of(ViewportId::ROOT);
        };
        let cur = self.current;
        match self.windows[cur].appear {
            Appear::Looking(looked) => {
                let known: Vec<isize> = self.windows.iter().filter_map(|w| w.hwnd).collect();
                let found = crate::caption::thread_window(&hidden_title(self.windows[cur].id))
                    .filter(|h| !known.contains(h));
                let window = &mut self.windows[cur];
                if let Some(hwnd) = found {
                    crate::caption::cloak(hwnd, true);
                    crate::caption::set_dark(hwnd, ctx.theme() == Theme::Dark);
                    window.hwnd = Some(hwnd);
                } else if looked < LOOK_FRAMES {
                    window.appear = Appear::Looking(looked + 1);
                    more();
                    return;
                }
                window.appear = Appear::Painting(0);
                // The title before the window shows, so the taskbar never
                // has the one it was made with.
                self.update_title(ctx);
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                more();
            }
            Appear::Painting(painted) => {
                let window = &mut self.windows[cur];
                if painted + 1 >= PAINTED_TO_SHOW {
                    if let Some(hwnd) = window.hwnd {
                        crate::caption::cloak(hwnd, false);
                    }
                    window.appear = Appear::Seen;
                } else {
                    window.appear = Appear::Painting(painted + 1);
                }
                more();
            }
            Appear::Seen => {}
        }
    }

    /// Opens `paths` in the current window.
    fn open_paths(&mut self, paths: Vec<PathBuf>) {
        for path in paths {
            let (path, anchor) = (normalize_path(&path), None);
            self.open_path(path, anchor);
        }
    }

    /// Restores and focuses the window that files handed over by another
    /// launch open in (`AppMsg::OpenPaths`).
    fn raise_window(&self, ctx: &egui::Context) {
        let window = &self.windows[self.window_index(self.focused).unwrap_or(0)];
        #[cfg(windows)]
        if let Some(hwnd) = window.hwnd {
            crate::placement::bring_to_front(hwnd);
            return;
        }
        ctx.send_viewport_cmd_to(window.viewport, ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd_to(window.viewport, ViewportCommand::Focus);
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

    /// Lets go of a tab taken out of its window, keeping where it was read.
    fn drop_tab(&mut self, tab: Tab) {
        if tab.doc.is_some()
            && let Some(position) = tab.view.saved_position()
        {
            self.settings.set_position(&tab.path, position);
        }
        self.settings_dirty = true;
        if !tab.restored || tab.doc.is_some() {
            self.closed_tabs.retain(|p| !same_path(p, &tab.path));
            self.closed_tabs.push(tab.path.clone());
            if self.closed_tabs.len() > MAX_CLOSED_TABS {
                self.closed_tabs.remove(0);
            }
        }
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
        // Files may have moved in the vault since they were looked for.
        crate::vault::forget();
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
                        } else if tab.doc.is_none()
                            && let Some((block, offset)) = self.settings.position(&tab.path)
                        {
                            // Opened again: where it was left last time.
                            let last = loaded.doc.blocks.len().saturating_sub(1);
                            tab.view.jump_to(block.min(last), offset);
                        }
                        tab.doc = Some(loaded.doc);
                        tab.encoding = loaded.encoding;
                        tab.lossy = loaded.lossy;
                        tab.error = None;
                        tab.deleted = false;
                        tab.failed = None;
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
                        if tab.doc.is_none() && tab.restored && error.not_found {
                            // A file of the last session, gone since.
                            let path = tab.path.clone();
                            let tab = self.windows[w].remove_tab(index);
                            self.drop_tab(tab);
                            self.settings.remove_recent(&path);
                            self.settings_dirty = true;
                        } else if tab.doc.is_some() {
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
                self.open_paths(paths);
                self.current = current;
            }
            AppMsg::FileChanged(path) => self.reload_path(&path),
            // Reloaded at once rather than when the watcher notices.
            AppMsg::Written { job, result } => {
                let undo = job.expect.is_some();
                match result {
                    Ok(written) => {
                        if let (Some(written), false, Some((w, t))) =
                            (written, undo, self.find_tab(job.tab))
                        {
                            let tab = &mut self.windows[w].tabs[t];
                            tab.undo.push((written.undo, written.digest));
                            // What was selected is not there any more.
                            if !matches!(job.change, Change::Tick(_)) {
                                ctx.plugin::<crate::ui::selection::TextSelection>()
                                    .lock()
                                    .clear();
                            }
                        }
                        self.reload_path(&job.path);
                    }
                    Err(error) => {
                        let failed = Failed::of(&job.change);
                        for window in &mut self.windows {
                            for tab in &mut window.tabs {
                                if same_path(&tab.path, &job.path) {
                                    tab.failed = Some((failed, error.clone()));
                                    // The file is not the one the changes
                                    // were made in any more.
                                    tab.undo.clear();
                                }
                            }
                        }
                    }
                }
            }
            AppMsg::Markdown { tab, result } => match result {
                Ok(text) => ctx.copy_text(text),
                Err(error) => {
                    if let Some((w, t)) = self.find_tab(tab) {
                        self.windows[w].tabs[t].failed = Some((Failed::Copy, error));
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
            AppMsg::Printed {
                window,
                pdf,
                result,
            } => self.printed(window, pdf, result),
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
        for (id, path) in std::mem::take(&mut self.deferred) {
            spawn_load(self.sender.clone(), self.ctx_cell.clone(), id, 0, path);
        }
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
        if self.bench.is_none()
            && let Some(ctx) = self.ctx_cell.get()
        {
            self.updates.start_if_due(ctx);
        }
    }

    /// Takes the answer of a finished update check and keeps the update
    /// settings, changed by it or in About.
    fn follow_updates(&mut self) {
        self.updates.poll();
        let (enabled, last) = (self.updates.enabled, self.updates.last_check);
        if (enabled, last) != (self.settings.check_updates, self.settings.last_update_check) {
            self.settings.check_updates = enabled;
            self.settings.last_update_check = last;
            self.settings_dirty = true;
        }
    }

    fn open_dialog(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("Markdown", &["md", "markdown", "mdown", "mkd", "txt"])
            .add_filter(tr!("All files", "Все файлы"), &["*"]);
        if let Some(dir) = self.win().active_tab().and_then(|t| t.path.parent()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(paths) = dialog.pick_files() {
            self.open_paths(paths);
        }
    }

    /// Prints the active tab's document, or saves it as PDF where the
    /// reader chooses, on a thread of its own (`crate::print`).
    fn print_document(&mut self, ctx: &egui::Context, pdf: bool) {
        let dark = ctx.theme() == Theme::Dark;
        let window = &self.windows[self.current];
        let (window_id, owner) = (window.id, window.hwnd);
        let Some(tab) = window.active_tab() else {
            return;
        };
        let Some(doc) = tab.doc.clone() else { return };
        if self
            .print_status
            .as_ref()
            .is_some_and(|s| s.until.is_none())
        {
            // One at a time: the status line shows the one under way.
            return;
        }
        let output = if pdf {
            let mut dialog = rfd::FileDialog::new()
                .set_title(tr!("Save as PDF", "Сохранить как PDF"))
                .add_filter("PDF", &["pdf"])
                .set_file_name(crate::print::pdf_name(&tab.path));
            if let Some(dir) = tab.path.parent() {
                dialog = dialog.set_directory(dir);
            }
            let Some(mut path) = dialog.save_file() else {
                return;
            };
            if path.extension().is_none() {
                path.set_extension("pdf");
            }
            crate::print::Output::Pdf(path)
        } else {
            crate::print::Output::Printer
        };
        let light = Palette::new(false, self.settings.dark_style);
        let mark = self.settings.mark_color(false).unwrap_or(light.mark_bg);
        let page = crate::print::html::PageOptions {
            title: tab.title.clone(),
            base_dir: tab.path.parent().map(Path::to_path_buf),
            body_font: self.settings.font.as_ref().map(|f| f.family.clone()),
            mono_font: self.settings.mono_font.as_ref().map(|f| f.family.clone()),
            text_px: body_size(),
            column_px: tab
                .view
                .column_width()
                .unwrap_or(crate::style::DEFAULT_CONTENT_WIDTH),
            mark_bg: [mark.r(), mark.g(), mark.b()],
            toggled_details: tab.view.toggled_details().clone(),
            print_on_load: false,
        };
        self.print_status = Some(PrintStatus {
            window: window_id,
            text: if pdf {
                tr!("saving PDF…", "сохраняется PDF…")
            } else {
                tr!("printing…", "печать…")
            },
            detail: None,
            pdf: None,
            until: None,
        });
        let request = crate::print::Request {
            doc,
            page,
            output,
            owner,
            dark,
        };
        let sender = self.sender.clone();
        let cell = self.ctx_cell.clone();
        crate::print::spawn(request, move |result| {
            let _ = sender.send(AppMsg::Printed {
                window: window_id,
                pdf,
                result,
            });
            request_repaint(&cell);
        });
    }

    /// Shows how a print or PDF went in the status line.
    fn printed(&mut self, window: u64, pdf: bool, result: Result<crate::print::Outcome, String>) {
        use crate::print::Outcome;
        let status = |text, detail, pdf, seconds| PrintStatus {
            window,
            text,
            detail,
            pdf,
            until: Some(Instant::now() + Duration::from_secs(seconds)),
        };
        self.print_status = match result {
            Ok(Outcome::Printed) => None,
            Ok(Outcome::Saved(path)) => Some(status(
                tr!("PDF saved", "PDF сохранён"),
                Some(tr!(
                    format!("{}\nClick to open it.", path.display()),
                    format!("{}\nЩёлкните, чтобы открыть.", path.display())
                )),
                Some(path),
                8,
            )),
            Ok(Outcome::InBrowser) => Some(status(
                tr!("opened in the browser", "открыто в браузере"),
                Some(
                    tr!(
                        "WebView2 is not installed, so the document was opened in the browser. Print it or save it as PDF from the browser's print dialog.",
                        "WebView2 не установлен, поэтому документ открыт в браузере. Напечатайте его или сохраните в PDF из диалога печати браузера."
                    )
                    .to_owned(),
                ),
                None,
                15,
            )),
            Err(error) => Some(status(
                if pdf {
                    tr!("PDF not saved", "PDF не сохранён")
                } else {
                    tr!("not printed", "не напечатано")
                },
                Some(error),
                None,
                15,
            )),
        };
    }

    fn handle_action(&mut self, ctx: &egui::Context, action: ViewAction) {
        let target = match action {
            ViewAction::OpenLink(target) => target,
            ViewAction::ToggleTask(task) => {
                self.edit(Change::Tick(task));
                return;
            }
            ViewAction::ShowPicture(shown) => {
                self.win().lightbox = Some(crate::ui::lightbox::Lightbox::new(shown));
                return;
            }
            ViewAction::CopyMarkdown => {
                if let Some(tab) = self.win().active_tab()
                    && !tab.view.selection().is_empty()
                {
                    let job = FileJob::Markdown {
                        tab: tab.id,
                        path: tab.path.clone(),
                        selection: tab.view.selection().to_vec(),
                    };
                    self.send_file_job(job);
                }
                return;
            }
            ViewAction::Highlight => {
                if let Some(tab) = self.win().active_tab()
                    && !tab.view.selection().is_empty()
                {
                    let selection = tab.view.selection().to_vec();
                    self.edit(Change::Highlight(selection));
                }
                return;
            }
            ViewAction::Unhighlight => {
                if let Some(tab) = self.win().active_tab() {
                    let selection = match tab.view.selection() {
                        [] => tab.view.menu_point().map(|p| vec![p.at.clone()]),
                        selection => Some(selection.to_vec()),
                    };
                    if let Some(selection) = selection {
                        self.edit(Change::Unhighlight(selection));
                    }
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
                tab.jump_from_here(block);
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
            let here = base.join(candidate);
            if here.exists() {
                here
            } else {
                // Obsidian finds a note anywhere in its vault by name.
                crate::vault::locate_now(&base, &decoded).unwrap_or(here)
            }
        } else {
            return;
        };
        let _ = ctx;
        self.open_link_target(if is_markdown_path(&resolved.to_string_lossy()) {
            LinkTarget::Markdown(resolved, fragment)
        } else {
            LinkTarget::File(resolved)
        });
    }

    /// Makes `change` in the active tab's file.
    fn edit(&mut self, change: Change) {
        if let Some(tab) = self.win().active_tab() {
            let job = EditJob {
                tab: tab.id,
                path: tab.path.clone(),
                change,
                expect: None,
            };
            self.send_file_job(FileJob::Edit(job));
        }
    }

    /// Changes a document's file or reads it, off the UI thread: the file
    /// may be on a slow disk or another computer.
    fn send_file_job(&mut self, job: FileJob) {
        let worker = self.file_worker.get_or_insert_with(|| {
            let (jobs, queue) = std::sync::mpsc::channel::<FileJob>();
            let sender = self.sender.clone();
            let cell = self.ctx_cell.clone();
            std::thread::Builder::new()
                .name("file".into())
                // The parse that finds places recurses once per level.
                .stack_size(16 * 1024 * 1024)
                .spawn(move || {
                    for job in queue {
                        let message = match job {
                            FileJob::Edit(job) => {
                                let result = crate::edit::apply(&job.path, &job.change, job.expect);
                                AppMsg::Written { job, result }
                            }
                            FileJob::Markdown {
                                tab,
                                path,
                                selection,
                            } => AppMsg::Markdown {
                                tab,
                                result: copy_markdown(&path, &selection),
                            },
                        };
                        let _ = sender.send(message);
                        request_repaint(&cell);
                    }
                })
                .expect("spawn file thread");
            jobs
        });
        let _ = worker.send(job);
    }

    /// Undoes the last change made in the active tab (Ctrl+Z).
    fn undo_edit(&mut self) {
        let Some(tab) = self.win().active_tab_mut() else {
            return;
        };
        let Some((change, digest)) = tab.undo.pop() else {
            return;
        };
        let job = EditJob {
            tab: tab.id,
            path: tab.path.clone(),
            change,
            expect: Some(digest),
        };
        self.send_file_job(FileJob::Edit(job));
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

    /// Alt+Left and Alt+Right: back and forward along the jumps.
    fn go_back(&mut self, forward: bool) {
        if let Some(tab) = self.win().active_tab_mut() {
            tab.step(forward);
        }
    }

    /// Opens tab `index`'s file in the editor, at the line of the first
    /// block in view.
    fn open_in_editor(&mut self, index: usize) {
        let exe = crate::editor::editor(self.settings.editor.as_deref());
        let Some(tab) = self.win().tabs.get_mut(index) else {
            return;
        };
        let line = tab
            .doc
            .as_ref()
            .zip(tab.view.reading_position())
            .and_then(|(doc, (block, _))| doc.lines.get(block).copied())
            .unwrap_or(1);
        if let Err(error) = crate::editor::open(&exe, &tab.path, line) {
            tab.failed = Some((Failed::Editor, error));
        }
    }

    /// Asks for the editor's program.
    fn choose_editor(&mut self) {
        let current = crate::editor::editor(self.settings.editor.as_deref());
        let mut dialog = rfd::FileDialog::new()
            .set_title(tr!("Choose the editor", "Выбор редактора"))
            .add_filter(tr!("Programs", "Программы"), &["exe"]);
        if let Some(dir) = current.parent() {
            dialog = dialog.set_directory(dir);
        }
        if let Some(exe) = dialog.pick_file() {
            self.settings.editor = Some(exe);
            self.settings_dirty = true;
        }
    }

    /// Ctrl+Shift+T: opens the tab closed last, where it was left.
    fn reopen_closed_tab(&mut self) {
        while let Some(path) = self.closed_tabs.pop() {
            // One open since is only shown.
            if self.all_tabs().any(|t| same_path(&t.path, &path)) {
                continue;
            }
            self.open_path(path, None);
            return;
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
            TextChange::Width(width) => {
                style::set_content_width(width);
                self.settings.text_width = width;
                self.settings_dirty = true;
            }
            TextChange::MarkColor { dark, color } => {
                let chosen = if dark {
                    &mut self.settings.mark_color_dark
                } else {
                    &mut self.settings.mark_color
                };
                *chosen = color.map(style::to_hex);
                self.settings_dirty = true;
            }
            TextChange::Reset => {
                self.font_request += 1;
                self.settings.font = None;
                self.settings.mono_font = None;
                self.settings.mark_color = None;
                self.settings.mark_color_dark = None;
                style::set_body_size(style::DEFAULT_BODY_SIZE);
                self.settings.text_size = style::DEFAULT_BODY_SIZE;
                style::set_content_width(style::DEFAULT_CONTENT_WIDTH);
                self.settings.text_width = style::DEFAULT_CONTENT_WIDTH;
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

        if self.win().lightbox.is_some() {
            // It takes the keys and the wheel from the document behind.
            if consume(plain(Key::Escape)) {
                self.win().lightbox = None;
            }
            return;
        }
        self.zoom_with_wheel(ctx);
        if consume(plain(Key::F1)) {
            self.toggle_about();
        }
        let fullscreen = ctx.input(|i| i.viewport().fullscreen == Some(true));
        if consume(plain(Key::F11)) {
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!fullscreen));
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
            self.open_dialog();
        }
        if consume(command(Key::P)) {
            self.print_document(ctx, false);
        }
        if consume(command(Key::E)) {
            let active = self.win().active;
            self.open_in_editor(active);
        }
        if consume(command_shift(Key::S)) {
            self.print_document(ctx, true);
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
            let alt = |key| KeyboardShortcut::new(Modifiers::ALT, key);
            let (case, words) = (consume(alt(Key::C)), consume(alt(Key::W)));
            if case || words {
                // Windows also types the letter (in the keyboard's layout),
                // which would go into the query.
                ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Text(_))));
            }
            if case {
                self.settings.find_case = !self.settings.find_case;
                self.settings_dirty = true;
            }
            if words {
                self.settings.find_words = !self.settings.find_words;
                self.settings_dirty = true;
            }
        }
        if consume(KeyboardShortcut::new(Modifiers::ALT, Key::ArrowLeft))
            || ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Extra1))
        {
            self.go_back(false);
        }
        if consume(KeyboardShortcut::new(Modifiers::ALT, Key::ArrowRight))
            || ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Extra2))
        {
            self.go_back(true);
        }
        if consume(command_shift(Key::T)) {
            self.reopen_closed_tab();
        }

        // Esc leaves full screen too, unless it closed find above.
        if fullscreen && consume(plain(Key::Escape)) {
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
        }

        // Reading keys, unless a text field has the keyboard.
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        if consume(command(Key::Z)) {
            self.undo_edit();
        }
        if consume(command_shift(Key::C)) {
            // Not copied as text too, which egui would do for Ctrl+C.
            ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Copy)));
            self.handle_action(ctx, ViewAction::CopyMarkdown);
        }
        if consume(command(Key::H)) {
            self.handle_action(ctx, ViewAction::Highlight);
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
        let mut print_choice = None;
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
                    let printable = self.windows[cur]
                        .active_tab()
                        .is_some_and(|t| t.doc.is_some());
                    print_choice = crate::ui::print_button(ui, printable);
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
                        crate::ui::menu::context_menu(&response, |ui| {
                            let edit =
                                egui::Button::new(tr!("Open in the editor", "Открыть в редакторе"))
                                    .shortcut_text("Ctrl+E");
                            if ui.add(edit).clicked() {
                                menu_action = Some(TabMenu::Edit(i));
                                ui.close();
                            }
                            ui.menu_button(tr!("Editor", "Редактор"), |ui| {
                                if let Some(action) = editor_menu(ui, &self.settings) {
                                    menu_action = Some(action);
                                    ui.close();
                                }
                            });
                            ui.separator();
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
                        if let Some(tag) = self.updates.newer().map(str::to_owned)
                            && crate::ui::update_button(ui, &tag).clicked()
                        {
                            let _ = open::that_detached(crate::update::release_url(&tag));
                        }
                        let theme = self.settings.theme;
                        let theme_button = crate::ui::theme_button(ui, theme);
                        if theme_button.clicked() {
                            self.settings.theme = theme.next();
                            ui.ctx().set_theme(self.settings.theme.preference());
                            self.settings_dirty = true;
                        }
                        if crate::ui::dark_style_menu(&theme_button, &mut self.settings.dark_style)
                        {
                            style::apply_visuals(ui.ctx(), self.settings.dark_style);
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
                            if let Some((failed, _)) = tab.failed {
                                status.push(failed.status().to_owned());
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
                                if let Some((failed, error)) = &tab.failed {
                                    label.on_hover_text(format!("{}\n{error}", failed.what()));
                                }
                            }
                        }
                        self.print_status_label(ui, palette, wid);
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
            Some(TabMenu::Edit(i)) => self.open_in_editor(i),
            Some(TabMenu::SetEditor(exe)) => {
                self.settings.editor = Some(exe);
                self.settings_dirty = true;
            }
            Some(TabMenu::ChooseEditor) => self.choose_editor(),
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
            self.open_dialog();
        }
        if let Some(choice) = print_choice {
            let ctx = ui.ctx().clone();
            self.print_document(&ctx, choice == crate::ui::PrintChoice::Pdf);
        }
    }

    /// What printing says, in the window it was asked in: while under way,
    /// then for a few seconds. A saved PDF opens with a click.
    fn print_status_label(&mut self, ui: &mut Ui, palette: &Palette, window: u64) {
        let Some(status) = self.print_status.as_ref() else {
            return;
        };
        if let Some(until) = status.until {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.print_status = None;
                return;
            }
            ui.ctx().request_repaint_after(left);
        }
        if status.window != window {
            return;
        }
        let mut label =
            egui::Label::new(RichText::new(status.text).size(12.0).color(palette.muted));
        if status.pdf.is_some() {
            label = label.sense(Sense::click());
        }
        let mut response = ui.add(label);
        if let Some(detail) = &status.detail {
            response = response.on_hover_text(detail);
        }
        if let Some(pdf) = status.pdf.clone() {
            if response.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if response.clicked() {
                std::thread::spawn(move || open::that_detached(pdf));
                self.print_status = None;
            }
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
        // Made hidden, under a title of its own, until found (`appear`).
        let builder = ViewportBuilder::default()
            .with_visible(false)
            .with_title(hidden_title(id))
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
                    let toggle = |ui: &mut Ui, on: bool, text: RichText, tip: &str| {
                        ui.add(egui::Button::selectable(on, text.size(13.0)))
                            .on_hover_text(tip)
                            .clicked()
                    };
                    let case_tip = tr!("Match case (Alt+C)", "Учитывать регистр (Alt+C)");
                    if toggle(ui, self.settings.find_case, RichText::new("Aa"), case_tip) {
                        self.settings.find_case = !self.settings.find_case;
                        self.settings_dirty = true;
                    }
                    let words_tip = tr!("Whole words (Alt+W)", "Слово целиком (Alt+W)");
                    let words = RichText::new("ab").underline();
                    if toggle(ui, self.settings.find_words, words, words_tip) {
                        self.settings.find_words = !self.settings.find_words;
                        self.settings_dirty = true;
                    }
                    let options = crate::ui::find::FindOptions {
                        case: self.settings.find_case,
                        words: self.settings.find_words,
                    };
                    if window.find.options != options {
                        window.find.options = options;
                        window.find.current = 0;
                        window.find_jump = true;
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
        // Not while the window is cloaked: it may not be in its place yet.
        if self.cloak.is_some() {
            return;
        }
        // egui reports the window in zoomed points, and the next start
        // creates it before the zoom is set: kept unzoomed, it would shrink
        // by the zoom at each start.
        let zoom = ctx.zoom_factor();
        let geometry = ctx.input(|i| {
            let viewport = i.viewport();
            // Full screen is not a size to come back to.
            if viewport.fullscreen == Some(true) {
                return None;
            }
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
            self.remember_session();
            self.settings.save_in_background();
        }
    }

    /// Puts the open tabs and where each is read into the settings: the
    /// session the next start without files opens again.
    fn remember_session(&mut self) {
        let mut session = Vec::new();
        let mut active = 0;
        for (w, window) in self.windows.iter().enumerate() {
            for (i, tab) in window.tabs.iter().enumerate() {
                if w == 0 && i == window.active {
                    active = session.len();
                }
                session.push(tab.path.clone());
            }
        }
        // The tab in front last, so that it is first in the list.
        let tabs: Vec<(PathBuf, (usize, f32))> = self
            .all_tabs()
            .filter(|t| t.doc.is_some())
            .filter_map(|t| Some((t.path.clone(), t.view.saved_position()?)))
            .collect();
        for (path, position) in tabs {
            self.settings.set_position(&path, position);
        }
        if let Some(tab) = self.windows[0].active_tab()
            && tab.doc.is_some()
            && let Some(position) = tab.view.saved_position()
        {
            let path = tab.path.clone();
            self.settings.set_position(&path, position);
        }
        self.settings.session = session;
        self.settings.session_active = active;
    }

    /// One frame of the current window.
    fn window_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let cur = self.current;
        if cur != 0 {
            self.appear(&ctx);
        }
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
            self.open_paths(dropped);
        }
        self.track_drag(&ctx);
        self.handle_shortcuts(&ctx);

        let dark = ctx.theme() == Theme::Dark;
        let mut palette = Palette::new(dark, self.settings.dark_style);
        if let Some(color) = self.settings.mark_color(dark) {
            palette.mark_bg = color;
        }
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
                        tab.jump_from_here(block);
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
            self.about
                .show(&ctx, &mut self.settings.language, &mut self.updates);
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
        if let Some(lightbox) = self.windows[cur].lightbox.as_mut()
            && !lightbox.show(&ctx, &self.pictures, &palette)
        {
            self.windows[cur].lightbox = None;
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
            Some(HomeAction::Open(path)) => self.open_paths(vec![path]),
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

        // A window not found yet keeps the title it is looked for by.
        if !matches!(self.windows[cur].appear, Appear::Looking(_)) {
            self.update_title(&ctx);
        }
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.right_click.hook(&mut raw_input.events);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.raise.swap(false, Ordering::Acquire) {
            self.raise_window(ctx);
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.frame += 1;
        self.uncloak(&ctx);
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
        self.follow_updates();

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
            i += 1;
        }
        self.current = 0;
        self.close_windows();

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
            self.remember_session();
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
pub(crate) fn has_url_scheme(target: &str) -> bool {
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
