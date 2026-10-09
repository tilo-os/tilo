//! `ActivityTimeline` — the history pane, grouped.
//!
//! Renders history as scrolling **groups** ([`crate::widget::history_group`])
//! that expand to individual [`HistoryEntry`] rows, because typing coalesces
//! on a one-second debounce and an afternoon of work would otherwise be
//! hundreds of rows reading `+3 -0 chars`. A group of one unnamed change
//! renders as a plain row, so the flat list is the degenerate case rather
//! than a second code path.
//!
//! Naming is retroactive (click `[Name]`, or shift-click a run to name it).
//! The widget has no document, so naming only fires [`NameGroupCallback`];
//! the host persists it through `tilo_ui::history_source::groups`.
//!
//! The filter chip strip's grouping is computed over the *unfiltered*
//! history, so toggling a chip narrows the list without re-chunking it under
//! the reader.

use crate::time::SystemTime;
use crate::widget::ModelPath;
use std::collections::{HashMap, HashSet};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::history::{
    HistoryAuthor, HistoryEntry, HistoryEntryId, HistoryFilter, format_relative_time,
    op_class_glyph,
};
use crate::widget::history_group::{
    GroupNameRequest, GroupingConfig, HistoryGroup, NamedRange, group_entries,
};
use crate::widget::name_field::{NameField, NameFieldOutcome, fit as fit_text};

use crate::tokens::TextRole;
use collab_types::OpClass;

/// 40px: a 600px pane fits ~15 rows without scrolling.
pub const ROW_HEIGHT: f64 = 40.0;
pub const GROUP_ROW_HEIGHT: f64 = 52.0;
pub const FILTER_STRIP_HEIGHT: f64 = 36.0;
pub const SELECTION_BAR_H: f64 = 44.0;
const PADDING: f64 = 8.0;
const AVATAR_DIAM: f64 = 16.0;
const REVERT_CHIP_W: f64 = 64.0;
const TIME_SLOT_W: f64 = 80.0;
const AUTHOR_SLOT_W: f64 = 160.0;
const INDENT: f64 = 20.0;
/// Right-hand gutter shared by every trailing element — a row's relative
/// time and a band header's — so the time column lines up down the pane.
/// A constant alongside `PADDING` and `INDENT`, which the row geometry that
/// hit-testing reads is built from without a theme; pinned to `spacing.s3`
/// by `gutter_constants_equal_the_spacing_tokens`.
const TRAILING_INSET: f64 = 12.0;
/// Budget of auto-expanded rows: a short history should read as a flat
/// list, a long one has to read as collapsed bands. This is the line
/// between the two, spent newest-first.
const AUTO_EXPAND_BUDGET: usize = 14;
/// Past this many top-level bands, nothing auto-expands regardless of budget.
const DENSE_GROUP_COUNT: usize = 6;

/// The pane paints on `surfaces.raised` (its own card, or the host's), where
/// `Colors::lift` (base → raised) has nowhere left to go, so every tier
/// inside it — hovered row, band backdrop, selected row — is **one ordered
/// ramp** of translucent steps against that fill, polarity chosen the way
/// `hairline_on` chooses it (HIG §9.3 caveat, §9.5). Ascending: hover /
/// band rest, band hover, selected.
const WASH_HOVER: u8 = 26;
const WASH_BAND_HOVER: u8 = 40;
const WASH_SELECTED: u8 = 56;

/// Sibling filter chips keep one `s2` (HIG §7b: siblings of equal weight
/// in a compact strip); the 6 px they kept before was off the scale.
const CHIP_GAP: f64 = crate::tokens::Spacing::BASE.s2 as f64;

/// A lifted entry row (hover, selection) is inset this much top and bottom,
/// so two lifted neighbours keep one `s1` between them — the same clearance
/// a band's backdrop keeps from the rows around it — instead of fusing into
/// one block (HIG §7 "nothing touches").
const ROW_LIFT_INSET: f64 = crate::tokens::Spacing::BASE.s1 as f64 / 2.0;

/// One step of the pane's own ramp: `alpha` of white or black, whichever
/// moves `surface` further.
fn wash(surface: Color, alpha: u8) -> Color {
    crate::tokens::colors::Colors::hairline_on(surface, alpha)
}

/// A nesting depth, row count or dot count, widened for pixel-space layout
/// math. Lossless in practice — none of these ever approach `u32::MAX` — but
/// `usize -> f64` cannot express that to the compiler, so this goes through
/// `u32` (exactly representable in `f64`) with a saturating fallback rather
/// than an `as` cast.
fn px(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Composes a paint transform with a translation. `Affine`'s `Mul` impl is
/// pure `f64` matrix multiply-add with no overflow trap, so it cannot panic
/// regardless of operand magnitude — centralised here rather than repeating
/// the exemption at every text transform composed in this module.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix math and cannot panic"
)]
fn translated(transform: Affine, offset: (f64, f64)) -> Affine {
    transform * Affine::translate(offset)
}

/// Top edge that centres one line of `size`-px text on `center_y`. The line
/// box the renderer paints is `line_height × size` — a quarter taller than
/// the font size at the default 1.25 — and it carries the accessibility text
/// scale, so centring on the *measured* height (never on `size / 2`) is what
/// keeps a row's text on the same centre line as its avatar dot at every
/// type scale.
fn text_top(tr: &mut TextRenderer, style: &TextStyle, center_y: f64) -> f64 {
    center_y - f64::from(tr.measure_style("Ag", None, style).1) / 2.0
}

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

    pub fn label(&self) -> String {
        crate::i18n!(match self {
            ChipKind::All => "history-chip-all",
            ChipKind::Edits => "history-chip-edits",
            ChipKind::Comments => "history-chip-comments",
            ChipKind::Governance => "history-chip-access",
            ChipKind::Deletes => "history-chip-deletions",
        })
    }

    /// Used when the full label set will not fit; every chip switches
    /// together, or mixed lengths would read as a rendering accident.
    pub fn short_label(&self) -> String {
        crate::i18n!(match self {
            ChipKind::All => "history-chip-all",
            ChipKind::Edits => "history-chip-edits-short",
            ChipKind::Comments => "history-chip-comments-short",
            ChipKind::Governance => "history-chip-access-short",
            ChipKind::Deletes => "history-chip-deletions-short",
        })
    }

    /// `All` is empty because "no op-class filter" is how [`HistoryFilter`]
    /// expresses "show everything".
    pub fn op_classes(&self) -> Vec<OpClass> {
        match self {
            ChipKind::All => Vec::new(),
            // Tombstone/Restore live in "Deletes" instead.
            ChipKind::Edits => vec![OpClass::Splice, OpClass::Move, OpClass::EditMeta],
            ChipKind::Comments => vec![OpClass::Comment],
            ChipKind::Governance => vec![OpClass::Govern],
            ChipKind::Deletes => vec![OpClass::Tombstone, OpClass::Restore],
        }
    }
}

/// Below this width the trailing time + revert slots shrink so the summary
/// keeps a usable share of the row.
const NARROW_WIDTH: f64 = 340.0;

/// Fired on `[Revert]`, or on a group's `[Restore]` (passing the group's
/// **last** change — restoring to a boundary is the point of grouping).
pub type RevertCallback = Box<dyn FnMut(&HistoryEntryId)>;

/// Fired when a change row is activated with Enter, Space, or assistive
/// technology. The timeline owns selection; the host can use this to open a
/// detail view, move a scrubber, or otherwise inspect the selected moment.
pub type EntryActivateCallback = Box<dyn FnMut(&HistoryEntryId)>;

/// Fired on `[Clear filters]`, after the widget resets its own chip/query —
/// the host should drop any filter it owns itself (e.g. a date range).
pub type ClearFiltersCallback = Box<dyn FnMut()>;

/// Callback fired when the user commits a name.
pub type NameGroupCallback = Box<dyn FnMut(&GroupNameRequest)>;

/// What `[Undo]` on a band asks for: the **range**, not a single id like
/// `[Restore]` sends. Restore means "put the document back to this moment,
/// discarding everything after"; undo means "take this band out, leave the
/// rest alone" — a host cannot tell the two apart from one id.
#[derive(Clone, Debug, PartialEq)]
pub struct BandUndoRequest {
    pub start: HistoryEntryId,
    pub end: HistoryEntryId,
    /// What a confirmation should quote.
    pub label: Option<String>,
    /// First-seen order; what a host filters the revert by.
    pub authors: Vec<HistoryAuthor>,
}

/// Fired on `[Undo]`. Installing it is what makes the chip appear at all.
pub type UndoBandCallback = Box<dyn FnMut(&BandUndoRequest)>;

/// One line in the rendered list: a group header, or an individual change at a
/// nesting depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VisualRow {
    /// Index into `row_groups`.
    Group(usize),
    /// Index into `entries`, plus the depth it renders at.
    Entry(usize, usize),
}

/// Durable identity for keyboard/AT focus. Row indices are deliberately not
/// retained: expanding a band or receiving an earlier change changes them.
#[derive(Clone, Debug, PartialEq, Eq)]
enum FocusedRow {
    Group(String),
    Entry(HistoryEntryId),
}

/// A group, flattened and pre-measured for painting.
#[derive(Clone, Debug)]
struct RowGroup {
    key: String,
    title: String,
    subtitle: String,
    /// Compact cross-app/workspace provenance, shown on a collapsed band so
    /// its hidden members do not erase where the work happened.
    scope_label: Option<String>,
    name: Option<String>,
    record_id: Option<String>,
    open: bool,
    depth: usize,
    first_entry: HistoryEntryId,
    last_entry: HistoryEntryId,
    last_time: SystemTime,
    authors: Vec<HistoryAuthor>,
    expanded: bool,
    restorable: bool,
    /// Calendar heading (Today / Yesterday / …), not a collapsible band.
    is_section: bool,
}

/// A band header's hover chips, in the order they are laid out.
struct GroupChips {
    name: Rect,
    /// `None` unless a host installed a band-undo callback.
    undo: Option<Rect>,
    /// `None` when nothing in the band can be restored to.
    restore: Option<Rect>,
}

impl GroupChips {
    fn left_edge(&self, fallback: f64) -> f64 {
        self.undo
            .into_iter()
            .chain(self.restore)
            .chain((self.name.area() > 0.0).then_some(self.name))
            .map(|rect| rect.x0)
            .fold(fallback, f64::min)
    }
}

/// The field the user is typing a name into.
#[derive(Clone, Debug, PartialEq)]
enum NameTarget {
    /// Naming (or renaming) a band; index into `row_groups`.
    Group(usize),
    /// Naming the current selection.
    Selection,
}

#[derive(Clone, Debug)]
struct NamingState {
    target: NameTarget,
    field: NameField,
}

/// Scrolling, filterable, groupable activity pane. See module-level docs.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag tracks an independent interaction, rendering, or invalidation state"
)]
pub struct ActivityTimeline {
    entries: Vec<HistoryEntry>,
    named: Vec<NamedRange>,
    grouping: GroupingConfig,
    /// The forest, over the **unfiltered** entry list.
    groups: Vec<HistoryGroup>,
    /// Display order of the top-level groups, by each band's newest change.
    group_order: Vec<usize>,
    filter: HistoryFilter,
    /// Indexes (into `entries`) that pass the filter, in display order.
    visible: Vec<usize>,
    /// `row_tops[i]` is line `i`'s top; one extra trailing entry holds the
    /// total height. Rows are not a uniform pitch, so lookups binary-search
    /// this rather than dividing.
    rows: Vec<VisualRow>,
    row_tops: Vec<f64>,
    row_groups: Vec<RowGroup>,
    /// Groups the user explicitly opened or closed. Anything in neither set
    /// follows [`AUTO_EXPAND_BUDGET`].
    expanded: HashSet<String>,
    collapsed: HashSet<String>,
    /// Selected run of changes, as inclusive indices into `entries`.
    selection: Option<(usize, usize)>,
    selection_anchor: Option<usize>,
    focused_row: Option<FocusedRow>,
    has_focus: bool,
    naming: Option<NamingState>,
    active_chip: ChipKind,
    /// Free-text author typeahead. Lower-cased before matching.
    author_query: String,
    rect: Rect,
    scroll_y: f64,
    /// Index into `rows`.
    hover_row: Option<usize>,
    last_cursor: Point,
    on_revert: Option<RevertCallback>,
    on_activate_entry: Option<EntryActivateCallback>,
    on_undo_band: Option<UndoBandCallback>,
    on_name_group: Option<NameGroupCallback>,
    /// "Now" for the relative-timestamp paint. Tests + headless callers pin
    /// this so a snapshot stays stable.
    now_override: Option<SystemTime>,
    newest_first: bool,
    /// Populated by `draw_filter_strip` each frame so click hit-testing
    /// matches what was actually painted.
    chip_rects: Vec<(ChipKind, Rect)>,
    author_pill_rect: Rect,
    /// `metrics.min_pointer_target`, taken from the theme each `draw` so the
    /// theme-less hit-test accessors can grow a chip's target to it.
    min_pointer_target: f64,
    /// `RoomActivity` may supply its own Everyone/person selector; suppressing
    /// this duplicate leaves the op-class strip intact.
    show_author_pill: bool,
    /// Calendar Today/Yesterday/This week/Earlier headings above bands.
    show_day_headings: bool,
    /// `Rect::ZERO` whenever the filtered-to-nothing empty state is not on
    /// screen, so a stale click cannot trip `[Clear filters]`.
    clear_chip_rect: Rect,
    /// Last-drawn rects of the selection bar's two actions.
    name_selection_rect: Rect,
    clear_selection_rect: Rect,
    time_range_label: Option<String>,
    on_clear_filters: Option<ClearFiltersCallback>,
    /// `false` when the host has already painted a surface underneath.
    draws_own_surface: bool,
    preview_selection: bool,
    dirty: bool,
}

impl Default for ActivityTimeline {
    fn default() -> Self {
        Self::new()
    }
}

