//! Single-line (optionally multiline) text input; edits replay through
//! `AutoCommit::splice_text`.
//!
//! Text and caret both centre against the same `line_height = font_size *
//! 1.25` band (see `text_baseline_centred_with_caret`) so glyphs don't drift
//! below the caret.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::color_compat::ColorCompat;
use crate::presence::{PathSegment, PresenceKey};
use crate::richtext::{RichSpan, RichTextLayout, TextAlign};
use crate::text_break::line_break_opportunities;
use crate::text_render::{FontChoice, TextRenderer, TextStyle, TextWeight};
use crate::text_selection::{
    TextAffinity, TextPosition, clipboard_copy, clipboard_paste, extend_span_drag,
    grapheme_boundary_at_or_before, next_char_boundary, next_word_boundary, prev_char_boundary,
    prev_word_boundary, word_boundaries,
};
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::code_editor::{Highlighter, Span, token_color};
use crate::widget::context_menu::{ContextMenu, MenuItem};
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value, transaction::Transactable};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use unicode_segmentation::UnicodeSegmentation;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::BlendMode;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

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
}

impl TextInputKind {
    fn checks_spelling(self) -> bool {
        matches!(self, Self::Prose)
    }
}

fn is_autocorrect_boundary(character: char) -> bool {
    character.is_whitespace() || (character.is_ascii_punctuation() && character != '\'')
}

const FONT_SIZE: f32 = 16.0;
/// Caret centres against this band, not `FONT_SIZE`, to line up with where
/// cosmic-text actually draws the glyphs.
const LINE_HEIGHT_RATIO: f32 = 1.25;
/// 400 ms matches the GTK / Qt default.
const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(400);
const MULTI_CLICK_RADIUS: f64 = 4.0;
/// Spellcheck runs immediately, but the word containing the caret stays
/// visually quiet for this long after an edit so its underline does not
/// flicker while the user is still forming it.
const SPELL_EDIT_SETTLE: Duration = Duration::from_millis(250);
/// External Hunspell sessions are persistent but still use IPC. Coalesce the
/// prefixes of a rapidly typed non-English word while preserving prior spans.
const EXTERNAL_SPELLCHECK_DELAY: Duration = Duration::from_millis(100);
const ML_PAD_X: f64 = 10.0;
const ML_PAD_Y: f64 = 8.0;
/// Approximate glyph advance; used identically for gutter geometry and
/// drawing so both stay aligned despite being approximate.
const GUTTER_DIGIT_RATIO: f64 = 0.62;
const GUTTER_PAD_L: f64 = 8.0;
const GUTTER_PAD_R: f64 = 12.0;

/// Unit the in-flight drag snaps to, set from the click ladder on press.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum DragUnit {
    Char,
    Word,
    All,
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
    drag_unit: DragUnit,
    /// Anchor span snapped to `drag_unit` boundaries; the drag extends from
    /// whichever end is "opposite" the current pointer.
    drag_anchor_span: (usize, usize),
    /// For double/triple-click detection against `MULTI_CLICK_INTERVAL`/`_RADIUS`.
    last_click_time: Option<Instant>,
    last_click_pos: Point,
    click_count: u32,
    /// Cleared on `take_dirty()`/`set_text()` — a programmatic write isn't a user edit.
    dirty: bool,
    /// Gates whether edits splice into `state`; false even with a path string
    /// set if it doesn't resolve to a real CRDT object.
    crdt_bound: bool,
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
    single_origin: Point,
    shaped_layout_signature: u64,
    /// In-progress IME composition. It is inserted only in the display
    /// layout and never exposed through value, clipboard, CRDT, or a11y.
    preedit: String,
    preedit_cursor: Option<(usize, usize)>,
    scroll_x: f64,
    last_edit_time: Option<Instant>,
    font_size_override: Option<f32>,
    /// Renders without the rounded-rect chrome, for block-editor cells that
    /// should read as document copy, not a form field.
    plain: bool,
    /// Direct Automerge `Text` binding, wins over `on_change_path` — how
    /// `BlockEditor` binds text nested inside a `List`, which a flat string
    /// key can't address.
    bound_text: Option<ObjId>,
    /// Stamped on every inserted run so peers can highlight "what they wrote".
    local_author: Option<String>,
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
    gutter: bool,
    highlight_current_line: bool,
    /// `None` renders the whole body in one colour; `Some` colours per [`Span`] via `token_color`.
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

pub(crate) const AUTHOR_MARK: &str = "author";

/// One drawable multiline-body run: text, line-relative x offset, colour.
type Seg = (String, f64, vello::peniko::Color);

/// One visible body row's y offset and its drawable segments, as collected
/// by [`TextInput::collect_ml_body_rows`].
type RowDraws = Vec<(f64, Vec<Seg>)>;

/// One gutter line number's y offset, label and "is a wrapped continuation"
/// flag, as collected by [`TextInput::collect_ml_body_rows`].
type GutterNums = Vec<(f64, String, bool)>;

impl TextInput {
    /// Empty `path` opts the widget out of presence rendering.
    pub fn new(path: Vec<PathSegment>) -> Self {
        let spell_language = spellcheck::default_language();
        spellcheck::warm_up_language(&spell_language);
        Self {
            id: String::new(),
            path,
            rect: Rect::ZERO,
            is_hovered: false,
            is_focused: false,
            text: String::new(),
            char_filter: None,
            cursor_pos: 0,
            caret_affinity: TextAffinity::Before,
            selection_anchor: None,
            is_dragging: false,
            drag_unit: DragUnit::Char,
            drag_anchor_span: (0, 0),
            last_click_time: None,
            last_click_pos: Point::ZERO,
            click_count: 0,
            on_change_path: String::new(),
            last_mouse_pos: Point::ZERO,
            char_x_positions: Vec::new(),
            single_layout: None,
            single_origin: Point::ZERO,
            shaped_layout_signature: u64::MAX,
            preedit: String::new(),
            preedit_cursor: None,
            scroll_x: 0.0,
            last_edit_time: None,
            dirty: false,
            crdt_bound: false,
            external_buffer: None,
            font_size_override: None,
            plain: false,
            bound_text: None,
            local_author: None,
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
            gutter: false,
            highlight_current_line: false,
            highlighter: None,
            tab_spaces: None,
            hl_spans: Vec::new(),
            hl_line_starts: Vec::new(),
            hl_sig: u64::MAX,
            last_scroll_caret: None,
            last_caret_width: 2.0,
        }
    }

    /// `None` disables authorship stamping.
    pub fn set_local_author(&mut self, author: Option<String>) {
        self.local_author = author;
    }

    /// After binding, [`sync`] reads from `obj` regardless of the string-path mode.
    pub fn bind_text(&mut self, obj: ObjId) {
        self.bound_text = Some(obj);
        self.crdt_bound = true;
    }

    /// For `BlockEditor` to match local blocks to doc list elements by object identity.
    pub fn bound_obj(&self) -> Option<&ObjId> {
        self.bound_text.as_ref()
    }

