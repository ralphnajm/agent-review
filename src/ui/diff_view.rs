//! Read-only, syntax highlighted rendering of one file's diff, with comment
//! threads shown inline below the lines they refer to.
//!
//! The buffer holds the diff lines without their `+`/`-` prefixes so the
//! file's language highlighting applies; old/new line numbers and the change
//! marker are drawn in a custom gutter. Comment cards are read-only child
//! widgets anchored on otherwise empty buffer lines.
//!
//! The comment composer is deliberately *not* a child of the text view: an
//! editable widget inside a `GtkTextView` behaves oddly and GTK crashes when
//! it is removed under the pointer. It floats in an overlay instead, over a gap
//! opened below the target lines, so it still reads as inline.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::ops::Range;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use sourceview5::prelude::*;

use crate::core::align::{self, Run};
use crate::core::diff::{FileDiff, LineKind};
use crate::core::review::{LineAnchor, Note, Side};
use crate::ui::util;

/// Comment cards are not stretched wider than this.
const MAX_THREAD_WIDTH: i32 = 900;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Hunk,
    Line(LineKind),
    /// An empty line holding a comment thread.
    Widget,
    /// A message for diffs without content, e.g. binary files.
    Info,
    /// An empty line the floating composer sits on.
    Spacer,
}

#[derive(Debug, Clone, Copy)]
struct Row {
    kind: RowKind,
    old_no: Option<u32>,
    new_no: Option<u32>,
    /// Index into `FileDiff::lines()` for code lines.
    index: Option<usize>,
}

impl Row {
    const fn plain(kind: RowKind) -> Self {
        Self { kind, old_no: None, new_no: None, index: None }
    }
}

/// Which match `find` selects.
#[derive(Debug, Clone, Copy)]
enum Find {
    /// The first one at or after the selection: the search text changed.
    FromSelection,
    Next,
    Previous,
}

/// What changed since the last render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    /// Another file is shown.
    File,
    /// The file's diff changed.
    Content,
    /// Only the comments changed.
    Notes,
}

/// What the open composer comments on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    anchor: Option<LineAnchor>,
    /// Inclusive range of `FileDiff::lines()` indices, for line comments.
    range: Option<(usize, usize)>,
}

type AddCallback = Box<dyn Fn(Option<LineAnchor>, String)>;
type DeleteCallback = Box<dyn Fn(u64)>;

struct Composer {
    root: gtk::Box,
    title: gtk::Label,
    text: gtk::TextView,
    cancel: gtk::Button,
    submit: gtk::Button,
}

/// The find bar above the diff.
struct Search {
    bar: gtk::SearchBar,
    entry: gtk::SearchEntry,
    count: gtk::Label,
    previous: gtk::Button,
    next: gtk::Button,
    settings: sourceview5::SearchSettings,
    context: sourceview5::SearchContext,
}

#[derive(Clone)]
pub struct DiffView(Rc<Inner>);

struct Inner {
    root: gtk::Box,
    search: Search,
    overlay: gtk::Overlay,
    scrolled: gtk::ScrolledWindow,
    view: sourceview5::View,
    buffer: sourceview5::Buffer,
    gutter: sourceview5::GutterRendererText,
    composer: Composer,
    rows: RefCell<Vec<Row>>,
    /// Width of each line-number column, in characters.
    number_width: Cell<usize>,
    diff: RefCell<Option<FileDiff>>,
    notes: RefCell<Vec<Note>>,
    target: RefCell<Option<Target>>,
    /// Child widgets in the buffer, resized with the view.
    children: RefCell<Vec<gtk::Widget>>,
    child_width: Cell<i32>,
    /// The spacer line the composer floats over.
    gap: Cell<Option<usize>>,
    /// Height reserved for the composer, in pixels.
    gap_height: Cell<i32>,
    /// Line where a gutter selection started, for shift-click.
    selection_origin: Cell<Option<i32>>,
    /// Pointer height in the view while selecting lines from the gutter.
    drag_y: Cell<Option<f64>>,
    /// Scrolls the view while that pointer is past its top or bottom edge.
    autoscroll: RefCell<Option<gtk::TickCallbackId>>,
    on_add: RefCell<Option<AddCallback>>,
    on_delete: RefCell<Option<DeleteCallback>>,
}

impl DiffView {
    pub fn new() -> Self {
        let buffer = sourceview5::Buffer::new(None);
        buffer.set_highlight_matching_brackets(false);
        buffer.set_enable_undo(false);
        let view = sourceview5::View::with_buffer(&buffer);
        view.set_editable(false);
        view.set_cursor_visible(false);
        view.set_left_margin(12);
        view.set_top_margin(8);
        view.set_bottom_margin(96);
        view.add_css_class("diff-view");

        let scrolled = gtk::ScrolledWindow::builder().child(&view).vexpand(true).hexpand(true).build();
        let overlay = gtk::Overlay::builder().child(&scrolled).overflow(gtk::Overflow::Hidden).build();
        let composer = build_composer();
        composer.root.set_visible(false);
        overlay.add_overlay(&composer.root);
        let search = build_search(&buffer);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&search.bar);
        root.append(&overlay);

        let gutter = sourceview5::GutterRendererText::new();
        gutter.set_xpad(8);
        gutter.set_xalign(0.0);
        ViewExt::gutter(&view, gtk::TextWindowType::Left).insert(&gutter, 0);

