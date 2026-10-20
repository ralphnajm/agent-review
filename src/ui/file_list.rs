//! The list of changed files in the sidebar.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;

use crate::core::changes::ChangeKind;
use crate::core::review::ApprovalState;
use crate::ui::util;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileItem {
    pub path: String,
    pub kind: ChangeKind,
    pub additions: u32,
    pub deletions: u32,
    pub approval: ApprovalState,
    pub unsent_notes: usize,
}

type SelectedCallback = Box<dyn Fn(Option<String>)>;

#[derive(Clone)]
pub struct FileList(Rc<Inner>);

struct Inner {
    scrolled: gtk::ScrolledWindow,
    list: gtk::ListBox,
    items: RefCell<Vec<FileItem>>,
    filter: RefCell<String>,
    /// Set while the list is changed programmatically, to silence callbacks.
    updating: Cell<bool>,
    on_selected: RefCell<Option<SelectedCallback>>,
}

impl FileList {
    pub fn new() -> Self {
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        list.add_css_class("navigation-sidebar");
        let scrolled =
            gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&list).build();

        let this = Self(Rc::new(Inner {
            scrolled,
            list,
            items: RefCell::default(),
            filter: RefCell::default(),
            updating: Cell::new(false),
            on_selected: RefCell::default(),
        }));

        let weak = Rc::downgrade(&this.0);
        this.0.list.set_filter_func(move |row| {
            let Some(inner) = weak.upgrade() else { return true };
            let filter = inner.filter.borrow();
            let items = inner.items.borrow();
            usize::try_from(row.index())
                .ok()
                .and_then(|i| items.get(i))
                .is_none_or(|item| filter.is_empty() || item.path.to_lowercase().contains(filter.as_str()))
        });

        let weak = Rc::downgrade(&this.0);
        this.0.list.connect_row_selected(move |_, row| {
            let Some(inner) = weak.upgrade() else { return };
            if inner.updating.get() {
                return;
            }
            let path = row
                .and_then(|r| usize::try_from(r.index()).ok())
                .and_then(|i| inner.items.borrow().get(i).map(|item| item.path.clone()));
            if let Some(callback) = inner.on_selected.borrow().as_ref() {
                callback(path);
            }
        });
        this
    }

    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.0.scrolled
    }

    pub fn connect_selected(&self, callback: impl Fn(Option<String>) + 'static) {
        self.0.on_selected.replace(Some(Box::new(callback)));
    }

    /// Replaces the list contents, keeping `selected` selected.
    pub fn set_items(&self, items: Vec<FileItem>, selected: Option<&str>) {
        let inner = &self.0;
        let old = inner.items.borrow().clone();
        if old.len() == items.len() && old.iter().zip(&items).all(|(a, b)| a.path == b.path) {
            // Same files: only refresh rows whose state changed, keeping scroll and focus.
            for (index, (before, after)) in old.iter().zip(&items).enumerate() {
                if before != after {
                    self.update_row(index, after);
                }
            }
            inner.items.replace(items);
        } else {
            inner.updating.set(true);
            inner.list.remove_all();
            for item in &items {
                inner.list.append(&build_row(item));
            }
            inner.items.replace(items);
            inner.updating.set(false);
        }
        self.select(selected);
    }

    /// Updates a single file's row in place.
    pub fn update_item(&self, item: FileItem) {
        let index = self.0.items.borrow().iter().position(|i| i.path == item.path);
        if let Some(index) = index {
            self.update_row(index, &item);
            self.0.items.borrow_mut()[index] = item;
        }
    }

    fn update_row(&self, index: usize, item: &FileItem) {
        let Some(row) = i32::try_from(index).ok().and_then(|i| self.0.list.row_at_index(i)) else {
            return;
        };
        fill_row(&row, item);
    }

    pub fn select(&self, path: Option<&str>) {
        let inner = &self.0;
        let index = path.and_then(|p| inner.items.borrow().iter().position(|i| i.path == p));
        let row = index.and_then(|i| i32::try_from(i).ok()).and_then(|i| inner.list.row_at_index(i));
        if inner.list.selected_row() == row {
            return;
        }
        inner.updating.set(true);
        inner.list.select_row(row.as_ref());
        inner.updating.set(false);
    }

    pub fn first_path(&self) -> Option<String> {
        self.visible_paths().into_iter().next()
    }

    /// The visible file `delta` positions away from `current`.
    pub fn neighbor(&self, current: Option<&str>, delta: isize) -> Option<String> {
        let paths = self.visible_paths();
        let Some(position) = current.and_then(|c| paths.iter().position(|p| p == c)) else {
            return paths.into_iter().next();
        };
        let target = position.checked_add_signed(delta)?;
        paths.get(target).cloned()
    }

    /// The next file after `current` that is not approved, wrapping around.
    pub fn next_unreviewed(&self, current: Option<&str>) -> Option<String> {
        let items = self.0.items.borrow();
        let filter = self.0.filter.borrow();
        let start = current.and_then(|c| items.iter().position(|i| i.path == c)).map_or(0, |p| p + 1);
        (0..items.len())
            .map(|offset| &items[(start + offset) % items.len()])
            .find(|item| {
                item.approval != ApprovalState::Approved
                    && Some(item.path.as_str()) != current
                    && (filter.is_empty() || item.path.to_lowercase().contains(filter.as_str()))
            })
            .map(|item| item.path.clone())
    }

    fn visible_paths(&self) -> Vec<String> {
        let filter = self.0.filter.borrow();
        self.0
            .items
            .borrow()
            .iter()
            .filter(|i| filter.is_empty() || i.path.to_lowercase().contains(filter.as_str()))
            .map(|i| i.path.clone())
            .collect()
    }

    pub fn set_filter(&self, text: &str) {
        self.0.filter.replace(text.trim().to_lowercase());
        self.0.list.invalidate_filter();
    }
}

