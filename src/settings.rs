//! User settings and recent files, stored as JSON in the config directory.

use crate::fonts::FontSpec;
use crate::i18n::LangChoice;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

pub const MAX_RECENT: usize = 9;

/// Documents whose reading position is kept, the last read first.
pub const MAX_POSITIONS: usize = 100;

/// The number of the last save asked for, and of the last one written.
static SAVES: AtomicU64 = AtomicU64::new(0);
static WRITTEN: Mutex<u64> = Mutex::new(0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeSetting {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeSetting {
    pub fn next(self) -> ThemeSetting {
        match self {
            ThemeSetting::System => ThemeSetting::Light,
            ThemeSetting::Light => ThemeSetting::Dark,
            ThemeSetting::Dark => ThemeSetting::System,
        }
    }

    pub fn preference(self) -> egui::ThemePreference {
        match self {
            ThemeSetting::System => egui::ThemePreference::System,
            ThemeSetting::Light => egui::ThemePreference::Light,
            ThemeSetting::Dark => egui::ThemePreference::Dark,
        }
    }
}

/// Which of the two dark themes the dark one is, chosen in the context menu
/// of the theme button.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DarkStyle {
    /// Neutral greys, as metalmpv.
    #[default]
    Grey,
    /// GitHub's, nearly black.
    Black,
}

/// The side of the window the table of contents is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TocSide {
    #[default]
    Left,
    Right,
}

/// What the wheel does in the picture viewer, as the Image Viewer
/// plugin's option: zoom (the arrow keys go to the next picture), or go to
/// the next picture (Ctrl+wheel zooms).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WheelMode {
    #[default]
    Zoom,
    Navigate,
}

/// How a picture is scaled when the viewer shows it (the plugin's default
/// zoom): fitted without being enlarged, fitted, filling the height or the
/// width, or at its own size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StartZoom {
    #[default]
    FitDown,
    Fit,
    Height,
    Width,
    Actual,
}

/// The shape of the picture viewer's thumbnails, as the plugin's option.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThumbShape {
    #[default]
    Landscape,
    Portrait,
    Square,
}

/// The picture viewer's options (`ui::lightbox`), set in its context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewerSettings {
    /// The strip of the document's pictures along the bottom.
    pub thumbnails: bool,
    pub thumb_shape: ThumbShape,
    /// The thumbnails' height in points, 40 to 160 (the plugin's range).
    pub thumb_height: u16,
    /// The ‹ › buttons beside the picture.
    pub arrows: bool,
    pub wheel: WheelMode,
    pub start_zoom: StartZoom,
}

impl Default for ViewerSettings {
    fn default() -> Self {
        ViewerSettings {
            thumbnails: true,
            thumb_shape: ThumbShape::Landscape,
            thumb_height: 64,
            arrows: true,
            wheel: WheelMode::Zoom,
            start_zoom: StartZoom::FitDown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

/// Where a document was left: its first visible top-level block and how
/// far below the top of the view that block started (negative once
/// scrolled into it), as `ViewState::reading_position` gives it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadingPosition {
    pub path: PathBuf,
    pub block: usize,
    pub offset: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeSetting,
    pub dark_style: DarkStyle,
    pub zoom: f32,
    pub toc_visible: bool,
    pub toc_side: TocSide,
    /// Interface language, chosen in About.
    pub language: LangChoice,
    pub recent: Vec<PathBuf>,
    pub window: Option<WindowGeometry>,
    /// The document's text font, chosen in the Text panel; `None` for the
    /// platform's.
    pub font: Option<FontSpec>,
    /// The font of code, chosen in the Text panel.
    pub mono_font: Option<FontSpec>,
    /// Body text size in points.
    pub text_size: f32,
    /// Widest the text column gets, in points; the end of
    /// `style::CONTENT_WIDTHS` for the whole window.
    pub text_width: f32,
    /// The background of highlighted text (`==…==`) in the light theme, as
    /// `#rrggbb`, chosen in the Text panel; `None` for the theme's own.
    pub mark_color: Option<String>,
    /// The same in the dark theme.
    pub mark_color_dark: Option<String>,
    /// Check for a newer release at start-up, at most once a day; off
    /// until turned on in About.
    pub check_updates: bool,
    /// Unix seconds of the last update check that got an answer.
    pub last_update_check: u64,
    /// The tabs open when the app was last closed, in the order of their
    /// windows; opened again when it starts without files.
    pub session: Vec<PathBuf>,
    /// Which of `session` was shown in the main window.
    pub session_active: usize,
    /// Where recently read documents were left, the last read first.
    pub positions: Vec<ReadingPosition>,
    /// The editor Ctrl+E opens documents in, chosen in a tab's context
    /// menu; `None` for the first known one installed.
    pub editor: Option<PathBuf>,
    /// Find matches case (Alt+C in the find bar).
    pub find_case: bool,
    /// Find matches whole words only (Alt+W).
    pub find_words: bool,
    /// The picture viewer's options.
    pub viewer: ViewerSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeSetting::System,
            dark_style: DarkStyle::Grey,
            zoom: 1.0,
            toc_visible: true,
            toc_side: TocSide::Left,
            language: LangChoice::System,
            recent: Vec::new(),
            window: None,
            font: None,
            mono_font: None,
            text_size: crate::style::DEFAULT_BODY_SIZE,
            text_width: crate::style::DEFAULT_CONTENT_WIDTH,
            mark_color: None,
            mark_color_dark: None,
            check_updates: false,
            last_update_check: 0,
            session: Vec::new(),
            session_active: 0,
            positions: Vec::new(),
            editor: None,
            find_case: false,
            find_words: false,
            viewer: ViewerSettings::default(),
        }
    }
}

