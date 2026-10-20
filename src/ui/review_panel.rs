//! The review panel: every changed file's diff in one scrolling column.
//!
//! Each file is a section with a header above its diff. While the diff
//! scrolls past, its header sticks to the top of the panel. Collapsed files,
//! such as reviewed ones, show only their header; their diff is built the
//! first time they are expanded. Find searches every expanded file.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use sourceview5::prelude::*;

use crate::core::changes::ChangeKind;
use crate::core::diff::FileDiff;
use crate::core::review::{ApprovalState, Author, LineAnchor, Note};
use crate::ui::diff_view::{DiffView, Find};
use crate::ui::scroller::Scroller;
use crate::ui::util;

/// Frames during which a scroll target is held while the layout settles.
const SETTLE_FRAMES: u32 = 12;
/// How far the arrow keys scroll.
const LINE_STEP: f64 = 48.0;

/// Where in a file the panel holds the top of the screen.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Spot {
    /// The file's header.
    Header,
    /// This far below the header, in pixels.
    Offset(f64),
    /// The line its diff pinned (see `DiffView::pin_top`).
    Line,
}

/// What the panel shows for one file.
pub struct FileData<'a> {
    pub path: &'a str,
    pub kind: ChangeKind,
    pub additions: u32,
    pub deletions: u32,
    /// Kind of change and rename details for the header.
    pub meta: String,
    pub approval: ApprovalState,
    pub diff: Cow<'a, FileDiff>,
    pub notes: &'a [Note],
}

type AddCallback = Box<dyn Fn(&str, Option<LineAnchor>, String)>;
type DeleteCallback = Box<dyn Fn(&str, u64)>;
type PathCallback = Box<dyn Fn(&str)>;
type LoadCallback = Box<dyn Fn(&str, Box<dyn FnOnce(Option<String>)>)>;

struct Section {
    path: String,
    widget: SectionBox,
    expander: gtk::Button,
    reviewed: gtk::CheckButton,
    badge: gtk::Label,
    dir: gtk::Label,
    name: gtk::Label,
    /// Comments still to send, beside the comment button.
    comments: gtk::Label,
    added: gtk::Label,
    removed: gtk::Label,
    meta: gtk::Label,
    stale: gtk::Label,
    history_slot: gtk::Box,
    view: RefCell<Option<DiffView>>,
    expanded: Cell<bool>,
    approval: Cell<ApprovalState>,
    /// The latest content, for the diff to show once it is built.
    content: RefCell<(FileDiff, Vec<Note>)>,
    /// Set while the checkbox is changed programmatically.
    updating: Cell<bool>,
}

/// The find bar above the files.
struct Search {
    bar: gtk::SearchBar,
    entry: gtk::SearchEntry,
    count: gtk::Label,
    previous: gtk::Button,
    next: gtk::Button,
    settings: sourceview5::SearchSettings,
}

#[derive(Clone)]
pub struct ReviewPanel(Rc<Inner>);

struct Inner {
    root: gtk::Box,
    scroller: Scroller,
    search: Search,
    sections: RefCell<Vec<Rc<Section>>>,
    filter: RefCell<String>,
    /// The file being read: the one at the top, or the last one worked in.
    current: RefCell<Option<String>>,
    /// A file, and where in it, to keep at the top while the layout settles,
    /// and for how many more frames.
    target: RefCell<Option<(String, Spot, u32)>>,
    settle: RefCell<Option<gtk::TickCallbackId>>,
    /// Set while scrolling programmatically.
    scrolling: Cell<bool>,
    /// Who comments are written by, shown in the composer.
    author: RefCell<Option<Author>>,
    /// Whether diffs show the old and new sides beside each other.
    split: Cell<bool>,
    on_add: RefCell<Option<AddCallback>>,
    on_delete: RefCell<Option<DeleteCallback>>,
    on_reviewed: RefCell<Option<PathCallback>>,
    on_current: RefCell<Option<PathCallback>>,
    on_load: RefCell<Option<LoadCallback>>,
}

impl ReviewPanel {
    pub fn new() -> Self {
        let scroller = Scroller::new();
        scroller.content().set_margin_bottom(96);
        let search = build_search();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&search.bar);
        root.append(scroller.window());

