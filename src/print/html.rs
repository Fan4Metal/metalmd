//! A document as a web page to print: the render model written out as HTML
//! with a GitHub-like print style sheet.
//!
//! The page is made from `Document`, not from comrak's HTML, so it shows
//! what the viewer shows (raw HTML reduced to the README subset, wikilinks,
//! embed sizes, `<details>` as the reader left them) and nothing from the
//! file reaches it unescaped. Formulas and diagrams are SVG, so they print
//! as sharp as the text; code is coloured by the light theme.

use crate::markdown::document::{
    AlertKind, Align, Block, Document, ImageRef, Inline, ListItem, Rich, Style,
};
use crate::markdown::highlight::{self, HlSpan};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};

/// What the page needs besides the document.
#[derive(Clone, Debug)]
pub struct PageOptions {
    pub title: String,
    /// The document's folder, which relative links and images start from.
    pub base_dir: Option<PathBuf>,
    /// The reader's fonts, or `None` for the platform's.
    pub body_font: Option<String>,
    pub mono_font: Option<String>,
    /// The body text size in CSS pixels.
    pub text_px: f32,
    /// The width of the viewer's text column in CSS pixels: the page is
    /// zoomed so that a line on paper holds what it holds in the window
    /// (`zoom`).
    pub column_px: f32,
    /// The highlight colour of the light theme.
    pub mark_bg: [u8; 3],
    /// `<details>` the reader opened or closed (`ViewState::toggled_details`).
    pub toggled_details: HashSet<u32>,
    /// The page opens the browser's print dialog once loaded, for a page
    /// opened in the browser when WebView2 is missing.
    pub print_on_load: bool,
}

/// The light palette of `style::Palette`, as CSS colours.
const TEXT: &str = "#1f2328";
const MUTED: &str = "#59636e";
const LINK: &str = "#0969da";
const BORDER: &str = "#d1d9e0";
const CODE_BG: &str = "#eceef1";
const CODE_BLOCK_BG: &str = "#f6f8fa";

/// The whole page for `doc`.
pub fn page(doc: &Document, options: &PageOptions) -> String {
    let codes = code_blocks(&doc.blocks);
    let colors = highlight::highlight_now(&codes);
    let mut writer = Writer {
        doc,
        options,
        colors: codes.iter().map(|&(_, code)| code).zip(colors).collect(),
        targets: doc.anchors.values().copied().collect(),
        diagrams: 0,
        open_details: 0,
        footnotes_started: false,
        out: String::with_capacity(4096),
    };
    writer.head();
    for (index, block) in doc.blocks.iter().enumerate() {
        writer.top_level(index, block);
    }
    writer.close_details_to(0);
    writer.out.push_str("</body>\n</html>\n");
    writer.out
}

