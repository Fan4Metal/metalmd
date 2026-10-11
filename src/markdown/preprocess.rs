//! Text-level preprocessing that runs before comrak, and the comrak options.
//!
//! Ported from Markpad's `src-tauri/src/markdown.rs`: wikilinks
//! (`[[#Heading]]`, `[[file#Heading|alias]]`), `^block-id` anchors, Obsidian
//! embeds (`![[image.png|300x200]]`), parenthesized bare URLs, and the
//! CommonMark-accurate code region scan that keeps all of these out of code.
//!
//! Differences from Markpad: embeds are rewritten to Markdown images rather
//! than `<img>` HTML (there is no browser here to render HTML), and math is
//! left to comrak's own `math_dollars` extension instead of being masked.
//!
//! Copyright (c) 2023, Alec Ames. Licensed under the BSD 3-Clause License;
//! see THIRD_PARTY_LICENSES.md.

use comrak::{Anchorizer, Options};
use regex::{Captures, Regex};
use std::borrow::Cow;
use std::sync::LazyLock;

/// `![[target]]` / `![[target|size]]`. Deliberately not `(?s)`: an embed never
/// spans lines, so a stray `![[` cannot pair with a `]]` several lines below.
static INTERNAL_EMBED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[\[(.*?)\]\]").expect("valid regex literal"));
/// `[[target]]` / `[[target|alias]]` where the target names a heading. The `#`
/// is a cheap prefilter; `process_wikilinks` re-checks it is in the target.
static WIKILINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]]*#[^\]]*)\]\]").expect("valid regex literal"));
/// ` ^block-id` at the end of a line. The leading whitespace is captured and
/// re-emitted so a block id alone on its own line keeps its newline. `R`
/// makes `$` match before `\r\n` and `\r` too: files from Windows editors.
static BLOCK_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?mR)(\s+)\^([a-zA-Z0-9_-]+)$").expect("valid regex literal"));

/// Title marker carried by a rewritten embed so the renderer can apply the
/// requested size: `![alt](src "qmd-size:300x200")`.
pub const EMBED_SIZE_TITLE_PREFIX: &str = "qmd-size:";
/// Class of the empty anchor a `^block-id` becomes.
pub const BLOCK_ID_ANCHOR_CLASS: &str = "block-id-anchor";

/// The comrak configuration documents are parsed with.
///
/// Markpad's dialect: GFM plus `==highlight==`, `++insert++`, `^[inline
/// footnotes]`, definition lists, and single newlines as line breaks.
/// `~sub~`, `^sup^` and `||spoiler||` stay off because they misread ordinary
/// GFM text (`~x~` is strikethrough, `a^2 + b^2` is prose, `||` is an empty
/// table cell). Alerts, dollar math and front matter are parsed by comrak.
pub fn markdown_options<'a>() -> Options<'a> {
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.highlight = true;
    options.extension.inline_footnotes = true;
    options.extension.insert = true;
    options.extension.footnotes = true;
    options.extension.description_lists = true;
    options.extension.alerts = true;
    options.extension.math_dollars = true;
    options.extension.front_matter_delimiter = Some("---".to_owned());
    options.extension.header_id_prefix = Some(String::new());
    options.render.hardbreaks = true;
    options
}

/// The anchor id comrak assigns to a heading with this text. A fresh
/// anchorizer per lookup: a link names a heading by text, and text can only
/// address the first heading that reads that way.
pub fn heading_anchor_id(target: &str) -> String {
    Anchorizer::new().anchorize(target.trim())
}

/// File extensions the viewer opens as documents.
pub const MARKDOWN_LINK_EXTENSIONS: [&str; 5] = ["md", "markdown", "mdown", "mkd", "txt"];

/// The extension of the last path component, if the trailing `.segment`
/// really reads as one. `v1.2 spec` has none, neither does `.gitignore`.
pub fn link_path_extension(path: &str) -> Option<&str> {
    let name = path.rsplit(['/', '\\']).next()?;
    let (stem, ext) = name.rsplit_once('.')?;
    let plausible = !stem.is_empty()
        && (1..=8).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric());
    plausible.then_some(ext)
}

/// Whether `path` names a document the viewer opens in a tab.
pub fn is_markdown_path(path: &str) -> bool {
    link_path_extension(path).is_some_and(|ext| {
        MARKDOWN_LINK_EXTENSIONS
            .iter()
            .any(|known| ext.eq_ignore_ascii_case(known))
    })
}

/// Percent-encodes what would end or corrupt a Markdown link destination.
fn encode_link_destination(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for character in path.chars() {
        match character {
            '%' => encoded.push_str("%25"),
            ' ' => encoded.push_str("%20"),
            '(' => encoded.push_str("%28"),
            ')' => encoded.push_str("%29"),
            '<' => encoded.push_str("%3C"),
            '>' => encoded.push_str("%3E"),
            '"' => encoded.push_str("%22"),
            '?' => encoded.push_str("%3F"),
            _ => encoded.push(character),
        }
    }
    encoded
}