impl ActivityTimeline {
    /// Use [`Self::with_entries`] / [`Self::set_entries`] to seed it.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            named: Vec::new(),
            grouping: GroupingConfig::default(),
            groups: Vec::new(),
            group_order: Vec::new(),
            filter: HistoryFilter::default(),
            visible: Vec::new(),
            rows: Vec::new(),
            row_tops: vec![0.0],
            row_groups: Vec::new(),
            expanded: HashSet::new(),
            collapsed: HashSet::new(),
            selection: None,
            selection_anchor: None,
            focused_row: None,
            has_focus: false,
            naming: None,
            active_chip: ChipKind::All,
            author_query: String::new(),
            rect: Rect::ZERO,
            scroll_y: 0.0,
            hover_row: None,
            last_cursor: Point::ZERO,
            on_revert: None,
            on_activate_entry: None,
            on_undo_band: None,
            on_name_group: None,
            now_override: None,
            newest_first: true,
            chip_rects: Vec::new(),
            author_pill_rect: Rect::ZERO,
            min_pointer_target: f64::from(crate::tokens::Metrics::BASE.min_pointer_target),
            show_author_pill: true,
            show_day_headings: false,
            clear_chip_rect: Rect::ZERO,
            name_selection_rect: Rect::ZERO,
            clear_selection_rect: Rect::ZERO,
            time_range_label: None,
            on_clear_filters: None,
            draws_own_surface: true,
            preview_selection: false,
            dirty: true,
        }
    }

    /// Use selection/inspection without offering metadata or direct revert actions.
    #[must_use]
    pub fn with_preview_selection(mut self) -> Self {
        self.set_preview_selection(true);
        self
    }

    /// Enable inspection without mutation affordances. Revocation cancels an
    /// in-progress naming field before it can emit a metadata write intent.
    pub fn set_preview_selection(&mut self, preview: bool) {
        self.preview_selection = preview;
        if preview {
            self.naming = None;
        }
        self.dirty = true;
    }

    pub fn with_entries(entries: Vec<HistoryEntry>) -> Self {
        let mut s = Self::new();
        s.set_entries(entries);
        s
    }

    pub fn set_entries(&mut self, entries: Vec<HistoryEntry>) {
        let selected_ids: Vec<HistoryEntryId> = self
            .selection
            .and_then(|(start, end)| self.entries.get(start..=end))
            .map(|selected| selected.iter().map(|entry| entry.id).collect())
            .unwrap_or_default();
        self.entries = entries;
        self.selection = if selected_ids.is_empty() {
            None
        } else {
            let positions: Vec<usize> = selected_ids
                .iter()
                .filter_map(|id| self.entries.iter().position(|entry| entry.id == *id))
                .collect();
            let contiguous = positions.len() == selected_ids.len()
                && positions
                    .windows(2)
                    .all(|pair| matches!(pair, [left, right] if *right == left.saturating_add(1)));
            contiguous.then(|| {
                (
                    *positions.first().unwrap_or(&0),
                    *positions.last().unwrap_or(&0),
                )
            })
        };
        self.selection_anchor = self.selection.map(|(start, _)| start);
        self.regroup();
    }

    /// The widget has no document; the host reads names
    /// (`tilo_ui::history_source::groups`) and pushes them here.
    pub fn set_named_ranges(&mut self, named: Vec<NamedRange>) {
        if self.named != named {
            self.named = named;
            self.regroup();
        }
    }

    pub fn named_ranges(&self) -> &[NamedRange] {
        &self.named
    }

    pub fn set_grouping(&mut self, cfg: GroupingConfig) {
        if self.grouping != cfg {
            self.grouping = cfg;
            self.regroup();
        }
    }

    /// For a streaming producer. Call [`Self::refresh`] after mutating it.
    pub fn entries_mut(&mut self) -> &mut Vec<HistoryEntry> {
        &mut self.entries
    }

    /// Idempotent.
    pub fn refresh(&mut self) {
        self.regroup();
    }

    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// Over the unfiltered history.
    pub fn groups(&self) -> &[HistoryGroup] {
        &self.groups
    }

    /// For a host that reserves a rect out of its own layout rather than
    /// putting the widget in a taffy tree.
    pub fn set_rect(&mut self, rect: Rect) {
        if self.rect != rect {
            self.rect = rect;
            self.dirty = true;
        }
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn set_filter(&mut self, filter: HistoryFilter) {
        self.filter = filter;
        self.rebuild();
    }

    pub fn filter(&self) -> &HistoryFilter {
        &self.filter
    }

    /// Fires from a row's `[Revert]` chip and a group's `[Restore]` chip.
    #[must_use]
    pub fn on_revert<F: FnMut(&HistoryEntryId) + 'static>(mut self, f: F) -> Self {
        self.on_revert = Some(Box::new(f));
        self
    }

    /// Observe explicit activation of an individual activity entry. Selection
    /// is updated before this callback runs.
    #[must_use]
    pub fn on_activate_entry<F: FnMut(&HistoryEntryId) + 'static>(mut self, f: F) -> Self {
        self.on_activate_entry = Some(Box::new(f));
        self
    }

    /// Also puts the `[Undo]` chip on band headers.
    #[must_use]
    pub fn on_undo_band<F: FnMut(&BandUndoRequest) + 'static>(mut self, f: F) -> Self {
        self.on_undo_band = Some(Box::new(f));
        self
    }

    #[must_use]
    pub fn on_name_group<F: FnMut(&GroupNameRequest) + 'static>(mut self, f: F) -> Self {
        self.on_name_group = Some(Box::new(f));
        self
    }

    #[must_use]
    pub fn on_clear_filters<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_clear_filters = Some(Box::new(f));
        self
    }

    /// For a host that has already painted a surface behind it.
    pub fn set_draws_own_surface(&mut self, draws: bool) {
        if self.draws_own_surface != draws {
            self.draws_own_surface = draws;
            self.dirty = true;
        }
    }

    /// In the words the host's own control uses.
    pub fn set_time_range_label(&mut self, label: Option<String>) {
        if self.time_range_label != label {
            self.time_range_label = label;
            self.dirty = true;
        }
    }

    pub fn clear_filters(&mut self) {
        self.active_chip = ChipKind::All;
        self.author_query.clear();
        self.rebuild();
        if let Some(cb) = self.on_clear_filters.as_mut() {
            cb();
        }
    }

    pub fn is_filtered(&self) -> bool {
        self.active_chip != ChipKind::All
            || !self.author_query.trim().is_empty()
            || !self.filter.authors.is_empty()
            || !self.filter.op_classes.is_empty()
            || self.filter.time_range.is_some()
    }

    /// Empty string when nothing narrows.
    pub fn active_filter_summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.active_chip != ChipKind::All {
            parts.push(self.active_chip.label());
        }
        let chip_classes = self.active_chip.op_classes();
        for op in self
            .filter
            .op_classes
            .iter()
            .filter(|op| !chip_classes.contains(op))
        {
            let label = crate::history_source::op_class_label(*op);
            if !parts.contains(&label) {
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
        (
            "No activity matches these filters".to_string(),
            Some(detail),
        )
    }

    /// `Rect::ZERO` when the filtered-to-nothing empty state is not on
    /// screen.
    pub fn clear_filters_chip_rect(&self) -> Rect {
        self.clear_chip_rect
    }

    pub fn author_pill_rect(&self) -> Rect {
        self.author_pill_rect
    }

    /// Show or hide the built-in `author:any` pill. The default is `true` for
    /// standalone use; hosts with an equivalent people control can turn it
    /// off without losing the operation-class filters.
    pub fn set_show_author_pill(&mut self, show: bool) {
        if self.show_author_pill != show {
            self.show_author_pill = show;
            self.author_pill_rect = Rect::ZERO;
            self.dirty = true;
        }
    }

    pub fn shows_author_pill(&self) -> bool {
        self.show_author_pill
    }

    /// Insert Today / Yesterday / This week / Earlier headings above bands.
    pub fn set_show_day_headings(&mut self, show: bool) {
        if self.show_day_headings != show {
            self.show_day_headings = show;
            self.rebuild_rows();
        }
    }

    // ---------- groups: expansion, naming, selection ----------

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// `None` when the line is a change rather than a header.
    pub fn group_at_row(&self, i: usize) -> Option<(&str, bool)> {
        match self.rows.get(i)? {
            VisualRow::Group(g) => {
                let rg = self.row_groups.get(*g)?;
                Some((rg.key.as_str(), rg.expanded))
            }
            VisualRow::Entry(..) => None,
        }
    }

    pub fn is_day_heading_at(&self, i: usize) -> bool {
        match self.rows.get(i) {
            Some(VisualRow::Group(g)) => self.row_groups.get(*g).is_some_and(|rg| rg.is_section),
            _ => false,
        }
    }

    pub fn group_title_at_row(&self, i: usize) -> Option<&str> {
        match self.rows.get(i)? {
            VisualRow::Group(g) => Some(self.row_groups.get(*g)?.title.as_str()),
            VisualRow::Entry(..) => None,
        }
    }

    pub fn group_subtitle_at_row(&self, i: usize) -> Option<&str> {
        match self.rows.get(i)? {
            VisualRow::Group(g) => Some(self.row_groups.get(*g)?.subtitle.as_str()),
            VisualRow::Entry(..) => None,
        }
    }

    /// 0 for a top-level band or a loose change.
    pub fn row_depth_at(&self, i: usize) -> Option<usize> {
        (i < self.rows.len()).then_some(self.row_depth(i))
    }

    /// `None` when the line is a header.
    pub fn row_entry(&self, i: usize) -> Option<&HistoryEntry> {
        match self.rows.get(i)? {
            VisualRow::Entry(e, _) => self.entries.get(*e),
            VisualRow::Group(_) => None,
        }
    }

    pub fn row_entry_index(&self, i: usize) -> Option<usize> {
        match self.rows.get(i)? {
            VisualRow::Entry(e, _) => Some(*e),
            VisualRow::Group(_) => None,
        }
    }

    pub fn set_group_expanded(&mut self, key: &str, expanded: bool) {
        if expanded {
            self.collapsed.remove(key);
            self.expanded.insert(key.to_string());
        } else {
            self.expanded.remove(key);
            self.collapsed.insert(key.to_string());
        }
        self.rebuild_rows();
    }

    /// `false` when that line is not a header.
    pub fn toggle_group_at_row(&mut self, i: usize) -> bool {
        let Some(VisualRow::Group(g)) = self.rows.get(i).copied() else {
            return false;
        };
        let Some((key, expanded, is_section)) = self
            .row_groups
            .get(g)
            .map(|rg| (rg.key.clone(), rg.expanded, rg.is_section))
        else {
            return false;
        };
        if is_section {
            return false;
        }
        self.set_group_expanded(&key, !expanded);
        true
    }

    pub fn set_all_expanded(&mut self, expanded: bool) {
        self.expanded.clear();
        self.collapsed.clear();
        let mut keys: Vec<String> = Vec::new();
        for g in &self.groups {
            let mut flat: Vec<&HistoryGroup> = Vec::new();
            g.walk(&mut flat);
            keys.extend(flat.iter().map(|x| x.key.clone()));
        }
        for k in keys {
            if expanded {
                self.expanded.insert(k);
            } else {
                self.collapsed.insert(k);
            }
        }
        self.rebuild_rows();
    }

    /// Inclusive indices into [`Self::entries`].
    pub fn selection(&self) -> Option<(usize, usize)> {
        self.selection
    }

    /// Entry index range, inclusive, either order.
    pub fn set_selection(&mut self, range: Option<(usize, usize)>) {
        let norm = range.map(|(a, b)| (a.min(b), a.max(b)));
        if self.selection != norm {
            self.selection = norm;
            self.selection_anchor = norm.map(|(a, _)| a);
            self.rebuild_rows();
        }
    }

    /// The currently focused visible row. This is distinct from selection:
    /// band headers can receive focus but cannot be selected as changes.
    pub fn focused_row(&self) -> Option<usize> {
        self.focused_row_index()
    }

    /// The focused change, if focus is on an individual entry rather than a
    /// collapsible band header.
    pub fn focused_entry(&self) -> Option<&HistoryEntry> {
        self.focused_row().and_then(|row| self.row_entry(row))
    }

    /// Geometry for an accessibility child, including off-viewport rows so a
    /// ScrollIntoView/Focus request can move the roving focus before paint.
    pub fn accessibility_row_rect(&self, row: usize) -> Option<Rect> {
        self.row_rect_unclipped(row)
    }

    /// Put keyboard/assistive-technology focus on a visible row and scroll it
    /// into view. Returns `false` for an out-of-range row.
    pub fn focus_row(&mut self, row: usize) -> bool {
        let Some(identity) = self.row_identity(row) else {
            return false;
        };
        if self.focused_row.as_ref() != Some(&identity) {
            self.focused_row = Some(identity);
            self.dirty = true;
        }
        self.scroll_row_into_view(row);
        true
    }

    /// Clear the current row focus without changing an existing range
    /// selection.
    pub fn clear_row_focus(&mut self) {
        if self.focused_row.take().is_some() {
            self.dirty = true;
        }
    }

    /// Activate a visible row. Activating a band toggles it; activating a
    /// change selects that one change and notifies the host callback.
    pub fn activate_row(&mut self, row: usize) -> bool {
        let Some(visual) = self.rows.get(row).copied() else {
            return false;
        };
        let _ = self.focus_row(row);
        match visual {
            VisualRow::Group(_) => self.toggle_group_at_row(row),
            VisualRow::Entry(entry, _) => {
                self.selection = Some((entry, entry));
                self.selection_anchor = Some(entry);
                let id = self.entries.get(entry).map(|item| item.id);
                self.rebuild_rows();
                if let Some(id) = id
                    && let Some(callback) = self.on_activate_entry.as_mut()
                {
                    callback(&id);
                }
                true
            }
        }
    }

    /// Activate the currently focused row.
    pub fn activate_focused_row(&mut self) -> bool {
        self.focused_row().is_some_and(|row| self.activate_row(row))
    }

    pub fn begin_naming_group_at_row(&mut self, i: usize) -> bool {
        if self.preview_selection {
            return false;
        }
        if self.on_name_group.is_none() {
            return false;
        }
        let Some(VisualRow::Group(g)) = self.rows.get(i).copied() else {
            return false;
        };
        let Some(rg) = self.row_groups.get(g) else {
            return false;
        };
        let seed = rg.name.clone().unwrap_or_default();
        self.naming = Some(NamingState {
            target: NameTarget::Group(g),
            field: NameField::new(seed),
        });
        self.dirty = true;
        true
    }

    /// `false` when nothing is selected or the host cannot persist a name.
    pub fn begin_naming_selection(&mut self) -> bool {
        if self.preview_selection || self.selection.is_none() || self.on_name_group.is_none() {
            return false;
        }
        self.naming = Some(NamingState {
            target: NameTarget::Selection,
            field: NameField::new(""),
        });
        self.dirty = true;
        true
    }

    pub fn naming_buffer(&self) -> Option<&str> {
        self.naming.as_ref().map(|n| n.field.text())
    }

    /// True while a name field has the keyboard; a host that routes keys by
    /// hit-test must send them here regardless of pointer position.
    pub fn wants_keyboard(&self) -> bool {
        self.naming.is_some()
    }

    pub fn cancel_naming(&mut self) {
        if self.naming.take().is_some() {
            self.dirty = true;
        }
    }

    /// An empty buffer over an **existing** name removes it (the callback
    /// fires with an empty label); over a range with no name it is a no-op.
    pub fn commit_naming(&mut self) -> bool {
        let Some(state) = self.naming.take() else {
            return false;
        };
        self.dirty = true;
        let label = state.field.text().trim().to_string();
        let req = match state.target {
            NameTarget::Group(g) => {
                let Some(rg) = self.row_groups.get(g) else {
                    return false;
                };
                GroupNameRequest {
                    record_id: rg.record_id.clone(),
                    start: rg.first_entry,
                    end: rg.last_entry,
                    label,
                }
            }
            NameTarget::Selection => {
                let Some((a, b)) = self.selection else {
                    return false;
                };
                let (Some(first), Some(last)) = (self.entries.get(a), self.entries.get(b)) else {
                    return false;
                };
                GroupNameRequest {
                    record_id: None,
                    start: first.id,
                    end: last.id,
                    label,
                }
            }
        };
        if req.label.is_empty() && req.record_id.is_none() {
            return false;
        }
        if let Some(cb) = self.on_name_group.as_mut() {
            cb(&req);
        }
        self.selection = None;
        self.selection_anchor = None;
        true
    }

    // ---------- geometry ----------

    /// Proportional to the pane with a floor so a name is never squeezed to
    /// nothing.
    fn author_slot_w(&self) -> f64 {
        (self.rect.width() * 0.32).clamp(84.0, AUTHOR_SLOT_W)
    }

    fn time_slot_w(&self) -> f64 {
        if self.rect.width() < NARROW_WIDTH {
            48.0
        } else {
            TIME_SLOT_W
        }
    }

    fn revert_chip_w(&self) -> f64 {
        if self.rect.width() < NARROW_WIDTH {
            48.0
        } else {
            REVERT_CHIP_W
        }
    }

    fn rows_top(&self) -> f64 {
        self.rect.y0 + PADDING + FILTER_STRIP_HEIGHT + PADDING
    }

    /// Above the selection bar when one is showing.
    fn rows_bottom(&self) -> f64 {
        let mut y = self.rect.y1 - PADDING;
        if self.selection.is_some() {
            y -= SELECTION_BAR_H;
        }
        y.max(self.rows_top())
    }

    pub fn selection_bar_rect(&self) -> Rect {
        if self.selection.is_none() {
            return Rect::ZERO;
        }
        let y0 = self.rect.y1 - PADDING - SELECTION_BAR_H;
        if y0 < self.rows_top() {
            return Rect::ZERO;
        }
        Rect::new(
            self.rect.x0 + PADDING,
            y0,
            self.rect.x1 - PADDING,
            self.rect.y1 - PADDING,
        )
    }

    /// `Rect::ZERO` when it is not on screen.
    pub fn name_selection_rect(&self) -> Rect {
        self.name_selection_rect
    }

    fn row_height(&self, i: usize) -> f64 {
        match self.rows.get(i) {
            Some(VisualRow::Group(_)) => GROUP_ROW_HEIGHT,
            _ => ROW_HEIGHT,
        }
    }

    fn row_depth(&self, i: usize) -> usize {
        match self.rows.get(i) {
            Some(VisualRow::Group(g)) => self.row_groups.get(*g).map_or(0, |rg| rg.depth),
            Some(VisualRow::Entry(_, d)) => *d,
            None => 0,
        }
    }

    fn row_identity(&self, i: usize) -> Option<FocusedRow> {
        match self.rows.get(i)? {
            VisualRow::Group(group) => self
                .row_groups
                .get(*group)
                .map(|item| FocusedRow::Group(item.key.clone())),
            VisualRow::Entry(entry, _) => self
                .entries
                .get(*entry)
                .map(|item| FocusedRow::Entry(item.id)),
        }
    }

    fn focused_row_index(&self) -> Option<usize> {
        let focused = self.focused_row.as_ref()?;
        self.rows.iter().enumerate().find_map(|(index, _)| {
            (self.row_identity(index).as_ref() == Some(focused)).then_some(index)
        })
    }

    fn row_rect_unclipped(&self, i: usize) -> Option<Rect> {
        if i >= self.rows.len() {
            return None;
        }
        let top = *self.row_tops.get(i)?;
        let y0 = self.rows_top() - self.scroll_y + top;
        Some(Rect::new(
            self.rect.x0 + PADDING,
            y0,
            self.rect.x1 - PADDING,
            y0 + self.row_height(i),
        ))
    }

    fn scroll_row_into_view(&mut self, i: usize) {
        let Some(&top) = self.row_tops.get(i) else {
            return;
        };
        let bottom = top + self.row_height(i);
        let viewport = self.viewport_height();
        if viewport <= 0.0 {
            return;
        }
        let scroll = if top < self.scroll_y {
            top
        } else if bottom > self.scroll_y + viewport {
            bottom - viewport
        } else {
            self.scroll_y
        }
        .clamp(0.0, self.max_scroll());
        if (scroll - self.scroll_y).abs() > f64::EPSILON {
            self.scroll_y = scroll;
            self.hover_row = self.row_at(self.last_cursor);
            self.dirty = true;
        }
    }

    /// Binary search over the cumulative tops rather than a scan, so a room
    /// with thousands of groups costs the same per frame as one with twenty.
    fn visible_row_range(&self) -> std::ops::Range<usize> {
        let n = self.rows.len();
        if n == 0 {
            return 0..0;
        }
        let vp = self.viewport_height();
        if vp <= 0.0 {
            return 0..0;
        }
        let first = self.row_at_offset(self.scroll_y).unwrap_or(0);
        let last = self
            .row_at_offset(self.scroll_y + vp)
            .map_or(n, |i| i.saturating_add(1));
        first.min(n)..last.min(n)
    }

    /// `None` past the end.
    fn row_at_offset(&self, y: f64) -> Option<usize> {
        if self.rows.is_empty() {
            return None;
        }
        if y < 0.0 {
            return Some(0);
        }
        let idx = self.row_tops.partition_point(|t| *t <= y).saturating_sub(1);
        if idx >= self.rows.len() {
            return None;
        }
        Some(idx)
    }

    /// `true` = newest at top (default).
    pub fn set_newest_first(&mut self, newest_first: bool) {
        if self.newest_first != newest_first {
            self.newest_first = newest_first;
            self.reorder_groups();
            self.rebuild();
        }
    }

    pub fn set_now_for_test(&mut self, now: SystemTime) {
        self.now_override = Some(now);
    }

    pub fn set_active_chip(&mut self, chip: ChipKind) {
        self.active_chip = chip;
        self.rebuild();
    }

    pub fn active_chip(&self) -> ChipKind {
        self.active_chip
    }

    /// Empty = no author narrowing.
    pub fn set_author_query(&mut self, q: impl Into<String>) {
        self.author_query = q.into();
        self.rebuild();
    }

    pub fn author_query(&self) -> &str {
        &self.author_query
    }

    pub fn visible_len(&self) -> usize {
        self.visible.len()
    }

    pub fn visible_entry(&self, i: usize) -> Option<&HistoryEntry> {
        self.visible.get(i).and_then(|&j| self.entries.get(j))
    }

    pub fn hover_row(&self) -> Option<usize> {
        self.hover_row
    }

    /// `None` when the line is outside the visible window.
    pub fn row_rect(&self, i: usize) -> Option<Rect> {
        let rect = self.row_rect_unclipped(i)?;
        let y0 = rect.y0;
        let y1 = rect.y1;
        if y1 < self.rows_top() || y0 > self.rows_bottom() {
            return None;
        }
        Some(rect)
    }

    /// `None` when the host cannot revert, the line is off-screen, is a group
    /// header, or its op class is non-revertible.
    pub fn revert_chip_rect(&self, i: usize) -> Option<Rect> {
        let row = self.row_rect(i)?;
        let entry = self.row_entry(i)?;
        if self.preview_selection || self.on_revert.is_none() || !entry.is_revertible() {
            return None;
        }
        let depth = self.row_depth(i);
        let chip_h = ROW_HEIGHT - 16.0;
        let cy = f64::midpoint(row.y0, row.y1);
        let _ = depth;
        Some(Rect::new(
            row.x1 - self.revert_chip_w() - 6.0,
            cy - chip_h / 2.0,
            row.x1 - 6.0,
            cy + chip_h / 2.0,
        ))
    }

    /// The pointer target for a chip painted as `painted` inside `within`:
    /// grown vertically to the minimum pointer target, centred on the paint
    /// and clamped to its container, while the paint stays the designed
    /// 20 / 28 px pill. Neighbouring chips only ever differ in x, so grown
    /// targets never overlap each other. Hit-testing and accessibility read
    /// this rect; painting reads the original.
    fn hit_rect_within(&self, painted: Rect, within: Rect) -> Rect {
        if painted.is_zero_area() {
            return Rect::ZERO;
        }
        let target = self.min_pointer_target.max(painted.height());
        let mut y0 = painted.y0 - (target - painted.height()) / 2.0;
        let mut y1 = y0 + target;
        // Against a container edge the target slides rather than shrinks,
        // so a chip near the top of its row still gets the full height.
        if y0 < within.y0 {
            y1 += within.y0 - y0;
            y0 = within.y0;
        }
        if y1 > within.y1 {
            y0 -= y1 - within.y1;
            y1 = within.y1;
            y0 = y0.max(within.y0);
        }
        Rect::new(painted.x0, y0, painted.x1, y1.max(y0))
    }

    /// `(name, restore)` pointer targets — see `Self::hit_rect_within`.
    /// `restore` is `None` when nothing in the range can be restored to.
    /// `(name, restore)`. `name` is zero-sized without a naming callback;
    /// `restore` is `None` without a restore callback or restorable change.
    pub fn group_chip_rects(&self, i: usize) -> Option<(Rect, Option<Rect>)> {
        let VisualRow::Group(g) = *self.rows.get(i)? else {
            return None;
        };
        let full = self.row_rect(i)?;
        let rg = self.row_groups.get(g)?;
        let row = Rect::new(
            full.x0 + px(rg.depth) * INDENT,
            full.y0 + 2.0,
            full.x1,
            full.y1 - 2.0,
        );
        let (name, restore) = self.group_chip_rects_local(row, rg);
        Some((
            self.hit_rect_within(name, row),
            restore.map(|r| self.hit_rect_within(r, row)),
        ))
    }

    /// `None` when the line is not a band header, is off-screen, holds
    /// nothing revertible, or no host installed
    /// [`ActivityTimeline::on_undo_band`].
    pub fn group_undo_chip_rect(&self, i: usize) -> Option<Rect> {
        let VisualRow::Group(g) = *self.rows.get(i)? else {
            return None;
        };
        let full = self.row_rect(i)?;
        let rg = self.row_groups.get(g)?;
        let row = Rect::new(
            full.x0 + px(rg.depth) * INDENT,
            full.y0 + 2.0,
            full.x1,
            full.y1 - 2.0,
        );
        self.group_chip_rects_full(row, rg)
            .undo
            .map(|r| self.hit_rect_within(r, row))
    }

    /// `None` on a line that is not a band header.
    pub fn band_undo_request(&self, i: usize) -> Option<BandUndoRequest> {
        let VisualRow::Group(g) = *self.rows.get(i)? else {
            return None;
        };
        let rg = self.row_groups.get(g)?;
        Some(BandUndoRequest {
            start: rg.first_entry,
            end: rg.last_entry,
            label: rg.name.clone(),
            authors: rg.authors.clone(),
        })
    }

    fn now(&self) -> SystemTime {
        self.now_override.unwrap_or_else(crate::time::wall)
    }

    fn passes(&self, e: &HistoryEntry, chip_ops: &[OpClass], needle: &str) -> bool {
        if !self.filter.matches(e) {
            return false;
        }
        if !chip_ops.is_empty() && !chip_ops.contains(&e.op_class) {
            return false;
        }
        if !needle.is_empty() && !e.author.name.to_lowercase().contains(needle) {
            return false;
        }
        true
    }

    /// Human provenance kept deliberately compact for a one-line history
    /// row. Legacy entries have no synthetic origin label; action-backed
    /// entries retain the app/resource path that a reader can actually use.
    fn entry_breadcrumb(entry: &HistoryEntry) -> Option<String> {
        let context = entry.context.as_ref()?;
        let mut parts = Vec::new();
        if let Some(app) = context
            .app
            .as_ref()
            .filter(|app| !app.label.trim().is_empty())
        {
            parts.push(app.label.trim().to_string());
        }
        if let Some(resource) = context
            .resource
            .as_ref()
            .filter(|resource| !resource.label.trim().is_empty())
        {
            parts.push(resource.label.trim().to_string());
        }
        (!parts.is_empty()).then(|| parts.join(" / "))
    }

    /// A collapsed band hides its constituent rows, so retain enough
    /// provenance to reveal a cross-app or cross-workspace handoff. A work
    /// group explicitly typed as `workspace` is treated as the workspace
    /// label; other groups remain visible only through their app labels.
    fn group_scope_label(&self, start: usize, end: usize) -> Option<String> {
        let compact = |labels: Vec<String>| -> Option<String> {
            if labels.is_empty() {
                return None;
            }
            let remaining = labels.len().saturating_sub(2);
            let mut shown = labels.into_iter().take(2).collect::<Vec<_>>().join(" + ");
            if remaining > 0 {
                use std::fmt::Write as _;
                let _ = write!(shown, " +{remaining}");
            }
            Some(shown)
        };
        let mut workspaces = Vec::new();
        let mut apps = Vec::new();
        for entry in self.entries.get(start..end)? {
            let Some(context) = entry.context.as_ref() else {
                continue;
            };
            if let Some(group) = context.group.as_ref().filter(|group| {
                group.kind.eq_ignore_ascii_case("workspace") && !group.label.trim().is_empty()
            }) {
                let label = group.label.trim().to_string();
                if !workspaces.contains(&label) {
                    workspaces.push(label);
                }
            }
            if let Some(app) = context
                .app
                .as_ref()
                .filter(|app| !app.label.trim().is_empty())
            {
                let label = app.label.trim().to_string();
                if !apps.contains(&label) {
                    apps.push(label);
                }
            }
            for facet in context.facets.iter().filter(|facet| {
                matches!(facet.key.as_str(), "workspace" | "workspace_name")
                    && !facet.value.trim().is_empty()
            }) {
                let label = facet.value.trim().to_string();
                if !workspaces.contains(&label) {
                    workspaces.push(label);
                }
            }
        }
        let workspace = compact(workspaces);
        let app = compact(apps);
        match (workspace, app) {
            (Some(workspace), Some(app)) => Some(format!("{workspace} / {app}")),
            (Some(workspace), None) => Some(workspace),
            (None, Some(app)) => Some(app),
            (None, None) => None,
        }
    }

    fn regroup(&mut self) {
        self.groups = group_entries(&self.entries, &self.named, &self.grouping);
        self.reorder_groups();
        // A selection that no longer addresses real changes would name a
        // range that does not exist.
        if let Some((a, b)) = self.selection
            && (a >= self.entries.len() || b >= self.entries.len())
        {
            self.selection = None;
            self.selection_anchor = None;
        }
        self.rebuild();
    }

    /// Orders bands, not rows: automerge stamps advisory time in whole
    /// seconds, and a peer that merges late can carry an old clock into the
    /// newest part of the graph, so sorting rows by clock alone would break
    /// a band apart. Sorting by each band's newest change keeps it contiguous.
    fn reorder_groups(&mut self) {
        let mut order: Vec<(usize, SystemTime)> = self
            .groups
            .iter()
            .enumerate()
            .map(|(i, g)| (i, g.last_time))
            .collect();
        order.sort_by(|(ai, at), (bi, bt)| {
            if self.newest_first {
                bt.cmp(at).then(bi.cmp(ai))
            } else {
                at.cmp(bt).then(ai.cmp(bi))
            }
        });
        self.group_order = order.into_iter().map(|(i, _)| i).collect();
    }

    fn group_entries_in_display_order(&self, g: &HistoryGroup, out: &mut Vec<usize>) {
        let mut members: Vec<Member<'_>> = Vec::new();
        let mut i = g.start;
        while i < g.end {
            if let Some(c) = g.children.iter().find(|c| c.start == i) {
                members.push(Member::Child(c));
                i = c.end;
            } else {
                members.push(Member::Entry(i));
                i = i.saturating_add(1);
            }
        }
        if self.newest_first {
            members.reverse();
        }
        for m in members {
            match m {
                Member::Entry(e) => out.push(e),
                Member::Child(c) => self.group_entries_in_display_order(c, out),
            }
        }
    }

    fn rebuild(&mut self) {
        let chip_ops = self.active_chip.op_classes();
        let needle = self.author_query.trim().to_lowercase();
        // Walked in the same order the lines are, so a count and a position
        // mean the same thing in both.
        let mut ordered: Vec<usize> = Vec::with_capacity(self.entries.len());
        for gi in self.group_order.clone() {
            let Some(g) = self.groups.get(gi) else {
                continue;
            };
            self.group_entries_in_display_order(g, &mut ordered);
        }
        self.visible = ordered
            .into_iter()
            .filter(|i| {
                self.entries
                    .get(*i)
                    .is_some_and(|e| self.passes(e, &chip_ops, &needle))
            })
            .collect();
        // A destructive selection must never survive a filter that hides any
        // member: the visible UI would no longer describe what Undo targets.
        if let Some((start, end)) = self.selection
            && (start..=end).any(|index| !self.visible.contains(&index))
        {
            self.selection = None;
            self.selection_anchor = None;
        }
        self.rebuild_rows();
    }

    fn rebuild_rows(&mut self) {
        let chip_ops = self.active_chip.op_classes();
        let needle = self.author_query.trim().to_lowercase();
        let mut rows: Vec<VisualRow> = Vec::new();
        let mut row_groups: Vec<RowGroup> = Vec::new();
        let mut budget = AUTO_EXPAND_BUDGET;

        let now = self.now();
        let mut last_section = None;
        for gi in self.group_order.clone() {
            let Some(g) = self.groups.get(gi) else {
                continue;
            };
            if self.show_day_headings
                && let Some(section) = self.group_day_section(g, &chip_ops, &needle, now)
            {
                if last_section != Some(section) {
                    Self::emit_day_section(section, &mut rows, &mut row_groups);
                    last_section = Some(section);
                }
            }
            self.emit_group(
                g,
                &chip_ops,
                &needle,
                &mut rows,
                &mut row_groups,
                &mut budget,
            );
        }

        self.rows = rows;
        self.row_groups = row_groups;
        self.row_tops = Vec::with_capacity(self.rows.len().saturating_add(1));
        let mut y = 0.0;
        for i in 0..self.rows.len() {
            self.row_tops.push(y);
            y += self.row_height(i);
        }
        self.row_tops.push(y);

        let max_scroll = self.max_scroll();
        if self.scroll_y > max_scroll {
            self.scroll_y = max_scroll;
        }
        if self.hover_row.is_some_and(|h| h >= self.rows.len()) {
            self.hover_row = None;
        }
        // Collapsing a band can hide the entry that had focus. Do not leave a
        // virtual focus target pointing at an unaddressable row.
        if self.focused_row.is_some() && self.focused_row_index().is_none() {
            self.focused_row = None;
        }
        // A name being typed into a band that no longer exists has nowhere
        // to land.
        if let Some(n) = &self.naming
            && let NameTarget::Group(g) = n.target
            && g >= self.row_groups.len()
        {
            self.naming = None;
        }
        self.dirty = true;
    }

    fn emit_group(
        &self,
        g: &HistoryGroup,
        chip_ops: &[OpClass],
        needle: &str,
        rows: &mut Vec<VisualRow>,
        row_groups: &mut Vec<RowGroup>,
        budget: &mut usize,
    ) {
        let visible_count = (g.start..g.end)
            .filter(|i| {
                self.entries
                    .get(*i)
                    .is_some_and(|e| self.passes(e, chip_ops, needle))
            })
            .count();
        if visible_count == 0 {
            return;
        }
        // One unnamed change is a row, not a band: a header over one change
        // is chrome for nothing.
        if g.is_bare_change() {
            rows.push(VisualRow::Entry(g.start, g.depth));
            *budget = budget.saturating_sub(1);
            return;
        }

        let expanded = if self.expanded.contains(&g.key) {
            true
        } else if self.collapsed.contains(&g.key) {
            false
        } else {
            // Both conditions are about the *whole* list, not this band.
            // Count bands, not groups — a run of unnamed changes draws as
            // plain rows, not a band, so it should not count toward density.
            let bands = self.groups.iter().filter(|g| !g.is_bare_change()).count();
            bands <= DENSE_GROUP_COUNT && visible_count <= *budget
        };
        if expanded {
            *budget = budget.saturating_sub(visible_count);
        }

        let total_count = g.len();
        let restorable =
            (g.start..g.end).any(|i| self.entries.get(i).is_some_and(HistoryEntry::is_revertible));
        let mut subtitle = g.subtitle(&self.entries);
        if visible_count < total_count {
            // Saying "12 changes" over three visible rows would be a lie the
            // reader cannot catch.
            subtitle = format!("{visible_count} of {total_count} shown \u{00b7} {subtitle}");
        }
        if g.open {
            subtitle = format!("still open \u{00b7} {subtitle}");
        }
        let gi = row_groups.len();
        row_groups.push(RowGroup {
            key: g.key.clone(),
            title: g.title(&self.entries),
            subtitle,
            scope_label: self.group_scope_label(g.start, g.end),
            name: g.name.clone(),
            record_id: g.record_id.clone(),
            open: g.open,
            depth: g.depth,
            first_entry: g.first_entry,
            last_entry: g.last_entry,
            last_time: g.last_time,
            authors: g.authors.clone(),
            expanded,
            restorable,
            is_section: false,
        });
        rows.push(VisualRow::Group(gi));
        if !expanded {
            return;
        }

        // Causal order, reversed as a whole so the band reads the same way
        // round as the list it sits in.
        let mut members: Vec<Member<'_>> = Vec::new();
        let mut i = g.start;
        while i < g.end {
            if let Some(c) = g.children.iter().find(|c| c.start == i) {
                members.push(Member::Child(c));
                i = c.end;
            } else {
                members.push(Member::Entry(i));
                i = i.saturating_add(1);
            }
        }
        if self.newest_first {
            members.reverse();
        }
        for m in members {
            match m {
                Member::Entry(e) => {
                    let shows = self
                        .entries
                        .get(e)
                        .is_some_and(|entry| self.passes(entry, chip_ops, needle));
                    if shows {
                        rows.push(VisualRow::Entry(e, g.depth.saturating_add(1)));
                    }
                }
                Member::Child(c) => self.emit_group(c, chip_ops, needle, rows, row_groups, budget),
            }
        }
    }

    fn content_height(&self) -> f64 {
        *self.row_tops.last().unwrap_or(&0.0)
    }

    fn viewport_height(&self) -> f64 {
        (self.rows_bottom() - self.rows_top()).max(0.0)
    }

    fn max_scroll(&self) -> f64 {
        (self.content_height() - self.viewport_height()).max(0.0)
    }

    pub fn filter_strip_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PADDING,
            self.rect.y0 + PADDING,
            self.rect.x1 - PADDING,
            self.rect.y0 + PADDING + FILTER_STRIP_HEIGHT,
        )
    }

    pub fn row_at(&self, p: Point) -> Option<usize> {
        if p.x < self.rect.x0 + PADDING || p.x > self.rect.x1 - PADDING {
            return None;
        }
        let top = self.rows_top();
        if p.y < top || p.y > self.rows_bottom() {
            return None;
        }
        let i = self.row_at_offset(p.y - top + self.scroll_y)?;
        // `row_rect` owns the clipping rules; going through it keeps hover
        // and paint agreeing on which lines exist.
        self.row_rect(i).filter(|r| r.contains(p)).map(|_| i)
    }
}

