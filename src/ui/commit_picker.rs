//! The button at the top of the file panel showing what is being reviewed:
//! the uncommitted changes, or a commit. It opens the history as a graph, in
//! which picking a commit reviews it, and the top row goes back to the
//! uncommitted changes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::cairo;
use gtk::prelude::*;

use crate::core::git::{Commit, History};
use crate::core::graph::{self, Row};
use crate::ui::util;

/// Commits listed at first, and added by each Show More.
const PAGE: usize = 200;
const LANE_WIDTH: i32 = 14;
/// Lanes beyond this many are cut off.
const MAX_LANES: usize = 8;
const DOT_RADIUS: f64 = 4.0;
/// One colour per lane, in turn: blue, green, orange, purple, red, teal.
const LANE_COLORS: [(f64, f64, f64); 6] = [
    (0.21, 0.52, 0.89),
    (0.15, 0.64, 0.41),
    (0.90, 0.38, 0.0),
    (0.57, 0.25, 0.67),
    (0.88, 0.11, 0.14),
    (0.13, 0.56, 0.64),
];

type PickCallback = Box<dyn Fn(Option<Commit>)>;
type LoadCallback = Box<dyn Fn(usize)>;

#[derive(Clone)]
pub struct CommitPicker(Rc<Inner>);

struct Inner {
    button: gtk::MenuButton,
    popover: gtk::Popover,
    id: gtk::Label,
    title: gtk::Label,
    stack: gtk::Stack,
    message: gtk::Label,
    list: gtk::ListBox,
    /// What each row reviews, in order: `None` for the uncommitted changes.
    rows: RefCell<Vec<Option<Commit>>>,
    /// The commit being reviewed.
    current: RefCell<Option<String>>,
    /// How many commits to list.
    limit: Cell<usize>,
    /// Whether there may be more commits than listed.
    more: Cell<bool>,
    on_pick: RefCell<Option<PickCallback>>,
    on_load: RefCell<Option<LoadCallback>>,
}

