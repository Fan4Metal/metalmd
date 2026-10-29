//! The render model: a Markdown document parsed once into blocks of styled
//! text, ready to be laid out by the viewer every frame without reparsing.
//!
//! Nothing here depends on egui, so documents are built on a background
//! thread and the module is testable without a window.

use super::html::{self, Token};
use super::preprocess::{
    self, BLOCK_ID_ANCHOR_CLASS, EMBED_SIZE_TITLE_PREFIX, Rewrite, is_embed_size,
};
use super::source::{InlineTrack, Sources, Tracker};
use comrak::nodes::{AlertType, ListType, Node, NodeValue, TableAlignment};
use comrak::{Anchorizer, Arena, parse_document};
use std::collections::HashMap;
use std::ops::Range;

/// Inline style flags. Combined with `|`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style(pub u16);

impl Style {
    pub const NONE: Style = Style(0);
    pub const STRONG: Style = Style(1);
    pub const EMPH: Style = Style(1 << 1);
    pub const STRIKE: Style = Style(1 << 2);
    pub const CODE: Style = Style(1 << 3);
    pub const MARK: Style = Style(1 << 4);
    pub const INSERT: Style = Style(1 << 5);
    pub const LINK: Style = Style(1 << 6);
    pub const SUP: Style = Style(1 << 7);
    pub const MATH: Style = Style(1 << 8);
    pub const KBD: Style = Style(1 << 9);
    pub const SUB: Style = Style(1 << 10);
    /// With `MATH`: `$$ … $$` inside a paragraph, typeset in display style.
    pub const DISPLAY: Style = Style(1 << 11);
    /// One emoji shown in colour (see [`super::emoji`]), a span of its own.
    pub const EMOJI: Style = Style(1 << 12);
    /// An Obsidian tag, `#tag` or `#nested/tag`, a span of its own.
    pub const TAG: Style = Style(1 << 13);

