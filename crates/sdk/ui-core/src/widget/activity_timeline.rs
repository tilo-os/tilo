//! `ActivityTimeline` — the history pane, grouped.

use crate::i18n::{widget_format, widget_text};
use crate::num::f64_of_usize;
use crate::time::SystemTime;
use crate::widget::ModelPath;
use std::collections::{HashMap, HashSet};
use tilo_sort::CompactSort as _;

use taffy::{NodeId, TaffyTree};
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
};
use crate::widget::history_face::{EntryFace, HistoryEntryPresenter};
use crate::widget::history_group::{
    GroupNameRequest, GroupingConfig, HistoryGroup, NamedRange, group_entries,
};
use crate::widget::name_field::{NameField, NameFieldOutcome, fit as fit_text};

use crate::tokens::TextRole;
use crate::widget::EntryKind;

pub const ROW_HEIGHT: f64 = 40.0;
pub const GROUP_ROW_HEIGHT: f64 = 52.0;
pub const FILTER_STRIP_HEIGHT: f64 = 36.0;
pub const SELECTION_BAR_H: f64 = 44.0;
const PADDING: f64 = 8.0;
const AVATAR_DIAM: f64 = 16.0;
const REVERT_CHIP_W: f64 = 64.0;
const TIME_SLOT_W: f64 = 80.0;
/// The author cluster of a row: the author's face and the gap after it.
/// A row is told apart by who made it at a glance, by face and colour; the
/// name is the face's tooltip and the row's accessible name, never a
/// column of names cut to fit.
const AUTHOR_SLOT_W: f64 = 8.0 + AVATAR_DIAM + 10.0;
const INDENT: f64 = 20.0;
/// Right-hand gutter shared by every trailing element — a row's relative
/// time and a band header's — so the time column lines up down the pane.
/// A constant alongside `PADDING` and `INDENT`, which the row geometry that
/// hit-testing reads is built from without a theme; pinned to `spacing.s3`
/// by `gutter_constants_equal_the_spacing_tokens`.
const TRAILING_INSET: f64 = 12.0;
const AUTO_EXPAND_BUDGET: usize = 14;
const DENSE_GROUP_COUNT: usize = 6;

/// The pane paints on `surfaces.raised` (its own card, or the host's), where `Colors::lift` has
/// nowhere left to go, so every tier inside it — hovered row, band backdrop, selected row — is
/// **one ordered ramp** of translucent steps against that fill, polarity chosen the way
/// `hairline_on` chooses it (HIG §9.3 caveat, §9.5). Ascending: hover and band rest, band hover,
/// selected.
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

fn wash(surface: Color, alpha: u8) -> Color {
    crate::tokens::colors::Colors::hairline_on(surface, alpha)
}

fn author_face(author: &HistoryAuthor, colour: Color) -> crate::widget::avatar::AvatarFace<'_> {
    let face = crate::widget::avatar::AvatarFace::new(&author.name, &author.identity_short)
        .presence(colour);
    if author.relation == crate::widget::history::AuthorRelation::Software {
        face.agent()
    } else {
        face
    }
}

/// Top edge that centres one line of `size`-px text on `center_y`. The line box the renderer paints
/// is `line_height × size`, a quarter taller than the font size at the default 1.25, and it carries
/// the accessibility text scale, so centring on the *measured* height (never `size / 2`) keeps a
/// row's text on its avatar dot's centre line at every type scale.
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
        widget_text(match self {
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
        widget_text(match self {
            ChipKind::All => "history-chip-all",
            ChipKind::Edits => "history-chip-edits-short",
            ChipKind::Comments => "history-chip-comments-short",
            ChipKind::Governance => "history-chip-access-short",
            ChipKind::Deletes => "history-chip-deletions-short",
        })
    }

    /// `All` is empty because "no op-class filter" is how [`HistoryFilter`]
    /// expresses "show everything".
    pub fn entry_kinds(&self) -> Vec<EntryKind> {
        match self {
            ChipKind::All => Vec::new(),
            // Tombstone/Restore live in "Deletes" instead.
            ChipKind::Edits => vec![EntryKind::Text, EntryKind::Move, EntryKind::Details],
            ChipKind::Comments => vec![EntryKind::Comment],
            ChipKind::Governance => vec![EntryKind::Access],
            ChipKind::Deletes => vec![EntryKind::Delete, EntryKind::Restore],
        }
    }
}

/// Below this width the trailing time + revert slots shrink so the summary
/// keeps a usable share of the row.
const NARROW_WIDTH: f64 = 340.0;

pub type RevertCallback = Box<dyn FnMut(&HistoryEntryId)>;

/// Fired when a change row is activated with Enter, Space, or assistive
/// technology. The timeline owns selection; the host can use this to open a
/// detail view, move a scrubber, or otherwise inspect the selected moment.
pub type EntryActivateCallback = Box<dyn FnMut(&HistoryEntryId)>;

pub type ClearFiltersCallback = Box<dyn FnMut()>;

pub type NameGroupCallback = Box<dyn FnMut(&GroupNameRequest)>;

/// What `[Undo]` on a band asks for: the **range**, not a single id like
/// `[Restore]` sends. Restore means "put the document back to this moment,
/// discarding everything after"; undo means "take this band out, leave the
/// rest alone" — a host cannot tell the two apart from one id.
#[derive(Clone, Debug, PartialEq)]
pub struct BandUndoRequest {
    pub start: HistoryEntryId,
    pub end: HistoryEntryId,
    pub label: Option<String>,
    /// First-seen order; what a host filters the revert by.
    pub authors: Vec<HistoryAuthor>,
}

