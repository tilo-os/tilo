use super::morning::{
    MORNING_OPEN, MORNING_REMOVE, MORNING_SOURCE, MorningCommand, MorningPreferences, MorningSource,
};
use tilo_sort::CompactSort as _;
mod morning_sources;
mod serve;
pub use serve::{ServedTree, is_document_write, is_personal, is_read, refused_to_agent};

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
use super::{ActivityDeepLink, ActivityHost, ActivityTree, TaskRunUndo, UnavailableActivityHost};

use crate::i18n::{t, t_args};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use ui::time::{Duration, SystemTime};

use automerge::transaction::Transactable;
use automerge::{ChangeHash, Prop, ReadDoc, ScalarValue, Value};
use collab_types::caps::{Access, CapOverride, Effect, Principal, Scope, Subject};
use ui::presence::PresenceCtx;
use ui::widget::EntryKind;
use ui::widget::{
    ArtifactKind, HistoryClassification, HistoryEntry, HistoryEntryId, HistoryFilter,
};
use ui::{
    ActivityDigest, ActivityTimeline, AgentRun, AuthorDirectory, BandUndoRequest, ChangeRun,
    GroupNameRequest, HistoryComparison, HistoryFacetIndex, HistorySelectionDetail, HistorySource,
    NamedRange, NarrativeRequest, RecordId, RecordStore, StepState, format_relative_time,
    participant_display_label,
};

use super::index::{
    FeedCoverage, WorkspaceDeparture, WorkspaceHistoryEntry, WorkspaceHistoryId,
    WorkspaceHistoryIndex, WorkspaceHistoryQuery, WorkspaceSeenFrontier,
};

/// How long a device must be away from a workspace before its return is
/// greeted with a recap of what changed.
const AWAY_RECAP_AFTER: Duration = Duration::from_secs(45 * 60);

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

/// Facet written onto the rows that are not news: setup and layout rows
/// ([`FEED_OMIT`]) stay out of the feed but in replay; a window opened
/// ([`FEED_HISTORY_ONLY`]) is listed in History but is nobody's news.
const FEED_OMIT_FACET: &str = "tilo.feed";
const FEED_OMIT: &str = "omit";
const FEED_HISTORY_ONLY: &str = "history";

