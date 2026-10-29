//! Obsidian's Properties: the front matter as keys and values. Only the
//! flat YAML Obsidian writes is read (`key: value`, lists as `- item`
//! lines or `[a, b]`, `|` and `>` block text, quotes, `#` comments); front
//! matter with anything else (nested maps, anchors) is `None`, and the
//! viewer shows it as YAML.

/// One property: its key and its values, one for a scalar, any number for
/// a list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property {
    pub key: String,
    pub values: Vec<String>,
    /// Written as a list, so its values are shown as chips.
    pub list: bool,
}

/// The properties of front matter `yaml` (without the `---` fences).
pub fn parse(yaml: &str) -> Option<Vec<Property>> {
    let lines: Vec<&str> = yaml.lines().collect();
    let mut properties = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end();
        i += 1;
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        // A key starts a line; anything indented here belongs to nothing.
        if line.starts_with([' ', '\t']) || line.starts_with("- ") || line == "-" {
            return None;
        }
        let (key, rest) = split_key(line)?;
        let rest = strip_comment(rest).trim();
        // The indented lines that follow.
        let start = i;
        while i < lines.len() && (lines[i].starts_with([' ', '\t']) || lines[i].trim().is_empty()) {
            i += 1;
        }
        let block: Vec<&str> = lines[start..i].iter().map(|l| l.trim()).collect();
        let block_lines = || block.iter().copied().filter(|l| !l.is_empty());
        let property = match rest {
            "" => {
                let items: Vec<&str> = block_lines().collect();
                if items.iter().all(|l| *l == "-" || l.starts_with("- ")) {
                    Property {
                        key,
                        values: items
                            .iter()
                            .map(|l| unquote(strip_comment(l.trim_start_matches('-')).trim()))
                            .filter(|v| !v.is_empty())
                            .collect(),
                        list: !items.is_empty(),
                    }
                } else {
                    // A nested map.
                    return None;
                }
            }
            _ if rest.starts_with(['|', '>']) => {
                let joiner = if rest.starts_with('|') { "\n" } else { " " };
                Property {
                    key,
                    values: vec![block_lines().collect::<Vec<_>>().join(joiner)],
                    list: false,
                }
            }
            _ if rest.starts_with(['&', '*', '!', '{']) => return None,
            _ if rest.starts_with('[') => {
                let inner = rest.strip_prefix('[')?.strip_suffix(']')?;
                if block_lines().next().is_some() {
                    return None;
                }
                Property {
                    key,
                    values: split_flow(inner)
                        .into_iter()
                        .map(|v| unquote(v.trim()))
                        .filter(|v| !v.is_empty())
                        .collect(),
                    list: true,
                }
            }
            _ => {
                // A plain scalar may go on over indented lines.
                let mut value = vec![rest];
                value.extend(block_lines());
                Property {
                    key,
                    values: vec![unquote(&value.join(" "))],
                    list: false,
                }
            }
        };
        properties.push(property);
    }
    Some(properties)
}

/// `key: rest` (the key may be quoted), or `None` if the line is no
/// key.
fn split_key(line: &str) -> Option<(String, &str)> {
    let (key, rest) = if let Some(quote) = line.chars().next().filter(|c| *c == '"' || *c == '\'') {
        let end = line[1..].find(quote)? + 1;
        let rest = line[end + 1..].strip_prefix(':')?;
        (line[1..end].to_owned(), rest)
    } else {
        // `key:` then a space or the end; `a:b` is no key.
        let at = line
            .match_indices(':')
            .map(|(at, _)| at)
            .find(|&at| line[at + 1..].is_empty() || line[at + 1..].starts_with([' ', '\t']))?;
        (line[..at].trim().to_owned(), &line[at + 1..])
    };
    if key.is_empty() || !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    Some((key, rest))
}

/// `text` without a ` #comment` at its end (outside quotes).
fn strip_comment(text: &str) -> &str {
    let mut quote = None;
    let mut previous = ' ';
    for (at, c) in text.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' && previous.is_whitespace() => return &text[..at],
            None => {}
        }
        previous = c;
    }
    text
}

/// The items of a flow sequence's inside, split at commas outside quotes.
fn split_flow(inner: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let mut quote = None;
    let mut start = 0;
    for (at, c) in inner.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == ',' => {
                items.push(&inner[start..at]);
                start = at + 1;
            }
            None => {}
        }
    }
    items.push(&inner[start..]);
    items
}

/// A scalar without its quotes; `''` in single quotes is one `'`, and
/// `\"` in double quotes one `"`.
fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 {
        if let Some(inner) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
            return inner.replace("\\\"", "\"").replace("\\\\", "\\");
        }
        if let Some(inner) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
            return inner.replace("''", "'");
        }
    }
    value.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(key: &str, values: &[&str], list: bool) -> Property {
        Property {
            key: key.to_owned(),
            values: values.iter().map(|v| (*v).to_owned()).collect(),
            list,
        }
    }

    #[test]
    fn obsidian_properties_are_read() {
        let yaml = "title: \"My note: draft\"\n\
                    tags:\n  - rust\n  - \"egui\"\n\
                    aliases: [one, 'two, three']\n\
                    created: 2024-03-01 # a date\n\
                    done: true\n\
                    related: \"[[Other note]]\"\n\
                    empty:\n\
                    url: https://example.com/a#b\n\
                    note: |\n  line one\n  line two\n\
                    # a comment\n\
                    \"quoted key\": it's\n";
        assert_eq!(
            parse(yaml),
            Some(vec![
                p("title", &["My note: draft"], false),
                p("tags", &["rust", "egui"], true),
                p("aliases", &["one", "two, three"], true),
                p("created", &["2024-03-01"], false),
                p("done", &["true"], false),
                p("related", &["[[Other note]]"], false),
                p("empty", &[], false),
                p("url", &["https://example.com/a#b"], false),
                p("note", &["line one\nline two"], false),
                p("quoted key", &["it's"], false),
            ])
        );
    }

    #[test]
    fn what_is_not_flat_is_none() {
        for yaml in [
            "author:\n  name: A\n  mail: b\n",
            "  indented: x\n",
            "- a\n- b\n",
            "just text\n",
            "anchor: &a x\n",
            "map: {a: 1}\n",
        ] {
            assert_eq!(parse(yaml), None, "{yaml:?}");
        }
        assert_eq!(parse(""), Some(vec![]));
    }
}