        let this = Self(Rc::new(Inner {
            root,
            search,
            overlay,
            scrolled,
            view,
            buffer,
            gutter,
            composer,
            rows: RefCell::default(),
            number_width: Cell::new(3),
            diff: RefCell::default(),
            notes: RefCell::default(),
            target: RefCell::default(),
            children: RefCell::default(),
            child_width: Cell::new(0),
            gap: Cell::new(None),
            gap_height: Cell::new(0),
            selection_origin: Cell::new(None),
            drag_y: Cell::new(None),
            autoscroll: RefCell::default(),
            on_add: RefCell::default(),
            on_delete: RefCell::default(),
        }));
        this.create_tags();
        this.update_style_scheme();
        this.connect_signals();
        this.connect_composer();
        this.connect_search();
        this
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.0.root
    }

    /// Called when a comment is written: on the anchored lines, or the file for `None`.
    pub fn connect_add(&self, callback: impl Fn(Option<LineAnchor>, String) + 'static) {
        self.0.on_add.replace(Some(Box::new(callback)));
    }

    pub fn connect_delete(&self, callback: impl Fn(u64) + 'static) {
        self.0.on_delete.replace(Some(Box::new(callback)));
    }

    fn weak(&self) -> std::rc::Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    fn create_tags(&self) {
        let table = self.0.buffer.tag_table();
        let tag = |name: &str, rgba: (f32, f32, f32, f32)| {
            let tag = gtk::TextTag::builder()
                .name(name)
                .paragraph_background_rgba(&gtk::gdk::RGBA::new(rgba.0, rgba.1, rgba.2, rgba.3))
                .build();
            table.add(&tag);
            tag
        };
        let grey = gtk::gdk::RGBA::new(0.5, 0.5, 0.55, 1.0);
        tag("added", (0.18, 0.76, 0.49, 0.13));
        tag("removed", (0.88, 0.11, 0.14, 0.11));
        let hunk = tag("hunk", (0.5, 0.5, 0.55, 0.08));
        hunk.set_pixels_above_lines(4);
        hunk.set_pixels_below_lines(4);
        hunk.set_foreground_rgba(Some(&grey));
        let info = tag("info", (0.0, 0.0, 0.0, 0.0));
        info.set_style(gtk::pango::Style::Italic);
        info.set_pixels_above_lines(12);
        info.set_foreground_rgba(Some(&grey));
        // Created last so it paints over the diff colours: the lines being commented on.
        tag("target", (0.21, 0.52, 0.89, 0.2));
        // The match the find bar is on: the selection is faint while the bar has focus.
        table.add(
            &gtk::TextTag::builder().name("found").background_rgba(&gtk::gdk::RGBA::new(0.96, 0.6, 0.0, 0.6)).build(),
        );
        // Room for the floating composer; its height is set in `relayout`.
        table.add(&gtk::TextTag::builder().name("gap").build());
    }

    fn update_style_scheme(&self) {
        let dark = adw::StyleManager::default().is_dark();
        let manager = sourceview5::StyleSchemeManager::default();
        let scheme = manager
            .scheme(if dark { "Adwaita-dark" } else { "Adwaita" })
            .or_else(|| manager.scheme(if dark { "classic-dark" } else { "classic" }));
        self.0.buffer.set_style_scheme(scheme.as_ref());
    }

    fn connect_signals(&self) {
        let weak = self.weak();
        adw::StyleManager::default().connect_dark_notify(move |_| {
            if let Some(inner) = weak.upgrade() {
                DiffView(inner).update_style_scheme();
            }
        });

        let weak = self.weak();
        self.0.gutter.connect_query_data(move |renderer, _, line| {
            if let Some(inner) = weak.upgrade() {
                renderer.set_markup(&DiffView(inner).gutter_markup(line));
            }
        });

        // Pressing on the line numbers selects whole lines and dragging extends the
        // selection; releasing comments on it. Shift-click extends the last selection.
        let gutter = ViewExt::gutter(&self.0.view, gtk::TextWindowType::Left);
        let drag = gtk::GestureDrag::new();
        let weak = self.weak();
        drag.connect_drag_begin(move |gesture, x, y| {
            let Some(inner) = weak.upgrade() else { return };
            // Keep the text view from starting a selection of its own.
            gesture.set_state(gtk::EventSequenceState::Claimed);
            let this = DiffView(inner);
            let extend = gesture.current_event_state().contains(gtk::gdk::ModifierType::SHIFT_MASK);
            if let Some(y) = this.gutter_to_view_y(x, y) {
                this.begin_line_drag(y, extend);
            }
        });
        let weak = self.weak();
        drag.connect_drag_update(move |gesture, dx, dy| {
            if let Some(inner) = weak.upgrade()
                && let Some((x, y)) = gesture.start_point()
            {
                let this = DiffView(inner);
                if let Some(y) = this.gutter_to_view_y(x + dx, y + dy) {
                    this.0.drag_y.set(Some(y));
                    this.extend_line_drag();
                }
            }
        });
        let weak = self.weak();
        drag.connect_drag_end(move |_, _, _| {
            if let Some(inner) = weak.upgrade() {
                DiffView(inner).end_line_drag();
            }
        });
        gutter.add_controller(drag);

        // Keep comment cards and the composer in step with size and scroll changes.
        let hadjustment = self.0.scrolled.hadjustment();
        let vadjustment = self.0.scrolled.vadjustment();
        for adjustment in [&hadjustment, &vadjustment] {
            let weak = self.weak();
            adjustment.connect_value_changed(move |_| {
                if let Some(inner) = weak.upgrade() {
                    DiffView(inner).relayout();
                }
            });
            let weak = self.weak();
            adjustment.connect_changed(move |_| {
                if let Some(inner) = weak.upgrade() {
                    DiffView(inner).relayout();
                }
            });
        }

        // Positioned while the overlay is allocated, after the text view it
        // floats over, so both are drawn from the same layout.
        let weak = self.weak();
        self.0.overlay.connect_get_child_position(move |_, widget| {
            let inner = weak.upgrade()?;
            if widget == inner.composer.root.upcast_ref::<gtk::Widget>() {
                DiffView(inner).composer_rect()
            } else {
                None
            }
        });
    }

    fn connect_composer(&self) {
        let composer = &self.0.composer;
        let keys = gtk::EventControllerKey::new();
        let weak = self.weak();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(inner) = weak.upgrade() else { return glib::Propagation::Proceed };
            let this = DiffView(inner);
            match key {
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter
                    if modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK) =>
                {
                    this.submit();
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::Escape => {
                    this.close_composer();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        composer.text.add_controller(keys);

        let weak = self.weak();
        composer.cancel.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                DiffView(inner).close_composer();
            }
        });
        let weak = self.weak();
        composer.submit.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                DiffView(inner).submit();
            }
        });
        let submit = composer.submit.clone();
        let weak = self.weak();
        composer.text.buffer().connect_changed(move |buffer| {
            submit.set_sensitive(!buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).trim().is_empty());
            // The composer grows with its text: make room once it has been measured.
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(inner) = weak.upgrade() {
                    DiffView(inner).relayout();
                }
            });
        });
        composer.submit.set_sensitive(false);
    }

    fn connect_search(&self) {
        let search = &self.0.search;
        let weak = self.weak();
        search.entry.connect_search_changed(move |entry| {
            if let Some(inner) = weak.upgrade() {
                let text = entry.text();
                inner.search.settings.set_search_text(Some(text.as_str()).filter(|t| !t.is_empty()));
                inner.search.context.set_highlight(!text.is_empty());
                DiffView(inner).find(Find::FromSelection);
            }
        });
        let connect = |find: Find| {
            let weak = self.weak();
            move || {
                if let Some(inner) = weak.upgrade() {
                    DiffView(inner).find(find);
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
                let open = bar.is_search_mode() && !inner.search.entry.text().is_empty();
                inner.search.context.set_highlight(open);
                if !bar.is_search_mode() {
                    let (start, end) = inner.buffer.bounds();
                    inner.buffer.remove_tag_by_name("found", &start, &end);
                    inner.view.grab_focus();
                }
            }
        });
        let weak = self.weak();
        search.context.connect_occurrences_count_notify(move |_| {
            if let Some(inner) = weak.upgrade() {
                DiffView(inner).update_search_count();
            }
        });
    }

    /// Opens the find bar, searching for the selected text if it is on one line.
    pub fn start_search(&self) {
        let search = &self.0.search;
        let buffer = &self.0.buffer;
        if let Some((start, end)) = buffer.selection_bounds()
            && start.line() == end.line()
        {
            search.entry.set_text(&buffer.text(&start, &end, false));
        }
        search.bar.set_search_mode(true);
        search.entry.grab_focus();
        search.entry.select_region(0, -1);
    }

    /// Selects a match and scrolls to it.
    fn find(&self, find: Find) {
        let search = &self.0.search;
        let buffer = &self.0.buffer;
        if search.settings.search_text().is_some() {
            // From the selection, or else the top of the view.
            let (start, end) = buffer.selection_bounds().unwrap_or_else(|| {
                let top = buffer.iter_at_line(self.line_at_view_y(0.0)).unwrap_or_else(|| buffer.start_iter());
                (top, top)
            });
            let found = match find {
                Find::FromSelection => search.context.forward(&start),
                Find::Next => search.context.forward(&end),
                Find::Previous => search.context.backward(&start),
            };
            let (first, last) = buffer.bounds();
            buffer.remove_tag_by_name("found", &first, &last);
            if let Some((start, end, _)) = found {
                // Above the match highlighting, whose tag is created on first use.
                let table = buffer.tag_table();
                if let Some(tag) = table.lookup("found") {
                    tag.set_priority(table.size() - 1);
                }
                buffer.apply_tag_by_name("found", &start, &end);
                buffer.select_range(&start, &end);
                self.0.view.scroll_to_mark(&buffer.get_insert(), 0.15, false, 0.0, 0.0);
            }
        }
        self.update_search_count();
    }

    fn update_search_count(&self) {
        let search = &self.0.search;
        let total = search.context.occurrences_count();
        let position =
            self.0.buffer.selection_bounds().map_or(0, |(start, end)| search.context.occurrence_position(&start, &end));
        let text = match total {
            _ if search.settings.search_text().is_none() => String::new(),
            // Still counting.
            ..0 => String::new(),
            0 => "No matches".to_owned(),
            total if position > 0 => format!("{position} of {total}"),
            1 => "1 match".to_owned(),
            total => format!("{total} matches"),
        };
        search.count.set_text(&text);
        if total == 0 && search.settings.search_text().is_some() {
            search.entry.add_css_class("error");
        } else {
            search.entry.remove_css_class("error");
        }
        search.previous.set_sensitive(total > 0);
        search.next.set_sensitive(total > 0);
    }

    /// Sizes the comment cards and the gap the composer floats over.
    fn relayout(&self) {
        let inner = &self.0;
        let gutter_width = ViewExt::gutter(&inner.view, gtk::TextWindowType::Left).width();
        let width = (inner.scrolled.width() - gutter_width - 64).clamp(240, MAX_THREAD_WIDTH);
        if width != inner.child_width.get() {
            inner.child_width.set(width);
            for child in inner.children.borrow().iter() {
                child.set_width_request(width);
            }
        }

        // Reserve room for the composer below (or above) its line.
        let composer = &inner.composer.root;
        let gap = inner.gap.get().filter(|_| composer.is_visible());
        let height = if gap.is_some() { composer.measure(gtk::Orientation::Vertical, width).1 } else { 0 };
        let reserved = if height > 0 { height + 16 } else { 0 };
        if reserved != inner.gap_height.get() {
            inner.gap_height.set(reserved);
            if let Some(tag) = inner.buffer.tag_table().lookup("gap") {
                tag.set_pixels_below_lines(reserved);
            }
        }

        if gap.is_some() {
            inner.overlay.queue_allocate();
        }
    }

    /// Where the composer floats: over the gap below its line.
    fn composer_rect(&self) -> Option<gtk::gdk::Rectangle> {
        let inner = &self.0;
        let line = inner.gap.get()?;
        let width = inner.child_width.get();
        let (_, height, _, _) = inner.composer.root.measure(gtk::Orientation::Vertical, width);
        let (y, _) = inner.view.line_yrange(&iter_at_line(&inner.buffer, line));
        let (x, top) = self.to_overlay(0, y + 8)?;
        Some(gtk::gdk::Rectangle::new(x + inner.view.left_margin(), top, width, height))
    }

    /// Converts buffer coordinates to overlay coordinates.
    fn to_overlay(&self, x: i32, y: i32) -> Option<(i32, i32)> {
        let (wx, wy) = self.0.view.buffer_to_window_coords(gtk::TextWindowType::Widget, x, y);
        #[allow(clippy::cast_precision_loss)]
        let point = gtk::graphene::Point::new(wx as f32, wy as f32);
        let point = self.0.view.compute_point(&self.0.overlay, &point)?;
        #[allow(clippy::cast_possible_truncation)]
        Some((point.x().round() as i32, point.y().round() as i32))
    }

    /// The buffer line showing `FileDiff::lines()[index]`.
    fn buffer_line_of(&self, index: usize) -> Option<usize> {
        self.0.rows.borrow().iter().position(|r| r.index == Some(index))
    }

    /// Converts a height in the gutter to the view's coordinates.
    fn gutter_to_view_y(&self, x: f64, y: f64) -> Option<f64> {
        let gutter = ViewExt::gutter(&self.0.view, gtk::TextWindowType::Left);
        #[allow(clippy::cast_possible_truncation)]
        let point = gtk::graphene::Point::new(x as f32, y as f32);
        gutter.compute_point(&self.0.view, &point).map(|p| f64::from(p.y()))
    }

    /// The buffer line shown at height `y` of the view, clamped to the visible area.
    fn line_at_view_y(&self, y: f64) -> i32 {
        let view = &self.0.view;
        let (_, origin) = view.buffer_to_window_coords(gtk::TextWindowType::Widget, 0, 0);
        #[allow(clippy::cast_possible_truncation)]
        let y = y.clamp(0.0, f64::from(view.height() - 1)) as i32;
        view.line_at_y(y - origin).0.line()
    }

    /// Starts selecting lines from the line number at height `y` of the view.
    fn begin_line_drag(&self, y: f64, extend: bool) {
        let line = self.line_at_view_y(y);
        if !extend || self.0.selection_origin.get().is_none() {
            self.0.selection_origin.set(Some(line));
        }
        self.0.drag_y.set(Some(y));
        self.0.view.grab_focus();
        self.extend_line_drag();
        let weak = self.weak();
        let tick = self.0.view.add_tick_callback(move |_, _| {
            let Some(inner) = weak.upgrade() else { return glib::ControlFlow::Break };
            DiffView(inner).autoscroll();
            glib::ControlFlow::Continue
        });
        if let Some(previous) = self.0.autoscroll.replace(Some(tick)) {
            previous.remove();
        }
    }

    /// The buffer lines from the drag's origin to the pointer.
    fn dragged_lines(&self) -> Option<(i32, i32)> {
        let (origin, y) = (self.0.selection_origin.get()?, self.0.drag_y.get()?);
        let line = self.line_at_view_y(y);
        Some((origin.min(line), origin.max(line)))
    }

    fn extend_line_drag(&self) {
        if let Some((first, last)) = self.dragged_lines() {
            self.select_lines(first, last);
        }
    }

    /// Scrolls towards the pointer while it is dragged past the top or bottom edge.
    fn autoscroll(&self) {
        let Some(y) = self.0.drag_y.get() else { return };
        let height = f64::from(self.0.view.height());
        let overshoot = if y < 0.0 { y } else { (y - height).max(0.0) };
        if overshoot != 0.0 {
            let adjustment = self.0.scrolled.vadjustment();
            adjustment.set_value(adjustment.value() + overshoot.clamp(-60.0, 60.0) / 3.0);
            self.extend_line_drag();
        }
    }

    /// Comments on the selected lines once the button is released.
    fn end_line_drag(&self) {
        let lines = self.dragged_lines().and_then(|(first, last)| self.covering(first, last));
        self.0.drag_y.set(None);
        if let Some(tick) = self.0.autoscroll.take() {
            tick.remove();
        }
        if lines.is_some() {
            self.open_composer_on(lines);
        }
    }

    fn gutter_markup(&self, line: u32) -> String {
        let Some(row) = usize::try_from(line).ok().and_then(|l| self.0.rows.borrow().get(l).copied()) else {
            return String::new();
        };
        let width = self.0.number_width.get();
        let number = |n: Option<u32>| n.map_or_else(String::new, |n| n.to_string());
        match row.kind {
            RowKind::Widget | RowKind::Info | RowKind::Spacer => String::new(),
            RowKind::Hunk => format!("<span alpha=\"40%\">{:>w$}</span>", "⋯", w = width * 2 + 1),
            RowKind::Line(kind) => {
                let sign = match kind {
                    LineKind::Added => "<span foreground=\"#2ec27e\" weight=\"bold\">+</span>",
                    LineKind::Removed => "<span foreground=\"#ed333b\" weight=\"bold\">−</span>",
                    LineKind::Context => " ",
                };
                format!(
                    "<span alpha=\"50%\">{:>w$} {:>w$}</span> {sign}",
                    number(row.old_no),
                    number(row.new_no),
                    w = width
                )
            }
        }
    }

    /// Shows `diff` with its comments. Showing the same content again keeps
    /// the scroll position and any comment being written.
    pub fn show(&self, diff: &FileDiff, notes: &[Note]) {
        let (same_file, same_content) = match self.0.diff.borrow().as_ref() {
            Some(current) if current.path() == diff.path() => (true, current.fingerprint() == diff.fingerprint()),
            _ => (false, false),
        };
        if same_content && *self.0.notes.borrow() == notes {
            return;
        }
        if !same_file {
            self.close_composer();
        } else if !same_content && self.0.target.borrow().as_ref().is_some_and(|t| t.anchor.is_some()) {
            // The lines under an open line comment may have moved: re-resolve them.
            let target = self.0.target.borrow().clone();
            let resolved = target.and_then(|t| t.anchor).and_then(|anchor| {
                let first = diff.locate(anchor.side == Side::Old, anchor.line)?;
                let last = diff.locate(anchor.side == Side::Old, anchor.last_line())?;
                Some(Target { anchor: Some(anchor), range: Some((first, last)) })
            });
            self.0.target.replace(resolved);
        }
        self.0.diff.replace(Some(diff.clone()));
        self.0.notes.replace(notes.to_vec());
        self.render(match (same_file, same_content) {
            (false, _) => Change::File,
            (true, false) => Change::Content,
            (true, true) => Change::Notes,
        });
    }

    pub fn clear(&self) {
        self.close_composer();
        for child in self.0.children.borrow_mut().drain(..) {
            self.0.view.remove(&child);
        }
        self.0.buffer.set_text("");
        self.0.rows.borrow_mut().clear();
        self.0.diff.replace(None);
        self.0.notes.borrow_mut().clear();
    }

    /// Forgets what is shown, so the next `show` re-renders.
    pub fn invalidate(&self) {
        self.0.diff.replace(None);
    }

    fn render(&self, change: Change) {
        let diff_ref = self.0.diff.borrow();
        let Some(diff) = diff_ref.as_ref() else { return };
        let notes = self.0.notes.borrow();
        let target = self.0.target.borrow().clone();

        // Group notes by the line they follow; outdated and file notes go on top.
        let mut by_line: BTreeMap<usize, Vec<&Note>> = BTreeMap::new();
        let mut top: Vec<&Note> = Vec::new();
        for note in notes.iter() {
            let end = note
                .anchor
                .as_ref()
                .filter(|a| a.is_current(Some(diff)))
                .and_then(|a| diff.locate(a.side == Side::Old, a.last_line()));
            match end {
                Some(end) => by_line.entry(end).or_default().push(note),
                None => top.push(note),
            }
        }
        // Every line, the last included, ends in a newline so line tags can apply to it.
        let mut text = String::new();
        let mut rows = Vec::with_capacity(diff.line_count() + by_line.len() + 2);
        let mut children: Vec<gtk::Widget> = Vec::new();

        // The composer gets an empty line of its own, below the target lines and their comments.
        let spacer_after = target.as_ref().map(|t| t.range.map(|(_, last)| last));
        if !top.is_empty() {
            text.push('\n');
            rows.push(Row::plain(RowKind::Widget));
            children.push(self.build_thread(&top, diff).upcast());
        }
        if spacer_after == Some(None) {
            text.push('\n');
            rows.push(Row::plain(RowKind::Spacer));
        }
        if diff.hunks.is_empty() {
            let _ = writeln!(text, "{}", describe_without_hunks(diff));
            rows.push(Row::plain(RowKind::Info));
        }
        let mut max_line = 0;
        for (index, (hunk_start, line)) in diff
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter().enumerate().map(move |(i, l)| ((i == 0).then_some(h), l)))
            .enumerate()
        {
            if let Some(hunk) = hunk_start {
                let _ = writeln!(text, "{}", hunk.header);
                rows.push(Row::plain(RowKind::Hunk));
            }
            text.push_str(&line.text);
            text.push('\n');
            rows.push(Row {
                kind: RowKind::Line(line.kind),
                old_no: line.old_no,
                new_no: line.new_no,
                index: Some(index),
            });
            max_line = max_line.max(line.old_no.unwrap_or(0)).max(line.new_no.unwrap_or(0));

            if let Some(thread) = by_line.get(&index) {
                text.push('\n');
                rows.push(Row::plain(RowKind::Widget));
                children.push(self.build_thread(thread, diff).upcast());
            }
            if spacer_after == Some(Some(index)) {
                text.push('\n');
                rows.push(Row::plain(RowKind::Spacer));
            }
        }
        let width = max_line.to_string().len().max(3);
        self.0.number_width.set(width);
        let sample = format!("{} {} −", "0".repeat(width), "0".repeat(width));
        // Measured with the gutter's own (monospace) font.
        let (sample_width, _) = self.0.gutter.create_pango_layout(Some(&sample)).pixel_size();
        self.0.gutter.set_width_request(sample_width + 2 * self.0.gutter.xpad() + 4);

        let buffer = &self.0.buffer;
        let language = sourceview5::LanguageManager::default().guess_language(Some(diff.path()), None);
        if buffer.language() != language {
            buffer.set_language(language.as_ref());
        }
        drop(notes);
        drop(diff_ref);

        // Detach the cards before their anchors are deleted: GTK crashes if a
        // child under the pointer is removed while the buffer is mid-update.
        for child in self.0.children.borrow_mut().drain(..) {
            self.0.view.remove(&child);
        }
        if change == Change::File || !self.update_lines(&rows, &text) {
            buffer.set_text(&text);
            self.0.rows.replace(rows);
            let lines = self.0.rows.borrow().len();
            self.apply_line_tags(0..lines);
            buffer.place_cursor(&buffer.start_iter());
            self.0.scrolled.vadjustment().set_value(0.0);
        }
        let (start, end) = buffer.bounds();
        buffer.remove_tag_by_name("target", &start, &end);
        self.apply_target(target.as_ref());
        for (line, child) in self.widget_lines().into_iter().zip(&children) {
            // Threads on lines that were kept still have their anchor.
            let anchor = iter_at_line(buffer, line)
                .child_anchor()
                .unwrap_or_else(|| buffer.create_child_anchor(&mut iter_at_line(buffer, line)));
            self.0.view.add_child_at_anchor(child, &anchor);
        }
        self.0.children.replace(children);
        self.relayout();
    }

    /// Brings the buffer to `text`, one line per row, rewriting only the lines
    /// that changed. The text view keeps its first visible line where it is as
    /// long as that line's start survives, so edits elsewhere don't move what is
    /// shown, and unchanged lines keep their highlighting. Returns `false`, having
    /// changed nothing, if the buffer is out of step with its rows.
    fn update_lines(&self, rows: &[Row], text: &str) -> bool {
        let buffer = &self.0.buffer;
        let current = buffer.slice(&buffer.start_iter(), &buffer.end_iter(), true);
        let new_lines: Vec<&str> = text.split_inclusive('\n').collect();
        let runs = {
            let old_rows = self.0.rows.borrow();
            let old_lines: Vec<&str> = current.split_inclusive('\n').collect();
            if old_lines.len() != old_rows.len() || new_lines.len() != rows.len() {
                return false;
            }
            align::runs(&line_keys(&old_rows, &old_lines), &line_keys(rows, &new_lines))
        };
        self.0.rows.replace(rows.to_vec());

        // Lines before `line` are up to date; the old ones follow.
        let mut line = 0;
        for run in runs {
            match run {
                Run::Keep(n) => line += n,
                Run::Change { delete, insert } => {
                    // Rewrite changed lines in place, so their starts survive.
                    let paired = delete.min(insert);
                    for (i, new_line) in new_lines.iter().enumerate().skip(line).take(paired) {
                        let mut end = iter_at_line(buffer, i + 1);
                        end.backward_char();
                        buffer.delete(&mut iter_at_line(buffer, i), &mut end);
                        buffer.insert(&mut iter_at_line(buffer, i), new_line.trim_end_matches('\n'));
                    }
                    if delete > paired {
                        buffer
                            .delete(&mut iter_at_line(buffer, line + paired), &mut iter_at_line(buffer, line + delete));
                    }
                    if insert > paired {
                        let at = line + paired;
                        let added = new_lines[at..line + insert].concat();
                        if at == 0 {
                            buffer.insert(&mut buffer.start_iter(), &added);
                        } else {
                            // Before the previous line's newline rather than at the start of
                            // the next line: should that one be at the top of the view, the
                            // view keeps it there instead of showing the new lines above it.
                            let mut end = iter_at_line(buffer, at);
                            end.backward_char();
                            buffer.insert(&mut end, &format!("\n{}", &added[..added.len() - 1]));
                        }
                    }
                    // From the line before, whose newline may have moved.
                    self.retag_lines(line.saturating_sub(1)..line + insert);
                    line += insert;
                }
            }
        }
        true
    }

    /// Colours buffer lines `lines` afresh: they were rewritten, and inserted
    /// text takes the tags around it.
    fn retag_lines(&self, lines: Range<usize>) {
        if lines.is_empty() {
            return;
        }
        let buffer = &self.0.buffer;
        let (from, to) = (iter_at_line(buffer, lines.start), iter_at_line(buffer, lines.end));
        for tag in ["added", "removed", "hunk", "info", "gap", "found"] {
            buffer.remove_tag_by_name(tag, &from, &to);
        }
        self.apply_line_tags(lines);
    }

    fn widget_lines(&self) -> Vec<usize> {
        self.0.rows.borrow().iter().enumerate().filter(|(_, r)| r.kind == RowKind::Widget).map(|(i, _)| i).collect()
    }

    /// Colours buffer lines `lines` by their kind.
    fn apply_line_tags(&self, lines: Range<usize>) {
        let tag_of = |row: &Row| match row.kind {
            RowKind::Hunk => Some("hunk"),
            RowKind::Info => Some("info"),
            RowKind::Line(LineKind::Added) => Some("added"),
            RowKind::Line(LineKind::Removed) => Some("removed"),
            RowKind::Line(LineKind::Context) | RowKind::Widget | RowKind::Spacer => None,
        };
        let mut runs = Vec::new();
        {
            let rows = self.0.rows.borrow();
            let mut start = lines.start;
            while start < lines.end {
                let tag = tag_of(&rows[start]);
                let end = rows[start..lines.end].iter().position(|r| tag_of(r) != tag).map_or(lines.end, |n| start + n);
                if let Some(tag) = tag {
                    runs.push((tag, start, end));
                }
                start = end;
            }
        }
        for (tag, start, end) in runs {
            self.tag_lines(tag, start, end);
        }
    }

    /// Highlights the lines being commented on and opens a gap for the composer.
    fn apply_target(&self, target: Option<&Target>) {
        let (lines, spacer) = {
            let rows = self.0.rows.borrow();
            let range = target.and_then(|t| t.range);
            let lines: Vec<usize> = rows
                .iter()
                .enumerate()
                .filter(|(_, r)| r.index.is_some_and(|i| range.is_some_and(|(first, last)| i >= first && i <= last)))
                .map(|(line, _)| line)
                .collect();
            (lines, rows.iter().position(|r| r.kind == RowKind::Spacer))
        };
        if let (Some(&from), Some(&to)) = (lines.first(), lines.last()) {
            self.tag_lines("target", from, to + 1);
        }
        if let Some(line) = spacer {
            self.tag_lines("gap", line, line + 1);
        }
        self.0.gap.set(spacer);
    }

    /// Applies `tag` to buffer lines `start..end`.
    fn tag_lines(&self, tag: &str, start: usize, end: usize) {
        let buffer = &self.0.buffer;
        let from = iter_at_line(buffer, start);
        let to = if end < self.0.rows.borrow().len() { iter_at_line(buffer, end) } else { buffer.end_iter() };
        buffer.apply_tag_by_name(tag, &from, &to);
    }

    fn select_lines(&self, first: i32, last: i32) {
        let buffer = &self.0.buffer;
        let start = buffer.iter_at_line(first).unwrap_or_else(|| buffer.end_iter());
        let mut end = buffer.iter_at_line(last).unwrap_or_else(|| buffer.end_iter());
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        buffer.select_range(&start, &end);
    }

    /// The code lines covered by the selection, with their index range.
    fn selection(&self) -> Option<(LineAnchor, (usize, usize))> {
        let (start, end) = self.0.buffer.selection_bounds()?;
        let mut last = end.line();
        if end.starts_line() && last > start.line() {
            last -= 1;
        }
        self.covering(start.line(), last)
    }

    /// The code lines among buffer lines `first..=last`, with their index range.
    fn covering(&self, first: i32, last: i32) -> Option<(LineAnchor, (usize, usize))> {
        let rows = self.0.rows.borrow();
        let indices: Vec<usize> = (first..=last)
            .filter_map(|l| usize::try_from(l).ok().and_then(|l| rows.get(l)).and_then(|r| r.index))
            .collect();
        let (&first, &last) = (indices.iter().min()?, indices.iter().max()?);
        let diff = self.0.diff.borrow();
        let anchor = LineAnchor::covering(diff.as_ref()?, first, last)?;
        Some((anchor, (first, last)))
    }

    /// Opens the composer for the selected lines (when `lines` is set and
    /// there is a selection), or for the whole file.
    pub fn open_composer(&self, lines: bool) {
        self.open_composer_on(self.selection().filter(|_| lines));
    }

    /// Opens the composer for the given code lines, or for the whole file.
    fn open_composer_on(&self, lines: Option<(LineAnchor, (usize, usize))>) {
        if self.0.diff.borrow().is_none() {
            return;
        }
        let target = match lines {
            Some((anchor, range)) => Target { anchor: Some(anchor), range: Some(range) },
            None => Target { anchor: None, range: None },
        };
        let title = target
            .anchor
            .as_ref()
            .map_or_else(|| "Comment on this file".to_owned(), |a| format!("Comment on {}", a.label().to_lowercase()));
        self.0.composer.title.set_text(&title);
        self.0.composer.root.set_visible(true);
        self.set_target(Some(target));
        // The target highlight replaces the selection, and the composer is
        // brought into view if it opened off screen, e.g. for a file comment.
        let buffer = &self.0.buffer;
        if let Some(line) = self.0.gap.get() {
            buffer.place_cursor(&iter_at_line(buffer, line));
            self.0.view.scroll_mark_onscreen(&buffer.get_insert());
        }
        // Once the click that opened it is over, or the view would take focus back.
        let text = self.0.composer.text.clone();
        glib::idle_add_local_once(move || {
            text.grab_focus();
        });
    }

    /// Closes the composer, discarding what was typed.
    fn close_composer(&self) {
        self.0.composer.text.buffer().set_text("");
        self.0.selection_origin.set(None);
        self.0.composer.root.set_visible(false);
        self.set_target(None);
        self.0.view.grab_focus();
    }

    /// Moves the target highlight and the composer's gap in place: re-rendering
    /// would rebuild the buffer for what is a one-line change.
    fn set_target(&self, target: Option<Target>) {
        let buffer = &self.0.buffer;
        let (start, end) = buffer.bounds();
        buffer.remove_tag_by_name("target", &start, &end);
        if let Some(line) = self.0.gap.take() {
            buffer.delete(&mut iter_at_line(buffer, line), &mut iter_at_line(buffer, line + 1));
            self.0.rows.borrow_mut().remove(line);
        }
        if let Some(target) = &target {
            let line = self.spacer_line(target);
            buffer.insert(&mut iter_at_line(buffer, line), "\n");
            self.0.rows.borrow_mut().insert(line, Row::plain(RowKind::Spacer));
            // Don't let it take the colour of the line it was inserted into.
            buffer.remove_all_tags(&iter_at_line(buffer, line), &iter_at_line(buffer, line + 1));
        }
        self.apply_target(target.as_ref());
        self.0.target.replace(target);
        self.relayout();
    }

    /// The line `render` puts the composer's spacer on: below the target lines and their comments.
    fn spacer_line(&self, target: &Target) -> usize {
        let after = target.range.map_or(Some(0), |(_, last)| self.buffer_line_of(last).map(|l| l + 1));
        let rows = self.0.rows.borrow();
        let mut line = after.unwrap_or(0);
        while rows.get(line).is_some_and(|r| r.kind == RowKind::Widget) {
            line += 1;
        }
        line
    }

    fn submit(&self) {
        let buffer = self.0.composer.text.buffer();
        let body = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
        let Some(target) = self.0.target.borrow().clone() else { return };
        if body.trim().is_empty() {
            return;
        }
        self.close_composer();
        if let Some(callback) = self.0.on_add.borrow().as_ref() {
            callback(target.anchor, body);
        }
    }

    fn build_thread(&self, notes: &[&Note], diff: &FileDiff) -> gtk::Box {
        let thread = gtk::Box::new(gtk::Orientation::Vertical, 6);
        thread.add_css_class("comment-thread");
        // Don't let the cards' expanding contents make the text view expand.
        thread.set_hexpand(false);
        thread.set_halign(gtk::Align::Start);
        if self.0.child_width.get() > 0 {
            thread.set_width_request(self.0.child_width.get());
        }
        let now = crate::core::now();
        for note in notes {
            thread.append(&self.build_card(note, diff, now));
        }
        thread
    }

    fn build_card(&self, note: &Note, diff: &FileDiff, now: i64) -> gtk::Box {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
        card.add_css_class("comment-card");
        if note.sent_at.is_some() {
            card.add_css_class("sent");
        }
        let outdated = note.anchor.as_ref().filter(|a| !a.is_current(Some(diff)));

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let target = match (&note.anchor, outdated) {
            (Some(anchor), None) => anchor.label(),
            (Some(anchor), Some(_)) => format!("{} · outdated", anchor.label()),
            (None, _) => "File".to_owned(),
        };
        header.append(&util::label(&target, &["comment-target"]));
        header.append(&util::label(&util::relative_time(note.created_at, now), &["comment-meta"]));
        if note.sent_at.is_some() {
            let sent = util::label("Sent", &["chip"]);
            sent.set_tooltip_text(Some("Included in feedback you copied or sent"));
            sent.set_valign(gtk::Align::Center);
            header.append(&sent);
        }
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        header.append(&spacer);
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
            // Deleting re-renders the view, which destroys this button: do it once the click is over.
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

        if let Some(anchor) = outdated {
            let quote = util::label(&anchor.text, &["comment-quote"]);
            quote.set_wrap(true);
            card.append(&quote);
        }

        let body = util::label(&note.body, &["comment-body"]);
        body.set_wrap(true);
        body.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        body.set_selectable(true);
        card.append(&body);
        card
    }
}

