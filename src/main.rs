//! Claude Review: a native desktop app for reviewing the uncommitted changes
//! Claude Code makes, and handing review notes back to it.

mod application;
mod config;
mod core;
mod ui;

fn main() -> gtk::glib::ExitCode {
    application::run()
}