impl Settings {
    /// The highlight colour chosen for the dark or the light theme, `None`
    /// for the theme's own (or one that is not `#rrggbb`).
    pub fn mark_color(&self, dark: bool) -> Option<egui::Color32> {
        let chosen = if dark {
            &self.mark_color_dark
        } else {
            &self.mark_color
        };
        chosen.as_deref().and_then(crate::style::parse_hex)
    }
}

/// Settings file that makes a copy portable: when it lies next to the exe
/// (even empty) and can be written, the settings are kept in it instead of
/// the user profile. The portable zip ships an empty one.
pub const PORTABLE_FILE: &str = "metalmd.json";

/// Where the settings of this run are kept.
#[derive(Debug)]
pub struct Location {
    /// The settings file, `None` if there is nowhere to keep it.
    pub file: Option<PathBuf>,
    /// `file` is [`PORTABLE_FILE`] next to the exe.
    pub portable: bool,
}

/// Where the settings are kept, decided once per run.
pub fn location() -> &'static Location {
    static LOCATION: OnceLock<Location> = OnceLock::new();
    LOCATION.get_or_init(|| {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_path_buf));
        match exe_dir.and_then(|d| portable_file_in(&d)) {
            Some(file) => Location {
                file: Some(file),
                portable: true,
            },
            None => Location {
                file: directories::ProjectDirs::from("", "", "MetalMD")
                    .map(|dirs| dirs.config_dir().join("settings.json")),
                portable: false,
            },
        }
    })
}

/// [`PORTABLE_FILE`] in `dir` if it is there and can be written; a copy on
/// a read-only medium keeps its settings in the profile instead of losing
/// them.
fn portable_file_in(dir: &Path) -> Option<PathBuf> {
    let file = dir.join(PORTABLE_FILE);
    // Opening for appending checks the permission without changing the file.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .ok()
        .map(|_| file)
}

impl Settings {
    /// Reads the settings file; missing, empty or malformed files give
    /// defaults.
    pub fn load() -> Settings {
        location()
            .file
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .map(|mut settings: Settings| {
                // What egui's zoom keys reach: 0.2 to 5.
                if !(0.2..=5.0).contains(&settings.zoom) {
                    settings.zoom = 1.0;
                }
                if !crate::style::BODY_SIZES.contains(&settings.text_size) {
                    settings.text_size = crate::style::DEFAULT_BODY_SIZE;
                }
                if !crate::style::CONTENT_WIDTHS.contains(&settings.text_width) {
                    settings.text_width = crate::style::DEFAULT_CONTENT_WIDTH;
                }
                settings
            })
            .unwrap_or_default()
    }

    pub fn save(&self) {
        self.save_numbered(SAVES.fetch_add(1, Ordering::SeqCst) + 1);
    }

    /// Saves a copy on a background thread. Saves are numbered when asked
    /// for, so one asked for later, here or in [`Settings::save`], wins even
    /// if this one finishes after it.
    pub fn save_in_background(&self) {
        let number = SAVES.fetch_add(1, Ordering::SeqCst) + 1;
        let settings = self.clone();
        std::thread::spawn(move || settings.save_numbered(number));
    }

    /// Writes the file unless a later save was written already. One writer
    /// at a time: they share the temporary file.
    fn save_numbered(&self, number: u64) {
        let Some(path) = &location().file else {
            return;
        };
        let mut written = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
        if *written > number {
            return;
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, path).is_ok() {
                *written = number;
            }
        }
    }

    /// Moves `path` to the front of the recent list.
    pub fn add_recent(&mut self, path: &Path) {
        self.recent.retain(|p| !same_path(p, path));
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(MAX_RECENT);
    }

    pub fn remove_recent(&mut self, path: &Path) {
        self.recent.retain(|p| !same_path(p, path));
    }

    /// Where the document at `path` was left, if it was read past its top.
    pub fn position(&self, path: &Path) -> Option<(usize, f32)> {
        self.positions
            .iter()
            .find(|p| same_path(&p.path, path))
            .map(|p| (p.block, p.offset))
    }

    /// Keeps where the document at `path` was left, first in the list; a
    /// document left at its top is forgotten.
    pub fn set_position(&mut self, path: &Path, position: (usize, f32)) {
        self.positions.retain(|p| !same_path(&p.path, path));
        let (block, offset) = position;
        if block == 0 && offset >= 0.0 {
            return;
        }
        self.positions.insert(
            0,
            ReadingPosition {
                path: path.to_path_buf(),
                block,
                offset: offset.round(),
            },
        );
        self.positions.truncate(MAX_POSITIONS);
    }
}

