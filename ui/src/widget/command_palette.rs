//! `CommandPalette` primitive — a fuzzy-matching searchable command list.
//!
//! Spec: `docs/superpowers/specs/2026-04-18-nirios-shell-design.md` — Phase A
//! step 9 (`CommandPalette` primitive + `GlobalSearch` using it).
//!
//! The palette holds a list of [`CommandEntry`] rows, supports a query string
//! and an optional composable filter predicate, and exposes the currently
//! visible entries via [`CommandPalette::visible_entries`]. Enter invokes the
//! selected entry's `on_select` callback with the CRDT handle and root path,
//! matching `quick-ui`'s direct-mutation event model.

use automerge::{AutoCommit, ObjId};
use std::collections::HashSet;
use std::sync::Arc;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::icons::{self, Icon};
use crate::tokens::colors::ON_CARD_SMALL_TEXT_CONTRAST;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;
use qos_actions::{ActionInput, FlatAction};

/// Scope chip in the GlobalSearch filter strip — concept renders these
/// as pill toggles immediately under the prompt row
/// (`nirios-concept/src/components/GlobalSearch.tsx:184-197`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum SearchScope {
    /// Limit results to the active workspace.
    Workspace,
    /// Search across every workspace.
    All,
}

/// Filetype filter chip — concept paints multi-select pills next to
/// the scope chips (`GlobalSearch.tsx:201-221`). The variant set here
/// matches the four buckets surfaced in the prompt; the underlying
/// command list still supplies its own filtering predicate.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Filetype {
    Docs,
    Code,
    Image,
    Video,
}

impl Filetype {
    pub const ALL: [Filetype; 4] =
        [Filetype::Docs, Filetype::Code, Filetype::Image, Filetype::Video];

    /// Short, all-caps label rendered inside the chip (per concept's
    /// `text-[10px] font-bold uppercase`).
    pub fn label(&self) -> &'static str {
        match self {
            Filetype::Docs => "DOCS",
            Filetype::Code => "CODE",
            Filetype::Image => "IMG",
            Filetype::Video => "VID",
        }
    }
}

/// Visual + interaction state for the filter strip painted under the
/// prompt row. Concept `GlobalSearch.tsx:184-244` — scope chips +
/// filetype chips + a full-text toggle.
///
/// Kept inside the palette widget for now; once `quick-shell::search`
/// gains its own `SearchFilter` (plan task T6 step 3), the chrome can
/// keep that as the source of truth and mirror it onto the palette
/// each frame via the existing per-frame draw path.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchFilter {
    pub scope: SearchScope,
    pub filetypes: HashSet<Filetype>,
    pub full_text: bool,
}

impl Default for SearchFilter {
    fn default() -> Self {
        Self {
            scope: SearchScope::Workspace,
            filetypes: HashSet::new(),
            full_text: false,
        }
    }
}

impl SearchFilter {
    pub fn toggle_full_text(&mut self) { self.full_text = !self.full_text; }

    pub fn toggle_filetype(&mut self, ft: Filetype) {
        if !self.filetypes.remove(&ft) {
            self.filetypes.insert(ft);
        }
    }

    pub fn set_scope(&mut self, scope: SearchScope) { self.scope = scope; }
}

type SelectFn = dyn Fn(&mut AutoCommit, &ObjId) + Send + Sync;

/// Declares that an entry, when chosen, first collects a free-text argument
/// from the user before it completes — e.g. "Go to cell: B12". The palette
/// switches into an argument-entry step showing `prompt` as a heading and an
/// editable field seeded with `prefill`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArgumentSpec {
    /// Heading shown while the argument is being typed (e.g. "Go to cell").
    pub prompt: String,
    /// Dimmed placeholder shown while the field is empty (e.g. "e.g. B12").
    pub placeholder: String,
    /// Text the field is pre-seeded (and pre-selected) with when the step opens.
    pub prefill: Option<String>,
}

/// The result of pressing Enter on an entry: its `id` plus the free-text
/// `argument` the user typed (only `Some` for entries built with
/// [`CommandEntry::with_argument`]). Drained by the host through
/// [`CommandPalette::take_outcome`] so an app can dispatch to its own
/// `&mut self` methods — the app-agnostic alternative to the
/// [`CommandEntry::on_select`] CRDT callback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutcome {
    pub id: String,
    pub argument: Option<String>,
}

/// Which chrome the palette wears.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PaletteMode {
    /// GlobalSearch styling: scope + filetype chips under the prompt and
    /// single-line result rows. The default; byte-for-byte the shell's Cmd+K
    /// search overlay.
    Search,
    /// Command-launcher styling: no filter chips, taller rows that show a
    /// per-entry description and an optional keybinding hint. This is the
    /// reusable "Ctrl+K command palette" any app seeds with its own actions.
    Commands,
}

/// One row in the palette.
#[derive(Clone)]
pub struct CommandEntry {
    pub id: String,
    pub label: String,
    /// One-line explanation rendered under the label in [`PaletteMode::Commands`]
    /// so users learn what each command does. `None` → label only.
    pub description: Option<String>,
    /// Optional keybinding hint (e.g. "Ctrl+D") rendered right-aligned in the
    /// row. Purely informational — the palette never binds it.
    pub keybinding: Option<String>,
    /// When set, choosing this entry collects a free-text argument before it
    /// completes (see [`ArgumentSpec`]).
    pub argument: Option<ArgumentSpec>,
    /// Section this row belongs to. Consecutive entries sharing a group
    /// render under one header; the palette draws a header row whenever
    /// the group changes. `None` → no header, the row joins whatever
    /// section precedes it.
    ///
    /// Grouping is how a host classifies rows *structurally* instead of
    /// smuggling a `[Files]` tag into [`Self::description`] — that tag
    /// was debug output rendered as UI, repeated on every row, and it
    /// made the subtitle unreadable.
    pub group: Option<String>,
    /// Total number of rows the host had for this entry's group before
    /// any per-group cap. Read off the *first* entry of a group and
    /// rendered next to the header, so a capped section still tells the
    /// truth about how much it is hiding. `None` → show the painted
    /// count.
    pub group_total: Option<usize>,
    /// Leading glyph for the row. Gives each result kind a visual
    /// identity at a glance — the job the bracketed literal was doing
    /// badly.
    pub icon: Option<Icon>,
    /// Short trailing chip stating a *fact about this row* that the
    /// title cannot carry — e.g. that the app it opens is not on this
    /// machine yet.
    ///
    /// Deliberately the same shape and weight as the keybinding chip:
    /// a row that differs from its neighbours has to be findable at a
    /// glance without becoming the loudest thing in a list of twenty.
    /// Keep it to two or three words; it is elided against the title,
    /// not given a line of its own. A row may carry a badge or a
    /// keybinding, not both — they occupy the same slot.
    pub badge: Option<String>,
    pub on_select: Option<Arc<SelectFn>>,
}

impl std::fmt::Debug for CommandEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandEntry")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("description", &self.description)
            .field("keybinding", &self.keybinding)
            .field("argument", &self.argument)
            .field("group", &self.group)
            .field("badge", &self.badge)
            .field("on_select", &self.on_select.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl CommandEntry {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
            keybinding: None,
            argument: None,
            group: None,
            group_total: None,
            icon: None,
            badge: None,
            on_select: None,
        }
    }

    /// Attach the trailing fact chip. See [`Self::badge`].
    pub fn badge(mut self, b: impl Into<String>) -> Self {
        self.badge = Some(b.into());
        self
    }

    /// Attach a one-line description shown under the label (Commands mode).
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }

    /// Put this row in a named section. See [`Self::group`].
    pub fn group(mut self, g: impl Into<String>) -> Self {
        self.group = Some(g.into());
        self
    }

    /// Declare how many rows the group actually has, when the host
    /// capped how many it handed over. See [`Self::group_total`].
    pub fn group_total(mut self, total: usize) -> Self {
        self.group_total = Some(total);
        self
    }

    /// Attach a leading glyph. See [`Self::icon`].
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Attach a keybinding hint rendered right-aligned in the row.
    pub fn keybinding(mut self, k: impl Into<String>) -> Self {
        self.keybinding = Some(k.into());
        self
    }

    /// Mark this entry as taking a free-text argument, collected in a second
    /// step before the outcome is emitted (e.g. "Go to cell: B12").
    pub fn with_argument(
        mut self,
        prompt: impl Into<String>,
        placeholder: impl Into<String>,
    ) -> Self {
        let prefill = self.argument.take().and_then(|s| s.prefill);
        self.argument = Some(ArgumentSpec {
            prompt: prompt.into(),
            placeholder: placeholder.into(),
            prefill,
        });
        self
    }

    /// Seed the argument field with `prefill` (fully selected) when the
    /// argument step opens, so the first keystroke replaces it.
    pub fn with_argument_prefill(mut self, prefill: impl Into<String>) -> Self {
        let prefill = prefill.into();
        match &mut self.argument {
            Some(spec) => spec.prefill = Some(prefill),
            None => {
                self.argument = Some(ArgumentSpec {
                    prefill: Some(prefill),
                    ..Default::default()
                })
            }
        }
        self
    }

    /// Attach a callback invoked when Enter is pressed on this entry.
    pub fn on_select<F>(mut self, f: F) -> Self
    where
        F: Fn(&mut AutoCommit, &ObjId) + Send + Sync + 'static,
    {
        self.on_select = Some(Arc::new(f));
        self
    }

    /// Build a palette row for one resource-scoped action exported by an app
    /// (see [`qos_actions`]). The row `id` is the flattened composite key so the
    /// host resolves the selection back to a replayable `qos_actions::Action`;
    /// the label reads `"<resource> — <action>"`. A [`ActionInput::Prompted`]
    /// action becomes an argument-collecting row via [`with_argument`].
    ///
    /// This is the seam that scopes Ctrl+K to the focused app instance's open
    /// resources — `ui::action_palette` builds the parallel id→action index.
    ///
    /// [`with_argument`]: CommandEntry::with_argument
    pub fn from_flat_action(fa: &FlatAction<'_>) -> Self {
        let mut entry = CommandEntry::new(
            fa.key.clone(),
            format!("{} — {}", fa.resource_label, fa.action.label),
        );
        if let Some(desc) = &fa.action.description {
            entry = entry.description(desc.clone());
        }
        if let ActionInput::Prompted {
            prompt, placeholder, ..
        } = &fa.action.input
        {
            entry = entry.with_argument(prompt.clone(), placeholder.clone());
        }
        entry
    }

    /// Does the entry match `query` as a case-insensitive subsequence over its
    /// label *or* description? Empty query always matches.
    fn matches(&self, query: &str) -> bool {
        if fuzzy_score(query, &self.label).is_some() {
            return true;
        }
        self.description
            .as_deref()
            .map(|d| fuzzy_score(query, d).is_some())
            .unwrap_or(false)
    }
}

/// Shorten `text` by eating its middle, keeping both ends readable.
///
/// The trailing-ellipsis case is [`TextRenderer::elide_to_width`]; this
/// is the variant paths need, where the head (the root the user
/// recognises) and the tail (the file's own folder) each carry more
/// information than the middle does.
fn elide_middle(tr: &mut TextRenderer, text: &str, size: f32, max_w: f64) -> String {
    if max_w <= 0.0 || tr.measure(text, size, None).0 as f64 <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    // Drop one character at a time from the middle, biased to keep the
    // tail, until it fits.
    for drop in 1..n {
        let keep = n - drop;
        let tail = (keep * 2) / 3;
        let head = keep - tail;
        let candidate: String = chars[..head].iter().collect::<String>()
            + "\u{2026}"
            + &chars[n - tail..].iter().collect::<String>();
        if tr.measure(&candidate, size, None).0 as f64 <= max_w {
            return candidate;
        }
    }
    "\u{2026}".to_string()
}

/// Subsequence fuzzy score: `Some(match_count - gap_penalty)` when `query`
/// matches `text` as a (case-insensitive) subsequence; `None` otherwise.
/// Empty queries match everything with score `0`.
fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() { return Some(0); }
    let q: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();
    let t: Vec<char> = text.chars().flat_map(|c| c.to_lowercase()).collect();
    let mut qi = 0usize;
    let mut matches = 0i32;
    let mut gaps = 0i32;
    let mut prev_match: Option<usize> = None;
    for (ti, ch) in t.iter().enumerate() {
        if qi < q.len() && *ch == q[qi] {
            if let Some(pm) = prev_match { gaps += (ti - pm - 1) as i32; }
            prev_match = Some(ti);
            qi += 1;
            matches += 1;
            if qi == q.len() { break; }
        }
    }
    if qi < q.len() { None } else { Some(matches - gaps) }
}

/// Geometry split of the prompt row into its three concept-spec slots.
///
/// Concept (`nirios-concept/src/components/GlobalSearch.tsx:138-153`)
/// places a leading lucide `Search` glyph + input + trailing `⌘K` kbd
/// chip inside the same row; this struct exposes the split so both
/// `draw` and the concept-parity tests share the same layout source of
/// truth.
#[derive(Clone, Debug, PartialEq)]
pub struct InputRowLayout {
    pub icon_rect: Rect,
    pub input_rect: Rect,
}

/// Live state while the palette is collecting a free-text argument for a
/// chosen entry (see [`ArgumentSpec`]). While `Some`, keyboard input edits
/// `buffer` instead of the query, and the result list is frozen.
#[derive(Clone, Debug)]
struct ArgState {
    entry_id: String,
    prompt: String,
    placeholder: String,
    buffer: String,
    selection: crate::text_selection::Selection,
}

