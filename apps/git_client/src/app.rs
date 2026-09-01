//! Root widget for the git client.
//!
//! Owns the in-memory [`RepoModel`], the currently-shown diff, the active
//! screen and all transient selection / scroll state. Durable preferences
//! live in the CRDT via [`Settings`]. The widget draws everything itself
//! into the Vello scene — a top app bar with a screen switcher, then the
//! active screen (changes / history / panels). Events are routed by
//! hit-testing the regions each screen emits.

use std::collections::HashMap;
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{FlapState, SplitLayout, TextInput, TextInputKind, Widget};

use crate::changes::{ChangesColumn, ChangesHit};
use crate::diffview::{self, DiffView};
use crate::draw;
use crate::graph::{self, GraphLayout};
use crate::history::HistoryView;
use crate::model::{ChangeArea, DiffKey, DiffMap, FileChange, RepoModel};
use crate::panels::{BranchAction, PanelHit, PanelsView, RemoteAction};
use crate::settings::{DiffLayout, Settings};

/// Which embedded text field currently holds keyboard focus. Text editing is
/// delegated to the toolkit's [`TextInput`] (cursor, selection, word-nav,
/// clipboard); this tracks which one keys route to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    /// The commit-message summary field.
    Commit,
    /// The new-branch name field on the Branches screen.
    Branch,
    /// The repository-path field on the empty "open a repository" screen.
    Open,
}

/// Which top-level screen is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Changes,
    History,
    Panels,
}

/// A destructive discard action armed for confirmation, scoped to the file
/// currently shown in the diff pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiscardTarget {
    /// Revert the whole file's worktree changes (or delete an untracked file).
    File,
    /// Revert a single hunk of the file's worktree changes.
    Hunk(usize),
}

impl Screen {
    /// Whether this screen has a primary left list column that adapts into an
    /// overlay flap on a narrow window. The Changes (file list) and History
    /// (commit list) screens are master-detail; Branches & Remotes is a
    /// full-width dashboard with no collapsible list.
    fn has_sidebar(self) -> bool {
        matches!(self, Screen::Changes | Screen::History)
    }
}

const TABS: [(Screen, &str); 3] = [
    (Screen::Changes, "Changes"),
    (Screen::History, "History"),
    (Screen::Panels, "Branches & Remotes"),
];

const APPBAR_H: f64 = 56.0;
const CHANGES_COL_W: f64 = 320.0;

/// Below this allocated width (logical px) the primary list column stops
/// fitting comfortably beside a readable detail pane, so it collapses into an
/// overlay flap. Chosen so the 320px list + a usable diff/detail pane keep
/// breathing room above the breakpoint and the app bar's tab + settings
/// clusters still fit.
const NARROW_BREAKPOINT: f64 = 760.0;

/// Width of the overlay flap when fully revealed on a narrow window. Matches
/// the docked changes column so the list looks identical in both layouts.
const FLAP_W: f64 = CHANGES_COL_W;

pub struct GitClientApp {
    model: RepoModel,
    diffs: DiffMap,
    graph_layout: GraphLayout,

    /// Framework-owned action dispatch (the only action-dispatch state the app
    /// holds). Every durable view-preference toggle dispatches its
    /// [`GitClientAction`](crate::actions::GitClientAction) through this: the
    /// host brackets the undo boundary, encodes and applies through the executor
    /// (so it replays byte-identically on peers), and remembers the last action
    /// for the affordance == `ActionSpec` covenant test.
    actions: ui::ActionHost,

    /// No repository is bound and none was reachable at launch — the app shows
    /// the honest "open a repository" empty state rather than any placeholder
    /// data. Cleared once a repo is opened (via the path field or at startup).
    awaiting_repo: bool,
    /// Which embedded text field holds focus, if any.
    focus: Option<Field>,
    /// Commit-message summary field (toolkit `TextInput`, local mode).
    commit_input: TextInput,
    /// New-branch name field (toolkit `TextInput`, local mode).
    branch_input: TextInput,
    /// Repository-path field on the empty-state screen (toolkit `TextInput`).
    open_input: TextInput,
    /// Hit rects for the empty-state open field + button.
    open_input_rect: Rect,
    open_button_rect: Rect,

    /// Path the repository was opened from, when backed by a real on-disk
    /// repo. `None` for the bundled sample (the `--shots` path), in which
    /// case actions fall back to the in-memory model mutation so the
    /// screenshot still demonstrates the flow. When `Some`, every action
    /// drives `git2` against the real repo and re-reads the model.
    repo_path: Option<std::path::PathBuf>,
    /// Last action error, surfaced in the UI (e.g. "no identity set").
    last_error: Option<String>,

    settings: Settings,
    /// Set once after the first sync seeds the settings into the doc.
    settings_seeded: bool,
    /// An initial preference was forced at construction (the `GIT_CLIENT_SPLIT`
    /// screenshot path), so the first sync keeps it rather than adopting the
    /// document's persisted value. Interactive toggles never set this — they go
    /// through the action executor, which writes the document directly.
    settings_forced: bool,
    /// Cached hit rects for the app-bar settings controls, set during draw.
    settings_seg: Rect,
    settings_syn: Rect,

    screen: Screen,
    rect: Rect,

    // --- Changes screen state ---
    selected_change: Option<(ChangeArea, usize)>,
    changes_scroll: f64,
    diff_scroll: f64,
    hover_hunk: Option<usize>,
    /// A discard action armed for confirmation on the selected file: the next
    /// click on the same (now red) affordance performs it; any other
    /// interaction disarms. Guards the destructive worktree revert behind a
    /// deliberate second click.
    armed_discard: Option<DiscardTarget>,

    // --- History screen state ---
    selected_commit: usize,
    history_scroll: f64,
    detail_scroll: f64,

    // --- Panels screen state ---
    hover_action: Option<RemoteAction>,
    /// Hit rects for the Panels "new branch" input + create button.
    new_branch_input: Rect,
    new_branch_button: Rect,

    // --- Responsive sidebar (the primary list column) ---
    /// Eased open/closed state of the primary list column, shared with every
    /// other adaptive app via the toolkit kernel. Open when docked (wide),
    /// closed overlay flap when narrow, summoned by the hamburger.
    flap: FlapState,
    /// Wall-clock of the last flap animation tick, so the eased slide is
    /// frame-rate independent.
    last_flap_tick: Instant,
    /// Hit rect for the app-bar hamburger toggle (overlay mode only).
    hamburger_rect: Rect,

    last_mouse: Point,

    // Click regions emitted by the last draw, used for hit-testing the
    // next click. Boxed closures keep geometry out of the event handler.
    change_hits: Vec<Box<dyn Fn(Point) -> Option<ChangesHit>>>,
    diff_hunk_buttons: Vec<diffview::HunkButton>,
    /// Hit rect for the diff-header "Discard file" button (worktree side only).
    discard_file_rect: Rect,
    history_rows: Vec<Rect>,
    panel_hits: Vec<Box<dyn Fn(Point) -> Option<PanelHit>>>,
    tab_rects: Vec<(Screen, Rect)>,
}

impl Default for GitClientApp {
    /// Equivalent to [`GitClientApp::new`].
    fn default() -> Self {
        Self::new()
    }
}

impl GitClientApp {
    /// Build the app, loading the repository discovered from the current
    /// directory (or `GIT_CLIENT_REPO`). Falls back to the bundled sample
    /// when no repository is reachable.
    ///
    /// Setting `GIT_CLIENT_SAMPLE=1` forces the bundled fixture regardless
    /// of any reachable repository — the headless screenshot path uses
    /// this so the capture is a fully populated, deterministic state
    /// (rename + delete + new file across all three areas, a multi-lane
    /// commit graph) rather than whatever happens to sit around the CWD.
    pub fn new() -> Self {
        let force_sample =
            std::env::var("GIT_CLIENT_SAMPLE").is_ok_and(|v| v != "0" && !v.is_empty());
        if force_sample {
            let (model, diffs) = crate::sample::model();
            return Self::from_model(model, diffs);
        }
        if let Some((path, model, diffs)) = Self::load_repo() {
            let mut app = Self::from_model(model, diffs);
            app.repo_path = Some(path);
            app
        } else {
            // No reachable repository — show the honest "open a repository"
            // empty state rather than any placeholder data. The path field
            // is pre-filled with the current directory as a convenience.
            let mut app = Self::from_model(RepoModel::default(), DiffMap::new());
            app.awaiting_repo = true;
            if let Ok(cwd) = std::env::current_dir() {
                app.open_input.set_text(&cwd.to_string_lossy());
            }
            app
        }
    }

    /// Open a real repository discovered from `path` and build the app
    /// bound to it, so every action drives `git2` and re-reads. Returns
    /// `None` when no repository is reachable. Used by the live app's
    /// `GIT_CLIENT_REPO` path and by integration tests that need to drive
    /// the production actions against a temp repo.
    pub fn open_at(path: &std::path::Path) -> Option<Self> {
        let anchor = crate::repo::workdir(path).unwrap_or_else(|| path.to_path_buf());
        let (model, diffs) = crate::repo::load(path)?;
        let mut app = Self::from_model(model, diffs);
        app.repo_path = Some(anchor);
        Some(app)
    }

    /// Build the honest empty "open a repository" state with no repo bound —
    /// the same state the live app shows when no repository is reachable.
    /// Exposed so integration tests can drive the open flow.
    #[doc(hidden)]
    pub fn open_empty() -> Self {
        let mut app = Self::from_model(RepoModel::default(), DiffMap::new());
        app.awaiting_repo = true;
        app
    }

    /// Construct from an explicit model — used by the sample path and tests.
    pub fn from_model(mut model: RepoModel, diffs: DiffMap) -> Self {
        let graph_layout = graph::layout(&mut model.commits);
        // Pre-select the first changed file so the diff pane is populated.
        let selected_change = first_change(&model);
        // Allow the headless capture to open a specific screen so each
        // screen can be reviewed; defaults to Changes for the live app.
        let screen = match std::env::var("GIT_CLIENT_SCREEN").as_deref() {
            Ok("history") => Screen::History,
            Ok("panels") => Screen::Panels,
            _ => Screen::Changes,
        };
        let mut settings = Settings::default();
        let split_forced = std::env::var("GIT_CLIENT_SPLIT").is_ok();
        if split_forced {
            settings.diff_layout = DiffLayout::SideBySide;
        }
        // The primary list column defaults open when docked. The headless
        // narrow capture sets `GIT_CLIENT_FLAP_OPEN` so the single offscreen
        // frame shows the overlay flap fully revealed over its scrim rather
        // than the closed default a fresh narrow window opens to.
        let mut flap = FlapState::open_when_docked();
        if std::env::var("GIT_CLIENT_FLAP_OPEN").is_ok_and(|v| v != "0" && !v.is_empty()) {
            flap.snap(true);
        }
        let commit_input = TextInput::local()
            .with_placeholder("Summary of changes…")
            .with_font_size(13.0);
        let branch_input = TextInput::local()
            .with_kind(TextInputKind::Name)
            .with_placeholder("new branch name…")
            .with_font_size(12.5);
        let open_input = TextInput::local()
            .with_kind(TextInputKind::Path)
            .with_placeholder("/path/to/repository")
            .with_font_size(14.0)
            .with_mono();
        Self {
            actions: ui::ActionHost::new(),
            flap,
            last_flap_tick: Instant::now(),
            hamburger_rect: Rect::ZERO,
            model,
            diffs,
            graph_layout,
            awaiting_repo: false,
            focus: None,
            commit_input,
            branch_input,
            open_input,
            open_input_rect: Rect::ZERO,
            open_button_rect: Rect::ZERO,
            repo_path: None,
            last_error: None,
            settings,
            settings_seeded: false,
            settings_forced: split_forced,
            settings_seg: Rect::ZERO,
            settings_syn: Rect::ZERO,
            screen,
            rect: Rect::ZERO,
            selected_change,
            changes_scroll: 0.0,
            diff_scroll: 0.0,
            hover_hunk: None,
            armed_discard: None,
            selected_commit: 0,
            history_scroll: 0.0,
            detail_scroll: 0.0,
            hover_action: None,
            new_branch_input: Rect::ZERO,
            new_branch_button: Rect::ZERO,
            last_mouse: Point::ZERO,
            change_hits: Vec::new(),
            diff_hunk_buttons: Vec::new(),
            discard_file_rect: Rect::ZERO,
            history_rows: Vec::new(),
            panel_hits: Vec::new(),
            tab_rects: Vec::new(),
        }
    }