/// Percent-encodes an embed target: `%`, controls, space and `()<>?#\"`.
fn encode_embed_destination(path: &str) -> String {
    const SPECIAL: &str = "\x7f%()<>?#\\\"";
    let mut encoded = String::with_capacity(path.len());
    for character in path.chars() {
        if character <= ' ' || SPECIAL.contains(character) {
            encoded.push_str(&format!("%{:02X}", character as u32));
        } else {
            encoded.push(character);
        }
    }
    encoded
}

/// Backslash-escapes the characters that would give Markdown meaning to
/// literal text placed inside `[...]`.
fn escape_markdown_text(text: &str) -> String {
    const SPECIAL: &str = "\\[]*_`<>!~=+^$|";
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if SPECIAL.contains(character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Decodes `%XX` escapes. Invalid sequences are kept as written.
pub fn percent_decode(input: &str) -> String {
    if !input.contains('%') {
        return input.to_owned();
    }
    let bytes = input.as_bytes();
    let hex = |b: u8| (b as char).to_digit(16);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    match String::from_utf8(out) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    }
}

/// The link destination for the file half of a wikilink, or `None` when the
/// target is not something the viewer can open (the wikilink then stays
/// literal). `.md` is appended when the target has no extension.
fn wikilink_file_destination(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() || path.ends_with('/') || path.ends_with('\\') {
        return None;
    }
    match link_path_extension(path) {
        Some(_) => is_markdown_path(path).then(|| encode_link_destination(path)),
        None => Some(format!("{}.md", encode_link_destination(path))),
    }
}

/// Byte ranges of code regions (fenced and indented code blocks, inline code
/// spans) paired with CommonMark's rules, in ascending document order.
///
/// Indented code blocks are recognised only where a line scan can be sure of
/// one: after a blank line, at the top level, outside any list.
pub fn code_region_ranges(content: &str) -> Vec<(usize, usize)> {
    let len = content.len();
    let mut regions: Vec<(usize, usize)> = Vec::new();
    // (fence char, opener run length, region start)
    let mut fence: Option<(u8, usize, usize)> = None;
    let mut seg_start = 0usize;
    let mut prev_blank = true;
    let mut list_open = false;

    let mut line_start = 0usize;
    while line_start < len {
        let in_fence = fence.is_some();
        let line_end = content[line_start..]
            .find('\n')
            .map(|i| line_start + i + 1)
            .unwrap_or(len);
        let line = &content[line_start..line_end];
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        let marker = trimmed.as_bytes().first().copied();
        let run_len = trimmed
            .as_bytes()
            .iter()
            .take_while(|&&b| Some(b) == marker)
            .count();
        let is_fence_line =
            indent <= 3 && matches!(marker, Some(b'`') | Some(b'~')) && run_len >= 3;

        match fence {
            Some((ch, opener_len, start)) => {
                if is_fence_line
                    && marker == Some(ch)
                    && run_len >= opener_len
                    && trimmed[run_len..].trim().is_empty()
                {
                    regions.push((start, line_end));
                    fence = None;
                    seg_start = line_end;
                }
            }
            None => {
                if prev_blank && !list_open && !line.trim().is_empty() && is_indented_line(line) {
                    let block_end = indented_code_block_end(content, line_start);
                    if seg_start < line_start {
                        push_inline_code_spans(content, seg_start, line_start, &mut regions);
                    }
                    regions.push((line_start, block_end));
                    seg_start = block_end;
                    line_start = block_end;
                    prev_blank = false;
                    continue;
                }
                // The info string of a backtick fence may not contain backticks.
                let info_ok = marker != Some(b'`') || !trimmed[run_len..].contains('`');
                if is_fence_line && info_ok {
                    if seg_start < line_start {
                        push_inline_code_spans(content, seg_start, line_start, &mut regions);
                    }
                    fence = Some((marker.expect("fence marker"), run_len, line_start));
                }
            }
        }
        if !in_fence {
            let blank = line.trim().is_empty();
            if !blank {
                if indent <= 3 && opens_a_list(trimmed) {
                    list_open = true;
                } else if indent == 0 && prev_blank {
                    list_open = false;
                }
            }
            prev_blank = blank;
        }
        line_start = line_end;
    }
    match fence {
        // An unclosed fence runs to the end of the input.
        Some((_, _, start)) => regions.push((start, len)),
        None => {
            if seg_start < len {
                push_inline_code_spans(content, seg_start, len, &mut regions);
            }
        }
    }

    debug_assert!(
        regions.windows(2).all(|pair| pair[0] <= pair[1]),
        "code_region_ranges emitted regions out of document order: {regions:?}",
    );
    regions
}

fn is_indented_line(line: &str) -> bool {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    indent >= 4 || line[..indent].contains('\t')
}

fn opens_a_list(trimmed: &str) -> bool {
    let bytes = trimmed.as_bytes();
    let rest = match bytes.first() {
        Some(b'-' | b'+' | b'*') => &trimmed[1..],
        _ => {
            let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
            match (digits, bytes.get(digits)) {
                (1..=9, Some(b'.' | b')')) => &trimmed[digits + 1..],
                _ => return false,
            }
        }
    };
    rest.is_empty() || rest.starts_with([' ', '\t', '\n', '\r'])
}

fn indented_code_block_end(content: &str, line_start: usize) -> usize {
    let len = content.len();
    let mut end = line_start;
    let mut cursor = line_start;
    while cursor < len {
        let line_end = content[cursor..]
            .find('\n')
            .map(|i| cursor + i + 1)
            .unwrap_or(len);
        let line = &content[cursor..line_end];
        if !line.trim().is_empty() {
            if !is_indented_line(line) {
                break;
            }
            end = line_end;
        }
        cursor = line_end;
    }
    end
}

/// Inline code spans between `start` and `end`. A blank line ends a block,
/// so backtick runs pair only within one paragraph.
fn push_inline_code_spans(
    content: &str,
    start: usize,
    end: usize,
    regions: &mut Vec<(usize, usize)>,
) {
    let mut chunk_start = start;
    let mut line_start = start;
    while line_start < end {
        let line_end = content[line_start..end]
            .find('\n')
            .map(|i| line_start + i + 1)
            .unwrap_or(end);
        if content[line_start..line_end].trim().is_empty() {
            pair_inline_code_runs(content, chunk_start, line_start, regions);
            chunk_start = line_end;
        }
        line_start = line_end;
    }
    pair_inline_code_runs(content, chunk_start, end, regions);
}

/// A span opened by N backticks closes only on a run of exactly N.
fn pair_inline_code_runs(
    content: &str,
    start: usize,
    end: usize,
    regions: &mut Vec<(usize, usize)>,
) {
    let chunk = &content.as_bytes()[start..end];
    let mut runs: Vec<(usize, usize)> = Vec::new(); // (offset in chunk, len)
    let mut i = 0usize;
    while i < chunk.len() {
        if chunk[i] == b'`' {
            let run_start = i;
            while i < chunk.len() && chunk[i] == b'`' {
                i += 1;
            }
            runs.push((run_start, i - run_start));
        } else {
            i += 1;
        }
    }

    let mut r = 0usize;
    while r < runs.len() {
        let (open_start, open_len) = runs[r];
        if let Some(close) = (r + 1..runs.len()).find(|&j| runs[j].1 == open_len) {
            let (close_start, close_len) = runs[close];
            regions.push((start + open_start, start + close_start + close_len));
            r = close + 1;
        } else {
            r += 1;
        }
    }
}

fn in_code_region(regions: &[(usize, usize)], pos: usize) -> bool {
    regions
        .binary_search_by(|&(s, e)| {
            if pos < s {
                std::cmp::Ordering::Greater
            } else if pos >= e {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// The code regions and the math between them, in document order: what
/// the block-id rewrite must leave as it is (` ^T` ending a line of a
/// formula is not a block id).
fn code_and_math_ranges(content: &str) -> Vec<(usize, usize)> {
    let code = code_region_ranges(content);
    if !content.contains('$') {
        return code;
    }
    let mut ranges = math_ranges(content, &code);
    ranges.extend(code);
    ranges.sort_unstable();
    ranges
}

/// Byte ranges of `$…$` and `$$…$$` outside `code`, read as comrak's dollar
/// math reads them: an opening `$` not followed by whitespace, a closing one
/// not preceded by whitespace nor followed by a digit (`$5 and $6` is
/// prose), both in one paragraph and with no code between them.
///
/// The closing delimiters are listed first and each opening one takes the
/// next one, so a paragraph of unclosed `$` is not scanned again for each.
fn math_ranges(content: &str, code: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let bytes = content.as_bytes();
    let escaped = |at: usize| {
        let backslashes = bytes[..at]
            .iter()
            .rev()
            .take_while(|&&b| b == b'\\')
            .count();
        backslashes % 2 == 1
    };
    // The delimiters: unescaped `$` outside code, `$$` as one, by position
    // and length.
    let dollars: Vec<usize> = content
        .match_indices('$')
        .map(|(at, _)| at)
        .filter(|&at| !in_code_region(code, at) && !escaped(at))
        .collect();
    let mut delimiters: Vec<(usize, usize)> = Vec::with_capacity(dollars.len());
    let mut k = 0;
    while k < dollars.len() {
        if dollars.get(k + 1) == Some(&(dollars[k] + 1)) {
            delimiters.push((dollars[k], 2));
            k += 2;
        } else {
            delimiters.push((dollars[k], 1));
            k += 1;
        }
    }
    if delimiters.len() < 2 {
        return Vec::new();
    }

    // Where paragraphs end: the line end before each blank line.
    let mut blank_lines = Vec::new();
    let mut previous_end = None;
    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        if let Some(end) = previous_end
            && line.trim().is_empty()
        {
            blank_lines.push(end);
        }
        offset += line.len();
        previous_end = Some(offset - 1);
    }
    let paragraph_end = |at: usize| {
        let i = blank_lines.partition_point(|&b| b < at);
        blank_lines.get(i).copied().unwrap_or(content.len())
    };
    let next_code = |at: usize| {
        let i = code.partition_point(|&(start, _)| start < at);
        code.get(i).map_or(content.len(), |&(start, _)| start)
    };

    let doubles: Vec<usize> = delimiters
        .iter()
        .filter(|&&(_, len)| len == 2)
        .map(|&(at, _)| at)
        .collect();
    let closers: Vec<usize> = delimiters
        .iter()
        .filter(|&&(at, len)| {
            let after_space = content[..at]
                .chars()
                .next_back()
                .is_none_or(char::is_whitespace);
            let before_digit = bytes.get(at + 1).is_some_and(u8::is_ascii_digit);
            len == 1 && !after_space && !before_digit
        })
        .map(|&(at, _)| at)
        .collect();

    let mut ranges = Vec::new();
    let (mut d, mut c) = (0, 0);
    let mut done_to = 0;
    for &(at, len) in &delimiters {
        if at < done_to {
            continue;
        }
        let limit = paragraph_end(at).min(next_code(at));
        if len == 2 {
            while d < doubles.len() && doubles[d] <= at {
                d += 1;
            }
            if let Some(&close) = doubles.get(d)
                && close < limit
            {
                ranges.push((at, close + 2));
                done_to = close + 2;
            }
            continue;
        }
        let opens = content[at + 1..]
            .chars()
            .next()
            .is_some_and(|ch| !ch.is_whitespace());
        if opens {
            while c < closers.len() && closers[c] <= at {
                c += 1;
            }
            if let Some(&close) = closers.get(c)
                && close < limit
            {
                ranges.push((at, close + 1));
                done_to = close + 1;
            }
        }
    }
    ranges
}

fn replace_outside_code<'a>(
    content: &'a str,
    re: &Regex,
    f: impl FnMut(&Captures) -> String,
) -> Cow<'a, str> {
    replace_outside(content, re, code_region_ranges, f)
}

/// Replaces the matches of `re` that do not start in one of the ranges
/// `protected` finds. Those are found at the first match: most documents
/// have none.
fn replace_outside<'a>(
    content: &'a str,
    re: &Regex,
    protected: fn(&str) -> Vec<(usize, usize)>,
    mut f: impl FnMut(&Captures) -> String,
) -> Cow<'a, str> {
    let mut regions = None;
    re.replace_all(content, |caps: &Captures| {
        let full = caps.get(0).expect("group 0 always matches");
        let regions = regions.get_or_insert_with(|| protected(content));
        if in_code_region(regions, full.start()) {
            full.as_str().to_string()
        } else {
            f(caps)
        }
    })
}