pub struct CommandPalette {
    entries: Vec<CommandEntry>,
    query: String,
    /// Text-cursor + selection inside the query string. Lets the
    /// palette participate in the same keyboard model `TextInput` /
    /// `NotesApp` use: arrow keys move the caret, shift+arrow
    /// extends, Ctrl+A/C/X/V copy / cut / paste / select-all via
    /// the shared `ui::text_selection` clipboard helpers. Defaults
    /// to a collapsed selection at position 0 (matches the new()
    /// "" query).
    query_selection: crate::text_selection::Selection,
    filter: Option<Box<dyn Fn(&CommandEntry) -> bool + Send + Sync>>,
    visible: Vec<usize>,
    cursor: usize,
    rect: Rect,
    is_focused: bool,
    search_filter: SearchFilter,
    /// Chrome style (search chips vs command descriptions). Default [`PaletteMode::Search`].
    mode: PaletteMode,
    /// Whether the palette traps host keyboard focus while open (see
    /// [`Widget::traps_focus`]). Default `false` so the shell's search overlay
    /// is unaffected; [`PaletteMode::Commands`] turns it on.
    modal: bool,
    /// `Some` while collecting a free-text argument for the chosen entry.
    arg_state: Option<ArgState>,
    /// The chosen action, set on Enter and drained by [`take_outcome`].
    outcome: Option<CommandOutcome>,
    /// Skip the built-in fuzzy filter when `true`. Provider hosts set
    /// this so their pre-matched result set shows verbatim.
    bypass_fuzzy: bool,
    /// Suppress the palette's built-in filter strip (scope chips,
    /// filetype chips, full-text toggle). Provider hosts that render
    /// their own kind-chip strip outside the palette set this so the
    /// built-in strip isn't duplicated. Also collapses the strip's
    /// reserved height so the palette rows draw immediately below the
    /// input row.
    hide_filter_strip: bool,
    /// The host paints the filter band itself. The palette keeps the
    /// band reserved in its own layout (so the host's chips land inside
    /// the card, on the card's content inset) but draws nothing there.
    ///
    /// This is what replaced the shell's free-floating "Search in:"
    /// banner above the card: the banner had its own inset arithmetic,
    /// so its chips never lined up with the field below them, and the
    /// two surfaces had to be stitched together with a third unified
    /// background rect.
    host_filter_strip: bool,
    /// How many chip rows the host's band is reserving.
    ///
    /// The host knows how many controls it has and how wide they measure;
    /// the palette knows how tall the card is. A band that always
    /// reserved one row made "does not fit" the host's problem to solve
    /// silently — and the way it solved it was to stop painting, which
    /// is how a filter chip became a control with no rect and therefore
    /// no way to click it.
    host_filter_strip_rows: usize,
    /// Visual row plan (headers + entries) rebuilt whenever the entry
    /// set, query or filter changes. Every geometry accessor reads this
    /// so hit-testing, scrolling and painting cannot disagree.
    rows: Vec<VisualRow>,
}

/// One painted line in the result list: either a section header or an
/// entry. Headers are not selectable and are skipped by the cursor.
#[derive(Clone, Debug, PartialEq)]
enum VisualRow {
    /// Section label plus the number of rows the section stands for
    /// (which may exceed the number painted when the host capped it).
    Header { label: String, total: usize },
    /// Index into `self.visible` — i.e. a cursor position.
    Entry(usize),
}

impl Default for CommandPalette {
    fn default() -> Self { Self::new() }
}

