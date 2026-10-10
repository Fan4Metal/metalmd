//! Find in document (Ctrl+F).

use crate::markdown::document::Document;
use std::ops::Range;

/// One occurrence of the query.
#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    /// The `Rich` / code block id the occurrence is in.
    pub id: u32,
    /// Top-level block to scroll to.
    pub block: usize,
    /// Byte range in that text.
    pub range: Range<usize>,
}

#[derive(Default)]
pub struct FindState {
    pub open: bool,
    pub query: String,
    pub matches: Vec<Match>,
    pub current: usize,
    pub request_focus: bool,
    /// What `matches` was computed for: (query, document pointer).
    computed_for: Option<(String, usize)>,
}

impl FindState {
    pub fn open(&mut self) {
        self.open = true;
        self.request_focus = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.matches.clear();
        self.computed_for = None;
    }

    /// Recomputes matches when the query or the document changed. Returns
    /// true when they were recomputed.
    pub fn refresh(&mut self, doc: &Document) -> bool {
        let key = (self.query.clone(), doc as *const Document as usize);
        if self.computed_for.as_ref() == Some(&key) {
            return false;
        }
        self.matches = find_all(doc, &self.query);
        self.current = self.current.min(self.matches.len().saturating_sub(1));
        self.computed_for = Some(key);
        true
    }

    pub fn invalidate(&mut self) {
        self.computed_for = None;
    }

    pub fn current_match(&self) -> Option<&Match> {
        self.matches.get(self.current)
    }

    pub fn step(&mut self, forward: bool) {
        let n = self.matches.len();
        if n == 0 {
            return;
        }
        self.current = if forward {
            (self.current + 1) % n
        } else {
            (self.current + n - 1) % n
        };
    }

    /// Matches inside text `id`, with whether each is the current one.
    pub fn matches_in(&self, id: u32) -> (&[Match], Option<usize>) {
        if !self.open || self.matches.is_empty() {
            return (&[], None);
        }
        let lo = self.matches.partition_point(|m| m.id < id);
        let hi = self.matches.partition_point(|m| m.id <= id);
        let current = (lo..hi).contains(&self.current).then(|| self.current - lo);
        (&self.matches[lo..hi], current)
    }
}

/// Case-insensitive occurrences of `query` in the document, in document
/// order.
pub fn find_all(doc: &Document, query: &str) -> Vec<Match> {
    let mut out = Vec::new();
    if query.is_empty() {
        return out;
    }
    for entry in &doc.search {
        for range in find_in(&entry.text, query) {
            out.push(Match {
                id: entry.id,
                block: entry.block,
                range,
            });
        }
    }
    out
}

/// Case-insensitive, non-overlapping occurrences of `needle` in `hay`, as
/// byte ranges of `hay`.
pub fn find_in(hay: &str, needle: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    if needle.is_empty() {
        return out;
    }
    if needle.is_ascii() {
        // ASCII bytes never occur inside a multi-byte UTF-8 sequence, so a
        // byte-wise comparison yields valid char boundaries.
        let h = hay.as_bytes();
        let n = needle.as_bytes();
        let mut i = 0;
        while i + n.len() <= h.len() {
            if h[i..i + n.len()].eq_ignore_ascii_case(n) {
                out.push(i..i + n.len());
                i += n.len();
            } else {
                i += 1;
            }
        }
        return out;
    }
    let folded: Vec<(char, usize)> = hay
        .char_indices()
        .flat_map(|(i, c)| c.to_lowercase().map(move |l| (l, i)))
        .collect();
    let pattern: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    let mut i = 0;
    while i + pattern.len() <= folded.len() {
        if folded[i..i + pattern.len()]
            .iter()
            .zip(&pattern)
            .all(|((c, _), p)| c == p)
        {
            let start = folded[i].1;
            let last = folded[i + pattern.len() - 1].1;
            let end = last + hay[last..].chars().next().map_or(0, char::len_utf8);
            out.push(start..end);
            i += pattern.len();
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_search_is_case_insensitive() {
        assert_eq!(find_in("Foo foo FOO", "foo"), vec![0..3, 4..7, 8..11]);
        assert_eq!(find_in("aaaa", "aa"), vec![0..2, 2..4]);
        assert!(find_in("abc", "").is_empty());
    }

    #[test]
    fn unicode_search_keeps_byte_offsets() {
        let hay = "Привет, МИР и мир";
        let found = find_in(hay, "мир");
        assert_eq!(found.len(), 2);
        for range in found {
            assert_eq!(hay[range].to_lowercase(), "мир");
        }
    }

    #[test]
    fn document_matches_are_in_order_and_grouped_by_text() {
        let doc = Document::parse("# Alpha\n\nalpha beta\n\n```\nALPHA\n```\n");
        let mut state = FindState::default();
        state.open();
        state.query = "alpha".into();
        state.refresh(&doc);
        assert_eq!(state.matches.len(), 3);
        assert!(state.matches.windows(2).all(|w| w[0].id <= w[1].id));
        assert_eq!(state.matches[2].block, 2);
        let (in_heading, current) = state.matches_in(state.matches[0].id);
        assert_eq!(in_heading.len(), 1);
        assert_eq!(current, Some(0));
        state.step(false);
        assert_eq!(state.current, 2);
    }
}
