//! Opening a document in a text editor (Ctrl+E) at the line being read.
//!
//! The editors offered are the known ones installed (VS Code and its kin,
//! Zed, Notepad++, Notepad3, Sublime Text, Typora, MarkText), then the
//! programs Windows recommends for `.md`, `.markdown` and `.txt` files (the
//! "Open with" list), then Notepad; MetalMD itself and browsers are left
//! out. The one used is the one chosen in a tab's context menu, else the
//! first of them. Each known editor is told the line its own way.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// An editor offered in the menu.
#[derive(Clone, Debug, PartialEq)]
pub struct Editor {
    pub name: String,
    pub exe: PathBuf,
}

/// The editor to use: `chosen` if it is still there, else the first one
/// found.
pub fn editor(chosen: Option<&Path>) -> PathBuf {
    chosen
        .filter(|exe| exe.is_file())
        .map(Path::to_path_buf)
        .or_else(|| installed().first().map(|e| e.exe.clone()))
        .unwrap_or_else(notepad)
}

/// The editors found, listed once per run, when first asked for.
pub fn installed() -> &'static [Editor] {
    static LIST: OnceLock<Vec<Editor>> = OnceLock::new();
    LIST.get_or_init(|| {
        let mut list: Vec<Editor> = known_editors()
            .into_iter()
            .filter(|e| e.exe.is_file())
            .collect();
        #[cfg(windows)]
        list.extend(recommended());
        // Notepad last, whoever else named it.
        let notepad_exe = notepad();
        list.retain(|e| !crate::settings::same_path(&e.exe, &notepad_exe));
        list.push(Editor {
            name: tr!("Notepad", "Блокнот").to_owned(),
            exe: notepad_exe,
        });
        let mut seen: Vec<String> = Vec::new();
        list.retain(|e| {
            let key = e.exe.to_string_lossy().to_lowercase();
            let fresh = !seen.contains(&key) && !is_excluded(&e.exe);
            seen.push(key);
            fresh
        });
        list
    })
}

/// Starts `exe` on `file` at `line` (from 1).
pub fn open(exe: &Path, file: &Path, line: usize) -> Result<(), String> {
    std::process::Command::new(exe)
        .args(arguments(exe, file, line))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", exe.display()))
}

/// The name of `exe` in the menu: the one found for it, else its file name
/// without `.exe`.
pub fn name(exe: &Path) -> String {
    installed()
        .iter()
        .find(|e| crate::settings::same_path(&e.exe, exe))
        .map(|e| e.name.clone())
        .unwrap_or_else(|| stem(exe))
}

fn stem(exe: &Path) -> String {
    exe.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// How `exe` is asked to open `file` at `line`.
fn arguments(exe: &Path, file: &Path, line: usize) -> Vec<OsString> {
    let stem = stem(exe).to_lowercase();
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
        "notepad3" => vec!["/g".into(), line.to_string().into(), file.into()],
        "sublime_text" | "subl" | "zed" => vec![at_line()],
        _ => vec![file.into()],
    }
}

