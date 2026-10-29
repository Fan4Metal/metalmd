//! Obsidian note embeds: `![[Note]]`, `![[Note#Heading]]`, `![[Note#^id]]`
//! alone in a paragraph are `Block::Embed`s, which [`fill`] fills with the
//! note's blocks (the whole note without its properties, the section under
//! the heading, or the block with the id) on the loader thread.
//!
//! The note's blocks keep the document's rules: their ids are moved past
//! the document's (`Document::ids`), so selection and find tell them apart;
//! their text is found by find, at the embed's top-level block; links and
//! images relative to the note are made absolute, since they are drawn
//! from the document's folder; and their task boxes cannot be ticked
//! (`Task::line` 0), since the document's file is the one written.
//! Selection maps back only to the document's own text, so "Copy as
//! Markdown" and highlighting refuse what is embedded.

use std::path::{Path, PathBuf};

use super::document::{Block, Document, Inline, SearchText};
use super::preprocess::percent_decode;

/// Embeds within embeds go this deep.
const MAX_DEPTH: usize = 4;
/// At most this many notes are read for one document.
const MAX_EMBEDS: usize = 64;

/// Reads and fills the note embeds of `doc`, the document of the file
/// `path`.
pub fn fill(doc: &mut Document, path: &Path) {
    let mut budget = MAX_EMBEDS;
    fill_in(doc, path, &mut vec![path.to_path_buf()], &mut budget);
}

/// Whether `doc` has note embeds for [`fill`] to fill.
pub fn has_embeds(doc: &Document) -> bool {
    doc.blocks.iter().any(has_embed)
}

fn fill_in(doc: &mut Document, path: &Path, chain: &mut Vec<PathBuf>, budget: &mut usize) {
    if !has_embeds(doc) {
        return;
    }
    let base = path.parent().unwrap_or(Path::new("")).to_path_buf();
    let mut next = Next {
        ids: doc.ids,
        folds: doc.folds,
        search: Vec::new(),
        images: Vec::new(),
    };
    for (top, block) in doc.blocks.iter_mut().enumerate() {
        embeds_mut(std::slice::from_mut(block), &mut |src, content| {
            *content = Some(load(src, &base, chain, budget, top, &mut next));
        });
    }
    doc.ids = next.ids;
    doc.folds = next.folds;
    doc.search.extend(next.search);
    // In document order: the embedded text where its block is.
    doc.search.sort_by_key(|s| s.block);
    for image in next.images {
        if !doc.images.contains(&image) {
            doc.images.push(image);
        }
    }
}

/// What the embeds of a document add to it.
struct Next {
    ids: u32,
    folds: u32,
    search: Vec<SearchText>,
    images: Vec<String>,
}

/// How far an embedded note's numbers move: past the document's, so the
/// viewer tells its text and its folds from the document's.
#[derive(Clone, Copy)]
struct Offset {
    ids: u32,
    folds: u32,
}

fn has_embed(block: &Block) -> bool {
    match block {
        Block::Embed { .. } => true,
        Block::Quote(blocks) | Block::Alert { blocks, .. } | Block::Footnote { blocks, .. } => {
            blocks.iter().any(has_embed)
        }
        Block::List { items, .. } => items.iter().any(|i| i.blocks.iter().any(has_embed)),
        Block::DefinitionList(items) => items
            .iter()
            .any(|(terms, details)| terms.iter().chain(details.iter().flatten()).any(has_embed)),
        _ => false,
    }
}

type Content = Option<Result<Vec<Block>, String>>;