        let this = Self(Rc::new(Inner {
            root,
            scroller,
            search,
            sections: RefCell::default(),
            filter: RefCell::default(),
            current: RefCell::default(),
            target: RefCell::default(),
            settle: RefCell::default(),
            scrolling: Cell::new(false),
            author: RefCell::default(),
            split: Cell::new(false),
            on_add: RefCell::default(),
            on_delete: RefCell::default(),
            on_reviewed: RefCell::default(),
            on_current: RefCell::default(),
            on_load: RefCell::default(),
        }));
        this.connect_scrolling();
        this.connect_search();
        this
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.0.root
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    /// Called when a comment is written on a file: on the anchored lines, or the file for `None`.
    pub fn connect_add(&self, callback: impl Fn(&str, Option<LineAnchor>, String) + 'static) {
        self.0.on_add.replace(Some(Box::new(callback)));
    }

    pub fn connect_delete(&self, callback: impl Fn(&str, u64) + 'static) {
        self.0.on_delete.replace(Some(Box::new(callback)));
    }

    /// Called when a file's "Reviewed" checkbox is clicked.
    pub fn connect_reviewed(&self, callback: impl Fn(&str) + 'static) {
        self.0.on_reviewed.replace(Some(Box::new(callback)));
    }

    /// Called when another file becomes the current one by scrolling, finding or focusing it.
    /// Called to read the new side of a file, for its unchanged lines; reply with `None` if it can't be.
    pub fn connect_load(&self, callback: impl Fn(&str, Box<dyn FnOnce(Option<String>)>) + 'static) {
        self.0.on_load.replace(Some(Box::new(callback)));
    }

    pub fn connect_current(&self, callback: impl Fn(&str) + 'static) {
        self.0.on_current.replace(Some(Box::new(callback)));
    }

    // ----- Files ---------------------------------------------------------

    /// Shows `files`, in order. Files already shown keep their diff, scroll
    /// position and whether they are collapsed.
    pub fn set_files(&self, files: Vec<FileData<'_>>) {
        let held = self.pin_top();
        let content = self.0.scroller.content();
        let mut old: HashMap<String, Rc<Section>> =
            self.0.sections.borrow_mut().drain(..).map(|s| (s.path.clone(), s)).collect();
        let mut sections = Vec::with_capacity(files.len());
        let mut previous: Option<gtk::Widget> = None;
        for file in files {
            let section = match old.remove(file.path) {
                Some(section) => {
                    self.update_section(&section, file);
                    section
                }
                None => self.build_section(file),
            };
            let widget = section.widget.upcast_ref::<gtk::Widget>();
            if widget.parent().is_none() {
                content.insert_child_after(widget, previous.as_ref());
            } else if widget.prev_sibling() != previous {
                content.reorder_child_after(widget, previous.as_ref());
            }
            previous = Some(widget.clone());
            sections.push(section);
        }
        for section in old.into_values() {
            content.remove(&section.widget);
        }
        self.0.sections.replace(sections);
        self.apply_filter();
        if let Some((path, spot)) = held {
            self.hold(&path, spot);
        }
    }

    /// What is at the top of the screen, as the file and where in it, unless
    /// something is already held there.
    fn pin_top(&self) -> Option<(String, Spot)> {
        if self.0.target.borrow().is_some() {
            return None;
        }
        let value = self.0.scroller.window().vadjustment().value();
        let section = self.section_at(value + self.0.scroller.inset())?;
        let top = self.0.scroller.offset_of(&section.widget)?;
        let pinned = section.view.borrow().as_ref().is_some_and(|view| section.expanded.get() && view.pin_top());
        Some((section.path.clone(), if pinned { Spot::Line } else { Spot::Offset(value - top) }))
    }

    /// Signs new comments with `author`, or as unknown.
    pub fn set_author(&self, author: Option<&Author>) {
        self.0.author.replace(author.cloned());
        for section in self.0.sections.borrow().iter() {
            if let Some(view) = section.view.borrow().as_ref() {
                view.set_author(author);
            }
        }
    }

    /// Shows the old and new sides of every diff beside each other, or one above the other.
    pub fn set_split(&self, split: bool) {
        self.0.split.set(split);
        for section in self.0.sections.borrow().iter() {
            if let Some(view) = section.view.borrow().as_ref() {
                view.set_split(split);
            }
        }
    }

    /// Removes every file, for another project or view.
    pub fn clear(&self) {
        self.set_files(Vec::new());
        self.stop_settling();
        self.0.current.replace(None);
        self.0.scroller.window().vadjustment().set_value(0.0);
    }

    /// Shows only the files whose path contains `text`, ignoring case.
    pub fn set_filter(&self, text: &str) {
        self.0.filter.replace(text.trim().to_lowercase());
        self.apply_filter();
    }

