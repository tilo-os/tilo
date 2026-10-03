//! Single-line (optionally multiline) text input; edits replay through
//! `AutoCommit::splice_text`.
//!
//! Text and caret both centre against the same `line_height = font_size *
//! 1.25` band (see `text_baseline_centred_with_caret`) so glyphs don't drift
//! below the caret.

use crate::time::{Duration, Instant};
use crate::widget::{AppModel, ModelPath};
use std::collections::HashSet;
use std::ops::Range;
use std::sync::{Arc, Mutex};

use crate::color_compat::ColorCompat;
use crate::gesture::{DragSelect, Granularity};
use crate::paint::PaintCtx;
use crate::presence::{PathSegment, PresenceKey};
use crate::richtext::{RichSpan, RichTextLayout, TextAlign};
use crate::text_break::line_break_opportunities;
use crate::text_break::{grapheme_indices, graphemes};
use crate::text_render::{FontChoice, TextKind, TextRenderer, TextScaling, TextStyle, TextWeight};
use crate::text_selection::{
    TextAffinity, TextPosition, clipboard_copy, clipboard_copy_primary, clipboard_paste,
    clipboard_paste_primary, extend_span_drag, grapheme_boundary_at_or_before, line_boundaries,
    next_char_boundary, next_word_boundary, prev_char_boundary, prev_word_boundary,
    word_boundaries,
};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::code_editor::{Highlighter, Span};
use crate::widget::context_menu::{ContextMenu, MenuItem};
use crate::widget::{ActionEmitter, ControlBindingKind, ControlBindingMetadata};
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value, transaction::Transactable};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::BlendMode;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, TouchPhase, WindowEvent};
use winit::keyboard::{Key, NamedKey};
use zeroize::Zeroize;

/// `MenuItem` callbacks are `Arc<dyn Fn>` and can't mutate the widget, so
/// clicks dispatch through this parallel list by `item_at` index instead.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CtxAction {
    Cut,
    Copy,
    Paste,
    SelectAll,
    ApplySpelling {
        start: usize,
        end: usize,
        replacement: String,
    },
    AddToDictionary(String),
    IgnoreInField(String),
    UndoAutocorrection,
    TextAction(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RegisteredTextAction {
    id: String,
    label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Autocorrection {
    start: usize,
    end: usize,
    boundary_end: usize,
    original: String,
    replacement: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AutocorrectionPreview {
    language: String,
    start: usize,
    end: usize,
    original: String,
    replacement: String,
}

/// A selected-text command requested from a field's context menu. The host can
/// execute this asynchronously (translate, rewrite, explain, grammar review)
/// without putting model or network work in the widget's input path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextActionRequest {
    pub id: String,
    pub text: String,
    pub range: (usize, usize),
}

/// Semantic intent of a field. Ordinary [`TextInput`] construction defaults to
/// Prose, so apps opt out only for the smaller set of structured fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextInputKind {
    #[default]
    Prose,
    Name,
    Search,
    Username,
    Email,
    Url,
    Path,
    Code,
    Number,
    Password,
    /// A code or key typed or pasted in the clear: verification and
    /// recovery codes, TOTP secrets, API and licence keys, Wi-Fi keys.
    Secret,
}

impl TextInputKind {
    fn checks_spelling(self) -> bool {
        matches!(self, Self::Prose)
    }

    /// What the field's text is, for the features it is set with while
    /// typing.
    #[must_use]
    pub const fn text_kind(self) -> TextKind {
        match self {
            Self::Prose | Self::Name | Self::Search => TextKind::Prose,
            Self::Username | Self::Email | Self::Url | Self::Path => TextKind::Identifier,
            Self::Code => TextKind::Code,
            Self::Number => TextKind::Numeric,
            Self::Password | Self::Secret => TextKind::Secret,
        }
    }
}

fn is_autocorrect_boundary(character: char) -> bool {
    character.is_whitespace() || (character.is_ascii_punctuation() && character != '\'')
}

/// Caret centres against this band, not the font size, to line up with where
/// cosmic-text draws the glyphs.
const LINE_HEIGHT_RATIO: f32 = 1.25;
/// Spellcheck runs immediately, but the word containing the caret stays
/// visually quiet for this long after an edit so its underline does not
/// flicker while the user is still forming it.
const SPELL_EDIT_SETTLE: Duration = Duration::from_millis(250);
/// External Hunspell sessions are persistent but still use IPC. Coalesce the
/// prefixes of a rapidly typed non-English word while preserving prior spans.
const EXTERNAL_SPELLCHECK_DELAY: Duration = Duration::from_millis(100);
const ML_PAD_X: f64 = 10.0;
const ML_PAD_Y: f64 = 8.0;
const GUTTER_DIGIT_RATIO: f64 = 0.62;
const GUTTER_PAD_L: f64 = 8.0;
const GUTTER_PAD_R: f64 = 12.0;

/// Unit the in-flight drag snaps to, set from the click ladder on press.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum DragUnit {
    Char,
    Word,
    Line,
}

type TextChangeCallback = Box<dyn FnMut(&str)>;

/// Complete committed-text runs for an unmasked single line. Decorations use
/// byte ranges in that same text; IME insertion remaps them only for display.
#[derive(Clone, Debug, Default)]
pub(super) struct SingleLineAttributes {
    pub spans: Vec<RichSpan>,
    pub underlines: Vec<(usize, usize)>,
    pub strikes: Vec<(usize, usize)>,
    pub chips: Vec<(usize, usize)>,
    pub links: Vec<(usize, usize)>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent widget-configuration or ephemeral-UI-state flag (hover/focus/drag/dirty/secret/etc), not a state machine; grouping them would misrepresent orthogonal concerns and ripple through every accessor in this file"
)]
pub struct TextInput {
    id: String,
    path: Vec<PathSegment>,
    rect: Rect,
    is_hovered: bool,
    is_focused: bool,
    /// Paint-only suppression for drag previews; focus, selection and IME remain intact.
    caret_paint_suppressed: bool,
    text: String,
    /// Optional per-character admission rule for domain-specific single-line
    /// fields (for example a spreadsheet cell-reference box). Applied to
    /// keyboard, IME, paste, accessibility, and programmatic insertion alike.
    char_filter: Option<fn(char) -> bool>,
    /// Always on a char boundary.
    cursor_pos: usize,
    /// Visual side of `cursor_pos` when a `BiDi` boundary or soft wrap has two.
    caret_affinity: TextAffinity,
    /// Visible range is `(min(anchor, cursor), max(anchor, cursor))`.
    selection_anchor: Option<usize>,
    /// Forces `wants_pointer_capture` so a drag past the rect edge keeps updating.
    is_dragging: bool,
    /// Exclusive finger ownership for native touch selection. Mouse state is
    /// deliberately separate so a secondary finger cannot steal or complete
    /// an in-progress selection gesture.
    active_touch: Option<u64>,
    drag_unit: DragUnit,
    /// Anchor span snapped to `drag_unit` boundaries; the drag extends from
    /// whichever end is "opposite" the current pointer.
    drag_anchor_span: (usize, usize),
    /// Session-configured click counting shared by every editable surface.
    drag_select: DragSelect,
    /// Cleared on `take_dirty()`/`set_text()` — a programmatic write isn't a user edit.
    dirty: bool,
    /// App-local notification hook. The comparison value prevents cursor,
    /// selection, and IME-preedit events from repeating the last edit.
    on_change: Option<TextChangeCallback>,
    action: Option<ActionEmitter<String>>,
    last_notified_text: String,
    /// Typed process-local model mirrored in both directions during the
    /// ordinary widget sync/event lifecycle.
    local_state: Option<crate::local_state::LocalBinding<String>>,
    /// A programmatic/model change can alter multiline measurement without
    /// pretending to be a user edit through `take_dirty`.
    model_layout_dirty: bool,
    /// Gates whether edits splice into `state`, materialising a missing path
    /// lazily on the first effective edit.
    crdt_bound: bool,
    /// Action-owned parents can retain a bound text object for reconciliation
    /// while keeping optimistic input entirely local until they dispatch their
    /// own semantic operation.
    direct_crdt_writes: bool,
    /// Distinguishes an uninitialised binding (where a local initial value may
    /// remain visible) from a path removed by undo or a remote model change.
    path_was_present: bool,
    /// Optional local-only mirror for secrets and other values that must not
    /// enter an Automerge history. The owner controls its lifetime and can
    /// zero/clear it immediately after use.
    external_buffer: Option<Arc<Mutex<String>>>,
    on_change_path: String,
    last_mouse_pos: Point,
    char_x_positions: Vec<f64>,
    /// Authoritative cosmic-text geometry for a visible, unmasked single-line
    /// value. Painting, carets, selection, and hit-testing all consume it.
    single_layout: Option<RichTextLayout>,
    single_attributes: Option<SingleLineAttributes>,
    single_origin: Point,
    shaped_layout_signature: u64,
    /// Renderer epoch invalidates cached glyph geometry when canonical fonts change.
    shaped_layout_epoch: Option<Arc<()>>,
    /// Complete authored runs supplied by a shared rich-text widget. They
    /// participate in the same shaping as wrapping, carets and hit-testing.
    rich_spans: Option<Vec<RichSpan>>,
    /// In-progress IME composition. It is inserted only in the display
    /// layout and never exposed through value, clipboard, CRDT, or a11y.
    preedit: String,
    preedit_cursor: Option<(usize, usize)>,
    scroll_x: f64,
    /// A wrapped row inside a compound code fence shares its parent's spacing.
    compact_multiline: bool,
    last_edit_time: Option<Instant>,
    /// What the field's text is; its 1× size is the role's.
    text_role: TextRole,
    /// A document editor built on this field (a heading block, a code view)
    /// sets its own 1× base size instead of a role's.
    font_size_override: Option<f32>,
    /// Theme scale captured during `build_layout`; direct measurement falls
    /// back to the renderer when no themed layout pass has occurred.
    layout_text_scale: Option<f32>,
    /// The text scale used for the most recent paint. Code gutter geometry is
    /// derived from the same scale as its shaped line-number text so it never
    /// intrudes into the viewport at accessibility sizes.
    gutter_text_scale: f64,
    /// Renders without the rounded-rect chrome, for block-editor cells that
    /// should read as document copy, not a form field.
    plain: bool,
    /// Direct Automerge `Text` binding, wins over `on_change_path` — how
    /// `BlockEditor` binds text nested inside a `List`, which a flat string
    /// key can't address.
    bound_text: Option<ObjId>,
    /// Stamped on every inserted run so peers can highlight "what they wrote".
    local_author: Option<String>,
    /// Horizontal alignment of wrapped text within the input's width.
    align: TextAlign,
    /// `self.text` stays the real value; only the render substitutes bullets.
    secret: bool,
    disabled: bool,
    kind: TextInputKind,
    /// Prose fields check spelling by default. Purpose-specific wrappers such
    /// as search/code/number inputs opt out explicitly.
    /// `None` follows semantic intent; `Some` is the app author's explicit
    /// opt-in/out. The user's global off switch still wins.
    spellcheck: Option<bool>,
    /// Per-field override for automatic replacement. `None` follows Settings.
    autocorrect: Option<bool>,
    /// Untouched form fields do no dictionary work. Focus/edit/right-click
    /// activates checking for the rest of the widget's lifetime.
    spell_active: bool,
    /// Hunspell dictionary name / BCP-47-ish language tag.
    spell_language: String,
    /// Cached by (language, text), so drawing idle fields never starts work.
    spell_cache_key: Option<(String, String)>,
    spell_spans: Vec<(usize, usize)>,
    /// Per-user vocabulary layered over every installed language.
    spell_dictionary: Option<spellcheck::Dictionary>,
    ignored_spellings: HashSet<String>,
    /// Caches both preview hits and misses so idle redraws never re-query a
    /// dictionary for the same caret word.
    autocorrection_preview_key: Option<(String, String, usize, usize)>,
    autocorrection_preview: Option<AutocorrectionPreview>,
    last_autocorrection: Option<Autocorrection>,
    text_actions: Vec<RegisteredTextAction>,
    pending_text_action: Option<TextActionRequest>,
    /// Reuses [`ContextMenu`]; rendered via the overlay pass so it floats above the field.
    context_menu: ContextMenu,
    /// Per-row action parallel to `context_menu`'s items (separators → `None`).
    context_actions: Vec<Option<CtxAction>>,
    accessible_name: String,
    placeholder: String,
    /// Measurement switches with this so the caret/selection stay aligned with the draw.
    mono: bool,
    /// Wins over `mono`; measurement uses the same family as the draw.
    family: Option<FontChoice>,
    multiline: bool,
    /// Unmasked multiline fields use the shared cosmic-text paragraph,
    /// including attributed syntax-highlighted code.
    multiline_shaped: bool,
    /// Plain `Enter` submits instead of inserting a newline; multiline keeps
    /// `Shift+Enter` for a real newline (chat-composer pattern).
    enter_submits: bool,
    submit_requested: bool,
    /// The plaintext still never reaches the a11y tree or `text_review`.
    reveal: bool,
    /// Rebuilt by each `draw`; empty in single-line mode.
    lines: Vec<VisualLine>,
    /// Preserved across consecutive `Up`/`Down`; reset on any other move.
    goal_x: Option<f64>,
    scroll_y: f64,
    preserve_unfocused_scroll: bool,
    gutter: bool,
    /// Width measured from the same scaled line-number style used to paint.
    measured_gutter_width: f64,
    highlight_current_line: bool,
    /// `None` renders the whole body in one colour; `Some` colours per [`Span`] from the theme's syntax palette.
    highlighter: Option<Box<dyn Highlighter>>,
    /// `Some(0)` inserts a literal `\t`; `None` leaves Tab for focus traversal.
    tab_spaces: Option<usize>,
    /// Rebuilt only when the text changes (keyed by `hl_sig`).
    hl_spans: Vec<Vec<Span>>,
    hl_line_starts: Vec<usize>,
    hl_sig: u64,
    /// Only re-centres when the caret moves, so a wheel scroll away from it sticks.
    last_scroll_caret: Option<usize>,
    /// Last configured caret width, retained for the platform IME anchor whose
    /// trait hook does not receive a theme.
    last_caret_width: f64,
    interaction: crate::interaction::InteractionEnvironment,
}

/// One on-screen row: a whole short logical line, or one soft-wrapped
/// fragment. Rebuilt every `draw`; caret/selection/hit-test all read it so
/// they agree pixel-for-pixel with what was painted.
#[derive(Clone, Debug)]
struct VisualLine {
    start: usize,
    /// Exclusive, NOT counting a trailing hard `\n`.
    end: usize,
    /// `false` on a soft-wrapped row: a boundary byte there belongs to the
    /// next row's start, not this row's end.
    hard_break: bool,
    /// `len() == chars_in_row + 1`; the last entry is the row's total width.
    char_x: Vec<f64>,
    /// Local coordinates, before `scroll_y`.
    y: f64,
}

/// Soft-wrap state across one `build_lines` pass: the metrics every
/// paragraph wraps with, and the rows laid out so far.
struct LineWrap {
    size: f32,
    wrap_w: f64,
    /// Row pitch.
    lh: f64,
    /// Top of the next row.
    y: f64,
    lines: Vec<VisualLine>,
}

pub(crate) const AUTHOR_MARK: &str = "author";

/// Which logical line each multiline-body visual row belongs to, which rows
/// start one, and the logical line holding the caret.
struct LogicalRows {
    line_of_row: Vec<usize>,
    is_line_start: Vec<bool>,
    caret_line: usize,
}

/// One drawable multiline-body run: text, line-relative x offset, colour.
type Seg = (String, f64, vello::peniko::Color);

/// One visible body row's y offset and its drawable segments, as collected
/// by [`TextInput::collect_ml_body_rows`].
type RowDraws = Vec<(f64, Vec<Seg>)>;

/// One gutter line number's y offset, label and "is a wrapped continuation"
/// flag, as collected by [`TextInput::collect_ml_body_rows`].
type GutterNums = Vec<(f64, String, bool)>;

impl TextInput {
    fn effectively_disabled(&self) -> bool {
        self.disabled
            || self
                .action
                .as_ref()
                .is_some_and(|action| !action.is_available())
    }
    /// Empty `path` opts the widget out of presence rendering.
    pub fn new(path: Vec<PathSegment>) -> Self {
        let spell_language = spellcheck::default_language();
        spellcheck::warm_up_language(&spell_language);
        Self::with_language(path, spell_language)
    }

    fn with_language(path: Vec<PathSegment>, spell_language: String) -> Self {
        Self {
            id: String::new(),
            path,
            rect: Rect::ZERO,
            is_hovered: false,
            is_focused: false,
            caret_paint_suppressed: false,
            text: String::new(),
            char_filter: None,
            cursor_pos: 0,
            caret_affinity: TextAffinity::Before,
            selection_anchor: None,
            is_dragging: false,
            active_touch: None,
            drag_unit: DragUnit::Char,
            drag_anchor_span: (0, 0),
            drag_select: DragSelect::new(),
            on_change_path: String::new(),
            last_mouse_pos: Point::ZERO,
            char_x_positions: Vec::new(),
            single_layout: None,
            single_attributes: None,
            single_origin: Point::ZERO,
            shaped_layout_signature: u64::MAX,
            shaped_layout_epoch: None,
            rich_spans: None,
            preedit: String::new(),
            preedit_cursor: None,
            scroll_x: 0.0,
            compact_multiline: false,
            last_edit_time: None,
            dirty: false,
            on_change: None,
            action: None,
            last_notified_text: String::new(),
            local_state: None,
            model_layout_dirty: false,
            crdt_bound: false,
            direct_crdt_writes: true,
            path_was_present: false,
            external_buffer: None,
            text_role: TextRole::Body,
            font_size_override: None,
            layout_text_scale: None,
            gutter_text_scale: 1.0,
            plain: false,
            bound_text: None,
            local_author: None,
            align: TextAlign::Left,
            secret: false,
            disabled: false,
            kind: TextInputKind::Prose,
            spellcheck: None,
            autocorrect: None,
            spell_active: false,
            spell_language,
            spell_cache_key: None,
            spell_spans: Vec::new(),
            spell_dictionary: None,
            ignored_spellings: HashSet::new(),
            autocorrection_preview_key: None,
            autocorrection_preview: None,
            last_autocorrection: None,
            text_actions: Vec::new(),
            pending_text_action: None,
            context_menu: ContextMenu::new(),
            context_actions: Vec::new(),
            accessible_name: String::new(),
            placeholder: String::new(),
            mono: false,
            family: None,
            multiline: false,
            multiline_shaped: false,
            enter_submits: false,
            submit_requested: false,
            reveal: false,
            lines: Vec::new(),
            goal_x: None,
            scroll_y: 0.0,
            preserve_unfocused_scroll: false,
            gutter: false,
            measured_gutter_width: 0.0,
            highlight_current_line: false,
            highlighter: None,
            tab_spaces: None,
            hl_spans: Vec::new(),
            hl_line_starts: Vec::new(),
            hl_sig: u64::MAX,
            last_scroll_caret: None,
            last_caret_width: 2.0,
            interaction: crate::interaction::InteractionEnvironment::default(),
        }
    }

    /// `None` disables authorship stamping.
    /// Align the text within the input's width (a centred title).
    pub fn set_align(&mut self, align: TextAlign) {
        if self.align != align {
            self.align = align;
            self.single_layout = None;
            self.dirty = true;
        }
    }

    /// How the text is aligned within the input's width.
    pub fn align(&self) -> TextAlign {
        self.align
    }

    pub fn set_local_author(&mut self, author: Option<String>) {
        self.local_author = author;
    }

    /// After binding, `sync` reads from `obj` regardless of the string-path mode.
    pub fn bind_text(&mut self, obj: ObjId) {
        self.local_state = None;
        self.bound_text = Some(obj);
        self.crdt_bound = true;
    }

    /// Detach an explicit nested-Text binding without changing live input state.
    ///
    /// A direct binding replaces any earlier local or flat-path mode. Detaching
    /// therefore leaves the input intentionally unbound until an owner calls
    /// [`Self::bind_text`] again; it does not revive a discarded fallback.
    pub fn clear_text_binding(&mut self) {
        if self.bound_text.take().is_none() {
            return;
        }
        self.crdt_bound = false;
        self.path_was_present = false;
    }

    /// For `BlockEditor` to match local blocks to doc list elements by object identity.
    pub fn bound_obj(&self) -> Option<&ObjId> {
        self.bound_text.as_ref()
    }

    /// Adopt only the bound Text and align presentation/edit-notification baselines.
    pub(super) fn adopt_bound_text(
        &mut self,
        state: &AutoCommit,
    ) -> Result<(), crate::view::ThawError> {
        let obj = self
            .bound_text
            .as_ref()
            .ok_or_else(|| crate::view::ThawError::State("text is not bound".into()))?;
        let text = state
            .text(obj)
            .map_err(|e| crate::view::ThawError::State(e.to_string()))?;
        if self.adopt_model_text(text) {
            self.dirty = true;
        }
        self.clamp_to_text();
        self.last_notified_text.clone_from(&self.text);
        Ok(())
    }

    /// Replace text read from model state without reporting a user edit.
    /// Returns whether the value changed so direct bindings can preserve
    /// their existing reconciliation signal.
    fn adopt_model_text(&mut self, text: String) -> bool {
        if self.text == text {
            return false;
        }
        self.text = text;
        self.model_layout_dirty |= self.multiline || self.single_attributes.is_some();
        true
    }

    /// Capture the resolved theme scale before a renderer-backed measurement.
    pub(super) fn capture_layout_text_scale(&mut self, text_scale: f32) {
        if self.layout_text_scale != Some(text_scale) {
            self.layout_text_scale = Some(text_scale);
            self.model_layout_dirty |= self.multiline || self.single_attributes.is_some();
        }
    }

    /// Keep the binding readable while an owning action bridge handles user writes.
    pub fn set_direct_crdt_writes(&mut self, enabled: bool) {
        self.direct_crdt_writes = enabled;
    }

    fn resolve_text_obj(&self, state: &AutoCommit, root: &ObjId) -> Option<ObjId> {
        if !self.direct_crdt_writes {
            return None;
        }
        if self.action.is_some() {
            return None;
        }
        if let Some(obj) = &self.bound_text {
            return Some(obj.clone());
        }
        if self.crdt_bound
            && let Ok(Some((Value::Object(ObjType::Text), id))) =
                state.get(root, self.on_change_path.as_str())
        {
            return Some(id);
        }
        None
    }