impl CommitPicker {
    pub fn new() -> Self {
        let id = util::label("", &["commit-id"]);
        id.set_visible(false);
        let title = util::label("Uncommitted Changes", &["commit-title"]);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_hexpand(true);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.append(&gtk::Image::from_icon_name("document-open-recent-symbolic"));
        content.append(&id);
        content.append(&title);
        content.append(&gtk::Image::from_icon_name("pan-down-symbolic"));

        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        list.add_css_class("history-list");
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(520)
            .child(&list)
            .build();
        let loading = gtk::Spinner::builder()
            .spinning(true)
            .width_request(24)
            .height_request(24)
            .margin_top(24)
            .margin_bottom(24)
            .build();
        let message = util::label("", &["dim-label"]);
        message.set_wrap(true);
        message.set_xalign(0.5);
        message.set_margin_top(18);
        message.set_margin_bottom(18);
        let stack = gtk::Stack::builder().vhomogeneous(false).build();
        stack.add_named(&loading, Some("loading"));
        stack.add_named(&message, Some("message"));
        stack.add_named(&scrolled, Some("list"));

        let heading = util::label("History", &["heading"]);
        heading.set_margin_start(10);
        heading.set_margin_top(6);
        heading.set_margin_bottom(4);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
        body.set_width_request(460);
        body.append(&heading);
        body.append(&stack);

        let popover = gtk::Popover::builder().child(&body).build();
        popover.add_css_class("history-popover");
        let button = gtk::MenuButton::builder()
            .child(&content)
            .popover(&popover)
            .tooltip_text("Review a Commit (Ctrl+H)")
            .build();
        button.add_css_class("commit-button");

        let this = Self(Rc::new(Inner {
            button,
            popover,
            id,
            title,
            stack,
            message,
            list,
            rows: RefCell::default(),
            current: RefCell::default(),
            limit: Cell::new(PAGE),
            more: Cell::new(false),
            on_pick: RefCell::default(),
            on_load: RefCell::default(),
        }));

        let weak = Rc::downgrade(&this.0);
        this.0.popover.connect_show(move |_| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).load();
            }
        });
        let weak = Rc::downgrade(&this.0);
        this.0.list.connect_row_activated(move |_, row| {
            let Some(inner) = weak.upgrade() else { return };
            let index = usize::try_from(row.index()).unwrap_or(usize::MAX);
            let picked = inner.rows.borrow().get(index).cloned();
            if let Some(commit) = picked {
                inner.popover.popdown();
                if let Some(callback) = inner.on_pick.borrow().as_ref() {
                    callback(commit);
                }
            } else {
                // The Show More row, after the commits.
                inner.limit.set(inner.limit.get() + PAGE);
                Self(inner).load();
            }
        });
        this
    }

    pub fn widget(&self) -> &gtk::MenuButton {
        &self.0.button
    }

    pub fn popup(&self) {
        self.0.button.popup();
    }

    /// Called with the commit picked, or `None` for the uncommitted changes.
    pub fn connect_pick(&self, callback: impl Fn(Option<Commit>) + 'static) {
        self.0.on_pick.replace(Some(Box::new(callback)));
    }

    /// Called to read the latest commits, at most as many as given; reply with
    /// [`Self::set_history`].
    pub fn connect_load(&self, callback: impl Fn(usize) + 'static) {
        self.0.on_load.replace(Some(Box::new(callback)));
    }

    fn load(&self) {
        if self.0.rows.borrow().is_empty() {
            self.0.stack.set_visible_child_name("loading");
        }
        if let Some(callback) = self.0.on_load.borrow().as_ref() {
            callback(self.0.limit.get());
        }
    }

    /// Shows what is being reviewed on the button.
    pub fn set_current(&self, commit: Option<&Commit>) {
        let inner = &self.0;
        inner.current.replace(commit.map(|c| c.id.clone()));
        inner.id.set_text(commit.map_or("", Commit::short_id));
        inner.id.set_visible(commit.is_some());
        inner.title.set_text(commit.map_or("Uncommitted Changes", |c| c.subject.as_str()));
        inner.button.set_tooltip_text(Some(match commit {
            Some(_) => "Review Another Commit or the Uncommitted Changes (Ctrl+H)",
            None => "Review a Commit (Ctrl+H)",
        }));
    }

    /// Forgets the listed commits, for another project.
    pub fn reset(&self) {
        self.0.rows.replace(Vec::new());
        self.0.list.remove_all();
        self.0.limit.set(PAGE);
    }

    pub fn show_error(&self, message: &str) {
        self.0.message.set_text(message);
        self.0.stack.set_visible_child_name("message");
    }

    /// Lists the uncommitted changes, then `history`'s commits, as a graph.
    pub fn set_history(&self, history: &History, now: i64) {
        let inner = &self.0;
        let mut nodes: Vec<(&str, &[String])> = vec![("", history.head.as_slice())];
        nodes.extend(history.commits.iter().map(|c| (c.id.as_str(), c.parents.as_slice())));
        let graph = graph::layout(nodes);
        let lanes = graph.iter().map(Row::width).max().unwrap_or(1).min(MAX_LANES);

        inner.list.remove_all();
        let current = inner.current.borrow().clone();
        let mut selected = None;
        let commits = std::iter::once(None).chain(history.commits.iter().map(Some));
        for (row, commit) in graph.into_iter().zip(commits) {
            let widget = build_row(row, lanes, commit, now);
            if commit.map(|c| &c.id) == current.as_ref() {
                selected = Some(widget.clone());
            }
            inner.list.append(&widget);
        }
        inner.more.set(history.commits.len() >= inner.limit.get());
        if inner.more.get() {
            let more = util::label("Show More", &["dim-label"]);
            more.set_xalign(0.5);
            more.set_margin_top(8);
            more.set_margin_bottom(8);
            inner.list.append(&gtk::ListBoxRow::builder().child(&more).build());
        }
        inner.list.select_row(selected.as_ref());
        inner.rows.replace(std::iter::once(None).chain(history.commits.iter().cloned().map(Some)).collect());
        inner.stack.set_visible_child_name("list");
    }
}