    pub fn has(self, other: Style) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for Style {
    type Output = Style;
    fn bitor(self, rhs: Style) -> Style {
        Style(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Style {
    fn bitor_assign(&mut self, rhs: Style) {
        self.0 |= rhs.0;
    }
}

/// A run of text with one style. `link` indexes `Rich::links`.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: Style,
    pub link: Option<u32>,
}

/// Styled text: one paragraph, heading, table cell, or a piece of one
/// between images.
#[derive(Clone, Debug, Default)]
pub struct Rich {
    /// Sequential id in document order. Search results refer to it.
    pub id: u32,
    pub text: String,
    /// Contiguous, non-overlapping, covering `text`.
    pub spans: Vec<Span>,
    pub links: Vec<String>,
}

impl Rich {
    /// The link index at byte offset `at`, if any.
    pub fn link_at(&self, at: usize) -> Option<u32> {
        self.spans
            .iter()
            .find(|s| s.range.start <= at && at < s.range.end)?
            .link
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageRef {
    /// As written in the document: a URL or a path relative to it.
    pub src: String,
    pub alt: String,
    /// The link the image is wrapped in (`[![badge](img)](url)`).
    pub link: Option<String>,
    pub width: Option<f32>,
    pub height: Option<f32>,
}

#[derive(Clone, Debug)]
pub enum Inline {
    Text(Rich),
    Image(ImageRef),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

#[derive(Clone, Debug)]
pub struct ListItem {
    /// Set for a task list item.
    pub task: Option<Task>,
    pub blocks: Vec<Block>,
}

/// The box of a task list item (`- [x]`), and where its mark is in the
/// source, so `edit::apply` can tick it there. Preprocessing keeps lines
/// and leaves alone what comes before the box on its line, so the position
/// comrak gives in the preprocessed text is the same in the file's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Task {
    /// Ticked: `x` or `X` between the brackets.
    pub checked: bool,
    /// What is between the brackets: ` `, `x`, or one of the statuses
    /// Obsidian's themes draw (`/` in progress, `-` cancelled, `>`, `?`…).
    pub mark: char,
    /// The line of the character between the brackets, from 1.
    pub line: usize,
    /// Its byte offset in the line, from 1.
    pub column: usize,
}

/// A foldable callout: its number among the `<details>` (whose toggled set
/// in `ViewState` it shares) and whether it is written open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fold {
    pub index: u32,
    pub open: bool,
}

#[derive(Clone, Debug)]
pub struct Table {
    pub aligns: Vec<Align>,
    pub header: Vec<Vec<Inline>>,
    pub rows: Vec<Vec<Vec<Inline>>>,
}

#[derive(Clone, Debug)]
pub enum Block {
    Heading {
        level: u8,
        inlines: Vec<Inline>,
        align: Align,
    },
    Paragraph {
        inlines: Vec<Inline>,
        align: Align,
    },
    Code {
        id: u32,
        /// Highlight cache key (`highlight::code_key`).
        key: u64,
        lang: String,
        code: String,
    },
    /// `$$ … $$` on its own: typeset centred on its own row.
    Math {
        id: u32,
        tex: String,
    },
    Quote(Vec<Block>),
    Alert {
        kind: AlertKind,
        title: String,
        /// Set for an Obsidian callout written `[!type]-` (folded) or
        /// `[!type]+` (open), which the title row folds and unfolds.
        fold: Option<Fold>,
        blocks: Vec<Block>,
    },
    List {
        ordered: bool,
        start: usize,
        tight: bool,
        items: Vec<ListItem>,
    },
    Table(Table),
    DefinitionList(Vec<(Vec<Block>, Vec<Vec<Block>>)>),
    Footnote {
        label: String,
        blocks: Vec<Block>,
    },
    FrontMatter {
        id: u32,
        yaml: String,
        /// Its keys and values, when it is the flat YAML Obsidian writes.
        properties: Option<Vec<super::properties::Property>>,
    },
    Rule,
    /// The `<summary>` row of a `<details>`. The blocks after it, up to its
    /// `DetailsEnd` in the same list of blocks, are what it hides. They stay
    /// blocks of their own, as the Markdown between `<details>` and
    /// `</details>` is a series of comrak blocks, and anchors, find and the
    /// viewer's measuring keep working on top-level blocks.
    Details {
        /// Its number in the document, which keeps across reloads unless
        /// one is added or removed before it.
        index: u32,
        /// Written `<details open>`.
        open: bool,
        /// Empty if it has none, shown as a browser does ("Details").
        summary: Vec<Inline>,
    },
    DetailsEnd,
    /// A `> [!gallery]` callout of the reader's Media Gallery plugin, made
    /// of pictures only: justified rows of them, or with `gallery-masonry`
    /// columns.
    Gallery {
        masonry: bool,
        images: Vec<ImageRef>,
    },
    /// An Obsidian note embed, `![[Note]]` or `![[Note#Heading]]`, alone
    /// in its paragraph: the note's blocks in a frame of their own.
    Embed {
        /// The note's name as written, `Note > Heading` for a part.
        title: String,
        /// The link to the note, `#anchor` for a part of it.
        src: String,
        /// Filled by `markdown::embed::fill` once the note is read.
        content: Option<Result<Vec<Block>, String>>,
    },
}

/// An entry of the table of contents.
#[derive(Clone, Debug, PartialEq)]
pub struct HeadingEntry {
    pub level: u8,
    pub text: String,
    pub slug: String,
    /// Index of the top-level block that contains the heading.
    pub block: usize,
}

/// A searchable piece of text: a `Rich` or a code block.
#[derive(Clone, Debug)]
pub struct SearchText {
    pub id: u32,
    pub block: usize,
    pub text: String,
}

/// Which CJK scripts the document uses, so the matching fallback font can be
/// loaded only when it is needed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scripts {
    pub han_or_kana: bool,
    pub hangul: bool,
}

impl Scripts {
    /// The scripts of both.
    pub fn with(self, other: Scripts) -> Scripts {
        Scripts {
            han_or_kana: self.han_or_kana || other.han_or_kana,
            hangul: self.hangul || other.hangul,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Document {
    pub blocks: Vec<Block>,
    /// The line (from 1) each top-level block starts at in the file.
    pub lines: Vec<usize>,
    pub headings: Vec<HeadingEntry>,
    /// Anchor id → top-level block index (headings, footnotes, block ids).
    pub anchors: HashMap<String, usize>,
    pub search: Vec<SearchText>,
    /// The `src` of every image, once each: what to forget when the
    /// document is closed.
    pub images: Vec<String>,
    pub has_code: bool,
    pub scripts: Scripts,
    /// The ids given out (`Rich`, code, formulas), embedded notes' too.
    pub ids: u32,
    /// The numbers given out to `<details>` and foldable callouts,
    /// embedded notes' too.
    pub folds: u32,
}

impl Document {
    /// Parses `source` (already decoded) into the render model.
    pub fn parse(source: &str) -> Document {
        Document::build(source, false).0
    }

    /// [`Document::parse`], also keeping where its text is in `source`
    /// (see `markdown::source`).
    pub(crate) fn parse_with_sources(source: &str) -> (Document, Sources) {
        let (doc, sources) = Document::build(source, true);
        (doc, sources.expect("kept when asked"))
    }

    fn build(source: &str, track: bool) -> (Document, Option<Sources>) {
        let mut passes = Vec::new();
        if track {
            passes.push(copy_marker_rewrites(source));
        }
        let source = &*without_copy_markers(source);
        let (preprocessed, rewrites) = if track {
            let (text, rewrites) = preprocess::preprocess_with_rewrites(source);
            (text, Some(rewrites))
        } else {
            (preprocess::preprocess(source), None)
        };
        let arena = Arena::new();
        let options = preprocess::markdown_options();
        let root = parse_document(&arena, &preprocessed, &options);

        let mut builder = Builder {
            track: track.then(|| Box::new(Tracker::new(&preprocessed))),
            ..Builder::default()
        };
        // Footnote numbers are assigned in reference order; definitions come
        // last in comrak's tree, so collect the numbers first.
        for node in root.descendants() {
            if let NodeValue::FootnoteReference(r) = &node.data().value {
                builder.footnote_ix.entry(r.name.clone()).or_insert(r.ix);
            }
        }
        let mut blocks = Vec::new();
        let mut lines = Vec::new();
        for child in root.children() {
            // One comrak block can become zero blocks (a comment) or several
            // (an HTML block with two paragraphs), so the index is the length
            // of the output, not the child's position.
            builder.top = blocks.len();
            // Preprocessing keeps lines, so this is the file's line.
            let line = child.data().sourcepos.start.line;
            builder.line = line;
            builder.block(child, &mut blocks);
            lines.resize(blocks.len(), line);
        }
        builder.finish_html_table(&mut blocks);
        lines.resize(blocks.len(), builder.line);
        for (block, line) in std::mem::take(&mut builder.table_lines) {
            if let Some(at) = lines.get_mut(block) {
                *at = line;
            }
        }
        let sources = builder.track.take().map(|tracker| {
            passes.extend(rewrites.into_iter().flatten());
            Sources {
                tracker: *tracker,
                passes,
            }
        });
        let doc = Document {
            blocks,
            lines,
            headings: builder.headings,
            anchors: builder.anchors,
            search: builder.search,
            images: builder.images.into_iter().collect(),
            has_code: builder.has_code,
            scripts: detect_scripts(source),
            ids: builder.next_id,
            folds: builder.details,
        };
        (doc, sources)
    }
}

/// The copy markers [`without_copy_markers`] takes out, as rewrites.
fn copy_marker_rewrites(source: &str) -> Vec<Rewrite> {
    source
        .char_indices()
        .filter(|&(_, c)| matches!(c, '\u{2062}'..='\u{2064}'))
        .map(|(at, c)| Rewrite {
            from: at..at + c.len_utf8(),
            len: 0,
        })
        .collect()
}

/// The invisible operators U+2062–U+2064, which `ui::copy` marks pictures
/// with in the text egui copies, left out of the document: in its text they
/// would be read as such marks. They draw as nothing, and copying dropped
/// them anyway.
fn without_copy_markers(source: &str) -> std::borrow::Cow<'_, str> {
    const MARKERS: [char; 3] = ['\u{2062}', '\u{2063}', '\u{2064}'];
    // Three searches for one character each (memchr) cost far less than one
    // for any of three, on a large file.
    if MARKERS.iter().any(|&c| source.contains(c)) {
        source.replace(MARKERS, "").into()
    } else {
        source.into()
    }
}

fn detect_scripts(source: &str) -> Scripts {
    let mut scripts = Scripts::default();
    if source.is_ascii() {
        return scripts;
    }
    for c in source.chars() {
        match c as u32 {
            0x3040..=0x30FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0xFF00..=0xFFEF => scripts.han_or_kana = true,
            0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => scripts.hangul = true,
            _ => {}
        }
        if scripts.han_or_kana && scripts.hangul {
            break;
        }
    }
    scripts
}

/// Containers (quotes, lists, alerts…) nested deeper than this are not
/// kept: their blocks go into the container at this depth. The viewer draws
/// nesting recursively, and a few hundred levels (a 300-byte file of `>`)
/// would overflow the main thread's stack; building them would overflow the
/// loader's.
const MAX_BLOCK_DEPTH: usize = 32;

/// Inline markup (emphasis, links…) nested deeper than this becomes plain
/// text, for the same reason.
const MAX_INLINE_DEPTH: usize = 64;

/// An HTML `<table>` being built. It can run over several blocks: an HTML
/// block ends at a blank line, so rows set apart by one are blocks of their
/// own, and so is Markdown set apart between `<td>` and `</td>`, which goes
/// into the cell.
struct HtmlTable {
    /// The `Builder::depth` it was opened at: blocks at it go into the
    /// table, and it is finished before its container is.
    depth: usize,
    /// The file's line it starts on, when opened at the top level.
    line: usize,
    /// Each row's cells, with whether a cell is a `<th>`. The last row is
    /// being filled while `row_open`, its last cell while `cell_open`.
    rows: Vec<Vec<(Vec<Inline>, bool)>>,
    row_open: bool,
    cell_open: bool,
    /// Each column's alignment, from the first cell in it that names one.
    aligns: Vec<Option<Align>>,
    /// `<table>`s open inside a cell: their markup is left out and their
    /// text goes into the cell.
    nested: usize,
    /// `<details>` open inside a cell: only their text is kept.
    details: usize,
    /// The text of its `<caption>`, shown above it, while `in_caption`.
    caption: Vec<Inline>,
    in_caption: bool,
}

impl HtmlTable {
    fn start_row(&mut self) {
        self.rows.push(Vec::new());
        self.row_open = true;
        self.cell_open = false;
    }

    fn end_row(&mut self) {
        self.row_open = false;
        self.cell_open = false;
    }

    fn start_cell(&mut self, header: bool, align: Option<Align>) {
        if !self.row_open {
            self.start_row();
        }
        let row = self.rows.last_mut().expect("a row was just started");
        let column = row.len();
        row.push((Vec::new(), header));
        if self.aligns.len() <= column {
            self.aligns.resize(column + 1, None);
        }
        if self.aligns[column].is_none() {
            self.aligns[column] = align;
        }
        self.cell_open = true;
    }

    /// The cell being filled, a new one if none is: `align` is its
    /// alignment when it names none, the one around the table.
    fn cell(&mut self, align: Option<Align>) -> &mut Vec<Inline> {
        if !self.cell_open {
            self.start_cell(false, align);
        }
        let row = self.rows.last_mut().expect("a cell is open");
        &mut row.last_mut().expect("a cell is open").0
    }

    /// The table's blocks: its caption, centred, and the table, whose first
    /// row is its header if it is all `<th>`, and whose rows shorter than
    /// the longest are filled with empty cells.
    fn finish(self) -> Vec<Block> {
        let mut blocks = Vec::new();
        if !inlines_are_blank(&self.caption) {
            blocks.push(Block::Paragraph {
                inlines: self.caption,
                align: Align::Center,
            });
        }
        let mut rows: Vec<_> = self.rows.into_iter().filter(|r| !r.is_empty()).collect();
        // Nothing in any cell (a table cut short by a list in its cell).
        if rows.iter().flatten().all(|(cell, _)| cell.is_empty()) {
            return blocks;
        }
        let Some(columns) = rows.iter().map(Vec::len).max() else {
            return blocks;
        };
        let has_header = rows[0].iter().all(|(_, header)| *header);
        let fill = |row: Vec<(Vec<Inline>, bool)>| {
            let mut cells: Vec<Vec<Inline>> = row.into_iter().map(|(cell, _)| cell).collect();
            cells.resize_with(columns, Vec::new);
            cells
        };
        let header = if has_header {
            fill(rows.remove(0))
        } else {
            Vec::new()
        };
        let mut aligns: Vec<Align> = self
            .aligns
            .into_iter()
            .map(Option::unwrap_or_default)
            .collect();
        aligns.resize(columns, Align::Left);
        blocks.push(Block::Table(Table {
            aligns,
            header,
            rows: rows.into_iter().map(fill).collect(),
        }));
        blocks
    }
}

/// Tags whose meaning is a part of a table.
fn is_table_tag(name: &str) -> bool {
    matches!(
        name,
        "table" | "thead" | "tbody" | "tfoot" | "tr" | "td" | "th" | "caption" | "colgroup" | "col"
    )
}

/// A table cell's alignment as written: `align` or a `text-align` style.
fn html_cell_align(attrs: &str) -> Option<Align> {
    let named = match html::attr(attrs, "align") {
        Some(align) => align.into_owned(),
        None => {
            let style = html::attr(attrs, "style")?.to_ascii_lowercase();
            let (_, rest) = style.split_once("text-align")?;
            let value = rest.trim_start().strip_prefix(':')?;
            value.split(';').next()?.to_owned()
        }
    };
    match named.trim().to_ascii_lowercase().as_str() {
        "center" => Some(Align::Center),
        "right" => Some(Align::Right),
        "left" => Some(Align::Left),
        _ => None,
    }
}

#[derive(Default)]
struct Builder {
    anchorizer: Anchorizer,
    /// Containers open around the block being built.
    depth: usize,
    next_id: u32,
    /// Index of the top-level block being built (into the final `blocks`).
    top: usize,
    headings: Vec<HeadingEntry>,
    anchors: HashMap<String, usize>,
    search: Vec<SearchText>,
    images: std::collections::BTreeSet<String>,
    has_code: bool,
    footnote_ix: HashMap<String, u32>,
    /// Alignment opened by an HTML block (`<p align="center">`) that is still
    /// open, so Markdown between it and its closing tag is aligned too.
    html_align: Vec<(String, Align)>,
    /// Style applied to everything built while set (table headers, terms).
    base_style: Style,
    /// `<details>` built so far.
    details: u32,
    /// The HTML table open, if any.
    html_table: Option<HtmlTable>,
    /// The file's line of the top-level block being built.
    line: usize,
    /// Top-level tables finished in a later block than they started in,
    /// with the line they started on.
    table_lines: Vec<(usize, usize)>,
    /// Places in the source, kept for `markdown::source` only.
    track: Option<Box<Tracker>>,
    /// The block being built, when places are kept.
    block_source: Option<Range<usize>>,
    /// The HTML block being built, when places are kept.
    html_source: Option<Range<usize>>,
}

impl Builder {
    /// A foldable callout's state, numbered with the `<details>`, whose
    /// toggling it shares in the viewer.
    fn fold(&mut self, open: bool) -> Fold {
        let index = self.details;
        self.details += 1;
        Fold { index, open }
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn current_align(&self) -> Align {
        self.html_align.last().map_or(Align::Left, |(_, a)| *a)
    }

    fn add_search(&mut self, id: u32, text: &str) {
        if !text.trim().is_empty() {
            self.search.push(SearchText {
                id,
                block: self.top,
                text: text.to_owned(),
            });
        }
    }

    fn children_blocks<'a>(&mut self, node: Node<'a>) -> Vec<Block> {
        let mut out = Vec::new();
        self.depth += 1;
        if self.depth >= MAX_BLOCK_DEPTH {
            // Iterates rather than recursing: only the leaf blocks below.
            for descendant in node.descendants().skip(1) {
                let value = &descendant.data().value;
                let part_of_table = matches!(value, NodeValue::TableRow(_) | NodeValue::TableCell);
                if value.block() && !is_container(value) && !part_of_table {
                    self.block(descendant, &mut out);
                }
            }
        } else {
            for child in node.children() {
                self.block(child, &mut out);
            }
        }
        // A table left open ends with its container.
        self.finish_html_table(&mut out);
        self.depth -= 1;
        out
    }

    /// Whether an HTML table is open at the depth being built.
    fn in_html_table(&self) -> bool {
        self.html_table
            .as_ref()
            .is_some_and(|table| table.depth == self.depth)
    }

    /// Pushes the HTML table open at this depth, if any, to `out`.
    fn finish_html_table(&mut self, out: &mut Vec<Block>) {
        if !self.in_html_table() {
            return;
        }
        let Some(table) = self.html_table.take() else {
            return;
        };
        let (depth, line) = (table.depth, table.line);
        for block in table.finish() {
            if depth == 0 && line != self.line {
                self.table_lines.push((out.len(), line));
            }
            out.push(block);
        }
    }

    /// The text gathered in `ctx`, from an HTML block inside a table, to
    /// the cell or caption it is in.
    fn html_cell_text(&mut self, ctx: &mut InlineCtx) {
        let Some(mut inlines) = self.take_html_inlines(ctx) else {
            return;
        };
        match self.html_table.as_mut() {
            Some(table) if table.in_caption => table.caption.append(&mut inlines),
            _ => self.add_to_html_table(inlines),
        }
    }

    /// Takes `token` if it is a table's tag and opens a table or one is
    /// open, telling whether it did.
    fn html_table_tag(
        &mut self,
        ctx: &mut InlineCtx,
        token: &Token<'_>,
        out: &mut Vec<Block>,
    ) -> bool {
        // `<td/>` opens a cell as `<td>` does in a browser.
        let (name, open, attrs) = match token {
            Token::Open { name, attrs, .. } => (name.as_str(), true, *attrs),
            Token::Close { name } => (name.as_str(), false, ""),
            Token::Text(_) => return false,
        };
        if !is_table_tag(name) {
            return false;
        }
        if !self.in_html_table() {
            if !(open && name == "table") {
                return false;
            }
            self.flush_html_paragraph(ctx, None, out);
            self.html_table = Some(HtmlTable {
                depth: self.depth,
                line: self.line,
                rows: Vec::new(),
                row_open: false,
                cell_open: false,
                aligns: Vec::new(),
                nested: 0,
                details: 0,
                caption: Vec::new(),
                in_caption: false,
            });
            return true;
        }
        let table = self.html_table.as_mut().expect("a table is open");
        if table.nested > 0 || open && name == "table" {
            // A table inside a cell: its cells are words of the cell.
            match (name, open) {
                ("table", true) => table.nested += 1,
                ("table", false) => table.nested -= 1,
                ("td" | "th" | "tr", false) => ctx.push_text(" ", Style::NONE),
                _ => {}
            }
            return true;
        }
        // What came before the tag is the cell's (or caption's) it ends.
        self.html_cell_text(ctx);
        ctx.html_close("th");
        let around = self.html_align.last().map(|(_, align)| *align);
        let table = self.html_table.as_mut().expect("a table is open");
        match (name, open) {
            ("table", false) => self.finish_html_table(out),
            ("tr", true) => {
                table.end_row();
                table.start_row();
            }
            ("tr", false) => table.end_row(),
            ("td" | "th", true) => {
                table.start_cell(name == "th", html_cell_align(attrs).or(around));
                if name == "th" {
                    ctx.html_open("th", Style::STRONG);
                }
            }
            ("td" | "th", false) => table.cell_open = false,
            ("caption", open) => {
                table.end_row();
                table.in_caption = open;
            }
            // thead, tbody, tfoot, colgroup, col.
            _ => {}
        }
        true
    }

    /// Adds `inlines` to the cell being filled, or to a new one. What a
    /// cell already holds ends its line first, as a paragraph would.
    fn add_to_html_table(&mut self, mut inlines: Vec<Inline>) {
        if inlines_are_blank(&inlines) {
            return;
        }
        let align = self.html_align.last().map(|(_, align)| *align);
        let Some(table) = self.html_table.as_mut() else {
            return;
        };
        let cell = table.cell(align);
        match cell.last_mut() {
            None => {}
            Some(Inline::Text(rich)) => {
                rich.text.push('\n');
                if let Some(span) = rich.spans.last_mut() {
                    span.range.end = rich.text.len();
                }
            }
            Some(Inline::Image(_)) => {
                let id = self.next_id;
                self.next_id += 1;
                cell.push(Inline::Text(Rich {
                    id,
                    text: "\n".to_owned(),
                    spans: vec![Span {
                        range: 0..1,
                        style: Style::NONE,
                        link: None,
                    }],
                    links: Vec::new(),
                }));
            }
        }
        cell.append(&mut inlines);
    }

    /// Builds `node` into `out`. At the top level `self.top` must equal the
    /// index the first pushed block will get.
    fn block<'a>(&mut self, node: Node<'a>, out: &mut Vec<Block>) {
        if self.in_html_table() {
            let value = &node.data().value;
            let heading = matches!(value, NodeValue::Heading(_));
            match value {
                NodeValue::HtmlBlock(_) => {}
                // Markdown between `<td>` and `</td>`: its text is the cell's.
                NodeValue::Paragraph | NodeValue::Heading(_) => {
                    let saved = self.base_style;
                    if heading {
                        self.base_style |= Style::STRONG;
                    }
                    let inlines = self.inlines(node);
                    self.base_style = saved;
                    self.add_to_html_table(inlines);
                    return;
                }
                // Anything else ends the table, as it cannot go in a cell.
                _ => self.finish_html_table(out),
            }
        }
        let data = node.data();
        if let Some(track) = &self.track {
            self.block_source = Some(track.range(&data.sourcepos));
        }
        match &data.value {
            NodeValue::FrontMatter(raw) => {
                let yaml = strip_front_matter_fences(raw);
                let id = self.alloc_id();
                self.add_search(id, &yaml);
                self.track_block(id);
                let properties = super::properties::parse(&yaml);
                out.push(Block::FrontMatter {
                    id,
                    yaml,
                    properties,
                });
            }
            NodeValue::Paragraph => {
                if let Some(tex) = lone_display_math(node) {
                    let id = self.alloc_id();
                    self.add_search(id, &tex);
                    out.push(Block::Math { id, tex });
                    return;
                }
                if let Some(embeds) = lone_note_embeds(node) {
                    out.extend(embeds.into_iter().map(|(title, src)| Block::Embed {
                        title,
                        src,
                        content: None,
                    }));
                    return;
                }
                let inlines = self.inlines(node);
                if !inlines_are_blank(&inlines) {
                    out.push(Block::Paragraph {
                        inlines,
                        align: self.current_align(),
                    });
                }
            }
            NodeValue::Heading(heading) => {
                let text = collect_text(node);
                let slug = self.anchorizer.anchorize(text.trim());
                let inlines = self.inlines(node);
                let display = plain_text(&inlines);
                self.anchors.entry(slug.clone()).or_insert(self.top);
                if !display.trim().is_empty() {
                    self.headings.push(HeadingEntry {
                        level: heading.level,
                        // One line in the table of contents.
                        text: display.trim().replace('\n', " "),
                        slug: slug.clone(),
                        block: self.top,
                    });
                }
                out.push(Block::Heading {
                    level: heading.level,
                    inlines,
                    align: self.current_align(),
                });
            }
            NodeValue::CodeBlock(code_block) => {
                let lang = code_block
                    .info
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_owned();
                let code = code_block.literal.clone();
                // GitHub's display math: a fence whose language is `math`.
                if lang.eq_ignore_ascii_case("math") {
                    let tex = code.trim_end_matches(['\n', '\r']).to_owned();
                    let id = self.alloc_id();
                    self.add_search(id, &tex);
                    out.push(Block::Math { id, tex });
                    return;
                }
                self.push_code(lang, code, out);
            }
            NodeValue::HtmlBlock(html_block) => {
                let literal = html_block.literal.clone();
                drop(data);
                self.html_source = self.block_source.clone();
                self.html_block(&literal, out);
                self.html_source = None;
            }
            NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => {
                drop(data);
                if let Some((kind, title, fold, name)) = take_callout(node) {
                    let blocks = self.children_blocks(node);
                    // The reader's Media Gallery plugin: a callout of
                    // pictures only, laid out in rows or columns.
                    if let Some(masonry) = match name.as_str() {
                        "gallery" => Some(false),
                        "gallery-masonry" => Some(true),
                        _ => None,
                    } && let Some(images) = gallery_images(&blocks)
                    {
                        out.push(Block::Gallery { masonry, images });
                        return;
                    }
                    let fold = fold.map(|open| self.fold(open));
                    out.push(Block::Alert {
                        kind,
                        title,
                        fold,
                        blocks,
                    });
                    return;
                }
                let blocks = self.children_blocks(node);
                out.push(Block::Quote(blocks));
            }
            NodeValue::Alert(alert) => {
                let kind = match alert.alert_type {
                    AlertType::Note => AlertKind::Note,
                    AlertType::Tip => AlertKind::Tip,
                    AlertType::Important => AlertKind::Important,
                    AlertType::Warning => AlertKind::Warning,
                    AlertType::Caution => AlertKind::Caution,
                };
                // Obsidian's fold mark, `[!NOTE]-` or `[!NOTE]+`, comes
                // as the start of the title.
                let (fold, title) = fold_mark(alert.title.as_deref().unwrap_or(""));
                let title = Some(title.trim())
                    .filter(|title| !title.is_empty())
                    .map_or_else(
                        || alert.alert_type.default_title().to_owned(),
                        str::to_owned,
                    );
                drop(data);
                let fold = fold.map(|open| self.fold(open));
                let blocks = self.children_blocks(node);
                out.push(Block::Alert {
                    kind,
                    title,
                    fold,
                    blocks,
                });
            }
            NodeValue::List(list) => {
                let list = *list;
                drop(data);
                let mut items = Vec::new();
                for item in node.children() {
                    let task = match &item.data().value {
                        NodeValue::TaskItem(task) => Some(Task {
                            checked: matches!(task.symbol, Some('x' | 'X')),
                            mark: task.symbol.unwrap_or(' '),
                            line: task.symbol_sourcepos.start.line,
                            column: task.symbol_sourcepos.start.column,
                        }),
                        _ => None,
                    };
                    let blocks = self.children_blocks(item);
                    items.push(ListItem { task, blocks });
                }
                out.push(Block::List {
                    ordered: list.list_type == ListType::Ordered,
                    start: list.start,
                    tight: list.tight,
                    items,
                });
            }
            NodeValue::Table(table) => {
                let aligns = table
                    .alignments
                    .iter()
                    .map(|a| match a {
                        TableAlignment::Center => Align::Center,
                        TableAlignment::Right => Align::Right,
                        _ => Align::Left,
                    })
                    .collect();
                drop(data);
                let mut header = Vec::new();
                let mut rows = Vec::new();
                for row in node.children() {
                    let is_header = matches!(row.data().value, NodeValue::TableRow(true));
                    let saved = self.base_style;
                    if is_header {
                        self.base_style |= Style::STRONG;
                    }
                    let cells: Vec<Vec<Inline>> =
                        row.children().map(|cell| self.inlines(cell)).collect();
                    self.base_style = saved;
                    if is_header {
                        header = cells;
                    } else {
                        rows.push(cells);
                    }
                }
                out.push(Block::Table(Table {
                    aligns,
                    header,
                    rows,
                }));
            }
            NodeValue::ThematicBreak => out.push(Block::Rule),
            NodeValue::FootnoteDefinition(definition) => {
                let name = definition.name.clone();
                drop(data);
                let label = self
                    .footnote_ix
                    .get(&name)
                    .map_or_else(|| name.clone(), |ix| ix.to_string());
                self.anchors.insert(format!("fn-{name}"), self.top);
                let blocks = self.children_blocks(node);
                out.push(Block::Footnote { label, blocks });
            }
            NodeValue::DescriptionList => {
                drop(data);
                let mut items = Vec::new();
                for item in node.children() {
                    let mut terms = Vec::new();
                    let mut details = Vec::new();
                    for part in item.children() {
                        let is_term = matches!(part.data().value, NodeValue::DescriptionTerm);
                        if is_term {
                            let saved = self.base_style;
                            self.base_style |= Style::STRONG;
                            terms.extend(self.children_blocks(part));
                            self.base_style = saved;
                        } else {
                            details.push(self.children_blocks(part));
                        }
                    }
                    items.push((terms, details));
                }
                out.push(Block::DefinitionList(items));
            }
            _ => {
                drop(data);
                for child in node.children() {
                    self.block(child, out);
                }
            }
        }
    }

    fn inlines<'a>(&mut self, node: Node<'a>) -> Vec<Inline> {
        let mut ctx = InlineCtx::new(self.base_style);
        ctx.track = self.track.as_ref().map(|_| Box::default());
        for child in node.children() {
            ctx.node(self, child, Style::NONE);
        }
        ctx.finish(self)
    }

    /// An HTML block: paragraphs, headings and images; unknown tags dropped.
    /// A code block of `code`, without its last line end.
    fn push_code(&mut self, lang: String, mut code: String, out: &mut Vec<Block>) {
        if code.ends_with('\n') {
            code.pop();
            if code.ends_with('\r') {
                code.pop();
            }
        }
        let id = self.alloc_id();
        self.add_search(id, &code);
        self.track_block(id);
        self.has_code = true;
        let key = super::highlight::code_key(&lang, &code);
        out.push(Block::Code {
            id,
            key,
            lang,
            code,
        });
    }

    /// The code of a `<pre>`: a line end right after `<pre>` is not part
    /// of it, as in a browser.
    fn push_pre(&mut self, (lang, code): (String, String), out: &mut Vec<Block>) {
        let code = match code
            .strip_prefix("\r\n")
            .or_else(|| code.strip_prefix('\n'))
        {
            Some(rest) => rest.to_owned(),
            None => code,
        };
        self.push_code(lang, code, out);
    }

    /// Notes that block `id` (code or front matter) is the one being built.
    fn track_block(&mut self, id: u32) {
        if let (Some(track), Some(source)) = (&mut self.track, &self.block_source) {
            track.blocks.insert(id, source.clone());
        }
    }

    fn html_block(&mut self, literal: &str, out: &mut Vec<Block>) {
        let mut ctx = InlineCtx::new(self.base_style);
        ctx.track = self.track.as_ref().map(|_| Box::default());
        ctx.collapse_whitespace = true;
        let mut heading: Option<u8> = None;
        // Inside `<pre>`: its language and its text so far, as written.
        let mut pre: Option<(String, String)> = None;
        // The `Details` in `out` that a `<summary>` would name.
        let mut summary_of: Option<usize> = None;
        for token in html::tokenize(literal) {
            if pre.is_none() && self.html_table_tag(&mut ctx, &token, out) {
                continue;
            }
            if let Some((lang, code)) = pre.as_mut() {
                match &token {
                    Token::Close { name } if name == "pre" => {
                        if let Some(pre) = pre.take() {
                            self.push_pre(pre, out);
                        }
                    }
                    Token::Text(text) => code.push_str(&html::decode_entities(text)),
                    // Markup inside (`<code>`, `<b>`, `<span>`) is dropped.
                    Token::Open { name, attrs, .. } => {
                        if name == "br" {
                            code.push('\n');
                        } else if lang.is_empty() {
                            *lang = html_code_language(attrs);
                        }
                    }
                    Token::Close { .. } => {}
                }
                continue;
            }
            match &token {
                Token::Open {
                    name,
                    attrs,
                    self_closing: false,
                } if name == "pre" && !self.in_html_table() => {
                    self.flush_html_paragraph(&mut ctx, heading.take(), out);
                    pre = Some((html_code_language(attrs), String::new()));
                }
                Token::Open {
                    name,
                    attrs,
                    self_closing,
                } if is_html_block_tag(name) => {
                    self.flush_html_paragraph(&mut ctx, heading.take(), out);
                    if *self_closing {
                        continue;
                    }
                    let align = match html::attr(attrs, "align").as_deref() {
                        Some(a) if a.eq_ignore_ascii_case("center") => Align::Center,
                        Some(a) if a.eq_ignore_ascii_case("right") => Align::Right,
                        Some(a) if a.eq_ignore_ascii_case("left") => Align::Left,
                        _ if name == "center" => Align::Center,
                        _ => self.current_align(),
                    };
                    self.html_align.push((name.clone(), align));
                    if let Some(level) = html_heading_level(name) {
                        heading = Some(level);
                    }
                    // In a table cell, which holds only text, a heading is
                    // bold and `<details>` only its text.
                    let in_table = self.in_html_table();
                    if in_table && html_heading_level(name).is_some() {
                        ctx.html_open(name, Style::STRONG);
                    }
                    if name == "details"
                        && let Some(table) = self.html_table.as_mut().filter(|_| in_table)
                    {
                        table.details += 1;
                    } else if name == "details" {
                        out.push(Block::Details {
                            index: self.details,
                            open: html::attr(attrs, "open").is_some(),
                            summary: Vec::new(),
                        });
                        self.details += 1;
                        summary_of = Some(out.len() - 1);
                    }
                    if name == "dt" || name == "th" {
                        ctx.html_open(name, Style::STRONG);
                    }
                }
                Token::Close { name } if is_html_block_tag(name) => {
                    if name == "dt" || name == "th" || html_heading_level(name).is_some() {
                        ctx.html_close(name);
                    }
                    if name == "summary"
                        && let Some(at) = summary_of.take()
                        && let Some(Block::Details { summary, .. }) = out.get_mut(at)
                    {
                        *summary = self.take_html_inlines(&mut ctx).unwrap_or_default();
                    }
                    self.flush_html_paragraph(&mut ctx, heading.take(), out);
                    if name == "details" {
                        let inner = self
                            .html_table
                            .as_mut()
                            .filter(|table| table.depth == self.depth && table.details > 0);
                        match inner {
                            Some(table) => table.details -= 1,
                            None => {
                                // The end of details around the table: it
                                // ends inside them.
                                self.finish_html_table(out);
                                summary_of = None;
                                out.push(Block::DetailsEnd);
                            }
                        }
                    }
                    if let Some(pos) = self.html_align.iter().rposition(|(n, _)| n == name) {
                        self.html_align.truncate(pos);
                    }
                }
                _ => ctx.html_token(self, &token),
            }
        }
        // An unclosed `<pre>` runs to the end of the block.
        if let Some(pre) = pre {
            self.push_pre(pre, out);
        }
        self.flush_html_paragraph(&mut ctx, heading, out);
        // A paragraph or heading left open ends with its block, as a
        // browser closes it before the next one: an unclosed
        // `<p align="center">` must not centre the rest of the document.
        // Containers (`<div>`, `<center>`) span the Markdown that follows.
        self.html_align.retain(|(name, _)| is_html_container(name));
    }

    fn flush_html_paragraph(
        &mut self,
        ctx: &mut InlineCtx,
        heading: Option<u8>,
        out: &mut Vec<Block>,
    ) {
        // Inside a table it is a line of the cell.
        if self.in_html_table() {
            self.html_cell_text(ctx);
            return;
        }
        let Some(inlines) = self.take_html_inlines(ctx) else {
            return;
        };
        let align = self.current_align();
        match heading {
            Some(level) => out.push(Block::Heading {
                level,
                inlines,
                align,
            }),
            None => out.push(Block::Paragraph { inlines, align }),
        }
    }

    /// The text gathered in `ctx` since the last block, `None` if blank;
    /// `ctx` starts afresh, keeping the inline tags still open.
    fn take_html_inlines(&mut self, ctx: &mut InlineCtx) -> Option<Vec<Inline>> {
        let fresh = InlineCtx {
            collapse_whitespace: true,
            html_style: ctx.html_style,
            html_styles: std::mem::take(&mut ctx.html_styles),
            html_links: std::mem::take(&mut ctx.html_links),
            link: ctx.link.clone(),
            track: ctx.track.as_ref().map(|_| Box::default()),
            ..InlineCtx::new(ctx.base)
        };
        let finished = std::mem::replace(ctx, fresh);
        let mut inlines = finished.finish(self);
        trim_inlines(&mut inlines);
        (!inlines_are_blank(&inlines)).then_some(inlines)
    }
}

/// A block that holds other blocks, as opposed to text, code or a table.
fn is_container(value: &NodeValue) -> bool {
    matches!(
        value,
        NodeValue::Document
            | NodeValue::BlockQuote
            | NodeValue::MultilineBlockQuote(_)
            | NodeValue::Alert(_)
            | NodeValue::List(_)
            | NodeValue::Item(_)
            | NodeValue::TaskItem(_)
            | NodeValue::FootnoteDefinition(_)
            | NodeValue::DescriptionList
            | NodeValue::DescriptionItem(_)
            | NodeValue::DescriptionTerm
            | NodeValue::DescriptionDetails
    )
}

fn is_html_block_tag(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "center"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "details"
            | "summary"
            | "table"
            | "thead"
            | "tbody"
            | "tr"
            | "td"
            | "th"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "nav"
            | "blockquote"
            | "pre"
            | "picture"
            | "figure"
            | "figcaption"
            | "hr"
    )
}

/// A block tag that can hold other blocks, so its alignment carries over
/// to the Markdown blocks until its closing tag.
fn is_html_container(name: &str) -> bool {
    matches!(
        name,
        "div"
            | "center"
            | "details"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "nav"
            | "blockquote"
            | "figure"
            | "table"
            | "thead"
            | "tbody"
            | "ul"
            | "ol"
            | "dl"
    )
}

/// The language of `<pre>` or `<code>` from its attributes: `lang="rust"`,
/// or `class="language-rust"` (`lang-rust`, `highlight-source-rust`) as
/// highlighters and GitHub write it. Empty if none.
fn html_code_language(attrs: &str) -> String {
    if let Some(lang) = html::attr(attrs, "lang") {
        return lang.trim().to_owned();
    }
    let class = html::attr(attrs, "class").unwrap_or_default();
    class
        .split_whitespace()
        .find_map(|c| {
            c.strip_prefix("language-")
                .or_else(|| c.strip_prefix("lang-"))
                .or_else(|| c.strip_prefix("highlight-source-"))
        })
        .unwrap_or_default()
        .to_owned()
}

fn html_heading_level(name: &str) -> Option<u8> {
    match name {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

/// Builds the inlines of one block.
struct InlineCtx {
    parts: Vec<Inline>,
    cur: Rich,
    base: Style,
    /// Style opened by inline HTML tags (`<b>`, `<kbd>`…).
    html_style: Style,
    html_styles: Vec<(String, Style)>,
    /// One entry per open `<a>`: whether it pushed a link.
    html_links: Vec<bool>,
    link: Option<String>,
    collapse_whitespace: bool,
    /// Inline nodes open around the one being added.
    depth: usize,
    /// Where the text came from, when the builder keeps it.
    track: Option<Box<InlineTrack>>,
}

impl InlineCtx {
    fn new(base: Style) -> InlineCtx {
        InlineCtx {
            parts: Vec::new(),
            cur: Rich::default(),
            base,
            html_style: Style::NONE,
            html_styles: Vec::new(),
            html_links: Vec::new(),
            link: None,
            collapse_whitespace: false,
            depth: 0,
            track: None,
        }
    }

    fn push_text(&mut self, text: &str, style: Style) {
        self.push_span(text, style, true);
    }

    /// Appends `text`, continuing the last span if it has the same style and
    /// `merge` allows it.
    fn push_span(&mut self, text: &str, style: Style, merge: bool) {
        if text.is_empty() {
            return;
        }
        let style = style | self.base | self.html_style;
        let text: std::borrow::Cow<'_, str> = if self.collapse_whitespace {
            let mut collapsed = String::with_capacity(text.len());
            // As in a browser, whitespace between two images is a space
            // (README badge rows); at the start of a paragraph it is none.
            let mut last_space = if self.cur.text.is_empty() {
                match self.parts.last() {
                    None => true,
                    Some(Inline::Text(rich)) => rich.text.ends_with([' ', '\n']),
                    Some(Inline::Image(_)) => false,
                }
            } else {
                self.cur.text.ends_with([' ', '\n'])
            };
            for c in text.chars() {
                if c.is_whitespace() && c != '\u{a0}' {
                    if !last_space {
                        collapsed.push(' ');
                        last_space = true;
                    }
                } else {
                    collapsed.push(c);
                    last_space = false;
                }
            }
            collapsed.into()
        } else {
            text.into()
        };
        if text.is_empty() {
            return;
        }
        let style = if self.link.is_some() {
            style | Style::LINK
        } else {
            style
        };
        if style.has(Style::CODE) || style.has(Style::MATH) {
            self.append(&text, style, merge);
            return;
        }
        let mut rest = &*text;
        while let Some(emoji) = super::emoji::find(rest) {
            self.append_with_tags(&rest[..emoji.start], style, merge);
            self.append(&rest[emoji.clone()], style | Style::EMOJI, false);
            rest = &rest[emoji.end..];
        }
        self.append_with_tags(rest, style, merge);
    }

    /// [`Self::append`], each Obsidian tag in `text` (outside links) a
    /// `Style::TAG` span of its own.
    fn append_with_tags(&mut self, text: &str, style: Style, merge: bool) {
        if style.has(Style::LINK) || !text.contains('#') {
            self.append(text, style, merge);
            return;
        }
        let mut rest = text;
        let mut merge = merge;
        while let Some(tag) = find_tag(rest, self.cur.text.chars().next_back()) {
            self.append(&rest[..tag.start], style, merge);
            // A link of its own, which the app turns into a find.
            self.link = Some(format!("{TAG_LINK}{}", &rest[tag.clone()]));
            self.append(&rest[tag.clone()], style | Style::TAG, false);
            self.link = None;
            rest = &rest[tag.end..];
            merge = false;
        }
        self.append(rest, style, merge);
    }

    /// Appends `text` with its final style and the current link.
    fn append(&mut self, text: &str, style: Style, merge: bool) {
        if text.is_empty() {
            return;
        }
        let link = self.link.as_ref().map(|target| {
            match self.cur.links.iter().rposition(|l| l == target) {
                Some(index) if index + 1 == self.cur.links.len() => index as u32,
                _ => {
                    self.cur.links.push(target.clone());
                    (self.cur.links.len() - 1) as u32
                }
            }
        });
        let start = self.cur.text.len();
        self.cur.text.push_str(text);
        let end = self.cur.text.len();
        if let Some(last) = self.cur.spans.last_mut()
            && merge
            && last.style == style
            && last.link == link
            && last.range.end == start
        {
            last.range.end = end;
            return;
        }
        self.cur.spans.push(Span {
            range: start..end,
            style,
            link,
        });
    }

    fn flush(&mut self, builder: &mut Builder) {
        if self.cur.text.is_empty() {
            return;
        }
        let mut rich = std::mem::take(&mut self.cur);
        rich.id = builder.alloc_id();
        builder.add_search(rich.id, &rich.text);
        if let (Some(track), Some(tracker)) = (&mut self.track, &mut builder.track) {
            let source = track.take(builder.html_source.clone());
            tracker.rich.insert(rich.id, source);
        }
        self.parts.push(Inline::Text(rich));
    }

    fn push_image(&mut self, builder: &mut Builder, image: ImageRef) {
        self.flush(builder);
        builder.images.insert(image.src.clone());
        self.parts.push(Inline::Image(image));
    }

    fn finish(mut self, builder: &mut Builder) -> Vec<Inline> {
        // A hard break at the very end of a block adds nothing.
        while self.cur.text.ends_with('\n') {
            self.cur.text.pop();
            if let Some(last) = self.cur.spans.last_mut() {
                last.range.end = self.cur.text.len();
                if last.range.is_empty() {
                    self.cur.spans.pop();
                }
            }
        }
        self.flush(builder);
        self.parts
    }

    fn with_link<'a>(&mut self, builder: &mut Builder, node: Node<'a>, style: Style, url: String) {
        let saved = self.link.replace(url);
        self.children(builder, node, style);
        self.link = saved;
    }

    fn node<'a>(&mut self, builder: &mut Builder, node: Node<'a>, style: Style) {
        let source = match (&self.track, &builder.track) {
            (Some(_), Some(tracker)) => tracker.range(&node.data().sourcepos),
            _ => return self.build_node(builder, node, style),
        };
        let (leaf, group, mark, text) = match &node.data().value {
            NodeValue::Text(_) => (true, false, false, true),
            NodeValue::Code(_)
            | NodeValue::Math(_)
            | NodeValue::HtmlInline(_)
            | NodeValue::FootnoteReference(_)
            | NodeValue::EscapedTag(_)
            | NodeValue::Raw(_) => (true, false, false, false),
            NodeValue::Highlight => (false, true, true, false),
            NodeValue::Emph
            | NodeValue::Strong
            | NodeValue::Strikethrough
            | NodeValue::Insert
            | NodeValue::Underline
            | NodeValue::Superscript
            | NodeValue::Subscript
            | NodeValue::Link(_)
            | NodeValue::WikiLink(_) => (false, true, false, false),
            _ => (false, false, false, false),
        };
        if mark && let Some(tracker) = &mut builder.track {
            tracker.highlights += 1;
        }
        if group && let Some(track) = &mut self.track {
            track.open(source.clone(), mark);
        }
        let (text_before, parts_before) = (self.cur.text.len(), self.parts.len());
        self.build_node(builder, node, style);
        let Some(track) = &mut self.track else { return };
        if group {
            track.close();
        }
        // Text added to the same `Rich` (an image in HTML starts another).
        if leaf && self.parts.len() == parts_before && self.cur.text.len() > text_before {
            let added = text_before..self.cur.text.len();
            let exact = text
                && builder
                    .track
                    .as_ref()
                    .is_some_and(|t| t.is_as_written(&self.cur.text[added.clone()], &source));
            track.piece(added, source, exact);
        }
    }

    fn build_node<'a>(&mut self, builder: &mut Builder, node: Node<'a>, style: Style) {
        let data = node.data();
        match &data.value {
            NodeValue::Text(text) => self.push_text(text, style),
            NodeValue::SoftBreak | NodeValue::LineBreak => self.push_text("\n", style),
            NodeValue::Code(code) => self.push_text(&code.literal, style | Style::CODE),
            NodeValue::Math(math) => {
                let display = if math.display_math {
                    Style::DISPLAY
                } else {
                    Style::NONE
                };
                // Each formula is a span of its own, even right after another.
                self.push_span(&math.literal, style | Style::MATH | display, false);
            }
            NodeValue::HtmlInline(raw) => {
                let raw = raw.clone();
                drop(data);
                for token in html::tokenize(&raw) {
                    self.html_token(builder, &token);
                }
            }
            NodeValue::Emph => {
                drop(data);
                self.children(builder, node, style | Style::EMPH);
            }
            NodeValue::Strong => {
                drop(data);
                self.children(builder, node, style | Style::STRONG);
            }
            NodeValue::Strikethrough => {
                drop(data);
                self.children(builder, node, style | Style::STRIKE);
            }
            NodeValue::Highlight => {
                drop(data);
                self.children(builder, node, style | Style::MARK);
            }
            NodeValue::Insert | NodeValue::Underline => {
                drop(data);
                self.children(builder, node, style | Style::INSERT);
            }
            NodeValue::Superscript => {
                drop(data);
                self.children(builder, node, style | Style::SUP);
            }
            NodeValue::Subscript => {
                drop(data);
                self.children(builder, node, style | Style::SUB);
            }
            NodeValue::Link(link) => {
                let url = link.url.clone();
                drop(data);
                self.with_link(builder, node, style, url);
            }
            NodeValue::WikiLink(link) => {
                let url = link.url.clone();
                drop(data);
                self.with_link(builder, node, style, url);
            }
            NodeValue::Image(link) if link.title == preprocess::NOTE_EMBED_TITLE => {
                // A note embed among other text: a link to the note.
                let url = link.url.clone();
                drop(data);
                let saved = self.link.replace(url);
                self.push_text(&collect_text(node), style);
                self.link = saved;
            }
            NodeValue::Image(link) if is_other_file(&link.url) => {
                // Obsidian embeds a PDF, a sound, a video or any file with
                // `![[file.pdf]]`: a viewer of its own, here a link to it.
                let url = link.url.clone();
                drop(data);
                let alt = collect_text(node);
                let text = alt
                    .rsplit_once('|')
                    .map_or(alt.as_str(), |(text, _)| text.trim_end());
                let text = match text.trim() {
                    "" => preprocess::percent_decode(
                        url.split(['?', '#'])
                            .next()
                            .unwrap_or(&url)
                            .rsplit('/')
                            .next()
                            .unwrap_or(&url),
                    ),
                    text => text.to_owned(),
                };
                let saved = self.link.replace(self.link.clone().unwrap_or(url));
                self.push_text(&text, style);
                self.link = saved;
            }
            NodeValue::Image(link) => {
                let src = link.url.clone();
                let title = link.title.clone();
                drop(data);
                let mut alt = collect_text(node);
                let (width, height) = match title.strip_prefix(EMBED_SIZE_TITLE_PREFIX) {
                    Some(size) => parse_embed_size(size),
                    // Obsidian sizes an ordinary image in its alt text:
                    // `![alt|300x200](src)`, `![alt|300](src)`.
                    None => match alt.rsplit_once('|') {
                        Some((text, size)) if is_embed_size(size) => {
                            let size = parse_embed_size(size.trim());
                            alt.truncate(text.trim_end().len());
                            size
                        }
                        _ => (None, None),
                    },
                };
                let image = ImageRef {
                    src,
                    alt,
                    link: self.link.clone(),
                    width,
                    height,
                };
                self.push_image(builder, image);
            }
            NodeValue::FootnoteReference(reference) => {
                let label = format!("[{}]", reference.ix);
                let url = format!("#fn-{}", reference.name);
                let saved = self.link.replace(url);
                self.push_text(&label, style | Style::SUP);
                self.link = saved;
            }
            NodeValue::EscapedTag(tag) => self.push_text(tag, style),
            NodeValue::Raw(raw) => self.push_text(raw, style),
            _ => {
                drop(data);
                self.children(builder, node, style);
            }
        }
    }