fn build_row(item: &FileItem) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("file-row");
    fill_row(&row, item);
    row
}

fn fill_row(row: &gtk::ListBoxRow, item: &FileItem) {
    let (name, dir) = util::split_path(&item.path);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    let badge = gtk::Label::new(Some(item.kind.letter()));
    badge.add_css_class("status-letter");
    badge.add_css_class(item.kind.css_class());
    content.append(&badge);

    let name_label = util::label(name, &["file-name"]);
    name_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    content.append(&name_label);
    let dir_label = util::label(dir, &["file-dir"]);
    dir_label.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    dir_label.set_hexpand(true);
    content.append(&dir_label);

    if item.unsent_notes > 0 {
        let count = gtk::Label::new(Some(&item.unsent_notes.to_string()));
        count.add_css_class("comment-count");
        count.set_valign(gtk::Align::Center);
        count.set_tooltip_text(Some(&match item.unsent_notes {
            1 => "1 comment not sent yet".to_owned(),
            n => format!("{n} comments not sent yet"),
        }));
        content.append(&count);
    }
    if item.additions > 0 {
        content.append(&util::label(&format!("+{}", item.additions), &["stat-added"]));
    }
    if item.deletions > 0 {
        content.append(&util::label(&format!("−{}", item.deletions), &["stat-removed"]));
    }

    let approval = gtk::Image::new();
    approval.set_pixel_size(14);
    match item.approval {
        ApprovalState::Approved => {
            approval.set_icon_name(Some("object-select-symbolic"));
            approval.add_css_class("success");
            approval.set_tooltip_text(Some("Approved"));
        }
        ApprovalState::Stale => {
            approval.set_icon_name(Some("dialog-warning-symbolic"));
            approval.add_css_class("warning");
            approval.set_tooltip_text(Some("Changed since you approved it"));
        }
        ApprovalState::Unreviewed => approval.set_opacity(0.0),
    }
    content.append(&approval);

    if item.approval == ApprovalState::Approved {
        row.add_css_class("approved");
    } else {
        row.remove_css_class("approved");
    }
    row.set_tooltip_text(Some(&format!("{}\n{}", item.path, item.kind.label())));
    row.set_child(Some(&content));
}
