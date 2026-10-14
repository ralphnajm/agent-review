//! Access to a repository through the `git` command-line tool.
//!
//! Shelling out keeps the binary small and honours the user's git
//! configuration (attributes, `safe.directory`, filters, …) exactly.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::core::diff::{self, FileDiff};
use crate::core::review::Author;
use crate::core::status::{self, DiffMode, RepoStatus, StatusEntry};

/// The well-known id of git's empty tree, used as the base before the first commit.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

const DIFF_ARGS: [&str; 7] =
    ["diff", "--no-color", "--no-ext-diff", "--no-textconv", "--find-renames", "--src-prefix=a/", "--dst-prefix=b/"];

/// The fields `git log` prints for each commit, in the order [`parse_log`] reads them.
const LOG_FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%at%x1f%D%x1f%s%x1f%b";

/// One commit of the history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub id: String,
    /// The parents in the history shown: when the review is limited to a
    /// folder, the nearest ancestors that change it.
    pub parents: Vec<String>,
    pub author: String,
    /// Unix seconds.
    pub time: i64,
    /// Branches and tags pointing at the commit.
    pub refs: Vec<String>,
    /// Whether `HEAD` points at the commit.
    pub is_head: bool,
    pub subject: String,
    /// The rest of the message.
    pub body: String,
}

impl Commit {
    pub fn short_id(&self) -> &str {
        self.id.get(..7).unwrap_or(&self.id)
    }
}

/// The latest commits, children before their parents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    /// The commit the uncommitted changes are based on: `HEAD`, or its latest
    /// ancestor that changes the reviewed folder. `None` before the first commit.
    pub head: Option<String>,
    pub commits: Vec<Commit>,
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

/// Everything the UI needs after a refresh.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub status: RepoStatus,
    /// `HEAD` → worktree diffs by path; approvals are fingerprinted against these.
    pub all: HashMap<String, FileDiff>,
    /// Diffs for a staged/unstaged view, `None` when the view is `All`.
    pub view: Option<HashMap<String, FileDiff>>,
}

impl Snapshot {
    pub fn diffs(&self) -> &HashMap<String, FileDiff> {
        self.view.as_ref().unwrap_or(&self.all)
    }
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

    pub fn status(&self) -> Result<RepoStatus> {
        let output = self.run(&["status", "--porcelain=v2", "-z", "--branch", "--untracked-files=all"])?;
        Ok(status::parse(&output))
    }

    /// Reads the status and the diffs needed to display `mode`.
    pub fn snapshot(&self, mode: DiffMode) -> Result<Snapshot> {
        let status = self.status()?;
        let all = self.diff(DiffMode::All, &status)?;
        let view = (mode != DiffMode::All).then(|| self.diff(mode, &status)).transpose()?;
        Ok(Snapshot { status, all, view })
    }

    fn diff(&self, mode: DiffMode, status: &RepoStatus) -> Result<HashMap<String, FileDiff>> {
        let mut args = DIFF_ARGS.to_vec();
        match mode {
            DiffMode::All => args.push(if status.has_head { "HEAD" } else { EMPTY_TREE }),
            DiffMode::Staged => args.push("--cached"),
            DiffMode::Unstaged => {}
        }
        let mut diffs = parse_diffs(&self.run(&args)?);

        if mode != DiffMode::Staged {
            for entry in status.entries.iter().filter(|e| e.is_untracked()) {
                diffs.insert(entry.path.clone(), self.untracked_diff(&entry.path));
            }
        }
        Ok(diffs)
    }

    /// The latest `limit` commits of `HEAD` and the local branches, newest
    /// first but children always before their parents. Limited to the commits
    /// that change the reviewed folder, if any.
    pub fn history(&self, limit: usize) -> Result<History> {
        let output = self.command().args(["rev-list", "--max-count=1", "HEAD"]).args(self.pathspec()).output();
        let Some(head) = output.ok().filter(|o| o.status.success()) else {
            // No commits yet.
            return Ok(History::default());
        };
        let head = String::from_utf8_lossy(&head.stdout).trim().to_owned();
        let limit = format!("--max-count={limit}");
        // `--parents` links each commit to the parents shown when limited to a folder.
        let output = self.run(&["log", "HEAD", "--branches", "--date-order", "--parents", "-z", &limit, LOG_FORMAT])?;
        Ok(History { head: (!head.is_empty()).then_some(head), commits: parse_log(&output) })
    }