    fn children<'a>(&mut self, builder: &mut Builder, node: Node<'a>, style: Style) {
        if self.depth >= MAX_INLINE_DEPTH {
            self.push_text(&collect_text(node), style);
            return;
        }
        self.depth += 1;
        for child in node.children() {
            self.node(builder, child, style);
        }
        self.depth -= 1;
    }

    fn html_open(&mut self, name: &str, style: Style) {
        self.html_styles.push((name.to_owned(), self.html_style));
        self.html_style |= style;
    }

    fn html_close(&mut self, name: &str) {
        if let Some(pos) = self.html_styles.iter().rposition(|(n, _)| n == name) {
            self.html_style = self.html_styles[pos].1;
            self.html_styles.truncate(pos);
        }
    }

    /// Applies one inline HTML token.
    fn html_token(&mut self, builder: &mut Builder, token: &Token<'_>) {
        match token {
            Token::Text(text) => {
                let decoded = html::decode_entities(text);
                self.push_text(&decoded, Style::NONE);
            }
            Token::Open {
                name,
                attrs,
                self_closing,
            } => match name.as_str() {
                "br" => {
                    // Not whitespace to collapse: in an HTML block it would
                    // become a space.
                    let collapse = std::mem::replace(&mut self.collapse_whitespace, false);
                    // Nor is a space before it, which would be drawn at the
                    // end of the line and shift a centred one.
                    if collapse && self.cur.text.ends_with(' ') {
                        self.cur.text.pop();
                        if let Some(last) = self.cur.spans.last_mut() {
                            last.range.end = last.range.end.min(self.cur.text.len());
                            if last.range.is_empty() {
                                self.cur.spans.pop();
                            }
                        }
                    }
                    self.push_text("\n", Style::NONE);
                    self.collapse_whitespace = collapse;
                }
                "img" => {
                    let Some(src) = html::attr(attrs, "src") else {
                        return;
                    };
                    let size = |key: &str| html::attr(attrs, key).and_then(|v| image_size(&v));
                    let image = ImageRef {
                        src: src.into_owned(),
                        alt: html::attr(attrs, "alt").unwrap_or_default().into_owned(),
                        link: self.link.clone(),
                        width: size("width"),
                        height: size("height"),
                    };
                    self.push_image(builder, image);
                }
                "a" => {
                    let is_block_id = html::attr(attrs, "class")
                        .is_some_and(|c| c.split_whitespace().any(|c| c == BLOCK_ID_ANCHOR_CLASS));
                    for key in ["id", "name"] {
                        if let Some(id) = html::attr(attrs, key) {
                            builder
                                .anchors
                                .entry(id.into_owned())
                                .or_insert(builder.top);
                        }
                    }
                    if *self_closing || is_block_id {
                        return;
                    }
                    match html::attr(attrs, "href") {
                        Some(href) => {
                            let previous = self.link.replace(href.into_owned());
                            self.html_links.push(true);
                            // Remember the outer link so `</a>` restores it.
                            self.html_styles
                                .push(("\u{0}a".to_owned(), self.html_style));
                            if let Some(previous) = previous {
                                self.html_styles
                                    .push((format!("\u{0}link:{previous}"), self.html_style));
                            }
                        }
                        None => self.html_links.push(false),
                    }
                }
                "b" | "strong" => self.html_open(name, Style::STRONG),
                "i" | "em" | "cite" | "var" | "dfn" => self.html_open(name, Style::EMPH),
                "s" | "del" | "strike" => self.html_open(name, Style::STRIKE),
                "code" | "tt" | "samp" => self.html_open(name, Style::CODE),
                "kbd" => self.html_open(name, Style::KBD | Style::CODE),
                "mark" => self.html_open(name, Style::MARK),
                "ins" | "u" => self.html_open(name, Style::INSERT),
                "sup" => self.html_open(name, Style::SUP),
                "sub" => self.html_open(name, Style::SUB),
                "hr" => self.push_text("\n", Style::NONE),
                _ => {}
            },
            Token::Close { name } => match name.as_str() {
                "a" => {
                    if self.html_links.pop() == Some(true) {
                        // Restore the link that was active before this `<a>`.
                        let mut restored = None;
                        while let Some((marker, _)) = self.html_styles.last() {
                            if let Some(previous) = marker.strip_prefix("\u{0}link:") {
                                restored = Some(previous.to_owned());
                                self.html_styles.pop();
                            } else if marker == "\u{0}a" {
                                self.html_styles.pop();
                                break;
                            } else {
                                break;
                            }
                        }
                        self.link = restored;
                    }
                }
                other => self.html_close(other),
            },
        }
    }
}