    /// Materialise a path-bound text object after the first effective edit.
    ///
    /// Path bindings deliberately stay absent while the user only focuses or
    /// navigates the field. When an edit was applied locally without an
    /// existing Automerge text object, publish the complete resulting value as
    /// one text object so sibling document readers observe that first edit.
    fn persist_first_path_edit(&self, previous_text: &str, state: &mut AutoCommit, root: &ObjId) {
        if self.action.is_some()
            || !self.crdt_bound
            || self.bound_text.is_some()
            || self.on_change_path.is_empty()
            || self.text == previous_text
            || self.resolve_text_obj(state, root).is_some()
        {
            return;
        }
        let Ok(obj) = state.put_object(root, self.on_change_path.as_str(), ObjType::Text) else {
            return;
        };
        if !self.text.is_empty() {
            let _ = state.splice_text(&obj, 0, 0, self.text.as_str());
        }
    }

    /// Set the field's text in `role` (default [`TextRole::Body`]).
    #[must_use]
    pub fn with_text_role(mut self, role: TextRole) -> Self {
        self.set_text_role(role);
        self
    }

    #[must_use]
    pub fn with_plain_chrome(mut self) -> Self {
        self.plain = true;
        self
    }

    /// Change the field's text role; see [`Self::with_text_role`].
    pub fn set_text_role(&mut self, role: TextRole) {
        if self.text_role != role || self.font_size_override.is_some() {
            self.model_layout_dirty |= self.multiline || self.single_attributes.is_some();
            self.text_role = role;
            self.font_size_override = None;
        }
    }

    /// The field's text role.
    #[must_use]
    pub fn text_role(&self) -> TextRole {
        self.text_role
    }

    /// A document editor built on this field (a heading block, a code view)
    /// sets its own 1× base size; chrome fields take a role.
    pub(crate) fn set_base_size(&mut self, size: f32) {
        if self.font_size_override != Some(size) {
            self.model_layout_dirty |= self.multiline || self.single_attributes.is_some();
            self.font_size_override = Some(size);
        }
    }

    pub fn set_plain_chrome(&mut self, plain: bool) {
        self.plain = plain;
    }

    #[must_use]
    pub fn with_mono(mut self) -> Self {
        self.mono = true;
        self.kind = TextInputKind::Code;
        self
    }

    pub fn set_mono(&mut self, mono: bool) {
        if self.mono != mono {
            self.model_layout_dirty |= self.multiline || self.single_attributes.is_some();
        }
        self.mono = mono;
        if mono {
            self.set_kind(TextInputKind::Code);
        }
    }

    pub fn is_mono(&self) -> bool {
        self.mono
    }

    /// Declare the exceptional structured intent of a field. Natural-language
    /// prose is already the default and never needs an annotation.
    #[must_use]
    pub fn with_kind(mut self, kind: TextInputKind) -> Self {
        self.set_kind(kind);
        self
    }

    pub fn set_kind(&mut self, kind: TextInputKind) {
        self.kind = kind;
        self.spell_cache_key = None;
        self.spell_spans.clear();
    }

    pub fn kind(&self) -> TextInputKind {
        self.kind
    }

    /// Enable or disable spelling assistance for this field. It defaults on
    /// for ordinary text and is automatically suppressed for secret and
    /// code-like fields.
    #[must_use]
    pub fn with_spellcheck(mut self, on: bool) -> Self {
        self.spellcheck = Some(on);
        self.spell_cache_key = None;
        self
    }

    pub fn set_spellcheck(&mut self, on: bool) {
        self.spellcheck = Some(on);
        self.spell_cache_key = None;
    }

    pub fn spellcheck_enabled(&self) -> bool {
        self.checks_spelling()
    }

    /// Override automatic correction for this field. Without an override the
    /// user-wide Spell checker setting applies.
    #[must_use]
    pub fn with_autocorrect(mut self, on: bool) -> Self {
        self.autocorrect = Some(on);
        self
    }

    pub fn set_autocorrect(&mut self, on: bool) {
        self.autocorrect = Some(on);
    }

    pub fn autocorrect_enabled(&self) -> bool {
        self.field_allows_spelling()
            && self
                .autocorrect
                .unwrap_or_else(|| crate::text_assistance::preferences().auto_correct)
    }

    /// Select a language dictionary, accepting both en-US and `en_US` style
    /// tags. If that Hunspell dictionary is unavailable, non-English checking
    /// quietly disables itself; English retains the embedded fallback.
    #[must_use]
    pub fn with_spell_language(mut self, language: &str) -> Self {
        self.set_spell_language(language);
        self
    }

    pub fn set_spell_language(&mut self, language: &str) {
        self.spell_language = language.to_string();
        self.spell_cache_key = None;
        self.autocorrection_preview_key = None;
        self.autocorrection_preview = None;
    }

    pub fn spell_language(&self) -> &str {
        &self.spell_language
    }

    /// Add a host-owned action to the selection context menu. Activating it
    /// produces a request drained by `take_text_action_request`; the widget
    /// never blocks on the action itself.
    #[must_use]
    pub fn with_text_action(mut self, id: &str, label: &str) -> Self {
        self.add_text_action(id, label);
        self
    }

    pub fn add_text_action(&mut self, id: &str, label: &str) {
        if id.is_empty() || label.is_empty() {
            return;
        }
        self.text_actions.push(RegisteredTextAction {
            id: id.to_string(),
            label: label.to_string(),
        });
    }

    pub fn clear_text_actions(&mut self) {
        self.text_actions.clear();
    }

    pub fn take_text_action_request(&mut self) -> Option<TextActionRequest> {
        self.pending_text_action.take()
    }

    /// Wins over the mono flag.
    #[must_use]
    pub fn with_font_family(mut self, family: FontChoice) -> Self {
        self.family = Some(family);
        self
    }

    pub fn set_font_family(&mut self, family: Option<FontChoice>) {
        if self.family != family {
            self.model_layout_dirty |= self.multiline || self.single_attributes.is_some();
            self.family = family;
        }
    }

    pub fn font_family(&self) -> Option<&FontChoice> {
        self.family.as_ref()
    }

    /// Picks the same family the draw path will use, so `char_x_positions`
    /// and the drawn glyphs agree.
    fn measure_str(&self, tr: &mut TextRenderer, s: &str, size: f32) -> (f32, f32) {
        if let Some(fam) = &self.family {
            tr.measure_font(s, size, None, TextWeight::Regular, false, fam)
        } else if self.mono {
            tr.measure_styled(s, size, None, TextWeight::Regular, false, true)
        } else {
            tr.measure(s, size, None)
        }
    }

