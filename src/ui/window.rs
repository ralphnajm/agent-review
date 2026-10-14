//! The main window: wires the widgets to the review state.
//!
//! Layout: one top bar holding the project switcher on the left and the
//! review state and actions on the right; a resizable file panel; and the
//! review panel, with every file's diff and comments inline, filling the rest.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gio, glib};

use crate::config;
use crate::core::diff::FileDiff;
use crate::core::feedback::{self, Feedback, FileInfo, Options};
use crate::core::git::{Repo, Snapshot};
use crate::core::queue::{self, Queue};
use crate::core::review::{ApprovalState, Author, LineAnchor, ReviewState};
use crate::core::status::{DiffMode, StatusEntry};
use crate::core::store::{self, Settings};
use crate::core::update;
use crate::core::{self as app_core, watcher};
use crate::ui::file_list::{FileItem, FileList};
use crate::ui::project_switcher::ProjectSwitcher;
use crate::ui::review_panel::{FileData, ReviewPanel};
use crate::ui::reviews_dialog::ReviewsDialog;
use crate::ui::util;

/// Approvals of files without a diff (e.g. type changes git can't show).
const NO_FINGERPRINT: &str = "-";

const MODES: [(DiffMode, &str, &str); 3] = [
    (DiffMode::All, "All", "All uncommitted changes (Alt+1)"),
    (DiffMode::Staged, "Staged", "Staged changes (Alt+2)"),
    (DiffMode::Unstaged, "Unstaged", "Unstaged and untracked changes (Alt+3)"),
];

struct Widgets {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    paned: gtk::Paned,
    sidebar: gtk::Box,
    switcher: ProjectSwitcher,
    mode_counts: Vec<(DiffMode, gtk::Label)>,
    search: gtk::SearchEntry,
    files: FileList,
    sidebar_stack: gtk::Stack,
    content_stack: gtk::Stack,
    review: ReviewPanel,
    review_actions: gtk::Box,
    progress: gtk::Label,
    post_count: gtk::Label,
    reviews_dialog: ReviewsDialog,
    error_page: adw::StatusPage,
    clean_page: adw::StatusPage,
}

struct Project {
    repo: Repo,
    /// Who the comments are written by, from git; unknown if it isn't set.
    author: Option<Author>,
    snapshot: Snapshot,
    /// `HEAD` → worktree fingerprints by path, for approvals.
    fingerprints: HashMap<String, String>,
    loaded: bool,
    review: ReviewState,
    /// Reviews posted for the agent to pull, and its progress on them.
    queue: Queue,
    posted: Vec<queue::Entry>,
    _watch: Option<watcher::WatchHandle>,
    _queue_watch: Option<watcher::WatchHandle>,
}

impl Project {
    fn fingerprint(&self, path: &str) -> &str {
        self.fingerprints.get(path).map_or(NO_FINGERPRINT, String::as_str)
    }

    fn approval(&self, path: &str) -> ApprovalState {
        self.review.approval(path, Some(self.fingerprint(path)))
    }

    fn visible_entries(&self, mode: DiffMode) -> impl Iterator<Item = &StatusEntry> {
        self.snapshot.status.entries.iter().filter(move |e| e.visible_in(mode))
    }

    fn save_review(&self) {
        if let Err(err) = store::save_review(&self.repo.path(), &self.review) {
            eprintln!("agent-review: could not save review: {err}");
        }
    }
}

#[derive(Default)]
struct State {
    settings: Settings,
    project: Option<Project>,
    error: Option<String>,
    /// The file being read: picked in the file list, or the one at the top of the review panel.
    selected: Option<String>,
    /// Bumped whenever the project changes, to drop stale async results.
    generation: u64,
    refreshing: bool,
    refresh_queued: bool,
}

#[derive(Clone)]
pub struct Window(Rc<Inner>);

struct Inner {
    w: Widgets,
    state: RefCell<State>,
}

impl Window {
    pub fn new(app: &adw::Application) -> Self {
        let settings = Settings::load();
        let w = build_widgets(app, &settings);
        let this = Self(Rc::new(Inner { w, state: RefCell::new(State { settings, ..State::default() }) }));
        this.setup_actions(app);
        this.connect_signals();
        this.render();
        let last = this.0.state.borrow().settings.last_project.clone();
        if let Some(path) = last.filter(|p| p.is_dir()) {
            this.open_project(path);
        }
        this
    }

    pub fn present(&self) {
        self.0.w.window.present();
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    fn toast(&self, title: &str) {
        self.0.w.toasts.add_toast(adw::Toast::new(title));
    }

    fn save_settings(&self) {
        if let Err(err) = self.0.state.borrow().settings.save() {
            eprintln!("agent-review: could not save settings: {err}");
        }
    }

    fn mode(&self) -> DiffMode {
        self.0.state.borrow().settings.diff_mode
    }

    // ----- Actions -------------------------------------------------------

    fn add_action(&self, name: &str, activate: impl Fn(&Self) + 'static) {
        let action = gio::SimpleAction::new(name, None);
        let weak = self.weak();
        action.connect_activate(move |_, _| {
            if let Some(inner) = weak.upgrade() {
                activate(&Self(inner));
            }
        });
        self.0.w.window.add_action(&action);
    }

    fn set_action_enabled(&self, name: &str, enabled: bool) {
        if let Some(action) = self.0.w.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
            action.set_enabled(enabled);
        }
    }

