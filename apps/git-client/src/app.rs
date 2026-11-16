//! The git client's window. `GitClientState` holds the in-memory [`RepoModel`], the
//! diffs, the active screen and all transient selection and scroll state; the view
//! shares it with its effect handler, and [`GitClientApp`] draws it. Durable
//! preferences live in the CRDT via [`Settings`]. The widget paints no header row:
//! the list column of the Changes and History screens sits on the sidebar region
//! through a [`SidebarDrawer`], and the screen's content on the window ground.
//! Presses are routed through the targets the last paint recorded.

mod a11y;
mod actions;
mod commits;
mod diff;
mod keys;
mod paint;
mod remote;
mod scroll;
mod test_support;
mod window_bar;

use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::rc::Rc;
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
use ui::framework::{
    EffectAnswer, EffectCx, EffectReply, Effected, Live, LiveRow, Publishes, RunMode, ViewCx,
};
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::ActionOutput;
use ui::tilo_resource::{AbsolutePath, Invoke};
use ui::tokens::TextRole;
use ui::widget::scrollbar::{BarFade, PressOutcome, ScrollMetrics, Scrollbar};
use ui::widget::{
    Banner, ButtonFace, ControlSize, Element, EmptyState, EmptyStateContent, ModalAction,
    ModalSpec, SidebarDrawer, TextInput, TextInputKind, Widget,
};

use crate::changes::{self, ChangesHit};
use crate::diffview::{self, DiffHit, HunkAction};
use crate::graph::{self, GraphLayout};
use crate::history;
use crate::i18n::{t, t_args};
use crate::model::{ChangeArea, DiffKey, DiffMap, FileChange, RepoModel};
use crate::panels::{self, BranchAction, PanelHit, RemoteAction};
use crate::settings::{DiffLayout, Settings};

/// Which embedded text field holds keyboard focus: text editing itself is the toolkit's
/// [`TextInput`], and this only says which one keys route to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Commit,
    Branch,
    Open,
    WorktreeBranch,
    WorktreePath,
    WorktreeStart,
}

struct InputEventCtx<'a> {
    event: &'a WindowEvent,
    scale_factor: f64,
    modifiers: winit::event::Modifiers,
    state: ui::widget::Edits<'a>,
    path: &'a ObjId,
    taffy: &'a TaffyTree<()>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Changes,
    History,
    Panels,
}

impl Screen {
    const ALL: [Self; 3] = [Self::Changes, Self::History, Self::Panels];

