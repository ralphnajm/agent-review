//! Watches a repository and reports when its changes may need refreshing.
//!
//! Only directories git cares about are watched (one inotify watch each), so
//! large ignored trees such as `node_modules` or `target` cost nothing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
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

    let mut first_pending: Option<Instant> = None;
    let mut last_event = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(event)) => {
                if !is_relevant(repo, &event) {
                    continue;
                }
                if matches!(event.kind, EventKind::Create(CreateKind::Folder | CreateKind::Any)) {
                    for path in &event.paths {
                        watch_new_dir(repo, &mut watcher, &mut watched_dirs, path);
                    }
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
    Ok(())
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

fn is_relevant(repo: &Repo, event: &Event) -> bool {
    let kind_matters = matches!(
        event.kind,
        EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Name(_) | ModifyKind::Any)
            | EventKind::Access(AccessKind::Close(AccessMode::Write))
    );
    kind_matters
        && event.paths.iter().any(|path| {
            if let Ok(inside) = path.strip_prefix(&repo.git_dir) {
                // Lock files and object writes are noise; the final rename is what counts.
                return matches!(inside.to_str(), Some("index" | "HEAD" | "packed-refs" | "logs/HEAD"));
            }
            !path.components().any(|c| c.as_os_str() == ".git")
        })
}
