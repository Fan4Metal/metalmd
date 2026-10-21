//! Printing a document and saving it as PDF.
//!
//! The document is written as a web page (`html`) and printed by WebView2
//! (`webview`) on a thread of its own, so nothing of it is loaded before
//! the reader asks. Where WebView2 is missing, the page opens in the
//! default browser with its print dialog, which can save a PDF too.

pub mod html;
#[cfg(windows)]
mod webview;

use crate::markdown::document::Document;
use html::PageOptions;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Where a document goes.
#[derive(Clone, Debug)]
pub enum Output {
    Printer,
    Pdf(PathBuf),
}

/// A document to print, and the window to show the print dialog over.
pub struct Request {
    pub doc: Arc<Document>,
    pub page: PageOptions,
    pub output: Output,
    pub owner: Option<isize>,
    /// The app shows the dark theme: the print window's caption follows.
    pub dark: bool,
}

/// How a request ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The print dialog closed (printed or not, it does not say).
    Printed,
    Saved(PathBuf),
    /// WebView2 is missing: the page was opened in the browser instead.
    InBrowser,
}

/// Prints on a thread of its own and calls `done` with how it went.
pub fn spawn(request: Request, done: impl FnOnce(Result<Outcome, String>) + Send + 'static) {
    std::thread::Builder::new()
        .name("print".into())
        // The page is written by walking the document's nesting.
        .stack_size(16 * 1024 * 1024)
        .spawn(move || done(print(request)))
        .expect("spawn print thread");
}

fn print(request: Request) -> Result<Outcome, String> {
    let page = html::page(&request.doc, &request.page);
    let file = TempPage::write(&page)?;
    #[cfg(windows)]
    {
        let target = match &request.output {
            Output::Printer => webview::Target::Printer {
                owner: request.owner,
                title: &request.page.title,
                dark: request.dark,
            },
            Output::Pdf(path) => webview::Target::Pdf(path),
        };
        match webview::run(&html::file_url(&file.0), target) {
            Ok(_) => {
                return Ok(match request.output {
                    Output::Printer => Outcome::Printed,
                    Output::Pdf(path) => Outcome::Saved(path),
                });
            }
            Err(webview::Error::NoRuntime) => {}
            Err(webview::Error::Closed) => return Ok(Outcome::Printed),
            Err(webview::Error::Failed(error)) => return Err(error),
        }
    }
    let _ = (request.owner, request.dark);
    in_browser(&request)
}

/// Opens the page in the default browser, which shows its print dialog.
fn in_browser(request: &Request) -> Result<Outcome, String> {
    let options = PageOptions {
        print_on_load: true,
        ..request.page.clone()
    };
    let page = html::page(&request.doc, &options);
    // Kept: the browser reads it after this returns. The next print of a
    // document with the same name writes over it.
    let path = temp_dir()?.join(format!("{}.html", file_stem(&request.page.title)));
    std::fs::write(&path, page).map_err(|e| e.to_string())?;
    open::that_detached(&path).map_err(|e| e.to_string())?;
    Ok(Outcome::InBrowser)
}

/// The reader's region measures in inches (the US and a few others), so
/// their paper is Letter, else A4.
fn letter_paper() -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::Globalization::{GetLocaleInfoEx, LOCALE_IMEASURE};
        let mut buffer = [0u16; 4];
        let len = unsafe {
            GetLocaleInfoEx(
                windows::core::PCWSTR::null(),
                LOCALE_IMEASURE,
                Some(&mut buffer),
            )
        };
        len > 0 && buffer[0] == u16::from(b'1')
    }
    #[cfg(not(windows))]
    false
}

/// The paper's size in inches, portrait.
fn paper_inches() -> (f64, f64) {
    if letter_paper() {
        (8.5, 11.0)
    } else {
        (210.0 / 25.4, 297.0 / 25.4)
    }
}

/// The side margins of the page, in millimetres.
const SIDE_MARGIN_MM: f64 = 15.0;

/// The width of the text on paper, in CSS pixels (96 to the inch).
fn paper_text_px() -> f32 {
    ((paper_inches().0 - 2.0 * SIDE_MARGIN_MM / 25.4) * 96.0) as f32
}

/// The app's folder for pages to print, in the temporary folder.
fn temp_dir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("MetalMD");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// `title` made fit for a file name.
fn file_stem(title: &str) -> String {
    let stem: String = title
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let stem = stem.trim_matches(['.', ' ']);
    if stem.is_empty() {
        "document".to_owned()
    } else {
        stem.to_owned()
    }
}

/// A page written for WebView2, deleted once printed.
struct TempPage(PathBuf);

impl TempPage {
    fn write(page: &str) -> Result<TempPage, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "print-{}-{}.html",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = temp_dir()?.join(name);
        std::fs::write(&path, page).map_err(|e| e.to_string())?;
        Ok(TempPage(path))
    }
}

impl Drop for TempPage {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The PDF file name offered for the document at `path`.
pub fn pdf_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("{}.pdf", file_stem(&stem))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_fit_for_files() {
        assert_eq!(file_stem("a: b?.md"), "a_ b_.md");
        assert_eq!(file_stem(" .. "), "document");
        assert_eq!(pdf_name(Path::new(r"C:\docs\notes.md")), "notes.pdf");
    }
}
