//! Drawing over a screenshot of the product, on Excalidraw: a marker, arrows,
//! boxes, sticky notes and text, which can be selected, moved, resized,
//! restyled and deleted. The user's comment sends the result to the agent as
//! one image.
//!
//! Excalidraw runs in a web view, on a page embedded in the app (built from
//! `whiteboard/` into `data/whiteboard`) and served as
//! `agent-review://whiteboard/`. The page's own toolbar is hidden: the tools
//! are in the app's bar, which drives the page through `window.board` and
//! hears back from it through the `whiteboard` script message handler.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{cairo, gdk, gio, glib};
use serde::Deserialize;
use webkit6::prelude::*;

mod assets {
    include!(concat!(env!("OUT_DIR"), "/whiteboard.rs"));
}

const SCHEME: &str = "agent-review";
const PAGE: &str = "agent-review://whiteboard/index.html";

/// What a click or drag on the whiteboard does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// Selects, moves and resizes what's drawn; a double click edits text.
    Select,
    Marker,
    Arrow,
    Box,
    /// A yellow note, placed with a click, to type on.
    Sticky,
    /// Text on its own, placed with a click.
    Text,
}

impl Tool {
    const ALL: [Self; 6] = [Self::Select, Self::Marker, Self::Arrow, Self::Box, Self::Sticky, Self::Text];

    /// Its name on the page.
    fn name(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::Marker => "marker",
            Self::Arrow => "arrow",
            Self::Box => "box",
            Self::Sticky => "sticky",
            Self::Text => "text",
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            Self::Select => "Select: move, resize or restyle what you drew; double-click to edit text",
            Self::Marker => "Marker",
            Self::Arrow => "Arrow",
            Self::Box => "Box",
            Self::Sticky => "Sticky Note: click where it goes, then type",
            Self::Text => "Text: click where it goes, then type",
        }
    }
}

/// The colours of the marker, arrows, boxes and text, by name.
pub const COLORS: [(&str, &str); 4] =
    [("Red", "#e01b24"), ("Blue", "#1c71d8"), ("Green", "#26a269"), ("Black", "#1a1a1a")];

/// What the page last said of the board.
#[derive(Debug, Clone, Copy, Default)]
struct State {
    drawing: bool,
    undo: bool,
    redo: bool,
}

/// A message from the page: `{ "ready": true }` once loaded, then the state
/// of the board whenever it changes, or a key's action.
#[derive(Debug, Deserialize)]
struct Message {
    #[serde(default)]
    ready: bool,
    /// `done` for Ctrl+Enter, `discard` for Esc.
    action: Option<String>,
    tool: Option<String>,
    #[serde(default)]
    drawing: bool,
    #[serde(default)]
    undo: bool,
    #[serde(default)]
    redo: bool,
}

/// The drawing, from the page.
#[derive(Debug, Deserialize)]
struct Exported {
    /// The screenshot drawn on, as a base64 PNG.
    png: String,
    texts: Vec<String>,
}

type ToolCallback = Box<dyn Fn(Tool)>;

#[derive(Clone)]
pub struct Whiteboard(Rc<Inner>);

struct Inner {
    root: gtk::Box,
    /// Created with the first screenshot, as web views are costly.
    view: RefCell<Option<webkit6::WebView>>,
    /// Whether the page can take a screenshot.
    ready: Cell<bool>,
    /// A screenshot waiting for the page, as a PNG `data:` URL.
    queued: RefCell<Option<String>>,
    tool: Cell<Tool>,
    state: Cell<State>,
    on_change: RefCell<Vec<Box<dyn Fn()>>>,
    on_tool: RefCell<Vec<ToolCallback>>,
    on_done: RefCell<Option<Box<dyn Fn()>>>,
    on_discard: RefCell<Option<Box<dyn Fn()>>>,
}

