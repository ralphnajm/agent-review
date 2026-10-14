//! Read-only, syntax highlighted rendering of one file's diff, with comment
//! threads shown inline below the lines they refer to. Comments already sent
//! to the agent are kept out of the diff, in a history panel above it.
//!
//! The view takes the diff's full height and scrolls with the other files'
//! diffs in the review panel's `Scroller`, but is only allocated the part on
//! screen (see `Slice`), so long diffs stay fast.
//!
//! The buffer holds the diff lines without their `+`/`-` prefixes, coloured
//! with the highlighting of the side of the file each comes from (see
//! `highlight`); old/new line numbers are drawn in a custom gutter. Comment cards are read-only child
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
use crate::core::diff::{DiffLine, FileDiff, Gap, LineKind};
use crate::core::review::{Author, LineAnchor, Note, Side};
use crate::core::split;
use crate::ui::highlight::Highlighter;
use crate::ui::scroller::{Scroller, Slice};
use crate::ui::util;

/// Comment cards are not stretched wider than this.
const MAX_THREAD_WIDTH: i32 = 900;
/// Lines highlighted above and below those on screen, at most.
const MAX_COLORED_LINES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowKind {
    /// Unchanged lines the diff leaves out; clicking it shows them.
    Hidden,
    Line(LineKind),
    /// A side-by-side row: the kinds of its old and new sides, `None` where blank.
    Pair(Option<LineKind>, Option<LineKind>),
    /// An empty line holding a comment thread.
    Widget,
    /// A message for diffs without content, e.g. binary files.
    Info,
    /// An empty line the floating composer sits on.
    Spacer,
}

impl RowKind {
    /// Whether the row shows code, which can be commented on.
    const fn is_code(self) -> bool {
        matches!(self, Self::Line(_) | Self::Pair(..))
    }
}

#[derive(Debug, Clone, Copy)]
struct Row {
    kind: RowKind,
    old_no: Option<u32>,
    new_no: Option<u32>,
    /// Index into `FileDiff::lines()` for code lines; in side-by-side rows, the new side's.
    index: Option<usize>,
    /// In side-by-side rows, the index of the old side's line.
    old_index: Option<usize>,
    /// The lines a `Hidden` row stands for.
    gap: Option<Gap>,
    /// The text the row shows from each side of the file, to colour it with
    /// that side's highlighting.
    pieces: [Option<Piece>; 2],
}

impl Row {
    const fn plain(kind: RowKind) -> Self {
        Self { kind, old_no: None, new_no: None, index: None, old_index: None, gap: None, pieces: [None, None] }
    }

    /// The `FileDiff::lines()` the row shows, once each.
    fn indices(&self) -> impl Iterator<Item = usize> {
        self.old_index.filter(|&old| Some(old) != self.index).into_iter().chain(self.index)
    }

    fn shows(&self, index: usize) -> bool {
        self.indices().any(|i| i == index)
    }
}

/// Part of a line of one side of the file, shown in a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Piece {
    side: Side,
    /// The line's number on that side.
    number: u32,
    /// The characters of the line shown, counted with its tabs expanded side by side.
    chars: (usize, usize),
    /// Where in the row they start.
    at: usize,
}

impl Piece {
    /// A whole line, starting the row.
    fn whole(side: Side, number: Option<u32>) -> [Option<Self>; 2] {
        [number.map(|number| Self { side, number, chars: (0, usize::MAX), at: 0 }), None]
    }
}

/// Which match `find` selects.
#[derive(Debug, Clone, Copy)]
pub enum Find {
    /// The first one at or after the selection, or the top of the screen:
    /// the search text changed.
    FromSelection,
    Next,
    Previous,
    First,
    Last,
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
type FocusCallback = Box<dyn Fn()>;
/// Reads the new side of the file, for the unchanged lines; replies with `None` if it can't.
type LoadCallback = Box<dyn Fn(Box<dyn FnOnce(Option<String>)>)>;

struct Composer {
    root: gtk::Box,
    /// Who the comment is by.
    author: gtk::Label,
    title: gtk::Label,
    text: gtk::TextView,
    cancel: gtk::Button,
    submit: gtk::Button,
}

/// Comments from earlier rounds: those already sent to the agent.
struct History {
    button: gtk::ToggleButton,
    count: gtk::Label,
    revealer: gtk::Revealer,
    /// The heading and the cards, lined up with the code.
    content: gtk::Box,
    list: gtk::Box,
}

#[derive(Clone)]
pub struct DiffView(Rc<Inner>);

struct Inner {
    root: gtk::Box,
    search: sourceview5::SearchContext,
    history: History,
    overlay: gtk::Overlay,
    scroller: Scroller,
    view: sourceview5::View,
    buffer: sourceview5::Buffer,
    gutter: sourceview5::GutterRendererText,
    /// The button floating at the start of the hovered line, to comment on it.
    button: gtk::Image,
    /// The box around the lines being picked or commented on.
    frame: gtk::Box,
    /// The buffer line the button is on.
    hovered: Cell<Option<i32>>,
    /// The buffer lines the composer comments on, first and last.
    target_lines: Cell<Option<(usize, usize)>>,
    composer: Composer,
    rows: RefCell<Vec<Row>>,
    /// Width of each line-number column, in characters.
    number_width: Cell<usize>,
    diff: RefCell<Option<FileDiff>>,
    notes: RefCell<Vec<Note>>,
    target: RefCell<Option<Target>>,
    /// Comment threads in the buffer, resized with the view, with the side
    /// they go on side by side.
    children: RefCell<Vec<(gtk::Widget, Option<Side>)>>,
    /// The arrows on rows of hidden lines, also in the buffer.
    expanders: RefCell<Vec<gtk::Widget>>,
    child_width: Cell<i32>,
    /// The spacer line the composer floats over.
    gap: Cell<Option<usize>>,
    /// Height reserved for the composer, in pixels.
    gap_height: Cell<i32>,
    /// Line where picking lines from the gutter started, for shift-click.
    selection_origin: Cell<Option<i32>>,
    /// Pointer height in the scroller while picking lines from the gutter.
    drag_y: Cell<Option<f64>>,
    /// Scrolls the view while that pointer is past its top or bottom edge.
    autoscroll: RefCell<Option<gtk::TickCallbackId>>,
    on_add: RefCell<Option<AddCallback>>,
    on_delete: RefCell<Option<DeleteCallback>>,
    on_focus: RefCell<Option<FocusCallback>>,
    on_load: RefCell<Option<LoadCallback>>,
    /// The new side of the file, split in lines, for the unchanged lines the diff leaves out.
    source: RefCell<Option<Vec<String>>>,
    /// The fingerprint of the diff the source was last asked for.
    source_for: RefCell<Option<String>>,
    /// Ranges of new-side line numbers whose unchanged lines are shown.
    revealed: RefCell<Vec<(u32, u32)>>,
    /// Whether the old and new sides are shown beside each other.
    split: Cell<bool>,
    /// Side by side, the width of each side in characters, once known.
    columns: Cell<usize>,
    /// Side by side, where the divider and the end of the line are, in pixels.
    tab_stops: Cell<(i32, i32)>,
    /// Side by side, the width of a character, in pixels.
    cell: Cell<i32>,
    /// Each side of the file, old then new, highlighted on its own.
    highlight: [Highlighter; 2],
    /// The buffer line that was at the top of the screen, followed through
    /// changes, and how far down it the top was, in pixels.
    pin: Cell<Option<(usize, f64)>>,
    /// Which buffer lines have their highlighting, done for those on screen.
    colored: RefCell<Vec<bool>>,
    /// Set while the lines on screen are waiting to be coloured.
    coloring: Cell<bool>,
}

impl DiffView {
    /// A diff scrolling in `scroller`, searched with `search`, which all files share.
    pub fn new(scroller: &Scroller, search: &sourceview5::SearchSettings) -> Self {
        let buffer = sourceview5::Buffer::new(None);
        // Coloured from `highlight` instead.
        buffer.set_highlight_syntax(false);
        buffer.set_highlight_matching_brackets(false);
        buffer.set_enable_undo(false);
        let view = sourceview5::View::with_buffer(&buffer);
        view.set_editable(false);
        view.set_cursor_visible(false);
        // Long lines wrap rather than scroll sideways.
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        // Room for the comment button between the line numbers and the code.
        view.set_left_margin(20);
        view.set_top_margin(8);
        view.set_bottom_margin(12);
        view.add_css_class("diff-view");

        // Lays out and draws only the lines on screen.
        let slice = Slice::new(&view, scroller);
        let overlay = gtk::Overlay::builder().child(&slice).overflow(gtk::Overflow::Hidden).build();
        let composer = build_composer();
        composer.root.set_visible(false);
        overlay.add_overlay(&composer.root);
        let search = sourceview5::SearchContext::new(&buffer, Some(search));
        search.set_highlight(false);
        let history = build_history();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&history.revealer);
        root.append(&overlay);

