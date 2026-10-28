//! Access to a repository through the `git` command-line tool.
//!
//! Shelling out keeps the binary small and honours the user's git
//! configuration (attributes, `safe.directory`, filters, …) exactly.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::core::changes::ChangedFile;
use crate::core::diff::{self, FileDiff};
use crate::core::review::Author;

/// The well-known id of git's empty tree, used as the base before the first commit.
pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

const DIFF_ARGS: [&str; 7] =
    ["diff", "--no-color", "--no-ext-diff", "--no-textconv", "--find-renames", "--src-prefix=a/", "--dst-prefix=b/"];

/// The fields `git show` prints for a commit, in the order [`parse_commit`] reads them.
const COMMIT_FORMAT: &str = "--format=%H%x1f%an%x1f%at%x1f%s%x1f%b";

/// A commit, with its full message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub id: String,
    pub author: String,
    /// Unix seconds.
    pub time: i64,
    pub subject: String,
    /// The rest of the message.
    pub body: String,
}

impl Commit {
    pub fn short_id(&self) -> &str {
        self.id.get(..7).unwrap_or(&self.id)
    }
}

/// What is reviewed: the uncommitted changes in the working directory, or a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Worktree,
    Commit(Commit),
}

/// A repository, optionally narrowed to a sub-directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// Top-level directory of the working tree.
    pub root: PathBuf,
    /// Absolute path of the git directory.
    pub git_dir: PathBuf,
    /// Sub-directory (relative to `root`) the review is limited to.
    pub scope: Option<PathBuf>,
}

/// What a commit, or the working directory, changed.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// The files changed, by path.
    pub files: Vec<ChangedFile>,
    pub diffs: HashMap<String, FileDiff>,
}

impl Repo {
    /// Finds the repository that contains `path`.
    pub fn discover(path: &Path) -> Result<Self> {
        let path = path.canonicalize().with_context(|| format!("{} does not exist", path.display()))?;
        let output = Command::new("git")
            .arg("-C")
            .arg(&path)
            .args(["rev-parse", "--show-toplevel", "--absolute-git-dir"])
            .stdin(Stdio::null())
            .output()
            .context("Could not run git. Is it installed?")?;
        if !output.status.success() {
            bail!("{} is not inside a Git repository", path.display());
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut lines = stdout.lines();
        let (Some(root), Some(git_dir)) = (lines.next(), lines.next()) else {
            bail!("{} is not a Git working tree", path.display());
        };
        let root = PathBuf::from(root);
        let scope = path.strip_prefix(&root).ok().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf);
        Ok(Self { root, git_dir: PathBuf::from(git_dir), scope })
    }

    /// The directory the user opened.
    pub fn path(&self) -> PathBuf {
        self.scope.as_ref().map_or_else(|| self.root.clone(), |s| self.root.join(s))
    }

