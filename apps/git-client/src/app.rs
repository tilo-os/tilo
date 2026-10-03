//! Root widget for the git client.
//!
//! Owns the in-memory [`RepoModel`], the diffs, the active screen and all
//! transient selection and scroll state. Durable preferences live in the
//! CRDT via [`Settings`]. The widget paints the window from toolkit faces:
//! one header row holding each column's header (the sidebar toggle and the
//! repository name over the list, the screen switcher and the screen's
//! commands over the content), the list column of the Changes and History
//! screens on the sidebar region through a [`SidebarDrawer`] (docked when the
//! window is wide, a floating drawer when it is narrow), and the screen's
//! content on the window ground. Presses are routed through the targets the
//! last paint recorded.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::toolbar::Orientation;
use ui::widget::{
    Banner, ButtonFace, ControlSize, EmptyState, EmptyStateContent, ModalAction, ModalHit,
    ModalLayout, ModalSpec, SegmentedFace, SidebarDrawer, TextInput, TextInputKind, ToggleGroup,
    Toolbar, Widget,
};

use crate::changes::{self, ChangesHit};
use crate::diffview::{self, DiffHit, HunkAction};
use crate::graph::{self, GraphLayout};
use crate::history;
use crate::model::{ChangeArea, DiffKey, DiffMap, FileChange, RepoModel};
use crate::panels::{self, BranchAction, PanelHit, RemoteAction};
use crate::settings::{DiffLayout, Settings};

/// Which embedded text field currently holds keyboard focus. Text editing is
/// delegated to the toolkit's [`TextInput`] (cursor, selection, word-nav,
/// clipboard); this tracks which one keys route to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    /// The commit-message field.
    Commit,
    /// The new-branch name field on the Branches screen.
    Branch,
    /// The repository-path field on the "open a repository" screen.
    Open,
}

/// Host event resources retained while one event is routed through embedded
/// text inputs and the app's own geometry handlers.
struct InputEventCtx<'a> {
    event: &'a WindowEvent,
    scale_factor: f64,
    modifiers: winit::event::Modifiers,
    state: &'a mut AutoCommit,
    path: &'a ObjId,
    taffy: &'a TaffyTree<()>,
}

/// Which top-level screen is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Changes,
    History,
    Panels,
}

impl Screen {
    const ALL: [Self; 3] = [Self::Changes, Self::History, Self::Panels];

    /// Whether the screen has a list column (on the sidebar region, a
    /// drawer in a narrow window). Branches is one page with no list.
    fn has_list(self) -> bool {
        matches!(self, Screen::Changes | Screen::History)
    }

    fn label(self) -> &'static str {
        match self {
            Screen::Changes => "Changes",
            Screen::History => "History",
            Screen::Panels => "Branches",
        }
    }

    fn target(self) -> &'static str {
        match self {
            Screen::Changes => "git.screen.changes",
            Screen::History => "git.screen.history",
            Screen::Panels => "git.screen.branches",
        }
    }
}

/// A destructive step waiting for the person to confirm it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Confirm {
    /// Revert a file's worktree changes (or delete an untracked file).
    DiscardFile {
        area: ChangeArea,
        index: usize,
        name: String,
    },
    /// Revert one hunk of the selected file's worktree changes.
    DiscardHunk(usize),
    /// Delete a local branch.
    DeleteBranch(String),
}

impl Confirm {
    fn title(&self) -> String {
        match self {
            Confirm::DiscardFile { name, .. } => format!("Discard changes to {name}?"),
            Confirm::DiscardHunk(_) => "Discard this change?".to_string(),
            Confirm::DeleteBranch(name) => format!("Delete {name}?"),
        }
    }

    fn body(&self) -> &'static str {
        match self {
            Confirm::DiscardFile { area, .. } if *area == ChangeArea::Untracked => {
                "The file is deleted from your working tree. This can't be undone."
            }
            Confirm::DiscardFile { .. } | Confirm::DiscardHunk(_) => {
                "Your edits are lost. This can't be undone."
            }
            Confirm::DeleteBranch(_) => "Commits that only this branch holds are lost.",
        }
    }

    fn action(&self) -> &'static str {
        match self {
            Confirm::DiscardFile { .. } | Confirm::DiscardHunk(_) => "Discard",
            Confirm::DeleteBranch(_) => "Delete branch",
        }
    }
}

/// Something a press, a key or an assistive action can do.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Hit {
    /// Show or hide the list column.
    SidebarToggle,
    Screen(Screen),
    DiffLayout(DiffLayout),
    SyntaxHighlight,
    Remote(RemoteAction),
    /// The scrim behind an open drawer: dismisses it.
    Scrim,
    DismissError,
    Change(ChangesHit),
    Diff(DiffHit),
    /// A commit row, by index.
    Commit(usize),
    Panel(PanelHit),
    OpenPath,
    OpenRepository,
}

/// A target the last paint recorded: where, and what it does.
#[derive(Clone, Debug)]
struct Target {
    rect: Rect,
    hit: Hit,
}

/// View state a screenshot scenario asks for, applied once the window has
/// the layout it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Staged {
    Nothing,
    /// Open the list drawer once the window is narrow.
    OpenDrawer,
}

/// What a wheel over a region scrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scroller {
    ChangesList,
    Diff,
    HistoryList,
    Detail,
    Panels,
}

/// One scrollable region of the last paint and how far it may scroll.
#[derive(Clone, Copy, Debug)]
struct ScrollRegion {
    rect: Rect,
    scroller: Scroller,
    max_y: f64,
    max_x: f64,
}

/// One owned repository operation. Completion stays available until its action
/// invocation consumes it, even when a normal UI sync receives it first.
struct RemoteJob {
    receiver: Receiver<Result<(), String>>,
    result: Option<Result<(), String>>,
    invocation: Option<tilo_actions::InvocationId>,
    progress_sent: bool,
    /// Palette label while the transfer is in flight (live hosted extra).
    label: String,
}

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
    actions: crate::resource::ActionSink,

    /// No repository is bound and none was reachable at launch — the app shows
    /// the honest "open a repository" empty state rather than any placeholder
    /// data. Cleared once a repo is opened (via the path field or at startup).
    awaiting_repo: bool,
    /// Which embedded text field holds focus, if any.
    focus: Option<Field>,
    /// Commit-message field (toolkit `TextInput`, local mode).
    commit_input: TextInput,
    /// New-branch name field (toolkit `TextInput`, local mode).
    branch_input: TextInput,
    /// Repository-path field on the empty-state screen (toolkit `TextInput`).
    open_input: TextInput,

    /// Path the repository was opened from, when backed by a real on-disk
    /// repo. `None` for the bundled sample (the `--shots` path), in which
    /// case actions fall back to the in-memory model mutation so the
    /// screenshot still demonstrates the flow. When `Some`, every action
    /// drives `git2` against the real repo and re-reads the model.
    repo_path: Option<std::path::PathBuf>,
    /// Last action error, shown in a banner over the content until dismissed.
    last_error: Option<String>,
    remote_job: Option<RemoteJob>,

    settings: Settings,
    /// Fingerprint of the last shared-repo projection written to the document.
    shared_repo_fingerprint: Option<String>,
    /// Set once after the first sync seeds the settings into the doc.
    settings_seeded: bool,
    /// An initial preference was forced at construction (the split-diff
    /// screenshot scenario), so the first sync keeps it rather than adopting
    /// the document's persisted value. Interactive toggles never set this —
    /// they go through the action executor, which writes the document
    /// directly.
    settings_forced: bool,

    screen: Screen,
    rect: Rect,

    // --- Changes screen state ---
    selected_change: Option<(ChangeArea, usize)>,
    changes_scroll: f64,
    diff_scroll: f64,
    diff_hscroll: f64,

    // --- History screen state ---
    selected_commit: usize,
    history_scroll: f64,
    detail_scroll: f64,

    // --- Branches screen state ---
    panels_scroll: f64,

    /// A destructive step waiting for confirmation, and where its sheet was
    /// laid out last.
    confirm: Option<Confirm>,
    confirm_layout: Option<ModalLayout>,

    /// The list column of the Changes and History screens: docked beside the
    /// content when the window is wide, a floating drawer when it is narrow.
    drawer: SidebarDrawer,
    /// The Syntax toggle in the Changes header, and the press it reports.
    syntax_tool: Toolbar,
    syntax_pressed: Rc<Cell<bool>>,
    /// View state a screenshot scenario asks for once the window is laid out.
    staged: Staged,

    /// The pointer, while it is over the window.
    pointer: Option<Point>,
    /// The host's keyboard conventions (which modifier is the command key).
    conventions: ui::platform::PlatformConventions,

    /// Targets the last paint recorded, back to front.
    targets: Vec<Target>,
    /// What assistive technology can reach: every control and row of the
    /// last paint, including row actions that show only on hover.
    semantic: Vec<Target>,
    /// Scrollable regions of the last paint, back to front.
    scroll_regions: Vec<ScrollRegion>,
}

impl Default for GitClientApp {
    /// Equivalent to [`GitClientApp::new`].
    fn default() -> Self {
        Self::new()
    }
}

