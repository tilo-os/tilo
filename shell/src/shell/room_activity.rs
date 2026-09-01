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
use ui::widget::{ArtifactKind, HistoryEntry, HistoryEntryId, HistoryFilter};
use ui::{
    ActivityDigest, ActivityTimeline, AuthorDirectory, GroupNameRequest, HistoryFacetIndex,
    HistorySource, NamedRange, Theme, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{Modifiers, WindowEvent};

use crate::shell::state::WorkspaceTree;

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
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RoomActivityOutcome {
    pub handled: bool,
    pub reverted: bool,
    /// A band was named/renamed/un-named — a document write too.
    pub named: bool,
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
    named: Vec<NamedRange>,
    /// Heads at the last rebuild, so a no-op frame skips re-deriving.
    last_heads: Vec<ChangeHash>,
    period: Period,
    /// Every row the document yielded, with the workspace each belongs
    /// to (`None` for no single workspace). [`Self::scope`] selects the
    /// subset handed to the timeline.
    derived: Vec<(HistoryEntry, Option<u64>)>,
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
    room_name: String,
    total_entries: usize,
    /// Outcome-first reduction of the currently scoped rows. The full
    /// timeline remains the source of detail underneath it.
    digest: ActivityDigest,
    /// Switching rooms moves no changes, so the heads check alone
    /// wouldn't notice — but it changes which rows are this room's.
    last_active: Option<u64>,
    now_override: Option<SystemTime>,
    /// Per-workspace private frontier. This is deliberately a wall-clock
    /// reduction rather than a document write: opening history must not
    /// create collaborative history.
    seen_at: HashMap<u64, SystemTime>,
    catchup_since: SystemTime,
    persistent_frontiers: bool,
    view: ActivityView,
    view_rects: Vec<(ActivityView, Rect)>,
    brief_rects: Vec<(usize, Rect)>,
    expanded_brief: Option<usize>,
    facet_index: HistoryFacetIndex,
    lens: LensSelection,
    lens_rects: Vec<(LensSelection, Rect)>,
}

impl Default for RoomActivityPane {
    fn default() -> Self {
        Self::new()
    }
}

impl RoomActivityPane {
    pub fn new() -> Self {
        let revert_intent = Rc::new(Cell::new(None));
        let name_intent: Rc<RefCell<Option<GroupNameRequest>>> = Rc::new(RefCell::new(None));
        let ri = Rc::clone(&revert_intent);
        let ni = Rc::clone(&name_intent);
        Self {
            timeline: {
                let mut t = ActivityTimeline::new()
                    .on_revert(move |id| ri.set(Some(*id)))
                    .on_name_group(move |req| *ni.borrow_mut() = Some(req.clone()));
                t.set_draws_own_surface(false);
                t
            },
            // A workspace is a cluster of windows saved/reopened collaboratively.
            source: HistorySource::new().with_artifact_kind(ArtifactKind::Cluster),
            revert_intent,
            name_intent,
            named: Vec::new(),
            last_heads: Vec::new(),
            period: Period::default(),
            derived: Vec::new(),
            scope: RoomScope::default(),
            scope_rects: Vec::new(),
            author: None,
            contributors: Vec::new(),
            sheet: Rect::ZERO,
            period_rects: Vec::new(),
            author_rects: Vec::new(),
            room_name: String::new(),
            total_entries: 0,
            digest: ActivityDigest::default(),
            last_active: None,
            now_override: None,
            seen_at: HashMap::new(),
            catchup_since: SystemTime::UNIX_EPOCH,
            persistent_frontiers: false,
            view: ActivityView::default(),
            view_rects: Vec::new(),
            brief_rects: Vec::new(),
            expanded_brief: None,
            facet_index: HistoryFacetIndex::default(),
            lens: LensSelection::default(),
            lens_rects: Vec::new(),
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
                let (Ok(id), Some(seconds)) = (workspace.parse::<u64>(), value.as_u64()) else {
                    continue;
                };
                pane.seen_at
                    .insert(id, SystemTime::UNIX_EPOCH + Duration::from_secs(seconds));
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
        self.catchup_since = self
            .seen_at
            .get(&workspace)
            .copied()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        self.rebuild_digest_since_frontier();
    }

    pub fn finish_view(&mut self, workspace: u64) {
        let Some(latest) = self
            .timeline
            .entries()
            .iter()
            .map(|entry| entry.timestamp)
            .max()
        else {
            return;
        };
        self.seen_at.insert(workspace, latest);
        self.catchup_since = latest;
        self.rebuild_digest_since_frontier();
        if self.persistent_frontiers {
            self.persist_frontiers();
        }
    }

    /// `(headline, unread, pulse)` consumed by passive chrome. `pulse` is a
    /// stable projection of the newest content id, not a frame counter.
    pub fn glance_signal(&self, workspace: u64) -> (Option<String>, usize, u64) {
        let since = self
            .seen_at
            .get(&workspace)
            .copied()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let unread = self
            .timeline
            .entries()
            .iter()
            .filter(|entry| entry.timestamp > since)
            .count();
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
        (self.digest.headline(), unread, pulse)
    }

    fn rebuild_digest_since_frontier(&mut self) {
        self.digest = ActivityDigest::from_entries(self.timeline.entries(), self.catchup_since, 5);
    }

    fn persist_frontiers(&self) {
        let mut frontiers = serde_json::Map::new();
        for (workspace, time) in &self.seen_at {
            let seconds = time
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            frontiers.insert(workspace.to_string(), serde_json::Value::from(seconds));
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

    /// Op-class chips and author typeahead are the widget's own, left alone.
    fn reinstall_filter(&mut self) {
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
        }
        self.timeline.set_filter(filter);
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
        self.total_entries = rows.len();
        self.facet_index = HistoryFacetIndex::from_entries(&rows);
        self.digest = ActivityDigest::from_entries(&rows, self.catchup_since, 5);

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
        self.timeline.set_named_ranges(self.named.clone());
        self.reinstall_filter();
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
        self.timeline.set_rect(Rect::new(
            self.sheet.x0 + 8.0 + if three_panel { EXPLORER_LEFT_W } else { 0.0 },
            rows_y0,
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
                self.draw_explorer_sidebars(scene, transform, tr, theme);
                self.timeline.draw(scene, transform, tr, theme, presence);
            }
        }
    }

    fn draw_explorer_sidebars(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.lens_rects.clear();
        if self.sheet.width() < 900.0 {
            return;
        }
        let top = self.sheet.y0 + self.chrome_height();
        let bottom = self.sheet.y1 - 8.0;
        let left = Rect::new(
            self.sheet.x0 + 8.0,
            top,
            self.sheet.x0 + EXPLORER_LEFT_W,
            bottom,
        );
        let right = Rect::new(
            self.sheet.x1 - EXPLORER_DETAIL_W,
            top,
            self.sheet.x1 - 8.0,
            bottom,
        );
        for rect in [left, right] {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surface_white_5,
                None,
                &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.md)),
            );
        }
        let size = theme.tokens.typography.scale.sm;
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
        self.draw_lens_row(
            scene,
            transform,
            tr,
            theme,
            &LensSelection::All,
            "Everything",
            &mut y,
            left,
        );
        let apps: Vec<(String, usize)> = self
            .facet_index
            .apps
            .iter()
            .map(|(label, count)| (label.clone(), *count))
            .collect();
        for (label, count) in apps.into_iter().take(4) {
            let lens = LensSelection::App(label.clone());
            self.draw_lens_row(
                scene,
                transform,
                tr,
                theme,
                &lens,
                &format!("{label}  {count}"),
                &mut y,
                left,
            );
        }
        let kinds: Vec<(String, usize)> = self
            .facet_index
            .resource_kinds
            .iter()
            .map(|(label, count)| (label.clone(), *count))
            .collect();
        for (label, count) in kinds.into_iter().take(4) {
            let lens = LensSelection::ResourceKind(label.clone());
            self.draw_lens_row(
                scene,
                transform,
                tr,
                theme,
                &lens,
                &format!("{label}  {count}"),
                &mut y,
                left,
            );
        }

        tr.draw_text_weighted(
            scene,
            "Provenance",
            size,
            theme.tokens.colors.text_primary,
            at(transform, right.x0 + 12.0, right.y0 + 12.0),
            None,
            ui::text_render::TextWeight::Medium,
        );
        if let Some(entry) = self.timeline.visible_entry(0) {
            let width = (right.width() - 24.0).max(0.0) as f32;
            tr.draw_text(
                scene,
                &entry.summary,
                theme.tokens.typography.scale.base,
                theme.tokens.colors.text_primary,
                at(transform, right.x0 + 12.0, right.y0 + 40.0),
                Some(width),
            );
            let context = entry.context.as_ref();
            let app = context
                .and_then(|value| value.app.as_ref())
                .map_or("Legacy / direct edit", |app| app.label.as_str());
            let resource = context
                .and_then(|value| value.resource.as_ref())
                .map_or("No resource declared", |resource| resource.label.as_str());
            let detail = format!("{}\n{}\n{}", entry.author.name, app, resource);
            tr.draw_text(
                scene,
                &detail,
                size,
                theme.tokens.colors.text_dim,
                at(transform, right.x0 + 12.0, right.y0 + 88.0),
                Some(width),
            );
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
        for (index, brief) in self.digest.briefs.iter().enumerate() {
            let expanded = self.expanded_brief == Some(index);
            let height = if expanded { 112.0 } else { 70.0 };
            if y + height > self.sheet.y1 - 10.0 {
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
                    &provenance,
                    theme.tokens.typography.scale.sm,
                    theme.tokens.colors.text_dim,
                    at(transform, rect.x0 + 12.0, rect.y0 + 76.0),
                    Some(width),
                );
            }
            y += height + 8.0;
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
        let people = if self.digest.people == 1 {
            "1 person".to_string()
        } else {
            format!("{} people", self.digest.people)
        };
        let subtitle = match self.scope {
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
        if let Some(headline) = self.digest.headline() {
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

        let mut people: Vec<(Option<String>, String)> = vec![(None, "Everyone".to_string())];
        for c in &self.contributors {
            people.push((Some(c.clone()), c.clone()));
        }
        let picked = self.author.clone();
        let y2 = y + CHIP_ROW_H;
        self.author_rects = draw_chip_row(
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
        let mut out = RoomActivityOutcome::default();

        // The shell's own chip rows first — they sit above the timeline and
        // the timeline would otherwise treat a chip click as a click on its
        // own filter strip.
        if let (Some(p), WindowEvent::MouseInput { state, button, .. }) = (cursor, event)
            && *state == winit::event::ElementState::Pressed
            && *button == winit::event::MouseButton::Left
        {
            for (scope, rect) in self.scope_rects.clone() {
                if rect.contains(p) {
                    self.set_scope(scope, tree);
                    return RoomActivityOutcome {
                        handled: true,
                        reverted: false,
                        named: false,
                    };
                }
            }
            for (view, rect) in self.view_rects.clone() {
                if rect.contains(p) {
                    self.set_view(view);
                    return RoomActivityOutcome {
                        handled: true,
                        reverted: false,
                        named: false,
                    };
                }
            }
            if self.view == ActivityView::CatchUp {
                for (index, rect) in self.brief_rects.clone() {
                    if rect.contains(p) {
                        self.expanded_brief = (self.expanded_brief != Some(index)).then_some(index);
                        return RoomActivityOutcome {
                            handled: true,
                            reverted: false,
                            named: false,
                        };
                    }
                }
            }
            if self.view == ActivityView::Explore {
                for (lens, rect) in self.lens_rects.clone() {
                    if rect.contains(p) {
                        self.lens = lens;
                        self.reinstall_filter();
                        return RoomActivityOutcome {
                            handled: true,
                            reverted: false,
                            named: false,
                        };
                    }
                }
            }
            for (period, rect) in self.period_rects.clone() {
                if rect.contains(p) {
                    self.set_period(period);
                    return RoomActivityOutcome {
                        handled: true,
                        reverted: false,
                        named: false,
                    };
                }
            }
            for (author, rect) in self.author_rects.clone() {
                if rect.contains(p) {
                    self.set_author_filter(author);
                    return RoomActivityOutcome {
                        handled: true,
                        reverted: false,
                        named: false,
                    };
                }
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
            if self.revert_to(tree, &id) {
                out.reverted = true;
            }
        }
        let name_req = self.name_intent.borrow_mut().take();
        if let Some(req) = name_req {
            out.handled = true;
            if self.apply_name(tree, &req) {
                out.named = true;
            }
        }
        out
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
        let Some(target) = ui::heads_for_entry(&mut tree.doc, id) else {
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
            entry.summary = summary;
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
}
