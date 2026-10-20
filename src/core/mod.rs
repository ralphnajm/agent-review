//! UI-independent logic: git access, diff parsing, review state, feedback
//! generation, and the review queue the app shares with the MCP server.

pub mod align;
pub mod changes;
pub mod diff;
pub mod feedback;
pub mod git;
pub mod hash;
pub mod mcp;
pub mod queue;
pub mod review;
pub mod skill;
pub mod split;
pub mod store;
pub mod update;
pub mod watcher;

/// Current time as Unix milliseconds.
pub fn now_millis() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

/// Current time as Unix seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
