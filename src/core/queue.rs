//! Posted reviews: the comments the user hands to the agent, which pulls them
//! through the MCP server (see [`crate::core::mcp`]) and resolves them.
//!
//! Each repository has a queue directory under the XDG data dir. The app
//! writes a review once, when it is posted; the MCP server records what the
//! agent did with it in a separate progress file. Every file has a single
//! writer, so the two processes never overwrite each other's changes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::review::LineAnchor;
use crate::core::store;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostedComment {
    /// Unique within the queue; the agent resolves comments by this id.
    pub id: u64,
    /// The review note the comment was posted from.
    pub note_id: u64,
    /// Relative to the repository root.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<LineAnchor>,
    pub body: String,
}

/// A batch of comments posted together. Never changed once written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostedReview {
    pub id: u64,
    pub posted_at: i64,
    pub comments: Vec<PostedComment>,
}

/// What the agent did with a posted review. Written by the MCP server only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Progress {
    /// When the agent first pulled the review.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pulled_at: Option<i64>,
    /// When each resolved comment was resolved, by comment id.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub resolved: BTreeMap<u64, i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Posted, not pulled by the agent yet.
    Waiting,
    /// Pulled by the agent, not resolved yet.
    PickedUp,
    Resolved,
}

/// A posted review together with the agent's progress on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub review: PostedReview,
    pub progress: Progress,
}

impl Entry {
    pub fn status(&self, comment: &PostedComment) -> Status {
        if self.progress.resolved.contains_key(&comment.id) {
            Status::Resolved
        } else if self.progress.pulled_at.is_some() {
            Status::PickedUp
        } else {
            Status::Waiting
        }
    }

    /// Comments not resolved yet.
    pub fn open(&self) -> impl Iterator<Item = &PostedComment> {
        self.review.comments.iter().filter(|c| !self.progress.resolved.contains_key(&c.id))
    }
}

/// The reviews posted for one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queue {
    dir: PathBuf,
}

impl Queue {
    /// The queue of the repository whose working tree is at `root`.
    pub fn for_repo(root: &Path) -> Self {
        Self::at(store::queue_dir(root))
    }

    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every posted review, oldest first.
    pub fn load(&self) -> Vec<Entry> {
        let Ok(files) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut entries: Vec<Entry> = files
            .flatten()
            .filter_map(|file| file.file_name().to_str()?.strip_suffix(".review.json")?.parse::<u64>().ok())
            .filter_map(|id| {
                let review: PostedReview = store::read_json(&self.review_file(id))?;
                let progress = store::read_json(&self.progress_file(id)).unwrap_or_default();
                Some(Entry { review, progress })
            })
            .collect();
        entries.sort_by_key(|e| e.review.id);
        entries
    }

    /// Posts `comments`, given as (note id, path, anchor, body), as a new review.
    pub fn post(
        &self,
        comments: impl IntoIterator<Item = (u64, String, Option<LineAnchor>, String)>,
        now: i64,
    ) -> std::io::Result<PostedReview> {
        let entries = self.load();
        let id = entries.last().map_or(0, |e| e.review.id) + 1;
        let first_comment = entries.iter().flat_map(|e| &e.review.comments).map(|c| c.id).max().unwrap_or(0) + 1;
        let comments = (first_comment..)
            .zip(comments)
            .map(|(id, (note_id, path, anchor, body))| PostedComment { id, note_id, path, anchor, body })
            .collect();
        let review = PostedReview { id, posted_at: now, comments };
        store::write_json(&self.review_file(id), &review)?;
        Ok(review)
    }

    /// Takes back review `id` unless the agent has pulled it already.
    /// Returns whether it was withdrawn.
    pub fn withdraw(&self, id: u64) -> std::io::Result<bool> {
        let progress: Progress = store::read_json(&self.progress_file(id)).unwrap_or_default();
        if progress.pulled_at.is_some() {
            return Ok(false);
        }
        std::fs::remove_file(self.review_file(id))?;
        Ok(true)
    }

