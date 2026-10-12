//! Parsing of `git status --porcelain=v2 -z --branch`.

/// The kind of change recorded for one side (index or worktree) of a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Conflicted,
    Untracked,
}

impl ChangeKind {
    fn from_code(code: u8) -> Option<Self> {
        match code {
            b'A' => Some(Self::Added),
            b'M' => Some(Self::Modified),
            b'D' => Some(Self::Deleted),
            b'R' => Some(Self::Renamed),
            b'C' => Some(Self::Copied),
            b'T' => Some(Self::TypeChanged),
            b'U' => Some(Self::Conflicted),
            _ => None,
        }
    }

    /// Single-letter badge shown in the file list.
    pub const fn letter(self) -> &'static str {
        match self {
            Self::Added | Self::Untracked => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Conflicted => "U",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Added => "Added",
            Self::Modified => "Modified",
            Self::Deleted => "Deleted",
            Self::Renamed => "Renamed",
            Self::Copied => "Copied",
            Self::TypeChanged => "Type changed",
            Self::Conflicted => "Conflicted",
            Self::Untracked => "New, untracked",
        }
    }

    /// CSS class used to colour the badge.
    pub const fn css_class(self) -> &'static str {
        match self {
            Self::Added | Self::Untracked | Self::Copied => "added",
            Self::Modified | Self::TypeChanged => "modified",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
            Self::Conflicted => "conflicted",
        }
    }
}

/// Which changes are being reviewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffMode {
    /// Everything that differs from `HEAD`, including untracked files.
    #[default]
    All,
    /// `HEAD` → index.
    Staged,
    /// Index → worktree, including untracked files.
    Unstaged,
}

impl DiffMode {
    pub const fn id(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "all" => Some(Self::All),
            "staged" => Some(Self::Staged),
            "unstaged" => Some(Self::Unstaged),
            _ => None,
        }
    }
}

/// One changed path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub path: String,
    pub orig_path: Option<String>,
    pub staged: Option<ChangeKind>,
    pub unstaged: Option<ChangeKind>,
}

impl StatusEntry {
    pub fn is_untracked(&self) -> bool {
        self.unstaged == Some(ChangeKind::Untracked)
    }

    pub fn is_conflicted(&self) -> bool {
        self.staged == Some(ChangeKind::Conflicted) || self.unstaged == Some(ChangeKind::Conflicted)
    }

    /// Whether the entry belongs in the list for `mode`.
    pub fn visible_in(&self, mode: DiffMode) -> bool {
        match mode {
            DiffMode::All => true,
            DiffMode::Staged => self.staged.is_some() && !self.is_untracked(),
            DiffMode::Unstaged => self.unstaged.is_some(),
        }
    }

    /// The change to display for `mode`.
    pub fn kind_in(&self, mode: DiffMode) -> ChangeKind {
        if self.is_conflicted() {
            return ChangeKind::Conflicted;
        }
        match mode {
            DiffMode::Staged => self.staged.unwrap_or(ChangeKind::Modified),
            DiffMode::Unstaged => self.unstaged.unwrap_or(ChangeKind::Modified),
            DiffMode::All => match (self.staged, self.unstaged) {
                (_, Some(ChangeKind::Untracked)) => ChangeKind::Untracked,
                (_, Some(ChangeKind::Deleted)) | (Some(ChangeKind::Deleted), _) => ChangeKind::Deleted,
                (Some(kind @ (ChangeKind::Added | ChangeKind::Renamed | ChangeKind::Copied)), _)
                | (Some(kind), None)
                | (None, Some(kind)) => kind,
                _ => ChangeKind::Modified,
            },
        }
    }
}

/// Parsed output of `git status`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoStatus {
    /// Branch name, `None` when detached.
    pub branch: Option<String>,
    /// `false` for a repository without commits.
    pub has_head: bool,
    pub entries: Vec<StatusEntry>,
}

