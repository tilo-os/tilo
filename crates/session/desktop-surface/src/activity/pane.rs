use super::morning::{
    MORNING_OPEN, MORNING_REMOVE, MORNING_SOURCE, MorningCommand, MorningPreferences, MorningSource,
};
mod morning_sources;
mod task_confirmation_view;
mod task_review_view;
pub use task_confirmation_view::ConfirmationNotice;

// Room activity — everything that has happened to this workspace, by
// everyone, in one filterable pane.
//
// The room's state lives in [`DesktopTree::doc`], a tree-wide
// automerge document holding every workspace on this machine, so
// [`RoomScope`] decides which workspaces' changes are shown — a pane
// titled with one room's name must never render another's governance.
// Not shown: the contents of documents open inside the room, which the
// broker routes as opaque bytes it never materialises.
//
// A row's `[Revert]` writes the delta back as a **new forward change**;
// history is appended to, never rewritten. Governance rows
// ([`EntryKind::Access`]) are non-revertible — an owner counters a
// governance act with another one rather than rewinding it.

use super::replay::{ReplayPointer, WorkspaceReplayFrame};
use super::{
    ActivityDeepLink, ActivityHost, ActivityTree, AgentLaunchEvent, AgentLaunchEventKind,
    UnavailableActivityHost,
};

use crate::i18n::{t, t_args};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::rc::Rc;
use ui::i18n::ArgValue;
use ui::time::{Duration, Instant, SystemTime};

use automerge::transaction::Transactable;
use automerge::{ChangeHash, Prop, ReadDoc, ScalarValue, Value};
use collab_types::caps::{Access, CapOverride, Effect, Principal, Scope, Subject};
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::EntryKind;
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

use super::index::{
    FeedCoverage, WorkspaceDeparture, WorkspaceHistoryEntry, WorkspaceHistoryId,
    WorkspaceHistoryIndex, WorkspaceHistoryQuery, WorkspaceSeenFrontier,
};
use tilo_desktop_model::{
    WorkspaceTask, WorkspaceTaskResource, WorkspaceTaskRoute, WorkspaceTaskStatus,
};
use ui::TextRole;
use ui::widget::ButtonRole;

/// Widest the sheet ever gets. Past this the rows stop gaining information
/// and start becoming hard to scan across.
/// How long a device must be away from a workspace before its return is
/// greeted with a recap of what changed.
const AWAY_RECAP_AFTER: Duration = Duration::from_secs(45 * 60);
/// How long an expanding catch-up card takes to open.
const BRIEF_EXPANSION_DURATION: Duration = Duration::from_millis(180);

fn away_recap_is_due(departed_at_secs: u64, now: SystemTime) -> bool {
    let departed_at = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(departed_at_secs))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    now.duration_since(departed_at)
        .is_ok_and(|elapsed| elapsed >= AWAY_RECAP_AFTER)
}

fn seed_roster_colours(authors: &mut AuthorDirectory, tree: &tilo_desktop_model::DesktopTree) {
    let people = tree.collaborators.iter().chain(
        tree.workspaces
            .iter()
            .flat_map(|ws| ws.collaborators.iter()),
    );
    for person in people {
        if person.name.trim().is_empty() {
            continue;
        }
        let rgb = person
            .cursor_color
            .as_deref()
            .and_then(ui::presence::parse_hex_color)
            .unwrap_or_else(|| authors.colour_for(&person.id));
        authors.set_color_override(&person.id, rgb);
        let handle = automerge::ActorId::from(person.id.as_bytes()).to_hex_string();
        authors.register(
            &handle,
            ui::history_source::author_coloured(
                participant_display_label(&person.name),
                ui::history_source::short_id(&person.id),
                rgb,
            ),
        );
    }
}

const SHEET_MAX_W: f64 = 560.0;
/// Narrowest the sheet gets before it takes the whole viewport instead —
/// below this the content behind it is too thin to be worth preserving
/// (HIG's content-panel rule: show one thing at a time).
const SHEET_MIN_W: f64 = 360.0;
const FULL_BLEED_UNDER: f64 = 720.0;
const SHEET_INSET: f64 = 16.0;
/// The header band at 100 % text: `HEADER_TOP` + a `Heading` title line
/// (20.8) + two `Caption` lines (subtitle, "Latest", 16.2 each) +
/// `HEADER_BOTTOM`, rounded up to a whole pixel. The live band is
/// [`RoomActivityPane::header_h`], which grows with the text scale and is
/// never shorter than this.
const HEADER_H: f64 = 80.0;
/// Title top, from the sheet's top edge.
const HEADER_TOP: f64 = 12.0;
/// Air under the "Latest" line before the view tabs.
const HEADER_BOTTOM: f64 = 14.0;
const VIEW_ROW_H: f64 = 34.0;
/// A filter row is one pointer target tall (`MIN_TARGET`), with the 28 px
/// Selector pill centred in it.
const CHIP_ROW_H: f64 = 32.0;
/// Selector height (HIG §9.6): every chip in this sheet — view tab, scope
/// pill, period and author filter — is painted at this height.
const SELECTOR_H: f64 = 28.0;
/// Minimum pointer target (HIG §9.6). A chip's hit rect is at least this
/// tall even where its painted pill is the 28 px Selector.
const MIN_TARGET: f64 = 32.0;
/// Facet written onto setup, view and layout rows so the feed can omit
/// them without dropping them from replay.
const FEED_OMIT_FACET: &str = "tilo.feed";
const EXPLORER_LEFT_W: f64 = 180.0;
const EXPLORER_DETAIL_W: f64 = 240.0;

pub const HISTORY_COMPARE: &str = "history.compare";
pub const HISTORY_UNDO: &str = "history.undo";
pub const HISTORY_RESTORE: &str = "history.restore";
pub const ACTIVITY_ACKNOWLEDGE: &str = "room.activity.acknowledge";
pub const ACTIVITY_TASKS: &str = "room.activity.tasks";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
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

    fn label(self) -> String {
        t(match self {
            Self::CatchUp => "activity-view-catch-up",
            Self::Tasks => "activity-view-tasks",
            Self::Explore => "activity-view-explore",
            Self::Replay => "activity-view-replay",
        })
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
    Morning(String, bool),
    MorningCommand(String, MorningCommand),
    Brief(usize),
    WorkSessionBoundary(WorkSessionBoundary),
    MoreBriefs,
    Acknowledge,
    NewTask,
    Task(String),
    TaskRoute(WorkspaceTaskRoute),
    TaskCreate,
    TaskCancelCompose,
    TaskAction(TaskAction),
    DirectionField,
    /// An editable field of the paused action's draft, by its place.
    ConfirmField(usize),
    /// The reason given for declining the paused action.
    DeclineReason,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskAction {
    ApproveRun,
    ReviewHistory,
    Steer,
    CancelRun,
    RetryRun,
    SendDirection,
    CloseDirection,
    NextTasks,
    PreviousTasks,
    ConfirmDraft,
    EditDraft,
    DiscardEdits,
    DeclineDraft,
    SendDecline,
    KeepDraft,
    AlwaysAllow,
    OpenResource,
    StartAgent,
    Launch,
    Start,
    Complete,
    Cancel,
}

impl TaskAction {
    fn label(self) -> String {
        t(match self {
            Self::ApproveRun => "activity-task-approve-run",
            Self::ReviewHistory => "activity-task-review-history",
            Self::Steer => "activity-task-steer",
            Self::CancelRun => "activity-task-cancel-run",
            Self::RetryRun => "activity-task-retry-run",
            Self::SendDirection => "activity-task-direction-send",
            Self::CloseDirection => "activity-task-direction-close",
            Self::NextTasks => "activity-task-next",
            Self::PreviousTasks => "activity-task-previous",
            Self::ConfirmDraft => "activity-confirm-confirm",
            Self::EditDraft => "activity-confirm-edit",
            Self::DiscardEdits => "activity-confirm-discard",
            Self::DeclineDraft => "activity-confirm-decline",
            Self::SendDecline => "activity-confirm-send-decline",
            Self::KeepDraft => "activity-confirm-back",
            Self::AlwaysAllow => "activity-confirm-always",
            Self::OpenResource => "activity-task-open-resource",
            Self::StartAgent => "activity-task-start-agent",
            Self::Launch => "activity-task-launch-agent",
            Self::Start => "activity-task-mark-started",
            Self::Complete => "activity-task-mark-done",
            Self::Cancel => "activity-task-cancel",
        })
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
    MorningSource,
    Open,
    Compare,
    Restore,
    Undo,
    UndoTask,
    Split,
    Merge,
}

impl DetailAction {
    fn label(self) -> String {
        t(match self {
            Self::MorningSource => "activity-morning-choose",
            Self::Open => "activity-detail-open",
            Self::Compare => "activity-detail-compare",
            Self::Restore => "activity-detail-restore",
            Self::Undo => "activity-detail-undo",
            Self::UndoTask => "activity-detail-undo-task",
            Self::Split => "activity-detail-split",
            Self::Merge => "activity-detail-merge",
        })
    }
}

/// A durable agent-run marker and the record whose document carries it.
/// `None` is reserved for a marker in the workspace tree itself.
#[derive(Clone, Debug, PartialEq)]
struct IndexedAgentRun {
    source_record: Option<RecordId>,
    run: AgentRun,
}

/// Where the pane's task list comes from: the workspace tree, the room
/// owner's review snapshot (by identity), or nowhere while a required review
/// is unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskListOrigin {
    Tree,
    Review(usize),
    Withheld,
}

/// What [`RoomActivityPane::tasks`] was last built from, so an unchanged
/// list is not cloned and sorted again on every refresh.
#[derive(Debug)]
struct TaskListSource {
    room: String,
    origin: TaskListOrigin,
    tree_tasks: Vec<WorkspaceTask>,
}

/// The agent runs a persisted record held at one revision, so an unchanged
/// document on disk is not read and decoded again on every refresh.
#[derive(Debug)]
struct ColdAgentRuns {
    revision: u64,
    runs: Vec<AgentRun>,
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

    fn label(self) -> String {
        t(match self {
            Self::Timeline => "activity-pane-timeline",
            Self::Browse => "activity-pane-browse",
            Self::Details => "activity-pane-details",
        })
    }
}

/// The local user's most recent change that names a resource, for the
/// "you left off" card.
struct ResumeSnapshot {
    target: ui::HistoryResourceTarget,
    author: ui::widget::history::HistoryAuthor,
    app: Option<String>,
    timestamp: SystemTime,
}

fn author_is_local(author: &ui::widget::history::HistoryAuthor, local: &str) -> bool {
    if local.is_empty() {
        return false;
    }
    author.name.eq_ignore_ascii_case(local)
        || author
            .name
            .eq_ignore_ascii_case(&participant_display_label(local))
        || author.identity_short == ui::history_source::short_id(local)
}

/// The unread rows someone else wrote. Catch up answers "what did others
/// do while I was away"; replaying the reader's own writes, including the
/// desktop's first setup, answers nothing.
fn others_work(rows: Vec<&WorkspaceHistoryEntry>, local: &str) -> Vec<HistoryEntry> {
    rows.into_iter()
        .map(WorkspaceHistoryEntry::timeline_entry)
        .filter(|entry| !author_is_local(&entry.author, local))
        .filter(is_feed_change)
        .collect()
}

fn is_feed_change(entry: &HistoryEntry) -> bool {
    !entry.context.as_ref().is_some_and(|context| {
        !context.is_activity()
            || context
                .facets
                .iter()
                .any(|facet| facet.key == FEED_OMIT_FACET && facet.value == "omit")
    })
}

fn local_resume_snapshot(entries: &[HistoryEntry], local: &str) -> Option<ResumeSnapshot> {
    entries
        .iter()
        .filter(|entry| author_is_local(&entry.author, local))
        .filter_map(|entry| {
            let context = entry.context.as_ref()?;
            let resource = context.resource.as_ref()?;
            let mut target = ui::HistoryResourceTarget {
                app_id: context.app.as_ref().map(|app| app.id.clone()),
                app_label: context.app.as_ref().map(|app| app.label.clone()),
                id: resource.id.clone(),
                kind: resource.kind.clone(),
                label: resource.label.clone(),
            };
            // A resource known only by its id has nothing to offer by name.
            let label = target.display_name()?.to_owned();
            target.label = label;
            Some(ResumeSnapshot {
                target,
                author: entry.author.clone(),
                app: context.app.as_ref().map(|app| app.label.clone()),
                timestamp: entry.timestamp,
            })
        })
        .max_by_key(|resume| resume.timestamp)
}

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
        colors.text_primary.with_alpha_factor(0.13),
        None,
        &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.control)),
    );
    scene.fill(
        Fill::NonZero,
        transform,
        colors.intent.inverted,
        None,
        &RoundedRect::new(rect.x0, rect.y0 + 8.0, rect.x0 + 3.0, rect.y1 - 8.0, 1.5),
    );
    let (r, g, b) = resume.author.rgb();
    let initial = resume
        .author
        .name
        .chars()
        .next()
        .unwrap_or('\u{2022}')
        .to_uppercase()
        .collect::<String>();
    ui::widget::AvatarFace::new(&initial, "")
        .presence(vello::peniko::Color::from_rgba8(r, g, b, 255))
        .paint(
            &mut ui::paint::PaintCtx::new(scene, transform, tr, theme),
            Rect::new(
                rect.x0 + 12.0,
                rect.center().y - 12.0,
                rect.x0 + 36.0,
                rect.center().y + 12.0,
            ),
        );
    tr.draw_style(
        scene,
        &t("activity-resume-heading"),
        colors.intent.inverted_hover,
        at(transform, rect.x0 + 46.0, rect.y0 + 8.0),
        None,
        &theme.text(TextRole::Caption),
    );
    // The button fits its label ("Fortsetzen →" is wider than "Resume →");
    // the title and context take what it leaves.
    let button_label = t("activity-resume-button");
    let (label_w, _) = tr.measure_style(&button_label, None, &theme.text(TextRole::Label));
    let button_w = (f64::from(label_w) + 24.0).max(74.0);
    let button_h = ui::widget::ButtonFace::height(ui::widget::ControlSize::Regular, theme);
    let (_, caption_h) = tr.measure_style("Mg", None, &theme.text(TextRole::Caption));
    let (_, title_h) = tr.measure_style("Mg", None, &theme.text(TextRole::Label));
    let title_y = rect.y0 + 12.0 + f64::from(caption_h);
    let context_y = title_y + 4.0 + f64::from(title_h);
    let button = Rect::new(
        rect.x1 - 10.0 - button_w,
        rect.center().y - button_h / 2.0,
        rect.x1 - 10.0,
        rect.center().y + button_h / 2.0,
    );
    let text_w = text_width((button.x0 - 12.0 - (rect.x0 + 46.0)).max(0.0));
    let title = tr.elide_to_width_style(&resume.target.label, text_w, &theme.text(TextRole::Label));
    tr.draw_style(
        scene,
        &title,
        colors.text_primary,
        at(transform, rect.x0 + 46.0, title_y),
        None,
        &theme.text(TextRole::Label),
    );
    let when = format_relative_time(resume.timestamp, now);
    let context = resume.app.as_ref().map_or_else(
        || when.clone(),
        |app| {
            t_args(
                "activity-resume-context",
                &[("app", app.into()), ("when", when.as_str().into())],
            )
        },
    );
    let context = tr.elide_to_width_style(&context, text_w, &theme.text(TextRole::Caption));
    tr.draw_style(
        scene,
        &context,
        colors.text_dim,
        at(transform, rect.x0 + 46.0, context_y),
        None,
        &theme.text(TextRole::Caption),
    );
    ui::widget::Button::paint(
        scene,
        transform,
        tr,
        theme,
        &ui::widget::ButtonFace::new(button, &button_label).role(ButtonRole::Primary),
    );
}

/// Compose `transform` with a translation to `(dx, dy)`. `Affine`'s `Mul`
/// is pure `f64` matrix composition — it cannot overflow or panic — but
/// `arithmetic_side_effects` can't see that for a non-primitive `Mul`
/// impl, so every call site funnels through this one function instead of
/// an `#[allow]` at each of them.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix multiplication; it cannot overflow or panic"
)]
fn at(transform: Affine, dx: f64, dy: f64) -> Affine {
    transform * Affine::translate((dx, dy))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "layout widths are far inside f32 range, the text renderer's unit"
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
        .map(ui::format::capitalize_first)
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

    pub fn label(self) -> String {
        t(match self {
            Period::AllTime => "activity-period-all-time",
            Period::Today => "activity-period-today",
            Period::Week => "activity-period-week",
            Period::Month => "activity-period-month",
        })
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

    pub fn label(self) -> String {
        t(match self {
            RoomScope::ThisRoom => "activity-scope-this-workspace",
            RoomScope::AllWorkspaces => "activity-scope-all-workspaces",
        })
    }
}

/// What a dispatched event did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeManualTaskRequest {
    pub workspace: tilo_desktop_model::WorkspaceId,
    pub room: String,
    pub title: String,
    pub actor: String,
    pub resource: Option<WorkspaceTaskResource>,
}

/// Deferred managed creation; selection grants neither actor authority nor a run lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeManagedTaskRequest {
    pub person_draft: NativeManualTaskRequest,
    pub assigned_agent: String,
    pub route: WorkspaceTaskRoute,
}

/// A task run to hand to its agent once the caller has released the tree.
/// Dispatch goes through the action broker, which reads the workspace tree
/// itself to check the room's authority, so it cannot run while the event
/// that asked for it still holds the tree for writing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskDispatchRequest {
    pub room: String,
    pub task_id: String,
    pub assigned_agent: Option<String>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "these independent flags report distinct effects to the overlay host"
)]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RoomActivityOutcome {
    pub create_native_manual: Option<NativeManualTaskRequest>,
    pub create_native_managed: Option<NativeManagedTaskRequest>,
    /// Run with [`RoomActivityPane::dispatch_task`] after releasing the tree.
    pub dispatch_task: Option<TaskDispatchRequest>,
    pub handled: bool,
    pub reverted: bool,
    /// A band was named/renamed/un-named — a document write too.
    pub named: bool,
    pub open_target: Option<ui::HistoryResourceTarget>,
    pub compared: bool,
    pub undo_partial: bool,
    pub task_changed: bool,
    pub task_refused: bool,
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
    /// [`Self::history_log`]'s rows revision at the last rebuild: while it
    /// holds, the changes since were all passed over (a workspace switch)
    /// and the rows are as derived.
    last_rows: u64,
    period: Period,
    /// Every row the document yielded, with the workspace each belongs
    /// to (`None` for no single workspace). [`Self::scope`] selects the
    /// subset handed to the timeline.
    derived: Vec<(HistoryEntry, Option<u64>)>,
    /// Tree rows after workspace scoping, retained as the shell-document
    /// contribution to the workspace-wide multi-document index.
    scoped_tree_rows: Vec<HistoryEntry>,
    history_index: WorkspaceHistoryIndex,
    detached_records: Vec<super::index::DetachedRecordHistory>,
    projected_origins: HashMap<HistoryEntryId, WorkspaceHistoryId>,
    /// One app-native event supplied by an explicit handoff (for example a
    /// signed Files version). It remains visible while underlying indexes
    /// catch up, but is excluded from unseen counts and Catch up prose.
    external_entry: Option<HistoryEntry>,
    feed_coverage: FeedCoverage,
    pub(super) scope: RoomScope,
    scope_rects: Vec<(RoomScope, Rect)>,
    #[cfg(test)]
    painted_title: RefCell<String>,
    author: Option<String>,
    /// First-seen order, derived from the rows rather than a roster — a
    /// name appears only if that person did something in this room.
    contributors: Vec<String>,
    sheet: Rect,
    /// The viewport `layout` was last given, so `draw` can lay the sheet
    /// out again when the theme's text scale changes the header's height.
    viewport: Rect,
    /// Whether `viewport` belongs to an ordinary tiled Activity window or
    /// the right-anchored overlay layout.
    window_layout: bool,
    /// The user's text scale as of the last draw. The header band is sized
    /// from it (`header_h`), so 200 % type has room instead of the title,
    /// subtitle and "Latest" line stacking on top of one another.
    text_scale: f32,
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
    /// Exact addresses exposed by a painted catch-up page, partitioned before
    /// workspace switches. Never infer visibility from the index's heads.
    viewed_changes: HashMap<u64, BTreeSet<WorkspaceHistoryId>>,
    review_clip: Option<Rect>,
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
    passive_rows: Vec<HistoryEntry>,
    persistent_frontiers: bool,
    view: ActivityView,
    view_rects: Vec<(ActivityView, Rect)>,
    brief_rects: Vec<(usize, Rect)>,
    acknowledge_rect: Option<Rect>,
    brief_more_rect: Option<Rect>,
    boundary_rects: Vec<(WorkSessionBoundary, Rect)>,
    /// Where the "you left off" card was drawn and what it reopens.
    resume_rect: Rect,
    resume_target: Option<ui::HistoryResourceTarget>,
    morning_sources: HashMap<u64, MorningPreferences>,
    morning_workspace: u64,
    morning_rects: Vec<((String, bool), Rect)>,
    morning_command_rects: Vec<((String, MorningCommand), Rect)>,
    morning_command_message: Option<String>,
    morning_source_offset: usize,
    brief_more_advance: usize,
    brief_offset: usize,
    brief_expansion_started: Option<(usize, Instant)>,
    expanded_brief: Option<usize>,
    tasks: Vec<WorkspaceTask>,
    task_source: Option<TaskListSource>,
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
    host: Rc<dyn ActivityHost>,
    task_room: String,
    task_review: Option<Result<std::sync::Arc<super::TaskReviewSnapshot>, String>>,
    review_approval: Option<task_review_view::ApprovalFrontier>,
    direction: Option<task_review_view::DirectionDraft>,
    confirmation: Option<task_confirmation_view::ConfirmationReview>,
    confirmation_notices: Vec<ConfirmationNotice>,
    noticed_confirmations: HashSet<String>,
    review_pending: Option<uuid::Uuid>,
    review_notice: Option<String>,
    review_task_offset: usize,
    requested_review_task: Option<String>,
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
    pub(super) replay_unavailable: usize,
    replay_presentation_note: Option<String>,
    pub(super) replay_frames: Vec<WorkspaceReplayFrame>,
    /// What [`Self::source`] has read of the room's change log, kept across
    /// refreshes: a refresh reads only the changes appended since the last
    /// one ([`HistorySource::entries_since`]).
    pub(super) history_log: ui::history_source::HistoryLog,
    /// Each room change's reading, kept across refreshes; see
    /// [`describe_room_changes`].
    room_changes: HashMap<ChangeHash, RoomChange>,
    /// Each replayed entry's desktop, kept across rebuilds; see
    /// [`Self::rebuild_replay_frames`].
    pub(super) replay_checkpoints: HashMap<HistoryEntryId, super::replay::ReplayCheckpoint>,
    pub(super) replay_index: usize,
    pub(super) replay_playing: bool,
    replay_last_advance: Instant,
    replay_back_rect: Rect,
    replay_play_rect: Rect,
    replay_forward_rect: Rect,
    narrative: Option<ui::GroundedNarrative>,
    narrative_request: Option<NarrativeRequest>,
    /// Task receipts live beside the records they describe. They are indexed
    /// here so selecting any affected change can explain the whole task.
    agent_runs: Vec<IndexedAgentRun>,
    cold_agent_runs: HashMap<RecordId, ColdAgentRuns>,
    agent_undo_notice: Option<(String, String)>,
    open_agent_records: HashSet<RecordId>,
}

impl Default for RoomActivityPane {
    fn default() -> Self {
        Self::new()
    }
}

impl RoomActivityPane {
    /// Install host services. Uninstalled services remain unavailable.
    pub fn set_host(&mut self, host: Rc<dyn ActivityHost>) {
        self.timeline
            .set_preview_selection(!host.mutations_available());
        if !host.mutations_available() {
            self.close_task_composer();
            self.new_task_rect = None;
            self.task_action_rects.clear();
            self.task_route_rects.clear();
            self.task_create_rect = None;
            self.task_cancel_rect = None;
            self.revert_intent.take();
            self.name_intent.borrow_mut().take();
            self.undo_intent.borrow_mut().take();
        }
        self.host = host;
    }

    fn detail_action_label(&self, action: DetailAction) -> String {
        if action == DetailAction::MorningSource {
            return t(
                if self
                    .selection_detail()
                    .and_then(|detail| detail.resources.first().cloned())
                    .is_some_and(|target| {
                        self.chosen_morning_sources()
                            .iter()
                            .any(|source| source.matches(&target))
                    })
                {
                    "activity-morning-remove"
                } else {
                    "activity-morning-choose"
                },
            );
        }
        if action == DetailAction::Open
            && self.selection_detail().is_some_and(|detail| {
                detail.resources.first().is_some_and(|resource| {
                    !resource.id.starts_with("file:") && !resource.id.contains("://")
                })
            })
        {
            t("activity-detail-open-app")
        } else {
            action.label()
        }
    }

    pub fn new() -> Self {
        Self::from_local_user(None)
    }