impl GitClientApp {
    /// Build the app, loading the repository discovered from the current
    /// directory (or `GIT_CLIENT_REPO`). Opens the empty repository picker
    /// when no repository is reachable.
    ///
    /// Setting `GIT_CLIENT_SAMPLE=1` forces the bundled fixture regardless
    /// of any reachable repository.
    pub fn new() -> Self {
        let force_sample =
            std::env::var("GIT_CLIENT_SAMPLE").is_ok_and(|v| v != "0" && !v.is_empty());
        if force_sample {
            return Self::sample();
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

    /// The app over the bundled sample repository: a populated, deterministic
    /// state with no repository on disk (screenshots and demos).
    pub fn sample() -> Self {
        let (model, diffs) = crate::sample::model();
        Self::from_model(model, diffs)
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

    /// `--shots --scenario`: open a screen directly so each one can be
    /// reviewed from a single frame.
    pub fn shot_screen(&mut self, screen: Screen) {
        self.switch_screen(screen);
    }

    /// `--shots --scenario split`: the side-by-side diff layout, pinned so
    /// the document sync does not fold it back to the stored preference.
    pub fn shot_split_diff(&mut self) {
        self.settings.diff_layout = DiffLayout::SideBySide;
        self.settings_forced = true;
    }

    /// `--shots --scenario commit`: a typed commit message so the commit
    /// button renders enabled.
    pub fn shot_commit_message(&mut self, message: &str) {
        self.commit_input.set_text(message);
    }

    /// `--shots --scenario discard`: the confirmation for discarding the
    /// first unstaged file, shown over its diff.
    pub fn shot_confirm_discard(&mut self) {
        if let Some(file) = self.model.unstaged.first() {
            let name = file.name().to_string();
            self.selected_change = Some((ChangeArea::Unstaged, 0));
            self.confirm = Some(Confirm::DiscardFile {
                area: ChangeArea::Unstaged,
                index: 0,
                name,
            });
        }
    }

    /// `--shots --scenario drawer`: the list drawer open in a narrow window.
    pub fn shot_open_drawer(&mut self) {
        self.staged = Staged::OpenDrawer;
    }

    /// Construct from an explicit model — used by the sample path and tests.
    pub fn from_model(mut model: RepoModel, diffs: DiffMap) -> Self {
        let graph_layout = graph::layout(&mut model.commits);
        // Pre-select the first changed file so the diff pane is populated.
        let selected_change = first_change(&model);
        let commit_input = TextInput::local()
            .with_id("git.commit-message")
            .with_accessible_name("Commit message")
            .with_placeholder("Commit message");
        let branch_input = TextInput::local()
            .with_id("git.branch-name")
            .with_accessible_name("New branch name")
            .with_kind(TextInputKind::Name)
            .with_placeholder("Branch name");
        let open_input = TextInput::local()
            .with_id("git.repository-path")
            .with_accessible_name("Repository path")
            .with_kind(TextInputKind::Path)
            .with_placeholder("Repository path");
        let syntax_pressed = Rc::new(Cell::new(false));
        let pressed = Rc::clone(&syntax_pressed);
        let syntax_tool =
            Toolbar::new(Orientation::Horizontal)
                .push_toggle("Syntax", true, move || pressed.set(true));
        Self {
            actions: crate::resource::ActionSink::default(),
            model,
            diffs,
            graph_layout,
            awaiting_repo: false,
            focus: None,
            commit_input,
            branch_input,
            open_input,
            repo_path: None,
            last_error: None,
            remote_job: None,
            settings: Settings::default(),
            shared_repo_fingerprint: None,
            settings_seeded: false,
            settings_forced: false,
            screen: Screen::Changes,
            rect: Rect::ZERO,
            selected_change,
            changes_scroll: 0.0,
            diff_scroll: 0.0,
            diff_hscroll: 0.0,
            selected_commit: 0,
            history_scroll: 0.0,
            detail_scroll: 0.0,
            panels_scroll: 0.0,
            confirm: None,
            confirm_layout: None,
            // Both lists are content the person reads: the overview width.
            // The diff beside them keeps the content minimum plus a rail's
            // width for its line numbers before the list becomes a drawer.
            drawer: SidebarDrawer::new()
                .docked_width(f64::from(ui::tokens::Metrics::BASE.overview_width))
                .breakpoint(f64::from(
                    ui::tokens::Metrics::BASE.overview_width
                        + ui::tokens::Metrics::BASE.content_min_width
                        + ui::tokens::Metrics::BASE.rail_width,
                )),
            syntax_tool,
            syntax_pressed,
            staged: Staged::Nothing,
            pointer: None,
            conventions: ui::platform::PlatformConventions::default(),
            targets: Vec::new(),
            semantic: Vec::new(),
            scroll_regions: Vec::new(),
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

    fn repository_busy(&mut self) -> bool {
        if self.remote_job.is_some() {
            self.last_error = Some(ui::i18n!("git-remote-busy"));
            return true;
        }
        false
    }

    fn start_remote(
        &mut self,
        action: RemoteAction,
        remote: Option<String>,
        invocation: Option<tilo_actions::InvocationId>,
    ) -> Result<(), String> {
        if self.remote_job.is_some() {
            return Err(ui::i18n!("git-remote-busy"));
        }
        let path = self
            .repo()
            .ok_or_else(|| "remote operation requires a repository".to_string())?;
        let label = match action {
            RemoteAction::Fetch => "Fetching…",
            RemoteAction::Pull => "Pulling…",
            RemoteAction::Push => "Pushing…",
        }
        .to_string();
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("git-remote".into())
            .spawn(move || {
                let result = match action {
                    RemoteAction::Fetch => crate::repo::fetch(&path, remote.as_deref()).map(|_| ()),
                    RemoteAction::Pull => crate::repo::pull(&path, remote.as_deref()),
                    RemoteAction::Push => crate::repo::push(&path, remote.as_deref()),
                }
                .map_err(|error| format!("{error:#}"));
                let _ = sender.send(result);
            })
            .map_err(|error| error.to_string())?;
        self.last_error = None;
        self.remote_job = Some(RemoteJob {
            receiver,
            result: None,
            invocation,
            progress_sent: false,
            label,
        });
        Ok(())
    }

    fn drain_remote(&mut self) {
        let Some(job) = self.remote_job.as_mut() else {
            return;
        };
        if job.result.is_some() {
            return;
        }
        let result = match job.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(ui::i18n!("git-remote-worker-stopped")),
        };
        self.last_error = result.as_ref().err().cloned();
        job.result = Some(result);
        if job.invocation.is_none() {
            self.remote_job = None;
        }
        // A failed transfer can still have fetched objects/refs. Refresh from
        // disk on either outcome; never infer rollback from an error.
        self.reload();
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

    fn selected_is_staged(&self) -> bool {
        matches!(self.selected_change, Some((ChangeArea::Staged, _)))
    }

    // --- Mutations on the model (mirror a real git op) -----------------

    /// Stage or unstage a single file. Against a real repo this calls
    /// `git2` (`add` / `reset HEAD -- path`) and re-reads the model so the
    /// list reflects the actual index. For the sample it falls back to the
    /// in-memory move so the offscreen capture still demonstrates the flow.
    fn toggle_stage(&mut self, area: ChangeArea, idx: usize) {
        if self.repository_busy() {
            return;
        }
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
        if self.repository_busy() {
            return;
        }
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
        if self.repository_busy() {
            return;
        }
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
        if self.repository_busy() {
            return;
        }
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
            when: ui::format::relative(0),
            time: ui::clock::local_now().timestamp(),
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
    /// untracked file). The controls reach here only through the
    /// confirmation sheet.
    fn discard_file(&mut self, area: ChangeArea, idx: usize) {
        if self.repository_busy() {
            return;
        }
        let Some(path) = self.repo() else {
            // The sample has no working tree: drop the change in memory so
            // the demonstration still shows the result.
            let list = match area {
                ChangeArea::Staged => return,
                ChangeArea::Unstaged => &mut self.model.unstaged,
                ChangeArea::Untracked => &mut self.model.untracked,
            };
            if idx < list.len() {
                list.remove(idx);
            }
            self.selected_change = first_change(&self.model);
            return;
        };
        let Some(file) = change_list(&self.model, area)
            .get(idx)
            .map(|f| f.path.clone())
        else {
            return;
        };
        let res = crate::repo::discard_file(&path, &file);
        self.run_action(res);
    }

    /// Discard a single hunk of the selected (unstaged) file's worktree change.
    fn discard_hunk(&mut self, hunk: usize) {
        if self.repository_busy() {
            return;
        }
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
        if self.repository_busy() {
            return;
        }
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
    /// header's layout control and the `d` shortcut.
    fn toggle_diff_layout(&mut self, doc: &mut AutoCommit) {
        self.settings.toggle_layout();
        self.diff_scroll = 0.0;
        self.diff_hscroll = 0.0;
        let action = crate::actions::set_diff_layout_action(self.settings.diff_layout);
        self.actions.dispatch_with_context(
            doc,
            crate::actions::GIT_CLIENT_ACTION_TYPE,
            &action,
            tilo_actions::ActionContext::for_action("so.tilo.git-client", "Git", &action),
        );
    }

    /// Flip diff syntax highlighting. Routed to by the header's Syntax toggle.
    fn toggle_syntax_highlight(&mut self, doc: &mut AutoCommit) {
        self.settings.syntax_highlight = !self.settings.syntax_highlight;
        let action = crate::actions::GitClientAction::SetSyntaxHighlight {
            on: self.settings.syntax_highlight,
        };
        self.actions.dispatch_with_context(
            doc,
            crate::actions::GIT_CLIENT_ACTION_TYPE,
            &action,
            tilo_actions::ActionContext::for_action("so.tilo.git-client", "Git", &action),
        );
    }

    /// Borrow the current external repository projection for the bounded
    /// resource catalogue. Presentation-only state is intentionally excluded.
    pub(crate) fn resource_model(&self) -> Option<&RepoModel> {
        (!self.awaiting_repo).then_some(&self.model)
    }

    /// The last preference [`EncodedAction`](tilo_actions::EncodedAction) an
    /// on-screen control dispatched, for the affordance == `ActionSpec`
    /// covenant test.
    #[doc(hidden)]
    #[allow(dead_code, reason = "framework migration test helper")]
    pub fn last_dispatched(&self) -> Option<&tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// Show the diff preferences the framework resource already holds. The
    /// live on-disk repository model is not replaced by the shared projection.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::doc::GitClientState) {
        if self.settings_forced {
            return;
        }
        self.settings = state.settings;
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::GitClient>,
    ) {
        self.actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// Label for an in-flight fetch/pull/push, when one is running.
    pub(crate) fn remote_job_label(&self) -> Option<String> {
        self.remote_job.as_ref().map(|job| job.label.clone())
    }

    /// Write the shared repository projection when branches/commits/remotes move.
    fn publish_shared_repo_if_needed(&mut self, doc: &mut AutoCommit) {
        if self.awaiting_repo {
            return;
        }
        let fingerprint = crate::doc::shared_fingerprint(&self.model);
        if self.shared_repo_fingerprint.as_deref() == Some(fingerprint.as_str()) {
            return;
        }
        let shared = crate::doc::SharedRepo::from_model(&self.model);
        let json = serde_json::to_string(&shared).unwrap_or_else(|_| "{}".into());
        let action = crate::actions::GitClientAction::WriteSharedRepo { json };
        self.actions.dispatch_with_context(
            doc,
            crate::actions::GIT_CLIENT_ACTION_TYPE,
            &action,
            tilo_actions::ActionContext::for_action("so.tilo.git-client", "Git", &action),
        );
        self.shared_repo_fingerprint = Some(fingerprint);
    }

    fn preference_actions(&self) -> [crate::actions::GitClientAction; 4] {
        [
            crate::actions::set_diff_layout_action(self.settings.diff_layout),
            crate::actions::GitClientAction::SetShowWhitespace {
                on: self.settings.show_whitespace,
            },
            crate::actions::GitClientAction::SetSyntaxHighlight {
                on: self.settings.syntax_highlight,
            },
            crate::actions::GitClientAction::SetWrapLines {
                on: self.settings.wrap_lines,
            },
        ]
    }

    fn publish_settings(&mut self, doc: &mut AutoCommit) {
        for action in self.preference_actions() {
            self.actions.dispatch_with_context(
                doc,
                crate::actions::GIT_CLIENT_ACTION_TYPE,
                &action,
                tilo_actions::ActionContext::for_action("so.tilo.git-client", "Git", &action),
            );
        }
    }

    fn create_new_branch(&mut self) {
        if self.repository_busy() {
            return;
        }
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
        if self.repository_busy() {
            return;
        }
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

    /// Apply a remote action (fetch / pull / push) against the live repo via
    /// `git2`, then re-read the model so the ahead/behind counters reflect
    /// the real tracking state. For the sample, reconcile the counters so the
    /// offscreen capture shows the effect.
    fn apply_remote_action(&mut self, action: RemoteAction) {
        if self.repo().is_some() {
            if let Err(error) = self.start_remote(action, None, None) {
                self.last_error = Some(error);
            }
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
        if self.repository_busy() {
            return;
        }
        let Some(path) = self.repo() else { return };
        let res = match action {
            BranchAction::Checkout(name) => crate::repo::checkout_branch(&path, &name),
            BranchAction::Delete(name) => crate::repo::delete_branch(&path, &name),
            BranchAction::StashApply(i) => crate::repo::stash_apply(&path, i),
        };
        self.run_action(res);
    }

    fn current_path(&self) -> Option<String> {
        let (area, idx) = self.selected_change?;
        change_list(&self.model, area)
            .get(idx)
            .map(|f| f.path.clone())
    }

    // --- Layout and paint ------------------------------------------------

    /// The header row's height: the shell metric, grown so a control and its
    /// `s3` insets always fit at a large text scale.
    fn header_height(theme: &Theme) -> f64 {
        let control = ui::widget::text_control_height(
            f64::from(ui::tokens::Metrics::BASE.min_pointer_target),
            theme,
        );
        f64::from(theme.tokens.metrics.header_height)
            .max(control + 2.0 * f64::from(theme.tokens.spacing.s3))
    }

    /// Whether an open drawer covers the window: everything under its scrim
    /// is inert until it closes.
    fn drawer_is_modal(&self) -> bool {
        self.screen.has_list() && self.drawer.is_overlay() && self.drawer.is_open()
    }

    fn push_target(&mut self, rect: Rect, hit: Hit) {
        if rect.width() > 0.0 && rect.height() > 0.0 {
            self.targets.push(Target {
                rect,
                hit: hit.clone(),
            });
            self.semantic.push(Target { rect, hit });
        }
    }

    /// Paint the whole window for this frame and record its targets.
    fn paint_window(&mut self, ctx: &mut PaintCtx<'_>, presence: &PresenceCtx) {
        self.targets.clear();
        self.semantic.clear();
        self.scroll_regions.clear();
        if self.awaiting_repo {
            self.syntax_tool.lay_out_in(Rect::ZERO, ctx.text, ctx.theme);
            self.paint_open(ctx, presence);
            return;
        }
        let theme = ctx.theme;
        let window = self.rect;
        let header_h = Self::header_height(theme);
        let has_list = self.screen.has_list();
        if has_list {
            self.drawer.tick(theme);
            self.drawer.resolve(window);
            // A staged drawer opens once the window has its narrow layout.
            if self.staged == Staged::OpenDrawer && self.drawer.is_overlay() {
                self.drawer.snap(true);
                self.staged = Staged::Nothing;
            }
        }
        let overlay = has_list && self.drawer.is_overlay();
        let column = if has_list {
            self.drawer.content_rect()
        } else {
            window
        };
        let header = Rect::new(
            column.x0,
            window.y0,
            column.x1,
            (window.y0 + header_h).min(window.y1),
        );
        let body = Rect::new(column.x0, header.y1, column.x1, window.y1.max(header.y1));
        let body = self.paint_banner(ctx, body);
        match self.screen {
            Screen::Changes => self.paint_diff(ctx, body),
            Screen::History => self.paint_detail(ctx, body),
            Screen::Panels => self.paint_panels(ctx, presence, body),
        }
        let toggle_in_list = has_list && self.drawer.is_open();
        self.paint_content_header(ctx, presence, header, has_list && !toggle_in_list);

        if !has_list {
            return;
        }
        let panel = self.drawer.sidebar_rect();
        if overlay {
            if self.drawer.reveal() <= 0.001 || panel.width() <= 0.0 {
                return;
            }
            self.drawer.draw_scrim(ctx.scene, ctx.transform, theme);
            // Everything under the drawer is inert while it is out.
            self.targets.push(Target {
                rect: window,
                hit: Hit::Scrim,
            });
            self.semantic.clear();
            self.scroll_regions.clear();
            let local = self.drawer.paint_panel(ctx.scene, ctx.transform, theme);
            let mut lctx = PaintCtx::new(&mut *ctx.scene, ctx.transform, &mut *ctx.text, &local);
            self.paint_list_column(&mut lctx, presence, panel, panel, header_h, toggle_in_list);
        } else if panel.width() > 0.5 {
            let local = self.drawer.paint_panel(ctx.scene, ctx.transform, theme);
            // The list keeps its width while the column opens or closes: it
            // slides under the column's edge instead of reflowing.
            let full = Rect::new(
                panel.x1 - self.drawer.resolved_docked_width(),
                panel.y0,
                panel.x1,
                panel.y1,
            );
            let mut lctx = PaintCtx::new(&mut *ctx.scene, ctx.transform, &mut *ctx.text, &local);
            self.paint_list_column(&mut lctx, presence, panel, full, header_h, toggle_in_list);
        } else {
            self.commit_input.apply_layout_external(Rect::ZERO);
        }
    }

    /// The square the sidebar toggle stands in, from `x` in `band`.
    fn toggle_rect(theme: &Theme, band: Rect, x: f64) -> Rect {
        let side = ui::widget::text_control_height(
            f64::from(ui::tokens::Metrics::BASE.min_pointer_target),
            theme,
        );
        let y = band.center().y - side / 2.0;
        Rect::new(x, y, x + side, y + side)
    }

    fn paint_toggle(&mut self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        let hovered = self.pointer.is_some_and(|p| rect.contains(p));
        self.drawer
            .draw_toggle(ctx.scene, ctx.transform, ctx.text, ctx.theme, rect, hovered);
        self.push_target(rect, Hit::SidebarToggle);
    }

    /// The list column: its header (the toggle and the repository's name)
    /// over the Changes or History list. `full` is the column at its full
    /// width; `clip` is what of it is showing.
    fn paint_list_column(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        presence: &PresenceCtx,
        clip: Rect,
        full: Rect,
        header_h: f64,
        with_toggle: bool,
    ) {
        let theme = ctx.theme;
        let s3 = f64::from(theme.tokens.spacing.s3);
        let gap = f64::from(theme.tokens.spacing.s2);
        ctx.scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &clip,
        );
        let header = Rect::new(full.x0, full.y0, full.x1, full.y0 + header_h);
        let mut x = header.x0 + s3;
        if with_toggle {
            let rect = Self::toggle_rect(theme, header, x);
            self.paint_toggle(ctx, rect);
            x = rect.x1 + gap;
        }
        let heading = theme.text(TextRole::Heading);
        let name = ctx.text.elide_to_width_style(
            &self.model.name,
            ui::num::f32_of_f64((header.x1 - s3 - x).max(0.0)),
            &heading,
        );
        ctx.text_role_in_band(
            &name,
            TextRole::Heading,
            theme.text_primary,
            x,
            header,
            None,
        );

        let list = Rect::new(full.x0, header.y1, full.x1, full.y1.max(header.y1));
        match self.screen {
            Screen::Changes => self.paint_changes_list(ctx, presence, list, clip),
            Screen::History => self.paint_history_list(ctx, list, clip),
            Screen::Panels => {}
        }
        ctx.scene.pop_layer();
    }

    /// The Changes list in `list`, of which `clip` is showing.
    fn paint_changes_list(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        presence: &PresenceCtx,
        list: Rect,
        clip: Rect,
    ) {
        let pointer = self.pointer.filter(|p| clip.contains(*p));
        let out = changes::paint(
            ctx,
            list,
            &changes::ChangesInput {
                model: &self.model,
                selected: self.selected_change,
                scroll: self.changes_scroll,
                message: self.commit_input.value(),
                pointer,
            },
        );
        for (rect, hit) in out.targets {
            let rect = rect.intersect(clip);
            if rect.width() <= 0.0 || rect.height() <= 0.0 {
                continue;
            }
            // Row actions show under the pointer; their semantic
            // twins are recorded for every row below.
            if matches!(hit, ChangesHit::ToggleStage(..)) {
                self.targets.push(Target {
                    rect,
                    hit: Hit::Change(hit),
                });
                continue;
            }
            if let ChangesHit::SelectFile(area, index) = hit {
                self.semantic.push(Target {
                    rect,
                    hit: Hit::Change(ChangesHit::ToggleStage(area, index)),
                });
            }
            self.push_target(rect, Hit::Change(hit));
        }
        let viewport =
            Rect::new(list.x0, list.y0, list.x1, list.y0 + out.viewport_height).intersect(clip);
        self.scroll_regions.push(ScrollRegion {
            rect: viewport,
            scroller: Scroller::ChangesList,
            max_y: out.content_height - out.viewport_height,
            max_x: 0.0,
        });
        self.changes_scroll = self
            .changes_scroll
            .clamp(0.0, (out.content_height - out.viewport_height).max(0.0));
        self.commit_input.apply_layout_external(out.commit_input);
        self.commit_input
            .draw(ctx.scene, ctx.transform, ctx.text, ctx.theme, presence);
    }

    /// The commit list in `list`, of which `clip` is showing.
    fn paint_history_list(&mut self, ctx: &mut PaintCtx<'_>, list: Rect, clip: Rect) {
        let pointer = self.pointer.filter(|p| clip.contains(*p));
        let out = history::paint_list(
            ctx,
            list,
            &history::ListInput {
                model: &self.model,
                layout: &self.graph_layout,
                selected: self.selected_commit,
                scroll: self.history_scroll,
                pointer,
            },
        );
        for (index, rect) in out.rows {
            self.push_target(rect.intersect(clip), Hit::Commit(index));
        }
        self.scroll_regions.push(ScrollRegion {
            rect: list.intersect(clip),
            scroller: Scroller::HistoryList,
            max_y: out.content_height - list.height(),
            max_x: 0.0,
        });
    }

    /// The content column's header: the toggle when the list is hidden, the
    /// screen switcher, and the screen's commands at the trailing end; one
    /// hairline under it.
    fn paint_content_header(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        presence: &PresenceCtx,
        header: Rect,
        with_toggle: bool,
    ) {
        let theme = ctx.theme;
        let s3 = f64::from(theme.tokens.spacing.s3);
        let gap = f64::from(theme.tokens.spacing.s2);
        ui::surface::separator(
            ctx.scene,
            ctx.transform,
            header.x0,
            header.x1,
            header.y1,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
        let mut x = header.x0 + s3;
        if with_toggle {
            let rect = Self::toggle_rect(theme, header, x);
            self.paint_toggle(ctx, rect);
            x = rect.x1 + gap;
        }

        let labels: Vec<&str> = Screen::ALL.iter().map(|s| s.label()).collect();
        let natural =
            SegmentedFace::intrinsic_width(&labels, ControlSize::Regular, ctx.text, theme);
        let mut right = header.x1 - s3;
        let min_x = x + natural + s3;
        match self.screen {
            Screen::Changes => {
                right = self.paint_diff_commands(ctx, presence, header, right, min_x);
            }
            Screen::Panels => {
                let placed = panels::remote_buttons(ctx, header, right, min_x, self.pointer);
                for (rect, action) in placed {
                    right = right.min(rect.x0 - s3);
                    self.push_target(rect, Hit::Remote(action));
                }
            }
            Screen::History => {}
        }
        if self.screen != Screen::Changes {
            self.syntax_tool.lay_out_in(Rect::ZERO, ctx.text, theme);
        }

        let width = natural.min((right - x).max(0.0));
        let h = SegmentedFace::height(ControlSize::Regular, theme);
        let y = header.center().y - h / 2.0;
        let each = width / labels.len() as f64;
        let rects: Vec<Rect> = (0..labels.len())
            .map(|i| Rect::new(x + each * i as f64, y, x + each * (i + 1) as f64, y + h))
            .collect();
        let selected = Screen::ALL
            .iter()
            .position(|s| *s == self.screen)
            .unwrap_or(0);
        let hovered = self
            .pointer
            .and_then(|p| rects.iter().position(|r| r.contains(p)));
        ToggleGroup::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &SegmentedFace::new(&rects, &labels, selected).hovered(hovered),
        );
        for (rect, screen) in rects.iter().zip(Screen::ALL) {
            self.push_target(*rect, Hit::Screen(screen));
        }
    }

    /// The diff commands at the header's trailing end: the layout choice and
    /// the Syntax toggle. Where both do not fit right of `min_x` the layout
    /// choice stays and the toggle leaves (it stays in the palette).
    /// Returns where the commands start.
    fn paint_diff_commands(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        presence: &PresenceCtx,
        header: Rect,
        right: f64,
        min_x: f64,
    ) -> f64 {
        let theme = ctx.theme;
        let s3 = f64::from(theme.tokens.spacing.s3);
        let h = SegmentedFace::height(ControlSize::Regular, theme);
        let y = header.center().y - h / 2.0;

        self.syntax_tool
            .set_active(0, self.settings.syntax_highlight);
        self.syntax_tool.lay_out_in(Rect::ZERO, ctx.text, theme);
        let tool_w = f64::from(
            Widget::measure(
                &mut self.syntax_tool,
                taffy::Size {
                    width: None,
                    height: None,
                },
                taffy::Size {
                    width: taffy::AvailableSpace::MaxContent,
                    height: taffy::AvailableSpace::MaxContent,
                },
                ctx.text,
            )
            .width,
        );
        let labels = ["Unified", "Split"];
        let layout_w =
            SegmentedFace::intrinsic_width(&labels, ControlSize::Regular, ctx.text, theme);
        let with_tool = right - tool_w - s3 - layout_w >= min_x;
        if !with_tool && right - layout_w < min_x {
            return right;
        }

        let mut right = right;
        if with_tool {
            // As tall as the header's other controls.
            let tool = Rect::new(right - tool_w, y, right, y + h);
            self.syntax_tool.lay_out_in(tool, ctx.text, theme);
            Widget::draw(
                &mut self.syntax_tool,
                ctx.scene,
                ctx.transform,
                ctx.text,
                theme,
                presence,
            );
            self.semantic.push(Target {
                rect: tool,
                hit: Hit::SyntaxHighlight,
            });
            right = tool.x0 - s3;
        }

        let x0 = right - layout_w;
        let rects = [
            Rect::new(x0, y, x0 + layout_w / 2.0, y + h),
            Rect::new(x0 + layout_w / 2.0, y, right, y + h),
        ];
        let selected = usize::from(self.settings.diff_layout == DiffLayout::SideBySide);
        let hovered = self
            .pointer
            .and_then(|p| rects.iter().position(|r| r.contains(p)));
        ToggleGroup::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &SegmentedFace::new(&rects, &labels, selected)
                .style(ui::widget::SegmentedStyle::Quiet)
                .hovered(hovered),
        );
        self.push_target(rects[0], Hit::DiffLayout(DiffLayout::Unified));
        self.push_target(rects[1], Hit::DiffLayout(DiffLayout::SideBySide));
        x0 - s3
    }

    /// An error or a running transfer as a banner at the top of the content;
    /// returns the content area under it.
    fn paint_banner(&mut self, ctx: &mut PaintCtx<'_>, body: Rect) -> Rect {
        use ui::widget::banner::{BannerContent, BannerKind};
        let (kind, title, dismissible) = if self.remote_job.is_some() {
            (BannerKind::Info, ui::i18n!("git-remote-running"), false)
        } else if let Some(error) = &self.last_error {
            (BannerKind::Error, error.clone(), true)
        } else {
            return body;
        };
        let theme = ctx.theme;
        let s = &theme.tokens.spacing;
        let m = &theme.tokens.metrics;
        let margin = f64::from(s.s3);
        let rect_w = (body.width() - 2.0 * margin).max(0.0);
        let text_w = (rect_w
            - 2.0 * f64::from(s.s4)
            - f64::from(m.icon_md)
            - 2.0 * f64::from(s.s3)
            - f64::from(m.bar_control_height))
        .max(1.0);
        let style = theme.text(TextRole::Body).strong();
        let (_, title_h) =
            ctx.text
                .measure_style(&title, Some(ui::num::f32_of_f64(text_w)), &style);
        let h = (f64::from(title_h) + 2.0 * f64::from(s.s3)).max(f64::from(m.panel_row_height));
        let rect = Rect::new(
            body.x0 + margin,
            body.y0 + margin,
            body.x1 - margin,
            body.y0 + margin + h,
        );
        let hover = self.pointer.unwrap_or(Point::new(f64::MIN, f64::MIN));
        let dismiss = Banner::paint(
            ctx,
            rect,
            &BannerContent::new(kind, &title).dismissible(dismissible),
            hover,
        );
        self.semantic.push(Target {
            rect,
            hit: Hit::DismissError,
        });
        if dismiss.width() > 0.0 {
            self.targets.push(Target {
                rect: dismiss,
                hit: Hit::DismissError,
            });
        }
        Rect::new(body.x0, rect.y1, body.x1, body.y1.max(rect.y1))
    }

    /// The selected file's diff, or one sentence when there is none.
    fn paint_diff(&mut self, ctx: &mut PaintCtx<'_>, body: Rect) {
        let key = self.current_diff_key();
        let Some(diff) = key.as_ref().and_then(|k| self.diffs.get(k)) else {
            let title = if self.model.change_count() == 0 {
                "Your working tree is clean"
            } else {
                "Select a file to see its changes"
            };
            EmptyState::paint(
                ctx,
                body,
                &EmptyStateContent::new(title),
                Point::new(f64::MIN, f64::MIN),
            );
            return;
        };
        let staged = self.selected_is_staged();
        let out = diffview::paint(
            ctx,
            body,
            &diffview::DiffInput {
                diff,
                scroll: self.diff_scroll,
                hscroll: self.diff_hscroll,
                settings: self.settings,
                staged,
                can_discard: true,
                pointer: self.pointer,
            },
        );
        for (rect, hit) in out.targets {
            self.push_target(rect, Hit::Diff(hit));
        }
        let max_y = out.content_height - out.viewport.height();
        self.diff_scroll = self.diff_scroll.clamp(0.0, max_y.max(0.0));
        self.diff_hscroll = self.diff_hscroll.clamp(0.0, out.max_hscroll);
        self.scroll_regions.push(ScrollRegion {
            rect: out.viewport,
            scroller: Scroller::Diff,
            max_y,
            max_x: out.max_hscroll,
        });
    }

    /// The selected commit.
    fn paint_detail(&mut self, ctx: &mut PaintCtx<'_>, body: Rect) {
        let Some(commit) = self.model.commits.get(self.selected_commit) else {
            EmptyState::paint(
                ctx,
                body,
                &EmptyStateContent::new("No commits yet"),
                Point::new(f64::MIN, f64::MIN),
            );
            return;
        };
        let height = history::paint_detail(ctx, body, commit, self.detail_scroll);
        let max_y = height - body.height();
        self.detail_scroll = self.detail_scroll.clamp(0.0, max_y.max(0.0));
        self.scroll_regions.push(ScrollRegion {
            rect: body,
            scroller: Scroller::Detail,
            max_y,
            max_x: 0.0,
        });
    }

    /// The branches page.
    fn paint_panels(&mut self, ctx: &mut PaintCtx<'_>, presence: &PresenceCtx, body: Rect) {
        let out = panels::paint(
            ctx,
            body,
            &panels::PanelsInput {
                model: &self.model,
                scroll: self.panels_scroll,
                pointer: self.pointer,
                can_create: self.repo_path.is_some(),
                branch_name: self.branch_input.value(),
            },
        );
        for (rect, hit) in out.targets {
            if rect.width() > 0.0 && rect.height() > 0.0 {
                self.targets.push(Target {
                    rect,
                    hit: Hit::Panel(hit),
                });
            }
        }
        for (rect, hit) in out.semantic {
            if rect.width() > 0.0 && rect.height() > 0.0 {
                self.semantic.push(Target {
                    rect,
                    hit: Hit::Panel(hit),
                });
            }
        }
        self.branch_input.apply_layout_external(out.branch_field);
        if out.branch_field.width() > 0.0 {
            self.branch_input
                .draw(ctx.scene, ctx.transform, ctx.text, ctx.theme, presence);
        }
        let max_y = out.content_height - body.height();
        self.panels_scroll = self.panels_scroll.clamp(0.0, max_y.max(0.0));
        self.scroll_regions.push(ScrollRegion {
            rect: body,
            scroller: Scroller::Panels,
            max_y,
            max_x: 0.0,
        });
    }

    /// The "open a repository" screen: what to do, the path field and the
    /// Open button, and the reason the last attempt failed.
    fn paint_open(&mut self, ctx: &mut PaintCtx<'_>, presence: &PresenceCtx) {
        use ui::widget::banner::{BannerContent, BannerKind};
        let theme = ctx.theme;
        let s = &theme.tokens.spacing;
        let lane = ui::content_layout::FocusedContentLane::new()
            .max_width(f64::from(theme.tokens.metrics.panel_width_sheet))
            .gutter(f64::from(s.s6))
            .layout(self.rect);
        let width = ui::num::f32_of_f64(lane.width());
        let mut y = self.rect.y0 + self.rect.height() / 3.0;
        let title = "Open a repository";
        let title_h = ctx.measure_role(title, TextRole::Title, Some(width)).height;
        ctx.text_role(
            title,
            TextRole::Title,
            theme.text_primary,
            Point::new(lane.x0, y),
            Some(width),
        );
        y += title_h + f64::from(s.s2);
        let hint = "Enter the path of a git repository on this computer.";
        let hint_h = ctx.measure_role(hint, TextRole::Body, Some(width)).height;
        ctx.text_role(
            hint,
            TextRole::Body,
            theme.text_dim,
            Point::new(lane.x0, y),
            Some(width),
        );
        y += hint_h + f64::from(s.s6);

        let h = ButtonFace::height(ControlSize::Regular, theme);
        let label = "Open";
        let button_w = ButtonFace::new(Rect::ZERO, label).intrinsic_width(ctx.text, theme);
        let button = Rect::new(lane.x1 - button_w, y, lane.x1, y + h);
        let field = Rect::new(lane.x0, y, button.x0 - f64::from(s.s2), y + h);
        self.open_input.apply_layout_external(field);
        self.open_input
            .draw(ctx.scene, ctx.transform, ctx.text, theme, presence);
        self.semantic.push(Target {
            rect: field,
            hit: Hit::OpenPath,
        });
        self.targets.push(Target {
            rect: field,
            hit: Hit::OpenPath,
        });
        let enabled = !self.open_input.value().trim().is_empty();
        ui::widget::Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(button, label)
                .role(ui::widget::ButtonRole::Primary)
                .hovered(enabled && self.pointer.is_some_and(|p| button.contains(p)))
                .disabled(!enabled),
        );
        self.semantic.push(Target {
            rect: button,
            hit: Hit::OpenRepository,
        });
        if enabled {
            self.targets.push(Target {
                rect: button,
                hit: Hit::OpenRepository,
            });
        }

        if let Some(error) = &self.last_error {
            let top = button.y1 + f64::from(s.s4);
            let banner = Rect::new(
                lane.x0,
                top,
                lane.x1,
                top + f64::from(theme.tokens.metrics.panel_row_height),
            );
            Banner::paint(
                ctx,
                banner,
                &BannerContent::new(BannerKind::Error, error),
                Point::new(f64::MIN, f64::MIN),
            );
        }
    }

    // --- Activation -----------------------------------------------------

    /// Switch the active screen.
    fn switch_screen(&mut self, screen: Screen) {
        self.screen = screen;
        self.set_focus(None);
        self.confirm = None;
    }

    /// Do what `hit` names: the one path pointer presses, keys and
    /// assistive actions share.
    fn activate(&mut self, hit: Hit, doc: &mut AutoCommit) {
        let in_drawer = self.drawer_is_modal();
        match hit {
            Hit::SidebarToggle => self.drawer.toggle(),
            Hit::Scrim => self.drawer.close(),
            Hit::Screen(screen) => self.switch_screen(screen),
            Hit::DiffLayout(layout) => {
                if self.settings.diff_layout != layout {
                    self.toggle_diff_layout(doc);
                }
            }
            Hit::SyntaxHighlight => self.toggle_syntax_highlight(doc),
            Hit::Remote(action) => self.apply_remote_action(action),
            Hit::DismissError => self.last_error = None,
            Hit::Change(change) => self.activate_change(change, in_drawer),
            Hit::Diff(diff) => self.activate_diff(diff),
            Hit::Commit(index) => {
                self.selected_commit = index;
                self.detail_scroll = 0.0;
                if in_drawer {
                    self.drawer.close();
                }
            }
            Hit::Panel(panel) => self.activate_panel(panel),
            Hit::OpenPath => self.set_focus(Some(Field::Open)),
            Hit::OpenRepository => self.open_repo_from_input(),
        }
    }

    fn activate_change(&mut self, hit: ChangesHit, in_drawer: bool) {
        match hit {
            ChangesHit::SelectFile(area, idx) => {
                self.selected_change = Some((area, idx));
                self.diff_scroll = 0.0;
                self.diff_hscroll = 0.0;
                // Choosing in a drawer shows what was chosen.
                if in_drawer {
                    self.drawer.close();
                }
            }
            ChangesHit::ToggleStage(area, idx) => self.toggle_stage(area, idx),
            ChangesHit::StageAll(_) => self.stage_all(),
            ChangesHit::UnstageAll(_) => self.unstage_all(),
            ChangesHit::Commit => self.do_commit(),
            ChangesHit::FocusMessage => self.set_focus(Some(Field::Commit)),
        }
    }

    fn activate_diff(&mut self, hit: DiffHit) {
        let Some((area, index)) = self.selected_change else {
            return;
        };
        match hit {
            DiffHit::StageFile => self.toggle_stage(area, index),
            DiffHit::DiscardFile => {
                if let Some(file) = change_list(&self.model, area).get(index) {
                    self.confirm = Some(Confirm::DiscardFile {
                        area,
                        index,
                        name: file.name().to_string(),
                    });
                }
            }
            DiffHit::Hunk(hunk, HunkAction::Toggle) => self.stage_hunk(hunk),
            DiffHit::Hunk(hunk, HunkAction::Discard) => {
                self.confirm = Some(Confirm::DiscardHunk(hunk));
            }
        }
    }

    fn activate_panel(&mut self, hit: PanelHit) {
        match hit {
            PanelHit::Branch(BranchAction::Delete(name)) => {
                self.confirm = Some(Confirm::DeleteBranch(name));
            }
            PanelHit::Branch(action) => self.apply_branch_action(action),
            PanelHit::BranchName => self.set_focus(Some(Field::Branch)),
            PanelHit::CreateBranch => self.create_new_branch(),
        }
    }

    /// Carry out the confirmed destructive step.
    fn perform_confirmed(&mut self) {
        match self.confirm.take() {
            Some(Confirm::DiscardFile { area, index, .. }) => self.discard_file(area, index),
            Some(Confirm::DiscardHunk(hunk)) => self.discard_hunk(hunk),
            Some(Confirm::DeleteBranch(name)) => {
                self.apply_branch_action(BranchAction::Delete(name));
            }
            None => {}
        }
    }

    /// A press while the confirmation sheet is up: its buttons, its close
    /// control and its scrim; nothing under it.
    fn confirm_press(&mut self, pos: Point) {
        let Some(layout) = &self.confirm_layout else {
            return;
        };
        match layout.hit(pos) {
            Some(ModalHit::Action(0) | ModalHit::Close) => self.confirm = None,
            Some(ModalHit::Action(_)) => self.perform_confirmed(),
            Some(ModalHit::Scrim) if layout.dismiss_on_scrim => self.confirm = None,
            _ => {}
        }
    }

    /// Scroll the region under `pos`.
    fn scroll_at(&mut self, pos: Point, dx: f64, dy: f64) {
        let Some(region) = self
            .scroll_regions
            .iter()
            .rev()
            .find(|r| r.rect.contains(pos))
            .copied()
        else {
            return;
        };
        let max_y = region.max_y.max(0.0);
        let max_x = region.max_x.max(0.0);
        match region.scroller {
            Scroller::ChangesList => {
                self.changes_scroll = (self.changes_scroll - dy).clamp(0.0, max_y);
            }
            Scroller::Diff => {
                self.diff_scroll = (self.diff_scroll - dy).clamp(0.0, max_y);
                self.diff_hscroll = (self.diff_hscroll - dx).clamp(0.0, max_x);
            }
            Scroller::HistoryList => {
                self.history_scroll = (self.history_scroll - dy).clamp(0.0, max_y);
            }
            Scroller::Detail => self.detail_scroll = (self.detail_scroll - dy).clamp(0.0, max_y),
            Scroller::Panels => self.panels_scroll = (self.panels_scroll - dy).clamp(0.0, max_y),
        }
    }

    // --- Accessibility --------------------------------------------------

    /// The semantic inventory of the last paint: one node per control and
    /// row, keyed by a stable action target.
    fn accessibility_targets(&self) -> Vec<(String, Rect, ui::a11y::AccessNode)> {
        self.semantic_entries()
            .into_iter()
            .map(|(target, rect, node, _)| (target, rect, node))
            .collect()
    }

    /// What the semantic target `target` does, if it does anything.
    fn semantic_hit(&self, target: &str) -> Option<Hit> {
        self.semantic_entries()
            .into_iter()
            .find(|(candidate, _, _, _)| candidate == target)
            .and_then(|(_, _, _, hit)| hit)
    }

    /// The semantic inventory with what each entry does.
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive match keeps every kind of target and its node auditable together"
    )]
    fn semantic_entries(&self) -> Vec<(String, Rect, ui::a11y::AccessNode, Option<Hit>)> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let button = |target: String, rect: Rect, label: String| {
            (
                target,
                rect,
                AccessNode::new(Role::Button)
                    .with_name(label)
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(rect),
            )
        };
        let field = |target: &str, rect: Rect, input: &TextInput, name: &str| {
            (
                target.to_string(),
                rect,
                input
                    .accessibility()
                    .unwrap_or_else(|| AccessNode::new(Role::TextInput).with_name(name))
                    .with_bounds(rect),
            )
        };
        let mut out = Vec::new();
        for target in &self.semantic {
            let rect = target.rect;
            let entry = match &target.hit {
                Hit::SidebarToggle => button(
                    "git.navigation-toggle".into(),
                    rect,
                    if self.drawer.is_open() {
                        "Hide sidebar"
                    } else {
                        "Show sidebar"
                    }
                    .into(),
                ),
                Hit::Screen(screen) => (
                    screen.target().to_string(),
                    rect,
                    AccessNode::new(Role::Tab)
                        .with_name(screen.label())
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .selected(*screen == self.screen),
                        )
                        .with_bounds(rect),
                ),
                Hit::DiffLayout(layout) => {
                    let (target, name) = match layout {
                        DiffLayout::Unified => ("git.diff-layout.unified", "Unified diff"),
                        DiffLayout::SideBySide => ("git.diff-layout.split", "Side-by-side diff"),
                    };
                    (
                        target.to_string(),
                        rect,
                        AccessNode::new(Role::Button)
                            .with_name(name)
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .selected(self.settings.diff_layout == *layout),
                            )
                            .with_bounds(rect),
                    )
                }
                Hit::SyntaxHighlight => (
                    "git.syntax-highlight".to_string(),
                    rect,
                    AccessNode::new(Role::Button)
                        .with_name("Syntax highlighting")
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .checked(self.settings.syntax_highlight),
                        )
                        .with_bounds(rect),
                ),
                Hit::Remote(action) => {
                    let (target, name) = match action {
                        RemoteAction::Fetch => ("git.remote.fetch", "Fetch from remote"),
                        RemoteAction::Pull => ("git.remote.pull", "Pull from remote"),
                        RemoteAction::Push => ("git.remote.push", "Push to remote"),
                    };
                    button(target.into(), rect, name.into())
                }
                Hit::DismissError => {
                    let text = if self.remote_job.is_some() {
                        ui::i18n!("git-remote-running")
                    } else {
                        self.last_error.clone().unwrap_or_default()
                    };
                    out.push((
                        "git.status".to_string(),
                        rect,
                        AccessNode::new(Role::StaticText)
                            .with_name(&text)
                            .with_value(&text)
                            .with_live(ui::a11y::Politeness::Polite)
                            .with_bounds(rect),
                        None,
                    ));
                    if self.remote_job.is_some() {
                        continue;
                    }
                    button("git.dismiss-error".into(), rect, "Dismiss error".into())
                }
                Hit::Change(ChangesHit::FocusMessage) => field(
                    "git.commit-message",
                    rect,
                    &self.commit_input,
                    "Commit message",
                ),
                Hit::Change(hit) => match self.change_node(hit, rect) {
                    Some(entry) => entry,
                    None => continue,
                },
                Hit::Diff(hit) => {
                    let (target, name) = match hit {
                        DiffHit::StageFile if self.selected_is_staged() => (
                            "git.diff.unstage-file".to_string(),
                            "Unstage file".to_string(),
                        ),
                        DiffHit::StageFile => {
                            ("git.diff.stage-file".to_string(), "Stage file".to_string())
                        }
                        DiffHit::DiscardFile => (
                            "git.discard-file".to_string(),
                            "Discard file changes".to_string(),
                        ),
                        DiffHit::Hunk(hunk, action) => {
                            let name = match action {
                                HunkAction::Toggle if self.selected_is_staged() => "Unstage hunk",
                                HunkAction::Toggle => "Stage hunk",
                                HunkAction::Discard => "Discard hunk",
                            };
                            (format!("git.hunk:{hunk}:{action:?}"), name.to_string())
                        }
                    };
                    button(target, rect, name)
                }
                Hit::Commit(index) => {
                    let Some(commit) = self.model.commits.get(*index) else {
                        continue;
                    };
                    (
                        format!("git.commit:{}", commit.id),
                        rect,
                        AccessNode::new(Role::ListItem)
                            .with_name(format!(
                                "{} by {}, {}",
                                commit.summary,
                                commit.author_name,
                                history::age(commit.time)
                            ))
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .selected(*index == self.selected_commit),
                            )
                            .with_bounds(rect),
                    )
                }
                Hit::Panel(PanelHit::BranchName) => field(
                    "git.new-branch-name",
                    rect,
                    &self.branch_input,
                    "New branch name",
                ),
                Hit::Panel(PanelHit::CreateBranch) => {
                    button("git.create-branch".into(), rect, "Create branch".into())
                }
                Hit::Panel(PanelHit::Branch(action)) => {
                    let (target, name) = match action {
                        BranchAction::Checkout(name) => (
                            format!("git.branch.checkout:{name}"),
                            format!("Check out {name}"),
                        ),
                        BranchAction::Delete(name) => (
                            format!("git.branch.delete:{name}"),
                            format!("Delete {name}"),
                        ),
                        BranchAction::StashApply(index) => (
                            format!("git.stash.apply:{index}"),
                            format!("Apply stash {index}"),
                        ),
                    };
                    button(target, rect, name)
                }
                Hit::OpenPath => field("git.open-path", rect, &self.open_input, "Repository path"),
                Hit::OpenRepository => {
                    button("git.open-repository".into(), rect, "Open repository".into())
                }
                Hit::Scrim => continue,
            };
            let (id, rect, node) = entry;
            out.push((id, rect, node, Some(target.hit.clone())));
        }
        out
    }

    /// The node for one entry of the changes column.
    fn change_node(
        &self,
        hit: &ChangesHit,
        rect: Rect,
    ) -> Option<(String, Rect, ui::a11y::AccessNode)> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let node = |role: Role, name: String, selected: bool| {
            AccessNode::new(role)
                .with_name(name)
                .with_state(AccessState::new().focusable(true).selected(selected))
                .with_bounds(rect)
        };
        Some(match hit {
            ChangesHit::SelectFile(area, row) => {
                let change = change_list(&self.model, *area).get(*row)?;
                (
                    format!("git.change.open:{area:?}:{}", change.path),
                    rect,
                    node(
                        Role::ListItem,
                        format!("Open {} diff, {}", change.path, change.kind.label()),
                        self.selected_change == Some((*area, *row)),
                    ),
                )
            }
            ChangesHit::ToggleStage(area, row) => {
                let verb = if *area == ChangeArea::Staged {
                    "Unstage"
                } else {
                    "Stage"
                };
                let change = change_list(&self.model, *area).get(*row)?;
                (
                    format!("git.change.{verb}:{area:?}:{}", change.path),
                    rect,
                    node(Role::Button, format!("{verb} {}", change.path), false),
                )
            }
            ChangesHit::StageAll(area) => (
                format!("git.changes.stage-all:{area:?}"),
                rect,
                node(Role::Button, "Stage all changes".into(), false),
            ),
            ChangesHit::UnstageAll(area) => (
                format!("git.changes.unstage-all:{area:?}"),
                rect,
                node(Role::Button, "Unstage all changes".into(), false),
            ),
            ChangesHit::Commit => (
                "git.changes.commit".into(),
                rect,
                node(Role::Button, changes::commit_label(&self.model), false),
            ),
            ChangesHit::FocusMessage => return None,
        })
    }
}

