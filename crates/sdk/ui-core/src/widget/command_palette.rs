//! `CommandPalette` primitive — a searchable command list that narrows as you type.
//!
//! Enter invokes the selected entry's `on_select` with the CRDT handle and
//! root path directly — there is no message layer.

use crate::num::f32_of_f64;
use crate::search::ListIndex;
use crate::widget::ModelPath;
use automerge::ObjId;
use std::collections::HashSet;
use std::sync::Arc;
use taffy::{NodeId, Size, Style, TaffyTree};
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
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST;
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

type SelectFn = dyn Fn(&mut crate::widget::Edits<'_>, &ObjId) + Send + Sync;

/// When set on an entry, choosing it first collects a free-text argument
/// before it completes: the palette switches into an argument-entry step
/// showing `prompt` as a heading and an editable field seeded with `prefill`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArgumentSpec {
    pub prompt: String,
    pub placeholder: String,
    pub prefill: Option<String>,
    /// What the field holds, which decides how it is shown and edited.
    pub kind: ArgumentKind,
}

/// The kind of value an argument step collects.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum ArgumentKind {
    /// Free text.
    #[default]
    Text,
    /// A password or key: painted as one bullet per character, never put
    /// on the clipboard, and reported to assistive technology as protected
    /// with no value.
    Secret,
    /// A number: Up and Down step it by `step` inside `min..=max`.
    Number {
        min: Option<f64>,
        max: Option<f64>,
        step: f64,
    },
}

/// A modifier held while a row is activated, choosing an alternative
/// activation the row offers ([`CommandEntry::alternate`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActivationModifier {
    Shift,
    Ctrl,
    Alt,
}

impl ActivationModifier {
    /// The one modifier `state` holds, or `None` for none or a mix: a chord
    /// of two is not one of the alternatives.
    pub fn of(state: winit::keyboard::ModifiersState) -> Option<Self> {
        match (
            state.shift_key(),
            state.control_key() || state.super_key(),
            state.alt_key(),
        ) {
            (true, false, false) => Some(Self::Shift),
            (false, true, false) => Some(Self::Ctrl),
            (false, false, true) => Some(Self::Alt),
            _ => None,
        }
    }

    /// The key's name, as the shared keycap legends spell it.
    fn key(self) -> &'static str {
        match self {
            Self::Shift => "Shift",
            Self::Ctrl => "Ctrl",
            Self::Alt => "Alt",
        }
    }
}

