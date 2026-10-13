//! UI-independent logic: git access, diff parsing, review state, feedback
//! generation.

pub mod align;
pub mod diff;
pub mod feedback;
pub mod git;
pub mod hash;
pub mod mcp;
pub mod queue;
pub mod review;
pub mod status;
pub mod store;
pub mod watcher;

/// Current time as Unix seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