    fn apply_filter(&self) {
        let filter = self.0.filter.borrow();
        for section in self.0.sections.borrow().iter() {
            section.widget.set_visible(filter.is_empty() || section.path.to_lowercase().contains(filter.as_str()));
        }
    }

    fn section(&self, path: &str) -> Option<Rc<Section>> {
        self.0.sections.borrow().iter().find(|s| s.path == path).cloned()
    }

    fn build_section(&self, file: FileData<'_>) -> Rc<Section> {
        let expander = gtk::Button::builder().valign(gtk::Align::Center).build();
        expander.add_css_class("flat");
        expander.add_css_class("small-button");
        let reviewed = gtk::CheckButton::builder()
            .tooltip_text("Reviewed: check to mark this file done and collapse it (Ctrl+Shift+A)")
            .valign(gtk::Align::Center)
            .build();
        reviewed.add_css_class("review-check");
        let badge = util::label("", &["status-letter"]);
        let dir = util::label("", &["file-header-dir"]);
        dir.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        let name = util::label("", &["file-header-name"]);
        let comments = gtk::Label::new(None);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        content.set_halign(gtk::Align::Center);
        content.append(&gtk::Image::from_icon_name("chat-message-new-symbolic"));
        content.append(&comments);
        let comment = gtk::Button::builder()
            .child(&content)
            .tooltip_text("Comment on This File")
            .valign(gtk::Align::Center)
            .build();
        comment.add_css_class("flat");
        comment.add_css_class("comment-button");
        let history_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let added = util::label("", &["stat-added"]);
        added.set_margin_start(8);
        let removed = util::label("", &["stat-removed"]);
        let meta = util::label("", &["file-header-meta"]);
        meta.set_margin_start(4);
        meta.set_hexpand(true);
        meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let stale = util::label("Changed since review", &["chip", "warning-chip"]);
        stale.set_valign(gtk::Align::Center);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.add_css_class("file-header");
        for widget in [
            expander.upcast_ref::<gtk::Widget>(),
            reviewed.upcast_ref(),
            badge.upcast_ref(),
            dir.upcast_ref(),
            name.upcast_ref(),
            comment.upcast_ref(),
            history_slot.upcast_ref(),
            added.upcast_ref(),
            removed.upcast_ref(),
            meta.upcast_ref(),
            stale.upcast_ref(),
        ] {
            header.append(widget);
        }

        // Reviewed files start collapsed, unless they hold comments still to send.
        let open = file.approval != ApprovalState::Approved || file.notes.iter().any(|n| n.sent_at.is_none());
        let section = Rc::new(Section {
            path: file.path.to_owned(),
            widget: SectionBox::new(&header, &self.0.scroller),
            expander,
            reviewed,
            badge,
            dir,
            name,
            comments,
            added,
            removed,
            meta,
            stale,
            history_slot,
            view: RefCell::default(),
            expanded: Cell::new(!open),
            approval: Cell::new(file.approval),
            content: RefCell::new((FileDiff::default(), Vec::new())),
            updating: Cell::new(false),
        });

        // Clicking the header, but not its buttons, collapses or expands the file.
        let click = gtk::GestureClick::new();
        let (weak, section_weak) = (self.weak(), Rc::downgrade(&section));
        click.connect_released(move |gesture, _, _, _| {
            if let (Some(inner), Some(section)) = (weak.upgrade(), section_weak.upgrade()) {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                ReviewPanel(inner).toggle_expanded(&section);
            }
        });
        header.add_controller(click);
        let (weak, section_weak) = (self.weak(), Rc::downgrade(&section));
        section.expander.connect_clicked(move |_| {
            if let (Some(inner), Some(section)) = (weak.upgrade(), section_weak.upgrade()) {
                ReviewPanel(inner).toggle_expanded(&section);
            }
        });
        let (weak, section_weak) = (self.weak(), Rc::downgrade(&section));
        section.reviewed.connect_toggled(move |_| {
            let (Some(inner), Some(section)) = (weak.upgrade(), section_weak.upgrade()) else { return };
            if section.updating.get() {
                return;
            }
            if let Some(callback) = inner.on_reviewed.borrow().as_ref() {
                callback(&section.path);
            }
        });
        let (weak, section_weak) = (self.weak(), Rc::downgrade(&section));
        comment.connect_clicked(move |_| {
            if let (Some(inner), Some(section)) = (weak.upgrade(), section_weak.upgrade()) {
                ReviewPanel(inner).open_composer(&section.path, false);
            }
        });

        self.update_section(&section, file);
        self.set_expanded(&section, open);
        section
    }

