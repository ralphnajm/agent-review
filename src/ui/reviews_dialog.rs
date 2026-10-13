//! The reviews posted for the open project, and whether the agent has
//! picked up and resolved their comments. Updates live while open.

use adw::prelude::*;

use crate::core::queue::{Entry, Status};
use crate::core::review::LineAnchor;
use crate::ui::util;

#[derive(Clone)]
pub struct ReviewsDialog {
    dialog: adw::Dialog,
    stack: gtk::Stack,
    list: gtk::Box,
}

impl ReviewsDialog {
    pub fn new() -> Self {
        let empty = adw::StatusPage::builder()
            .icon_name("mail-send-symbolic")
            .title("No Posted Reviews")
            .description(
                "Post Review hands your comments to your agent. To pull them, your agent needs the MCP \
                 server <tt>agent-review --mcp</tt>.",
            )
            .build();
        empty.add_css_class("compact");

        let list = gtk::Box::new(gtk::Orientation::Vertical, 18);
        let hint = util::label(
            "Your agent pulls posted reviews through the MCP server agent-review --mcp, and resolves comments \
             once it has addressed them.",
            &["dim-label", "caption"],
        );
        hint.set_wrap(true);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(12)
            .margin_bottom(18)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&list);
        content.append(&hint);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&adw::Clamp::builder().maximum_size(640).child(&content).build())
            .build();

        let stack = gtk::Stack::new();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&scroller, Some("list"));

        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&stack));
        let dialog =
            adw::Dialog::builder().title("Posted Reviews").content_width(560).content_height(620).child(&view).build();
        Self { dialog, stack, list }
    }

    pub fn present(&self, parent: &impl IsA<gtk::Widget>) {
        self.dialog.present(Some(parent));
    }

    /// Whether the dialog is on screen.
    pub fn is_open(&self) -> bool {
        self.dialog.parent().is_some()
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
    let comments = &entry.review.comments;
    let resolved = comments.len() - entry.open().count();
    let noun = if comments.len() == 1 { "comment" } else { "comments" };
    let group = adw::PreferencesGroup::builder()
        .title(format!("Posted {}", util::relative_time(entry.review.posted_at, now)))
        .description(format!("{resolved} of {} {noun} resolved", comments.len()))
        .build();

    for comment in comments {
        let (name, dir) = util::split_path(&comment.path);
        let target = comment.anchor.as_ref().map_or_else(|| "File".to_owned(), LineAnchor::label);
        let title = if dir.is_empty() { format!("{name} · {target}") } else { format!("{name} · {target} · {dir}") };
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(comment.body.lines().find(|l| !l.trim().is_empty()).unwrap_or_default())
            .subtitle_lines(2)
            .use_markup(false)
            .tooltip_text(&comment.body)
            .build();
        row.add_suffix(&status_pill(entry.status(comment)));
        group.add(&row);
    }
    group
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