impl CommandPalette {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            query: String::new(),
            query_selection: crate::text_selection::Selection::collapsed(0),
            filter: None,
            visible: Vec::new(),
            cursor: 0,
            rect: Rect::ZERO,
            is_focused: false,
            search_filter: SearchFilter::default(),
            mode: PaletteMode::Search,
            modal: false,
            arg_state: None,
            outcome: None,
            bypass_fuzzy: false,
            hide_filter_strip: false,
            host_filter_strip: false,
            host_filter_strip_rows: 1,
            rows: Vec::new(),
        }
    }

    /// Build a command-launcher palette: [`PaletteMode::Commands`] chrome
    /// (descriptions + keybinding hints, no scope/filetype chips) and focus
    /// trapping enabled. This is the reusable Ctrl+K command palette.
    pub fn commands() -> Self {
        let mut p = Self::new();
        p.mode = PaletteMode::Commands;
        p.modal = true;
        p
    }

    /// Current chrome mode.
    pub fn mode(&self) -> PaletteMode {
        self.mode
    }

    /// Switch chrome mode. `Commands` also enables focus trapping.
    pub fn set_mode(&mut self, mode: PaletteMode) {
        self.mode = mode;
        if mode == PaletteMode::Commands {
            self.modal = true;
        }
    }

    /// Override whether the palette traps host keyboard focus while open.
    pub fn set_modal(&mut self, modal: bool) {
        self.modal = modal;
    }

    /// Remove every registered entry (and any filtered view of them). Apps that
    /// rebuild their action set each time they open the palette call this first.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.visible.clear();
        self.cursor = 0;
    }

    /// Open the palette: clear the query + any in-progress argument, focus it,
    /// and reset the highlight to the first entry. Registers already-pushed
    /// entries in the visible list.
    pub fn open(&mut self) {
        self.is_focused = true;
        self.query.clear();
        self.query_selection = crate::text_selection::Selection::collapsed(0);
        self.arg_state = None;
        self.outcome = None;
        self.cursor = 0;
        self.rebuild();
    }

    /// Close the palette and discard any in-progress argument entry. Does not
    /// clear the registered entries (so the next `open` reuses them).
    pub fn close(&mut self) {
        self.is_focused = false;
        self.arg_state = None;
    }

    /// Whether the palette is currently open (focused).
    pub fn is_open(&self) -> bool {
        self.is_focused
    }

    /// Whether the palette is currently collecting a free-text argument.
    pub fn in_argument_mode(&self) -> bool {
        self.arg_state.is_some()
    }

    /// The text currently typed into the argument field, if in argument mode.
    pub fn argument_buffer(&self) -> Option<&str> {
        self.arg_state.as_ref().map(|a| a.buffer.as_str())
    }

    /// Drain the chosen action, if the user pressed Enter since the last drain.
    /// Rising-edge: returns `Some` at most once per selection.
    pub fn take_outcome(&mut self) -> Option<CommandOutcome> {
        self.outcome.take()
    }

    /// Place the palette card directly (bypassing taffy), for apps that
    /// hand-position their overlays. Pair with [`desired_size`] to centre it.
    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// The card's natural `(width, height)` for the current mode and visible
    /// row count. Apps use this to centre the palette via [`set_rect`]; it
    /// matches the size [`build_layout`] requests from taffy.
    pub fn desired_size(&self) -> (f64, f64) {
        // Size to the plan, not to a row count: headers and entries have
        // different heights, so `rows * row_height` sized the card wrong
        // the moment grouping landed. Clamped to the list budget so a
        // 40-hit query doesn't grow a card taller than the screen.
        let list = if self.rows.is_empty() {
            // The zero-state is a composed block, not a row — sized to
            // the row height it would print the empty-state text on top
            // of its own footer.
            EMPTY_STATE_HEIGHT
        } else {
            self.rows_height().clamp(self.row_height(), self.max_list_height())
        };
        let height =
            PADDING * 2.0 + INPUT_HEIGHT + PADDING + self.filter_block() + list + PADDING + FOOTER_HEIGHT;
        (self.card_width(), height)
    }

    /// How many results the list stands for, counting what a host
    /// capped away. Falls back to the painted row count when no host
    /// declared group totals.
    ///
    /// The footer would otherwise contradict the section headers it
    /// sits under — "7 results" beneath a section reading "40".
    pub fn result_total(&self) -> usize {
        if self.rows.iter().any(|r| matches!(r, VisualRow::Header { .. })) {
            self.rows
                .iter()
                .map(|r| match r {
                    VisualRow::Header { total, .. } => *total,
                    VisualRow::Entry(_) => 0,
                })
                .sum()
        } else {
            self.visible.len()
        }
    }

    /// Total height of every planned row, headers included.
    fn rows_height(&self) -> f64 {
        self.rows.iter().map(|r| self.visual_row_height(r)).sum()
    }

    /// Painted height of one planned row.
    fn visual_row_height(&self, row: &VisualRow) -> f64 {
        match row {
            VisualRow::Header { .. } => HEADER_HEIGHT,
            VisualRow::Entry(_) => self.row_height(),
        }
    }

    /// Ceiling on the result list, so the card stays a card. Expressed
    /// in rows rather than pixels so the cap tracks the row height, plus
    /// room for the headers the plan actually has — reserving header
    /// space unconditionally would let an ungrouped list paint a ninth
    /// row into slack meant for something that isn't there.
    fn max_list_height(&self) -> f64 {
        let headers = self
            .rows
            .iter()
            .filter(|r| matches!(r, VisualRow::Header { .. }))
            .count()
            .min(MAX_HEADERS_IN_VIEW);
        self.max_visible_rows() as f64 * self.row_height() + headers as f64 * HEADER_HEIGHT
    }

    /// Height reserved for the filter strip + its trailing gap (Search only).
    fn filter_block(&self) -> f64 {
        if self.hide_filter_strip {
            return 0.0;
        }
        // A host-painted strip still occupies the band — that is the
        // whole point of reserving it here rather than letting the host
        // invent its own rect outside the card.
        if self.host_filter_strip {
            return self.filter_strip_height() + PADDING;
        }
        match self.mode {
            PaletteMode::Search => FILTER_STRIP_HEIGHT + PADDING,
            PaletteMode::Commands => 0.0,
        }
    }

    /// Per-result-row height, uniform across the list.
    ///
    /// Two heights only — one line or two — chosen once for the whole
    /// list rather than per row, because a list whose rows change height
    /// from one entry to the next reads as ragged even when every row is
    /// individually well-proportioned.
    fn row_height(&self) -> f64 {
        let any_subtitle = self.visible.iter().any(|&i| {
            self.entries
                .get(i)
                .map(|e| e.description.is_some())
                .unwrap_or(false)
        });
        match self.mode {
            PaletteMode::Search if !any_subtitle => ROW_HEIGHT,
            _ => TWO_LINE_ROW_HEIGHT,
        }
    }

    /// Card width for the current mode.
    fn card_width(&self) -> f64 {
        match self.mode {
            PaletteMode::Search => SEARCH_CARD_WIDTH,
            PaletteMode::Commands => CMD_CARD_WIDTH,
        }
    }

    /// Max entry rows the card grows to before the list scrolls.
    /// Section headers get their own allowance on top (see
    /// [`Self::max_list_height`]).
    fn max_visible_rows(&self) -> usize {
        8
    }

    /// Activate whatever row is currently highlighted, exactly as pressing
    /// Enter would: an entry that takes an argument opens the argument step
    /// (seeded + pre-selected); any other entry runs immediately, emitting its
    /// [`CommandOutcome`]. Hosts wire pointer clicks to this so a click and
    /// Enter share one path. No-op while already collecting an argument.
    pub fn activate_selected(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if self.arg_state.is_some() {
            return;
        }
        let Some(&idx) = self.visible.get(self.cursor) else {
            return;
        };
        if let Some(spec) = self.entries[idx].argument.clone() {
            let buffer = spec.prefill.clone().unwrap_or_default();
            let selection = crate::text_selection::Selection {
                anchor: 0,
                cursor: buffer.len(),
            };
            self.arg_state = Some(ArgState {
                entry_id: self.entries[idx].id.clone(),
                prompt: spec.prompt,
                placeholder: spec.placeholder,
                buffer,
                selection,
            });
        } else {
            let id = self.entries[idx].id.clone();
            self.run_entry(state, path, id, None);
        }
    }

    /// Finalise the chosen entry: fire its legacy `on_select` callback (if
    /// any), record the [`CommandOutcome`] for `take_outcome`, and (in
    /// Commands mode) close the palette. Search mode keeps the palette open,
    /// matching the shell's existing behaviour.
    fn run_entry(
        &mut self,
        state: &mut AutoCommit,
        path: &ObjId,
        id: String,
        argument: Option<String>,
    ) {
        if let Some(cb) = self
            .entries
            .iter()
            .find(|e| e.id == id)
            .and_then(|e| e.on_select.clone())
        {
            cb(state, path);
        }
        self.outcome = Some(CommandOutcome { id, argument });
        self.arg_state = None;
        if self.mode == PaletteMode::Commands {
            self.is_focused = false;
        }
    }

    /// Keyboard handling while the argument step is open. Enter finalises the
    /// entry with the typed text, Escape returns to the result list, and every
    /// other key edits the argument buffer via the shared text-selection
    /// helpers. All keys are consumed so nothing leaks to the app behind.
    fn handle_arg_key(
        &mut self,
        key: &winit::keyboard::Key,
        shift: bool,
        ctrl: bool,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) {
        use crate::text_selection::{
            clipboard_copy, clipboard_paste, next_char_boundary, next_word_boundary,
            prev_char_boundary, prev_word_boundary, Selection,
        };
        // Everything past this point consumes the key.
        *handled = true;
        match key {
            Key::Named(NamedKey::Enter) => {
                let (id, value) = {
                    let arg = self.arg_state.as_ref().unwrap();
                    (arg.entry_id.clone(), arg.buffer.clone())
                };
                self.run_entry(state, path, id, Some(value));
                return;
            }
            Key::Named(NamedKey::Escape) => {
                // Cancel the argument step, fall back to the result list.
                self.arg_state = None;
                return;
            }
            // From here on we mutate the buffer in place.
            Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::ArrowDown) => {
                // Swallowed — no list navigation while typing an argument.
                return;
            }
            _ => {}
        }
        let arg = self.arg_state.as_mut().unwrap();
        match key {
            Key::Named(NamedKey::ArrowLeft) => {
                let pos = arg.selection.cursor;
                arg.selection.cursor = if ctrl {
                    prev_word_boundary(&arg.buffer, pos)
                } else {
                    prev_char_boundary(&arg.buffer, pos)
                };
                if !shift {
                    arg.selection.collapse_to_cursor();
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                let pos = arg.selection.cursor;
                arg.selection.cursor = if ctrl {
                    next_word_boundary(&arg.buffer, pos)
                } else {
                    next_char_boundary(&arg.buffer, pos)
                };
                if !shift {
                    arg.selection.collapse_to_cursor();
                }
            }
            Key::Named(NamedKey::Home) => {
                arg.selection.cursor = 0;
                if !shift {
                    arg.selection.collapse_to_cursor();
                }
            }
            Key::Named(NamedKey::End) => {
                arg.selection.cursor = arg.buffer.len();
                if !shift {
                    arg.selection.collapse_to_cursor();
                }
            }
            Key::Named(NamedKey::Backspace) => {
                let r = arg.selection.range();
                if !r.is_empty() {
                    arg.buffer.replace_range(r.clone(), "");
                    arg.selection = Selection::collapsed(r.start);
                } else if r.start > 0 {
                    let prev = prev_char_boundary(&arg.buffer, r.start);
                    arg.buffer.replace_range(prev..r.start, "");
                    arg.selection = Selection::collapsed(prev);
                }
            }
            Key::Named(NamedKey::Delete) => {
                let r = arg.selection.range();
                if !r.is_empty() {
                    arg.buffer.replace_range(r.clone(), "");
                    arg.selection = Selection::collapsed(r.start);
                } else if r.start < arg.buffer.len() {
                    let next = next_char_boundary(&arg.buffer, r.start);
                    arg.buffer.replace_range(r.start..next, "");
                }
            }
            Key::Named(NamedKey::Space) => {
                let r = arg.selection.range();
                arg.buffer.replace_range(r.clone(), " ");
                arg.selection = Selection::collapsed(r.start + 1);
            }
            Key::Character(s) if ctrl => match s.as_str() {
                "a" | "A" => {
                    arg.selection.anchor = 0;
                    arg.selection.cursor = arg.buffer.len();
                }
                "c" | "C" => {
                    let r = arg.selection.range();
                    if !r.is_empty() {
                        clipboard_copy(&arg.buffer[r]);
                    }
                }
                "x" | "X" => {
                    let r = arg.selection.range();
                    if !r.is_empty() {
                        clipboard_copy(&arg.buffer[r.clone()]);
                        arg.buffer.replace_range(r.clone(), "");
                        arg.selection = Selection::collapsed(r.start);
                    }
                }
                "v" | "V" => {
                    if let Some(paste) = clipboard_paste() {
                        let r = arg.selection.range();
                        arg.buffer.replace_range(r.clone(), &paste);
                        arg.selection = Selection::collapsed(r.start + paste.len());
                    }
                }
                _ => {}
            },
            Key::Character(s) => {
                let filtered: String = s.chars().filter(|c| !c.is_control()).collect();
                if !filtered.is_empty() {
                    let r = arg.selection.range();
                    arg.buffer.replace_range(r.clone(), &filtered);
                    arg.selection = Selection::collapsed(r.start + filtered.len());
                }
            }
            _ => {}
        }
    }

    /// Mutable handle to the filter strip's state — callers wire chip
    /// clicks to `search_filter_mut().toggle_*()`.
    pub fn search_filter(&self) -> &SearchFilter { &self.search_filter }
    pub fn search_filter_mut(&mut self) -> &mut SearchFilter {
        &mut self.search_filter
    }

    pub fn push(&mut self, e: CommandEntry) {
        self.entries.push(e);
        self.rebuild();
    }

    /// Replace the whole entry list. Used by search-provider hosts
    /// (`shell::overlay_host`) that generate a fresh result set on
    /// every keystroke. Resets the cursor to 0.
    pub fn set_entries(&mut self, entries: Vec<CommandEntry>) {
        self.entries = entries;
        self.cursor = 0;
        self.rebuild();
    }

    /// When set to `true`, `rebuild` skips the palette's built-in
    /// fuzzy filter — every entry passes through visibility unless the
    /// custom [`Self::search_filter`] rejects it. Turn on when a
    /// provider host is doing its own matching outside the palette
    /// (so a low-signal fuzzy prefix doesn't hide provider hits whose
    /// labels don't literally spell the query).
    pub fn set_bypass_fuzzy(&mut self, on: bool) {
        self.bypass_fuzzy = on;
        self.rebuild();
    }

    /// Hide (and collapse) the palette's built-in scope/filetype/full-
    /// text filter strip. The `shell::overlay_host` chip strip renders
    /// a dynamic version above the palette; without this, both strips
    /// paint and the palette looks doubled up.
    pub fn set_hide_filter_strip(&mut self, on: bool) {
        self.hide_filter_strip = on;
    }

    /// Keep the filter band reserved inside the card but paint nothing
    /// into it — the host draws its own controls there via
    /// [`Self::filter_strip_rect`]. See [`Self::host_filter_strip`].
    pub fn set_host_filter_strip(&mut self, on: bool) {
        self.host_filter_strip = on;
        if on {
            self.hide_filter_strip = false;
        }
    }

    /// Reserve `rows` rows of filter band for the host's own controls.
    ///
    /// The card grows by exactly the extra rows, and the result list
    /// starts below them — which is the point. A host with more controls
    /// than fit across one row otherwise has only bad options: paint them
    /// over the results, or stop painting and leave a control the user
    /// cannot click.
    ///
    /// Returns whether the reservation changed, so a caller can flag its
    /// layout dirty only when it has to.
    pub fn set_host_filter_strip_rows(&mut self, rows: usize) -> bool {
        let rows = rows.max(1);
        let changed = self.host_filter_strip_rows != rows;
        self.host_filter_strip_rows = rows;
        changed
    }

    /// Left/right inset every band of the card shares — input field,
    /// filter strip, rows and footer all start here. Hosts painting
    /// into [`Self::filter_strip_rect`] read this instead of inventing
    /// their own padding, which is what let the old banner's chips sit
    /// misaligned with the field under them.
    pub fn content_inset_px(&self) -> f64 {
        PADDING
    }

    pub fn set_query(&mut self, q: &str) {
        self.query = q.into();
        // Pin caret to end on programmatic set — preserves the legacy
        // semantics callers expect (set_query("") + new chars typed
        // appears at end).
        let end = self.query.len();
        self.query_selection = crate::text_selection::Selection::collapsed(end);
        self.rebuild();
    }

    /// Replace the selected range in the query (or insert at the
    /// caret when the selection is collapsed) with `s`. Used by the
    /// keyboard handler for character input and Ctrl+V paste.
    fn replace_query_selection(&mut self, s: &str) {
        let r = self.query_selection.range();
        self.query.replace_range(r.clone(), s);
        let new_caret = r.start + s.len();
        self.query_selection =
            crate::text_selection::Selection::collapsed(new_caret);
        self.rebuild();
    }

    /// Delete the selection if non-empty, returns `true` if a range
    /// was actually removed (so the caller can skip the per-char
    /// fallback path for Backspace / Delete).
    fn delete_query_selection(&mut self) -> bool {
        if self.query_selection.is_collapsed() {
            return false;
        }
        let r = self.query_selection.range();
        self.query.replace_range(r.clone(), "");
        self.query_selection =
            crate::text_selection::Selection::collapsed(r.start);
        self.rebuild();
        true
    }

    pub fn query(&self) -> &str { &self.query }

    pub fn set_filter(&mut self, f: Box<dyn Fn(&CommandEntry) -> bool + Send + Sync>) {
        self.filter = Some(f);
        self.rebuild();
    }

    pub fn visible_entries(&self) -> Vec<&CommandEntry> {
        self.visible.iter().map(|&i| &self.entries[i]).collect()
    }

    pub fn cursor(&self) -> usize { self.cursor }

    /// Move the cursor to `index` within the visible window. Silently
    /// clamps to the last visible row so overlay callers that do hit-
    /// testing on the post-layout rects don't have to bounds-check.
    pub fn set_cursor(&mut self, index: usize) {
        if self.visible.is_empty() {
            self.cursor = 0;
        } else {
            self.cursor = index.min(self.visible.len() - 1);
        }
    }

    /// Id of the currently-selected visible entry, or `None` if the
    /// filter matched no rows.
    pub fn selected_id(&self) -> Option<&str> {
        self.visible
            .get(self.cursor)
            .map(|&i| self.entries[i].id.as_str())
    }

    /// Position of `visible[entry_i]` in the visual row plan.
    fn visual_index_of(&self, entry_i: usize) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| matches!(r, VisualRow::Entry(e) if *e == entry_i))
    }

    /// The half-open range of visual rows currently painted, scrolled
    /// just far enough to keep the cursor's row inside the card.
    ///
    /// Computed from the plan rather than from a row count: with section
    /// headers interleaved, "ten rows" is not a fixed number of pixels,
    /// and the old count-based window is what let `draw` and `row_at`
    /// disagree about which entry sat under the pointer.
    fn painted_window(&self) -> (usize, usize) {
        let budget = (self.footer_rect().y0 - PADDING - self.list_top_y()).max(0.0);
        let target = self
            .visual_index_of(self.cursor)
            .unwrap_or(0);
        let mut start = 0usize;
        loop {
            let mut used = 0.0;
            let mut end = start;
            while end < self.rows.len() {
                let h = self.visual_row_height(&self.rows[end]);
                if used + h > budget {
                    break;
                }
                used += h;
                end += 1;
            }
            if target < end || end >= self.rows.len() || start + 1 >= self.rows.len() {
                // Never leave a section's rows stranded under a header
                // that just scrolled away — pull the header back in.
                if start > 0 && matches!(self.rows[start - 1], VisualRow::Header { .. }) {
                    start -= 1;
                }
                return (start, end.max(start));
            }
            start += 1;
        }
    }

    /// Rect of the `i`th *visible* entry, in the same coordinate space as
    /// `get_rect()`. Returns `None` when the row is scrolled out of the
    /// currently painted window.
    pub fn row_rect(&self, i: usize) -> Option<Rect> {
        if i >= self.visible.len() {
            return None;
        }
        let target = self.visual_index_of(i)?;
        let (start, end) = self.painted_window();
        if target < start || target >= end {
            return None;
        }
        let mut y0 = self.list_top_y();
        for r in &self.rows[start..target] {
            y0 += self.visual_row_height(r);
        }
        let y1 = y0 + self.row_height();
        Some(Rect::new(
            self.rect.x0 + PADDING,
            y0,
            self.rect.x1 - PADDING,
            y1,
        ))
    }

    /// Y coordinate where the result list begins (immediately under the
    /// filter strip + its gap in Search mode, or straight under the prompt row
    /// in Commands mode). Shared by `row_rect` and `draw`.
    fn list_top_y(&self) -> f64 {
        self.rect.y0 + PADDING + INPUT_HEIGHT + PADDING + self.filter_block()
    }

    /// Index of the visible row whose rect contains `p`, if any. Used by
    /// overlay callers that accept a click inside the palette rect and
    /// want to move the cursor to the clicked row.
    pub fn row_at(&self, p: Point) -> Option<usize> {
        (0..self.visible.len()).find(|&i| {
            self.row_rect(i).map(|r| r.contains(p)).unwrap_or(false)
        })
    }

    /// Concept (`nirios-concept/src/components/GlobalSearch.tsx:120-150`)
    /// wraps the palette card in `rounded-3xl` (Tailwind = 24 px). Pin
    /// here so the radius is one source of truth shared by `draw` and
    /// the concept-parity tests.
    pub fn surface_radius_px(&self) -> f64 {
        24.0
    }

    /// Splits the input row into two sub-rects: a leading 24 px icon slot
    /// (lucide `Search`) and the input field, which runs from there to the
    /// row's trailing inset.
    ///
    /// The row used to reserve a trailing 56 px slot for a `Ctrl+K` kbd
    /// pill. That chip is gone (see `draw`), so the width it held now goes
    /// to the query — a long search string elides ~66 px later than it did.
    pub fn compute_input_layout(&self, row: Rect) -> InputRowLayout {
        const ICON_W: f64 = 24.0;
        const EDGE: f64 = 12.0;
        const GAP: f64 = 10.0;
        let cy = (row.y0 + row.y1) / 2.0;
        let icon_y0 = cy - ICON_W / 2.0;
        let icon_rect = Rect::new(
            row.x0 + EDGE,
            icon_y0,
            row.x0 + EDGE + ICON_W,
            icon_y0 + ICON_W,
        );
        // The text slot runs from the glyph to the trailing inset, *inside*
        // the same field — the icon is an ornament on one control, not a
        // second control flanking a boxed input.
        let input_rect = Rect::new(icon_rect.x1 + GAP, row.y0, row.x1 - EDGE, row.y1);
        InputRowLayout {
            icon_rect,
            input_rect,
        }
    }

    /// Rect of the prompt row inside the post-layout palette rect.
    /// Used by [`compute_input_layout`] callers that don't want to
    /// reconstruct the inset arithmetic at every call site.
    pub fn input_row_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PADDING,
            self.rect.y0 + PADDING,
            self.rect.x1 - PADDING,
            self.rect.y0 + PADDING + INPUT_HEIGHT,
        )
    }

    /// Filter band painted directly under the prompt row. Holds scope
    /// chips on the left, filetype chips in the middle, and a full-text
    /// toggle on the right (concept `GlobalSearch.tsx:184-244`).
    ///
    /// One [`FILTER_STRIP_HEIGHT`] row tall, unless a host painting its
    /// own controls asked for more ([`Self::set_host_filter_strip_rows`]).
    pub fn filter_strip_rect(&self) -> Rect {
        let row = self.input_row_rect();
        let y0 = row.y1 + PADDING;
        Rect::new(row.x0, y0, row.x1, y0 + self.filter_strip_height())
    }

    /// Height of one row of the filter band. A host laying controls out
    /// across several rows reads this rather than re-deriving it, so the
    /// rows it paints line up with the band the card reserved.
    pub fn filter_strip_row_height(&self) -> f64 {
        FILTER_STRIP_HEIGHT
    }

    /// Rows the band is currently reserving.
    pub fn host_filter_strip_rows(&self) -> usize {
        if self.host_filter_strip {
            self.host_filter_strip_rows.max(1)
        } else {
            1
        }
    }

    fn filter_strip_height(&self) -> f64 {
        FILTER_STRIP_HEIGHT * self.host_filter_strip_rows() as f64
    }

    /// Footer band painted at the bottom of the palette card with
    /// `↑↓ Navigate / Enter Select` kbd-chip hints + a right-aligned
    /// scope label. Concept `GlobalSearch.tsx:274-280`.
    pub fn footer_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PADDING,
            self.rect.y1 - PADDING - FOOTER_HEIGHT,
            self.rect.x1 - PADDING,
            self.rect.y1 - PADDING,
        )
    }

    /// Layout split for one filter chip. Each chip is a rounded-full
    /// pill `px-2 py-1` (≈ 12 px horizontal padding) centred vertically
    /// in the strip.
    fn chip_rect(strip: Rect, x: f64, width: f64) -> Rect {
        let h = (strip.height() - 8.0).max(20.0);
        let cy = (strip.y0 + strip.y1) / 2.0;
        Rect::new(x, cy - h / 2.0, x + width, cy + h / 2.0)
    }

    /// Paint scope chips + filetype chips + full-text toggle into the
    /// filter strip. Active chip = `accent_primary` (indigo-500) fill +
    /// `text_primary` (white) text; idle = white/5 fill + `text_dim`
    /// (slate-400) text. The full-text toggle uses `accent[2]` (≈
    /// emerald-500) when on, matching concept `bg-emerald-500/20`.
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
        let idle_fill = Color::from_rgba8(255, 255, 255, 13);  // white/5
        let active_text = theme.tokens.colors.text_primary;
        let idle_text = theme.tokens.colors.text_dim;

        // Scope chips on the left.
        let mut x = strip.x0;
        for (scope, label) in [
            (SearchScope::Workspace, "WORKSPACE"),
            (SearchScope::All, "ALL"),
        ] {
            let (w, _) = text_renderer.measure(label, chip_text_size, None);
            let chip_w = w as f64 + 16.0;
            let chip = Self::chip_rect(strip, x, chip_w);
            let active = self.search_filter.scope == scope;
            self.draw_chip(
                scene,
                transform,
                text_renderer,
                chip,
                label,
                chip_text_size,
                if active { active_fill } else { idle_fill },
                if active { active_text } else { idle_text },
            );
            x += chip_w + 6.0;
        }

        // Right-anchored full-text toggle.
        let ft_label = "FULL TEXT";
        let (ft_w, _) = text_renderer.measure(ft_label, chip_text_size, None);
        let ft_chip_w = ft_w as f64 + 16.0;
        let ft_chip = Self::chip_rect(strip, strip.x1 - ft_chip_w, ft_chip_w);
        let ft_on = self.search_filter.full_text;
        // Concept `GlobalSearch.tsx:227-231`: `bg-emerald-500/20
        // text-emerald-400` when on. Hex literals are pinned by the
        // plan's palette table.
        let ft_active_fill = Color::from_rgba8(34, 197, 94, 51);  // emerald-500/20
        let ft_active_text = Color::from_rgba8(0x4a, 0xde, 0x80, 0xff);  // emerald-400
        self.draw_chip(
            scene,
            transform,
            text_renderer,
            ft_chip,
            ft_label,
            chip_text_size,
            if ft_on { ft_active_fill } else { idle_fill },
            if ft_on { ft_active_text } else { idle_text },
        );

        // Filetype chips fill the gap between scope chips and the
        // full-text toggle. Right-bound stops 8 px before the toggle.
        let mut fx = x;
        let max_x = ft_chip.x0 - 8.0;
        for ft in Filetype::ALL {
            let label = ft.label();
            let (w, _) = text_renderer.measure(label, chip_text_size, None);
            let chip_w = w as f64 + 16.0;
            if fx + chip_w > max_x { break; }
            let chip = Self::chip_rect(strip, fx, chip_w);
            let active = self.search_filter.filetypes.contains(&ft);
            self.draw_chip(
                scene,
                transform,
                text_renderer,
                chip,
                label,
                chip_text_size,
                if active { active_fill } else { idle_fill },
                if active { active_text } else { idle_text },
            );
            fx += chip_w + 6.0;
        }
    }

    /// Single chip body + label. Shared by scope/filetype/full-text
    /// renders so they all pin to the same `rounded-full` shape.
    fn draw_chip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        chip: Rect,
        label: &str,
        text_size: f32,
        body: Color,
        text_color: Color,
    ) {
        // `rounded-full` ≈ half-height radius.
        let radius = chip.height() / 2.0;
        let shape = RoundedRect::from_rect(chip, radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            body,
            None,
            &shape,
        );
        let (w, h) = text_renderer.measure(label, text_size, None);
        let tx = (chip.x0 + chip.x1) / 2.0 - w as f64 / 2.0;
        let ty = (chip.y0 + chip.y1) / 2.0 - h as f64 / 2.0;
        text_renderer.draw_text(
            scene,
            label,
            text_size,
            text_color,
            transform * Affine::translate((tx, ty)),
            None,
        );
    }

    /// Slim hint band at the bottom of the card: keyboard affordances on
    /// the left, the result count on the right, separated from the list
    /// by a hairline rather than by a filled band.
    ///
    /// The right slot used to repeat the active scope, which the scope
    /// control two rows up already showed. A count is the thing the
    /// chips cannot tell you.
    fn draw_footer(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        let footer = self.footer_rect();
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard.with_alpha_factor(0.6),
            None,
            &vello::kurbo::Line::new(
                Point::new(footer.x0 + 4.0, footer.y0),
                Point::new(footer.x1 - 4.0, footer.y0),
            ),
        );

        let size = (theme.tokens.typography.scale.xs).max(9.0);
        let dim = colors.text_dim;
        let cy = (footer.y0 + footer.y1) / 2.0;
        let chip_h = (footer.height() - 12.0).max(13.0);

        let mut x = footer.x0 + 6.0;
        let hints: &[(&str, &str)] = match self.mode {
            PaletteMode::Search => &[
                ("\u{2191}\u{2193}", "Navigate"),
                ("\u{21b5}", "Open"),
                ("Esc", "Close"),
            ],
            PaletteMode::Commands => &[
                ("\u{2191}\u{2193}", "Navigate"),
                ("\u{21b5}", "Run"),
                ("Esc", "Close"),
            ],
        };
        for (key, label) in hints {
            let (kw, kh) = text_renderer.measure(key, size, None);
            let chip_w = (kw as f64 + 10.0).max(chip_h);
            let chip_rect = Rect::new(x, cy - chip_h / 2.0, x + chip_w, cy + chip_h / 2.0);
            let chip_shape = RoundedRect::from_rect(chip_rect, 4.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                colors.surface_white_5,
                None,
                &chip_shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                colors.border_white_10,
                None,
                &chip_shape,
            );
            text_renderer.draw_text(
                scene,
                key,
                size,
                dim,
                transform
                    * Affine::translate((
                        chip_rect.x0 + (chip_rect.width() - kw as f64) / 2.0,
                        chip_rect.y0 + (chip_rect.height() - kh as f64) / 2.0,
                    )),
                None,
            );
            x = chip_rect.x1 + 4.0;
            let (lw, lh) = text_renderer.measure(label, size, None);
            text_renderer.draw_text(
                scene,
                label,
                size,
                dim,
                transform * Affine::translate((x, cy - lh as f64 / 2.0)),
                None,
            );
            x += lw as f64 + 12.0;
        }

        let n = self.result_total();
        if n > 0 {
            let right = if n == 1 {
                "1 result".to_string()
            } else {
                format!("{n} results")
            };
            let (sw, sh) = text_renderer.measure(&right, size, None);
            text_renderer.draw_text(
                scene,
                &right,
                size,
                dim,
                transform
                    * Affine::translate((footer.x1 - sw as f64 - 6.0, cy - sh as f64 / 2.0)),
                None,
            );
        }
    }

    /// Paint the result list: a section header wherever the group
    /// changes, then that group's rows.
    ///
    /// One renderer for both modes. Search mode used to have its own
    /// copy that ignored the scroll window entirely, so pressing Down
    /// past the tenth hit moved a selection nobody could see while
    /// `row_at` reported a different row than the one under the cursor.
    fn draw_result_rows(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        let label_size = theme.tokens.typography.scale.sm;
        let desc_size = (theme.tokens.typography.scale.xs).max(10.0);
        let header_size = (theme.tokens.typography.scale.xs).max(9.0);
        let rh = self.row_height();
        let x0 = self.rect.x0 + PADDING;
        let x1 = self.rect.x1 - PADDING;
        let (start, end) = self.painted_window();
        let mut y = self.list_top_y();

        for (offset, row) in self.rows[start..end].iter().enumerate() {
            match row {
                VisualRow::Header { label, total } => {
                    // The rows this header actually has under it, which
                    // is what decides whether its count says anything
                    // the footer has not already said.
                    let shown_here = self.rows[start + offset + 1..]
                        .iter()
                        .take_while(|r| matches!(r, VisualRow::Entry(_)))
                        .count();
                    let hy = y + (HEADER_HEIGHT - header_size as f64) / 2.0 + 2.0;
                    let upper = label.to_uppercase();
                    text_renderer.draw_text(
                        scene,
                        &upper,
                        header_size,
                        colors.text_dim,
                        transform * Affine::translate((x0 + 12.0, hy)),
                        None,
                    );
                    // A capped section says how much it is holding back.
                    // Only a capped one: when the section is showing
                    // everything it has, this figure is the footer's
                    // "N results" over again, and it arrived as a bare
                    // right-aligned digit with nothing naming it — the
                    // same fact stated twice, once unreadably. So it is
                    // painted when it carries something the footer does
                    // not, and it says what it counts when it is.
                    if let Some(caption) = Self::header_caption(*total, shown_here) {
                        let (cw, _) = text_renderer.measure(&caption, header_size, None);
                        text_renderer.draw_text(
                            scene,
                            &caption,
                            header_size,
                            colors.text_dim,
                            transform * Affine::translate((x1 - 12.0 - cw as f64, hy)),
                            None,
                        );
                    }
                    y += HEADER_HEIGHT;
                }
                VisualRow::Entry(entry_i) => {
                    let row_rect = Rect::new(x0, y, x1, y + rh);
                    let selected = *entry_i == self.cursor;
                    let entry = &self.entries[self.visible[*entry_i]];
                    self.draw_one_row(
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        row_rect,
                        entry,
                        selected,
                        label_size,
                        desc_size,
                    );
                    y += rh;
                }
            }
        }
    }

    /// The opaque colour a row's own content is painted on.
    ///
    /// A selected row is not the card it sits in — it is the card plus
    /// an accent wash — and ink chosen against the card reads a tier
    /// lower there. Resolving it here means the paint and anything that
    /// checks the paint are answering from one place, rather than a
    /// check agreeing with a second copy of the arithmetic.
    pub(crate) fn row_ground(colors: &crate::tokens::Colors, selected: bool) -> Color {
        if selected {
            crate::tokens::colors::over(colors.surfaces.raised, colors.accent[5], SELECTION_WASH)
        } else {
            colors.surfaces.raised
        }
    }

    /// What a section header prints on its right, if anything.
    ///
    /// `None` whenever the section is showing everything it has: the
    /// footer already counts those rows, in words, a few pixels below.
    /// A second copy of the same number — right-aligned, unlabelled, at
    /// header size — is not a count, it is a stray digit.
    ///
    /// `Some` only when rows were held back, which the footer cannot
    /// say because it counts the whole result set rather than this
    /// section's share of it. Then it names both figures, so the number
    /// that appears is the one the user cannot otherwise get.
    pub(crate) fn header_caption(total: usize, shown: usize) -> Option<String> {
        (total > shown).then(|| format!("{shown} of {total}"))
    }

    /// One result row: selection body, leading kind glyph, title, and a
    /// dimmed subtitle or right-aligned keybinding.
    #[allow(clippy::too_many_arguments)]
    fn draw_one_row(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        entry: &CommandEntry,
        selected: bool,
        label_size: f32,
        desc_size: f32,
    ) {
        let colors = &theme.tokens.colors;
        // What this row's text and chips will actually be painted on.
        // A selected row is not the card it sits in — it is the card
        // plus an accent wash — and ink chosen against the card reads a
        // tier lower there. Resolve the composite once, and measure
        // everything on the row against it rather than against the
        // panel colour the palette started from.
        let row_ground = Self::row_ground(colors, selected);
        // The second line is the smallest text the palette draws, and on
        // a selected row it was the dim tier fighting an accent wash it
        // was never measured against. Ask for the tone the theme
        // authored and take the nearest legible step from it, so the
        // subtitle stays a subtitle rather than being promoted to the
        // title's weight to survive.
        let subtitle_ink =
            colors.ink_on(row_ground, colors.text_dim, ON_CARD_SMALL_TEXT_CONTRAST);
        if selected {
            // Selection has to survive a glance across a dense list. A
            // near-invisible grey wash did not: it read as a rendering
            // artefact rather than "Enter opens this". Accent body plus
            // an accent rule down the leading edge.
            let shape = RoundedRect::from_rect(row, theme.tokens.radii.md as f64);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                colors.accent[5].with_alpha_factor(SELECTION_WASH),
                None,
                &shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                colors.accent[5].with_alpha_factor(0.55),
                None,
                &shape,
            );
            let bar = RoundedRect::from_rect(
                Rect::new(row.x0 + 1.0, row.y0 + 6.0, row.x0 + 4.0, row.y1 - 6.0),
                1.5,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                colors.accent[5],
                None,
                &bar,
            );
        }

        // Leading glyph. The kind is a picture, not a `[Files]` literal
        // repeated down the left edge of every subtitle.
        let mut text_x = row.x0 + 12.0;
        if let Some(icon) = entry.icon {
            let size = 16.0;
            let cy = (row.y0 + row.y1) / 2.0;
            let icon_rect = Rect::new(
                row.x0 + 12.0,
                cy - size / 2.0,
                row.x0 + 12.0 + size,
                cy + size / 2.0,
            );
            let tint = if selected {
                colors.text_primary
            } else {
                colors.text_dim
            };
            icons::draw_icon(scene, transform, icon_rect, icon, tint, 1.5);
            text_x = icon_rect.x1 + 10.0;
        }

        // Reserve the trailing chip before measuring the text, so a long
        // title is ellipsised rather than painted under it.
        //
        // A badge and a keybinding share this slot. The badge wins when
        // both are set: a keybinding is a convenience the user can also
        // discover elsewhere, while a badge is a fact about what
        // activating the row will do, and losing that to a shortcut hint
        // would be the wrong trade.
        let mut text_right = row.x1 - 12.0;
        let chip_text = entry.badge.as_deref().or(entry.keybinding.as_deref());
        if let Some(chip_label) = chip_text {
            let is_badge = entry.badge.is_some();
            let (kw, kh) = text_renderer.measure(chip_label, desc_size, None);
            let chip_w = kw as f64 + 14.0;
            let chip_h = kh as f64 + 8.0;
            let cy = (row.y0 + row.y1) / 2.0;
            let chip = Rect::new(
                row.x1 - chip_w - 8.0,
                cy - chip_h / 2.0,
                row.x1 - 8.0,
                cy + chip_h / 2.0,
            );
            let chip_shape = RoundedRect::from_rect(chip, 5.0);
            // A badge is tinted with the accent, a keybinding is not.
            // The tint is what makes it findable in a list without
            // giving it a banner, and it stays quieter than the
            // selection, which is still the loudest thing on screen.
            //
            // What the badge does *not* do is take its label straight
            // from the accent token. Painted that way it was accent ink
            // on an accent wash on an accent selection — one hue three
            // times over, and the row's only statement that the app is
            // not on this machine was the least readable mark in the
            // frame. The chip is resolved against the ground it lands
            // on instead, at the floor that suits the size it is drawn
            // at, so the pill keeps the accent and the label stops
            // sharing it. Both grounds go through the same derivation,
            // so a selected row does not need a second set of values.
            let (fill, border, ink) = if is_badge {
                let chip = colors.accent_chip_on(row_ground, ON_CARD_SMALL_TEXT_CONTRAST);
                (chip.fill, chip.border, chip.ink)
            } else {
                // A keybinding chip's wash is faint enough that its body
                // is within a shade of the row, but it is still a body:
                // resolve it too rather than measuring the label against
                // the row it does not quite sit on.
                let kb_fill = crate::tokens::colors::over(
                    row_ground,
                    Color::WHITE,
                    KEYBINDING_CHIP_WASH,
                );
                (
                    colors.surface_white_5,
                    colors.border_white_10,
                    colors.ink_on(kb_fill, colors.text_dim, ON_CARD_SMALL_TEXT_CONTRAST),
                )
            };
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                fill,
                None,
                &chip_shape,
            );
            scene.stroke(&Stroke::new(1.0), transform, border, None, &chip_shape);
            text_renderer.draw_text(
                scene,
                chip_label,
                desc_size,
                ink,
                transform
                    * Affine::translate((chip.x0 + 7.0, chip.center().y - kh as f64 / 2.0)),
                None,
            );
            text_right = chip.x0 - 8.0;
        } else if entry.argument.is_some() {
            let cue = "\u{2192}"; // →
            let (hw, hh) = text_renderer.measure(cue, label_size, None);
            let color = if selected {
                colors.text_primary
            } else {
                colors.text_dim
            };
            text_renderer.draw_text(
                scene,
                cue,
                label_size,
                color,
                transform
                    * Affine::translate((
                        row.x1 - hw as f64 - 14.0,
                        (row.y0 + row.y1) / 2.0 - hh as f64 / 2.0,
                    )),
                None,
            );
            text_right = row.x1 - hw as f64 - 22.0;
        }
        let avail = (text_right - text_x).max(0.0);

        match &entry.description {
            Some(desc) => {
                let label = text_renderer.elide_to_width(&entry.label, label_size, avail as f32);
                // A path is most informative at its tail — the file's
                // own folder — so it loses its middle, not its end.
                let sub = elide_middle(text_renderer, desc, desc_size, avail);
                let block = label_size as f64 + 3.0 + desc_size as f64;
                let ly = row.y0 + (row.height() - block) / 2.0;
                text_renderer.draw_text(
                    scene,
                    &label,
                    label_size,
                    colors.text_primary,
                    transform * Affine::translate((text_x, ly)),
                    None,
                );
                text_renderer.draw_text(
                    scene,
                    &sub,
                    desc_size,
                    subtitle_ink,
                    transform
                        * Affine::translate((text_x, ly + label_size as f64 + 3.0)),
                    None,
                );
            }
            None => {
                let label = text_renderer.elide_to_width(&entry.label, label_size, avail as f32);
                let ly = row.y0 + (row.height() - label_size as f64) / 2.0;
                text_renderer.draw_text(
                    scene,
                    &label,
                    label_size,
                    colors.text_primary,
                    transform * Affine::translate((text_x, ly)),
                    None,
                );
            }
        }
    }

    /// Composed zero-state for a query that matched nothing. Painted in
    /// the list area so the card keeps its shape instead of collapsing
    /// to a bare input with a footer stuck to it.
    fn draw_empty_state(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        let top = self.list_top_y();
        let bottom = self.footer_rect().y0 - PADDING;
        if bottom <= top {
            return;
        }
        let cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let cy = (top + bottom) / 2.0;
        let icon_size = 24.0;
        icons::draw_icon(
            scene,
            transform,
            Rect::new(
                cx - icon_size / 2.0,
                cy - icon_size - 10.0,
                cx + icon_size / 2.0,
                cy - 10.0,
            ),
            Icon::Search,
            colors.text_dim.with_alpha_factor(0.55),
            1.5,
        );
        let title_size = theme.tokens.typography.scale.sm;
        let title = if self.query.is_empty() {
            "Nothing to show".to_string()
        } else {
            format!("No results for \u{201c}{}\u{201d}", self.query)
        };
        let title =
            text_renderer.elide_to_width(&title, title_size, (self.rect.width() - 48.0) as f32);
        let (tw, _) = text_renderer.measure(&title, title_size, None);
        text_renderer.draw_text(
            scene,
            &title,
            title_size,
            colors.text_primary,
            transform * Affine::translate((cx - tw as f64 / 2.0, cy)),
            None,
        );
        let hint_size = (theme.tokens.typography.scale.xs).max(10.0);
        let hint = "Try fewer words, or widen the scope";
        let (hw, _) = text_renderer.measure(hint, hint_size, None);
        text_renderer.draw_text(
            scene,
            hint,
            hint_size,
            colors.text_dim,
            transform
                * Affine::translate((cx - hw as f64 / 2.0, cy + title_size as f64 + 6.0)),
            None,
        );
    }

    fn rebuild(&mut self) {
        self.visible.clear();
        for (i, e) in self.entries.iter().enumerate() {
            if let Some(f) = &self.filter {
                if !f(e) { continue; }
            }
            if !self.bypass_fuzzy && !e.matches(&self.query) { continue; }
            self.visible.push(i);
        }
        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len().saturating_sub(1);
        }
        self.replan_rows();
    }

    /// Turn the visible entry list into the painted row plan: a header
    /// wherever the group changes, then that group's entries.
    fn replan_rows(&mut self) {
        self.rows.clear();
        let mut current: Option<&str> = None;
        for (entry_i, &idx) in self.visible.iter().enumerate() {
            let entry = &self.entries[idx];
            if let Some(group) = entry.group.as_deref() {
                if current != Some(group) {
                    current = Some(group);
                    // The host may have handed over fewer rows than the
                    // group really has; show what it says, and fall back
                    // to counting the contiguous run when it says nothing.
                    let total = entry.group_total.unwrap_or_else(|| {
                        self.visible[entry_i..]
                            .iter()
                            .take_while(|&&j| {
                                self.entries[j].group.as_deref() == Some(group)
                            })
                            .count()
                    });
                    self.rows.push(VisualRow::Header {
                        label: group.to_string(),
                        total,
                    });
                }
            }
            self.rows.push(VisualRow::Entry(entry_i));
        }
    }
}

