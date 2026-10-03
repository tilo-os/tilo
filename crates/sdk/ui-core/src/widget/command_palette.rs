//! `CommandPalette` primitive — a searchable command list that narrows as you type.
//!
//! Enter invokes the selected entry's `on_select` with the CRDT handle and
//! root path directly — there is no message layer.

use crate::search::ListIndex;
use crate::widget::{AppModel, ModelPath};
use automerge::{AutoCommit, ObjId};
use std::collections::HashSet;
use std::sync::Arc;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use tilo_search::FieldWeight;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::icons::{self, Icon};
use crate::paint::PaintCtx;
use crate::platform::{PlatformConventions, Shortcut};
use crate::presence::PresenceCtx;
use crate::query_text::{QueryComposition, QueryEventChange, QueryInk, QueryView};
use crate::text_metrics::centered_text_top;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::ON_CARD_SMALL_TEXT_CONTRAST;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use tilo_actions::{ActionAddress, ActionInput, ControlInvocation, FlatAction, PROMPTED_PARAM};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum SearchScope {
    Workspace,
    All,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Filetype {
    Docs,
    Code,
    Image,
    Video,
}

impl Filetype {
    pub const ALL: [Filetype; 4] = [
        Filetype::Docs,
        Filetype::Code,
        Filetype::Image,
        Filetype::Video,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Filetype::Docs => "DOCS",
            Filetype::Code => "CODE",
            Filetype::Image => "IMG",
            Filetype::Video => "VID",
        }
    }
}

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
    pub fn toggle_full_text(&mut self) {
        self.full_text = !self.full_text;
    }

    pub fn toggle_filetype(&mut self, ft: Filetype) {
        if !self.filetypes.remove(&ft) {
            self.filetypes.insert(ft);
        }
    }

    pub fn set_scope(&mut self, scope: SearchScope) {
        self.scope = scope;
    }
}

type SelectFn = dyn Fn(&mut AutoCommit, &ObjId) + Send + Sync;

/// When set on an entry, choosing it first collects a free-text argument
/// before it completes: the palette switches into an argument-entry step
/// showing `prompt` as a heading and an editable field seeded with `prefill`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArgumentSpec {
    pub prompt: String,
    pub placeholder: String,
    pub prefill: Option<String>,
}

/// The result of pressing Enter on an entry: its `id` plus the free-text
/// `argument` (only `Some` for entries built with
/// [`CommandEntry::with_argument`]). Drained via [`CommandPalette::take_outcome`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutcome {
    pub id: String,
    pub argument: Option<String>,
}

/// Which chrome the palette wears.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PaletteMode {
    /// Scope + filetype chips under the prompt, single-line result rows.
    /// The default; the shell's Cmd+K search overlay.
    Search,
    /// No filter chips; taller rows show a per-entry description and an
    /// optional keybinding hint. The reusable Ctrl+K command palette.
    Commands,
}

#[derive(Clone)]
pub struct CommandEntry {
    pub id: String,
    pub label: String,
    /// Rendered under the label in [`PaletteMode::Commands`]. `None` → label only.
    pub description: Option<String>,
    /// Short shell-rendered status shown as a trailing pill. Unlike a
    /// keybinding it is descriptive and is announced as part of the row.
    pub badge: Option<String>,
    /// Rendered right-aligned in the row. Purely informational — the palette
    /// never binds it.
    pub keybinding: Option<String>,
    /// Preferred semantic form of a shortcut. When present, it is rendered
    /// with the host platform's native command modifier and notation.
    pub shortcut: Option<Shortcut>,
    pub argument: Option<ArgumentSpec>,
    /// Consecutive entries sharing a group render under one header; `None`
    /// joins whatever section precedes it.
    pub group: Option<String>,
    /// Real row count for this group before any host-side cap, so a capped
    /// section can report what it's hiding instead of the painted count.
    pub group_total: Option<usize>,
    pub icon: Option<Icon>,
    /// The application's own artwork, painted in place of `icon` when it
    /// decodes.
    pub image: Option<icons::AppImage>,
    pub on_select: Option<Arc<SelectFn>>,
    action_address: Option<ActionAddress>,
    action_argument: Option<String>,
}

impl std::fmt::Debug for CommandEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandEntry")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("description", &self.description)
            .field("badge", &self.badge)
            .field("keybinding", &self.keybinding)
            .field("shortcut", &self.shortcut)
            .field("argument", &self.argument)
            .field("group", &self.group)
            .field("group_total", &self.group_total)
            .field("icon", &self.icon)
            .field("image", &self.image)
            .field("on_select", &self.on_select.as_ref().map(|_| "<fn>"))
            .field("action_address", &self.action_address)
            .field("action_argument", &self.action_argument)
            .finish()
    }
}

impl CommandEntry {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
            badge: None,
            keybinding: None,
            shortcut: None,
            argument: None,
            group: None,
            group_total: None,
            icon: None,
            image: None,
            on_select: None,
            action_address: None,
            action_argument: None,
        }
    }

    #[must_use]
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }

    #[must_use]
    pub fn badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    #[must_use]
    pub fn group(mut self, g: impl Into<String>) -> Self {
        self.group = Some(g.into());
        self
    }

    #[must_use]
    pub fn group_total(mut self, total: usize) -> Self {
        self.group_total = Some(total);
        self
    }

    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    #[must_use]
    pub fn image(mut self, image: icons::AppImage) -> Self {
        self.image = Some(image);
        self
    }

    #[must_use]
    pub fn keybinding(mut self, k: impl Into<String>) -> Self {
        self.keybinding = Some(k.into());
        self
    }

    /// Adds a platform-formatted shortcut hint. Prefer this to
    /// [`Self::keybinding`] for commands using the native command modifier.
    #[must_use]
    pub fn shortcut(mut self, shortcut: Shortcut) -> Self {
        self.shortcut = Some(shortcut);
        self
    }

    pub fn keybinding_label(&self, platform: PlatformConventions) -> Option<String> {
        self.shortcut
            .as_ref()
            .map(|shortcut| shortcut.label(platform))
            .or_else(|| self.free_text_keycaps().map(|caps| caps.join("+")))
    }

    fn keycap_labels(&self, platform: PlatformConventions) -> Option<Vec<String>> {
        self.shortcut
            .as_ref()
            .map(|shortcut| shortcut.keycap_labels(platform))
            .or_else(|| self.free_text_keycaps())
    }

    /// A free-text keybinding (`Super+H`), one cap per `+`-separated part,
    /// each in the legends of the active keyboard layout.
    fn free_text_keycaps(&self) -> Option<Vec<String>> {
        let keycaps = crate::keycaps::active();
        self.keybinding.as_ref().map(|binding| {
            binding
                .split('+')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(|part| keycaps.token(part))
                .collect()
        })
    }

    #[must_use]
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

    /// Prefill is fully selected when the argument step opens, so the first
    /// keystroke replaces it.
    #[must_use]
    pub fn with_argument_prefill(mut self, prefill: impl Into<String>) -> Self {
        let prefill = prefill.into();
        match &mut self.argument {
            Some(spec) => spec.prefill = Some(prefill),
            None => {
                self.argument = Some(ArgumentSpec {
                    prefill: Some(prefill),
                    ..Default::default()
                });
            }
        }
        self
    }

    #[must_use]
    pub fn on_select<F>(mut self, f: F) -> Self
    where
        F: Fn(&mut AutoCommit, &ObjId) + Send + Sync + 'static,
    {
        self.on_select = Some(Arc::new(f));
        self
    }

    /// The row `id` is the flattened composite key, so the host resolves the
    /// selection back to a replayable `tilo_actions::Action`; a
    /// [`ActionInput::Prompted`] action becomes an argument-collecting row.
    pub fn from_flat_action(fa: &FlatAction<'_>) -> Self {
        let mut entry = CommandEntry::new(
            fa.key.clone(),
            format!("{} — {}", fa.resource_label, fa.action.label),
        );
        if let Some(desc) = &fa.action.description {
            entry = entry.description(desc.clone());
        }
        if let ActionInput::Prompted {
            prompt,
            placeholder,
            ..
        } = &fa.action.input
        {
            entry = entry.with_argument(prompt.clone(), placeholder.clone());
            entry.action_argument = Some(PROMPTED_PARAM.to_string());
        }
        entry.action_address = Some(ActionAddress::new(&fa.resource_id.0, &fa.action.id));
        entry
    }
}

/// Shorten `text` by eating its middle, keeping both ends readable — for
/// paths, where the head (root) and tail (own folder) both carry more
/// information than the middle. Trailing-ellipsis case is
/// [`TextRenderer::elide_to_width`].
fn elide_middle(tr: &mut TextRenderer, text: &str, size: f32, max_w: f64) -> String {
    if max_w <= 0.0 || f64::from(tr.measure(text, size, None).0) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    for drop in 1..n {
        let keep = n.saturating_sub(drop);
        let tail = keep.saturating_mul(2) / 3;
        let head = keep.saturating_sub(tail);
        let Some(head_chars) = chars.get(..head) else {
            continue;
        };
        let Some(tail_chars) = chars.get(n.saturating_sub(tail)..) else {
            continue;
        };
        let candidate: String = head_chars.iter().collect::<String>()
            + "\u{2026}"
            + &tail_chars.iter().collect::<String>();
        if f64::from(tr.measure(&candidate, size, None).0) <= max_w {
            return candidate;
        }
    }
    "\u{2026}".to_string()
}

/// Narrows a layout/measurement value to `f32` for vello/taffy. Every call
/// site is a pixel size or offset inside this card (row widths, card
/// dimensions), always many orders of magnitude below `f32::MAX`, so the
/// truncation the lint warns about can't actually occur here.
#[inline]
#[allow(
    clippy::cast_possible_truncation,
    reason = "screen-space pixel values in this widget never approach f32::MAX"
)]
fn px(v: f64) -> f32 {
    v as f32
}

/// Shared by `draw` and the layout tests so they can't disagree.
#[derive(Clone, Debug, PartialEq)]
pub struct InputRowLayout {
    pub icon_rect: Rect,
    pub input_rect: Rect,
}

/// While `Some`, keyboard input edits `buffer` instead of the query, and the
/// result list is frozen.
struct ArgState {
    entry_id: String,
    prompt: String,
    placeholder: String,
    buffer: String,
    selection: crate::text_selection::Selection,
    composition: QueryComposition,
}

/// A predicate narrowing which [`CommandEntry`] rows are shown.
type EntryFilter = Box<dyn Fn(&CommandEntry) -> bool + Send + Sync>;

/// A filter chip's colours in one state: its body fill, an optional
/// hairline edge, and its label ink.
#[derive(Clone, Copy)]
struct FilterChipLook {
    body: Color,
    edge: Option<Color>,
    text: Color,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent, orthogonal display/behavior toggle \
              (focus, modal trap, fuzzy bypass, and two host-strip display \
              modes) set individually by callers — bundling them into a \
              bitflag or sub-struct would not make any call site clearer"
)]
pub struct CommandPalette {
    entries: Vec<CommandEntry>,
    /// Labels and descriptions of `entries`, indexed while they stay the
    /// same.
    entry_index: ListIndex,
    query: String,
    /// Hint shown while the main query is empty. Hosts use this to name the
    /// resource being searched (files, commands, applications, ...).
    query_placeholder: String,
    query_selection: crate::text_selection::Selection,
    query_composition: QueryComposition,
    filter: Option<EntryFilter>,
    visible: Vec<usize>,
    cursor: usize,
    rect: Rect,
    text_scale: f64,
    is_focused: bool,
    search_filter: SearchFilter,
    mode: PaletteMode,
    /// Traps host keyboard focus while open (see [`Widget::traps_focus`]).
    /// Default `false`; [`PaletteMode::Commands`] turns it on.
    modal: bool,
    arg_state: Option<ArgState>,
    /// Whether argument entry was opened without a backing list row. Escape
    /// closes such a dedicated prompt instead of revealing an unrelated list.
    direct_argument: bool,
    outcome: Option<CommandOutcome>,
    /// Native palette rows enqueue an inert address+arguments request here.
    /// The application host re-resolves it against the fresh resource tree,
    /// exactly like a bound Button; the palette never executes app code.
    action_invocation: crate::control_custody::ControlHolder,
    /// Skip the built-in fuzzy filter when `true`, for provider hosts whose
    /// pre-matched result set should show verbatim.
    bypass_fuzzy: bool,
    /// Suppress the palette's built-in filter strip (and its reserved
    /// height) so a host rendering its own strip doesn't duplicate it.
    hide_filter_strip: bool,
    /// The host paints the filter band itself; the palette still reserves
    /// the band in its own layout but draws nothing there.
    host_filter_strip: bool,
    /// Chip rows the host's band is reserving — the palette doesn't know
    /// how many controls the host has, so this is negotiated, not fixed.
    host_filter_strip_rows: usize,
    /// Rebuilt whenever the entry set, query or filter changes. Every
    /// geometry accessor reads this so hit-testing, scrolling and painting
    /// cannot disagree.
    rows: Vec<VisualRow>,
    /// Last pointer position in logical coordinates. The palette paints its
    /// rows itself, so it owns hover hit testing rather than asking an
    /// overlay host to translate row clicks back into a cursor update.
    last_cursor: Point,
    platform: PlatformConventions,
}

