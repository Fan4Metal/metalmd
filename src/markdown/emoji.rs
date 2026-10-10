//! Finding the emoji in text that are shown as emoji (in colour) rather
//! than as text, following Unicode's emoji presentation rules (UTS #51):
//! a character with `Emoji_Presentation`, or any emoji character followed
//! by U+FE0F, with its skin tone, tags and zero-width-joined parts; flags;
//! keycaps. `©`, `✔` or `↔` alone stay text, as in a browser.

use std::ops::Range;
use unicode_properties::emoji::{EmojiStatus, UnicodeEmoji};

const TEXT_STYLE: char = '\u{FE0E}';
const EMOJI_STYLE: char = '\u{FE0F}';
const ZWJ: char = '\u{200D}';
const KEYCAP: char = '\u{20E3}';

/// The byte range of the first emoji in `text`.
pub fn find(text: &str) -> Option<Range<usize>> {
    // Every emoji has a character outside ASCII (a keycap's digit is just
    // before one), so the text is skipped byte by byte to one that can be.
    let bytes = text.as_bytes();
    let mut at = 0;
    while let Some(offset) = bytes[at..].iter().position(|&b| may_lead(b)) {
        let start = at + offset;
        let c = text[start..].chars().next()?;
        let keycap = start
            .checked_sub(1)
            .filter(|&k| matches!(bytes[k], b'0'..=b'9' | b'#' | b'*'));
        for from in keycap.into_iter().chain(may_start(c).then_some(start)) {
            if let Some(rest) = emoji_at(&text[from..]) {
                return Some(from..text.len() - rest.len());
            }
        }
        at = start + c.len_utf8();
    }
    None
}

/// Outside these no character is an emoji, so most letters of other
/// scripts skip the table.
fn may_start(c: char) -> bool {
    matches!(c, '0'..='9' | '#' | '*' | '©' | '®' | '\u{2000}'..='\u{32FF}' | '\u{1F000}'..)
}

/// The UTF-8 lead bytes of the characters outside ASCII that [`may_start`]
/// takes, and of U+FE0F after a keycap's digit.
fn may_lead(b: u8) -> bool {
    matches!(b, 0xC2 | 0xE2 | 0xE3 | 0xEF | 0xF0)
}

/// The text after the emoji `s` starts with.
fn emoji_at(s: &str) -> Option<&str> {
    let first = s.chars().next()?;
    let after_first = &s[first.len_utf8()..];
    if first.is_ascii() {
        let rest = after_first.strip_prefix(EMOJI_STYLE).unwrap_or(after_first);
        return rest.strip_prefix(KEYCAP);
    }
    if is_regional_indicator(first) {
        // Two make a flag; one alone is still an emoji.
        return Some(match after_first.chars().next() {
            Some(c) if is_regional_indicator(c) => &after_first[c.len_utf8()..],
            _ => after_first,
        });
    }
    let mut rest = element(s, false)?;
    while let Some(next) = rest.strip_prefix(ZWJ).and_then(|r| element(r, true)) {
        rest = next;
    }
    Some(rest)
}

/// The text after one emoji character at the start of `s` with what
/// belongs to it. After a zero-width joiner the presentation selector is
/// often left out.
fn element(s: &str, joined: bool) -> Option<&str> {
    let c = s.chars().next()?;
    if c.is_ascii() || !c.is_emoji_char() {
        return None;
    }
    let status = c.emoji_status();
    let mut rest = &s[c.len_utf8()..];
    if rest.starts_with(TEXT_STYLE) {
        return None;
    }
    let mut emoji = joined
        || matches!(
            status,
            EmojiStatus::EmojiPresentation
                | EmojiStatus::EmojiPresentationAndModifierBase
                | EmojiStatus::EmojiPresentationAndEmojiComponent
                | EmojiStatus::EmojiPresentationAndModifierAndEmojiComponent
        );
    if let Some(r) = rest.strip_prefix(EMOJI_STYLE) {
        rest = r;
        emoji = true;
    }
    let modifier_base = matches!(
        status,
        EmojiStatus::EmojiModifierBase | EmojiStatus::EmojiPresentationAndModifierBase
    );
    if modifier_base && let Some(m) = rest.chars().next().filter(|&m| is_skin_tone(m)) {
        rest = &rest[m.len_utf8()..];
        emoji = true;
    }
    // Tags make subdivision flags of a black flag.
    while let Some(tag) = rest.chars().next().filter(|&t| is_tag(t)) {
        rest = &rest[tag.len_utf8()..];
    }
    emoji.then_some(rest)
}

fn is_regional_indicator(c: char) -> bool {
    matches!(c, '\u{1F1E6}'..='\u{1F1FF}')
}

fn is_skin_tone(c: char) -> bool {
    matches!(c, '\u{1F3FB}'..='\u{1F3FF}')
}

fn is_tag(c: char) -> bool {
    matches!(c, '\u{E0020}'..='\u{E007F}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(text: &str) -> Vec<&str> {
        let mut found = Vec::new();
        let mut at = 0;
        while let Some(range) = find(&text[at..]) {
            found.push(&text[at + range.start..at + range.end]);
            at += range.end;
        }
        found
    }

    #[test]
    fn every_emoji_character_may_start_an_emoji() {
        let missed: Vec<char> = (0..=0x10FFFF)
            .filter_map(char::from_u32)
            .filter(|&c| {
                let lead = c.encode_utf8(&mut [0; 4]).as_bytes()[0];
                c.is_emoji_char() && !(may_start(c) && (c.is_ascii() || may_lead(lead)))
            })
            .collect();
        assert!(missed.is_empty(), "{missed:?}");
        for c in [EMOJI_STYLE, KEYCAP] {
            assert!(may_lead(c.encode_utf8(&mut [0; 4]).as_bytes()[0]));
        }
    }

    #[test]
    fn emoji_presentation_characters_are_emoji() {
        assert_eq!(all("done ✅ ok 😀!"), ["✅", "😀"]);
        assert_eq!(all("⌚⏰"), ["⌚", "⏰"]);
    }

    #[test]
    fn text_presentation_characters_need_the_selector() {
        assert!(all("© ® ™ ✔ ↔ ❤ 1 # x").is_empty());
        assert_eq!(all("❤️ ✔️"), ["❤️", "✔️"]);
        assert!(all("😀\u{FE0E}").is_empty());
        assert!(all("Привет, 中文 — «ok»").is_empty());
    }

    #[test]
    fn sequences_are_one_emoji() {
        assert_eq!(all("👋🏽"), ["👋🏽"]);
        assert_eq!(all("👩‍💻 x"), ["👩‍💻"]);
        assert_eq!(all("👨‍👩‍👧‍👦"), ["👨‍👩‍👧‍👦"]);
        assert_eq!(all("🏳️‍🌈"), ["🏳️‍🌈"]);
        assert_eq!(all("👩‍⚕"), ["👩‍⚕"]);
        assert_eq!(all("🇺🇦🇯🇵"), ["🇺🇦", "🇯🇵"]);
        assert_eq!(
            all("🏴\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}").len(),
            1
        );
    }

    #[test]
    fn keycaps_are_emoji_but_digits_are_not() {
        assert_eq!(all("1️⃣ 2⃣ 3 #️⃣ é"), ["1️⃣", "2⃣", "#️⃣"]);
    }

    #[test]
    fn a_joiner_without_an_emoji_after_it_is_left_out() {
        assert_eq!(all("😀\u{200D}a"), ["😀"]);
    }
}