    fn resolve_text_obj(&self, state: &AutoCommit, root: &ObjId) -> Option<ObjId> {
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

    #[must_use]
    pub fn with_font_size(mut self, size: f32) -> Self {
        self.font_size_override = Some(size);
        self
    }

    #[must_use]
    pub fn with_plain_chrome(mut self) -> Self {
        self.plain = true;
        self
    }

    pub fn set_font_size(&mut self, size: f32) {
        self.font_size_override = Some(size);
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
        self.family = family;
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
            span.color.hash(&mut hash);
            span.bold.hash(&mut hash);
            span.italic.hash(&mut hash);
        }
        hash.finish()
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
                    let color = token_color(span.kind, theme);
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

    /// No CRDT integration — for a field with no CRDT path to spend.
    pub fn local() -> Self {
        let mut s = Self::new(Vec::new());
        s.crdt_bound = false;
        s
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
        self
    }

    /// Cursor parks at the end; dirty stays false (programmatic seed).
    #[must_use]
    pub fn with_initial_value(mut self, s: &str) -> Self {
        self.text = s.to_string();
        self.cursor_pos = self.text.len();
        self.caret_affinity = TextAffinity::Before;
        self.dirty = false;
        self
    }

    pub fn set_text(&mut self, s: &str) {
        if self.text == s {
            return;
        }
        self.text = s.to_string();
        self.selection_anchor = None;
        self.clamp_to_text();
        self.dirty = false;
        self.last_edit_time = None;
    }

    pub fn value(&self) -> &str {
        &self.text
    }

    /// Local-mode only. In CRDT-bound mode prefer a real key event instead,
    /// so the splice carries authorship marks.
    pub fn insert_at_cursor(&mut self, s: &str) {
        // `insert_str` only touches the CRDT when given an object id.
        let mut sink = AutoCommit::new();
        self.insert_str(s, &mut sink, None);
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
    }

    /// Seat the caret at an exact shaped position without counting as an edit.
    pub fn set_caret_position(&mut self, position: TextPosition) {
        self.cursor_pos = grapheme_boundary_at_or_before(&self.text, position.byte);
        self.caret_affinity = position.affinity;
        self.selection_anchor = None;
    }

    pub fn selection_range_public(&self) -> Option<(usize, usize)> {
        self.selection_range()
    }

    /// `len()` is `text.len() + 1`; empty until the first draw lands.
    pub fn char_x_positions(&self) -> &[f64] {
        &self.char_x_positions
    }

    pub fn scroll_x(&self) -> f64 {
        self.scroll_x
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
        let view_h = self.rect.height() - ML_PAD_Y * 2.0;
        let max = (self.content_height() - ML_PAD_Y * 2.0 - view_h).max(0.0);
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
        let text_size = self.font_size_override.unwrap_or(FONT_SIZE);
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

    /// Single-line only; multiline callers use [`selection_rects`].
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
        let py = ML_PAD_Y;
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
        self.font_size_override.unwrap_or(FONT_SIZE)
    }

    fn line_height(&self) -> f64 {
        f64::from(self.effective_font_size() * LINE_HEIGHT_RATIO)
    }

    /// Falls back to one row when nothing has been laid out yet.
    pub fn content_height(&self) -> f64 {
        if self.multiline_shaped
            && let Some(layout) = &self.single_layout
        {
            return f64::from(layout.height()) + ML_PAD_Y * 2.0;
        }
        let lh = self.line_height();
        let rows = u32::try_from(self.lines.len().max(1)).unwrap_or(u32::MAX);
        f64::from(rows) * lh + ML_PAD_Y * 2.0
    }

    fn x_in_row(&self, r: &VisualLine, byte: usize) -> f64 {
        let b = grapheme_boundary_at_or_before(&self.text, byte.clamp(r.start, r.end));
        let ci = self
            .text
            .get(r.start..b)
            .map_or(0, |s| s.graphemes(true).count());
        r.char_x
            .get(ci)
            .copied()
            .unwrap_or_else(|| r.char_x.last().copied().unwrap_or(0.0))
    }

    fn byte_of_row_char(&self, r: &VisualLine, ci: usize) -> usize {
        let Some(row_text) = self.text.get(r.start..r.end) else {
            return r.end;
        };
        match row_text.grapheme_indices(true).nth(ci) {
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
        let cy = self.rect.y0 + ML_PAD_Y + y - self.scroll_y;
        Rect::new(cx, cy, cx + width, cy + self.line_height())
    }

    /// Requires a prior `draw` to have built the layout.
    fn cursor_from_point(&self, pt: Point) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        let lh = self.line_height();
        let local_y = pt.y - (self.rect.y0 + ML_PAD_Y) + self.scroll_y;
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

    fn build_lines(&mut self, tr: &mut TextRenderer, avail_w: f64) {
        let size = self.effective_font_size();
        let lh = self.line_height();
        let wrap_w = if avail_w > 1.0 { avail_w } else { f64::MAX };
        let mut lines: Vec<VisualLine> = Vec::new();
        let mut y = 0.0f64;
        let mut para_start = 0usize;
        while let Some(tail) = self.text.get(para_start..) {
            let rel_nl = tail.find('\n');
            let para_end = rel_nl.map_or(self.text.len(), |i| para_start.saturating_add(i));
            self.wrap_paragraph(
                tr,
                size,
                para_start,
                para_end,
                rel_nl.is_some(),
                wrap_w,
                lh,
                &mut y,
                &mut lines,
            );
            match rel_nl {
                Some(i) => para_start = para_start.saturating_add(i).saturating_add(1),
                None => break,
            }
        }
        self.lines = lines;
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
    #[allow(clippy::too_many_arguments)]
    fn wrap_paragraph(
        &self,
        tr: &mut TextRenderer,
        size: f32,
        start: usize,
        end: usize,
        hard_break: bool,
        wrap_w: f64,
        lh: f64,
        y: &mut f64,
        out: &mut Vec<VisualLine>,
    ) {
        let Some(para) = self.text.get(start..end) else {
            return;
        };
        let mut boundaries: Vec<usize> = para.grapheme_indices(true).map(|(i, _)| i).collect();
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
            tr.draw_text_font(
                scene,
                text,
                size,
                color,
                origin,
                None,
                TextWeight::Regular,
                false,
                fam,
            );
        } else {
            tr.draw_text_styled(
                scene,
                text,
                size,
                color,
                origin,
                None,
                TextWeight::Regular,
                false,
                self.mono,
            );
        }
    }

    /// Every unmasked multiline input—including attributed source code—uses
    /// the same shaped paragraph for paint, wrapping, carets, selection,
    /// hit-testing, vertical movement, and IME composition.
    /// Rebuilds `self.single_layout` (and its signature) if the shaped
    /// spans, family or size changed since the last draw. Returns the
    /// `TextStyle` used, so gutter line numbers reuse the exact metrics.
    fn ensure_shaped_layout(
        &mut self,
        tr: &mut TextRenderer,
        theme: &Theme,
        size: f32,
        available_width: f32,
    ) -> TextStyle {
        let family = self.family.clone().unwrap_or(if self.mono {
            FontChoice::Monospace
        } else {
            FontChoice::SansSerif
        });
        let style = TextStyle::new(size)
            .with_line_height(LINE_HEIGHT_RATIO)
            .with_family(family.clone());
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
            self.shaped_signature(&display, &family, size, Some(available_width)),
            &spans,
        );
        if self.single_layout.is_none() || self.shaped_layout_signature != signature {
            let mut layout = RichTextLayout::shape_styled(
                &mut tr.font_system,
                &spans,
                &style,
                Some(available_width),
                TextAlign::Left,
                Vec::new(),
                Vec::new(),
            );
            if !self.preedit.is_empty() {
                let start = self.cursor_pos.min(self.text.len());
                layout.add_underline(start..start.saturating_add(self.preedit.len()));
            }
            self.single_layout = Some(layout);
            self.shaped_layout_signature = signature;
        }
        style
    }

    /// Keeps the caret in view by adjusting `self.scroll_y`. Only re-centres
    /// when the caret moved since the last draw, so a wheel scroll away from
    /// it sticks.
    fn update_ml_scroll(&mut self, caret_top: f32, caret_height: f32, layout_height: f32) {
        let view_height = (self.rect.height() - ML_PAD_Y * 2.0).max(1.0);
        if self.is_focused && self.last_scroll_caret != Some(self.cursor_pos) {
            if f64::from(caret_top) - self.scroll_y < 0.0 {
                self.scroll_y = f64::from(caret_top);
            } else if f64::from(caret_top + caret_height) - self.scroll_y > view_height {
                self.scroll_y = f64::from(caret_top + caret_height) - view_height;
            }
        } else if !self.is_focused {
            self.scroll_y = 0.0;
        }
        self.last_scroll_caret = Some(self.cursor_pos);
        let max_scroll = (f64::from(layout_height) - view_height).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
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
            theme.bg_hover.with_alpha_factor(0.35),
            None,
            &gutter,
        );
        let edge = self.rect.x0 + gutter_width;
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
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
        let band_color = theme.bg_hover.with_alpha_factor(0.6);
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

    #[allow(clippy::too_many_arguments, reason = "internal draw-pass helper")]
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
                    self.rect.x0 + gutter_width - GUTTER_PAD_R - f64::from(width),
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

    fn draw_ml_caret(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        caret_position: TextPosition,
    ) {
        if !self.is_focused || self.disabled {
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
            theme.accent_primary,
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
        let left = self.ml_left();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "widget width is a bounded on-screen size for vello/layout math, never near f32's range"
        )]
        let available_width = (self.rect.width() - left - ML_PAD_X).max(1.0) as f32;
        let style = self.ensure_shaped_layout(tr, theme, size, available_width);

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
        self.single_origin =
            Point::new(self.rect.x0 + left, self.rect.y0 + ML_PAD_Y - self.scroll_y);
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
            let color = theme.accent_primary.with_alpha_factor(0.35);
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
            tr.draw_text(
                scene,
                &self.placeholder,
                size,
                theme.text_dim,
                text_transform,
                Some(available_width),
            );
        } else if let Some(layout) = &self.single_layout {
            let color = if self.disabled {
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
        let view_h = self.rect.height() - ML_PAD_Y * 2.0;
        let caret_moved = self.last_scroll_caret != Some(self.cursor_pos);
        if self.is_focused {
            if caret_moved {
                if caret_top - self.scroll_y < 0.0 {
                    self.scroll_y = caret_top;
                } else if caret_bot - self.scroll_y > view_h {
                    self.scroll_y = caret_bot - view_h;
                }
            }
        } else {
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
    fn ml_body_row_metadata(&self) -> (Vec<usize>, Vec<bool>, usize) {
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
        (line_of_row, is_line_start, caret_line)
    }

    #[allow(clippy::too_many_arguments, reason = "internal draw-pass helper")]
    fn draw_body_current_line_band(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        line_of_row: &[usize],
        caret_line: usize,
        lh: f64,
        gutter_w: f64,
    ) {
        if !(self.highlight_current_line && self.is_focused && !self.shows_placeholder()) {
            return;
        }
        let view_h = self.rect.height() - ML_PAD_Y * 2.0;
        let band_color = theme.bg_hover.with_alpha_factor(0.6);
        for (row, &lor) in self.lines.iter().zip(line_of_row.iter()) {
            if lor != caret_line {
                continue;
            }
            let top = row.y - self.scroll_y;
            if top + lh < 0.0 || top > view_h {
                continue;
            }
            let y0 = self.rect.y0 + ML_PAD_Y + top;
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
    #[allow(clippy::too_many_arguments, reason = "internal draw-pass helper")]
    fn collect_ml_body_rows(
        &self,
        line_of_row: &[usize],
        is_line_start: &[bool],
        caret_line: usize,
        view_h: f64,
        lh: f64,
        text_color: vello::peniko::Color,
        theme: &Theme,
        gutter_w: f64,
    ) -> (RowDraws, GutterNums) {
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

    #[allow(clippy::too_many_arguments, reason = "internal draw-pass helper")]
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
            let x = self.rect.x0 + gutter_w - GUTTER_PAD_R - w;
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine transform composition over f64 screen coordinates"
            )]
            let origin =
                transform * Affine::translate((x, self.rect.y0 + ML_PAD_Y + ry - self.scroll_y));
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
                        self.rect.y0 + ML_PAD_Y + ry - self.scroll_y,
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

        let (line_of_row, is_line_start, caret_line) = self.ml_body_row_metadata();
        self.draw_code_gutter_background(scene, transform, theme, gutter_w);
        self.draw_body_current_line_band(
            scene,
            transform,
            theme,
            &line_of_row,
            caret_line,
            lh,
            gutter_w,
        );

        if let Some((s, e)) = self.selection_range() {
            let sel_color = theme.accent_primary.with_alpha_factor(0.35);
            for r in self.selection_rects(theme, s, e) {
                scene.fill(vello::peniko::Fill::NonZero, transform, sel_color, None, &r);
            }
        }

        if self.shows_placeholder() {
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine transform composition over f64 screen coordinates"
            )]
            let origin = transform
                * Affine::translate((self.rect.x0 + left, self.rect.y0 + ML_PAD_Y - self.scroll_y));
            tr.draw_text(scene, &self.placeholder, size, theme.text_dim, origin, None);
        }

        let text_color = if self.disabled {
            theme.text_dim
        } else {
            theme.text_primary
        };
        let view_h = self.rect.height() - ML_PAD_Y * 2.0;
        let (row_draws, gutter_nums) = self.collect_ml_body_rows(
            &line_of_row,
            &is_line_start,
            caret_line,
            view_h,
            lh,
            text_color,
            theme,
            gutter_w,
        );

        self.draw_body_gutter_numbers(scene, transform, tr, theme, size, gutter_w, &gutter_nums);
        self.draw_body_rows(scene, transform, tr, size, left, &row_draws);

        if self.is_focused && !self.disabled {
            let caret = self.caret_rect_ml(self.cursor_pos, theme.caret_width);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.accent_primary,
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
                        token_color(sp.kind, theme),
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
        let default = if self.disabled {
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
            self.dirty = false;
            Some(self.text.clone())
        } else {
            None
        }
    }

    /// Bypasses `draw_with_presence` — only safe on local-mode `TextInputs`
    /// with no presence path or permission scope.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
    }

    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.id = id.to_string();
        self
    }

    #[must_use]
    pub fn with_on_change_path(mut self, path: &str) -> Self {
        self.on_change_path = path.to_string();
        self.crdt_bound = !path.is_empty();
        self
    }

    /// Mirror edits into caller-owned process-local memory without creating a
    /// CRDT object. Intended for passwords/PINs; use with [`Self::local`].
    #[must_use]
    pub fn with_external_buffer(mut self, buffer: Arc<Mutex<String>>) -> Self {
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

    /// Only meaningful together with [`with_secret`].
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

    /// Pure function of line count + font size, so geometry and painting stay identical.
    fn code_gutter_w(&self) -> f64 {
        if !self.gutter || !self.multiline {
            return 0.0;
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
        let adv = f64::from(self.effective_font_size()) * GUTTER_DIGIT_RATIO;
        GUTTER_PAD_L + f64::from(digits) * adv + GUTTER_PAD_R
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
        let mut sig: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in self.text.as_bytes() {
            sig ^= u64::from(b);
            sig = sig.wrapping_mul(0x100_0000_01b3);
        }
        sig ^= self.text.len() as u64;
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

    /// Read the signal with [`take_submit`].
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
        self.disabled
    }

    #[must_use]
    pub fn with_placeholder(mut self, hint: &str) -> Self {
        self.placeholder = hint.to_string();
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
        self.last_edit_time = Some(Instant::now());
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
            self.caret_affinity = TextAffinity::Before;
            insert_char_at = char_idx;
        }
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
        self.last_edit_time = Some(Instant::now());
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
        self.last_edit_time = Some(Instant::now());
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
            clipboard_copy(&owned);
            self.delete_selection(state, obj_id);
        }
    }

