//! Asking before a link starts something. A document can link to a program
//! next to it (`[setup](setup.exe)`, a `.lnk`, a script), to a scheme that
//! starts another application (`ms-msdt:`, `search-ms:`), or to a file on
//! another computer (`\\host\share`, which Windows reaches by sending the
//! user's credentials). A Markdown file from a downloaded archive should not
//! do any of that on one click, so those links open only after the reader
//! says so; web pages, mail and ordinary files open at once.

use std::path::{Path, PathBuf};

/// Where a link leads, once resolved against the document.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkTarget {
    /// A URL with a scheme, opened by the program registered for it.
    Url(String),
    /// A Markdown file, opened in a tab at the fragment.
    Markdown(PathBuf, Option<String>),
    /// Any other file or a folder, opened by its default program.
    File(PathBuf),
}

/// Why opening a target is asked about first, if it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Risk {
    /// A program, a script or a shortcut: opening it runs it.
    Program,
    /// A file on another computer of the network.
    Network,
    /// A URL scheme other than the web's or mail's.
    Scheme,
}

/// Extensions Windows runs, or hands to something that runs code, when they
/// are opened.
const RUNNABLE: &[&str] = &[
    "exe",
    "com",
    "bat",
    "cmd",
    "ps1",
    "psm1",
    "psd1",
    "vbs",
    "vbe",
    "vb",
    "js",
    "jse",
    "ws",
    "wsf",
    "wsh",
    "wsc",
    "sct",
    "msc",
    "msi",
    "msp",
    "msix",
    "msixbundle",
    "appx",
    "appxbundle",
    "appref-ms",
    "application",
    "scr",
    "hta",
    "cpl",
    "lnk",
    "url",
    "website",
    "pif",
    "reg",
    "inf",
    "scf",
    "jar",
    "py",
    "pyw",
    "chm",
    "gadget",
    "xbap",
    "diagcab",
    "settingcontent-ms",
    "library-ms",
    "search-ms",
    "searchconnector-ms",
    "iqy",
    "slk",
];

/// What makes opening `target` risky, if anything. `document` is the file
/// the link is in: a network file next to a document on the same computer
/// is not asked about.
pub fn risk(target: &LinkTarget, document: Option<&Path>) -> Option<Risk> {
    let path = match target {
        LinkTarget::Url(url) => {
            let scheme = url.split(':').next().unwrap_or("").to_ascii_lowercase();
            return (!matches!(scheme.as_str(), "http" | "https" | "mailto"))
                .then_some(Risk::Scheme);
        }
        LinkTarget::Markdown(path, _) | LinkTarget::File(path) => path,
    };
    if on_another_host(path, document) {
        return Some(Risk::Network);
    }
    let runnable = matches!(target, LinkTarget::File(_))
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| RUNNABLE.iter().any(|r| r.eq_ignore_ascii_case(e)));
    runnable.then_some(Risk::Program)
}

/// The computer a UNC path (`\\host\share\…`) is on, in lower case.
pub fn network_host(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let rest = text
        .strip_prefix(r"\\")
        .or_else(|| text.strip_prefix("//"))?;
    // `\\?\C:\…` and `\\.\…` are local device paths, not hosts.
    let host = rest.split(['\\', '/']).next()?;
    (!host.is_empty() && host != "?" && host != ".").then(|| host.to_ascii_lowercase())
}

/// Whether `path` is on another computer of the network than `here` (the
/// document's file or folder). Windows reaches one by sending it the
/// user's credentials, which a document must not do by being opened.
pub fn on_another_host(path: &Path, here: Option<&Path>) -> bool {
    network_host(path).is_some_and(|host| here.and_then(network_host) != Some(host))
}

/// The dialog asking whether to open a risky target.
#[derive(Default)]
pub struct ConfirmOpen {
    pending: Option<(LinkTarget, Risk)>,
}

impl ConfirmOpen {
    pub fn ask(&mut self, target: LinkTarget, risk: Risk) {
        self.pending = Some((target, risk));
    }

    pub fn is_open(&self) -> bool {
        self.pending.is_some()
    }