enum Member<'a> {
    Child(&'a HistoryGroup),
    Entry(usize),
}

/// `accent` fills the pill with the accent colour (the affirmative action);
/// otherwise it is a quiet outlined chip.
fn draw_chip(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    accent: bool,
) {
    if rect.width() <= 2.0 || rect.height() <= 2.0 {
        return;
    }
    let radius = f64::from(theme.tokens.radii.detail);
    let shape = RoundedRect::from_rect(rect, radius);
    let colors = &theme.tokens.colors;
    // Accent chip: the face is its own contrast step, no edge (HIG §9.5
    // "selected accent, no edge"). Quiet chip: the raised rest fill under an
    // edge derived from it — a Button Neutral in miniature.
    if accent {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.intent.inverted,
            None,
            &shape,
        );
    } else {
        crate::surface::card(scene, transform, rect, radius, colors.surfaces.raised);
    }
    let size = theme.text(TextRole::Caption);
    let shown = fit_text(tr, label, &size, rect.width() - 8.0);
    let (lw, lh) = tr.measure_style(&shown, None, &size);
    // Ink chosen against the face it sits on: `text_primary` is near-black
    // on the light preset and vanished on its blue accent.
    let ink = if accent {
        colors.intent.on_inverted
    } else {
        colors.text_primary
    };
    tr.draw_style(
        scene,
        &shown,
        ink,
        translated(
            transform,
            (
                rect.x0 + (rect.width() - f64::from(lw)) / 2.0,
                rect.y0 + (rect.height() - f64::from(lh)) / 2.0,
            ),
        ),
        None,
        &size,
    );
}

/// A visual row's full-width band and its nesting depth.
#[derive(Clone, Copy)]
struct RowSlot {
    rect: Rect,
    depth: usize,
}

impl RowSlot {
    /// The band less its depth's indent: where the row's own content sits.
    fn indented(self) -> Rect {
        Rect::new(
            self.rect.x0 + px(self.depth) * INDENT,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1,
        )
    }
}

/// A row's pointer, selection and keyboard-focus state this frame.
#[derive(Clone, Copy)]
struct RowEmphasis {
    hovered: bool,
    selected: bool,
    focused: bool,
}

/// Where an entry row's columns paint, resolved once by `draw_row`.
struct EntryRowLayout {
    /// The indented row.
    row: Rect,
    /// The row's centre line.
    cy: f64,
    text_style: TextStyle,
    author_slot_w: f64,
    /// The pointer is over a revertible entry and a host handles reverts.
    offers_revert: bool,
    /// Summary width given up to the revert chip; zero unless `offers_revert`.
    chip_reserve: f64,
}

/// A band header's group and the title column both of its lines share.
struct BandHeader<'a> {
    rg: &'a RowGroup,
    /// The inset header row.
    row: Rect,
    title_x: f64,
    title_style: TextStyle,
}

/// The filter strip's frame-wide paint inputs, shared by its chip row and
/// its author pill.
struct FilterStripPaint {
    strip: Rect,
    /// The chip height, never taller than the strip.
    chip_h: f64,
}

/// When a row's change was made; empty for the genesis, which no one made
/// at any time worth naming.
fn row_time(entry: &HistoryEntry, now: SystemTime) -> String {
    if entry.is_genesis() {
        String::new()
    } else {
        format_relative_time(entry.timestamp, now)
    }
}

/// Calendar bucket for a timeline heading. Days are UTC civil days so a
/// pinned `now` in tests matches production without a timezone table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DaySection {
    Today,
    Yesterday,
    ThisWeek,
    Earlier,
}

impl DaySection {
    fn of(ts: SystemTime, now: SystemTime) -> Self {
        let today = civil_day(now);
        let day = civil_day(ts);
        if day == today {
            return Self::Today;
        }
        if day == today.saturating_sub(1) {
            return Self::Yesterday;
        }
        let week_start = today.saturating_sub(monday_offset(today));
        if day >= week_start {
            return Self::ThisWeek;
        }
        Self::Earlier
    }

    fn key(self) -> &'static str {
        match self {
            Self::Today => "day:today",
            Self::Yesterday => "day:yesterday",
            Self::ThisWeek => "day:this-week",
            Self::Earlier => "day:earlier",
        }
    }

    fn title(self) -> String {
        crate::i18n::widget_format(
            match self {
                Self::Today => "history-day-today",
                Self::Yesterday => "history-day-yesterday",
                Self::ThisWeek => "history-day-this-week",
                Self::Earlier => "history-day-earlier",
            },
            &[],
        )
    }
}

fn civil_day(ts: SystemTime) -> i64 {
    const DAY: u64 = 86_400;
    match ts.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(delta) => i64::try_from(delta.as_secs() / DAY).unwrap_or(i64::MAX),
        Err(err) => -i64::try_from(err.duration().as_secs() / DAY).unwrap_or(i64::MAX),
    }
}

/// Days since Monday of the week that contains `day`. `weekday` is
/// 0 = Sunday … 6 = Saturday.
fn monday_offset(day: i64) -> i64 {
    i64::from((crate::date::weekday(day) + 6) % 7)
}

impl ActivityTimeline {
    fn group_day_section(
        &self,
        g: &HistoryGroup,
        chip_ops: &[OpClass],
        needle: &str,
        now: SystemTime,
    ) -> Option<DaySection> {
        (g.start..g.end)
            .filter_map(|i| self.entries.get(i))
            .filter(|entry| self.passes(entry, chip_ops, needle))
            .map(|entry| entry.timestamp)
            .max()
            .map(|ts| DaySection::of(ts, now))
    }

    fn emit_day_section(
        section: DaySection,
        rows: &mut Vec<VisualRow>,
        row_groups: &mut Vec<RowGroup>,
    ) {
        let gi = row_groups.len();
        row_groups.push(RowGroup {
            key: section.key().to_string(),
            title: section.title(),
            subtitle: String::new(),
            scope_label: None,
            name: None,
            record_id: None,
            open: false,
            depth: 0,
            first_entry: HistoryEntryId([0; 16]),
            last_entry: HistoryEntryId([0; 16]),
            last_time: SystemTime::UNIX_EPOCH,
            authors: Vec::new(),
            expanded: true,
            restorable: false,
            is_section: true,
        });
        rows.push(VisualRow::Group(gi));
    }

    fn draw_row(
        &self,
        ctx: &mut PaintCtx<'_>,
        slot: RowSlot,
        entry: &HistoryEntry,
        emphasis: RowEmphasis,
        now: SystemTime,
    ) {
        Self::draw_row_chrome(ctx, slot, emphasis);

        let row = slot.indented();
        let cy = f64::midpoint(row.y0, row.y1);
        // A genesis row names no one: no avatar, no author, no time.
        let dot_x = if entry.is_genesis() {
            row.x0 + 8.0 + AVATAR_DIAM / 2.0
        } else {
            Self::draw_row_avatar(ctx.scene, ctx.transform, row, cy, entry)
        };

        // Reserved only when hovered — reserving it on every row cost the
        // summary a third of its width for something not on screen.
        let offers_revert = emphasis.hovered && self.on_revert.is_some() && entry.is_revertible();
        let layout = EntryRowLayout {
            row,
            cy,
            text_style: ctx.theme.text(TextRole::Caption),
            // The indent comes out of the author cluster, not the summary —
            // the summary is what the reader is here for.
            author_slot_w: (self.author_slot_w() - px(slot.depth) * INDENT).max(72.0),
            offers_revert,
            chip_reserve: if offers_revert {
                self.revert_chip_w() + 6.0
            } else {
                0.0
            },
        };
        if !entry.is_genesis() {
            self.draw_row_author(ctx, &layout, entry, dot_x);
        }
        self.draw_row_body(ctx, &layout, entry, now);
        self.draw_row_trailing(ctx, &layout, entry, now);
    }

    /// The avatar dot; returns its center x so the author cluster can start
    /// just past it.
    fn draw_row_avatar(
        scene: &mut Scene,
        transform: Affine,
        row: Rect,
        cy: f64,
        entry: &HistoryEntry,
    ) -> f64 {
        let (r, g, b) = entry.author.rgb();
        let dot_color = Color::from_rgba8(r, g, b, 255);
        let dot_x = row.x0 + 8.0 + AVATAR_DIAM / 2.0;
        // A person is a filled dot in their colour; software acting for
        // someone (an agent, a Shortcut, an app) is a ring.
        if entry.author.relation == crate::widget::history::AuthorRelation::Software {
            let ring = Circle::new(Point::new(dot_x, cy), AVATAR_DIAM / 2.0 - 0.75);
            scene.stroke(&Stroke::new(1.5), transform, dot_color, None, &ring);
        } else {
            let dot = Circle::new(Point::new(dot_x, cy), AVATAR_DIAM / 2.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                dot_color,
                None,
                &dot,
            );
        }
        dot_x
    }

    /// The depth rail plus the selection/hover backdrop, under everything
    /// else in the row.
    fn draw_row_chrome(ctx: &mut PaintCtx<'_>, slot: RowSlot, emphasis: RowEmphasis) {
        let PaintCtx {
            scene,
            transform,
            theme,
            ..
        } = ctx.reborrow();
        let RowSlot {
            rect: full_row,
            depth,
        } = slot;
        let row = slot.indented();
        let RowEmphasis {
            hovered: is_hovered,
            selected: is_selected,
            focused: is_focused,
        } = emphasis;
        // The rail: a hairline per nesting level, so rows read as belonging
        // to the bands above them rather than just starting further right.
        let surface = theme.tokens.colors.surfaces.raised;
        for level in 1..=depth {
            let x = full_row.x0 + (px(level) - 0.5) * INDENT;
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                crate::surface::edge_on(surface),
                None,
                &Line::new(Point::new(x, full_row.y0), Point::new(x, full_row.y1)),
            );
        }