/// One painted line in the result list. Headers are not selectable and are
/// skipped by the cursor.
#[derive(Clone, Debug, PartialEq)]
enum VisualRow {
    /// `total` may exceed the number of entries painted for this group,
    /// when the host capped it.
    Header { label: String, total: usize },
    /// Index into `self.visible`.
    Entry(usize),
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandPalette {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            entry_index: ListIndex::default(),
            query: String::new(),
            query_placeholder: crate::i18n!("palette-placeholder"),
            query_selection: crate::text_selection::Selection::collapsed(0),
            query_composition: QueryComposition::new(),
            filter: None,
            visible: Vec::new(),
            cursor: 0,
            rect: Rect::ZERO,
            text_scale: 1.0,
            is_focused: false,
            search_filter: SearchFilter::default(),
            mode: PaletteMode::Search,
            modal: false,
            arg_state: None,
            direct_argument: false,
            outcome: None,
            action_invocation: crate::control_custody::ControlHolder::new(
                crate::control_custody::ControlProducerDiscipline::ReplaceOne,
            ),
            bypass_fuzzy: false,
            hide_filter_strip: false,
            host_filter_strip: false,
            host_filter_strip_rows: 1,
            rows: Vec::new(),
            last_cursor: Point::ZERO,
            platform: PlatformConventions::default(),
        }
    }

    /// [`PaletteMode::Commands`] chrome with focus trapping enabled.
    pub fn commands() -> Self {
        let mut p = Self::new();
        p.mode = PaletteMode::Commands;
        p.modal = true;
        p
    }

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

    pub fn set_modal(&mut self, modal: bool) {
        self.modal = modal;
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.visible.clear();
        self.cursor = 0;
    }

    /// Clears the query and any in-progress argument, focuses the palette,
    /// and resets the highlight to the first entry.
    pub fn open(&mut self) {
        self.is_focused = true;
        self.query.clear();
        self.query_selection = crate::text_selection::Selection::collapsed(0);
        self.query_composition.clear();
        self.arg_state = None;
        self.direct_argument = false;
        self.outcome = None;
        self.action_invocation.clear();
        self.cursor = 0;
        self.rebuild();
    }

    /// Open directly in the argument-entry step without first presenting a
    /// selectable row. This is useful for dedicated shortcuts such as
    /// "Go to line", while retaining the palette's shared text selection,
    /// clipboard and IME behavior.
    /// Internal compatibility path for toolkit-owned, non-application UI.
    /// Tilo applications cannot use this to manufacture an outcome that
    /// bypasses the resource/action catalogue; use [`Self::open_action_argument`].
    #[cfg(test)]
    fn open_argument(
        &mut self,
        id: impl Into<String>,
        prompt: impl Into<String>,
        placeholder: impl Into<String>,
        prefill: Option<String>,
    ) {
        let buffer = prefill.unwrap_or_default();
        self.is_focused = true;
        self.outcome = None;
        self.direct_argument = true;
        self.arg_state = Some(ArgState {
            entry_id: id.into(),
            prompt: prompt.into(),
            placeholder: placeholder.into(),
            selection: crate::text_selection::Selection {
                anchor: 0,
                cursor: buffer.len(),
            },
            buffer,
            composition: QueryComposition::new(),
        });
    }

    /// Open a resource-backed prompted action directly at its argument step.
    /// Keyboard shortcuts can therefore reuse the exact same typed action row
    /// as Ctrl+K without manufacturing a private outcome id.
    pub fn open_action_argument(&mut self, entry: CommandEntry, prefill: Option<String>) {
        let Some(spec) = entry.argument.clone() else {
            self.set_entries(vec![entry]);
            self.open();
            return;
        };
        debug_assert!(entry.action_address.is_some());
        let buffer = prefill.or(spec.prefill).unwrap_or_default();
        let id = entry.id.clone();
        self.set_entries(vec![entry]);
        self.is_focused = true;
        self.outcome = None;
        self.action_invocation.clear();
        self.direct_argument = true;
        self.arg_state = Some(ArgState {
            entry_id: id,
            prompt: spec.prompt,
            placeholder: spec.placeholder,
            selection: crate::text_selection::Selection {
                anchor: 0,
                cursor: buffer.len(),
            },
            buffer,
            composition: QueryComposition::new(),
        });
    }

    /// Discards any in-progress argument entry but keeps registered entries,
    /// so the next `open` reuses them.
    pub fn close(&mut self) {
        self.is_focused = false;
        self.arg_state = None;
        self.direct_argument = false;
        self.query_composition.clear();
    }

    pub fn is_open(&self) -> bool {
        self.is_focused
    }

    pub fn in_argument_mode(&self) -> bool {
        self.arg_state.is_some()
    }

    pub fn argument_buffer(&self) -> Option<&str> {
        self.arg_state.as_ref().map(|a| a.buffer.as_str())
    }

    /// Returns `Some` at most once per selection.
    pub fn take_outcome(&mut self) -> Option<CommandOutcome> {
        self.outcome.take()
    }

    /// Bypasses taffy, for apps that hand-position their overlays.
    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// Text scale used by layout and host-painted filter controls.
    pub fn text_scale(&self) -> f64 {
        self.text_scale
    }

    /// Matches the size `build_layout` requests from taffy.
    pub fn desired_size(&self) -> (f64, f64) {
        // Sized off the row plan, not a row count, since headers and entries
        // have different heights; clamped so a big hit count doesn't grow
        // the card taller than the screen.
        let list = if self.rows.is_empty() {
            EMPTY_STATE_HEIGHT * self.text_scale
        } else {
            self.rows_height()
                .clamp(self.row_height(), self.max_list_height())
        };
        let height = PADDING * 2.0
            + (INPUT_HEIGHT * self.text_scale)
            + PADDING
            + self.filter_block()
            + list
            + PADDING
            + (FOOTER_HEIGHT * self.text_scale);
        (self.card_width(), height)
    }

    /// Counts what a host capped away, falling back to the painted count
    /// with no host totals — the footer must agree with the section headers.
    pub fn result_total(&self) -> usize {
        if self
            .rows
            .iter()
            .any(|r| matches!(r, VisualRow::Header { .. }))
        {
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

    fn rows_height(&self) -> f64 {
        self.rows.iter().map(|r| self.visual_row_height(r)).sum()
    }

    fn visual_row_height(&self, row: &VisualRow) -> f64 {
        match row {
            VisualRow::Header { .. } => HEADER_HEIGHT * self.text_scale,
            VisualRow::Entry(_) => self.row_height(),
        }
    }

    /// Adds room only for the headers the plan actually has — reserving it
    /// unconditionally would let an ungrouped list paint an extra row into
    /// slack meant for something that isn't there.
    #[allow(
        clippy::cast_precision_loss,
        reason = "both counts are tiny UI row/header caps (<=8 and \
                  <=MAX_HEADERS_IN_VIEW), far below f64's exact-integer range"
    )]
    fn max_list_height(&self) -> f64 {
        let headers = self
            .rows
            .iter()
            .filter(|r| matches!(r, VisualRow::Header { .. }))
            .count()
            .min(MAX_HEADERS_IN_VIEW);
        Self::max_visible_rows() as f64 * self.row_height()
            + headers as f64 * (HEADER_HEIGHT * self.text_scale)
    }

    fn filter_block(&self) -> f64 {
        if self.hide_filter_strip {
            return 0.0;
        }
        // A host-painted strip still occupies the band, so the host isn't
        // left to invent its own rect outside the card.
        if self.host_filter_strip {
            return self.filter_strip_height() + PADDING;
        }
        match self.mode {
            PaletteMode::Search => (FILTER_STRIP_HEIGHT * self.text_scale) + PADDING,
            PaletteMode::Commands => 0.0,
        }
    }

    /// One height for the whole list, not per row — mixed row heights read
    /// as ragged even when each row is individually well-proportioned.
    fn row_height(&self) -> f64 {
        let any_subtitle = self
            .visible
            .iter()
            .any(|&i| self.entries.get(i).is_some_and(|e| e.description.is_some()));
        match self.mode {
            PaletteMode::Search if !any_subtitle => ROW_HEIGHT * self.text_scale,
            _ => TWO_LINE_ROW_HEIGHT * self.text_scale,
        }
    }

    fn card_width(&self) -> f64 {
        match self.mode {
            PaletteMode::Search => SEARCH_CARD_WIDTH * self.text_scale,
            PaletteMode::Commands => CMD_CARD_WIDTH * self.text_scale,
        }
    }

    /// Section headers get their own allowance on top; see
    /// [`Self::max_list_height`].
    fn max_visible_rows() -> usize {
        8
    }

    /// Exactly what Enter does; hosts also wire pointer clicks here, so
    /// click and Enter share one path.
    pub fn activate_selected(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if self.arg_state.is_some() {
            return;
        }
        let Some(&idx) = self.visible.get(self.cursor) else {
            return;
        };
        let Some(entry) = self.entries.get(idx) else {
            return;
        };
        let id = entry.id.clone();
        let argument = entry.argument.clone();
        if let Some(spec) = argument {
            let buffer = spec.prefill.clone().unwrap_or_default();
            let selection = crate::text_selection::Selection {
                anchor: 0,
                cursor: buffer.len(),
            };
            self.arg_state = Some(ArgState {
                entry_id: id,
                prompt: spec.prompt,
                placeholder: spec.placeholder,
                buffer,
                selection,
                composition: QueryComposition::new(),
            });
            self.direct_argument = false;
        } else {
            self.run_entry(state, path, id, None);
        }
    }

    /// Fires `on_select` (if any), records the outcome, and closes the
    /// palette in Commands mode only (Search mode stays open).
    fn run_entry(
        &mut self,
        state: &mut AutoCommit,
        path: &ObjId,
        id: String,
        argument: Option<String>,
    ) {
        if let Some(entry) = self.entries.iter().find(|e| e.id == id)
            && let Some(address) = entry.action_address.clone()
        {
            let mut arguments = serde_json::Map::new();
            if let (Some(name), Some(value)) = (&entry.action_argument, argument) {
                arguments.insert(name.clone(), serde_json::Value::String(value));
            }
            self.action_invocation
                .emit(ControlInvocation { address, arguments });
            self.outcome = None;
            self.arg_state = None;
            if self.mode == PaletteMode::Commands {
                self.is_focused = false;
            }
            return;
        }
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

    /// All keys are consumed so nothing leaks to the app behind.
    /// One immediate-mode key-routing step for the argument-entry mode; the
    /// full parameter list is the same key/state/path threading every other
    /// event handler in this file needs, so splitting it up would only
    /// fragment one cohesive state transition.
    fn handle_arg_key(
        &mut self,
        event: &WindowEvent,
        key: &winit::keyboard::Key,
        shift: bool,
        ctrl: bool,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) {
        // Everything past this point consumes the key.
        *handled = true;
        match key {
            Key::Named(NamedKey::Enter) => {
                let Some(arg) = self.arg_state.as_ref() else {
                    return;
                };
                let (id, value) = (arg.entry_id.clone(), arg.buffer.clone());
                self.run_entry(state, path, id, Some(value));
                return;
            }
            Key::Named(NamedKey::Escape) => {
                self.arg_state = None;
                if self.direct_argument {
                    self.is_focused = false;
                    self.direct_argument = false;
                }
                return;
            }
            // No list navigation while typing an argument.
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                return;
            }
            _ => {}
        }
        let Some(arg) = self.arg_state.as_mut() else {
            return;
        };
        arg.composition
            .handle_event(event, &mut arg.buffer, &mut arg.selection, shift, ctrl);
    }

    pub fn search_filter(&self) -> &SearchFilter {
        &self.search_filter
    }
    pub fn search_filter_mut(&mut self) -> &mut SearchFilter {
        &mut self.search_filter
    }

    pub fn push(&mut self, e: CommandEntry) {
        self.entries.push(e);
        self.rebuild();
    }

    /// Resets the cursor to 0.
    pub fn set_entries(&mut self, entries: Vec<CommandEntry>) {
        self.entries = entries;
        self.cursor = 0;
        self.rebuild();
    }

    /// Every entry passes through unless [`Self::search_filter`] rejects it.
    /// For a provider host doing its own matching, so hits whose labels
    /// don't literally spell the query aren't hidden by fuzzy scoring.
    pub fn set_bypass_fuzzy(&mut self, on: bool) {
        self.bypass_fuzzy = on;
        self.rebuild();
    }

    /// For hosts rendering their own chip strip — without this, both strips
    /// paint and the palette looks doubled up.
    pub fn set_hide_filter_strip(&mut self, on: bool) {
        self.hide_filter_strip = on;
    }

    /// See `Self::host_filter_strip`.
    pub fn set_host_filter_strip(&mut self, on: bool) {
        self.host_filter_strip = on;
        if on {
            self.hide_filter_strip = false;
        }
    }

    /// The card grows by exactly `rows` and results start below it, rather
    /// than a host with too many controls painting over results. Returns
    /// whether the reservation changed.
    pub fn set_host_filter_strip_rows(&mut self, rows: usize) -> bool {
        let rows = rows.max(1);
        let changed = self.host_filter_strip_rows != rows;
        self.host_filter_strip_rows = rows;
        changed
    }

    /// Shared by every band of the card — input field, filter strip, rows,
    /// footer.
    pub fn content_inset_px(&self) -> f64 {
        PADDING
    }

    pub fn set_query(&mut self, q: &str) {
        self.query = q.into();
        // Caret pinned to end so subsequently typed characters append
        // rather than insert mid-string.
        let end = self.query.len();
        self.query_selection = crate::text_selection::Selection::collapsed(end);
        self.query_composition.clear();
        self.rebuild();
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn set_query_placeholder(&mut self, placeholder: impl Into<String>) {
        self.query_placeholder = placeholder.into();
    }

    pub fn set_filter(&mut self, f: Box<dyn Fn(&CommandEntry) -> bool + Send + Sync>) {
        self.filter = Some(f);
        self.rebuild();
    }

    pub fn visible_entries(&self) -> Vec<&CommandEntry> {
        self.visible
            .iter()
            .filter_map(|&i| self.entries.get(i))
            .collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Clamps silently so overlay callers hit-testing post-layout rects
    /// don't have to bounds-check.
    pub fn set_cursor(&mut self, index: usize) {
        if self.visible.is_empty() {
            self.cursor = 0;
        } else {
            self.cursor = index.min(self.visible.len().saturating_sub(1));
        }
    }

    /// `None` if the filter matched no rows.
    pub fn selected_id(&self) -> Option<&str> {
        self.visible
            .get(self.cursor)
            .and_then(|&i| self.entries.get(i))
            .map(|e| e.id.as_str())
    }

    fn visual_index_of(&self, entry_i: usize) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| matches!(r, VisualRow::Entry(e) if *e == entry_i))
    }

    /// Half-open range of visual rows currently painted, scrolled just far
    /// enough to keep the cursor's row inside the card.
    fn painted_window(&self) -> (usize, usize) {
        let budget = (self.footer_rect().y0 - PADDING - self.list_top_y()).max(0.0);
        let target = self.visual_index_of(self.cursor).unwrap_or(0);
        let mut start = 0usize;
        loop {
            let mut used = 0.0;
            let mut end = start;
            while let Some(row) = self.rows.get(end) {
                let h = self.visual_row_height(row);
                if used + h > budget {
                    break;
                }
                used += h;
                end = end.saturating_add(1);
            }
            if target < end || end >= self.rows.len() || start.saturating_add(1) >= self.rows.len()
            {
                // Pull the header back in rather than stranding its rows.
                if start > 0
                    && used + HEADER_HEIGHT * self.text_scale <= budget
                    && self
                        .rows
                        .get(start.saturating_sub(1))
                        .is_some_and(|r| matches!(r, VisualRow::Header { .. }))
                {
                    start = start.saturating_sub(1);
                }
                return (start, end.max(start));
            }
            start = start.saturating_add(1);
        }
    }

    /// `None` when the row is scrolled out of the currently painted window.
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
        let window = self.rows.get(start..target)?;
        for r in window {
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

    /// Shared by `row_rect` and `draw`.
    fn list_top_y(&self) -> f64 {
        self.rect.y0 + PADDING + (INPUT_HEIGHT * self.text_scale) + PADDING + self.filter_block()
    }

    pub fn row_at(&self, p: Point) -> Option<usize> {
        (0..self.visible.len()).find(|&i| self.row_rect(i).is_some_and(|r| r.contains(p)))
    }

    /// The ordinal in the synthetic list is deliberately relative to the
    /// currently painted rows. This keeps the semantic tree bounded exactly
    /// like painting while each row still reports its absolute result
    /// position to assistive technology.
    fn painted_entry_at(&self, ordinal: usize) -> Option<usize> {
        (0..self.visible.len())
            .filter(|&index| self.row_rect(index).is_some())
            .nth(ordinal)
    }

    fn palette_result_target(entry_id: &str) -> String {
        format!("command-palette:result:{entry_id}")
    }

    fn visible_entry_for_target(&self, target: &str) -> Option<usize> {
        self.visible
            .iter()
            .position(|&entry_index| {
                self.entries
                    .get(entry_index)
                    .is_some_and(|entry| target == Self::palette_result_target(&entry.id))
            })
            .filter(|&visible_index| self.row_rect(visible_index).is_some())
    }

    fn handle_pointer_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
            if let Some(row) = self.row_at(self.last_cursor) {
                self.set_cursor(row);
                *handled = true;
            }
            return true;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && let Some(row) = self.row_at(self.last_cursor)
        {
            self.set_cursor(row);
            self.activate_selected(state, path);
            *handled = true;
            return true;
        }
        false
    }

    /// `rounded-3xl` = 24 px (Tailwind).
    pub fn surface_radius_px(&self) -> f64 {
        24.0
    }

    pub fn compute_input_layout(&self, row: Rect) -> InputRowLayout {
        const ICON_W: f64 = 24.0;
        const EDGE: f64 = 12.0;
        const GAP: f64 = 10.0;
        let cy = f64::midpoint(row.y0, row.y1);
        let icon_y0 = cy - ICON_W / 2.0;
        let icon_rect = Rect::new(
            row.x0 + EDGE,
            icon_y0,
            row.x0 + EDGE + ICON_W,
            icon_y0 + ICON_W,
        );
        // The icon is an ornament on one control, not a second control
        // flanking a boxed input — the text slot runs inside the same field.
        let input_rect = Rect::new(icon_rect.x1 + GAP, row.y0, row.x1 - EDGE, row.y1);
        InputRowLayout {
            icon_rect,
            input_rect,
        }
    }

    pub fn input_row_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PADDING,
            self.rect.y0 + PADDING,
            self.rect.x1 - PADDING,
            self.rect.y0 + PADDING + (INPUT_HEIGHT * self.text_scale),
        )
    }

    /// One `FILTER_STRIP_HEIGHT` row tall unless a host asked for more
    /// ([`Self::set_host_filter_strip_rows`]).
    pub fn filter_strip_rect(&self) -> Rect {
        let row = self.input_row_rect();
        let y0 = row.y1 + PADDING;
        Rect::new(row.x0, y0, row.x1, y0 + self.filter_strip_height())
    }

    pub fn filter_strip_row_height(&self) -> f64 {
        FILTER_STRIP_HEIGHT * self.text_scale
    }

    pub fn host_filter_strip_rows(&self) -> usize {
        if self.host_filter_strip {
            self.host_filter_strip_rows.max(1)
        } else {
            1
        }
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "a host-declared filter-strip row count, realistically a \
                  handful of rows, far below f64's exact-integer range"
    )]
    fn filter_strip_height(&self) -> f64 {
        (FILTER_STRIP_HEIGHT * self.text_scale) * self.host_filter_strip_rows() as f64
    }

    pub fn footer_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PADDING,
            self.rect.y1 - PADDING - (FOOTER_HEIGHT * self.text_scale),
            self.rect.x1 - PADDING,
            self.rect.y1 - PADDING,
        )
    }

    /// Draws either the in-progress argument buffer (with its prompt
    /// heading) or the plain query, whichever step the palette is in.
    fn draw_query_text(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: &InputRowLayout,
    ) {
        let colors = &theme.tokens.colors;
        let text_size = theme.tokens.typography.scale.base;
        let mut tx = layout.input_rect.x0;
        let style = theme.tokens.typography.style(TextRole::Body);
        if let Some(arg) = &mut self.arg_state {
            if !arg.prompt.is_empty() {
                let heading = format!("{}:  ", arg.prompt);
                // Centre the heading on its measured line box, not the font size.
                let (hw, hh) = text_renderer.measure(&heading, text_size, None);
                let text_y = centered_text_top(
                    layout.input_rect.y0,
                    layout.input_rect.height(),
                    f64::from(hh),
                );
                text_renderer.draw_text(
                    scene,
                    &heading,
                    text_size,
                    colors.text_dim,
                    transform.pre_translate(Vec2::new(tx, text_y)),
                    None,
                );
                tx += f64::from(hw);
            }
            arg.composition.draw(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                QueryView {
                    text: &arg.buffer,
                    selection: &arg.selection,
                    placeholder: &arg.placeholder,
                    show_caret: self.is_focused,
                },
                Rect::new(
                    tx,
                    layout.input_rect.y0,
                    layout.input_rect.x1,
                    layout.input_rect.y1,
                ),
                &style,
                QueryInk::accented(colors.text_primary, colors.text_dim, colors.accent[5]),
            );
        } else {
            self.query_composition.draw(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                QueryView {
                    text: &self.query,
                    selection: &self.query_selection,
                    placeholder: &self.query_placeholder,
                    show_caret: self.is_focused,
                },
                layout.input_rect,
                &style,
                QueryInk::accented(colors.text_primary, colors.text_dim, colors.accent[5]),
            );
        }
    }

    /// `rounded-full` pill, `px-2 py-1` (≈ 12 px horizontal padding).
    fn chip_rect(strip: Rect, x: f64, width: f64) -> Rect {
        let h = (strip.height() - 8.0).max(20.0);
        let cy = f64::midpoint(strip.y0, strip.y1);
        Rect::new(x, cy - h / 2.0, x + width, cy + h / 2.0)
    }

    /// Active = accent fill + primary text; idle = white/5 fill + dim text.
    fn draw_filter_strip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let strip = self.filter_strip_rect();
        let chip_text_size = (theme.tokens.typography.scale.sm - 1.0).max(10.0);

        let active = FilterChipLook {
            body: theme.tokens.colors.accent[5],
            edge: None,
            text: theme.tokens.colors.text_primary,
        };
        // Idle scope chips are Selectors (HIG §9.5): no fill, outlined by a
        // hairline derived against the panel ground they sit on — a white
        // veneer disappeared on a light palette, and a base-relative lift
        // reads as a well on a panel ground darker than `raised`.
        let idle = FilterChipLook {
            body: Color::TRANSPARENT,
            edge: Some(crate::surface::edge_on(crate::chrome::panel_ground(theme))),
            text: theme.tokens.colors.text_dim,
        };
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);

        let mut x = strip.x0;
        for (scope, label) in [
            (SearchScope::Workspace, "WORKSPACE"),
            (SearchScope::All, "ALL"),
        ] {
            let (w, _) = ctx.text.measure(label, chip_text_size, None);
            let chip_w = f64::from(w) + 16.0;
            let chip = Self::chip_rect(strip, x, chip_w);
            let look = if self.search_filter.scope == scope {
                active
            } else {
                idle
            };
            Self::draw_chip(&mut ctx, chip, label, chip_text_size, look);
            x += chip_w + 6.0;
        }

        let ft_label = "FULL TEXT";
        let (ft_w, _) = ctx.text.measure(ft_label, chip_text_size, None);
        let ft_chip_w = f64::from(ft_w) + 16.0;
        let ft_chip = Self::chip_rect(strip, strip.x1 - ft_chip_w, ft_chip_w);
        let ft_on = self.search_filter.full_text;
        // "On" is a semantic state the theme defines (HIG §9.4), not a
        // Tailwind emerald that ignores the palette.
        let success = theme.tokens.colors.semantic.success;
        let ft_active = FilterChipLook {
            body: success.with_alpha_factor(0.2),
            edge: None,
            text: success,
        };
        Self::draw_chip(
            &mut ctx,
            ft_chip,
            ft_label,
            chip_text_size,
            if ft_on { ft_active } else { idle },
        );

        // Filetype chips fill the gap up to 8 px before the full-text toggle.
        let mut fx = x;
        let max_x = ft_chip.x0 - 8.0;
        for ft in Filetype::ALL {
            let label = ft.label();
            let (w, _) = ctx.text.measure(label, chip_text_size, None);
            let chip_w = f64::from(w) + 16.0;
            if fx + chip_w > max_x {
                break;
            }
            let chip = Self::chip_rect(strip, fx, chip_w);
            let look = if self.search_filter.filetypes.contains(&ft) {
                active
            } else {
                idle
            };
            Self::draw_chip(&mut ctx, chip, label, chip_text_size, look);
            fx += chip_w + 6.0;
        }
    }

    fn draw_chip(
        ctx: &mut PaintCtx<'_>,
        chip: Rect,
        label: &str,
        text_size: f32,
        look: FilterChipLook,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            ..
        } = ctx.reborrow();
        // `rounded-full` ≈ half-height radius.
        let radius = chip.height() / 2.0;
        let shape = RoundedRect::from_rect(chip, radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            look.body,
            None,
            &shape,
        );
        if let Some(edge) = look.edge {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                edge,
                None,
                &crate::surface::edge_outline(chip, radius),
            );
        }
        let (w, h) = text_renderer.measure(label, text_size, None);
        let tx = f64::midpoint(chip.x0, chip.x1) - f64::from(w) / 2.0;
        let ty = f64::midpoint(chip.y0, chip.y1) - f64::from(h) / 2.0;
        text_renderer.draw_text(
            scene,
            label,
            text_size,
            look.text,
            transform.pre_translate(Vec2::new(tx, ty)),
            None,
        );
    }

    fn draw_footer(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        let footer = self.footer_rect();
        // HIG §9.5: a separator inside a Panel derives against the panel
        // ground and is inset `spacing.s3`, never full-bleed into the radius.
        let ground = crate::chrome::panel_ground(theme);
        // Centred on the half pixel so the 1 px rule fills the footer's
        // first pixel row instead of smearing across two at half strength.
        crate::surface::separator(
            scene,
            transform,
            footer.x0,
            footer.x1,
            footer.y0 + 0.5,
            f64::from(theme.tokens.spacing.s3),
            ground,
        );
        // Key chips are static hints resting on the panel: no fill, just a
        // hairline derived against the panel ground (HIG §9.5) — never a
        // white veneer that is invisible on a light palette, and not a
        // base-relative lift that reads as a well on a darker ground.
        let chip_edge = crate::surface::edge_on(ground);

        let size = (theme.tokens.typography.scale.xs).max(9.0);
        let dim = colors.text_dim;
        let cy = f64::midpoint(footer.y0, footer.y1);
        let chip_h = (footer.height() - 12.0).max(13.0);

        let mut x = footer.x0 + 6.0;
        let activate = match self.mode {
            PaletteMode::Search => "palette-hint-open",
            PaletteMode::Commands => "palette-hint-run",
        };
        let hints = [
            ("\u{2191}\u{2193}", crate::i18n!("palette-hint-navigate")),
            ("\u{21b5}", crate::i18n!(activate)),
            ("Esc", crate::i18n!("palette-hint-close")),
        ];
        for (key, label) in &hints {
            let (kw, kh) = text_renderer.measure(key, size, None);
            let chip_w = (f64::from(kw) + 10.0).max(chip_h);
            let chip_rect = Rect::new(x, cy - chip_h / 2.0, x + chip_w, cy + chip_h / 2.0);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                chip_edge,
                None,
                &crate::surface::edge_outline(chip_rect, 4.0),
            );
            text_renderer.draw_text(
                scene,
                key,
                size,
                dim,
                transform.pre_translate(Vec2::new(
                    chip_rect.x0 + (chip_rect.width() - f64::from(kw)) / 2.0,
                    chip_rect.y0 + (chip_rect.height() - f64::from(kh)) / 2.0,
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
                transform.pre_translate(Vec2::new(x, cy - f64::from(lh) / 2.0)),
                None,
            );
            x += f64::from(lw) + 12.0;
        }

        let n = self.result_total();
        if n > 0 {
            let right = crate::i18n!("palette-result-count", "count" => n);
            let (sw, sh) = text_renderer.measure(&right, size, None);
            text_renderer.draw_text(
                scene,
                &right,
                size,
                dim,
                transform.pre_translate(Vec2::new(
                    footer.x1 - f64::from(sw) - 6.0,
                    cy - f64::from(sh) / 2.0,
                )),
                None,
            );
        }
    }

    /// One renderer for both modes.
    fn draw_result_rows(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        let header_size = (theme.tokens.typography.scale.xs).max(9.0);
        let rh = self.row_height();
        let x0 = self.rect.x0 + PADDING;
        let x1 = self.rect.x1 - PADDING;
        let (start, end) = self.painted_window();
        let mut y = self.list_top_y();

        let Some(window) = self.rows.get(start..end) else {
            return;
        };
        for row in window {
            match row {
                VisualRow::Header { label, total } => {
                    let (_, height) = text_renderer.measure(label, header_size, None);
                    let hy = y + (HEADER_HEIGHT * self.text_scale - f64::from(height)) / 2.0;
                    let upper = label.to_uppercase();
                    text_renderer.draw_text(
                        scene,
                        &upper,
                        header_size,
                        colors.text_dim,
                        transform.pre_translate(Vec2::new(x0 + 12.0, hy)),
                        None,
                    );
                    let count = total.to_string();
                    let (cw, _) = text_renderer.measure(&count, header_size, None);
                    text_renderer.draw_text(
                        scene,
                        &count,
                        header_size,
                        colors.text_dim,
                        transform.pre_translate(Vec2::new(x1 - 12.0 - f64::from(cw), hy)),
                        None,
                    );
                    y += HEADER_HEIGHT * self.text_scale;
                }
                VisualRow::Entry(entry_i) => {
                    let row_rect = Rect::new(x0, y, x1, y + rh);
                    if let Some(entry) = self
                        .visible
                        .get(*entry_i)
                        .and_then(|&idx| self.entries.get(idx))
                    {
                        let selected = *entry_i == self.cursor;
                        self.draw_one_row(
                            &mut PaintCtx::new(scene, transform, text_renderer, theme),
                            row_rect,
                            entry,
                            selected,
                        );
                    }
                    y += rh;
                }
            }
        }
    }

    /// A selected row is the card plus an accent wash; ink chosen against
    /// the bare card would read a tier lower there. Resolve the composite
    /// once so paint and contrast checks agree.
    pub(crate) fn row_ground(theme: &Theme, selected: bool) -> Color {
        let ground = crate::chrome::panel_ground(theme);
        if selected {
            crate::tokens::colors::over(ground, theme.tokens.colors.accent[5], SELECTION_WASH)
        } else {
            ground
        }
    }

    /// The accent fill + stroke + left bar painted under a selected row.
    fn draw_selection_highlight(
        scene: &mut Scene,
        transform: Affine,
        row: Rect,
        colors: &crate::tokens::Colors,
        radius: f64,
    ) {
        let shape = RoundedRect::from_rect(row, radius);
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

    /// Paints the row's leading icon, if any, and returns where the label
    /// text should start.
    fn row_icon_rect(row: Rect) -> Rect {
        let size = 16.0;
        let cy = f64::midpoint(row.y0, row.y1);
        Rect::new(
            row.x0 + 12.0,
            cy - size / 2.0,
            row.x0 + 12.0 + size,
            cy + size / 2.0,
        )
    }

    /// The row's application artwork; the x its label starts at, or `None`
    /// when the artwork has no pixels and the glyph must stand in.
    fn draw_row_image(
        scene: &mut Scene,
        transform: Affine,
        row: Rect,
        image: &icons::AppImage,
    ) -> Option<f64> {
        let icon_rect = Self::row_icon_rect(row);
        icons::draw_app_image(scene, transform, icon_rect, image).then_some(icon_rect.x1 + 10.0)
    }

    fn draw_row_icon(
        scene: &mut Scene,
        transform: Affine,
        row: Rect,
        icon: Icon,
        colors: &crate::tokens::Colors,
        selected: bool,
    ) -> f64 {
        let icon_rect = Self::row_icon_rect(row);
        let tint = if selected {
            colors.text_primary
        } else {
            colors.text_dim
        };
        icons::draw_icon(scene, transform, icon_rect, icon, tint, 1.5);
        icon_rect.x1 + 10.0
    }

    /// Paints the row's trailing decoration — a keybinding chip or an
    /// argument-entry cue, mutually exclusive — and returns the x the label
    /// text must stay clear of.
    fn draw_row_trailing(
        &self,
        ctx: &mut PaintCtx<'_>,
        row: Rect,
        entry: &CommandEntry,
        selected: bool,
    ) -> f64 {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let colors = &theme.tokens.colors;
        let (label_size, _) = Self::row_text_sizes(theme);
        // Reserved before measuring the text, so a long title is ellipsised
        // rather than painted under the chip.
        if let Some(badge) = &entry.badge {
            let badge_size = label_size - 2.0;
            let (text_w, text_h) = text_renderer.measure(badge, badge_size, None);
            let pill_w = f64::from(text_w) + 16.0;
            let pill_h = f64::from(text_h) + 8.0;
            let cy = f64::midpoint(row.y0, row.y1);
            let pill = Rect::new(
                row.x1 - pill_w - 8.0,
                cy - pill_h / 2.0,
                row.x1 - 8.0,
                cy + pill_h / 2.0,
            );
            let shape = RoundedRect::from_rect(pill, pill_h / 2.0);
            let fill = colors.accent[5].with_alpha_factor(if selected { 0.28 } else { 0.16 });
            scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                colors.accent[5].with_alpha_factor(0.55),
                None,
                &shape,
            );
            let ink = colors.ink_on(
                Self::row_ground(theme, selected),
                colors.accent[5],
                ON_CARD_SMALL_TEXT_CONTRAST,
            );
            text_renderer.draw_text(
                scene,
                badge,
                badge_size,
                ink,
                transform.pre_translate(Vec2::new(pill.x0 + 8.0, cy - f64::from(text_h) / 2.0)),
                None,
            );
            pill.x0 - 8.0
        } else if let Some(labels) = entry.keycap_labels(self.platform) {
            let chip_w =
                crate::widget::InputHint::measure_keycap_labels(&labels, text_renderer, theme);
            let chip_h = 26.0;
            let cy = f64::midpoint(row.y0, row.y1);
            let chip = Rect::new(
                row.x1 - chip_w - 8.0,
                cy - chip_h / 2.0,
                row.x1 - 8.0,
                cy + chip_h / 2.0,
            );
            crate::widget::InputHint::paint_keycap_labels_at(
                scene,
                transform,
                Point::new(chip.x0, chip.y0),
                &labels,
                text_renderer,
                theme,
            );
            chip.x0 - 8.0
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
                transform.pre_translate(Vec2::new(
                    row.x1 - f64::from(hw) - 14.0,
                    f64::midpoint(row.y0, row.y1) - f64::from(hh) / 2.0,
                )),
                None,
            );
            row.x1 - f64::from(hw) - 22.0
        } else {
            row.x1 - 12.0
        }
    }

    /// Type sizes of a result row's title and of its description line.
    fn row_text_sizes(theme: &Theme) -> (f32, f32) {
        let scale = &theme.tokens.typography.scale;
        (scale.sm, scale.xs.max(10.0))
    }

    fn draw_one_row(
        &self,
        ctx: &mut PaintCtx<'_>,
        row: Rect,
        entry: &CommandEntry,
        selected: bool,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let colors = &theme.tokens.colors;
        let (label_size, desc_size) = Self::row_text_sizes(theme);
        let row_ground = Self::row_ground(theme, selected);
        // Needs contrast against the selection wash: take the nearest
        // legible step from the theme's dim tone rather than promoting it
        // to the title's weight to survive.
        let subtitle_ink = colors.ink_on(row_ground, colors.text_dim, ON_CARD_SMALL_TEXT_CONTRAST);
        if selected {
            Self::draw_selection_highlight(
                scene,
                transform,
                row,
                colors,
                f64::from(theme.tokens.radii.md),
            );
        }

        let mut text_x = row.x0 + 12.0;
        let image_x = entry
            .image
            .as_ref()
            .and_then(|image| Self::draw_row_image(scene, transform, row, image));
        if let Some(x) = image_x {
            text_x = x;
        } else if let Some(icon) = entry.icon {
            text_x = Self::draw_row_icon(scene, transform, row, icon, colors, selected);
        }

        let text_right = self.draw_row_trailing(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            row,
            entry,
            selected,
        );
        let avail = (text_right - text_x).max(0.0);

        if let Some(desc) = &entry.description {
            let label = text_renderer.elide_to_width(&entry.label, label_size, px(avail));
            let sub = elide_middle(text_renderer, desc, desc_size, avail);
            let (_, label_height) = text_renderer.measure(&label, label_size, None);
            let (_, desc_height) = text_renderer.measure(&sub, desc_size, None);
            let gap = 3.0 * f64::from(theme.text_scale);
            let block = f64::from(label_height + desc_height) + gap;
            let ly = row.y0 + (row.height() - block) / 2.0;
            text_renderer.draw_text(
                scene,
                &label,
                label_size,
                colors.text_primary,
                transform.pre_translate(Vec2::new(text_x, ly)),
                None,
            );
            text_renderer.draw_text(
                scene,
                &sub,
                desc_size,
                subtitle_ink,
                transform.pre_translate(Vec2::new(text_x, ly + f64::from(label_height) + gap)),
                None,
            );
        } else {
            let label = text_renderer.elide_to_width(&entry.label, label_size, px(avail));
            let (_, height) = text_renderer.measure(&label, label_size, None);
            let ly = row.y0 + (row.height() - f64::from(height)) / 2.0;
            text_renderer.draw_text(
                scene,
                &label,
                label_size,
                colors.text_primary,
                transform.pre_translate(Vec2::new(text_x, ly)),
                None,
            );
        }
    }

    /// Painted in the list area so the card keeps its shape instead of
    /// collapsing to a bare input with a footer stuck to it.
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
        let cx = f64::midpoint(self.rect.x0, self.rect.x1);
        let cy = f64::midpoint(top, bottom);
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
        let title = self.empty_results_title();
        let title = text_renderer.elide_to_width(&title, title_size, px(self.rect.width() - 48.0));
        let (tw, title_height) = text_renderer.measure(&title, title_size, None);
        text_renderer.draw_text(
            scene,
            &title,
            title_size,
            colors.text_primary,
            transform.pre_translate(Vec2::new(cx - f64::from(tw) / 2.0, cy)),
            None,
        );
        let hint_size = (theme.tokens.typography.scale.xs).max(10.0);
        let hint = crate::i18n!("palette-no-results-hint");
        let hint = hint.as_str();
        let (hw, _) = text_renderer.measure(hint, hint_size, None);
        text_renderer.draw_text(
            scene,
            hint,
            hint_size,
            colors.text_dim,
            transform.pre_translate(Vec2::new(
                cx - f64::from(hw) / 2.0,
                cy + f64::from(title_height) + 6.0,
            )),
            None,
        );
    }

    fn empty_results_title(&self) -> String {
        if self.query.is_empty() {
            crate::i18n!("palette-empty")
        } else {
            crate::i18n!("palette-no-results", "query" => self.query.as_str())
        }
    }

    fn empty_results_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PADDING,
            self.list_top_y(),
            self.rect.x1 - PADDING,
            self.footer_rect().y0,
        )
    }

    fn rebuild(&mut self) {
        self.visible.clear();
        let matching = (!self.bypass_fuzzy).then(|| {
            self.entry_index.ranks(
                &self.query,
                self.entries.iter().map(|entry| {
                    [
                        (entry.label.as_str(), FieldWeight::TITLE),
                        (
                            entry.description.as_deref().unwrap_or_default(),
                            FieldWeight::SUBTITLE,
                        ),
                    ]
                }),
            )
        });
        for (i, e) in self.entries.iter().enumerate() {
            if let Some(f) = &self.filter
                && !f(e)
            {
                continue;
            }
            if matching
                .as_ref()
                .is_some_and(|ranks| ranks.get(i).is_none_or(Option::is_none))
            {
                continue;
            }
            self.visible.push(i);
        }
        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len().saturating_sub(1);
        }
        self.replan_rows();
    }

    fn replan_rows(&mut self) {
        self.rows.clear();
        let mut current: Option<&str> = None;
        for (entry_i, &idx) in self.visible.iter().enumerate() {
            let Some(entry) = self.entries.get(idx) else {
                continue;
            };
            if let Some(group) = entry.group.as_deref()
                && current != Some(group)
            {
                current = Some(group);
                // Fall back to the contiguous run when the host gives no
                // uncapped total.
                let total = entry.group_total.unwrap_or_else(|| {
                    self.visible.get(entry_i..).map_or(0, |rest| {
                        rest.iter()
                            .take_while(|&&j| {
                                self.entries
                                    .get(j)
                                    .is_some_and(|e| e.group.as_deref() == Some(group))
                            })
                            .count()
                    })
                });
                self.rows.push(VisualRow::Header {
                    label: group.to_string(),
                    total,
                });
            }
            self.rows.push(VisualRow::Entry(entry_i));
        }
    }
}

