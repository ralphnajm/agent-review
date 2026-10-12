//! Small widget helpers shared across the UI.

use gtk::prelude::*;

/// A label with the given CSS classes.
pub fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    for class in classes {
        label.add_css_class(class);
    }
    label
}

/// Human friendly "5 minutes ago" for a Unix timestamp in seconds.
pub fn relative_time(timestamp: i64, now: i64) -> String {
    let delta = (now - timestamp).max(0);
    match delta {
        0..60 => "just now".to_owned(),
        60..3_600 => plural(delta / 60, "minute"),
        3_600..86_400 => plural(delta / 3_600, "hour"),
        _ => plural(delta / 86_400, "day"),
    }
}

fn plural(n: i64, unit: &str) -> String {
    if n == 1 { format!("1 {unit} ago") } else { format!("{n} {unit}s ago") }
}

/// Splits a path into file name and parent directory.
pub fn split_path(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((dir, name)) => (name, dir),
        None => (path, ""),
    }
}

/// Replaces `$HOME` with `~` for display.
pub fn tilde(path: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_relative_times() {
        assert_eq!(relative_time(100, 110), "just now");
        assert_eq!(relative_time(0, 60), "1 minute ago");
        assert_eq!(relative_time(0, 7_200), "2 hours ago");
        assert_eq!(relative_time(0, 3 * 86_400), "3 days ago");
    }

    #[test]
    fn splits_paths() {
        assert_eq!(split_path("src/ui/mod.rs"), ("mod.rs", "src/ui"));
        assert_eq!(split_path("README.md"), ("README.md", ""));
    }
}