/// Whether `p` falls inside a non-empty `rect` (a zero-size rect never hits).
fn rect_hit(rect: Rect, p: Point) -> bool {
    rect.width() > 0.0 && rect.height() > 0.0 && rect.contains(p)
}

/// The files of one change area.
fn change_list(model: &RepoModel, area: ChangeArea) -> &[FileChange] {
    match area {
        ChangeArea::Staged => &model.staged,
        ChangeArea::Unstaged => &model.unstaged,
        ChangeArea::Untracked => &model.untracked,
    }
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
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::GIT_CLIENT_OPERATION_TYPE]
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
        self.drain_remote();
        if self.settings_seeded {
            // Adopt document changes: remote collaborators, and this app's own
            // preference actions, which the framework `ActionHost` applies
            // straight to the document through the executor.
            self.settings = Settings::read(state);
        } else {
            // First sync: adopt any persisted preferences, then seed the doc so
            // it is the source of truth and collaborators see the same
            // preferences. An initial override forced at construction (the
            // split-diff screenshot scenario) wins over the persisted value.
            if !self.settings_forced {
                // Read without creating the map. The preference actions below
                // are what write it, hosted or standalone.
                self.settings = Settings::read_only(state);
            }
            self.publish_settings(state);
            self.settings_seeded = true;
        }
        self.publish_shared_repo_if_needed(state);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
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
        // The list column's breakpoint and widths grow with the text scale.
        self.drawer.scale_for(theme);
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
        // The drawer adopts the new width now, so the next press is tested
        // against the layout the next paint shows.
        self.drawer.resolve(self.rect);
    }

    fn set_platform_conventions(&mut self, conventions: ui::platform::PlatformConventions) {
        self.conventions = conventions;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        self.paint_window(&mut ctx, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use ui::widget::modal_dialog;
        Widget::draw_overlay(
            &mut self.syntax_tool,
            scene,
            transform,
            text,
            theme,
            presence,
        );
        let Some(confirm) = &self.confirm else {
            self.confirm_layout = None;
            return;
        };
        let window = presence.viewport.get().unwrap_or(self.rect);
        let title = confirm.title();
        let body = confirm.body();
        let action = confirm.action();
        let hovered = |index: usize| {
            self.confirm_layout
                .as_ref()
                .and_then(|layout| layout.actions.get(index).copied())
                .zip(self.pointer)
                .is_some_and(|(rect, p)| rect.contains(p))
        };
        let actions = [
            ModalAction::secondary("Cancel").hovered(hovered(0)),
            ModalAction::danger(action).hovered(hovered(1)),
        ];
        let style = theme.text(TextRole::Body);
        let spec = ModalSpec::new(&title).actions(&actions);
        let width = modal_dialog::body_width(window, spec.max_width, theme);
        let (_, body_h) = text.measure_style(body, Some(ui::num::f32_of_f64(width)), &style);
        let spec = spec.body_height(f64::from(body_h)).dismiss_on_scrim(true);
        let layout = modal_dialog::layout(window, &spec, text, theme);
        modal_dialog::paint_backdrop(scene, transform, text, theme, &layout, &spec);
        let (_, dim) = modal_dialog::ink(theme);
        text.draw_style(
            scene,
            body,
            dim,
            transform.pre_translate(vello::kurbo::Vec2::new(layout.body.x0, layout.body.y0)),
            Some(ui::num::f32_of_f64(layout.body.width())),
            &style,
        );
        modal_dialog::paint_actions(scene, transform, text, theme, &layout, &spec);
        self.confirm_layout = Some(layout);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let mut input = InputEventCtx {
            event,
            scale_factor: ctx.scale_factor,
            modifiers: *ctx.modifiers,
            state: ctx.state,
            path: ctx.path,
            taffy: ctx.taffy,
        };
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Some(Point::new(
                    position.x / ctx.scale_factor,
                    position.y / ctx.scale_factor,
                ));
                // Keep an in-flight text-selection drag updating.
                self.forward_to_inputs(&mut input);
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer = None;
                self.forward_to_inputs(&mut input);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_press(&mut input);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } => {
                self.forward_to_inputs(&mut input);
                if self.syntax_pressed.replace(false) {
                    self.toggle_syntax_highlight(input.state);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        let line = f64::from(ui::tokens::Metrics::BASE.min_pointer_target);
                        (f64::from(*x) * line, f64::from(*y) * line)
                    }
                    MouseScrollDelta::PixelDelta(p) => {
                        (p.x / ctx.scale_factor, p.y / ctx.scale_factor)
                    }
                };
                // Shift turns a vertical wheel sideways.
                let (dx, dy) = if ctx.modifiers.state().shift_key() && dx == 0.0 {
                    (dy, 0.0)
                } else {
                    (dx, dy)
                };
                if self.confirm.is_none()
                    && let Some(pos) = self.pointer
                {
                    self.scroll_at(pos, dx, dy);
                }
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput { .. } => {
                self.handle_key(
                    event,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.taffy,
                    ctx.handled,
                );
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
            .map(|(index, (target, _, node))| {
                // The ordinal is only the synthetic-tree geometry key. The
                // action target is semantic and stable across responsive
                // layouts, so assistive technology never activates a
                // different control after a row appears or disappears.
                node.with_key(index as u64).with_action_target(target)
            })
            .collect();
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Group)
                .with_name("Git")
                .with_description(if self.remote_job.is_some() {
                    ui::i18n!("git-remote-running")
                } else if self.awaiting_repo {
                    "Open a repository".to_string()
                } else {
                    "Repository changes, history, branches and remotes".to_string()
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
            .map(|(_, rect, _)| *rect)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        use ui::a11y::Action;
        let (state, path) = ui::doc::document(state, path);

        let field = match target {
            "git.open-path" => Some(Field::Open),
            "git.commit-message" => Some(Field::Commit),
            "git.new-branch-name" => Some(Field::Branch),
            _ => None,
        };
        if let Some(field) = field {
            if matches!(action, Action::Focus | Action::Activate) {
                self.set_focus(Some(field));
                return true;
            }
            let input = match field {
                Field::Open => &mut self.open_input,
                Field::Commit => &mut self.commit_input,
                Field::Branch => &mut self.branch_input,
            };
            return input.perform_action(action, state, path);
        }
        if !matches!(action, Action::Activate | Action::ToggleCheck) {
            return false;
        }
        if target == "git.open-repository" && self.awaiting_repo {
            self.open_repo_from_input();
            return true;
        }
        let Some(hit) = self.semantic_hit(target) else {
            return false;
        };
        self.activate(hit, state);
        true
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if self.remote_job.as_ref()?.invocation != Some(invocation) {
            return None;
        }
        // libgit2 operations are currently non-cancellable. In particular,
        // never report a cancelled push when the server may have accepted it.
        self.drain_remote();
        let job = self.remote_job.as_mut()?;
        if let Some(result) = job.result.take() {
            self.remote_job = None;
            return Some(match result {
                Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                Err(message) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "git.remote_failed".into(),
                        message,
                        details: None,
                        retryable: false,
                    },
                },
            });
        }
        if !job.progress_sent {
            job.progress_sent = true;
            return Some(tilo_actions::InvocationUpdate::Progress {
                invocation,
                sequence: 1,
                phase: "git.remote".into(),
                completed: None,
                total: None,
                message: Some(ui::i18n!("git-remote-running")),
            });
        }
        None
    }

    fn needs_animation_frame(&self) -> bool {
        // Redraw while the list column slides, while a transfer runs, and
        // while an embedded text field is focused so its caret keeps
        // blinking.
        (self.screen.has_list() && self.drawer.is_animating())
            || self
                .remote_job
                .as_ref()
                .is_some_and(|job| job.result.is_none())
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

impl ui::resources::PublishesResources for GitClientApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the repository door — keep only live working-tree status and in-flight
    /// remote jobs.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        // The framework root already owns the repository door.
        if self.is_hosted() && id == "git_client.repo" {
            return None;
        }
        if let Some(found) = self.publish_resource(doc, id) {
            return Some(found);
        }
        if self.is_hosted() {
            return crate::resource::live_extras(self)
                .into_iter()
                .find(|resource| resource.id.as_str() == id);
        }
        None
    }
}

