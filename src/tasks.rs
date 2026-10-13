//! Ticking a task list item (`- [ ]` ↔ `- [x]`): the one change the viewer
//! makes to a document. The file is read again, the mark is checked to be
//! where the parse put it, and only that character is changed; the file is
//! written back in the encoding it was read in, or not at all.
//!
//! A tick is undone by ticking again, but only in the file as it was
//! written: changed by someone else since, its lines may have moved, and
//! the same place may hold another item's box.

use crate::fs_text::{self, UTF8_BOM_LABEL, UTF8_LABEL};
use crate::markdown::document::Task;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;

/// Ticks or clears `task` in the file at `path`, and gives the digest of
/// what it wrote (`None` if the task already was as wanted: a second click
/// before the reload). With `expect`, the file must still be the one of
/// that digest.
pub fn toggle(path: &Path, task: Task, expect: Option<u64>) -> Result<Option<u64>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let unchanged = || {
        tr!(
            "The file has changed since it was shown.",
            "Файл изменился после того, как был показан."
        )
        .to_owned()
    };
    if expect.is_some_and(|digest| digest != digest_of(&bytes)) {
        return Err(unchanged());
    }
    let decoded = fs_text::decode_text(&bytes);
    if decoded.lossy || encode(&decoded.content, &decoded.encoding).as_deref() != Some(&bytes) {
        return Err(tr!(
            "The file cannot be saved in its encoding without other changes.",
            "Файл нельзя сохранить в его кодировке без других изменений."
        )
        .to_owned());
    }
    let Some(text) = toggled(&decoded.content, task) else {
        return Err(unchanged());
    };
    if text == decoded.content {
        return Ok(None);
    }
    let out = encode(&text, &decoded.encoding).ok_or_else(unchanged)?;
    std::fs::write(path, &out).map_err(|e| e.to_string())?;
    Ok(Some(digest_of(&out)))
}