    /// The changes commit `id` made to its first parent, as a snapshot whose
    /// entries are all staged.
    pub fn commit_snapshot(&self, id: &str) -> Result<Snapshot> {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("{id} is not a commit id");
        }
        let output = self.command().args(["rev-parse", "--verify", "--quiet", &format!("{id}^")]).output()?;
        let parent = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let base = if output.status.success() && !parent.is_empty() { parent.as_str() } else { EMPTY_TREE };
        let mut args = DIFF_ARGS.to_vec();
        args.extend([base, id]);
        let all = parse_diffs(&self.run(&args)?);
        let mut entries: Vec<StatusEntry> = all.values().map(StatusEntry::committed).collect();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Snapshot { status: RepoStatus { branch: None, has_head: true, entries }, all, view: None })
    }

    /// The text of `path` as commit `id` left it. `None` for files too large
    /// to show, or that the commit deleted.
    pub fn commit_text(&self, id: &str, path: &str) -> Result<Option<String>> {
        self.blob_text(&format!("{id}:{path}"))
    }

    fn blob_text(&self, object: &str) -> Result<Option<String>> {
        let output = self.command().args(["cat-file", "blob", object]).output().context("Could not run git")?;
        if !output.status.success()
            || u64::try_from(output.stdout.len()).unwrap_or(u64::MAX) > diff::MAX_UNTRACKED_BYTES
        {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
    }

    /// The text of the new side of `path` in `mode`'s diff: the staged version
    /// for staged changes, else the working tree's. `None` for files too large
    /// to show, or that are gone.
    pub fn new_side_text(&self, path: &str, mode: DiffMode) -> Result<Option<String>> {
        if mode == DiffMode::Staged {
            return self.blob_text(&format!(":{path}"));
        }
        let full = self.root.join(path);
        let bytes = match std::fs::metadata(&full) {
            Ok(meta) if meta.is_file() => std::fs::read(&full)?,
            _ => return Ok(None),
        };
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > diff::MAX_UNTRACKED_BYTES {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    fn untracked_diff(&self, path: &str) -> FileDiff {
        let full = self.root.join(path);
        match std::fs::metadata(&full) {
            Ok(meta) if meta.len() > diff::MAX_UNTRACKED_BYTES => diff::oversized(path),
            Ok(meta) if meta.is_file() => {
                std::fs::read(&full).map_or_else(|_| diff::oversized(path), |bytes| diff::untracked(path, &bytes))
            }
            // Symlinks to directories, sockets, …: show as an empty new file.
            _ => diff::untracked(path, b""),
        }
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

fn parse_diffs(output: &[u8]) -> HashMap<String, FileDiff> {
    diff::parse(&String::from_utf8_lossy(output)).into_iter().map(|d| (d.path().to_owned(), d)).collect()
}

/// Parses `git log -z` output in [`LOG_FORMAT`].
fn parse_log(output: &[u8]) -> Vec<Commit> {
    output
        .split(|b| *b == 0)
        .filter_map(|record| {
            let record = String::from_utf8_lossy(record);
            let mut fields = record.trim_start_matches('\n').splitn(7, '\x1f');
            let id = fields.next().filter(|id| !id.is_empty())?.to_owned();
            let parents = fields.next()?.split_whitespace().map(str::to_owned).collect();
            let author = fields.next()?.to_owned();
            let time = fields.next()?.parse().unwrap_or(0);
            let (refs, is_head) = parse_refs(fields.next()?);
            let subject = fields.next()?.to_owned();
            let body = fields.next().unwrap_or_default().trim().to_owned();
            Some(Commit { id, parents, author, time, refs, is_head, subject, body })
        })
        .collect()
}

/// Reads `%D` (`HEAD -> main, origin/main, tag: v1.0`) into the names of the
/// branches and tags, and whether `HEAD` is among them.
fn parse_refs(decoration: &str) -> (Vec<String>, bool) {
    let mut is_head = false;
    let mut refs = Vec::new();
    for name in decoration.split(", ").map(str::trim).filter(|n| !n.is_empty()) {
        let name = match name.strip_prefix("HEAD -> ") {
            Some(branch) => {
                is_head = true;
                branch
            }
            None if name == "HEAD" => {
                is_head = true;
                continue;
            }
            None => name.strip_prefix("tag: ").unwrap_or(name),
        };
        if !name.ends_with("/HEAD") {
            refs.push(name.to_owned());
        }
    }
    (refs, is_head)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn snapshot_covers_staged_unstaged_and_untracked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("tracked.txt"), "one\n").unwrap();
        std::fs::write(root.join("staged.txt"), "a\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "init"]);

        std::fs::write(root.join("tracked.txt"), "one\ntwo\n").unwrap();
        std::fs::write(root.join("staged.txt"), "b\n").unwrap();
        git(root, &["add", "staged.txt"]);
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/new.txt"), "fresh\n").unwrap();

        let repo = Repo::discover(root).unwrap();
        assert_eq!(repo.scope, None);

        let all = repo.snapshot(DiffMode::All).unwrap();
        assert_eq!(all.status.branch.as_deref(), Some("main"));
        let mut paths: Vec<_> = all.diffs().keys().cloned().collect();
        paths.sort();
        assert_eq!(paths, ["staged.txt", "sub/new.txt", "tracked.txt"]);
        assert_eq!(all.all["tracked.txt"].additions, 1);

        let staged = repo.snapshot(DiffMode::Staged).unwrap();
        assert_eq!(staged.diffs().keys().collect::<Vec<_>>(), ["staged.txt"]);

        // The text the unchanged lines come from: staged or in the working tree.
        std::fs::write(root.join("staged.txt"), "c\n").unwrap();
        assert_eq!(repo.new_side_text("staged.txt", DiffMode::Staged).unwrap().as_deref(), Some("b\n"));
        assert_eq!(repo.new_side_text("staged.txt", DiffMode::All).unwrap().as_deref(), Some("c\n"));
        assert_eq!(repo.new_side_text("missing.txt", DiffMode::All).unwrap(), None);

        let scoped = Repo::discover(&root.join("sub")).unwrap();
        assert_eq!(scoped.scope.as_deref(), Some(Path::new("sub")));
        let scoped_snapshot = scoped.snapshot(DiffMode::All).unwrap();
        assert_eq!(scoped_snapshot.status.entries.len(), 1);

        let dirs = repo.watched_dirs().unwrap();
        assert!(dirs.contains(&repo.root.join("sub")));
    }

    #[test]
    fn reads_the_history_and_what_a_commit_changed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "-b", "main"]);
        let repo = Repo::discover(root).unwrap();
        assert_eq!(repo.history(10).unwrap(), History::default());

        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/a.txt"), "one\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "First"]);
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "Second", "-m", "With a body."]);
        std::fs::write(root.join("sub/a.txt"), "one\ntwo\n").unwrap();
        git(root, &["commit", "-q", "-am", "Third"]);
        git(root, &["tag", "v1"]);

        let history = repo.history(10).unwrap();
        let subjects: Vec<&str> = history.commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, ["Third", "Second", "First"]);
        let [third, second, first] = &history.commits[..] else { unreachable!() };
        assert_eq!(history.head.as_deref(), Some(third.id.as_str()));
        assert!(third.is_head && !second.is_head);
        assert_eq!(third.refs, ["main", "v1"]);
        assert_eq!(third.parents, std::slice::from_ref(&second.id));
        assert_eq!((second.author.as_str(), second.body.as_str()), ("Test", "With a body."));
        assert_eq!(first.parents, Vec::<String>::new());
        assert_eq!(repo.history(1).unwrap().commits.len(), 1);

        let snapshot = repo.commit_snapshot(&third.id).unwrap();
        assert_eq!(snapshot.all["sub/a.txt"].additions, 1);
        assert_eq!(snapshot.status.entries[0].kind_in(DiffMode::All), status::ChangeKind::Modified);
        let initial = repo.commit_snapshot(&first.id).unwrap();
        assert_eq!(initial.status.entries[0].kind_in(DiffMode::All), status::ChangeKind::Added);
        assert_eq!(repo.commit_text(&first.id, "sub/a.txt").unwrap().as_deref(), Some("one\n"));
        assert_eq!(repo.commit_text(&first.id, "b.txt").unwrap(), None);
        assert!(repo.commit_snapshot("HEAD~1").is_err());

        // Limited to a folder: the commits that change it, linked to each other.
        let scoped = Repo::discover(&root.join("sub")).unwrap();
        let history = scoped.history(10).unwrap();
        let subjects: Vec<&str> = history.commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, ["Third", "First"]);
        assert_eq!(history.commits[0].parents, std::slice::from_ref(&first.id));
        assert_eq!(scoped.commit_snapshot(&second.id).unwrap().status.entries, []);
    }

    #[test]
    fn reads_branch_and_tag_names() {
        assert_eq!(
            parse_refs("HEAD -> main, origin/main, origin/HEAD, tag: v1"),
            (vec!["main".into(), "origin/main".into(), "v1".into()], true)
        );
        assert_eq!(parse_refs("HEAD"), (vec![], true));
        assert_eq!(parse_refs(""), (vec![], false));
    }

    #[test]
    fn works_before_the_first_commit() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
        git(dir.path(), &["add", "a.txt"]);
        let snapshot = Repo::discover(dir.path()).unwrap().snapshot(DiffMode::All).unwrap();
        assert!(!snapshot.status.has_head);
        assert_eq!(snapshot.all["a.txt"].additions, 1);
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
