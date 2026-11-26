use super::morning::{MorningCommand, MorningPreferences, MorningSource};
use tilo_desktop_model::WorkspaceId;
use tilo_sort::CompactSort as _;
mod compare;
mod morning_sources;
mod runs;
mod serve;
pub use compare::comparison_field_changes;
use compare::{comparison_change_labels, detail_provenance};
pub use serve::{ServedTree, is_document_write, is_personal, is_read, refused_to_agent};

// Workspace activity — everything that has happened to this workspace, by
// everyone, in one filterable pane.
//
// The workspace's state lives in [`Layout::doc`], a tree-wide
// automerge document holding every workspace on this machine, so
// [`WorkspaceScope`] decides which workspaces' changes are shown — a pane
// titled with one workspace's name must never render another's governance.
// Not shown: the contents of documents open inside the workspace, which the
// broker routes as opaque bytes it never materialises.
//
// A row's `[Revert]` writes the delta back as a **new forward change**;
// history is appended to, never rewritten. Governance rows
// ([`EntryKind::Access`]) are non-revertible — an owner counters a
// governance act with another one rather than rewinding it.

use super::replay::{ReplayPointer, WorkspaceReplayFrame};
use super::{ActivityDeepLink, ActivityHost, ActivityTree, TaskRunUndo, UnavailableActivityHost};

use crate::desktop::i18n::{t, t_args};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use ui::time::{Duration, SystemTime};

use automerge::transaction::Transactable;
use automerge::{ChangeHash, Prop, ReadDoc, ScalarValue, Value};
use collab_types::caps::{Access, CapOverride, Effect, Scope};
use tilo_action_policy::{Grantee, MemberId};
use tilo_ui_base::presence::PresenceCtx;
use ui::widget::EntryKind;
use ui::{widget::HistoryClassification, widget::HistoryEntry, widget::HistoryFilter};
use tilo_ui_base::{widget::ArtifactKind, widget::HistoryEntryId};
use ui::{
    ActivityDigest, ActivityTimeline, AgentSession, AuthorDirectory, BandUndoRequest, ChangeRange,
    DocumentId, GroupNameRequest, HistoryComparison, HistoryFacetIndex, HistorySelectionDetail,
    HistorySource, NamedRange, NarrativeRequest, format_relative_time,
};

use super::index::{
    FeedCoverage, WorkspaceDeparture, WorkspaceHistoryEntry, WorkspaceHistoryId,
    WorkspaceHistoryIndex, WorkspaceHistoryQuery, WorkspaceSeenFrontier,
};

/// How long a device must be away from a workspace before its return is greeted with a recap of
/// what changed.
const AWAY_RECAP_AFTER: Duration = Duration::from_secs(45 * 60);

fn away_recap_is_due(departed_at_secs: u64, now: SystemTime) -> bool {
    let departed_at = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(departed_at_secs))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    now.duration_since(departed_at)
        .is_ok_and(|elapsed| elapsed >= AWAY_RECAP_AFTER)
}

fn seed_roster_colours(authors: &mut AuthorDirectory, tree: &tilo_desktop_model::Layout) {
    let members = tree
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.members.iter());
    for member in members {
        if member.name.trim().is_empty() {
            continue;
        }
        let rgb = collab_types::presence::identity_rgb(member.colour);
        authors.set_color_override(&member.id, rgb);
        authors.register_member(
            &member.id,
            ui::history_source::author_coloured(
                member.name.clone(),
                ui::history_source::short_id(member.id.id()),
                rgb,
            ),
        );
    }
}

/// Facet written onto the rows that are not news: setup and layout rows ([`FEED_OMIT`]) stay out of
/// the feed but in replay; a window opened ([`FEED_HISTORY_ONLY`]) is listed in History but is
/// nobody's news.
const FEED_OMIT_FACET: &str = "tilo.feed";
const FEED_OMIT: &str = "omit";
const FEED_HISTORY_ONLY: &str = "history";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActivityView {
    /// Outcome-first summaries since this device's last completed visit.
    #[default]
    CatchUp,
    /// Full granular history with grouping, filters, rename and revert.
    Explore,
    /// Causal workspace checkpoints played as layout + member changes.
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

/// A durable agent-run marker and the record whose document carries it. `None` is reserved for a
/// marker in the workspace tree itself.
#[derive(Clone, Debug, PartialEq)]
struct IndexedAgentSession {
    source_record: Option<String>,
    session: AgentSession,
}

/// The local user's most recent change that names a resource, for the "you left off" card.
struct ResumeSnapshot {
    target: ui::HistoryResourceTarget,
    author: ui::widget::history::HistoryAuthor,
    app: Option<String>,
    timestamp: SystemTime,
}

fn author_is_local(author: &ui::widget::history::HistoryAuthor, local: &str) -> bool {
    // History already knows the reader's own changes, whatever name the writer gave them.
    if author.relation == ui::widget::history::AuthorRelation::Reader {
        return true;
    }
    if local.is_empty() {
        return false;
    }
    author.name.eq_ignore_ascii_case(local)
        || author.identity_short == ui::history_source::short_id(local)
}

/// The unread rows someone else wrote. Catch up answers "what did others do while I was away";
/// replaying the reader's own writes, including the desktop's first setup, answers nothing.
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

/// Whether History lists `entry` .
fn is_feed_change(entry: &HistoryEntry) -> bool {
    entry
        .context
        .as_ref()
        .is_none_or(ui::tilo_actions::ActionContext::is_activity)
        && feed_tag(entry) != Some(FEED_OMIT)
}

/// Whether `entry` is news to someone who did not make it: listed, and more than a window opened.
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
        .map(tilo_ui_base::format::capitalize_first)
        .collect::<Vec<_>>()
        .join(" ")
}

/// How far back the pane is showing. Maps onto [`HistoryFilter::time_range`].
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

    /// The half-open `[start, now)` window, or `None` for "everything". `now` is passed in, not
    /// read from the clock, so behaviour is testable and every row in one frame is judged against
    /// one instant.
    pub fn range(self, now: SystemTime) -> Option<(SystemTime, SystemTime)> {
        let back = match self {
            Period::AllTime => return None,
            Period::Today => Duration::from_secs(86400),
            Period::Week => Duration::from_secs(604_800),
            Period::Month => Duration::from_secs(2_592_000),
        };
        // The end is nudged past `now` so a change committed during this very frame is inside the
        // window rather than one tick outside it — the interval is half-open at the top.
        let end = now.checked_add(Duration::from_secs(1)).unwrap_or(now);
        Some((now.checked_sub(back).unwrap_or(SystemTime::UNIX_EPOCH), end))
    }
}

/// Which workspaces' changes the pane is showing. A genuine scope decision, not a filter — a row
/// outside it is not this workspace's history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum WorkspaceScope {
    /// Changes touching the active workspace, plus ones belonging to no single workspace — those
    /// can't carry another workspace into this view.
    #[default]
    ThisWorkspace,
    /// Every change in the tree document, each row labelled by the workspace it belongs to.
    AllWorkspaces,
}

impl WorkspaceScope {
    pub const ALL: [WorkspaceScope; 2] =
        [WorkspaceScope::ThisWorkspace, WorkspaceScope::AllWorkspaces];

    pub fn label(self) -> String {
        t(match self {
            WorkspaceScope::ThisWorkspace => "activity-scope-this-workspace",
            WorkspaceScope::AllWorkspaces => "activity-scope-all-workspaces",
        })
    }
}