    fn shaped_signature(
        &self,
        display: &str,
        family: &FontChoice,
        size: f32,
        width: Option<f32>,
    ) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hash = DefaultHasher::new();
        display.hash(&mut hash);
        family.hash(&mut hash);
        size.to_bits().hash(&mut hash);
        self.kind.text_kind().hash(&mut hash);
        (self.align as u8).hash(&mut hash);
        width.map(f32::to_bits).hash(&mut hash);
        if !self.preedit.is_empty() {
            self.cursor_pos.hash(&mut hash);
            self.preedit_cursor.hash(&mut hash);
        }
        hash.finish()
    }

    fn attributed_signature(base: u64, spans: &[RichSpan]) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hash = DefaultHasher::new();
        base.hash(&mut hash);
        for span in spans {
            span.text.hash(&mut hash);
            span.color.hash(&mut hash);
            span.bold.hash(&mut hash);
            span.italic.hash(&mut hash);
            span.size.map(f32::to_bits).hash(&mut hash);
            span.family.hash(&mut hash);
            span.background.hash(&mut hash);
            span.baseline.hash(&mut hash);
        }
        hash.finish()
    }

    pub(super) fn set_rich_spans(&mut self, spans: Option<Vec<RichSpan>>) {
        if self.rich_spans != spans {
            self.rich_spans = spans;
            self.model_layout_dirty = true;
        }
    }

    /// Convert the highlighter's logical-line ranges into a complete set of
    /// attributed document spans, then insert IME preedit as a display-only
    /// default-colour run. Concatenating the result always reproduces
    /// `layout_display_text()` byte-for-byte.
    fn highlighted_display_spans(&self, theme: &Theme) -> Vec<RichSpan> {
        let mut colored = Vec::<(usize, usize, [u8; 3])>::new();
        if self.highlighter.is_some() {
            for (line, spans) in self.hl_spans.iter().enumerate() {
                let Some(&line_start) = self.hl_line_starts.get(line) else {
                    continue;
                };
                let Some(line_tail) = self.text.get(line_start..) else {
                    continue;
                };
                let line_end = line_tail
                    .find('\n')
                    .map_or(self.text.len(), |at| line_start.saturating_add(at));
                for span in spans {
                    let start = line_start.saturating_add(span.range.start).min(line_end);
                    let end = line_start.saturating_add(span.range.end).min(line_end);
                    if start >= end
                        || !self.text.is_char_boundary(start)
                        || !self.text.is_char_boundary(end)
                    {
                        continue;
                    }
                    let color = theme.tokens.syntax.color(span.kind);
                    colored.push((start, end, [color.r(), color.g(), color.b()]));
                }
            }
        }
        colored.sort_by_key(|&(start, end, _)| (start, end));

        let mut spans = Vec::new();
        let mut cursor = 0usize;
        for (start, end, color) in colored {
            let start = start.max(cursor);
            if start >= end {
                continue;
            }
            if cursor < start
                && let Some(before) = self.text.get(cursor..start)
            {
                spans.push(RichSpan::new(before));
            }
            if let Some(mid) = self.text.get(start..end) {
                spans.push(RichSpan::new(mid).color(Some(color)));
            }
            cursor = end;
        }
        if (cursor < self.text.len() || spans.is_empty())
            && let Some(tail) = self.text.get(cursor..)
        {
            spans.push(RichSpan::new(tail));
        }

        // An optimistic edit can temporarily precede its parent's mark update.
        // Never shape stale text in that interval.
        if let Some(authored) = &self.rich_spans
            && authored
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
                == self.text
        {
            spans.clone_from(authored);
        }

        self.insert_preedit_span(spans)
    }

    fn insert_preedit_span(&self, spans: Vec<RichSpan>) -> Vec<RichSpan> {
        if self.preedit.is_empty() {
            return spans;
        }
        let at = self.cursor_pos.min(self.text.len());
        let mut display = Vec::with_capacity(spans.len().saturating_add(2));
        let mut consumed = 0usize;
        let mut inserted = false;
        for span in spans {
            let end = consumed.saturating_add(span.text.len());
            if !inserted && at <= end {
                let local = at.saturating_sub(consumed);
                if let (Some(before_str), Some(after_str)) =
                    (span.text.get(..local), span.text.get(local..))
                {
                    if !before_str.is_empty() {
                        let mut before = span.clone();
                        before.text = before_str.to_string();
                        display.push(before);
                    }
                    display.push(RichSpan::new(&self.preedit));
                    if !after_str.is_empty() {
                        let mut after = span.clone();
                        after.text = after_str.to_string();
                        display.push(after);
                    }
                } else {
                    // `local` didn't land on a char boundary — shouldn't
                    // happen since `cursor_pos` is always kept on one, but
                    // fall back to keeping the span intact rather than
                    // panicking on a slice.
                    display.push(span);
                    display.push(RichSpan::new(&self.preedit));
                }
                inserted = true;
            } else {
                display.push(span);
            }
            consumed = end;
        }
        if !inserted {
            display.push(RichSpan::new(&self.preedit));
        }
        display
    }

    pub(super) fn set_single_line_attributes(&mut self, attributes: SingleLineAttributes) {
        let signature = Self::single_attributes_signature(&attributes);
        if self
            .single_attributes
            .as_ref()
            .is_none_or(|old| Self::single_attributes_signature(old) != signature)
        {
            self.single_attributes = Some(attributes);
            self.single_layout = None;
            self.model_layout_dirty = true;
        }
    }

    fn single_attributes_signature(attributes: &SingleLineAttributes) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        Self::attributed_signature(0, &attributes.spans).hash(&mut hash);
        attributes.underlines.hash(&mut hash);
        attributes.strikes.hash(&mut hash);
        attributes.chips.hash(&mut hash);
        attributes.links.hash(&mut hash);
        hash.finish()
    }

    fn valid_single_attributes(&self) -> Option<&SingleLineAttributes> {
        self.single_attributes.as_ref().filter(|attributes| {
            attributes
                .spans
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>()
                == self.text
        })
    }

    /// Preedit is its own run: adjacent committed decorations do not expand
    /// over it. An insertion inside a marked run splits its decoration.
    fn display_ranges(&self, ranges: &[(usize, usize)]) -> Vec<(usize, usize)> {
        let at = self.cursor_pos.min(self.text.len());
        let extra = self.preedit.len();
        let mut display = Vec::new();
        for &(start, end) in ranges {
            if start >= end
                || end > self.text.len()
                || !self.text.is_char_boundary(start)
                || !self.text.is_char_boundary(end)
            {
                continue;
            }
            if extra == 0 || end <= at {
                display.push((start, end));
            } else if start >= at {
                display.push((start.saturating_add(extra), end.saturating_add(extra)));
            } else {
                display.push((start, at));
                display.push((at.saturating_add(extra), end.saturating_add(extra)));
            }
        }
        display
    }

    /// No CRDT integration — for a field with no CRDT path to spend.
    pub fn local() -> Self {
        let mut s = Self::new(Vec::new());
        s.crdt_bound = false;
        s
    }

    /// Explicit host language without discovery or background dictionary warmup.
    #[doc(hidden)]
    pub fn local_with_language(language: &str) -> Self {
        Self::with_language(Vec::new(), language.to_owned())
    }

    /// Create a true two-way binding to typed process-local text state.
    ///
    /// User edits update `state`; mutations through any clone of `state`
    /// update this input on the next synchronization pass. Several inputs and
    /// read-only [`Text`](super::Text) views can therefore share one value
    /// without initial-value snapshots, callbacks, or app-owned fan-out.
    /// Selecting a document or external-buffer binding later replaces this
    /// process-local binding explicitly.
    pub fn from_state(state: &crate::LocalState<String>) -> Self {
        let (value, binding) = crate::local_state::LocalBinding::new(state);
        let mut input = Self::local().with_initial_value(&value);
        input.local_state = Some(binding);
        input
    }

    /// Create an input bound to one root-level document string.
    ///
    /// The path may contain either an Automerge string scalar or `Text`
    /// object. The first effective edit upgrades a missing path (or scalar)
    /// to collaborative `Text`; focus and selection alone do not materialise
    /// model state. Pair with
    /// [`Text::from_document_string`](super::Text::from_document_string) for a
    /// synchronized read-only view without app-owned callbacks.
    pub fn from_document_string(path: impl Into<String>) -> Self {
        let path = path.into();
        let mut input = Self::local();
        input.crdt_bound = !path.is_empty();
        input.on_change_path = path;
        input
    }

    /// Admit only characters accepted by `filter` on every insertion path.
    ///
    /// This is deliberately a character predicate, not a validator for the
    /// whole value: callers still own domain parsing and error reporting when
    /// the field is submitted.
    #[must_use]
    pub fn with_char_filter(mut self, filter: fn(char) -> bool) -> Self {
        self.char_filter = Some(filter);
        self.text.retain(filter);
        self.clamp_to_text();
        self.last_notified_text.clone_from(&self.text);
        if self.action.is_none() {
            self.publish_local_state();
        }
        self
    }

    /// Cursor parks at the end; dirty stays false (programmatic seed).
    #[must_use]
    pub fn with_initial_value(mut self, s: &str) -> Self {
        self.text = s.to_string();
        self.cursor_pos = self.text.len();
        self.caret_affinity = TextAffinity::Before;
        self.dirty = false;
        self.last_notified_text.clone_from(&self.text);
        self.publish_local_state();
        self
    }

    pub fn set_text(&mut self, s: &str) {
        if self.text == s {
            return;
        }
        if self.secret {
            self.text.zeroize();
        }
        self.text = s.to_string();
        self.selection_anchor = None;
        self.clamp_to_text();
        self.dirty = false;
        self.model_layout_dirty = self.multiline || self.single_attributes.is_some();
        self.last_edit_time = None;
        self.last_notified_text.clone_from(&self.text);
        if self.action.is_none() {
            self.publish_local_state();
        }
    }

    pub fn value(&self) -> &str {
        &self.text
    }

    /// Register an app-local callback for committed user edits.
    ///
    /// Keyboard input, paste, accessibility edits, and committed IME text
    /// notify once with the complete new value. IME preedit, cursor movement,
    /// selection changes, [`Self::set_text`], and CRDT synchronization do not.
    #[must_use]
    pub fn on_change(mut self, callback: impl FnMut(&str) + 'static) -> Self {
        if self.action.is_none() {
            self.on_change = Some(Box::new(callback));
        }
        self.last_notified_text.clone_from(&self.text);
        self
    }

    /// Route committed keyboard, paste, IME, and accessibility edits through
    /// an exported action with the complete current string as input.
    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<String>) -> Self {
        self.action = Some(ActionEmitter::new(binding));
        self.on_change = None;
        self
    }

    fn notify_user_change(&mut self) {
        if !self.dirty || self.last_notified_text == self.text {
            return;
        }
        self.last_notified_text.clone_from(&self.text);
        if let Some(action) = &mut self.action {
            let _ = action.emit(self.text.clone());
        } else {
            self.publish_local_state();
        }
        if self.action.is_none()
            && let Some(callback) = self.on_change.as_mut()
        {
            callback(&self.text);
        }
    }

    fn publish_local_state(&mut self) {
        if let Some(binding) = &mut self.local_state {
            binding.publish(&self.text);
        }
    }

    /// Local-mode only. In CRDT-bound mode prefer a real key event instead,
    /// so the splice carries authorship marks.
    pub fn insert_at_cursor(&mut self, s: &str) {
        // `insert_str` only touches the CRDT when given an object id.
        let mut sink = AutoCommit::new();
        self.insert_str(s, &mut sink, None);
        self.last_notified_text.clone_from(&self.text);
        self.publish_local_state();
    }

    /// Delete the selected text in a host-owned local projection.
    pub(super) fn delete_local_selection(&mut self) {
        let mut sink = AutoCommit::new();
        self.delete_selection(&mut sink, None);
        self.last_notified_text.clone_from(&self.text);
        self.publish_local_state();
    }

    pub fn cursor_pos(&self) -> usize {
        self.cursor_pos
    }

    /// The caret's document offset and visual side at an ambiguous boundary.
    pub fn caret_position(&self) -> TextPosition {
        TextPosition::with_affinity(self.cursor_pos, self.caret_affinity)
    }

    /// Does not count as a user edit (dirty stays false).
    pub fn set_cursor_pos(&mut self, pos: usize) {
        self.cursor_pos = grapheme_boundary_at_or_before(&self.text, pos);
        self.caret_affinity = TextAffinity::Before;
        self.selection_anchor = None;
        self.goal_x = None;
    }

    /// Seat the caret at an exact shaped position without counting as an edit.
    pub fn set_caret_position(&mut self, position: TextPosition) {
        self.cursor_pos = grapheme_boundary_at_or_before(&self.text, position.byte);
        self.caret_affinity = position.affinity;
        self.selection_anchor = None;
        self.goal_x = None;
    }

    pub fn selection_range_public(&self) -> Option<(usize, usize)> {
        self.selection_range()
    }

    /// The live selection endpoints in anchor/focus order for accessibility
    /// bridges. Unlike [`Self::selection_range_public`], this preserves the
    /// direction in which the user extended the selection.
    pub fn selection_anchor_focus_public(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        (anchor != self.cursor_pos).then_some((anchor, self.cursor_pos))
    }

    /// `len()` is `text.len() + 1`; empty until the first draw lands.
    pub fn char_x_positions(&self) -> &[f64] {
        &self.char_x_positions
    }

    pub fn scroll_x(&self) -> f64 {
        self.scroll_x
    }

    /// Retain bounded multiline scroll when used as an inert, unfocused source view.
    /// Does not enable focus, caret painting, input handling or document binding.
    pub fn set_preserve_unfocused_scroll(&mut self, preserve: bool) {
        self.preserve_unfocused_scroll = preserve;
    }

    pub fn scroll_y(&self) -> f64 {
        self.scroll_y
    }

    pub fn body_line_height(&self) -> f64 {
        self.line_height()
    }

    pub fn gutter_width(&self) -> f64 {
        self.code_gutter_w()
    }

    #[cfg(test)]
    pub(crate) fn shaped_font_size_for_test(&self) -> Option<f32> {
        self.single_layout
            .as_ref()
            .map(|layout| layout.buffer.metrics().font_size)
    }

    /// Requires a prior `draw` to have built the metrics.
    pub fn byte_at_point(&self, point: Point) -> usize {
        self.position_at_point(point).byte
    }

    /// Exact caret position under a point, including mixed-BiDi affinity.
    pub fn position_at_point(&self, point: Point) -> TextPosition {
        self.resolve_position(point)
    }

    /// No-op for a single-line field.
    pub fn scroll_by(&mut self, dy: f64) {
        if !self.multiline {
            return;
        }
        let view_h = self.rect.height() - self.multiline_padding_y() * 2.0;
        let max = (self.content_height() - self.multiline_padding_y() * 2.0 - view_h).max(0.0);
        self.scroll_y = (self.scroll_y + dy).clamp(0.0, max);
    }

    pub fn is_plain(&self) -> bool {
        self.plain
    }

    pub fn text_padding_x(&self, theme: &Theme) -> f64 {
        if self.plain {
            0.0
        } else {
            f64::from(theme.padding_standard)
        }
    }

    /// Single source of truth for both the local and any peer caret, so they line up.
    pub fn text_band(&self) -> (f64, f64) {
        if !self.multiline
            && !self.masked()
            && let Some(layout) = &self.single_layout
        {
            return (
                self.single_origin.y,
                f64::from(layout.buffer.metrics().line_height),
            );
        }
        let text_size = self.effective_font_size();
        let line_height = f64::from(text_size * LINE_HEIGHT_RATIO);
        let text_y = self.rect.y0 + (self.rect.height() - line_height) / 2.0;
        (text_y, line_height)
    }

    /// Requires a prior `draw` to have populated `char_x_positions`.
    fn caret_x_at(&self, theme: &Theme, position: TextPosition) -> f64 {
        if (!self.multiline || self.multiline_shaped)
            && !self.masked()
            && let Some(layout) = &self.single_layout
        {
            return self.single_origin.x
                + f64::from(
                    layout
                        .caret_geometry_at(self.text_to_layout_position(position))
                        .0,
                )
                - self.scroll_x;
        }
        let off = self
            .char_x_positions
            .get(position.byte)
            .copied()
            .unwrap_or_else(|| self.char_x_positions.last().copied().unwrap_or(0.0));
        self.rect.x0 + self.text_padding_x(theme) + off - self.scroll_x
    }

    fn caret_x(&self, theme: &Theme, byte: usize) -> f64 {
        self.caret_x_at(theme, TextPosition::new(byte))
    }

    /// Peer carets render through this too, so a remote caret is
    /// pixel-identical, only recoloured.
    pub fn caret_rect(&self, theme: &Theme, byte: usize) -> Rect {
        self.caret_rect_at(theme, TextPosition::new(byte))
    }

    /// Caret geometry for an exact byte-and-affinity position.
    pub fn caret_rect_at(&self, theme: &Theme, position: TextPosition) -> Rect {
        if self.multiline {
            if self.multiline_shaped
                && !self.masked()
                && let Some(layout) = &self.single_layout
            {
                let (x, top, height) =
                    layout.caret_geometry_at(self.text_to_layout_position(position));
                let x = self.single_origin.x + f64::from(x);
                let y = self.single_origin.y + f64::from(top);
                return Rect::new(x, y, x + theme.caret_width, y + f64::from(height));
            }
            return self.caret_rect_ml(position.byte, theme.caret_width);
        }
        let x = self.caret_x_at(theme, position);
        let (text_y, line_height) = self.text_band();
        Rect::new(x, text_y, x + theme.caret_width, text_y + line_height)
    }

    /// Single-line only; multiline callers use `selection_rects`.
    pub fn selection_rect(&self, theme: &Theme, start: usize, end: usize) -> Rect {
        if (!self.multiline || self.multiline_shaped)
            && !self.masked()
            && let Some(layout) = &self.single_layout
        {
            let mut bands = layout
                .selection_rects(
                    self.text_to_layout_byte(start),
                    self.text_to_layout_byte(end),
                )
                .into_iter();
            if let Some((x0, y0, x1, y1)) = bands.next() {
                let mut rect = Rect::new(
                    self.single_origin.x + f64::from(x0) - self.scroll_x,
                    self.single_origin.y + f64::from(y0),
                    self.single_origin.x + f64::from(x1) - self.scroll_x,
                    self.single_origin.y + f64::from(y1),
                );
                for (x0, y0, x1, y1) in bands {
                    rect = rect.union(Rect::new(
                        self.single_origin.x + f64::from(x0) - self.scroll_x,
                        self.single_origin.y + f64::from(y0),
                        self.single_origin.x + f64::from(x1) - self.scroll_x,
                        self.single_origin.y + f64::from(y1),
                    ));
                }
                return rect;
            }
        }
        let x0 = self.caret_x(theme, start);
        let x1 = self.caret_x(theme, end);
        let (text_y, line_height) = self.text_band();
        Rect::new(x0.min(x1), text_y, x0.max(x1), text_y + line_height)
    }

    /// One rect per visual row the range spans.
    pub fn selection_rects(&self, theme: &Theme, start: usize, end: usize) -> Vec<Rect> {
        if (!self.multiline || self.multiline_shaped)
            && !self.masked()
            && let Some(layout) = &self.single_layout
        {
            return layout
                .selection_rects(
                    self.text_to_layout_byte(start),
                    self.text_to_layout_byte(end),
                )
                .into_iter()
                .map(|(x0, y0, x1, y1)| {
                    Rect::new(
                        self.single_origin.x + f64::from(x0) - self.scroll_x,
                        self.single_origin.y + f64::from(y0),
                        self.single_origin.x + f64::from(x1) - self.scroll_x,
                        self.single_origin.y + f64::from(y1),
                    )
                })
                .collect();
        }
        if !self.multiline {
            return vec![self.selection_rect(theme, start, end)];
        }
        let lh = self.line_height();
        let px = self.ml_left();
        let py = self.multiline_padding_y();
        let mut out = Vec::new();
        for r in &self.lines {
            let lo = start.max(r.start);
            let hi = end.min(r.end);
            if lo > hi {
                continue;
            }
            // Selection continuing past this row's break: extend the
            // highlight past the last glyph to mark the newline.
            let spans_break = end > r.end;
            let x0 = self.x_in_row(r, lo);
            let mut x1 = self.x_in_row(r, hi);
            if spans_break && hi == r.end {
                x1 += f64::from(self.effective_font_size()) * 0.4;
            }
            if x1 - x0 < 0.5 {
                continue;
            }
            let y0 = self.rect.y0 + py + r.y - self.scroll_y;
            out.push(Rect::new(
                self.rect.x0 + px + x0,
                y0,
                self.rect.x0 + px + x1,
                y0 + lh,
            ));
        }
        out
    }

    fn effective_font_size(&self) -> f32 {
        self.font_size_override
            .unwrap_or_else(|| crate::tokens::typography::stock().size(self.text_role))
    }

    fn line_height(&self) -> f64 {
        if self.multiline_shaped
            && let Some(layout) = &self.single_layout
        {
            return f64::from(layout.buffer.metrics().line_height);
        }
        f64::from(self.effective_font_size() * LINE_HEIGHT_RATIO)
    }

    /// Falls back to one row when nothing has been laid out yet.
    pub fn content_height(&self) -> f64 {
        if self.multiline_shaped
            && let Some(layout) = &self.single_layout
        {
            return f64::from(layout.height()) + self.multiline_padding_y() * 2.0;
        }
        let lh = self.line_height();
        let rows = u32::try_from(self.lines.len().max(1)).unwrap_or(u32::MAX);
        f64::from(rows) * lh + self.multiline_padding_y() * 2.0
    }

    fn x_in_row(&self, r: &VisualLine, byte: usize) -> f64 {
        let b = grapheme_boundary_at_or_before(&self.text, byte.clamp(r.start, r.end));
        let ci = self
            .text
            .get(r.start..b)
            .map_or(0, |s| graphemes(s).count());
        r.char_x
            .get(ci)
            .copied()
            .unwrap_or_else(|| r.char_x.last().copied().unwrap_or(0.0))
    }

    fn byte_of_row_char(&self, r: &VisualLine, ci: usize) -> usize {
        let Some(row_text) = self.text.get(r.start..r.end) else {
            return r.end;
        };
        match grapheme_indices(row_text).nth(ci) {
            Some((b, _)) => r.start.saturating_add(b),
            None => r.end,
        }
    }

    /// A byte at both the end of a soft-wrapped row and the start of the next
    /// lands on the next row's left edge, not a phantom position past the right edge.
    fn locate_ml(&self, byte: usize) -> (usize, f64) {
        if self.lines.is_empty() {
            return (0, 0.0);
        }
        for (i, r) in self.lines.iter().enumerate() {
            if byte >= r.start && byte < r.end {
                return (i, self.x_in_row(r, byte));
            }
        }
        for (i, r) in self.lines.iter().enumerate().rev() {
            if byte >= r.start && byte <= r.end {
                if byte == r.end
                    && !r.hard_break
                    && let Some(next) = self.lines.get(i.saturating_add(1))
                    && next.start == byte
                {
                    return (i.saturating_add(1), 0.0);
                }
                return (i, self.x_in_row(r, byte));
            }
        }
        let last = self.lines.len().saturating_sub(1);
        let Some(r) = self.lines.get(last) else {
            return (0, 0.0);
        };
        (last, self.x_in_row(r, r.end))
    }

    fn caret_rect_ml(&self, byte: usize, width: f64) -> Rect {
        let (i, x) = self.locate_ml(byte);
        let y = self.lines.get(i).map_or(0.0, |r| r.y);
        let cx = self.rect.x0 + self.ml_left() + x;
        let cy = self.rect.y0 + self.multiline_padding_y() + y - self.scroll_y;
        Rect::new(cx, cy, cx + width, cy + self.line_height())
    }

    /// Requires a prior `draw` to have built the layout.
    fn cursor_from_point(&self, pt: Point) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        let lh = self.line_height();
        let local_y = pt.y - (self.rect.y0 + self.multiline_padding_y()) + self.scroll_y;
        let max_row = self.lines.len().saturating_sub(1);
        let row = {
            let row_f = (local_y / lh).floor().max(0.0);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "row_f is floored and clamped to >= 0.0 just above; Rust's `as` float-to-int cast saturates rather than UB for any out-of-range value, and the result is immediately bounded to max_row below"
            )]
            let candidate = row_f as usize;
            candidate.min(max_row)
        };
        let Some(r) = self.lines.get(row) else {
            return 0;
        };
        let rel_x = pt.x - (self.rect.x0 + self.ml_left());
        let mut best = 0usize;
        let mut best_d = f64::MAX;
        for (ci, &cx) in r.char_x.iter().enumerate() {
            let d = (rel_x - cx).abs();
            if d < best_d {
                best_d = d;
                best = ci;
            }
        }
        // Snapping to the very last boundary of a soft-wrapped row would jump
        // the caret to the next row's start; keep it on this row.
        let is_last = row == max_row;
        if best.saturating_add(1) == r.char_x.len() && !r.hard_break && !is_last && best > 0 {
            best = best.saturating_sub(1);
        }
        self.byte_of_row_char(r, best)
    }

    fn move_vertical(&mut self, down: bool, extend: bool) {
        if self.multiline_shaped
            && let Some(layout) = &self.single_layout
        {
            let position = self.text_to_layout_position(self.caret_position());
            let (x, top, height) = layout.caret_geometry_at(position);
            let goal = self.goal_x.unwrap_or_else(|| f64::from(x));
            let y = if down {
                top + height * 1.5
            } else {
                (top - height * 0.5).max(0.0)
            };
            #[allow(
                clippy::cast_possible_truncation,
                reason = "goal is a bounded on-screen x coordinate for vello/layout hit-testing, never near f32's range"
            )]
            let goal_x32 = goal as f32;
            let target = self.layout_to_text_position(layout.hit_position(goal_x32, y));
            self.set_cursor_position(target, extend);
            self.goal_x = Some(goal);
            return;
        }
        if self.lines.is_empty() {
            return;
        }
        let (i, cur_x) = self.locate_ml(self.cursor_pos);
        let goal = self.goal_x.unwrap_or(cur_x);
        let target = if down {
            if let Some(next) = i.checked_add(1).filter(|&n| n < self.lines.len()) {
                next
            } else {
                self.set_cursor(self.text.len(), extend);
                self.goal_x = Some(goal);
                return;
            }
        } else if i > 0 {
            i.saturating_sub(1)
        } else {
            self.set_cursor(0, extend);
            self.goal_x = Some(goal);
            return;
        };
        let Some(r) = self.lines.get(target) else {
            return;
        };
        let mut best = 0usize;
        let mut best_d = f64::MAX;
        for (ci, &cx) in r.char_x.iter().enumerate() {
            let d = (goal - cx).abs();
            if d < best_d {
                best_d = d;
                best = ci;
            }
        }
        let is_last = target == self.lines.len().saturating_sub(1);
        if best.saturating_add(1) == r.char_x.len() && !r.hard_break && !is_last && best > 0 {
            best = best.saturating_sub(1);
        }
        let byte = self.byte_of_row_char(r, best);
        self.set_cursor(byte, extend);
        self.goal_x = Some(goal);
    }

    /// Preferred column in editor coordinates, preserving the exact visual
    /// side of a wrap or bidi boundary when beginning a vertical movement.
    pub(super) fn vertical_goal_x(&self) -> f64 {
        if let Some(goal) = self.goal_x {
            return self.single_origin.x + goal;
        }
        self.caret_rect_at(&Theme::dark(), self.caret_position()).x0
    }

    pub(super) fn set_vertical_goal_x(&mut self, x: f64) {
        self.goal_x = Some(x - self.single_origin.x);
    }

    pub(super) fn move_visual_row(&mut self, down: bool) {
        self.move_vertical(down, false);
    }

    /// Restore a document selection's active endpoint after its per-block
    /// bands have been painted, retaining wrap/bidi affinity.
    pub(super) fn set_selection_at(&mut self, anchor: usize, cursor: TextPosition) {
        self.set_selection(anchor, cursor.byte);
        self.caret_affinity = cursor.affinity;
    }

    /// Whether arrow navigation can stay within this shaped paragraph.
    pub(super) fn has_visual_row(&self, down: bool) -> bool {
        if let Some(layout) = &self.single_layout {
            let (_, top, height) =
                layout.caret_geometry_at(self.text_to_layout_position(self.caret_position()));
            return if down {
                top + height < layout.height() - 0.5
            } else {
                top > 0.5
            };
        }
        let (row, _) = self.locate_ml(self.cursor_pos);
        if down {
            row.saturating_add(1) < self.lines.len()
        } else {
            row > 0
        }
    }

    fn build_lines(&mut self, tr: &mut TextRenderer, avail_w: f64) {
        let mut wrap = LineWrap {
            size: self.effective_font_size(),
            wrap_w: if avail_w > 1.0 { avail_w } else { f64::MAX },
            lh: self.line_height(),
            y: 0.0,
            lines: Vec::new(),
        };
        let mut para_start = 0usize;
        while let Some(tail) = self.text.get(para_start..) {
            let rel_nl = tail.find('\n');
            let para_end = rel_nl.map_or(self.text.len(), |i| para_start.saturating_add(i));
            self.wrap_paragraph(tr, &mut wrap, para_start..para_end, rel_nl.is_some());
            match rel_nl {
                Some(i) => para_start = para_start.saturating_add(i).saturating_add(1),
                None => break,
            }
        }
        self.lines = wrap.lines;
    }

    /// Uses UAX #14 opportunities, matching cosmic-text's shaped layout. It
    /// falls back to an extended-grapheme boundary only when an unbreakable
    /// segment is itself wider than `wrap_w`.
    ///
    /// Every index below (`a`, `b`, `k`, `row_start_k`, `next_break`) is kept
    /// in range by this function's own loop invariants — `k < nchars ==
    /// boundaries.len() - 1` throughout, and `push_row` is only ever called
    /// with `a <= b <= nchars`. The `.get`/`unwrap_or` fallbacks below exist
    /// so a future change to this invariant degrades gracefully instead of
    /// panicking; they are not expected to trigger.
    fn wrap_paragraph(
        &self,
        tr: &mut TextRenderer,
        wrap: &mut LineWrap,
        span: Range<usize>,
        hard_break: bool,
    ) {
        let Range { start, end } = span;
        let (size, wrap_w, lh) = (wrap.size, wrap.wrap_w, wrap.lh);
        let (y, out) = (&mut wrap.y, &mut wrap.lines);
        let Some(para) = self.text.get(start..end) else {
            return;
        };
        let mut boundaries: Vec<usize> = grapheme_indices(para).map(|(i, _)| i).collect();
        boundaries.push(para.len()); // end sentinel
        let cum: Vec<f64> = boundaries
            .iter()
            .map(|&b| {
                f64::from(
                    self.measure_str(
                        tr,
                        &self.display_slice(start, start.saturating_add(b)),
                        size,
                    )
                    .0,
                )
            })
            .collect();
        let nchars = boundaries.len().saturating_sub(1);
        if nchars == 0 {
            out.push(VisualLine {
                start,
                end,
                hard_break,
                char_x: vec![0.0],
                y: *y,
            });
            *y += lh;
            return;
        }
        let push_row = |out: &mut Vec<VisualLine>, y: &mut f64, a: usize, b: usize, hard: bool| {
            let base = cum.get(a).copied().unwrap_or(0.0);
            let char_x: Vec<f64> = (a..=b)
                .map(|j| cum.get(j).copied().unwrap_or(base) - base)
                .collect();
            out.push(VisualLine {
                start: start.saturating_add(boundaries.get(a).copied().unwrap_or(0)),
                end: start.saturating_add(boundaries.get(b).copied().unwrap_or(para.len())),
                hard_break: hard,
                char_x,
                y: *y,
            });
            *y += lh;
        };
        let breaks: Vec<usize> = line_break_opportunities(para)
            .map(|(byte, _)| byte)
            .collect();
        let mut next_break = 0usize;
        let mut row_start_k = 0usize;
        let mut last_break_k: Option<usize> = None;
        let mut k = 0usize;
        while k < nchars {
            let boundary_k = boundaries.get(k).copied().unwrap_or(usize::MAX);
            while breaks
                .get(next_break)
                .is_some_and(|&byte| byte <= boundary_k)
            {
                if breaks.get(next_break) == Some(&boundary_k) && k > row_start_k {
                    last_break_k = Some(k);
                }
                next_break = next_break.saturating_add(1);
            }
            let row_w = cum.get(k.saturating_add(1)).copied().unwrap_or(0.0)
                - cum.get(row_start_k).copied().unwrap_or(0.0);
            if row_w > wrap_w && k > row_start_k {
                let break_k = last_break_k
                    .filter(|&candidate| candidate > row_start_k)
                    .unwrap_or(k);
                push_row(out, y, row_start_k, break_k, false);
                row_start_k = break_k;
                last_break_k = None;
                continue;
            }
            k = k.saturating_add(1);
        }
        push_row(out, y, row_start_k, nchars, hard_break);
    }

    /// Same family choice as `measure_str`, so glyphs stay aligned with the
    /// measured caret/selection metrics.
    fn draw_text_run(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        text: &str,
        size: f32,
        color: vello::peniko::Color,
        origin: Affine,
    ) {
        if let Some(fam) = &self.family {
            tr.draw_style(
                scene,
                text,
                color,
                origin,
                None,
                &TextStyle::new(size).with_family(fam.clone()),
            );
        } else {
            tr.draw_style(
                scene,
                text,
                color,
                origin,
                None,
                &TextStyle::new(size).with_family(if self.mono {
                    FontChoice::Monospace
                } else {
                    FontChoice::SansSerif
                }),
            );
        }
    }

    fn refresh_layout_epoch(&mut self, renderer: &TextRenderer) {
        if self
            .shaped_layout_epoch
            .as_ref()
            .is_none_or(|epoch| !Arc::ptr_eq(epoch, renderer.layout_epoch()))
        {
            self.single_layout = None;
            self.shaped_layout_epoch = Some(Arc::clone(renderer.layout_epoch()));
        }
    }

    /// Every unmasked multiline input—including attributed source code—uses
    /// the same shaped paragraph for paint, wrapping, carets, selection,
    /// hit-testing, vertical movement, and IME composition.
    /// Rebuilds `self.single_layout` (and its signature) if the shaped
    /// spans, family, size, or available width changed.
    fn ensure_shaped_layout(
        &mut self,
        tr: &mut TextRenderer,
        theme: &Theme,
        style: &TextStyle,
        available_width: f32,
    ) {
        self.refresh_layout_epoch(tr);
        self.refresh_highlight();
        let display = self.layout_display_text();
        let spans = self.highlighted_display_spans(theme);
        debug_assert_eq!(
            spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>(),
            display
        );
        let signature = Self::attributed_signature(
            self.shaped_signature(&display, &style.family, style.size, Some(available_width)),
            &spans,
        );
        if self.single_layout.is_none() || self.shaped_layout_signature != signature {
            let browser_emoji = tr.browser_color_emoji_installed();
            let mut layout = RichTextLayout::shape_styled_reusing_browser(
                &mut tr.font_system,
                &spans,
                style,
                Some(available_width),
                self.align,
                self.single_layout.take(),
                browser_emoji,
            );
            tr.note_browser_rich_layout(&layout);
            if !self.preedit.is_empty() {
                let start = self.cursor_pos.min(self.text.len());
                layout.add_underline(start..start.saturating_add(self.preedit.len()));
            }
            self.single_layout = Some(layout);
            self.shaped_layout_signature = signature;
        }
    }

    fn multiline_style(&self, scale: f32) -> TextStyle {
        let family = self.family.clone().unwrap_or(if self.mono {
            FontChoice::Monospace
        } else {
            FontChoice::SansSerif
        });
        // Already grown by the text scale here, so the renderer paints it as
        // given.
        let mut style = TextStyle::new(self.effective_font_size() * scale)
            .with_line_height(LINE_HEIGHT_RATIO)
            .with_family(family);
        style.scaling = TextScaling::Resolved;
        style.kind = self.kind.text_kind();
        style
    }

    fn update_gutter_metrics(&mut self, tr: &mut TextRenderer, style: &TextStyle, scale: f32) {
        if !self.gutter || !self.multiline {
            self.measured_gutter_width = 0.0;
            return;
        }
        let lines = self
            .text
            .bytes()
            .filter(|&byte| byte == b'\n')
            .count()
            .saturating_add(1);
        let label = lines.max(10).to_string();
        let (number_width, _) = tr.measure_style(&label, None, style);
        self.measured_gutter_width =
            f64::from(number_width) + (GUTTER_PAD_L + GUTTER_PAD_R) * f64::from(scale);
    }

    /// Keeps the caret in view by adjusting `self.scroll_y`. Only re-centres
    /// when the caret moved since the last draw, so a wheel scroll away from
    /// it sticks.
    fn update_ml_scroll(&mut self, caret_top: f32, caret_height: f32, layout_height: f32) {
        let view_height = (self.rect.height() - self.multiline_padding_y() * 2.0).max(1.0);
        if self.is_focused && self.last_scroll_caret != Some(self.cursor_pos) {
            if f64::from(caret_top) - self.scroll_y < 0.0 {
                self.scroll_y = f64::from(caret_top);
            } else if f64::from(caret_top + caret_height) - self.scroll_y > view_height {
                self.scroll_y = f64::from(caret_top + caret_height) - view_height;
            }
        } else if !self.is_focused && !self.preserve_unfocused_scroll {
            self.scroll_y = 0.0;
        }
        self.last_scroll_caret = Some(self.cursor_pos);
        let max_scroll = (f64::from(layout_height) - view_height).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
    }

    /// Embedded auto-height editors must reserve exactly the paragraph and
    /// padding used by painting, including attributed runs and empty lines.
    pub(super) fn measure_wrapped_content(
        &mut self,
        width: f32,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> f32 {
        self.gutter_text_scale = f64::from(theme.text_scale);
        let available = (f64::from(width) - self.ml_left() - ML_PAD_X).max(1.0);
        let style = self.multiline_style(theme.text_scale);
        #[allow(clippy::cast_possible_truncation, reason = "bounded widget width")]
        let available_width = available as f32;
        self.ensure_shaped_layout(tr, theme, &style, available_width);
        let minimum = self.effective_font_size() * theme.text_scale * LINE_HEIGHT_RATIO;
        let height = self
            .single_layout
            .as_ref()
            .map_or(minimum, |layout| layout.height().max(minimum));
        height + if self.compact_multiline { 0.0 } else { 16.0 }
    }

    /// Prepare the shared shaped hit-testing geometry for a projected cell
    /// before its first focus/click, without painting a throwaway scene.
    pub(super) fn prepare_wrapped_layout(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded widget geometry is consumed by the f32 text layout API"
        )]
        let width = self.rect.width() as f32;
        self.measure_wrapped_content(width, tr, theme);
        self.single_origin = Point::new(
            self.rect.x0 + self.ml_left(),
            self.rect.y0 + self.multiline_padding_y() - self.scroll_y,
        );
        self.multiline_shaped = true;
    }

    fn draw_code_gutter_background(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        gutter_width: f64,
    ) {
        if gutter_width <= 0.0 {
            return;
        }
        let gutter = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x0 + gutter_width,
            self.rect.y1,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme
                .tokens
                .colors
                .lift_on(theme.tokens.colors.surfaces.raised, 0.5)
                .with_alpha_factor(0.35),
            None,
            &gutter,
        );
        let edge = self.rect.x0 + gutter_width;
        // Gutter rule derived from the field it divides (HIG §9.5).
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::surface::edge_on(theme.tokens.colors.surfaces.raised),
            None,
            &vello::kurbo::Line::new((edge, self.rect.y0), (edge, self.rect.y1)),
        );
    }

    fn draw_current_line_band(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        caret_byte: usize,
        gutter_width: f64,
    ) {
        if !(self.highlight_current_line && self.is_focused && !self.shows_placeholder()) {
            return;
        }
        let Some(layout) = &self.single_layout else {
            return;
        };
        let band_color = theme
            .tokens
            .colors
            .lift_on(theme.tokens.colors.surfaces.raised, 0.5)
            .with_alpha_factor(0.6);
        for (top, height) in layout.logical_line_bands(caret_byte) {
            let y0 = self.single_origin.y + f64::from(top);
            let band = Rect::new(
                self.rect.x0 + gutter_width,
                y0,
                self.rect.x1,
                y0 + f64::from(height),
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                band_color,
                None,
                &band,
            );
        }
    }

    /// The shaped code paragraph applies `Theme::text_scale` directly because
    /// it bypasses [`TextRenderer`]. Gutter labels go through `TextRenderer`,
    /// which applies that scale itself. Convert the paragraph style back to
    /// its unscaled form so a 200% line number is measured and painted once,
    /// not twice (which would push two-digit labels beyond the left edge).
    #[cfg(test)]
    fn gutter_number_style(style: &TextStyle, theme: &Theme) -> TextStyle {
        let mut gutter = style.clone();
        let scale = theme.text_scale;
        if (scale - 1.0).abs() > f32::EPSILON {
            gutter.size /= scale;
            gutter.letter_spacing /= scale;
        }
        gutter
    }

    fn draw_gutter_line_numbers(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        style: &TextStyle,
        caret_byte: usize,
        gutter_width: f64,
    ) {
        if gutter_width <= 0.0 {
            return;
        }
        let Some(layout) = &self.single_layout else {
            return;
        };
        let current_line = layout.logical_line_index(caret_byte);
        for (line, top, _height) in layout.logical_line_origins() {
            let number = line.saturating_add(1).to_string();
            let (width, _) = tr.measure_style(&number, None, style);
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine transform composition over f64 screen coordinates"
            )]
            let origin = transform
                * Affine::translate((
                    self.rect.x0 + gutter_width
                        - GUTTER_PAD_R * self.gutter_text_scale
                        - f64::from(width),
                    self.single_origin.y + f64::from(top),
                ));
            let color = if line == current_line {
                theme.text_primary
            } else {
                theme.text_dim
            };
            tr.draw_style(scene, &number, color, origin, None, style);
        }
    }

    pub(crate) fn suppress_caret_paint(&mut self, suppressed: bool) {
        self.caret_paint_suppressed = suppressed;
    }

    fn draw_ml_caret(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        caret_position: TextPosition,
    ) {
        if self.caret_paint_suppressed || !self.is_focused || self.effectively_disabled() {
            return;
        }
        let Some(layout) = self.single_layout.as_ref() else {
            return;
        };
        let (x, top, height) = layout.caret_geometry_at(caret_position);
        let caret = Rect::new(
            self.single_origin.x + f64::from(x),
            self.single_origin.y + f64::from(top),
            self.single_origin.x + f64::from(x) + theme.caret_width,
            self.single_origin.y + f64::from(top + height),
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.intent.you,
            None,
            &caret,
        );
    }

    /// Every unmasked multiline input—including attributed source code—uses
    /// the same shaped paragraph for paint, wrapping, carets, selection,
    /// hit-testing, vertical movement and IME composition.
    fn draw_multiline_shaped(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        shape: &RoundedRect,
    ) {
        let size = self.effective_font_size();
        let style = self.multiline_style(theme.text_scale);
        self.update_gutter_metrics(tr, &style, theme.text_scale);
        let left = self.ml_left();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "widget width is a bounded on-screen size for vello/layout math, never near f32's range"
        )]
        let available_width = (self.rect.width() - left - ML_PAD_X).max(1.0) as f32;
        self.ensure_shaped_layout(tr, theme, &style, available_width);

        let caret_position = if self.preedit.is_empty() {
            self.text_to_layout_position(self.caret_position())
        } else {
            TextPosition::new(self.preedit_layout_caret())
        };
        let caret_byte = caret_position.byte;
        let Some((caret_top, caret_height, layout_height)) =
            self.single_layout.as_ref().map(|layout| {
                let (_, top, height) = layout.caret_geometry_at(caret_position);
                (top, height, layout.height())
            })
        else {
            // Unreachable: `ensure_shaped_layout` just set `self.single_layout` to `Some`.
            return;
        };
        self.update_ml_scroll(caret_top, caret_height, layout_height);
        self.single_origin = Point::new(
            self.rect.x0 + left,
            self.rect.y0 + self.multiline_padding_y() - self.scroll_y,
        );
        self.multiline_shaped = true;
        self.lines.clear();

        scene.push_layer(
            vello::peniko::Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            shape,
        );

        let gutter_width = self.code_gutter_w();
        self.draw_code_gutter_background(scene, transform, theme, gutter_width);
        self.draw_current_line_band(scene, transform, theme, caret_byte, gutter_width);

        if let Some((start, end)) = self.selection_range() {
            let color = theme.tokens.colors.intent.selection_tint;
            for rect in self.selection_rects(theme, start, end) {
                scene.fill(vello::peniko::Fill::NonZero, transform, color, None, &rect);
            }
        }

        // `Affine * Affine` composes two geometric transforms over f64
        // coordinates; it has no integer overflow/wrap semantics.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine transform composition over f64 screen coordinates"
        )]
        let text_transform =
            transform * Affine::translate((self.single_origin.x, self.single_origin.y));
        if self.shows_placeholder() {
            let placeholder = crate::i18n!(&self.placeholder);
            tr.draw_text(
                scene,
                &placeholder,
                size,
                theme.text_dim,
                text_transform,
                Some(available_width),
            );
        } else if let Some(layout) = &self.single_layout {
            let color = if self.effectively_disabled() {
                theme.text_dim
            } else {
                theme.text_primary
            };
            tr.draw_rich(scene, layout, text_transform, color);
        }

        self.draw_gutter_line_numbers(
            scene,
            transform,
            tr,
            theme,
            &style,
            caret_byte,
            gutter_width,
        );
        self.draw_ml_caret(scene, transform, theme, caret_position);
        scene.pop_layer();
    }

    /// Adjusts `self.scroll_y` so the caret row stays in view, re-centring
    /// only on caret movement (matches [`Self::update_ml_scroll`]'s policy
    /// for the shaped path).
    fn update_ml_body_scroll(&mut self, lh: f64) {
        let (caret_row, _) = self.locate_ml(self.cursor_pos);
        let caret_top = self.lines.get(caret_row).map_or(0.0, |r| r.y);
        let caret_bot = caret_top + lh;
        let view_h = self.rect.height() - self.multiline_padding_y() * 2.0;
        let caret_moved = self.last_scroll_caret != Some(self.cursor_pos);
        if self.is_focused {
            if caret_moved {
                if caret_top - self.scroll_y < 0.0 {
                    self.scroll_y = caret_top;
                } else if caret_bot - self.scroll_y > view_h {
                    self.scroll_y = caret_bot - view_h;
                }
            }
        } else if !self.preserve_unfocused_scroll {
            self.scroll_y = 0.0;
        }
        self.last_scroll_caret = Some(self.cursor_pos);
        let rows = f64::from(u32::try_from(self.lines.len()).unwrap_or(u32::MAX));
        let content_h = rows * lh;
        let max_scroll = (content_h - view_h).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
    }

    /// Per-row logical-line index and whether the row starts a new logical
    /// line (a row starts one iff the previous row ended at a hard `\n`),
    /// plus which logical line the caret is on.
    fn ml_body_row_metadata(&self) -> LogicalRows {
        let mut line_of_row: Vec<usize> = Vec::with_capacity(self.lines.len());
        let mut is_line_start: Vec<bool> = Vec::with_capacity(self.lines.len());
        let mut cur_line = 0usize;
        let mut prev_hard_break = false;
        for (i, line) in self.lines.iter().enumerate() {
            let starts_new_line = i == 0 || prev_hard_break;
            if i > 0 && prev_hard_break {
                cur_line = cur_line.saturating_add(1);
            }
            line_of_row.push(cur_line);
            is_line_start.push(starts_new_line);
            prev_hard_break = line.hard_break;
        }
        let caret_idx = self.cursor_pos.min(self.text.len());
        let caret_line = self
            .text
            .get(..caret_idx)
            .map_or(0, |s| s.bytes().filter(|&b| b == b'\n').count());
        LogicalRows {
            line_of_row,
            is_line_start,
            caret_line,
        }
    }

    fn draw_body_current_line_band(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rows: &LogicalRows,
        lh: f64,
        gutter_w: f64,
    ) {
        let (line_of_row, caret_line) = (&rows.line_of_row, rows.caret_line);
        if !(self.highlight_current_line && self.is_focused && !self.shows_placeholder()) {
            return;
        }
        let view_h = self.rect.height() - self.multiline_padding_y() * 2.0;
        let band_color = theme
            .tokens
            .colors
            .lift_on(theme.tokens.colors.surfaces.raised, 0.5)
            .with_alpha_factor(0.6);
        for (row, &lor) in self.lines.iter().zip(line_of_row.iter()) {
            if lor != caret_line {
                continue;
            }
            let top = row.y - self.scroll_y;
            if top + lh < 0.0 || top > view_h {
                continue;
            }
            let y0 = self.rect.y0 + self.multiline_padding_y() + top;
            let band = Rect::new(self.rect.x0 + gutter_w, y0, self.rect.x1, y0 + lh);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                band_color,
                None,
                &band,
            );
        }
    }

    /// Collected as owned data so all `self` borrows end before the draw calls.
    fn collect_ml_body_rows(
        &self,
        rows: &LogicalRows,
        lh: f64,
        text_color: vello::peniko::Color,
        theme: &Theme,
    ) -> (RowDraws, GutterNums) {
        let LogicalRows {
            line_of_row,
            is_line_start,
            caret_line,
        } = rows;
        let caret_line = *caret_line;
        let view_h = self.rect.height() - self.multiline_padding_y() * 2.0;
        let gutter_w = self.code_gutter_w();
        let mut row_draws: RowDraws = Vec::new();
        let mut gutter_nums: GutterNums = Vec::new();
        for (i, ((line, &li), &starts_new_line)) in self
            .lines
            .iter()
            .zip(line_of_row.iter())
            .zip(is_line_start.iter())
            .enumerate()
        {
            let top = line.y - self.scroll_y;
            if top + lh < 0.0 || top > view_h {
                continue;
            }
            if gutter_w > 0.0 && starts_new_line {
                gutter_nums.push((line.y, li.saturating_add(1).to_string(), li == caret_line));
            }
            let segs = self.row_segments(i, li, text_color, theme);
            if !segs.is_empty() {
                row_draws.push((line.y, segs));
            }
        }
        (row_draws, gutter_nums)
    }

    fn draw_body_gutter_numbers(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        size: f32,
        gutter_w: f64,
        gutter_nums: &[(f64, String, bool)],
    ) {
        if gutter_w <= 0.0 {
            return;
        }
        for (ry, num, current) in gutter_nums {
            let w = f64::from(self.measure_str(tr, num, size).0);
            let x = self.rect.x0 + gutter_w - GUTTER_PAD_R * self.gutter_text_scale - w;
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine transform composition over f64 screen coordinates"
            )]
            let origin = transform
                * Affine::translate((
                    x,
                    self.rect.y0 + self.multiline_padding_y() + ry - self.scroll_y,
                ));
            let col = if *current {
                theme.text_primary
            } else {
                theme.text_dim
            };
            self.draw_text_run(scene, tr, num, size, col, origin);
        }
    }

    fn draw_body_rows(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        size: f32,
        left: f64,
        row_draws: &[(f64, Vec<Seg>)],
    ) {
        for (ry, segs) in row_draws {
            for (text, x, color) in segs {
                if text.is_empty() {
                    continue;
                }
                #[allow(
                    clippy::arithmetic_side_effects,
                    reason = "Affine transform composition over f64 screen coordinates"
                )]
                let origin = transform
                    * Affine::translate((
                        self.rect.x0 + left + x,
                        self.rect.y0 + self.multiline_padding_y() + ry - self.scroll_y,
                    ));
                self.draw_text_run(scene, tr, text, size, *color, origin);
            }
        }
    }

    fn draw_multiline_body(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        shape: &RoundedRect,
    ) {
        let size = self.effective_font_size();
        let lh = self.line_height();
        let left = self.ml_left();
        let gutter_w = self.code_gutter_w();
        let avail_w = self.rect.width() - left - ML_PAD_X;
        self.build_lines(tr, avail_w);
        self.refresh_highlight();
        self.update_ml_body_scroll(lh);

        scene.push_layer(
            vello::peniko::Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            shape,
        );

        let rows = self.ml_body_row_metadata();
        self.draw_code_gutter_background(scene, transform, theme, gutter_w);
        self.draw_body_current_line_band(scene, transform, theme, &rows, lh, gutter_w);

        if let Some((s, e)) = self.selection_range() {
            let sel_color = theme.tokens.colors.intent.selection_tint;
            for r in self.selection_rects(theme, s, e) {
                scene.fill(vello::peniko::Fill::NonZero, transform, sel_color, None, &r);
            }
        }

        if self.shows_placeholder() {
            let placeholder = crate::i18n!(&self.placeholder);
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine transform composition over f64 screen coordinates"
            )]
            let origin = transform
                * Affine::translate((
                    self.rect.x0 + left,
                    self.rect.y0 + self.multiline_padding_y() - self.scroll_y,
                ));
            tr.draw_text(scene, &placeholder, size, theme.text_dim, origin, None);
        }

        let text_color = if self.effectively_disabled() {
            theme.text_dim
        } else {
            theme.text_primary
        };
        let (row_draws, gutter_nums) = self.collect_ml_body_rows(&rows, lh, text_color, theme);

        self.draw_body_gutter_numbers(scene, transform, tr, theme, size, gutter_w, &gutter_nums);
        self.draw_body_rows(scene, transform, tr, size, left, &row_draws);

        if !self.caret_paint_suppressed && self.is_focused && !self.effectively_disabled() {
            let caret = self.caret_rect_ml(self.cursor_pos, theme.caret_width);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.intent.you,
                None,
                &caret,
            );
        }

        scene.pop_layer();
    }

    /// Splits the row against the logical line's [`Span`]s when a
    /// highlighter is set, gaps kept `default`. Returned `x` is line-relative
    /// (add [`ml_left`] + `rect.x0` to place it).
    fn row_segments(
        &self,
        i: usize,
        line_idx: usize,
        default: vello::peniko::Color,
        theme: &Theme,
    ) -> Vec<Seg> {
        let Some(r) = self.lines.get(i) else {
            return Vec::new();
        };
        let (rs, re) = (r.start, r.end);
        let mut out: Vec<Seg> = Vec::new();
        if self.highlighter.is_some()
            && let Some(spans) = self.hl_spans.get(line_idx)
            && let Some(&ls) = self.hl_line_starts.get(line_idx)
        {
            let mut cur = rs;
            for sp in spans {
                let ss = ls.saturating_add(sp.range.start);
                let se = ls.saturating_add(sp.range.end);
                let a = ss.max(rs).max(cur);
                let b = se.min(re);
                if a >= b {
                    continue;
                }
                if a > cur
                    && let Some(before) = self.text.get(cur..a)
                {
                    out.push((before.to_string(), self.x_in_row(r, cur), default));
                }
                if let Some(mid) = self.text.get(a..b) {
                    out.push((
                        mid.to_string(),
                        self.x_in_row(r, a),
                        theme.tokens.syntax.color(sp.kind),
                    ));
                }
                cur = b;
            }
            if cur < re
                && let Some(tail) = self.text.get(cur..re)
            {
                out.push((tail.to_string(), self.x_in_row(r, cur), default));
            }
        } else {
            let text = self.display_slice(rs, re);
            if !text.is_empty() {
                out.push((text, 0.0, default));
            }
        }
        out
    }

    /// Test-only view of the attributed paragraph's colour runs, for
    /// `CodeEditor` tests to assert highlighter spans reach the shared layout.
    /// Requires a prior draw so the highlight cache is current.
    #[cfg(test)]
    pub(crate) fn code_row_colors(&self, theme: &Theme) -> Vec<(String, vello::peniko::Color)> {
        let default = if self.effectively_disabled() {
            theme.text_dim
        } else {
            theme.text_primary
        };
        self.highlighted_display_spans(theme)
            .into_iter()
            .filter(|span| !span.text.is_empty())
            .map(|span| {
                let color = span.color.map_or(default, |[r, g, b]| {
                    vello::peniko::Color::from_rgb8(r, g, b)
                });
                (span.text, color)
            })
            .collect()
    }

    /// `Some(text)` if a user-initiated edit landed since the last call.
    pub fn take_dirty(&mut self) -> Option<String> {
        if self.dirty {
            // Publishing the edit to the document does not mean the host has
            // measured its new intrinsic height yet. Keep those lifecycles
            // separate so CRDT projection cannot swallow layout invalidation.
            self.model_layout_dirty |= self.multiline;
            self.dirty = false;
            Some(self.text.clone())
        } else {
            None
        }
    }

    pub(super) fn acknowledge_layout_measure(&mut self) {
        self.model_layout_dirty = false;
    }

    /// Bypasses `draw_with_presence` — only safe on local-mode `TextInputs`
    /// with no presence path or permission scope.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.single_origin.x += rect.x0 - self.rect.x0;
        self.single_origin.y += rect.y0 - self.rect.y0;
        self.rect = rect;
        self.refresh_multiline_origin();
    }

    fn refresh_multiline_origin(&mut self) {
        if self.multiline && self.multiline_shaped {
            self.single_origin = Point::new(
                self.rect.x0 + self.ml_left(),
                self.rect.y0 + self.multiline_padding_y() - self.scroll_y,
            );
        }
    }

    /// Adopt the paragraph just shaped by [`Self::measure_wrapped_content`]
    /// for hit-testing and carets, as painting would. A host that skips
    /// painting an off-screen field keeps its geometry current this way.
    pub(super) fn adopt_measured_layout(&mut self) {
        if self.multiline && !self.masked() && self.single_layout.is_some() {
            self.multiline_shaped = true;
            self.lines.clear();
            self.refresh_multiline_origin();
        }
    }

    /// Embedded hosts that route movement only to their focused child must
    /// transfer the current pointer before forwarding a press to a new child.
    /// This updates hover/hit-test input without changing caret or selection.
    pub(super) fn update_pointer_position(&mut self, point: Point) -> bool {
        let hover = self.rect.contains(point);
        let changed = hover != self.is_hovered;
        self.is_hovered = hover;
        self.last_mouse_pos = point;
        changed
    }

    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.id = id.to_string();
        self
    }

    pub(crate) fn set_id(&mut self, id: &str) {
        self.id = id.to_string();
    }

    /// Bind this input to one root-level document string.
    ///
    /// Prefer [`Self::from_document_string`] when constructing a new field;
    /// this builder remains useful when the rest of the input is assembled
    /// dynamically first.
    #[must_use]
    pub fn with_on_change_path(mut self, path: &str) -> Self {
        self.local_state = None;
        self.on_change_path = path.to_string();
        self.crdt_bound = !path.is_empty();
        self
    }

    /// Mirror edits into caller-owned process-local memory without creating a
    /// CRDT object. Intended for passwords/PINs; use with [`Self::local`].
    #[must_use]
    pub fn with_external_buffer(mut self, buffer: Arc<Mutex<String>>) -> Self {
        self.local_state = None;
        self.external_buffer = Some(buffer);
        self.crdt_bound = false;
        self
    }

    #[must_use]
    pub fn with_secret(mut self, on: bool) -> Self {
        self.secret = on;
        if on {
            self.kind = TextInputKind::Password;
        }
        self
    }

    pub fn set_secret(&mut self, on: bool) {
        self.secret = on;
        if on {
            self.set_kind(TextInputKind::Password);
        }
    }

    pub fn is_secret(&self) -> bool {
        self.secret
    }

    /// Only meaningful together with `with_secret`.
    #[must_use]
    pub fn with_reveal(mut self, on: bool) -> Self {
        self.reveal = on;
        self
    }

    pub fn set_reveal(&mut self, on: bool) {
        self.reveal = on;
    }

    /// No-op on a non-secret field.
    pub fn toggle_reveal(&mut self) {
        if self.secret {
            self.reveal = !self.reveal;
        }
    }

    pub fn is_revealed(&self) -> bool {
        self.reveal
    }

    #[must_use]
    pub fn multiline(mut self, on: bool) -> Self {
        self.multiline = on;
        self
    }

    pub fn set_multiline(&mut self, on: bool) {
        self.multiline = on;
    }

    pub(super) fn set_compact_multiline(&mut self, compact: bool) {
        if self.compact_multiline != compact {
            self.compact_multiline = compact;
            self.model_layout_dirty = true;
        }
    }

    fn multiline_padding_y(&self) -> f64 {
        if self.compact_multiline {
            0.0
        } else {
            ML_PAD_Y
        }
    }

    pub fn is_multiline(&self) -> bool {
        self.multiline
    }

    // Power `CodeEditor`; every setter below defaults off.
    #[must_use]
    pub fn with_gutter(mut self, on: bool) -> Self {
        self.gutter = on;
        self
    }

    pub fn set_gutter(&mut self, on: bool) {
        self.gutter = on;
    }

    pub fn has_gutter(&self) -> bool {
        self.gutter
    }

    #[must_use]
    pub fn with_current_line_highlight(mut self, on: bool) -> Self {
        self.highlight_current_line = on;
        self
    }

    pub fn set_current_line_highlight(&mut self, on: bool) {
        self.highlight_current_line = on;
    }

    #[must_use]
    pub fn with_highlighter(mut self, h: Box<dyn Highlighter>) -> Self {
        self.highlighter = Some(h);
        self.hl_sig = u64::MAX; // force a rebuild on next draw
        self
    }

    pub fn set_highlighter(&mut self, h: Option<Box<dyn Highlighter>>) {
        self.highlighter = h;
        self.hl_sig = u64::MAX;
    }

    /// `0` = literal `\t`. `Shift+Tab` outdents by the same width.
    #[must_use]
    pub fn with_tab_width(mut self, n: usize) -> Self {
        self.tab_spaces = Some(n);
        self
    }

    pub fn set_tab_width(&mut self, n: usize) {
        self.tab_spaces = Some(n);
    }

    /// Uses measured line-number glyphs after layout, with a font-size
    /// estimate only before the first measurement.
    fn code_gutter_w(&self) -> f64 {
        if !self.gutter || !self.multiline {
            return 0.0;
        }
        if self.measured_gutter_width > 0.0 {
            return self.measured_gutter_width;
        }
        let lines = self
            .text
            .bytes()
            .filter(|&b| b == b'\n')
            .count()
            .saturating_add(1);
        // Counted with integer division rather than `log10().floor()` so a
        // float-precision wobble near a power of ten can't under-count a digit.
        let mut digits = 0u32;
        let mut n = lines;
        while n > 0 {
            digits = digits.saturating_add(1);
            n /= 10;
        }
        let digits = digits.max(2);
        let scale = self.gutter_text_scale;
        let adv = f64::from(self.effective_font_size()) * scale * GUTTER_DIGIT_RATIO;
        GUTTER_PAD_L * scale + f64::from(digits) * adv + GUTTER_PAD_R * scale
    }

    fn ml_left(&self) -> f64 {
        ML_PAD_X + self.code_gutter_w()
    }

    /// Threads the cross-line carry over all lines so block comments / fenced
    /// code stay coherent.
    fn refresh_highlight(&mut self) {
        let Some(hl) = &self.highlighter else {
            self.hl_spans.clear();
            self.hl_line_starts.clear();
            return;
        };
        // FNV-1a signature so re-tokenising is skipped when the text hasn't changed.
        let mut hasher = fnv::FnvHasher::default();
        std::hash::Hasher::write(&mut hasher, self.text.as_bytes());
        let sig = std::hash::Hasher::finish(&hasher) ^ self.text.len() as u64;
        if sig == self.hl_sig && !self.hl_line_starts.is_empty() {
            return;
        }
        self.hl_sig = sig;
        self.hl_spans.clear();
        self.hl_line_starts.clear();
        let mut carry = 0u64;
        let mut start = 0usize;
        for line in self.text.split('\n') {
            self.hl_line_starts.push(start);
            let ls = hl.spans(line, carry);
            carry = ls.carry;
            self.hl_spans.push(ls.spans);
            start = start.saturating_add(line.len()).saturating_add(1);
        }
    }

    /// Read the signal with `take_submit`.
    #[must_use]
    pub fn with_enter_submits(mut self, on: bool) -> Self {
        self.enter_submits = on;
        self
    }

    pub fn set_enter_submits(&mut self, on: bool) {
        self.enter_submits = on;
    }

    /// `true` once per submitting `Enter`; clears on read.
    pub fn take_submit(&mut self) -> bool {
        std::mem::take(&mut self.submit_requested)
    }

    #[must_use]
    pub fn disabled(mut self, on: bool) -> Self {
        self.disabled = on;
        self
    }

    /// Drops any in-progress drag so the widget doesn't hold pointer capture while inert.
    pub fn set_disabled(&mut self, on: bool) {
        self.disabled = on;
        if on {
            self.is_dragging = false;
            self.is_hovered = false;
        }
    }

    pub fn is_disabled(&self) -> bool {
        self.effectively_disabled()
    }

    #[must_use]
    pub fn with_placeholder(mut self, hint: &str) -> Self {
        self.placeholder = hint.to_string();
        self
    }

    /// Set the stable label announced for this field by assistive technology.
    /// Placeholder text is intentionally not used as the label because it
    /// disappears after entry and often describes format rather than purpose.
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = name.into();
        self
    }

    pub fn set_placeholder(&mut self, hint: &str) {
        self.placeholder = hint.to_string();
    }

    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }

    pub fn shows_placeholder(&self) -> bool {
        !self.placeholder.is_empty() && self.text.is_empty() && !self.is_focused
    }

    fn masked(&self) -> bool {
        self.secret && !self.reveal
    }

    /// One bullet per *character* of the real text when masked.
    fn display_text(&self) -> String {
        if self.masked() {
            "•".repeat(self.text.chars().count())
        } else {
            self.text.clone()
        }
    }

    /// Text passed to the shaped single-line layout. IME preedit occupies a
    /// temporary display-only byte range at the committed caret.
    fn layout_display_text(&self) -> String {
        let committed = self.display_text();
        if self.preedit.is_empty() || self.masked() {
            return committed;
        }
        // `masked()` is false here, so `committed == self.text` byte-for-byte
        // and `caret` (already clamped to a char boundary of `self.text`) is
        // a valid boundary of `committed` too.
        let caret = self.cursor_pos.min(self.text.len());
        let Some(before) = committed.get(..caret) else {
            return committed;
        };
        let after = committed.get(caret..).unwrap_or("");
        let mut display = String::with_capacity(committed.len().saturating_add(self.preedit.len()));
        display.push_str(before);
        display.push_str(&self.preedit);
        display.push_str(after);
        display
    }

    fn text_to_layout_byte(&self, byte: usize) -> usize {
        let byte = grapheme_boundary_at_or_before(&self.text, byte);
        if self.preedit.is_empty() || self.masked() || byte <= self.cursor_pos {
            byte
        } else {
            byte.saturating_add(self.preedit.len())
        }
    }

    fn text_to_layout_position(&self, position: TextPosition) -> TextPosition {
        TextPosition::with_affinity(self.text_to_layout_byte(position.byte), position.affinity)
    }

    fn layout_to_text_byte(&self, byte: usize) -> usize {
        if self.preedit.is_empty() || self.masked() {
            return grapheme_boundary_at_or_before(&self.text, byte);
        }
        let caret = self.cursor_pos.min(self.text.len());
        if byte <= caret {
            byte
        } else if byte < caret.saturating_add(self.preedit.len()) {
            caret
        } else {
            byte.saturating_sub(self.preedit.len())
        }
    }

    fn layout_to_text_position(&self, position: TextPosition) -> TextPosition {
        TextPosition::with_affinity(self.layout_to_text_byte(position.byte), position.affinity)
    }

    fn preedit_layout_caret(&self) -> usize {
        if self.preedit.is_empty() || self.masked() {
            return self.text_to_layout_byte(self.cursor_pos);
        }
        let within = self
            .preedit_cursor
            .map_or(self.preedit.len(), |(_, end)| end.min(self.preedit.len()));
        self.cursor_pos
            .min(self.text.len())
            .saturating_add(grapheme_boundary_at_or_before(&self.preedit, within))
    }

    /// Keyed by real-text byte offsets so caret placement stays correct against the masked glyph run.
    fn display_prefix(&self, byte_idx: usize) -> String {
        let Some(prefix) = self.text.get(..byte_idx) else {
            return String::new();
        };
        if self.masked() {
            "•".repeat(prefix.chars().count())
        } else {
            prefix.to_string()
        }
    }

    fn display_slice(&self, start: usize, end: usize) -> String {
        let Some(slice) = self.text.get(start..end) else {
            return String::new();
        };
        if self.masked() {
            "•".repeat(slice.chars().count())
        } else {
            slice.to_string()
        }
    }

    fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        if anchor == self.cursor_pos {
            return None;
        }
        Some((anchor.min(self.cursor_pos), anchor.max(self.cursor_pos)))
    }

    fn has_selection(&self) -> bool {
        self.selection_range().is_some()
    }

    fn selected_str(&self) -> Option<&str> {
        let (s, e) = self.selection_range()?;
        self.text.get(s..e)
    }

    fn set_cursor(&mut self, pos: usize, extend: bool) {
        self.set_cursor_position(TextPosition::new(pos), extend);
    }

    fn set_cursor_position(&mut self, position: TextPosition, extend: bool) {
        let pos = position.byte.min(self.text.len());
        if extend {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor_pos);
            }
        } else {
            self.selection_anchor = None;
        }
        self.cursor_pos = pos;
        self.caret_affinity = position.affinity;
        // `move_vertical` re-sets this right after calling here, so a run of
        // Up/Down still tracks its column despite the reset.
        self.goal_x = None;
    }

    /// No-op if there's no selection.
    fn delete_selection(&mut self, state: &mut AutoCommit, obj_id: Option<&ObjId>) {
        let Some((start, end)) = self.selection_range() else {
            return;
        };
        let (Some(before), Some(selected)) = (self.text.get(..start), self.text.get(start..end))
        else {
            return;
        };
        let char_start = before.chars().count();
        let char_count = selected.chars().count();
        self.text.replace_range(start..end, "");
        self.cursor_pos = start;
        self.caret_affinity = TextAffinity::Before;
        self.selection_anchor = None;
        self.goal_x = None;
        self.last_edit_time = Some(crate::time::now());
        if let Some(obj_id) = obj_id {
            let del = isize::try_from(char_count).unwrap_or(isize::MAX);
            let _ = state.splice_text(obj_id, char_start, del, "");
        }
        self.dirty = true;
    }

    fn insert_str(&mut self, s: &str, state: &mut AutoCommit, obj_id: Option<&ObjId>) {
        let filtered;
        let s = if let Some(filter) = self.char_filter {
            filtered = s.chars().filter(|ch| filter(*ch)).collect::<String>();
            filtered.as_str()
        } else {
            s
        };
        if s.is_empty() {
            return;
        }
        self.last_autocorrection = None;
        // Char index the inserted text lands at, to stamp the authorship mark over it.
        let insert_char_at;
        if let Some((start, end)) = self.selection_range() {
            let (Some(before), Some(selected)) =
                (self.text.get(..start), self.text.get(start..end))
            else {
                return;
            };
            let char_start = before.chars().count();
            let char_count = selected.chars().count();
            self.text.replace_range(start..end, s);
            self.cursor_pos = start.saturating_add(s.len());
            self.caret_affinity = TextAffinity::Before;
            self.selection_anchor = None;
            if let Some(obj_id) = obj_id {
                let del = isize::try_from(char_count).unwrap_or(isize::MAX);
                let _ = state.splice_text(obj_id, char_start, del, s);
            }
            insert_char_at = char_start;
        } else {
            let Some(before) = self.text.get(..self.cursor_pos) else {
                return;
            };
            let char_idx = before.chars().count();
            self.text.insert_str(self.cursor_pos, s);
            if let Some(obj_id) = obj_id {
                let _ = state.splice_text(obj_id, char_idx, 0, s);
            }
            self.cursor_pos = self.cursor_pos.saturating_add(s.len());
            self.selection_anchor = None;
            self.caret_affinity = TextAffinity::Before;
            insert_char_at = char_idx;
        }
        // A drag can leave an anchor equal to the old caret. Insertion must
        // collapse that anchor too, or the newly inserted text becomes selected.
        self.selection_anchor = None;
        // Latest-wins per position, so a peer typing into another's text splits their run correctly.
        if let (Some(obj_id), Some(author)) = (obj_id, &self.local_author) {
            let nchars = s.chars().count();
            if nchars > 0 {
                let mark = automerge::marks::Mark::new(
                    AUTHOR_MARK.to_string(),
                    author.clone(),
                    insert_char_at,
                    insert_char_at.saturating_add(nchars),
                );
                let _ = state.mark(obj_id, mark, automerge::marks::ExpandMark::None);
            }
        }
        self.goal_x = None;
        self.last_edit_time = Some(crate::time::now());
        self.dirty = true;
    }

    /// `width == 0` removes a single leading whitespace char instead of a tab-stop.
    fn outdent(&mut self, width: usize, state: &mut AutoCommit, obj_id: Option<&ObjId>) {
        let Some(before_caret) = self.text.get(..self.cursor_pos) else {
            return;
        };
        let line_start = before_caret.rfind('\n').map_or(0, |i| i.saturating_add(1));
        let w = if width == 0 { 1 } else { width };
        let Some(line_tail) = self.text.get(line_start..) else {
            return;
        };
        let mut remove = 0usize;
        for c in line_tail.chars() {
            if c == '\t' {
                remove = 1;
                break;
            } else if c == ' ' && remove < w {
                remove = remove.saturating_add(1);
            } else {
                break;
            }
        }
        if remove == 0 {
            return;
        }
        let del_end = line_start.saturating_add(remove);
        let (Some(before_line), Some(removed)) = (
            self.text.get(..line_start),
            self.text.get(line_start..del_end),
        ) else {
            return;
        };
        let char_start = before_line.chars().count();
        let char_count = removed.chars().count();
        self.text.replace_range(line_start..del_end, "");
        if let Some(obj_id) = obj_id {
            let del = isize::try_from(char_count).unwrap_or(isize::MAX);
            let _ = state.splice_text(obj_id, char_start, del, "");
        }
        self.cursor_pos = self.cursor_pos.saturating_sub(remove).max(line_start);
        self.caret_affinity = TextAffinity::Before;
        self.selection_anchor = None;
        self.goal_x = None;
        self.last_edit_time = Some(crate::time::now());
        self.dirty = true;
    }

    /// For `BlockEditor`'s cross-block drag coordinator.
    pub fn is_dragging(&self) -> bool {
        self.is_dragging
    }

    /// For `BlockEditor`'s cross-block drag coordinator, painting a selection
    /// band on a block being dragged through without it being the click
    /// target. `anchor == cursor` clears.
    pub fn set_selection(&mut self, anchor: usize, cursor: usize) {
        let a = anchor.min(self.text.len());
        let c = cursor.min(self.text.len());
        if a == c {
            self.selection_anchor = None;
        } else {
            self.selection_anchor = Some(a);
        }
        self.cursor_pos = c;
        self.caret_affinity = TextAffinity::Before;
    }

    /// Public counterpart to `cursor_from_x`, for `BlockEditor`'s cross-block drag.
    pub fn byte_at_x(&self, logical_x: f64) -> usize {
        self.cursor_from_x(logical_x)
    }

    fn do_copy(&self) {
        if let Some(s) = self.selected_str() {
            clipboard_copy(s);
        }
    }

    fn do_cut(&mut self, state: &mut AutoCommit, obj_id: Option<&ObjId>) {
        if let Some(s) = self.selected_str() {
            let owned = s.to_string();
            // Delete only once the text is on a clipboard; a failed copy
            // must not lose the selection.
            if crate::text_selection::clipboard_copy_accepted(&owned) {
                self.delete_selection(state, obj_id);
            }
        }
    }

    /// Single-line strips all control characters; multiline keeps newlines
    /// (normalising `\r\n`/`\r` to `\n`) and tabs.
    fn do_paste(&mut self, state: &mut AutoCommit, obj_id: Option<&ObjId>) {
        if let Some(paste) = clipboard_paste() {
            let cleaned = self.clean_pasted_text(&paste);
            if !cleaned.is_empty() {
                self.insert_str(&cleaned, state, obj_id);
            }
        }
    }

    fn clean_pasted_text(&self, paste: &str) -> String {
        let cleaned: String = if self.multiline {
            paste
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .chars()
                .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
                .collect()
        } else {
            paste.chars().filter(|c| !c.is_control()).collect()
        };
        match self.char_filter {
            Some(filter) => cleaned.chars().filter(|ch| filter(*ch)).collect(),
            None => cleaned,
        }
    }

    fn do_select_all(&mut self) {
        if !self.text.is_empty() {
            self.selection_anchor = Some(0);
            self.cursor_pos = self.text.len();
            self.caret_affinity = TextAffinity::Before;
        }
    }

    /// For wrappers with "focus selects the value" semantics, e.g. `NumberInput`.
    pub fn select_all(&mut self) {
        self.do_select_all();
    }

    fn field_allows_spelling(&self) -> bool {
        !self.secret
            && !matches!(self.kind, TextInputKind::Password | TextInputKind::Secret)
            && self.spellcheck.unwrap_or_else(|| {
                self.kind.checks_spelling()
                    && !self.mono
                    && self.highlighter.is_none()
                    && self.tab_spaces.is_none()
            })
    }

    fn checks_spelling(&self) -> bool {
        self.field_allows_spelling() && crate::text_assistance::preferences().check_spelling
    }

    fn autocorrect_previous_word(
        &mut self,
        state: &mut AutoCommit,
        obj_id: Option<&ObjId>,
    ) -> Option<Autocorrection> {
        if !self.autocorrect_enabled() || self.has_selection() || self.cursor_pos == 0 {
            return None;
        }
        self.refresh_autocorrection_preview();
        // The visual preview is deliberately debounced (and only painted for a
        // focused field), but committing a boundary must not depend on whether
        // that preview has already had a frame in which to appear. This also
        // keeps fast typists from outrunning an external-language checker.
        let preview = self
            .autocorrection_preview
            .clone()
            .or_else(|| self.autocorrection_candidate())?;
        let (start, end) = (preview.start, preview.end);
        let (original, replacement) = (preview.original, preview.replacement);
        self.selection_anchor = Some(start);
        self.cursor_pos = end;
        self.insert_str(&replacement, state, obj_id);
        let replacement_end = start.saturating_add(replacement.len());
        Some(Autocorrection {
            start,
            end: replacement_end,
            boundary_end: replacement_end,
            original,
            replacement,
        })
    }

    fn word_ending_at_caret(&self) -> Option<(usize, usize)> {
        if self.has_selection() || self.cursor_pos == 0 {
            return None;
        }
        let probe = prev_char_boundary(&self.text, self.cursor_pos);
        let (start, end) = word_boundaries(&self.text, probe);
        (start < end && end == self.cursor_pos).then_some((start, end))
    }

    fn refresh_autocorrection_preview(&mut self) {
        if !self.autocorrect_enabled() || !self.is_focused || !self.preedit.is_empty() {
            self.autocorrection_preview_key = None;
            self.autocorrection_preview = None;
            return;
        }
        if !spellcheck::checks_in_process(&self.spell_language)
            && self
                .last_edit_time
                .is_some_and(|edited| edited.elapsed() < EXTERNAL_SPELLCHECK_DELAY)
        {
            self.autocorrection_preview_key = None;
            self.autocorrection_preview = None;
            return;
        }
        let Some((start, end)) = self.word_ending_at_caret() else {
            self.autocorrection_preview_key = None;
            self.autocorrection_preview = None;
            return;
        };
        let Some(original) = self.text.get(start..end) else {
            self.autocorrection_preview_key = None;
            self.autocorrection_preview = None;
            return;
        };
        let key = (
            self.spell_language.clone(),
            original.to_string(),
            start,
            end,
        );
        if self.autocorrection_preview_key.as_ref() == Some(&key) {
            return;
        }
        let preview = self.autocorrection_candidate();
        self.autocorrection_preview_key = Some(key);
        self.autocorrection_preview = preview;
    }

    fn autocorrection_candidate(&mut self) -> Option<AutocorrectionPreview> {
        let (start, end) = self.word_ending_at_caret()?;
        let original = self.text.get(start..end)?.to_string();
        let dictionary = self
            .spell_dictionary
            .get_or_insert_with(spellcheck::Dictionary::user_dict_overlay);
        if dictionary.contains(&original) {
            return None;
        }
        let replacement = spellcheck::probable_correction(&self.spell_language, &original)?;
        (replacement != original).then(|| AutocorrectionPreview {
            language: self.spell_language.clone(),
            start,
            end,
            original,
            replacement,
        })
    }

    fn insert_word_boundary(
        &mut self,
        boundary: &str,
        state: &mut AutoCommit,
        obj_id: Option<&ObjId>,
    ) {
        let correction = self.autocorrect_previous_word(state, obj_id);
        self.insert_str(boundary, state, obj_id);
        if let Some(mut correction) = correction {
            correction.boundary_end = self.cursor_pos;
            self.last_autocorrection = Some(correction);
        }
    }

    fn undo_last_autocorrection(
        &mut self,
        remove_boundary: bool,
        state: &mut AutoCommit,
        obj_id: Option<&ObjId>,
    ) -> bool {
        let Some(correction) = self.last_autocorrection.clone() else {
            return false;
        };
        if self.text.get(correction.start..correction.end) != Some(&correction.replacement) {
            self.last_autocorrection = None;
            return false;
        }
        let replace_end = if remove_boundary {
            if self.cursor_pos != correction.boundary_end {
                return false;
            }
            let Some(boundary) = self.text.get(correction.end..correction.boundary_end) else {
                return false;
            };
            if boundary.chars().count() > 1 || boundary.chars().any(char::is_alphanumeric) {
                return false;
            }
            correction.boundary_end
        } else {
            correction.end
        };
        self.selection_anchor = Some(correction.start);
        self.cursor_pos = replace_end;
        self.insert_str(&correction.original, state, obj_id);
        true
    }

    fn refresh_spelling(&mut self) {
        if !self.checks_spelling() || self.text.is_empty() {
            self.spell_spans.clear();
            self.spell_cache_key = None;
            return;
        }
        if !self.spell_active {
            self.spell_spans.clear();
            return;
        }
        if !spellcheck::checks_in_process(&self.spell_language)
            && self.is_focused
            && self
                .last_edit_time
                .is_some_and(|edited| edited.elapsed() < EXTERNAL_SPELLCHECK_DELAY)
        {
            return;
        }
        let key = (self.spell_language.clone(), self.text.clone());
        if self.spell_cache_key.as_ref() == Some(&key) {
            return;
        }

        let mut spans = spellcheck::spans_for_language(&self.spell_language, &self.text);
        let dictionary = self
            .spell_dictionary
            .get_or_insert_with(spellcheck::Dictionary::user_dict_overlay);
        spans.retain(|&(start, end)| {
            self.text.get(start..end).is_some_and(|word| {
                !dictionary.contains(word) && !self.ignored_spellings.contains(&word.to_lowercase())
            })
        });
        self.spell_spans = spans;
        self.spell_cache_key = Some(key);
    }

    /// The word currently being formed. Checking is immediate, but its result
    /// is hidden briefly; all completed words remain visible during typing.
    fn actively_edited_word(&self) -> Option<(usize, usize)> {
        if !self.is_focused
            || self.has_selection()
            || self
                .last_edit_time
                .is_none_or(|edited| edited.elapsed() >= SPELL_EDIT_SETTLE)
        {
            return None;
        }

        let is_word_char =
            |character: char| character.is_alphabetic() || matches!(character, '\'' | '\u{2019}');
        let at_caret = self
            .text
            .get(self.cursor_pos..)
            .and_then(|tail| tail.chars().next())
            .filter(|character| is_word_char(*character))
            .map(|_| self.cursor_pos);
        let before_caret = self
            .text
            .get(..self.cursor_pos)
            .and_then(|head| head.char_indices().next_back())
            .filter(|(_, character)| is_word_char(*character))
            .map(|(byte, _)| byte);
        let probe = at_caret.or(before_caret)?;
        let range = word_boundaries(&self.text, probe);
        (range.0 < range.1).then_some(range)
    }

    fn spelling_span_is_visible(&self, start: usize, end: usize) -> bool {
        if self
            .autocorrection_preview
            .as_ref()
            .is_some_and(|preview| end > preview.start && start < preview.end)
        {
            return false;
        }
        self.actively_edited_word()
            .is_none_or(|(active_start, active_end)| end <= active_start || start >= active_end)
    }

    /// Draw a theme-aware wavy underline using the same range geometry as
    /// selection, so it follows shaping, `BiDi`, and soft wrapping.
    #[allow(
        clippy::while_float,
        reason = "x advances by a fixed positive 2px and is clamped to x1"
    )]
    fn draw_spelling(&mut self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        self.refresh_autocorrection_preview();
        self.refresh_spelling();
        let color = theme.tokens.colors.semantic.danger;
        for &(start, end) in &self.spell_spans {
            if !self.spelling_span_is_visible(start, end) {
                continue;
            }
            for rect in self.selection_rects(theme, start, end) {
                let x0 = rect.x0.max(self.rect.x0);
                let x1 = rect.x1.min(self.rect.x1);
                if x1 <= x0 || rect.y1 < self.rect.y0 || rect.y1 > self.rect.y1 {
                    continue;
                }
                let y = rect.y1 - 1.0;
                let mut path = vello::kurbo::BezPath::new();
                path.move_to((x0, y));
                let mut x = x0;
                let mut up = false;
                while x < x1 {
                    x = (x + 2.0).min(x1);
                    path.line_to((x, y + if up { -1.0 } else { 1.0 }));
                    up = !up;
                }
                scene.stroke(&Stroke::new(1.0), transform, color, None, &path);
            }
        }
    }

    /// A quiet solid underline marks the most recent automatic replacement.
    /// It disappears on the next edit, but remains long enough to explain what
    /// changed and to invite Backspace or the context-menu undo action.
    fn draw_autocorrection(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let Some(correction) = self.last_autocorrection.as_ref() else {
            return;
        };
        if self.text.get(correction.start..correction.end) != Some(&correction.replacement) {
            return;
        }
        let (start, end) = (correction.start, correction.end);
        for rect in self.selection_rects(theme, start, end) {
            let line = vello::kurbo::Line::new(
                (rect.x0.max(self.rect.x0), rect.y1 - 0.5),
                (rect.x1.min(self.rect.x1), rect.y1 - 0.5),
            );
            scene.stroke(
                &Stroke::new(1.5),
                transform,
                theme.tokens.colors.semantic.info,
                None,
                &line,
            );
        }
    }

    /// Show the exact high-confidence replacement before Space/punctuation
    /// applies it. The compact chip floats above the field like an IME
    /// candidate, leaving the text and caret unobscured.
    fn draw_autocorrection_preview(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.refresh_autocorrection_preview();
        let Some(preview) = self.autocorrection_preview.as_ref() else {
            return;
        };
        let label = format!("Space → {}", preview.replacement);
        let style = theme.text(TextRole::Caption);
        let (text_width, text_height) = text_renderer.measure_style(&label, None, &style);
        let pad_x = 8.0_f64;
        let pad_y = 4.0_f64;
        let width = f64::from(text_width) + pad_x * 2.0;
        let height = f64::from(text_height) + pad_y * 2.0;
        let caret = self.ime_cursor_rect().unwrap_or(self.rect);
        let max_x = (self.rect.x1 - width).max(self.rect.x0);
        let x = caret.x0.clamp(self.rect.x0, max_x);
        let below = caret.y1 + 4.0;
        let y = if below + height <= self.rect.y1 - 4.0 {
            below
        } else {
            self.rect.y0 - height - 4.0
        };
        let rect = Rect::new(x, y, x + width, y + height);
        let shape = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.detail));
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::surface::edge_on(theme.tokens.colors.surfaces.overlay),
            None,
            &shape,
        );
        text_renderer.draw_style(
            scene,
            &label,
            theme.tokens.colors.text_primary,
            transform.pre_translate(Vec2::new(rect.x0 + pad_x, rect.y0 + pad_y)),
            None,
            &style,
        );
    }

    /// If nothing is selected, the word under the click is selected first
    /// (matches every native editor's right-click-on-a-word behaviour).
    fn open_context_menu(&mut self, pos: [f32; 2]) {
        self.spell_active = true;
        if !self.has_selection() {
            let byte = self.resolve_cursor(Point::new(f64::from(pos[0]), f64::from(pos[1])));
            let (start, end) = word_boundaries(&self.text, byte);
            if start == end {
                self.cursor_pos = byte.min(self.text.len());
            } else {
                self.selection_anchor = Some(start);
                self.cursor_pos = end;
            }
            self.caret_affinity = TextAffinity::Before;
        }

        let has_sel = self.has_selection();
        let can_paste = clipboard_paste().is_some_and(|s| !s.is_empty());

        let mut menu = ContextMenu::new();
        let mut actions: Vec<Option<CtxAction>> = Vec::new();
        self.refresh_spelling();
        if let (Some((start, end)), Some(correction)) =
            (self.selection_range(), self.last_autocorrection.as_ref())
            && start == correction.start
            && end == correction.end
        {
            menu.push(MenuItem::action(
                format!("Undo autocorrection to ‘{}’", correction.original),
                || {},
            ));
            actions.push(Some(CtxAction::UndoAutocorrection));
            menu.push(MenuItem::separator());
            actions.push(None);
        }
        if let Some((start, end)) = self.selection_range()
            && self.spell_spans.contains(&(start, end))
            && let Some(word) = self.text.get(start..end)
        {
            let word = word.to_string();
            for replacement in spellcheck::suggestions_for_language(&self.spell_language, &word, 3)
            {
                menu.push(MenuItem::action(replacement.clone(), || {}));
                actions.push(Some(CtxAction::ApplySpelling {
                    start,
                    end,
                    replacement,
                }));
            }
            menu.push(MenuItem::action(
                format!("Add ‘{word}’ to dictionary"),
                || {},
            ));
            actions.push(Some(CtxAction::AddToDictionary(word.clone())));
            menu.push(MenuItem::action("Ignore in this field", || {}));
            actions.push(Some(CtxAction::IgnoreInField(word)));
            menu.push(MenuItem::separator());
            actions.push(None);
        }
        if has_sel && !self.text_actions.is_empty() {
            for (index, action) in self.text_actions.iter().enumerate() {
                menu.push(MenuItem::action(action.label.clone(), || {}));
                actions.push(Some(CtxAction::TextAction(index)));
            }
            menu.push(MenuItem::separator());
            actions.push(None);
        }
        if has_sel {
            menu.push(MenuItem::action("Cut", || {}));
            actions.push(Some(CtxAction::Cut));
            menu.push(MenuItem::action("Copy", || {}));
            actions.push(Some(CtxAction::Copy));
        }
        if can_paste {
            menu.push(MenuItem::action("Paste", || {}));
            actions.push(Some(CtxAction::Paste));
        }
        if !menu.items().is_empty() {
            menu.push(MenuItem::separator());
            actions.push(None);
        }
        menu.push(MenuItem::action("Select All", || {}));
        actions.push(Some(CtxAction::SelectAll));

        menu.open_at(pos);
        self.context_menu = menu;
        self.context_actions = actions;
    }

    /// Open the native text menu at a logical-pixel position. Hosts use this
    /// for keyboard menu keys, accessibility actions, and deterministic UI
    /// previews; pointer right-clicks route here automatically.
    pub fn open_text_menu_at(&mut self, pos: [f32; 2]) {
        self.open_context_menu(pos);
    }

    /// Out-of-range / separator rows are ignored.
    fn dispatch_context_action(
        &mut self,
        idx: usize,
        state: &mut AutoCommit,
        obj_id: Option<&ObjId>,
    ) -> bool {
        let Some(Some(action)) = self.context_actions.get(idx).cloned() else {
            return false;
        };
        match action {
            CtxAction::Cut => self.do_cut(state, obj_id),
            CtxAction::Copy => self.do_copy(),
            CtxAction::Paste => self.do_paste(state, obj_id),
            CtxAction::SelectAll => self.do_select_all(),
            CtxAction::ApplySpelling {
                start,
                end,
                replacement,
            } => {
                self.selection_anchor = Some(start);
                self.cursor_pos = end;
                self.insert_str(&replacement, state, obj_id);
                self.spell_cache_key = None;
            }
            CtxAction::AddToDictionary(word) => {
                let dictionary = self
                    .spell_dictionary
                    .get_or_insert_with(spellcheck::Dictionary::user_dict_overlay);
                if dictionary.add_word(&word) {
                    let _ = dictionary.save_user_dict();
                }
                self.spell_cache_key = None;
                self.refresh_spelling();
            }
            CtxAction::IgnoreInField(word) => {
                self.ignored_spellings.insert(word.to_lowercase());
                self.spell_cache_key = None;
                self.refresh_spelling();
            }
            CtxAction::UndoAutocorrection => {
                let _ = self.undo_last_autocorrection(false, state, obj_id);
            }
            CtxAction::TextAction(index) => {
                if let (Some(action), Some((start, end))) =
                    (self.text_actions.get(index), self.selection_range())
                    && let Some(selected) = self.text.get(start..end)
                {
                    self.pending_text_action = Some(TextActionRequest {
                        id: action.id.clone(),
                        text: selected.to_string(),
                        range: (start, end),
                    });
                }
            }
        }
        self.context_menu.close();
        true
    }

    #[cfg(test)]
    fn context_menu_labels(&self) -> Vec<String> {
        if !self.context_menu.is_open() {
            return Vec::new();
        }
        self.context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(str::to_string))
            .collect()
    }

    fn cursor_from_x(&self, logical_x: f64) -> usize {
        // Input can arrive before the first draw has populated glyph metrics
        // (headless tests and a newly focused modal both exercise this path).
        if self.char_x_positions.is_empty() {
            return 0;
        }
        let padding_x = 10.0;
        let relative_x = logical_x - (self.rect.x0 + padding_x) + self.scroll_x;
        let mut best_dist = f64::MAX;
        let mut best_idx = 0;
        // Stale if the text changed since the last draw and this fires before
        // the next one — fall back to skipping rather than panicking.
        for (i, _) in grapheme_indices(&self.text) {
            let Some(&x) = self.char_x_positions.get(i) else {
                continue;
            };
            let dist = (relative_x - x).abs();
            if dist < best_dist {
                best_dist = dist;
                best_idx = i;
            }
        }
        let end = self.text.len();
        if let Some(&x) = self.char_x_positions.get(end) {
            let end_dist = (relative_x - x).abs();
            if end_dist < best_dist {
                best_idx = end;
            }
        }
        best_idx
    }

    /// Both require a prior `draw` to have built their metrics.
    fn resolve_position(&self, pt: Point) -> TextPosition {
        if (!self.multiline || self.multiline_shaped)
            && !self.masked()
            && let Some(layout) = &self.single_layout
        {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "pointer coordinates are bounded on-screen positions for vello/layout hit-testing, never near f32's range"
            )]
            let (x, y) = (
                (pt.x - self.single_origin.x + self.scroll_x) as f32,
                (pt.y - self.single_origin.y) as f32,
            );
            self.layout_to_text_position(layout.hit_position(x, y))
        } else if self.multiline {
            TextPosition::new(self.cursor_from_point(pt))
        } else {
            TextPosition::new(self.cursor_from_x(pt.x))
        }
    }

    fn resolve_cursor(&self, pt: Point) -> usize {
        self.resolve_position(pt).byte
    }

    fn clamp_to_text(&mut self) {
        self.cursor_pos = grapheme_boundary_at_or_before(&self.text, self.cursor_pos);
        if let Some(a) = self.selection_anchor {
            self.selection_anchor = Some(grapheme_boundary_at_or_before(&self.text, a));
        }
    }
}