/// Single-line result row.
const ROW_HEIGHT: f64 = 32.0;
/// Row carrying a label plus a dimmed subtitle underneath. Tightened
/// from 50 px: at 50 the two lines floated in the middle of a band far
/// taller than the text, which is what made a ten-hit list fill most of
/// the screen.
const TWO_LINE_ROW_HEIGHT: f64 = 42.0;
/// Section header row — the group name plus its count.
const HEADER_HEIGHT: f64 = 24.0;
/// How many section headers the card grows to accommodate before the
/// list starts scrolling instead.
const MAX_HEADERS_IN_VIEW: usize = 3;
/// Vertical room the composed zero-state needs: glyph, headline, hint.
const EMPTY_STATE_HEIGHT: f64 = 104.0;
const SEARCH_CARD_WIDTH: f64 = 360.0;
/// Command palettes are wider so descriptions read on one line.
const CMD_CARD_WIDTH: f64 = 480.0;
const INPUT_HEIGHT: f64 = 40.0;
const PADDING: f64 = 8.0;
/// Band under the input row holding the filter controls.
const FILTER_STRIP_HEIGHT: f64 = 36.0;
/// Slim hint band at the bottom of the card.
const FOOTER_HEIGHT: f64 = 28.0;
/// How much accent a selected row's body carries over the card.
/// Named because the same figure has to be composited to know what any
/// ink on that row is really being measured against.
const SELECTION_WASH: f32 = 0.22;
/// The white wash under a keybinding chip, matching `surface_white_5`.
const KEYBINDING_CHIP_WASH: f32 = 13.0 / 255.0;

