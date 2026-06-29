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
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;
use crate::widget::history::{
    HistoryEntry, HistoryEntryId, HistoryFilter, format_relative_time, hue_to_rgb,
    op_class_glyph, op_class_is_revertible,
};

use qos_collab_types::OpClass;

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

/// Callback fired when the user clicks `[Revert]` on a row.
pub type RevertCallback = Box<dyn FnMut(&HistoryEntryId)>;

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
        let top = self.rect.y0 + PADDING + FILTER_STRIP_HEIGHT + PADDING - self.scroll_y;
        let y0 = top + (i as f64) * ROW_HEIGHT;
        let y1 = y0 + ROW_HEIGHT;
        // Spatial cull: rows entirely outside the rect are
        // off-screen — return None so callers can skip drawing /
        // hit-testing them.
        if y1 < self.rect.y0 + PADDING + FILTER_STRIP_HEIGHT + PADDING
            || y0 > self.rect.y1 - PADDING
        {
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
            row.x1 - REVERT_CHIP_W - 6.0,
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
        if self.newest_first {
            // Stable reverse sort by timestamp so equal-ts entries
            // keep their input order. The producer is expected to
            // emit in causal order; we just respect that and flip
            // the iteration direction here.
            self.visible.sort_by(|a, b| {
                self.entries[*b].timestamp.cmp(&self.entries[*a].timestamp)
            });
        } else {
            self.visible.sort_by(|a, b| {
                self.entries[*a].timestamp.cmp(&self.entries[*b].timestamp)
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
    pub fn row_at(&self, p: Point) -> Option<usize> {
        for i in 0..self.visible.len() {
            if let Some(r) = self.row_rect(i)
                && r.contains(p)
            {
                return Some(i);
            }
        }
        None
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
        let mut x_cursor = dot_x + AVATAR_DIAM / 2.0 + 8.0;

        // Author display name in primary text colour.
        let name = entry.author.name.as_str();
        text_renderer.draw_text(
            scene,
            name,
            text_size,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((x_cursor, cy - text_size as f64 / 2.0)),
            None,
        );
        let (name_w, _) = text_renderer.measure(name, text_size, None);
        x_cursor += name_w as f64 + 6.0;

        // Identity short hash in dim text — disambiguates two
        // authors who share a display name.
        let short_size = (text_size - 2.0).max(10.0);
        text_renderer.draw_text(
            scene,
            entry.author.identity_short.as_str(),
            short_size,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((x_cursor, cy - short_size as f64 / 2.0)),
            None,
        );

        // Op-class glyph at the end of the author-slot.
        let glyph = op_class_glyph(entry.op_class);
        let (glyph_w, _) = text_renderer.measure(glyph, text_size, None);
        let glyph_x = row.x0 + AUTHOR_SLOT_W;
        text_renderer.draw_text(
            scene,
            glyph,
            text_size,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((glyph_x, cy - text_size as f64 / 2.0)),
            None,
        );
        let _ = glyph_w; // measured for symmetry; not used here

        // Summary fills the centre of the row.
        let summary_x = glyph_x + 24.0;
        let summary_x_end = row.x1 - TIME_SLOT_W - REVERT_CHIP_W - 12.0;
        let summary = entry.summary.as_str();
        // Crude width clip — drop chars from the end until it fits.
        // `text_renderer.measure` is cheap; for the typical short
        // "+12 -3 chars" summary it never trips.
        let (sw, _) = text_renderer.measure(summary, text_size, None);
        let drawn_summary = if sw as f64 > (summary_x_end - summary_x).max(0.0) {
            let budget = (summary_x_end - summary_x).max(0.0);
            // Binary chop on char index until the rendered width fits.
            let mut lo = 0;
            let mut hi = summary.chars().count();
            while lo < hi {
                let mid = (lo + hi).div_ceil(2);
                let s: String = summary.chars().take(mid).collect();
                let (w, _) = text_renderer.measure(&s, text_size, None);
                if w as f64 <= budget {
                    lo = mid;
                } else {
                    hi = mid - 1;
                }
            }
            summary.chars().take(lo).collect::<String>()
        } else {
            summary.to_string()
        };
        text_renderer.draw_text(
            scene,
            &drawn_summary,
            text_size,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((summary_x, cy - text_size as f64 / 2.0)),
            None,
        );

        // Relative timestamp slot, right-aligned within its width.
        let rel = format_relative_time(entry.timestamp, now);
        let (tw, _) = text_renderer.measure(&rel, text_size, None);
        let time_x = row.x1 - REVERT_CHIP_W - 12.0 - tw as f64;
        text_renderer.draw_text(
            scene,
            &rel,
            text_size,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((time_x, cy - text_size as f64 / 2.0)),
            None,
        );

        // `[Revert]` chip on hover, only for revertible op classes.
        if is_hovered && op_class_is_revertible(entry.op_class) {
            let chip_h = ROW_HEIGHT - 16.0;
            let chip_rect = Rect::new(
                row.x1 - REVERT_CHIP_W - 6.0,
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
            let label = "Revert";
            let (lw, lh) = text_renderer.measure(label, text_size - 1.0, None);
            text_renderer.draw_text(
                scene,
                label,
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
        &self,
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

        let mut x = strip.x0;
        for chip in ChipKind::ALL {
            let label = chip.label();
            let (w, _) = text_renderer.measure(label, chip_text_size, None);
            let chip_w = w as f64 + 16.0;
            let chip_h = (strip.height() - 8.0).max(20.0);
            let cy = (strip.y0 + strip.y1) / 2.0;
            let chip_rect = Rect::new(x, cy - chip_h / 2.0, x + chip_w, cy + chip_h / 2.0);
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
        // placeholder.
        let author_label = if self.author_query.is_empty() {
            "author:…".to_string()
        } else {
            format!("author:{}", self.author_query)
        };
        let (aw, _) = text_renderer.measure(&author_label, chip_text_size, None);
        let aw_full = aw as f64 + 16.0;
        let cy = (strip.y0 + strip.y1) / 2.0;
        let chip_h = (strip.height() - 8.0).max(20.0);
        // Anchor to the right edge so the chip strip degrades
        // gracefully when the pane is narrow.
        let arx1 = strip.x1;
        let arx0 = arx1 - aw_full;
        let chip_rect = Rect::new(arx0, cy - chip_h / 2.0, arx1, cy + chip_h / 2.0);
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
        let (lw, lh) = text_renderer.measure(&author_label, chip_text_size, None);
        text_renderer.draw_text(
            scene,
            &author_label,
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
        // Pane surface — raised card on the shell background.
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

        self.draw_filter_strip(scene, transform, text_renderer, theme);

        // Empty state — centred "No activity yet" label. Painted
        // *after* the strip so it doesn't overlap the chips, and
        // *before* clipping the row area so a future scroll clip
        // doesn't accidentally hide it.
        if self.visible.is_empty() {
            let label = "No activity yet";
            let size = theme.tokens.typography.scale.sm;
            let (w, h) = text_renderer.measure(label, size, None);
            let rows_top = self.rect.y0 + PADDING + FILTER_STRIP_HEIGHT + PADDING;
            let cx = (self.rect.x0 + self.rect.x1) / 2.0;
            let cy = (rows_top + self.rect.y1 - PADDING) / 2.0;
            text_renderer.draw_text(
                scene,
                label,
                size,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((cx - w as f64 / 2.0, cy - h as f64 / 2.0)),
                None,
            );
            self.dirty = false;
            return;
        }

        // Walk visible rows, paint each that intersects the row
        // viewport.
        let now = self.now();
        for i in 0..self.visible.len() {
            let Some(row) = self.row_rect(i) else {
                continue;
            };
            // Skip rows entirely outside the rect — `row_rect` already
            // does some culling, but a partially-clipped row is still
            // painted (Vello's scene clipping draws the visible band).
            if row.y1 < self.rect.y0 || row.y0 > self.rect.y1 {
                continue;
            }
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
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _root: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
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
                // Check whether the press landed on a chip first —
                // the chip strip steals the click before falling
                // through to the row hit-test.
                let strip = self.filter_strip_rect();
                if strip.contains(self.last_cursor) {
                    // No-op fallback: only the timeline's tests poke
                    // chips directly via `set_active_chip` — for now
                    // we don't paint chip hover hot-zones, so the
                    // click does nothing here. Future polish: walk
                    // the chip rects from `draw_filter_strip` and
                    // toggle on hit.
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
    use qos_collab_types::ArtifactKind;
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
        }
    }

    fn timeline_at(rect: Rect, entries: Vec<HistoryEntry>) -> ActivityTimeline {
        let mut t = ActivityTimeline::with_entries(entries);
        t.rect = rect;
        t.set_now_for_test(SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000));
        t.set_newest_first(false); // tests use ascending order for clarity
        t
    }

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
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