    /// Display name for the project.
    pub fn name(&self) -> String {
        let path = self.path();
        path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.root)
            // Never write to the index: that would wake our own file watcher.
            .args(["--no-optional-locks", "-c", "core.quotepath=off", "-c", "color.ui=never"])
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null());
        cmd
    }

    /// Who the repository's commits are authored by (`user.name` and `user.email`), if set.
    pub fn author(&self) -> Option<Author> {
        let config = |key: &str| {
            let output = self.command().args(["config", key]).output().ok()?;
            let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            (output.status.success() && !value.is_empty()).then_some(value)
        };
        Some(Author { name: config("user.name")?, email: config("user.email") })
    }

    fn pathspec(&self) -> [&std::ffi::OsStr; 2] {
        let scope = self.scope.as_deref().map_or(std::ffi::OsStr::new("."), Path::as_os_str);
        [std::ffi::OsStr::new("--"), scope]
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        let output = self.command().args(args).args(self.pathspec()).output().context("Could not run git")?;
        if !output.status.success() {
            bail!("git {} failed: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(output.stdout)
    }

    /// The branch checked out; `None` when `HEAD` is detached.
    pub fn branch(&self) -> Option<String> {
        let output = self.command().args(["branch", "--show-current"]).output().ok()?;
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        (output.status.success() && !branch.is_empty()).then_some(branch)
    }

    /// Whether there are changes not committed yet, untracked files included.
    pub fn has_uncommitted_changes(&self) -> Result<bool> {
        Ok(!self.run(&["status", "--porcelain", "--untracked-files=normal"])?.is_empty())
    }

    /// The uncommitted changes: everything in the working directory that
    /// differs from `HEAD`, untracked files included.
    pub fn worktree_snapshot(&self) -> Result<Snapshot> {
        let mut args = DIFF_ARGS.to_vec();
        let head = self.head();
        args.push(head.as_deref().unwrap_or(EMPTY_TREE));
        let mut diffs = parse_diffs(&self.run(&args)?);
        let untracked = self.run(&["ls-files", "-z", "--others", "--exclude-standard"])?;
        for path in untracked.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let path = String::from_utf8_lossy(path).into_owned();
            let diff = self.untracked_diff(&path);
            diffs.insert(path, diff);
        }
        let mut files: Vec<ChangedFile> = diffs.values().map(ChangedFile::of).collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Snapshot { files, diffs })
    }

    fn untracked_diff(&self, path: &str) -> FileDiff {
        let full = self.root.join(path);
        match std::fs::metadata(&full) {
            Ok(meta) if meta.is_file() && meta.len() <= diff::MAX_TEXT_BYTES => {
                diff::untracked(path, &std::fs::read(&full).unwrap_or_default())
            }
            // Too large to show, or a symlink to a directory, a socket, …: an empty new file.
            _ => diff::untracked(path, b""),
        }
    }

    /// The text of `path` in the working directory. `None` for files too
    /// large to show, or that are gone.
    pub fn worktree_text(&self, path: &str) -> Result<Option<String>> {
        let full = self.root.join(path);
        match std::fs::metadata(&full) {
            Ok(meta) if meta.is_file() && meta.len() <= diff::MAX_TEXT_BYTES => {
                Ok(Some(String::from_utf8_lossy(&std::fs::read(&full)?).into_owned()))
            }
            _ => Ok(None),
        }
    }

    /// The latest `limit` commits of `HEAD`, newest first. Limited to the
    /// commits that change the reviewed folder, if any.
    pub fn history(&self, limit: usize) -> Result<Vec<Commit>> {
        if self.head().is_none() {
            return Ok(Vec::new());
        }
        let limit = format!("--max-count={limit}");
        let output = self.run(&["log", "-z", &limit, COMMIT_FORMAT, "HEAD"])?;
        Ok(output.split(|b| *b == 0).filter_map(parse_commit).collect())
    }

    /// The changes from commit `base` to commit `id`.
    pub fn range_snapshot(&self, base: &str, id: &str) -> Result<Snapshot> {
        check_id(base)?;
        check_id(id)?;
        let mut args = DIFF_ARGS.to_vec();
        args.extend([base, id]);
        let diffs = parse_diffs(&self.run(&args)?);
        let mut files: Vec<ChangedFile> = diffs.values().map(ChangedFile::of).collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Snapshot { files, diffs })
    }

    /// The commit `HEAD` points at; `None` before the first commit.
    pub fn head(&self) -> Option<String> {
        self.rev_parse("HEAD")
    }

    /// Where the histories of commits `a` and `b` meet.
    pub fn merge_base(&self, a: &str, b: &str) -> Result<String> {
        let output = self.command().args(["merge-base", "--end-of-options", a, b]).output()?;
        let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !output.status.success() || id.is_empty() {
            bail!("{a} and {b} have no history in common");
        }
        Ok(id)
    }

    /// The commit `rev` names, as a full id.
    pub fn resolve(&self, rev: &str) -> Result<String> {
        self.rev_parse(&format!("{rev}^{{commit}}")).with_context(|| format!("{rev} is not a commit"))
    }

    /// What commit `id` alone is compared to: its parent, or nothing before the first commit.
    pub fn parent_or_empty(&self, id: &str) -> String {
        self.rev_parse(&format!("{id}^")).unwrap_or_else(|| EMPTY_TREE.to_owned())
    }

    fn rev_parse(&self, rev: &str) -> Option<String> {
        let output =
            self.command().args(["rev-parse", "--verify", "--quiet", "--end-of-options", rev]).output().ok()?;
        let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        (output.status.success() && !id.is_empty()).then_some(id)
    }

    /// Commit `id`, with its full message.
    pub fn commit(&self, id: &str) -> Result<Commit> {
        check_id(id)?;
        let output = self.command().args(["show", "--no-patch", COMMIT_FORMAT, id]).output()?;
        if !output.status.success() {
            bail!("Commit {id} no longer exists");
        }
        parse_commit(&output.stdout).with_context(|| format!("Could not read commit {id}"))
    }

    /// The text of `path` as commit `id` left it. `None` for files too large
    /// to show, or that the commit deleted.
    pub fn commit_text(&self, id: &str, path: &str) -> Result<Option<String>> {
        let output =
            self.command().args(["cat-file", "blob", &format!("{id}:{path}")]).output().context("Could not run git")?;
        if !output.status.success() || u64::try_from(output.stdout.len()).unwrap_or(u64::MAX) > diff::MAX_TEXT_BYTES {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
    }

    /// Directories containing tracked or non-ignored files, for the file watcher.
    pub fn watched_dirs(&self) -> Result<BTreeSet<PathBuf>> {
        let output = self.run(&["ls-files", "-z", "--cached", "--others", "--exclude-standard"])?;
        let mut dirs = BTreeSet::new();
        dirs.insert(self.path());
        for file in output.split(|b| *b == 0).filter(|f| !f.is_empty()) {
            let mut dir = Path::new(&*String::from_utf8_lossy(file)).parent().map(Path::to_path_buf);
            while let Some(current) = dir {
                if current.as_os_str().is_empty() || !dirs.insert(self.root.join(&current)) {
                    break;
                }
                dir = current.parent().map(Path::to_path_buf);
            }
        }
        Ok(dirs)
    }

    /// Whether git ignores `path`.
    pub fn is_ignored(&self, path: &Path) -> bool {
        self.command()
            .args(["check-ignore", "-q", "--"])
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

fn check_id(id: &str) -> Result<()> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("{id} is not a commit id");
    }
    Ok(())
}

