//! Application setup: single instance, app-wide actions and styling.

use std::cell::RefCell;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::config;
use crate::ui::{self, Window};

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

pub fn run() -> glib::ExitCode {
    glib::set_application_name(config::APP_NAME);
    let app =
        adw::Application::builder().application_id(config::APP_ID).flags(gio::ApplicationFlags::HANDLES_OPEN).build();

    app.connect_startup(|app| {
        gtk::Window::set_default_icon_name(config::APP_ID);
        load_css();
        setup_actions(app);
    });
    app.connect_activate(|app| window(app).present());
    // `agent-review <folder>` opens that folder, in the running instance if any.
    app.connect_open(|app, files, _| {
        let window = window(app);
        if let Some(path) = files.first().and_then(FileExt::path) {
            window.open_project(path);
        }
        window.present();
    });
    app.run()
}

/// The main window, created on first use.
fn window(app: &adw::Application) -> Window {
    WINDOW.with_borrow_mut(|slot| slot.get_or_insert_with(|| Window::new(app)).clone())
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(ui::STYLE);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }

    // Comments live inside the monospace diff view: give them the interface font.
    let interface_font = gtk::Settings::default()
        .and_then(|s| s.gtk_font_name())
        .and_then(|name| gtk::pango::FontDescription::from_string(&name).family())
        .map_or_else(|| "sans-serif".to_owned(), |family| format!("\"{family}\""));
    let fonts = gtk::CssProvider::new();
    fonts.load_from_string(&format!(".comment-thread {{ font-family: {interface_font}; }}"));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &fonts, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

fn setup_actions(app: &adw::Application) {
    let quit = gio::ActionEntry::builder("quit").activate(|app: &adw::Application, _, _| app.quit()).build();
    let about = gio::ActionEntry::builder("about").activate(|app: &adw::Application, _, _| show_about(app)).build();
    app.add_action_entries([quit, about]);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
}

fn show_about(app: &adw::Application) {
    let about = adw::AboutDialog::builder()
        .application_name(config::APP_NAME)
        .application_icon(config::APP_ID)
        .version(config::VERSION)
        .comments("Review the changes Claude Code makes and send your notes back to it.")
        .website(config::REPOSITORY)
        .license_type(gtk::License::MitX11)
        .build();
    about.present(app.active_window().as_ref());
}
