//! Agent Review: a native desktop app for reviewing the uncommitted changes
//! coding agents make, and handing review notes back to them.

mod application;
mod config;
mod core;
mod ui;

fn main() -> gtk::glib::ExitCode {
    // `agent-review --mcp` is the MCP server agents pull posted reviews from; it needs no display.
    // `agent-review --wait [path]` waits for the user to post a review, for agents to run in the background.
    let mut args = std::env::args_os().skip(1);
    let headless = match args.next().as_deref().and_then(std::ffi::OsStr::to_str) {
        Some("--mcp") => Some(core::mcp::serve()),
        Some("--wait") => Some(core::wait::run(args.next().map(std::path::PathBuf::from))),
        _ => None,
    };
    match headless {
        Some(Ok(())) => gtk::glib::ExitCode::SUCCESS,
        Some(Err(err)) => {
            eprintln!("agent-review: {err:#}");
            gtk::glib::ExitCode::FAILURE
        }
        None => application::run(),
    }
}