/// `![[path]]`, `![[path|300]]`, `![[path|300x200]]` → a Markdown image whose
/// title carries the requested size.
pub fn process_internal_embeds(content: &str) -> Cow<'_, str> {
    if !content.contains("![[") {
        return Cow::Borrowed(content);
    }
    replace_outside_code(content, &INTERNAL_EMBED_RE, |caps| {
        let inner = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let mut parts = inner.split('|');
        let path = parts.next().unwrap_or("").trim();
        // Nothing to embed: left as typed, not an image of the folder.
        if path.is_empty() {
            return caps[0].to_owned();
        }
        let size = parts.next().map(str::trim).filter(|size| {
            !size.is_empty() && size.chars().all(|c| c.is_ascii_digit() || c == 'x')
        });
        let src = encode_embed_destination(path);
        let alt = escape_markdown_text(path);
        match size {
            Some(size) => format!("![{alt}]({src} \"{EMBED_SIZE_TITLE_PREFIX}{size}\")"),
            None => format!("![{alt}]({src})"),
        }
    })
}

/// `[[#Heading]]`, `[[file#Heading]]` and their `|alias` forms become
/// Markdown links; ` ^block-id` at a line end becomes an empty anchor.
/// Bare `[[Note]]` is left literal on purpose: it would swallow `[[1]]`
/// citations and CommonMark reference links.
pub fn process_wikilinks(content: &str) -> Cow<'_, str> {
    let mut processed = Cow::Borrowed(content);

    if content.contains("[[") && WIKILINK_RE.is_match(&processed) {
        let source: &str = &processed;
        let replaced = replace_outside_code(source, &WIKILINK_RE, |caps| {
            let full = caps.get(0).expect("group 0 always matches");
            let literal = || full.as_str().to_string();
            // A target spanning lines can never resolve, and rewriting it
            // would collapse two source lines into one.
            if full.as_str().contains('\n') {
                return literal();
            }
            // `![[…]]` is an embed, never a link.
            if source.as_bytes()[..full.start()].last() == Some(&b'!') {
                return literal();
            }
            // `[[1#x]](url)` is a CommonMark link whose text is `[1#x]`.
            if source[full.end()..].starts_with('(') {
                return literal();
            }

            let inner = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let (target, alias) = match inner.split_once('|') {
                Some((target, alias)) => (target, Some(alias)),
                None => (inner, None),
            };
            let alias = alias.filter(|a| !a.trim().is_empty());
            // Only a `#` in the target half makes a heading link.
            let Some((path, heading)) = target.split_once('#') else {
                return literal();
            };
            let (path, heading) = (path.trim(), heading.trim());
            if heading.is_empty() {
                return literal();
            }
            let anchor = heading_anchor_id(heading);

            if path.is_empty() {
                return format!("[{}](#{anchor})", alias.unwrap_or(heading));
            }

            let Some(destination) = wikilink_file_destination(path) else {
                return literal();
            };
            // Obsidian renders an un-aliased heading link as "Note > Heading".
            format!(
                "[{}]({destination}#{anchor})",
                alias
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{path} > {heading}")),
            )
        });
        processed = Cow::Owned(replaced.into_owned());
    }

    if processed.contains('^') && BLOCK_ID_RE.is_match(&processed) {
        let replaced = replace_outside(&processed, &BLOCK_ID_RE, code_and_math_ranges, |caps| {
            let leading = caps.get(1).map(|m| m.as_str()).unwrap_or(" ");
            let id = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            format!("{leading}<a id=\"{id}\" class=\"{BLOCK_ID_ANCHOR_CLASS}\"></a>")
        });
        processed = Cow::Owned(replaced.into_owned());
    }

    processed
}

