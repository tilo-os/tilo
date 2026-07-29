//! `ActivityTimeline` — Phase 1 activity-pane widget.
//!
//! Renders a chronological scrolling list of [`HistoryEntry`] rows
//! with an author-coloured avatar dot, an op-class glyph, a relative
//! timestamp, and a `[Revert]` chip on hover (for op classes that
//! support reverting — see [`op_class_is_revertible`]).
//!
//! Spec: `docs/superpowers/specs/2026-06-29-spaces-channels-and-cryptographic-roles.md`
//! Phase 1 ("Audit timeline + soft-delete + restore-from-history").
//!
//! ## Design rationale (high-level)
//!
//! - **Filter chip strip** across the top: "All / Edits / Comments /
//!   Governance / Deletes" plus a typeahead "author:Alice" input.
//!   Clicking a chip rewrites `self.filter` and rebuilds the visible
//!   list. The filter is stored *in the widget* so the chrome can
//!   wire a single state without owning a separate `HistoryFilter`
//!   field; mutating it from the outside is a one-line setter
//!   ([`Self::set_filter`]).
//! - **Row hover + `[Revert]` chip**: hover tracking is by row index;
//!   the chip is hit-tested only when the row's op class is
//!   revertible. Comments and Govern rows never paint the chip so
//!   the user can't trip the callback for op classes whose inverse
//!   is undefined.
//! - **Mouse wheel scrolling**: pixel-accurate vertical scroll over
//!   the row list, clamped to the natural extent so the user can't
//!   over-scroll into empty space.
//! - **Empty state**: the widget paints "No activity yet" centred in
//!   the rect when the visible row count is zero. Useful for the
//!   first-time-collaboration affordance.
//!
//! The widget never mutates the underlying CRDT — it is a viewer
//! over a `Vec<HistoryEntry>`. The host wires `on_revert` to whatever
//! op-emission path it has chosen for this artifact.

use std::collections::HashMap;
use std::time::SystemTime;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;
use crate::widget::history::{
    HistoryEntry, HistoryEntryId, HistoryFilter, format_relative_time, hue_to_rgb,
    op_class_glyph, op_class_is_revertible,
};

use collab_types::OpClass;

/// Height of one entry row. Pinned at 40 px so a 600 px tall pane
/// fits ~15 rows without scrolling — the typical "give me the last
/// half-hour of activity" glance.
pub const ROW_HEIGHT: f64 = 40.0;
/// Height of the filter chip strip across the top of the pane.
pub const FILTER_STRIP_HEIGHT: f64 = 36.0;
/// Inner padding between the pane edge and any content (rows, chips,
/// the typeahead input).
const PADDING: f64 = 8.0;
/// Diameter of the per-author avatar dot.
const AVATAR_DIAM: f64 = 16.0;
/// Width of the trailing `[Revert]` chip.
const REVERT_CHIP_W: f64 = 64.0;
/// Width of the relative-time slot to the left of the revert chip.
const TIME_SLOT_W: f64 = 80.0;
/// Width allocated to the op-class glyph + author label cluster.
const AUTHOR_SLOT_W: f64 = 160.0;

/// Five filter chips painted across the top of the pane.
/// Ordered left-to-right so a flat `[i]` lookup matches the visual.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ChipKind {
    All,
    Edits,
    Comments,
    Governance,
    Deletes,
}

impl ChipKind {
    pub const ALL: [ChipKind; 5] = [
        ChipKind::All,
        ChipKind::Edits,
        ChipKind::Comments,
        ChipKind::Governance,
        ChipKind::Deletes,
    ];

    /// Short uppercase label for the chip body.
    pub fn label(&self) -> &'static str {
        match self {
            ChipKind::All => "ALL",
            ChipKind::Edits => "EDITS",
            ChipKind::Comments => "COMMENTS",
            ChipKind::Governance => "GOVERNANCE",
            ChipKind::Deletes => "DELETES",
        }
    }

    /// Abbreviated label used when the full set of chips will not fit the
    /// pane's width. Every chip switches together — a strip with three long
    /// labels and two short ones reads as a rendering accident rather than as
    /// a deliberate density change.
    pub fn short_label(&self) -> &'static str {
        match self {
            ChipKind::All => "ALL",
            ChipKind::Edits => "EDIT",
            ChipKind::Comments => "NOTE",
            ChipKind::Governance => "GOV",
            ChipKind::Deletes => "DEL",
        }
    }

    /// Map a chip to the op-class subset it filters to. `All` is
    /// `None` because "no op-class filter" is how
    /// [`HistoryFilter`] expresses "show everything"; the empty Vec
    /// means "match all".
    pub fn op_classes(&self) -> Vec<OpClass> {
        match self {
            ChipKind::All => Vec::new(),
            // "Edits" bundles the three primary content-mutating
            // classes. Tombstone/Restore live in "Deletes" instead so
            // the user can isolate destructive activity from normal
            // edits.
            ChipKind::Edits => vec![OpClass::Splice, OpClass::Move, OpClass::EditMeta],
            ChipKind::Comments => vec![OpClass::Comment],
            ChipKind::Governance => vec![OpClass::Govern],
            ChipKind::Deletes => vec![OpClass::Tombstone, OpClass::Restore],
        }
    }
}

/// Below this pane width the row drops its per-author identity hash and the
/// chip strip switches to [`ChipKind::short_label`]. Picked as the width at
/// which the full-label strip plus the author pill stop fitting side by side.
const COMPACT_WIDTH: f64 = 420.0;
/// Below this pane width the trailing time + revert slots shrink so the
/// summary — the row's load-bearing text — keeps a usable share of the row.
const NARROW_WIDTH: f64 = 340.0;

/// Callback fired when the user clicks `[Revert]` on a row.
pub type RevertCallback = Box<dyn FnMut(&HistoryEntryId)>;

/// Callback fired when the user clicks `[Clear filters]` on the
/// filtered-to-nothing empty state. The widget resets its own chip + author
/// query before calling this; the host uses it to drop any filter it owns
/// itself (a date range, say) so "clear" clears everything the user can see.
pub type ClearFiltersCallback = Box<dyn FnMut()>;

/// Scrolling, filterable activity-pane widget. See module-level
/// docs for the contract.
pub struct ActivityTimeline {
    entries: Vec<HistoryEntry>,
    filter: HistoryFilter,
    /// Indexes (into `entries`) that pass the filter, in display
    /// order. Rebuilt whenever `entries`, `filter`, or the typeahead
    /// input changes.
    visible: Vec<usize>,
    /// Selected chip — drives the chip strip's active styling. `None`
    /// means "no active chip" (the user typed into the author input
    /// instead).
    active_chip: ChipKind,
    /// Free-text author typeahead. Lower-cased before matching.
    author_query: String,
    rect: Rect,
    scroll_y: f64,
    /// Index (into `visible`) of the row the cursor is currently
    /// hovering, or `None`. Drives the row highlight + the visibility
    /// of the `[Revert]` chip.
    hover_row: Option<usize>,
    last_cursor: Point,
    on_revert: Option<RevertCallback>,
    /// "Now" for the relative-timestamp paint. Tests + headless
    /// callers can pin this so a snapshot stays stable; live callers
    /// leave it at `None`, which means "use `SystemTime::now()` on
    /// every draw".
    now_override: Option<SystemTime>,
    /// Reverse-chronological display order. Defaults to `true`
    /// (newest at top); host can flip it.
    newest_first: bool,
    /// Last-drawn chip rects, in (kind, rect) order. Populated by
    /// `draw_filter_strip` so the click handler can hit-test without
    /// re-measuring text. Empty until the first paint — clicks land
    /// in the strip before that are no-ops.
    chip_rects: Vec<(ChipKind, Rect)>,
    /// Last-drawn rect of the trailing `author:` pill, so a click can clear
    /// the typeahead without the host having to re-derive the geometry.
    author_pill_rect: Rect,
    /// Last-drawn rect of the `[Clear filters]` chip in the
    /// filtered-to-nothing empty state. `Rect::ZERO` whenever that state is
    /// not on screen, so a stale click cannot trip it.
    clear_chip_rect: Rect,
    /// Human label for whatever time range the host installed on
    /// [`Self::filter`]. The widget cannot name a `SystemTime` pair in the
    /// user's own vocabulary ("Last 7 days"), so the host supplies the words
    /// and the widget renders them in the filter summary.
    time_range_label: Option<String>,
    on_clear_filters: Option<ClearFiltersCallback>,
    /// Whether to paint the widget's own card. `false` when the host has
    /// already drawn a surface underneath — two nested cards eight pixels
    /// apart read as a rendering mistake, not as depth.
    draws_own_surface: bool,
    dirty: bool,
}

