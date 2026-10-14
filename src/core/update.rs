//! Updates: [`check`] asks GitHub for the latest release, and [`run`]
//! downloads `scripts/update.sh` from the repository's main branch and runs
//! it. The script finds out how the running binary was installed, updates it
//! the same way, and starts the new version once this one has quit.

use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, bail};

use crate::config;

/// The script's exit status when the running version is already the latest.
const UP_TO_DATE: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Updated: the app must quit so the new version can start.
    Updated,
    UpToDate,
}

/// The latest release's version when it is newer than this one, else `None`.
pub fn check() -> Result<Option<String>> {
    let repo = config::REPOSITORY.trim_start_matches("https://github.com/");
    let output = Command::new("curl")
        .args(["-fsSL", "-H", "Accept: application/vnd.github+json"])
        .arg(format!("https://api.github.com/repos/{repo}/releases/latest"))
        .stdin(Stdio::null())
        .output()
        .context("Could not run curl. Is it installed?")?;
    if !output.status.success() {
        bail!("Could not reach GitHub: {}", last_line(&output.stderr).unwrap_or("curl failed"));
    }
    let latest = latest_version(&output.stdout).context("Could not find the latest release")?;
    Ok(is_newer(&latest, config::VERSION).then_some(latest))
}

/// The version of the release GitHub's API describes in `json`.
fn latest_version(json: &[u8]) -> Option<String> {
    let release: serde_json::Value = serde_json::from_slice(json).ok()?;
    let tag = release.get("tag_name")?.as_str()?;
    Some(tag.strip_prefix('v').unwrap_or(tag).to_owned())
}

/// Whether version `a` comes after `b`, comparing their numbers in turn.
fn is_newer(a: &str, b: &str) -> bool {
    let numbers = |v: &str| v.split('.').map(|n| n.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    numbers(a) > numbers(b)
}

/// Where the update script is downloaded from.
pub fn script_url() -> String {
    let repo = config::REPOSITORY.trim_start_matches("https://github.com/");
    format!("https://raw.githubusercontent.com/{repo}/main/scripts/update.sh")
}

/// Downloads and runs the update script. Blocks until it is done, which can
/// take minutes when the update is built from source.
pub fn run() -> Result<Outcome> {
    let exe = std::env::current_exe().context("Could not find the running binary")?;
    let download = Command::new("curl")
        .args(["-fsSL", &script_url()])
        .stdin(Stdio::null())
        .output()
        .context("Could not run curl. Is it installed?")?;
    if !download.status.success() {
        bail!("Could not download the update script: {}", last_line(&download.stderr).unwrap_or("curl failed"));
    }
    let script = String::from_utf8(download.stdout).context("The update script is not text")?;
    let output = Command::new("sh")
        .arg("-c")
        .arg(script)
        .arg("update.sh")
        .arg(config::VERSION)
        .arg(exe)
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .output()
        .context("Could not run the update script")?;
    outcome(&output)
}

fn outcome(output: &Output) -> Result<Outcome> {
    match output.status.code() {
        Some(0) => Ok(Outcome::Updated),
        Some(UP_TO_DATE) => Ok(Outcome::UpToDate),
        _ => bail!("{}", last_line(&output.stderr).unwrap_or("the update script failed")),
    }
}

/// The last non-blank line of a command's output, which explains a failure.
fn last_line(output: &[u8]) -> Option<&str> {
    std::str::from_utf8(output).ok()?.lines().map(str::trim).rfind(|line| !line.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_sh(script: &str) -> Output {
        Command::new("sh").args(["-c", script]).output().unwrap()
    }

    #[test]
    fn script_comes_from_the_main_branch() {
        assert_eq!(script_url(), "https://raw.githubusercontent.com/ralphnajm/agent-review/main/scripts/update.sh");
    }

    #[test]
    fn compares_release_versions() {
        assert_eq!(latest_version(br#"{"tag_name": "v0.10.0", "name": "0.10.0"}"#).as_deref(), Some("0.10.0"));
        assert_eq!(latest_version(b"{}"), None);
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(is_newer("1.0.0", "0.99.1"));
        assert!(!is_newer("0.9.0", "0.9.0"));
        assert!(!is_newer("0.8.2", "0.9.0"));
    }

    #[test]
    fn exit_status_tells_the_outcome() {
        assert_eq!(outcome(&run_sh("exit 0")).unwrap(), Outcome::Updated);
        assert_eq!(outcome(&run_sh("exit 3")).unwrap(), Outcome::UpToDate);
    }

    #[test]
    fn failure_reports_the_last_line_of_stderr() {
        let err =
            outcome(&run_sh("echo 'Downloading…' >&2; echo 'Update cancelled' >&2; echo >&2; exit 1")).unwrap_err();
        assert_eq!(err.to_string(), "Update cancelled");
        let err = outcome(&run_sh("exit 100")).unwrap_err();
        assert_eq!(err.to_string(), "the update script failed");
    }
}
