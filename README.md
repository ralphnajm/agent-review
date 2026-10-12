# Agent Review

A native Linux desktop app for reviewing the changes AI coding agents make to your projects.
Comment on lines and files, mark files as reviewed, and copy the feedback to paste back into
your agent.

Built with Rust, GTK 4 and libadwaita (no web view). The release binary is about 1 MB and
links against the GTK libraries that Fedora and Ubuntu already ship.

## Features

- **Projects:** open any Git repository (or a sub-folder of one) and switch between recent
  projects at any time (Ctrl+P).
- **Changes:** browse all uncommitted changes, or only staged or only unstaged ones. Untracked
  files count as new. The view updates live as the agent edits files.
- **Diffs:** syntax highlighted, with old and new line numbers. Ctrl+F finds text in the open
  file; Ctrl+Shift+F filters the file list.
- **Comments:**
  - Click a line number to comment on that line, or drag down the line numbers to comment on
    several lines. Shift-click extends the previous selection.
  - Or select lines in the code and press Ctrl+M.
  - Use the icon next to the file name to comment on the whole file.
  - Comments appear inline, under the lines they refer to.
  - Comments already sent to the agent leave the diff. The clock button next to the file name
    shows how many there are and opens them above the diff for reference.
- **Reviewed checkbox:** tick the box next to the file name (Ctrl+Shift+A) to mark the file
  done; the next unreviewed file opens and the file list shows it as checked. Commenting on a
  file also marks it reviewed. If the file changes afterwards, it is flagged for another look.
- **Feedback:** *Copy Feedback* (Ctrl+Shift+C) puts all unsent comments on the clipboard as a
  Markdown prompt, ready to paste into your agent, and marks them as sent. The toast lets you
  mark them unsent again.

Review state lives in `~/.local/share/agent-review/`, never inside the repository.

## Install

Runtime requirements: GTK ≥ 4.14, libadwaita ≥ 1.5, GtkSourceView ≥ 5.12 and git. These are
available on Fedora 40+ and Ubuntu 24.04+.

```sh
make install PREFIX=~/.local    # just for you; "Agent Review" then shows up in app search
sudo make install               # system-wide, into /usr/local
```

To build a package instead:

```sh
cargo install cargo-generate-rpm && make rpm   # target/generate-rpm/*.rpm
cargo install cargo-deb && make deb            # target/debian/*.deb
```

You can also open a project straight from a terminal: `agent-review path/to/repo`.

## Development

Rust comes from [rustup](https://rustup.rs); `rust-toolchain.toml` pins the latest stable
release, so local builds and CI use the same compiler. Other build dependencies:

```sh
# Fedora
sudo dnf install rustup gtk4-devel libadwaita-devel gtksourceview5-devel desktop-file-utils appstream
# Ubuntu
sudo apt install libgtk-4-dev libadwaita-1-dev libgtksourceview-5-dev desktop-file-utils appstream
```

| Command | What it does |
| --- | --- |
| `make run` (or `cargo run -- <repo>`) | Run a debug build |
| `make check` | rustfmt check, clippy (pedantic, warnings as errors), tests, and desktop/AppStream validation |
| `make fmt` | Format the code |
| `make release` | Optimised build (thin LTO, stripped) |

CI (`.github/workflows/ci.yml`) runs `make check` on Ubuntu 24.04 and builds the `.deb` and
`.rpm` packages for every push and pull request.

### Releasing

Describe every user-facing change in `CHANGELOG.md` under `## [Unreleased]` as you make it.
To release:

1. Bump `version` in `Cargo.toml` following [Semantic Versioning](https://semver.org/): patch
   for fixes, minor for new features, major for breaking changes. Run `cargo build` so
   `Cargo.lock` picks up the new version.
2. Rename `## [Unreleased]` in `CHANGELOG.md` to `## [x.y.z] - YYYY-MM-DD`.
3. Add a matching `<release version="x.y.z" date="YYYY-MM-DD"/>` entry at the top of
   `<releases>` in `data/io.github.ralphnajm.AgentReview.metainfo.xml`.
4. Run `make check` (it fails if the changelog or metadata entry is missing), commit and push.

When the build succeeds, CI creates the `vx.y.z` tag and a GitHub release with the packages
attached and the changelog section as its notes. Pushing without a version bump only runs
the checks, because that version already has a release.

### Layout

```
src/
  core/         UI-independent logic, unit tested
    git.rs        repository access through the git CLI (status, diffs, watched dirs)
    status.rs     `git status --porcelain=v2` parser
    diff.rs       unified diff parser, fingerprints, line snippets
    review.rs     comments, line-range anchors and reviewed state
    feedback.rs   Markdown prompt generation
    watcher.rs    inotify watcher limited to non-ignored directories
    store.rs      settings and review persistence (XDG directories)
  ui/           GTK widgets
    window.rs     main window, actions and state
    diff_view.rs  GtkSourceView diff with inline comment threads
    file_list.rs, project_switcher.rs
data/           desktop entry, AppStream metadata, icons
```
