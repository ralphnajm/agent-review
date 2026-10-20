//! The running product of a version under review, in an embedded browser,
//! with the user's comments on it in a panel beside the page.
//!
//! The web view is only created the first time a product is opened, since
//! the web engine's processes cost far more than the rest of the app. Its
//! website data lives under the XDG dirs, so logins to the products under
//! review last.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gio, glib};
use webkit6::prelude::*;

use crate::core::review::{Author, Note};
use crate::core::store;
use crate::ui::diff_view::{self, Composer};
use crate::ui::util;

type AddCallback = Box<dyn Fn(String, String)>;
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
    stack: gtk::Stack,
    page: gtk::Box,
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
        bar.add_css_class("product-bar");
        for widget in
            [back.upcast_ref::<gtk::Widget>(), forward.upcast_ref(), reload.upcast_ref(), address.upcast_ref()]
        {
            bar.append(widget);
        }
        bar.append(&comments);
        bar.append(&open);

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

        let heading = util::label("Comments on the Product", &["heading"]);
        let empty =
            util::label("Comment on what you see: it reaches your agent with the code comments.", &["dim-label"]);
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
        root.append(&bar);
        root.append(&body);

        let this = Self(Rc::new(Inner {
            root,
            back,
            forward,
            address,
            comments,
            count,
            stack,
            page,
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
                inner.panel.set_reveal_child(button.is_active());
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

    /// Called with the address of the page and the comment's text.
    pub fn connect_add(&self, callback: impl Fn(String, String) + 'static) {
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

    /// Shows the comments with the composer ready for a new one.
    pub fn open_composer(&self) {
        self.0.comments.set_active(true);
        self.0.composer.text.grab_focus();
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

        let body = util::label(&note.body, &["comment-body"]);
        body.set_wrap(true);
        body.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        body.set_selectable(true);
        card.append(&body);
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
                inner.composer.text.buffer().set_text("");
                inner.comments.set_active(false);
            }
        });
        let weak = self.weak();
        composer.submit.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).submit();
            }
        });
        let submit = composer.submit.clone();
        composer.text.buffer().connect_changed(move |buffer| {
            submit.set_sensitive(!buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).trim().is_empty());
        });
        composer.submit.set_sensitive(false);
    }

    fn submit(&self) {
        let buffer = self.0.composer.text.buffer();
        let body = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
        let Some(page) = self.current_uri().filter(|_| !body.trim().is_empty()) else { return };
        buffer.set_text("");
        if let Some(callback) = self.0.on_add.borrow().as_ref() {
            callback(page, body);
        }
    }
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
