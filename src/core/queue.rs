//! Posted reviews: the comments the user hands to the agent, which pulls them
//! through the MCP server (see [`crate::core::mcp`]) and resolves them. And
//! review requests, in which the agent asks the user to review a version.
//!
//! Each repository has a queue directory under the XDG data dir. The app
//! writes a review once, when it is posted; the MCP server records what the
//! agent did with it in a separate progress file, and writes each review
//! request once. Every file has a single writer, so the two processes never
//! overwrite each other's changes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::review::{LineAnchor, ReviewedCommit};
use crate::core::store;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostedComment {
    /// Unique within the queue; the agent resolves comments by this id.
    pub id: u64,
    /// The review note the comment was posted from.
    pub note_id: u64,
    /// Relative to the repository root; empty for a comment on the running product.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<LineAnchor>,
    /// For a comment on the running product, the address of the page it is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    pub body: String,
}

/// A batch of comments posted together. Never changed once written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostedReview {
    pub id: u64,
    pub posted_at: i64,
    /// The commit reviewed; `None` for the uncommitted changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<ReviewedCommit>,
    pub comments: Vec<PostedComment>,
}

/// A version the agent asks the user to review: the changes from `base` to
/// `commit`, and the running product at `url`. Written by the MCP server once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub id: u64,
    pub requested_at: i64,
    pub commit: String,
    pub subject: String,
    /// The rest of the commit message.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    /// The commit the changes are compared to.
    pub base: String,
    /// Where the running product can be opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl ReviewRequest {
    pub fn short_id(&self) -> &str {
        self.commit.get(..7).unwrap_or(&self.commit)
    }
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

    /// Posts `comments` on `commit`, or on the uncommitted changes, as a new
    /// review, numbering them.
    pub fn post(
        &self,
        commit: Option<ReviewedCommit>,
        comments: impl IntoIterator<Item = PostedComment>,
        now: i64,
    ) -> std::io::Result<PostedReview> {
        let entries = self.load();
        let id = entries.last().map_or(0, |e| e.review.id) + 1;
        let first_comment = entries.iter().flat_map(|e| &e.review.comments).map(|c| c.id).max().unwrap_or(0) + 1;
        let comments = (first_comment..).zip(comments).map(|(id, comment)| PostedComment { id, ..comment }).collect();
        let review = PostedReview { id, posted_at: now, commit, comments };
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

    /// Every review request, oldest first.
    pub fn requests(&self) -> Vec<ReviewRequest> {
        let Ok(files) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut requests: Vec<ReviewRequest> = files
            .flatten()
            .filter(|file| file.file_name().to_str().is_some_and(|name| name.ends_with(".request.json")))
            .filter_map(|file| store::read_json(&file.path()))
            .collect();
        requests.sort_by_key(|r| r.id);
        requests
    }

    /// Records a request to review `request`, giving it the next id. Several
    /// agents may request reviews at once, so the file is only ever created.
    pub fn request(&self, mut request: ReviewRequest) -> std::io::Result<ReviewRequest> {
        std::fs::create_dir_all(&self.dir)?;
        loop {
            request.id = self.requests().last().map_or(0, |r| r.id) + 1;
            let file = self.dir.join(format!("{}.request.json", request.id));
            let tmp = self.dir.join(format!("{}.request.{}.tmp", request.id, std::process::id()));
            std::fs::write(&tmp, serde_json::to_vec_pretty(&request).map_err(std::io::Error::other)?)?;
            // Linking fails if another agent took the id meanwhile.
            let linked = std::fs::hard_link(&tmp, &file);
            std::fs::remove_file(&tmp)?;
            match linked {
                Ok(()) => return Ok(request),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(err),
            }
        }
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

    fn comment(note: u64, body: &str) -> PostedComment {
        let anchor = LineAnchor { side: Side::New, line: 3, end_line: None, text: "+x".into() };
        PostedComment { id: 0, note_id: note, path: "a.rs".into(), anchor: Some(anchor), page: None, body: body.into() }
    }

    #[test]
    fn comments_move_from_waiting_to_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let queue = Queue::at(dir.path().join("queue"));
        assert_eq!(queue.load(), []);

        let first = queue.post(None, [comment(7, "Rename"), comment(8, "Test")], 10).unwrap();
        let second = queue.post(None, [comment(9, "Split")], 11).unwrap();
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
    fn requests_get_their_own_ids() {
        let dir = tempfile::tempdir().unwrap();
        let queue = Queue::at(dir.path().join("queue"));
        assert_eq!(queue.requests(), []);
        let request = ReviewRequest {
            id: 0,
            requested_at: 1,
            commit: "0123456789abcdef".into(),
            subject: "Add the parser".into(),
            body: String::new(),
            base: "fedcba9876543210".into(),
            url: Some("http://localhost:5173".into()),
        };
        assert_eq!(queue.request(request.clone()).unwrap().id, 1);
        assert_eq!(queue.request(request.clone()).unwrap().id, 2);
        // Requests and posted reviews share the folder without mixing.
        queue.post(None, [comment(1, "One")], 2).unwrap();
        let requests = queue.requests();
        assert_eq!(requests.iter().map(|r| r.id).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(requests[0], ReviewRequest { id: 1, ..request });
        assert_eq!(requests[0].short_id(), "0123456");
        assert_eq!(queue.load().len(), 1);
    }

    #[test]
    fn only_reviews_not_pulled_can_be_withdrawn() {
        let dir = tempfile::tempdir().unwrap();
        let queue = Queue::at(dir.path().to_path_buf());
        let first = queue.post(None, [comment(1, "One")], 1).unwrap();
        let second = queue.post(None, [comment(2, "Two")], 2).unwrap();
        queue.mark_pulled(&[second.id], 3).unwrap();
        assert!(queue.withdraw(first.id).unwrap());
        assert!(!queue.withdraw(second.id).unwrap());
        let left: Vec<u64> = queue.load().iter().map(|e| e.review.id).collect();
        assert_eq!(left, [second.id]);
    }
}
