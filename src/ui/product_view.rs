//! The running product of a version under review, in an embedded browser,
//! with the user's comments on it in a panel beside the page. Commenting
//! freezes the page into a screenshot to draw on (see [`Whiteboard`]), which
//! goes to the agent with the comment.
//!
//! The web view is only created the first time a product is opened, since
//! the web engine's processes cost far more than the rest of the app. Its
//! website data lives under the XDG dirs, so logins to the products under
//! review last.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gio, glib};
use webkit6::prelude::*;

use crate::core::review::{Author, Note};
use crate::core::store;
use crate::ui::diff_view::{self, Composer};
use crate::ui::util;
use crate::ui::whiteboard::{self, Tool, Whiteboard};

/// Screenshots wider than this are scaled down, to keep them light for the agent.
const MAX_SCREENSHOT_WIDTH: i32 = 1600;

type AddCallback = Box<dyn Fn(String, String, Option<PathBuf>)>;
type DeleteCallback = Box<dyn Fn(u64)>;

#[derive(Clone)]
pub struct ProductView(Rc<Inner>);

struct Inner {
    root: gtk::Box,
    back: gtk::Button,
    forward: gtk::Button,
    address: gtk::Label,
    comments: gtk::ToggleButton,
    count: gtk::Label,
    /// The browser's bar, or the whiteboard's tools.
    bars: gtk::Stack,
    stack: gtk::Stack,
    page: gtk::Box,
    whiteboard: Whiteboard,
    error_page: adw::StatusPage,
    view: RefCell<Option<webkit6::WebView>>,
    /// The address that failed to load, for Retry.
    failed: RefCell<Option<String>>,
    panel: gtk::Revealer,
    cards: gtk::Box,
    empty: gtk::Label,
    composer: Composer,
    on_add: RefCell<Option<AddCallback>>,
    on_delete: RefCell<Option<DeleteCallback>>,
}

