//! Reloads documents when they change on disk.
//!
//! The parent directory is watched rather than the file, so editors that save
//! by writing a temporary file and renaming it over the original are seen.
//! Events are coalesced for 150 ms so one save causes one reload.

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

const COALESCE: Duration = Duration::from_millis(150);

pub struct FileWatcher {
    watcher: RecommendedWatcher,
    /// Watched directory → number of open documents in it.
    dirs: HashMap<PathBuf, usize>,
}

impl FileWatcher {
    /// `on_change` is called from a background thread with each changed path.
    pub fn new(on_change: impl Fn(PathBuf) + Send + 'static) -> notify::Result<FileWatcher> {
        let (sender, receiver) = channel::<PathBuf>();
        let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            let Ok(event) = result else { return };
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            for path in event.paths {
                let _ = sender.send(path);
            }
        })?;
        std::thread::Builder::new()
            .name("watch-coalesce".into())
            .spawn(move || {
                let mut pending = HashSet::new();
                while let Ok(first) = receiver.recv() {
                    pending.insert(first);
                    let deadline = Instant::now() + COALESCE;
                    loop {
                        let left = deadline.saturating_duration_since(Instant::now());
                        match receiver.recv_timeout(left) {
                            Ok(path) => {
                                pending.insert(path);
                            }
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    for path in pending.drain() {
                        on_change(path);
                    }
                }
            })
            .expect("spawn watcher thread");
        Ok(FileWatcher {
            watcher,
            dirs: HashMap::new(),
        })
    }

    /// Watches `file`'s folder for one more open document. The folder is
    /// watched anew even if it already was: a watch on a folder that was
    /// deleted and made again (by git, by an editor) sees nothing, and
    /// notify does not say so.
    pub fn watch(&mut self, file: &Path) {
        let Some(dir) = file.parent() else { return };
        if !self.rewatch(dir) {
            return;
        }
        *self.dirs.entry(dir.to_path_buf()).or_insert(0) += 1;
    }

    /// Watches `file`'s folder anew, for a reload asked for by hand: it may
    /// be why the file seemed unchanged.
    pub fn refresh(&mut self, file: &Path) {
        let Some(dir) = file.parent() else { return };
        if self.dirs.contains_key(dir) && !self.rewatch(dir) {
            self.dirs.remove(dir);
        }
    }

    /// Drops the folder's watch, if any, and makes a new one.
    fn rewatch(&mut self, dir: &Path) -> bool {
        if self.dirs.contains_key(dir) {
            let _ = self.watcher.unwatch(dir);
        }
        self.watcher.watch(dir, RecursiveMode::NonRecursive).is_ok()
    }

    pub fn unwatch(&mut self, file: &Path) {
        let Some(dir) = file.parent() else { return };
        if let Some(count) = self.dirs.get_mut(dir) {
            *count -= 1;
            if *count == 0 {
                self.dirs.remove(dir);
                let _ = self.watcher.unwatch(dir);
            }
        }
    }
}
