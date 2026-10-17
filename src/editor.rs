//! Opening a document in a text editor (Ctrl+E) at the line being read.
//!
//! The editor is the one chosen in the tab's context menu, or else the
//! first of the known ones installed (VS Code, Cursor, Notepad++, Sublime
//! Text), or Notepad. Each known editor is told the line its own way.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The editor to use: `chosen` if it is still there, else one found.
pub fn editor(chosen: Option<&Path>) -> PathBuf {
    chosen
        .filter(|exe| exe.is_file())
        .map(Path::to_path_buf)
        .or_else(|| known_editors().into_iter().find(|exe| exe.is_file()))
        .unwrap_or_else(notepad)
}

/// Starts `exe` on `file` at `line` (from 1).
pub fn open(exe: &Path, file: &Path, line: usize) -> Result<(), String> {
    std::process::Command::new(exe)
        .args(arguments(exe, file, line))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", exe.display()))
}

/// The editor's name for the menu: its file name without `.exe`.
pub fn name(exe: &Path) -> String {
    exe.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// How `exe` is asked to open `file` at `line`.
fn arguments(exe: &Path, file: &Path, line: usize) -> Vec<OsString> {
    let stem = name(exe).to_lowercase();
    let at_line = || {
        let mut arg = file.as_os_str().to_owned();
        arg.push(format!(":{line}"));
        arg
    };
    match stem.as_str() {
        "code" | "code - insiders" | "cursor" | "codium" | "vscodium" | "windsurf" => {
            vec!["-g".into(), at_line()]
        }
        "notepad++" => vec![format!("-n{line}").into(), file.into()],
        "sublime_text" | "subl" => vec![at_line()],
        _ => vec![file.into()],
    }
}

/// Where the known editors install themselves, in the order they are tried.
fn known_editors() -> Vec<PathBuf> {
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let mut out = Vec::new();
    if let Some(local) = env("LOCALAPPDATA") {
        out.push(local.join(r"Programs\Microsoft VS Code\Code.exe"));
        out.push(local.join(r"Programs\cursor\Cursor.exe"));
    }
    for root in [env("ProgramFiles"), env("ProgramFiles(x86)")]
        .into_iter()
        .flatten()
    {
        out.push(root.join(r"Microsoft VS Code\Code.exe"));
        out.push(root.join(r"Notepad++\notepad++.exe"));
        out.push(root.join(r"Sublime Text\sublime_text.exe"));
        out.push(root.join(r"Sublime Text 3\sublime_text.exe"));
    }
    out
}

fn notepad() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join(r"System32\notepad.exe"))
        .unwrap_or_else(|| PathBuf::from("notepad.exe"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_editor_is_told_the_line_its_way() {
        let file = Path::new(r"C:\docs\a b.md");
        let args = |exe: &str| -> Vec<String> {
            arguments(Path::new(exe), file, 12)
                .into_iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(args(r"C:\x\Code.exe"), ["-g", r"C:\docs\a b.md:12"]);
        assert_eq!(args(r"C:\x\notepad++.exe"), ["-n12", r"C:\docs\a b.md"]);
        assert_eq!(args(r"C:\x\sublime_text.exe"), [r"C:\docs\a b.md:12"]);
        assert_eq!(args(r"C:\Windows\notepad.exe"), [r"C:\docs\a b.md"]);
    }

    #[test]
    fn a_chosen_editor_that_is_gone_is_replaced() {
        let gone = Path::new(r"C:\no\such\editor.exe");
        assert_ne!(editor(Some(gone)), gone);
        assert!(editor(None).file_name().is_some());
    }
}