        let gutter = sourceview5::GutterRendererText::new();
        gutter.set_xpad(8);
        gutter.set_xalign(0.0);
        // On the first row of a wrapped line.
        gutter.set_alignment_mode(sourceview5::GutterRendererAlignmentMode::First);
        gutter.set_yalign(0.5);
        let left = ViewExt::gutter(&view, gtk::TextWindowType::Left);
        left.insert(&gutter, 0);
        left.set_cursor_from_name(Some("pointer"));
        // Drawn over the text, so they take no room from it.
        let frame = gtk::Box::builder().css_classes(["line-frame"]).can_target(false).visible(false).build();
        overlay.add_overlay(&frame);
        let button = gtk::Image::builder()
            .icon_name("list-add-symbolic")
            .css_classes(["line-comment-button"])
            .cursor(&gtk::gdk::Cursor::from_name("pointer", None).expect("the pointer cursor"))
            .visible(false)
            .build();
        overlay.add_overlay(&button);

        let highlight = [Highlighter::new(&buffer.tag_table()), Highlighter::new(&buffer.tag_table())];
        let this = Self(Rc::new(Inner {
            root,
            search,
            history,
            overlay,
            scroller: scroller.clone(),
            view,
            buffer,
            gutter,
            button,
            frame,
            hovered: Cell::new(None),
            target_lines: Cell::new(None),
            composer,
            rows: RefCell::default(),
            number_width: Cell::new(3),
            diff: RefCell::default(),
            notes: RefCell::default(),
            target: RefCell::default(),
            children: RefCell::default(),
            expanders: RefCell::default(),
            child_width: Cell::new(0),
            gap: Cell::new(None),
            gap_height: Cell::new(0),
            selection_origin: Cell::new(None),
            drag_y: Cell::new(None),
            autoscroll: RefCell::default(),
            on_add: RefCell::default(),
            on_delete: RefCell::default(),
            on_focus: RefCell::default(),
            on_load: RefCell::default(),
            source: RefCell::default(),
            source_for: RefCell::default(),
            revealed: RefCell::default(),
            split: Cell::new(false),
            columns: Cell::new(0),
            tab_stops: Cell::new((0, 0)),
            cell: Cell::new(0),
            highlight,
            pin: Cell::new(None),
            colored: RefCell::default(),
            coloring: Cell::new(false),
        }));
        this.create_tags();
        this.update_style_scheme();
        this.connect_signals();
        this.connect_composer();
        let history = &this.0.history;
        let (revealer, list) = (history.revealer.clone(), history.list.clone());
        history.button.connect_toggled(move |button| {
            revealer.set_reveal_child(button.is_active() && list.first_child().is_some());
        });
        this
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.0.root
    }

    /// Shows and hides the history of sent comments; for the file header.
    pub fn history_button(&self) -> &gtk::ToggleButton {
        &self.0.history.button
    }

    /// Called to read the new side of the file, for the unchanged lines between changes.
    pub fn connect_load(&self, callback: impl Fn(Box<dyn FnOnce(Option<String>)>) + 'static) {
        self.0.on_load.replace(Some(Box::new(callback)));
    }

    /// Called when a comment is written: on the anchored lines, or the file for `None`.
    pub fn connect_add(&self, callback: impl Fn(Option<LineAnchor>, String) + 'static) {
        self.0.on_add.replace(Some(Box::new(callback)));
    }

    pub fn connect_delete(&self, callback: impl Fn(u64) + 'static) {
        self.0.on_delete.replace(Some(Box::new(callback)));
    }

    /// Called when the diff or its composer takes the keyboard focus.
    pub fn connect_focus(&self, callback: impl Fn() + 'static) {
        self.0.on_focus.replace(Some(Box::new(callback)));
    }

    /// Signs new comments with `author`, or as unknown, shown above what is being written.
    pub fn set_author(&self, author: Option<&Author>) {
        show_author(&self.0.composer.author, author);
    }

    pub fn search(&self) -> &sourceview5::SearchContext {
        &self.0.search
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
        let hidden = tag("hidden", (0.5, 0.5, 0.55, 0.08));
        hidden.set_pixels_above_lines(4);
        hidden.set_pixels_below_lines(4);
        hidden.set_foreground_rgba(Some(&grey));
        let info = tag("info", (0.0, 0.0, 0.0, 0.0));
        info.set_style(gtk::pango::Style::Italic);
        info.set_pixels_above_lines(12);
        info.set_foreground_rgba(Some(&grey));
        // The match the find bar is on: the selection is faint while the bar has focus.
        table.add(
            &gtk::TextTag::builder().name("found").background_rgba(&gtk::gdk::RGBA::new(0.96, 0.6, 0.0, 0.6)).build(),
        );
        // Room for the floating composer; its height is set in `relayout`.
        table.add(&gtk::TextTag::builder().name("gap").build());
        // Side by side, each half is coloured on its own, up to the tab that ends it.
        let side = |name: &str, rgba: (f32, f32, f32, f32)| {
            let tag = gtk::TextTag::builder()
                .name(name)
                .background_rgba(&gtk::gdk::RGBA::new(rgba.0, rgba.1, rgba.2, rgba.3))
                .background_full_height(true)
                .build();
            table.add(&tag);
        };
        side("added-side", (0.18, 0.76, 0.49, 0.13));
        side("removed-side", (0.88, 0.11, 0.14, 0.11));
        side("blank-side", (0.5, 0.5, 0.55, 0.06));
        table.add(&gtk::TextTag::builder().name("divider").foreground_rgba(&grey.with_alpha(0.35)).build());
        // The tab stops lining up the halves; set in `update_columns`.
        table.add(&gtk::TextTag::builder().name("columns").build());
    }

    fn update_style_scheme(&self) {
        let dark = adw::StyleManager::default().is_dark();
        let manager = sourceview5::StyleSchemeManager::default();
        let scheme = manager
            .scheme(if dark { "Adwaita-dark" } else { "Adwaita" })
            .or_else(|| manager.scheme(if dark { "classic-dark" } else { "classic" }));
        self.0.buffer.set_style_scheme(scheme.as_ref());
        for side in &self.0.highlight {
            side.set_style_scheme(scheme.as_ref());
        }
    }

