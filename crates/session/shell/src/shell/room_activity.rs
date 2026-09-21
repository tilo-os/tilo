//! Room activity — everything that has happened to this workspace, by
//! everyone, in one filterable pane.
//!
//! The room's state lives in [`WorkspaceTree::doc`], a tree-wide
//! automerge document holding every workspace on this machine, so
//! [`RoomScope`] decides which workspaces' changes are shown — a pane
//! titled with one room's name must never render another's governance.
//! Not shown: the contents of documents open inside the room, which the
//! broker routes as opaque bytes it never materialises.
//!
//! A row's `[Revert]` writes the delta back as a **new forward change**;
//! history is appended to, never rewritten. Governance rows
//! ([`OpClass::Govern`]) are non-revertible — an owner counters a
//! governance act with another one rather than rewinding it.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use automerge::{ChangeHash, Prop, ReadDoc, ScalarValue, Value};
use collab_types::OpClass;
use collab_types::caps::{CapOverride, Effect, Principal, Scope, Subject, Verb};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::{
    ArtifactKind, HistoryClassification, HistoryEntry, HistoryEntryId, HistoryFilter,
};
use ui::{
    ActivityBrief, ActivityDigest, ActivityTimeline, AgentRun, AuthorDirectory, BandUndoRequest,
    ChangeRun, ColorCompat, GroupNameRequest, HistoryComparison, HistoryFacetIndex,
    HistorySelectionDetail, HistorySource, NamedRange, NarrativeRequest, RecordId, RecordStore,
    StepState, Theme, Widget, format_relative_time, participant_display_label,
};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::shell::state::{
    WorkspaceTask, WorkspaceTaskResource, WorkspaceTaskRoute, WorkspaceTaskStatus, WorkspaceTree,
};
use crate::shell::workspace_history::{
    FeedCoverage, WorkspaceHistoryEntry, WorkspaceHistoryId, WorkspaceHistoryIndex,
    WorkspaceHistoryQuery, WorkspaceSeenFrontier,
};

const AWAY_RECAP_AFTER: Duration = Duration::from_secs(45 * 60);
const BRIEF_EXPANSION_DURATION: Duration = Duration::from_millis(180);

fn away_recap_is_due(departed_at_secs: u64, now: SystemTime) -> bool {
    let departed_at = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(departed_at_secs))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    now.duration_since(departed_at)
        .is_ok_and(|elapsed| elapsed >= AWAY_RECAP_AFTER)
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct WorkspaceDeparture {
    departed_at_secs: u64,
    frontier: WorkspaceSeenFrontier,
}

/// Widest the sheet ever gets. Past this the rows stop gaining information
/// and start becoming hard to scan across.
const SHEET_MAX_W: f64 = 560.0;
/// Narrowest the sheet gets before it takes the whole viewport instead —
/// below this the content behind it is too thin to be worth preserving
/// (HIG's content-panel rule: show one thing at a time).
const SHEET_MIN_W: f64 = 360.0;
const FULL_BLEED_UNDER: f64 = 720.0;
const SHEET_INSET: f64 = 16.0;
const HEADER_H: f64 = 76.0;
const VIEW_ROW_H: f64 = 34.0;
const CHIP_ROW_H: f64 = 30.0;
const EXPLORER_LEFT_W: f64 = 180.0;
const EXPLORER_DETAIL_W: f64 = 240.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActivityView {
    /// Outcome-first summaries since this device's last completed visit.
    #[default]
    CatchUp,
    /// Durable work assigned to people or agents, connected to live app
    /// resources and the action history it produces.
    Tasks,
    /// Full granular history with grouping, filters, rename and revert.
    Explore,
    /// Causal workspace checkpoints played as layout + participant changes.
    Replay,
}

impl ActivityView {
    const ALL: [Self; 4] = [Self::CatchUp, Self::Tasks, Self::Explore, Self::Replay];

    fn label(self) -> &'static str {
        match self {
            Self::CatchUp => "Recap",
            Self::Tasks => "Tasks",
            Self::Explore => "Full history",
            Self::Replay => "Replay",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum LensSelection {
    #[default]
    All,
    AgentTasks,
    App(String),
    ResourceKind(String),
    Resource(String),
    WorkGroup(String),
    Facet(String, String),
    Unclassified,
}

/// Stable logical focus, independent of the rectangles produced by a frame.
/// Keeping values rather than row numbers means a refreshed history feed does
/// not silently move keyboard focus onto a different person or lens.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ActivityFocus {
    Scope(RoomScope),
    View(ActivityView),
    Period(Period),
    Author(Option<String>),
    MorePeople,
    Resume,
    Brief(usize),
    WorkSessionBoundary(WorkSessionBoundary),
    MoreBriefs,
    NewTask,
    Task(String),
    TaskRoute(WorkspaceTaskRoute),
    TaskCreate,
    TaskCancelCompose,
    TaskAction(TaskAction),
    Lens(LensSelection),
    MoreLenses,
    Detail(DetailAction),
    ExplorerPane(ExplorerPane),
    Timeline,
    ReplayBack,
    ReplayPlay,
    ReplayForward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BoundaryDirection {
    IntoNewer,
    IntoOlder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WorkSessionBoundary {
    older: usize,
    newer: usize,
    direction: BoundaryDirection,
}

#[derive(Clone, Debug)]
struct ReplayGroup {
    tabs: Vec<String>,
    active: usize,
    width_permille: u16,
}

#[derive(Clone, Debug)]
struct ReplayWorkspace {
    name: String,
    groups: Vec<ReplayGroup>,
}

#[derive(Clone, Debug)]
struct WorkspaceReplayFrame {
    entry_id: HistoryEntryId,
    author: ui::widget::HistoryAuthor,
    summary: String,
    timestamp: SystemTime,
    workspaces: Vec<ReplayWorkspace>,
    pointers: Vec<ReplayPointer>,
}

#[derive(Clone, Debug)]
struct ReplayPointer {
    name: String,
    color: (u8, u8, u8),
    point: (f32, f32),
    desktop_space: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskAction {
    OpenResource,
    StartAgent,
    Launch,
    Start,
    Complete,
    Cancel,
}

impl TaskAction {
    fn label(self) -> &'static str {
        match self {
            Self::OpenResource => "Open resource",
            Self::StartAgent => "Start agent",
            Self::Launch => "Launch agent",
            Self::Start => "Mark started",
            Self::Complete => "Mark done",
            Self::Cancel => "Cancel task",
        }
    }
}

struct AccessControl {
    role: ui::a11y::Role,
    selected: bool,
    expanded: Option<bool>,
    description: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DetailAction {
    Open,
    Compare,
    Undo,
    UndoTask,
    Split,
    Merge,
}

impl DetailAction {
    fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Compare => "Compare",
            Self::Undo => "Undo",
            Self::UndoTask => "Undo whole task",
            Self::Split => "Split",
            Self::Merge => "Merge",
        }
    }
}

/// A durable agent-run marker and the record whose document carries it.
/// `None` is reserved for a marker in the workspace tree itself.
#[derive(Clone, Debug, PartialEq)]
struct IndexedAgentRun {
    source_record: Option<RecordId>,
    run: AgentRun,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ExplorerPane {
    #[default]
    Timeline,
    Browse,
    Details,
}

impl ExplorerPane {
    const ALL: [Self; 3] = [Self::Timeline, Self::Browse, Self::Details];

    fn label(self) -> &'static str {
        match self {
            Self::Timeline => "Timeline",
            Self::Browse => "Browse",
            Self::Details => "Details",
        }
    }
}

/// Compose `transform` with a translation to `(dx, dy)`. `Affine`'s `Mul`
/// is pure `f64` matrix composition — it cannot overflow or panic — but
/// `arithmetic_side_effects` can't see that for a non-primitive `Mul`
/// impl, so every call site funnels through this one function instead of
/// an `#[allow]` at each of them.
struct ResumeSnapshot {
    target: ui::HistoryResourceTarget,
    author: ui::widget::history::HistoryAuthor,
    app: Option<String>,
    timestamp: SystemTime,
}

fn author_is_local(author: &ui::widget::history::HistoryAuthor) -> bool {
    let local = crate::shell::state::local_user_id();
    if local.is_empty() {
        return false;
    }
    author.name.eq_ignore_ascii_case(&local)
        || author
            .name
            .eq_ignore_ascii_case(&participant_display_label(&local))
        || author.identity_short == ui::history_source::short_id(&local)
}

fn local_resume_snapshot(entries: &[HistoryEntry]) -> Option<ResumeSnapshot> {
    entries
        .iter()
        .filter(|entry| author_is_local(&entry.author))
        .filter_map(|entry| {
            let context = entry.context.as_ref()?;
            let resource = context.resource.as_ref()?;
            Some(ResumeSnapshot {
                target: ui::HistoryResourceTarget {
                    app_id: context.app.as_ref().map(|app| app.id.clone()),
                    app_label: context.app.as_ref().map(|app| app.label.clone()),
                    id: resource.id.clone(),
                    kind: resource.kind.clone(),
                    label: resource.label.clone(),
                },
                author: entry.author.clone(),
                app: context.app.as_ref().map(|app| app.label.clone()),
                timestamp: entry.timestamp,
            })
        })
        .max_by_key(|resume| resume.timestamp)
}

#[allow(
    clippy::too_many_arguments,
    reason = "resume card rendering receives the ordinary immediate-mode scene and typography context"
)]
fn draw_resume_card(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    resume: &ResumeSnapshot,
    now: SystemTime,
) {
    let colors = &theme.tokens.colors;
    scene.fill(
        Fill::NonZero,
        transform,
        colors.accent[5].with_alpha_factor(0.13),
        None,
        &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.md)),
    );
    scene.fill(
        Fill::NonZero,
        transform,
        colors.accent[5],
        None,
        &RoundedRect::new(rect.x0, rect.y0 + 8.0, rect.x0 + 3.0, rect.y1 - 8.0, 1.5),
    );
    let (r, g, b) = ui::widget::history::hue_to_rgb(resume.author.avatar_hue);
    let initial = resume
        .author
        .name
        .chars()
        .next()
        .unwrap_or('\u{2022}')
        .to_uppercase()
        .collect::<String>();
    ui::widget::Avatar::draw_at(
        scene,
        transform,
        Rect::new(
            rect.x0 + 12.0,
            rect.y0 + 22.0,
            rect.x0 + 36.0,
            rect.y0 + 46.0,
        ),
        &initial,
        vello::peniko::Color::from_rgba8(r, g, b, 255),
        theme,
        tr,
    );
    tr.draw_text(
        scene,
        "YOU LEFT OFF",
        (theme.tokens.typography.scale.sm - 2.0).max(9.0),
        colors.accent[4],
        at(transform, rect.x0 + 46.0, rect.y0 + 7.0),
        None,
    );
    let title = tr.elide_to_width(
        &resume.target.label,
        theme.tokens.typography.scale.sm,
        text_width(rect.width() - 144.0),
    );
    tr.draw_text_weighted(
        scene,
        &title,
        theme.tokens.typography.scale.sm,
        colors.text_primary,
        at(transform, rect.x0 + 46.0, rect.y0 + 27.0),
        None,
        ui::text_render::TextWeight::Medium,
    );
    let context = resume.app.as_ref().map_or_else(
        || format_relative_time(resume.timestamp, now),
        |app| format!("{app} · {}", format_relative_time(resume.timestamp, now)),
    );
    let context = tr.elide_to_width(
        &context,
        theme.tokens.typography.scale.sm,
        text_width(rect.width() - 144.0),
    );
    tr.draw_text(
        scene,
        &context,
        theme.tokens.typography.scale.sm,
        colors.text_dim,
        at(transform, rect.x0 + 46.0, rect.y0 + 46.0),
        None,
    );
    let button = Rect::new(
        rect.x1 - 84.0,
        rect.y0 + 19.0,
        rect.x1 - 10.0,
        rect.y0 + 49.0,
    );
    scene.fill(
        Fill::NonZero,
        transform,
        colors.accent[5],
        None,
        &RoundedRect::from_rect(button, f64::from(theme.tokens.radii.sm)),
    );
    tr.draw_text_weighted(
        scene,
        "Resume →",
        theme.tokens.typography.scale.sm,
        theme
            .tokens
            .colors
            .on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
        at(transform, button.x0 + 10.0, button.y0 + 8.0),
        None,
        ui::text_render::TextWeight::Medium,
    );
}

#[cfg(test)]
mod resume_card_tests {
    use super::*;
    use collab_types::{ArtifactKind, OpClass};
    use ui::tilo_actions::ActionContext;
    use ui::widget::history::{HistoryAuthor, HistoryEntryId};

    fn resume_entry(id: u8, author: &str, timestamp: SystemTime) -> HistoryEntry {
        HistoryEntry {
            id: HistoryEntryId([id; 16]),
            author: HistoryAuthor::new(author, 210.0, ui::history_source::short_id(author)),
            op_class: OpClass::Splice,
            artifact_kind: ArtifactKind::File,
            timestamp,
            summary: "Edited assumptions".into(),
            text_delta: Some((4, 1)),
            diff_payload: None,
            redaction: None,
            context: Some(
                ActionContext::for_resource("Edit", "finance:q2", "sheet", "Q2 finance report")
                    .with_app("org.tilo.Sheets", "Sheets"),
            ),
        }
    }

    #[test]
    fn resume_uses_the_local_users_latest_targeted_action() {
        let local = crate::shell::state::local_user_id();
        let now = SystemTime::now();
        let older = now.checked_sub(Duration::from_secs(60)).unwrap_or(now);
        let entries = vec![resume_entry(1, &local, older), resume_entry(2, "Ada", now)];
        let resume = local_resume_snapshot(&entries).expect("local targeted work is resumable");
        assert_eq!(resume.target.label, "Q2 finance report");
        assert_eq!(resume.timestamp, older, "a peer's newer work is not yours");
    }

    #[test]
    fn resume_card_click_opens_the_exact_resource() {
        let mut pane = RoomActivityPane::new();
        let mut tree = WorkspaceTree::bootstrap_default();
        pane.resume_rect = Rect::new(10.0, 10.0, 200.0, 70.0);
        pane.resume_target = Some(ui::HistoryResourceTarget {
            app_id: Some("org.tilo.Sheets".into()),
            app_label: Some("Sheets".into()),
            id: "finance:q2".into(),
            kind: "sheet".into(),
            label: "Q2 finance report".into(),
        });
        let outcome = pane
            .handle_catchup_click(Point::new(40.0, 30.0), &mut tree)
            .expect("resume card handles its click");
        assert_eq!(outcome.open_target, pane.resume_target);
    }
}

#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix multiplication; it cannot overflow or panic"
)]
fn at(transform: Affine, dx: f64, dy: f64) -> Affine {
    transform * Affine::translate((dx, dy))
}

/// Convert a non-negative on-screen measurement for the text renderer.
#[allow(
    clippy::cast_possible_truncation,
    reason = "finite viewport dimensions are well within f32's range"
)]
fn text_width(width: f64) -> f32 {
    width.max(0.0) as f32
}

fn humanize_app_id(app_id: &str) -> String {
    app_id
        .rsplit(['.', '/'])
        .next()
        .unwrap_or(app_id)
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Number of complete rows that fit in a non-negative pixel extent.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped non-negative and bounded by the viewport"
)]
fn complete_rows(height: f64, row_height: f64) -> usize {
    (height.max(0.0) / row_height).floor() as usize
}

/// How far back the pane is showing. Maps onto
/// [`HistoryFilter::time_range`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Period {
    #[default]
    AllTime,
    Today,
    Week,
    Month,
}

impl Period {
    pub const ALL: [Period; 4] = [Period::AllTime, Period::Today, Period::Week, Period::Month];

    pub fn label(self) -> &'static str {
        match self {
            Period::AllTime => "All time",
            Period::Today => "Today",
            Period::Week => "7 days",
            Period::Month => "30 days",
        }
    }

    /// The half-open `[start, now)` window, or `None` for "everything".
    /// `now` is passed in, not read from the clock, so behaviour is
    /// testable and every row in one frame is judged against one instant.
    pub fn range(self, now: SystemTime) -> Option<(SystemTime, SystemTime)> {
        let back = match self {
            Period::AllTime => return None,
            Period::Today => Duration::from_secs(86400),
            Period::Week => Duration::from_secs(604_800),
            Period::Month => Duration::from_secs(2_592_000),
        };
        // The end is nudged past `now` so a change committed during this very
        // frame is inside the window rather than one tick outside it — the
        // interval is half-open at the top.
        let end = now.checked_add(Duration::from_secs(1)).unwrap_or(now);
        Some((now.checked_sub(back).unwrap_or(SystemTime::UNIX_EPOCH), end))
    }
}

/// Which workspaces' changes the pane is showing. A genuine scope
/// decision, not a filter — a row outside it is not this room's history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RoomScope {
    /// Changes touching the active workspace, plus ones belonging to no
    /// single workspace — those can't carry another room into this view.
    #[default]
    ThisRoom,
    /// Every change in the tree document, each row labelled by the
    /// workspace it belongs to.
    AllWorkspaces,
}

impl RoomScope {
    pub const ALL: [RoomScope; 2] = [RoomScope::ThisRoom, RoomScope::AllWorkspaces];

    pub fn label(self) -> &'static str {
        match self {
            RoomScope::ThisRoom => "This workspace",
            RoomScope::AllWorkspaces => "All workspaces",
        }
    }
}

/// What a dispatched event did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "these independent flags report distinct effects to the overlay host"
)]
pub struct RoomActivityOutcome {
    pub handled: bool,
    pub reverted: bool,
    /// A band was named/renamed/un-named — a document write too.
    pub named: bool,
    pub open_target: Option<ui::HistoryResourceTarget>,
    pub compared: bool,
    pub undo_partial: bool,
    pub task_changed: bool,
    /// A local managed-agent lifecycle changed. It is intentionally not a
    /// task-document write: a process beginning to connect is not work done.
    pub agent_runtime_changed: bool,
}

const AGENT_START_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Clone, Debug)]
enum AgentStartup {
    Starting { since: Instant },
    Failed { detail: String },
}

#[derive(Clone, Copy, Debug, Default)]
struct UndoApply {
    wrote: bool,
    partial: bool,
}

/// The room-activity sheet.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag represents an independently focusable activity-pane mode or asynchronous visual state"
)]
pub struct RoomActivityPane {
    explicit_local_user: Option<String>,
    timeline: ActivityTimeline,
    source: HistorySource,
    /// Set by the timeline's `FnMut` callback, drained right after — it
    /// can't reach back into this struct itself.
    revert_intent: Rc<Cell<Option<HistoryEntryId>>>,
    /// Same handshake as `revert_intent`; `RefCell` since the payload isn't `Copy`.
    name_intent: Rc<RefCell<Option<GroupNameRequest>>>,
    undo_intent: Rc<RefCell<Option<BandUndoRequest>>>,
    clear_filters_intent: Rc<Cell<bool>>,
    named: Vec<NamedRange>,
    /// Heads at the last rebuild, so a no-op frame skips re-deriving.
    last_heads: Vec<ChangeHash>,
    period: Period,
    /// Every row the document yielded, with the workspace each belongs
    /// to (`None` for no single workspace). [`Self::scope`] selects the
    /// subset handed to the timeline.
    derived: Vec<(HistoryEntry, Option<u64>)>,
    /// Tree rows after workspace scoping, retained as the shell-document
    /// contribution to the workspace-wide multi-document index.
    scoped_tree_rows: Vec<HistoryEntry>,
    history_index: WorkspaceHistoryIndex,
    projected_origins: HashMap<HistoryEntryId, WorkspaceHistoryId>,
    /// One app-native event supplied by an explicit handoff (for example a
    /// signed Files version). It remains visible while underlying indexes
    /// catch up, but is excluded from unseen counts and Catch up prose.
    external_entry: Option<HistoryEntry>,
    feed_coverage: FeedCoverage,
    scope: RoomScope,
    scope_rects: Vec<(RoomScope, Rect)>,
    author: Option<String>,
    /// First-seen order, derived from the rows rather than a roster — a
    /// name appears only if that person did something in this room.
    contributors: Vec<String>,
    sheet: Rect,
    /// Cleared whenever the pane isn't painted, so a stale click can't trip them.
    period_rects: Vec<(Period, Rect)>,
    author_rects: Vec<(Option<String>, Rect)>,
    author_more_rect: Option<Rect>,
    author_more_advance: usize,
    author_offset: usize,
    room_name: String,
    total_entries: usize,
    /// Outcome-first reduction of the currently scoped rows. The full
    /// timeline remains the source of detail underneath it.
    digest: ActivityDigest,
    /// Switching rooms moves no changes, so the heads check alone
    /// wouldn't notice — but it changes which rows are this room's.
    last_active: Option<u64>,
    now_override: Option<SystemTime>,
    /// Per-workspace private causal frontier. Opening history must not create
    /// collaborative history, and timestamps cannot identify delayed or
    /// concurrent CRDT changes correctly.
    seen_frontiers: HashMap<u64, WorkspaceSeenFrontier>,
    /// Private causal baseline captured when this device leaves a workspace.
    /// Wall time gates the presentation; the frontier decides which changes
    /// actually happened during the absence.
    departures: HashMap<u64, WorkspaceDeparture>,
    presence_workspace: Option<u64>,
    away_digest: ActivityDigest,
    prepared_away_workspace: Option<u64>,
    catchup_frontier: WorkspaceSeenFrontier,
    passive_digest: ActivityDigest,
    passive_unread: usize,
    persistent_frontiers: bool,
    view: ActivityView,
    view_rects: Vec<(ActivityView, Rect)>,
    brief_rects: Vec<(usize, Rect)>,
    boundary_rects: Vec<(WorkSessionBoundary, Rect)>,
    brief_more_rect: Option<Rect>,
    brief_more_advance: usize,
    brief_offset: usize,
    expanded_brief: Option<usize>,
    brief_expansion_started: Option<(usize, Instant)>,
    resume_rect: Rect,
    resume_target: Option<ui::HistoryResourceTarget>,
    tasks: Vec<WorkspaceTask>,
    selected_task: Option<String>,
    task_rects: Vec<(String, Rect)>,
    new_task_rect: Option<Rect>,
    task_action_rects: Vec<(TaskAction, Rect)>,
    task_compose_open: bool,
    task_draft: String,
    task_route: WorkspaceTaskRoute,
    task_route_rects: Vec<(WorkspaceTaskRoute, Rect)>,
    task_create_rect: Option<Rect>,
    task_cancel_rect: Option<Rect>,
    task_attachment: Option<WorkspaceTaskResource>,
    task_agent: Option<String>,
    task_broker: Option<crate::shell::collab_broker::AppActionBroker>,
    task_room: String,
    /// Local process lifecycle only. Broker subscription remains the source
    /// of truth for whether an agent may receive a task lease.
    agent_startup: HashMap<(String, String), AgentStartup>,
    facet_index: HistoryFacetIndex,
    lens: LensSelection,
    lens_rects: Vec<(LensSelection, Rect)>,
    lens_more_rect: Option<Rect>,
    lens_more_advance: usize,
    lens_offset: usize,
    keyboard_focus: Option<ActivityFocus>,
    detail_action_rects: Vec<(DetailAction, Rect)>,
    comparison: Option<HistoryComparison>,
    comparison_attempted: bool,
    comparison_selection: Vec<HistoryEntryId>,
    explorer_pane: ExplorerPane,
    explorer_pane_rects: Vec<(ExplorerPane, Rect)>,
    replay_frames: Vec<WorkspaceReplayFrame>,
    replay_index: usize,
    replay_playing: bool,
    replay_last_advance: Instant,
    replay_back_rect: Rect,
    replay_play_rect: Rect,
    replay_forward_rect: Rect,
    narrative: Option<ui::GroundedNarrative>,
    narrative_request: Option<NarrativeRequest>,
    /// Task receipts live beside the records they describe. They are indexed
    /// here so selecting any affected change can explain the whole task.
    agent_runs: Vec<IndexedAgentRun>,
    agent_undo_notice: Option<(String, String)>,
    open_agent_records: HashSet<RecordId>,
}

impl Default for RoomActivityPane {
    fn default() -> Self {
        Self::new()
    }
}

impl RoomActivityPane {
    /// Attach the live broker after the session binds. Without it, agent tasks
    /// remain visible but cannot be launched into an imaginary offline queue.
    pub fn attach_task_broker(&mut self, broker: crate::shell::collab_broker::AppActionBroker) {
        self.task_broker = Some(broker);
    }