impl Drop for TextInput {
    fn drop(&mut self) {
        if self.secret {
            self.text.zeroize();
            self.preedit.zeroize();
        }
    }
}

impl Widget for TextInput {
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::presence::capability::PublishesPresence,
        dyn crate::undo::UndoPoints
    );

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.interaction = environment;
        self.is_hovered &= environment.supports_hover();
    }

    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = crate::doc::document(state, root);
        let local_value = self
            .local_state
            .as_mut()
            .and_then(crate::local_state::LocalBinding::poll);
        if let Some(value) = local_value {
            self.set_text(&value);
        }
        if self.action.is_none()
            && self.dirty
            && let Some(buffer) = &self.external_buffer
            && let Ok(mut value) = buffer.lock()
        {
            value.zeroize();
            value.push_str(&self.text);
        }
        // Directly-bound Text wins over the string-path mode entirely.
        if self.bound_text.is_some() {
            let _ = self.adopt_bound_text(state);
            return;
        }
        if !self.crdt_bound {
            return;
        }
        match state.get(root, self.on_change_path.as_str()) {
            Ok(Some((val, obj_id))) => {
                self.path_was_present = true;
                match val {
                    Value::Scalar(s) => {
                        if let automerge::ScalarValue::Str(text) = s.as_ref() {
                            self.adopt_model_text(text.to_string());
                        }
                    }
                    Value::Object(ObjType::Text) => {
                        if let Ok(text) = state.text(&obj_id) {
                            self.adopt_model_text(text);
                        }
                    }
                    Value::Object(_) => {}
                }
            }
            Ok(None) if self.path_was_present => {
                self.path_was_present = false;
                self.adopt_model_text(String::new());
                self.cursor_pos = 0;
                self.selection_anchor = None;
                self.goal_x = None;
            }
            Ok(None) | Err(_) => {
                // Keep an app-provided initial value until the binding has
                // existed. An error is likewise not evidence of deletion.
            }
        }
        self.clamp_to_text();
        self.last_notified_text.clone_from(&self.text);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.capture_layout_text_scale(theme.text_scale);
        // Multiline is measured (grows with content, see `measure`). Single-
        // line is the 40 px form-field band — until the scaled line no
        // longer fits it: at 2× text the 16 px face shapes a 40 px line,
        // which left zero slack in a 40 px band, so the band grows to the
        // line plus the same vertical inset the multiline body keeps
        // (`ML_PAD_Y` each side). At 1× a 20 px line leaves the band at 40.
        let height = if self.multiline {
            Dimension::auto()
        } else {
            let line_px = f64::from(self.effective_font_size() * LINE_HEIGHT_RATIO)
                * f64::from(theme.text_scale);
            let band = (line_px + ML_PAD_Y * 2.0).max(40.0);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a control band is a few dozen logical pixels, far inside f32"
            )]
            let band = band as f32;
            Dimension::length(band.max(self.interaction.minimum_target()))
        };
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height,
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.apply_layout_external(Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        ));
    }

    /// Single-line keeps its fixed row height from `build_layout` (ZERO
    /// here). Embedded callers use `apply_layout_external` and never reach this.
    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        self.refresh_layout_epoch(text_renderer);
        // Taffy measures before painting. Retain the renderer scale here too,
        // so a freshly opened editor reserves its full accessibility gutter
        // on the very first layout rather than correcting on frame two.
        self.gutter_text_scale = f64::from(text_renderer.text_scale());
        if !self.multiline {
            if !self.masked() && self.single_attributes.is_some() {
                let scale = self
                    .layout_text_scale
                    .unwrap_or_else(|| text_renderer.text_scale());
                let display = self.layout_display_text();
                self.build_single_line_metrics(
                    text_renderer,
                    &display,
                    self.effective_font_size(),
                    scale,
                );
            }
            self.model_layout_dirty = false;
            return taffy::Size::ZERO;
        }
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => 400.0,
        });
        if !self.masked() {
            let scale = self
                .layout_text_scale
                .unwrap_or_else(|| text_renderer.text_scale());
            let style = self.multiline_style(scale);
            self.update_gutter_metrics(text_renderer, &style, scale);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "layout width originates as f32; nonnegative insets only reduce it before shaping at f32 precision"
            )]
            let available_width = (f64::from(width) - self.ml_left() - ML_PAD_X).max(1.0) as f32;
            let display = self.layout_display_text();
            let signature =
                self.shaped_signature(&display, &style.family, style.size, Some(available_width));
            if self.single_layout.is_none() || self.shaped_layout_signature != signature {
                let browser_emoji = text_renderer.browser_color_emoji_installed();
                let mut layout = RichTextLayout::shape_plain_browser(
                    &mut text_renderer.font_system,
                    &display,
                    &style,
                    Some(available_width),
                    browser_emoji,
                );
                text_renderer.note_browser_rich_layout(&layout);
                if !self.preedit.is_empty() {
                    let start = self.cursor_pos.min(self.text.len());
                    layout.add_underline(start..start.saturating_add(self.preedit.len()));
                }
                self.single_layout = Some(layout);
                self.shaped_layout_signature = signature;
            }
            self.multiline_shaped = true;
            self.lines.clear();
            self.refresh_multiline_origin();
            let height = self.single_layout.as_ref().map_or_else(
                || f64::from(style.line_height_px()),
                |layout| f64::from(layout.height()),
            ) + ML_PAD_Y * 2.0;
            self.model_layout_dirty = false;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "content height is a bounded on-screen size for taffy layout, never near f32's range"
            )]
            return taffy::Size {
                width,
                // Taffy rounds layout boxes to pixels. Never round intrinsic
                // text below its last row: subpixel inner overflow would eat
                // the first wheel event instead of scrolling the outer pane.
                height: height.ceil() as f32,
            };
        }
        let avail_w = (f64::from(width) - self.ml_left() - ML_PAD_X).max(1.0);
        self.build_lines(text_renderer, avail_w);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "content height is a bounded on-screen size for taffy layout, never near f32's range"
        )]
        let height = self.content_height() as f32;
        self.model_layout_dirty = false;
        taffy::Size { width, height }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        self.last_caret_width = theme.caret_width;
        self.gutter_text_scale = f64::from(theme.text_scale);
        let shape = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.control));
        if !self.plain {
            self.draw_field_chrome(scene, transform, theme, &shape);
        }

        if self.multiline {
            if self.masked() {
                self.multiline_shaped = false;
                self.single_layout = None;
                self.draw_multiline_body(scene, transform, text_renderer, theme, &shape);
            } else {
                self.draw_multiline_shaped(scene, transform, text_renderer, theme, &shape);
            }
            self.draw_spelling(scene, transform, theme);
            self.draw_autocorrection(scene, transform, theme);
            self.draw_autocorrection_preview(scene, transform, text_renderer, theme);
            return;
        }
        self.multiline_shaped = false;

        let text_size = self.effective_font_size();
        let line_height = f64::from(text_size * theme.text_scale * LINE_HEIGHT_RATIO);
        let padding_x = if self.plain {
            // The parent already adds whatever gutter the document layout wants.
            0.0
        } else {
            f64::from(theme.padding_standard)
        };
        let text_y = self.rect.y0 + (self.rect.height() - line_height) / 2.0;

        // `char_x_positions` remains as a compatibility/readback surface, but
        // normal text now gets every position from the same shaped paragraph
        // used for paint and hit-testing. Masked fields keep the prefix path
        // because bullet byte offsets intentionally differ from the secret.
        let display = self.layout_display_text();
        let total_w =
            self.build_single_line_metrics(text_renderer, &display, text_size, theme.text_scale);
        self.single_origin = Point::new(self.rect.x0 + padding_x, text_y);

        let cursor_x_offset = self.single_line_cursor_x_offset(total_w);
        self.update_single_line_scroll(cursor_x_offset, padding_x, theme.caret_width);

        scene.push_layer(
            vello::peniko::Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &shape,
        );

        self.draw_single_line_decorations(scene, transform, theme, true);
        self.draw_single_line_selection(scene, transform, theme);

        self.draw_single_line_text(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            &display,
        );

        self.draw_single_line_decorations(scene, transform, theme, false);
        if self.is_focused && !self.effectively_disabled() {
            self.draw_single_line_caret(scene, transform, theme);
        }

        scene.pop_layer();
        self.draw_spelling(scene, transform, theme);
        self.draw_autocorrection(scene, transform, theme);
        self.draw_autocorrection_preview(scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        // Single guard that makes both pointer and keyboard input no-ops when disabled.
        if self.effectively_disabled() {
            return;
        }
        self.clamp_to_text();

        match event {
            // Only when hovered, so it never steals the wheel from a surrounding scroll region.
            WindowEvent::MouseWheel { delta, .. }
                if self.multiline && self.is_hovered && !self.effectively_disabled() =>
            {
                self.handle_mouse_wheel(delta, ctx.handled);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(*position, ctx.scale_factor, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } if self.is_hovered && !self.effectively_disabled() => {
                self.handle_right_click(ctx.focus_target, ctx.node_id, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_click(
                    *element_state,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.focus_target,
                    ctx.node_id,
                    ctx.handled,
                );
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Middle,
                ..
            } if self.is_hovered && !self.effectively_disabled() => {
                self.handle_middle_click(
                    ctx.state,
                    ctx.path,
                    ctx.focus_target,
                    ctx.node_id,
                    ctx.handled,
                );
            }
            WindowEvent::Touch(touch) => {
                self.handle_touch_event(
                    touch,
                    ctx.scale_factor,
                    ctx.focus_target,
                    ctx.node_id,
                    ctx.handled,
                );
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        text,
                        state: element_state,
                        ..
                    },
                ..
            } => {
                self.handle_keyboard_input(
                    logical_key,
                    text.as_deref(),
                    *element_state,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.handled,
                );
            }
            WindowEvent::Ime(winit::event::Ime::Commit(t)) => {
                self.handle_ime_commit(t, ctx.state, ctx.path, ctx.handled);
            }
            WindowEvent::Ime(winit::event::Ime::Preedit(t, cursor)) => {
                self.handle_ime_preedit(t, *cursor, ctx.handled);
            }
            _ => {}
        }
        self.notify_user_change();
    }

    fn name(&self) -> &'static str {
        "TextInput"
    }

    fn test_id(&self) -> Option<&str> {
        if self.id.is_empty() {
            Some("text_input")
        } else {
            Some(self.id.as_str())
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// A single-line field scrolls content horizontally rather than growing,
    /// so it only needs re-measure before its first layout or right after a
    /// user edit — never blocking the scroll fast path while idle.
    fn layout_needs_measure(&self) -> bool {
        self.rect == Rect::ZERO || self.dirty || self.model_layout_dirty
    }

    /// Same inputs as [`Self::layout_needs_measure`]: an input's size follows
    /// its content only through a user edit (`dirty`, drained by the owner's
    /// `take_dirty`) or a model change that reshapes multiline or attributed
    /// text (`model_layout_dirty`, cleared by `measure`). Idle inputs would
    /// otherwise be re-measured on every retained frame.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO || self.dirty || self.model_layout_dirty
    }

    /// A disabled input claims no cursor and defers to the host default.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if !self.effectively_disabled() && self.rect.contains(point) {
            Some(winit::window::CursorIcon::Text)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool {
        !self.effectively_disabled()
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
        if focused {
            self.spell_active = true;
        }
        if !focused {
            self.preedit.clear();
            self.preedit_cursor = None;
        }
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }
    fn wants_tab_input(&self) -> bool {
        self.tab_spaces.is_some()
    }
    fn ime_cursor_rect(&self) -> Option<Rect> {
        if !self.is_focused || self.effectively_disabled() {
            return None;
        }
        if self.multiline && !self.multiline_shaped {
            return Some(self.caret_rect_ml(self.cursor_pos, self.last_caret_width));
        }
        let (x, top, height) = if (!self.multiline || self.multiline_shaped)
            && !self.masked()
            && let Some(layout) = &self.single_layout
        {
            let position = if self.preedit.is_empty() {
                self.text_to_layout_position(self.caret_position())
            } else {
                TextPosition::new(self.preedit_layout_caret())
            };
            let (x, top, height) = layout.caret_geometry_at(position);
            (
                self.single_origin.x + f64::from(x) - self.scroll_x,
                self.single_origin.y + f64::from(top),
                f64::from(height),
            )
        } else {
            let offset = self
                .char_x_positions
                .get(self.cursor_pos)
                .copied()
                .unwrap_or(0.0);
            (
                self.single_origin.x + offset - self.scroll_x,
                self.single_origin.y,
                self.line_height(),
            )
        };
        Some(Rect::new(x, top, x + self.last_caret_width, top + height))
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Some(if self.secret {
            winit::window::ImePurpose::Password
        } else {
            winit::window::ImePurpose::Normal
        })
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // A secret (password) input never leaks its value into the a11y tree —
        // it reports `protected` and a `None` value instead.
        let value = (!self.secret).then(|| self.text.clone());
        let role = if self.multiline {
            Role::MultilineTextInput
        } else {
            Role::TextInput
        };
        let mut node = AccessNode::new(role)
            .with_optional_value(value)
            .with_state(AccessState {
                focusable: self.is_focusable(),
                focused: self.is_focused(),
                disabled: self.effectively_disabled(),
                protected: self.secret,
                multiline: self.multiline,
                ..Default::default()
            });
        if !self.id.is_empty() {
            node = node.with_semantic_key(&self.id);
        }
        if self.action.is_none() && self.on_change.is_none() {
            node = node.with_action_target(self.test_id().unwrap_or("text_input"));
        }
        if !self.secret {
            node = node.with_text_selection(
                self.selection_anchor.unwrap_or(self.cursor_pos),
                self.cursor_pos,
            );
        }
        if !self.accessible_name.is_empty() {
            node = node.with_name(crate::i18n!(&self.accessible_name));
        } else if !self.placeholder.is_empty() {
            // A visible placeholder is already the app author's concise field
            // description. Reuse it as the accessible name by default; an
            // explicit name still wins when the prompt is merely an example.
            node = node.with_name(crate::i18n!(&self.placeholder));
        }
        Some(node)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AppModel,
        root: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        let (state, root) = crate::doc::document(state, root);

        if self.effectively_disabled() {
            return false;
        }
        match action {
            Action::SetValue(value) => {
                let previous_text = self.text.clone();
                let cleaned = if self.multiline {
                    value
                        .replace("\r\n", "\n")
                        .replace('\r', "\n")
                        .chars()
                        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
                        .collect::<String>()
                } else {
                    value.chars().filter(|c| !c.is_control()).collect()
                };
                self.selection_anchor = (!self.text.is_empty()).then_some(0);
                self.cursor_pos = self.text.len();
                let obj = self.resolve_text_obj(state, root);
                // SetValue replaces the complete value, including an empty
                // replacement. insert_str intentionally ignores empty typing.
                if cleaned.is_empty() {
                    self.delete_selection(state, obj.as_ref());
                } else {
                    self.insert_str(&cleaned, state, obj.as_ref());
                }
                if obj.is_none() {
                    self.persist_first_path_edit(&previous_text, state, root);
                }
                self.notify_user_change();
                true
            }
            Action::SetSelection { anchor, focus } => {
                let byte_at = |character_index: usize| {
                    grapheme_indices(&self.text)
                        .nth(character_index)
                        .map_or(self.text.len(), |(byte, _)| byte)
                };
                let anchor = byte_at(*anchor);
                let focus = byte_at(*focus);
                self.selection_anchor = (anchor != focus).then_some(anchor);
                self.cursor_pos = focus;
                self.caret_affinity = TextAffinity::Before;
                self.goal_x = None;
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    /// Keeps receiving cursor events during a drag even if the cursor wanders
    /// into a sibling widget.
    fn wants_pointer_capture(&self) -> bool {
        self.is_dragging
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        if !self.checks_spelling() || !self.spell_active || !self.is_focused {
            return None;
        }
        self.last_edit_time
            .map(|edited| edited + SPELL_EDIT_SETTLE)
            .filter(|deadline| *deadline > crate::time::now())
    }

    fn text_review(&self) -> Option<(usize, String)> {
        // A secret field never surfaces its contents.
        if !self.is_focused || self.secret {
            return None;
        }
        Some((self.cursor_pos, self.text.clone()))
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        if presence.should_cull(self.get_rect()) {
            return;
        }
        self.draw(scene, transform, text_renderer, theme, presence);

        if let Some(key) = crate::presence::capability::presence_key(self) {
            // Peer caret + selection reuse `caret_rect`/`selection_rect`, never a
            // re-derived approximation. `transform` maps the local rect to the scene.

            // Fading "just typed" highlighter under the caret/selection.
            let now = crate::time::now();
            for (user, range, alpha) in presence.typing_marks_for_key(&key, now) {
                let rect = self.selection_rect(theme, range.start, range.end);
                // `Affine * Point` is a geometric transform over f64 coordinates,
                // not integer arithmetic that can overflow/wrap.
                #[allow(
                    clippy::arithmetic_side_effects,
                    reason = "Affine*Point transform over f64 screen coordinates, no overflow/wrap semantics"
                )]
                let (p0, p1) = (
                    transform * vello::kurbo::Point::new(rect.x0, rect.y0),
                    transform * vello::kurbo::Point::new(rect.x1, rect.y1),
                );
                presence.draw_peer_typing(
                    scene,
                    theme,
                    &user,
                    &[(vello::kurbo::Rect::new(p0.x, p0.y, p1.x, p1.y), alpha)],
                );
            }
            if let Some(list) = presence.carets.get(&key) {
                for (user, caret) in list {
                    let name = presence.display_name(user);
                    presence.draw_peer_caret_labeled(
                        scene,
                        theme,
                        text_renderer,
                        transform,
                        user,
                        &name,
                        self.caret_rect(theme, *caret),
                    );
                }
            }

            if let Some(list) = presence.selections.get(&key) {
                for (user, range) in list {
                    for r in self.selection_rects(theme, range.start, range.end) {
                        presence.draw_peer_selection(scene, theme, transform, user, r);
                    }
                }
            }
        }

        if let Some(scope) = crate::permissions::capability::permission_scope(self)
            && !permissions.check(&scope)
        {
            let r = self.rect;
            let overlay = Self::PERMISSION_DENIED_OVERLAY;
            scene.fill(
                vello::peniko::Fill::NonZero,
                Affine::IDENTITY,
                overlay,
                None,
                &r,
            );
        }
    }

    /// Paints the open context menu here so it floats above every sibling
    /// regardless of tree order.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        if self.context_menu.is_open() {
            Widget::draw(
                &mut self.context_menu,
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
        }
    }
}