/// What a row does when it is activated with `modifier` held, in the words
/// the selected row shows while that modifier is down ("Opens as a tab
/// here").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alternate {
    pub modifier: ActivationModifier,
    pub label: String,
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
    /// Other ways to activate the row, each behind a held modifier. The
    /// selected row shows the one whose modifier is down, or the first as a
    /// hint; the host decides what each does.
    pub alternates: Vec<Alternate>,
    /// The description is a path, so it loses its middle rather than its
    /// end when it doesn't fit: the root and the item's own folder carry
    /// more than what lies between them. Prose loses its end.
    pub description_is_path: bool,
    /// The description as separate facts, drawn as runs of their own with
    /// space between them ([`Self::facts`]). Empty: the description is one
    /// run.
    pub description_facts: Vec<String>,
    /// Its value steps with ←/→ while the row is selected and the query's
    /// caret is at its end ([`CommandPalette::take_adjustment`]): a volume,
    /// a timeout, a mode.
    pub adjustable: bool,
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
            .field("alternates", &self.alternates)
            .field("description_is_path", &self.description_is_path)
            .field("description_facts", &self.description_facts)
            .field("adjustable", &self.adjustable)
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
            alternates: Vec::new(),
            description_is_path: false,
            description_facts: Vec::new(),
            adjustable: false,
            action_address: None,
            action_argument: None,
        }
    }

    /// Let ←/→ step the row's value ([`Self::adjustable`]).
    #[must_use]
    pub fn adjustable(mut self) -> Self {
        self.adjustable = true;
        self
    }

    /// Offer another activation behind `modifier` ([`Alternate`]).
    #[must_use]
    pub fn alternate(mut self, modifier: ActivationModifier, label: impl Into<String>) -> Self {
        self.alternates.push(Alternate {
            modifier,
            label: label.into(),
        });
        self
    }

    /// Show the description as separate facts ([`Self::description_facts`]).
    /// Its text for assistive technology and matching is the facts joined
    /// by commas.
    #[must_use]
    pub fn facts(mut self, facts: Vec<String>) -> Self {
        if !facts.is_empty() {
            self.description = Some(facts.join(", "));
        }
        self.description_facts = facts;
        self
    }

    /// Mark the description as a path ([`Self::description_is_path`]).
    #[must_use]
    pub fn path_description(mut self) -> Self {
        self.description_is_path = true;
        self
    }

    /// Ask for a value of `kind` in the argument step.
    #[must_use]
    pub fn argument_kind(mut self, kind: ArgumentKind) -> Self {
        if let Some(spec) = &mut self.argument {
            spec.kind = kind;
        }
        self
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
        let (prefill, kind) = self
            .argument
            .take()
            .map_or((None, ArgumentKind::Text), |s| (s.prefill, s.kind));
        self.argument = Some(ArgumentSpec {
            prompt: prompt.into(),
            placeholder: placeholder.into(),
            prefill,
            kind,
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
        F: Fn(&mut crate::widget::Edits<'_>, &ObjId) + Send + Sync + 'static,
    {
        self.on_select = Some(Arc::new(f));
        self
    }

    /// The row `id` is the flattened composite key, so the host resolves the
    /// selection back to a replayable `tilo_actions::Action`; a
    /// [`ActionInput::Prompted`] action becomes an argument-collecting row
    /// whose text is the action's primary parameter (its first required one,
    /// else its first), as the action's own prompt reads it.
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
            let primary = fa.action.prompt_param().unwrap_or(PROMPTED_PARAM);
            entry = entry.with_argument(prompt.clone(), placeholder.clone());
            entry.action_argument = Some(primary.to_string());
        }
        entry.action_address = Some(ActionAddress::new(&fa.resource_id.0, &fa.action.id));
        entry
    }
}

/// Shorten `text` by eating its middle, keeping both ends readable — for
/// paths, where the head (root) and tail (own folder) both carry more
/// information than the middle. Trailing-ellipsis case is
/// [`TextRenderer::elide_to_width`].
fn elide_middle(tr: &mut TextRenderer, text: &str, style: &TextStyle, max_w: f64) -> String {
    if max_w <= 0.0 || f64::from(tr.measure_style(text, None, style).0) <= max_w {
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
        if f64::from(tr.measure_style(&candidate, None, style).0) <= max_w {
            return candidate;
        }
    }
    "\u{2026}".to_string()
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
    kind: ArgumentKind,
    /// A step the host opened ([`CommandPalette::open_argument_step`]):
    /// Enter hands the value back through
    /// [`CommandPalette::take_submitted_argument`] and Escape reports
    /// [`CommandPalette::take_argument_cancelled`]; neither runs a row.
    host_step: bool,
}

impl ArgState {
    fn new(entry_id: String, spec: ArgumentSpec, prefill: Option<String>, host_step: bool) -> Self {
        let buffer = prefill.or(spec.prefill).unwrap_or_default();
        Self {
            entry_id,
            prompt: spec.prompt,
            placeholder: spec.placeholder,
            selection: crate::text_selection::Selection {
                anchor: 0,
                cursor: buffer.len(),
            },
            buffer,
            composition: QueryComposition::new(),
            kind: spec.kind,
            host_step,
        }
    }

    fn is_secret(&self) -> bool {
        self.kind == ArgumentKind::Secret
    }

    /// What a secret field paints: one bullet per character, with the
    /// caret and selection at the same characters. `None` for any other.
    fn masked(&self) -> Option<(String, crate::text_selection::Selection)> {
        if !self.is_secret() {
            return None;
        }
        let bullets = |byte: usize| {
            MASK.len_utf8()
                .saturating_mul(self.buffer.get(..byte).map_or(0, |s| s.chars().count()))
        };
        Some((
            MASK.to_string().repeat(self.buffer.chars().count()),
            crate::text_selection::Selection {
                anchor: bullets(self.selection.anchor),
                cursor: bullets(self.selection.cursor),
            },
        ))
    }

    /// Up (`direction` 1) or Down (-1) on a number field: the value one
    /// step on, inside its bounds. A field that holds no number starts from
    /// the bound it moves away from, else from zero.
    fn step_number(&mut self, direction: f64) -> bool {
        let ArgumentKind::Number { min, max, step } = self.kind else {
            return false;
        };
        let current = self
            .buffer
            .trim()
            .trim_end_matches('%')
            .trim()
            .parse::<f64>()
            .ok();
        let start = current.unwrap_or_else(|| {
            if direction > 0.0 {
                min.unwrap_or(0.0)
            } else {
                max.unwrap_or(0.0)
            }
        });
        let mut next = if current.is_some() {
            start + direction * step
        } else {
            start
        };
        if let Some(min) = min {
            next = next.max(min);
        }
        if let Some(max) = max {
            next = next.min(max);
        }
        self.buffer = format_number(next, step);
        self.selection = crate::text_selection::Selection::collapsed(self.buffer.len());
        true
    }
}

/// `value` with as many decimals as `step` has, so a whole step never shows
/// a fraction and 0.05 steps never show float noise.
fn format_number(value: f64, step: f64) -> String {
    let mut decimals = 0usize;
    let mut probe = step.abs();
    while decimals < 6 && (probe - probe.round()).abs() > 1e-9 {
        probe *= 10.0;
        decimals = decimals.saturating_add(1);
    }
    format!("{value:.decimals$}")
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
    ui_scale: f64,
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
    /// A ←/→ step asked of the selected adjustable row: its id and -1 or 1.
    adjustment: Option<(String, i8)>,
    /// Native palette rows enqueue an inert address+arguments request here.
    /// The application host re-resolves it against the fresh resource tree,
    /// exactly like a bound Button; the palette never executes app code.
    action_invocation: crate::control_custody::ControlHolder,
    /// Skip the built-in fuzzy filter when `true`, for provider hosts whose
    /// pre-matched result set should show verbatim.
    bypass_fuzzy: bool,
    /// More results are still coming ([`Self::set_searching`]).
    searching: bool,
    /// [`crate::search::engine_generation`] the entries were ranked with:
    /// where the index's code loads on first use, they are ranked again
    /// once it is in.
    ranked_generation: u64,
    /// Suppress the palette's built-in filter strip (and its reserved
    /// height) so a host rendering its own strip doesn't duplicate it.
    hide_filter_strip: bool,
    /// The host paints the filter band itself; the palette still reserves
    /// the band in its own layout but draws nothing there.
    host_filter_strip: bool,
    /// Chip rows the host's band is reserving — the palette doesn't know
    /// how many controls the host has, so this is negotiated, not fixed.
    host_filter_strip_rows: usize,
    /// Show the query field alone while nothing is typed: no filter band
    /// and no list ([`Self::set_field_only_while_empty`]).
    field_only_while_empty: bool,
    /// Rebuilt whenever the entry set, query or filter changes. Every
    /// geometry accessor reads this so hit-testing, scrolling and painting
    /// cannot disagree.
    rows: Vec<VisualRow>,
    /// Last pointer position in logical coordinates. The palette paints its
    /// rows itself, so it owns hover hit testing rather than asking an
    /// overlay host to translate row clicks back into a cursor update.
    last_cursor: Point,
    platform: PlatformConventions,
    /// The first visual row painted. Kept between frames, so moving the
    /// cursor inside the painted window never scrolls it; it only moves
    /// when the cursor leaves the window or the wheel turns.
    scroll_start: std::cell::Cell<usize>,
    /// Tallest the result list may grow before it scrolls; the host knows
    /// the display, the palette does not ([`Self::set_max_list_height`]).
    max_list_height: Option<f64>,
    /// The modifiers held now, so the selected row can show what its
    /// alternative activation would do before it is chosen.
    held: winit::keyboard::ModifiersState,
    /// A host step's confirmed value, until the host takes it.
    submitted_argument: Option<String>,
    /// A host step was backed out of with Escape, until the host notices.
    argument_cancelled: bool,
    /// What the search is narrowed to, shown as a token at the start of the
    /// field ([`Self::set_scope_token`]).
    scope_token: Option<String>,
    /// Backspace at the start of the field took the token away, until the
    /// host notices ([`Self::take_scope_token_removed`]).
    scope_token_removed: bool,
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
            ui_scale: 1.0,
            is_focused: false,
            search_filter: SearchFilter::default(),
            mode: PaletteMode::Search,
            modal: false,
            arg_state: None,
            direct_argument: false,
            outcome: None,
            adjustment: None,
            action_invocation: crate::control_custody::ControlHolder::new(
                crate::control_custody::ControlProducerDiscipline::ReplaceOne,
            ),
            bypass_fuzzy: false,
            searching: false,
            ranked_generation: crate::search::engine_generation(),
            hide_filter_strip: false,
            host_filter_strip: false,
            host_filter_strip_rows: 1,
            field_only_while_empty: false,
            rows: Vec::new(),
            last_cursor: Point::ZERO,
            platform: PlatformConventions::default(),
            scroll_start: std::cell::Cell::new(0),
            max_list_height: None,
            held: winit::keyboard::ModifiersState::empty(),
            submitted_argument: None,
            argument_cancelled: false,
            scope_token: None,
            scope_token_removed: false,
        }
    }

    /// Show what the search is narrowed to ("Notes") as a token at the
    /// start of the field, where the narrowing is read with the query
    /// instead of as a separate control. Backspace at the start of the
    /// field removes it; the host widens the scope
    /// ([`Self::take_scope_token_removed`]).
    pub fn set_scope_token(&mut self, token: Option<String>) {
        self.scope_token = token;
    }

    /// The scope token the field shows, if any.
    pub fn scope_token(&self) -> Option<&str> {
        self.scope_token.as_deref()
    }

    /// Whether Backspace removed the scope token since the last call.
    pub fn take_scope_token_removed(&mut self) -> bool {
        std::mem::take(&mut self.scope_token_removed)
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

    /// Show the query field alone until something is typed, for a host
    /// that opens search over content the person came to look at (the
    /// desktop's Super surface): an empty query lists nothing, so the card
    /// shrinks to its field and the first keystroke grows it into results.
    pub fn set_field_only_while_empty(&mut self, on: bool) {
        self.field_only_while_empty = on;
    }

    /// Whether the card is showing its query field alone right now: an
    /// empty field the host asked to stand alone, or an argument step with
    /// nothing listed under it.
    pub fn shows_field_only(&self) -> bool {
        (self.field_only_while_empty && self.query.is_empty() && self.arg_state.is_none())
            || (self.arg_state.is_some() && self.visible.is_empty())
    }

    /// Leave any argument step without running or reporting anything: the
    /// host has moved on (to another step, or back to the results).
    pub fn cancel_argument(&mut self) {
        self.arg_state = None;
        self.direct_argument = false;
        self.submitted_argument = None;
        self.argument_cancelled = false;
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
        self.submitted_argument = None;
        self.argument_cancelled = false;
        self.action_invocation.clear();
        self.cursor = 0;
        self.scroll_start.set(0);
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
        self.is_focused = true;
        self.outcome = None;
        self.direct_argument = true;
        self.arg_state = Some(ArgState::new(
            id.into(),
            ArgumentSpec {
                prompt: prompt.into(),
                placeholder: placeholder.into(),
                prefill,
                kind: ArgumentKind::Text,
            },
            None,
            false,
        ));
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
        let id = entry.id.clone();
        self.set_entries(vec![entry]);
        self.is_focused = true;
        self.outcome = None;
        self.action_invocation.clear();
        self.direct_argument = true;
        self.arg_state = Some(ArgState::new(id, spec, prefill, false));
    }

    /// Ask for one value in the field, for a host that collects an
    /// action's arguments one step at a time. The list stays where it is
    /// underneath. Enter hands the value to
    /// [`Self::take_submitted_argument`] and Escape sets
    /// [`Self::take_argument_cancelled`]; the host decides what follows.
    /// Neither runs a row, so a step can't stand in for one.
    pub fn open_argument_step(&mut self, id: impl Into<String>, spec: ArgumentSpec) {
        self.is_focused = true;
        self.outcome = None;
        self.submitted_argument = None;
        self.argument_cancelled = false;
        self.direct_argument = false;
        self.arg_state = Some(ArgState::new(id.into(), spec, None, true));
    }

    /// The value a host step was confirmed with, once.
    pub fn take_submitted_argument(&mut self) -> Option<String> {
        self.submitted_argument.take()
    }

    /// Whether a host step was backed out of since the last call.
    pub fn take_argument_cancelled(&mut self) -> bool {
        std::mem::take(&mut self.argument_cancelled)
    }

    /// The modifier held now that names one of the alternatives
    /// ([`ActivationModifier::of`]).
    pub fn held_modifier(&self) -> Option<ActivationModifier> {
        ActivationModifier::of(self.held)
    }

    /// Tell the palette which modifiers are down, for a host that routes
    /// modifier changes itself.
    pub fn set_held_modifiers(&mut self, held: winit::keyboard::ModifiersState) -> bool {
        let changed = self.held != held;
        self.held = held;
        changed
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

    /// The step ←/→ asked of the selected adjustable row since the last
    /// call: its id and the direction (-1 or 1). The host changes the value
    /// and refreshes the row.
    pub fn take_adjustment(&mut self) -> Option<(String, i8)> {
        self.adjustment.take()
    }

    /// ←/→ on a selected adjustable row with the caret at the query's end
    /// steps its value instead of moving the caret, which has nowhere to go
    /// on the right and is not what a person with a value row selected
    /// means on the left.
    fn step_selected(&mut self, key: &Key) -> bool {
        let delta = match key {
            Key::Named(NamedKey::ArrowRight) => 1,
            Key::Named(NamedKey::ArrowLeft) => -1,
            _ => return false,
        };
        let at_end =
            self.query_selection == crate::text_selection::Selection::collapsed(self.query.len());
        let Some(entry) = self
            .visible
            .get(self.cursor)
            .and_then(|&at| self.entries.get(at))
            .filter(|entry| entry.adjustable)
        else {
            return false;
        };
        if !at_end {
            return false;
        }
        self.adjustment = Some((entry.id.clone(), delta));
        true
    }

    /// Bypasses taffy, for apps that hand-position their overlays.
    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// Text scale used by layout and host-painted filter controls.
    pub fn text_scale(&self) -> f64 {
        self.text_scale
    }

    // Theme typography already includes both scales; geometry must reserve
    // the same space without changing the public text-scale cache.
    fn layout_scale(&self) -> f64 {
        self.text_scale * self.ui_scale
    }

    /// Matches the size `build_layout` requests from taffy.
    pub fn desired_size(&self) -> (f64, f64) {
        if self.shows_field_only() {
            return (
                self.card_width(),
                PADDING * 2.0 + INPUT_HEIGHT * self.layout_scale(),
            );
        }
        // Sized off the rows it holds, up to the tallest list the host
        // allows; past that the list scrolls.
        let list = if self.rows.is_empty() {
            EMPTY_STATE_HEIGHT * self.layout_scale()
        } else {
            self.fitted_list_height()
        };
        let height = PADDING
            + (INPUT_HEIGHT * self.layout_scale())
            + PADDING
            + self.filter_block()
            + list
            + PADDING;
        (self.card_width(), height)
    }

    /// The tallest the result list may grow before it scrolls, in logical
    /// pixels. The host sets it from the display it shows the palette on.
    pub fn set_max_list_height(&mut self, height: f64) {
        self.max_list_height = Some(height.max(0.0));
    }

    /// How many result rows a full list holds: what a host asks its sources
    /// for, so the first screen is full and nothing is fetched that only
    /// scrolling could reach.
    pub fn entry_row_capacity(&self) -> usize {
        let row = self.row_height().max(1.0);
        crate::num::usize_of_f64((self.max_list_height() / row).floor()).max(1)
    }

    /// The whole rows from the top that fit under the cap: a cap that cut
    /// a row in half would leave a sliver nothing paints.
    fn fitted_list_height(&self) -> f64 {
        let cap = self.max_list_height().max(self.row_height());
        let mut used = 0.0;
        for row in &self.rows {
            let h = self.visual_row_height(row);
            if used + h > cap + 0.01 {
                break;
            }
            used += h;
        }
        used.max(self.row_height())
    }

    fn visual_row_height(&self, row: &VisualRow) -> f64 {
        match row {
            VisualRow::Header { .. } => HEADER_HEIGHT * self.layout_scale(),
            VisualRow::Entry(_) => self.row_height(),
        }
    }

    /// The host's cap, else ten rows: enough to see past the first group
    /// without the card standing over the whole screen.
    fn max_list_height(&self) -> f64 {
        self.max_list_height
            .unwrap_or_else(|| DEFAULT_LIST_ROWS * self.row_height())
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
            PaletteMode::Search => (FILTER_STRIP_HEIGHT * self.layout_scale()) + PADDING,
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
            PaletteMode::Search if !any_subtitle => ROW_HEIGHT * self.layout_scale(),
            _ => TWO_LINE_ROW_HEIGHT * self.layout_scale(),
        }
    }

    fn card_width(&self) -> f64 {
        match self.mode {
            PaletteMode::Search => SEARCH_CARD_WIDTH * self.layout_scale(),
            PaletteMode::Commands => CMD_CARD_WIDTH * self.layout_scale(),
        }
    }

    /// Exactly what Enter does; hosts also wire pointer clicks here, so
    /// click and Enter share one path.
    pub fn activate_selected(&mut self, state: &mut crate::widget::Edits<'_>, path: &ObjId) {
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
            self.arg_state = Some(ArgState::new(id, spec, None, false));
            self.direct_argument = false;
        } else {
            self.run_entry(state, path, id, None);
        }
    }

    /// Fires `on_select` (if any), records the outcome, and closes the
    /// palette in Commands mode only (Search mode stays open).
    fn run_entry(
        &mut self,
        state: &mut crate::widget::Edits<'_>,
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
        state: &mut crate::widget::Edits<'_>,
        path: &ObjId,
        handled: &mut bool,
    ) {
        // Everything past this point consumes the key.
        *handled = true;
        match key {
            Key::Named(NamedKey::Enter) => {
                let Some(arg) = self.arg_state.as_mut() else {
                    return;
                };
                if arg.host_step {
                    self.submitted_argument = Some(std::mem::take(&mut arg.buffer));
                    self.arg_state = None;
                    return;
                }
                let (id, value) = (arg.entry_id.clone(), arg.buffer.clone());
                self.run_entry(state, path, id, Some(value));
                return;
            }
            Key::Named(NamedKey::Escape) => {
                if self.arg_state.as_ref().is_some_and(|arg| arg.host_step) {
                    self.argument_cancelled = true;
                }
                self.arg_state = None;
                if self.direct_argument {
                    self.is_focused = false;
                    self.direct_argument = false;
                }
                return;
            }
            // No list navigation while typing an argument; a number steps.
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                let up = matches!(key, Key::Named(NamedKey::ArrowUp));
                if let Some(arg) = self.arg_state.as_mut() {
                    arg.step_number(if up { 1.0 } else { -1.0 });
                }
                return;
            }
            // A secret never reaches the clipboard, where every app reads it.
            Key::Character(c)
                if ctrl
                    && matches!(c.as_str(), "c" | "C" | "x" | "X")
                    && self.arg_state.as_ref().is_some_and(ArgState::is_secret) =>
            {
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

    /// Show host feedback on the selected row using the shared status pill and
    /// description. Keep the query, visible order and cursor so an unsuccessful
    /// invocation can be corrected or retried. A new entry set replaces feedback.
    pub fn set_selected_status(
        &mut self,
        badge: impl Into<String>,
        description: impl Into<String>,
    ) {
        let Some(&index) = self.visible.get(self.cursor) else {
            return;
        };
        if let Some(entry) = self.entries.get_mut(index) {
            entry.badge = Some(badge.into());
            entry.description = Some(description.into());
        }
    }

    /// Whether more results are still on their way (an index whose code is
    /// still loading): an empty list then says "Searching…" instead of
    /// "No results".
    pub fn set_searching(&mut self, searching: bool) {
        self.searching = searching;
    }

    /// Every entry passes through unless [`Self::search_filter`] rejects it.
    /// For a provider host doing its own matching, so hits whose labels
    /// don't literally spell the query aren't hidden by fuzzy scoring.
    pub fn set_bypass_fuzzy(&mut self, on: bool) {
        self.bypass_fuzzy = on;
        self.rebuild();
    }

    /// See `Self::host_filter_strip`.
    pub fn set_host_filter_strip(&mut self, on: bool) {
        self.host_filter_strip = on;
        if on {
            self.hide_filter_strip = false;
        }
    }

    /// Show or hide the filter band. A host hides it until the results
    /// have something to tell apart, so it isn't standing chrome over every
    /// search. Returns whether that changed.
    pub fn set_filter_band_shown(&mut self, shown: bool) -> bool {
        let changed = self.hide_filter_strip == shown;
        self.hide_filter_strip = !shown;
        changed
    }

    /// Whether the card reserves its filter band right now.
    pub fn filter_band_shown(&self) -> bool {
        !self.hide_filter_strip && self.filter_block() > 0.0
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

    /// Shared by every band of the card — input field, filter strip and
    /// rows.
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

    /// The visual rows from `start` that fit `budget`, as the end of the
    /// half-open range.
    fn window_end(&self, start: usize, budget: f64) -> usize {
        let mut used = 0.0;
        let mut end = start;
        while let Some(row) = self.rows.get(end) {
            let h = self.visual_row_height(row);
            if used + h > budget + 0.01 {
                break;
            }
            used += h;
            end = end.saturating_add(1);
        }
        end
    }

    /// Half-open range of visual rows currently painted. The window stays
    /// where it was while the cursor is inside it, moves just far enough to
    /// bring the cursor back when it leaves, and fills from above when
    /// rows below have run out. A header is never its window's last row:
    /// a heading with nothing under it reads as a broken list.
    fn painted_window(&self) -> (usize, usize) {
        let budget = (self.list_bottom_y() - self.list_top_y()).max(0.0);
        let len = self.rows.len();
        let target = self.visual_index_of(self.cursor);
        let mut start = self.scroll_start.get().min(len.saturating_sub(1));
        if let Some(t) = target
            && t < start
        {
            // Moving up into a group brings its header along.
            start = if t > 0
                && self
                    .rows
                    .get(t.saturating_sub(1))
                    .is_some_and(|r| matches!(r, VisualRow::Header { .. }))
            {
                t.saturating_sub(1)
            } else {
                t
            };
        }
        let mut end = self.window_end(start, budget);
        if let Some(t) = target {
            while t >= end && start < t {
                start = start.saturating_add(1);
                end = self.window_end(start, budget);
            }
        }
        // Use slack at the bottom for rows above, which also brings back
        // the header of a group whose first row the window starts on.
        while start > 0 && self.window_end(start.saturating_sub(1), budget) >= end {
            start = start.saturating_sub(1);
        }
        if end > start
            && self
                .rows
                .get(end.saturating_sub(1))
                .is_some_and(|r| matches!(r, VisualRow::Header { .. }))
        {
            end = end.saturating_sub(1);
        }
        self.scroll_start.set(start);
        (start, end.max(start))
    }

    /// Scroll the list by `rows` visual rows (down for positive), keeping
    /// the cursor on a painted row so the window stays where it was put.
    fn scroll_by(&mut self, rows: isize) {
        let (start, end) = self.painted_window();
        let span = end.saturating_sub(start);
        let max_start = self.rows.len().saturating_sub(span);
        let next = start.saturating_add_signed(rows).min(max_start);
        if next == start {
            return;
        }
        self.scroll_start.set(next);
        let next_end = self.window_end(next, (self.list_bottom_y() - self.list_top_y()).max(0.0));
        let Some(target) = self.visual_index_of(self.cursor) else {
            return;
        };
        let entry_at = |i: usize| match self.rows.get(i) {
            Some(VisualRow::Entry(entry)) => Some(*entry),
            _ => None,
        };
        if target < next {
            if let Some(entry) = (next..next_end).find_map(entry_at) {
                self.cursor = entry;
            }
        } else if target >= next_end
            && let Some(entry) = (next..next_end).rev().find_map(entry_at)
        {
            self.cursor = entry;
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
        self.rect.y0
            + PADDING
            + (INPUT_HEIGHT * self.layout_scale())
            + PADDING
            + self.filter_block()
    }

    /// The label of the group whose painted header is under `p`: headers
    /// are how a host offers "only these".
    pub fn header_at(&self, p: Point) -> Option<String> {
        if self.shows_field_only() || !self.rect.contains(p) {
            return None;
        }
        let (start, end) = self.painted_window();
        let mut y = self.list_top_y();
        for row in self.rows.get(start..end)? {
            let h = self.visual_row_height(row);
            if let VisualRow::Header { label, .. } = row
                && p.y >= y
                && p.y < y + h
            {
                return Some(label.clone());
            }
            y += h;
        }
        None
    }

    pub fn row_at(&self, p: Point) -> Option<usize> {
        if self.shows_field_only() {
            return None;
        }
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
        state: &mut crate::widget::Edits<'_>,
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

    /// The Panel radius ([`crate::tokens::Radii::panel`]): the palette is a
    /// floating panel.
    pub fn surface_radius_px(theme: &Theme) -> f64 {
        crate::widget::floating_surface::radius(theme)
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
            self.rect.y0 + PADDING + (INPUT_HEIGHT * self.layout_scale()),
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
        FILTER_STRIP_HEIGHT * self.layout_scale()
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
        (FILTER_STRIP_HEIGHT * self.layout_scale()) * self.host_filter_strip_rows() as f64
    }

    /// Where the result list ends: the card's bottom inset.
    fn list_bottom_y(&self) -> f64 {
        self.rect.y1 - PADDING
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
        let text_style = theme.text(TextRole::Body);
        let mut tx = layout.input_rect.x0;
        let style = theme.tokens.typography.style(TextRole::Body);
        if let Some(arg) = &mut self.arg_state {
            if !arg.prompt.is_empty() {
                let heading = format!("{}:  ", arg.prompt);
                // Centre the heading on its measured line box, not the font size.
                let (hw, hh) = text_renderer.measure_style(&heading, None, &text_style);
                let text_y = centered_text_top(
                    layout.input_rect.y0,
                    layout.input_rect.height(),
                    f64::from(hh),
                );
                text_renderer.draw_style(
                    scene,
                    &heading,
                    colors.text_dim,
                    transform.pre_translate(Vec2::new(tx, text_y)),
                    None,
                    &text_style,
                );
                tx += f64::from(hw);
            }
            let rect = Rect::new(
                tx,
                layout.input_rect.y0,
                layout.input_rect.x1,
                layout.input_rect.y1,
            );
            let empty = arg.buffer.is_empty() && !arg.composition.is_composing();
            let masked = arg.masked();
            let (shown, selection) = masked
                .as_ref()
                .map_or((arg.buffer.as_str(), &arg.selection), |(text, sel)| {
                    (text.as_str(), sel)
                });
            arg.composition.draw(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                QueryView {
                    text: shown,
                    selection,
                    placeholder: "",
                    show_caret: self.is_focused,
                },
                rect,
                &style,
                QueryInk::of(colors),
            );
            if empty {
                let placeholder = arg.placeholder.clone();
                Self::draw_placeholder(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    rect,
                    &placeholder,
                    &style,
                    self.is_focused,
                );
            }
        } else {
            let mut input = layout.input_rect;
            if let Some(token) = &self.scope_token {
                input.x0 = Self::draw_scope_token(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    input,
                    token,
                );
            }
            let layout = &InputRowLayout {
                icon_rect: layout.icon_rect,
                input_rect: input,
            };
            let empty = self.query.is_empty() && !self.query_composition.is_composing();
            self.query_composition.draw(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                QueryView {
                    text: &self.query,
                    selection: &self.query_selection,
                    placeholder: "",
                    show_caret: self.is_focused,
                },
                layout.input_rect,
                &style,
                QueryInk::of(colors),
            );
            if empty {
                Self::draw_placeholder(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    layout.input_rect,
                    &self.query_placeholder,
                    &style,
                    self.is_focused,
                );
            }
        }
    }

    /// The scope token at the start of `field`: a selector pill holding
    /// its name. Returns where the query starts after it.
    fn draw_scope_token(ctx: &mut PaintCtx<'_>, field: Rect, token: &str) -> f64 {
        let theme = ctx.theme;
        let style = theme.text(TextRole::Caption);
        let max_w = f32_of_f64((field.width() / 2.0).max(1.0));
        let shown = ctx.text.elide_to_width_style(token, max_w, &style);
        let (w, h) = ctx.text.measure_style(&shown, None, &style);
        let pad = f64::from(theme.tokens.spacing.s2);
        let pill_h = (f64::from(h) + 8.0).min(field.height() - 8.0);
        let cy = field.center().y;
        let pill = Rect::new(
            field.x0,
            cy - pill_h / 2.0,
            field.x0 + f64::from(w) + pad * 2.0,
            cy + pill_h / 2.0,
        );
        let shape = RoundedRect::from_rect(pill, pill_h / 2.0);
        let colors = &theme.tokens.colors;
        ctx.scene.fill(
            vello::peniko::Fill::NonZero,
            ctx.transform,
            colors.intent.selection_tint,
            None,
            &shape,
        );
        ctx.text.draw_style(
            ctx.scene,
            &shown,
            colors.ink_on(
                colors.surfaces.base,
                colors.text_primary,
                SMALL_TEXT_MIN_CONTRAST,
            ),
            ctx.transform
                .pre_translate(Vec2::new(pill.x0 + pad, cy - f64::from(h) / 2.0)),
            None,
            &style,
        );
        pill.x1 + f64::from(theme.tokens.spacing.s1)
    }

    /// The placeholder of an empty field, set off from the caret by the
    /// `s1` step so the caret never stands on its first letter.
    fn draw_placeholder(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        placeholder: &str,
        style: &TextStyle,
        caret: bool,
    ) {
        if placeholder.is_empty() {
            return;
        }
        let theme = ctx.theme;
        let offset = if caret {
            f64::from(theme.tokens.spacing.s1) + 2.0
        } else {
            0.0
        };
        let (_, h) = ctx.text.measure_style(placeholder, None, style);
        let y = centered_text_top(rect.y0, rect.height(), f64::from(h));
        let width = crate::num::f32_of_f64((rect.width() - offset).max(1.0));
        let shown = ctx.text.elide_to_width_style(placeholder, width, style);
        ctx.text.draw_style(
            ctx.scene,
            &shown,
            QueryInk::of(&theme.tokens.colors).placeholder,
            ctx.transform.pre_translate(Vec2::new(rect.x0 + offset, y)),
            None,
            style,
        );
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
        let chip_text_style = theme.text(TextRole::Caption);

        let active = FilterChipLook {
            body: theme.tokens.colors.intent.accent,
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
            let (w, _) = ctx.text.measure_style(label, None, &chip_text_style);
            let chip_w = f64::from(w) + 16.0;
            let chip = Self::chip_rect(strip, x, chip_w);
            let look = if self.search_filter.scope == scope {
                active
            } else {
                idle
            };
            Self::draw_chip(&mut ctx, chip, label, &chip_text_style, look);
            x += chip_w + 6.0;
        }

        let ft_label = "FULL TEXT";
        let (ft_w, _) = ctx.text.measure_style(ft_label, None, &chip_text_style);
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
            &chip_text_style,
            if ft_on { ft_active } else { idle },
        );

        // Filetype chips fill the gap up to 8 px before the full-text toggle.
        let mut fx = x;
        let max_x = ft_chip.x0 - 8.0;
        for ft in Filetype::ALL {
            let label = ft.label();
            let (w, _) = ctx.text.measure_style(label, None, &chip_text_style);
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
            Self::draw_chip(&mut ctx, chip, label, &chip_text_style, look);
            fx += chip_w + 6.0;
        }
    }

    fn draw_chip(
        ctx: &mut PaintCtx<'_>,
        chip: Rect,
        label: &str,
        text_style: &TextStyle,
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
        let (w, h) = text_renderer.measure_style(label, None, text_style);
        let tx = f64::midpoint(chip.x0, chip.x1) - f64::from(w) / 2.0;
        let ty = f64::midpoint(chip.y0, chip.y1) - f64::from(h) / 2.0;
        text_renderer.draw_style(
            scene,
            label,
            look.text,
            transform.pre_translate(Vec2::new(tx, ty)),
            None,
            text_style,
        );
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
        // Section headers are sentence case, a `Subheading` in the
        // secondary ink; the count beside one is a tabular caption.
        let header_style = theme.text(TextRole::Subheading);
        let count_style = theme.text(TextRole::Caption).numeric();
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
                    let (_, height) = text_renderer.measure_style(label, None, &header_style);
                    let hy = y + (HEADER_HEIGHT * self.layout_scale() - f64::from(height)) / 2.0;
                    text_renderer.draw_style(
                        scene,
                        label,
                        colors.text_dim,
                        transform.pre_translate(Vec2::new(x0 + 12.0, hy)),
                        None,
                        &header_style,
                    );
                    let count = total.to_string();
                    let (cw, ch) = text_renderer.measure_style(&count, None, &count_style);
                    let cy = y + (HEADER_HEIGHT * self.layout_scale() - f64::from(ch)) / 2.0;
                    text_renderer.draw_style(
                        scene,
                        &count,
                        colors.text_dim,
                        transform.pre_translate(Vec2::new(x1 - 12.0 - f64::from(cw), cy)),
                        None,
                        &count_style,
                    );
                    y += HEADER_HEIGHT * self.layout_scale();
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

    /// A selected row stands on the panel's strong lift, as the armed row
    /// of every menu (`floating_surface::armed_row`); ink is measured on
    /// that ground.
    pub(crate) fn row_ground(theme: &Theme, selected: bool) -> Color {
        if selected {
            crate::widget::floating_surface::armed_row(theme).0
        } else {
            crate::chrome::panel_ground(theme)
        }
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
        tint: Color,
    ) -> f64 {
        let icon_rect = Self::row_icon_rect(row);
        icons::draw_icon(scene, transform, icon_rect, icon, tint, 1.5);
        icon_rect.x1 + 10.0
    }

    /// Paints the row's trailing decoration — a badge or a keybinding
    /// chip, mutually exclusive — and returns the x the label
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
        // Reserved before measuring the text, so a long title is ellipsised
        // rather than painted under the chip.
        if let Some(badge) = &entry.badge {
            let badge_style = theme.text(TextRole::Caption);
            let (text_w, text_h) = text_renderer.measure_style(badge, None, &badge_style);
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
            let fill = colors
                .text_primary
                .with_alpha_factor(if selected { 0.28 } else { 0.16 });
            scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                colors.text_primary.with_alpha_factor(0.55),
                None,
                &shape,
            );
            let ink = colors.ink_on(
                Self::row_ground(theme, selected),
                colors.text_primary,
                SMALL_TEXT_MIN_CONTRAST,
            );
            text_renderer.draw_style(
                scene,
                badge,
                ink,
                transform.pre_translate(Vec2::new(pill.x0 + 8.0, cy - f64::from(text_h) / 2.0)),
                None,
                &badge_style,
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
        } else if selected && let Some(alternate) = self.shown_alternate(entry) {
            // The selected row says what its other activation does: while
            // the modifier is held, in full ink as a preview of where it
            // lands; otherwise dimmed, with its keys, as the one hint.
            let held = self.held_modifier() == Some(alternate.modifier);
            let style = theme.text(TextRole::Caption);
            let ink = if held {
                colors.text_primary
            } else {
                colors.ink_on(
                    Self::row_ground(theme, selected),
                    colors.text_dim,
                    SMALL_TEXT_MIN_CONTRAST,
                )
            };
            let keycaps = crate::keycaps::active();
            let labels = vec![keycaps.key(alternate.modifier.key()), keycaps.key("Enter")];
            let caps_w =
                crate::widget::InputHint::measure_keycap_labels(&labels, text_renderer, theme);
            let gap = f64::from(theme.tokens.spacing.s1);
            let (label_w, label_h) = text_renderer.measure_style(&alternate.label, None, &style);
            let cy = f64::midpoint(row.y0, row.y1);
            let label_x = row.x1 - 12.0 - f64::from(label_w);
            text_renderer.draw_style(
                scene,
                &alternate.label,
                ink,
                transform.pre_translate(Vec2::new(label_x, cy - f64::from(label_h) / 2.0)),
                None,
                &style,
            );
            let caps_x = label_x - gap - caps_w;
            let cap_h = crate::widget::InputHint::keycap_height(theme);
            crate::widget::InputHint::paint_keycap_labels_at(
                scene,
                transform,
                Point::new(caps_x, cy - cap_h / 2.0),
                &labels,
                text_renderer,
                theme,
            );
            caps_x - 8.0
        } else {
            // A row that asks for an argument is a row like any other: the
            // prompt it opens says what comes next.
            row.x1 - 12.0
        }
    }

    /// The alternative a selected row shows: the one whose modifier is
    /// held, else the first it offers.
    fn shown_alternate<'a>(&self, entry: &'a CommandEntry) -> Option<&'a Alternate> {
        let held = self.held_modifier();
        entry
            .alternates
            .iter()
            .find(|alternate| Some(alternate.modifier) == held)
            .or_else(|| entry.alternates.first())
    }

    /// Where `text` shows the query's words, merged and in order: the
    /// part of a row a person reads to see why it matched.
    fn match_ranges(&self, text: &str) -> Vec<std::ops::Range<usize>> {
        let mut ranges: Vec<std::ops::Range<usize>> = self
            .query
            .split_whitespace()
            .filter_map(|word| tilo_search::locate(word, text))
            .collect();
        ranges.sort_by_key(|range| range.start);
        let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(ranges.len());
        for range in ranges {
            match merged.last_mut() {
                Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                _ => merged.push(range),
            }
        }
        merged
    }

    /// `text` at `origin`, the parts in `ranges` in a heavier weight.
    fn draw_marked(
        ctx: &mut PaintCtx<'_>,
        text: &str,
        ranges: &[std::ops::Range<usize>],
        origin: Point,
        ink: Color,
        style: &TextStyle,
    ) {
        let marked = style
            .clone()
            .with_weight(crate::text_render::TextWeight::Semibold);
        let mut x = origin.x;
        let mut at = 0usize;
        let mut pieces: Vec<(&str, bool)> = Vec::new();
        for range in ranges {
            if let Some(before) = text.get(at..range.start) {
                pieces.push((before, false));
            }
            if let Some(hit) = text.get(range.start..range.end) {
                pieces.push((hit, true));
            }
            at = range.end;
        }
        if let Some(rest) = text.get(at..) {
            pieces.push((rest, false));
        }
        for (piece, strong) in pieces {
            if piece.is_empty() {
                continue;
            }
            let piece_style = if strong { &marked } else { style };
            ctx.text.draw_style(
                ctx.scene,
                piece,
                ink,
                ctx.transform.pre_translate(Vec2::new(x, origin.y)),
                None,
                piece_style,
            );
            x += f64::from(ctx.text.measure_style(piece, None, piece_style).0);
        }
    }

    /// The roles of a result row's title and of its description line.
    fn row_text_styles(theme: &Theme) -> (TextStyle, TextStyle) {
        (theme.text(TextRole::Body), theme.text(TextRole::Caption))
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
        let (label_style, desc_style) = Self::row_text_styles(theme);
        let row_ground = Self::row_ground(theme, selected);
        // Needs contrast against the armed lift: take the nearest
        // legible step from the theme's dim tone rather than promoting it
        // to the title's weight to survive.
        let subtitle_ink = colors.ink_on(row_ground, colors.text_dim, SMALL_TEXT_MIN_CONTRAST);
        let label_ink = if selected {
            crate::widget::floating_surface::paint_armed_row(
                scene,
                transform,
                row,
                row.x0 - self.rect.x0,
                theme,
            )
        } else {
            colors.text_primary
        };

        let mut text_x = row.x0 + 12.0;
        let image_x = entry
            .image
            .as_ref()
            .and_then(|image| Self::draw_row_image(scene, transform, row, image));
        if let Some(x) = image_x {
            text_x = x;
        } else if let Some(icon) = entry.icon {
            let icon_ink = if selected { label_ink } else { colors.text_dim };
            text_x = Self::draw_row_icon(scene, transform, row, icon, icon_ink);
        }

        let text_right = self.draw_row_trailing(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            row,
            entry,
            selected,
        );
        let avail = (text_right - text_x).max(0.0);

        let label =
            text_renderer.elide_to_width_style(&entry.label, f32_of_f64(avail), &label_style);
        let label_marks = self.match_ranges(&label);
        if let Some(desc) = &entry.description {
            // A path keeps both ends; prose keeps its beginning, where the
            // meaning is.
            let sub = if entry.description_is_path {
                elide_middle(text_renderer, desc, &desc_style, avail)
            } else {
                text_renderer.elide_to_width_style(desc, f32_of_f64(avail), &desc_style)
            };
            let (_, label_height) = text_renderer.measure_style(&label, None, &label_style);
            let (_, desc_height) = text_renderer.measure_style(&sub, None, &desc_style);
            let gap = 3.0 * self.layout_scale();
            let block = f64::from(label_height + desc_height) + gap;
            let ly = row.y0 + (row.height() - block) / 2.0;
            // Only one line is set off: the title when it shows the query,
            // else the subtitle, so a row never shouts twice.
            let desc_marks = if label_marks.is_empty() {
                self.match_ranges(&sub)
            } else {
                Vec::new()
            };
            let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
            Self::draw_marked(
                &mut ctx,
                &label,
                &label_marks,
                Point::new(text_x, ly),
                label_ink,
                &label_style,
            );
            let sub_y = ly + f64::from(label_height) + gap;
            if entry.description_facts.len() > 1 {
                // Facts are runs of their own with space between them, the
                // last losing its end when the row runs out.
                let fact_gap = 12.0 * self.layout_scale();
                let mut x = text_x;
                let right = text_x + avail;
                for fact in &entry.description_facts {
                    let room = right - x;
                    if room <= fact_gap {
                        break;
                    }
                    let shown = ctx
                        .text
                        .elide_to_width_style(fact, f32_of_f64(room), &desc_style);
                    let (w, _) = ctx.text.measure_style(&shown, None, &desc_style);
                    let marks = if label_marks.is_empty() {
                        self.match_ranges(&shown)
                    } else {
                        Vec::new()
                    };
                    Self::draw_marked(
                        &mut ctx,
                        &shown,
                        &marks,
                        Point::new(x, sub_y),
                        subtitle_ink,
                        &desc_style,
                    );
                    x += f64::from(w) + fact_gap;
                }
            } else {
                Self::draw_marked(
                    &mut ctx,
                    &sub,
                    &desc_marks,
                    Point::new(text_x, sub_y),
                    subtitle_ink,
                    &desc_style,
                );
            }
        } else {
            let (_, height) = text_renderer.measure_style(&label, None, &label_style);
            let ly = row.y0 + (row.height() - f64::from(height)) / 2.0;
            Self::draw_marked(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                &label,
                &label_marks,
                Point::new(text_x, ly),
                label_ink,
                &label_style,
            );
        }
    }

    /// Painted in the list area so the card keeps its shape instead of
    /// collapsing to a bare input.
    fn draw_empty_state(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        let top = self.list_top_y();
        let bottom = self.list_bottom_y();
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
        let title_style = theme.text(TextRole::Caption);
        let title = self.empty_results_title();
        let title = text_renderer.elide_to_width_style(
            &title,
            f32_of_f64(self.rect.width() - 48.0),
            &title_style,
        );
        let (tw, title_height) = text_renderer.measure_style(&title, None, &title_style);
        text_renderer.draw_style(
            scene,
            &title,
            colors.text_primary,
            transform.pre_translate(Vec2::new(cx - f64::from(tw) / 2.0, cy)),
            None,
            &title_style,
        );
        if self.is_searching() {
            return;
        }
        let hint_style = theme.text(TextRole::Caption);
        let hint = crate::i18n!("palette-no-results-hint");
        let hint = hint.as_str();
        let (hw, _) = text_renderer.measure_style(hint, None, &hint_style);
        text_renderer.draw_style(
            scene,
            hint,
            colors.text_dim,
            transform.pre_translate(Vec2::new(
                cx - f64::from(hw) / 2.0,
                cy + f64::from(title_height) + 6.0,
            )),
            None,
            &hint_style,
        );
    }

    fn is_searching(&self) -> bool {
        // Entries this palette ranks itself wait for the index's code too.
        (self.searching || (!self.bypass_fuzzy && !crate::search::engine_ready()))
            && !self.query.is_empty()
    }

    fn empty_results_title(&self) -> String {
        if self.is_searching() {
            crate::i18n!("palette-searching")
        } else if self.query.is_empty() {
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
            self.list_bottom_y(),
        )
    }

    /// Moves through the results: arrows step and wrap, Page keys move a
    /// window's worth, Tab and Shift+Tab jump to the next and previous
    /// group, and Home and End reach the first and last result when the
    /// caret has nowhere left to go in the field (always with Ctrl).
    /// `true` when the key moved the selection or belonged to the list.
    pub fn handle_list_key(&mut self, key: &Key, shift: bool, ctrl: bool) -> bool {
        let len = self.visible.len();
        let last = len.saturating_sub(1);
        let target = match key {
            Key::Named(NamedKey::ArrowDown) => {
                if self.cursor >= last {
                    0
                } else {
                    self.cursor.saturating_add(1)
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                if self.cursor == 0 {
                    last
                } else {
                    self.cursor.saturating_sub(1)
                }
            }
            Key::Named(NamedKey::PageDown) => self
                .cursor
                .saturating_add(self.entries_per_page())
                .min(last),
            Key::Named(NamedKey::PageUp) => self.cursor.saturating_sub(self.entries_per_page()),
            Key::Named(NamedKey::Tab) => self.next_group_start(!shift),
            Key::Named(NamedKey::Home)
                if ctrl
                    || (!shift
                        && self.query_selection
                            == crate::text_selection::Selection::collapsed(0)) =>
            {
                0
            }
            Key::Named(NamedKey::End)
                if ctrl
                    || (!shift
                        && self.query_selection
                            == crate::text_selection::Selection::collapsed(self.query.len())) =>
            {
                last
            }
            _ => return false,
        };
        // A key the list owns is consumed at its ends too, so it never
        // scrolls or tabs through whatever is behind the palette.
        if len > 0 {
            self.cursor = target.min(last);
        }
        true
    }

    /// Result rows in one painted window, the step of PageUp and PageDown.
    fn entries_per_page(&self) -> usize {
        let (start, end) = self.painted_window();
        self.rows
            .get(start..end)
            .map_or(0, |rows| {
                rows.iter()
                    .filter(|r| matches!(r, VisualRow::Entry(_)))
                    .count()
            })
            .saturating_sub(1)
            .max(1)
    }

    /// The first result of the group after the cursor's (`forward`) or of
    /// the cursor's own group, else the one before it; wrapping at the
    /// ends. An ungrouped list has one group, so Tab stays put.
    fn next_group_start(&self, forward: bool) -> usize {
        let starts: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| match (row, self.rows.get(i.saturating_add(1))) {
                (VisualRow::Header { .. }, Some(VisualRow::Entry(entry))) => Some(*entry),
                _ => None,
            })
            .collect();
        if starts.is_empty() {
            return self.cursor;
        }
        if forward {
            starts
                .iter()
                .copied()
                .find(|&s| s > self.cursor)
                .or_else(|| starts.first().copied())
                .unwrap_or(self.cursor)
        } else {
            let own = starts.iter().copied().rfind(|&s| s <= self.cursor);
            match own {
                Some(s) if s < self.cursor => s,
                _ => starts
                    .iter()
                    .copied()
                    .rfind(|&s| s < self.cursor)
                    .or_else(|| starts.last().copied())
                    .unwrap_or(self.cursor),
            }
        }
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

const ROW_HEIGHT: f64 = 36.0;
const TWO_LINE_ROW_HEIGHT: f64 = 52.0;
const HEADER_HEIGHT: f64 = 24.0;
/// What a secret's characters are painted as.
const MASK: char = '\u{2022}';
/// Rows the list shows before it scrolls when the host sets no height.
const DEFAULT_LIST_ROWS: f64 = 10.0;
/// Room the composed zero-state needs: glyph, headline, hint.
const EMPTY_STATE_HEIGHT: f64 = 104.0;
const SEARCH_CARD_WIDTH: f64 = 360.0;
const CMD_CARD_WIDTH: f64 = 480.0;
const INPUT_HEIGHT: f64 = 40.0;
const PADDING: f64 = 8.0;
const FILTER_STRIP_HEIGHT: f64 = 36.0;

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
        self.ui_scale = f64::from(theme.ui_scale);
        let generation = crate::search::engine_generation();
        if self.ranked_generation != generation {
            self.ranked_generation = generation;
            if !self.bypass_fuzzy {
                self.rebuild();
            }
        }
        // `desired_size` is the single source of truth so a hand-placed
        // palette (`set_rect`) and a taffy-laid one agree.
        let (width, height) = self.desired_size();
        let style = Style {
            size: Size::from_lengths(f32_of_f64(width), f32_of_f64(height)),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

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
        self.ui_scale = f64::from(theme.ui_scale);
        let colors = &theme.tokens.colors;
        // The one floating card (HIG §9.1): the opaque panel ground and its
        // edges. Opaque, as the card is rows of small text: a tinted
        // backdrop let the window under it read through them, text over
        // text. Only the scrim around it lets the desktop show. The host
        // paints the card's shadow, fading it in with the card.
        crate::widget::floating_surface::paint_card_body(scene, transform, self.rect, theme);

        // A floating panel whose header slot holds the query field,
        // embedded: the card's own first row, not a field boxed inside it.
        // Results, when there are any, are set off from it by the panel's
        // hairline under the header.
        let row_rect = self.input_row_rect();
        let layout = self.compute_input_layout(row_rect);
        let panel = crate::widget::FloatingPanelFace::new()
            .header(row_rect.height())
            .body_empty(self.shows_field_only())
            .layout(self.rect, theme);
        if let Some(rule) = panel.rule {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                crate::surface::edge_on(crate::chrome::panel_ground(theme)),
                None,
                &rule,
            );
        }

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
        if self.shows_field_only() {
            text_renderer.set_text_scale(renderer_scale);
            return;
        }

        // Skipped when the host paints its own controls into the reserved band.
        if self.mode == PaletteMode::Search && !self.hide_filter_strip && !self.host_filter_strip {
            self.draw_filter_strip(scene, transform, text_renderer, theme);
        }

        if self.visible.is_empty() {
            self.draw_empty_state(scene, transform, text_renderer, theme);
        } else {
            self.draw_result_rows(scene, transform, text_renderer, theme);
        }

        text_renderer.set_text_scale(renderer_scale);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.is_focused {
            return;
        }
        // Read, never consumed: every other surface needs modifiers too.
        if let WindowEvent::ModifiersChanged(modifiers) = event {
            self.held = modifiers.state();
            return;
        }
        if let WindowEvent::MouseWheel { delta, .. } = event
            && self.rect.contains(self.last_cursor)
            && self.arg_state.is_none()
        {
            let rows = match *delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => -f64::from(y),
                winit::event::MouseScrollDelta::PixelDelta(p) => {
                    -p.y / ctx.scale_factor / self.row_height().max(1.0)
                }
            };
            let rows = isize::try_from(crate::num::i64_of_f64(rows.round())).unwrap_or_default();
            if rows != 0 {
                self.scroll_by(rows);
            }
            *ctx.handled = true;
            return;
        }
        if self.handle_pointer_event(
            event,
            ctx.scale_factor,
            &mut ctx.state,
            ctx.path,
            ctx.handled,
        ) {
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
                    repeat,
                    ..
                },
            ..
        } = event
        {
            if *element_state != ElementState::Pressed {
                return;
            }
            // Submission needs a fresh press, including after loading or opening a prompt.
            if *repeat && matches!(logical_key, Key::Named(NamedKey::Enter)) {
                *ctx.handled = true;
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
                    &mut ctx.state,
                    ctx.path,
                    ctx.handled,
                );
                return;
            }
            if self.handle_list_key(logical_key, shift, ctrl) {
                *ctx.handled = true;
                return;
            }
            if !shift && !ctrl && self.step_selected(logical_key) {
                *ctx.handled = true;
                return;
            }
            if matches!(logical_key, Key::Named(NamedKey::Backspace))
                && self.scope_token.is_some()
                && self.query_selection == crate::text_selection::Selection::collapsed(0)
            {
                self.scope_token = None;
                self.scope_token_removed = true;
                *ctx.handled = true;
                return;
            }
            match logical_key {
                Key::Named(NamedKey::Enter) => {
                    self.activate_selected(&mut ctx.state, ctx.path);
                    *ctx.handled = true;
                }
                // A typed query is cleared first; only an empty field closes,
                // so a slip of the finger never costs the whole search.
                Key::Named(NamedKey::Escape) => {
                    if self.query.is_empty() {
                        self.is_focused = false;
                    } else {
                        self.set_query("");
                    }
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

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        if let Some(arg) = &self.arg_state {
            let field = arg
                .composition
                .ime_edit_fingerprint(&arg.buffer, &arg.selection, key)?;
            Some(crate::ime::scoped_fingerprint(
                arg.entry_id.as_bytes(),
                field,
                key,
            ))
        } else {
            self.query_composition
                .ime_edit_fingerprint(&self.query, &self.query_selection, key)
        }
    }

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        Some(match self.arg_state.as_ref().map(|arg| arg.kind) {
            Some(ArgumentKind::Secret) => crate::ime::InputContent::PASSWORD,
            Some(ArgumentKind::Number { .. }) => {
                crate::ime::InputContent::of(crate::ime::InputPurpose::Number)
            }
            Some(ArgumentKind::Text) | None => crate::ime::InputContent::TEXT,
        })
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
        // The caret makes the field a text field for assistive technology
        // (AT-SPI Text and EditableText), not only a labelled value: a screen
        // reader can then read and set the query as it would any other input.
        let (anchor, caret) = if self.arg_state.is_some() {
            (query.1.len(), query.1.len())
        } else {
            (self.query_selection.anchor, self.query_selection.cursor)
        };
        // A secret reports that it is protected and never its value.
        let secret = self.arg_state.as_ref().is_some_and(ArgState::is_secret);
        let mut query_node = AccessNode::new(Role::TextInput)
            .with_action_target("command-palette:query")
            .with_name(query.0.clone())
            .with_bounds(self.input_row_rect())
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.is_focused)
                    .protected(secret),
            );
        if !secret {
            query_node = query_node
                .with_value(query.1.clone())
                .with_text_selection(anchor, caret);
        }

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
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.is_focused && visible_index == self.cursor)
                            .selected(visible_index == self.cursor),
                    );
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
                    self.list_bottom_y(),
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
                self.list_bottom_y(),
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
        _key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
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
        state: &mut crate::widget::Edits<'_>,
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
    /// Drive `handle_event` without a layout: it ignores the taffy tree +
    /// node id, so a throwaway single-leaf tree runs the exact same logic.
    #[cfg(test)]
    fn handle_overlay_key(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        state: &mut crate::widget::Edits<'_>,
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
                modifiers: &modifiers,
                state: state.reborrow(),
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
    use crate::testing::{Dispatch, dispatch};
    use taffy::TaffyTree;
    use winit::event::Modifiers;
    use winit::keyboard::SmolStr;

    fn key_press(logical_key: Key) -> WindowEvent {
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            logical_key,
            None,
            ElementState::Pressed,
        ))
    }

    #[test]
    fn palette_card_geometry_and_rows() {
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
            let (_, label_h) = tr.measure(
                "Open Files",
                theme.tokens.typography.size(TextRole::Caption) / 2.0,
                None,
            );
            let (_, desc_h) = tr.measure(
                "Opens beside",
                theme.tokens.typography.size(TextRole::Caption) / 2.0,
                None,
            );
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
                    .all(|run| run.font_size <= theme.tokens.typography.size(TextRole::Body))
            );
            assert!(runs.iter().any(|run| {
                (run.font_size - theme.tokens.typography.size(TextRole::Caption)).abs() < 0.01
            }));
            assert!(p.row_rect(1).unwrap().y1 <= p.list_bottom_y());
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
        }

        fn palette_card_is_a_panel() {
            let theme = Theme::dark();
            assert_eq!(
                CommandPalette::surface_radius_px(&theme),
                f64::from(theme.tokens.radii.panel),
                "the palette is a floating panel"
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
            // Must accommodate input row + filter strip + 2 result rows.
            p.rect = Rect::new(0.0, 0.0, 360.0, 280.0);
            let mid_row_1 = p.row_rect(1).unwrap().center();
            assert_eq!(p.row_at(mid_row_1), Some(1));
            assert!(p.row_at(Point::new(1000.0, 1000.0)).is_none());
        }

        fn the_card_ends_with_its_last_row_and_no_footer() {
            let mut p = CommandPalette::new();
            p.set_bypass_fuzzy(true);
            p.set_entries(vec![
                CommandEntry::new("a", "Alpha"),
                CommandEntry::new("b", "Bravo"),
            ]);
            let (w, h) = p.desired_size();
            p.set_rect(Rect::new(0.0, 0.0, w, h));
            let last = p.row_rect(1).expect("the second row is painted");
            assert!(
                (p.rect.y1 - PADDING - last.y1).abs() < 0.01,
                "the card ends one inset below its last row: {last:?} in {:?}",
                p.rect
            );
        }

        large_text_palette_reserves_painted_rows_and_scales_fonts_once();
        palette_card_is_a_panel();
        input_row_layout_gives_the_query_the_full_trailing_width();
        row_at_finds_visible_row_inside_the_post_layout_rect();
        the_card_ends_with_its_last_row_and_no_footer();
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
            let handled = dispatch(&mut p, &event);

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
                && child.description.as_deref()
                    == Some("Check the spelling, or try another word")));
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
            let mut doc = automerge::AutoCommit::new();

            assert!(p.handle_overlay_key(
                &crate::synthetic_input::cursor_moved(row.center().x, row.center().y),
                modifiers,
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert_eq!(p.cursor(), 1, "hover highlights the hit-tested row");
            assert_eq!(
                Widget::cursor_shape(&p, row.center()),
                Some(winit::window::CursorIcon::Pointer)
            );
            assert!(p.handle_overlay_key(
                &crate::synthetic_input::mouse_press(),
                modifiers,
                &mut crate::widget::Edits::lend(&mut doc),
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
            let mut doc = automerge::AutoCommit::new();

            assert!(Widget::perform_semantic_action(
                &mut p,
                &target,
                &crate::a11y::Action::Focus,
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert_eq!(p.selected_id(), Some("bravo"));
            assert!(Widget::perform_semantic_action(
                &mut p,
                &target,
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut doc),
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
            palette.activate_selected(
                &mut crate::widget::Edits::lend(&mut automerge::AutoCommit::new()),
                &automerge::ROOT,
            );

            assert!(palette.take_outcome().is_none());
            let invocation =
                crate::widget::controls::take_control_invocation(&mut palette).unwrap();
            assert_eq!(invocation.address, ActionAddress::new("document:1", "save"));
            assert!(invocation.arguments.is_empty());
        }

        fn prompted_resource_action_binds_its_declared_argument() {
            let spec = tilo_actions::ActionSpec::prompted_param(
                "rename",
                "Rename",
                tilo_actions::ActionParam::required(
                    PROMPTED_PARAM,
                    tilo_actions::ParamType::Text,
                    "New name",
                ),
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

            let handled = dispatch(&mut palette, &key_press(Key::Named(NamedKey::Enter)));
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

        /// The palette's typed text lands in the parameter the action names,
        /// whatever it is called: the prompt is the description.
        fn prompted_action_with_a_named_parameter_gets_the_typed_text_under_that_name() {
            let spec = tilo_actions::ActionSpec::prompted_param(
                "rename",
                "Rename",
                tilo_actions::ActionParam::required(
                    "title",
                    tilo_actions::ParamType::Text,
                    "The note's new title",
                ),
                "title…",
                tilo_actions::ExecutionLocus::ClientLocal,
                |title| {
                    tilo_actions::EncodedAction::encode_operation("test.rename", &title).unwrap()
                },
            );
            let resources = vec![
                tilo_actions::Resource::new("document:1", "document", "Document").with_action(spec),
            ];
            let flat = tilo_actions::flatten(&resources);
            let entry = CommandEntry::from_flat_action(&flat[0]);
            let mut palette = CommandPalette::commands();
            palette.open_action_argument(entry, Some("Project Atlas".into()));
            assert!(dispatch(
                &mut palette,
                &key_press(Key::Named(NamedKey::Enter))
            ));
            let invocation =
                crate::widget::controls::take_control_invocation(&mut palette).unwrap();
            assert_eq!(invocation.arguments["title"], "Project Atlas");
            assert!(!invocation.arguments.contains_key(PROMPTED_PARAM));
            let encoded = flat[0].action.build_json(&invocation.arguments).unwrap();
            assert_eq!(encoded.type_id, "test.rename");
        }

        fn typed_resource_action_binds_its_primary_parameter() {
            let spec = tilo_actions::ActionSpec::typed_operation(
                "go-to-cell",
                "Go to cell",
                "Cell reference",
                tilo_actions::OperationKind::View,
                "test.go",
                vec![tilo_actions::ActionParam::required_owned(
                    "reference",
                    tilo_actions::ParamType::Text,
                    "Cell reference",
                )],
                |args| {
                    tilo_actions::EncodedAction::encode_operation(
                        "test.go",
                        &args.text("reference").unwrap_or_default(),
                    )
                    .unwrap()
                },
            );
            let resources = vec![
                tilo_actions::Resource::new("document:1", "document", "Document").with_action(spec),
            ];
            let flat = tilo_actions::flatten(&resources);
            let entry = CommandEntry::from_flat_action(&flat[0]);
            let mut palette = CommandPalette::commands();
            palette.open_action_argument(entry, Some("B12".into()));

            dispatch(&mut palette, &key_press(Key::Named(NamedKey::Enter)));
            let invocation =
                crate::widget::controls::take_control_invocation(&mut palette).unwrap();
            assert_eq!(invocation.arguments["reference"], "B12");
            let encoded = flat[0].action.build_json(&invocation.arguments).unwrap();
            assert_eq!(
                encoded.payload,
                serde_json::to_vec("B12").unwrap(),
                "the typed builder reads the text"
            );
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
        prompted_action_with_a_named_parameter_gets_the_typed_text_under_that_name();
        typed_resource_action_binds_its_primary_parameter();
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
            let mut document = automerge::AutoCommit::new();
            let modifiers = Modifiers::default();

            assert!(palette.handle_overlay_key(
                &WindowEvent::Ime(winit::event::Ime::Preedit(
                    "東京".into(),
                    Some(("東".len(), "東".len())),
                )),
                modifiers,
                &mut crate::widget::Edits::lend(&mut document),
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
                modifiers,
                &mut crate::widget::Edits::lend(&mut document),
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
            dispatch(&mut p, &key_press(Key::Named(NamedKey::ArrowLeft)));
            assert_eq!(p.query_selection.cursor, 4);
            dispatch(&mut p, &key_press(Key::Named(NamedKey::Home)));
            assert_eq!(p.query_selection.cursor, 0);
        }

        fn backspace_deletes_at_caret_not_end() {
            let mut p = CommandPalette::new();
            p.set_focus(true);
            p.set_query("abc");
            p.query_selection.cursor = 1;
            p.query_selection.anchor = 1;
            dispatch(&mut p, &key_press(Key::Named(NamedKey::Backspace)));
            assert_eq!(p.query(), "bc");
            assert_eq!(p.query_selection.cursor, 0);
        }

        fn typing_inserts_at_caret() {
            let mut p = CommandPalette::new();
            p.set_focus(true);
            p.set_query("ac");
            p.query_selection.cursor = 1;
            p.query_selection.anchor = 1;
            dispatch(&mut p, &key_press(Key::Character(SmolStr::new("b"))));
            assert_eq!(p.query(), "abc");
            assert_eq!(p.query_selection.cursor, 2);
        }

        fn ctrl_a_selects_query() {
            let mut p = CommandPalette::new();
            p.set_focus(true);
            p.set_query("hello");
            let mut mods_state = winit::keyboard::ModifiersState::empty();
            mods_state.set(winit::keyboard::ModifiersState::CONTROL, true);
            Dispatch::new()
                .modifiers(mods_state)
                .send(&mut p, &key_press(Key::Character(SmolStr::new("a"))));
            assert_eq!(p.query_selection.anchor, 0);
            assert_eq!(p.query_selection.cursor, 5);
        }

        fn typing_replaces_selection() {
            let mut p = CommandPalette::new();
            p.set_focus(true);
            p.set_query("hello");
            p.query_selection.anchor = 1;
            p.query_selection.cursor = 4;
            dispatch(&mut p, &key_press(Key::Character(SmolStr::new("i"))));
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
            assert!(
                query.text_selection.is_some(),
                "the query carries a caret, so AT-SPI exposes it as editable text"
            );
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

            for _ in 0..2 {
                let handled = dispatch(&mut p, &key_press(Key::Named(NamedKey::ArrowDown)));
                assert!(handled, "ArrowDown must be consumed by the palette");
            }
            assert_eq!(p.selected_id(), Some("export"), "cursor advanced two rows");

            let handled = dispatch(&mut p, &key_press(Key::Named(NamedKey::ArrowUp)));
            assert!(handled);
            assert_eq!(p.selected_id(), Some("sum"));

            let handled = dispatch(&mut p, &key_press(Key::Named(NamedKey::Enter)));
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
                p.row_rect(crate::num::usize_of_f64(DEFAULT_LIST_ROWS))
                    .is_none(),
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

            let handled = dispatch(&mut p, &key_press(Key::Named(NamedKey::Enter)));
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
                let h = dispatch(&mut p, &key_press(Key::Character(SmolStr::new(ch))));
                assert!(h, "typing in the arg field is consumed");
            }
            assert_eq!(p.argument_buffer(), Some("B12"));

            let handled = dispatch(&mut p, &key_press(Key::Named(NamedKey::Enter)));
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
            let handled = dispatch(&mut p, &key_press(Key::Named(NamedKey::Enter)));
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
            let handled = dispatch(&mut p, &key_press(Key::Named(NamedKey::Escape)));
            assert!(handled);
            assert!(!p.is_open());
            assert!(!p.in_argument_mode());
        }

        fn command_mode_escape_from_argument_returns_to_list() {
            let mut p = seeded_command_palette();
            dispatch(&mut p, &key_press(Key::Named(NamedKey::Enter)));
            assert!(p.in_argument_mode());
            let h = dispatch(&mut p, &key_press(Key::Named(NamedKey::Escape)));
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

        fn no_painted_row_runs_past_the_card() {
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
            let bottom = p.list_bottom_y();
            for i in 0..40 {
                if let Some(r) = p.row_rect(i) {
                    assert!(
                        r.y1 <= bottom,
                        "row {i} at {r:?} runs past the list's end at {bottom}"
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
            let room = p.list_bottom_y() - p.list_top_y();
            assert!(
                room >= EMPTY_STATE_HEIGHT - 1.0,
                "the zero-state needs {EMPTY_STATE_HEIGHT} px of list area, got {room}"
            );
        }

        grouped_entries_plan_one_header_per_section();
        a_trimmed_section_reports_its_untrimmed_count();
        a_scrolled_list_hit_tests_the_row_it_painted();
        no_painted_row_runs_past_the_card();
        an_empty_result_set_still_gets_a_readable_card();
    }
}

#[cfg(test)]
mod selected_status_tests {
    use super::*;

    #[test]
    fn combined_scaling_keeps_two_line_results_inside_rows() {
        let theme = Theme::dark().with_ui_scale(1.5).with_text_scale(1.3);
        let mut palette = CommandPalette::commands();
        palette.set_entries(vec![
            CommandEntry::new("fixture", "Open Recovery Fixture")
                .description("The application is temporarily unavailable. Try again."),
        ]);
        palette.open();
        let mut taffy = TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        palette.build_layout(&mut taffy, &theme, &mut nodes);
        let (width, height) = palette.desired_size();
        palette.set_rect(Rect::new(0.0, 0.0, width, height));
        let row = palette.row_rect(0).expect("a visible result row");
        let mut text = TextRenderer::new();
        let (_, title_height) =
            text.measure_style("Open Recovery Fixture", None, &theme.text(TextRole::Body));
        let (_, detail_height) = text.measure_style(
            "The application is temporarily unavailable. Try again.",
            None,
            &theme.text(TextRole::Caption),
        );
        assert!(
            title_height > 0.0 && detail_height > 0.0,
            "fonts must be observable"
        );
        let text_height =
            f64::from(title_height + detail_height) + 3.0 * f64::from(theme.text_scale);
        assert!(
            row.height() >= text_height,
            "rendered title and detail need {text_height}px inside {row:?}"
        );
    }

    #[test]
    fn selected_status_preserves_query_order_focus_and_accessible_detail() {
        let mut palette = CommandPalette::commands();
        palette.set_entries(vec![
            CommandEntry::new("one", "Fixture one"),
            CommandEntry::new("two", "Fixture two"),
        ]);
        palette.open();
        palette.set_rect(Rect::new(0.0, 0.0, 640.0, 400.0));
        palette.set_query("fixture");
        palette.set_cursor(1);
        palette.set_selected_status("Couldn't run", "Try again after restoring the connection.");
        assert_eq!(palette.selected_id(), Some("two"));
        assert_eq!(palette.query(), "fixture");
        assert!(palette.is_focused());
        let entries = palette.visible_entries();
        assert_eq!(entries.first().unwrap().id, "one");
        assert_eq!(
            entries.get(1).unwrap().badge.as_deref(),
            Some("Couldn't run")
        );
        let semantics = palette.accessibility().unwrap();
        let row = semantics.children.get(1).unwrap().children.get(1).unwrap();
        assert!(
            row.description
                .as_deref()
                .unwrap()
                .contains("restoring the connection")
        );
        assert!(row.description.as_deref().unwrap().contains("Couldn't run"));
        palette.set_entries(vec![CommandEntry::new("two", "Fixture two")]);
        assert!(palette.visible_entries().first().unwrap().badge.is_none());
    }
}

#[cfg(test)]
mod state_submit_tests {
    use super::*;
    use crate::testing::dispatch;

    fn key(key: NamedKey, repeat: bool) -> WindowEvent {
        let mut event =
            crate::synthetic_input::key_event(Key::Named(key), None, ElementState::Pressed);
        event.repeat = repeat;
        crate::synthetic_input::keyboard_input(event)
    }

    #[test]
    fn loaded_results_require_a_fresh_enter_and_remain_navigable() {
        let mut palette = CommandPalette::new();
        palette.open();
        assert!(dispatch(&mut palette, &key(NamedKey::Enter, false)));
        assert!(palette.take_outcome().is_none());
        palette.set_entries(vec![
            CommandEntry::new("first", "First"),
            CommandEntry::new("second", "Second"),
        ]);
        assert!(dispatch(&mut palette, &key(NamedKey::Enter, true)));
        assert!(
            palette.take_outcome().is_none(),
            "loading must not turn held Enter into activation"
        );
        assert!(dispatch(&mut palette, &key(NamedKey::ArrowDown, true)));
        assert_eq!(palette.selected_id(), Some("second"));
        assert!(dispatch(&mut palette, &key(NamedKey::Enter, false)));
        assert_eq!(
            palette.take_outcome().expect("fresh Enter activates").id,
            "second"
        );
        assert!(dispatch(&mut palette, &key(NamedKey::Enter, true)));
        assert!(
            palette.take_outcome().is_none(),
            "one press must not invoke repeatedly"
        );
        palette.close();
        palette.open();
        assert!(dispatch(&mut palette, &key(NamedKey::Enter, true)));
        assert!(
            palette.take_outcome().is_none(),
            "reopening must not accept an orphan repeat"
        );
    }

    #[test]
    fn opening_an_argument_requires_a_separate_confirmation() {
        let mut palette = CommandPalette::commands();
        palette.push(
            CommandEntry::new("goto", "Go to cell")
                .with_argument("Go to cell", "Cell")
                .with_argument_prefill("A1"),
        );
        palette.open();
        assert!(dispatch(&mut palette, &key(NamedKey::Enter, false)));
        assert!(palette.arg_state.is_some());
        assert!(dispatch(&mut palette, &key(NamedKey::Enter, true)));
        assert!(palette.is_open(), "held Enter must leave the prompt open");
        assert!(
            palette.take_outcome().is_none(),
            "prefill needs fresh confirmation"
        );
        assert!(dispatch(&mut palette, &key(NamedKey::Enter, false)));
        let outcome = palette
            .take_outcome()
            .expect("fresh Enter submits the prompt");
        assert_eq!(outcome.id, "goto");
        assert_eq!(outcome.argument.as_deref(), Some("A1"));
        assert!(!palette.is_open());
        palette.open();
        assert!(dispatch(&mut palette, &key(NamedKey::Enter, false)));
        assert!(dispatch(&mut palette, &key(NamedKey::Escape, false)));
        assert!(palette.arg_state.is_none(), "Escape cancels the argument");
        assert!(palette.take_outcome().is_none());
    }

    #[test]
    fn a_field_only_palette_lists_nothing_until_something_is_typed() {
        let mut palette = CommandPalette::new();
        palette.push(CommandEntry::new("music", "Music"));
        palette.push(CommandEntry::new("notes", "Notes"));
        palette.set_rect(Rect::new(0.0, 0.0, 600.0, 400.0));
        let full = palette.desired_size().1;
        palette.set_field_only_while_empty(true);
        assert!(palette.shows_field_only());
        let field = palette.desired_size().1;
        assert!(field < full, "the empty card is its field alone");
        assert!(
            palette.row_at(Point::new(300.0, field + 30.0)).is_none(),
            "no hidden row takes a click"
        );
        palette.set_query("mu");
        assert!(!palette.shows_field_only());
        assert!(
            palette.desired_size().1 > field,
            "typing grows the card into results"
        );
    }
}

#[cfg(test)]
mod keyboard_flow_tests {
    use super::*;
    use crate::testing::{Dispatch, dispatch};
    use winit::keyboard::{ModifiersState, SmolStr};

    fn press(key: NamedKey) -> WindowEvent {
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            Key::Named(key),
            None,
            ElementState::Pressed,
        ))
    }

    fn typed(text: &str) -> WindowEvent {
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            Key::Character(SmolStr::new(text)),
            Some(SmolStr::new(text)),
            ElementState::Pressed,
        ))
    }

    fn with(palette: &mut CommandPalette, held: ModifiersState, event: &WindowEvent) -> bool {
        Dispatch::new().modifiers(held).send(palette, event).handled
    }

    /// `groups` of (label, rows), laid out in a card of the default size.
    fn grouped(groups: &[(&str, usize)]) -> CommandPalette {
        let mut p = CommandPalette::new();
        p.set_bypass_fuzzy(true);
        p.open();
        let mut entries = Vec::new();
        for (group, rows) in groups {
            for i in 0..*rows {
                entries.push(
                    CommandEntry::new(format!("{group}{i}"), format!("{group} {i}")).group(*group),
                );
            }
        }
        p.set_entries(entries);
        let (w, h) = p.desired_size();
        p.set_rect(Rect::new(0.0, 0.0, w, h));
        p
    }

    fn painted_rows(p: &CommandPalette) -> Vec<VisualRow> {
        let (start, end) = p.painted_window();
        p.rows
            .get(start..end)
            .map(<[VisualRow]>::to_vec)
            .unwrap_or_default()
    }

    #[test]
    fn escape_clears_the_query_before_it_closes() {
        let mut p = grouped(&[("Files", 3)]);
        dispatch(&mut p, &typed("f"));
        assert_eq!(p.query(), "f");
        assert!(dispatch(&mut p, &press(NamedKey::Escape)));
        assert_eq!(p.query(), "", "the first Escape clears what was typed");
        assert!(p.is_open(), "…and keeps the palette open");
        assert!(dispatch(&mut p, &press(NamedKey::Escape)));
        assert!(!p.is_open(), "Escape on an empty field closes");
    }

    #[test]
    fn arrows_wrap_and_page_keys_tab_home_and_end_move_through_results() {
        let mut p = grouped(&[("Apps", 2), ("Files", 30), ("Settings", 2)]);
        assert_eq!(p.selected_id(), Some("Apps0"));
        dispatch(&mut p, &press(NamedKey::ArrowUp));
        assert_eq!(p.selected_id(), Some("Settings1"), "Up from the top wraps");
        dispatch(&mut p, &press(NamedKey::ArrowDown));
        assert_eq!(p.selected_id(), Some("Apps0"), "Down from the bottom wraps");

        dispatch(&mut p, &press(NamedKey::Tab));
        assert_eq!(
            p.selected_id(),
            Some("Files0"),
            "Tab goes to the next group"
        );
        dispatch(&mut p, &press(NamedKey::Tab));
        assert_eq!(p.selected_id(), Some("Settings0"));
        dispatch(&mut p, &press(NamedKey::Tab));
        assert_eq!(
            p.selected_id(),
            Some("Apps0"),
            "Tab wraps to the first group"
        );
        with(&mut p, ModifiersState::SHIFT, &press(NamedKey::Tab));
        assert_eq!(p.selected_id(), Some("Settings0"), "Shift+Tab goes back");

        p.set_cursor(2);
        let page = p.entries_per_page();
        assert!(page > 1, "a page is more than one row");
        assert!(dispatch(&mut p, &press(NamedKey::PageDown)));
        assert_eq!(p.cursor(), page.saturating_add(2));
        let before = p.cursor();
        let page = p.entries_per_page();
        assert!(dispatch(&mut p, &press(NamedKey::PageUp)));
        assert_eq!(
            p.cursor(),
            before.saturating_sub(page),
            "PageUp moves a window's worth back"
        );

        assert!(
            dispatch(&mut p, &press(NamedKey::End)),
            "End with the caret at the end"
        );
        assert_eq!(p.selected_id(), Some("Settings1"));
        assert!(dispatch(&mut p, &press(NamedKey::Home)));
        assert_eq!(p.selected_id(), Some("Apps0"));
    }

    #[test]
    fn home_and_end_edit_the_query_until_the_caret_is_at_that_end() {
        let mut p = grouped(&[("Files", 5)]);
        dispatch(&mut p, &typed("a"));
        dispatch(&mut p, &typed("b"));
        p.set_cursor(2);
        dispatch(&mut p, &press(NamedKey::Home));
        assert_eq!(p.cursor(), 2, "Home first moves the caret");
        assert_eq!(
            p.query_selection,
            crate::text_selection::Selection::collapsed(0)
        );
        dispatch(&mut p, &press(NamedKey::Home));
        assert_eq!(p.cursor(), 0, "Home again reaches the first result");
        with(&mut p, ModifiersState::CONTROL, &press(NamedKey::End));
        assert_eq!(p.cursor(), 4, "Ctrl+End always reaches the last result");
    }

    #[test]
    fn the_list_scrolls_only_when_the_cursor_leaves_it_and_never_ends_on_a_header() {
        let mut p = grouped(&[("Apps", 3), ("Files", 40)]);
        let first = p.painted_window();
        assert_eq!(first.0, 0);
        // Down to the bottom of the first window, then one past it.
        let last_painted = (0..p.visible.len())
            .filter(|&i| p.row_rect(i).is_some())
            .max()
            .expect("rows are painted");
        p.set_cursor(last_painted.saturating_add(1));
        let scrolled = p.painted_window();
        assert!(scrolled.0 > 0, "leaving the window scrolls it");
        // Back up one row: still inside the window, which stays put.
        p.set_cursor(last_painted);
        assert_eq!(
            p.painted_window(),
            scrolled,
            "moving inside the window keeps it"
        );
        // Wherever it stands, the window's last row is a result.
        for cursor in 0..p.visible.len() {
            p.set_cursor(cursor);
            let rows = painted_rows(&p);
            assert!(
                matches!(rows.last(), Some(VisualRow::Entry(_))),
                "cursor {cursor}: window ends on {:?}",
                rows.last()
            );
        }
    }

    #[test]
    fn the_card_grows_with_its_rows_up_to_the_host_cap_and_scrolls_past_it() {
        let mut p = grouped(&[("Files", 3)]);
        let small = p.desired_size().1;
        p.set_entries(
            (0..40)
                .map(|i| CommandEntry::new(format!("f{i}"), format!("File {i}")).group("Files"))
                .collect(),
        );
        let default_cap = p.desired_size().1;
        assert!(default_cap > small, "more rows make a taller card");
        p.set_max_list_height(20.0 * p.row_height() + HEADER_HEIGHT);
        let (w, tall) = p.desired_size();
        assert!(tall > default_cap, "the host's cap, not a fixed row count");
        p.set_rect(Rect::new(0.0, 0.0, w, tall));
        let painted = (0..40).filter(|&i| p.row_rect(i).is_some()).count();
        assert_eq!(painted, 20, "the cap's whole rows are painted");
        assert_eq!(p.entry_row_capacity(), 20);
        p.last_cursor = Point::new(w / 2.0, tall / 2.0);
        assert!(dispatch(
            &mut p,
            &crate::synthetic_input::mouse_wheel(0.0, -3.0)
        ));
        assert!(p.painted_window().0 >= 3, "the wheel scrolls the list");
        assert!(p.row_rect(p.cursor()).is_some(), "the cursor stays painted");
    }

    #[test]
    fn a_secret_step_is_masked_kept_from_assistive_technology_and_handed_to_the_host() {
        let mut p = grouped(&[("Actions", 1)]);
        p.open_argument_step(
            "secret",
            ArgumentSpec {
                prompt: "Unlock".into(),
                placeholder: "Password".into(),
                prefill: None,
                kind: ArgumentKind::Secret,
            },
        );
        for c in ["h", "u", "n", "t", "e", "r", "2"] {
            dispatch(&mut p, &typed(c));
        }
        assert_eq!(p.argument_buffer(), Some("hunter2"));
        let node = Widget::accessibility(&p).expect("the open palette publishes itself");
        let field = node.children.first().expect("the field");
        assert!(field.state.protected, "the field says it holds a secret");
        assert_eq!(field.value, None, "and never what it holds");
        assert_eq!(
            Widget::ime_content(&p),
            Some(crate::ime::InputContent::PASSWORD)
        );
        let arg = p.arg_state.as_ref().expect("still in the step");
        let (shown, selection) = arg.masked().expect("a secret is painted masked");
        assert_eq!(
            shown,
            MASK.to_string().repeat(7),
            "one bullet per character"
        );
        assert_eq!(
            selection.cursor,
            shown.len(),
            "the caret after the last bullet"
        );
        assert!(dispatch(&mut p, &press(NamedKey::Enter)));
        assert_eq!(p.take_submitted_argument().as_deref(), Some("hunter2"));
        assert!(!p.in_argument_mode());
        assert!(p.take_outcome().is_none(), "a host step runs no row");
        assert!(p.is_open());

        p.open_argument_step("again", ArgumentSpec::default());
        assert!(dispatch(&mut p, &press(NamedKey::Escape)));
        assert!(p.take_argument_cancelled(), "Escape backs out of the step");
        assert!(p.is_open(), "…to the list, not out of the palette");
        assert!(!p.take_argument_cancelled(), "reported once");
    }

    #[test]
    fn a_number_step_steps_with_the_arrows_inside_its_bounds() {
        let mut p = grouped(&[("Actions", 1)]);
        p.open_argument_step(
            "volume",
            ArgumentSpec {
                prompt: "Volume".into(),
                placeholder: String::new(),
                prefill: Some("95".into()),
                kind: ArgumentKind::Number {
                    min: Some(0.0),
                    max: Some(100.0),
                    step: 5.0,
                },
            },
        );
        dispatch(&mut p, &press(NamedKey::ArrowDown));
        assert_eq!(p.argument_buffer(), Some("90"));
        for _ in 0..4 {
            dispatch(&mut p, &press(NamedKey::ArrowUp));
        }
        assert_eq!(p.argument_buffer(), Some("100"), "clamped at the top");
        assert_eq!(format_number(0.35, 0.05), "0.35");
    }

    #[test]
    fn the_selected_row_shows_the_alternate_for_the_held_modifier() {
        let entry = CommandEntry::new("files", "Files")
            .alternate(ActivationModifier::Shift, "As a tab here")
            .alternate(ActivationModifier::Ctrl, "Beside");
        let mut p = grouped(&[("Apps", 1)]);
        assert_eq!(
            p.shown_alternate(&entry).map(|a| a.label.as_str()),
            Some("As a tab here"),
            "the first is the hint while nothing is held"
        );
        let held = winit::event::Modifiers::from(ModifiersState::CONTROL);
        dispatch(&mut p, &WindowEvent::ModifiersChanged(held));
        assert_eq!(p.held_modifier(), Some(ActivationModifier::Ctrl));
        assert_eq!(
            p.shown_alternate(&entry).map(|a| a.label.as_str()),
            Some("Beside")
        );
        assert_eq!(
            ActivationModifier::of(ModifiersState::CONTROL | ModifiersState::SHIFT),
            None,
            "a chord of two is no alternative"
        );
    }

    #[test]
    fn arrows_step_an_adjustable_row_only_with_the_caret_at_the_end() {
        let mut p = CommandPalette::new();
        p.set_entries(vec![
            CommandEntry::new("volume", "Volume").adjustable(),
            CommandEntry::new("files", "Files"),
        ]);
        p.open();
        assert!(p.step_selected(&Key::Named(NamedKey::ArrowRight)));
        assert_eq!(p.take_adjustment(), Some(("volume".into(), 1)));
        assert!(p.step_selected(&Key::Named(NamedKey::ArrowLeft)));
        assert_eq!(p.take_adjustment(), Some(("volume".into(), -1)));
        p.set_cursor(1);
        assert!(
            !p.step_selected(&Key::Named(NamedKey::ArrowLeft)),
            "a row with no value leaves the arrows to the caret"
        );
        p.set_cursor(0);
        p.query = "vol".into();
        p.query_selection = crate::text_selection::Selection::collapsed(1);
        assert!(
            !p.step_selected(&Key::Named(NamedKey::ArrowRight)),
            "inside the query the arrows move the caret"
        );
    }

    #[test]
    fn matched_words_are_located_in_the_row_and_prose_is_cut_at_its_end() {
        let mut p = grouped(&[("Files", 1)]);
        p.query = "dark the".into();
        assert_eq!(
            p.match_ranges("Use the dark theme"),
            vec![4..7, 8..12],
            "each query word, merged and in order"
        );
        assert!(p.match_ranges("Brightness").is_empty());

        let theme = Theme::dark();
        let style = theme.text(TextRole::Caption);
        let mut tr = TextRenderer::new();
        let prose = "Request one-session microphone access for the focused field, or finish the current recording.";
        let width = f64::from(tr.measure_style(prose, None, &style).0) / 2.0;
        let cut = tr.elide_to_width_style(prose, f32_of_f64(width), &style);
        assert!(
            cut.starts_with("Request one-session"),
            "prose keeps its start: {cut}"
        );
        assert!(cut.ends_with('\u{2026}'));
        let path = "~/src/tilo/crates/sdk/ui-core/src/widget/command_palette.rs";
        let width = f64::from(tr.measure_style(path, None, &style).0) / 2.0;
        let cut = elide_middle(&mut tr, path, &style, width);
        assert!(
            cut.starts_with("~/") && cut.ends_with("palette.rs"),
            "a path keeps both ends: {cut}"
        );
    }

    #[test]
    fn the_card_is_opaque_so_nothing_reads_through_its_rows() {
        let mut p = grouped(&[("Files", 2)]);
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let recording = crate::backdrop::BackdropRecording::begin();
        p.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::default(),
        );
        assert!(
            recording.finish().is_empty(),
            "the card samples no backdrop: it stands on the opaque panel ground"
        );
    }

    #[test]
    fn backspace_at_the_start_of_the_field_removes_the_scope_token() {
        let mut p = grouped(&[("Files", 2)]);
        p.set_scope_token(Some("Notes".into()));
        dispatch(&mut p, &typed("a"));
        dispatch(&mut p, &press(NamedKey::Backspace));
        assert_eq!(p.query(), "");
        assert!(
            !p.take_scope_token_removed(),
            "the first Backspace deletes the letter"
        );
        dispatch(&mut p, &press(NamedKey::Backspace));
        assert!(p.take_scope_token_removed(), "the next one takes the token");
        assert!(p.scope_token.is_none());
    }
}
