//! Review state: approvals and notes per file. Persisted per project.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::core::diff::{DiffLine, FileDiff, LineKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// A removed line, numbered in the old file.
    Old,
    /// An added or unchanged line, numbered in the new file.
    New,
}

/// Where a note is attached inside a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineAnchor {
    pub side: Side,
    /// First line of the range.
    pub line: u32,
    /// Last line, when the note covers more than one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    /// The anchored diff lines when the note was written (see [`FileDiff::snippet`]).
    pub text: String,
}

impl LineAnchor {
    /// The anchor for the lines at indices `first..=last` of [`FileDiff::lines`].
    ///
    /// Ranges are numbered in the new file when they contain any added or
    /// unchanged line, and in the old file when they only cover removals.
    pub fn covering(diff: &FileDiff, first: usize, last: usize) -> Option<Self> {
        let (first, last) = (first.min(last), first.max(last));
        let lines: Vec<&DiffLine> = diff.lines().skip(first).take(last - first + 1).collect();
        let new: Vec<u32> = lines.iter().filter(|l| l.kind != LineKind::Removed).filter_map(|l| l.new_no).collect();
        let (side, numbers) = if new.is_empty() {
            (Side::Old, lines.iter().filter_map(|l| l.old_no).collect::<Vec<_>>())
        } else {
            (Side::New, new)
        };
        let start = *numbers.iter().min()?;
        let end = *numbers.iter().max()?;
        let text = diff.snippet(side == Side::Old, start, end)?;
        Some(Self { side, line: start, end_line: (end != start).then_some(end), text })
    }

    pub fn last_line(&self) -> u32 {
        self.end_line.unwrap_or(self.line)
    }

    pub fn label(&self) -> String {
        let removed = if self.side == Side::Old { "Removed l" } else { "L" };
        match self.end_line {
            Some(end) => format!("{removed}ines {}–{end}", self.line),
            None => format!("{removed}ine {}", self.line),
        }
    }

    /// Whether the anchored lines still exist unchanged in `diff`.
    pub fn is_current(&self, diff: Option<&FileDiff>) -> bool {
        diff.and_then(|d| d.snippet(self.side == Side::Old, self.line, self.last_line()))
            .is_some_and(|s| s == self.text)
    }
}