    fn detail_action_label(&self, action: DetailAction) -> &'static str {
        if action == DetailAction::Open
            && self.selection_detail().is_some_and(|detail| {
                detail.resources.first().is_some_and(|resource| {
                    !resource.id.starts_with("file:") && !resource.id.contains("://")
                })
            })
        {
            "Open app"
        } else {
            action.label()
        }
    }

    pub fn new() -> Self {
        Self::from_local_user(None)
    }

    pub(crate) fn with_local_user(local: String) -> Self {
        Self::from_local_user(Some(local))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the shared pane constructor wires retained controllers and one-shot intents"
    )]
    fn from_local_user(explicit_local_user: Option<String>) -> Self {
        let revert_intent = Rc::new(Cell::new(None));
        let name_intent: Rc<RefCell<Option<GroupNameRequest>>> = Rc::new(RefCell::new(None));
        let undo_intent: Rc<RefCell<Option<BandUndoRequest>>> = Rc::new(RefCell::new(None));
        let clear_filters_intent = Rc::new(Cell::new(false));
        let ri = Rc::clone(&revert_intent);
        let ni = Rc::clone(&name_intent);
        let ui = Rc::clone(&undo_intent);
        let ci = Rc::clone(&clear_filters_intent);
        Self {
            explicit_local_user,
            timeline: {
                let mut t = ActivityTimeline::new()
                    .on_revert(move |id| ri.set(Some(*id)))
                    .on_undo_band(move |req| *ui.borrow_mut() = Some(req.clone()))
                    .on_clear_filters(move || ci.set(true))
                    .on_name_group(move |req| *ni.borrow_mut() = Some(req.clone()));
                t.set_draws_own_surface(false);
                // The shell owns the Everyone/person selector directly above
                // the timeline. Keep the operation chips, but never repeat an
                // `author:any` row inside the embedded widget.
                t.set_show_author_pill(false);
                t
            },
            // A workspace is a cluster of windows saved/reopened collaboratively.
            source: HistorySource::new().with_artifact_kind(ArtifactKind::Cluster),
            revert_intent,
            name_intent,
            undo_intent,
            clear_filters_intent,
            named: Vec::new(),
            last_heads: Vec::new(),
            period: Period::default(),
            derived: Vec::new(),
            scoped_tree_rows: Vec::new(),
            history_index: WorkspaceHistoryIndex::new(),
            projected_origins: HashMap::new(),
            external_entry: None,
            feed_coverage: FeedCoverage::default(),
            scope: RoomScope::default(),
            scope_rects: Vec::new(),
            author: None,
            contributors: Vec::new(),
            sheet: Rect::ZERO,
            period_rects: Vec::new(),
            author_rects: Vec::new(),
            author_more_rect: None,
            author_more_advance: 0,
            author_offset: 0,
            room_name: String::new(),
            total_entries: 0,
            digest: ActivityDigest::default(),
            last_active: None,
            now_override: None,
            seen_frontiers: HashMap::new(),
            departures: HashMap::new(),
            presence_workspace: None,
            away_digest: ActivityDigest::default(),
            prepared_away_workspace: None,
            catchup_frontier: WorkspaceSeenFrontier::default(),
            passive_digest: ActivityDigest::default(),
            passive_unread: 0,
            persistent_frontiers: false,
            view: ActivityView::default(),
            view_rects: Vec::new(),
            brief_rects: Vec::new(),
            boundary_rects: Vec::new(),
            brief_more_rect: None,
            brief_more_advance: 0,
            brief_offset: 0,
            expanded_brief: None,
            brief_expansion_started: None,
            resume_rect: Rect::ZERO,
            resume_target: None,
            tasks: Vec::new(),
            selected_task: None,
            task_rects: Vec::new(),
            new_task_rect: None,
            task_action_rects: Vec::new(),
            task_compose_open: false,
            task_draft: String::new(),
            task_route: WorkspaceTaskRoute::Manual,
            task_route_rects: Vec::new(),
            task_create_rect: None,
            task_cancel_rect: None,
            task_attachment: None,
            task_agent: None,
            task_broker: None,
            task_room: String::new(),
            agent_startup: HashMap::new(),
            facet_index: HistoryFacetIndex::default(),
            lens: LensSelection::default(),
            lens_rects: Vec::new(),
            lens_more_rect: None,
            lens_more_advance: 0,
            lens_offset: 0,
            keyboard_focus: None,
            detail_action_rects: Vec::new(),
            comparison: None,
            comparison_attempted: false,
            comparison_selection: Vec::new(),
            explorer_pane: ExplorerPane::default(),
            explorer_pane_rects: Vec::new(),
            replay_frames: Vec::new(),
            replay_index: 0,
            replay_playing: false,
            replay_last_advance: Instant::now(),
            replay_back_rect: Rect::ZERO,
            replay_play_rect: Rect::ZERO,
            replay_forward_rect: Rect::ZERO,
            narrative: None,
            narrative_request: None,
            agent_runs: Vec::new(),
            agent_undo_notice: None,
            open_agent_records: HashSet::new(),
        }
    }

    /// Production constructor. Tests use [`Self::new`] so ambient desktop
    /// settings cannot make their unread counts order-dependent.
    pub fn new_persistent() -> Self {
        let mut pane = Self::new();
        pane.persistent_frontiers = true;
        let section = settings_store::load_section("history");
        if let Some(frontiers) = section.get("seen_frontiers").and_then(|v| v.as_object()) {
            for (workspace, value) in frontiers {
                let Ok(id) = workspace.parse::<u64>() else {
                    continue;
                };
                if let Ok(frontier) = serde_json::from_value(value.clone()) {
                    pane.seen_frontiers.insert(id, frontier);
                }
            }
        }
        if let Some(departures) = section
            .get("departures")
            .and_then(|value| value.as_object())
        {
            for (workspace, value) in departures {
                let Ok(id) = workspace.parse::<u64>() else {
                    continue;
                };
                if let Ok(departure) = serde_json::from_value(value.clone()) {
                    pane.departures.insert(id, departure);
                }
            }
        }
        pane
    }

    /// Observe the active-workspace edge before refreshing the history index.
    /// The previous index still describes the workspace being left, so its
    /// exact causal frontier is the correct departure baseline.
    pub fn observe_workspace(&mut self, workspace: u64) {
        if self.presence_workspace == Some(workspace) {
            return;
        }
        if let Some(previous) = self.presence_workspace {
            let mut frontier = WorkspaceSeenFrontier::default();
            frontier.mark_seen(&self.history_index);
            let departed_at_secs = self
                .now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            self.departures.insert(
                previous,
                WorkspaceDeparture {
                    departed_at_secs,
                    frontier,
                },
            );
            if self.persistent_frontiers {
                self.persist_frontiers();
            }
        }
        self.presence_workspace = Some(workspace);
        self.away_digest = ActivityDigest::default();
    }

    /// Tests pin "now" so period windows don't depend on when the suite runs.
    pub fn set_now_for_test(&mut self, now: SystemTime) {
        self.now_override = Some(now);
        self.timeline.set_now_for_test(now);
    }

    fn now(&self) -> SystemTime {
        self.now_override.unwrap_or_else(SystemTime::now)
    }

    pub fn entries(&self) -> &[HistoryEntry] {
        self.timeline.entries()
    }

    /// Install the exact app-native event selected by another app before its
    /// Workspace Activity deep link is applied.
    pub fn open_external_entry(&mut self, entry: HistoryEntry) {
        let old = self.external_entry.as_ref().map(|entry| entry.id);
        let rows = self
            .timeline
            .entries()
            .iter()
            .filter(|candidate| Some(candidate.id) != old)
            .cloned()
            .collect();
        self.external_entry = Some(entry);
        self.install_rows(rows);
    }

    pub fn visible_len(&self) -> usize {
        self.timeline.visible_len()
    }

    pub fn visible_entry(&self, i: usize) -> Option<&HistoryEntry> {
        self.timeline.visible_entry(i)
    }

    pub fn digest(&self) -> &ActivityDigest {
        &self.digest
    }

    /// Snapshot the previous private frontier before the catch-up surface is
    /// shown. It advances only when the surface closes, so opening it cannot
    /// erase the very summary the user came to read.
    pub fn begin_view(&mut self, workspace: u64) {
        if self.prepared_away_workspace.take() == Some(workspace)
            && let Some(departure) = self.departures.get(&workspace)
        {
            self.catchup_frontier = departure.frontier.clone();
            self.digest = self.away_digest.clone();
            self.reset_narrative();
            return;
        }
        self.catchup_frontier = self
            .seen_frontiers
            .get(&workspace)
            .cloned()
            .unwrap_or_default();
        self.digest = self.passive_digest.clone();
        self.reset_narrative();
    }

    pub fn prepare_away_view(&mut self, workspace: u64) {
        self.prepared_away_workspace = Some(workspace);
    }

    pub fn away_change_count(&self, workspace: u64) -> usize {
        if self.presence_workspace == Some(workspace) {
            self.away_digest.total_changes
        } else {
            0
        }
    }

    pub fn needs_animation_frame(&self, now: Instant) -> bool {
        self.brief_expansion_started.is_some_and(|(_, started)| {
            now.saturating_duration_since(started) < BRIEF_EXPANSION_DURATION
        })
    }

    pub fn finish_view(&mut self, workspace: u64) {
        if self.timeline.entries().is_empty() {
            return;
        }
        let frontier = self.seen_frontiers.entry(workspace).or_default();
        if self.history_index.entries().is_empty() {
            for entry in self.timeline.entries() {
                if self
                    .external_entry
                    .as_ref()
                    .is_some_and(|external| external.id == entry.id)
                {
                    continue;
                }
                frontier.acknowledge(&WorkspaceHistoryId {
                    origin: crate::shell::workspace_history::HistoryOrigin::WorkspaceTree,
                    change: entry.id,
                });
            }
        } else {
            frontier.mark_seen(&self.history_index);
        }
        self.catchup_frontier = frontier.clone();
        self.digest = ActivityDigest::default();
        self.passive_digest = ActivityDigest::default();
        self.passive_unread = 0;
        self.reset_narrative();
        if self.persistent_frontiers {
            self.persist_frontiers();
        }
    }

    /// `(headline, unread, pulse)` consumed by passive chrome. `pulse` is a
    /// stable projection of the newest content id, not a frame counter.
    pub fn glance_signal(&self, _workspace: u64) -> (Option<String>, usize, u64) {
        let unread = self.passive_unread;
        let latest = self
            .timeline
            .entries()
            .iter()
            .max_by_key(|entry| entry.timestamp);
        let pulse = latest.map_or(0, |entry| {
            let mut bytes = [0_u8; 8];
            bytes.copy_from_slice(&entry.id.as_bytes()[..8]);
            u64::from_le_bytes(bytes)
        });
        (self.passive_digest.headline(), unread, pulse)
    }

    pub fn digest_for_workspace(&self, workspace: u64) -> ActivityDigest {
        let _ = workspace;
        self.passive_digest.clone()
    }

    fn open_brief_in_explorer(&mut self, index: usize) {
        let Some(brief) = self.digest.briefs.get(index) else {
            return;
        };
        let positions: Vec<usize> = brief
            .entry_ids
            .iter()
            .filter_map(|id| {
                self.timeline
                    .entries()
                    .iter()
                    .position(|entry| entry.id == *id)
            })
            .collect();
        if let (Some(start), Some(end)) = (positions.iter().min(), positions.iter().max()) {
            self.timeline.set_selection(Some((*start, *end)));
            self.view = ActivityView::Explore;
            self.expanded_brief = None;
            self.clear_comparison();
        }
    }

    fn persist_frontiers(&self) {
        let mut frontiers = serde_json::Map::new();
        for (workspace, frontier) in &self.seen_frontiers {
            if let Ok(value) = serde_json::to_value(frontier) {
                frontiers.insert(workspace.to_string(), value);
            }
        }
        let mut section = serde_json::Map::new();
        section.insert(
            "seen_frontiers".to_string(),
            serde_json::Value::Object(frontiers),
        );
        let departures = self
            .departures
            .iter()
            .filter_map(|(workspace, departure)| {
                serde_json::to_value(departure)
                    .ok()
                    .map(|value| (workspace.to_string(), value))
            })
            .collect();
        section.insert(
            "departures".to_string(),
            serde_json::Value::Object(departures),
        );
        if let Err(error) = settings_store::update_section("history", section) {
            tracing::warn!(%error, "could not persist history seen frontier");
        }
    }

    pub fn contributors(&self) -> &[String] {
        &self.contributors
    }

    pub fn view(&self) -> ActivityView {
        self.view
    }

    pub fn set_view(&mut self, view: ActivityView) {
        if view != ActivityView::Replay {
            self.replay_playing = false;
        }
        self.view = view;
        self.expanded_brief = None;
    }

    fn step_replay(&mut self, forward: bool) {
        self.replay_playing = false;
        self.replay_index = if forward {
            self.replay_index
                .saturating_add(1)
                .min(self.replay_frames.len().saturating_sub(1))
        } else {
            self.replay_index.saturating_sub(1)
        };
        self.replay_last_advance = Instant::now();
    }

    fn toggle_replay(&mut self) {
        if self.replay_frames.is_empty() {
            return;
        }
        if self.replay_index.saturating_add(1) >= self.replay_frames.len() {
            self.replay_index = 0;
        }
        self.replay_playing = !self.replay_playing;
        self.replay_last_advance = Instant::now();
    }

    pub fn period(&self) -> Period {
        self.period
    }

    pub fn author_filter(&self) -> Option<&str> {
        self.author.as_deref()
    }

    /// `Rect::ZERO` before the first layout.
    pub fn rect(&self) -> Rect {
        self.sheet
    }

    pub fn timeline(&self) -> &ActivityTimeline {
        &self.timeline
    }

    pub fn timeline_mut(&mut self) -> &mut ActivityTimeline {
        &mut self.timeline
    }

    pub fn has_comparison(&self) -> bool {
        self.comparison.is_some()
    }

    pub fn replay_is_playing(&self) -> bool {
        self.view == ActivityView::Replay && self.replay_playing
    }

    pub fn selection_detail(&self) -> Option<HistorySelectionDetail> {
        let range = self.timeline.selection()?;
        let mut detail = HistorySelectionDetail::from_range(self.timeline.entries(), range)?;
        let mut origin = None;
        for id in &detail.entry_ids {
            let next = self.projected_origins.get(id).map_or(
                crate::shell::workspace_history::HistoryOrigin::WorkspaceTree,
                |address| address.origin.clone(),
            );
            if origin.as_ref().is_some_and(|current| current != &next) {
                detail.can_compare = false;
                detail.can_undo = false;
                break;
            }
            origin = Some(next);
        }
        Some(detail)
    }

    fn selected_agent_run(&self) -> Option<&IndexedAgentRun> {
        let selected = self.selected_ids();
        self.agent_runs.iter().find(|indexed| {
            indexed.run.steps.iter().any(|step| {
                selected
                    .iter()
                    .any(|id| self.agent_step_contains(indexed, step, id))
            })
        })
    }

    fn agent_step_contains(
        &self,
        indexed: &IndexedAgentRun,
        step: &ui::RunStep,
        selected: &HistoryEntryId,
    ) -> bool {
        let projected = self.projected_origins.get(selected);
        let origin = projected.map(|item| &item.origin);
        // Timeline ids are collision-safe projections across every open
        // record. Agent receipts address the underlying document change, so
        // compare and order through the origin index rather than mixing those
        // two namespaces.
        let selected_change = projected.map_or(*selected, |item| item.change);
        let origin_matches = match origin {
            Some(crate::shell::workspace_history::HistoryOrigin::Record { id, .. }) => {
                step.record.is_empty() || id.as_str() == step.record
            }
            Some(crate::shell::workspace_history::HistoryOrigin::WorkspaceTree) | None => {
                indexed.source_record.is_none()
            }
        };
        if !origin_matches {
            return false;
        }
        if selected_change == step.run.start || selected_change == step.run.end {
            return true;
        }

        // History IDs are hashes, not sortable sequence numbers. Determine
        // membership from this origin's causal timeline order instead.
        let ids = self
            .history_index
            .entries()
            .iter()
            .filter(|entry| match (&entry.id.origin, origin) {
                (
                    crate::shell::workspace_history::HistoryOrigin::Record { id: a, .. },
                    Some(crate::shell::workspace_history::HistoryOrigin::Record { id: b, .. }),
                ) => a == b,
                (
                    crate::shell::workspace_history::HistoryOrigin::WorkspaceTree,
                    Some(crate::shell::workspace_history::HistoryOrigin::WorkspaceTree) | None,
                ) => true,
                _ => false,
            })
            .map(|entry| entry.id.change)
            .collect::<Vec<_>>();
        let Some(at) = ids.iter().position(|id| *id == selected_change) else {
            return false;
        };
        let (Some(start), Some(end)) = (
            ids.iter().position(|id| *id == step.run.start),
            ids.iter().position(|id| *id == step.run.end),
        ) else {
            return false;
        };
        (start.min(end)..=start.max(end)).contains(&at)
    }

    /// The reviewable task associated with the selected history change.
    /// Kept small and read-only for integration tests and future deep links.
    pub fn selected_agent_task(&self) -> Option<&AgentRun> {
        self.selected_agent_run().map(|indexed| &indexed.run)
    }

    fn agent_name(&self, run: &AgentRun) -> String {
        run.by.as_deref().map_or_else(
            || "Agent".to_string(),
            |actor| self.source.authors().resolve(actor).name,
        )
    }

    fn issuer_name(&self, run: &AgentRun) -> String {
        run.issued_by.as_deref().map_or_else(
            || "Unknown collaborator".to_string(),
            |actor| self.source.authors().resolve(actor).name,
        )
    }

    fn can_undo_agent_run(&self) -> bool {
        let Some(indexed) = self.selected_agent_run() else {
            return false;
        };
        indexed.run.steps.iter().all(|step| {
            RecordId::parse(&step.record).is_some_and(|id| self.open_agent_records.contains(&id))
        })
    }

    fn selected_ids(&self) -> Vec<HistoryEntryId> {
        self.timeline
            .selection()
            .and_then(|(start, end)| self.timeline.entries().get(start..=end))
            .map(|entries| entries.iter().map(|entry| entry.id).collect())
            .unwrap_or_default()
    }

    fn clear_comparison(&mut self) {
        self.comparison = None;
        self.comparison_attempted = false;
        self.comparison_selection.clear();
    }

    fn advance_people(&mut self) {
        let next = self.author_offset.saturating_add(self.author_more_advance);
        self.author_offset = if next < self.contributors.len() {
            next
        } else {
            0
        };
    }

    fn advance_briefs(&mut self) {
        let next = self.brief_offset.saturating_add(self.brief_more_advance);
        self.brief_offset = if next < self.digest.briefs.len() {
            next
        } else {
            0
        };
        self.expanded_brief = None;
    }

    fn move_work_session_boundary(
        &mut self,
        tree: &mut WorkspaceTree,
        boundary: WorkSessionBoundary,
    ) -> bool {
        let Some(older) = self.digest.briefs.get(boundary.older).cloned() else {
            return false;
        };
        let Some(newer) = self.digest.briefs.get(boundary.newer).cloned() else {
            return false;
        };
        let (Some(older_start), Some(older_end), Some(newer_start), Some(newer_end)) = (
            older.entry_ids.first().copied(),
            older.entry_ids.last().copied(),
            newer.entry_ids.first().copied(),
            newer.entry_ids.last().copied(),
        ) else {
            return false;
        };
        let (new_left_end, new_right_start) = match boundary.direction {
            BoundaryDirection::IntoNewer => {
                let Some((&new_right_start, preceding)) = older.entry_ids.split_last() else {
                    return false;
                };
                let Some(&new_left_end) = preceding.last() else {
                    return false;
                };
                (new_left_end, new_right_start)
            }
            BoundaryDirection::IntoOlder => {
                let [new_left_end, new_right_start, ..] = newer.entry_ids.as_slice() else {
                    return false;
                };
                (*new_left_end, *new_right_start)
            }
        };
        if ui::history_groups::materialize_and_move_boundary(
            &mut tree.doc,
            older.work_session_id.as_deref(),
            &older_start,
            &older_end,
            &older.title,
            newer.work_session_id.as_deref(),
            &newer_start,
            &newer_end,
            &newer.title,
            &new_left_end,
            &new_right_start,
        )
        .is_none()
        {
            return false;
        }
        tree.commit_room_change();
        self.last_heads.clear();
        self.refresh(tree);
        true
    }

    #[cfg(feature = "hosted-test-control")]
    pub fn expand_work_session_for_shots(&mut self, index: usize) -> bool {
        if index >= self.digest.briefs.len() {
            return false;
        }
        self.expanded_brief = Some(index);
        true
    }

    /// Focus the first recap outcome that the settled responsive layout
    /// actually rendered. The following Enter still travels through the
    /// production keyboard handler; this only avoids assuming whether a
    /// compact layout ended with a paging control.
    #[cfg(feature = "hosted-test-control")]
    pub fn focus_first_work_session_for_shots(&mut self) -> bool {
        let Some((index, _)) = self.brief_rects.first().copied() else {
            return false;
        };
        self.keyboard_focus = Some(ActivityFocus::Brief(index));
        true
    }

    /// Give deterministic visual journeys a real, elapsed departure frontier
    /// without reading or overwriting this device's persisted history state.
    #[cfg(feature = "hosted-test-control")]
    pub fn prepare_away_recap_for_shots(&mut self, tree: &mut WorkspaceTree) {
        let workspace = tree.active.raw();
        let departed_at_secs = self
            .now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_sub(AWAY_RECAP_AFTER.as_secs().saturating_add(60));
        self.departures.insert(
            workspace,
            WorkspaceDeparture {
                departed_at_secs,
                frontier: WorkspaceSeenFrontier::default(),
            },
        );
        self.presence_workspace = Some(workspace);
        self.refresh(tree);
        self.prepare_away_view(workspace);
    }

    #[cfg(feature = "hosted-test-control")]
    pub fn move_work_session_boundary_for_shots(
        &mut self,
        tree: &mut WorkspaceTree,
        older: usize,
        newer: usize,
        into_newer: bool,
    ) -> bool {
        self.move_work_session_boundary(
            tree,
            WorkSessionBoundary {
                older,
                newer,
                direction: if into_newer {
                    BoundaryDirection::IntoNewer
                } else {
                    BoundaryDirection::IntoOlder
                },
            },
        )
    }

    fn advance_lenses(&mut self) {
        let next = self.lens_offset.saturating_add(self.lens_more_advance);
        self.lens_offset = if next < self.lens_candidates().len() {
            next
        } else {
            0
        };
    }

    fn entry_position(&self, id: &HistoryEntryId) -> Option<usize> {
        self.timeline
            .entries()
            .iter()
            .position(|entry| entry.id == *id)
    }

    fn named_positions(&self) -> Vec<(&NamedRange, usize, usize)> {
        let last = self.timeline.entries().len().saturating_sub(1);
        let mut ranges = self
            .named
            .iter()
            .filter_map(|range| {
                let start = self.entry_position(&range.start)?;
                let end = range
                    .end
                    .as_ref()
                    .and_then(|id| self.entry_position(id))
                    .unwrap_or(last);
                Some((range, start.min(end), start.max(end)))
            })
            .collect::<Vec<_>>();
        ranges.sort_by_key(|(_, start, _)| *start);
        ranges
    }

    fn split_candidate(&self) -> Option<(&NamedRange, HistoryEntryId, HistoryEntryId)> {
        let (selected, _) = self.timeline.selection()?;
        let (range, start, end) = self
            .named_positions()
            .into_iter()
            .find(|(_, start, end)| *start < selected && selected <= *end)?;
        let left_end = self.timeline.entries().get(selected.checked_sub(1)?)?.id;
        let right_start = self.timeline.entries().get(selected)?.id;
        debug_assert!(start < selected && selected <= end);
        Some((range, left_end, right_start))
    }

    fn merge_candidate(&self) -> Option<(Vec<String>, HistoryEntryId, HistoryEntryId, String)> {
        let (selected_start, selected_end) = self.timeline.selection()?;
        let ranges = self
            .named_positions()
            .into_iter()
            .filter(|(_, start, end)| *end >= selected_start && *start <= selected_end)
            .collect::<Vec<_>>();
        if ranges.len() < 2 {
            return None;
        }
        let first = ranges.first()?;
        let last = ranges.last()?;
        Some((
            ranges
                .iter()
                .map(|(range, _, _)| range.id.clone())
                .collect(),
            self.timeline.entries().get(first.1)?.id,
            self.timeline.entries().get(last.2)?.id,
            first.0.label.clone(),
        ))
    }

    fn split_selection_group(&mut self, tree: &mut WorkspaceTree) -> bool {
        let Some((range, left_end, right_start)) = self.split_candidate() else {
            return false;
        };
        let record_id = range.id.clone();
        let label = range.label.clone();
        let right_label = format!("{label} — continued");
        if ui::history_groups::split_named_range(
            &mut tree.doc,
            &record_id,
            &left_end,
            &right_start,
            &label,
            &right_label,
        )
        .is_none()
        {
            return false;
        }
        tree.commit_room_change();
        self.last_heads.clear();
        self.refresh(tree);
        true
    }

    fn merge_selection_groups(&mut self, tree: &mut WorkspaceTree) -> bool {
        let Some((records, start, end, label)) = self.merge_candidate() else {
            return false;
        };
        if ui::history_groups::merge_named_ranges(
            &mut tree.doc,
            &records,
            &start,
            Some(&end),
            &label,
        )
        .is_none()
        {
            return false;
        }
        tree.commit_room_change();
        self.last_heads.clear();
        self.refresh(tree);
        true
    }

    /// Materialize the exact before/after document for the current selection.
    /// Mixed-document selections deliberately have no synthetic diff: callers
    /// can split them by provenance and compare each resource honestly.
    pub fn compare_selection(
        &self,
        tree: &mut WorkspaceTree,
        records: Option<&RecordStore>,
    ) -> Option<HistoryComparison> {
        let (start, end) = self.timeline.selection()?;
        let selected = self.timeline.entries().get(start..=end)?;
        let addresses: Vec<WorkspaceHistoryId> = selected
            .iter()
            .map(|entry| {
                self.projected_origins
                    .get(&entry.id)
                    .cloned()
                    .unwrap_or(WorkspaceHistoryId {
                        origin: crate::shell::workspace_history::HistoryOrigin::WorkspaceTree,
                        change: entry.id,
                    })
            })
            .collect();
        let first = addresses.first()?;
        if addresses
            .iter()
            .any(|address| address.origin != first.origin)
        {
            return None;
        }
        let ids: Vec<HistoryEntryId> = addresses.iter().map(|address| address.change).collect();
        match &first.origin {
            crate::shell::workspace_history::HistoryOrigin::WorkspaceTree => {
                ui::compare_entries(&mut tree.doc, &ids).ok()
            }
            crate::shell::workspace_history::HistoryOrigin::Record { id, .. } => records?
                .with(id, |doc| ui::compare_entries(doc, &ids).ok())
                .flatten(),
        }
    }

    pub fn set_period(&mut self, period: Period) {
        if self.period != period {
            self.period = period;
            self.reinstall_filter();
        }
    }

    pub fn set_author_filter(&mut self, author: Option<String>) {
        if self.author != author {
            self.author = author;
            self.reinstall_filter();
        }
    }

    pub fn apply_deep_link(&mut self, focus: crate::shell::chrome_state::ActivityFocus) {
        if let Some(author) = focus.author {
            self.author = Some(author);
        }
        if let Some(group) = focus.group_id {
            self.lens = LensSelection::WorkGroup(group);
            self.view = ActivityView::Explore;
        }
        if let Some(raw) = focus.entry_id.or(focus.resource_version) {
            let id = HistoryEntryId::from_bytes(raw);
            if let Some(index) = self
                .timeline
                .entries()
                .iter()
                .position(|entry| entry.id == id)
            {
                self.timeline.set_selection(Some((index, index)));
                self.view = ActivityView::Explore;
            }
        }
        if let Some(resource) = focus.resource_id {
            let matching = self.timeline.entries().iter().position(|entry| {
                let context_match = entry.context.as_ref().is_some_and(|context| {
                    context.resource.as_ref().is_some_and(|item| {
                        item.id == resource
                            || item.id.strip_prefix("file:") == Some(resource.as_str())
                    })
                });
                let origin_match = self.projected_origins.get(&entry.id).is_some_and(|origin| {
                    matches!(
                        &origin.origin,
                        crate::shell::workspace_history::HistoryOrigin::Record { id, .. }
                            if id.as_str() == resource
                    )
                });
                context_match || origin_match
            });
            self.lens = LensSelection::Resource(resource);
            self.view = ActivityView::Explore;
            if let Some(index) = matching {
                self.timeline.set_selection(Some((index, index)));
                self.explorer_pane = ExplorerPane::Details;
            }
        }
        self.reinstall_filter();
    }

    /// Op-class chips and author typeahead are the widget's own, left alone.
    fn reinstall_filter(&mut self) {
        let prior_selection = self.selected_ids();
        let mut filter = HistoryFilter {
            time_range: self.period.range(self.now()),
            ..HistoryFilter::default()
        };
        if let Some(a) = &self.author {
            filter.authors.push(a.clone());
        }
        match &self.lens {
            LensSelection::All => {}
            LensSelection::AgentTasks => {
                filter
                    .work_groups
                    .extend(self.agent_runs.iter().map(|indexed| {
                        indexed
                            .run
                            .instruction
                            .as_ref()
                            .unwrap_or(&indexed.run.label)
                            .clone()
                    }));
            }
            LensSelection::App(app) => filter.apps.push(app.clone()),
            LensSelection::ResourceKind(kind) => filter.resource_kinds.push(kind.clone()),
            LensSelection::Resource(resource) => filter.resources.push(resource.clone()),
            LensSelection::WorkGroup(group) => filter.work_groups.push(group.clone()),
            LensSelection::Facet(key, value) => filter
                .facets
                .push(ui::tilo_actions::ActionFacet::new(key, value)),
            LensSelection::Unclassified => {
                filter.classification = HistoryClassification::Unclassified;
            }
        }
        self.timeline.set_filter(filter);
        if self.selected_ids() != prior_selection {
            self.clear_comparison();
        }
        self.timeline.set_time_range_label(match self.period {
            Period::AllTime => None,
            p => Some(p.label().to_string()),
        });
    }

    /// Re-derive the timeline from the room document if the graph has
    /// moved; called every frame, the heads check keeps that cheap.
    /// `&mut` because reading a change graph closes any open transaction.
    #[allow(
        clippy::too_many_lines,
        reason = "timeline refresh keeps document observation, filter projection, and retained activity rows in one ordered read-only projection pass"
    )]
    pub fn refresh(&mut self, tree: &mut WorkspaceTree) {
        tree.commit_room_change();

        let name = tree
            .active_workspace()
            .map(|w| w.name.clone())
            .unwrap_or_default();
        if name != self.room_name {
            self.room_name = name;
        }
        self.task_room = tree
            .active_workspace()
            .map(|workspace| workspace.room.to_string())
            .unwrap_or_default();
        self.tasks = tree
            .active_workspace()
            .map(|workspace| workspace.tasks.clone())
            .unwrap_or_default();
        self.tasks.sort_by(|left, right| {
            left.status
                .is_finished()
                .cmp(&right.status.is_finished())
                .then_with(|| right.updated_at.cmp(&left.updated_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        self.refresh_agent_startup();
        if self
            .selected_task
            .as_ref()
            .is_some_and(|id| !self.tasks.iter().any(|task| &task.id == id))
        {
            self.selected_task = None;
        }

        let heads = tree.doc.get_heads();
        let active = tree.active.raw();
        if heads == self.last_heads && !self.timeline.entries().is_empty() {
            if self.last_active != Some(active) {
                self.apply_scope(tree);
            }
            return;
        }
        self.last_heads = heads;

        // Bind the local automerge actor to the Tilo identity, or local user
        // id; a peer's actor stays unresolved until collaboration says who.
        let actor = tree.doc.get_actor().to_hex_string();
        if !self.source.authors().is_registered(&actor) {
            let principal = if let Some(local) = &self.explicit_local_user {
                Some(local.clone())
            } else {
                #[cfg(test)]
                crate::shell::test_support::ambient_start("history author identity");
                if self.source.authors_mut().register_local_identity(&actor) {
                    None
                } else {
                    let me = crate::shell::state::local_user_id();
                    (!me.is_empty()).then_some(me)
                }
            };
            if let Some(principal) = principal {
                self.source.authors_mut().register(
                    &actor,
                    ui::widget::HistoryAuthor::new(
                        principal,
                        ui::history_source::stable_hue(&actor),
                        ui::history_source::short_id(&actor),
                    ),
                );
            }
        }

        let mut entries = self.source.entries(&mut tree.doc);
        let mut tags = describe_room_changes(&mut tree.doc, &mut entries);
        // A receipt is committed by the room host, but its authenticated
        // `actor` is the person (or agent) who performed the semantic action.
        // Attribute the projected CRDT row to that actor rather than to the
        // host's Automerge key; otherwise every collaborative work session
        // misleadingly appears to have been done by the same machine.
        let receipt_actors = tree
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.effect_receipts.iter())
            .map(|receipt| (receipt.invocation.0.to_string(), receipt.actor.as_str()))
            .collect::<HashMap<_, _>>();
        for entry in &mut entries {
            let Some(actor) = entry
                .context
                .as_ref()
                .and_then(|context| {
                    context
                        .facets
                        .iter()
                        .find(|facet| facet.key == "tilo.invocation")
                })
                .and_then(|facet| receipt_actors.get(facet.value.as_str()))
            else {
                continue;
            };
            entry.author = ui::widget::HistoryAuthor::new(
                participant_display_label(actor),
                ui::history_source::stable_hue(actor),
                ui::history_source::short_id(actor),
            );
        }
        let represented_receipts = entries
            .iter()
            .filter_map(|entry| entry.context.as_ref())
            .flat_map(|context| context.facets.iter())
            .filter(|facet| facet.key == "tilo.invocation")
            .map(|facet| facet.value.clone())
            .collect::<HashSet<_>>();
        for workspace in &tree.workspaces {
            for receipt in &workspace.effect_receipts {
                if represented_receipts.contains(&receipt.invocation.0.to_string()) {
                    continue;
                }
                let actor = receipt.actor.clone();
                entries.push(HistoryEntry {
                    id: HistoryEntryId::from_bytes(*receipt.invocation.0.as_bytes()),
                    author: ui::widget::HistoryAuthor::new(
                        participant_display_label(&actor),
                        ui::history_source::stable_hue(&actor),
                        ui::history_source::short_id(&actor),
                    ),
                    op_class: OpClass::EditMeta,
                    artifact_kind: ArtifactKind::Cluster,
                    timestamp: SystemTime::UNIX_EPOCH
                        .checked_add(Duration::from_secs(
                            u64::try_from(receipt.completed_at).unwrap_or_default(),
                        ))
                        .unwrap_or(SystemTime::UNIX_EPOCH),
                    summary: receipt
                        .context
                        .summary()
                        .unwrap_or_else(|| receipt.status.label().to_string()),
                    text_delta: None,
                    diff_payload: None,
                    redaction: None,
                    context: Some(receipt.context.clone()),
                });
                tags.push(Some(workspace.id.raw()));
            }
        }
        self.derived = entries.into_iter().zip(tags).collect();
        self.derived.sort_by_key(|(entry, _)| entry.timestamp);
        // Names come out of the same document as the changes, so a peer's name
        // arrives with their edits.
        self.named = ui::history_groups::read_named_ranges(&tree.doc, self.source.authors());
        self.apply_scope(tree);
    }

    fn agent_is_live(&self, agent: &str) -> bool {
        self.task_broker
            .as_ref()
            .is_some_and(|broker| broker.agent_is_live_task_capable(&self.task_room, agent))
    }

    fn agent_startup_state(&self, agent: &str) -> Option<&AgentStartup> {
        self.agent_startup
            .get(&(self.task_room.clone(), agent.to_string()))
    }

    fn refresh_agent_startup(&mut self) {
        let room = self.task_room.clone();
        let live_agents = self
            .agent_startup
            .keys()
            .filter(|(launch_room, _)| launch_room == &room)
            .filter_map(|(_, agent)| self.agent_is_live(agent).then_some(agent.clone()))
            .collect::<HashSet<_>>();
        self.agent_startup.retain(|(launch_room, agent), state| {
            if launch_room != &room {
                return true;
            }
            if live_agents.contains(agent) {
                return false;
            }
            if let AgentStartup::Starting { since } = state
                && since.elapsed() >= AGENT_START_TIMEOUT
            {
                *state = AgentStartup::Failed {
                    detail: "The agent did not connect to the workspace. Retry to start a new managed session.".into(),
                };
            }
            true
        });
    }

    /// Consume one correlated managed-agent launcher event. A process
    /// starting is not treated as live; only the broker's subscription makes
    /// the task launchable.
    pub fn observe_agent_launch_event(
        &mut self,
        event: &crate::shell::launcher::LaunchEvent,
    ) -> bool {
        let Some(agent) = event.managed_agent_id.as_deref() else {
            return false;
        };
        let Some(workspace) = event.managed_workspace_id.as_deref() else {
            return false;
        };
        let key = (workspace.to_string(), agent.to_string());
        match &event.kind {
            crate::shell::launcher::LaunchEventKind::Started { .. } => {
                self.agent_startup
                    .entry(key)
                    .or_insert_with(|| AgentStartup::Starting {
                        since: Instant::now(),
                    });
                true
            }
            crate::shell::launcher::LaunchEventKind::SpawnFailed { detail } => {
                self.agent_startup.insert(
                    key,
                    AgentStartup::Failed {
                        detail: detail.clone(),
                    },
                );
                true
            }
            crate::shell::launcher::LaunchEventKind::Failed { code, stderr_tail } => {
                let detail = if stderr_tail.trim().is_empty() {
                    code.map_or_else(
                        || "The managed agent stopped before connecting.".to_string(),
                        |code| format!("The managed agent launcher exited with status {code}."),
                    )
                } else {
                    stderr_tail.clone()
                };
                self.agent_startup
                    .insert(key, AgentStartup::Failed { detail });
                true
            }
            crate::shell::launcher::LaunchEventKind::Exited => false,
        }
    }

    /// Hand the timeline the rows [`Self::scope`] selects and re-derive
    /// everything that depends on which rows are in play.
    fn apply_scope(&mut self, tree: &WorkspaceTree) {
        let active = tree.active.raw();
        self.last_active = Some(active);
        let names: std::collections::HashMap<u64, String> = tree
            .workspaces
            .iter()
            .map(|w| (w.id.raw(), w.name.clone()))
            .collect();

        let mut rows: Vec<HistoryEntry> = Vec::with_capacity(self.derived.len());
        for (entry, tag) in &self.derived {
            match (self.scope, tag) {
                (RoomScope::ThisRoom, Some(id)) if *id != active => {}
                (RoomScope::ThisRoom, _) => rows.push(entry.clone()),
                (RoomScope::AllWorkspaces, tag) => {
                    let mut e = entry.clone();
                    // Otherwise three identical "Set the room owner" lines are noise.
                    if let Some(id) = tag {
                        let label = names
                            .get(id)
                            .cloned()
                            .unwrap_or_else(|| "a removed workspace".to_string());
                        e.summary = format!("{label} \u{00b7} {}", e.summary);
                        let mut context =
                            e.context
                                .take()
                                .unwrap_or_else(|| ui::tilo_actions::ActionContext {
                                    action_label: e.summary.clone(),
                                    ..ui::tilo_actions::ActionContext::default()
                                });
                        context
                            .facets
                            .push(ui::tilo_actions::ActionFacet::new("workspace", label));
                        e.context = Some(context);
                    }
                    rows.push(e);
                }
            }
        }
        self.scoped_tree_rows.clone_from(&rows);
        self.rebuild_replay_frames(tree, &rows, active);
        self.install_rows(rows);
    }

    /// Build each replay checkpoint from the causal document state.
    /// Recent frames are bounded so a room with years of edits still opens
    /// immediately; every retained frame remains an exact Automerge fork.
    fn rebuild_replay_frames(
        &mut self,
        tree: &WorkspaceTree,
        rows: &[HistoryEntry],
        active_workspace: u64,
    ) {
        let mut ordered = rows.to_vec();
        ordered.sort_by_key(|entry| entry.timestamp);
        let skip = ordered.len().saturating_sub(120);
        let mut baseline = tree.clone();
        let retained_pointers: HashMap<HistoryEntryId, Vec<ReplayPointer>> = self
            .replay_frames
            .iter()
            .map(|frame| (frame.entry_id, frame.pointers.clone()))
            .collect();
        let mut frames = Vec::with_capacity(ordered.len().min(120));
        for entry in ordered.into_iter().skip(skip) {
            let Some(heads) = ui::heads_for_entry(&mut baseline.doc, &entry.id) else {
                continue;
            };
            // Automerge 0.8's `fork_at` collector can traverse a valid merged
            // graph in non-topological order and panic on a dependency that
            // appears later. Clone the complete graph, then constrain reads to
            // the historical clock; the replay snapshot is discarded after
            // this frame is materialised.
            let mut doc = baseline.doc.fork();
            doc.isolate(&heads);
            let mut snapshot = baseline.clone();
            snapshot.doc = doc;
            snapshot.resync_cache_from_doc();

            let mut workspaces = Vec::new();
            for workspace in &snapshot.workspaces {
                if self.scope == RoomScope::ThisRoom && workspace.id.raw() != active_workspace {
                    continue;
                }
                let multiple_strips = workspace.strips.len() > 1;
                for strip in &workspace.strips {
                    let name = if multiple_strips {
                        format!("{} · {}", workspace.name, strip.name)
                    } else {
                        workspace.name.clone()
                    };
                    let groups = strip
                        .window_groups
                        .iter()
                        .map(|group| ReplayGroup {
                            tabs: group.tabs.iter().map(|tab| tab.title.clone()).collect(),
                            active: group
                                .tabs
                                .iter()
                                .position(|tab| tab.id == group.active_tab)
                                .unwrap_or(0),
                            width_permille: group.width.permille(),
                        })
                        .collect();
                    workspaces.push(ReplayWorkspace { name, groups });
                }
            }
            frames.push(WorkspaceReplayFrame {
                entry_id: entry.id,
                author: entry.author,
                summary: entry.summary,
                timestamp: entry.timestamp,
                workspaces,
                pointers: retained_pointers
                    .get(&entry.id)
                    .cloned()
                    .unwrap_or_default(),
            });
        }
        self.replay_frames = frames;
        self.replay_index = self
            .replay_index
            .min(self.replay_frames.len().saturating_sub(1));
        if self.replay_frames.is_empty() {
            self.replay_playing = false;
        }
    }

    /// Join the room's shell history with every locally available record in
    /// that room. The projected id map keeps callbacks compact while every
    /// authoritative operation can still resolve the originating document.
    pub fn refresh_records(&mut self, tree: &mut WorkspaceTree, records: &RecordStore) {
        self.refresh_records_inner(tree, records, None);
    }

    /// Production aggregation additionally reads validated, local record
    /// documents which are not mounted in an app's live record store.
    pub fn refresh_records_with_local(
        &mut self,
        tree: &mut WorkspaceTree,
        records: &RecordStore,
        local: &dyn crate::shell::workspace_history::PersistedRecordHistory,
    ) {
        self.refresh_records_inner(tree, records, Some(local));
    }

    #[allow(
        clippy::too_many_lines,
        reason = "history aggregation keeps source refresh, causal frontiers, and digest projection in one ordered pass"
    )]
    fn refresh_records_inner(
        &mut self,
        tree: &mut WorkspaceTree,
        records: &RecordStore,
        local: Option<&dyn crate::shell::workspace_history::PersistedRecordHistory>,
    ) {
        self.history_index
            .set_authors(self.source.authors().clone());
        let query = match self.scope {
            RoomScope::ThisRoom => tree
                .active_workspace()
                .map(|workspace| WorkspaceHistoryQuery::for_room(workspace.room))
                .unwrap_or_default(),
            RoomScope::AllWorkspaces => WorkspaceHistoryQuery::for_rooms(
                tree.workspaces.iter().map(|workspace| workspace.room),
            ),
        };
        let changed = if let Some(local) = local {
            self.history_index
                .refresh_with_workspace_entries_and_local_records(
                    tree,
                    records,
                    local,
                    query,
                    &self.scoped_tree_rows,
                )
        } else {
            self.history_index.refresh_with_workspace_entries(
                tree,
                records,
                query,
                &self.scoped_tree_rows,
            )
        };
        if changed {
            self.feed_coverage = self.history_index.coverage().clone();
            self.projected_origins.clear();
            let record_workspaces: HashMap<String, Vec<String>> =
                tree.workspaces
                    .iter()
                    .fold(HashMap::new(), |mut labels, workspace| {
                        if let Some(records) = tree.room_records_held.get(&workspace.room) {
                            for id in records.keys() {
                                labels
                                    .entry(id.clone())
                                    .or_default()
                                    .push(workspace.name.clone());
                            }
                        }
                        labels
                    });
            let rows: Vec<HistoryEntry> = self
                .history_index
                .entries()
                .iter()
                .map(|row| {
                    let mut projected = row.timeline_entry();
                    if self.scope == RoomScope::AllWorkspaces
                        && let crate::shell::workspace_history::HistoryOrigin::Record { id, .. } =
                            &row.id.origin
                        && let Some(labels) = record_workspaces.get(id.as_str())
                    {
                        let label = labels.join(" + ");
                        projected.summary = format!("{label} \u{00b7} {}", projected.summary);
                        let mut context = projected.context.take().unwrap_or_else(|| {
                            ui::tilo_actions::ActionContext {
                                action_label: projected.summary.clone(),
                                ..ui::tilo_actions::ActionContext::default()
                            }
                        });
                        context
                            .facets
                            .push(ui::tilo_actions::ActionFacet::new("workspace", label));
                        projected.context = Some(context);
                    }
                    self.projected_origins.insert(projected.id, row.id.clone());
                    projected
                })
                .collect();
            self.install_rows(rows);
            self.timeline
                .set_named_ranges(self.history_index.origin_safe_named_ranges(&self.named));
        }
        self.refresh_agent_runs(tree, records, local);

        let workspace = tree.active.raw();
        let passive_frontier = self
            .seen_frontiers
            .get(&workspace)
            .cloned()
            .unwrap_or_default();
        let unseen = |frontier: &WorkspaceSeenFrontier, tree: &mut WorkspaceTree| {
            if let Some(local) = local {
                self.history_index
                    .unseen_with_local_records(frontier, tree, records, local)
            } else {
                self.history_index.unseen(frontier, tree, records)
            }
        };
        let passive_rows: Vec<HistoryEntry> = unseen(&passive_frontier, tree)
            .into_iter()
            .map(WorkspaceHistoryEntry::timeline_entry)
            .collect();
        self.passive_unread = passive_rows.len();
        self.passive_digest = ActivityDigest::from_entries_with_ranges(
            &passive_rows,
            SystemTime::UNIX_EPOCH,
            5,
            &self.named,
        );
        self.away_digest = self
            .departures
            .get(&workspace)
            .cloned()
            .filter(|departure| away_recap_is_due(departure.departed_at_secs, self.now()))
            .map_or_else(ActivityDigest::default, |departure| {
                let rows = unseen(&departure.frontier, tree)
                    .into_iter()
                    .map(WorkspaceHistoryEntry::timeline_entry)
                    .collect::<Vec<_>>();
                ActivityDigest::from_entries_with_ranges(
                    &rows,
                    SystemTime::UNIX_EPOCH,
                    5,
                    &self.named,
                )
            });
        let catchup_rows: Vec<HistoryEntry> = unseen(&self.catchup_frontier, tree)
            .into_iter()
            .map(WorkspaceHistoryEntry::timeline_entry)
            .collect();
        self.digest = ActivityDigest::from_entries_with_ranges(
            &catchup_rows,
            SystemTime::UNIX_EPOCH,
            5,
            &self.named,
        );
        self.reset_narrative();
    }

    fn refresh_agent_runs(
        &mut self,
        tree: &WorkspaceTree,
        records: &RecordStore,
        local: Option<&dyn crate::shell::workspace_history::PersistedRecordHistory>,
    ) {
        self.open_agent_records = records
            .open_records()
            .into_iter()
            .map(|checkout| checkout.id)
            .collect();
        let mut indexed = ui::read_runs(&tree.doc)
            .into_iter()
            .map(|run| IndexedAgentRun {
                source_record: None,
                run,
            })
            .collect::<Vec<_>>();

        let rooms: HashSet<_> = match self.scope {
            RoomScope::ThisRoom => tree
                .active_workspace()
                .map(|workspace| [workspace.room].into_iter().collect())
                .unwrap_or_default(),
            RoomScope::AllWorkspaces => tree
                .workspaces
                .iter()
                .map(|workspace| workspace.room)
                .collect(),
        };
        let record_ids: BTreeSet<RecordId> = rooms
            .into_iter()
            .filter_map(|room| tree.room_records_held.get(&room))
            .flat_map(|held| held.keys())
            .filter_map(|id| RecordId::parse(id))
            .collect();
        for id in record_ids {
            let runs = records
                .read(&id, ui::read_runs)
                .or_else(|| {
                    local.and_then(|source| source.load_record(&id).map(|doc| ui::read_runs(&doc)))
                })
                .unwrap_or_default();
            indexed.extend(runs.into_iter().map(|run| IndexedAgentRun {
                source_record: Some(id.clone()),
                run,
            }));
        }
        indexed.sort_by(|a, b| {
            b.run
                .at
                .cmp(&a.run.at)
                .then_with(|| a.run.id.cmp(&b.run.id))
        });
        indexed.dedup_by(|a, b| a.source_record == b.source_record && a.run.id == b.run.id);
        self.agent_runs = indexed;
    }

    pub fn feed_coverage(&self) -> &FeedCoverage {
        &self.feed_coverage
    }

    fn install_rows(&mut self, mut rows: Vec<HistoryEntry>) {
        let prior_selection = self.selected_ids();
        let digest_rows = rows.clone();
        if let Some(entry) = self.external_entry.clone()
            && !rows.iter().any(|candidate| candidate.id == entry.id)
        {
            rows.push(entry);
        }
        self.total_entries = rows.len();
        self.facet_index = HistoryFacetIndex::from_entries(&rows);
        self.digest = ActivityDigest::from_entries_with_ranges(
            &digest_rows,
            SystemTime::UNIX_EPOCH,
            5,
            &self.named,
        );
        self.passive_unread = digest_rows.len();
        self.passive_digest = self.digest.clone();
        self.reset_narrative();

        let mut seen: HashSet<String> = HashSet::new();
        self.contributors = rows
            .iter()
            .filter(|e| seen.insert(e.author.name.clone()))
            .map(|e| e.author.name.clone())
            .collect();
        let stale_author = self
            .author
            .as_ref()
            .is_some_and(|a| !self.contributors.iter().any(|c| c == a));
        if stale_author {
            self.author = None;
        }

        self.timeline.set_entries(rows);
        if self.selected_ids() != prior_selection {
            self.clear_comparison();
        }
        self.timeline.set_named_ranges(self.named.clone());
        self.reinstall_filter();
    }

    fn reset_narrative(&mut self) {
        let request = NarrativeRequest::from_digest(&self.digest);
        if self.narrative_request == request {
            return;
        }
        self.narrative = request.as_ref().map(ui::deterministic_narrative);
        self.narrative_request = request;
    }

    pub fn narrative_request(&self) -> Option<NarrativeRequest> {
        self.narrative_request.clone()
    }

    /// Install optional model prose only when every cited action belongs to
    /// the current digest. Invalid or stale output leaves the deterministic
    /// summary in place.
    pub fn apply_generated_narrative(&mut self, narrative: ui::GroundedNarrative) -> bool {
        let Some(request) = self.narrative_request.as_ref() else {
            return false;
        };
        let Some(valid) = ui::validate_narrative(request, narrative) else {
            return false;
        };
        self.narrative = Some(valid);
        true
    }

    pub fn set_scope(&mut self, scope: RoomScope, tree: &WorkspaceTree) {
        if self.scope != scope {
            self.scope = scope;
            self.apply_scope(tree);
        }
    }

    pub fn scope(&self) -> RoomScope {
        self.scope
    }

    pub fn authors_mut(&mut self) -> &mut AuthorDirectory {
        self.source.authors_mut()
    }

    // ---------- Geometry ----------

    /// Wide: right-anchored, so the room stays visible behind it. Narrow:
    /// full-bleed — a 360px sheet over a 600px desktop is two unusable halves.
    pub fn layout(&mut self, viewport: Rect) {
        let vw = viewport.width();
        let sheet_w = if vw < FULL_BLEED_UNDER {
            (vw - 2.0 * SHEET_INSET).max(0.0)
        } else if matches!(self.view, ActivityView::Explore | ActivityView::Replay) && vw >= 1_000.0
        {
            (vw - 2.0 * SHEET_INSET).min(1_100.0)
        } else if self.view == ActivityView::Tasks && vw >= 900.0 {
            (vw - 2.0 * SHEET_INSET).min(760.0)
        } else {
            (vw * 0.34).clamp(SHEET_MIN_W, SHEET_MAX_W)
        };
        let x1 = viewport.x1 - SHEET_INSET;
        let x0 = (x1 - sheet_w).max(viewport.x0 + SHEET_INSET.min(vw / 2.0));
        let y0 = viewport.y0 + SHEET_INSET;
        let y1 = viewport.y1 - SHEET_INSET;
        self.sheet = Rect::new(x0, y0, x1, y1.max(y0));

        let rows_y0 = (self.sheet.y0 + self.chrome_height()).min(self.sheet.y1);
        let rows_y1 = (self.sheet.y1 - 8.0).max(rows_y0);
        let three_panel = self.view == ActivityView::Explore && self.sheet.width() >= 900.0;
        let compact_tabs = self.view == ActivityView::Explore && !three_panel;
        self.timeline.set_rect(Rect::new(
            self.sheet.x0 + 8.0 + if three_panel { EXPLORER_LEFT_W } else { 0.0 },
            rows_y0 + if compact_tabs { 34.0 } else { 0.0 },
            (self.sheet.x1 - 8.0 - if three_panel { EXPLORER_DETAIL_W } else { 0.0 })
                .max(self.sheet.x0 + 8.0),
            rows_y1,
        ));
    }

    /// Lay the retained activity surface into an ordinary tiled window.
    /// Window chrome, clipping, sizing and movement remain owned by the
    /// shared `WindowGroup`; this method only consumes its content viewport.
    pub fn layout_window(&mut self, viewport: Rect) {
        let inset = 2.0;
        self.sheet = Rect::new(
            viewport.x0 + inset,
            viewport.y0 + inset,
            (viewport.x1 - inset).max(viewport.x0 + inset),
            (viewport.y1 - inset).max(viewport.y0 + inset),
        );
        let rows_y0 = (self.sheet.y0 + self.chrome_height()).min(self.sheet.y1);
        let rows_y1 = (self.sheet.y1 - 8.0).max(rows_y0);
        let three_panel = self.view == ActivityView::Explore && self.sheet.width() >= 900.0;
        let compact_tabs = self.view == ActivityView::Explore && !three_panel;
        self.timeline.set_rect(Rect::new(
            self.sheet.x0 + 8.0 + if three_panel { EXPLORER_LEFT_W } else { 0.0 },
            rows_y0 + if compact_tabs { 34.0 } else { 0.0 },
            (self.sheet.x1 - 8.0 - if three_panel { EXPLORER_DETAIL_W } else { 0.0 })
                .max(self.sheet.x0 + 8.0),
            rows_y1,
        ));
    }

    fn shows_chip_rows(&self) -> bool {
        self.view == ActivityView::Explore
            && self.sheet.height() > HEADER_H + VIEW_ROW_H + 2.0 * CHIP_ROW_H + 140.0
    }

    fn chrome_height(&self) -> f64 {
        if self.view == ActivityView::CatchUp {
            return HEADER_H;
        }
        if self.shows_chip_rows() {
            HEADER_H + VIEW_ROW_H + 2.0 * CHIP_ROW_H
        } else {
            HEADER_H + VIEW_ROW_H
        }
    }

    fn timeline_is_visible(&self) -> bool {
        self.view == ActivityView::Explore
            && (self.sheet.width() >= 900.0 || self.explorer_pane == ExplorerPane::Timeline)
    }

    // ---------- Paint ----------

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let card = RoundedRect::from_rect(self.sheet, f64::from(tokens.radii.lg));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &card,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &card,
        );

        self.draw_title_and_subtitle(scene, transform, tr, theme);
        if self.view == ActivityView::CatchUp {
            self.scope_rects.clear();
            self.view_rects.clear();
        } else {
            self.draw_scope_chips(scene, transform, tr, theme);
            self.draw_view_tabs(scene, transform, tr, theme);
        }

        self.period_rects.clear();
        self.author_rects.clear();
        if self.shows_chip_rows() {
            self.draw_filter_chip_rows(scene, transform, tr, theme);
        }
        match self.view {
            ActivityView::CatchUp => self.draw_catch_up(scene, transform, tr, theme),
            ActivityView::Tasks => self.draw_task_center(scene, transform, tr, theme),
            ActivityView::Explore => {
                self.draw_explorer_panel_tabs(scene, transform, tr, theme);
                self.draw_explorer_sidebars(scene, transform, tr, theme);
                if self.sheet.width() >= 900.0 || self.explorer_pane == ExplorerPane::Timeline {
                    self.timeline.draw(scene, transform, tr, theme, presence);
                }
            }
            ActivityView::Replay => self.draw_replay(scene, transform, tr, theme),
        }
        self.draw_keyboard_focus(scene, transform, theme);
    }

    /// Presence is intentionally sampled only at durable action boundaries:
    /// pointer motion remains ephemeral and cheap, while the latest exact
    /// positions survive subsequent checkpoint rebuilds for this session.
    pub fn observe_replay_presence(&mut self, presence: &PresenceCtx) {
        let Some(frame) = self.replay_frames.last_mut() else {
            return;
        };
        let mut pointers = Vec::new();
        for (key, people) in &presence.pointers {
            let desktop_space = key.scopes_a_desktop_pointer();
            for (user, point) in people {
                pointers.push(ReplayPointer {
                    name: presence.display_name(user),
                    color: presence.color_for(user),
                    point: *point,
                    desktop_space,
                });
            }
        }
        pointers.sort_by(|a, b| a.name.cmp(&b.name));
        pointers.dedup_by(|a, b| a.name == b.name && a.desktop_space == b.desktop_space);
        if !pointers.is_empty() {
            frame.pointers = pointers;
        }
    }

    fn draw_explorer_panel_tabs(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.explorer_pane_rects.clear();
        if self.sheet.width() >= 900.0 {
            return;
        }
        let y = self.sheet.y0 + self.chrome_height();
        let items: Vec<_> = ExplorerPane::ALL
            .into_iter()
            .map(|pane| (pane, pane.label().to_string()))
            .collect();
        let selected = self.explorer_pane;
        self.explorer_pane_rects = draw_chip_row(
            scene,
            transform,
            tr,
            theme,
            Rect::new(self.sheet.x0 + 12.0, y, self.sheet.x1 - 12.0, y + 30.0),
            &items,
            |pane| *pane == selected,
        );
    }

    /// Paint one unambiguous two-pixel focus ring. It is deliberately static:
    /// focus must remain visible with reduced-motion enabled and must not rely
    /// on a pulse or fade to be noticed.
    fn draw_keyboard_focus(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let Some(rect) = self.focused_rect() else {
            return;
        };
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            theme.tokens.colors.border_focus,
            None,
            &RoundedRect::from_rect(rect.inset(-2.0), 7.0),
        );
    }

    fn draw_explorer_sidebars(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.lens_rects.clear();
        self.lens_more_rect = None;
        self.lens_more_advance = 0;
        self.detail_action_rects.clear();
        let compact = self.sheet.width() < 900.0;
        if compact && self.explorer_pane == ExplorerPane::Timeline {
            return;
        }
        let top = self.sheet.y0 + self.chrome_height() + if compact { 34.0 } else { 0.0 };
        let bottom = self.sheet.y1 - 8.0;
        let content = Rect::new(self.sheet.x0 + 8.0, top, self.sheet.x1 - 8.0, bottom);
        let left = if compact {
            content
        } else {
            Rect::new(
                self.sheet.x0 + 8.0,
                top,
                self.sheet.x0 + EXPLORER_LEFT_W,
                bottom,
            )
        };
        let right = if compact {
            content
        } else {
            Rect::new(
                self.sheet.x1 - EXPLORER_DETAIL_W,
                top,
                self.sheet.x1 - 8.0,
                bottom,
            )
        };
        let show_left = !compact || self.explorer_pane == ExplorerPane::Browse;
        let show_right = !compact || self.explorer_pane == ExplorerPane::Details;
        for rect in [(show_left, left), (show_right, right)]
            .into_iter()
            .filter_map(|(show, rect)| show.then_some(rect))
        {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.lift(0.35),
                None,
                &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.md)),
            );
        }
        if show_left {
            self.draw_explorer_lenses(scene, transform, tr, theme, left);
        }
        if show_right {
            self.draw_explorer_details(scene, transform, tr, theme, right);
        }
    }

    fn draw_explorer_lenses(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        bounds: Rect,
    ) {
        tr.draw_text_weighted(
            scene,
            "Browse by",
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_primary,
            at(transform, bounds.x0 + 12.0, bounds.y0 + 12.0),
            None,
            ui::text_render::TextWeight::Medium,
        );
        let mut y = bounds.y0 + 38.0;
        let candidates = self.lens_candidates();
        if self.lens_offset >= candidates.len() {
            self.lens_offset = 0;
        }
        let capacity = complete_rows(bounds.y1 - 6.0 - y, 32.0).max(1);
        let remaining = candidates.len().saturating_sub(self.lens_offset);
        let shown = remaining.min(if remaining > capacity {
            capacity.saturating_sub(1)
        } else {
            capacity
        });
        for (lens, label) in candidates.iter().skip(self.lens_offset).take(shown) {
            self.draw_lens_row(scene, transform, tr, theme, lens, label, &mut y, bounds);
        }
        if shown < remaining || self.lens_offset > 0 {
            let label = if shown < remaining {
                "More lenses…"
            } else {
                "First lenses…"
            };
            self.lens_more_rect = draw_more_row(scene, transform, tr, theme, bounds, y, label);
            self.lens_more_advance = shown.max(1);
        }
    }

    fn draw_explorer_details(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        bounds: Rect,
    ) {
        let size = theme.tokens.typography.scale.sm;
        tr.draw_text_weighted(
            scene,
            "Provenance",
            size,
            theme.tokens.colors.text_primary,
            at(transform, bounds.x0 + 12.0, bounds.y0 + 12.0),
            None,
            ui::text_render::TextWeight::Medium,
        );
        if let Some(detail) = self.selection_detail() {
            let width = text_width(bounds.width() - 24.0);
            if let Some(indexed) = self.selected_agent_run().cloned() {
                self.draw_agent_task_detail(scene, transform, tr, theme, bounds, width, &indexed);
                return;
            }
            tr.draw_text(
                scene,
                &detail.title,
                theme.tokens.typography.scale.base,
                theme.tokens.colors.text_primary,
                at(transform, bounds.x0 + 12.0, bounds.y0 + 40.0),
                Some(width),
            );
            tr.draw_text(
                scene,
                &detail_provenance(&detail),
                size,
                theme.tokens.colors.text_dim,
                at(transform, bounds.x0 + 12.0, bounds.y0 + 88.0),
                Some(width),
            );
            let actions_bottom = self.draw_detail_actions(
                scene,
                transform,
                tr,
                theme,
                bounds,
                &detail,
                bounds.y0 + 170.0,
            );
            self.draw_detail_comparison(
                scene,
                transform,
                tr,
                theme,
                bounds,
                width,
                actions_bottom + 14.0,
            );
        } else {
            let width = text_width(bounds.width() - 24.0);
            tr.draw_text(
                scene,
                "Select a work session or a range of actions",
                theme.tokens.typography.scale.base,
                theme.tokens.colors.text_primary,
                at(transform, bounds.x0 + 12.0, bounds.y0 + 44.0),
                Some(width),
            );
            tr.draw_text(
                scene,
                "See who was involved, affected resources, and an exact before-and-after comparison.",
                size,
                theme.tokens.colors.text_dim,
                at(transform, bounds.x0 + 12.0, bounds.y0 + 88.0),
                Some(width),
            );
        }
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "one immediate-mode detail renderer keeps the agent task narrative and actions spatially coherent"
    )]
    fn draw_agent_task_detail(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        bounds: Rect,
        width: f32,
        indexed: &IndexedAgentRun,
    ) {
        let run = &indexed.run;
        let size = theme.tokens.typography.scale.sm;
        let x = bounds.x0 + 12.0;
        let mut y = bounds.y0 + 40.0;
        tr.draw_text_weighted(
            scene,
            "Agent task",
            theme.tokens.typography.scale.base,
            theme.tokens.colors.accent[4],
            at(transform, x, y),
            None,
            ui::text_render::TextWeight::Medium,
        );
        y += 26.0;
        let instruction = run.instruction.as_deref().unwrap_or(&run.label);
        tr.draw_text(
            scene,
            instruction,
            theme.tokens.typography.scale.base,
            theme.tokens.colors.text_primary,
            at(transform, x, y),
            Some(width),
        );
        y += 52.0;
        tr.draw_text(
            scene,
            &format!(
                "Assigned by {}  ·  Run by {}",
                self.issuer_name(run),
                self.agent_name(run)
            ),
            size,
            theme.tokens.colors.text_dim,
            at(transform, x, y),
            Some(width),
        );
        y += 38.0;
        tr.draw_text_weighted(
            scene,
            "Why this work",
            size,
            theme.tokens.colors.text_primary,
            at(transform, x, y),
            None,
            ui::text_render::TextWeight::Medium,
        );
        y += 20.0;
        tr.draw_text(
            scene,
            run.decision
                .as_deref()
                .unwrap_or("No decision route was recorded."),
            size,
            theme.tokens.colors.text_dim,
            at(transform, x, y),
            Some(width),
        );
        y += 48.0;
        tr.draw_text_weighted(
            scene,
            "Affected records",
            size,
            theme.tokens.colors.text_primary,
            at(transform, x, y),
            None,
            ui::text_render::TextWeight::Medium,
        );
        y += 22.0;
        for step in &run.steps {
            let app = crate::shell::launcher::display_name_for_app_id(&step.app)
                .unwrap_or_else(|| humanize_app_id(&step.app));
            let record = RecordId::parse(&step.record)
                .and_then(|id| id.file_path())
                .and_then(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| step.record.clone());
            tr.draw_text_weighted(
                scene,
                &format!("{app}  ·  {record}"),
                size,
                theme.tokens.colors.text_primary,
                at(transform, x, y),
                Some(width),
                ui::text_render::TextWeight::Medium,
            );
            y += 18.0;
            tr.draw_text(
                scene,
                &step.summary,
                size,
                theme.tokens.colors.text_dim,
                at(transform, x + 10.0, y),
                Some((width - 10.0).max(0.0)),
            );
            y += 28.0;
        }
        if !self.can_undo_agent_run() {
            tr.draw_text(
                scene,
                "Open every affected document to undo this task together.",
                size,
                theme.tokens.colors.text_dim,
                at(transform, x, y),
                Some(width),
            );
            y += 38.0;
        }
        if let Some((id, notice)) = &self.agent_undo_notice
            && id == &run.id
        {
            tr.draw_text(
                scene,
                notice,
                size,
                theme.tokens.colors.text_dim,
                at(transform, x, y),
                Some(width),
            );
            y += 38.0;
        }
        if let Some(detail) = self.selection_detail() {
            let _ = self.draw_detail_actions(scene, transform, tr, theme, bounds, &detail, y);
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "immediate-mode action row needs shared scene, typography, theme, bounds, and selection context"
    )]
    fn draw_detail_actions(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        bounds: Rect,
        detail: &HistorySelectionDetail,
        start_y: f64,
    ) -> f64 {
        let mut x = bounds.x0 + 12.0;
        let mut y = start_y;
        let compared = self.comparison_attempted;
        for action in [
            DetailAction::Open,
            DetailAction::Compare,
            DetailAction::Undo,
            DetailAction::UndoTask,
            DetailAction::Split,
            DetailAction::Merge,
        ] {
            let enabled = match action {
                DetailAction::Open => !detail.resources.is_empty(),
                DetailAction::Compare => detail.can_compare,
                DetailAction::Undo => detail.can_undo && self.selected_agent_run().is_none(),
                DetailAction::UndoTask => self.can_undo_agent_run(),
                DetailAction::Split => self.split_candidate().is_some(),
                DetailAction::Merge => self.merge_candidate().is_some(),
            };
            if !enabled {
                continue;
            }
            let label = self.detail_action_label(action);
            let button_width = f64::from(
                (tr.measure(label, theme.tokens.typography.scale.sm, None).0 + 20.0).max(60.0),
            );
            if x + button_width > bounds.x1 - 12.0 {
                x = bounds.x0 + 12.0;
                y += 34.0;
            }
            let rect = Rect::new(x, y, x + button_width, y + 28.0);
            let is_active = action == DetailAction::Compare && compared;
            let shape = RoundedRect::from_rect(rect, 6.0);
            scene.fill(
                Fill::NonZero,
                transform,
                if is_active {
                    theme.tokens.colors.accent[5].with_alpha_factor(0.28)
                } else {
                    theme.tokens.colors.lift(0.35)
                },
                None,
                &shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                if is_active {
                    theme.tokens.colors.accent[4]
                } else {
                    theme.tokens.colors.border_standard
                },
                None,
                &shape,
            );
            tr.draw_text(
                scene,
                label,
                theme.tokens.typography.scale.sm,
                if is_active {
                    theme.tokens.colors.accent[4]
                } else {
                    theme.tokens.colors.text_primary
                },
                at(transform, rect.x0 + 8.0, rect.y0 + 7.0),
                None,
            );
            self.detail_action_rects.push((action, rect));
            x += button_width + 6.0;
        }
        y + 28.0
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        clippy::cast_precision_loss,
        reason = "comparison paint, bounds, and text layout stay in one retained-rendering pass so labels, hit geometry, and accessible summary cannot diverge"
    )]
    fn draw_detail_comparison(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        bounds: Rect,
        width: f32,
        y: f64,
    ) {
        if !self.comparison_attempted {
            return;
        }
        if y + 70.0 > bounds.y1 {
            return;
        }
        let Some(comparison) = &self.comparison else {
            Self::draw_unavailable_comparison(scene, transform, tr, theme, bounds, y);
            return;
        };
        let labels = comparison_change_labels(&comparison.patches);
        let visible = labels.len().min(4);
        let overflow_height = if labels.len() > visible { 18.0 } else { 0.0 };
        let card_height = 68.0 + 20.0 * visible as f64 + overflow_height;
        let card = Rect::new(
            bounds.x0 + 12.0,
            y,
            bounds.x1 - 12.0,
            (y + card_height).min(bounds.y1 - 8.0),
        );
        let shape = RoundedRect::from_rect(card, 8.0);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.accent[5].with_alpha_factor(0.12),
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.accent[5].with_alpha_factor(0.55),
            None,
            &shape,
        );
        tr.draw_text_weighted(
            scene,
            "What changed",
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_primary,
            at(transform, card.x0 + 12.0, card.y0 + 10.0),
            None,
            ui::text_render::TextWeight::Medium,
        );
        let action_word = if comparison.entries.len() == 1 {
            "action"
        } else {
            "actions"
        };
        let change_word = if comparison.patches.len() == 1 {
            "document change"
        } else {
            "document changes"
        };
        tr.draw_text(
            scene,
            &format!(
                "{} {action_word} · {} {change_word}",
                comparison.entries.len(),
                comparison.patches.len()
            ),
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_dim,
            at(transform, card.x0 + 12.0, card.y0 + 32.0),
            Some(width - 24.0),
        );
        if labels.is_empty() {
            tr.draw_text(
                scene,
                "No content differences in this selection",
                theme.tokens.typography.scale.sm,
                theme.tokens.colors.text_dim,
                at(transform, card.x0 + 12.0, card.y0 + 52.0),
                Some(width - 24.0),
            );
            return;
        }
        for (index, label) in labels.iter().take(visible).enumerate() {
            tr.draw_text(
                scene,
                &format!("• {label}"),
                theme.tokens.typography.scale.sm,
                theme.tokens.colors.text_primary,
                at(
                    transform,
                    card.x0 + 12.0,
                    card.y0 + 54.0 + index as f64 * 20.0,
                ),
                Some(width - 24.0),
            );
        }
        if labels.len() > visible {
            tr.draw_text(
                scene,
                &format!("+{} more", labels.len().saturating_sub(visible)),
                theme.tokens.typography.scale.sm,
                theme.tokens.colors.text_dim,
                at(
                    transform,
                    card.x0 + 22.0,
                    card.y0 + 54.0 + visible as f64 * 20.0,
                ),
                Some(width - 34.0),
            );
        }
    }

    fn draw_unavailable_comparison(
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        bounds: Rect,
        y: f64,
    ) {
        let card = Rect::new(bounds.x0 + 12.0, y, bounds.x1 - 12.0, y + 92.0);
        let shape = RoundedRect::from_rect(card, 8.0);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.lift(0.35),
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &shape,
        );
        tr.draw_text_weighted(
            scene,
            "Preview unavailable",
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_primary,
            at(transform, card.x0 + 12.0, card.y0 + 10.0),
            None,
            ui::text_render::TextWeight::Medium,
        );
        tr.draw_text(
            scene,
            "This action has provenance, but no materialized before-and-after snapshot.",
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_dim,
            at(transform, card.x0 + 12.0, card.y0 + 36.0),
            Some(text_width(card.width() - 24.0)),
        );
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "small immediate-mode explorer row"
    )]
    fn draw_lens_row(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        lens: &LensSelection,
        label: &str,
        y: &mut f64,
        bounds: Rect,
    ) {
        if *y + 28.0 > bounds.y1 - 6.0 {
            return;
        }
        let rect = Rect::new(bounds.x0 + 6.0, *y, bounds.x1 - 6.0, *y + 28.0);
        if self.lens == *lens {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.accent[5],
                None,
                &RoundedRect::from_rect(rect, 6.0),
            );
        }
        tr.draw_text(
            scene,
            label,
            theme.tokens.typography.scale.sm,
            if self.lens == *lens {
                theme.tokens.colors.text_primary
            } else {
                theme.tokens.colors.text_dim
            },
            at(transform, rect.x0 + 8.0, rect.y0 + 6.0),
            Some(text_width(rect.width() - 16.0)),
        );
        self.lens_rects.push((lens.clone(), rect));
        *y += 32.0;
    }

    fn lens_candidates(&self) -> Vec<(LensSelection, String)> {
        let mut rows = vec![(LensSelection::All, "Everything".to_string())];
        if !self.agent_runs.is_empty() {
            rows.push((
                LensSelection::AgentTasks,
                format!("Agent tasks  {}", self.agent_runs.len()),
            ));
        }
        rows.extend(self.facet_index.apps.iter().map(|(label, count)| {
            (
                LensSelection::App(label.clone()),
                format!("{label}  {count}"),
            )
        }));
        rows.extend(
            self.facet_index
                .resource_kinds
                .iter()
                .map(|(label, count)| {
                    (
                        LensSelection::ResourceKind(label.clone()),
                        format!("{label}  {count}"),
                    )
                }),
        );
        rows.extend(self.facet_index.resources.iter().map(|(label, count)| {
            (
                LensSelection::Resource(label.clone()),
                format!("{label}  {count}"),
            )
        }));
        rows.extend(self.facet_index.work_groups.iter().map(|(label, count)| {
            (
                LensSelection::WorkGroup(label.clone()),
                format!("{label}  {count}"),
            )
        }));
        rows.extend(self.facet_index.facets.iter().flat_map(|(key, values)| {
            values.iter().map(move |(value, count)| {
                (
                    LensSelection::Facet(key.clone(), value.clone()),
                    format!("{key}: {value}  {count}"),
                )
            })
        }));
        if self.facet_index.unclassified > 0 {
            rows.push((
                LensSelection::Unclassified,
                format!("Unclassified  {}", self.facet_index.unclassified),
            ));
        }
        rows
    }

    fn draw_view_tabs(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let y = self.sheet.y0 + HEADER_H + 4.0;
        let items: Vec<(ActivityView, String)> = ActivityView::ALL
            .iter()
            .map(|view| (*view, view.label().to_string()))
            .collect();
        let selected = self.view;
        self.view_rects = draw_chip_row(
            scene,
            transform,
            tr,
            theme,
            Rect::new(self.sheet.x0 + 12.0, y, self.sheet.x1 - 12.0, y + 26.0),
            &items,
            |view| *view == selected,
        );
    }

    #[allow(
        clippy::too_many_lines,
        clippy::cast_precision_loss,
        reason = "catch-up cards share one ordered retained-rendering pass; count conversions only affect bounded display geometry"
    )]
    fn draw_catch_up(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.brief_rects.clear();
        self.boundary_rects.clear();
        self.brief_more_rect = None;
        self.brief_more_advance = 0;
        self.resume_rect = Rect::ZERO;
        self.resume_target = None;
        let mut y = self.sheet.y0 + self.chrome_height() + 8.0;
        let x0 = self.sheet.x0 + 12.0;
        let x1 = self.sheet.x1 - 12.0;
        if let Some(resume) = local_resume_snapshot(self.timeline.entries()) {
            let rect = Rect::new(x0, y, x1, y + 66.0);
            draw_resume_card(scene, transform, tr, theme, rect, &resume, self.now());
            self.resume_rect = rect;
            self.resume_target = Some(resume.target);
            y += 74.0;
        }
        if self.digest.briefs.is_empty() {
            tr.draw_text(
                scene,
                "You're caught up",
                theme.tokens.typography.scale.base,
                theme.tokens.colors.text_dim,
                at(transform, x0 + 4.0, y + 12.0),
                Some(text_width(x1 - x0 - 8.0)),
            );
            return;
        }
        if let Some(narrative) = self.narrative.as_ref() {
            let generated = narrative.generator != "tilo-deterministic";
            tr.draw_text(
                scene,
                if generated {
                    "THE SHORT VERSION · ON THIS DEVICE"
                } else {
                    "THE SHORT VERSION"
                },
                (theme.tokens.typography.scale.sm - 1.0).max(10.0),
                theme.tokens.colors.accent[4],
                at(transform, x0 + 4.0, y + 2.0),
                Some(text_width(x1 - x0 - 8.0)),
            );
            let lines = tr.clamp_to_lines(
                &narrative.text,
                theme.tokens.typography.scale.base,
                text_width(x1 - x0 - 8.0),
                2,
            );
            for (line, text) in lines.iter().enumerate() {
                tr.draw_text_weighted(
                    scene,
                    text,
                    theme.tokens.typography.scale.base,
                    theme.tokens.colors.text_primary,
                    at(transform, x0 + 4.0, y + 24.0 + line as f64 * 20.0),
                    Some(text_width(x1 - x0 - 8.0)),
                    ui::text_render::TextWeight::Medium,
                );
            }
            y += 76.0;
        }
        if self.brief_offset >= self.digest.briefs.len() {
            self.brief_offset = 0;
        }
        let mut shown = 0usize;
        for (index, brief) in self
            .digest
            .briefs
            .iter()
            .enumerate()
            .skip(self.brief_offset)
        {
            let expansion = if self.expanded_brief == Some(index) {
                self.brief_expansion_started
                    .filter(|(animated, _)| *animated == index)
                    .map_or(1.0, |(_, started)| {
                        let linear = Instant::now()
                            .saturating_duration_since(started)
                            .as_secs_f64()
                            / BRIEF_EXPANSION_DURATION.as_secs_f64();
                        let linear = linear.clamp(0.0, 1.0);
                        1.0 - (1.0 - linear).powi(3)
                    })
            } else {
                0.0
            };
            let expanded = self.expanded_brief == Some(index);
            // Outcome, who/when, and affected context must all be scannable
            // without opening the card.
            let title_lines = tr.clamp_to_lines(
                &brief.title,
                theme.tokens.typography.scale.base,
                text_width(x1 - x0 - 24.0),
                2,
            );
            let title_extra = 18.0 * title_lines.len().saturating_sub(1) as f64;
            let expanded_height = 138.0 + 24.0 * brief.actions.len() as f64;
            let more_room = 34.0;
            let has_later = index
                .checked_add(1)
                .is_some_and(|next| next < self.digest.briefs.len())
                || self.brief_offset > 0;
            let available_height =
                (self.sheet.y1 - 10.0 - y - if has_later { more_room } else { 0.0 })
                    .max(112.0 + title_extra);
            let height =
                (112.0 + (expanded_height - 112.0) * expansion + title_extra).min(available_height);
            if y + height + if has_later { more_room } else { 0.0 } > self.sheet.y1 - 10.0 {
                break;
            }
            let rect = Rect::new(x0, y, x1, y + height);
            self.brief_rects.push((index, rect));
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &rect,
            );
            draw_brief_card(
                scene,
                transform,
                tr,
                theme,
                brief,
                rect,
                expanded,
                self.now(),
            );
            scene.pop_layer();
            y += height + 8.0;
            shown = shown.saturating_add(1);
        }
        let remaining = self
            .digest
            .briefs
            .len()
            .saturating_sub(self.brief_offset.saturating_add(shown));
        if remaining > 0 || self.brief_offset > 0 {
            let label = if remaining > 0 {
                "More work sessions…"
            } else {
                "First work sessions…"
            };
            self.brief_more_rect = draw_more_row(
                scene,
                transform,
                tr,
                theme,
                Rect::new(x0, y, x1, self.sheet.y1 - 8.0),
                y,
                label,
            );
            self.brief_more_advance = shown.max(1);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "replay controls, current-frame geometry, and focus targets must be painted together for deterministic playback"
    )]
    fn draw_replay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.replay_back_rect = Rect::ZERO;
        self.replay_play_rect = Rect::ZERO;
        self.replay_forward_rect = Rect::ZERO;
        if self.replay_frames.is_empty() {
            tr.draw_text(
                scene,
                "No replayable workspace checkpoints yet",
                theme.tokens.typography.scale.base,
                theme.tokens.colors.text_primary,
                at(
                    transform,
                    self.sheet.x0 + 16.0,
                    self.sheet.y0 + self.chrome_height() + 24.0,
                ),
                Some(text_width(self.sheet.width() - 32.0)),
            );
            return;
        }
        if self.replay_playing && self.replay_last_advance.elapsed() >= Duration::from_millis(850) {
            if self.replay_index.saturating_add(1) < self.replay_frames.len() {
                self.replay_index = self.replay_index.saturating_add(1);
                self.replay_last_advance = Instant::now();
            } else {
                self.replay_playing = false;
            }
        }
        self.replay_index = self
            .replay_index
            .min(self.replay_frames.len().saturating_sub(1));
        let Some(frame) = self.replay_frames.get(self.replay_index) else {
            return;
        };
        let top = self.sheet.y0 + self.chrome_height() + 10.0;
        let controls = Rect::new(self.sheet.x0 + 12.0, top, self.sheet.x1 - 12.0, top + 34.0);
        self.replay_back_rect =
            Rect::new(controls.x0, controls.y0, controls.x0 + 36.0, controls.y1);
        self.replay_play_rect = Rect::new(
            self.replay_back_rect.x1 + 6.0,
            controls.y0,
            self.replay_back_rect.x1 + 82.0,
            controls.y1,
        );
        self.replay_forward_rect = Rect::new(
            self.replay_play_rect.x1 + 6.0,
            controls.y0,
            self.replay_play_rect.x1 + 42.0,
            controls.y1,
        );
        for (rect, label, active) in [
            (self.replay_back_rect, "←", false),
            (
                self.replay_play_rect,
                if self.replay_playing { "Pause" } else { "Play" },
                self.replay_playing,
            ),
            (self.replay_forward_rect, "→", false),
        ] {
            let shape = RoundedRect::from_rect(rect, 7.0);
            scene.fill(
                Fill::NonZero,
                transform,
                if active {
                    theme.tokens.colors.accent[5].with_alpha_factor(0.28)
                } else {
                    theme.tokens.colors.lift(0.35)
                },
                None,
                &shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                if active {
                    theme.tokens.colors.accent[4]
                } else {
                    theme.tokens.colors.border_standard
                },
                None,
                &shape,
            );
            let (tw, th) = tr.measure(label, theme.tokens.typography.scale.sm, None);
            tr.draw_text(
                scene,
                label,
                theme.tokens.typography.scale.sm,
                theme.tokens.colors.text_primary,
                at(
                    transform,
                    rect.center().x - f64::from(tw) / 2.0,
                    rect.center().y - f64::from(th) / 2.0,
                ),
                None,
            );
        }
        tr.draw_text(
            scene,
            &format!(
                "Checkpoint {} of {} · {}",
                self.replay_index.saturating_add(1),
                self.replay_frames.len(),
                format_relative_time(frame.timestamp, self.now())
            ),
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_dim,
            at(
                transform,
                self.replay_forward_rect.x1 + 12.0,
                controls.y0 + 9.0,
            ),
            Some(text_width(controls.x1 - self.replay_forward_rect.x1 - 12.0)),
        );

        let info_h = 94.0;
        let canvas = Rect::new(
            self.sheet.x0 + 12.0,
            controls.y1 + 10.0,
            self.sheet.x1 - 12.0,
            (self.sheet.y1 - info_h - 20.0).max(controls.y1 + 80.0),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &RoundedRect::from_rect(canvas, 10.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &RoundedRect::from_rect(canvas, 10.0),
        );
        draw_replay_layout(scene, transform, tr, theme, canvas, frame);
        draw_replay_pointers(scene, transform, tr, theme, canvas, frame, self.sheet);

        let info = Rect::new(canvas.x0, canvas.y1 + 8.0, canvas.x1, self.sheet.y1 - 10.0);
        let (r, g, b) = ui::widget::history::hue_to_rgb(frame.author.avatar_hue);
        scene.fill(
            Fill::NonZero,
            transform,
            vello::peniko::Color::from_rgba8(r, g, b, 255),
            None,
            &vello::kurbo::Circle::new(Point::new(info.x0 + 9.0, info.y0 + 11.0), 5.0),
        );
        tr.draw_text_weighted(
            scene,
            &format!("{} · {}", frame.author.name, frame.summary),
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_primary,
            at(transform, info.x0 + 20.0, info.y0 + 4.0),
            Some(text_width(info.width() - 24.0)),
            ui::text_render::TextWeight::Medium,
        );
        draw_presence_trail(
            scene,
            transform,
            tr,
            theme,
            Rect::new(info.x0, info.y0 + 34.0, info.x1, info.y1),
            &self.replay_frames,
            self.replay_index,
        );
    }

    #[allow(
        clippy::too_many_lines,
        clippy::cast_precision_loss,
        reason = "task-center rows are one retained rendering and accessibility pass; count conversions only affect bounded display geometry"
    )]
    fn draw_task_center(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.task_rects.clear();
        self.task_action_rects.clear();
        self.task_route_rects.clear();
        self.task_create_rect = None;
        self.task_cancel_rect = None;

        let x0 = self.sheet.x0 + 12.0;
        let x1 = self.sheet.x1 - 12.0;
        let mut y = self.sheet.y0 + self.chrome_height() + 8.0;
        let new_task = Rect::new(x1 - 116.0, y, x1, y + 30.0);
        self.new_task_rect = Some(new_task);
        draw_task_button(scene, transform, tr, theme, new_task, "+ New task", true);
        tr.draw_text_weighted(
            scene,
            "WORKSPACE QUEUE",
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_dim,
            at(transform, x0 + 4.0, y + 8.0),
            None,
            ui::text_render::TextWeight::Medium,
        );
        y += 40.0;

        if self.task_compose_open {
            let composer = Rect::new(x0, y, x1, y + 164.0);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.lift(0.45),
                None,
                &RoundedRect::from_rect(composer, f64::from(theme.tokens.radii.md)),
            );
            tr.draw_text_weighted(
                scene,
                "Create work without leaving the desktop",
                theme.tokens.typography.scale.base,
                theme.tokens.colors.text_primary,
                at(transform, composer.x0 + 12.0, composer.y0 + 10.0),
                None,
                ui::text_render::TextWeight::Medium,
            );
            let field = Rect::new(
                composer.x0 + 12.0,
                composer.y0 + 38.0,
                composer.x1 - 12.0,
                composer.y0 + 70.0,
            );
            scene.stroke(
                &Stroke::new(1.5),
                transform,
                theme.tokens.colors.border_focus,
                None,
                &RoundedRect::from_rect(field, 6.0),
            );
            tr.draw_text(
                scene,
                if self.task_draft.is_empty() {
                    "What needs doing?"
                } else {
                    &self.task_draft
                },
                theme.tokens.typography.scale.base,
                if self.task_draft.is_empty() {
                    theme.tokens.colors.text_dim
                } else {
                    theme.tokens.colors.text_primary
                },
                at(transform, field.x0 + 10.0, field.y0 + 7.0),
                Some(text_width(field.width() - 20.0)),
            );
            let routes = [
                WorkspaceTaskRoute::Manual,
                WorkspaceTaskRoute::RulesOnly,
                WorkspaceTaskRoute::Model,
            ]
            .into_iter()
            .map(|route| (route, route.label().to_string()))
            .collect::<Vec<_>>();
            self.task_route_rects = draw_chip_row(
                scene,
                transform,
                tr,
                theme,
                Rect::new(
                    composer.x0 + 8.0,
                    composer.y0 + 78.0,
                    composer.x1 - 8.0,
                    composer.y0 + 106.0,
                ),
                &routes,
                |route| *route == self.task_route,
            );
            let executor = match self.task_route {
                WorkspaceTaskRoute::Manual => {
                    "Executor · You or another collaborator · no model".to_string()
                }
                WorkspaceTaskRoute::RulesOnly | WorkspaceTaskRoute::Model => {
                    self.task_agent.as_ref().map_or_else(
                        || "Executor · No agent available · task stays unassigned".to_string(),
                        |agent| format!("Executor · ▣ AGENT · {agent}"),
                    )
                }
            };
            let executor = tr.elide_to_width(
                &executor,
                theme.tokens.typography.scale.sm,
                text_width(composer.width() - 24.0),
            );
            tr.draw_text(
                scene,
                &executor,
                theme.tokens.typography.scale.sm,
                theme.tokens.colors.text_primary,
                at(transform, composer.x0 + 12.0, composer.y0 + 107.0),
                None,
            );
            let attached = self.task_attachment.as_ref().map_or_else(
                || "No resource attached · task stays workspace-wide".to_string(),
                |resource| {
                    format!(
                        "Attached · {} · {}",
                        resource.app_label.as_deref().unwrap_or("Workspace"),
                        resource.label
                    )
                },
            );
            let attached = tr.elide_to_width(
                &attached,
                theme.tokens.typography.scale.sm,
                text_width(composer.width() - 24.0),
            );
            tr.draw_text(
                scene,
                &attached,
                theme.tokens.typography.scale.sm,
                theme.tokens.colors.text_dim,
                at(transform, composer.x0 + 12.0, composer.y0 + 126.0),
                None,
            );
            let cancel = Rect::new(
                composer.x1 - 190.0,
                composer.y1 - 32.0,
                composer.x1 - 104.0,
                composer.y1 - 6.0,
            );
            let create = Rect::new(
                composer.x1 - 98.0,
                composer.y1 - 32.0,
                composer.x1 - 8.0,
                composer.y1 - 6.0,
            );
            self.task_cancel_rect = Some(cancel);
            self.task_create_rect = Some(create);
            draw_task_button(scene, transform, tr, theme, cancel, "Cancel", false);
            draw_task_button(
                scene,
                transform,
                tr,
                theme,
                create,
                "Create",
                !self.task_draft.trim().is_empty(),
            );
            y = composer.y1 + 8.0;
        }

        if self.tasks.is_empty() {
            tr.draw_text_weighted(
                scene,
                "No tasks yet",
                theme.tokens.typography.scale.base,
                theme.tokens.colors.text_primary,
                at(transform, x0 + 4.0, y + 16.0),
                None,
                ui::text_render::TextWeight::Medium,
            );
            tr.draw_text(
                scene,
                "Create one manually, or attach the resource selected in Explore.",
                theme.tokens.typography.scale.sm,
                theme.tokens.colors.text_dim,
                at(transform, x0 + 4.0, y + 40.0),
                Some(text_width(x1 - x0 - 8.0)),
            );
            return;
        }

        for task in &self.tasks {
            let selected = self.selected_task.as_deref() == Some(task.id.as_str());
            let audit_rows = if selected { task.audit.len().min(3) } else { 0 };
            let execution_notice = selected.then(|| self.task_execution_notice(task)).flatten();
            let height = 82.0
                + if selected {
                    72.0 + 18.0 * audit_rows as f64
                        + if execution_notice.is_some() {
                            18.0
                        } else {
                            0.0
                        }
                } else {
                    0.0
                };
            if y + height > self.sheet.y1 - 8.0 {
                break;
            }
            let rect = Rect::new(x0, y, x1, y + height);
            self.task_rects.push((task.id.clone(), rect));
            draw_task_card(
                scene,
                transform,
                tr,
                theme,
                task,
                rect,
                selected,
                self.now(),
                execution_notice.as_deref(),
            );
            if selected {
                let actions = self.task_actions_for(task);
                let items = actions
                    .iter()
                    .map(|action| (*action, self.task_action_label(*action, task)))
                    .collect::<Vec<_>>();
                self.task_action_rects = draw_chip_row(
                    scene,
                    transform,
                    tr,
                    theme,
                    Rect::new(rect.x0 + 8.0, rect.y1 - 32.0, rect.x1 - 8.0, rect.y1 - 4.0),
                    &items,
                    |_| false,
                );
            }
            y += height + 8.0;
        }
    }

    /// Paint the sheet's title, factual scope/count, and latest outcome.
    #[allow(
        clippy::too_many_lines,
        reason = "the title, factual scope, outcome, and matching accessible summary are one retained header composition"
    )]
    fn draw_title_and_subtitle(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let title_size = if self.view == ActivityView::CatchUp {
            tokens.typography.scale.xl
        } else {
            tokens.typography.scale.lg
        };
        let title = match self.view {
            ActivityView::CatchUp => "While you were away",
            ActivityView::Tasks => "Task Center",
            ActivityView::Explore => "Workspace activity",
            ActivityView::Replay => "Replay workspace",
        };
        tr.draw_text(
            scene,
            title,
            title_size,
            colors.text_primary,
            at(transform, self.sheet.x0 + 16.0, self.sheet.y0 + 12.0),
            None,
        );
        let sub_size = (tokens.typography.scale.sm - 1.0).max(10.0);
        let (change_count, contributor_count) = match self.view {
            ActivityView::CatchUp => (self.digest.total_changes, self.digest.people),
            ActivityView::Tasks => (
                self.tasks
                    .iter()
                    .filter(|task| !task.status.is_finished())
                    .count(),
                self.tasks
                    .iter()
                    .filter_map(|task| task.assigned_agent.as_ref())
                    .collect::<HashSet<_>>()
                    .len(),
            ),
            ActivityView::Explore | ActivityView::Replay => {
                (self.total_entries, self.contributors.len())
            }
        };
        if self.view == ActivityView::Tasks {
            let active = change_count;
            let task_noun = if active == 1 { "task" } else { "tasks" };
            let agent_noun = if contributor_count == 1 {
                "agent"
            } else {
                "agents"
            };
            let subtitle = format!(
                "{} · {active} active {task_noun} · {contributor_count} assigned {agent_noun}",
                if self.room_name.is_empty() {
                    "This workspace"
                } else {
                    self.room_name.as_str()
                }
            );
            tr.draw_text(
                scene,
                &subtitle,
                sub_size,
                colors.text_dim,
                at(transform, self.sheet.x0 + 16.0, self.sheet.y0 + 36.0),
                Some(text_width(self.sheet.width() - 32.0)),
            );
            tr.draw_text(
                scene,
                "Native workspace tasks · model optional · every action auditable",
                sub_size,
                colors.text_primary,
                at(transform, self.sheet.x0 + 16.0, self.sheet.y0 + 55.0),
                Some(text_width(self.sheet.width() - 32.0)),
            );
            return;
        }
        if self.view == ActivityView::CatchUp {
            let workspace = if self.room_name.is_empty() {
                "This workspace"
            } else {
                self.room_name.as_str()
            };
            let mut names = self
                .digest
                .briefs
                .iter()
                .flat_map(|brief| brief.authors.iter().map(|author| author.name.clone()))
                .collect::<Vec<_>>();
            names.sort();
            names.dedup();
            let subtitle = match names.as_slice() {
                [] => format!("{workspace} · Nothing new needs your attention"),
                [name] => format!("{workspace} · {name} kept things moving"),
                [first, second] => format!("{workspace} · {first} and {second} kept things moving"),
                [first, second, rest @ ..] => {
                    format!(
                        "{workspace} · {first}, {second} and {} others were active",
                        rest.len()
                    )
                }
            };
            tr.draw_text(
                scene,
                &subtitle,
                sub_size,
                colors.text_dim,
                at(transform, self.sheet.x0 + 16.0, self.sheet.y0 + 38.0),
                Some(text_width(self.sheet.width() - 32.0)),
            );
            return;
        }
        let people = if contributor_count == 1 {
            "1 person".to_string()
        } else {
            format!("{contributor_count} people")
        };
        let freshness = if self.view == ActivityView::CatchUp {
            "new "
        } else {
            ""
        };
        let change_noun = if change_count == 1 {
            "change"
        } else {
            "changes"
        };
        let mut subtitle = if self.view == ActivityView::CatchUp && change_count == 0 {
            match self.scope {
                RoomScope::ThisRoom => format!(
                    "{} · You're caught up",
                    if self.room_name.is_empty() {
                        "This workspace"
                    } else {
                        self.room_name.as_str()
                    }
                ),
                RoomScope::AllWorkspaces => "Every workspace · You're caught up".to_string(),
            }
        } else {
            match self.scope {
                RoomScope::ThisRoom => {
                    let workspace = if self.room_name.is_empty() {
                        "This workspace"
                    } else {
                        self.room_name.as_str()
                    };
                    format!(
                        "{workspace} \u{00b7} {change_count} {freshness}{change_noun} by {people}"
                    )
                }
                RoomScope::AllWorkspaces => format!(
                    "Every workspace on this machine \u{00b7} {change_count} {freshness}{change_noun} by {people}",
                ),
            }
        };
        if !self.feed_coverage.is_complete() {
            let missing = self.feed_coverage.unavailable_records.len();
            let capped = self.feed_coverage.capped_records;
            if missing > 0 {
                let _ = write!(
                    subtitle,
                    " · {missing} record{} unavailable",
                    if missing == 1 { "" } else { "s" }
                );
            }
            if capped > 0 {
                let _ = write!(subtitle, " · {capped} additional records truncated");
            }
        }
        let wrap_width = text_width(self.sheet.width() - 32.0);
        tr.draw_text(
            scene,
            &subtitle,
            sub_size,
            colors.text_dim,
            at(transform, self.sheet.x0 + 16.0, self.sheet.y0 + 32.0),
            Some(wrap_width),
        );
        if let Some(headline) = self
            .narrative
            .as_ref()
            .map(|narrative| narrative.text.clone())
            .or_else(|| self.digest.headline())
        {
            let latest = format!("Latest \u{00b7} {headline}");
            let latest = tr.elide_to_width(&latest, sub_size, wrap_width);
            tr.draw_text(
                scene,
                &latest,
                sub_size,
                colors.text_primary,
                at(transform, self.sheet.x0 + 16.0, self.sheet.y0 + 51.0),
                None,
            );
        }
    }

    /// Paint the workspace scope pills and record their rects for hit-testing.
    /// Labels progressively compact rather than disappearing: scope is a real
    /// data boundary, so even the narrowest supported sheet must expose it.
    fn draw_scope_chips(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        self.scope_rects.clear();
        let size = (tokens.typography.scale.sm - 1.0).max(10.0);
        let h = 22.0_f64;
        let cy = self.sheet.y0 + 24.0;
        let label_sets = [
            ["This workspace", "All workspaces"],
            ["Workspace", "All"],
            ["Here", "All"],
        ];
        let available = (self.sheet.width() * 0.62).max(1.0);
        let first_set = usize::from(self.sheet.width() < 520.0);
        let (labels, widths, total) = label_sets
            .iter()
            .skip(first_set)
            .map(|labels| {
                let widths = labels
                    .iter()
                    .map(|label| f64::from(tr.measure(label, size, None).0) + 16.0)
                    .collect::<Vec<_>>();
                let total = widths.iter().sum::<f64>() + 4.0;
                (labels, widths, total)
            })
            .find(|(_, _, total)| *total < available)
            .unwrap_or_else(|| {
                let labels = &label_sets[2];
                let widths = labels
                    .iter()
                    .map(|label| f64::from(tr.measure(label, size, None).0) + 12.0)
                    .collect::<Vec<_>>();
                let total = widths.iter().sum::<f64>() + 4.0;
                (labels, widths, total)
            });
        let mut x = self.sheet.x1 - 16.0 - total;
        for (i, sc) in RoomScope::ALL.iter().enumerate() {
            let w = widths.get(i).copied().unwrap_or(0.0);
            let label = labels.get(i).copied().unwrap_or_else(|| sc.label());
            let rect = Rect::new(x, cy - h / 2.0, x + w, cy + h / 2.0);
            self.scope_rects.push((*sc, rect));
            let active = self.scope == *sc;
            let shape = RoundedRect::from_rect(rect, h / 2.0);
            scene.fill(
                Fill::NonZero,
                transform,
                if active {
                    colors.accent[5]
                } else {
                    colors.lift(0.35)
                },
                None,
                &shape,
            );
            let (lw, lh) = tr.measure(label, size, None);
            tr.draw_text(
                scene,
                label,
                size,
                if active {
                    colors.text_primary
                } else {
                    colors.text_dim
                },
                at(
                    transform,
                    rect.x0 + (rect.width() - f64::from(lw)) / 2.0,
                    rect.y0 + (rect.height() - f64::from(lh)) / 2.0,
                ),
                None,
            );
            x += w + 4.0;
        }
    }

    /// Paint the Period and Author filter chip rows beneath the header.
    fn draw_filter_chip_rows(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let y = self.sheet.y0 + HEADER_H + VIEW_ROW_H;
        let periods: Vec<(Period, String)> = Period::ALL
            .iter()
            .map(|p| (*p, p.label().to_string()))
            .collect();
        let selected = self.period;
        self.period_rects = draw_chip_row(
            scene,
            transform,
            tr,
            theme,
            Rect::new(
                self.sheet.x0 + 12.0,
                y,
                self.sheet.x1 - 12.0,
                y + CHIP_ROW_H,
            ),
            &periods,
            |p| *p == selected,
        );

        if self.author_offset >= self.contributors.len() {
            self.author_offset = 0;
        }
        let mut people: Vec<(Option<String>, String)> = vec![(None, "Everyone".to_string())];
        people.extend(
            self.contributors
                .iter()
                .skip(self.author_offset)
                .map(|c| (Some(c.clone()), c.clone())),
        );
        let picked = self.author.clone();
        let y2 = y + CHIP_ROW_H;
        let mut page = draw_chip_row_paginated(
            scene,
            transform,
            tr,
            theme,
            Rect::new(
                self.sheet.x0 + 12.0,
                y2,
                self.sheet.x1 - 12.0,
                y2 + CHIP_ROW_H,
            ),
            &people,
            |a| *a == picked,
        );
        if self.author_offset > 0 && page.more_rect.is_none() {
            let row = Rect::new(
                self.sheet.x0 + 12.0,
                y2,
                self.sheet.x1 - 12.0,
                y2 + CHIP_ROW_H,
            );
            page.more_rect = draw_more_row(
                scene,
                transform,
                tr,
                theme,
                row,
                row.y0 + 1.0,
                "First people…",
            );
        }
        self.author_rects = page.items;
        self.author_more_rect = page.more_rect;
        // Everyone is repeated to preserve a stable escape from a person filter.
        self.author_more_advance = page.shown.saturating_sub(1).max(1);
    }

    // ---------- Events ----------

    /// `tree` is the live room document: a revert writes into it as a
    /// new forward change and commits, exactly as a `doc_*` mirror would.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        cursor: Option<Point>,
        tree: &mut WorkspaceTree,
    ) -> RoomActivityOutcome {
        self.handle_event_with_records(event, scale_factor, modifiers, cursor, tree, None)
    }

    fn handle_pointer_click(
        &mut self,
        point: Point,
        tree: &mut WorkspaceTree,
        records: Option<&RecordStore>,
    ) -> Option<RoomActivityOutcome> {
        if let Some(scope) = hit_value(&self.scope_rects, point) {
            self.keyboard_focus = Some(ActivityFocus::Scope(scope));
            self.set_scope(scope, tree);
            return Some(handled_outcome());
        }
        if let Some(view) = hit_value(&self.view_rects, point) {
            self.keyboard_focus = Some(ActivityFocus::View(view));
            self.set_view(view);
            return Some(handled_outcome());
        }
        let view_outcome = match self.view {
            ActivityView::CatchUp => self.handle_catchup_click(point, tree),
            ActivityView::Tasks => self.handle_tasks_click(point, tree),
            ActivityView::Explore => self.handle_explore_click(point, tree, records),
            ActivityView::Replay => self.handle_replay_click(point),
        };
        if view_outcome.is_some() {
            return view_outcome;
        }
        if let Some(period) = hit_value(&self.period_rects, point) {
            self.keyboard_focus = Some(ActivityFocus::Period(period));
            self.set_period(period);
            return Some(handled_outcome());
        }
        if let Some(author) = hit_value(&self.author_rects, point) {
            self.keyboard_focus = Some(ActivityFocus::Author(author.clone()));
            self.set_author_filter(author);
            return Some(handled_outcome());
        }
        if self
            .author_more_rect
            .is_some_and(|rect| rect.contains(point))
        {
            self.keyboard_focus = Some(ActivityFocus::MorePeople);
            self.advance_people();
            return Some(handled_outcome());
        }
        None
    }

    fn handle_tasks_click(
        &mut self,
        point: Point,
        tree: &mut WorkspaceTree,
    ) -> Option<RoomActivityOutcome> {
        if self.new_task_rect.is_some_and(|rect| rect.contains(point)) {
            self.keyboard_focus = Some(ActivityFocus::NewTask);
            self.open_task_composer(tree);
            return Some(handled_outcome());
        }
        if let Some(route) = hit_value(&self.task_route_rects, point) {
            self.keyboard_focus = Some(ActivityFocus::TaskRoute(route));
            self.task_route = route;
            return Some(handled_outcome());
        }
        if self
            .task_cancel_rect
            .is_some_and(|rect| rect.contains(point))
        {
            self.keyboard_focus = Some(ActivityFocus::TaskCancelCompose);
            self.close_task_composer();
            return Some(handled_outcome());
        }
        if self
            .task_create_rect
            .is_some_and(|rect| rect.contains(point))
        {
            self.keyboard_focus = Some(ActivityFocus::TaskCreate);
            return Some(self.create_task_from_draft(tree));
        }
        if let Some(action) = hit_value(&self.task_action_rects, point) {
            self.keyboard_focus = Some(ActivityFocus::TaskAction(action));
            return Some(self.apply_task_action(action, tree));
        }
        if let Some(id) = hit_value(&self.task_rects, point) {
            self.keyboard_focus = Some(ActivityFocus::Task(id.clone()));
            self.selected_task = (self.selected_task.as_deref() != Some(id.as_str())).then_some(id);
            return Some(handled_outcome());
        }
        None
    }

    fn open_task_composer(&mut self, tree: &WorkspaceTree) {
        self.task_compose_open = true;
        self.task_draft.clear();
        self.task_route = WorkspaceTaskRoute::Manual;
        self.task_agent = tree
            .active_workspace()
            .and_then(|workspace| workspace.agent_members().into_iter().next())
            .map(|(agent, _)| agent);
        self.task_attachment = self.selection_detail().and_then(|detail| {
            detail
                .resources
                .first()
                .map(|resource| WorkspaceTaskResource {
                    app_id: resource.app_id.clone(),
                    app_label: resource.app_label.clone(),
                    id: resource.id.clone(),
                    kind: resource.kind.clone(),
                    label: resource.label.clone(),
                })
        });
    }

    fn close_task_composer(&mut self) {
        self.task_compose_open = false;
        self.task_draft.clear();
        self.task_attachment = None;
    }

    fn local_user(&self) -> String {
        self.explicit_local_user
            .clone()
            .unwrap_or_else(crate::shell::state::local_user_id)
    }

    fn create_task_from_draft(&mut self, tree: &mut WorkspaceTree) -> RoomActivityOutcome {
        let workspace = tree.active;
        let assigned_agent = (self.task_route != WorkspaceTaskRoute::Manual)
            .then(|| self.task_agent.clone())
            .flatten();
        let created = tree.create_workspace_task(
            workspace,
            self.task_draft.clone(),
            self.local_user(),
            assigned_agent,
            self.task_route,
            self.task_attachment.clone(),
        );
        if let Some(id) = created {
            self.selected_task = Some(id);
            self.close_task_composer();
            self.refresh(tree);
            RoomActivityOutcome {
                handled: true,
                task_changed: true,
                ..RoomActivityOutcome::default()
            }
        } else {
            handled_outcome()
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "task action admission keeps explicit early refusals and the single status mutation path together for auditable outcomes"
    )]
    fn apply_task_action(
        &mut self,
        action: TaskAction,
        tree: &mut WorkspaceTree,
    ) -> RoomActivityOutcome {
        let Some(task_id) = self.selected_task.clone() else {
            return handled_outcome();
        };
        let Some(task) = self.tasks.iter().find(|task| task.id == task_id) else {
            return handled_outcome();
        };
        // Starting a manual task is a human-authored state transition. Model
        // and rules-only tasks need an admitted runtime run before their state
        // may advance; presenting the same transition as "Start" would claim
        // execution that never happened.
        if !self.task_actions_for(task).contains(&action) {
            return handled_outcome();
        }
        if action == TaskAction::OpenResource {
            let open_target = self
                .tasks
                .iter()
                .find(|task| task.id == task_id)
                .and_then(|task| task.resource.as_ref())
                .map(|resource| ui::HistoryResourceTarget {
                    app_id: resource.app_id.clone(),
                    app_label: resource.app_label.clone(),
                    id: resource.id.clone(),
                    kind: resource.kind.clone(),
                    label: resource.label.clone(),
                });
            return RoomActivityOutcome {
                handled: true,
                open_target,
                ..RoomActivityOutcome::default()
            };
        }
        if action == TaskAction::Launch {
            let assigned_agent = task.assigned_agent.clone();
            let dispatched = self
                .task_broker
                .as_ref()
                .ok_or_else(|| "agent task dispatch is unavailable".to_string())
                .and_then(|broker| broker.dispatch_task(&self.task_room, &task_id));
            if dispatched.is_ok() {
                self.refresh(tree);
            } else if let Some(agent) = assigned_agent {
                let detail = dispatched
                    .as_ref()
                    .err()
                    .cloned()
                    .unwrap_or_else(|| "agent task dispatch failed".into());
                self.agent_startup.insert(
                    (self.task_room.clone(), agent),
                    AgentStartup::Failed { detail },
                );
            }
            return RoomActivityOutcome {
                handled: true,
                task_changed: dispatched.is_ok(),
                agent_runtime_changed: dispatched.is_err(),
                ..RoomActivityOutcome::default()
            };
        }
        if action == TaskAction::StartAgent {
            let Some(agent) = task.assigned_agent.as_deref() else {
                return handled_outcome();
            };
            let Some(workspace) = tree.active_workspace().cloned() else {
                return handled_outcome();
            };
            let key = (workspace.room.to_string(), agent.to_string());
            if matches!(
                self.agent_startup.get(&key),
                Some(AgentStartup::Starting { .. })
            ) {
                return handled_outcome();
            }
            let state = match crate::shell::launcher::launch_workspace_agent(agent, &workspace) {
                Ok(_) => AgentStartup::Starting {
                    since: Instant::now(),
                },
                Err(error) => AgentStartup::Failed {
                    detail: error.user_message(),
                },
            };
            self.agent_startup.insert(key, state);
            return RoomActivityOutcome {
                handled: true,
                agent_runtime_changed: true,
                ..RoomActivityOutcome::default()
            };
        }
        let status = match action {
            TaskAction::Start => WorkspaceTaskStatus::InProgress,
            TaskAction::Complete => WorkspaceTaskStatus::Done,
            TaskAction::Cancel => WorkspaceTaskStatus::Cancelled,
            TaskAction::OpenResource | TaskAction::StartAgent | TaskAction::Launch => {
                return handled_outcome();
            }
        };
        let changed =
            tree.set_workspace_task_status(tree.active, &task_id, status, &self.local_user());
        if changed {
            self.refresh(tree);
        }
        RoomActivityOutcome {
            handled: true,
            task_changed: changed,
            ..RoomActivityOutcome::default()
        }
    }

    fn task_actions_for(&self, task: &WorkspaceTask) -> Vec<TaskAction> {
        let mut actions = task_actions(task);
        let live = task
            .assigned_agent
            .as_ref()
            .is_some_and(|agent| self.agent_is_live(agent));
        if task.route != WorkspaceTaskRoute::Manual
            && matches!(
                task.status,
                WorkspaceTaskStatus::Queued | WorkspaceTaskStatus::Waiting
            )
        {
            let Some(agent) = task.assigned_agent.as_deref() else {
                return actions;
            };
            if live {
                actions.insert(0, TaskAction::Launch);
            } else if !matches!(
                self.agent_startup_state(agent),
                Some(AgentStartup::Starting { .. })
            ) {
                actions.insert(0, TaskAction::StartAgent);
            }
        }
        actions
    }

    fn task_action_label(&self, action: TaskAction, task: &WorkspaceTask) -> String {
        if action == TaskAction::StartAgent
            && task.assigned_agent.as_deref().is_some_and(|agent| {
                matches!(
                    self.agent_startup_state(agent),
                    Some(AgentStartup::Failed { .. })
                )
            })
        {
            "Retry agent".into()
        } else {
            action.label().to_string()
        }
    }

    fn task_execution_notice(&self, task: &WorkspaceTask) -> Option<String> {
        if task.route == WorkspaceTaskRoute::Manual
            || !matches!(
                task.status,
                WorkspaceTaskStatus::Queued | WorkspaceTaskStatus::Waiting
            )
        {
            return None;
        }
        let Some(agent) = task.assigned_agent.as_deref() else {
            return Some(
                "No enabled workspace agent is assigned · this task has not started".into(),
            );
        };
        if self.agent_is_live(agent) {
            return Some("Agent connected · ready to run this task".into());
        }
        match self.agent_startup_state(agent) {
            Some(AgentStartup::Starting { .. }) => {
                Some("Starting managed agent · waiting for its broker subscription".into())
            }
            Some(AgentStartup::Failed { detail }) => Some(format!("Agent offline · {detail}")),
            None => Some("Agent runtime not connected · this task has not started".into()),
        }
    }

    fn handle_catchup_click(
        &mut self,
        point: Point,
        tree: &mut WorkspaceTree,
    ) -> Option<RoomActivityOutcome> {
        if self.resume_rect.contains(point)
            && let Some(open_target) = self.resume_target.clone()
        {
            return Some(RoomActivityOutcome {
                handled: true,
                open_target: Some(open_target),
                ..RoomActivityOutcome::default()
            });
        }
        if let Some(index) = hit_value(&self.brief_rects, point) {
            self.keyboard_focus = Some(ActivityFocus::Brief(index));
            if self.expanded_brief == Some(index) {
                self.open_brief_in_explorer(index);
            } else {
                self.expanded_brief = Some(index);
                self.brief_expansion_started = Some((index, Instant::now()));
            }
            return Some(handled_outcome());
        }
        if let Some(boundary) = hit_value(&self.boundary_rects, point) {
            self.keyboard_focus = Some(ActivityFocus::WorkSessionBoundary(boundary));
            return Some(RoomActivityOutcome {
                handled: true,
                named: self.move_work_session_boundary(tree, boundary),
                ..RoomActivityOutcome::default()
            });
        }
        if self
            .brief_more_rect
            .is_some_and(|rect| rect.contains(point))
        {
            self.keyboard_focus = Some(ActivityFocus::MoreBriefs);
            self.advance_briefs();
            return Some(handled_outcome());
        }
        None
    }

    fn handle_replay_click(&mut self, point: Point) -> Option<RoomActivityOutcome> {
        if self.replay_back_rect.contains(point) {
            self.keyboard_focus = Some(ActivityFocus::ReplayBack);
            self.step_replay(false);
        } else if self.replay_play_rect.contains(point) {
            self.keyboard_focus = Some(ActivityFocus::ReplayPlay);
            self.toggle_replay();
        } else if self.replay_forward_rect.contains(point) {
            self.keyboard_focus = Some(ActivityFocus::ReplayForward);
            self.step_replay(true);
        } else {
            return None;
        }
        Some(handled_outcome())
    }

    fn handle_explore_click(
        &mut self,
        point: Point,
        tree: &mut WorkspaceTree,
        records: Option<&RecordStore>,
    ) -> Option<RoomActivityOutcome> {
        if let Some(pane) = hit_value(&self.explorer_pane_rects, point) {
            self.keyboard_focus = Some(ActivityFocus::ExplorerPane(pane));
            self.explorer_pane = pane;
            return Some(handled_outcome());
        }
        if let Some(action) = hit_value(&self.detail_action_rects, point) {
            self.keyboard_focus = Some(ActivityFocus::Detail(action));
            return Some(self.apply_detail_action(action, tree, records));
        }
        if let Some(lens) = hit_value(&self.lens_rects, point) {
            self.keyboard_focus = Some(ActivityFocus::Lens(lens.clone()));
            self.lens = lens;
            self.reinstall_filter();
            return Some(handled_outcome());
        }
        if self.lens_more_rect.is_some_and(|rect| rect.contains(point)) {
            self.keyboard_focus = Some(ActivityFocus::MoreLenses);
            self.advance_lenses();
            return Some(handled_outcome());
        }
        None
    }

    fn apply_detail_action(
        &mut self,
        action: DetailAction,
        tree: &mut WorkspaceTree,
        records: Option<&RecordStore>,
    ) -> RoomActivityOutcome {
        let mut outcome = handled_outcome();
        match action {
            DetailAction::Open => {
                outcome.open_target = self
                    .selection_detail()
                    .and_then(|detail| detail.resources.into_iter().next());
            }
            DetailAction::Compare => {
                self.comparison_attempted = true;
                self.comparison = self.compare_selection(tree, records);
                self.comparison_selection = self.selected_ids();
                // Reading an `AutoCommit` comparison closes any pending
                // transaction. That changes the observed heads without
                // changing content; keep refresh from treating the next
                // paint as an external edit and discarding the result.
                self.last_heads = tree.doc.get_heads();
                outcome.compared = self.comparison.is_some();
            }
            DetailAction::Undo => {
                if let Some(request) = self.selection_undo_request() {
                    let applied = self.undo_band(tree, records, &request);
                    outcome.reverted = applied.wrote;
                    outcome.undo_partial = applied.partial;
                }
            }
            DetailAction::UndoTask => {
                let applied = self.undo_selected_agent_run(records);
                outcome.reverted = applied.wrote;
                outcome.undo_partial = applied.partial;
            }
            DetailAction::Split => outcome.named = self.split_selection_group(tree),
            DetailAction::Merge => outcome.named = self.merge_selection_groups(tree),
        }
        outcome
    }

    fn undo_selected_agent_run(&mut self, records: Option<&RecordStore>) -> UndoApply {
        let Some(indexed) = self.selected_agent_run().cloned() else {
            return UndoApply::default();
        };
        let Some(records) = records else {
            return UndoApply::default();
        };
        // This is the all-or-nothing reachability check from `undo_run`,
        // adapted to RecordStore's one-document-at-a-time borrow API. Nothing
        // is touched unless every referenced document is live here.
        if !indexed.run.steps.iter().all(|step| {
            RecordId::parse(&step.record).is_some_and(|id| records.read(&id, |_| ()).is_some())
        }) {
            self.agent_undo_notice = Some((
                indexed.run.id,
                "Nothing changed. Open every affected document first.".to_string(),
            ));
            return UndoApply::default();
        }

        let authors = self.source.authors().clone();
        let mut wrote = false;
        let mut partial = false;
        for step in &indexed.run.steps {
            let Some(id) = RecordId::parse(&step.record) else {
                return UndoApply::default();
            };
            let state = records
                .with(&id, |doc| {
                    let outcome = ui::undo_step(doc, step, None, &authors);
                    if matches!(
                        outcome.state,
                        StepState::Undone { .. } | StepState::KeptSome { .. }
                    ) {
                        let _ = qdoc::history::commit_stamped(doc);
                    }
                    outcome.state
                })
                .unwrap_or(StepState::Unreachable);
            wrote |= matches!(state, StepState::Undone { .. } | StepState::KeptSome { .. });
            partial |= !state.is_clean();
        }
        let notice = if wrote && partial {
            "Task partly undone; later collaborator edits were kept."
        } else if wrote {
            "Whole task undone as a new history entry."
        } else {
            "The task's effects were already gone."
        };
        self.agent_undo_notice = Some((indexed.run.id, notice.to_string()));
        UndoApply { wrote, partial }
    }

    fn selection_undo_request(&self) -> Option<BandUndoRequest> {
        let (start, end) = self.timeline.selection()?;
        let first = self.timeline.entries().get(start)?;
        let last = self.timeline.entries().get(end)?;
        Some(BandUndoRequest {
            start: first.id,
            end: last.id,
            label: self.selection_detail().map(|detail| detail.title),
            authors: Vec::new(),
        })
    }

    fn handle_pane_key(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
        tree: &mut WorkspaceTree,
        records: Option<&RecordStore>,
    ) -> Option<RoomActivityOutcome> {
        if self.task_compose_open {
            let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
            else {
                return None;
            };
            return match logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.close_task_composer();
                    Some(handled_outcome())
                }
                Key::Named(NamedKey::Enter) => Some(self.create_task_from_draft(tree)),
                Key::Named(NamedKey::Backspace) => {
                    self.task_draft.pop();
                    Some(handled_outcome())
                }
                Key::Character(text)
                    if !modifiers.state().control_key()
                        && !modifiers.state().super_key()
                        && !modifiers.state().alt_key() =>
                {
                    if self.task_draft.chars().count() < 160 {
                        self.task_draft.push_str(text);
                    }
                    Some(handled_outcome())
                }
                _ => Some(handled_outcome()),
            };
        }
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return None;
        };
        let state = modifiers.state();
        if self.timeline_is_visible() && self.timeline.wants_keyboard() {
            return None;
        }
        if self.timeline_is_visible() && self.timeline.is_focused() {
            if matches!(logical_key, Key::Named(NamedKey::Tab)) {
                self.timeline.set_focus(false);
                self.keyboard_focus = Some(ActivityFocus::Timeline);
                self.move_focus(state.shift_key(), None);
                return Some(handled_outcome());
            }
            return None;
        }
        if state.control_key() || state.super_key() || state.alt_key() {
            return None;
        }
        self.handle_navigation_key(logical_key, state.shift_key(), tree, records)
    }

    pub fn handle_event_with_records(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        cursor: Option<Point>,
        tree: &mut WorkspaceTree,
        records: Option<&RecordStore>,
    ) -> RoomActivityOutcome {
        let mut out = RoomActivityOutcome::default();

        // The inline group-name editor owns every unchorded key until it
        // commits or cancels; Tab included, so a half-typed name cannot be
        // abandoned by the pane-level focus controller.
        if let Some(outcome) = self.handle_pane_key(event, *modifiers, tree, records) {
            return outcome;
        }

        // Pane chrome gets first refusal so the timeline cannot claim a click
        // on controls painted above it.
        if let (Some(point), WindowEvent::MouseInput { state, button, .. }) = (cursor, event)
            && *state == ElementState::Pressed
            && *button == winit::event::MouseButton::Left
            && let Some(outcome) = self.handle_pointer_click(point, tree, records)
        {
            return outcome;
        }

        if !self.timeline_is_visible() {
            return out;
        }

        let mut handled = false;
        let mut focus_target = None;
        // `Widget::handle_event` needs a layout tree; a throwaway one since
        // the timeline hit-tests against its own rect and ignores it.
        let mut scratch: taffy::TaffyTree<()> = taffy::TaffyTree::new();
        // A fresh, empty tree has no capacity limit to hit in practice;
        // declining to forward the event is still safer than a panic.
        let Ok(node) = scratch.new_leaf(taffy::Style::default()) else {
            return out;
        };
        let root = automerge::ROOT;
        self.timeline.handle_event(
            event,
            scale_factor,
            modifiers,
            &mut tree.doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &scratch,
        );
        if focus_target.is_some() {
            self.timeline.set_focus(true);
            self.keyboard_focus = Some(ActivityFocus::Timeline);
        }
        out.handled = handled;

        if let Some(id) = self.revert_intent.take() {
            out.handled = true;
            if self.revert_to_with_records(tree, records, &id) {
                out.reverted = true;
            }
        }
        let undo_req = self.undo_intent.borrow_mut().take();
        if let Some(req) = undo_req {
            out.handled = true;
            let applied = self.undo_band(tree, records, &req);
            out.reverted = applied.wrote;
            out.undo_partial = applied.partial;
        }
        let name_req = self.name_intent.borrow_mut().take();
        if let Some(req) = name_req {
            out.handled = true;
            if self.apply_name(tree, &req) {
                out.named = true;
            }
        }
        if self.clear_filters_intent.replace(false) {
            self.period = Period::AllTime;
            self.author = None;
            self.lens = LensSelection::All;
            self.reinstall_filter();
            out.handled = true;
        }
        out
    }

    fn focus_order(&self) -> Vec<ActivityFocus> {
        let mut order = Vec::new();
        order.extend(
            self.scope_rects
                .iter()
                .map(|(scope, _)| ActivityFocus::Scope(*scope)),
        );
        order.extend(
            self.view_rects
                .iter()
                .map(|(view, _)| ActivityFocus::View(*view)),
        );
        match self.view {
            ActivityView::CatchUp => {
                if self.resume_target.is_some() && self.resume_rect.area() > 0.0 {
                    order.push(ActivityFocus::Resume);
                }
                order.extend(
                    self.brief_rects
                        .iter()
                        .map(|(index, _)| ActivityFocus::Brief(*index)),
                );
                order.extend(
                    self.boundary_rects
                        .iter()
                        .map(|(boundary, _)| ActivityFocus::WorkSessionBoundary(*boundary)),
                );
            }
            ActivityView::Tasks => {
                if self.new_task_rect.is_some() {
                    order.push(ActivityFocus::NewTask);
                }
                if self.task_compose_open {
                    order.extend(
                        self.task_route_rects
                            .iter()
                            .map(|(route, _)| ActivityFocus::TaskRoute(*route)),
                    );
                    if self.task_cancel_rect.is_some() {
                        order.push(ActivityFocus::TaskCancelCompose);
                    }
                    if self.task_create_rect.is_some() {
                        order.push(ActivityFocus::TaskCreate);
                    }
                }
                order.extend(
                    self.task_rects
                        .iter()
                        .map(|(id, _)| ActivityFocus::Task(id.clone())),
                );
                order.extend(
                    self.task_action_rects
                        .iter()
                        .map(|(action, _)| ActivityFocus::TaskAction(*action)),
                );
            }
            ActivityView::Explore => {
                order.extend(
                    self.explorer_pane_rects
                        .iter()
                        .map(|(pane, _)| ActivityFocus::ExplorerPane(*pane)),
                );
                if self.timeline_is_visible() && self.timeline.visible_len() > 0 {
                    order.push(ActivityFocus::Timeline);
                }
                if self.author_more_rect.is_some() {
                    order.push(ActivityFocus::MorePeople);
                }
                order.extend(
                    self.period_rects
                        .iter()
                        .map(|(period, _)| ActivityFocus::Period(*period)),
                );
                if self.lens_more_rect.is_some() {
                    order.push(ActivityFocus::MoreLenses);
                }
                order.extend(
                    self.author_rects
                        .iter()
                        .map(|(author, _)| ActivityFocus::Author(author.clone())),
                );
                order.extend(
                    self.lens_rects
                        .iter()
                        .map(|(lens, _)| ActivityFocus::Lens(lens.clone())),
                );
                order.extend(
                    self.detail_action_rects
                        .iter()
                        .map(|(action, _)| ActivityFocus::Detail(*action)),
                );
            }
            ActivityView::Replay => {
                if !self.replay_frames.is_empty() {
                    order.extend([
                        ActivityFocus::ReplayBack,
                        ActivityFocus::ReplayPlay,
                        ActivityFocus::ReplayForward,
                    ]);
                }
            }
        }
        if self.view == ActivityView::CatchUp && self.brief_more_rect.is_some() {
            order.push(ActivityFocus::MoreBriefs);
        }
        order
    }

    fn focused_rect(&self) -> Option<Rect> {
        match self.keyboard_focus.as_ref()? {
            ActivityFocus::Scope(value) => self
                .scope_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::View(value) => self
                .view_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::Period(value) => self
                .period_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::Author(value) => self
                .author_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::MorePeople => self.author_more_rect,
            ActivityFocus::Resume => Some(self.resume_rect),
            ActivityFocus::Brief(value) => self
                .brief_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::WorkSessionBoundary(value) => self
                .boundary_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::MoreBriefs => self.brief_more_rect,
            ActivityFocus::NewTask => self.new_task_rect,
            ActivityFocus::Task(value) => self
                .task_rects
                .iter()
                .find_map(|(id, rect)| (id == value).then_some(*rect)),
            ActivityFocus::TaskRoute(value) => self
                .task_route_rects
                .iter()
                .find_map(|(route, rect)| (route == value).then_some(*rect)),
            ActivityFocus::TaskCreate => self.task_create_rect,
            ActivityFocus::TaskCancelCompose => self.task_cancel_rect,
            ActivityFocus::TaskAction(value) => self
                .task_action_rects
                .iter()
                .find_map(|(action, rect)| (action == value).then_some(*rect)),
            ActivityFocus::Lens(value) => self
                .lens_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::MoreLenses => self.lens_more_rect,
            ActivityFocus::Detail(value) => self
                .detail_action_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::ExplorerPane(value) => self
                .explorer_pane_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::Timeline => Some(self.timeline.rect()),
            ActivityFocus::ReplayBack => Some(self.replay_back_rect),
            ActivityFocus::ReplayPlay => Some(self.replay_play_rect),
            ActivityFocus::ReplayForward => Some(self.replay_forward_rect),
        }
    }

    fn move_focus(&mut self, backwards: bool, to_edge: Option<bool>) {
        let order = self.focus_order();
        if order.is_empty() {
            self.keyboard_focus = None;
            return;
        }
        let last = order.len().saturating_sub(1);
        let next = if let Some(end) = to_edge {
            if end { last } else { 0 }
        } else if let Some(current) = self
            .keyboard_focus
            .as_ref()
            .and_then(|focus| order.iter().position(|candidate| candidate == focus))
        {
            if backwards {
                current.checked_sub(1).unwrap_or(last)
            } else {
                current
                    .checked_add(1)
                    .filter(|next| *next < order.len())
                    .unwrap_or(0)
            }
        } else if backwards {
            last
        } else {
            0
        };
        self.keyboard_focus = order.get(next).cloned();
        self.timeline
            .set_focus(self.keyboard_focus == Some(ActivityFocus::Timeline));
    }

    fn activate_focus(
        &mut self,
        tree: &mut WorkspaceTree,
        records: Option<&RecordStore>,
    ) -> RoomActivityOutcome {
        let Some(focus) = self.keyboard_focus.clone() else {
            return RoomActivityOutcome::default();
        };
        let mut outcome = RoomActivityOutcome {
            handled: true,
            ..RoomActivityOutcome::default()
        };
        match focus {
            ActivityFocus::Scope(value) => self.set_scope(value, tree),
            ActivityFocus::View(value) => self.set_view(value),
            ActivityFocus::Period(value) => self.set_period(value),
            ActivityFocus::Author(value) => self.set_author_filter(value),
            ActivityFocus::MorePeople => self.advance_people(),
            ActivityFocus::Resume => outcome.open_target.clone_from(&self.resume_target),
            ActivityFocus::Brief(index) => {
                if self.expanded_brief == Some(index) {
                    self.open_brief_in_explorer(index);
                } else {
                    self.expanded_brief = Some(index);
                    self.brief_expansion_started = Some((index, Instant::now()));
                }
            }
            ActivityFocus::WorkSessionBoundary(boundary) => {
                outcome.named = self.move_work_session_boundary(tree, boundary);
            }
            ActivityFocus::MoreBriefs => self.advance_briefs(),
            ActivityFocus::NewTask => self.open_task_composer(tree),
            ActivityFocus::Task(id) => {
                self.selected_task =
                    (self.selected_task.as_deref() != Some(id.as_str())).then_some(id);
            }
            ActivityFocus::TaskRoute(route) => self.task_route = route,
            ActivityFocus::TaskCreate => return self.create_task_from_draft(tree),
            ActivityFocus::TaskCancelCompose => self.close_task_composer(),
            ActivityFocus::TaskAction(action) => return self.apply_task_action(action, tree),
            ActivityFocus::Lens(value) => {
                self.lens = value;
                self.reinstall_filter();
            }
            ActivityFocus::MoreLenses => self.advance_lenses(),
            ActivityFocus::Detail(DetailAction::Open) => {
                outcome.open_target = self
                    .selection_detail()
                    .and_then(|detail| detail.resources.into_iter().next());
            }
            ActivityFocus::Detail(DetailAction::Compare) => {
                self.comparison_attempted = true;
                self.comparison = self.compare_selection(tree, records);
                self.comparison_selection = self.selected_ids();
                self.last_heads = tree.doc.get_heads();
                outcome.compared = self.comparison.is_some();
            }
            ActivityFocus::Detail(DetailAction::Undo) => {
                if let Some((start, end)) = self.timeline.selection()
                    && let (Some(first), Some(last)) = (
                        self.timeline.entries().get(start),
                        self.timeline.entries().get(end),
                    )
                {
                    let request = BandUndoRequest {
                        start: first.id,
                        end: last.id,
                        label: self.selection_detail().map(|detail| detail.title),
                        authors: Vec::new(),
                    };
                    let applied = self.undo_band(tree, records, &request);
                    outcome.reverted = applied.wrote;
                    outcome.undo_partial = applied.partial;
                }
            }
            ActivityFocus::Detail(DetailAction::UndoTask) => {
                let applied = self.undo_selected_agent_run(records);
                outcome.reverted = applied.wrote;
                outcome.undo_partial = applied.partial;
            }
            ActivityFocus::Detail(DetailAction::Split) => {
                outcome.named = self.split_selection_group(tree);
            }
            ActivityFocus::Detail(DetailAction::Merge) => {
                outcome.named = self.merge_selection_groups(tree);
            }
            ActivityFocus::ExplorerPane(pane) => self.explorer_pane = pane,
            ActivityFocus::Timeline => self.timeline.set_focus(true),
            ActivityFocus::ReplayBack => self.step_replay(false),
            ActivityFocus::ReplayPlay => self.toggle_replay(),
            ActivityFocus::ReplayForward => self.step_replay(true),
        }
        outcome
    }

    fn handle_navigation_key(
        &mut self,
        key: &Key,
        shift: bool,
        tree: &mut WorkspaceTree,
        records: Option<&RecordStore>,
    ) -> Option<RoomActivityOutcome> {
        match key {
            Key::Named(NamedKey::Tab) => self.move_focus(shift, None),
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) => {
                self.move_focus(false, None);
            }
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) => {
                self.move_focus(true, None);
            }
            Key::Named(NamedKey::Home) => self.move_focus(false, Some(false)),
            Key::Named(NamedKey::End) => self.move_focus(false, Some(true)),
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                return Some(self.activate_focus(tree, records));
            }
            _ => return None,
        }
        Some(RoomActivityOutcome {
            handled: true,
            ..RoomActivityOutcome::default()
        })
    }

    /// The label currently carrying keyboard focus. Besides making focus
    /// observable in tests, this is the stable hand-off point for the shell's
    /// overlay AccessKit bridge (immediate-mode overlays have no `NodeId`).
    pub fn focused_control_label(&self) -> Option<String> {
        self.focus_label(self.keyboard_focus.as_ref()?)
    }

    fn focus_label(&self, focus: &ActivityFocus) -> Option<String> {
        match focus {
            ActivityFocus::Scope(value) => Some(value.label().to_string()),
            ActivityFocus::View(value) => Some(value.label().to_string()),
            ActivityFocus::Period(value) => Some(value.label().to_string()),
            ActivityFocus::Author(Some(value))
            | ActivityFocus::Lens(
                LensSelection::App(value)
                | LensSelection::ResourceKind(value)
                | LensSelection::Resource(value)
                | LensSelection::WorkGroup(value),
            ) => Some(value.clone()),
            ActivityFocus::Author(None) => Some("Everyone".to_string()),
            ActivityFocus::MorePeople => Some("More people".to_string()),
            ActivityFocus::Resume => self
                .resume_target
                .as_ref()
                .map(|target| format!("Resume {}", target.label)),
            ActivityFocus::Brief(index) => self
                .digest
                .briefs
                .get(*index)
                .map(|brief| brief.title.clone()),
            ActivityFocus::WorkSessionBoundary(boundary) => Some(match boundary.direction {
                BoundaryDirection::IntoNewer => {
                    "Move the last action into the newer work session".to_string()
                }
                BoundaryDirection::IntoOlder => {
                    "Move the first action into the older work session".to_string()
                }
            }),
            ActivityFocus::MoreBriefs => Some("More work sessions".to_string()),
            ActivityFocus::NewTask => Some("New task".to_string()),
            ActivityFocus::Task(id) => self
                .tasks
                .iter()
                .find(|task| &task.id == id)
                .map(|task| task.title.clone()),
            ActivityFocus::TaskRoute(route) => Some(route.label().to_string()),
            ActivityFocus::TaskCreate => Some("Create task".to_string()),
            ActivityFocus::TaskCancelCompose => Some("Cancel task creation".to_string()),
            ActivityFocus::TaskAction(action) => Some(action.label().to_string()),
            ActivityFocus::Lens(LensSelection::All) => Some("Everything".to_string()),
            ActivityFocus::Lens(LensSelection::AgentTasks) => Some("Agent tasks".to_string()),
            ActivityFocus::Lens(LensSelection::Facet(key, value)) => {
                Some(format!("{key}: {value}"))
            }
            ActivityFocus::Lens(LensSelection::Unclassified) => Some("Unclassified".to_string()),
            ActivityFocus::MoreLenses => Some("More lenses".to_string()),
            ActivityFocus::Detail(action) => Some(self.detail_action_label(*action).to_string()),
            ActivityFocus::ExplorerPane(pane) => Some(pane.label().to_string()),
            ActivityFocus::Timeline => Some("History entries".to_string()),
            ActivityFocus::ReplayBack => Some("Previous checkpoint".to_string()),
            ActivityFocus::ReplayPlay => Some(if self.replay_playing {
                "Pause replay".to_string()
            } else {
                "Play replay".to_string()
            }),
            ActivityFocus::ReplayForward => Some("Next checkpoint".to_string()),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "exhaustive focus-to-accessibility mapping is clearest beside the focus enum variants"
    )]
    fn access_control(&self, focus: &ActivityFocus) -> Option<AccessControl> {
        use ui::a11y::Role;

        let control = |role, selected, expanded, description: &str| AccessControl {
            role,
            selected,
            expanded,
            description: description.to_string(),
        };
        Some(match focus {
            ActivityFocus::Scope(value) => control(
                Role::RadioButton,
                self.scope == *value,
                None,
                "History scope",
            ),
            ActivityFocus::View(value) => {
                control(Role::Tab, self.view == *value, None, "History view")
            }
            ActivityFocus::Period(value) => {
                control(Role::RadioButton, self.period == *value, None, "Time range")
            }
            ActivityFocus::Author(value) => control(
                Role::RadioButton,
                self.author == *value,
                None,
                "Person filter",
            ),
            ActivityFocus::MorePeople => {
                control(Role::Button, false, None, "Show more contributor filters")
            }
            ActivityFocus::Resume => {
                let target = self.resume_target.as_ref()?;
                control(
                    Role::Button,
                    false,
                    None,
                    &format!("Open the last resource you worked on: {}", target.label),
                )
            }
            ActivityFocus::Brief(index) => {
                let brief = self.digest.briefs.get(*index)?;
                let change_noun = if brief.changes == 1 {
                    "change"
                } else {
                    "changes"
                };
                let mut description = format!(
                    "{}; last changed {}; {} {} by {}",
                    brief.detail,
                    format_relative_time(brief.last_at, self.now()),
                    brief.changes,
                    change_noun,
                    brief
                        .authors
                        .iter()
                        .map(|author| author.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                if self.expanded_brief == Some(*index) {
                    for action in &brief.actions {
                        let _ = write!(description, "; {}: {}", action.author.name, action.summary);
                    }
                }
                AccessControl {
                    role: Role::Button,
                    selected: false,
                    expanded: Some(self.expanded_brief == Some(*index)),
                    description,
                }
            }
            ActivityFocus::WorkSessionBoundary(boundary) => control(
                Role::Button,
                false,
                None,
                match boundary.direction {
                    BoundaryDirection::IntoNewer => {
                        "Redraw this boundary by moving one action into the newer work session"
                    }
                    BoundaryDirection::IntoOlder => {
                        "Redraw this boundary by moving one action into the older work session"
                    }
                },
            ),
            ActivityFocus::MoreBriefs => {
                control(Role::Button, false, None, "Show more work sessions")
            }
            ActivityFocus::NewTask => control(
                Role::Button,
                false,
                None,
                "Create a workspace task without requiring a model",
            ),
            ActivityFocus::Task(id) => {
                let task = self.tasks.iter().find(|task| &task.id == id)?;
                AccessControl {
                    role: Role::Button,
                    selected: self.selected_task.as_ref() == Some(id),
                    expanded: Some(self.selected_task.as_ref() == Some(id)),
                    description: format!(
                        "{}; {}; {}; {}",
                        task.status.label(),
                        task.route.label(),
                        task.assigned_agent
                            .as_deref()
                            .map_or("unassigned", |_| "assigned to an agent"),
                        task.resource
                            .as_ref()
                            .map_or("no attached resource", |resource| resource.label.as_str())
                    ),
                }
            }
            ActivityFocus::TaskRoute(route) => control(
                Role::RadioButton,
                self.task_route == *route,
                None,
                "Task execution route",
            ),
            ActivityFocus::TaskCreate | ActivityFocus::TaskCancelCompose => {
                control(Role::Button, false, None, "Task composer action")
            }
            ActivityFocus::TaskAction(_) => {
                control(Role::Button, false, None, "Selected task action")
            }
            ActivityFocus::Lens(value) => {
                control(Role::RadioButton, self.lens == *value, None, "History lens")
            }
            ActivityFocus::MoreLenses => {
                control(Role::Button, false, None, "Show more history lenses")
            }
            ActivityFocus::Detail(DetailAction::UndoTask) => control(
                Role::Button,
                false,
                None,
                "Undo every affected record in this agent task; later collaborator edits are kept",
            ),
            ActivityFocus::Detail(_) => {
                control(Role::Button, false, None, "Selected history action")
            }
            ActivityFocus::ExplorerPane(value) => control(
                Role::Tab,
                self.explorer_pane == *value,
                None,
                "Explorer panel",
            ),
            ActivityFocus::Timeline => control(
                Role::List,
                true,
                None,
                "History entries; use arrow keys to navigate",
            ),
            ActivityFocus::ReplayBack | ActivityFocus::ReplayForward => control(
                Role::Button,
                false,
                None,
                "Move through workspace checkpoints",
            ),
            ActivityFocus::ReplayPlay => control(
                Role::Button,
                self.replay_playing,
                None,
                "Play or pause the workspace replay",
            ),
        })
    }

    fn accessibility_control_node(
        &self,
        focus: &ActivityFocus,
        index: usize,
        set_size: usize,
    ) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        if *focus == ActivityFocus::Timeline {
            return self.timeline.accessibility();
        }

        let label = self.focus_label(focus)?;
        let control = self.access_control(focus)?;
        let mut state = AccessState::new()
            .focusable(true)
            .focused(self.keyboard_focus.as_ref() == Some(focus));
        state = if control.role == Role::RadioButton {
            state.checked(control.selected)
        } else {
            state.selected(control.selected)
        };
        if let Some(expanded) = control.expanded {
            state = state.expanded(expanded);
        }
        Some(
            AccessNode::new(control.role)
                .with_name(label)
                .with_description(control.description)
                .with_state(state)
                .with_position(index.saturating_add(1), set_size),
        )
    }

    /// Semantic mirror of this immediate-mode overlay. Shell overlays are not
    /// backed by taffy `NodeId`s, so the app-level widget walker cannot infer
    /// this tree. The overlay accessibility bridge can publish this snapshot
    /// verbatim; state is derived from the same values used for paint and hit
    /// testing, preventing spoken and visible selection from drifting apart.
    pub fn accessibility_snapshot(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        let mut root = AccessNode::new(Role::Window)
            .with_name("Workspace activity")
            .with_description(match self.view {
                ActivityView::CatchUp => {
                    "A summary of outcomes since this device last viewed the workspace"
                }
                ActivityView::Tasks => {
                    "Durable workspace tasks with resource links, execution routes, and audit trails"
                }
                ActivityView::Explore => {
                    "The complete workspace history with filters and provenance"
                }
                ActivityView::Replay => {
                    "A causal replay of workspace layout and participant activity"
                }
            })
            .with_state(AccessState::new());
        let order = self.focus_order();
        let set_size = order.len();
        root.children = order
            .iter()
            .enumerate()
            .filter_map(|(index, focus)| self.accessibility_control_node(focus, index, set_size))
            .collect();
        if self.timeline_is_visible() {
            let mut history = AccessNode::new(Role::List).with_name("History entries");
            history.children = (0..self.timeline.visible_len().min(100))
                .filter_map(|index| self.timeline.visible_entry(index))
                .map(|entry| {
                    AccessNode::new(Role::ListItem)
                        .with_name(entry.summary.clone())
                        .with_description(format!("By {}", entry.author.name))
                })
                .collect();
            root.children.push(history);
        }
        if self.view == ActivityView::Tasks {
            let mut tasks = AccessNode::new(Role::List).with_name("Workspace tasks");
            tasks.children = self
                .tasks
                .iter()
                .map(|task| {
                    AccessNode::new(Role::ListItem)
                        .with_name(task.title.clone())
                        .with_description(task_accessibility_description(task, self.now()))
                })
                .collect();
            root.children.push(tasks);
        }
        if let Some(detail) = self.selection_detail() {
            root.children.push(
                AccessNode::new(Role::Group)
                    .with_name("Selected history detail")
                    .with_description(format!(
                        "{}; {} action{}; by {}",
                        detail.title,
                        detail.entry_ids.len(),
                        if detail.entry_ids.len() == 1 { "" } else { "s" },
                        detail.authors.join(", ")
                    )),
            );
        }
        if let Some(indexed) = self.selected_agent_run() {
            let run = &indexed.run;
            let mut description = format!(
                "Instruction: {}; assigned by {}; run by {}; reason: {}",
                run.instruction.as_deref().unwrap_or(&run.label),
                self.issuer_name(run),
                self.agent_name(run),
                run.decision.as_deref().unwrap_or("not recorded")
            );
            for step in &run.steps {
                let _ = write!(
                    description,
                    "; {} in {}: {}",
                    humanize_app_id(&step.app),
                    step.record,
                    step.summary
                );
            }
            root.children.push(
                AccessNode::new(Role::Group)
                    .with_name("Agent task review")
                    .with_description(description),
            );
        }
        root
    }

    pub fn accessibility_rect_for_path(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [0] => Some(self.sheet),
            [0, index] => self
                .focus_order()
                .get(*index)
                .and_then(|focus| self.rect_for_focus(focus)),
            [0, index, row] if self.focus_order().get(*index) == Some(&ActivityFocus::Timeline) => {
                self.timeline.accessibility_row_rect(*row)
            }
            _ => None,
        }
    }

    pub fn focus_accessibility_path(&mut self, path: &[usize]) -> bool {
        if let [0, index, row] = path
            && self.focus_order().get(*index) == Some(&ActivityFocus::Timeline)
        {
            self.keyboard_focus = Some(ActivityFocus::Timeline);
            self.timeline.set_focus(true);
            return self.timeline.focus_row(*row);
        }
        let [0, index] = path else {
            return false;
        };
        let Some(focus) = self.focus_order().get(*index).cloned() else {
            return false;
        };
        self.keyboard_focus = Some(focus);
        self.timeline
            .set_focus(self.keyboard_focus == Some(ActivityFocus::Timeline));
        true
    }

    fn rect_for_focus(&self, focus: &ActivityFocus) -> Option<Rect> {
        match focus {
            ActivityFocus::Scope(value) => self
                .scope_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::View(value) => self
                .view_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::Period(value) => self
                .period_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::Author(value) => self
                .author_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::MorePeople => self.author_more_rect,
            ActivityFocus::Resume => Some(self.resume_rect),
            ActivityFocus::Brief(value) => self
                .brief_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::WorkSessionBoundary(value) => self
                .boundary_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::MoreBriefs => self.brief_more_rect,
            ActivityFocus::NewTask => self.new_task_rect,
            ActivityFocus::Task(value) => self
                .task_rects
                .iter()
                .find_map(|(id, rect)| (id == value).then_some(*rect)),
            ActivityFocus::TaskRoute(value) => self
                .task_route_rects
                .iter()
                .find_map(|(route, rect)| (route == value).then_some(*rect)),
            ActivityFocus::TaskCreate => self.task_create_rect,
            ActivityFocus::TaskCancelCompose => self.task_cancel_rect,
            ActivityFocus::TaskAction(value) => self
                .task_action_rects
                .iter()
                .find_map(|(action, rect)| (action == value).then_some(*rect)),
            ActivityFocus::Lens(value) => self
                .lens_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::MoreLenses => self.lens_more_rect,
            ActivityFocus::Detail(value) => self
                .detail_action_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::ExplorerPane(value) => self
                .explorer_pane_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::Timeline => Some(self.timeline.rect()),
            ActivityFocus::ReplayBack => Some(self.replay_back_rect),
            ActivityFocus::ReplayPlay => Some(self.replay_play_rect),
            ActivityFocus::ReplayForward => Some(self.replay_forward_rect),
        }
    }

    /// Write (or remove, for an empty label) a name over a run of the
    /// room's changes, committed so it reaches peers through the same
    /// sync as the changes it names.
    pub fn apply_name(&mut self, tree: &mut WorkspaceTree, req: &GroupNameRequest) -> bool {
        let before = tree.doc.get_heads();
        if req.label.is_empty() {
            let Some(record) = req.record_id.as_deref() else {
                return false;
            };
            if !ui::history_groups::clear_name(&mut tree.doc, record) {
                return false;
            }
        } else {
            ui::history_groups::name_range(&mut tree.doc, &req.start, Some(&req.end), &req.label);
        }
        tree.commit_room_change();
        if tree.doc.get_heads() == before {
            return false;
        }
        self.last_heads.clear();
        self.refresh(tree);
        true
    }

    /// True while the pane's name field has the keyboard, so Escape
    /// cancels the field rather than closing the sheet.
    pub fn wants_keyboard(&self) -> bool {
        self.task_compose_open || self.timeline.wants_keyboard()
    }

    /// Rewrite the document to the state at `id`, as a **new forward
    /// change** — the versions rolled past stay in the graph.
    pub fn revert_to(&mut self, tree: &mut WorkspaceTree, id: &HistoryEntryId) -> bool {
        self.revert_to_with_records(tree, None, id)
    }

    fn revert_to_with_records(
        &mut self,
        tree: &mut WorkspaceTree,
        records: Option<&RecordStore>,
        id: &HistoryEntryId,
    ) -> bool {
        let origin = self.projected_origins.get(id).cloned();
        if let Some(WorkspaceHistoryId {
            origin: crate::shell::workspace_history::HistoryOrigin::Record { id: record, .. },
            change,
        }) = origin
        {
            return records.is_some_and(|records| {
                records
                    .with(&record, |doc| {
                        let Some(target) = ui::heads_for_entry(doc, &change) else {
                            return false;
                        };
                        let current = doc.get_heads();
                        if current == target {
                            return false;
                        }
                        ui::state::write_revert_patches(doc, &current, &target);
                        qdoc::history::commit_stamped(doc).is_some()
                    })
                    .unwrap_or(false)
            });
        }
        let native = origin.map_or(*id, |address| address.change);
        let Some(target) = ui::heads_for_entry(&mut tree.doc, &native) else {
            return false;
        };
        let current = tree.doc.get_heads();
        if current == target {
            return false;
        }
        ui::state::write_revert_patches(&mut tree.doc, &current, &target);
        tree.commit_room_change();
        if tree.doc.get_heads() == current {
            return false;
        }
        // Bring the read model back in line with what just changed
        // underneath it, so the revert shows up as the newest entry.
        tree.resync_cache_from_doc();
        self.last_heads.clear();
        self.refresh(tree);
        true
    }

    fn undo_band(
        &mut self,
        tree: &mut WorkspaceTree,
        records: Option<&RecordStore>,
        request: &BandUndoRequest,
    ) -> UndoApply {
        let positions = (
            self.timeline
                .entries()
                .iter()
                .position(|entry| entry.id == request.start),
            self.timeline
                .entries()
                .iter()
                .position(|entry| entry.id == request.end),
        );
        if let (Some(a), Some(b)) = positions {
            let (start_index, end_index) = (a.min(b), a.max(b));
            let mut expected = None;
            let Some(entries) = self.timeline.entries().get(start_index..=end_index) else {
                return UndoApply::default();
            };
            for entry in entries {
                let origin = self.projected_origins.get(&entry.id).map_or(
                    crate::shell::workspace_history::HistoryOrigin::WorkspaceTree,
                    |address| address.origin.clone(),
                );
                if expected.as_ref().is_some_and(|value| value != &origin) {
                    return UndoApply::default();
                }
                expected = Some(origin);
            }
        }
        let start = self.projected_origins.get(&request.start).cloned();
        let end = self.projected_origins.get(&request.end).cloned();
        match (start, end) {
            (Some(first), Some(last)) if first.origin == last.origin => {
                let run = ChangeRun::new(first.change, last.change);
                match first.origin {
                    crate::shell::workspace_history::HistoryOrigin::WorkspaceTree => {
                        let Ok(outcome) =
                            ui::revert_run(&mut tree.doc, &run, None, self.source.authors())
                        else {
                            return UndoApply::default();
                        };
                        if !outcome.wrote {
                            return UndoApply::default();
                        }
                        tree.commit_room_change();
                        tree.resync_cache_from_doc();
                        self.last_heads.clear();
                        self.refresh(tree);
                        UndoApply {
                            wrote: true,
                            partial: !outcome.is_complete(),
                        }
                    }
                    crate::shell::workspace_history::HistoryOrigin::Record { id, .. } => records
                        .map_or_else(UndoApply::default, |records| {
                            records
                                .with(&id, |doc| {
                                    let Ok(outcome) =
                                        ui::revert_run(doc, &run, None, self.source.authors())
                                    else {
                                        return UndoApply::default();
                                    };
                                    let committed = outcome.wrote
                                        && qdoc::history::commit_stamped(doc).is_some();
                                    UndoApply {
                                        wrote: committed,
                                        partial: committed && !outcome.is_complete(),
                                    }
                                })
                                .unwrap_or_default()
                        }),
                }
            }
            // Tree-only callers and pre-index tests retain native addressing.
            (None, None) => {
                let run = ChangeRun::new(request.start, request.end);
                let Ok(outcome) = ui::revert_run(&mut tree.doc, &run, None, self.source.authors())
                else {
                    return UndoApply::default();
                };
                if !outcome.wrote {
                    return UndoApply::default();
                }
                tree.commit_room_change();
                tree.resync_cache_from_doc();
                self.last_heads.clear();
                self.refresh(tree);
                UndoApply {
                    wrote: true,
                    partial: !outcome.is_complete(),
                }
            }
            _ => UndoApply::default(),
        }
    }

    pub fn invalidate(&mut self) {
        self.last_heads.clear();
    }
}