/// Ink drawn on a selected row is measured against the composite this
/// produces — the two values must stay equal.
const SELECTION_WASH: f32 = 0.22;

const ROW_HEIGHT: f64 = 32.0;
const TWO_LINE_ROW_HEIGHT: f64 = 42.0;
const HEADER_HEIGHT: f64 = 24.0;
const MAX_HEADERS_IN_VIEW: usize = 3;
/// Room the composed zero-state needs: glyph, headline, hint.
const EMPTY_STATE_HEIGHT: f64 = 104.0;
const SEARCH_CARD_WIDTH: f64 = 360.0;
const CMD_CARD_WIDTH: f64 = 480.0;
const INPUT_HEIGHT: f64 = 40.0;
const PADDING: f64 = 8.0;
const FILTER_STRIP_HEIGHT: f64 = 36.0;
const FOOTER_HEIGHT: f64 = 28.0;

impl Widget for CommandPalette {
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.platform = conventions;
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.text_scale = f64::from(theme.text_scale);
        // `desired_size` is the single source of truth so a hand-placed
        // palette (`set_rect`) and a taffy-laid one agree.
        let (width, height) = self.desired_size();
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(px(width)),
                height: Dimension::length(px(height)),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
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
        // Theme typography is already scaled. Keep all legacy text paint and
        // metric calls in that same space, restoring the child renderer after.
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        self.text_scale = f64::from(theme.text_scale);
        let colors = &theme.tokens.colors;
        // Panel role (HIG §9.1): panel ground + the derived panel edge as a
        // crisp 1 px inset stroke. The declared `border_standard` sat within
        // rounding distance of `raised` on the dark preset and read as no
        // edge at all.
        let radius = self.surface_radius_px();
        let shape = RoundedRect::from_rect(self.rect, radius);
        crate::floating_surface::paint_material(
            scene,
            transform,
            &shape,
            crate::floating_surface::Material::Floating,
            theme,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::chrome::panel_border(theme),
            None,
            &crate::surface::edge_outline(self.rect, radius),
        );