    fn setup_actions(&self, app: &adw::Application) {
        self.add_action("open-folder", Self::choose_folder);
        self.add_action("switch-project", |w| w.0.w.switcher.popup());
        self.add_action("refresh", Self::refresh);
        self.add_action("next-file", |w| w.step(1));
        self.add_action("prev-file", |w| w.step(-1));
        self.add_action("reviewed", |w| {
            let selected = w.0.state.borrow().selected.clone();
            if let Some(path) = selected {
                w.toggle_approve(&path, true);
            }
        });
        self.add_action("comment", |w| w.open_composer(true));
        self.add_action("comment-file", |w| w.open_composer(false));
        self.add_action("find", |w| w.0.w.review.start_search());
        self.add_action("post-review", Self::post_review);
        self.add_action("copy-feedback", Self::copy_feedback);
        self.add_action("show-reviews", Self::show_reviews);
        self.add_action("focus-search", |w| {
            w.0.w.sidebar.set_visible(true);
            w.0.w.search.grab_focus();
        });
        self.add_action("toggle-sidebar", |w| {
            let sidebar = &w.0.w.sidebar;
            sidebar.set_visible(!sidebar.is_visible());
        });
        self.add_action("clear-review", Self::confirm_clear_review);
        self.add_action("self-update", Self::self_update);

        let mode =
            gio::SimpleAction::new_stateful("diff-mode", Some(glib::VariantTy::STRING), &self.mode().id().to_variant());
        let weak = self.weak();
        mode.connect_activate(move |action, parameter| {
            let Some(id) = parameter.and_then(glib::Variant::str) else { return };
            let (Some(inner), Some(mode)) = (weak.upgrade(), DiffMode::from_id(id)) else { return };
            action.set_state(&id.to_variant());
            let this = Self(inner);
            if this.mode() != mode {
                this.0.state.borrow_mut().settings.diff_mode = mode;
                this.save_settings();
                this.0.w.review.clear();
                this.render();
                this.refresh();
            }
        });
        self.0.w.window.add_action(&mode);

        let side_by_side = self.0.state.borrow().settings.side_by_side;
        self.0.w.review.set_split(side_by_side);
        let split = gio::SimpleAction::new_stateful("side-by-side", None, &side_by_side.to_variant());
        let weak = self.weak();
        split.connect_activate(move |action, _| {
            let Some(inner) = weak.upgrade() else { return };
            let side_by_side = !action.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            action.set_state(&side_by_side.to_variant());
            let this = Self(inner);
            this.0.state.borrow_mut().settings.side_by_side = side_by_side;
            this.save_settings();
            this.0.w.review.set_split(side_by_side);
        });
        self.0.w.window.add_action(&split);

        let accels: &[(&str, &[&str])] = &[
            ("win.open-folder", &["<Control>o"]),
            ("win.switch-project", &["<Control>p"]),
            ("win.refresh", &["<Control>r", "F5"]),
            ("win.next-file", &["<Alt>Down", "<Control>Page_Down"]),
            ("win.prev-file", &["<Alt>Up", "<Control>Page_Up"]),
            ("win.reviewed", &["<Control><Shift>a"]),
            ("win.comment", &["<Control>m"]),
            ("win.post-review", &["<Control><Shift>p"]),
            ("win.copy-feedback", &["<Control><Shift>c"]),
            ("win.show-reviews", &["<Control><Shift>r"]),
            ("win.find", &["<Control>f"]),
            ("win.focus-search", &["<Control><Shift>f"]),
            ("win.toggle-sidebar", &["<Control>b", "F9"]),
            ("win.diff-mode::all", &["<Alt>1"]),
            ("win.diff-mode::staged", &["<Alt>2"]),
            ("win.diff-mode::unstaged", &["<Alt>3"]),
            ("window.close", &["<Control>w"]),
        ];
        for (action, keys) in accels {
            app.set_accels_for_action(action, keys);
        }
    }

    fn update_actions(&self) {
        let (has_project, has_file) = {
            let state = self.0.state.borrow();
            (state.project.is_some(), state.project.is_some() && state.selected.is_some())
        };
        for name in [
            "refresh",
            "next-file",
            "prev-file",
            "post-review",
            "copy-feedback",
            "show-reviews",
            "clear-review",
            "focus-search",
        ] {
            self.set_action_enabled(name, has_project);
        }
        for name in ["reviewed", "comment", "comment-file", "find"] {
            self.set_action_enabled(name, has_file);
        }
        self.0.w.review_actions.set_visible(has_project);
    }

    // ----- Signals -------------------------------------------------------