/// What a dispatched event did.
#[allow(
    clippy::struct_excessive_bools,
    reason = "these independent flags report distinct effects to the overlay host"
)]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WorkspaceActivityOutcome {
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

/// The undo of an agent's run the host is carrying out: which invocation to ask about, the run it
/// takes back and the documents it asked for.
#[derive(Clone, Debug)]
struct PendingUndo {
    invocation: uuid::Uuid,
    run: String,
    steps: Vec<(String, String)>,
}

/// An agent's run as undoing it needs it.
#[derive(Clone, Debug)]
struct UndoRun {
    /// The run's name in each document it changed: the task it worked, or the agent connection's
    /// group, which that document's history undoes the run by.
    named: String,
    /// The run as the activity shows it, which its outcome is reported beside: the recorded run's
    /// own id, or the connection's group.
    shown: String,
    /// The documents it changed: each its app and record.
    steps: Vec<(String, String)>,
}

/// The workspace-activity sheet.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag represents an independently focusable activity-pane mode or asynchronous visual state"
)]
pub struct WorkspaceActivityPane {
    explicit_local_user: Option<String>,
    timeline: ActivityTimeline,
    source: HistorySource,
    /// Set by the timeline's `FnMut` callback, drained right after — it can't reach back into this
    /// struct itself.
    revert_intent: Rc<Cell<Option<HistoryEntryId>>>,
    /// Same handshake as `revert_intent` ; `RefCell` since the payload isn't `Copy` .
    name_intent: Rc<RefCell<Option<GroupNameRequest>>>,
    undo_intent: Rc<RefCell<Option<BandUndoRequest>>>,
    named: Vec<NamedRange>,
    /// Heads at the last rebuild, so a no-op frame skips re-deriving.
    last_heads: Vec<ChangeHash>,
    /// [`Self::history_log`]'s rows revision at the last rebuild: while it holds, the changes since
    /// were all passed over (a workspace switch) and the rows are as derived.
    last_rows: u64,
    /// The heads of the workspace's own document at the last rebuild: what it recorded since is
    /// read even while the layout's own document stands still.
    last_governing: Vec<ChangeHash>,
    /// Every row the document yielded, with the workspace each belongs to (`None` for no single
    /// workspace). [`Self::scope`] selects the subset handed to the timeline.
    derived: Vec<(HistoryEntry, Option<WorkspaceId>)>,
    /// Tree rows after workspace scoping, retained as the layout document's contribution to the
    /// workspace-wide multi-document index.
    scoped_tree_rows: Vec<HistoryEntry>,
    history_index: WorkspaceHistoryIndex,
    detached_records: Vec<super::index::DetachedRecordHistory>,
    /// The last lines of the command runs [`Self::serve_runs`] showed, by the content hash of
    /// their output: recorded output never changes, so a live read need not read it again.
    command_tails: RefCell<HashMap<String, (Vec<String>, bool)>>,
    /// The record documents one read of the runs read, copied once each for that read.
    served_documents: RefCell<HashMap<String, automerge::AutoCommit>>,
    projected_origins: HashMap<HistoryEntryId, WorkspaceHistoryId>,
    feed_coverage: FeedCoverage,
    pub(super) scope: WorkspaceScope,
    /// First-seen order, derived from the rows rather than a roster — a name appears only if that
    /// person did something in this workspace.
    contributors: Vec<String>,
    workspace_name: String,
    total_entries: usize,
    /// Outcome-first reduction of the currently scoped rows. The full timeline remains the source
    /// of detail underneath it.
    digest: ActivityDigest,
    /// Switching workspaces moves no changes, so the heads check alone wouldn't notice — but it
    /// changes which rows are this workspace's.
    last_active: Option<WorkspaceId>,
    now_override: Option<SystemTime>,
    /// Per-workspace private causal frontier. Opening history must not create collaborative
    /// history, and timestamps cannot identify delayed or concurrent CRDT changes correctly.
    seen_frontiers: HashMap<WorkspaceId, WorkspaceSeenFrontier>,
    /// Exact addresses exposed by a painted catch-up page, partitioned before workspace switches.
    /// Never infer visibility from the index's heads.
    viewed_changes: HashMap<WorkspaceId, BTreeSet<WorkspaceHistoryId>>,
    /// Private causal baseline captured when this device leaves a workspace. Wall time checks the
    /// presentation; the frontier decides which changes actually happened during the absence.
    departures: HashMap<WorkspaceId, WorkspaceDeparture>,
    away_digest: ActivityDigest,
    /// The workspace whose recap is on screen: what the view opened on after long enough away.
    pub(super) away_workspace: Option<WorkspaceId>,
    catchup_frontier: WorkspaceSeenFrontier,
    passive_digest: ActivityDigest,
    passive_unread: usize,
    passive_rows: Vec<HistoryEntry>,
    /// When the person here joined the workspace in front, where it recorded that.
    joined: Option<SystemTime>,
    persistent_frontiers: bool,
    view: ActivityView,
    morning_sources: HashMap<WorkspaceId, MorningPreferences>,
    morning_workspace: WorkspaceId,
    morning_command_message: Option<String>,
    morning_source_offset: usize,
    expanded_brief: Option<usize>,
    host: Rc<dyn ActivityHost>,
    /// The active workspace's durable identity, hex.
    workspace: String,
    /// The undo of an agent's run the host is carrying out.
    undo_pending: Option<PendingUndo>,
    facet_index: HistoryFacetIndex,
    comparison: Option<HistoryComparison>,
    comparison_attempted: bool,
    comparison_selection: Vec<HistoryEntryId>,
    pub(super) replay_unavailable: usize,
    replay_presentation_note: Option<String>,
    pub(super) replay_frames: Vec<WorkspaceReplayFrame>,
    /// What [`Self::source`] has read of the workspace's change log, kept across refreshes: a
    /// refresh reads only the changes appended since the last one
    /// ([`HistorySource::entries_since`]).
    pub(super) history_log: ui::history_source::HistoryLog,
    /// What [`Self::source`] has read of the workspace's own document, kept across refreshes: the
    /// rows of its governance come from there ([`ActivityTree::governance_document`]).
    governance_log: ui::history_source::HistoryLog,
    /// The rows read from the workspace's own document: they address changes in it, not in the
    /// document the layout is read from, so nothing here diffs or reverts them.
    governance_rows: HashSet<HistoryEntryId>,
    /// Each workspace change's reading, kept across refreshes; see [`describe_workspace_changes`].
    workspace_changes: HashMap<ChangeHash, WorkspaceChange>,
    /// Each replayed entry's desktop, kept across rebuilds; see [`Self::rebuild_replay_frames`].
    pub(super) replay_checkpoints: HashMap<HistoryEntryId, super::replay::ReplayCheckpoint>,
    pub(super) replay_index: usize,
    pub(super) replay_playing: bool,
    narrative: Option<ui::GroundedNarrative>,
    narrative_request: Option<NarrativeRequest>,
    /// Task receipts live beside the records they describe. They are indexed here so selecting any
    /// affected change can explain the whole task.
    agent_sessions: Vec<IndexedAgentSession>,
    agent_undo_notice: Option<(String, String)>,
    /// Records the last task undo did not reach, as sentences naming each: reported beside the run,
    /// never left out of it.
    unreverted: Vec<String>,
    open_agent_records: HashSet<String>,
    /// What a deep link last asked the feed to show, and how many it asked.
    focus: Option<tilo_actions::remote::workspace_activity::Focus>,
    focus_serial: u64,
}

impl Default for WorkspaceActivityPane {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceActivityPane {
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
                // The pane owns the Everyone/person selector directly above the timeline. Keep the
                // operation chips, but never repeat an `author:any` row inside the embedded widget.
                t.set_show_author_pill(false);
                t.set_show_day_headings(true);
                t
            },
            // A workspace is a cluster of windows saved/reopened collaboratively: what its layout
            // record holds is one row per change of the layout.
            source: HistorySource::new().with_artifact_kind(ArtifactKind::Cluster),
            revert_intent,
            name_intent,
            undo_intent,
            named: Vec::new(),
            last_heads: Vec::new(),
            last_rows: 0,
            last_governing: Vec::new(),
            derived: Vec::new(),
            scoped_tree_rows: Vec::new(),
            history_index: WorkspaceHistoryIndex::new(),
            detached_records: Vec::new(),
            command_tails: RefCell::default(),
            served_documents: RefCell::default(),
            projected_origins: HashMap::new(),
            feed_coverage: FeedCoverage::default(),
            scope: WorkspaceScope::default(),
            contributors: Vec::new(),
            workspace_name: String::new(),
            total_entries: 0,
            digest: ActivityDigest::default(),
            last_active: None,
            now_override: None,
            seen_frontiers: HashMap::new(),
            viewed_changes: HashMap::new(),
            departures: HashMap::new(),
            away_digest: ActivityDigest::default(),
            away_workspace: None,
            catchup_frontier: WorkspaceSeenFrontier::default(),
            passive_digest: ActivityDigest::default(),
            passive_unread: 0,
            passive_rows: Vec::new(),
            joined: None,
            persistent_frontiers: false,
            view: ActivityView::default(),
            morning_sources: HashMap::new(),
            morning_workspace: WorkspaceId::nil(),
            morning_command_message: None,
            morning_source_offset: 0,
            expanded_brief: None,
            host: Rc::new(UnavailableActivityHost),
            workspace: String::new(),
            undo_pending: None,
            facet_index: HistoryFacetIndex::default(),
            comparison: None,
            comparison_attempted: false,
            comparison_selection: Vec::new(),
            replay_unavailable: 0,
            replay_presentation_note: None,
            replay_frames: Vec::new(),
            history_log: ui::history_source::HistoryLog::default(),
            governance_log: ui::history_source::HistoryLog::default(),
            governance_rows: HashSet::new(),
            workspace_changes: HashMap::new(),
            replay_checkpoints: HashMap::new(),
            replay_index: 0,
            replay_playing: false,
            narrative: None,
            narrative_request: None,
            agent_sessions: Vec::new(),
            agent_undo_notice: None,
            unreverted: Vec::new(),
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

    /// Brief positions change when fresh work arrives. Retain the expanded work session so it
    /// cannot drift onto a different result.
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