/// A row of the history: its piece of the graph, then the commit's subject
/// above its id, author and age.
fn build_row(graph: Row, lanes: usize, commit: Option<&Commit>, now: i64) -> gtk::ListBoxRow {
    let width = i32::try_from(lanes).unwrap_or(1) * LANE_WIDTH + 4;
    let area = gtk::DrawingArea::builder().content_width(width).build();
    let hollow = commit.is_none();
    area.set_draw_func(move |_, cr, _, height| draw(cr, &graph, f64::from(height), hollow));

    let title = util::label(commit.map_or("Uncommitted Changes", |c| c.subject.as_str()), &["history-subject"]);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_hexpand(true);
    let first_line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    for name in commit.map_or(&[][..], |c| c.refs.as_slice()) {
        let chip = util::label(name, &["chip", "ref-chip"]);
        if commit.is_some_and(|c| c.is_head && c.refs.first() == Some(name)) {
            chip.add_css_class("head");
        }
        first_line.append(&chip);
    }
    first_line.append(&title);
    let meta = match commit {
        Some(c) => format!("{} · {} · {}", c.short_id(), c.author, util::relative_time(c.time, now)),
        None => "Staged, unstaged and untracked".to_owned(),
    };
    let meta = util::label(&meta, &["dim-label", "caption"]);
    meta.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_valign(gtk::Align::Center);
    text.set_margin_top(6);
    text.set_margin_bottom(6);
    text.append(&first_line);
    text.append(&meta);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&area);
    content.append(&text);

    let row = gtk::ListBoxRow::builder().child(&content).build();
    if let Some(commit) = commit {
        let message = if commit.body.is_empty() {
            commit.subject.clone()
        } else {
            format!("{}\n\n{}", commit.subject, commit.body)
        };
        row.set_tooltip_text(Some(&message));
    }
    row
}

/// The middle of `lane`, from the left edge.
fn lane_x(lane: usize) -> f64 {
    let width = f64::from(LANE_WIDTH);
    f64::from(u32::try_from(lane).unwrap_or(u32::MAX)) * width + width / 2.0 + 2.0
}

fn set_lane_color(cr: &cairo::Context, lane: usize) {
    let (r, g, b) = LANE_COLORS[lane % LANE_COLORS.len()];
    cr.set_source_rgb(r, g, b);
}

/// Draws a row of the graph; a `hollow` dot stands for the uncommitted changes.
fn draw(cr: &cairo::Context, row: &Row, height: f64, hollow: bool) {
    let middle = height / 2.0;
    cr.set_line_width(2.0);
    for &(from, to) in &row.up {
        set_lane_color(cr, from);
        line(cr, (lane_x(from), 0.0), (lane_x(to), middle));
    }
    for &(from, to) in &row.down {
        set_lane_color(cr, to);
        // Lines leave a hollow dot from its edge.
        let start = if hollow && from == row.lane { middle + DOT_RADIUS } else { middle };
        line(cr, (lane_x(from), start), (lane_x(to), height));
    }
    set_lane_color(cr, row.lane);
    cr.arc(lane_x(row.lane), middle, DOT_RADIUS, 0.0, std::f64::consts::TAU);
    if hollow {
        cr.set_line_width(1.5);
        let _ = cr.stroke();
    } else {
        let _ = cr.fill();
    }
}

/// A straight line down, or an S-curve from one lane to another.
fn line(cr: &cairo::Context, (x1, y1): (f64, f64), (x2, y2): (f64, f64)) {
    cr.move_to(x1, y1);
    if (x1 - x2).abs() < f64::EPSILON {
        cr.line_to(x2, y2);
    } else {
        let y = f64::midpoint(y1, y2);
        cr.curve_to(x1, y, x2, y, x2, y2);
    }
    let _ = cr.stroke();
}