fn parse_embed_size(size: &str) -> (Option<f32>, Option<f32>) {
    let mut parts = size.split('x');
    let width = parts.next().and_then(image_size);
    let height = parts.next().and_then(image_size);
    (width, height)
}

/// An image's width or height as written (`300`, `300px`): a size only if
/// it is a number above zero and of sane size. `0` would divide by zero in
/// the layout, and `inf` parses as a number too.
fn image_size(text: &str) -> Option<f32> {
    let value: f32 = text.trim().trim_end_matches("px").parse().ok()?;
    (value.is_finite() && value > 0.0).then(|| value.min(100_000.0))
}

fn strip_front_matter_fences(raw: &str) -> String {
    let mut lines: Vec<&str> = raw.lines().collect();
    if lines.first().is_some_and(|l| l.trim() == "---") {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.last().is_some_and(|l| l.trim() == "---") {
        lines.pop();
    }
    lines.join("\n")
}

/// Whether an image's source names a local file that is no picture (a
/// PDF, a sound, a video, a spreadsheet): one with an extension that is
/// not of an image. A source without one may still be a picture, and so
/// may a web address with any (`avatar.php`).
fn is_other_file(src: &str) -> bool {
    const PICTURES: [&str; 14] = [
        "png", "jpg", "jpeg", "jfif", "gif", "webp", "svg", "bmp", "ico", "apng", "avif", "tif",
        "tiff", "heic",
    ];
    let src = src.trim();
    if src.contains("://") && !src.starts_with("file:") || src.starts_with("data:") {
        return false;
    }
    let path = src.split(['?', '#']).next().unwrap_or(src);
    preprocess::link_path_extension(path)
        .is_some_and(|ext| !PICTURES.iter().any(|p| ext.eq_ignore_ascii_case(p)))
}

/// The link a tag's span carries, followed by the tag (`#name`): a click
/// on it finds the tag in the document.
pub const TAG_LINK: &str = "metalmd-tag:";

/// The first Obsidian tag in `text`: `#` at the start or after white
/// space (`before` is the character before `text`), then letters, digits,
/// `_`, `-` and `/`, not digits only (`#123` is no tag), not ending in `/`.
fn find_tag(text: &str, before: Option<char>) -> Option<Range<usize>> {
    let mut from = 0;
    while let Some(found) = text[from..].find('#') {
        let at = from + found;
        from = at + 1;
        let previous = text[..at].chars().next_back().or(before);
        if previous.is_some_and(|c| !c.is_whitespace()) {
            continue;
        }
        let body = &text[at + 1..];
        let len = body
            .char_indices()
            .find(|&(_, c)| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '/')))
            .map_or(body.len(), |(i, _)| i);
        let name = body[..len].trim_end_matches('/');
        if !name.is_empty() && !name.chars().all(|c| c.is_ascii_digit()) {
            return Some(at..at + 1 + name.len());
        }
    }
    None
}