        // ONE control for the query — the magnifier lives inside its rounded
        // body rather than floating beside it.
        let row_rect = self.input_row_rect();
        let layout = self.compute_input_layout(row_rect);
        let field_shape = RoundedRect::from_rect(row_rect, f64::from(theme.tokens.radii.lg));
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.surfaces.base,
            None,
            &field_shape,
        );
        // A loud focus ring would be permanent noise — this is the
        // palette's only input, focused the whole time it's open — so focus
        // reads as the OS accent at low weight instead.
        let (ring, ring_w) = if self.is_focused {
            (colors.accent[5].with_alpha_factor(0.45), 1.0)
        } else {
            (crate::surface::edge_on(colors.surfaces.base), 1.0)
        };
        scene.stroke(&Stroke::new(ring_w), transform, ring, None, &field_shape);

        // Inset inside its 24 px slot — edge to edge would read as a
        // button, not an ornament.
        icons::draw_icon(
            scene,
            transform,
            layout.icon_rect.inflate(-3.5, -3.5),
            Icon::Search,
            colors.text_dim,
            1.5,
        );

        // No shortcut chip here: a kbd hint invites a chord you've already
        // pressed by now, and two different chords open this palette at two
        // scopes — one label would misname one of them.
        self.draw_query_text(scene, transform, text_renderer, theme, &layout);

        // Skipped when the host paints its own controls into the reserved band.
        if self.mode == PaletteMode::Search && !self.hide_filter_strip && !self.host_filter_strip {
            self.draw_filter_strip(scene, transform, text_renderer, theme);
        }

        if self.visible.is_empty() {
            self.draw_empty_state(scene, transform, text_renderer, theme);
        } else {
            self.draw_result_rows(scene, transform, text_renderer, theme);
        }

        self.draw_footer(scene, transform, text_renderer, theme);
        text_renderer.set_text_scale(renderer_scale);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.is_focused {
            return;
        }
        if self.handle_pointer_event(event, ctx.scale_factor, ctx.state, ctx.path, ctx.handled) {
            return;
        }
        if matches!(event, WindowEvent::Ime(_)) {
            let change = if let Some(arg) = &mut self.arg_state {
                arg.composition.handle_event(
                    event,
                    &mut arg.buffer,
                    &mut arg.selection,
                    false,
                    false,
                )
            } else {
                self.query_composition.handle_event(
                    event,
                    &mut self.query,
                    &mut self.query_selection,
                    false,
                    false,
                )
            };
            if self.arg_state.is_none() && change == QueryEventChange::Edited {
                self.rebuild();
            }
            *ctx.handled = true;
            return;
        }
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: element_state,
                    ..
                },
            ..
        } = event
        {
            if *element_state != ElementState::Pressed {
                return;
            }
            let mods = ctx.modifiers.state();
            let shift = mods.shift_key();
            let ctrl = mods.control_key() || mods.super_key();
            if self.arg_state.is_some() {
                self.handle_arg_key(
                    event,
                    logical_key,
                    shift,
                    ctrl,
                    ctx.state,
                    ctx.path,
                    ctx.handled,
                );
                return;
            }
            match logical_key {
                Key::Named(NamedKey::ArrowDown) => {
                    if !self.visible.is_empty() {
                        self.cursor = self
                            .cursor
                            .saturating_add(1)
                            .min(self.visible.len().saturating_sub(1));
                    }
                    // Consume even at the list end so it never scrolls the app.
                    *ctx.handled = true;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.cursor = self.cursor.saturating_sub(1);
                    *ctx.handled = true;
                }
                Key::Named(NamedKey::Enter) => {
                    self.activate_selected(ctx.state, ctx.path);
                    *ctx.handled = true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.is_focused = false;
                    *ctx.handled = true;
                }
                _ => {
                    let change = self.query_composition.handle_event(
                        event,
                        &mut self.query,
                        &mut self.query_selection,
                        shift,
                        ctrl,
                    );
                    if change == QueryEventChange::Edited {
                        self.rebuild();
                    }
                    if change != QueryEventChange::Ignored {
                        *ctx.handled = true;
                    }
                }
            }
        }
    }

    fn name(&self) -> &'static str {
        "CommandPalette"
    }

    /// `widget_registry` resolves this to the palette's rect; centre-of-rect
    /// lands inside the input row in all supported viewports.
    fn test_id(&self) -> Option<&str> {
        Some("command_palette_input")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.is_open() && self.row_at(point).is_some() {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.arg_state.as_ref().map_or_else(
            || self.query_composition.ime_cursor_rect(),
            |arg| arg.composition.ime_cursor_rect(),
        )
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Some(winit::window::ImePurpose::Normal)
    }

    /// Only traps focus as a modal command launcher; the default Search
    /// palette leaves this `false` so the shell's overlay host drives focus.
    fn traps_focus(&self) -> bool {
        self.modal && self.is_focused
    }

    fn modal_capture(&self) -> Option<Rect> {
        if self.modal && self.is_focused {
            Some(self.rect)
        } else {
            None
        }
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let open = self.is_open();
        let mut node = AccessNode::new(Role::Dialog)
            .with_name(crate::i18n!("palette-name"))
            .with_state(AccessState {
                focusable: self.is_focusable(),
                focused: self.is_focused(),
                // Match the actual capture contract: command chrome opens
                // modal by default, while search chrome is an ordinary
                // overlay. A closed dialog cannot be modal to AT.
                modal: open && self.modal,
                expanded: Some(open),
                hidden: !open,
                ..Default::default()
            });
        if !open {
            return Some(node);
        }

        let query = self
            .arg_state
            .as_ref()
            .map_or((&self.query_placeholder, &self.query), |arg| {
                (&arg.placeholder, &arg.buffer)
            });
        let query_node = AccessNode::new(Role::TextInput)
            .with_action_target("command-palette:query")
            .with_name(query.0.clone())
            .with_value(query.1.clone())
            .with_bounds(self.input_row_rect())
            .with_state(AccessState {
                focusable: true,
                focused: self.is_focused,
                ..Default::default()
            });

        let total = self.visible.len();
        let mut result_children: Vec<AccessNode> = (0..total)
            .filter_map(|visible_index| {
                let rect = self.row_rect(visible_index)?;
                let entry = self
                    .visible
                    .get(visible_index)
                    .and_then(|&entry_index| self.entries.get(entry_index))?;
                let mut item = AccessNode::new(Role::ListItem)
                    .with_action_target(Self::palette_result_target(&entry.id))
                    .with_name(entry.label.clone())
                    .with_position(visible_index.saturating_add(1), total)
                    .with_bounds(rect)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.is_focused && visible_index == self.cursor,
                        selected: visible_index == self.cursor,
                        ..Default::default()
                    });
                if let Some(description) = &entry.description {
                    item = item.with_description(description.clone());
                }
                if let Some(badge) = &entry.badge {
                    let description = entry.description.as_deref().map_or_else(
                        || format!("Status: {badge}"),
                        |detail| format!("{detail}. Status: {badge}"),
                    );
                    item = item.with_description(description);
                }
                Some(item)
            })
            .collect();
        if total == 0 && self.arg_state.is_none() {
            result_children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(self.empty_results_title())
                    .with_description(crate::i18n!("palette-no-results-hint"))
                    .with_bounds(self.empty_results_rect()),
            );
        }
        node = node.with_children(vec![
            query_node,
            AccessNode::new(Role::List)
                .with_name(crate::i18n!("palette-results"))
                .with_bounds(Rect::new(
                    self.rect.x0 + PADDING,
                    self.list_top_y(),
                    self.rect.x1 - PADDING,
                    self.footer_rect().y0,
                ))
                .with_children(result_children),
        ]);
        Some(node)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [0] => Some(self.input_row_rect()),
            [1] => Some(Rect::new(
                self.rect.x0 + PADDING,
                self.list_top_y(),
                self.rect.x1 - PADDING,
                self.footer_rect().y0,
            )),
            [1, 0] if self.visible.is_empty() && self.arg_state.is_none() => {
                Some(self.empty_results_rect())
            }
            [1, ordinal] => self
                .painted_entry_at(*ordinal)
                .and_then(|visible_index| self.row_rect(visible_index)),
            _ => None,
        }
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        match path {
            [0] => {
                self.is_focused = true;
                true
            }
            [1, ordinal] => {
                let Some(visible_index) = self.painted_entry_at(*ordinal) else {
                    return false;
                };
                self.set_cursor(visible_index);
                self.is_focused = true;
                true
            }
            _ => false,
        }
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        match action {
            crate::a11y::Action::Focus | crate::a11y::Action::Activate => {
                self.is_focused = true;
                true
            }
            _ => false,
        }
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        let (state, path) = crate::doc::document(state, path);

        if target == "command-palette:query" {
            return match action {
                Action::Focus | Action::Activate => {
                    self.is_focused = true;
                    true
                }
                // Assistive technology and agents set the query as a value,
                // exactly as typing it would.
                Action::SetValue(value) if self.arg_state.is_none() => {
                    self.is_focused = true;
                    self.set_query(value);
                    true
                }
                _ => false,
            };
        }
        let Some(visible_index) = self.visible_entry_for_target(target) else {
            return false;
        };
        match action {
            Action::Focus => {
                self.set_cursor(visible_index);
                self.is_focused = true;
                true
            }
            Action::Activate => {
                self.set_cursor(visible_index);
                self.is_focused = true;
                self.activate_selected(state, path);
                true
            }
            _ => false,
        }
    }
}