    /// Single-line strips all control characters; multiline keeps newlines
    /// (normalising `\r\n`/`\r` to `\n`) and tabs.
    fn do_paste(&mut self, state: &mut AutoCommit, obj_id: Option<&ObjId>) {
        if let Some(paste) = clipboard_paste() {
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
            if !cleaned.is_empty() {
                self.insert_str(&cleaned, state, obj_id);
            }
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
            && self.kind != TextInputKind::Password
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
                theme.accent_primary,
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
        let size = theme.tokens.typography.scale.sm;
        let (text_width, text_height) = text_renderer.measure(&label, size, None);
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
        let shape = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.sm));
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
            theme.accent_primary,
            None,
            &shape,
        );
        text_renderer.draw_text(
            scene,
            &label,
            size,
            theme.tokens.colors.text_primary,
            transform.pre_translate(Vec2::new(rect.x0 + pad_x, rect.y0 + pad_y)),
            None,
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
        for (i, _) in self.text.grapheme_indices(true) {
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

impl Widget for TextInput {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.dirty
            && let Some(buffer) = &self.external_buffer
            && let Ok(mut value) = buffer.lock()
        {
            value.clear();
            value.push_str(&self.text);
        }
        // Directly-bound Text wins over the string-path mode entirely.
        if let Some(obj_id) = self.bound_text.clone() {
            if let Ok(text) = state.text(&obj_id) {
                self.text = text;
            }
            self.clamp_to_text();
            return;
        }
        if !self.crdt_bound {
            return;
        }
        if let Ok(Some((val, obj_id))) = state.get(root, self.on_change_path.as_str()) {
            match val {
                Value::Scalar(s) => {
                    if let automerge::ScalarValue::Str(text) = s.as_ref() {
                        self.text = text.to_string();
                    }
                }
                Value::Object(ObjType::Text) => {
                    if let Ok(text) = state.text(&obj_id) {
                        self.text = text;
                    }
                }
                Value::Object(_) => {}
            }
        }
        self.clamp_to_text();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Multiline is measured (grows with content, see `measure`); single-line is a fixed 40px row.
        let height = if self.multiline {
            Dimension::auto()
        } else {
            Dimension::length(40.0)
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
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
    }

    /// Single-line keeps its fixed row height from `build_layout` (ZERO
    /// here). Embedded callers use `apply_layout_external` and never reach this.
    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        if !self.multiline {
            return taffy::Size::ZERO;
        }
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => 400.0,
        });
        let avail_w = (f64::from(width) - self.ml_left() - ML_PAD_X).max(1.0);
        self.build_lines(text_renderer, avail_w);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "content height is a bounded on-screen size for taffy layout, never near f32's range"
        )]
        let height = self.content_height() as f32;
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
        let shape = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.md));
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

        let text_size = self.font_size_override.unwrap_or(FONT_SIZE);
        let line_height = f64::from(text_size * LINE_HEIGHT_RATIO);
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
        let total_w = self.build_single_line_metrics(text_renderer, &display, text_size);
        self.single_origin = Point::new(self.rect.x0 + padding_x, text_y);

        let cursor_x_offset = self.single_line_cursor_x_offset(total_w);
        self.update_single_line_scroll(cursor_x_offset, padding_x);

        scene.push_layer(
            vello::peniko::Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &shape,
        );

        self.draw_single_line_selection(scene, transform, theme);

        let text_color = if self.disabled {
            theme.text_dim
        } else {
            theme.text_primary
        };
        self.draw_single_line_text(
            scene,
            transform,
            text_renderer,
            theme,
            &display,
            text_size,
            padding_x,
            text_y,
            text_color,
        );

        if self.is_focused && !self.disabled {
            self.draw_single_line_caret(scene, transform, theme);
        }

        scene.pop_layer();
        self.draw_spelling(scene, transform, theme);
        self.draw_autocorrection(scene, transform, theme);
        self.draw_autocorrection_preview(scene, transform, text_renderer, theme);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Single guard that makes both pointer and keyboard input no-ops when disabled.
        if self.disabled {
            return;
        }
        self.clamp_to_text();

        match event {
            // Only when hovered, so it never steals the wheel from a surrounding scroll region.
            WindowEvent::MouseWheel { delta, .. }
                if self.multiline && self.is_hovered && !self.disabled =>
            {
                self.handle_mouse_wheel(delta, handled);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(*position, scale_factor, handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } if self.is_hovered && !self.disabled => {
                self.handle_right_click(focus_target, node_id, handled);
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_click(
                    *element_state,
                    *modifiers,
                    state,
                    root,
                    focus_target,
                    node_id,
                    handled,
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
                    *modifiers,
                    state,
                    root,
                    handled,
                );
            }
            WindowEvent::Ime(winit::event::Ime::Commit(t)) => {
                self.handle_ime_commit(t, state, root, handled);
            }
            WindowEvent::Ime(winit::event::Ime::Preedit(t, cursor)) => {
                self.handle_ime_preedit(t, *cursor, handled);
            }
            _ => {}
        }
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
        self.rect == Rect::ZERO || self.dirty
    }

    /// A disabled input claims no cursor and defers to the host default.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if !self.disabled && self.rect.contains(point) {
            Some(winit::window::CursorIcon::Text)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool {
        !self.disabled
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
    fn ime_cursor_rect(&self) -> Option<Rect> {
        if !self.is_focused || self.disabled {
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
                disabled: self.disabled,
                protected: self.secret,
                multiline: self.multiline,
                ..Default::default()
            });
        if !self.secret {
            node = node.with_text_selection(
                self.selection_anchor.unwrap_or(self.cursor_pos),
                self.cursor_pos,
            );
        }
        Some(node)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> bool {
        use crate::a11y::Action;

        if self.disabled {
            return false;
        }
        match action {
            Action::SetValue(value) => {
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
                self.insert_str(&cleaned, state, obj.as_ref());
                true
            }
            Action::SetSelection { anchor, focus } => {
                let byte_at = |character_index: usize| {
                    self.text
                        .grapheme_indices(true)
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

    fn needs_animation_frame(&self) -> bool {
        self.checks_spelling()
            && self.spell_active
            && self.is_focused
            && self
                .last_edit_time
                .is_some_and(|edited| edited.elapsed() < SPELL_EDIT_SETTLE)
    }

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
        Some(match self.caret_affinity {
            // Keep the historical state shape for the overwhelmingly common
            // unambiguous/default caret.
            TextAffinity::Before => self.cursor_pos.to_string(),
            TextAffinity::After => format!("{}:a", self.cursor_pos),
        })
    }

    fn set_state(&mut self, state: &str) {
        let (byte, affinity) = state.split_once(':').unwrap_or((state, "b"));
        if let Ok(pos) = byte.parse::<usize>() {
            self.cursor_pos = pos;
            self.caret_affinity = if affinity == "a" {
                TextAffinity::After
            } else {
                TextAffinity::Before
            };
        }
    }

    fn text_review(&self) -> Option<(usize, String)> {
        // A secret field never surfaces its contents.
        if !self.is_focused || self.secret {
            return None;
        }
        Some((self.cursor_pos, self.text.clone()))
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        if self.path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget {
                path: self.path.clone(),
            })
        }
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

        if let Some(key) = self.presence_key() {
            // Peer caret + selection reuse `caret_rect`/`selection_rect`, never a
            // re-derived approximation. `transform` maps the local rect to the scene.

            // Fading "just typed" highlighter under the caret/selection.
            let now = std::time::Instant::now();
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

        if let Some(scope) = self.permission_scope()
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

impl TextInput {
    fn draw_field_chrome(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        shape: &RoundedRect,
    ) {
        let bg_color = if self.disabled {
            // One step darker than the raised form-field fill, to read as "inert".
            theme.tokens.colors.surfaces.base
        } else if self.is_focused {
            theme.bg_active
        } else {
            theme.tokens.colors.surfaces.raised
        };
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            bg_color,
            None,
            shape,
        );
        let border_color = if self.disabled {
            theme.tokens.colors.border_standard
        } else if self.is_focused {
            theme.tokens.colors.border_focus
        } else {
            theme.tokens.colors.border_standard
        };
        scene.stroke(
            &Stroke::new(if self.is_focused && !self.disabled {
                2.0
            } else {
                1.0
            }),
            transform,
            border_color,
            None,
            shape,
        );
    }

    /// Rebuilds `self.char_x_positions` (and, when unmasked, `self.single_layout`)
    /// for the current single-line text and returns the total shaped width.
    fn build_single_line_metrics(
        &mut self,
        text_renderer: &mut TextRenderer,
        display: &str,
        text_size: f32,
    ) -> f32 {
        self.char_x_positions.clear();
        self.char_x_positions
            .resize(self.text.len().saturating_add(1), 0.0);
        if self.masked() {
            self.single_layout = None;
            for (byte_idx, _) in self.text.grapheme_indices(true) {
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
            let style = TextStyle::new(text_size)
                .with_line_height(LINE_HEIGHT_RATIO)
                .with_family(family.clone());
            let signature = self.shaped_signature(display, &family, text_size, None);
            if self.single_layout.is_none() || self.shaped_layout_signature != signature {
                let mut layout = RichTextLayout::shape_plain(
                    &mut text_renderer.font_system,
                    display,
                    &style,
                    None,
                );
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
            let positions: Vec<(usize, f64)> = self
                .text
                .grapheme_indices(true)
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

    fn update_single_line_scroll(&mut self, cursor_x_offset: f64, padding_x: f64) {
        let visible_width = self.rect.width() - padding_x * 2.0;
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
        let sel_color = theme.accent_primary.with_alpha_factor(0.35);
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

    #[allow(clippy::too_many_arguments, reason = "internal draw-pass helper")]
    fn draw_single_line_text(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        display: &str,
        text_size: f32,
        padding_x: f64,
        text_y: f64,
        text_color: vello::peniko::Color,
    ) {
        if self.shows_placeholder() {
            // `Affine * Affine` composes two geometric transforms over f64
            // coordinates; it has no integer overflow/wrap semantics.
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine transform composition over f64 screen coordinates"
            )]
            let placeholder_origin =
                transform * Affine::translate((self.rect.x0 + padding_x, text_y));
            text_renderer.draw_text(
                scene,
                &self.placeholder,
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
            transform * Affine::translate((self.rect.x0 + padding_x - self.scroll_x, text_y));
        if !self.masked() {
            if let Some(layout) = &self.single_layout {
                text_renderer.draw_rich(scene, layout, text_origin, text_color);
            }
        } else if let Some(fam) = &self.family {
            text_renderer.draw_text_font(
                scene,
                display,
                text_size,
                text_color,
                text_origin,
                None,
                TextWeight::Regular,
                false,
                fam,
            );
        } else {
            text_renderer.draw_text_styled(
                scene,
                display,
                text_size,
                text_color,
                text_origin,
                None,
                TextWeight::Regular,
                false,
                self.mono,
            );
        }
    }

    fn draw_single_line_caret(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
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
            theme.accent_primary,
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
        let hover = self.rect.contains(logical_pos);
        if hover != self.is_hovered {
            self.is_hovered = hover;
            *handled = true;
        }
        self.last_mouse_pos = logical_pos;

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
            DragUnit::All => (anchor_lo, anchor_hi),
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
    }

    fn click_select_all_drag(&mut self) {
        self.selection_anchor = Some(0);
        self.cursor_pos = self.text.len();
        self.caret_affinity = TextAffinity::Before;
        self.drag_unit = DragUnit::All;
        self.drag_anchor_span = (0, self.text.len());
        self.is_dragging = true;
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

        // Multi-click ladder: single → double (word) → triple (all).
        let now = Instant::now();
        let within_time = self
            .last_click_time
            .is_some_and(|t| now.duration_since(t) <= MULTI_CLICK_INTERVAL);
        // `Point - Point` is a geometric vector subtraction over
        // f64 coordinates; it has no integer overflow/wrap semantics.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Point subtraction over f64 screen coordinates"
        )]
        let within_radius =
            (self.last_click_pos - self.last_mouse_pos).hypot() <= MULTI_CLICK_RADIUS;
        self.click_count = if within_time && within_radius {
            self.click_count.saturating_add(1)
        } else {
            1
        };
        self.last_click_time = Some(now);
        self.last_click_pos = self.last_mouse_pos;

        let position = self.resolve_position(self.last_mouse_pos);
        let pos = position.byte;
        match self.click_count {
            1 => self.click_place_caret(pos, position.affinity, modifiers),
            2 => self.click_select_word(pos),
            _ => self.click_select_all_drag(),
        }
        *handled = true;
    }

    #[allow(clippy::too_many_arguments, reason = "internal event-dispatch helper")]
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
                    let obj_id_opt = self.resolve_text_obj(state, root);
                    self.dispatch_context_action(idx, state, obj_id_opt.as_ref());
                } else {
                    self.context_menu.close();
                }
                *handled = true;
            }
            return;
        }
        match element_state {
            ElementState::Pressed if self.is_hovered => {
                self.handle_left_press(modifiers, focus_target, node_id, handled);
            }
            ElementState::Released if self.is_dragging => {
                self.is_dragging = false;
                *handled = true;
            }
            _ => {}
        }
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
        // A plain arrow with a selection collapses to the selection edge
        // rather than moving one char past it (native editor behaviour).
        if !shift
            && !ctrl
            && let Some((s, _)) = self.selection_range()
        {
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
        if !shift
            && !ctrl
            && let Some((_, e)) = self.selection_range()
        {
            self.cursor_pos = e;
            self.caret_affinity = TextAffinity::Before;
            self.selection_anchor = None;
            *handled = true;
            return;
        }
        self.set_cursor_position(new_position, shift);
        *handled = true;
    }

    fn handle_home(&mut self, shift: bool, handled: &mut bool) {
        let target = if self.multiline_shaped
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

    fn handle_end(&mut self, shift: bool, handled: &mut bool) {
        let target = if self.multiline_shaped
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
                self.last_edit_time = Some(Instant::now());
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
                self.last_edit_time = Some(Instant::now());
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

    #[allow(clippy::too_many_arguments, reason = "internal event-dispatch helper")]
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
            Key::Named(NamedKey::Home) => self.handle_home(shift, handled),
            Key::Named(NamedKey::End) => self.handle_end(shift, handled),
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
mod tests {
    use super::*;

    #[test]
    fn programmatic_caret_never_splits_a_grapheme() {
        let mut combining = TextInput::local().with_initial_value("a\u{301}b");
        combining.set_cursor_pos(1);
        assert_eq!(combining.cursor_pos(), 0);

        let mut emoji = TextInput::local().with_initial_value("👩‍👩‍👧‍👦!");
        emoji.set_cursor_pos(4);
        assert_eq!(emoji.cursor_pos(), 0);
        emoji.set_cursor_pos("👩‍👩‍👧‍👦".len());
        assert_eq!(emoji.cursor_pos(), "👩‍👩‍👧‍👦".len());
    }

    #[test]
    fn permission_denied_overlay_alpha_matches_other_widgets() {
        let expected = crate::tokens::colors::PERMISSION_DENIED_SCRIM;
        assert_eq!(TextInput::PERMISSION_DENIED_OVERLAY, expected);
    }

    #[test]
    fn text_input_test_id_default_is_text_input() {
        let ti = TextInput::new(Vec::new());
        assert_eq!(Widget::test_id(&ti), Some("text_input"));
    }

    #[test]
    fn ime_purpose_does_not_leak_secret_field_contents() {
        let plain = TextInput::local();
        let secret = TextInput::local().with_secret(true);
        assert_eq!(
            Widget::ime_purpose(&plain),
            Some(winit::window::ImePurpose::Normal)
        );
        assert_eq!(
            Widget::ime_purpose(&secret),
            Some(winit::window::ImePurpose::Password)
        );
    }

    #[test]
    fn cursor_shape_is_text_over_rect() {
        let mut ti = TextInput::local();
        ti.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));
        assert_eq!(
            Widget::cursor_shape(&ti, Point::new(20.0, 20.0)),
            Some(winit::window::CursorIcon::Text),
            "pointer over the editable area requests an I-beam",
        );
        assert_eq!(
            Widget::cursor_shape(&ti, Point::new(500.0, 500.0)),
            None,
            "pointer outside the input defers (no cursor claim)",
        );

        let mut off = TextInput::local().disabled(true);
        off.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));
        assert_eq!(
            Widget::cursor_shape(&off, Point::new(20.0, 20.0)),
            None,
            "disabled input claims no cursor",
        );
    }

    /// The peer caret and the local caret are the SAME geometry — full line
    /// height, advancing via per-char metrics, NOT a fixed 2px×16px bar —
    /// the contract that makes a remote caret pixel-identical, only recoloured.
    #[test]
    fn caret_rect_is_real_caret_geometry() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 24.0));
        let theme = Theme::dark();
        let presence = crate::presence::PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        Widget::draw(
            &mut ti,
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
        );

        let r0 = ti.caret_rect(&theme, 0);
        let r5 = ti.caret_rect(&theme, 5);
        let (_text_y, line_height) = ti.text_band();

        assert!(
            (r0.width() - theme.caret_width).abs() < 0.01,
            "caret uses the configured width"
        );
        assert!(
            (r0.height() - line_height).abs() < 0.01,
            "caret spans the full line band, not a fixed 16px"
        );
        assert!(
            line_height > 16.0,
            "body line height is >16 (so the old fixed 16px was wrong)"
        );
        assert!(
            r5.x0 > r0.x0,
            "caret advances with byte position via real glyph metrics"
        );
    }

    #[test]
    fn single_line_bidi_uses_visual_caret_hit_and_selection_geometry() {
        let text = "abc אבג xyz";
        let mut ti = TextInput::local().with_initial_value(text);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 400.0, 40.0));
        draw_to_populate(&mut ti);
        let theme = Theme::dark();

        // Use two unambiguous interior stops. At the LTR/RTL boundaries a
        // byte position legitimately has two visual affinities.
        let after_alef = "abc א".len();
        let after_bet = "abc אב".len();
        let before = ti.caret_rect(&theme, after_alef);
        let after = ti.caret_rect(&theme, after_bet);
        assert!(
            before.x0 > after.x0,
            "the Hebrew run advances right-to-left: before={before:?}, after={after:?}"
        );

        let hit = ti.byte_at_point(Point::new(before.x0 + 1.0, before.center().y));
        assert_eq!(hit, after_alef);

        let selected_end = "abc א".len();
        let bands = ti.selection_rects(&theme, 0, selected_end);
        assert_eq!(bands.len(), 2, "mixed-direction selection stays disjoint");
    }

    #[test]
    fn single_line_bidi_caret_retains_affinity_through_arrow_motion() {
        let mut ti = TextInput::local().with_initial_value("abc אבג xyz");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 400.0, 40.0));
        draw_to_populate(&mut ti);
        ti.set_focus(true);
        let theme = Theme::dark();
        let y = ti.rect.center().y;
        let mut positions = Vec::new();
        for x in 0..=400 {
            let position = ti.position_at_point(Point::new(f64::from(x), y));
            if positions.last() != Some(&position) {
                positions.push(position);
            }
        }
        let (a, b) = positions
            .iter()
            .find_map(|a| {
                let ax = ti.caret_rect_at(&theme, *a).x0;
                positions
                    .iter()
                    .find(|b| {
                        a.byte == b.byte
                            && a.affinity != b.affinity
                            && (ax - ti.caret_rect_at(&theme, **b).x0).abs() > 1.0
                    })
                    .map(|b| (*a, *b))
            })
            .expect("the mixed-direction boundary has two visual positions");
        let (left, right) = if ti.caret_rect_at(&theme, a).x0 < ti.caret_rect_at(&theme, b).x0 {
            (a, b)
        } else {
            (b, a)
        };

        ti.set_caret_position(left);
        let mut doc = AutoCommit::new();
        for _ in 0..16 {
            if ti.caret_position() == right {
                break;
            }
            drive(
                &mut ti,
                &mut doc,
                &named_key_event(NamedKey::ArrowRight),
                mods(false, false),
            );
        }
        assert_eq!(ti.caret_position(), right);
        assert_eq!(left.byte, right.byte);
        assert_ne!(left.affinity, right.affinity);
    }

    #[test]
    fn caret_affinity_survives_widget_state_round_trip() {
        let mut original = TextInput::local().with_initial_value("abc אבג");
        let position = TextPosition::with_affinity(4, TextAffinity::After);
        original.set_caret_position(position);
        let saved = Widget::get_state(&original).unwrap();

        let mut rebuilt = TextInput::local().with_initial_value("abc אבג");
        Widget::set_state(&mut rebuilt, &saved);
        assert_eq!(rebuilt.caret_position(), position);
        assert_eq!(saved, "4:a");
    }

    #[test]
    fn highlighted_code_uses_shared_bidi_geometry_through_the_gutter() {
        let text = "let אבג = 1;\nprintln!(\"ok\");";
        let mut ti = TextInput::local()
            .multiline(true)
            .with_mono()
            .with_gutter(true)
            .with_current_line_highlight(true)
            .with_initial_value(text);
        ti.set_highlighter(crate::widget::code_editor::Language::Rust.highlighter());
        ti.set_focus(true);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 420.0, 140.0));
        draw_to_populate(&mut ti);

        assert!(ti.multiline_shaped);
        assert!(
            ti.lines.is_empty(),
            "code no longer builds the approximate VisualLine geometry"
        );
        let after_alef = "let א".len();
        let after_bet = "let אב".len();
        let theme = Theme::dark();
        let alef = ti.caret_rect(&theme, after_alef);
        let bet = ti.caret_rect(&theme, after_bet);
        assert!(
            alef.x0 > bet.x0,
            "the highlighted Hebrew identifier advances visually right-to-left"
        );
        assert!(alef.x0 > ti.rect.x0 + ti.gutter_width());
        assert_eq!(
            ti.byte_at_point(Point::new(alef.x0 + 1.0, alef.center().y)),
            after_alef
        );
    }

    #[test]
    fn highlighted_code_shapes_visible_preedit_and_recolors_with_theme() {
        let mut ti = TextInput::local()
            .multiline(true)
            .with_mono()
            .with_gutter(true)
            .with_initial_value("let value = 1;");
        ti.set_highlighter(crate::widget::code_editor::Language::Rust.highlighter());
        ti.set_cursor_pos(4);
        ti.set_focus(true);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 420.0, 120.0));
        let mut doc = AutoCommit::new();
        assert!(drive(
            &mut ti,
            &mut doc,
            &WindowEvent::Ime(winit::event::Ime::Preedit(
                "漢".into(),
                Some(("漢".len(), "漢".len())),
            )),
            mods(false, false),
        ));

        draw_to_populate(&mut ti);
        assert_eq!(ti.value(), "let value = 1;");
        assert_eq!(
            ti.single_layout.as_ref().unwrap().underlines,
            vec![(4, 4 + "漢".len())],
            "preedit is part of the attributed display layout"
        );
        assert!(Widget::ime_cursor_rect(&ti).is_some());

        ti.preedit.clear();
        ti.preedit_cursor = None;
        draw_with_theme(&mut ti, &Theme::dark());
        let dark_signature = ti.shaped_layout_signature;
        draw_with_theme(&mut ti, &Theme::light());
        assert_ne!(
            ti.shaped_layout_signature, dark_signature,
            "theme-dependent syntax colours invalidate the attributed layout"
        );
    }

    #[test]
    fn shaped_paragraph_is_reused_until_its_display_inputs_change() {
        let mut ti = TextInput::local().with_initial_value("office שלום");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 400.0, 40.0));
        draw_to_populate(&mut ti);
        let initial_signature = ti.shaped_layout_signature;
        let initial_layout = std::ptr::from_ref(ti.single_layout.as_ref().unwrap());

        ti.set_cursor_pos(1);
        draw_to_populate(&mut ti);
        assert_eq!(ti.shaped_layout_signature, initial_signature);
        assert_eq!(
            std::ptr::from_ref(ti.single_layout.as_ref().unwrap()),
            initial_layout,
            "caret-only movement reuses the shaped paragraph"
        );

        ti.preedit = "漢".into();
        ti.preedit_cursor = Some(("漢".len(), "漢".len()));
        draw_to_populate(&mut ti);
        assert_ne!(
            ti.shaped_layout_signature, initial_signature,
            "preedit changes the displayed paragraph and invalidates shaping"
        );
    }

    #[test]
    fn rtl_arrow_keys_follow_visual_not_storage_direction() {
        let text = "שלום";
        let mut ti = TextInput::local().with_initial_value(text);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        draw_to_populate(&mut ti);
        ti.set_focus(true);
        ti.set_cursor_pos(0); // visual right edge
        let mut doc = AutoCommit::new();

        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowLeft),
            mods(false, false),
        );
        assert_eq!(
            ti.cursor_pos(),
            text.chars().next().unwrap().len_utf8(),
            "visual Left advances logically through an RTL run"
        );
        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowRight),
            mods(false, false),
        );
        assert_eq!(ti.cursor_pos(), 0, "visual Right returns to the RTL start");

        ti.set_cursor_pos(text.len()); // visual left edge
        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowRight),
            mods(false, false),
        );
        assert!(ti.cursor_pos() < text.len());
    }

    #[test]
    fn ime_preedit_is_shaped_underlined_and_committed_atomically() {
        let mut ti = TextInput::local().with_initial_value("ab");
        ti.set_cursor_pos(1);
        ti.set_focus(true);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        let mut doc = AutoCommit::new();

        assert!(drive(
            &mut ti,
            &mut doc,
            &WindowEvent::Ime(winit::event::Ime::Preedit(
                "漢".into(),
                Some(("漢".len(), "漢".len())),
            )),
            mods(false, false),
        ));
        assert_eq!(ti.value(), "ab", "preedit never mutates the document");

        draw_to_populate(&mut ti);
        let layout = ti.single_layout.as_ref().expect("preedit was shaped");
        assert_eq!(layout.underlines, vec![(1, 1 + "漢".len())]);
        let ime_rect = Widget::ime_cursor_rect(&ti).expect("focused input enables IME");
        let committed_caret = ti.caret_rect(&Theme::dark(), 1);
        assert!(
            ime_rect.x0 > committed_caret.x0,
            "candidate anchor follows the in-preedit caret"
        );

        assert!(drive(
            &mut ti,
            &mut doc,
            &WindowEvent::Ime(winit::event::Ime::Commit("漢".into())),
            mods(false, false),
        ));
        assert_eq!(ti.value(), "a漢b");
        assert!(ti.preedit.is_empty());

        ti.set_focus(false);
        assert!(Widget::ime_cursor_rect(&ti).is_none());
    }

    /// Otherwise caret-following scroll hides the first characters.
    #[test]
    fn unfocused_input_renders_from_start_not_caret() {
        let long = "a very long line that easily exceeds a narrow input width so caret-following scroll would otherwise hide the beginning";
        let mut ti = TextInput::local().with_initial_value(long);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 120.0, 24.0)); // narrow
        ti.set_cursor_pos(long.len()); // caret at the very end
        let theme = Theme::dark();
        let presence = crate::presence::PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        Widget::draw(
            &mut ti,
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
        );
        assert_eq!(
            ti.scroll_x(),
            0.0,
            "unfocused input must not scroll the start off-screen"
        );

        ti.set_focus(true);
        Widget::draw(
            &mut ti,
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
        );
        assert!(
            ti.scroll_x() > 0.0,
            "focused input follows the caret on a long line"
        );
    }

    #[test]
    fn disabled_input_is_not_focusable() {
        let enabled = TextInput::new(Vec::new());
        assert!(Widget::is_focusable(&enabled), "default input is focusable");

        let off = TextInput::new(Vec::new()).disabled(true);
        assert!(
            !Widget::is_focusable(&off),
            "disabled input must not be focusable"
        );

        // The setter path agrees with the builder.
        let mut later = TextInput::new(Vec::new());
        later.set_disabled(true);
        assert!(!Widget::is_focusable(&later));
        assert!(later.is_disabled());
    }

    /// `KeyEvent` has a `pub(crate)` platform field so it can't be
    /// struct-literalled from a test; zero it then overwrite fields.
    fn char_key_event(s: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(winit::keyboard::SmolStr::new(s));
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(winit::keyboard::SmolStr::new(s));
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    #[test]
    fn disabled_input_ignores_keyboard_input() {
        let mut ti = TextInput::local().disabled(true);
        ti.set_focus(true); // even if somehow focused, the input is inert
        ti.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));

        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.leaf_node(Style::default());
        let event = char_key_event("x");
        ti.handle_event(
            &event,
            1.0,
            &Modifiers::default(),
            &mut doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        assert!(!handled, "disabled input must not consume the keypress");
        assert_eq!(ti.value(), "", "disabled input must not insert text");
        assert!(ti.take_dirty().is_none(), "no edit should be recorded");

        // The SAME event DOES edit an enabled input, proving the bail-out suppressed it.
        let mut on = TextInput::local();
        on.set_focus(true);
        on.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));
        let mut handled = false;
        on.handle_event(
            &char_key_event("x"),
            1.0,
            &Modifiers::default(),
            &mut doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        assert!(handled);
        assert_eq!(on.value(), "x");
    }

    #[test]
    fn disabled_input_ignores_pointer_input() {
        use winit::dpi::PhysicalPosition;
        use winit::event::MouseButton;

        let mut ti = TextInput::local().disabled(true);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));
        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.leaf_node(Style::default());

        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(20.0, 20.0),
        };
        ti.handle_event(
            &moved,
            1.0,
            &Modifiers::default(),
            &mut doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        ti.handle_event(
            &press,
            1.0,
            &Modifiers::default(),
            &mut doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );

        assert!(!handled, "disabled input must not consume the click");
        assert!(
            focus_target.is_none(),
            "disabled input must not request focus"
        );
    }

    #[test]
    fn text_input_test_id_can_be_overridden() {
        let ti = TextInput::new(Vec::new()).with_id("search_box");
        assert_eq!(Widget::test_id(&ti), Some("search_box"));
    }

    /// Sends `CursorMoved` first so `last_mouse_pos`/`is_hovered` are set, matching real input order.
    #[cfg(test)]
    fn right_click(ti: &mut TextInput, doc: &mut AutoCommit, x: f64, y: f64) -> bool {
        use winit::dpi::PhysicalPosition;
        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.leaf_node(Style::default());
        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(x, y),
        };
        ti.handle_event(
            &moved,
            1.0,
            &Modifiers::default(),
            doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Right,
        };
        ti.handle_event(
            &press,
            1.0,
            &Modifiers::default(),
            doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        handled
    }

    #[test]
    fn right_click_selects_word_and_opens_menu_with_expected_labels() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        assert!(ti.selection_range().is_none(), "precondition: no selection");
        assert!(
            right_click(&mut ti, &mut doc, 20.0, 20.0),
            "right-click is consumed"
        );

        // No char_x_positions yet → cursor_from_x resolves to byte 0, selecting "hello".
        assert_eq!(ti.selected_str(), Some("hello"), "clicked word is selected");
        assert!(ti.context_menu.is_open(), "menu opened");

        // Empty clipboard in the headless test env omits Paste.
        let labels = ti.context_menu_labels();
        assert!(labels.contains(&"Cut".to_string()), "got {labels:?}");
        assert!(labels.contains(&"Copy".to_string()), "got {labels:?}");
        assert!(labels.contains(&"Select All".to_string()), "got {labels:?}");
    }

    #[test]
    fn spelling_defaults_on_and_can_be_disabled_per_field() {
        let mut prose = TextInput::local()
            .with_spell_language("en_US")
            .with_initial_value("teh");
        prose.set_focus(true);
        prose.refresh_spelling();
        assert!(prose.spellcheck_enabled());
        assert_eq!(prose.spell_spans, vec![(0, 3)]);

        let mut search_or_identifier = TextInput::local()
            .with_kind(TextInputKind::Search)
            .with_spell_language("en_US")
            .with_initial_value("teh");
        search_or_identifier.set_focus(true);
        search_or_identifier.refresh_spelling();
        assert!(search_or_identifier.spell_spans.is_empty());

        for kind in [
            TextInputKind::Name,
            TextInputKind::Search,
            TextInputKind::Username,
            TextInputKind::Email,
            TextInputKind::Url,
            TextInputKind::Path,
            TextInputKind::Code,
            TextInputKind::Number,
            TextInputKind::Password,
        ] {
            let mut structured = TextInput::local()
                .with_kind(kind)
                .with_spell_language("en_US")
                .with_initial_value("teh");
            structured.set_focus(true);
            structured.refresh_spelling();
            assert!(structured.spell_spans.is_empty(), "kind {kind:?}");
        }

        let mut opted_in = TextInput::local()
            .with_kind(TextInputKind::Search)
            .with_spellcheck(true)
            .with_spell_language("en_US")
            .with_initial_value("teh");
        opted_in.set_focus(true);
        opted_in.refresh_spelling();
        assert_eq!(opted_in.spell_spans, vec![(0, 3)]);

        let mut secret = TextInput::local()
            .with_secret(true)
            .with_spell_language("en_US")
            .with_initial_value("teh");
        secret.refresh_spelling();
        assert!(
            secret.spell_spans.is_empty(),
            "secret contents must never reach a checker"
        );
    }

    #[test]
    fn custom_vocabulary_suppresses_a_squiggle() {
        let mut ti = TextInput::local()
            .with_spell_language("en_US")
            .with_initial_value("qwordz");
        ti.set_focus(true);
        ti.spell_dictionary = Some(spellcheck::Dictionary::from_words(["qwordz"]));
        ti.refresh_spelling();
        assert!(ti.spell_spans.is_empty());
    }

    #[test]
    fn spelling_checks_immediately_but_only_hides_the_word_being_typed() {
        let mut ti = TextInput::local()
            .with_spell_language("en_US")
            .with_initial_value("teh wrold");
        ti.set_focus(true);
        ti.cursor_pos = ti.text.len();
        ti.last_edit_time = Some(Instant::now());

        ti.refresh_spelling();
        // Dictionary warm-up can dominate this unit test in an unoptimised
        // build. Model a fresh edit after checking to test presentation timing.
        ti.last_edit_time = Some(Instant::now());

        assert_eq!(ti.spell_spans, vec![(0, 3), (4, 9)]);
        assert!(ti.spelling_span_is_visible(0, 3));
        assert!(!ti.spelling_span_is_visible(4, 9));
        assert!(Widget::needs_animation_frame(&ti));
    }

    #[test]
    fn a_word_becomes_visible_immediately_when_a_boundary_is_typed() {
        let mut ti = TextInput::local()
            .with_spell_language("en_US")
            .with_initial_value("wrold ");
        ti.set_focus(true);
        ti.cursor_pos = ti.text.len();
        ti.last_edit_time = Some(Instant::now());

        ti.refresh_spelling();

        assert_eq!(ti.spell_spans, vec![(0, 5)]);
        assert!(ti.actively_edited_word().is_none());
        assert!(ti.spelling_span_is_visible(0, 5));
    }

    #[test]
    fn active_word_is_revealed_after_the_short_settle_period() {
        let mut ti = TextInput::local()
            .with_spell_language("en_US")
            .with_initial_value("wrold");
        ti.set_focus(true);
        ti.cursor_pos = ti.text.len();
        ti.last_edit_time = Instant::now()
            .checked_sub(SPELL_EDIT_SETTLE)
            .and_then(|t| t.checked_sub(Duration::from_millis(1)));

        ti.refresh_spelling();

        assert_eq!(ti.spell_spans, vec![(0, 5)]);
        assert!(ti.spelling_span_is_visible(0, 5));
        assert!(!Widget::needs_animation_frame(&ti));
    }

    #[test]
    fn external_language_check_coalesces_rapid_prefixes_without_dropping_old_spans() {
        let mut ti = TextInput::local()
            .with_spellcheck(true)
            .with_spell_language("de_DE")
            .with_initial_value("alt neu");
        ti.set_focus(true);
        ti.cursor_pos = ti.text.len();
        ti.spell_spans = vec![(0, 3)];
        ti.spell_cache_key = Some(("de_DE".to_string(), "alt".to_string()));
        ti.last_edit_time = Some(Instant::now());

        ti.refresh_spelling();

        assert_eq!(ti.spell_spans, vec![(0, 3)]);
        assert_eq!(
            ti.spell_cache_key,
            Some(("de_DE".to_string(), "alt".to_string()))
        );
        assert!(Widget::needs_animation_frame(&ti));
    }

    #[test]
    fn field_language_override_is_independent_of_session_default() {
        let english = TextInput::local().with_spell_language("en-US");
        let german = TextInput::local().with_spell_language("de-DE");
        assert_eq!(english.spell_language(), "en-US");
        assert_eq!(german.spell_language(), "de-DE");
    }

    #[test]
    fn obvious_typo_autocorrects_at_boundary_and_backspace_restores_it() {
        let mut ti = TextInput::local()
            .with_spellcheck(true)
            .with_autocorrect(true)
            .with_spell_language("en_US")
            .with_initial_value("teh");
        ti.cursor_pos = ti.text.len();
        let mut doc = AutoCommit::new();
        ti.insert_word_boundary(" ", &mut doc, None);
        assert_eq!(ti.value(), "the ");
        assert!(ti.last_autocorrection.is_some());

        let mut handled = false;
        ti.handle_backspace_key(false, &mut doc, None, &mut handled);
        assert!(handled);
        assert_eq!(ti.value(), "teh");
        assert!(ti.last_autocorrection.is_none());
    }

    #[test]
    fn autocorrection_is_previewed_before_the_boundary_applies_it() {
        let mut ti = TextInput::local()
            .with_autocorrect(true)
            .with_spell_language("en_US")
            .with_initial_value("adress");
        ti.set_focus(true);
        ti.cursor_pos = ti.text.len();

        ti.refresh_autocorrection_preview();
        ti.refresh_spelling();

        let preview = ti
            .autocorrection_preview
            .as_ref()
            .expect("a high-confidence correction is previewed");
        assert_eq!(preview.original, "adress");
        assert_eq!(preview.replacement, "address");
        assert!(!ti.spelling_span_is_visible(0, 6));

        let mut doc = AutoCommit::new();
        ti.insert_word_boundary(" ", &mut doc, None);
        assert_eq!(ti.value(), "address ");
    }

    #[test]
    fn autocorrect_respects_field_opt_out_and_personal_vocabulary() {
        let mut off = TextInput::local()
            .with_autocorrect(false)
            .with_spell_language("en_US")
            .with_initial_value("teh");
        off.cursor_pos = off.text.len();
        let mut doc = AutoCommit::new();
        off.insert_word_boundary(" ", &mut doc, None);
        assert_eq!(off.value(), "teh ");

        let mut known = TextInput::local()
            .with_autocorrect(true)
            .with_spell_language("en_US")
            .with_initial_value("teh");
        known.cursor_pos = known.text.len();
        known.spell_dictionary = Some(spellcheck::Dictionary::from_words(["teh"]));
        known.insert_word_boundary(" ", &mut doc, None);
        assert_eq!(known.value(), "teh ");
    }

    #[test]
    fn untouched_inactive_field_does_no_spellcheck_work() {
        let mut ti = TextInput::local()
            .with_spell_language("en_US")
            .with_initial_value("teh");
        ti.refresh_spelling();
        assert!(ti.spell_spans.is_empty());
        assert!(
            ti.spell_cache_key.is_none(),
            "inactive initial values must not populate the checker cache"
        );
    }

    #[test]
    fn misspelling_context_action_replaces_the_selected_word() {
        let mut ti = TextInput::local()
            .with_spell_language("en_US")
            .with_initial_value("teh");
        ti.selection_anchor = Some(0);
        ti.cursor_pos = 3;
        ti.open_context_menu([10.0, 10.0]);

        let item_idx = ti
            .context_actions
            .iter()
            .position(|action| {
                matches!(
                    action,
                    Some(CtxAction::ApplySpelling { replacement, .. })
                        if replacement == "the"
                )
            })
            .expect("the correction is offered");
        let mut doc = AutoCommit::new();
        assert!(ti.dispatch_context_action(item_idx, &mut doc, None));
        assert_eq!(ti.value(), "the");
    }

    #[test]
    fn selected_text_action_is_reported_without_blocking_the_widget() {
        let mut ti = TextInput::local()
            .with_spellcheck(false)
            .with_text_action("translate", "Translate")
            .with_initial_value("hello world");
        ti.selection_anchor = Some(0);
        ti.cursor_pos = 5;
        ti.open_context_menu([10.0, 10.0]);
        let item_idx = ti
            .context_actions
            .iter()
            .position(|action| matches!(action, Some(CtxAction::TextAction(0))))
            .expect("registered action is in the context menu");

        let mut doc = AutoCommit::new();
        assert!(ti.dispatch_context_action(item_idx, &mut doc, None));
        assert_eq!(
            ti.take_text_action_request(),
            Some(TextActionRequest {
                id: "translate".to_string(),
                text: "hello".to_string(),
                range: (0, 5),
            })
        );
        assert_eq!(ti.value(), "hello world", "dispatch itself is read-only");
    }

    #[test]
    fn context_menu_dispatch_maps_index_to_copy() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        let mut doc = AutoCommit::new();
        // Pre-select "world" so Cut/Copy are present and the selection survives a copy.
        ti.selection_anchor = Some(6);
        ti.cursor_pos = 11;

        ti.open_context_menu([50.0, 18.0]);
        let labels = ti.context_menu_labels();
        let copy_idx = labels
            .iter()
            .position(|l| l == "Copy")
            .expect("Copy row present");
        // Labels skip separators, but Cut/Copy precede any separator so indices align here.
        let item_idx = ti
            .context_menu
            .items()
            .iter()
            .position(|i| i.label() == Some("Copy"))
            .unwrap();
        assert_eq!(
            ti.context_actions[item_idx],
            Some(CtxAction::Copy),
            "Copy row maps to the Copy action (label idx {copy_idx})"
        );

        assert!(
            ti.dispatch_context_action(item_idx, &mut doc, None),
            "dispatch ran"
        );
        assert!(!ti.context_menu.is_open(), "menu closed after dispatch");
        // Copy preserves the selection (it's a read-only action).
        assert_eq!(ti.selected_str(), Some("world"));
    }

    #[test]
    fn context_menu_dispatch_cut_removes_selection() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        let mut doc = AutoCommit::new();
        ti.selection_anchor = Some(0);
        ti.cursor_pos = 5; // "hello"
        ti.open_context_menu([10.0, 18.0]);
        let cut_idx = ti
            .context_menu
            .items()
            .iter()
            .position(|i| i.label() == Some("Cut"))
            .expect("Cut row present");
        assert_eq!(ti.context_actions[cut_idx], Some(CtxAction::Cut));
        ti.dispatch_context_action(cut_idx, &mut doc, None);
        assert_eq!(ti.value(), " world", "cut removed the selected word");
    }

    #[test]
    fn left_click_outside_open_menu_dismisses_it() {
        use winit::dpi::PhysicalPosition;
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        ti.set_focus(true);
        let mut doc = AutoCommit::new();
        ti.open_context_menu([10.0, 18.0]);
        assert!(ti.context_menu.is_open());

        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.leaf_node(Style::default());
        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(280.0, 38.0),
        };
        ti.handle_event(
            &moved,
            1.0,
            &Modifiers::default(),
            &mut doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        ti.handle_event(
            &press,
            1.0,
            &Modifiers::default(),
            &mut doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        assert!(handled, "click consumed while menu open");
        assert!(
            !ti.context_menu.is_open(),
            "menu dismissed by outside click"
        );
    }

    #[test]
    fn placeholder_visible_only_when_empty_and_unfocused() {
        let mut ti = TextInput::local().with_placeholder("Search…");
        assert_eq!(ti.placeholder(), "Search…");
        assert!(
            ti.shows_placeholder(),
            "empty unfocused input shows placeholder"
        );

        ti.set_focus(true);
        assert!(!ti.shows_placeholder(), "focused input hides placeholder");

        ti.set_focus(false);
        ti.set_text("hello");
        assert!(!ti.shows_placeholder(), "non-empty input hides placeholder");

        ti.set_text("");
        assert!(ti.shows_placeholder());

        let plain = TextInput::local();
        assert!(
            !plain.shows_placeholder(),
            "no placeholder set → never shows"
        );
    }

    /// Proves the dim hint actually paints, not just that the flag is set.
    #[test]
    fn placeholder_emits_glyphs_when_shown() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = crate::presence::PresenceCtx::empty();

        let mut shown = TextInput::local().with_placeholder("Type here");
        shown.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));
        let mut scene = Scene::new();
        Widget::draw(
            &mut shown,
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
        );
        assert!(
            !scene.encoding().resources.glyph_runs.is_empty(),
            "placeholder must paint a glyph run when shown",
        );

        let mut hidden = TextInput::local().with_placeholder("Type here");
        hidden.set_focus(true);
        hidden.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));
        let mut scene2 = Scene::new();
        Widget::draw(
            &mut hidden,
            &mut scene2,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
        );
        assert!(
            scene2.encoding().resources.glyph_runs.is_empty(),
            "focused input must not paint the placeholder",
        );
    }

    #[test]
    fn prev_word_boundary_walks_word_starts() {
        let s = "hello world quick ui";
        //       ^0    ^6    ^12   ^18
        assert_eq!(prev_word_boundary(s, s.len()), 18);
        assert_eq!(prev_word_boundary(s, 18), 12);
        assert_eq!(prev_word_boundary(s, 12), 6);
        assert_eq!(prev_word_boundary(s, 6), 0);
        assert_eq!(prev_word_boundary(s, 0), 0);
    }

    /// `next_word_boundary` lands *past* the trailing space, like "Ctrl + Right".
    #[test]
    fn next_word_boundary_walks_word_ends() {
        let s = "hello world quick ui";
        assert_eq!(next_word_boundary(s, 0), 6);
        assert_eq!(next_word_boundary(s, 6), 12);
        assert_eq!(next_word_boundary(s, 12), 18);
        assert_eq!(next_word_boundary(s, 18), s.len());
        assert_eq!(next_word_boundary(s, s.len()), s.len());
    }

    /// At a space immediately after a word, snaps to the word on the left
    /// (matches every native editor's double-click-on-trailing-space).
    #[test]
    fn word_boundaries_snaps_to_word_bounds() {
        let s = "hello world";
        assert_eq!(word_boundaries(s, 2), (0, 5)); // mid-word
        assert_eq!(word_boundaries(s, 0), (0, 5)); // word start
        assert_eq!(word_boundaries(s, 5), (0, 5)); // space → snap left
        assert_eq!(word_boundaries(s, 6), (6, 11)); // start of next word
        assert_eq!(word_boundaries(s, 11), (6, 11)); // end → snap left
    }

    #[test]
    fn delete_selection_removes_range_and_collapses() {
        let mut ti = TextInput::new(Vec::new()).with_on_change_path("text_1");
        ti.text = "hello world".to_string();
        ti.selection_anchor = Some(6);
        ti.cursor_pos = 11;
        let mut doc = AutoCommit::new();
        let obj_id = doc
            .put_object(&automerge::ROOT, "text_1", ObjType::Text)
            .unwrap();
        let _ = doc.splice_text(&obj_id, 0, 0, "hello world");
        ti.delete_selection(&mut doc, Some(&obj_id));
        assert_eq!(ti.text, "hello ");
        assert_eq!(ti.cursor_pos, 6);
        assert!(!ti.has_selection());
    }

    #[test]
    fn insert_at_cursor_splices_and_marks_dirty() {
        let mut ti = TextInput::local().with_initial_value("hi world");
        ti.set_cursor_pos(2); // just after "hi"
        ti.insert_at_cursor(" there");
        assert_eq!(ti.value(), "hi there world");
        assert_eq!(
            ti.cursor_pos(),
            "hi there".len(),
            "caret advances past the insert"
        );
        assert_eq!(
            ti.take_dirty().as_deref(),
            Some("hi there world"),
            "the edit is observable"
        );
    }

    #[test]
    fn character_filter_applies_before_replacing_the_selection() {
        let mut ti = TextInput::local()
            .with_initial_value("A1")
            .with_char_filter(|ch| !ch.is_whitespace());
        ti.select_all();
        ti.insert_at_cursor(" B 2 : D 4 ");
        assert_eq!(ti.value(), "B2:D4");
        assert_eq!(ti.cursor_pos(), "B2:D4".len());
        assert_eq!(ti.take_dirty().as_deref(), Some("B2:D4"));
    }

    /// The path Ctrl+V uses when pasting over selected text.
    #[test]
    fn insert_str_replaces_selection() {
        let mut ti = TextInput::new(Vec::new()).with_on_change_path("text_1");
        ti.text = "hello world".to_string();
        ti.selection_anchor = Some(6);
        ti.cursor_pos = 11;
        let mut doc = AutoCommit::new();
        let obj_id = doc
            .put_object(&automerge::ROOT, "text_1", ObjType::Text)
            .unwrap();
        let _ = doc.splice_text(&obj_id, 0, 0, "hello world");
        ti.insert_str("there", &mut doc, Some(&obj_id));
        assert_eq!(ti.text, "hello there");
        assert_eq!(ti.cursor_pos, "hello there".len());
        assert!(!ti.has_selection());
    }

    /// Proven end-to-end through a real Automerge `Text` object.
    #[test]
    fn secret_input_masks_glyphs_but_keeps_value() {
        let mut ti = TextInput::new(Vec::new())
            .with_on_change_path("password")
            .with_secret(true);

        let mut doc = AutoCommit::new();
        let obj_id = doc
            .put_object(&automerge::ROOT, "password", ObjType::Text)
            .unwrap();
        let _ = doc.splice_text(&obj_id, 0, 0, "hunter2");

        ti.sync(&mut doc, &automerge::ROOT);
        assert_eq!(ti.text, "hunter2", "stored value must be the real secret");

        assert_eq!(ti.display_text(), "•••••••");
        // The masked prefix tracks character count, not bytes.
        assert_eq!(ti.display_prefix(3), "•••");

        let mut plain = TextInput::new(Vec::new());
        plain.text = "hunter2".to_string();
        assert_eq!(plain.display_text(), "hunter2");
        assert_eq!(plain.display_prefix(3), "hun");
    }

    #[test]
    fn local_secret_mirrors_without_entering_the_document() {
        let buffer = Arc::new(Mutex::new(String::new()));
        let mut input = TextInput::local()
            .with_external_buffer(Arc::clone(&buffer))
            .with_secret(true);
        input.text = "correct horse battery staple".into();
        input.dirty = true;
        let mut document = AutoCommit::new();
        input.sync(&mut document, &automerge::ROOT);

        assert_eq!(&*buffer.lock().unwrap(), "correct horse battery staple");
        assert!(document.keys(&automerge::ROOT).next().is_none());
    }

    #[test]
    fn accessibility_reports_text_input_role_and_value() {
        use crate::a11y::{AccessibleTextSelection, Role};
        let mut ti = TextInput::new(Vec::new());
        ti.text = "hello".to_string();
        ti.selection_anchor = Some(1);
        ti.cursor_pos = 4;
        let node = Widget::accessibility(&ti).unwrap();
        assert_eq!(node.role, Role::TextInput);
        assert_eq!(node.value.as_deref(), Some("hello"));
        assert!(node.state.focusable, "enabled input is focusable");
        assert!(!node.state.disabled);
        assert!(!node.state.protected);
        assert_eq!(
            node.text_selection,
            Some(AccessibleTextSelection {
                anchor: 1,
                focus: 4,
            })
        );
    }

    #[test]
    fn accessibility_actions_use_extended_grapheme_indices_and_update_crdt() {
        use crate::a11y::Action;

        let family = "👩‍👩‍👧‍👦";
        let original = format!("a{family}b");
        let mut ti = TextInput::new(Vec::new()).with_on_change_path("body");
        ti.text = original.clone();
        let mut doc = AutoCommit::new();
        let obj = doc
            .put_object(&automerge::ROOT, "body", ObjType::Text)
            .unwrap();
        doc.splice_text(&obj, 0, 0, &original).unwrap();

        assert!(Widget::perform_action(
            &mut ti,
            &Action::SetSelection {
                anchor: 1,
                focus: 2,
            },
            &mut doc,
            &automerge::ROOT,
        ));
        assert_eq!(
            ti.selection_range_public(),
            Some((1, 1 + family.len())),
            "one AccessKit character selects the whole ZWJ family"
        );

        assert!(Widget::perform_action(
            &mut ti,
            &Action::SetValue("new value".into()),
            &mut doc,
            &automerge::ROOT,
        ));
        assert_eq!(ti.value(), "new value");
        assert_eq!(doc.text(&obj).unwrap(), "new value");
    }

    #[test]
    fn accessibility_secret_input_masks_value_and_sets_protected() {
        let mut ti = TextInput::new(Vec::new()).with_secret(true);
        ti.text = "hunter2".to_string();
        let node = Widget::accessibility(&ti).unwrap();
        assert_eq!(
            node.value, None,
            "a secret value must not leak into the a11y tree"
        );
        assert!(node.state.protected, "secret input is marked protected");
        assert_eq!(
            node.text_selection, None,
            "selection offsets must not reveal a secret's length"
        );
    }

    #[test]
    fn accessibility_disabled_input_reports_disabled_and_not_focusable() {
        let ti = TextInput::new(Vec::new()).disabled(true);
        let node = Widget::accessibility(&ti).unwrap();
        assert!(node.state.disabled);
        assert!(
            !node.state.focusable,
            "disabled input is dropped from focus order"
        );
    }

    /// Pins the text top against `line_height`, *not* `FONT_SIZE` itself —
    /// the naive computation shifts glyphs ~2-3 px below the caret in a
    /// 40 px input.
    #[test]
    fn text_baseline_centred_with_caret() {
        let rect_h = 40.0_f32;
        let line_height = FONT_SIZE * LINE_HEIGHT_RATIO;
        let text_y_correct = (rect_h - line_height) / 2.0;
        let text_y_naive = (rect_h - FONT_SIZE) / 2.0;
        assert!(text_y_naive > text_y_correct);
        assert!((text_y_naive - text_y_correct - (line_height - FONT_SIZE) / 2.0).abs() < 0.01);
    }

    // Driven through `handle_event`, never by poking private fields, so a
    // regression in event routing is caught too.
    fn mods(ctrl: bool, shift: bool) -> Modifiers {
        use winit::keyboard::ModifiersState;
        let mut s = ModifiersState::empty();
        if ctrl {
            s |= ModifiersState::CONTROL;
        }
        if shift {
            s |= ModifiersState::SHIFT;
        }
        Modifiers::from(s)
    }

    fn command_mods() -> Modifiers {
        use winit::keyboard::ModifiersState;
        Modifiers::from(ModifiersState::SUPER)
    }

    fn named_key_event(k: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(k);
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
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

    fn mouse_move(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }
    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn left_release() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }

    /// Uses a throwaway taffy node — the input embeds no children.
    fn drive(ti: &mut TextInput, doc: &mut AutoCommit, ev: &WindowEvent, m: Modifiers) -> bool {
        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.leaf_node(Style::default());
        ti.handle_event(
            ev,
            1.0,
            &m,
            doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        handled
    }

    /// Unfocused, so `scroll_x` stays 0 and click math is simple.
    fn draw_to_populate(ti: &mut TextInput) {
        draw_with_theme(ti, &Theme::dark());
    }

    fn draw_with_theme(ti: &mut TextInput, theme: &Theme) {
        let presence = crate::presence::PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        Widget::draw(ti, &mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    }

    /// Mirrors the handler's own `padding_x = 10.0`; assumes `scroll_x == 0`.
    fn x_for_byte(ti: &TextInput, b: usize) -> f64 {
        ti.rect.x0 + 10.0 + ti.char_x_positions()[b]
    }

    #[test]
    fn shift_arrow_extends_selection_by_char() {
        let mut ti = TextInput::local().with_initial_value("abcdef");
        ti.set_focus(true);
        ti.set_cursor_pos(3);
        let mut doc = AutoCommit::new();

        assert!(drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowRight),
            mods(false, true)
        ));
        assert_eq!(
            ti.selection_range(),
            Some((3, 4)),
            "shift+Right extends by one char"
        );
        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowRight),
            mods(false, true),
        );
        assert_eq!(ti.selection_range(), Some((3, 5)));
        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowLeft),
            mods(false, true),
        );
        assert_eq!(
            ti.selection_range(),
            Some((3, 4)),
            "shift+Left shrinks it back"
        );
    }

    #[test]
    fn ctrl_arrow_moves_caret_by_word() {
        let mut ti = TextInput::local().with_initial_value("hello world foo");
        ti.set_focus(true);
        ti.set_cursor_pos(0);
        let mut doc = AutoCommit::new();

        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowRight),
            mods(true, false),
        );
        assert_eq!(
            ti.cursor_pos(),
            6,
            "ctrl+Right jumps to start of the next word"
        );
        assert_eq!(ti.selection_range(), None, "no shift → no selection");
        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowRight),
            mods(true, false),
        );
        assert_eq!(ti.cursor_pos(), 12, "ctrl+Right again → start of 'foo'");
        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowLeft),
            mods(true, false),
        );
        assert_eq!(ti.cursor_pos(), 6, "ctrl+Left walks back a word");
    }

    #[test]
    fn shift_ctrl_arrow_extends_selection_by_word() {
        let mut ti = TextInput::local().with_initial_value("hello world foo");
        ti.set_focus(true);
        ti.set_cursor_pos(0);
        let mut doc = AutoCommit::new();

        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowRight),
            mods(true, true),
        );
        assert_eq!(
            ti.selection_range(),
            Some((0, 6)),
            "shift+ctrl+Right selects the first word span"
        );
        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowRight),
            mods(true, true),
        );
        assert_eq!(
            ti.selection_range(),
            Some((0, 12)),
            "extends by another word"
        );
    }

    #[test]
    fn home_end_move_and_extend() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.set_focus(true);
        ti.set_cursor_pos(5);
        let mut doc = AutoCommit::new();

        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::End),
            mods(false, false),
        );
        assert_eq!(ti.cursor_pos(), 11, "End → end of field");
        assert_eq!(ti.selection_range(), None);
        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::Home),
            mods(false, false),
        );
        assert_eq!(ti.cursor_pos(), 0, "Home → start of field");

        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::End),
            mods(false, true),
        );
        assert_eq!(
            ti.selection_range(),
            Some((0, 11)),
            "shift+End selects to line end"
        );
        ti.set_cursor_pos(11);
        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::Home),
            mods(false, true),
        );
        assert_eq!(
            ti.selection_range(),
            Some((0, 11)),
            "shift+Home selects to line start"
        );
    }

    #[test]
    fn ctrl_a_selects_all_and_ctrl_x_cuts() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        drive(&mut ti, &mut doc, &char_key_event("a"), mods(true, false));
        assert_eq!(
            ti.selection_range(),
            Some((0, 11)),
            "ctrl+A selects everything"
        );

        drive(&mut ti, &mut doc, &char_key_event("x"), mods(true, false));
        assert_eq!(ti.value(), "", "ctrl+X cuts the selection");
        assert_eq!(ti.selection_range(), None, "selection collapsed after cut");
    }

    #[test]
    fn command_a_selects_all_on_platforms_using_super() {
        let mut ti = TextInput::local().with_initial_value("hello 👋");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        drive(&mut ti, &mut doc, &char_key_event("a"), command_mods());
        assert_eq!(ti.selection_range(), Some((0, ti.value().len())));
    }

    #[test]
    fn typing_replaces_selection_via_handle_event() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();
        drive(&mut ti, &mut doc, &char_key_event("a"), mods(true, false)); // select all
        drive(&mut ti, &mut doc, &char_key_event("Z"), mods(false, false)); // type over it
        assert_eq!(ti.value(), "Z", "typing over a full selection replaces it");
    }

    #[test]
    fn double_click_selects_word_via_handle_event() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        draw_to_populate(&mut ti);
        let mut doc = AutoCommit::new();

        let bx = x_for_byte(&ti, 8); // inside "world" (bytes 6..11)
        drive(&mut ti, &mut doc, &mouse_move(bx, 20.0), mods(false, false));
        drive(&mut ti, &mut doc, &left_press(), mods(false, false));
        drive(&mut ti, &mut doc, &left_release(), mods(false, false));
        // Within the multi-click interval → double click.
        drive(&mut ti, &mut doc, &left_press(), mods(false, false));

        assert_eq!(
            ti.selection_range(),
            Some((6, 11)),
            "double-click selected the word"
        );
        assert_eq!(ti.selected_str(), Some("world"));
    }

    #[test]
    fn drag_selects_range_via_handle_event() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        draw_to_populate(&mut ti);
        let mut doc = AutoCommit::new();

        let x_start = x_for_byte(&ti, 2);
        let x_end = x_for_byte(&ti, 9);
        drive(
            &mut ti,
            &mut doc,
            &mouse_move(x_start, 20.0),
            mods(false, false),
        );
        drive(&mut ti, &mut doc, &left_press(), mods(false, false));
        assert!(ti.is_dragging(), "press starts a drag");
        drive(
            &mut ti,
            &mut doc,
            &mouse_move(x_end, 20.0),
            mods(false, false),
        );
        assert_eq!(
            ti.selection_range(),
            Some((2, 9)),
            "drag selected the swept range"
        );
        drive(&mut ti, &mut doc, &left_release(), mods(false, false));
        assert!(!ti.is_dragging(), "release ends the drag");
    }

    #[test]
    fn triple_click_selects_all_via_handle_event() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        draw_to_populate(&mut ti);
        let mut doc = AutoCommit::new();

        let bx = x_for_byte(&ti, 8);
        drive(&mut ti, &mut doc, &mouse_move(bx, 20.0), mods(false, false));
        drive(&mut ti, &mut doc, &left_press(), mods(false, false));
        drive(&mut ti, &mut doc, &left_press(), mods(false, false));
        drive(&mut ti, &mut doc, &left_press(), mods(false, false));
        assert_eq!(
            ti.selection_range(),
            Some((0, 11)),
            "triple-click selects the whole field"
        );
    }

    /// The authoritative input layout resolves its ASCII glyphs from the
    /// requested named family rather than silently reverting to sans.
    #[test]
    fn font_family_override_shapes_in_named_family() {
        let mut tr = TextRenderer::new();
        let fams = tr.system_families();
        assert!(!fams.is_empty(), "the system font db exposes some families");
        let fam = fams[0].clone();

        let mut ti = TextInput::local()
            .with_initial_value("Reading")
            .with_font_family(FontChoice::Named(fam.clone()));
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        let theme = Theme::dark();
        let presence = crate::presence::PresenceCtx::empty();
        let mut scene = Scene::new();
        Widget::draw(
            &mut ti,
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
        );

        let layout = ti
            .single_layout
            .as_ref()
            .expect("input retained its layout");
        let face_ids: Vec<_> = layout
            .buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.font_id))
            .collect();
        assert!(!face_ids.is_empty());
        assert!(face_ids.iter().all(|id| {
            tr.font_system.db().face(*id).is_some_and(|face| {
                face.families
                    .iter()
                    .any(|(family, _language)| family == &fam)
            })
        }));
    }

    fn ml_input(text: &str, w: f64, h: f64) -> TextInput {
        let mut ti = TextInput::local().multiline(true).with_initial_value(text);
        ti.set_focus(true);
        ti.apply_layout_external(Rect::new(0.0, 0.0, w, h));
        draw_to_populate(&mut ti);
        ti
    }

    fn shaped_row(ti: &TextInput, byte: usize) -> usize {
        ti.single_layout
            .as_ref()
            .expect("multiline input retained its shaped paragraph")
            .visual_line_index(ti.text_to_layout_byte(byte))
    }

    #[test]
    fn multiline_enter_inserts_newline() {
        let mut ti = ml_input("ab", 300.0, 120.0);
        ti.set_cursor_pos(1); // between 'a' and 'b'
        let mut doc = AutoCommit::new();
        assert!(drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::Enter),
            mods(false, false)
        ));
        assert_eq!(ti.value(), "a\nb", "Enter inserted a newline at the caret");
        assert_eq!(
            ti.cursor_pos(),
            2,
            "caret sits just after the inserted newline"
        );
    }

    #[test]
    fn enter_submits_signals_without_inserting_and_shift_enter_still_splits() {
        let mut ti = TextInput::local()
            .with_enter_submits(true)
            .with_initial_value("hi");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();
        assert!(drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::Enter),
            mods(false, false)
        ));
        assert_eq!(
            ti.value(),
            "hi",
            "Enter must not insert a newline in submit mode"
        );
        assert!(ti.take_submit(), "Enter fired the submit signal");
        assert!(!ti.take_submit(), "submit is one-shot — cleared on read");

        let mut ml = TextInput::local()
            .multiline(true)
            .with_enter_submits(true)
            .with_initial_value("hi");
        ml.set_focus(true);
        ml.set_cursor_pos(2);
        drive(
            &mut ml,
            &mut doc,
            &named_key_event(NamedKey::Enter),
            mods(false, true),
        ); // Shift+Enter
        assert_eq!(
            ml.value(),
            "hi\n",
            "Shift+Enter inserts a newline in multiline submit mode"
        );
        assert!(!ml.take_submit(), "Shift+Enter must not submit");
    }

    #[test]
    fn multiline_up_down_moves_between_visual_lines() {
        let mut ti = ml_input("hello\nworld\nfoo", 400.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(8); // inside "world" (row 1)
        assert_eq!(shaped_row(&ti, ti.cursor_pos()), 1, "precondition: row 1");

        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowUp),
            mods(false, false),
        );
        assert_eq!(shaped_row(&ti, ti.cursor_pos()), 0, "Up moved to row 0");

        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowDown),
            mods(false, false),
        );
        assert_eq!(
            shaped_row(&ti, ti.cursor_pos()),
            1,
            "Down returned to row 1"
        );
        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowDown),
            mods(false, false),
        );
        assert_eq!(shaped_row(&ti, ti.cursor_pos()), 2, "Down reached row 2");
    }

    #[test]
    fn multiline_vertical_motion_preserves_goal_column() {
        let mut ti = ml_input("abcdefgh\nx\nabcdefgh", 500.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(8); // end of the first "abcdefgh" (row 0, high column)

        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowDown),
            mods(false, false),
        );
        assert_eq!(
            shaped_row(&ti, ti.cursor_pos()),
            1,
            "Down reached short row"
        );

        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowDown),
            mods(false, false),
        );
        let row = shaped_row(&ti, ti.cursor_pos());
        assert_eq!(row, 2, "Down reached the third row");
        assert!(
            ti.cursor_pos() >= 17,
            "goal column preserved across the short line — caret is near row 3's end, got {}",
            ti.cursor_pos()
        );
    }

    #[test]
    fn multiline_selection_spans_lines_and_copy_includes_newline() {
        let mut ti = ml_input("hello\nworld", 400.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(3); // inside "hello"
        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowDown),
            mods(false, true),
        ); // Shift+Down
        let (s, e) = ti
            .selection_range_public()
            .expect("a cross-line selection is active");
        assert!(
            s <= 5 && e > 6,
            "selection crosses the newline at byte 5, got ({s},{e})"
        );
        let selected = ti.selected_str().expect("selected slice");
        assert!(
            selected.contains('\n'),
            "copy would emit the newline: {selected:?}"
        );
    }

    #[test]
    fn multiline_soft_wraps_at_word_boundaries() {
        let text = "the quick brown fox jumps over the lazy dog again and again";
        let mut ti = ml_input(text, 170.0, 220.0); // narrow → must wrap
        let layout = ti.single_layout.as_ref().expect("paragraph was shaped");
        let runs: Vec<_> = layout.buffer.layout_runs().collect();
        assert!(runs.len() > 1, "long paragraph wrapped into visual rows");
        let first_end = runs[0]
            .glyphs
            .iter()
            .map(|glyph| glyph.end)
            .max()
            .unwrap_or(0);
        assert!(
            ti.value()
                .get(first_end..)
                .is_some_and(|s| s.starts_with(' ')),
            "wrap happened at a word boundary (the break precedes whitespace): {:?}",
            ti.value().get(..first_end)
        );

        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(0);
        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowDown),
            mods(false, false),
        );
        assert_eq!(shaped_row(&ti, ti.cursor_pos()), 1, "Down entered row 1");
        assert!(
            ti.cursor_pos() > 0,
            "caret advanced into the second visual row"
        );
    }

    #[test]
    fn multiline_row_geometry_uses_unicode_cjk_breaks() {
        let mut ti = TextInput::local()
            .multiline(true)
            .with_initial_value("東京京都大阪名古屋");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 70.0, 220.0));
        let mut renderer = TextRenderer::new();
        ti.build_lines(&mut renderer, 50.0);
        assert!(ti.lines.len() > 1, "narrow CJK input creates visual rows");
        let opportunities: Vec<_> = line_break_opportunities(ti.value())
            .map(|(byte, _)| byte)
            .collect();
        for row in ti.lines.iter().filter(|row| !row.hard_break) {
            assert!(
                opportunities.contains(&row.end),
                "soft row end {} is a UAX #14 opportunity: {opportunities:?}",
                row.end
            );
        }
    }

    #[test]
    fn multiline_home_end_are_per_visual_line() {
        let mut ti = ml_input("hello\nworld", 400.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(8); // "wo|rld" on row 1
        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::Home),
            mods(false, false),
        );
        assert_eq!(
            ti.cursor_pos(),
            6,
            "Home went to the start of the second line, not the field"
        );
        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::End),
            mods(false, false),
        );
        assert_eq!(
            ti.cursor_pos(),
            11,
            "End went to the end of the second line"
        );
    }

    #[test]
    fn multiline_sets_accessibility_flag() {
        let ml = TextInput::local().multiline(true);
        assert!(
            Widget::accessibility(&ml).unwrap().state.multiline,
            "multiline flagged in a11y"
        );
        let single = TextInput::local();
        assert!(!Widget::accessibility(&single).unwrap().state.multiline);
    }

    /// The newline is just a byte, so the shared delete path handles it.
    #[test]
    fn multiline_backspace_at_line_start_merges_lines() {
        let mut ti = ml_input("hello\nworld", 400.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(6); // start of "world", just after the newline
        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::Backspace),
            mods(false, false),
        );
        assert_eq!(
            ti.value(),
            "helloworld",
            "Backspace removed the newline, merging the lines"
        );
        assert_eq!(ti.cursor_pos(), 5);
    }

    #[test]
    fn secret_reveal_toggle_shows_glyphs_but_never_leaks_to_a11y() {
        let mut ti = TextInput::new(Vec::new()).with_secret(true);
        ti.text = "hunter2".to_string();
        assert_eq!(ti.display_text(), "•••••••", "masked by default");

        ti.toggle_reveal();
        assert!(ti.is_revealed());
        assert_eq!(
            ti.display_text(),
            "hunter2",
            "revealed shows the real glyphs"
        );
        let node = Widget::accessibility(&ti).unwrap();
        assert_eq!(
            node.value, None,
            "revealed value still must not reach the a11y tree"
        );
        assert!(
            node.state.protected,
            "still marked protected while revealed"
        );

        ti.toggle_reveal();
        assert_eq!(ti.display_text(), "•••••••", "toggles back to masked");
    }

    #[test]
    fn secret_masks_render_but_clipboard_and_caret_use_the_real_value() {
        let mut ti = TextInput::local()
            .with_secret(true)
            .with_initial_value("s3cr3t");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        ti.set_cursor_pos(0);
        drive(
            &mut ti,
            &mut doc,
            &named_key_event(NamedKey::ArrowRight),
            mods(false, false),
        );
        assert_eq!(ti.cursor_pos(), 1, "Right moved one real char boundary");

        drive(&mut ti, &mut doc, &char_key_event("a"), mods(true, false));
        assert_eq!(
            ti.selected_str(),
            Some("s3cr3t"),
            "copy sends the real value, never the mask"
        );

        drive(&mut ti, &mut doc, &char_key_event("x"), mods(true, false));
        assert_eq!(ti.value(), "", "Ctrl+X cut the real selection");
    }
}