/// Parses `git status --porcelain=v2 -z --branch` output.
pub fn parse(output: &[u8]) -> RepoStatus {
    let mut status = RepoStatus { has_head: true, ..RepoStatus::default() };
    let mut records = output.split(|b| *b == 0).filter(|r| !r.is_empty());

    while let Some(record) = records.next() {
        let record = String::from_utf8_lossy(record);
        let mut fields = record.splitn(2, ' ');
        let (Some(tag), Some(rest)) = (fields.next(), fields.next()) else {
            continue;
        };
        match tag {
            "#" => parse_header(rest, &mut status),
            "?" => status.entries.push(StatusEntry {
                path: rest.to_owned(),
                orig_path: None,
                staged: None,
                unstaged: Some(ChangeKind::Untracked),
            }),
            "1" => {
                if let Some((xy, path)) = split_entry(rest, 6) {
                    status.entries.push(entry(xy, path, None));
                }
            }
            "2" => {
                if let Some((xy, path)) = split_entry(rest, 7) {
                    let orig = records.next().map(|r| String::from_utf8_lossy(r).into_owned());
                    status.entries.push(entry(xy, path, orig));
                }
            }
            "u" => {
                if let Some((_, path)) = split_entry(rest, 8) {
                    status.entries.push(StatusEntry {
                        path: path.to_owned(),
                        orig_path: None,
                        staged: Some(ChangeKind::Conflicted),
                        unstaged: Some(ChangeKind::Conflicted),
                    });
                }
            }
            _ => {}
        }
    }

    status.entries.sort_by(|a, b| a.path.cmp(&b.path));
    status
}

fn parse_header(rest: &str, status: &mut RepoStatus) {
    if let Some(oid) = rest.strip_prefix("branch.oid ") {
        status.has_head = oid != "(initial)";
    } else if let Some(head) = rest.strip_prefix("branch.head ") {
        status.branch = (head != "(detached)").then(|| head.to_owned());
    }
}

/// Splits `XY <n other fields> <path>` into `XY` and the path.
fn split_entry(rest: &str, other_fields: usize) -> Option<(&str, &str)> {
    let mut parts = rest.splitn(other_fields + 2, ' ');
    let xy = parts.next()?;
    let path = parts.nth(other_fields)?;
    Some((xy, path))
}

fn entry(xy: &str, path: &str, orig_path: Option<String>) -> StatusEntry {
    let bytes = xy.as_bytes();
    StatusEntry {
        path: path.to_owned(),
        orig_path,
        staged: bytes.first().copied().and_then(ChangeKind::from_code),
        unstaged: bytes.get(1).copied().and_then(ChangeKind::from_code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = b"# branch.oid 1234567890abcdef\0# branch.head main\0\
1 .M N... 100644 100644 100644 aaa aaa src/main.rs\0\
1 M. N... 100644 100644 100644 aaa bbb src/lib.rs\0\
1 MM N... 100644 100644 100644 aaa bbb README.md\0\
2 R. N... 100644 100644 100644 aaa aaa R100 src/new name.rs\0src/old.rs\0\
1 A. N... 000000 100644 100644 000 bbb added.txt\0\
u UU N... 100644 100644 100644 100644 a b c conflict.rs\0\
? notes/todo.md\0";

    #[test]
    fn parses_branch_and_entries() {
        let status = parse(SAMPLE);
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert!(status.has_head);
        let paths: Vec<_> = status.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            ["README.md", "added.txt", "conflict.rs", "notes/todo.md", "src/lib.rs", "src/main.rs", "src/new name.rs"]
        );
        let renamed = status.entries.iter().find(|e| e.path == "src/new name.rs").unwrap();
        assert_eq!(renamed.orig_path.as_deref(), Some("src/old.rs"));
        assert_eq!(renamed.staged, Some(ChangeKind::Renamed));
        assert_eq!(renamed.unstaged, None);
    }

    #[test]
    fn filters_by_mode() {
        let status = parse(SAMPLE);
        let visible = |mode| status.entries.iter().filter(|e| e.visible_in(mode)).count();
        assert_eq!(visible(DiffMode::All), 7);
        assert_eq!(visible(DiffMode::Staged), 5);
        assert_eq!(visible(DiffMode::Unstaged), 4);
    }

    #[test]
    fn combined_kind_prefers_meaningful_change() {
        let status = parse(SAMPLE);
        let kind = |path: &str| status.entries.iter().find(|e| e.path == path).unwrap().kind_in(DiffMode::All);
        assert_eq!(kind("added.txt"), ChangeKind::Added);
        assert_eq!(kind("notes/todo.md"), ChangeKind::Untracked);
        assert_eq!(kind("conflict.rs"), ChangeKind::Conflicted);
        assert_eq!(kind("README.md"), ChangeKind::Modified);
    }

    #[test]
    fn detects_unborn_branch() {
        let status = parse(b"# branch.oid (initial)\0# branch.head (detached)\0");
        assert!(!status.has_head);
        assert_eq!(status.branch, None);
    }
}
