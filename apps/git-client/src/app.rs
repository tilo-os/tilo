//! Root widget for the git client.
//!
//! Owns the in-memory [`RepoModel`], the diffs, the active screen and all
//! transient selection and scroll state. Durable preferences live in the
//! CRDT via [`Settings`]. The window's bar holds the client's controls
//! (`window_bar`): the repository as the location, the screens as its
//! views, the list column's toggle, Fetch, Pull and Push, the diff's layout
//! and syntax highlighting, and History. The widget paints no header row:
//! the list column of the Changes and History screens stands on the sidebar
//! region through a [`SidebarDrawer`] (docked when the window is wide, a
//! floating drawer when it is narrow), and the screen's content on the
//! window ground. Presses are routed through the targets the last paint
//! recorded.

mod commits;
mod keys;
mod window_bar;

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

#[cfg(test)]
use automerge::AutoCommit;
use automerge::ObjId;
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::actions::GitClientEffect;
use ui::framework::{EffectAnswer, EffectCx, EffectReply, Effected, LiveRow, RunMode};
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::ActionOutput;
use ui::tilo_resource::Invoke;
use ui::tokens::TextRole;
use ui::widget::scrollbar::{BarFade, PressOutcome, ScrollMetrics, Scrollbar};
use ui::widget::{
    Banner, ButtonFace, ControlSize, EmptyState, EmptyStateContent, ModalAction, ModalHit,
    ModalLayout, ModalSpec, SidebarDrawer, TextInput, TextInputKind, Widget,
};

use crate::changes::{self, ChangesHit};
use crate::diffview::{self, DiffHit, HunkAction};
use crate::graph::{self, GraphLayout};
use crate::history;
use crate::i18n::{t, t_args};
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
    Branch,
    /// The repository-path field on the "open a repository" screen.
    Open,
    WorktreeBranch,
    WorktreePath,
    WorktreeStart,
}

/// Host event resources retained while one event is routed through embedded
/// text inputs and the app's own geometry handlers.
struct InputEventCtx<'a> {
    event: &'a WindowEvent,
    scale_factor: f64,
    modifiers: winit::event::Modifiers,
    state: ui::widget::Edits<'a>,
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

    fn label(self) -> String {
        t(match self {
            Screen::Changes => "git-screen-changes",
            Screen::History => "git-screen-history",
            Screen::Panels => "git-screen-branches",
        })
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
    /// Switch to a branch while the working tree holds uncommitted work:
    /// the person chooses what becomes of it.
    SwitchBranch {
        name: String,
        /// Whether there is uncommitted work at all (a clean tree switches
        /// without asking).
        dirty: bool,
    },
}

/// One answer to a confirmation sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfirmChoice {
    Discard,
    DeleteBranch,
    /// Switch, taking the uncommitted work along.
    SwitchCarrying,
    /// Switch, parking the uncommitted work on the stash first.
    SwitchStashing,
}

/// How an answer reads: its label, and whether it destroys something.
struct ChoiceFace {
    choice: ConfirmChoice,
    label: String,
    danger: bool,
}

impl Confirm {
    fn title(&self) -> String {
        match self {
            Confirm::DiscardFile { name, .. } => {
                t_args("git-confirm-discard-file-title", &[("name", name.into())])
            }
            Confirm::DiscardHunk(_) => t("git-confirm-discard-hunk-title"),
            Confirm::DeleteBranch(name) => {
                t_args("git-confirm-delete-branch-title", &[("name", name.into())])
            }
            Confirm::SwitchBranch { name, .. } => {
                t_args("git-confirm-switch-title", &[("name", name.into())])
            }
        }
    }

    fn body(&self) -> String {
        t(match self {
            Confirm::DiscardFile { area, .. } if *area == ChangeArea::Untracked => {
                "git-confirm-discard-untracked-body"
            }
            Confirm::DiscardFile { .. } | Confirm::DiscardHunk(_) => "git-confirm-discard-body",
            Confirm::DeleteBranch(_) => "git-confirm-delete-branch-body",
            Confirm::SwitchBranch { dirty: true, .. } => "git-confirm-switch-dirty-body",
            Confirm::SwitchBranch { dirty: false, .. } => "git-confirm-switch-body",
        })
    }

    /// The answers this sheet offers, in reading order after Cancel.
    fn choices(&self) -> Vec<ChoiceFace> {
        match self {
            Confirm::DiscardFile { .. } | Confirm::DiscardHunk(_) => vec![ChoiceFace {
                choice: ConfirmChoice::Discard,
                label: t("git-confirm-discard"),
                danger: true,
            }],
            Confirm::DeleteBranch(_) => vec![ChoiceFace {
                choice: ConfirmChoice::DeleteBranch,
                label: t("git-delete-branch"),
                danger: true,
            }],
            Confirm::SwitchBranch { dirty, .. } if *dirty => vec![
                ChoiceFace {
                    choice: ConfirmChoice::SwitchStashing,
                    label: t("git-switch-stash"),
                    danger: false,
                },
                ChoiceFace {
                    choice: ConfirmChoice::SwitchCarrying,
                    label: t("git-switch-carry"),
                    danger: false,
                },
            ],
            // A clean tree has nothing to decide: Cancel, or go.
            Confirm::SwitchBranch { .. } => vec![ChoiceFace {
                choice: ConfirmChoice::SwitchCarrying,
                label: t("git-switch-go"),
                danger: false,
            }],
        }
    }
}

/// Something a press, a key or an assistive action can do.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Hit {
    /// The scrim behind an open drawer: dismisses it.
    Scrim,
    DismissError,
    Change(ChangesHit),
    Diff(DiffHit),
    /// A commit row, by index.
    Commit(usize),
    /// A changed file of the selected commit, by index: opens its diff.
    CommitFile(usize),
    /// The way back from a commit file's diff to the commit itself.
    BackToCommit,
    /// Keep one side of a conflicted file.
    ResolveConflict(crate::repo::ConflictSide),
    /// The conflicted file's own sentence: what keeping each side costs.
    ConflictNote,
    Panel(PanelHit),
    OpenPath,
    /// Choose the repository's folder with the system's file chooser.
    ChooseRepository,
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
    /// The ground the content stands on, which the edge fades blend into.
    ground: vello::peniko::Color,
}

/// One owned repository operation. Completion stays available until its action
/// invocation consumes it, even when a normal UI sync receives it first.
struct RemoteJob {
    receiver: Receiver<Result<(), String>>,
    reply: Option<EffectReply>,
    /// Palette label while the transfer is in flight (a live row).
    label: String,
}

pub struct GitClientApp {
    model: RepoModel,
    diffs: DiffMap,
    graph_layout: GraphLayout,

    /// Framework-owned action dispatch: opening a repository and recording
    /// what collaborators see of it go through this, as do the effects the
    /// controls ask the host for.
    dispatcher: Option<ui::framework::Dispatcher<crate::resource::GitClient>>,
    mode: RunMode,
    opened: Option<String>,
    presented: bool,
    preferences: Option<ui::framework::Setting<Settings>>,
    control_actions: Option<ui::framework::Actions<crate::resource::GitClient>>,

    /// No repository is bound: keep the "open a repository" empty state
    /// until an explicit path or restored record selects a repository.
    awaiting_repo: bool,
    focus: Option<Field>,
    /// Commit-message field (toolkit `TextInput`, local mode).
    commit_input: TextInput,
    /// New-branch name field (toolkit `TextInput`, local mode).
    branch_input: TextInput,
    worktree_branch_input: TextInput,
    worktree_path_input: TextInput,
    worktree_start_input: TextInput,
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

    /// The changed files of the commits whose detail has been opened, by
    /// commit id. Read on demand: walking the history for every commit's
    /// tree diff up front costs a second on a real repository.
    commit_files: HashMap<String, Vec<FileChange>>,
    /// The commit diff on screen: the commit and the file whose changes the
    /// detail column shows instead of the commit's own page.
    commit_diff: Option<(String, String)>,
    /// The commit diffs shown so far, by commit and path.
    commit_diffs: HashMap<(String, String), crate::model::FileDiff>,
    /// Which file of the selected commit is under the keyboard, and whether
    /// the keyboard is in the file list or the commit list.
    file_selection: Option<usize>,
    /// Which local branch of the branches page is under the keyboard.
    branch_selection: Option<usize>,
    /// What keeping each side of the conflicted file under review would
    /// discard, and which file it was read for. Read once per conflict: the
    /// choice is worth naming, not re-reading every frame.
    conflict_cost: Option<(String, Option<crate::repo::ConflictCost>)>,
    /// What the last action did, said once: a branch switched, a stash made,
    /// a push sent. Only failures used to be reported, so work that
    /// succeeded looked like work that had not happened.
    notice: Option<String>,

    /// A destructive step waiting for confirmation, and where its sheet was
    /// laid out last.
    confirm: Option<Confirm>,
    confirm_layout: Option<ModalLayout>,

    /// The list column of the Changes and History screens: docked beside the
    /// content when the window is wide, a floating drawer when it is narrow.
    drawer: SidebarDrawer,
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
    /// The scroll bar track of every region of the last paint whose content
    /// overflows, with how its content stood.
    scroll_bars: Vec<(Scroller, Rect, ScrollMetrics)>,
    /// The toolkit scroll bar of the region whose thumb is held or under
    /// the pointer, and which region that is.
    scrollbar: Scrollbar,
    scrollbar_region: Option<Scroller>,
    /// The bars show while a region scrolls or the pointer is on one, then
    /// fade out like every toolkit scroll view's.
    bar_fade: BarFade,
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
        Self::open_empty()
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

