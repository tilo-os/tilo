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
use vello::kurbo::{Affine, Point, Rect, Vec2};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::icons::{self, Icon};
use crate::platform::{PlatformConventions, Shortcut};
use crate::presence::PresenceCtx;
use crate::search::{CommandRow, SearchGroup, SearchResult, SearchRow};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::{
    ChipFace, ChipLead, ControlSize, SearchField, SearchResultsView, TextInputKind, Widget,
};
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

pub use crate::search::{ActivationModifier, Alternate, RowFace};

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

/// The action target of the scope token when it is a control
/// ([`CommandPalette::set_scope_token_action`]).
const SCOPE_TOKEN_TARGET: &str = "command-palette:scope-token";

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
    /// A person (an identity, a collaborator) as `(name, key)`: painted as
    /// their avatar in place of `icon`, its initials from the name and its
    /// colour from the key.
    pub avatar: Option<(String, String)>,
    /// The people the row is about (who is in a workspace, in a call), as
    /// a stack of faces at its trailing end: the same faces they wear in
    /// the top bar and the window bar.
    pub faces: Vec<RowFace>,
    /// What choosing the row does, shown at its trailing edge ("Join",
    /// "Open"), for a row whose outcome is not its title.
    pub outcome: Option<String>,
    /// The section the row is in ("Happening now"), above its group: a
    /// header of its own, and a named group to assistive technology.
    pub section: Option<String>,
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
    /// The value `action_argument` takes when the row asks for none.
    action_preset: Option<String>,
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
            .field("avatar", &self.avatar)
            .field("faces", &self.faces)
            .field("outcome", &self.outcome)
            .field("section", &self.section)
            .field("on_select", &self.on_select.as_ref().map(|_| "<fn>"))
            .field("alternates", &self.alternates)
            .field("description_is_path", &self.description_is_path)
            .field("description_facts", &self.description_facts)
            .field("adjustable", &self.adjustable)
            .field("action_address", &self.action_address)
            .field("action_argument", &self.action_argument)
            .field("action_preset", &self.action_preset)
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
            avatar: None,
            faces: Vec::new(),
            outcome: None,
            section: None,
            on_select: None,
            alternates: Vec::new(),
            description_is_path: false,
            description_facts: Vec::new(),
            adjustable: false,
            action_address: None,
            action_argument: None,
            action_preset: None,
        }
    }

    /// Run the row's action ([`Self::from_flat_action`]) with `value` as its
    /// parameter `name`, without asking for it: a search hit the action
    /// opens names its item itself.
    #[must_use]
    pub fn with_action_preset(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.argument = None;
        self.action_argument = Some(name.into());
        self.action_preset = Some(value.into());
        self
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

    /// Show the row as a person ([`Self::avatar`]): `name` gives the
    /// initials, `key` (a stable identity) the colour.
    #[must_use]
    pub fn avatar(mut self, name: impl Into<String>, key: impl Into<String>) -> Self {
        self.avatar = Some((name.into(), key.into()));
        self
    }

    /// Show the people the row is about as faces at its end
    /// ([`Self::faces`]).
    #[must_use]
    pub fn faces(mut self, faces: Vec<RowFace>) -> Self {
        self.faces = faces;
        self
    }

    /// What choosing the row does, at its trailing edge.
    #[must_use]
    pub fn outcome(mut self, outcome: impl Into<String>) -> Self {
        self.outcome = Some(outcome.into());
        self
    }

    /// Put the row in section `section` ([`Self::section`]).
    #[must_use]
    pub fn in_section(mut self, section: impl Into<String>) -> Self {
        self.section = Some(section.into());
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
        // The action's own label, and what it acts on or means under it
        // (`tilo_actions::listed_row`): never "Feeds — New folder".
        let row = fa.row();
        let mut entry = CommandEntry::new(fa.key.clone(), row.title);
        if let Some(subtitle) = row.subtitle {
            entry = entry.description(subtitle);
        }
        if let ActionInput::Prompted {
            prompt,
            placeholder,
            ..
        } = fa.action.input()
        {
            let primary = fa.action.prompt_param().unwrap_or(PROMPTED_PARAM);
            entry = entry.with_argument(prompt.to_owned(), placeholder.to_owned());
            entry.action_argument = Some(primary.to_string());
        }
        entry.action_address = Some(ActionAddress::new(&fa.resource_id.0, &fa.action.id));
        entry
    }
}

/// While `Some`, the field collects the step's value instead of the query
/// (which is kept aside until the step ends), and the result list is
/// frozen.
struct ArgState {
    entry_id: String,
    kind: ArgumentKind,
    /// A step the host opened ([`CommandPalette::open_argument_step`]):
    /// Enter hands the value back through
    /// [`CommandPalette::take_submitted_argument`] and Escape reports
    /// [`CommandPalette::take_argument_cancelled`]; neither runs a row.
    host_step: bool,
}

impl ArgState {
    fn is_secret(&self) -> bool {
        self.kind == ArgumentKind::Secret
    }
}

