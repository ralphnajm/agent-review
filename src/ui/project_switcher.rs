//! Header button showing the current project, with a popover of recent ones.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;

use crate::core::store::RecentProject;
use crate::ui::util;

type PathCallback = Box<dyn Fn(PathBuf)>;

#[derive(Clone)]
pub struct ProjectSwitcher(Rc<Inner>);

struct Inner {
    button: gtk::MenuButton,
    popover: gtk::Popover,
    name: gtk::Label,
    detail: gtk::Label,
    list: gtk::ListBox,
    paths: RefCell<Vec<PathBuf>>,
    on_open: RefCell<Option<PathCallback>>,
    on_forget: RefCell<Option<PathCallback>>,
}

impl ProjectSwitcher {
    pub fn new() -> Self {
        let name = util::label("No Project", &["project-name"]);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name.set_max_width_chars(28);
        let detail = util::label("", &["project-branch"]);
        detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
        detail.set_max_width_chars(24);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.append(&gtk::Image::from_icon_name("folder-symbolic"));
        content.append(&name);
        content.append(&detail);
        content.append(&gtk::Image::from_icon_name("pan-down-symbolic"));

        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        list.add_css_class("navigation-sidebar");
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(420)
            .child(&list)
            .build();

        let open = gtk::Button::builder()
            .child(&adw::ButtonContent::builder().icon_name("folder-open-symbolic").label("Open Folder…").build())
            .action_name("win.open-folder")
            .build();
        open.add_css_class("flat");

        let heading = util::label("Recent Projects", &["heading"]);
        heading.set_margin_start(10);
        heading.set_margin_top(6);
        heading.set_margin_bottom(4);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
        body.set_width_request(320);
        body.append(&heading);
        body.append(&scrolled);
        body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        body.append(&open);

        let popover = gtk::Popover::builder().child(&body).build();
        let button = gtk::MenuButton::builder()
            .child(&content)
            .popover(&popover)
            .tooltip_text("Switch Project (Ctrl+P)")
            .build();
        button.add_css_class("flat");
        button.add_css_class("project-button");

        let this = Self(Rc::new(Inner {
            button,
            popover,
            name,
            detail,
            list,
            paths: RefCell::default(),
            on_open: RefCell::default(),
            on_forget: RefCell::default(),
        }));

        let weak = Rc::downgrade(&this.0);
        this.0.list.connect_row_activated(move |_, row| {
            let Some(inner) = weak.upgrade() else { return };
            let path = usize::try_from(row.index()).ok().and_then(|i| inner.paths.borrow().get(i).cloned());
            inner.popover.popdown();
            if let (Some(path), Some(callback)) = (path, inner.on_open.borrow().as_ref()) {
                callback(path);
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

    pub fn connect_open(&self, callback: impl Fn(PathBuf) + 'static) {
        self.0.on_open.replace(Some(Box::new(callback)));
    }

    pub fn connect_forget(&self, callback: impl Fn(PathBuf) + 'static) {
        self.0.on_forget.replace(Some(Box::new(callback)));
    }

    pub fn set_current(&self, name: &str, detail: &str) {
        self.0.name.set_text(name);
        self.0.detail.set_text(&if detail.is_empty() { String::new() } else { format!("⎇ {detail}") });
        self.0.detail.set_visible(!detail.is_empty());
    }

    pub fn set_recent(&self, recent: &[RecentProject], current: Option<&Path>) {
        let inner = &self.0;
        inner.list.remove_all();
        for project in recent {
            inner.list.append(&self.build_row(&project.path, current == Some(project.path.as_path())));
        }
        inner.list.set_visible(!recent.is_empty());
        inner.paths.replace(recent.iter().map(|p| p.path.clone()).collect());
    }

    fn build_row(&self, path: &Path, is_current: bool) -> gtk::ListBoxRow {
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        let title = util::label(&name, &[]);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let subtitle = util::label(&util::tilde(path), &["dim-label", "caption"]);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        text.set_hexpand(true);
        text.append(&title);
        text.append(&subtitle);

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let check = gtk::Image::from_icon_name("object-select-symbolic");
        check.set_opacity(if is_current { 1.0 } else { 0.0 });
        content.append(&check);
        content.append(&text);

        let forget = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Remove from Recent Projects")
            .valign(gtk::Align::Center)
            .build();
        forget.add_css_class("flat");
        forget.add_css_class("circular");
        let weak = Rc::downgrade(&self.0);
        let path_buf = path.to_path_buf();
        forget.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade()
                && let Some(callback) = inner.on_forget.borrow().as_ref()
            {
                callback(path_buf.clone());
            }
        });
        content.append(&forget);

        gtk::ListBoxRow::builder().child(&content).tooltip_text(path.display().to_string()).build()
    }
}
