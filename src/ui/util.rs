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

/// Shows the pointing hand over the buttons in `window`, including those GTK
/// and libadwaita create themselves (header bar, dialogs, toasts, popovers):
/// each gets the cursor the first time the pointer reaches it.
pub fn use_pointer_cursor(window: &impl IsA<gtk::Widget>) {
    let motion = gtk::EventControllerMotion::new();
    motion.set_propagation_phase(gtk::PropagationPhase::Capture);
    motion.connect_enter(|motion, _, _| set_pointer_cursor(motion));
    motion.connect_motion(|motion, _, _| set_pointer_cursor(motion));
    window.add_controller(motion);
}

fn set_pointer_cursor(motion: &gtk::EventControllerMotion) {
    // The event may come from a popover, which has its own surface.
    let Some(event) = motion.current_event() else { return };
    let (Some(native), Some((x, y))) = (event.surface().and_then(|s| gtk::Native::for_surface(&s)), event.position())
    else {
        return;
    };
    if let Some(button) = button_at(&native, x, y)
        && button.cursor().is_none()
    {
        button.set_cursor_from_name(Some("pointer"));
    }
}

/// The button under surface coordinates `x`, `y` of `native`.
pub fn button_at(native: &gtk::Native, x: f64, y: f64) -> Option<gtk::Widget> {
    let (dx, dy) = native.surface_transform();
    let target = native.pick(x - dx, y - dy, gtk::PickFlags::DEFAULT)?;
    std::iter::successors(Some(target), WidgetExt::parent)
        .find(|w| w.is::<gtk::Button>() || w.is::<gtk::CheckButton>() || w.is::<gtk::Switch>())
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