impl crate::undo::UndoPoints for TextInput {
    fn needs_commit(&self) -> bool {
        if let Some(t) = self.last_edit_time {
            t.elapsed() >= Duration::from_secs(1)
        } else {
            false
        }
    }
    fn on_commit(&mut self) {
        self.last_edit_time = None;
    }
    fn get_state(&self) -> Option<String> {
        if let Some(anchor) = self.selection_anchor {
            let affinity = if self.caret_affinity == TextAffinity::After {
                "a"
            } else {
                "b"
            };
            return Some(format!("{}:{affinity}:{anchor}", self.cursor_pos));
        }
        Some(match self.caret_affinity {
            // Keep the historical state shape for the overwhelmingly common
            // unambiguous/default caret.
            TextAffinity::Before => self.cursor_pos.to_string(),
            TextAffinity::After => format!("{}:a", self.cursor_pos),
        })
    }
    fn set_state(&mut self, state: &str) {
        let mut parts = state.split(':');
        let byte = parts.next().unwrap_or_default();
        let affinity = parts.next().unwrap_or("b");
        let anchor = parts.next().and_then(|anchor| anchor.parse::<usize>().ok());
        if let Ok(pos) = byte.parse::<usize>() {
            self.cursor_pos = pos;
            self.selection_anchor = anchor;
            self.caret_affinity = if affinity == "a" {
                TextAffinity::After
            } else {
                TextAffinity::Before
            };
        }
    }
}

