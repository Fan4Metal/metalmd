//! A tiny HTML tokenizer for the raw HTML that shows up in Markdown files.
//!
//! READMEs use a handful of tags (`<img>`, `<a>`, `<br>`, `<p align>`,
//! `<details>`, `<kbd>`, `<sub>`) to lay out logos and badges. A full HTML
//! engine is out of scope for a fast native viewer, so this only splits the
//! input into tags and text; `document.rs` decides what each tag means and
//! drops everything it does not understand.

use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq)]
pub enum Token<'a> {
    /// `<name attr=…>` or `<name … />`.
    Open {
        name: String,
        attrs: &'a str,
        self_closing: bool,
    },
    /// `</name>`.
    Close { name: String },
    /// Text between tags, entities still encoded.
    Text(&'a str),
}

/// Splits `input` into tags and text. Comments, doctypes and processing
/// instructions are dropped. A `<` that does not start a tag is text.
pub fn tokenize(input: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let bytes = input.as_bytes();
    let mut text_start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &input[i..];
        // Comments and declarations.
        let skip_to = if rest.starts_with("<!--") {
            Some(rest.find("-->").map_or(input.len(), |end| i + end + 3))
        } else if rest.starts_with("<!") || rest.starts_with("<?") {
            Some(rest.find('>').map_or(input.len(), |end| i + end + 1))
        } else {
            None
        };
        if let Some(end) = skip_to {
            if text_start < i {
                tokens.push(Token::Text(&input[text_start..i]));
            }
            i = end;
            text_start = end;
            continue;
        }

        let closing = rest.starts_with("</");
        let name_start = if closing { 2 } else { 1 };
        let name_len = rest[name_start..]
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-')
            .count();
        let starts_with_letter = rest[name_start..]
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic());
        let Some(gt) = find_tag_end(rest) else {
            i += 1;
            continue;
        };
        if name_len == 0 || !starts_with_letter {
            i += 1;
            continue;
        }
        if text_start < i {
            tokens.push(Token::Text(&input[text_start..i]));
        }
        let name = rest[name_start..name_start + name_len].to_ascii_lowercase();
        if closing {
            tokens.push(Token::Close { name });
        } else {
            let inner = &rest[name_start + name_len..gt];
            let self_closing = inner.trim_end().ends_with('/');
            let attrs = inner.trim_end().trim_end_matches('/');
            tokens.push(Token::Open {
                name,
                attrs,
                self_closing,
            });
        }
        i += gt + 1;
        text_start = i;
    }
    if text_start < input.len() {
        tokens.push(Token::Text(&input[text_start..]));
    }
    tokens
}

/// Index of the `>` that ends the tag starting at `tag[0] == '<'`, skipping
/// `>` inside quoted attribute values.
fn find_tag_end(tag: &str) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (i, b) in tag.bytes().enumerate().skip(1) {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'>' => return Some(i),
                b'<' => return None,
                _ => {}
            },
        }
    }
    None
}

/// The value of attribute `name` in a tag's attribute string.
pub fn attr<'a>(attrs: &'a str, name: &str) -> Option<Cow<'a, str>> {
    let bytes = attrs.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' {
            i += 1;
        }
        let key = &attrs[key_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value: Option<&str> = None;
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let q = bytes[i];
                let v_start = i + 1;
                let v_end = attrs[v_start..]
                    .bytes()
                    .position(|b| b == q)
                    .map_or(attrs.len(), |p| v_start + p);
                value = Some(&attrs[v_start..v_end]);
                i = (v_end + 1).min(attrs.len());
            } else {
                let v_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = Some(&attrs[v_start..i]);
            }
        }
        if key.eq_ignore_ascii_case(name) {
            return Some(decode_entities(value.unwrap_or("")));
        }
        if key.is_empty() && i == key_start {
            i += 1;
        }
    }
    None
}

/// Decodes the common named entities and numeric character references.
pub fn decode_entities(text: &str) -> Cow<'_, str> {
    if !text.contains('&') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        // The `;` within the longest name; searching the whole rest for it
        // made text with many bare `&` quadratic.
        let end = after.bytes().take(11).position(|b| b == b';');
        let decoded = end.and_then(|e| {
            let entity = &after[..e];
            let ch = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                "copy" => Some('©'),
                "reg" => Some('®'),
                "trade" => Some('™'),
                "mdash" => Some('—'),
                "ndash" => Some('–'),
                "hellip" => Some('…'),
                "middot" => Some('·'),
                "bull" => Some('•'),
                "laquo" => Some('«'),
                "raquo" => Some('»'),
                "larr" => Some('←'),
                "rarr" => Some('→'),
                "uarr" => Some('↑'),
                "darr" => Some('↓'),
                "times" => Some('×'),
                "check" => Some('✓'),
                _ => entity.strip_prefix('#').and_then(|num| {
                    let code = match num.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => num.parse().ok(),
                    };
                    code.and_then(char::from_u32)
                }),
            };
            ch.map(|c| (c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &after[e + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_tags_text_and_skips_comments() {
        let tokens =
            tokenize("<p align=\"center\">Hi <!-- note --><img src='a.png' alt=\"x > y\"/></p>");
        assert_eq!(
            tokens,
            vec![
                Token::Open {
                    name: "p".into(),
                    attrs: " align=\"center\"",
                    self_closing: false
                },
                Token::Text("Hi "),
                Token::Open {
                    name: "img".into(),
                    attrs: " src='a.png' alt=\"x > y\"",
                    self_closing: true
                },
                Token::Close { name: "p".into() },
            ]
        );
    }

    #[test]
    fn a_lone_angle_bracket_is_text() {
        let tokens = tokenize("a < b and 1<2");
        assert_eq!(tokens, vec![Token::Text("a < b and 1<2")]);
    }

    #[test]
    fn reads_attributes_in_every_quoting_style() {
        let attrs = " src=\"a b.png\" width=100 alt='it&amp;s' hidden";
        assert_eq!(attr(attrs, "src").as_deref(), Some("a b.png"));
        assert_eq!(attr(attrs, "width").as_deref(), Some("100"));
        assert_eq!(attr(attrs, "ALT").as_deref(), Some("it&s"));
        assert_eq!(attr(attrs, "hidden").as_deref(), Some(""));
        assert_eq!(attr(attrs, "height"), None);
    }

    #[test]
    fn decodes_entities() {
        assert_eq!(
            decode_entities("a &amp; b &lt;c&gt; &#169; &#x41; &bogus; &"),
            "a & b <c> © A &bogus; &"
        );
        // A `;` too far away to end a name leaves the `&` as it is.
        assert_eq!(decode_entities("&amp x; y;"), "&amp x; y;");
        assert_eq!(decode_entities("Привет &laquo;мир&raquo;"), "Привет «мир»");
    }

    #[test]
    fn many_bare_ampersands_take_linear_time() {
        // Searching the rest of the text for a `;` took seconds on this.
        let text = "a & ".repeat(400_000);
        assert_eq!(decode_entities(&text), text);
    }
}