impl Default for ActivityTimeline {
    fn default() -> Self {
        Self::new()
    }
}

impl ActivityTimeline {
    /// Construct an empty timeline. Use [`Self::with_entries`] /
    /// [`Self::set_entries`] to seed it.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            filter: HistoryFilter::default(),
            visible: Vec::new(),
            active_chip: ChipKind::All,
            author_query: String::new(),
            rect: Rect::ZERO,
            scroll_y: 0.0,
            hover_row: None,
            last_cursor: Point::ZERO,
            on_revert: None,
            now_override: None,
            newest_first: true,
            chip_rects: Vec::new(),
            author_pill_rect: Rect::ZERO,
            clear_chip_rect: Rect::ZERO,
            time_range_label: None,
            on_clear_filters: None,
            draws_own_surface: true,
            dirty: true,
        }
    }

    /// Builder: seed entries at construction time.
    pub fn with_entries(entries: Vec<HistoryEntry>) -> Self {
        let mut s = Self::new();
        s.set_entries(entries);
        s
    }

    /// Replace the entry list. Rebuilds the visible index. Scroll
    /// position is preserved (clamped to the new extent in `draw`).
    pub fn set_entries(&mut self, entries: Vec<HistoryEntry>) {
        self.entries = entries;
        self.rebuild();
    }

    /// Mutable handle to the underlying entry list. Use this when
    /// streaming new ops in — the chrome can push then call
    /// [`Self::refresh`].
    pub fn entries_mut(&mut self) -> &mut Vec<HistoryEntry> {
        &mut self.entries
    }

    /// Force a re-evaluation of the visible list after mutating
    /// `entries_mut`. Idempotent.
    pub fn refresh(&mut self) {
        self.rebuild();
    }

    /// Read the entries backing the pane, unfiltered.
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// Place the pane by hand, for a host that reserves a rect out of its own
    /// layout rather than putting the widget in a taffy tree — the compositor
    /// draws its chrome that way. The `Widget::apply_layout` path is unchanged;
    /// this is the same assignment it makes.
    pub fn set_rect(&mut self, rect: Rect) {
        if self.rect != rect {
            self.rect = rect;
            self.dirty = true;
        }
    }

    /// The rect the pane currently occupies.
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Install / replace the active filter. Author-typeahead +
    /// chip state are kept in the widget; everything else (time
    /// range, explicit author list) is what this method controls.
    pub fn set_filter(&mut self, filter: HistoryFilter) {
        self.filter = filter;
        self.rebuild();
    }

    /// Read the current filter — useful for tests + for the chrome
    /// that wants to render a "current filter" badge.
    pub fn filter(&self) -> &HistoryFilter {
        &self.filter
    }

    /// Builder: install a revert callback. Fires when the user
    /// clicks `[Revert]` on a row.
    pub fn on_revert<F: FnMut(&HistoryEntryId) + 'static>(mut self, f: F) -> Self {
        self.on_revert = Some(Box::new(f));
        self
    }

    /// Builder: install the callback fired by `[Clear filters]`.
    pub fn on_clear_filters<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_clear_filters = Some(Box::new(f));
        self
    }

    /// Suppress the widget's own card, for a host that has already painted a
    /// surface behind it. The rows, the chip strip and the empty state are
    /// unaffected — only the background fill and border are dropped.
    pub fn set_draws_own_surface(&mut self, draws: bool) {
        if self.draws_own_surface != draws {
            self.draws_own_surface = draws;
            self.dirty = true;
        }
    }

    /// Name the time range the host installed on the filter, in the words the
    /// host's own control uses. Shown in the "nothing matched" explanation so
    /// a date filter is never an invisible reason for an empty list.
    pub fn set_time_range_label(&mut self, label: Option<String>) {
        if self.time_range_label != label {
            self.time_range_label = label;
            self.dirty = true;
        }
    }

    /// Reset every filter the widget owns — the op-class chip and the author
    /// typeahead — and fire [`Self::on_clear_filters`] so the host drops the
    /// ones it owns. Wired to the `[Clear filters]` chip; also callable
    /// directly by a host that offers its own reset.
    pub fn clear_filters(&mut self) {
        self.active_chip = ChipKind::All;
        self.author_query.clear();
        self.rebuild();
        if let Some(cb) = self.on_clear_filters.as_mut() {
            cb();
        }
    }

    /// Whether any filter is currently narrowing the list — the widget's own
    /// chip / typeahead, or a range or author list the host installed.
    pub fn is_filtered(&self) -> bool {
        self.active_chip != ChipKind::All
            || !self.author_query.trim().is_empty()
            || !self.filter.authors.is_empty()
            || !self.filter.op_classes.is_empty()
            || self.filter.time_range.is_some()
    }

    /// The active narrowing, spelled out. Empty string when nothing narrows.
    ///
    /// This is what turns "the list is blank" into "the list is blank
    /// *because*" — it is rendered under the empty-state title whenever a
    /// filter, rather than an empty history, is why there is nothing to show.
    pub fn active_filter_summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.active_chip != ChipKind::All {
            parts.push(self.active_chip.label().to_string());
        }
        for op in &self.filter.op_classes {
            let label = format!("{op:?}").to_uppercase();
            if !parts.iter().any(|p| *p == label) {
                parts.push(label);
            }
        }
        let typed = self.author_query.trim();
        if !typed.is_empty() {
            parts.push(format!("author:{typed}"));
        }
        for a in &self.filter.authors {
            parts.push(a.clone());
        }
        if self.filter.time_range.is_some() {
            parts.push(
                self.time_range_label
                    .clone()
                    .unwrap_or_else(|| "a date range".to_string()),
            );
        }
        parts.join(" \u{00b7} ")
    }

    /// Title + explanation for the current empty state.
    ///
    /// Two genuinely different situations that must not read the same: a room
    /// nothing has happened in yet, and a room whose activity the current
    /// filter is hiding. The second returns the count it is hiding and the
    /// filter doing the hiding.
    pub fn empty_state_lines(&self) -> (String, Option<String>) {
        if self.entries.is_empty() {
            return ("No activity yet".to_string(), None);
        }
        let hidden = self.entries.len();
        let summary = self.active_filter_summary();
        let detail = if summary.is_empty() {
            format!("{hidden} entries are loaded but none are shown")
        } else {
            format!("{hidden} hidden by {summary}")
        };
        ("No activity matches these filters".to_string(), Some(detail))
    }

    /// Rect of the `[Clear filters]` chip, or `Rect::ZERO` when the
    /// filtered-to-nothing empty state is not on screen.
    pub fn clear_filters_chip_rect(&self) -> Rect {
        self.clear_chip_rect
    }

    /// Rect of the trailing `author:` pill in the chip strip. `Rect::ZERO`
    /// before the first paint.
    pub fn author_pill_rect(&self) -> Rect {
        self.author_pill_rect
    }

    /// Whether the pane is narrow enough to drop per-row secondary detail.
    fn is_compact(&self) -> bool {
        self.rect.width() < COMPACT_WIDTH
    }

    /// Width of the leading author cluster, proportional to the pane with a
    /// floor so a name is never squeezed to nothing.
    fn author_slot_w(&self) -> f64 {
        (self.rect.width() * 0.32).clamp(84.0, AUTHOR_SLOT_W)
    }

    /// Width of the trailing relative-time slot.
    fn time_slot_w(&self) -> f64 {
        if self.rect.width() < NARROW_WIDTH {
            48.0
        } else {
            TIME_SLOT_W
        }
    }

    /// Width of the trailing `[Revert]` chip.
    fn revert_chip_w(&self) -> f64 {
        if self.rect.width() < NARROW_WIDTH {
            48.0
        } else {
            REVERT_CHIP_W
        }
    }

    /// Top edge of the row list, below the pane padding and the chip strip.
    fn rows_top(&self) -> f64 {
        self.rect.y0 + PADDING + FILTER_STRIP_HEIGHT + PADDING
    }

    /// Half-open range of visible-row indices that intersect the viewport.
    ///
    /// Derived from the scroll offset rather than by walking every row, so a
    /// room with thousands of entries costs the same per frame as one with
    /// twenty. Nothing downstream may assume the range is the whole list.
    fn visible_row_range(&self) -> std::ops::Range<usize> {
        let n = self.visible.len();
        if n == 0 {
            return 0..0;
        }
        let vp = self.viewport_height();
        if vp <= 0.0 {
            return 0..0;
        }
        // Derived from the same inequalities `row_rect` clips on, so the window
        // and the rows that actually lay out cannot disagree: row `i` is on
        // screen iff its bottom edge is at or below the top of the list and
        // its top edge is at or above the bottom of it.
        let first = ((self.scroll_y / ROW_HEIGHT).ceil() as usize).saturating_sub(1);
        let last = ((self.scroll_y + vp) / ROW_HEIGHT).floor() as usize + 1;
        first.min(n)..last.min(n)
    }

    /// Flip the display order. `true` = newest at top (default).
    pub fn set_newest_first(&mut self, newest_first: bool) {
        if self.newest_first != newest_first {
            self.newest_first = newest_first;
            self.rebuild();
        }
    }

    /// Pin "now" for the relative-timestamp paint. Tests use this so
    /// "3 min ago" stays stable across runs.
    pub fn set_now_for_test(&mut self, now: SystemTime) {
        self.now_override = Some(now);
    }

    /// Change the active filter chip. Combines with the explicit
    /// op-class list in `filter` (chip-op-classes win when the chip
    /// is non-`All` to keep the chrome's mental model simple).
    pub fn set_active_chip(&mut self, chip: ChipKind) {
        self.active_chip = chip;
        self.rebuild();
    }

    /// Current active chip.
    pub fn active_chip(&self) -> ChipKind {
        self.active_chip
    }

    /// Set the author-typeahead text. Empty = no author narrowing.
    pub fn set_author_query(&mut self, q: impl Into<String>) {
        self.author_query = q.into();
        self.rebuild();
    }

    /// Current author-typeahead text.
    pub fn author_query(&self) -> &str {
        &self.author_query
    }

    /// Number of rows currently visible (post-filter).
    pub fn visible_len(&self) -> usize {
        self.visible.len()
    }

    /// Borrow a visible row by visible-index.
    pub fn visible_entry(&self, i: usize) -> Option<&HistoryEntry> {
        self.visible.get(i).map(|&j| &self.entries[j])
    }

    /// Row currently under the cursor, or `None` when the cursor is
    /// outside the row list.
    pub fn hover_row(&self) -> Option<usize> {
        self.hover_row
    }

    /// Compute the absolute rect of visible row `i` taking the
    /// current scroll into account. Returns `None` when the row is
    /// off the visible window (above the strip or below the rect).
    pub fn row_rect(&self, i: usize) -> Option<Rect> {
        if i >= self.visible.len() {
            return None;
        }
        let top = self.rows_top() - self.scroll_y;
        let y0 = top + (i as f64) * ROW_HEIGHT;
        let y1 = y0 + ROW_HEIGHT;
        // Spatial cull: rows entirely outside the rect are
        // off-screen — return None so callers can skip drawing /
        // hit-testing them.
        if y1 < self.rows_top() || y0 > self.rect.y1 - PADDING {
            return None;
        }
        Some(Rect::new(
            self.rect.x0 + PADDING,
            y0,
            self.rect.x1 - PADDING,
            y1,
        ))
    }

    /// Hit-test the `[Revert]` chip on row `i`. Returns `None` when
    /// the row is off-screen or its op class is non-revertible.
    pub fn revert_chip_rect(&self, i: usize) -> Option<Rect> {
        let row = self.row_rect(i)?;
        let entry = self.visible_entry(i)?;
        if !op_class_is_revertible(entry.op_class) {
            return None;
        }
        // The chip lives at the right edge of the row, 6 px inset
        // and vertically centred.
        let chip_h = ROW_HEIGHT - 16.0;
        let cy = (row.y0 + row.y1) / 2.0;
        Some(Rect::new(
            row.x1 - self.revert_chip_w() - 6.0,
            cy - chip_h / 2.0,
            row.x1 - 6.0,
            cy + chip_h / 2.0,
        ))
    }

    /// "Now" used for relative-time rendering. Pinned via
    /// `now_override` in tests; live `SystemTime::now()` otherwise.
    fn now(&self) -> SystemTime {
        self.now_override.unwrap_or_else(SystemTime::now)
    }

    /// Rebuild `self.visible` from the current entries / filter /
    /// chip / author query. O(n).
    fn rebuild(&mut self) {
        self.visible.clear();
        let chip_ops = self.active_chip.op_classes();
        let needle = self.author_query.trim().to_lowercase();
        for (i, e) in self.entries.iter().enumerate() {
            if !self.filter.matches(e) {
                continue;
            }
            if !chip_ops.is_empty() && !chip_ops.contains(&e.op_class) {
                continue;
            }
            if !needle.is_empty() && !e.author.name.to_lowercase().contains(&needle) {
                continue;
            }
            self.visible.push(i);
        }
        // Sort by time, tie-broken by the producer's own order — which is the
        // document's causal order. Automerge records a change's advisory time
        // in *seconds*, so a burst of activity inside one second is a wall of
        // ties; ordering those by input index is what keeps "newest first"
        // meaning something rather than silently degrading to oldest-first.
        if self.newest_first {
            self.visible.sort_by(|a, b| {
                self.entries[*b]
                    .timestamp
                    .cmp(&self.entries[*a].timestamp)
                    .then(b.cmp(a))
            });
        } else {
            self.visible.sort_by(|a, b| {
                self.entries[*a]
                    .timestamp
                    .cmp(&self.entries[*b].timestamp)
                    .then(a.cmp(b))
            });
        }
        // Clamp scroll to the new extent.
        let max_scroll = self.max_scroll();
        if self.scroll_y > max_scroll {
            self.scroll_y = max_scroll;
        }
        if self.hover_row.is_some_and(|h| h >= self.visible.len()) {
            self.hover_row = None;
        }
        self.dirty = true;
    }

    /// Pixel height of the rendered row list (un-clipped).
    fn content_height(&self) -> f64 {
        self.visible.len() as f64 * ROW_HEIGHT
    }

    /// Viewport height available for rows.
    fn viewport_height(&self) -> f64 {
        (self.rect.height() - 2.0 * PADDING - FILTER_STRIP_HEIGHT - PADDING).max(0.0)
    }

    /// Maximum legal `scroll_y` value — never scrolls past content
    /// end.
    fn max_scroll(&self) -> f64 {
        (self.content_height() - self.viewport_height()).max(0.0)
    }

    /// Rect of the chip-strip + author-input row across the top.
    pub fn filter_strip_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PADDING,
            self.rect.y0 + PADDING,
            self.rect.x1 - PADDING,
            self.rect.y0 + PADDING + FILTER_STRIP_HEIGHT,
        )
    }

    /// Find the visible-row index containing `p`, or `None`.
    ///
    /// Constant time: rows are a uniform-pitch stack, so the index falls out
    /// of the arithmetic. Scanning every row here was the other half of the
    /// per-frame cost a room with thousands of entries would have paid — this
    /// runs on every `CursorMoved`.
    pub fn row_at(&self, p: Point) -> Option<usize> {
        if p.x < self.rect.x0 + PADDING || p.x > self.rect.x1 - PADDING {
            return None;
        }
        let top = self.rows_top();
        if p.y < top || p.y > self.rect.y1 - PADDING {
            return None;
        }
        let idx = ((p.y - top + self.scroll_y) / ROW_HEIGHT).floor();
        if idx < 0.0 {
            return None;
        }
        let i = idx as usize;
        // `row_rect` owns the clipping rules; going through it keeps hover and
        // paint agreeing on which rows exist.
        self.row_rect(i).filter(|r| r.contains(p)).map(|_| i)
    }

    /// Draw one row body — avatar dot, author name, glyph, summary,
    /// time, optional revert chip.
    #[allow(clippy::too_many_arguments)]
    fn draw_row(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        entry: &HistoryEntry,
        is_hovered: bool,
        now: SystemTime,
    ) {
        // Highlight band when hovered. Mirrors CommandPalette's
        // selected-row affordance for visual continuity.
        if is_hovered {
            let shape = RoundedRect::from_rect(row, theme.tokens.radii.sm as f64);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.neutral[3],
                None,
                &shape,
            );
        }

        // Avatar dot. Hue → RGB conversion happens here so the
        // widget can pick its own saturation/value without forcing
        // the producer to pre-compute one.
        let cy = (row.y0 + row.y1) / 2.0;
        let (r, g, b) = hue_to_rgb(entry.author.avatar_hue);
        let dot_color = Color::from_rgba8(r, g, b, 255);
        let dot_x = row.x0 + 8.0 + AVATAR_DIAM / 2.0;
        let dot = Circle::new(Point::new(dot_x, cy), AVATAR_DIAM / 2.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            dot_color,
            None,
            &dot,
        );

        let text_size = theme.tokens.typography.scale.sm;
        let author_slot_w = self.author_slot_w();
        let compact = self.is_compact();
        let mut x_cursor = dot_x + AVATAR_DIAM / 2.0 + 8.0;

        // Author display name in primary text colour. Ellipsised to whatever
        // is left of the author slot, so a long display name cannot push the
        // identity hash over the glyph or the summary over the timestamp.
        let short_size = (text_size - 2.0).max(10.0);
        let short = entry.author.identity_short.as_str();
        let slot_end = row.x0 + author_slot_w - 6.0;
        let full_name_w = text_renderer
            .measure(entry.author.name.as_str(), text_size, None)
            .0 as f64;
        // The hash disambiguates two people with the same display name; the
        // name is what the reader is actually scanning for. So the hash is
        // shown out of what the name leaves over, and dropped entirely rather
        // than truncating the name to make room for it.
        let short_w = if compact || short.is_empty() {
            0.0
        } else {
            let w = text_renderer.measure(short, short_size, None).0 as f64 + 6.0;
            if x_cursor + full_name_w + w <= slot_end {
                w
            } else {
                0.0
            }
        };
        let name_budget = (slot_end - x_cursor - short_w).max(24.0);
        let name = fit_text(
            text_renderer,
            entry.author.name.as_str(),
            text_size,
            name_budget,
        );
        text_renderer.draw_text(
            scene,
            &name,
            text_size,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((x_cursor, cy - text_size as f64 / 2.0)),
            None,
        );
        let (name_w, _) = text_renderer.measure(&name, text_size, None);
        x_cursor += name_w as f64 + 6.0;

        // Identity short hash in dim text — disambiguates two
        // authors who share a display name. Dropped on a compact pane, where
        // the width it costs is worth more to the summary.
        if short_w > 0.0 {
            text_renderer.draw_text(
                scene,
                short,
                short_size,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((x_cursor, cy - short_size as f64 / 2.0)),
                None,
            );
        }

        // Op-class glyph at the end of the author-slot.
        let glyph = op_class_glyph(entry.op_class);
        let (glyph_w, _) = text_renderer.measure(glyph, text_size, None);
        let glyph_x = row.x0 + author_slot_w;
        text_renderer.draw_text(
            scene,
            glyph,
            text_size,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((glyph_x, cy - text_size as f64 / 2.0)),
            None,
        );
        let _ = glyph_w; // measured for symmetry; not used here

        // Summary fills the centre of the row. Phase 9: if the entry
        // has been redacted, the producer-supplied `summary` is
        // suppressed in favour of a struck-through "[redacted by …]"
        // label so the original content never re-renders, even
        // accidentally, on the audit pane.
        // The `[Revert]` chip is only painted on hover, so only a hovered row
        // pays for it. Reserving it on every row cost the summary — the row's
        // load-bearing text — a third of its width for something not on
        // screen.
        let chip_reserve = if is_hovered {
            self.revert_chip_w() + 6.0
        } else {
            0.0
        };
        let summary_x = glyph_x + 24.0;
        let summary_x_end = row.x1 - self.time_slot_w() - chip_reserve - 12.0;
        let (summary_owned, is_redacted_row): (String, bool) =
            if let Some(redaction) = entry.redaction.as_ref() {
                (
                    format!(
                        "[redacted by {} on {} \u{2014} {:?}]",
                        redaction.redactor_label,
                        format_relative_time(redaction.redacted_at, now),
                        redaction.reason,
                    ),
                    true,
                )
            } else {
                (entry.summary.clone(), false)
            };
        let summary = summary_owned.as_str();
        let drawn_summary = fit_text(
            text_renderer,
            summary,
            text_size,
            (summary_x_end - summary_x).max(0.0),
        );
        let summary_color = if is_redacted_row {
            theme.tokens.colors.text_dim
        } else {
            theme.tokens.colors.text_primary
        };
        text_renderer.draw_text(
            scene,
            &drawn_summary,
            text_size,
            summary_color,
            transform * Affine::translate((summary_x, cy - text_size as f64 / 2.0)),
            None,
        );
        // Strike-through line through the centre of the rendered
        // summary. Drawn after the text so a future regression that
        // forgets to gate on `is_redacted_row` paints a visible mark.
        if is_redacted_row {
            let (dw, _) = text_renderer.measure(&drawn_summary, text_size, None);
            let strike = Line::new(
                Point::new(summary_x, cy),
                Point::new(summary_x + dw as f64, cy),
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.tokens.colors.text_dim,
                None,
                &strike,
            );
        }

        // Relative timestamp slot, right-aligned within its width.
        let rel = fit_text(
            text_renderer,
            &format_relative_time(entry.timestamp, now),
            text_size,
            self.time_slot_w(),
        );
        let (tw, _) = text_renderer.measure(&rel, text_size, None);
        let time_x = row.x1 - chip_reserve - 12.0 - tw as f64;
        text_renderer.draw_text(
            scene,
            &rel,
            text_size,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((time_x, cy - text_size as f64 / 2.0)),
            None,
        );

        // `[Revert]` chip on hover. Two veto conditions:
        //   - op-class non-revertibility (Comment/Govern/Compaction/Redact);
        //   - Phase 9: the entry has been redacted — the chip's
        //     payload is forbidden by policy. We never offer "revert"
        //     across a redaction boundary.
        if is_hovered && op_class_is_revertible(entry.op_class) && !is_redacted_row {
            let chip_h = ROW_HEIGHT - 16.0;
            let chip_rect = Rect::new(
                row.x1 - self.revert_chip_w() - 6.0,
                cy - chip_h / 2.0,
                row.x1 - 6.0,
                cy + chip_h / 2.0,
            );
            let chip_shape = RoundedRect::from_rect(chip_rect, theme.tokens.radii.sm as f64);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surface_white_10,
                None,
                &chip_shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.tokens.colors.border_white_20,
                None,
                &chip_shape,
            );
            let label = fit_text(
                text_renderer,
                "Revert",
                text_size - 1.0,
                chip_rect.width() - 8.0,
            );
            let (lw, lh) = text_renderer.measure(&label, text_size - 1.0, None);
            text_renderer.draw_text(
                scene,
                &label,
                text_size - 1.0,
                theme.tokens.colors.text_primary,
                transform
                    * Affine::translate((
                        (chip_rect.x0 + chip_rect.x1) / 2.0 - lw as f64 / 2.0,
                        (chip_rect.y0 + chip_rect.y1) / 2.0 - lh as f64 / 2.0,
                    )),
                None,
            );
        }
    }

    /// Paint the filter chip strip + author typeahead. Returns the
    /// right edge of the chip cluster so callers know where the
    /// author input begins (the chrome doesn't currently render an
    /// input box — the typeahead is keyboard-only — but the rect is
    /// kept available for future polish).
    fn draw_filter_strip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let strip = self.filter_strip_rect();
        let chip_text_size = (theme.tokens.typography.scale.sm - 1.0).max(10.0);

        let active_fill = theme.tokens.colors.accent[5];
        let idle_fill = theme.tokens.colors.surface_white_5;
        let active_text = theme.tokens.colors.text_primary;
        let idle_text = theme.tokens.colors.text_dim;

        // Rebuild the chip-rect cache every frame so click hit-testing
        // (`handle_event`) lines up with what was just painted.
        self.chip_rects.clear();

        // The author pill is anchored to the right edge; reserve its width
        // first so the chips lay out into what is actually left rather than
        // sliding underneath it on a narrow pane.
        let author_label = if self.author_query.is_empty() {
            "author:any".to_string()
        } else {
            format!("author:{}", self.author_query)
        };
        let author_full_w =
            (text_renderer.measure(&author_label, chip_text_size, None).0 as f64 + 16.0)
                .min(strip.width() * 0.45);
        let chips_budget = (strip.width() - author_full_w - 8.0).max(0.0);

        // One measurement pass to decide whether the full labels fit. Every
        // chip switches together — a strip with a mix reads as a bug.
        let measure_run = |tr: &mut TextRenderer, short: bool| -> f64 {
            let mut total = 0.0;
            for chip in ChipKind::ALL {
                let l = if short { chip.short_label() } else { chip.label() };
                total += tr.measure(l, chip_text_size, None).0 as f64 + 16.0 + 6.0;
            }
            total
        };
        let use_short = measure_run(text_renderer, false) > chips_budget;

        let mut x = strip.x0;
        for chip in ChipKind::ALL {
            let label = if use_short {
                chip.short_label()
            } else {
                chip.label()
            };
            let (w, _) = text_renderer.measure(label, chip_text_size, None);
            let chip_w = w as f64 + 16.0;
            let chip_h = (strip.height() - 8.0).max(20.0);
            let cy = (strip.y0 + strip.y1) / 2.0;
            // Even the short labels can overrun a very narrow pane. Stop
            // rather than paint a chip half-under the author pill; the
            // remaining classes stay reachable from a wider window and,
            // crucially, an unpainted chip is not in `chip_rects` so it
            // cannot be clicked either.
            if x + chip_w > strip.x0 + chips_budget && !self.chip_rects.is_empty() {
                break;
            }
            let chip_rect = Rect::new(x, cy - chip_h / 2.0, x + chip_w, cy + chip_h / 2.0);
            self.chip_rects.push((chip, chip_rect));
            let active = self.active_chip == chip;
            let body = if active { active_fill } else { idle_fill };
            let text_color = if active { active_text } else { idle_text };
            let shape = RoundedRect::from_rect(chip_rect, chip_h / 2.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                body,
                None,
                &shape,
            );
            let (lw, lh) = text_renderer.measure(label, chip_text_size, None);
            text_renderer.draw_text(
                scene,
                label,
                chip_text_size,
                text_color,
                transform
                    * Affine::translate((
                        chip_rect.x0 + (chip_rect.width() - lw as f64) / 2.0,
                        chip_rect.y0 + (chip_rect.height() - lh as f64) / 2.0,
                    )),
                None,
            );
            x += chip_w + 6.0;
        }

        // Show the typeahead author filter as a trailing pill so the
        // user always sees what's narrowed. Empty = "no filter"
        // placeholder. The query is ellipsised from the left-hand side of
        // the name, keeping the characters most recently typed visible.
        let cy = (strip.y0 + strip.y1) / 2.0;
        let chip_h = (strip.height() - 8.0).max(20.0);
        // Anchor to the right edge so the chip strip degrades
        // gracefully when the pane is narrow.
        let arx1 = strip.x1;
        let arx0 = arx1 - author_full_w;
        let chip_rect = Rect::new(arx0, cy - chip_h / 2.0, arx1, cy + chip_h / 2.0);
        self.author_pill_rect = chip_rect;
        let drawn_author = fit_text(
            text_renderer,
            &author_label,
            chip_text_size,
            chip_rect.width() - 12.0,
        );
        let body = if self.author_query.is_empty() {
            idle_fill
        } else {
            active_fill
        };
        let text_color = if self.author_query.is_empty() {
            idle_text
        } else {
            active_text
        };
        let shape = RoundedRect::from_rect(chip_rect, chip_h / 2.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            body,
            None,
            &shape,
        );
        let (lw, lh) = text_renderer.measure(&drawn_author, chip_text_size, None);
        text_renderer.draw_text(
            scene,
            &drawn_author,
            chip_text_size,
            text_color,
            transform
                * Affine::translate((
                    chip_rect.x0 + (chip_rect.width() - lw as f64) / 2.0,
                    chip_rect.y0 + (chip_rect.height() - lh as f64) / 2.0,
                )),
            None,
        );
    }
}

