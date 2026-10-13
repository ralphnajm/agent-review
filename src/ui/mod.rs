//! The GTK user interface.

mod diff_view;
mod file_list;
mod project_switcher;
mod review_panel;
mod reviews_dialog;
mod scroller;
mod util;
mod window;

pub use window::Window;

/// Application stylesheet, loaded once at startup.
pub const STYLE: &str = include_str!("style.css");
