//! The agent skill that teaches coding agents the review workflow, built into
//! the app from `skills/agent-review/SKILL.md` so the app installs the skill
//! that matches it. A skill is a folder holding a `SKILL.md`, in a skills
//! folder the agent reads. Wherever it is installed, the app keeps it up to
//! date when it starts (see [`update`]).

use std::path::{Path, PathBuf};

/// The skill's folder name, which matches the `name` in its frontmatter.
pub const NAME: &str = "agent-review";

pub const SKILL: &str = include_str!("../../skills/agent-review/SKILL.md");

/// A skills folder agents read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub title: &'static str,
    /// Which agents read it.
    pub agents: &'static str,
    pub dir: PathBuf,
}

/// The skills folders for all projects of the agents most people use.
pub fn locations() -> Vec<Location> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    vec![
        Location { title: "Claude Code", agents: "Claude Code", dir: home.join(".claude/skills") },
        Location {
            title: "Standard Agents Folder",
            agents: "Codex, Cline, Zed, Warp and other agents",
            dir: home.join(".agents/skills"),
        },
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Missing,
    /// Installed, but not the version built into this app.
    Outdated,
    Installed,
}

/// Whether the skill is installed in the skills folder `dir`.
pub fn state(dir: &Path) -> State {
    match std::fs::read_to_string(file(dir)) {
        Ok(text) if text == SKILL => State::Installed,
        Ok(_) => State::Outdated,
        Err(_) => State::Missing,
    }
}

/// Installs the skill in the skills folder `dir`, replacing any other
/// version of it; returns the file written.
pub fn install(dir: &Path) -> std::io::Result<PathBuf> {
    let file = file(dir);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&file, SKILL)?;
    Ok(file)
}

/// Brings the skill up to date in those of the skills folders `dirs` that
/// have an older version of it; returns the folders updated. Never installs
/// it where it isn't.
pub fn update<'a>(dirs: impl IntoIterator<Item = &'a Path>) -> Vec<PathBuf> {
    dirs.into_iter()
        .filter(|dir| state(dir) == State::Outdated)
        .filter(|dir| install(dir).is_ok())
        .map(Path::to_path_buf)
        .collect()
}

fn file(dir: &Path) -> PathBuf {
    dir.join(NAME).join("SKILL.md")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_and_updates_the_skill() {
        let dir = tempfile::tempdir().unwrap();
        let skills = dir.path().join("skills");
        assert_eq!(state(&skills), State::Missing);
        let file = install(&skills).unwrap();
        assert_eq!(file, skills.join("agent-review/SKILL.md"));
        assert_eq!(state(&skills), State::Installed);
        std::fs::write(&file, "an older version").unwrap();
        assert_eq!(state(&skills), State::Outdated);
        install(&skills).unwrap();
        assert_eq!(state(&skills), State::Installed);
    }

    #[test]
    fn updates_only_older_versions() {
        let dir = tempfile::tempdir().unwrap();
        let (old, current, none) = (dir.path().join("old"), dir.path().join("current"), dir.path().join("none"));
        install(&old).unwrap();
        std::fs::write(file(&old), "an older version").unwrap();
        install(&current).unwrap();
        assert_eq!(update([old.as_path(), current.as_path(), none.as_path()]), std::slice::from_ref(&old));
        assert_eq!(state(&old), State::Installed);
        assert_eq!(state(&none), State::Missing);
    }

    #[test]
    fn the_skill_is_named_after_its_folder() {
        assert!(SKILL.starts_with(&format!("---\nname: {NAME}\n")));
    }
}