impl Whiteboard {
    pub fn new() -> Self {
        register_scheme();
        let root = gtk::Box::builder().hexpand(true).vexpand(true).build();
        root.add_css_class("whiteboard");
        let this = Self(Rc::new(Inner {
            root,
            view: RefCell::default(),
            ready: Cell::new(false),
            queued: RefCell::default(),
            tool: Cell::new(Tool::Marker),
            state: Cell::default(),
            on_change: RefCell::default(),
            on_tool: RefCell::default(),
            on_done: RefCell::default(),
            on_discard: RefCell::default(),
        }));
        let weak = this.weak();
        adw::StyleManager::default().connect_dark_notify(move |style| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).run(&format!("board.setDark({})", style.is_dark()));
            }
        });
        this
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.0.root
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    /// Called whenever the drawing, or what Undo and Redo can do, changes.
    pub fn connect_change(&self, callback: impl Fn() + 'static) {
        self.0.on_change.borrow_mut().push(Box::new(callback));
    }

    /// Called when the tool changes on the page, as when drawing a box goes
    /// back to selecting.
    fn connect_tool(&self, callback: impl Fn(Tool) + 'static) {
        self.0.on_tool.borrow_mut().push(Box::new(callback));
    }

    /// Called on Ctrl+Enter on the whiteboard, to add the drawing to the review.
    pub fn connect_done(&self, callback: impl Fn() + 'static) {
        self.0.on_done.replace(Some(Box::new(callback)));
    }

    /// Called on Esc on the whiteboard, when there's nothing else to stop, to
    /// throw the drawing away.
    pub fn connect_discard(&self, callback: impl Fn() + 'static) {
        self.0.on_discard.replace(Some(Box::new(callback)));
    }

    pub fn set_tool(&self, tool: Tool) {
        if self.0.tool.replace(tool) != tool {
            self.run(&format!("board.setTool({:?})", tool.name()));
        }
    }

    /// What's drawn next takes `color`, as `#rrggbb`, and so does what's selected.
    pub fn set_color(&self, color: &str) {
        self.run(&format!("board.setColor({color:?})"));
    }

    /// Starts over on `screenshot`, with the marker.
    pub fn set_image(&self, screenshot: &gdk::Texture) {
        let png = format!("data:image/png;base64,{}", glib::base64_encode(&screenshot.save_to_png_bytes()));
        self.0.state.set(State::default());
        self.0.tool.set(Tool::Marker);
        self.tool_changed(Tool::Marker);
        self.changed();
        self.view();
        if self.0.ready.get() {
            self.load(png);
        } else {
            self.0.queued.replace(Some(png));
        }
    }

    /// Puts the keyboard on the whiteboard.
    pub fn focus(&self) {
        if let Some(view) = self.0.view.borrow().as_ref() {
            view.grab_focus();
        }
    }

    /// Whether the keyboard is on the whiteboard, whose page then handles the keys.
    pub fn has_focus(&self) -> bool {
        self.0.view.borrow().as_ref().is_some_and(WidgetExt::has_focus)
    }

    /// Whether the user drew anything.
    pub fn has_drawing(&self) -> bool {
        self.0.state.get().drawing
    }

    fn can_undo(&self) -> bool {
        self.0.state.get().undo
    }

    fn can_redo(&self) -> bool {
        self.0.state.get().redo
    }

    pub fn undo(&self) {
        self.run("board.undo()");
    }

    pub fn redo(&self) {
        self.run("board.redo()");
    }

    /// Saves the screenshot with the drawing on it as a PNG, scaled down to at
    /// most `max_width` pixels wide; returns the text written on it, in order.
    pub async fn export(&self, path: &Path, max_width: i32) -> Result<Vec<String>, String> {
        let view = self.0.view.borrow().clone().ok_or("There is no drawing")?;
        let arguments = glib::VariantDict::new(None);
        arguments.insert("maxWidth", max_width);
        let value = view
            .call_async_javascript_function_future(
                "return await board.export(maxWidth);",
                Some(&arguments.end()),
                None,
                None,
            )
            .await
            .map_err(|e| e.to_string())?;
        let exported: Exported = serde_json::from_str(&value.to_str()).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, glib::base64_decode(&exported.png)).map_err(|e| e.to_string())?;
        Ok(exported.texts)
    }

    fn changed(&self) {
        for callback in self.0.on_change.borrow().iter() {
            callback();
        }
    }

    fn tool_changed(&self, tool: Tool) {
        for callback in self.0.on_tool.borrow().iter() {
            callback(tool);
        }
    }

    /// Runs `script` on the page, once it's ready: before, there's nothing to act on.
    fn run(&self, script: &str) {
        let Some(view) = self.0.view.borrow().clone().filter(|_| self.0.ready.get()) else { return };
        view.evaluate_javascript(script, None, None, None::<&gio::Cancellable>, |result| {
            if let Err(err) = result {
                eprintln!("agent-review: the whiteboard failed: {err}");
            }
        });
    }

    fn load(&self, png: String) {
        let Some(view) = self.0.view.borrow().clone() else { return };
        let arguments = glib::VariantDict::new(None);
        arguments.insert("png", png);
        arguments.insert("tool", self.0.tool.get().name());
        glib::spawn_future_local(async move {
            let loaded = view
                .call_async_javascript_function_future(
                    "await board.load(png, tool);",
                    Some(&arguments.end()),
                    None,
                    None,
                )
                .await;
            if let Err(err) = loaded {
                eprintln!("agent-review: could not put the screenshot on the whiteboard: {err}");
            }
        });
    }

    fn receive(&self, message: &str) {
        let message: Message = match serde_json::from_str(message) {
            Ok(message) => message,
            Err(err) => {
                eprintln!("agent-review: unexpected message from the whiteboard: {err}");
                return;
            }
        };
        if message.ready {
            self.0.ready.set(true);
            self.run(&format!("board.setDark({})", adw::StyleManager::default().is_dark()));
            if let Some(png) = self.0.queued.take() {
                self.load(png);
            }
            return;
        }
        if let Some(action) = message.action {
            let callback = match action.as_str() {
                "done" => &self.0.on_done,
                _ => &self.0.on_discard,
            };
            if let Some(callback) = callback.borrow().as_ref() {
                callback();
            }
            return;
        }
        let tool = message.tool.as_deref().and_then(|name| Tool::ALL.into_iter().find(|t| t.name() == name));
        if let Some(tool) = tool.filter(|&tool| self.0.tool.replace(tool) != tool) {
            self.tool_changed(tool);
        }
        self.0.state.set(State { drawing: message.drawing, undo: message.undo, redo: message.redo });
        self.changed();
    }

    /// The web view, created on first use.
    fn view(&self) -> webkit6::WebView {
        if let Some(view) = self.0.view.borrow().as_ref() {
            return view.clone();
        }
        let content = webkit6::UserContentManager::new();
        content.register_script_message_handler("whiteboard", None);
        let weak = self.weak();
        content.connect_script_message_received(Some("whiteboard"), move |_, value| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).receive(&value.to_str());
            }
        });
        let view = webkit6::WebView::builder()
            // Nothing on the whiteboard is kept.
            .network_session(&webkit6::NetworkSession::new_ephemeral())
            .user_content_manager(&content)
            .hexpand(true)
            .vexpand(true)
            .build();
        if let Some(settings) = WebViewExt::settings(&view) {
            settings.set_enable_developer_extras(cfg!(debug_assertions));
            settings.set_enable_write_console_messages_to_stdout(cfg!(debug_assertions));
        }
        // Excalidraw has its own menu.
        view.connect_context_menu(|_, _, _| true);
        let weak = self.weak();
        view.connect_web_process_terminated(move |view, reason| {
            eprintln!("agent-review: the whiteboard stopped: {reason:?}");
            if let Some(inner) = weak.upgrade() {
                inner.ready.set(false);
                view.load_uri(PAGE);
            }
        });
        view.load_uri(PAGE);
        self.0.root.append(&view);
        self.0.view.replace(Some(view.clone()));
        view
    }
}