    /// Construct from an explicit model — used by the sample path and tests.
    pub fn from_model(mut model: RepoModel, diffs: DiffMap) -> Self {
        let graph_layout = graph::layout(&mut model.commits);
        // Pre-select the first changed file so the diff pane is populated.
        let selected_change = first_change(&model);
        let commit_input = TextInput::local()
            .with_id("git.commit-message")
            .with_accessible_name(t("git-commit-message"))
            .with_placeholder(t("git-commit-message"));
        let branch_input = TextInput::local()
            .with_id("git.branch-name")
            .with_accessible_name(t("git-new-branch-name"))
            .with_kind(TextInputKind::Name)
            .with_placeholder(t("git-branch-name"));
        let worktree_branch_input = TextInput::local()
            .with_id("git.worktree-branch")
            .with_accessible_name(t("git-worktree-branch"))
            .with_kind(TextInputKind::Name)
            .with_placeholder(t("git-worktree-branch"));
        let worktree_path_input = TextInput::local()
            .with_id("git.worktree-path")
            .with_accessible_name(t("git-worktree-path"))
            .with_kind(TextInputKind::Path)
            .with_placeholder(t("git-worktree-path"));
        let mut worktree_start_input = TextInput::local()
            .with_id("git.worktree-start")
            .with_accessible_name(t("git-worktree-start"))
            .with_kind(TextInputKind::Code)
            .with_placeholder(t("git-worktree-start"));
        worktree_start_input.set_text("HEAD");
        let open_input = TextInput::local()
            .with_id("git.repository-path")
            .with_accessible_name(t("git-repository-path"))
            .with_kind(TextInputKind::Path)
            .with_placeholder(t("git-repository-path"));
        Self {
            dispatcher: None,
            mode: RunMode::Live,
            opened: None,
            presented: false,
            preferences: None,
            control_actions: None,
            model,
            diffs,
            graph_layout,
            awaiting_repo: false,
            focus: None,
            commit_input,
            branch_input,
            worktree_branch_input,
            worktree_path_input,
            worktree_start_input,
            open_input,
            repo_path: None,
            last_error: None,
            remote_job: None,
            settings: Settings::default(),
            shared_repo_fingerprint: None,
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
            commit_files: HashMap::new(),
            commit_diff: None,
            commit_diffs: HashMap::new(),
            file_selection: None,
            branch_selection: None,
            conflict_cost: None,
            notice: None,
            confirm: None,
            confirm_layout: None,
            // Both lists are content the person reads: the overview width,
            // docked or as a drawer, so a row reads the same in both.
            // The diff beside them keeps the content minimum plus a rail's
            // width for its line numbers before the list becomes a drawer.
            drawer: SidebarDrawer::new()
                .docked_width(f64::from(ui::tokens::Metrics::BASE.overview_width))
                .overlay_width(f64::from(ui::tokens::Metrics::BASE.overview_width))
                .breakpoint(f64::from(
                    ui::tokens::Metrics::BASE.overview_width
                        + ui::tokens::Metrics::BASE.content_min_width
                        + ui::tokens::Metrics::BASE.rail_width,
                )),
            staged: Staged::Nothing,
            pointer: None,
            conventions: ui::platform::PlatformConventions::default(),
            targets: Vec::new(),
            semantic: Vec::new(),
            scroll_regions: Vec::new(),
            scroll_bars: Vec::new(),
            scrollbar: Scrollbar::vertical(),
            bar_fade: BarFade::default(),
            scrollbar_region: None,
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
    /// What the last action did, as the banner says it.
    #[doc(hidden)]
    pub fn test_notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }
    /// The branch whose switch is waiting for an answer, if any.
    #[doc(hidden)]
    pub fn test_pending_switch(&self) -> Option<&str> {
        match &self.confirm {
            Some(Confirm::SwitchBranch { name, .. }) => Some(name.as_str()),
            _ => None,
        }
    }
    /// Answer the pending switch the way its sheet's buttons do.
    #[doc(hidden)]
    pub fn test_answer_switch(&mut self, plan: crate::repo::CheckoutPlan) {
        let choice = match plan {
            crate::repo::CheckoutPlan::Carry => ConfirmChoice::SwitchCarrying,
            crate::repo::CheckoutPlan::Stash => ConfirmChoice::SwitchStashing,
        };
        self.perform_confirmed(choice);
    }
    /// Show commit `index` and read its changed files, as opening History does.
    #[doc(hidden)]
    pub fn test_commit_files(&mut self, index: usize) -> Vec<String> {
        self.select_commit(index);
        let Some(commit) = self.model.commits.get(index) else {
            return Vec::new();
        };
        let id = commit.id.clone();
        self.load_commit_files(&id);
        self.commit_files
            .get(&id)
            .map(|files| files.iter().map(|file| file.path.clone()).collect())
            .unwrap_or_default()
    }
    /// Open the diff of file `index` in the selected commit.
    #[doc(hidden)]
    pub fn test_open_commit_file(&mut self, index: usize) {
        self.open_commit_file(index);
    }
    /// The commit diff on screen, as `(commit, file)`.
    #[doc(hidden)]
    pub fn test_commit_diff(&self) -> Option<(String, String)> {
        self.commit_diff.clone()
    }
    /// Select the first conflicted file, as choosing its row does.
    #[doc(hidden)]
    pub fn test_select_conflicted_file(&mut self) {
        for area in [
            crate::model::ChangeArea::Unstaged,
            crate::model::ChangeArea::Staged,
            crate::model::ChangeArea::Untracked,
        ] {
            let found = change_list(&self.model, area)
                .iter()
                .position(|file| file.kind == crate::model::ChangeKind::Conflicted);
            if let Some(index) = found {
                self.selected_change = Some((area, index));
                return;
            }
        }
    }
    /// Keep one side of the selected conflicted file, as `o` and `t` do.
    #[doc(hidden)]
    pub fn test_resolve_conflict(&mut self, side: crate::repo::ConflictSide) {
        self.resolve_conflict(side);
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
                self.notice = None;
                self.reload();
            }
            Err(e) => self.report_error(&e),
        }
    }

    /// Report a failed action, in place of any notice.
    fn report_error(&mut self, error: &anyhow::Error) {
        self.notice = None;
        self.last_error = Some(error.to_string());
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
        reply: Option<EffectReply>,
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
        // The worker's reply wakes the app; nothing polls frame by frame.
        let (sender, receiver) = ui::wake::waking_channel();
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
            reply,
            label,
        });
        Ok(())
    }

    fn drain_remote(&mut self) {
        let Some(job) = self.remote_job.as_mut() else {
            return;
        };
        let result = match job.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(ui::i18n!("git-remote-worker-stopped")),
        };
        self.last_error = result.as_ref().err().cloned();
        // libgit2 operations are not cancellable: the worker's actual outcome
        // is the answer, so a push the server accepted is never reported as
        // cancelled.
        if let Some(reply) = self.remote_job.take().and_then(|job| job.reply) {
            reply.finish(result.map(|()| ActionOutput::default()));
        }
        // A failed operation can still have changed objects/refs. Refresh from
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

    /// Whether there is something staged and a message to commit it with.
    fn can_commit(&self) -> bool {
        !self.model.staged.is_empty() && !self.commit_input.value().trim().is_empty()
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
    /// untracked file). The controls reach here only through the
    /// confirmation sheet.
    fn discard_file(&mut self, area: ChangeArea, idx: usize) {
        if self.repository_busy() {
            return;
        }
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
        let raw = self.open_input.value().trim().to_string();
        let _ = self.open_repository_at(&raw);
    }

    /// Open the repository at `raw`, typed, chosen in the file chooser or
    /// named by an action. Answers why not when there is none.
    fn open_repository_at(&mut self, raw: &str) -> Result<(), String> {
        if self.repository_busy() {
            return Err(ui::i18n!("git-remote-busy"));
        }
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("no repository path was given".into());
        }
        if self.bind_repository(std::path::Path::new(raw)) {
            self.last_error = None;
            self.set_focus(None);
            self.screen = Screen::Changes;
            self.drawer.resolve(self.rect);
            // The record opens this repository from now on, on every device
            // that opens it.
            if let Some(path) = &self.repo_path {
                let path = path.to_string_lossy().into_owned();
                self.opened = Some(path.clone());
                let repository = crate::resource::GitClient::paths().repository;
                self.submit(Invoke::edit(&repository.set(ui::tilo_resource::Atom(path))));
            }
            Ok(())
        } else {
            let error = format!("No git repository at {raw}");
            self.last_error = Some(error.clone());
            Err(error)
        }
    }

    /// Give keyboard focus to one embedded text field (or none), keeping the
    /// toolkit inputs' focus flags mutually exclusive.
    fn set_focus(&mut self, field: Option<Field>) {
        self.focus = field;
        self.commit_input.set_focus(field == Some(Field::Commit));
        self.branch_input.set_focus(field == Some(Field::Branch));
        self.open_input.set_focus(field == Some(Field::Open));
        self.worktree_branch_input
            .set_focus(field == Some(Field::WorktreeBranch));
        self.worktree_path_input
            .set_focus(field == Some(Field::WorktreePath));
        self.worktree_start_input
            .set_focus(field == Some(Field::WorktreeStart));
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
    /// window bar's layout items and the `d` shortcut.
    fn toggle_diff_layout(&mut self) {
        if let Err(error) = self.switch_diff_layout() {
            self.last_error = Some(error);
        }
    }

    /// Flip diff syntax highlighting. Routed to by the window bar's item.
    fn toggle_syntax_highlight(&mut self) {
        if let Err(error) = self.change_preferences(|s| s.syntax_highlight = !s.syntax_highlight) {
            self.last_error = Some(error);
        }
    }

    /// Write the shared repository projection when branches/commits/remotes move.
    fn publish_shared_repo_if_needed(&mut self) {
        if self.awaiting_repo || !self.mode.is_live() {
            return;
        }
        let fingerprint = crate::shared::shared_fingerprint(&self.model);
        if self.shared_repo_fingerprint.as_deref() == Some(fingerprint.as_str()) {
            return;
        }
        let shared = crate::shared::SharedRepo::from_model(&self.model);
        self.submit(Ok(
            crate::resource::GitClient::RECORD_REPOSITORY.with(shared)
        ));
        self.shared_repo_fingerprint = Some(fingerprint);
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
            // Switching branches with work in the tree asks first: the work
            // either comes along or is parked, and the person decides which.
            BranchAction::Checkout(name) => {
                let dirty = self.model.change_count() > 0;
                if dirty {
                    self.confirm = Some(Confirm::SwitchBranch { name, dirty });
                    return;
                }
                crate::repo::checkout_branch(&path, &name)
            }
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

    /// Paint the whole window for this frame, record its targets, and show
    /// where each overflowing region stands.
    fn paint_window(&mut self, ctx: &mut PaintCtx<'_>, presence: &PresenceCtx) {
        self.paint_screens(ctx, presence);
        self.paint_scroll_bars(ctx);
    }

    /// Paint the screens and the list column.
    fn paint_screens(&mut self, ctx: &mut PaintCtx<'_>, presence: &PresenceCtx) {
        self.targets.clear();
        self.semantic.clear();
        self.scroll_regions.clear();
        if self.awaiting_repo {
            self.paint_open(ctx, presence);
            return;
        }
        let theme = ctx.theme;
        let window = self.rect;
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
        let body = Rect::new(column.x0, window.y0, column.x1, window.y1);
        let body = self.paint_banner(ctx, body);
        match self.screen {
            Screen::Changes => self.paint_diff(ctx, body),
            Screen::History => self.paint_detail(ctx, body),
            Screen::Panels => self.paint_panels(ctx, presence, body),
        }

        if !has_list {
            return;
        }
        let panel = self.drawer.sidebar_rect();
        if overlay {
            if self.drawer.reveal() <= 0.001 || panel.width() <= 0.0 {
                return;
            }
            // Everything under the drawer is inert while it is out.
            self.targets.clear();
            self.semantic.clear();
            self.targets.push(Target {
                rect: window,
                hit: Hit::Scrim,
            });
            self.scroll_regions.clear();
            let local = self.drawer.paint_panel(ctx.scene, ctx.transform, theme);
            let mut lctx = PaintCtx::new(&mut *ctx.scene, ctx.transform, &mut *ctx.text, &local);
            self.paint_list_column(&mut lctx, presence, panel, panel);
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
            self.paint_list_column(&mut lctx, presence, panel, full);
        } else {
            self.commit_input.apply_layout_external(Rect::ZERO);
        }
    }

    /// The list column: the Changes or History list. `full` is the column
    /// at its full width; `clip` is what of it is showing.
    fn paint_list_column(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        presence: &PresenceCtx,
        clip: Rect,
        full: Rect,
    ) {
        ctx.scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &clip,
        );
        match self.screen {
            Screen::Changes => self.paint_changes_list(ctx, presence, full, clip),
            Screen::History => self.paint_history_list(ctx, full, clip),
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
            ground: ctx.theme.tokens.colors.surfaces.content,
        });
        self.changes_scroll = self
            .changes_scroll
            .clamp(0.0, (out.content_height - out.viewport_height).max(0.0));
        if !out.commit_enabled {
            let rect = out.commit_button.intersect(clip);
            if rect.width() > 0.0 && rect.height() > 0.0 {
                self.semantic.push(Target {
                    rect,
                    hit: Hit::Change(ChangesHit::Commit),
                });
            }
        }
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
            ground: ctx.theme.tokens.colors.surfaces.content,
        });
    }

    /// An error or a running transfer as a banner at the top of the content;
    /// returns the content area under it.
    fn paint_banner(&mut self, ctx: &mut PaintCtx<'_>, body: Rect) -> Rect {
        use ui::widget::banner::{BannerContent, BannerKind};
        let (kind, title, dismissible) = if let Some(job) = &self.remote_job {
            (BannerKind::Info, job.label.clone(), false)
        } else if let Some(error) = &self.last_error {
            (BannerKind::Error, error.clone(), true)
        } else if let Some(notice) = &self.notice {
            (BannerKind::Success, notice.clone(), true)
        } else {
            return body;
        };
        let theme = ctx.theme;
        let margin = f64::from(theme.tokens.spacing.s3);
        let rect_w = (body.width() - 2.0 * margin).max(1.0);
        let content = BannerContent::new(kind, &title).dismissible(dismissible);
        let h = Banner::height(&content, rect_w, ctx.text, theme);
        let rect = Rect::new(
            body.x0 + margin,
            body.y0 + margin,
            body.x1 - margin,
            body.y0 + margin + h,
        );
        let hover = self.pointer.unwrap_or(Point::new(f64::MIN, f64::MIN));
        let hit = Banner::paint(ctx, rect, &content, hover);
        self.semantic.push(Target {
            rect,
            hit: Hit::DismissError,
        });
        if let Some(dismiss) = hit.dismiss {
            self.targets.push(Target {
                rect: dismiss,
                hit: Hit::DismissError,
            });
        }
        Rect::new(body.x0, rect.y1, body.x1, body.y1.max(rect.y1))
    }

    /// The selected file's diff, or one sentence when there is none.
    fn paint_diff(&mut self, ctx: &mut PaintCtx<'_>, body: Rect) {
        let body = if let Some(file) = self.selected_conflict() {
            let (below, hits) = self.paint_conflict_actions(ctx, body, &file, self.pointer);
            for (rect, hit) in hits {
                self.push_target(rect, hit);
            }
            below
        } else {
            body
        };
        let key = self.current_diff_key();
        let Some(diff) = key.as_ref().and_then(|k| self.diffs.get(k)) else {
            let title = t(if self.model.change_count() == 0 {
                "git-tree-clean"
            } else {
                "git-select-file"
            });
            EmptyState::paint(
                ctx,
                body,
                &EmptyStateContent::new(&title),
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
                // A conflicted file's decision is the band above its diff:
                // staging the markers would settle it as neither side.
                actions: self.selected_conflict().is_none(),
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
            ground: ctx.theme.tokens.colors.surfaces.content,
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
                selected: self.branch_selection,
                worktree_fields: [
                    self.worktree_branch_input.value(),
                    self.worktree_path_input.value(),
                    self.worktree_start_input.value(),
                ],
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
        for (input, rect) in [
            &mut self.worktree_branch_input,
            &mut self.worktree_path_input,
            &mut self.worktree_start_input,
        ]
        .into_iter()
        .zip(out.worktree_fields)
        {
            input.apply_layout_external(rect);
            if rect.width() > 0.0 {
                input.draw(ctx.scene, ctx.transform, ctx.text, ctx.theme, presence);
            }
        }
        let max_y = out.content_height - body.height();
        self.panels_scroll = self.panels_scroll.clamp(0.0, max_y.max(0.0));
        self.scroll_regions.push(ScrollRegion {
            rect: body,
            scroller: Scroller::Panels,
            max_y,
            max_x: 0.0,
            ground: ctx.theme.tokens.colors.surfaces.content,
        });
    }

    /// The open screen's title and what to do, from `at` down; returns
    /// where the path row starts.
    fn paint_open_intro(ctx: &mut PaintCtx<'_>, at: Point, width: f32) -> f64 {
        let theme = ctx.theme;
        let s = &theme.tokens.spacing;
        let mut y = at.y;
        let title = t("git-open-title");
        let title_h = ctx
            .measure_role(&title, TextRole::Title, Some(width))
            .height;
        ctx.text_role(
            &title,
            TextRole::Title,
            theme.text_primary,
            Point::new(at.x, y),
            Some(width),
        );
        y += title_h + f64::from(s.s2);
        let hint = t("git-open-hint");
        let hint_h = ctx.measure_role(&hint, TextRole::Body, Some(width)).height;
        ctx.text_role(
            &hint,
            TextRole::Body,
            theme.text_dim,
            Point::new(at.x, y),
            Some(width),
        );
        y + hint_h + f64::from(s.s6)
    }

    /// The "open a repository" screen: what to do, the path field and the
    /// Open button, and the reason the last attempt failed.
    fn paint_open(&mut self, ctx: &mut PaintCtx<'_>, presence: &PresenceCtx) {
        use ui::widget::banner::{BannerContent, BannerKind};
        let theme = ctx.theme;
        let s = &theme.tokens.spacing;
        let body = self.rect;
        let lane = ui::content_layout::FocusedContentLane::new()
            .max_width(f64::from(theme.tokens.metrics.panel_width_sheet))
            .gutter(f64::from(s.s6))
            .layout(body);
        let width = ui::num::f32_of_f64(lane.width());
        let y = Self::paint_open_intro(
            ctx,
            Point::new(lane.x0, body.y0 + body.height() / 3.0),
            width,
        );

        let h = ButtonFace::height(ControlSize::Regular, theme);
        let label = t("git-open");
        let button_w = ButtonFace::new(Rect::ZERO, &label).intrinsic_width(ctx.text, theme);
        let button = Rect::new(lane.x1 - button_w, y, lane.x1, y + h);
        // Beside the path, the system's file chooser: most people pick the
        // folder rather than type where it is.
        let choose_label = t("git-choose");
        let choose_face =
            || ButtonFace::new(Rect::ZERO, &choose_label).icon(ui::icons::Icon::FolderOpen);
        let choose_w = choose_face().intrinsic_width(ctx.text, theme);
        let choose_x1 = button.x0 - f64::from(s.s2);
        let choose = Rect::new(choose_x1 - choose_w, y, choose_x1, y + h);
        let field = Rect::new(lane.x0, y, choose.x0 - f64::from(s.s2), y + h);
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
        ui::widget::Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(choose, &choose_label)
                .icon(ui::icons::Icon::FolderOpen)
                .hovered(self.pointer.is_some_and(|p| choose.contains(p))),
        );
        self.semantic.push(Target {
            rect: choose,
            hit: Hit::ChooseRepository,
        });
        self.targets.push(Target {
            rect: choose,
            hit: Hit::ChooseRepository,
        });
        let enabled = !self.open_input.value().trim().is_empty();
        ui::widget::Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(button, &label)
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
            let content = BannerContent::new(BannerKind::Error, error);
            let h = Banner::height(&content, lane.width(), ctx.text, theme);
            let banner = Rect::new(lane.x0, top, lane.x1, top + h);
            Banner::paint(ctx, banner, &content, Point::new(f64::MIN, f64::MIN));
        }
    }

    /// Switch the active screen.
    fn switch_screen(&mut self, screen: Screen) {
        self.screen = screen;
        self.set_focus(None);
        self.confirm = None;
    }

    /// Do what `hit` names: the one path pointer presses, keys and
    /// assistive actions share.
    fn activate(&mut self, hit: Hit, _doc: &mut ui::widget::Edits<'_>) {
        if let (Some(dispatcher), Some((row, effect))) =
            (&self.dispatcher, self.effect_for_hit(&hit))
        {
            if let Some(row) = row {
                dispatcher.effect_on(&row, &effect);
            } else {
                dispatcher.effect(&effect);
            }
            return;
        }
        let in_drawer = self.drawer_is_modal();
        match hit {
            Hit::Scrim => {
                if !self
                    .pointer
                    .is_some_and(|point| self.drawer.sidebar_rect().contains(point))
                {
                    self.drawer.close();
                }
            }
            Hit::DismissError => {
                self.last_error = None;
                self.notice = None;
            }
            Hit::Change(change) => self.activate_change(change, in_drawer),
            Hit::Diff(diff) => self.activate_diff(diff),
            Hit::Commit(index) => {
                self.select_commit(index);
                if in_drawer {
                    self.drawer.close();
                }
            }
            Hit::CommitFile(index) => {
                self.file_selection = Some(index);
                self.open_commit_file(index);
            }
            Hit::BackToCommit => {
                self.commit_diff = None;
                self.diff_scroll = 0.0;
                self.diff_hscroll = 0.0;
            }
            Hit::ResolveConflict(side) => self.resolve_conflict(side),
            // The note is read, never pressed.
            Hit::ConflictNote => {}
            Hit::Panel(panel) => self.activate_panel(panel),
            Hit::OpenPath => self.set_focus(Some(Field::Open)),
            Hit::OpenRepository => self.open_repo_from_input(),
            // Dispatched as its effect (`effect_for_hit`); a direct press
            // without the published action does nothing.
            Hit::ChooseRepository => {}
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
            PanelHit::WorktreeBranch => self.set_focus(Some(Field::WorktreeBranch)),
            PanelHit::WorktreePath => self.set_focus(Some(Field::WorktreePath)),
            PanelHit::WorktreeStart => self.set_focus(Some(Field::WorktreeStart)),
            PanelHit::WorktreeInfo(_) | PanelHit::WorktreeError => {}
            PanelHit::CreateWorktree => {
                self.last_error = Some(t("git-worktree-host-required"));
            }
        }
    }

    /// Carry out the answer `choice` to the open confirmation sheet.
    fn perform_confirmed(&mut self, choice: ConfirmChoice) {
        match (self.confirm.take(), choice) {
            (Some(Confirm::DiscardFile { area, index, .. }), ConfirmChoice::Discard) => {
                self.discard_file(area, index);
            }
            (Some(Confirm::DiscardHunk(hunk)), ConfirmChoice::Discard) => {
                self.discard_hunk(hunk);
            }
            (Some(Confirm::DeleteBranch(name)), ConfirmChoice::DeleteBranch) => {
                self.apply_branch_action(BranchAction::Delete(name));
            }
            (Some(Confirm::SwitchBranch { name, .. }), ConfirmChoice::SwitchCarrying) => {
                self.switch_branch(&name, crate::repo::CheckoutPlan::Carry);
            }
            (Some(Confirm::SwitchBranch { name, .. }), ConfirmChoice::SwitchStashing) => {
                self.switch_branch(&name, crate::repo::CheckoutPlan::Stash);
            }
            _ => {}
        }
    }

    /// Switch to `name`, deciding what happens to uncommitted work through
    /// `plan`, and say what became of it.
    fn switch_branch(&mut self, name: &str, plan: crate::repo::CheckoutPlan) {
        let Some(path) = self.repo() else { return };
        match crate::repo::checkout_branch_planned(&path, name, plan) {
            Ok(stash) => {
                self.last_error = None;
                self.notice = Some(match stash {
                    Some(stash) => t_args(
                        "git-switched-stashed",
                        &[("name", name.into()), ("stash", stash.into())],
                    ),
                    None => t_args("git-switched", &[("name", name.into())]),
                });
                self.reload();
            }
            Err(error) => self.report_error(&error),
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
            Some(ModalHit::Action(index)) => {
                // Action 0 is Cancel; the rest are the sheet's own answers,
                // in the order it offered them.
                let choice = self.confirm.as_ref().and_then(|confirm| {
                    confirm
                        .choices()
                        .get(index.saturating_sub(1))
                        .map(|f| f.choice)
                });
                self.confirm = None;
                if let Some(choice) = choice {
                    self.perform_confirmed(choice);
                }
            }
            Some(ModalHit::Scrim) if layout.dismiss_on_scrim => self.confirm = None,
            _ => {}
        }
    }

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
        self.bar_fade.wake(ui::time::now());
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

    /// How far `scroller` is scrolled.
    fn scroll_offset(&self, scroller: Scroller) -> f64 {
        match scroller {
            Scroller::ChangesList => self.changes_scroll,
            Scroller::Diff => self.diff_scroll,
            Scroller::HistoryList => self.history_scroll,
            Scroller::Detail => self.detail_scroll,
            Scroller::Panels => self.panels_scroll,
        }
    }

    fn set_scroll_offset(&mut self, scroller: Scroller, offset: f64) {
        let slot = match scroller {
            Scroller::ChangesList => &mut self.changes_scroll,
            Scroller::Diff => &mut self.diff_scroll,
            Scroller::HistoryList => &mut self.history_scroll,
            Scroller::Detail => &mut self.detail_scroll,
            Scroller::Panels => &mut self.panels_scroll,
        };
        *slot = offset;
        self.bar_fade.wake(ui::time::now());
    }

    /// Whether the bars stay shown: a thumb is held, or the pointer is on a
    /// bar's track.
    fn bars_held(&self) -> bool {
        self.scrollbar.is_active()
            || self.pointer.is_some_and(|p| {
                self.scroll_bars
                    .iter()
                    .any(|(_, track, _)| track.inflate(4.0, 0.0).contains(p))
            })
    }

    /// Fade each region's content out at an edge more of it can scroll in
    /// from, so a list or a diff cut by its edge shows that it goes on.
    fn paint_edge_fades(&self, ctx: &mut PaintCtx<'_>) {
        use ui::surface::{ContentEdge, content_edge_fade};
        for region in &self.scroll_regions {
            let offset = self.scroll_offset(region.scroller);
            let mut edges = Vec::new();
            if offset > 0.5 {
                edges.push(ContentEdge::Top);
            }
            if region.max_y - offset > 0.5 {
                edges.push(ContentEdge::Bottom);
            }
            // The diff's line numbers stand still while its code scrolls
            // sideways, so only the trailing edge fades.
            if region.scroller == Scroller::Diff && region.max_x - self.diff_hscroll > 0.5 {
                edges.push(ContentEdge::Trailing);
            }
            for edge in edges {
                content_edge_fade(ctx.scene, ctx.transform, region.rect, edge, region.ground);
            }
        }
    }

    /// The edge fades, then the toolkit scroll thumb along the trailing edge
    /// of every region whose content overflows, shown while it scrolls or is
    /// pointed at and faded out at rest; records the tracks for the pointer.
    fn paint_scroll_bars(&mut self, ctx: &mut PaintCtx<'_>) {
        self.paint_edge_fades(ctx);
        // The toolkit scroll view's bar: a thumb `s2` thick, held half an
        // `s1` off the region's edges.
        let s = &ctx.theme.tokens.spacing;
        let thickness = f64::from(s.s2);
        let margin = f64::from(s.s1) / 2.0;
        self.scroll_bars = self
            .scroll_regions
            .iter()
            .filter_map(|region| {
                let r = region.rect;
                let metrics = ScrollMetrics::new(
                    r.height() + region.max_y.max(0.0),
                    r.height(),
                    self.scroll_offset(region.scroller),
                );
                metrics.overflows().then(|| {
                    (
                        region.scroller,
                        Rect::new(
                            r.x1 - margin - thickness,
                            r.y0 + margin,
                            r.x1 - margin,
                            r.y1 - margin,
                        ),
                        metrics,
                    )
                })
            })
            .collect();
        let opacity = self.bar_fade.opacity_at(ui::time::now(), self.bars_held());
        if opacity <= 0.003 {
            return;
        }
        for &(scroller, track, metrics) in &self.scroll_bars {
            let bar = if self.scrollbar_region == Some(scroller) {
                self.scrollbar
            } else {
                Scrollbar::vertical()
            };
            ctx.scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                opacity,
                ctx.transform,
                &track,
            );
            bar.draw(ctx.scene, ctx.transform, track, metrics, ctx.theme, false);
            ctx.scene.pop_layer();
        }
    }

    /// A press on a scroll bar: the thumb starts a drag, the track pages.
    /// Returns whether the press was the scroll bar's.
    fn scroll_bar_press(&mut self, pos: Point) -> bool {
        let Some(&(scroller, track, metrics)) = self
            .scroll_bars
            .iter()
            .rev()
            .find(|(_, track, _)| track.contains(pos))
        else {
            return false;
        };
        let mut bar = Scrollbar::vertical();
        match bar.press(track, metrics, pos) {
            PressOutcome::Thumb => {
                self.scrollbar = bar;
                self.scrollbar_region = Some(scroller);
                true
            }
            PressOutcome::Page(offset) => {
                self.set_scroll_offset(scroller, offset);
                true
            }
            PressOutcome::Miss => false,
        }
    }

    /// Pointer motion over the scroll bars: a held thumb follows it, and the
    /// thumb under it brightens.
    fn scroll_bar_move(&mut self, pos: Point) {
        if self.scrollbar.is_dragging() {
            let held = self
                .scroll_bars
                .iter()
                .find(|(scroller, _, _)| Some(*scroller) == self.scrollbar_region)
                .copied();
            if let Some((scroller, track, metrics)) = held
                && let Some(offset) = self.scrollbar.drag_to(track, metrics, pos)
            {
                self.set_scroll_offset(scroller, offset);
            }
            return;
        }
        let hovered = self
            .scroll_bars
            .iter()
            .rev()
            .find(|(_, track, metrics)| {
                Scrollbar::vertical()
                    .thumb_rect(*track, *metrics)
                    .is_some_and(|thumb| thumb.contains(pos))
            })
            .map(|(scroller, _, _)| *scroller);
        self.scrollbar.set_hover(hovered.is_some());
        if hovered.is_some() {
            // Leaving the bar lets it rest a moment before it fades.
            self.bar_fade.wake(ui::time::now());
        }
        self.scrollbar_region = hovered;
    }

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
                Hit::DismissError => {
                    let text = if let Some(job) = &self.remote_job {
                        job.label.clone()
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
                    button("git.dismiss-error".into(), rect, t("git-dismiss-error"))
                }
                Hit::Change(ChangesHit::FocusMessage) => field(
                    "git.commit-message",
                    rect,
                    &self.commit_input,
                    &t("git-commit-message"),
                ),
                Hit::Change(hit) => match self.change_node(hit, rect) {
                    Some(entry) => entry,
                    None => continue,
                },
                Hit::Diff(hit) => {
                    let (target, name) = match hit {
                        DiffHit::StageFile if self.selected_is_staged() => {
                            ("git.diff.unstage-file".to_string(), t("git-unstage-file"))
                        }
                        DiffHit::StageFile => {
                            ("git.diff.stage-file".to_string(), t("git-stage-file"))
                        }
                        DiffHit::DiscardFile => {
                            ("git.discard-file".to_string(), t("git-discard-file-name"))
                        }
                        DiffHit::Hunk(hunk, action) => {
                            let name = t(match action {
                                HunkAction::Toggle if self.selected_is_staged() => {
                                    "git-unstage-hunk"
                                }
                                HunkAction::Toggle => "git-stage-hunk",
                                HunkAction::Discard => "git-discard-hunk",
                            });
                            (format!("git.hunk:{hunk}:{action:?}"), name)
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
                            .with_name(t_args(
                                "git-commit-row-name",
                                &[
                                    ("summary", commit.summary.as_str().into()),
                                    ("author", commit.author_name.as_str().into()),
                                    ("age", history::age(commit.time).into()),
                                ],
                            ))
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .selected(*index == self.selected_commit),
                            )
                            .with_bounds(rect),
                    )
                }
                Hit::CommitFile(index) => {
                    let commit_id = self
                        .model
                        .commits
                        .get(self.selected_commit)
                        .map(|commit| commit.id.clone())
                        .unwrap_or_default();
                    let Some(file) = self
                        .commit_files
                        .get(&commit_id)
                        .and_then(|files| files.get(*index))
                    else {
                        continue;
                    };
                    (
                        format!("git.commit-file:{index}"),
                        rect,
                        AccessNode::new(Role::ListItem)
                            .with_name(t_args(
                                "git-commit-file-name",
                                &[
                                    ("file", file.path.as_str().into()),
                                    ("kind", file.kind.label().into()),
                                ],
                            ))
                            .with_description(t("git-show-commit-file"))
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .selected(self.file_selection == Some(*index)),
                            )
                            .with_bounds(rect),
                    )
                }
                Hit::BackToCommit => {
                    button("git.back-to-commit".into(), rect, t("git-back-to-commit"))
                }
                Hit::ConflictNote => {
                    let note = self.conflict_sentence();
                    out.push((
                        "git.conflict.note".into(),
                        rect,
                        AccessNode::new(Role::StaticText)
                            .with_name(&note)
                            .with_value(&note)
                            .with_bounds(rect),
                        None,
                    ));
                    continue;
                }
                Hit::ResolveConflict(side) => {
                    let (target, name) = match side {
                        crate::repo::ConflictSide::Ours => {
                            ("git.conflict.ours".to_string(), t("git-conflict-take-ours"))
                        }
                        crate::repo::ConflictSide::Theirs => (
                            "git.conflict.theirs".to_string(),
                            t("git-conflict-take-theirs"),
                        ),
                    };
                    button(target, rect, name)
                }
                Hit::Panel(PanelHit::WorktreeInfo(index)) => {
                    let Some(worktree) = self.model.worktrees.get(*index) else {
                        continue;
                    };
                    out.push((
                        format!("git.worktree:{}", worktree.path),
                        rect,
                        AccessNode::new(Role::ListItem)
                            .with_name(worktree.head.clone())
                            .with_description(worktree.path.clone())
                            .with_bounds(rect),
                        None,
                    ));
                    continue;
                }
                Hit::Panel(PanelHit::WorktreeError) => {
                    let Some(error) = &self.model.worktree_error else {
                        continue;
                    };
                    out.push((
                        "git.worktrees-error".into(),
                        rect,
                        AccessNode::new(Role::StaticText)
                            .with_name(t("git-worktree-read-failed"))
                            .with_description(error.clone())
                            .with_bounds(rect),
                        None,
                    ));
                    continue;
                }
                Hit::Panel(PanelHit::BranchName) => field(
                    "git.new-branch-name",
                    rect,
                    &self.branch_input,
                    &t("git-new-branch-name"),
                ),
                Hit::Panel(PanelHit::WorktreeBranch) => field(
                    "git.worktree-branch",
                    rect,
                    &self.worktree_branch_input,
                    &t("git-worktree-branch"),
                ),
                Hit::Panel(PanelHit::WorktreePath) => field(
                    "git.worktree-path",
                    rect,
                    &self.worktree_path_input,
                    &t("git-worktree-path"),
                ),
                Hit::Panel(PanelHit::WorktreeStart) => field(
                    "git.worktree-start",
                    rect,
                    &self.worktree_start_input,
                    &t("git-worktree-start"),
                ),
                Hit::Panel(PanelHit::CreateWorktree) => {
                    let (target, rect, node) =
                        button("git.create-worktree".into(), rect, t("git-create-worktree"));
                    let disabled = [
                        self.worktree_branch_input.value(),
                        self.worktree_path_input.value(),
                        self.worktree_start_input.value(),
                    ]
                    .iter()
                    .any(|text| text.trim().is_empty());
                    (
                        target,
                        rect,
                        node.with_state(AccessState::new().focusable(true).disabled(disabled)),
                    )
                }
                Hit::Panel(PanelHit::CreateBranch) => {
                    let (target, rect, node) =
                        button("git.create-branch".into(), rect, t("git-create-branch"));
                    let disabled = self.branch_input.value().trim().is_empty();
                    (
                        target,
                        rect,
                        node.with_state(AccessState::new().focusable(true).disabled(disabled)),
                    )
                }
                Hit::Panel(PanelHit::Branch(action)) => {
                    let (target, name) = match action {
                        BranchAction::Checkout(name) => (
                            format!("git.branch.checkout:{name}"),
                            t_args("git-check-out-name", &[("name", name.into())]),
                        ),
                        BranchAction::Delete(name) => (
                            format!("git.branch.delete:{name}"),
                            t_args("git-delete-name", &[("name", name.into())]),
                        ),
                        BranchAction::StashApply(index) => (
                            format!("git.stash.apply:{index}"),
                            t_args("git-apply-stash-name", &[("index", (*index).into())]),
                        ),
                    };
                    button(target, rect, name)
                }
                Hit::OpenPath => field(
                    "git.open-path",
                    rect,
                    &self.open_input,
                    &t("git-repository-path"),
                ),
                Hit::ChooseRepository => {
                    button("git.choose-repository".into(), rect, t("git-choose-name"))
                }
                Hit::OpenRepository => {
                    let (target, rect, node) =
                        button("git.open-repository".into(), rect, t("git-open-name"));
                    let disabled = self.open_input.value().trim().is_empty();
                    (
                        target,
                        rect,
                        node.with_state(AccessState::new().focusable(true).disabled(disabled)),
                    )
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
                        t_args(
                            "git-change-open",
                            &[
                                ("path", change.path.as_str().into()),
                                ("kind", change.kind.label().into()),
                            ],
                        ),
                        self.selected_change == Some((*area, *row)),
                    ),
                )
            }
            ChangesHit::ToggleStage(area, row) => {
                let (verb, key) = if *area == ChangeArea::Staged {
                    ("Unstage", "git-unstage-path")
                } else {
                    ("Stage", "git-stage-path")
                };
                let change = change_list(&self.model, *area).get(*row)?;
                (
                    format!("git.change.{verb}:{area:?}:{}", change.path),
                    rect,
                    node(
                        Role::Button,
                        t_args(key, &[("path", change.path.as_str().into())]),
                        false,
                    ),
                )
            }
            ChangesHit::StageAll(area) => (
                format!("git.changes.stage-all:{area:?}"),
                rect,
                node(Role::Button, t("git-stage-all-name"), false),
            ),
            ChangesHit::UnstageAll(area) => (
                format!("git.changes.unstage-all:{area:?}"),
                rect,
                node(Role::Button, t("git-unstage-all-name"), false),
            ),
            ChangesHit::Commit => (
                "git.changes.commit".into(),
                rect,
                AccessNode::new(Role::Button)
                    .with_name(changes::commit_label(&self.model))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .disabled(!self.can_commit()),
                    )
                    .with_bounds(rect),
            ),
            ChangesHit::FocusMessage => return None,
        })
    }
}

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
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        self.adopt_preferences();
        self.drain_remote();
        self.publish_shared_repo_if_needed();
    }

    fn settle(&mut self, _edits: &mut ui::widget::Edits<'_>, _path: &ObjId) {
        self.run_window_toolbar();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The app draws itself; a single full-size leaf node carries it.
        let node = taffy.fill_leaf(node_map, self);
        // The list column's breakpoint and widths grow with the text scale.
        self.drawer.scale_for(theme);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // Real fallback rather than a panic if `node` somehow doesn't
        // belong to this tree, matching every `ui` widget's `apply_layout`.
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = ui::widget::placed(&layout, origin);
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
        // A key (1, 2, 3, d) changes what the bar shows without a sync.
        ui::window_toolbar::declare(Some(self.window_toolbar()));
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
        let Some(confirm) = &self.confirm else {
            self.confirm_layout = None;
            return;
        };
        let window = presence.viewport.get().unwrap_or(self.rect);
        let title = confirm.title();
        let body = confirm.body();
        let choices = confirm.choices();
        let cancel = t("git-cancel");
        let hovered = |index: usize| {
            self.confirm_layout
                .as_ref()
                .and_then(|layout| layout.actions.get(index).copied())
                .zip(self.pointer)
                .is_some_and(|(rect, p)| rect.contains(p))
        };
        let mut actions = vec![ModalAction::secondary(&cancel).hovered(hovered(0))];
        actions.extend(choices.iter().enumerate().map(|(index, face)| {
            let action = if face.danger {
                ModalAction::danger(&face.label)
            } else {
                ModalAction::primary(&face.label)
            };
            action.hovered(hovered(index.saturating_add(1)))
        }));
        let style = theme.text(TextRole::Body);
        let spec = ModalSpec::new(&title).actions(&actions);
        let width = modal_dialog::body_width(window, spec.max_width, theme);
        let (_, body_h) = text.measure_style(&body, Some(ui::num::f32_of_f64(width)), &style);
        let spec = spec.body_height(f64::from(body_h)).dismiss_on_scrim(true);
        let layout = modal_dialog::layout(window, &spec, text, theme);
        modal_dialog::paint_backdrop(scene, transform, text, theme, &layout, &spec);
        let (_, dim) = modal_dialog::ink(theme);
        text.draw_style(
            scene,
            &body,
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
            state: ctx.state.reborrow(),
            path: ctx.path,
            taffy: ctx.taffy,
        };
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pos = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.pointer = Some(pos);
                self.scroll_bar_move(pos);
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
                self.scrollbar.release();
                self.forward_to_inputs(&mut input);
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
                    &mut ctx.state,
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
        let children: Vec<_> = self
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
                .with_description(if let Some(job) = &self.remote_job {
                    job.label.clone()
                } else if self.awaiting_repo {
                    t("git-open-title")
                } else {
                    t("git-app-description")
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
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        use ui::a11y::Action;
        let (state, path) = ui::doc::document(state, path);

        let field = match target {
            "git.open-path" => Some(Field::Open),
            "git.commit-message" => Some(Field::Commit),
            "git.new-branch-name" => Some(Field::Branch),
            "git.worktree-branch" => Some(Field::WorktreeBranch),
            "git.worktree-path" => Some(Field::WorktreePath),
            "git.worktree-start" => Some(Field::WorktreeStart),
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
                Field::WorktreeBranch => &mut self.worktree_branch_input,
                Field::WorktreePath => &mut self.worktree_path_input,
                Field::WorktreeStart => &mut self.worktree_start_input,
            };
            return input.perform_action(action, &[], state, path);
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

    fn needs_animation_frame(&self) -> bool {
        // Frames only while the list column slides or the bars fade. A
        // transfer wakes the app when its worker replies, and the toolkit's
        // caret is steady: neither needs frames.
        (self.screen.has_list() && self.drawer.is_animating())
            || self.bar_fade.fading_at(ui::time::now(), self.bars_held())
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        // The bars start to fade once a scroll has rested a moment.
        self.bar_fade
            .fade_deadline_at(ui::time::now(), self.bars_held())
    }
}

impl GitClientApp {
    /// Forward one event to a single embedded text field, returning whether
    /// it consumed the event.
    fn fwd(
        widget: &mut dyn Widget,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut ui::widget::Edits<'_>,
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
                state: state.reborrow(),
                path,
                handled: &mut handled,
                focus_target: &mut ft,
                node_id: NodeId::new(0),
                taffy,
            },
        );
        handled
    }

    /// Forward a pointer event to every text field, so an in-flight
    /// drag-select keeps updating regardless of which field started it.
    fn forward_to_inputs(&mut self, ctx: &mut InputEventCtx<'_>) {
        let widgets: [&mut dyn Widget; 6] = [
            &mut self.commit_input,
            &mut self.branch_input,
            &mut self.worktree_branch_input,
            &mut self.worktree_path_input,
            &mut self.worktree_start_input,
            &mut self.open_input,
        ];
        for widget in widgets {
            Self::fwd(
                widget,
                ctx.event,
                ctx.scale_factor,
                ctx.modifiers,
                &mut ctx.state,
                ctx.path,
                ctx.taffy,
            );
        }
    }

    /// Route a left press: the confirmation sheet when it is up, a text
    /// field (focus and caret), then the target under the pointer. A press
    /// anywhere else blurs the focused field.
    fn handle_press(&mut self, ctx: &mut InputEventCtx<'_>) {
        let Some(pos) = self.pointer else {
            return;
        };
        if self.confirm.is_some() {
            self.confirm_press(pos);
            return;
        }
        if self.scroll_bar_press(pos) {
            self.set_focus(None);
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
            Some(Hit::Panel(PanelHit::WorktreeBranch)) => Some(Field::WorktreeBranch),
            Some(Hit::Panel(PanelHit::WorktreePath)) => Some(Field::WorktreePath),
            Some(Hit::Panel(PanelHit::WorktreeStart)) => Some(Field::WorktreeStart),
            Some(Hit::OpenPath) => Some(Field::Open),
            _ => None,
        };
        if let Some(field) = field {
            self.set_focus(Some(field));
            let input = match field {
                Field::Commit => &mut self.commit_input,
                Field::Branch => &mut self.branch_input,
                Field::WorktreeBranch => &mut self.worktree_branch_input,
                Field::WorktreePath => &mut self.worktree_path_input,
                Field::WorktreeStart => &mut self.worktree_start_input,
                Field::Open => &mut self.open_input,
            };
            Self::fwd(
                input,
                ctx.event,
                ctx.scale_factor,
                ctx.modifiers,
                &mut ctx.state,
                ctx.path,
                ctx.taffy,
            );
            return;
        }
        self.set_focus(None);
        if let Some(hit) = hit {
            self.activate(hit, &mut ctx.state);
        }
    }

    /// Submit the focused field's action (Enter).
    fn submit_focused(&mut self, field: Field) {
        match field {
            Field::Commit => self.do_commit(),
            Field::Branch => self.create_new_branch(),
            Field::Open => self.open_repo_from_input(),
            Field::WorktreeBranch | Field::WorktreePath | Field::WorktreeStart => {
                if let Some(dispatcher) = &self.dispatcher {
                    dispatcher.effect(&GitClientEffect::CreateWorktree {
                        path: self.worktree_path_input.value().to_owned(),
                        branch: self.worktree_branch_input.value().to_owned(),
                        start: self.worktree_start_input.value().to_owned(),
                    });
                }
            }
        }
    }

    fn handle_key(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        state: &mut ui::widget::Edits<'_>,
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
            if matches!(&ev.logical_key, Key::Named(NamedKey::Escape)) {
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
                        Field::WorktreeBranch => &mut self.worktree_branch_input,
                        Field::WorktreePath => &mut self.worktree_path_input,
                        Field::WorktreeStart => &mut self.worktree_start_input,
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
        if matches!(&ev.logical_key, Key::Named(NamedKey::Escape)) && self.drawer_is_modal() {
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
                self.toggle_diff_layout();
                *handled = true;
                return;
            }
            if self.handle_action_key(s.as_str(), modifiers, &ev.logical_key) {
                *handled = true;
                return;
            }
        }

        if self.navigate_list(&ev.logical_key) {
            *handled = true;
            return;
        }
        if self.navigate_panels(&ev.logical_key) {
            *handled = true;
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

impl GitClientApp {
    pub(crate) fn attach_control_actions(
        &mut self,
        actions: ui::framework::Actions<crate::resource::GitClient>,
    ) {
        self.control_actions = Some(actions);
    }

    fn effect_for_hit(&self, hit: &Hit) -> Option<(Option<String>, GitClientEffect)> {
        let effect = match hit {
            Hit::ChooseRepository => GitClientEffect::ChooseRepository,
            Hit::Panel(PanelHit::CreateWorktree) => GitClientEffect::CreateWorktree {
                path: self.worktree_path_input.value().to_owned(),
                branch: self.worktree_branch_input.value().to_owned(),
                start: self.worktree_start_input.value().to_owned(),
            },
            Hit::Diff(DiffHit::StageFile | DiffHit::DiscardFile) => {
                let (area, _) = self.selected_change?;
                let path = self.current_path()?;
                let row = crate::actions::change_resource_id(area, &path);
                let effect = match hit {
                    Hit::Diff(DiffHit::DiscardFile) => GitClientEffect::DiscardFile { path },
                    _ if area == ChangeArea::Staged => GitClientEffect::UnstageFile { path },
                    _ => GitClientEffect::StageFile { path },
                };
                return Some((Some(row), effect));
            }
            Hit::Change(ChangesHit::StageAll(_)) => GitClientEffect::StageAll,
            Hit::Change(ChangesHit::UnstageAll(_)) => GitClientEffect::UnstageAll,
            Hit::Change(ChangesHit::Commit) => GitClientEffect::Commit {
                message: self.commit_input.value().to_owned(),
            },
            _ => return None,
        };
        Some((None, effect))
    }

    fn binding_for_hit(&self, hit: &Hit) -> Option<ui::tilo_actions::ActionBinding<()>> {
        use ui::tilo_resource::Effect as _;
        let (row, effect) = self.effect_for_hit(hit)?;
        if let Some(row) = row {
            let published = self.working_tree_row(&row)?.into_resource();
            ui::tilo_actions::ActionBinding::unit_for(&published, effect.id()).ok()
        } else {
            self.control_actions.as_ref()?.effect(&effect)
        }
    }
}

impl ui::widget::controls::ActionControls for GitClientApp {
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        self.semantic_entries()
            .into_iter()
            .map(
                |(id, _, _, hit)| match hit.as_ref().and_then(|hit| self.binding_for_hit(hit)) {
                    Some(binding) => ui::widget::ControlBindingMetadata {
                        control_id: id,
                        kind: ui::widget::ControlBindingKind::Action(binding.address().clone()),
                    },
                    None => ui::widget::ControlBindingMetadata::semantic(id),
                },
            )
            .collect()
    }
}

impl GitClientApp {
    fn submit(&self, invoke: Result<Invoke<crate::resource::GitClient>, serde_json::Error>) {
        // A path and the repository's projection always encode.
        if let (Some(dispatcher), Ok(invoke)) = (&self.dispatcher, invoke) {
            dispatcher.submit(invoke);
        }
    }
}

impl GitClientApp {
    fn switch_diff_layout(&mut self) -> Result<(), String> {
        self.diff_scroll = 0.0;
        self.change_preferences(Settings::toggle_layout)
    }
}

impl GitClientApp {
    fn change_preferences(&mut self, change: impl FnOnce(&mut Settings)) -> Result<(), String> {
        change(&mut self.settings);
        if let Some(preferences) = &self.preferences {
            preferences
                .set(self.settings)
                .map_err(|error| format!("Couldn't save the diff preferences: {error}"))?;
        }
        Ok(())
    }
}

impl GitClientApp {
    pub(crate) fn follow_preferences(&mut self, preferences: ui::framework::Setting<Settings>) {
        self.settings = preferences.get();
        self.preferences = Some(preferences);
    }
}

impl GitClientApp {
    fn adopt_preferences(&mut self) {
        if let Some(preferences) = &self.preferences {
            self.settings = preferences.state().with(|stored| *stored);
        }
    }
}

impl GitClientApp {
    pub(crate) fn preference_statuses(&self) -> Vec<ui::framework::EffectStatus> {
        use ui::framework::EffectStatus;
        let s = self.settings;
        vec![
            EffectStatus::labelled(
                GitClientEffect::DIFF_LAYOUT,
                match s.diff_layout {
                    DiffLayout::Unified => "Diff layout: side-by-side",
                    DiffLayout::SideBySide => "Diff layout: unified",
                },
            ),
            EffectStatus::labelled(
                GitClientEffect::SYNTAX_HIGHLIGHT,
                if s.syntax_highlight {
                    "Turn off syntax highlighting"
                } else {
                    "Turn on syntax highlighting"
                },
            ),
            EffectStatus::labelled(
                GitClientEffect::SHOW_WHITESPACE,
                if s.show_whitespace {
                    "Hide whitespace"
                } else {
                    "Show whitespace"
                },
            ),
            EffectStatus::labelled(
                GitClientEffect::WRAP_LINES,
                if s.wrap_lines {
                    "Stop wrapping long lines"
                } else {
                    "Wrap long lines"
                },
            ),
        ]
    }
}

impl GitClientApp {
    pub(crate) fn attach_dispatcher(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::GitClient>,
        mode: RunMode,
        sample: Option<&crate::resource::SampleRepository>,
    ) {
        self.dispatcher = Some(dispatcher);
        self.mode = mode;
        if let Some(sample) = sample {
            self.show_model(sample.model.clone(), sample.diffs.clone());
        }
    }
}

impl GitClientApp {
    pub(crate) fn present_state(&mut self, state: &crate::resource::GitClient) {
        match self.mode {
            RunMode::Live => {
                let first = !std::mem::replace(&mut self.presented, true);
                let repository = state.opened().map(str::to_owned);
                if !first && self.opened == repository {
                    return;
                }
                self.opened.clone_from(&repository);
                if let Some(path) = &repository
                    && !self.bind_repository(std::path::Path::new(path))
                {
                    self.last_error = Some(format!("No git repository at {path}"));
                    self.open_input.set_text(path);
                }
            }
            RunMode::Past => self.show_model(state.shared.clone().into_model(), DiffMap::new()),
            RunMode::Capture => {}
        }
    }
}

impl GitClientApp {
    pub(crate) fn working_tree_rows(&self) -> Vec<LiveRow> {
        let mut rows = crate::actions::repository_rows(&self.model);
        if !self.awaiting_repo {
            rows.push(crate::actions::working_tree_rows(&self.model));
            rows.push(crate::actions::worktree_rows(&self.model));
        }
        if let Some(job) = &self.remote_job {
            rows.push(LiveRow::new(
                "git_client.remote-job",
                "git_client.job",
                job.label.clone(),
            ));
        }
        rows
    }
}

impl GitClientApp {
    pub(crate) fn working_tree_row(&self, id: &str) -> Option<LiveRow> {
        crate::actions::repository_row(&self.model, id).or_else(|| {
            (!self.awaiting_repo)
                .then(|| {
                    crate::actions::working_tree_row(&self.model, id)
                        .or_else(|| crate::actions::worktree_row(&self.model, id))
                })
                .flatten()
        })
    }
}

impl GitClientApp {
    /// Resolve a navigation target against the current repository projection.
    fn open_effect_view(&mut self, effect: GitClientEffect) -> Result<(), String> {
        match effect {
            GitClientEffect::ShowChanges => self.switch_screen(Screen::Changes),
            GitClientEffect::ShowHistory => self.switch_screen(Screen::History),
            GitClientEffect::ShowBranches => self.switch_screen(Screen::Panels),
            GitClientEffect::OpenChange { area, path } => {
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
            GitClientEffect::OpenCommit { id } => {
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
            _ => return Ok(()),
        }
        Ok(())
    }

    /// Local diff preferences touch neither the repository nor the document.
    fn apply_diff_preference(&mut self, effect: &GitClientEffect) -> Option<Result<(), String>> {
        match effect {
            GitClientEffect::DiffLayout => Some(self.switch_diff_layout()),
            GitClientEffect::SyntaxHighlight => {
                Some(self.change_preferences(|s| s.syntax_highlight = !s.syntax_highlight))
            }
            GitClientEffect::ShowWhitespace => {
                Some(self.change_preferences(|s| s.show_whitespace = !s.show_whitespace))
            }
            GitClientEffect::WrapLines => {
                Some(self.change_preferences(|s| s.wrap_lines = !s.wrap_lines))
            }
            _ => None,
        }
    }

    fn save_stash(&mut self, message: &str) -> Result<(), String> {
        let path = self
            .repo()
            .ok_or_else(|| "stashing requires a real repository".to_string())?;
        crate::repo::stash_save(&path, message).map_err(|error| error.to_string())?;
        self.last_error = None;
        self.reload();
        Ok(())
    }

    pub(crate) fn answer_effect(
        &mut self,
        effect: GitClientEffect,
        cx: &mut EffectCx<crate::resource::GitClient, Self>,
    ) -> EffectAnswer {
        let preference = self.apply_diff_preference(&effect);
        if let Some(kept) = preference {
            return kept.map(|()| Effected::done());
        }
        let view = matches!(
            effect,
            GitClientEffect::ShowChanges
                | GitClientEffect::ShowHistory
                | GitClientEffect::ShowBranches
                | GitClientEffect::OpenChange { .. }
                | GitClientEffect::OpenCommit { .. }
        );
        if !view && self.repository_busy() {
            return Err(ui::i18n!("git-remote-busy"));
        }
        match effect {
            GitClientEffect::OpenRepository { path } => self.open_repository_at(&path)?,
            GitClientEffect::ChooseRepository => return Ok(Self::choose_repository(cx)),
            GitClientEffect::RunApprovedCheck { check_id } => {
                let checked = cx.approved_check_reported(&check_id)?;
                return Ok(cx.spawn(checked, |_app, observed, _cx| observed.map(Effected::value)));
            }
            effect @ (GitClientEffect::ShowChanges
            | GitClientEffect::ShowHistory
            | GitClientEffect::ShowBranches
            | GitClientEffect::OpenChange { .. }
            | GitClientEffect::OpenCommit { .. }) => self.open_effect_view(effect)?,
            GitClientEffect::StageAll => self.stage_all(),
            GitClientEffect::UnstageAll => self.unstage_all(),
            GitClientEffect::StageFile { path } => {
                let (area, index) = self
                    .worktree_change(&path)
                    .ok_or_else(|| "file is no longer unstaged".to_string())?;
                self.toggle_stage(area, index);
            }
            GitClientEffect::UnstageFile { path } => {
                let index = self
                    .model
                    .staged
                    .iter()
                    .position(|change| change.path == path)
                    .ok_or_else(|| "file is no longer staged".to_string())?;
                self.toggle_stage(ChangeArea::Staged, index);
            }
            GitClientEffect::DiscardFile { path } => {
                let (area, index) = self
                    .worktree_change(&path)
                    .ok_or_else(|| "file has no worktree changes".to_string())?;
                if self.repo_path.is_none() {
                    return Err("discard is unavailable in the demonstration repository".into());
                }
                self.discard_file(area, index);
            }
            GitClientEffect::Commit { message } => self.commit_from_effect(&message)?,
            GitClientEffect::CreateBranch { name } => {
                if name.trim().is_empty() {
                    return Err("branch name cannot be empty".into());
                }
                if self.repo_path.is_none() {
                    return Err("branch creation requires a real repository".into());
                }
                self.branch_input.set_text(&name);
                self.create_new_branch();
            }
            GitClientEffect::CreateWorktree {
                path,
                branch,
                start,
            } => {
                return self.start_effect_worktree(cx, path, branch, start);
            }
            GitClientEffect::CheckoutBranch { name } => {
                self.apply_branch_action(BranchAction::Checkout(name));
            }
            GitClientEffect::DeleteBranch { name } => {
                self.apply_branch_action(BranchAction::Delete(name));
            }
            GitClientEffect::ApplyStash { index } => {
                self.apply_branch_action(BranchAction::StashApply(index));
            }
            GitClientEffect::SaveStash { message } => self.save_stash(&message)?,
            GitClientEffect::Fetch { remote } => {
                return self.start_effect_remote(RemoteAction::Fetch, remote, cx);
            }
            GitClientEffect::Pull { remote } => {
                return self.start_effect_remote(RemoteAction::Pull, remote, cx);
            }
            GitClientEffect::Push { remote } => {
                return self.start_effect_remote(RemoteAction::Push, remote, cx);
            }
            GitClientEffect::DiffLayout
            | GitClientEffect::SyntaxHighlight
            | GitClientEffect::ShowWhitespace
            | GitClientEffect::WrapLines => {}
        }
        match &self.last_error {
            Some(error) if !view => Err(error.clone()),
            _ => Ok(Effected::done()),
        }
    }
}

impl GitClientApp {
    /// Ask the system's file chooser for the repository's folder, then
    /// open it; cancelling leaves the window as it was.
    fn choose_repository(cx: &mut EffectCx<crate::resource::GitClient, Self>) -> Effected {
        let picker =
            ui::portal::PickRequest::writable_directory(&t("git-choose-title"), &t("git-open"))
                .thread_name("git-repository-picker")
                .send();
        cx.thread(
            move |_| {
                picker.recv().unwrap_or_else(|_| {
                    ui::portal::PortalOutcome::Failed(
                        "The folder chooser stopped responding".into(),
                    )
                })
            },
            |app: &mut Self, outcome, _cx| match outcome {
                ui::portal::PortalOutcome::Opened(path) => app
                    .open_repository_at(&path.to_string_lossy())
                    .map(|()| Effected::done()),
                ui::portal::PortalOutcome::Cancelled => Ok(Effected::done()),
                ui::portal::PortalOutcome::Failed(message) => {
                    app.last_error = Some(message.clone());
                    Err(message)
                }
            },
        )
    }

    fn commit_from_effect(&mut self, message: &str) -> Result<(), String> {
        if self.model.staged.is_empty() {
            return Err("there are no staged changes to commit".into());
        }
        if message.trim().is_empty() {
            return Err("commit message cannot be empty".into());
        }
        self.commit_input.set_text(message);
        self.do_commit();
        Ok(())
    }

    fn start_effect_worktree(
        &mut self,
        cx: &mut EffectCx<crate::resource::GitClient, Self>,
        path: String,
        branch: String,
        start: String,
    ) -> EffectAnswer {
        let repository = self.repo().ok_or_else(|| t("git-worktree-host-required"))?;
        let reply = cx.reply();
        let label = t("git-worktree-running");
        reply.progress().report(label.clone(), None, None);
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("git-worktree".into())
            .spawn(move || {
                let result = crate::repo::create_worktree(
                    &repository,
                    std::path::Path::new(&path),
                    &branch,
                    &start,
                )
                .map_err(|error| format!("{error:#}"));
                let _ = sender.send(result);
            })
            .map_err(|error| error.to_string())?;
        self.last_error = None;
        self.remote_job = Some(RemoteJob {
            receiver,
            reply: Some(reply),
            label,
        });
        Ok(Effected::Pending)
    }

    fn start_effect_remote(
        &mut self,
        action: RemoteAction,
        remote: String,
        cx: &mut EffectCx<crate::resource::GitClient, Self>,
    ) -> EffectAnswer {
        let reply = cx.reply();
        reply
            .progress()
            .report(ui::i18n!("git-remote-running"), None, None);
        self.start_remote(action, Some(remote), Some(reply))?;
        Ok(Effected::Pending)
    }
}

impl GitClientApp {
    fn worktree_change(&self, path: &str) -> Option<(ChangeArea, usize)> {
        self.model
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
    }
}

impl GitClientApp {
    fn bind_repository(&mut self, path: &std::path::Path) -> bool {
        let Some((model, diffs)) = crate::repo::load(path) else {
            return false;
        };
        // Anchor actions on the working directory so later opens are stable
        // even if the CWD changes; fall back to the given path when bare.
        let anchor = crate::repo::workdir(path).unwrap_or_else(|| path.to_path_buf());
        self.show_model(model, diffs);
        self.repo_path = Some(anchor);
        true
    }
}

impl GitClientApp {
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        match scenario {
            // The side-by-side diff layout.
            Some("split") => {
                if let Err(error) =
                    self.change_preferences(|s| s.diff_layout = DiffLayout::SideBySide)
                {
                    self.last_error = Some(error);
                }
            }
            Some("history") => self.switch_screen(Screen::History),
            Some("branches") => self.switch_screen(Screen::Panels),
            // A typed commit summary, so the commit box and its primary
            // action render enabled.
            Some("commit") => self
                .commit_input
                .set_text("Flush the blur pass before presenting"),
            // A changed file in the worktree, whose "Discard file" the
            // scenario's input then presses.
            Some("discard") => {
                if !self.model.unstaged.is_empty() {
                    self.selected_change = Some((ChangeArea::Unstaged, 0));
                } else if !self.model.untracked.is_empty() {
                    self.selected_change = Some((ChangeArea::Untracked, 0));
                }
                self.diff_scroll = 0.0;
            }
            // The file a merge left conflicted, open, so the capture shows
            // the choice it waits on and what each side costs.
            Some("conflict") => self.test_select_conflicted_file(),
            // The narrow window's list column, revealed over its scrim
            // rather than the closed default a fresh narrow window opens to.
            Some("narrow") => self.drawer.snap(true),
            _ => {}
        }
    }
}

impl GitClientApp {
    fn show_model(&mut self, model: RepoModel, diffs: DiffMap) {
        self.model = model;
        self.diffs = diffs;
        self.graph_layout = graph::layout(&mut self.model.commits);
        self.model.graph_width = self.graph_layout.lanes;
        self.selected_change = first_change(&self.model);
        if self.selected_commit >= self.model.commits.len() {
            self.selected_commit = 0;
        }
        self.diff_scroll = 0.0;
        self.awaiting_repo = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::Widget;

    #[test]
    fn live_initial_view_stays_empty_until_the_repository_is_explicitly_bound() {
        let mut app = GitClientApp::new();
        let mut state = crate::resource::GitClient::default();
        app.present_state(&state);
        assert!(app.awaiting_repo);
        assert!(app.repo_path.is_none());
        assert!(app.model.branches.is_empty());
        assert!(app.model.commits.is_empty());
        let repository = tempfile::tempdir().unwrap();
        git2::Repository::init(repository.path()).unwrap();
        state.repository =
            ui::tilo_resource::Atom(repository.path().to_string_lossy().into_owned());
        app.present_state(&state);
        assert!(!app.awaiting_repo);
        assert_eq!(app.repo_path.as_deref(), Some(repository.path()));
    }

    /// Build an app over the bundled sample, laid out at `w` x 800 with the
    /// flap settled to its mode default so geometry is deterministic.
    fn app_at(w: f64, screen: Screen) -> GitClientApp {
        let mut app = GitClientApp::from_model(crate::sample::model().0, crate::sample::model().1);
        app.screen = screen;
        app.rect = Rect::new(0.0, 0.0, w, 800.0);
        app.drawer.resolve(app.rect);
        app.drawer.snap(app.drawer.is_open());
        draw_frame(&mut app);
        app
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
            app.activate(hit, &mut ui::widget::Edits::lend(doc));
        }
        draw_frame(app);
    }

    /// Adaptive screen mode: wide Changes docks, narrow master-detail overlays closed,
    /// the breakpoint boundary is inclusive, Panels never collapses, and crossing the
    /// breakpoint restores the mode default. Each phase is a former test with its own fresh fixture.
    #[test]
    fn screen_mode_follows_the_breakpoint_and_preset() {
        wide_changes_screen_is_docked();
        narrow_master_detail_screen_is_overlay_closed();
        breakpoint_boundary_is_inclusive_docked();
        panels_screen_never_collapses();
        crossing_breakpoint_restores_mode_default();
    }

    fn wide_changes_screen_is_docked() {
        let app = app_at(1280.0, Screen::Changes);
        assert!(app.drawer.is_docked());
        assert!(app.drawer.is_open());
    }

    fn narrow_master_detail_screen_is_overlay_closed() {
        let app = app_at(680.0, Screen::Changes);
        assert!(app.drawer.is_overlay());
        assert!(!app.drawer.is_open());
    }

    fn breakpoint_boundary_is_inclusive_docked() {
        let boundary = app_at(1280.0, Screen::Changes).drawer.resolved_breakpoint();
        assert!(app_at(boundary, Screen::Changes).drawer.is_docked());
        assert!(app_at(boundary - 1.0, Screen::Changes).drawer.is_overlay());
    }

    fn panels_screen_never_collapses() {
        let app = app_at(400.0, Screen::Panels);
        let toolbar = app.window_toolbar();
        assert!(
            toolbar.sidebar_unavailable,
            "no list, so the toggle cannot act"
        );
        assert!(!app.drawer_is_modal());
    }

    /// The overlay flap: the bar's sidebar toggle opens and closes it, a scrim
    /// click dismisses it, a click inside keeps it open, and while open it
    /// hides and blocks background controls. Each phase is a former test with
    /// its own fresh fixture.
    #[test]
    fn overlay_flap_toggles_dismisses_and_blocks_the_background() {
        bar_toggle_opens_and_closes_overlay_flap();
        scrim_click_dismisses_open_overlay_flap();
        click_inside_open_flap_does_not_dismiss_it();
        open_overlay_hides_and_blocks_background_controls();
    }

    fn bar_toggle_opens_and_closes_overlay_flap() {
        let mut app = app_at(680.0, Screen::Changes);
        assert!(!app.drawer.is_open());
        assert_eq!(app.window_toolbar().sidebar, Some(false));
        app.run_window_bar_event(&ui::window_toolbar::ToolbarEvent::ToggleSidebar);
        draw_frame(&mut app);
        assert!(app.drawer.is_open());
        assert_eq!(app.window_toolbar().sidebar, Some(true));
        app.run_window_bar_event(&ui::window_toolbar::ToolbarEvent::ToggleSidebar);
        draw_frame(&mut app);
        assert!(!app.drawer.is_open());
    }

    fn scrim_click_dismisses_open_overlay_flap() {
        let mut app = app_at(680.0, Screen::Changes);
        app.drawer.snap(true);
        draw_frame(&mut app);
        assert!(app.drawer.is_open());
        press(&mut app, Point::new(670.0, 400.0), &mut AutoCommit::new());
        assert!(!app.drawer.is_open());
    }

    fn click_inside_open_flap_does_not_dismiss_it() {
        let mut app = app_at(680.0, Screen::Changes);
        app.drawer.snap(true);
        draw_frame(&mut app);
        let panel = app.drawer.sidebar_rect();
        press(
            &mut app,
            Point::new(panel.x0 + 4.0, panel.y1 - 4.0),
            &mut AutoCommit::new(),
        );
        assert!(app.drawer.is_open());
    }

    fn open_overlay_hides_and_blocks_background_controls() {
        let mut app = app_at(680.0, Screen::Changes);
        app.drawer.snap(true);
        draw_frame(&mut app);
        assert!(app.drawer_is_modal());
        assert!(
            app.semantic
                .iter()
                .all(|t| matches!(t.hit, Hit::Change(_) | Hit::Commit(_))),
            "covered content controls leave the accessible tree"
        );
        let covered = app.drawer.sidebar_rect();
        press(
            &mut app,
            Point::new(covered.x0 + 4.0, covered.y1 - 4.0),
            &mut AutoCommit::new(),
        );
        assert_eq!(app.screen, Screen::Changes);
        assert!(app.drawer.is_open());
    }

    fn crossing_breakpoint_restores_mode_default() {
        let mut app = app_at(1280.0, Screen::Changes);
        app.drawer.close();
        assert!(!app.drawer.is_open());
        app.rect = Rect::new(0.0, 0.0, 680.0, 800.0);
        app.drawer.resolve(app.rect);
        assert!(app.drawer.is_overlay());
        assert!(!app.drawer.is_open());
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.drawer.resolve(app.rect);
        assert!(app.drawer.is_docked());
        assert!(
            !app.drawer.is_open(),
            "resizing preserves the person's docked choice"
        );
    }

    /// Semantic parity: semantic open-repository and text edits reach their
    /// inputs, and synthetic accessibility controls sit at their drawn rects.
    /// Each phase is a former test with its own fresh fixture.
    #[test]
    fn affordances_and_semantic_controls_reach_the_production_actions() {
        semantic_open_repository_binds_the_entered_repository();
        semantic_text_edits_reach_embedded_repository_commit_and_branch_inputs();
        synthetic_accessibility_controls_are_addressable_at_their_drawn_rects();
    }

    fn semantic_open_repository_binds_the_entered_repository() {
        let directory = tempfile::tempdir().unwrap();
        let _repository = git2::Repository::init(directory.path()).unwrap();
        let mut app = GitClientApp::open_empty();
        app.open_input.set_text(directory.path().to_str().unwrap());
        let mut doc = automerge::AutoCommit::new();
        assert!(Widget::perform_semantic_action(
            &mut app,
            "git.open-repository",
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(!app.test_awaiting_repo());
        assert_eq!(app.test_last_error(), None);
    }

    fn semantic_text_edits_reach_embedded_repository_commit_and_branch_inputs() {
        let mut app = GitClientApp::open_empty();
        let mut doc = automerge::AutoCommit::new();
        for (target, text) in [
            ("git.open-path", r"C:\Work\Grün 🌍"),
            ("git.commit-message", "Keep Unicode: Grün 🌍"),
            ("git.new-branch-name", "feature/grün"),
        ] {
            assert!(Widget::perform_semantic_action(
                &mut app,
                target,
                &ui::a11y::Action::SetValue(text.into()),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
        }
        assert_eq!(app.open_input.value(), r"C:\Work\Grün 🌍");
        assert_eq!(app.commit_input.value(), "Keep Unicode: Grün 🌍");
        assert_eq!(app.branch_input.value(), "feature/grün");
        assert!(app.test_awaiting_repo(), "editing a path must not open it");
    }

    fn synthetic_accessibility_controls_are_addressable_at_their_drawn_rects() {
        let mut app = app_at(1_280.0, Screen::Changes);
        let node = Widget::accessibility(&app).expect("git client is semantic");
        assert!(!node.children.is_empty());
        let targets: std::collections::HashSet<_> = node
            .children
            .iter()
            .filter_map(|child| child.action_target.as_deref())
            .collect();
        assert_eq!(targets.len(), node.children.len());
        assert!(targets.contains("git.commit-message"));
        assert!(
            targets
                .iter()
                .any(|target| target.starts_with("git.change.open:")),
            "the changed files are reachable: {targets:?}"
        );
        for (index, child) in node.children.iter().enumerate() {
            assert_eq!(
                Widget::synthetic_accessibility_rect(&app, &[index]),
                child.bounds.map(ui::a11y::AccessBounds::to_rect),
                "semantic control {index} must activate inside the visible control"
            );
        }

        let mut doc = automerge::AutoCommit::new();
        assert!(Widget::perform_semantic_action(
            &mut app,
            "git.commit-message",
            &ui::a11y::Action::Focus,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(app.focus, Some(Field::Commit));
    }

    /// Paint one frame with a real text renderer so every measured rect is
    /// the production geometry, not a hand-poked stand-in.
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
}