fn handled_outcome() -> RoomActivityOutcome {
    RoomActivityOutcome {
        handled: true,
        ..RoomActivityOutcome::default()
    }
}

fn task_actions(task: &WorkspaceTask) -> Vec<TaskAction> {
    let mut actions = Vec::new();
    if task.resource.is_some() {
        actions.push(TaskAction::OpenResource);
    }
    match task.status {
        WorkspaceTaskStatus::Queued | WorkspaceTaskStatus::Waiting => {
            if task.route == WorkspaceTaskRoute::Manual {
                actions.push(TaskAction::Start);
            }
            actions.push(TaskAction::Cancel);
        }
        WorkspaceTaskStatus::InProgress => {
            actions.push(TaskAction::Complete);
            actions.push(TaskAction::Cancel);
        }
        WorkspaceTaskStatus::Done | WorkspaceTaskStatus::Cancelled => {}
    }
    actions
}

fn draw_task_button(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    prominent: bool,
) {
    scene.fill(
        Fill::NonZero,
        transform,
        if prominent {
            theme.tokens.colors.accent[5]
        } else {
            theme.tokens.colors.lift(0.35)
        },
        None,
        &RoundedRect::from_rect(rect, 6.0),
    );
    let size = theme.tokens.typography.scale.sm;
    let (width, height) = tr.measure(label, size, None);
    tr.draw_text_weighted(
        scene,
        label,
        size,
        theme.tokens.colors.text_primary,
        at(
            transform,
            rect.center().x - f64::from(width) / 2.0,
            rect.center().y - f64::from(height) / 2.0,
        ),
        None,
        ui::text_render::TextWeight::Medium,
    );
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "task-card rendering needs the shared renderer context and explicit card data together to keep the visible and accessible states aligned"
)]
fn draw_task_card(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    task: &WorkspaceTask,
    rect: Rect,
    selected: bool,
    now: SystemTime,
    execution_notice: Option<&str>,
) {
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.lift(if selected { 0.55 } else { 0.3 }),
        None,
        &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.md)),
    );
    if selected {
        scene.stroke(
            &Stroke::new(1.5),
            transform,
            theme.tokens.colors.border_focus,
            None,
            &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.md)),
        );
    }
    let status = task.status.label();
    tr.draw_text_weighted(
        scene,
        status,
        theme.tokens.typography.scale.sm,
        if task.status.is_finished() {
            theme.tokens.colors.text_dim
        } else {
            theme.tokens.colors.accent[4]
        },
        at(transform, rect.x0 + 12.0, rect.y0 + 9.0),
        None,
        ui::text_render::TextWeight::Medium,
    );
    let route = task.route.label();
    let route_width = tr.measure(route, theme.tokens.typography.scale.sm, None).0;
    tr.draw_text(
        scene,
        route,
        theme.tokens.typography.scale.sm,
        theme.tokens.colors.text_dim,
        at(
            transform,
            rect.x1 - 12.0 - f64::from(route_width),
            rect.y0 + 9.0,
        ),
        None,
    );
    let title = tr.elide_to_width(
        &task.title,
        theme.tokens.typography.scale.base,
        text_width(rect.width() - 24.0),
    );
    tr.draw_text_weighted(
        scene,
        &title,
        theme.tokens.typography.scale.base,
        theme.tokens.colors.text_primary,
        at(transform, rect.x0 + 12.0, rect.y0 + 31.0),
        None,
        ui::text_render::TextWeight::Medium,
    );
    let assignee = task_assignee_label(task);
    let resource = task
        .resource
        .as_ref()
        .map(|resource| format!(" · {}", resource.label))
        .unwrap_or_default();
    let byline = tr.elide_to_width(
        &format!("{assignee}{resource}"),
        theme.tokens.typography.scale.sm,
        text_width(rect.width() - 24.0),
    );
    tr.draw_text(
        scene,
        &byline,
        theme.tokens.typography.scale.sm,
        theme.tokens.colors.text_dim,
        at(transform, rect.x0 + 12.0, rect.y0 + 55.0),
        None,
    );
    if selected {
        let mut audit_y = rect.y0 + 80.0;
        if let Some(notice) = execution_notice {
            tr.draw_text(
                scene,
                notice,
                theme.tokens.typography.scale.sm,
                theme.tokens.colors.accent[4],
                at(transform, rect.x0 + 12.0, audit_y),
                Some(text_width(rect.width() - 24.0)),
            );
            audit_y += 18.0;
        }
        tr.draw_text_weighted(
            scene,
            "AUDIT TRAIL",
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_dim,
            at(transform, rect.x0 + 12.0, audit_y),
            None,
            ui::text_render::TextWeight::Medium,
        );
        audit_y += 20.0;
        for event in task.audit.iter().rev().take(3) {
            let event_time = u64::try_from(event.at)
                .ok()
                .and_then(|secs| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs)))
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let line = task_audit_line(event, event_time, now);
            let line = tr.elide_to_width(
                &line,
                theme.tokens.typography.scale.sm,
                text_width(rect.width() - 32.0),
            );
            tr.draw_text(
                scene,
                &line,
                theme.tokens.typography.scale.sm,
                theme.tokens.colors.text_primary,
                at(transform, rect.x0 + 20.0, audit_y),
                None,
            );
            audit_y += 18.0;
        }
    }
}

