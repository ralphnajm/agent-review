//! Self-update: downloads `scripts/update.sh` from the repository's main
//! branch and runs it. The script finds out how the running binary was
//! installed, updates it the same way, and starts the new version once this
//! one has quit.

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
