//! The panel on the right with the reviews and approvals posted for the open
//! project: whether the agent has picked up and resolved each comment, and
//! its replies. Updates live while the agent works.

use adw::prelude::*;

use crate::core::queue::{Entry, PostedComment, Reply, Status};
use crate::core::review::LineAnchor;
use crate::ui::util;

#[derive(Clone)]
pub struct ReviewsPanel {
    root: gtk::Box,
    stack: gtk::Stack,
    list: gtk::Box,
}

impl ReviewsPanel {
    pub fn new() -> Self {
        let title = util::label("Posted Reviews", &["heading"]);
        title.set_hexpand(true);
        let close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Hide Posted Reviews (Ctrl+Shift+R)")
            .action_name("win.show-reviews")
            .valign(gtk::Align::Center)
            .build();
        close.add_css_class("flat");
        close.add_css_class("small-button");
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.add_css_class("reviews-header");
        header.append(&title);
        header.append(&close);

        let empty = adw::StatusPage::builder()
            .icon_name("mail-send-symbolic")
            .title("Nothing Posted Yet")
            .description(
                "Post your comments, or approve the work, for your agent to pull through the MCP server \
                 <tt>agent-review --mcp</tt>. You follow its progress here.",
            )
            .vexpand(true)
            .build();
        empty.add_css_class("compact");

        let list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(6)
            .margin_bottom(18)
            .margin_start(12)
            .margin_end(12)
            .build();
        let scroller =
            gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&list).build();

        let stack = gtk::Stack::new();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&scroller, Some("list"));

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("reviews-panel");
        root.set_width_request(340);
        root.set_visible(false);
        root.append(&header);
        root.append(&stack);
        Self { root, stack, list }
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// Shows `entries` newest first.
    pub fn set_entries(&self, entries: &[Entry], now: i64) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for entry in entries.iter().rev() {
            self.list.append(&build_group(entry, now));
        }
        self.stack.set_visible_child_name(if entries.is_empty() { "empty" } else { "list" });
    }
}

fn build_group(entry: &Entry, now: i64) -> adw::PreferencesGroup {
    let review = &entry.review;
    let on = review.commit.as_ref().map(|c| format!("On {} {}", c.short_id(), c.subject));
    let when = util::relative_time(review.posted_at, now);
    if review.approved {
        let group = adw::PreferencesGroup::builder()
            .title(format!("Approved {when}"))
            .description(on.unwrap_or_else(|| "The uncommitted changes".to_owned()))
            .build();
        let status = if entry.progress.pulled_at.is_some() { Status::Resolved } else { Status::Waiting };
        let pill = status_pill(status);
        if status == Status::Resolved {
            pill.set_text("Seen");
            pill.set_tooltip_text(Some("Your agent pulled the approval"));
        }
        group.set_header_suffix(Some(&pill));
        return group;
    }

    let comments = &review.comments;
    let resolved = comments.len() - entry.open().count();
    let noun = if comments.len() == 1 { "comment" } else { "comments" };
    let on = on.map_or_else(String::new, |on| format!("{on} · "));
    let group = adw::PreferencesGroup::builder()
        .title(format!("Posted {when}"))
        .description(format!("{on}{resolved} of {} {noun} resolved", comments.len()))
        .build();
    for comment in comments {
        group.add(&build_comment(entry, comment, now));
    }
    group
}

/// A comment's row, with the agent's replies under it.
fn build_comment(entry: &Entry, comment: &PostedComment, now: i64) -> gtk::Widget {
    let (name, dir) = util::split_path(&comment.path);
    let target = comment.anchor.as_ref().map_or_else(|| "File".to_owned(), LineAnchor::label);
    let title = match &comment.page {
        Some(page) => format!("Product · {page}"),
        None if comment.path.is_empty() => "Product".to_owned(),
        None if dir.is_empty() => format!("{name} · {target}"),
        None => format!("{name} · {target} · {dir}"),
    };
    let subtitle = comment.body.lines().find(|l| !l.trim().is_empty()).unwrap_or_default();
    let pill = status_pill(entry.status(comment));
    let replies = entry.replies(comment);
    if replies.is_empty() {
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(subtitle)
            .subtitle_lines(3)
            .use_markup(false)
            .tooltip_text(&comment.body)
            .build();
        row.add_suffix(&pill);
        return row.upcast();
    }
    let row = adw::ExpanderRow::builder()
        .title(title)
        .subtitle(subtitle)
        .subtitle_lines(3)
        .use_markup(false)
        .tooltip_text(&comment.body)
        .expanded(true)
        .build();
    row.add_suffix(&pill);
    for reply in replies {
        row.add_row(&build_reply(reply, now));
    }
    row.upcast()
}

fn build_reply(reply: &Reply, now: i64) -> gtk::ListBoxRow {
    let meta = util::label(&format!("Agent · {}", util::relative_time(reply.at, now)), &["dim-label", "caption"]);
    let body = util::label(&reply.body, &["reply-body"]);
    body.set_wrap(true);
    body.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    body.set_selectable(true);
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&meta);
    content.append(&body);
    gtk::ListBoxRow::builder().child(&content).activatable(false).build()
}

fn status_pill(status: Status) -> gtk::Label {
    let (text, class, tooltip) = match status {
        Status::Waiting => ("Waiting", "waiting", "Posted; your agent has not pulled it yet"),
        Status::PickedUp => ("Picked up", "picked-up", "Your agent pulled it and has not resolved it yet"),
        Status::Resolved => ("Resolved", "done", "Your agent resolved it"),
    };
    let pill = util::label(text, &["status-pill", class]);
    pill.set_valign(gtk::Align::Center);
    pill.set_tooltip_text(Some(tooltip));
    pill
}