fn task_assignee_label(task: &WorkspaceTask) -> String {
    task.assigned_agent.as_ref().map_or_else(
        || {
            format!(
                "Unassigned · created by {}",
                participant_display_label(&task.created_by)
            )
        },
        |agent| {
            // The card already owns the prominent AGENT badge, so reuse the
            // shared presentation while avoiding a redundant second suffix.
            let label = participant_display_label(agent);
            let name = label.strip_suffix(" · Agent").unwrap_or(&label);
            format!("▣ AGENT · {name}")
        },
    )
}

fn task_audit_line(
    event: &crate::shell::state::WorkspaceTaskAuditEvent,
    event_time: SystemTime,
    now: SystemTime,
) -> String {
    format!(
        "{} · {} · {}",
        participant_display_label(&event.actor),
        event.summary,
        format_relative_time(event_time, now)
    )
}

fn task_accessibility_description(task: &WorkspaceTask, now: SystemTime) -> String {
    let latest = task.audit.last().map(|event| {
        let event_time = u64::try_from(event.at)
            .ok()
            .and_then(|secs| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs)))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        task_audit_line(event, event_time, now)
    });
    let mut description = format!(
        "{}; {}; {}; {} audit events",
        task.status.label(),
        task.route.label(),
        task_assignee_label(task),
        task.audit.len()
    );
    if let Some(latest) = latest {
        let _ = write!(description, "; latest: {latest}");
    }
    description
}