/// The (language, code) of every code block but diagrams, in any depth.
fn code_blocks(blocks: &[Block]) -> Vec<(&str, &str)> {
    fn walk<'d>(blocks: &'d [Block], out: &mut Vec<(&'d str, &'d str)>) {
        for block in blocks {
            match block {
                Block::Code { lang, code, .. } if !is_diagram(lang) => {
                    out.push((lang.as_str(), code.as_str()))
                }
                Block::Quote(blocks)
                | Block::Alert { blocks, .. }
                | Block::Footnote { blocks, .. }
                | Block::Embed {
                    content: Some(Ok(blocks)),
                    ..
                } => walk(blocks, out),
                Block::List { items, .. } => {
                    for item in items {
                        walk(&item.blocks, out);
                    }
                }
                Block::DefinitionList(entries) => {
                    for (term, details) in entries {
                        walk(term, out);
                        for detail in details {
                            walk(detail, out);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(blocks, &mut out);
    out
}

fn is_diagram(lang: &str) -> bool {
    lang.eq_ignore_ascii_case("mermaid")
}

struct Writer<'d> {
    doc: &'d Document,
    options: &'d PageOptions,
    /// The colours of each code block, by its text.
    colors: HashMap<&'d str, Vec<HlSpan>>,
    /// Top-level blocks some anchor points at.
    targets: HashSet<usize>,
    /// Diagrams written, for their ids.
    diagrams: usize,
    /// `<details>` left open in the output.
    open_details: usize,
    footnotes_started: bool,
    out: String,
}

impl<'d> Writer<'d> {
    fn head(&mut self) {
        let o = self.options;
        let body_font = family_list(
            o.body_font.as_deref(),
            "\"Segoe UI\", system-ui, sans-serif",
        );
        let mono_font = family_list(
            o.mono_font.as_deref(),
            "Consolas, \"Courier New\", monospace",
        );
        let [r, g, b] = o.mark_bg;
        // Scripts are kept out, but for the one that opens the print dialog.
        let script = if o.print_on_load {
            "<script>addEventListener(\"load\", () => print());</script>\n"
        } else {
            "<meta http-equiv=\"Content-Security-Policy\" content=\"script-src 'none'\">\n"
        };
        let _ = write!(
            self.out,
            "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n{script}\
             <title>{title}</title>\n<style>\n{css}</style>\n</head>\n<body>\n",
            title = escape(&o.title),
            css = STYLE
                .replace("$BODY_FONT", &body_font)
                .replace("$MONO_FONT", &mono_font)
                .replace("$TEXT_PX", &format!("{:.1}", o.text_px))
                .replace("$ZOOM", &format!("{:.3}", zoom(o.column_px)))
                .replace("$MARK", &format!("#{r:02x}{g:02x}{b:02x}"))
                .replace("$TEXT", TEXT)
                .replace("$MUTED", MUTED)
                .replace("$LINK", LINK)
                .replace("$BORDER", BORDER)
                .replace("$CODE_BLOCK_BG", CODE_BLOCK_BG)
                .replace("$CODE_BG", CODE_BG),
        );
    }

    /// A top-level block, with the id that links into the document lead to.
    fn top_level(&mut self, index: usize, block: &Block) {
        if matches!(block, Block::Footnote { .. }) && !self.footnotes_started {
            self.footnotes_started = true;
            self.out.push_str("<hr class=\"footnotes\">\n");
        }
        let id = self.targets.contains(&index).then(|| block_id(index));
        self.block(block, id.as_deref());
    }

    fn blocks(&mut self, blocks: &[Block]) {
        let depth = self.open_details;
        for block in blocks {
            self.block(block, None);
        }
        self.close_details_to(depth);
    }

    /// Closes the `<details>` opened past `depth`: one whose end is not in
    /// its list of blocks ends with the list.
    fn close_details_to(&mut self, depth: usize) {
        while self.open_details > depth {
            self.open_details -= 1;
            self.out.push_str("</details>\n");
        }
    }

    fn block(&mut self, block: &Block, id: Option<&str>) {
        let id_attr = id.map(|id| format!(" id=\"{id}\"")).unwrap_or_default();
        match block {
            Block::Heading {
                level,
                inlines,
                align,
            } => {
                let level = (*level).clamp(1, 6);
                let _ = write!(self.out, "<h{level}{id_attr}{}>", align_attr(*align));
                self.inlines(inlines);
                let _ = writeln!(self.out, "</h{level}>");
            }
            Block::Paragraph { inlines, align } => {
                let _ = write!(self.out, "<p{id_attr}{}>", align_attr(*align));
                self.inlines(inlines);
                self.out.push_str("</p>\n");
            }
            Block::Code { lang, code, .. } if is_diagram(lang) => {
                self.diagrams += 1;
                match crate::ui::diagram::html_svg(code, &format!("d{}-", self.diagrams)) {
                    Ok(svg) => {
                        let _ = writeln!(self.out, "<div class=\"diagram\"{id_attr}>{svg}</div>");
                    }
                    Err(error) => self.code(lang, code, Some(&error), &id_attr),
                }
            }
            Block::Code { lang, code, .. } => self.code(lang, code, None, &id_attr),
            Block::Math { tex, .. } => match crate::ui::math::html_svg(tex, true, text_rgba()) {
                Ok(svg) => {
                    let _ = writeln!(self.out, "<div class=\"math\"{id_attr}>{svg}</div>");
                }
                Err(error) => self.code("tex", tex, Some(&error), &id_attr),
            },
            Block::Quote(blocks) => {
                let _ = writeln!(self.out, "<blockquote{id_attr}>");
                self.blocks(blocks);
                self.out.push_str("</blockquote>\n");
            }
            Block::Alert {
                kind,
                title,
                fold,
                blocks,
            } => {
                let _ = writeln!(
                    self.out,
                    "<div class=\"alert alert-{}\"{id_attr}>\n<p class=\"alert-title\">{}</p>",
                    alert_class(*kind),
                    escape(title)
                );
                // A folded callout prints as the reader left it.
                let open =
                    fold.is_none_or(|f| f.open != self.options.toggled_details.contains(&f.index));
                if open {
                    self.blocks(blocks);
                }
                self.out.push_str("</div>\n");
            }
            Block::List {
                ordered,
                start,
                tight,
                items,
            } => self.list(*ordered, *start, *tight, items, &id_attr),
            Block::Table(table) => {
                let _ = writeln!(self.out, "<table{id_attr}>");
                // An HTML table may have no header.
                if !table.header.is_empty() {
                    self.out.push_str("<thead>\n<tr>");
                    for (column, cell) in table.header.iter().enumerate() {
                        self.cell("th", table.aligns.get(column).copied(), cell);
                    }
                    self.out.push_str("</tr>\n</thead>\n");
                }
                self.out.push_str("<tbody>\n");
                for row in &table.rows {
                    self.out.push_str("<tr>");
                    for (column, cell) in row.iter().enumerate() {
                        self.cell("td", table.aligns.get(column).copied(), cell);
                    }
                    self.out.push_str("</tr>\n");
                }
                self.out.push_str("</tbody>\n</table>\n");
            }
            Block::DefinitionList(entries) => {
                let _ = writeln!(self.out, "<dl{id_attr}>");
                for (term, details) in entries {
                    self.out.push_str("<dt>");
                    self.blocks(term);
                    self.out.push_str("</dt>\n");
                    for detail in details {
                        self.out.push_str("<dd>");
                        self.blocks(detail);
                        self.out.push_str("</dd>\n");
                    }
                }
                self.out.push_str("</dl>\n");
            }
            Block::Footnote { label, blocks } => {
                let _ = writeln!(
                    self.out,
                    "<div class=\"footnote\"{id_attr}>\n<span class=\"footnote-label\">{}.</span>",
                    escape(label)
                );
                self.blocks(blocks);
                self.out.push_str("</div>\n");
            }
            Block::FrontMatter {
                properties: Some(properties),
                ..
            } if !properties.is_empty() => {
                let _ = writeln!(self.out, "<table class=\"properties\"{id_attr}>");
                for property in properties {
                    let values = property
                        .values
                        .iter()
                        .map(|v| escape(v).replace('\n', "<br>"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let _ = writeln!(
                        self.out,
                        "<tr><th>{}</th><td>{values}</td></tr>",
                        escape(&property.key)
                    );
                }
                self.out.push_str("</table>\n");
            }
            Block::FrontMatter { yaml, .. } => {
                let _ = writeln!(
                    self.out,
                    "<pre class=\"front-matter\"{id_attr}><code>{}</code></pre>",
                    escape(yaml.trim_end())
                );
            }
            Block::Rule => {
                let _ = writeln!(self.out, "<hr{id_attr}>");
            }
            Block::Embed { title, content, .. } => {
                let _ = writeln!(
                    self.out,
                    "<div class=\"embed\"{id_attr}>\n<p class=\"embed-title\">{}</p>",
                    escape(title)
                );
                match content {
                    Some(Ok(blocks)) => self.blocks(blocks),
                    Some(Err(problem)) => {
                        let _ = writeln!(self.out, "<p class=\"missing\">{}</p>", escape(problem));
                    }
                    None => {}
                }
                self.out.push_str("</div>\n");
            }
            Block::Details {
                index,
                open,
                summary,
            } => {
                let shown = *open != self.options.toggled_details.contains(index);
                self.open_details += 1;
                let _ = write!(
                    self.out,
                    "<details{id_attr}{}>\n<summary>",
                    if shown { " open" } else { "" }
                );
                if summary.is_empty() {
                    self.out.push_str("Details");
                } else {
                    self.inlines(summary);
                }
                self.out.push_str("</summary>\n");
            }
            Block::DetailsEnd => {
                if self.open_details > 0 {
                    self.open_details -= 1;
                    self.out.push_str("</details>\n");
                }
            }
        }
    }

    fn code(&mut self, lang: &str, code: &str, error: Option<&str>, id_attr: &str) {
        let _ = write!(self.out, "<pre{id_attr}><code>");
        let spans = self.colors.get(code);
        // The colours cover the code as written; its last line break is
        // not shown.
        let code = code.strip_suffix('\n').unwrap_or(code);
        match spans {
            Some(spans) if !spans.is_empty() => {
                let mut end = 0;
                for span in spans {
                    let range = span.range.start.min(code.len())..span.range.end.min(code.len());
                    if range.is_empty() {
                        continue;
                    }
                    let [r, g, b] = span.rgb;
                    let _ = write!(self.out, "<span style=\"color:#{r:02x}{g:02x}{b:02x}");
                    if span.bold {
                        self.out.push_str(";font-weight:600");
                    }
                    if span.italic {
                        self.out.push_str(";font-style:italic");
                    }
                    let _ = write!(self.out, "\">{}</span>", escape(&code[range.clone()]));
                    end = range.end;
                }
                self.out.push_str(&escape(&code[end..]));
            }
            _ => self.out.push_str(&escape(code)),
        }
        self.out.push_str("</code></pre>\n");
        if let Some(error) = error {
            let _ = writeln!(
                self.out,
                "<p class=\"error\">{}: {}</p>",
                escape(if lang.is_empty() { "error" } else { lang }),
                escape(error)
            );
        }
    }

    fn list(&mut self, ordered: bool, start: usize, tight: bool, items: &[ListItem], id: &str) {
        let tag = if ordered { "ol" } else { "ul" };
        let start_attr = if ordered && start != 1 {
            format!(" start=\"{start}\"")
        } else {
            String::new()
        };
        let class = if tight { " class=\"tight\"" } else { "" };
        let _ = writeln!(self.out, "<{tag}{id}{start_attr}{class}>");
        for item in items {
            match item.task {
                Some(task) => {
                    let checked = if task.checked { " checked" } else { "" };
                    let _ = write!(
                        self.out,
                        "<li class=\"task\"><input type=\"checkbox\" disabled{checked}>"
                    );
                }
                None => self.out.push_str("<li>"),
            }
            self.blocks(&item.blocks);
            self.out.push_str("</li>\n");
        }
        let _ = writeln!(self.out, "</{tag}>");
    }

    fn cell(&mut self, tag: &str, align: Option<Align>, inlines: &[Inline]) {
        let _ = write!(self.out, "<{tag}{}>", align_attr(align.unwrap_or_default()));
        self.inlines(inlines);
        let _ = write!(self.out, "</{tag}>");
    }

    fn inlines(&mut self, inlines: &[Inline]) {
        for inline in inlines {
            match inline {
                Inline::Text(rich) => self.rich(rich),
                Inline::Image(image) => self.image(image),
            }
        }
    }

    fn rich(&mut self, rich: &Rich) {
        let mut open_link: Option<u32> = None;
        for span in &rich.spans {
            if span.link != open_link {
                if open_link.is_some() {
                    self.out.push_str("</a>");
                }
                if let Some(link) = span.link {
                    let target = rich.links.get(link as usize).map_or("", String::as_str);
                    let _ = write!(self.out, "<a href=\"{}\">", escape(&self.href(target)));
                }
                open_link = span.link;
            }
            let text = rich.text.get(span.range.clone()).unwrap_or_default();
            self.span(text, span.style);
        }
        if open_link.is_some() {
            self.out.push_str("</a>");
        }
    }

    fn span(&mut self, text: &str, style: Style) {
        if style.has(Style::MATH) {
            let display = style.has(Style::DISPLAY);
            match crate::ui::math::html_svg(text, display, text_rgba()) {
                Ok(svg) => {
                    let class = if display {
                        "math-inline display"
                    } else {
                        "math-inline"
                    };
                    let _ = write!(self.out, "<span class=\"{class}\">{svg}</span>");
                }
                Err(_) => {
                    let _ = write!(self.out, "<code>{}</code>", escape(text));
                }
            }
            return;
        }
        const TAGS: [(Style, &str); 9] = [
            (Style::CODE, "code"),
            (Style::KBD, "kbd"),
            (Style::STRONG, "strong"),
            (Style::EMPH, "em"),
            (Style::STRIKE, "del"),
            (Style::INSERT, "ins"),
            (Style::MARK, "mark"),
            (Style::SUP, "sup"),
            (Style::SUB, "sub"),
        ];
        let tags: Vec<&str> = TAGS
            .iter()
            .filter(|(flag, _)| style.has(*flag))
            .map(|&(_, tag)| tag)
            .collect();
        for tag in &tags {
            let _ = write!(self.out, "<{tag}>");
        }
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                self.out.push_str("<br>\n");
            }
            self.out.push_str(&escape(line));
        }
        for tag in tags.iter().rev() {
            let _ = write!(self.out, "</{tag}>");
        }
    }

    fn image(&mut self, image: &ImageRef) {
        let base = self.options.base_dir.as_deref();
        let Some(src) = image_url(&image.src, base) else {
            let alt = if image.alt.is_empty() {
                "image"
            } else {
                &image.alt
            };
            let _ = write!(self.out, "<span class=\"missing\">[{}]</span>", escape(alt));
            return;
        };
        if let Some(link) = &image.link {
            let _ = write!(self.out, "<a href=\"{}\">", escape(&self.href(link)));
        }
        let _ = write!(
            self.out,
            "<img src=\"{}\" alt=\"{}\"",
            escape(&src),
            escape(&image.alt)
        );
        if let Some(width) = image.width {
            let _ = write!(self.out, " width=\"{width}\"");
        }
        if let Some(height) = image.height {
            let _ = write!(self.out, " height=\"{height}\"");
        }
        self.out.push('>');
        if image.link.is_some() {
            self.out.push_str("</a>");
        }
    }

    /// Where a link of the document leads on the page: an anchor in it
    /// becomes the id of its block, a relative path a `file:` URL.
    fn href(&self, target: &str) -> String {
        let target = target.trim();
        if let Some(fragment) = target.strip_prefix('#') {
            return match crate::app::resolve_anchor(self.doc, fragment) {
                Some(block) => format!("#{}", block_id(block)),
                None => target.to_owned(),
            };
        }
        if crate::app::has_url_scheme(target) {
            return target.to_owned();
        }
        let (path, fragment) = match target.split_once('#') {
            Some((path, fragment)) => (path, Some(fragment)),
            None => (target, None),
        };
        let path = path.split('?').next().unwrap_or(path);
        let decoded = crate::markdown::preprocess::percent_decode(path);
        let path = Path::new(&decoded);
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            match self.options.base_dir.as_deref() {
                // Not there: where the document's Obsidian vault has it.
                Some(base) if !base.join(path).exists() => {
                    crate::vault::locate_now(base, &decoded).unwrap_or_else(|| base.join(path))
                }
                Some(base) => base.join(path),
                None => return target.to_owned(),
            }
        };
        let mut url = file_url(&absolute);
        if let Some(fragment) = fragment {
            url.push('#');
            url.push_str(fragment);
        }
        url
    }
}

fn block_id(index: usize) -> String {
    format!("b{index}")
}

fn text_rgba() -> [u8; 4] {
    [0x1f, 0x23, 0x28, 0xff]
}

/// The URL of an image on the page: as the viewer finds it (a file next to
/// the document, a web address), or `None` where the viewer shows none.
fn image_url(src: &str, base_dir: Option<&Path>) -> Option<String> {
    let src = src.trim();
    if src.starts_with("data:") {
        return Some(src.to_owned());
    }
    let uri = crate::ui::viewer::image_uri_now(src, base_dir)?;
    // A path the viewer reads as it is: for a browser it is a URL.
    match uri.strip_prefix("file://") {
        Some(path) => Some(format!("file://{}", encode_path(path))),
        None => Some(uri),
    }
}

/// `file:///C:/dir/a%20b.md` for `C:\dir\a b.md`, `file://host/share/…` for a
/// UNC path.
pub fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    match text.strip_prefix("//") {
        Some(unc) => format!("file://{}", encode_path(unc)),
        None if text.starts_with('/') => format!("file://{}", encode_path(&text)),
        None => format!("file:///{}", encode_path(&text)),
    }
}

/// A path with what a URL reads otherwise percent-encoded.
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            '\\' => out.push('/'),
            ' ' | '%' | '#' | '?' | '"' | '<' | '>' | '`' | '^' | '{' | '}' | '|' => {
                let _ = write!(out, "%{:02X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

fn align_attr(align: Align) -> &'static str {
    match align {
        Align::Left => "",
        Align::Center => " style=\"text-align:center\"",
        Align::Right => " style=\"text-align:right\"",
    }
}

fn alert_class(kind: AlertKind) -> &'static str {
    match kind {
        AlertKind::Note => "note",
        AlertKind::Tip => "tip",
        AlertKind::Important => "important",
        AlertKind::Warning => "warning",
        AlertKind::Caution => "caution",
    }
}

/// A CSS font list: the reader's family first, if any, then `fallback`.
fn family_list(family: Option<&str>, fallback: &str) -> String {
    match family {
        Some(family) if !family.is_empty() => {
            let quoted = family.replace(['"', '\\', '<', '>', ';', '{', '}'], "");
            format!("\"{quoted}\", {fallback}")
        }
        _ => fallback.to_owned(),
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// The page's scale for a text column of `column_px` in the window: a line
/// on paper holds as much as there, but the print is never larger than its
/// own size, nor so small that it cannot be read (a column as wide as a
/// large window).
fn zoom(column_px: f32) -> f32 {
    let zoom = super::paper_text_px() / column_px;
    if zoom.is_finite() {
        zoom.clamp(0.6, 1.0)
    } else {
        1.0
    }
}

/// The style sheet; `$NAME`s are filled in by `Writer::head`. The side
/// margins are those of `super::paper_text_px` and `webview::pdf`.
const STYLE: &str = r#"@page { margin: 16mm 15mm; }
html { -webkit-print-color-adjust: exact; print-color-adjust: exact; }
body {
  margin: 0; color: $TEXT; background: #fff; zoom: $ZOOM;
  font-family: $BODY_FONT, "Segoe UI Emoji", "Segoe UI Symbol";
  font-size: $TEXT_PXpx; line-height: 1.5; overflow-wrap: break-word;
}
body > :first-child { margin-top: 0 !important; }
p, blockquote, ul, ol, dl, table, pre, details, .alert, .math, .diagram { margin: 0 0 1em; }
h1, h2, h3, h4, h5, h6 {
  margin: 1.5em 0 1em; font-weight: 600; line-height: 1.25;
  break-after: avoid; page-break-after: avoid;
}
h1 { font-size: 2em; padding-bottom: .3em; border-bottom: 1px solid $BORDER; }
h2 { font-size: 1.5em; padding-bottom: .3em; border-bottom: 1px solid $BORDER; }
h3 { font-size: 1.25em; }
h4 { font-size: 1em; }
h5 { font-size: .875em; }
h6 { font-size: .85em; color: $MUTED; }
a { color: $LINK; text-decoration: none; }
strong { font-weight: 600; }
code, kbd, pre { font-family: $MONO_FONT; }
code {
  font-size: 85%; padding: .2em .4em; border-radius: 6px; background: $CODE_BG;
}
pre {
  font-size: 85%; line-height: 1.45; padding: 1em; border-radius: 6px;
  background: $CODE_BLOCK_BG; white-space: pre-wrap; overflow-wrap: anywhere;
}
pre code { font-size: 100%; padding: 0; background: none; border-radius: 0; }
kbd {
  font-size: 85%; padding: .1em .4em; border: 1px solid $BORDER;
  border-bottom-width: 2px; border-radius: 6px; background: $CODE_BLOCK_BG;
}
mark { background: $MARK; color: inherit; }
sup, sub { line-height: 0; }
hr { height: .25em; margin: 1.5em 0; border: 0; background: $BORDER; }
hr.footnotes { height: 1px; margin-top: 2em; }
blockquote { margin-left: 0; margin-right: 0; padding: 0 1em; color: $MUTED; border-left: .25em solid $BORDER; }
blockquote > :last-child, .alert > :last-child, li > :last-child, dd > :last-child,
details > :last-child, .footnote > :last-child { margin-bottom: 0; }
ul, ol { padding-left: 2em; }
ul.tight > li > p, ol.tight > li > p { margin: 0; }
li + li { margin-top: .25em; }
li > ul, li > ol { margin-bottom: 0; }
li.task { list-style: none; }
li.task > input { margin: 0 .35em 0 -1.5em; vertical-align: -.1em; }
li.task > p:first-of-type { display: inline; }
dt { font-weight: 600; margin-top: 1em; }
dd { margin: 0 0 1em; padding: 0 1em; }
table { border-collapse: collapse; border-spacing: 0; max-width: 100%; font-size: .9375em; }
th, td { padding: 6px 10px; border: 1px solid $BORDER; overflow-wrap: anywhere; }
th { font-weight: 600; }
tbody tr:nth-child(2n) { background: $CODE_BLOCK_BG; }
tr, img, svg, .diagram, .math { break-inside: avoid; page-break-inside: avoid; }
img { max-width: 100%; border: 0; }
img[width] { height: auto; }
.alert { padding: .5em 1em; border-left: .25em solid var(--alert); }
.alert-title { margin: 0 0 .5em; font-weight: 600; color: var(--alert); }
.embed { padding: 0 1em; border-left: .25em solid $BORDER; }
.embed-title { margin: 0 0 .5em; font-size: .85em; color: $MUTED; }
.alert-note { --alert: #0969da; }
.alert-tip { --alert: #1a7f37; }
.alert-important { --alert: #8250df; }
.alert-warning { --alert: #9a6700; }
.alert-caution { --alert: #d1242f; }
.math { text-align: center; }
.math svg { max-width: 100%; }
.math-inline.display { display: block; margin: .5em 0; text-align: center; }
.diagram { text-align: center; }
.diagram svg { max-width: 100%; height: auto; }
.footnote { font-size: .875em; color: $MUTED; }
.footnote-label { float: left; margin-right: .4em; }
.front-matter { color: $MUTED; }
table.properties th { text-align: left; font-weight: normal; color: $MUTED; background: none; }
table.properties th, table.properties td { border: none; padding: .1em 1em .1em 0; }
.error, .missing { color: $MUTED; font-size: .875em; }
summary { margin-bottom: .5em; }
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> PageOptions {
        PageOptions {
            title: "a <b>.md".to_owned(),
            base_dir: Some(PathBuf::from(r"C:\docs\my notes")),
            body_font: None,
            mono_font: Some("Fira Code".to_owned()),
            text_px: 16.0,
            column_px: 880.0,
            mark_bg: [0xff, 0xee, 0x80],
            toggled_details: HashSet::new(),
            print_on_load: false,
        }
    }

    fn html(source: &str) -> String {
        page(&Document::parse(source), &options())
    }

    #[test]
    fn the_page_has_a_title_and_styles() {
        let page = html("text");
        assert!(page.starts_with("<!DOCTYPE html>"));
        assert!(page.contains("<title>a &lt;b&gt;.md</title>"));
        assert!(page.contains("\"Fira Code\", Consolas"));
        assert!(page.contains("#ffee80"));
        assert!(page.contains("font-size: 16.0px"));
        assert!(!page.contains('$'), "a placeholder left");
        assert!(page.ends_with("</body>\n</html>\n"));
    }

    #[test]
    fn a_line_on_paper_holds_what_it_holds_in_the_window() {
        let paper = super::super::paper_text_px();
        assert!((zoom(paper) - 1.0).abs() < 1e-6);
        assert!((zoom(paper * 1.25) - 0.8).abs() < 1e-6);
        assert_eq!(zoom(paper / 2.0), 1.0);
        assert_eq!(zoom(f32::INFINITY), 0.6);
        assert_eq!(zoom(0.0), 1.0);
        assert!(html("text").contains(&format!("zoom: {:.3};", paper / 880.0)));
    }

    #[test]
    fn text_styles_become_tags_and_text_is_escaped() {
        let page = html("**bold** *it* `a<b` ~~no~~ ==hi== <kbd>K</kbd> & x\nnext");
        for part in [
            "<strong>bold</strong>",
            "<em>it</em>",
            "<code>a&lt;b</code>",
            "<del>no</del>",
            "<mark>hi</mark>",
            "<kbd>K</kbd>",
            "&amp; x<br>\nnext",
        ] {
            assert!(page.contains(part), "{part} in {page}");
        }
    }

    #[test]
    fn raw_html_does_not_reach_the_page() {
        let page = html("<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n");
        assert!(!page.contains("<script>alert"));
        assert!(!page.contains("onerror"));
    }

    #[test]
    fn links_lead_to_blocks_files_and_the_web() {
        let page = html(
            "# Intro\n\n[up](#intro) [web](https://example.com/?a=1&b=2) [doc](other%20doc.md#part)\n",
        );
        assert!(page.contains("<h1 id=\"b0\">Intro</h1>"), "{page}");
        assert!(page.contains("<a href=\"#b0\">up</a>"));
        assert!(page.contains("<a href=\"https://example.com/?a=1&amp;b=2\">web</a>"));
        assert!(
            page.contains("<a href=\"file:///C:/docs/my%20notes/other%20doc.md#part\">doc</a>"),
            "{page}"
        );
    }

    #[test]
    fn images_are_found_next_to_the_document() {
        let page = html("![a cat](img/cat.png) ![[pic.png|300]]");
        assert!(
            page.contains("<img src=\"file:///C:/docs/my%20notes/img/cat.png\" alt=\"a cat\">"),
            "{page}"
        );
        assert!(page.contains("width=\"300\""), "{page}");
    }

    #[test]
    fn code_is_coloured_and_diagrams_and_formulas_are_svg() {
        let page = html(
            "```rust\nfn main() {}\n```\n\n```mermaid\nflowchart LR\n  A --> B\n```\n\n$x^2$ and\n\n$$\n\\frac{a}{b}\n$$\n",
        );
        assert!(page.contains("<pre><code><span style=\"color:#"), "{page}");
        assert!(page.contains("<div class=\"diagram\"><svg "));
        assert!(page.contains("<span class=\"math-inline\"><svg "));
        assert!(page.contains("<div class=\"math\"><svg "));
    }

    #[test]
    fn lists_tasks_tables_and_alerts() {
        let page = html(
            "- [x] done\n- [ ] todo\n\n3. three\n\n| a | b |\n|:-:|--:|\n| 1 | 2 |\n\n> [!WARNING]\n> careful\n",
        );
        assert!(page.contains("<input type=\"checkbox\" disabled checked>"));
        assert!(page.contains("<ol start=\"3\" class=\"tight\">"), "{page}");
        assert!(page.contains("<th style=\"text-align:center\"><strong>a</strong></th>"));
        assert!(page.contains("<td style=\"text-align:right\">2</td>"));
        assert!(page.contains("<div class=\"alert alert-warning\">"));
    }

    #[test]
    fn details_are_shown_as_the_reader_left_them() {
        let source = "<details>\n<summary>More</summary>\n\nhidden\n\n</details>\n\nafter\n";
        let doc = Document::parse(source);
        let closed = page(&doc, &options());
        assert!(
            closed.contains("<details>\n<summary>More</summary>"),
            "{closed}"
        );
        assert_eq!(closed.matches("</details>").count(), 1);
        let mut toggled = options();
        toggled.toggled_details.insert(0);
        let opened = page(&doc, &toggled);
        assert!(opened.contains("<details open>"), "{opened}");
    }

    #[test]
    fn footnotes_link_to_their_text() {
        let page = html("Text[^n].\n\n[^n]: The note.\n");
        assert!(page.contains("<sup>[1]</sup>"), "{page}");
        assert!(page.contains("<hr class=\"footnotes\">"));
        assert!(page.contains("<div class=\"footnote\" id=\"b"));
        let target = page.split("<a href=\"#").nth(1).unwrap();
        let id = &target[..target.find('"').unwrap()];
        assert!(page.contains(&format!("class=\"footnote\" id=\"{id}\"")));
    }

    #[test]
    fn file_urls_are_encoded() {
        assert_eq!(
            file_url(Path::new(r"C:\a b\#1.md")),
            "file:///C:/a%20b/%231.md"
        );
        assert_eq!(
            file_url(Path::new(r"\\host\share\x.md")),
            "file://host/share/x.md"
        );
    }
}