    pub fn with_local_user(local: String) -> Self {
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
                    .with_preview_selection()
                    .on_revert(move |id| ri.set(Some(*id)))
                    .on_undo_band(move |req| *ui.borrow_mut() = Some(req.clone()))
                    .on_clear_filters(move || ci.set(true))
                    .on_name_group(move |req| *ni.borrow_mut() = Some(req.clone()));
                t.set_draws_own_surface(false);
                // The shell owns the Everyone/person selector directly above
                // the timeline. Keep the operation chips, but never repeat an
                // `author:any` row inside the embedded widget.
                t.set_show_author_pill(false);
                t.set_show_day_headings(true);
                t
            },
            // A workspace is a cluster of windows saved/reopened collaboratively.
            // Which workspace shows is the device's view, written on every
            // switch: no history, and no place in the window, which switching
            // back and forth used to fill until nothing anyone did was left.
            source: HistorySource::new()
                .with_artifact_kind(ArtifactKind::Cluster)
                .without_field_paths()
                .passing_over(tilo_desktop_model::desktop_doc::only_sets_active),
            revert_intent,
            name_intent,
            undo_intent,
            clear_filters_intent,
            named: Vec::new(),
            last_heads: Vec::new(),
            last_rows: 0,
            period: Period::default(),
            derived: Vec::new(),
            scoped_tree_rows: Vec::new(),
            history_index: WorkspaceHistoryIndex::new(),
            detached_records: Vec::new(),
            projected_origins: HashMap::new(),
            external_entry: None,
            feed_coverage: FeedCoverage::default(),
            scope: RoomScope::default(),
            scope_rects: Vec::new(),
            #[cfg(test)]
            painted_title: RefCell::default(),
            author: None,
            contributors: Vec::new(),
            sheet: Rect::ZERO,
            viewport: Rect::ZERO,
            window_layout: false,
            text_scale: 1.0,
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
            viewed_changes: HashMap::new(),
            review_clip: None,
            departures: HashMap::new(),
            presence_workspace: None,
            away_digest: ActivityDigest::default(),
            prepared_away_workspace: None,
            catchup_frontier: WorkspaceSeenFrontier::default(),
            passive_digest: ActivityDigest::default(),
            passive_unread: 0,
            passive_rows: Vec::new(),
            persistent_frontiers: false,
            view: ActivityView::default(),
            view_rects: Vec::new(),
            brief_rects: Vec::new(),
            acknowledge_rect: None,
            brief_more_rect: None,
            boundary_rects: Vec::new(),
            resume_rect: Rect::ZERO,
            resume_target: None,
            morning_sources: HashMap::new(),
            morning_workspace: 0,
            morning_rects: Vec::new(),
            morning_command_rects: Vec::new(),
            morning_command_message: None,
            morning_source_offset: 0,
            brief_more_advance: 0,
            brief_offset: 0,
            expanded_brief: None,
            brief_expansion_started: None,
            tasks: Vec::new(),
            task_source: None,
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
            host: Rc::new(UnavailableActivityHost),
            task_room: String::new(),
            task_review: None,
            review_approval: None,
            direction: None,
            confirmation: None,
            confirmation_notices: Vec::new(),
            noticed_confirmations: HashSet::new(),
            review_pending: None,
            review_notice: None,
            review_task_offset: 0,
            requested_review_task: None,
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
            replay_unavailable: 0,
            replay_presentation_note: None,
            replay_frames: Vec::new(),
            history_log: ui::history_source::HistoryLog::default(),
            room_changes: HashMap::new(),
            replay_checkpoints: HashMap::new(),
            replay_index: 0,
            replay_playing: false,
            replay_last_advance: Instant::now(),
            replay_back_rect: Rect::ZERO,
            replay_play_rect: Rect::ZERO,
            replay_forward_rect: Rect::ZERO,
            narrative: None,
            narrative_request: None,
            agent_runs: Vec::new(),
            cold_agent_runs: HashMap::new(),
            agent_undo_notice: None,
            open_agent_records: HashSet::new(),
        }
    }

    /// Restore viewer-private read state from the installed host.
    pub fn load_frontiers(&mut self) {
        self.persistent_frontiers = true;
        self.seen_frontiers = self.host.load_frontiers();
        self.departures = self.host.load_departures();
        self.morning_sources = self.host.load_morning_sources();
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

    /// Native tiles can extend beyond the output while strips scroll. Only
    /// fully exposed summaries qualify for acknowledgment.
    pub fn set_review_clip(&mut self, clip: Rect) {
        self.review_clip = Some(clip);
    }

    /// Brief positions change when fresh work arrives. Retain the addressed
    /// work session so keyboard focus cannot drift onto a different result.
    fn replace_digest(&mut self, digest: ActivityDigest) {
        let anchor = |index: usize| {
            self.digest
                .briefs
                .get(index)
                .and_then(|brief| brief.entry_ids.first())
                .copied()
        };
        let focused = match self.keyboard_focus {
            Some(ActivityFocus::Brief(index)) => anchor(index),
            _ => None,
        };
        let expanded = self.expanded_brief.and_then(anchor);
        let page = anchor(self.brief_offset);
        let position = |id| {
            digest
                .briefs
                .iter()
                .position(|brief| brief.entry_ids.contains(&id))
        };
        if let Some(id) = focused {
            self.keyboard_focus =
                Some(position(id).map_or(ActivityFocus::Acknowledge, ActivityFocus::Brief));
        }
        self.expanded_brief = expanded.and_then(position);
        self.brief_offset = page.and_then(position).unwrap_or_default();
        self.digest = digest;
    }

    /// Snapshot the previous private frontier before the catch-up surface is
    /// shown. It advances only when the surface closes, so opening it cannot
    /// erase the very summary the user came to read.
    pub fn begin_view(&mut self, workspace: u64) {
        self.viewed_changes.remove(&workspace);
        if self.scope != RoomScope::ThisRoom {
            self.scope = RoomScope::ThisRoom;
            self.invalidate();
        }
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

    /// Observe the active-workspace edge before refreshing the history index.
    /// Departure retains the last acknowledged frontier. Merely being in a
    /// workspace must not count hidden or unpainted history as reviewed.
    pub fn observe_workspace(&mut self, workspace: u64) {
        if self.presence_workspace == Some(workspace) {
            return;
        }
        if let Some(previous) = self.presence_workspace {
            let frontier = self
                .seen_frontiers
                .get(&previous)
                .cloned()
                .unwrap_or_default();
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
        self.review_pending.is_some()
            || (self.view == ActivityView::Tasks && self.task_review.is_none())
            || self.brief_expansion_started.is_some_and(|(_, started)| {
                now.saturating_duration_since(started) < BRIEF_EXPANSION_DURATION
            })
    }

    pub fn finish_view(&mut self, workspace: u64) {
        self.replay_playing = false;
        let Some(viewed) = self.viewed_changes.remove(&workspace) else {
            return;
        };
        let frontier = self.seen_frontiers.entry(workspace).or_default();
        for id in &viewed {
            frontier.acknowledge(id);
        }
        if self.last_active != Some(workspace) {
            if self.persistent_frontiers {
                self.persist_frontiers();
            }
            return;
        }
        self.catchup_frontier = frontier.clone();
        if let Some(departure) = self.departures.get_mut(&workspace) {
            departure.frontier = frontier.clone();
        }
        // Keep everything not exposed on screen in the catch-up immediately,
        // as well as in the persisted exceptions used by the next refresh.
        let pending: HashSet<_> = self
            .digest
            .briefs
            .iter()
            .flat_map(|brief| brief.entry_ids.iter().copied())
            .collect();
        let was_viewed = |entry: &HistoryEntry| {
            viewed.contains(&self.projected_origins.get(&entry.id).cloned().unwrap_or(
                WorkspaceHistoryId {
                    origin: super::index::HistoryOrigin::DesktopTree,
                    change: entry.id,
                },
            ))
        };
        self.passive_rows.retain(|entry| !was_viewed(entry));
        let rows: Vec<_> = self
            .timeline
            .entries()
            .iter()
            .filter(|entry| pending.contains(&entry.id) && !was_viewed(entry))
            .cloned()
            .collect();
        self.passive_unread = self.passive_rows.len();
        self.replace_digest(ActivityDigest::from_entries_with_ranges(
            &rows,
            SystemTime::UNIX_EPOCH,
            super::index::MAX_FEED_ENTRIES,
            &self.named,
        ));
        self.passive_digest = ActivityDigest::from_entries_with_ranges(
            &self.passive_rows,
            SystemTime::UNIX_EPOCH,
            5,
            &self.named,
        );
        self.reset_narrative();
        if self.persistent_frontiers {
            self.persist_frontiers();
        }
    }

    /// `(headline, unread, pulse)` consumed by passive chrome. `pulse` is a
    /// stable projection of the newest content id, not a frame counter.
    pub fn glance_signal(&self, workspace: u64) -> (Option<String>, usize, u64) {
        if self.last_active != Some(workspace) {
            return (None, 0, 0);
        }
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
        if self.last_active != Some(workspace) {
            return ActivityDigest::default();
        }
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

    /// What "new" is counted from in `workspace`. One this device has never
    /// read starts from the history it holds now: a fresh install's own
    /// setup or a joined workspace's past stays browsable, but is not news.
    fn passive_frontier(&mut self, workspace: u64) -> WorkspaceSeenFrontier {
        if self.persistent_frontiers
            && !self.seen_frontiers.contains_key(&workspace)
            && !self.history_index.entries().is_empty()
        {
            let mut frontier = WorkspaceSeenFrontier::default();
            frontier.mark_seen(&self.history_index);
            self.seen_frontiers.insert(workspace, frontier);
            self.persist_frontiers();
        }
        self.seen_frontiers
            .get(&workspace)
            .cloned()
            .unwrap_or_default()
    }

    /// The tree-only feed's rows the active workspace's frontier has not
    /// acknowledged, with the same first-sight baseline as
    /// [`Self::passive_frontier`].
    fn unseen_tree_rows(&mut self, rows: &[HistoryEntry]) -> Vec<HistoryEntry> {
        let Some(workspace) = self.last_active else {
            return rows.to_vec();
        };
        let id = |entry: &HistoryEntry| WorkspaceHistoryId {
            origin: super::index::HistoryOrigin::DesktopTree,
            change: entry.id,
        };
        if self.persistent_frontiers
            && !self.seen_frontiers.contains_key(&workspace)
            && !rows.is_empty()
        {
            let frontier = self.seen_frontiers.entry(workspace).or_default();
            for row in rows {
                frontier.acknowledge(&id(row));
            }
            self.persist_frontiers();
        }
        let Some(frontier) = self.seen_frontiers.get(&workspace) else {
            return rows.to_vec();
        };
        rows.iter()
            .filter(|row| !frontier.acknowledged(&id(row)))
            .cloned()
            .collect()
    }

    fn persist_frontiers(&self) {
        self.host.save_frontiers(&self.seen_frontiers);
        self.host.save_departures(&self.departures);
    }

    pub fn contributors(&self) -> &[String] {
        &self.contributors
    }

    pub fn view(&self) -> ActivityView {
        self.view
    }

    pub fn set_view(&mut self, view: ActivityView) {
        self.requested_review_task = None;
        if view != ActivityView::Replay {
            self.replay_playing = false;
        }
        self.view = view;
        self.expanded_brief = None;
        if view == ActivityView::Tasks
            && self.selected_task.is_none()
            && let Some(task) = self
                .tasks
                .iter()
                .find(|task| task.status == WorkspaceTaskStatus::Waiting)
        {
            self.selected_task = Some(task.id.clone());
            self.keyboard_focus = Some(ActivityFocus::Task(task.id.clone()));
        }
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

    /// Checkpoints whose causal heads or historical projection were unavailable.
    pub fn replay_unavailable_checkpoints(&self) -> usize {
        self.replay_unavailable
    }

    /// Workspace names at each checkpoint Replay can play, oldest first.
    pub fn replay_checkpoint_names(&self) -> Vec<Vec<String>> {
        self.replay_frames
            .iter()
            .map(|frame| frame.workspaces.iter().map(|ws| ws.name.clone()).collect())
            .collect()
    }

    pub fn replay_is_playing(&self) -> bool {
        self.view == ActivityView::Replay && self.replay_playing
    }

    /// Diff, restore and undo for the selected explorer row — the same
    /// door search, agents and assistive technology use.
    pub fn selection_history_actions(&self) -> Vec<(&'static str, String)> {
        let Some(detail) = self.selection_detail() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if self.selected_morning_source().is_some() {
            out.push((
                MORNING_SOURCE,
                self.detail_action_label(DetailAction::MorningSource),
            ));
        }
        if detail.can_compare {
            out.push((HISTORY_COMPARE, t("activity-history-compare")));
            out.push((HISTORY_RESTORE, t("activity-history-restore")));
        }
        if detail.can_undo && self.selected_agent_run().is_none() {
            out.push((HISTORY_UNDO, t("activity-history-undo")));
        }
        out
    }

    pub fn apply_selection_history_action(
        &mut self,
        id: &str,
        tree: &mut impl ActivityTree,
        records: Option<&RecordStore>,
    ) -> RoomActivityOutcome {
        if let Some(room) = id.strip_prefix(&format!("{MORNING_SOURCE}.")) {
            if room == self.task_room {
                self.toggle_morning_selection();
            }
            return handled_outcome();
        }
        if id == MORNING_SOURCE {
            self.toggle_morning_selection();
            return handled_outcome();
        }
        if let Some((room, key)) = super::morning::decode_action(id, MORNING_OPEN) {
            if room != self.task_room {
                return handled_outcome();
            }
            return RoomActivityOutcome {
                handled: true,
                open_target: self.activate_morning_source(key, false),
                ..Default::default()
            };
        }
        if let Some((room, key)) = super::morning::decode_action(id, MORNING_REMOVE) {
            if room != self.task_room {
                return handled_outcome();
            }
            self.remove_morning_source(key);
            return handled_outcome();
        }
        for command in [
            MorningCommand::Refresh,
            MorningCommand::DraftRefresh,
            MorningCommand::NextSource,
        ] {
            if let Some((room, key)) = super::morning::decode_action(id, command.prefix()) {
                if room == self.task_room {
                    self.submit_morning_command(key, command);
                }
                return handled_outcome();
            }
        }
        if id == ACTIVITY_TASKS {
            self.set_view(ActivityView::Tasks);
            return handled_outcome();
        }
        if id == ACTIVITY_ACKNOWLEDGE {
            self.finish_view(tree.active.raw());
            return handled_outcome();
        }
        let action = match id {
            HISTORY_COMPARE => DetailAction::Compare,
            HISTORY_RESTORE => DetailAction::Restore,
            HISTORY_UNDO => DetailAction::Undo,
            _ => return RoomActivityOutcome::default(),
        };
        self.apply_detail_action(action, tree, records)
    }

    fn selected_resource_target(&self) -> Option<ui::HistoryResourceTarget> {
        self.selection_detail()
            .and_then(|detail| detail.resources.into_iter().next())
    }

    pub fn selection_detail(&self) -> Option<HistorySelectionDetail> {
        let range = self.timeline.selection()?;
        let mut detail = HistorySelectionDetail::from_range(self.timeline.entries(), range)?;
        let mut origin = None;
        let mut app_origins = BTreeSet::new();
        for id in &detail.entry_ids {
            let next = self
                .projected_origins
                .get(id)
                .map_or(super::index::HistoryOrigin::DesktopTree, |address| {
                    address.origin.clone()
                });
            if matches!(next, super::index::HistoryOrigin::AppRecord { .. }) {
                app_origins.insert(next.clone());
            }
            if origin.as_ref().is_some_and(|current| current != &next) {
                detail.can_compare = false;
                detail.can_undo = false;
            }
            origin.get_or_insert(next);
        }
        if !app_origins.is_empty() {
            let titles = std::mem::take(&mut detail.resources);
            detail.resources = app_origins
                .into_iter()
                .filter_map(|origin| {
                    let super::index::HistoryOrigin::AppRecord { app, id, kind } = &origin else {
                        return None;
                    };
                    detail.can_compare &= self.detached_records.iter().any(|record| {
                        record.origin() == origin
                            && record
                                .document
                                .as_ref()
                                .is_some_and(|doc| doc.pending_ops() == 0)
                    });
                    let app_label = self.host.app_display_name(app);
                    // The record's title as its own actions named it, else
                    // its app: the record id is an address, not a name.
                    let label = titles
                        .iter()
                        .find(|target| target.id == id.as_str())
                        .and_then(|target| ui::human_label(&target.label, &target.id))
                        .or(app_label.as_deref())
                        .map_or_else(|| t("overview-untitled"), str::to_owned);
                    Some(ui::HistoryResourceTarget {
                        app_label,
                        app_id: Some(app.clone()),
                        id: id.as_str().into(),
                        kind: kind.as_str().into(),
                        label,
                    })
                })
                .collect();
            detail.can_undo = false;
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
            Some(super::index::HistoryOrigin::Record { id, .. }) => {
                step.record.is_empty() || id.as_str() == step.record
            }
            Some(super::index::HistoryOrigin::AppRecord { .. }) => false,
            Some(super::index::HistoryOrigin::DesktopTree) | None => {
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
                    super::index::HistoryOrigin::Record { id: a, .. },
                    Some(super::index::HistoryOrigin::Record { id: b, .. }),
                ) => a == b,
                (
                    super::index::HistoryOrigin::DesktopTree,
                    Some(super::index::HistoryOrigin::DesktopTree) | None,
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
            || t("activity-agent-fallback"),
            |actor| self.source.authors().resolve(actor).name,
        )
    }

    fn issuer_name(&self, run: &AgentRun) -> String {
        run.issued_by.as_deref().map_or_else(
            || t("activity-unknown-collaborator"),
            |actor| self.source.authors().resolve(actor).name,
        )
    }

    fn can_undo_agent_run(&self) -> bool {
        if !self.host.mutations_available() {
            return false;
        }
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
        tree: &mut impl ActivityTree,
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
            tree.document_mut(),
            ui::history_groups::DisplayedSession {
                record: older.work_session_id.as_deref(),
                start: &older_start,
                end: &older_end,
                label: &older.title,
            },
            ui::history_groups::DisplayedSession {
                record: newer.work_session_id.as_deref(),
                start: &newer_start,
                end: &newer_end,
                label: &newer.title,
            },
            &new_left_end,
            &new_right_start,
        )
        .is_none()
        {
            return false;
        }
        tree.commit_activity_change();
        self.last_heads.clear();
        self.refresh(tree);
        true
    }

    #[doc(hidden)]
    /// Focus the first recap outcome that the settled responsive layout
    /// actually rendered, for deterministic visual journeys. The following
    /// Enter still travels through the production keyboard handler.
    pub fn focus_first_work_session_for_shots(&mut self) -> bool {
        let Some((index, _)) = self.brief_rects.first().copied() else {
            return false;
        };
        self.keyboard_focus = Some(ActivityFocus::Brief(index));
        true
    }

    /// Give deterministic visual journeys a real, elapsed departure frontier
    /// without reading or overwriting this device's persisted history state.
    pub fn prepare_away_recap_for_shots(&mut self, tree: &mut impl ActivityTree) {
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

    pub fn expand_work_session_for_shots(&mut self, index: usize) -> bool {
        if index >= self.digest.briefs.len() {
            return false;
        }
        self.expanded_brief = Some(index);
        true
    }

    #[doc(hidden)]
    pub fn move_work_session_boundary_for_shots(
        &mut self,
        tree: &mut impl ActivityTree,
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

    fn split_selection_group(&mut self, tree: &mut impl ActivityTree) -> bool {
        if !self.host.mutations_available() {
            return false;
        }
        let Some((range, left_end, right_start)) = self.split_candidate() else {
            return false;
        };
        let record_id = range.id.clone();
        let label = range.label.clone();
        let right_label = format!("{label} — continued");
        if ui::history_groups::split_named_range(
            tree.document_mut(),
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
        tree.commit_activity_change();
        self.last_heads.clear();
        self.refresh(tree);
        true
    }

    fn merge_selection_groups(&mut self, tree: &mut impl ActivityTree) -> bool {
        if !self.host.mutations_available() {
            return false;
        }
        let Some((records, start, end, label)) = self.merge_candidate() else {
            return false;
        };
        if ui::history_groups::merge_named_ranges(
            tree.document_mut(),
            &records,
            &start,
            Some(&end),
            &label,
        )
        .is_none()
        {
            return false;
        }
        tree.commit_activity_change();
        self.last_heads.clear();
        self.refresh(tree);
        true
    }

    /// Materialize the exact before/after document for the current selection.
    /// Mixed-document selections deliberately have no synthetic diff: callers
    /// can split them by provenance and compare each resource honestly.
    pub fn compare_selection(
        &self,
        tree: &mut impl ActivityTree,
        records: Option<&RecordStore>,
    ) -> Option<HistoryComparison> {
        if tree.document().pending_ops() > 0 {
            return None;
        }
        let (start, end) = self.timeline.selection()?;
        let selected = self.timeline.entries().get(start..=end)?;
        let addresses: Vec<WorkspaceHistoryId> = selected
            .iter()
            .map(|entry| {
                self.projected_origins
                    .get(&entry.id)
                    .cloned()
                    .unwrap_or(WorkspaceHistoryId {
                        origin: super::index::HistoryOrigin::DesktopTree,
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
            super::index::HistoryOrigin::DesktopTree => {
                ui::compare_entries(tree.document_mut(), &ids).ok()
            }
            super::index::HistoryOrigin::AppRecord { .. } => {
                let mut doc = self
                    .detached_records
                    .iter()
                    .find(|record| record.origin() == first.origin)?
                    .document
                    .clone()?;
                if doc.pending_ops() > 0 {
                    return None;
                }
                ui::compare_entries(&mut doc, &ids).ok()
            }
            super::index::HistoryOrigin::Record { id, .. } => records?
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

    pub fn apply_deep_link(&mut self, focus: ActivityDeepLink) {
        if let Some(task_id) = focus.task_id {
            self.set_view(ActivityView::Tasks);
            self.requested_review_task = Some(task_id.clone());
            self.selected_task = self
                .tasks
                .iter()
                .find(|task| task.id == task_id)
                .map(|task| task.id.clone());
            self.keyboard_focus = self.selected_task.clone().map(ActivityFocus::Task);
            if self.selected_task.is_none() {
                self.review_notice = Some(t("activity-task-review-missing"));
            }
            return;
        }
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
                        super::index::HistoryOrigin::Record { id, .. }
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
            p => Some(p.label()),
        });
    }

    fn defer_pending_tree(&mut self, tree: &impl ActivityTree) {
        self.room_name = tree
            .active_workspace()
            .map(|workspace| workspace.name.clone())
            .unwrap_or_default();
        self.last_heads.clear();
        self.last_active = None;
        self.derived.clear();
        self.scoped_tree_rows.clear();
        self.projected_origins.clear();
        self.external_entry = None;
        self.named.clear();
        self.timeline.set_named_ranges(Vec::new());
        self.replay_frames.clear();
        self.replay_checkpoints.clear();
        self.replay_playing = false;
        self.tasks.clear();
        self.task_source = None;
        self.selected_task = None;
        self.close_task_composer();
        self.clear_comparison();
        self.history_index = WorkspaceHistoryIndex::new();
        self.feed_coverage = FeedCoverage {
            pending_tree: true,
            ..FeedCoverage::default()
        };
        self.install_rows(Vec::new());
    }

    /// Re-derive the timeline from the room document if the graph has
    /// moved; called every frame, the heads check keeps that cheap.
    /// A producer's pending transaction is deferred until its own stamped
    /// commit. History never closes that transaction as a side effect.
    #[allow(
        clippy::too_many_lines,
        reason = "timeline refresh keeps document observation, filter projection, and retained activity rows in one ordered read-only projection pass"
    )]
    pub fn refresh(&mut self, tree: &mut impl ActivityTree) {
        // Commit only what was written: an empty commit on every refresh
        // would still open and roll back a transaction each frame.
        if tree.document().pending_ops() > 0 {
            tree.commit_activity_change();
        }
        if tree.document().pending_ops() > 0 {
            self.defer_pending_tree(tree);
            return;
        }
        self.feed_coverage.pending_tree = false;
        self.replay_presentation_note = tree.replay_presentation_note();

        let name = tree
            .active_workspace()
            .map(|w| w.name.clone())
            .unwrap_or_default();
        if name != self.room_name {
            self.room_name = name;
        }
        let room = tree
            .active_workspace()
            .map(|workspace| workspace.room.to_string())
            .unwrap_or_default();
        if self.task_room != room || self.morning_workspace != tree.active.raw() {
            self.morning_command_message = None;
            self.morning_source_offset = 0;
        }
        self.task_room = room;
        self.morning_workspace = tree.active.raw();
        let origin = self.refresh_task_review();
        let tree_tasks = tree
            .active_workspace()
            .map_or(&[][..], |workspace| workspace.tasks.as_slice());
        self.rebuild_tasks_if_changed(origin, tree_tasks);
        self.observe_confirmations();
        self.refresh_agent_startup();
        if self
            .selected_task
            .as_ref()
            .is_some_and(|id| !self.tasks.iter().any(|task| &task.id == id))
        {
            self.requested_review_task = self.selected_task.take();
            self.review_notice = Some(t("activity-task-review-missing"));
            self.keyboard_focus = Some(ActivityFocus::View(ActivityView::Tasks));
        }

        let heads = tree.document_mut().get_heads();
        let active = tree.active.raw();
        // A switch writes the document but no row: read just the changes
        // since and, when all of them were passed over, only show the rows
        // of the workspace now active.
        let only_passed_over = heads != self.last_heads && !self.last_heads.is_empty() && {
            self.source
                .follow(tree.document_mut(), &mut self.history_log);
            self.history_log.rows_revision() == self.last_rows
        };
        if only_passed_over || (heads == self.last_heads && !self.timeline.entries().is_empty()) {
            self.last_heads = heads;
            if self.last_active != Some(active) {
                self.apply_scope(tree);
            }
            return;
        }
        self.last_heads = heads;
        seed_roster_colours(self.source.authors_mut(), tree);
        // A change an agent's own key signed reads as the agent's, for its
        // person, once the host verified its delegation for that device.
        for agent in tree.roster_agents() {
            self.source.authors_mut().register_roster_agent(&agent);
        }
        let local = self.local_user();
        if !local.is_empty() {
            // The reader's own changes read as theirs.
            self.source.authors_mut().set_reader(local.clone());
        }
        if !local.is_empty() && self.explicit_local_user.is_none() {
            let handle = automerge::ActorId::from(local.as_bytes()).to_hex_string();
            self.host.local_author(self.source.authors_mut(), &handle);
        }

        // Bind the local automerge actor to the Tilo identity, or local user
        // id; a peer's actor stays unresolved until collaboration says who.
        let actor = tree.document_mut().get_actor().to_hex_string();
        if !self.source.authors().is_registered(&actor) {
            let principal = if let Some(local) = &self.explicit_local_user {
                Some(local.clone())
            } else {
                self.host.local_author(self.source.authors_mut(), &actor)
            };
            // The hue follows the person, as receipt rows' does: one person
            // editing from this device and acting through a receipt is one
            // colour, not one per key.
            if let Some(principal) = principal {
                let rgb = self.source.authors().colour_for(&principal);
                self.source.authors_mut().register(
                    &actor,
                    ui::history_source::author_coloured(
                        principal,
                        ui::history_source::short_id(&actor),
                        rgb,
                    ),
                );
            }
        }

        let mut entries = self
            .source
            .entries_since(tree.document_mut(), &mut self.history_log);
        self.last_rows = self.history_log.rows_revision();
        let mut tags = describe_room_changes(
            tree.document_mut(),
            &self.history_log,
            &mut entries,
            &mut self.room_changes,
        );
        // A receipt is committed by the room host, but its authenticated
        // `actor` is the person (or agent) who performed the semantic action.
        // Attribute the projected CRDT row to that actor rather than to the
        // host's Automerge key; otherwise every collaborative work session
        // misleadingly appears to have been done by the same machine.
        let receipt_actors = tree
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.effect_receipts.iter())
            .map(|receipt| (receipt.invocation.0.to_string(), receipt.actor.clone()))
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
            entry.author = ui::history_source::author_coloured(
                participant_display_label(actor),
                ui::history_source::short_id(actor),
                self.source.authors().colour_for(actor),
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
                    author: ui::history_source::author_coloured(
                        participant_display_label(&actor),
                        ui::history_source::short_id(&actor),
                        self.source.authors().colour_for(&actor),
                    ),
                    entry_kind: EntryKind::Details,
                    artifact_kind: ArtifactKind::Cluster,
                    timestamp: SystemTime::UNIX_EPOCH
                        .checked_add(Duration::from_secs(
                            u64::try_from(receipt.completed_at).unwrap_or_default(),
                        ))
                        .unwrap_or(SystemTime::UNIX_EPOCH),
                    summary: receipt_summary(&receipt.context, &receipt.status),
                    text_delta: None,
                    diff_payload: None,
                    redaction: None,
                    context: Some(receipt.context.clone()),
                });
                tags.push(Some(workspace.id.raw()));
            }
        }
        // Housekeeping and what came from outside stay in the room
        // document's history, out of the feed.
        self.derived = entries
            .into_iter()
            .zip(tags)
            .filter(|(entry, _)| {
                entry
                    .context
                    .as_ref()
                    .is_none_or(ui::tilo_actions::ActionContext::is_activity)
            })
            .collect();
        self.derived.sort_by_key(|(entry, _)| entry.timestamp);
        // Names come out of the same document as the changes, so a peer's name
        // arrives with their edits.
        self.named = ui::history_groups::read_named_ranges(tree.document(), self.source.authors());
        self.apply_scope(tree);
    }

    fn agent_is_live(&self, agent: &str) -> bool {
        self.host.agent_is_live(&self.task_room, agent)
    }

    fn agent_startup_state(&self, agent: &str) -> Option<&AgentStartup> {
        self.agent_startup
            .get(&(self.task_room.clone(), agent.to_string()))
    }

    /// Rebuild the sorted task list only when its source moved: the room,
    /// the review snapshot it is read from, or the tree's tasks.
    fn rebuild_tasks_if_changed(&mut self, origin: TaskListOrigin, tree_tasks: &[WorkspaceTask]) {
        let unchanged = self.task_source.as_ref().is_some_and(|source| {
            source.room == self.task_room
                && source.origin == origin
                && source.tree_tasks.as_slice() == tree_tasks
        });
        if unchanged {
            return;
        }
        self.tasks = match origin {
            TaskListOrigin::Tree => tree_tasks.to_vec(),
            TaskListOrigin::Review(_) => match &self.task_review {
                Some(Ok(snapshot)) => snapshot.tasks.clone(),
                _ => Vec::new(),
            },
            TaskListOrigin::Withheld => Vec::new(),
        };
        let needs_person: HashSet<String> = self
            .tasks
            .iter()
            .filter(|task| self.task_needs_person(task))
            .map(|task| task.id.clone())
            .collect();
        self.tasks.sort_by(|left, right| {
            (!needs_person.contains(&left.id))
                .cmp(&!needs_person.contains(&right.id))
                .then_with(|| left.status.is_finished().cmp(&right.status.is_finished()))
                .then_with(|| right.updated_at.cmp(&left.updated_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        self.task_source = Some(TaskListSource {
            room: self.task_room.clone(),
            origin,
            tree_tasks: tree_tasks.to_vec(),
        });
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
                    detail: t("activity-agent-connect-timeout"),
                };
            }
            true
        });
    }

    /// Consume one correlated managed-agent launcher event. A process
    /// starting is not treated as live; only the broker's subscription makes
    /// the task launchable.
    pub fn observe_agent_launch_event(&mut self, event: &AgentLaunchEvent) -> bool {
        let Some(agent) = event.managed_agent_id.as_deref() else {
            return false;
        };
        let Some(workspace) = event.managed_workspace_id.as_deref() else {
            return false;
        };
        let key = (workspace.to_string(), agent.to_string());
        match &event.kind {
            AgentLaunchEventKind::Started { .. } => {
                self.agent_startup
                    .entry(key)
                    .or_insert_with(|| AgentStartup::Starting {
                        since: Instant::now(),
                    });
                true
            }
            AgentLaunchEventKind::SpawnFailed { detail } => {
                self.agent_startup.insert(
                    key,
                    AgentStartup::Failed {
                        detail: detail.clone(),
                    },
                );
                true
            }
            AgentLaunchEventKind::Failed { code, stderr_tail } => {
                let detail = if stderr_tail.trim().is_empty() {
                    code.map_or_else(
                        || t("activity-agent-stopped"),
                        |code| {
                            t_args(
                                "activity-agent-exited",
                                &[("code", code.to_string().into())],
                            )
                        },
                    )
                } else {
                    stderr_tail.clone()
                };
                self.agent_startup
                    .insert(key, AgentStartup::Failed { detail });
                true
            }
            AgentLaunchEventKind::Exited => false,
        }
    }

    /// Hand the timeline the rows [`Self::scope`] selects and re-derive
    /// everything that depends on which rows are in play.
    /// Every entry of the room's derived history, in any workspace.
    pub(super) fn derived_entry_ids(&self) -> impl Iterator<Item = HistoryEntryId> + '_ {
        self.derived.iter().map(|(entry, _)| entry.id)
    }

    fn apply_scope(&mut self, tree: &impl ActivityTree) {
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
                            .unwrap_or_else(|| t("activity-removed-workspace"));
                        e.summary = format!("{label} \u{00b7} {}", e.summary);
                        let mut context = e.context.take().unwrap_or_default();
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

    /// Join the room's shell history with every locally available record in
    /// that room. The projected id map keeps callbacks compact while every
    /// authoritative operation can still resolve the originating document.
    pub fn refresh_records(&mut self, tree: &mut impl ActivityTree, records: &RecordStore) {
        self.refresh_records_inner(tree, records, None);
    }

    /// Production aggregation additionally reads validated, local record
    /// documents which are not mounted in an app's live record store.
    pub fn refresh_records_with_local(
        &mut self,
        tree: &mut impl ActivityTree,
        records: &RecordStore,
        local: &dyn super::index::PersistedRecordHistory,
    ) {
        self.refresh_records_inner(tree, records, Some(local));
    }

    /// Refresh one coherent admission of detached, app-qualified record owners.
    pub fn refresh_detached_records(
        &mut self,
        tree: &mut impl ActivityTree,
        records: Vec<super::index::DetachedRecordHistory>,
    ) {
        if tree.document().pending_ops() > 0 {
            self.defer_pending_tree(tree);
            return;
        }
        self.history_index
            .set_authors(self.source.authors().clone());
        let query = tree
            .active_workspace()
            .map(|w| WorkspaceHistoryQuery::for_room(w.room))
            .unwrap_or_default();
        let changed =
            self.history_index
                .refresh_detached(tree, query, &self.scoped_tree_rows, &records);
        self.detached_records = records;
        self.feed_coverage = self.history_index.coverage().clone();
        if changed {
            self.projected_origins.clear();
            let rows = self
                .history_index
                .entries()
                .iter()
                .map(|row| {
                    let projected = row.timeline_entry();
                    self.projected_origins.insert(projected.id, row.id.clone());
                    projected
                })
                .collect();
            self.install_rows(rows);
            self.timeline
                .set_named_ranges(self.history_index.origin_safe_named_ranges(&self.named));
        }
        let passive = self.passive_frontier(tree.active.raw());
        let local = self.local_user();
        let rows = others_work(
            self.history_index
                .unseen_detached(&passive, tree, &self.detached_records),
            &local,
        );
        self.passive_unread = rows.len();
        self.passive_rows.clone_from(&rows);
        self.passive_digest =
            ActivityDigest::from_entries_with_ranges(&rows, SystemTime::UNIX_EPOCH, 5, &self.named);
        let rows = others_work(
            self.history_index.unseen_detached(
                &self.catchup_frontier,
                tree,
                &self.detached_records,
            ),
            &local,
        );
        self.replace_digest(ActivityDigest::from_entries_with_ranges(
            &rows,
            SystemTime::UNIX_EPOCH,
            super::index::MAX_FEED_ENTRIES,
            &self.named,
        ));
        if changed {
            self.reset_narrative();
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Refreshing records keeps persisted history, live tree state and replay boundaries in one coherent update."
    )]
    fn refresh_records_inner<T: ActivityTree>(
        &mut self,
        tree: &mut T,
        records: &RecordStore,
        local: Option<&dyn super::index::PersistedRecordHistory>,
    ) {
        if tree.document().pending_ops() > 0 {
            self.defer_pending_tree(tree);
            return;
        }
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
                        && let super::index::HistoryOrigin::Record { id, .. } = &row.id.origin
                        && let Some(labels) = record_workspaces.get(id.as_str())
                    {
                        let label = labels.join(" + ");
                        projected.summary = format!("{label} \u{00b7} {}", projected.summary);
                        let mut context = projected.context.take().unwrap_or_default();
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
        let passive_frontier = self.passive_frontier(workspace);
        let unseen = |frontier: &WorkspaceSeenFrontier, tree: &mut T| {
            if let Some(local) = local {
                self.history_index
                    .unseen_with_local_records(frontier, tree, records, local)
            } else {
                self.history_index.unseen(frontier, tree, records)
            }
        };
        let local_user = self.local_user();
        let passive_rows = others_work(unseen(&passive_frontier, tree), &local_user);
        self.passive_unread = passive_rows.len();
        self.passive_rows.clone_from(&passive_rows);
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
                let rows = others_work(unseen(&departure.frontier, tree), &local_user);
                ActivityDigest::from_entries_with_ranges(
                    &rows,
                    SystemTime::UNIX_EPOCH,
                    5,
                    &self.named,
                )
            });
        let catchup_rows = others_work(unseen(&self.catchup_frontier, tree), &local_user);
        self.replace_digest(ActivityDigest::from_entries_with_ranges(
            &catchup_rows,
            SystemTime::UNIX_EPOCH,
            super::index::MAX_FEED_ENTRIES,
            &self.named,
        ));
        self.reset_narrative();
    }

    fn refresh_agent_runs(
        &mut self,
        tree: &impl ActivityTree,
        records: &RecordStore,
        local: Option<&dyn super::index::PersistedRecordHistory>,
    ) {
        self.open_agent_records = records
            .open_records()
            .into_iter()
            .map(|checkout| checkout.id)
            .collect();
        let mut indexed = ui::read_runs(tree.document())
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
        self.cold_agent_runs.retain(|id, _| record_ids.contains(id));
        for id in record_ids {
            let runs = records
                .read(&id, ui::read_runs)
                .or_else(|| local.and_then(|source| self.cold_agent_runs_of(source, &id)))
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

    /// The agent runs persisted record `id` carries, read from `source` only
    /// when its revision moved since the last read.
    fn cold_agent_runs_of(
        &mut self,
        source: &dyn super::index::PersistedRecordHistory,
        id: &RecordId,
    ) -> Option<Vec<AgentRun>> {
        let revision = source.revision(id);
        if let Some(revision) = revision
            && let Some(cached) = self.cold_agent_runs.get(id)
            && cached.revision == revision
        {
            return Some(cached.runs.clone());
        }
        let runs = ui::read_runs(&source.load_record(id)?);
        match revision {
            Some(revision) => {
                self.cold_agent_runs.insert(
                    id.clone(),
                    ColdAgentRuns {
                        revision,
                        runs: runs.clone(),
                    },
                );
            }
            None => {
                self.cold_agent_runs.remove(id);
            }
        }
        Some(runs)
    }

    /// A host bounded the retained desktop document's history projection.
    /// Call after refreshing records, which rebuilds the other coverage counts.
    pub fn mark_desktop_history_capped(&mut self) {
        self.feed_coverage.capped_records = self.feed_coverage.capped_records.saturating_add(1);
    }

    pub fn feed_coverage(&self) -> &FeedCoverage {
        &self.feed_coverage
    }

    fn install_rows(&mut self, mut rows: Vec<HistoryEntry>) {
        rows.retain(is_feed_change);
        let prior_selection = self.selected_ids();
        let local = self.local_user();
        let digest_rows: Vec<HistoryEntry> = rows
            .iter()
            .filter(|entry| !author_is_local(&entry.author, &local))
            .filter(|entry| {
                !self.history_index.entries().is_empty()
                    || !self.catchup_frontier.acknowledged(&WorkspaceHistoryId {
                        origin: super::index::HistoryOrigin::DesktopTree,
                        change: entry.id,
                    })
            })
            .cloned()
            .collect();
        if let Some(entry) = self.external_entry.clone()
            && !rows.iter().any(|candidate| candidate.id == entry.id)
        {
            rows.push(entry);
        }
        self.total_entries = rows.len();
        self.facet_index = HistoryFacetIndex::from_entries(&rows);
        self.replace_digest(ActivityDigest::from_entries_with_ranges(
            &digest_rows,
            SystemTime::UNIX_EPOCH,
            super::index::MAX_FEED_ENTRIES,
            &self.named,
        ));
        if self.history_index.entries().is_empty() {
            let theirs: HashSet<HistoryEntryId> = digest_rows.iter().map(|row| row.id).collect();
            let unseen: Vec<HistoryEntry> = self
                .unseen_tree_rows(&rows)
                .into_iter()
                .filter(|row| theirs.contains(&row.id))
                .collect();
            self.passive_unread = unseen.len();
            self.passive_rows.clone_from(&unseen);
            self.passive_digest = ActivityDigest::from_entries_with_ranges(
                &unseen,
                SystemTime::UNIX_EPOCH,
                5,
                &self.named,
            );
        } else {
            self.passive_unread = digest_rows.len();
            self.passive_rows.clone_from(&digest_rows);
            self.passive_digest = self.digest.clone();
        }
        self.reset_narrative();

        // Filters and counts name the people history resolved; a change it
        // could not name still shows under Everyone.
        let mut seen: HashSet<String> = HashSet::new();
        self.contributors = rows
            .iter()
            .filter(|e| e.author.name != ui::history_source::UNKNOWN_AUTHOR_NAME)
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
        // The deterministic summary only restates the brief titles below
        // it, so the card waits for prose that adds something.
        self.narrative = None;
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

    pub fn set_scope(&mut self, scope: RoomScope, tree: &impl ActivityTree) {
        if tree.document().pending_ops() > 0 {
            self.defer_pending_tree(tree);
            return;
        }
        if scope == RoomScope::AllWorkspaces && !self.host.all_workspaces_available() {
            return;
        }
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
        self.viewport = viewport;
        self.window_layout = false;
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
        self.viewport = viewport;
        self.window_layout = true;
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
            && self.sheet.height() > self.header_h() + VIEW_ROW_H + CHIP_ROW_H + 140.0
    }

    fn chrome_height(&self) -> f64 {
        if self.view == ActivityView::CatchUp {
            return self.header_h();
        }
        if self.shows_chip_rows() {
            self.header_h() + VIEW_ROW_H + CHIP_ROW_H
        } else {
            self.header_h() + VIEW_ROW_H
        }
    }

    /// Single-line height of a chrome type role at the user's text scale.
    /// Geometry is resolved before a frame has a renderer, so the header
    /// takes its minimum from the toolkit's own line metrics; the draw path
    /// then places the lines it actually measured inside that band.
    fn line_height(&self, role: TextRole) -> f64 {
        f64::from(
            ui::tokens::Typography::default()
                .style(role)
                .line_height_px(),
        ) * f64::from(self.text_scale)
    }
    /// The title line (`Heading`).
    fn title_line_h(&self) -> f64 {
        self.line_height(TextRole::Heading)
    }
    /// A subtitle / "Latest" line (`Caption`).
    fn sub_line_h(&self) -> f64 {
        self.line_height(TextRole::Caption)
    }
    /// The header band: title, subtitle and "Latest" lines at the user's
    /// text scale inside `HEADER_TOP`/`HEADER_BOTTOM`, never shorter than the
    /// authored `HEADER_H` — which it equals at 100 %.
    fn header_h(&self) -> f64 {
        HEADER_H.max(HEADER_TOP + self.title_line_h() + 2.0 * self.sub_line_h() + HEADER_BOTTOM)
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
        // The header is sized from the text scale (`header_h`), and `layout`
        // has no theme: when the scale the last layout assumed is not this
        // frame's, lay the sheet out again before anything is placed.
        if (self.text_scale - theme.text_scale).abs() > f32::EPSILON {
            self.text_scale = theme.text_scale;
            if self.viewport.area() > 0.0 {
                if self.window_layout {
                    self.layout_window(self.viewport);
                } else {
                    self.layout(self.viewport);
                }
            }
        }
        // Panel role (HIG §9.1/§9.2/§9.5): the sheet's corner is the Panel
        // radius and its material is the same as every other floating
        // Panel — `panel_ground` with the derived Panel border — rather than
        // a raised fill under a declared border that vanishes on the dark
        // preset.
        let card = RoundedRect::from_rect(
            self.sheet,
            ui::chrome::ChromeRole::Panel.radius(theme, self.sheet.height()),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::shell::panel_ground(theme),
            None,
            &card,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            ui::chrome::shell::panel_border(theme),
            None,
            &card,
        );

        if self.view == ActivityView::CatchUp {
            // Catch Up is the friendly, focused recap. Full-history scope and
            // mode tabs belong to the deeper views and must not leak into its
            // paint, hit-testing, keyboard order, or accessibility snapshot.
            self.scope_rects.clear();
            self.view_rects.clear();
        } else {
            // Scope pills first: the title is elided to the room left of them,
            // so at 200 % the two never paint over each other.
            self.draw_scope_chips(scene, transform, tr, theme);
            self.draw_view_tabs(scene, transform, tr, theme);
        }
        // Title elision must use this frame's scope geometry, not rectangles
        // retained from an earlier viewport, view, or text scale.
        self.draw_title_and_subtitle(scene, transform, tr, theme);

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
            .map(|pane| (pane, pane.label()))
            .collect();
        let selected = self.explorer_pane;
        self.explorer_pane_rects = draw_chip_row(
            scene,
            transform,
            tr,
            theme,
            Rect::new(
                self.sheet.x0 + 12.0,
                y,
                self.sheet.x1 - 12.0,
                y + MIN_TARGET,
            ),
            &items,
            |pane| *pane == selected,
        );
    }

    /// Paint one unambiguous two-pixel focus ring. It is deliberately static:
    /// focus must remain visible with reduced-motion enabled and must not rely
    /// on a pulse or fade to be noticed.
    fn draw_keyboard_focus(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let Some(rect) = self
            .keyboard_focus
            .as_ref()
            .and_then(|focus| self.rect_for_focus(focus))
        else {
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
                &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.control)),
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
        tr.draw_style(
            scene,
            &t("activity-browse-by"),
            theme.tokens.colors.text_primary,
            at(transform, bounds.x0 + 12.0, bounds.y0 + 12.0),
            None,
            &theme.text(TextRole::Label),
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
            self.draw_lens_row(
                &mut PaintCtx::new(scene, transform, tr, theme),
                lens,
                label,
                &mut y,
                bounds,
            );
        }
        if shown < remaining || self.lens_offset > 0 {
            let label = t(if shown < remaining {
                "activity-more-lenses"
            } else {
                "activity-first-lenses"
            });
            self.lens_more_rect = draw_more_row(scene, transform, tr, theme, bounds, y, &label);
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
        let style = theme.text(TextRole::Caption);
        tr.draw_style(
            scene,
            &t("activity-provenance"),
            theme.tokens.colors.text_primary,
            at(transform, bounds.x0 + 12.0, bounds.y0 + 12.0),
            None,
            &style
                .clone()
                .with_weight(ui::text_render::TextWeight::Medium),
        );
        let width = text_width(bounds.width() - 24.0);
        if let Some(detail) = self.selection_detail() {
            if let Some(indexed) = self.selected_agent_run().cloned() {
                self.draw_agent_task_detail(scene, transform, tr, theme, bounds, width, &indexed);
                return;
            }
            let gap = f64::from(theme.tokens.spacing.s2);
            let title_style = theme.text(TextRole::Body);
            let title = tr
                .clamp_to_lines(&detail.title, &title_style, width, 2)
                .join("\n");
            let (_, heading_height) = tr.measure_style(&t("activity-provenance"), None, &style);
            let title_y =
                bounds.y0 + f64::from(theme.tokens.spacing.s3) + f64::from(heading_height) + gap;
            let (_, title_height) = tr.measure_style(&title, Some(width), &title_style);
            tr.draw_style(
                scene,
                &title,
                theme.tokens.colors.text_primary,
                at(transform, bounds.x0 + 12.0, title_y),
                Some(width),
                &title_style,
            );
            let provenance = tr
                .clamp_to_lines(&detail_provenance(&detail), &style, width, 4)
                .join("\n");
            let provenance_y = title_y + f64::from(title_height) + gap;
            let (_, provenance_height) = tr.measure_style(&provenance, Some(width), &style);
            tr.draw_style(
                scene,
                &provenance,
                theme.tokens.colors.text_dim,
                at(transform, bounds.x0 + 12.0, provenance_y),
                Some(width),
                &style,
            );
            let actions_bottom = self.draw_detail_actions(
                scene,
                transform,
                tr,
                theme,
                bounds,
                &detail,
                provenance_y + f64::from(provenance_height) + gap,
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
            tr.draw_style(
                scene,
                &t("activity-select-prompt"),
                theme.tokens.colors.text_primary,
                at(transform, bounds.x0 + 12.0, bounds.y0 + 44.0),
                Some(width),
                &theme.text(TextRole::Body),
            );
            tr.draw_style(
                scene,
                &t("activity-select-hint"),
                theme.tokens.colors.text_dim,
                at(transform, bounds.x0 + 12.0, bounds.y0 + 88.0),
                Some(width),
                &style,
            );
        }
    }

    #[allow(
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
        let style = theme.text(TextRole::Caption);
        let x = bounds.x0 + 12.0;
        let mut y = bounds.y0 + 40.0;
        tr.draw_style(
            scene,
            &t("activity-agent-task"),
            theme.tokens.colors.intent.inverted_hover,
            at(transform, x, y),
            None,
            &theme
                .text(TextRole::Body)
                .with_weight(ui::text_render::TextWeight::Medium),
        );
        y += 26.0;
        let instruction = run.instruction.as_deref().unwrap_or(&run.label);
        tr.draw_style(
            scene,
            instruction,
            theme.tokens.colors.text_primary,
            at(transform, x, y),
            Some(width),
            &theme.text(TextRole::Body),
        );
        y += 52.0;
        tr.draw_style(
            scene,
            &t_args(
                "activity-agent-task-people",
                &[
                    ("issuer", self.issuer_name(run).into()),
                    ("agent", self.agent_name(run).into()),
                ],
            ),
            theme.tokens.colors.text_dim,
            at(transform, x, y),
            Some(width),
            &style,
        );
        y += 38.0;
        tr.draw_style(
            scene,
            &t("activity-why-this-work"),
            theme.tokens.colors.text_primary,
            at(transform, x, y),
            None,
            &style
                .clone()
                .with_weight(ui::text_render::TextWeight::Medium),
        );
        y += 20.0;
        let decision = run
            .decision
            .clone()
            .unwrap_or_else(|| t("activity-no-decision"));
        tr.draw_style(
            scene,
            &decision,
            theme.tokens.colors.text_dim,
            at(transform, x, y),
            Some(width),
            &style,
        );
        y += 48.0;
        tr.draw_style(
            scene,
            &t("activity-affected-records"),
            theme.tokens.colors.text_primary,
            at(transform, x, y),
            None,
            &style
                .clone()
                .with_weight(ui::text_render::TextWeight::Medium),
        );
        y += 22.0;
        for step in &run.steps {
            let app = self
                .host
                .app_display_name(&step.app)
                .unwrap_or_else(|| humanize_app_id(&step.app));
            let record = RecordId::parse(&step.record)
                .and_then(|id| id.file_path())
                .and_then(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| step.record.clone());
            tr.draw_style(
                scene,
                &format!("{app}  ·  {record}"),
                theme.tokens.colors.text_primary,
                at(transform, x, y),
                Some(width),
                &style
                    .clone()
                    .with_weight(ui::text_render::TextWeight::Medium),
            );
            y += 18.0;
            tr.draw_style(
                scene,
                &step.sentence(),
                theme.tokens.colors.text_dim,
                at(transform, x + 10.0, y),
                Some((width - 10.0).max(0.0)),
                &style,
            );
            y += 28.0;
        }
        if !self.can_undo_agent_run() {
            tr.draw_style(
                scene,
                &t("activity-undo-task-needs-open"),
                theme.tokens.colors.text_dim,
                at(transform, x, y),
                Some(width),
                &style,
            );
            y += 38.0;
        }
        if let Some((id, notice)) = &self.agent_undo_notice
            && id == &run.id
        {
            tr.draw_style(
                scene,
                notice,
                theme.tokens.colors.text_dim,
                at(transform, x, y),
                Some(width),
                &style,
            );
            y += 38.0;
        }
        if let Some(detail) = self.selection_detail() {
            let _ = self.draw_detail_actions(scene, transform, tr, theme, bounds, &detail, y);
        }
    }

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
        let spacing = theme.tokens.spacing;
        let inset = f64::from(spacing.s3);
        let gap = f64::from(spacing.s2);
        let mut row_height = 0.0_f64;
        let mut x = bounds.x0 + inset;
        let mut y = start_y;
        let compared = self.comparison_attempted;
        for action in [
            DetailAction::MorningSource,
            DetailAction::Open,
            DetailAction::Compare,
            DetailAction::Restore,
            DetailAction::Undo,
            DetailAction::UndoTask,
            DetailAction::Split,
            DetailAction::Merge,
        ] {
            if !self.host.mutations_available()
                && !matches!(
                    action,
                    DetailAction::MorningSource | DetailAction::Open | DetailAction::Compare
                )
            {
                continue;
            }
            let enabled = match action {
                DetailAction::MorningSource => self.selected_morning_source().is_some(),
                DetailAction::Open => !detail.resources.is_empty(),
                DetailAction::Compare | DetailAction::Restore => detail.can_compare,
                DetailAction::Undo => detail.can_undo && self.selected_agent_run().is_none(),
                DetailAction::UndoTask => self.can_undo_agent_run(),
                DetailAction::Split => self.split_candidate().is_some(),
                DetailAction::Merge => self.merge_candidate().is_some(),
            };
            if !enabled {
                continue;
            }
            let label = self.detail_action_label(action);
            let (label_width, label_height) =
                tr.measure_style(&label, None, &theme.text(TextRole::Caption));
            let button_width = (f64::from(label_width) + 2.0 * gap).max(
                ui::widget::ButtonFace::height(ui::widget::ControlSize::Regular, theme),
            );
            let button_height = (f64::from(label_height) + 2.0 * gap).max(
                ui::widget::ButtonFace::height(ui::widget::ControlSize::Regular, theme),
            );
            if x + button_width > bounds.x1 - inset {
                x = bounds.x0 + inset;
                y += row_height + gap;
                row_height = 0.0;
            }
            row_height = row_height.max(button_height);
            let rect = Rect::new(x, y, x + button_width, y + button_height);
            let is_active = action == DetailAction::Compare && compared;
            // Control role (HIG §9.1/§9.3): a chip at the Control radius. The
            // one that is "on" (Compare, while a comparison shows) is a lift
            // with the tier's hairline; idle chips rest on the lift floor with
            // an edge derived from that floor — never an accent wash or a
            // declared border.
            let shape = RoundedRect::from_rect(
                rect,
                ui::chrome::ChromeRole::Control.radius(theme, rect.height()),
            );
            paint_control_chip(scene, transform, theme, &shape, is_active);
            tr.draw_style(
                scene,
                &label,
                theme.tokens.colors.text_primary,
                at(transform, rect.x0 + gap, rect.y0 + gap),
                None,
                &theme.text(TextRole::Caption),
            );
            self.detail_action_rects.push((action, rect));
            x += button_width + gap;
        }
        y + row_height
    }

    #[allow(
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
            theme.tokens.colors.text_primary.with_alpha_factor(0.12),
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.text_primary.with_alpha_factor(0.55),
            None,
            &shape,
        );
        tr.draw_style(
            scene,
            &t("activity-what-changed"),
            theme.tokens.colors.text_primary,
            at(transform, card.x0 + 12.0, card.y0 + 10.0),
            None,
            &theme.text(TextRole::Label),
        );
        tr.draw_style(
            scene,
            &t_args(
                "activity-comparison-counts",
                &[
                    ("actions", comparison.entries.len().into()),
                    ("changes", comparison.patches.len().into()),
                ],
            ),
            theme.tokens.colors.text_dim,
            at(transform, card.x0 + 12.0, card.y0 + 32.0),
            Some(width - 24.0),
            &theme.text(TextRole::Caption),
        );
        if labels.is_empty() {
            tr.draw_style(
                scene,
                &t("activity-no-differences"),
                theme.tokens.colors.text_dim,
                at(transform, card.x0 + 12.0, card.y0 + 52.0),
                Some(width - 24.0),
                &theme.text(TextRole::Caption),
            );
            return;
        }
        for (index, label) in labels.iter().take(visible).enumerate() {
            tr.draw_style(
                scene,
                &format!("• {label}"),
                theme.tokens.colors.text_primary,
                at(
                    transform,
                    card.x0 + 12.0,
                    card.y0 + 54.0 + index as f64 * 20.0,
                ),
                Some(width - 24.0),
                &theme.text(TextRole::Caption),
            );
        }
        if labels.len() > visible {
            tr.draw_style(
                scene,
                &counted(
                    "activity-comparison-more",
                    labels.len().saturating_sub(visible),
                ),
                theme.tokens.colors.text_dim,
                at(
                    transform,
                    card.x0 + 22.0,
                    card.y0 + 54.0 + visible as f64 * 20.0,
                ),
                Some(width - 34.0),
                &theme.text(TextRole::Caption),
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
        ui::surface::card(
            scene,
            transform,
            card,
            f64::from(theme.tokens.radii.control),
            theme.tokens.colors.lift(0.35),
        );
        tr.draw_style(
            scene,
            &t("activity-preview-unavailable"),
            theme.tokens.colors.text_primary,
            at(transform, card.x0 + 12.0, card.y0 + 10.0),
            None,
            &theme.text(TextRole::Label),
        );
        tr.draw_style(
            scene,
            &t("activity-preview-unavailable-detail"),
            theme.tokens.colors.text_dim,
            at(transform, card.x0 + 12.0, card.y0 + 36.0),
            Some(text_width(card.width() - 24.0)),
            &theme.text(TextRole::Caption),
        );
    }

    fn draw_lens_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        lens: &LensSelection,
        label: &str,
        y: &mut f64,
        bounds: Rect,
    ) {
        let (scene, transform, tr, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        if *y + 28.0 > bounds.y1 - 6.0 {
            return;
        }
        let rect = Rect::new(bounds.x0 + 6.0, *y, bounds.x1 - 6.0, *y + 28.0);
        if self.lens == *lens {
            // Selection is a lift (HIG §9.3), never an accent fill: the
            // accent is reserved for live state (§9.4).
            let radius = ui::chrome::ChromeRole::Control.radius(theme, rect.height());
            let shape = RoundedRect::from_rect(rect, radius);
            let tier = ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(true));
            scene.fill(Fill::NonZero, transform, tier.fill, None, &shape);
            if let Some(border) = tier.border {
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    border,
                    None,
                    &ui::surface::edge_outline(rect, radius),
                );
            }
        }
        tr.draw_style(
            scene,
            label,
            if self.lens == *lens {
                theme.tokens.colors.text_primary
            } else {
                theme.tokens.colors.text_dim
            },
            at(transform, rect.x0 + 8.0, rect.y0 + 6.0),
            Some(text_width(rect.width() - 16.0)),
            &theme.text(TextRole::Caption),
        );
        self.lens_rects.push((lens.clone(), rect));
        *y += 32.0;
    }

    fn lens_candidates(&self) -> Vec<(LensSelection, String)> {
        let mut rows = vec![(LensSelection::All, t("activity-lens-everything"))];
        if !self.agent_runs.is_empty() {
            rows.push((
                LensSelection::AgentTasks,
                t_args(
                    "activity-lens-agent-tasks",
                    &[("count", self.agent_runs.len().into())],
                ),
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
                t_args(
                    "activity-lens-unclassified",
                    &[("count", self.facet_index.unclassified.into())],
                ),
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
        // One pointer target tall, centred in the 34 px view band.
        let y = self.sheet.y0 + self.header_h() + (VIEW_ROW_H - MIN_TARGET) / 2.0;
        let items: Vec<(ActivityView, String)> = ActivityView::ALL
            .iter()
            .map(|view| (*view, view.label()))
            .collect();
        let selected = self.view;
        self.view_rects = draw_chip_row(
            scene,
            transform,
            tr,
            theme,
            Rect::new(
                self.sheet.x0 + 12.0,
                y,
                self.sheet.x1 - 12.0,
                y + MIN_TARGET,
            ),
            &items,
            |view| *view == selected,
        );
    }

    fn catch_up_task_summary(&self) -> String {
        let done = self
            .tasks
            .iter()
            .filter(|task| task.status == WorkspaceTaskStatus::Done)
            .count();
        let unfinished = self
            .tasks
            .iter()
            .filter(|task| !task.status.is_finished())
            .count();
        let updated = self
            .tasks
            .iter()
            .map(|task| task.updated_at)
            .max()
            .unwrap_or_default();
        let updated = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(
                u64::try_from(updated).unwrap_or_default(),
            ))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        t_args(
            "activity-catch-up-goals",
            &[
                ("done", done.into()),
                ("unfinished", unfinished.into()),
                ("when", format_relative_time(updated, self.now()).into()),
            ],
        )
    }

    /// Task state is a live room projection, separate from read history. Seeing
    /// a summary never completes, dismisses or changes a waiting task.
    fn draw_catch_up_tasks(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        mut y: f64,
    ) -> f64 {
        if self.tasks.is_empty() {
            return y;
        }
        let waiting = self
            .tasks
            .iter()
            .filter(|task| self.task_needs_person(task))
            .count();
        let x0 = self.sheet.x0 + 12.0;
        let x1 = self.sheet.x1 - 12.0;
        let summary = self.catch_up_task_summary();
        let style = theme.text(TextRole::Caption);
        let width = text_width(x1 - x0 - 24.0);
        let (_, height) = tr.measure_style(&summary, Some(width), &style);
        tr.draw_style(
            scene,
            &summary,
            theme.tokens.colors.text_dim,
            at(transform, x0 + 12.0, y),
            Some(width),
            &style,
        );
        y += f64::from(height) + 8.0;
        let label = if waiting == 0 {
            t("activity-catch-up-review-goals")
        } else {
            t_args(
                "activity-catch-up-needs-review",
                &[("count", waiting.into())],
            )
        };
        let (_, height) = tr.measure_style(&label, Some(width), &theme.text(TextRole::Body));
        let control_h = ui::widget::ButtonFace::height(ui::widget::ControlSize::Regular, theme);
        let rect = Rect::new(x0, y, x1, y + (f64::from(height) + 16.0).max(control_h));
        if rect.y1 > self.sheet.y1 - 10.0 {
            return y;
        }
        draw_task_button(scene, transform, tr, theme, rect, &label, true);
        self.view_rects.push((ActivityView::Tasks, rect));
        let mut next = rect.y1 + 8.0;
        if let Some(task) = self.tasks.iter().find(|task| self.task_needs_person(task)) {
            let detail = t_args(
                "activity-catch-up-waiting-detail",
                &[
                    ("task", task.title.as_str().into()),
                    (
                        "who",
                        task_assignee_label(task, &self.task_creator_label(task)).into(),
                    ),
                ],
            );
            let style = theme.text(TextRole::Caption);
            for line in tr.clamp_to_lines(&detail, &style, width, 2) {
                let (_, height) = tr.measure_style(&line, Some(width), &style);
                if next + f64::from(height) > self.sheet.y1 - 10.0 {
                    break;
                }
                tr.draw_style(
                    scene,
                    &line,
                    theme.tokens.colors.text_primary,
                    at(transform, x0 + 12.0, next),
                    Some(width),
                    &style,
                );
                next += f64::from(height);
            }
        }
        next + 8.0
    }

    #[allow(
        clippy::too_many_lines,
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
        self.acknowledge_rect = None;
        self.boundary_rects.clear();
        self.brief_more_rect = None;
        self.brief_more_advance = 0;
        self.morning_rects.clear();
        self.morning_command_rects.clear();
        self.resume_rect = Rect::ZERO;
        self.resume_target = None;
        let mut y = self.sheet.y0 + self.chrome_height() + 8.0;
        let x0 = self.sheet.x0 + 12.0;
        let x1 = self.sheet.x1 - 12.0;
        let label = t("activity-acknowledge-viewed");
        let width = text_width(x1 - x0 - 24.0);
        let (_, height) = tr.measure_style(&label, Some(width), &theme.text(TextRole::Caption));
        let control_h = ui::widget::ButtonFace::height(ui::widget::ControlSize::Regular, theme);
        let acknowledge = Rect::new(x0, y, x1, y + (f64::from(height) + 16.0).max(control_h));
        if acknowledge.y1 < self.sheet.y1 - 10.0 {
            draw_task_button(scene, transform, tr, theme, acknowledge, &label, false);
            self.acknowledge_rect = Some(acknowledge);
            y = acknowledge.y1 + 8.0;
        }
        y = self.draw_catch_up_tasks(scene, transform, tr, theme, y);
        y = self.draw_morning_sources(scene, transform, tr, theme, y);
        if let Some(resume) = local_resume_snapshot(self.timeline.entries(), &self.local_user()) {
            let (_, caption_h) = tr.measure_style("Mg", None, &theme.text(TextRole::Caption));
            let (_, title_h) = tr.measure_style("Mg", None, &theme.text(TextRole::Label));
            let height =
                (f64::from(caption_h) * 2.0 + f64::from(title_h) + 24.0).max(control_h + 16.0);
            let rect = Rect::new(x0, y, x1, y + height);
            draw_resume_card(scene, transform, tr, theme, rect, &resume, self.now());
            self.resume_rect = rect;
            self.resume_target = Some(resume.target);
            y = rect.y1 + 8.0;
        }
        if self.digest.briefs.is_empty() {
            tr.draw_style(
                scene,
                &t(if self.feed_coverage.pending_tree {
                    "activity-history-pending"
                } else {
                    "activity-caught-up"
                }),
                theme.tokens.colors.text_dim,
                at(transform, x0 + 4.0, y + 12.0),
                Some(text_width(x1 - x0 - 8.0)),
                &theme.text(TextRole::Body),
            );
            return;
        }
        let metrics = BriefCardMetrics::new(tr, theme);
        if let Some(narrative) = self.narrative.as_ref() {
            // Section-header role for the label (HIG §9.6: `xs`) — the former
            // `sm - 1` was an invented size — over up to two body lines, each
            // seated on its measured line so 200 % type grows the card.
            let label_style = theme.text(TextRole::Caption);
            let body_style = theme.text(TextRole::Caption);
            let label = t("activity-summary-on-device");
            let inner_w = text_width(x1 - x0 - 24.0);
            let (_, label_h) = tr.measure_style(&label, Some(inner_w), &label_style);
            let lines = tr.clamp_to_lines(&narrative.text, &body_style, inner_w, 2);
            let body_top = metrics.pad + f64::from(label_h) + metrics.gap;
            let card_h = body_top + 2.0 * metrics.line_lh + metrics.pad;
            let card = Rect::new(x0, y, x1, y + card_h);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.text_primary.with_alpha_factor(0.13),
                None,
                &RoundedRect::from_rect(card, f64::from(theme.tokens.radii.control)),
            );
            tr.draw_style(
                scene,
                &label,
                theme.tokens.colors.text_dim,
                at(transform, card.x0 + 12.0, card.y0 + metrics.pad),
                Some(inner_w),
                &label_style,
            );
            for (line, text) in lines.iter().enumerate() {
                tr.draw_style(
                    scene,
                    text,
                    theme.tokens.colors.text_primary,
                    at(
                        transform,
                        card.x0 + 12.0,
                        card.y0 + body_top + small_count_to_f64(line) * metrics.line_lh,
                    ),
                    Some(inner_w),
                    &body_style,
                );
            }
            y += card_h + 8.0;
        }
        if self.brief_offset >= self.digest.briefs.len() {
            self.brief_offset = 0;
        }
        let mut shown = 0usize;
        let mut previous_index: Option<usize> = None;
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
            #[expect(
                clippy::indexing_slicing,
                reason = "previous_index is an earlier enumerate() index of the same briefs being iterated"
            )]
            if let Some(newer) = previous_index
                && (expanded || self.expanded_brief == Some(newer))
            {
                let rail = Rect::new(x0 + 8.0, y - 5.0, x1 - 8.0, y + 21.0);
                draw_work_session_boundary(
                    scene,
                    transform,
                    tr,
                    theme,
                    rail,
                    brief.changes > 1,
                    self.digest.briefs[newer].changes > 1,
                );
                let (into_newer, into_older) = work_session_boundary_button_rects(rail);
                if self.digest.briefs[newer].changes > 1 {
                    self.boundary_rects.push((
                        WorkSessionBoundary {
                            older: index,
                            newer,
                            direction: BoundaryDirection::IntoOlder,
                        },
                        into_older,
                    ));
                }
                if brief.changes > 1 {
                    self.boundary_rects.push((
                        WorkSessionBoundary {
                            older: index,
                            newer,
                            direction: BoundaryDirection::IntoNewer,
                        },
                        into_newer,
                    ));
                }
                y += 26.0;
            }
            // Outcome, who/when, and affected context must all be scannable
            // without opening the card.
            let title_lines = tr.clamp_to_lines(
                &brief.title,
                &theme.text(TextRole::Body),
                brief_text_width(x1 - x0),
                2,
            );
            let collapsed_height = metrics.height(title_lines.len(), 0);
            let expanded_height = metrics.height(title_lines.len(), brief.actions.len());
            let height = collapsed_height + (expanded_height - collapsed_height) * expansion;
            let more_room = 34.0;
            let has_later = index
                .checked_add(1)
                .is_some_and(|next| next < self.digest.briefs.len())
                || self.brief_offset > 0;
            let available_height =
                (self.sheet.y1 - 10.0 - y - if has_later { more_room } else { 0.0 })
                    .max(collapsed_height);
            let height = height.min(available_height);
            if y + height + if has_later { more_room } else { 0.0 } > self.sheet.y1 - 10.0 {
                break;
            }
            let rect = Rect::new(x0, y, x1, y + height);
            self.brief_rects.push((index, rect));
            if self.scope == RoomScope::ThisRoom
                && self.review_clip.unwrap_or(self.viewport).intersect(rect) == rect
                && let Some(workspace) = self.last_active
            {
                let viewed = self.viewed_changes.entry(workspace).or_default();
                for id in &brief.entry_ids {
                    if self
                        .external_entry
                        .as_ref()
                        .is_some_and(|entry| entry.id == *id)
                    {
                        continue;
                    }
                    viewed.insert(self.projected_origins.get(id).cloned().unwrap_or(
                        WorkspaceHistoryId {
                            origin: super::index::HistoryOrigin::DesktopTree,
                            change: *id,
                        },
                    ));
                }
            }
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &rect,
            );
            draw_brief_card(
                &mut PaintCtx::new(scene, transform, tr, theme),
                brief,
                rect,
                metrics,
                expanded,
                self.now(),
            );
            scene.pop_layer();
            y += height + 8.0;
            shown = shown.saturating_add(1);
            previous_index = Some(index);
        }
        let remaining = self
            .digest
            .briefs
            .len()
            .saturating_sub(self.brief_offset.saturating_add(shown));
        if remaining > 0 || self.brief_offset > 0 {
            let label = t(if remaining > 0 {
                "activity-more-sessions"
            } else {
                "activity-first-sessions"
            });
            self.brief_more_rect = draw_more_row(
                scene,
                transform,
                tr,
                theme,
                Rect::new(x0, y, x1, self.sheet.y1 - 8.0),
                y,
                &label,
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
            tr.draw_style(
                scene,
                &t(if self.feed_coverage.pending_tree {
                    "activity-replay-pending"
                } else if self.replay_unavailable > 0 {
                    "activity-replay-unavailable"
                } else {
                    "activity-replay-empty"
                }),
                theme.tokens.colors.text_primary,
                at(
                    transform,
                    self.sheet.x0 + 16.0,
                    self.sheet.y0 + self.chrome_height() + 24.0,
                ),
                Some(text_width(self.sheet.width() - 32.0)),
                &theme.text(TextRole::Body),
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
        // Control role: transport chips at the Control radius. Playing is
        // live state (HIG §9.4), so the accent stays on the active Pause
        // chip; idle chips derive their edge from the lift floor (§9.5).
        let floor = theme.tokens.colors.lift(PILL_FLOOR_LIFT_T);
        let idle_edge = ui::surface::edge_on(floor);
        let play_label = t(if self.replay_playing {
            "activity-replay-pause"
        } else {
            "activity-replay-play"
        });
        for (rect, label, active) in [
            (self.replay_back_rect, "←", false),
            (
                self.replay_play_rect,
                play_label.as_str(),
                self.replay_playing,
            ),
            (self.replay_forward_rect, "→", false),
        ] {
            let radius = ui::chrome::ChromeRole::Control.radius(theme, rect.height());
            let shape = RoundedRect::from_rect(rect, radius);
            scene.fill(
                Fill::NonZero,
                transform,
                if active {
                    theme.tokens.colors.text_primary.with_alpha_factor(0.28)
                } else {
                    floor
                },
                None,
                &shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                if active {
                    theme.tokens.colors.intent.inverted_hover
                } else {
                    idle_edge
                },
                None,
                &ui::surface::edge_outline(rect, radius),
            );
            let (tw, th) = tr.measure_style(label, None, &theme.text(TextRole::Caption));
            tr.draw_style(
                scene,
                label,
                theme.tokens.colors.text_primary,
                at(
                    transform,
                    rect.center().x - f64::from(tw) / 2.0,
                    rect.center().y - f64::from(th) / 2.0,
                ),
                None,
                &theme.text(TextRole::Caption),
            );
        }
        tr.draw_style(
            scene,
            &t_args(
                "activity-replay-position",
                &[
                    ("position", self.replay_index.saturating_add(1).into()),
                    ("total", self.replay_frames.len().into()),
                    (
                        "when",
                        format_relative_time(frame.timestamp, self.now()).into(),
                    ),
                ],
            ),
            theme.tokens.colors.text_dim,
            at(
                transform,
                self.replay_forward_rect.x1 + 12.0,
                controls.y0 + 9.0,
            ),
            Some(text_width(controls.x1 - self.replay_forward_rect.x1 - 12.0)),
            &theme.text(TextRole::Caption),
        );

        let info_h = 94.0;
        let canvas = Rect::new(
            self.sheet.x0 + 12.0,
            controls.y1 + 10.0,
            self.sheet.x1 - 12.0,
            (self.sheet.y1 - info_h - 20.0).max(controls.y1 + 80.0),
        );
        // The replay canvas is a card on the sheet: `radii.control` (10 was off
        // the radius scale) with an edge derived from its base fill.
        ui::surface::card(
            scene,
            transform,
            canvas,
            f64::from(theme.tokens.radii.control),
            theme.tokens.colors.surfaces.base,
        );
        draw_replay_layout(scene, transform, tr, theme, canvas, frame);
        draw_replay_pointers(scene, transform, tr, theme, canvas, frame, self.sheet);

        let info = Rect::new(canvas.x0, canvas.y1 + 8.0, canvas.x1, self.sheet.y1 - 10.0);
        let (r, g, b) = frame.author.rgb();
        scene.fill(
            Fill::NonZero,
            transform,
            vello::peniko::Color::from_rgba8(r, g, b, 255),
            None,
            &vello::kurbo::Circle::new(Point::new(info.x0 + 9.0, info.y0 + 11.0), 5.0),
        );
        tr.draw_style(
            scene,
            &format!("{} · {}", frame.author.name, frame.summary),
            theme.tokens.colors.text_primary,
            at(transform, info.x0 + 20.0, info.y0 + 4.0),
            Some(text_width(info.width() - 24.0)),
            &theme.text(TextRole::Label),
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
        self.new_task_rect = None;
        if self.host.mutations_available()
            && !self.feed_coverage.pending_tree
            && !self.host.task_review_required(&self.task_room)
        {
            self.new_task_rect = Some(new_task);
            draw_task_button(
                scene,
                transform,
                tr,
                theme,
                new_task,
                &t("activity-task-new"),
                true,
            );
        }
        tr.draw_style(
            scene,
            &t("activity-task-queue"),
            theme.tokens.colors.text_dim,
            at(transform, x0 + 4.0, y + 8.0),
            None,
            &theme.text(TextRole::Label),
        );
        y += 40.0;

        if self.direction.is_some() {
            self.draw_direction(
                &mut PaintCtx::new(scene, transform, tr, theme),
                Rect::new(x0, y, x1, self.sheet.y1),
            );
            return;
        }

        if matches!(&self.task_review,Some(Ok(snapshot)) if snapshot.room==self.task_room) {
            self.draw_review_tasks(
                &mut PaintCtx::new(scene, transform, tr, theme),
                Rect::new(x0, y, x1, self.sheet.y1),
            );
            return;
        }

        if self.task_compose_open {
            let composer = Rect::new(x0, y, x1, y + 164.0);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.lift(0.45),
                None,
                &RoundedRect::from_rect(composer, f64::from(theme.tokens.radii.control)),
            );
            tr.draw_style(
                scene,
                &t("activity-task-composer-title"),
                theme.tokens.colors.text_primary,
                at(transform, composer.x0 + 12.0, composer.y0 + 10.0),
                None,
                &theme
                    .text(TextRole::Body)
                    .with_weight(ui::text_render::TextWeight::Medium),
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
            let placeholder = t("activity-task-placeholder");
            tr.draw_style(
                scene,
                if self.task_draft.is_empty() {
                    &placeholder
                } else {
                    &self.task_draft
                },
                if self.task_draft.is_empty() {
                    theme.tokens.colors.text_dim
                } else {
                    theme.tokens.colors.text_primary
                },
                at(transform, field.x0 + 10.0, field.y0 + 7.0),
                Some(text_width(field.width() - 20.0)),
                &theme.text(TextRole::Body),
            );
            let routes = [
                WorkspaceTaskRoute::Manual,
                WorkspaceTaskRoute::RulesOnly,
                WorkspaceTaskRoute::Model,
            ]
            .into_iter()
            .map(|route| (route, task_route_label(route)))
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
                WorkspaceTaskRoute::Manual => t("activity-task-executor-manual"),
                WorkspaceTaskRoute::RulesOnly | WorkspaceTaskRoute::Model => {
                    self.task_agent.as_ref().map_or_else(
                        || t("activity-task-executor-none"),
                        |agent| t_args("activity-task-executor-agent", &[("agent", agent.into())]),
                    )
                }
            };
            let executor = tr.elide_to_width_style(
                &executor,
                text_width(composer.width() - 24.0),
                &theme.text(TextRole::Caption),
            );
            tr.draw_style(
                scene,
                &executor,
                theme.tokens.colors.text_primary,
                at(transform, composer.x0 + 12.0, composer.y0 + 107.0),
                None,
                &theme.text(TextRole::Caption),
            );
            let attached = self.task_attachment.as_ref().map_or_else(
                || t("activity-task-no-attachment"),
                |resource| {
                    t_args(
                        "activity-task-attached",
                        &[
                            (
                                "app",
                                resource
                                    .app_label
                                    .clone()
                                    .unwrap_or_else(|| t("activity-task-attached-workspace"))
                                    .into(),
                            ),
                            ("resource", (&resource.label).into()),
                        ],
                    )
                },
            );
            let attached = tr.elide_to_width_style(
                &attached,
                text_width(composer.width() - 24.0),
                &theme.text(TextRole::Caption),
            );
            tr.draw_style(
                scene,
                &attached,
                theme.tokens.colors.text_dim,
                at(transform, composer.x0 + 12.0, composer.y0 + 126.0),
                None,
                &theme.text(TextRole::Caption),
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
            draw_task_button(
                scene,
                transform,
                tr,
                theme,
                cancel,
                &t("activity-task-cancel-compose"),
                false,
            );
            draw_task_button(
                scene,
                transform,
                tr,
                theme,
                create,
                &t("activity-task-create"),
                !self.task_draft.trim().is_empty(),
            );
            y = composer.y1 + 8.0;
        }

        if self.tasks.is_empty() {
            tr.draw_style(
                scene,
                &t(
                    if self.host.task_review_required(&self.task_room)
                        && !matches!(&self.task_review, Some(Ok(_)))
                    {
                        "activity-task-review-unavailable"
                    } else if self.feed_coverage.pending_tree {
                        "activity-tasks-pending"
                    } else {
                        "activity-tasks-empty"
                    },
                ),
                theme.tokens.colors.text_primary,
                at(transform, x0 + 4.0, y + 16.0),
                None,
                &theme
                    .text(TextRole::Body)
                    .with_weight(ui::text_render::TextWeight::Medium),
            );
            tr.draw_style(
                scene,
                &t("activity-tasks-empty-hint"),
                theme.tokens.colors.text_dim,
                at(transform, x0 + 4.0, y + 40.0),
                Some(text_width(x1 - x0 - 8.0)),
                &theme.text(TextRole::Caption),
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
                &mut PaintCtx::new(scene, transform, tr, theme),
                task,
                rect,
                selected,
                self.now(),
                execution_notice.as_deref(),
                &self.task_creator_label(task),
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
        let title_style = tokens.typography.style(TextRole::Heading);
        let title = t(match self.view {
            ActivityView::CatchUp => "activity-title-catch-up",
            ActivityView::Tasks => "activity-title-tasks",
            ActivityView::Explore => "activity-title-explore",
            ActivityView::Replay => "activity-title-replay",
        });
        // The title yields to the scope pills on its right (painted first):
        // elided to the room left of them, so at 200 % the two never
        // overprint. Its measured line, not the authored 20, seats the
        // subtitle underneath.
        let title_x = self.sheet.x0 + 16.0;
        let pills_x0 = self
            .scope_rects
            .iter()
            .map(|(_, rect)| rect.x0)
            .fold(self.sheet.x1 - 16.0, f64::min);
        let title_room = (pills_x0 - f64::from(tokens.spacing.s3) - title_x).max(1.0);
        let title = tr.elide_to_width_style(&title, text_width(title_room), &title_style);
        #[cfg(test)]
        self.painted_title.replace(title.clone());
        let (_, title_h) = tr.measure_style(&title, None, &title_style);
        let title_y = self.sheet.y0 + HEADER_TOP;
        tr.draw_style(
            scene,
            &title,
            colors.text_primary,
            at(transform, title_x, title_y),
            None,
            &title_style,
        );
        // Control sub-line scale (HIG §9.6). Shrinking `sm` by a pixel to
        // buy room was hiding a fit problem rather than solving it.
        let sub_style = tokens.typography.style(TextRole::Caption);
        let subtitle_y = title_y + f64::from(title_h).max(self.title_line_h());
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
        let workspace = if self.room_name.is_empty() {
            t("activity-workspace-fallback")
        } else {
            self.room_name.clone()
        };
        if self.view == ActivityView::Tasks {
            let subtitle = t_args(
                "activity-tasks-subtitle",
                &[
                    ("workspace", workspace.as_str().into()),
                    ("active", change_count.into()),
                    ("agents", contributor_count.into()),
                ],
            );
            // Both lines are elided to one line each: the header band holds
            // exactly one subtitle line and one line under it, so a wrapped
            // subtitle would run into the tagline. The tagline follows the
            // subtitle's measured height.
            let wrap_width = text_width(self.sheet.width() - 32.0);
            let subtitle = tr.elide_to_width_style(&subtitle, wrap_width, &sub_style);
            let (_, subtitle_h) = tr.measure_style(&subtitle, None, &sub_style);
            tr.draw_style(
                scene,
                &subtitle,
                colors.text_dim,
                at(transform, self.sheet.x0 + 16.0, subtitle_y),
                None,
                &sub_style,
            );
            let tagline =
                tr.elide_to_width_style(&t("activity-tasks-tagline"), wrap_width, &sub_style);
            tr.draw_style(
                scene,
                &tagline,
                colors.text_primary,
                at(
                    transform,
                    self.sheet.x0 + 16.0,
                    subtitle_y + f64::from(subtitle_h).max(self.sub_line_h()),
                ),
                None,
                &sub_style,
            );
            return;
        }
        if self.view == ActivityView::CatchUp {
            // Catch Up is the friendly recap: who kept the workspace moving,
            // not the counts the deeper views report.
            let mut names = self
                .digest
                .briefs
                .iter()
                .flat_map(|brief| brief.authors.iter().map(|author| author.name.clone()))
                .collect::<Vec<_>>();
            names.sort();
            names.dedup();
            let workspace = ("workspace", workspace.as_str().into());
            let mut subtitle = match names.as_slice() {
                [] => t_args("activity-catch-up-quiet", &[workspace]),
                [name] => t_args("activity-catch-up-one", &[workspace, ("name", name.into())]),
                [first, second] => t_args(
                    "activity-catch-up-two",
                    &[
                        workspace,
                        ("first", first.into()),
                        ("second", second.into()),
                    ],
                ),
                [first, second, rest @ ..] => t_args(
                    "activity-catch-up-many",
                    &[
                        workspace,
                        ("first", first.into()),
                        ("second", second.into()),
                        ("others", rest.len().into()),
                    ],
                ),
            };
            if self.feed_coverage.pending_tree {
                push_part(&mut subtitle, &t("activity-pending-edits"));
            }
            let subtitle = tr.elide_to_width_style(
                &subtitle,
                text_width(self.sheet.width() - 32.0),
                &sub_style,
            );
            tr.draw_style(
                scene,
                &subtitle,
                colors.text_dim,
                at(transform, self.sheet.x0 + 16.0, subtitle_y),
                None,
                &sub_style,
            );
            return;
        }
        let counts = [
            ("changes", ArgValue::from(change_count)),
            ("people", contributor_count.into()),
        ];
        let mut subtitle = match self.scope {
            RoomScope::ThisRoom => t_args(
                "activity-changes-workspace",
                &[
                    ("workspace", workspace.as_str().into()),
                    counts[0].clone(),
                    counts[1].clone(),
                ],
            ),
            RoomScope::AllWorkspaces => t_args("activity-changes-everywhere", &counts),
        };
        if self.feed_coverage.pending_tree {
            push_part(&mut subtitle, &t("activity-pending-edits"));
        }
        if !self.feed_coverage.is_complete() {
            if !self.feed_coverage.excluded_apps.is_empty() {
                push_part(
                    &mut subtitle,
                    &t_args(
                        "activity-coverage-excluded",
                        &[("apps", self.feed_coverage.excluded_apps.join(", ").into())],
                    ),
                );
            }
            let missing = self.feed_coverage.unavailable_records.len();
            let capped = self.feed_coverage.capped_records;
            if missing > 0 {
                push_part(
                    &mut subtitle,
                    &t_args("activity-coverage-missing", &[("count", missing.into())]),
                );
            }
            if capped > 0 {
                push_part(
                    &mut subtitle,
                    &t_args("activity-coverage-capped", &[("count", capped.into())]),
                );
            }
        }
        if self.view == ActivityView::Replay && self.replay_unavailable > 0 {
            push_part(
                &mut subtitle,
                &t_args(
                    "activity-replay-checkpoints-unavailable",
                    &[("count", self.replay_unavailable.into())],
                ),
            );
        }
        let wrap_width = text_width(self.sheet.width() - 32.0);
        let latest = if self.view == ActivityView::Replay {
            self.replay_presentation_note.clone()
        } else {
            None
        }
        .or_else(|| {
            self.narrative
                .as_ref()
                .map(|narrative| narrative.text.clone())
                .or_else(|| self.digest.headline())
                .map(|headline| t_args("activity-latest", &[("headline", headline.into())]))
        });
        // The subtitle may wrap, but the header band holds one subtitle line
        // plus "Latest", which used to sit at an authored offset underneath,
        // so a two-line subtitle painted straight through the headline.
        // Measure the wrapped subtitle; if the stack cannot fit above the
        // view tabs, fall back to one elided line rather than overlapping.
        let stack_bottom = self.sheet.y0 + self.header_h();
        let (_, latest_h) = latest.as_deref().map_or((0.0, 0.0), |latest| {
            tr.measure_style(latest, None, &sub_style)
        });
        let (_, wrapped_h) = tr.measure_style(&subtitle, Some(wrap_width), &sub_style);
        let fits_wrapped = subtitle_y + f64::from(wrapped_h) + f64::from(latest_h) <= stack_bottom;
        let (subtitle, subtitle_h) = if fits_wrapped {
            (subtitle, wrapped_h)
        } else {
            let one_line = tr.elide_to_width_style(&subtitle, wrap_width, &sub_style);
            let (_, h) = tr.measure_style(&one_line, None, &sub_style);
            (one_line, h)
        };
        tr.draw_style(
            scene,
            &subtitle,
            colors.text_dim,
            at(transform, self.sheet.x0 + 16.0, subtitle_y),
            fits_wrapped.then_some(wrap_width),
            &sub_style,
        );
        if let Some(latest) = latest {
            let latest = tr.elide_to_width_style(&latest, wrap_width, &sub_style);
            tr.draw_style(
                scene,
                &latest,
                colors.text_primary,
                at(
                    transform,
                    self.sheet.x0 + 16.0,
                    // Directly under the subtitle's measured stack — never
                    // over it, and never higher than one sub-line below the
                    // title, so a one-line subtitle keeps the authored seat.
                    subtitle_y + f64::from(subtitle_h).max(self.sub_line_h()),
                ),
                None,
                &sub_style,
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
        let style = tokens.typography.style(TextRole::Caption);
        // Selector pill, hit-tested through a full pointer target, centred
        // on the title's line so the pair share a centre at every text scale.
        let h = SELECTOR_H;
        let cy = self.sheet.y0 + HEADER_TOP + self.title_line_h() / 2.0;
        let label_sets = [
            [
                t("activity-scope-this-workspace"),
                t("activity-scope-all-workspaces"),
            ],
            [
                t("activity-scope-workspace-short"),
                t("activity-scope-all-short"),
            ],
            [t("activity-scope-here"), t("activity-scope-all-short")],
        ];
        let available = (self.sheet.width() * 0.62).max(1.0);
        let first_set = usize::from(self.sheet.width() < 520.0);
        let (labels, widths, total) = label_sets
            .iter()
            .skip(first_set)
            .map(|labels| {
                let widths = labels
                    .iter()
                    .map(|label| {
                        (f64::from(tr.measure_style(label, None, &style).0) + 16.0).max(MIN_TARGET)
                    })
                    .collect::<Vec<_>>();
                let total = widths.iter().sum::<f64>() + 4.0;
                (labels, widths, total)
            })
            .find(|(_, _, total)| *total < available)
            .unwrap_or_else(|| {
                let labels = &label_sets[2];
                let widths = labels
                    .iter()
                    .map(|label| {
                        (f64::from(tr.measure_style(label, None, &style).0) + 12.0).max(MIN_TARGET)
                    })
                    .collect::<Vec<_>>();
                let total = widths.iter().sum::<f64>() + 4.0;
                (labels, widths, total)
            });
        let mut x = self.sheet.x1 - 16.0 - total;
        for (i, sc) in RoomScope::ALL.iter().enumerate() {
            if *sc == RoomScope::AllWorkspaces && !self.host.all_workspaces_available() {
                continue;
            }
            let w = widths.get(i).copied().unwrap_or(0.0);
            let label = labels.get(i).cloned().unwrap_or_else(|| sc.label());
            let rect = Rect::new(x, cy - h / 2.0, x + w, cy + h / 2.0);
            self.scope_rects.push((
                *sc,
                Rect::new(x, cy - MIN_TARGET / 2.0, x + w, cy + MIN_TARGET / 2.0),
            ));
            let active = self.scope == *sc;
            let shape = RoundedRect::from_rect(rect, h / 2.0);
            paint_selector_pill(scene, transform, theme, &shape, active);
            let (lw, lh) = tr.measure_style(&label, None, &style);
            tr.draw_style(
                scene,
                &label,
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
                &style,
            );
            x += w + 4.0;
        }
    }

    /// Paint labelled Time and People filters on one row.
    fn draw_filter_chip_rows(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let y = self.sheet.y0 + self.header_h() + VIEW_ROW_H;
        let x0 = self.sheet.x0 + 12.0;
        let x1 = self.sheet.x1 - 12.0;
        let mid = x0 + ((x1 - x0) * 0.48).max(160.0);
        let row_h = CHIP_ROW_H;
        let time_x = draw_filter_axis_label(
            scene,
            transform,
            tr,
            theme,
            Rect::new(x0, y, mid - 8.0, y + row_h),
            &t("activity-filter-time"),
        );
        let periods: Vec<(Period, String)> = Period::ALL.iter().map(|p| (*p, p.label())).collect();
        let selected = self.period;
        self.period_rects = draw_chip_row(
            scene,
            transform,
            tr,
            theme,
            Rect::new(time_x, y, mid - 8.0, y + row_h),
            &periods,
            |p| *p == selected,
        );

        if self.author_offset >= self.contributors.len() {
            self.author_offset = 0;
        }
        let people_x = draw_filter_axis_label(
            scene,
            transform,
            tr,
            theme,
            Rect::new(mid, y, x1, y + row_h),
            &t("activity-filter-people"),
        );
        let mut people: Vec<(Option<String>, String)> = vec![(None, t("activity-people-everyone"))];
        people.extend(
            self.contributors
                .iter()
                .skip(self.author_offset)
                .map(|c| (Some(c.clone()), c.clone())),
        );
        let picked = self.author.clone();
        let mut page = draw_chip_row_paginated(
            scene,
            transform,
            tr,
            theme,
            Rect::new(people_x, y, x1, y + row_h),
            &people,
            |a| *a == picked,
        );
        if self.author_offset > 0 && page.more_rect.is_none() {
            let row = Rect::new(people_x, y, x1, y + row_h);
            page.more_rect = draw_more_row(
                scene,
                transform,
                tr,
                theme,
                row,
                row.y0 + 1.0,
                &t("activity-first-people"),
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
        tree: &mut impl ActivityTree,
    ) -> RoomActivityOutcome {
        self.handle_event_with_records(event, scale_factor, modifiers, cursor, tree, None)
    }

    fn handle_pointer_click(
        &mut self,
        point: Point,
        tree: &mut impl ActivityTree,
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
        tree: &mut impl ActivityTree,
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

    fn open_task_composer(&mut self, tree: &impl ActivityTree) {
        if !self.host.mutations_available() || tree.document().pending_ops() > 0 {
            return;
        }
        self.task_compose_open = true;
        self.task_draft.clear();
        self.task_route = WorkspaceTaskRoute::Manual;
        self.task_agent = tree
            .active_workspace()
            .and_then(|workspace| self.host.agent_members(workspace).into_iter().next())
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
                    agent_request: None,
                })
        });
    }

    fn close_task_composer(&mut self) {
        self.task_compose_open = false;
        self.task_draft.clear();
        self.task_attachment = None;
    }

    fn task_creator_label(&self, task: &WorkspaceTask) -> String {
        let handle = automerge::ActorId::from(task.created_by.as_bytes()).to_hex_string();
        let authors = self.source.authors();
        if authors.is_registered(&handle) {
            return authors.resolve(&handle).name;
        }
        if task.created_by == self.local_user() {
            return t("workspace-settings-you");
        }
        if task.created_by.len() == 64 && task.created_by.bytes().all(|b| b.is_ascii_hexdigit()) {
            return format!(
                "{} · {}",
                t("activity-unknown-collaborator"),
                ui::history_source::short_id(&task.created_by),
            );
        }
        participant_display_label(&task.created_by)
    }

    fn local_user(&self) -> String {
        self.explicit_local_user
            .clone()
            .unwrap_or_else(|| self.host.local_user())
    }

    /// Close the draft only after the native owner retained its actual receipt.
    /// Hand a task run the pane asked for to its agent. Call it with the tree
    /// unlocked: the broker checks the room's authority against the tree.
    /// On success the caller refreshes the pane, which shows the run; a
    /// refusal is kept as the agent's failed start, so the task offers a
    /// retry with the reason.
    pub fn dispatch_task(&mut self, request: TaskDispatchRequest) -> bool {
        let TaskDispatchRequest {
            room,
            task_id,
            assigned_agent,
        } = request;
        match self.host.dispatch_task(&room, &task_id) {
            Ok(()) => true,
            Err(detail) => {
                if let Some(agent) = assigned_agent {
                    let detail = if detail.is_empty() {
                        t("activity-agent-dispatch-failed")
                    } else {
                        detail
                    };
                    self.agent_startup
                        .insert((room, agent), AgentStartup::Failed { detail });
                }
                false
            }
        }
    }

    pub fn complete_native_manual_task(&mut self, id: String, tree: &mut impl ActivityTree) {
        self.selected_task = Some(id);
        self.close_task_composer();
        self.refresh(tree);
    }

    fn create_task_from_draft(&mut self, tree: &mut impl ActivityTree) -> RoomActivityOutcome {
        if !self.host.mutations_available() || tree.document().pending_ops() > 0 {
            if tree.native_manual_tasks(tree.active) {
                return RoomActivityOutcome {
                    handled: true,
                    task_refused: true,
                    ..RoomActivityOutcome::default()
                };
            }
            self.close_task_composer();
            return handled_outcome();
        }
        let workspace = tree.active;
        if self.task_route == WorkspaceTaskRoute::Manual && tree.native_manual_tasks(workspace) {
            let Some(slot) = tree.active_workspace() else {
                return handled_outcome();
            };
            return RoomActivityOutcome {
                handled: true,
                create_native_manual: Some(NativeManualTaskRequest {
                    workspace,
                    room: slot.room.to_string(),
                    title: self.task_draft.clone(),
                    actor: self.local_user(),
                    resource: self.task_attachment.clone(),
                }),
                ..RoomActivityOutcome::default()
            };
        }
        if tree.native_manual_tasks(workspace) {
            let Some(slot) = tree.active_workspace() else {
                return handled_outcome();
            };
            let Some(agent) = self.task_agent.clone().filter(|agent| !agent.is_empty()) else {
                return RoomActivityOutcome {
                    handled: true,
                    task_refused: true,
                    ..Default::default()
                };
            };
            return RoomActivityOutcome {
                handled: true,
                create_native_managed: Some(NativeManagedTaskRequest {
                    person_draft: NativeManualTaskRequest {
                        workspace,
                        room: slot.room.to_string(),
                        title: self.task_draft.clone(),
                        actor: self.local_user(),
                        resource: self.task_attachment.clone(),
                    },
                    assigned_agent: agent,
                    route: self.task_route,
                }),
                ..Default::default()
            };
        }
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
        tree: &mut impl ActivityTree,
    ) -> RoomActivityOutcome {
        if matches!(
            action,
            TaskAction::ReviewHistory
                | TaskAction::ApproveRun
                | TaskAction::Steer
                | TaskAction::CancelRun
                | TaskAction::RetryRun
                | TaskAction::SendDirection
                | TaskAction::CloseDirection
                | TaskAction::NextTasks
                | TaskAction::PreviousTasks
        ) {
            self.apply_review_action(action);
            return handled_outcome();
        }
        if matches!(
            action,
            TaskAction::ConfirmDraft
                | TaskAction::EditDraft
                | TaskAction::DiscardEdits
                | TaskAction::DeclineDraft
                | TaskAction::SendDecline
                | TaskAction::KeepDraft
                | TaskAction::AlwaysAllow
        ) {
            self.apply_confirmation_action(action);
            return handled_outcome();
        }
        if action != TaskAction::OpenResource
            && (!self.host.mutations_available() || tree.document().pending_ops() > 0)
        {
            return handled_outcome();
        }
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
            return RoomActivityOutcome {
                handled: true,
                dispatch_task: Some(TaskDispatchRequest {
                    room: self.task_room.clone(),
                    task_id,
                    assigned_agent: task.assigned_agent.clone(),
                }),
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
            let state = match self.host.launch_agent(agent, &workspace) {
                Ok(()) => AgentStartup::Starting {
                    since: Instant::now(),
                },
                Err(error) => AgentStartup::Failed { detail: error },
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
            _ => {
                return handled_outcome();
            }
        };
        let native_manual =
            task.route == WorkspaceTaskRoute::Manual && tree.native_manual_tasks(tree.active);
        let changed =
            tree.set_workspace_task_status(tree.active, &task_id, status, &self.local_user());
        if changed {
            self.refresh(tree);
        }
        RoomActivityOutcome {
            handled: true,
            task_changed: changed,
            task_refused: !changed && native_manual,
            ..RoomActivityOutcome::default()
        }
    }

    fn reviewed_task_description(&self, task: &WorkspaceTask) -> String {
        let mut description =
            task_accessibility_description(task, self.now(), &self.task_creator_label(task));
        if let Some((_, control)) = self.reviewed_control(&task.id) {
            for line in task_review_view::task_run_evidence(control, self.now()) {
                description.push('\n');
                description.push_str(&line);
            }
        }
        description
    }

    fn task_actions_for(&self, task: &WorkspaceTask) -> Vec<TaskAction> {
        if let Some((snapshot, control)) = self.reviewed_control(&task.id) {
            let mut actions = vec![TaskAction::ReviewHistory];
            if task.resource.is_some() {
                actions.push(TaskAction::OpenResource);
            }
            if snapshot.writable && self.review_pending.is_none() {
                if task.status == WorkspaceTaskStatus::Queued
                    && control.active_run.is_none()
                    && task.assigned_agent.is_some()
                    && task.route != WorkspaceTaskRoute::Manual
                    && task
                        .resource
                        .as_ref()
                        .is_none_or(|resource| resource.agent_request.is_none())
                {
                    actions.push(TaskAction::ApproveRun);
                }
                if task.status == WorkspaceTaskStatus::Waiting && control.active_run.is_none() {
                    actions.push(TaskAction::RetryRun);
                }
                if !task.status.is_finished() {
                    if control.active_run.is_some() {
                        actions.push(TaskAction::Steer);
                    }
                    actions.push(TaskAction::CancelRun);
                }
            }
            // A reviewed run reaches its agent only once that agent
            // subscribes, so an offline assigned agent must be startable
            // here too: approving alone would wait for it forever.
            if self.host.mutations_available()
                && !self.feed_coverage.pending_tree
                && !task.status.is_finished()
                && task.route != WorkspaceTaskRoute::Manual
                && let Some(agent) = task.assigned_agent.as_deref()
                && !self.agent_is_live(agent)
                && !matches!(
                    self.agent_startup_state(agent),
                    Some(AgentStartup::Starting { .. })
                )
            {
                actions.insert(1, TaskAction::StartAgent);
            }
            return actions;
        }
        let mut actions = task_actions(task);
        if self.host.task_review_required(&self.task_room)
            && task.route != WorkspaceTaskRoute::Manual
        {
            actions.retain(|action| {
                !matches!(
                    action,
                    TaskAction::Cancel | TaskAction::Complete | TaskAction::Start
                )
            });
        }
        if !self.host.mutations_available() || self.feed_coverage.pending_tree {
            actions.retain(|action| *action == TaskAction::OpenResource);
            return actions;
        }
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
            t("activity-task-retry-agent")
        } else {
            action.label()
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
            return Some(t("activity-task-notice-no-agent"));
        };
        if self.agent_is_live(agent) {
            return Some(t("activity-task-notice-ready"));
        }
        match self.agent_startup_state(agent) {
            Some(AgentStartup::Starting { .. }) => Some(t("activity-task-notice-starting")),
            Some(AgentStartup::Failed { detail }) => Some(t_args(
                "activity-task-notice-offline",
                &[("detail", detail.into())],
            )),
            None => Some(t("activity-task-notice-no-runtime")),
        }
    }

    fn handle_catchup_click(
        &mut self,
        point: Point,
        tree: &mut impl ActivityTree,
    ) -> Option<RoomActivityOutcome> {
        if self
            .acknowledge_rect
            .is_some_and(|rect| rect.contains(point))
        {
            self.keyboard_focus = Some(ActivityFocus::Acknowledge);
            self.finish_view(tree.active.raw());
            return Some(handled_outcome());
        }
        if let Some((key, command)) = hit_value(&self.morning_command_rects, point) {
            self.keyboard_focus = Some(ActivityFocus::MorningCommand(key, command));
            return Some(self.activate_focus(tree, None));
        }
        if let Some((key, remove)) = hit_value(&self.morning_rects, point) {
            self.keyboard_focus = Some(ActivityFocus::Morning(key, remove));
            return Some(self.activate_focus(tree, None));
        }
        if self.resume_rect.contains(point)
            && let Some(open_target) = self.resume_target.clone()
        {
            self.keyboard_focus = Some(ActivityFocus::Resume);
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
        tree: &mut impl ActivityTree,
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
        tree: &mut impl ActivityTree,
        records: Option<&RecordStore>,
    ) -> RoomActivityOutcome {
        let mut outcome = handled_outcome();
        match action {
            DetailAction::MorningSource => self.toggle_morning_selection(),
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
                self.last_heads = tree.document_mut().get_heads();
                outcome.compared = self.comparison.is_some();
            }
            DetailAction::Restore => {
                if let Some(id) = self.selected_ids().last().copied()
                    && self.revert_to_with_records(tree, records, &id)
                {
                    outcome.reverted = true;
                }
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
        if !self.host.mutations_available() {
            return UndoApply::default();
        }
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
            self.agent_undo_notice =
                Some((indexed.run.id, t("activity-undo-task-needs-documents")));
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
                        let _ = tilo_resource::history::commit_stamped(doc);
                    }
                    outcome.state
                })
                .unwrap_or(StepState::Unreachable);
            wrote |= matches!(state, StepState::Undone { .. } | StepState::KeptSome { .. });
            partial |= !state.is_clean();
        }
        let notice = t(if wrote && partial {
            "activity-undo-task-partial"
        } else if wrote {
            "activity-undo-task-done"
        } else {
            "activity-undo-task-gone"
        });
        self.agent_undo_notice = Some((indexed.run.id, notice));
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
        tree: &mut impl ActivityTree,
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
                    ui::delete_last_grapheme(&mut self.task_draft);
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
        tree: &mut impl ActivityTree,
        records: Option<&RecordStore>,
    ) -> RoomActivityOutcome {
        if tree.document().pending_ops() > 0 {
            self.defer_pending_tree(tree);
            return handled_outcome();
        }
        let mut out = RoomActivityOutcome::default();
        if cursor.is_some_and(|point| {
            self.direction
                .as_ref()
                .is_some_and(|draft| draft.rect.contains(point))
        }) && matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) {
            self.keyboard_focus = Some(ActivityFocus::DirectionField);
        }
        if self.direction_event(event, scale_factor, *modifiers, tree) {
            return handled_outcome();
        }
        if self.confirmation_event(event, (scale_factor, *modifiers, cursor), tree) {
            return handled_outcome();
        }

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
            &mut ui::widget::EventCtx {
                scale_factor,
                modifiers,
                state: ui::widget::Edits::lend(tree.document_mut()),
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &scratch,
            },
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

    #[expect(
        clippy::too_many_lines,
        reason = "one ordered list of every focusable control, view by view"
    )]
    fn focus_order(&self) -> Vec<ActivityFocus> {
        let mut order = Vec::new();
        order.extend(
            self.scope_rects
                .iter()
                .map(|(scope, _)| ActivityFocus::Scope(*scope)),
        );
        if self.view != ActivityView::CatchUp {
            order.extend(
                self.view_rects
                    .iter()
                    .map(|(view, _)| ActivityFocus::View(*view)),
            );
        }
        match self.view {
            ActivityView::CatchUp => {
                if self.acknowledge_rect.is_some() {
                    order.push(ActivityFocus::Acknowledge);
                }
                order.extend(
                    self.view_rects
                        .iter()
                        .map(|(view, _)| ActivityFocus::View(*view)),
                );
                order.extend(
                    self.morning_rects
                        .iter()
                        .map(|((key, remove), _)| ActivityFocus::Morning(key.clone(), *remove)),
                );
                order.extend(
                    self.morning_command_rects
                        .iter()
                        .map(|((key, command), _)| {
                            ActivityFocus::MorningCommand(key.clone(), *command)
                        }),
                );
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
                if self.direction.is_some() {
                    order.push(ActivityFocus::DirectionField);
                }
                order.extend(self.confirmation_focus_order());
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

    fn activate_brief(&mut self, index: usize) {
        if self.expanded_brief == Some(index) {
            self.open_brief_in_explorer(index);
        } else {
            self.expanded_brief = Some(index);
            self.brief_expansion_started = Some((index, Instant::now()));
        }
    }

    fn activate_focus(
        &mut self,
        tree: &mut impl ActivityTree,
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
            ActivityFocus::MorningCommand(key, command) => {
                self.submit_morning_command(&key, command);
            }
            ActivityFocus::Morning(key, remove) => {
                outcome.open_target = self.activate_morning_source(&key, remove);
            }
            ActivityFocus::Resume => outcome.open_target.clone_from(&self.resume_target),
            ActivityFocus::Brief(index) => self.activate_brief(index),
            ActivityFocus::WorkSessionBoundary(boundary) => {
                outcome.named = self.move_work_session_boundary(tree, boundary);
            }
            ActivityFocus::MoreBriefs => self.advance_briefs(),
            ActivityFocus::Acknowledge => self.finish_view(tree.active.raw()),
            ActivityFocus::NewTask => self.open_task_composer(tree),
            ActivityFocus::Task(id) => {
                self.selected_task =
                    (self.selected_task.as_deref() != Some(id.as_str())).then_some(id);
            }
            ActivityFocus::TaskRoute(route) => self.task_route = route,
            ActivityFocus::DirectionField
            | ActivityFocus::ConfirmField(_)
            | ActivityFocus::DeclineReason => {}
            ActivityFocus::TaskCreate => return self.create_task_from_draft(tree),
            ActivityFocus::TaskCancelCompose => self.close_task_composer(),
            ActivityFocus::TaskAction(action) => return self.apply_task_action(action, tree),
            ActivityFocus::Lens(value) => {
                self.lens = value;
                self.reinstall_filter();
            }
            ActivityFocus::MoreLenses => self.advance_lenses(),
            ActivityFocus::Detail(DetailAction::MorningSource) => self.toggle_morning_selection(),
            ActivityFocus::Detail(DetailAction::Open) => {
                outcome.open_target = self.selected_resource_target();
            }
            ActivityFocus::Detail(DetailAction::Compare) => {
                self.comparison_attempted = true;
                self.comparison = self.compare_selection(tree, records);
                self.comparison_selection = self.selected_ids();
                // Reading an `AutoCommit` comparison closes any pending
                // transaction. That changes the observed heads without
                // changing content; keep refresh from treating the next
                // paint as an external edit and discarding the result.
                self.last_heads = tree.document_mut().get_heads();
                outcome.compared = self.comparison.is_some();
            }
            ActivityFocus::Detail(DetailAction::Restore) => {
                if let Some(id) = self.selected_ids().last().copied()
                    && self.revert_to_with_records(tree, records, &id)
                {
                    outcome.reverted = true;
                }
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
        tree: &mut impl ActivityTree,
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
            ActivityFocus::Scope(value) => Some(value.label()),
            ActivityFocus::View(ActivityView::Tasks) if self.view == ActivityView::CatchUp => {
                Some(t("activity-catch-up-review-goals"))
            }
            ActivityFocus::View(value) => Some(value.label()),
            ActivityFocus::Period(value) => Some(value.label()),
            ActivityFocus::Author(Some(value))
            | ActivityFocus::Lens(
                LensSelection::App(value)
                | LensSelection::ResourceKind(value)
                | LensSelection::Resource(value)
                | LensSelection::WorkGroup(value),
            ) => Some(value.clone()),
            ActivityFocus::Author(None) => Some(t("a11y-shell-activity-everyone")),
            ActivityFocus::MorePeople => Some(t("a11y-shell-activity-more-people")),
            ActivityFocus::MorningCommand(_, command) => Some(t(command.label_key())),
            ActivityFocus::Morning(key, remove) => Some(if *remove {
                t("activity-morning-remove")
            } else {
                self.morning_target(key).map_or_else(
                    || t("activity-morning-unavailable"),
                    |target| morning_sources::morning_resource_name(&target),
                )
            }),
            ActivityFocus::Resume => self.resume_target.as_ref().map(|target| {
                t_args(
                    "a11y-shell-activity-resume",
                    &[("target", (&target.label).into())],
                )
            }),
            ActivityFocus::Brief(index) => self
                .digest
                .briefs
                .get(*index)
                .map(|brief| brief.title.clone()),
            ActivityFocus::WorkSessionBoundary(boundary) => Some(match boundary.direction {
                BoundaryDirection::IntoNewer => t("a11y-shell-activity-boundary-newer"),
                BoundaryDirection::IntoOlder => t("a11y-shell-activity-boundary-older"),
            }),
            ActivityFocus::MoreBriefs => Some(t("a11y-shell-activity-more-sessions")),
            ActivityFocus::Acknowledge => Some(t("activity-acknowledge-viewed")),
            ActivityFocus::NewTask => Some(t("a11y-shell-activity-new-task")),
            ActivityFocus::Task(id) => self
                .tasks
                .iter()
                .find(|task| &task.id == id)
                .map(|task| task.title.clone()),
            ActivityFocus::TaskRoute(route) => Some(task_route_label(*route)),
            ActivityFocus::DirectionField => Some(t("activity-task-direction-label")),
            ActivityFocus::ConfirmField(_) | ActivityFocus::DeclineReason => {
                self.confirmation_field_label(focus)
            }
            ActivityFocus::TaskCreate => Some(t("a11y-shell-activity-create-task")),
            ActivityFocus::TaskCancelCompose => Some(t("a11y-shell-activity-cancel-task")),
            ActivityFocus::TaskAction(action) => Some(action.label()),
            ActivityFocus::Lens(LensSelection::All) => Some(t("a11y-shell-activity-everything")),
            ActivityFocus::Lens(LensSelection::AgentTasks) => {
                Some(t("a11y-shell-activity-agent-tasks"))
            }
            ActivityFocus::Lens(LensSelection::Facet(key, value)) => {
                Some(format!("{key}: {value}"))
            }
            ActivityFocus::Lens(LensSelection::Unclassified) => {
                Some(t("a11y-shell-activity-unclassified"))
            }
            ActivityFocus::MoreLenses => Some(t("a11y-shell-activity-more-lenses")),
            ActivityFocus::Detail(action) => Some(self.detail_action_label(*action)),
            ActivityFocus::ExplorerPane(pane) => Some(pane.label()),
            ActivityFocus::Timeline => Some(t("a11y-shell-activity-history-entries")),
            ActivityFocus::ReplayBack => Some(t("a11y-shell-activity-previous-checkpoint")),
            ActivityFocus::ReplayPlay => Some(if self.replay_playing {
                t("a11y-shell-activity-pause-replay")
            } else {
                t("a11y-shell-activity-play-replay")
            }),
            ActivityFocus::ReplayForward => Some(t("a11y-shell-activity-next-checkpoint")),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "exhaustive focus-to-accessibility mapping is clearest beside the focus enum variants"
    )]
    fn access_control(&self, focus: &ActivityFocus) -> Option<AccessControl> {
        use ui::a11y::Role;

        let control = |role, selected, expanded, description: String| AccessControl {
            role,
            selected,
            expanded,
            description,
        };
        Some(match focus {
            ActivityFocus::Acknowledge => control(
                Role::Button,
                false,
                None,
                t("activity-acknowledge-description"),
            ),
            ActivityFocus::Scope(value) => control(
                Role::RadioButton,
                self.scope == *value,
                None,
                t("a11y-shell-activity-history-scope"),
            ),
            ActivityFocus::View(ActivityView::Tasks) if self.view == ActivityView::CatchUp => {
                control(
                    Role::Button,
                    false,
                    None,
                    t("activity-catch-up-review-description"),
                )
            }
            ActivityFocus::View(value) => control(
                Role::Tab,
                self.view == *value,
                None,
                t("a11y-shell-activity-history-view"),
            ),
            ActivityFocus::Period(value) => control(
                Role::RadioButton,
                self.period == *value,
                None,
                t("a11y-shell-activity-time-range"),
            ),
            ActivityFocus::Author(value) => control(
                Role::RadioButton,
                self.author == *value,
                None,
                t("a11y-shell-activity-person-filter"),
            ),
            ActivityFocus::MorePeople => control(
                Role::Button,
                false,
                None,
                t("a11y-shell-activity-more-people-description"),
            ),
            ActivityFocus::MorningCommand(_, command) => control(
                Role::Button,
                false,
                None,
                t(if *command == MorningCommand::DraftRefresh {
                    "activity-morning-draft-description"
                } else {
                    command.label_key()
                }),
            ),
            ActivityFocus::Morning(key, _) => control(
                Role::Button,
                false,
                None,
                format!(
                    "{}\n{}",
                    self.morning_display_label(key, self.morning_evidence(key).as_ref()),
                    self.morning_payload_text(key)
                ),
            ),
            ActivityFocus::Resume => {
                let target = self.resume_target.as_ref()?;
                control(
                    Role::Button,
                    false,
                    None,
                    t_args(
                        "a11y-shell-activity-resume-description",
                        &[("target", (&target.label).into())],
                    ),
                )
            }
            ActivityFocus::Brief(index) => {
                let brief = self.digest.briefs.get(*index)?;
                let mut description = t_args(
                    "a11y-shell-activity-brief-description",
                    &[
                        ("detail", (&brief.detail).into()),
                        (
                            "when",
                            format_relative_time(brief.last_at, self.now()).into(),
                        ),
                        ("count", brief.changes.into()),
                        (
                            "authors",
                            brief
                                .authors
                                .iter()
                                .map(|author| author.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                                .into(),
                        ),
                    ],
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
                        t("a11y-shell-activity-boundary-newer-description")
                    }
                    BoundaryDirection::IntoOlder => {
                        t("a11y-shell-activity-boundary-older-description")
                    }
                },
            ),
            ActivityFocus::MoreBriefs => control(
                Role::Button,
                false,
                None,
                t("a11y-shell-activity-more-sessions-description"),
            ),
            ActivityFocus::NewTask => control(
                Role::Button,
                false,
                None,
                t("a11y-shell-activity-new-task-description"),
            ),
            ActivityFocus::Task(id) => {
                let task = self.tasks.iter().find(|task| &task.id == id)?;
                AccessControl {
                    role: Role::Button,
                    selected: self.selected_task.as_ref() == Some(id),
                    expanded: Some(self.selected_task.as_ref() == Some(id)),
                    description: format!(
                        "{}; {}; {}; {}",
                        task_status_label(task.status),
                        task_route_label(task.route),
                        task.assigned_agent.as_deref().map_or_else(
                            || t("a11y-shell-activity-task-unassigned"),
                            |_| t("a11y-shell-activity-task-assigned-agent"),
                        ),
                        task.resource.as_ref().map_or_else(
                            || t("a11y-shell-activity-task-no-resource"),
                            |resource| resource.label.clone(),
                        )
                    ),
                }
            }
            ActivityFocus::TaskRoute(route) => control(
                Role::RadioButton,
                self.task_route == *route,
                None,
                t("a11y-shell-activity-task-route"),
            ),
            ActivityFocus::TaskCreate | ActivityFocus::TaskCancelCompose => control(
                Role::Button,
                false,
                None,
                t("a11y-shell-activity-task-composer-action"),
            ),
            ActivityFocus::DirectionField => control(
                Role::TextInput,
                false,
                None,
                t("activity-task-direction-label"),
            ),
            ActivityFocus::ConfirmField(_) | ActivityFocus::DeclineReason => control(
                Role::TextInput,
                false,
                None,
                t("activity-confirm-field-description"),
            ),
            ActivityFocus::TaskAction(_) => control(
                Role::Button,
                false,
                None,
                t("a11y-shell-activity-task-action"),
            ),
            ActivityFocus::Lens(value) => control(
                Role::RadioButton,
                self.lens == *value,
                None,
                t("a11y-shell-activity-history-lens"),
            ),
            ActivityFocus::MoreLenses => control(
                Role::Button,
                false,
                None,
                t("a11y-shell-activity-more-lenses-description"),
            ),
            ActivityFocus::Detail(DetailAction::UndoTask) => control(
                Role::Button,
                false,
                None,
                t("a11y-shell-activity-undo-task-description"),
            ),
            ActivityFocus::Detail(_) => control(
                Role::Button,
                false,
                None,
                t("a11y-shell-activity-history-action"),
            ),
            ActivityFocus::ExplorerPane(value) => control(
                Role::Tab,
                self.explorer_pane == *value,
                None,
                t("a11y-shell-activity-explorer-panel"),
            ),
            ActivityFocus::Timeline => control(
                Role::List,
                true,
                None,
                t("a11y-shell-activity-timeline-description"),
            ),
            ActivityFocus::ReplayBack | ActivityFocus::ReplayForward => control(
                Role::Button,
                false,
                None,
                t("a11y-shell-activity-checkpoints-description"),
            ),
            ActivityFocus::ReplayPlay => control(
                Role::Button,
                self.replay_playing,
                None,
                t("a11y-shell-activity-replay-description"),
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

        if *focus == ActivityFocus::DirectionField {
            return self.direction.as_ref()?.input.accessibility();
        }
        if matches!(
            focus,
            ActivityFocus::ConfirmField(_) | ActivityFocus::DeclineReason
        ) {
            return self.confirmation_field_accessibility(focus);
        }
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
        let node = AccessNode::new(control.role)
            .with_name(label)
            .with_description(control.description)
            .with_state(state)
            .with_position(index.saturating_add(1), set_size);
        // The same rect the pointer hits, so assistive technology, the
        // control socket and the UX bench can all address this control.
        Some(match self.rect_for_focus(focus) {
            Some(rect) if rect.area() > 0.0 => node.with_bounds(rect),
            _ => node,
        })
    }

    /// Semantic mirror of this immediate-mode overlay. Shell overlays are not
    /// backed by taffy `NodeId`s, so the app-level widget walker cannot infer
    /// this tree. The overlay accessibility bridge can publish this snapshot
    /// verbatim; state is derived from the same values used for paint and hit
    /// testing, preventing spoken and visible selection from drifting apart.
    pub fn accessibility_snapshot(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        let mut root = AccessNode::new(Role::Window)
            .with_name(t("a11y-shell-workspace-activity"))
            .with_description(match self.view {
                ActivityView::CatchUp => t("a11y-shell-activity-view-catch-up"),
                ActivityView::Tasks => t("a11y-shell-activity-view-tasks"),
                ActivityView::Explore => t("a11y-shell-activity-view-explore"),
                ActivityView::Replay => t("a11y-shell-activity-view-replay"),
            })
            .with_state(AccessState::new());
        if self.sheet.area() > 0.0 {
            root = root.with_bounds(self.sheet);
        }
        let order = self.focus_order();
        let set_size = order.len();
        root.children = order
            .iter()
            .enumerate()
            .filter_map(|(index, focus)| self.accessibility_control_node(focus, index, set_size))
            .collect();
        if self.view == ActivityView::CatchUp && !self.tasks.is_empty() {
            root.children
                .push(AccessNode::new(Role::StaticText).with_name(self.catch_up_task_summary()));
            if let Some(task) = self
                .tasks
                .iter()
                .find(|task| task.status == WorkspaceTaskStatus::Waiting)
            {
                root.children
                    .push(AccessNode::new(Role::StaticText).with_name(t_args(
                        "activity-catch-up-waiting-detail",
                        &[
                            ("task", task.title.as_str().into()),
                            (
                                "who",
                                task_assignee_label(task, &self.task_creator_label(task)).into(),
                            ),
                        ],
                    )));
            }
        }
        if self.timeline_is_visible() {
            let mut history =
                AccessNode::new(Role::List).with_name(t("a11y-shell-activity-history-entries"));
            history.children = (0..self.timeline.visible_len().min(100))
                .filter_map(|index| self.timeline.visible_entry(index))
                .map(|entry| {
                    AccessNode::new(Role::ListItem)
                        .with_name(entry.summary.clone())
                        .with_description(t_args(
                            "a11y-shell-activity-entry-author",
                            &[("author", (&entry.author.name).into())],
                        ))
                })
                .collect();
            root.children.push(history);
        }
        if self.view == ActivityView::Tasks {
            let mut tasks = AccessNode::new(Role::List).with_name(t("a11y-shell-activity-tasks"));
            tasks.children = self
                .tasks
                .iter()
                .map(|task| {
                    AccessNode::new(Role::ListItem)
                        .with_name(task.title.clone())
                        .with_description(self.reviewed_task_description(task))
                })
                .collect();
            root.children.push(tasks);
        }
        if let Some(detail) = self.selection_detail() {
            root.children.push(
                AccessNode::new(Role::Group)
                    .with_name(t("a11y-shell-activity-selected-detail"))
                    .with_description(t_args(
                        "a11y-shell-activity-selected-detail-description",
                        &[
                            ("title", (&detail.title).into()),
                            ("count", detail.entry_ids.len().into()),
                            ("authors", detail.authors.join(", ").into()),
                        ],
                    )),
            );
        }
        if let Some(indexed) = self.selected_agent_run() {
            let description = self.agent_run_accessibility_description(&indexed.run);
            root.children.push(
                AccessNode::new(Role::Group)
                    .with_name(t("a11y-shell-activity-agent-review"))
                    .with_description(description),
            );
        }
        root
    }

    /// What an agent run reads as to assistive technology: the instruction,
    /// who issued it, which agent ran it, why, and every step it took.
    fn agent_run_accessibility_description(&self, run: &AgentRun) -> String {
        let mut description = t_args(
            "a11y-shell-activity-agent-run-description",
            &[
                (
                    "instruction",
                    run.instruction.as_deref().unwrap_or(&run.label).into(),
                ),
                ("issuer", self.issuer_name(run).into()),
                ("agent", self.agent_name(run).into()),
                (
                    "reason",
                    run.decision
                        .clone()
                        .unwrap_or_else(|| t("a11y-shell-activity-reason-not-recorded"))
                        .into(),
                ),
            ],
        );
        for step in &run.steps {
            description.push_str("; ");
            description.push_str(&t_args(
                "a11y-shell-activity-agent-step",
                &[
                    ("app", humanize_app_id(&step.app).into()),
                    ("record", (&step.record).into()),
                    ("summary", step.sentence().into()),
                ],
            ));
        }
        description
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
            ActivityFocus::MorningCommand(key, command) => self
                .morning_command_rects
                .iter()
                .find_map(|((id, item), rect)| (id == key && item == command).then_some(*rect)),
            ActivityFocus::Morning(key, remove) => {
                self.morning_rects
                    .iter()
                    .find_map(|((id, is_remove), rect)| {
                        (id == key && is_remove == remove).then_some(*rect)
                    })
            }
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
            ActivityFocus::Acknowledge => self.acknowledge_rect,
            ActivityFocus::NewTask => self.new_task_rect,
            ActivityFocus::Task(value) => self
                .task_rects
                .iter()
                .find_map(|(id, rect)| (id == value).then_some(*rect)),
            ActivityFocus::TaskRoute(value) => self
                .task_route_rects
                .iter()
                .find_map(|(route, rect)| (route == value).then_some(*rect)),
            ActivityFocus::DirectionField => self.direction.as_ref().map(|draft| draft.rect),
            ActivityFocus::ConfirmField(_) | ActivityFocus::DeclineReason => {
                self.confirmation_field_rect(focus)
            }
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
    pub fn apply_name(&mut self, tree: &mut impl ActivityTree, req: &GroupNameRequest) -> bool {
        if !self.host.mutations_available() {
            return false;
        }
        let before = tree.document_mut().get_heads();
        if req.label.is_empty() {
            let Some(record) = req.record_id.as_deref() else {
                return false;
            };
            if !ui::history_groups::clear_name(tree.document_mut(), record) {
                return false;
            }
        } else {
            ui::history_groups::name_range(
                tree.document_mut(),
                &req.start,
                Some(&req.end),
                &req.label,
            );
        }
        tree.commit_activity_change();
        if tree.document_mut().get_heads() == before {
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
    pub fn revert_to(&mut self, tree: &mut impl ActivityTree, id: &HistoryEntryId) -> bool {
        self.revert_to_with_records(tree, None, id)
    }

    fn revert_to_with_records(
        &mut self,
        tree: &mut impl ActivityTree,
        records: Option<&RecordStore>,
        id: &HistoryEntryId,
    ) -> bool {
        if !self.host.mutations_available() {
            return false;
        }
        let origin = self.projected_origins.get(id).cloned();
        if origin.as_ref().is_some_and(|address| {
            matches!(
                address.origin,
                super::index::HistoryOrigin::AppRecord { .. }
            )
        }) {
            return false;
        }
        if let Some(WorkspaceHistoryId {
            origin: super::index::HistoryOrigin::Record { id: record, .. },
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
                        tilo_resource::history::commit_stamped(doc).is_some()
                    })
                    .unwrap_or(false)
            });
        }
        let native = origin.map_or(*id, |address| address.change);
        let Some(target) = ui::heads_for_entry(tree.document_mut(), &native) else {
            return false;
        };
        let current = tree.document_mut().get_heads();
        if current == target {
            return false;
        }
        ui::state::write_revert_patches(tree.document_mut(), &current, &target);
        tree.commit_activity_change();
        if tree.document_mut().get_heads() == current {
            return false;
        }
        // Bring the read model back in line with what just changed
        // underneath it, so the revert shows up as the newest entry.
        let _ = tree.resync_activity_cache();
        self.last_heads.clear();
        self.refresh(tree);
        true
    }

    fn undo_band(
        &mut self,
        tree: &mut impl ActivityTree,
        records: Option<&RecordStore>,
        request: &BandUndoRequest,
    ) -> UndoApply {
        if !self.host.mutations_available() {
            return UndoApply::default();
        }
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
                let origin = self
                    .projected_origins
                    .get(&entry.id)
                    .map_or(super::index::HistoryOrigin::DesktopTree, |address| {
                        address.origin.clone()
                    });
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
                    super::index::HistoryOrigin::AppRecord { .. } => UndoApply::default(),
                    super::index::HistoryOrigin::DesktopTree => {
                        let Ok(outcome) =
                            ui::revert_run(tree.document_mut(), &run, None, self.source.authors())
                        else {
                            return UndoApply::default();
                        };
                        if !outcome.wrote {
                            return UndoApply::default();
                        }
                        tree.commit_activity_change();
                        let _ = tree.resync_activity_cache();
                        self.last_heads.clear();
                        self.refresh(tree);
                        UndoApply {
                            wrote: true,
                            partial: !outcome.is_complete(),
                        }
                    }
                    super::index::HistoryOrigin::Record { id, .. } => {
                        records.map_or_else(UndoApply::default, |records| {
                            records
                                .with(&id, |doc| {
                                    let Ok(outcome) =
                                        ui::revert_run(doc, &run, None, self.source.authors())
                                    else {
                                        return UndoApply::default();
                                    };
                                    let committed = outcome.wrote
                                        && tilo_resource::history::commit_stamped(doc).is_some();
                                    UndoApply {
                                        wrote: committed,
                                        partial: committed && !outcome.is_complete(),
                                    }
                                })
                                .unwrap_or_default()
                        })
                    }
                }
            }
            // Tree-only callers and pre-index tests retain native addressing.
            (None, None) => {
                let run = ChangeRun::new(request.start, request.end);
                let Ok(outcome) =
                    ui::revert_run(tree.document_mut(), &run, None, self.source.authors())
                else {
                    return UndoApply::default();
                };
                if !outcome.wrote {
                    return UndoApply::default();
                }
                tree.commit_activity_change();
                let _ = tree.resync_activity_cache();
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
    ui::widget::Button::paint(
        scene,
        transform,
        tr,
        theme,
        &ui::widget::ButtonFace::new(rect, label).role(if prominent {
            ButtonRole::Primary
        } else {
            ButtonRole::Secondary
        }),
    );
}

#[allow(
    clippy::too_many_lines,
    reason = "task-card rendering needs the shared renderer context and explicit card data together to keep the visible and accessible states aligned"
)]
fn draw_task_card(
    ctx: &mut PaintCtx<'_>,
    task: &WorkspaceTask,
    rect: Rect,
    selected: bool,
    now: SystemTime,
    execution_notice: Option<&str>,
    creator: &str,
) {
    let (scene, transform, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.lift(if selected { 0.55 } else { 0.3 }),
        None,
        &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.control)),
    );
    if selected {
        scene.stroke(
            &Stroke::new(1.5),
            transform,
            theme.tokens.colors.border_focus,
            None,
            &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.control)),
        );
    }
    let status = task_status_label(task.status);
    tr.draw_style(
        scene,
        &status,
        if task.status.is_finished() {
            theme.tokens.colors.text_dim
        } else {
            theme.tokens.colors.intent.inverted_hover
        },
        at(transform, rect.x0 + 12.0, rect.y0 + 9.0),
        None,
        &theme.text(TextRole::Label),
    );
    let route = task_route_label(task.route);
    let route_width = tr
        .measure_style(&route, None, &theme.text(TextRole::Caption))
        .0;
    tr.draw_style(
        scene,
        &route,
        theme.tokens.colors.text_dim,
        at(
            transform,
            rect.x1 - 12.0 - f64::from(route_width),
            rect.y0 + 9.0,
        ),
        None,
        &theme.text(TextRole::Caption),
    );
    let title = tr.elide_to_width_style(
        &task.title,
        text_width(rect.width() - 24.0),
        &theme.text(TextRole::Body),
    );
    tr.draw_style(
        scene,
        &title,
        theme.tokens.colors.text_primary,
        at(transform, rect.x0 + 12.0, rect.y0 + 31.0),
        None,
        &theme
            .text(TextRole::Body)
            .with_weight(ui::text_render::TextWeight::Medium),
    );
    let assignee = task_assignee_label(task, creator);
    let resource = task
        .resource
        .as_ref()
        .map(|resource| format!(" · {}", resource.label))
        .unwrap_or_default();
    let byline = tr.elide_to_width_style(
        &format!("{assignee}{resource}"),
        text_width(rect.width() - 24.0),
        &theme.text(TextRole::Caption),
    );
    tr.draw_style(
        scene,
        &byline,
        theme.tokens.colors.text_dim,
        at(transform, rect.x0 + 12.0, rect.y0 + 55.0),
        None,
        &theme.text(TextRole::Caption),
    );
    if selected {
        let mut audit_y = rect.y0 + 80.0;
        if let Some(notice) = execution_notice {
            tr.draw_style(
                scene,
                notice,
                theme.tokens.colors.intent.inverted_hover,
                at(transform, rect.x0 + 12.0, audit_y),
                Some(text_width(rect.width() - 24.0)),
                &theme.text(TextRole::Caption),
            );
            audit_y += 18.0;
        }
        tr.draw_style(
            scene,
            &t("activity-task-audit-trail"),
            theme.tokens.colors.text_dim,
            at(transform, rect.x0 + 12.0, audit_y),
            None,
            &theme.text(TextRole::Label),
        );
        audit_y += 20.0;
        for event in task.audit.iter().rev().take(3) {
            let event_time = u64::try_from(event.at)
                .ok()
                .and_then(|secs| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs)))
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let line = task_audit_line(event, event_time, now);
            let line = tr.elide_to_width_style(
                &line,
                text_width(rect.width() - 32.0),
                &theme.text(TextRole::Caption),
            );
            tr.draw_style(
                scene,
                &line,
                theme.tokens.colors.text_primary,
                at(transform, rect.x0 + 20.0, audit_y),
                None,
                &theme.text(TextRole::Caption),
            );
            audit_y += 18.0;
        }
    }
}

