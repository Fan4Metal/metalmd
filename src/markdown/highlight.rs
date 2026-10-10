//! Syntax highlighting for code blocks, off the UI thread.
//!
//! Loading syntect's grammars costs tens of milliseconds and highlighting a
//! long block can cost more, so neither happens on the frame that shows the
//! document. Code blocks are drawn plain until their colors arrive from the
//! worker, which then asks for a repaint.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

/// A colored run of a code block.
#[derive(Clone, Debug, PartialEq)]
pub struct HlSpan {
    pub range: Range<usize>,
    pub rgb: [u8; 3],
    pub bold: bool,
    pub italic: bool,
}

pub type Highlights = Arc<Vec<HlSpan>>;

/// Code larger than this is shown without colors.
const MAX_HIGHLIGHT_BYTES: usize = 256 * 1024;

/// The cache key of a code block: its language and text. Computed once when
/// the document is parsed.
pub fn code_key(lang: &str, code: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    lang.hash(&mut hasher);
    code.hash(&mut hasher);
    hasher.finish()
}

struct Job {
    key: (u64, bool),
    lang: String,
    code: String,
}

enum Entry {
    Pending,
    Ready(Highlights),
}

type RepaintFn = Arc<dyn Fn() + Send + Sync>;

struct Inner {
    cache: Mutex<HashMap<(u64, bool), Entry>>,
    sender: Mutex<Option<Sender<Job>>>,
    repaint: RepaintFn,
}

#[derive(Clone)]
pub struct Highlighter {
    inner: Arc<Inner>,
}

impl Highlighter {
    pub fn new(repaint: impl Fn() + Send + Sync + 'static) -> Highlighter {
        Highlighter {
            inner: Arc::new(Inner {
                cache: Mutex::new(HashMap::new()),
                sender: Mutex::new(None),
                repaint: Arc::new(repaint),
            }),
        }
    }

    /// Starts the worker so grammars load before the first code block needs
    /// them. Idempotent.
    pub fn warm_up(&self) {
        self.sender();
    }

    fn sender(&self) -> Sender<Job> {
        let mut guard = self.inner.sender.lock().expect("highlighter lock");
        if let Some(sender) = guard.as_ref() {
            return sender.clone();
        }
        let (sender, receiver) = channel::<Job>();
        let inner = Arc::clone(&self.inner);
        std::thread::Builder::new()
            .name("highlighter".into())
            .spawn(move || worker(receiver, inner))
            .expect("spawn highlighter thread");
        *guard = Some(sender.clone());
        sender
    }

    /// The colors of a code block, or `None` while they are being computed.
    pub fn get(&self, key: u64, lang: &str, code: &str, dark: bool) -> Option<Highlights> {
        if lang.is_empty() || code.len() > MAX_HIGHLIGHT_BYTES {
            return None;
        }
        let full_key = (key, dark);
        {
            let mut cache = self.inner.cache.lock().expect("highlighter lock");
            match cache.get(&full_key) {
                Some(Entry::Ready(highlights)) => return Some(Arc::clone(highlights)),
                Some(Entry::Pending) => return None,
                None => {
                    cache.insert(full_key, Entry::Pending);
                }
            }
        }
        let _ = self.sender().send(Job {
            key: full_key,
            lang: lang.to_owned(),
            code: code.to_owned(),
        });
        None
    }
}

fn normalize_lang(lang: &str) -> String {
    let lower = lang.trim().to_ascii_lowercase();
    let mapped = match lower.as_str() {
        "shell" | "console" | "zsh" | "shellscript" | "sh-session" => "sh",
        "c++" => "cpp",
        "c#" | "csharp" => "cs",
        "jsonc" | "json5" => "json",
        "js" | "javascript" | "mjs" | "cjs" | "node" => "js",
        "ts" | "typescript" => "ts",
        "py" | "python3" => "py",
        "yml" => "yaml",
        "ps" | "ps1" | "pwsh" => "ps1",
        "docker" => "dockerfile",
        "golang" => "go",
        "rs" => "rust",
        "text" | "txt" | "plain" | "plaintext" => "txt",
        other => other,
    };
    mapped.to_owned()
}

fn worker(receiver: Receiver<Job>, inner: Arc<Inner>) {
    let syntaxes = two_face::syntax::extra_newlines();
    let themes = two_face::theme::extra();
    let light = themes.get(two_face::theme::EmbeddedThemeName::Github);
    let dark = themes.get(two_face::theme::EmbeddedThemeName::OneHalfDark);

    while let Ok(job) = receiver.recv() {
        let lang = normalize_lang(&job.lang);
        let syntax = syntaxes
            .find_syntax_by_token(&lang)
            .or_else(|| syntaxes.find_syntax_by_extension(&lang));
        let spans = match syntax {
            Some(syntax) if lang != "txt" => {
                let theme = if job.key.1 { dark } else { light };
                highlight(&syntaxes, syntax, theme, &job.code)
            }
            _ => Vec::new(),
        };
        inner
            .cache
            .lock()
            .expect("highlighter lock")
            .insert(job.key, Entry::Ready(Arc::new(spans)));
        (inner.repaint)();
    }
}

fn highlight(
    syntaxes: &syntect::parsing::SyntaxSet,
    syntax: &syntect::parsing::SyntaxReference,
    theme: &syntect::highlighting::Theme,
    code: &str,
) -> Vec<HlSpan> {
    use syntect::easy::HighlightLines;
    use syntect::highlighting::FontStyle;
    use syntect::util::LinesWithEndings;

    let mut lines = HighlightLines::new(syntax, theme);
    let mut spans: Vec<HlSpan> = Vec::new();
    let mut offset = 0usize;
    for line in LinesWithEndings::from(code) {
        let Ok(ranges) = lines.highlight_line(line, syntaxes) else {
            return Vec::new();
        };
        for (style, piece) in ranges {
            let range = offset..offset + piece.len();
            offset += piece.len();
            let span = HlSpan {
                range,
                rgb: [style.foreground.r, style.foreground.g, style.foreground.b],
                bold: style.font_style.contains(FontStyle::BOLD),
                italic: style.font_style.contains(FontStyle::ITALIC),
            };
            match spans.last_mut() {
                Some(last)
                    if last.range.end == span.range.start
                        && last.rgb == span.rgb
                        && last.bold == span.bold
                        && last.italic == span.italic =>
                {
                    last.range.end = span.range.end;
                }
                _ => spans.push(span),
            }
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn highlights_arrive_from_the_worker_and_cover_the_code() {
        let highlighter = Highlighter::new(|| {});
        let code = "fn main() {\n    let x = 1;\n}";
        let key = code_key("rust", code);
        assert!(highlighter.get(key, "rust", code, false).is_none());
        let deadline = Instant::now() + Duration::from_secs(20);
        let spans = loop {
            if let Some(spans) = highlighter.get(key, "rust", code, false) {
                break spans;
            }
            assert!(Instant::now() < deadline, "highlighter never answered");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(spans.len() > 3, "{spans:?}");
        assert_eq!(spans.last().unwrap().range.end, code.len());
        assert!(spans.windows(2).all(|w| w[0].range.end == w[1].range.start));
    }

    #[test]
    fn unknown_languages_and_plain_text_are_not_highlighted() {
        let highlighter = Highlighter::new(|| {});
        assert!(highlighter.get(1, "", "x", false).is_none());
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(spans) = highlighter.get(2, "no-such-language", "x", true) {
                assert!(spans.is_empty());
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
