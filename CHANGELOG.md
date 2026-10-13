# Changelog

All notable changes to Agent Review are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/). Each version's section becomes the
notes of its GitHub release.

## [0.7.0] - 2026-10-13

### Added

- A one-line installer: `curl -fsSL https://raw.githubusercontent.com/ralphnajm/agent-review/main/scripts/install.sh | sh`
  installs or updates to the latest release on Ubuntu, Debian, Fedora and their derivatives.

### Changed

- **Posted Reviews** moved from its button in the top bar to the main menu (still Ctrl+Shift+R).
- The button for commenting on lines shows a comment icon instead of a plus.

## [0.6.0] - 2026-10-13

### Added

- Comments show who wrote them, both while you write one and on the comment itself: the
  repository's git `user.name`, with `user.email` on hover, or "Unknown" when git has no name.

### Changed

- Long lines wrap to fit the window instead of scrolling the review sideways.
- Hovering a line shows a **+** button at its start: click it to comment on the line, or drag it
  up or down to comment on several lines. The lines are outlined in a blue box rather than
  selected, so selecting code to copy it no longer gets mixed up with picking lines to comment
  on.
- The `+`/`−` markers next to the line numbers are gone: the line colours already tell added
  from removed lines.

### Fixed

- Selecting lines to comment on no longer scrolls the diff away from them, pushing the comment
  box out of view: the diff only scrolls when the comment box would open off screen.

## [0.5.0] - 2026-10-13

### Added

- **Post Review** hands your comments to your agent without copy and paste: your agent pulls
  them through Agent Review's MCP server (`agent-review --mcp`; see "Connecting your agent"
  in the README) and marks each comment resolved once it has addressed it. Undo takes a
  review back until the agent pulls it.
- **Posted reviews** (Ctrl+Shift+R, or the list button in the top bar) shows every review you
  posted, with each comment marked *Waiting*, *Picked up* or *Resolved*, and updates live as
  your agent works. The button counts the comments not resolved yet.
- The unchanged lines a diff leaves out are no longer out of reach: click the arrow on a grey
  row to show them, whether at the start of the file, between two changes or at its end.

### Changed

- Copy Feedback moved to the arrow next to Post Review; Ctrl+Shift+C still copies.
- The main panel now shows every changed file in one scrolling view, so you can read through
  all the changes in one go. Each file's header stays at the top while you scroll through it.
- Ticking a file's "Reviewed" checkbox collapses it to its header and scrolls to the next
  file to review. Click a header to collapse or expand a file yourself. Reviewed files start
  collapsed unless they have comments still to send, and a file that changes after you
  reviewed it opens again.
- Clicking a file in the file panel scrolls to it, expanding it if needed. As you scroll, the
  file panel follows along.
- Find (Ctrl+F) searches every expanded file, moving on to the next file when one runs out of
  matches.

### Removed

- The previous and next file buttons next to the file name. Alt+↑ and Alt+↓ still move
  between files.

## [0.4.0] - 2026-10-12

### Changed

- Comments already sent to the agent no longer appear in the diff. A button next to the file
  name shows how many there are and opens them above the diff, with the lines they were
  about, for reference.

## [0.3.0] - 2026-10-12

### Added

- Ctrl+F finds text in the open file, highlighting every match; Enter and Shift+Enter move
  between them.

### Changed

- Clicking a line number no longer opens the comment box straight away: drag down the line
  numbers to select several lines, and the comment box opens when you release the button.
- The "Comment on lines …" button for selected text is gone: select lines from the line numbers
  instead, or select code and press Ctrl+M.
- Filtering the file list moved to Ctrl+Shift+F.
- Buttons show a pointing-hand cursor.
- Opening a file comment while scrolled down brings the comment box into view.

### Fixed

- Opening, closing and posting a comment no longer makes the diff flash or jump back to the
  top of the file for a moment.
- When the agent edits the file you are viewing, only the changed lines update: the view no
  longer flashes the top of the file, and what you are reading stays where it is.

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