    /// Snapshot the previous private frontier before the catch-up surface is shown. It advances
    /// only when the surface closes, so opening it cannot erase the very summary the user came to
    /// read. A return after long enough away starts from the frontier this device left, so the
    /// recap is exactly what others did in the meantime.
    pub fn begin_view(&mut self, workspace: WorkspaceId) {
        self.viewed_changes.remove(&workspace);
        if self.scope != WorkspaceScope::ThisWorkspace {
            self.scope = WorkspaceScope::ThisWorkspace;
            self.invalidate();
        }
        self.away_workspace = None;
        if self.away_digest.total_changes > 0
            && self.last_active == Some(workspace)
            && let Some(departure) = self.departures.get(&workspace)
        {
            self.catchup_frontier = departure.frontier.clone();
            self.digest = self.away_digest.clone();
            self.away_workspace = Some(workspace);
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

    pub fn finish_view(&mut self, workspace: WorkspaceId) {
        self.replay_playing = false;
        self.away_workspace = None;
        let viewed = self.viewed_changes.remove(&workspace).unwrap_or_default();
        let frontier = self.seen_frontiers.entry(workspace).or_default();
        for id in &viewed {
            frontier.acknowledge(id);
        }
        let frontier = frontier.clone();
        // Looking ends here: the next visit counts what others did since this frontier as a recap
        // once enough time has passed.
        self.departures.insert(
            workspace,
            WorkspaceDeparture {
                departed_at_secs: self
                    .now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                frontier: frontier.clone(),
            },
        );
        if self.persistent_frontiers {
            self.persist_frontiers();
        }
        if self.last_active != Some(workspace) {
            return;
        }
        self.catchup_frontier = frontier;
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
                    origin: super::index::HistoryOrigin::Layout,
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

    /// What "new" is counted from in `workspace` . One this device has never read starts from when
    /// the person joined it: what others did since is news, and the workspace's past stays
    /// browsable. One whose joining it did not keep starts from the history it holds now.
    fn passive_frontier(&mut self, workspace: WorkspaceId) -> WorkspaceSeenFrontier {
        if self.persistent_frontiers
            && !self.seen_frontiers.contains_key(&workspace)
            && !self.history_index.entries().is_empty()
        {
            let mut frontier = WorkspaceSeenFrontier::default();
            match self.joined {
                Some(joined) => self
                    .history_index
                    .entries()
                    .iter()
                    .filter(|row| row.entry.timestamp < joined)
                    .for_each(|row| frontier.acknowledge(&row.id)),
                None => frontier.mark_seen(&self.history_index),
            }
            self.seen_frontiers.insert(workspace, frontier);
            self.persist_frontiers();
        }
        self.seen_frontiers
            .get(&workspace)
            .cloned()
            .unwrap_or_default()
    }

    /// The tree-only feed's rows the active workspace's frontier has not acknowledged, with the
    /// same first-sight baseline as [`Self::passive_frontier`].
    fn unseen_tree_rows(&mut self, rows: &[HistoryEntry]) -> Vec<HistoryEntry> {
        let Some(workspace) = self.last_active else {
            return rows.to_vec();
        };
        let id = |entry: &HistoryEntry| WorkspaceHistoryId {
            origin: super::index::HistoryOrigin::Layout,
            change: entry.id,
        };
        if self.persistent_frontiers
            && !self.seen_frontiers.contains_key(&workspace)
            && !rows.is_empty()
        {
            let joined = self.joined;
            let frontier = self.seen_frontiers.entry(workspace).or_default();
            for row in rows
                .iter()
                .filter(|row| joined.is_none_or(|joined| row.timestamp < joined))
            {
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

    /// Whether every selected change can be written back here: its history is one the host lets the
    /// model change.
    fn selection_is_writable(&self, detail: &HistorySelectionDetail) -> bool {
        detail.entry_ids.iter().all(|id| {
            let origin = self
                .projected_origins
                .get(id)
                .map_or(super::index::HistoryOrigin::Layout, |address| {
                    address.origin.clone()
                });
            self.host.writable(&origin)
        })
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
                .map_or(super::index::HistoryOrigin::Layout, |address| {
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
        // A row read from the workspace's own document addresses a change the layout's document does
        // not hold: there is nothing here to compare it with or to take back.
        if detail
            .entry_ids
            .iter()
            .any(|id| self.governance_rows.contains(id))
        {
            detail.can_compare = false;
            detail.can_undo = false;
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
                    // The record's title as its own actions named it, else its app: the record id
                    // is an address, not a name.
                    let label = titles
                        .iter()
                        .find(|target| target.id == *id)
                        .and_then(|target| tilo_ui_base::human_label(&target.label, &target.id))
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
        }
        Some(detail)
    }

    fn selected_agent_session(&self) -> Option<&IndexedAgentSession> {
        let selected = self.selected_ids();
        self.agent_sessions.iter().find(|indexed| {
            indexed.session.steps.iter().any(|step| {
                selected
                    .iter()
                    .any(|id| self.agent_step_contains(indexed, step, id))
            })
        })
    }

    fn agent_step_contains(
        &self,
        indexed: &IndexedAgentSession,
        step: &ui::InvocationStep,
        selected: &HistoryEntryId,
    ) -> bool {
        let projected = self.projected_origins.get(selected);
        let origin = projected.map(|item| &item.origin);
        // Timeline ids are collision-safe projections across every open record. Agent receipts
        // address the underlying document change, so compare and order through the origin index
        // rather than mixing those two namespaces.
        let selected_change = projected.map_or(*selected, |item| item.change);
        let origin_matches = match origin {
            Some(origin) => origin.record().map_or_else(
                || indexed.source_record.is_none(),
                |id| step.record.is_empty() || id == step.record,
            ),
            None => indexed.source_record.is_none(),
        };
        if !origin_matches {
            return false;
        }
        if selected_change == step.range.start || selected_change == step.range.end {
            return true;
        }

        // History IDs are hashes, not sortable sequence numbers. Determine membership from this
        // origin's causal timeline order instead.
        let wanted = origin.and_then(super::index::HistoryOrigin::record);
        let ids = self
            .history_index
            .entries()
            .iter()
            .filter(|entry| entry.id.origin.record() == wanted)
            .map(|entry| entry.id.change)
            .collect::<Vec<_>>();
        let Some(at) = ids.iter().position(|id| *id == selected_change) else {
            return false;
        };
        let (Some(start), Some(end)) = (
            ids.iter().position(|id| *id == step.range.start),
            ids.iter().position(|id| *id == step.range.end),
        ) else {
            return false;
        };
        (start.min(end)..=start.max(end)).contains(&at)
    }

    fn agent_name(&self, run: &AgentSession) -> String {
        run.by.as_deref().map_or_else(
            || t("activity-agent-fallback"),
            |actor| self.source.authors().resolve(actor).name,
        )
    }

    fn issuer_name(&self, run: &AgentSession) -> String {
        run.issued_by.as_deref().map_or_else(
            || t("activity-unknown-member"),
            |actor| self.source.authors().resolve(actor).name,
        )
    }

    /// The run recorded for the agent's work on task `task_id` , if it changed anything.
    fn task_agent_session(&self, task_id: &str) -> Option<&IndexedAgentSession> {
        self.agent_sessions
            .iter()
            .find(|indexed| indexed.session.task.as_deref() == Some(task_id))
    }

    /// Ask the host to undo the agent's whole run of `task_id` , through each changed document's
    /// own history. Returns whether it was asked.
    fn undo_task_run(&mut self, task_id: &str) -> bool {
        self.undo_task_run_of(task_id, None, None)
    }

    /// Ask the host to undo the agent's run of `task_id` in the one record `only` names (its app
    /// and record), or in every record the run changed when it names none; `step` names the one
    /// change of that record to take back, or takes back the run's whole share of it. Returns
    /// whether it was asked.
    fn undo_task_run_of(
        &mut self,
        task_id: &str,
        only: Option<&(String, String)>,
        step: Option<&str>,
    ) -> bool {
        let Some(indexed) = self.task_agent_session(task_id) else {
            return false;
        };
        let run = UndoRun {
            named: task_id.to_owned(),
            shown: indexed.session.id.clone(),
            steps: indexed
                .session
                .steps
                .iter()
                .map(|step| (step.app.clone(), step.record.clone()))
                .collect(),
        };
        self.undo_run_of(&run, only, step)
    }

    /// Undo the agent's run of task `task_id` in one changed record, from the run's step list: the
    /// document it name is not otherwise touched. `step` names the one change to take back there.
    fn undo_task_record(
        &mut self,
        task_id: &str,
        app: &str,
        record: &str,
        step: Option<&str>,
    ) -> bool {
        let only = (app.to_owned(), record.to_owned());
        self.undo_task_run_of(task_id, Some(&only), step)
    }

    /// Ask the host to undo an agent connection's whole run, through each changed document's own
    /// history. Returns whether it was asked.
    fn undo_group_run(&mut self, group: &UndoRun) -> bool {
        self.undo_run_of(group, None, None)
    }

    /// Ask the host to undo one record of an agent connection's run, the record `only` names (its
    /// app and record), or the one change `step` names there. Returns whether it was asked.
    fn undo_group_record(
        &mut self,
        group: &UndoRun,
        app: &str,
        record: &str,
        step: Option<&str>,
    ) -> bool {
        let only = (app.to_owned(), record.to_owned());
        self.undo_run_of(group, Some(&only), step)
    }

    /// Ask the host to undo the run in the documents `steps` name, or in the one of them `only`
    /// names: each document's own history takes back what it grouped under the run, so later edits
    /// by others stay. `step` names the one change to take back, rather than the run's whole share
    /// of that document. Returns whether it was asked.
    fn undo_run_of(
        &mut self,
        run: &UndoRun,
        only: Option<&(String, String)>,
        step: Option<&str>,
    ) -> bool {
        if self.undo_pending.is_some() {
            return true;
        }
        let steps: Vec<(String, String)> = run
            .steps
            .iter()
            .filter(|step| only.is_none_or(|one| one == *step))
            .cloned()
            .collect();
        if steps.is_empty() {
            return false;
        }
        let invocation = uuid::Uuid::new_v4();
        let undo = TaskRunUndo {
            workspace: self.workspace.clone(),
            invocation,
            task_id: run.named.clone(),
            steps: steps.clone(),
            step: step.map(str::to_owned),
        };
        match self.host.undo_task_run(undo) {
            Ok(()) => {
                self.undo_pending = Some(PendingUndo {
                    invocation,
                    run: run.shown.clone(),
                    steps,
                });
                // Quiet until it lands.
                self.agent_undo_notice = None;
                true
            }
            Err(error) => {
                self.agent_undo_notice = Some((
                    run.shown.clone(),
                    t_args("activity-undo-task-refused", &[("detail", error.into())]),
                ));
                true
            }
        }
    }

    /// Tell how the host's undo of an agent's run ended, once it has.
    fn poll_undo(&mut self) {
        let Some(pending) = self.undo_pending.clone() else {
            return;
        };
        let Some(result) = self.host.poll_task_run_undo(pending.invocation) else {
            return;
        };
        self.undo_pending = None;
        self.unreverted = Vec::new();
        let notice = match result {
            Ok(documents) => {
                let asked: Vec<(String, String)> = documents
                    .iter()
                    .map(|document| (document.app.clone(), document.record.clone()))
                    .collect();
                // Every record of the run the host did not ask a document about is one undo did not
                // reach: it is named, never quietly left behind.
                self.unreverted = pending
                    .steps
                    .iter()
                    .filter(|step| !asked.contains(step))
                    .map(|(app, record)| {
                        let record = DocumentId::parse(record)
                            .and_then(|id| id.file_path())
                            .and_then(|path| {
                                path.file_name()
                                    .map(|name| name.to_string_lossy().into_owned())
                            })
                            .unwrap_or_else(|| record.clone());
                        t_args(
                            "activity-undo-task-record-left",
                            &[("record", record.into()), ("app", app.clone().into())],
                        )
                    })
                    .collect();
                if self.unreverted.is_empty() {
                    t("activity-undo-task-done")
                } else {
                    t("activity-undo-task-partial-records")
                }
            }
            Err(error) => t_args("activity-undo-task-refused", &[("detail", error.into())]),
        };
        self.agent_undo_notice = Some((pending.run, notice));
    }

    fn can_undo_agent_run(&self) -> bool {
        if !self.host.mutations_available() {
            return false;
        }
        if let Some(indexed) = self.selected_agent_session() {
            // A run is undone in the documents it changed, which the host does
            // for the task that recorded it; a run no task names has no one to
            // carry that out here.
            return indexed.session.task.is_some()
                && indexed
                    .session
                    .steps
                    .iter()
                    .all(|step| self.record_is_open(step));
        }
        // An agent connection's run is undone by its group, which its documents' history takes
        // back the same way a task's.
        self.selected_group_session()
            .is_some_and(|group| self.undo_blocked(&group.steps).is_none())
    }

    /// Why the documents `steps` (each its app and record) cannot be undone whole from here, if
    /// they cannot: an undo at a time, and only where this device holds each document open.
    fn undo_blocked(&self, steps: &[(String, String)]) -> Option<String> {
        if !self.host.mutations_available() {
            return Some(t("activity-served-read-only"));
        }
        if self.undo_pending.is_some() {
            return Some(t("activity-undo-run-in-progress"));
        }
        if steps.is_empty() {
            return Some(t("activity-undo-run-nothing-to-undo"));
        }
        (!steps
            .iter()
            .all(|(_, record)| self.record_id_is_open(record)))
        .then(|| t("activity-undo-run-needs-open"))
    }

    /// Whether the document behind a run's step is open here, which undoing just that step needs.
    fn record_is_open(&self, step: &ui::InvocationStep) -> bool {
        self.record_id_is_open(&step.record)
    }

    /// Whether the record `record` is open here, which undoing its share of a run needs: whether
    /// a document this host admitted is that record, named as the history names it — a record's
    /// own id, or a package's record of the workspace, which travels as its primary.
    fn record_id_is_open(&self, record: &str) -> bool {
        self.open_agent_records.contains(record)
    }

    /// Whether one record of the selected run can be undone on its own from the task view.
    fn can_undo_agent_record(&self, step: &ui::InvocationStep) -> bool {
        self.host.mutations_available() && self.undo_pending.is_none() && self.record_is_open(step)
    }

    /// The timeline's changes the selection covers, oldest first.
    fn selected_entries(&self) -> &[HistoryEntry] {
        self.timeline
            .selection()
            .and_then(|(start, end)| self.timeline.entries().get(start..=end))
            .unwrap_or_default()
    }

    fn selected_ids(&self) -> Vec<HistoryEntryId> {
        self.selected_entries()
            .iter()
            .map(|entry| entry.id)
            .collect()
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
        let document_id = range.id.clone();
        let label = range.label.clone();
        let right_label = format!("{label} — continued");
        if ui::history_groups::split_named_range(
            tree.document_mut(),
            &document_id,
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

    /// Materialize the exact before/after document for the current selection. Mixed-document
    /// selections deliberately have no synthetic diff: callers can split them by provenance and
    /// compare each resource honestly.
    pub fn compare_selection(&self, tree: &mut impl ActivityTree) -> Option<HistoryComparison> {
        let (start, end) = self.timeline.selection()?;
        let selected = self.timeline.entries().get(start..=end)?;
        let addresses: Vec<WorkspaceHistoryId> = selected
            .iter()
            .map(|entry| {
                self.projected_origins
                    .get(&entry.id)
                    .cloned()
                    .unwrap_or(WorkspaceHistoryId {
                        origin: super::index::HistoryOrigin::Layout,
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
            super::index::HistoryOrigin::Layout => {
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
        }
    }

    /// What a read narrowed to `period`, `author` and `lens` lists: each reader's own, so two readers
    /// asking differently never narrow each other's.
    fn filter_for(
        &self,
        period: Period,
        author: Option<&str>,
        lens: &LensSelection,
    ) -> HistoryFilter {
        let mut filter = HistoryFilter {
            time_range: period.range(self.now()),
            ..HistoryFilter::default()
        };
        filter.authors.extend(author.map(str::to_owned));
        match lens {
            LensSelection::All => {}
            LensSelection::AgentTasks => {
                filter
                    .work_groups
                    .extend(self.agent_sessions.iter().map(|indexed| {
                        indexed
                            .session
                            .instruction
                            .as_ref()
                            .unwrap_or(&indexed.session.label)
                            .clone()
                    }));
                // A task's run groups its changes under the task itself.
                filter.work_groups.extend(
                    self.agent_sessions
                        .iter()
                        .filter_map(|indexed| indexed.session.task.clone()),
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
        filter
    }

    /// Re-derive the timeline from the workspace document if the graph has moved; called every
    /// frame, the heads check keeps that cheap.
    #[allow(
        clippy::too_many_lines,
        reason = "timeline refresh keeps document observation, filter projection, and retained activity rows in one ordered read-only projection pass"
    )]
    pub fn refresh(&mut self, tree: &mut impl ActivityTree) {
        // History reads its rows in the desktop's words: loading them later would change the
        // reading's basis and read the log again.
        crate::desktop::i18n::ensure();
        self.replay_presentation_note = tree.replay_presentation_note();

        let name = tree
            .active_workspace()
            .map(|w| w.name.clone())
            .unwrap_or_default();
        if name != self.workspace_name {
            self.workspace_name = name;
        }
        let local = self.local_user();
        self.joined = tree
            .active_workspace()
            .and_then(|workspace| workspace.policy.joined.get(&local))
            .and_then(|ms| {
                SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_millis(*ms))
            });
        let workspace = tree
            .active_workspace()
            .map(|workspace| workspace.id().to_string())
            .unwrap_or_default();
        if self.workspace != workspace || self.morning_workspace != tree.active {
            self.morning_command_message = None;
            self.morning_source_offset = 0;
        }
        self.workspace = workspace;
        self.morning_workspace = tree.active;
        self.poll_undo();

        let heads = tree.document_mut().get_heads();
        let governing = Self::governing_heads(tree);
        let active = tree.active;
        // A switch writes the document but no row: read just the changes since and, when all of
        // them were passed over, only show the rows of the workspace now active.
        let only_passed_over = heads != self.last_heads
            && governing == self.last_governing
            && !self.last_heads.is_empty()
            && {
                self.source
                    .follow(tree.document_mut(), &mut self.history_log);
                self.history_log.rows_revision() == self.last_rows
            };
        let stayed = heads == self.last_heads && governing == self.last_governing;
        if only_passed_over || (stayed && !self.timeline.entries().is_empty()) {
            self.last_heads = heads;
            self.last_governing = governing;
            if self.last_active != Some(active) {
                self.apply_scope(tree);
            }
            return;
        }
        self.last_heads = heads;
        self.last_governing = governing;
        seed_roster_colours(self.source.authors_mut(), tree);
        // A change an agent's own key signed reads as the agent's, for its person, once the host
        // verified its delegation for that device.
        for agent in tree.roster_agents() {
            self.source.authors_mut().register_roster_agent(&agent);
        }
        let local = self.local_user();
        if !local.is_empty() {
            // The reader's own changes read as theirs.
            self.source
                .authors_mut()
                .set_reader(tilo_ui_base::permissions::MemberId::person(local.clone()));
        }
        if !local.is_empty() && self.explicit_local_user.is_none() {
            let handle = automerge::ActorId::from(local.as_bytes()).to_hex_string();
            self.host.local_author(self.source.authors_mut(), &handle);
        }

        // Bind the local automerge actor to the Tilo identity, or local user id; a peer's actor
        // stays unresolved until collaboration says who. The workspace's own document is written by
        // this device too, and its rows read as the same person's.
        self.register_writer(&tree.document_mut().get_actor().to_hex_string());
        if let Some(document) = tree.governance_document() {
            self.register_writer(&document.get_actor().to_hex_string());
        }

        let mut entries = self
            .source
            .entries_since(tree.document_mut(), &mut self.history_log);
        self.last_rows = self.history_log.rows_revision();
        let reader = tree.governance_reader();
        let mut tags = describe_workspace_changes(
            tree.document_mut(),
            &self.history_log,
            &mut entries,
            &mut self.workspace_changes,
            &Readers {
                governance: reader.as_deref(),
                authors: self.source.authors(),
            },
        );
        // The document holding one workspace's layout is that workspace's: every change of it is
        // the workspace's, whichever part of the record it wrote.
        if let Some(own) = tree.document_workspace() {
            tags.fill(Some(own));
        }
        // What the workspace's governance says — who owns it, who it is shared with, what it
        // grants — is written in the workspace's own document, which the layout is one record of.
        self.read_governance(tree, &mut entries, &mut tags);
        // A receipt is committed by the workspace host, but its authenticated `actor` is the person
        // (or agent) who performed the semantic action. Attribute the projected CRDT row to that
        // actor rather than to the host's Automerge key; otherwise every collaborative work session
        // misleadingly appears to have been done by the same machine.
        let receipt_actors = tree
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.effect_receipts.iter())
            .map(|receipt| {
                (
                    receipt.invocation.0.to_string(),
                    hex::encode(receipt.actor.actor_bytes()),
                )
            })
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
            entry.author = self.source.authors().resolve(actor);
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
                entries.push(HistoryEntry {
                    id: HistoryEntryId::from_bytes(*receipt.invocation.0.as_bytes()),
                    author: self
                        .source
                        .authors()
                        .resolve(&hex::encode(receipt.actor.actor_bytes())),
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
                    target: None,
                });
                tags.push(Some(workspace.id()));
            }
        }
        // Housekeeping and what came from outside stay in the workspace document's history, out of
        // the feed.
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
        // One window over the workspace's history, whichever of its documents the rows came from.
        let excess = self
            .derived
            .len()
            .saturating_sub(ui::history_source::DEFAULT_MAX_ENTRIES);
        if excess > 0 {
            self.derived.drain(..excess);
        }
        // Names come out of the same document as the changes, so a peer's name arrives with their
        // edits.
        self.named = ui::history_groups::read_named_ranges(tree.document(), self.source.authors());
        self.apply_scope(tree);
    }

    /// Register the actor `actor` as whoever writes here, where it is not known yet.
    fn register_writer(&mut self, actor: &str) {
        if self.source.authors().is_registered(actor) {
            return;
        }
        let member = if let Some(local) = &self.explicit_local_user {
            Some(local.clone())
        } else {
            self.host.local_author(self.source.authors_mut(), actor)
        };
        // The hue follows the person, as receipt rows' does: one person editing from this device
        // and acting through a receipt is one colour, not one per key.
        if let Some(member) = member {
            let rgb = self.source.authors().colour_for(&member);
            self.source.authors_mut().register(
                actor,
                ui::history_source::author_coloured(
                    member,
                    ui::history_source::short_id(actor),
                    rgb,
                ),
            );
        }
    }

    /// The heads of the workspace's own document, where one is read.
    fn governing_heads(tree: &impl ActivityTree) -> Vec<ChangeHash> {
        tree.governance_document()
            .map(automerge::AutoCommit::committed_heads)
            .unwrap_or_default()
    }

    /// Append what the workspace's own document says of its governance — who owns it, who it is
    /// shared with, what it grants — to `entries`, each row tagged with its workspace.
    fn read_governance(
        &mut self,
        tree: &impl ActivityTree,
        entries: &mut Vec<HistoryEntry>,
        tags: &mut Vec<Option<WorkspaceId>>,
    ) {
        let Some(document) = tree.governance_document().cloned() else {
            self.governance_rows.clear();
            return;
        };
        let mut document = document;
        let mut rows = self
            .source
            .entries_since(&mut document, &mut self.governance_log);
        let reader = tree.governance_reader();
        let row_tags = describe_workspace_changes(
            &mut document,
            &self.governance_log,
            &mut rows,
            &mut self.workspace_changes,
            &Readers {
                governance: reader.as_deref(),
                authors: self.source.authors(),
            },
        );
        self.governance_rows = rows.iter().map(|row| row.id).collect();
        let workspace = Some(tree.active);
        tags.extend(row_tags.into_iter().map(|tag| tag.or(workspace)));
        entries.append(&mut rows);
    }

    /// Hand the timeline the rows [`Self::scope`] selects and re-derive everything that depends on
    /// which rows are in play. Every entry of the workspace's derived history, in any workspace.
    pub(super) fn derived_entry_ids(&self) -> impl Iterator<Item = HistoryEntryId> + '_ {
        self.derived.iter().map(|(entry, _)| entry.id)
    }

    fn apply_scope(&mut self, tree: &impl ActivityTree) {
        let active = tree.active;
        self.last_active = Some(active);
        let names: std::collections::HashMap<WorkspaceId, String> = tree
            .workspaces
            .iter()
            .map(|w| (w.id(), w.name.clone()))
            .collect();

        let mut rows: Vec<HistoryEntry> = Vec::with_capacity(self.derived.len());
        for (entry, tag) in &self.derived {
            match (self.scope, tag) {
                (WorkspaceScope::ThisWorkspace, Some(id)) if *id != active => {}
                (WorkspaceScope::ThisWorkspace, _) => rows.push(entry.clone()),
                (WorkspaceScope::AllWorkspaces, tag) => {
                    let mut e = entry.clone();
                    // Otherwise three identical "Set the workspace owner" lines are noise.
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

    /// Refresh one coherent admission of detached, app-qualified record owners.
    pub fn refresh_detached_records(
        &mut self,
        tree: &mut impl ActivityTree,
        mut records: Vec<super::index::DetachedRecordHistory>,
    ) {
        self.history_index
            .set_authors(self.source.authors().clone());
        let query = tree
            .active_workspace()
            .map(|w| WorkspaceHistoryQuery::for_room(w.id()))
            .unwrap_or_default();
        let changed =
            self.history_index
                .refresh_detached(tree, query, &self.scoped_tree_rows, &mut records);
        self.detached_records = records;
        self.feed_coverage = self.history_index.coverage().clone();
        self.refresh_detached_agent_sessions(tree);
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
        let passive = self.passive_frontier(tree.active);
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
        let away = self
            .departures
            .get(&tree.active)
            .cloned()
            .filter(|departure| away_recap_is_due(departure.departed_at_secs, self.now()));
        let away_rows = away.map_or_else(Vec::new, |departure| {
            others_work(
                self.history_index.unseen_detached(
                    &departure.frontier,
                    tree,
                    &self.detached_records,
                ),
                &local,
            )
        });
        self.away_digest = ActivityDigest::from_entries_with_ranges(
            &away_rows,
            SystemTime::UNIX_EPOCH,
            5,
            &self.named,
        );
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
    /// The agent runs of every detached record the host admitted, and of the workspace tree itself,
    /// each naming the record it came from.
    fn refresh_detached_agent_sessions(&mut self, tree: &impl ActivityTree) {
        let mut ordered = self.detached_records.iter().collect::<Vec<_>>();
        ordered.compact_sort_by_key(|record| record.origin());
        self.open_agent_records = ordered
            .iter()
            .filter(|record| record.document.is_some())
            .map(|record| record.id.clone())
            .collect();
        let mut indexed = ui::read_invocations(tree.document())
            .into_iter()
            .map(|run| IndexedAgentSession {
                source_record: None,
                session: run,
            })
            .collect::<Vec<_>>();
        for record in ordered {
            let id = &record.id;
            let Some(document) = &record.document else {
                continue;
            };
            indexed.extend(ui::read_invocations(document).into_iter().map(|run| {
                IndexedAgentSession {
                    source_record: Some(id.clone()),
                    session: run,
                }
            }));
        }
        indexed.compact_sort_by(|a, b| {
            b.session
                .at
                .cmp(&a.session.at)
                .then_with(|| a.session.id.cmp(&b.session.id))
        });
        indexed.dedup_by(|a, b| a.source_record == b.source_record && a.session.id == b.session.id);
        self.agent_sessions = indexed;
    }

    /// A host bounded the retained desktop document's history projection. Call after refreshing
    /// records, which rebuilds the other coverage counts.
    pub fn mark_desktop_history_capped(&mut self) {
        self.feed_coverage.capped_records = self.feed_coverage.capped_records.saturating_add(1);
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
                        origin: super::index::HistoryOrigin::Layout,
                        change: entry.id,
                    })
            })
            .cloned()
            .collect();
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

        // Filters and counts name the people history resolved; a change it could not name still
        // shows under Everyone.
        let mut seen: HashSet<String> = HashSet::new();
        self.contributors = rows
            .iter()
            .filter(|e| e.author.name != ui::history_source::UNKNOWN_AUTHOR_NAME)
            .filter(|e| seen.insert(e.author.name.clone()))
            .map(|e| e.author.name.clone())
            .collect();
        self.timeline.set_entries(rows);
        if self.selected_ids() != prior_selection {
            self.clear_comparison();
        }
        self.timeline.set_named_ranges(self.named.clone());
    }

    fn reset_narrative(&mut self) {
        let request = NarrativeRequest::from_digest(&self.digest);
        if self.narrative_request == request {
            return;
        }
        // The deterministic summary only restates the brief titles below it, so the card waits for
        // prose that adds something.
        self.narrative = None;
        self.narrative_request = request;
    }

    pub fn set_scope(&mut self, scope: WorkspaceScope, tree: &impl ActivityTree) {
        if scope == WorkspaceScope::AllWorkspaces && !self.host.all_workspaces_available() {
            return;
        }
        if self.scope != scope {
            self.scope = scope;
            self.apply_scope(tree);
        }
    }

    pub fn authors_mut(&mut self) -> &mut AuthorDirectory {
        self.source.authors_mut()
    }

    /// Presence is intentionally sampled only at durable action boundaries: pointer motion remains
    /// ephemeral and cheap, while the latest exact positions survive subsequent checkpoint rebuilds
    /// for this session.
    pub fn observe_replay_presence(&mut self, presence: &PresenceCtx) {
        let Some(frame) = self.replay_frames.last_mut() else {
            return;
        };
        let mut pointers = Vec::new();
        for (key, people) in &presence.pointers {
            let desktop_space = key.scopes_a_shell_pointer();
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
        if !self.agent_sessions.is_empty() {
            rows.push((
                LensSelection::AgentTasks,
                t_args(
                    "activity-lens-agent-tasks",
                    &[("count", self.agent_sessions.len().into())],
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
    ) -> WorkspaceActivityOutcome {
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
                self.comparison = self.compare_selection(tree);
                self.comparison_selection = self.selected_ids();
                // Reading an `AutoCommit` comparison closes any pending transaction. That changes
                // the observed heads without changing content; keep refresh from treating the next
                // paint as an external edit and discarding the result.
                self.last_heads = tree.document_mut().get_heads();
                outcome.compared = self.comparison.is_some();
            }
            DetailAction::Restore => {
                if let Some(id) = self.selected_ids().last().copied()
                    && self.revert_to(tree, &id)
                {
                    outcome.reverted = true;
                }
            }
            DetailAction::Undo => {
                if let Some(request) = self.selection_undo_request() {
                    let applied = self.undo_band(tree, &request);
                    outcome.reverted = applied.wrote;
                    outcome.undo_partial = applied.partial;
                }
            }
            DetailAction::UndoTask => {
                let applied = self.undo_selected_agent_session();
                outcome.reverted = applied.wrote;
                outcome.undo_partial = applied.partial;
            }
            DetailAction::Split => outcome.named = self.split_selection_group(tree),
            DetailAction::Merge => outcome.named = self.merge_selection_groups(tree),
        }
        outcome
    }

    fn undo_selected_agent_session(&mut self) -> UndoApply {
        if !self.host.mutations_available() {
            return UndoApply::default();
        }
        if let Some(indexed) = self.selected_agent_session().cloned() {
            // An agent's run of a task is undone by each document's own history, which also takes
            // back what a change queued outside the document (a file renamed on disk); reverting
            // the document alone would leave that behind.
            if let Some(task) = &indexed.session.task {
                self.undo_task_run(task);
            }
            return UndoApply::default();
        }
        // An agent connection's run is undone by its group, which each document's history takes
        // back the same way.
        if let Some(group) = self.selected_group_session() {
            self.undo_group_run(&group);
        }
        UndoApply::default()
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

    /// Write (or remove, for an empty label) a name over a run of the workspace's changes,
    /// committed so it reaches peers through the same sync as the changes it names.
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

    /// Write the workspace document back to the state `id` names. A record's
    /// history belongs to its app, so a row of one reverts nothing here.
    fn revert_to(&mut self, tree: &mut impl ActivityTree, id: &HistoryEntryId) -> bool {
        if !self.host.mutations_available() {
            return false;
        }
        // A row of another record, or of the workspace's own document, is not the layout's to
        // rewind: the layout's document is this device's to write.
        if self.governance_rows.contains(id) {
            return false;
        }
        let origin = self.projected_origins.get(id).cloned();
        if origin
            .as_ref()
            .is_some_and(|address| address.origin.record().is_some())
        {
            return false;
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
        // Bring the read model back in line with what just changed underneath it, so the revert
        // shows up as the newest entry.
        let _ = tree.resync_activity_cache();
        self.last_heads.clear();
        self.refresh(tree);
        true
    }

    fn undo_band(&mut self, tree: &mut impl ActivityTree, request: &BandUndoRequest) -> UndoApply {
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
                    .map_or(super::index::HistoryOrigin::Layout, |address| {
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
                let run = ChangeRange::new(first.change, last.change);
                match first.origin {
                    super::index::HistoryOrigin::Layout => {
                        let Ok(outcome) = ui::revert_invocation(
                            tree.document_mut(),
                            &run,
                            None,
                            self.source.authors(),
                        ) else {
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
                    // A record's history is its app's to write.
                    super::index::HistoryOrigin::AppRecord { .. } => UndoApply::default(),
                }
            }
            // Tree-only callers and pre-index tests retain native addressing.
            (None, None) => {
                let run = ChangeRange::new(request.start, request.end);
                let Ok(outcome) =
                    ui::revert_invocation(tree.document_mut(), &run, None, self.source.authors())
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

fn handled_outcome() -> WorkspaceActivityOutcome {
    WorkspaceActivityOutcome {
        handled: true,
        ..WorkspaceActivityOutcome::default()
    }
}

// ===================================================================== Workspace vocabulary
// =====================================================================

/// What one workspace change did, read from its patches once: a change's patches are fixed by its
/// hash, and diffing every change of the workspace on every refresh made a workspace switch's first
/// frame O(history).
#[derive(Default)]
pub(super) struct WorkspaceChange {
    /// The workspace the change belongs to; `None` for several or none.
    workspace: Option<WorkspaceId>,
    counts: PatchCounts,
    bookkeeping: bool,
}

impl WorkspaceChange {
    fn read(
        doc: &mut automerge::AutoCommit,
        deps: &[ChangeHash],
        hash: ChangeHash,
        reader: Option<&dyn super::GovernanceReader>,
    ) -> Self {
        #[cfg(test)]
        WORKSPACE_CHANGE_READS.with(|reads| reads.set(reads.get().saturating_add(1)));
        let patches = doc.diff(deps, &[hash]);
        let mut counts = PatchCounts::default();
        for patch in &patches {
            counts.record(patch, reader);
        }
        // A change that only recorded which parts of the governance another change wrote is that
        // change's bookkeeping.
        let recorded = counts.recorded.clone();
        Self {
            workspace: workspace_touched(doc, &patches, hash),
            bookkeeping: is_workspace_bookkeeping(&patches, &|key| {
                recorded.iter().any(|held| held == key)
            }),
            counts,
        }
    }
}

#[cfg(test)]
thread_local! {
    /// How many workspace changes this thread diffed, for the tests that pin that a refresh reads
    /// only new changes.
    pub(super) static WORKSPACE_CHANGE_READS: Cell<usize> = const { Cell::new(0) };
}

/// Rewrite each row's summary in the workspace's own words where nameable,
/// else keeps [`HistorySource`]'s generic one. Returns, per entry, the
/// workspace the change belongs to — `None` for several or none. `known`
/// keeps each change's reading across refreshes; only new changes are
/// diffed. Each entry's hash and dependencies come from `log`, the history
/// log `entries` were read through, so the workspace's changes are not listed
/// again.
/// What reads a workspace change's patches into words: the host's reader of the signed values its
/// governance keeps, and who the people its changes name are.
struct Readers<'a> {
    governance: Option<&'a dyn super::GovernanceReader>,
    authors: &'a ui::history_source::AuthorDirectory,
}

fn describe_workspace_changes(
    doc: &mut automerge::AutoCommit,
    log: &ui::history_source::HistoryLog,
    entries: &mut [HistoryEntry],
    known: &mut HashMap<ChangeHash, WorkspaceChange>,
    readers: &Readers<'_>,
) -> Vec<Option<WorkspaceId>> {
    let mut tags: Vec<Option<WorkspaceId>> = vec![None; entries.len()];
    for (entry, tag) in entries.iter_mut().zip(&mut tags) {
        let Some((hash, deps)) = log.change(&entry.id) else {
            continue;
        };
        // `deps -> [hash]` isolates exactly that change.
        let change = known
            .entry(hash)
            .or_insert_with(|| WorkspaceChange::read(doc, deps, hash, readers.governance));
        *tag = change.workspace;
        if let Some((summary, entry_kind)) = change.counts.describe(readers.authors) {
            // A change that says what was done keeps its own words; one that only says the layout
            // changed is read from its patches, which say which window opened or whose name
            // changed.
            let tells_its_own_story = entry
                .context
                .as_ref()
                .is_some_and(|context| context.what.is_some());
            if !tells_its_own_story {
                entry.summary = summary;
            }
            entry.entry_kind = entry_kind;
            // Changes that each did the same to someone, one after another, read as one row:
            // "Added Ada and Claude Code".
            if let Some((action, items)) = change.counts.fold(readers.authors) {
                let mut context = entry.context.take().unwrap_or_default();
                context.facets.retain(|facet| {
                    facet.key != ui::FOLD_ACTION_FACET && facet.key != ui::FOLD_ITEM_FACET
                });
                context.facets.push(ui::tilo_actions::ActionFacet::new(
                    ui::FOLD_ACTION_FACET,
                    action,
                ));
                context.facets.extend(
                    items
                        .into_iter()
                        .map(|item| ui::tilo_actions::ActionFacet::new(ui::FOLD_ITEM_FACET, item)),
                );
                entry.context = Some(context);
            }
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
        } else if entry
            .context
            .as_ref()
            .is_none_or(|context| context.what.is_none() && context.group.is_none())
        {
            // The workspace's own storage keys are not words anyone should read, and a change
            // that names only who made it says nothing of its own about what it did. One in a
            // work session keeps the session's words.
            entry.summary = t("activity-event-updated");
        }
    }
    tags
}

/// The workspace keys a desktop writes to keep its projection consistent (the id counter, the
/// record index, and in older workspaces who owned each id namespace and a kept
/// strip's marker), and those its devices write to keep it: the receipt of each action done in it
/// (the action is its own row), what each device holds of it, where each device that keeps it
/// answers, and the invitation parts its receipts share. State about the workspace, not something
/// anyone did in it.
const WORKSPACE_BOOKKEEPING_KEYS: &[&str] = &[
    tilo_desktop_model::projection::WORKSPACE_DESKTOP_COUNTER_KEY,
    "contents_owners_v1",
    tilo_desktop_model::projection::WORKSPACE_RECORDS_KEY,
    "retained_empty_v1",
    tilo_desktop_model::projection::WORKSPACE_EFFECT_RECEIPTS_KEY,
    "held_copies_v1",
    "keeping_devices_v1",
    "invitation_parts_v1",
];

/// Whether a change only wrote the workspace's bookkeeping: its own state keys, or the record's
/// copy of its fields — a name, an owner, a grant — which is state about the workspace, not
/// something anyone did in it.
fn is_workspace_bookkeeping(patches: &[automerge::Patch], recorded: &dyn Fn(&str) -> bool) -> bool {
    !patches.is_empty()
        && patches.iter().all(|patch| {
            if in_workspace_copy(patch) {
                return true;
            }
            if patch.path.is_empty()
                && let automerge::PatchAction::PutMap { key, .. } = &patch.action
                && recorded(key)
            {
                return true;
            }
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
                .any(|key| WORKSPACE_BOOKKEEPING_KEYS.contains(&key))
        })
}

/// The workspace a change belongs to, or `None` for more than one or none. Read **at the change's
/// own heads**: a cache flush re-inserts every entry, so a historical object may not exist at the
/// current state.
fn workspace_touched(
    doc: &automerge::AutoCommit,
    patches: &[automerge::Patch],
    hash: ChangeHash,
) -> Option<WorkspaceId> {
    let heads = [hash];
    let mut found: Option<WorkspaceId> = None;
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
                ScalarValue::Str(text) => match WorkspaceId::parse(text) {
                    Some(id) => id,
                    None => continue,
                },
                _ => continue,
            },
            _ => continue,
        };
        match found {
            None => found = Some(id),
            // Two workspaces in one change — a whole-tree flush. It is nobody's workspace in
            // particular.
            Some(prev) if prev != id => return None,
            Some(_) => {}
        }
    }
    found
}

/// Running counts of one change's patches, turned into a single workspace event
/// ([`PatchCounts::describe`]) and its place in the feed
/// ([`PatchCounts::feed_kind`]). Content, membership and permission changes
/// are news; a window opened is listed in History but is nobody's news;
/// setup (the workspace's own first writes) and window layout stay out of
/// the feed: they are durable, but they are not "what people did".
#[derive(Default)]
struct PatchCounts {
    grants: Vec<String>,
    revokes: usize,
    /// Signed changes to who is in the workspace, their roles and its grants, that no reader could
    /// say more of.
    membership: usize,
    /// What the signed membership changes did, person by person, as the host read them.
    members: Vec<MemberStep>,
    /// The keys of operations that only recorded which parts of the governance another change
    /// wrote.
    recorded: Vec<String>,
    /// The devices whose rows the change wrote, each by its name where the host read one.
    devices: Vec<Option<String>>,
    names: Vec<String>,
    owner_set: bool,
    workspace_set: bool,
    tabs_added: usize,
    tabs_removed: usize,
    tab_titles: Vec<String>,
    groups_moved: usize,
    /// Groups the change put in a strip's `window_groups` list, and took out of one: a group added
    /// or removed is a layout change, but a group taken out and not put back is a window closed.
    groups_added: usize,
    groups_removed: usize,
    workspaces_added: usize,
    workspaces_removed: usize,
    geometry: usize,
    active_switched: bool,
}

impl PatchCounts {
    fn record(&mut self, p: &automerge::Patch, reader: Option<&dyn super::GovernanceReader>) {
        // The record keeps a copy of each workspace's own fields — its name,
        // its workspace, its policy and the grants in it — under
        // `workspaces/<id>/workspace`, written once by whichever device first
        // lays the record out. What people change is the workspace document's own
        // root, so the copy is not a change.
        if in_workspace_copy(p) {
            return;
        }
        let container = path_tail(p);
        if self.record_governance(p, &container, reader) {
            return;
        }
        match (&p.action, container.as_str()) {
            // A workspace's layout: a window is an entry of its `windows` , and a group closes with
            // its last window. (Older workspaces kept each window under its group's `tabs` .)
            (automerge::PatchAction::PutMap { value, .. }, "windows" | "tabs")
                if matches!(value.0, Value::Object(_)) =>
            {
                self.tabs_added = self.tabs_added.saturating_add(1);
            }
            (automerge::PatchAction::DeleteMap { .. }, "windows" | "tabs" | "groups") => {
                self.tabs_removed = self.tabs_removed.saturating_add(1);
            }
            // A window's whole tab, its title among it.
            (automerge::PatchAction::PutMap { key, value, .. }, _) if key == "tab" => {
                if let Some(title) = scalar_string(&value.0)
                    .and_then(|json| serde_json::from_str::<tilo_desktop_model::Tab>(&json).ok())
                    .map(|tab| tab.title)
                    .filter(|title| !title.is_empty())
                {
                    self.tab_titles.push(title);
                }
            }
            // Where an entry is: its order among its siblings, its parent.
            (automerge::PatchAction::PutMap { key, .. }, _)
                if key == "order" || key == "group" || key == "strip" =>
            {
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
            // The workspace's id at its root, not a field of the same name in its layout.
            (automerge::PatchAction::PutMap { key, .. }, "")
                if key == tilo_desktop_model::projection::WORKSPACE_ID_KEY =>
            {
                self.workspace_set = true;
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
                self.groups_added = self.groups_added.saturating_add(values.len());
            }
            (automerge::PatchAction::DeleteSeq { length, .. }, "window_groups") => {
                self.groups_moved = self.groups_moved.saturating_add(*length);
                self.groups_removed = self.groups_removed.saturating_add(*length);
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

    /// What the patch `p`, landing in `container`, did to the workspace's governance: the grants
    /// its apps' sandboxes hold, who is in it, the devices it knows. Whether it was one of those.
    fn record_governance(
        &mut self,
        p: &automerge::Patch,
        container: &str,
        reader: Option<&dyn super::GovernanceReader>,
    ) -> bool {
        match (&p.action, container) {
            (automerge::PatchAction::PutMap { value, .. }, "sandbox") => {
                self.grants.push(
                    describe_sandbox(&value.0).unwrap_or_else(|| t("activity-capability-unknown")),
                );
            }
            (automerge::PatchAction::DeleteMap { .. }, "sandbox") => {
                self.revokes = self.revokes.saturating_add(1);
            }
            // Who is in the workspace, their roles and its grants: each change
            // a signed operation under its own root key.
            (automerge::PatchAction::PutMap { key, value, .. }, "")
                if key
                    .strip_prefix(tilo_desktop_model::projection::WORKSPACE_MEMBERS_KEY)
                    .is_some_and(|rest| rest.starts_with('/')) =>
            {
                self.record_membership(key, &value.0, reader);
            }
            // A device's row: its certificate, and what admitted it.
            (automerge::PatchAction::PutMap { value, .. }, "peer_identities_v1") => {
                self.devices.push(
                    reader
                        .zip(scalar_bytes(&value.0))
                        .and_then(|(reader, row)| reader.device_label(row))
                        .filter(|label| !label.trim().is_empty()),
                );
            }
            _ => return false,
        }
        true
    }

    /// The signed membership operation stored under `key`, as the host reads it.
    fn record_membership(
        &mut self,
        key: &str,
        value: &Value<'_>,
        reader: Option<&dyn super::GovernanceReader>,
    ) {
        match reader
            .zip(scalar_bytes(value))
            .and_then(|(reader, op)| reader.membership(op))
        {
            Some(super::MembershipWrite::Recorded) => self.recorded.push(key.to_owned()),
            Some(super::MembershipWrite::Admitted { person }) => {
                self.members.push(MemberStep::Admitted { person });
            }
            Some(super::MembershipWrite::Changes(changes)) => {
                self.members.extend(changes.into_iter().map(MemberStep::of));
            }
            None => self.membership = self.membership.saturating_add(1),
        }
    }

    /// Groups the change took out of strips and did not put back: a group closes with its last
    /// window, so each is a window closed. A group moved is taken out and put back, and is no
    /// close.
    fn closed_groups(&self) -> usize {
        self.groups_removed.saturating_sub(self.groups_added)
    }

    /// The windows the change closed: the tabs it removed, and the groups it closed with their last
    /// window.
    fn closed_windows(&self) -> usize {
        self.tabs_removed.saturating_add(self.closed_groups())
    }

    /// Who and what the change brought into the workspace or out of it: its people, else the
    /// devices whose rows it wrote.
    fn describe_people(&self, authors: &ui::history_source::AuthorDirectory) -> Option<String> {
        if let Some(said) = self.describe_members(authors) {
            return Some(said);
        }
        if self.membership > 0 {
            return Some(t("activity-event-membership"));
        }
        match self.devices.as_slice() {
            [] => None,
            [Some(name)] => Some(t_args(
                "activity-event-device-added",
                &[("name", name.into())],
            )),
            [None] => Some(t("activity-event-device-added-unnamed")),
            many => Some(counted("activity-event-devices-added", many.len())),
        }
    }

    /// What the change did to who is in the workspace, naming each person: who it added and as
    /// what, who it removed, whose role it changed. `None` when it wrote no membership change the
    /// host read.
    fn describe_members(&self, authors: &ui::history_source::AuthorDirectory) -> Option<String> {
        let named: Vec<&MemberStep> = self
            .members
            .iter()
            .filter(|step| !matches!(step, MemberStep::Other))
            .collect();
        match named.as_slice() {
            [] if self.members.is_empty() => None,
            [] => Some(t("activity-event-membership")),
            [one] => Some(one.describe(authors)),
            many if many.iter().all(|step| step.added().is_some()) => {
                let people: Vec<String> = many
                    .iter()
                    .filter_map(|step| step.added())
                    .map(|person| member_name(authors, person))
                    .collect();
                Some(t_args(
                    "activity-event-members-added",
                    &[("names", join_titles(&people).into())],
                ))
            }
            _ => Some(t("activity-event-membership")),
        }
    }

    /// What the change did and to whom, when changes like it one after another read as one row
    /// ([`ui::FOLD_ACTION_FACET`]): someone added, a device, or the windows a person opened.
    fn fold(&self, authors: &ui::history_source::AuthorDirectory) -> Option<(String, Vec<String>)> {
        if let [step] = self.members.as_slice()
            && self.membership == 0
            && let Some(person) = step.added()
        {
            return Some((t("activity-fold-added"), vec![member_name(authors, person)]));
        }
        if self.members.is_empty()
            && self.membership == 0
            && let [Some(device)] = self.devices.as_slice()
        {
            return Some((
                t("activity-fold-added-devices"),
                vec![t_args("activity-fold-device", &[("name", device.into())])],
            ));
        }
        // Only a change that reads as the windows it opened, and nothing more.
        if !self.tab_titles.is_empty()
            && self.describe(authors).map(|(said, _)| said)
                == Some(t_args(
                    "activity-event-opened",
                    &[("titles", join_titles(&self.tab_titles).into())],
                ))
        {
            return Some((t("activity-fold-opened"), self.tab_titles.clone()));
        }
        None
    }

    /// The highest-priority signal wins — governance beats structural noise, and a whole-list write
    /// (the first `seed_doc_from_cache` ) is recognised before the per-field cases, or a flush that
    /// granted nothing reads as "granted 4 capabilities".
    fn describe(
        &self,
        authors: &ui::history_source::AuthorDirectory,
    ) -> Option<(String, EntryKind)> {
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
        // A workspace document's first change names the workspace, its owner and its name at once:
        // that is the workspace being made.
        if self.workspace_set && self.owner_set {
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
        if let Some(said) = self.describe_people(authors) {
            return Some((said, EntryKind::Access));
        }
        if self.owner_set {
            return Some((t("activity-event-owner"), EntryKind::Access));
        }
        if self.workspace_set {
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
        if self.closed_windows() > 0 {
            return Some((
                counted("activity-event-closed-windows", self.closed_windows()),
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

/// One person's membership change, as a signed operation wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum MemberStep {
    Added {
        person: String,
        role: tilo_action_policy::Role,
    },
    Admitted {
        person: String,
    },
    Removed {
        person: String,
    },
    Role {
        person: String,
        role: tilo_action_policy::Role,
    },
    /// Anything else the workspace's authority took: a ban, a grant, a role defined.
    Other,
}

impl MemberStep {
    fn of(change: tilo_action_policy::Change) -> Self {
        use tilo_action_policy::Change;
        match change {
            Change::AddMember { person, role } => Self::Added { person, role },
            Change::RemoveMember { person } => Self::Removed { person },
            Change::SetRole { person, role } => Self::Role { person, role },
            _ => Self::Other,
        }
    }

    /// Who the step brought into the workspace, when it brought someone in.
    fn added(&self) -> Option<&str> {
        match self {
            Self::Added { person, .. } | Self::Admitted { person } => Some(person),
            _ => None,
        }
    }

    fn describe(&self, authors: &ui::history_source::AuthorDirectory) -> String {
        let name = |person: &str| member_name(authors, person);
        match self {
            Self::Added { person, role } => t_args(
                "activity-event-member-added",
                &[
                    ("name", name(person).into()),
                    ("role", role_key(*role).into()),
                ],
            ),
            Self::Admitted { person } => t_args(
                "activity-event-member-joined",
                &[("name", name(person).into())],
            ),
            Self::Removed { person } => t_args(
                "activity-event-member-removed",
                &[("name", name(person).into())],
            ),
            Self::Role { person, role } => t_args(
                "activity-event-member-role",
                &[
                    ("name", name(person).into()),
                    ("role", role_key(*role).into()),
                ],
            ),
            Self::Other => t("activity-event-membership"),
        }
    }
}

/// How the workspace names `person` (as a membership change names them), or "a member" for
/// someone it does not know.
fn member_name(authors: &ui::history_source::AuthorDirectory, person: &str) -> String {
    let name = authors.resolve(person).name;
    if name.trim().is_empty() || name == ui::history_source::UNKNOWN_AUTHOR_NAME {
        t("activity-event-member-unknown")
    } else {
        name
    }
}

/// The selector a sentence naming `role` reads it by.
fn role_key(role: tilo_action_policy::Role) -> &'static str {
    use tilo_action_policy::Role;
    match role {
        Role::Viewer => "viewer",
        Role::Commenter => "commenter",
        Role::Editor => "editor",
        Role::Moderator => "moderator",
        Role::Owner => "owner",
    }
}

fn scalar_bytes<'a>(v: &'a Value<'_>) -> Option<&'a [u8]> {
    match v {
        Value::Scalar(s) => match s.as_ref() {
            ScalarValue::Bytes(bytes) => Some(bytes.as_slice()),
            _ => None,
        },
        Value::Object(_) => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkspaceEventKind {
    Change,
    Setup,
    View,
    Layout,
}

impl WorkspaceEventKind {
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
    fn feed_kind(&self) -> WorkspaceEventKind {
        if self.workspaces_removed > 0 && self.workspaces_added == 0 {
            return WorkspaceEventKind::Change;
        }
        if self.workspaces_added > 0 {
            return WorkspaceEventKind::Setup;
        }
        if self.workspace_set && self.owner_set {
            return WorkspaceEventKind::Setup;
        }
        if !self.grants.is_empty()
            || self.revokes > 0
            || self.membership > 0
            || !self.members.is_empty()
            || !self.devices.is_empty()
        {
            return WorkspaceEventKind::Change;
        }
        if self.owner_set || self.workspace_set {
            return WorkspaceEventKind::Setup;
        }
        if !self.names.is_empty() {
            return WorkspaceEventKind::Change;
        }
        if self.tabs_added > 0 {
            return WorkspaceEventKind::View;
        }
        if self.closed_windows() > 0 {
            return WorkspaceEventKind::Change;
        }
        if self.groups_moved > 0 || self.geometry > 0 || self.active_switched {
            return WorkspaceEventKind::Layout;
        }
        WorkspaceEventKind::Change
    }
}

/// Whether `p` lands in the layout record's copy of a workspace's own fields
/// (`workspaces/<id>/workspace/…`), not in the workspace document's own root or in the record's
/// strips, groups and windows.
fn in_workspace_copy(p: &automerge::Patch) -> bool {
    let map = |index: usize| match p.path.get(index) {
        Some((_, Prop::Map(key))) => Some(key.as_str()),
        _ => None,
    };
    map(0) == Some("workspaces") && map(2) == Some("workspace")
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

/// What one sandbox capability the workspace records says, in its own words.
fn describe_sandbox(value: &Value<'_>) -> Option<String> {
    let Value::Scalar(s) = value else {
        return None;
    };
    let ScalarValue::Bytes(blob) = s.as_ref() else {
        return None;
    };
    serde_json::from_slice::<CapOverride>(blob)
        .ok()
        .map(|cap| describe_capability(&cap))
}

/// A sandbox capability in one short phrase: what, over what, to whom.
fn describe_capability(cap: &CapOverride) -> String {
    let verb = match cap.access {
        Access::Read => "read",
        Access::Write => "write",
    };
    let effect = match cap.effect {
        Effect::Grant | Effect::WorkspaceFileAccess => "grant",
        Effect::Restrict | Effect::WorkspaceFileBoundary => "restrict",
    };
    t_args(
        "activity-capability",
        &[
            ("effect", effect.into()),
            ("verb", verb.into()),
            ("scope", describe_scope(&cap.scope).into()),
            (
                "who",
                describe_grantee(&cap.grantee, cap.app.as_ref()).into(),
            ),
        ],
    )
}

fn describe_scope(scope: &Scope) -> String {
    match scope {
        Scope::FsPath(p) => p.clone(),
        // Record authority intentionally has no path: it follows the file across rename/move and
        // the stable id is not useful activity copy.
        Scope::FsResource(_) => t("activity-capability-one-shared-file"),
        Scope::Device(d) => d.name().to_owned(),
        Scope::NetHost(h) => t_args("activity-capability-network", &[("host", h.into())]),
        Scope::NetCalls => t("activity-capability-calls"),
    }
}

/// Who a capability reaches, and through which app when it names one.
fn describe_grantee(grantee: &Grantee, app: Option<&tilo_app_id::AppId>) -> String {
    let who = match grantee {
        Grantee::Member(MemberId::Person(id)) => short(id.as_str()),
        Grantee::Member(MemberId::Agent { id: agent, .. }) => {
            t_args("activity-capability-agent", &[("agent", agent.into())])
        }
        Grantee::Member(MemberId::Bot(bot)) => {
            t_args("activity-capability-bot", &[("bot", bot.into())])
        }
        Grantee::Role(_) => t("activity-capability-this-workspace"),
        Grantee::CustomRole(id) => id.clone(),
        Grantee::App(app) => app.clone(),
        Grantee::AnyKey => grantee.to_string(),
    };
    match app {
        Some(app) => t_args(
            "activity-capability-through-app",
            &[("who", who.into()), ("app", app.as_str().into())],
        ),
        None => who,
    }
}

/// A person id is either a readable account name or a 64-character public key. Show the first
/// whole, and enough of the second to compare against another one — chopping every id to eight
/// characters turned `test-owner` into `test-own` .
fn short(id: &str) -> String {
    const READABLE_MAX: usize = 24;
    if id.chars().count() <= READABLE_MAX {
        return id.to_string();
    }
    let mut out: String = id.chars().take(12).collect();
    out.push('\u{2026}');
    out
}

/// A catalogue message whose only variable is `$count` , so the catalogue's plural rules pick the
/// wording.
fn counted(key: &str, count: usize) -> String {
    t_args(key, &[("count", count.into())])
}

/// A catalogue message naming one document field.
fn field_change(key: &str, field: String) -> String {
    t_args(key, &[("field", field.into())])
}

/// How an action with no summary of its own ended. What an effect's receipt says: what the effect
/// did, with how it ended when it did not complete.
fn receipt_summary(
    context: &tilo_actions::ActionContext,
    status: &tilo_actions::EffectReceiptStatus,
) -> String {
    crate::desktop::i18n::ensure();
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

/// Name up to two windows, then count the rest — the row has a line, not a paragraph.
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

// ===================================================================== Tests
// =====================================================================

include!("native_history_tests.rs");
include!("workspace_activity_tests.rs");
include!("workspace_activity_desktop_tests.rs");

#[cfg(test)]
#[path = "pane/clock_tests.rs"]
mod clock_tests;
#[cfg(test)]
#[path = "pane_tests.rs"]
mod pane_tests;
#[cfg(test)]
#[path = "pane/resume_tests.rs"]
mod resume_tests;