/// Shorten `s` until it fits `budget` pixels at `size`, appending an ellipsis
/// when anything was dropped.
///
/// Binary chop on the char index; `measure` is cheap and the common case (a
/// short summary in a wide row) exits on the first measurement. Returns an
/// empty string when not even one character fits, which is the honest answer
/// for a slot that has been squeezed to nothing.
fn fit_text(tr: &mut TextRenderer, s: &str, size: f32, budget: f64) -> String {
    if budget <= 0.0 {
        return String::new();
    }
    if tr.measure(s, size, None).0 as f64 <= budget {
        return s.to_string();
    }
    const ELLIPSIS: &str = "\u{2026}";
    let ell_w = tr.measure(ELLIPSIS, size, None).0 as f64;
    if ell_w > budget {
        return String::new();
    }
    let inner = budget - ell_w;
    let mut lo = 0usize;
    let mut hi = s.chars().count();
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate: String = s.chars().take(mid).collect();
        if tr.measure(&candidate, size, None).0 as f64 <= inner {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        return ELLIPSIS.to_string();
    }
    let mut out: String = s.chars().take(lo).collect();
    out.push_str(ELLIPSIS);
    out
}

impl Widget for ActivityTimeline {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {
        // Entries arrive via the producer's push API. The widget is
        // a viewer — it does not pull from a CRDT path.
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let prev = self.rect;
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        if prev != self.rect {
            self.dirty = true;
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Pane surface — raised card on the shell background. Skipped when the
        // host has already drawn one.
        if self.draws_own_surface {
            let shape = RoundedRect::from_rect(self.rect, theme.tokens.radii.md as f64);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.raised,
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
        }

        self.draw_filter_strip(scene, transform, text_renderer, theme);

        // Empty state. Painted *after* the strip so it doesn't overlap the
        // chips. Two distinct cases — a room nothing has happened in, and a
        // room whose activity the filter is hiding — because a pane that
        // renders those identically reads as broken in the second case.
        if self.visible.is_empty() {
            let (title, detail) = self.empty_state_lines();
            let size = theme.tokens.typography.scale.sm;
            let small = (size - 1.0).max(10.0);
            let rows_top = self.rows_top();
            let cx = (self.rect.x0 + self.rect.x1) / 2.0;
            let cy = (rows_top + self.rect.y1 - PADDING) / 2.0;
            let budget = (self.rect.width() - 2.0 * PADDING - 16.0).max(0.0);

            let title = fit_text(text_renderer, &title, size, budget);
            let (w, h) = text_renderer.measure(&title, size, None);
            let title_y = if detail.is_some() {
                cy - h as f64 - 14.0
            } else {
                cy - h as f64 / 2.0
            };
            text_renderer.draw_text(
                scene,
                &title,
                size,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((cx - w as f64 / 2.0, title_y)),
                None,
            );

            self.clear_chip_rect = Rect::ZERO;
            if let Some(detail) = detail {
                let detail = fit_text(text_renderer, &detail, small, budget);
                let (dw, dh) = text_renderer.measure(&detail, small, None);
                let detail_y = title_y + h as f64 + 8.0;
                text_renderer.draw_text(
                    scene,
                    &detail,
                    small,
                    theme.tokens.colors.text_dim,
                    transform * Affine::translate((cx - dw as f64 / 2.0, detail_y)),
                    None,
                );

                // `[Clear filters]` — the way back out. Without it the user
                // has to remember which chip they pressed to get here.
                let label = "Clear filters";
                let (lw, lh) = text_renderer.measure(label, small, None);
                let chip_w = (lw as f64 + 20.0).min(budget.max(24.0));
                let chip_h = lh as f64 + 10.0;
                let chip_y0 = detail_y + dh as f64 + 12.0;
                if chip_y0 + chip_h <= self.rect.y1 - PADDING {
                    let chip = Rect::new(
                        cx - chip_w / 2.0,
                        chip_y0,
                        cx + chip_w / 2.0,
                        chip_y0 + chip_h,
                    );
                    self.clear_chip_rect = chip;
                    let shape = RoundedRect::from_rect(chip, chip_h / 2.0);
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        theme.tokens.colors.surface_white_10,
                        None,
                        &shape,
                    );
                    scene.stroke(
                        &Stroke::new(1.0),
                        transform,
                        theme.tokens.colors.border_white_20,
                        None,
                        &shape,
                    );
                    text_renderer.draw_text(
                        scene,
                        label,
                        small,
                        theme.tokens.colors.text_primary,
                        transform
                            * Affine::translate((
                                chip.x0 + (chip.width() - lw as f64) / 2.0,
                                chip.y0 + (chip.height() - lh as f64) / 2.0,
                            )),
                        None,
                    );
                }
            }
            self.dirty = false;
            return;
        }
        self.clear_chip_rect = Rect::ZERO;

