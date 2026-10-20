//! The button at the top of the file panel showing the version under review.
//! It lists the versions the agent asked to have reviewed, newest first,
//! marking those already reviewed and those the repository has moved past;
//! picking one reviews it.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use gtk::prelude::*;

use crate::core::queue::ReviewRequest;
use crate::ui::util;

type PickCallback = Box<dyn Fn(ReviewRequest)>;

#[derive(Clone)]
pub struct VersionPicker(Rc<Inner>);

struct Inner {
    button: gtk::MenuButton,
    popover: gtk::Popover,
    id: gtk::Label,
    title: gtk::Label,
    stack: gtk::Stack,
    list: gtk::ListBox,
    /// The version each row reviews, in order.
    rows: RefCell<Vec<ReviewRequest>>,
    on_pick: RefCell<Option<PickCallback>>,
}

impl VersionPicker {
    pub fn new() -> Self {
        let id = util::label("", &["commit-id"]);
        id.set_visible(false);
        let title = util::label("No Version", &["commit-title"]);
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
        let empty = util::label(
            "No versions yet. When your agent asks you to review its work, the version appears here.",
            &["dim-label"],
        );
        empty.set_wrap(true);
        empty.set_xalign(0.5);
        empty.set_margin_top(18);
        empty.set_margin_bottom(18);
        empty.set_margin_start(12);
        empty.set_margin_end(12);
        let stack = gtk::Stack::builder().vhomogeneous(false).build();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&scrolled, Some("list"));

        let heading = util::label("Versions to Review", &["heading"]);
        heading.set_margin_start(10);
        heading.set_margin_top(6);
        heading.set_margin_bottom(4);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
        body.set_width_request(420);
        body.append(&heading);
        body.append(&stack);

        let popover = gtk::Popover::builder().child(&body).build();
        popover.add_css_class("history-popover");
        let button = gtk::MenuButton::builder()
            .child(&content)
            .popover(&popover)
            .tooltip_text("Versions to Review (Ctrl+H)")
            .build();
        button.add_css_class("commit-button");

        let this = Self(Rc::new(Inner {
            button,
            popover,
            id,
            title,
            stack,
            list,
            rows: RefCell::default(),
            on_pick: RefCell::default(),
        }));
        let weak = Rc::downgrade(&this.0);
        this.0.list.connect_row_activated(move |_, row| {
            let Some(inner) = weak.upgrade() else { return };
            let index = usize::try_from(row.index()).unwrap_or(usize::MAX);
            let picked = inner.rows.borrow().get(index).cloned();
            if let Some(request) = picked {
                inner.popover.popdown();
                if let Some(callback) = inner.on_pick.borrow().as_ref() {
                    callback(request);
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

    /// Called with the version picked.
    pub fn connect_pick(&self, callback: impl Fn(ReviewRequest) + 'static) {
        self.0.on_pick.replace(Some(Box::new(callback)));
    }

    /// Lists `requests` newest first, and shows `current` on the button.
    /// `head` is where the repository is, and `reviewed` the ids of the
    /// versions the user posted a review of.
    pub fn set_versions(
        &self,
        requests: &[ReviewRequest],
        current: Option<&ReviewRequest>,
        head: Option<&str>,
        reviewed: &HashSet<u64>,
        now: i64,
    ) {
        let inner = &self.0;
        inner.id.set_text(current.map_or("", ReviewRequest::short_id));
        inner.id.set_visible(current.is_some());
        inner.title.set_text(current.map_or("No Version", |r| r.subject.as_str()));

        inner.list.remove_all();
        let mut selected = None;
        for request in requests.iter().rev() {
            let out_of_date = head != Some(request.commit.as_str());
            let row = build_row(request, out_of_date, reviewed.contains(&request.id), now);
            if current.is_some_and(|c| c.id == request.id) {
                selected = Some(row.clone());
            }
            inner.list.append(&row);
        }
        inner.list.select_row(selected.as_ref());
        inner.rows.replace(requests.iter().rev().cloned().collect());
        inner.stack.set_visible_child_name(if requests.is_empty() { "empty" } else { "list" });
    }
}

/// A row of the list: the version's title above its commit, age and state.
fn build_row(request: &ReviewRequest, out_of_date: bool, reviewed: bool, now: i64) -> gtk::ListBoxRow {
    let title = util::label(&request.subject, &["history-subject"]);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_hexpand(true);
    let first_line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    first_line.append(&title);
    if reviewed {
        first_line.append(&util::label("Reviewed", &["chip", "done-chip"]));
    }
    if out_of_date {
        first_line.append(&util::label("Out of date", &["chip", "warning-chip"]));
    }
    let meta = format!("{} · {}", request.short_id(), util::relative_time(request.requested_at, now));
    let meta = util::label(&meta, &["dim-label", "caption"]);
    meta.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_margin_top(6);
    text.set_margin_bottom(6);
    text.set_margin_start(8);
    text.append(&first_line);
    text.append(&meta);
    let row = gtk::ListBoxRow::builder().child(&text).build();
    let message = if request.body.is_empty() {
        request.subject.clone()
    } else {
        format!("{}\n\n{}", request.subject, request.body)
    };
    row.set_tooltip_text(Some(&message));
    row
}