/// Up (`direction` 1) or Down (-1) on a number field: `value` one step on,
/// inside its bounds. A field that holds no number starts from the bound it
/// moves away from, else from zero. `None` for any other kind.
fn step_number(kind: ArgumentKind, value: &str, direction: f64) -> Option<String> {
    let ArgumentKind::Number { min, max, step } = kind else {
        return None;
    };
    let current = value
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
    Some(format_number(next, step))
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
    /// The query field, and an argument step's field while one is open:
    /// the toolkit's own search field, so caret, selection, clipboard, IME
    /// and the scope token are the shared text core's.
    field: SearchField,
    /// The query, kept aside while an argument step uses the field.
    saved_query: String,
    /// Hint shown while the main query is empty. Hosts use this to name the
    /// resource being searched (files, commands, applications, ...).
    query_placeholder: String,
    filter: Option<EntryFilter>,
    visible: Vec<usize>,
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
    /// The result list: rows, headers, scrolling and their painting, fed
    /// from `visible` whenever the entry set, query or filter changes.
    /// Every geometry accessor reads it so hit-testing, scrolling and
    /// painting cannot disagree.
    results: SearchResultsView,
    /// Last pointer position in logical coordinates. The palette routes
    /// pointer moves to the cursor itself, so hover is the selection.
    last_cursor: Point,
    platform: PlatformConventions,
    /// Tallest the result list may grow before it scrolls; the host knows
    /// the display, the palette does not ([`Self::set_max_list_height`]).
    max_list_height: Option<f64>,
    /// The widest the card may be: the room the host has for it, so a
    /// narrow display shows all of it ([`Self::set_max_width`]).
    max_width: Option<f64>,
    /// The modifiers held now, so the selected row can show what its
    /// alternative activation would do before it is chosen.
    held: winit::keyboard::ModifiersState,
    /// A host step's confirmed value, until the host takes it.
    submitted_argument: Option<String>,
    /// A host step was backed out of with Escape, until the host notices.
    argument_cancelled: bool,
    /// Backspace at the start of the field took the token away, until the
    /// host notices ([`Self::take_scope_token_removed`]).
    scope_token_removed: bool,
    /// What pressing the token does, when it is a control: its accessible
    /// name ("Search every workspace"). `None`: Backspace alone removes it.
    scope_token_action: Option<String>,
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
            field: Self::query_field(&crate::i18n!("palette-placeholder")),
            saved_query: String::new(),
            query_placeholder: crate::i18n!("palette-placeholder"),
            filter: None,
            visible: Vec::new(),
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
            results: Self::result_list(),
            last_cursor: Point::ZERO,
            platform: PlatformConventions::default(),
            max_list_height: None,
            max_width: None,
            held: winit::keyboard::ModifiersState::empty(),
            submitted_argument: None,
            argument_cancelled: false,
            scope_token_removed: false,
            scope_token_action: None,
        }
    }

    fn result_list() -> SearchResultsView {
        let mut results = SearchResultsView::new();
        results.set_group_gap(f64::from(crate::tokens::RhythmScale::BASE.section));
        results
    }

    /// The query field: the card's own first row, embedded in its header,
    /// with no clear button (Escape clears).
    fn query_field(placeholder: &str) -> SearchField {
        let mut field = SearchField::new()
            .control_size(ControlSize::Large)
            .without_clear()
            .with_placeholder(placeholder);
        field.set_embedded(true);
        field
    }

    /// The query: the field's text, or what it held before an argument
    /// step took it.
    fn query_text(&self) -> &str {
        if self.arg_state.is_some() {
            &self.saved_query
        } else {
            self.field.value()
        }
    }

    /// Start collecting `spec`'s value for `entry_id` in the field, seeded
    /// with `prefill` (else the spec's own) and fully selected so the first
    /// keystroke replaces it.
    fn begin_argument(
        &mut self,
        entry_id: String,
        spec: ArgumentSpec,
        prefill: Option<String>,
        host_step: bool,
    ) {
        if self.arg_state.is_none() {
            self.saved_query = self.field.value().to_owned();
        }
        let value = prefill.or(spec.prefill).unwrap_or_default();
        let kind = match spec.kind {
            ArgumentKind::Number { .. } => TextInputKind::Number,
            ArgumentKind::Text | ArgumentKind::Secret => TextInputKind::Prose,
        };
        self.field
            .set_input_kind(kind, spec.kind == ArgumentKind::Secret);
        self.field.set_query(&value);
        self.field.set_selection(0, value.len());
        self.field.set_placeholder(&spec.placeholder);
        self.field
            .set_prompt((!spec.prompt.is_empty()).then_some(spec.prompt));
        let _ = self.field.take_dirty();
        self.arg_state = Some(ArgState {
            entry_id,
            kind: spec.kind,
            host_step,
        });
    }

    /// End any argument step: the field holds the query again.
    fn end_argument(&mut self) {
        if self.arg_state.take().is_none() {
            return;
        }
        // A secret is wiped while the field still knows it is one.
        self.field.set_query("");
        self.field.set_input_kind(TextInputKind::Search, false);
        self.field.set_query(&std::mem::take(&mut self.saved_query));
        self.field.set_placeholder(&self.query_placeholder);
        self.field.set_prompt(None);
        let _ = self.field.take_dirty();
    }

    /// Take or give up the keyboard; the list's selected row is focused to
    /// assistive technology exactly while the palette holds it.
    fn focus(&mut self, on: bool) {
        self.is_focused = on;
        self.results.set_focused(on);
    }

    /// The field holds the keyboard exactly while the palette does.
    fn sync_field_focus(&mut self) {
        if self.field.is_focused() != self.is_focused {
            self.field.set_focused(self.is_focused);
        }
    }

    /// Show what the search is narrowed to ("Notes") as a token at the
    /// start of the field, where the narrowing is read with the query
    /// instead of as a separate control. Backspace at the start of the
    /// field removes it; the host widens the scope
    /// ([`Self::take_scope_token_removed`]).
    pub fn set_scope_token(&mut self, token: Option<String>) {
        self.field.set_scope_token(token);
    }

    /// Make the scope token a control that widens the search when pressed
    /// or activated, named for that outcome (`action`); `None` leaves it a
    /// token Backspace removes. The host notices a press as it notices
    /// Backspace ([`Self::take_scope_token_removed`]).
    pub fn set_scope_token_action(&mut self, action: Option<String>) {
        self.scope_token_action = action;
    }

    /// Take the scope token away as Backspace would.
    fn remove_scope_token(&mut self) {
        if self.field.scope_token().is_some() {
            self.field.set_scope_token(None);
            self.scope_token_removed = true;
        }
    }

    /// The scope token the field shows, if any.
    pub fn scope_token(&self) -> Option<&str> {
        self.field.scope_token()
    }

    /// What pressing the scope token does, when it is a control.
    pub fn scope_token_action(&self) -> Option<&str> {
        self.scope_token_action.as_deref()
    }

    /// Whether Backspace removed the scope token since the last call.
    pub fn take_scope_token_removed(&mut self) -> bool {
        // The field's own × and Backspace take the token away too.
        let cleared = self.field.take_scope_cleared();
        std::mem::take(&mut self.scope_token_removed) || cleared
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
        self.sync_list();
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
        (self.field_only_while_empty && self.field.is_empty() && self.arg_state.is_none())
            || (self.arg_state.is_some() && self.visible.is_empty())
    }

    /// Leave any argument step without running or reporting anything: the
    /// host has moved on (to another step, or back to the results).
    pub fn cancel_argument(&mut self) {
        self.end_argument();
        self.direct_argument = false;
        self.submitted_argument = None;
        self.argument_cancelled = false;
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.visible.clear();
        self.replan_rows();
    }

    /// Clears the query and any in-progress argument, focuses the palette,
    /// and resets the highlight to the first entry.
    pub fn open(&mut self) {
        self.focus(true);
        self.end_argument();
        self.field.set_query("");
        let _ = self.field.take_dirty();
        self.sync_field_focus();
        self.direct_argument = false;
        self.outcome = None;
        self.submitted_argument = None;
        self.argument_cancelled = false;
        self.action_invocation.clear();
        self.rebuild();
        self.set_cursor(0);
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
        self.focus(true);
        self.outcome = None;
        self.direct_argument = true;
        self.begin_argument(
            id.into(),
            ArgumentSpec {
                prompt: prompt.into(),
                placeholder: placeholder.into(),
                prefill,
                kind: ArgumentKind::Text,
            },
            None,
            false,
        );
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
        self.focus(true);
        self.outcome = None;
        self.action_invocation.clear();
        self.direct_argument = true;
        self.begin_argument(id, spec, prefill, false);
    }

    /// Ask for one value in the field, for a host that collects an
    /// action's arguments one step at a time. The list stays where it is
    /// underneath. Enter hands the value to
    /// [`Self::take_submitted_argument`] and Escape sets
    /// [`Self::take_argument_cancelled`]; the host decides what follows.
    /// Neither runs a row, so a step can't stand in for one.
    pub fn open_argument_step(&mut self, id: impl Into<String>, spec: ArgumentSpec) {
        self.focus(true);
        self.outcome = None;
        self.submitted_argument = None;
        self.argument_cancelled = false;
        self.direct_argument = false;
        self.begin_argument(id.into(), spec, None, true);
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
        self.results.set_held_modifier(ActivationModifier::of(held));
        changed
    }

    /// Discards any in-progress argument entry but keeps registered entries,
    /// so the next `open` reuses them.
    pub fn close(&mut self) {
        self.focus(false);
        self.end_argument();
        self.direct_argument = false;
        self.sync_field_focus();
    }

    pub fn is_open(&self) -> bool {
        self.is_focused
    }

    pub fn in_argument_mode(&self) -> bool {
        self.arg_state.is_some()
    }

    pub fn argument_buffer(&self) -> Option<&str> {
        self.arg_state.as_ref().map(|_| self.field.value())
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
        let end = self.field.value().len();
        let at_end = self.field.selection_anchor_focus_or_caret() == (end, end);
        let Some(entry) = self
            .visible
            .get(self.results.cursor())
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
        self.sync_list();
    }

    /// The result list's rect (the card under its field and filter band,
    /// with the rows' padding) and cursor, from the card's.
    fn sync_list(&mut self) {
        // Before the first layout there is nothing to place the rows in;
        // a pending reveal waits for it.
        if self.rect.height() > 0.0 {
            self.results.set_rect(Rect::new(
                self.rect.x0,
                self.list_top_y() - PADDING,
                self.rect.x1,
                self.list_bottom_y() + PADDING,
            ));
        }
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
        let list = if self.visible.is_empty() {
            EMPTY_STATE_HEIGHT * self.layout_scale()
        } else {
            self.fitted_list_height()
        };
        (self.card_width(), self.chrome_height() + list)
    }

    /// The card's height less its result list: the field, the filter band
    /// and the padding around them. A host that keeps the card inside a
    /// region gives the list what is left of it
    /// ([`Self::set_max_list_height`]).
    pub fn chrome_height(&self) -> f64 {
        PADDING + (INPUT_HEIGHT * self.layout_scale()) + PADDING + self.filter_block() + PADDING
    }

    /// The tallest the result list may grow before it scrolls, in logical
    /// pixels. The host sets it from the display it shows the palette on.
    pub fn set_max_list_height(&mut self, height: f64) {
        self.max_list_height = Some(height.max(0.0));
    }

    /// The widest the card may be, in logical pixels: the host sets it from
    /// the room it shows the palette in, so on a phone the card narrows to
    /// fit rather than run off the screen. `None` keeps its own width.
    pub fn set_max_width(&mut self, width: Option<f64>) {
        self.max_width = width.map(|width| width.max(0.0));
    }

    /// How many result rows a full list holds: what a host asks its sources
    /// for, so the first screen is full and nothing is fetched that only
    /// scrolling could reach.
    pub fn entry_row_capacity(&self) -> usize {
        let row = self.row_height().max(1.0);
        crate::num::usize_of_f64((self.max_list_height() / row).floor()).max(1)
    }

    /// The whole rows from the top that fit under the cap: a cap that cut
    /// a row in half would leave a sliver nothing paints, and a header
    /// with no row under it is not painted.
    fn fitted_list_height(&self) -> f64 {
        self.results
            .fitted_height(self.max_list_height().max(self.row_height()))
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

    /// The list's nominal row: what capacity, the default cap and a wheel
    /// notch count in. Each row is as tall as its own lines.
    fn row_height(&self) -> f64 {
        let any_subtitle = self
            .visible
            .iter()
            .any(|&i| self.entries.get(i).is_some_and(|e| e.description.is_some()));
        self.results.row_band(any_subtitle)
    }

    fn card_width(&self) -> f64 {
        let width = match self.mode {
            PaletteMode::Search => SEARCH_CARD_WIDTH * self.layout_scale(),
            PaletteMode::Commands => CMD_CARD_WIDTH * self.layout_scale(),
        };
        self.max_width.map_or(width, |max| width.min(max))
    }

    /// Exactly what Enter does; hosts also wire pointer clicks here, so
    /// click and Enter share one path.
    pub fn activate_selected(&mut self, state: &mut crate::widget::Edits<'_>, path: &ObjId) {
        if self.arg_state.is_some() {
            return;
        }
        let Some(&idx) = self.visible.get(self.results.cursor()) else {
            return;
        };
        let Some(entry) = self.entries.get(idx) else {
            return;
        };
        let id = entry.id.clone();
        let argument = entry.argument.clone();
        if let Some(spec) = argument {
            self.begin_argument(id, spec, None, false);
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
            if let (Some(name), Some(value)) = (
                &entry.action_argument,
                argument.or_else(|| entry.action_preset.clone()),
            ) {
                arguments.insert(name.clone(), serde_json::Value::String(value));
            }
            self.action_invocation
                .emit(ControlInvocation { address, arguments });
            self.outcome = None;
            self.end_argument();
            if self.mode == PaletteMode::Commands {
                self.focus(false);
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
        self.end_argument();
        if self.mode == PaletteMode::Commands {
            self.focus(false);
        }
    }

    /// All keys are consumed so nothing leaks to the app behind: Enter,
    /// Escape and the arrows are the step's, the rest edit its field.
    fn handle_arg_key(
        &mut self,
        event: &WindowEvent,
        key: &Key,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        *ctx.handled = true;
        match key {
            Key::Named(NamedKey::Enter) => {
                let Some(arg) = self.arg_state.as_ref() else {
                    return;
                };
                let value = self.field.value().to_owned();
                if arg.host_step {
                    self.end_argument();
                    self.submitted_argument = Some(value);
                    return;
                }
                let id = arg.entry_id.clone();
                self.run_entry(&mut ctx.state, ctx.path, id, Some(value));
            }
            Key::Named(NamedKey::Escape) => {
                if self.arg_state.as_ref().is_some_and(|arg| arg.host_step) {
                    self.argument_cancelled = true;
                }
                self.end_argument();
                if self.direct_argument {
                    self.focus(false);
                    self.direct_argument = false;
                    self.sync_field_focus();
                }
            }
            // No list navigation while typing an argument; a number steps.
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                let up = matches!(key, Key::Named(NamedKey::ArrowUp));
                let Some(kind) = self.arg_state.as_ref().map(|arg| arg.kind) else {
                    return;
                };
                if let Some(next) =
                    step_number(kind, self.field.value(), if up { 1.0 } else { -1.0 })
                {
                    self.field.set_query(&next);
                    let _ = self.field.take_dirty();
                }
            }
            _ => {
                self.field.handle_event(event, ctx);
                let _ = self.field.take_dirty();
                *ctx.handled = true;
            }
        }
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
        self.rebuild();
        self.set_cursor(0);
    }

    /// Show host feedback on the selected row using the shared status pill and
    /// description. Keep the query, visible order and cursor so an unsuccessful
    /// invocation can be corrected or retried. A new entry set replaces feedback.
    pub fn set_selected_status(
        &mut self,
        badge: impl Into<String>,
        description: impl Into<String>,
    ) {
        let Some(&index) = self.visible.get(self.results.cursor()) else {
            return;
        };
        if let Some(entry) = self.entries.get_mut(index) {
            entry.badge = Some(badge.into());
            entry.description = Some(description.into());
        }
        let groups = self.list_groups();
        self.results.replace_groups(groups);
        self.sync_list();
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
        self.sync_list();
    }

    /// Show or hide the filter band. A host hides it until the results
    /// have something to tell apart, so it isn't standing chrome over every
    /// search. Returns whether that changed.
    pub fn set_filter_band_shown(&mut self, shown: bool) -> bool {
        let changed = self.hide_filter_strip == shown;
        self.hide_filter_strip = !shown;
        self.sync_list();
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
        self.sync_list();
        changed
    }

    /// Shared by every band of the card — input field, filter strip and
    /// rows.
    pub fn content_inset_px(&self) -> f64 {
        PADDING
    }

    /// Replace the query; the caret lands at its end so typing appends.
    pub fn set_query(&mut self, q: &str) {
        if self.arg_state.is_some() {
            q.clone_into(&mut self.saved_query);
        } else {
            self.field.set_query(q);
            let _ = self.field.take_dirty();
        }
        self.rebuild();
    }

    pub fn query(&self) -> &str {
        self.query_text()
    }

    pub fn set_query_placeholder(&mut self, placeholder: impl Into<String>) {
        self.query_placeholder = placeholder.into();
        if self.arg_state.is_none() {
            self.field.set_placeholder(&self.query_placeholder);
        }
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
        self.results.cursor()
    }

    /// Clamps silently so overlay callers hit-testing post-layout rects
    /// don't have to bounds-check.
    pub fn set_cursor(&mut self, index: usize) {
        self.results.set_cursor(index);
        self.sync_list();
    }

    /// `None` if the filter matched no rows.
    pub fn selected_id(&self) -> Option<&str> {
        self.visible
            .get(self.cursor())
            .and_then(|&i| self.entries.get(i))
            .map(|e| e.id.as_str())
    }

    /// Scroll the list by `dy` logical px (down for positive), keeping the
    /// cursor on a row in view so the list stays where it was put.
    fn scroll_list(&mut self, dy: f64) {
        self.sync_list();
        if !self.results.scroll_by(dy) {
            return;
        }
        let shown = self.results.shown_rows();
        if shown.is_empty() {
            return;
        }
        let cursor = self
            .cursor()
            .clamp(shown.start, shown.end.saturating_sub(1));
        self.results.set_cursor(cursor);
        self.sync_list();
    }

    /// `None` when the row is scrolled out of view.
    pub fn row_rect(&self, i: usize) -> Option<Rect> {
        if i >= self.visible.len() || self.shows_field_only() {
            return None;
        }
        self.results.row_rect(i)
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
        self.results.header_at(p)
    }

    /// The id of the row drawn at `p`.
    pub fn id_at(&self, p: Point) -> Option<&str> {
        let row = self.row_at(p)?;
        self.visible
            .get(row)
            .and_then(|&i| self.entries.get(i))
            .map(|e| e.id.as_str())
    }

    pub fn row_at(&self, p: Point) -> Option<usize> {
        if self.shows_field_only() {
            return None;
        }
        self.results
            .row_at(p)
            .filter(|&row| row < self.visible.len())
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
            // The pointer picks the row it is over where it stands: a row
            // half in view is not scrolled out from under it.
            if let Some(row) = self.row_at(self.last_cursor) {
                self.results.set_cursor_in_place(row);
                *handled = true;
            }
            // The field follows the pointer too, for its caret and drags.
            return false;
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

    /// The scope chips, the filetype chips that fit, and the full-text
    /// toggle at the strip's end: the toolkit's chips, selected when on.
    fn draw_filter_strip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let strip = self.filter_strip_rect();
        let gap = f64::from(theme.tokens.spacing.s2);
        let h = ChipFace::height(theme);
        let y0 = f64::midpoint(strip.y0, strip.y1) - h / 2.0;
        let mut chip = |x: f64, label: &str, on: bool, text: &mut TextRenderer| {
            let w = ChipFace::intrinsic_width(label, ChipLead::None, false, text, theme);
            ChipFace::new(Rect::new(x, y0, x + w, y0 + h), label)
                .selected(on)
                .paint(scene, transform, text, theme);
            w
        };
        let ft_label = "FULL TEXT";
        let ft_w = ChipFace::intrinsic_width(ft_label, ChipLead::None, false, text_renderer, theme);
        let ft_x = strip.x1 - ft_w;
        chip(ft_x, ft_label, self.search_filter.full_text, text_renderer);
        let mut x = strip.x0;
        for (scope, label) in [
            (SearchScope::Workspace, "WORKSPACE"),
            (SearchScope::All, "ALL"),
        ] {
            x += chip(x, label, self.search_filter.scope == scope, text_renderer) + gap;
        }
        // Filetype chips fill the gap up to the full-text toggle.
        for ft in Filetype::ALL {
            let label = ft.label();
            let w = ChipFace::intrinsic_width(label, ChipLead::None, false, text_renderer, theme);
            if x + w > ft_x - gap {
                break;
            }
            x += chip(
                x,
                label,
                self.search_filter.filetypes.contains(&ft),
                text_renderer,
            ) + gap;
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
            && !self.query_text().is_empty()
    }

    fn empty_results_title(&self) -> String {
        if self.is_searching() {
            crate::i18n!("palette-searching")
        } else if self.query_text().is_empty() {
            crate::i18n!("palette-empty")
        } else {
            crate::i18n!("palette-no-results", "query" => self.query_text())
        }
    }

    /// The result list's rows, inside the card's padding.
    fn list_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PADDING,
            self.list_top_y(),
            self.rect.x1 - PADDING,
            self.list_bottom_y(),
        )
    }

    /// Moves through the results: arrows step and wrap, Page keys move a
    /// view's worth, Tab and Shift+Tab jump to the next and previous
    /// group, and Home and End reach the first and last result when the
    /// caret has nowhere left to go in the field (always with Ctrl).
    /// `true` when the key moved the selection or belonged to the list.
    pub fn handle_list_key(&mut self, key: &Key, shift: bool, ctrl: bool) -> bool {
        let caret = self.field.selection_anchor_focus_or_caret();
        let end = self.field.value().len();
        let owned =
            self.results
                .handle_list_key(key, shift, ctrl, (caret == (0, 0), caret == (end, end)));
        if owned {
            self.sync_list();
        }
        owned
    }

    fn rebuild(&mut self) {
        self.visible.clear();
        let matching = (!self.bypass_fuzzy).then(|| {
            self.entry_index.ranks(
                self.query_text(),
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
        self.replan_rows();
    }

    /// Feed the result list the visible entries, read from their top.
    fn replan_rows(&mut self) {
        let groups = self.list_groups();
        self.results.set_groups(groups);
        let query = self.query_text().to_owned();
        self.results.set_highlight(&query);
        self.sync_list();
    }

    /// The visible entries as the result list's groups: a run of entries
    /// sharing a group under its header with the group's count, entries in
    /// a section under the section's header with their groups as captions,
    /// and an entry with neither running on under the rows above it.
    fn list_groups(&self) -> Vec<SearchGroup> {
        let mut groups: Vec<SearchGroup> = Vec::new();
        let mut current: Option<&str> = None;
        let mut section: Option<&str> = None;
        for (entry_i, &idx) in self.visible.iter().enumerate() {
            let Some(entry) = self.entries.get(idx) else {
                continue;
            };
            let mut opened: Option<SearchGroup> = None;
            if let Some(name) = entry.section.as_deref() {
                if section != Some(name) {
                    section = Some(name);
                    current = Some(name);
                    opened = Some(SearchGroup {
                        section: Some(name.to_owned()),
                        ..SearchGroup::new("", Vec::new())
                    });
                }
                if let Some(group) = entry.group.as_deref()
                    && current != Some(group)
                {
                    current = Some(group);
                    opened = Some(SearchGroup {
                        section: Some(name.to_owned()),
                        ..SearchGroup::new(group, Vec::new())
                    });
                }
            } else {
                section = None;
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
                    opened = Some(SearchGroup {
                        total: Some(total),
                        ..SearchGroup::new(group, Vec::new())
                    });
                }
            }
            let row = self.list_result(entry);
            match (opened, groups.last_mut()) {
                (Some(mut group), _) => {
                    group.results.push(row);
                    groups.push(group);
                }
                (None, Some(last)) if last.section.as_deref() == entry.section.as_deref() => {
                    last.results.push(row);
                }
                (None, _) => groups.push(SearchGroup {
                    section: entry.section.clone(),
                    ..SearchGroup::new("", vec![row])
                }),
            }
        }
        groups
    }

    /// An entry as a result list row.
    fn list_result(&self, entry: &CommandEntry) -> SearchResult {
        let mut result = SearchResult::new(
            entry.group.clone().unwrap_or_default(),
            entry.label.clone(),
            entry.id.clone(),
        );
        result.subtitle.clone_from(&entry.description);
        result.icon = entry.icon;
        result.row = Some(SearchRow::Command(Box::new(CommandRow {
            description_is_path: entry.description_is_path,
            facts: entry.description_facts.clone(),
            image: entry.image.clone(),
            avatar: entry.avatar.clone(),
            badge: entry.badge.clone(),
            keycaps: entry.keycap_labels(self.platform),
            faces: entry.faces.clone(),
            outcome: entry.outcome.clone(),
            alternates: entry.alternates.clone(),
        })));
        result
    }
}

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
        if self.platform != conventions {
            self.platform = conventions;
            // Keycaps follow the platform's notation.
            let groups = self.list_groups();
            self.results.replace_groups(groups);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.text_scale = f64::from(theme.text_scale);
        self.ui_scale = f64::from(theme.ui_scale);
        self.results.resolve_metrics(theme);
        self.results
            .set_group_gap(f64::from(theme.tokens.spacing.rhythm.section));
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

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.sync_list();
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Sized by its style alone (no `measure`): nothing it changes needs
    /// taffy to measure it again, and saying it does re-solved the layout of
    /// everything above it on every frame.
    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Theme typography is already scaled. Keep all legacy text paint and
        // metric calls in that same space, restoring the child renderer after.
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        self.text_scale = f64::from(theme.text_scale);
        self.ui_scale = f64::from(theme.ui_scale);
        self.results.resolve_metrics(theme);
        self.results
            .set_group_gap(f64::from(theme.tokens.spacing.rhythm.section));
        self.sync_list();
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

        // No shortcut chip here: a kbd hint invites a chord you've already
        // pressed by now, and two different chords open this palette at two
        // scopes — one label would misname one of them.
        self.sync_field_focus();
        self.field.apply_layout_external(row_rect);
        self.field
            .draw(scene, transform, text_renderer, theme, presence);
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
            self.results
                .draw_list(scene, transform, text_renderer, theme);
        }

        text_renderer.set_text_scale(renderer_scale);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.is_focused {
            return;
        }
        self.sync_field_focus();
        self.sync_list();
        // Read, never consumed: every other surface needs modifiers too.
        if let WindowEvent::ModifiersChanged(modifiers) = event {
            self.set_held_modifiers(modifiers.state());
            return;
        }
        if let WindowEvent::MouseWheel { delta, .. } = event
            && self.rect.contains(self.last_cursor)
            && self.arg_state.is_none()
        {
            // A notch moves the list a row.
            let dy = match *delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => {
                    -f64::from(y) * self.row_height()
                }
                winit::event::MouseScrollDelta::PixelDelta(p) => -p.y / ctx.scale_factor,
            };
            self.scroll_list(dy);
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
        if matches!(
            event,
            WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
        ) {
            // Presses on the field place its caret or take the scope token.
            let pressed = matches!(event, WindowEvent::MouseInput { .. });
            if !pressed || self.input_row_rect().contains(self.last_cursor) {
                self.edit_field(event, ctx);
            }
            return;
        }
        if matches!(event, WindowEvent::Ime(_)) {
            self.edit_field(event, ctx);
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
                self.handle_arg_key(event, logical_key, ctx);
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
            match logical_key {
                Key::Named(NamedKey::Enter) => {
                    self.activate_selected(&mut ctx.state, ctx.path);
                    *ctx.handled = true;
                }
                // A typed query is cleared first; only an empty field closes,
                // so a slip of the finger never costs the whole search.
                Key::Named(NamedKey::Escape) => {
                    if self.field.is_empty() {
                        self.focus(false);
                        self.sync_field_focus();
                    } else {
                        self.set_query("");
                    }
                    *ctx.handled = true;
                }
                _ => self.edit_field(event, ctx),
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
        self.focus(focused);
        self.sync_field_focus();
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
        self.field.ime_cursor_rect()
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        let field = self.field.ime_edit_fingerprint(key)?;
        Some(match &self.arg_state {
            Some(arg) => crate::ime::scoped_fingerprint(arg.entry_id.as_bytes(), field, key),
            None => field,
        })
    }

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        self.field.ime_content()
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

    fn accessibility_hidden(&self) -> bool {
        !self.is_open()
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
                hidden: self.accessibility_hidden(),
                ..Default::default()
            });
        if !open {
            return Some(node);
        }

        let name = match &self.arg_state {
            Some(_) => self.field.prompt().unwrap_or_default().to_owned(),
            None => self.query_placeholder.clone(),
        };
        // The caret makes the field a text field for assistive technology
        // (AT-SPI Text and EditableText), not only a labelled value: a screen
        // reader can then read and set the query as it would any other input.
        let (anchor, caret) = self.field.selection_anchor_focus_or_caret();
        // A secret reports that it is protected and never its value.
        let secret = self.arg_state.as_ref().is_some_and(ArgState::is_secret);
        let mut query_node = AccessNode::new(Role::TextInput)
            .with_action_target("command-palette:query")
            .with_name(name)
            .with_bounds(self.input_row_rect())
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.is_focused)
                    .protected(secret),
            );
        if !secret {
            query_node = query_node
                .with_value(self.field.value().to_owned())
                .with_text_selection(anchor, caret);
        }

        let mut list = self
            .results
            .accessibility()
            .unwrap_or_else(|| AccessNode::new(Role::List));
        if self.visible.is_empty() {
            list.children = if self.arg_state.is_none() {
                vec![
                    AccessNode::new(Role::StaticText)
                        .with_name(self.empty_results_title())
                        .with_description(crate::i18n!("palette-no-results-hint"))
                        .with_bounds(self.list_rect()),
                ]
            } else {
                Vec::new()
            };
        }
        let mut children = vec![
            query_node,
            list.with_name(crate::i18n!("palette-results"))
                .with_bounds(self.list_rect()),
        ];
        if let (Some(token), Some(action)) = (self.field.scope_token(), &self.scope_token_action) {
            children.push(
                AccessNode::new(Role::Button)
                    .with_action_target(SCOPE_TOKEN_TARGET)
                    .with_name(action.clone())
                    .with_description(token.to_owned())
                    .with_bounds(self.field.scope_token_rect())
                    .with_state(AccessState::new().focusable(true)),
            );
        }
        node = node.with_children(children);
        Some(node)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let mut node = self.accessibility()?;
        for &index in path {
            node = node.children.into_iter().nth(index)?;
        }
        node.bounds.map(crate::a11y::AccessBounds::to_rect)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let target = match path {
            [0] => None,
            [1, ..] => {
                let Some(target) = self.accessibility().and_then(|mut node| {
                    for &index in path {
                        node = node.children.into_iter().nth(index)?;
                    }
                    node.action_target
                }) else {
                    return false;
                };
                Some(target)
            }
            _ => return false,
        };
        if let Some(target) = target
            && !self.results.select_action(&target)
        {
            return false;
        }
        self.focus(true);
        true
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
                self.focus(true);
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

        if target == SCOPE_TOKEN_TARGET {
            return match action {
                Action::Activate if self.scope_token_action.is_some() => {
                    self.remove_scope_token();
                    true
                }
                _ => false,
            };
        }
        if target == "command-palette:query" {
            return match action {
                Action::Focus | Action::Activate => {
                    self.focus(true);
                    true
                }
                // Assistive technology and agents set the query as a value,
                // exactly as typing it would.
                Action::SetValue(value) if self.arg_state.is_none() => {
                    self.focus(true);
                    self.set_query(value);
                    true
                }
                _ => false,
            };
        }
        if !self
            .results
            .perform_semantic_action(target, action, state, path)
        {
            return false;
        }
        self.focus(true);
        if self.results.take_activated().is_some() {
            self.activate_selected(state, path);
        }
        true
    }
}

