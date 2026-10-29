//! Obsidian vaults: a folder with `.obsidian` in it. Obsidian finds a
//! linked note or attachment anywhere in the vault by its name (or by the
//! end of its path), while a Markdown viewer looks next to the document.
//! When a relative link or image is not where it says, it is looked up
//! here, by the end of its path, among the vault's files.
//!
//! Nothing of this runs at startup: a vault is listed only when something
//! in it is missing, on a thread of its own when a frame asks
//! ([`locate`], which says `Waiting` meanwhile and repaints when the list
//! is ready) or at once for a click or printing ([`locate_now`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Where a vault's listing stops: a folder of this many files is not a
/// vault worth walking on every miss.
const MAX_FILES: usize = 200_000;
/// A listing older than this is made again when a click misses in it: a
/// note may have been created since.
const STALE: Duration = Duration::from_secs(10);

/// The files of a vault.
pub struct Index {
    root: PathBuf,
    /// Paths relative to the root with `/` between parts, lower case, for
    /// matching; `paths` holds them as they are.
    lower: Vec<String>,
    paths: Vec<PathBuf>,
    /// Lower-case file name → indices into `paths`.
    by_name: HashMap<String, Vec<u32>>,
    made: Instant,
}

impl Index {
    /// Lists `root`'s files, leaving out folders whose name starts with a
    /// dot (`.obsidian`, `.git`, `.trash`) and `node_modules`.
    pub fn build(root: &Path) -> Index {
        let mut index = Index {
            root: root.to_path_buf(),
            lower: Vec::new(),
            paths: Vec::new(),
            by_name: HashMap::new(),
            made: Instant::now(),
        };
        let mut stack = vec![PathBuf::new()];
        while let Some(relative) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(root.join(&relative)) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    if !name.starts_with('.') && name != "node_modules" {
                        stack.push(relative.join(&*name));
                    }
                    continue;
                }
                if index.paths.len() >= MAX_FILES {
                    break;
                }
                let path = relative.join(&*name);
                let lower = path.to_string_lossy().replace('\\', "/").to_lowercase();
                index
                    .by_name
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(index.paths.len() as u32);
                index.lower.push(lower);
                index.paths.push(path);
            }
        }
        index.made = Instant::now();
        index
    }

    /// The file `target` (a path relative to some folder, `/` or `\`
    /// between parts) names in the vault: one whose path ends with it,
    /// else, for a target that is not a note already, with `.md` added.
    /// Of several, the one whose folder shares most with `near` (a folder
    /// in the vault), then the shortest path.
    pub fn find(&self, target: &str, near: &Path) -> Option<PathBuf> {
        let target = target.replace('\\', "/").to_lowercase();
        // `../x/y.png` that was not there: look for `x/y.png`.
        let target = target
            .rsplit_once("../")
            .map_or(target.as_str(), |(_, rest)| rest)
            .trim_start_matches("./")
            .trim_start_matches('/');
        if target.is_empty() {
            return None;
        }
        self.find_exact(target, near).or_else(|| {
            let note = crate::markdown::preprocess::is_markdown_path(target);
            (!note)
                .then(|| self.find_exact(&format!("{target}.md"), near))
                .flatten()
        })
    }

    fn find_exact(&self, target: &str, near: &Path) -> Option<PathBuf> {
        let name = target.rsplit('/').next()?;
        let candidates = self.by_name.get(name)?;
        let near: Vec<String> = near
            .strip_prefix(&self.root)
            .unwrap_or(Path::new(""))
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect();
        candidates
            .iter()
            .map(|&i| i as usize)
            .filter(|&i| {
                let path = &self.lower[i];
                path == target
                    || (path.ends_with(target) && path[..path.len() - target.len()].ends_with('/'))
            })
            .max_by_key(|&i| {
                let parts: Vec<&str> = self.lower[i].split('/').collect();
                let folder = &parts[..parts.len() - 1];
                let shared = folder
                    .iter()
                    .zip(&near)
                    .take_while(|(a, b)| **a == b.as_str())
                    .count();
                (shared, std::cmp::Reverse(parts.len()), std::cmp::Reverse(i))
            })
            .map(|i| self.root.join(&self.paths[i]))
    }
}

enum Slot {
    Building,
    Ready(Arc<Index>),
}

