//! Persistence of settings and review state under the XDG base directories.
//!
//! Review state is deliberately kept outside the repository so reviewing
//! never adds files to the working tree being reviewed.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::hash;
use crate::core::review::ReviewState;
use crate::core::status::DiffMode;

const APP_DIR: &str = "agent-review";
const MAX_RECENT_PROJECTS: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentProject {
    pub path: PathBuf,
    pub opened_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

impl Default for WindowGeometry {
    fn default() -> Self {
        Self { width: 1280, height: 820, maximized: false }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub recent: Vec<RecentProject>,
    pub last_project: Option<PathBuf>,
    pub diff_mode: DiffMode,
    /// Whether diffs show the old and new sides beside each other.
    pub side_by_side: bool,
    pub show_sidebar: bool,
    pub sidebar_width: i32,
    pub window: WindowGeometry,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            recent: Vec::new(),
            last_project: None,
            diff_mode: DiffMode::All,
            side_by_side: false,
            show_sidebar: true,
            sidebar_width: 320,
            window: WindowGeometry::default(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        read_json(&config_dir().join("settings.json")).unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        write_json(&config_dir().join("settings.json"), self)
    }

    /// Moves `path` to the top of the recent projects.
    pub fn touch_project(&mut self, path: &Path, now: i64) {
        self.recent.retain(|p| p.path != path);
        self.recent.insert(0, RecentProject { path: path.to_path_buf(), opened_at: now });
        self.recent.truncate(MAX_RECENT_PROJECTS);
        self.last_project = Some(path.to_path_buf());
    }

    pub fn forget_project(&mut self, path: &Path) {
        self.recent.retain(|p| p.path != path);
        if self.last_project.as_deref() == Some(path) {
            self.last_project = None;
        }
    }
}

/// Review state for the project at `path`: of its uncommitted changes, or of `commit`.
pub fn load_review(path: &Path, commit: Option<&str>) -> ReviewState {
    read_json(&review_file(path, commit)).unwrap_or_default()
}

pub fn save_review(path: &Path, commit: Option<&str>, review: &ReviewState) -> std::io::Result<()> {
    write_json(&review_file(path, commit), review)
}

fn review_file(path: &Path, commit: Option<&str>) -> PathBuf {
    let key = hash::hex(path.as_os_str().as_encoded_bytes());
    let reviews = data_dir().join("reviews");
    match commit {
        None => reviews.join(format!("{key}.json")),
        Some(id) => reviews.join("commits").join(key).join(format!("{id}.json")),
    }
}

/// Where the reviews posted for the repository at `root` are queued.
pub fn queue_dir(root: &Path) -> PathBuf {
    let key = hash::hex(root.as_os_str().as_encoded_bytes());
    data_dir().join("queue").join(key)
}

fn config_dir() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join(APP_DIR)
}

fn data_dir() -> PathBuf {
    xdg_dir("XDG_DATA_HOME", ".local/share").join(APP_DIR)
}

fn xdg_dir(variable: &str, fallback: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(fallback))
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Writes atomically so a crash never leaves a truncated file behind.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let json = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_projects_are_ordered_and_deduplicated() {
        let mut settings = Settings::default();
        settings.touch_project(Path::new("/a"), 1);
        settings.touch_project(Path::new("/b"), 2);
        settings.touch_project(Path::new("/a"), 3);
        let paths: Vec<_> = settings.recent.iter().map(|p| p.path.clone()).collect();
        assert_eq!(paths, [PathBuf::from("/a"), PathBuf::from("/b")]);
        settings.forget_project(Path::new("/a"));
        assert_eq!(settings.last_project, None);
        assert_eq!(settings.recent.len(), 1);
    }

    #[test]
    fn json_round_trip_tolerates_missing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nested/settings.json");
        let settings = Settings { sidebar_width: 280, ..Settings::default() };
        write_json(&file, &settings).unwrap();
        assert_eq!(read_json::<Settings>(&file), Some(settings));

        std::fs::write(&file, r#"{"show_sidebar": false}"#).unwrap();
        let partial: Settings = read_json(&file).unwrap();
        assert!(!partial.show_sidebar);
        assert_eq!(partial.sidebar_width, 320);
        assert_eq!(partial.diff_mode, DiffMode::All);
    }
}
