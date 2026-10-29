//! Where text the viewer shows comes from in the file, for "Copy as
//! Markdown" and highlighting (`==text==`), which work on the source of a
//! selection. Worked out when asked, by parsing the text again with the
//! positions kept: `Document::parse` keeps none, to show a document as soon
//! as it can.
//!
//! comrak gives each node its place in the preprocessed text; the rewrites
//! preprocessing made take it back to the file's (a place inside a
//! rewritten wikilink becomes its start or end). Text that is not as
//! written (escapes, entities, code spans, formulas, HTML) has no places
//! inside it, only its ends. A selection that starts or ends inside
//! emphasis, a link or other markup takes in all of it, so what is copied
//! or highlighted is Markdown of its own.

use super::document::{Block, Document, Inline};
use super::preprocess::{self, Rewrite};
use comrak::nodes::{NodeValue, Sourcepos};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;

/// What a selection is in: a `Rich` or a code block, by id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    Text(u32),
    Code(u32),
}

/// The selected part of one `Rich` or code block, as bytes of its text,
/// with the digest of that text to tell it is still the same.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selected {
    pub part: Part,
    pub range: Range<usize>,
    pub digest: u64,
}

pub fn digest(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// A piece of a `Rich`'s text and where it is in the preprocessed text.
#[derive(Clone, Debug)]
struct Piece {
    text: Range<usize>,
    source: Range<usize>,
    /// The text is the source as written, so each place inside one is a
    /// place inside the other.
    exact: bool,
}

/// Where a `Rich` came from.
#[derive(Clone, Debug, Default)]
pub(crate) struct RichSource {
    pieces: Vec<Piece>,
    /// Inline markup around some of the text (emphasis, links,
    /// highlights): a selection may not take part of one.
    groups: Vec<Range<usize>>,
    /// The highlights among `groups`.
    marks: Vec<Range<usize>>,
    /// Built from this HTML block, which is all that is known.
    html: Option<Range<usize>>,
}

/// Inline markup open around the text being built.
#[derive(Default)]
pub(crate) struct InlineTrack {
    cur: RichSource,
    open: Vec<(Range<usize>, bool)>,
}

impl InlineTrack {
    /// Notes that `text` of the `Rich` being built came from `source`.
    pub(crate) fn piece(&mut self, text: Range<usize>, source: Range<usize>, exact: bool) {
        for (group, mark) in &self.open {
            if !self.cur.groups.contains(group) {
                self.cur.groups.push(group.clone());
                if *mark {
                    self.cur.marks.push(group.clone());
                }
            }
        }
        self.cur.pieces.push(Piece {
            text,
            source,
            exact,
        });
    }

    pub(crate) fn open(&mut self, source: Range<usize>, mark: bool) {
        self.open.push((source, mark));
    }

    pub(crate) fn close(&mut self) {
        self.open.pop();
    }

    /// What was noted for the `Rich` just finished, built from the HTML
    /// block at `html` if any; markup stays open.
    pub(crate) fn take(&mut self, html: Option<Range<usize>>) -> RichSource {
        RichSource {
            html,
            ..std::mem::take(&mut self.cur)
        }
    }
}

/// Places in the preprocessed text, kept while a document is built by
/// `Document::parse_with_sources`.
pub(crate) struct Tracker {
    pub(crate) text: String,
    line_starts: Vec<usize>,
    pub(crate) rich: HashMap<u32, RichSource>,
    /// Code blocks and front matter: the whole block.
    pub(crate) blocks: HashMap<u32, Range<usize>>,
    pub(crate) highlights: usize,
}

impl Tracker {
    pub(crate) fn new(text: &str) -> Tracker {
        Tracker {
            text: text.to_owned(),
            line_starts: line_starts(text),
            rich: HashMap::new(),
            blocks: HashMap::new(),
            highlights: 0,
        }
    }

    /// The bytes a node spans.
    pub(crate) fn range(&self, pos: &Sourcepos) -> Range<usize> {
        let offset = |line: usize, column: usize| {
            let Some(&start) = line.checked_sub(1).and_then(|l| self.line_starts.get(l)) else {
                return self.text.len();
            };
            floor_char_boundary(
                &self.text,
                (start + column.saturating_sub(1)).min(self.text.len()),
            )
        };
        let start = offset(pos.start.line, pos.start.column);
        // The end is the last byte of the last character, so the next one.
        let last = offset(pos.end.line, pos.end.column);
        let end = self.text[last..]
            .chars()
            .next()
            .map_or(last, |c| last + c.len_utf8());
        start..end.max(start)
    }

    /// Whether `text` is the preprocessed text at `source`.
    pub(crate) fn is_as_written(&self, text: &str, source: &Range<usize>) -> bool {
        self.text.get(source.clone()) == Some(text)
    }
}

fn floor_char_boundary(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Where each line starts, lines ending as comrak ends them: at `\n`,
/// `\r\n` or a lone `\r`.
fn line_starts(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut starts = vec![0];
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' || (b == b'\r' && bytes.get(i + 1) != Some(&b'\n')) {
            starts.push(i + 1);
        }
    }
    starts
}