    fn connect_signals(&self) {
        let w = &self.0.w;

        let weak = self.weak();
        w.files.connect_selected(move |path| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).select(path, true);
            }
        });

        let weak = self.weak();
        w.search.connect_search_changed(move |entry| {
            if let Some(inner) = weak.upgrade() {
                inner.w.files.set_filter(&entry.text());
                inner.w.review.set_filter(&entry.text());
                Self(inner).render_content();
            }
        });
        let weak = self.weak();
        w.search.connect_activate(move |_| {
            if let Some(inner) = weak.upgrade() {
                let this = Self(inner);
                let first = this.0.w.files.first_path();
                this.select(first, false);
            }
        });

        let weak = self.weak();
        w.switcher.connect_open(move |path| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).open_project(path);
            }
        });
        let weak = self.weak();
        w.switcher.connect_forget(move |path| {
            if let Some(inner) = weak.upgrade() {
                let this = Self(inner);
                this.0.state.borrow_mut().settings.forget_project(&path);
                this.save_settings();
                this.render_switcher();
            }
        });

        let weak = self.weak();
        w.review.connect_add(move |path, anchor, body| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).add_note(path, anchor, &body);
            }
        });
        let weak = self.weak();
        w.review.connect_delete(move |path, id| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).delete_note(path, id);
            }
        });
        let weak = self.weak();
        w.review.connect_reviewed(move |path| {
            if let Some(inner) = weak.upgrade() {
                // The file collapses in place, which already brings the next one up.
                Self(inner).toggle_approve(path, false);
            }
        });
        let weak = self.weak();
        w.review.connect_current(move |path| {
            // The panel can report this while a render still borrows the state.
            let weak = weak.clone();
            let path = path.to_owned();
            glib::idle_add_local_once(move || {
                if let Some(inner) = weak.upgrade() {
                    Self(inner).follow(&path);
                }
            });
        });
        let weak = self.weak();
        w.review.connect_load(move |path, reply| {
            let Some(inner) = weak.upgrade() else { return reply(None) };
            let Some(repo) = inner.state.borrow().project.as_ref().map(|p| p.repo.clone()) else {
                return reply(None);
            };
            let (mode, path) = (Self(inner).mode(), path.to_owned());
            glib::spawn_future_local(async move {
                let text = gio::spawn_blocking(move || repo.new_side_text(&path, mode)).await;
                reply(text.ok().and_then(Result::ok).flatten());
            });
        });

        // Catch changes made while the window was in the background.
        let weak = self.weak();
        w.window.connect_is_active_notify(move |window| {
            if window.is_active()
                && let Some(inner) = weak.upgrade()
            {
                Self(inner).refresh();
            }
        });

        let weak = self.weak();
        w.window.connect_close_request(move |window| {
            if let Some(inner) = weak.upgrade() {
                let this = Self(inner);
                {
                    let mut state = this.0.state.borrow_mut();
                    let (width, height) = window.default_size();
                    state.settings.window = store::WindowGeometry { width, height, maximized: window.is_maximized() };
                    state.settings.sidebar_width = this.0.w.paned.position();
                    state.settings.show_sidebar = this.0.w.sidebar.is_visible();
                }
                this.save_settings();
            }
            glib::Propagation::Proceed
        });
    }

    // ----- Projects ------------------------------------------------------

    fn choose_folder(&self) {
        let dialog = gtk::FileDialog::builder().title("Open Project Folder").modal(true).build();
        let weak = self.weak();
        let window = self.0.w.window.clone();
        glib::spawn_future_local(async move {
            if let Ok(folder) = dialog.select_folder_future(Some(&window)).await
                && let (Some(path), Some(inner)) = (folder.path(), weak.upgrade())
            {
                Self(inner).open_project(path);
            }
        });
    }

    pub fn open_project(&self, path: PathBuf) {
        let weak = self.weak();
        glib::spawn_future_local(async move {
            let lookup_path = path.clone();
            let result = gio::spawn_blocking(move || Repo::discover(&lookup_path)).await;
            let Some(inner) = weak.upgrade() else { return };
            let this = Self(inner);
            match result {
                Ok(Ok(repo)) => this.set_project(repo),
                Ok(Err(err)) => this.set_error(format!("{err:#}")),
                Err(_) => this.set_error("Opening the folder failed unexpectedly".to_owned()),
            }
        });
    }

    fn set_project(&self, repo: Repo) {
        let path = repo.path();
        let watch = self.start_watching(&repo);
        let queue = Queue::for_repo(&repo.root);
        let queue_watch = self.start_watching_queue(&queue);
        let author = repo.author();
        {
            let mut state = self.0.state.borrow_mut();
            state.generation += 1;
            state.error = None;
            state.selected = None;
            state.settings.touch_project(&path, app_core::now());
            state.project = Some(Project {
                author: author.clone(),
                review: store::load_review(&path),
                posted: queue.load(),
                queue,
                _queue_watch: Some(queue_watch),
                repo,
                snapshot: Snapshot::default(),
                fingerprints: HashMap::new(),
                loaded: false,
                _watch: Some(watch),
            });
        }
        self.save_settings();
        self.0.w.search.set_text("");
        self.0.w.review.clear();
        self.0.w.review.set_author(author.as_ref());
        self.render();
        self.refresh();
    }

    fn set_error(&self, message: String) {
        {
            let mut state = self.0.state.borrow_mut();
            state.generation += 1;
            state.project = None;
            state.selected = None;
            state.error = Some(message);
        }
        self.0.w.review.clear();
        self.render();
    }

    fn start_watching(&self, repo: &Repo) -> watcher::WatchHandle {
        let (sender, receiver) = async_channel::bounded::<()>(1);
        let handle = watcher::watch(repo.clone(), move || {
            // A full channel already has a refresh pending.
            let _ = sender.try_send(());
        });
        let weak = self.weak();
        glib::spawn_future_local(async move {
            while receiver.recv().await.is_ok() {
                let Some(inner) = weak.upgrade() else { break };
                Self(inner).refresh();
            }
        });
        handle
    }

    /// Reloads the posted reviews whenever the agent pulls or resolves them.
    fn start_watching_queue(&self, queue: &Queue) -> watcher::WatchHandle {
        let (sender, receiver) = async_channel::bounded::<()>(1);
        let handle = watcher::watch_dir(queue.dir().to_path_buf(), move || {
            let _ = sender.try_send(());
        });
        let weak = self.weak();
        glib::spawn_future_local(async move {
            while receiver.recv().await.is_ok() {
                let Some(inner) = weak.upgrade() else { break };
                Self(inner).reload_posted();
            }
        });
        handle
    }

    fn reload_posted(&self) {
        if let Some(project) = self.0.state.borrow_mut().project.as_mut() {
            project.posted = project.queue.load();
        }
        self.render_toolbar();
        if self.0.w.reviews_dialog.is_open() {
            self.update_reviews_dialog();
        }
    }

    // ----- Refreshing ----------------------------------------------------

    pub fn refresh(&self) {
        let (repo, mode, generation) = {
            let mut state = self.0.state.borrow_mut();
            let Some(project) = &state.project else { return };
            let repo = project.repo.clone();
            if state.refreshing {
                state.refresh_queued = true;
                return;
            }
            state.refreshing = true;
            (repo, state.settings.diff_mode, state.generation)
        };

        let weak = self.weak();
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || {
                repo.snapshot(mode).map(|snapshot| {
                    let fingerprints =
                        snapshot.all.iter().map(|(path, diff)| (path.clone(), diff.fingerprint())).collect();
                    (snapshot, fingerprints)
                })
            })
            .await;
            let Some(inner) = weak.upgrade() else { return };
            let this = Self(inner);
            let queued = {
                let mut state = this.0.state.borrow_mut();
                state.refreshing = false;
                std::mem::take(&mut state.refresh_queued)
            };
            match result {
                Ok(Ok((snapshot, fingerprints))) => this.apply_snapshot(generation, mode, snapshot, fingerprints),
                Ok(Err(err)) => {
                    if this.0.state.borrow().generation == generation {
                        this.toast(&format!("Refresh failed: {err:#}"));
                    }
                }
                Err(_) => this.toast("Refresh failed unexpectedly"),
            }
            if queued {
                this.refresh();
            }
        });
    }

    fn apply_snapshot(
        &self,
        generation: u64,
        mode: DiffMode,
        snapshot: Snapshot,
        fingerprints: HashMap<String, String>,
    ) {
        {
            let mut state = self.0.state.borrow_mut();
            if state.generation != generation || state.settings.diff_mode != mode {
                drop(state);
                // The project or the mode changed meanwhile: this result is stale.
                self.refresh();
                return;
            }
            let Some(project) = state.project.as_mut() else { return };
            project.snapshot = snapshot;
            project.fingerprints = fingerprints;
            project.loaded = true;
        }
        self.render();
    }

    // ----- Rendering -----------------------------------------------------

    fn render(&self) {
        self.render_switcher();
        self.render_list();
        self.render_content();
        self.render_toolbar();
        self.update_actions();
    }

    fn render_switcher(&self) {
        let state = self.0.state.borrow();
        let current = state.project.as_ref().map(|p| p.repo.path());
        match &state.project {
            Some(project) => {
                let branch = match &project.snapshot.status.branch {
                    Some(branch) => branch.clone(),
                    None if project.loaded => "detached".to_owned(),
                    None => String::new(),
                };
                self.0.w.switcher.set_current(&project.repo.name(), &branch);
            }
            None => self.0.w.switcher.set_current("Open a Project", ""),
        }
        self.0.w.switcher.set_recent(&state.settings.recent, current.as_deref());
    }

    fn file_items(&self) -> Vec<FileItem> {
        let state = self.0.state.borrow();
        let Some(project) = &state.project else { return Vec::new() };
        let mode = state.settings.diff_mode;
        let diffs = project.snapshot.diffs();
        project
            .visible_entries(mode)
            .map(|entry| {
                let diff = diffs.get(&entry.path);
                FileItem {
                    path: entry.path.clone(),
                    kind: entry.kind_in(mode),
                    detail: staging_detail(entry),
                    additions: diff.map_or(0, |d| d.additions),
                    deletions: diff.map_or(0, |d| d.deletions),
                    approval: project.approval(&entry.path),
                    unsent_notes: project.review.unsent_notes(&entry.path),
                }
            })
            .collect()
    }

    fn render_list(&self) {
        let items = self.file_items();
        {
            let state = self.0.state.borrow();
            self.0.w.sidebar_stack.set_visible_child_name(if state.project.is_some() { "files" } else { "none" });
            for (mode, label) in &self.0.w.mode_counts {
                let count = state.project.as_ref().filter(|p| p.loaded).map(|p| p.visible_entries(*mode).count());
                label.set_text(&count.map_or_else(String::new, |c| c.to_string()));
            }
        }

        // Keep a file selected so the main area is never empty.
        let mut selected = self.0.state.borrow().selected.clone();
        if selected.as_ref().is_some_and(|s| !items.iter().any(|i| &i.path == s)) {
            selected = None;
        }
        if selected.is_none() {
            selected = items.first().map(|i| i.path.clone());
        }
        self.0.state.borrow_mut().selected.clone_from(&selected);
        self.0.w.files.set_items(items, selected.as_deref());
    }

    /// The review state and actions on the right of the top bar.
    fn render_toolbar(&self) {
        let w = &self.0.w;
        let state = self.0.state.borrow();
        let Some(project) = state.project.as_ref() else { return };
        let mode = state.settings.diff_mode;

        let paths: Vec<&str> = project.visible_entries(mode).map(|e| e.path.as_str()).collect();
        let approved = paths.iter().filter(|p| project.approval(p) == ApprovalState::Approved).count();
        w.progress.set_text(&format!("{approved}/{} reviewed", paths.len()));
        w.progress.set_visible(project.loaded && !paths.is_empty());
        if approved == paths.len() && !paths.is_empty() {
            w.progress.add_css_class("done");
        } else {
            w.progress.remove_css_class("done");
        }

        let unsent: usize = project.review.files.keys().map(|p| project.review.unsent_notes(p)).sum();
        w.post_count.set_text(&unsent.to_string());
        w.post_count.set_visible(unsent > 0);
    }

    fn render_content(&self) {
        let w = &self.0.w;
        let state = self.0.state.borrow();
        let mode = state.settings.diff_mode;

        let Some(project) = &state.project else {
            if let Some(error) = &state.error {
                w.error_page.set_description(Some(&glib::markup_escape_text(error)));
                w.content_stack.set_visible_child_name("error");
            } else {
                w.content_stack.set_visible_child_name("welcome");
            }
            return;
        };
        if !project.loaded {
            w.content_stack.set_visible_child_name("loading");
            return;
        }
        if project.visible_entries(mode).next().is_none() {
            let (title, description) = match mode {
                DiffMode::All => ("No Changes", "Nothing to review yet. Changes appear here as your agent works."),
                DiffMode::Staged => ("No Staged Changes", "Nothing has been staged yet."),
                DiffMode::Unstaged => ("No Unstaged Changes", "Every change is staged."),
            };
            w.clean_page.set_title(title);
            w.clean_page.set_description(Some(description));
            w.content_stack.set_visible_child_name("clean");
            return;
        }

        let diffs = project.snapshot.diffs();
        let files = project
            .visible_entries(mode)
            .map(|entry| {
                let path = entry.path.as_str();
                let diff = diffs.get(path);
                FileData {
                    path,
                    kind: entry.kind_in(mode),
                    additions: diff.map_or(0, |d| d.additions),
                    deletions: diff.map_or(0, |d| d.deletions),
                    meta: file_meta(entry, diff, mode),
                    approval: project.approval(path),
                    diff: diff.map_or_else(|| Cow::Owned(placeholder_diff(path)), Cow::Borrowed),
                    notes: project.review.notes(path),
                }
            })
            .collect();
        w.review.set_files(files);
        w.content_stack.set_visible_child_name(if w.files.first_path().is_some() { "diff" } else { "select" });
    }

    /// Re-renders everything that depends on the current file's review state.
    fn render_review_change(&self, path: &str) {
        let item = self.file_items().into_iter().find(|i| i.path == path);
        if let Some(item) = item {
            self.0.w.files.update_item(item);
        }
        self.render_content();
        self.render_toolbar();
    }

    // ----- Navigation ----------------------------------------------------

    /// Scrolls the review panel to a file.
    fn select(&self, path: Option<String>, from_list: bool) {
        let Some(path) = path else { return };
        if !from_list {
            self.0.w.files.select(Some(&path));
        }
        self.0.state.borrow_mut().selected = Some(path.clone());
        self.0.w.review.scroll_to(&path);
        self.update_actions();
    }

    /// Selects the file the review panel moved to.
    fn follow(&self, path: &str) {
        self.0.state.borrow_mut().selected = Some(path.to_owned());
        self.0.w.files.select(Some(path));
        self.update_actions();
    }

    fn step(&self, delta: isize) {
        let current = self.0.state.borrow().selected.clone();
        if let Some(path) = self.0.w.files.neighbor(current.as_deref(), delta) {
            self.select(Some(path), false);
        }
    }

    // ----- Review --------------------------------------------------------

    /// Approves a file, collapsing it and, with `advance`, moving on to the next
    /// one, or takes the approval back.
    fn toggle_approve(&self, path: &str, advance: bool) {
        let approved_now = {
            let mut state = self.0.state.borrow_mut();
            let Some(project) = state.project.as_mut() else { return };
            let approve = project.approval(path) != ApprovalState::Approved;
            let fingerprint = approve.then(|| project.fingerprint(path).to_owned());
            project.review.set_approved(path, fingerprint.as_deref(), app_core::now());
            project.save_review();
            approve
        };
        self.render_review_change(path);
        if approved_now {
            self.0.w.review.collapse(path);
            match self.0.w.files.next_unreviewed(Some(path)) {
                Some(next) if advance => self.select(Some(next), false),
                Some(_) => {}
                None => self.toast("All files reviewed"),
            }
        }
    }

    fn open_composer(&self, lines: bool) {
        let selected = self.0.state.borrow().selected.clone();
        if let Some(path) = selected {
            self.0.w.review.open_composer(&path, lines);
        }
    }

    fn add_note(&self, path: &str, anchor: Option<LineAnchor>, body: &str) {
        {
            let mut state = self.0.state.borrow_mut();
            let Some(project) = state.project.as_mut() else { return };
            let now = app_core::now();
            project.review.add_note(path, anchor, body, project.author.as_ref(), now);
            // Commenting on a file counts as reviewing it.
            if project.approval(path) != ApprovalState::Approved {
                let fingerprint = project.fingerprint(path).to_owned();
                project.review.set_approved(path, Some(&fingerprint), now);
            }
            project.save_review();
        }
        self.render_review_change(path);
    }

    fn delete_note(&self, path: &str, id: u64) {
        let note = {
            let mut state = self.0.state.borrow_mut();
            let Some(project) = state.project.as_mut() else { return };
            let note = project.review.notes(path).iter().find(|n| n.id == id).cloned();
            project.review.remove_note(path, id);
            project.save_review();
            note
        };
        self.render_review_change(path);
        let path = path.to_owned();

        let Some(note) = note else { return };
        let toast = adw::Toast::builder().title("Comment deleted").button_label("Undo").build();
        let weak = self.weak();
        toast.connect_button_clicked(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            let this = Self(inner);
            {
                let mut state = this.0.state.borrow_mut();
                let Some(project) = state.project.as_mut() else { return };
                project.review.restore_note(&path, note.clone());
                project.save_review();
            }
            this.render_review_change(&path);
        });
        self.0.w.toasts.add_toast(toast);
    }

    fn confirm_clear_review(&self) {
        let dialog = adw::AlertDialog::new(
            Some("Clear Review?"),
            Some("All approvals and comments for this project will be deleted."),
        );
        dialog.add_responses(&[("cancel", "_Cancel"), ("clear", "_Clear Review")]);
        dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = self.weak();
        dialog.connect_response(None, move |_, response| {
            if response != "clear" {
                return;
            }
            let Some(inner) = weak.upgrade() else { return };
            let this = Self(inner);
            if let Some(project) = this.0.state.borrow_mut().project.as_mut() {
                project.review.clear();
                project.save_review();
            }
            this.render();
        });
        dialog.present(Some(&self.0.w.window));
    }

    // ----- Feedback ------------------------------------------------------

    /// Feedback on the unsent comments and the reviewed files.
    fn feedback(&self) -> Option<Feedback> {
        let state = self.0.state.borrow();
        let project = state.project.as_ref()?;
        let files: Vec<FileInfo<'_>> = project
            .snapshot
            .status
            .entries
            .iter()
            .map(|e| FileInfo {
                path: &e.path,
                diff: project.snapshot.all.get(&e.path),
                approval: project.approval(&e.path),
            })
            .collect();
        Some(feedback::build(&project.review, &files, &Options::default()))
    }

    fn copy_feedback(&self) {
        let Some(feedback) = self.feedback() else { return };
        if feedback.is_empty() {
            self.toast("Nothing to copy yet: comment on or review some files first");
            return;
        }
        self.0.w.window.clipboard().set_text(&feedback.text);
        self.mark_sent(feedback.note_ids);
    }

    /// Queues the unsent comments for the agent to pull through the MCP server.
    fn post_review(&self) {
        let Some(feedback) = self.feedback() else { return };
        let ids = feedback.note_ids;
        if ids.is_empty() {
            self.toast("Nothing to post yet: comment on some lines or files first");
            return;
        }
        let posted = {
            let mut state = self.0.state.borrow_mut();
            let Some(project) = state.project.as_mut() else { return };
            // In the order of the feedback: files as listed, then comments as written.
            let comments: Vec<_> = ids
                .iter()
                .filter_map(|&id| {
                    project.review.files.iter().find_map(|(path, file)| {
                        let note = file.notes.iter().find(|n| n.id == id)?;
                        Some((id, path.clone(), note.anchor.clone(), note.body.clone()))
                    })
                })
                .collect();
            let now = app_core::now();
            let posted = project.queue.post(comments, now);
            if posted.is_ok() {
                project.review.set_sent(&ids, Some(now));
                project.save_review();
                project.posted = project.queue.load();
            }
            posted
        };
        let review = match posted {
            Ok(review) => review,
            Err(err) => {
                self.toast(&format!("Could not post the review: {err}"));
                return;
            }
        };
        self.render();

        let toast = adw::Toast::builder()
            .title("Review posted. Ask your agent to pull it")
            .button_label("Undo")
            .timeout(5)
            .build();
        let weak = self.weak();
        toast.connect_button_clicked(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            Self(inner).withdraw_review(review.id, &ids);
        });
        self.0.w.toasts.add_toast(toast);
    }

    /// Takes back a posted review, unless the agent has pulled it already.
    fn withdraw_review(&self, id: u64, note_ids: &[u64]) {
        let withdrawn = {
            let mut state = self.0.state.borrow_mut();
            let Some(project) = state.project.as_mut() else { return };
            let withdrawn = project.queue.withdraw(id);
            if let Ok(true) = withdrawn {
                project.review.set_sent(note_ids, None);
                project.save_review();
                project.posted = project.queue.load();
            }
            withdrawn
        };
        match withdrawn {
            Ok(true) => self.render(),
            Ok(false) => self.toast("Your agent has already pulled this review"),
            Err(err) => self.toast(&format!("Could not take the review back: {err}")),
        }
    }

    fn show_reviews(&self) {
        self.update_reviews_dialog();
        self.0.w.reviews_dialog.present(&self.0.w.window);
    }

    fn update_reviews_dialog(&self) {
        let state = self.0.state.borrow();
        let posted = state.project.as_ref().map_or(&[][..], |p| &p.posted);
        self.0.w.reviews_dialog.set_entries(posted, app_core::now());
    }

    /// Marks copied comments as sent and confirms with an undoable toast.
    fn mark_sent(&self, ids: Vec<u64>) {
        if let Some(project) = self.0.state.borrow_mut().project.as_mut() {
            project.review.set_sent(&ids, Some(app_core::now()));
            project.save_review();
        }
        self.render();

        let toast = adw::Toast::builder().title("Feedback copied. Paste it into your agent").timeout(5).build();
        if !ids.is_empty() {
            toast.set_button_label(Some("Mark Unsent"));
            let weak = self.weak();
            toast.connect_button_clicked(move |_| {
                let Some(inner) = weak.upgrade() else { return };
                let this = Self(inner);
                if let Some(project) = this.0.state.borrow_mut().project.as_mut() {
                    project.review.set_sent(&ids, None);
                    project.save_review();
                }
                this.render();
            });
        }
        self.0.w.toasts.add_toast(toast);
    }

    // ----- Self-update ---------------------------------------------------

    /// Runs the update script; when it updated, quits so the script can start the new version.
    fn self_update(&self) {
        self.set_action_enabled("self-update", false);
        let progress = adw::Toast::builder().title("Updating Agent Review…").timeout(0).build();
        self.0.w.toasts.add_toast(progress.clone());
        let weak = self.weak();
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(update::run).await;
            progress.dismiss();
            let Some(inner) = weak.upgrade() else { return };
            let this = Self(inner);
            this.set_action_enabled("self-update", true);
            match result {
                Ok(Ok(update::Outcome::Updated)) => {
                    if let Some(app) = this.0.w.window.application() {
                        app.quit();
                    }
                }
                Ok(Ok(update::Outcome::UpToDate)) => this.toast("Agent Review is up to date"),
                Ok(Err(err)) => this.toast(&format!("Could not update: {err}")),
                Err(_) => this.toast("The update failed unexpectedly"),
            }
        });
    }
}