impl crate::presence::capability::PublishesPresence for TextInput {
    /// The widget's path, else the document field it edits (an input bound
    /// with [`TextInput::with_on_change_path`] or
    /// [`TextInput::from_document_string`] has no structural path), so peers
    /// editing the same field share carets.
    fn presence_key(&self) -> Option<PresenceKey> {
        if !self.path.is_empty() {
            return Some(PresenceKey::Widget {
                path: self.path.clone(),
            });
        }
        (!self.on_change_path.is_empty()).then(|| PresenceKey::Widget {
            path: vec![PathSegment::Field(self.on_change_path.clone())],
        })
    }

    /// The caret, or the selection when one is extended, while focused. A
    /// secret field publishes nothing.
    fn presence_payload(&self) -> Option<(PresenceKey, crate::collaboration::PresencePayload)> {
        if !self.is_focused || self.secret {
            return None;
        }
        let key = self.presence_key()?;
        let payload = match self.selection_anchor {
            Some(anchor) if anchor != self.cursor_pos => {
                crate::collaboration::PresencePayload::Selection {
                    start: anchor.min(self.cursor_pos),
                    end: anchor.max(self.cursor_pos),
                }
            }
            _ => crate::collaboration::PresencePayload::Caret {
                pos: self.cursor_pos,
            },
        };
        Some((key, payload))
    }
}

