//! Agent Review: a native desktop app for reviewing the uncommitted changes
//! coding agents make, and handing review notes back to them.

mod application;
mod config;
mod core;
mod ui;

fn main() -> gtk::glib::ExitCode {
    // `agent-review --mcp` is the MCP server agents pull posted reviews from; it needs no display.
    if std::env::args().nth(1).as_deref() == Some("--mcp") {
        if let Err(err) = core::mcp::serve() {
            eprintln!("agent-review: {err:#}");
            return gtk::glib::ExitCode::FAILURE;
        }
        return gtk::glib::ExitCode::SUCCESS;
    }
    application::run()
}