fn task_assignee_label(task: &WorkspaceTask, creator: &str) -> String {
    task.assigned_agent.as_ref().map_or_else(
        || {
            t_args(
                "activity-task-unassigned-by",
                &[("creator", creator.into())],
            )
        },
        |agent| {
            // The card already owns the prominent AGENT badge, so reuse the
            // shared presentation while avoiding a redundant second suffix.
            let name = ui::history_source::agent_principal_name(agent)
                .unwrap_or_else(|| agent.trim().to_string());
            t_args("activity-task-agent-badge", &[("name", name.into())])
        },
    )
}

fn task_audit_line(
    event: &tilo_desktop_model::WorkspaceTaskAuditEvent,
    event_time: SystemTime,
    now: SystemTime,
) -> String {
    crate::i18n::ensure();
    let what = ui::history_source::sentence::sentence(&event.entry());
    let what = match &event.note {
        Some(note) => t_args(
            "activity-task-audit-noted",
            &[("what", what.into()), ("note", note.as_str().into())],
        ),
        None => what,
    };
    format!(
        "{} · {} · {}",
        participant_display_label(&event.actor),
        what,
        format_relative_time(event_time, now)
    )
}

fn task_accessibility_description(task: &WorkspaceTask, now: SystemTime, creator: &str) -> String {
    let latest = task.audit.last().map(|event| {
        let event_time = u64::try_from(event.at)
            .ok()
            .and_then(|secs| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs)))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        task_audit_line(event, event_time, now)
    });
    let mut description = t_args(
        "a11y-shell-activity-task-description",
        &[
            ("status", task_status_label(task.status).into()),
            ("route", task_route_label(task.route).into()),
            ("assignee", task_assignee_label(task, creator).into()),
            ("count", task.audit.len().into()),
        ],
    );
    if let Some(latest) = latest {
        description.push_str("; ");
        description.push_str(&t_args(
            "a11y-shell-activity-task-latest",
            &[("latest", latest.into())],
        ));
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
        "{}\n{}\n{}{}",
        t_args(
            "activity-detail-action-count",
            &[("count", detail.entry_ids.len().into())],
        ),
        people,
        if resources.is_empty() {
            t("activity-no-resource")
        } else {
            resources
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
            automerge::PatchAction::PutMap { key, conflict, .. } => field_change(
                if *conflict {
                    "activity-change-resolved-field"
                } else {
                    "activity-change-updated-field"
                },
                humanize_field(key),
            ),
            automerge::PatchAction::DeleteMap { key } => {
                field_change("activity-change-removed-field", humanize_field(key))
            }
            automerge::PatchAction::PutSeq { conflict, .. } => t(if *conflict {
                "activity-change-resolved-list"
            } else {
                "activity-change-updated-list-item"
            }),
            automerge::PatchAction::Insert { values, .. } => {
                counted("activity-change-added-items", values.len())
            }
            automerge::PatchAction::SpliceText { .. } => t("activity-change-edited-text"),
            automerge::PatchAction::Increment { prop, value } => {
                let field = match prop {
                    Prop::Map(key) => humanize_field(key),
                    Prop::Seq(_) => t("activity-change-a-counter"),
                };
                field_change(
                    if *value < 0 {
                        "activity-change-decreased"
                    } else {
                        "activity-change-increased"
                    },
                    field,
                )
            }
            automerge::PatchAction::Conflict { prop } => match prop {
                Prop::Map(key) => {
                    field_change("activity-change-concurrent-field", humanize_field(key))
                }
                Prop::Seq(_) => t("activity-change-concurrent-list"),
            },
            automerge::PatchAction::DeleteSeq { length, .. } => {
                counted("activity-change-removed-items", *length)
            }
            automerge::PatchAction::Mark { .. } => t("activity-change-formatting"),
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
        t("activity-change-content")
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
        tr.draw_style(
            scene,
            &t("activity-replay-no-workspace"),
            theme.tokens.colors.text_dim,
            at(transform, bounds.x0 + 16.0, bounds.y0 + 16.0),
            Some(text_width(bounds.width() - 32.0)),
            &theme.text(TextRole::Caption),
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
        tr.draw_style(
            scene,
            &workspace.name,
            theme.tokens.colors.text_primary,
            at(transform, row.x0 + 6.0, row.y0 + 5.0),
            Some(text_width(row.width() - 12.0)),
            &theme.text(TextRole::Label),
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
                ui::surface::card(
                    scene,
                    transform,
                    rect,
                    f64::from(theme.tokens.radii.control),
                    theme.tokens.colors.lift(0.35),
                );
                let title = group
                    .tabs
                    .get(group.active)
                    .or_else(|| group.tabs.first())
                    .cloned()
                    .unwrap_or_else(|| t("activity-replay-empty-window"));
                tr.draw_style(
                    scene,
                    &title,
                    theme.tokens.colors.text_primary,
                    at(transform, rect.x0 + 8.0, rect.y0 + 8.0),
                    Some(text_width(rect.width() - 16.0)),
                    &theme.text(TextRole::Caption),
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
                            tr.measure_style(&label, None, &theme.text(TextRole::Caption))
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
                                theme.tokens.colors.text_primary.with_alpha_factor(0.18)
                            } else {
                                theme.tokens.colors.lift(0.55)
                            },
                            None,
                            &chip,
                        );
                        tr.draw_style(
                            scene,
                            &label,
                            theme.tokens.colors.text_dim,
                            at(transform, tab_x + 6.0, tab_y + 4.0),
                            Some(text_width(width - 12.0)),
                            &theme.text(TextRole::Caption),
                        );
                        tab_x += width + 5.0;
                    }
                    if group.tabs.len() > 4 && tab_x + 28.0 < rect.x1 {
                        tr.draw_style(
                            scene,
                            &format!("+{}", group.tabs.len().saturating_sub(4)),
                            theme.tokens.colors.text_dim,
                            at(transform, tab_x + 2.0, tab_y + 4.0),
                            None,
                            &theme.text(TextRole::Caption),
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
    tr.draw_style(
        scene,
        &t("activity-presence-trail"),
        theme.tokens.colors.text_dim,
        at(transform, bounds.x0, bounds.y0 + 2.0),
        None,
        &theme.text(TextRole::Caption),
    );
    let start = current.saturating_sub(8);
    let slice = frames.get(start..=current).unwrap_or(&[]);
    let x0 = bounds.x0 + 92.0;
    let available = (bounds.x1 - x0).max(1.0);
    let step = available / slice.len().max(1) as f64;
    for (index, frame) in slice.iter().enumerate() {
        let x = x0 + step * (index as f64 + 0.5);
        if frame.pointers.is_empty() {
            let (r, g, b) = frame.author.rgb();
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
        tr.draw_style(
            scene,
            &pointer.name,
            color,
            at(transform, point.x + 14.0, point.y + 4.0),
            Some(text_width(100.0)),
            &theme.text(TextRole::Caption),
        );
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "brief-card rendering keeps its geometry, summary, and accessible label in one retained pass"
)]
fn draw_brief_card(
    ctx: &mut PaintCtx<'_>,
    brief: &ActivityBrief,
    rect: Rect,
    m: BriefCardMetrics,
    expanded: bool,
    now: SystemTime,
) {
    let (scene, transform, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    let rail_x = rect.x0 + BRIEF_RAIL_X;
    let text_x = rect.x0 + BRIEF_TEXT_INSET;
    scene.stroke(
        &Stroke::new(2.0),
        transform,
        ui::surface::edge_on(theme.tokens.colors.surfaces.raised),
        None,
        &Line::new((rail_x, rect.y0), (rail_x, rect.y1 + 8.0)),
    );
    scene.fill(
        Fill::NonZero,
        transform,
        ui::surface::edge_on(theme.tokens.colors.surfaces.raised),
        None,
        &vello::kurbo::Circle::new(
            (rail_x, rect.y0 + m.pad + m.title_lh / 2.0),
            BRIEF_RAIL_KNOB,
        ),
    );
    let width = brief_text_width(rect.width());
    let title_lines = tr.clamp_to_lines(&brief.title, &theme.text(TextRole::Body), width, 2);
    // Every line sits where `BriefCardMetrics` says it does — the same
    // arithmetic that sized the card in `draw_catch_up`, so the lines and
    // the card they are in cannot disagree at any text scale.
    for (index, title) in title_lines.iter().enumerate() {
        tr.draw_style(
            scene,
            title,
            theme.tokens.colors.text_primary,
            at(
                transform,
                text_x,
                rect.y0 + m.pad + small_count_to_f64(index) * m.title_lh,
            ),
            None,
            &theme
                .text(TextRole::Body)
                .with_weight(ui::text_render::TextWeight::Medium),
        );
    }
    let title_count = title_lines.len();
    let byline = format!(
        "{} \u{00b7} {}",
        brief.detail,
        format_relative_time(brief.last_at, now)
    );
    let byline_y = rect.y0 + m.byline_top(title_count);
    let byline_x = draw_brief_author_stack(
        scene,
        transform,
        tr,
        theme,
        &brief.authors,
        text_x,
        byline_y,
    );
    let byline_width = text_width((rect.x1 - 12.0 - byline_x).max(0.0));
    let byline = tr.elide_to_width_style(&byline, byline_width, &theme.text(TextRole::Caption));
    tr.draw_style(
        scene,
        &byline,
        theme.tokens.colors.text_dim,
        at(transform, byline_x, byline_y),
        None,
        &theme.text(TextRole::Caption),
    );
    let mut provenance = brief.apps.join(", ");
    if !brief.resources.is_empty() {
        if !provenance.is_empty() {
            provenance.push_str(" · ");
        }
        provenance.push_str(&brief.resources.join(", "));
    }
    if provenance.is_empty() {
        provenance = counted("activity-brief-underlying", brief.changes);
    }
    let provenance = tr.elide_to_width_style(&provenance, width, &theme.text(TextRole::Caption));
    tr.draw_style(
        scene,
        &provenance,
        theme.tokens.colors.text_dim,
        at(transform, text_x, rect.y0 + m.provenance_top(title_count)),
        None,
        &theme.text(TextRole::Caption),
    );
    if expanded {
        let mut action_y = rect.y0 + m.actions_top(title_count);
        for action in &brief.actions {
            if action_y + 24.0 > rect.y1 - 28.0 {
                break;
            }
            let (r, g, b) = action.author.rgb();
            let initial = action
                .author
                .name
                .chars()
                .next()
                .unwrap_or('\u{2022}')
                .to_uppercase()
                .collect::<String>();
            ui::widget::AvatarFace::new(&initial, "")
                .presence(vello::peniko::Color::from_rgba8(r, g, b, 255))
                .paint(
                    &mut ui::paint::PaintCtx::new(scene, transform, tr, theme),
                    Rect::new(rail_x - 8.0, action_y - 2.0, rail_x + 8.0, action_y + 14.0),
                );
            let action_line = format!(
                "{} · {} · {}",
                action.author.name,
                action.summary,
                format_relative_time(action.timestamp, now)
            );
            let action_line =
                tr.elide_to_width_style(&action_line, width, &theme.text(TextRole::Caption));
            tr.draw_style(
                scene,
                &action_line,
                theme.tokens.colors.text_primary,
                at(transform, text_x, action_y),
                None,
                &theme.text(TextRole::Caption),
            );
            action_y += m.action_pitch();
        }
    }
    let affordance = t(if expanded {
        "activity-brief-open-explorer"
    } else {
        "activity-brief-show-affected"
    });
    let action_count = if expanded { brief.actions.len() } else { 0 };
    tr.draw_style(
        scene,
        &affordance,
        if expanded {
            theme.tokens.colors.intent.inverted_hover
        } else {
            theme.tokens.colors.text_dim
        },
        at(
            transform,
            text_x,
            rect.y0 + m.affordance_top(title_count, action_count),
        ),
        Some(width),
        &theme.text(TextRole::Caption),
    );
}

/// Idle floor of a Selector pill in this sheet, on the `lift` ramp under
/// the selection tier so the two states stay ordered on every palette.
const PILL_FLOOR_LIFT_T: f32 = 0.35;

/// Paint a Selector pill (view tab, scope pill, filter chip): the chosen one
/// is the shared selection tier — a lift with its hairline (HIG §9.3), never
/// an accent fill, which the accent's "live" meaning (§9.4) forbids — and an
/// idle one sits on a quiet floor so the row reads as pills, not bare text.
fn paint_selector_pill(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    shape: &RoundedRect,
    active: bool,
) {
    paint_tiered(scene, transform, theme, shape, active, None);
}

/// A Control chip inside the sheet (detail actions): the chosen one is
/// `selection_tier`; an idle one rests on the lift floor with an edge
/// derived from that floor (HIG §9.5), so it registers on every palette.
fn paint_control_chip(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    shape: &RoundedRect,
    active: bool,
) {
    let floor = theme.tokens.colors.lift(PILL_FLOOR_LIFT_T);
    let idle_edge = ui::surface::edge_on(floor);
    paint_tiered(scene, transform, theme, shape, active, Some(idle_edge));
}

/// The one recipe behind both: the chosen state is `selection_tier`, an
/// idle one rests on the lift floor, and whichever edge applies is stroked
/// on the shape's outermost pixel row (`ui::surface::edge_outline`) so it
/// lands on the same row as every `ui::surface::card` edge on the sheet.
fn paint_tiered(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    shape: &RoundedRect,
    active: bool,
    idle_edge: Option<vello::peniko::Color>,
) {
    let tier = ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(active));
    let fill = if active {
        tier.fill
    } else {
        theme.tokens.colors.lift(PILL_FLOOR_LIFT_T)
    };
    scene.fill(Fill::NonZero, transform, fill, None, shape);
    let border = if active {
        tier.border
    } else {
        tier.border.or(idle_edge)
    };
    if let Some(border) = border {
        let outline = ui::surface::edge_outline(shape.rect(), shape.radii().top_left);
        scene.stroke(&Stroke::new(1.0), transform, border, None, &outline);
    }
}

fn draw_work_session_boundary(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    can_move_up: bool,
    can_move_down: bool,
) {
    let center = rect.x0 + rect.width() / 2.0;
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        theme.tokens.colors.border_standard,
        None,
        &Line::new((rect.x0, rect.y0 + 13.0), (rect.x1, rect.y0 + 13.0)),
    );
    let seam = vello::kurbo::Circle::new((center, rect.y0 + 13.0), 3.5);
    scene.fill(
        Fill::NonZero,
        transform,
        ui::chrome::shell::panel_ground(theme),
        None,
        &seam,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        theme.tokens.colors.border_standard,
        None,
        &seam,
    );
    let (into_newer, into_older) = work_session_boundary_button_rects(rect);
    // The neighboring cards already carry the session names. Keep the seam
    // itself spatial: each half transfers the bordering action across it in
    // the direction shown, instead of repeating two long explanatory labels.
    let buttons = [
        (into_newer, t("activity-session-move-newer"), can_move_up),
        (into_older, t("activity-session-move-older"), can_move_down),
    ];
    for (button, label, enabled) in buttons {
        let shape = RoundedRect::from_rect(button, 13.0);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.lift(if enabled { 0.65 } else { 0.25 }),
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            if enabled {
                theme
                    .tokens
                    .colors
                    .intent
                    .inverted_hover
                    .with_alpha_factor(0.65)
            } else {
                theme.tokens.colors.border_standard
            },
            None,
            &shape,
        );
        let label = tr.elide_to_width_style(
            &label,
            text_width(button.width() - 18.0),
            &theme.text(TextRole::Caption),
        );
        let (width, _) = tr.measure_style(&label, None, &theme.text(TextRole::Caption));
        tr.draw_style(
            scene,
            &label,
            if enabled {
                theme.tokens.colors.text_primary
            } else {
                theme.tokens.colors.text_dim
            },
            at(
                transform,
                button.x0 + (button.width() - f64::from(width)) / 2.0,
                button.y0 + 6.0,
            ),
            None,
            &theme.text(TextRole::Caption),
        );
    }
}

fn work_session_boundary_button_rects(rect: Rect) -> (Rect, Rect) {
    let center = rect.x0 + rect.width() / 2.0;
    let button_width = 104.0_f64.min((rect.width() - 16.0).max(0.0) / 2.0);
    (
        Rect::new(center - 6.0 - button_width, rect.y0, center - 6.0, rect.y1),
        Rect::new(center + 6.0, rect.y0, center + 6.0 + button_width, rect.y1),
    )
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
        let avatar_x = x + small_count_to_f64(index) * 12.0;
        let (red, green, blue) = author.rgb();
        let initial = author
            .name
            .chars()
            .next()
            .unwrap_or('\u{2022}')
            .to_uppercase()
            .collect::<String>();
        ui::widget::AvatarFace::new(&initial, "")
            .presence(vello::peniko::Color::from_rgba8(red, green, blue, 255))
            .paint(
                &mut ui::paint::PaintCtx::new(scene, transform, tr, theme),
                Rect::new(avatar_x, y - 2.0, avatar_x + 18.0, y + 16.0),
            );
    }
    if authors.len() > visible {
        let badge_x = x + small_count_to_f64(visible) * 12.0;
        ui::widget::AvatarFace::paint_overflow(
            &mut ui::paint::PaintCtx::new(scene, transform, tr, theme),
            Rect::new(badge_x, y - 2.0, badge_x + 18.0, y + 16.0),
            authors.len().saturating_sub(visible),
            None,
        );
    }
    // `visible` is at most two, so adding the overflow badge cannot saturate.
    let badges = visible.saturating_add(usize::from(authors.len() > visible));
    if badges == 0 {
        x
    } else {
        x + small_count_to_f64(badges) * 12.0 + 10.0
    }
}

/// A brief card's timeline rail, and the knob beside its title.
const BRIEF_RAIL_X: f64 = 14.0;
const BRIEF_RAIL_KNOB: f64 = 4.0;
/// Where a brief card's text starts: clear of the rail, its knob, and the
/// avatars expanded actions centre on it.
const BRIEF_TEXT_INSET: f64 = 32.0;

/// The width a brief card's lines wrap and elide to. The card is sized from
/// the same width, so a title can't wrap differently from the card it fills.
fn brief_text_width(card_width: f64) -> f32 {
    text_width(card_width - BRIEF_TEXT_INSET - 12.0)
}

/// The vertical rhythm of one catch-up card, from measured line heights so
/// a 200 % card grows to hold its lines instead of stacking them on top of
/// each other (the authored 10 / 34 / 56 / 72 offsets only fit 1× type).
/// Padding `s2`, gaps `s1`.
#[derive(Clone, Copy, Debug)]
struct BriefCardMetrics {
    /// One title line (`scale.base`, medium).
    title_lh: f64,
    /// One byline / provenance / action / affordance line (`scale.sm`).
    line_lh: f64,
    pad: f64,
    gap: f64,
}

impl BriefCardMetrics {
    fn new(tr: &mut TextRenderer, theme: &Theme) -> Self {
        let tokens = &theme.tokens;
        let (_, title_lh) = tr.measure_style(
            "Ag",
            None,
            &tokens
                .typography
                .style(TextRole::Body)
                .with_weight(ui::text_render::TextWeight::Medium),
        );
        let (_, line_lh) =
            tr.measure_style("Ag", None, &tokens.typography.style(TextRole::Caption));
        Self {
            title_lh: f64::from(title_lh),
            line_lh: f64::from(line_lh),
            pad: f64::from(tokens.spacing.s2),
            gap: f64::from(tokens.spacing.s1),
        }
    }
    /// Top of the byline, below `title_lines` title lines.
    fn byline_top(self, title_lines: usize) -> f64 {
        self.pad + small_count_to_f64(title_lines) * self.title_lh + self.gap
    }
    fn provenance_top(self, title_lines: usize) -> f64 {
        self.byline_top(title_lines) + self.line_lh + self.gap
    }
    /// Top of the first expanded action line.
    fn actions_top(self, title_lines: usize) -> f64 {
        self.provenance_top(title_lines) + self.line_lh + self.gap
    }
    /// Pitch between expanded action lines.
    fn action_pitch(self) -> f64 {
        self.line_lh + self.gap
    }
    fn affordance_top(self, title_lines: usize, actions: usize) -> f64 {
        let actions_h = if actions > 0 {
            small_count_to_f64(actions) * self.action_pitch()
        } else {
            0.0
        };
        self.actions_top(title_lines) + actions_h
    }
    /// The whole card, `actions` being the expanded action lines (0 when
    /// collapsed).
    fn height(self, title_lines: usize, actions: usize) -> f64 {
        self.affordance_top(title_lines, actions) + self.line_lh + self.pad
    }
}

/// Widen a small count (title lines, action rows) for layout math.
fn small_count_to_f64(n: usize) -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a card holds at most a handful of lines — far under 2^53"
    )]
    {
        n as f64
    }
}