    // --- Test-facing drivers --------------------------------------------
    // These expose the *production* action methods so integration tests can
    // drive the exact code path the UI uses, then assert on the real repo.
    // They are thin pass-throughs — no test-only logic lives behind them.

    /// Drive the production "stage/unstage this file" action by path.
    #[doc(hidden)]
    pub fn test_toggle_stage(&mut self, area: ChangeArea, idx: usize) {
        self.toggle_stage(area, idx);
    }
    /// Drive the production "stage all" action.
    #[doc(hidden)]
    pub fn test_stage_all(&mut self) {
        self.stage_all();
    }
    /// Drive the production "unstage all" action.
    #[doc(hidden)]
    pub fn test_unstage_all(&mut self) {
        self.unstage_all();
    }
    /// Set the commit message and drive the production commit action.
    #[doc(hidden)]
    pub fn test_commit(&mut self, message: &str) {
        self.commit_input.set_text(message);
        self.do_commit();
    }
    /// Drive the production per-hunk stage/unstage action for the selected file.
    #[doc(hidden)]
    pub fn test_stage_hunk(&mut self, hunk: usize) {
        self.stage_hunk(hunk);
    }
    /// Drive the production branch action (checkout / delete / stash-apply).
    #[doc(hidden)]
    pub fn test_branch_action(&mut self, action: BranchAction) {
        self.apply_branch_action(action);
    }
    /// Drive the production "create branch" action from a name.
    #[doc(hidden)]
    pub fn test_create_branch(&mut self, name: &str) {
        self.branch_input.set_text(name);
        self.create_new_branch();
    }
    /// Drive the production "discard file" action by area + index.
    #[doc(hidden)]
    pub fn test_discard_file(&mut self, area: ChangeArea, idx: usize) {
        self.discard_file(area, idx);
    }
    /// Drive the production "discard hunk" action for the selected file.
    #[doc(hidden)]
    pub fn test_discard_hunk(&mut self, hunk: usize) {
        self.discard_hunk(hunk);
    }
    /// Whether the app is showing the empty "open a repository" state.
    #[doc(hidden)]
    pub fn test_awaiting_repo(&self) -> bool {
        self.awaiting_repo
    }
    /// Drive the empty-state "open this path" action.
    #[doc(hidden)]
    pub fn test_open_path(&mut self, path: &str) {
        self.open_input.set_text(path);
        self.open_repo_from_input();
    }
    /// Select a file in a given area + index (mirrors a list click).
    #[doc(hidden)]
    pub fn test_select(&mut self, area: ChangeArea, idx: usize) {
        self.selected_change = Some((area, idx));
    }
    /// The current `head` shorthand from the live model.
    #[doc(hidden)]
    pub fn test_head(&self) -> &str {
        &self.model.head
    }
    /// Counts of (staged, unstaged, untracked) entries in the live model.
    #[doc(hidden)]
    pub fn test_change_counts(&self) -> (usize, usize, usize) {
        (
            self.model.staged.len(),
            self.model.unstaged.len(),
            self.model.untracked.len(),
        )
    }
    /// The live model's commit summaries, newest first.
    #[doc(hidden)]
    pub fn test_commit_summaries(&self) -> Vec<String> {
        self.model
            .commits
            .iter()
            .map(|c| c.summary.clone())
            .collect()
    }
    /// The live model's local + remote branch names.
    #[doc(hidden)]
    pub fn test_branch_names(&self) -> Vec<String> {
        self.model.branches.iter().map(|b| b.name.clone()).collect()
    }
    /// The last action error message, if any.
    #[doc(hidden)]
    pub fn test_last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    fn load_repo() -> Option<(std::path::PathBuf, RepoModel, DiffMap)> {
        let path = std::env::var("GIT_CLIENT_REPO").map_or_else(
            |_| std::env::current_dir().unwrap_or_else(|_| ".".into()),
            std::path::PathBuf::from,
        );
        // Anchor actions on the working directory so later opens are stable
        // even if the CWD changes; fall back to the given path when bare.
        let anchor = crate::repo::workdir(&path).unwrap_or_else(|| path.clone());
        let (model, diffs) = crate::repo::load(&path)?;
        Some((anchor, model, diffs))
    }

    /// Re-read the live repository into the model after a mutation, so the
    /// UI reflects the *actual* on-disk state (proving the op took effect).
    /// A no-op for the sample-backed app. Preserves the selected screen and
    /// clamps stale selections.
    fn reload(&mut self) {
        let Some(path) = self.repo_path.clone() else {
            return;
        };
        if let Some((model, diffs)) = crate::repo::load(&path) {
            self.model = model;
            self.diffs = diffs;
            self.graph_layout = graph::layout(&mut self.model.commits);
            self.model.graph_width = self.graph_layout.lanes;
            self.selected_change = first_change(&self.model);
            if self.selected_commit >= self.model.commits.len() {
                self.selected_commit = 0;
            }
            self.diff_scroll = 0.0;
        }
    }

    /// Run an action and record any error for the status line. On success
    /// the model is reloaded from disk so the render reflects reality.
    fn run_action(&mut self, result: anyhow::Result<()>) {
        match result {
            Ok(()) => {
                self.last_error = None;
                self.reload();
            }
            Err(e) => {
                self.last_error = Some(e.to_string());
            }
        }
    }

    /// The repo path for actions; `None` means sample-backed (no real repo).
    fn repo(&self) -> Option<std::path::PathBuf> {
        self.repo_path.clone()
    }

    // --- Responsive layout -------------------------------------------

    /// Resolve the sidebar layout from the app's allocated width. The active
    /// screen also gates this: only master-detail screens have a collapsible
    /// list, so screens without one always report `Docked` (their content is
    /// already full-width and no flap/scrim/hamburger appears).
    fn layout_mode(&self) -> SplitLayout {
        if !self.screen.has_sidebar() {
            return SplitLayout::Docked;
        }
        SplitLayout::from_width(self.rect.width(), NARROW_BREAKPOINT)
    }

    /// Whether the primary list column is currently a revealed/open overlay.
    fn flap_open(&self) -> bool {
        self.flap.is_open(self.layout_mode())
    }