impl Widget for CommandPalette {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // padding (top) + input row + gap + [filter strip + gap] + list +
        // gap + footer + padding (bottom). The `gap` between sections reuses
        // `PADDING` so the surface stays a single visual rhythm. The filter
        // strip block collapses to zero in Commands mode, where rows are
        // taller. `desired_size` is the single source of truth so a
        // hand-placed palette (`set_rect`) and a taffy-laid one agree.
        let (width, height) = self.desired_size();
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(width as f32),
                    height: Dimension::length(height as f32),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let colors = &theme.tokens.colors;
        let shape = RoundedRect::from_rect(self.rect, self.surface_radius_px());
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &shape,
        );

        // ONE control for the query. The magnifier and the Ctrl+K chip
        // live inside this field's rounded body; previously the field
        // was a hard-outlined box with the icon floating to its left and
        // the chip floating to its right, which read as three separate
        // widgets that happened to be adjacent.
        let row_rect = self.input_row_rect();
        let layout = self.compute_input_layout(row_rect);
        let field_shape =
            RoundedRect::from_rect(row_rect, theme.tokens.radii.lg as f64);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.surfaces.base,
            None,
            &field_shape,
        );
        // Focus reads as the OS accent at low weight rather than a
        // 2 px form-field blue. This is the palette's only input and it
        // is focused the entire time the palette is open, so a loud ring
        // is permanent noise rather than information — the field's own
        // darker fill is what says "type here".
        let (ring, ring_w) = if self.is_focused {
            (colors.accent[5].with_alpha_factor(0.45), 1.0)
        } else {
            (colors.border_standard, 1.0)
        };
        scene.stroke(&Stroke::new(ring_w), transform, ring, None, &field_shape);

        // The glyph sits inside its reserved slot rather than filling
        // it — a magnifier drawn edge to edge in a 24 px box reads as an
        // icon button next to the field instead of part of it.
        icons::draw_icon(
            scene,
            transform,
            layout.icon_rect.inflate(-3.5, -3.5),
            Icon::Search,
            colors.text_dim,
            1.5,
        );

        // No shortcut chip here. A kbd chip is an *invitation* — it tells
        // you the chord that reaches a thing you have not reached yet — so
        // it belongs on the collapsed affordance an app shows in its own
        // chrome (`SearchField` in launcher mode), and it is noise inside
        // the surface it opens: by the time this row is on screen the user
        // has already pressed the key. It could not even be honest here,
        // because two chords open this palette at two different scopes
        // (Super+K workspace-wide, Ctrl+K scoped to the active app) and a
        // single label has to misname one of them. What scope the palette
        // is actually in is stated properly by the scope chips in the
        // filter strip below, and the keys that do something *now* are in
        // the footer (Navigate / Open / Close). The row's full trailing
        // width goes to the query instead.

        // Query text inside the field. While collecting an argument the
        // slot shows the entry's prompt (dim) followed by the editable
        // buffer (or its placeholder).
        let text_size = theme.tokens.typography.scale.base;
        let text_y =
            layout.input_rect.y0 + (layout.input_rect.height() - text_size as f64) / 2.0;
        let mut tx = layout.input_rect.x0;
        let avail = layout.input_rect.width();
        if let Some(arg) = &self.arg_state {
            if !arg.prompt.is_empty() {
                let heading = format!("{}:  ", arg.prompt);
                text_renderer.draw_text(
                    scene,
                    &heading,
                    text_size,
                    colors.text_dim,
                    transform * Affine::translate((tx, text_y)),
                    None,
                );
                let (hw, _) = text_renderer.measure(&heading, text_size, None);
                tx += hw as f64;
            }
            let (body, color) = if arg.buffer.is_empty() {
                (arg.placeholder.as_str(), colors.text_dim)
            } else {
                (arg.buffer.as_str(), colors.text_primary)
            };
            let body = text_renderer.elide_to_width(body, text_size, (layout.input_rect.x1 - tx) as f32);
            text_renderer.draw_text(
                scene,
                &body,
                text_size,
                color,
                transform * Affine::translate((tx, text_y)),
                None,
            );
        } else {
            let (display, text_color) = if self.query.is_empty() {
                ("Search\u{2026}".to_string(), colors.text_dim)
            } else {
                (self.query.clone(), colors.text_primary)
            };
            let display = text_renderer.elide_to_width(&display, text_size, avail as f32);
            text_renderer.draw_text(
                scene,
                &display,
                text_size,
                text_color,
                transform * Affine::translate((tx, text_y)),
                None,
            );
        }

        // Filter strip. Skipped entirely when the host paints its own
        // controls into the reserved band.
        if self.mode == PaletteMode::Search
            && !self.hide_filter_strip
            && !self.host_filter_strip
        {
            self.draw_filter_strip(scene, transform, text_renderer, theme);
        }

        if self.visible.is_empty() {
            self.draw_empty_state(scene, transform, text_renderer, theme);
        } else {
            self.draw_result_rows(scene, transform, text_renderer, theme);
        }

        self.draw_footer(scene, transform, text_renderer, theme);
    }


    fn handle_event(
        &mut self,
        event: &WindowEvent,
        _scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        if !self.is_focused { return; }
        if let WindowEvent::KeyboardInput {
            event: KeyEvent { logical_key, state: element_state, .. },
            ..
        } = event
        {
            if *element_state != ElementState::Pressed { return; }
            use crate::text_selection::{
                clipboard_copy, clipboard_paste, next_char_boundary,
                next_word_boundary, prev_char_boundary, prev_word_boundary,
                Selection,
            };
            let mods = modifiers.state();
            let shift = mods.shift_key();
            let ctrl = mods.control_key() || mods.super_key();
            // While collecting a free-text argument, every key edits the
            // argument field (or finalises / cancels it) — the result list is
            // frozen and arrow/enter never reach the app behind the palette.
            if self.arg_state.is_some() {
                self.handle_arg_key(logical_key, shift, ctrl, state, path, handled);
                return;
            }
            match logical_key {
                // --- Row navigation: down / up over visible entries.
                Key::Named(NamedKey::ArrowDown) => {
                    if !self.visible.is_empty() {
                        self.cursor = (self.cursor + 1).min(self.visible.len() - 1);
                    }
                    // Consume even at the list end so it never scrolls the app.
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.cursor = self.cursor.saturating_sub(1);
                    *handled = true;
                }
                Key::Named(NamedKey::Enter) => {
                    self.activate_selected(state, path);
                    *handled = true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.is_focused = false;
                    *handled = true;
                }
                // --- Caret navigation inside the query.
                Key::Named(NamedKey::ArrowLeft) => {
                    let pos = self.query_selection.cursor;
                    let new_pos = if ctrl {
                        prev_word_boundary(&self.query, pos)
                    } else {
                        prev_char_boundary(&self.query, pos)
                    };
                    self.query_selection.cursor = new_pos;
                    if !shift {
                        self.query_selection.collapse_to_cursor();
                    }
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowRight) => {
                    let pos = self.query_selection.cursor;
                    let new_pos = if ctrl {
                        next_word_boundary(&self.query, pos)
                    } else {
                        next_char_boundary(&self.query, pos)
                    };
                    self.query_selection.cursor = new_pos;
                    if !shift {
                        self.query_selection.collapse_to_cursor();
                    }
                    *handled = true;
                }
                Key::Named(NamedKey::Home) => {
                    self.query_selection.cursor = 0;
                    if !shift {
                        self.query_selection.collapse_to_cursor();
                    }
                    *handled = true;
                }
                Key::Named(NamedKey::End) => {
                    self.query_selection.cursor = self.query.len();
                    if !shift {
                        self.query_selection.collapse_to_cursor();
                    }
                    *handled = true;
                }
                // --- Deletion.
                Key::Named(NamedKey::Backspace) => {
                    if !self.delete_query_selection() {
                        let pos = self.query_selection.cursor;
                        if pos > 0 {
                            let prev = prev_char_boundary(&self.query, pos);
                            self.query.replace_range(prev..pos, "");
                            self.query_selection = Selection::collapsed(prev);
                            self.rebuild();
                        }
                    }
                    *handled = true;
                }
                Key::Named(NamedKey::Delete) => {
                    if !self.delete_query_selection() {
                        let pos = self.query_selection.cursor;
                        if pos < self.query.len() {
                            let next = next_char_boundary(&self.query, pos);
                            self.query.replace_range(pos..next, "");
                            self.rebuild();
                        }
                    }
                    *handled = true;
                }
                // --- Clipboard / select-all chords.
                Key::Character(s) if ctrl => {
                    match s.as_str() {
                        "a" | "A" => {
                            self.query_selection.anchor = 0;
                            self.query_selection.cursor = self.query.len();
                            *handled = true;
                        }
                        "c" | "C" => {
                            let r = self.query_selection.range();
                            if !r.is_empty() {
                                clipboard_copy(&self.query[r]);
                            }
                            *handled = true;
                        }
                        "x" | "X" => {
                            let r = self.query_selection.range();
                            if !r.is_empty() {
                                clipboard_copy(&self.query[r.clone()]);
                                self.delete_query_selection();
                            }
                            *handled = true;
                        }
                        "v" | "V" => {
                            if let Some(s) = clipboard_paste() {
                                self.replace_query_selection(&s);
                            }
                            *handled = true;
                        }
                        _ => {}
                    }
                }
                Key::Named(NamedKey::Space) => {
                    self.replace_query_selection(" ");
                    *handled = true;
                }
                Key::Character(s) => {
                    // smol-str carries the typed text — strip control
                    // chars, splice the remainder at the caret.
                    let filtered: String =
                        s.chars().filter(|c| !c.is_control()).collect();
                    if !filtered.is_empty() {
                        self.replace_query_selection(&filtered);
                        *handled = true;
                    }
                }
                _ => {}
            }
        }
    }

    fn name(&self) -> &str { "CommandPalette" }

    /// Stable e2e test identifier. The driver-side `widget_registry`
    /// resolves this to the palette's rect so scenarios can address
    /// "the palette input" without going through hard-coded pixel
    /// coordinates. Centre-of-rect lands inside the input row in all
    /// supported viewports (the rect is the palette card; the input
    /// row sits inside it).
    fn test_id(&self) -> Option<&str> { Some("command_palette_input") }

    fn get_rect(&self) -> Rect { self.rect }

    fn is_focusable(&self) -> bool { true }
    fn set_focus(&mut self, focused: bool) { self.is_focused = focused; }
    fn is_focused(&self) -> bool { self.is_focused }

    /// Traps host keyboard focus while open, but only when built as a modal
    /// command launcher (`CommandPalette::commands` / `set_modal(true)`). The
    /// default Search palette leaves this `false` so the shell's overlay host
    /// keeps driving focus itself.
    fn traps_focus(&self) -> bool {
        self.modal && self.is_focused
    }

    /// Reports the palette card as the modal capture rect while open + modal,
    /// so the host suppresses its global shortcuts and swallows click-through.
    fn modal_capture(&self) -> Option<Rect> {
        if self.modal && self.is_focused {
            Some(self.rect)
        } else {
            None
        }
    }

    /// The palette is a modal command launcher: it reports
    /// [`Role::Dialog`] with `modal` set so an AT announces it as a focus-
    /// trapping overlay. The query string is the user's transient input,
    /// not a stable value, so it is not surfaced in `value`.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Dialog)
                .with_name("Command palette")
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused(),
                    modal: true,
                    ..Default::default()
                }),
        )
    }
}