/// Serves the whiteboard's page as `agent-review://whiteboard/`.
fn register_scheme() {
    thread_local!(static REGISTERED: Cell<bool> = const { Cell::new(false) });
    if REGISTERED.replace(true) {
        return;
    }
    let Some(context) = webkit6::WebContext::default() else { return };
    if let Some(security) = context.security_manager() {
        security.register_uri_scheme_as_secure(SCHEME);
        security.register_uri_scheme_as_cors_enabled(SCHEME);
    }
    context.register_uri_scheme(SCHEME, |request| {
        let path = request.path().unwrap_or_default();
        let path = path.trim_start_matches('/');
        match assets::WHITEBOARD.iter().find(|(name, _)| *name == path) {
            Some(&(_, data)) => {
                let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_static(data));
                request.finish(&stream, i64::try_from(data.len()).unwrap_or(-1), Some(content_type(path)));
            }
            None => request.finish_error(&mut glib::Error::new(gio::IOErrorEnum::NotFound, &format!("No {path}"))),
        }
    });
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// The whiteboard's tools, colours, Undo and Redo, in a bar of icons.
pub fn build_tools(board: &Whiteboard) -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    bar.add_css_class("whiteboard-tools");
    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    let mut buttons: Vec<(Tool, gtk::ToggleButton)> = Vec::new();
    for tool in Tool::ALL {
        let icon = gtk::DrawingArea::builder().content_width(18).content_height(18).build();
        icon.set_draw_func(move |area, cr, width, height| draw_tool_icon(area, cr, tool, width, height));
        let button = gtk::ToggleButton::builder().child(&icon).tooltip_text(tool.tooltip()).build();
        button.add_css_class("flat");
        button.add_css_class("tool-button");
        button.set_group(buttons.first().map(|(_, first)| first));
        button.set_active(tool == Tool::Marker);
        let board = board.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                board.set_tool(tool);
            }
        });
        tools.append(&button);
        buttons.push((tool, button));
    }
    board.connect_tool(move |tool| {
        if let Some((_, button)) = buttons.iter().find(|(t, _)| *t == tool) {
            button.set_active(true);
        }
    });
    bar.append(&tools);
    bar.append(&gtk::Separator::new(gtk::Orientation::Vertical));

    let colors = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    colors.set_margin_start(4);
    colors.set_margin_end(4);
    let mut first: Option<gtk::ToggleButton> = None;
    for (name, color) in COLORS {
        let button = gtk::ToggleButton::builder().tooltip_text(name).valign(gtk::Align::Center).build();
        button.add_css_class("swatch");
        button.add_css_class(&format!("swatch-{}", name.to_lowercase()));
        button.set_group(first.as_ref());
        let board = board.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                board.set_color(color);
            }
        });
        colors.append(&button);
        first.get_or_insert(button);
    }
    if let Some(first) = first {
        first.set_active(true);
    }
    bar.append(&colors);
    bar.append(&gtk::Separator::new(gtk::Orientation::Vertical));

    let undo = gtk::Button::builder().icon_name("edit-undo-symbolic").tooltip_text("Undo (Ctrl+Z)").build();
    let redo = gtk::Button::builder().icon_name("edit-redo-symbolic").tooltip_text("Redo (Ctrl+Shift+Z)").build();
    for (button, redo_it) in [(&undo, false), (&redo, true)] {
        button.add_css_class("flat");
        button.add_css_class("tool-button");
        let weak = board.weak();
        button.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                if redo_it { Whiteboard(inner).redo() } else { Whiteboard(inner).undo() }
            }
        });
        bar.append(button);
    }
    let (weak, buttons) = (board.weak(), (undo, redo));
    let update = move || {
        if let Some(inner) = weak.upgrade() {
            let board = Whiteboard(inner);
            buttons.0.set_sensitive(board.can_undo());
            buttons.1.set_sensitive(board.can_redo());
        }
    };
    update();
    board.connect_change(update);
    bar
}

