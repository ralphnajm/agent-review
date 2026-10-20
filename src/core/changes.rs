//! The files a version changed, and how.

use crate::core::diff::FileDiff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl ChangeKind {
    /// Single-letter badge shown in the file list.
    pub const fn letter(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
            Self::Renamed => "R",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Added => "Added",
            Self::Modified => "Modified",
            Self::Deleted => "Deleted",
            Self::Renamed => "Renamed",
        }
    }

    /// CSS class used to colour the badge.
    pub const fn css_class(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
        }
    }
}

/// One file a version changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub kind: ChangeKind,
}

impl ChangedFile {
    pub fn of(diff: &FileDiff) -> Self {
        let kind = if diff.is_new {
            ChangeKind::Added
        } else if diff.is_deleted {
            ChangeKind::Deleted
        } else if diff.renamed_from().is_some() {
            ChangeKind::Renamed
        } else {
            ChangeKind::Modified
        };
        Self { path: diff.path().to_owned(), kind }
    }
}