/// Who wrote a comment: the repository's git identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<LineAnchor>,
    pub body: String,
    /// Who wrote it; missing when git had no `user.name`, or for comments
    /// written before authors were recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<Author>,
    pub created_at: i64,
    /// When the note was last included in copied or sent feedback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    /// Fingerprint of the `HEAD` → worktree diff that was approved.
    pub fingerprint: String,
    pub at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileReview {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<Approval>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalState {
    Unreviewed,
    Approved,
    /// Approved, but the file has changed since.
    Stale,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewState {
    #[serde(default)]
    pub files: BTreeMap<String, FileReview>,
    #[serde(default)]
    next_id: u64,
}

impl ReviewState {
    pub fn approval(&self, path: &str, fingerprint: Option<&str>) -> ApprovalState {
        match self.files.get(path).and_then(|f| f.approval.as_ref()) {
            None => ApprovalState::Unreviewed,
            Some(a) if Some(a.fingerprint.as_str()) == fingerprint => ApprovalState::Approved,
            Some(_) => ApprovalState::Stale,
        }
    }

    pub fn set_approved(&mut self, path: &str, fingerprint: Option<&str>, now: i64) {
        let approval = fingerprint.map(|f| Approval { fingerprint: f.to_owned(), at: now });
        self.files.entry(path.to_owned()).or_default().approval = approval;
        self.prune(path);
    }

    pub fn notes(&self, path: &str) -> &[Note] {
        self.files.get(path).map_or(&[], |f| &f.notes)
    }

    pub fn unsent_notes(&self, path: &str) -> usize {
        self.notes(path).iter().filter(|n| n.sent_at.is_none()).count()
    }

    pub fn add_note(
        &mut self,
        path: &str,
        anchor: Option<LineAnchor>,
        body: &str,
        author: Option<&Author>,
        now: i64,
    ) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let author = author.cloned();
        let note = Note { id, anchor, body: body.trim().to_owned(), author, created_at: now, sent_at: None };
        self.files.entry(path.to_owned()).or_default().notes.push(note);
        id
    }

    pub fn remove_note(&mut self, path: &str, id: u64) {
        if let Some(file) = self.files.get_mut(path) {
            file.notes.retain(|n| n.id != id);
        }
        self.prune(path);
    }

    /// Puts back a note removed with [`Self::remove_note`].
    pub fn restore_note(&mut self, path: &str, note: Note) {
        let notes = &mut self.files.entry(path.to_owned()).or_default().notes;
        let position = notes.iter().position(|n| n.id > note.id).unwrap_or(notes.len());
        notes.insert(position, note);
    }

    /// Marks notes as sent (`Some(time)`) or unsent again (`None`).
    pub fn set_sent(&mut self, ids: &[u64], sent_at: Option<i64>) {
        for note in self.files.values_mut().flat_map(|f| &mut f.notes) {
            if ids.contains(&note.id) {
                note.sent_at = sent_at;
            }
        }
    }

    pub fn clear(&mut self) {
        self.files.clear();
    }

    fn prune(&mut self, path: &str) {
        if self.files.get(path).is_some_and(|f| f.approval.is_none() && f.notes.is_empty()) {
            self.files.remove(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_goes_stale_when_fingerprint_changes() {
        let mut review = ReviewState::default();
        assert_eq!(review.approval("a.rs", Some("1")), ApprovalState::Unreviewed);
        review.set_approved("a.rs", Some("1"), 10);
        assert_eq!(review.approval("a.rs", Some("1")), ApprovalState::Approved);
        assert_eq!(review.approval("a.rs", Some("2")), ApprovalState::Stale);
        review.set_approved("a.rs", None, 11);
        assert!(review.files.is_empty());
    }

    #[test]
    fn anchors_cover_line_ranges() {
        let diff = crate::core::diff::parse(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n keep\n-old\n+new\n same\n",
        )
        .remove(0);
        let range = LineAnchor::covering(&diff, 0, 3).unwrap();
        assert_eq!((range.side, range.line, range.end_line), (Side::New, 1, Some(3)));
        assert_eq!(range.label(), "Lines 1–3");
        assert_eq!(range.text, " keep\n-old\n+new\n same");
        assert!(range.is_current(Some(&diff)));

        let removed = LineAnchor::covering(&diff, 1, 1).unwrap();
        assert_eq!((removed.side, removed.line, removed.end_line), (Side::Old, 2, None));
        assert_eq!(removed.label(), "Removed line 2");
        assert_eq!(removed.text, "-old");
    }

    #[test]
    fn notes_round_trip_and_track_sent_state() {
        let mut review = ReviewState::default();
        let anchor = LineAnchor { side: Side::New, line: 3, end_line: None, text: "+let x = 1;".into() };
        let ada = Author { name: "Ada".into(), email: Some("ada@example.com".into()) };
        let first = review.add_note("a.rs", Some(anchor), "  Rename x  ", Some(&ada), 1);
        let second = review.add_note("a.rs", None, "Needs tests", None, 2);
        assert_ne!(first, second);
        assert_eq!(review.notes("a.rs")[0].body, "Rename x");
        assert_eq!(review.notes("a.rs")[0].author.as_ref(), Some(&ada));
        review.set_sent(&[first], Some(5));
        assert_eq!(review.unsent_notes("a.rs"), 1);

        let json = serde_json::to_string(&review).unwrap();
        let restored: ReviewState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, review);

        review.remove_note("a.rs", first);
        review.remove_note("a.rs", second);
        assert!(review.files.is_empty());
    }
}