impl crate::widget::controls::ActionControls for TextInput {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        if let Some(action) = &mut self.action {
            let _ = action.install_custody(domain, attachment);
        }
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.action.as_mut().and_then(ActionEmitter::take)
    }
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        let control_id = self.test_id().unwrap_or("text_input");
        if let Some(action) = &self.action {
            vec![action.metadata(control_id)]
        } else if self.crdt_bound || self.bound_text.is_some() {
            vec![ControlBindingMetadata {
                control_id: control_id.to_string(),
                kind: ControlBindingKind::CollaborativeDocument(
                    if self.on_change_path.is_empty() {
                        "<nested-text>".to_string()
                    } else {
                        self.on_change_path.clone()
                    },
                ),
            }]
        } else if self.on_change.is_some() {
            vec![ControlBindingMetadata {
                control_id: control_id.to_string(),
                kind: ControlBindingKind::LegacyCallback,
            }]
        } else {
            vec![ControlBindingMetadata {
                control_id: control_id.to_string(),
                kind: ControlBindingKind::SemanticAction(control_id.to_string()),
            }]
        }
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        if let Some(action) = &mut self.action {
            action.refresh_availability(allowed);
        }
    }
}

impl TextInput {
    fn draw_field_chrome(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        shape: &RoundedRect,
    ) {
        let disabled = self.effectively_disabled();
        let rest = theme.tokens.colors.surfaces.raised;
        let bg_color = if self.is_focused && !disabled {
            theme.tokens.colors.lift_on(rest, 1.0)
        } else {
            rest
        };
        // Disabled is the rest face under one group alpha (HIG §9.5), so rest
        // and disabled never look alike; a "one step darker" fill with the
        // same declared border did, on the dark preset.
        if disabled {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                transform,
                shape,
            );
        }
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            bg_color,
            None,
            shape,
        );
        // Rest edge derived from the field fill; focus keeps `border_focus`.
        let (border_w, border_color) = if self.is_focused && !disabled {
            (2.0, theme.tokens.colors.border_focus)
        } else {
            (1.0, crate::surface::edge_on(rest))
        };
        // Rest edge and focus ring share one half-pixel-inset outline, so
        // the hairline never bleeds onto the ground and the ring sits where
        // the edge was.
        scene.stroke(
            &Stroke::new(border_w),
            transform,
            border_color,
            None,
            &crate::surface::edge_outline(shape.rect(), shape.radii().top_left),
        );
        if disabled {
            scene.pop_layer();
        }
    }

    /// Rebuilds `self.char_x_positions` (and, when unmasked, `self.single_layout`)
    /// for the current single-line text and returns the total shaped width.
    fn build_single_line_metrics(
        &mut self,
        text_renderer: &mut TextRenderer,
        display: &str,
        text_size: f32,
        text_scale: f32,
    ) -> f32 {
        self.refresh_layout_epoch(text_renderer);
        self.char_x_positions.clear();
        self.char_x_positions
            .resize(self.text.len().saturating_add(1), 0.0);
        if self.masked() {
            self.single_layout = None;
            for (byte_idx, _) in grapheme_indices(&self.text) {
                let (w, _) =
                    self.measure_str(text_renderer, &self.display_prefix(byte_idx), text_size);
                if let Some(slot) = self.char_x_positions.get_mut(byte_idx) {
                    *slot = f64::from(w);
                }
            }
            let (width, _) = self.measure_str(text_renderer, display, text_size);
            if let Some(slot) = self.char_x_positions.get_mut(self.text.len()) {
                *slot = f64::from(width);
            }
            width
        } else {
            let family = self.family.clone().unwrap_or(if self.mono {
                FontChoice::Monospace
            } else {
                FontChoice::SansSerif
            });
            let layout_size = text_size * text_scale;
            let mut style = TextStyle::new(layout_size)
                .with_line_height(LINE_HEIGHT_RATIO)
                .with_family(family.clone());
            style.kind = self.kind.text_kind();
            let base = self.shaped_signature(display, &family, layout_size, None);
            let attributes = self.valid_single_attributes();
            let signature = attributes.map_or(base, |a| {
                Self::attributed_signature(base ^ Self::single_attributes_signature(a), &a.spans)
            });
            if self.single_layout.is_none() || self.shaped_layout_signature != signature {
                let browser_emoji = text_renderer.browser_color_emoji_installed();
                let mut layout = if let Some(attributes) = attributes {
                    RichTextLayout::shape_styled_browser(
                        &mut text_renderer.font_system,
                        &self.insert_preedit_span(attributes.spans.clone()),
                        &style,
                        None,
                        TextAlign::Left,
                        self.display_ranges(&attributes.underlines),
                        self.display_ranges(&attributes.strikes),
                        browser_emoji,
                    )
                } else {
                    RichTextLayout::shape_plain_browser(
                        &mut text_renderer.font_system,
                        display,
                        &style,
                        None,
                        browser_emoji,
                    )
                };
                text_renderer.note_browser_rich_layout(&layout);
                if !self.preedit.is_empty() {
                    let start = self.cursor_pos.min(self.text.len());
                    layout.add_underline(start..start.saturating_add(self.preedit.len()));
                }
                self.single_layout = Some(layout);
                self.shaped_layout_signature = signature;
            }
            let Some(layout) = self.single_layout.as_ref() else {
                return 0.0;
            };
            let end_byte = self.text_to_layout_byte(self.text.len());
            let positions: Vec<(usize, f64)> = grapheme_indices(&self.text)
                .map(|(byte_idx, _)| {
                    let x = layout.caret_geometry(self.text_to_layout_byte(byte_idx)).0;
                    (byte_idx, f64::from(x))
                })
                .collect();
            let end_x = f64::from(layout.caret_geometry(end_byte).0);
            let width = layout.width();
            for (byte_idx, x) in positions {
                if let Some(slot) = self.char_x_positions.get_mut(byte_idx) {
                    *slot = x;
                }
            }
            if let Some(slot) = self.char_x_positions.get_mut(self.text.len()) {
                *slot = end_x;
            }
            width
        }
    }

    fn single_line_cursor_x_offset(&self, total_w: f32) -> f64 {
        let cursor_pos = self.cursor_pos.min(self.text.len());
        if !self.preedit.is_empty()
            && !self.masked()
            && let Some(layout) = &self.single_layout
        {
            f64::from(layout.caret_geometry(self.preedit_layout_caret()).0)
        } else if !self.masked()
            && let Some(layout) = &self.single_layout
        {
            f64::from(
                layout
                    .caret_geometry_at(self.text_to_layout_position(self.caret_position()))
                    .0,
            )
        } else if let Some(&x) = self.char_x_positions.get(cursor_pos) {
            x
        } else {
            f64::from(total_w)
        }
    }

    fn update_single_line_scroll(
        &mut self,
        cursor_x_offset: f64,
        padding_x: f64,
        caret_width: f64,
    ) {
        // Include the caret's painted width, especially for padding-free code
        // lines: putting its left edge on the clip boundary hides it entirely.
        let visible_width = (self.rect.width() - padding_x * 2.0 - caret_width * 2.0).max(0.0);
        if self.is_focused {
            if cursor_x_offset - self.scroll_x < 0.0 {
                self.scroll_x = cursor_x_offset;
            } else if cursor_x_offset - self.scroll_x > visible_width {
                self.scroll_x = cursor_x_offset - visible_width;
            }
        } else {
            // Unfocused (e.g. a block-editor cell after load) must not scroll to the caret.
            self.scroll_x = 0.0;
        }
    }

    fn draw_single_line_selection(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let Some((sel_start, sel_end)) = self.selection_range() else {
            return;
        };
        let sel_color = theme.tokens.colors.intent.selection_tint;
        for sel_rect in self.selection_rects(theme, sel_start, sel_end) {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                sel_color,
                None,
                &sel_rect,
            );
        }
    }

    /// Code chips precede selection and glyphs; accent link rules follow
    /// glyphs. All bounds come from the exact layout used for interaction.
    fn draw_single_line_decorations(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        backgrounds: bool,
    ) {
        if self.masked() {
            return;
        }
        let (Some(attributes), Some(layout)) =
            (self.valid_single_attributes(), &self.single_layout)
        else {
            return;
        };
        let ranges = if backgrounds {
            &attributes.chips
        } else {
            &attributes.links
        };
        for (start, end) in self.display_ranges(ranges) {
            for (x0, y0, x1, y1) in layout.selection_rects(start, end) {
                let x = self.single_origin.x - self.scroll_x;
                let y = self.single_origin.y;
                let rect = if backgrounds {
                    Rect::new(
                        x + f64::from(x0) - 2.0,
                        y + f64::from(y0) + 1.0,
                        x + f64::from(x1) + 2.0,
                        y + f64::from(y1) - 1.0,
                    )
                } else {
                    Rect::new(
                        x + f64::from(x0),
                        y + f64::from(y1) - 2.0,
                        x + f64::from(x1),
                        y + f64::from(y1) - 1.0,
                    )
                };
                let color = if backgrounds {
                    theme.tokens.colors.surfaces.raised
                } else {
                    theme.tokens.colors.text_primary
                };
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    color,
                    None,
                    &RoundedRect::from_rect(rect, if backgrounds { 3.0 } else { 0.0 }),
                );
            }
        }
    }

    /// Paint the placeholder and the field's text from `single_origin`, as
    /// laid out by `draw`; `display` is the masked text of a secret field.
    fn draw_single_line_text(&self, ctx: &mut PaintCtx<'_>, display: &str) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let text_size = self.effective_font_size();
        let text_y = self.single_origin.y;
        let text_color = if self.effectively_disabled() {
            theme.text_dim
        } else {
            theme.text_primary
        };
        if self.shows_placeholder() {
            let placeholder = crate::i18n!(&self.placeholder);
            // `Affine * Affine` composes two geometric transforms over f64
            // coordinates; it has no integer overflow/wrap semantics.
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine transform composition over f64 screen coordinates"
            )]
            let placeholder_origin = transform * Affine::translate((self.single_origin.x, text_y));
            text_renderer.draw_text(
                scene,
                &placeholder,
                text_size,
                theme.text_dim,
                placeholder_origin,
                None,
            );
        }
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine transform composition over f64 screen coordinates"
        )]
        let text_origin =
            transform * Affine::translate((self.single_origin.x - self.scroll_x, text_y));
        if !self.masked() {
            if let Some(layout) = &self.single_layout {
                text_renderer.draw_rich(scene, layout, text_origin, text_color);
            }
        } else if let Some(fam) = &self.family {
            text_renderer.draw_style(
                scene,
                display,
                text_color,
                text_origin,
                None,
                &TextStyle::new(text_size).with_family(fam.clone()),
            );
        } else {
            text_renderer.draw_style(
                scene,
                display,
                text_color,
                text_origin,
                None,
                &TextStyle::new(text_size).with_family(if self.mono {
                    FontChoice::Monospace
                } else {
                    FontChoice::SansSerif
                }),
            );
        }
    }

    fn draw_single_line_caret(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if self.caret_paint_suppressed {
            return;
        }
        let cursor_rect = if !self.preedit.is_empty()
            && !self.masked()
            && let Some(layout) = &self.single_layout
        {
            let (x, top, height) = layout.caret_geometry(self.preedit_layout_caret());
            Rect::new(
                self.single_origin.x + f64::from(x) - self.scroll_x,
                self.single_origin.y + f64::from(top),
                self.single_origin.x + f64::from(x) - self.scroll_x + theme.caret_width,
                self.single_origin.y + f64::from(top + height),
            )
        } else {
            self.caret_rect_at(theme, self.caret_position())
        };
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.intent.you,
            None,
            &cursor_rect,
        );
    }

    fn handle_mouse_wheel(&mut self, delta: &winit::event::MouseScrollDelta, handled: &mut bool) {
        let dy = match delta {
            winit::event::MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * self.line_height(),
            winit::event::MouseScrollDelta::PixelDelta(p) => -p.y,
        };
        let before = self.scroll_y;
        self.scroll_by(dy);
        if (self.scroll_y - before).abs() > f64::EPSILON {
            *handled = true;
        }
    }

    fn handle_cursor_moved(
        &mut self,
        position: winit::dpi::PhysicalPosition<f64>,
        scale_factor: f64,
        handled: &mut bool,
    ) {
        let logical_pos = Point::new(position.x / scale_factor, position.y / scale_factor);
        if self.update_pointer_position(logical_pos) {
            *handled = true;
        }

        if !self.is_dragging {
            return;
        }
        let raw_position = self.resolve_position(logical_pos);
        let raw = raw_position.byte;
        let (anchor_lo, anchor_hi) = self.drag_anchor_span;
        // Selection always *contains* the original span clicked.
        let (new_anchor, new_cursor) = match self.drag_unit {
            DragUnit::Char => (anchor_lo, raw),
            DragUnit::Word => {
                let (w_start, w_end) = word_boundaries(&self.text, raw);
                extend_span_drag(anchor_lo, anchor_hi, raw, w_start, w_end)
            }
            DragUnit::Line => {
                let (line_start, line_end) = line_boundaries(&self.text, raw);
                extend_span_drag(anchor_lo, anchor_hi, raw, line_start, line_end)
            }
        };
        if new_anchor != self.cursor_pos
            || self.selection_anchor != Some(new_anchor)
            || new_cursor != self.cursor_pos
        {
            self.selection_anchor = Some(new_anchor);
            self.cursor_pos = new_cursor;
            self.caret_affinity = if self.drag_unit == DragUnit::Char {
                raw_position.affinity
            } else {
                TextAffinity::Before
            };
            *handled = true;
        }
    }

    fn handle_right_click(
        &mut self,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        handled: &mut bool,
    ) {
        if !self.is_focused {
            *focus_target = Some(node_id);
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "pointer coordinates are bounded on-screen positions, never near f32's range"
        )]
        let pos = [self.last_mouse_pos.x as f32, self.last_mouse_pos.y as f32];
        self.open_context_menu(pos);
        *handled = true;
    }

    fn click_place_caret(&mut self, pos: usize, affinity: TextAffinity, modifiers: Modifiers) {
        let anchor = if modifiers.state().shift_key() {
            let anchor = self.selection_anchor.unwrap_or(self.cursor_pos);
            self.selection_anchor = Some(anchor);
            anchor
        } else {
            self.selection_anchor = None;
            pos
        };
        self.cursor_pos = pos;
        self.caret_affinity = affinity;
        self.drag_unit = DragUnit::Char;
        self.drag_anchor_span = (anchor, anchor);
        self.is_dragging = true;
    }

    /// Arms a word-granularity drag so dragging beyond it extends by whole words.
    fn click_select_word(&mut self, pos: usize) {
        let (start, end) = word_boundaries(&self.text, pos);
        self.selection_anchor = Some(start);
        self.cursor_pos = end;
        self.caret_affinity = TextAffinity::Before;
        self.drag_unit = DragUnit::Word;
        self.drag_anchor_span = (start, end);
        self.is_dragging = true;
        self.publish_primary_selection();
    }

    fn click_select_line(&mut self, pos: usize) {
        let (start, end) = line_boundaries(&self.text, pos);
        self.selection_anchor = Some(start);
        self.cursor_pos = end;
        self.caret_affinity = TextAffinity::Before;
        self.drag_unit = DragUnit::Line;
        self.drag_anchor_span = (start, end);
        self.is_dragging = true;
        self.publish_primary_selection();
    }

    fn handle_left_press(
        &mut self,
        modifiers: Modifiers,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        handled: &mut bool,
    ) {
        if !self.is_focused {
            *focus_target = Some(node_id);
        }

        let intent = self
            .drag_select
            .press(self.last_mouse_pos, modifiers.state().shift_key());
        let position = self.resolve_position(self.last_mouse_pos);
        let pos = position.byte;
        match intent.granularity {
            Granularity::Char => self.click_place_caret(pos, position.affinity, modifiers),
            Granularity::Word => self.click_select_word(pos),
            Granularity::Line if self.multiline => self.click_select_line(pos),
            // A single-line field has exactly one line, so this is select-all.
            Granularity::Line => self.click_select_line(pos),
        }
        *handled = true;
    }

    fn handle_left_click(
        &mut self,
        element_state: ElementState,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        handled: &mut bool,
    ) {
        // A left press while the menu is open is routed to it, never to caret placement.
        if self.context_menu.is_open() {
            if element_state == ElementState::Pressed {
                let point = [self.last_mouse_pos.x, self.last_mouse_pos.y];
                if let Some(idx) = self.context_menu.item_at(point) {
                    let previous_text = self.text.clone();
                    let obj_id_opt = self.resolve_text_obj(state, root);
                    self.dispatch_context_action(idx, state, obj_id_opt.as_ref());
                    if obj_id_opt.is_none() {
                        self.persist_first_path_edit(&previous_text, state, root);
                    }
                } else {
                    self.context_menu.close();
                }
                *handled = true;
            }
            return;
        }
        match element_state {
            // The tap the toolkit synthesises when a finger lifts: that
            // finger's own touch already pressed, and a second press would
            // count as a double click.
            ElementState::Pressed if self.active_touch.is_some() => *handled = true,
            ElementState::Pressed if self.is_hovered => {
                self.handle_left_press(modifiers, focus_target, node_id, handled);
            }
            ElementState::Released if self.is_dragging => {
                self.is_dragging = false;
                self.drag_select.release();
                self.publish_primary_selection();
                *handled = true;
            }
            _ => {}
        }
    }

    /// Linux primary selection is intentionally available only for ordinary
    /// text. A password must never leave its protected widget merely because
    /// the user dragged across its bullets; middle click may still paste a
    /// primary selection *into* a secret field, matching GTK password inputs.
    fn publish_primary_selection(&self) {
        if let Some(selected) = self.primary_selection_text() {
            clipboard_copy_primary(selected);
        }
    }

    fn primary_selection_text(&self) -> Option<&str> {
        (!self.secret).then(|| self.selected_str()).flatten()
    }

    fn handle_middle_click(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        handled: &mut bool,
    ) {
        let primary = clipboard_paste_primary();
        self.handle_middle_click_with_primary(
            primary.as_deref(),
            state,
            root,
            focus_target,
            node_id,
            handled,
        );
    }

    fn handle_middle_click_with_primary(
        &mut self,
        primary: Option<&str>,
        state: &mut AutoCommit,
        root: &ObjId,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        handled: &mut bool,
    ) {
        if !self.is_focused {
            *focus_target = Some(node_id);
        }
        let position = self.resolve_position(self.last_mouse_pos);
        self.cursor_pos = position.byte;
        self.caret_affinity = position.affinity;
        self.selection_anchor = None;
        self.goal_x = None;

        let previous_text = self.text.clone();
        let obj_id = self.resolve_text_obj(state, root);
        if let Some(paste) = primary {
            let cleaned = self.clean_pasted_text(paste);
            if !cleaned.is_empty() {
                self.insert_str(&cleaned, state, obj_id.as_ref());
            }
        }
        if obj_id.is_none() {
            self.persist_first_path_edit(&previous_text, state, root);
        }
        *handled = true;
    }

    fn handle_arrow_left(&mut self, shift: bool, ctrl: bool, handled: &mut bool) {
        let new_position =
            if ctrl {
                TextPosition::new(prev_word_boundary(&self.text, self.cursor_pos))
            } else if (!self.multiline || self.multiline_shaped)
                && !self.masked()
                && let Some(layout) = &self.single_layout
            {
                self.layout_to_text_position(layout.visual_motion_position(
                    self.text_to_layout_position(self.caret_position()),
                    false,
                ))
            } else {
                TextPosition::new(prev_char_boundary(&self.text, self.cursor_pos))
            };
        // An unmodified or Ctrl-arrow with a selection collapses to the
        // selection edge rather than moving past it (native editor behaviour).
        if !shift && let Some((s, _)) = self.selection_range() {
            self.cursor_pos = s;
            self.caret_affinity = TextAffinity::Before;
            self.selection_anchor = None;
            *handled = true;
            return;
        }
        self.set_cursor_position(new_position, shift);
        *handled = true;
    }

    fn handle_arrow_right(&mut self, shift: bool, ctrl: bool, handled: &mut bool) {
        let new_position =
            if ctrl {
                TextPosition::new(next_word_boundary(&self.text, self.cursor_pos))
            } else if (!self.multiline || self.multiline_shaped)
                && !self.masked()
                && let Some(layout) = &self.single_layout
            {
                self.layout_to_text_position(layout.visual_motion_position(
                    self.text_to_layout_position(self.caret_position()),
                    true,
                ))
            } else {
                TextPosition::new(next_char_boundary(&self.text, self.cursor_pos))
            };
        if !shift && let Some((_, e)) = self.selection_range() {
            self.cursor_pos = e;
            self.caret_affinity = TextAffinity::Before;
            self.selection_anchor = None;
            *handled = true;
            return;
        }
        self.set_cursor_position(new_position, shift);
        *handled = true;
    }

    fn handle_home(&mut self, shift: bool, ctrl: bool, handled: &mut bool) {
        let target = if ctrl {
            0
        } else if self.multiline_shaped
            && let Some(layout) = &self.single_layout
        {
            let (home, _) = layout.visual_line_edges(self.text_to_layout_byte(self.cursor_pos));
            self.layout_to_text_byte(home)
        } else if self.multiline && !self.lines.is_empty() {
            let (i, _) = self.locate_ml(self.cursor_pos);
            self.lines.get(i).map_or(0, |r| r.start)
        } else {
            0
        };
        self.set_cursor(target, shift);
        *handled = true;
    }

    fn handle_end(&mut self, shift: bool, ctrl: bool, handled: &mut bool) {
        let target = if ctrl {
            self.text.len()
        } else if self.multiline_shaped
            && let Some(layout) = &self.single_layout
        {
            let (_, end) = layout.visual_line_edges(self.text_to_layout_byte(self.cursor_pos));
            self.layout_to_text_byte(end)
        } else if self.multiline && !self.lines.is_empty() {
            let (i, _) = self.locate_ml(self.cursor_pos);
            self.lines.get(i).map_or(self.text.len(), |r| r.end)
        } else {
            self.text.len()
        };
        self.set_cursor(target, shift);
        *handled = true;
    }

    fn handle_enter_key(
        &mut self,
        shift: bool,
        state: &mut AutoCommit,
        obj_id: Option<&ObjId>,
        handled: &mut bool,
    ) {
        let correction = (self.enter_submits || self.multiline)
            .then(|| self.autocorrect_previous_word(state, obj_id))
            .flatten();
        if self.enter_submits && !shift {
            self.submit_requested = true;
            *handled = true;
        } else if self.multiline {
            self.insert_str("\n", state, obj_id);
            *handled = true;
        }
        if let Some(mut correction) = correction {
            correction.boundary_end = self.cursor_pos;
            self.last_autocorrection = Some(correction);
        }
    }

    /// Only reached when `self.tab_spaces.is_some()` — otherwise the host keeps Tab for focus traversal.
    fn handle_tab_key(
        &mut self,
        shift: bool,
        state: &mut AutoCommit,
        obj_id: Option<&ObjId>,
        handled: &mut bool,
    ) {
        let width = self.tab_spaces.unwrap_or(0);
        if shift {
            self.outdent(width, state, obj_id);
        } else if width == 0 {
            self.insert_str("\t", state, obj_id);
        } else {
            let spaces = " ".repeat(width);
            self.insert_str(&spaces, state, obj_id);
        }
        *handled = true;
    }

    fn handle_backspace_key(
        &mut self,
        ctrl: bool,
        state: &mut AutoCommit,
        obj_id: Option<&ObjId>,
        handled: &mut bool,
    ) {
        if !ctrl && self.undo_last_autocorrection(true, state, obj_id) {
            *handled = true;
            return;
        }
        self.last_autocorrection = None;
        if self.has_selection() {
            self.delete_selection(state, obj_id);
        } else if self.cursor_pos > 0 {
            let start = if ctrl {
                prev_word_boundary(&self.text, self.cursor_pos)
            } else {
                prev_char_boundary(&self.text, self.cursor_pos)
            };
            let end = self.cursor_pos;
            if let (Some(before), Some(removed)) =
                (self.text.get(..start), self.text.get(start..end))
            {
                let char_start = before.chars().count();
                let char_count = removed.chars().count();
                self.text.replace_range(start..end, "");
                self.cursor_pos = start;
                self.caret_affinity = TextAffinity::Before;
                self.last_edit_time = Some(crate::time::now());
                if let Some(obj_id) = obj_id {
                    let del = isize::try_from(char_count).unwrap_or(isize::MAX);
                    let _ = state.splice_text(obj_id, char_start, del, "");
                }
                self.dirty = true;
            }
        }
        *handled = true;
    }

    fn handle_delete_key(
        &mut self,
        ctrl: bool,
        state: &mut AutoCommit,
        obj_id: Option<&ObjId>,
        handled: &mut bool,
    ) {
        if self.has_selection() {
            self.delete_selection(state, obj_id);
        } else if self.cursor_pos < self.text.len() {
            let start = self.cursor_pos;
            let end = if ctrl {
                next_word_boundary(&self.text, self.cursor_pos)
            } else {
                next_char_boundary(&self.text, self.cursor_pos)
            };
            if let (Some(before), Some(removed)) =
                (self.text.get(..start), self.text.get(start..end))
            {
                let char_start = before.chars().count();
                let char_count = removed.chars().count();
                self.text.replace_range(start..end, "");
                self.last_edit_time = Some(crate::time::now());
                if let Some(obj_id) = obj_id {
                    let del = isize::try_from(char_count).unwrap_or(isize::MAX);
                    let _ = state.splice_text(obj_id, char_start, del, "");
                }
                self.dirty = true;
            }
        }
        *handled = true;
    }

    fn handle_ctrl_shortcut(
        &mut self,
        c: &str,
        state: &mut AutoCommit,
        obj_id: Option<&ObjId>,
        handled: &mut bool,
    ) {
        let lc = c.to_lowercase();
        match lc.as_str() {
            "a" => {
                self.do_select_all();
                *handled = true;
            }
            "c" => {
                self.do_copy();
                *handled = true;
            }
            "x" => {
                self.do_cut(state, obj_id);
                *handled = true;
            }
            "v" => {
                self.do_paste(state, obj_id);
                *handled = true;
            }
            _ => {}
        }
    }

    fn handle_touch_event(
        &mut self,
        touch: &winit::event::Touch,
        scale_factor: f64,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        handled: &mut bool,
    ) {
        let point = Point::new(
            touch.location.x / scale_factor,
            touch.location.y / scale_factor,
        );
        match touch.phase {
            TouchPhase::Started if self.active_touch.is_none() && self.rect.contains(point) => {
                self.active_touch = Some(touch.id);
                self.last_mouse_pos = point;
                self.is_hovered = true;
                self.handle_left_press(Modifiers::default(), focus_target, node_id, handled);
            }
            TouchPhase::Moved if self.active_touch == Some(touch.id) => {
                self.handle_cursor_moved(touch.location, scale_factor, handled);
                // Movement that leaves the field can retain the same resolved
                // cursor, but the owned gesture is consumed until release.
                *handled = true;
            }
            TouchPhase::Ended if self.active_touch == Some(touch.id) => {
                self.active_touch = None;
                self.is_dragging = false;
                self.is_hovered = self.rect.contains(point);
                *handled = true;
            }
            TouchPhase::Cancelled if self.active_touch == Some(touch.id) => {
                self.active_touch = None;
                self.is_dragging = false;
                *handled = true;
            }
            _ if self.active_touch.is_some() => {
                // A secondary finger cannot leak to a control below while
                // this field owns the selection gesture.
                *handled = true;
            }
            _ => {}
        }
    }

    fn handle_keyboard_input(
        &mut self,
        logical_key: &Key,
        text: Option<&str>,
        element_state: ElementState,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
    ) {
        if element_state != ElementState::Pressed {
            return;
        }
        // Checked even if the field isn't focused, since the menu is the focused surface.
        if self.context_menu.is_open() && matches!(logical_key, Key::Named(NamedKey::Escape)) {
            self.context_menu.close();
            *handled = true;
            return;
        }
        if !self.is_focused {
            return;
        }
        let previous_text = self.text.clone();
        let obj_id_opt: Option<ObjId> = self.resolve_text_obj(state, root);
        let obj_id = obj_id_opt.as_ref();
        let shift = modifiers.state().shift_key();
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();

        match logical_key {
            Key::Named(NamedKey::ArrowLeft) => self.handle_arrow_left(shift, ctrl, handled),
            Key::Named(NamedKey::ArrowRight) => self.handle_arrow_right(shift, ctrl, handled),
            Key::Named(NamedKey::ArrowUp) if self.multiline => {
                self.move_vertical(false, shift);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) if self.multiline => {
                self.move_vertical(true, shift);
                *handled = true;
            }
            Key::Named(NamedKey::Home) => self.handle_home(shift, ctrl, handled),
            Key::Named(NamedKey::End) => self.handle_end(shift, ctrl, handled),
            Key::Named(NamedKey::Enter) => self.handle_enter_key(shift, state, obj_id, handled),
            Key::Named(NamedKey::Tab) if self.tab_spaces.is_some() => {
                self.handle_tab_key(shift, state, obj_id, handled);
            }
            Key::Named(NamedKey::Backspace) => {
                self.handle_backspace_key(ctrl, state, obj_id, handled);
            }
            Key::Named(NamedKey::Delete) => self.handle_delete_key(ctrl, state, obj_id, handled),
            Key::Named(NamedKey::Space) => {
                self.insert_word_boundary(" ", state, obj_id);
                *handled = true;
            }
            Key::Character(c) if ctrl => {
                self.handle_ctrl_shortcut(c.as_str(), state, obj_id, handled);
            }
            _ => {
                if let Some(t) = text
                    && !t.chars().all(char::is_control)
                {
                    if t.chars().next().is_some_and(is_autocorrect_boundary) {
                        self.insert_word_boundary(t, state, obj_id);
                    } else {
                        self.insert_str(t, state, obj_id);
                    }
                    *handled = true;
                }
            }
        }
        if obj_id_opt.is_none() {
            self.persist_first_path_edit(&previous_text, state, root);
        }
    }

    fn handle_ime_commit(
        &mut self,
        t: &str,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
    ) {
        if !self.is_focused {
            return;
        }
        self.preedit.clear();
        self.preedit_cursor = None;
        let previous_text = self.text.clone();
        let obj_id_opt: Option<ObjId> = self.resolve_text_obj(state, root);
        if !t.chars().all(char::is_control) {
            if let Some((boundary_at, boundary)) = t.char_indices().next_back()
                && is_autocorrect_boundary(boundary)
            {
                if let Some(prefix) = t.get(..boundary_at)
                    && !prefix.is_empty()
                {
                    self.insert_str(prefix, state, obj_id_opt.as_ref());
                }
                let mut encoded = [0; 4];
                self.insert_word_boundary(
                    boundary.encode_utf8(&mut encoded),
                    state,
                    obj_id_opt.as_ref(),
                );
            } else {
                self.insert_str(t, state, obj_id_opt.as_ref());
            }
            *handled = true;
        }
        if obj_id_opt.is_none() {
            self.persist_first_path_edit(&previous_text, state, root);
        }
    }

    fn handle_ime_preedit(&mut self, t: &str, cursor: Option<(usize, usize)>, handled: &mut bool) {
        if !self.is_focused {
            return;
        }
        if let Some(filter) = self.char_filter {
            let filtered = t.chars().filter(|ch| filter(*ch)).collect::<String>();
            self.preedit_cursor = if filtered == t { cursor } else { None };
            self.preedit = filtered;
        } else {
            t.clone_into(&mut self.preedit);
            self.preedit_cursor = cursor;
        }
        *handled = true;
    }
}

impl TextInput {
    /// Overlay color used to dim the control when the permission scope
    /// resolves to denied. Concept's every-widget convention is
    /// `bg-black/30` ≈ 0.30 × 255 ≈ 77.
    pub(crate) const PERMISSION_DENIED_OVERLAY: vello::peniko::Color =
        crate::tokens::colors::PERMISSION_DENIED_SCRIM;
}

#[cfg(test)]
#[path = "text_input_tests.rs"]
mod tests;