impl ui::undo::CommittedActions for GitClientApp {
    /// Hand the host the document heads captured before an on-screen preference
    /// control dispatched, so it records exactly one undo boundary bracketing
    /// it — the same path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl GitClientApp {
    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        use crate::actions::GitClientOperation;
        if type_id != crate::actions::GIT_CLIENT_OPERATION_TYPE {
            return Ok(None);
        }
        let operation: crate::actions::GitClientOperation =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        let remote = match (kind, operation) {
            (tilo_actions::OperationKind::Effect, GitClientOperation::Fetch { remote }) => {
                Some((RemoteAction::Fetch, remote))
            }
            (tilo_actions::OperationKind::Effect, GitClientOperation::Pull { remote }) => {
                Some((RemoteAction::Pull, remote))
            }
            (tilo_actions::OperationKind::Effect, GitClientOperation::Push { remote }) => {
                Some((RemoteAction::Push, remote))
            }
            _ => None,
        };
        if let Some((action, remote)) = remote {
            self.start_remote(action, Some(remote), Some(invocation))?;
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        self.answer_resource_operation(kind, type_id, payload, doc, path)
            .map(|handled| {
                handled.then(|| {
                    ui::widget::ResourceOperationStart::Completed(
                        tilo_actions::ActionOutput::default(),
                    )
                })
            })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive typed protocol match keeps publication-to-handler coverage auditable"
    )]
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if type_id != crate::actions::GIT_CLIENT_OPERATION_TYPE {
            return Ok(false);
        }
        if kind == tilo_actions::OperationKind::Effect && self.repository_busy() {
            return Err(ui::i18n!("git-remote-busy"));
        }
        let operation: crate::actions::GitClientOperation =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        match (kind, operation) {
            (
                tilo_actions::OperationKind::View,
                crate::actions::GitClientOperation::ShowChanges,
            ) => {
                self.switch_screen(Screen::Changes);
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::GitClientOperation::ShowHistory,
            ) => {
                self.switch_screen(Screen::History);
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::GitClientOperation::ShowBranches,
            ) => {
                self.switch_screen(Screen::Panels);
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::GitClientOperation::OpenChange { area, path },
            ) => {
                let area = match area.as_str() {
                    "staged" => ChangeArea::Staged,
                    "unstaged" => ChangeArea::Unstaged,
                    "untracked" => ChangeArea::Untracked,
                    _ => return Err("unknown change area".into()),
                };
                let list = match area {
                    ChangeArea::Staged => &self.model.staged,
                    ChangeArea::Unstaged => &self.model.unstaged,
                    ChangeArea::Untracked => &self.model.untracked,
                };
                let index = list
                    .iter()
                    .position(|change| change.path == path)
                    .ok_or_else(|| "changed file no longer exists".to_string())?;
                self.switch_screen(Screen::Changes);
                self.selected_change = Some((area, index));
                self.diff_scroll = 0.0;
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::GitClientOperation::OpenCommit { id },
            ) => {
                let index = self
                    .model
                    .commits
                    .iter()
                    .position(|commit| commit.id == id)
                    .ok_or_else(|| "commit is outside the current history window".to_string())?;
                self.switch_screen(Screen::History);
                self.selected_commit = index;
                self.detail_scroll = 0.0;
            }
            (tilo_actions::OperationKind::Effect, crate::actions::GitClientOperation::StageAll) => {
                self.stage_all();
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::UnstageAll,
            ) => {
                self.unstage_all();
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::StageFile { path },
            ) => {
                let found = self
                    .model
                    .unstaged
                    .iter()
                    .position(|change| change.path == path)
                    .map(|index| (ChangeArea::Unstaged, index))
                    .or_else(|| {
                        self.model
                            .untracked
                            .iter()
                            .position(|change| change.path == path)
                            .map(|index| (ChangeArea::Untracked, index))
                    })
                    .ok_or_else(|| "file is no longer unstaged".to_string())?;
                self.toggle_stage(found.0, found.1);
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::UnstageFile { path },
            ) => {
                let index = self
                    .model
                    .staged
                    .iter()
                    .position(|change| change.path == path)
                    .ok_or_else(|| "file is no longer staged".to_string())?;
                self.toggle_stage(ChangeArea::Staged, index);
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::DiscardFile { path },
            ) => {
                let found = self
                    .model
                    .unstaged
                    .iter()
                    .position(|change| change.path == path)
                    .map(|index| (ChangeArea::Unstaged, index))
                    .or_else(|| {
                        self.model
                            .untracked
                            .iter()
                            .position(|change| change.path == path)
                            .map(|index| (ChangeArea::Untracked, index))
                    })
                    .ok_or_else(|| "file has no worktree changes".to_string())?;
                if self.repo_path.is_none() {
                    return Err("discard is unavailable in the demonstration repository".into());
                }
                self.discard_file(found.0, found.1);
            }
            (
                tilo_actions::OperationKind::Effect,
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
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::CreateBranch { name },
            ) => {
                if name.trim().is_empty() {
                    return Err("branch name cannot be empty".into());
                }
                if self.repo_path.is_none() {
                    return Err("branch creation requires a real repository".into());
                }
                self.branch_input.set_text(&name);
                self.create_new_branch();
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::CheckoutBranch { name },
            ) => self.apply_branch_action(BranchAction::Checkout(name)),
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::DeleteBranch { name },
            ) => self.apply_branch_action(BranchAction::Delete(name)),
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::ApplyStash { index },
            ) => self.apply_branch_action(BranchAction::StashApply(index)),
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::SaveStash { message },
            ) => {
                let path = self
                    .repo()
                    .ok_or_else(|| "stashing requires a real repository".to_string())?;
                crate::repo::stash_save(&path, &message).map_err(|error| error.to_string())?;
                self.last_error = None;
                self.reload();
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::Fetch { .. }
                | crate::actions::GitClientOperation::Pull { .. }
                | crate::actions::GitClientOperation::Push { .. },
            ) => return Err("remote operations require lifecycle-aware action dispatch".into()),
            _ => return Err("operation kind does not match Git operation".into()),
        }
        if kind == tilo_actions::OperationKind::Effect
            && let Some(error) = &self.last_error
        {
            return Err(error.clone());
        }
        Ok(true)
    }
}