/// Programs that open Markdown but are not editors, and MetalMD itself.
fn is_excluded(exe: &Path) -> bool {
    const NOT_EDITORS: [&str; 10] = [
        "metalmd", "msedge", "chrome", "firefox", "iexplore", "opera", "brave", "vivaldi",
        "browser", "launcher",
    ];
    let stem = stem(exe).to_lowercase();
    NOT_EDITORS.contains(&stem.as_str())
        // A Store app's own exe cannot be started directly.
        || exe.to_string_lossy().to_lowercase().contains(r"\windowsapps\")
}

/// Where the known editors install themselves, in the order they are
/// preferred.
fn known_editors() -> Vec<Editor> {
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let mut out = Vec::new();
    let mut add = |name: &str, path: PathBuf| {
        out.push(Editor {
            name: name.to_owned(),
            exe: path,
        })
    };
    let local = env("LOCALAPPDATA").map(|d| d.join("Programs"));
    let roots: Vec<PathBuf> = [env("ProgramFiles"), env("ProgramFiles(x86)")]
        .into_iter()
        .flatten()
        .collect();
    let everywhere = |relative: &str| -> Vec<PathBuf> {
        local
            .iter()
            .chain(&roots)
            .map(|root| root.join(relative))
            .collect()
    };
    for (name, relative) in [
        ("Visual Studio Code", r"Microsoft VS Code\Code.exe"),
        (
            "Visual Studio Code Insiders",
            r"Microsoft VS Code Insiders\Code - Insiders.exe",
        ),
        ("Cursor", r"cursor\Cursor.exe"),
        ("VSCodium", r"VSCodium\VSCodium.exe"),
        ("Windsurf", r"Windsurf\Windsurf.exe"),
        ("Zed", r"Zed\Zed.exe"),
        ("Notepad++", r"Notepad++\notepad++.exe"),
        ("Notepad3", r"Notepad3\Notepad3.exe"),
        ("Sublime Text", r"Sublime Text\sublime_text.exe"),
        ("Sublime Text 3", r"Sublime Text 3\sublime_text.exe"),
        ("Typora", r"Typora\Typora.exe"),
        ("MarkText", r"MarkText\MarkText.exe"),
    ] {
        for path in everywhere(relative) {
            add(name, path);
        }
    }
    out
}

/// The programs Windows recommends for Markdown and text files ("Open
/// with"), those that are a program file of their own.
#[cfg(windows)]
fn recommended() -> Vec<Editor> {
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree};
    use windows::Win32::UI::Shell::{ASSOC_FILTER_RECOMMENDED, IAssocHandler, SHAssocEnumHandlers};
    use windows::core::{HSTRING, PWSTR};

    // Already initialised on the UI thread (for drag and drop); a second
    // call only counts.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let take = |text: PWSTR| -> String {
        let out = unsafe { text.to_string() }.unwrap_or_default();
        unsafe { CoTaskMemFree(Some(text.0 as _)) };
        out
    };
    let mut out = Vec::new();
    for extension in [".md", ".markdown", ".txt"] {
        let Ok(handlers) =
            (unsafe { SHAssocEnumHandlers(&HSTRING::from(extension), ASSOC_FILTER_RECOMMENDED) })
        else {
            continue;
        };
        loop {
            let mut handler: [Option<IAssocHandler>; 1] = [None];
            let mut fetched = 0;
            if unsafe { handlers.Next(&mut handler, Some(&mut fetched)) }.is_err() || fetched == 0 {
                break;
            }
            let Some(handler) = handler[0].take() else {
                break;
            };
            let Ok(path) = (unsafe { handler.GetName() }).map(take) else {
                continue;
            };
            let exe = PathBuf::from(path);
            if !exe.is_absolute() || !exe.is_file() {
                continue;
            }
            let name = (unsafe { handler.GetUIName() })
                .map(take)
                .ok()
                .map(|n| {
                    // Some programs give their file name.
                    let lower = n.to_lowercase();
                    match lower.strip_suffix(".exe") {
                        Some(stem) => n[..stem.len()].to_owned(),
                        None => n,
                    }
                })
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| stem(&exe));
            out.push(Editor { name, exe });
        }
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
        assert_eq!(args(r"C:\x\Notepad3.exe"), ["/g", "12", r"C:\docs\a b.md"]);
        assert_eq!(args(r"C:\x\sublime_text.exe"), [r"C:\docs\a b.md:12"]);
        assert_eq!(args(r"C:\Windows\notepad.exe"), [r"C:\docs\a b.md"]);
    }

    #[test]
    fn a_chosen_editor_that_is_gone_is_replaced() {
        let gone = Path::new(r"C:\no\such\editor.exe");
        assert_ne!(editor(Some(gone)), gone);
        assert!(editor(None).file_name().is_some());
    }

    #[test]
    fn the_list_ends_with_notepad_and_leaves_out_browsers() {
        let list = installed();
        assert!(
            list.last().unwrap().exe.ends_with("notepad.exe"),
            "{list:#?}"
        );
        assert!(list.iter().all(|e| !is_excluded(&e.exe)));
        assert!(is_excluded(Path::new(r"C:\x\msedge.exe")));
    }

    /// Prints the editors found here: `cargo test print_editors -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn print_editors() {
        for e in installed() {
            println!("{} — {}", e.name, e.exe.display());
        }
    }
}
