//! The GTK user interface.

mod diff_view;
mod file_list;
mod highlight;
mod product_view;
mod project_switcher;
mod review_panel;
mod reviews_dialog;
mod scroller;
mod skill_dialog;
mod util;
mod version_picker;
mod whiteboard;
mod window;

pub use window::Window;

/// Application stylesheet, loaded once at startup.
pub const STYLE: &str = include_str!("style.css");
