//! User settings and recent files, stored as JSON in the config directory.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MAX_RECENT: usize = 9;

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

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeSetting,
    pub zoom: f32,
    pub toc_visible: bool,
    pub recent: Vec<PathBuf>,
    pub window: Option<WindowGeometry>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeSetting::System,
            zoom: 1.0,
            toc_visible: true,
            recent: Vec::new(),
            window: None,
        }
    }
}

/// Where the settings are kept.
pub fn settings_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "qmarkdown")
        .map(|dirs| dirs.config_dir().join("settings.json"))
}

impl Settings {
    /// Reads the settings file; missing or malformed files give defaults.
    pub fn load() -> Settings {
        settings_path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .map(|mut settings: Settings| {
                if !(0.3..=5.0).contains(&settings.zoom) {
                    settings.zoom = 1.0;
                }
                settings
            })
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = settings_path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
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
    fn settings_round_trip_and_tolerate_unknown_fields() {
        let settings = Settings {
            theme: ThemeSetting::Dark,
            zoom: 1.25,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), settings);
        let partial: Settings = serde_json::from_str(r#"{"theme":"light","future":1}"#).unwrap();
        assert_eq!(partial.theme, ThemeSetting::Light);
        assert_eq!(partial.zoom, 1.0);
    }
}
