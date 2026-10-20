//! The running product of a version under review, in an embedded browser.
//! Drawing on it freezes the page into a screenshot on a whiteboard (see
//! [`Whiteboard`]), whose tools take the place of the browser's bar; what the
//! user draws and writes there is their comment, and goes to the agent as an
//! image.
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

use crate::core::review::Note;
use crate::core::store;
use crate::ui::diff_view;
use crate::ui::util;
use crate::ui::whiteboard::{self, Whiteboard};

/// Screenshots wider than this are scaled down, to keep them light for the agent.
const MAX_SCREENSHOT_WIDTH: i32 = 1600;

type AddCallback = Box<dyn Fn(String, String, Option<PathBuf>)>;
/// Handles a key; returns whether it did.
type KeyHandler = fn(&ProductView) -> bool;
type DeleteCallback = Box<dyn Fn(u64)>;

#[derive(Clone)]
pub struct ProductView(Rc<Inner>);

struct Inner {
    root: gtk::Box,
    back: gtk::Button,
    forward: gtk::Button,
    address: gtk::Label,
    /// Draws on the page, or once drawing, Done.
    draw: gtk::Button,
    /// The browser's bar, or the whiteboard's tools.
    bars: gtk::Stack,
    /// The comments on the product not posted yet, in a popover.
    pending: gtk::MenuButton,
    count: gtk::Label,
    cards: gtk::Box,
    stack: gtk::Stack,
    page: gtk::Box,
    whiteboard: Whiteboard,
    error_page: adw::StatusPage,
    view: RefCell<Option<webkit6::WebView>>,
    /// The address that failed to load, for Retry.
    failed: RefCell<Option<String>>,
    /// The address of the page frozen on the whiteboard.
    drawn_page: RefCell<Option<String>>,
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
        let draw = button("document-edit-symbolic", "Draw on the Page (Ctrl+M)");
        let address = util::label("", &["product-address"]);
        address.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        address.set_hexpand(true);
        address.set_selectable(true);

        let count = util::label("", &["count-badge"]);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(&gtk::Image::from_icon_name("chat-message-new-symbolic"));
        content.append(&count);
        let cards = gtk::Box::new(gtk::Orientation::Vertical, 8);
        cards.set_margin_top(10);
        cards.set_margin_bottom(10);
        cards.set_margin_start(10);
        cards.set_margin_end(10);
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(560)
            .width_request(340)
            .child(&cards)
            .build();
        let pending = gtk::MenuButton::builder()
            .child(&content)
            .popover(&gtk::Popover::builder().child(&scrolled).build())
            .tooltip_text("Drawings Not Posted Yet")
            .visible(false)
            .build();
        pending.add_css_class("flat");
        pending.add_css_class("comment-button");