/// A document's places in its file.
pub(crate) struct Sources {
    pub(crate) tracker: Tracker,
    /// Each pass's rewrites, first pass first, from the file's text to
    /// the preprocessed one.
    pub(crate) passes: Vec<Vec<Rewrite>>,
}

/// Why a selection could not be found or changed in the file.
#[derive(Debug, PartialEq, Eq)]
pub enum Problem {
    /// The text is not the one shown any more.
    Changed,
    /// The selection cannot be highlighted (or shows no highlight).
    Cannot,
    /// The selection is in an embedded note, another file.
    Embedded,
}

impl Problem {
    pub fn message(&self) -> String {
        match self {
            Problem::Changed => tr!(
                "The file has changed since it was shown.",
                "Файл изменился после того, как был показан."
            ),
            Problem::Cannot => tr!(
                "This text cannot be highlighted here.",
                "Этот текст здесь нельзя выделить цветом."
            ),
            Problem::Embedded => tr!(
                "This text is in an embedded note, another file.",
                "Этот текст во вставленной заметке, в другом файле."
            ),
        }
        .to_owned()
    }
}

struct Located<'a> {
    source: &'a str,
    doc: Document,
    sources: Sources,
}

impl<'a> Located<'a> {
    /// Parses `source` with places kept and checks `selection` is in it.
    fn new(source: &'a str, selection: &[Selected]) -> Result<Located<'a>, Problem> {
        let (doc, sources) = Document::parse_with_sources(source);
        // Ids past the file's own are an embedded note's (`markdown::embed`).
        if selection.iter().any(|s| match s.part {
            Part::Text(id) | Part::Code(id) => id >= doc.ids,
        }) {
            return Err(Problem::Embedded);
        }
        let mut digests = HashMap::new();
        each_part(&doc.blocks, &mut |part, text| {
            if selection.iter().any(|s| s.part == part) {
                digests.insert(part, digest(text));
            }
        });
        let shown = |s: &Selected| digests.get(&s.part) == Some(&s.digest);
        if selection.is_empty() || !selection.iter().all(shown) {
            return Err(Problem::Changed);
        }
        Ok(Located {
            source,
            doc,
            sources,
        })
    }

    fn rich(&self, part: Part) -> Option<&RichSource> {
        match part {
            Part::Text(id) => self.sources.tracker.rich.get(&id),
            Part::Code(_) => None,
        }
    }

    /// Where `at` in the selected part is in the preprocessed text: a start
    /// of a selection, or with `end` its end.
    fn position(&self, part: Part, at: usize, end: bool) -> Option<usize> {
        let tracker = &self.sources.tracker;
        let rich = match part {
            Part::Code(id) => {
                let block = tracker.blocks.get(&id)?;
                return Some(if end { block.end } else { block.start });
            }
            Part::Text(id) => tracker.rich.get(&id)?,
        };
        if let Some(html) = &rich.html {
            return Some(if end { html.end } else { html.start });
        }
        let inside = |piece: &Piece| {
            if piece.exact {
                piece.source.start + (at - piece.text.start)
            } else if end {
                piece.source.end
            } else {
                piece.source.start
            }
        };
        if end {
            match rich.pieces.iter().rev().find(|p| p.text.start < at) {
                Some(piece) if piece.text.end <= at => Some(piece.source.end),
                Some(piece) => Some(inside(piece)),
                None => rich.pieces.first().map(|p| p.source.start),
            }
        } else {
            match rich.pieces.iter().find(|p| p.text.end > at) {
                Some(piece) if piece.text.start >= at => Some(piece.source.start),
                Some(piece) => Some(inside(piece)),
                None => rich.pieces.last().map(|p| p.source.end),
            }
        }
    }

    /// The place in the file of `at` in the preprocessed text.
    fn original(&self, at: usize, end: bool) -> usize {
        let at = self
            .sources
            .passes
            .iter()
            .rev()
            .fold(at, |at, pass| preprocess::before_rewrites(pass, at, end));
        floor_char_boundary(self.source, at.min(self.source.len()))
    }

    fn original_range(&self, range: &Range<usize>) -> Range<usize> {
        self.original(range.start, false)..self.original(range.end, true)
    }

    /// The selected bytes of `selection` in the preprocessed text, taking
    /// in all of the markup it starts or ends inside.
    fn span(&self, first: &Selected, last: &Selected) -> Option<Range<usize>> {
        let start = self.position(first.part, first.range.start, false)?;
        let end = self.position(last.part, last.range.end, true)?;
        let mut groups = Vec::new();
        for part in [first.part, last.part] {
            if let Some(rich) = self.rich(part) {
                groups.extend(rich.groups.iter().cloned());
            }
        }
        Some(take_in(start..end.max(start), &groups))
    }
}

/// `range` grown until each of `groups` is in it or it is in each.
fn take_in(mut range: Range<usize>, groups: &[Range<usize>]) -> Range<usize> {
    let cut = |range: &Range<usize>, group: &Range<usize>| {
        let overlaps = group.start < range.end && range.start < group.end;
        let holds = range.start <= group.start && group.end <= range.end;
        let inside = group.start <= range.start && range.end <= group.end;
        overlaps && !holds && !inside
    };
    while let Some(group) = groups.iter().find(|group| cut(&range, group)) {
        range = range.start.min(group.start)..range.end.max(group.end);
    }
    range
}

/// Calls `f` with each `Rich` and code block of `blocks` and its text.
fn each_part(blocks: &[Block], f: &mut impl FnMut(Part, &str)) {
    let inlines = |inlines: &[Inline], f: &mut dyn FnMut(Part, &str)| {
        for inline in inlines {
            if let Inline::Text(rich) = inline {
                f(Part::Text(rich.id), &rich.text);
            }
        }
    };
    for block in blocks {
        match block {
            Block::Heading { inlines: i, .. } | Block::Paragraph { inlines: i, .. } => {
                inlines(i, f)
            }
            Block::Details { summary, .. } => inlines(summary, f),
            Block::Code { id, code, .. } => f(Part::Code(*id), code),
            Block::FrontMatter { id, yaml, .. } => f(Part::Code(*id), yaml),
            Block::Quote(blocks) | Block::Alert { blocks, .. } | Block::Footnote { blocks, .. } => {
                each_part(blocks, f)
            }
            Block::List { items, .. } => {
                for item in items {
                    each_part(&item.blocks, f);
                }
            }
            Block::Table(table) => {
                for cell in table.header.iter().chain(table.rows.iter().flatten()) {
                    inlines(cell, f);
                }
            }
            Block::DefinitionList(items) => {
                for (terms, details) in items {
                    each_part(terms, f);
                    for detail in details {
                        each_part(detail, f);
                    }
                }
            }
            // An embedded note's text is another file's.
            Block::Math { .. } | Block::Rule | Block::DetailsEnd | Block::Embed { .. } => {}
        }
    }
}

/// The Markdown of a selection: its parts in document order. Starting
/// where only block markup precedes it on its line (`# `, `- `, `> `, a
/// table's `|`), that is taken in too, and so is what follows its end on
/// the line if only `|`, `#` or spaces do.
pub fn markdown(source: &str, selection: &[Selected]) -> Result<String, Problem> {
    let (Some(first), Some(last)) = (selection.first(), selection.last()) else {
        return Err(Problem::Changed);
    };
    let located = Located::new(source, selection)?;
    if let ([only], Part::Code(_)) = (selection, first.part) {
        // The code as written, or near enough (indentation in a list).
        let mut code = None;
        each_part(&located.doc.blocks, &mut |part, text| {
            if part == only.part {
                code = text.get(only.range.clone()).map(str::to_owned);
            }
        });
        return code.ok_or(Problem::Changed);
    }
    let span = located.span(first, last).ok_or(Problem::Changed)?;
    let span = located.original_range(&span);
    let line_start = source[..span.start]
        .rfind(['\n', '\r'])
        .map_or(0, |at| at + 1);
    let start = if is_block_markup(&source[line_start..span.start]) {
        line_start
    } else {
        span.start
    };
    let line_end = source[span.end..]
        .find(['\n', '\r'])
        .map_or(source.len(), |at| span.end + at);
    let rest = &source[span.end..line_end];
    let end = if rest
        .chars()
        .all(|c| c == '|' || c == '#' || c.is_whitespace())
    {
        line_end
    } else {
        span.end
    };
    Ok(source[start..end.max(start)].to_owned())
}

/// Whether `text`, what precedes a block's text on its line, is only
/// block markup: indentation, quote marks, list markers, task boxes, a
/// heading's `#`s, a table's `|`.
fn is_block_markup(text: &str) -> bool {
    let mut rest = text;
    loop {
        rest = rest.trim_start_matches([' ', '\t']);
        if rest.is_empty() {
            return true;
        }
        let spaced = |after: &str| after.is_empty() || after.starts_with([' ', '\t']);
        if let Some(after) = rest.strip_prefix(['>', '|']) {
            rest = after;
        } else if let Some(after) = rest.strip_prefix(['-', '*', '+'])
            && spaced(after)
        {
            rest = after;
        } else if rest.starts_with('#') {
            let after = rest.trim_start_matches('#');
            if rest.len() - after.len() > 6 || !spaced(after) {
                return false;
            }
            rest = after;
        } else if rest.starts_with(|c: char| c.is_ascii_digit()) {
            let after = rest.trim_start_matches(|c: char| c.is_ascii_digit());
            match after.strip_prefix(['.', ')']) {
                Some(after) if rest.len() - after.len() <= 10 && spaced(after) => rest = after,
                _ => return false,
            }
        } else if let Some(after) = ["[ ]", "[x]", "[X]"]
            .iter()
            .find_map(|b| rest.strip_prefix(b))
            && spaced(after)
        {
            rest = after;
        } else {
            return false;
        }
    }
}

/// One change to the file's text.
struct Edit {
    range: Range<usize>,
    with: &'static str,
}

fn apply(source: &str, mut edits: Vec<Edit>) -> String {
    // An insertion before a removal at the same place.
    edits.sort_by_key(|e| (e.range.start, e.range.end));
    let mut out = String::with_capacity(source.len() + 4 * edits.len());
    let mut copied = 0;
    for edit in edits {
        out.push_str(&source[copied..edit.range.start]);
        out.push_str(edit.with);
        copied = edit.range.end;
    }
    out.push_str(&source[copied..]);
    out
}

/// Removes the `==` around `mark` (a highlight's place in the file).
fn unmark(source: &str, mark: &Range<usize>, edits: &mut Vec<Edit>) -> Result<(), Problem> {
    let ok = mark.len() >= 4
        && source.get(mark.start..mark.start + 2) == Some("==")
        && source.get(mark.end - 2..mark.end) == Some("==");
    if !ok {
        return Err(Problem::Cannot);
    }
    edits.push(Edit {
        range: mark.start..mark.start + 2,
        with: "",
    });
    edits.push(Edit {
        range: mark.end - 2..mark.end,
        with: "",
    });
    Ok(())
}

/// The number of highlights in a parse of `text`.
fn count_highlights(text: &str) -> usize {
    let arena = comrak::Arena::new();
    let preprocessed = preprocess::preprocess(text);
    let root = comrak::parse_document(&arena, &preprocessed, &preprocess::markdown_options());
    root.descendants()
        .filter(|n| matches!(n.data().value, NodeValue::Highlight))
        .count()
}

/// `source` with the selection highlighted (`==text==`), each `Rich` of it
/// on its own, as a highlight cannot span paragraphs. Highlights it takes
/// part of join it. `None` if it is highlighted already.
pub fn highlight(source: &str, selection: &[Selected]) -> Result<Option<String>, Problem> {
    let located = Located::new(source, selection)?;
    let mut edits = Vec::new();
    let mut added = 0;
    let mut removed = 0;
    // Some of it is text that can be highlighted.
    let mut possible = false;
    for selected in selection {
        let Some(rich) = located.rich(selected.part) else {
            continue;
        };
        if rich.html.is_some() {
            continue;
        }
        let Some(span) = located.span(selected, selected) else {
            continue;
        };
        let mut span = located.original_range(&take_in(span, &rich.groups));
        // `==` next to a space would not open or close a highlight.
        let trimmed = source[span.clone()].trim_start();
        span.start = span.end - trimmed.len();
        span.end = span.start + trimmed.trim_end().len();
        if span.is_empty() {
            continue;
        }
        possible = true;
        let marks: Vec<Range<usize>> = rich
            .marks
            .iter()
            .map(|m| located.original_range(m))
            .collect();
        if marks
            .iter()
            .any(|m| m.start <= span.start && span.end <= m.end)
        {
            continue;
        }
        for mark in marks
            .iter()
            .filter(|m| span.start <= m.start && m.end <= span.end)
        {
            unmark(source, mark, &mut edits)?;
            removed += 1;
        }
        edits.push(Edit {
            range: span.start..span.start,
            with: "==",
        });
        edits.push(Edit {
            range: span.end..span.end,
            with: "==",
        });
        added += 1;
    }
    if added == 0 {
        return if possible {
            Ok(None)
        } else {
            Err(Problem::Cannot)
        };
    }
    let out = apply(source, edits);
    let expected = located.sources.tracker.highlights + added - removed;
    if count_highlights(&out) != expected {
        return Err(Problem::Cannot);
    }
    Ok(Some(out))
}

/// `source` without the highlights the selection touches, or, for an empty
/// selection, the one around it. `None` if there are none.
pub fn unhighlight(source: &str, selection: &[Selected]) -> Result<Option<String>, Problem> {
    let located = Located::new(source, selection)?;
    let mut marks: Vec<Range<usize>> = Vec::new();
    for selected in selection {
        let Some(rich) = located.rich(selected.part) else {
            continue;
        };
        if rich.html.is_some() {
            continue;
        }
        let (Some(start), Some(end)) = (
            located.position(selected.part, selected.range.start, false),
            located.position(selected.part, selected.range.end, true),
        ) else {
            continue;
        };
        let (start, end) = (start.min(end), start.max(end));
        for mark in &rich.marks {
            let touches = if selected.range.is_empty() {
                mark.start <= start && end <= mark.end
            } else {
                mark.start < end && start < mark.end
            };
            if touches && !marks.contains(mark) {
                marks.push(mark.clone());
            }
        }
    }
    if marks.is_empty() {
        return Ok(None);
    }
    let mut edits = Vec::new();
    for mark in &marks {
        unmark(source, &located.original_range(mark), &mut edits)?;
    }
    let out = apply(source, edits);
    if count_highlights(&out) + marks.len() != located.sources.tracker.highlights {
        return Err(Problem::Cannot);
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The selection of `selected` in the rendered text of `source`: from
    /// its first occurrence in one `Rich` to the end of its first in the
    /// same or a later one, as the viewer would give it.
    fn select(source: &str, from: &str, to: &str) -> Vec<Selected> {
        let doc = Document::parse(source);
        let mut parts: Vec<(Part, String)> = Vec::new();
        each_part(&doc.blocks, &mut |part, text| {
            parts.push((part, text.to_owned()))
        });
        let first = parts
            .iter()
            .position(|(_, text)| text.contains(from))
            .unwrap_or_else(|| panic!("{from:?} is not shown in {parts:?}"));
        let start = parts[first].1.find(from).unwrap();
        let last = (first..parts.len())
            .find(|&i| {
                let skip = if i == first { start } else { 0 };
                parts[i].1[skip..].contains(to)
            })
            .unwrap_or_else(|| panic!("{to:?} is not shown after {from:?}"));
        let skip = if last == first { start } else { 0 };
        let end = skip + parts[last].1[skip..].find(to).unwrap() + to.len();
        (first..=last)
            .map(|i| {
                let (part, text) = &parts[i];
                let range = match (i == first, i == last) {
                    (true, true) => start..end,
                    (true, false) => start..text.len(),
                    (false, true) => 0..end,
                    (false, false) => 0..text.len(),
                };
                Selected {
                    part: *part,
                    range,
                    digest: digest(text),
                }
            })
            .collect()
    }

    fn md(source: &str, from: &str, to: &str) -> String {
        markdown(source, &select(source, from, to)).unwrap()
    }

    fn marked(source: &str, from: &str, to: &str) -> String {
        highlight(source, &select(source, from, to))
            .unwrap()
            .expect("something to highlight")
    }

    #[test]
    fn plain_text_is_copied_as_written() {
        assert_eq!(md("Hello brave world\n", "brave", "brave"), "brave");
        assert_eq!(md("Hello brave world\n", "brave", "world"), "brave world");
    }

    #[test]
    fn markup_cut_by_the_selection_is_taken_in() {
        let source = "Some **bold text** and *more* here\n";
        assert_eq!(md(source, "text", "and"), "**bold text** and");
        assert_eq!(md(source, "Some", "bold"), "Some **bold text**");
        // Inside it, only the text.
        assert_eq!(md(source, "bold", "text"), "bold text");
        let link = "See [the docs](https://example.com) now\n";
        assert_eq!(
            md(link, "docs", "now"),
            "[the docs](https://example.com) now"
        );
    }

    #[test]
    fn code_spans_and_formulas_are_whole() {
        let source = "Run `cargo build` and $x^2$ then\n";
        assert_eq!(md(source, "build", "and"), "`cargo build` and");
        assert_eq!(md(source, "and", "and"), "and");
    }

    #[test]
    fn block_markup_before_the_start_is_taken_in() {
        assert_eq!(md("# Title\n\nText\n", "Title", "Text"), "# Title\n\nText");
        let list = "- one\n- two\n- three\n";
        assert_eq!(md(list, "one", "two"), "- one\n- two");
        assert_eq!(
            md("> quoted\n> more\n", "quoted", "more"),
            "> quoted\n> more"
        );
        let tasks = "- [ ] open\n- [x] done\n";
        assert_eq!(md(tasks, "open", "done"), "- [ ] open\n- [x] done");
        // Not where text precedes it.
        assert_eq!(md("- one two\n", "two", "two"), "two");
    }

    #[test]
    fn table_rows_keep_their_pipes() {
        let table = "| a | b |\n|---|---|\n| c | d |\n";
        assert_eq!(md(table, "a", "b"), "| a | b |");
        assert_eq!(md(table, "c", "d"), "| c | d |");
    }

    #[test]
    fn places_after_a_rewritten_wikilink_are_in_the_file() {
        let source = "See [[#Intro|the intro]] and **bold** words\n\n# Intro\n";
        assert_eq!(md(source, "bold", "words"), "**bold** words");
        assert_eq!(md(source, "intro", "and"), "[[#Intro|the intro]] and");
        assert_eq!(
            marked(source, "bold", "words"),
            "See [[#Intro|the intro]] and ==**bold** words==\n\n# Intro\n"
        );
    }

    #[test]
    fn markdown_in_html_table_cells_is_found() {
        let source = "<table><tr><td>\n\nSome **bold** words\n\n</td></tr></table>\n";
        assert_eq!(md(source, "bold", "words"), "**bold** words");
        assert_eq!(
            marked(source, "bold", "words"),
            "<table><tr><td>\n\nSome ==**bold** words==\n\n</td></tr></table>\n"
        );
    }

    #[test]
    fn places_after_a_comment_are_in_the_file() {
        let source = "Before %%a\nhidden note%% **bold** words\n";
        assert_eq!(md(source, "bold", "words"), "**bold** words");
        assert_eq!(
            marked(source, "bold", "words"),
            "Before %%a\nhidden note%% ==**bold** words==\n"
        );
    }

    #[test]
    fn code_blocks_give_their_code() {
        let source = "Text\n\n```rust\nfn main() {}\n```\n";
        assert_eq!(md(source, "main", "{}"), "main() {}");
        assert_eq!(
            md(source, "Text", "main"),
            "Text\n\n```rust\nfn main() {}\n```"
        );
    }

    #[test]
    fn highlighting_wraps_the_selection() {
        assert_eq!(
            marked("Hello brave world\n", "brave", "brave"),
            "Hello ==brave== world\n"
        );
        assert_eq!(
            marked("Some **bold text** here\n", "text", "here"),
            "Some ==**bold text** here==\n"
        );
        // Spaces at its ends stay outside.
        assert_eq!(marked("a b c\n", " b ", " b "), "a ==b== c\n");
        // Each paragraph on its own.
        assert_eq!(
            marked("First one\n\nSecond one\n", "one", "Second"),
            "First ==one==\n\n==Second== one\n"
        );
        assert_eq!(
            marked("- item one\n- item two\n", "one", "item"),
            "- item ==one==\n- ==item== two\n"
        );
    }

    #[test]
    fn highlights_taken_part_of_are_joined() {
        assert_eq!(
            marked("Some ==marked text== and more\n", "text", "and"),
            "Some ==marked text and== more\n"
        );
        let source = "Some ==marked text== here\n";
        assert_eq!(
            highlight(source, &select(source, "marked", "text")),
            Ok(None)
        );
    }

    #[test]
    fn highlights_are_removed() {
        let source = "Some ==marked text== and ==more== here\n";
        let removed = unhighlight(source, &select(source, "text", "more")).unwrap();
        assert_eq!(removed.as_deref(), Some("Some marked text and more here\n"));
        let mut point = select(source, "mark", "mark");
        point[0].range.end = point[0].range.start + 2;
        point[0].range.start = point[0].range.end;
        let removed = unhighlight(source, &point).unwrap();
        assert_eq!(
            removed.as_deref(),
            Some("Some marked text and ==more== here\n")
        );
        assert_eq!(
            unhighlight(source, &select(source, "here", "here")),
            Ok(None)
        );
    }

    #[test]
    fn lines_ending_in_cr_lf_keep_their_places() {
        let source = "# Title\r\n\r\nSome **bold** text\r\n> quote\r\n";
        assert_eq!(md(source, "bold", "text"), "**bold** text");
        assert_eq!(
            marked(source, "bold", "text"),
            "# Title\r\n\r\nSome ==**bold** text==\r\n> quote\r\n"
        );
    }

    #[test]
    fn places_in_quotes_lists_and_tables_are_found() {
        let source =
            "> a quote with **bold**\n>\n> - item *em* end\n\n| x | y **z** |\n|---|---|\n";
        assert_eq!(md(source, "with", "bold"), "with **bold**");
        assert_eq!(md(source, "em end", "end"), "*em* end");
        assert_eq!(md(source, "y", "z"), "y **z** |");
        assert_eq!(
            marked(source, "em end", "end"),
            "> a quote with **bold**\n>\n> - item ==*em* end==\n\n| x | y **z** |\n|---|---|\n"
        );
    }

    #[test]
    fn a_changed_file_is_noticed() {
        let selection = select("Hello world\n", "world", "world");
        assert_eq!(markdown("Hello there\n", &selection), Err(Problem::Changed));
    }

    #[test]
    fn html_blocks_are_not_highlighted() {
        let source = "<p align=\"center\">Centred text</p>\n";
        assert_eq!(
            highlight(source, &select(source, "Centred", "text")),
            Err(Problem::Cannot)
        );
        assert_eq!(
            md(source, "text", "text"),
            "<p align=\"center\">Centred text</p>"
        );
    }
}
