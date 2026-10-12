//! Read-only, syntax highlighted rendering of one file's diff, with comment
//! threads and the comment composer shown inline below the lines they refer to.
//!
//! The buffer holds the diff lines without their `+`/`-` prefixes so the
//! file's language highlighting applies; old/new line numbers and the change
//! marker are drawn in a custom gutter. Threads are child widgets anchored on
//! otherwise empty buffer lines.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use sourceview5::prelude::*;

use crate::core::diff::{FileDiff, LineKind};
use crate::core::review::{LineAnchor, Note, Side};
use crate::ui::util;

/// Comment cards are not stretched wider than this.
const MAX_THREAD_WIDTH: i32 = 900;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Hunk,
    Line(LineKind),
    /// An empty line holding a thread or the composer.
    Widget,
    /// A message for diffs without content, e.g. binary files.
    Info,
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

#[derive(Clone)]
pub struct DiffView(Rc<Inner>);

struct Inner {
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
    /// Line where a gutter selection started, for shift-click.
    selection_origin: Cell<Option<i32>>,
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

        let gutter = sourceview5::GutterRendererText::new();
        gutter.set_xpad(8);
        gutter.set_xalign(0.0);
        ViewExt::gutter(&view, gtk::TextWindowType::Left).insert(&gutter, 0);