/// `(https://example.com/path)text` → the URL becomes an explicit link that
/// stops at the closing parenthesis instead of running into `text`.
pub fn process_parenthesized_autolinks(content: &str) -> Cow<'_, str> {
    if !content.contains("://") {
        return Cow::Borrowed(content);
    }
    let regions = code_region_ranges(content);
    let mut output = String::new();
    let mut copied_to = 0;
    let mut scan_from = 0;
    // An opening parenthesis and the one that closes it, within a run of
    // text without whitespace (a URL ends at whitespace). Found for a whole
    // run in one pass: scanning on from each `(http` would go over the same
    // text again for every one of them, quadratic in a long run.
    let mut opens: Vec<(usize, Option<usize>)> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();

    let mut run_start = 0;
    while run_start < content.len() {
        let run_end = content[run_start..]
            .find(char::is_whitespace)
            .map_or(content.len(), |offset| run_start + offset);
        let run = &content[run_start..run_end];
        if run.contains('(') {
            opens.clear();
            stack.clear();
            for (offset, byte) in run.bytes().enumerate() {
                match byte {
                    b'(' => {
                        stack.push(opens.len());
                        opens.push((run_start + offset, None));
                    }
                    b')' => {
                        if let Some(open) = stack.pop() {
                            opens[open].1 = Some(run_start + offset);
                        }
                    }
                    _ => {}
                }
            }
            for &(opening, closing) in &opens {
                if opening < scan_from {
                    continue;
                }
                let url_start = opening + 1;
                let url_tail = &content[url_start..];
                if !(url_tail.starts_with("http://")
                    || url_tail.starts_with("https://")
                    || url_tail.starts_with("ftp://"))
                {
                    continue;
                }
                let Some(closing) = closing else {
                    continue;
                };
                let after_closing = closing + ')'.len_utf8();
                scan_from = after_closing;
                let adjacent_text = content[after_closing..]
                    .chars()
                    .next()
                    .is_some_and(char::is_alphanumeric);
                if !adjacent_text || in_code_region(&regions, opening) {
                    continue;
                }

                let url = &content[url_start..closing];
                output.push_str(&content[copied_to..url_start]);
                output.push('[');
                output.push_str(url);
                output.push_str("](");
                output.push_str(url);
                output.push(')');
                output.push(')');
                copied_to = after_closing;
            }
        }
        // Past the whitespace that ended the run.
        run_start = content[run_end..]
            .chars()
            .next()
            .map_or(content.len(), |c| run_end + c.len_utf8());
    }

    if output.is_empty() {
        Cow::Borrowed(content)
    } else {
        output.push_str(&content[copied_to..]);
        Cow::Owned(output)
    }
}

