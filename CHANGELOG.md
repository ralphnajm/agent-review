# Changelog

All notable changes to Agent Review are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/). Each version's section becomes the
notes of its GitHub release.

## [0.2.0] - 2026-10-12

### Fixed

- Cancel and Comment in the comment box no longer crash the app.
- The comment box gets keyboard focus as soon as it opens and behaves like a normal text
  field: typing, pasting, the right-click menu and clicking away all work.
- Cancelling a comment (Cancel or Esc) discards the draft instead of keeping it for the
  next comment.

### Added

- Selecting lines in the diff shows a "Comment on lines …" button for the selection.

## [0.1.2] - 2026-10-12

### Changed

- Wording no longer mentions Claude: Agent Review works with any coding agent, and the
  copied feedback can be pasted into whichever one you use.

## [0.1.1] - 2026-10-12

### Fixed

- The Fedora package can be installed: its glibc requirements were malformed and dnf could
  not resolve them.
- Package descriptions match what the app does.

## [0.1.0] - 2026-10-12

### Added

- Open any Git repository, or a folder inside one, and switch between recent projects.
- Browse all, staged or unstaged uncommitted changes, including untracked files, with
  live updates as files change.
- Syntax highlighted diffs with old and new line numbers.
- Inline comments on a line, a range of lines (shift-click or select and press Ctrl+M),
  or the whole file.
- "Reviewed" checkbox per file; checking it opens the next unreviewed file, and files
  changed after review are flagged.
- Copy Feedback: copies unsent comments to the clipboard as a Markdown prompt for
  Claude Code.
- Fedora (`.rpm`) and Ubuntu (`.deb`) packages.