pub type UndoBandCallback = Box<dyn FnMut(&BandUndoRequest)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VisualRow {
    Group(usize),
    Entry(usize, usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FocusedRow {
    Group(String),
    Entry(HistoryEntryId),
}

fn work_icon(kind: &str) -> Option<(crate::icons::Icon, &'static str)> {
    match kind {
        "task" | "issue" => Some((crate::icons::Icon::ListChecks, "history-work-task")),
        "agent-run" => Some((crate::icons::Icon::Bot, "history-work-agent-run")),
        "shortcut" => Some((crate::icons::Icon::Play, "history-work-shortcut")),
        _ => None,
    }
}

impl RowGroup {
    fn meta_facts(&self) -> Vec<crate::widget::primitives::Fact> {
        let scope = self.scope_label.as_ref().filter(|_| !self.expanded);
        let count = self.facts.len();
        scope
            .into_iter()
            .chain(self.facts.iter())
            .enumerate()
            .map(|(i, text)| {
                let priority = u8::try_from(count.saturating_add(1).saturating_sub(i)).unwrap_or(0);
                crate::widget::primitives::Fact::text(text.clone()).priority(priority)
            })
            .collect()
    }

    /// The second line as a screen reader says it: each fact, in order.
    fn spoken_facts(&self) -> String {
        self.work
            .map(|(_, kind)| crate::i18n::I18nManager::lookup_plain(kind))
            .into_iter()
            .chain(self.meta_facts().into_iter().map(|fact| fact.text))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Clone, Debug)]
struct RowGroup {
    key: String,
    title: String,
    /// The second line's facts (who, how much moved, how many shown),
    /// drawn apart, never joined into one string.
    facts: Vec<String>,
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
    is_section: bool,
    work: Option<(crate::icons::Icon, &'static str)>,
}

/// A band header's hover chips, in the order they are laid out.
struct GroupChips {
    name: Rect,
    /// `None` unless a host installed a band-undo callback.
    undo: Option<Rect>,
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

#[derive(Clone, Debug, PartialEq)]
enum NameTarget {
    Group(usize),
    Selection,
}

#[derive(Clone, Debug)]
struct NamingState {
    target: NameTarget,
    field: NameField,
}

#[derive(Clone, Debug, Default)]
struct NothingPlacement {
    title: String,
    title_at: Point,
    detail: Option<(String, Point)>,
    clear: Rect,
}

#[derive(Clone, Debug, Default)]
struct SelectionBarPlacement {
    bar: Rect,
    count: String,
    count_at: Point,
    clear: Rect,
    name: Rect,
    /// The name field; `Rect::ZERO` unless a name is being typed.
    field: Rect,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag tracks an independent interaction, rendering, or invalidation state"
)]
#[crate::component::not_a_component(
    part,
    "`ui.ActivityTimeline` (`DescribedActivityTimeline`) is its described form"
)]
pub struct ActivityTimeline {
    entries: Vec<HistoryEntry>,
    named: Vec<NamedRange>,
    descriptions: std::collections::HashMap<HistoryEntryId, String>,
    presenter: Option<std::rc::Rc<dyn HistoryEntryPresenter>>,
    faces: HashMap<HistoryEntryId, EntryFace>,
    grouping: GroupingConfig,
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
    expanded: HashSet<String>,
    collapsed: HashSet<String>,
    selection: Option<(usize, usize)>,
    selection_anchor: Option<usize>,
    focused_row: Option<FocusedRow>,
    has_focus: bool,
    naming: Option<NamingState>,
    active_chip: ChipKind,
    author_query: String,
    rect: Rect,
    scroll_y: f64,
    hover_row: Option<usize>,
    last_cursor: Point,
    on_revert: Option<RevertCallback>,
    on_activate_entry: Option<EntryActivateCallback>,
    on_open_entry: Option<EntryActivateCallback>,
    on_undo_band: Option<UndoBandCallback>,
    on_name_group: Option<NameGroupCallback>,
    /// "Now" for the relative-timestamp paint. Tests + headless callers pin
    /// this so a snapshot stays stable.
    now_override: Option<SystemTime>,
    newest_first: bool,
    /// The strip's chips, placed by the layout: what a press on one filters
    /// the pane by, and where it stands. Painting reads this and adds nothing
    /// to it, so a press before a paint lands on the chip it means.
    strip_chips: Vec<StripChip>,
    author_pill_painted: Rect,
    author_pill_rect: Rect,
    /// The strip filters by who made the changes ([`Self::set_filters_people`]):
    /// Everyone, then each person and agent in the history.
    filters_people: bool,
    /// `metrics.min_pointer_target`, taken from the frame's theme so the
    /// theme-less hit-test accessors can grow a chip's target to it.
    min_pointer_target: f64,
    face_theme: Theme,
    /// The host's Activity view may supply its own Everyone/person selector; suppressing
    /// this duplicate leaves the op-class strip intact.
    show_author_pill: bool,
    show_day_headings: bool,
    /// The empty state's lines and its way back out, as the layout placed
    /// them: `clear` is `Rect::ZERO` whenever that state is not on screen, so
    /// a stale click cannot trip `[Clear filters]`.
    nothing: NothingPlacement,
    selection_bar: SelectionBarPlacement,
    time_range_label: Option<String>,
    on_clear_filters: Option<ClearFiltersCallback>,
    draws_own_surface: bool,
    preview_selection: bool,
    fill: bool,
    dirty: bool,
    /// Whether what the layout places may have moved since it last did: a
    /// selection, a name being typed, a filter, a list that changed. Cleared
    /// by the layout that places the strip's and the bar's controls, so a
    /// frame read before it paints finds them where it would have drawn them.
    layout_dirty: bool,
}

impl Default for ActivityTimeline {
    fn default() -> Self {
        Self::new()
    }
}

impl ActivityTimeline {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            named: Vec::new(),
            descriptions: std::collections::HashMap::new(),
            presenter: None,
            faces: HashMap::new(),
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
            on_open_entry: None,
            on_undo_band: None,
            on_name_group: None,
            now_override: None,
            newest_first: true,
            strip_chips: Vec::new(),
            author_pill_painted: Rect::ZERO,
            author_pill_rect: Rect::ZERO,
            filters_people: false,
            min_pointer_target: f64::from(crate::tokens::Metrics::BASE.min_pointer_target),
            face_theme: Theme::default(),
            show_author_pill: true,
            show_day_headings: false,
            nothing: NothingPlacement::default(),
            selection_bar: SelectionBarPlacement::default(),
            time_range_label: None,
            on_clear_filters: None,
            draws_own_surface: true,
            preview_selection: false,
            fill: true,
            dirty: true,
            layout_dirty: true,
        }
    }

    #[must_use]
    pub fn with_preview_selection(mut self) -> Self {
        self.set_preview_selection(true);
        self
    }

    pub fn set_preview_selection(&mut self, preview: bool) {
        self.preview_selection = preview;
        if preview {
            self.naming = None;
        }
        self.invalidate();
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
        self.refresh_faces();
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

    pub fn set_descriptions(
        &mut self,
        descriptions: std::collections::HashMap<HistoryEntryId, String>,
    ) {
        if self.descriptions != descriptions {
            self.descriptions = descriptions;
            self.rebuild();
        }
    }

    /// What `entry` did, in words: what the app's face for it says, else its
    /// description when a member's device wrote one, otherwise its own
    /// summary.
    pub fn summary_of<'a>(&'a self, entry: &'a HistoryEntry) -> &'a str {
        if let Some(face) = self.faces.get(&entry.id) {
            return face.spoken.as_str();
        }
        self.descriptions
            .get(&entry.id)
            .map_or(entry.summary.as_str(), String::as_str)
    }

    /// Present the changes the way the app does: each change it gives a face
    /// draws that face, copies what it copies and, when the face opens it,
    /// is taken up by the app when chosen ([`Self::on_open_entry`]). `None`
    /// reads every change as its sentence.
    pub fn set_entry_presenter(
        &mut self,
        presenter: Option<std::rc::Rc<dyn HistoryEntryPresenter>>,
    ) {
        let same = match (&self.presenter, &presenter) {
            (None, None) => true,
            (Some(was), Some(now)) => std::rc::Rc::ptr_eq(was, now),
            _ => false,
        };
        if same {
            return;
        }
        self.presenter = presenter;
        self.refresh_faces();
        self.rebuild_rows();
    }

    pub fn face_of(&self, entry: &HistoryEntryId) -> Option<&EntryFace> {
        self.faces.get(entry)
    }

    fn refresh_faces(&mut self) {
        self.faces = match &self.presenter {
            Some(presenter) => self
                .entries
                .iter()
                .filter_map(|entry| presenter.face(entry).map(|face| (entry.id, face)))
                .collect(),
            None => HashMap::new(),
        };
    }

    fn row_face(&self, i: usize) -> Option<&EntryFace> {
        self.faces.get(&self.row_entry(i)?.id)
    }

    pub fn copy_row(&self, i: usize) -> bool {
        let Some(text) = self.row_face(i).and_then(|face| face.copy.as_deref()) else {
            return false;
        };
        crate::text_selection::clipboard_copy(text);
        true
    }

    pub fn set_named_ranges(&mut self, named: Vec<NamedRange>) {
        if self.named != named {
            self.named = named;
            self.regroup();
        }
    }

    pub fn named_ranges(&self) -> &[NamedRange] {
        &self.named
    }

    pub fn refresh(&mut self) {
        self.regroup();
    }

    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    pub fn groups(&self) -> &[HistoryGroup] {
        &self.groups
    }

    /// For a host that reserves a rect out of its own layout rather than
    /// putting the widget in a taffy tree: a host that places it by hand
    /// places it with [`Self::place_in`], which also places the strip's and
    /// the bar's controls the way the layout would.
    pub fn set_rect(&mut self, rect: Rect) {
        if self.rect != rect {
            self.rect = rect;
            self.invalidate();
        }
    }

    /// Take `rect` and place in it everything a layout pass would: the
    /// strip's chips, its author pill, the way back out of a filter and the
    /// selection bar's count and actions, measured in `theme`'s own type. A
    /// host that reserves the pane's box out of its own layout calls this
    /// before it draws, so a press or a screen reader finds those controls
    /// where the pane draws them.
    pub fn place_in(&mut self, rect: Rect, theme: &Theme) {
        self.set_rect(rect);
        self.place_controls(theme);
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

    #[must_use]
    pub fn on_revert<F: FnMut(&HistoryEntryId) + 'static>(mut self, f: F) -> Self {
        self.on_revert = Some(Box::new(f));
        self
    }

    /// Called with a change whose face opens it ([`EntryFace::opens`]) when a
    /// person chooses it, by click, Enter or assistive technology, instead
    /// of selecting it.
    #[must_use]
    pub fn on_open_entry<F: FnMut(&HistoryEntryId) + 'static>(mut self, f: F) -> Self {
        self.on_open_entry = Some(Box::new(f));
        self
    }

    pub fn set_on_open_entry<F: FnMut(&HistoryEntryId) + 'static>(&mut self, f: F) {
        self.on_open_entry = Some(Box::new(f));
    }

    fn open_row(&mut self, i: usize) -> bool {
        let Some(id) = self
            .row_face(i)
            .filter(|face| face.opens)
            .and(self.row_entry(i).map(|entry| entry.id))
        else {
            return false;
        };
        let Some(open) = self.on_open_entry.as_mut() else {
            return false;
        };
        open(&id);
        true
    }

    #[must_use]
    pub fn on_activate_entry<F: FnMut(&HistoryEntryId) + 'static>(mut self, f: F) -> Self {
        self.on_activate_entry = Some(Box::new(f));
        self
    }

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

    pub fn set_draws_own_surface(&mut self, draws: bool) {
        if self.draws_own_surface != draws {
            self.draws_own_surface = draws;
            self.dirty = true;
        }
    }

    #[must_use]
    pub fn with_fill(mut self, fill: bool) -> Self {
        self.fill = fill;
        self
    }

    /// Filter by who made the changes instead of by what kind they are: the
    /// strip shows Everyone, then each person and each agent in the history,
    /// and choosing one shows only their changes.
    pub fn set_filters_people(&mut self, people: bool) {
        if self.filters_people != people {
            self.filters_people = people;
            self.invalidate();
        }
    }

    /// Show only `author`'s changes, or everyone's (`None`).
    pub fn filter_by_author(&mut self, author: Option<&str>) {
        let authors: Vec<String> = author.map(str::to_owned).into_iter().collect();
        if self.filter.authors != authors {
            self.filter.authors = authors;
            self.rebuild();
            self.dirty = true;
        }
    }

    /// Who made the changes in this history, each once: people first, then
    /// agents, each in the order their latest change is listed.
    pub fn people(&self) -> Vec<HistoryAuthor> {
        let mut people: Vec<HistoryAuthor> = Vec::new();
        let mut agents: Vec<HistoryAuthor> = Vec::new();
        let mut order: Vec<usize> = (0..self.entries.len()).collect();
        if self.newest_first {
            order.compact_sort_by_key(|&i| {
                std::cmp::Reverse(self.entries.get(i).map(|e| e.timestamp))
            });
        }
        for index in order {
            let Some(entry) = self.entries.get(index) else {
                continue;
            };
            if entry.is_genesis() || entry.author.name.trim().is_empty() {
                continue;
            }
            let seen = people
                .iter()
                .chain(agents.iter())
                .any(|known| known.name == entry.author.name);
            if seen {
                continue;
            }
            if entry.author.relation == crate::widget::history::AuthorRelation::Software {
                agents.push(entry.author.clone());
            } else {
                people.push(entry.author.clone());
            }
        }
        people.extend(agents);
        people
    }

    pub fn set_time_range_label(&mut self, label: Option<String>) {
        if self.time_range_label != label {
            self.time_range_label = label;
            self.invalidate();
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
            || !self.filter.entry_kinds.is_empty()
            || self.filter.time_range.is_some()
    }

    pub fn active_filter_summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.active_chip != ChipKind::All {
            parts.push(self.active_chip.label());
        }
        let chip_classes = self.active_chip.entry_kinds();
        for op in self
            .filter
            .entry_kinds
            .iter()
            .filter(|op| !chip_classes.contains(op))
        {
            let label = (*op).label();
            if !parts.contains(&label) {
                parts.push(label);
            }
        }
        let typed = self.author_query.trim();
        if !typed.is_empty() {
            parts.push(typed.to_owned());
        }
        for a in &self.filter.authors {
            parts.push(a.clone());
        }
        if self.filter.time_range.is_some() {
            parts.push(
                self.time_range_label
                    .clone()
                    .unwrap_or_else(|| widget_text("history-filter-dates")),
            );
        }
        parts.join(" \u{00b7} ")
    }

    pub fn empty_state_lines(&self) -> (String, Option<String>) {
        if self.entries.is_empty() {
            return (widget_text("history-empty"), None);
        }
        let hidden = self.entries.len();
        let summary = self.active_filter_summary();
        let detail = if summary.is_empty() {
            widget_format("history-empty-hidden", &[("count", hidden.into())])
        } else {
            widget_format(
                "history-empty-hidden-by",
                &[("count", hidden.into()), ("filters", summary.into())],
            )
        };
        (widget_text("history-empty-filtered"), Some(detail))
    }

    pub fn author_pill_rect(&self) -> Rect {
        self.author_pill_rect
    }

    pub fn chip_rect(&self, kind: ChipKind) -> Option<Rect> {
        self.strip_chips.iter().find_map(|chip| match chip.target {
            StripTarget::Class(placed) if placed == kind => Some(chip.hit),
            _ => None,
        })
    }

    pub fn people_chip_rect(&self, author: Option<&str>) -> Option<Rect> {
        self.strip_chips.iter().find_map(|chip| match &chip.target {
            StripTarget::Person(placed) if placed.as_deref() == author => Some(chip.hit),
            _ => None,
        })
    }

    /// Show or hide the built-in author pill. The default is `true` for
    /// standalone use; hosts with an equivalent people control can turn it
    /// off without losing the operation-class filters.
    pub fn set_show_author_pill(&mut self, show: bool) {
        if self.show_author_pill != show {
            self.show_author_pill = show;
            self.invalidate();
        }
    }

    pub fn shows_author_pill(&self) -> bool {
        self.show_author_pill
    }

    pub fn set_show_day_headings(&mut self, show: bool) {
        if self.show_day_headings != show {
            self.show_day_headings = show;
            self.rebuild_rows();
        }
    }

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

    /// A band's second line as it is spoken: its facts in order.
    pub fn group_subtitle_at_row(&self, i: usize) -> Option<String> {
        match self.rows.get(i)? {
            VisualRow::Group(g) => Some(self.row_groups.get(*g)?.spoken_facts()),
            VisualRow::Entry(..) => None,
        }
    }

    pub fn row_depth_at(&self, i: usize) -> Option<usize> {
        (i < self.rows.len()).then_some(self.row_depth(i))
    }

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

    pub fn clear_row_focus(&mut self) {
        if self.focused_row.take().is_some() {
            self.dirty = true;
        }
    }

    pub fn activate_row(&mut self, row: usize) -> bool {
        let Some(visual) = self.rows.get(row).copied() else {
            return false;
        };
        let _ = self.focus_row(row);
        match visual {
            VisualRow::Group(_) => self.toggle_group_at_row(row),
            VisualRow::Entry(..) if self.open_row(row) => true,
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
        self.invalidate();
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
        self.invalidate();
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
            self.invalidate();
        }
    }

    pub fn commit_naming(&mut self) -> bool {
        let Some(state) = self.naming.take() else {
            return false;
        };
        self.invalidate();
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

    fn author_slot_w(&self) -> f64 {
        AUTHOR_SLOT_W
    }

    fn author_label(&self, entry: &HistoryEntry) -> String {
        if self.filters_people {
            entry.author.name.clone()
        } else {
            crate::history_source::sentence::author_line(&entry.author)
        }
    }

    fn bot_chip(
        entry: &HistoryEntry,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<(String, f64)> {
        is_bot_author(&entry.author).then(|| {
            let label = crate::i18n::widget_format("author-bot-chip", &[]);
            let width = crate::widget::ChipFace::intrinsic_width(
                &label,
                crate::widget::ChipLead::None,
                false,
                text_renderer,
                theme,
            );
            (label, width)
        })
    }

    fn avatar_rect(&self, i: usize) -> Option<Rect> {
        let entry = self.row_entry(i)?;
        if entry.is_genesis() {
            return None;
        }
        let row = RowSlot {
            rect: self.row_rect(i)?,
            depth: self.row_depth(i),
        }
        .indented();
        let cy = f64::midpoint(row.y0, row.y1);
        let x = row.x0 + 8.0;
        Some(Rect::new(
            x,
            cy - AVATAR_DIAM / 2.0,
            x + AVATAR_DIAM,
            cy + AVATAR_DIAM / 2.0,
        ))
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

    pub fn name_selection_rect(&self) -> Rect {
        self.selection_bar.name
    }

    pub fn clear_selection_rect(&self) -> Rect {
        self.selection_bar.clear
    }

    pub fn clear_filters_rect(&self) -> Rect {
        self.nothing.clear
    }

    fn row_height(&self, i: usize) -> f64 {
        match self.rows.get(i) {
            Some(VisualRow::Group(_)) => GROUP_ROW_HEIGHT,
            Some(VisualRow::Entry(entry, _)) => self
                .entries
                .get(*entry)
                .and_then(|entry| self.faces.get(&entry.id))
                .map_or(ROW_HEIGHT, |face| face.height(ROW_HEIGHT, &self.face_theme)),
            None => ROW_HEIGHT,
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
        let scroll = crate::widget::scroll_region::reveal_offset(
            self.scroll_y,
            viewport,
            top..bottom,
            self.max_scroll(),
        );
        if (scroll - self.scroll_y).abs() > f64::EPSILON {
            self.scroll_y = scroll;
            self.hover_row = self.row_at(self.last_cursor);
            self.dirty = true;
        }
    }

    /// Binary search over the cumulative tops rather than a scan, so a workspace
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

    pub fn row_rect(&self, i: usize) -> Option<Rect> {
        let rect = self.row_rect_unclipped(i)?;
        let y0 = rect.y0;
        let y1 = rect.y1;
        if y1 < self.rows_top() || y0 > self.rows_bottom() {
            return None;
        }
        Some(rect)
    }

    fn trailing_chip_rect(row: Rect, width: f64) -> Rect {
        let chip_h = ROW_HEIGHT - 16.0;
        let cy = f64::midpoint(row.y0, row.y1);
        Rect::new(
            row.x1 - width - 6.0,
            cy - chip_h / 2.0,
            row.x1 - 6.0,
            cy + chip_h / 2.0,
        )
    }

    fn copy_chip_painted(&self, row: Rect, beside_revert: bool) -> Rect {
        let width = self.revert_chip_w();
        let chip = Self::trailing_chip_rect(row, width);
        if beside_revert {
            chip - vello::kurbo::Vec2::new(width + 6.0, 0.0)
        } else {
            chip
        }
    }

    pub fn copy_chip_rect(&self, i: usize) -> Option<Rect> {
        if self.hover_row != Some(i) {
            return None;
        }
        self.row_face(i)?.copy.as_ref()?;
        let beside_revert = self.revert_chip_rect(i).is_some();
        let row = self.row_rect(i)?;
        let depth = self.row_depth(i);
        let row = RowSlot { rect: row, depth }.indented();
        Some(self.hit_rect_within(self.copy_chip_painted(row, beside_revert), row))
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

    fn note_frame_theme(&mut self, theme: &Theme) {
        if self.face_theme != *theme {
            self.face_theme = theme.clone();
        }
        self.min_pointer_target = f64::from(theme.tokens.metrics.min_pointer_target);
    }

    fn invalidate(&mut self) {
        self.dirty = true;
        self.layout_dirty = true;
    }

    /// Put the strip's chips and the empty state's way back out and the
    /// selection bar's actions where the pane's box puts them, measured in
    /// the theme's own type. The layout runs this, so a frame that is read
    /// before it is drawn — a test, assistive technology, a press — finds
    /// those controls inside the pane rather than at the window's origin,
    /// where a control nothing placed stands.
    fn place_controls(&mut self, theme: &Theme) {
        self.note_frame_theme(theme);
        let theme = self.face_theme.clone();
        crate::text_render::with_layout_renderer(&theme, |tr| {
            self.place_author_pill(tr, &theme);
            self.place_strip_chips(tr, &theme);
            self.place_selection_bar(tr, &theme);
            self.place_empty_state(tr, &theme);
        });
        self.layout_dirty = false;
    }

    /// The pointer target for a chip painted as `painted` inside `within`: grown vertically to the
    /// minimum pointer target, centred on the paint and clamped to its container, while the paint
    /// stays the designed 20 / 28 px pill. Neighbouring chips differ only in x, so grown targets
    /// never overlap. Hit-testing and accessibility read this rect; painting reads the original.
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

    pub fn group_chip_rects(&self, i: usize) -> Option<(Rect, Option<Rect>)> {
        let VisualRow::Group(g) = *self.rows.get(i)? else {
            return None;
        };
        let full = self.row_rect(i)?;
        let rg = self.row_groups.get(g)?;
        let row = Rect::new(
            full.x0 + f64_of_usize(rg.depth) * INDENT,
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

    pub fn group_undo_chip_rect(&self, i: usize) -> Option<Rect> {
        let VisualRow::Group(g) = *self.rows.get(i)? else {
            return None;
        };
        let full = self.row_rect(i)?;
        let rg = self.row_groups.get(g)?;
        let row = Rect::new(
            full.x0 + f64_of_usize(rg.depth) * INDENT,
            full.y0 + 2.0,
            full.x1,
            full.y1 - 2.0,
        );
        self.group_chip_rects_full(row, rg)
            .undo
            .map(|r| self.hit_rect_within(r, row))
    }

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

    fn passes(&self, e: &HistoryEntry, chip_ops: &[EntryKind], needle: &str) -> bool {
        if !self.filter.matches(e) {
            return false;
        }
        if !chip_ops.is_empty() && !chip_ops.contains(&e.entry_kind) {
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
        order.compact_sort_by(|(ai, at), (bi, bt)| {
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
        let chip_ops = self.active_chip.entry_kinds();
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
        self.layout_dirty = true;
    }

    fn rebuild_rows(&mut self) {
        let chip_ops = self.active_chip.entry_kinds();
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
        if self.focused_row.is_some() && self.focused_row_index().is_none() {
            self.focused_row = None;
        }
        if let Some(n) = &self.naming
            && let NameTarget::Group(g) = n.target
            && g >= self.row_groups.len()
        {
            self.naming = None;
        }
        self.invalidate();
    }

    fn emit_group(
        &self,
        g: &HistoryGroup,
        chip_ops: &[EntryKind],
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
        // The app may say what the band amounts to (its calculations, its
        // functions) where counting characters would mean nothing.
        let summary = self.presenter.as_ref().and_then(|presenter| {
            self.entries
                .get(g.start..g.end)
                .and_then(|members| presenter.group_summary(members))
        });
        let mut facts = g.subtitle_facts_summarised(&self.entries, summary);
        if visible_count < total_count {
            // Saying "12 changes" over three visible rows would be a lie the
            // reader cannot catch.
            facts.insert(
                0,
                widget_format(
                    "history-band-shown",
                    &[
                        ("shown", visible_count.into()),
                        ("total", total_count.into()),
                    ],
                ),
            );
        }
        if g.open {
            facts.insert(0, widget_text("history-band-open"));
        }
        let gi = row_groups.len();
        row_groups.push(RowGroup {
            key: g.key.clone(),
            title: g.title(&self.entries),
            facts,
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
            work: g.work.as_deref().and_then(work_icon),
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
            colors.intent.accent,
            None,
            &shape,
        );
    } else {
        crate::surface::card(scene, transform, rect, radius, colors.surfaces.raised);
    }
    let size = theme.text(TextRole::Caption);
    let shown = fit_text(tr, label, &size, rect.width() - 8.0);
    let (lw, lh) = tr.measure_style(&shown, None, &size);
    let ink = if accent {
        colors.intent.on_accent
    } else {
        colors.text_primary
    };
    tr.draw_style(
        scene,
        &shown,
        ink,
        transform.pre_translate(
            (
                rect.x0 + (rect.width() - f64::from(lw)) / 2.0,
                rect.y0 + (rect.height() - f64::from(lh)) / 2.0,
            )
                .into(),
        ),
        None,
        &size,
    );
}

#[derive(Clone, Copy)]
struct RowSlot {
    rect: Rect,
    depth: usize,
}

impl RowSlot {
    fn indented(self) -> Rect {
        Rect::new(
            self.rect.x0 + f64_of_usize(self.depth) * INDENT,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1,
        )
    }
}

#[derive(Clone, Copy)]
struct RowEmphasis {
    hovered: bool,
    selected: bool,
    focused: bool,
}

struct EntryRowLayout {
    row: Rect,
    cy: f64,
    text_style: TextStyle,
    author_slot_w: f64,
    offers_revert: bool,
    offers_copy: bool,
    /// Summary width given up to the revert or copy chip; zero unless one
    /// is offered.
    chip_reserve: f64,
}

struct BandHeader<'a> {
    rg: &'a RowGroup,
    row: Rect,
    title_x: f64,
    title_style: TextStyle,
}

struct StripChip {
    target: StripTarget,
    label: String,
    lead: crate::widget::ChipLead,
    painted: Rect,
    hit: Rect,
    selected: bool,
}

#[derive(Clone, PartialEq, Debug)]
enum StripTarget {
    Class(ChipKind),
    Person(Option<String>),
}

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
                Self::Today => "day-today",
                Self::Yesterday => "day-yesterday",
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
        chip_ops: &[EntryKind],
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
            facts: Vec::new(),
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
            work: None,
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
        let dot_x = if entry.is_genesis() {
            row.x0 + 8.0 + AVATAR_DIAM / 2.0
        } else {
            Self::draw_row_avatar(ctx, row, cy, entry)
        };

        // Reserved only when hovered — reserving it on every row cost the
        // summary a third of its width for something not on screen.
        let offers_revert = emphasis.hovered && self.on_revert.is_some() && entry.is_revertible();
        let offers_copy = emphasis.hovered
            && self
                .faces
                .get(&entry.id)
                .is_some_and(|face| face.copy.is_some());
        let bot_w = if entry.is_genesis() {
            0.0
        } else {
            Self::bot_chip(entry, ctx.text, ctx.theme).map_or(0.0, |(_, width)| width + 6.0)
        };
        let layout = EntryRowLayout {
            row,
            cy,
            text_style: ctx.theme.text(TextRole::Caption),
            author_slot_w: self.author_slot_w() + bot_w,
            offers_revert,
            offers_copy,
            chip_reserve: (self.revert_chip_w() + 6.0)
                * f64::from(u8::from(offers_revert) + u8::from(offers_copy)),
        };
        if !entry.is_genesis() {
            self.draw_row_author(ctx, &layout, entry, dot_x);
        }
        self.draw_row_body(ctx, &layout, entry, now);
        self.draw_row_trailing(ctx, &layout, entry, now);
    }

    /// The author's face; returns its center x so the author cluster can
    /// start just past it. A person is their round face in their colour;
    /// software acting for someone (an agent, a Shortcut, an app) is the
    /// agent mark on a square, as it shows everywhere else.
    fn draw_row_avatar(ctx: &mut PaintCtx<'_>, row: Rect, cy: f64, entry: &HistoryEntry) -> f64 {
        let (r, g, b) = entry.author.rgb();
        let dot_x = row.x0 + 8.0 + AVATAR_DIAM / 2.0;
        let face = Rect::new(
            dot_x - AVATAR_DIAM / 2.0,
            cy - AVATAR_DIAM / 2.0,
            dot_x + AVATAR_DIAM / 2.0,
            cy + AVATAR_DIAM / 2.0,
        );
        author_face(&entry.author, Color::from_rgb8(r, g, b)).paint(ctx, face);
        dot_x
    }

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
            let x = full_row.x0 + (f64_of_usize(level) - 0.5) * INDENT;
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
                theme.tokens.colors.intent.accent_hover,
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
            author_slot_w,
            ..
        } = layout;
        let Some((label, width)) = Self::bot_chip(entry, text_renderer, theme) else {
            return;
        };
        let slot_end = row.x0 + author_slot_w - 6.0;
        let chip_x = dot_x + AVATAR_DIAM / 2.0 + 6.0;
        let height = crate::widget::ChipFace::height(theme).min(row.height());
        let chip = Rect::new(
            chip_x,
            cy - height / 2.0,
            (chip_x + width).min(slot_end),
            cy + height / 2.0,
        );
        crate::widget::ChipFace::new(chip, &label).paint(scene, transform, text_renderer, theme);
    }

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
        // A change the app presents itself draws as the app draws it, in the
        // slot the kind icon and the sentence share. A redacted one never
        // does: its strike-through says it was taken back.
        if entry.redaction.is_none()
            && let Some(body) = self.faces.get(&entry.id).and_then(|face| face.body.clone())
        {
            let slot = Rect::new(
                glyph_x,
                row.y0,
                (row.x1 - self.time_slot_w() - chip_reserve - TRAILING_INSET).max(glyph_x),
                row.y1,
            );
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &slot,
            );
            body.paint(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                slot,
            );
            scene.pop_layer();
            return;
        }
        let icon_side = f64::from(theme.tokens.metrics.icon_sm);
        crate::icons::draw_icon(
            scene,
            transform,
            Rect::new(
                glyph_x,
                cy - icon_side / 2.0,
                glyph_x + icon_side,
                cy + icon_side / 2.0,
            ),
            entry.entry_kind.icon(),
            theme.tokens.colors.text_dim,
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );

        let line_top = text_top(text_renderer, text_style, cy);
        let summary_x = glyph_x + 24.0;
        let summary_x_end = row.x1 - self.time_slot_w() - chip_reserve - TRAILING_INSET;
        let (summary_owned, is_redacted_row): (String, bool) =
            if let Some(redaction) = entry.redaction.as_ref() {
                (
                    widget_format(
                        "history-redacted",
                        &[
                            ("who", redaction.redactor_label.as_str().into()),
                            (
                                "when",
                                format_relative_time(redaction.redacted_at, now).into(),
                            ),
                            ("reason", redaction.reason.as_str().into()),
                        ],
                    ),
                    true,
                )
            } else {
                let breadcrumb = if self.filters_people {
                    // One document's History: every change is of this app and
                    // this document, so naming them is noise.
                    None
                } else {
                    Self::entry_breadcrumb(entry)
                };
                let said = self.summary_of(entry);
                let summary = breadcrumb.map_or_else(
                    || said.to_owned(),
                    |breadcrumb| format!("{breadcrumb} · {said}"),
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
            transform.pre_translate((summary_x, line_top).into()),
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
            offers_copy,
            chip_reserve,
            ..
        } = layout;
        if offers_copy {
            draw_chip(
                scene,
                transform,
                text_renderer,
                theme,
                self.copy_chip_painted(row, offers_revert),
                &widget_text("history-entry-copy"),
                false,
            );
        }
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
            transform.pre_translate((time_x, cy - f64::from(th) / 2.0).into()),
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
                &widget_text("history-entry-revert"),
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
            full_row.x0 + f64_of_usize(rg.depth) * INDENT,
            full_row.y0 + 2.0,
            full_row.x1,
            full_row.y1 - 2.0,
        );
        Self::draw_group_row_chrome(ctx, full_row, row, rg, emphasis);

        let mut title_x = row.x0 + 26.0;
        if let Some((icon, _)) = rg.work {
            let side = f64::from(ctx.theme.tokens.metrics.icon_sm);
            let top = row.y0 + 15.0 - side / 2.0;
            crate::icons::draw_icon(
                ctx.scene,
                ctx.transform,
                Rect::new(title_x, top, title_x + side, top + side),
                icon,
                ctx.theme.tokens.colors.text_dim,
                crate::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            title_x += side + 6.0;
        }
        let band = BandHeader {
            rg,
            row,
            title_x,
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
                    &crate::i18n::I18nManager::lookup_plain(if rg.name.is_some() {
                        "history-band-rename"
                    } else {
                        "history-band-name"
                    }),
                    rg.name.is_some(),
                );
            }
            if let Some(r) = self.group_chip_rects_full(row, rg).undo {
                draw_chip(
                    scene,
                    transform,
                    tr,
                    theme,
                    r,
                    &widget_text("history-band-undo"),
                    false,
                );
            }
            if let Some(r) = restore_rect {
                draw_chip(
                    scene,
                    transform,
                    tr,
                    theme,
                    r,
                    &widget_text("history-band-restore"),
                    false,
                );
            }
        }
    }

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
            let x = full_row.x0 + (f64_of_usize(level) - 0.5) * INDENT;
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
        if rg.name.is_some() {
            let spine = RoundedRect::from_rect(
                Rect::new(row.x0, row.y0 + 4.0, row.x0 + 3.0, row.y1 - 4.0),
                1.5,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                colors.text_tertiary,
                None,
                &spine,
            );
        }

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
                transform.pre_translate((title_x, title_y).into()),
                None,
                title_style,
            );
            if rg.open {
                let (w, h) = tr.measure_style(&title, None, title_style);
                let dot_x = title_x + f64::from(w) + 8.0;
                if dot_x + 5.0 < row.x1 - actions_w {
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        colors.intent.accent_hover,
                        None,
                        &Circle::new(Point::new(dot_x, title_y + f64::from(h) / 2.0), 3.0),
                    );
                }
            }
        }
    }

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
        let faces = rg.authors.len().min(3);
        let face_d = AVATAR_DIAM;
        let overlap = f64::from(theme.tokens.spacing.s1);
        let dots_w = if faces > 0 {
            face_d + f64_of_usize(faces - 1) * (face_d - overlap) + 6.0
        } else {
            0.0
        };
        let sub_budget =
            (row.x1 - title_x - f64::from(rel_w) - dots_w - TRAILING_INSET - 16.0).max(0.0);
        let facts = rg.meta_facts();
        let facts_rect = Rect::new(
            title_x,
            meta_y,
            title_x + sub_budget,
            meta_y + f64::from(rel_h),
        );
        crate::widget::primitives::FactsFace::new(&facts)
            .paint(&mut PaintCtx::new(scene, transform, tr, theme), facts_rect);
        // Right-aligned to the same gutter as the rows' time column, so the
        // band header's "2 hr ago" and the rows' "1 hr ago" share one edge.
        tr.draw_style(
            scene,
            &rel,
            colors.text_dim,
            transform.pre_translate((row.x1 - TRAILING_INSET - f64::from(rel_w), meta_y).into()),
            None,
            &meta_style,
        );

        // Their faces, overlapping, on the meta line's measured centre: each
        // in their own colour, as presence shows them.
        let face_cy = meta_y + f64::from(rel_h) / 2.0;
        let mut x1 = row.x1 - TRAILING_INSET - 6.0 - f64::from(rel_w);
        let ground = theme.tokens.colors.surfaces.inspector;
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        for a in rg.authors.iter().take(3) {
            let (r, g, b) = a.rgb();
            let face = Rect::new(
                x1 - face_d,
                face_cy - face_d / 2.0,
                x1,
                face_cy + face_d / 2.0,
            );
            author_face(a, Color::from_rgb8(r, g, b))
                .ringed_on(ground)
                .paint(&mut ctx, face);
            x1 -= face_d - overlap;
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

    /// Paint the strip: the chips the layout placed in it, then the author
    /// pill. Where each stands is the layout's; painting adds nothing to
    /// what a press can find.
    fn draw_filter_strip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        // The filter chips are the shared chip: quiet at rest, a lift when
        // chosen (HIG §9.5).
        for chip in &self.strip_chips {
            crate::widget::ChipFace::new(chip.painted, &chip.label)
                .lead(chip.lead)
                .selected(chip.selected)
                .paint(scene, transform, text_renderer, theme);
        }
        let pill = self.author_pill_painted;
        if pill.is_zero_area() {
            return;
        }
        crate::widget::ChipFace::new(pill, &self.author_pill_label())
            .selected(!self.author_query.is_empty())
            .paint(scene, transform, text_renderer, theme);
    }

    fn author_pill_label(&self) -> String {
        if self.author_query.is_empty() {
            widget_text("history-author-anyone")
        } else {
            self.author_query.clone()
        }
    }

    /// The room the author pill takes at the strip's right edge, which the
    /// class chips leave it: its label, clamped under half the strip.
    fn author_pill_width(&self, tr: &mut TextRenderer, theme: &Theme) -> f64 {
        if !self.show_author_pill {
            return 0.0;
        }
        crate::widget::ChipFace::intrinsic_width(
            &self.author_pill_label(),
            crate::widget::ChipLead::None,
            false,
            tr,
            theme,
        )
        .min(self.filter_strip_rect().width() * 0.45)
    }

    /// Where the author pill stands: anchored to the strip's right edge, in
    /// the strip's height. Placed by the layout, so a frame read before it
    /// paints — a test, assistive technology, a press — finds it in the strip
    /// rather than at the window's origin.
    fn place_author_pill(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let strip = self.filter_strip_rect();
        let shown = !self.filters_people && self.show_author_pill;
        let width = if shown {
            self.author_pill_width(tr, theme)
        } else {
            0.0
        };
        self.author_pill_painted = if shown {
            let h = crate::widget::ChipFace::height(theme).min(strip.height());
            let cy = f64::midpoint(strip.y0, strip.y1);
            Rect::new(strip.x1 - width, cy - h / 2.0, strip.x1, cy + h / 2.0)
        } else {
            Rect::ZERO
        };
        self.author_pill_rect = if self.author_pill_painted.is_zero_area() {
            Rect::ZERO
        } else {
            self.hit_rect_within(self.author_pill_painted, strip)
        };
    }

    /// Where the strip's chips stand, in the strip's height, measured in the
    /// theme's type: the class chips left to right in the room the author
    /// pill leaves them, or — when the strip filters by who — Everyone and
    /// then a chip per person and agent. Chips that do not fit are left out
    /// rather than squeezed, and a chip left out has no place to press.
    fn place_strip_chips(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let strip = self.filter_strip_rect();
        let chip_h = crate::widget::ChipFace::height(theme).min(strip.height());
        let cy = f64::midpoint(strip.y0, strip.y1);
        let mut chips: Vec<StripChip> = Vec::new();
        let mut x = strip.x0;

        if self.filters_people {
            let chosen = self.filter.authors.first().cloned();
            let mut offered: Vec<(Option<String>, String, crate::widget::ChipLead)> = vec![(
                None,
                widget_text("history-chip-all"),
                crate::widget::ChipLead::None,
            )];
            for author in self.people() {
                let lead = if author.relation == crate::widget::history::AuthorRelation::Software {
                    crate::widget::ChipLead::Glyph(crate::icons::Icon::Bot)
                } else {
                    let (r, g, b) = author.rgb();
                    crate::widget::ChipLead::Face(Color::from_rgb8(r, g, b))
                };
                let label = author.name.clone();
                offered.push((Some(author.name.clone()), label, lead));
            }
            for (author, label, lead) in offered {
                let chip_w =
                    crate::widget::ChipFace::intrinsic_width(&label, lead, false, tr, theme);
                if x + chip_w > strip.x1 && !chips.is_empty() {
                    break;
                }
                let painted = Rect::new(x, cy - chip_h / 2.0, x + chip_w, cy + chip_h / 2.0);
                chips.push(StripChip {
                    selected: author == chosen,
                    target: StripTarget::Person(author),
                    label,
                    lead,
                    painted,
                    hit: self.hit_rect_within(painted, strip),
                });
                x += chip_w + CHIP_GAP;
            }
            self.strip_chips = chips;
            return;
        }

        let chips_budget = if self.show_author_pill {
            (strip.width() - self.author_pill_painted.width() - 8.0).max(0.0)
        } else {
            strip.width()
        };
        let mut run = 0.0;
        for chip in ChipKind::ALL {
            run += crate::widget::ChipFace::intrinsic_width(
                &chip.label(),
                crate::widget::ChipLead::None,
                false,
                tr,
                theme,
            ) + CHIP_GAP;
        }
        let use_short = run > chips_budget;
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
                tr,
                theme,
            );
            // Stop rather than place a chip half-under the author pill; a
            // chip nothing placed cannot be pressed.
            if x + chip_w > strip.x0 + chips_budget && !chips.is_empty() {
                break;
            }
            let painted = Rect::new(x, cy - chip_h / 2.0, x + chip_w, cy + chip_h / 2.0);
            chips.push(StripChip {
                selected: self.active_chip == chip,
                target: StripTarget::Class(chip),
                label,
                lead: crate::widget::ChipLead::None,
                painted,
                hit: self.hit_rect_within(painted, strip),
            });
            x += chip_w + CHIP_GAP;
        }
        self.strip_chips = chips;
    }

    /// Where the bar's count, its way out and its name action stand: decided
    /// by the layout, so a frame read before it paints finds them in the bar.
    fn selection_bar_placement(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> SelectionBarPlacement {
        let mut out = SelectionBarPlacement::default();
        if self.filters_people && self.preview_selection {
            return out;
        }
        let Some((a, b)) = self.selection else {
            return out;
        };
        let bar = self.selection_bar_rect();
        if bar.height() < 20.0 {
            return out;
        }
        let style = theme.text(TextRole::Caption);
        // `selection` is normalised so `b >= a` (see `set_selection`).
        let n = b.saturating_sub(a).saturating_add(1);
        let naming = matches!(
            self.naming.as_ref().map(|s| &s.target),
            Some(NameTarget::Selection)
        );

        let chip_w = |tr: &mut TextRenderer, label: &str, min: f64| {
            (f64::from(tr.measure_style(label, None, &style).0) + 20.0).max(min)
        };
        let clear_w = chip_w(tr, &widget_text("history-selection-clear"), 54.0);
        let action_w = if naming || self.preview_selection || self.on_name_group.is_none() {
            0.0
        } else {
            chip_w(tr, &widget_text("history-selection-name"), 96.0)
        };
        let clear = Rect::new(
            bar.x1 - 8.0 - clear_w,
            bar.center().y - 11.0,
            bar.x1 - 8.0,
            bar.center().y + 11.0,
        );
        let label = widget_format("history-selection-count", &[("count", n.into())]);
        let label_budget = (bar.width() - clear_w - action_w - 40.0).max(0.0);
        out.count = fit_text(tr, &label, &style, label_budget);
        out.count_at = Point::new(bar.x0 + 10.0, text_top(tr, &style, bar.center().y));
        out.clear = clear;
        if naming {
            let field_x0 =
                bar.x0 + 10.0 + f64::from(tr.measure_style(&out.count, None, &style).0) + 12.0;
            out.field = Rect::new(
                field_x0,
                bar.center().y - 12.0,
                clear.x0 - 8.0,
                bar.center().y + 12.0,
            );
        } else if self.on_name_group.is_some() {
            let action = Rect::new(
                clear.x0 - 6.0 - action_w,
                bar.center().y - 11.0,
                clear.x0 - 6.0,
                bar.center().y + 11.0,
            );
            if !self.preview_selection && action.x0 > bar.x0 + 10.0 {
                out.name = action;
            }
        }
        out.bar = bar;
        out
    }

    fn place_selection_bar(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        self.selection_bar = self.selection_bar_placement(tr, theme);
    }

    fn draw_selection_bar(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let placed = &self.selection_bar;
        if placed.bar.area() <= 0.0 {
            return;
        }
        let bar = placed.bar;
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
        tr.draw_style(
            scene,
            &placed.count,
            colors.text_primary,
            transform.pre_translate(placed.count_at.to_vec2()),
            None,
            &style,
        );

        if placed.field.width() > 40.0
            && let Some(state) = self.naming.as_ref()
        {
            state
                .field
                .draw(scene, transform, tr, theme, placed.field, &style);
        }
        let clear_label = widget_text("history-selection-clear");
        draw_chip(
            scene,
            transform,
            tr,
            theme,
            placed.clear,
            &clear_label,
            false,
        );
        if !placed.name.is_zero_area() {
            let name_label = widget_text("history-selection-name");
            draw_chip(scene, transform, tr, theme, placed.name, &name_label, true);
        }
    }
}