/// Dim axis label at the start of a filter group. Returns the x where
/// the chips begin.
fn draw_filter_axis_label(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    row: Rect,
    label: &str,
) -> f64 {
    let style = theme.text(TextRole::Caption);
    let (lw, lh) = tr.measure_style(label, None, &style);
    tr.draw_style(
        scene,
        label,
        theme.tokens.colors.text_dim,
        at(
            transform,
            row.x0,
            row.y0 + (row.height() - f64::from(lh)) / 2.0,
        ),
        None,
        &style,
    );
    row.x0 + f64::from(lw) + 8.0
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
    // Control label scale (HIG §9.6); the previous `sm - 1` was an
    // off-scale size invented to make more chips fit.
    let style = tokens.typography.style(TextRole::Caption);
    // Painted as a Selector pill; the returned hit rect is the whole row,
    // so the pointer target is never smaller than the row it sits in.
    let chip_h = row.height().min(SELECTOR_H);
    let cy = row.center().y;
    let mut out = Vec::with_capacity(items.len());
    let mut x = row.x0;
    let mut dropped = 0usize;
    for (value, label) in items {
        // Reserve room for a "+N" overflow marker so it never itself overflows.
        let (lw, lh) = tr.measure_style(label, None, &style);
        // A short label ("All") still gets a full pointer target.
        let w = (f64::from(lw) + 16.0).max(MIN_TARGET);
        if x + w > row.x1 - 28.0 {
            dropped = dropped.saturating_add(1);
            continue;
        }
        let rect = Rect::new(x, cy - chip_h / 2.0, x + w, cy + chip_h / 2.0);
        let hit = Rect::new(x, row.y0, x + w, row.y1);
        let active = is_active(value);
        let shape = RoundedRect::from_rect(rect, chip_h / 2.0);
        paint_selector_pill(scene, transform, theme, &shape, active);
        tr.draw_style(
            scene,
            label,
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
            &style,
        );
        out.push((value.clone(), hit));
        x += w + 6.0;
    }
    if dropped > 0 {
        let more = format!("+{dropped}");
        let (mw, mh) = tr.measure_style(&more, None, &style);
        tr.draw_style(
            scene,
            &more,
            colors.text_dim,
            at(transform, row.x1 - f64::from(mw), cy - f64::from(mh) / 2.0),
            None,
            &style,
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
    let style = tokens.typography.style(TextRole::Caption);
    let chip_h = row.height().min(SELECTOR_H);
    let cy = row.center().y;
    let mut out = Vec::with_capacity(items.len());
    let mut x = row.x0;
    for (value, label) in items {
        let (lw, lh) = tr.measure_style(label, None, &style);
        let w = (f64::from(lw) + 16.0).max(MIN_TARGET);
        if x + w > row.x1 - 58.0 {
            break;
        }
        let rect = Rect::new(x, cy - chip_h / 2.0, x + w, cy + chip_h / 2.0);
        let hit = Rect::new(x, row.y0, x + w, row.y1);
        paint_selector_pill(
            scene,
            transform,
            theme,
            &RoundedRect::from_rect(rect, chip_h / 2.0),
            is_active(value),
        );
        tr.draw_style(
            scene,
            label,
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
            &style,
        );
        out.push((value.clone(), hit));
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
        let more = t("activity-more");
        let (mw, mh) = tr.measure_style(&more, None, &style);
        tr.draw_style(
            scene,
            &more,
            colors.text_primary,
            at(
                transform,
                rect.x0 + (rect.width() - f64::from(mw)) / 2.0,
                rect.y0 + (rect.height() - f64::from(mh)) / 2.0,
            ),
            None,
            &style,
        );
        Some(Rect::new(rect.x0, row.y0, rect.x1, row.y1))
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
    tr.draw_style(
        scene,
        label,
        theme.tokens.colors.text_primary,
        at(transform, rect.x0 + 8.0, rect.y0 + 6.0),
        Some(text_width(rect.width() - 16.0)),
        &theme.text(TextRole::Caption),
    );
    Some(rect)
}

// =====================================================================
// Room vocabulary
// =====================================================================

/// What one room change did, read from its patches once: a change's
/// patches are fixed by its hash, and diffing every change of the room on
/// every refresh made a workspace switch's first frame O(history).
#[derive(Default)]
pub(super) struct RoomChange {
    /// The workspace the change belongs to; `None` for several or none.
    workspace: Option<u64>,
    counts: PatchCounts,
    bookkeeping: bool,
}

impl RoomChange {
    fn read(doc: &mut automerge::AutoCommit, deps: &[ChangeHash], hash: ChangeHash) -> Self {
        #[cfg(test)]
        ROOM_CHANGE_READS.with(|reads| reads.set(reads.get().saturating_add(1)));
        let patches = doc.diff(deps, &[hash]);
        let mut counts = PatchCounts::default();
        for patch in &patches {
            counts.record(patch);
        }
        Self {
            workspace: workspace_touched(doc, &patches, hash),
            counts,
            bookkeeping: is_room_bookkeeping(&patches),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// How many room changes this thread diffed, for the tests that pin
    /// that a refresh reads only new changes.
    pub(super) static ROOM_CHANGE_READS: Cell<usize> = const { Cell::new(0) };
}

/// Rewrite each row's summary in the room's own words where nameable,
/// else keeps [`HistorySource`]'s generic one. Returns, per entry, the
/// workspace the change belongs to — `None` for several or none. `known`
/// keeps each change's reading across refreshes; only new changes are
/// diffed. Each entry's hash and dependencies come from `log`, the history
/// log `entries` were read through, so the room's changes are not listed
/// again.
fn describe_room_changes(
    doc: &mut automerge::AutoCommit,
    log: &ui::history_source::HistoryLog,
    entries: &mut [HistoryEntry],
    known: &mut HashMap<ChangeHash, RoomChange>,
) -> Vec<Option<u64>> {
    let mut tags: Vec<Option<u64>> = vec![None; entries.len()];
    for (entry, tag) in entries.iter_mut().zip(&mut tags) {
        let Some((hash, deps)) = log.change(&entry.id) else {
            continue;
        };
        // `deps -> [hash]` isolates exactly that change.
        let change = known
            .entry(hash)
            .or_insert_with(|| RoomChange::read(doc, deps, hash));
        *tag = change.workspace;
        if let Some((summary, entry_kind)) = change.counts.describe() {
            if entry.context.is_none() {
                entry.summary = summary;
            }
            entry.entry_kind = entry_kind;
            if !change.counts.feed_kind().belongs_in_feed() {
                let mut context = entry.context.take().unwrap_or_default();
                if !context
                    .facets
                    .iter()
                    .any(|facet| facet.key == FEED_OMIT_FACET)
                {
                    context
                        .facets
                        .push(ui::tilo_actions::ActionFacet::new(FEED_OMIT_FACET, "omit"));
                }
                entry.context = Some(context);
            }
        } else if change.bookkeeping {
            entry.context = Some(
                entry
                    .context
                    .take()
                    .unwrap_or_default()
                    .with_change(ui::tilo_actions::ChangeKind::Housekeeping),
            );
        } else if entry.context.is_none() {
            // The room's own storage keys are not words anyone should read.
            entry.summary = t("activity-event-updated");
        }
    }
    tags
}

/// The room keys a desktop writes to keep its projection consistent (the id
/// counter, who owns each id namespace, the record index, schedule claims, a
/// retained strip's marker): state about the room, not something anyone did
/// in it.
const ROOM_BOOKKEEPING_KEYS: &[&str] = &[
    tilo_desktop_model::projection::ROOM_DESKTOP_COUNTER_KEY,
    tilo_desktop_model::projection::ROOM_CONTENTS_OWNERS_KEY,
    tilo_desktop_model::projection::ROOM_RECORDS_KEY,
    tilo_desktop_model::projection::ROOM_SCHEDULE_CLAIMS_KEY,
    tilo_desktop_model::projection::STRIP_RETAINED_EMPTY_KEY,
];

/// Whether a change only wrote the room's bookkeeping.
fn is_room_bookkeeping(patches: &[automerge::Patch]) -> bool {
    !patches.is_empty()
        && patches.iter().all(|patch| {
            let leaf = match &patch.action {
                automerge::PatchAction::PutMap { key, .. }
                | automerge::PatchAction::DeleteMap { key }
                | automerge::PatchAction::Increment {
                    prop: Prop::Map(key),
                    ..
                } => Some(key.as_str()),
                _ => None,
            };
            patch
                .path
                .iter()
                .filter_map(|(_, prop)| match prop {
                    Prop::Map(key) => Some(key.as_str()),
                    Prop::Seq(_) => None,
                })
                .chain(leaf)
                .any(|key| ROOM_BOOKKEEPING_KEYS.contains(&key))
        })
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

/// Running counts of one change's patches, turned into a single room event
/// ([`PatchCounts::describe`]) and its place in the feed
/// ([`PatchCounts::feed_kind`]). Content, membership and permission changes
/// stay in the feed; setup (the workspace's own first writes), a plain open
/// and window layout do not — they are durable, but they are not "what
/// people did".
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
            (
                automerge::PatchAction::PutMap { key, value, .. },
                map @ ("grants" | "sandbox" | "members"),
            ) => {
                let described = match (&value.0, map) {
                    (Value::Scalar(s), _) => match s.as_ref() {
                        ScalarValue::Bytes(blob) => match map {
                            "grants" => serde_json::from_slice::<tilo_action_policy::Grant>(blob)
                                .ok()
                                .map(|grant| describe_grant(&grant)),
                            "sandbox" => serde_json::from_slice::<CapOverride>(blob)
                                .ok()
                                .map(|cap| describe_capability(&cap)),
                            _ => serde_json::from_slice::<tilo_action_policy::Role>(blob)
                                .ok()
                                .map(|role| describe_member(key, role)),
                        },
                        _ => None,
                    },
                    (Value::Object(_), _) => None,
                };
                self.grants
                    .push(described.unwrap_or_else(|| t("activity-capability-unknown")));
            }
            (automerge::PatchAction::DeleteMap { .. }, "grants" | "sandbox" | "members") => {
                self.revokes = self.revokes.saturating_add(1);
            }
            // A room's `contents` map: a window is an entry of a group's
            // `tabs`, and a group closes with its last window.
            (automerge::PatchAction::PutMap { value, .. }, "tabs")
                if matches!(value.0, Value::Object(_)) =>
            {
                self.tabs_added = self.tabs_added.saturating_add(1);
            }
            (automerge::PatchAction::DeleteMap { .. }, "tabs" | "groups") => {
                self.tabs_removed = self.tabs_removed.saturating_add(1);
            }
            (automerge::PatchAction::PutMap { key, .. }, _) if key == "order" => {
                self.groups_moved = self.groups_moved.saturating_add(1);
            }
            // The workspace's own name, not a strip's.
            (automerge::PatchAction::PutMap { key, value, .. }, "" | "workspaces")
                if key == "name" =>
            {
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
    /// noise, and a whole-list write (the first `seed_doc_from_cache`) is
    /// recognised before the per-field cases, or a flush that granted
    /// nothing reads as "granted 4 capabilities".
    fn describe(&self) -> Option<(String, EntryKind)> {
        if self.workspaces_removed > 0 && self.workspaces_added == 0 {
            return Some((
                counted("activity-event-removed-workspaces", self.workspaces_removed),
                EntryKind::Delete,
            ));
        }
        if self.workspaces_added > 0 {
            let s = match self.names.len() {
                0 => counted("activity-event-recorded-workspaces", self.workspaces_added),
                1 => t_args(
                    "activity-event-recorded-workspace",
                    &[("name", self.names.first().map_or("", String::as_str).into())],
                ),
                n => t_args(
                    "activity-event-recorded-named-workspaces",
                    &[
                        ("count", n.into()),
                        ("names", join_titles(&self.names).into()),
                    ],
                ),
            };
            return Some((s, EntryKind::Text));
        }
        // A room document's first change names the room, its owner and its
        // name at once: that is the workspace being made.
        if self.room_set && self.owner_set {
            let s = match self.names.first().filter(|name| !name.is_empty()) {
                Some(name) => t_args("activity-event-created-named", &[("name", name.into())]),
                None => t("activity-event-created"),
            };
            return Some((s, EntryKind::Access));
        }

        if !self.grants.is_empty() {
            let s = if self.grants.len() == 1 {
                t_args(
                    "activity-event-granted",
                    &[(
                        "capability",
                        self.grants.first().map_or("", String::as_str).into(),
                    )],
                )
            } else {
                counted("activity-event-granted-capabilities", self.grants.len())
            };
            return Some((s, EntryKind::Access));
        }
        if self.revokes > 0 {
            return Some((
                counted("activity-event-revoked-capabilities", self.revokes),
                EntryKind::Access,
            ));
        }
        if self.owner_set {
            return Some((t("activity-event-owner"), EntryKind::Access));
        }
        if self.room_set {
            return Some((t("activity-event-identity"), EntryKind::Access));
        }
        if let Some(name) = self.names.first() {
            let s = if name.is_empty() {
                t("activity-event-renamed")
            } else {
                t_args("activity-event-named", &[("name", name.into())])
            };
            return Some((s, EntryKind::Details));
        }
        if self.tabs_added > 0 {
            let s = if self.tab_titles.is_empty() {
                counted("activity-event-opened-windows", self.tabs_added)
            } else {
                t_args(
                    "activity-event-opened",
                    &[("titles", join_titles(&self.tab_titles).into())],
                )
            };
            return Some((s, EntryKind::Text));
        }
        if self.tabs_removed > 0 {
            return Some((
                counted("activity-event-closed-windows", self.tabs_removed),
                EntryKind::Delete,
            ));
        }
        if self.groups_moved > 0 {
            return Some((t("activity-event-rearranged"), EntryKind::Move));
        }
        if self.geometry > 0 {
            return Some((t("activity-event-resized"), EntryKind::Details));
        }
        if self.active_switched {
            return Some((
                // This also feeds the fixed-width top-bar activity summary;
                // "active" adds no useful distinction there and caused the
                // workspace-switch confirmation to truncate at accessibility
                // text sizes.
                t("activity-event-switched"),
                EntryKind::Details,
            ));
        }
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RoomEventKind {
    Change,
    Setup,
    View,
    Layout,
}

impl RoomEventKind {
    fn belongs_in_feed(self) -> bool {
        matches!(self, Self::Change)
    }
}

impl PatchCounts {
    fn feed_kind(&self) -> RoomEventKind {
        if self.workspaces_removed > 0 && self.workspaces_added == 0 {
            return RoomEventKind::Change;
        }
        if self.workspaces_added > 0 {
            return RoomEventKind::Setup;
        }
        if self.room_set && self.owner_set {
            return RoomEventKind::Setup;
        }
        if !self.grants.is_empty() || self.revokes > 0 {
            return RoomEventKind::Change;
        }
        if self.owner_set || self.room_set {
            return RoomEventKind::Setup;
        }
        if !self.names.is_empty() {
            return RoomEventKind::Change;
        }
        if self.tabs_added > 0 {
            return RoomEventKind::View;
        }
        if self.tabs_removed > 0 {
            return RoomEventKind::Change;
        }
        if self.groups_moved > 0 || self.geometry > 0 || self.active_switched {
            return RoomEventKind::Layout;
        }
        RoomEventKind::Change
    }
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

/// A grant in one short phrase: its answer, for which kinds of action, on
/// what, to whom.
fn describe_grant(grant: &tilo_action_policy::Grant) -> String {
    use tilo_action_policy::{ActionKind, Answer};
    let answer = match grant.answer {
        Answer::Allow => "allow",
        Answer::Ask => "ask",
        Answer::Deny => "deny",
    };
    let kinds = grant.kinds;
    let kinds = match (
        kinds.contains(ActionKind::Read),
        kinds.contains(ActionKind::Undoable),
        kinds.contains(ActionKind::Outside),
    ) {
        (true, true, true) => "all",
        (true, true, false) => "read-undoable",
        (true, false, true) => "read-outside",
        (false, true, true) => "undoable-outside",
        (true, false, false) => "read",
        (false, true, false) => "undoable",
        (false, false, true) => "outside",
        (false, false, false) => "none",
    };
    t_args(
        "activity-grant",
        &[
            ("answer", answer.into()),
            ("kinds", kinds.into()),
            (
                "node",
                tilo_action_policy::describe_node(&grant.node, &tilo_action_policy::Ids).into(),
            ),
            ("subject", describe_grantee(&grant.subject).into()),
        ],
    )
}

fn describe_grantee(subject: &tilo_action_policy::Subject) -> String {
    use tilo_action_policy::Subject as Grantee;
    match subject {
        Grantee::Person(id) => short(id),
        Grantee::Role(role) => t_args(
            "activity-capability-role",
            &[("role", role_key(*role).into())],
        ),
        Grantee::Agent(agent) => t_args("activity-capability-agent", &[("agent", agent.into())]),
        Grantee::Task(task) => t_args("activity-grant-task", &[("task", short(task).into())]),
        Grantee::Shortcut(shortcut) => {
            t_args("activity-grant-shortcut", &[("shortcut", shortcut.into())])
        }
        Grantee::App(app) => app.clone(),
        Grantee::Bot(bot) => t_args("activity-grant-bot", &[("bot", short(bot).into())]),
        Grantee::ForeignDevice => t("activity-grant-foreign-device"),
    }
}

/// The catalogue selector of a role.
fn role_key(role: tilo_action_policy::Role) -> &'static str {
    match role {
        tilo_action_policy::Role::Owner => "owner",
        tilo_action_policy::Role::Moderator => "moderator",
        tilo_action_policy::Role::Editor => "editor",
        tilo_action_policy::Role::Commenter => "commenter",
        tilo_action_policy::Role::Viewer => "viewer",
    }
}

/// A member admitted to the room, or their role changed.
fn describe_member(person: &str, role: tilo_action_policy::Role) -> String {
    t_args(
        "activity-member",
        &[
            ("person", short(person).into()),
            ("role", role_key(role).into()),
        ],
    )
}

/// A sandbox capability in one short phrase: what, over what, to whom.
fn describe_capability(cap: &CapOverride) -> String {
    let verb = match cap.access {
        Access::Read => "read",
        Access::Write => "write",
    };
    let effect = match cap.effect {
        Effect::Grant => "grant",
        Effect::Restrict => "restrict",
    };
    t_args(
        "activity-capability",
        &[
            ("effect", effect.into()),
            ("verb", verb.into()),
            ("scope", describe_scope(&cap.scope).into()),
            ("subject", describe_subject(&cap.subject).into()),
        ],
    )
}

fn describe_scope(scope: &Scope) -> String {
    match scope {
        Scope::FsPath(p) => p.clone(),
        // Record authority intentionally has no path: it follows the file
        // across rename/move and the stable id is not useful activity copy.
        Scope::FsRecord(_) => t("activity-capability-one-shared-file"),
        Scope::Device(d) => d.clone(),
        Scope::NetHost(h) => t_args("activity-capability-network", &[("host", h.into())]),
    }
}

fn describe_subject(subject: &Subject) -> String {
    match subject {
        Subject::User(_) => t("activity-capability-a-person"),
        Subject::Principal(p) => describe_principal(p),
    }
}

fn describe_principal(p: &Principal) -> String {
    match p {
        Principal::Person(id) => short(id),
        Principal::App(a) => a.clone(),
        Principal::Agent(a) => t_args("activity-capability-agent", &[("agent", a.into())]),
        Principal::Token(_) => t("activity-capability-invite-token"),
        Principal::Room(_) => t("activity-capability-this-workspace"),
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

/// A catalogue message whose only variable is `$count`, so the catalogue's
/// plural rules pick the wording.
fn counted(key: &str, count: usize) -> String {
    t_args(key, &[("count", count.into())])
}

/// A catalogue message naming one document field.
fn field_change(key: &str, field: String) -> String {
    t_args(key, &[("field", field.into())])
}

/// Append one more ` · `-separated part to a header line.
fn push_part(line: &mut String, part: &str) {
    line.push_str(" \u{00b7} ");
    line.push_str(part);
}

/// A task's status as the card shows it.
fn task_status_label(status: WorkspaceTaskStatus) -> String {
    t(match status {
        WorkspaceTaskStatus::Queued => "activity-task-status-queued",
        WorkspaceTaskStatus::InProgress => "activity-task-status-in-progress",
        WorkspaceTaskStatus::Waiting => "activity-task-status-waiting",
        WorkspaceTaskStatus::Done => "activity-task-status-done",
        WorkspaceTaskStatus::Cancelled => "activity-task-status-cancelled",
    })
}

/// How a task runs, as the card and the composer show it.
fn task_route_label(route: WorkspaceTaskRoute) -> String {
    t(match route {
        WorkspaceTaskRoute::Manual => "activity-task-route-manual",
        WorkspaceTaskRoute::RulesOnly => "activity-task-route-rules-only",
        WorkspaceTaskRoute::Model => "activity-task-route-model",
    })
}

/// How an action with no summary of its own ended.
/// What an effect's receipt says: what the effect did, with how it ended
/// when it did not complete.
fn receipt_summary(
    context: &tilo_actions::ActionContext,
    status: &tilo_actions::EffectReceiptStatus,
) -> String {
    crate::i18n::ensure();
    let Some(what) = ui::history_source::sentence::step_summary(context) else {
        return receipt_status_label(status);
    };
    match status {
        tilo_actions::EffectReceiptStatus::Succeeded => what,
        _ => t_args(
            "activity-receipt-ended",
            &[
                ("status", receipt_status_label(status).into()),
                ("what", what.into()),
            ],
        ),
    }
}

fn receipt_status_label(status: &tilo_actions::EffectReceiptStatus) -> String {
    t(match status {
        tilo_actions::EffectReceiptStatus::Succeeded => "activity-receipt-completed",
        tilo_actions::EffectReceiptStatus::Failed { .. } => "activity-receipt-failed",
        tilo_actions::EffectReceiptStatus::Cancelled => "activity-receipt-cancelled",
    })
}

/// Name up to two windows, then count the rest — the row has a line, not a
/// paragraph.
fn join_titles(titles: &[String]) -> String {
    let first = || titles.first().map_or("", String::as_str);
    let second = || titles.get(1).map_or("", String::as_str);
    match titles.len() {
        0 => String::new(),
        1 => first().to_string(),
        2 => t_args(
            "activity-titles-two",
            &[("first", first().into()), ("second", second().into())],
        ),
        n => t_args(
            "activity-titles-more",
            &[
                ("first", first().into()),
                ("second", second().into()),
                ("count", n.saturating_sub(2).into()),
            ],
        ),
    }
}

// =====================================================================
// Tests
// =====================================================================

include!("workspace_activity_tests.rs");

#[cfg(test)]
#[path = "pane_tests.rs"]
mod pane_tests;