/// Path equality as the file system sees it (case-insensitive on Windows).
pub fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.as_os_str().to_string_lossy().to_lowercase()
            == b.as_os_str().to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_colours_are_kept_per_theme_as_hex() {
        let red = egui::Color32::from_rgb(0xff, 0x10, 0x0a);
        let settings = Settings {
            mark_color: Some(crate::style::to_hex(red)),
            mark_color_dark: Some("not a colour".to_owned()),
            ..Settings::default()
        };
        assert_eq!(settings.mark_color.as_deref(), Some("#ff100a"));
        assert_eq!(settings.mark_color(false), Some(red));
        assert_eq!(settings.mark_color(true), None);
        assert_eq!(
            crate::style::parse_hex("D4E9FF"),
            Some(egui::Color32::from_rgb(0xd4, 0xe9, 0xff))
        );
        assert_eq!(crate::style::parse_hex("#ффф"), None);
    }

    #[test]
    fn recent_files_are_deduplicated_and_capped() {
        let mut settings = Settings::default();
        for i in 0..12 {
            settings.add_recent(Path::new(&format!("/docs/{i}.md")));
        }
        settings.add_recent(Path::new("/docs/5.md"));
        assert_eq!(settings.recent.len(), MAX_RECENT);
        assert_eq!(settings.recent[0], PathBuf::from("/docs/5.md"));
        assert_eq!(
            settings
                .recent
                .iter()
                .filter(|p| p.ends_with("5.md"))
                .count(),
            1
        );
    }

    #[test]
    fn reading_positions_are_kept_last_first_and_capped() {
        let mut settings = Settings::default();
        for i in 0..MAX_POSITIONS + 5 {
            settings.set_position(Path::new(&format!("/docs/{i}.md")), (i + 1, -20.4));
        }
        assert_eq!(settings.positions.len(), MAX_POSITIONS);
        settings.set_position(Path::new("/docs/50.md"), (7, 8.0));
        assert_eq!(settings.positions[0].path, PathBuf::from("/docs/50.md"));
        assert_eq!(settings.position(Path::new("/docs/50.md")), Some((7, 8.0)));
        assert_eq!(
            settings.position(Path::new(&format!("/docs/{}.md", MAX_POSITIONS + 4))),
            Some((MAX_POSITIONS + 5, -20.0))
        );
        // Back at the top: nothing to keep.
        settings.set_position(Path::new("/docs/50.md"), (0, 8.0));
        assert_eq!(settings.position(Path::new("/docs/50.md")), None);
    }

    #[test]
    fn portable_file_only_when_present_and_writable() {
        let dir = std::env::temp_dir().join(format!("qmd_portable_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(PORTABLE_FILE);
        assert_eq!(portable_file_in(&dir), None);
        std::fs::write(&file, "").unwrap();
        assert_eq!(portable_file_in(&dir), Some(file.clone()));
        let mut perm = std::fs::metadata(&file).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&file, perm.clone()).unwrap();
        assert_eq!(portable_file_in(&dir), None);
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        std::fs::set_permissions(&file, perm).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn settings_round_trip_and_tolerate_unknown_fields() {
        let settings = Settings {
            theme: ThemeSetting::Dark,
            dark_style: DarkStyle::Black,
            zoom: 1.25,
            toc_side: TocSide::Right,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), settings);
        let partial: Settings = serde_json::from_str(r#"{"theme":"light","future":1}"#).unwrap();
        assert_eq!(partial.theme, ThemeSetting::Light);
        assert_eq!(partial.zoom, 1.0);
        assert_eq!(partial.toc_side, TocSide::Left);
    }

    #[test]
    fn settings_of_version_0_1_take_defaults_for_what_came_later() {
        let old = r#"{
            "theme": "dark",
            "zoom": 1.1,
            "toc_visible": false,
            "toc_side": "right",
            "language": "ru",
            "recent": ["/notes/a.md"],
            "window": {"x": 10.0, "y": 20.0, "width": 900.0, "height": 700.0, "maximized": false}
        }"#;
        let settings: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(settings.theme, ThemeSetting::Dark);
        assert_eq!(settings.dark_style, DarkStyle::Grey);
        assert_eq!(settings.toc_side, TocSide::Right);
        assert_eq!(settings.recent, [PathBuf::from("/notes/a.md")]);
        assert_eq!(settings.font, None);
        assert_eq!(settings.mono_font, None);
        assert_eq!(settings.text_size, crate::style::DEFAULT_BODY_SIZE);
        assert!(!settings.check_updates);
        assert_eq!(settings.last_update_check, 0);
    }
}