fn build_composer() -> Composer {
    let title = util::label("Comment", &["comment-target"]);
    let text = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .accepts_tab(false)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(10)
        .right_margin(10)
        .build();
    text.add_css_class("comment-input");
    let placeholder = util::label("Leave a comment…", &["dim-label"]);
    placeholder.set_valign(gtk::Align::Start);
    placeholder.set_margin_start(11);
    placeholder.set_margin_top(8);
    placeholder.set_can_target(false);
    text.buffer().connect_changed({
        let placeholder = placeholder.clone();
        move |buffer| placeholder.set_visible(buffer.char_count() == 0)
    });
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(72)
        .max_content_height(240)
        .propagate_natural_height(true)
        .child(&text)
        .build();
    let overlay = gtk::Overlay::builder().child(&scroller).build();
    overlay.add_overlay(&placeholder);

    let hint = util::label("Ctrl+Enter to comment · Esc to cancel", &["comment-meta"]);
    hint.set_hexpand(true);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("flat");
    cancel.add_css_class("composer-button");
    let submit = gtk::Button::with_label("Comment");
    submit.add_css_class("suggested-action");
    submit.add_css_class("composer-button");
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    actions.append(&hint);
    actions.append(&cancel);
    actions.append(&submit);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.add_css_class("comment-composer");
    root.append(&title);
    root.append(&overlay);
    root.append(&actions);
    Composer { root, title, text, cancel, submit }
}