    /// Draws the dialog while a target waits; returns it once the reader
    /// chose to open it. Esc, Cancel or a click outside drop it.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<LinkTarget> {
        let (target, risk) = self.pending.as_ref()?;
        let shown = match target {
            LinkTarget::Url(url) => url.clone(),
            LinkTarget::Markdown(path, _) | LinkTarget::File(path) => path.display().to_string(),
        };
        let reason = match risk {
            Risk::Program => tr!(
                "It is a program, a script or a shortcut: opening it runs it.".to_owned(),
                "Это программа, сценарий или ярлык: открыть — значит запустить.".to_owned()
            ),
            Risk::Network => tr!(
                "It is on another computer of the network, which Windows will sign in to."
                    .to_owned(),
                "Это файл на другом компьютере сети, и Windows передаст ему ваши учётные данные."
                    .to_owned()
            ),
            Risk::Scheme => {
                let scheme = shown.split(':').next().unwrap_or("");
                tr!(
                    format!("It is opened by the application registered for “{scheme}:”."),
                    format!("Её откроет приложение, зарегистрированное для «{scheme}:».")
                )
            }
        };
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::new("confirm-open")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading(tr!("Open this link?", "Открыть ссылку?"));
            ui.add_space(6.0);
            ui.label(reason);
            ui.add_space(6.0);
            ui.add(egui::Label::new(egui::RichText::new(&shown).monospace()).wrap());
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                // Cancel first: the safe choice is the one at hand.
                if ui.button(tr!("Cancel", "Отмена")).clicked() {
                    choice = Some(false);
                }
                if ui.button(tr!("Open", "Открыть")).clicked() {
                    choice = Some(true);
                }
            });
        });
        if choice.is_none() && modal.should_close() {
            choice = Some(false);
        }
        match choice {
            Some(true) => self.pending.take().map(|(target, _)| target),
            Some(false) => {
                self.pending = None;
                None
            }
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> LinkTarget {
        LinkTarget::File(PathBuf::from(path))
    }

    #[test]
    fn web_mail_and_ordinary_files_open_at_once() {
        let doc = Some(Path::new(r"C:\docs\readme.md"));
        for url in ["https://example.com", "HTTP://x", "mailto:a@b.c"] {
            assert_eq!(risk(&LinkTarget::Url(url.into()), doc), None, "{url}");
        }
        for path in [
            r"C:\docs\a.png",
            r"C:\docs\report.pdf",
            r"C:\docs",
            r"C:\docs\notes.txt",
        ] {
            assert_eq!(risk(&file(path), doc), None, "{path}");
        }
        let markdown = LinkTarget::Markdown(PathBuf::from(r"C:\docs\b.md"), None);
        assert_eq!(risk(&markdown, doc), None);
    }

    #[test]
    fn programs_schemes_and_other_computers_are_asked_about() {
        let doc = Some(Path::new(r"C:\docs\readme.md"));
        for path in [
            r"C:\docs\setup.EXE",
            r"C:\docs\run.bat",
            r"C:\docs\x.lnk",
            r"C:\a\s.ps1",
        ] {
            assert_eq!(risk(&file(path), doc), Some(Risk::Program), "{path}");
        }
        for url in [
            "ms-msdt:/id PCWDiagnostic",
            "search-ms:query=x",
            "file:///C:/x",
        ] {
            assert_eq!(
                risk(&LinkTarget::Url(url.into()), doc),
                Some(Risk::Scheme),
                "{url}"
            );
        }
        assert_eq!(risk(&file(r"\\evil\share\a.png"), doc), Some(Risk::Network));
        let markdown = LinkTarget::Markdown(PathBuf::from("//evil/share/a.md"), None);
        assert_eq!(risk(&markdown, doc), Some(Risk::Network));
    }

    #[test]
    fn files_next_to_a_document_on_the_network_open_at_once() {
        let doc = Some(Path::new(r"\\Server\docs\readme.md"));
        assert_eq!(risk(&file(r"\\server\docs\a.png"), doc), None);
        assert_eq!(risk(&file(r"\\other\docs\a.png"), doc), Some(Risk::Network));
        assert_eq!(
            risk(&file(r"\\server\docs\setup.exe"), doc),
            Some(Risk::Program)
        );
        assert_eq!(network_host(Path::new(r"\\?\C:\a")), None);
        assert!(!on_another_host(Path::new(r"\\server\docs\a.md"), doc));
        assert!(on_another_host(Path::new("//other/docs/a.md"), doc));
        let local = Some(Path::new(r"C:\docs"));
        assert!(on_another_host(Path::new("//other/docs/a.md"), local));
        assert!(!on_another_host(Path::new(r"C:\docs\a.md"), None));
    }
}
