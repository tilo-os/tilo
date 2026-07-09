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

use crate::icons::{self, Icon};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

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
            on_select: None,
        }
    }

    /// Attach a one-line description shown under the label (Commands mode).
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
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
    pub kbd_chip_rect: Rect,
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
        let rows = self.visible.len().clamp(1, self.max_visible_rows()) as f64;
        let height = PADDING * 2.0
            + INPUT_HEIGHT
            + PADDING
            + self.filter_block()
            + rows * self.row_height()
            + PADDING
            + FOOTER_HEIGHT;
        (self.card_width(), height)
    }

    /// Height reserved for the filter strip + its trailing gap (Search only).
    fn filter_block(&self) -> f64 {
        if self.hide_filter_strip {
            return 0.0;
        }
        match self.mode {
            PaletteMode::Search => FILTER_STRIP_HEIGHT + PADDING,
            PaletteMode::Commands => 0.0,
        }
    }

    /// Per-result-row height. Commands rows are taller to fit a
    /// description; Search rows grow the same way when
    /// `set_bypass_fuzzy(true)` is enabled AND at least one visible
    /// entry carries a description — the provider-driven path pushes
    /// subtitles like `[Files] ~/foo/bar` through `description`, so we
    /// need the extra vertical room to render them.
    fn row_height(&self) -> f64 {
        let search_row = if self.bypass_fuzzy
            && self.visible.iter().any(|&i| {
                self.entries
                    .get(i)
                    .map(|e| e.description.is_some())
                    .unwrap_or(false)
            }) {
            CMD_ROW_HEIGHT
        } else {
            ROW_HEIGHT
        };
        match self.mode {
            PaletteMode::Search => search_row,
            PaletteMode::Commands => CMD_ROW_HEIGHT,
        }
    }

    /// Card width for the current mode.
    fn card_width(&self) -> f64 {
        match self.mode {
            PaletteMode::Search => SEARCH_CARD_WIDTH,
            PaletteMode::Commands => CMD_CARD_WIDTH,
        }
    }

    /// Max rows rendered before the list clips.
    fn max_visible_rows(&self) -> usize {
        match self.mode {
            PaletteMode::Search => 10,
            PaletteMode::Commands => 8,
        }
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

    /// Index of the first visible entry actually painted, given the cursor and
    /// the row cap. The list scrolls just enough to keep the highlighted row on
    /// screen when there are more entries than fit.
    fn first_visible_row(&self) -> usize {
        let max = self.max_visible_rows();
        if self.cursor >= max {
            self.cursor + 1 - max
        } else {
            0
        }
    }

    /// Rect of the `i`th *visible* row, in the same coordinate space as
    /// `get_rect()`. Returns `None` when the row is scrolled out of the
    /// currently painted window.
    pub fn row_rect(&self, i: usize) -> Option<Rect> {
        if i >= self.visible.len() {
            return None;
        }
        let first = self.first_visible_row();
        if i < first || i >= first + self.max_visible_rows() {
            return None;
        }
        let list_top = self.list_top_y();
        let display = (i - first) as f64;
        let y0 = list_top + display * self.row_height();
        let y1 = y0 + self.row_height();
        // The footer + its gap eat into the bottom of the card; cap at
        // the start of the footer band rather than `rect.y1 - PADDING`.
        let max_y = self.footer_rect().y0 - PADDING;
        if y1 > max_y {
            return None;
        }
        Some(Rect::new(self.rect.x0 + PADDING, y0, self.rect.x1 - PADDING, y1))
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

    /// Splits the input row into three sub-rects: a leading 24 px icon
    /// slot (lucide `Search`), the input field, and a trailing 56 px
    /// kbd-chip slot (`⌘K`). Both flanking slots are inset 12 px from
    /// the row edge, with another 12 px gap before/after the input.
    ///
    /// Concept ref: `GlobalSearch.tsx:138-153` — leading magnifier glyph
    /// + input + trailing `⌘K` kbd pill.
    pub fn compute_input_layout(&self, row: Rect) -> InputRowLayout {
        const ICON_W: f64 = 24.0;
        const CHIP_W: f64 = 48.0;
        const EDGE: f64 = 12.0;
        const GAP: f64 = 12.0;
        let cy = (row.y0 + row.y1) / 2.0;
        let icon_y0 = cy - ICON_W / 2.0;
        let icon_rect = Rect::new(
            row.x0 + EDGE,
            icon_y0,
            row.x0 + EDGE + ICON_W,
            icon_y0 + ICON_W,
        );
        let chip_h = (row.height() - EDGE).max(20.0);
        let chip_y0 = cy - chip_h / 2.0;
        let kbd_chip_rect = Rect::new(
            row.x1 - EDGE - CHIP_W,
            chip_y0,
            row.x1 - EDGE,
            chip_y0 + chip_h,
        );
        let input_rect = Rect::new(
            icon_rect.x1 + GAP,
            row.y0,
            kbd_chip_rect.x0 - GAP,
            row.y1,
        );
        InputRowLayout {
            icon_rect,
            input_rect,
            kbd_chip_rect,
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

    /// 40 px filter strip painted directly under the prompt row. Holds
    /// scope chips on the left, filetype chips in the middle, and a
    /// full-text toggle on the right (concept
    /// `GlobalSearch.tsx:184-244`).
    pub fn filter_strip_rect(&self) -> Rect {
        let row = self.input_row_rect();
        let y0 = row.y1 + PADDING;
        Rect::new(row.x0, y0, row.x1, y0 + FILTER_STRIP_HEIGHT)
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

    /// Footer paint: `↑↓ Navigate / Enter Select` on the left and the
    /// active scope label on the right, on top of a `bg-black/20` band
    /// with a 1 px slate-700 top border.
    fn draw_footer(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let footer = self.footer_rect();
        // `bg-black/20` band — 20% black over the slate surface.
        let band_body = Color::from_rgba8(0, 0, 0, 51);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            band_body,
            None,
            &footer,
        );
        // 1 px top border (slate-700 ≈ `border_standard`).
        let border = vello::kurbo::Line::new(
            (footer.x0, footer.y0),
            (footer.x1, footer.y0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &border,
        );

        let chip_text_size = (theme.tokens.typography.scale.sm - 2.0).max(9.0);
        let chip_body = Color::from_rgba8(255, 255, 255, 13);  // white/5
        let chip_border = Color::from_rgba8(255, 255, 255, 26);  // white/10
        let dim = theme.tokens.colors.text_dim;
        let chip_h = (footer.height() - 14.0).max(14.0);
        let cy = (footer.y0 + footer.y1) / 2.0;

        // Left cluster: kbd chip + label, repeated for ↑↓ / Enter.
        let mut x = footer.x0 + 4.0;
        for (key, label) in [("\u{2191}\u{2193}", "Navigate"), ("Enter", "Select")] {
            let (kw, kh) = text_renderer.measure(key, chip_text_size, None);
            let chip_w = kw as f64 + 10.0;
            let chip_rect =
                Rect::new(x, cy - chip_h / 2.0, x + chip_w, cy + chip_h / 2.0);
            let chip_shape = RoundedRect::from_rect(chip_rect, 4.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                chip_body,
                None,
                &chip_shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                chip_border,
                None,
                &chip_shape,
            );
            text_renderer.draw_text(
                scene,
                key,
                chip_text_size,
                dim,
                transform
                    * Affine::translate((
                        chip_rect.x0 + (chip_rect.width() - kw as f64) / 2.0,
                        chip_rect.y0 + (chip_rect.height() - kh as f64) / 2.0,
                    )),
                None,
            );
            x = chip_rect.x1 + 4.0;
            let (lw, lh) = text_renderer.measure(label, chip_text_size, None);
            text_renderer.draw_text(
                scene,
                label,
                chip_text_size,
                dim,
                transform * Affine::translate((x, cy - lh as f64 / 2.0)),
                None,
            );
            x += lw as f64 + 12.0;
        }

        // Right-anchored label. Search shows the active scope; a command
        // palette shows the dismiss hint instead (scope is meaningless here).
        let right_label = match self.mode {
            PaletteMode::Search => match self.search_filter.scope {
                SearchScope::Workspace => "in Workspace",
                SearchScope::All => "in All Workspaces",
            },
            PaletteMode::Commands => "Esc to close",
        };
        let (sw, sh) = text_renderer.measure(right_label, chip_text_size, None);
        text_renderer.draw_text(
            scene,
            right_label,
            chip_text_size,
            dim,
            transform * Affine::translate((
                footer.x1 - sw as f64 - 4.0,
                cy - sh as f64 / 2.0,
            )),
            None,
        );
    }

    /// Draw the [`PaletteMode::Commands`] result rows: a highlighted selection
    /// row, each entry's label, a dimmed one-line description underneath, and
    /// an optional right-aligned keybinding chip (or a `→` cue for entries that
    /// take an argument).
    fn draw_command_rows(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        list_top: f64,
        footer_top: f64,
    ) {
        let colors = &theme.tokens.colors;
        let label_size = theme.tokens.typography.scale.base;
        let desc_size = (theme.tokens.typography.scale.sm - 1.0).max(10.0);
        let rh = self.row_height();
        // Scroll the window so the highlighted row is always painted.
        let first = self.first_visible_row();
        let max_rows = self.max_visible_rows();
        for (i, &entry_idx) in self.visible.iter().enumerate().skip(first).take(max_rows) {
            let y0 = list_top + ((i - first) as f64) * rh;
            let y1 = y0 + rh;
            if y1 > footer_top - PADDING {
                break;
            }
            let row = Rect::new(self.rect.x0 + PADDING, y0, self.rect.x1 - PADDING, y1);
            let selected = i == self.cursor;
            if selected {
                // Concept selected row = fully indigo (accent[5]).
                let highlight = RoundedRect::from_rect(row, theme.tokens.radii.sm as f64);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    colors.accent[5],
                    None,
                    &highlight,
                );
            }
            let entry = &self.entries[entry_idx];
            let has_desc = entry.description.is_some();
            let lx = row.x0 + 12.0;

            // Label.
            let label_y = if has_desc {
                row.y0 + 8.0
            } else {
                row.y0 + (row.height() - label_size as f64) / 2.0
            };
            text_renderer.draw_text(
                scene,
                &entry.label,
                label_size,
                colors.text_primary,
                transform * Affine::translate((lx, label_y)),
                None,
            );

            // Description (dimmed, second line).
            if let Some(desc) = &entry.description {
                let dy = row.y1 - desc_size as f64 - 9.0;
                let desc_color = if selected {
                    colors.text_primary
                } else {
                    colors.text_dim
                };
                text_renderer.draw_text(
                    scene,
                    desc,
                    desc_size,
                    desc_color,
                    transform * Affine::translate((lx, dy)),
                    None,
                );
            }

            // Right-aligned keybinding chip, or an argument cue.
            if let Some(kb) = &entry.keybinding {
                let (kw, kh) = text_renderer.measure(kb, desc_size, None);
                let chip_w = kw as f64 + 14.0;
                let chip_h = kh as f64 + 8.0;
                let cy = (row.y0 + row.y1) / 2.0;
                let chip = Rect::new(
                    row.x1 - chip_w - 6.0,
                    cy - chip_h / 2.0,
                    row.x1 - 6.0,
                    cy + chip_h / 2.0,
                );
                let chip_shape = RoundedRect::from_rect(chip, 5.0);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    Color::from_rgba8(255, 255, 255, 13),
                    None,
                    &chip_shape,
                );
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    Color::from_rgba8(255, 255, 255, 26),
                    None,
                    &chip_shape,
                );
                text_renderer.draw_text(
                    scene,
                    kb,
                    desc_size,
                    colors.text_dim,
                    transform * Affine::translate((chip.x0 + 7.0, chip.center().y - kh as f64 / 2.0)),
                    None,
                );
            } else if entry.argument.is_some() {
                let cue = "\u{2192}"; // →
                let (hw, hh) = text_renderer.measure(cue, label_size, None);
                let color = if selected { colors.text_primary } else { colors.text_dim };
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
            }
        }
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
    }
}

const ROW_HEIGHT: f64 = 32.0;
/// Taller result row in [`PaletteMode::Commands`] — fits a label plus a
/// dimmed one-line description underneath.
const CMD_ROW_HEIGHT: f64 = 50.0;
const SEARCH_CARD_WIDTH: f64 = 360.0;
/// Command palettes are wider so descriptions read on one line.
const CMD_CARD_WIDTH: f64 = 480.0;
const INPUT_HEIGHT: f64 = 36.0;
const PADDING: f64 = 8.0;
/// Concept `GlobalSearch.tsx:184` — a `py-2` strip (≈ 40 px) wrapping
/// the chip row.
const FILTER_STRIP_HEIGHT: f64 = 40.0;
/// Concept `GlobalSearch.tsx:274-280` — a `p-3` band (≈ 36 px) holding
/// the kbd-chip hints + scope label.
const FOOTER_HEIGHT: f64 = 36.0;

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
        // Popover-shaped surface with a soft border. Concept's
        // `rounded-3xl` (= 24 px) lives in [`surface_radius_px`] so the
        // value is pinned by the radius test and reused here.
        let shape = RoundedRect::from_rect(self.rect, self.surface_radius_px());
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

        // Split the prompt row into leading-icon / input / trailing
        // kbd-chip slots — concept `GlobalSearch.tsx:138-153`.
        let row_rect = self.input_row_rect();
        let layout = self.compute_input_layout(row_rect);
        let input_shape =
            RoundedRect::from_rect(layout.input_rect, theme.tokens.radii.sm as f64);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &input_shape,
        );
        let border_color = if self.is_focused {
            theme.tokens.colors.border_focus
        } else {
            theme.tokens.colors.border_standard
        };
        scene.stroke(
            &Stroke::new(if self.is_focused { 2.0 } else { 1.0 }),
            transform,
            border_color,
            None,
            &input_shape,
        );

        // Leading lucide `Search` glyph (slate-400 = `text_dim`). Drawn
        // unconditionally whenever the palette is open — concept never
        // hover-gates this affordance.
        icons::draw_icon(
            scene,
            transform,
            layout.icon_rect,
            Icon::Search,
            theme.tokens.colors.text_dim,
            1.5,
        );

        // Trailing `⌘K` kbd chip — `bg-white/5 border border-white/10
        // text-slate-300 font-mono rounded-md`. Body fill + border
        // stroke + small mono label.
        let chip_shape = RoundedRect::from_rect(layout.kbd_chip_rect, 6.0);
        // Concept: `bg-white/5 border border-white/10` — semi-transparent
        // white at 5% / 10% opacity over the slate-900 surface tint.
        let chip_body = Color::from_rgba8(255, 255, 255, 13);
        let chip_border = Color::from_rgba8(255, 255, 255, 26);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            chip_body,
            None,
            &chip_shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            chip_border,
            None,
            &chip_shape,
        );
        let chip_text = "\u{2318}K"; // ⌘K
        let chip_text_size = (theme.tokens.typography.scale.sm - 1.0).max(10.0);
        let (cw, ch) = text_renderer.measure(chip_text, chip_text_size, None);
        let chip_text_x = (layout.kbd_chip_rect.x0 + layout.kbd_chip_rect.x1) / 2.0
            - cw as f64 / 2.0;
        let chip_text_y = (layout.kbd_chip_rect.y0 + layout.kbd_chip_rect.y1) / 2.0
            - ch as f64 / 2.0;
        text_renderer.draw_text(
            scene,
            chip_text,
            chip_text_size,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((chip_text_x, chip_text_y)),
            None,
        );

        // Prompt text inside the input slot. While collecting an argument the
        // slot shows the entry's prompt (dim) followed by the editable buffer
        // (or its placeholder); otherwise the search query / placeholder.
        let text_size = theme.tokens.typography.scale.sm;
        let text_y = layout.input_rect.y0
            + (layout.input_rect.height() - text_size as f64) / 2.0;
        let mut tx = layout.input_rect.x0 + 8.0;
        if let Some(arg) = &self.arg_state {
            if !arg.prompt.is_empty() {
                let heading = format!("{}:  ", arg.prompt);
                text_renderer.draw_text(
                    scene,
                    &heading,
                    text_size,
                    theme.tokens.colors.text_dim,
                    transform * Affine::translate((tx, text_y)),
                    None,
                );
                let (hw, _) = text_renderer.measure(&heading, text_size, None);
                tx += hw as f64;
            }
            let (body, color) = if arg.buffer.is_empty() {
                (arg.placeholder.as_str(), theme.tokens.colors.text_dim)
            } else {
                (arg.buffer.as_str(), theme.tokens.colors.text_primary)
            };
            text_renderer.draw_text(
                scene,
                body,
                text_size,
                color,
                transform * Affine::translate((tx, text_y)),
                None,
            );
        } else {
            let display = if self.query.is_empty() { "Search\u{2026}" } else { &self.query };
            let text_color = if self.query.is_empty() {
                theme.tokens.colors.text_dim
            } else {
                theme.tokens.colors.text_primary
            };
            text_renderer.draw_text(
                scene,
                display,
                text_size,
                text_color,
                transform * Affine::translate((tx, text_y)),
                None,
            );
        }

        // Filter strip (Search only) — scope chips, filetype chips, full-text
        // toggle. Suppressed when a provider host is rendering its own chip
        // strip above the palette (see `set_hide_filter_strip`).
        if self.mode == PaletteMode::Search && !self.hide_filter_strip {
            self.draw_filter_strip(scene, transform, text_renderer, theme);
        }

        // Vertical list of visible entries.
        let list_top = self.list_top_y();
        let footer_top = self.footer_rect().y0;
        match self.mode {
            PaletteMode::Commands => {
                self.draw_command_rows(scene, transform, text_renderer, theme, list_top, footer_top);
            }
            PaletteMode::Search => {
                let row_h = self.row_height();
                for (i, &entry_idx) in self.visible.iter().enumerate() {
                    let y0 = list_top + (i as f64) * row_h;
                    let y1 = y0 + row_h;
                    if y1 > footer_top - PADDING {
                        break;
                    }
                    let entry_rect =
                        Rect::new(self.rect.x0 + PADDING, y0, self.rect.x1 - PADDING, y1);
                    if i == self.cursor {
                        let highlight =
                            RoundedRect::from_rect(entry_rect, theme.tokens.radii.sm as f64);
                        scene.fill(
                            vello::peniko::Fill::NonZero,
                            transform,
                            theme.tokens.colors.neutral[4],
                            None,
                            &highlight,
                        );
                    }
                    let entry = &self.entries[entry_idx];
                    let label = &entry.label;
                    let desc = entry.description.as_deref();
                    // Two-line layout when a description is present so
                    // provider hosts can surface a subtitle (kind label
                    // + path snippet + ...) without switching modes.
                    if let Some(desc) = desc {
                        let label_y = entry_rect.y0 + 6.0;
                        let desc_y = label_y + text_size as f64 + 4.0;
                        text_renderer.draw_text(
                            scene,
                            label,
                            text_size,
                            theme.tokens.colors.text_primary,
                            transform * Affine::translate((entry_rect.x0 + 8.0, label_y)),
                            None,
                        );
                        text_renderer.draw_text(
                            scene,
                            desc,
                            text_size - 2.0,
                            theme.tokens.colors.text_dim,
                            transform * Affine::translate((entry_rect.x0 + 8.0, desc_y)),
                            None,
                        );
                    } else {
                        let label_y = entry_rect.y0 + (entry_rect.height() - text_size as f64) / 2.0;
                        text_renderer.draw_text(
                            scene,
                            label,
                            text_size,
                            theme.tokens.colors.text_primary,
                            transform * Affine::translate((entry_rect.x0 + 8.0, label_y)),
                            None,
                        );
                    }
                }
            }
        }

        // Footer band: kbd-chip hints on the left + scope label on the
        // right. Concept `GlobalSearch.tsx:274-280`.
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

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn input_row_layout_reserves_leading_icon_and_kbd_chip() {
        let palette = CommandPalette::new();
        let layout = palette.compute_input_layout(Rect::new(0.0, 0.0, 640.0, 56.0));
        assert!(
            layout.icon_rect.width() >= 24.0 && layout.icon_rect.width() <= 40.0,
            "leading icon slot must be 24..=40 px wide; got {}",
            layout.icon_rect.width()
        );
        assert!(
            layout.kbd_chip_rect.width() >= 48.0,
            "Cmd+K chip needs at least 48 px; got {}",
            layout.kbd_chip_rect.width()
        );
        assert!(
            layout.kbd_chip_rect.x1 < 640.0,
            "kbd chip must be inside the row"
        );
        assert!(
            layout.input_rect.x0 > layout.icon_rect.x1,
            "input must start after the icon"
        );
        assert!(
            layout.input_rect.x1 < layout.kbd_chip_rect.x0,
            "input must end before the kbd chip"
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
        assert_eq!(cmd.row_height(), CMD_ROW_HEIGHT);
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