    /// Records that the agent pulled the reviews with the given ids.
    pub fn mark_pulled(&self, ids: &[u64], now: i64) -> std::io::Result<()> {
        for &id in ids {
            let mut progress: Progress = store::read_json(&self.progress_file(id)).unwrap_or_default();
            if progress.pulled_at.is_none() {
                progress.pulled_at = Some(now);
                store::write_json(&self.progress_file(id), &progress)?;
            }
        }
        Ok(())
    }

    /// Marks the open comments among `ids` as resolved; returns their ids.
    pub fn resolve(&self, ids: &[u64], now: i64) -> std::io::Result<Vec<u64>> {
        let mut resolved = Vec::new();
        for entry in self.load() {
            let found: Vec<u64> = entry.open().map(|c| c.id).filter(|id| ids.contains(id)).collect();
            if found.is_empty() {
                continue;
            }
            let mut progress = entry.progress;
            progress.pulled_at.get_or_insert(now);
            progress.resolved.extend(found.iter().map(|&id| (id, now)));
            store::write_json(&self.progress_file(entry.review.id), &progress)?;
            resolved.extend(found);
        }
        resolved.sort_unstable();
        Ok(resolved)
    }

    fn review_file(&self, id: u64) -> PathBuf {
        self.dir.join(format!("{id}.review.json"))
    }

    fn progress_file(&self, id: u64) -> PathBuf {
        self.dir.join(format!("{id}.progress.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::review::Side;

    fn comment(note: u64, body: &str) -> (u64, String, Option<LineAnchor>, String) {
        let anchor = LineAnchor { side: Side::New, line: 3, end_line: None, text: "+x".into() };
        (note, "a.rs".to_owned(), Some(anchor), body.to_owned())
    }

    #[test]
    fn comments_move_from_waiting_to_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let queue = Queue::at(dir.path().join("queue"));
        assert_eq!(queue.load(), []);

        let first = queue.post([comment(7, "Rename"), comment(8, "Test")], 10).unwrap();
        let second = queue.post([comment(9, "Split")], 11).unwrap();
        assert_eq!((first.id, second.id), (1, 2));
        let ids: Vec<u64> = first.comments.iter().chain(&second.comments).map(|c| c.id).collect();
        assert_eq!(ids, [1, 2, 3]);

        let entries = queue.load();
        assert_eq!(entries[0].status(&entries[0].review.comments[0]), Status::Waiting);

        queue.mark_pulled(&[1], 12).unwrap();
        assert_eq!(queue.resolve(&[1, 3, 99], 13).unwrap(), [1, 3]);
        // Already resolved: nothing left to do.
        assert_eq!(queue.resolve(&[1], 14).unwrap(), Vec::<u64>::new());

        let entries = queue.load();
        let statuses: Vec<Status> =
            entries.iter().flat_map(|e| e.review.comments.iter().map(|c| e.status(c))).collect();
        assert_eq!(statuses, [Status::Resolved, Status::PickedUp, Status::Resolved]);
        assert_eq!(entries[0].progress.pulled_at, Some(12));
        assert_eq!(entries[1].open().count(), 0);
    }

    #[test]
    fn only_reviews_not_pulled_can_be_withdrawn() {
        let dir = tempfile::tempdir().unwrap();
        let queue = Queue::at(dir.path().to_path_buf());
        let first = queue.post([comment(1, "One")], 1).unwrap();
        let second = queue.post([comment(2, "Two")], 2).unwrap();
        queue.mark_pulled(&[second.id], 3).unwrap();
        assert!(queue.withdraw(first.id).unwrap());
        assert!(!queue.withdraw(second.id).unwrap());
        let left: Vec<u64> = queue.load().iter().map(|e| e.review.id).collect();
        assert_eq!(left, [second.id]);
    }
}