/// The pictures of a callout's blocks if they are paragraphs of nothing
/// else (white space and line breaks aside), else `None`.
fn gallery_images(blocks: &[Block]) -> Option<Vec<ImageRef>> {
    let mut images = Vec::new();
    for block in blocks {
        let Block::Paragraph { inlines, .. } = block else {
            return None;
        };
        for inline in inlines {
            match inline {
                Inline::Image(image) => images.push(image.clone()),
                Inline::Text(rich) if rich.text.trim().is_empty() => {}
                Inline::Text(_) => return None,
            }
        }
    }
    (!images.is_empty()).then_some(images)
}

/// The (title, src) of each note embed of a paragraph made of nothing
/// else (`![[A]]` lines one after another), or `None`.
fn lone_note_embeds<'a>(node: Node<'a>) -> Option<Vec<(String, String)>> {
    let mut found = Vec::new();
    for child in node.children() {
        match &child.data().value {
            NodeValue::Image(link) if link.title == preprocess::NOTE_EMBED_TITLE => {
                found.push((collect_text(child), link.url.clone()));
            }
            NodeValue::SoftBreak | NodeValue::LineBreak => {}
            NodeValue::Text(t) if t.trim().is_empty() => {}
            _ => return None,
        }
    }
    (!found.is_empty()).then_some(found)
}

