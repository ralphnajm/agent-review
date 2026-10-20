//! Installs the agent skill in the skills folder of the user's agent, or in
//! a folder they choose.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::core::skill::{self, State};
use crate::ui::util;

type InstallCallback = Box<dyn Fn(&Path)>;

#[derive(Clone)]
pub struct SkillDialog(Rc<Inner>);

struct Inner {
    dialog: adw::Dialog,
    toasts: adw::ToastOverlay,
    /// Each known location's install button, by folder.
    buttons: Vec<(PathBuf, gtk::Button)>,
    on_install: RefCell<Option<InstallCallback>>,
}

impl SkillDialog {
    pub fn new() -> Self {
        let intro = util::label(
            "The Agent Review skill teaches your coding agent the review workflow: when to ask you for a review, \
             and how to address your comments on the code and the product. It works through the MCP server \
             agent-review --mcp, which your agent needs too.",
            &["dim-label"],
        );
        intro.set_wrap(true);

        let group = adw::PreferencesGroup::builder().title("Install For").build();
        let mut buttons = Vec::new();
        for location in skill::locations() {
            let row = adw::ActionRow::builder()
                .title(location.title)
                .subtitle(if location.agents == location.title {
                    home_relative(&location.dir)
                } else {
                    format!("{} · {}", location.agents, home_relative(&location.dir))
                })
                .subtitle_lines(2)
                .build();
            let button = gtk::Button::builder().valign(gtk::Align::Center).build();
            row.add_suffix(&button);
            group.add(&row);
            buttons.push((location.dir, button));
        }
        let choose = adw::ActionRow::builder()
            .title("Choose Folder…")
            .subtitle("Another agent's skills folder, or a project's, such as .claude/skills or .agents/skills")
            .subtitle_lines(2)
            .activatable(true)
            .build();
        choose.add_suffix(&gtk::Image::from_icon_name("folder-open-symbolic"));
        group.add(&choose);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(12)
            .margin_bottom(18)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&intro);
        content.append(&group);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&adw::Clamp::builder().maximum_size(560).child(&content).build()));
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&view));
        let dialog = adw::Dialog::builder().title("Install Agent Skill").content_width(520).child(&toasts).build();

        let this = Self(Rc::new(Inner { dialog, toasts, buttons, on_install: RefCell::default() }));
        for (dir, button) in &this.0.buttons {
            let weak = Rc::downgrade(&this.0);
            let dir = dir.clone();
            button.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    Self(inner).install(&dir);
                }
            });
        }
        let weak = Rc::downgrade(&this.0);
        choose.connect_activated(move |_| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).choose_folder();
            }
        });
        this
    }

    /// Called with the skills folder the skill was installed in.
    pub fn connect_install(&self, callback: impl Fn(&Path) + 'static) {
        self.0.on_install.replace(Some(Box::new(callback)));
    }

    pub fn present(&self, parent: &impl IsA<gtk::Widget>) {
        self.update();
        self.0.dialog.present(Some(parent));
    }

    /// Shows whether the skill is installed, and up to date, in each location.
    fn update(&self) {
        for (dir, button) in &self.0.buttons {
            let state = skill::state(dir);
            button.set_label(match state {
                State::Missing => "Install",
                State::Outdated => "Update",
                State::Installed => "Installed",
            });
            button.set_sensitive(state != State::Installed);
            if state == State::Installed {
                button.remove_css_class("suggested-action");
                button.add_css_class("flat");
            } else {
                button.remove_css_class("flat");
                button.add_css_class("suggested-action");
            }
        }
    }

    fn install(&self, dir: &Path) {
        let message = match skill::install(dir) {
            Ok(file) => {
                if let Some(callback) = self.0.on_install.borrow().as_ref() {
                    callback(dir);
                }
                format!("Skill installed in {}", home_relative(file.parent().unwrap_or(dir)))
            }
            Err(err) => format!("Could not install the skill: {err}"),
        };
        self.0.toasts.add_toast(adw::Toast::new(&message));
        self.update();
    }

    fn choose_folder(&self) {
        let dialog = gtk::FileDialog::builder().title("Choose a Skills Folder").modal(true).build();
        let window = self.0.dialog.root().and_downcast::<gtk::Window>();
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            if let Ok(folder) = dialog.select_folder_future(window.as_ref()).await
                && let (Some(dir), Some(inner)) = (folder.path(), weak.upgrade())
            {
                Self(inner).install(&dir);
            }
        });
    }
}

/// `path` with the home folder written `~`.
fn home_relative(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home.as_deref().and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}