    /// Advance the flap reveal toward its target using real elapsed time.
    /// Called once per drawn frame so the slide + scrim fade are smooth and
    /// frame-rate independent.
    fn tick_flap(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_flap_tick).as_secs_f64();
        self.last_flap_tick = now;
        self.flap.tick(self.layout_mode(), dt.min(0.1));
    }

    /// The diff-map key for the currently selected change, if any. Resolves
    /// the staged vs. worktree side from the selected area so a partially
    /// staged file shows the correct diff on each side.
    fn current_diff_key(&self) -> Option<DiffKey> {
        let (area, idx) = self.selected_change?;
        let list = match area {
            ChangeArea::Staged => &self.model.staged,
            ChangeArea::Unstaged => &self.model.unstaged,
            ChangeArea::Untracked => &self.model.untracked,
        };
        Some(DiffKey::for_area(area, list.get(idx)?.path.clone()))
    }

    /// Resolve the [`FileDiff`] for the currently selected change, if any.
    fn current_diff(&self) -> Option<&crate::model::FileDiff> {
        self.diffs.get(&self.current_diff_key()?)
    }

    fn selected_is_staged(&self) -> bool {
        matches!(self.selected_change, Some((ChangeArea::Staged, _)))
    }

    // --- Mutations on the model (mirror a real git op) -----------------

    /// Stage or unstage a single file. Against a real repo this calls
    /// `git2` (`add` / `reset HEAD -- path`) and re-reads the model so the
    /// list reflects the actual index. For the sample it falls back to the
    /// in-memory move so the offscreen capture still demonstrates the flow.
    fn toggle_stage(&mut self, area: ChangeArea, idx: usize) {
        if let Some(path) = self.repo() {
            let list = match area {
                ChangeArea::Staged => &self.model.staged,
                ChangeArea::Unstaged => &self.model.unstaged,
                ChangeArea::Untracked => &self.model.untracked,
            };
            let Some(file) = list.get(idx).map(|f| f.path.clone()) else {
                return;
            };
            let res = match area {
                ChangeArea::Staged => crate::repo::unstage_file(&path, &file),
                ChangeArea::Unstaged | ChangeArea::Untracked => {
                    crate::repo::stage_file(&path, &file)
                }
            };
            self.run_action(res);
            return;
        }
        self.toggle_stage_in_memory(area, idx);
    }

    /// In-memory move used only by the sample-backed app (no repo on disk).
    fn toggle_stage_in_memory(&mut self, area: ChangeArea, idx: usize) {
        match area {
            ChangeArea::Staged => {
                if idx < self.model.staged.len() {
                    let mut f = self.model.staged.remove(idx);
                    f.area = ChangeArea::Unstaged;
                    self.model.unstaged.push(f);
                }
            }
            ChangeArea::Unstaged => {
                if idx < self.model.unstaged.len() {
                    let mut f = self.model.unstaged.remove(idx);
                    f.area = ChangeArea::Staged;
                    self.model.staged.push(f);
                }
            }
            ChangeArea::Untracked => {
                if idx < self.model.untracked.len() {
                    let mut f = self.model.untracked.remove(idx);
                    f.area = ChangeArea::Staged;
                    f.kind = crate::model::ChangeKind::Added;
                    self.model.staged.push(f);
                }
            }
        }
        self.selected_change = first_change(&self.model);
    }

    fn stage_all(&mut self) {
        if let Some(path) = self.repo() {
            let res = crate::repo::stage_all(&path);
            self.run_action(res);
            return;
        }
        let mut moved: Vec<FileChange> = Vec::new();
        for mut f in self.model.unstaged.drain(..) {
            f.area = ChangeArea::Staged;
            moved.push(f);
        }
        for mut f in self.model.untracked.drain(..) {
            f.area = ChangeArea::Staged;
            f.kind = crate::model::ChangeKind::Added;
            moved.push(f);
        }
        self.model.staged.append(&mut moved);
        self.selected_change = first_change(&self.model);
    }

    fn unstage_all(&mut self) {
        if let Some(path) = self.repo() {
            let res = crate::repo::unstage_all(&path);
            self.run_action(res);
            return;
        }
        let mut moved: Vec<FileChange> = Vec::new();
        for mut f in self.model.staged.drain(..) {
            f.area = ChangeArea::Unstaged;
            moved.push(f);
        }
        self.model.unstaged.append(&mut moved);
        self.selected_change = first_change(&self.model);
    }

    fn do_commit(&mut self) {
        if self.model.staged.is_empty() || self.commit_input.value().trim().is_empty() {
            return;
        }
        if let Some(path) = self.repo() {
            let msg = self.commit_input.value().to_string();
            let res = crate::repo::commit(&path, &msg).map(|_| ());
            let ok = res.is_ok();
            self.run_action(res);
            if ok {
                self.commit_input.set_text("");
                self.set_focus(None);
                // Land on the History screen so the new commit is visible.
                self.switch_screen(Screen::History);
                self.selected_commit = 0;
                self.history_scroll = 0.0;
            }
            return;
        }
        self.do_commit_in_memory();
    }

    /// Sample-only commit: synthesise a tip commit so the offscreen capture
    /// shows the action's effect without a repository.
    fn do_commit_in_memory(&mut self) {
        let staged = std::mem::take(&mut self.model.staged);
        let parent = self.model.commits.first().map(|c| c.id.clone());
        let id = format!("{:07x}", self.model.commits.len() * 0x1f7 + 0xa10);
        let message = self.commit_input.value().to_string();
        let summary = message.lines().next().unwrap_or("").to_string();
        let body = message
            .split_once('\n')
            .map(|x| x.1)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let new = crate::model::Commit {
            id,
            summary,
            body,
            author_name: self.model.author_name.clone(),
            author_email: self.model.author_email.clone(),
            when: "just now".into(),
            time: chrono::Utc::now().timestamp(),
            parents: parent.into_iter().collect(),
            refs: vec![
                crate::model::RefLabel {
                    name: "HEAD".into(),
                    kind: crate::model::RefKind::Head,
                },
                crate::model::RefLabel {
                    name: self.model.head.clone(),
                    kind: crate::model::RefKind::LocalBranch,
                },
            ],
            lane: 0,
            files: staged,
        };
        if let Some(prev) = self.model.commits.first_mut() {
            prev.refs
                .retain(|r| r.kind != crate::model::RefKind::Head && r.name != self.model.head);
        }
        self.model.commits.insert(0, new);
        self.graph_layout = graph::layout(&mut self.model.commits);
        self.model.graph_width = self.graph_layout.lanes;
        self.commit_input.set_text("");
        self.set_focus(None);
        self.selected_change = first_change(&self.model);
    }

    /// Discard a file's worktree changes (revert unstaged edits, or delete an
    /// untracked file). Two-click armed: the first click on a discard
    /// affordance arms the row; only a second confirming click reaches here.
    fn discard_file(&mut self, area: ChangeArea, idx: usize) {
        self.armed_discard = None;
        let Some(path) = self.repo() else { return };
        let list = match area {
            ChangeArea::Staged => &self.model.staged,
            ChangeArea::Unstaged => &self.model.unstaged,
            ChangeArea::Untracked => &self.model.untracked,
        };
        let Some(file) = list.get(idx).map(|f| f.path.clone()) else {
            return;
        };
        let res = crate::repo::discard_file(&path, &file);
        self.run_action(res);
    }

    /// Discard a single hunk of the selected (unstaged) file's worktree change.
    fn discard_hunk(&mut self, hunk: usize) {
        let Some((area, _)) = self.selected_change else {
            return;
        };
        // Only worktree (unstaged/untracked) hunks are discardable — a staged
        // file's diff is the index side, which "discard" doesn't act on.
        if area == ChangeArea::Staged {
            return;
        }
        if let (Some(path), Some(file)) = (self.repo(), self.current_path()) {
            let res = crate::repo::discard_hunk(&path, &file, hunk);
            self.run_action(res);
        }
    }

    /// Open the repository at the empty-state path field, binding it if valid.
    fn open_repo_from_input(&mut self) {
        let raw = self.open_input.value().trim().to_string();
        if raw.is_empty() {
            return;
        }
        let path = std::path::PathBuf::from(&raw);
        match crate::repo::load(&path) {
            Some((model, diffs)) => {
                let anchor = crate::repo::workdir(&path).unwrap_or(path);
                self.model = model;
                self.diffs = diffs;
                self.graph_layout = graph::layout(&mut self.model.commits);
                self.model.graph_width = self.graph_layout.lanes;
                self.repo_path = Some(anchor);
                self.selected_change = first_change(&self.model);
                self.awaiting_repo = false;
                self.last_error = None;
                self.set_focus(None);
                self.screen = Screen::Changes;
                self.flap.reconcile(self.layout_mode());
            }
            None => {
                self.last_error = Some(format!("No git repository at {raw}"));
            }
        }
    }

    /// Give keyboard focus to one embedded text field (or none), keeping the
    /// toolkit inputs' focus flags mutually exclusive.
    fn set_focus(&mut self, field: Option<Field>) {
        self.focus = field;
        self.commit_input.set_focus(field == Some(Field::Commit));
        self.branch_input.set_focus(field == Some(Field::Branch));
        self.open_input.set_focus(field == Some(Field::Open));
    }

    // --- Durable view-preference actions -------------------------------
    // The diff-view preferences are the app's only shared-document state.
    // Each toggle updates the local snapshot for an immediate redraw AND
    // dispatches its `GitClientAction` through the framework `ActionHost`,
    // which brackets the undo boundary and applies it through the executor
    // (so it records one Ctrl+Z step and replays byte-identically on peers).
    // The next `sync` re-reads the same value straight back out of the doc.
    // These are the exact actions the Ctrl+K resource tree exports — one
    // source of truth (see `crate::actions`).

    /// Flip the diff-pane layout (unified ⇄ side-by-side). Routed to by the
    /// app-bar segmented control and the `d` shortcut.
    fn toggle_diff_layout(&mut self, doc: &mut AutoCommit) {
        self.settings.toggle_layout();
        self.diff_scroll = 0.0;
        let action = crate::actions::set_diff_layout_action(self.settings.diff_layout);
        self.actions.dispatch_with_context(
            doc,
            crate::actions::GIT_CLIENT_ACTION_TYPE,
            &action,
            qos_actions::ActionContext::for_action("org.qos.Git", "Git", &action),
        );
    }

    /// Flip diff syntax highlighting. Routed to by the app-bar `</>` pill.
    fn toggle_syntax_highlight(&mut self, doc: &mut AutoCommit) {
        self.settings.syntax_highlight = !self.settings.syntax_highlight;
        let action = crate::actions::GitClientAction::SetSyntaxHighlight {
            on: self.settings.syntax_highlight,
        };
        self.actions.dispatch_with_context(
            doc,
            crate::actions::GIT_CLIENT_ACTION_TYPE,
            &action,
            qos_actions::ActionContext::for_action("org.qos.Git", "Git", &action),
        );
    }

    /// The label for the open-repository resource Ctrl+K publishes, or `None`
    /// when no repository is open (the empty state) so the palette shows
    /// nothing to act on.
    pub(crate) fn resource_repo_label(&self) -> Option<String> {
        if self.awaiting_repo {
            return None;
        }
        Some(self.model.name.clone())
    }

    /// The last preference [`EncodedAction`](qos_actions::EncodedAction) an
    /// on-screen control dispatched, for the affordance == `ActionSpec`
    /// covenant test.
    #[doc(hidden)]
    pub fn last_dispatched(&self) -> Option<&qos_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    // --- Drawing -------------------------------------------------------

    fn draw_appbar(&mut self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme) {
        let c = &theme.tokens.colors;
        let bar = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + APPBAR_H,
        );
        draw::fill_rect(scene, t, bar, c.surfaces.raised);
        draw::hline(scene, t, bar.y1, bar.x0, bar.x1, c.border_standard);

        // Hamburger toggle: shown only when the primary list column has
        // collapsed into an overlay flap (narrow window, master-detail
        // screen). It summons/dismisses the flap. The title block shifts
        // right to make room. Outside overlay mode there's no flap to toggle.
        let left_pad = if self.layout_mode() == SplitLayout::Overlay {
            self.draw_hamburger(scene, t, theme, bar);
            44.0
        } else {
            self.hamburger_rect = Rect::ZERO;
            0.0
        };

        // Screen tabs (centered group on the right side of name). Measure the
        // group first so the title can be clamped to stop before it on a
        // narrow window instead of running underneath.
        self.tab_rects.clear();
        // Keep the settings cluster wide enough for the full "Syntax" label.
        // The old 46px icon-sized control left only "Syn" visible in the
        // shipped shot, which made a presentation preference look like a
        // truncated status label.
        let settings_w = 230.0;
        let (widths, total) = Self::measure_appbar_tabs(tr);
        let tx_start = bar.x1 - settings_w - total - 20.0;
        let tab_group_left = tx_start - 6.0;

        // Repo name + branch. Clamp the title so it ellipsises before the tab
        // group rather than colliding with it when the bar is tight.
        let title_x = bar.x0 + 20.0 + left_pad;
        self.draw_appbar_title(scene, tr, t, theme, bar, title_x, tab_group_left);

        self.draw_appbar_tab_group(scene, tr, t, theme, bar, &widths, total, tx_start);

        // Settings cluster on the far right: diff layout toggle + flags.
        self.draw_settings_cluster(scene, tr, t, theme, bar, settings_w);
    }

    /// Each tab's pixel width and their combined width (including the 6px
    /// gaps between them) — needed both to clamp the title and to lay out
    /// the tab group's pill background.
    fn measure_appbar_tabs(tr: &mut TextRenderer) -> (Vec<f64>, f64) {
        let widths: Vec<f64> = TABS
            .iter()
            .map(|(_, label)| {
                f64::from(tr.measure_weighted(label, 13.0, None, TextWeight::Medium).0) + 28.0
            })
            .collect();
        let total = widths.iter().sum::<f64>() + (TABS.len() as f64 - 1.0) * 6.0;
        (widths, total)
    }

    /// Draw the repo name, branch pill and pending-changes/error subtitle,
    /// clamped so a long name ellipsises before the tab group.
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    fn draw_appbar_title(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        bar: Rect,
        title_x: f64,
        tab_group_left: f64,
    ) {
        let c = &theme.tokens.colors;
        let title_max = (tab_group_left - title_x - 8.0).max(40.0) as f32;
        draw::text_weighted(
            scene,
            tr,
            t,
            &self.model.name,
            15.0,
            c.text_primary,
            title_x,
            bar.y0 + 9.0,
            Some(title_max),
            TextWeight::Bold,
        );
        let nw = f64::from(
            tr.measure_weighted(&self.model.name, 15.0, None, TextWeight::Bold)
                .0,
        )
        .min(f64::from(title_max));
        let pill_x = title_x + 8.0 + nw;
        // Only paint the branch pill if it still fits left of the tab group.
        if pill_x + 40.0 < tab_group_left {
            draw::pill(
                scene,
                tr,
                t,
                pill_x,
                bar.y0 + 9.0,
                20.0,
                &self.model.head,
                Color::WHITE,
                theme.accent_primary,
                None,
            );
        }
        // change count subtitle — or the last action error when present.
        let (sub, sub_color) = match &self.last_error {
            Some(err) => (err.clone(), c.semantic.danger),
            None => (
                format!(
                    "{} pending change{}",
                    self.model.change_count(),
                    if self.model.change_count() == 1 {
                        ""
                    } else {
                        "s"
                    }
                ),
                c.text_dim,
            ),
        };
        draw::text(
            scene,
            tr,
            t,
            &sub,
            11.5,
            sub_color,
            title_x,
            bar.y0 + 30.0,
            Some(title_max),
        );
    }

    /// Draw the tab-group pill background and each tab, recording its hit
    /// rect in `self.tab_rects`.
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    fn draw_appbar_tab_group(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        bar: Rect,
        widths: &[f64],
        total: f64,
        tx_start: f64,
    ) {
        let c = &theme.tokens.colors;
        let tab_h = 32.0;
        let tab_y = bar.y0 + (APPBAR_H - tab_h) / 2.0;
        let group_bg = Rect::new(tx_start - 6.0, tab_y, tx_start + total + 6.0, tab_y + tab_h);
        draw::fill_round(scene, t, group_bg, 9.0, c.surfaces.base);
        let mut tx = tx_start;
        for (i, (screen, label)) in TABS.iter().enumerate() {
            let Some(&w) = widths.get(i) else {
                continue;
            };
            let trect = Rect::new(tx, tab_y, tx + w, tab_y + tab_h);
            let active = *screen == self.screen;
            if active {
                draw::fill_round(scene, t, trect, 7.0, theme.accent_primary);
            }
            let fg = if active { Color::WHITE } else { c.text_dim };
            let lw = f64::from(tr.measure_weighted(label, 13.0, None, TextWeight::Medium).0);
            draw::text_weighted(
                scene,
                tr,
                t,
                label,
                13.0,
                fg,
                tx + (w - lw) / 2.0,
                tab_y + 8.0,
                None,
                TextWeight::Medium,
            );
            self.tab_rects.push((*screen, trect));
            tx += w + 6.0;
        }
    }

    fn draw_settings_cluster(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        bar: Rect,
        cluster_w: f64,
    ) {
        let c = &theme.tokens.colors;
        let x = bar.x1 - cluster_w;
        let y = bar.y0 + (APPBAR_H - 32.0) / 2.0;
        // Diff-layout segmented control.
        let seg_w = 150.0;
        let seg = Rect::new(x, y, x + seg_w, y + 32.0);
        draw::fill_round(scene, t, seg, 9.0, c.surfaces.base);
        let half = seg_w / 2.0;
        let unified_active = self.settings.diff_layout == DiffLayout::Unified;
        let active_rect = if unified_active {
            Rect::new(seg.x0 + 3.0, seg.y0 + 3.0, seg.x0 + half, seg.y1 - 3.0)
        } else {
            Rect::new(seg.x0 + half, seg.y0 + 3.0, seg.x1 - 3.0, seg.y1 - 3.0)
        };
        draw::fill_round(scene, t, active_rect, 7.0, theme.accent_primary);
        for (i, label) in ["Unified", "Split"].iter().enumerate() {
            let active = (i == 0) == unified_active;
            let fg = if active { Color::WHITE } else { c.text_dim };
            let cx = seg.x0 + half * i as f64;
            let lw = f64::from(tr.measure_weighted(label, 12.0, None, TextWeight::Medium).0);
            draw::text_weighted(
                scene,
                tr,
                t,
                label,
                12.0,
                fg,
                cx + (half - lw) / 2.0,
                y + 8.0,
                None,
                TextWeight::Medium,
            );
        }
        // Note: the segmented control's click rect is stored as the whole
        // seg; the click handler flips the layout.
        self.settings_seg = seg;

        // Syntax-highlight toggle. Keep the visible label explicit: this is a
        // preference, not a repository status, and "Syn" is ambiguous when a
        // control is viewed without its tooltip or action palette.
        let hx = seg.x1 + 10.0;
        let hl = Rect::new(hx, y, hx + 64.0, y + 32.0);
        let on = self.settings.syntax_highlight;
        draw::fill_round(
            scene,
            t,
            hl,
            9.0,
            if on {
                theme.accent_primary
            } else {
                c.surfaces.base
            },
        );
        let hlfg = if on { Color::WHITE } else { c.text_dim };
        let label = "Syntax";
        let lw = f64::from(tr.measure_weighted(label, 12.0, None, TextWeight::Medium).0);
        draw::text_weighted(
            scene,
            tr,
            t,
            label,
            12.0,
            hlfg,
            hl.x0 + (hl.width() - lw) / 2.0,
            y + 8.0,
            None,
            TextWeight::Medium,
        );
        self.settings_syn = hl;
    }

    /// Draw the hamburger toggle at the left of the app bar and record its hit
    /// rect. Highlighted while the flap is open so it reads as an active
    /// toggle. Three stacked bars — the universal "open the menu" affordance.
    fn draw_hamburger(&mut self, scene: &mut Scene, t: Affine, theme: &Theme, bar: Rect) {
        let c = &theme.tokens.colors;
        let size = 32.0;
        let bx = bar.x0 + 12.0;
        let by = bar.y0 + (APPBAR_H - size) / 2.0;
        let btn = Rect::new(bx, by, bx + size, by + size);
        let open = self.flap_open();
        draw::fill_round(
            scene,
            t,
            btn,
            8.0,
            if open {
                theme.accent_primary
            } else {
                c.surfaces.base
            },
        );
        let fg = if open { Color::WHITE } else { c.text_dim };
        // Three bars.
        let inset = 8.0;
        let x0 = btn.x0 + inset;
        let x1 = btn.x1 - inset;
        for i in 0..3 {
            let ly = btn.y0 + 10.0 + f64::from(i) * 6.0;
            let line = Rect::new(x0, ly, x1, ly + 2.0);
            draw::fill_round(scene, t, line, 1.0, fg);
        }
        self.hamburger_rect = btn;
    }

    /// Resolve a master-detail screen's geometry responsively from the flap.
    ///
    /// Returns `(list_rect, detail_rect, list_visible)`. Wide (docked): the
    /// list column sits beside the detail, which is inset to make room; the
    /// reveal eases the column's width so a toggle is smooth. Narrow
    /// (overlay): the detail spans the full body and the list column slides
    /// in over it from the left edge (the caller paints the scrim between the
    /// two via [`Self::draw_overlay_scrim`]). The list is hidden entirely
    /// while collapsed so it doesn't paint behind a zero-width column.
    fn master_detail_rects(&self, body: Rect, list_w: f64) -> (Rect, Rect, bool) {
        match self.layout_mode() {
            SplitLayout::Docked => {
                let inset = list_w * self.flap.reveal();
                let col = Rect::new(body.x0, body.y0, body.x0 + inset, body.y1);
                let detail = Rect::new(col.x1, body.y0, body.x1, body.y1);
                (col, detail, inset > 0.5)
            }
            SplitLayout::Overlay => {
                let slide = self.flap.slide();
                let x0 = body.x0 - list_w * (1.0 - slide);
                let col = Rect::new(x0, body.y0, x0 + list_w, body.y1);
                (col, body, self.flap.reveal() > 0.001)
            }
        }
    }

    /// Paint the dimming scrim over `body` in overlay mode, sized to the flap
    /// reveal. A no-op when docked or fully closed. Drawn after the detail and
    /// before the list column so the column floats above the dimmed content.
    fn draw_overlay_scrim(&self, scene: &mut Scene, t: Affine, body: Rect) {
        if self.layout_mode() != SplitLayout::Overlay {
            return;
        }
        let reveal = self.flap.reveal();
        if reveal <= 0.001 {
            return;
        }
        let scrim = Color::from_rgba8(0, 0, 0, 255).with_alpha_factor((reveal * 0.5) as f32);
        scene.fill(Fill::NonZero, t, scrim, None, &body);
    }

    fn draw_changes(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        presence: &PresenceCtx,
        body: Rect,
    ) {
        let (col, pane, list_visible) = self.master_detail_rects(body, CHANGES_COL_W);
        // Detail (diff) first — it sits behind the overlay flap.
        self.draw_diff_pane(scene, tr, t, theme, pane);
        // Scrim between detail and list (overlay mode only).
        self.draw_overlay_scrim(scene, t, body);
        if list_visible {
            let msg = self.commit_input.value().to_string();
            let out = ChangesColumn::draw(
                scene,
                tr,
                t,
                theme,
                col,
                &self.model,
                self.selected_change,
                self.changes_scroll,
                &msg,
            );
            self.change_hits = out.hits;
            // Position and draw the commit-message TextInput in the reserved
            // rect — the toolkit input owns its frame, text, caret and
            // selection so editing is a first-class ui component.
            self.commit_input.apply_layout_external(out.commit_input);
            self.commit_input.draw(scene, t, tr, theme, presence);
        } else {
            self.change_hits.clear();
            self.commit_input.apply_layout_external(Rect::ZERO);
        }
    }

    /// Paint the diff detail pane (header + diff body) into `pane`.
    fn draw_diff_pane(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        pane: Rect,
    ) {
        let header = Rect::new(pane.x0, pane.y0, pane.x1, pane.y0 + 36.0);
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, header, c.surfaces.raised);
        draw::hline(scene, t, header.y1, header.x0, header.x1, c.border_standard);

        let key = self.current_diff_key();
        // Pull the header facts out as owned values so the `self.diffs` borrow
        // ends before we mutate `self` (discard rect) or re-borrow for the body.
        let info = key.as_ref().and_then(|k| self.diffs.get(k)).map(|d| {
            (
                d.old_path.clone(),
                d.path.clone(),
                d.additions(),
                d.deletions(),
            )
        });
        self.discard_file_rect = Rect::ZERO;
        let staged = self.selected_is_staged();

        if let Some((old_path, path, adds, dels)) = info {
            self.draw_diff_header_file(
                scene,
                tr,
                t,
                theme,
                header,
                old_path.as_deref(),
                path,
                adds,
                dels,
                staged,
            );
        } else {
            draw::text(
                scene,
                tr,
                t,
                "Select a file to view its diff",
                13.0,
                c.text_dim,
                header.x0 + 16.0,
                header.y0 + 9.0,
                None,
            );
        }

        self.draw_diff_body(scene, tr, t, theme, pane, header, key.as_ref(), staged);
    }

    /// Draw the diff-pane header for the selected file: title, +/− summary,
    /// and (on the worktree side of a real repo) the two-click "Discard
    /// file" affordance.
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    fn draw_diff_header_file(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        header: Rect,
        old_path: Option<&str>,
        path: String,
        adds: u32,
        dels: u32,
        staged: bool,
    ) {
        let c = &theme.tokens.colors;
        let title = match old_path {
            Some(old) => format!("{old}  →  {path}"),
            None => path,
        };
        let summary = format!("+{adds}  −{dels}");
        let aw = f64::from(draw::mono_width(tr, &summary, 12.0));
        draw::mono(
            scene,
            tr,
            t,
            &summary,
            12.0,
            c.text_dim,
            header.x1 - aw - 16.0,
            header.y0 + 10.0,
        );

        // Discard-file affordance on the worktree (unstaged / untracked)
        // side of a real repo. Two-click armed: shows "Discard file?" when
        // armed, filled danger.
        let mut title_max = (header.width() - 160.0) as f32;
        let can_discard = self.repo_path.is_some() && !staged && self.selected_change.is_some();
        if can_discard {
            title_max = self.draw_discard_file_button(scene, tr, t, theme, header, aw);
        }
        draw::text_weighted(
            scene,
            tr,
            t,
            &title,
            13.0,
            c.text_primary,
            header.x0 + 16.0,
            header.y0 + 9.0,
            Some(title_max),
            TextWeight::Medium,
        );
    }

    /// Draw the "Discard file" / (armed) "Discard file?" button and return
    /// the title's clamped max width now that the button occupies space.
    fn draw_discard_file_button(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        header: Rect,
        summary_w: f64,
    ) -> f32 {
        let c = &theme.tokens.colors;
        let armed = self.armed_discard == Some(DiscardTarget::File);
        let label = if armed {
            "Discard file?"
        } else {
            "Discard file"
        };
        let lw = f64::from(tr.measure_weighted(label, 11.5, None, TextWeight::Medium).0);
        let bw = lw + 20.0;
        let bx = header.x1 - summary_w - 16.0 - 12.0 - bw;
        let brect = Rect::new(bx, header.y0 + 6.0, bx + bw, header.y1 - 6.0);
        let bg = if armed {
            c.semantic.danger
        } else {
            c.lift(0.72)
        };
        let fg = if armed {
            Color::WHITE
        } else {
            c.semantic.danger
        };
        draw::fill_round(scene, t, brect, 6.0, bg);
        draw::text_weighted(
            scene,
            tr,
            t,
            label,
            11.5,
            fg,
            bx + 10.0,
            header.y0 + 10.0,
            None,
            TextWeight::Medium,
        );
        self.discard_file_rect = brect;
        (bx - header.x0 - 24.0).max(40.0) as f32
    }

    /// Draw the diff body below the header: the hunk view for the selected
    /// file, or an empty pane when nothing is selected.
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    fn draw_diff_body(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        pane: Rect,
        header: Rect,
        key: Option<&DiffKey>,
        staged: bool,
    ) {
        let c = &theme.tokens.colors;
        let viewport = Rect::new(pane.x0, header.y1, pane.x1, pane.y1);
        let armed_hunk = match self.armed_discard {
            Some(DiscardTarget::Hunk(h)) => Some(h),
            _ => None,
        };
        if let Some(diff) = key.and_then(|k| self.diffs.get(k)) {
            self.diff_hunk_buttons = DiffView::draw(
                scene,
                tr,
                t,
                theme,
                viewport,
                diff,
                self.diff_scroll,
                &self.settings,
                self.hover_hunk,
                staged,
                armed_hunk,
            );
        } else {
            draw::fill_rect(scene, t, viewport, c.surfaces.base);
            self.diff_hunk_buttons.clear();
        }
    }

    fn current_path(&self) -> Option<String> {
        let (area, idx) = self.selected_change?;
        let list = match area {
            ChangeArea::Staged => &self.model.staged,
            ChangeArea::Unstaged => &self.model.unstaged,
            ChangeArea::Untracked => &self.model.untracked,
        };
        list.get(idx).map(|f| f.path.clone())
    }

    fn draw_history(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        body: Rect,
    ) {
        match self.layout_mode() {
            SplitLayout::Docked => {
                // Wide: the commit list keeps its generous ~56% split, eased
                // in/out by the flap reveal so a toggle slides smoothly.
                let full_split = (body.width() * 0.56).max(420.0).min(body.width() - 320.0);
                let split = body.x0 + full_split * self.flap.reveal();
                let detail = Rect::new(split, body.y0, body.x1, body.y1);
                if let Some(commit) = self.model.commits.get(self.selected_commit) {
                    HistoryView::draw_detail(
                        scene,
                        tr,
                        t,
                        theme,
                        detail,
                        commit,
                        self.detail_scroll,
                    );
                }
                if self.flap.reveal() > 0.001 {
                    let list = Rect::new(body.x0, body.y0, split, body.y1);
                    self.history_rows = HistoryView::draw_list(
                        scene,
                        tr,
                        t,
                        theme,
                        list,
                        &self.model,
                        &self.graph_layout,
                        self.selected_commit,
                        self.history_scroll,
                    );
                } else {
                    self.history_rows.clear();
                }
            }
            SplitLayout::Overlay => {
                // Narrow: detail spans full width; the commit list overlays it
                // as a fixed-width flap behind a scrim.
                let detail = body;
                if let Some(commit) = self.model.commits.get(self.selected_commit) {
                    HistoryView::draw_detail(
                        scene,
                        tr,
                        t,
                        theme,
                        detail,
                        commit,
                        self.detail_scroll,
                    );
                }
                self.draw_overlay_scrim(scene, t, body);
                if self.flap.reveal() > 0.001 {
                    let slide = self.flap.slide();
                    let x0 = body.x0 - FLAP_W * (1.0 - slide);
                    let list = Rect::new(x0, body.y0, x0 + FLAP_W, body.y1);
                    self.history_rows = HistoryView::draw_list(
                        scene,
                        tr,
                        t,
                        theme,
                        list,
                        &self.model,
                        &self.graph_layout,
                        self.selected_commit,
                        self.history_scroll,
                    );
                } else {
                    self.history_rows.clear();
                }
            }
        }
    }

    fn draw_panels(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        presence: &PresenceCtx,
        body: Rect,
    ) {
        self.panel_hits =
            PanelsView::draw(scene, tr, t, theme, body, &self.model, self.hover_action);
        // New-branch creator: a compact labelled input + button drawn in the
        // toolbar's centre band, between the branch status (left) and the
        // fetch/pull/push cluster (right). Only meaningful for a real repo.
        if self.repo_path.is_some() {
            self.draw_new_branch(scene, tr, t, theme, presence, body);
        } else {
            self.new_branch_input = Rect::ZERO;
            self.new_branch_button = Rect::ZERO;
            self.branch_input.apply_layout_external(Rect::ZERO);
        }
    }

    /// Draw the new-branch input + "Create" button in the panels toolbar and
    /// record their hit rects. The toolbar is the top 56px band of `body`.
    fn draw_new_branch(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        presence: &PresenceCtx,
        body: Rect,
    ) {
        let c = &theme.tokens.colors;
        let tb = Rect::new(body.x0, body.y0, body.x1, body.y0 + 56.0);
        // Reserve room for the 3 action buttons (84*3 + gaps + margins ≈ 300).
        let actions_w = 300.0;
        let btn_w = 70.0;
        let gap = 8.0;
        let h = 28.0;
        let y = tb.y0 + (tb.height() - h) / 2.0;
        let right = tb.x1 - actions_w;
        let bx = right - btn_w;
        let input = Rect::new(
            (right - btn_w - gap - 180.0).max(tb.x0 + 180.0),
            y,
            bx - gap,
            y + h,
        );
        if input.width() < 60.0 {
            // Too tight to render meaningfully — skip.
            self.new_branch_input = Rect::ZERO;
            self.new_branch_button = Rect::ZERO;
            self.branch_input.apply_layout_external(Rect::ZERO);
            return;
        }
        // Input field — the toolkit TextInput draws its own frame + caret.
        self.branch_input.apply_layout_external(input);
        self.branch_input.draw(scene, t, tr, theme, presence);
        self.new_branch_input = input;
        // Create button.
        let brect = Rect::new(bx, y, bx + btn_w, y + h);
        let enabled = !self.branch_input.value().trim().is_empty();
        draw::fill_round(
            scene,
            t,
            brect,
            6.0,
            if enabled {
                theme.accent_primary
            } else {
                c.lift(0.72)
            },
        );
        let fg = if enabled { Color::WHITE } else { c.neutral[5] };
        let lw = f64::from(
            tr.measure_weighted("Branch", 12.0, None, TextWeight::Medium)
                .0,
        );
        draw::text_weighted(
            scene,
            tr,
            t,
            "Branch",
            12.0,
            fg,
            bx + (btn_w - lw) / 2.0,
            y + 6.0,
            None,
            TextWeight::Medium,
        );
        self.new_branch_button = brect;
    }

    fn create_new_branch(&mut self) {
        let name = self.branch_input.value().trim().to_string();
        if name.is_empty() {
            return;
        }
        if let Some(path) = self.repo() {
            let res = crate::repo::create_branch(&path, &name, None, true);
            let ok = res.is_ok();
            self.run_action(res);
            if ok {
                self.branch_input.set_text("");
                self.set_focus(None);
            }
        }
    }

    /// Draw the empty "open a repository" state: a centered card with a path
    /// field (toolkit `TextInput`) and an Open button. This is the honest
    /// default when no repository is reachable — no placeholder repo data.
    fn draw_open(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, self.rect, c.surfaces.base);

        let cw = 520.0_f64.min(self.rect.width() - 48.0);
        let cx = self.rect.x0 + (self.rect.width() - cw) / 2.0;
        let cy = self.rect.y0 + (self.rect.height() * 0.30).max(80.0);

        let y = Self::draw_open_intro(scene, tr, t, theme, cx, cw, cy);
        self.draw_open_path_field(scene, tr, t, theme, presence, cx, cw, y);
    }

    /// Draw the branch-glyph mark, title and subtitle above the path field.
    /// Returns the y position the path field should start at.
    fn draw_open_intro(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        cx: f64,
        cw: f64,
        cy: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        // Branch glyph mark.
        let mark = Rect::new(cx + cw / 2.0 - 22.0, cy, cx + cw / 2.0 + 22.0, cy + 44.0);
        draw::fill_round(scene, t, mark, 12.0, c.accent[2]);
        let gw = f64::from(tr.measure_weighted("⑂", 24.0, None, TextWeight::Bold).0);
        draw::text_weighted(
            scene,
            tr,
            t,
            "⑂",
            24.0,
            theme.accent_primary,
            mark.x0 + (mark.width() - gw) / 2.0,
            mark.y0 + 8.0,
            None,
            TextWeight::Bold,
        );

        let mut y = cy + 64.0;
        let title = "No repository open";
        let tw = f64::from(tr.measure_weighted(title, 20.0, None, TextWeight::Bold).0);
        draw::text_weighted(
            scene,
            tr,
            t,
            title,
            20.0,
            c.text_primary,
            cx + (cw - tw) / 2.0,
            y,
            None,
            TextWeight::Bold,
        );
        y += 30.0;
        let sub = "Open a git repository to view its changes, history and branches.";
        let sw = f64::from(tr.measure(sub, 13.0, None).0);
        draw::text(
            scene,
            tr,
            t,
            sub,
            13.0,
            c.text_dim,
            cx + (cw - sw).max(0.0) / 2.0,
            y,
            Some(cw as f32),
        );
        y + 40.0
    }

    /// Draw the repository path field, Open button, and any open error
    /// below the intro block drawn by [`Self::draw_open_intro`].
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    fn draw_open_path_field(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        presence: &PresenceCtx,
        cx: f64,
        cw: f64,
        mut y: f64,
    ) {
        let c = &theme.tokens.colors;
        // Path field + Open button.
        let btn_w = 92.0;
        let gap = 10.0;
        let field_h = 40.0;
        let field = Rect::new(cx, y, cx + cw - btn_w - gap, y + field_h);
        self.open_input.apply_layout_external(field);
        self.open_input.draw(scene, t, tr, theme, presence);
        self.open_input_rect = field;
        let brect = Rect::new(field.x1 + gap, y, cx + cw, y + field_h);
        let enabled = !self.open_input.value().trim().is_empty();
        draw::fill_round(
            scene,
            t,
            brect,
            8.0,
            if enabled {
                theme.accent_primary
            } else {
                c.lift(0.72)
            },
        );
        let fg = if enabled { Color::WHITE } else { c.neutral[5] };
        let ow = f64::from(
            tr.measure_weighted("Open", 14.0, None, TextWeight::Semibold)
                .0,
        );
        draw::text_weighted(
            scene,
            tr,
            t,
            "Open",
            14.0,
            fg,
            brect.x0 + (brect.width() - ow) / 2.0,
            brect.y0 + 11.0,
            None,
            TextWeight::Semibold,
        );
        self.open_button_rect = brect;

        // Any open error (e.g. "No git repository at …").
        if let Some(err) = &self.last_error {
            y += field_h + 16.0;
            let ew = f64::from(tr.measure(err, 12.5, None).0);
            draw::text(
                scene,
                tr,
                t,
                err,
                12.5,
                c.semantic.danger,
                cx + (cw - ew).max(0.0) / 2.0,
                y,
                Some(cw as f32),
            );
        }
    }

    // --- Event helpers -------------------------------------------------

    /// Switch the active screen and reconcile the flap to the new screen's
    /// layout mode. Switching to a full-width screen (Branches & Remotes) or
    /// between master-detail screens may change whether a flap exists, so the
    /// reconcile re-applies the right default for the destination.
    fn switch_screen(&mut self, screen: Screen) {
        self.screen = screen;
        self.set_focus(None);
        self.armed_discard = None;
        self.flap.reconcile(self.layout_mode());
    }

    fn handle_click(&mut self, pos: Point, doc: &mut AutoCommit) {
        // Hamburger toggle (overlay mode only) summons / dismisses the flap.
        if self.hamburger_rect.width() > 0.0 && self.hamburger_rect.contains(pos) {
            self.flap.toggle(self.layout_mode());
            return;
        }
        // While the overlay flap is open, a click on the scrim (the dimmed
        // detail to the right of the list column) dismisses it — and is
        // consumed so it doesn't also hit the content behind.
        if self.layout_mode() == SplitLayout::Overlay && self.flap_open() {
            let col_w = match self.screen {
                Screen::History => FLAP_W,
                _ => CHANGES_COL_W,
            };
            let on_sidebar = pos.x <= self.rect.x0 + col_w;
            if !on_sidebar {
                self.flap.close();
                return;
            }
        }
        // Tabs.
        for (screen, rect) in &self.tab_rects {
            if rect.contains(pos) {
                self.switch_screen(*screen);
                return;
            }
        }
        // Settings segmented control + syntax toggle — durable preferences,
        // dispatched as actions so they undo and replicate.
        if self.settings_seg.contains(pos) {
            self.toggle_diff_layout(doc);
            return;
        }
        if self.settings_syn.contains(pos) {
            self.toggle_syntax_highlight(doc);
            return;
        }

        match self.screen {
            Screen::Changes => self.handle_changes_click(pos),
            Screen::History => self.handle_history_click(pos),
            Screen::Panels => self.handle_panels_click(pos),
        }
    }

    fn handle_changes_click(&mut self, pos: Point) {
        // Discard-file button (diff header) — armed two-click confirm.
        if self.discard_file_rect.width() > 0.0 && self.discard_file_rect.contains(pos) {
            if self.armed_discard == Some(DiscardTarget::File) {
                if let Some((area, idx)) = self.selected_change {
                    self.discard_file(area, idx);
                }
            } else {
                self.armed_discard = Some(DiscardTarget::File);
            }
            return;
        }
        // Hunk action buttons take priority (they overlap the diff body).
        if let Some((hunk, action)) = diffview::hunk_action_at(&self.diff_hunk_buttons, pos) {
            match action {
                diffview::HunkAction::Toggle => {
                    self.armed_discard = None;
                    self.stage_hunk(hunk);
                }
                diffview::HunkAction::Discard => {
                    if self.armed_discard == Some(DiscardTarget::Hunk(hunk)) {
                        self.discard_hunk(hunk);
                    } else {
                        self.armed_discard = Some(DiscardTarget::Hunk(hunk));
                    }
                }
            }
            return;
        }
        // Resolve against the changes column regions.
        let mut hit: Option<ChangesHit> = None;
        for f in &self.change_hits {
            if let Some(h) = f(pos) {
                hit = Some(h);
                break;
            }
        }
        // Any other interaction disarms a pending discard.
        self.armed_discard = None;
        let Some(hit) = hit else {
            return;
        };
        match hit {
            ChangesHit::SelectFile(area, idx) => {
                self.selected_change = Some((area, idx));
                self.diff_scroll = 0.0;
            }
            ChangesHit::ToggleStage(area, idx) => self.toggle_stage(area, idx),
            ChangesHit::StageAll => self.stage_all(),
            ChangesHit::UnstageAll => self.unstage_all(),
            ChangesHit::Commit => self.do_commit(),
            ChangesHit::FocusMessage => self.set_focus(Some(Field::Commit)),
        }
    }

    /// Stage (or unstage) a single hunk of the selected file. Against a real
    /// repo this applies exactly that hunk to the index via `git2` (the
    /// equivalent of `git add -p` / `git restore --staged -p`) and re-reads.
    /// The `hunk` argument is the hunk's index within the file's diff, which
    /// the diff view assigns its button to.
    ///
    /// The currently shown diff direction depends on which area is selected:
    /// a staged file shows the HEAD→index diff (so a hunk action *unstages*),
    /// an unstaged/untracked file shows the index→workdir diff (so a hunk
    /// action *stages*). That matches `apply_hunk`'s ordering exactly.
    fn stage_hunk(&mut self, hunk: usize) {
        let Some((area, _)) = self.selected_change else {
            return;
        };
        if let (Some(path), Some(file)) = (self.repo(), self.current_path()) {
            let unstage = area == ChangeArea::Staged;
            // Map the displayed line's hunk to the Nth diff hunk for the file.
            let local = Self::local_hunk_index(&file, hunk);
            let res = crate::repo::apply_hunk(&path, &file, local, unstage);
            self.run_action(res);
            return;
        }
        // Sample fallback: move the whole file to prove the hit-test.
        if let Some((area, idx)) = self.selected_change {
            self.toggle_stage_in_memory(area, idx);
        }
    }

    /// The 0-based hunk position (within the file's diff, excluding meta
    /// "hunk 0 = file header" noise) for the diff line's `hunk` field. The
    /// parsed `DiffLine.hunk` is already a 0-based index into the file's
    /// `DiffHunk` list, so it maps directly; this helper exists so the
    /// mapping is named and testable if the parser's convention shifts.
    fn local_hunk_index(file: &str, hunk: usize) -> usize {
        // `hunk` indexes `FileDiff.hunks`; libgit2's hunk callback walks the
        // same hunks in the same order, so they're 1:1.
        let _ = file;
        hunk
    }

    fn handle_history_click(&mut self, pos: Point) {
        for (i, rect) in self.history_rows.iter().enumerate() {
            if rect.contains(pos) {
                self.selected_commit = i;
                self.detail_scroll = 0.0;
                return;
            }
        }
    }

    fn handle_panels_click(&mut self, pos: Point) {
        // Create button (the input's own focus is handled before this router).
        if self.new_branch_button.width() > 0.0 && self.new_branch_button.contains(pos) {
            self.create_new_branch();
            return;
        }
        let mut hit: Option<PanelHit> = None;
        for f in &self.panel_hits {
            if let Some(action) = f(pos) {
                hit = Some(action);
                break;
            }
        }
        match hit {
            Some(PanelHit::Remote(a)) => self.apply_remote_action(a),
            Some(PanelHit::Branch(a)) => self.apply_branch_action(a),
            None => {}
        }
    }

    /// Apply a remote action (fetch / pull / push) against the live repo via
    /// `git2`, then re-read the model so the ahead/behind counters reflect
    /// the real tracking state. For the sample, reconcile the counters so the
    /// offscreen capture shows the effect.
    fn apply_remote_action(&mut self, action: RemoteAction) {
        if let Some(path) = self.repo() {
            let res = match action {
                RemoteAction::Fetch => crate::repo::fetch(&path, None).map(|_| ()),
                RemoteAction::Pull => crate::repo::pull(&path, None),
                RemoteAction::Push => crate::repo::push(&path, None),
            };
            self.run_action(res);
            return;
        }
        if let Some(b) = self.model.branches.iter_mut().find(|b| b.is_head) {
            match action {
                RemoteAction::Fetch => {}
                RemoteAction::Pull => b.behind = 0,
                RemoteAction::Push => b.ahead = 0,
            }
        }
    }

    /// Apply a branch action (checkout / create / delete) or a stash apply
    /// against the live repo, then re-read. Sample-backed apps ignore these
    /// (the panels' branch buttons only appear for real repos).
    fn apply_branch_action(&mut self, action: BranchAction) {
        let Some(path) = self.repo() else { return };
        let res = match action {
            BranchAction::Checkout(name) => crate::repo::checkout_branch(&path, &name),
            BranchAction::Delete(name) => crate::repo::delete_branch(&path, &name),
            BranchAction::StashApply(i) => crate::repo::stash_apply(&path, i),
        };
        self.run_action(res);
    }

    fn scroll_active(&mut self, dy: f64, pos: Point) {
        // When the list is an open overlay flap it captures the wheel over its
        // own column; the detail behind it scrolls only when the flap is shut.
        let overlay_open = self.layout_mode() == SplitLayout::Overlay && self.flap_open();
        match self.screen {
            Screen::Changes => {
                // The list column occupies the left `CHANGES_COL_W` strip
                // whether docked or an open overlay; scroll it there.
                let over_list = (overlay_open || self.layout_mode() == SplitLayout::Docked)
                    && pos.x < self.rect.x0 + CHANGES_COL_W
                    && self.flap_open();
                if over_list {
                    self.changes_scroll = (self.changes_scroll - dy).max(0.0);
                } else {
                    let max = self.current_diff().map_or(0.0, DiffView::content_height)
                        - (self.rect.height() - APPBAR_H - 36.0);
                    self.diff_scroll = (self.diff_scroll - dy).clamp(0.0, max.max(0.0));
                }
            }
            Screen::History => {
                let list_w = if overlay_open {
                    FLAP_W
                } else {
                    (self.rect.width() * 0.56).max(420.0)
                };
                let over_list = pos.x < self.rect.x0 + list_w && self.flap_open();
                if over_list {
                    let max =
                        HistoryView::content_height(&self.model) - (self.rect.height() - APPBAR_H);
                    self.history_scroll = (self.history_scroll - dy).clamp(0.0, max.max(0.0));
                } else {
                    self.detail_scroll = (self.detail_scroll - dy).max(0.0);
                }
            }
            Screen::Panels => {}
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "this is the single derived semantic inventory for the app's three mutually exclusive screens"
    )]
    fn accessibility_targets(&self) -> Vec<(Rect, ui::a11y::AccessNode)> {
        use ui::a11y::{AccessNode, AccessState, Role};

        let button = |rect: Rect, label: String| {
            (
                rect,
                AccessNode::new(Role::Button)
                    .with_name(label)
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(rect),
            )
        };
        if self.awaiting_repo {
            let mut targets = Vec::new();
            if self.open_input_rect.area() > 0.0 {
                targets.push((
                    self.open_input_rect,
                    self.open_input
                        .accessibility()
                        .unwrap_or_else(|| {
                            AccessNode::new(Role::TextInput).with_name("Repository path")
                        })
                        .with_bounds(self.open_input_rect),
                ));
            }
            if self.open_button_rect.area() > 0.0 {
                targets.push(button(self.open_button_rect, "Open repository".into()));
            }
            return targets;
        }

        let mut targets = Vec::new();
        if self.hamburger_rect.area() > 0.0 {
            targets.push(button(
                self.hamburger_rect,
                "Show repository navigation".into(),
            ));
        }
        targets.extend(self.tab_rects.iter().map(|(screen, rect)| {
            let label = TABS
                .iter()
                .find(|(candidate, _)| candidate == screen)
                .map_or("Repository view", |(_, label)| *label);
            (
                *rect,
                AccessNode::new(Role::Tab)
                    .with_name(label)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(*screen == self.screen),
                    )
                    .with_bounds(*rect),
            )
        }));
        if self.settings_seg.area() > 0.0 {
            targets.push(button(self.settings_seg, "Diff layout".into()));
        }
        if self.settings_syn.area() > 0.0 {
            targets.push(button(self.settings_syn, "Syntax highlighting".into()));
        }
        match self.screen {
            Screen::Changes if self.commit_input.get_rect().area() > 0.0 => targets.push((
                self.commit_input.get_rect(),
                self.commit_input
                    .accessibility()
                    .unwrap_or_else(|| AccessNode::new(Role::TextInput).with_name("Commit message"))
                    .with_bounds(self.commit_input.get_rect()),
            )),
            Screen::History => {
                for (index, rect) in self.history_rows.iter().enumerate() {
                    if let Some(commit) = self.model.commits.get(index) {
                        targets.push((
                            *rect,
                            AccessNode::new(Role::ListItem)
                                .with_name(format!(
                                    "{} by {}, {}",
                                    commit.summary, commit.author_name, commit.when
                                ))
                                .with_state(
                                    AccessState::new()
                                        .focusable(true)
                                        .selected(index == self.selected_commit),
                                )
                                .with_bounds(*rect),
                        ));
                    }
                }
            }
            Screen::Panels => {
                if self.new_branch_input.area() > 0.0 {
                    targets.push((
                        self.new_branch_input,
                        self.branch_input
                            .accessibility()
                            .unwrap_or_else(|| {
                                AccessNode::new(Role::TextInput).with_name("New branch name")
                            })
                            .with_bounds(self.new_branch_input),
                    ));
                }
                if self.new_branch_button.area() > 0.0 {
                    targets.push(button(self.new_branch_button, "Create branch".into()));
                }
            }
            Screen::Changes => {}
        }
        if self.discard_file_rect.area() > 0.0 {
            targets.push(button(
                self.discard_file_rect,
                "Discard file changes".into(),
            ));
        }
        targets
    }
}