impl Widget for ActivityTimeline {
    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        if !self.rect.contains(point) {
            return None;
        }
        (0..self.row_count()).find_map(|i| {
            let face = self.avatar_rect(i).filter(|face| face.contains(point))?;
            Some((face, self.author_label(self.row_entry(i)?)))
        })
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.note_frame_theme(theme);
        if !self.fill {
            // Its strip, its rows and its padding: the page scrolls it with the rest.
            let height = self.content_height() + FILTER_STRIP_HEIGHT + PADDING * 3.0;
            let style = taffy::Style {
                size: taffy::Size {
                    width: taffy::style_helpers::percent(1.0_f32),
                    height: taffy::style_helpers::length(crate::num::f32_of_f64(height)),
                },
                ..Default::default()
            };
            return taffy.widget_leaf(style, node_map, self);
        }
        taffy.fill_leaf(node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let prev = self.rect;
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if prev != self.rect {
            self.dirty = true;
        }
        let theme = self.face_theme.clone();
        self.place_controls(&theme);
    }

    /// Dirty while what it places may have moved: a selection, a name being
    /// typed, a filter, a list that changed. A leaf that always answered
    /// "dirty" re-solved its box every frame for a strip and a bar nobody
    /// moved.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO || self.layout_dirty
    }

    fn layout_measured(&mut self) {
        self.layout_dirty = false;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.note_frame_theme(theme);
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
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Character(c),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if (ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key())
                && c.eq_ignore_ascii_case("c")
                && self
                    .focused_row()
                    .and_then(|row| self.row_face(row))
                    .is_some_and(|face| face.copy.is_some()) =>
            {
                *ctx.handled = self.focused_row().is_some_and(|row| self.copy_row(row));
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
                        let description = item.spoken_facts();
                        Some(
                            AccessNode::new(Role::TreeItem)
                                .with_action_target(format!("timeline-group:{}", item.key))
                                .with_name(item.title.clone())
                                .with_description(description)
                                .with_position(index.saturating_add(1), row_count)
                                .with_level(item.depth.saturating_add(1))
                                .with_bounds(bounds)
                                .with_state(
                                    AccessState::new()
                                        .focusable(true)
                                        .focused(
                                            self.focused_row.as_ref()
                                                == Some(&FocusedRow::Group(item.key.clone())),
                                        )
                                        .expanded(item.expanded),
                                ),
                        )
                    }
                    VisualRow::Entry(entry, depth) => {
                        let item = self.entries.get(*entry)?;
                        let id = crate::widget::history_group::hex16(&item.id);
                        let said = if item.is_genesis() {
                            self.summary_of(item).to_owned()
                        } else {
                            format!(
                                "{}: {}",
                                crate::history_source::sentence::author_line(&item.author),
                                self.summary_of(item)
                            )
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
                                        item.entry_kind.glyph().to_owned(),
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
                                .with_state(
                                    AccessState::new()
                                        .focusable(true)
                                        .focused(
                                            self.focused_row.as_ref()
                                                == Some(&FocusedRow::Entry(item.id)),
                                        )
                                        .selected(self.selection.is_some_and(|(start, end)| {
                                            *entry >= start && *entry <= end
                                        })),
                                ),
                        )
                    }
                }
            })
            .collect();
        Some(
            AccessNode::new(Role::List)
                .with_name(widget_text("history-entries"))
                .with_description(description)
                .with_state(
                    AccessState::new()
                        .focusable(self.is_focusable())
                        .focused(self.has_focus),
                )
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

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        Some(crate::ime::InputContent::TEXT)
    }

    crate::redraw_flag!();
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

    /// Where the empty state's lines and its way back out stand: centred in
    /// what the pane has under the strip, decided by the layout so a frame
    /// read before it paints finds the `[Clear filters]` chip there.
    fn empty_placement(&self, tr: &mut TextRenderer, theme: &Theme) -> NothingPlacement {
        let mut out = NothingPlacement::default();
        if !self.rows.is_empty() {
            return out;
        }
        let (title, detail) = self.empty_state_lines();
        let style = theme.text(TextRole::Body);
        let small = theme.text(TextRole::Caption);
        let rows_top = self.rows_top();
        let cx = f64::midpoint(self.rect.x0, self.rect.x1);
        let cy = (rows_top + self.rect.y1 - PADDING) / 2.0;
        let budget = (self.rect.width() - 2.0 * PADDING - 16.0).max(0.0);

        out.title = fit_text(tr, &title, &style, budget);
        let (w, h) = tr.measure_style(&out.title, None, &style);
        let title_y = if detail.is_some() {
            cy - f64::from(h) - 14.0
        } else {
            cy - f64::from(h) / 2.0
        };
        out.title_at = Point::new(cx - f64::from(w) / 2.0, title_y);

        if let Some(detail) = detail {
            let detail = fit_text(tr, &detail, &small, budget);
            let (dw, dh) = tr.measure_style(&detail, None, &small);
            let detail_y = title_y + f64::from(h) + 8.0;
            out.detail = Some((detail, Point::new(cx - f64::from(dw) / 2.0, detail_y)));

            let label = widget_text("history-filters-clear");
            let (lw, lh) = tr.measure_style(&label, None, &small);
            let chip_w = (f64::from(lw) + 20.0).min(budget.max(24.0));
            let chip_h = f64::from(lh) + 10.0;
            let chip_y0 = detail_y + f64::from(dh) + 12.0;
            if chip_y0 + chip_h <= self.rect.y1 - PADDING {
                out.clear = Rect::new(
                    cx - chip_w / 2.0,
                    chip_y0,
                    cx + chip_w / 2.0,
                    chip_y0 + chip_h,
                );
            }
        }
        out
    }

    fn place_empty_state(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        self.nothing = self.empty_placement(tr, theme);
    }

    fn draw_empty_state(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let placed = &self.nothing;
        let style = theme.text(TextRole::Body);
        let small = theme.text(TextRole::Caption);
        text_renderer.draw_style(
            scene,
            &placed.title,
            theme.tokens.colors.text_dim,
            transform.pre_translate(placed.title_at.to_vec2()),
            None,
            &style,
        );

        if let Some((detail, at)) = &placed.detail {
            text_renderer.draw_style(
                scene,
                detail,
                theme.tokens.colors.text_dim,
                transform.pre_translate(at.to_vec2()),
                None,
                &small,
            );
            let label = widget_text("history-filters-clear");
            draw_chip(
                scene,
                transform,
                text_renderer,
                theme,
                placed.clear,
                &label,
                false,
            );
        }
    }

    /// Paints only the lines the viewport can show, so a workspace with ten
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
        // The way out is zeroed by the layout the moment the empty state
        // leaves the pane, so this can never fire on a stale hit zone.
        let clear_filters = self.nothing.clear;
        if clear_filters.area() > 0.0 && clear_filters.contains(self.last_cursor) {
            self.clear_filters();
            self.dirty = true;
            *handled = true;
            return;
        }

        let placed = &self.selection_bar;
        let bar = placed.bar;
        if bar.area() > 0.0 && bar.contains(self.last_cursor) {
            if placed.clear.contains(self.last_cursor) {
                self.naming = None;
                self.selection = None;
                self.selection_anchor = None;
                self.rebuild_rows();
            } else if placed.name.area() > 0.0 && placed.name.contains(self.last_cursor) {
                self.begin_naming_selection();
                *focus_target = Some(node_id);
            }
            *handled = true;
            return;
        }

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
        let target = self
            .strip_chips
            .iter()
            .find(|chip| chip.hit.contains(self.last_cursor))
            .map(|chip| chip.target.clone());
        if let Some(target) = target {
            match target {
                StripTarget::Person(author) => self.filter_by_author(author.as_deref()),
                StripTarget::Class(kind) => {
                    if self.active_chip != kind {
                        self.active_chip = kind;
                        self.rebuild();
                        self.dirty = true;
                    }
                }
            }
            *handled = true;
            return;
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
        if self
            .copy_chip_rect(i)
            .is_some_and(|chip| chip.contains(self.last_cursor))
        {
            let _ = self.copy_row(i);
            *handled = true;
            return;
        }
        if !modifiers.state().shift_key() && self.open_row(i) {
            *handled = true;
            return;
        }
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

fn is_bot_author(author: &crate::widget::history::HistoryAuthor) -> bool {
    author
        .attribution
        .as_ref()
        .is_some_and(|attribution| attribution.kind == tilo_actions::ActorKind::Bot)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod group_tests;