impl CommandPalette {
    /// Drive the palette from a keyboard event without a taffy node, for hosts
    /// that render it as a free-floating overlay (the `ui::App` Ctrl+K action
    /// palette). Returns whether the event was consumed. The palette's
    /// [`Widget::handle_event`] ignores the taffy tree + node id, so a throwaway
    /// single-leaf tree drives the exact same keyboard logic — one code path,
    /// no duplication.
    pub fn handle_overlay_key(
        &mut self,
        event: &WindowEvent,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).expect("leaf never fails");
        <Self as Widget>::handle_event(
            self,
            event,
            1.0,
            modifiers,
            state,
            path,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        handled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::contrast_ratio;
    use crate::tokens::colors::NON_TEXT_MIN_CONTRAST;
    use taffy::TaffyTree;
    use winit::event::Modifiers;
    use winit::keyboard::{KeyLocation, PhysicalKey, SmolStr};

    /// Synthesise a `WindowEvent::KeyboardInput` carrying `logical_key`
    /// in the `Pressed` state. Mirrors the zero-init workaround in
    /// `shell/tests/e2e_shell_flows.rs:1287-1307` — winit 0.30
    /// keeps `KeyEventExtra` `pub(crate)` so we can't construct a
    /// `KeyEvent` through a struct literal.
    fn key_press(logical_key: Key) -> WindowEvent {
        // SAFETY: all pub fields of `KeyEvent` tolerate the all-zero bit
        // pattern; `handle_event` only inspects `logical_key` + `state`.
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key =
            PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    /// Paint one row into a fresh scene and hand back the scene, so a
    /// test can compare what two rows actually encode rather than what
    /// the code path claims to have drawn.
    fn render_row(entry: &CommandEntry, theme: &Theme, selected: bool) -> Scene {
        let p = CommandPalette::new();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        p.draw_one_row(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            theme,
            Rect::new(0.0, 0.0, 420.0, 50.0),
            entry,
            selected,
            14.0,
            11.0,
        );
        scene
    }

    fn count_color_fills(scene: &Scene) -> usize {
        use vello_encoding::DrawTag;
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count()
    }

    /// A badge is a visible chip, not a field nobody paints: a badged
    /// row encodes strictly more than the identical row without one.
    #[test]
    fn a_badged_row_paints_more_than_the_same_row_without_a_badge() {
        let theme = Theme::dark();
        let plain = CommandEntry::new("a", "Open Sheets").description("Opens beside · Office");
        let badged = CommandEntry::new("a", "Fetch and open Sheets")
            .description("Opens beside · Office · from qOS main")
            .badge("not on this machine");

        let bare = count_color_fills(&render_row(&plain, &theme, false));
        let chipped = count_color_fills(&render_row(&badged, &theme, false));
        assert!(
            chipped > bare,
            "the badge has to reach the scene: {bare} fills without it, \
             {chipped} with"
        );
    }

    /// The chip's colours come from the theme, so the marker survives
    /// a theme the designer of this row never saw. A hard-coded fill
    /// would encode identically in all three.
    #[test]
    fn the_badge_is_themed_rather_than_hard_coded() {
        let badged = CommandEntry::new("a", "Fetch and open Sheets").badge("not on this machine");
        let mut seen: Vec<Vec<u32>> = Vec::new();
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let scene = render_row(&badged, &theme, false);
            seen.push(scene.encoding().draw_data.clone());
        }
        assert_ne!(
            seen[0], seen[1],
            "light and dark must not paint the badge identically"
        );
        assert_ne!(
            seen[0], seen[2],
            "high contrast must not paint the badge identically to dark"
        );
    }

    /// Every theme the palette is graded in, each with both grounds a
    /// row's content can land on. A single-theme check is how a chip
    /// derived against one palette ships unreadable on the other two,
    /// and a single-ground check is how it ships unreadable on exactly
    /// the row the user has selected.
    fn grounds() -> Vec<(&'static str, Theme, bool)> {
        let mut out = Vec::new();
        for (name, theme) in [
            ("dark", Theme::dark()),
            ("light", Theme::light()),
            ("high contrast", Theme::high_contrast()),
        ] {
            out.push((name, theme.clone(), false));
            out.push((name, theme, true));
        }
        out
    }

    /// The badge's label has to be readable on the badge, which is the
    /// point of the badge existing: it is the row's whole statement
    /// that the app is not on this machine.
    ///
    /// Both grounds, because the chip's body is a wash — over a
    /// selected row it composites to something else entirely, and the
    /// version of this that measured the accent token against the accent
    /// token would have passed while the screen showed 2.4:1.
    #[test]
    fn the_badge_label_clears_aa_against_the_badge_it_sits_in() {
        for (name, theme, selected) in grounds() {
            let colors = &theme.tokens.colors;
            let ground = CommandPalette::row_ground(colors, selected);
            let chip = colors.accent_chip_on(ground, ON_CARD_SMALL_TEXT_CONTRAST);
            let r = contrast_ratio(chip.ink, chip.fill);
            assert!(
                r >= 4.5,
                "{name} (selected={selected}): badge label {:?} on badge body \
                 {:?} is {r:.2}:1, under AA",
                chip.ink,
                chip.fill
            );
        }
    }

    /// And with headroom above the line, not on it. The chip is drawn
    /// at the bottom of the type scale, where anti-aliasing spends
    /// contrast the measurement of a flat colour does not see — an ink
    /// resolved at exactly 4.5 lands under it on screen.
    #[test]
    fn the_badge_label_is_derived_with_small_text_headroom() {
        for (name, theme, selected) in grounds() {
            let colors = &theme.tokens.colors;
            let ground = CommandPalette::row_ground(colors, selected);
            let chip = colors.accent_chip_on(ground, ON_CARD_SMALL_TEXT_CONTRAST);
            let r = contrast_ratio(chip.ink, chip.fill);
            assert!(
                r >= ON_CARD_SMALL_TEXT_CONTRAST,
                "{name} (selected={selected}): badge label is {r:.2}:1, which \
                 clears AA but leaves nothing for the anti-aliasing"
            );
        }
    }

    /// A pill whose body is within a shade of the row it sits on is
    /// only a pill because of its edge, so the edge has to clear the
    /// non-text floor against the row — not against the body, which is
    /// the measurement that lets an invisible outline pass.
    #[test]
    fn the_badge_edge_clears_the_non_text_floor_against_the_row() {
        for (name, theme, selected) in grounds() {
            let colors = &theme.tokens.colors;
            let ground = CommandPalette::row_ground(colors, selected);
            let chip = colors.accent_chip_on(ground, ON_CARD_SMALL_TEXT_CONTRAST);
            let r = contrast_ratio(chip.border, ground);
            assert!(
                r >= NON_TEXT_MIN_CONTRAST,
                "{name} (selected={selected}): badge edge {:?} on row {:?} is \
                 {r:.2}:1, under the non-text floor",
                chip.border,
                ground
            );
        }
    }

    /// The badge stays an accent badge. Deriving a legible label is not
    /// licence to paint a grey chip — the tint is what makes the marker
    /// findable in a list at a glance, and a derivation that discarded
    /// it would pass every contrast check here while losing the thing
    /// the contrast was for.
    #[test]
    fn the_badge_keeps_the_accent_it_is_derived_from() {
        for (name, theme, selected) in grounds() {
            let colors = &theme.tokens.colors;
            let ground = CommandPalette::row_ground(colors, selected);
            let chip = colors.accent_chip_on(ground, ON_CARD_SMALL_TEXT_CONTRAST);
            assert_ne!(
                chip.fill, ground,
                "{name} (selected={selected}): the badge body has to be a step \
                 off the row, not the row"
            );
            assert_eq!(
                chip.border,
                colors.accent[5],
                "{name} (selected={selected}): the accent already clears the \
                 non-text floor on this ground, so the edge should be it"
            );
        }
    }

    /// The subtitle is the other small text on the row, and on a
    /// selected row it was the theme's dim tier measured against a card
    /// it was no longer sitting on.
    #[test]
    fn the_subtitle_clears_aa_on_both_row_grounds() {
        for (name, theme, selected) in grounds() {
            let colors = &theme.tokens.colors;
            let ground = CommandPalette::row_ground(colors, selected);
            let ink = colors.ink_on(ground, colors.text_dim, ON_CARD_SMALL_TEXT_CONTRAST);
            let r = contrast_ratio(ink, ground);
            assert!(
                r >= 4.5,
                "{name} (selected={selected}): subtitle {ink:?} on row {ground:?} \
                 is {r:.2}:1, under AA"
            );
        }
    }

    /// A section header's count says something only when the section is
    /// holding rows back. Otherwise it is the footer's own number, in
    /// digits, with nothing naming it.
    #[test]
    fn an_uncapped_section_header_prints_no_count() {
        assert_eq!(CommandPalette::header_caption(1, 1), None);
        assert_eq!(CommandPalette::header_caption(5, 5), None);
    }

    /// And when it does say something, it says what it is counting.
    #[test]
    fn a_capped_section_header_names_both_figures() {
        assert_eq!(
            CommandPalette::header_caption(40, 7).as_deref(),
            Some("7 of 40")
        );
    }

    /// A row cannot show a badge and a keybinding at once — they are
    /// the same slot — and the badge is the one that wins, because it
    /// is a fact about what the row does rather than a shortcut hint.
    #[test]
    fn a_badge_takes_the_trailing_slot_from_a_keybinding() {
        let theme = Theme::dark();
        let kb = CommandEntry::new("a", "Sum").keybinding("Alt+=");
        let both = CommandEntry::new("a", "Sum")
            .keybinding("Alt+=")
            .badge("not on this machine");
        let badge_only = CommandEntry::new("a", "Sum").badge("not on this machine");
        assert_eq!(
            render_row(&both, &theme, false).encoding().draw_data,
            render_row(&badge_only, &theme, false).encoding().draw_data,
            "with both set the row paints the badge and nothing of the \
             keybinding"
        );
        assert_ne!(
            render_row(&both, &theme, false).encoding().draw_data,
            render_row(&kb, &theme, false).encoding().draw_data,
            "and it is not simply falling back to the keybinding chip"
        );
    }

    #[test]
    fn fuzzy_score_empty_query_matches_everything() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn fuzzy_score_subsequence_matches() {
        assert!(fuzzy_score("ren", "Rename Workspace").is_some());
    }

    #[test]
    fn fuzzy_score_non_subsequence_is_none() {
        assert_eq!(fuzzy_score("xyz", "Rename Workspace"), None);
    }

    #[test]
    fn set_cursor_clamps_to_visible_len() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.push(CommandEntry::new("b", "Bravo"));
        p.set_cursor(99);
        assert_eq!(p.cursor(), 1);
    }