        // Paint only the rows the viewport can actually show. The range comes
        // out of the scroll offset, so a room with ten thousand entries costs
        // the same per frame as one with ten.
        let now = self.now();
        for i in self.visible_row_range() {
            let Some(row) = self.row_rect(i) else {
                continue;
            };
            let entry = &self.entries[self.visible[i]];
            let is_hovered = self.hover_row == Some(i);
            self.draw_row(scene, transform, text_renderer, theme, row, entry, is_hovered, now);
        }

        self.dirty = false;
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _root: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            // Author typeahead. Printable characters and Backspace narrow the
            // list by contributor as they are typed; the pill in the chip
            // strip is the visible echo. Chorded keys are left alone so the
            // host's own shortcuts still work while the pane has the keyboard.
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if !modifiers.state().control_key()
                && !modifiers.state().super_key()
                && !modifiers.state().alt_key() =>
            {
                match logical_key {
                    Key::Character(c) => {
                        let text: String =
                            c.chars().filter(|ch| !ch.is_control()).collect();
                        if !text.is_empty() {
                            self.author_query.push_str(&text);
                            self.rebuild();
                            *handled = true;
                        }
                    }
                    Key::Named(NamedKey::Space) => {
                        self.author_query.push(' ');
                        self.rebuild();
                        *handled = true;
                    }
                    Key::Named(NamedKey::Backspace) => {
                        if self.author_query.pop().is_some() {
                            self.rebuild();
                            *handled = true;
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = p;
                let new_hover = self.row_at(p);
                if new_hover != self.hover_row {
                    self.hover_row = new_hover;
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // The `[Clear filters]` chip only exists while the
                // filtered-to-nothing empty state is painted, and the rect is
                // zeroed the moment it is not — so this can never fire on a
                // stale hit zone.
                if self.clear_chip_rect.area() > 0.0
                    && self.clear_chip_rect.contains(self.last_cursor)
                {
                    self.clear_filters();
                    self.dirty = true;
                    *handled = true;
                    return;
                }

                // Check whether the press landed on a chip first —
                // the chip strip steals the click before falling
                // through to the row hit-test. `chip_rects` is
                // populated by `draw_filter_strip` so the hit-zones
                // match what's painted byte-for-byte.
                let strip = self.filter_strip_rect();
                if strip.contains(self.last_cursor) {
                    for (kind, rect) in &self.chip_rects {
                        if rect.contains(self.last_cursor) {
                            if self.active_chip != *kind {
                                self.active_chip = *kind;
                                self.rebuild();
                                self.dirty = true;
                            }
                            *handled = true;
                            return;
                        }
                    }
                    // The author pill clears the typeahead — the only way back
                    // from a mistyped name without reaching for backspace.
                    if self.author_pill_rect.contains(self.last_cursor) {
                        if !self.author_query.is_empty() {
                            self.author_query.clear();
                            self.rebuild();
                            self.dirty = true;
                        }
                        *handled = true;
                        return;
                    }
                    // Click inside the strip but not on a chip — eat
                    // it so the row hit-test below doesn't fire.
                    return;
                }
                if let Some(i) = self.hover_row
                    && let Some(chip) = self.revert_chip_rect(i)
                    && chip.contains(self.last_cursor)
                    && let Some(entry) = self.visible_entry(i)
                {
                    let id = entry.id;
                    if let Some(cb) = self.on_revert.as_mut() {
                        cb(&id);
                    }
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                const LINE_PX: f64 = 60.0;
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -(*y as f64) * LINE_PX,
                    MouseScrollDelta::PixelDelta(pp) => -pp.y,
                };
                let new_scroll = (self.scroll_y + dy).clamp(0.0, self.max_scroll());
                if (new_scroll - self.scroll_y).abs() > f64::EPSILON {
                    self.scroll_y = new_scroll;
                    self.dirty = true;
                    *handled = true;
                    // After the scroll, the row under the cursor may
                    // have changed — re-evaluate.
                    self.hover_row = self.row_at(self.last_cursor);
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "ActivityTimeline"
    }

    /// Accessibility: the timeline is a labelled list of history
    /// entries. The accessible name is fixed ("Activity timeline"); the
    /// description carries the count of currently-visible (post-filter)
    /// entries so a screen reader can announce "Activity timeline, 7 items".
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        Some(
            AccessNode::new(Role::List)
                .with_name("Activity timeline")
                .with_description(format!("{} entries", self.visible.len())),
        )
    }

    fn test_id(&self) -> Option<&str> {
        Some("activity_timeline")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;
    use collab_types::ArtifactKind;
    use winit::event::Modifiers;
    use crate::widget::history::HistoryAuthor;

    fn entry(op: OpClass, author: &str, ts: u64, id: u8) -> HistoryEntry {
        let mut id_bytes = [0u8; 16];
        id_bytes[0] = id;
        HistoryEntry {
            id: HistoryEntryId(id_bytes),
            author: HistoryAuthor::new(author, 120.0, "deadbeef"),
            op_class: op,
            artifact_kind: ArtifactKind::Note,
            timestamp: SystemTime::UNIX_EPOCH + Duration::from_secs(ts),
            summary: format!("op {} by {}", id, author),
            diff_payload: None,
            redaction: None,
        }
    }

    fn timeline_at(rect: Rect, entries: Vec<HistoryEntry>) -> ActivityTimeline {
        let mut t = ActivityTimeline::with_entries(entries);
        t.rect = rect;
        t.set_now_for_test(SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000));
        t.set_newest_first(false); // tests use ascending order for clarity
        t
    }

    #[test]
    fn accessibility_reports_list_role_name_and_count() {
        use crate::a11y::Role;
        let entries = vec![
            entry(OpClass::Splice, "Alice", 10, 1),
            entry(OpClass::Splice, "Bob", 20, 2),
        ];
        let t = timeline_at(Rect::new(0.0, 0.0, 300.0, 400.0), entries);
        let node = Widget::accessibility(&t).expect("timeline is not presentational");
        assert_eq!(node.role, Role::List);
        assert_eq!(node.name.as_deref(), Some("Activity timeline"));
        // Both entries pass the (default, no-op) filter → "2 entries".
        assert_eq!(node.description.as_deref(), Some("2 entries"));
    }

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    /// Build a `KeyEvent` over a zeroed buffer — winit 0.30's `KeyEventExtra`
    /// is `pub(crate)`, so a struct literal is not available. Only the pub
    /// fields the widget reads are overwritten.
    fn key_press(logical_key: winit::keyboard::Key) -> WindowEvent {
        // SAFETY: every pub field of `KeyEvent` tolerates the all-zero bit
        // pattern — `logical_key` is overwritten below and `text` is an
        // `Option` whose niche-optimised `None` is all-zero.
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn mouse_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn dispatch(t: &mut ActivityTimeline, event: &WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        t.handle_event(
            event,
            1.0,
            &Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &tree,
        );
    }

    #[test]
    fn empty_timeline_paints_placeholder() {
        // No entries → the empty-state path must be reached. We
        // assert on the publicly-observable invariant
        // (`visible_len == 0`) plus the strip + rect geometry that
        // the draw path uses for placement. Pinning the literal
        // "No activity yet" string would make the test brittle for
        // future i18n changes; we'd rather pin the structural
        // contract.
        let t = timeline_at(Rect::new(0.0, 0.0, 400.0, 300.0), vec![]);
        assert_eq!(t.visible_len(), 0);
        // The strip sits at the top inside PADDING; no rows lay out
        // below it because there are no visible entries.
        let strip = t.filter_strip_rect();
        assert!(strip.y0 >= t.rect.y0);
        assert!(strip.y1 < t.rect.y1);
        // `row_rect` returns None for every index when the entry
        // list is empty.
        assert!(t.row_rect(0).is_none());
    }

    #[test]
    fn filter_by_op_class_hides_other_rows() {
        // Setting the chip to Comments must hide every non-Comment
        // row. Drives the chrome's chip-click path through
        // `set_active_chip`.
        let entries = vec![
            entry(OpClass::Splice, "Alice", 1, 1),
            entry(OpClass::Comment, "Bob", 2, 2),
            entry(OpClass::Move, "Carol", 3, 3),
            entry(OpClass::Comment, "Dave", 4, 4),
        ];
        let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);
        assert_eq!(t.visible_len(), 4);
        t.set_active_chip(ChipKind::Comments);
        assert_eq!(t.visible_len(), 2);
        for i in 0..t.visible_len() {
            assert_eq!(t.visible_entry(i).unwrap().op_class, OpClass::Comment);
        }
    }

    #[test]
    fn filter_by_author_typeahead_narrows_list() {
        // The author typeahead is case-insensitive substring match.
        // Two rows authored by "Alice" + "Alicia" should both match
        // "ali"; a row by "Bob" should not.
        let entries = vec![
            entry(OpClass::Splice, "Alice", 1, 1),
            entry(OpClass::Splice, "Alicia", 2, 2),
            entry(OpClass::Splice, "Bob", 3, 3),
        ];
        let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);
        t.set_author_query("ali");
        assert_eq!(t.visible_len(), 2);
        t.set_author_query("");
        assert_eq!(t.visible_len(), 3);
    }

    #[test]
    fn revert_callback_fires_on_chip_click() {
        // End-to-end: hover the row, press at the chip rect, the
        // callback fires with the row's entry id. Pins the
        // hover→hit-test→callback wire.
        let entries = vec![entry(OpClass::Splice, "Alice", 1, 42)];
        let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 200.0), entries);
        let captured: Rc<RefCell<Option<HistoryEntryId>>> = Rc::new(RefCell::new(None));
        let captured_clone = captured.clone();
        t.on_revert = Some(Box::new(move |id| {
            *captured_clone.borrow_mut() = Some(*id);
        }));

        // Move into the first row, then press inside the revert chip.
        let row = t.row_rect(0).expect("seeded row 0 must lay out");
        dispatch(&mut t, &cursor_moved(row.center().x, row.center().y));
        assert_eq!(t.hover_row(), Some(0));
        let chip = t.revert_chip_rect(0).expect("splice row must have a chip");
        // CursorMoved to the chip centre (re-asserts hover_row) then
        // press.
        dispatch(&mut t, &cursor_moved(chip.center().x, chip.center().y));
        dispatch(&mut t, &mouse_press());

        let mut id_bytes = [0u8; 16];
        id_bytes[0] = 42;
        assert_eq!(*captured.borrow(), Some(HistoryEntryId(id_bytes)));
    }

    #[test]
    fn hover_row_tracks_cursor_y() {
        // CursorMoved at row N's y should latch hover_row to N.
        // Three rows seeded; we move into row 1 + 2 and assert the
        // hover index follows.
        let entries = vec![
            entry(OpClass::Splice, "Alice", 1, 1),
            entry(OpClass::Splice, "Alice", 2, 2),
            entry(OpClass::Splice, "Alice", 3, 3),
        ];
        let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);
        let row1 = t.row_rect(1).unwrap();
        dispatch(&mut t, &cursor_moved(row1.center().x, row1.center().y));
        assert_eq!(t.hover_row(), Some(1));
        let row2 = t.row_rect(2).unwrap();
        dispatch(&mut t, &cursor_moved(row2.center().x, row2.center().y));
        assert_eq!(t.hover_row(), Some(2));
        // Move out of the rows entirely. Cache the y bound first so
        // the borrow checker doesn't see a live `&t` while we hand
        // `&mut t` to `dispatch`.
        let far_y = t.rect.y1 + 1000.0;
        dispatch(&mut t, &cursor_moved(0.0, far_y));
        assert_eq!(t.hover_row(), None);
    }