pub const HISTORY_COMPARE: &str = "history.compare";
pub const HISTORY_UNDO: &str = "history.undo";
pub const HISTORY_RESTORE: &str = "history.restore";
pub const ACTIVITY_ACKNOWLEDGE: &str = "room.activity.acknowledge";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActivityView {
    /// Outcome-first summaries since this device's last completed visit.
    #[default]
    CatchUp,
    /// Full granular history with grouping, filters, rename and revert.
    Explore,
    /// Causal workspace checkpoints played as layout + participant changes.
    Replay,
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

/// The agent runs a persisted record held at one revision, so an unchanged
/// document on disk is not read and decoded again on every refresh.
#[derive(Debug)]
struct ColdAgentRuns {
    revision: u64,
    runs: Vec<AgentRun>,
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
    // History already knows the reader's own changes, whatever name the
    // writer gave them.
    if author.relation == ui::widget::history::AuthorRelation::Reader {
        return true;
    }
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
        .filter(is_news)
        .collect()
}

/// The feed tag `entry` carries, if any ([`FEED_OMIT_FACET`]).
fn feed_tag(entry: &HistoryEntry) -> Option<&str> {
    entry.context.as_ref().and_then(|context| {
        context
            .facets
            .iter()
            .find(|facet| facet.key == FEED_OMIT_FACET)
            .map(|facet| facet.value.as_str())
    })
}

/// Whether History lists `entry`.
fn is_feed_change(entry: &HistoryEntry) -> bool {
    entry
        .context
        .as_ref()
        .is_none_or(ui::tilo_actions::ActionContext::is_activity)
        && feed_tag(entry) != Some(FEED_OMIT)
}

/// Whether `entry` is news to someone who did not make it: listed, and more
/// than a window opened.
fn is_news(entry: &HistoryEntry) -> bool {
    is_feed_change(entry) && feed_tag(entry) != Some(FEED_HISTORY_ONLY)
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "these independent flags report distinct effects to the overlay host"
)]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RoomActivityOutcome {
    pub handled: bool,
    pub reverted: bool,
    /// A band was named/renamed/un-named — a document write too.
    pub named: bool,
    pub open_target: Option<ui::HistoryResourceTarget>,
    pub compared: bool,
    pub undo_partial: bool,
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
    author: Option<String>,
    /// First-seen order, derived from the rows rather than a roster — a
    /// name appears only if that person did something in this room.
    contributors: Vec<String>,
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
    morning_sources: HashMap<u64, MorningPreferences>,
    morning_workspace: u64,
    morning_command_message: Option<String>,
    morning_source_offset: usize,
    expanded_brief: Option<usize>,
    host: Rc<dyn ActivityHost>,
    /// The active workspace's room.
    room: String,
    /// The undo of an agent's run the host is carrying out, and the run.
    undo_pending: Option<(uuid::Uuid, String)>,
    facet_index: HistoryFacetIndex,
    lens: LensSelection,
    comparison: Option<HistoryComparison>,
    comparison_attempted: bool,
    comparison_selection: Vec<HistoryEntryId>,
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
    narrative: Option<ui::GroundedNarrative>,
    narrative_request: Option<NarrativeRequest>,
    /// Task receipts live beside the records they describe. They are indexed
    /// here so selecting any affected change can explain the whole task.
    agent_runs: Vec<IndexedAgentRun>,
    cold_agent_runs: HashMap<RecordId, ColdAgentRuns>,
    agent_undo_notice: Option<(String, String)>,
    open_agent_records: HashSet<RecordId>,
    /// What a deep link last asked the feed to show, and how many it asked.
    focus: Option<tilo_actions::remote::workspace_activity::Focus>,
    focus_serial: u64,
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
        let ri = Rc::clone(&revert_intent);
        let ni = Rc::clone(&name_intent);
        let ui = Rc::clone(&undo_intent);
        Self {
            explicit_local_user,
            timeline: {
                let mut t = ActivityTimeline::new()
                    .with_preview_selection()
                    .on_revert(move |id| ri.set(Some(*id)))
                    .on_undo_band(move |req| *ui.borrow_mut() = Some(req.clone()))
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
            author: None,
            contributors: Vec::new(),
            room_name: String::new(),
            total_entries: 0,
            digest: ActivityDigest::default(),
            last_active: None,
            now_override: None,
            seen_frontiers: HashMap::new(),
            viewed_changes: HashMap::new(),
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
            morning_sources: HashMap::new(),
            morning_workspace: 0,
            morning_command_message: None,
            morning_source_offset: 0,
            expanded_brief: None,
            host: Rc::new(UnavailableActivityHost),
            room: String::new(),
            undo_pending: None,
            facet_index: HistoryFacetIndex::default(),
            lens: LensSelection::default(),
            comparison: None,
            comparison_attempted: false,
            comparison_selection: Vec::new(),
            replay_unavailable: 0,
            replay_presentation_note: None,
            replay_frames: Vec::new(),
            history_log: ui::history_source::HistoryLog::default(),
            room_changes: HashMap::new(),
            replay_checkpoints: HashMap::new(),
            replay_index: 0,
            replay_playing: false,
            narrative: None,
            narrative_request: None,
            agent_runs: Vec::new(),
            cold_agent_runs: HashMap::new(),
            agent_undo_notice: None,
            open_agent_records: HashSet::new(),
            focus: None,
            focus_serial: 0,
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

    /// Brief positions change when fresh work arrives. Retain the expanded
    /// work session so it cannot drift onto a different result.
    fn replace_digest(&mut self, digest: ActivityDigest) {
        let anchor = |index: usize| {
            self.digest
                .briefs
                .get(index)
                .and_then(|brief| brief.entry_ids.first())
                .copied()
        };
        let expanded = self.expanded_brief.and_then(anchor);
        let position = |id| {
            digest
                .briefs
                .iter()
                .position(|brief| brief.entry_ids.contains(&id))
        };
        self.expanded_brief = expanded.and_then(position);
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
        if view != ActivityView::Replay {
            self.replay_playing = false;
        }
        self.view = view;
        self.expanded_brief = None;
    }

    pub fn period(&self) -> Period {
        self.period
    }

    pub fn author_filter(&self) -> Option<&str> {
        self.author.as_deref()
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
            if room == self.room {
                self.toggle_morning_selection();
            }
            return handled_outcome();
        }
        if id == MORNING_SOURCE {
            self.toggle_morning_selection();
            return handled_outcome();
        }
        if let Some((room, key)) = super::morning::decode_action(id, MORNING_OPEN) {
            if room != self.room {
                return handled_outcome();
            }
            return RoomActivityOutcome {
                handled: true,
                open_target: self.activate_morning_source(key, false),
                ..Default::default()
            };
        }
        if let Some((room, key)) = super::morning::decode_action(id, MORNING_REMOVE) {
            if room != self.room {
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
                if room == self.room {
                    self.submit_morning_command(key, command);
                }
                return handled_outcome();
            }
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

    /// The run recorded for the agent's work on task `task_id`, if it
    /// changed anything.
    fn task_agent_run(&self, task_id: &str) -> Option<&IndexedAgentRun> {
        self.agent_runs
            .iter()
            .find(|indexed| indexed.run.task.as_deref() == Some(task_id))
    }

    /// Ask the host to undo the agent's whole run of `task_id`, through
    /// each changed document's own history. Returns whether it was asked.
    fn undo_task_run(&mut self, task_id: &str) -> bool {
        if self.undo_pending.is_some() {
            return true;
        }
        let Some(indexed) = self.task_agent_run(task_id) else {
            return false;
        };
        let run = indexed.run.id.clone();
        let invocation = uuid::Uuid::new_v4();
        let undo = TaskRunUndo {
            room: self.room.clone(),
            invocation,
            task_id: task_id.to_owned(),
            steps: indexed
                .run
                .steps
                .iter()
                .map(|step| (step.app.clone(), step.record.clone()))
                .collect(),
        };
        match self.host.undo_task_run(undo) {
            Ok(()) => {
                self.undo_pending = Some((invocation, run));
                true
            }
            Err(error) => {
                self.agent_undo_notice = Some((
                    run,
                    t_args("activity-undo-task-refused", &[("detail", error.into())]),
                ));
                true
            }
        }
    }

    /// Tell how the host's undo of an agent's run ended, once it has.
    fn poll_undo(&mut self) {
        let Some((invocation, run)) = self.undo_pending.clone() else {
            return;
        };
        let Some(result) = self.host.poll_task_run_undo(invocation) else {
            return;
        };
        self.undo_pending = None;
        let notice = match result {
            Ok(()) => t("activity-undo-task-done"),
            Err(error) => t_args("activity-undo-task-refused", &[("detail", error.into())]),
        };
        self.agent_undo_notice = Some((run, notice));
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
        ranges.compact_sort_by_key(|(_, start, _)| *start);
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
                // A task's run groups its changes under the task itself.
                filter.work_groups.extend(
                    self.agent_runs
                        .iter()
                        .filter_map(|indexed| indexed.run.task.clone()),
                );
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
        // History reads its rows in the desktop's words: loading them later
        // would change the reading's basis and read the log again.
        crate::i18n::ensure();
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
            .map(|workspace| workspace.room().to_string())
            .unwrap_or_default();
        if self.room != room || self.morning_workspace != tree.active.raw() {
            self.morning_command_message = None;
            self.morning_source_offset = 0;
        }
        self.room = room;
        self.morning_workspace = tree.active.raw();
        self.poll_undo();

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
        self.derived
            .compact_sort_by_key(|(entry, _)| entry.timestamp);
        // Names come out of the same document as the changes, so a peer's name
        // arrives with their edits.
        self.named = ui::history_groups::read_named_ranges(tree.document(), self.source.authors());
        self.apply_scope(tree);
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
            .map(|w| WorkspaceHistoryQuery::for_room(w.room()))
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
                .map(|workspace| WorkspaceHistoryQuery::for_room(workspace.room()))
                .unwrap_or_default(),
            RoomScope::AllWorkspaces => WorkspaceHistoryQuery::for_rooms(
                tree.workspaces
                    .iter()
                    .map(tilo_desktop_model::Workspace::room),
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
                        if let Some(records) = tree.room_records_held.get(&workspace.room()) {
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
                .map(|workspace| [workspace.room()].into_iter().collect())
                .unwrap_or_default(),
            RoomScope::AllWorkspaces => tree
                .workspaces
                .iter()
                .map(tilo_desktop_model::Workspace::room)
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
        indexed.compact_sort_by(|a, b| {
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
            .filter(|entry| is_news(entry))
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
            for (user, _) in people {
                pointers.push(ReplayPointer {
                    name: presence.display_name(user),
                    color: presence.color_for(user),
                    desktop_space,
                });
            }
        }
        pointers.compact_sort_by(|a, b| a.name.cmp(&b.name));
        pointers.dedup_by(|a, b| a.name == b.name && a.desktop_space == b.desktop_space);
        if !pointers.is_empty() {
            frame.pointers = pointers;
        }
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

    fn local_user(&self) -> String {
        self.explicit_local_user
            .clone()
            .unwrap_or_else(|| self.host.local_user())
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
        // An agent's run of a task is undone by each document's own
        // history, which also takes back what a change queued outside the
        // document (a file renamed on disk); reverting the document alone
        // would leave that behind.
        if let Some(task) = &indexed.run.task
            && self.undo_task_run(task)
        {
            // Quiet until the undo lands or is refused.
            self.agent_undo_notice = None;
            return UndoApply::default();
        }
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

    /// Write (or remove, for an empty label) a name over a run of the
    /// room's changes, committed so it reaches peers through the same
    /// sync as the changes it names.
    pub fn apply_name(&mut self, tree: &mut impl ActivityTree, req: &GroupNameRequest) -> bool {
        if !self.host.mutations_available() {
            return false;
        }
        let before = tree.document_mut().get_heads();
        if !ui::history_groups::apply_name_request(tree.document_mut(), req) {
            return false;
        }
        tree.commit_activity_change();
        if tree.document_mut().get_heads() == before {
            return false;
        }
        self.last_heads.clear();
        self.refresh(tree);
        true
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

/// The app the workspace's tasks live in.
pub const TASKS_APP: &str = "so.tilo.tasks";

fn handled_outcome() -> RoomActivityOutcome {
    RoomActivityOutcome {
        handled: true,
        ..RoomActivityOutcome::default()
    }
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
/// The fields a comparison changed, with what each held either side, read
/// from the before and after documents the comparison kept.
pub(crate) fn comparison_field_changes(
    comparison: &ui::history_source::HistoryComparison,
) -> Vec<tilo_actions::remote::workspace_activity::FieldChange> {
    let before = automerge::AutoCommit::load(&comparison.before_document).ok();
    let after = automerge::AutoCommit::load(&comparison.after_document).ok();
    let (Some(before), Some(after)) = (before, after) else {
        return Vec::new();
    };
    let mut changes: Vec<tilo_actions::remote::workspace_activity::FieldChange> = Vec::new();
    for patch in &comparison.patches {
        let automerge::PatchAction::PutMap { key, .. } = &patch.action else {
            continue;
        };
        let field = humanize_field(key);
        if changes.iter().any(|change| change.field == field) {
            continue;
        }
        changes.push(tilo_actions::remote::workspace_activity::FieldChange {
            before: map_value(&before, &patch.obj, key),
            after: map_value(&after, &patch.obj, key),
            field,
        });
    }
    changes
}

/// What `key` of the map `object` holds in `doc`, as a person reads it.
fn map_value(doc: &automerge::AutoCommit, object: &automerge::ObjId, key: &str) -> String {
    match doc.get(object, key).ok().flatten() {
        None => String::new(),
        Some((automerge::Value::Scalar(value), _)) => match value.as_ref() {
            automerge::ScalarValue::Str(text) => text.to_string(),
            automerge::ScalarValue::Int(number) => number.to_string(),
            automerge::ScalarValue::Uint(number) => number.to_string(),
            automerge::ScalarValue::F64(number) => format!("{number}"),
            automerge::ScalarValue::Boolean(flag) => {
                if *flag {
                    "yes".into()
                } else {
                    "no".into()
                }
            }
            automerge::ScalarValue::Null => String::new(),
            other => format!("{other:?}"),
        },
        // A list or a map reads as what it holds, not as its identity.
        Some(_) => t("activity-change-content"),
    }
}

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
            if let Some(tag) = change.counts.feed_kind().feed_tag() {
                let mut context = entry.context.take().unwrap_or_default();
                if !context
                    .facets
                    .iter()
                    .any(|facet| facet.key == FEED_OMIT_FACET)
                {
                    context
                        .facets
                        .push(ui::tilo_actions::ActionFacet::new(FEED_OMIT_FACET, tag));
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
/// are news; a window opened is listed in History but is nobody's news;
/// setup (the workspace's own first writes) and window layout stay out of
/// the feed: they are durable, but they are not "what people did".
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
    /// How the feed treats a row of this kind: `None` lists it as news.
    fn feed_tag(self) -> Option<&'static str> {
        match self {
            Self::Change => None,
            Self::View => Some(FEED_HISTORY_ONLY),
            Self::Setup | Self::Layout => Some(FEED_OMIT),
        }
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
