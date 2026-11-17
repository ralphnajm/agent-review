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
    let on = review
        .commit
        .as_ref()
        .map_or_else(|| "The uncommitted changes".to_owned(), |c| format!("{} {}", c.short_id(), c.subject));
    let when = util::relative_time(review.posted_at, now);
    let suffix = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    suffix.set_valign(gtk::Align::Start);
    let dismiss = gtk::Button::builder()
        .label("Dismiss")
        .tooltip_text("Dismiss Review: your agent no longer sees it")
        .action_name("win.dismiss-review")
        .action_target(&review.id.to_variant())
        .valign(gtk::Align::Center)
        .build();
    dismiss.add_css_class("flat");
    dismiss.add_css_class("small-button");

    if review.approved {
        let group = adw::PreferencesGroup::builder().title(on).description(format!("Approved {when}")).build();
        let status = if entry.progress.pulled_at.is_some() { Status::Resolved } else { Status::Waiting };
        let pill = status_pill(status);
        if status == Status::Resolved {
            pill.set_text("Seen");
            pill.set_tooltip_text(Some("Your agent pulled the approval"));
        }
        suffix.append(&pill);
        suffix.append(&dismiss);
        group.set_header_suffix(Some(&suffix));
        return group;
    }

    let comments = &review.comments;
    let resolved = comments.len() - entry.open().count();
    let group = adw::PreferencesGroup::builder()
        .title(on)
        .description(format!("Posted {when} · {resolved} of {} resolved", comments.len()))
        .build();
    suffix.append(&dismiss);
    group.set_header_suffix(Some(&suffix));
    for comment in comments {
        group.add(&build_comment(entry, comment, now));
    }
    group
}

/// The most lines of code a comment's preview shows.
const PREVIEW_LINES: usize = 6;

/// A comment's row: what it says first, then where it is, the code it covers
/// and the agent's replies.
fn build_comment(entry: &Entry, comment: &PostedComment, now: i64) -> gtk::ListBoxRow {
    let (name, dir) = util::split_path(&comment.path);
    let target = comment.anchor.as_ref().map_or_else(|| "File".to_owned(), LineAnchor::label);
    let location = match &comment.page {
        Some(page) => format!("Product · {page}"),
        None if comment.path.is_empty() => "Product".to_owned(),
        None => format!("{name} · {target}"),
    };
    let location = util::label(&location, &["dim-label", "caption"]);
    location.set_hexpand(true);
    location.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    if !dir.is_empty() {
        location.set_tooltip_text(Some(&comment.path));
    }
    let pill = status_pill(entry.status(comment));
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header.append(&location);
    header.append(&pill);

    let body = util::label(comment.body.trim(), &["reviews-comment-body"]);
    body.set_wrap(true);
    body.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    body.set_lines(5);
    body.set_ellipsize(gtk::pango::EllipsizeMode::End);
    body.set_tooltip_text(Some(&comment.body));

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&header);
    content.append(&body);
    if let Some(anchor) = &comment.anchor {
        content.append(&code_preview(&anchor.text));
    }
    for reply in entry.replies(comment) {
        content.append(&build_reply(reply, now));
    }
    gtk::ListBoxRow::builder().child(&content).activatable(false).build()
}

/// The first lines of the diff a comment covers, coloured like the diff.
fn code_preview(text: &str) -> gtk::Box {
    let preview = gtk::Box::new(gtk::Orientation::Vertical, 0);
    preview.add_css_class("reviews-snippet");
    let lines: Vec<&str> = text.lines().collect();
    for line in lines.iter().take(PREVIEW_LINES) {
        let class = match line.chars().next() {
            Some('+') => "added",
            Some('-') => "removed",
            _ => "context",
        };
        let label = util::label(line, &[class]);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        preview.append(&label);
    }
    if lines.len() > PREVIEW_LINES {
        let more = lines.len() - PREVIEW_LINES;
        let noun = if more == 1 { "line" } else { "lines" };
        preview.append(&util::label(&format!("{more} more {noun}"), &["more"]));
    }
    preview
}

fn build_reply(reply: &Reply, now: i64) -> gtk::Box {
    let meta = util::label(&format!("Agent · {}", util::relative_time(reply.at, now)), &["dim-label", "caption"]);
    let body = util::label(&reply.body, &["reply-body"]);
    body.set_wrap(true);
    body.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    body.set_selectable(true);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
    content.add_css_class("reviews-reply");
    content.append(&meta);
    content.append(&body);
    content
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
