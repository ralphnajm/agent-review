# Changelog

All notable changes to Agent Review are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/). Each version's section becomes the
notes of its GitHub release.

## [0.10.0] - 2026-10-14

### Added

- Review commits, not just uncommitted changes: the button above the file panel opens the repository's history as
  a graph of its branches and merges. Pick a commit to see what it changed, with its message beside the file list,
  then comment and tick files off as usual. Each commit keeps its own comments and reviewed files, and posted or
  copied feedback tells your agent which commit it is about. The top row of the history, or Alt+1, 2 or 3, takes
  you back to the uncommitted changes. Ctrl+H opens the history.

### Changed

- **Self-Update** is now **Check for Updates**. It opens a dialog that checks for a newer release and tells you
  when you already have the latest one. When there is a newer one, it asks before updating.

## [0.9.0] - 2026-10-14

### Added

- **Self-Update** in the main menu updates Agent Review to the latest release the way you installed it: with apt
  or dnf for the `.deb` and `.rpm` packages (asking for your password), or by building the release's source for
  `make install`. Agent Review then restarts as the new version and reopens your project.

### Fixed

- Agent Review no longer crashes when a file you marked reviewed changes and opens again for another look. The
  reopened file also no longer takes the keyboard focus.

## [0.8.2] - 2026-10-14

### Fixed

- Syntax highlighting reads the old and new versions of a file each on their own, so a string or comment changed
  on one side no longer miscolours the code around it, in either view.
- Side by side, comments sit below the side they are on, rather than always below the old one.
- When the repository changes while you are reviewing, the line at the top of the screen stays there, even when
  lines are added or removed above it in the same file or in files above.

## [0.8.1] - 2026-10-13

### Fixed

- Checking a file's Reviewed box collapses it in place instead of also scrolling to the next file.

## [0.8.0] - 2026-10-13

### Added

- **Side by Side** in the main menu shows the old and new versions of each file beside each other, with long
  lines wrapped. Selecting text on the old side and pressing Ctrl+M comments on the removed lines.
- A file's header shows how many comments it has that are not sent yet, next to its comment button.

### Changed

- The line counts in a file's header are green and red, as in the file panel.
- A file's header no longer says whether it is staged.

### Fixed

- Ticking **Reviewed**, or clicking a header, now collapses the file when you have scrolled into it and its header
  is stuck at the top. Before, the file opened again straight away.

## [0.7.1] - 2026-10-13

### Changed

- The button for commenting on lines shows a plus again: press it to comment on the line, or drag it to pick more lines.
- The **Post Review** button no longer shows an icon.

### Fixed

- Long diffs no longer slow the app down: with thousands of lines open, scrolling, selecting and resizing the window
  stay smooth, as only the lines on screen are laid out and drawn.

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
