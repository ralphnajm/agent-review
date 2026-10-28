//! Watches a repository and reports when its changes may need refreshing,
//! and watches the review queue for the agent's progress and requests.
//!
//! Only directories git cares about are watched (one inotify watch each), so
//! large ignored trees such as `node_modules` or `target` cost nothing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use notify::event::{AccessKind, AccessMode, CreateKind, ModifyKind};
use notify::{Event, EventKind, RecursiveMode, Watcher as _};

use crate::core::git::Repo;

/// Wait for this much quiet before reporting a change…
const SETTLE: Duration = Duration::from_millis(250);
/// …but never delay a report longer than this while changes keep coming.
const MAX_DELAY: Duration = Duration::from_secs(1);

/// Stops watching when dropped.
pub struct WatchHandle {
    stop: Arc<AtomicBool>,
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Starts watching `repo`; `on_change` is called from a background thread.
pub fn watch(repo: Repo, on_change: impl Fn() + Send + 'static) -> WatchHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let handle = WatchHandle { stop: Arc::clone(&stop) };
    std::thread::Builder::new()
        .name("repo-watcher".into())
        .spawn(move || {
            if let Err(err) = run(&repo, &stop, &on_change) {
                eprintln!("agent-review: file watching disabled: {err}");
            }
        })
        .ok();
    handle
}

/// Starts watching the files directly inside `dir`, creating it if needed;
/// `on_change` is called from a background thread.
pub fn watch_dir(dir: PathBuf, on_change: impl Fn() + Send + 'static) -> WatchHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let handle = WatchHandle { stop: Arc::clone(&stop) };
    std::thread::Builder::new()
        .name("dir-watcher".into())
        .spawn(move || {
            let result = std::fs::create_dir_all(&dir).map_err(notify::Error::io).and_then(|()| {
                let (tx, rx) = channel();
                let mut watcher = notify::recommended_watcher(tx)?;
                watcher.watch(&dir, RecursiveMode::NonRecursive)?;
                debounce(&rx, &stop, &on_change, |event| is_write(event.kind));
                Ok(())
            });
            if let Err(err) = result {
                eprintln!("agent-review: not watching {}: {err}", dir.display());
            }
        })
        .ok();
    handle
}

fn run(repo: &Repo, stop: &AtomicBool, on_change: &dyn Fn()) -> notify::Result<()> {
    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(tx)?;
    let mut watched_dirs = BTreeSet::new();

    // HEAD and index changes (commits, staging, checkouts) live in the git dir.
    watcher.watch(&repo.git_dir, RecursiveMode::NonRecursive)?;
    // The reflog is appended on every commit, even when the index is untouched.
    let _ = watcher.watch(&repo.git_dir.join("logs"), RecursiveMode::NonRecursive);
    for dir in repo.watched_dirs().unwrap_or_default() {
        if watcher.watch(&dir, RecursiveMode::NonRecursive).is_ok() {
            watched_dirs.insert(dir);
        }
    }

    debounce(&rx, stop, on_change, |event| {
        if !is_relevant(repo, event) {
            return false;
        }
        if matches!(event.kind, EventKind::Create(CreateKind::Folder | CreateKind::Any)) {
            for path in &event.paths {
                watch_new_dir(repo, &mut watcher, &mut watched_dirs, path);
            }
        }
        true
    });
    Ok(())
}

/// Calls `on_change` once events accepted by `accept` settle, until `stop` is set.
fn debounce(
    rx: &Receiver<notify::Result<Event>>,
    stop: &AtomicBool,
    on_change: &dyn Fn(),
    mut accept: impl FnMut(&Event) -> bool,
) {
    let mut first_pending: Option<Instant> = None;
    let mut last_event = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(event)) => {
                if !accept(&event) {
                    continue;
                }
                last_event = Instant::now();
                first_pending.get_or_insert(last_event);
            }
            Ok(Err(_)) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if let Some(first) = first_pending
            && (last_event.elapsed() >= SETTLE || first.elapsed() >= MAX_DELAY)
        {
            first_pending = None;
            on_change();
        }
    }
}

fn watch_new_dir(repo: &Repo, notifier: &mut impl notify::Watcher, watched: &mut BTreeSet<PathBuf>, path: &Path) {
    if !path.is_dir() || watched.contains(path) || repo.is_ignored(path) {
        return;
    }
    if notifier.watch(path, RecursiveMode::NonRecursive).is_ok() {
        watched.insert(path.to_path_buf());
    }
    // Directories created together with their contents (`mkdir -p`, `git checkout`).
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            watch_new_dir(repo, notifier, watched, &entry.path());
        }
    }
}

/// Whether an event may have changed what a file or directory holds.
fn is_write(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Name(_) | ModifyKind::Any)
            | EventKind::Access(AccessKind::Close(AccessMode::Write))
    )
}

fn is_relevant(repo: &Repo, event: &Event) -> bool {
    is_write(event.kind)
        && event.paths.iter().any(|path| {
            if let Ok(inside) = path.strip_prefix(&repo.git_dir) {
                // Lock files and object writes are noise; the final rename is what counts.
                return matches!(inside.to_str(), Some("index" | "HEAD" | "packed-refs" | "logs/HEAD"));
            }
            !path.components().any(|c| c.as_os_str() == ".git")
        })
}