fn parse_diffs(output: &[u8]) -> HashMap<String, FileDiff> {
    diff::parse(&String::from_utf8_lossy(output)).into_iter().map(|d| (d.path().to_owned(), d)).collect()
}

/// Parses a commit `git show` or `git log` printed in [`COMMIT_FORMAT`].
fn parse_commit(output: &[u8]) -> Option<Commit> {
    let record = String::from_utf8_lossy(output);
    let mut fields = record.trim_start_matches('\n').splitn(5, '\x1f');
    let id = fields.next().filter(|id| !id.is_empty())?.trim().to_owned();
    let author = fields.next()?.to_owned();
    let time = fields.next()?.parse().unwrap_or(0);
    let subject = fields.next()?.to_owned();
    let body = fields.next().unwrap_or_default().trim().to_owned();
    Some(Commit { id, author, time, subject, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::changes::ChangeKind;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn reads_commits_and_their_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "-b", "main"]);
        let repo = Repo::discover(root).unwrap();
        assert_eq!(repo.head(), None);

        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "Start"]);
        let start = repo.head().unwrap();
        // No parent: everything.
        assert_eq!(repo.parent_or_empty(&start), EMPTY_TREE);
        let initial = repo.range_snapshot(EMPTY_TREE, &start).unwrap();
        assert_eq!(initial.files, [ChangedFile { path: "a.txt".into(), kind: ChangeKind::Added }]);

        git(root, &["checkout", "-q", "-b", "feature"]);
        assert_eq!(repo.branch().as_deref(), Some("feature"));
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "Add b"]);
        let middle = repo.head().unwrap();
        assert_eq!(repo.parent_or_empty(&middle), start);
        std::fs::write(root.join("a.txt"), "a\nmore\n").unwrap();
        std::fs::write(root.join("c.txt"), "c\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "Add c", "-m", "Details."]);
        let head = repo.head().unwrap();
        assert_eq!(repo.parent_or_empty(&head), middle);
        let history = repo.history(10).unwrap();
        let subjects: Vec<&str> = history.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, ["Add c", "Add b", "Start"]);
        assert_eq!((history[0].body.as_str(), history[1].body.as_str()), ("Details.", ""));
        assert_eq!(repo.history(1).unwrap().len(), 1);

        // Compared to an earlier commit: everything since.
        let snapshot = repo.range_snapshot(&start, &head).unwrap();
        let files: Vec<(&str, ChangeKind)> = snapshot.files.iter().map(|f| (f.path.as_str(), f.kind)).collect();
        assert_eq!(
            files,
            [("a.txt", ChangeKind::Modified), ("b.txt", ChangeKind::Added), ("c.txt", ChangeKind::Added)]
        );
        assert_eq!(snapshot.diffs["a.txt"].additions, 1);
        assert!(repo.range_snapshot("HEAD~1", &head).is_err());

        assert_eq!(repo.commit_text(&start, "a.txt").unwrap().as_deref(), Some("a\n"));
        assert_eq!(repo.commit_text(&start, "b.txt").unwrap(), None);
        assert_eq!(repo.resolve("HEAD~1").unwrap(), middle);
        assert_eq!(repo.merge_base("main", &head).unwrap(), start);
        assert_eq!(repo.merge_base(&middle, &head).unwrap(), middle);
        assert!(repo.resolve("nope").is_err());
        let commit = repo.commit(&head).unwrap();
        assert_eq!(
            (commit.subject.as_str(), commit.body.as_str(), commit.author.as_str()),
            ("Add c", "Details.", "Test")
        );
        assert_eq!(commit.id, head);
        assert!(repo.commit("0123456789abcdef0123456789abcdef01234567").is_err());

        assert!(!repo.has_uncommitted_changes().unwrap());
        assert_eq!(repo.worktree_snapshot().unwrap().files, []);
        std::fs::write(root.join("d.txt"), "d\n").unwrap();
        std::fs::write(root.join("b.txt"), "b\nmore\n").unwrap();
        assert!(repo.has_uncommitted_changes().unwrap());
        // The uncommitted changes, untracked files included.
        let worktree = repo.worktree_snapshot().unwrap();
        let files: Vec<(&str, ChangeKind)> = worktree.files.iter().map(|f| (f.path.as_str(), f.kind)).collect();
        assert_eq!(files, [("b.txt", ChangeKind::Modified), ("d.txt", ChangeKind::Added)]);
        assert_eq!(worktree.diffs["d.txt"].additions, 1);
        assert_eq!(repo.worktree_text("b.txt").unwrap().as_deref(), Some("b\nmore\n"));
        assert_eq!(repo.worktree_text("gone.txt").unwrap(), None);
        git(root, &["checkout", "-q", "--", "b.txt"]);

        // Limited to a folder: only the changes inside it.
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/e.txt"), "e\n").unwrap();
        git(root, &["add", "sub"]);
        git(root, &["commit", "-q", "-m", "Add e"]);
        let scoped = Repo::discover(&root.join("sub")).unwrap();
        assert_eq!(scoped.scope.as_deref(), Some(Path::new("sub")));
        let files: Vec<String> =
            scoped.range_snapshot(&start, &repo.head().unwrap()).unwrap().files.into_iter().map(|f| f.path).collect();
        assert_eq!(files, ["sub/e.txt"]);
    }

    #[test]
    fn reads_the_author_from_git_config() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["config", "user.name", "Ada Lovelace"]);
        git(dir.path(), &["config", "user.email", "ada@example.com"]);
        let author = Repo::discover(dir.path()).unwrap().author().unwrap();
        assert_eq!(author.name, "Ada Lovelace");
        assert_eq!(author.email.as_deref(), Some("ada@example.com"));
    }

    #[test]
    fn rejects_non_repositories() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Repo::discover(dir.path()).is_err());
    }
}