    #[test]
    fn selected_id_reflects_cursor_position() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.push(CommandEntry::new("b", "Bravo"));
        assert_eq!(p.selected_id(), Some("a"));
        p.set_cursor(1);
        assert_eq!(p.selected_id(), Some("b"));
    }

    #[test]
    fn selected_id_is_none_when_filter_removes_everything() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.set_filter(Box::new(|_| false));
        assert_eq!(p.selected_id(), None);
    }

    #[test]
    fn palette_card_radius_is_24_px() {
        let palette = CommandPalette::new();
        assert_eq!(
            palette.surface_radius_px(),
            24.0,
            "concept palette uses rounded-3xl = 24 px"
        );
    }

    /// The prompt row is a leading glyph and then the query, all the way to
    /// the trailing inset.
    ///
    /// It used to hold back 56 px + a 10 px gap for a `Ctrl+K` kbd pill —
    /// a chip that named a chord for opening a palette that was, by the
    /// time you could read it, already open, and that could not be honest
    /// anyway with two chords bound at two different scopes. Removing it
    /// gives that width to the query, so this pins the row's trailing edge:
    /// nothing may reserve a slot there again without this failing.
    #[test]
    fn input_row_layout_gives_the_query_the_full_trailing_width() {
        const ROW_W: f64 = 640.0;
        const EDGE: f64 = 12.0;
        let palette = CommandPalette::new();
        let layout = palette.compute_input_layout(Rect::new(0.0, 0.0, ROW_W, 56.0));
        assert!(
            layout.icon_rect.width() >= 24.0 && layout.icon_rect.width() <= 40.0,
            "leading icon slot must be 24..=40 px wide; got {}",
            layout.icon_rect.width()
        );
        assert!(
            layout.input_rect.x0 > layout.icon_rect.x1,
            "input must start after the icon"
        );
        assert!(
            layout.input_rect.x1 <= ROW_W - EDGE,
            "input must stay inside the row's trailing inset"
        );
        assert_eq!(
            ROW_W - layout.input_rect.x1,
            EDGE,
            "the query runs to the trailing inset — no reserved chip slot \
             between it and the row edge",
        );
    }

    #[test]
    fn row_at_finds_visible_row_inside_the_post_layout_rect() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.push(CommandEntry::new("b", "Bravo"));
        // Seat the palette rect so row_rect has meaningful coordinates.
        // Height must accommodate input row + filter strip + 2 result
        // rows + footer band — see `build_layout`.
        p.rect = Rect::new(0.0, 0.0, 360.0, 280.0);
        let mid_row_1 = p.row_rect(1).unwrap().center();
        assert_eq!(p.row_at(mid_row_1), Some(1));
        // Clicks outside the rect return None.
        assert!(p.row_at(Point::new(1000.0, 1000.0)).is_none());
    }

    // -------------------------------------------------------------------
    // Task 6 — filter strip + footer hints
    //
    // Failing tests added FIRST (per the plan's TDD discipline). They
    // pin the SearchFilter state machine and the filter-strip / footer
    // layout split. Concept ref:
    // `nirios-concept/src/components/GlobalSearch.tsx:184-244,274-280`.
    // -------------------------------------------------------------------

    #[test]
    fn search_filter_full_text_toggles() {
        let mut f = SearchFilter::default();
        assert!(!f.full_text);
        f.toggle_full_text();
        assert!(f.full_text);
        f.toggle_full_text();
        assert!(!f.full_text);
    }

    #[test]
    fn search_filter_filetypes_multi_select() {
        let mut f = SearchFilter::default();
        f.toggle_filetype(Filetype::Code);
        f.toggle_filetype(Filetype::Docs);
        assert!(f.filetypes.contains(&Filetype::Code));
        assert!(f.filetypes.contains(&Filetype::Docs));
        // Toggling again removes the entry — multi-select acts like a set.
        f.toggle_filetype(Filetype::Code);
        assert!(!f.filetypes.contains(&Filetype::Code));
        assert!(f.filetypes.contains(&Filetype::Docs));
    }

    #[test]
    fn search_filter_default_scope_is_workspace() {
        let f = SearchFilter::default();
        assert_eq!(f.scope, SearchScope::Workspace);
    }

    #[test]
    fn filter_strip_sits_below_input_row_and_above_results() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.rect = Rect::new(0.0, 0.0, 360.0, 280.0);
        let input = p.input_row_rect();
        let strip = p.filter_strip_rect();
        let row0 = p.row_rect(0).expect("seeded row 0 must lay out");
        assert!(
            strip.y0 >= input.y1,
            "filter strip must start at or below input row (input.y1={}, strip.y0={})",
            input.y1,
            strip.y0,
        );
        assert!(
            strip.y1 <= row0.y0,
            "results list must start at or below filter strip (strip.y1={}, row0.y0={})",
            strip.y1,
            row0.y0,
        );
        assert!(
            strip.height() >= 32.0,
            "filter strip must be tall enough for chip pills (got {})",
            strip.height(),
        );
    }

    #[test]
    fn typing_filters_visible_entries() {
        // B-1: typing into a focused palette must filter the visible
        // result list and update the query string. Drives the
        // production `handle_event` code path with a real
        // `WindowEvent::KeyboardInput { Key::Character("h") }`.
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.push(CommandEntry::new("h", "Hotel"));
        p.push(CommandEntry::new("b", "Bravo"));
        p.set_focus(true);

        let total = p.entries.len();
        // Sanity: with no query, all entries are visible.
        assert_eq!(p.visible_entries().len(), total);

        let event = key_press(Key::Character(SmolStr::new("h")));
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();

        p.handle_event(
            &event,
            1.0,
            &modifiers,
            &mut doc,
            &doc_root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );

        assert_eq!(p.query(), "h", "typing must update the query string");
        assert!(
            p.visible_entries().len() < total,
            "typing 'h' must filter out non-matching entries (visible={}, total={})",
            p.visible_entries().len(),
            total,
        );
        assert!(handled, "the palette must consume the keystroke");
    }

    #[test]
    fn footer_sits_below_results_at_card_bottom() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.rect = Rect::new(0.0, 0.0, 360.0, 280.0);
        let row0 = p.row_rect(0).expect("seeded row 0 must lay out");
        let footer = p.footer_rect();
        assert!(
            footer.y0 >= row0.y1,
            "footer must sit below the result list (row0.y1={}, footer.y0={})",
            row0.y1,
            footer.y0,
        );
        assert!(
            footer.y1 <= p.rect.y1,
            "footer must stay inside the card (rect.y1={}, footer.y1={})",
            p.rect.y1,
            footer.y1,
        );
        assert!(
            footer.height() >= 24.0,
            "footer must be tall enough for kbd chips (got {})",
            footer.height(),
        );
    }

    /// Helper for keyboard-routing tests below.
    fn dispatch(
        p: &mut CommandPalette,
        event: WindowEvent,
        modifiers: Modifiers,
    ) {
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        p.handle_event(
            &event,
            1.0,
            &modifiers,
            &mut doc,
            &doc_root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }

    /// ArrowLeft / ArrowRight move the caret inside the query without
    /// touching the row cursor. Pins #277-style caret support in the
    /// palette.
    #[test]
    fn arrow_keys_move_caret_inside_query() {
        let mut p = CommandPalette::new();
        p.set_focus(true);
        p.set_query("hello");
        assert_eq!(p.query_selection.cursor, 5);
        dispatch(
            &mut p,
            key_press(Key::Named(NamedKey::ArrowLeft)),
            Modifiers::default(),
        );
        assert_eq!(p.query_selection.cursor, 4);
        dispatch(
            &mut p,
            key_press(Key::Named(NamedKey::Home)),
            Modifiers::default(),
        );
        assert_eq!(p.query_selection.cursor, 0);
    }

    /// Backspace at a non-end caret deletes the char to the left of
    /// the caret (not the last char of the string).
    #[test]
    fn backspace_deletes_at_caret_not_end() {
        let mut p = CommandPalette::new();
        p.set_focus(true);
        p.set_query("abc");
        p.query_selection.cursor = 1;
        p.query_selection.anchor = 1;
        dispatch(
            &mut p,
            key_press(Key::Named(NamedKey::Backspace)),
            Modifiers::default(),
        );
        assert_eq!(p.query(), "bc");
        assert_eq!(p.query_selection.cursor, 0);
    }

    /// Typing splices into the query at the caret, not at the end.
    #[test]
    fn typing_inserts_at_caret() {
        let mut p = CommandPalette::new();
        p.set_focus(true);
        p.set_query("ac");
        p.query_selection.cursor = 1;
        p.query_selection.anchor = 1;
        dispatch(
            &mut p,
            key_press(Key::Character(SmolStr::new("b"))),
            Modifiers::default(),
        );
        assert_eq!(p.query(), "abc");
        assert_eq!(p.query_selection.cursor, 2);
    }

    /// Ctrl+A selects the entire query.
    #[test]
    fn ctrl_a_selects_query() {
        let mut p = CommandPalette::new();
        p.set_focus(true);
        p.set_query("hello");
        let mut mods_state = winit::keyboard::ModifiersState::empty();
        mods_state.set(winit::keyboard::ModifiersState::CONTROL, true);
        dispatch(
            &mut p,
            key_press(Key::Character(SmolStr::new("a"))),
            Modifiers::from(mods_state),
        );
        assert_eq!(p.query_selection.anchor, 0);
        assert_eq!(p.query_selection.cursor, 5);
    }

    #[test]
    fn accessibility_reports_modal_dialog_role() {
        use crate::a11y::Role;
        let p = CommandPalette::new();
        let node = Widget::accessibility(&p).expect("palette is a modal dialog");
        assert_eq!(node.role, Role::Dialog);
        assert_eq!(node.name.as_deref(), Some("Command palette"));
        assert!(node.state.modal, "the palette traps focus while open");
        assert!(node.state.focusable);
        assert!(!node.state.focused, "fresh palette is not focused");
        assert_eq!(node.value, None, "the query is not surfaced as a value");
    }

    #[test]
    fn accessibility_focus_tracks_focus_state() {
        let mut p = CommandPalette::new();
        p.set_focus(true);
        let node = Widget::accessibility(&p).unwrap();
        assert!(node.state.focused);
    }

    /// Typing into a query with an active selection replaces the
    /// selection instead of appending past it.
    #[test]
    fn typing_replaces_selection() {
        let mut p = CommandPalette::new();
        p.set_focus(true);
        p.set_query("hello");
        p.query_selection.anchor = 1;
        p.query_selection.cursor = 4;
        dispatch(
            &mut p,
            key_press(Key::Character(SmolStr::new("i"))),
            Modifiers::default(),
        );
        assert_eq!(p.query(), "hio");
        assert_eq!(p.query_selection.cursor, 2);
    }

    // -------------------------------------------------------------------
    // Command-launcher mode: descriptions, keybinding hints, free-text
    // arguments, focus capture, and the pull-based outcome API.
    // -------------------------------------------------------------------

    fn seeded_command_palette() -> CommandPalette {
        let mut p = CommandPalette::commands();
        p.push(
            CommandEntry::new("goto", "Go to cell\u{2026}")
                .description("Jump to and select a cell by reference")
                .with_argument("Go to cell", "e.g. B12")
                .with_argument_prefill("A1"),
        );
        p.push(
            CommandEntry::new("sum", "Sum selection")
                .description("Write =SUM() of the selected range below it")
                .keybinding("Alt+="),
        );
        p.push(
            CommandEntry::new("export", "Export CSV")
                .description("Write the active sheet to a .csv file"),
        );
        p.open();
        p
    }

    /// Commands mode traps focus and reports a modal-capture rect while open;
    /// the default Search palette does neither.
    #[test]
    fn command_mode_traps_focus_search_does_not() {
        let mut cmd = seeded_command_palette();
        cmd.set_rect(Rect::new(0.0, 0.0, 480.0, 400.0));
        assert!(Widget::traps_focus(&cmd), "command palette must trap focus");
        assert!(Widget::modal_capture(&cmd).is_some());
        cmd.close();
        assert!(!Widget::traps_focus(&cmd), "closed palette releases focus");

        let mut search = CommandPalette::new();
        search.push(CommandEntry::new("a", "Alpha"));
        search.set_focus(true);
        assert!(!Widget::traps_focus(&search), "search palette never traps");
        assert!(Widget::modal_capture(&search).is_none());
    }

    /// The crux: ↑/↓ move the highlighted row (not the app), Enter runs the
    /// highlighted action, and every one of those keys is consumed so nothing
    /// leaks to the app behind the overlay.
    #[test]
    fn command_mode_arrows_and_enter_are_captured_and_navigate() {
        let mut p = seeded_command_palette();
        assert_eq!(p.selected_id(), Some("goto"));

        // ArrowDown moves the highlight and is consumed.
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT.clone();
        for _ in 0..2 {
            let mut handled = false;
            let mut focus = None;
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            p.handle_event(
                &key_press(Key::Named(NamedKey::ArrowDown)),
                1.0,
                &Modifiers::default(),
                &mut doc,
                &root,
                &mut handled,
                &mut focus,
                node,
                &taffy,
            );
            assert!(handled, "ArrowDown must be consumed by the palette");
        }
        assert_eq!(p.selected_id(), Some("export"), "cursor advanced two rows");

        // ArrowUp is also consumed and moves back.
        let mut handled = false;
        dispatch_h(&mut p, key_press(Key::Named(NamedKey::ArrowUp)), Modifiers::default(), &mut handled);
        assert!(handled);
        assert_eq!(p.selected_id(), Some("sum"));

        // Enter on a plain (no-argument) entry runs it → outcome, palette closes.
        let mut handled = false;
        dispatch_h(&mut p, key_press(Key::Named(NamedKey::Enter)), Modifiers::default(), &mut handled);
        assert!(handled, "Enter is consumed");
        let outcome = p.take_outcome().expect("Enter emits an outcome");
        assert_eq!(outcome.id, "sum");
        assert_eq!(outcome.argument, None);
        assert!(!p.is_open(), "running a command closes the palette");
    }

    /// Filtering matches over label *and* description.
    #[test]
    fn command_mode_filter_matches_label_and_description() {
        let mut p = seeded_command_palette();
        // "csv" only appears in the Export entry's description.
        p.set_query("csv");
        let ids: Vec<&str> = p.visible_entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["export"], "description text is searchable");

        // A label match still works.
        p.set_query("sum");
        let ids: Vec<&str> = p.visible_entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["sum"]);
    }

    /// The "Go to cell" flow: Enter opens the argument step (seeded + fully
    /// selected), typing replaces the prefill, and a second Enter emits the
    /// outcome carrying the typed argument.
    #[test]
    fn command_mode_argument_flow_returns_typed_value() {
        let mut p = seeded_command_palette();
        assert_eq!(p.selected_id(), Some("goto"));

        // Enter → argument mode, prefilled with the active cell "A1", fully
        // selected so the first keystroke replaces it.
        let mut handled = false;
        dispatch_h(&mut p, key_press(Key::Named(NamedKey::Enter)), Modifiers::default(), &mut handled);
        assert!(handled);
        assert!(p.in_argument_mode(), "entry with an argument opens the arg step");
        assert_eq!(p.argument_buffer(), Some("A1"));
        assert!(p.take_outcome().is_none(), "no outcome until the argument is confirmed");

        // Type "B12" — replaces the selected prefill.
        for ch in ["B", "1", "2"] {
            let mut h = false;
            dispatch_h(&mut p, key_press(Key::Character(SmolStr::new(ch))), Modifiers::default(), &mut h);
            assert!(h, "typing in the arg field is consumed");
        }
        assert_eq!(p.argument_buffer(), Some("B12"));

        // Confirm.
        let mut handled = false;
        dispatch_h(&mut p, key_press(Key::Named(NamedKey::Enter)), Modifiers::default(), &mut handled);
        assert!(handled);
        let outcome = p.take_outcome().expect("confirming emits an outcome");
        assert_eq!(outcome.id, "goto");
        assert_eq!(outcome.argument.as_deref(), Some("B12"));
        assert!(!p.is_open());
    }

    /// Escape from the argument step returns to the list rather than closing.
    #[test]
    fn command_mode_escape_from_argument_returns_to_list() {
        let mut p = seeded_command_palette();
        let mut h = false;
        dispatch_h(&mut p, key_press(Key::Named(NamedKey::Enter)), Modifiers::default(), &mut h);
        assert!(p.in_argument_mode());
        let mut h = false;
        dispatch_h(&mut p, key_press(Key::Named(NamedKey::Escape)), Modifiers::default(), &mut h);
        assert!(h, "Escape is consumed");
        assert!(!p.in_argument_mode(), "Escape leaves the argument step");
        assert!(p.is_open(), "…but the palette is still open on the list");
        assert!(p.take_outcome().is_none());
    }

    /// Command rows are taller than search rows and the card is wider, and the
    /// filter-strip block collapses so the list sits directly under the prompt.
    #[test]
    fn command_mode_geometry_differs_from_search() {
        let cmd = seeded_command_palette();
        let (cw, _) = cmd.desired_size();
        assert_eq!(cw, CMD_CARD_WIDTH);
        assert_eq!(cmd.row_height(), TWO_LINE_ROW_HEIGHT);
        assert_eq!(cmd.filter_block(), 0.0, "no filter strip in command mode");

        let search = CommandPalette::new();
        let (sw, _) = search.desired_size();
        assert_eq!(sw, SEARCH_CARD_WIDTH);
        assert_eq!(search.row_height(), ROW_HEIGHT);
        assert!(search.filter_block() > 0.0, "search keeps its filter strip");
    }

    /// With more entries than fit, the list scrolls so the highlighted row is
    /// always painted (its `row_rect` is `Some`) while off-window rows are not.
    #[test]
    fn command_mode_scrolls_to_keep_cursor_visible() {
        let mut p = CommandPalette::commands();
        for i in 0..20 {
            p.push(CommandEntry::new(format!("id{i}"), format!("Command {i}")));
        }
        p.open();
        let (w, h) = p.desired_size();
        p.set_rect(Rect::new(0.0, 0.0, w, h));
        // Cursor at the top: row 0 visible, a row past the cap is not.
        assert!(p.row_rect(0).is_some());
        assert!(p.row_rect(p.max_visible_rows()).is_none(), "row past the cap is scrolled off");

        // Move the cursor to the last entry: it must now be painted, and row 0
        // must have scrolled off the top.
        p.set_cursor(19);
        assert!(p.row_rect(19).is_some(), "highlighted last row stays visible");
        assert!(p.row_rect(0).is_none(), "the top row scrolled out of view");
    }

    /// A grouped result set plans one header per group, in the order the
    /// entries arrived, and never splits a group.
    #[test]
    fn grouped_entries_plan_one_header_per_section() {
        let mut p = CommandPalette::new();
        p.set_bypass_fuzzy(true);
        p.set_entries(vec![
            CommandEntry::new("a", "Open Calculator").group("Applications"),
            CommandEntry::new("b", "calc.rs").group("Files"),
            CommandEntry::new("c", "calculator.rs").group("Files"),
        ]);
        let headers: Vec<&str> = p
            .rows
            .iter()
            .filter_map(|r| match r {
                VisualRow::Header { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(headers, vec!["Applications", "Files"]);
        assert_eq!(
            p.rows.len(),
            5,
            "two headers plus three entries: {:?}",
            p.rows
        );
    }

    /// A header reports the group's real size, not the number of rows
    /// the host handed over, so a trimmed section says what it is
    /// hiding — and the footer agrees with it.
    #[test]
    fn a_trimmed_section_reports_its_untrimmed_count() {
        let mut p = CommandPalette::new();
        p.set_bypass_fuzzy(true);
        p.set_entries(vec![
            CommandEntry::new("a", "one").group("Files").group_total(40),
            CommandEntry::new("b", "two").group("Files").group_total(40),
        ]);
        match &p.rows[0] {
            VisualRow::Header { label, total } => {
                assert_eq!(label, "Files");
                assert_eq!(*total, 40);
            }
            other => panic!("expected a header, got {other:?}"),
        }
        assert_eq!(
            p.result_total(),
            40,
            "the footer count must not contradict the section header"
        );
    }

    /// Painting and hit-testing must agree about which row is where,
    /// including once the list has scrolled. Search mode used to paint
    /// from entry 0 while `row_rect`/`row_at` scrolled with the cursor,
    /// so past the visible cap a click landed on a different result than
    /// the one under the pointer.
    #[test]
    fn a_scrolled_list_hit_tests_the_row_it_painted() {
        let mut p = CommandPalette::new();
        p.set_bypass_fuzzy(true);
        p.set_entries(
            (0..30)
                .map(|i| {
                    CommandEntry::new(format!("id{i}"), format!("Result {i}"))
                        .description(format!("~/path/to/{i}"))
                        .group("Files")
                })
                .collect(),
        );
        let (w, h) = p.desired_size();
        p.set_rect(Rect::new(0.0, 0.0, w, h));

        // Scroll to the end.
        p.set_cursor(29);
        let painted: Vec<usize> = (0..30).filter(|&i| p.row_rect(i).is_some()).collect();
        assert!(
            painted.contains(&29),
            "the cursor's row must be painted: {painted:?}"
        );
        assert!(
            !painted.contains(&0),
            "row 0 must have scrolled off: {painted:?}"
        );
        // Every painted row answers `row_at` at its own centre, and with
        // its own index.
        for i in painted {
            let r = p.row_rect(i).expect("painted");
            assert_eq!(
                p.row_at(r.center()),
                Some(i),
                "row {i} painted at {r:?} but hit-tests as {:?}",
                p.row_at(r.center())
            );
        }
    }

    /// Rows and headers never spill past the footer.
    #[test]
    fn no_painted_row_overlaps_the_footer() {
        let mut p = CommandPalette::new();
        p.set_bypass_fuzzy(true);
        p.set_entries(
            (0..40)
                .map(|i| {
                    CommandEntry::new(format!("id{i}"), format!("Result {i}"))
                        .description("sub")
                        .group(if i < 3 { "Actions" } else { "Files" })
                })
                .collect(),
        );
        let (w, h) = p.desired_size();
        p.set_rect(Rect::new(0.0, 0.0, w, h));
        let footer_top = p.footer_rect().y0;
        for i in 0..40 {
            if let Some(r) = p.row_rect(i) {
                assert!(
                    r.y1 <= footer_top,
                    "row {i} at {r:?} runs into the footer at {footer_top}"
                );
                assert!(r.y0 >= p.list_top_y() - 0.01, "row {i} starts above the list");
            }
        }
    }

    /// The zero-state gets real vertical room. Sized off the row height
    /// it printed its own hint on top of the footer.
    #[test]
    fn an_empty_result_set_still_gets_a_readable_card() {
        let mut p = CommandPalette::new();
        p.set_bypass_fuzzy(true);
        p.set_query("nothing matches this");
        p.set_entries(vec![]);
        let (_, h) = p.desired_size();
        p.set_rect(Rect::new(0.0, 0.0, 640.0, h));
        let room = p.footer_rect().y0 - p.list_top_y();
        assert!(
            room >= EMPTY_STATE_HEIGHT - 1.0,
            "the zero-state needs {EMPTY_STATE_HEIGHT} px of list area, got {room}"
        );
    }

    /// A host that paints its own filter controls still gets the band
    /// reserved inside the card, on the card's own inset — that is what
    /// keeps its chips aligned with the field above and the rows below.
    #[test]
    fn a_host_filter_strip_is_reserved_on_the_cards_inset() {
        let mut p = CommandPalette::new();
        p.set_host_filter_strip(true);
        p.set_rect(Rect::new(0.0, 0.0, 640.0, 400.0));
        let band = p.filter_strip_rect();
        let field = p.input_row_rect();
        assert_eq!(
            band.x0, field.x0,
            "the host band must share the field's left edge"
        );
        assert_eq!(band.x1, field.x1);
        assert_eq!(band.x0, p.rect.x0 + p.content_inset_px());
        assert!(band.y0 >= field.y1, "the band sits under the field");
        assert!(
            p.list_top_y() >= band.y1,
            "rows start below the band, not behind it"
        );
    }

    /// A host with more controls than fit on one row asks for more band,
    /// and gets it: the card grows by exactly those rows and the results
    /// still start below them.
    #[test]
    fn a_host_can_reserve_more_than_one_row_of_filter_band() {
        let mut one = CommandPalette::new();
        one.set_host_filter_strip(true);
        one.set_rect(Rect::new(0.0, 0.0, 640.0, 400.0));
        let (_, one_high) = one.desired_size();
        let one_band = one.filter_strip_rect();

        let mut two = CommandPalette::new();
        two.set_host_filter_strip(true);
        assert!(
            two.set_host_filter_strip_rows(2),
            "asking for a different row count is a change"
        );
        assert!(
            !two.set_host_filter_strip_rows(2),
            "asking for the same row count again is not"
        );
        two.set_rect(Rect::new(0.0, 0.0, 640.0, 400.0));
        let (_, two_high) = two.desired_size();
        let band = two.filter_strip_rect();

        assert_eq!(two.host_filter_strip_rows(), 2);
        assert_eq!(
            band.height(),
            one_band.height() + two.filter_strip_row_height(),
            "the band grows by exactly one row"
        );
        assert_eq!(
            two_high - one_high,
            two.filter_strip_row_height(),
            "and so does the card, so nothing else moves"
        );
        assert!(
            two.list_top_y() >= band.y1,
            "rows still start below the band: {} vs {}",
            two.list_top_y(),
            band.y1
        );
        assert_eq!(
            band.y0, one_band.y0,
            "the band still starts under the field — it grows downward"
        );
    }

    /// Helper: dispatch one event and surface whether the palette consumed it.
    fn dispatch_h(
        p: &mut CommandPalette,
        event: WindowEvent,
        modifiers: Modifiers,
        handled: &mut bool,
    ) {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT.clone();
        let mut focus: Option<NodeId> = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        p.handle_event(
            &event,
            1.0,
            &modifiers,
            &mut doc,
            &root,
            handled,
            &mut focus,
            node,
            &taffy,
        );
    }
}