fn digest_of(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// `text` with the mark of `task` set to its other state, `text` itself if
/// it already is, or `None` if no task box is where `task` says.
fn toggled(text: &str, task: Task) -> Option<String> {
    let start = line_start(text, task.line)?;
    let at = start + task.column.checked_sub(1)?;
    let bytes = text.as_bytes();
    if at == 0 || bytes.get(at - 1) != Some(&b'[') || bytes.get(at + 1) != Some(&b']') {
        return None;
    }
    let checked = match bytes[at] {
        b' ' => false,
        b'x' | b'X' => true,
        _ => return None,
    };
    let mut out = text.to_owned();
    if checked == task.checked {
        out.replace_range(at..at + 1, if checked { " " } else { "x" });
    }
    Some(out)
}

/// The byte offset where line `line` (from 1) starts, lines ending as
/// comrak ends them: at `\n`, `\r\n` or a lone `\r`.
fn line_start(text: &str, line: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut current = 1;
    let mut i = 0;
    while current < line {
        match bytes.get(i)? {
            b'\n' => current += 1,
            b'\r' if bytes.get(i + 1) != Some(&b'\n') => current += 1,
            _ => {}
        }
        i += 1;
    }
    Some(i)
}

/// `text` in the encoding `fs_text::decode_text` named, `None` where a
/// character has no form in it.
fn encode(text: &str, label: &str) -> Option<Vec<u8>> {
    let utf16 = |big_endian: bool| {
        let mut out = Vec::with_capacity(2 + text.len() * 2);
        for unit in std::iter::once(0xFEFF).chain(text.encode_utf16()) {
            out.extend(if big_endian {
                unit.to_be_bytes()
            } else {
                unit.to_le_bytes()
            });
        }
        out
    };
    match label {
        UTF8_LABEL => Some(text.as_bytes().to_vec()),
        UTF8_BOM_LABEL => Some([b"\xEF\xBB\xBF", text.as_bytes()].concat()),
        "UTF-16LE" => Some(utf16(false)),
        "UTF-16BE" => Some(utf16(true)),
        _ => {
            let encoding = encoding_rs::Encoding::for_label(label.as_bytes())?;
            let (bytes, _, unmappable) = encoding.encode(text);
            (!unmappable).then(|| bytes.into_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::document::{Block, Document};

    fn tasks(source: &str) -> Vec<Task> {
        fn walk(blocks: &[Block], out: &mut Vec<Task>) {
            for block in blocks {
                match block {
                    Block::List { items, .. } => {
                        for item in items {
                            out.extend(item.task);
                            walk(&item.blocks, out);
                        }
                    }
                    Block::Quote(blocks) | Block::Alert { blocks, .. } => walk(blocks, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&Document::parse(source).blocks, &mut out);
        out
    }

    /// Every task in `source` toggled once, each on its own.
    fn each_toggled(source: &str) -> Vec<String> {
        tasks(source)
            .into_iter()
            .map(|task| toggled(source, task).expect("the box is where the parse put it"))
            .collect()
    }

    #[test]
    fn ticks_and_clears() {
        let source = "- [ ] one\n- [x] two\n- [X] three\n";
        assert_eq!(
            each_toggled(source),
            [
                "- [x] one\n- [x] two\n- [X] three\n",
                "- [ ] one\n- [ ] two\n- [X] three\n",
                "- [ ] one\n- [x] two\n- [ ] three\n",
            ]
        );
    }

    #[test]
    fn finds_the_box_after_front_matter_quotes_tabs_and_crlf() {
        let source = concat!(
            "---\r\ntitle: [x]\r\n---\r\n\r\n",
            "Text with [[#Heading|a wikilink]] and ![[pic.png|20x20]].\r\n\r\n",
            "> 1. [ ] quoted ^block\r\n",
            ">    - [x] nested\r\n\r\n",
            "-\t[ ] tab\r\n",
            "* [ ] after \u{2063}markers\r\n",
        );
        let all = each_toggled(source);
        assert_eq!(all.len(), 4);
        assert!(all[0].contains("> 1. [x] quoted ^block\r\n"));
        assert!(all[1].contains(">    - [ ] nested\r\n"));
        assert!(all[2].contains("-\t[x] tab\r\n"));
        assert!(all[3].ends_with("* [x] after \u{2063}markers\r\n"));
        for text in all {
            assert_eq!(text.len(), source.len());
        }
    }

    #[test]
    fn a_box_already_as_wanted_or_gone_is_left_alone() {
        let task = tasks("- [ ] one\n")[0];
        assert_eq!(toggled("- [x] one\n", task).as_deref(), Some("- [x] one\n"));
        assert_eq!(toggled("- one\n", task), None);
        assert_eq!(toggled("", task), None);
    }

    #[test]
    fn writes_in_the_encoding_it_was_read_in() {
        let dir = std::env::temp_dir().join(format!("metalmd-tasks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = "- [ ] Задача\n";
        let cases: [(&str, Vec<u8>); 4] = [
            ("utf8.md", text.as_bytes().to_vec()),
            ("bom.md", encode(text, UTF8_BOM_LABEL).unwrap()),
            ("utf16.md", encode(text, "UTF-16LE").unwrap()),
            ("1251.md", encode(text, "windows-1251").unwrap()),
        ];
        let task = tasks(text)[0];
        for (name, bytes) in cases {
            let path = dir.join(name);
            std::fs::write(&path, &bytes).unwrap();
            toggle(&path, task, None).unwrap();
            let after = fs_text::decode_text(&std::fs::read(&path).unwrap());
            assert_eq!(after.content, "- [x] Задача\n", "{name}");
            assert_eq!(
                std::fs::read(&path).unwrap().len(),
                bytes.len(),
                "{name}: only the mark changes"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undoes_only_in_the_file_it_wrote() {
        let dir = std::env::temp_dir().join(format!("metalmd-undo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("undo.md");
        let text = "- [ ] one
- [ ] two
";
        let [one, two] = tasks(text)[..] else {
            panic!()
        };
        std::fs::write(&path, text).unwrap();
        let after_one = toggle(&path, one, None).unwrap().unwrap();
        let after_two = toggle(&path, two, None).unwrap().unwrap();
        let back = |task: Task| Task {
            checked: !task.checked,
            ..task
        };
        // Undone in the opposite order, each in the file the tick left.
        assert_eq!(
            toggle(&path, back(two), Some(after_two)).unwrap(),
            Some(after_one)
        );
        toggle(&path, back(one), Some(after_one)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        // Edited since: a line above moves the boxes.
        let after = toggle(&path, one, None).unwrap().unwrap();
        std::fs::write(
            &path,
            "intro
- [x] one
- [ ] two
",
        )
        .unwrap();
        assert!(toggle(&path, back(one), Some(after)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