#[derive(Default)]
struct State {
    /// A folder → the vault it is in.
    roots: HashMap<PathBuf, Option<PathBuf>>,
    vaults: HashMap<PathBuf, Slot>,
    /// (folder, target) → where it was found.
    found: HashMap<(PathBuf, String), Option<PathBuf>>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The vault `folder` is in: the nearest folder holding `.obsidian`.
pub fn root_of(folder: &Path) -> Option<PathBuf> {
    if let Some(root) = state().roots.get(folder) {
        return root.clone();
    }
    let root = folder
        .ancestors()
        .find(|dir| dir.join(".obsidian").is_dir())
        .map(Path::to_path_buf);
    state().roots.insert(folder.to_path_buf(), root.clone());
    root
}

pub enum Located {
    Found(PathBuf),
    /// Not in a vault, or not in it.
    Missing,
    /// The vault is being listed; the frame is repainted when it is.
    Waiting,
}

/// Where `target`, a relative path that is not there from `folder`, is in
/// `folder`'s vault, without waiting: a vault not listed yet is listed on
/// a thread of its own, which repaints `ctx` when done.
pub fn locate(folder: &Path, target: &str, ctx: &egui::Context) -> Located {
    let key = (folder.to_path_buf(), target.to_owned());
    if let Some(found) = state().found.get(&key) {
        return found.clone().map_or(Located::Missing, Located::Found);
    }
    let Some(root) = root_of(folder) else {
        state().found.insert(key, None);
        return Located::Missing;
    };
    let index = {
        let mut guard = state();
        match guard.vaults.get(&root) {
            Some(Slot::Ready(index)) => index.clone(),
            Some(Slot::Building) => return Located::Waiting,
            None => {
                guard.vaults.insert(root.clone(), Slot::Building);
                drop(guard);
                let ctx = ctx.clone();
                let made = root.clone();
                let spawned = std::thread::Builder::new()
                    .name("vault".into())
                    .spawn(move || {
                        let index = Arc::new(Index::build(&made));
                        state().vaults.insert(made, Slot::Ready(index));
                        ctx.request_repaint();
                    });
                if spawned.is_ok() {
                    return Located::Waiting;
                }
                // No thread: listed here rather than waited for forever.
                let index = Arc::new(Index::build(&root));
                state().vaults.insert(root, Slot::Ready(index.clone()));
                index
            }
        }
    };
    let found = index.find(target, folder);
    state().found.insert(key, found.clone());
    found.map_or(Located::Missing, Located::Found)
}

/// [`locate`], listing the vault on this thread if it must (a click, a
/// page to print). A listing made more than [`STALE`] ago is made again
/// when the target is not in it.
pub fn locate_now(folder: &Path, target: &str) -> Option<PathBuf> {
    let root = root_of(folder)?;
    let ready = match state().vaults.get(&root) {
        Some(Slot::Ready(index)) => Some(index.clone()),
        _ => None,
    };
    if let Some(index) = &ready
        && let Some(found) = index.find(target, folder)
    {
        return Some(found);
    }
    if ready
        .as_ref()
        .is_some_and(|index| index.made.elapsed() < STALE)
    {
        return None;
    }
    let index = Arc::new(Index::build(&root));
    let found = index.find(target, folder);
    let mut state = state();
    state.vaults.insert(root, Slot::Ready(index));
    // What the old listing missed may be there now.
    state.found.clear();
    found
}

/// Forgets what was found and the listings, so that the next miss looks
/// again (a document reloaded by hand: files may have moved).
pub fn forget() {
    let mut state = state();
    state.found.clear();
    state.roots.clear();
    state
        .vaults
        .retain(|_, slot| matches!(slot, Slot::Building));
    drop(state);
    checks().clear();
}

/// Whether a link's file was found: folder → decoded path → found, `None`
/// while the check is queued.
type Checks = HashMap<PathBuf, HashMap<String, Option<bool>>>;

static CHECKS: LazyLock<Mutex<Checks>> = LazyLock::new(Default::default);

fn checks() -> std::sync::MutexGuard<'static, Checks> {
    CHECKS.lock().unwrap_or_else(|e| e.into_inner())
}

/// The thread that checks links, started with the first one.
static CHECKER: std::sync::OnceLock<Mutex<std::sync::mpsc::Sender<(PathBuf, String)>>> =
    std::sync::OnceLock::new();