impl ProductView {
    pub fn new() -> Self {
        let button = |icon: &str, tooltip: &str| {
            let button = gtk::Button::builder().icon_name(icon).tooltip_text(tooltip).build();
            button.add_css_class("flat");
            button.add_css_class("icon-button");
            button
        };
        let back = button("go-previous-symbolic", "Back (Alt+Left)");
        let forward = button("go-next-symbolic", "Forward (Alt+Right)");
        let reload = button("view-refresh-symbolic", "Reload Page");
        let open = button("adw-external-link-symbolic", "Open in Browser");
        let address = util::label("", &["product-address"]);
        address.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        address.set_hexpand(true);
        address.set_selectable(true);

        let count = util::label("", &["count-badge"]);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(&gtk::Image::from_icon_name("chat-message-new-symbolic"));
        content.append(&count);
        let comments =
            gtk::ToggleButton::builder().child(&content).tooltip_text("Comments on the Product (Ctrl+M)").build();
        comments.add_css_class("flat");
        comments.add_css_class("comment-button");

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        for widget in
            [back.upcast_ref::<gtk::Widget>(), forward.upcast_ref(), reload.upcast_ref(), address.upcast_ref()]
        {
            bar.append(widget);
        }
        bar.append(&open);
        let whiteboard = Whiteboard::new();
        let (tools, undo) = build_tools(&whiteboard);
        let bars = gtk::Stack::builder().hhomogeneous(true).hexpand(true).build();
        bars.add_named(&bar, Some("browse"));
        bars.add_named(&tools, Some("annotate"));
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        top.add_css_class("product-bar");
        top.append(&bars);
        top.append(&comments);

        let page = gtk::Box::builder().hexpand(true).vexpand(true).build();
        let retry = gtk::Button::builder().label("Retry").halign(gtk::Align::Center).build();
        retry.add_css_class("pill");
        let error_page = adw::StatusPage::builder()
            .icon_name("network-offline-symbolic")
            .title("The Product Isn't Running")
            .child(&retry)
            .build();
        let stack = gtk::Stack::builder().hexpand(true).vexpand(true).build();
        stack.add_named(&page, Some("page"));
        stack.add_named(&error_page, Some("error"));
        stack.add_named(whiteboard.widget(), Some("whiteboard"));

        let heading = util::label("Comments on the Product", &["heading"]);
        let empty = util::label(
            "Draw on the screenshot and say what you want changed: it reaches your agent with the code comments.",
            &["dim-label"],
        );
        empty.set_wrap(true);
        let cards = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        list.set_margin_top(12);
        list.set_margin_bottom(12);
        list.set_margin_start(12);
        list.set_margin_end(12);
        list.append(&heading);
        list.append(&empty);
        list.append(&cards);
        let scrolled =
            gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&list).build();
        let composer = diff_view::build_composer();
        composer.root.set_margin_start(12);
        composer.root.set_margin_end(12);
        composer.root.set_margin_bottom(12);
        let side = gtk::Box::new(gtk::Orientation::Vertical, 0);
        side.add_css_class("product-comments");
        side.set_width_request(340);
        side.append(&scrolled);
        side.append(&composer.root);
        // The page takes the room the comments don't need.
        let panel = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideLeft)
            .hexpand(false)
            .child(&side)
            .build();

        let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        body.append(&stack);
        body.append(&panel);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&top);
        root.append(&body);

        let this = Self(Rc::new(Inner {
            root,
            back,
            forward,
            address,
            comments,
            count,
            bars,
            stack,
            page,
            whiteboard,
            error_page,
            view: RefCell::default(),
            failed: RefCell::default(),
            panel,
            cards,
            empty,
            composer,
            on_add: RefCell::default(),
            on_delete: RefCell::default(),
        }));
        this.update_navigation();

        let weak = this.weak();
        this.0.back.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade().and_then(|inner| inner.view.borrow().clone()) {
                view.go_back();
            }
        });
        let weak = this.weak();
        this.0.forward.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade().and_then(|inner| inner.view.borrow().clone()) {
                view.go_forward();
            }
        });
        let weak = this.weak();
        reload.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).reload();
            }
        });
        let weak = this.weak();
        retry.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).reload();
            }
        });
        let weak = this.weak();
        open.connect_clicked(move |button| {
            let Some(uri) = weak.upgrade().and_then(|inner| Self(inner).current_uri()) else { return };
            let window = button.root().and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(&uri).launch(window.as_ref(), None::<&gio::Cancellable>, |_| {});
        });
        let weak = this.weak();
        this.0.comments.connect_toggled(move |button| {
            if let Some(inner) = weak.upgrade() {
                if button.is_active() { Self(inner).annotate() } else { Self(inner).leave_annotation() }
            }
        });
        let weak = this.weak();
        undo.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.whiteboard.undo();
            }
        });
        let weak = this.weak();
        this.0.whiteboard.connect_change(move || {
            if let Some(inner) = weak.upgrade() {
                Self(inner).update_submit();
            }
        });
        this.connect_composer();
        this
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.0.root
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    /// Called with the address of the page, the comment's text, and the
    /// screenshot drawn on, if any.
    pub fn connect_add(&self, callback: impl Fn(String, String, Option<PathBuf>) + 'static) {
        self.0.on_add.replace(Some(Box::new(callback)));
    }

    /// Called with the id of a comment to delete.
    pub fn connect_delete(&self, callback: impl Fn(u64) + 'static) {
        self.0.on_delete.replace(Some(Box::new(callback)));
    }

    pub fn set_author(&self, author: Option<&Author>) {
        diff_view::show_author(&self.0.composer.author, author);
    }

    /// Opens `url`, afresh: the product may have changed since it was last shown.
    pub fn open(&self, url: &str) {
        self.0.comments.set_active(false);
        self.view().load_uri(url);
        self.0.stack.set_visible_child_name("page");
    }

    fn reload(&self) {
        let failed = self.0.failed.take();
        let Some(view) = self.0.view.borrow().clone() else { return };
        match failed {
            Some(uri) => view.load_uri(&uri),
            None => view.reload(),
        }
        self.0.stack.set_visible_child_name("page");
    }

    fn current_uri(&self) -> Option<String> {
        let failed = self.0.failed.borrow().clone();
        failed.or_else(|| self.0.view.borrow().as_ref().and_then(WebViewExt::uri).map(Into::into))
    }

    /// The web view, created on first use.
    fn view(&self) -> webkit6::WebView {
        if let Some(view) = self.0.view.borrow().as_ref() {
            return view.clone();
        }
        let data = store::web_data_dir();
        let cache = store::web_cache_dir();
        let session = webkit6::NetworkSession::new(data.to_str(), cache.to_str());
        let view = webkit6::WebView::builder().network_session(&session).hexpand(true).vexpand(true).build();
        if let Some(settings) = WebViewExt::settings(&view) {
            // Inspect Element, for the developers reviewing their product.
            settings.set_enable_developer_extras(true);
        }

        let weak = self.weak();
        view.connect_uri_notify(move |_| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).update_navigation();
            }
        });
        let weak = self.weak();
        view.connect_load_changed(move |_, event| {
            let Some(inner) = weak.upgrade() else { return };
            if event == webkit6::LoadEvent::Committed {
                inner.failed.replace(None);
                inner.stack.set_visible_child_name("page");
            }
            Self(inner).update_navigation();
        });
        let weak = self.weak();
        view.connect_load_failed(move |_, _, uri, error| {
            // Another load took over, or the page started a download.
            if error.matches(webkit6::NetworkError::Cancelled)
                || error.matches(webkit6::PolicyError::FrameLoadInterruptedByPolicyChange)
            {
                return false;
            }
            let Some(inner) = weak.upgrade() else { return false };
            inner.failed.replace(Some(uri.to_owned()));
            inner.error_page.set_description(Some(&format!(
                "Nothing answered at {}. Ask your agent to start it, then retry.\n\n<small>{}</small>",
                glib::markup_escape_text(uri),
                glib::markup_escape_text(error.message())
            )));
            inner.stack.set_visible_child_name("error");
            Self(inner).update_navigation();
            true
        });

        let keys = gtk::ShortcutController::new();
        keys.set_scope(gtk::ShortcutScope::Managed);
        for (trigger, back) in [("<Alt>Left", true), ("<Alt>Right", false)] {
            let weak = self.weak();
            let action = gtk::CallbackAction::new(move |_, _| {
                if let Some(view) = weak.upgrade().and_then(|inner| inner.view.borrow().clone()) {
                    if back { view.go_back() } else { view.go_forward() }
                }
                glib::Propagation::Stop
            });
            keys.add_shortcut(gtk::Shortcut::new(gtk::ShortcutTrigger::parse_string(trigger), Some(action)));
        }
        self.0.root.add_controller(keys);

        self.0.page.append(&view);
        self.0.view.replace(Some(view.clone()));
        view
    }

    fn update_navigation(&self) {
        let view = self.0.view.borrow();
        self.0.back.set_sensitive(view.as_ref().is_some_and(WebViewExt::can_go_back));
        self.0.forward.set_sensitive(view.as_ref().is_some_and(WebViewExt::can_go_forward));
        drop(view);
        let uri = self.current_uri().unwrap_or_default();
        self.0.address.set_text(&uri);
        self.0.composer.title.set_text(&format!("on {}", page_label(&uri)));
    }

    /// Shows the comments on the product, oldest first.
    pub fn set_notes(&self, notes: &[Note]) {
        while let Some(child) = self.0.cards.first_child() {
            self.0.cards.remove(&child);
        }
        let now = crate::core::now();
        for note in notes {
            self.0.cards.append(&self.build_card(note, now));
        }
        self.0.empty.set_visible(notes.is_empty());
        self.0.count.set_text(&notes.len().to_string());
        self.0.count.set_visible(!notes.is_empty());
    }

    /// Starts a comment: on a screenshot of the page to draw on, when there is a page.
    pub fn open_composer(&self) {
        self.0.comments.set_active(true);
    }

    fn annotating(&self) -> bool {
        self.0.stack.visible_child_name().as_deref() == Some("whiteboard")
    }

    /// Freezes the page into a screenshot on the whiteboard, with the
    /// comments and the composer beside it.
    fn annotate(&self) {
        self.0.composer.text.grab_focus();
        let view = self.0.view.borrow().clone();
        let view = view.filter(|_| self.0.stack.visible_child_name().as_deref() == Some("page"));
        // Open the comments first, so the page takes its new width before the screenshot.
        let panel = &self.0.panel;
        panel.set_reveal_child(true);
        let Some(view) = view else { return };
        if panel.is_child_revealed() {
            self.screenshot(view);
            return;
        }
        let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
        let (weak, once) = (self.weak(), Rc::clone(&handler));
        let id = panel.connect_child_revealed_notify(move |panel| {
            if let Some(id) = once.take() {
                panel.disconnect(id);
            }
            // Give the page a moment to lay itself out at its new width.
            let (weak, view) = (weak.clone(), view.clone());
            glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
                if let Some(inner) = weak.upgrade() {
                    Self(inner).screenshot(view);
                }
            });
        });
        handler.replace(Some(id));
    }

    /// Freezes what `view` shows onto the whiteboard.
    fn screenshot(&self, view: webkit6::WebView) {
        let weak = self.weak();
        glib::spawn_future_local(async move {
            let snapshot = view.snapshot_future(webkit6::SnapshotRegion::Visible, webkit6::SnapshotOptions::NONE).await;
            let Some(inner) = weak.upgrade() else { return };
            // Closed meanwhile.
            if !inner.comments.is_active() {
                return;
            }
            match snapshot
                .map_err(|e| e.to_string())
                .and_then(|t| inner.whiteboard.set_image(&t).map_err(|e| e.to_string()))
            {
                Ok(()) => {
                    inner.stack.set_visible_child_name("whiteboard");
                    inner.bars.set_visible_child_name("annotate");
                    Self(inner).update_submit();
                }
                Err(err) => eprintln!("agent-review: could not take a screenshot: {err}"),
            }
        });
    }

    /// Back to the live page.
    fn leave_annotation(&self) {
        self.0.panel.set_reveal_child(false);
        self.0.composer.text.buffer().set_text("");
        if self.annotating() {
            self.0.stack.set_visible_child_name("page");
        }
        self.0.bars.set_visible_child_name("browse");
    }

    /// A comment needs words, or a drawing on the screenshot.
    fn update_submit(&self) {
        let buffer = self.0.composer.text.buffer();
        let text = !buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).trim().is_empty();
        self.0.composer.submit.set_sensitive(text || (self.annotating() && self.0.whiteboard.has_drawing()));
    }

    fn build_card(&self, note: &Note, now: i64) -> gtk::Box {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
        card.add_css_class("comment-card");
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let author = util::label("", &["comment-author"]);
        diff_view::show_author(&author, note.author.as_ref());
        header.append(&author);
        let time = match note.sent_at {
            Some(sent) => format!("sent {}", util::relative_time(sent, now)),
            None => util::relative_time(note.created_at, now),
        };
        let page = note.page.as_deref().unwrap_or_default();
        let meta = util::label(&format!("on {} · {time}", page_label(page)), &["comment-meta"]);
        meta.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        meta.set_hexpand(true);
        meta.set_tooltip_text(Some(page));
        header.append(&meta);
        let delete = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Delete Comment")
            .valign(gtk::Align::Center)
            .build();
        delete.add_css_class("flat");
        delete.add_css_class("small-button");
        let weak = self.weak();
        let id = note.id;
        delete.connect_clicked(move |_| {
            // Deleting re-renders the cards, which destroys this button: do it once the click is over.
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(inner) = weak.upgrade()
                    && let Some(callback) = inner.on_delete.borrow().as_ref()
                {
                    callback(id);
                }
            });
        });
        header.append(&delete);
        card.append(&header);

        if let Some(path) = note.screenshot.as_deref().map(PathBuf::from).filter(|p| p.is_file()) {
            let picture = gtk::Picture::for_filename(&path);
            picture.set_content_fit(gtk::ContentFit::Contain);
            picture.set_can_shrink(true);
            picture.set_height_request(150);
            picture.add_css_class("comment-screenshot");
            picture.set_cursor_from_name(Some("pointer"));
            picture.set_tooltip_text(Some("Open the Screenshot"));
            let click = gtk::GestureClick::new();
            click.connect_released(move |gesture, _, _, _| {
                let window = gesture.widget().and_then(|w| w.root()).and_downcast::<gtk::Window>();
                gtk::FileLauncher::new(Some(&gio::File::for_path(&path))).launch(
                    window.as_ref(),
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            });
            picture.add_controller(click);
            card.append(&picture);
        }
        if !note.body.is_empty() {
            let body = util::label(&note.body, &["comment-body"]);
            body.set_wrap(true);
            body.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            body.set_selectable(true);
            card.append(&body);
        }
        card
    }

    fn connect_composer(&self) {
        let composer = &self.0.composer;
        let keys = gtk::EventControllerKey::new();
        let weak = self.weak();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(inner) = weak.upgrade() else { return glib::Propagation::Proceed };
            match key {
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter
                    if modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK) =>
                {
                    Self(inner).submit();
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::Escape => {
                    inner.comments.set_active(false);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        composer.text.add_controller(keys);

        let weak = self.weak();
        composer.cancel.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.comments.set_active(false);
            }
        });
        let weak = self.weak();
        composer.submit.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).submit();
            }
        });
        let weak = self.weak();
        composer.text.buffer().connect_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).update_submit();
            }
        });
        composer.submit.set_sensitive(false);
    }

    fn submit(&self) {
        let buffer = self.0.composer.text.buffer();
        let mut body = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).trim().to_owned();
        let drawing = self.annotating() && self.0.whiteboard.has_drawing();
        let Some(page) = self.current_uri().filter(|_| !body.is_empty() || drawing) else { return };
        let mut screenshot = None;
        if self.annotating() {
            // The notes written on the screenshot, as text too.
            let texts = self.0.whiteboard.texts();
            if !texts.is_empty() {
                if !body.is_empty() {
                    body.push_str("\n\n");
                }
                body.push_str("Written on the screenshot:");
                for text in texts {
                    body.push_str("\n- ");
                    body.push_str(&text);
                }
            }
            let path = store::screenshot_file(crate::core::now_millis());
            match self.0.whiteboard.export(&path, MAX_SCREENSHOT_WIDTH) {
                Ok(()) => screenshot = Some(path),
                Err(err) => eprintln!("agent-review: could not save the screenshot: {err}"),
            }
        }
        if let Some(callback) = self.0.on_add.borrow().as_ref() {
            callback(page, body, screenshot);
        }
        self.0.comments.set_active(false);
    }
}