/// Whether `p` falls inside a non-empty `rect` (a zero-size rect never hits).
fn rect_hit(rect: Rect, p: Point) -> bool {
    rect.width() > 0.0 && rect.contains(p)
}

fn first_change(model: &RepoModel) -> Option<(ChangeArea, usize)> {
    if !model.staged.is_empty() {
        Some((ChangeArea::Staged, 0))
    } else if !model.unstaged.is_empty() {
        Some((ChangeArea::Unstaged, 0))
    } else if !model.untracked.is_empty() {
        Some((ChangeArea::Untracked, 0))
    } else {
        None
    }
}

impl Widget for GitClientApp {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::GIT_CLIENT_OPERATION_TYPE]
    }

    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if self.settings_seeded {
            // Adopt document changes: remote collaborators, and this app's own
            // preference actions, which the framework `ActionHost` applies
            // straight to the document through the executor.
            self.settings = Settings::read(state);
        } else {
            // First sync: adopt any persisted preferences, then seed the doc so
            // it is the source of truth and collaborators see the same
            // preferences. An initial override forced at construction (the
            // GIT_CLIENT_SPLIT screenshot path) wins over the persisted value.
            if !self.settings_forced {
                self.settings = Settings::read(state);
            }
            self.settings.write(state);
            self.settings_seeded = true;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The app draws itself; a single full-size leaf node carries it.
        // `new_leaf` is typed `-> TaffyResult<NodeId>` for API generality but
        // cannot actually return `Err` for a fresh leaf (see the
        // `TaffyTreeExt` doc comment in `ui::widget::mod`), and
        // `Widget::build_layout` returns a bare `NodeId` across every
        // implementor, so there is nowhere to surface a theoretical error.
        #[allow(
            clippy::unwrap_used,
            reason = "taffy::TaffyTree::new_leaf cannot return Err; see ui::widget::TaffyTreeExt"
        )]
        let node = taffy
            .new_leaf(Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // Real fallback rather than a panic if `node` somehow doesn't
        // belong to this tree, matching every `ui` widget's `apply_layout`.
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        // The layout mode is now known from the allocated width — reconcile
        // the flap so a window resize across the breakpoint adopts the new
        // mode's default (wide ⇒ docked-open, narrow ⇒ overlay-closed) unless
        // the user expressed an explicit preference this session.
        self.flap.reconcile(self.layout_mode());
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;

        // Honest empty state: no repository bound. Show the open-a-repository
        // screen instead of the workspace chrome.
        if self.awaiting_repo {
            self.draw_open(scene, tr, transform, theme, presence);
            return;
        }

        // Advance the flap reveal toward its target so the slide + scrim fade
        // are smooth. The live loop keeps redrawing while
        // `needs_animation_frame` is true.
        self.flap.reconcile(self.layout_mode());
        self.tick_flap();

        draw::fill_rect(scene, transform, self.rect, c.surfaces.base);

        self.draw_appbar(scene, tr, transform, theme);

        let body = Rect::new(
            self.rect.x0,
            self.rect.y0 + APPBAR_H,
            self.rect.x1,
            self.rect.y1,
        );
        match self.screen {
            Screen::Changes => self.draw_changes(scene, tr, transform, theme, presence, body),
            Screen::History => self.draw_history(scene, tr, transform, theme, body),
            Screen::Panels => self.draw_panels(scene, tr, transform, theme, presence, body),
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
                // Hover state for hunk buttons + remote actions.
                self.hover_hunk =
                    diffview::hunk_button_at(&self.diff_hunk_buttons, self.last_mouse);
                self.hover_action = None;
                for f in &self.panel_hits {
                    if let Some(PanelHit::Remote(a)) = f(self.last_mouse) {
                        self.hover_action = Some(a);
                        break;
                    }
                }
                // Keep an in-flight text-selection drag updating.
                self.forward_to_inputs(event, scale_factor, *modifiers, state, path, taffy);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_press(event, scale_factor, *modifiers, state, path, taffy);
                *handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } => {
                self.forward_to_inputs(event, scale_factor, *modifiers, state, path, taffy);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 36.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
                };
                self.scroll_active(dy, self.last_mouse);
                *handled = true;
            }
            WindowEvent::KeyboardInput { .. } => {
                self.handle_key(event, *modifiers, state, path, taffy, handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "GitClientApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let children = self
            .accessibility_targets()
            .into_iter()
            .enumerate()
            .map(|(index, (_, node))| {
                // Synthetic children need an opaque action target before the
                // bridge advertises them as operable. The ordinal remains the
                // geometry lookup key, so it must stay in lock-step with
                // `synthetic_accessibility_rect` below.
                node.with_key(index as u64)
                    .with_action_target(format!("git-target:{index}"))
            })
            .collect();
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Group)
                .with_name("Git Client")
                .with_description(if self.awaiting_repo {
                    "Open a repository"
                } else {
                    "Repository changes, history, branches and remotes"
                })
                .with_children(children),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else {
            return None;
        };
        self.accessibility_targets()
            .get(*index)
            .map(|(rect, _)| *rect)
    }

    /// Publish the open repository and its view-preference actions to the
    /// shell's Ctrl+K palette (delegates to the [`ResourceProvider`] impl in
    /// [`crate::actions`]). Empty when no repository is open.
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the document heads captured before an on-screen preference
    /// control dispatched, so it records exactly one undo boundary bracketing
    /// it — the same path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    fn invoke_resource_operation(
        &mut self,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if type_id != crate::actions::GIT_CLIENT_OPERATION_TYPE {
            return Ok(false);
        }
        let operation: crate::actions::GitClientOperation =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        match (kind, operation) {
            (qos_actions::OperationKind::View, crate::actions::GitClientOperation::ShowChanges) => {
                self.switch_screen(Screen::Changes);
            }
            (qos_actions::OperationKind::View, crate::actions::GitClientOperation::ShowHistory) => {
                self.switch_screen(Screen::History);
            }
            (
                qos_actions::OperationKind::View,
                crate::actions::GitClientOperation::ShowBranches,
            ) => {
                self.switch_screen(Screen::Panels);
            }
            (qos_actions::OperationKind::Effect, crate::actions::GitClientOperation::StageAll) => {
                self.stage_all();
            }
            (
                qos_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::UnstageAll,
            ) => {
                self.unstage_all();
            }
            (
                qos_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::Commit { message },
            ) => {
                if self.model.staged.is_empty() {
                    return Err("there are no staged changes to commit".into());
                }
                if message.trim().is_empty() {
                    return Err("commit message cannot be empty".into());
                }
                self.commit_input.set_text(&message);
                self.do_commit();
            }
            _ => return Err("operation kind does not match Git operation".into()),
        }
        Ok(true)
    }

    fn needs_animation_frame(&self) -> bool {
        // Redraw while the flap animates, and while any embedded text field is
        // focused so its caret keeps blinking.
        self.flap.is_animating(self.layout_mode())
            || self.commit_input.needs_animation_frame()
            || self.branch_input.needs_animation_frame()
            || self.open_input.needs_animation_frame()
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, tr, theme, presence);
    }
}