    fn update_section(&self, section: &Section, file: FileData<'_>) {
        let (name, dir) = util::split_path(file.path);
        section.badge.set_text(file.kind.letter());
        section.badge.set_css_classes(&["status-letter", file.kind.css_class()]);
        section.dir.set_text(&if dir.is_empty() { String::new() } else { format!("{dir}/") });
        section.name.set_text(name);
        let unsent = file.notes.iter().filter(|n| n.sent_at.is_none()).count();
        section.comments.set_text(&unsent.to_string());
        section.comments.set_visible(unsent > 0);
        section.added.set_text(&format!("+{}", file.additions));
        section.added.set_visible(file.additions > 0);
        section.removed.set_text(&format!("−{}", file.deletions));
        section.removed.set_visible(file.deletions > 0);
        section.meta.set_text(&file.meta);
        section.stale.set_visible(file.approval == ApprovalState::Stale);
        section.updating.set(true);
        section.reviewed.set_active(file.approval == ApprovalState::Approved);
        section.updating.set(false);

        // A file that is no longer approved, e.g. because it changed since, needs another look.
        let was_approved = section.approval.replace(file.approval) == ApprovalState::Approved;
        let view = section.view.borrow().clone();
        match view {
            Some(view) => view.show(&file.diff, file.notes),
            None => {
                section.content.replace((file.diff.into_owned(), file.notes.to_vec()));
            }
        }
        if was_approved && file.approval != ApprovalState::Approved {
            self.set_expanded(section, true);
        }
    }

    // ----- Collapsing ----------------------------------------------------

    fn toggle_expanded(&self, section: &Section) {
        self.set_expanded(section, !section.expanded.get());
        if !section.expanded.get() {
            self.keep_header_on_screen(&section.path);
        }
    }

    fn set_expanded(&self, section: &Section, expanded: bool) {
        if section.expanded.replace(expanded) == expanded {
            return;
        }
        if expanded {
            self.ensure_view(section);
        }
        if let Some(view) = section.view.borrow().as_ref() {
            view.widget().set_visible(expanded);
        }
        let (icon, tooltip) = if expanded { ("pan-down-symbolic", "Collapse") } else { ("pan-end-symbolic", "Expand") };
        section.expander.set_icon_name(icon);
        section.expander.set_tooltip_text(Some(tooltip));
        if expanded {
            section.widget.remove_css_class("collapsed");
        } else {
            section.widget.add_css_class("collapsed");
        }
    }

    /// Collapses a file, keeping its header on screen.
    pub fn collapse(&self, path: &str) {
        if let Some(section) = self.section(path) {
            self.set_expanded(&section, false);
            self.keep_header_on_screen(path);
        }
    }

    /// Scrolls back to a file's header if it is above the top, as after
    /// collapsing a file whose header was stuck there.
    fn keep_header_on_screen(&self, path: &str) {
        let Some(section) = self.section(path) else { return };
        let value = self.0.scroller.window().vadjustment().value();
        if self.0.scroller.offset_of(&section.widget).is_some_and(|top| top < value) {
            self.hold_at_top(path);
        }
    }

    /// Builds a file's diff, which waits until the file is first expanded.
    fn ensure_view(&self, section: &Section) {
        if section.view.borrow().is_some() {
            return;
        }
        let view = DiffView::new(&self.0.scroller, &self.0.search.settings);
        view.set_author(self.0.author.borrow().as_ref());
        view.set_split(self.0.split.get());
        let path = section.path.clone();
        let weak = self.weak();
        view.connect_add(move |anchor, body| {
            if let Some(inner) = weak.upgrade()
                && let Some(callback) = inner.on_add.borrow().as_ref()
            {
                callback(&path, anchor, body);
            }
        });
        let path = section.path.clone();
        let weak = self.weak();
        view.connect_delete(move |id| {
            if let Some(inner) = weak.upgrade()
                && let Some(callback) = inner.on_delete.borrow().as_ref()
            {
                callback(&path, id);
            }
        });
        let path = section.path.clone();
        let weak = self.weak();
        view.connect_focus(move || {
            if let Some(inner) = weak.upgrade() {
                ReviewPanel(inner).set_current(&path);
            }
        });
        let weak = self.weak();
        view.search().connect_occurrences_count_notify(move |_| {
            if let Some(inner) = weak.upgrade() {
                ReviewPanel(inner).update_search_count();
            }
        });
        view.search().set_highlight(self.search_highlighted());
        let path = section.path.clone();
        let weak = self.weak();
        view.connect_load(move |reply| match weak.upgrade() {
            Some(inner) if inner.on_load.borrow().is_some() => {
                if let Some(callback) = inner.on_load.borrow().as_ref() {
                    callback(&path, reply);
                }
            }
            _ => reply(None),
        });
        section.history_slot.append(view.history_button());
        section.widget.set_body(view.widget());
        {
            let (diff, notes) = &*section.content.borrow();
            view.show(diff, notes);
        }
        section.content.replace((FileDiff::default(), Vec::new()));
        section.view.replace(Some(view));
    }

