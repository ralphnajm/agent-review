//! The button at the top of the file panel showing what is under review: the
//! uncommitted changes in the working directory, or a commit. It lists the
//! working directory, then the commits of `HEAD`, newest first, with a check
//! on those the user reviewed; picking one reviews it.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use gtk::prelude::*;

use crate::core::git::{Commit, Target};
use crate::ui::util;

/// Commits listed at first, and added by each Show More.
pub const PAGE: usize = 100;

type PickCallback = Box<dyn Fn(Target)>;
type MoreCallback = Box<dyn Fn(usize)>;

#[derive(Clone)]
pub struct CommitPicker(Rc<Inner>);

struct Inner {
    button: gtk::MenuButton,
    popover: gtk::Popover,
    id: gtk::Label,
    title: gtk::Label,
    list: gtk::ListBox,
    /// What each row reviews, in order; the Show More row comes after them.
    rows: RefCell<Vec<Target>>,
    /// How many commits to list.
    limit: Cell<usize>,
    on_pick: RefCell<Option<PickCallback>>,
    on_more: RefCell<Option<MoreCallback>>,
}

impl CommitPicker {
    pub fn new() -> Self {
        let id = util::label("", &["commit-id"]);
        id.set_visible(false);
        let title = util::label("Working Directory", &["commit-title"]);
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

        let heading = util::label("Commits", &["heading"]);
        heading.set_margin_start(10);
        heading.set_margin_top(6);
        heading.set_margin_bottom(4);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
        body.set_width_request(420);
        body.append(&heading);
        body.append(&scrolled);

        let popover = gtk::Popover::builder().child(&body).build();
        popover.add_css_class("history-popover");
        let button =
            gtk::MenuButton::builder().child(&content).popover(&popover).tooltip_text("Commits (Ctrl+H)").build();
        button.add_css_class("commit-button");

        let this = Self(Rc::new(Inner {
            button,
            popover,
            id,
            title,
            list,
            rows: RefCell::default(),
            limit: Cell::new(PAGE),
            on_pick: RefCell::default(),
            on_more: RefCell::default(),
        }));
        let weak = Rc::downgrade(&this.0);
        this.0.list.connect_row_activated(move |_, row| {
            let Some(inner) = weak.upgrade() else { return };
            let index = usize::try_from(row.index()).unwrap_or(usize::MAX);
            let picked = inner.rows.borrow().get(index).cloned();
            if let Some(target) = picked {
                inner.popover.popdown();
                if let Some(callback) = inner.on_pick.borrow().as_ref() {
                    callback(target);
                }
            } else {
                // The Show More row, after the commits.
                inner.limit.set(inner.limit.get() + PAGE);
                if let Some(callback) = inner.on_more.borrow().as_ref() {
                    callback(inner.limit.get());
                }
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

    /// Called with what was picked.
    pub fn connect_pick(&self, callback: impl Fn(Target) + 'static) {
        self.0.on_pick.replace(Some(Box::new(callback)));
    }

    /// Called with how many commits to list, after Show More.
    pub fn connect_more(&self, callback: impl Fn(usize) + 'static) {
        self.0.on_more.replace(Some(Box::new(callback)));
    }

    /// How many commits to list.
    pub fn limit(&self) -> usize {
        self.0.limit.get()
    }

    /// Back to the first commits only, for another project.
    pub fn reset(&self) {
        self.0.limit.set(PAGE);
    }

    /// Lists the working directory, then `commits`, newest first, checking
    /// those in `reviewed`, and shows `current` on the button.
    pub fn set_commits(&self, commits: &[Commit], current: Option<&Target>, reviewed: &HashSet<String>, now: i64) {
        let inner = &self.0;
        let commit = match current {
            Some(Target::Commit(commit)) => Some(commit),
            _ => None,
        };
        inner.id.set_text(commit.map_or("", Commit::short_id));
        inner.id.set_visible(commit.is_some());
        inner.title.set_text(commit.map_or("Working Directory", |c| c.subject.as_str()));

        let rows: Vec<Target> =
            std::iter::once(Target::Worktree).chain(commits.iter().cloned().map(Target::Commit)).collect();
        inner.list.remove_all();
        let mut selected = None;
        for target in &rows {
            let row = match target {
                Target::Worktree => build_row("Working Directory", "Uncommitted changes", false, None),
                Target::Commit(c) => {
                    let meta = format!("{} · {} · {}", c.short_id(), c.author, util::relative_time(c.time, now));
                    build_row(&c.subject, &meta, reviewed.contains(&c.id), Some(c))
                }
            };
            if current.is_some_and(|current| same(current, target)) {
                selected = Some(row.clone());
            }
            inner.list.append(&row);
        }
        if commits.len() >= inner.limit.get() {
            let more = util::label("Show More", &["dim-label"]);
            more.set_xalign(0.5);
            more.set_margin_top(8);
            more.set_margin_bottom(8);
            inner.list.append(&gtk::ListBoxRow::builder().child(&more).build());
        }
        inner.list.select_row(selected.as_ref());
        inner.rows.replace(rows);
    }
}

fn same(a: &Target, b: &Target) -> bool {
    match (a, b) {
        (Target::Worktree, Target::Worktree) => true,
        (Target::Commit(a), Target::Commit(b)) => a.id == b.id,
        _ => false,
    }
}

/// A row of the list: the title above the details, and a check once reviewed.
fn build_row(title: &str, meta: &str, reviewed: bool, commit: Option<&Commit>) -> gtk::ListBoxRow {
    let title = util::label(title, &["history-subject"]);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let meta = util::label(meta, &["dim-label", "caption"]);
    meta.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.set_margin_top(6);
    text.set_margin_bottom(6);
    text.append(&title);
    text.append(&meta);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.append(&text);
    if reviewed {
        let check = gtk::Image::from_icon_name("object-select-symbolic");
        check.add_css_class("reviewed-check");
        check.set_tooltip_text(Some("Reviewed"));
        content.append(&check);
    }
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