        let this = Self(Rc::new(Inner {
            scrolled,
            view,
            buffer,
            gutter,
            composer: build_composer(),
            rows: RefCell::default(),
            number_width: Cell::new(3),
            diff: RefCell::default(),
            notes: RefCell::default(),
            target: RefCell::default(),
            children: RefCell::default(),
            child_width: Cell::new(0),
            selection_origin: Cell::new(None),
            on_add: RefCell::default(),
            on_delete: RefCell::default(),
        }));
        this.create_tags();
        this.update_style_scheme();
        this.connect_signals();
        this.connect_composer();
        this
    }

    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.0.scrolled
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

        let weak = self.weak();
        self.0.gutter.connect_query_activatable(move |_, iter, _| {
            weak.upgrade().is_some_and(|inner| DiffView(inner).row(iter.line()).is_some_and(|r| r.index.is_some()))
        });

        // Clicking a line number comments on that line; shift-click extends the range.
        let weak = self.weak();
        self.0.gutter.connect_activate(move |_, iter, _, _, modifiers, _| {
            let Some(inner) = weak.upgrade() else { return };
            let this = DiffView(inner);
            let line = iter.line();
            let origin = match this.0.selection_origin.get() {
                Some(origin) if modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK) => origin,
                _ => line,
            };
            this.0.selection_origin.set(Some(origin));
            this.select_lines(origin.min(line), origin.max(line));
            this.open_composer(true);
        });

        // Keep inline widgets as wide as the visible text area.
        let weak = self.weak();
        self.0.view.add_tick_callback(move |_, _| {
            let Some(inner) = weak.upgrade() else { return glib::ControlFlow::Break };
            DiffView(inner).sync_child_width();
            glib::ControlFlow::Continue
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
        // The composer is only inserted into the view when opened: take focus whenever it appears.
        composer.text.connect_map(|text| {
            text.grab_focus();
        });

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
        composer.text.buffer().connect_changed(move |buffer| {
            submit.set_sensitive(!buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).trim().is_empty());
        });
        composer.submit.set_sensitive(false);
    }

    fn sync_child_width(&self) {
        let gutter_width = ViewExt::gutter(&self.0.view, gtk::TextWindowType::Left).width();
        let width = (self.0.scrolled.width() - gutter_width - 64).clamp(240, MAX_THREAD_WIDTH);
        if width != self.0.child_width.get() {
            self.0.child_width.set(width);
            for child in self.0.children.borrow().iter() {
                child.set_width_request(width);
            }
        }
    }

    fn row(&self, line: i32) -> Option<Row> {
        usize::try_from(line).ok().and_then(|l| self.0.rows.borrow().get(l).copied())
    }

    fn gutter_markup(&self, line: u32) -> String {
        let Some(row) = usize::try_from(line).ok().and_then(|l| self.0.rows.borrow().get(l).copied()) else {
            return String::new();
        };
        let width = self.0.number_width.get();
        let number = |n: Option<u32>| n.map_or_else(String::new, |n| n.to_string());
        match row.kind {
            RowKind::Widget | RowKind::Info => String::new(),
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
            self.0.target.replace(None);
            self.0.composer.text.buffer().set_text("");
            self.0.selection_origin.set(None);
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
        self.render(same_file);
    }

    pub fn clear(&self) {
        self.detach_composer();
        self.0.buffer.set_text("");
        self.0.rows.borrow_mut().clear();
        self.0.children.borrow_mut().clear();
        self.0.diff.replace(None);
        self.0.notes.borrow_mut().clear();
        self.0.target.replace(None);
    }

    /// Forgets what is shown, so the next `show` re-renders.
    pub fn invalidate(&self) {
        self.0.diff.replace(None);
    }

    fn detach_composer(&self) {
        let root = &self.0.composer.root;
        if let Some(parent) = root.parent().and_downcast::<gtk::Box>() {
            parent.remove(root);
        }
    }

    fn render(&self, keep_scroll: bool) {
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
        let composer_after = target.as_ref().and_then(|t| t.range).map(|(_, end)| end);
        let composer_on_file = target.as_ref().is_some_and(|t| t.anchor.is_none());

        let mut text = String::new();
        let mut rows = Vec::with_capacity(diff.line_count() + by_line.len() + 2);
        let mut children: Vec<gtk::Widget> = Vec::new();
        self.detach_composer();

        if !top.is_empty() || composer_on_file {
            text.push('\n');
            rows.push(Row::plain(RowKind::Widget));
            children.push(self.build_thread(&top, composer_on_file, diff).upcast());
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

            let thread = by_line.get(&index);
            let with_composer = composer_after == Some(index);
            if thread.is_some() || with_composer {
                text.push('\n');
                rows.push(Row::plain(RowKind::Widget));
                let notes: &[&Note] = thread.map_or(&[], Vec::as_slice);
                children.push(self.build_thread(notes, with_composer, diff).upcast());
            }
        }
        text.pop();

        let width = max_line.to_string().len().max(3);
        self.0.number_width.set(width);
        let sample = format!("{} {} −", "0".repeat(width), "0".repeat(width));
        // Measured with the gutter's own (monospace) font.
        let (sample_width, _) = self.0.gutter.create_pango_layout(Some(&sample)).pixel_size();
        self.0.gutter.set_width_request(sample_width + 2 * self.0.gutter.xpad() + 4);

        let adjustment = self.0.scrolled.vadjustment();
        let scroll = keep_scroll.then(|| adjustment.value());

        let buffer = &self.0.buffer;
        let language = sourceview5::LanguageManager::default().guess_language(Some(diff.path()), None);
        if buffer.language() != language {
            buffer.set_language(language.as_ref());
        }
        drop(notes);
        drop(diff_ref);

        buffer.set_text(&text);
        self.0.rows.replace(rows);
        self.apply_line_tags(target.as_ref().and_then(|t| t.range));
        for (line, child) in self.widget_lines().into_iter().zip(&children) {
            let mut iter = iter_at_line(buffer, line);
            let anchor = buffer.create_child_anchor(&mut iter);
            self.0.view.add_child_at_anchor(child, &anchor);
        }
        buffer.place_cursor(&buffer.start_iter());

        self.0.child_width.set(0);
        self.0.children.replace(children);
        self.sync_child_width();

        match scroll {
            // Wait for the new text to be measured before restoring the position.
            Some(value) => {
                glib::idle_add_local_once(move || adjustment.set_value(value));
            }
            None => adjustment.set_value(0.0),
        }
    }

    fn widget_lines(&self) -> Vec<usize> {
        self.0.rows.borrow().iter().enumerate().filter(|(_, r)| r.kind == RowKind::Widget).map(|(i, _)| i).collect()
    }

    fn apply_line_tags(&self, target: Option<(usize, usize)>) {
        let tag_of = |row: &Row| match row.kind {
            RowKind::Hunk => Some("hunk"),
            RowKind::Info => Some("info"),
            RowKind::Line(LineKind::Added) => Some("added"),
            RowKind::Line(LineKind::Removed) => Some("removed"),
            RowKind::Line(LineKind::Context) | RowKind::Widget => None,
        };
        let mut runs = Vec::new();
        let mut target_lines = Vec::new();
        {
            let rows = self.0.rows.borrow();
            let mut start = 0;
            while start < rows.len() {
                let tag = tag_of(&rows[start]);
                let end = rows[start..].iter().position(|r| tag_of(r) != tag).map_or(rows.len(), |n| start + n);
                if let Some(tag) = tag {
                    runs.push((tag, start, end));
                }
                start = end;
            }
            if let Some((first, last)) = target {
                target_lines.extend(
                    rows.iter()
                        .enumerate()
                        .filter(|(_, r)| r.index.is_some_and(|i| i >= first && i <= last))
                        .map(|(line, _)| line),
                );
            }
        }
        for (tag, start, end) in runs {
            self.tag_lines(tag, start, end);
        }
        if let (Some(&from), Some(&to)) = (target_lines.first(), target_lines.last()) {
            self.tag_lines("target", from, to + 1);
        }
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
        let rows = self.0.rows.borrow();
        let indices: Vec<usize> = (start.line()..=last)
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
        if self.0.diff.borrow().is_none() {
            return;
        }
        let target = match self.selection().filter(|_| lines) {
            Some((anchor, range)) => Target { anchor: Some(anchor), range: Some(range) },
            None => Target { anchor: None, range: None },
        };
        let title = target
            .anchor
            .as_ref()
            .map_or_else(|| "Comment on this file".to_owned(), |a| format!("Comment on {}", a.label().to_lowercase()));
        self.0.composer.title.set_text(&title);
        self.0.target.replace(Some(target));
        self.render(true);
        // After the click that opened it: the view would otherwise take focus back.
        let text = self.0.composer.text.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(60), move || {
            text.grab_focus();
        });
    }

    fn close_composer(&self) {
        self.0.selection_origin.set(None);
        if self.0.target.replace(None).is_some() {
            self.render(true);
        }
    }

    fn submit(&self) {
        let buffer = self.0.composer.text.buffer();
        let body = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
        let Some(target) = self.0.target.borrow().clone() else { return };
        if body.trim().is_empty() {
            return;
        }
        buffer.set_text("");
        self.0.target.replace(None);
        self.0.selection_origin.set(None);
        if let Some(callback) = self.0.on_add.borrow().as_ref() {
            callback(target.anchor, body);
        }
        // Normally already re-rendered with the new comment; this closes the composer otherwise.
        self.render(true);
    }

    fn build_thread(&self, notes: &[&Note], with_composer: bool, diff: &FileDiff) -> gtk::Box {
        let thread = gtk::Box::new(gtk::Orientation::Vertical, 6);
        thread.add_css_class("comment-thread");
        // Don't let the cards' expanding contents make the text view expand.
        thread.set_hexpand(false);
        thread.set_halign(gtk::Align::Start);
        let now = crate::core::now();
        for note in notes {
            thread.append(&self.build_card(note, diff, now));
        }
        if with_composer {
            thread.append(&self.0.composer.root);
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

fn iter_at_line(buffer: &sourceview5::Buffer, line: usize) -> gtk::TextIter {
    i32::try_from(line).ok().and_then(|l| buffer.iter_at_line(l)).unwrap_or_else(|| buffer.end_iter())
}