impl CommandPalette {
    /// Hand `event` to the field; a query it edited narrows the list.
    fn edit_field(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.field.handle_event(event, ctx);
        if self.field.take_dirty().is_some() && self.arg_state.is_none() {
            self.rebuild();
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
#[path = "command_palette_tests.rs"]
mod tests;

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
        let page = p.results.rows_per_page();
        assert!(page > 1, "a page is more than one row");
        assert!(dispatch(&mut p, &press(NamedKey::PageDown)));
        assert_eq!(p.cursor(), page.saturating_add(2));
        let before = p.cursor();
        let page = p.results.rows_per_page();
        assert!(dispatch(&mut p, &press(NamedKey::PageUp)));
        assert_eq!(
            p.cursor(),
            before.saturating_sub(page),
            "PageUp moves a view's worth back"
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
        assert_eq!(p.field.selection_anchor_focus_or_caret(), (0, 0));
        dispatch(&mut p, &press(NamedKey::Home));
        assert_eq!(p.cursor(), 0, "Home again reaches the first result");
        with(&mut p, ModifiersState::CONTROL, &press(NamedKey::End));
        assert_eq!(p.cursor(), 4, "Ctrl+End always reaches the last result");
    }

    #[test]
    fn the_list_scrolls_only_when_the_cursor_leaves_it_and_brings_a_groups_header_along() {
        let mut p = grouped(&[("Apps", 3), ("Files", 40)]);
        let shown = p.results.shown_rows();
        assert_eq!(shown.start, 0);
        // Down to the bottom of the view, then one past it.
        let last_shown = shown.end.saturating_sub(1);
        p.set_cursor(last_shown.saturating_add(1));
        assert!(
            p.results.shown_rows().start > 0,
            "leaving the view scrolls it"
        );
        let scrolled = p.row_rect(last_shown);
        // Back up one row: still inside the view, which stays put.
        p.set_cursor(last_shown);
        assert_eq!(
            p.row_rect(last_shown),
            scrolled,
            "moving inside the view keeps it"
        );
        // Wherever it stands, the cursor's row shows whole.
        for cursor in 0..p.visible.len() {
            p.set_cursor(cursor);
            assert!(
                p.results.shown_rows().contains(&cursor),
                "cursor {cursor} is out of view"
            );
        }
        // Moving up into a group brings its header along.
        p.set_cursor(3);
        let files = p.row_rect(3).expect("Files' first row shows");
        assert_eq!(
            p.header_at(Point::new(files.center().x, files.y0 - 1.0))
                .as_deref(),
            Some("Files")
        );
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
        p.set_max_list_height(20.0 * p.row_height() + p.results.header_band());
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
        assert!(
            p.results.shown_rows().start >= 3,
            "the wheel scrolls the list"
        );
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
        assert!(
            p.field.ime_content() == Some(crate::ime::InputContent::PASSWORD),
            "the field masks what it holds"
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
        let Some(SearchRow::Command(row)) = p.list_result(&entry).row else {
            panic!("an entry is a command row");
        };
        assert_eq!(
            row.shown_alternate(p.held_modifier())
                .map(|a| a.label.as_str()),
            Some("As a tab here"),
            "the first is the hint while nothing is held"
        );
        let held = winit::event::Modifiers::from(ModifiersState::CONTROL);
        dispatch(&mut p, &WindowEvent::ModifiersChanged(held));
        assert_eq!(p.held_modifier(), Some(ActivationModifier::Ctrl));
        assert_eq!(
            row.shown_alternate(p.held_modifier())
                .map(|a| a.label.as_str()),
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
        p.field.set_query("vol");
        p.field.set_selection(1, 1);
        assert!(
            !p.step_selected(&Key::Named(NamedKey::ArrowRight)),
            "inside the query the arrows move the caret"
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
        assert!(p.scope_token().is_none());
    }

    #[test]
    fn a_scope_token_that_widens_is_a_control_for_the_pointer_and_assistive_technology() {
        let mut p = grouped(&[("Files", 2)]);
        p.set_rect(Rect::new(0.0, 0.0, 480.0, 400.0));
        p.set_scope_token(Some("Workspace".into()));
        p.set_scope_token_action(Some("Search every workspace".into()));
        p.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::default(),
        );
        let token = p.field.scope_token_rect();
        assert!(token.width() > 0.0, "the token stands in the field");
        let tree = p
            .accessibility()
            .expect("the open palette describes itself");
        let button = tree
            .children
            .iter()
            .find(|child| child.role == crate::a11y::Role::Button)
            .expect("the token is a button");
        assert_eq!(button.name.as_deref(), Some("Search every workspace"));
        let at = token.center();
        dispatch(&mut p, &crate::synthetic_input::cursor_moved(at.x, at.y));
        dispatch(&mut p, &crate::synthetic_input::mouse_press());
        assert!(p.take_scope_token_removed(), "a press widens");
        assert!(p.scope_token().is_none());
    }

    #[test]
    fn a_list_cut_under_a_header_ends_on_the_row_above_it() {
        let mut p = CommandPalette::new();
        p.set_bypass_fuzzy(true);
        p.set_entries(vec![
            CommandEntry::new("a", "Pitch deck").in_section("Continue"),
            CommandEntry::new("b", "Notes").in_section("Apps"),
        ]);
        let gap = f64::from(crate::tokens::RhythmScale::BASE.section);
        let header = p.results.header_band();
        p.set_max_list_height(header * 2.0 + gap + p.row_height());
        assert_eq!(
            p.fitted_list_height(),
            header + p.row_height(),
            "the Apps header has no row under it, so the list ends on Pitch deck"
        );
    }

    #[test]
    fn a_section_after_rows_stands_the_themes_section_gap_below_them() {
        let mut p = CommandPalette::new();
        p.set_bypass_fuzzy(true);
        p.set_entries(vec![
            CommandEntry::new("bo", "Bo").in_section("Waiting on you"),
            CommandEntry::new("doc", "Plan").in_section("Continue"),
        ]);
        let (w, h) = p.desired_size();
        p.set_rect(Rect::new(0.0, 0.0, w, h));
        let headers = p.results.header_list();
        let first_row = p.row_rect(0).expect("Bo shows");
        let gap = f64::from(crate::tokens::RhythmScale::BASE.section);
        assert_eq!(headers.len(), 2);
        assert!(
            (headers[0].3.y0 - p.list_top_y()).abs() < 0.01,
            "the first header opens the list"
        );
        assert!(
            (headers[1].3.y0 - first_row.y1 - gap).abs() < 0.01,
            "the next stands a section gap below the row above it"
        );
    }

    #[test]
    fn rows_in_sections_are_named_groups_under_their_sections_header() {
        let mut p = CommandPalette::new();
        p.set_bypass_fuzzy(true);
        p.set_entries(vec![
            CommandEntry::new("home", "Home")
                .group("Happening now")
                .in_section("Happening now")
                .outcome("Open"),
            CommandEntry::new("calc", "Calculator")
                .group("Utilities")
                .in_section("Apps"),
            CommandEntry::new("notes", "Notes")
                .group("Office")
                .in_section("Apps"),
        ]);
        use crate::widget::search_results::HeaderLevel;
        let headers: Vec<(String, HeaderLevel)> = p
            .results
            .header_list()
            .into_iter()
            .map(|(label, _, level, _)| (label, level))
            .collect();
        assert_eq!(
            headers,
            [
                ("Happening now".to_owned(), HeaderLevel::Section),
                ("Apps".to_owned(), HeaderLevel::Section),
                ("Utilities".to_owned(), HeaderLevel::Subgroup),
                ("Office".to_owned(), HeaderLevel::Subgroup),
            ],
            "a section's own group needs no second header; Apps has its header and its purposes"
        );
        p.set_rect(Rect::new(20.0, 30.0, 380.0, 400.0));
        p.open();
        let tree = p
            .accessibility()
            .expect("the open palette describes itself");
        let groups: Vec<(Option<&str>, usize)> = tree.children[1]
            .children
            .iter()
            .map(|group| {
                assert_eq!(group.role, crate::a11y::Role::Group);
                (group.name.as_deref(), group.children.len())
            })
            .collect();
        assert_eq!(groups, [(Some("Happening now"), 1), (Some("Apps"), 2)]);
        assert_eq!(p.synthetic_accessibility_rect(&[1, 1, 1]), p.row_rect(2));
    }

    #[test]
    fn a_rows_facts_are_its_description_to_assistive_technology() {
        let mut p = CommandPalette::new();
        p.push(
            CommandEntry::new("pitch", "Pitch deck")
                .facts(vec!["Notes".into(), "Opened 4 minutes ago".into()]),
        );
        p.set_rect(Rect::new(20.0, 30.0, 380.0, 310.0));
        p.open();
        let tree = p
            .accessibility()
            .expect("the open palette describes itself");
        let row = tree.children[1].children.first().expect("the row");
        assert_eq!(row.name.as_deref(), Some("Pitch deck"));
        assert_eq!(
            row.description.as_deref(),
            Some("Notes, Opened 4 minutes ago"),
            "the reason a row is there is read with it"
        );
    }
}