/// Whether `target`, a link in a document in `folder`, names a file that
/// is neither there nor in the vault, as Obsidian shows such a link
/// fainter. Only paths are checked (not web addresses or anchors), on a
/// thread of its own: `false` until it has looked, then `ctx` repaints.
pub fn link_missing(folder: &Path, target: &str, ctx: &egui::Context) -> bool {
    let target = target.trim();
    if target.is_empty() || target.starts_with('#') || crate::app::has_url_scheme(target) {
        return false;
    }
    let path = target.split(['#', '?']).next().unwrap_or(target);
    if let Some(found) = checks().get(folder).and_then(|m| m.get(path)) {
        return *found == Some(false);
    }
    checks()
        .entry(folder.to_path_buf())
        .or_default()
        .insert(path.to_owned(), None);
    let sender = CHECKER.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::channel::<(PathBuf, String)>();
        let ctx = ctx.clone();
        let _ = std::thread::Builder::new()
            .name("links".into())
            .spawn(move || {
                for (folder, path) in receiver {
                    let decoded = crate::markdown::preprocess::percent_decode(&path);
                    let here = folder.join(&decoded);
                    // Not asked, as Windows would sign in to that computer.
                    let found = crate::ui::confirm::on_another_host(&here, Some(&folder))
                        || here.exists()
                        || locate_now(&folder, &decoded).is_some();
                    checks()
                        .entry(folder)
                        .or_default()
                        .insert(path, Some(found));
                    ctx.request_repaint();
                }
            });
        Mutex::new(sender)
    });
    let _ = sender
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .send((folder.to_path_buf(), path.to_owned()));
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(files: &[&str]) -> (PathBuf, Index) {
        let root = std::env::temp_dir().join(format!(
            "metalmd-vault-{}-{}",
            std::process::id(),
            files.len()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".obsidian")).unwrap();
        for file in files {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let index = Index::build(&root);
        (root, index)
    }

    #[test]
    fn missing_links_are_told_once_checked() {
        let (root, _) = vault(&["notes/a.md", "other/b.md"]);
        let folder = root.join("notes");
        let ctx = egui::Context::default();
        let ask = |target: &str| {
            // Queued first, then answered by the thread.
            for _ in 0..200 {
                let missing = link_missing(&folder, target, &ctx);
                if checks()
                    .get(&folder)
                    .and_then(|m| m.get(target))
                    .is_some_and(Option::is_some)
                {
                    return missing || link_missing(&folder, target, &ctx);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("{target} not checked");
        };
        assert!(!ask("a.md"));
        assert!(!ask("b.md"), "found in the vault");
        assert!(ask("gone.md"));
        assert!(ask("My%20Gone.md"));
        assert!(!link_missing(&folder, "https://example.com/x.md", &ctx));
        assert!(!link_missing(&folder, "#top", &ctx));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn notes_and_attachments_are_found_by_the_end_of_their_path() {
        let (root, index) = vault(&[
            "Windows/Скрипты.md",
            "Tools/mp3.md",
            "Tools/old/mp3.md",
            "!assets/Mouse/Pasted image 1.png",
            ".trash/gone.md",
            "A/x.png",
            "B/x.png",
        ]);
        let near = root.join("Windows");
        let rel = |p: Option<PathBuf>| {
            p.map(|p| {
                p.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
        };
        assert_eq!(
            rel(index.find("mp3.md", &near)).as_deref(),
            Some("Tools/mp3.md")
        );
        assert_eq!(
            rel(index.find("MP3.md", &near)).as_deref(),
            Some("Tools/mp3.md")
        );
        assert_eq!(
            rel(index.find("old/mp3.md", &near)).as_deref(),
            Some("Tools/old/mp3.md")
        );
        assert_eq!(
            rel(index.find("Pasted image 1.png", &near)).as_deref(),
            Some("!assets/Mouse/Pasted image 1.png")
        );
        assert_eq!(
            rel(index.find("../../Mouse/Pasted image 1.png", &near)).as_deref(),
            Some("!assets/Mouse/Pasted image 1.png")
        );
        // `ld/mp3.md` is not the end of a path's parts.
        assert_eq!(index.find("ld/mp3.md", &near), None);
        assert_eq!(index.find("gone.md", &near), None);
        // A note named with a dot: `[[Note.v2]]` links to `Note.v2`.
        assert_eq!(
            rel(index.find("Скрипты", &near)).as_deref(),
            Some("Windows/Скрипты.md")
        );
        // Of two, the one nearer.
        assert_eq!(
            rel(index.find("x.png", &root.join("B"))).as_deref(),
            Some("B/x.png")
        );
        assert_eq!(
            rel(index.find("x.png", &root.join("A"))).as_deref(),
            Some("A/x.png")
        );
        assert_eq!(root_of(&near), Some(root.clone()));
        let _ = std::fs::remove_dir_all(&root);
    }
}