    /// Whether the screen has a list column; Branches is one page with no list.
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum Confirm {
    DiscardFile {
        area: ChangeArea,
        index: usize,
        name: String,
    },
    DiscardHunk(usize),
    DeleteBranch(String),
    /// Switch to a branch while the working tree holds uncommitted work: the person chooses what
    /// becomes of it. A clean tree switches without a question, so this only asks when there is
    /// work to lose.
    SwitchBranch {
        name: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct OpenControls {
    field: Rect,
    choose: Rect,
    open: Rect,
}

impl OpenControls {
    /// One row while the path keeps `path_room` beside the two buttons, two when it does not: at a
    /// large text scale in a long language the three controls do not fit side by side.
    fn at(
        lane: Rect,
        y: f64,
        h: f64,
        gap: f64,
        choose_w: f64,
        open_w: f64,
        path_room: f64,
    ) -> Self {
        let room = lane.width() - choose_w - open_w - 2.0 * gap;
        let path_room = path_room.min(lane.width() / 2.0);
        if room >= path_room {
            let open = Rect::new(lane.x1 - open_w, y, lane.x1, y + h);
            let choose = Rect::new(open.x0 - gap - choose_w, y, open.x0 - gap, y + h);
            return Self {
                field: Rect::new(lane.x0, y, choose.x0 - gap, y + h),
                choose,
                open,
            };
        }
        let choose = Rect::new(
            lane.x0,
            y + h + gap,
            (lane.x0 + choose_w).min(lane.x1),
            y + 2.0 * h + gap,
        );
        Self {
            field: Rect::new(lane.x0, y, lane.x1, y + h),
            open: Rect::new(lane.x1 - open_w, choose.y0, lane.x1, choose.y1),
            choose,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfirmChoice {
    Discard,
    DeleteBranch,
    /// Switch, taking the uncommitted work along.
    SwitchCarrying,
    /// Switch, parking the uncommitted work on the stash first.
    SwitchStashing,
}

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
            Confirm::SwitchBranch { .. } => "git-confirm-switch-body",
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
            // The work comes along or it is parked; there is no third answer that keeps it anywhere
            // else.
            Confirm::SwitchBranch { .. } => vec![
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
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Hit {
    /// The scrim behind an open drawer: dismisses it.
    Scrim,
    DismissError,
    Change(ChangesHit),
    Diff(DiffHit),
    Commit(usize),
    CommitFile(usize),
    BackToCommit,
    ResolveConflict(crate::repo::ConflictSide),
    ConflictNote,
    ConfirmQuestion,
    /// One answer on the sheet, by index: Cancel first, then the choices in the order the sheet
    /// offers them.
    ConfirmAnswer(usize),
    Panel(PanelHit),
    OpenPath,
    ChooseRepository,
    OpenRepository,
}

#[derive(Clone, Debug)]
struct Target {
    rect: Rect,
    hit: Hit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Staged {
    Nothing,
    OpenDrawer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scroller {
    ChangesList,
    Diff,
    HistoryList,
    Detail,
    Panels,
}

#[derive(Clone, Copy, Debug)]
struct ScrollRegion {
    rect: Rect,
    scroller: Scroller,
    max_y: f64,
    max_x: f64,
    /// The ground the content stands on, which the edge fades blend into.
    ground: vello::peniko::Color,
}

/// One owned repository operation. Its completion stays available until its action invocation
/// consumes it, even when a normal UI sync receives it first.
struct RemoteJob {
    receiver: Receiver<Result<(), String>>,
    reply: Option<EffectReply>,
    label: String,
}

/// What the window shows and the view's effect handler answers with: the repository, where the
/// person is, the fields, the question asked and the transfer running.
pub(crate) struct GitClientState {
    model: RepoModel,
    diffs: DiffMap,
    graph_layout: GraphLayout,

    /// Framework-owned action dispatch: opening a repository and recording what collaborators see
    /// of it, as do the effects the controls ask the host for.
    dispatcher: Option<ui::framework::Dispatcher<crate::resource::GitClient>>,
    bar: ui::framework::BarCommands<window_bar::BarCommand>,
    mode: RunMode,
    opened: Option<String>,
    presented: bool,
    preferences: Option<ui::framework::Setting<Settings>>,
    control_actions: Option<ui::framework::Actions<crate::resource::GitClient>>,

    /// No repository is bound: keep the empty state until an explicit path or the restored record
    /// selects one.
    awaiting_repo: bool,
    focus: Option<Field>,
    commit_input: TextInput,
    branch_input: TextInput,
    worktree_branch_input: TextInput,
    worktree_path_input: TextInput,
    worktree_start_input: TextInput,
    open_input: TextInput,

    /// The path the repository was opened from, `None` for the bundled sample (the screenshot
    /// path), where actions fall back to the in-memory model so the capture still shows the flow.
    repo_path: Option<std::path::PathBuf>,
    last_error: Option<String>,
    remote_job: Option<RemoteJob>,

    settings: Settings,
    /// The fingerprint of the last shared-repo projection written to the document.
    shared_repo_fingerprint: Option<String>,
    screen: Screen,
    rect: Rect,

    selected_change: Option<(ChangeArea, usize)>,
    changes_scroll: f64,
    diff_scroll: f64,
    diff_hscroll: f64,

    selected_commit: usize,
    history_scroll: f64,
    detail_scroll: f64,

    panels_scroll: f64,

    /// The changed files of the commits whose detail was opened, read on demand: walking every
    /// commit's tree diff up front costs a second on a real repository.
    commit_files: HashMap<String, Vec<FileChange>>,
    /// The commit diff on screen: the commit and the file the detail column shows instead of the
    /// commit's own page.
    commit_diff: Option<(String, String)>,
    /// The commit diffs shown so far, by commit and path.
    commit_diffs: HashMap<(String, String), crate::model::FileDiff>,
    file_selection: Option<usize>,
    branch_selection: Option<usize>,
    /// What keeping each side of the conflicted file would discard, and which file it was read for:
    /// read once per conflict rather than every frame.
    conflict_cost: Option<(String, Option<crate::repo::ConflictCost>)>,
    /// What the last action did, said once: a branch switched, a stash made, a push sent.
    notice: Option<String>,

    confirm: Option<Confirm>,
    /// The sheet asking the question; it owns its own hover, focus and activation, so the question
    /// answers to Tab and Enter like every other question in the system.
    confirm_sheet: ui::widget::ModalSheet,

    /// The list column of the Changes and History screens, docked beside the content when the
    /// window is wide and a floating drawer when it is narrow.
    drawer: SidebarDrawer,
    staged: Staged,

    pointer: Option<Point>,
    conventions: ui::platform::PlatformConventions,

    /// Targets the last paint recorded, back to front.
    targets: Vec<Target>,
    /// What assistive technology can reach: every control and row of the last paint, including row
    /// actions that show only on hover.
    semantic: Vec<Target>,
    scroll_regions: Vec<ScrollRegion>,
    scroll_bars: Vec<(Scroller, Rect, ScrollMetrics)>,
    scrollbar: Scrollbar,
    scrollbar_region: Option<Scroller>,
    /// The bars show while a region scrolls or the pointer is on one, then fade out like every
    /// toolkit scroll view's.
    bar_fade: BarFade,
}

impl GitClientState {
    fn open_at(path: &std::path::Path) -> Option<Self> {
        let anchor = crate::repo::workdir(path).unwrap_or_else(|| path.to_path_buf());
        let (model, diffs) = crate::repo::load(path)?;
        let mut app = Self::from_model(model, diffs);
        app.repo_path = Some(anchor);
        Some(app)
    }

    fn open_empty() -> Self {
        let mut app = Self::from_model(RepoModel::default(), DiffMap::new());
        app.awaiting_repo = true;
        app
    }

    fn from_model(mut model: RepoModel, diffs: DiffMap) -> Self {
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
            bar: ui::framework::BarCommands::new(),
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
            confirm_sheet: ui::widget::ModalSheet::new(),
            // Both lists are content the person reads, so one width docked or as a drawer keeps a
            // row the same in both; the content keeps the minimum plus a rail for its line numbers
            // before the list becomes a drawer.
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
}

/// The window: the shared `GitClientState`, drawn as one leaf that paints itself and follows the
/// record the document holds.
pub struct GitClientApp {
    state: Rc<RefCell<GitClientState>>,
    /// The record as the document holds it, shown as it changes; `None` for a window outside the
    /// framework.
    document: Option<Live<crate::resource::GitClient>>,
    presented: Option<(u64, u64)>,

    /// Input or a new document value changed what this widget paints: it repaints once and rests
    /// after the frame draws it.
    dirty: bool,
}

impl Default for GitClientApp {
    fn default() -> Self {
        Self::new()
    }
}

impl GitClientApp {
    pub fn new() -> Self {
        Self::open_empty()
    }

    /// The window over the bundled sample repository: a deterministic state with no repository on
    /// disk, for screenshots and demos.
    #[cfg(feature = "samples")]
    pub fn sample() -> Self {
        let (model, diffs) = crate::sample::model();
        Self::from_model(model, diffs)
    }

    /// Open the real repository discovered from `path`, so every action drives `git2` and re-reads;
    /// `None` when no repository is reachable.
    pub fn open_at(path: &std::path::Path) -> Option<Self> {
        GitClientState::open_at(path).map(Self::showing)
    }

    /// The empty "open a repository" state, the same one the live app shows when no repository is
    /// reachable.
    #[doc(hidden)]
    pub fn open_empty() -> Self {
        Self::showing(GitClientState::open_empty())
    }

    pub fn from_model(model: RepoModel, diffs: DiffMap) -> Self {
        Self::showing(GitClientState::from_model(model, diffs))
    }

    fn showing(state: GitClientState) -> Self {
        Self {
            dirty: true,
            state: Rc::new(RefCell::new(state)),
            document: None,
            presented: None,
        }
    }

    pub(crate) fn state(&self) -> RefMut<'_, GitClientState> {
        self.state.borrow_mut()
    }

    fn follow_document(&mut self) {
        let Some(document) = &self.document else {
            return;
        };
        let Some(shown) = document.changed_since(&mut self.presented) else {
            return;
        };
        self.state().present_state(&shown);
    }
}

/// Mount `window` as the view of the record: the view shares its state with the effect handler and
/// publishes the working tree, the history and an in-flight transfer as live rows.
pub(crate) fn window(cx: &ViewCx<crate::resource::GitClient>, mut window: GitClientApp) -> Element {
    window.document = Some(cx.live().clone());
    let state = Rc::clone(&window.state);
    {
        let mut state = state.borrow_mut();
        state.control_actions = Some(cx.actions());
        state.dispatcher = Some(cx.dispatcher());
        state.mode = cx.mode();
        #[cfg(feature = "samples")]
        if let Some(sample) = cx.fixture::<crate::resource::SampleRepository>() {
            state.show_model(sample.model.clone(), sample.diffs.clone());
        }
        state.follow_preferences(cx.settings::<Settings>());
    }
    {
        let (answer, status) = (Rc::clone(&state), Rc::clone(&state));
        cx.effects(move |effect: GitClientEffect, cx| {
            let shared = Rc::clone(&answer);
            answer.borrow_mut().answer_effect(effect, cx, shared)
        })
        .status(move || status.borrow().preference_statuses());
    }
    {
        let state = Rc::clone(&state);
        cx.on_stage(move |scenario| state.borrow_mut().stage_scenario(scenario));
    }
    let (rows, row) = (Rc::clone(&state), state);
    cx.publishing(
        Publishes::new()
            .live_rows(move || rows.borrow().working_tree_rows())
            .live_row(move |id| row.borrow().working_tree_row(id)),
        Box::new(window),
    )
}

impl Widget for GitClientApp {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        self.dirty = true;
        self.follow_document();
        self.state().sync();
    }

    fn settle(&mut self, _edits: &mut ui::widget::Edits<'_>, _path: &ObjId) {
        self.state().settle();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.state().drawer.scale_for(theme);
        // The app draws itself; a single full-size leaf node carries it.
        taffy.fill_leaf(node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.state().place(ui::widget::placed(&layout, origin));
    }

    fn set_platform_conventions(&mut self, conventions: ui::platform::PlatformConventions) {
        self.state().set_platform_conventions(conventions);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.state().draw(scene, transform, tr, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.state()
            .draw_overlay(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.dirty = true;
        self.state().handle_event(event, ctx);
    }

    fn needs_redraw(&self) -> bool {
        ui::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        ui::widget::Widget::mark_children_drawn(self);
    }

    fn name(&self) -> &'static str {
        "GitClientApp"
    }

    fn get_rect(&self) -> Rect {
        self.state.borrow().rect
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(self.state.borrow().accessibility())
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        self.state.borrow().synthetic_accessibility_rect(path)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.state()
            .perform_semantic_action(target, action, state, path)
    }

    fn needs_animation_frame(&self) -> bool {
        self.state.borrow().needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.state.borrow().next_wake_deadline()
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl ui::widget::controls::ActionControls for GitClientApp {
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        self.state.borrow().control_action_bindings()
    }
}

fn change_list(model: &RepoModel, area: ChangeArea) -> &[FileChange] {
    match area {
        ChangeArea::Staged => &model.staged,
        ChangeArea::Unstaged => &model.unstaged,
        ChangeArea::Untracked => &model.untracked,
    }
}

fn change_area(area: &str) -> Result<ChangeArea, String> {
    match area {
        "staged" => Ok(ChangeArea::Staged),
        "unstaged" => Ok(ChangeArea::Unstaged),
        "untracked" => Ok(ChangeArea::Untracked),
        _ => Err("unknown change area".into()),
    }
}

/// `diff` as unified diff text: each line behind its `+`, `-` or space marker, hunk headers and
/// metadata as git prints them.
fn unified_text(diff: &crate::model::FileDiff) -> String {
    use crate::model::DiffLineKind;
    let mut text = String::new();
    for line in &diff.lines {
        let marker = match line.kind {
            DiffLineKind::Context => " ",
            DiffLineKind::Addition => "+",
            DiffLineKind::Deletion => "-",
            DiffLineKind::Hunk | DiffLineKind::Meta => "",
        };
        text.push_str(marker);
        text.push_str(&line.content);
        text.push('\n');
    }
    text
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

impl GitClientState {
    fn sync(&mut self) {
        self.adopt_preferences();
        self.drain_remote();
        self.publish_shared_repo_if_needed();
    }

    fn settle(&mut self) {
        self.run_window_toolbar();
    }

    fn place(&mut self, rect: Rect) {
        self.rect = rect;
        // The drawer adopts the new width now, so the next press is tested against the layout the
        // next paint shows.
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
        let Some(confirm) = &self.confirm else {
            self.confirm_sheet.reset();
            return;
        };
        let window = presence.viewport.get().unwrap_or(self.rect);
        let title = confirm.title();
        let body = confirm.body();
        let choices = confirm.choices();
        let cancel = t("git-cancel");
        let mut actions = vec![ModalAction::secondary(&cancel)];
        // The answer the sheet recommends is its primary; a second answer that keeps the work
        // another way is the alternate, so the two do not read as the same act.
        let mut recommended = true;
        for face in &choices {
            actions.push(if face.danger {
                ModalAction::danger(&face.label)
            } else if std::mem::take(&mut recommended) {
                ModalAction::primary(&face.label)
            } else {
                ModalAction::alternate(&face.label)
            });
        }
        let spec = ModalSpec::new(&title)
            .actions(&actions)
            .dismiss_on_scrim(true);
        self.confirm_sheet
            .paint_message(scene, transform, text, theme, window, &spec, &body);
        // While the question is up it is the whole of what a person can act on: the picture under
        // the scrim is context, so the accessibility tree holds the question and its answers, not
        // the page they were asked about.
        let Some(layout) = self.confirm_sheet.layout() else {
            return;
        };
        let mut reached = vec![Target {
            rect: Rect::new(
                layout.title.x0,
                layout.title.y0,
                layout.body.x1,
                layout.body.y1,
            ),
            hit: Hit::ConfirmQuestion,
        }];
        for (index, rect) in layout.actions.iter().copied().enumerate() {
            if rect.area() > 0.0 {
                reached.push(Target {
                    rect,
                    hit: Hit::ConfirmAnswer(index),
                });
            }
        }
        self.semantic = reached;
        self.targets.clear();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // A question holds the window while it asks: it takes the pointer, the keyboard (Tab
        // between its answers, Enter for the one under focus) and the scrim's dismissal, and the
        // page behind takes nothing.
        if self.confirm.is_some() {
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.pointer = Some(Point::new(
                    position.x / ctx.scale_factor,
                    position.y / ctx.scale_factor,
                ));
            }
            if let Some(response) = self.confirm_sheet.handle(event, ctx.scale_factor) {
                self.answer_sheet(response);
            }
            *ctx.handled = true;
            return;
        }
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

    fn accessibility(&self) -> ui::a11y::AccessNode {
        let children: Vec<_> = self
            .accessibility_targets()
            .into_iter()
            .enumerate()
            .map(|(index, (target, _, node))| {
                // The ordinal is only the synthetic-tree geometry key: the action target is
                // semantic and stable across responsive layouts, so assistive technology never
                // activates a different control after a row appears or disappears.
                node.with_key(index as u64).with_action_target(target)
            })
            .collect();
        ui::a11y::AccessNode::new(ui::a11y::Role::Group)
            .with_name(crate::i18n::t("git-window-name"))
            .with_description(if let Some(job) = &self.remote_job {
                job.label.clone()
            } else if self.awaiting_repo {
                t("git-open-title")
            } else {
                t("git-app-description")
            })
            .with_children(children)
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
        // Frames only while the list column slides or the bars fade: a transfer wakes the app when
        // its worker replies, and the toolkit's caret is steady.
        (self.screen.has_list() && self.drawer.is_animating())
            || self.bar_fade.fading_at(ui::time::now(), self.bars_held())
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.bar_fade
            .fade_deadline_at(ui::time::now(), self.bars_held())
    }
}

impl GitClientState {
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

    /// Forward a pointer event to every text field, so an in-flight drag-select keeps updating
    /// whichever field started it.
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

    /// Route a left press: the confirmation sheet when it is up, a text field (focus and caret),
    /// then the target under the pointer; a press anywhere else blurs the focused field.
    fn handle_press(&mut self, ctx: &mut InputEventCtx<'_>) {
        let Some(pos) = self.pointer else {
            return;
        };
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

    fn submit_focused(&mut self, field: Field) {
        match field {
            Field::Commit => self.do_commit(),
            Field::Branch => self.create_new_branch(),
            Field::Open => self.open_repo_from_input(),
            Field::WorktreeBranch | Field::WorktreePath | Field::WorktreeStart => {
                let path = match AbsolutePath::new(self.worktree_path_input.value()) {
                    Ok(path) => path,
                    Err(error) => {
                        self.last_error = Some(error.into());
                        return;
                    }
                };
                if let Some(dispatcher) = &self.dispatcher {
                    dispatcher.effect(&GitClientEffect::CreateWorktree {
                        path,
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

        // A focused text field takes the keys, except Enter (submit) and Escape (blur) which the
        // app intercepts; text editing itself is the toolkit's TextInput.
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
        let cur = match self.selected_change {
            Some((ChangeArea::Staged, i)) => i,
            Some((ChangeArea::Unstaged, i)) => self.model.staged.len() + i,
            Some((ChangeArea::Untracked, i)) => {
                self.model.staged.len() + self.model.unstaged.len() + i
            }
            None => 0,
        } as i32;
        let next = (cur + dir).clamp(0, all as i32 - 1) as usize;
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

impl GitClientState {
    fn effect_for_hit(&self, hit: &Hit) -> Option<(Option<String>, GitClientEffect)> {
        let effect = match hit {
            Hit::ChooseRepository => GitClientEffect::ChooseRepository,
            Hit::Panel(PanelHit::CreateWorktree) => GitClientEffect::CreateWorktree {
                path: AbsolutePath::new(self.worktree_path_input.value()).ok()?,
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

impl GitClientState {
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

impl GitClientState {
    fn submit(&self, invoke: Result<Invoke<crate::resource::GitClient>, serde_json::Error>) {
        if let (Some(dispatcher), Ok(invoke)) = (&self.dispatcher, invoke) {
            dispatcher.submit(invoke);
        }
    }
}

impl GitClientState {
    fn switch_diff_layout(&mut self) -> Result<(), String> {
        self.diff_scroll = 0.0;
        self.change_preferences(Settings::toggle_layout)
    }
}

impl GitClientState {
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

impl GitClientState {
    pub(crate) fn follow_preferences(&mut self, preferences: ui::framework::Setting<Settings>) {
        self.settings = preferences.get();
        self.preferences = Some(preferences);
    }
}

impl GitClientState {
    fn adopt_preferences(&mut self) {
        if let Some(preferences) = &self.preferences {
            self.settings = preferences.state().with(|stored| *stored);
        }
    }
}

impl GitClientState {
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

impl GitClientState {
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

impl GitClientState {
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

impl GitClientState {
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

impl GitClientState {
    /// What changed in the file at `path` in `area`: the change as the list shows it and its diff
    /// as unified text.
    fn read_change(&self, area: &str, path: &str) -> Result<serde_json::Value, String> {
        let area = change_area(area)?;
        let change = change_list(&self.model, area)
            .iter()
            .find(|change| change.path == path)
            .ok_or_else(|| "changed file no longer exists".to_string())?;
        let diff = self.diffs.get(&DiffKey::for_area(area, path));
        Ok(serde_json::json!({
            "path": change.path,
            "old_path": change.old_path,
            "area": crate::actions::change_area_name(area),
            "kind": change.kind.label(),
            "additions": change.additions,
            "deletions": change.deletions,
            "binary": diff.is_some_and(|diff| diff.binary),
            "diff": diff.map(unified_text),
        }))
    }

    fn open_effect_view(&mut self, effect: GitClientEffect) -> Result<(), String> {
        match effect {
            GitClientEffect::ShowChanges => self.switch_screen(Screen::Changes),
            GitClientEffect::ShowHistory => self.switch_screen(Screen::History),
            GitClientEffect::ShowBranches => self.switch_screen(Screen::Panels),
            GitClientEffect::OpenChange { area, path } => {
                let area = change_area(&area)?;
                let index = change_list(&self.model, area)
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

    /// Do what `effect` asks, for any caller: the window's controls, the palette, an agent.
    /// `shared` is this state as the view shares it, for work that finishes after the answer.
    fn answer_effect(
        &mut self,
        effect: GitClientEffect,
        cx: &mut EffectCx<crate::resource::GitClient>,
        shared: Rc<RefCell<Self>>,
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
                | GitClientEffect::ReadChange { .. }
                | GitClientEffect::OpenCommit { .. }
        );
        if !view && self.repository_busy() {
            return Err(crate::i18n::t("git-remote-busy"));
        }
        match effect {
            GitClientEffect::OpenRepository { path } => self.open_repository_at(&path)?,
            GitClientEffect::ChooseRepository => return Ok(Self::choose_repository(cx, shared)),
            GitClientEffect::ReadChange { area, path } => {
                return self.read_change(&area, &path).map(Effected::value);
            }
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

impl GitClientState {
    /// Ask the system's file chooser for the repository's folder, then open it; cancelling leaves
    /// the window as it was.
    fn choose_repository(
        cx: &mut EffectCx<crate::resource::GitClient>,
        shared: Rc<RefCell<Self>>,
    ) -> Effected {
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
            move |&mut (), outcome, _cx| match outcome {
                ui::portal::PortalOutcome::Opened(path) => shared
                    .borrow_mut()
                    .open_repository_at(&path)
                    .map(|()| Effected::done()),
                ui::portal::PortalOutcome::Cancelled => Ok(Effected::done()),
                ui::portal::PortalOutcome::Failed(message) => {
                    shared.borrow_mut().last_error = Some(message.clone());
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
        cx: &mut EffectCx<crate::resource::GitClient>,
        path: AbsolutePath,
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
                let result = crate::repo::create_worktree(&repository, &path, &branch, &start)
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
        cx: &mut EffectCx<crate::resource::GitClient>,
    ) -> EffectAnswer {
        let reply = cx.reply();
        reply
            .progress()
            .report(crate::i18n::t("git-remote-running"), None, None);
        self.start_remote(action, Some(remote), Some(reply))?;
        Ok(Effected::Pending)
    }
}

impl GitClientState {
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

impl GitClientState {
    fn bind_repository(&mut self, path: &std::path::Path) -> bool {
        let Some((model, diffs)) = crate::repo::load(path) else {
            return false;
        };
        // Anchor actions on the working directory so later opens are stable even if the CWD
        // changes; fall back to the given path when bare.
        let anchor = crate::repo::workdir(path).unwrap_or_else(|| path.to_path_buf());
        self.show_model(model, diffs);
        self.repo_path = Some(anchor);
        true
    }
}

impl GitClientState {
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
            // A typed commit summary, so the commit box and its primary action render enabled.
            Some("commit") => self
                .commit_input
                .set_text("Flush the blur pass before presenting"),
            // A changed file in the worktree, whose "Discard file" the scenario then presses.
            Some("discard") => {
                if !self.model.unstaged.is_empty() {
                    self.selected_change = Some((ChangeArea::Unstaged, 0));
                } else if !self.model.untracked.is_empty() {
                    self.selected_change = Some((ChangeArea::Untracked, 0));
                }
                self.diff_scroll = 0.0;
            }
            // The file a merge left conflicted, open, so the capture shows the choice it waits on
            // and what each side costs.
            Some("conflict") => self.select_conflicted_file(),
            // The question a dirty tree asks before it changes branch.
            Some("switch") => {
                self.confirm = Some(Confirm::SwitchBranch {
                    name: "wgpu-pipeline".to_owned(),
                });
            }
            // The narrow window's list column, revealed over its scrim rather than the closed
            // default a fresh narrow window opens to.
            Some("narrow") => self.drawer.snap(true),
            _ => {}
        }
    }
}

impl GitClientState {
    fn select_conflicted_file(&mut self) {
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
}

impl GitClientState {
    pub(crate) fn show_model(&mut self, model: RepoModel, diffs: DiffMap) {
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
mod tests;
