//! `agent-review --wait [path]`: waits until the user posts a review or an
//! approval the agent has not pulled yet, then says so and exits.
//!
//! An agent runs it in the background after requesting a review: when it
//! exits, the agent learns that the review is in, and pulls it through the MCP
//! server. It only reads the queue, so it leaves the pulling, and the writing
//! of progress, to the server.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::core::git::Repo;
use crate::core::queue::{Entry, Queue};

/// How often the queue is checked.
const INTERVAL: Duration = Duration::from_secs(1);

/// Waits on the repository containing `path`, or the working directory.
pub fn run(path: Option<PathBuf>) -> Result<()> {
    let path = match path {
        Some(path) => path,
        None => std::env::current_dir().context("No working directory")?,
    };
    let root = Repo::discover(&path)?.root;
    let queue = Queue::for_repo(&root);
    let entries = wait(&queue, INTERVAL);
    writeln!(std::io::stdout(), "{}", notice(&root, &entries))?;
    Ok(())
}

/// The reviews posted that the agent has not pulled, once there are any.
fn wait(queue: &Queue, interval: Duration) -> Vec<Entry> {
    loop {
        let entries: Vec<Entry> = queue.unpulled(0).into_iter().filter(Entry::is_pending).collect();
        if !entries.is_empty() {
            return entries;
        }
        std::thread::sleep(interval);
    }
}

/// What the user posted, and what to do next.
fn notice(root: &Path, entries: &[Entry]) -> String {
    let mut text = format!("The user posted in Agent Review, for {}:\n", root.display());
    for entry in entries {
        let on = entry.review.commit.as_ref().map_or_else(
            || "the uncommitted changes".to_owned(),
            |c| {
                let request = c.request.map_or_else(String::new, |r| format!(", review request {r}"));
                format!("commit {} ({}){request}", c.short_id(), c.subject)
            },
        );
        let count = entry.review.comments.len();
        let what = if entry.review.approved {
            "An approval".to_owned()
        } else if count == 1 {
            "A review with 1 comment".to_owned()
        } else {
            format!("A review with {count} comments")
        };
        let _ = writeln!(text, "- {what} of {on}");
    }
    let _ = write!(
        text,
        "\nCall get_review to pull it, and act on it. When you hand over the next version, run {} in the \
         background again to hear about the next review.",
        command(root)
    );
    text
}

/// The command that waits for reviews of the repository at `root`, as an
/// agent can run it.
pub fn command(root: &Path) -> String {
    format!("{} --wait {}", quote(&executable()), quote(&root.display().to_string()))
}

/// This program, by name when that is what the `PATH` finds.
fn executable() -> String {
    let Ok(exe) = std::env::current_exe() else { return "agent-review".to_owned() };
    let name = exe.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let on_path = std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            dir.join(name).canonicalize().ok().zip(exe.canonicalize().ok()).is_some_and(|(found, exe)| found == exe)
        })
    });
    if on_path { name.to_owned() } else { exe.display().to_string() }
}

/// `text` as a single shell word.
fn quote(text: &str) -> String {
    if !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+=:,@".contains(c)) {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::queue::PostedComment;
    use crate::core::review::ReviewedCommit;

    fn comment(body: &str) -> PostedComment {
        PostedComment {
            id: 0,
            note_id: 1,
            path: "a.rs".into(),
            anchor: None,
            page: None,
            screenshot: None,
            body: body.into(),
        }
    }

    #[test]
    fn waits_for_reviews_the_agent_has_not_pulled() {
        let dir = tempfile::tempdir().unwrap();
        let queue = Queue::at(dir.path().to_path_buf());
        let pulled = queue.post(None, [comment("Old")], 1).unwrap();
        queue.mark_pulled(&[pulled.id], 2).unwrap();

        let poster = {
            let queue = queue.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                let commit = ReviewedCommit {
                    id: "0123456789".into(),
                    subject: "Add a.rs".into(),
                    base: None,
                    request: Some(4),
                };
                queue.post(Some(commit.clone()), [comment("One"), comment("Two")], 3).unwrap();
                queue.approve(Some(commit), 4).unwrap();
            })
        };
        let entries = wait(&queue, Duration::from_millis(5));
        poster.join().unwrap();
        // The review, possibly with the approval if it came in time.
        assert_eq!(entries[0].review.comments.len(), 2);

        let entries: Vec<Entry> = queue.unpulled(0);
        let text = notice(Path::new("/repo"), &entries);
        assert!(text.starts_with("The user posted in Agent Review, for /repo:\n"), "{text}");
        assert!(text.contains("- A review with 2 comments of commit 0123456 (Add a.rs), review request 4\n"));
        assert!(text.contains("- An approval of commit 0123456 (Add a.rs), review request 4\n"));
        assert!(text.contains("Call get_review") && text.contains("--wait /repo in the background again"));
    }

    #[test]
    fn quotes_shell_words() {
        assert_eq!(quote("/home/me/repo"), "/home/me/repo");
        assert_eq!(quote("/home/me/my repo"), "'/home/me/my repo'");
        assert_eq!(quote("it's"), r"'it'\''s'");
    }
}