    fn connect_signals(&self) {
        // The view is shown, as its file is expanded, or the slice of it on screen moves.
        let weak = self.weak();
        self.0.view.connect_map(move |_| {
            if let Some(inner) = weak.upgrade() {
                DiffView(inner).queue_coloring();
            }
        });
        if let Some(adjustment) = self.0.view.vadjustment() {
            let weak = self.weak();
            adjustment.connect_value_changed(move |_| {
                if let Some(inner) = weak.upgrade() {
                    DiffView(inner).queue_coloring();
                }
            });
            let weak = self.weak();
            adjustment.connect_changed(move |_| {
                if let Some(inner) = weak.upgrade() {
                    DiffView(inner).queue_coloring();
                }
            });
        }

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

        // Hovering a code line shows a comment button in the gutter.
        let motion = gtk::EventControllerMotion::new();
        let weak = self.weak();
        motion.connect_motion(move |_, x, y| {
            if let Some(inner) = weak.upgrade() {
                let this = DiffView(inner);
                #[allow(clippy::cast_possible_truncation)]
                let point = gtk::graphene::Point::new(x as f32, y as f32);
                let y = this.0.overlay.compute_point(&this.0.view, &point).map(|p| f64::from(p.y()));
                this.hover(y);
            }
        });
        let weak = self.weak();
        motion.connect_leave(move |_| {
            if let Some(inner) = weak.upgrade() {
                DiffView(inner).hover(None);
            }
        });
        // On the overlay, so that moving onto the button keeps it.
        self.0.overlay.add_controller(motion);

        self.connect_line_drag();

        // Clicking a row of hidden lines shows them. Captured, so that the
        // click selects nothing and opens no comment.
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.weak();
        click.connect_pressed(move |gesture, presses, _, y| {
            let Some(inner) = weak.upgrade() else { return };
            let this = DiffView(inner);
            let line = usize::try_from(this.line_at_view_y(y)).ok();
            let gap = line.and_then(|l| this.0.rows.borrow().get(l).and_then(|r| r.gap));
            match gap {
                Some(gap) if presses == 1 => {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    // Once the press is handled: this rewrites the buffer under the pointer.
                    let weak = this.weak();
                    glib::idle_add_local_once(move || {
                        if let Some(inner) = weak.upgrade() {
                            DiffView(inner).reveal(gap);
                        }
                    });
                }
                // The second press of a double click: already handled.
                Some(_) => {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
                None => {
                    gesture.set_state(gtk::EventSequenceState::Denied);
                }
            }
        });
        self.0.view.add_controller(click);

        let focus = gtk::EventControllerFocus::new();
        let weak = self.weak();
        focus.connect_enter(move |_| {
            if let Some(inner) = weak.upgrade()
                && let Some(callback) = inner.on_focus.borrow().as_ref()
            {
                callback();
            }
        });
        self.0.root.add_controller(focus);

        // Positioned while the overlay is allocated, after the text view it
        // floats over, so both are drawn from the same layout.
        let weak = self.weak();
        self.0.overlay.connect_get_child_position(move |_, widget| {
            let inner = weak.upgrade()?;
            let this = DiffView(inner);
            if widget == this.0.composer.root.upcast_ref::<gtk::Widget>() {
                this.composer_rect()
            } else if widget == this.0.frame.upcast_ref::<gtk::Widget>() {
                this.frame_rect()
            } else if widget == this.0.button.upcast_ref::<gtk::Widget>() {
                this.button_rect()
            } else {
                None
            }
        });
    }

    /// Pressing the button or a code line's numbers picks the line, and dragging picks
    /// more or fewer; releasing comments on them. Shift-click extends the last pick.
    /// Selecting text is left to the text view, for copying. Captured on the overlay,
    /// which holds both the view and the button.
    fn connect_line_drag(&self) {
        let drag = gtk::GestureDrag::new();
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.weak();
        drag.connect_drag_begin(move |gesture, x, y| {
            let Some(inner) = weak.upgrade() else { return };
            let this = DiffView(inner);
            if !this.picks_at(x, y) {
                gesture.set_state(gtk::EventSequenceState::Denied);
                return;
            }
            // Keep the text view from starting a selection of its own.
            gesture.set_state(gtk::EventSequenceState::Claimed);
            let extend = gesture.current_event_state().contains(gtk::gdk::ModifierType::SHIFT_MASK);
            if let Some(y) = this.to_scroller_y(x, y) {
                this.begin_line_drag(y, extend);
            }
        });
        let weak = self.weak();
        drag.connect_drag_update(move |gesture, dx, dy| {
            if let Some(inner) = weak.upgrade()
                && let Some((x, y)) = gesture.start_point()
            {
                let this = DiffView(inner);
                if this.0.drag_y.get().is_some()
                    && let Some(y) = this.to_scroller_y(x + dx, y + dy)
                {
                    this.0.drag_y.set(Some(y));
                    this.extend_line_drag();
                }
            }
        });
        let weak = self.weak();
        drag.connect_drag_end(move |_, _, _| {
            if let Some(inner) = weak.upgrade() {
                let this = DiffView(inner);
                if this.0.drag_y.get().is_some() {
                    this.end_line_drag();
                }
            }
        });
        self.0.overlay.add_controller(drag);
    }

    /// Whether pressing at `x`, `y` of the overlay picks lines: on the button, or
    /// on the numbers of a code line.
    fn picks_at(&self, x: f64, y: f64) -> bool {
        #[allow(clippy::cast_possible_truncation)]
        let point = gtk::graphene::Point::new(x as f32, y as f32);
        let within = |widget: &gtk::Widget| {
            widget.is_visible() && widget.compute_bounds(&self.0.overlay).is_some_and(|b| b.contains_point(&point))
        };
        if within(self.0.button.upcast_ref()) {
            return true;
        }
        let gutter = ViewExt::gutter(&self.0.view, gtk::TextWindowType::Left);
        let Some(y) = self.0.overlay.compute_point(&self.0.view, &point).map(|p| f64::from(p.y())) else {
            return false;
        };
        let line = usize::try_from(self.line_at_view_y(y)).ok();
        let code = line.and_then(|l| self.0.rows.borrow().get(l).map(|r| r.kind.is_code()));
        within(gutter.upcast_ref()) && code == Some(true)
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

    /// Selects a match of the shared search and brings it on screen. Searches
    /// this file only, without wrapping around; returns whether there was a match.
    pub fn find(&self, find: Find) -> bool {
        let search = &self.0.search;
        let buffer = &self.0.buffer;
        if search.settings().search_text().is_none() {
            return false;
        }
        // The shared settings don't wrap around: the panel moves on to the next file instead.
        let selection = buffer.selection_bounds();
        let found = match find {
            Find::First => search.forward(&buffer.start_iter()),
            Find::Last => search.backward(&buffer.end_iter()),
            Find::FromSelection => {
                // From the selection, or else the top of the screen if this file is on it.
                let from = selection.map(|(start, _)| start).or_else(|| self.visible_lines().map(|(top, _)| top));
                search.forward(&from.unwrap_or_else(|| buffer.start_iter()))
            }
            Find::Next => search.forward(&selection.map_or_else(|| buffer.start_iter(), |(_, end)| end)),
            Find::Previous => search.backward(&selection.map_or_else(|| buffer.end_iter(), |(start, _)| start)),
        };
        let Some((start, end, _)) = found else { return false };
        self.clear_found();
        // Above the match highlighting, whose tag is created on first use.
        let table = buffer.tag_table();
        if let Some(tag) = table.lookup("found") {
            tag.set_priority(table.size() - 1);
        }
        buffer.apply_tag_by_name("found", &start, &end);
        buffer.select_range(&start, &end);
        if let Some((top, bottom)) = self.line_span(start.line(), end.line()) {
            self.0.scroller.reveal(&self.0.view, top - 48.0, bottom + 48.0);
        }
        true
    }

    /// The selected text, if it is on one line.
    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.0.buffer.selection_bounds()?;
        (start.line() == end.line()).then(|| self.0.buffer.text(&start, &end, false).to_string())
    }

    /// Forgets the selected match.
    pub fn clear_found(&self) {
        let buffer = &self.0.buffer;
        let (start, end) = buffer.bounds();
        buffer.remove_tag_by_name("found", &start, &end);
        if let Some((start, _)) = buffer.selection_bounds() {
            buffer.place_cursor(&start);
        }
    }

    /// The number of matches, negative while counting, and which one is selected (from 1, or 0).
    pub fn search_position(&self) -> (i32, i32) {
        let search = &self.0.search;
        let position =
            self.0.buffer.selection_bounds().map_or(0, |(start, end)| search.occurrence_position(&start, &end));
        (search.occurrences_count(), position.max(0))
    }

    /// The first and last buffer lines on screen, if any.
    fn visible_lines(&self) -> Option<(gtk::TextIter, gtk::TextIter)> {
        let (top, bottom) = self.0.scroller.visible_span(&self.0.view)?;
        if bottom <= 0.0 || top >= f64::from(self.0.view.height()) {
            return None;
        }
        let at = |y: f64| self.0.buffer.iter_at_line(self.line_at_view_y(y));
        Some((at(top)?, at(bottom)?))
    }

    /// The heights of buffer lines `first..=last` in the view.
    fn line_span(&self, first: i32, last: i32) -> Option<(f64, f64)> {
        let view = &self.0.view;
        let (top, _) = view.line_yrange(&self.0.buffer.iter_at_line(first)?);
        let (y, height) = view.line_yrange(&self.0.buffer.iter_at_line(last)?);
        let (_, top) = view.buffer_to_window_coords(gtk::TextWindowType::Widget, 0, top);
        let (_, bottom) = view.buffer_to_window_coords(gtk::TextWindowType::Widget, 0, y + height);
        Some((f64::from(top), f64::from(bottom)))
    }

    /// Remembers the line at the top of the screen, if it is in this diff, to
    /// bring it back there with `pinned_delta` once the diff changes; returns
    /// whether it is.
    pub fn pin_top(&self) -> bool {
        let view = &self.0.view;
        let top = self.0.scroller.visible_span(view).map(|(top, _)| top);
        let pin = top.filter(|&top| view.is_mapped() && top >= 0.0 && top < f64::from(view.height())).and_then(|top| {
            let line = self.line_at_view_y(top);
            let (line_top, _) = self.line_span(line, line)?;
            Some((usize::try_from(line).ok()?, top - line_top))
        });
        self.0.pin.set(pin);
        pin.is_some()
    }

    /// How far to scroll to bring the pinned line back to where it was at the top of the screen.
    pub fn pinned_delta(&self) -> Option<f64> {
        let (line, offset) = self.0.pin.get()?;
        let (top, _) = self.0.scroller.visible_span(&self.0.view).filter(|_| self.0.view.is_mapped())?;
        let line = i32::try_from(line).ok()?;
        let (line_top, _) = self.line_span(line, line)?;
        Some(line_top + offset - top)
    }

    pub fn unpin(&self) {
        self.0.pin.set(None);
    }

    /// Follows the pinned line as buffer lines `at..at + removed` are replaced with `inserted` others.
    fn move_pin(&self, at: usize, removed: usize, inserted: usize) {
        if let Some((line, offset)) = self.0.pin.get()
            && line >= at
        {
            let pin = if line >= at + removed {
                (line + inserted - removed, offset)
            } else {
                // The line was rewritten: keep to its place among the new ones.
                (at + (line - at).min(inserted.saturating_sub(1)), if line - at < inserted { offset } else { 0.0 })
            };
            self.0.pin.set(Some(pin));
        }
    }

    /// Shows the old and new sides beside each other, or the changes one above the other.
    pub fn set_split(&self, split: bool) {
        if self.0.split.replace(split) != split {
            self.render(Change::Content);
        }
    }

    /// Lines up the halves of side-by-side rows with the view's width; returns
    /// whether the rows must be wrapped again, at another width.
    fn update_columns(&self) -> bool {
        let view = &self.0.view;
        let width = self.0.scroller.window().width()
            - self.0.gutter.width_request()
            - view.left_margin()
            - view.right_margin()
            // Short of the edge, where the tab ending the line would wrap.
            - 6;
        if width <= 0 {
            return false;
        }
        let (sample, _) = view.create_pango_layout(Some("0000000000")).size();
        let cell = f64::from(sample) / f64::from(10 * gtk::pango::SCALE);
        if cell <= 0.0 {
            return false;
        }
        #[allow(clippy::cast_possible_truncation)]
        self.0.cell.set(cell.ceil() as i32);
        let half = width / 2;
        // Each half leaves a character before the divider, and the new one a space after it.
        let fit = |pixels: i32| (f64::from(pixels) / cell).floor().max(0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let columns = fit(half).min(fit(width - half) - 2.0).max(8.0) as usize - 1;
        if self.0.tab_stops.replace((half, width)) != (half, width) {
            let mut tabs = gtk::pango::TabArray::new(2, true);
            tabs.set_tab(0, gtk::pango::TabAlign::Left, half);
            tabs.set_tab(1, gtk::pango::TabAlign::Left, width);
            if let Some(tag) = self.0.buffer.tag_table().lookup("columns") {
                tag.set_tabs(Some(&tabs));
            }
        }
        self.0.columns.replace(columns) != columns
    }

    /// Sizes the comment cards and the gap the composer floats over; call
    /// when the scroller changes size.
    pub fn relayout(&self) {
        let inner = &self.0;
        if inner.split.get() && self.update_columns() {
            // Calls this again, with the rows wrapped to the new width.
            self.render(Change::Content);
            return;
        }
        let gutter_width = ViewExt::gutter(&inner.view, gtk::TextWindowType::Left).width();
        let width = (inner.scroller.window().width() - gutter_width - 64).clamp(240, MAX_THREAD_WIDTH);
        if width != inner.child_width.get() {
            inner.child_width.set(width);
            inner.history.content.set_width_request(width);
        }
        for (child, side) in inner.children.borrow().iter() {
            let (start, width) = self.thread_place(*side);
            child.set_margin_start(start);
            child.set_width_request(width);
        }
        // History cards line up with the inline ones.
        inner.history.content.set_margin_start(gutter_width + inner.view.left_margin());

        // Reserve room for the composer below (or above) its line.
        let composer = &inner.composer.root;
        let gap = inner.gap.get().filter(|_| composer.is_visible());
        let (_, width) = self.thread_place(self.target_side());
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

    /// Where comments on `side` go across the view, as their offset from the
    /// start of the code and their width: side by side, below their half.
    fn thread_place(&self, side: Option<Side>) -> (i32, i32) {
        let inner = &self.0;
        let full = inner.child_width.get();
        let ((half, end), cell) = (inner.tab_stops.get(), inner.cell.get());
        if !inner.split.get() || half <= 0 {
            return (0, full);
        }
        // The new half starts after the divider and a space.
        let (start, room) = match side {
            None => return (0, full),
            Some(Side::Old) => (0, half - 2 * cell),
            Some(Side::New) => (half + 2 * cell, end - half - 2 * cell),
        };
        (start, room.clamp(160, MAX_THREAD_WIDTH))
    }

    /// The side the composer's comment is on.
    fn target_side(&self) -> Option<Side> {
        self.0.target.borrow().as_ref().and_then(|t| t.anchor.as_ref()).map(|a| a.side)
    }

    /// Where the composer floats: over the gap below its line.
    fn composer_rect(&self) -> Option<gtk::gdk::Rectangle> {
        let inner = &self.0;
        let line = inner.gap.get()?;
        let (start, width) = self.thread_place(self.target_side());
        let (_, height, _, _) = inner.composer.root.measure(gtk::Orientation::Vertical, width);
        let (y, _) = inner.view.line_yrange(&iter_at_line(&inner.buffer, line));
        let (x, top) = self.to_overlay(0, y + 8)?;
        Some(gtk::gdk::Rectangle::new(x + inner.view.left_margin() + start, top, width, height))
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

    /// The first buffer line showing `FileDiff::lines()[index]`.
    fn buffer_line_of(&self, index: usize) -> Option<usize> {
        self.0.rows.borrow().iter().position(|r| r.shows(index))
    }

    /// The last buffer line showing `FileDiff::lines()[index]`, which wraps
    /// over several side by side.
    fn last_buffer_line_of(&self, index: usize) -> Option<usize> {
        self.0.rows.borrow().iter().rposition(|r| r.shows(index))
    }

    /// Converts a point in the overlay to a height in the scroller, which
    /// stays put while the view scrolls under the pointer.
    fn to_scroller_y(&self, x: f64, y: f64) -> Option<f64> {
        #[allow(clippy::cast_possible_truncation)]
        let point = gtk::graphene::Point::new(x as f32, y as f32);
        self.0.overlay.compute_point(self.0.scroller.window(), &point).map(|p| f64::from(p.y()))
    }

    /// The buffer line shown at height `y` of the view, clamped to the view.
    fn line_at_view_y(&self, y: f64) -> i32 {
        let view = &self.0.view;
        let (_, origin) = view.buffer_to_window_coords(gtk::TextWindowType::Widget, 0, 0);
        // The view is only as tall as the part of the diff on screen, maybe none of it.
        #[allow(clippy::cast_possible_truncation)]
        let y = y.clamp(0.0, f64::from((view.height() - 1).max(0))) as i32;
        view.line_at_y(y - origin).0.line()
    }

    /// The buffer line shown at height `y` of the scroller, clamped to what is on screen.
    fn line_at_scroller_y(&self, y: f64) -> i32 {
        let window = self.0.scroller.window();
        let y = y.clamp(self.0.scroller.inset(), f64::from(window.height() - 1).max(0.0));
        #[allow(clippy::cast_possible_truncation)]
        let point = window.compute_point(&self.0.view, &gtk::graphene::Point::new(0.0, y as f32));
        self.line_at_view_y(point.map_or(0.0, |p| f64::from(p.y())))
    }

    /// Shows the comment button on the code line at height `y` of the view, or hides it.
    fn hover(&self, y: Option<f64>) {
        // While picking lines, it stays on the first one.
        if self.0.drag_y.get().is_some() {
            return;
        }
        let line = y.map(|y| self.line_at_view_y(y)).filter(|&line| {
            let rows = self.0.rows.borrow();
            usize::try_from(line).ok().and_then(|l| rows.get(l)).is_some_and(|r| r.kind.is_code())
        });
        self.set_hovered(line);
    }

    fn set_hovered(&self, line: Option<i32>) {
        if self.0.hovered.replace(line) != line {
            self.0.button.set_visible(line.is_some());
            self.0.overlay.queue_allocate();
        }
    }

    /// The buffer lines in the box: those being picked, or else those commented on.
    fn framed_lines(&self) -> Option<(usize, usize)> {
        match self.dragged_lines() {
            Some((first, last)) => Some((usize::try_from(first).ok()?, usize::try_from(last).ok()?)),
            None => self.0.target_lines.get(),
        }
    }

    /// Shows or hides the box once the lines in it change.
    fn update_frame(&self) {
        self.0.frame.set_visible(self.framed_lines().is_some());
        self.0.overlay.queue_allocate();
    }

    /// Where the box goes: around its lines, across the view.
    fn frame_rect(&self) -> Option<gtk::gdk::Rectangle> {
        let (first, last) = self.framed_lines()?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let (top, bottom) = self.line_span(first as i32, last as i32)?;
        #[allow(clippy::cast_possible_truncation)]
        let point = gtk::graphene::Point::new(0.0, top as f32);
        let origin = self.0.view.compute_point(&self.0.overlay, &point)?;
        #[allow(clippy::cast_possible_truncation)]
        let (y, height) = (origin.y().round() as i32, (bottom - top).round() as i32);
        Some(gtk::gdk::Rectangle::new(2, y - 1, self.0.view.width() - 4, height + 2))
    }

    /// Where the button goes: between the line numbers and the code, centred
    /// on the first row of its line.
    fn button_rect(&self) -> Option<gtk::gdk::Rectangle> {
        let line = self.0.hovered.get()?;
        let row = self.0.view.iter_location(&self.0.buffer.iter_at_line(line)?);
        let (x, y) = self.to_overlay(0, row.y())?;
        let (_, width, _, _) = self.0.button.measure(gtk::Orientation::Horizontal, -1);
        let (_, height, _, _) = self.0.button.measure(gtk::Orientation::Vertical, width);
        let x = x + (self.0.view.left_margin() - width) / 2;
        Some(gtk::gdk::Rectangle::new(x, y + (row.height() - height) / 2, width, height))
    }

    /// Starts picking lines from the one at height `y` of the scroller.
    fn begin_line_drag(&self, y: f64, extend: bool) {
        let line = self.line_at_scroller_y(y);
        if !extend || self.0.selection_origin.get().is_none() {
            self.0.selection_origin.set(Some(line));
        }
        self.set_hovered(self.0.selection_origin.get());
        self.0.drag_y.set(Some(y));
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
        let line = self.line_at_scroller_y(y);
        Some((origin.min(line), origin.max(line)))
    }

    fn extend_line_drag(&self) {
        self.update_frame();
    }

    /// Scrolls towards the pointer while it is dragged past the top or bottom edge.
    fn autoscroll(&self) {
        let Some(y) = self.0.drag_y.get() else { return };
        let (top, bottom) = (self.0.scroller.inset(), f64::from(self.0.scroller.window().height()));
        let overshoot = if y < top { y - top } else { (y - bottom).max(0.0) };
        if overshoot != 0.0 {
            self.0.scroller.scroll_by(overshoot.clamp(-60.0, 60.0) / 3.0);
            self.extend_line_drag();
        }
    }

    /// Comments on the picked lines once the button is released.
    fn end_line_drag(&self) {
        let lines = self.dragged_lines().and_then(|(first, last)| self.covering(first, last, None));
        self.0.drag_y.set(None);
        self.update_frame();
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
            RowKind::Widget | RowKind::Info | RowKind::Spacer | RowKind::Hidden => String::new(),
            // The line's colour tells added from removed.
            RowKind::Line(_) | RowKind::Pair(..) => {
                format!("<span alpha=\"50%\">{:>w$} {:>w$}</span>", number(row.old_no), number(row.new_no), w = width)
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

    fn render(&self, change: Change) {
        // The lines move: hidden until the pointer does.
        self.set_hovered(None);
        let diff_ref = self.0.diff.borrow();
        let Some(diff) = diff_ref.as_ref() else { return };
        let notes = self.0.notes.borrow();
        let target = self.0.target.borrow().clone();

        // Group notes by the line they follow; outdated and file notes go on
        // top. Notes already sent belong to the history instead.
        let (sent, unsent): (Vec<&Note>, Vec<&Note>) = notes.iter().partition(|n| n.sent_at.is_some());
        let now = crate::core::now();
        let history: Vec<gtk::Box> = sent.iter().map(|note| self.build_card(note, diff, now, true)).collect();
        let mut by_line: BTreeMap<usize, Vec<&Note>> = BTreeMap::new();
        let mut top: Vec<&Note> = Vec::new();
        for note in unsent {
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
        // Side by side, the width each side wraps at, and tabs expand to.
        let split = self.0.split.get().then(|| {
            self.update_columns();
            let columns = self.0.columns.get();
            Columns {
                width: if columns == 0 { 80 } else { columns },
                tab: usize::try_from(self.0.view.tab_width()).unwrap_or(8),
            }
        });
        // Every line, the last included, ends in a newline so line tags can apply to it.
        let mut text = String::new();
        let mut rows = Vec::with_capacity(diff.line_count() + by_line.len() + 2);
        let mut children: Vec<(gtk::Widget, Option<Side>)> = Vec::new();

        // The composer gets an empty line of its own, below the target lines and their comments.
        let spacer_after = target.as_ref().map(|t| t.range.map(|(_, last)| last));
        if !top.is_empty() {
            text.push('\n');
            rows.push(Row::plain(RowKind::Widget));
            children.push((self.build_thread(&top, diff).upcast(), None));
        }
        if spacer_after == Some(None) {
            text.push('\n');
            rows.push(Row::plain(RowKind::Spacer));
        }
        if diff.hunks.is_empty() {
            let _ = writeln!(text, "{}", describe_without_hunks(diff));
            rows.push(Row::plain(RowKind::Info));
        }
        // Unchanged lines between the hunks: shown once revealed and read
        // from the file, else a row standing for them.
        let source_ref = self.0.source.borrow();
        let source = source_ref.as_deref().filter(|lines| diff.matches_new_side(lines));
        let language = sourceview5::LanguageManager::default().guess_language(Some(diff.path()), None);
        for (side, lines) in self.0.highlight.iter().zip(diff.sides(source)) {
            side.set(language.as_ref(), &lines);
        }
        let gaps = diff.gaps(source.and_then(|lines| u32::try_from(lines.len()).ok()));
        let mut revealed = self.0.revealed.borrow_mut();
        let mut gap_rows = |text: &mut String, rows: &mut Vec<Row>, before: usize, context: &str| {
            let Some(&gap) = gaps.iter().find(|g| g.before == before) else { return };
            let shown = source.filter(|_| revealed.iter().any(|&(a, b)| a <= gap.new_end() && b >= gap.new_start));
            let range = (gap.new_start, gap.new_end());
            if shown.is_some() && !revealed.contains(&range) {
                // Keep it shown as it grows or moves with later edits.
                revealed.push(range);
            }
            push_gap(text, rows, gap, shown, context, split);
        };
        let mut index = 0;
        for (number, hunk) in diff.hunks.iter().enumerate() {
            let context = hunk.header.splitn(3, "@@").nth(2).unwrap_or_default().trim();
            gap_rows(&mut text, &mut rows, number, context);
            if let Some(columns) = split {
                for (old, new) in split::pairs(&hunk.lines) {
                    let (old, new) = (old.map(|i| index + i), new.map(|i| index + i));
                    let line = |i: Option<usize>| i.map(|i| &hunk.lines[i - index]);
                    push_pair(&mut text, &mut rows, (line(old), line(new)), (old, new), columns);
                    // Comments, then the composer, below the pair: on its old line, then its new one.
                    let row = Row { index: new, old_index: old, ..Row::plain(RowKind::Spacer) };
                    for i in row.indices() {
                        if let Some(thread) = by_line.get(&i) {
                            text.push('\n');
                            rows.push(Row::plain(RowKind::Widget));
                            let side = side_of_kind(hunk.lines[i - index].kind);
                            children.push((self.build_thread(thread, diff).upcast(), Some(side)));
                        }
                    }
                    if row.indices().any(|i| spacer_after == Some(Some(i))) {
                        text.push('\n');
                        rows.push(Row::plain(RowKind::Spacer));
                    }
                }
                index += hunk.lines.len();
                continue;
            }
            for line in &hunk.lines {
                text.push_str(&line.text);
                text.push('\n');
                let side = side_of_kind(line.kind);
                rows.push(Row {
                    kind: RowKind::Line(line.kind),
                    old_no: line.old_no,
                    new_no: line.new_no,
                    index: Some(index),
                    old_index: None,
                    gap: None,
                    pieces: Piece::whole(side, if side == Side::Old { line.old_no } else { line.new_no }),
                });
                if let Some(thread) = by_line.get(&index) {
                    text.push('\n');
                    rows.push(Row::plain(RowKind::Widget));
                    children.push((self.build_thread(thread, diff).upcast(), Some(side)));
                }
                if spacer_after == Some(Some(index)) {
                    text.push('\n');
                    rows.push(Row::plain(RowKind::Spacer));
                }
                index += 1;
            }
        }
        gap_rows(&mut text, &mut rows, diff.hunks.len(), "");
        drop(revealed);
        // Ask for the file's text once per version of the diff.
        let load = (diff.has_gaps() && source.is_none())
            .then(|| diff.fingerprint())
            .filter(|fingerprint| self.0.source_for.borrow().as_ref() != Some(fingerprint));
        drop(source_ref);
        let max_line = rows.iter().filter_map(|r| r.old_no.max(r.new_no)).max().unwrap_or(0);
        let width = max_line.to_string().len().max(3);
        self.0.number_width.set(width);
        let sample = format!("{} {}", "0".repeat(width), "0".repeat(width));
        // Measured with the gutter's own (monospace) font.
        let (sample_width, _) = self.0.gutter.create_pango_layout(Some(&sample)).pixel_size();
        let gutter_width = sample_width + 2 * self.0.gutter.xpad() + 4;
        self.0.gutter.set_width_request(gutter_width);

        let buffer = &self.0.buffer;
        drop(notes);
        let hunks = diff.hunks.len();
        drop(diff_ref);

        // Detach the cards before their anchors are deleted: GTK crashes if a
        // child under the pointer is removed while the buffer is mid-update.
        let threads = self.0.children.borrow_mut().drain(..).map(|(child, _)| child).collect::<Vec<_>>();
        for child in threads.into_iter().chain(self.0.expanders.borrow_mut().drain(..)) {
            self.0.view.remove(&child);
        }
        if change == Change::File || !self.update_lines(&rows, &text) {
            self.unpin();
            buffer.set_text(&text);
            self.0.rows.replace(rows);
            let lines = self.0.rows.borrow().len();
            self.apply_line_tags(0..lines);
            buffer.place_cursor(&buffer.start_iter());
        }
        self.apply_target(target.as_ref());
        for (line, (child, _)) in self.widget_lines().into_iter().zip(&children) {
            // Threads on lines that were kept still have their anchor.
            let anchor = iter_at_line(buffer, line)
                .child_anchor()
                .unwrap_or_else(|| buffer.create_child_anchor(&mut iter_at_line(buffer, line)));
            self.0.view.add_child_at_anchor(child, &anchor);
        }
        self.0.children.replace(children);
        let hidden: Vec<(usize, Gap)> =
            self.0.rows.borrow().iter().enumerate().filter_map(|(line, r)| r.gap.map(|gap| (line, gap))).collect();
        let mut expanders = Vec::with_capacity(hidden.len());
        for (line, gap) in hidden {
            // Rows that were kept still have their arrow's anchor.
            let anchor = iter_at_line(buffer, line)
                .child_anchor()
                .unwrap_or_else(|| buffer.create_child_anchor(&mut iter_at_line(buffer, line)));
            // The anchor starts the line: give it the row's colour.
            self.tag_lines("hidden", line, line + 1);
            let expander = build_expander(gap, hunks);
            self.0.view.add_child_at_anchor(&expander, &anchor);
            expanders.push(expander.upcast());
        }
        self.0.expanders.replace(expanders);
        // Lines may be highlighted differently, even unchanged ones.
        self.0.colored.replace(vec![false; self.0.rows.borrow().len()]);
        self.queue_coloring();
        self.show_history(&history);
        self.relayout();
        if let Some(fingerprint) = load {
            self.load_source(fingerprint);
        }
    }

    /// Reads the new side of the file, then shows the unchanged lines again.
    fn load_source(&self, fingerprint: String) {
        self.0.source_for.replace(Some(fingerprint));
        let Some(load) = self.0.on_load.borrow_mut().take() else { return };
        let weak = self.weak();
        load(Box::new(move |text| {
            if let Some(inner) = weak.upgrade() {
                inner.source.replace(text.map(|t| t.lines().map(str::to_owned).collect()));
                DiffView(inner).render(Change::Content);
            }
        }));
        self.0.on_load.replace(Some(load));
    }

    /// Shows the unchanged lines of `gap`, reading the file first if needed.
    fn reveal(&self, gap: Gap) {
        self.0.revealed.borrow_mut().push((gap.new_start, gap.new_end()));
        // Read the file again if it couldn't be before.
        self.0.source_for.replace(None);
        self.render(Change::Content);
    }

    /// Puts `cards` in the history panel, offering the button only if there are any.
    fn show_history(&self, cards: &[gtk::Box]) {
        let history = &self.0.history;
        while let Some(child) = history.list.first_child() {
            history.list.remove(&child);
        }
        for card in cards {
            history.list.append(card);
        }
        history.count.set_text(&cards.len().to_string());
        history.button.set_visible(!cards.is_empty());
        history.revealer.set_reveal_child(history.button.is_active() && !cards.is_empty());
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
                    self.move_pin(line, delete, insert);
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
        for tag in [
            "added",
            "removed",
            "hidden",
            "info",
            "gap",
            "found",
            "added-side",
            "removed-side",
            "blank-side",
            "divider",
            "columns",
        ] {
            buffer.remove_tag_by_name(tag, &from, &to);
        }
        self.uncolor(&from, &to);
        self.apply_line_tags(lines);
    }

    /// Takes the highlighting off the text from `from` to `to`.
    fn uncolor(&self, from: &gtk::TextIter, to: &gtk::TextIter) {
        for tag in self.0.highlight.iter().flat_map(Highlighter::tags) {
            self.0.buffer.remove_tag(&tag, from, to);
        }
    }

    /// Colours the lines on screen once the view is laid out: at the start
    /// of the frame after the next, as lines are laid out during a frame.
    fn queue_coloring(&self) {
        if self.0.coloring.replace(true) {
            return;
        }
        let weak = self.weak();
        let frames = Cell::new(0);
        self.0.view.add_tick_callback(move |_, _| {
            frames.set(frames.get() + 1);
            if frames.get() < 2 {
                return glib::ControlFlow::Continue;
            }
            if let Some(inner) = weak.upgrade() {
                inner.coloring.set(false);
                DiffView(inner).color_visible();
            }
            glib::ControlFlow::Break
        });
    }

    /// Colours the lines on screen, and as many again either side of them,
    /// that are not yet.
    fn color_visible(&self) {
        let view = &self.0.view;
        let Some((top, bottom)) = self.0.scroller.visible_span(view).filter(|_| view.is_mapped()) else { return };
        if bottom <= top {
            return;
        }
        // Lines off screen are not laid out yet, so they are counted rather than measured.
        let (first, last) = (self.line_at_view_y(top), self.line_at_view_y(bottom));
        let (Ok(first), Ok(last)) = (usize::try_from(first), usize::try_from(last)) else { return };
        let margin = (last - first + 1).min(MAX_COLORED_LINES);
        let lines: Vec<usize> = {
            let mut colored = self.0.colored.borrow_mut();
            (first.saturating_sub(margin)..=last + margin)
                .filter(|&line| colored.get_mut(line).is_some_and(|done| !std::mem::replace(done, true)))
                .take(3 * MAX_COLORED_LINES)
                .collect()
        };
        for line in lines {
            self.color_line(line);
        }
    }

    /// Colours buffer line `line` with the highlighting of the lines it shows.
    fn color_line(&self, line: usize) {
        let Some(row) = self.0.rows.borrow().get(line).copied() else { return };
        let buffer = &self.0.buffer;
        let Ok(number) = i32::try_from(line) else { return };
        self.uncolor(&iter_at_line(buffer, line), &iter_at_line(buffer, line + 1));
        let tab = usize::try_from(self.0.view.tab_width()).unwrap_or(8);
        for piece in row.pieces.into_iter().flatten() {
            let highlight = &self.0.highlight[usize::from(piece.side == Side::New)];
            // Side by side, where the line's characters went once its tabs were expanded.
            let offsets = (self.0.split.get())
                .then(|| highlight.line(piece.number))
                .flatten()
                .map(|text| split::expanded_offsets(&text, tab));
            let expanded = |offset: usize| match &offsets {
                Some(offsets) => offsets[offset.min(offsets.len() - 1)],
                None => offset,
            };
            for span in highlight.spans(piece.number) {
                let (start, end) = (expanded(span.start).max(piece.chars.0), expanded(span.end).min(piece.chars.1));
                if start >= end {
                    continue;
                }
                let moved = |offset: usize| i32::try_from(offset - piece.chars.0 + piece.at).ok();
                if let (Some(start), Some(end)) = (moved(start), moved(end))
                    && let (Some(start), Some(end)) =
                        (buffer.iter_at_line_offset(number, start), buffer.iter_at_line_offset(number, end))
                {
                    buffer.apply_tag(&span.tag, &start, &end);
                }
            }
        }
    }

    fn widget_lines(&self) -> Vec<usize> {
        self.0.rows.borrow().iter().enumerate().filter(|(_, r)| r.kind == RowKind::Widget).map(|(i, _)| i).collect()
    }

    /// Colours buffer lines `lines` by their kind.
    fn apply_line_tags(&self, lines: Range<usize>) {
        let tag_of = |row: &Row| match row.kind {
            RowKind::Hidden => Some("hidden"),
            RowKind::Info => Some("info"),
            RowKind::Line(LineKind::Added) => Some("added"),
            RowKind::Line(LineKind::Removed) => Some("removed"),
            RowKind::Line(LineKind::Context) | RowKind::Pair(..) | RowKind::Widget | RowKind::Spacer => None,
        };
        let mut runs = Vec::new();
        let mut pairs = Vec::new();
        {
            let rows = self.0.rows.borrow();
            for (line, row) in rows.iter().enumerate().take(lines.end).skip(lines.start) {
                if let RowKind::Pair(old, new) = row.kind {
                    pairs.push((line, old, new));
                }
            }
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

        for (line, old, new) in pairs {
            self.tag_halves(line, old, new);
        }
    }

    /// Colours the halves of a side-by-side row by their kind, around the divider.
    fn tag_halves(&self, line: usize, old: Option<LineKind>, new: Option<LineKind>) {
        let buffer = &self.0.buffer;
        let start = iter_at_line(buffer, line);
        let mut end = start;
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        let Some(mut divider) = first_tab(start) else { return };
        divider.forward_char();
        let mut new_start = divider;
        new_start.forward_char();
        let tag_of = |kind: Option<LineKind>| match kind {
            None => Some("blank-side"),
            Some(LineKind::Added) => Some("added-side"),
            Some(LineKind::Removed) => Some("removed-side"),
            Some(LineKind::Context) => None,
        };
        if let Some(tag) = tag_of(old) {
            buffer.apply_tag_by_name(tag, &start, &divider);
        }
        buffer.apply_tag_by_name("divider", &divider, &new_start);
        if let Some(tag) = tag_of(new) {
            buffer.apply_tag_by_name(tag, &new_start, &end);
        }
        self.tag_lines("columns", line, line + 1);
    }

    /// Highlights the lines being commented on and opens a gap for the composer.
    fn apply_target(&self, target: Option<&Target>) {
        let (lines, spacer) = {
            let rows = self.0.rows.borrow();
            let range = target.and_then(|t| t.range);
            let lines: Vec<usize> = rows
                .iter()
                .enumerate()
                .filter(|(_, r)| r.indices().any(|i| range.is_some_and(|(first, last)| i >= first && i <= last)))
                .map(|(line, _)| line)
                .collect();
            (lines, rows.iter().position(|r| r.kind == RowKind::Spacer))
        };
        self.0.target_lines.set(lines.first().zip(lines.last()).map(|(&first, &last)| (first, last)));
        self.update_frame();
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

    /// The code lines covered by the selection, with their index range. Side
    /// by side, a selection within one half covers that side's lines only.
    fn selection(&self) -> Option<(LineAnchor, (usize, usize))> {
        let (start, end) = self.0.buffer.selection_bounds()?;
        let mut last = end.line();
        if end.starts_line() && last > start.line() {
            last -= 1;
        }
        let side = if self.0.split.get() && !end.starts_line() {
            match (side_of(&start), side_of(&end)) {
                (old, new) if old == new => Some(old),
                _ => None,
            }
        } else {
            None
        };
        self.covering(start.line(), last, side)
    }

    /// The code lines among buffer lines `first..=last`, on `side` only if
    /// given, with their index range.
    fn covering(&self, first: i32, last: i32, side: Option<Side>) -> Option<(LineAnchor, (usize, usize))> {
        let rows = self.0.rows.borrow();
        let mut indices = Vec::new();
        for row in (first..=last).filter_map(|l| usize::try_from(l).ok().and_then(|l| rows.get(l))) {
            match side {
                Some(Side::Old) => indices.extend(row.old_index),
                Some(Side::New) => indices.extend(row.index),
                None => indices.extend(row.indices()),
            }
        }
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
        self.0.composer.title.set_text(&format!("on {}", target_label(target.anchor.as_ref())));
        self.0.composer.root.set_visible(true);
        self.set_target(Some(target));
        // The target highlight replaces the selection, and the composer is
        // brought into view if it opened off screen, e.g. for a file comment.
        let buffer = &self.0.buffer;
        if let Some(line) = self.0.gap.get() {
            buffer.place_cursor(&iter_at_line(buffer, line));
            let first =
                self.0.target.borrow().as_ref().and_then(|t| t.range).and_then(|(first, _)| self.buffer_line_of(first));
            let first = first.unwrap_or(line);
            let composer = self.0.composer.root.measure(gtk::Orientation::Vertical, self.0.child_width.get()).1;
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            if let Some((top, bottom)) = self.line_span(first as i32, line as i32) {
                self.0.scroller.reveal(&self.0.view, top - 24.0, bottom + f64::from(composer) + 24.0);
            }
        }
        // Once the click that opened it is over, or the view would take focus back.
        let text = self.0.composer.text.clone();
        glib::idle_add_local_once(move || {
            text.grab_focus();
        });
    }

    /// Closes the composer, discarding what was typed.
    fn close_composer(&self) {
        let open = self.0.composer.root.is_visible();
        self.0.composer.text.buffer().set_text("");
        self.0.selection_origin.set(None);
        self.0.composer.root.set_visible(false);
        self.set_target(None);
        // Only give focus back from a closed composer: a view shown for the
        // first time, e.g. a reviewed file reopening, must not steal it.
        if open {
            self.0.view.grab_focus();
        }
    }

    /// Moves the target highlight and the composer's gap in place: re-rendering
    /// would rebuild the buffer for what is a one-line change.
    fn set_target(&self, target: Option<Target>) {
        let buffer = &self.0.buffer;
        if let Some(line) = self.0.gap.take() {
            buffer.delete(&mut iter_at_line(buffer, line), &mut iter_at_line(buffer, line + 1));
            self.0.rows.borrow_mut().remove(line);
            self.0.colored.borrow_mut().remove(line);
            self.move_pin(line, 1, 0);
        }
        if let Some(target) = &target {
            let line = self.spacer_line(target);
            buffer.insert(&mut iter_at_line(buffer, line), "\n");
            self.0.rows.borrow_mut().insert(line, Row::plain(RowKind::Spacer));
            self.0.colored.borrow_mut().insert(line, true);
            self.move_pin(line, 0, 1);
            // Don't let it take the colour of the line it was inserted into.
            buffer.remove_all_tags(&iter_at_line(buffer, line), &iter_at_line(buffer, line + 1));
        }
        self.apply_target(target.as_ref());
        self.0.target.replace(target);
        self.relayout();
    }

    /// The line `render` puts the composer's spacer on: below the target lines and their comments.
    fn spacer_line(&self, target: &Target) -> usize {
        let after = target.range.map_or(Some(0), |(_, last)| self.last_buffer_line_of(last).map(|l| l + 1));
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
            thread.append(&self.build_card(note, diff, now, false));
        }
        thread
    }

    /// A comment's card. History cards quote the lines they were about, which
    /// inline cards only do once those lines have changed.
    fn build_card(&self, note: &Note, diff: &FileDiff, now: i64, history: bool) -> gtk::Box {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
        card.add_css_class("comment-card");
        let outdated = note.anchor.as_ref().filter(|a| !a.is_current(Some(diff)));

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let time = match note.sent_at {
            Some(sent) => format!("sent {}", util::relative_time(sent, now)),
            None => util::relative_time(note.created_at, now),
        };
        let tag = if outdated.is_some() { " · outdated" } else { "" };
        let author = util::label("", &["comment-author"]);
        show_author(&author, note.author.as_ref());
        header.append(&author);
        let target = format!("on {}{tag} · {time}", target_label(note.anchor.as_ref()));
        header.append(&util::label(&target, &["comment-meta"]));
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

        if let Some(anchor) = note.anchor.as_ref().filter(|_| history || outdated.is_some()) {
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
    let author = util::label("", &["comment-author"]);
    show_author(&author, None);
    let title = util::label("", &["comment-meta"]);
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
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header.append(&author);
    header.append(&title);
    root.append(&header);
    root.append(&overlay);
    root.append(&actions);
    Composer { root, author, title, text, cancel, submit }
}

/// Shows the author's name, with their email on hover, or "Unknown" when git has no name.
fn show_author(label: &gtk::Label, author: Option<&Author>) {
    label.set_text(author.map_or("Unknown", |a| a.name.as_str()));
    label.set_tooltip_text(author.and_then(|a| a.email.as_deref()));
}

/// What a comment is on, to follow "on": "lines 4–6", or "this file".
fn target_label(anchor: Option<&LineAnchor>) -> String {
    anchor.map_or_else(|| "this file".to_owned(), |a| a.label().to_lowercase())
}

/// The layout of side-by-side rows, in characters.
#[derive(Debug, Clone, Copy)]
struct Columns {
    /// The width each side wraps at.
    width: usize,
    /// The width tabs expand to.
    tab: usize,
}

/// Adds a side-by-side row: `lines` beside each other, old then new, with their
/// indices into `FileDiff::lines()`. A side too long for its column wraps onto
/// rows of their own, with the other side blank. Each row reads `old⇥│ new⇥`:
/// the tabs, at stops set by `update_columns`, line the halves up and colour
/// each one to its end.
fn push_pair(
    text: &mut String,
    rows: &mut Vec<Row>,
    lines: (Option<&DiffLine>, Option<&DiffLine>),
    indices: (Option<usize>, Option<usize>),
    columns: Columns,
) {
    let wrap = |line: Option<&DiffLine>| {
        line.map_or_else(|| vec![String::new()], |l| split::wrap(&l.text, columns.width, columns.tab))
    };
    let (old, new) = (wrap(lines.0), wrap(lines.1));
    let numbers = [lines.0.and_then(|l| l.old_no), lines.1.and_then(|l| l.new_no)];
    // How far into each side's line, tabs expanded, its next piece starts.
    let mut from = [0, 0];
    for i in 0..old.len().max(new.len()) {
        let piece = |pieces: &[String]| pieces.get(i).map_or("", String::as_str).to_owned();
        let texts = [piece(&old), piece(&new)];
        let _ = writeln!(text, "{}\t│ {}\t", texts[0], texts[1]);
        // The new half starts after the old one, its tab, the divider and a space.
        let at = [0, texts[0].chars().count() + 3];
        let mut pieces = [None, None];
        for (side, which) in [Side::Old, Side::New].into_iter().enumerate() {
            let len = texts[side].chars().count();
            pieces[side] = numbers[side].map(|number| Piece {
                side: which,
                number,
                chars: (from[side], from[side] + len),
                at: at[side],
            });
            from[side] += len;
        }
        rows.push(Row {
            kind: RowKind::Pair(lines.0.map(|l| l.kind), lines.1.map(|l| l.kind)),
            // Numbered on the first row only.
            old_no: numbers[0].filter(|_| i == 0),
            new_no: numbers[1].filter(|_| i == 0),
            index: indices.1,
            old_index: indices.0,
            gap: None,
            pieces,
        });
    }
}

/// Adds the unchanged lines of `gap` from `shown`, the new side of the file,
/// or when they are not shown, a row standing for them, with the function
/// they are in from the hunk header.
fn push_gap(
    text: &mut String,
    rows: &mut Vec<Row>,
    gap: Gap,
    shown: Option<&[String]>,
    context: &str,
    split: Option<Columns>,
) {
    let Some(lines) = shown else {
        // After the arrow `render` puts at the start of the row.
        let _ = writeln!(text, "{context}");
        rows.push(Row { gap: Some(gap), ..Row::plain(RowKind::Hidden) });
        return;
    };
    for offset in 0..gap.len {
        let new_no = gap.new_start + offset;
        let line = usize::try_from(new_no - 1).ok().and_then(|i| lines.get(i)).map_or("", String::as_str);
        let old_no = Some(gap.old_start + offset);
        if let Some(columns) = split {
            let line = DiffLine { kind: LineKind::Context, old_no, new_no: Some(new_no), text: line.to_owned() };
            push_pair(text, rows, (Some(&line), Some(&line)), (None, None), columns);
            continue;
        }
        text.push_str(line);
        text.push('\n');
        rows.push(Row {
            kind: RowKind::Line(LineKind::Context),
            old_no,
            new_no: Some(new_no),
            index: None,
            old_index: None,
            gap: None,
            pieces: Piece::whole(Side::New, Some(new_no)),
        });
    }
}

/// The arrow at the start of a row of hidden lines, pointing to where they
/// are: up before the first change, down after the last, both ways between.
fn build_expander(gap: Gap, hunks: usize) -> gtk::Box {
    let icons: &[&str] = match gap.before {
        0 => &["pan-up-symbolic"],
        before if before >= hunks => &["pan-down-symbolic"],
        _ => &["pan-up-symbolic", "pan-down-symbolic"],
    };
    let expander = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for name in icons {
        let image = gtk::Image::from_icon_name(name);
        image.set_pixel_size(if icons.len() == 1 { 16 } else { 10 });
        expander.append(&image);
    }
    let plural = if gap.len == 1 { "" } else { "s" };
    expander.set_tooltip_text(Some(&format!("Show {} unchanged line{plural}", gap.len)));
    expander.set_valign(gtk::Align::Center);
    expander.set_cursor_from_name(Some("pointer"));
    expander.add_css_class("gap-expander");
    expander
}

fn build_history() -> History {
    let count = gtk::Label::new(None);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    content.append(&gtk::Image::from_icon_name("document-open-recent-symbolic"));
    content.append(&count);
    let button = gtk::ToggleButton::builder()
        .child(&content)
        .tooltip_text("Previous Comments")
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    button.add_css_class("flat");
    button.add_css_class("history-button");

    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    heading.append(&util::label("Previous comments", &["comment-target"]));
    heading.append(&util::label("Already sent to the agent, for reference", &["comment-meta"]));
    let list = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_halign(gtk::Align::Start);
    content.set_margin_top(10);
    content.set_margin_bottom(12);
    content.append(&heading);
    content.append(&list);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .max_content_height(320)
        .propagate_natural_height(true)
        .child(&content)
        .build();
    scroller.add_css_class("comment-history");
    let revealer =
        gtk::Revealer::builder().child(&scroller).transition_type(gtk::RevealerTransitionType::SlideDown).build();
    History { button, count, revealer, content, list }
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
            // Without the arrow's anchor, which only the buffer has.
            RowKind::Hidden => (row.kind, line.trim_start_matches('\u{FFFC}')),
            kind => (kind, *line),
        })
        .collect()
}

/// The side a diff line's comments are on.
const fn side_of_kind(kind: LineKind) -> Side {
    match kind {
        LineKind::Removed => Side::Old,
        LineKind::Context | LineKind::Added => Side::New,
    }
}

/// The tab ending the old half of the side-by-side row starting at `start`:
/// the line's first, as tabs in the code are expanded.
fn first_tab(start: gtk::TextIter) -> Option<gtk::TextIter> {
    let mut end = start;
    if !end.ends_line() {
        end.forward_to_line_end();
    }
    let mut tab = start;
    // Searching forward skips the character it starts on.
    (tab.char() == '\t' || tab.forward_find_char(|c| c == '\t', Some(&end))).then_some(tab)
}

/// The half of a side-by-side row `iter` is in: up to the tab ending the old one, or after.
fn side_of(iter: &gtk::TextIter) -> Side {
    let mut start = *iter;
    start.set_line_offset(0);
    match first_tab(start) {
        Some(tab) if iter.offset() > tab.offset() => Side::New,
        _ => Side::Old,
    }
}

fn iter_at_line(buffer: &sourceview5::Buffer, line: usize) -> gtk::TextIter {
    i32::try_from(line).ok().and_then(|l| buffer.iter_at_line(l)).unwrap_or_else(|| buffer.end_iter())
}
