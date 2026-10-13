//! Access to a repository through the `git` command-line tool.
//!
//! Shelling out keeps the binary small and honours the user's git
//! configuration (attributes, `safe.directory`, filters, …) exactly.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::core::diff::{self, FileDiff};
use crate::core::status::{self, DiffMode, RepoStatus};

/// The well-known id of git's empty tree, used as the base before the first commit.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

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
        let mut args = vec![
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--find-renames",
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ];
        match mode {
            DiffMode::All => args.push(if status.has_head { "HEAD" } else { EMPTY_TREE }),
            DiffMode::Staged => args.push("--cached"),
            DiffMode::Unstaged => {}
        }
        let output = self.run(&args)?;
        let mut diffs: HashMap<String, FileDiff> =
            diff::parse(&String::from_utf8_lossy(&output)).into_iter().map(|d| (d.path().to_owned(), d)).collect();

        if mode != DiffMode::Staged {
            for entry in status.entries.iter().filter(|e| e.is_untracked()) {
                diffs.insert(entry.path.clone(), self.untracked_diff(&entry.path));
            }
        }
        Ok(diffs)
    }

    /// The text of the new side of `path` in `mode`'s diff: the staged version
    /// for staged changes, else the working tree's. `None` for files too large
    /// to show, or that are gone.
    pub fn new_side_text(&self, path: &str, mode: DiffMode) -> Result<Option<String>> {
        let bytes = if mode == DiffMode::Staged {
            let output =
                self.command().args(["cat-file", "blob", &format!(":{path}")]).output().context("Could not run git")?;
            if !output.status.success() {
                return Ok(None);
            }
            output.stdout
        } else {
            let full = self.root.join(path);
            match std::fs::metadata(&full) {
                Ok(meta) if meta.is_file() => std::fs::read(&full)?,
                _ => return Ok(None),
            }
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
    fn rejects_non_repositories() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Repo::discover(dir.path()).is_err());
    }
}