    // ----- Scrolling -----------------------------------------------------

    fn connect_scrolling(&self) {
        let window = self.0.scroller.window();
        let vadjustment = window.vadjustment();
        let weak = self.weak();
        vadjustment.connect_value_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                ReviewPanel(inner).scrolled();
            }
        });
        let weak = self.weak();
        vadjustment.connect_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                ReviewPanel(inner).update_stuck();
            }
        });
        // Comment cards follow the width.
        let weak = self.weak();
        window.hadjustment().connect_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                let this = ReviewPanel(inner);
                for section in this.0.sections.borrow().iter() {
                    if let Some(view) = section.view.borrow().as_ref() {
                        view.relayout();
                    }
                }
            }
        });

        // Scrolling by hand lets go of a file being scrolled to.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.weak();
        scroll.connect_scroll(move |_, _, _| {
            if let Some(inner) = weak.upgrade() {
                ReviewPanel(inner).stop_settling();
            }
            glib::Propagation::Proceed
        });
        window.add_controller(scroll);

        // The diffs are as tall as their content, so keys that would scroll
        // them scroll the panel instead.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.weak();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(inner) = weak.upgrade() else { return glib::Propagation::Proceed };
            let modified = modifiers.intersects(
                gtk::gdk::ModifierType::SHIFT_MASK
                    | gtk::gdk::ModifierType::CONTROL_MASK
                    | gtk::gdk::ModifierType::ALT_MASK,
            );
            let in_diff = inner.root.root().and_then(|r| r.focus()).is_some_and(|f| f.is::<sourceview5::View>());
            if modified || !in_diff {
                return glib::Propagation::Proceed;
            }
            let scroller = &inner.scroller;
            let page = f64::from(scroller.window().height()) - scroller.inset() - LINE_STEP;
            let delta = match key {
                gtk::gdk::Key::Up | gtk::gdk::Key::KP_Up => -LINE_STEP,
                gtk::gdk::Key::Down | gtk::gdk::Key::KP_Down => LINE_STEP,
                gtk::gdk::Key::Page_Up | gtk::gdk::Key::KP_Page_Up => -page,
                gtk::gdk::Key::Page_Down | gtk::gdk::Key::KP_Page_Down | gtk::gdk::Key::space => page,
                gtk::gdk::Key::Home | gtk::gdk::Key::KP_Home => f64::MIN,
                gtk::gdk::Key::End | gtk::gdk::Key::KP_End => f64::MAX,
                _ => return glib::Propagation::Proceed,
            };
            let this = ReviewPanel(inner);
            this.stop_settling();
            let adjustment = this.0.scroller.window().vadjustment();
            adjustment.set_value(adjustment.value() + delta);
            glib::Propagation::Stop
        });
        self.0.root.add_controller(keys);
    }

    fn scrolled(&self) {
        self.update_stuck();
        if self.0.scrolling.get() || self.0.target.borrow().is_some() {
            return;
        }
        // The file at the top becomes the current one.
        let value = self.0.scroller.window().vadjustment().value();
        if let Some(section) = self.section_at(value + 1.0) {
            self.set_current(&section.path);
        }
    }

    /// The visible section at height `y` of the content.
    fn section_at(&self, y: f64) -> Option<Rc<Section>> {
        self.0
            .sections
            .borrow()
            .iter()
            .filter(|s| s.widget.is_visible())
            .find(|s| {
                self.0
                    .scroller
                    .offset_of(&s.widget)
                    .is_some_and(|top| top <= y && y < top + f64::from(s.widget.height()))
            })
            .cloned()
    }

    /// Re-positions the header stuck to the top, and any header that is out of place:
    /// sections laid out while above the top have their header at their bottom.
    fn update_stuck(&self) {
        let value = self.0.scroller.window().vadjustment().value();
        let stuck = self.section_at(value);
        for section in self.0.sections.borrow().iter() {
            if section.widget.header_offset() != 0 || stuck.as_ref().is_some_and(|s| Rc::ptr_eq(s, section)) {
                section.widget.queue_allocate();
            }
        }
    }

    fn set_current(&self, path: &str) {
        if self.0.current.borrow().as_deref() == Some(path) {
            return;
        }
        self.0.current.replace(Some(path.to_owned()));
        if let Some(callback) = self.0.on_current.borrow().as_ref() {
            callback(path);
        }
    }

    /// Expands a file and scrolls its header to the top.
    pub fn scroll_to(&self, path: &str) {
        let Some(section) = self.section(path) else { return };
        self.set_expanded(&section, true);
        self.hold_at_top(path);
    }

    /// Scrolls a file's header to the top, collapsed or not.
    fn hold_at_top(&self, path: &str) {
        self.0.current.replace(Some(path.to_owned()));
        self.hold(path, Spot::Header);
    }

    /// Keeps `spot` in a file at the top of the screen: sizes above it may
    /// still change as diffs are laid out, so for a few frames.
    fn hold(&self, path: &str, spot: Spot) {
        self.unpin();
        self.0.target.replace(Some((path.to_owned(), spot, SETTLE_FRAMES)));
        self.apply_target();
        if self.0.settle.borrow().is_none() {
            let weak = self.weak();
            let tick = self.0.root.add_tick_callback(move |_, _| {
                let Some(inner) = weak.upgrade() else { return glib::ControlFlow::Break };
                let this = ReviewPanel(inner);
                if this.apply_target() {
                    glib::ControlFlow::Continue
                } else {
                    this.0.settle.replace(None);
                    glib::ControlFlow::Break
                }
            });
            self.0.settle.replace(Some(tick));
        }
    }

    /// Scrolls to the target; returns whether to keep doing so.
    fn apply_target(&self) -> bool {
        let Some((path, spot, frames)) = self.0.target.borrow().clone() else { return false };
        if frames == 0 {
            self.unpin();
            self.0.target.replace(None);
            return false;
        }
        self.0.target.replace(Some((path.clone(), spot, frames - 1)));
        let Some(section) = self.section(&path) else { return true };
        let adjustment = self.0.scroller.window().vadjustment();
        let value = match spot {
            Spot::Header => self.0.scroller.offset_of(&section.widget),
            Spot::Offset(offset) => self.0.scroller.offset_of(&section.widget).map(|top| top + offset),
            Spot::Line => {
                section.view.borrow().as_ref().and_then(DiffView::pinned_delta).map(|delta| adjustment.value() + delta)
            }
        };
        if let Some(value) = value {
            self.0.scrolling.set(true);
            adjustment.set_value(value);
            self.0.scrolling.set(false);
        }
        true
    }

    /// Forgets the line a diff pinned for the target.
    fn unpin(&self) {
        let path = self.0.target.borrow().as_ref().map(|(path, _, _)| path.clone());
        if let Some(view) = path.and_then(|p| self.section(&p)).and_then(|s| s.view.borrow().clone()) {
            view.unpin();
        }
    }

    fn stop_settling(&self) {
        self.unpin();
        self.0.target.replace(None);
        if let Some(tick) = self.0.settle.take() {
            tick.remove();
        }
    }

    // ----- Comments ------------------------------------------------------

    /// Opens the composer on a file: for its selected lines (when `lines` is
    /// set and there is a selection), or for the whole file.
    pub fn open_composer(&self, path: &str, lines: bool) {
        let Some(section) = self.section(path) else { return };
        self.set_expanded(&section, true);
        let view = section.view.borrow().clone();
        if let Some(view) = view {
            view.open_composer(lines);
        }
    }

    // ----- Find ----------------------------------------------------------

    fn connect_search(&self) {
        let search = &self.0.search;
        let weak = self.weak();
        search.entry.connect_search_changed(move |entry| {
            if let Some(inner) = weak.upgrade() {
                let text = entry.text();
                inner.search.settings.set_search_text(Some(text.as_str()).filter(|t| !t.is_empty()));
                let this = ReviewPanel(inner);
                this.highlight_matches();
                this.find(Find::FromSelection);
            }
        });
        let connect = |find: Find| {
            let weak = self.weak();
            move || {
                if let Some(inner) = weak.upgrade() {
                    ReviewPanel(inner).find(find);
                }
            }
        };
        let next = connect(Find::Next);
        search.entry.connect_activate(move |_| next());
        let next = connect(Find::Next);
        search.entry.connect_next_match(move |_| next());
        let next = connect(Find::Next);
        search.next.connect_clicked(move |_| next());
        let previous = connect(Find::Previous);
        search.entry.connect_previous_match(move |_| previous());
        let previous = connect(Find::Previous);
        search.previous.connect_clicked(move |_| previous());

        let keys = gtk::EventControllerKey::new();
        let previous = connect(Find::Previous);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            if matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter)
                && modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK)
            {
                previous();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        search.entry.add_controller(keys);

        let weak = self.weak();
        search.bar.connect_search_mode_enabled_notify(move |bar| {
            if let Some(inner) = weak.upgrade() {
                let this = ReviewPanel(inner);
                this.highlight_matches();
                if !bar.is_search_mode() {
                    for (_, view) in this.searched_views() {
                        view.clear_found();
                    }
                }
            }
        });
    }

    /// Opens the find bar, searching for the selected text if it is on one line.
    pub fn start_search(&self) {
        let search = &self.0.search;
        let current = self.0.current.borrow().clone();
        let view = current.and_then(|p| self.section(&p)).and_then(|s| s.view.borrow().clone());
        if let Some(text) = view.and_then(|v| v.selected_text()) {
            search.entry.set_text(&text);
        }
        search.bar.set_search_mode(true);
        search.entry.grab_focus();
        search.entry.select_region(0, -1);
    }

    fn search_highlighted(&self) -> bool {
        self.0.search.bar.is_search_mode() && !self.0.search.entry.text().is_empty()
    }

    fn highlight_matches(&self) {
        let highlight = self.search_highlighted();
        for (_, view) in self.searched_views() {
            view.search().set_highlight(highlight);
        }
    }

    /// The diffs find searches: those of the expanded files shown.
    fn searched_views(&self) -> Vec<(String, DiffView)> {
        self.0
            .sections
            .borrow()
            .iter()
            .filter(|s| s.expanded.get() && s.widget.is_visible())
            .filter_map(|s| s.view.borrow().clone().map(|v| (s.path.clone(), v)))
            .collect()
    }

    /// Selects a match, moving on to the next files (or previous ones) and wrapping around.
    fn find(&self, find: Find) {
        let views = self.searched_views();
        let current = self.0.current.borrow().clone();
        let start = views.iter().position(|(p, _)| Some(p) == current.as_ref()).unwrap_or(0);
        let count = views.len();
        let backwards = matches!(find, Find::Previous);
        let found = if count == 0 || self.0.search.settings.search_text().is_none() {
            None
        } else if views[start].1.find(find) {
            Some(start)
        } else {
            (1..=count)
                .map(|k| if backwards { (start + count - k) % count } else { (start + k) % count })
                .find(|&i| views[i].1.find(if backwards { Find::Last } else { Find::First }))
        };
        if let Some(found) = found {
            for (i, (_, view)) in views.iter().enumerate() {
                if i != found {
                    view.clear_found();
                }
            }
            self.set_current(&views[found].0);
        }
        self.update_search_count();
    }

    fn update_search_count(&self) {
        let search = &self.0.search;
        let (mut total, mut position, mut counting) = (0, 0, false);
        for (_, view) in self.searched_views() {
            let (count, selected) = view.search_position();
            if count < 0 {
                counting = true;
                continue;
            }
            if selected > 0 && position == 0 {
                position = total + selected;
            }
            total += count;
        }
        let searching = search.settings.search_text().is_some();
        let text = match total {
            _ if !searching || counting => String::new(),
            0 => "No matches".to_owned(),
            total if position > 0 => format!("{position} of {total}"),
            1 => "1 match".to_owned(),
            total => format!("{total} matches"),
        };
        search.count.set_text(&text);
        if total == 0 && searching && !counting {
            search.entry.add_css_class("error");
        } else {
            search.entry.remove_css_class("error");
        }
        search.previous.set_sensitive(total > 0);
        search.next.set_sensitive(total > 0);
    }
}