impl GitClientApp {
    /// Forward one event to a single embedded [`TextInput`], returning whether
    /// it consumed the event.
    fn fwd(
        input: &mut TextInput,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        let mut handled = false;
        let mut ft: Option<NodeId> = None;
        input.handle_event(
            event,
            scale_factor,
            &modifiers,
            state,
            path,
            &mut handled,
            &mut ft,
            NodeId::new(0),
            taffy,
        );
        handled
    }

    /// Forward a pointer event to every text field (so an in-flight drag-select
    /// keeps updating regardless of which field started it).
    fn forward_to_inputs(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        taffy: &TaffyTree<()>,
    ) {
        Self::fwd(
            &mut self.commit_input,
            event,
            scale_factor,
            modifiers,
            state,
            path,
            taffy,
        );
        Self::fwd(
            &mut self.branch_input,
            event,
            scale_factor,
            modifiers,
            state,
            path,
            taffy,
        );
        Self::fwd(
            &mut self.open_input,
            event,
            scale_factor,
            modifiers,
            state,
            path,
            taffy,
        );
    }

    /// Route a left-press: text fields first (focus + caret placement), then
    /// the geometry click router. Blurs any focused field on a click outside.
    #[allow(clippy::too_many_arguments)]
    fn handle_press(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        taffy: &TaffyTree<()>,
    ) {
        let pos = self.last_mouse;
        // Empty "open a repository" state routes entirely to its own controls.
        if self.awaiting_repo {
            if self.open_button_rect.width() > 0.0 && self.open_button_rect.contains(pos) {
                self.open_repo_from_input();
                return;
            }
            if self.open_input_rect.width() > 0.0 && self.open_input_rect.contains(pos) {
                self.set_focus(Some(Field::Open));
                Self::fwd(
                    &mut self.open_input,
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    taffy,
                );
                return;
            }
            self.set_focus(None);
            return;
        }
        // A press on a visible text field focuses it and places the caret.
        if self.screen == Screen::Changes && rect_hit(self.commit_input.get_rect(), pos) {
            self.set_focus(Some(Field::Commit));
            Self::fwd(
                &mut self.commit_input,
                event,
                scale_factor,
                modifiers,
                state,
                path,
                taffy,
            );
            return;
        }
        if self.screen == Screen::Panels && rect_hit(self.new_branch_input, pos) {
            self.set_focus(Some(Field::Branch));
            Self::fwd(
                &mut self.branch_input,
                event,
                scale_factor,
                modifiers,
                state,
                path,
                taffy,
            );
            return;
        }
        // Anywhere else: blur the fields and run the normal click router.
        self.set_focus(None);
        self.handle_click(pos, state);
    }