        let browse = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        for widget in [
            back.upcast_ref::<gtk::Widget>(),
            forward.upcast_ref(),
            reload.upcast_ref(),
            address.upcast_ref(),
            open.upcast_ref(),
        ] {
            browse.append(widget);
        }
        let whiteboard = Whiteboard::new();
        let bars = gtk::Stack::builder().hhomogeneous(true).hexpand(true).build();
        bars.add_named(&browse, Some("browse"));
        bars.add_named(&whiteboard::build_tools(&whiteboard), Some("draw"));
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        bar.add_css_class("product-bar");
        bar.append(&bars);
        bar.append(&pending);
        bar.append(&draw);

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

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&bar);
        root.append(&stack);

        let this = Self(Rc::new(Inner {
            root,
            back,
            forward,
            address,
            draw,
            bars,
            pending,
            count,
            cards,
            stack,
            page,
            whiteboard,
            error_page,
            view: RefCell::default(),
            failed: RefCell::default(),
            drawn_page: RefCell::default(),
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
        this.0.draw.connect_clicked(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            let this = Self(inner);
            if this.drawing() {
                this.finish_drawing();
            } else {
                this.start_drawing();
            }
        });
        this.add_shortcuts();
        this
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.0.root
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    /// Called with the address of the page, the text written on the
    /// screenshot, and the screenshot drawn on.
    pub fn connect_add(&self, callback: impl Fn(String, String, Option<PathBuf>) + 'static) {
        self.0.on_add.replace(Some(Box::new(callback)));
    }

    /// Called with the id of a comment to delete.
    pub fn connect_delete(&self, callback: impl Fn(u64) + 'static) {
        self.0.on_delete.replace(Some(Box::new(callback)));
    }

    /// Opens `url`, afresh: the product may have changed since it was last shown.
    pub fn open(&self, url: &str) {
        self.leave_drawing();
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
        // Cookies only last when given a file, so logins to the product survive restarts.
        if let (Some(cookies), Some(file)) = (session.cookie_manager(), data.join("cookies.sqlite").to_str()) {
            cookies.set_persistent_storage(file, webkit6::CookiePersistentStorage::Sqlite);
        }
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
                if inner.stack.visible_child_name().as_deref() == Some("error") {
                    inner.stack.set_visible_child_name("page");
                }
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

        self.0.page.append(&view);
        self.0.view.replace(Some(view.clone()));
        view
    }

    /// Back and forward on the page, and the whiteboard's keys while drawing.
    fn add_shortcuts(&self) {
        let keys = gtk::ShortcutController::new();
        keys.set_scope(gtk::ShortcutScope::Managed);
        // The whiteboard's keys only act while drawing, and are left to the rest of the window otherwise.
        let shortcuts: [(&str, bool, KeyHandler); 7] = [
            ("<Alt>Left", false, |this| this.navigate(true)),
            ("<Alt>Right", false, |this| this.navigate(false)),
            ("<Control>z", true, |this| {
                this.0.whiteboard.undo();
                true
            }),
            ("<Control><Shift>z", true, |this| {
                this.0.whiteboard.redo();
                true
            }),
            ("<Control>y", true, |this| {
                this.0.whiteboard.redo();
                true
            }),
            ("<Control>Return", true, |this| {
                this.finish_drawing();
                true
            }),
            ("Escape", true, |this| {
                this.leave_drawing();
                true
            }),
        ];
        for (trigger, while_drawing, handle) in shortcuts {
            let weak = self.weak();
            let action = gtk::CallbackAction::new(move |_, _| {
                let Some(this) = weak.upgrade().map(Self) else { return glib::Propagation::Proceed };
                let handled = (!while_drawing || this.drawing()) && handle(&this);
                if handled { glib::Propagation::Stop } else { glib::Propagation::Proceed }
            });
            keys.add_shortcut(gtk::Shortcut::new(gtk::ShortcutTrigger::parse_string(trigger), Some(action)));
        }
        self.0.root.add_controller(keys);
    }

    /// Goes back or forward on the page; returns whether there was a page.
    fn navigate(&self, back: bool) -> bool {
        let Some(view) = self.0.view.borrow().clone().filter(|_| !self.drawing()) else { return false };
        if back {
            view.go_back();
        } else {
            view.go_forward();
        }
        true
    }

    fn update_navigation(&self) {
        let view = self.0.view.borrow();
        self.0.back.set_sensitive(view.as_ref().is_some_and(WebViewExt::can_go_back));
        self.0.forward.set_sensitive(view.as_ref().is_some_and(WebViewExt::can_go_forward));
        drop(view);
        self.0.address.set_text(&self.current_uri().unwrap_or_default());
    }

    /// Lists the comments on the product not posted yet, oldest first.
    pub fn set_notes(&self, notes: &[Note]) {
        while let Some(child) = self.0.cards.first_child() {
            self.0.cards.remove(&child);
        }
        let now = crate::core::now();
        let pending: Vec<&Note> = notes.iter().filter(|n| n.sent_at.is_none()).collect();
        for note in &pending {
            self.0.cards.append(&self.build_card(note, now));
        }
        self.0.count.set_text(&pending.len().to_string());
        self.0.pending.set_visible(!pending.is_empty());
    }

    /// Whether the page is frozen on the whiteboard.
    fn drawing(&self) -> bool {
        self.0.stack.visible_child_name().as_deref() == Some("whiteboard")
    }

    /// Freezes the page into a screenshot on the whiteboard, to draw on.
    pub fn start_drawing(&self) {
        let view = self.0.view.borrow().clone();
        let Some(view) = view.filter(|_| self.0.stack.visible_child_name().as_deref() == Some("page")) else { return };
        let page = self.current_uri();
        let weak = self.weak();
        glib::spawn_future_local(async move {
            let snapshot = view.snapshot_future(webkit6::SnapshotRegion::Visible, webkit6::SnapshotOptions::NONE).await;
            let Some(inner) = weak.upgrade() else { return };
            let loaded = snapshot
                .map_err(|e| e.to_string())
                .and_then(|t| inner.whiteboard.set_image(&t).map_err(|e| e.to_string()));
            match loaded {
                Ok(()) => {
                    inner.drawn_page.replace(page);
                    inner.stack.set_visible_child_name("whiteboard");
                    Self(inner).show_drawing(true);
                }
                Err(err) => eprintln!("agent-review: could not take a screenshot: {err}"),
            }
        });
    }

    /// Swaps the browser's bar for the whiteboard's tools, and the draw
    /// button for Done, or back.
    fn show_drawing(&self, drawing: bool) {
        let draw = &self.0.draw;
        self.0.bars.set_visible_child_name(if drawing { "draw" } else { "browse" });
        if drawing {
            draw.set_icon_name("object-select-symbolic");
            draw.set_tooltip_text(Some("Done: Add the Drawing to the Review (Ctrl+Enter)"));
            draw.remove_css_class("flat");
            draw.add_css_class("suggested-action");
            self.0.whiteboard.widget().grab_focus();
        } else {
            draw.set_icon_name("document-edit-symbolic");
            draw.set_tooltip_text(Some("Draw on the Page (Ctrl+M)"));
            draw.remove_css_class("suggested-action");
            draw.add_css_class("flat");
        }
    }

    /// Done: adds the drawing to the review, if there is one, and goes back
    /// to the page.
    fn finish_drawing(&self) {
        if !self.commit_drawing() {
            self.leave_drawing();
        }
    }

    /// Back to the live page, dropping the drawing.
    fn leave_drawing(&self) {
        if self.drawing() {
            self.0.stack.set_visible_child_name("page");
        }
        self.0.drawn_page.replace(None);
        self.show_drawing(false);
    }

    /// Adds what the user drew to the review as a comment on the page, and
    /// goes back to it; returns whether there was a drawing.
    pub fn commit_drawing(&self) -> bool {
        if !self.drawing() || !self.0.whiteboard.has_drawing() {
            return false;
        }
        let page = self.0.drawn_page.borrow().clone().or_else(|| self.current_uri()).unwrap_or_default();
        // What the user wrote on the screenshot, as text too.
        let texts = self.0.whiteboard.texts();
        let body = match texts.as_slice() {
            [only] => only.clone(),
            texts => texts.iter().map(|t| format!("- {t}")).collect::<Vec<_>>().join("\n"),
        };
        let path = store::screenshot_file(crate::core::now_millis());
        let screenshot = match self.0.whiteboard.export(&path, MAX_SCREENSHOT_WIDTH) {
            Ok(()) => Some(path),
            Err(err) => {
                eprintln!("agent-review: could not save the screenshot: {err}");
                None
            }
        };
        self.leave_drawing();
        if let Some(callback) = self.0.on_add.borrow().as_ref() {
            callback(page, body, screenshot);
        }
        true
    }

    fn build_card(&self, note: &Note, now: i64) -> gtk::Box {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        card.add_css_class("comment-card");
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let author = util::label("", &["comment-author"]);
        diff_view::show_author(&author, note.author.as_ref());
        header.append(&author);
        let page = note.page.as_deref().unwrap_or_default();
        let meta = format!("on {} · {}", page_label(page), util::relative_time(note.created_at, now));
        let meta = util::label(&meta, &["comment-meta"]);
        meta.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        meta.set_hexpand(true);
        meta.set_tooltip_text(Some(page));
        header.append(&meta);
        let delete = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Delete Drawing")
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
            picture.set_height_request(170);
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