fn build_search() -> Search {
    // Each file's search stops at its end: `ReviewPanel::find` moves on to the next one.
    let settings = sourceview5::SearchSettings::new();
    settings.set_wrap_around(false);

    let entry = gtk::SearchEntry::builder().placeholder_text("Find in files").width_chars(28).build();
    let count = util::label("", &["search-count"]);
    count.set_width_chars(11);
    let previous = gtk::Button::builder()
        .icon_name("go-up-symbolic")
        .tooltip_text("Previous Match (Shift+Enter)")
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    let next = gtk::Button::builder()
        .icon_name("go-down-symbolic")
        .tooltip_text("Next Match (Enter)")
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    for button in [&previous, &next] {
        button.add_css_class("flat");
        button.add_css_class("icon-button");
    }
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    controls.append(&entry);
    controls.append(&count);
    controls.append(&previous);
    controls.append(&next);
    let bar = gtk::SearchBar::builder().child(&controls).show_close_button(true).build();
    bar.connect_entry(&entry);
    Search { bar, entry, count, previous, next, settings }
}

// ----- Section widget ------------------------------------------------------

glib::wrapper! {
    /// A file's header above its diff. While the diff scrolls past the top
    /// of the panel, the header stays there.
    pub struct SectionBox(ObjectSubclass<imp::SectionBox>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SectionBox {
    fn new(header: &impl IsA<gtk::Widget>, scroller: &Scroller) -> Self {
        let this: Self = glib::Object::new();
        header.set_parent(&this);
        this.imp().header.replace(Some(header.clone().upcast()));
        this.imp().scroller.replace(Some(scroller.clone()));
        this
    }

    /// How far the header is from the top, when stuck.
    fn header_offset(&self) -> i32 {
        self.imp().offset.get()
    }

    /// Puts `body` under the header, which is drawn over it when stuck.
    fn set_body(&self, body: &impl IsA<gtk::Widget>) {
        body.insert_before(self, self.imp().header.borrow().as_ref());
        self.imp().body.replace(Some(body.clone().upcast()));
    }
}

mod imp {
    use std::cell::{Cell, RefCell};

    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk::{glib, graphene, gsk};

    use crate::ui::scroller::Scroller;

    #[derive(Default)]
    pub struct SectionBox {
        pub header: RefCell<Option<gtk::Widget>>,
        pub body: RefCell<Option<gtk::Widget>>,
        pub scroller: RefCell<Option<Scroller>>,
        /// The header's distance from the top, set while stuck.
        pub offset: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SectionBox {
        const NAME: &'static str = "AgentReviewSection";
        type Type = super::SectionBox;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("filesection");
        }
    }

    impl ObjectImpl for SectionBox {
        fn dispose(&self) {
            for child in [self.body.take(), self.header.take()].into_iter().flatten() {
                child.unparent();
            }
            self.scroller.take();
        }
    }

    impl SectionBox {
        fn children(&self) -> impl Iterator<Item = gtk::Widget> {
            [self.header.borrow().clone(), self.body.borrow().clone()]
                .into_iter()
                .flatten()
                .filter(WidgetExt::should_layout)
        }
    }

    impl WidgetImpl for SectionBox {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (mut minimum, mut natural) = (0, 0);
            for child in self.children() {
                let (min, nat, _, _) = child.measure(orientation, for_size);
                if orientation == gtk::Orientation::Vertical {
                    minimum += min;
                    natural += nat;
                } else {
                    minimum = minimum.max(min);
                    natural = natural.max(nat);
                }
            }
            (minimum, natural, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let scroller = self.scroller.borrow().clone();
            // Where the panel's visible area begins, in this widget's coordinates.
            let origin = scroller
                .as_ref()
                .and_then(|s| s.window().compute_point(&*self.obj(), &graphene::Point::zero()))
                .unwrap_or_else(graphene::Point::zero);

            let mut header_height = 0;
            if let Some(header) = self.header.borrow().as_ref().filter(|h| h.should_layout()) {
                header_height = header.measure(gtk::Orientation::Vertical, width).0;
                // Stuck while the section passes the top, pushed up by the next one; a
                // section entirely above the top keeps it in place, off screen.
                #[allow(clippy::cast_possible_truncation)]
                let top = origin.y().round() as i32;
                let y = if top >= height { 0 } else { top.clamp(0, (height - header_height).max(0)) };
                #[allow(clippy::cast_precision_loss)]
                let at = gsk::Transform::new().translate(&graphene::Point::new(0.0, y as f32));
                header.allocate(width, header_height, -1, Some(at));
                self.offset.set(y);
                if let Some(scroller) = &scroller {
                    scroller.set_inset(f64::from(header_height));
                }
            }
            if let Some(body) = self.body.borrow().as_ref().filter(|b| b.should_layout()) {
                #[allow(clippy::cast_precision_loss)]
                let at = gsk::Transform::new().translate(&graphene::Point::new(0.0, header_height as f32));
                body.allocate(width, (height - header_height).max(0), -1, Some(at));
            }
        }
    }
}
