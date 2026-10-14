//! The GTK user interface.

mod commit_picker;
mod diff_view;
mod file_list;
mod highlight;
mod project_switcher;
mod review_panel;
mod reviews_dialog;
mod scroller;
mod util;
mod window;

pub use window::Window;

/// Application stylesheet, loaded once at startup.
pub const STYLE: &str = include_str!("style.css");