        if is_selected {
            // A neutral raise plus an accent edge, not an accent *fill*: the
            // pale end of the accent ramp behind primary text is unreadable.
            // The raise is the top step of the pane's own ramp under a
            // derived edge, so it reads on a light pane as well as a dark one.
            let radius = f64::from(theme.tokens.radii.detail);
            let lift = row.inflate(0.0, -ROW_LIFT_INSET);
            let shape = RoundedRect::from_rect(lift, radius);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                wash(surface, WASH_SELECTED),
                None,
                &shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                crate::surface::edge_on(surface),
                None,
                &crate::surface::edge_outline(lift, radius),
            );
            let edge = RoundedRect::from_rect(
                Rect::new(row.x0, row.y0 + 2.0, row.x0 + 2.5, row.y1 - 2.0),
                1.25,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.intent.inverted_hover,
                None,
                &edge,
            );
        } else if is_hovered {
            let shape = RoundedRect::from_rect(
                row.inflate(0.0, -ROW_LIFT_INSET),
                f64::from(theme.tokens.radii.detail),
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                wash(surface, WASH_HOVER),
                None,
                &shape,
            );
        }
        if is_focused {
            let shape =
                RoundedRect::from_rect(row.inset(-1.0), f64::from(theme.tokens.radii.detail) + 1.0);
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.tokens.colors.border_focus,
                None,
                &shape,
            );
        }
    }

    /// The author's name. Their identity is the dot's colour and, in full,
    /// the provenance detail; a row is read by the name.
    fn draw_row_author(
        &self,
        ctx: &mut PaintCtx<'_>,
        layout: &EntryRowLayout,
        entry: &HistoryEntry,
        dot_x: f64,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let &EntryRowLayout {
            row,
            cy,
            ref text_style,
            author_slot_w,
            ..
        } = layout;
        let x = dot_x + AVATAR_DIAM / 2.0 + 8.0;
        let slot_end = row.x0 + author_slot_w - 6.0;
        let name = fit_text(
            text_renderer,
            entry.author.name.as_str(),
            text_style,
            (slot_end - x).max(24.0),
        );
        let name_top = text_top(text_renderer, text_style, cy);
        // The reader's own changes read quieter than other people's.
        let colour = if entry.author.relation == crate::widget::history::AuthorRelation::Reader {
            theme.tokens.colors.text_dim
        } else {
            theme.tokens.colors.text_primary
        };
        text_renderer.draw_style(
            scene,
            &name,
            colour,
            translated(transform, (x, name_top)),
            None,
            text_style,
        );
    }

    /// The op-class glyph and the summary column, including the redaction
    /// strike-through when the change was redacted.
    fn draw_row_body(
        &self,
        ctx: &mut PaintCtx<'_>,
        layout: &EntryRowLayout,
        entry: &HistoryEntry,
        now: SystemTime,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let &EntryRowLayout {
            row,
            cy,
            ref text_style,
            author_slot_w,
            chip_reserve,
            ..
        } = layout;
        let glyph_x = row.x0 + author_slot_w;
        let glyph = op_class_glyph(entry.op_class);
        let line_top = text_top(text_renderer, text_style, cy);
        text_renderer.draw_style(
            scene,
            glyph,
            theme.tokens.colors.text_dim,
            translated(transform, (glyph_x, line_top)),
            None,
            text_style,
        );

        let summary_x = glyph_x + 24.0;
        let summary_x_end = row.x1 - self.time_slot_w() - chip_reserve - TRAILING_INSET;
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
                let summary = Self::entry_breadcrumb(entry).map_or_else(
                    || entry.summary.clone(),
                    |breadcrumb| format!("{breadcrumb} · {}", entry.summary),
                );
                (summary, false)
            };
        let summary = summary_owned.as_str();
        let drawn_summary = fit_text(
            text_renderer,
            summary,
            text_style,
            (summary_x_end - summary_x).max(0.0),
        );
        let summary_color = if is_redacted_row {
            theme.tokens.colors.text_dim
        } else {
            theme.tokens.colors.text_primary
        };
        text_renderer.draw_style(
            scene,
            &drawn_summary,
            summary_color,
            translated(transform, (summary_x, line_top)),
            None,
            text_style,
        );
        // Drawn after the text so a regression that forgets to gate on
        // `is_redacted_row` paints a visible mark.
        if is_redacted_row {
            let (dw, _) = text_renderer.measure_style(&drawn_summary, None, text_style);
            let strike = Line::new(
                Point::new(summary_x, cy),
                Point::new(summary_x + f64::from(dw), cy),
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.tokens.colors.text_dim,
                None,
                &strike,
            );
        }
    }

    /// The relative-time slot, and the `[Revert]` chip when it applies.
    fn draw_row_trailing(
        &self,
        ctx: &mut PaintCtx<'_>,
        layout: &EntryRowLayout,
        entry: &HistoryEntry,
        now: SystemTime,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let &EntryRowLayout {
            row,
            cy,
            ref text_style,
            offers_revert,
            chip_reserve,
            ..
        } = layout;
        let rel = fit_text(
            text_renderer,
            &row_time(entry, now),
            text_style,
            self.time_slot_w(),
        );
        let (tw, th) = text_renderer.measure_style(&rel, None, text_style);
        let time_x = row.x1 - chip_reserve - TRAILING_INSET - f64::from(tw);
        text_renderer.draw_style(
            scene,
            &rel,
            theme.tokens.colors.text_dim,
            translated(transform, (time_x, cy - f64::from(th) / 2.0)),
            None,
            text_style,
        );

        // Two veto conditions on the chip: op-class non-revertibility, and a
        // redaction — we never offer "revert" across a redaction boundary.
        if !self.preview_selection && offers_revert && entry.redaction.is_none() {
            let chip_h = ROW_HEIGHT - 16.0;
            let chip_rect = Rect::new(
                row.x1 - self.revert_chip_w() - 6.0,
                cy - chip_h / 2.0,
                row.x1 - 6.0,
                cy + chip_h / 2.0,
            );
            draw_chip(
                scene,
                transform,
                text_renderer,
                theme,
                chip_rect,
                "Revert",
                false,
            );
        }
    }

    fn draw_group_row(
        &self,
        ctx: &mut PaintCtx<'_>,
        full_row: Rect,
        gi: usize,
        emphasis: RowEmphasis,
        now: SystemTime,
    ) {
        let Some(rg) = self.row_groups.get(gi) else {
            return;
        };
        let naming = self
            .naming
            .as_ref()
            .filter(|n| matches!(n.target, NameTarget::Group(g) if g == gi))
            .map(|n| &n.field);
        let is_hovered = emphasis.hovered;
        let row = Rect::new(
            full_row.x0 + px(rg.depth) * INDENT,
            full_row.y0 + 2.0,
            full_row.x1,
            full_row.y1 - 2.0,
        );
        Self::draw_group_row_chrome(ctx, full_row, row, rg, emphasis);

        let band = BandHeader {
            rg,
            row,
            title_x: row.x0 + 26.0,
            title_style: ctx.theme.text(TextRole::Label),
        };
        let chips = self.group_chip_rects_local(row, rg);
        self.draw_group_row_title(ctx, &band, is_hovered, naming);
        if rg.is_section {
            return;
        }

        // Line two: who and how much, plus — right-aligned — when. Stacked
        // under the title's *measured* line box (top pad + line + gap; 27 px
        // at 1×, the literal this replaces) so a larger text scale keeps the
        // two lines apart. The group row itself is still a fixed
        // `GROUP_ROW_HEIGHT`, so at 2× line two overhangs the row's bottom —
        // the row height has to follow the text scale for that (F5).
        let (_, title_h) = ctx.text.measure_style("Ag", None, &band.title_style);
        let meta_y = row.y0 + 6.0 + f64::from(title_h) + 6.0;
        Self::draw_group_row_meta(ctx, &band, now, meta_y);

        let PaintCtx {
            scene,
            transform,
            text: tr,
            theme,
        } = ctx.reborrow();
        if !self.preview_selection && (is_hovered || naming.is_some()) {
            let (name_rect, restore_rect) = chips;
            if name_rect.area() > 0.0 {
                draw_chip(
                    scene,
                    transform,
                    tr,
                    theme,
                    name_rect,
                    if rg.name.is_some() { "Rename" } else { "Name" },
                    rg.name.is_some(),
                );
            }
            if let Some(r) = self.group_chip_rects_full(row, rg).undo {
                draw_chip(scene, transform, tr, theme, r, "Undo", false);
            }
            if let Some(r) = restore_rect {
                draw_chip(scene, transform, tr, theme, r, "Restore", false);
            }
        }
    }

    /// The band header's depth rail, backdrop, name spine and disclosure
    /// triangle — everything drawn before any text.
    fn draw_group_row_chrome(
        ctx: &mut PaintCtx<'_>,
        full_row: Rect,
        row: Rect,
        rg: &RowGroup,
        emphasis: RowEmphasis,
    ) {
        let PaintCtx {
            scene,
            transform,
            theme,
            ..
        } = ctx.reborrow();
        let RowEmphasis {
            hovered: is_hovered,
            focused: is_focused,
            ..
        } = emphasis;
        let colors = &theme.tokens.colors;
        let surface = colors.surfaces.raised;
        for level in 1..=rg.depth {
            let x = full_row.x0 + (px(level) - 0.5) * INDENT;
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                crate::surface::edge_on(surface),
                None,
                &Line::new(Point::new(x, full_row.y0), Point::new(x, full_row.y1)),
            );
        }

        // The band's backdrop is a step of the pane's own ramp under an edge
        // derived from the pane, so a band reads as a band on a light pane
        // too — the white veneers it replaces were one colour per theme.
        let radius = f64::from(theme.tokens.radii.detail);
        let shape = RoundedRect::from_rect(row, radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            wash(
                surface,
                if is_hovered {
                    WASH_BAND_HOVER
                } else {
                    WASH_HOVER
                },
            ),
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::surface::edge_on(surface),
            None,
            &crate::surface::edge_outline(row, radius),
        );
        if is_focused {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                colors.border_focus,
                None,
                &RoundedRect::from_rect(
                    row.inset(-1.0),
                    f64::from(theme.tokens.radii.detail) + 1.0,
                ),
            );
        }
        if rg.is_section {
            return;
        }
        // A named band gets an accent spine: the visual difference between
        // "someone decided this is a thing" and an automatic chunk.
        if rg.name.is_some() {
            let spine = RoundedRect::from_rect(
                Rect::new(row.x0, row.y0 + 4.0, row.x0 + 3.0, row.y1 - 4.0),
                1.5,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                colors.intent.inverted,
                None,
                &spine,
            );
        }

        // Disclosure triangle: right when collapsed, down when open.
        let tri_center = Point::new(row.x0 + 14.0, row.y0 + 15.0);
        let mut tri = BezPath::new();
        if rg.expanded {
            tri.move_to((tri_center.x - 4.5, tri_center.y - 2.5));
            tri.line_to((tri_center.x + 4.5, tri_center.y - 2.5));
            tri.line_to((tri_center.x, tri_center.y + 3.0));
        } else {
            tri.move_to((tri_center.x - 2.5, tri_center.y - 4.5));
            tri.line_to((tri_center.x + 3.0, tri_center.y));
            tri.line_to((tri_center.x - 2.5, tri_center.y + 4.5));
        }
        tri.close_path();
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.text_dim,
            None,
            &tri,
        );
    }

    /// The band title, or the in-place rename field when `naming` is set;
    /// plus the "still open" dot.
    fn draw_group_row_title(
        &self,
        ctx: &mut PaintCtx<'_>,
        band: &BandHeader<'_>,
        is_hovered: bool,
        naming: Option<&NameField>,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: tr,
            theme,
        } = ctx.reborrow();
        let &BandHeader {
            rg,
            row,
            title_x,
            ref title_style,
        } = band;
        let chip_x0 = if self.preview_selection {
            row.x1 + 8.0
        } else {
            self.group_chip_rects_full(row, rg).left_edge(row.x1)
        };
        let colors = &theme.tokens.colors;
        let title_y = row.y0 + 6.0;
        let actions_w = if is_hovered || naming.is_some() {
            row.x1 - chip_x0 + 8.0
        } else {
            0.0
        };
        let title_budget = (row.x1 - actions_w - title_x - 8.0).max(0.0);

        if let Some(field) = naming {
            // The field replaces the title in place, so the name is typed where
            // it will live.
            let box_rect = Rect::new(
                title_x - 4.0,
                row.y0 + 4.0,
                (row.x1 - actions_w).max(title_x + 40.0),
                row.y0 + 24.0,
            );
            field.draw(scene, transform, tr, theme, box_rect, title_style);
        } else {
            let title = fit_text(tr, &rg.title, title_style, title_budget);
            tr.draw_style(
                scene,
                &title,
                colors.text_primary,
                translated(transform, (title_x, title_y)),
                None,
                title_style,
            );
            if rg.open {
                // "Still open" has to read as live, not as a label — and sit
                // on the title's measured centre line, not the font size's.
                let (w, h) = tr.measure_style(&title, None, title_style);
                let dot_x = title_x + f64::from(w) + 8.0;
                if dot_x + 5.0 < row.x1 - actions_w {
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        colors.intent.inverted_hover,
                        None,
                        &Circle::new(Point::new(dot_x, title_y + f64::from(h) / 2.0), 3.0),
                    );
                }
            }
        }
    }

    /// Line two: subtitle, right-aligned relative time, and the packed
    /// author dots between them.
    fn draw_group_row_meta(
        ctx: &mut PaintCtx<'_>,
        band: &BandHeader<'_>,
        now: SystemTime,
        meta_y: f64,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: tr,
            theme,
        } = ctx.reborrow();
        let &BandHeader {
            rg, row, title_x, ..
        } = band;
        let colors = &theme.tokens.colors;
        let meta_style = theme.text(TextRole::Caption);
        let rel = format_relative_time(rg.last_time, now);
        let (rel_w, rel_h) = tr.measure_style(&rel, None, &meta_style);
        // Author dots sit between the subtitle and the timestamp; reserve them.
        let dots = rg.authors.len().min(3);
        let dots_w = if dots > 0 { px(dots) * 10.0 + 6.0 } else { 0.0 };
        let sub_budget =
            (row.x1 - title_x - f64::from(rel_w) - dots_w - TRAILING_INSET - 16.0).max(0.0);
        let subtitle = if rg.expanded {
            rg.subtitle.clone()
        } else {
            rg.scope_label.as_ref().map_or_else(
                || rg.subtitle.clone(),
                |scope| format!("{scope} · {}", rg.subtitle),
            )
        };
        let sub = fit_text(tr, &subtitle, &meta_style, sub_budget);
        tr.draw_style(
            scene,
            &sub,
            colors.text_dim,
            translated(transform, (title_x, meta_y)),
            None,
            &meta_style,
        );
        // Right-aligned to the same gutter as the rows' time column, so the
        // band header's "2 h ago" and the rows' "1 h ago" share one edge.
        tr.draw_style(
            scene,
            &rel,
            colors.text_dim,
            translated(
                transform,
                (row.x1 - TRAILING_INSET - f64::from(rel_w), meta_y),
            ),
            None,
            &meta_style,
        );

        // Author dots, packed tight, on the meta line's measured centre.
        let dot_cy = meta_y + f64::from(rel_h) / 2.0;
        let mut dx = row.x1 - TRAILING_INSET - 8.0 - f64::from(rel_w);
        for a in rg.authors.iter().take(3) {
            let (r, g, b) = a.rgb();
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                Color::from_rgba8(r, g, b, 255),
                None,
                &Circle::new(Point::new(dx, dot_cy), 4.0),
            );
            dx -= 10.0;
        }
    }

    /// Laid out right to left — `[Restore]`, `[Undo]`, `[Name]` — so the two
    /// document actions sit together. `[Undo]` only allocates when a host
    /// installed [`ActivityTimeline::on_undo_band`].
    fn group_chip_rects_full(&self, row: Rect, rg: &RowGroup) -> GroupChips {
        if self.preview_selection {
            return GroupChips {
                name: Rect::ZERO,
                undo: None,
                restore: None,
            };
        }
        let chip_h = 20.0;
        let cy = row.y0 + 15.0;
        let narrow = self.rect.width() < NARROW_WIDTH;
        let restore_w = if narrow { 52.0 } else { 62.0 };
        let undo_w = if narrow { 42.0 } else { 48.0 };
        let name_w = if narrow { 46.0 } else { 54.0 };
        let mut x1 = row.x1 - 6.0;
        let chip = |x1: f64, w: f64| Rect::new(x1 - w, cy - chip_h / 2.0, x1, cy + chip_h / 2.0);
        if rg.is_section {
            return GroupChips {
                name: Rect::ZERO,
                undo: None,
                restore: None,
            };
        }
        let restore = if self.on_revert.is_some() && rg.restorable {
            let r = chip(x1, restore_w);
            x1 -= restore_w + 5.0;
            Some(r)
        } else {
            None
        };
        let undo = if self.on_undo_band.is_some() && rg.restorable {
            let r = chip(x1, undo_w);
            x1 -= undo_w + 5.0;
            Some(r)
        } else {
            None
        };
        GroupChips {
            name: if self.on_name_group.is_some() {
                chip(x1, name_w)
            } else {
                Rect::ZERO
            },
            undo,
            restore,
        }
    }

    fn group_chip_rects_local(&self, row: Rect, rg: &RowGroup) -> (Rect, Option<Rect>) {
        let c = self.group_chip_rects_full(row, rg);
        (c.name, c.restore)
    }

    fn draw_filter_strip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let strip = self.filter_strip_rect();
        // The filter chips are the shared chip: quiet at rest, a lift when
        // chosen (HIG §9.5).
        let paint = FilterStripPaint {
            strip,
            chip_h: crate::widget::ChipFace::height(theme).min(strip.height()),
        };
        let chip_width = |tr: &mut TextRenderer, label: &str| {
            crate::widget::ChipFace::intrinsic_width(
                label,
                crate::widget::ChipLead::None,
                false,
                tr,
                theme,
            )
        };

        // Rebuild the chip-rect cache every frame so click hit-testing lines up
        // with what was just painted.
        self.chip_rects.clear();

        // The author pill is anchored right when this standalone widget owns
        // people filtering. RoomActivity can supply that control itself.
        let author_label = if self.author_query.is_empty() {
            "author:any".to_string()
        } else {
            format!("author:{}", self.author_query)
        };
        let author_full_w = if self.show_author_pill {
            chip_width(text_renderer, &author_label).min(strip.width() * 0.45)
        } else {
            0.0
        };
        let chips_budget = if self.show_author_pill {
            (strip.width() - author_full_w - 8.0).max(0.0)
        } else {
            strip.width()
        };

        let measure_run = |tr: &mut TextRenderer, short: bool| -> f64 {
            let mut total = 0.0;
            for chip in ChipKind::ALL {
                let l = if short {
                    chip.short_label()
                } else {
                    chip.label()
                };
                total += chip_width(tr, &l) + CHIP_GAP;
            }
            total
        };
        let use_short = measure_run(text_renderer, false) > chips_budget;

        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        self.draw_filter_chips(&mut ctx, &paint, chips_budget, use_short);
        if self.show_author_pill {
            self.draw_author_pill(&mut ctx, &paint, &author_label, author_full_w);
        } else {
            self.author_pill_rect = Rect::ZERO;
        }
    }

    /// The `[All] [Edits] [Comments] ...` chip row; populates
    /// [`Self::chip_rects`] so hit-testing lines up with what was painted.
    fn draw_filter_chips(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        paint: &FilterStripPaint,
        chips_budget: f64,
        use_short: bool,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let &FilterStripPaint { strip, chip_h } = paint;
        let mut x = strip.x0;
        for chip in ChipKind::ALL {
            let label = if use_short {
                chip.short_label()
            } else {
                chip.label()
            };
            let chip_w = crate::widget::ChipFace::intrinsic_width(
                &label,
                crate::widget::ChipLead::None,
                false,
                text_renderer,
                theme,
            );
            let cy = f64::midpoint(strip.y0, strip.y1);
            // Stop rather than paint a chip half-under the author pill; an
            // unpainted chip is not in `chip_rects`, so it cannot be clicked.
            if x + chip_w > strip.x0 + chips_budget && !self.chip_rects.is_empty() {
                break;
            }
            let chip_rect = Rect::new(x, cy - chip_h / 2.0, x + chip_w, cy + chip_h / 2.0);
            self.chip_rects
                .push((chip, self.hit_rect_within(chip_rect, strip)));
            crate::widget::ChipFace::new(chip_rect, &label)
                .selected(self.active_chip == chip)
                .paint(scene, transform, text_renderer, theme);
            x += chip_w + CHIP_GAP;
        }
    }

    /// The right-anchored `author:...` pill.
    fn draw_author_pill(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        paint: &FilterStripPaint,
        author_label: &str,
        author_full_w: f64,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let &FilterStripPaint { strip, chip_h } = paint;
        let cy = f64::midpoint(strip.y0, strip.y1);
        let arx1 = strip.x1;
        let arx0 = arx1 - author_full_w;
        let chip_rect = Rect::new(arx0, cy - chip_h / 2.0, arx1, cy + chip_h / 2.0);
        self.author_pill_rect = self.hit_rect_within(chip_rect, strip);
        crate::widget::ChipFace::new(chip_rect, author_label)
            .selected(!self.author_query.is_empty())
            .paint(scene, transform, text_renderer, theme);
    }

    fn draw_selection_bar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.name_selection_rect = Rect::ZERO;
        self.clear_selection_rect = Rect::ZERO;
        let Some((a, b)) = self.selection else {
            return;
        };
        let bar = self.selection_bar_rect();
        if bar.height() < 20.0 {
            return;
        }
        let colors = &theme.tokens.colors;
        // Overlay bar under an edge derived from it: the bar announces a
        // selection through its copy and actions, not by wearing the focus
        // ring — `border_focus` means keyboard focus (HIG §9.4/§9.5).
        crate::surface::card(
            scene,
            transform,
            bar,
            f64::from(theme.tokens.radii.detail),
            colors.surfaces.overlay,
        );

        let style = theme.text(TextRole::Caption);
        // `selection` is normalised so `b >= a` (see `set_selection`).
        let n = b.saturating_sub(a).saturating_add(1);
        let naming = matches!(
            self.naming.as_ref().map(|s| &s.target),
            Some(NameTarget::Selection)
        );

        let clear_label = crate::i18n!("history-selection-clear");
        let name_label = crate::i18n!("history-selection-name");
        let chip_w = |tr: &mut TextRenderer, label: &str, min: f64| {
            (f64::from(tr.measure_style(label, None, &style).0) + 20.0).max(min)
        };
        let clear_w = chip_w(tr, &clear_label, 54.0);
        let action_w = if naming || self.preview_selection || self.on_name_group.is_none() {
            0.0
        } else {
            chip_w(tr, &name_label, 96.0)
        };
        let clear = Rect::new(
            bar.x1 - 8.0 - clear_w,
            bar.center().y - 11.0,
            bar.x1 - 8.0,
            bar.center().y + 11.0,
        );
        self.clear_selection_rect = clear;
        draw_chip(scene, transform, tr, theme, clear, &clear_label, false);

        let label = crate::i18n!("history-selection-count", "count" => n);
        let label_budget = (bar.width() - clear_w - action_w - 40.0).max(0.0);
        let shown = fit_text(tr, &label, &style, label_budget);
        let label_top = text_top(tr, &style, bar.center().y);
        tr.draw_style(
            scene,
            &shown,
            colors.text_primary,
            translated(transform, (bar.x0 + 10.0, label_top)),
            None,
            &style,
        );

        if naming {
            let field_x0 =
                bar.x0 + 10.0 + f64::from(tr.measure_style(&shown, None, &style).0) + 12.0;
            let field = Rect::new(
                field_x0,
                bar.center().y - 12.0,
                clear.x0 - 8.0,
                bar.center().y + 12.0,
            );
            if field.width() > 40.0
                && let Some(state) = self.naming.as_ref()
            {
                state.field.draw(scene, transform, tr, theme, field, &style);
            }
        } else if self.on_name_group.is_some() {
            let action = Rect::new(
                clear.x0 - 6.0 - action_w,
                bar.center().y - 11.0,
                clear.x0 - 6.0,
                bar.center().y + 11.0,
            );
            if !self.preview_selection && action.x0 > bar.x0 + 10.0 {
                self.name_selection_rect = action;
                draw_chip(scene, transform, tr, theme, action, &name_label, true);
            }
        }
    }
}

impl Widget for ActivityTimeline {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let prev = self.rect;
        self.rect = crate::widget::placed(&layout, origin);
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
        self.min_pointer_target = f64::from(theme.tokens.metrics.min_pointer_target);
        if self.draws_own_surface {
            self.draw_surface_chrome(scene, transform, theme);
        }

        self.draw_filter_strip(scene, transform, text_renderer, theme);

        // Nothing-has-happened and filter-is-hiding-activity are distinct
        // cases; rendering them identically reads as broken.
        if self.rows.is_empty() {
            self.draw_empty_state(scene, transform, text_renderer, theme);
            self.dirty = false;
            return;
        }
        self.clear_chip_rect = Rect::ZERO;

        self.draw_rows(scene, transform, text_renderer, theme);