impl GitClientApp {
    /// Forward one event to a single embedded widget (a text field or the
    /// Syntax toggle), returning whether it consumed the event.
    fn fwd(
        widget: &mut dyn Widget,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        let mut handled = false;
        let mut ft: Option<NodeId> = None;
        widget.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor,
                modifiers: &modifiers,
                state,
                path,
                handled: &mut handled,
                focus_target: &mut ft,
                node_id: NodeId::new(0),
                taffy,
            },
        );
        handled
    }

    /// Forward a pointer event to every text field (so an in-flight drag-select
    /// keeps updating regardless of which field started it) and to the
    /// Syntax toggle.
    fn forward_to_inputs(&mut self, ctx: &mut InputEventCtx<'_>) {
        let modal = self.drawer_is_modal() || self.confirm.is_some();
        let tool = (!modal && self.screen == Screen::Changes)
            .then_some(&mut self.syntax_tool as &mut dyn Widget);
        let widgets: [Option<&mut dyn Widget>; 4] = [
            Some(&mut self.commit_input),
            Some(&mut self.branch_input),
            Some(&mut self.open_input),
            tool,
        ];
        for widget in widgets.into_iter().flatten() {
            Self::fwd(
                widget,
                ctx.event,
                ctx.scale_factor,
                ctx.modifiers,
                &mut *ctx.state,
                ctx.path,
                ctx.taffy,
            );
        }
    }

    /// Route a left press: the confirmation sheet when it is up, a text
    /// field (focus and caret), the Syntax toggle, then the target under the
    /// pointer. A press anywhere else blurs the focused field.
    fn handle_press(&mut self, ctx: &mut InputEventCtx<'_>) {
        let Some(pos) = self.pointer else {
            return;
        };
        if self.confirm.is_some() {
            self.confirm_press(pos);
            return;
        }
        if !self.drawer_is_modal()
            && self.screen == Screen::Changes
            && rect_hit(self.syntax_tool.get_rect(), pos)
        {
            self.set_focus(None);
            Self::fwd(
                &mut self.syntax_tool,
                ctx.event,
                ctx.scale_factor,
                ctx.modifiers,
                &mut *ctx.state,
                ctx.path,
                ctx.taffy,
            );
            return;
        }
        let hit = self
            .targets
            .iter()
            .rev()
            .find(|target| target.rect.contains(pos))
            .map(|target| target.hit.clone());
        let field = match hit {
            Some(Hit::Change(ChangesHit::FocusMessage)) => Some(Field::Commit),
            Some(Hit::Panel(PanelHit::BranchName)) => Some(Field::Branch),
            Some(Hit::OpenPath) => Some(Field::Open),
            _ => None,
        };
        if let Some(field) = field {
            self.set_focus(Some(field));
            let input = match field {
                Field::Commit => &mut self.commit_input,
                Field::Branch => &mut self.branch_input,
                Field::Open => &mut self.open_input,
            };
            Self::fwd(
                input,
                ctx.event,
                ctx.scale_factor,
                ctx.modifiers,
                &mut *ctx.state,
                ctx.path,
                ctx.taffy,
            );
            return;
        }
        self.set_focus(None);
        if let Some(hit) = hit {
            self.activate(hit, ctx.state);
        }
    }

    /// Submit the focused field's action (Enter).
    fn submit_focused(&mut self, field: Field) {
        match field {
            Field::Commit => self.do_commit(),
            Field::Branch => self.create_new_branch(),
            Field::Open => self.open_repo_from_input(),
        }
    }

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

        // The confirmation sheet holds the keyboard: Escape cancels it.
        if self.confirm.is_some() {
            if let Key::Named(NamedKey::Escape) = &ev.logical_key {
                self.confirm = None;
            }
            *handled = true;
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

        // The system shortcut that shows and hides the list column.
        if self.screen.has_list()
            && self.conventions.command_modifier_down(modifiers.state())
            && let Key::Character(c) = &ev.logical_key
            && ui::platform::HostShortcut::resolve(
                self.conventions,
                c,
                modifiers.state().shift_key(),
            ) == Some(ui::platform::HostShortcut::Sidebar)
        {
            self.drawer.toggle();
            *handled = true;
            return;
        }

        // Escape dismisses an open drawer.
        if let Key::Named(NamedKey::Escape) = &ev.logical_key
            && self.drawer_is_modal()
        {
            self.drawer.close();
            *handled = true;
            return;
        }

        if !self.drawer_is_modal()
            && let Key::Character(s) = &ev.logical_key
        {
            // 1/2/3 switch screens; d flips the diff layout.
            let screen = match s.as_str() {
                "1" => Some(Screen::Changes),
                "2" => Some(Screen::History),
                "3" => Some(Screen::Panels),
                _ => None,
            };
            if let Some(screen) = screen {
                self.switch_screen(screen);
                *handled = true;
                return;
            }
            if s.as_str() == "d" {
                self.toggle_diff_layout(state);
                *handled = true;
                return;
            }
        }

        if self.navigate_list(&ev.logical_key) {
            *handled = true;
        }
    }

    /// Arrow keys move the selection in the active list. Returns whether
    /// `key` did.
    fn navigate_list(&mut self, key: &Key) -> bool {
        let step = match key {
            Key::Named(NamedKey::ArrowDown) => 1,
            Key::Named(NamedKey::ArrowUp) => -1,
            _ => return false,
        };
        match self.screen {
            Screen::History => {
                self.selected_commit = if step > 0 {
                    (self.selected_commit + 1).min(self.model.commits.len().saturating_sub(1))
                } else {
                    self.selected_commit.saturating_sub(1)
                };
                self.detail_scroll = 0.0;
                true
            }
            Screen::Changes => {
                self.select_next_change(step);
                true
            }
            Screen::Panels => false,
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
        self.diff_hscroll = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::Widget;

    /// The sample app laid out at `w` × 800, painted once so every target is
    /// the production geometry.
    fn app_at(w: f64, screen: Screen) -> GitClientApp {
        let mut app = GitClientApp::sample();
        app.screen = screen;
        app.rect = Rect::new(0.0, 0.0, w, 800.0);
        app.drawer.resolve(app.rect);
        app.drawer.snap(app.drawer.is_open());
        draw_frame(&mut app);
        app
    }

    fn draw_frame(app: &mut GitClientApp) {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        Widget::draw(
            app,
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::default(),
        );
    }

    fn target(app: &GitClientApp, hit: &Hit) -> Option<Rect> {
        app.targets
            .iter()
            .rev()
            .find(|t| t.hit == *hit)
            .map(|t| t.rect)
    }

    fn press(app: &mut GitClientApp, pos: Point, doc: &mut AutoCommit) {
        app.pointer = Some(pos);
        let hit = app
            .targets
            .iter()
            .rev()
            .find(|t| t.rect.contains(pos))
            .map(|t| t.hit.clone());
        if let Some(hit) = hit {
            app.activate(hit, doc);
        }
        draw_frame(app);
    }

    /// The first sync publishes preferences through the action sink.
    #[test]
    fn a_fresh_client_publishes_preferences_through_the_sink() {
        let mut app = GitClientApp::sample();
        let mut state = ui::State::new();
        Widget::sync(&mut app, &mut state.doc, &state.root);
        let recorded = app
            .actions
            .last_dispatched()
            .expect("the first sync submits a preference action");
        assert_eq!(
            recorded.type_id.as_str(),
            crate::actions::GIT_CLIENT_ACTION_TYPE
        );
        assert_eq!(
            crate::settings::Settings::read_only(&state.doc).diff_layout,
            app.settings.diff_layout
        );
    }

    /// A wide window docks the list column beside the content; a narrow one
    /// shows the content with the list in a drawer the toggle opens, a scrim
    /// press closes, and choosing a file closes too. Branches has no list.
    #[test]
    fn the_list_docks_wide_and_is_a_drawer_narrow() {
        let wide = app_at(1280.0, Screen::Changes);
        assert!(wide.drawer.is_docked() && wide.drawer.is_open());
        assert!(wide.drawer.sidebar_rect().width() > 0.0);

        let mut doc = AutoCommit::new();
        let mut narrow = app_at(800.0, Screen::Changes);
        assert!(narrow.drawer.is_overlay() && !narrow.drawer.is_open());
        let toggle = target(&narrow, &Hit::SidebarToggle).expect("the toggle is in the header");
        press(&mut narrow, toggle.center(), &mut doc);
        narrow.drawer.snap(true);
        draw_frame(&mut narrow);
        assert!(narrow.drawer_is_modal());
        assert!(
            !narrow
                .semantic
                .iter()
                .any(|t| matches!(t.hit, Hit::Screen(_))),
            "what the drawer covers leaves the semantic tree"
        );
        let row = target(
            &narrow,
            &Hit::Change(ChangesHit::SelectFile(ChangeArea::Unstaged, 0)),
        )
        .expect("an unstaged row in the drawer");
        press(&mut narrow, row.center(), &mut doc);
        assert!(
            !narrow.drawer.is_open(),
            "choosing a file closes the drawer"
        );
        assert_eq!(narrow.selected_change, Some((ChangeArea::Unstaged, 0)));

        let mut narrow = app_at(800.0, Screen::History);
        narrow.drawer.snap(true);
        draw_frame(&mut narrow);
        press(&mut narrow, Point::new(790.0, 400.0), &mut doc);
        assert!(
            !narrow.drawer.is_open(),
            "a press on the scrim closes the drawer"
        );

        let panels = app_at(800.0, Screen::Panels);
        assert!(target(&panels, &Hit::SidebarToggle).is_none());
    }

    /// Header controls stay inside the window and never overlap, at every
    /// width, on every screen.
    #[test]
    fn header_controls_lay_out_inside_the_window_without_overlap() {
        let theme = Theme::dark();
        let header_h = GitClientApp::header_height(&theme);
        for width in [390.0, 560.0, 800.0, 1280.0] {
            for screen in Screen::ALL {
                let app = app_at(width, screen);
                let header: Vec<&Target> = app
                    .targets
                    .iter()
                    .filter(|t| {
                        matches!(
                            t.hit,
                            Hit::SidebarToggle
                                | Hit::Screen(_)
                                | Hit::DiffLayout(_)
                                | Hit::Remote(_)
                        )
                    })
                    .collect();
                assert!(
                    header
                        .iter()
                        .filter(|t| matches!(t.hit, Hit::Screen(_)))
                        .count()
                        == 3,
                    "{width}: every screen is reachable"
                );
                for (i, a) in header.iter().enumerate() {
                    assert!(
                        a.rect.x0 >= -0.01 && a.rect.x1 <= width + 0.01 && a.rect.y1 <= header_h,
                        "{width}: {:?} at {:?} leaves the header",
                        a.hit,
                        a.rect
                    );
                    for b in header.iter().skip(i + 1) {
                        assert!(
                            a.rect.intersect(b.rect).area() <= 0.01,
                            "{width}: {:?} overlaps {:?}",
                            a.hit,
                            b.hit
                        );
                    }
                }
            }
        }
    }

    /// The header's layout control equals the palette action: it mutates
    /// the shared document, records one undo boundary, and dispatches the
    /// byte-identical action Ctrl+K would.
    #[test]
    fn the_layout_control_dispatches_the_palette_action() {
        let mut app = app_at(1280.0, Screen::Changes);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(Settings::read_only(&doc).diff_layout, DiffLayout::Unified);
        let resources = ui::resources::resources(&app, &doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "diff-layout")
            .unwrap();
        let palette_action = match &spec.input {
            tilo_actions::ActionInput::Immediate(enc) => enc.clone(),
            tilo_actions::ActionInput::Prompted { .. }
            | tilo_actions::ActionInput::Deferred { .. } => {
                panic!("diff-layout is an immediate action")
            }
        };
        let split = target(&app, &Hit::DiffLayout(DiffLayout::SideBySide))
            .expect("the layout control is in the header");
        press(&mut app, split.center(), &mut doc);
        assert_eq!(
            Settings::read_only(&doc).diff_layout,
            DiffLayout::SideBySide
        );
        assert!(ui::undo::take_committed_action(&mut app).is_some());
        assert_eq!(app.last_dispatched(), Some(&palette_action));
    }

    /// Destructive steps wait for confirmation: discarding a file or deleting
    /// a branch opens the sheet, Cancel and Escape leave everything as it
    /// was, and confirming discards.
    #[test]
    fn destructive_steps_wait_for_confirmation() {
        let mut doc = AutoCommit::new();
        let mut app = app_at(1280.0, Screen::Changes);
        app.activate(
            Hit::Change(ChangesHit::SelectFile(ChangeArea::Unstaged, 0)),
            &mut doc,
        );
        draw_frame(&mut app);
        let before = app.model.unstaged.len();
        let discard = target(&app, &Hit::Diff(DiffHit::DiscardFile)).expect("discard file");
        press(&mut app, discard.center(), &mut doc);
        assert!(matches!(app.confirm, Some(Confirm::DiscardFile { .. })));
        assert_eq!(app.model.unstaged.len(), before, "nothing is lost yet");
        app.confirm = None;

        app.activate(Hit::Diff(DiffHit::DiscardFile), &mut doc);
        app.perform_confirmed();
        assert_eq!(app.model.unstaged.len(), before - 1, "confirming discards");

        app.activate(
            Hit::Panel(PanelHit::Branch(BranchAction::Delete(
                "fix/scene-leak".into(),
            ))),
            &mut doc,
        );
        assert_eq!(
            app.confirm,
            Some(Confirm::DeleteBranch("fix/scene-leak".into()))
        );
    }

    /// Semantic parity: open-repository and text edits reach their inputs,
    /// every synthetic control sits at its drawn rect, and activating one
    /// runs the same action a press does.
    #[test]
    fn semantic_controls_reach_the_production_actions() {
        let directory = tempfile::tempdir().unwrap();
        let _repository = git2::Repository::init(directory.path()).unwrap();
        let mut app = GitClientApp::open_empty();
        app.open_input.set_text(directory.path().to_str().unwrap());
        let mut doc = automerge::AutoCommit::new();
        assert!(Widget::perform_semantic_action(
            &mut app,
            "git.open-repository",
            &ui::a11y::Action::Activate,
            &mut doc,
            &automerge::ROOT,
        ));
        assert!(!app.test_awaiting_repo());
        assert_eq!(app.test_last_error(), None);

        let mut app = GitClientApp::open_empty();
        for (target, text) in [
            ("git.open-path", r"C:\Work\Grün 🌍"),
            ("git.commit-message", "Keep Unicode: Grün 🌍"),
            ("git.new-branch-name", "feature/grün"),
        ] {
            assert!(Widget::perform_semantic_action(
                &mut app,
                target,
                &ui::a11y::Action::SetValue(text.into()),
                &mut doc,
                &automerge::ROOT,
            ));
        }
        assert_eq!(app.open_input.value(), r"C:\Work\Grün 🌍");
        assert_eq!(app.commit_input.value(), "Keep Unicode: Grün 🌍");
        assert_eq!(app.branch_input.value(), "feature/grün");
        assert!(app.test_awaiting_repo(), "editing a path must not open it");

        let mut app = app_at(1280.0, Screen::Changes);
        let node = Widget::accessibility(&app).expect("git client is semantic");
        let targets: std::collections::HashSet<_> = node
            .children
            .iter()
            .filter_map(|child| child.action_target.as_deref())
            .collect();
        assert_eq!(targets.len(), node.children.len(), "targets are unique");
        for expected in [
            "git.screen.changes",
            "git.screen.history",
            "git.screen.branches",
            "git.diff-layout.split",
            "git.syntax-highlight",
            "git.commit-message",
            "git.navigation-toggle",
            "git.change.Stage:Unstaged:src/app.rs",
        ] {
            assert!(targets.contains(expected), "missing {expected}");
        }
        for (index, child) in node.children.iter().enumerate() {
            assert_eq!(
                Widget::synthetic_accessibility_rect(&app, &[index]),
                child.bounds.map(ui::a11y::AccessBounds::to_rect),
                "semantic control {index} must activate inside the visible control"
            );
        }
        let before = app.model.unstaged.len();
        assert!(Widget::perform_semantic_action(
            &mut app,
            "git.change.Stage:Unstaged:src/app.rs",
            &ui::a11y::Action::Activate,
            &mut doc,
            &automerge::ROOT,
        ));
        assert_eq!(
            app.model.unstaged.len(),
            before - 1,
            "the row's hover action is reachable"
        );
        assert!(Widget::perform_semantic_action(
            &mut app,
            "git.screen.history",
            &ui::a11y::Action::Activate,
            &mut doc,
            &automerge::ROOT,
        ));
        assert_eq!(app.screen, Screen::History);
        assert!(Widget::perform_semantic_action(
            &mut app,
            "git.commit-message",
            &ui::a11y::Action::Focus,
            &mut doc,
            &automerge::ROOT,
        ));
        assert_eq!(app.focus, Some(Field::Commit));
    }
}
