//! The render model: a Markdown document parsed once into blocks of styled
//! text, ready to be laid out by the viewer every frame without reparsing.
//!
//! Nothing here depends on egui, so documents are built on a background
//! thread and the module is testable without a window.

use super::html::{self, Token};
use super::preprocess::{self, BLOCK_ID_ANCHOR_CLASS, EMBED_SIZE_TITLE_PREFIX};
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
    /// `Some(checked)` for a task list item.
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
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
    },
    Rule,
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

#[derive(Clone, Debug, Default)]
pub struct Document {
    pub blocks: Vec<Block>,
    pub headings: Vec<HeadingEntry>,
    /// Anchor id → top-level block index (headings, footnotes, block ids).
    pub anchors: HashMap<String, usize>,
    pub search: Vec<SearchText>,
    /// The `src` of every image, once each: what to forget when the
    /// document is closed.
    pub images: Vec<String>,
    pub has_code: bool,
    pub scripts: Scripts,
}

impl Document {
    /// Parses `source` (already decoded) into the render model.
    pub fn parse(source: &str) -> Document {
        let preprocessed = preprocess::preprocess(source);
        let arena = Arena::new();
        let options = preprocess::markdown_options();
        let root = parse_document(&arena, &preprocessed, &options);

        let mut builder = Builder::default();
        // Footnote numbers are assigned in reference order; definitions come
        // last in comrak's tree, so collect the numbers first.
        for node in root.descendants() {
            if let NodeValue::FootnoteReference(r) = &node.data().value {
                builder.footnote_ix.entry(r.name.clone()).or_insert(r.ix);
            }
        }
        let mut blocks = Vec::new();
        for child in root.children() {
            // One comrak block can become zero blocks (a comment) or several
            // (an HTML block with two paragraphs), so the index is the length
            // of the output, not the child's position.
            builder.top = blocks.len();
            builder.block(child, &mut blocks);
        }
        Document {
            blocks,
            headings: builder.headings,
            anchors: builder.anchors,
            search: builder.search,
            images: builder.images.into_iter().collect(),
            has_code: builder.has_code,
            scripts: detect_scripts(source),
        }
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
}

impl Builder {
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
        self.depth -= 1;
        out
    }

    /// Builds `node` into `out`. At the top level `self.top` must equal the
    /// index the first pushed block will get.
    fn block<'a>(&mut self, node: Node<'a>, out: &mut Vec<Block>) {
        let data = node.data();
        match &data.value {
            NodeValue::FrontMatter(raw) => {
                let yaml = strip_front_matter_fences(raw);
                let id = self.alloc_id();
                self.add_search(id, &yaml);
                out.push(Block::FrontMatter { id, yaml });
            }
            NodeValue::Paragraph => {
                if let Some(tex) = lone_display_math(node) {
                    let id = self.alloc_id();
                    self.add_search(id, &tex);
                    out.push(Block::Math { id, tex });
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
                        text: display.trim().to_owned(),
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
                let mut code = code_block.literal.clone();
                if code.ends_with('\n') {
                    code.pop();
                    if code.ends_with('\r') {
                        code.pop();
                    }
                }
                let id = self.alloc_id();
                self.add_search(id, &code);
                self.has_code = true;
                let key = super::highlight::code_key(&lang, &code);
                out.push(Block::Code {
                    id,
                    key,
                    lang,
                    code,
                });
            }
            NodeValue::HtmlBlock(html_block) => {
                let literal = html_block.literal.clone();
                drop(data);
                self.html_block(&literal, out);
            }
            NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => {
                drop(data);
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
                let title = alert
                    .title
                    .clone()
                    .unwrap_or_else(|| alert.alert_type.default_title().to_owned());
                drop(data);
                let blocks = self.children_blocks(node);
                out.push(Block::Alert {
                    kind,
                    title,
                    blocks,
                });
            }
            NodeValue::List(list) => {
                let list = *list;
                drop(data);
                let mut items = Vec::new();
                for item in node.children() {
                    let task = match &item.data().value {
                        NodeValue::TaskItem(task) => Some(task.symbol.is_some()),
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
        for child in node.children() {
            ctx.node(self, child, Style::NONE);
        }
        ctx.finish(self)
    }

    /// An HTML block: paragraphs, headings and images; unknown tags dropped.
    fn html_block(&mut self, literal: &str, out: &mut Vec<Block>) {
        let mut ctx = InlineCtx::new(self.base_style);
        ctx.collapse_whitespace = true;
        let mut heading: Option<u8> = None;
        for token in html::tokenize(literal) {
            match &token {
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
                    if name == "summary" || name == "dt" || name == "th" {
                        ctx.html_open(name, Style::STRONG);
                    }
                }
                Token::Close { name } if is_html_block_tag(name) => {
                    if name == "summary" || name == "dt" || name == "th" {
                        ctx.html_close(name);
                    }
                    self.flush_html_paragraph(&mut ctx, heading.take(), out);
                    if let Some(pos) = self.html_align.iter().rposition(|(n, _)| n == name) {
                        self.html_align.truncate(pos);
                    }
                }
                _ => ctx.html_token(self, &token),
            }
        }
        self.flush_html_paragraph(&mut ctx, heading, out);
    }

    fn flush_html_paragraph(
        &mut self,
        ctx: &mut InlineCtx,
        heading: Option<u8>,
        out: &mut Vec<Block>,
    ) {
        let fresh = InlineCtx {
            collapse_whitespace: true,
            html_style: ctx.html_style,
            html_styles: std::mem::take(&mut ctx.html_styles),
            html_links: std::mem::take(&mut ctx.html_links),
            link: ctx.link.clone(),
            ..InlineCtx::new(ctx.base)
        };
        let finished = std::mem::replace(ctx, fresh);
        let mut inlines = finished.finish(self);
        trim_inlines(&mut inlines);
        if inlines_are_blank(&inlines) {
            return;
        }
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
            self.append(&rest[..emoji.start], style, merge);
            self.append(&rest[emoji.clone()], style | Style::EMOJI, false);
            rest = &rest[emoji.end..];
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
            NodeValue::Image(link) => {
                let src = link.url.clone();
                let title = link.title.clone();
                drop(data);
                let alt = collect_text(node);
                let (width, height) = title
                    .strip_prefix(EMBED_SIZE_TITLE_PREFIX)
                    .map(parse_embed_size)
                    .unwrap_or((None, None));
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
                    let size = |key: &str| {
                        html::attr(attrs, key)
                            .and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
                            .filter(|v| *v > 0.0)
                    };
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
    let width = parts.next().and_then(|w| w.parse().ok());
    let height = parts.next().and_then(|h| h.parse().ok());
    (width, height)
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
fn collect_text<'a>(node: Node<'a>) -> String {
    let mut text = String::new();
    for descendant in node.descendants() {
        match &descendant.data().value {
            NodeValue::Text(literal) => text.push_str(literal),
            NodeValue::Code(code) => text.push_str(&code.literal),
            NodeValue::Math(math) => text.push_str(&math.literal),
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
    fn heading_slugs_are_the_ids_comrak_renders() {
        let markdown = concat!(
            "# 11. Mermaid Diagrams\n\n",
            "Prose.\n\n",
            "## Objectives\n\n",
            "## Objectives\n\n",
            "Setext heading\n",
            "---\n\n",
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
        assert_eq!(items[0].task, Some(true));
        assert_eq!(items[1].task, Some(false));
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
}
