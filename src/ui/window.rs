//! The main window: wires the widgets to the review state.
//!
//! Layout: one top bar holding the project switcher on the left and the
//! review state and actions on the right; a resizable file panel; and the
//! diff, with comments inline, filling the rest.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gio, glib};

use crate::config;
use crate::core::diff::FileDiff;
use crate::core::feedback::{self, FileInfo, Options};
use crate::core::git::{Repo, Snapshot};
use crate::core::review::{ApprovalState, LineAnchor, ReviewState};
use crate::core::status::{DiffMode, StatusEntry};
use crate::core::store::{self, Settings};
use crate::core::{self as app_core, watcher};
use crate::ui::diff_view::DiffView;
use crate::ui::file_list::{FileItem, FileList};
use crate::ui::project_switcher::ProjectSwitcher;
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
    file_header: gtk::Box,
    file_badge: gtk::Label,
    file_dir: gtk::Label,
    file_name: gtk::Label,
    file_meta: gtk::Label,
    stale_chip: gtk::Label,
    diff: DiffView,
    review_actions: gtk::Box,
    progress: gtk::Label,
    copy_count: gtk::Label,
    error_page: adw::StatusPage,
    clean_page: adw::StatusPage,
}

struct Project {
    repo: Repo,
    snapshot: Snapshot,
    /// `HEAD` → worktree fingerprints by path, for approvals.
    fingerprints: HashMap<String, String>,
    loaded: bool,
    review: ReviewState,
    _watch: Option<watcher::WatchHandle>,
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
        // The "Reviewed" checkbox next to the file name; its state follows the current file.
        let reviewed = gio::SimpleAction::new_stateful("reviewed", None, &false.to_variant());
        let weak = self.weak();
        reviewed.connect_activate(move |_, _| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).toggle_approve();
            }
        });
        self.0.w.window.add_action(&reviewed);
        self.add_action("comment", |w| w.0.w.diff.open_composer(true));
        self.add_action("comment-file", |w| w.0.w.diff.open_composer(false));
        self.add_action("copy-feedback", Self::copy_feedback);
        self.add_action("focus-search", |w| {
            w.0.w.sidebar.set_visible(true);
            w.0.w.search.grab_focus();
        });
        self.add_action("toggle-sidebar", |w| {
            let sidebar = &w.0.w.sidebar;
            sidebar.set_visible(!sidebar.is_visible());
        });
        self.add_action("clear-review", Self::confirm_clear_review);

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
                this.0.w.diff.invalidate();
                this.render();
                this.refresh();
            }
        });
        self.0.w.window.add_action(&mode);

        let accels: &[(&str, &[&str])] = &[
            ("win.open-folder", &["<Control>o"]),
            ("win.switch-project", &["<Control>p"]),
            ("win.refresh", &["<Control>r", "F5"]),
            ("win.next-file", &["<Alt>Down", "<Control>Page_Down"]),
            ("win.prev-file", &["<Alt>Up", "<Control>Page_Up"]),
            ("win.reviewed", &["<Control><Shift>a"]),
            ("win.comment", &["<Control>m"]),
            ("win.copy-feedback", &["<Control><Shift>c"]),
            ("win.focus-search", &["<Control>f"]),
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
        for name in ["refresh", "next-file", "prev-file", "copy-feedback", "clear-review", "focus-search"] {
            self.set_action_enabled(name, has_project);
        }
        for name in ["reviewed", "comment", "comment-file"] {
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
        w.diff.connect_add(move |anchor, body| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).add_note(anchor, &body);
            }
        });
        let weak = self.weak();
        w.diff.connect_delete(move |id| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).delete_note(id);
            }
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
        {
            let mut state = self.0.state.borrow_mut();
            state.generation += 1;
            state.error = None;
            state.selected = None;
            state.settings.touch_project(&path, app_core::now());
            state.project = Some(Project {
                review: store::load_review(&path),
                repo,
                snapshot: Snapshot::default(),
                fingerprints: HashMap::new(),
                loaded: false,
                _watch: Some(watch),
            });
        }
        self.save_settings();
        self.0.w.search.set_text("");
        self.0.w.diff.clear();
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
        w.copy_count.set_text(&unsent.to_string());
        w.copy_count.set_visible(unsent > 0);

        let reviewed = state.selected.as_deref().is_some_and(|p| project.approval(p) == ApprovalState::Approved);
        if let Some(action) = w.window.lookup_action("reviewed").and_downcast::<gio::SimpleAction>() {
            action.set_state(&reviewed.to_variant());
        }
    }

    fn render_content(&self) {
        let w = &self.0.w;
        let state = self.0.state.borrow();
        let mode = state.settings.diff_mode;
        w.file_header.set_visible(false);

        let Some(project) = &state.project else {
            if let Some(error) = &state.error {
                w.error_page.set_description(Some(&glib::markup_escape_text(error)));
                w.content_stack.set_visible_child_name("error");
            } else {
                w.content_stack.set_visible_child_name("welcome");
            }
            return;
        };

        let Some(path) = state.selected.as_deref() else {
            if !project.loaded {
                w.content_stack.set_visible_child_name("loading");
            } else if project.visible_entries(mode).next().is_none() {
                let (title, description) = match mode {
                    DiffMode::All => ("No Changes", "Nothing to review yet. Changes appear here as Claude works."),
                    DiffMode::Staged => ("No Staged Changes", "Nothing has been staged yet."),
                    DiffMode::Unstaged => ("No Unstaged Changes", "Every change is staged."),
                };
                w.clean_page.set_title(title);
                w.clean_page.set_description(Some(description));
                w.content_stack.set_visible_child_name("clean");
            } else {
                w.content_stack.set_visible_child_name("select");
            }
            return;
        };

        let (name, dir) = util::split_path(path);
        let entry = project.snapshot.status.entries.iter().find(|e| e.path == path);
        let diff = project.snapshot.diffs().get(path);
        let kind = entry.map(|e| e.kind_in(mode));
        w.file_badge.set_text(kind.map_or("", |k| k.letter()));
        w.file_badge.set_css_classes(&["status-letter", kind.map_or("modified", |k| k.css_class())]);
        w.file_dir.set_text(&if dir.is_empty() { String::new() } else { format!("{dir}/") });
        w.file_name.set_text(name);
        let mut meta = Vec::new();
        if let Some(diff) = diff.filter(|d| d.additions + d.deletions > 0) {
            let counts: Vec<String> = [("+", diff.additions), ("−", diff.deletions)]
                .iter()
                .filter(|(_, n)| *n > 0)
                .map(|(sign, n)| format!("{sign}{n}"))
                .collect();
            meta.push(counts.join(" "));
        }
        if let Some(entry) = entry {
            meta.push(entry.kind_in(mode).label().to_owned());
            let detail = staging_detail(entry);
            if !detail.is_empty() {
                meta.push(detail);
            }
        }
        if let Some(from) = diff.and_then(FileDiff::renamed_from) {
            meta.push(format!("from {from}"));
        }
        w.file_meta.set_text(&meta.join("  ·  "));
        w.stale_chip.set_visible(project.approval(path) == ApprovalState::Stale);
        w.file_header.set_visible(true);

        match diff {
            Some(diff) => w.diff.show(diff, project.review.notes(path)),
            None => w.diff.show(&placeholder_diff(path), project.review.notes(path)),
        }
        w.content_stack.set_visible_child_name("diff");
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

    fn select(&self, path: Option<String>, from_list: bool) {
        if self.0.state.borrow().selected == path {
            return;
        }
        if !from_list {
            self.0.w.files.select(path.as_deref());
        }
        self.0.state.borrow_mut().selected = path;
        self.render_content();
        self.render_toolbar();
        self.update_actions();
    }

    fn step(&self, delta: isize) {
        let current = self.0.state.borrow().selected.clone();
        if let Some(path) = self.0.w.files.neighbor(current.as_deref(), delta) {
            self.select(Some(path), false);
        }
    }

    // ----- Review --------------------------------------------------------

    fn toggle_approve(&self) {
        let (path, approved_now) = {
            let mut state = self.0.state.borrow_mut();
            let Some(path) = state.selected.clone() else { return };
            let Some(project) = state.project.as_mut() else { return };
            let approve = project.approval(&path) != ApprovalState::Approved;
            let fingerprint = approve.then(|| project.fingerprint(&path).to_owned());
            project.review.set_approved(&path, fingerprint.as_deref(), app_core::now());
            project.save_review();
            (path, approve)
        };
        self.render_review_change(&path);
        if approved_now {
            match self.0.w.files.next_unreviewed(Some(&path)) {
                Some(next) => self.select(Some(next), false),
                None => self.toast("All files reviewed"),
            }
        }
    }

    fn add_note(&self, anchor: Option<LineAnchor>, body: &str) {
        let path = {
            let mut state = self.0.state.borrow_mut();
            let Some(path) = state.selected.clone() else { return };
            let Some(project) = state.project.as_mut() else { return };
            let now = app_core::now();
            project.review.add_note(&path, anchor, body, now);
            // Commenting on a file counts as reviewing it.
            if project.approval(&path) != ApprovalState::Approved {
                let fingerprint = project.fingerprint(&path).to_owned();
                project.review.set_approved(&path, Some(&fingerprint), now);
            }
            project.save_review();
            path
        };
        self.render_review_change(&path);
    }

    fn delete_note(&self, id: u64) {
        let (path, note) = {
            let mut state = self.0.state.borrow_mut();
            let Some(path) = state.selected.clone() else { return };
            let Some(project) = state.project.as_mut() else { return };
            let note = project.review.notes(&path).iter().find(|n| n.id == id).cloned();
            project.review.remove_note(&path, id);
            project.save_review();
            (path, note)
        };
        self.render_review_change(&path);

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

    fn copy_feedback(&self) {
        let feedback = {
            let state = self.0.state.borrow();
            let Some(project) = &state.project else { return };
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
            feedback::build(&project.review, &files, &Options::default())
        };
        if feedback.is_empty() {
            self.toast("Nothing to copy yet: comment on or review some files first");
            return;
        }
        self.0.w.window.clipboard().set_text(&feedback.text);
        self.mark_sent(feedback.note_ids);
    }

    /// Marks copied comments as sent and confirms with an undoable toast.
    fn mark_sent(&self, ids: Vec<u64>) {
        if let Some(project) = self.0.state.borrow_mut().project.as_mut() {
            project.review.set_sent(&ids, Some(app_core::now()));
            project.save_review();
        }
        self.render();

        let toast = adw::Toast::builder().title("Feedback copied. Paste it into Claude Code").timeout(5).build();
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

    let copy_count = gtk::Label::new(None);
    copy_count.add_css_class("count-badge");
    copy_count.set_valign(gtk::Align::Center);
    let copy_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    copy_content.append(&gtk::Image::from_icon_name("edit-copy-symbolic"));
    copy_content.append(&gtk::Label::new(Some("Copy Feedback")));
    copy_content.append(&copy_count);
    let copy_button = gtk::Button::builder()
        .child(&copy_content)
        .tooltip_text("Copy unsent comments as a prompt for Claude Code (Ctrl+Shift+C)")
        .action_name("win.copy-feedback")
        .build();
    copy_button.add_css_class("suggested-action");
    copy_button.add_css_class("toolbar-button");

    let review_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    review_actions.append(&progress);
    review_actions.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    review_actions.append(&copy_button);

    let main_menu = gio::Menu::new();
    let section = gio::Menu::new();
    section.append(Some("Open Folder…"), Some("win.open-folder"));
    section.append(Some("Refresh"), Some("win.refresh"));
    main_menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append(Some("Clear Review…"), Some("win.clear-review"));
    main_menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append(Some("Keyboard Shortcuts"), Some("win.show-help-overlay"));
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
    let search = gtk::SearchEntry::builder().placeholder_text("Filter files (Ctrl+F)").build();
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
    let reviewed = gtk::CheckButton::builder()
        .action_name("win.reviewed")
        .tooltip_text("Reviewed: check to mark this file done and open the next one (Ctrl+Shift+A)")
        .valign(gtk::Align::Center)
        .build();
    reviewed.add_css_class("review-check");
    let file_badge = util::label("", &["status-letter"]);
    let file_dir = util::label("", &["file-header-dir"]);
    file_dir.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    let file_name = util::label("", &["file-header-name"]);
    let file_comment = gtk::Button::builder()
        .icon_name("chat-message-new-symbolic")
        .tooltip_text("Comment on This File")
        .action_name("win.comment-file")
        .valign(gtk::Align::Center)
        .build();
    file_comment.add_css_class("flat");
    file_comment.add_css_class("icon-button");
    let file_meta = util::label("", &["file-header-meta"]);
    file_meta.set_margin_start(8);
    file_meta.set_hexpand(true);
    file_meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let stale_chip = util::label("Changed since review", &["chip", "warning-chip"]);
    stale_chip.set_valign(gtk::Align::Center);
    let prev = gtk::Button::builder()
        .icon_name("go-up-symbolic")
        .tooltip_text("Previous File (Alt+↑)")
        .action_name("win.prev-file")
        .build();
    let next = gtk::Button::builder()
        .icon_name("go-down-symbolic")
        .tooltip_text("Next File (Alt+↓)")
        .action_name("win.next-file")
        .build();
    for button in [&prev, &next] {
        button.add_css_class("flat");
        button.add_css_class("small-button");
    }
    let file_header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    file_header.add_css_class("file-header");
    file_header.append(&reviewed);
    file_header.append(&file_badge);
    file_header.append(&file_dir);
    file_header.append(&file_name);
    file_header.append(&file_comment);
    file_header.append(&file_meta);
    file_header.append(&stale_chip);
    file_header.append(&prev);
    file_header.append(&next);

    let diff = DiffView::new();

    let open_button =
        gtk::Button::builder().label("Open Folder…").halign(gtk::Align::Center).action_name("win.open-folder").build();
    open_button.add_css_class("pill");
    open_button.add_css_class("suggested-action");
    let welcome = adw::StatusPage::builder()
        .icon_name(config::APP_ID)
        .title("Review Claude's Changes")
        .description(
            "Open a Git repository to review its uncommitted changes, comment on lines, and send the feedback \
             back to Claude Code.",
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
    content_stack.add_named(diff.widget(), Some("diff"));

    let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
    main.add_css_class("main-area");
    main.append(&file_header);
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
        file_header,
        file_badge,
        file_dir,
        file_name,
        file_meta,
        stale_chip,
        diff,
        review_actions,
        progress,
        copy_count,
        error_page,
        clean_page,
    }
}