    #[test]
    fn non_revertible_op_classes_hide_chip() {
        // Comment + Govern rows must never expose a [Revert] chip.
        // Hover the row, ask for the chip rect — it should be None.
        let entries = vec![
            entry(OpClass::Comment, "Alice", 1, 1),
            entry(OpClass::Govern, "Bob", 2, 2),
            entry(OpClass::Splice, "Carol", 3, 3),
        ];
        let t = timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);
        assert!(t.revert_chip_rect(0).is_none(), "Comment must hide chip");
        assert!(t.revert_chip_rect(1).is_none(), "Govern must hide chip");
        assert!(
            t.revert_chip_rect(2).is_some(),
            "Splice must expose chip for hit-test"
        );
    }

    #[test]
    fn chip_strip_active_state_round_trips() {
        // Pin the active-chip getter/setter — the chrome wires this
        // to its chip-click handler.
        let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 200.0), vec![]);
        assert_eq!(t.active_chip(), ChipKind::All);
        t.set_active_chip(ChipKind::Governance);
        assert_eq!(t.active_chip(), ChipKind::Governance);
        // Switching back to All restores "no chip narrowing": every
        // op class is visible.
        let entries = vec![
            entry(OpClass::Comment, "Alice", 1, 1),
            entry(OpClass::Govern, "Bob", 2, 2),
        ];
        t.set_entries(entries);
        assert_eq!(t.visible_len(), 1, "Governance chip narrows to govern row");
        t.set_active_chip(ChipKind::All);
        assert_eq!(t.visible_len(), 2);
    }

    #[test]
    fn chip_strip_click_toggles_active_chip() {
        // Phase 1 follow-up (task #291): chip strip click hit-testing.
        // Without a real paint pass the chip_rects cache stays empty,
        // so seed it directly with a synthetic layout matching what
        // `draw_filter_strip` would have produced — five 60×24 chips
        // packed horizontally at the top of the widget.
        let entries = vec![
            entry(OpClass::Splice, "Alice", 1, 1),
            entry(OpClass::Comment, "Bob", 2, 2),
            entry(OpClass::Govern, "Carol", 3, 3),
        ];
        let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);

        // Synthesize chip rects (paint normally populates these).
        let mut x = PADDING;
        let cy = PADDING + FILTER_STRIP_HEIGHT / 2.0;
        for chip in ChipKind::ALL {
            let rect = Rect::new(x, cy - 12.0, x + 60.0, cy + 12.0);
            t.chip_rects.push((chip, rect));
            x += 66.0;
        }

        // ChipKind::ALL order: All, Edits, Comments, Governance, ....
        // Click on the "Comments" chip — index 2, x range 132..192.
        // Move cursor first so handle_event has a valid last_cursor.
        dispatch(&mut t, &cursor_moved(162.0, cy));
        dispatch(&mut t, &mouse_press());
        assert_eq!(
            t.active_chip(),
            ChipKind::Comments,
            "clicking the Comments chip should activate it",
        );
        // visible should narrow to Comment-only rows.
        assert_eq!(t.visible_len(), 1, "narrowed to the one Comment row");

        // Click on the "All" chip — index 0, x range 0..60.
        dispatch(&mut t, &cursor_moved(30.0, cy));
        dispatch(&mut t, &mouse_press());
        assert_eq!(t.active_chip(), ChipKind::All);
        assert_eq!(t.visible_len(), 3, "All restores every row");

        // Click inside the strip but between chips (gap at x=62..66) —
        // active_chip stays put.
        dispatch(&mut t, &cursor_moved(63.0, cy));
        dispatch(&mut t, &mouse_press());
        assert_eq!(
            t.active_chip(),
            ChipKind::All,
            "click in the gap between chips is a no-op",
        );
    }

    #[test]
    fn entries_sharing_a_timestamp_still_order_newest_first() {
        // Automerge stamps in whole seconds, so anything done in a burst ties.
        // Newest-first has to fall back to causal order, not to input order —
        // which would render the list upside down.
        let entries: Vec<HistoryEntry> = (0..5)
            .map(|i| entry(OpClass::Splice, "Alice", 100, i as u8))
            .collect();
        let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);
        t.set_newest_first(true);
        let ids: Vec<u8> = (0..t.visible_len())
            .map(|i| t.visible_entry(i).unwrap().id.0[0])
            .collect();
        assert_eq!(
            ids,
            vec![4, 3, 2, 1, 0],
            "the last change the producer emitted is the newest"
        );

        t.set_newest_first(false);
        let ids: Vec<u8> = (0..t.visible_len())
            .map(|i| t.visible_entry(i).unwrap().id.0[0])
            .collect();
        assert_eq!(ids, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn only_the_visible_window_of_a_huge_list_is_touched_per_frame() {
        // Ten thousand rows in a 600 px viewport. The paint and hit-test
        // paths must look at the fifteen-odd rows on screen, not at all ten
        // thousand — this is the difference between a room-scale history
        // that scrolls and one that drops frames.
        let entries: Vec<HistoryEntry> = (0..10_000)
            .map(|i| entry(OpClass::Splice, "Alice", i as u64, (i % 251) as u8))
            .collect();
        let t = timeline_at(Rect::new(0.0, 0.0, 600.0, 600.0), entries);
        assert_eq!(t.visible_len(), 10_000, "every row passes the filter");

        let range = t.visible_row_range();
        let viewport_rows = (t.viewport_height() / ROW_HEIGHT).ceil() as usize;
        assert!(
            range.len() <= viewport_rows + 2,
            "painted {} rows for a viewport that holds {}",
            range.len(),
            viewport_rows
        );
        assert_eq!(range.start, 0, "unscrolled, the window starts at the top");

        // Every row inside the window lays out; the first one past it does not.
        for i in range.clone() {
            assert!(t.row_rect(i).is_some(), "row {i} is on screen");
        }
        assert!(
            t.row_rect(range.end + 4).is_none(),
            "a row well past the window must not lay out"
        );
    }

    #[test]
    fn row_at_agrees_with_row_rect_at_the_boundaries() {
        // `row_at` no longer scans; it computes. The two must still answer
        // the same question, including one pixel either side of a seam.
        let entries: Vec<HistoryEntry> = (0..40)
            .map(|i| entry(OpClass::Splice, "Alice", i as u64, i as u8))
            .collect();
        let t = timeline_at(Rect::new(0.0, 0.0, 500.0, 500.0), entries);
        for i in 0..8 {
            let r = t.row_rect(i).unwrap();
            assert_eq!(t.row_at(r.center()), Some(i));
            assert_eq!(t.row_at(Point::new(r.center().x, r.y0 + 0.5)), Some(i));
            assert_eq!(
                t.row_at(Point::new(r.center().x, r.y1 - 0.5)),
                Some(i),
                "the bottom edge of row {i} still belongs to it"
            );
        }
        // Left of the row list's inset, and below the last row.
        let r0 = t.row_rect(0).unwrap();
        assert_eq!(t.row_at(Point::new(r0.x0 - 4.0, r0.center().y)), None);
        assert_eq!(t.row_at(Point::new(250.0, 499.9)), None);
    }

    #[test]
    fn an_empty_history_and_a_filtered_one_explain_themselves_differently() {
        let empty = timeline_at(Rect::new(0.0, 0.0, 400.0, 300.0), vec![]);
        let (title, detail) = empty.empty_state_lines();
        assert_eq!(title, "No activity yet");
        assert!(detail.is_none(), "nothing happened; nothing to explain");
        assert!(!empty.is_filtered());

        let mut t = timeline_at(
            Rect::new(0.0, 0.0, 400.0, 300.0),
            vec![
                entry(OpClass::Splice, "Alice", 1, 1),
                entry(OpClass::Splice, "Bob", 2, 2),
            ],
        );
        t.set_active_chip(ChipKind::Comments);
        assert_eq!(t.visible_len(), 0);
        assert!(t.is_filtered());
        let (title, detail) = t.empty_state_lines();
        assert_eq!(title, "No activity matches these filters");
        let detail = detail.expect("a filtered-empty list must say why");
        assert!(detail.contains("2 hidden"), "counts what it is hiding: {detail}");
        assert!(detail.contains("COMMENTS"), "names the filter: {detail}");
    }

    #[test]
    fn a_host_time_range_is_named_in_the_explanation() {
        // The widget cannot put a `SystemTime` pair into the user's words, so
        // the host labels it. Without the label the row would read "a date
        // range", which is still honest but less use.
        let mut t = timeline_at(
            Rect::new(0.0, 0.0, 400.0, 300.0),
            vec![entry(OpClass::Splice, "Alice", 1, 1)],
        );
        t.set_filter(HistoryFilter {
            time_range: Some((
                SystemTime::UNIX_EPOCH + Duration::from_secs(500),
                SystemTime::UNIX_EPOCH + Duration::from_secs(600),
            )),
            ..Default::default()
        });
        assert_eq!(t.visible_len(), 0);
        assert!(t.active_filter_summary().contains("a date range"));
        t.set_time_range_label(Some("Last 7 days".to_string()));
        assert!(
            t.active_filter_summary().contains("Last 7 days"),
            "got {:?}",
            t.active_filter_summary()
        );
    }

    #[test]
    fn clear_filters_resets_the_widget_and_tells_the_host() {
        let entries = vec![
            entry(OpClass::Splice, "Alice", 1, 1),
            entry(OpClass::Comment, "Bob", 2, 2),
        ];
        let cleared = Rc::new(RefCell::new(0usize));
        let c = cleared.clone();
        let mut t = ActivityTimeline::with_entries(entries).on_clear_filters(move || {
            *c.borrow_mut() += 1;
        });
        t.set_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
        t.set_active_chip(ChipKind::Governance);
        t.set_author_query("zzz");
        assert_eq!(t.visible_len(), 0);

        t.clear_filters();
        assert_eq!(t.active_chip(), ChipKind::All);
        assert_eq!(t.author_query(), "");
        assert_eq!(t.visible_len(), 2, "every row is back");
        assert_eq!(
            *cleared.borrow(),
            1,
            "the host is told so it can drop the filters it owns"
        );
    }

    #[test]
    fn typing_narrows_by_author_and_backspace_widens_again() {
        let entries = vec![
            entry(OpClass::Splice, "Alice", 1, 1),
            entry(OpClass::Splice, "Bob", 2, 2),
        ];
        let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 300.0), entries);
        dispatch(&mut t, &key_press(winit::keyboard::Key::Character("b".into())));
        assert_eq!(t.author_query(), "b");
        assert_eq!(t.visible_len(), 1);
        assert_eq!(t.visible_entry(0).unwrap().author.name, "Bob");
        dispatch(&mut t, &key_press(winit::keyboard::Key::Named(NamedKey::Backspace)));
        assert_eq!(t.author_query(), "");
        assert_eq!(t.visible_len(), 2);
    }

    #[test]
    fn narrow_panes_shrink_the_trailing_slots_rather_than_overlapping_them() {
        // On a narrow pane the revert chip must still sit inside the row and
        // leave the summary somewhere to go.
        let entries = vec![entry(OpClass::Splice, "Alice", 1, 1)];
        let wide = timeline_at(Rect::new(0.0, 0.0, 700.0, 300.0), entries.clone());
        let narrow = timeline_at(Rect::new(0.0, 0.0, 300.0, 300.0), entries);

        let wide_chip = wide.revert_chip_rect(0).unwrap();
        let narrow_chip = narrow.revert_chip_rect(0).unwrap();
        assert!(
            narrow_chip.width() < wide_chip.width(),
            "the chip gives width back on a narrow pane"
        );
        let narrow_row = narrow.row_rect(0).unwrap();
        assert!(
            narrow_chip.x0 > narrow_row.x0 + narrow.author_slot_w(),
            "the chip must not overlap the author cluster"
        );
        assert!(narrow_chip.x1 <= narrow_row.x1, "and must stay inside the row");
    }

    #[test]
    fn newest_first_reverses_chronological_order() {
        // With `newest_first = true`, the most recent entry must
        // surface first in the visible list. Pins the default
        // sort.
        let entries = vec![
            entry(OpClass::Splice, "Alice", 100, 1),
            entry(OpClass::Splice, "Alice", 200, 2),
            entry(OpClass::Splice, "Alice", 300, 3),
        ];
        let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);
        t.set_newest_first(true);
        assert_eq!(t.visible_entry(0).unwrap().timestamp.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs(), 300);
        assert_eq!(t.visible_entry(2).unwrap().timestamp.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs(), 100);
    }
}