fn build_search(buffer: &sourceview5::Buffer) -> Search {
    let settings = sourceview5::SearchSettings::new();
    settings.set_wrap_around(true);
    let context = sourceview5::SearchContext::new(buffer, Some(&settings));
    context.set_highlight(false);

    let entry = gtk::SearchEntry::builder().placeholder_text("Find in file").width_chars(28).build();
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
    Search { bar, entry, count, previous, next, settings, context }
}

fn describe_without_hunks(diff: &FileDiff) -> String {
    if diff.is_binary {
        return "Binary file: no diff to show.".into();
    }
    if diff.is_oversized {
        return "New file larger than 4 MB: not shown.".into();
    }
    if let Some(from) = diff.renamed_from() {
        return format!("Renamed from {from}, content unchanged.");
    }
    if let (Some(old), Some(new)) = (&diff.old_mode, &diff.new_mode) {
        return format!("File mode changed from {old} to {new}.");
    }
    if diff.is_new {
        return "New empty file.".into();
    }
    if diff.is_deleted {
        return "Empty file deleted.".into();
    }
    "No content changes.".into()
}

/// What makes rows equal when aligning two renders: their kind and text.
/// Threads and spacers compare by kind only, as their contents are rebuilt.
fn line_keys<'a>(rows: &[Row], lines: &[&'a str]) -> Vec<(RowKind, &'a str)> {
    rows.iter()
        .zip(lines)
        .map(|(row, line)| match row.kind {
            RowKind::Widget | RowKind::Spacer => (row.kind, ""),
            kind => (kind, *line),
        })
        .collect()
}

fn iter_at_line(buffer: &sourceview5::Buffer, line: usize) -> gtk::TextIter {
    i32::try_from(line).ok().and_then(|l| buffer.iter_at_line(l)).unwrap_or_else(|| buffer.end_iter())
}