        self.draw_selection_bar(scene, transform, text_renderer, theme);

        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::Ime(_) if self.naming.is_some() => {
                self.handle_naming_ime(event, *ctx.modifiers, ctx.handled);
            }
            // A name field owns the keyboard while it is up, so a version
            // called "edits" cannot accidentally re-filter the pane.
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.naming.is_some()
                && !ctx.modifiers.state().control_key()
                && !ctx.modifiers.state().super_key() =>
            {
                self.handle_naming_key(event, *ctx.modifiers, ctx.handled);
            }
            // Author typeahead. Chorded keys are left alone so the host's
            // own shortcuts still work while the pane has the keyboard.
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if !ctx.modifiers.state().control_key()
                && !ctx.modifiers.state().super_key()
                && !ctx.modifiers.state().alt_key() =>
            {
                self.handle_timeline_navigation_key(
                    logical_key,
                    ctx.modifiers.state().shift_key(),
                    ctx.handled,
                );
                if !*ctx.handled {
                    self.handle_author_typeahead_key(logical_key, ctx.handled);
                }
            }
            // An input method's text types into the author filter as keys do.
            WindowEvent::Ime(winit::event::Ime::Commit(text)) if self.naming.is_none() => {
                let text: String = text.chars().filter(|ch| !ch.is_control()).collect();
                if !text.is_empty() {
                    self.author_query.push_str(&text);
                    self.rebuild();
                    *ctx.handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position, ctx.scale_factor, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_click(ctx.handled, ctx.focus_target, ctx.node_id, *ctx.modifiers);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(delta, ctx.handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ActivityTimeline"
    }

    fn is_focusable(&self) -> bool {
        !self.rows.is_empty()
    }

    fn set_focus(&mut self, focused: bool) {
        if self.has_focus != focused {
            self.has_focus = focused;
            if focused && self.focused_row.is_none() && !self.rows.is_empty() {
                let _ = self.focus_row(0);
            }
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.has_focus
    }

    /// The description carries both counts, because "7 items" over a list
    /// of three collapsed bands would not describe what is on screen.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let groups = self
            .rows
            .iter()
            .filter(|r| matches!(r, VisualRow::Group(_)))
            .count();
        let description = if groups == 0 {
            format!("{} entries", self.visible.len())
        } else {
            format!("{} entries in {} groups", self.visible.len(), groups)
        };
        let row_count = self.rows.len();
        let children = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let bounds = self.row_rect_unclipped(index)?;
                match row {
                    VisualRow::Group(group) => {
                        let item = self.row_groups.get(*group)?;
                        let description = if item.expanded {
                            item.subtitle.clone()
                        } else {
                            item.scope_label.as_ref().map_or_else(
                                || item.subtitle.clone(),
                                |scope| format!("{scope} · {}", item.subtitle),
                            )
                        };
                        Some(
                            AccessNode::new(Role::TreeItem)
                                .with_action_target(format!("timeline-group:{}", item.key))
                                .with_name(item.title.clone())
                                .with_description(description)
                                .with_position(index.saturating_add(1), row_count)
                                .with_level(item.depth.saturating_add(1))
                                .with_bounds(bounds)
                                .with_state(AccessState {
                                    focusable: true,
                                    focused: self.focused_row.as_ref()
                                        == Some(&FocusedRow::Group(item.key.clone())),
                                    expanded: Some(item.expanded),
                                    ..Default::default()
                                }),
                        )
                    }
                    VisualRow::Entry(entry, depth) => {
                        let item = self.entries.get(*entry)?;
                        let id = crate::widget::history_group::hex16(&item.id);
                        let said = if item.is_genesis() {
                            item.summary.clone()
                        } else {
                            format!("{}: {}", item.author.name, item.summary)
                        };
                        let name = Self::entry_breadcrumb(item).map_or_else(
                            || said.clone(),
                            |breadcrumb| format!("{breadcrumb}: {said}"),
                        );
                        Some(
                            AccessNode::new(Role::ListItem)
                                .with_action_target(format!("timeline-entry:{id}"))
                                .with_name(name)
                                .with_description(
                                    [
                                        op_class_glyph(item.op_class).to_owned(),
                                        row_time(item, self.now()),
                                    ]
                                    .into_iter()
                                    .filter(|part| !part.is_empty())
                                    .collect::<Vec<_>>()
                                    .join(" · "),
                                )
                                .with_position(index.saturating_add(1), row_count)
                                .with_level(depth.saturating_add(1))
                                .with_bounds(bounds)
                                .with_state(AccessState {
                                    focusable: true,
                                    focused: self.focused_row.as_ref()
                                        == Some(&FocusedRow::Entry(item.id)),
                                    selected: self.selection.is_some_and(|(start, end)| {
                                        *entry >= start && *entry <= end
                                    }),
                                    ..Default::default()
                                }),
                        )
                    }
                }
            })
            .collect();
        Some(
            AccessNode::new(Role::List)
                .with_name("History entries")
                .with_description(description)
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.has_focus,
                    ..Default::default()
                })
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        let row = if let Some(key) = target.strip_prefix("timeline-group:") {
            self.rows
                .iter()
                .enumerate()
                .find_map(|(index, row)| match row {
                    VisualRow::Group(group)
                        if self
                            .row_groups
                            .get(*group)
                            .is_some_and(|item| item.key == key) =>
                    {
                        Some(index)
                    }
                    _ => None,
                })
        } else if let Some(hex_id) = target.strip_prefix("timeline-entry:") {
            self.rows
                .iter()
                .enumerate()
                .find_map(|(index, row)| match row {
                    VisualRow::Entry(entry, _)
                        if self.entries.get(*entry).is_some_and(|item| {
                            crate::widget::history_group::hex16(&item.id) == hex_id
                        }) =>
                    {
                        Some(index)
                    }
                    _ => None,
                })
        } else {
            None
        };
        let Some(row) = row else {
            return false;
        };
        match action {
            Action::Focus => self.focus_row(row),
            Action::Activate => self.activate_row(row),
            Action::ScrollIntoView => {
                self.scroll_row_into_view(row);
                true
            }
            Action::Expand
                if self
                    .group_at_row(row)
                    .is_some_and(|(_, expanded)| !expanded) =>
            {
                self.toggle_group_at_row(row)
            }
            Action::Collapse if self.group_at_row(row).is_some_and(|(_, expanded)| expanded) => {
                self.toggle_group_at_row(row)
            }
            _ => false,
        }
    }

    fn test_id(&self) -> Option<&str> {
        Some("activity_timeline")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.naming
            .as_ref()
            .and_then(|naming| naming.field.ime_cursor_rect())
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        let naming = self.naming.as_ref()?;
        let scope = format!("{:?}:{:?}", naming.target, self.selection);
        Some(crate::ime::scoped_fingerprint(
            scope.as_bytes(),
            naming.field.ime_edit_fingerprint(key)?,
            key,
        ))
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Some(winit::window::ImePurpose::Normal)
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

impl ActivityTimeline {
    /// The pane's own rounded backdrop, painted only when no host has
    /// already put a surface behind it.
    fn draw_surface_chrome(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        // Raised card under an edge derived from it: the declared
        // `border_standard` this replaces is three shades from the dark
        // preset's raised surface and vanished there (HIG §9.5).
        crate::surface::card(
            scene,
            transform,
            self.rect,
            f64::from(theme.tokens.radii.control),
            theme.tokens.colors.surfaces.raised,
        );
    }

    /// The "nothing here" / "nothing matches the filter" centered message,
    /// plus `[Clear filters]` when a filter is the reason the list is empty.
    fn draw_empty_state(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let (title, detail) = self.empty_state_lines();
        let style = theme.text(TextRole::Body);
        let small = theme.text(TextRole::Caption);
        let rows_top = self.rows_top();
        let cx = f64::midpoint(self.rect.x0, self.rect.x1);
        let cy = (rows_top + self.rect.y1 - PADDING) / 2.0;
        let budget = (self.rect.width() - 2.0 * PADDING - 16.0).max(0.0);

        let title = fit_text(text_renderer, &title, &style, budget);
        let (w, h) = text_renderer.measure_style(&title, None, &style);
        let title_y = if detail.is_some() {
            cy - f64::from(h) - 14.0
        } else {
            cy - f64::from(h) / 2.0
        };
        text_renderer.draw_style(
            scene,
            &title,
            theme.tokens.colors.text_dim,
            translated(transform, (cx - f64::from(w) / 2.0, title_y)),
            None,
            &style,
        );

        self.clear_chip_rect = Rect::ZERO;
        if let Some(detail) = detail {
            let detail = fit_text(text_renderer, &detail, &small, budget);
            let (dw, dh) = text_renderer.measure_style(&detail, None, &small);
            let detail_y = title_y + f64::from(h) + 8.0;
            text_renderer.draw_style(
                scene,
                &detail,
                theme.tokens.colors.text_dim,
                translated(transform, (cx - f64::from(dw) / 2.0, detail_y)),
                None,
                &small,
            );

            // `[Clear filters]` — the way back out. Without it the user has
            // to remember which chip they pressed to get here.
            let label = "Clear filters";
            let (lw, lh) = text_renderer.measure_style(label, None, &small);
            let chip_w = (f64::from(lw) + 20.0).min(budget.max(24.0));
            let chip_h = f64::from(lh) + 10.0;
            let chip_y0 = detail_y + f64::from(dh) + 12.0;
            if chip_y0 + chip_h <= self.rect.y1 - PADDING {
                let chip = Rect::new(
                    cx - chip_w / 2.0,
                    chip_y0,
                    cx + chip_w / 2.0,
                    chip_y0 + chip_h,
                );
                self.clear_chip_rect = chip;
                draw_chip(scene, transform, text_renderer, theme, chip, label, false);
            }
        }
    }

    /// Paints only the lines the viewport can show, so a room with ten
    /// thousand entries costs the same per frame as one with ten.
    fn draw_rows(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = self.now();
        let selection = self.selection;
        let focused_row = self.focused_row();
        // Clip to the row band: without it the line straddling the bottom
        // edge paints over the selection bar and past the pane edge.
        let clip = Rect::new(
            self.rect.x0,
            self.rows_top(),
            self.rect.x1,
            self.rows_bottom(),
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &clip,
        );
        for i in self.visible_row_range() {
            let Some(row) = self.row_rect(i) else {
                continue;
            };
            let emphasis = RowEmphasis {
                hovered: self.hover_row == Some(i),
                selected: false,
                focused: focused_row == Some(i),
            };
            let Some(&visual_row) = self.rows.get(i) else {
                continue;
            };
            let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
            match visual_row {
                VisualRow::Group(g) => {
                    self.draw_group_row(&mut ctx, row, g, emphasis, now);
                }
                VisualRow::Entry(e, depth) => {
                    let Some(entry) = self.entries.get(e) else {
                        continue;
                    };
                    let emphasis = RowEmphasis {
                        selected: selection.is_some_and(|(a, b)| e >= a && e <= b),
                        ..emphasis
                    };
                    self.draw_row(&mut ctx, RowSlot { rect: row, depth }, entry, emphasis, now);
                }
            }
        }

        scene.pop_layer();
    }

    fn handle_naming_ime(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        let outcome = self.naming.as_mut().map_or(NameFieldOutcome::Ignored, |n| {
            n.field.handle_event(event, &modifiers)
        });
        if outcome == NameFieldOutcome::Edited {
            self.dirty = true;
            *handled = true;
        }
    }

    fn handle_naming_key(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        let outcome = self.naming.as_mut().map_or(NameFieldOutcome::Ignored, |n| {
            n.field.handle_event(event, &modifiers)
        });
        match outcome {
            NameFieldOutcome::Commit(_) => {
                self.commit_naming();
                *handled = true;
            }
            NameFieldOutcome::Cancel => {
                self.cancel_naming();
                *handled = true;
            }
            NameFieldOutcome::Edited => {
                self.dirty = true;
                *handled = true;
            }
            NameFieldOutcome::Ignored => {}
        }
    }

    /// Roving focus for the custom-painted list. This runs before author
    /// typeahead, so Space and directional keys are always list controls.
    fn handle_timeline_navigation_key(
        &mut self,
        logical_key: &Key,
        shift: bool,
        handled: &mut bool,
    ) {
        let len = self.rows.len();
        if len == 0 {
            return;
        }
        let last = len.saturating_sub(1);
        let current = self.focused_row().unwrap_or(0);
        let destination = match logical_key {
            Key::Named(NamedKey::ArrowDown) => Some(current.saturating_add(1).min(last)),
            Key::Named(NamedKey::ArrowUp) => Some(current.saturating_sub(1)),
            Key::Named(NamedKey::Home) => Some(0),
            Key::Named(NamedKey::End) => Some(last),
            Key::Named(NamedKey::ArrowRight) => {
                if self
                    .group_at_row(current)
                    .is_some_and(|(_, expanded)| !expanded)
                {
                    let _ = self.toggle_group_at_row(current);
                } else {
                    let next = current.saturating_add(1);
                    if self.group_at_row(current).is_some() && next < self.rows.len() {
                        let _ = self.focus_row(next);
                    }
                }
                *handled = true;
                return;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if self
                    .group_at_row(current)
                    .is_some_and(|(_, expanded)| expanded)
                {
                    let _ = self.toggle_group_at_row(current);
                } else {
                    let depth = self.row_depth(current);
                    if let Some(parent) = (0..current).rev().find(|candidate| {
                        self.group_at_row(*candidate).is_some()
                            && self.row_depth(*candidate) < depth
                    }) {
                        let _ = self.focus_row(parent);
                    }
                }
                *handled = true;
                return;
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let _ = self.activate_focused_row();
                *handled = true;
                return;
            }
            Key::Named(NamedKey::Escape) => {
                if !self.author_query.is_empty() {
                    self.author_query.clear();
                    self.rebuild();
                } else if self.selection.take().is_some() {
                    self.selection_anchor = None;
                    self.rebuild_rows();
                }
                *handled = true;
                return;
            }
            _ => None,
        };
        let Some(destination) = destination else {
            return;
        };
        let _ = self.focus_row(destination);
        if let Some(entry) = self.row_entry_index(destination) {
            if shift {
                let anchor = self.selection_anchor.unwrap_or(entry);
                self.selection = Some((anchor.min(entry), anchor.max(entry)));
            } else {
                self.selection = Some((entry, entry));
                self.selection_anchor = Some(entry);
            }
            self.rebuild_rows();
        }
        *handled = true;
    }

    fn handle_author_typeahead_key(&mut self, logical_key: &Key, handled: &mut bool) {
        match logical_key {
            Key::Character(c) => {
                let text: String = c.chars().filter(|ch| !ch.is_control()).collect();
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
                if crate::text_edit::delete_last_grapheme(&mut self.author_query) {
                    self.rebuild();
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Escape) => {
                if !self.author_query.is_empty() {
                    self.author_query.clear();
                    self.rebuild();
                    *handled = true;
                } else if self.selection.take().is_some() {
                    self.selection_anchor = None;
                    self.rebuild_rows();
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn handle_cursor_moved(
        &mut self,
        position: &winit::dpi::PhysicalPosition<f64>,
        scale_factor: f64,
        handled: &mut bool,
    ) {
        let p = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.last_cursor = p;
        let new_hover = self.row_at(p);
        if new_hover != self.hover_row {
            self.hover_row = new_hover;
            self.dirty = true;
            *handled = true;
        }
    }

    fn handle_mouse_wheel(&mut self, delta: &MouseScrollDelta, handled: &mut bool) {
        const LINE_PX: f64 = 60.0;
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * LINE_PX,
            MouseScrollDelta::PixelDelta(pp) => -pp.y,
        };
        let new_scroll = (self.scroll_y + dy).clamp(0.0, self.max_scroll());
        if (new_scroll - self.scroll_y).abs() > f64::EPSILON {
            self.scroll_y = new_scroll;
            self.dirty = true;
            *handled = true;
            // After the scroll, the line under the cursor may have
            // changed — re-evaluate.
            self.hover_row = self.row_at(self.last_cursor);
        }
    }

    /// A left click, in hit-test priority order: `[Clear filters]`, the
    /// selection bar, the filter strip, then the row underneath the cursor.
    fn handle_left_click(
        &mut self,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        modifiers: winit::event::Modifiers,
    ) {
        // `clear_chip_rect` is zeroed the moment the empty state is
        // not painted, so this can never fire on a stale hit zone.
        if self.clear_chip_rect.area() > 0.0 && self.clear_chip_rect.contains(self.last_cursor) {
            self.clear_filters();
            self.dirty = true;
            *handled = true;
            return;
        }

        // The selection bar sits over the rows; it takes the click.
        let bar = self.selection_bar_rect();
        if bar.area() > 0.0 && bar.contains(self.last_cursor) {
            if self.clear_selection_rect.contains(self.last_cursor) {
                self.naming = None;
                self.selection = None;
                self.selection_anchor = None;
                self.rebuild_rows();
            } else if self.name_selection_rect.area() > 0.0
                && self.name_selection_rect.contains(self.last_cursor)
            {
                self.begin_naming_selection();
                *focus_target = Some(node_id);
            }
            *handled = true;
            return;
        }

        // The chip strip steals the click before the row hit-test.
        let strip = self.filter_strip_rect();
        if strip.contains(self.last_cursor) {
            self.handle_click_in_filter_strip(handled);
            return;
        }

        let Some(i) = self.hover_row else {
            return;
        };
        match self.rows.get(i).copied() {
            Some(VisualRow::Group(g)) => {
                self.handle_click_on_group_row(i, g, handled, focus_target, node_id);
            }
            Some(VisualRow::Entry(e, _)) => {
                self.handle_click_on_entry_row(i, e, handled, modifiers);
            }
            None => {}
        }
    }

    fn handle_click_in_filter_strip(&mut self, handled: &mut bool) {
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
        }
        // Inside the strip but not on a chip or the author pill — eat it so
        // the row hit-test below does not fire.
    }

    fn handle_click_on_group_row(
        &mut self,
        i: usize,
        g: usize,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
    ) {
        if self.row_groups.get(g).is_some_and(|rg| rg.is_section) {
            *handled = true;
            return;
        }
        // Undo hit-tested before restore: a click on Undo
        // must never fall through to the destructive Restore.
        if let Some(u) = self.group_undo_chip_rect(i)
            && u.contains(self.last_cursor)
        {
            if let Some(req) = self.band_undo_request(i)
                && let Some(cb) = self.on_undo_band.as_mut()
            {
                cb(&req);
            }
            *handled = true;
            return;
        }
        if let Some((name, restore)) = self.group_chip_rects(i) {
            if let Some(r) = restore
                && r.contains(self.last_cursor)
            {
                if let Some(id) = self.row_groups.get(g).map(|rg| rg.last_entry)
                    && let Some(cb) = self.on_revert.as_mut()
                {
                    cb(&id);
                }
                *handled = true;
                return;
            }
            if name.area() > 0.0 && name.contains(self.last_cursor) {
                self.begin_naming_group_at_row(i);
                *focus_target = Some(node_id);
                *handled = true;
                return;
            }
        }
        self.naming = None;
        self.toggle_group_at_row(i);
        *handled = true;
    }

    fn handle_click_on_entry_row(
        &mut self,
        i: usize,
        e: usize,
        handled: &mut bool,
        modifiers: winit::event::Modifiers,
    ) {
        if let Some(chip) = self.revert_chip_rect(i)
            && chip.contains(self.last_cursor)
        {
            if let Some(id) = self.entries.get(e).map(|entry| entry.id)
                && let Some(cb) = self.on_revert.as_mut()
            {
                cb(&id);
            }
            *handled = true;
            return;
        }
        // Click selects; shift-click extends. This is the range
        // gesture for naming a run the automatic boundaries did
        // not draw.
        self.naming = None;
        if modifiers.state().shift_key()
            && let Some(anchor) = self.selection_anchor
        {
            self.selection = Some((anchor.min(e), anchor.max(e)));
        } else if self.selection == Some((e, e)) {
            self.selection = None;
            self.selection_anchor = None;
        } else {
            self.selection_anchor = Some(e);
            self.selection = Some((e, e));
        }
        self.rebuild_rows();
        *handled = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic_input::{cursor_moved, mouse_press};
    use crate::time::Duration;
    use crate::widget::history::HistoryAuthor;
    use automerge::AutoCommit;
    use collab_types::ArtifactKind;
    use std::cell::RefCell;
    use std::rc::Rc;
    use winit::event::Modifiers;

    /// Relative luminance of the composited pixel at (`x`, `y`).
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "probe coordinates are non-negative pixel positions inside the test surface"
    )]
    fn lum_at(rgba: &[u8], w: u32, x: f64, y: f64) -> f64 {
        let (r, g, b, _) = crate::offscreen::OffscreenRenderer::sample(
            rgba,
            w,
            x.floor() as u32,
            y.floor() as u32,
        );
        crate::a11y::relative_luminance(Color::from_rgb8(r, g, b))
    }

    /// `$UX1X_F4_OUT/<theme>/<name>` when the 1× review asked for evidence.
    fn ux1x_out(theme: &Theme, name: &str) -> Option<std::path::PathBuf> {
        let dir = std::path::PathBuf::from(std::env::var_os("UX1X_F4_OUT")?)
            .join(theme.name.to_lowercase());
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join(name))
    }

    /// Eight changes by two authors — enough for a band and loose rows —
    /// with two rows selected so the selection bar and its chips paint.
    fn ux1x_fixture() -> ActivityTimeline {
        let entries: Vec<HistoryEntry> = (0..8u8)
            .map(|i| {
                let who = if i < 5 { "Alice" } else { "Bob" };
                entry(OpClass::Splice, who, 1_000 + u64::from(i) * 30, i)
            })
            .collect();
        let mut t = ActivityTimeline::with_entries(entries);
        t.set_now_for_test(
            SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(2_000))
                .unwrap_or(SystemTime::UNIX_EPOCH),
        );
        t.set_all_expanded(true);
        t.set_selection(Some((5, 6)));
        t
    }

    #[test]
    fn provenance_and_author_pill_presentation() {
        /// The pane, its quiet chips, the band backdrops and the selected rows
        /// keep a **visible derived edge** at 1× on both presets (HIG §9.5).
        /// Before, the pane wore `border_standard` (three shades from the dark
        /// raised surface), chips and bands literal white veneers, hover
        /// `neutral[3]`, and the selection bar the focus ring.
        ///
        /// Writes `$UX1X_F4_OUT/<theme>/activity_timeline.png` plus the harness
        /// inspection when that variable is set; asserts regardless.
        fn ux1x_pane_chips_and_bands_keep_a_derived_edge_on_both_presets() {
            use crate::offscreen::OffscreenRenderer;
            const EDGE_MIN: f64 = 0.008;
            let margin = 16.0;
            let (pw, ph) = (520.0, 520.0);
            // Evidence first, for both presets, so a failing assertion on one
            // preset still leaves the other's PNG and inspection on disk.
            for theme in [Theme::dark(), Theme::light()] {
                let theme = theme.with_reduce_motion(true);
                if let Some(png) = ux1x_out(&theme, "activity_timeline.png") {
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "a small test viewport"
                    )]
                    let harness = crate::headless::FrameHarness::new_with_theme(
                        crate::State::new(),
                        Box::new(ux1x_fixture()),
                        (pw as u32, ph as u32),
                        theme.clone(),
                    );
                    harness.render_png(&png);
                    if let Ok(json) = harness.inspection_json() {
                        std::fs::write(png.with_extension("inspection.json"), json)
                            .expect("write inspection");
                    }
                }
            }
            for theme in [Theme::dark(), Theme::light()] {
                let theme = theme.with_reduce_motion(true);
                let mut t = ux1x_fixture();
                let pane = Rect::new(margin, margin, margin + pw, margin + ph);
                t.set_rect(pane);
                let (w, h) = (pane.x1 + margin, pane.y1 + margin);
                let mut tr = TextRenderer::new();
                let mut scene = Scene::new();
                t.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut tr,
                    &theme,
                    &PresenceCtx::empty(),
                );
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a small test surface"
                )]
                let rgba = OffscreenRenderer::new(w as u32, h as u32)
                    .render(&scene, theme.tokens.colors.surfaces.content);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a small test surface"
                )]
                let lum = |x: f64, y: f64| lum_at(&rgba, w as u32, x, y);

                // Pane edge against its own fill, on the pane's centre line (clear
                // of the corner arcs; rows start one `PADDING` in, so the columns
                // beside the edge are bare fill).
                let y = pane.center().y;
                let edge = lum(pane.x0, y);
                let fill = lum(pane.x0 + 3.0, y);
                assert!(
                    (edge - fill).abs() >= EDGE_MIN,
                    "{}: pane edge {edge:.4} vs fill {fill:.4} did not register",
                    theme.name
                );

                // Quiet chip (the second filter chip, not the active first one):
                // its top edge — the straight run of the capsule, where the 1 px
                // stroke covers the whole pixel row — against its own fill just
                // under it, above the label's line box. `chip_rects` holds the
                // pointer target, so recover the painted capsule from the strip.
                let (_, hit) = t.chip_rects.get(1).copied().expect("a second filter chip");
                let strip = t.filter_strip_rect();
                let chip_h = (strip.height() - 8.0).max(20.0);
                let chip_top = strip.center().y - chip_h / 2.0;
                let cx = hit.center().x;
                let cedge = lum(cx, chip_top);
                let cfill = lum(cx, chip_top + 3.0);
                assert!(
                    (cedge - cfill).abs() >= EDGE_MIN,
                    "{}: chip edge {cedge:.4} vs fill {cfill:.4} did not register",
                    theme.name
                );

                // Band backdrop against the pane beside its inset.
                let band_row = (0..t.row_count())
                    .find(|i| t.group_at_row(*i).is_some())
                    .expect("two authors' runs produce a band");
                let band = t.row_rect(band_row).expect("the band row is laid out");
                let inner = Rect::new(band.x0, band.y0 + 2.0, band.x1, band.y1 - 2.0);
                let bfill = lum(inner.x1 - 3.0, inner.center().y);
                let pane_fill = lum(pane.x1 - PADDING / 2.0, inner.center().y);
                assert!(
                    (bfill - pane_fill).abs() >= EDGE_MIN,
                    "{}: band {bfill:.4} vs pane {pane_fill:.4} did not register",
                    theme.name
                );
            }
        }

        fn provenance_breadcrumbs_and_collapsed_mixed_scope_survive_hidden_rows() {
            use tilo_actions::ActionContext;
            let mut files = entry(OpClass::Splice, "Alice", 10, 1);
            files.context = Some(
                ActionContext::for_resource("files:brief", "files.entry", "brief.md")
                    .with_app("org.tilo.Files", "Files")
                    .with_group("workspace:a", "Alpha", "workspace"),
            );
            let mut notes = entry(OpClass::Splice, "Alice", 11, 2);
            notes.context = Some(
                ActionContext::for_resource("notes:plan", "notes.document", "plan")
                    .with_app("org.tilo.Notes", "Notes")
                    .with_group("workspace:a", "Beta", "workspace"),
            );
            let mut timeline = timeline_at(Rect::new(0.0, 0.0, 300.0, 400.0), vec![files, notes]);
            assert_eq!(
                ActivityTimeline::entry_breadcrumb(&timeline.entries()[0]).as_deref(),
                Some("Files / brief.md")
            );
            timeline.set_all_expanded(false);
            let group = (0..timeline.row_count())
                .find(|row| timeline.group_at_row(*row).is_some())
                .expect("collapsed group");
            let semantics = Widget::accessibility(&timeline).expect("timeline semantics");
            let description = semantics.children[group]
                .description
                .as_deref()
                .unwrap_or_default();
            assert!(description.contains("Alpha") && description.contains("Files"));
            assert!(description.contains("Beta") && description.contains("Notes"));
        }

        fn host_can_suppress_duplicate_author_pill_without_hiding_op_filters() {
            let mut timeline = timeline_at(Rect::new(0.0, 0.0, 300.0, 400.0), vec![]);
            assert!(timeline.shows_author_pill());
            timeline.set_show_author_pill(false);
            assert!(!timeline.shows_author_pill());
            assert_eq!(timeline.author_pill_rect(), Rect::ZERO);
        }

        ux1x_pane_chips_and_bands_keep_a_derived_edge_on_both_presets();
        provenance_breadcrumbs_and_collapsed_mixed_scope_survive_hidden_rows();
        host_can_suppress_duplicate_author_pill_without_hiding_op_filters();
    }

    fn entry(op: OpClass, author: &str, ts: u64, id: u8) -> HistoryEntry {
        let mut id_bytes = [0u8; 16];
        id_bytes[0] = id;
        HistoryEntry {
            id: HistoryEntryId(id_bytes),
            author: HistoryAuthor::new(author, 120.0, "deadbeef"),
            op_class: op,
            artifact_kind: ArtifactKind::Note,
            timestamp: SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(ts))
                .unwrap_or(SystemTime::UNIX_EPOCH),
            summary: format!("op {id} by {author}"),
            text_delta: Some((1, 0)),
            diff_payload: None,
            redaction: None,
            context: None,
        }
    }

    fn timeline_at(rect: Rect, entries: Vec<HistoryEntry>) -> ActivityTimeline {
        let mut t = ActivityTimeline::with_entries(entries);
        t.rect = rect;
        t.set_now_for_test(
            SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(2_000_000))
                .unwrap_or(SystemTime::UNIX_EPOCH),
        );
        t.set_newest_first(false); // ascending order for clarity
        t
    }

    // Geometry and naming-input tests explicitly install the capabilities they
    // exercise; the ordinary fixture remains a read-only history viewer.
    fn editable_timeline_at(rect: Rect, entries: Vec<HistoryEntry>) -> ActivityTimeline {
        timeline_at(rect, entries)
            .on_revert(|_| {})
            .on_name_group(|_| {})
    }

    #[test]
    fn a_genesis_row_names_no_author_and_no_time() {
        use crate::a11y::Role;
        let mut genesis = entry(OpClass::EditMeta, "Unknown participant", 0, 1);
        genesis.summary = "Created".into();
        genesis.context = Some(tilo_actions::ActionContext {
            facets: vec![crate::widget::history::genesis_facet()],
            ..tilo_actions::ActionContext::default()
        });
        let entries = vec![genesis, entry(OpClass::Splice, "Alice", 10, 2)];
        let t = timeline_at(Rect::new(0.0, 0.0, 300.0, 400.0), entries);
        let semantics = Widget::accessibility(&t).expect("timeline semantics");
        let rows: Vec<_> = semantics
            .children
            .iter()
            .filter(|node| node.role == Role::ListItem)
            .map(|node| (node.name.clone(), node.description.clone()))
            .collect();
        assert_eq!(
            rows[0],
            (
                Some("Created".to_owned()),
                Some(op_class_glyph(OpClass::EditMeta).to_owned())
            ),
            "the version the document was created as, by no one at no time"
        );
        assert_eq!(rows[1].0.as_deref(), Some("Alice: op 2 by Alice"));
        assert!(
            rows[1]
                .1
                .as_deref()
                .is_some_and(|text| text.contains(" · "))
        );
    }

    #[test]
    fn timeline_rows_are_accessible_keyboard_and_hover_targets() {
        fn accessibility_reports_list_role_name_and_count() {
            use crate::a11y::Role;
            let entries = vec![
                entry(OpClass::Splice, "Alice", 10, 1),
                entry(OpClass::Splice, "Bob", 20, 2),
            ];
            let t = timeline_at(Rect::new(0.0, 0.0, 300.0, 400.0), entries);
            let node = Widget::accessibility(&t).expect("timeline is not presentational");
            assert_eq!(node.role, Role::List);
            assert_eq!(node.name.as_deref(), Some("History entries"));
            assert_eq!(node.description.as_deref(), Some("2 entries"));
        }

        fn individual_entries_have_stable_accessible_targets_and_activate() {
            use crate::a11y::{Action, Role};
            let activated = Rc::new(RefCell::new(Vec::new()));
            let received = activated.clone();
            let entries = vec![entry(OpClass::Splice, "Alice", 10, 1)];
            let mut timeline = timeline_at(Rect::new(0.0, 0.0, 300.0, 400.0), entries)
                .on_activate_entry(move |id| received.borrow_mut().push(*id));
            let semantics = Widget::accessibility(&timeline).expect("timeline semantics");
            let row = semantics
                .children
                .iter()
                .find(|node| node.role == Role::ListItem)
                .expect("individual entry semantic row");
            assert!(row.bounds.is_some(), "custom-painted row has AT bounds");
            assert!(row.state.focusable);
            let target = row.action_target.clone().expect("stable entry target");
            assert!(Widget::perform_semantic_action(
                &mut timeline,
                &target,
                &Action::Focus,
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert_eq!(
                timeline.focused_entry().map(|entry| entry.id),
                Some(HistoryEntryId([
                    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
                ]))
            );
            assert!(Widget::perform_semantic_action(
                &mut timeline,
                &target,
                &Action::Activate,
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert_eq!(timeline.selection(), Some((0, 0)));
            assert_eq!(
                activated.borrow().as_slice(),
                &[HistoryEntryId([
                    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
                ])]
            );
        }

        fn keyboard_navigation_moves_focus_and_selects_individual_rows() {
            let entries = vec![
                entry(OpClass::Splice, "Alice", 10, 1),
                entry(OpClass::Splice, "Alice", 11, 2),
            ];
            let mut timeline = timeline_at(Rect::new(0.0, 0.0, 300.0, 400.0), entries);
            timeline.set_all_expanded(true);
            assert!(timeline.focus_row(0));
            dispatch(&mut timeline, &key_press(Key::Named(NamedKey::ArrowDown)));
            let focused = timeline.focused_row().expect("moved focus");
            assert!(focused > 0);
            if let Some(entry) = timeline.row_entry_index(focused) {
                assert_eq!(timeline.selection(), Some((entry, entry)));
            }
        }

        fn hover_row_tracks_cursor_y() {
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
            // Cache the y bound first so the borrow checker doesn't see a live
            // `&t` while we hand `&mut t` to `dispatch`.
            let far_y = t.rect.y1 + 1000.0;
            dispatch(&mut t, &cursor_moved(0.0, far_y));
            assert_eq!(t.hover_row(), None);
        }

        accessibility_reports_list_role_name_and_count();
        individual_entries_have_stable_accessible_targets_and_activate();
        keyboard_navigation_moves_focus_and_selects_individual_rows();
        hover_row_tracks_cursor_y();
    }

    /// Only the pub fields the widget reads are overwritten.
    fn key_press(logical_key: winit::keyboard::Key) -> WindowEvent {
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            logical_key,
            None,
            ElementState::Pressed,
        ))
    }

    fn dispatch(t: &mut ActivityTimeline, event: &WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        t.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: crate::widget::Edits::lend(&mut doc),
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &tree,
            },
        );
    }

    #[test]
    fn empty_and_filtered_timelines_explain_themselves() {
        fn empty_timeline_paints_placeholder() {
            // Pin the structural contract, not the literal string (i18n).
            let t = timeline_at(Rect::new(0.0, 0.0, 400.0, 300.0), vec![]);
            assert_eq!(t.visible_len(), 0);
            let strip = t.filter_strip_rect();
            assert!(strip.y0 >= t.rect.y0);
            assert!(strip.y1 < t.rect.y1);
            assert!(t.row_rect(0).is_none());
        }

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
            assert!(
                detail.contains("2 hidden"),
                "counts what it is hiding: {detail}"
            );
            assert!(detail.contains("Comments"), "names the filter: {detail}");
        }

        fn a_host_time_range_is_named_in_the_explanation() {
            // The widget cannot put a `SystemTime` pair into the user's words,
            // so the host labels it.
            let mut t = timeline_at(
                Rect::new(0.0, 0.0, 400.0, 300.0),
                vec![entry(OpClass::Splice, "Alice", 1, 1)],
            );
            t.set_filter(HistoryFilter {
                time_range: Some((
                    SystemTime::UNIX_EPOCH
                        .checked_add(Duration::from_secs(500))
                        .unwrap_or(SystemTime::UNIX_EPOCH),
                    SystemTime::UNIX_EPOCH
                        .checked_add(Duration::from_secs(600))
                        .unwrap_or(SystemTime::UNIX_EPOCH),
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

        empty_timeline_paints_placeholder();
        an_empty_history_and_a_filtered_one_explain_themselves_differently();
        a_host_time_range_is_named_in_the_explanation();
    }

    #[test]
    fn filters_narrow_by_op_class_and_author_and_clear() {
        fn filter_by_op_class_hides_other_rows() {
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

        fn chip_strip_active_state_round_trips() {
            let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 200.0), vec![]);
            assert_eq!(t.active_chip(), ChipKind::All);
            t.set_active_chip(ChipKind::Governance);
            assert_eq!(t.active_chip(), ChipKind::Governance);
            let entries = vec![
                entry(OpClass::Comment, "Alice", 1, 1),
                entry(OpClass::Govern, "Bob", 2, 2),
            ];
            t.set_entries(entries);
            assert_eq!(t.visible_len(), 1, "Governance chip narrows to govern row");
            t.set_active_chip(ChipKind::All);
            assert_eq!(t.visible_len(), 2);
        }

        fn chip_strip_click_toggles_active_chip() {
            // No real paint pass, so seed chip_rects directly: five 60x24 chips
            // packed horizontally, matching what draw_filter_strip would produce.
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

            // ChipKind::ALL order: All, Edits, Comments, ... — click "Comments"
            // (index 2, x range 132..192).
            dispatch(&mut t, &cursor_moved(162.0, cy));
            dispatch(&mut t, &mouse_press());
            assert_eq!(
                t.active_chip(),
                ChipKind::Comments,
                "clicking the Comments chip should activate it",
            );
            assert_eq!(t.visible_len(), 1, "narrowed to the one Comment row");

            // "All" chip — index 0, x range 0..60.
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

        fn filter_by_author_typeahead_narrows_list() {
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

        fn typing_narrows_by_author_and_escape_or_backspace_widens_again() {
            let entries = vec![
                entry(OpClass::Splice, "Alice", 1, 1),
                entry(OpClass::Splice, "Bob", 2, 2),
            ];
            let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 300.0), entries);
            dispatch(
                &mut t,
                &key_press(winit::keyboard::Key::Character("b".into())),
            );
            assert_eq!(t.author_query(), "b");
            assert_eq!(t.visible_len(), 1);
            assert_eq!(t.visible_entry(0).unwrap().author.name, "Bob");
            dispatch(
                &mut t,
                &key_press(winit::keyboard::Key::Named(NamedKey::Escape)),
            );
            assert_eq!(t.author_query(), "");
            assert_eq!(t.visible_len(), 2);

            dispatch(
                &mut t,
                &key_press(winit::keyboard::Key::Character("b".into())),
            );
            dispatch(
                &mut t,
                &key_press(winit::keyboard::Key::Named(NamedKey::Backspace)),
            );
            assert_eq!(t.author_query(), "");
            assert_eq!(t.visible_len(), 2);

            // An input method's commit types too, and Backspace takes back a
            // whole accented letter, not half of it.
            dispatch(
                &mut t,
                &WindowEvent::Ime(winit::event::Ime::Commit("bo\u{301}".into())),
            );
            assert_eq!(t.author_query(), "bo\u{301}");
            dispatch(
                &mut t,
                &key_press(winit::keyboard::Key::Named(NamedKey::Backspace)),
            );
            assert_eq!(t.author_query(), "b");
        }

        filter_by_op_class_hides_other_rows();
        chip_strip_active_state_round_trips();
        chip_strip_click_toggles_active_chip();
        clear_filters_resets_the_widget_and_tells_the_host();
        filter_by_author_typeahead_narrows_list();
        typing_narrows_by_author_and_escape_or_backspace_widens_again();
    }

    #[test]
    fn revert_chip_fires_only_for_revertible_ops() {
        fn revert_callback_fires_on_chip_click() {
            let entries = vec![entry(OpClass::Splice, "Alice", 1, 42)];
            let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 200.0), entries);
            let captured: Rc<RefCell<Option<HistoryEntryId>>> = Rc::new(RefCell::new(None));
            let captured_clone = captured.clone();
            t.on_revert = Some(Box::new(move |id| {
                *captured_clone.borrow_mut() = Some(*id);
            }));

            let row = t.row_rect(0).expect("seeded row 0 must lay out");
            dispatch(&mut t, &cursor_moved(row.center().x, row.center().y));
            assert_eq!(t.hover_row(), Some(0));
            let chip = t.revert_chip_rect(0).expect("splice row must have a chip");
            dispatch(&mut t, &cursor_moved(chip.center().x, chip.center().y));
            dispatch(&mut t, &mouse_press());

            let mut id_bytes = [0u8; 16];
            id_bytes[0] = 42;
            assert_eq!(*captured.borrow(), Some(HistoryEntryId(id_bytes)));
        }

        fn non_revertible_op_classes_hide_chip() {
            let entries = vec![
                entry(OpClass::Comment, "Alice", 1, 1),
                entry(OpClass::Govern, "Bob", 2, 2),
                entry(OpClass::Splice, "Carol", 3, 3),
            ];
            let t = editable_timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);
            assert!(t.revert_chip_rect(0).is_none(), "Comment must hide chip");
            assert!(t.revert_chip_rect(1).is_none(), "Govern must hide chip");
            assert!(
                t.revert_chip_rect(2).is_some(),
                "Splice must expose chip for hit-test"
            );
        }

        revert_callback_fires_on_chip_click();
        non_revertible_op_classes_hide_chip();
    }

    #[test]
    fn rows_order_newest_first_and_virtualize_within_narrow_panes() {
        fn entries_sharing_a_timestamp_still_order_newest_first() {
            // Automerge stamps in whole seconds, so a burst ties; newest-first
            // must fall back to causal order, not input order.
            let entries: Vec<HistoryEntry> = (0u8..5)
                .map(|i| entry(OpClass::Splice, "Alice", 100, i))
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

        fn newest_first_reverses_chronological_order() {
            let entries = vec![
                entry(OpClass::Splice, "Alice", 100, 1),
                entry(OpClass::Splice, "Alice", 200, 2),
                entry(OpClass::Splice, "Alice", 300, 3),
            ];
            let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);
            t.set_newest_first(true);
            assert_eq!(
                t.visible_entry(0)
                    .unwrap()
                    .timestamp
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                300
            );
            assert_eq!(
                t.visible_entry(2)
                    .unwrap()
                    .timestamp
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                100
            );
        }

        fn only_the_visible_window_of_a_huge_list_is_touched_per_frame() {
            // Paint and hit-test must look at the ~15 rows on screen, not all
            // ten thousand, or a room-scale history drops frames.
            let entries: Vec<HistoryEntry> = (0u32..10_000)
                .map(|i| {
                    entry(
                        OpClass::Splice,
                        "Alice",
                        u64::from(i),
                        u8::try_from(i % 251).unwrap_or(u8::MAX),
                    )
                })
                .collect();
            let t = timeline_at(Rect::new(0.0, 0.0, 600.0, 600.0), entries);
            assert_eq!(t.visible_len(), 10_000, "every row passes the filter");

            let range = t.visible_row_range();
            let viewport_rows_bounded = (t.viewport_height() / ROW_HEIGHT)
                .ceil()
                .clamp(0.0, 1_000_000.0);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to 0..=1_000_000 above, so this cast cannot truncate or flip sign"
            )]
            let viewport_rows = viewport_rows_bounded as usize;
            assert!(
                range.len() <= viewport_rows.saturating_add(2),
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
                t.row_rect(range.end.saturating_add(4)).is_none(),
                "a row well past the window must not lay out"
            );
        }

        fn row_at_agrees_with_row_rect_at_the_boundaries() {
            // `row_at` binary-searches the cumulative row tops; must still agree
            // with `row_rect` one pixel either side of a seam between two rows
            // of *different* heights.
            let entries: Vec<HistoryEntry> = (0u8..40)
                .map(|i| entry(OpClass::Splice, "Alice", u64::from(i), i))
                .collect();
            let mut t = timeline_at(Rect::new(0.0, 0.0, 500.0, 500.0), entries);
            // Forty changes by one author in forty seconds is one band, and it
            // collapses. Open it so the list has both kinds of line in it.
            t.set_all_expanded(true);
            assert!(
                matches!(t.group_at_row(0), Some((_, true))),
                "line 0 is the opened band header"
            );
            assert!(t.row_entry(1).is_some(), "line 1 is a change inside it");
            assert!(
                (t.row_rect(0).unwrap().height() - t.row_rect(1).unwrap().height()).abs() > 1.0,
                "the two line kinds must be different heights for this test to mean anything"
            );
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

        fn narrow_panes_shrink_the_trailing_slots_rather_than_overlapping_them() {
            // On a narrow pane the revert chip must still sit inside the row and
            // leave the summary somewhere to go.
            let entries = vec![entry(OpClass::Splice, "Alice", 1, 1)];
            let wide = editable_timeline_at(Rect::new(0.0, 0.0, 700.0, 300.0), entries.clone());
            let narrow = editable_timeline_at(Rect::new(0.0, 0.0, 300.0, 300.0), entries);

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
            assert!(
                narrow_chip.x1 <= narrow_row.x1,
                "and must stay inside the row"
            );
        }

        entries_sharing_a_timestamp_still_order_newest_first();
        newest_first_reverses_chronological_order();
        only_the_visible_window_of_a_huge_list_is_touched_per_frame();
        row_at_agrees_with_row_rect_at_the_boundaries();
        narrow_panes_shrink_the_trailing_slots_rather_than_overlapping_them();
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;
    use crate::time::Duration;
    use crate::widget::history::HistoryAuthor;
    use crate::widget::history_group::NamedRange;
    use automerge::AutoCommit;
    use collab_types::ArtifactKind;
    use std::cell::RefCell;
    use std::rc::Rc;
    use winit::event::Modifiers;

    const NOW: u64 = 2_000_000;

    fn entry(op: OpClass, author: &str, ts: u64, id: u8) -> HistoryEntry {
        let mut id_bytes = [0u8; 16];
        id_bytes[0] = id;
        HistoryEntry {
            id: HistoryEntryId(id_bytes),
            author: HistoryAuthor::new(author, 120.0, "deadbeef"),
            op_class: op,
            artifact_kind: ArtifactKind::Note,
            timestamp: SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(ts))
                .unwrap_or(SystemTime::UNIX_EPOCH),
            summary: format!("+{} -0 chars", usize::from(id).saturating_add(1)),
            text_delta: Some((usize::from(id).saturating_add(1), 0)),
            diff_payload: None,
            redaction: None,
            context: None,
        }
    }

    fn timeline_at(rect: Rect, entries: Vec<HistoryEntry>) -> ActivityTimeline {
        let mut t = ActivityTimeline::with_entries(entries);
        t.set_rect(rect);
        t.set_now_for_test(
            SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(NOW))
                .unwrap_or(SystemTime::UNIX_EPOCH),
        );
        t
    }

    // Capability-dependent layout fixtures must declare the operations they offer.
    fn editable_timeline_at(rect: Rect, entries: Vec<HistoryEntry>) -> ActivityTimeline {
        timeline_at(rect, entries)
            .on_revert(|_| {})
            .on_name_group(|_| {})
    }

    #[test]
    fn band_mutation_affordances_need_callbacks_and_restorable_changes() {
        fn readonly_timeline_hides_mutations_but_keeps_selectable_semantics() {
            let mut timeline = timeline_at(Rect::new(0.0, 0.0, 620.0, 620.0), typing_run(20));
            let (name, restore) = timeline.group_chip_rects(0).expect("group row");
            assert_eq!(name, Rect::ZERO);
            assert!(restore.is_none());
            assert!(!timeline.begin_naming_group_at_row(0));
            timeline.set_all_expanded(true);
            let row = (0..timeline.row_count())
                .find(|row| timeline.row_entry(*row).is_some())
                .expect("entry row");
            assert!(timeline.revert_chip_rect(row).is_none());
            assert!(timeline.activate_row(row));
            assert!(!timeline.begin_naming_selection());
            paint(&mut timeline);
            assert_eq!(timeline.name_selection_rect(), Rect::ZERO);
            assert!(
                timeline.clear_selection_rect.area() > 0.0,
                "selection can still be cleared"
            );
            let semantics = timeline.accessibility().expect("history semantics");
            assert!(semantics.children.iter().any(
                |node| node.role == crate::a11y::Role::ListItem && node.action_target.is_some()
            ));
            assert!(semantics.children.iter().all(|node| matches!(
                node.role,
                crate::a11y::Role::ListItem | crate::a11y::Role::TreeItem
            )));
        }

        fn history_mutation_affordances_require_their_own_callback() {
            let rect = Rect::new(0.0, 0.0, 620.0, 620.0);
            let mut naming = timeline_at(rect, typing_run(20)).on_name_group(|_| {});
            let (name, restore) = naming.group_chip_rects(0).expect("group row");
            assert!(name.area() > 0.0);
            assert!(restore.is_none());
            assert!(naming.begin_naming_group_at_row(0));
            let mut restoring = timeline_at(rect, typing_run(20)).on_revert(|_| {});
            let (name, restore) = restoring.group_chip_rects(0).expect("group row");
            assert_eq!(name, Rect::ZERO);
            assert!(restore.is_some());
            assert!(!restoring.begin_naming_group_at_row(0));
            restoring.set_all_expanded(true);
            let row = (0..restoring.row_count())
                .find(|row| restoring.row_entry(*row).is_some())
                .expect("entry row");
            assert!(restoring.revert_chip_rect(row).is_some());
        }

        fn a_band_of_nothing_restorable_offers_no_restore_chip() {
            // Governance is its own audit trail; an owner counters it rather than
            // rewinding it. A band made only of those must not offer the chip.
            let entries: Vec<HistoryEntry> = (0..6)
                .map(|i| {
                    entry(
                        OpClass::Govern,
                        "Alice",
                        1_000_000u64.saturating_add(u64::from(i)),
                        i,
                    )
                })
                .collect();
            let t = timeline_at(Rect::new(0.0, 0.0, 520.0, 500.0), entries);
            let (_, restore) = t.group_chip_rects(0).expect("still a band");
            assert!(restore.is_none(), "no restore across governance");
        }

        fn restoring_from_a_band_passes_the_bands_last_change_not_its_first() {
            // This is the whole point of grouping for restore: a boundary that means
            // something, rather than keystroke 4,182.
            let captured: Rc<RefCell<Option<HistoryEntryId>>> = Rc::new(RefCell::new(None));
            let c = captured.clone();
            let mut t = ActivityTimeline::with_entries(typing_run(20))
                .on_revert(move |id| *c.borrow_mut() = Some(*id));
            t.set_rect(Rect::new(0.0, 0.0, 520.0, 500.0));

            let (_, restore) = t.group_chip_rects(0).expect("a band header");
            let restore = restore.expect("a band of splices can be restored to");
            click(&mut t, restore.center());
            let mut want = [0u8; 16];
            want[0] = 19;
            assert_eq!(*captured.borrow(), Some(HistoryEntryId(want)));
        }

        readonly_timeline_hides_mutations_but_keeps_selectable_semantics();
        history_mutation_affordances_require_their_own_callback();
        a_band_of_nothing_restorable_offers_no_restore_chip();
        restoring_from_a_band_passes_the_bands_last_change_not_its_first();
    }

    /// Run a real paint so the rects the click handler reads from the last frame
    /// (the selection bar's actions) are populated exactly as they are live.
    fn paint(t: &mut ActivityTimeline) {
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        t.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    use crate::synthetic_input::cursor_moved;

    use crate::synthetic_input::mouse_press;

    fn key_press(logical_key: winit::keyboard::Key) -> WindowEvent {
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            logical_key,
            None,
            ElementState::Pressed,
        ))
    }

    fn dispatch(t: &mut ActivityTimeline, event: &WindowEvent) {
        dispatch_mod(t, event, Modifiers::default());
    }

    fn dispatch_mod(t: &mut ActivityTimeline, event: &WindowEvent, m: Modifiers) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        t.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &m,
                state: crate::widget::Edits::lend(&mut doc),
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &tree,
            },
        );
    }

    /// Shift held down, built the same way winit reports it.
    fn shift() -> Modifiers {
        let state = winit::keyboard::ModifiersState::SHIFT;
        Modifiers::from(state)
    }

    /// Click whatever is at `p`: move there first so `hover_row` latches, the
    /// way a real pointer does.
    fn click(t: &mut ActivityTimeline, p: Point) {
        dispatch(t, &cursor_moved(p.x, p.y));
        dispatch(t, &mouse_press());
    }

    fn type_str(t: &mut ActivityTimeline, s: &str) {
        for ch in s.chars() {
            if ch == ' ' {
                dispatch(t, &key_press(Key::Named(NamedKey::Space)));
            } else {
                dispatch(t, &key_press(Key::Character(ch.to_string().into())));
            }
        }
    }

    /// Twenty changes a second apart by one person: a debounced typing run.
    fn typing_run(n: u8) -> Vec<HistoryEntry> {
        (0..n)
            .map(|i| {
                entry(
                    OpClass::Splice,
                    "Alice",
                    1_000_000u64.saturating_add(u64::from(i)),
                    i,
                )
            })
            .collect()
    }

    #[test]
    fn typing_runs_collapse_into_expandable_bands() {
        fn a_typing_run_collapses_to_one_band_and_the_header_opens_it() {
            let mut t = timeline_at(Rect::new(0.0, 0.0, 520.0, 500.0), typing_run(20));
            assert_eq!(t.visible_len(), 20, "every change still passes the filter");
            assert_eq!(t.row_count(), 1, "twenty changes read as one band");
            let (_, expanded) = t.group_at_row(0).expect("line 0 is a band");
            assert!(!expanded, "a twenty-change band is past the expand budget");
            assert_eq!(t.group_title_at_row(0), Some("20 edits"));

            // Click the header (not the chips) — the production toggle path.
            let row = t.row_rect(0).expect("the header lays out");
            click(&mut t, Point::new(row.x0 + 60.0, row.center().y));
            assert!(t.group_at_row(0).unwrap().1, "the click opened it");
            assert_eq!(t.row_count(), 21, "header plus twenty changes");
            assert!(t.row_entry(1).is_some(), "line 1 is a change");
            assert_eq!(
                t.row_depth_at(1),
                Some(1),
                "and it is indented under the band"
            );

            click(&mut t, Point::new(row.x0 + 60.0, row.center().y));
            assert_eq!(t.row_count(), 1, "clicking again closes it");
        }

        fn a_short_history_reads_as_a_flat_list_with_no_bands_at_all() {
            // The flat list is the degenerate case, not a second mode: four changes
            // by four people are four groups of one, and a group of one is a row.
            let entries = vec![
                entry(OpClass::Splice, "Alice", 1_000_000, 0),
                entry(OpClass::Splice, "Bob", 1_000_001, 1),
                entry(OpClass::Splice, "Carol", 1_000_002, 2),
                entry(OpClass::Splice, "Dave", 1_000_003, 3),
            ];
            let t = timeline_at(Rect::new(0.0, 0.0, 520.0, 500.0), entries);
            assert_eq!(t.row_count(), 4);
            for i in 0..4 {
                assert!(t.row_entry(i).is_some(), "line {i} is a plain change row");
                assert_eq!(t.row_depth_at(i), Some(0), "and it is not indented");
            }
        }

        fn an_open_band_says_it_is_still_open_while_it_is_being_appended_to() {
            let entries = typing_run(20);
            let mut t = timeline_at(Rect::new(0.0, 0.0, 620.0, 500.0), entries.clone());
            t.set_named_ranges(vec![NamedRange {
                id: "tilo:group:rec".into(),
                label: "Working".into(),
                start: entries[0].id,
                end: None,
                named_by: None,
                created: SystemTime::UNIX_EPOCH,
            }]);
            let sub = t.group_subtitle_at_row(0).unwrap();
            assert!(sub.starts_with("still open"), "got {sub:?}");
            assert!(sub.contains("20 changes"), "got {sub:?}");

            // A new change lands while the reader is looking at it. The band must
            // absorb it and the count must move.
            let mut grown = entries;
            grown.push(entry(OpClass::Splice, "Alice", 1_000_400, 99));
            t.set_entries(grown);
            let sub = t.group_subtitle_at_row(0).unwrap();
            assert!(sub.contains("21 changes"), "got {sub:?}");
            assert!(
                sub.starts_with("still open"),
                "and it is still open: {sub:?}"
            );
        }

        fn expansion_survives_a_refresh_that_appends_a_change() {
            // The band key is derived from its first change, which does not move as
            // the band grows — so an open band stays open when the document does.
            let mut t = timeline_at(Rect::new(0.0, 0.0, 520.0, 500.0), typing_run(20));
            let row = t.row_rect(0).unwrap();
            click(&mut t, Point::new(row.x0 + 60.0, row.center().y));
            assert!(t.group_at_row(0).unwrap().1);
            let key = t.group_at_row(0).unwrap().0.to_string();

            let mut grown = typing_run(20);
            grown.push(entry(OpClass::Splice, "Alice", 1_000_020, 99));
            t.set_entries(grown);
            assert_eq!(t.group_at_row(0).unwrap().0, key, "same band, same key");
            assert!(
                t.group_at_row(0).unwrap().1,
                "it did not slam shut under the reader"
            );
        }

        fn nesting_renders_the_child_inside_the_parent_and_no_deeper() {
            let entries = typing_run(20);
            let mut t = timeline_at(Rect::new(0.0, 0.0, 620.0, 900.0), entries.clone());
            t.set_named_ranges(vec![
                NamedRange {
                    id: "tilo:group:outer".into(),
                    label: "Q3 release".into(),
                    start: entries[0].id,
                    end: Some(entries[19].id),
                    named_by: None,
                    created: SystemTime::UNIX_EPOCH,
                },
                NamedRange {
                    id: "tilo:group:inner".into(),
                    label: "Intro rewrite".into(),
                    start: entries[4].id,
                    end: Some(entries[8].id),
                    named_by: None,
                    created: SystemTime::UNIX_EPOCH,
                },
            ]);
            t.set_all_expanded(true);
            let depths: Vec<usize> = (0..t.row_count())
                .map(|i| t.row_depth_at(i).unwrap())
                .collect();
            assert_eq!(
                *depths.iter().max().unwrap(),
                2,
                "rows inside a nested band"
            );
            assert_eq!(t.group_title_at_row(0), Some("Q3 release"));
            let inner = (0..t.row_count())
                .find(|i| t.group_title_at_row(*i) == Some("Intro rewrite"))
                .expect("the child band renders");
            assert_eq!(t.row_depth_at(inner), Some(1));
            // Every line between the child header and the next depth<=1 line is a
            // change of the child's.
            let after = inner + 1;
            assert_eq!(t.row_depth_at(after), Some(2));
            assert!(t.row_entry(after).is_some());
        }

        a_typing_run_collapses_to_one_band_and_the_header_opens_it();
        a_short_history_reads_as_a_flat_list_with_no_bands_at_all();
        an_open_band_says_it_is_still_open_while_it_is_being_appended_to();
        expansion_survives_a_refresh_that_appends_a_change();
        nesting_renders_the_child_inside_the_parent_and_no_deeper();
    }

    #[test]
    fn naming_a_band_writes_its_whole_range_or_nothing() {
        fn naming_a_band_asks_the_host_to_write_the_bands_whole_range() {
            let captured: Rc<RefCell<Vec<GroupNameRequest>>> = Rc::new(RefCell::new(Vec::new()));
            let c = captured.clone();
            let mut t = ActivityTimeline::with_entries(typing_run(20))
                .on_name_group(move |r| c.borrow_mut().push(r.clone()));
            t.set_rect(Rect::new(0.0, 0.0, 520.0, 500.0));
            t.set_now_for_test(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW));

            let (name_chip, _) = t
                .group_chip_rects(0)
                .expect("a band header has a name chip");
            click(&mut t, name_chip.center());
            assert_eq!(t.naming_buffer(), Some(""), "the field is up and empty");
            assert!(t.wants_keyboard(), "the field owns the keyboard");

            type_str(&mut t, "Intro rewrite");
            assert_eq!(t.naming_buffer(), Some("Intro rewrite"));
            dispatch(&mut t, &key_press(Key::Named(NamedKey::Enter)));

            let reqs = captured.borrow();
            assert_eq!(reqs.len(), 1, "one write, on commit");
            let r = &reqs[0];
            assert_eq!(r.label, "Intro rewrite");
            assert_eq!(r.record_id, None, "nothing was named here before");
            let mut first = [0u8; 16];
            let mut last = [0u8; 16];
            last[0] = 19;
            assert_eq!(
                r.start,
                HistoryEntryId(first),
                "the range starts at change 0"
            );
            assert_eq!(r.end, HistoryEntryId(last), "and ends at change 19");
            let _ = &mut first;
            assert!(!t.wants_keyboard(), "the field is gone");
        }

        fn escape_abandons_a_name_without_writing_anything() {
            let captured: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
            let c = captured.clone();
            let mut t = ActivityTimeline::with_entries(typing_run(20))
                .on_name_group(move |_| *c.borrow_mut() += 1);
            t.set_rect(Rect::new(0.0, 0.0, 520.0, 500.0));

            let (name_chip, _) = t.group_chip_rects(0).unwrap();
            click(&mut t, name_chip.center());
            type_str(&mut t, "oops");
            dispatch(&mut t, &key_press(Key::Named(NamedKey::Escape)));
            assert!(!t.wants_keyboard());
            assert_eq!(*captured.borrow(), 0, "nothing was written");
        }

        fn typing_a_name_does_not_leak_into_the_author_filter() {
            // The pane's other keyboard consumer is the author typeahead. A version
            // called "Bob's edits" must not silently filter the pane to Bob.
            let mut t = editable_timeline_at(Rect::new(0.0, 0.0, 520.0, 500.0), typing_run(20));
            let (name_chip, _) = t.group_chip_rects(0).unwrap();
            click(&mut t, name_chip.center());
            type_str(&mut t, "Bob");
            assert_eq!(t.author_query(), "", "the typeahead was not touched");
            assert_eq!(t.naming_buffer(), Some("Bob"));
        }

        fn a_named_band_renames_through_the_record_it_already_has() {
            let captured: Rc<RefCell<Vec<GroupNameRequest>>> = Rc::new(RefCell::new(Vec::new()));
            let c = captured.clone();
            let entries = typing_run(20);
            let mut t = ActivityTimeline::with_entries(entries.clone())
                .on_name_group(move |r| c.borrow_mut().push(r.clone()));
            t.set_rect(Rect::new(0.0, 0.0, 520.0, 500.0));
            t.set_named_ranges(vec![NamedRange {
                id: "tilo:group:rec".into(),
                label: "Intro rewrite".into(),
                start: entries[0].id,
                end: Some(entries[19].id),
                named_by: Some(HistoryAuthor::new("Alice", 120.0, "deadbeef")),
                created: SystemTime::UNIX_EPOCH,
            }]);
            assert_eq!(t.group_title_at_row(0), Some("Intro rewrite"));

            let (name_chip, _) = t.group_chip_rects(0).unwrap();
            click(&mut t, name_chip.center());
            assert_eq!(
                t.naming_buffer(),
                Some("Intro rewrite"),
                "renaming starts from the current name, not from blank"
            );
            // Clear it out entirely and commit: that is how a name comes back off.
            for _ in 0.."Intro rewrite".len() {
                dispatch(&mut t, &key_press(Key::Named(NamedKey::Backspace)));
            }
            dispatch(&mut t, &key_press(Key::Named(NamedKey::Enter)));
            let reqs = captured.borrow();
            assert_eq!(reqs.len(), 1);
            assert_eq!(reqs[0].label, "", "an empty label is 'remove the name'");
            assert_eq!(
                reqs[0].record_id.as_deref(),
                Some("tilo:group:rec"),
                "and it addresses the record that already exists"
            );
        }

        fn a_band_someone_else_named_says_who_named_it() {
            let entries = typing_run(20);
            let mut t = timeline_at(Rect::new(0.0, 0.0, 620.0, 500.0), entries.clone());
            t.set_named_ranges(vec![NamedRange {
                id: "tilo:group:rec".into(),
                label: "Ada's pass".into(),
                start: entries[0].id,
                end: Some(entries[19].id),
                named_by: Some(HistoryAuthor::new("Ada Lovelace", 300.0, "cafebabe")),
                created: SystemTime::UNIX_EPOCH,
            }]);
            let sub = t
                .group_subtitle_at_row(0)
                .expect("a band has a second line");
            assert!(
                sub.contains("named by Ada Lovelace"),
                "a name written by someone else must say so: {sub:?}"
            );
            assert!(
                sub.contains("Alice"),
                "and still attribute the changes to whoever made them: {sub:?}"
            );
        }

        naming_a_band_asks_the_host_to_write_the_bands_whole_range();
        escape_abandons_a_name_without_writing_anything();
        typing_a_name_does_not_leak_into_the_author_filter();
        a_named_band_renames_through_the_record_it_already_has();
        a_band_someone_else_named_says_who_named_it();
    }

    #[test]
    fn band_undo_chip_targets_the_whole_band_only_when_supported() {
        fn a_band_offers_no_undo_chip_until_a_host_can_perform_one() {
            // Opt-in: a control that silently does nothing is worse than none.
            let entries = typing_run(20);
            let t = timeline_at(Rect::new(0.0, 0.0, 620.0, 500.0), entries.clone());
            assert!(
                t.group_undo_chip_rect(0).is_none(),
                "no band-undo callback, no chip"
            );

            let t = timeline_at(Rect::new(0.0, 0.0, 620.0, 500.0), entries).on_undo_band(|_| {});
            assert!(
                t.group_undo_chip_rect(0).is_some(),
                "with one installed, the band offers it"
            );
            assert!(
                t.group_undo_chip_rect(1).is_none(),
                "and only band headers get it, never a plain change row"
            );
        }

        fn the_undo_chip_asks_for_the_whole_band_not_one_change() {
            // Must carry a range, not a single id — that's what distinguishes
            // undo from restore.
            let entries = typing_run(20);
            let captured: Rc<RefCell<Vec<BandUndoRequest>>> = Rc::new(RefCell::new(Vec::new()));
            let c = captured.clone();
            let mut t = timeline_at(Rect::new(0.0, 0.0, 620.0, 500.0), entries.clone())
                .on_undo_band(move |r| c.borrow_mut().push(r.clone()));
            t.set_named_ranges(vec![NamedRange {
                id: "tilo:group:rec".into(),
                label: "The agent's pass".into(),
                start: entries[0].id,
                end: Some(entries[19].id),
                named_by: Some(HistoryAuthor::new("Alice", 120.0, "deadbeef")),
                created: SystemTime::UNIX_EPOCH,
            }]);

            let chip = t.group_undo_chip_rect(0).expect("a named band offers undo");
            click(&mut t, chip.center());

            let reqs = captured.borrow();
            assert_eq!(reqs.len(), 1, "the chip fired once");
            assert_eq!(reqs[0].start, entries[0].id, "the band's first change");
            assert_eq!(reqs[0].end, entries[19].id, "and its last");
            assert_ne!(
                reqs[0].start, reqs[0].end,
                "a range, not the single id a restore would carry"
            );
            assert_eq!(reqs[0].label.as_deref(), Some("The agent's pass"));
            assert_eq!(
                reqs[0]
                    .authors
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["Alice"],
                "and who authored it, so the host can scope the revert"
            );
        }

        fn clicking_undo_never_falls_through_to_restore() {
            // Undo takes the band out; restore discards everything after it. A
            // click on one must not reach the other.
            let entries = typing_run(20);
            let undone: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
            let restored: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
            let (u, r) = (undone.clone(), restored.clone());
            let mut t = timeline_at(Rect::new(0.0, 0.0, 620.0, 500.0), entries)
                .on_undo_band(move |_| *u.borrow_mut() += 1)
                .on_revert(move |_| *r.borrow_mut() += 1);

            let undo_chip = t.group_undo_chip_rect(0).expect("undo chip");
            let (_, restore) = t.group_chip_rects(0).expect("a band header");
            let restore = restore.expect("restore chip");
            assert!(
                undo_chip.x1 <= restore.x0,
                "undo sits left of restore and they do not overlap: {undo_chip:?} {restore:?}"
            );

            click(&mut t, undo_chip.center());
            assert_eq!((*undone.borrow(), *restored.borrow()), (1, 0));

            click(&mut t, restore.center());
            assert_eq!((*undone.borrow(), *restored.borrow()), (1, 1));
        }

        a_band_offers_no_undo_chip_until_a_host_can_perform_one();
        the_undo_chip_asks_for_the_whole_band_not_one_change();
        clicking_undo_never_falls_through_to_restore();
    }

    #[test]
    fn band_rows_keep_chips_names_and_text_inside_the_row() {
        fn three_band_chips_still_fit_inside_the_row() {
            for w in [360.0_f64, 620.0, 980.0] {
                let entries = typing_run(20);
                let t = editable_timeline_at(Rect::new(0.0, 0.0, w, 500.0), entries)
                    .on_undo_band(|_| {});
                let row = t.row_rect(0).expect("a band header");
                let (name, restore) = t.group_chip_rects(0).unwrap();
                let undo = t.group_undo_chip_rect(0).unwrap();
                let restore = restore.unwrap();
                assert!(restore.x1 <= row.x1, "restore inside the row at {w}");
                assert!(undo.x1 <= restore.x0, "undo left of restore at {w}");
                assert!(name.x1 <= undo.x0, "name left of undo at {w}");
                assert!(
                    name.x0 >= row.x0,
                    "and the strip does not run off the left at {w}"
                );
            }
        }

        fn a_very_long_name_is_truncated_rather_than_pushed_over_the_actions() {
            let entries = typing_run(20);
            let long = "Rewrote the introduction, then the second section, then \
                    reconsidered the whole framing and put most of it back"
                .to_string();
            let mut t = editable_timeline_at(Rect::new(0.0, 0.0, 520.0, 500.0), entries.clone());
            t.set_named_ranges(vec![NamedRange {
                id: "tilo:group:rec".into(),
                label: long.clone(),
                start: entries[0].id,
                end: Some(entries[19].id),
                named_by: None,
                created: SystemTime::UNIX_EPOCH,
            }]);
            assert_eq!(t.group_title_at_row(0), Some(long.as_str()));

            let row = t.row_rect(0).unwrap();
            let (name, restore) = t.group_chip_rects(0).unwrap();
            let restore = restore.unwrap();
            assert!(name.x0 > row.x0, "the name chip stays inside the row");
            assert!(restore.x1 <= row.x1, "so does the restore chip");
            assert!(name.x1 <= restore.x0, "and they do not overlap");
            // Painting must not panic and must fit the title into what is left.
            paint(&mut t);
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            let budget = name.x0 - (row.x0 + 26.0) - 8.0;
            let shown = fit_text(&mut tr, &long, &theme.text(TextRole::Label), budget);
            assert!(
                shown.chars().count() < long.chars().count(),
                "the header's own budget must actually truncate this name"
            );
            assert!(
                shown.ends_with('\u{2026}'),
                "and say that it did: {shown:?}"
            );
        }

        fn a_narrow_pane_keeps_the_band_actions_inside_the_row() {
            let mut t = editable_timeline_at(Rect::new(0.0, 0.0, 300.0, 500.0), typing_run(20));
            let row = t.row_rect(0).unwrap();
            let (name, restore) = t.group_chip_rects(0).unwrap();
            let restore = restore.unwrap();
            assert!(name.x0 >= row.x0, "the name chip starts inside the row");
            assert!(restore.x1 <= row.x1);
            assert!(name.x1 <= restore.x0);
            paint(&mut t);

            // Narrow, but tall enough to scroll the child band into view — an
            // off-screen line correctly does not lay out at all.
            let mut t = editable_timeline_at(Rect::new(0.0, 0.0, 300.0, 1200.0), typing_run(20));
            let entries = typing_run(20);
            t.set_named_ranges(vec![
                NamedRange {
                    id: "tilo:group:outer".into(),
                    label: "Outer".into(),
                    start: entries[0].id,
                    end: Some(entries[19].id),
                    named_by: None,
                    created: SystemTime::UNIX_EPOCH,
                },
                NamedRange {
                    id: "tilo:group:inner".into(),
                    label: "Inner".into(),
                    start: entries[5].id,
                    end: Some(entries[9].id),
                    named_by: None,
                    created: SystemTime::UNIX_EPOCH,
                },
            ]);
            t.set_all_expanded(true);
            let nested = (0..t.row_count())
                .find(|i| t.group_title_at_row(*i) == Some("Inner"))
                .expect("the nested band renders");
            assert_eq!(t.row_depth_at(nested), Some(1));
            let (n2, _) = t.group_chip_rects(nested).unwrap();
            let row2 = t.row_rect(nested).unwrap();
            assert!(
                n2.x0 > row2.x0 + INDENT,
                "indented, and the chip still fits"
            );
            paint(&mut t);
        }

        /// Row text is centred on the renderer's measured line box, which is
        /// what keeps it on the avatar's centre line when type is scaled.
        /// Every chip the timeline offers the pointer — the filter strip's
        /// class chips, the author pill, a band header's Name/Restore — is at
        /// least the minimum pointer target tall and no two claim the same
        /// pixels, while the strip's chips stay inside their strip. The paint
        /// is still the 28 / 20 px pill; only the target grew.
        fn chip_targets_meet_the_minimum_pointer_target_and_never_overlap() {
            let mut t = ActivityTimeline::with_entries(typing_run(20))
                .on_revert(|_| {})
                .on_name_group(|_| {});
            t.set_rect(Rect::new(0.0, 0.0, 520.0, 500.0));
            t.set_now_for_test(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW));
            let theme = Theme::dark();
            let min = f64::from(theme.tokens.metrics.min_pointer_target);
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            t.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::default(),
            );

            let strip = t.filter_strip_rect();
            let mut targets: Vec<(String, Rect)> = t
                .chip_rects
                .iter()
                .map(|(k, r)| (format!("{k:?}"), *r))
                .collect();
            assert!(targets.len() >= 2, "the strip painted no chips");
            for (k, r) in &targets {
                assert!(
                    r.y0 >= strip.y0 - 1e-6 && r.y1 <= strip.y1 + 1e-6,
                    "{k} target {r:?} leaves the strip {strip:?}"
                );
            }
            targets.push(("author".to_string(), t.author_pill_rect()));
            let (name, restore) = t.group_chip_rects(0).expect("a band header has chips");
            targets.push(("name".to_string(), name));
            if let Some(r) = restore {
                targets.push(("restore".to_string(), r));
            }
            for (k, r) in &targets {
                assert!(
                    r.height() >= min - 1e-6,
                    "{k} target {r:?} is under the {min}px minimum pointer target"
                );
            }
            for (i, (a_name, a)) in targets.iter().enumerate() {
                for (b_name, b) in targets.iter().skip(i + 1) {
                    assert!(
                        a.intersect(*b).is_zero_area(),
                        "{a_name} {a:?} and {b_name} {b:?} claim the same pixels"
                    );
                }
            }
        }

        fn row_text_centring_tracks_the_measured_line_box_at_every_text_scale() {
            let theme = Theme::dark();
            let style = theme.text(TextRole::Caption);
            for scale in [1.0_f32, 2.0] {
                let mut tr = TextRenderer::new();
                tr.set_text_scale(scale);
                let cy = 40.0;
                let top = text_top(&mut tr, &style, cy);
                let (_, h) = tr.measure_style("Alice", None, &style);
                assert!(
                    (top + f64::from(h) / 2.0 - cy).abs() < 1e-6,
                    "at {scale}x: top {top} + half of {h} misses the row centre {cy}"
                );
                assert!(
                    f64::from(h) > f64::from(style.size),
                    "the line box {h} is taller than the bare size {}; centring on \
                 size/2 would sit the text low",
                    style.size
                );
            }
        }

        /// The gutters are constants because hit-testing has no theme; they must
        /// nevertheless be the tokens' values, so a token change surfaces here.
        fn gutter_constants_equal_the_spacing_tokens() {
            let spacing = crate::tokens::Spacing::default();
            assert!(
                (PADDING - f64::from(spacing.s2)).abs() < 1e-9,
                "PADDING is s2"
            );
            assert!(
                (TRAILING_INSET - f64::from(spacing.s3)).abs() < 1e-9,
                "TRAILING_INSET is s3"
            );
        }

        three_band_chips_still_fit_inside_the_row();
        a_very_long_name_is_truncated_rather_than_pushed_over_the_actions();
        a_narrow_pane_keeps_the_band_actions_inside_the_row();
        chip_targets_meet_the_minimum_pointer_target_and_never_overlap();
        row_text_centring_tracks_the_measured_line_box_at_every_text_scale();
        gutter_constants_equal_the_spacing_tokens();
    }

    #[test]
    fn band_selection_tracks_ids_and_filters() {
        fn shift_clicking_two_rows_selects_the_run_between_them_and_names_it() {
            let captured: Rc<RefCell<Vec<GroupNameRequest>>> = Rc::new(RefCell::new(Vec::new()));
            let c = captured.clone();
            let mut t = ActivityTimeline::with_entries(typing_run(20))
                .on_name_group(move |r| c.borrow_mut().push(r.clone()));
            t.set_rect(Rect::new(0.0, 0.0, 620.0, 620.0));
            t.set_now_for_test(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW));
            t.set_all_expanded(true);

            // Newest-first, so line 1 is change 19 and line 4 is change 16.
            let a = t.row_entry_index(1).expect("line 1 is a change");
            let b = t.row_entry_index(4).expect("line 4 is a change");
            assert_eq!((a, b), (19, 16));

            let r1 = t.row_rect(1).unwrap();
            click(&mut t, Point::new(r1.x0 + 40.0, r1.center().y));
            assert_eq!(t.selection(), Some((19, 19)), "a plain click anchors");

            let r4 = t.row_rect(4).unwrap();
            dispatch(&mut t, &cursor_moved(r4.x0 + 40.0, r4.center().y));
            dispatch_mod(&mut t, &mouse_press(), shift());
            assert_eq!(t.selection(), Some((16, 19)), "shift-click extends the run");

            // Paint so the bar's action rect is where the click handler will look.
            paint(&mut t);
            let action = t.name_selection_rect();
            assert!(
                action.area() > 0.0,
                "the selection bar offers a name action"
            );
            click(&mut t, action.center());
            type_str(&mut t, "Second pass");
            dispatch(&mut t, &key_press(Key::Named(NamedKey::Enter)));

            let reqs = captured.borrow();
            assert_eq!(reqs.len(), 1);
            let mut start = [0u8; 16];
            start[0] = 16;
            let mut end = [0u8; 16];
            end[0] = 19;
            assert_eq!(reqs[0].start, HistoryEntryId(start));
            assert_eq!(reqs[0].end, HistoryEntryId(end));
            assert_eq!(reqs[0].label, "Second pass");
            assert_eq!(t.selection(), None, "committing clears the selection");
        }

        fn selection_tracks_stable_ids_across_feed_insertion() {
            let mut timeline = timeline_at(
                Rect::new(0.0, 0.0, 620.0, 600.0),
                vec![
                    entry(OpClass::Splice, "Ada", 1, 1),
                    entry(OpClass::Splice, "Ada", 3, 3),
                ],
            );
            let selected_id = timeline.entries()[1].id;
            timeline.set_selection(Some((1, 1)));
            timeline.set_entries(vec![
                entry(OpClass::Splice, "Ada", 1, 1),
                entry(OpClass::Splice, "Ada", 2, 2),
                entry(OpClass::Splice, "Ada", 3, 3),
            ]);
            assert_eq!(timeline.selection(), Some((2, 2)));
            assert_eq!(timeline.entries()[2].id, selected_id);
        }

        fn filtering_out_a_selection_clears_destructive_targeting() {
            let mut timeline = timeline_at(
                Rect::new(0.0, 0.0, 620.0, 600.0),
                vec![
                    entry(OpClass::Splice, "Ada", 1, 1),
                    entry(OpClass::Splice, "Maren", 2, 2),
                ],
            );
            timeline.set_selection(Some((0, 0)));
            timeline.set_filter(HistoryFilter::default().with_author("Maren"));
            assert_eq!(timeline.selection(), None);
        }

        fn a_filter_that_hides_part_of_a_band_says_how_much_it_is_showing() {
            let mut entries = typing_run(10);
            entries[3].op_class = OpClass::Comment;
            entries[4].op_class = OpClass::Comment;
            // Comments split the band automatically, so name the whole run to force
            // one band that a filter can then cut into.
            let ids = (entries[0].id, entries[9].id);
            let mut t = timeline_at(Rect::new(0.0, 0.0, 620.0, 500.0), entries);
            t.set_named_ranges(vec![NamedRange {
                id: "tilo:group:rec".into(),
                label: "Morning".into(),
                start: ids.0,
                end: Some(ids.1),
                named_by: None,
                created: SystemTime::UNIX_EPOCH,
            }]);
            assert!(
                !t.group_subtitle_at_row(0).unwrap().contains(" of "),
                "unfiltered, there is nothing to qualify: {:?}",
                t.group_subtitle_at_row(0)
            );

            t.set_active_chip(ChipKind::Comments);
            let sub = t.group_subtitle_at_row(0).expect("the band survives");
            assert!(
                sub.starts_with("2 of 10 shown"),
                "a band cut by a filter must say so: {sub:?}"
            );
            assert_eq!(t.visible_len(), 2);
        }

        shift_clicking_two_rows_selects_the_run_between_them_and_names_it();
        selection_tracks_stable_ids_across_feed_insertion();
        filtering_out_a_selection_clears_destructive_targeting();
        a_filter_that_hides_part_of_a_band_says_how_much_it_is_showing();
    }

    #[test]
    fn bands_order_virtualize_and_describe_themselves() {
        fn a_thousand_bands_only_lay_out_the_ones_on_screen() {
            // A thousand distinct authors is a thousand bands. The paint window and
            // the hit-test must both stay proportional to the viewport.
            let entries: Vec<HistoryEntry> = (0u32..2000)
                .map(|i| {
                    entry(
                        OpClass::Splice,
                        &format!("Person {}", i / 2),
                        1_000_000u64.saturating_add(u64::from(i)),
                        u8::try_from(i % 251).unwrap_or(u8::MAX),
                    )
                })
                .collect();
            let t = timeline_at(Rect::new(0.0, 0.0, 620.0, 620.0), entries);
            assert_eq!(t.visible_len(), 2000);
            assert!(
                t.row_count() >= 1000,
                "two changes each by a thousand people is a thousand bands, got {}",
                t.row_count()
            );

            let range = t.visible_row_range();
            let max_lines_bounded = (620.0_f64 / ROW_HEIGHT).ceil().clamp(0.0, 1_000_000.0);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to 0..=1_000_000 above, so this cast cannot truncate or flip sign"
            )]
            let max_lines = (max_lines_bounded as usize).saturating_add(2);
            assert!(
                range.len() <= max_lines,
                "laid out {} lines for a viewport that holds at most {}",
                range.len(),
                max_lines
            );
            assert_eq!(range.start, 0, "unscrolled, the window starts at the top");
            for i in range.clone() {
                assert!(t.row_rect(i).is_some(), "line {i} is on screen");
            }
            assert!(
                t.row_rect(range.end + 8).is_none(),
                "a line well past the window must not lay out"
            );
        }

        fn the_selection_bar_takes_its_space_out_of_the_scrollable_list() {
            let mut t = timeline_at(Rect::new(0.0, 0.0, 620.0, 620.0), typing_run(20));
            t.set_all_expanded(true);
            let before = t.row_rect(1).unwrap();
            let reachable_before = (0..t.row_count())
                .filter(|i| t.row_rect(*i).is_some())
                .count();

            t.set_selection(Some((16, 19)));
            let bar = t.selection_bar_rect();
            assert!(bar.area() > 0.0, "a selection puts the bar on screen");

            // The rows above it do not move — the bar is not a layout reflow.
            assert_eq!(t.row_rect(1).unwrap(), before);
            // It takes its space out of the viewport, so strictly fewer lines fit.
            let reachable_after = (0..t.row_count())
                .filter(|i| t.row_rect(*i).is_some())
                .count();
            assert!(
                reachable_after < reachable_before,
                "{reachable_after} lines still fit where {reachable_before} did"
            );
            // Nothing inside the bar is a hit target for the list, so a click on
            // [Name these] can never also select a row underneath it.
            for x in [bar.x0 + 4.0, bar.center().x, bar.x1 - 4.0] {
                for y in [bar.y0 + 2.0, bar.center().y, bar.y1 - 2.0] {
                    assert_eq!(
                        t.row_at(Point::new(x, y)),
                        None,
                        "({x}, {y}) is inside the bar and must not hit a row"
                    );
                }
            }
            // And everything stays reachable: the scroll extent grew by exactly the
            // height the bar took.
            let with_bar = t.max_scroll();
            t.set_selection(None);
            let without_bar = t.max_scroll();
            assert!(
                (with_bar - without_bar - SELECTION_BAR_H).abs() < 0.01,
                "scroll extent moved by {} for a {SELECTION_BAR_H} px bar",
                with_bar - without_bar
            );
        }

        fn bands_are_ordered_by_their_clock_but_never_broken_apart() {
            // A peer's session merges in last while carrying a clock from days
            // ago; ordering rows by clock alone would interleave it with local
            // work, so bands sort by their newest change and stay contiguous.
            let day = 86_400u64;
            let mut entries: Vec<HistoryEntry> = Vec::new();
            // Local session, recent.
            for i in 0..4 {
                entries.push(entry(OpClass::Splice, "Alice", NOW - 60 + u64::from(i), i));
            }
            // Peer session, merged after but stamped days earlier.
            for i in 0..3 {
                entries.push(entry(
                    OpClass::Splice,
                    "Ada",
                    NOW - 5 * day + u64::from(i),
                    10 + i,
                ));
            }
            let t = timeline_at(Rect::new(0.0, 0.0, 620.0, 900.0), entries);
            let mut tl = t;
            tl.set_all_expanded(true);

            let authors: Vec<&str> = (0..tl.row_count())
                .filter_map(|i| tl.row_entry(i).map(|e| e.author.name.as_str()))
                .collect();
            // Newest-first: Alice's whole band, then Ada's whole band. No interleaving.
            assert_eq!(
                authors,
                vec!["Alice", "Alice", "Alice", "Alice", "Ada", "Ada", "Ada"],
                "the recent band comes first and neither band is broken apart"
            );
            // And the band headers agree with that order.
            let titles: Vec<&str> = (0..tl.row_count())
                .filter_map(|i| tl.group_title_at_row(i))
                .collect();
            assert_eq!(titles.len(), 2, "two bands: {titles:?}");

            // `visible` walks the same order, so a count and a position agree.
            let visible_authors: Vec<&str> = (0..tl.visible_len())
                .map(|i| tl.visible_entry(i).unwrap().author.name.as_str())
                .collect();
            assert_eq!(visible_authors, authors);

            tl.set_newest_first(false);
            let authors: Vec<&str> = (0..tl.row_count())
                .filter_map(|i| tl.row_entry(i).map(|e| e.author.name.as_str()))
                .collect();
            assert_eq!(
                authors,
                vec!["Ada", "Ada", "Ada", "Alice", "Alice", "Alice", "Alice"],
                "oldest-first flips the bands, and still does not break them"
            );
        }

        fn the_accessible_description_counts_bands_as_well_as_changes() {
            let t = timeline_at(Rect::new(0.0, 0.0, 520.0, 500.0), typing_run(20));
            let node = Widget::accessibility(&t).unwrap();
            assert_eq!(
                node.description.as_deref(),
                Some("20 entries in 1 groups"),
                "a screen reader must not be told '20 items' over one collapsed band"
            );
        }

        a_thousand_bands_only_lay_out_the_ones_on_screen();
        the_selection_bar_takes_its_space_out_of_the_scrollable_list();
        bands_are_ordered_by_their_clock_but_never_broken_apart();
        the_accessible_description_counts_bands_as_well_as_changes();
    }

    #[test]
    fn day_headings_bucket_changes_into_today_yesterday_this_week_and_earlier() {
        // 1970-01-01 was Thursday. Day 8 is Friday 1970-01-09; that week's
        // Monday is day 4. Today / yesterday / mid-week / previous week.
        let now = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(8 * 86_400 + 12 * 3_600))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let secs = |day: u64, hour: u64| day * 86_400 + hour * 3_600;
        let entries = vec![
            entry(OpClass::Splice, "Ada", secs(8, 10), 1),
            entry(OpClass::Splice, "Maren", secs(7, 16), 2),
            entry(OpClass::Splice, "Quinten", secs(5, 9), 3),
            entry(OpClass::Splice, "Xaver", secs(2, 11), 4),
        ];
        let mut t = ActivityTimeline::with_entries(entries);
        t.set_now_for_test(now);
        t.set_newest_first(true);
        t.set_all_expanded(true);
        t.set_show_day_headings(true);
        let headings: Vec<String> = (0..t.row_count())
            .filter_map(|i| t.group_title_at_row(i).map(str::to_string))
            .filter(|title| {
                matches!(
                    title.as_str(),
                    "Today" | "Yesterday" | "This week" | "Earlier"
                )
            })
            .collect();
        assert_eq!(
            headings,
            vec!["Today", "Yesterday", "This week", "Earlier"],
            "newest-first calendar headings, not work-band titles"
        );
        assert!(
            (0..t.row_count()).any(|i| t.row_entry(i).is_some_and(|e| e.author.name == "Ada")),
            "Today still lists today's change"
        );
    }
}