impl crate::widget::controls::ActionControls for CommandPalette {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        let _ = self.action_invocation.install(domain, attachment);
    }
    fn take_control_invocation(&mut self) -> Option<ControlInvocation> {
        self.action_invocation.take()
    }
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        self.entries
            .iter()
            .filter_map(|entry| {
                entry
                    .action_address
                    .clone()
                    .map(|address| crate::widget::ControlBindingMetadata {
                        control_id: entry.id.clone(),
                        kind: crate::widget::ControlBindingKind::Action(address),
                    })
            })
            .collect()
    }
}

impl CommandPalette {
    /// For hosts rendering the palette as a free-floating overlay.
    /// `handle_event` ignores the taffy tree + node id, so a throwaway
    /// single-leaf tree drives the exact same logic.
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
        let node = taffy.leaf_node(Style::default());
        <Self as Widget>::handle_event(
            self,
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers,
                state,
                path,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::TaffyTree;
    use winit::event::Modifiers;
    use winit::keyboard::{KeyLocation, PhysicalKey, SmolStr};

    /// winit 0.30 keeps `KeyEventExtra` `pub(crate)`, so a `KeyEvent` can't
    /// be built through a struct literal — this zero-inits instead.
    fn key_press(logical_key: Key) -> WindowEvent {
        // SAFETY: all pub fields of `KeyEvent` tolerate the all-zero bit
        // pattern; `handle_event` only inspects `logical_key` + `state`.
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
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
    fn palette_card_geometry_rows_and_footer() {
        fn large_text_palette_reserves_painted_rows_and_scales_fonts_once() {
            let mut p = CommandPalette::new();
            p.push(
                CommandEntry::new("files", "Open Files")
                    .description("Opens beside")
                    .group("Applications"),
            );
            p.push(
                CommandEntry::new("editor", "Open Editor")
                    .description("Opens beside")
                    .group("Applications"),
            );
            p.open();
            let theme = Theme::dark().with_text_scale(2.0);
            let mut tree = TaffyTree::new();
            p.build_layout(&mut tree, &theme, &mut std::collections::HashMap::new());
            let (_, height) = p.desired_size();
            p.set_rect(Rect::new(16.0, 96.0, 784.0, 96.0 + height));
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let (_, label_h) =
                tr.measure("Open Files", theme.tokens.typography.scale.sm / 2.0, None);
            let (_, desc_h) =
                tr.measure("Opens beside", theme.tokens.typography.scale.xs / 2.0, None);
            assert!(p.row_height() >= f64::from(label_h + desc_h) + 6.0);
            let mut scene = Scene::new();
            p.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::default(),
            );
            let runs = &scene.encoding().resources.glyph_runs;
            assert!(
                runs.iter()
                    .all(|run| run.font_size <= theme.tokens.typography.scale.base)
            );
            assert!(
                runs.iter()
                    .any(|run| (run.font_size - theme.tokens.typography.scale.sm).abs() < 0.01)
            );
            assert!(p.row_rect(1).unwrap().y1 <= p.footer_rect().y0);
            tr.set_text_scale(1.0); // palette paint's already-scaled theme space
            let mut rows_scene = Scene::new();
            p.draw_result_rows(&mut rows_scene, Affine::IDENTITY, &mut tr, &theme);
            let first = p.row_rect(0).unwrap();
            let second = p.row_rect(1).unwrap();
            let header = Rect::new(first.x0, p.list_top_y(), first.x1, first.y0);
            for run in &rows_scene.encoding().resources.glyph_runs {
                let top = f64::from(run.transform.translation[1]);
                let (_, height) = tr.measure("Ag", run.font_size, None);
                assert!(
                    [header, first, second]
                        .iter()
                        .any(|band| top >= band.y0 - 0.01
                            && top + f64::from(height) <= band.y1 + 0.01),
                    "painted glyph line escapes row/header: y={top}, h={height}"
                );
            }
            let mut footer_scene = Scene::new();
            p.draw_footer(&mut footer_scene, Affine::IDENTITY, &mut tr, &theme);
            for run in &footer_scene.encoding().resources.glyph_runs {
                let top = f64::from(run.transform.translation[1]);
                let (_, height) = tr.measure("Ag", run.font_size, None);
                assert!(
                    top >= p.footer_rect().y0 - 0.01
                        && top + f64::from(height) <= p.footer_rect().y1 + 0.01
                );
            }
        }

        fn palette_card_radius_is_24_px() {
            let palette = CommandPalette::new();
            assert_eq!(
                palette.surface_radius_px(),
                24.0,
                "concept palette uses rounded-3xl = 24 px"
            );
        }

        /// Pins that nothing may reserve a slot for a kbd chip in the query row.
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

        fn row_at_finds_visible_row_inside_the_post_layout_rect() {
            let mut p = CommandPalette::new();
            p.push(CommandEntry::new("a", "Alpha"));
            p.push(CommandEntry::new("b", "Bravo"));
            // Must accommodate input row + filter strip + 2 result rows + footer.
            p.rect = Rect::new(0.0, 0.0, 360.0, 280.0);
            let mid_row_1 = p.row_rect(1).unwrap().center();
            assert_eq!(p.row_at(mid_row_1), Some(1));
            assert!(p.row_at(Point::new(1000.0, 1000.0)).is_none());
        }

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

        large_text_palette_reserves_painted_rows_and_scales_fonts_once();
        palette_card_radius_is_24_px();
        input_row_layout_gives_the_query_the_full_trailing_width();
        row_at_finds_visible_row_inside_the_post_layout_rect();
        footer_sits_below_results_at_card_bottom();
    }

    #[test]
    fn typing_narrows_the_list_with_feedback() {
        fn typing_filters_visible_entries() {
            let mut p = CommandPalette::new();
            p.push(CommandEntry::new("a", "Alpha"));
            p.push(CommandEntry::new("h", "Hotel"));
            p.push(CommandEntry::new("b", "Bravo"));
            p.set_focus(true);

            let total = p.entries.len();
            assert_eq!(p.visible_entries().len(), total);

            let event = key_press(Key::Character(SmolStr::new("h")));
            let mut doc = AutoCommit::new();
            let doc_root = automerge::ROOT.clone();
            let modifiers = Modifiers::default();
            let mut handled = false;
            let mut focus: Option<NodeId> = None;
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let node = taffy.leaf_node(Style::default());

            p.handle_event(
                &event,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &modifiers,
                    state: &mut doc,
                    path: &doc_root,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
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

        fn empty_search_exposes_feedback_and_recovery() {
            let mut p = CommandPalette::new();
            p.open();
            p.set_rect(Rect::new(20.0, 30.0, 380.0, 310.0));
            p.set_query("missing");
            let node = Widget::accessibility(&p).unwrap();
            let results = &node.children[1];
            assert!(results.children.iter().any(|child| child.name.as_deref()
                == Some("No results for “missing”")
                && child.description.as_deref() == Some("Try fewer words, or widen the scope")));
        }

        typing_filters_visible_entries();
        empty_search_exposes_feedback_and_recovery();
    }

    #[test]
    fn cursor_selection_clamps_and_tracks_filtered_entries() {
        fn set_cursor_clamps_to_visible_len() {
            let mut p = CommandPalette::new();
            p.push(CommandEntry::new("a", "Alpha"));
            p.push(CommandEntry::new("b", "Bravo"));
            p.set_cursor(99);
            assert_eq!(p.cursor(), 1);
        }

        fn selected_id_reflects_cursor_position() {
            let mut p = CommandPalette::new();
            p.push(CommandEntry::new("a", "Alpha"));
            p.push(CommandEntry::new("b", "Bravo"));
            assert_eq!(p.selected_id(), Some("a"));
            p.set_cursor(1);
            assert_eq!(p.selected_id(), Some("b"));
        }

        fn selected_id_is_none_when_filter_removes_everything() {
            let mut p = CommandPalette::new();
            p.push(CommandEntry::new("a", "Alpha"));
            p.set_filter(Box::new(|_| false));
            assert_eq!(p.selected_id(), None);
        }

        fn pointer_hover_and_click_select_the_painted_row_through_enter_path() {
            let mut p = CommandPalette::new();
            p.push(CommandEntry::new("alpha", "Alpha"));
            p.push(CommandEntry::new("bravo", "Bravo"));
            p.open();
            p.set_rect(Rect::new(0.0, 0.0, 360.0, 280.0));
            let row = p.row_rect(1).expect("second result is painted");
            let modifiers = Modifiers::default();
            let mut doc = AutoCommit::new();

            assert!(p.handle_overlay_key(
                &WindowEvent::CursorMoved {
                    device_id: unsafe { std::mem::zeroed() },
                    position: winit::dpi::PhysicalPosition::new(row.center().x, row.center().y),
                },
                &modifiers,
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(p.cursor(), 1, "hover highlights the hit-tested row");
            assert_eq!(
                Widget::cursor_shape(&p, row.center()),
                Some(winit::window::CursorIcon::Pointer)
            );
            assert!(p.handle_overlay_key(
                &WindowEvent::MouseInput {
                    device_id: unsafe { std::mem::zeroed() },
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
                &modifiers,
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(
                p.take_outcome(),
                Some(CommandOutcome {
                    id: "bravo".into(),
                    argument: None
                })
            );
        }

        fn semantic_result_focus_and_activation_use_the_same_selected_entry_path() {
            let mut p = CommandPalette::commands();
            p.push(CommandEntry::new("alpha", "Alpha"));
            p.push(CommandEntry::new("bravo", "Bravo"));
            p.open();
            p.set_rect(Rect::new(0.0, 0.0, 480.0, 260.0));
            let target = Widget::accessibility(&p).unwrap().children[1].children[1]
                .action_target
                .clone()
                .expect("result has a stable semantic target");
            let mut doc = AutoCommit::new();

            assert!(Widget::perform_semantic_action(
                &mut p,
                &target,
                &crate::a11y::Action::Focus,
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(p.selected_id(), Some("bravo"));
            assert!(Widget::perform_semantic_action(
                &mut p,
                &target,
                &crate::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(
                p.take_outcome(),
                Some(CommandOutcome {
                    id: "bravo".into(),
                    argument: None
                })
            );
            assert!(!p.is_open(), "command activation closes exactly like Enter");
        }

        set_cursor_clamps_to_visible_len();
        selected_id_reflects_cursor_position();
        selected_id_is_none_when_filter_removes_everything();
        pointer_hover_and_click_select_the_painted_row_through_enter_path();
        semantic_result_focus_and_activation_use_the_same_selected_entry_path();
    }

    #[test]
    fn resource_actions_enqueue_addresses_and_bind_arguments() {
        fn immediate_resource_action_enqueues_an_address_not_a_private_outcome() {
            let spec = tilo_actions::ActionSpec::operation(
                "save",
                "Save",
                tilo_actions::OperationKind::Effect,
                "test.save",
                &(),
            )
            .unwrap();
            let resources = vec![
                tilo_actions::Resource::new("document:1", "document", "Document").with_action(spec),
            ];
            let flat = tilo_actions::flatten(&resources);
            let mut palette = CommandPalette::commands();
            palette.set_entries(vec![CommandEntry::from_flat_action(&flat[0])]);
            palette.open();
            palette.activate_selected(&mut AutoCommit::new(), &automerge::ROOT);

            assert!(palette.take_outcome().is_none());
            let invocation =
                crate::widget::controls::take_control_invocation(&mut palette).unwrap();
            assert_eq!(invocation.address, ActionAddress::new("document:1", "save"));
            assert!(invocation.arguments.is_empty());
        }

        fn prompted_resource_action_binds_its_declared_argument() {
            let spec = tilo_actions::ActionSpec::prompted(
                "rename",
                "Rename",
                "New name",
                "name…",
                tilo_actions::ExecutionLocus::ClientLocal,
                |name| tilo_actions::EncodedAction::encode_operation("test.rename", &name).unwrap(),
            );
            let resources = vec![
                tilo_actions::Resource::new("document:1", "document", "Document").with_action(spec),
            ];
            let flat = tilo_actions::flatten(&resources);
            let entry = CommandEntry::from_flat_action(&flat[0]);
            let mut palette = CommandPalette::commands();
            palette.open_action_argument(entry, Some("Project Atlas".into()));

            let mut handled = false;
            dispatch_h(
                &mut palette,
                &key_press(Key::Named(NamedKey::Enter)),
                Modifiers::default(),
                &mut handled,
            );
            assert!(handled);
            assert!(palette.take_outcome().is_none());
            let invocation =
                crate::widget::controls::take_control_invocation(&mut palette).unwrap();
            assert_eq!(
                invocation.address,
                ActionAddress::new("document:1", "rename")
            );
            assert_eq!(invocation.arguments[PROMPTED_PARAM], "Project Atlas");

            let encoded = flat[0].action.build_json(&invocation.arguments).unwrap();
            assert_eq!(encoded.type_id, "test.rename");
        }

        fn semantic_shortcut_labels_follow_the_palette_platform() {
            use crate::platform::{Platform, PlatformConventions};

            let entry = CommandEntry::new("find", "Find").shortcut(Shortcut::command("F"));
            assert_eq!(
                entry.keybinding_label(PlatformConventions::new(Platform::MacOs)),
                Some("⌘F".into())
            );
            assert_eq!(
                entry.keybinding_label(PlatformConventions::new(Platform::Windows)),
                Some("Ctrl+F".into())
            );
        }

        immediate_resource_action_enqueues_an_address_not_a_private_outcome();
        prompted_resource_action_binds_its_declared_argument();
        semantic_shortcut_labels_follow_the_palette_platform();
    }

    #[test]
    fn search_filters_toggle_scope_and_filetypes() {
        fn search_filter_full_text_toggles() {
            let mut f = SearchFilter::default();
            assert!(!f.full_text);
            f.toggle_full_text();
            assert!(f.full_text);
            f.toggle_full_text();
            assert!(!f.full_text);
        }

        fn search_filter_filetypes_multi_select() {
            let mut f = SearchFilter::default();
            f.toggle_filetype(Filetype::Code);
            f.toggle_filetype(Filetype::Docs);
            assert!(f.filetypes.contains(&Filetype::Code));
            assert!(f.filetypes.contains(&Filetype::Docs));
            f.toggle_filetype(Filetype::Code);
            assert!(!f.filetypes.contains(&Filetype::Code));
            assert!(f.filetypes.contains(&Filetype::Docs));
        }

        fn search_filter_default_scope_is_workspace() {
            let f = SearchFilter::default();
            assert_eq!(f.scope, SearchScope::Workspace);
        }

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

        search_filter_full_text_toggles();
        search_filter_filetypes_multi_select();
        search_filter_default_scope_is_workspace();
        filter_strip_sits_below_input_row_and_above_results();
        a_host_filter_strip_is_reserved_on_the_cards_inset();
        a_host_can_reserve_more_than_one_row_of_filter_band();
    }

    #[test]
    fn query_editing_follows_the_caret_selection_and_ime() {
        fn ime_preedit_is_visible_but_filters_only_after_atomic_commit() {
            let mut palette = CommandPalette::new();
            palette.push(CommandEntry::new("jp", "東京"));
            palette.push(CommandEntry::new("en", "London"));
            palette.set_focus(true);
            palette.rect = Rect::new(0.0, 0.0, 420.0, 260.0);
            let mut document = AutoCommit::new();
            let modifiers = Modifiers::default();

            assert!(palette.handle_overlay_key(
                &WindowEvent::Ime(winit::event::Ime::Preedit(
                    "東京".into(),
                    Some(("東".len(), "東".len())),
                )),
                &modifiers,
                &mut document,
                &automerge::ROOT,
            ));
            assert_eq!(palette.query(), "", "preedit is not committed query state");
            assert_eq!(
                palette.visible_entries().len(),
                2,
                "preedit does not filter"
            );

            let mut scene = Scene::new();
            let mut renderer = TextRenderer::new();
            Widget::draw(
                &mut palette,
                &mut scene,
                Affine::IDENTITY,
                &mut renderer,
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
            let candidate = Widget::ime_cursor_rect(&palette).expect("query publishes IME caret");
            assert!(palette.input_row_rect().contains(candidate.center()));

            assert!(palette.handle_overlay_key(
                &WindowEvent::Ime(winit::event::Ime::Commit("東京".into())),
                &modifiers,
                &mut document,
                &automerge::ROOT,
            ));
            assert_eq!(palette.query(), "東京");
            assert_eq!(palette.visible_entries().len(), 1);
            assert_eq!(palette.visible_entries()[0].label, "東京");
        }

        /// `ArrowLeft` / `ArrowRight` move the caret inside the query without
        /// touching the row cursor.
        fn arrow_keys_move_caret_inside_query() {
            let mut p = CommandPalette::new();
            p.set_focus(true);
            p.set_query("hello");
            assert_eq!(p.query_selection.cursor, 5);
            dispatch(
                &mut p,
                &key_press(Key::Named(NamedKey::ArrowLeft)),
                Modifiers::default(),
            );
            assert_eq!(p.query_selection.cursor, 4);
            dispatch(
                &mut p,
                &key_press(Key::Named(NamedKey::Home)),
                Modifiers::default(),
            );
            assert_eq!(p.query_selection.cursor, 0);
        }

        fn backspace_deletes_at_caret_not_end() {
            let mut p = CommandPalette::new();
            p.set_focus(true);
            p.set_query("abc");
            p.query_selection.cursor = 1;
            p.query_selection.anchor = 1;
            dispatch(
                &mut p,
                &key_press(Key::Named(NamedKey::Backspace)),
                Modifiers::default(),
            );
            assert_eq!(p.query(), "bc");
            assert_eq!(p.query_selection.cursor, 0);
        }

        fn typing_inserts_at_caret() {
            let mut p = CommandPalette::new();
            p.set_focus(true);
            p.set_query("ac");
            p.query_selection.cursor = 1;
            p.query_selection.anchor = 1;
            dispatch(
                &mut p,
                &key_press(Key::Character(SmolStr::new("b"))),
                Modifiers::default(),
            );
            assert_eq!(p.query(), "abc");
            assert_eq!(p.query_selection.cursor, 2);
        }

        fn ctrl_a_selects_query() {
            let mut p = CommandPalette::new();
            p.set_focus(true);
            p.set_query("hello");
            let mut mods_state = winit::keyboard::ModifiersState::empty();
            mods_state.set(winit::keyboard::ModifiersState::CONTROL, true);
            dispatch(
                &mut p,
                &key_press(Key::Character(SmolStr::new("a"))),
                Modifiers::from(mods_state),
            );
            assert_eq!(p.query_selection.anchor, 0);
            assert_eq!(p.query_selection.cursor, 5);
        }

        fn typing_replaces_selection() {
            let mut p = CommandPalette::new();
            p.set_focus(true);
            p.set_query("hello");
            p.query_selection.anchor = 1;
            p.query_selection.cursor = 4;
            dispatch(
                &mut p,
                &key_press(Key::Character(SmolStr::new("i"))),
                Modifiers::default(),
            );
            assert_eq!(p.query(), "hio");
            assert_eq!(p.query_selection.cursor, 2);
        }

        ime_preedit_is_visible_but_filters_only_after_atomic_commit();
        arrow_keys_move_caret_inside_query();
        backspace_deletes_at_caret_not_end();
        typing_inserts_at_caret();
        ctrl_a_selects_query();
        typing_replaces_selection();
    }

    fn dispatch(p: &mut CommandPalette, event: &WindowEvent, modifiers: Modifiers) {
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.leaf_node(Style::default());
        p.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: &mut doc,
                path: &doc_root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
    }

    #[test]
    fn palette_accessibility_reports_open_modal_and_focus() {
        fn accessibility_reports_open_state_modal_mode_and_painted_result_children() {
            use crate::a11y::Role;
            let mut p = CommandPalette::new();
            p.push(CommandEntry::new("alpha", "Alpha"));
            p.push(
                CommandEntry::new("bravo", "Bravo")
                    .description("Second result")
                    .badge("Live"),
            );
            p.set_rect(Rect::new(20.0, 30.0, 380.0, 310.0));

            let closed = Widget::accessibility(&p).expect("palette is semantic while closed");
            assert!(
                closed.state.hidden,
                "closed palettes leave the reading order"
            );
            assert_eq!(closed.state.expanded, Some(false));
            assert!(!closed.state.modal, "search chrome is not modal");
            assert!(
                closed.children.is_empty(),
                "closed palettes expose no stale rows"
            );

            p.open();
            let node = Widget::accessibility(&p).expect("open palette is semantic");
            assert_eq!(node.role, Role::Dialog);
            assert_eq!(node.name.as_deref(), Some("Command palette"));
            assert!(!node.state.hidden);
            assert_eq!(node.state.expanded, Some(true));
            assert!(!node.state.modal, "search chrome is an ordinary overlay");
            assert!(node.state.focusable);
            assert!(node.state.focused, "open palette owns its query focus");
            assert_eq!(node.children.len(), 2, "query plus its result list");
            let query = &node.children[0];
            assert_eq!(query.role, Role::TextInput);
            assert_eq!(query.value.as_deref(), Some(""));
            assert!(
                query.action_target.is_some(),
                "query has a stable synthetic id"
            );
            assert_eq!(query.bounds, Some(p.input_row_rect().into()));

            let results = &node.children[1];
            assert_eq!(results.role, Role::List);
            assert_eq!(
                results.children.len(),
                2,
                "one synthetic item per painted row"
            );
            let selected = &results.children[0];
            assert_eq!(selected.name.as_deref(), Some("Alpha"));
            assert!(selected.state.selected);
            assert_eq!(selected.pos_in_set, Some(1));
            assert_eq!(selected.set_size, Some(2));
            assert_eq!(selected.bounds, p.row_rect(0).map(Into::into));
            assert_eq!(
                results.children[1].description.as_deref(),
                Some("Second result. Status: Live")
            );
            assert_eq!(
                selected.action_target.as_deref(),
                Some("command-palette:result:alpha"),
                "row ids are model-stable rather than paint ordinals"
            );
            assert_eq!(
                p.synthetic_accessibility_rect(&[0]),
                Some(p.input_row_rect())
            );
            assert_eq!(p.synthetic_accessibility_rect(&[1, 1]), p.row_rect(1));

            let mut commands = CommandPalette::commands();
            commands.open();
            assert!(
                Widget::accessibility(&commands).unwrap().state.modal,
                "command chrome advertises its focus trap as modal"
            );
        }

        fn accessibility_focus_tracks_focus_state() {
            let mut p = CommandPalette::new();
            p.set_focus(true);
            let node = Widget::accessibility(&p).unwrap();
            assert!(node.state.focused);
        }

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

        accessibility_reports_open_state_modal_mode_and_painted_result_children();
        accessibility_focus_tracks_focus_state();
        command_mode_traps_focus_search_does_not();
    }

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

    #[test]
    fn command_mode_navigates_filters_and_scrolls() {
        fn command_mode_arrows_and_enter_are_captured_and_navigate() {
            let mut p = seeded_command_palette();
            assert_eq!(p.selected_id(), Some("goto"));

            let mut doc = AutoCommit::new();
            let root = automerge::ROOT.clone();
            for _ in 0..2 {
                let mut handled = false;
                let mut focus = None;
                let mut taffy: TaffyTree<()> = TaffyTree::new();
                let node = taffy.leaf_node(Style::default());
                p.handle_event(
                    &key_press(Key::Named(NamedKey::ArrowDown)),
                    &mut crate::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &Modifiers::default(),
                        state: &mut doc,
                        path: &root,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &taffy,
                    },
                );
                assert!(handled, "ArrowDown must be consumed by the palette");
            }
            assert_eq!(p.selected_id(), Some("export"), "cursor advanced two rows");

            let mut handled = false;
            dispatch_h(
                &mut p,
                &key_press(Key::Named(NamedKey::ArrowUp)),
                Modifiers::default(),
                &mut handled,
            );
            assert!(handled);
            assert_eq!(p.selected_id(), Some("sum"));

            let mut handled = false;
            dispatch_h(
                &mut p,
                &key_press(Key::Named(NamedKey::Enter)),
                Modifiers::default(),
                &mut handled,
            );
            assert!(handled, "Enter is consumed");
            let outcome = p.take_outcome().expect("Enter emits an outcome");
            assert_eq!(outcome.id, "sum");
            assert_eq!(outcome.argument, None);
            assert!(!p.is_open(), "running a command closes the palette");
        }

        fn command_mode_filter_matches_label_and_description() {
            let mut p = seeded_command_palette();
            // "csv" only appears in the Export entry's description.
            p.set_query("csv");
            let ids: Vec<&str> = p.visible_entries().iter().map(|e| e.id.as_str()).collect();
            assert_eq!(ids, vec!["export"], "description text is searchable");

            p.set_query("sum");
            let ids: Vec<&str> = p.visible_entries().iter().map(|e| e.id.as_str()).collect();
            assert_eq!(ids, vec!["sum"]);
        }

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

        fn command_mode_scrolls_to_keep_cursor_visible() {
            let mut p = CommandPalette::commands();
            for i in 0..20 {
                p.push(CommandEntry::new(format!("id{i}"), format!("Command {i}")));
            }
            p.open();
            let (w, h) = p.desired_size();
            p.set_rect(Rect::new(0.0, 0.0, w, h));
            assert!(p.row_rect(0).is_some());
            assert!(
                p.row_rect(CommandPalette::max_visible_rows()).is_none(),
                "row past the cap is scrolled off"
            );

            p.set_cursor(19);
            assert!(
                p.row_rect(19).is_some(),
                "highlighted last row stays visible"
            );
            assert!(p.row_rect(0).is_none(), "the top row scrolled out of view");
        }

        command_mode_arrows_and_enter_are_captured_and_navigate();
        command_mode_filter_matches_label_and_description();
        command_mode_geometry_differs_from_search();
        command_mode_scrolls_to_keep_cursor_visible();
    }

    #[test]
    fn command_argument_prompts_return_values_or_back_out() {
        fn command_mode_argument_flow_returns_typed_value() {
            let mut p = seeded_command_palette();
            assert_eq!(p.selected_id(), Some("goto"));

            let mut handled = false;
            dispatch_h(
                &mut p,
                &key_press(Key::Named(NamedKey::Enter)),
                Modifiers::default(),
                &mut handled,
            );
            assert!(handled);
            assert!(
                p.in_argument_mode(),
                "entry with an argument opens the arg step"
            );
            assert_eq!(p.argument_buffer(), Some("A1"));
            assert!(
                p.take_outcome().is_none(),
                "no outcome until the argument is confirmed"
            );

            for ch in ["B", "1", "2"] {
                let mut h = false;
                dispatch_h(
                    &mut p,
                    &key_press(Key::Character(SmolStr::new(ch))),
                    Modifiers::default(),
                    &mut h,
                );
                assert!(h, "typing in the arg field is consumed");
            }
            assert_eq!(p.argument_buffer(), Some("B12"));

            let mut handled = false;
            dispatch_h(
                &mut p,
                &key_press(Key::Named(NamedKey::Enter)),
                Modifiers::default(),
                &mut handled,
            );
            assert!(handled);
            let outcome = p.take_outcome().expect("confirming emits an outcome");
            assert_eq!(outcome.id, "goto");
            assert_eq!(outcome.argument.as_deref(), Some("B12"));
            assert!(!p.is_open());
        }

        fn direct_argument_mode_skips_the_artificial_command_row() {
            let mut p = CommandPalette::commands();
            p.open_argument(
                "goto-line",
                "Go to line",
                "Line number\u{2026}",
                Some("42".into()),
            );
            assert!(p.is_open());
            assert!(p.in_argument_mode());
            assert_eq!(p.argument_buffer(), Some("42"));
            let mut handled = false;
            dispatch_h(
                &mut p,
                &key_press(Key::Named(NamedKey::Enter)),
                Modifiers::default(),
                &mut handled,
            );
            assert!(handled);
            assert_eq!(
                p.take_outcome(),
                Some(CommandOutcome {
                    id: "goto-line".into(),
                    argument: Some("42".into()),
                })
            );
            assert!(!p.is_open());
        }

        fn escape_closes_a_direct_argument_prompt() {
            let mut p = CommandPalette::commands();
            p.open_argument("goto-line", "Go to line", "Line number…", None);
            let mut handled = false;
            dispatch_h(
                &mut p,
                &key_press(Key::Named(NamedKey::Escape)),
                Modifiers::default(),
                &mut handled,
            );
            assert!(handled);
            assert!(!p.is_open());
            assert!(!p.in_argument_mode());
        }

        fn command_mode_escape_from_argument_returns_to_list() {
            let mut p = seeded_command_palette();
            let mut h = false;
            dispatch_h(
                &mut p,
                &key_press(Key::Named(NamedKey::Enter)),
                Modifiers::default(),
                &mut h,
            );
            assert!(p.in_argument_mode());
            let mut h = false;
            dispatch_h(
                &mut p,
                &key_press(Key::Named(NamedKey::Escape)),
                Modifiers::default(),
                &mut h,
            );
            assert!(h, "Escape is consumed");
            assert!(!p.in_argument_mode(), "Escape leaves the argument step");
            assert!(p.is_open(), "…but the palette is still open on the list");
            assert!(p.take_outcome().is_none());
        }

        command_mode_argument_flow_returns_typed_value();
        direct_argument_mode_skips_the_artificial_command_row();
        escape_closes_a_direct_argument_prompt();
        command_mode_escape_from_argument_returns_to_list();
    }

    #[test]
    fn grouped_results_plan_headers_counts_and_painted_hits() {
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
                    VisualRow::Entry(_) => None,
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
                VisualRow::Entry(i) => panic!("expected a header, got entry row {i}"),
            }
            assert_eq!(
                p.result_total(),
                40,
                "the footer count must not contradict the section header"
            );
        }

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
                    assert!(
                        r.y0 >= p.list_top_y() - 0.01,
                        "row {i} starts above the list"
                    );
                }
            }
        }

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

        grouped_entries_plan_one_header_per_section();
        a_trimmed_section_reports_its_untrimmed_count();
        a_scrolled_list_hit_tests_the_row_it_painted();
        no_painted_row_overlaps_the_footer();
        an_empty_result_set_still_gets_a_readable_card();
    }

    fn dispatch_h(
        p: &mut CommandPalette,
        event: &WindowEvent,
        modifiers: Modifiers,
        handled: &mut bool,
    ) {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT.clone();
        let mut focus: Option<NodeId> = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.leaf_node(Style::default());
        p.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: &mut doc,
                path: &root,
                handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
    }
}
