# Agent Review

Native GTK 4 / libadwaita app (Rust) for reviewing uncommitted changes made by coding agents. See README.md.

## Commands

- `make check`: fmt check, clippy `-D warnings` (pedantic), tests, desktop/AppStream validation. Run before finishing any change.
- `cargo run -- <repo path>`: run against a repository.
- If the build dependencies live in a toolbox, run commands through `toolbox run -c <toolbox> …`.

## Changelog and releases

Describe every user-facing change in `CHANGELOG.md` under `## [Unreleased]` (`Added` / `Changed` / `Fixed` / `Removed`), written for users. Release with every change pushed to `main`: CI turns each new `Cargo.toml` version into a GitHub release, and a push without a version bump publishes nothing:

1. Bump `version` in `Cargo.toml` (semver: patch for fixes, minor for features, major for breaking changes) and run `cargo build` to update `Cargo.lock`.
2. Rename `## [Unreleased]` to `## [x.y.z] - YYYY-MM-DD`. It becomes the release notes.
3. Add `<release version="x.y.z" date="YYYY-MM-DD"/>` at the top of `<releases>` in `data/io.github.ralphnajm.AgentReview.metainfo.xml`.
4. `make check` verifies steps 2 and 3.

## Conventions

- Keep `src/core/` free of GTK so it stays unit-testable; UI code goes in `src/ui/`.
- Feedback reaches the agent only through the clipboard or the MCP server (`agent-review --mcp`, `src/core/mcp.rs`), which the agent pulls from: no terminal launching or agent-specific integration. Keep user-facing text agent-neutral.
- The app and the MCP server share the review queue on disk (`src/core/queue.rs`). Keep one writer per file: the app writes posted reviews, the server writes progress.
- Git is accessed through the `git` CLI with `--no-optional-locks` so refreshes never touch the index (which would retrigger the watcher).
- Review state is stored under XDG data dirs, never in the reviewed repository.
- Target GTK 4.14 / libadwaita 1.5 / GtkSourceView 5.12 (Ubuntu 24.04); don't enable newer feature flags.
- The UI follows a compact, flat style: one top bar with review state and actions on the right, file panel on the left, comments inline in the diff.
