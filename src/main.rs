//! Agent Review: a native desktop app for reviewing the uncommitted changes
//! coding agents make, and handing review notes back to them.

mod application;
mod config;
mod core;
mod ui;

fn main() -> gtk::glib::ExitCode {
    application::run()
}