/// The TeX of a paragraph that is nothing but one `$$ … $$` span.
fn lone_display_math<'a>(node: Node<'a>) -> Option<String> {
    let mut found = None;
    for child in node.children() {
        match &child.data().value {
            NodeValue::Math(math) if math.display_math && found.is_none() => {
                found = Some(math.literal.trim().to_owned());
            }
            NodeValue::SoftBreak | NodeValue::LineBreak => {}
            NodeValue::Text(t) if t.trim().is_empty() => {}
            _ => return None,
        }
    }
    found
}

/// The text a heading is anchorized from: the literals of its inline
/// children, with emphasis and link markup dropped. Matches Markpad.
/// An Obsidian callout that comrak's alerts do not take: a quote whose
/// first line is `[!type]`, `[!type]-` or `[!type]+`, then an optional
/// title. That line is taken out of the quote, and the callout comes back
/// as the GitHub alert nearest in colour, titled with its type unless it
/// has a title of its own, with its fold mark ([`fold_mark`]) and its type
/// in lower case.
fn take_callout<'a>(quote: Node<'a>) -> Option<(AlertKind, String, Option<bool>, String)> {
    let paragraph = quote.first_child()?;
    if !matches!(paragraph.data().value, NodeValue::Paragraph) {
        return None;
    }
    let mut line = Vec::new();
    let mut text = String::new();
    for inline in paragraph.children() {
        let end = matches!(
            inline.data().value,
            NodeValue::SoftBreak | NodeValue::LineBreak
        );
        line.push(inline);
        if end {
            break;
        }
        text.push_str(&collect_text(inline));
    }
    let rest = text.strip_prefix("[!")?;
    let (kind, rest) = rest.split_once(']')?;
    if kind.is_empty()
        || !kind
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let (fold, rest) = fold_mark(rest);
    // `[!info]text` is not a callout: the title is set off by a space.
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let title = match rest.trim() {
        "" => {
            let mut chars = kind.chars();
            let first = chars.next().map(|c| c.to_ascii_uppercase());
            first
                .into_iter()
                .chain(chars.map(|c| c.to_ascii_lowercase()))
                .collect()
        }
        title => title.to_owned(),
    };
    for inline in line {
        inline.detach();
    }
    Some((callout_kind(kind), title, fold, kind.to_ascii_lowercase()))
}

/// Obsidian's fold mark after a callout's type: `-` folded, `+` open but
/// foldable; `None` without one. And the rest of the line.
fn fold_mark(rest: &str) -> (Option<bool>, &str) {
    match rest.strip_prefix('-') {
        Some(rest) => (Some(false), rest),
        None => match rest.strip_prefix('+') {
            Some(rest) => (Some(true), rest),
            None => (None, rest),
        },
    }
}

/// The GitHub alert whose colour is nearest an Obsidian callout type's.
fn callout_kind(kind: &str) -> AlertKind {
    match kind.to_ascii_lowercase().as_str() {
        "abstract" | "summary" | "tldr" | "tip" | "hint" | "success" | "check" | "done" => {
            AlertKind::Tip
        }
        "important" | "example" => AlertKind::Important,
        "warning" | "attention" | "question" | "help" | "faq" => AlertKind::Warning,
        "caution" | "failure" | "fail" | "missing" | "danger" | "error" | "bug" => {
            AlertKind::Caution
        }
        // note, info, todo, quote, cite, and types of the reader's own.
        _ => AlertKind::Note,
    }
}

fn collect_text<'a>(node: Node<'a>) -> String {
    let mut text = String::new();
    for descendant in node.descendants() {
        match &descendant.data().value {
            NodeValue::Text(literal) => text.push_str(literal),
            NodeValue::Code(code) => text.push_str(&code.literal),
            NodeValue::Math(math) => text.push_str(&math.literal),
            // A heading over two lines (setext): comrak's ids join them
            // with a space.
            NodeValue::SoftBreak | NodeValue::LineBreak => text.push(' '),
            _ => {}
        }
    }
    text
}

/// The visible text of some inlines (images contribute nothing).
pub fn plain_text(inlines: &[Inline]) -> String {
    let mut text = String::new();
    for inline in inlines {
        if let Inline::Text(rich) = inline {
            text.push_str(&rich.text);
        }
    }
    text
}

fn inlines_are_blank(inlines: &[Inline]) -> bool {
    inlines.iter().all(|inline| match inline {
        Inline::Text(rich) => rich.text.trim().is_empty(),
        Inline::Image(_) => false,
    })
}