/// The whole preprocessing pipeline, in Markpad's order.
pub fn preprocess(content: &str) -> Cow<'_, str> {
    let autolinks = process_parenthesized_autolinks(content);
    let embeds = match process_internal_embeds(&autolinks) {
        Cow::Borrowed(_) => autolinks,
        Cow::Owned(s) => Cow::Owned(s),
    };
    match process_wikilinks(&embeds) {
        Cow::Borrowed(_) => embeds,
        Cow::Owned(s) => Cow::Owned(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embed_protection_survives_longer_backtick_runs_earlier_in_the_doc() {
        let input = "```` ```mermaid ```` fence sample\n\ncode: `![[not-an-embed.md]]`\n";
        let out = process_internal_embeds(input);
        assert!(out.contains("`![[not-an-embed.md]]`"), "got: {out}");
    }

    #[test]
    fn embeds_inside_tilde_fences_are_protected() {
        let input = "~~~\n![[inside.md]]\n~~~\n\n![[outside.md]]\n";
        let out = process_internal_embeds(input);
        assert!(out.contains("![[inside.md]]"), "got: {out}");
        assert!(out.contains("![outside.md](outside.md)"), "got: {out}");
    }

    #[test]
    fn fence_closes_only_on_a_run_at_least_as_long() {
        let input = "````\n```\n![[still-code.md]]\n````\n![[after.md]]\n";
        let out = process_internal_embeds(input);
        assert!(out.contains("![[still-code.md]]"), "got: {out}");
        assert!(out.contains("![after.md](after.md)"), "got: {out}");
    }

    #[test]
    fn unclosed_fence_protects_to_end_of_input() {
        let out = process_internal_embeds("```\n![[never-closed.md]]\n");
        assert!(out.contains("![[never-closed.md]]"), "got: {out}");
    }

    #[test]
    fn double_backtick_span_pairs_only_with_double_backticks() {
        let input = "`` a ` ![[in-span.md]] `` then ![[outside.md]]\n";
        let out = process_internal_embeds(input);
        assert!(out.contains("![[in-span.md]]"), "got: {out}");
        assert!(out.contains("![outside.md](outside.md)"), "got: {out}");
    }

    #[test]
    fn embeds_outside_code_are_rewritten_with_sizes() {
        let out = process_internal_embeds("![[pic.png|300x200]]\n");
        assert_eq!(out, "![pic.png](pic.png \"qmd-size:300x200\")\n");
        let width_only = process_internal_embeds("![[pic.png|300]]\n");
        assert!(width_only.contains("\"qmd-size:300\""), "got: {width_only}");
    }

    #[test]
    fn embed_targets_are_encoded_and_escaped() {
        let out = process_internal_embeds("![[my pic (1).png]]\n");
        assert!(out.contains("(my%20pic%20%281%29.png)"), "got: {out}");
        // A quote cannot end the destination early: it is percent-encoded.
        let quoted = process_internal_embeds("![[a\" b.png]]\n");
        assert!(quoted.contains("(a%22%20b.png)"), "got: {quoted}");
    }

    #[test]
    fn a_stray_backtick_does_not_swallow_later_paragraphs() {
        let input =
            "Use the ` character to start code.\n\n![[photo.png]]\n\nThen `run()` finishes.\n";
        let out = process_internal_embeds(input);
        assert!(out.contains("![photo.png](photo.png)"), "got: {out}");
        assert!(out.contains("`run()`"), "got: {out}");
    }

    #[test]
    fn inline_code_spans_still_pair_across_lines_inside_one_paragraph() {
        let out = process_internal_embeds("start `code\n![[inside.png]]` end\n");
        assert!(out.contains("![[inside.png]]"), "got: {out}");
    }

    #[test]
    fn multibyte_content_inside_a_fence_does_not_panic() {
        let input = "```text\n中文开头的一行\n```\n\n![[outside.png]]\n";
        let out = process_internal_embeds(input);
        assert!(out.contains("中文开头的一行"), "got: {out}");
        assert!(out.contains("![outside.png](outside.png)"), "got: {out}");
    }

    const FENCE_AFTER_INLINE_CODE: &str = concat!(
        "Prose with `a code span` in it.\n",
        "\n",
        "```text\n",
        "![[embed.md]]\n",
        "[[Notes#Setup]]\n",
        "line ^blockid\n",
        "(https://example.com/x)y\n",
        "```\n",
    );

    #[test]
    fn an_inline_span_before_a_fence_does_not_expose_the_fence() {
        let out = preprocess(FENCE_AFTER_INLINE_CODE);
        assert_eq!(out, FENCE_AFTER_INLINE_CODE);
    }

    /// Markpad's scan, one character at a time from each `(`: the reference
    /// the one-pass version must agree with.
    fn parenthesized_autolinks_by_rescanning(content: &str) -> String {
        let regions = code_region_ranges(content);
        let mut output = String::new();
        let mut copied_to = 0;
        let mut scan_from = 0;
        while let Some(opening_offset) = content[scan_from..].find('(') {
            let opening = scan_from + opening_offset;
            let url_start = opening + 1;
            let url_tail = &content[url_start..];
            if !(url_tail.starts_with("http://")
                || url_tail.starts_with("https://")
                || url_tail.starts_with("ftp://"))
            {
                scan_from = url_start;
                continue;
            }
            let mut depth = 1usize;
            let mut closing = None;
            for (offset, ch) in url_tail.char_indices() {
                if ch.is_whitespace() {
                    break;
                }
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            closing = Some(url_start + offset);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(closing) = closing else {
                scan_from = url_start;
                continue;
            };
            let after_closing = closing + 1;
            let adjacent_text = content[after_closing..]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric);
            if !adjacent_text || in_code_region(&regions, opening) {
                scan_from = after_closing;
                continue;
            }
            let url = &content[url_start..closing];
            output.push_str(&content[copied_to..url_start]);
            output.push_str(&format!("[{url}]({url}))"));
            copied_to = after_closing;
            scan_from = after_closing;
        }
        output.push_str(&content[copied_to..]);
        output
    }

    #[test]
    fn parenthesized_autolinks_match_the_rescanning_version() {
        let pieces = [
            "(",
            ")",
            "(http://a",
            "(https://b.c/",
            "x",
            "Ж",
            " ",
            "\n",
            "`",
            "\u{3000}",
            "ftp://",
        ];
        let mut state = 0x2545_f491_u32;
        for _ in 0..5000 {
            let mut text = String::new();
            for _ in 0..(state % 24) {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                text.push_str(pieces[state as usize % pieces.len()]);
            }
            state = state.wrapping_add(1);
            assert_eq!(
                process_parenthesized_autolinks(&text),
                parenthesized_autolinks_by_rescanning(&text),
                "{text:?}"
            );
        }
    }

    #[test]
    fn unclosed_parenthesized_urls_take_linear_time() {
        // Rescanning from each `(http` took minutes on this.
        let text = "(http://a".repeat(200_000);
        assert_eq!(process_parenthesized_autolinks(&text), text);
    }

    #[test]
    fn autolink_inside_parentheses_stops_before_adjacent_text() {
        let out = process_parenthesized_autolinks(
            "See (https://www.speedtest.net/awards/united_states/)for more information.",
        );
        assert_eq!(
            out,
            "See ([https://www.speedtest.net/awards/united_states/](https://www.speedtest.net/awards/united_states/))for more information."
        );
    }

    #[test]
    fn wikilink_targets_survive_punctuation_in_the_heading() {
        assert_eq!(heading_anchor_id("1. 概述"), "1-概述");
        let out = process_wikilinks("[[#1. 概述|Overview]]\n");
        assert!(out.contains("[Overview](#1-概述)"), "got: {out}");
    }

    #[test]
    fn multiline_wikilinks_are_left_literal() {
        let out = process_wikilinks("[[#first\nsecond|alias]]\n");
        assert!(out.contains("[[#first\nsecond|alias]]"), "got: {out}");
    }

    #[test]
    fn copy_reference_output_becomes_a_real_link() {
        let out = process_wikilinks("[[Notes#Setup]]\n");
        assert!(
            out.contains("[Notes > Setup](Notes.md#setup)"),
            "got: {out}"
        );
    }

    #[test]
    fn file_wikilink_href_carries_a_markdown_extension() {
        let out = process_wikilinks("[[docs/Guide#Setup]]\n");
        assert!(out.contains("(docs/Guide.md#setup)"), "got: {out}");
    }

    #[test]
    fn file_wikilink_keeps_an_extension_it_was_already_given() {
        let out = process_wikilinks("[[Notes.md#Setup]]\n");
        assert!(out.contains("(Notes.md#setup)"), "got: {out}");
        assert!(!out.contains("Notes.md.md"), "got: {out}");
        let txt = process_wikilinks("[[log.txt#Errors]]\n");
        assert!(txt.contains("(log.txt#errors)"), "got: {txt}");
    }

    #[test]
    fn wikilinks_without_a_heading_are_deliberately_left_literal() {
        for input in [
            "[[Notes]]\n",
            "[[1]] Author, Title.\n",
            "[[TODO]] revisit this.\n",
            "[[foo]] and [[foo|bar]]\n",
            "[[docs/Guide|Guide]]\n",
            "[[Notes|see #1]]\n",
        ] {
            assert_eq!(process_wikilinks(input), input, "should be literal");
        }
    }

    #[test]
    fn file_wikilink_alias_and_subfolder_and_punctuated_heading() {
        let out = process_wikilinks("[[docs/Guide#1. 概述|Overview]]\n");
        assert!(
            out.contains("[Overview](docs/Guide.md#1-概述)"),
            "got: {out}"
        );
    }

    #[test]
    fn file_wikilink_percent_encodes_what_would_break_the_destination() {
        let out = process_wikilinks("[[My Notes (v2)#Setup]]\n");
        assert!(
            out.contains("(My%20Notes%20%28v2%29.md#setup)"),
            "got: {out}"
        );
        assert!(out.contains("[My Notes (v2) > Setup]"), "got: {out}");
    }

    #[test]
    fn file_wikilink_block_reference_targets_the_block_id_anchor() {
        let out = process_wikilinks("[[Notes#^abc123]]\n");
        assert!(out.contains("(Notes.md#abc123)"), "got: {out}");
    }

    #[test]
    fn wikilinks_to_files_the_viewer_cannot_open_stay_literal() {
        for input in ["[[report.pdf#Intro]]\n", "[[diagram.svg#part]]\n"] {
            assert_eq!(process_wikilinks(input), input);
        }
    }

    #[test]
    fn same_document_wikilinks() {
        let out = process_wikilinks("[[#Some Heading|jump]]\n");
        assert!(out.contains("[jump](#some-heading)"), "got: {out}");
        let bare = process_wikilinks("[[#Setup]]\n");
        assert!(bare.contains("[Setup](#setup)"), "got: {bare}");
    }

    #[test]
    fn file_wikilinks_in_code_spans_and_fences_stay_literal() {
        let span = process_wikilinks("`[[Notes#Setup]]` but [[Notes#Setup]]\n");
        assert!(span.contains("`[[Notes#Setup]]`"), "got: {span}");
        assert!(span.contains("(Notes.md#setup)"), "got: {span}");
        let fence = process_wikilinks("```\n[[Notes#Setup]]\n```\n");
        assert!(fence.contains("```\n[[Notes#Setup]]\n```"), "got: {fence}");
    }

    #[test]
    fn embeds_are_not_also_treated_as_file_wikilinks() {
        let out = process_wikilinks("![[photo.png]]\n");
        assert!(out.contains("![[photo.png]]"), "got: {out}");
    }

    #[test]
    fn bracketed_link_text_is_still_a_commonmark_link() {
        for input in [
            "See [[1]](https://example.com) for details.\n",
            "See [[1#x]](https://example.com) for details.\n",
            "[[Notes#Setup]](https://example.com)\n",
        ] {
            assert_eq!(process_wikilinks(input), input, "should be literal");
        }
    }

    #[test]
    fn block_ids_become_anchors_and_keep_line_count() {
        let out = process_wikilinks("A paragraph ^para1\n\nNext\n^own-line\n");
        assert!(
            out.contains("<a id=\"para1\" class=\"block-id-anchor\"></a>"),
            "got: {out}"
        );
        assert!(out.contains("<a id=\"own-line\""), "got: {out}");
        assert_eq!(out.lines().count(), 4);
    }

    #[test]
    fn block_ids_are_not_read_in_math() {
        let display = "$$\na ^b\n$$\n\nText ^real\n";
        let out = process_wikilinks(display);
        assert!(out.starts_with("$$\na ^b\n$$"), "{out}");
        assert!(out.contains("<a id=\"real\""), "{out}");

        let inline = "Transposed: $A\n^T$ and more ^id\n";
        let out = process_wikilinks(inline);
        assert!(out.contains("$A\n^T$"), "{out}");
        assert!(out.contains("<a id=\"id\""), "{out}");

        // Dollar amounts are prose, so a block id after them is one.
        let prose = "Costs $5 and then $6 ^cost\n";
        assert!(process_wikilinks(prose).contains("<a id=\"cost\""));
    }

    #[test]
    fn math_ranges_follow_comrak() {
        let ranges = |text: &str| -> Vec<String> {
            math_ranges(text, &code_region_ranges(text))
                .into_iter()
                .map(|(a, b)| text[a..b].to_owned())
                .collect()
        };
        assert_eq!(ranges("a $x$ b $$y$$ c"), ["$x$", "$$y$$"]);
        assert_eq!(ranges("$5 and $6"), Vec::<String>::new());
        assert_eq!(ranges("$ x$ and $x $"), Vec::<String>::new());
        // The `$` after `x` is followed by a space, so it opens nothing.
        assert_eq!(ranges(r"\$x$ and $y$"), ["$y$"]);
        assert_eq!(
            ranges("$a\n\nb$"),
            Vec::<String>::new(),
            "not across paragraphs"
        );
        assert_eq!(ranges("`$a` $b$"), ["$b$"], "not in code");
        // Linear: a paragraph of unclosed dollars.
        let many = "$5 ".repeat(200_000);
        assert!(ranges(&many).is_empty());
    }

    #[test]
    fn block_ids_are_found_with_windows_and_old_mac_line_ends() {
        for source in [
            "A paragraph ^para1\r\n\r\nNext\r\n",
            "A paragraph ^para1\r\rNext\r",
        ] {
            let out = process_wikilinks(source);
            assert!(
                out.contains("A paragraph <a id=\"para1\" class=\"block-id-anchor\"></a>\r"),
                "got: {out:?}"
            );
        }
    }

    #[test]
    fn percent_decode_round_trips_what_the_encoders_write() {
        assert_eq!(
            percent_decode("My%20Notes%20%28v2%29.md"),
            "My Notes (v2).md"
        );
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%E4%B8%AD"), "中");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("a%2"), "a%2");
    }
}