/// The header details of a file after its line counts: kind of change and rename.
fn file_meta(entry: &StatusEntry, diff: Option<&FileDiff>, mode: DiffMode) -> String {
    let mut meta = vec![entry.kind_in(mode).label().to_owned()];
    if let Some(from) = diff.and_then(FileDiff::renamed_from) {
        meta.push(format!("from {from}"));
    }
    meta.join("  ·  ")
}

fn staging_detail(entry: &StatusEntry) -> String {
    if entry.is_untracked() {
        return "Untracked".to_owned();
    }
    match (entry.staged.is_some(), entry.unstaged.is_some()) {
        (true, true) => "Partly staged".to_owned(),
        (true, false) => "Staged".to_owned(),
        (false, true) => "Not staged".to_owned(),
        (false, false) => String::new(),
    }
}

/// Stand-in for a changed path git produced no diff for, so it can still be commented on.
fn placeholder_diff(path: &str) -> FileDiff {
    FileDiff { old_path: Some(path.to_owned()), new_path: Some(path.to_owned()), ..FileDiff::default() }
}

// ----- Widget construction ---------------------------------------------

fn build_widgets(app: &adw::Application, settings: &Settings) -> Widgets {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(config::APP_NAME)
        .default_width(settings.window.width)
        .default_height(settings.window.height)
        .maximized(settings.window.maximized)
        .width_request(520)
        .height_request(420)
        .build();
    util::use_pointer_cursor(&window);

    // ----- Top bar -----
    let sidebar_toggle = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text("Toggle File Panel (Ctrl+B)")
        .active(settings.show_sidebar)
        .build();
    sidebar_toggle.add_css_class("flat");
    let switcher = ProjectSwitcher::new();

    let progress = util::label("", &["status-pill"]);
    progress.set_valign(gtk::Align::Center);
    progress.set_tooltip_text(Some("Files approved in this view"));

    let post_count = gtk::Label::new(None);
    post_count.add_css_class("count-badge");
    post_count.set_valign(gtk::Align::Center);
    let post_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    post_content.append(&gtk::Label::new(Some("Post Review")));
    post_content.append(&post_count);
    let post_menu = gio::Menu::new();
    post_menu.append(Some("Copy Feedback Instead"), Some("win.copy-feedback"));
    let post_button = adw::SplitButton::builder()
        .child(&post_content)
        .menu_model(&post_menu)
        .tooltip_text("Post unsent comments for your agent to pull (Ctrl+Shift+P)")
        .dropdown_tooltip("More Ways to Send")
        .action_name("win.post-review")
        .build();
    post_button.add_css_class("suggested-action");
    post_button.add_css_class("toolbar-button");

    let review_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    review_actions.append(&progress);
    review_actions.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    review_actions.append(&post_button);

    let main_menu = gio::Menu::new();
    let section = gio::Menu::new();
    section.append(Some("Open Folder…"), Some("win.open-folder"));
    section.append(Some("Refresh"), Some("win.refresh"));
    main_menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append(Some("Side by Side"), Some("win.side-by-side"));
    main_menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append(Some("Posted Reviews"), Some("win.show-reviews"));
    section.append(Some("Clear Review…"), Some("win.clear-review"));
    main_menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append(Some("Keyboard Shortcuts"), Some("win.show-help-overlay"));
    section.append(Some("Self-Update"), Some("win.self-update"));
    section.append(Some("About Agent Review"), Some("app.about"));
    main_menu.append_section(None, &section);
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&main_menu)
        .primary(true)
        .tooltip_text("Main Menu")
        .build();
    menu_button.add_css_class("flat");

    let header = adw::HeaderBar::new();
    header.add_css_class("topbar");
    header.set_title_widget(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
    header.pack_start(&sidebar_toggle);
    header.pack_start(switcher.widget());
    header.pack_end(&menu_button);
    header.pack_end(&review_actions);

    // ----- File panel -----
    let tabs = gtk::Box::builder().homogeneous(true).build();
    tabs.add_css_class("mode-tabs");
    let mut mode_counts = Vec::new();
    for (mode, label, tooltip) in MODES {
        let count = util::label("", &["tab-count"]);
        let content = gtk::Box::builder().spacing(6).halign(gtk::Align::Center).build();
        content.append(&gtk::Label::new(Some(label)));
        content.append(&count);
        let button = gtk::ToggleButton::builder()
            .child(&content)
            .tooltip_text(tooltip)
            .action_name("win.diff-mode")
            .action_target(&mode.id().to_variant())
            .build();
        button.add_css_class("mode-tab");
        tabs.append(&button);
        mode_counts.push((mode, count));
    }
    let search = gtk::SearchEntry::builder().placeholder_text("Filter files (Ctrl+Shift+F)").build();
    search.add_css_class("panel-search");
    let tools = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(10)
        .margin_start(10)
        .margin_end(10)
        .margin_bottom(6)
        .build();
    tools.append(&tabs);
    tools.append(&search);

    let files = FileList::new();
    let no_project = util::label("Open a project folder to start reviewing.", &["dim-label"]);
    no_project.set_wrap(true);
    no_project.set_justify(gtk::Justification::Center);
    no_project.set_xalign(0.5);
    no_project.set_margin_start(24);
    no_project.set_margin_end(24);
    let sidebar_stack = gtk::Stack::builder().vexpand(true).build();
    sidebar_stack.add_named(files.widget(), Some("files"));
    sidebar_stack.add_named(&no_project, Some("none"));

    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar.add_css_class("file-panel");
    sidebar.set_width_request(220);
    sidebar.set_visible(settings.show_sidebar);
    sidebar.append(&tools);
    sidebar.append(&sidebar_stack);
    sidebar.bind_property("visible", &sidebar_toggle, "active").bidirectional().sync_create().build();

    // ----- Main area -----
    let review = ReviewPanel::new();

    let open_button =
        gtk::Button::builder().label("Open Folder…").halign(gtk::Align::Center).action_name("win.open-folder").build();
    open_button.add_css_class("pill");
    open_button.add_css_class("suggested-action");
    let welcome = adw::StatusPage::builder()
        .icon_name(config::APP_ID)
        .title("Review Your Agent's Changes")
        .description(
            "Open a Git repository to review its uncommitted changes, comment on lines, and send the feedback \
             back to your coding agent.",
        )
        .child(&open_button)
        .build();
    let retry = gtk::Button::builder()
        .label("Choose Another Folder…")
        .halign(gtk::Align::Center)
        .action_name("win.open-folder")
        .build();
    retry.add_css_class("pill");
    let error_page = adw::StatusPage::builder()
        .icon_name("dialog-warning-symbolic")
        .title("Can't Open This Folder")
        .child(&retry)
        .build();
    let loading = gtk::Spinner::builder()
        .spinning(true)
        .width_request(32)
        .height_request(32)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    let clean_page = adw::StatusPage::builder().icon_name("object-select-symbolic").title("No Changes").build();
    let select_page = adw::StatusPage::builder()
        .icon_name("edit-find-symbolic")
        .title("No Matching Files")
        .description("Clear the filter to see all changed files.")
        .build();

    let content_stack = gtk::Stack::builder().vexpand(true).build();
    content_stack.add_named(&welcome, Some("welcome"));
    content_stack.add_named(&error_page, Some("error"));
    content_stack.add_named(&loading, Some("loading"));
    content_stack.add_named(&clean_page, Some("clean"));
    content_stack.add_named(&select_page, Some("select"));
    content_stack.add_named(review.widget(), Some("diff"));

    let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
    main.add_css_class("main-area");
    main.append(&content_stack);

    let paned = gtk::Paned::builder()
        .start_child(&sidebar)
        .end_child(&main)
        .resize_start_child(false)
        .shrink_start_child(false)
        .shrink_end_child(false)
        .position(settings.sidebar_width)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.set_top_bar_style(adw::ToolbarStyle::Raised);
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&paned));
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&toolbar));
    window.set_content(Some(&toasts));

    // Narrow windows: icon-only actions, and the file panel tucked away.
    let narrow = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 820sp").expect("valid condition"));
    narrow.add_setter(&progress, "visible", Some(&false.to_value()));
    narrow.add_setter(&sidebar, "visible", Some(&false.to_value()));
    window.add_breakpoint(narrow);

    let shortcuts = gtk::Builder::from_string(include_str!("shortcuts.ui"));
    if let Some(overlay) = shortcuts.object::<gtk::ShortcutsWindow>("help_overlay") {
        window.set_help_overlay(Some(&overlay));
    }

    Widgets {
        window,
        toasts,
        paned,
        sidebar,
        switcher,
        mode_counts,
        search,
        files,
        sidebar_stack,
        content_stack,
        review,
        review_actions,
        progress,
        post_count,
        reviews_dialog: ReviewsDialog::new(),
        error_page,
        clean_page,
    }
}