/// Draws the icon of `tool` in the button's text colour.
fn draw_tool_icon(area: &gtk::DrawingArea, cr: &cairo::Context, tool: Tool, width: i32, height: i32) {
    let color = area.color();
    cr.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        f64::from(color.alpha()),
    );
    // Laid out on an 18 × 18 grid.
    let scale = f64::from(width.min(height)) / 18.0;
    cr.scale(scale, scale);
    cr.set_line_width(1.8);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    match tool {
        Tool::Select => {
            cr.move_to(4.5, 2.5);
            cr.line_to(4.5, 14.5);
            cr.line_to(7.6, 11.6);
            cr.line_to(9.8, 16.0);
            cr.line_to(11.8, 15.0);
            cr.line_to(9.6, 10.7);
            cr.line_to(13.8, 10.4);
            cr.close_path();
        }
        Tool::Marker => {
            cr.move_to(2.5, 13.0);
            cr.curve_to(5.0, 5.0, 7.5, 5.0, 9.0, 9.5);
            cr.curve_to(10.5, 14.0, 13.0, 14.0, 15.5, 5.0);
        }
        Tool::Arrow => {
            cr.move_to(3.0, 15.0);
            cr.line_to(15.0, 3.0);
            cr.move_to(8.0, 3.0);
            cr.line_to(15.0, 3.0);
            cr.line_to(15.0, 10.0);
        }
        Tool::Box => cr.rectangle(2.5, 4.0, 13.0, 10.0),
        Tool::Sticky => {
            cr.move_to(3.0, 3.0);
            cr.line_to(15.0, 3.0);
            cr.line_to(15.0, 11.0);
            cr.line_to(11.0, 15.0);
            cr.line_to(3.0, 15.0);
            cr.close_path();
            cr.move_to(15.0, 11.0);
            cr.line_to(11.0, 11.0);
            cr.line_to(11.0, 15.0);
        }
        Tool::Text => {
            cr.move_to(3.5, 4.0);
            cr.line_to(14.5, 4.0);
            cr.move_to(9.0, 4.0);
            cr.line_to(9.0, 15.0);
        }
    }
    let _ = cr.stroke();
}