    /// Submit the focused field's action (Enter).
    fn submit_focused(&mut self, field: Field) {
        match field {
            Field::Commit => self.do_commit(),
            Field::Branch => self.create_new_branch(),
            Field::Open => self.open_repo_from_input(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_key(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        taffy: &TaffyTree<()>,
        handled: &mut bool,
    ) {
        let WindowEvent::KeyboardInput { event: ev, .. } = event else {
            return;
        };
        if ev.state != ElementState::Pressed {
            return;
        }

        // A focused text field takes the keys, except Enter (submit) and Escape
        // (blur) which the app intercepts. Text editing — cursor, selection,
        // word-nav, clipboard — is all handled by the toolkit TextInput.
        if let Some(field) = self.focus {
            match &ev.logical_key {
                Key::Named(NamedKey::Enter) => {
                    self.submit_focused(field);
                    *handled = true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.set_focus(None);
                    *handled = true;
                }
                _ => {
                    let input: &mut TextInput = match field {
                        Field::Commit => &mut self.commit_input,
                        Field::Branch => &mut self.branch_input,
                        Field::Open => &mut self.open_input,
                    };
                    if Self::fwd(input, event, 1.0, modifiers, state, path, taffy) {
                        *handled = true;
                    }
                }
            }
            return;
        }

        // The empty-state screen has no other keys.
        if self.awaiting_repo {
            return;
        }

        // Esc dismisses an open overlay flap (Adwaita flap behaviour).
        if let Key::Named(NamedKey::Escape) = &ev.logical_key
            && self.layout_mode() == SplitLayout::Overlay
            && self.flap_open()
        {
            self.flap.close();
            *handled = true;
            return;
        }

        // Global shortcuts: 1/2/3 switch screens.
        if let Key::Character(s) = &ev.logical_key {
            match s.as_str() {
                "1" => {
                    self.switch_screen(Screen::Changes);
                    *handled = true;
                }
                "2" => {
                    self.switch_screen(Screen::History);
                    *handled = true;
                }
                "3" => {
                    self.switch_screen(Screen::Panels);
                    *handled = true;
                }
                "d" => {
                    self.toggle_diff_layout(state);
                    *handled = true;
                }
                _ => {}
            }
        }

        // Arrow navigation in the active list.
        match (&ev.logical_key, self.screen) {
            (Key::Named(NamedKey::ArrowDown), Screen::History) => {
                if self.selected_commit + 1 < self.model.commits.len() {
                    self.selected_commit += 1;
                    self.detail_scroll = 0.0;
                }
                *handled = true;
            }
            (Key::Named(NamedKey::ArrowUp), Screen::History) => {
                self.selected_commit = self.selected_commit.saturating_sub(1);
                self.detail_scroll = 0.0;
                *handled = true;
            }
            (Key::Named(NamedKey::ArrowDown), Screen::Changes) => {
                self.select_next_change(1);
                *handled = true;
            }
            (Key::Named(NamedKey::ArrowUp), Screen::Changes) => {
                self.select_next_change(-1);
                *handled = true;
            }
            _ => {}
        }
    }

    fn select_next_change(&mut self, dir: i32) {
        let all = self.model.all_changes().len();
        if all == 0 {
            return;
        }
        // Flatten current selection to a global index.
        let cur = match self.selected_change {
            Some((ChangeArea::Staged, i)) => i,
            Some((ChangeArea::Unstaged, i)) => self.model.staged.len() + i,
            Some((ChangeArea::Untracked, i)) => {
                self.model.staged.len() + self.model.unstaged.len() + i
            }
            None => 0,
        } as i32;
        let next = (cur + dir).clamp(0, all as i32 - 1) as usize;
        // Map back to (area, idx).
        let s = self.model.staged.len();
        let u = self.model.unstaged.len();
        self.selected_change = Some(if next < s {
            (ChangeArea::Staged, next)
        } else if next < s + u {
            (ChangeArea::Unstaged, next - s)
        } else {
            (ChangeArea::Untracked, next - s - u)
        });
        self.diff_scroll = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an app over the bundled sample, laid out at `w` x 800 with the
    /// flap settled to its mode default so geometry is deterministic.
    fn app_at(w: f64, screen: Screen) -> GitClientApp {
        let (model, diffs) = crate::sample::model();
        let mut app = GitClientApp::from_model(model, diffs);
        app.screen = screen;
        app.rect = Rect::new(0.0, 0.0, w, 800.0);
        app.flap.reconcile(app.layout_mode());
        // Settle the reveal to the resolved default for this width.
        app.flap.snap(app.flap_open());
        app
    }

    #[test]
    fn wide_changes_screen_is_docked() {
        let app = app_at(1280.0, Screen::Changes);
        assert_eq!(app.layout_mode(), SplitLayout::Docked);
        assert!(app.flap_open(), "docked list column is open by default");
    }

    #[test]
    fn narrow_master_detail_screen_is_overlay_closed() {
        let app = app_at(680.0, Screen::Changes);
        assert_eq!(app.layout_mode(), SplitLayout::Overlay);
        assert!(!app.flap_open(), "narrow overlay flap is closed by default");
    }

    #[test]
    fn breakpoint_boundary_is_inclusive_docked() {
        assert_eq!(
            app_at(NARROW_BREAKPOINT, Screen::Changes).layout_mode(),
            SplitLayout::Docked,
        );
        assert_eq!(
            app_at(NARROW_BREAKPOINT - 1.0, Screen::Changes).layout_mode(),
            SplitLayout::Overlay,
        );
    }

    #[test]
    fn panels_screen_never_collapses() {
        // The full-width dashboard has no list column, so it stays docked even
        // on a narrow window — no flap, scrim or hamburger.
        let app = app_at(400.0, Screen::Panels);
        assert_eq!(app.layout_mode(), SplitLayout::Docked);
    }

    #[test]
    fn hamburger_click_toggles_overlay_flap() {
        let mut app = app_at(680.0, Screen::Changes);
        assert!(!app.flap_open());
        // Simulate the hamburger being drawn this frame, then click it.
        app.hamburger_rect = Rect::new(12.0, 12.0, 44.0, 44.0);
        app.handle_click(Point::new(28.0, 28.0), &mut AutoCommit::new());
        assert!(app.flap_open(), "hamburger summons the overlay flap");
        app.handle_click(Point::new(28.0, 28.0), &mut AutoCommit::new());
        assert!(!app.flap_open(), "hamburger dismisses it again");
    }

    #[test]
    fn scrim_click_dismisses_open_overlay_flap() {
        let mut app = app_at(680.0, Screen::Changes);
        app.flap.snap(true); // open overlay flap
        assert!(app.flap_open());
        // Click far to the right of the list column (over the dimmed detail).
        app.handle_click(Point::new(600.0, 400.0), &mut AutoCommit::new());
        assert!(!app.flap_open(), "clicking the scrim closes the flap");
    }

    #[test]
    fn click_inside_open_flap_does_not_dismiss_it() {
        let mut app = app_at(680.0, Screen::Changes);
        app.flap.snap(true);
        // A click within the list column must not be treated as a scrim tap.
        app.handle_click(Point::new(40.0, 400.0), &mut AutoCommit::new());
        assert!(
            app.flap_open(),
            "clicks on the list column keep the flap open"
        );
    }

    #[test]
    fn crossing_breakpoint_restores_mode_default() {
        // Open the flap while docked-wide, then narrow past the breakpoint:
        // the stale preference clears and the overlay default (closed) wins.
        let mut app = app_at(1280.0, Screen::Changes);
        app.flap.close(); // user collapses the docked column
        assert!(!app.flap_open());
        app.rect = Rect::new(0.0, 0.0, 680.0, 800.0);
        app.flap.reconcile(app.layout_mode());
        assert!(!app.flap_open(), "overlay default is closed");
        // Widen again → back to docked-open default.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.flap.reconcile(app.layout_mode());
        assert!(app.flap_open(), "docked default restores the open column");
    }

    /// The affordance == `ActionSpec` covenant on the *real* production path:
    /// the app-bar diff-layout control mutates the shared document, records one
    /// undo boundary, and dispatches the byte-identical `EncodedAction` the
    /// Ctrl+K palette would — one source of truth, not two code paths.
    #[test]
    fn appbar_diff_layout_toggle_matches_the_palette_action() {
        let (model, diffs) = crate::sample::model();
        let mut app = GitClientApp::from_model(model, diffs);
        let mut doc = AutoCommit::new();
        // First sync seeds the default (unified) preferences into the document.
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(Settings::read_only(&doc).diff_layout, DiffLayout::Unified);

        // The action Ctrl+K would dispatch right now: flip unified → split.
        let resources = Widget::resources(&app, &doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "diff-layout")
            .unwrap();
        let palette_action = match &spec.input {
            qos_actions::ActionInput::Immediate(enc) => enc.clone(),
            qos_actions::ActionInput::Prompted { .. } => {
                panic!("diff-layout is an immediate action")
            }
        };

        // Drive the real app-bar segmented control.
        app.toggle_diff_layout(&mut doc);

        // It mutated the shared document...
        assert_eq!(
            Settings::read_only(&doc).diff_layout,
            DiffLayout::SideBySide,
            "the control switched the persisted layout"
        );
        // ...recorded exactly one undo boundary bracketing the toggle...
        assert!(
            app.take_committed_action().is_some(),
            "one undo boundary recorded for the control's action"
        );
        // ...and dispatched the byte-identical action Ctrl+K would.
        let dispatched = app
            .last_dispatched()
            .expect("the control dispatched an action");
        assert_eq!(dispatched.type_id, crate::actions::GIT_CLIENT_ACTION_TYPE);
        assert_eq!(
            dispatched, &palette_action,
            "the app-bar control and the Ctrl+K entry build one and the same action"
        );
    }

    #[test]
    fn synthetic_accessibility_controls_are_addressable_at_their_drawn_rects() {
        let mut app = app_at(1_280.0, Screen::Changes);
        app.tab_rects = vec![
            (Screen::Changes, Rect::new(20.0, 8.0, 120.0, 40.0)),
            (Screen::History, Rect::new(124.0, 8.0, 224.0, 40.0)),
        ];
        app.settings_seg = Rect::new(1_000.0, 8.0, 1_080.0, 40.0);
        app.settings_syn = Rect::new(1_084.0, 8.0, 1_160.0, 40.0);
        app.commit_input
            .apply_layout_external(Rect::new(360.0, 700.0, 700.0, 736.0));

        let node = Widget::accessibility(&app).expect("git client is semantic");
        assert!(!node.children.is_empty());
        for (index, child) in node.children.iter().enumerate() {
            let expected_target = format!("git-target:{index}");
            assert_eq!(
                child.action_target.as_deref(),
                Some(expected_target.as_str())
            );
            assert_eq!(
                Widget::synthetic_accessibility_rect(&app, &[index]),
                child.bounds.map(ui::a11y::AccessBounds::to_rect),
                "semantic control {index} must activate inside the visible control"
            );
        }
    }
}