/// Calls `f` with each embed in `blocks` not filled yet, nested ones too
/// (in quotes, callouts, lists), but not inside another embed.
fn embeds_mut(blocks: &mut [Block], f: &mut dyn FnMut(&str, &mut Content)) {
    for block in blocks {
        match block {
            Block::Embed { src, content, .. } if content.is_none() => f(src, content),
            Block::Quote(blocks) | Block::Alert { blocks, .. } | Block::Footnote { blocks, .. } => {
                embeds_mut(blocks, f)
            }
            Block::List { items, .. } => {
                for item in items {
                    embeds_mut(&mut item.blocks, f);
                }
            }
            Block::DefinitionList(items) => {
                for (terms, details) in items {
                    embeds_mut(terms, f);
                    for detail in details {
                        embeds_mut(detail, f);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The blocks `src` (relative to `base`, `#anchor` for a part) names, or
/// why there are none.
fn load(
    src: &str,
    base: &Path,
    chain: &mut Vec<PathBuf>,
    budget: &mut usize,
    top: usize,
    next: &mut Next,
) -> Result<Vec<Block>, String> {
    let (file, anchor) = match src.split_once('#') {
        Some((file, anchor)) => (file, Some(anchor)),
        None => (src, None),
    };
    let decoded = percent_decode(file);
    let here = base.join(&decoded);
    if crate::ui::confirm::on_another_host(&here, Some(base)) {
        return Err(tr!(
            "The note is on another computer",
            "Заметка на другом компьютере"
        )
        .to_owned());
    }
    let file = if here.is_file() {
        here
    } else {
        crate::vault::locate_now(base, &decoded)
            .ok_or_else(|| tr!("The note is not found", "Заметка не найдена").to_owned())?
    };
    if chain.contains(&file) {
        return Err(tr!("The note embeds itself", "Заметка вставляет сама себя").to_owned());
    }
    if chain.len() > MAX_DEPTH || *budget == 0 {
        return Err(tr!(
            "Too many embedded notes",
            "Слишком много вставленных заметок"
        )
        .to_owned());
    }
    *budget -= 1;
    let text = crate::fs_text::read_to_string_lossy(&file).map_err(|e| e.to_string())?;
    let mut note = Document::parse(&text.content);
    chain.push(file.clone());
    fill_in(&mut note, &file, chain, budget);
    chain.pop();

    let range = match anchor {
        None => 0..note.blocks.len(),
        Some(anchor) => section(&note, anchor).ok_or_else(|| {
            tr!("The note has no such part", "В заметке нет такой части").to_owned()
        })?,
    };
    // The properties are not shown, as in Obsidian.
    let kept: Vec<usize> = range
        .filter(|&i| !matches!(note.blocks[i], Block::FrontMatter { .. }))
        .collect();
    let offset = Offset {
        ids: next.ids,
        folds: next.folds,
    };
    next.ids += note.ids;
    next.folds += note.folds;
    next.search.extend(
        note.search
            .iter()
            .filter(|s| kept.contains(&s.block))
            .map(|s| SearchText {
                id: s.id + offset.ids,
                block: top,
                text: s.text.clone(),
            }),
    );
    let mut blocks: Vec<Block> = kept.iter().map(|&i| note.blocks[i].clone()).collect();
    rebase(&mut blocks, offset, &file, &mut next.images);
    Ok(blocks)
}

/// The top-level blocks `anchor` names in `note`: a heading and what
/// follows it up to the next heading of its level or above, or the one
/// block that holds a block id.
fn section(note: &Document, anchor: &str) -> Option<std::ops::Range<usize>> {
    let start = *note.anchors.get(anchor)?;
    let Block::Heading { level, .. } = note.blocks[start] else {
        return Some(start..start + 1);
    };
    let end = note.blocks[start + 1..]
        .iter()
        .position(|b| matches!(b, Block::Heading { level: l, .. } if *l <= level))
        .map_or(note.blocks.len(), |i| start + 1 + i);
    Some(start..end)
}

/// Moves the ids and fold numbers of `blocks` (from the note `file`) by
/// `offset`, makes their links and images absolute (adding the images to
/// `images`) and their task boxes read-only.
fn rebase(blocks: &mut [Block], offset: Offset, file: &Path, images: &mut Vec<String>) {
    let inlines = |inlines: &mut [Inline], images: &mut Vec<String>| {
        for inline in inlines {
            match inline {
                Inline::Text(rich) => {
                    rich.id += offset.ids;
                    for link in &mut rich.links {
                        *link = absolute(link, file);
                    }
                }
                Inline::Image(image) => {
                    image.src = absolute(&image.src, file);
                    if let Some(link) = &mut image.link {
                        *link = absolute(link, file);
                    }
                    if !images.contains(&image.src) {
                        images.push(image.src.clone());
                    }
                }
            }
        }
    };
    for block in blocks {
        match block {
            Block::Heading { inlines: i, .. } | Block::Paragraph { inlines: i, .. } => {
                inlines(i, images)
            }
            Block::Details { index, summary, .. } => {
                *index += offset.folds;
                inlines(summary, images)
            }
            Block::Code { id, .. } | Block::Math { id, .. } | Block::FrontMatter { id, .. } => {
                *id += offset.ids
            }
            Block::Alert { fold, blocks, .. } => {
                if let Some(fold) = fold {
                    fold.index += offset.folds;
                }
                rebase(blocks, offset, file, images)
            }
            Block::Quote(blocks) | Block::Footnote { blocks, .. } => {
                rebase(blocks, offset, file, images)
            }
            Block::List { items, .. } => {
                for item in items {
                    if let Some(task) = &mut item.task {
                        task.line = 0;
                    }
                    rebase(&mut item.blocks, offset, file, images);
                }
            }
            Block::Table(table) => {
                for cell in table
                    .header
                    .iter_mut()
                    .chain(table.rows.iter_mut().flatten())
                {
                    inlines(cell, images);
                }
            }
            Block::DefinitionList(items) => {
                for (terms, details) in items {
                    rebase(terms, offset, file, images);
                    for detail in details {
                        rebase(detail, offset, file, images);
                    }
                }
            }
            Block::Embed { src, content, .. } => {
                *src = absolute(src, file);
                if let Some(Ok(blocks)) = content {
                    rebase(blocks, offset, file, images);
                }
            }
            Block::Gallery { images: list, .. } => {
                for image in list {
                    image.src = absolute(&image.src, file);
                    if !images.contains(&image.src) {
                        images.push(image.src.clone());
                    }
                }
            }
            Block::Rule | Block::DetailsEnd => {}
        }
    }
}

/// `target`, a link or image source in the note `file`, as one that means
/// the same from anywhere: a path relative to the note made absolute, an
/// anchor alone made the note's.
fn absolute(target: &str, file: &Path) -> String {
    let target = target.trim();
    if let Some(anchor) = target.strip_prefix('#') {
        return format!("{}#{anchor}", encode(file));
    }
    if crate::app::has_url_scheme(target) {
        return target.to_owned();
    }
    let (path, rest) = match target.find(['#', '?']) {
        Some(at) => target.split_at(at),
        None => (target, ""),
    };
    let decoded = percent_decode(path);
    if decoded.is_empty() || Path::new(&decoded).is_absolute() {
        return target.to_owned();
    }
    let folder = file.parent().unwrap_or(Path::new(""));
    format!("{}{rest}", encode(&folder.join(decoded)))
}

/// A path as a link destination that decodes back to it.
fn encode(path: &Path) -> String {
    path.to_string_lossy()
        .replace('%', "%25")
        .replace('#', "%23")
        .replace('?', "%3F")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("metalmd-embed-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".obsidian")).unwrap();
        for (file, text) in files {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        root
    }

    fn texts(blocks: &[Block]) -> Vec<String> {
        let mut out = Vec::new();
        for block in blocks {
            match block {
                Block::Heading { inlines, .. } | Block::Paragraph { inlines, .. } => {
                    for inline in inlines {
                        if let Inline::Text(rich) = inline {
                            out.push(rich.text.clone());
                        }
                    }
                }
                Block::List { items, .. } => {
                    for item in items {
                        out.extend(texts(&item.blocks));
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn content(doc: &Document, index: usize) -> &Result<Vec<Block>, String> {
        match &doc.blocks[index] {
            Block::Embed {
                content: Some(content),
                ..
            } => content,
            other => panic!("not a filled embed: {other:?}"),
        }
    }

    #[test]
    fn notes_parts_and_blocks_are_embedded() {
        let root = vault(
            "parts",
            &[
                ("a.md", "# A\n\n![[b]]\n\n![[b#Two]]\n\n![[b#^tail]]\n"),
                (
                    "other/b.md",
                    "---\ntags: x\n---\n# One\n\nfirst\n\n## Two\n\nsecond [here](#one) ![p](p.png)\n\n- [ ] task\n\n# Three\n\nlast ^tail\n",
                ),
            ],
        );
        let path = root.join("a.md");
        let mut doc = Document::parse(&std::fs::read_to_string(&path).unwrap());
        let own = doc.ids;
        fill(&mut doc, &path);

        let whole = content(&doc, 1).as_ref().unwrap();
        assert!(!whole.iter().any(|b| matches!(b, Block::FrontMatter { .. })));
        assert_eq!(
            texts(whole),
            [
                "One",
                "first",
                "Two",
                "second here ",
                "task",
                "Three",
                "last "
            ]
        );
        let two = content(&doc, 2).as_ref().unwrap();
        assert_eq!(texts(two), ["Two", "second here ", "task"]);
        let tail = content(&doc, 3).as_ref().unwrap();
        assert_eq!(texts(tail), ["last "]);

        // Ids past the document's, found by find at the embed's block.
        assert!(doc.ids > own);
        let found: Vec<_> = doc.search.iter().filter(|s| s.text == "last ").collect();
        assert_eq!(found.len(), 2);
        assert!(
            found
                .iter()
                .all(|s| s.id >= own && [1, 3].contains(&s.block))
        );
        let mut ids: Vec<u32> = doc.search.iter().map(|s| s.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), doc.search.len(), "ids are unique");

        // Links and images made absolute, tasks read-only.
        let Block::Paragraph { inlines, .. } = &two[1] else {
            panic!()
        };
        let Inline::Text(rich) = &inlines[0] else {
            panic!()
        };
        let b = root.join("other").join("b.md");
        assert_eq!(rich.links[0], format!("{}#one", b.to_string_lossy()));
        let image = inlines.iter().find_map(|i| match i {
            Inline::Image(image) => Some(image),
            _ => None,
        });
        assert_eq!(
            image.unwrap().src,
            root.join("other").join("p.png").to_string_lossy()
        );
        let Block::List { items, .. } = &two[2] else {
            panic!()
        };
        assert_eq!(items[0].task.map(|t| t.line), Some(0));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_notes_cycles_and_parts_are_errors() {
        let root = vault(
            "errors",
            &[
                (
                    "a.md",
                    "![[gone]]\n\n![[a]]\n\n![[b#Nowhere]]\n\n![[b]]\n\n![[//evil/share/n]]\n",
                ),
                ("b.md", "# B\n\n![[a]]\n"),
            ],
        );
        let path = root.join("a.md");
        let mut doc = Document::parse(&std::fs::read_to_string(&path).unwrap());
        fill(&mut doc, &path);
        assert!(content(&doc, 0).is_err());
        assert!(content(&doc, 1).is_err());
        assert!(content(&doc, 2).is_err());
        // b embeds a, which is already being shown.
        let b = content(&doc, 3).as_ref().unwrap();
        let Block::Embed {
            content: Some(inner),
            ..
        } = &b[1]
        else {
            panic!("{b:?}")
        };
        assert!(inner.is_err());
        // Never read from another computer: Windows would sign in to it.
        assert_eq!(
            content(&doc, 4).as_ref().unwrap_err(),
            tr!(
                "The note is on another computer",
                "Заметка на другом компьютере"
            )
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn embedded_folds_are_numbered_after_the_documents() {
        let root = vault(
            "folds",
            &[
                ("a.md", "> [!note]- A\n> a\n\n![[b]]\n\n> [!tip]+ C\n> c\n"),
                (
                    "b.md",
                    "> [!tip]- B\n> b\n\n<details>\n<summary>S</summary>\n\nx\n\n</details>\n",
                ),
            ],
        );
        let path = root.join("a.md");
        let mut doc = Document::parse(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(doc.folds, 2);
        fill(&mut doc, &path);
        let fold = |block: &Block| match block {
            Block::Alert { fold: Some(f), .. } => f.index,
            Block::Details { index, .. } => *index,
            other => panic!("{other:?}"),
        };
        assert_eq!(fold(&doc.blocks[0]), 0);
        assert_eq!(fold(&doc.blocks[2]), 1);
        let b = content(&doc, 1).as_ref().unwrap();
        assert_eq!(fold(&b[0]), 2);
        assert_eq!(fold(&b[1]), 3);
        assert_eq!(doc.folds, 4);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn embeds_among_text_are_links() {
        let doc = Document::parse("see ![[b]] here\n\n![[b]]\n![[c#X]]\n");
        let Block::Paragraph { inlines, .. } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks[0])
        };
        let Inline::Text(rich) = &inlines[0] else {
            panic!()
        };
        assert_eq!(rich.text, "see b here");
        assert_eq!(rich.links, ["b.md"]);
        assert!(
            matches!(&doc.blocks[1], Block::Embed { title, src, .. } if title == "b" && src == "b.md")
        );
        assert!(
            matches!(&doc.blocks[2], Block::Embed { title, src, .. } if title == "c > X" && src == "c.md#x")
        );
    }
}