/// Trims whitespace at the start of the first and end of the last text.
fn trim_inlines(inlines: &mut Vec<Inline>) {
    if let Some(Inline::Text(rich)) = inlines.first_mut() {
        let cut = rich.text.len() - rich.text.trim_start().len();
        if cut > 0 {
            rich.text.drain(..cut);
            for span in &mut rich.spans {
                span.range.start = span.range.start.saturating_sub(cut);
                span.range.end = span.range.end.saturating_sub(cut);
            }
            rich.spans.retain(|s| !s.range.is_empty());
        }
    }
    if let Some(Inline::Text(rich)) = inlines.last_mut() {
        let keep = rich.text.trim_end().len();
        rich.text.truncate(keep);
        for span in &mut rich.spans {
            span.range.start = span.range.start.min(keep);
            span.range.end = span.range.end.min(keep);
        }
        rich.spans.retain(|s| !s.range.is_empty());
    }
    inlines.retain(|inline| !matches!(inline, Inline::Text(rich) if rich.text.is_empty()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_rich(block: &Block) -> &Rich {
        let inlines = match block {
            Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } => inlines,
            other => panic!("not a text block: {other:?}"),
        };
        match &inlines[0] {
            Inline::Text(rich) => rich,
            other => panic!("not text: {other:?}"),
        }
    }

    #[test]
    fn every_top_level_block_knows_its_line() {
        let doc = Document::parse(
            "---\ntitle: x\n---\n# Head\n\ntext [[Note]]\nmore\n\n<p>a</p>\n\n<p>b</p>\n\n- item\n",
        );
        assert_eq!(doc.lines.len(), doc.blocks.len());
        let line_of =
            |pred: &dyn Fn(&Block) -> bool| doc.lines[doc.blocks.iter().position(pred).unwrap()];
        assert_eq!(line_of(&|b| matches!(b, Block::FrontMatter { .. })), 1);
        assert_eq!(line_of(&|b| matches!(b, Block::Heading { .. })), 4);
        assert_eq!(line_of(&|b| matches!(b, Block::List { .. })), 13);
        assert!(
            doc.lines.windows(2).all(|w| w[0] <= w[1]),
            "{:?}",
            doc.lines
        );
    }

    #[test]
    fn heading_slugs_are_the_ids_comrak_renders() {
        let markdown = concat!(
            "# 11. Mermaid Diagrams\n\n",
            "Prose.\n\n",
            "## Objectives\n\n",
            "## Objectives\n\n",
            "Setext heading\n",
            "---\n\n",
            "Setext on\n",
            "two lines\n",
            "===\n\n",
            "## A **bold** word and `code`\n\n",
            "## 1. 概述\n\n",
            "```bash\n",
            "# not a heading, a shell comment\n",
            "```\n",
        );
        let doc = Document::parse(markdown);
        let texts: Vec<&str> = doc.headings.iter().map(|h| h.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "11. Mermaid Diagrams",
                "Objectives",
                "Objectives",
                "Setext heading",
                "Setext on two lines",
                "A bold word and code",
                "1. 概述",
            ]
        );
        let html = comrak::markdown_to_html(markdown, &preprocess::markdown_options());
        for heading in &doc.headings {
            assert!(
                html.contains(&format!("id=\"{}\"", heading.slug)),
                "id {:?} is not in the rendered document: {html}",
                heading.slug
            );
        }
        let objectives: Vec<&str> = doc
            .headings
            .iter()
            .filter(|h| h.text == "Objectives")
            .map(|h| h.slug.as_str())
            .collect();
        assert_eq!(objectives, vec!["objectives", "objectives-1"]);
        assert_eq!(doc.headings[0].level, 1);
        assert_eq!(doc.headings[1].level, 2);
        assert_eq!(doc.headings[4].slug, "setext-on-two-lines");
        // Each heading points at its own top-level block.
        for heading in &doc.headings {
            assert!(matches!(doc.blocks[heading.block], Block::Heading { .. }));
        }
    }

    #[test]
    fn inline_styles_become_spans() {
        let doc = Document::parse(
            "Plain **bold** *it* `code` ~~gone~~ ==hi== ++ins++ [link](https://x.y)\n",
        );
        let rich = first_rich(&doc.blocks[0]);
        let styled = |needle: &str| {
            let start = rich.text.find(needle).unwrap();
            rich.spans
                .iter()
                .find(|s| s.range.start <= start && start < s.range.end)
                .unwrap()
                .style
        };
        assert!(styled("bold").has(Style::STRONG));
        assert!(styled("it").has(Style::EMPH));
        assert!(styled("code").has(Style::CODE));
        assert!(styled("gone").has(Style::STRIKE));
        assert!(styled("hi").has(Style::MARK));
        assert!(styled("ins").has(Style::INSERT));
        assert!(styled("link").has(Style::LINK));
        let at = rich.text.find("link").unwrap();
        assert_eq!(
            rich.link_at(at).map(|l| rich.links[l as usize].as_str()),
            Some("https://x.y")
        );
        assert_eq!(rich.link_at(0), None);
    }

    #[test]
    fn single_newlines_are_line_breaks() {
        let doc = Document::parse("one\ntwo\n");
        assert_eq!(first_rich(&doc.blocks[0]).text, "one\ntwo");
    }

    #[test]
    fn wikilinks_and_block_ids_resolve() {
        let doc = Document::parse(
            "# Setup\n\nSee [[#Setup|setup]] and [[Notes#Intro]].\n\nTarget ^anchor1\n",
        );
        let rich = first_rich(&doc.blocks[1]);
        assert_eq!(
            rich.links,
            vec!["#setup".to_owned(), "Notes.md#intro".to_owned()]
        );
        assert_eq!(doc.anchors.get("setup"), Some(&0));
        assert_eq!(doc.anchors.get("anchor1"), Some(&2));
        assert_eq!(first_rich(&doc.blocks[2]).text.trim_end(), "Target");
    }

    #[test]
    fn embeds_become_sized_images() {
        let doc = Document::parse("![[diagram.png|300x200]]\n");
        let Block::Paragraph { inlines, .. } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks[0]);
        };
        let Inline::Image(image) = &inlines[0] else {
            panic!("{inlines:?}");
        };
        assert_eq!(image.src, "diagram.png");
        assert_eq!(image.alt, "diagram.png");
        assert_eq!((image.width, image.height), (Some(300.0), Some(200.0)));
    }

    #[test]
    fn sizes_in_alt_text_size_ordinary_images() {
        let image = |source: &str| {
            let doc = Document::parse(source);
            let Some(Block::Paragraph { inlines, .. }) = doc.blocks.first() else {
                panic!("{:?}", doc.blocks);
            };
            let Inline::Image(image) = &inlines[0] else {
                panic!("{inlines:?}");
            };
            image.clone()
        };
        let sized = image(
            "![Cover|160](../img/cover.jpg)
",
        );
        assert_eq!(sized.alt, "Cover");
        assert_eq!((sized.width, sized.height), (Some(160.0), None));
        let both = image(
            "![Still | 284x200](still.jpg)
",
        );
        assert_eq!(both.alt, "Still");
        assert_eq!((both.width, both.height), (Some(284.0), Some(200.0)));
        let bare = image(
            "![|120](a.png)
",
        );
        assert_eq!(bare.alt, "");
        assert_eq!(bare.width, Some(120.0));
        // A `|` followed by text is part of the alt.
        let text = image(
            "![a|b](a.png)
",
        );
        assert_eq!(text.alt, "a|b");
        assert_eq!((text.width, text.height), (None, None));
    }

    #[test]
    fn image_sizes_are_numbers_above_zero() {
        let sizes = |source: &str| {
            let doc = Document::parse(source);
            let Some(Block::Paragraph { inlines, .. }) = doc.blocks.first() else {
                panic!("{:?}", doc.blocks);
            };
            let Inline::Image(image) = &inlines[0] else {
                panic!("{inlines:?}");
            };
            (image.width, image.height)
        };
        assert_eq!(sizes("![[a.png|0x0]]\n"), (None, None));
        assert_eq!(
            sizes("<img src=\"a.png\" width=\"inf\" height=\"-5\">\n"),
            (None, None)
        );
        assert_eq!(
            sizes("<img src=\"a.png\" width=\"120px\">\n"),
            (Some(120.0), None)
        );
        assert_eq!(
            sizes("![[a.png|1e9]]\n"),
            (None, None),
            "not a size of digits"
        );
        assert_eq!(
            sizes("<img src=\"a.png\" width=\"1e9\">\n"),
            (Some(100_000.0), None)
        );
    }

    #[test]
    fn empty_embeds_stay_text() {
        for source in ["![[]]\n", "![[|300]]\n", "![[ ]]\n"] {
            let doc = Document::parse(source);
            assert!(
                matches!(&doc.blocks[0], Block::Paragraph { inlines, .. }
                    if matches!(&inlines[..], [Inline::Text(rich)] if rich.text == source.trim())),
                "{source}: {:?}",
                doc.blocks
            );
        }
    }

    #[test]
    fn badges_keep_their_links() {
        let doc =
            Document::parse("[![CI](https://img.shields.io/ci.svg)](https://ci.example) text\n");
        let Block::Paragraph { inlines, .. } = &doc.blocks[0] else {
            panic!();
        };
        let Inline::Image(image) = &inlines[0] else {
            panic!("{inlines:?}");
        };
        assert_eq!(image.link.as_deref(), Some("https://ci.example"));
        assert_eq!(image.alt, "CI");
    }

    #[test]
    fn html_blocks_become_aligned_paragraphs_with_images() {
        let doc = Document::parse(concat!(
            "<p align=\"center\">\n",
            "  <a href=\"https://example.com\"><img src=\"logo.png\" width=\"120\" alt=\"Logo\"></a>\n",
            "  <br>\n",
            "  <b>Fast</b> &amp; small\n",
            "</p>\n",
            "\n",
            "<!-- a comment -->\n",
            "\n",
            "<h1 align=\"center\">Title</h1>\n",
        ));
        let Block::Paragraph { inlines, align } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks);
        };
        assert_eq!(*align, Align::Center);
        let Inline::Image(image) = &inlines[0] else {
            panic!("{inlines:?}");
        };
        assert_eq!(image.src, "logo.png");
        assert_eq!(image.width, Some(120.0));
        assert_eq!(image.link.as_deref(), Some("https://example.com"));
        let text = plain_text(inlines);
        assert!(text.contains("Fast & small"), "{text:?}");
        let Inline::Text(rich) = inlines.last().unwrap() else {
            panic!()
        };
        let fast = rich.text.find("Fast").unwrap();
        assert!(
            rich.spans
                .iter()
                .any(|s| s.range.start <= fast && fast < s.range.end && s.style.has(Style::STRONG))
        );
        assert_eq!(
            rich.link_at(fast),
            None,
            "the <a> was closed before the text"
        );
        assert!(matches!(
            &doc.blocks[1],
            Block::Heading {
                level: 1,
                align: Align::Center,
                ..
            }
        ));
        assert_eq!(doc.blocks.len(), 2, "{:?}", doc.blocks);
    }

    #[test]
    fn an_unclosed_paragraph_does_not_align_what_follows() {
        let doc =
            Document::parse("<p align=\"center\">\n  <img src=\"logo.png\">\n\n# Title\n\nText\n");
        let aligns: Vec<Align> = doc
            .blocks
            .iter()
            .map(|b| match b {
                Block::Paragraph { align, .. } | Block::Heading { align, .. } => *align,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(aligns, [Align::Center, Align::Left, Align::Left]);
    }

    #[test]
    fn pre_becomes_a_code_block_kept_as_written() {
        let doc = Document::parse(
            "<pre lang=\"rust\">\nfn main() {\n    let x = 1 &lt; 2;\n\n    <b>y</b>();\n}\n</pre>\n\nAfter\n",
        );
        let Block::Code { lang, code, .. } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks[0]);
        };
        assert_eq!(lang, "rust");
        assert_eq!(code, "fn main() {\n    let x = 1 < 2;\n\n    y();\n}");
        assert!(matches!(&doc.blocks[1], Block::Paragraph { .. }));

        let doc = Document::parse("<pre><code class=\"language-py\">a  =  1\nb = 2</code></pre>\n");
        let Block::Code { lang, code, .. } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks[0]);
        };
        assert_eq!((lang.as_str(), code.as_str()), ("py", "a  =  1\nb = 2"));
    }

    #[test]
    fn br_breaks_the_line_in_html_blocks() {
        for source in [
            "<p align=\"center\">line one<br>line two</p>\n",
            "<p align=\"center\">line one<br/>\n  line two</p>\n",
            "<div>\nline one <br> line two\n</div>\n",
        ] {
            let doc = Document::parse(source);
            assert_eq!(
                first_rich(&doc.blocks[0]).text,
                "line one\nline two",
                "{source}"
            );
        }
    }

    #[test]
    fn html_badges_on_separate_lines_get_a_space_between() {
        let doc = Document::parse(concat!(
            "<p align=\"center\">\n",
            "  <a href=\"x\"><img src=\"a.svg\" alt=\"A\"></a>\n",
            "  <img src=\"b.svg\" alt=\"B\"><img src=\"c.svg\" alt=\"C\">\n",
            "</p>\n",
        ));
        let Block::Paragraph { inlines, .. } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks);
        };
        let shape: Vec<&str> = inlines
            .iter()
            .map(|inline| match inline {
                Inline::Image(_) => "img",
                Inline::Text(rich) => rich.text.as_str(),
            })
            .collect();
        assert_eq!(shape, ["img", " ", "img", "img"]);
    }

    #[test]
    fn html_alignment_spans_markdown_between_blocks() {
        let doc =
            Document::parse("<div align=\"center\">\n\n![logo](logo.png)\n\n</div>\n\nAfter\n");
        assert!(
            matches!(
                &doc.blocks[0],
                Block::Paragraph {
                    align: Align::Center,
                    ..
                }
            ),
            "{:?}",
            doc.blocks
        );
        assert!(
            matches!(
                &doc.blocks[1],
                Block::Paragraph {
                    align: Align::Left,
                    ..
                }
            ),
            "{:?}",
            doc.blocks
        );
    }

    #[test]
    fn task_lists_tables_alerts_footnotes_and_front_matter() {
        let doc = Document::parse(concat!(
            "---\ntitle: Test\n---\n\n",
            "- [x] done\n- [ ] todo\n\n",
            "| a | b |\n|:-:|--:|\n| 1 | 2 |\n\n",
            "> [!WARNING]\n> Careful\n\n",
            "Text with a note.[^n] And ^[inline].\n\n",
            "[^n]: The note.\n",
        ));
        assert!(matches!(&doc.blocks[0], Block::FrontMatter { yaml, .. } if yaml == "title: Test"));
        let Block::List { items, .. } = &doc.blocks[1] else {
            panic!("{:?}", doc.blocks[1])
        };
        assert_eq!(items[0].task.map(|t| t.checked), Some(true));
        assert_eq!(items[1].task.map(|t| t.checked), Some(false));
        let Block::Table(table) = &doc.blocks[2] else {
            panic!()
        };
        assert_eq!(table.aligns, vec![Align::Center, Align::Right]);
        assert_eq!(table.rows.len(), 1);
        assert!(
            matches!(&doc.blocks[3], Block::Alert { kind: AlertKind::Warning, title, .. } if title == "Warning")
        );
        let rich = first_rich(&doc.blocks[4]);
        assert!(rich.text.contains("[1]"), "{:?}", rich.text);
        assert!(rich.links.iter().any(|l| l == "#fn-n"));
        let footnotes: Vec<&str> = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Footnote { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(footnotes, vec!["1", "2"]);
        assert!(doc.anchors.contains_key("fn-n"));
    }

    fn html_table(doc: &Document, at: usize) -> (&Table, Vec<Vec<String>>) {
        let Block::Table(table) = &doc.blocks[at] else {
            panic!("{:?}", doc.blocks);
        };
        let texts = |row: &Vec<Vec<Inline>>| row.iter().map(|cell| plain_text(cell)).collect();
        let rows = std::iter::once(&table.header)
            .filter(|header| !header.is_empty())
            .chain(&table.rows)
            .map(texts)
            .collect();
        (table, rows)
    }

    #[test]
    fn html_tables_are_tables() {
        let doc = Document::parse(concat!(
            "<table>\n",
            "  <tr><th>Name</th><th align=\"right\">Size</th></tr>\n",
            "  <tr><td>a <b>bold</b></td><td style=\"text-align: center\">1<br>2</td></tr>\n",
            "  <tr><td>short</td></tr>\n",
            "</table>\n",
        ));
        assert_eq!(doc.blocks.len(), 1, "{:?}", doc.blocks);
        let (table, rows) = html_table(&doc, 0);
        assert_eq!(
            rows,
            [["Name", "Size"], ["a bold", "1\n2"], ["short", ""]].map(|r| r.map(str::to_owned))
        );
        assert_eq!(table.aligns, [Align::Left, Align::Right]);
        let Inline::Text(name) = &table.header[0][0] else {
            panic!("{:?}", table.header);
        };
        assert!(name.spans.iter().all(|span| span.style.has(Style::STRONG)));
        assert!(doc.search.iter().any(|s| s.text == "short" && s.block == 0));
    }

    #[test]
    fn html_tables_hold_markdown_and_rows_set_apart() {
        let doc = Document::parse(concat!(
            "Before\n\n",
            "<div align=\"center\">\n",
            "<table>\n",
            "<tr>\n",
            "<td>\n\n",
            "### Fast\n\n",
            "**Bold** text\n\n",
            "</td>\n",
            "<td>\n\n",
            "![logo](logo.png)\n\n",
            "Caption\n\n",
            "</td>\n",
            "</tr>\n\n",
            "<tr><td>x</td><td>y</td></tr>\n",
            "</table>\n",
            "</div>\n\n",
            "After\n",
        ));
        assert_eq!(doc.blocks.len(), 3, "{:?}", doc.blocks);
        let (table, rows) = html_table(&doc, 1);
        assert_eq!(
            rows,
            [["Fast\nBold text", "\nCaption"], ["x", "y"]].map(|r| r.map(str::to_owned))
        );
        assert!(table.header.is_empty());
        assert_eq!(table.aligns, [Align::Center, Align::Center]);
        assert!(matches!(table.rows[0][1][0], Inline::Image(_)));
        // The line of the HTML block it starts in, `<div …>`.
        assert_eq!(doc.lines[1], 3, "the table's line is where it starts");
        assert_eq!(plain_text_of(&doc.blocks[2..]), "After");
    }

    #[test]
    fn html_tables_end_at_what_a_cell_cannot_hold() {
        // A list in a cell: the table so far ends before it, and the tags
        // after it are what they were without tables.
        let doc = Document::parse("<table><tr><td>\n\n- item\n\n</td></tr></table>\n");
        assert!(
            matches!(doc.blocks[0], Block::List { .. }),
            "{:?}",
            doc.blocks
        );
        assert_eq!(doc.blocks.len(), 1, "{:?}", doc.blocks);
        // Unclosed: it ends with the document, or with the item it is in.
        let doc = Document::parse("<table><tr><td>a</td>\n");
        assert_eq!(html_table(&doc, 0).1, [["a"]]);
        let doc = Document::parse("- item\n\n  <table><tr><td>a</td>\n\n- next\n");
        let Block::List { items, .. } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks);
        };
        assert!(
            matches!(items[0].blocks[1], Block::Table(_)),
            "{:?}",
            items[0]
        );
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn html_tables_inside_cells_and_captions_are_text() {
        let doc = Document::parse(concat!(
            "<table><caption>Results</caption>\n",
            "<tr><td><table><tr><td>in</td><td>ner</td></tr></table></td><td>b</td></tr>\n",
            "</table>\n",
        ));
        assert!(
            matches!(&doc.blocks[0], Block::Paragraph { align: Align::Center, inlines } if plain_text(inlines) == "Results"),
            "{:?}",
            doc.blocks
        );
        assert_eq!(html_table(&doc, 1).1, [["in ner", "b"]]);
    }

    #[test]
    fn obsidian_callouts_are_alerts() {
        let alert = |source: &str| {
            let doc = Document::parse(source);
            match &doc.blocks[0] {
                Block::Alert {
                    kind,
                    title,
                    blocks,
                    ..
                } => (*kind, title.clone(), plain_text_of(blocks)),
                other => panic!("{other:?}"),
            }
        };
        let fold = |source: &str| {
            let doc = Document::parse(source);
            doc.blocks
                .iter()
                .map(|b| match b {
                    Block::Alert { fold, .. } => *fold,
                    other => panic!("{other:?}"),
                })
                .collect::<Vec<_>>()
        };
        // Folded, open, not foldable; numbered with the `<details>`.
        assert_eq!(
            fold("> [!info]- A\n> a\n\n> [!NOTE]+\n> b\n\n> [!tip]\n> c\n\n> [!bug]-\n> d\n"),
            [
                Some(Fold {
                    index: 0,
                    open: false
                }),
                Some(Fold {
                    index: 1,
                    open: true
                }),
                None,
                Some(Fold {
                    index: 2,
                    open: false
                }),
            ]
        );
        assert_eq!(
            alert("> [!info] Custom\n> body\n"),
            (AlertKind::Note, "Custom".to_owned(), "body".to_owned())
        );
        assert_eq!(
            alert("> [!BUG]\n> body\n"),
            (AlertKind::Caution, "Bug".to_owned(), "body".to_owned())
        );
        assert_eq!(
            alert("> [!faq]- Why *so*?\n> body\n"),
            (AlertKind::Warning, "Why so?".to_owned(), "body".to_owned())
        );
        assert_eq!(
            alert(">[!example]+\n").0,
            AlertKind::Important,
            "a callout without a body or the space after `>`"
        );
        assert_eq!(
            alert("> [!NOTE]- folded\n> body\n"),
            (AlertKind::Note, "folded".to_owned(), "body".to_owned())
        );
        assert_eq!(alert("> [!TIP]-\n> body\n").1, "Tip");
        for quote in [
            "> [!info]text\n",
            "> [!]\n",
            "> [!a b]\n",
            "> text\n> [!info]\n",
        ] {
            let doc = Document::parse(quote);
            assert!(matches!(doc.blocks[0], Block::Quote(_)), "{quote:?}");
        }
    }

    fn plain_text_of(blocks: &[Block]) -> String {
        blocks
            .iter()
            .map(|block| match block {
                Block::Paragraph { inlines, .. } => plain_text(inlines),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn github_math_syntax_is_math() {
        let doc = Document::parse("Inline $`a^2`$ math.\n\n```math\nE = mc^2\n```\n");
        let rich = first_rich(&doc.blocks[0]);
        let span = rich
            .spans
            .iter()
            .find(|span| span.style.has(Style::MATH))
            .expect("a formula span");
        assert_eq!(&rich.text[span.range.clone()], "a^2");
        assert!(
            matches!(&doc.blocks[1], Block::Math { tex, .. } if tex == "E = mc^2"),
            "{:?}",
            doc.blocks[1]
        );
    }

    #[test]
    fn math_is_kept_as_text() {
        let doc = Document::parse("Inline $a_i + b_i$ math.\n\n$$\nE = mc^2\n$$\n");
        let rich = first_rich(&doc.blocks[0]);
        assert!(rich.text.contains("a_i + b_i"), "{:?}", rich.text);
        assert!(
            matches!(&doc.blocks[1], Block::Math { tex, .. } if tex == "E = mc^2"),
            "{:?}",
            doc.blocks[1]
        );
    }

    #[test]
    fn each_formula_is_a_span_of_its_own() {
        let doc = Document::parse("$a$$b$ and $$c$$ end\n");
        let rich = first_rich(&doc.blocks[0]);
        let maths: Vec<(&str, bool)> = rich
            .spans
            .iter()
            .filter(|s| s.style.has(Style::MATH))
            .map(|s| (&rich.text[s.range.clone()], s.style.has(Style::DISPLAY)))
            .collect();
        assert_eq!(maths, vec![("a", false), ("b", false), ("c", true)]);
    }

    #[test]
    fn gallery_callouts_of_pictures_are_galleries() {
        let doc = Document::parse(
            "> [!gallery]\n> ![a](a.png)\n> ![b](b.png)\n>\n> ![c](c.png)\n\n> [!gallery-masonry]\n> ![d](d.png)\n\n> [!gallery]\n> text ![e](e.png)\n",
        );
        let galleries: Vec<(bool, usize)> = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Gallery { masonry, images } => Some((*masonry, images.len())),
                _ => None,
            })
            .collect();
        assert_eq!(galleries, [(false, 3), (true, 1)]);
        // With text in it, a callout as any other.
        assert!(matches!(doc.blocks[2], Block::Alert { .. }));
    }

    #[test]
    fn embedded_files_that_are_no_pictures_are_links() {
        let doc = Document::parse(
            "![[report.pdf]] ![PC](!assets/PC/PC.xlsx) ![](a%20b/song.mp3) ![x](pic.png) ![w](https://e.com/a.php)\n",
        );
        let Block::Paragraph { inlines, .. } = &doc.blocks[0] else {
            panic!()
        };
        let Inline::Text(rich) = &inlines[0] else {
            panic!()
        };
        assert_eq!(rich.text, "report.pdf PC song.mp3 ");
        assert_eq!(
            rich.links,
            ["report.pdf", "!assets/PC/PC.xlsx", "a%20b/song.mp3"]
        );
        let images: Vec<&str> = inlines
            .iter()
            .filter_map(|i| match i {
                Inline::Image(image) => Some(image.src.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(images, ["pic.png", "https://e.com/a.php"]);
    }

    #[test]
    fn task_statuses_keep_their_mark() {
        let doc =
            Document::parse("- [ ] a\n- [x] b\n- [X] c\n- [/] d\n- [-] e\n- [>] f\n- [?] g\n");
        let Block::List { items, .. } = &doc.blocks[0] else {
            panic!()
        };
        let marks: Vec<(char, bool)> = items
            .iter()
            .map(|i| i.task.map(|t| (t.mark, t.checked)).unwrap())
            .collect();
        assert_eq!(
            marks,
            [
                (' ', false),
                ('x', true),
                ('X', true),
                ('/', false),
                ('-', false),
                ('>', false),
                ('?', false)
            ]
        );
    }

    #[test]
    fn tags_are_found_as_obsidian_finds_them() {
        let tags = |text: &str| {
            let mut out = Vec::new();
            let mut rest = text;
            let mut before = None;
            while let Some(range) = find_tag(rest, before) {
                out.push(rest[range.clone()].to_owned());
                before = rest[..range.end].chars().next_back();
                rest = &rest[range.end..];
            }
            out
        };
        assert_eq!(
            tags("#rust and #nested/tag, #тег_1."),
            ["#rust", "#nested/tag", "#тег_1"]
        );
        assert_eq!(tags("#123 a#b (#c) #/ #a/ #x-y"), ["#a", "#x-y"]);
        assert!(tags("url.com/#frag").is_empty());

        let doc = Document::parse("See #todo, `#code` and [#link](x) #done\n");
        let Block::Paragraph { inlines, .. } = &doc.blocks[0] else {
            panic!()
        };
        let Inline::Text(rich) = &inlines[0] else {
            panic!()
        };
        let tagged: Vec<&str> = rich
            .spans
            .iter()
            .filter(|s| s.style.has(Style::TAG))
            .map(|s| &rich.text[s.range.clone()])
            .collect();
        assert_eq!(tagged, ["#todo", "#done"]);
        let link = rich
            .spans
            .iter()
            .find(|s| s.style.has(Style::TAG))
            .unwrap()
            .link;
        assert_eq!(
            rich.links[link.unwrap() as usize],
            format!("{TAG_LINK}#todo")
        );
    }

    #[test]
    fn each_emoji_is_a_span_of_its_own_outside_code() {
        let doc = Document::parse("**ok ✅✅** [go 🚀](x) `❌` 👩‍💻\n");
        let rich = first_rich(&doc.blocks[0]);
        let emoji: Vec<(&str, Style)> = rich
            .spans
            .iter()
            .filter(|s| s.style.has(Style::EMOJI))
            .map(|s| (&rich.text[s.range.clone()], s.style))
            .collect();
        assert_eq!(
            emoji,
            vec![
                ("✅", Style::STRONG | Style::EMOJI),
                ("✅", Style::STRONG | Style::EMOJI),
                ("🚀", Style::LINK | Style::EMOJI),
                ("👩‍💻", Style::EMOJI),
            ]
        );
        let rocket = rich.text.find('🚀').unwrap();
        assert_eq!(rich.link_at(rocket), Some(0));
    }

    #[test]
    fn code_blocks_are_searchable_and_flag_has_code() {
        let doc = Document::parse("```rust\nfn main() {}\n```\n");
        assert!(doc.has_code);
        assert!(
            matches!(&doc.blocks[0], Block::Code { lang, code, .. } if lang == "rust" && code == "fn main() {}")
        );
        assert_eq!(doc.search.len(), 1);
        assert_eq!(doc.search[0].block, 0);
    }

    /// Levels of quotes and lists, the deepest path.
    fn nesting(blocks: &[Block]) -> usize {
        blocks
            .iter()
            .map(|block| match block {
                Block::Quote(inner) => 1 + nesting(inner),
                Block::List { items, .. } => {
                    1 + items.iter().map(|i| nesting(&i.blocks)).max().unwrap_or(0)
                }
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn deep_nesting_is_cut_off_and_keeps_the_text() {
        let quotes = Document::parse(&format!(
            "{} deep
",
            ">".repeat(3000)
        ));
        assert_eq!(nesting(&quotes.blocks), MAX_BLOCK_DEPTH);
        assert!(quotes.search.iter().any(|s| s.text == "deep"));

        let list: String = (0..300)
            .map(|i| {
                format!(
                    "{}- item {i}
",
                    "  ".repeat(i)
                )
            })
            .collect();
        let list = Document::parse(&list);
        assert_eq!(nesting(&list.blocks), MAX_BLOCK_DEPTH);
        for i in [0, 31, 32, 299] {
            let text = format!("item {i}");
            assert!(list.search.iter().any(|s| s.text == text), "{text}");
        }

        let emphasis = format!("{}a{}", "*".repeat(20000), "*".repeat(20000));
        let emphasis = Document::parse(&emphasis);
        assert!(emphasis.search.iter().any(|s| s.text.contains('a')));
    }

    #[test]
    fn copy_markers_in_the_text_are_left_out() {
        let doc = Document::parse("x\u{2063}\u{2064}\u{a0}y \u{2062}z\n\n```\na\u{2064}b\n```\n");
        assert_eq!(first_rich(&doc.blocks[0]).text, "x\u{a0}y z");
        assert!(matches!(&doc.blocks[1], Block::Code { code, .. } if code == "ab"));
    }

    #[test]
    fn scripts_are_detected() {
        assert_eq!(Document::parse("plain").scripts, Scripts::default());
        assert!(Document::parse("中文").scripts.han_or_kana);
        assert!(Document::parse("한국어").scripts.hangul);
        assert_eq!(Document::parse("Привет").scripts, Scripts::default());
    }

    #[test]
    fn the_markpad_sample_parses() {
        let sample = include_str!("../../samples/markdown-syntax.md");
        let doc = Document::parse(sample);
        assert!(doc.blocks.len() > 50);
        assert!(doc.headings.len() > 10);
        assert!(doc.has_code);
    }

    #[test]
    fn details_become_a_summary_row_and_an_end_around_their_blocks() {
        let doc = Document::parse(concat!(
            "<details>\n<summary>More <b>info</b></summary>\n\n",
            "Hidden *text*\n\n</details>\n\n",
            "<details open><summary>Open</summary>Inline</details>\n\n",
            "<details>\n\nNo summary\n\n</details>\n",
        ));
        let kinds: Vec<String> = doc
            .blocks
            .iter()
            .map(|block| match block {
                Block::Details {
                    index,
                    open,
                    summary,
                } => {
                    let text = match summary.first() {
                        Some(Inline::Text(rich)) => rich.text.as_str(),
                        _ => "",
                    };
                    format!("details {index} {open} {text}")
                }
                Block::DetailsEnd => "end".to_owned(),
                other => first_rich(other).text.clone(),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "details 0 false More info",
                "Hidden text",
                "end",
                "details 1 true Open",
                "Inline",
                "end",
                "details 2 false ",
                "No summary",
                "end",
            ]
        );
    }
}
