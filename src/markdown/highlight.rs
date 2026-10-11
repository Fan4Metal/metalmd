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
/// Colors stop at a line longer than this, or after this long on one
/// block, and the rest is plain: some grammars take minutes on a long
/// line, and one block would hold up all the others behind it.
const MAX_LINE_BYTES: usize = 10_000;
const MAX_BLOCK_TIME: std::time::Duration = std::time::Duration::from_secs(1);

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

/// Code blocks whose colors are kept at most. Live reload adds an entry for
/// each edit of a block, so past this the ones asked for longest ago go.
/// Far above what one document has: a code block's height does not wait
/// for its colors, but one shown again is highlighted again.
const MAX_ENTRIES: usize = 16384;

/// The entries with the request count each was last asked for at.
#[derive(Default)]
struct Cache {
    entries: HashMap<(u64, bool), (Entry, u64)>,
    requests: u64,
}

impl Cache {
    /// Drops the entries asked for longest ago, down to three quarters of
    /// the limit, if over it. The blocks on screen were asked for last.
    fn evict(&mut self) {
        if self.entries.len() <= MAX_ENTRIES {
            return;
        }
        let mut by_age: Vec<(u64, (u64, bool))> =
            self.entries.iter().map(|(k, (_, at))| (*at, *k)).collect();
        by_age.sort_unstable();
        let excess = self.entries.len() - MAX_ENTRIES * 3 / 4;
        for (_, key) in by_age.into_iter().take(excess) {
            self.entries.remove(&key);
        }
    }
}

struct Inner {
    cache: Mutex<Cache>,
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
                cache: Mutex::new(Cache::default()),
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
            cache.requests += 1;
            let now = cache.requests;
            match cache.entries.get_mut(&full_key) {
                Some((entry, at)) => {
                    *at = now;
                    return match entry {
                        Entry::Ready(highlights) => Some(Arc::clone(highlights)),
                        Entry::Pending => None,
                    };
                }
                None => {
                    cache.entries.insert(full_key, (Entry::Pending, now));
                    cache.evict();
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
        // Unless dropped from the cache meanwhile.
        if let Some((entry, _)) = inner
            .cache
            .lock()
            .expect("highlighter lock")
            .entries
            .get_mut(&job.key)
        {
            *entry = Entry::Ready(Arc::new(spans));
        }
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

    let started = std::time::Instant::now();
    let mut lines = HighlightLines::new(syntax, theme);
    let mut spans: Vec<HlSpan> = Vec::new();
    let mut offset = 0usize;
    for line in LinesWithEndings::from(code) {
        if line.len() > MAX_LINE_BYTES || started.elapsed() > MAX_BLOCK_TIME {
            break;
        }
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
    fn the_blocks_asked_for_longest_ago_go_first() {
        let mut cache = Cache::default();
        let total = MAX_ENTRIES as u64 + 1000;
        for i in 0..total {
            cache.entries.insert((i, false), (Entry::Pending, i));
        }
        cache.evict();
        let kept = (MAX_ENTRIES * 3 / 4) as u64;
        assert_eq!(cache.entries.len() as u64, kept);
        assert!(cache.entries.values().all(|(_, at)| *at >= total - kept));
    }

    #[test]
    fn colors_stop_at_a_very_long_line_and_in_time() {
        let syntaxes = two_face::syntax::extra_newlines();
        let themes = two_face::theme::extra();
        let theme = themes.get(two_face::theme::EmbeddedThemeName::Github);
        let rust = syntaxes.find_syntax_by_token("rust").unwrap();
        let code = format!(
            "let a = 1;\n{}\nlet b = 2;\n",
            "x".repeat(MAX_LINE_BYTES + 1)
        );
        let spans = highlight(&syntaxes, rust, theme, &code);
        let end = spans.last().unwrap().range.end;
        assert_eq!(end, "let a = 1;\n".len(), "colors up to the long line");

        // The vim grammar took minutes on this one line.
        let vim = syntaxes.find_syntax_by_token("vim").unwrap();
        let started = Instant::now();
        highlight(&syntaxes, vim, theme, &"/*".repeat(100_000));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

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
