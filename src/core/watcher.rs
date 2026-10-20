//! Watches a repository for `HEAD` moving, which may put the version under
//! review out of date, and watches the review queue for the agent's progress
//! and requests.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use notify::event::{AccessKind, AccessMode, ModifyKind};
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
    // HEAD and branch moves (commits, amends, resets, checkouts) live in the git dir.
    watcher.watch(&repo.git_dir, RecursiveMode::NonRecursive)?;
    // The reflog is appended on every commit, even when the index is untouched.
    let _ = watcher.watch(&repo.git_dir.join("logs"), RecursiveMode::NonRecursive);
    debounce(&rx, stop, on_change, |event| is_relevant(repo, event));
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
            // Lock files and object writes are noise; the final rename is what counts.
            let inside = path.strip_prefix(&repo.git_dir).ok().and_then(Path::to_str);
            matches!(inside, Some("HEAD" | "packed-refs" | "logs/HEAD"))
        })
}
