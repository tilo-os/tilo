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
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use automerge::{ChangeHash, Prop, ReadDoc, ScalarValue, Value};
use collab_types::OpClass;
use collab_types::caps::{CapOverride, Effect, Principal, Scope, Subject, Verb};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::{
    ArtifactKind, HistoryClassification, HistoryEntry, HistoryEntryId, HistoryFilter,
};
use ui::{
    ActivityDigest, ActivityTimeline, AuthorDirectory, BandUndoRequest, ChangeRun,
    GroupNameRequest, HistoryComparison, HistoryFacetIndex, HistorySelectionDetail, HistorySource,
    NamedRange, NarrativeRequest, RecordStore, Theme, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::shell::state::WorkspaceTree;
use crate::shell::workspace_history::{
    FeedCoverage, WorkspaceHistoryId, WorkspaceHistoryIndex, WorkspaceHistoryQuery,
    WorkspaceSeenFrontier,
};

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
    /// Full granular history with grouping, filters, rename and revert.
    Explore,
}

impl ActivityView {
    const ALL: [Self; 2] = [Self::CatchUp, Self::Explore];

    fn label(self) -> &'static str {
        match self {
            Self::CatchUp => "Catch up",
            Self::Explore => "Explore",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum LensSelection {
    #[default]
    All,
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
    Brief(usize),
    MoreBriefs,
    Lens(LensSelection),
    MoreLenses,
    Detail(DetailAction),
    ExplorerPane(ExplorerPane),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DetailAction {
    Open,
    Compare,
    Undo,
    Split,
    Merge,
}

impl DetailAction {
    fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Compare => "Compare",
            Self::Undo => "Undo",
            Self::Split => "Split",
            Self::Merge => "Merge",
        }
    }
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
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix multiplication; it cannot overflow or panic"
)]
fn at(transform: Affine, dx: f64, dy: f64) -> Affine {
    transform * Affine::translate((dx, dy))
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
            RoomScope::ThisRoom => "This room",
            RoomScope::AllWorkspaces => "All workspaces",
        }
    }
}

/// What a dispatched event did.
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
pub struct RoomActivityPane {
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
    catchup_frontier: WorkspaceSeenFrontier,
    passive_digest: ActivityDigest,
    passive_unread: usize,
    persistent_frontiers: bool,
    view: ActivityView,
    view_rects: Vec<(ActivityView, Rect)>,
    brief_rects: Vec<(usize, Rect)>,
    brief_more_rect: Option<Rect>,
    brief_more_advance: usize,
    brief_offset: usize,
    expanded_brief: Option<usize>,
    facet_index: HistoryFacetIndex,
    lens: LensSelection,
    lens_rects: Vec<(LensSelection, Rect)>,
    lens_more_rect: Option<Rect>,
    lens_more_advance: usize,
    lens_offset: usize,
    keyboard_focus: Option<ActivityFocus>,
    detail_action_rects: Vec<(DetailAction, Rect)>,
    comparison: Option<HistoryComparison>,
    comparison_selection: Vec<HistoryEntryId>,
    explorer_pane: ExplorerPane,
    explorer_pane_rects: Vec<(ExplorerPane, Rect)>,
    narrative: Option<ui::GroundedNarrative>,
}

impl Default for RoomActivityPane {
    fn default() -> Self {
        Self::new()
    }
}