fn hit_value<T: Clone>(rects: &[(T, Rect)], point: Point) -> Option<T> {
    rects
        .iter()
        .find(|(_, rect)| rect.contains(point))
        .map(|(value, _)| value.clone())
}

fn detail_provenance(detail: &HistorySelectionDetail) -> String {
    let people = detail.authors.join(", ");
    let resources = detail
        .resources
        .iter()
        .map(|resource| resource.label.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let facets = detail
        .facets
        .iter()
        .map(|facet| format!("{}: {}", facet.key, facet.value))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{} action{}\n{}\n{}{}",
        detail.entry_ids.len(),
        if detail.entry_ids.len() == 1 { "" } else { "s" },
        people,
        if resources.is_empty() {
            "No resource declared"
        } else {
            &resources
        },
        if facets.is_empty() {
            String::new()
        } else {
            format!("\n{facets}")
        },
    )
}

/// Turn Automerge's structural patch vocabulary into a small, stable set of
/// phrases suitable for the human-facing comparison preview. Resource-native
/// renderers can still use the complete before/after documents when opened.
fn comparison_change_labels(patches: &[automerge::Patch]) -> Vec<String> {
    let mut labels = Vec::new();
    for patch in patches {
        let label = match &patch.action {
            automerge::PatchAction::PutMap { key, conflict, .. } => {
                if *conflict {
                    format!("Resolved concurrent changes to {}", humanize_field(key))
                } else {
                    format!("Updated {}", humanize_field(key))
                }
            }
            automerge::PatchAction::DeleteMap { key } => {
                format!("Removed {}", humanize_field(key))
            }
            automerge::PatchAction::PutSeq { conflict, .. } => {
                if *conflict {
                    "Resolved concurrent list changes".to_string()
                } else {
                    "Updated a list item".to_string()
                }
            }
            automerge::PatchAction::Insert { values, .. } => plural(values.len(), "item", "Added"),
            automerge::PatchAction::SpliceText { .. } => "Edited text".to_string(),
            automerge::PatchAction::Increment { prop, value } => {
                let field = match prop {
                    Prop::Map(key) => humanize_field(key),
                    Prop::Seq(_) => "a counter".to_string(),
                };
                if *value < 0 {
                    format!("Decreased {field}")
                } else {
                    format!("Increased {field}")
                }
            }
            automerge::PatchAction::Conflict { prop } => match prop {
                Prop::Map(key) => format!("Concurrent changes to {}", humanize_field(key)),
                Prop::Seq(_) => "Concurrent list changes".to_string(),
            },
            automerge::PatchAction::DeleteSeq { length, .. } => plural(*length, "item", "Removed"),
            automerge::PatchAction::Mark { .. } => "Changed text formatting".to_string(),
        };
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    labels
}

fn humanize_field(field: &str) -> String {
    let words = field.replace(['_', '-'], " ");
    if words.trim().is_empty() {
        "content".to_string()
    } else {
        words
    }
}

#[allow(
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    reason = "replay layout derives all bounded workspace geometry in one pass; count conversions only affect display allocation"
)]
fn draw_replay_layout(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    bounds: Rect,
    frame: &WorkspaceReplayFrame,
) {
    let shown = frame.workspaces.len().min(4);
    if shown == 0 {
        tr.draw_text(
            scene,
            "The workspace did not exist at this checkpoint",
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_dim,
            at(transform, bounds.x0 + 16.0, bounds.y0 + 16.0),
            Some(text_width(bounds.width() - 32.0)),
        );
        return;
    }
    let gap = 10.0;
    let row_h =
        ((bounds.height() - 20.0 - gap * shown.saturating_sub(1) as f64) / shown as f64).max(54.0);
    let mut y = bounds.y0 + 10.0;
    for workspace in frame.workspaces.iter().take(shown) {
        let row = Rect::new(
            bounds.x0 + 10.0,
            y,
            bounds.x1 - 10.0,
            (y + row_h).min(bounds.y1 - 8.0),
        );
        tr.draw_text_weighted(
            scene,
            &workspace.name,
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_primary,
            at(transform, row.x0 + 6.0, row.y0 + 5.0),
            Some(text_width(row.width() - 12.0)),
            ui::text_render::TextWeight::Medium,
        );
        let groups = Rect::new(row.x0 + 4.0, row.y0 + 26.0, row.x1 - 4.0, row.y1 - 4.0);
        let total = workspace
            .groups
            .iter()
            .map(|group| u32::from(group.width_permille.max(1)))
            .sum::<u32>()
            .max(1);
        let mut x = groups.x0;
        for (index, group) in workspace.groups.iter().enumerate() {
            let share = f64::from(group.width_permille.max(1)) / f64::from(total);
            let x1 = if index.saturating_add(1) == workspace.groups.len() {
                groups.x1
            } else {
                (x + groups.width() * share).min(groups.x1)
            };
            let rect = Rect::new(x, groups.y0, x1 - 4.0, groups.y1);
            if rect.width() > 6.0 {
                let group_card = RoundedRect::from_rect(rect, 6.0);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.lift(0.35),
                    None,
                    &group_card,
                );
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    theme.tokens.colors.border_standard,
                    None,
                    &group_card,
                );
                let title = group
                    .tabs
                    .get(group.active)
                    .or_else(|| group.tabs.first())
                    .map_or("Empty window", String::as_str);
                tr.draw_text(
                    scene,
                    title,
                    theme.tokens.typography.scale.sm,
                    theme.tokens.colors.text_primary,
                    at(transform, rect.x0 + 8.0, rect.y0 + 8.0),
                    Some(text_width(rect.width() - 16.0)),
                );
                if group.tabs.len() > 1 && rect.height() >= 54.0 {
                    let mut tab_x = rect.x0 + 8.0;
                    let tab_y = rect.y0 + 31.0;
                    for (tab_index, tab) in group.tabs.iter().take(4).enumerate() {
                        let label = if tab_index == group.active {
                            format!("• {tab}")
                        } else {
                            tab.clone()
                        };
                        let width = (f64::from(
                            tr.measure(
                                &label,
                                (theme.tokens.typography.scale.sm - 2.0).max(9.0),
                                None,
                            )
                            .0,
                        ) + 12.0)
                            .min(96.0);
                        if tab_x + width > rect.x1 - 8.0 {
                            break;
                        }
                        let chip = RoundedRect::from_rect(
                            Rect::new(tab_x, tab_y, tab_x + width, tab_y + 20.0),
                            5.0,
                        );
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            if tab_index == group.active {
                                theme.tokens.colors.accent[5].with_alpha_factor(0.18)
                            } else {
                                theme.tokens.colors.lift(0.55)
                            },
                            None,
                            &chip,
                        );
                        tr.draw_text(
                            scene,
                            &label,
                            (theme.tokens.typography.scale.sm - 2.0).max(9.0),
                            theme.tokens.colors.text_dim,
                            at(transform, tab_x + 6.0, tab_y + 4.0),
                            Some(text_width(width - 12.0)),
                        );
                        tab_x += width + 5.0;
                    }
                    if group.tabs.len() > 4 && tab_x + 28.0 < rect.x1 {
                        tr.draw_text(
                            scene,
                            &format!("+{}", group.tabs.len().saturating_sub(4)),
                            (theme.tokens.typography.scale.sm - 2.0).max(9.0),
                            theme.tokens.colors.text_dim,
                            at(transform, tab_x + 2.0, tab_y + 4.0),
                            None,
                        );
                    }
                }
            }
            x = x1;
        }
        y += row_h + gap;
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "the bounded replay-frame and pointer counts only determine visual trail coordinates"
)]
fn draw_presence_trail(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    bounds: Rect,
    frames: &[WorkspaceReplayFrame],
    current: usize,
) {
    tr.draw_text(
        scene,
        "Presence trail",
        (theme.tokens.typography.scale.sm - 1.0).max(10.0),
        theme.tokens.colors.text_dim,
        at(transform, bounds.x0, bounds.y0 + 2.0),
        None,
    );
    let start = current.saturating_sub(8);
    let slice = frames.get(start..=current).unwrap_or(&[]);
    let x0 = bounds.x0 + 92.0;
    let available = (bounds.x1 - x0).max(1.0);
    let step = available / slice.len().max(1) as f64;
    for (index, frame) in slice.iter().enumerate() {
        let x = x0 + step * (index as f64 + 0.5);
        if frame.pointers.is_empty() {
            let (r, g, b) = ui::widget::history::hue_to_rgb(frame.author.avatar_hue);
            scene.fill(
                Fill::NonZero,
                transform,
                vello::peniko::Color::from_rgba8(r, g, b, 255),
                None,
                &vello::kurbo::Circle::new(Point::new(x, bounds.y0 + 9.0), 5.0),
            );
        } else {
            for (person, pointer) in frame.pointers.iter().take(3).enumerate() {
                let (r, g, b) = pointer.color;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    vello::peniko::Color::from_rgba8(r, g, b, 255),
                    None,
                    &vello::kurbo::Circle::new(
                        Point::new(x, bounds.y0 + 5.0 + person as f64 * 5.0),
                        3.5,
                    ),
                );
            }
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::cast_precision_loss,
    reason = "presence-trail painting receives the shared renderer context and uses bounded frame and pointer counts only for display geometry"
)]
fn draw_replay_pointers(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    canvas: Rect,
    frame: &WorkspaceReplayFrame,
    viewport: Rect,
) {
    let source_w = (viewport.x1 + SHEET_INSET).max(1.0);
    let source_h = (viewport.y1 + SHEET_INSET).max(1.0);
    for pointer in frame
        .pointers
        .iter()
        .filter(|pointer| pointer.desktop_space)
    {
        let px = (f64::from(pointer.point.0) / source_w).clamp(0.0, 1.0);
        let py = (f64::from(pointer.point.1) / source_h).clamp(0.0, 1.0);
        let point = Point::new(
            canvas.x0 + 10.0 + px * (canvas.width() - 20.0),
            canvas.y0 + 10.0 + py * (canvas.height() - 20.0),
        );
        let (r, g, b) = pointer.color;
        let color = vello::peniko::Color::from_rgba8(r, g, b, 255);
        let cursor =
            vello::kurbo::BezPath::from_svg("M0 0 L0 18 L5 13 L9 21 L13 19 L9 11 L17 11 Z")
                .unwrap_or_default();
        scene.fill(
            Fill::NonZero,
            transform.then_translate(point.to_vec2()),
            color,
            None,
            &cursor,
        );
        tr.draw_text(
            scene,
            &pointer.name,
            (theme.tokens.typography.scale.sm - 2.0).max(9.0),
            color,
            at(transform, point.x + 14.0, point.y + 4.0),
            Some(text_width(100.0)),
        );
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    reason = "brief-card rendering keeps its geometry, summary, and accessible label in one retained pass"
)]
fn draw_brief_card(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    brief: &ActivityBrief,
    rect: Rect,
    expanded: bool,
    now: SystemTime,
) {
    let rail_x = rect.x0 + 20.0;
    scene.stroke(
        &Stroke::new(2.0),
        transform,
        theme.tokens.colors.border_standard,
        None,
        &Line::new((rail_x, rect.y0), (rail_x, rect.y1 + 8.0)),
    );
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.surfaces.raised,
        None,
        &vello::kurbo::Circle::new((rail_x, rect.y0 + 18.0), 8.0),
    );
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.accent[4],
        None,
        &vello::kurbo::Circle::new((rail_x, rect.y0 + 18.0), 4.0),
    );
    let content_x = rect.x0 + 48.0;
    let width = text_width(rect.x1 - content_x - 12.0);
    let title_lines = tr.clamp_to_lines(&brief.title, theme.tokens.typography.scale.base, width, 2);
    for (index, title) in title_lines.iter().enumerate() {
        tr.draw_text_weighted(
            scene,
            title,
            theme.tokens.typography.scale.base,
            theme.tokens.colors.text_primary,
            at(transform, content_x, rect.y0 + 10.0 + index as f64 * 18.0),
            None,
            ui::text_render::TextWeight::Medium,
        );
    }
    let title_extra = 18.0 * title_lines.len().saturating_sub(1) as f64;
    let byline_y = rect.y0 + 34.0 + title_extra;
    let byline_x = draw_brief_author_stack(
        scene,
        transform,
        tr,
        theme,
        &brief.authors,
        content_x,
        byline_y,
    );
    let byline_width = text_width((rect.x1 - 12.0 - byline_x).max(0.0));
    let authors = brief
        .authors
        .iter()
        .map(|author| author.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let byline = tr.elide_to_width(&authors, theme.tokens.typography.scale.sm, byline_width);
    tr.draw_text(
        scene,
        &byline,
        theme.tokens.typography.scale.sm,
        theme.tokens.colors.text_dim,
        at(transform, byline_x, byline_y),
        None,
    );
    tr.draw_text(
        scene,
        &format_activity_range(brief.first_at, brief.last_at, now),
        theme.tokens.typography.scale.sm,
        theme.tokens.colors.text_dim,
        at(transform, content_x, rect.y0 + 56.0 + title_extra),
        Some(width),
    );
    let mut provenance = brief.apps.join(", ");
    if !brief.resources.is_empty() {
        if !provenance.is_empty() {
            provenance.push_str(" · ");
        }
        provenance.push_str(&brief.resources.join(", "));
    }
    if provenance.is_empty() {
        provenance = if brief.changes == 1 {
            "1 underlying action".to_string()
        } else {
            format!("{} underlying actions", brief.changes)
        };
    }
    let provenance = tr.elide_to_width(&provenance, theme.tokens.typography.scale.sm, width);
    tr.draw_text(
        scene,
        &provenance,
        theme.tokens.typography.scale.sm,
        theme.tokens.colors.text_dim,
        at(transform, content_x, rect.y0 + 76.0 + title_extra),
        None,
    );
    if expanded {
        let mut action_y = rect.y0 + 100.0 + title_extra;
        // Always reserve the final line for the route into Full history.
        // Compact windows show as many complete action rows as fit instead
        // of dropping the expanded outcome altogether.
        for action in &brief.actions {
            if action_y + 24.0 > rect.y1 - 28.0 {
                break;
            }
            let (r, g, b) = ui::widget::history::hue_to_rgb(action.author.avatar_hue);
            let initial = action
                .author
                .name
                .chars()
                .next()
                .unwrap_or('\u{2022}')
                .to_uppercase()
                .collect::<String>();
            ui::widget::Avatar::draw_at(
                scene,
                transform,
                Rect::new(
                    content_x + 8.0,
                    action_y - 2.0,
                    content_x + 24.0,
                    action_y + 14.0,
                ),
                &initial,
                vello::peniko::Color::from_rgba8(r, g, b, 255),
                theme,
                tr,
            );
            let action_line = format!("{} · {}", action.author.name, action.summary);
            let action_line = tr.elide_to_width(
                &action_line,
                theme.tokens.typography.scale.sm,
                text_width(rect.x1 - content_x - 44.0),
            );
            tr.draw_text(
                scene,
                &action_line,
                theme.tokens.typography.scale.sm,
                theme.tokens.colors.text_primary,
                at(transform, content_x + 32.0, action_y),
                None,
            );
            tr.draw_text(
                scene,
                &format_activity_timestamp(action.timestamp, now),
                (theme.tokens.typography.scale.sm - 1.0).max(10.0),
                theme.tokens.colors.text_dim,
                at(transform, content_x + 32.0, action_y + 13.0),
                None,
            );
            action_y += 24.0;
        }
        tr.draw_text(
            scene,
            "Open in full history  →",
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.accent[4],
            at(transform, content_x, (action_y + 6.0).min(rect.y1 - 12.0)),
            Some(width),
        );
        return;
    }
    tr.draw_text(
        scene,
        "Show the underlying actions  ↓",
        theme.tokens.typography.scale.sm,
        theme.tokens.colors.text_dim,
        at(transform, content_x, rect.y0 + 96.0 + title_extra),
        Some(width),
    );
}

