//! Copying text with pictures in it. A formula or emoji is a no-break space
//! in its layout job (see `viewer::rich_job`), and egui's selection copies
//! the job's text. So each picture's space comes after a marker naming its
//! source: U+2063 and the source's number in binary, U+2062 for 0 and
//! U+2064 for 1, all of which egui draws as nothing, with no width. Where
//! the pointer meets a picture, the cursor lands before its marker or after
//! its space, so a selection takes all of it or none. [`CopyPlugin`] puts
//! the sources back into what is copied.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};

const START: char = '\u{2063}';
const ZERO: char = '\u{2062}';
const ONE: char = '\u{2064}';
const SPACE: char = '\u{a0}';

/// Every source given a marker, by number.
#[derive(Default)]
struct Sources {
    ids: HashMap<String, usize>,
    list: Vec<String>,
}

static SOURCES: LazyLock<Mutex<Sources>> = LazyLock::new(Mutex::default);

fn sources() -> MutexGuard<'static, Sources> {
    SOURCES.lock().unwrap_or_else(|e| e.into_inner())
}

/// The space that stands for a picture copied as `source`, with its marker.
pub fn placeholder(source: &str) -> String {
    let mut sources = sources();
    let id = match sources.ids.get(source) {
        Some(&id) => id,
        None => {
            let id = sources.list.len();
            sources.list.push(source.to_owned());
            sources.ids.insert(source.to_owned(), id);
            id
        }
    };
    drop(sources);
    let digits = (usize::BITS - id.leading_zeros()).max(1);
    let mut text = String::with_capacity(3 * (digits as usize + 1) + 2);
    text.push(START);
    for digit in (0..digits).rev() {
        text.push(if id >> digit & 1 == 1 { ONE } else { ZERO });
    }
    text.push(SPACE);
    text
}

/// `text` with each placeholder replaced by its source and stray marker
/// characters left out; `None` if it has none.
fn restore(text: &str, list: &[String]) -> Option<String> {
    if !text.contains([START, ZERO, ONE]) {
        return None;
    }
    let mut restored = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            START => {
                let mut id = 0usize;
                while let Some(&digit @ (ZERO | ONE)) = chars.peek() {
                    id = id.saturating_mul(2).saturating_add((digit == ONE) as usize);
                    chars.next();
                }
                match list.get(id) {
                    Some(source) if chars.peek() == Some(&SPACE) => {
                        chars.next();
                        restored.push_str(source);
                    }
                    _ => {}
                }
            }
            ZERO | ONE => {}
            _ => restored.push(c),
        }
    }
    Some(restored)
}

/// Puts the sources of pictures back into copied text.
pub struct CopyPlugin;

impl egui::plugin::Plugin for CopyPlugin {
    fn debug_name(&self) -> &'static str {
        "copy pictures"
    }

    fn output_hook(&mut self, _ctx: &egui::Context, output: &mut egui::FullOutput) {
        for command in &mut output.platform_output.commands {
            if let egui::OutputCommand::CopyText(text) = command
                && let Some(restored) = restore(text, &sources().list)
            {
                *text = restored;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_restored_to_their_sources() {
        let rocket = placeholder("🚀");
        let formula = placeholder("$x^2$");
        assert!(rocket.ends_with(SPACE) && rocket.starts_with(START));
        assert_eq!(placeholder("🚀"), rocket);
        let copied = format!("Go {rocket} now, {formula}\u{a0}ok");
        let restored = restore(&copied, &sources().list).unwrap();
        assert_eq!(restored, "Go 🚀 now, $x^2$\u{a0}ok");
    }

    #[test]
    fn text_without_markers_is_left_alone() {
        assert_eq!(restore("plain\u{a0}text", &[]), None);
    }

    #[test]
    fn a_marker_cut_short_is_dropped() {
        let list = ["a".to_owned(), "b".to_owned()];
        // The start of the marker was not selected.
        assert_eq!(restore("x\u{2064}\u{a0}y", &list).unwrap(), "x\u{a0}y");
        // The space was not selected.
        assert_eq!(restore("x\u{2063}\u{2064}", &list).unwrap(), "x");
        assert_eq!(restore("\u{2063}\u{2064}\u{a0}", &list).unwrap(), "b");
    }
}