/// The whiteboard's tools, its colours and Undo, in a bar; returns the bar and Undo.
fn build_tools(whiteboard: &Whiteboard) -> (gtk::Box, gtk::Button) {
    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tools.add_css_class("linked");
    let mut first: Option<gtk::ToggleButton> = None;
    for (tool, label, tooltip) in [
        (Tool::Marker, "Marker", "Draw freely"),
        (Tool::Arrow, "Arrow", "Point at something"),
        (Tool::Box, "Box", "Frame an area"),
        (Tool::Sticky, "Sticky Note", "Click where the note goes"),
        (Tool::Text, "Text", "Click where the text goes"),
    ] {
        let button = gtk::ToggleButton::builder().label(label).tooltip_text(tooltip).build();
        button.set_group(first.as_ref());
        let board = whiteboard.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                board.set_tool(tool);
            }
        });
        tools.append(&button);
        first.get_or_insert(button);
    }
    if let Some(first) = first {
        first.set_active(true);
    }

    let colors = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let mut first: Option<gtk::ToggleButton> = None;
    for (name, color) in whiteboard::COLORS {
        let button = gtk::ToggleButton::builder().tooltip_text(name).valign(gtk::Align::Center).build();
        button.add_css_class("swatch");
        button.add_css_class(&format!("swatch-{}", name.to_lowercase()));
        button.set_group(first.as_ref());
        let board = whiteboard.clone();
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

    let undo = gtk::Button::builder().icon_name("edit-undo-symbolic").tooltip_text("Undo").build();
    undo.add_css_class("flat");
    undo.add_css_class("icon-button");
    let hint = util::label("Draw on the screenshot, then comment beside it", &["dim-label", "caption"]);
    hint.set_hexpand(true);
    hint.set_ellipsize(gtk::pango::EllipsizeMode::End);
    hint.set_xalign(1.0);
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    bar.append(&tools);
    bar.append(&colors);
    bar.append(&undo);
    bar.append(&hint);
    (bar, undo)
}

/// A page's address without its scheme and host, which are the same
/// throughout a product: `/pricing?plan=pro`.
fn page_label(uri: &str) -> &str {
    let Some((_, rest)) = uri.split_once("://") else { return uri };
    rest.find('/').map_or("/", |at| &rest[at..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_pages_by_their_path() {
        assert_eq!(page_label("http://localhost:5173/pricing?plan=pro"), "/pricing?plan=pro");
        assert_eq!(page_label("http://localhost:5173"), "/");
        assert_eq!(page_label("about:blank"), "about:blank");
    }
}