fn activity_day_label(timestamp: SystemTime, now: SystemTime) -> String {
    use chrono::{DateTime, Local};
    let timestamp: DateTime<Local> = timestamp.into();
    let now: DateTime<Local> = now.into();
    let today = now.date_naive();
    let yesterday = today.pred_opt().unwrap_or(today);
    if timestamp.date_naive() == today {
        "Today".to_string()
    } else if timestamp.date_naive() == yesterday {
        "Yesterday".to_string()
    } else {
        timestamp.format("%a, %-d %b").to_string()
    }
}

fn format_activity_timestamp(timestamp: SystemTime, now: SystemTime) -> String {
    use chrono::{DateTime, Local};
    let local: DateTime<Local> = timestamp.into();
    format!(
        "{}, {}",
        activity_day_label(timestamp, now),
        local.format("%H:%M")
    )
}

fn format_activity_range(first: SystemTime, last: SystemTime, now: SystemTime) -> String {
    use chrono::{DateTime, Local};
    if first == last {
        return format_activity_timestamp(last, now);
    }
    let first_local: DateTime<Local> = first.into();
    let last_local: DateTime<Local> = last.into();
    let minutes = last
        .duration_since(first)
        .unwrap_or_default()
        .as_secs()
        .saturating_add(30)
        / 60;
    let duration = if minutes >= 60 {
        let hours = minutes / 60;
        let remainder = minutes % 60;
        if remainder == 0 {
            format!("{hours} hr")
        } else {
            format!("{hours} hr {remainder} min")
        }
    } else {
        format!("{} min", minutes.max(1))
    };
    if first_local.date_naive() == last_local.date_naive() {
        format!(
            "{}, {} – {} ({duration})",
            activity_day_label(first, now),
            first_local.format("%H:%M"),
            last_local.format("%H:%M")
        )
    } else {
        format!(
            "{} – {} ({duration})",
            format_activity_timestamp(first, now),
            format_activity_timestamp(last, now)
        )
    }
}

fn draw_brief_author_stack(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    authors: &[ui::widget::history::HistoryAuthor],
    x: f64,
    y: f64,
) -> f64 {
    let visible = authors.len().min(2);
    for (index, author) in authors.iter().take(visible).enumerate() {
        let avatar_x = x + index as f64 * 12.0;
        let (r, g, b) = ui::widget::history::hue_to_rgb(author.avatar_hue);
        let initial = author
            .name
            .chars()
            .next()
            .unwrap_or('\u{2022}')
            .to_uppercase()
            .collect::<String>();
        ui::widget::Avatar::draw_at(
            scene,
            transform,
            Rect::new(avatar_x, y - 2.0, avatar_x + 18.0, y + 16.0),
            &initial,
            vello::peniko::Color::from_rgba8(r, g, b, 255),
            theme,
            tr,
        );
    }
    if authors.len() > visible {
        let badge_x = x + visible as f64 * 12.0;
        ui::widget::Avatar::draw_at(
            scene,
            transform,
            Rect::new(badge_x, y - 2.0, badge_x + 18.0, y + 16.0),
            &format!("+{}", authors.len() - visible),
            theme.tokens.colors.surfaces.raised,
            theme,
            tr,
        );
    }
    let badges = visible + usize::from(authors.len() > visible);
    if badges == 0 {
        x
    } else {
        x + badges as f64 * 12.0 + 10.0
    }
}

/// Chips that would overflow the row are neither painted nor returned,
/// so a click cannot select one the user cannot see.
fn draw_chip_row<T: Clone>(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    row: Rect,
    items: &[(T, String)],
    is_active: impl Fn(&T) -> bool,
) -> Vec<(T, Rect)> {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let size = (tokens.typography.scale.sm - 1.0).max(10.0);
    let chip_h = (row.height() - 8.0).max(18.0);
    let cy = row.center().y;
    let mut out = Vec::with_capacity(items.len());
    let mut x = row.x0;
    let mut dropped = 0usize;
    for (value, label) in items {
        // Reserve room for a "+N" overflow marker so it never itself overflows.
        let (lw, lh) = tr.measure(label, size, None);
        let w = f64::from(lw) + 16.0;
        if x + w > row.x1 - 28.0 {
            dropped = dropped.saturating_add(1);
            continue;
        }
        let rect = Rect::new(x, cy - chip_h / 2.0, x + w, cy + chip_h / 2.0);
        let active = is_active(value);
        let shape = RoundedRect::from_rect(rect, chip_h / 2.0);
        scene.fill(
            Fill::NonZero,
            transform,
            if active {
                colors.accent[5]
            } else {
                colors.lift(0.35)
            },
            None,
            &shape,
        );
        tr.draw_text(
            scene,
            label,
            size,
            if active {
                colors.text_primary
            } else {
                colors.text_dim
            },
            at(
                transform,
                rect.x0 + (rect.width() - f64::from(lw)) / 2.0,
                rect.y0 + (rect.height() - f64::from(lh)) / 2.0,
            ),
            None,
        );
        out.push((value.clone(), rect));
        x += w + 6.0;
    }
    if dropped > 0 {
        let more = format!("+{dropped}");
        let (mw, mh) = tr.measure(&more, size, None);
        tr.draw_text(
            scene,
            &more,
            size,
            colors.text_dim,
            at(transform, row.x1 - f64::from(mw), cy - f64::from(mh) / 2.0),
            None,
        );
    }
    out
}

struct ChipPage<T> {
    items: Vec<(T, Rect)>,
    more_rect: Option<Rect>,
    shown: usize,
}

fn draw_chip_row_paginated<T: Clone>(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    row: Rect,
    items: &[(T, String)],
    is_active: impl Fn(&T) -> bool,
) -> ChipPage<T> {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let size = (tokens.typography.scale.sm - 1.0).max(10.0);
    let chip_h = (row.height() - 8.0).max(18.0);
    let cy = row.center().y;
    let mut out = Vec::with_capacity(items.len());
    let mut x = row.x0;
    for (value, label) in items {
        let (lw, lh) = tr.measure(label, size, None);
        let w = f64::from(lw) + 16.0;
        if x + w > row.x1 - 58.0 {
            break;
        }
        let rect = Rect::new(x, cy - chip_h / 2.0, x + w, cy + chip_h / 2.0);
        scene.fill(
            Fill::NonZero,
            transform,
            if is_active(value) {
                colors.accent[5]
            } else {
                colors.lift(0.35)
            },
            None,
            &RoundedRect::from_rect(rect, chip_h / 2.0),
        );
        tr.draw_text(
            scene,
            label,
            size,
            if is_active(value) {
                colors.text_primary
            } else {
                colors.text_dim
            },
            at(
                transform,
                rect.x0 + (rect.width() - f64::from(lw)) / 2.0,
                rect.y0 + (rect.height() - f64::from(lh)) / 2.0,
            ),
            None,
        );
        out.push((value.clone(), rect));
        x += w + 6.0;
    }
    let shown = out.len();
    let more_rect = if shown < items.len() {
        let rect = Rect::new(row.x1 - 52.0, cy - chip_h / 2.0, row.x1, cy + chip_h / 2.0);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.lift(0.35),
            None,
            &RoundedRect::from_rect(rect, chip_h / 2.0),
        );
        tr.draw_text(
            scene,
            "More…",
            size,
            colors.text_primary,
            at(transform, rect.x0 + 8.0, rect.y0 + 5.0),
            None,
        );
        Some(rect)
    } else {
        None
    };
    ChipPage {
        items: out,
        more_rect,
        shown,
    }
}

fn draw_more_row(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    bounds: Rect,
    y: f64,
    label: &str,
) -> Option<Rect> {
    if y + 28.0 > bounds.y1 {
        return None;
    }
    let rect = Rect::new(bounds.x0 + 6.0, y, bounds.x1 - 6.0, y + 28.0);
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.lift(0.35),
        None,
        &RoundedRect::from_rect(rect, 6.0),
    );
    tr.draw_text(
        scene,
        label,
        theme.tokens.typography.scale.sm,
        theme.tokens.colors.text_primary,
        at(transform, rect.x0 + 8.0, rect.y0 + 6.0),
        Some(text_width(rect.width() - 16.0)),
    );
    Some(rect)
}

// =====================================================================
// Room vocabulary
// =====================================================================

/// Rewrite each row's summary in the room's own words where nameable,
/// else keeps [`HistorySource`]'s generic one. Returns, per entry, the
/// workspace the change belongs to — `None` for several or none.
fn describe_room_changes(
    doc: &mut automerge::AutoCommit,
    entries: &mut [HistoryEntry],
) -> Vec<Option<u64>> {
    let mut tags: Vec<Option<u64>> = vec![None; entries.len()];
    if entries.is_empty() {
        return tags;
    }
    // Indexed so a long-lived room avoids a linear scan per change:
    // `deps -> [hash]` isolates exactly that change.
    let mut by_id: std::collections::HashMap<HistoryEntryId, usize> =
        std::collections::HashMap::with_capacity(entries.len());
    for (i, e) in entries.iter().enumerate() {
        by_id.insert(e.id, i);
    }
    let changes: Vec<(HistoryEntryId, Vec<ChangeHash>, ChangeHash)> = doc
        .get_changes(&[])
        .into_iter()
        .map(|c| {
            (
                ui::history_source::entry_id_from_hash(&c.hash().to_string()),
                c.deps().to_vec(),
                c.hash(),
            )
        })
        .collect();

    for (id, deps, hash) in changes {
        let Some(&i) = by_id.get(&id) else {
            continue;
        };
        let patches = doc.diff(&deps, &[hash]);
        if let Some(tag) = tags.get_mut(i) {
            *tag = workspace_touched(doc, &patches, hash);
        }
        if let Some((summary, op_class)) = describe_patches(&patches)
            && let Some(entry) = entries.get_mut(i)
        {
            if entry.context.is_none() {
                entry.summary = summary;
            }
            entry.op_class = op_class;
        }
    }
    tags
}

/// The workspace a change belongs to, or `None` for more than one or
/// none. Read **at the change's own heads**: a cache flush re-inserts
/// every entry, so a historical object may not exist at the current state.
fn workspace_touched(
    doc: &automerge::AutoCommit,
    patches: &[automerge::Patch],
    hash: ChangeHash,
) -> Option<u64> {
    let heads = [hash];
    let mut found: Option<u64> = None;
    for p in patches {
        let entry_obj = match p.path.len() {
            0 | 1 => continue, // the document root: not a workspace's change
            2 => p.obj.clone(),
            _ => {
                let Some((obj, _)) = p.path.get(2) else {
                    continue;
                };
                obj.clone()
            }
        };
        // Unrecognised layout otherwise; guessing would be worse than declining.
        let Some((_, root_prop)) = p.path.first() else {
            continue;
        };
        if !matches!(root_prop, Prop::Map(k) if k == "workspaces") {
            continue;
        }
        let Some((_, seq_prop)) = p.path.get(1) else {
            continue;
        };
        if !matches!(seq_prop, Prop::Seq(_)) {
            continue;
        }
        let id = match doc.get_at(&entry_obj, "id", &heads) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Uint(n) => *n,
                _ => continue,
            },
            _ => continue,
        };
        match found {
            None => found = Some(id),
            // Two workspaces in one change — a whole-tree flush. It is nobody's
            // room in particular.
            Some(prev) if prev != id => return None,
            Some(_) => {}
        }
    }
    found
}

/// Running counts [`describe_patches`] accumulates one patch at a time,
/// then turns into a single room-event description. Split out from
/// [`describe_patches`] purely to keep each step under the line-count
/// guard — the accumulation and the priority cascade are still one
/// pipeline, just two named steps of it.
#[derive(Default)]
struct PatchCounts {
    grants: Vec<String>,
    revokes: usize,
    names: Vec<String>,
    owner_set: bool,
    room_set: bool,
    tabs_added: usize,
    tabs_removed: usize,
    tab_titles: Vec<String>,
    groups_moved: usize,
    workspaces_added: usize,
    workspaces_removed: usize,
    geometry: usize,
    active_switched: bool,
}

impl PatchCounts {
    fn record(&mut self, p: &automerge::Patch) {
        let container = path_tail(p);
        match (&p.action, container.as_str()) {
            (automerge::PatchAction::PutMap { key, value, .. }, "capabilities") => {
                let _ = key;
                if let Value::Scalar(s) = &value.0
                    && let ScalarValue::Bytes(blob) = s.as_ref()
                    && let Ok(cap) = serde_json::from_slice::<CapOverride>(blob)
                {
                    self.grants.push(describe_capability(&cap));
                } else {
                    self.grants.push("a capability".to_string());
                }
            }
            (automerge::PatchAction::DeleteMap { .. }, "capabilities") => {
                self.revokes = self.revokes.saturating_add(1);
            }
            (automerge::PatchAction::PutMap { key, value, .. }, _) if key == "name" => {
                if let Some(n) = scalar_string(&value.0) {
                    self.names.push(n);
                }
            }
            (automerge::PatchAction::PutMap { key, .. }, _) if key == "owner" => {
                self.owner_set = true;
            }
            (automerge::PatchAction::PutMap { key, .. }, _) if key == "room" => {
                self.room_set = true;
            }
            (automerge::PatchAction::PutMap { key, value, .. }, _) if key == "title" => {
                if let Some(t) = scalar_string(&value.0)
                    && !t.is_empty()
                {
                    self.tab_titles.push(t);
                }
            }
            (automerge::PatchAction::PutMap { key, .. }, _)
                if key == "maximized" || key == "width_pixels" =>
            {
                self.geometry = self.geometry.saturating_add(1);
            }
            (automerge::PatchAction::PutMap { key, .. }, _) if key == "active" => {
                self.active_switched = true;
            }
            (automerge::PatchAction::Insert { values, .. }, "tabs") => {
                self.tabs_added = self.tabs_added.saturating_add(values.len());
            }
            (automerge::PatchAction::DeleteSeq { length, .. }, "tabs") => {
                self.tabs_removed = self.tabs_removed.saturating_add(*length);
            }
            (automerge::PatchAction::Insert { values, .. }, "window_groups") => {
                self.groups_moved = self.groups_moved.saturating_add(values.len());
            }
            (automerge::PatchAction::DeleteSeq { length, .. }, "window_groups") => {
                self.groups_moved = self.groups_moved.saturating_add(*length);
            }
            (automerge::PatchAction::Insert { values, .. }, "workspaces") => {
                self.workspaces_added = self.workspaces_added.saturating_add(values.len());
            }
            (automerge::PatchAction::DeleteSeq { length, .. }, "workspaces") => {
                self.workspaces_removed = self.workspaces_removed.saturating_add(*length);
            }
            _ => {}
        }
    }

    /// The highest-priority signal wins — governance beats structural
    /// noise, and a whole-list rewrite (`seed_doc_from_cache`) is
    /// recognised before the per-field cases, or a flush that granted
    /// nothing reads as "granted 4 capabilities".
    fn describe(&self) -> Option<(String, OpClass)> {
        if self.workspaces_removed > 0 && self.workspaces_added == 0 {
            return Some((
                plural(self.workspaces_removed, "workspace", "Removed"),
                OpClass::Tombstone,
            ));
        }
        if self.workspaces_added > 0 {
            let s = match self.names.len() {
                0 => plural(self.workspaces_added, "workspace", "Recorded"),
                1 => format!(
                    "Recorded workspace \u{201c}{}\u{201d}",
                    self.names.first().map_or("", String::as_str)
                ),
                n => format!("Recorded {n} workspaces: {}", join_titles(&self.names)),
            };
            return Some((s, OpClass::Splice));
        }

        if !self.grants.is_empty() {
            let s = if self.grants.len() == 1 {
                format!("Granted {}", self.grants.first().map_or("", String::as_str))
            } else {
                format!("Granted {} capabilities", self.grants.len())
            };
            return Some((s, OpClass::Govern));
        }
        if self.revokes > 0 {
            let s = if self.revokes == 1 {
                "Revoked a capability".to_string()
            } else {
                format!("Revoked {} capabilities", self.revokes)
            };
            return Some((s, OpClass::Govern));
        }
        if self.owner_set {
            return Some(("Set the workspace owner".to_string(), OpClass::Govern));
        }
        if self.room_set {
            return Some((
                "Assigned the workspace identity".to_string(),
                OpClass::Govern,
            ));
        }
        if let Some(name) = self.names.first() {
            let s = if name.is_empty() {
                "Renamed the workspace".to_string()
            } else {
                format!("Named the workspace \u{201c}{name}\u{201d}")
            };
            return Some((s, OpClass::EditMeta));
        }
        if self.tabs_added > 0 {
            let s = if self.tab_titles.is_empty() {
                plural(self.tabs_added, "window", "Opened")
            } else {
                format!("Opened {}", join_titles(&self.tab_titles))
            };
            return Some((s, OpClass::Splice));
        }
        if self.tabs_removed > 0 {
            return Some((
                plural(self.tabs_removed, "window", "Closed"),
                OpClass::Tombstone,
            ));
        }
        if self.groups_moved > 0 {
            return Some(("Rearranged the strip".to_string(), OpClass::Move));
        }
        if self.geometry > 0 {
            return Some(("Resized a window group".to_string(), OpClass::EditMeta));
        }
        if self.active_switched {
            return Some((
                // This also feeds the fixed-width top-bar activity summary;
                // "active" adds no useful distinction there and caused the
                // workspace-switch confirmation to truncate at accessibility
                // text sizes.
                "Switched workspace".to_string(),
                OpClass::EditMeta,
            ));
        }
        None
    }
}

/// Map a change's patches onto a room event, or `None` when nothing in it is
/// recognised.
fn describe_patches(patches: &[automerge::Patch]) -> Option<(String, OpClass)> {
    let mut counts = PatchCounts::default();
    for p in patches {
        counts.record(p);
    }
    counts.describe()
}

/// The map key of the object a patch lands in — `"capabilities"`, `"tabs"`,
/// `"workspaces"` — or an empty string when the patch targets the root or a
/// list element whose parent has no name.
fn path_tail(p: &automerge::Patch) -> String {
    for (_, prop) in p.path.iter().rev() {
        if let Prop::Map(name) = prop {
            return name.clone();
        }
    }
    String::new()
}

fn scalar_string(v: &Value<'_>) -> Option<String> {
    match v {
        Value::Scalar(s) => match s.as_ref() {
            ScalarValue::Str(t) => Some(t.to_string()),
            _ => None,
        },
        Value::Object(_) => None,
    }
}

/// A capability in one short phrase: what, over what, to whom.
fn describe_capability(cap: &CapOverride) -> String {
    let verb = match cap.verb {
        Verb::Read => "read",
        Verb::Comment => "comment",
        Verb::Write => "write",
        Verb::Invite => "invite",
        Verb::Admin => "admin",
    };
    let effect = match cap.effect {
        Effect::Grant => "",
        Effect::Restrict => "restricted ",
    };
    format!(
        "{effect}{verb} on {} to {}",
        describe_scope(&cap.scope),
        describe_subject(&cap.subject)
    )
}

fn describe_scope(scope: &Scope) -> String {
    match scope {
        Scope::Workspace | Scope::Room(_) => "this workspace".to_string(),
        Scope::App(a) => a.clone(),
        Scope::ResourceType(k) => format!("every {k:?}"),
        Scope::ResourceInstance(_) => "one artifact".to_string(),
        Scope::FsPath(p) => p.clone(),
        // Record authority intentionally has no path: it follows the file
        // across rename/move and the stable id is not useful activity copy.
        Scope::FsRecord(_) => "one shared file".to_string(),
        Scope::Device(d) => d.clone(),
        Scope::NetHost(h) => format!("network {h}"),
        Scope::Action { app, action } => format!("{app}:{action}"),
    }
}

fn describe_subject(subject: &Subject) -> String {
    match subject {
        Subject::Role(r) => format!("{r:?}s"),
        Subject::User(_) => "a person".to_string(),
        Subject::Principal(p) => describe_principal(p),
    }
}

fn describe_principal(p: &Principal) -> String {
    match p {
        Principal::Person(id) => short(id),
        Principal::App(a) => a.clone(),
        Principal::Agent(a) => format!("agent {a}"),
        Principal::Token(_) => "an invite token".to_string(),
        Principal::Room(_) => "this workspace".to_string(),
    }
}

/// A person id is either a readable account name or a 64-character public key.
/// Show the first whole, and enough of the second to compare against another
/// one — chopping every id to eight characters turned `test-owner` into
/// `test-own`.
fn short(id: &str) -> String {
    const READABLE_MAX: usize = 24;
    if id.chars().count() <= READABLE_MAX {
        return id.to_string();
    }
    let mut out: String = id.chars().take(12).collect();
    out.push('\u{2026}');
    out
}

fn plural(n: usize, noun: &str, verb: &str) -> String {
    if n == 1 {
        format!("{verb} 1 {noun}")
    } else {
        format!("{verb} {n} {noun}s")
    }
}

