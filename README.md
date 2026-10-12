# Agent Review

A native Linux desktop app for reviewing the changes Claude Code makes to your projects.
Comment on lines and files, mark files as reviewed, and copy the feedback to paste back into
Claude Code.

Built with Rust, GTK 4 and libadwaita (no web view). The release binary is about 1 MB and
links against the GTK libraries that Fedora and Ubuntu already ship.

## Features

- **Projects:** open any Git repository (or a sub-folder of one) and switch between recent
  projects at any time (Ctrl+P).
- **Changes:** browse all uncommitted changes, or only staged or only unstaged ones. Untracked
  files count as new. The view updates live as Claude edits files.
- **Diffs:** syntax highlighted, with old and new line numbers.
- **Comments:**
  - Click a line number to comment on that line, or shift-click to extend to a range.
  - Select lines and press Ctrl+M to comment on the selection.
  - Use the icon next to the file name to comment on the whole file.
  - Comments appear inline, under the lines they refer to.
- **Reviewed checkbox:** tick the box next to the file name (Ctrl+Shift+A) to mark the file
  done; the next unreviewed file opens and the file list shows it as checked. Commenting on a
  file also marks it reviewed. If the file changes afterwards, it is flagged for another look.
- **Feedback:** *Copy Feedback* (Ctrl+Shift+C) puts all unsent comments on the clipboard as a
  Markdown prompt, ready to paste into Claude Code, and marks them as sent. The toast lets you
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

CI (`.github/workflows/ci.yml`) runs `make check` on Ubuntu 24.04 and builds `.deb` and `.rpm`
artifacts.

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