impl RoomActivityPane {
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
        let revert_intent = Rc::new(Cell::new(None));
        let name_intent: Rc<RefCell<Option<GroupNameRequest>>> = Rc::new(RefCell::new(None));
        let undo_intent: Rc<RefCell<Option<BandUndoRequest>>> = Rc::new(RefCell::new(None));
        let clear_filters_intent = Rc::new(Cell::new(false));
        let ri = Rc::clone(&revert_intent);
        let ni = Rc::clone(&name_intent);
        let ui = Rc::clone(&undo_intent);
        let ci = Rc::clone(&clear_filters_intent);
        Self {
            timeline: {
                let mut t = ActivityTimeline::new()
                    .on_revert(move |id| ri.set(Some(*id)))
                    .on_undo_band(move |req| *ui.borrow_mut() = Some(req.clone()))
                    .on_clear_filters(move || ci.set(true))
                    .on_name_group(move |req| *ni.borrow_mut() = Some(req.clone()));
                t.set_draws_own_surface(false);
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
            catchup_frontier: WorkspaceSeenFrontier::default(),
            passive_digest: ActivityDigest::default(),
            passive_unread: 0,
            persistent_frontiers: false,
            view: ActivityView::default(),
            view_rects: Vec::new(),
            brief_rects: Vec::new(),
            brief_more_rect: None,
            brief_more_advance: 0,
            brief_offset: 0,
            expanded_brief: None,
            facet_index: HistoryFacetIndex::default(),
            lens: LensSelection::default(),
            lens_rects: Vec::new(),
            lens_more_rect: None,
            lens_more_advance: 0,
            lens_offset: 0,
            keyboard_focus: None,
            detail_action_rects: Vec::new(),
            comparison: None,
            comparison_selection: Vec::new(),
            explorer_pane: ExplorerPane::default(),
            explorer_pane_rects: Vec::new(),
            narrative: None,
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
        pane
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
        self.catchup_frontier = self
            .seen_frontiers
            .get(&workspace)
            .cloned()
            .unwrap_or_default();
        self.digest = self.passive_digest.clone();
        self.reset_narrative();
    }

    pub fn finish_view(&mut self, workspace: u64) {
        if self.timeline.entries().is_empty() {
            return;
        }
        let frontier = self.seen_frontiers.entry(workspace).or_default();
        if self.history_index.entries().is_empty() {
            for entry in self.timeline.entries() {
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
        self.view = view;
        self.expanded_brief = None;
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

    pub fn selection_detail(&self) -> Option<HistorySelectionDetail> {
        let range = self.timeline.selection()?;
        let mut detail = HistorySelectionDetail::from_range(self.timeline.entries(), range)?;
        let mut origin = None;
        for id in &detail.entry_ids {
            let next = self
                .projected_origins
                .get(id)
                .map(|address| address.origin.clone())
                .unwrap_or(crate::shell::workspace_history::HistoryOrigin::WorkspaceTree);
            if origin.as_ref().is_some_and(|current| current != &next) {
                detail.can_compare = false;
                detail.can_undo = false;
                break;
            }
            origin = Some(next);
        }
        Some(detail)
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
        if let Some(raw) = focus.entry_id {
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
            LensSelection::App(app) => filter.apps.push(app.clone()),
            LensSelection::ResourceKind(kind) => filter.resource_kinds.push(kind.clone()),
            LensSelection::Resource(resource) => filter.resources.push(resource.clone()),
            LensSelection::WorkGroup(group) => filter.work_groups.push(group.clone()),
            LensSelection::Facet(key, value) => filter
                .facets
                .push(ui::qos_actions::ActionFacet::new(key, value)),
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
    pub fn refresh(&mut self, tree: &mut WorkspaceTree) {
        tree.commit_room_change();

        let name = tree
            .active_workspace()
            .map(|w| w.name.clone())
            .unwrap_or_default();
        if name != self.room_name {
            self.room_name = name;
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

        // Bind the local automerge actor to the qOS identity, or local user
        // id; a peer's actor stays unresolved until collaboration says who.
        let actor = tree.doc.get_actor().to_hex_string();
        if !self.source.authors().is_registered(&actor) {
            let registered = self.source.authors_mut().register_local_identity(&actor);
            if !registered {
                let me = crate::shell::state::local_user_id();
                if !me.is_empty() {
                    self.source.authors_mut().register(
                        &actor,
                        ui::widget::HistoryAuthor::new(
                            me,
                            ui::history_source::stable_hue(&actor),
                            ui::history_source::short_id(&actor),
                        ),
                    );
                }
            }
        }

        let mut entries = self.source.entries(&mut tree.doc);
        let tags = describe_room_changes(&mut tree.doc, &mut entries);
        self.derived = entries.into_iter().zip(tags).collect();
        // Names come out of the same document as the changes, so a peer's name
        // arrives with their edits.
        self.named = ui::history_groups::read_named_ranges(&tree.doc, self.source.authors());
        self.apply_scope(tree);
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
                    }
                    rows.push(e);
                }
            }
        }
        self.scoped_tree_rows.clone_from(&rows);
        self.install_rows(rows);
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
            let rows: Vec<HistoryEntry> = self
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
            .map(|row| row.timeline_entry())
            .collect();
        self.passive_unread = passive_rows.len();
        self.passive_digest =
            ActivityDigest::from_entries(&passive_rows, SystemTime::UNIX_EPOCH, 5);
        let catchup_rows: Vec<HistoryEntry> = unseen(&self.catchup_frontier, tree)
            .into_iter()
            .map(|row| row.timeline_entry())
            .collect();
        self.digest = ActivityDigest::from_entries(&catchup_rows, SystemTime::UNIX_EPOCH, 5);
        self.reset_narrative();
    }

    pub fn feed_coverage(&self) -> &FeedCoverage {
        &self.feed_coverage
    }

    fn install_rows(&mut self, rows: Vec<HistoryEntry>) {
        let prior_selection = self.selected_ids();
        self.total_entries = rows.len();
        self.facet_index = HistoryFacetIndex::from_entries(&rows);
        self.digest = ActivityDigest::from_entries(&rows, SystemTime::UNIX_EPOCH, 5);
        self.passive_unread = rows.len();
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
        self.narrative = NarrativeRequest::from_digest(&self.digest)
            .map(|request| ui::deterministic_narrative(&request));
    }

    /// Install optional model prose only when every cited action belongs to
    /// the current digest. Invalid or stale output leaves the deterministic
    /// summary in place.
    pub fn apply_generated_narrative(&mut self, narrative: ui::GroundedNarrative) -> bool {
        let Some(request) = NarrativeRequest::from_digest(&self.digest) else {
            return false;
        };
        let Some(valid) = ui::validate_narrative(&request, narrative) else {
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
        } else if self.view == ActivityView::Explore && vw >= 1_000.0 {
            (vw - 2.0 * SHEET_INSET).min(1_100.0)
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

    fn shows_chip_rows(&self) -> bool {
        self.view == ActivityView::Explore
            && self.sheet.height() > HEADER_H + VIEW_ROW_H + 2.0 * CHIP_ROW_H + 140.0
    }

    fn chrome_height(&self) -> f64 {
        if self.shows_chip_rows() {
            HEADER_H + VIEW_ROW_H + 2.0 * CHIP_ROW_H
        } else {
            HEADER_H + VIEW_ROW_H
        }
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
        self.draw_scope_chips(scene, transform, tr, theme);
        self.draw_view_tabs(scene, transform, tr, theme);

        self.period_rects.clear();
        self.author_rects.clear();
        if self.shows_chip_rows() {
            self.draw_filter_chip_rows(scene, transform, tr, theme);
        }
        match self.view {
            ActivityView::CatchUp => self.draw_catch_up(scene, transform, tr, theme),
            ActivityView::Explore => {
                self.draw_explorer_panel_tabs(scene, transform, tr, theme);
                self.draw_explorer_sidebars(scene, transform, tr, theme);
                if self.sheet.width() >= 900.0 || self.explorer_pane == ExplorerPane::Timeline {
                    self.timeline.draw(scene, transform, tr, theme, presence);
                }
            }
        }
        self.draw_keyboard_focus(scene, transform, theme);
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
                theme.tokens.colors.surface_white_5,
                None,
                &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.md)),
            );
        }
        let size = theme.tokens.typography.scale.sm;
        if show_left {
            tr.draw_text_weighted(
                scene,
                "Browse by",
                size,
                theme.tokens.colors.text_primary,
                at(transform, left.x0 + 12.0, left.y0 + 12.0),
                None,
                ui::text_render::TextWeight::Medium,
            );
            let mut y = left.y0 + 38.0;
            let candidates = self.lens_candidates();
            if self.lens_offset >= candidates.len() {
                self.lens_offset = 0;
            }
            let capacity = (((left.y1 - 6.0 - y) / 32.0).floor() as usize).max(1);
            let remaining = candidates.len().saturating_sub(self.lens_offset);
            let shown = remaining.min(if remaining > capacity {
                capacity.saturating_sub(1)
            } else {
                capacity
            });
            for (lens, label) in candidates.iter().skip(self.lens_offset).take(shown) {
                self.draw_lens_row(scene, transform, tr, theme, lens, label, &mut y, left);
            }
            if shown < remaining || self.lens_offset > 0 {
                let label = if shown < remaining {
                    "More lenses…"
                } else {
                    "First lenses…"
                };
                self.lens_more_rect = draw_more_row(scene, transform, tr, theme, left, y, label);
                self.lens_more_advance = shown.max(1);
            }
        }

        if show_right {
            tr.draw_text_weighted(
                scene,
                "Provenance",
                size,
                theme.tokens.colors.text_primary,
                at(transform, right.x0 + 12.0, right.y0 + 12.0),
                None,
                ui::text_render::TextWeight::Medium,
            );
            if let Some(detail) = self.selection_detail() {
                let width = (right.width() - 24.0).max(0.0) as f32;
                tr.draw_text(
                    scene,
                    &detail.title,
                    theme.tokens.typography.scale.base,
                    theme.tokens.colors.text_primary,
                    at(transform, right.x0 + 12.0, right.y0 + 40.0),
                    Some(width),
                );
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
                let provenance = format!(
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
                );
                tr.draw_text(
                    scene,
                    &provenance,
                    size,
                    theme.tokens.colors.text_dim,
                    at(transform, right.x0 + 12.0, right.y0 + 88.0),
                    Some(width),
                );
                let mut x = right.x0 + 12.0;
                let mut action_y = right.y0 + 170.0;
                for action in [
                    DetailAction::Open,
                    DetailAction::Compare,
                    DetailAction::Undo,
                    DetailAction::Split,
                    DetailAction::Merge,
                ] {
                    let enabled = match action {
                        DetailAction::Open => !detail.resources.is_empty(),
                        DetailAction::Compare => detail.can_compare,
                        DetailAction::Undo => detail.can_undo,
                        DetailAction::Split => self.split_candidate().is_some(),
                        DetailAction::Merge => self.merge_candidate().is_some(),
                    };
                    if !enabled {
                        continue;
                    }
                    if x + 60.0 > right.x1 - 12.0 {
                        x = right.x0 + 12.0;
                        action_y += 34.0;
                    }
                    let rect = Rect::new(x, action_y, x + 60.0, action_y + 28.0);
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        theme.tokens.colors.surface_white_5,
                        None,
                        &RoundedRect::from_rect(rect, 6.0),
                    );
                    tr.draw_text(
                        scene,
                        self.detail_action_label(action),
                        size,
                        theme.tokens.colors.text_primary,
                        at(transform, rect.x0 + 8.0, rect.y0 + 7.0),
                        None,
                    );
                    self.detail_action_rects.push((action, rect));
                    x += 66.0;
                }
                let selected_ids = self.selected_ids();
                if self.comparison_selection == selected_ids
                    && let Some(comparison) = &self.comparison
                {
                    let mut lines = vec![format!(
                        "Before {} B · After {} B · {} patches",
                        comparison.before_document.len(),
                        comparison.after_document.len(),
                        comparison.patches.len()
                    )];
                    lines.extend(
                        comparison
                            .patches
                            .iter()
                            .take(4)
                            .map(|patch| format!("{:?}", patch.action)),
                    );
                    tr.draw_text(
                        scene,
                        &lines.join("\n"),
                        size,
                        theme.tokens.colors.text_dim,
                        at(transform, right.x0 + 12.0, right.y0 + 246.0),
                        Some(width),
                    );
                }
            }
        }
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
            Some((rect.width() - 16.0).max(0.0) as f32),
        );
        self.lens_rects.push((lens.clone(), rect));
        *y += 32.0;
    }

    fn lens_candidates(&self) -> Vec<(LensSelection, String)> {
        let mut rows = vec![(LensSelection::All, "Everything".to_string())];
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

    fn draw_catch_up(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.brief_rects.clear();
        self.brief_more_rect = None;
        self.brief_more_advance = 0;
        let mut y = self.sheet.y0 + self.chrome_height() + 8.0;
        let x0 = self.sheet.x0 + 12.0;
        let x1 = self.sheet.x1 - 12.0;
        if self.digest.briefs.is_empty() {
            tr.draw_text(
                scene,
                "You're caught up",
                theme.tokens.typography.scale.base,
                theme.tokens.colors.text_dim,
                at(transform, x0 + 4.0, y + 12.0),
                Some((x1 - x0 - 8.0).max(0.0) as f32),
            );
            return;
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
            let expanded = self.expanded_brief == Some(index);
            let height = if expanded { 112.0 } else { 70.0 };
            let more_room = 34.0;
            let has_later = index + 1 < self.digest.briefs.len() || self.brief_offset > 0;
            if y + height + if has_later { more_room } else { 0.0 } > self.sheet.y1 - 10.0 {
                break;
            }
            let rect = Rect::new(x0, y, x1, y + height);
            self.brief_rects.push((index, rect));
            let card = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.md));
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surface_white_5,
                None,
                &card,
            );
            let width = (rect.width() - 24.0).max(0.0) as f32;
            tr.draw_text_weighted(
                scene,
                &brief.title,
                theme.tokens.typography.scale.base,
                theme.tokens.colors.text_primary,
                at(transform, rect.x0 + 12.0, rect.y0 + 10.0),
                Some(width),
                ui::text_render::TextWeight::Medium,
            );
            tr.draw_text(
                scene,
                &brief.detail,
                theme.tokens.typography.scale.sm,
                theme.tokens.colors.text_dim,
                at(transform, rect.x0 + 12.0, rect.y0 + 34.0),
                Some(width),
            );
            if expanded {
                let mut provenance = brief.apps.join(", ");
                if !brief.resources.is_empty() {
                    if !provenance.is_empty() {
                        provenance.push_str(" · ");
                    }
                    provenance.push_str(&brief.resources.join(", "));
                }
                if provenance.is_empty() {
                    provenance = format!("{} underlying changes", brief.changes);
                }
                tr.draw_text(
                    scene,
                    &format!("{provenance}\nOpen in Explorer →"),
                    theme.tokens.typography.scale.sm,
                    theme.tokens.colors.text_dim,
                    at(transform, rect.x0 + 12.0, rect.y0 + 76.0),
                    Some(width),
                );
            }
            y += height + 8.0;
            shown += 1;
        }
        let remaining = self
            .digest
            .briefs
            .len()
            .saturating_sub(self.brief_offset + shown);
        if remaining > 0 || self.brief_offset > 0 {
            let label = if remaining > 0 {
                "More outcomes…"
            } else {
                "First outcomes…"
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

    /// Paint the sheet's title, factual scope/count, and latest outcome.
    fn draw_title_and_subtitle(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let title_size = tokens.typography.scale.lg;
        let title = match self.scope {
            RoomScope::ThisRoom => "Room activity",
            RoomScope::AllWorkspaces => "Workspace activity",
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
        let contributor_count = self.contributors.len();
        let people = if contributor_count == 1 {
            "1 person".to_string()
        } else {
            format!("{contributor_count} people")
        };
        let mut subtitle = match self.scope {
            RoomScope::ThisRoom => {
                let room = if self.room_name.is_empty() {
                    "This room"
                } else {
                    self.room_name.as_str()
                };
                format!("{room} \u{00b7} {} changes by {people}", self.total_entries)
            }
            RoomScope::AllWorkspaces => format!(
                "Every workspace on this machine \u{00b7} {} changes by {people}",
                self.total_entries,
            ),
        };
        if !self.feed_coverage.is_complete() {
            let missing = self.feed_coverage.unavailable_records.len();
            let capped = self.feed_coverage.capped_records;
            if missing > 0 {
                subtitle.push_str(&format!(
                    " · {missing} record{} unavailable",
                    if missing == 1 { "" } else { "s" }
                ));
            }
            if capped > 0 {
                subtitle.push_str(&format!(" · {capped} additional records truncated"));
            }
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "sheet width is a UI pixel measurement, always far under f32's range"
        )]
        let wrap_width = (self.sheet.width() - 32.0) as f32;
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
            tr.draw_text(
                scene,
                &latest,
                sub_size,
                colors.text_primary,
                at(transform, self.sheet.x0 + 16.0, self.sheet.y0 + 51.0),
                Some(wrap_width),
            );
        }
    }

    /// Paint the "This room" / "All workspaces" scope pills and record
    /// their rects for hit-testing. Not drawn at all if they wouldn't fit.
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
        let mut widths: Vec<f64> = Vec::with_capacity(2);
        for sc in RoomScope::ALL {
            widths.push(f64::from(tr.measure(sc.label(), size, None).0) + 16.0);
        }
        let total: f64 = widths.iter().sum::<f64>() + 4.0;
        if total >= self.sheet.width() * 0.62 {
            return;
        }
        let mut x = self.sheet.x1 - 16.0 - total;
        for (i, sc) in RoomScope::ALL.iter().enumerate() {
            let w = widths.get(i).copied().unwrap_or(0.0);
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
                    colors.surface_white_5
                },
                None,
                &shape,
            );
            let (lw, lh) = tr.measure(sc.label(), size, None);
            tr.draw_text(
                scene,
                sc.label(),
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

        if self.timeline.wants_keyboard() {
            // The inline group-name editor owns every unchorded key until it
            // commits or cancels; Tab included, so a half-typed name cannot be
            // abandoned by the pane-level focus controller.
        } else if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
            && !modifiers.state().control_key()
            && !modifiers.state().super_key()
            && !modifiers.state().alt_key()
        {
            if let Some(outcome) = self.handle_navigation_key(
                logical_key,
                modifiers.state().shift_key(),
                tree,
                records,
            ) {
                return outcome;
            }
        }

        // The shell's own chip rows first — they sit above the timeline and
        // the timeline would otherwise treat a chip click as a click on its
        // own filter strip.
        if let (Some(p), WindowEvent::MouseInput { state, button, .. }) = (cursor, event)
            && *state == winit::event::ElementState::Pressed
            && *button == winit::event::MouseButton::Left
        {
            for (scope, rect) in self.scope_rects.clone() {
                if rect.contains(p) {
                    self.keyboard_focus = Some(ActivityFocus::Scope(scope));
                    self.set_scope(scope, tree);
                    return RoomActivityOutcome {
                        handled: true,
                        ..RoomActivityOutcome::default()
                    };
                }
            }
            for (view, rect) in self.view_rects.clone() {
                if rect.contains(p) {
                    self.keyboard_focus = Some(ActivityFocus::View(view));
                    self.set_view(view);
                    return RoomActivityOutcome {
                        handled: true,
                        ..RoomActivityOutcome::default()
                    };
                }
            }
            if self.view == ActivityView::CatchUp {
                for (index, rect) in self.brief_rects.clone() {
                    if rect.contains(p) {
                        self.keyboard_focus = Some(ActivityFocus::Brief(index));
                        if self.expanded_brief == Some(index) {
                            self.open_brief_in_explorer(index);
                        } else {
                            self.expanded_brief = Some(index);
                        }
                        return RoomActivityOutcome {
                            handled: true,
                            ..RoomActivityOutcome::default()
                        };
                    }
                }
                if self.brief_more_rect.is_some_and(|rect| rect.contains(p)) {
                    self.keyboard_focus = Some(ActivityFocus::MoreBriefs);
                    self.advance_briefs();
                    return RoomActivityOutcome {
                        handled: true,
                        ..RoomActivityOutcome::default()
                    };
                }
            }
            if self.view == ActivityView::Explore {
                for (pane, rect) in self.explorer_pane_rects.clone() {
                    if rect.contains(p) {
                        self.keyboard_focus = Some(ActivityFocus::ExplorerPane(pane));
                        self.explorer_pane = pane;
                        return RoomActivityOutcome {
                            handled: true,
                            ..RoomActivityOutcome::default()
                        };
                    }
                }
                for (action, rect) in self.detail_action_rects.clone() {
                    if !rect.contains(p) {
                        continue;
                    }
                    self.keyboard_focus = Some(ActivityFocus::Detail(action));
                    out.handled = true;
                    match action {
                        DetailAction::Open => {
                            out.open_target = self
                                .selection_detail()
                                .and_then(|detail| detail.resources.into_iter().next());
                        }
                        DetailAction::Compare => {
                            self.comparison = self.compare_selection(tree, records);
                            self.comparison_selection = self.selected_ids();
                            out.compared = self.comparison.is_some();
                        }
                        DetailAction::Undo => {
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
                                out.reverted = applied.wrote;
                                out.undo_partial = applied.partial;
                            }
                        }
                        DetailAction::Split => out.named = self.split_selection_group(tree),
                        DetailAction::Merge => out.named = self.merge_selection_groups(tree),
                    }
                    return out;
                }
                for (lens, rect) in self.lens_rects.clone() {
                    if rect.contains(p) {
                        self.keyboard_focus = Some(ActivityFocus::Lens(lens.clone()));
                        self.lens = lens;
                        self.reinstall_filter();
                        return RoomActivityOutcome {
                            handled: true,
                            ..RoomActivityOutcome::default()
                        };
                    }
                }
                if self.lens_more_rect.is_some_and(|rect| rect.contains(p)) {
                    self.keyboard_focus = Some(ActivityFocus::MoreLenses);
                    self.advance_lenses();
                    return RoomActivityOutcome {
                        handled: true,
                        ..RoomActivityOutcome::default()
                    };
                }
            }
            for (period, rect) in self.period_rects.clone() {
                if rect.contains(p) {
                    self.keyboard_focus = Some(ActivityFocus::Period(period));
                    self.set_period(period);
                    return RoomActivityOutcome {
                        handled: true,
                        ..RoomActivityOutcome::default()
                    };
                }
            }
            for (author, rect) in self.author_rects.clone() {
                if rect.contains(p) {
                    self.keyboard_focus = Some(ActivityFocus::Author(author.clone()));
                    self.set_author_filter(author);
                    return RoomActivityOutcome {
                        handled: true,
                        ..RoomActivityOutcome::default()
                    };
                }
            }
            if self.author_more_rect.is_some_and(|rect| rect.contains(p)) {
                self.keyboard_focus = Some(ActivityFocus::MorePeople);
                self.advance_people();
                return RoomActivityOutcome {
                    handled: true,
                    ..RoomActivityOutcome::default()
                };
            }
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
            ActivityView::CatchUp => order.extend(
                self.brief_rects
                    .iter()
                    .map(|(index, _)| ActivityFocus::Brief(*index)),
            ),
            ActivityView::Explore => {
                order.extend(
                    self.explorer_pane_rects
                        .iter()
                        .map(|(pane, _)| ActivityFocus::ExplorerPane(*pane)),
                );
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
            ActivityFocus::Brief(value) => self
                .brief_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::MoreBriefs => self.brief_more_rect,
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
        }
    }

    fn move_focus(&mut self, backwards: bool, to_edge: Option<bool>) {
        let order = self.focus_order();
        if order.is_empty() {
            self.keyboard_focus = None;
            return;
        }
        let next = if let Some(end) = to_edge {
            if end {
                order.len().saturating_sub(1)
            } else {
                0
            }
        } else if let Some(current) = self
            .keyboard_focus
            .as_ref()
            .and_then(|focus| order.iter().position(|candidate| candidate == focus))
        {
            if backwards {
                current.checked_sub(1).unwrap_or_else(|| order.len() - 1)
            } else {
                current.saturating_add(1) % order.len()
            }
        } else if backwards {
            order.len() - 1
        } else {
            0
        };
        self.keyboard_focus = order.get(next).cloned();
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
            ActivityFocus::Brief(index) => {
                if self.expanded_brief == Some(index) {
                    self.open_brief_in_explorer(index);
                } else {
                    self.expanded_brief = Some(index);
                }
            }
            ActivityFocus::MoreBriefs => self.advance_briefs(),
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
                self.comparison = self.compare_selection(tree, records);
                self.comparison_selection = self.selected_ids();
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
            ActivityFocus::Detail(DetailAction::Split) => {
                outcome.named = self.split_selection_group(tree);
            }
            ActivityFocus::Detail(DetailAction::Merge) => {
                outcome.named = self.merge_selection_groups(tree);
            }
            ActivityFocus::ExplorerPane(pane) => self.explorer_pane = pane,
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
            ActivityFocus::Author(Some(value)) => Some(value.clone()),
            ActivityFocus::Author(None) => Some("Everyone".to_string()),
            ActivityFocus::MorePeople => Some("More people".to_string()),
            ActivityFocus::Brief(index) => self
                .digest
                .briefs
                .get(*index)
                .map(|brief| brief.title.clone()),
            ActivityFocus::MoreBriefs => Some("More outcomes".to_string()),
            ActivityFocus::Lens(LensSelection::All) => Some("Everything".to_string()),
            ActivityFocus::Lens(LensSelection::App(value))
            | ActivityFocus::Lens(LensSelection::ResourceKind(value))
            | ActivityFocus::Lens(LensSelection::Resource(value))
            | ActivityFocus::Lens(LensSelection::WorkGroup(value)) => Some(value.clone()),
            ActivityFocus::Lens(LensSelection::Facet(key, value)) => {
                Some(format!("{key}: {value}"))
            }
            ActivityFocus::Lens(LensSelection::Unclassified) => Some("Unclassified".to_string()),
            ActivityFocus::MoreLenses => Some("More lenses".to_string()),
            ActivityFocus::Detail(action) => Some(self.detail_action_label(*action).to_string()),
            ActivityFocus::ExplorerPane(pane) => Some(pane.label().to_string()),
        }
    }

    /// Semantic mirror of this immediate-mode overlay. Shell overlays are not
    /// backed by taffy `NodeId`s, so the app-level widget walker cannot infer
    /// this tree. The overlay accessibility bridge can publish this snapshot
    /// verbatim; state is derived from the same values used for paint and hit
    /// testing, preventing spoken and visible selection from drifting apart.
    pub fn accessibility_snapshot(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        let mut root = AccessNode::new(Role::Dialog)
            .with_name("Room activity")
            .with_description(match self.view {
                ActivityView::CatchUp => {
                    "A summary of outcomes since this device last viewed the room"
                }
                ActivityView::Explore => {
                    "The complete workspace history with filters and provenance"
                }
            })
            .with_state(AccessState::new().modal(true));
        let order = self.focus_order();
        let set_size = order.len();
        root.children = order
            .iter()
            .enumerate()
            .filter_map(|(index, focus)| {
                let label = self.focus_label(focus)?;
                let (role, selected, expanded, description) = match focus {
                    ActivityFocus::Scope(value) => (
                        Role::RadioButton,
                        self.scope == *value,
                        None,
                        Some("History scope".to_string()),
                    ),
                    ActivityFocus::View(value) => (
                        Role::Tab,
                        self.view == *value,
                        None,
                        Some("History view".to_string()),
                    ),
                    ActivityFocus::Period(value) => (
                        Role::RadioButton,
                        self.period == *value,
                        None,
                        Some("Time range".to_string()),
                    ),
                    ActivityFocus::Author(value) => (
                        Role::RadioButton,
                        self.author == *value,
                        None,
                        Some("Person filter".to_string()),
                    ),
                    ActivityFocus::MorePeople => (
                        Role::Button,
                        false,
                        None,
                        Some("Show more contributor filters".to_string()),
                    ),
                    ActivityFocus::Brief(brief_index) => {
                        let brief = self.digest.briefs.get(*brief_index)?;
                        (
                            Role::Button,
                            false,
                            Some(self.expanded_brief == Some(*brief_index)),
                            Some(format!(
                                "{}; {} changes by {}",
                                brief.detail,
                                brief.changes,
                                brief
                                    .authors
                                    .iter()
                                    .map(|author| author.name.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )),
                        )
                    }
                    ActivityFocus::MoreBriefs => (
                        Role::Button,
                        false,
                        None,
                        Some("Show more outcome summaries".to_string()),
                    ),
                    ActivityFocus::Lens(value) => (
                        Role::RadioButton,
                        self.lens == *value,
                        None,
                        Some("History lens".to_string()),
                    ),
                    ActivityFocus::MoreLenses => (
                        Role::Button,
                        false,
                        None,
                        Some("Show more history lenses".to_string()),
                    ),
                    ActivityFocus::Detail(_) => (
                        Role::Button,
                        false,
                        None,
                        Some("Selected history action".to_string()),
                    ),
                    ActivityFocus::ExplorerPane(value) => (
                        Role::Tab,
                        self.explorer_pane == *value,
                        None,
                        Some("Explorer panel".to_string()),
                    ),
                };
                let mut state = AccessState::new()
                    .focusable(true)
                    .focused(self.keyboard_focus.as_ref() == Some(focus));
                state = if role == Role::RadioButton {
                    state.checked(selected)
                } else {
                    state.selected(selected)
                };
                if let Some(value) = expanded {
                    state = state.expanded(value);
                }
                let mut node = AccessNode::new(role)
                    .with_name(label)
                    .with_state(state)
                    .with_position(index.saturating_add(1), set_size);
                if let Some(value) = description {
                    node = node.with_description(value);
                }
                Some(node)
            })
            .collect();
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
        root
    }

    pub fn accessibility_rect_for_path(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [0] => Some(self.sheet),
            [0, index] => self
                .focus_order()
                .get(*index)
                .and_then(|focus| self.rect_for_focus(focus)),
            _ => None,
        }
    }

    pub fn focus_accessibility_path(&mut self, path: &[usize]) -> bool {
        let [0, index] = path else {
            return false;
        };
        let Some(focus) = self.focus_order().get(*index).cloned() else {
            return false;
        };
        self.keyboard_focus = Some(focus);
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
            ActivityFocus::Brief(value) => self
                .brief_rects
                .iter()
                .find_map(|(item, rect)| (item == value).then_some(*rect)),
            ActivityFocus::MoreBriefs => self.brief_more_rect,
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
        self.timeline.wants_keyboard()
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
            for entry in &self.timeline.entries()[start_index..=end_index] {
                let origin = self
                    .projected_origins
                    .get(&entry.id)
                    .map(|address| address.origin.clone())
                    .unwrap_or(crate::shell::workspace_history::HistoryOrigin::WorkspaceTree);
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
                colors.surface_white_5
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
                colors.surface_white_5
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
            colors.surface_white_5,
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
        theme.tokens.colors.surface_white_5,
        None,
        &RoundedRect::from_rect(rect, 6.0),
    );
    tr.draw_text(
        scene,
        label,
        theme.tokens.typography.scale.sm,
        theme.tokens.colors.text_primary,
        at(transform, rect.x0 + 8.0, rect.y0 + 6.0),
        Some((rect.width() - 16.0).max(0.0) as f32),
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
            return Some(("Set the room owner".to_string(), OpClass::Govern));
        }
        if self.room_set {
            return Some(("Assigned the room identity".to_string(), OpClass::Govern));
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
                "Switched the active workspace".to_string(),
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
        Scope::Workspace => "this workspace".to_string(),
        Scope::App(a) => a.clone(),
        Scope::ResourceType(k) => format!("every {k:?}"),
        Scope::ResourceInstance(_) => "one artifact".to_string(),
        Scope::FsPath(p) => p.clone(),
        Scope::Device(d) => d.clone(),
        Scope::NetHost(h) => format!("network {h}"),
        Scope::Action { app, action } => format!("{app}:{action}"),
        Scope::Room(_) => "this room".to_string(),
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
        Principal::Room(_) => "this room".to_string(),
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

    fn pane_over(tree: &mut WorkspaceTree) -> RoomActivityPane {
        let mut pane = RoomActivityPane::new();
        // Legacy timeline tests exercise the granular surface explicitly;
        // production opens on the outcome-first Catch up tab.
        pane.set_view(ActivityView::Explore);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.refresh(tree);
        pane
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
            .filter(|e| e.summary.contains("Set the room owner"))
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
        assert_eq!(pane.focused_control_label().as_deref(), Some("This room"));

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
        assert_eq!(pane.focused_control_label().as_deref(), Some("This room"));
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
        assert_eq!(snapshot.role, ui::a11y::Role::Dialog);
        assert!(snapshot.state.modal);
        assert!(
            snapshot.children.iter().any(|node| {
                node.role == ui::a11y::Role::Tab
                    && node.name.as_deref() == Some("Catch up")
                    && node.state.selected
            }),
            "selected tab is announced"
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
}