/// Name up to two windows, then count the rest — the row has a line, not a
/// paragraph.
fn join_titles(titles: &[String]) -> String {
    let first = || titles.first().map_or("", String::as_str);
    let second = || titles.get(1).map_or("", String::as_str);
    match titles.len() {
        0 => String::new(),
        1 => first().to_string(),
        2 => format!("{} and {}", first(), second()),
        n => format!("{}, {} and {} more", first(), second(), n.saturating_sub(2)),
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    //! Every fixture mutates the room through the *production* mirrors
    //! and reads the pane back — nothing writes a synthetic `HistoryEntry`.

    use super::*;
    use crate::shell::state::{WorkspaceId, WorkspaceTree};
    use automerge::ActorId;
    use automerge::transaction::{CommitOptions, Transactable};
    use collab_types::caps::{CapOverride, Scope, Subject, Verb};
    use ui::widget::op_class_is_revertible;

    /// A plausible half-hour: owner, one grant, one revoke, one rename,
    /// one window opened.
    fn busy_room() -> (WorkspaceTree, WorkspaceId, String) {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let user: String = "test-owner".to_string();
        tree.adopt_ownerless_workspaces(&user);
        let ws_id = tree.active;

        let granted = tree
            .grant_in_workspace(
                ws_id,
                &user,
                CapOverride::grant(
                    Subject::app("files"),
                    Scope::FsPath("/home/quinten/notes".to_string()),
                    Verb::Read,
                ),
            )
            .expect("the owner may grant in their own room");
        tree.grant_in_workspace(
            ws_id,
            &user,
            CapOverride::grant(
                Subject::app("terminal"),
                Scope::Device("camera".to_string()),
                Verb::Read,
            ),
        )
        .expect("the owner may grant in their own room");
        assert!(tree.revoke_in_workspace(ws_id, &user, &granted));

        if let Some(ws) = tree.workspaces.iter_mut().find(|w| w.id == ws_id) {
            ws.name = "Design review".to_string();
        }
        tree.seed_doc_from_cache();
        (tree, ws_id, user)
    }

    #[test]
    fn workspace_switch_summary_fits_the_chrome_activity_line() {
        let summary = PatchCounts {
            active_switched: true,
            ..Default::default()
        }
        .describe()
        .expect("workspace switch is activity")
        .0;
        assert_eq!(summary, "Switched workspace");
        assert!(
            summary.len() <= 18,
            "fixed-width topbar summaries must stay concise: {summary}"
        );
    }

    #[test]
    fn persisted_effect_receipt_reappears_in_workspace_activity_without_revert() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let invocation = tilo_actions::InvocationId::new();
        assert!(
            tree.append_workspace_effect_receipt(
                tree.active,
                invocation,
                "ada",
                tilo_actions::ActionContext::for_resource(
                    "Export",
                    "document:brief",
                    "document",
                    "Release brief",
                )
                .with_app("org.tilo.Writer", "Writer"),
                tilo_actions::EffectReceiptStatus::Succeeded,
            )
        );
        let persisted = serde_json::to_vec(&tree).expect("persist workspace tree");
        let mut restored: WorkspaceTree =
            serde_json::from_slice(&persisted).expect("restore workspace tree");
        restored.seed_doc_from_cache();

        let mut pane = RoomActivityPane::new();
        pane.refresh(&mut restored);
        let entry = pane
            .entries()
            .iter()
            .find(|entry| entry.id == HistoryEntryId::from_bytes(*invocation.0.as_bytes()))
            .expect("receipt is visible after restart");
        assert_eq!(entry.summary, "Completed · Export · Release brief");
        assert!(!entry.is_revertible());
    }

    fn pane_over(tree: &mut WorkspaceTree) -> RoomActivityPane {
        let mut pane = RoomActivityPane::new();
        // Legacy timeline tests exercise the granular surface explicitly;
        // production opens on the outcome-first Catch up tab.
        pane.set_view(ActivityView::Explore);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.refresh(tree);
        pane
    }

    fn agent_task_fixture() -> (WorkspaceTree, RecordStore, RoomActivityPane, RecordId) {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let room = tree.active_workspace().expect("workspace").room;
        let records = RecordStore::in_memory();
        let record = RecordId::for_file("campaign-brief.qdoc");
        let checkout = ui::RecordCheckout::new(record.clone(), ui::RecordKind::new("document"));
        assert!(records.open_record(&checkout));
        let agent = ActorId::from(b"writing-agent".as_slice());
        let issuer = ActorId::from(b"ada-identity".as_slice());
        let agent_hex = agent.to_hex_string();
        let issuer_hex = issuer.to_hex_string();
        records.with(&record, |doc| {
            ui::record::init(doc, &record, &checkout.kind, &room.to_string());
            doc.set_actor(agent);
            for (time, field, value, action) in [
                (20, "intro", "A concrete opening", "Rewrite opening"),
                (21, "body", "A clearer launch story", "Clarify launch story"),
                (22, "outro", "Keep the pricing table", "Preserve pricing"),
            ] {
                doc.put(automerge::ROOT, field, value).unwrap();
                let context = ui::tilo_actions::ActionContext::for_resource(
                    action,
                    record.as_str(),
                    "document",
                    "Campaign brief",
                )
                .with_app("org.tilo.Writer", "Writer")
                .with_purpose("Make the opening concrete")
                .with_group(
                    "agent-run:test",
                    "Rewrite the opening and preserve the pricing table",
                    "agent-run",
                );
                doc.commit_with(
                    CommitOptions::default()
                        .with_time(time)
                        .with_message(context.change_message().unwrap()),
                );
            }
            let change = ui::latest_run_by(doc, &agent_hex).expect("agent change");
            ui::record_run_with_details(
                doc,
                "Rewrite the opening and preserve the pricing table",
                Some(&agent_hex),
                Some(&issuer_hex),
                Some("Rewrite the opening and preserve the pricing table"),
                Some("The opening was vague; the pricing table was already correct."),
                &[ui::RunStep {
                    app: "org.tilo.Writer".to_string(),
                    record: record.to_string(),
                    run: change,
                    summary: "Rewrote the launch story".to_string(),
                }],
            )
            .expect("task receipt");
            qdoc::history::commit_stamped_at(doc, 23);
        });
        tree.room_records_held
            .entry(room)
            .or_default()
            .insert(record.to_string(), checkout.kind.to_string());

        let mut pane = RoomActivityPane::new();
        pane.set_view(ActivityView::Explore);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.authors_mut().register(
            &agent_hex,
            ui::widget::HistoryAuthor::new("Quill", 210.0, "agent"),
        );
        pane.authors_mut().register(
            &issuer_hex,
            ui::widget::HistoryAuthor::new("Ada", 120.0, "ada"),
        );
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);
        let end = pane.agent_runs[0].run.steps[0].run.end;
        let selected = pane
            .timeline
            .entries()
            .iter()
            .position(|entry| {
                pane.projected_origins
                    .get(&entry.id)
                    .is_some_and(|origin| origin.change == end)
            })
            .expect("affected history entry");
        pane.timeline.set_selection(Some((selected, selected)));
        pane.explorer_pane = ExplorerPane::Details;
        (tree, records, pane, record)
    }

    #[test]
    fn selecting_agent_work_exposes_the_real_task_receipt_and_named_people() {
        let (_tree, _records, mut pane, _record) = agent_task_fixture();
        let run = pane.agent_runs[0].run.steps[0].run.clone();
        let middle = pane
            .timeline
            .entries()
            .iter()
            .position(|entry| {
                pane.projected_origins.get(&entry.id).is_some_and(|item| {
                    item.change != run.start
                        && item.change != run.end
                        && matches!(
                            item.origin,
                            crate::shell::workspace_history::HistoryOrigin::Record { .. }
                        )
                }) && entry.context.as_ref().is_some_and(|context| {
                    context
                        .group
                        .as_ref()
                        .is_some_and(|group| group.kind == "agent-run")
                })
            })
            .expect("three-change task has an interior history entry");
        pane.timeline.set_selection(Some((middle, middle)));
        let task = pane.selected_agent_task().expect("selected task receipt");
        assert_eq!(
            task.instruction.as_deref(),
            Some("Rewrite the opening and preserve the pricing table")
        );
        assert_eq!(pane.agent_name(task), "Quill");
        assert_eq!(pane.issuer_name(task), "Ada");
        assert_eq!(task.steps[0].summary, "Rewrote the launch story");
        assert!(
            pane.lens_candidates().iter().any(
                |(lens, label)| *lens == LensSelection::AgentTasks && label == "Agent tasks  1"
            ),
            "Browse exposes a first-class agent-task lens"
        );

        let a11y = pane.accessibility_snapshot();
        let review = a11y
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Agent task review"))
            .expect("agent task has a semantic review group");
        let description = review.description.as_deref().unwrap_or_default();
        assert!(description.contains("Ada"));
        assert!(description.contains("Quill"));
        assert!(description.contains("pricing table"));
        assert!(description.contains("Rewrote the launch story"));
    }

    #[test]
    fn agent_task_undo_is_one_keyboard_reachable_safe_action() {
        let (mut tree, records, mut pane, record) = agent_task_fixture();
        paint(&mut pane);
        assert!(pane.can_undo_agent_run());
        assert!(
            pane.focus_order()
                .contains(&ActivityFocus::Detail(DetailAction::UndoTask))
        );
        pane.keyboard_focus = Some(ActivityFocus::Detail(DetailAction::UndoTask));
        let outcome = pane.handle_event_with_records(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Enter,
            )),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
            Some(&records),
        );
        assert!(outcome.reverted);
        assert!(!outcome.undo_partial);
        assert_eq!(
            records.read(&record, |doc| doc
                .get(automerge::ROOT, "body")
                .ok()
                .flatten()
                .is_some()),
            Some(false)
        );
        assert!(
            pane.agent_undo_notice
                .as_ref()
                .is_some_and(|(_, notice)| notice.contains("Whole task undone"))
        );
    }

    #[test]
    fn catch_up_is_default_and_closing_advances_only_private_frontier() {
        let (mut tree, workspace, _) = busy_room();
        let before = tree.doc.get_heads();
        let mut pane = RoomActivityPane::new();
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.refresh(&mut tree);
        assert_eq!(pane.view(), ActivityView::CatchUp);

        let (_, unread_before, _) = pane.glance_signal(workspace.raw());
        assert!(unread_before > 0);
        pane.begin_view(workspace.raw());
        assert!(!pane.digest().briefs.is_empty());
        pane.finish_view(workspace.raw());
        let (_, unread_after, _) = pane.glance_signal(workspace.raw());
        assert_eq!(unread_after, 0);
        assert_eq!(
            tree.doc.get_heads(),
            before,
            "reading history is not a room edit"
        );
    }

    #[test]
    fn away_recap_requires_the_full_threshold_and_fails_closed_on_clock_rollback() {
        let departed = 10_000;
        assert!(!away_recap_is_due(
            departed,
            SystemTime::UNIX_EPOCH + Duration::from_secs(departed + 45 * 60 - 1)
        ));
        assert!(away_recap_is_due(
            departed,
            SystemTime::UNIX_EPOCH + Duration::from_secs(departed + 45 * 60)
        ));
        assert!(!away_recap_is_due(
            departed,
            SystemTime::UNIX_EPOCH + Duration::from_secs(departed - 1)
        ));
    }

    fn away_change_count_after(elapsed: Duration, changed: bool) -> usize {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let records = RecordStore::in_memory();
        let home = tree.active;
        let elsewhere = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.id != home)
            .expect("demo has another workspace")
            .id;
        let departed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        let mut pane = RoomActivityPane::new();
        pane.persistent_frontiers = false;
        pane.set_now_for_test(departed_at);
        pane.observe_workspace(home.raw());
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);

        pane.observe_workspace(elsewhere.raw());
        tree.active = elsewhere;
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);
        if changed {
            tree.workspaces
                .iter_mut()
                .find(|workspace| workspace.id == home)
                .expect("home workspace")
                .name = "Board review".to_string();
            tree.seed_doc_from_cache();
        }

        tree.active = home;
        pane.set_now_for_test(departed_at + elapsed);
        pane.observe_workspace(home.raw());
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);
        pane.away_change_count(home.raw())
    }

    #[test]
    fn away_recap_requires_both_time_away_and_causal_changes() {
        assert_eq!(
            away_change_count_after(Duration::from_secs(45 * 60 - 1), true),
            0,
            "a changed workspace does not interrupt a short absence"
        );
        assert_eq!(
            away_change_count_after(Duration::from_secs(45 * 60), false),
            0,
            "elapsed time alone never creates a recap"
        );
        assert!(
            away_change_count_after(Duration::from_secs(45 * 60), true) > 0,
            "the exact threshold plus causally new work produces a recap"
        );
    }

    #[test]
    fn task_center_creates_manual_work_without_a_model() {
        let (mut tree, workspace, _) = busy_room();
        let mut pane = RoomActivityPane::new();
        pane.set_view(ActivityView::Tasks);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.refresh(&mut tree);
        pane.open_task_composer(&tree);
        pane.task_draft = "Check the launch copy".into();

        let outcome = pane.create_task_from_draft(&mut tree);

        assert!(outcome.task_changed);
        let task = tree
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .and_then(|workspace| workspace.tasks.first())
            .expect("task is stored on the workspace");
        assert_eq!(task.route, WorkspaceTaskRoute::Manual);
        assert!(task.assigned_agent.is_none());
        assert_eq!(task.status, WorkspaceTaskStatus::Queued);
        assert!(!pane.task_compose_open);
    }

    #[test]
    fn task_center_attaches_the_resource_selected_in_explore() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let resource_entry = HistoryEntry {
            id: HistoryEntryId([71; 16]),
            author: ui::widget::HistoryAuthor::new("ada", 10.0, "Ada"),
            op_class: OpClass::EditMeta,
            artifact_kind: ArtifactKind::File,
            timestamp: SystemTime::UNIX_EPOCH,
            summary: "Edited launch brief".into(),
            text_delta: None,
            diff_payload: None,
            redaction: None,
            context: Some(
                ui::tilo_actions::ActionContext::for_resource(
                    "Edit",
                    "document:launch-brief",
                    "document",
                    "Launch brief",
                )
                .with_app("org.tilo.Writer", "Writer"),
            ),
        };
        pane.timeline_mut().set_entries(vec![resource_entry]);
        let resource_row = 0;
        pane.timeline_mut()
            .set_selection(Some((resource_row, resource_row)));
        let expected = pane
            .selection_detail()
            .and_then(|detail| detail.resources.first().cloned())
            .expect("selected action has a resource");

        pane.set_view(ActivityView::Tasks);
        pane.open_task_composer(&tree);

        let attached = pane.task_attachment.as_ref().expect("resource is attached");
        assert_eq!(attached.id, expected.id);
        assert_eq!(attached.app_id, expected.app_id);
    }

    #[test]
    fn task_accessibility_exposes_route_status_and_audit() {
        let (mut tree, workspace, _) = busy_room();
        tree.create_workspace_task(
            workspace,
            "Prepare screenshots",
            "ada",
            Some("studio-agent".into()),
            WorkspaceTaskRoute::RulesOnly,
            None,
        )
        .unwrap();
        let mut pane = RoomActivityPane::new();
        pane.set_view(ActivityView::Tasks);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.refresh(&mut tree);
        let snapshot = pane.accessibility_snapshot();
        let debug = format!("{snapshot:?}");
        assert!(debug.contains("Prepare screenshots"));
        assert!(debug.contains("rules only"));
        assert!(debug.contains("1 audit events"));
    }

    #[test]
    fn task_center_presents_agent_principals_without_leaking_wire_ids() {
        let event_time = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(42))
            .expect("fixture time");
        let task = WorkspaceTask {
            id: "task:review".into(),
            title: "Review launch copy".into(),
            created_by: "ada".into(),
            assigned_agent: Some("agent:scribe".into()),
            route: WorkspaceTaskRoute::RulesOnly,
            status: WorkspaceTaskStatus::InProgress,
            resource: None,
            created_at: 42,
            updated_at: 42,
            audit: vec![crate::shell::state::WorkspaceTaskAuditEvent {
                at: 42,
                actor: "agent:scribe".into(),
                summary: "Reviewed section 2".into(),
            }],
        };

        let byline = task_assignee_label(&task);
        let audit = task_audit_line(&task.audit[0], event_time, event_time);
        let accessible = task_accessibility_description(&task, event_time);

        assert_eq!(byline, "▣ AGENT · scribe");
        assert!(audit.starts_with("scribe · Agent · Reviewed section 2"));
        assert!(accessible.contains("▣ AGENT · scribe"));
        assert!(accessible.contains("scribe · Agent"));
        for rendered in [&byline, &audit, &accessible] {
            assert!(
                !rendered.contains("agent:"),
                "wire principal leaked into visible copy: {rendered}"
            );
        }
    }

    #[test]
    fn only_manual_tasks_offer_a_human_started_transition() {
        let (mut tree, workspace, _) = busy_room();
        let manual_id = tree
            .create_workspace_task(
                workspace,
                "Review the brief",
                "ada",
                None,
                WorkspaceTaskRoute::Manual,
                None,
            )
            .expect("manual task is created");
        let automated_id = tree
            .create_workspace_task(
                workspace,
                "Prepare screenshots",
                "ada",
                Some("studio-agent".into()),
                WorkspaceTaskRoute::RulesOnly,
                None,
            )
            .expect("automated task is created");
        let tasks = &tree
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .expect("workspace remains available")
            .tasks;
        let manual = tasks
            .iter()
            .find(|task| task.id == manual_id)
            .expect("manual task is stored");
        let automated = tasks
            .iter()
            .find(|task| task.id == automated_id)
            .expect("automated task is stored");

        assert!(task_actions(manual).contains(&TaskAction::Start));
        assert!(!task_actions(automated).contains(&TaskAction::Start));
        let mut pane = RoomActivityPane::new();
        pane.task_room = "room".into();
        assert!(
            !pane
                .task_actions_for(automated)
                .contains(&TaskAction::Launch),
            "membership without a live task subscription must not enable Launch"
        );
        assert!(
            pane.task_actions_for(automated)
                .contains(&TaskAction::StartAgent),
            "offline work offers a managed agent start, never a fake task run"
        );
        assert_eq!(
            pane.task_execution_notice(automated).as_deref(),
            Some("Agent runtime not connected · this task has not started")
        );
    }

    #[test]
    fn automated_task_cannot_be_marked_running_without_a_runtime_run() {
        let (mut tree, workspace, _) = busy_room();
        let task_id = tree
            .create_workspace_task(
                workspace,
                "Prepare screenshots",
                "ada",
                Some("studio-agent".into()),
                WorkspaceTaskRoute::Model,
                None,
            )
            .expect("task is created");
        let mut pane = RoomActivityPane::new();
        pane.set_view(ActivityView::Tasks);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.refresh(&mut tree);
        pane.selected_task = Some(task_id.clone());

        let outcome = pane.apply_task_action(TaskAction::Start, &mut tree);

        assert!(!outcome.task_changed);
        let task = tree
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .and_then(|workspace| workspace.tasks.iter().find(|task| task.id == task_id))
            .expect("task remains stored");
        assert_eq!(task.status, WorkspaceTaskStatus::Queued);
        assert_eq!(task.audit.len(), 1, "no synthetic start audit was added");
    }

    #[test]
    fn offline_agent_start_is_deduplicated_and_failure_becomes_retry() {
        let (mut tree, workspace, _) = busy_room();
        let task_id = tree
            .create_workspace_task(
                workspace,
                "Prepare screenshots",
                "ada",
                Some("studio-agent".into()),
                WorkspaceTaskRoute::RulesOnly,
                None,
            )
            .expect("task is created");
        let mut pane = RoomActivityPane::new();
        pane.refresh(&mut tree);
        let task = pane
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .cloned()
            .expect("task is projected");
        assert!(
            pane.task_actions_for(&task)
                .contains(&TaskAction::StartAgent)
        );

        pane.agent_startup.insert(
            (pane.task_room.clone(), "studio-agent".into()),
            AgentStartup::Starting {
                since: Instant::now(),
            },
        );
        assert!(
            !pane
                .task_actions_for(&task)
                .contains(&TaskAction::StartAgent),
            "one pending managed launch suppresses another start click"
        );

        assert!(
            pane.observe_agent_launch_event(&crate::shell::launcher::LaunchEvent {
                package_id: crate::shell::launcher::WORKSPACE_AGENT_PACKAGE_ID.into(),
                display_name: "Agent".into(),
                managed_agent_id: Some("studio-agent".into()),
                managed_workspace_id: Some(pane.task_room.clone()),
                kind: crate::shell::launcher::LaunchEventKind::SpawnFailed {
                    detail: "launcher unavailable".into(),
                },
            })
        );
        assert!(
            pane.task_actions_for(&task)
                .contains(&TaskAction::StartAgent)
        );
        assert_eq!(
            pane.task_action_label(TaskAction::StartAgent, &task),
            "Retry agent"
        );
        assert_eq!(task.status, WorkspaceTaskStatus::Queued);
    }

    #[test]
    fn explore_expands_to_three_columns_only_when_the_viewport_can_hold_them() {
        let (mut tree, _, _) = busy_room();
        let mut pane = RoomActivityPane::new();
        pane.set_view(ActivityView::Explore);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.refresh(&mut tree);
        assert!(pane.sheet.width() >= 900.0);
        assert!(pane.timeline.rect().width() < pane.sheet.width() - 400.0);

        pane.layout(Rect::new(0.0, 0.0, 700.0, 900.0));
        assert!(pane.sheet.width() < 900.0);
        assert!(pane.timeline.rect().width() > pane.sheet.width() - 40.0);
    }

    #[test]
    fn narrow_explorer_pushes_between_timeline_browse_and_details() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_view(ActivityView::Explore);
        pane.layout(Rect::new(0.0, 0.0, 700.0, 800.0));
        paint(&mut pane);
        assert_eq!(pane.explorer_pane_rects.len(), 3);

        pane.explorer_pane = ExplorerPane::Browse;
        paint(&mut pane);
        assert!(!pane.lens_rects.is_empty());

        pane.timeline.set_selection(Some((0, 0)));
        pane.explorer_pane = ExplorerPane::Details;
        paint(&mut pane);
        assert!(!pane.detail_action_rects.is_empty());
    }

    #[test]
    fn rows_come_from_the_rooms_own_change_graph() {
        let (mut tree, _, _) = busy_room();
        let pane = pane_over(&mut tree);
        assert!(
            pane.entries().len() >= 4,
            "bootstrap + owner + two grants + a revoke is at least four changes, got {}",
            pane.entries().len()
        );
        let summaries: Vec<&str> = pane.entries().iter().map(|e| e.summary.as_str()).collect();
        assert!(
            summaries.iter().any(|s| s.contains("/home/quinten/notes")),
            "the filesystem grant is named by the path it granted: {summaries:?}"
        );
        assert!(
            summaries.iter().any(|s| s.starts_with("Revoked")),
            "the revoke is its own row: {summaries:?}"
        );
        assert!(
            summaries.iter().any(|s| s.contains("Design review")),
            "the flush that carried the rename names it: {summaries:?}"
        );
        assert!(
            !summaries.iter().any(|s| s == &"Granted 4 capabilities"),
            "a flush that granted nothing must not read as a grant: {summaries:?}"
        );
    }

    #[test]
    fn app_native_handoff_focuses_the_exact_version_without_marking_it_unseen() {
        let (mut tree, workspace, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let unread_before = pane.passive_unread;
        let id = HistoryEntryId::from_bytes([9; 16]);
        let resource = "signed-file-record".to_string();
        let context = ui::tilo_actions::ActionContext::for_resource(
            "Edited",
            resource.clone(),
            "files.entry",
            "Launch brief.md",
        )
        .with_app("org.tilo.Files", "Files");
        pane.open_external_entry(HistoryEntry {
            id,
            author: ui::widget::HistoryAuthor::new("Ada", 280.0, "01010101"),
            op_class: collab_types::OpClass::Compaction,
            artifact_kind: ArtifactKind::File,
            timestamp: SystemTime::UNIX_EPOCH + Duration::from_secs(20),
            summary: "Edited · Launch brief.md".into(),
            text_delta: None,
            diff_payload: None,
            redaction: None,
            context: Some(context),
        });
        pane.apply_deep_link(crate::shell::chrome_state::ActivityFocus {
            entry_id: Some(*id.as_bytes()),
            resource_id: Some(resource.clone()),
            resource_version: Some(*id.as_bytes()),
            ..crate::shell::chrome_state::ActivityFocus::default()
        });

        assert_eq!(pane.view(), ActivityView::Explore);
        assert_eq!(pane.lens, LensSelection::Resource(resource));
        let (selected, _) = pane.timeline.selection().expect("exact version selected");
        assert_eq!(pane.timeline.entries()[selected].id, id);
        assert_eq!(
            pane.timeline.visible_entry(0).map(|entry| entry.id),
            Some(id)
        );
        assert_eq!(pane.passive_unread, unread_before);
        assert_eq!(pane.glance_signal(workspace.raw()).1, unread_before);
    }

    /// A second workspace with its own governance, interleaved in one document.
    fn two_room_tree() -> (WorkspaceTree, WorkspaceId, WorkspaceId) {
        let (mut tree, first, user) = busy_room();
        let second = tree.append_empty_workspace("Research");
        tree.adopt_ownerless_workspaces(&user);
        tree.grant_in_workspace(
            second,
            &user,
            CapOverride::grant(
                Subject::app("browser"),
                Scope::NetHost("research.example".to_string()),
                Verb::Read,
            ),
        )
        .expect("owner may grant in the second room too");
        tree.set_active(first);
        (tree, first, second)
    }

    #[test]
    fn this_rooms_scope_excludes_another_workspaces_governance() {
        let (mut tree, _first, _second) = two_room_tree();
        let mut pane = pane_over(&mut tree);
        assert_eq!(pane.scope(), RoomScope::ThisRoom, "scoped by default");

        let scoped: Vec<String> = pane.entries().iter().map(|e| e.summary.clone()).collect();
        assert!(
            !scoped.iter().any(|s| s.contains("research.example")),
            "the other room's network grant leaked into this room: {scoped:?}"
        );
        assert!(
            scoped.iter().any(|s| s.contains("/home/quinten/notes")),
            "this room's own grant is present: {scoped:?}"
        );

        let all_tree = { tree.workspaces.clone() };
        let _ = all_tree;
        pane.set_scope(RoomScope::AllWorkspaces, &tree);
        let widened: Vec<String> = pane.entries().iter().map(|e| e.summary.clone()).collect();
        assert!(
            widened.iter().any(|s| s.contains("research.example")),
            "widening the scope must reveal the other room: {widened:?}"
        );
        assert!(
            widened.len() > scoped.len(),
            "the whole tree is strictly more than one room: {} vs {}",
            widened.len(),
            scoped.len()
        );
    }

    #[test]
    fn a_widened_row_names_the_workspace_it_happened_in() {
        let (mut tree, _first, _second) = two_room_tree();
        let mut pane = pane_over(&mut tree);
        pane.set_scope(RoomScope::AllWorkspaces, &tree);
        let owner_rows: Vec<String> = pane
            .entries()
            .iter()
            .filter(|e| e.summary.contains("Set the workspace owner"))
            .map(|e| e.summary.clone())
            .collect();
        assert!(
            owner_rows.len() > 1,
            "the fixture has several workspaces adopting an owner: {owner_rows:?}"
        );
        assert!(
            owner_rows
                .iter()
                .any(|s| s.starts_with("Research \u{00b7} ")),
            "a row must name the room it happened in: {owner_rows:?}"
        );
        let distinct: std::collections::HashSet<&String> = owner_rows.iter().collect();
        assert_eq!(
            distinct.len(),
            owner_rows.len(),
            "no two rows may read identically: {owner_rows:?}"
        );
    }

    #[test]
    fn switching_the_active_workspace_re_scopes_without_a_document_change() {
        let (mut tree, first, second) = two_room_tree();
        let mut pane = pane_over(&mut tree);
        let in_first: Vec<String> = pane.entries().iter().map(|e| e.summary.clone()).collect();
        assert!(in_first.iter().any(|s| s.contains("/home/quinten/notes")));

        tree.set_active(second);
        pane.refresh(&mut tree);
        let in_second: Vec<String> = pane.entries().iter().map(|e| e.summary.clone()).collect();
        assert!(
            in_second.iter().any(|s| s.contains("research.example")),
            "the second room's own grant: {in_second:?}"
        );
        assert!(
            !in_second.iter().any(|s| s.contains("/home/quinten/notes")),
            "the first room's grant must not follow us: {in_second:?}"
        );
        let _ = first;
    }

    #[test]
    fn a_whole_tree_flush_belongs_to_no_single_room() {
        let (mut tree, _first, _second) = two_room_tree();
        let mut pane = pane_over(&mut tree);
        let scoped_flushes = pane
            .entries()
            .iter()
            .filter(|e| e.summary.starts_with("Recorded "))
            .count();
        assert!(scoped_flushes > 0, "the fixture flushed at least once");
        pane.set_scope(RoomScope::AllWorkspaces, &tree);
        let widened_flushes = pane
            .entries()
            .iter()
            .filter(|e| e.summary.starts_with("Recorded "))
            .count();
        assert_eq!(
            scoped_flushes, widened_flushes,
            "a flush is unattributed, so widening must not add or drop one"
        );
    }

    #[test]
    fn the_local_users_own_rows_are_attributed_to_them() {
        let (mut tree, _, _) = busy_room();
        let pane = pane_over(&mut tree);
        let me = crate::shell::state::local_user_id();
        assert!(!me.is_empty(), "there is always a local user id");
        assert!(
            pane.entries()
                .iter()
                .all(|e| e.author.name != ui::history_source::UNKNOWN_AUTHOR_NAME),
            "local changes resolved to an unknown participant: {:?}",
            pane.contributors()
        );
        assert_eq!(
            pane.contributors().len(),
            1,
            "one person did all of this: {:?}",
            pane.contributors()
        );
    }

    #[test]
    fn every_row_carries_a_real_timestamp() {
        // The whole date filter rests on this — an epoch row can never be
        // selected by a date range.
        let (mut tree, _, _) = busy_room();
        let pane = pane_over(&mut tree);
        assert!(!pane.entries().is_empty());
        for e in pane.entries() {
            assert!(
                e.timestamp > SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000),
                "row {:?} is stamped at {:?} — the room's commits are not carrying a clock",
                e.summary,
                e.timestamp
            );
        }
    }

    #[test]
    fn date_range_filter_partitions_the_real_history() {
        // One change stamped a week back via `commit_stamped_at`, the rest
        // at now: "Today" drops the old one, "30 days" keeps everything.
        let mut tree = WorkspaceTree::bootstrap_demo();
        let user: String = "test-owner".to_string();
        tree.adopt_ownerless_workspaces(&user);
        let ws_id = tree.active;

        let week_ago = SystemTime::now() - Duration::from_secs(604_800);
        let week_ago_secs = i64::try_from(
            week_ago
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap();
        assert!(tree.doc_append_tab_to_first_group_of_active_workspace(
            9001,
            "Last week's window".to_string(),
            b"{}".to_vec(),
        ));
        qdoc::history::commit_stamped_at(&mut tree.doc, week_ago_secs)
            .expect("the append left a transaction to stamp");

        tree.grant_in_workspace(
            ws_id,
            &user,
            CapOverride::grant(
                Subject::app("files"),
                Scope::FsPath("/home/quinten/today".to_string()),
                Verb::Read,
            ),
        )
        .expect("owner may grant");

        let mut pane = pane_over(&mut tree);
        let all = pane.visible_len();
        assert!(all >= 3, "expected the full history, got {all}");

        pane.set_period(Period::Today);
        let today = pane.visible_len();
        assert!(today > 0, "today's changes must survive the day filter");
        assert!(
            today < all,
            "the week-old change must be filtered out: {today} of {all} survived"
        );
        assert!(
            !pane.timeline().entries().is_empty(),
            "filtering hides rows, it does not discard them"
        );
        for i in 0..pane.visible_len() {
            let e = pane.visible_entry(i).unwrap();
            assert!(
                !e.summary.contains("Last week"),
                "the week-old row leaked through the day filter: {}",
                e.summary
            );
        }

        pane.set_period(Period::Month);
        assert_eq!(
            pane.visible_len(),
            all,
            "a 30-day window covers a change from seven days ago"
        );
    }

    #[test]
    fn contributor_filter_narrows_to_one_person() {
        let (mut tree, _, _) = busy_room();

        let mut peer = automerge::AutoCommit::new();
        peer.set_actor(automerge::ActorId::from("bob".as_bytes()));
        peer.apply_changes(
            tree.export_changes_since(&[])
                .iter()
                .map(|b| automerge::Change::try_from(b.as_slice()).unwrap()),
        )
        .unwrap();
        {
            use automerge::transaction::Transactable;
            let (_, ws_entry) = peer
                .get(
                    &peer.get(&automerge::ROOT, "workspaces").unwrap().unwrap().1,
                    0_usize,
                )
                .unwrap()
                .unwrap();
            peer.put(&ws_entry, "name", "Bob's rename").unwrap();
        }
        qdoc::history::commit_stamped(&mut peer).expect("the peer's edit commits");
        let blobs: Vec<Vec<u8>> = peer
            .get_changes(&tree.doc.get_heads())
            .into_iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect();
        tree.apply_changes(&blobs).unwrap();

        let mut pane = pane_over(&mut tree);
        assert!(
            pane.contributors().len() >= 2,
            "two actors wrote to this room; contributors = {:?}",
            pane.contributors()
        );
        let bob = pane
            .contributors()
            .iter()
            .find(|c| c.as_str() == "bob")
            .cloned()
            .expect("the peer's self-declared handle resolves to a contributor");

        let everyone = pane.visible_len();
        pane.set_author_filter(Some(bob.clone()));
        let just_bob = pane.visible_len();
        assert!(just_bob > 0, "bob authored at least one row");
        assert!(
            just_bob < everyone,
            "filtering to bob must hide the local actor's rows: {just_bob} of {everyone}"
        );
        for i in 0..pane.visible_len() {
            assert_eq!(pane.visible_entry(i).unwrap().author.name, bob);
        }

        pane.set_author_filter(None);
        assert_eq!(pane.visible_len(), everyone, "clearing restores every row");
    }

    #[test]
    fn action_type_chips_narrow_by_op_class() {
        use ui::widget::activity_timeline::ChipKind;
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let all = pane.visible_len();

        pane.timeline_mut().set_active_chip(ChipKind::Governance);
        let gov = pane.visible_len();
        assert!(
            gov > 0,
            "grants and revokes are governance; the room has some"
        );
        assert!(gov < all, "governance is a subset: {gov} of {all}");
        for i in 0..pane.visible_len() {
            assert_eq!(
                pane.visible_entry(i).unwrap().op_class,
                OpClass::Govern,
                "the Governance chip must only show governance rows"
            );
        }

        pane.timeline_mut().set_active_chip(ChipKind::All);
        assert_eq!(pane.visible_len(), all);
    }

    #[test]
    fn reverting_a_row_writes_a_new_forward_change() {
        let (mut tree, ws_id, user) = busy_room();
        let mut pane = pane_over(&mut tree);

        let before_name = tree
            .workspaces
            .iter()
            .find(|w| w.id == ws_id)
            .map(|w| w.name.clone())
            .unwrap();
        assert_eq!(before_name, "Design review");
        let versions_before = pane.entries().len();

        // Rename again, so there is something to revert back across.
        if let Some(ws) = tree.workspaces.iter_mut().find(|w| w.id == ws_id) {
            ws.name = "Renamed by mistake".to_string();
        }
        tree.seed_doc_from_cache();
        pane.refresh(&mut tree);
        assert!(pane.entries().len() > versions_before);

        // Revert to the version the mistake was made on top of.
        let target = pane.entries()[versions_before - 1].id;
        assert!(
            pane.revert_to(&mut tree, &target),
            "the revert wrote something"
        );

        assert_eq!(
            tree.workspaces
                .iter()
                .find(|w| w.id == ws_id)
                .map(|w| w.name.as_str()),
            Some("Design review"),
            "the room is back to the state the row named"
        );
        assert!(
            pane.entries().len() > versions_before + 1,
            "the revert is a new row on top, not a rewind: {} rows",
            pane.entries().len()
        );
        assert!(
            pane.entries().iter().any(|e| e.id == target),
            "the version reverted to is still in the graph"
        );
        let _ = user;
    }

    #[test]
    fn reverting_an_unknown_row_is_a_no_op() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let before = tree.doc.get_heads();
        assert!(!pane.revert_to(&mut tree, &HistoryEntryId([0x5a; 16])));
        assert_eq!(tree.doc.get_heads(), before, "nothing was written");
    }

    #[test]
    fn governance_rows_never_offer_revert() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.timeline_mut()
            .set_active_chip(ui::widget::activity_timeline::ChipKind::Governance);
        assert!(pane.visible_len() > 0);
        for i in 0..pane.visible_len() {
            let e = pane.visible_entry(i).unwrap();
            assert!(
                !op_class_is_revertible(e.op_class),
                "{:?} offered a revert chip",
                e.summary
            );
            assert!(
                pane.timeline().revert_chip_rect(i).is_none(),
                "row {i} ({:?}) laid out a revert chip",
                e.summary
            );
        }
    }

    #[test]
    fn a_capability_is_described_in_the_rooms_vocabulary() {
        let (mut tree, _, _) = busy_room();
        let pane = pane_over(&mut tree);
        let grant = pane
            .entries()
            .iter()
            .find(|e| e.summary.contains("/home/quinten/notes"))
            .expect("the filesystem grant has a row");
        assert!(
            grant
                .summary
                .starts_with("Granted read on /home/quinten/notes to files"),
            "the grant names verb, scope and subject: {:?}",
            grant.summary
        );
        assert_eq!(grant.op_class, OpClass::Govern);
    }

    #[test]
    fn an_empty_room_and_a_filtered_one_do_not_read_the_same() {
        let mut empty = WorkspaceTree::default();
        let pane = pane_over(&mut empty);
        let (title, detail) = pane.timeline().empty_state_lines();
        assert_eq!(title, "No activity yet");
        assert!(detail.is_none(), "nothing to explain when nothing happened");

        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_author_filter(Some("nobody-by-that-name".to_string()));
        assert_eq!(pane.visible_len(), 0);
        let (title, detail) = pane.timeline().empty_state_lines();
        assert_eq!(title, "No activity matches these filters");
        let detail = detail.expect("a filtered-empty list must explain itself");
        assert!(
            detail.contains("nobody-by-that-name"),
            "the explanation names the filter doing the hiding: {detail}"
        );
        assert!(
            detail.contains(&pane.entries().len().to_string()),
            "the explanation says how many rows are hidden: {detail}"
        );
    }

    #[test]
    fn the_sheet_is_a_side_panel_when_wide_and_full_bleed_when_narrow() {
        let mut pane = RoomActivityPane::new();

        pane.layout(Rect::new(0.0, 0.0, 1920.0, 1080.0));
        let wide = pane.rect();
        assert!(
            wide.width() <= SHEET_MAX_W,
            "a 1920 px viewport must not get a 700 px sheet: {}",
            wide.width()
        );
        assert!(
            wide.x0 > 1920.0 * 0.5,
            "the sheet is anchored right, leaving the room visible behind it"
        );

        pane.layout(Rect::new(0.0, 0.0, 640.0, 480.0));
        let narrow = pane.rect();
        assert!(
            narrow.width() > 640.0 * 0.9,
            "under {FULL_BLEED_UNDER} px the sheet takes the viewport: {}",
            narrow.width()
        );
        assert!(narrow.x0 < 32.0, "full bleed starts at the left inset");
    }

    #[test]
    fn a_short_viewport_drops_the_chip_rows_before_the_row_list() {
        let mut pane = RoomActivityPane::new();
        pane.set_view(ActivityView::Explore);
        pane.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        assert!(
            pane.shows_chip_rows(),
            "a normal viewport affords the filters"
        );
        let tall_rows = pane.timeline().rect().height();

        pane.layout(Rect::new(0.0, 0.0, 1280.0, 220.0));
        assert!(
            !pane.shows_chip_rows(),
            "a 220 px viewport must spend its height on rows, not chips"
        );
        assert!(
            pane.timeline().rect().height() > 0.0,
            "the row list survives even when the filters do not"
        );
        assert!(tall_rows > pane.timeline().rect().height());
    }

    #[test]
    fn contributors_are_only_people_who_actually_did_something() {
        let (mut tree, _, _) = busy_room();
        tree.collaborators
            .push(crate::shell::state::Collaborator::new(
                "ghost",
                "Never Edited",
            ));
        let pane = pane_over(&mut tree);
        assert!(
            !pane.contributors().iter().any(|c| c == "Never Edited"),
            "a peer who never wrote must not be listed: {:?}",
            pane.contributors()
        );
    }

    #[test]
    fn activity_header_digest_reduces_the_same_scoped_rows() {
        let (mut tree, _, _) = busy_room();
        let pane = pane_over(&mut tree);
        assert_eq!(pane.digest().total_changes, pane.entries().len());
        assert!(!pane.digest().briefs.is_empty());
        assert!(pane.digest().headline().is_some());
    }

    #[test]
    fn replay_materializes_real_causal_layouts_and_exposes_controls() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        assert!(tree.doc_append_tab_to_first_group_of_active_workspace(
            91_001,
            "Plan".to_string(),
            b"{}".to_vec(),
        ));
        qdoc::history::commit_stamped(&mut tree.doc);
        assert!(tree.doc_append_tab_to_first_group_of_active_workspace(
            91_002,
            "Review".to_string(),
            b"{}".to_vec(),
        ));
        qdoc::history::commit_stamped(&mut tree.doc);
        let mut pane = pane_over(&mut tree);
        assert!(pane.replay_frames.len() >= 2);
        let tab_counts: Vec<usize> = pane
            .replay_frames
            .iter()
            .map(|frame| {
                frame
                    .workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.groups)
                    .map(|group| group.tabs.len())
                    .sum()
            })
            .collect();
        assert!(
            tab_counts.windows(2).any(|pair| pair[0] != pair[1]),
            "causal forks must expose a changing real layout: {tab_counts:?}"
        );
        pane.set_view(ActivityView::Replay);
        paint(&mut pane);
        let order = pane.focus_order();
        assert!(order.contains(&ActivityFocus::ReplayBack));
        assert!(order.contains(&ActivityFocus::ReplayPlay));
        assert!(order.contains(&ActivityFocus::ReplayForward));

        let mut presence = PresenceCtx::empty();
        let ada = ui::permissions::UserId("ada".into());
        presence.names.insert(ada.clone(), "Ada".into());
        presence.set_pointer(
            &ui::presence::PresenceKey::desktop_pointer("design-room"),
            ada,
            (420.0, 260.0),
        );
        pane.observe_replay_presence(&presence);
        let pointer = pane
            .replay_frames
            .last()
            .and_then(|frame| frame.pointers.first())
            .expect("session presence is retained against the durable checkpoint");
        assert_eq!(pointer.name, "Ada");
        assert_eq!(pointer.point, (420.0, 260.0));
        assert!(pointer.desktop_space);
    }

    #[test]
    fn narrow_history_keeps_both_scope_choices_visible() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.layout(Rect::new(0.0, 0.0, 380.0, 640.0));
        paint(&mut pane);
        assert_eq!(pane.scope_rects.len(), 2);
        assert!(
            pane.scope_rects
                .iter()
                .all(|(_, rect)| { rect.area() > 0.0 && pane.sheet.contains(rect.center()) })
        );
    }

    #[test]
    fn a_long_lived_room_stays_bounded() {
        // Well past the producer's ceiling, all of it real: each iteration is
        // the same mirror the drop handler calls when a file is dropped into
        // a workspace. The pane must show the recent end of the history
        // rather than growing without limit.
        let mut tree = WorkspaceTree::bootstrap_demo();
        for i in 0..240u64 {
            assert!(tree.doc_append_tab_to_first_group_of_active_workspace(
                50_000 + i,
                format!("Window {i}"),
                b"{}".to_vec(),
            ));
            tree.commit_room_change();
        }
        let pane = pane_over(&mut tree);
        assert!(
            pane.entries().len() <= ui::history_source::DEFAULT_MAX_ENTRIES,
            "the pane derived {} rows from a 240-change room",
            pane.entries().len()
        );
        assert!(
            pane.entries().len() > 100,
            "and it is showing a real window of them, not a token few"
        );
        // The newest change is the one that must survive the cap.
        assert!(
            pane.entries()
                .iter()
                .any(|e| e.summary.contains("Window 239")),
            "the most recent change is inside the window"
        );
    }

    #[test]
    fn period_windows_are_half_open_and_all_time_is_unbounded() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        assert!(Period::AllTime.range(now).is_none());
        let (start, end) = Period::Week.range(now).unwrap();
        assert_eq!(
            now.duration_since(start).unwrap(),
            Duration::from_secs(604_800)
        );
        assert!(
            end > now,
            "a change committed this instant is inside the window"
        );
    }

    // -----------------------------------------------------------------
    // Grouping and names
    // -----------------------------------------------------------------

    #[test]
    fn the_rooms_history_reads_as_bands_rather_than_one_row_per_change() {
        let (mut tree, _, _) = busy_room();
        let pane = pane_over(&mut tree);
        assert!(pane.entries().len() >= 4);
        // Bands, not a wall: every rendered line is either a band header or a
        // change, and there are fewer lines than changes.
        let bands = (0..pane.timeline().row_count())
            .filter(|i| pane.timeline().group_at_row(*i).is_some())
            .count();
        assert!(
            bands >= 1,
            "a room with governance, edits and a deletion must produce at least one band"
        );
        // Every band describes itself without a name, and describes itself
        // honestly — a count it did not measure would be the failure here.
        for i in 0..pane.timeline().row_count() {
            if let Some(title) = pane.timeline().group_title_at_row(i) {
                assert!(!title.is_empty(), "band {i} has no title");
                assert!(
                    !title.contains("{}"),
                    "band {i} rendered a format placeholder: {title:?}"
                );
            }
        }
    }

    #[test]
    fn naming_a_band_round_trips_through_the_room_document() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let entries = pane.entries().to_vec();
        let (start, end) = (entries[1].id, entries[3].id);

        let wrote = pane.apply_name(
            &mut tree,
            &GroupNameRequest {
                record_id: None,
                start,
                end,
                label: "Set up the room".to_string(),
            },
        );
        assert!(wrote, "the name went into the document");

        // It is in the document, not in the widget: a fresh pane over the same
        // tree finds it.
        let ranges = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new());
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].label, "Set up the room");
        assert_eq!(ranges[0].start, start);
        assert_eq!(ranges[0].end, Some(end));

        let mut fresh = RoomActivityPane::new();
        fresh.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        fresh.refresh(&mut tree);
        let titles: Vec<&str> = (0..fresh.timeline().row_count())
            .filter_map(|i| fresh.timeline().group_title_at_row(i))
            .collect();
        assert!(
            titles.contains(&"Set up the room"),
            "the band is on screen for anyone who opens the pane: {titles:?}"
        );
    }

    #[test]
    fn naming_a_band_does_not_add_a_row_of_its_own() {
        // The name is already on screen as the band's title. A "1 field" row
        // directly under it would be noise, so the producer filters the marker
        // change out — and the change count must not move.
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let before = pane.entries().len();
        let entries = pane.entries().to_vec();
        pane.apply_name(
            &mut tree,
            &GroupNameRequest {
                record_id: None,
                start: entries[1].id,
                end: entries[2].id,
                label: "A version".to_string(),
            },
        );
        assert_eq!(
            pane.entries().len(),
            before,
            "naming added a row to the history it named"
        );
    }

    #[test]
    fn un_naming_removes_the_band_and_leaves_the_changes_alone() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let entries = pane.entries().to_vec();
        let (start, end) = (entries[1].id, entries[3].id);
        pane.apply_name(
            &mut tree,
            &GroupNameRequest {
                record_id: None,
                start,
                end,
                label: "Temporary".to_string(),
            },
        );
        let record = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new())[0]
            .id
            .clone();
        let changes_before = pane.entries().len();

        // An empty label is the request to take the name off.
        assert!(pane.apply_name(
            &mut tree,
            &GroupNameRequest {
                record_id: Some(record),
                start,
                end,
                label: String::new(),
            },
        ));
        assert!(
            ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new()).is_empty(),
            "the band is gone"
        );
        assert_eq!(
            pane.entries().len(),
            changes_before,
            "and the changes it named are all still there"
        );
    }

    #[test]
    fn two_people_naming_the_same_run_converge_on_one_band() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let entries = pane.entries().to_vec();
        let (start, end) = (entries[1].id, entries[3].id);

        // A peer forks the room document, names the same run differently, and
        // syncs back — the shape a remote replica's sync round has.
        let mut peer = tree.doc.fork();
        peer.set_actor(automerge::ActorId::from("ada".as_bytes()));
        ui::history_groups::name_range(&mut peer, &start, Some(&end), "Ada's pass");
        qdoc::history::commit_stamped(&mut peer);

        pane.apply_name(
            &mut tree,
            &GroupNameRequest {
                record_id: None,
                start,
                end,
                label: "My pass".to_string(),
            },
        );

        // Merge each way round and compare. Both sides have to land on the same
        // single band, whichever order the names arrived in.
        let mut mine = tree.doc.fork();
        mine.merge(&mut peer.clone()).expect("peers merge");
        let mut theirs = peer.fork();
        theirs.merge(&mut tree.doc.clone()).expect("peers merge");

        let a = ui::history_groups::read_named_ranges(&mine, &AuthorDirectory::new());
        let b = ui::history_groups::read_named_ranges(&theirs, &AuthorDirectory::new());
        assert_eq!(a.len(), 1, "one run, one band: {a:?}");
        assert_eq!(b.len(), 1);
        assert_eq!(a[0].label, b[0].label, "both peers read the same winner");
        assert!(
            a[0].label == "My pass" || a[0].label == "Ada's pass",
            "the winner is one of the two names actually written: {:?}",
            a[0].label
        );
        assert_eq!(a[0].start, start);
        assert_eq!(a[0].end, Some(end));
    }

    #[test]
    fn a_peers_name_is_attributed_to_the_peer_not_to_whoever_is_looking() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let entries = pane.entries().to_vec();
        let (start, end) = (entries[1].id, entries[3].id);

        let mut peer = tree.doc.fork();
        peer.set_actor(automerge::ActorId::from("ada".as_bytes()));
        ui::history_groups::name_range(&mut peer, &start, Some(&end), "Ada's pass");
        qdoc::history::commit_stamped(&mut peer);
        tree.doc.merge(&mut peer).expect("peers merge");

        pane.invalidate();
        pane.refresh(&mut tree);
        let named = (0..pane.timeline().row_count())
            .find(|i| pane.timeline().group_title_at_row(*i) == Some("Ada's pass"))
            .expect("the peer's band is on screen");
        let sub = pane.timeline().group_subtitle_at_row(named).unwrap();
        assert!(
            sub.contains("named by"),
            "a name someone else wrote must say so: {sub:?}"
        );
        assert!(
            !sub.contains("named by test-owner") && !sub.contains("named by quinten"),
            "and it must not be attributed to the reader: {sub:?}"
        );
    }

    #[test]
    fn restoring_to_a_band_boundary_lands_on_the_state_at_that_boundary() {
        // The band header's [Restore] passes the band's last change, so this is
        // the production path for "restore to the end of this version".
        let (mut tree, ws_id, _) = busy_room();
        let mut pane = pane_over(&mut tree);

        // Find the change that named the workspace, and the state just after it.
        let renamed_at = pane
            .entries()
            .iter()
            .position(|e| e.summary.contains("Design review"))
            .expect("the fixture renames the workspace");
        let boundary = pane.entries()[renamed_at].id;

        // Move on: rename again, so the document is no longer in that state.
        if let Some(ws) = tree.workspaces.iter_mut().find(|w| w.id == ws_id) {
            ws.name = "Something else".to_string();
        }
        tree.seed_doc_from_cache();
        pane.invalidate();
        pane.refresh(&mut tree);
        assert_eq!(
            tree.workspaces
                .iter()
                .find(|w| w.id == ws_id)
                .map(|w| w.name.clone()),
            Some("Something else".to_string())
        );

        assert!(
            pane.revert_to(&mut tree, &boundary),
            "the restore wrote something"
        );
        assert_eq!(
            tree.workspaces
                .iter()
                .find(|w| w.id == ws_id)
                .map(|w| w.name.clone()),
            Some("Design review".to_string()),
            "the room is back in the state that boundary produced"
        );
        // Forward, not a rewind: the change we rolled past is still in the graph.
        assert!(
            pane.entries().len() > renamed_at + 1,
            "history was appended to, not rewritten"
        );
    }

    // -----------------------------------------------------------------
    // Naming, driven the way a person drives it: real `WindowEvent`s into
    // `handle_event`, read back out of the **room document**, not the
    // widget — proving the whole chain, not just `apply_name`.
    // -----------------------------------------------------------------

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn mouse_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: winit::event::ElementState::Pressed,
            button: winit::event::MouseButton::Left,
        }
    }

    fn key_press(logical_key: winit::keyboard::Key) -> WindowEvent {
        // SAFETY: as in `ui`'s own widget tests — every pub field of `KeyEvent`
        // tolerates the all-zero bit pattern, and winit 0.30 keeps
        // `KeyEventExtra` crate-private so a struct literal is unavailable.
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = winit::event::ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    /// Paint the sheet so the rects the click handler reads off the last frame
    /// are populated exactly as they are live.
    fn paint(pane: &mut RoomActivityPane) {
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        pane.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
    }

    /// Click at `p`: move there first so the hover latches, the way a pointer
    /// does.
    fn click_at(
        pane: &mut RoomActivityPane,
        tree: &mut WorkspaceTree,
        p: Point,
    ) -> RoomActivityOutcome {
        let m = Modifiers::default();
        pane.handle_event(&cursor_moved(p.x, p.y), 1.0, &m, Some(p), tree);
        pane.handle_event(&mouse_press(), 1.0, &m, Some(p), tree)
    }

    fn type_into(pane: &mut RoomActivityPane, tree: &mut WorkspaceTree, text: &str) {
        let m = Modifiers::default();
        for ch in text.chars() {
            let ev = if ch == ' ' {
                key_press(winit::keyboard::Key::Named(
                    winit::keyboard::NamedKey::Space,
                ))
            } else {
                key_press(winit::keyboard::Key::Character(ch.to_string().into()))
            };
            pane.handle_event(&ev, 1.0, &m, None, tree);
        }
    }

    /// The first line of the pane that is a band header.
    fn first_band(pane: &RoomActivityPane) -> usize {
        (0..pane.timeline().row_count())
            .find(|i| pane.timeline().group_at_row(*i).is_some())
            .expect("the room's history produces at least one band")
    }

    #[test]
    fn clicking_name_typing_and_pressing_enter_puts_the_name_in_the_room_document() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        paint(&mut pane);

        let band = first_band(&pane);
        let (name_chip, _) = pane
            .timeline()
            .group_chip_rects(band)
            .expect("a band header carries a [Name] chip");
        let entries = pane.entries().to_vec();
        // What the band under the pointer covers, read before the gesture so the
        // assertion is against the range the user pointed at rather than whatever
        // came back. Rows are in display order and groups in causal order, so the
        // band is found by the key the row carries, not by position.
        let (first, last) = {
            let key = pane.timeline().group_at_row(band).unwrap().0.to_string();
            let mut flat: Vec<&ui::HistoryGroup> = Vec::new();
            for g in pane.timeline().groups() {
                g.walk(&mut flat);
            }
            let g = flat
                .iter()
                .find(|g| g.key == key)
                .expect("the band the header belongs to");
            (g.first_entry, g.last_entry)
        };

        let out = click_at(&mut pane, &mut tree, name_chip.center());
        assert!(out.handled, "the chip took the click");
        assert!(
            pane.wants_keyboard(),
            "the field is up and owns the keyboard, so the sheet's Escape does not close it"
        );
        assert_eq!(pane.timeline().naming_buffer(), Some(""));

        type_into(&mut pane, &mut tree, "Set up the room");
        assert_eq!(pane.timeline().naming_buffer(), Some("Set up the room"));

        let out = pane.handle_event(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Enter,
            )),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );
        assert!(out.named, "the pane reports a document write");
        assert!(!pane.wants_keyboard(), "the field is gone");

        // The name is in the room document, over the range the band covered.
        let ranges = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new());
        assert_eq!(ranges.len(), 1, "one name, from one gesture: {ranges:?}");
        assert_eq!(ranges[0].label, "Set up the room");
        assert_eq!(ranges[0].start, first);
        assert_eq!(ranges[0].end, Some(last));
        assert!(
            entries.iter().any(|e| e.id == first),
            "the range addresses changes this room actually has"
        );

        // And it is the document's, not the widget's: a pane that never saw the
        // gesture reads the same band off the same tree.
        let mut fresh = RoomActivityPane::new();
        fresh.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        fresh.refresh(&mut tree);
        let titles: Vec<&str> = (0..fresh.timeline().row_count())
            .filter_map(|i| fresh.timeline().group_title_at_row(i))
            .collect();
        assert!(
            titles.contains(&"Set up the room"),
            "the band is not on screen for the next person to open the pane: {titles:?}"
        );
    }

    #[test]
    fn escape_in_the_name_field_writes_nothing_to_the_room_document() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        paint(&mut pane);
        let heads_before = tree.doc.get_heads();

        let band = first_band(&pane);
        let (name_chip, _) = pane.timeline().group_chip_rects(band).unwrap();
        click_at(&mut pane, &mut tree, name_chip.center());
        type_into(&mut pane, &mut tree, "never mind");
        assert!(pane.wants_keyboard());

        let out = pane.handle_event(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Escape,
            )),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );
        assert!(!out.named);
        assert!(!pane.wants_keyboard(), "the field is gone");
        assert!(
            ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new()).is_empty(),
            "Escape wrote a name anyway"
        );
        assert_eq!(
            tree.doc.get_heads(),
            heads_before,
            "abandoning a name must not commit anything at all"
        );
    }

    #[test]
    fn clicking_a_band_header_collapses_and_expands_it() {
        // The disclosure is a click on the header itself, and it must not be
        // confused with the chips that sit on the same row.
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        paint(&mut pane);

        let band = first_band(&pane);
        let (key, expanded) = {
            let (k, e) = pane.timeline().group_at_row(band).unwrap();
            (k.to_string(), e)
        };
        let header = pane.timeline().row_rect(band).expect("the header lays out");
        let (name_chip, _) = pane.timeline().group_chip_rects(band).unwrap();
        // Somewhere on the header that is not a chip: next to the disclosure
        // triangle.
        let on_header = Point::new(header.x0 + 60.0, header.center().y);
        assert!(
            !name_chip.contains(on_header),
            "the fixture aims off the chips"
        );

        let lines_before = pane.timeline().row_count();
        let out = click_at(&mut pane, &mut tree, on_header);
        assert!(out.handled);
        assert!(!out.named, "toggling a band is not a document write");
        assert_eq!(
            pane.timeline().group_at_row(band).map(|(_, e)| e),
            Some(!expanded),
            "the click did not toggle the band"
        );
        let lines_after = pane.timeline().row_count();
        assert_ne!(
            lines_before, lines_after,
            "the band's changes neither appeared nor went away"
        );

        // And back, addressing the same band by the same key.
        click_at(&mut pane, &mut tree, on_header);
        assert_eq!(
            pane.timeline().group_at_row(band).map(|(_, e)| e),
            Some(expanded)
        );
        assert_eq!(pane.timeline().row_count(), lines_before);
        assert_eq!(
            pane.timeline().group_at_row(band).map(|(k, _)| k),
            Some(key.as_str()),
            "the band moved under the toggle"
        );
    }

    #[test]
    fn keyboard_traverses_wraps_and_activates_modal_controls() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        paint(&mut pane);
        let modifiers = Modifiers::default();

        let home = pane.handle_event(
            &key_press(winit::keyboard::Key::Named(winit::keyboard::NamedKey::Home)),
            1.0,
            &modifiers,
            None,
            &mut tree,
        );
        assert!(home.handled);
        assert_eq!(
            pane.focused_control_label().as_deref(),
            Some("This workspace")
        );

        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::ArrowRight,
            )),
            1.0,
            &modifiers,
            None,
            &mut tree,
        );
        assert_eq!(
            pane.focused_control_label().as_deref(),
            Some("All workspaces")
        );
        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Enter,
            )),
            1.0,
            &modifiers,
            None,
            &mut tree,
        );
        assert_eq!(pane.scope(), RoomScope::AllWorkspaces);

        // End addresses the last rendered control; Tab wraps within this
        // modal instead of leaking keyboard focus into the desktop beneath it.
        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(winit::keyboard::NamedKey::End)),
            1.0,
            &modifiers,
            None,
            &mut tree,
        );
        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab)),
            1.0,
            &modifiers,
            None,
            &mut tree,
        );
        assert_eq!(
            pane.focused_control_label().as_deref(),
            Some("This workspace")
        );
    }

    #[test]
    fn catch_up_outcomes_expand_from_the_keyboard() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_view(ActivityView::CatchUp);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        paint(&mut pane);
        assert!(
            !pane.brief_rects.is_empty(),
            "fixture has catch-up outcomes"
        );

        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(winit::keyboard::NamedKey::End)),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );
        let focused = pane.focused_control_label().expect("last outcome focused");
        let last = pane.brief_rects.last().expect("last outcome").0;
        assert_eq!(focused, pane.digest.briefs[last].title);

        let out = pane.handle_event(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Space,
            )),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );
        assert!(out.handled);
        assert_eq!(pane.expanded_brief, Some(last));
    }

    #[test]
    fn explorer_lenses_and_detail_actions_are_in_the_keyboard_focus_order() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_view(ActivityView::Explore);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        paint(&mut pane);
        assert!(!pane.lens_rects.is_empty());

        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(winit::keyboard::NamedKey::End)),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );
        let label = pane
            .focused_control_label()
            .expect("last explorer control focused");
        assert!(
            pane.lens_rects.iter().any(|(lens, _)| match lens {
                LensSelection::All => label == "Everything",
                LensSelection::AgentTasks => label == "Agent tasks",
                LensSelection::App(value)
                | LensSelection::ResourceKind(value)
                | LensSelection::Resource(value)
                | LensSelection::WorkGroup(value) => value == &label,
                LensSelection::Facet(key, value) => format!("{key}: {value}") == label,
                LensSelection::Unclassified => label == "Unclassified",
            }) || pane
                .detail_action_rects
                .iter()
                .any(|(action, _)| pane.detail_action_label(*action) == label),
            "focused control is a rendered explorer control: {label}"
        );
        assert!(
            pane.handle_event(
                &key_press(winit::keyboard::Key::Named(
                    winit::keyboard::NamedKey::Enter,
                )),
                1.0,
                &Modifiers::default(),
                None,
                &mut tree,
            )
            .handled
        );
    }

    #[test]
    fn accessibility_snapshot_matches_visible_focus_selection_and_disclosure() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_view(ActivityView::CatchUp);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        paint(&mut pane);
        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(winit::keyboard::NamedKey::End)),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );
        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Enter,
            )),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );

        let snapshot = pane.accessibility_snapshot();
        assert_eq!(snapshot.role, ui::a11y::Role::Window);
        assert!(!snapshot.state.modal);
        assert!(
            !snapshot
                .children
                .iter()
                .any(|node| node.role == ui::a11y::Role::Tab),
            "the friendly recap does not expose full-history mode tabs"
        );
        let focused = snapshot
            .children
            .iter()
            .find(|node| node.state.focused)
            .expect("one control carries semantic focus");
        assert_eq!(focused.name, pane.focused_control_label());
        assert_eq!(focused.state.expanded, Some(true));
        assert!(
            snapshot
                .children
                .iter()
                .take(pane.focus_order().len())
                .all(|node| node.state.focusable)
        );
        assert!(
            !snapshot.children.iter().any(|node| {
                node.role == ui::a11y::Role::List && node.name.as_deref() == Some("History entries")
            }),
            "the hidden Explorer timeline is not exposed from Catch up"
        );
        pane.set_view(ActivityView::Explore);
        let snapshot = pane.accessibility_snapshot();
        assert!(snapshot.children.iter().any(|node| {
            node.role == ui::a11y::Role::List && node.name.as_deref() == Some("History entries")
        }));
    }

    #[test]
    fn renaming_through_the_chip_replaces_the_name_rather_than_adding_a_band() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        paint(&mut pane);

        // Name it once, through the gesture.
        let band = first_band(&pane);
        let (chip, _) = pane.timeline().group_chip_rects(band).unwrap();
        click_at(&mut pane, &mut tree, chip.center());
        type_into(&mut pane, &mut tree, "First try");
        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Enter,
            )),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );
        let record = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new())[0]
            .id
            .clone();

        // Now rename it. The field seeds from the existing name, and the write
        // has to land on the record that already exists.
        paint(&mut pane);
        let band = (0..pane.timeline().row_count())
            .find(|i| pane.timeline().group_title_at_row(*i) == Some("First try"))
            .expect("the named band is on screen");
        let (chip, _) = pane.timeline().group_chip_rects(band).unwrap();
        click_at(&mut pane, &mut tree, chip.center());
        assert_eq!(
            pane.timeline().naming_buffer(),
            Some("First try"),
            "a rename starts from the name that is there"
        );
        type_into(&mut pane, &mut tree, " again");
        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Enter,
            )),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );

        let ranges = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new());
        assert_eq!(
            ranges.len(),
            1,
            "a rename must not leave two bands: {ranges:?}"
        );
        assert_eq!(
            ranges[0].id, record,
            "it addressed the record it already had"
        );
        assert_eq!(ranges[0].label, "First try again");
    }

    #[test]
    fn overflow_controls_make_every_filter_page_keyboard_reachable() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_view(ActivityView::Explore);
        pane.contributors = (0..12).map(|i| format!("Contributor {i}")).collect();
        for i in 0..18 {
            pane.facet_index
                .apps
                .insert(format!("Application {i}"), i + 1);
        }
        pane.layout(Rect::new(0.0, 0.0, 1000.0, 520.0));
        paint(&mut pane);

        assert!(
            pane.author_more_rect.is_some(),
            "contributors expose a More control"
        );
        assert!(
            pane.lens_more_rect.is_some(),
            "short browse panel exposes a More control"
        );
        assert!(pane.focus_order().contains(&ActivityFocus::MorePeople));
        assert!(pane.focus_order().contains(&ActivityFocus::MoreLenses));
        let first_people = pane.author_rects.clone();
        pane.advance_people();
        paint(&mut pane);
        assert_ne!(
            first_people, pane.author_rects,
            "More people advances the page"
        );
        let first_lenses = pane.lens_rects.clone();
        pane.advance_lenses();
        paint(&mut pane);
        assert_ne!(
            first_lenses, pane.lens_rects,
            "More lenses advances the page"
        );
        assert!(
            pane.lens_rects
                .iter()
                .all(|(_, rect)| rect.y1 <= pane.sheet.y1)
        );
    }

    #[test]
    fn short_catch_up_has_a_more_outcomes_affordance() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_view(ActivityView::CatchUp);
        let first = pane
            .digest
            .briefs
            .first()
            .cloned()
            .expect("fixture outcome");
        while pane.digest.briefs.len() < 5 {
            pane.digest.briefs.push(first.clone());
        }
        pane.layout(Rect::new(0.0, 0.0, 1000.0, 390.0));
        paint(&mut pane);
        assert!(
            pane.brief_more_rect.is_some(),
            "outcomes are paged, not silently clipped"
        );
        assert!(pane.focus_order().contains(&ActivityFocus::MoreBriefs));
        let before = pane.brief_rects.clone();
        pane.advance_briefs();
        paint(&mut pane);
        assert_ne!(before, pane.brief_rects);
    }

    #[test]
    fn selected_named_bands_can_be_split_and_merged() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let entries = pane.entries().to_vec();
        assert!(pane.apply_name(
            &mut tree,
            &GroupNameRequest {
                record_id: None,
                start: entries[1].id,
                end: entries[3].id,
                label: "Investigate".to_string(),
            },
        ));
        pane.timeline_mut().set_selection(Some((2, 2)));
        assert!(pane.split_selection_group(&mut tree));
        let split = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new());
        assert_eq!(split.len(), 2);
        assert!(
            split
                .iter()
                .any(|range| { range.start == entries[1].id && range.end == Some(entries[1].id) })
        );
        assert!(split.iter().any(|range| range.start == entries[2].id));

        pane.timeline_mut().set_selection(Some((1, 3)));
        assert!(pane.merge_selection_groups(&mut tree));
        let merged = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new());
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].start, entries[1].id);
        assert_eq!(merged[0].end, Some(entries[3].id));
    }

    #[test]
    fn catch_up_redraws_work_session_boundaries_without_rewriting_history() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let entries = pane.entries().to_vec();
        assert!(entries.len() >= 4);
        let original_ids = entries.iter().map(|entry| entry.id).collect::<Vec<_>>();
        assert!(pane.apply_name(
            &mut tree,
            &GroupNameRequest {
                record_id: None,
                start: entries[0].id,
                end: entries[1].id,
                label: "Write Q3 financial report".into(),
            },
        ));
        assert!(pane.apply_name(
            &mut tree,
            &GroupNameRequest {
                record_id: None,
                start: entries[2].id,
                end: entries[3].id,
                label: "Work on onboarding Sarah".into(),
            },
        ));
        pane.set_view(ActivityView::CatchUp);

        let older = pane
            .digest
            .briefs
            .iter()
            .position(|brief| brief.title == "Write Q3 financial report")
            .expect("older named work session is projected into Catch up");
        let newer = pane
            .digest
            .briefs
            .iter()
            .position(|brief| brief.title == "Work on onboarding Sarah")
            .expect("newer named work session is projected into Catch up");
        assert!(pane.move_work_session_boundary(
            &mut tree,
            WorkSessionBoundary {
                older,
                newer,
                direction: BoundaryDirection::IntoNewer,
            },
        ));

        let ranges = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new());
        let finance = ranges
            .iter()
            .find(|range| range.label == "Write Q3 financial report")
            .unwrap();
        let onboarding = ranges
            .iter()
            .find(|range| range.label == "Work on onboarding Sarah")
            .unwrap();
        assert_eq!(finance.start, entries[0].id);
        assert_eq!(finance.end, Some(entries[0].id));
        assert_eq!(onboarding.start, entries[1].id);
        assert_eq!(onboarding.end, Some(entries[3].id));
        assert_eq!(
            pane.entries()
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            original_ids,
            "redrawing a work-session seam never rewrites or reorders actions"
        );
        assert_eq!(
            pane.digest
                .briefs
                .iter()
                .find(|brief| brief.title == "Write Q3 financial report")
                .unwrap()
                .changes,
            1
        );
        assert_eq!(
            pane.digest
                .briefs
                .iter()
                .find(|brief| brief.title == "Work on onboarding Sarah")
                .unwrap()
                .changes,
            3
        );

        let older = pane
            .digest
            .briefs
            .iter()
            .position(|brief| brief.title == "Write Q3 financial report")
            .unwrap();
        let newer = pane
            .digest
            .briefs
            .iter()
            .position(|brief| brief.title == "Work on onboarding Sarah")
            .unwrap();
        assert!(
            !pane.move_work_session_boundary(
                &mut tree,
                WorkSessionBoundary {
                    older,
                    newer,
                    direction: BoundaryDirection::IntoNewer,
                },
            ),
            "a work session cannot be emptied"
        );
        assert!(pane.move_work_session_boundary(
            &mut tree,
            WorkSessionBoundary {
                older,
                newer,
                direction: BoundaryDirection::IntoOlder,
            },
        ));
    }

    #[test]
    fn first_correction_materializes_automatic_work_sessions_as_shared_ranges() {
        let mut tree = WorkspaceTree::bootstrap_default();
        for (group_id, group_label, action) in [
            ("finance", "Write Q3 financial report", "Draft totals"),
            ("finance", "Write Q3 financial report", "Check forecast"),
            ("onboarding", "Onboard Sarah", "Open welcome notes"),
            ("onboarding", "Onboard Sarah", "Assign mentor"),
        ] {
            assert!(
                tree.append_workspace_effect_receipt(
                    tree.active,
                    tilo_actions::InvocationId::new(),
                    "ada",
                    tilo_actions::ActionContext::for_resource(
                        action,
                        format!("test:{group_id}"),
                        "document",
                        group_label,
                    )
                    .with_app("org.tilo.Writer", "Writer")
                    .with_group(group_id, group_label, "work-session"),
                    tilo_actions::EffectReceiptStatus::Succeeded,
                )
            );
        }
        let mut pane = RoomActivityPane::new();
        pane.refresh(&mut tree);
        pane.set_view(ActivityView::CatchUp);
        let finance = pane
            .digest
            .briefs
            .iter()
            .position(|brief| brief.title == "Write Q3 financial report")
            .expect("automatic finance session");
        let onboarding = pane
            .digest
            .briefs
            .iter()
            .position(|brief| brief.title == "Onboard Sarah")
            .expect("automatic onboarding session");
        assert_eq!(
            pane.digest.briefs[finance]
                .authors
                .iter()
                .map(|author| author.name.as_str())
                .collect::<Vec<_>>(),
            vec!["ada"],
            "receipt-backed history is attributed to its authenticated action actor, not the room host"
        );
        assert!(pane.digest.briefs[finance].work_session_id.is_none());
        assert!(pane.digest.briefs[onboarding].work_session_id.is_none());
        let finance_first = pane
            .entry_position(&pane.digest.briefs[finance].entry_ids[0])
            .unwrap();
        let onboarding_first = pane
            .entry_position(&pane.digest.briefs[onboarding].entry_ids[0])
            .unwrap();
        let (older, newer) = if finance_first < onboarding_first {
            (finance, onboarding)
        } else {
            (onboarding, finance)
        };

        assert!(pane.move_work_session_boundary(
            &mut tree,
            WorkSessionBoundary {
                older,
                newer,
                direction: BoundaryDirection::IntoNewer,
            },
        ));
        let ranges = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new());
        assert!(
            ranges
                .iter()
                .any(|range| range.label == "Write Q3 financial report")
        );
        assert!(ranges.iter().any(|range| range.label == "Onboard Sarah"));
        assert_eq!(
            pane.digest
                .briefs
                .iter()
                .find(|brief| brief.title == "Write Q3 financial report")
                .unwrap()
                .changes
                + pane
                    .digest
                    .briefs
                    .iter()
                    .find(|brief| brief.title == "Onboard Sarah")
                    .unwrap()
                    .changes,
            4,
            "materializing and moving a seam must preserve every atomic action"
        );
    }
}
