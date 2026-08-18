//! Single-line text input with full editing affordances:
//!
//! * Cursor placement via click; click + drag selects.
//! * Double-click selects a word; triple-click selects all.
//! * Shift + arrow / Home / End extends the selection.
//! * Ctrl + arrow jumps to the previous / next word boundary.
//! * Ctrl + A / C / V / X for select-all / copy / paste / cut.
//! * Backspace / Delete respect the selection (replace then collapse);
//!   Ctrl-modified delete consumes the next word.
//! * CRDT-backed: every mutation is replayed through
//!   `AutoCommit::splice_text` so collaborators see the same edits.
//!
//! Text vertical alignment matches the caret rendering — both are
//! centered against the same `line_height = font_size * 1.25` band so
//! the glyphs don't drift below the caret (visual-gap regression
//! pinned by `text_baseline_centred_with_caret`).

use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId, Value, ReadDoc, transaction::Transactable, ObjType};
use vello::Scene;
use vello::kurbo::{Affine, RoundedRect, Rect, Point, Stroke};
use vello::peniko::BlendMode;
use winit::event::{WindowEvent, MouseButton, ElementState, KeyEvent, Modifiers};
use winit::keyboard::{Key, NamedKey};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::color_compat::ColorCompat;
use crate::widget::Widget;
use crate::text_render::{FontChoice, TextRenderer, TextWeight};
use crate::text_selection::{
    clipboard_copy, clipboard_paste, extend_span_drag, next_char_boundary, next_word_boundary,
    prev_char_boundary, prev_word_boundary, word_boundaries,
};
use crate::theme::Theme;
use crate::presence::{PathSegment, PresenceKey};
use crate::widget::context_menu::{ContextMenu, MenuItem};
use crate::widget::code_editor::{token_color, Highlighter, Span};

/// The editing action a [`TextInput`] right-click menu row maps to. The menu's
/// `MenuItem` callbacks can't mutate the widget (they're `Arc<dyn Fn>`), so the
/// widget keeps this parallel list and dispatches on the `item_at` index itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CtxAction {
    Cut,
    Copy,
    Paste,
    SelectAll,
}

/// Render-time font size for the input's body text. Kept as a single
/// constant so the measure-and-draw pair always agrees.
const FONT_SIZE: f32 = 16.0;
/// Line-height ratio cosmic-text uses when shaping (see
/// `text_render.rs::measure_weighted`). The caret is centered
/// against this band, not against the font size directly, so the
/// glyph row that draws *below* the cosmic-text baseline ends up
/// vertically aligned with the caret. Multiplied against the
/// effective font size (which may be a per-widget override) at
/// draw time.
const LINE_HEIGHT_RATIO: f32 = 1.25;
/// Caret thickness in CSS-like pixels. 2 px reads as a clean ibeam
/// without becoming an obvious block at smaller font sizes.
const CARET_WIDTH: f64 = 2.0;
/// Maximum gap between two mouse-clicks that still counts as a
/// double / triple click. 400 ms matches the GTK / Qt default.
const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(400);
/// Pixels of cursor wiggle still allowed between rapid clicks before
/// they're treated as separate click chains.
const MULTI_CLICK_RADIUS: f64 = 4.0;
/// Horizontal text inset for the multiline path, in logical px. Shared by the
/// draw, caret geometry, and pointer hit-test so all three agree — the
/// single-line path keeps its own (theme-derived) padding untouched.
const ML_PAD_X: f64 = 10.0;
/// Vertical text inset (top/bottom) for the multiline path, in logical px.
const ML_PAD_Y: f64 = 8.0;
/// Monospace digit advance as a fraction of the font size, used to size the
/// code-editor line-number gutter. The gutter offset uses this identically for
/// geometry (caret/click/selection) and for drawing, so they stay aligned even
/// if it is only an approximation of the true glyph advance.
const GUTTER_DIGIT_RATIO: f64 = 0.62;
/// Left padding inside the code-editor gutter, in logical px.
const GUTTER_PAD_L: f64 = 8.0;
/// Right padding (gutter → text) inside the code-editor gutter, in logical px.
const GUTTER_PAD_R: f64 = 12.0;

/// What unit the in-flight drag snaps to. Set on press based on the
/// click ladder so a double-click-then-drag extends by whole words
/// (matching every editor on the planet).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum DragUnit {
    /// Plain char-level drag (single click + drag, or shift+click).
    Char,
    /// Word-level drag (double click + drag).
    Word,
    /// All-content (triple click + drag, mostly a no-op for single-line).
    All,
}

pub struct TextInput {
    _id: String,
    path: Vec<PathSegment>,
    rect: Rect,
    is_hovered: bool,
    is_focused: bool,
    text: String,
    /// Current caret position as a *byte* index into `text`. Always on
    /// a char boundary (clamped by `sync` and after every mutation).
    cursor_pos: usize,
    /// When `Some`, an active selection is anchored here. The visible
    /// range is `(min(anchor, cursor), max(anchor, cursor))`. Cleared
    /// to `None` whenever the cursor moves without `shift` held, or
    /// when text is inserted / a single click lands.
    selection_anchor: Option<usize>,
    /// True while the user is dragging the mouse to extend a selection
    /// (between MouseInput::Pressed and MouseInput::Released). Forces
    /// `wants_pointer_capture` so a drag that leaves the rect keeps
    /// updating the selection.
    is_dragging: bool,
    /// Snap-unit for the in-flight drag. Set on the click that started
    /// the drag based on the click ladder: single click → `Char`,
    /// double click → `Word`, triple click → `All`. The CursorMoved
    /// handler honours this so dragging from a double-click extends
    /// by whole words, etc.
    drag_unit: DragUnit,
    /// Anchor span snapped to `drag_unit` boundaries. For a
    /// double-click this is the whole word the user clicked on; the
    /// drag then extends from whichever end of this span is
    /// "opposite" the current pointer.
    drag_anchor_span: (usize, usize),
    /// Time + position of the most recent click — used to detect
    /// double / triple clicks (within `MULTI_CLICK_INTERVAL` and
    /// `MULTI_CLICK_RADIUS`).
    last_click_time: Option<Instant>,
    last_click_pos: Point,
    click_count: u32,
    /// True when a user edit landed since the last `take_dirty()`.
    /// Cleared on `take_dirty()` and on `set_text()` (programmatic
    /// writes by the parent don't count as user edits).
    dirty: bool,
    /// `true` in CRDT mode (a path was supplied); `false` in local
    /// mode. Gates whether the edit helpers attempt to splice into
    /// `state` (they no-op cleanly when this is false even if a
    /// path string was set without an actual CRDT object).
    crdt_bound: bool,
    on_change_path: String,
    last_mouse_pos: Point,
    char_x_positions: Vec<f64>,
    scroll_x: f64,
    last_edit_time: Option<Instant>,
    /// When `Some`, overrides the default body font size. Set via
    /// [`with_font_size`] / [`set_font_size`]. Wrapper widgets such as
    /// `RichTextLine` use this to render heading rows larger than
    /// paragraph rows without forking the widget.
    font_size_override: Option<f32>,
    /// When true, the input renders without the rounded-rect bg /
    /// border that signals "this is a form field". Used by block-
    /// editor cells where the surrounding text should read as plain
    /// document copy, not as a form input.
    plain: bool,
    /// When `Some`, the input is bound directly to this Automerge `Text`
    /// object. This takes precedence over the string-`on_change_path`
    /// mode and is how a `BlockEditor` binds each block's text — its
    /// `Text` lives at `pages[id].blocks[i].text`, nested through a List
    /// index that a flat string key can't address. `sync` reads from it
    /// and every edit splices into it.
    bound_text: Option<ObjId>,
    /// The local user's id, stamped as an authorship mark on every
    /// inserted run so peers can highlight "what they wrote" in this
    /// user's color. `None` disables authorship stamping (examples,
    /// tests, the pre-collab single-user case).
    local_author: Option<String>,
    /// When true the widget renders / measures a row of bullet glyphs in
    /// place of the real text (password masking). The CRDT-backed
    /// `self.text` stays the *true* value so callers that read the
    /// document (e.g. the greeter's auth path) get the real secret.
    secret: bool,
    /// When true the input is inert: it is dropped from the focus order
    /// (`is_focusable() == false`), ignores all key / pointer input, and
    /// renders dimmed (sunken background + dim text, no caret). Mirrors the
    /// installer's `ActionButton::disabled`. Default `false`, so existing
    /// callers are unaffected.
    disabled: bool,
    /// Right-click context menu (Cut / Copy / Paste / Select All). Built and
    /// opened on a secondary (right) mouse press, dismissed on click-outside or
    /// Escape. Reuses the shared [`ContextMenu`] primitive; rendered via the
    /// overlay pass so it floats above the field.
    context_menu: ContextMenu,
    /// Per-row action parallel to `context_menu`'s items (separators map to
    /// `None`). The menu's `Arc<dyn Fn>` callbacks can't reach back into the
    /// widget, so a left-click on a row dispatches by index through this list.
    context_actions: Vec<Option<CtxAction>>,
    /// Hint text shown in `theme.text_dim` when the input is BOTH empty
    /// and unfocused. Cleared (rendered as nothing) once the user focuses
    /// the field or types into it, exactly like a native form placeholder.
    /// Empty string (the default) disables the placeholder entirely, so
    /// existing callers are unaffected.
    placeholder: String,
    /// When true the text is shaped + drawn in the system monospace face.
    /// Both the per-char measurement (`char_x_positions`) and the draw call
    /// switch to `Family::Monospace`, so the caret/selection stay aligned.
    /// Default `false`; code-block cells opt in so code reads as code.
    mono: bool,
    /// When `Some`, both measurement and draw shape the text in this explicit
    /// [`FontChoice`] family (a serif book face, a named installed family, …)
    /// rather than the default sans / mono. Because the per-char metrics
    /// (`char_x_positions`) are measured with the same family the draw uses,
    /// the caret + selection stay aligned. This is how a rename field can
    /// render the edited value in its own typeface. Takes precedence over
    /// `mono`. Default `None` (sans / mono per `mono`).
    family: Option<FontChoice>,
    /// When true the field is a multi-line editor: newlines are real
    /// characters, `Enter` inserts one, `Up`/`Down` move the caret by visual
    /// line, `Home`/`End` act per visual line, selection spans lines, and the
    /// text soft-wraps at the field width instead of scrolling horizontally.
    /// Default `false` — the single-line behaviour is byte-for-byte unchanged.
    multiline: bool,
    /// When true, a plain `Enter` (no Shift) fires a one-shot submit signal
    /// (`take_submit`) instead of inserting a newline — for single-line hosts
    /// that want Enter to accept, and for chat-style multiline composers where
    /// Enter sends and Shift+Enter inserts a newline. Default `false`.
    enter_submits: bool,
    /// One-shot: set when `Enter` was pressed while [`enter_submits`]. The host
    /// reads and clears it via [`take_submit`].
    submit_requested: bool,
    /// Reveal a `secret` field's real glyphs (the eye toggle). Only meaningful
    /// when `secret` is set; the plaintext is still kept out of the a11y tree
    /// and `text_review` regardless, so revealing shows it on screen only.
    reveal: bool,
    /// Visual-line layout for the multiline path, rebuilt by each `draw`. Each
    /// entry is one on-screen row (a soft-wrapped fragment or a full short
    /// line); the caret/selection/hit-test math reads it. Empty in single-line
    /// mode.
    lines: Vec<VisualLine>,
    /// Preferred line-relative x (logical px) preserved across consecutive
    /// `Up`/`Down` so the caret keeps its column over shorter lines. Reset to
    /// `None` on any horizontal move, edit, or click. Multiline only.
    goal_x: Option<f64>,
    /// Vertical scroll offset in logical px (multiline). Follows the caret so a
    /// body taller than the field stays legible while editing.
    scroll_y: f64,
    /// Code-editor mode: draw a line-number gutter down the left edge (multiline
    /// only). Enabled by [`CodeEditor`](crate::widget::CodeEditor); default
    /// `false` so every existing field is unchanged.
    gutter: bool,
    /// Code-editor mode: draw a faint band across the caret's logical line
    /// (multiline only). Default `false`.
    highlight_current_line: bool,
    /// Optional language tokenizer. When `Some`, each visible row's glyph runs
    /// are coloured by the [`Span`]s it produces (colours resolved from the
    /// [`Theme`] via `token_color`). `None` renders the whole body in one
    /// colour, exactly as before. Default `None`.
    highlighter: Option<Box<dyn Highlighter>>,
    /// Number of spaces a `Tab` inserts (`Some(0)` inserts a literal `\t`).
    /// `None` (the default) means the field does not intercept `Tab` at all, so
    /// the host keeps using it for focus traversal — every existing caller is
    /// unaffected. `Shift+Tab` outdents by the same width.
    tab_spaces: Option<usize>,
    /// Cached per-logical-line highlight spans, rebuilt only when the text
    /// changes (keyed by `hl_sig`). Empty when no highlighter is set.
    hl_spans: Vec<Vec<Span>>,
    /// Start byte of each logical line in `text`, parallel to `hl_spans`.
    hl_line_starts: Vec<usize>,
    /// Cheap signature of the text `hl_spans`/`hl_line_starts` were built for.
    hl_sig: u64,
    /// Caret position the multiline body last followed with vertical scroll.
    /// The caret-follow only re-centres when the caret actually *moves* (edit /
    /// arrow), so a wheel scroll away from the caret sticks instead of snapping
    /// straight back every frame. `None` before the first draw.
    last_scroll_caret: Option<usize>,

    /// Opt-in: when true, typing a `:shortcode` token opens an inline fuzzy
    /// emoji-completion overlay (see [`with_emoji_completion`]). Off by default,
    /// so no existing field changes behaviour — an app enables it explicitly.
    emoji_complete: bool,
    /// Live completion state (only meaningful while `emoji_complete`).
    ec: EmojiCompletion,
}

/// Inline `:shortcode:` emoji-completion state for a [`TextInput`]. Reads the
/// shared [`crate::emoji`] dataset, so no app re-implements emoji search.
#[derive(Default)]
struct EmojiCompletion {
    /// Whether the suggestion overlay is currently showing.
    active: bool,
    /// Byte offset of the `:` that starts the active token.
    token_start: usize,
    /// Current suggestions (best first), rebuilt on every edit.
    matches: Vec<&'static crate::emoji::Emoji>,
    /// Highlighted row within `matches`.
    selected: usize,
}

/// One on-screen row of a multiline [`TextInput`]: either a whole short logical
/// line or one soft-wrapped fragment of a longer paragraph. Rebuilt every
/// `draw`; the caret, selection, and pointer hit-test all read it so they agree
/// pixel-for-pixel with what was painted.
#[derive(Clone, Debug)]
struct VisualLine {
    /// Byte offset in `text` where this row starts.
    start: usize,
    /// Byte offset (exclusive) where this row ends, NOT counting a trailing
    /// hard `\n`.
    end: usize,
    /// True when the row ends at a hard `\n` (or is the final row). A
    /// soft-wrapped row is `false`, which is how the caret decides a boundary
    /// byte belongs to the *next* row's start rather than this row's end.
    hard_break: bool,
    /// Per-char left-edge x within the row (line-relative logical px), measured
    /// with the same family the draw uses. `len() == chars_in_row + 1`; the
    /// last entry is the row's total width.
    char_x: Vec<f64>,
    /// Top y of the row in this input's local coordinates (before `scroll_y`).
    y: f64,
}

/// Automerge mark name used to attribute each character's author.
pub(crate) const AUTHOR_MARK: &str = "author";

impl TextInput {
    /// Construct a text input bound to a presence `path` (used as the
    /// widget's `PresenceKey::Widget { path }`). Empty path opts the widget
    /// out of presence rendering.
    pub fn new(path: Vec<PathSegment>) -> Self {
        Self {
            _id: String::new(),
            path,
            rect: Rect::ZERO,
            is_hovered: false,
            is_focused: false,
            text: String::new(),
            cursor_pos: 0,
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
            scroll_x: 0.0,
            last_edit_time: None,
            dirty: false,
            crdt_bound: false,
            font_size_override: None,
            plain: false,
            bound_text: None,
            local_author: None,
            secret: false,
            disabled: false,
            context_menu: ContextMenu::new(),
            context_actions: Vec::new(),
            placeholder: String::new(),
            mono: false,
            family: None,
            multiline: false,
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
            emoji_complete: false,
            ec: EmojiCompletion::default(),
        }
    }

    /// Opt into inline `:shortcode:` emoji completion. While enabled, typing a
    /// colon token (`:smi`) opens a small fuzzy-search overlay of matching emoji;
    /// Up/Down move the selection, Enter/Tab accept it (replacing the `:token`
    /// with the emoji), and Escape dismisses it. Typing the closing colon of an
    /// exact shortcode (`:fire:`) auto-replaces it. Off by default so an app
    /// turns it on only where it wants it (e.g. a chat composer).
    pub fn with_emoji_completion(mut self) -> Self {
        self.emoji_complete = true;
        self
    }

    /// Mutable form of [`with_emoji_completion`](Self::with_emoji_completion).
    pub fn set_emoji_completion(&mut self, on: bool) {
        self.emoji_complete = on;
        if !on {
            self.ec.active = false;
        }
    }

    /// Whether the inline emoji-completion overlay is currently open. A host can
    /// use this to keep a submit key (Enter) from firing while the overlay owns
    /// it — though the widget already consumes those keys itself.
    pub fn emoji_completion_open(&self) -> bool {
        self.ec.active
    }

    /// Programmatically set the text (local mode), park the caret at the end,
    /// and re-run `:emoji:` completion against it so the overlay reflects the
    /// new value. Used by a host that seeds a value and wants the completion
    /// state to follow (e.g. a screenshot scenario). No-op on completion when
    /// [`with_emoji_completion`](Self::with_emoji_completion) is off.
    pub fn set_text_with_completion(&mut self, s: &str) {
        self.set_text(s);
        self.cursor_pos = self.text.len();
        let mut sink = AutoCommit::new();
        self.refresh_emoji_completion(&mut sink, None);
    }

    /// Set the local user's id for authorship stamping. Every subsequent
    /// edit attributes the inserted run to this id. `None` disables it.
    pub fn set_local_author(&mut self, author: Option<String>) {
        self.local_author = author;
    }

    /// Bind this input directly to an Automerge `Text` object. After
    /// binding, [`sync`] reads its content from `obj` and every edit
    /// splices into `obj` — regardless of the string-path mode. Used by
    /// `RichTextLine`/`BlockEditor` to back a block with a per-block
    /// `Text` nested inside the page's `blocks` List.
    pub fn bind_text(&mut self, obj: ObjId) {
        self.bound_text = Some(obj);
        self.crdt_bound = true;
    }

    /// The Automerge `Text` this input is bound to, if any. Used by
    /// `BlockEditor` to match local blocks to doc list elements by
    /// object identity when reconciling structure.
    pub fn bound_obj(&self) -> Option<&ObjId> {
        self.bound_text.as_ref()
    }

    /// Resolve the Automerge `Text` this input edits, if any. A directly
    /// bound `ObjId` (via [`bind_text`]) wins over the string-path mode. This is
    /// the read-only resolver: it never creates the object, so a caller that only
    /// needs to read (or is on a non-mutating path) leaves the document untouched.
    fn resolve_text_obj(&self, state: &AutoCommit, root: &ObjId) -> Option<ObjId> {
        if let Some(obj) = &self.bound_text {
            return Some(obj.clone());
        }
        if self.crdt_bound {
            if let Ok(Some((Value::Object(ObjType::Text), id))) =
                state.get(root, self.on_change_path.as_str())
            {
                return Some(id);
            }
        }
        None
    }

    /// Resolve the backing `Text` object for an *edit*, seeding it on first use
    /// when the field is `crdt_bound` but the host never created one.
    ///
    /// A `with_on_change_path` field used to persist edits only if the host had
    /// already parked a `Text` object at the key; otherwise a keystroke mutated
    /// the local buffer alone and was silently lost on the next `sync` — the
    /// caret had already moved, so a Backspace read as "jumps back a char without
    /// deleting", and typed text vanished on a widget rebuild. Seeding here makes
    /// every bound field self-sufficient. A scalar string previously parked at
    /// the key (a seeded initial value) is preserved by splicing it into the new
    /// object, mirroring the eager `ensure_text_object` helper apps carried.
    fn resolve_or_seed_text_obj(&self, state: &mut AutoCommit, root: &ObjId) -> Option<ObjId> {
        if let Some(obj) = &self.bound_text {
            return Some(obj.clone());
        }
        if !self.crdt_bound || self.on_change_path.is_empty() {
            return None;
        }
        let key = self.on_change_path.as_str();
        // An existing Text object is used untouched.
        if let Ok(Some((Value::Object(ObjType::Text), id))) = state.get(root, key) {
            return Some(id);
        }
        // Preserve a scalar string already at the key (a seeded value).
        let existing = match state.get(root, key) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                automerge::ScalarValue::Str(t) => t.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        };
        let obj = state.put_object(root, key, ObjType::Text).ok()?;
        if !existing.is_empty() {
            let _ = state.splice_text(&obj, 0, 0, &existing);
        }
        Some(obj)
    }

    /// Builder-style: override the body font size in CSS-like pixels.
    /// Affects both the rendered glyph height and the row's intrinsic
    /// vertical centering. Returning `Self` matches the with_*
    /// pattern across the widget family.
    pub fn with_font_size(mut self, size: f32) -> Self {
        self.font_size_override = Some(size);
        self
    }

    /// Builder-style: render without the rounded-rect bg + border.
    /// The caret + selection still paint as usual when focused.
    /// Used by `RichTextLine` so a block-editor cell reads as
    /// document copy instead of a form field.
    pub fn with_plain_chrome(mut self) -> Self {
        self.plain = true;
        self
    }

    /// Mutate the font-size override after construction.
    pub fn set_font_size(&mut self, size: f32) {
        self.font_size_override = Some(size);
    }

    /// Mutate the plain-chrome flag after construction.
    pub fn set_plain_chrome(&mut self, plain: bool) {
        self.plain = plain;
    }

    /// Builder-style: shape + draw the text in the system monospace face.
    /// Used by code-block cells so code reads as code.
    pub fn with_mono(mut self) -> Self {
        self.mono = true;
        self
    }

    /// Mutate the monospace flag after construction.
    pub fn set_mono(&mut self, mono: bool) {
        self.mono = mono;
    }

    /// True iff this input renders its text in the monospace face.
    pub fn is_mono(&self) -> bool {
        self.mono
    }

    /// Builder-style: shape + draw + measure the text in an explicit
    /// [`FontChoice`] family (a serif book face, a named installed family, …).
    /// Takes precedence over the mono flag. Used by
    /// [`crate::widget::EditableLabel`] so an inline rename field renders the
    /// value in its own typeface. Passing [`FontChoice::SansSerif`] is
    /// equivalent to the default.
    pub fn with_font_family(mut self, family: FontChoice) -> Self {
        self.family = Some(family);
        self
    }

    /// Mutate the font-family override after construction. `None` restores the
    /// default sans / mono behaviour.
    pub fn set_font_family(&mut self, family: Option<FontChoice>) {
        self.family = family;
    }

    /// The active font-family override, if any.
    pub fn font_family(&self) -> Option<&FontChoice> {
        self.family.as_ref()
    }

    /// Measure `s` with the family the draw path will use (an explicit
    /// [`FontChoice`] override wins, else mono when `self.mono`, else sans), so
    /// `char_x_positions` and the drawn glyphs agree.
    fn measure_str(&self, tr: &mut TextRenderer, s: &str, size: f32) -> (f32, f32) {
        if let Some(fam) = &self.family {
            tr.measure_font(s, size, None, TextWeight::Regular, false, fam)
        } else if self.mono {
            tr.measure_styled(s, size, None, TextWeight::Regular, false, true)
        } else {
            tr.measure(s, size, None)
        }
    }

    /// Construct a TextInput that owns its own string with no CRDT
    /// integration. Use when a parent widget needs a small edit
    /// field (color-picker hex, in-place rename, etc.) and doesn't
    /// have a CRDT path to spend on it. Read the live string via
    /// [`value`] and observe changes with [`take_dirty`].
    pub fn local() -> Self {
        let mut s = Self::new(Vec::new());
        s.crdt_bound = false;
        s
    }

    /// Set the initial text. Cursor is parked at the end. The
    /// dirty flag stays false — this is a programmatic seed, not
    /// a user edit.
    pub fn with_initial_value(mut self, s: &str) -> Self {
        self.text = s.to_string();
        self.cursor_pos = self.text.len();
        self.dirty = false;
        self
    }

    /// Programmatically replace the text. Clamps the cursor,
    /// clears the dirty flag — the parent set this, the user
    /// didn't edit it.
    pub fn set_text(&mut self, s: &str) {
        if self.text == s { return; }
        self.text = s.to_string();
        self.selection_anchor = None;
        self.clamp_to_text();
        self.dirty = false;
        self.last_edit_time = None;
    }

    /// Current text content, regardless of CRDT/local mode.
    pub fn value(&self) -> &str {
        &self.text
    }

    /// Insert `s` at the caret (replacing any active selection) as if the user
    /// had typed it, marking the field dirty so a host observing
    /// [`take_dirty`] picks the edit up. This is the programmatic sibling of a
    /// keystroke, for hosts that splice non-keyboard text into a **local-mode**
    /// field — e.g. a chat composer receiving an emoji picked from a palette.
    /// In CRDT-bound mode prefer routing a real key event so the splice lands
    /// in the document with authorship marks.
    pub fn insert_at_cursor(&mut self, s: &str) {
        // A throwaway sink: `insert_str` only touches the CRDT when given an
        // object id, so a local field mutates `self.text` alone.
        let mut sink = AutoCommit::new();
        self.insert_str(s, &mut sink, None);
    }

    // ---- inline :shortcode: emoji completion --------------------------------

    /// If the caret sits inside an open `:word` token, return `(colon_byte,
    /// query)`. The colon must sit at the start of the text or right after
    /// whitespace so `http://`, `3:45`, and `key:value` don't trigger it.
    fn emoji_token(&self) -> Option<(usize, String)> {
        let cursor = self.cursor_pos.min(self.text.len());
        let is_word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '+' | '-');
        // Walk back over the token's word-chars.
        let mut start = cursor;
        for (i, c) in self.text[..cursor].char_indices().rev() {
            if is_word(c) {
                start = i;
            } else {
                break;
            }
        }
        // The char immediately before the token must be a ':'.
        let colon_start = self.text[..start]
            .char_indices()
            .next_back()
            .and_then(|(i, c)| (c == ':').then_some(i))?;
        // ...and the char before the colon must be nothing or whitespace.
        if colon_start > 0 {
            let prev = self.text[..colon_start].chars().next_back();
            if prev.is_some_and(|c| !c.is_whitespace()) {
                return None;
            }
        }
        let query = self.text[start..cursor].to_string();
        if query.is_empty() {
            return None;
        }
        Some((colon_start, query))
    }

    /// Recompute the emoji-completion overlay from the current text + caret, and
    /// apply an exact-shortcode auto-replace when the user just closed a token
    /// (`:fire:` → 🔥). No-op unless emoji completion is enabled.
    fn refresh_emoji_completion(&mut self, state: &mut AutoCommit, obj_id: Option<&ObjId>) {
        if !self.emoji_complete {
            self.ec.active = false;
            return;
        }
        let is_word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '+' | '-');
        // Exact `:word:` auto-replace when the caret is just past a closing ':'.
        let cursor = self.cursor_pos.min(self.text.len());
        if self.text[..cursor].ends_with(':') {
            let inner_end = cursor - 1;
            if let Some(open) = self.text[..inner_end].rfind(':') {
                let word = &self.text[open + 1..inner_end];
                let ok_prefix = open == 0
                    || self.text[..open]
                        .chars()
                        .next_back()
                        .map_or(true, |c| c.is_whitespace());
                if ok_prefix && !word.is_empty() && word.chars().all(is_word) {
                    if let Some(e) = crate::emoji::exact(word) {
                        let ch = e.ch.to_string();
                        self.set_selection(open, cursor);
                        self.insert_str(&ch, state, obj_id);
                        self.ec.active = false;
                        return;
                    }
                }
            }
        }
        // Otherwise show fuzzy suggestions for an open `:token`.
        match self.emoji_token() {
            Some((start, query)) => {
                let matches = crate::emoji::search(&query, 8);
                if matches.is_empty() {
                    self.ec.active = false;
                } else {
                    self.ec.token_start = start;
                    self.ec.matches = matches;
                    if self.ec.selected >= self.ec.matches.len() {
                        self.ec.selected = 0;
                    }
                    self.ec.active = true;
                }
            }
            None => self.ec.active = false,
        }
    }

    /// Accept the highlighted emoji suggestion, replacing the `:token` with it.
    fn accept_emoji_completion(&mut self, state: &mut AutoCommit, obj_id: Option<&ObjId>) {
        if !self.ec.active {
            return;
        }
        let Some(e) = self.ec.matches.get(self.ec.selected).copied() else {
            self.ec.active = false;
            return;
        };
        let ch = e.ch.to_string();
        let start = self.ec.token_start;
        let end = self.cursor_pos.min(self.text.len());
        self.set_selection(start, end);
        self.insert_str(&ch, state, obj_id);
        self.ec.active = false;
    }

    /// Paint the emoji-completion overlay (a small suggestion list anchored just
    /// above the token's caret). Called from `draw_overlay`, so it composites
    /// over sibling widgets.
    fn draw_emoji_overlay(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.ec.active || self.ec.matches.is_empty() {
            return;
        }
        let tokens = &theme.tokens;
        let caret = self.caret_rect(theme, self.ec.token_start.min(self.text.len()));
        let row_h = 26.0;
        let w = 244.0;
        let n = self.ec.matches.len().min(8);
        let h = n as f64 * row_h + 8.0;
        let x0 = caret.x0;
        // Anchor above the caret (composers sit near the window's bottom edge).
        let y1 = caret.y0 - 4.0;
        let y0 = y1 - h;
        let panel = Rect::new(x0, y0, x0 + w, y1);
        let radius = tokens.radii.md as f64;

        // Themed drop shadow from the elevation token.
        let elev = &tokens.elevation.e3;
        let shadow = Rect::new(
            panel.x0,
            panel.y0 + elev.offset_y as f64,
            panel.x1,
            panel.y1 + elev.offset_y as f64,
        )
        .inflate(elev.blur as f64 * 0.5, elev.blur as f64 * 0.5);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            elev.color.with_alpha_factor(0.7),
            None,
            &RoundedRect::from_rect(shadow, radius + elev.blur as f64 * 0.5),
        );
        let surface = RoundedRect::from_rect(panel, radius);
        scene.fill(vello::peniko::Fill::NonZero, transform, theme.bg_primary, None, &surface);
        scene.stroke(&Stroke::new(1.0), transform, theme.border_standard, None, &surface);

        for (i, e) in self.ec.matches.iter().take(8).enumerate() {
            let ry = panel.y0 + 4.0 + i as f64 * row_h;
            let row = Rect::new(panel.x0 + 4.0, ry, panel.x1 - 4.0, ry + row_h);
            if i == self.ec.selected {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.bg_active,
                    None,
                    &RoundedRect::from_rect(row, tokens.radii.sm as f64),
                );
            }
            tr.draw_text(
                scene,
                e.ch,
                16.0,
                tokens.colors.text_primary,
                transform * Affine::translate((row.x0 + 8.0, row.center().y - 9.0)),
                None,
            );
            let name = format!(":{}:", e.name);
            tr.draw_text(
                scene,
                &name,
                13.0,
                tokens.colors.text_dim,
                transform * Affine::translate((row.x0 + 36.0, row.center().y - 8.0)),
                None,
            );
        }
    }

    /// Current caret position as a byte index into [`value`]. Guaranteed
    /// to land on a UTF-8 char boundary. Used by wrapper widgets (like
    /// `RichTextLine`) that need to know where to split / merge / decide
    /// whether the cursor is at the start or end of the line.
    pub fn cursor_pos(&self) -> usize {
        self.cursor_pos
    }

    /// Move the caret to `pos` bytes. Clamped to the text length and
    /// to the nearest preceding char boundary. Selection is collapsed.
    /// Programmatic only — does not count as a user edit (dirty stays
    /// false). Used by `RichTextLine` after a split to land the caret
    /// at the start of the new line.
    pub fn set_cursor_pos(&mut self, pos: usize) {
        let mut p = pos.min(self.text.len());
        while p > 0 && !self.text.is_char_boundary(p) {
            p -= 1;
        }
        self.cursor_pos = p;
        self.selection_anchor = None;
    }

    /// If a selection is active, return its byte range as
    /// `Some((min, max))`. Used by `RichTextLine` to apply inline
    /// marks (`Ctrl-B` / `Ctrl-I` / `Ctrl-E`) to the selected slice.
    pub fn selection_range_public(&self) -> Option<(usize, usize)> {
        self.selection_range()
    }

    /// Per-character left-edge x positions in logical pixels
    /// (filled by the most-recent draw call). `len()` is
    /// `text.len() + 1` — the last entry is the right edge of the
    /// final glyph. Empty until the first draw lands.
    pub fn char_x_positions(&self) -> &[f64] {
        &self.char_x_positions
    }

    /// Current horizontal scroll offset (logical pixels). Used by
    /// `RichTextLine` overlay drawing to project mark backgrounds
    /// against the same scroll axis as the underlying text.
    pub fn scroll_x(&self) -> f64 {
        self.scroll_x
    }

    /// Current vertical scroll offset (logical px, multiline). Exposed so a
    /// code-editor host can align a diagnostics overlay with the scrolled text.
    pub fn scroll_y(&self) -> f64 {
        self.scroll_y
    }

    /// The line-height band (logical px) both text and caret share. Public so a
    /// host can position per-line overlays (gutter markers, squiggles).
    pub fn body_line_height(&self) -> f64 {
        self.line_height()
    }

    /// The multiline gutter width (logical px, `0` when no gutter). Lets a host
    /// place gutter-area decorations (a diagnostic severity dot) precisely.
    pub fn gutter_width(&self) -> f64 {
        self.code_gutter_w()
    }

    /// Resolve a logical-space `point` to a byte offset in the text (multiline
    /// row+column hit-test, or the single-line horizontal one). Requires a prior
    /// `draw` to have built the metrics. Public counterpart to the internal
    /// hit-test for hosts that map a pointer to a caret (a code editor's
    /// hover-a-diagnostic lookup).
    pub fn byte_at_point(&self, point: Point) -> usize {
        self.resolve_cursor(point)
    }

    /// Scroll the multiline body by `dy` logical px (positive = down), clamped
    /// to the content. No-op for a single-line field. A code-editor host wires
    /// this to the mouse wheel.
    pub fn scroll_by(&mut self, dy: f64) {
        if !self.multiline {
            return;
        }
        let view_h = self.rect.height() - ML_PAD_Y * 2.0;
        let max = (self.content_height() - ML_PAD_Y * 2.0 - view_h).max(0.0);
        self.scroll_y = (self.scroll_y + dy).clamp(0.0, max);
    }

    /// True iff the input is currently rendering in plain-chrome mode
    /// (no rounded background / border). Block-editor cells use this.
    pub fn is_plain(&self) -> bool {
        self.plain
    }

    /// Internal-padding-x the input uses when laying out its text
    /// against `rect`. In plain-chrome mode this is 0; in the styled
    /// form-field mode it's `theme.padding_standard`. Exposed so
    /// wrapper widgets can compute the absolute pixel position of a
    /// byte offset (caret-precise popover anchoring, link hit-test).
    pub fn text_padding_x(&self, theme: &Theme) -> f64 {
        if self.plain {
            0.0
        } else {
            theme.padding_standard as f64
        }
    }

    /// `(text_y, line_height)` — the vertical band the text + caret share,
    /// in this input's local coordinates. The single source of truth for
    /// caret/selection height + position used by BOTH the focused local
    /// caret and any remote (peer) caret, so they line up exactly.
    pub fn text_band(&self) -> (f64, f64) {
        let text_size = self.font_size_override.unwrap_or(FONT_SIZE);
        let line_height = (text_size * LINE_HEIGHT_RATIO) as f64;
        let text_y = self.rect.y0 + (self.rect.height() - line_height) / 2.0;
        (text_y, line_height)
    }

    /// Local-coordinate x of the left edge of the caret at byte `byte` —
    /// the same metric the focused caret uses (per-char layout + padding −
    /// horizontal scroll). Falls back to the end-of-text x when `byte` is
    /// past the laid-out range. Requires a prior `draw` to have populated
    /// `char_x_positions`.
    fn caret_x(&self, theme: &Theme, byte: usize) -> f64 {
        let off = self
            .char_x_positions
            .get(byte)
            .copied()
            .unwrap_or_else(|| self.char_x_positions.last().copied().unwrap_or(0.0));
        self.rect.x0 + self.text_padding_x(theme) + off - self.scroll_x
    }

    /// The exact caret rectangle for byte offset `byte`, in local
    /// coordinates — identical geometry to the focused local caret. Peer
    /// carets render through this so a remote caret is pixel-identical to
    /// the local one, only recoloured. In multiline mode the caret is placed
    /// on the visual row that contains `byte` (from the last `draw`'s layout).
    pub fn caret_rect(&self, theme: &Theme, byte: usize) -> Rect {
        if self.multiline {
            return self.caret_rect_ml(byte);
        }
        let x = self.caret_x(theme, byte);
        let (text_y, line_height) = self.text_band();
        Rect::new(x, text_y, x + CARET_WIDTH, text_y + line_height)
    }

    /// The selection highlight rect for the `[start, end)` byte range, in
    /// local coordinates — same vertical band as the caret. Single-line only;
    /// multiline callers use [`selection_rects`], which returns one rect per
    /// visual row the selection covers.
    pub fn selection_rect(&self, theme: &Theme, start: usize, end: usize) -> Rect {
        let x0 = self.caret_x(theme, start);
        let x1 = self.caret_x(theme, end);
        let (text_y, line_height) = self.text_band();
        Rect::new(x0, text_y, x1, text_y + line_height)
    }

    /// Highlight rects for the `[start, end)` byte range. Single-line returns
    /// exactly one rect ([`selection_rect`]); multiline returns one rect per
    /// visual row the range spans, so a cross-line selection paints correctly.
    pub fn selection_rects(&self, theme: &Theme, start: usize, end: usize) -> Vec<Rect> {
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
            // Does the selection continue past this row's break? If so, extend
            // the highlight a little past the last glyph to mark the newline.
            let spans_break = end > r.end;
            let x0 = self.x_in_row(r, lo);
            let mut x1 = self.x_in_row(r, hi);
            if spans_break && hi == r.end {
                x1 += (self.effective_font_size() as f64) * 0.4;
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

    // ---- multiline geometry / layout ---------------------------------------

    /// Effective body font size (per-widget override or the default).
    fn effective_font_size(&self) -> f32 {
        self.font_size_override.unwrap_or(FONT_SIZE)
    }

    /// The line-height band both text and caret share, in logical px.
    fn line_height(&self) -> f64 {
        (self.effective_font_size() * LINE_HEIGHT_RATIO) as f64
    }

    /// Total laid-out content height for the multiline path (rows × line height
    /// plus top/bottom inset), from the last `draw`/`measure`. Hosts that want
    /// the field to grow with its content read this. Falls back to one row when
    /// nothing has been laid out yet.
    pub fn content_height(&self) -> f64 {
        let lh = self.line_height();
        let rows = self.lines.len().max(1) as f64;
        rows * lh + ML_PAD_Y * 2.0
    }

    /// Line-relative x of the char boundary nearest `byte` within row `r`.
    fn x_in_row(&self, r: &VisualLine, byte: usize) -> f64 {
        let b = byte.clamp(r.start, r.end);
        let ci = self.text[r.start..b].chars().count();
        r.char_x
            .get(ci)
            .copied()
            .unwrap_or_else(|| r.char_x.last().copied().unwrap_or(0.0))
    }

    /// Byte offset of the `ci`-th char boundary within row `r`.
    fn byte_of_row_char(&self, r: &VisualLine, ci: usize) -> usize {
        match self.text[r.start..r.end].char_indices().nth(ci) {
            Some((b, _)) => r.start + b,
            None => r.end,
        }
    }

    /// Resolve `byte` to `(row index, line-relative x)` against the current
    /// visual-line layout. Handles the soft-wrap boundary: a byte that is both
    /// the end of a soft-wrapped row and the start of the next lands on the
    /// next row's left edge (so the caret reads as "start of the wrapped
    /// continuation", never a phantom position past the right edge).
    fn locate_ml(&self, byte: usize) -> (usize, f64) {
        if self.lines.is_empty() {
            return (0, 0.0);
        }
        for (i, r) in self.lines.iter().enumerate() {
            if byte >= r.start && byte < r.end {
                return (i, self.x_in_row(r, byte));
            }
        }
        for i in (0..self.lines.len()).rev() {
            let r = &self.lines[i];
            if byte >= r.start && byte <= r.end {
                if byte == r.end
                    && !r.hard_break
                    && i + 1 < self.lines.len()
                    && self.lines[i + 1].start == byte
                {
                    return (i + 1, 0.0);
                }
                return (i, self.x_in_row(r, byte));
            }
        }
        let last = self.lines.len() - 1;
        let r = &self.lines[last];
        (last, self.x_in_row(r, r.end))
    }

    /// Multiline caret rect for `byte`, in local coordinates.
    fn caret_rect_ml(&self, byte: usize) -> Rect {
        let (i, x) = self.locate_ml(byte);
        let y = self.lines.get(i).map(|r| r.y).unwrap_or(0.0);
        let cx = self.rect.x0 + self.ml_left() + x;
        let cy = self.rect.y0 + ML_PAD_Y + y - self.scroll_y;
        Rect::new(cx, cy, cx + CARET_WIDTH, cy + self.line_height())
    }

    /// Resolve a local-space point to a byte offset in multiline mode: pick the
    /// row by y (clamped to the first/last), then the nearest char boundary in
    /// that row by x. Requires a prior `draw` to have built the layout.
    fn cursor_from_point(&self, pt: Point) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        let lh = self.line_height();
        let local_y = pt.y - (self.rect.y0 + ML_PAD_Y) + self.scroll_y;
        let mut row = (local_y / lh).floor() as isize;
        row = row.clamp(0, self.lines.len() as isize - 1);
        let r = &self.lines[row as usize];
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
        // On a soft-wrapped row, snapping to the very last boundary would jump
        // the caret to the next row's start; keep it on this row.
        let is_last = row as usize == self.lines.len() - 1;
        if best + 1 == r.char_x.len() && !r.hard_break && !is_last && best > 0 {
            best -= 1;
        }
        self.byte_of_row_char(r, best)
    }

    /// Move the caret one visual row up or down, preserving the goal column
    /// across consecutive vertical moves. Multiline only; reads the last
    /// `draw`'s layout.
    fn move_vertical(&mut self, down: bool, extend: bool) {
        if self.lines.is_empty() {
            return;
        }
        let (i, cur_x) = self.locate_ml(self.cursor_pos);
        let goal = self.goal_x.unwrap_or(cur_x);
        let target = if down {
            if i + 1 < self.lines.len() {
                i + 1
            } else {
                self.set_cursor(self.text.len(), extend);
                self.goal_x = Some(goal);
                return;
            }
        } else if i > 0 {
            i - 1
        } else {
            self.set_cursor(0, extend);
            self.goal_x = Some(goal);
            return;
        };
        // Nearest char boundary in the target row to the goal x.
        let r = &self.lines[target];
        let mut best = 0usize;
        let mut best_d = f64::MAX;
        for (ci, &cx) in r.char_x.iter().enumerate() {
            let d = (goal - cx).abs();
            if d < best_d {
                best_d = d;
                best = ci;
            }
        }
        let is_last = target == self.lines.len() - 1;
        if best + 1 == r.char_x.len() && !r.hard_break && !is_last && best > 0 {
            best -= 1;
        }
        let byte = self.byte_of_row_char(r, best);
        self.set_cursor(byte, extend);
        self.goal_x = Some(goal);
    }

    /// Rebuild the visual-line layout for the current text at width `avail_w`
    /// (logical px, the inner text width). Soft-wraps each paragraph at word
    /// boundaries, falling back to a mid-word break for a single word wider
    /// than the field. Measures with the same family the draw uses so caret,
    /// selection, and hit-test stay glyph-aligned.
    fn build_lines(&mut self, tr: &mut TextRenderer, avail_w: f64) {
        let size = self.effective_font_size();
        let lh = self.line_height();
        let wrap_w = if avail_w > 1.0 { avail_w } else { f64::MAX };
        let mut lines: Vec<VisualLine> = Vec::new();
        let mut y = 0.0f64;
        let mut para_start = 0usize;
        loop {
            let rel_nl = self.text[para_start..].find('\n');
            let para_end = rel_nl.map(|i| para_start + i).unwrap_or(self.text.len());
            self.wrap_paragraph(tr, size, para_start, para_end, rel_nl.is_some(), wrap_w, lh, &mut y, &mut lines);
            match rel_nl {
                Some(i) => para_start = para_start + i + 1,
                None => break,
            }
        }
        self.lines = lines;
    }

    /// Wrap one paragraph `[start, end)` (no interior '\n') into visual rows,
    /// appending them to `out` and advancing `y`. `hard_break` marks the final
    /// row as ending at a real newline (vs. the document end).
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
        let para = &self.text[start..end];
        // Char boundaries relative to `start`, plus an end sentinel.
        let mut boundaries: Vec<usize> = para.char_indices().map(|(i, _)| i).collect();
        boundaries.push(para.len());
        // Cumulative (masked-aware) x at each boundary.
        let cum: Vec<f64> = boundaries
            .iter()
            .map(|&b| self.measure_str(tr, &self.display_slice(start, start + b), size).0 as f64)
            .collect();
        let nchars = boundaries.len() - 1;
        if nchars == 0 {
            out.push(VisualLine { start, end, hard_break, char_x: vec![0.0], y: *y });
            *y += lh;
            return;
        }
        let push_row =
            |out: &mut Vec<VisualLine>, y: &mut f64, a: usize, b: usize, hard: bool| {
                let char_x: Vec<f64> = (a..=b).map(|j| cum[j] - cum[a]).collect();
                out.push(VisualLine {
                    start: start + boundaries[a],
                    end: start + boundaries[b],
                    hard_break: hard,
                    char_x,
                    y: *y,
                });
                *y += lh;
            };
        let mut row_start_k = 0usize;
        let mut last_space_k: Option<usize> = None;
        let mut k = 0usize;
        while k < nchars {
            let row_w = cum[k + 1] - cum[row_start_k];
            if row_w > wrap_w && k > row_start_k {
                let break_k = match last_space_k {
                    Some(sp) if sp + 1 > row_start_k => sp + 1,
                    _ => k,
                };
                push_row(out, y, row_start_k, break_k, false);
                row_start_k = break_k;
                last_space_k = None;
                continue;
            }
            if &para[boundaries[k]..boundaries[k + 1]] == " " {
                last_space_k = Some(k);
            }
            k += 1;
        }
        push_row(out, y, row_start_k, nchars, hard_break);
    }

    /// Draw one run of body text in whatever family the input is configured
    /// for (explicit family override wins, else mono when `self.mono`, else
    /// sans) — the same choice `measure_str` makes, so glyphs stay aligned
    /// with the measured caret/selection metrics.
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
            tr.draw_text_font(scene, text, size, color, origin, None, TextWeight::Regular, false, fam);
        } else {
            tr.draw_text_styled(scene, text, size, color, origin, None, TextWeight::Regular, false, self.mono);
        }
    }

    /// The multiline body: rebuild the wrapped layout, follow the caret with
    /// vertical scroll, then paint per-row selection, text, placeholder, and
    /// caret inside a clip of the field rect. `shape` is the field's rounded
    /// clip (already used for the chrome).
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

        // Vertical scroll follows the caret while focused; otherwise pin to the
        // top so the first line is always visible.
        let (caret_row, _) = self.locate_ml(self.cursor_pos);
        let caret_top = self.lines.get(caret_row).map(|r| r.y).unwrap_or(0.0);
        let caret_bot = caret_top + lh;
        let view_h = self.rect.height() - ML_PAD_Y * 2.0;
        // Only re-centre on the caret when it actually moved (edit / arrow key);
        // a wheel scroll leaves the caret put, so it must not snap back.
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
        let content_h = self.lines.len() as f64 * lh;
        let max_scroll = (content_h - view_h).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);

        scene.push_layer(vello::peniko::Fill::NonZero, BlendMode::default(), 1.0, transform, shape);

        // Which logical line each visual row belongs to — one shared pass drives
        // the current-line band and the gutter numbering (a row starts a new
        // logical line iff the previous row ended at a hard `\n`).
        let mut line_of_row: Vec<usize> = Vec::with_capacity(self.lines.len());
        let mut cur_line = 0usize;
        for i in 0..self.lines.len() {
            if i > 0 && self.lines[i - 1].hard_break {
                cur_line += 1;
            }
            line_of_row.push(cur_line);
        }
        let caret_line = self.text[..self.cursor_pos.min(self.text.len())]
            .bytes()
            .filter(|&b| b == b'\n')
            .count();

        // Gutter background + divider (drawn first, under everything).
        if gutter_w > 0.0 {
            let gr = Rect::new(self.rect.x0, self.rect.y0, self.rect.x0 + gutter_w, self.rect.y1);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.bg_hover.with_alpha_factor(0.35),
                None,
                &gr,
            );
            let x = self.rect.x0 + gutter_w;
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &vello::kurbo::Line::new((x, self.rect.y0), (x, self.rect.y1)),
            );
        }

        // Current-line band across the content width (behind selection + text).
        if self.highlight_current_line && self.is_focused && !self.shows_placeholder() {
            let band_color = theme.bg_hover.with_alpha_factor(0.6);
            for i in 0..self.lines.len() {
                if line_of_row[i] != caret_line {
                    continue;
                }
                let top = self.lines[i].y - self.scroll_y;
                if top + lh < 0.0 || top > view_h {
                    continue;
                }
                let y0 = self.rect.y0 + ML_PAD_Y + top;
                let band = Rect::new(self.rect.x0 + gutter_w, y0, self.rect.x1, y0 + lh);
                scene.fill(vello::peniko::Fill::NonZero, transform, band_color, None, &band);
            }
        }

        // Selection (per visual row), painted under the glyphs.
        if let Some((s, e)) = self.selection_range() {
            let sel_color = theme.accent_primary.with_alpha_factor(0.35);
            for r in self.selection_rects(theme, s, e) {
                scene.fill(vello::peniko::Fill::NonZero, transform, sel_color, None, &r);
            }
        }

        // Placeholder when empty + unfocused.
        if self.shows_placeholder() {
            let origin = transform
                * Affine::translate((self.rect.x0 + left, self.rect.y0 + ML_PAD_Y - self.scroll_y));
            tr.draw_text(scene, &self.placeholder, size, theme.text_dim, origin, None);
        }

        let text_color = if self.disabled { theme.text_dim } else { theme.text_primary };
        // Collect per-row draw data (owned) so all `self` borrows end before the
        // draw calls. Each row becomes a list of coloured segments (text, line-
        // relative x, colour): highlighter spans mapped to theme colours, gaps in
        // the default text colour. Plus the gutter number of each logical line's
        // first row.
        type Seg = (String, f64, vello::peniko::Color);
        let mut row_draws: Vec<(f64, Vec<Seg>)> = Vec::new();
        let mut gutter_nums: Vec<(f64, String, bool)> = Vec::new();
        for i in 0..self.lines.len() {
            let top = self.lines[i].y - self.scroll_y;
            if top + lh < 0.0 || top > view_h {
                continue;
            }
            let li = line_of_row[i];
            if gutter_w > 0.0 && (i == 0 || self.lines[i - 1].hard_break) {
                gutter_nums.push((self.lines[i].y, (li + 1).to_string(), li == caret_line));
            }
            let segs = self.row_segments(i, li, text_color, theme);
            if !segs.is_empty() {
                row_draws.push((self.lines[i].y, segs));
            }
        }

        // Gutter line numbers (right-aligned inside the gutter).
        if gutter_w > 0.0 {
            for (ry, num, current) in &gutter_nums {
                let w = self.measure_str(tr, num, size).0 as f64;
                let x = self.rect.x0 + gutter_w - GUTTER_PAD_R - w;
                let origin = transform
                    * Affine::translate((x, self.rect.y0 + ML_PAD_Y + ry - self.scroll_y));
                let col = if *current { theme.text_primary } else { theme.text_dim };
                self.draw_text_run(scene, tr, num, size, col, origin);
            }
        }

        // Body glyphs.
        for (ry, segs) in &row_draws {
            for (text, x, color) in segs {
                if text.is_empty() {
                    continue;
                }
                let origin = transform
                    * Affine::translate((
                        self.rect.x0 + left + x,
                        self.rect.y0 + ML_PAD_Y + ry - self.scroll_y,
                    ));
                self.draw_text_run(scene, tr, text, size, *color, origin);
            }
        }

        if self.is_focused && !self.disabled {
            let caret = self.caret_rect_ml(self.cursor_pos);
            scene.fill(vello::peniko::Fill::NonZero, transform, theme.accent_primary, None, &caret);
        }

        scene.pop_layer();
    }

    /// Build the coloured draw segments for visual row `i` (on logical line
    /// `line_idx`). With no highlighter the whole row is one `default`-coloured
    /// segment; with one, the row's byte span is split against the logical
    /// line's [`Span`]s, each coloured via `token_color`, gaps kept `default`.
    /// Returned `x` is line-relative (add [`ml_left`] + `rect.x0` to place it).
    fn row_segments(
        &self,
        i: usize,
        line_idx: usize,
        default: vello::peniko::Color,
        theme: &Theme,
    ) -> Vec<(String, f64, vello::peniko::Color)> {
        // Local newtype-free tuple; caller wraps into `Seg`.
        let r = &self.lines[i];
        let (rs, re) = (r.start, r.end);
        let mut out: Vec<(String, f64, vello::peniko::Color)> = Vec::new();
        if self.highlighter.is_some()
            && line_idx < self.hl_spans.len()
            && line_idx < self.hl_line_starts.len()
        {
            let ls = self.hl_line_starts[line_idx];
            let mut cur = rs;
            for sp in &self.hl_spans[line_idx] {
                let ss = ls + sp.range.start;
                let se = ls + sp.range.end;
                let a = ss.max(rs).max(cur);
                let b = se.min(re);
                if a >= b {
                    continue;
                }
                if a > cur {
                    out.push((self.text[cur..a].to_string(), self.x_in_row(r, cur), default));
                }
                out.push((self.text[a..b].to_string(), self.x_in_row(r, a), token_color(sp.kind, theme)));
                cur = b;
            }
            if cur < re {
                out.push((self.text[cur..re].to_string(), self.x_in_row(r, cur), default));
            }
        } else {
            let text = self.display_slice(rs, re);
            if !text.is_empty() {
                out.push((text, 0.0, default));
            }
        }
        out
    }

    /// Test-only: the coloured draw segments for every visual row, exactly as
    /// [`draw_multiline_body`] paints them (same `row_segments` + `token_color`
    /// path). Requires a prior `draw` to have built the visual lines + the
    /// highlight cache. Lets `CodeEditor`'s tests assert that highlighter spans
    /// reach the real renderer as themed colours.
    #[cfg(test)]
    pub(crate) fn code_row_colors(
        &self,
        theme: &Theme,
    ) -> Vec<(String, vello::peniko::Color)> {
        let text_color = if self.disabled { theme.text_dim } else { theme.text_primary };
        let mut cur_line = 0usize;
        let mut out = Vec::new();
        for i in 0..self.lines.len() {
            if i > 0 && self.lines[i - 1].hard_break {
                cur_line += 1;
            }
            for (t, _x, c) in self.row_segments(i, cur_line, text_color, theme) {
                out.push((t, c));
            }
        }
        out
    }

    /// One-shot read of user edits. Returns `Some(text)` if any
    /// user-initiated edit has landed since the last call, else
    /// `None`. The dirty flag is cleared.
    pub fn take_dirty(&mut self) -> Option<String> {
        if self.dirty {
            self.dirty = false;
            Some(self.text.clone())
        } else {
            None
        }
    }

    /// External-layout assignment for callers that embed this widget
    /// outside the taffy tree (the parent owns layout). Pairs with the
    /// trait method `apply_layout`: a parent calling this is asserting
    /// it has done what `apply_layout` would normally do. Today the
    /// only invariant `apply_layout` would establish is `self.rect`; if
    /// `apply_layout` grows responsibilities in the future, this method
    /// will need to grow with it.
    ///
    /// Embed bypasses `draw_with_presence` too — only safe with
    /// local-mode TextInputs that have no presence path or permission
    /// scope.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub fn with_id(mut self, id: &str) -> Self {
        self._id = id.to_string();
        self
    }

    pub fn with_on_change_path(mut self, path: &str) -> Self {
        self.on_change_path = path.to_string();
        self.crdt_bound = !path.is_empty();
        self
    }

    /// Enable password masking: glyphs render as `•` bullets while the
    /// stored CRDT value remains the real text. No-op visual change for
    /// the default (`false`) so existing callers are unaffected.
    pub fn with_secret(mut self, on: bool) -> Self {
        self.secret = on;
        self
    }

    /// Mutate the secret (password-masking) flag after construction.
    pub fn set_secret(&mut self, on: bool) {
        self.secret = on;
    }

    /// Whether this input masks its value (password mode).
    pub fn is_secret(&self) -> bool {
        self.secret
    }

    /// Builder-style: start a secret field with its glyphs revealed (the eye
    /// toggle on). Only meaningful together with [`with_secret`]. Even when
    /// revealed the plaintext is kept out of the a11y tree and `text_review`;
    /// revealing shows the characters on screen only.
    pub fn with_reveal(mut self, on: bool) -> Self {
        self.reveal = on;
        self
    }

    /// Mutate the reveal flag after construction.
    pub fn set_reveal(&mut self, on: bool) {
        self.reveal = on;
    }

    /// Flip the reveal flag (drives an eye toggle button). No-op on a
    /// non-secret field.
    pub fn toggle_reveal(&mut self) {
        if self.secret {
            self.reveal = !self.reveal;
        }
    }

    /// Whether a secret field is currently showing its real glyphs.
    pub fn is_revealed(&self) -> bool {
        self.reveal
    }

    /// Builder-style: make this a multi-line editor. Newlines become real
    /// characters, `Enter` inserts one, `Up`/`Down` move by visual line,
    /// `Home`/`End` act per visual line, selection spans lines, and text
    /// soft-wraps at the field width. Unblocks mail/chat compose bodies,
    /// slides notes, etc. The default (`false`) is the single-line field.
    pub fn multiline(mut self, on: bool) -> Self {
        self.multiline = on;
        self
    }

    /// Mutate the multiline flag after construction.
    pub fn set_multiline(&mut self, on: bool) {
        self.multiline = on;
    }

    /// Whether this input is in multi-line mode.
    pub fn is_multiline(&self) -> bool {
        self.multiline
    }

    // ---- code-editor mode (gutter + syntax highlight + soft tabs) ----------
    // These power [`CodeEditor`](crate::widget::CodeEditor); every setter
    // defaults off so plain fields are unaffected.

    /// Builder: draw a line-number gutter (multiline only).
    pub fn with_gutter(mut self, on: bool) -> Self {
        self.gutter = on;
        self
    }

    /// Toggle the line-number gutter after construction.
    pub fn set_gutter(&mut self, on: bool) {
        self.gutter = on;
    }

    /// Whether the line-number gutter is enabled.
    pub fn has_gutter(&self) -> bool {
        self.gutter
    }

    /// Builder: draw a faint band across the caret's logical line.
    pub fn with_current_line_highlight(mut self, on: bool) -> Self {
        self.highlight_current_line = on;
        self
    }

    /// Toggle the current-line highlight after construction.
    pub fn set_current_line_highlight(&mut self, on: bool) {
        self.highlight_current_line = on;
    }

    /// Builder: install a syntax [`Highlighter`](crate::widget::code_editor::Highlighter).
    pub fn with_highlighter(mut self, h: Box<dyn Highlighter>) -> Self {
        self.highlighter = Some(h);
        self.hl_sig = u64::MAX; // force a rebuild on next draw
        self
    }

    /// Install / clear the syntax highlighter after construction.
    pub fn set_highlighter(&mut self, h: Option<Box<dyn Highlighter>>) {
        self.highlighter = h;
        self.hl_sig = u64::MAX;
    }

    /// Builder: number of spaces `Tab` inserts (`0` = literal `\t`). Enables
    /// Tab interception; `Shift+Tab` outdents by the same width.
    pub fn with_tab_width(mut self, n: usize) -> Self {
        self.tab_spaces = Some(n);
        self
    }

    /// Set the soft-tab width after construction (`0` = literal `\t`).
    pub fn set_tab_width(&mut self, n: usize) {
        self.tab_spaces = Some(n);
    }

    /// Width of the line-number gutter in logical px (`0` when disabled). A pure
    /// function of the logical line count + font size, so it is stable between
    /// `draw`s and identical for geometry and painting.
    fn code_gutter_w(&self) -> f64 {
        if !self.gutter || !self.multiline {
            return 0.0;
        }
        let lines = self.text.bytes().filter(|&b| b == b'\n').count() + 1;
        let digits = ((lines as f64).log10().floor() as usize + 1).max(2);
        let adv = self.effective_font_size() as f64 * GUTTER_DIGIT_RATIO;
        GUTTER_PAD_L + digits as f64 * adv + GUTTER_PAD_R
    }

    /// Left inset of the multiline text area: the shared pad plus any gutter.
    /// Every multiline geometry helper (caret, selection, hit-test, draw) reads
    /// this so the gutter shifts them all consistently.
    fn ml_left(&self) -> f64 {
        ML_PAD_X + self.code_gutter_w()
    }

    /// Rebuild the per-line highlight cache when the text changed and a
    /// highlighter is installed. Threads the cross-line carry over all lines so
    /// block comments / fenced code stay coherent.
    fn refresh_highlight(&mut self) {
        let Some(hl) = &self.highlighter else {
            self.hl_spans.clear();
            self.hl_line_starts.clear();
            return;
        };
        // Cheap signature: length + FNV-1a over the bytes. Recompute only on a
        // real change (tokenising is skipped otherwise).
        let mut sig: u64 = 0xcbf29ce484222325;
        for &b in self.text.as_bytes() {
            sig ^= b as u64;
            sig = sig.wrapping_mul(0x100000001b3);
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
            start += line.len() + 1; // + '\n'
        }
    }

    /// Builder-style: make a plain `Enter` (no Shift) fire a one-shot submit
    /// signal instead of inserting a newline. In multiline mode `Shift+Enter`
    /// still inserts a newline (the chat-composer pattern); in single-line mode
    /// this lets `Enter` accept the field. Read the signal with [`take_submit`].
    pub fn with_enter_submits(mut self, on: bool) -> Self {
        self.enter_submits = on;
        self
    }

    /// Mutate the enter-submits flag after construction.
    pub fn set_enter_submits(&mut self, on: bool) {
        self.enter_submits = on;
    }

    /// One-shot read of the submit signal: `true` exactly once after the user
    /// pressed `Enter` while [`enter_submits`] was set. Clears on read.
    pub fn take_submit(&mut self) -> bool {
        std::mem::take(&mut self.submit_requested)
    }

    /// Builder-style: mark the input disabled. A disabled input is inert —
    /// not focusable, ignores key / pointer input, and renders dimmed. The
    /// default (`false`) leaves the input fully interactive, so existing
    /// callers are unaffected.
    pub fn disabled(mut self, on: bool) -> Self {
        self.disabled = on;
        self
    }

    /// Mutate the disabled flag after construction. When transitioning to
    /// disabled, any in-progress drag selection is dropped so the widget
    /// doesn't hold pointer capture while inert.
    pub fn set_disabled(&mut self, on: bool) {
        self.disabled = on;
        if on {
            self.is_dragging = false;
            self.is_hovered = false;
        }
    }

    /// Whether the input is currently disabled (inert).
    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    /// Builder-style: set the placeholder hint shown in dim text while the
    /// input is empty AND unfocused. Pass `""` to clear it. The default
    /// (empty) shows no placeholder, so existing callers are unaffected.
    pub fn with_placeholder(mut self, hint: &str) -> Self {
        self.placeholder = hint.to_string();
        self
    }

    /// Mutate the placeholder after construction.
    pub fn set_placeholder(&mut self, hint: &str) {
        self.placeholder = hint.to_string();
    }

    /// The placeholder hint, or `""` when none is set.
    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }

    /// Whether the placeholder hint should be painted this frame — true
    /// only when a non-empty placeholder is set and the input is both
    /// empty and unfocused (native form-field semantics).
    pub fn shows_placeholder(&self) -> bool {
        !self.placeholder.is_empty() && self.text.is_empty() && !self.is_focused
    }

    /// Whether glyphs are masked this frame — true for a secret field that is
    /// not currently revealed by the eye toggle.
    fn masked(&self) -> bool {
        self.secret && !self.reveal
    }

    /// The string actually painted / measured. For a masked (secret,
    /// unrevealed) input this is one bullet per *character* of the real text;
    /// otherwise it's the real text itself.
    fn display_text(&self) -> String {
        if self.masked() {
            "•".repeat(self.text.chars().count())
        } else {
            self.text.clone()
        }
    }

    /// Display string for the prefix `self.text[..byte_idx]` — used to
    /// keep `char_x_positions` (which is keyed by real-text byte offsets)
    /// aligned with the masked glyph run, so caret placement and click
    /// resolution stay correct under masking.
    fn display_prefix(&self, byte_idx: usize) -> String {
        if self.masked() {
            "•".repeat(self.text[..byte_idx].chars().count())
        } else {
            self.text[..byte_idx].to_string()
        }
    }

    /// The display string for the byte range `self.text[start..end]` — masked
    /// bullets when secret+unrevealed, else the real slice. Used by the
    /// multiline layout, which measures and paints one visual row at a time.
    fn display_slice(&self, start: usize, end: usize) -> String {
        if self.masked() {
            "•".repeat(self.text[start..end].chars().count())
        } else {
            self.text[start..end].to_string()
        }
    }

    /// Sorted `(start, end)` byte indices of the live selection, or
    /// `None` when nothing is selected (anchor missing or collapsed
    /// against the caret).
    fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        if anchor == self.cursor_pos { return None; }
        Some((anchor.min(self.cursor_pos), anchor.max(self.cursor_pos)))
    }

    fn has_selection(&self) -> bool {
        self.selection_range().is_some()
    }

    /// Substring of `text` covered by the live selection.
    fn selected_str(&self) -> Option<&str> {
        self.selection_range().map(|(s, e)| &self.text[s..e])
    }

    /// Move the caret to `pos`. When `extend` is true the existing
    /// selection anchor is preserved (or the current cursor is
    /// promoted to anchor if there's no active selection); when
    /// false, the selection is collapsed.
    fn set_cursor(&mut self, pos: usize, extend: bool) {
        let pos = pos.min(self.text.len());
        if extend {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor_pos);
            }
        } else {
            self.selection_anchor = None;
        }
        self.cursor_pos = pos;
        // Any explicit caret move resets the vertical goal column; a following
        // Up/Down establishes a fresh one. `move_vertical` re-sets it after
        // calling here so vertical runs still track their column.
        self.goal_x = None;
    }

    /// Delete the current selection from both the local string and
    /// the CRDT. No-op if there's no selection. In local mode
    /// (`obj_id` is `None`), only the local string is updated.
    fn delete_selection(&mut self, state: &mut AutoCommit, obj_id: Option<&ObjId>) {
        let Some((start, end)) = self.selection_range() else { return; };
        let char_start = self.text[..start].chars().count();
        let char_count = self.text[start..end].chars().count();
        self.text.replace_range(start..end, "");
        self.cursor_pos = start;
        self.selection_anchor = None;
        self.goal_x = None;
        self.last_edit_time = Some(Instant::now());
        if let Some(obj_id) = obj_id {
            let _ = state.splice_text(obj_id, char_start, char_count as isize, "");
        }
        self.dirty = true;
    }

    /// Insert `s` at the caret, replacing the current selection if
    /// any. The CRDT delta and the local cursor are kept in sync.
    /// In local mode (`obj_id` is `None`), only the local string is
    /// updated.
    fn insert_str(&mut self, s: &str, state: &mut AutoCommit, obj_id: Option<&ObjId>) {
        // Char index where the inserted text lands, captured so we can
        // stamp an authorship mark over exactly the inserted run.
        let insert_char_at;
        if let Some((start, end)) = self.selection_range() {
            let char_start = self.text[..start].chars().count();
            let char_count = self.text[start..end].chars().count();
            self.text.replace_range(start..end, s);
            self.cursor_pos = start + s.len();
            self.selection_anchor = None;
            if let Some(obj_id) = obj_id {
                let _ = state.splice_text(obj_id, char_start, char_count as isize, s);
            }
            insert_char_at = char_start;
        } else {
            let char_idx = self.text[..self.cursor_pos].chars().count();
            self.text.insert_str(self.cursor_pos, s);
            if let Some(obj_id) = obj_id {
                let _ = state.splice_text(obj_id, char_idx, 0, s);
            }
            self.cursor_pos += s.len();
            insert_char_at = char_idx;
        }
        // Authorship: stamp the inserted run with the local user's id so
        // peers can highlight "what they wrote" in the author's color.
        // Latest-wins per position, so a peer typing into another's text
        // splits the prior author's run correctly.
        if let (Some(obj_id), Some(author)) = (obj_id, &self.local_author) {
            let nchars = s.chars().count();
            if nchars > 0 {
                let mark = automerge::marks::Mark::new(
                    AUTHOR_MARK.to_string(),
                    author.clone(),
                    insert_char_at,
                    insert_char_at + nchars,
                );
                let _ = state.mark(obj_id, mark, automerge::marks::ExpandMark::None);
            }
        }
        self.goal_x = None;
        self.last_edit_time = Some(Instant::now());
        self.dirty = true;
    }

    /// Outdent the caret's logical line: drop up to `width` leading spaces (or
    /// a single leading `\t`). Splices through the CRDT exactly like every other
    /// edit and pulls the caret left by the removed amount. `width` `0` removes
    /// one leading whitespace. No-op when the line has no leading indent.
    fn outdent(&mut self, width: usize, state: &mut AutoCommit, obj_id: Option<&ObjId>) {
        let line_start = self.text[..self.cursor_pos]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let w = if width == 0 { 1 } else { width };
        let mut remove = 0usize; // bytes to remove (leading spaces are 1 byte each)
        for c in self.text[line_start..].chars() {
            if c == '\t' {
                remove = 1;
                break;
            } else if c == ' ' && remove < w {
                remove += 1;
            } else {
                break;
            }
        }
        if remove == 0 {
            return;
        }
        let del_end = line_start + remove;
        let char_start = self.text[..line_start].chars().count();
        let char_count = self.text[line_start..del_end].chars().count();
        self.text.replace_range(line_start..del_end, "");
        if let Some(obj_id) = obj_id {
            let _ = state.splice_text(obj_id, char_start, char_count as isize, "");
        }
        self.cursor_pos = self.cursor_pos.saturating_sub(remove).max(line_start);
        self.selection_anchor = None;
        self.goal_x = None;
        self.last_edit_time = Some(Instant::now());
        self.dirty = true;
    }

    /// Whether the user is mid-drag — public for BlockEditor's cross-
    /// block drag coordinator, which checks if any inner TextInput is
    /// still dragging to decide whether to paint multi-block bands.
    pub fn is_dragging(&self) -> bool {
        self.is_dragging
    }

    /// Force the selection to span `[anchor, cursor)` from outside the
    /// input. Used by BlockEditor's cross-block drag coordinator to
    /// paint a selection band on a block the user is dragging
    /// THROUGH without that block ever having been the click target.
    /// `cursor` becomes the active end; `anchor == cursor` clears.
    pub fn set_selection(&mut self, anchor: usize, cursor: usize) {
        let a = anchor.min(self.text.len());
        let c = cursor.min(self.text.len());
        if a == c {
            self.selection_anchor = None;
        } else {
            self.selection_anchor = Some(a);
        }
        self.cursor_pos = c;
    }

    /// Snap a logical-space x to the byte position in this input's text
    /// — public counterpart to the private `cursor_from_x` for callers
    /// that need to hit-test from outside the widget (BlockEditor's
    /// cross-block drag).
    pub fn byte_at_x(&self, logical_x: f64) -> usize {
        self.cursor_from_x(logical_x)
    }

    // ---- right-click context menu ------------------------------------------

    /// Copy the current selection to the clipboard (no-op when nothing is
    /// selected). Shared by the Ctrl-C path and the menu's **Copy** row.
    fn do_copy(&self) {
        if let Some(s) = self.selected_str() {
            clipboard_copy(s);
        }
    }

    /// Cut the current selection: copy it, then delete it. No-op without a
    /// selection. Shared by the Ctrl-X path and the menu's **Cut** row.
    fn do_cut(&mut self, state: &mut AutoCommit, obj_id: Option<&ObjId>) {
        if let Some(s) = self.selected_str() {
            let owned = s.to_string();
            clipboard_copy(&owned);
            self.delete_selection(state, obj_id);
        }
    }

    /// Paste clipboard text at the caret (replacing the selection). A
    /// single-line field strips all control characters; a multiline field
    /// keeps real newlines (normalising `\r\n`/`\r` to `\n`) and tabs, so a
    /// pasted paragraph lands intact. Shared by the Ctrl-V path and the
    /// menu's **Paste** row.
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

    /// Select the whole field (no-op when empty). Shared by the Ctrl-A path and
    /// the menu's **Select All** row.
    fn do_select_all(&mut self) {
        if !self.text.is_empty() {
            self.selection_anchor = Some(0);
            self.cursor_pos = self.text.len();
        }
    }

    /// Public select-all: span the whole field (anchor at 0, caret at end).
    /// No-op when empty. Used by wrapper widgets that want "focus selects
    /// the value" semantics — e.g. [`NumberInput`](crate::widget::NumberInput)
    /// selects its number on focus so the first keystroke replaces it.
    pub fn select_all(&mut self) {
        self.do_select_all();
    }

    /// Build (and open) the right-click context menu at `pos`.
    ///
    /// If nothing is selected, the word under the click is selected first
    /// (matches every native editor's right-click-on-a-word behaviour). Cut and
    /// Copy are shown only when there is a selection; Paste only when the
    /// clipboard holds non-empty text (when that can be determined). The parallel
    /// `context_actions` list lets a left-click on a row dispatch by index.
    fn open_context_menu(&mut self, pos: [f32; 2]) {
        // Clicked-word fallback: select the word under the caret-from-point when
        // there's no live selection, so Cut/Copy act on something sensible.
        if !self.has_selection() {
            let byte = self.resolve_cursor(Point::new(pos[0] as f64, pos[1] as f64));
            let (start, end) = word_boundaries(&self.text, byte);
            if start != end {
                self.selection_anchor = Some(start);
                self.cursor_pos = end;
            } else {
                self.cursor_pos = byte.min(self.text.len());
            }
        }

        let has_sel = self.has_selection();
        let can_paste = clipboard_paste().is_some_and(|s| !s.is_empty());

        let mut menu = ContextMenu::new();
        let mut actions: Vec<Option<CtxAction>> = Vec::new();
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

    /// Dispatch the context-menu row at `idx` (mapped through `context_actions`)
    /// against this input, then close the menu. Out-of-range / separator rows
    /// are ignored. Returns `true` when a row was dispatched.
    fn dispatch_context_action(
        &mut self,
        idx: usize,
        state: &mut AutoCommit,
        obj_id: Option<&ObjId>,
    ) -> bool {
        let Some(Some(action)) = self.context_actions.get(idx).copied() else {
            return false;
        };
        match action {
            CtxAction::Cut => self.do_cut(state, obj_id),
            CtxAction::Copy => self.do_copy(),
            CtxAction::Paste => self.do_paste(state, obj_id),
            CtxAction::SelectAll => self.do_select_all(),
        }
        self.context_menu.close();
        true
    }

    /// Test-visible labels of the currently-open context menu's rows
    /// (separators omitted), in order. Empty when the menu is closed.
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

    /// Resolve the cursor byte-position from a logical-space x point
    /// inside the input's rect.
    fn cursor_from_x(&self, logical_x: f64) -> usize {
        let padding_x = 10.0;
        let relative_x = logical_x - (self.rect.x0 + padding_x) + self.scroll_x;
        let mut best_dist = f64::MAX;
        let mut best_idx = 0;
        for (i, &x_pos) in self.char_x_positions.iter().enumerate() {
            if i == self.text.len() || self.text.is_char_boundary(i) {
                let dist = (relative_x - x_pos).abs();
                if dist < best_dist {
                    best_dist = dist;
                    best_idx = i;
                }
            }
        }
        best_idx
    }

    /// Resolve a pointer position to a byte offset, dispatching by mode:
    /// multiline uses the 2D row+column hit-test, single-line the horizontal
    /// one. Both require a prior `draw` to have built their metrics.
    fn resolve_cursor(&self, pt: Point) -> usize {
        if self.multiline {
            self.cursor_from_point(pt)
        } else {
            self.cursor_from_x(pt.x)
        }
    }

    fn clamp_to_text(&mut self) {
        if self.cursor_pos > self.text.len() {
            self.cursor_pos = self.text.len();
        }
        while self.cursor_pos > 0 && !self.text.is_char_boundary(self.cursor_pos) {
            self.cursor_pos -= 1;
        }
        if let Some(a) = self.selection_anchor {
            let mut a = a.min(self.text.len());
            while a > 0 && !self.text.is_char_boundary(a) { a -= 1; }
            self.selection_anchor = Some(a);
        }
    }
}

impl Widget for TextInput {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // Directly-bound Text wins: read its content straight from the
        // object, ignoring the string-path mode entirely.
        if let Some(obj_id) = self.bound_text.clone() {
            if let Ok(text) = state.text(&obj_id) {
                self.text = text;
            }
            self.clamp_to_text();
            return;
        }
        if !self.crdt_bound { return; }
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
                _ => {}
            }
        }
        self.clamp_to_text();
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        // A multiline field is measured (it grows with its content, see
        // `measure`); a single-line field is a fixed 40 px row.
        let height = if self.multiline {
            Dimension::auto()
        } else {
            Dimension::length(40.0)
        };
        let node = taffy.new_leaf(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height,
            },
            ..Default::default()
        }).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    /// A multiline field is measured so it grows with its content: wrap at the
    /// resolved width, report the wrapped height. Single-line fields keep the
    /// fixed row height from `build_layout` (return ZERO here, the trait
    /// default). Only hit for taffy-hosted inputs — embedded callers use
    /// `apply_layout_external` and never reach `measure`.
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
        let avail_w = (width as f64 - self.ml_left() - ML_PAD_X).max(1.0);
        self.build_lines(text_renderer, avail_w);
        taffy::Size { width, height: self.content_height() as f32 }
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &crate::presence::PresenceCtx) {
        let shape = RoundedRect::from_rect(self.rect, theme.tokens.radii.md as f64);
        if !self.plain {
            let bg_color = if self.disabled {
                // Sunken surface reads as "inert" — one step darker than the
                // raised form-field fill.
                theme.tokens.colors.surfaces.base
            } else if self.is_focused {
                theme.bg_active
            } else {
                theme.tokens.colors.surfaces.raised
            };
            scene.fill(vello::peniko::Fill::NonZero, transform, bg_color, None, &shape);
            let border_color = if self.disabled {
                theme.tokens.colors.border_standard
            } else if self.is_focused {
                theme.tokens.colors.border_focus
            } else {
                theme.tokens.colors.border_standard
            };
            scene.stroke(
                &Stroke::new(if self.is_focused && !self.disabled { 2.0 } else { 1.0 }),
                transform,
                border_color,
                None,
                &shape,
            );
        }

        // Multiline shares the chrome above, then takes its own body path
        // (soft-wrapped rows, vertical scroll, per-row selection).
        if self.multiline {
            self.draw_multiline_body(scene, transform, text_renderer, theme, &shape);
            return;
        }

        let text_size = self.font_size_override.unwrap_or(FONT_SIZE);
        let line_height = (text_size * LINE_HEIGHT_RATIO) as f64;
        let padding_x = if self.plain {
            // Plain-chrome inputs (block editor cells) don't need the
            // inset gutter — the parent already adds whatever gutter
            // the document layout wants. Saves ~10 px per side.
            0.0
        } else {
            theme.padding_standard as f64
        };

        // The painted/measured string — bullets when masked, real text
        // otherwise. `char_x_positions` stays keyed by *real-text* byte
        // offsets (so the caret/click logic below is unchanged) but the
        // measured widths come from the masked glyph run.
        let display = self.display_text();

        // Pre-calculate positions for exact caret placement and click resolution.
        self.char_x_positions.clear();
        self.char_x_positions.resize(self.text.len() + 1, 0.0);
        for (byte_idx, _) in self.text.char_indices() {
            let (w, _) = self.measure_str(text_renderer, &self.display_prefix(byte_idx), text_size);
            self.char_x_positions[byte_idx] = w as f64;
        }
        let (total_w, _) = self.measure_str(text_renderer, &display, text_size);
        self.char_x_positions[self.text.len()] = total_w as f64;

        // Caret + scroll math.
        let cursor_pos = self.cursor_pos.min(self.text.len());
        let cursor_x_offset = if cursor_pos < self.char_x_positions.len() {
            self.char_x_positions[cursor_pos]
        } else {
            total_w as f64
        };

        let visible_width = self.rect.width() - padding_x * 2.0;
        if self.is_focused {
            // Follow the caret horizontally while editing.
            if cursor_x_offset - self.scroll_x < 0.0 {
                self.scroll_x = cursor_x_offset;
            } else if cursor_x_offset - self.scroll_x > visible_width {
                self.scroll_x = cursor_x_offset - visible_width;
            }
        } else {
            // Unfocused: always render from the start. A block-editor cell
            // parks its caret at the end after load, so caret-following
            // scroll would push the first letters off-screen — exactly the
            // "paragraph doesn't start at the first letter" bug.
            self.scroll_x = 0.0;
        }

        scene.push_layer(vello::peniko::Fill::NonZero, BlendMode::default(), 1.0, transform, &shape);

        // Vertical centering: position the *line_height* band in the
        // middle of the rect so glyph descenders + caret share the
        // same visual axis. `text_y` is the top of the line.
        let text_y = self.rect.y0 + (self.rect.height() - line_height) / 2.0;

        // Selection background — paint before the text so glyphs sit on
        // top. Same geometry helper peers use, so local + remote match.
        if let Some((sel_start, sel_end)) = self.selection_range() {
            let sel_rect = self.selection_rect(theme, sel_start, sel_end);
            // Translucent accent so the underlying text reads through.
            let sel_color = theme.accent_primary.with_alpha_factor(0.35);
            scene.fill(vello::peniko::Fill::NonZero, transform, sel_color, None, &sel_rect);
        }

        let text_color = if self.disabled {
            theme.text_dim
        } else {
            theme.text_primary
        };
        // Placeholder hint: painted in dim text in the body slot when the
        // input is empty and unfocused. `display` is empty here so the
        // two never overlap; the placeholder shares the same baseline and
        // gutter as real text so the transition is seamless on focus.
        if self.shows_placeholder() {
            text_renderer.draw_text(
                scene,
                &self.placeholder,
                text_size,
                theme.text_dim,
                transform * Affine::translate((self.rect.x0 + padding_x, text_y)),
                None,
            );
        }
        let text_origin =
            transform * Affine::translate((self.rect.x0 + padding_x - self.scroll_x, text_y));
        if let Some(fam) = &self.family {
            text_renderer.draw_text_font(
                scene,
                &display,
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
                &display,
                text_size,
                text_color,
                text_origin,
                None,
                TextWeight::Regular,
                false,
                self.mono,
            );
        }

        if self.is_focused && !self.disabled {
            // Same caret geometry peers render through `caret_rect`, just
            // painted in the local accent colour.
            let cursor_rect = self.caret_rect(theme, cursor_pos);
            scene.fill(vello::peniko::Fill::NonZero, transform, theme.accent_primary, None, &cursor_rect);
        }

        scene.pop_layer();
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, modifiers: &Modifiers, state: &mut AutoCommit, root: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, _taffy: &TaffyTree<()>) {
        // A disabled input is inert: swallow nothing, mutate nothing, never
        // grab focus. Bailing here is the single guard that makes both
        // pointer and keyboard input no-ops.
        if self.disabled {
            return;
        }
        self.clamp_to_text();

        match event {
            // Wheel scroll for a multiline field with content taller than the
            // view (a code editor). Only when hovered, so it never steals the
            // wheel from a surrounding scroll region unless the pointer is over
            // the editable body.
            WindowEvent::MouseWheel { delta, .. }
                if self.multiline && self.is_hovered && !self.disabled =>
            {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => -(*y as f64) * self.line_height(),
                    winit::event::MouseScrollDelta::PixelDelta(p) => -p.y,
                };
                let before = self.scroll_y;
                self.scroll_by(dy);
                if (self.scroll_y - before).abs() > f64::EPSILON {
                    *handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let logical_pos = Point::new(position.x / scale_factor, position.y / scale_factor);
                let hover = self.rect.contains(logical_pos);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *handled = true;
                }
                self.last_mouse_pos = logical_pos;

                if self.is_dragging {
                    let raw = self.resolve_cursor(logical_pos);
                    let (anchor_lo, anchor_hi) = self.drag_anchor_span;
                    // Snap the pointer position + pick the anchor end
                    // so the selection always *contains* the original
                    // span clicked. Standard "extend selection by
                    // word/line" behaviour.
                    let (new_anchor, new_cursor) = match self.drag_unit {
                        DragUnit::Char => {
                            // Plain char-level drag: anchor stays put
                            // (the position the press landed on), cursor
                            // follows the pointer.
                            (anchor_lo, raw)
                        }
                        DragUnit::Word => {
                            // Extend by whole words, keeping the double-clicked
                            // word inside the selection — the shared rule the
                            // reader uses too (`text_selection::extend_span_drag`).
                            let (w_start, w_end) = word_boundaries(&self.text, raw);
                            extend_span_drag(anchor_lo, anchor_hi, raw, w_start, w_end)
                        }
                        DragUnit::All => {
                            // Whole content is already selected; drag is
                            // a no-op.
                            (anchor_lo, anchor_hi)
                        }
                    };
                    if new_anchor != self.cursor_pos
                        || self.selection_anchor != Some(new_anchor)
                        || new_cursor != self.cursor_pos
                    {
                        self.selection_anchor = Some(new_anchor);
                        self.cursor_pos = new_cursor;
                        *handled = true;
                    }
                }
            }
            // Right (secondary) click opens the editing context menu. A press
            // selects the word under the click (when nothing is selected) and
            // anchors the menu at the pointer; the menu paints in the overlay
            // pass so it floats above the field.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } if self.is_hovered && !self.disabled => {
                if !self.is_focused {
                    *focus_target = Some(node_id);
                }
                let pos = [self.last_mouse_pos.x as f32, self.last_mouse_pos.y as f32];
                self.open_context_menu(pos);
                *handled = true;
            }
            WindowEvent::MouseInput { state: element_state, button: MouseButton::Left, .. } => {
                // While the context menu is open, a left press is routed to it:
                // a row dispatches its action; a click outside dismisses it.
                // Either way the click is consumed so it never falls through to
                // caret placement underneath the menu.
                if self.context_menu.is_open() {
                    if *element_state == ElementState::Pressed {
                        let point = [self.last_mouse_pos.x, self.last_mouse_pos.y];
                        if let Some(idx) = self.context_menu.item_at(point) {
                            // Cut / Paste mutate, so seed the backing object.
                            let obj_id_opt = self.resolve_or_seed_text_obj(state, root);
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
                        if !self.is_focused {
                            *focus_target = Some(node_id);
                        }

                        // Multi-click ladder: single → double (word) → triple (all).
                        let now = Instant::now();
                        let within_time = self.last_click_time
                            .is_some_and(|t| now.duration_since(t) <= MULTI_CLICK_INTERVAL);
                        let within_radius =
                            (self.last_click_pos - self.last_mouse_pos).hypot() <= MULTI_CLICK_RADIUS;
                        self.click_count = if within_time && within_radius {
                            self.click_count + 1
                        } else {
                            1
                        };
                        self.last_click_time = Some(now);
                        self.last_click_pos = self.last_mouse_pos;

                        let pos = self.resolve_cursor(self.last_mouse_pos);
                        match self.click_count {
                            1 => {
                                if modifiers.state().shift_key() {
                                    // Shift+click extends the existing
                                    // selection (or starts one from the
                                    // current caret) to the clicked
                                    // position. Drag continues at
                                    // char-granularity from the original
                                    // anchor.
                                    let anchor = self
                                        .selection_anchor
                                        .unwrap_or(self.cursor_pos);
                                    self.selection_anchor = Some(anchor);
                                    self.cursor_pos = pos;
                                    self.drag_unit = DragUnit::Char;
                                    self.drag_anchor_span = (anchor, anchor);
                                    self.is_dragging = true;
                                } else {
                                    self.selection_anchor = None;
                                    self.cursor_pos = pos;
                                    self.drag_unit = DragUnit::Char;
                                    self.drag_anchor_span = (pos, pos);
                                    self.is_dragging = true;
                                }
                            }
                            2 => {
                                // Double-click: select the word + arm a
                                // word-granularity drag so dragging
                                // beyond the word extends by whole words.
                                let (start, end) = word_boundaries(&self.text, pos);
                                self.selection_anchor = Some(start);
                                self.cursor_pos = end;
                                self.drag_unit = DragUnit::Word;
                                self.drag_anchor_span = (start, end);
                                self.is_dragging = true;
                            }
                            _ => {
                                // Triple-click: select all + arm an
                                // all-granularity drag (no-op extension
                                // for single-line, but the flag flow
                                // stays uniform).
                                self.selection_anchor = Some(0);
                                self.cursor_pos = self.text.len();
                                self.drag_unit = DragUnit::All;
                                self.drag_anchor_span = (0, self.text.len());
                                self.is_dragging = true;
                            }
                        }
                        *handled = true;
                    }
                    ElementState::Released => {
                        if self.is_dragging {
                            self.is_dragging = false;
                            *handled = true;
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::KeyboardInput { event: KeyEvent { logical_key, text, state: element_state, .. }, .. } => {
                if *element_state != ElementState::Pressed {
                    return;
                }
                // Escape dismisses an open context menu first (even if the field
                // isn't focused, since the menu is the focused surface).
                if self.context_menu.is_open()
                    && matches!(logical_key, Key::Named(NamedKey::Escape))
                {
                    self.context_menu.close();
                    *handled = true;
                    return;
                }
                if !self.is_focused {
                    return;
                }
                // A key that can mutate the buffer resolves-or-seeds the backing
                // Text object so the edit actually persists (see
                // `resolve_or_seed_text_obj`); pure navigation uses the read-only
                // resolver so arrow/Home/End never create a document object.
                let will_mutate = matches!(
                    logical_key,
                    Key::Named(NamedKey::Backspace)
                        | Key::Named(NamedKey::Delete)
                        | Key::Named(NamedKey::Enter)
                        | Key::Named(NamedKey::Tab)
                        | Key::Named(NamedKey::Space)
                        | Key::Character(_)
                ) || text
                    .as_ref()
                    .is_some_and(|t| !t.as_str().chars().all(|c| c.is_control()));
                let obj_id_opt: Option<ObjId> = if will_mutate {
                    self.resolve_or_seed_text_obj(state, root)
                } else {
                    self.resolve_text_obj(state, root)
                };
                let shift = modifiers.state().shift_key();
                let ctrl = modifiers.state().control_key();

                // While the inline emoji-completion overlay is open it owns
                // navigation + accept, ahead of the field's own Arrow/Enter/Tab.
                if self.emoji_complete && self.ec.active {
                    match logical_key {
                        Key::Named(NamedKey::Escape) => {
                            self.ec.active = false;
                            *handled = true;
                            return;
                        }
                        Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Tab) => {
                            self.accept_emoji_completion(state, obj_id_opt.as_ref());
                            *handled = true;
                            return;
                        }
                        Key::Named(NamedKey::ArrowDown) => {
                            let n = self.ec.matches.len();
                            if n > 0 {
                                self.ec.selected = (self.ec.selected + 1) % n;
                            }
                            *handled = true;
                            return;
                        }
                        Key::Named(NamedKey::ArrowUp) => {
                            let n = self.ec.matches.len();
                            if n > 0 {
                                self.ec.selected = (self.ec.selected + n - 1) % n;
                            }
                            *handled = true;
                            return;
                        }
                        _ => {}
                    }
                }

                match logical_key {
                    Key::Named(NamedKey::ArrowLeft) => {
                        let new_pos = if ctrl {
                            prev_word_boundary(&self.text, self.cursor_pos)
                        } else {
                            prev_char_boundary(&self.text, self.cursor_pos)
                        };
                        // Plain arrow with active selection collapses
                        // to the selection edge instead of moving one
                        // character past it — matches every native
                        // editor's behaviour.
                        if !shift && !ctrl {
                            if let Some((s, _)) = self.selection_range() {
                                self.cursor_pos = s;
                                self.selection_anchor = None;
                                *handled = true;
                                return;
                            }
                        }
                        self.set_cursor(new_pos, shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        let new_pos = if ctrl {
                            next_word_boundary(&self.text, self.cursor_pos)
                        } else {
                            next_char_boundary(&self.text, self.cursor_pos)
                        };
                        if !shift && !ctrl {
                            if let Some((_, e)) = self.selection_range() {
                                self.cursor_pos = e;
                                self.selection_anchor = None;
                                *handled = true;
                                return;
                            }
                        }
                        self.set_cursor(new_pos, shift);
                        *handled = true;
                    }
                    // Vertical caret motion, multiline only. Moves by *visual*
                    // row (from the last draw's wrapped layout), preserving the
                    // goal column across a run of Up/Down.
                    Key::Named(NamedKey::ArrowUp) if self.multiline => {
                        self.move_vertical(false, shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowDown) if self.multiline => {
                        self.move_vertical(true, shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::Home) => {
                        // Multiline Home goes to the start of the current visual
                        // row; single-line to the field start.
                        let target = if self.multiline && !self.lines.is_empty() {
                            let (i, _) = self.locate_ml(self.cursor_pos);
                            self.lines[i].start
                        } else {
                            0
                        };
                        self.set_cursor(target, shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::End) => {
                        // Multiline End goes to the end of the current visual
                        // row; single-line to the field end.
                        let target = if self.multiline && !self.lines.is_empty() {
                            let (i, _) = self.locate_ml(self.cursor_pos);
                            self.lines[i].end
                        } else {
                            self.text.len()
                        };
                        self.set_cursor(target, shift);
                        *handled = true;
                    }
                    // Enter: in enter-submit mode a plain Enter fires the submit
                    // signal (Shift+Enter still inserts a newline in multiline);
                    // otherwise a multiline field inserts a real newline. A
                    // single-line field with no submit binding ignores Enter,
                    // exactly as before.
                    Key::Named(NamedKey::Enter) => {
                        if self.enter_submits && !shift {
                            self.submit_requested = true;
                            *handled = true;
                        } else if self.multiline {
                            self.insert_str("\n", state, obj_id_opt.as_ref());
                            *handled = true;
                        }
                    }
                    // Soft-tab / outdent — only when a tab width was set
                    // (`with_tab_width`), i.e. code-editor mode. Untouched
                    // otherwise so the host keeps Tab for focus traversal.
                    Key::Named(NamedKey::Tab) if self.tab_spaces.is_some() => {
                        let width = self.tab_spaces.unwrap_or(0);
                        if shift {
                            self.outdent(width, state, obj_id_opt.as_ref());
                        } else if width == 0 {
                            self.insert_str("\t", state, obj_id_opt.as_ref());
                        } else {
                            let spaces = " ".repeat(width);
                            self.insert_str(&spaces, state, obj_id_opt.as_ref());
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::Backspace) => {
                        if self.has_selection() {
                            self.delete_selection(state, obj_id_opt.as_ref());
                        } else if self.cursor_pos > 0 {
                            let start = if ctrl {
                                prev_word_boundary(&self.text, self.cursor_pos)
                            } else {
                                prev_char_boundary(&self.text, self.cursor_pos)
                            };
                            let end = self.cursor_pos;
                            let char_start = self.text[..start].chars().count();
                            let char_count = self.text[start..end].chars().count();
                            self.text.replace_range(start..end, "");
                            self.cursor_pos = start;
                            self.last_edit_time = Some(Instant::now());
                            if let Some(obj_id) = obj_id_opt.as_ref() {
                                let _ = state.splice_text(obj_id, char_start, char_count as isize, "");
                            }
                            self.dirty = true;
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::Delete) => {
                        if self.has_selection() {
                            self.delete_selection(state, obj_id_opt.as_ref());
                        } else if self.cursor_pos < self.text.len() {
                            let start = self.cursor_pos;
                            let end = if ctrl {
                                next_word_boundary(&self.text, self.cursor_pos)
                            } else {
                                next_char_boundary(&self.text, self.cursor_pos)
                            };
                            let char_start = self.text[..start].chars().count();
                            let char_count = self.text[start..end].chars().count();
                            self.text.replace_range(start..end, "");
                            self.last_edit_time = Some(Instant::now());
                            if let Some(obj_id) = obj_id_opt.as_ref() {
                                let _ = state.splice_text(obj_id, char_start, char_count as isize, "");
                            }
                            self.dirty = true;
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::Space) => {
                        self.insert_str(" ", state, obj_id_opt.as_ref());
                        *handled = true;
                    }
                    Key::Character(c) if ctrl => {
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
                                self.do_cut(state, obj_id_opt.as_ref());
                                *handled = true;
                            }
                            "v" => {
                                self.do_paste(state, obj_id_opt.as_ref());
                                *handled = true;
                            }
                            _ => {}
                        }
                    }
                    _ => {
                        if let Some(t) = text {
                            if !t.as_str().chars().all(|c| c.is_control()) {
                                self.insert_str(t.as_str(), state, obj_id_opt.as_ref());
                                *handled = true;
                            }
                        }
                    }
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(t)) => {
                if !self.is_focused {
                    return;
                }
                if !t.chars().all(|c| c.is_control()) {
                    let obj_id_opt: Option<ObjId> = self.resolve_or_seed_text_obj(state, root);
                    self.insert_str(t.as_str(), state, obj_id_opt.as_ref());
                    *handled = true;
                }
            }
            _ => {}
        }

        // Re-derive the inline emoji-completion overlay after any edit / caret
        // move (opt-in; a cheap no-op otherwise).
        if self.emoji_complete {
            let obj = self.resolve_text_obj(state, root);
            self.refresh_emoji_completion(state, obj.as_ref());
        }
    }

    fn name(&self) -> &str { "TextInput" }

    fn test_id(&self) -> Option<&str> {
        if self._id.is_empty() {
            Some("text_input")
        } else {
            Some(self._id.as_str())
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// A single-line field scrolls its content horizontally rather than growing,
    /// so once laid out its box never needs a taffy re-solve on a mere
    /// reposition (e.g. a sibling scroll). It reports needing a measure only
    /// before its first layout, or right after a user edit (which `dirty`
    /// marks) in case a host auto-sizes it — so it never blocks the App's
    /// scroll fast path while idle.
    fn layout_needs_measure(&self) -> bool {
        self.rect == Rect::ZERO || self.dirty
    }

    /// An editable text field shows the I-beam over its area. A disabled
    /// input is inert (it swallows no input) so it claims no cursor and
    /// defers to the host default. `point` is in the same logical space
    /// as `rect`.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if !self.disabled && self.rect.contains(point) {
            Some(winit::window::CursorIcon::Text)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool { !self.disabled }
    fn set_focus(&mut self, focused: bool) { self.is_focused = focused; }
    fn is_focused(&self) -> bool { self.is_focused }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // `is_enabled()` is not overridden on TextInput (the inert flag is its
        // own `disabled` field), so derive the a11y state from `disabled`
        // directly. A secret (password) input never leaks its value into the
        // a11y tree — it reports `protected` and a `None` value instead.
        let value = (!self.secret).then(|| self.text.clone());
        Some(
            AccessNode::new(Role::TextInput)
                .with_optional_value(value)
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused(),
                    disabled: self.disabled,
                    protected: self.secret,
                    multiline: self.multiline,
                    ..Default::default()
                }),
        )
    }

    /// While dragging the selection or while a multi-click chain is
    /// still alive, the input wants to keep receiving cursor events
    /// even when the cursor wanders into a sibling widget. Matches
    /// the same pattern as `Slider::wants_pointer_capture`.
    fn wants_pointer_capture(&self) -> bool {
        self.is_dragging
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
        Some(self.cursor_pos.to_string())
    }

    fn set_state(&mut self, state: &str) {
        if let Ok(pos) = state.parse::<usize>() {
            self.cursor_pos = pos;
        }
    }

    fn text_review(&self) -> Option<(usize, String)> {
        // Only the focused field is being reviewed; a secret field never
        // surfaces its contents, so no caret review either.
        if !self.is_focused || self.secret {
            return None;
        }
        Some((self.cursor_pos, self.text.clone()))
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        if self.path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget { path: self.path.clone() })
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
        if presence.should_cull(self.get_rect()) { return; }
        self.draw(scene, transform, text_renderer, theme, presence);

        if let Some(key) = self.presence_key() {
            // Peer caret + selection use the SAME geometry as this input's
            // own caret (`caret_rect`/`selection_rect`), so a remote caret
            // is pixel-identical to the local one — never a re-derived
            // approximation. `transform` maps the local rect to the scene.

            // Fading "just typed" highlighter under the caret/selection.
            let now = std::time::Instant::now();
            for (user, range, alpha) in presence.typing_marks_for_key(&key, now) {
                let rect = self.selection_rect(theme, range.start, range.end);
                let p0 = transform * vello::kurbo::Point::new(rect.x0, rect.y0);
                let p1 = transform * vello::kurbo::Point::new(rect.x1, rect.y1);
                presence.draw_peer_typing(
                    scene,
                    theme,
                    &user,
                    &[(vello::kurbo::Rect::new(p0.x, p0.y, p1.x, p1.y), alpha)],
                );
            }
            if let Some(list) = presence.carets.get(&key) {
                for (user, caret) in list.iter() {
                    // Labeled peer caret: bar + a small name chip above it.
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
                for (user, range) in list.iter() {
                    // Multiline spans multiple rows → one peer highlight per
                    // row; single-line resolves to exactly one rect.
                    for r in self.selection_rects(theme, range.start, range.end) {
                        presence.draw_peer_selection(scene, theme, transform, user, r);
                    }
                }
            }
        }

        if let Some(scope) = self.permission_scope() {
            if !permissions.check(&scope) {
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
    }

    /// Top-layer pass: paint the right-click context menu (when open) so it
    /// floats above every sibling regardless of tree order. Delegates to the
    /// shared [`ContextMenu`] primitive's own `draw`.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        if self.context_menu.is_open() {
            Widget::draw(&mut self.context_menu, scene, transform, text_renderer, theme, presence);
        }
        self.draw_emoji_overlay(scene, transform, text_renderer, theme);
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

    /// Polish-sweep #127 pin: the permission-denied dim must paint at
    /// the same alpha across every widget.
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
    fn cursor_shape_is_text_over_rect() {
        // An enabled input shows the I-beam over its editable area and
        // defers elsewhere.
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

        // A disabled input is inert — it swallows no input, so it claims
        // no cursor even when the pointer is over its rect.
        let mut off = TextInput::local().disabled(true);
        off.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));
        assert_eq!(
            Widget::cursor_shape(&off, Point::new(20.0, 20.0)),
            None,
            "disabled input claims no cursor",
        );
    }

    /// The peer caret and the local caret are the SAME geometry: a real
    /// caret rect (full line height, CARET_WIDTH wide, advancing with the
    /// text via per-char metrics) — NOT a fixed 2px×16px bar at the block
    /// origin. This is the contract that makes a remote caret look
    /// identical to the local one, only recoloured.
    #[test]
    fn caret_rect_is_real_caret_geometry() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 24.0));
        let theme = Theme::dark();
        let presence = crate::presence::PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        // A draw populates char_x_positions that caret_rect reads.
        Widget::draw(&mut ti, &mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let r0 = ti.caret_rect(&theme, 0);
        let r5 = ti.caret_rect(&theme, 5);
        let (_text_y, line_height) = ti.text_band();

        assert!((r0.width() - 2.0).abs() < 0.01, "caret is CARET_WIDTH wide");
        assert!(
            (r0.height() - line_height).abs() < 0.01,
            "caret spans the full line band, not a fixed 16px"
        );
        assert!(line_height > 16.0, "body line height is >16 (so the old fixed 16px was wrong)");
        assert!(r5.x0 > r0.x0, "caret advances with byte position via real glyph metrics");
    }

    /// An unfocused input must render from the start (scroll_x == 0) even
    /// when its caret is parked at the end of a line wider than the rect —
    /// otherwise caret-following scroll hides the first characters (the
    /// "paragraph doesn't start at the first letter" bug).
    #[test]
    fn unfocused_input_renders_from_start_not_caret() {
        let long = "a very long line that easily exceeds a narrow input width so caret-following scroll would otherwise hide the beginning";
        let mut ti = TextInput::local().with_initial_value(long);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 120.0, 24.0)); // narrow
        ti.set_cursor_pos(long.len()); // caret at the very end
        // Not focused (the default).
        let theme = Theme::dark();
        let presence = crate::presence::PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        Widget::draw(&mut ti, &mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        assert_eq!(ti.scroll_x(), 0.0, "unfocused input must not scroll the start off-screen");

        // Focused: caret-following scroll IS expected for a long line.
        ti.set_focus(true);
        Widget::draw(&mut ti, &mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        assert!(ti.scroll_x() > 0.0, "focused input follows the caret on a long line");
    }

    /// A disabled input must drop out of the focus order so keyboard nav
    /// (Tab / arrows) skips it — mirrors `ActionButton::disabled`.
    #[test]
    fn disabled_input_is_not_focusable() {
        let enabled = TextInput::new(Vec::new());
        assert!(Widget::is_focusable(&enabled), "default input is focusable");

        let off = TextInput::new(Vec::new()).disabled(true);
        assert!(!Widget::is_focusable(&off), "disabled input must not be focusable");

        // The setter path agrees with the builder.
        let mut later = TextInput::new(Vec::new());
        later.set_disabled(true);
        assert!(!Widget::is_focusable(&later));
        assert!(later.is_disabled());
    }

    /// Build a printable-character `KeyboardInput`. `KeyEvent` has a
    /// `pub(crate)` platform field so it can't be struct-literalled from a
    /// test; the all-zero pattern + field overwrite is the same approach
    /// `modal_dialog::tests::key_press_event` uses.
    fn char_key_event(s: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(winit::keyboard::SmolStr::new(s));
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
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

    /// A disabled input must ignore keyboard input entirely — no text is
    /// spliced and the dirty flag stays clear (so the parent never sees a
    /// phantom edit).
    #[test]
    fn disabled_input_ignores_keyboard_input() {
        let mut ti = TextInput::local().disabled(true);
        ti.set_focus(true); // even if somehow focused, the input is inert
        ti.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));

        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
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

        // Sanity: the SAME event DOES edit an enabled, focused input — proves
        // the bail-out (not some unrelated wiring) is what suppressed it.
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

    /// A disabled input must ignore pointer input: a click over it does not
    /// request focus (no `focus_target`) and does not consume the event.
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
        let node = taffy.new_leaf(Style::default()).unwrap();

        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(20.0, 20.0),
        };
        ti.handle_event(&moved, 1.0, &Modifiers::default(), &mut doc, &automerge::ROOT, &mut handled, &mut focus_target, node, &taffy);
        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        ti.handle_event(&press, 1.0, &Modifiers::default(), &mut doc, &automerge::ROOT, &mut handled, &mut focus_target, node, &taffy);

        assert!(!handled, "disabled input must not consume the click");
        assert!(focus_target.is_none(), "disabled input must not request focus");
    }

    #[test]
    fn text_input_test_id_can_be_overridden() {
        let ti = TextInput::new(Vec::new()).with_id("search_box");
        assert_eq!(Widget::test_id(&ti), Some("search_box"));
    }

    /// Opt-in `:shortcode` completion opens an overlay while a token is typed
    /// and, on Enter, replaces the token with the chosen emoji glyph.
    #[test]
    fn emoji_completion_replaces_token_on_accept() {
        let mut ti = TextInput::local().with_emoji_completion();
        ti.set_focus(true);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        let mut doc = AutoCommit::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let feed = |ti: &mut TextInput, ev: WindowEvent, doc: &mut AutoCommit, taffy: &TaffyTree<()>| {
            let mut h = false;
            let mut f = None;
            ti.handle_event(&ev, 1.0, &Modifiers::default(), doc, &automerge::ROOT, &mut h, &mut f, node, taffy);
        };
        for s in [":", "s", "m", "i"] {
            feed(&mut ti, char_key_event(s), &mut doc, &taffy);
        }
        assert!(ti.emoji_completion_open(), "typing ':smi' opens the emoji overlay");
        assert!(
            ti.ec.matches.iter().any(|e| e.name == "smile"),
            "':smi' should suggest :smile:",
        );
        feed(&mut ti, named_key_event(NamedKey::Enter), &mut doc, &taffy);
        assert!(!ti.emoji_completion_open(), "accepting closes the overlay");
        assert!(
            !ti.value().contains(':'),
            "the :token was replaced (no colon left): {:?}",
            ti.value(),
        );
        assert_eq!(
            ti.value().chars().count(),
            1,
            "value is a single emoji glyph: {:?}",
            ti.value(),
        );
    }

    /// Typing the closing colon of an exact shortcode auto-replaces it.
    #[test]
    fn emoji_completion_auto_replaces_exact_shortcode() {
        let mut ti = TextInput::local().with_emoji_completion();
        ti.set_focus(true);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        let mut doc = AutoCommit::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        for s in [":", "f", "i", "r", "e", ":"] {
            let mut h = false;
            let mut f = None;
            ti.handle_event(
                &char_key_event(s), 1.0, &Modifiers::default(), &mut doc, &automerge::ROOT,
                &mut h, &mut f, node, &taffy,
            );
        }
        assert_eq!(
            ti.value(),
            "\u{1f525}",
            "typing ':fire:' auto-replaces with the fire emoji",
        );
    }

    /// Emoji completion is opt-in: a plain field never opens the overlay.
    #[test]
    fn emoji_completion_off_by_default() {
        let mut ti = TextInput::local();
        ti.set_focus(true);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        let mut doc = AutoCommit::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        for s in [":", "s", "m", "i"] {
            let mut h = false;
            let mut f = None;
            ti.handle_event(
                &char_key_event(s), 1.0, &Modifiers::default(), &mut doc, &automerge::ROOT,
                &mut h, &mut f, node, &taffy,
            );
        }
        assert!(!ti.emoji_completion_open(), "default field never opens emoji completion");
        assert_eq!(ti.value(), ":smi", "plain field just holds the literal text");
    }

    /// Drive a right (secondary) mouse press at `(x, y)` against `ti`, returning
    /// whether the event was consumed. Sends a `CursorMoved` first so
    /// `last_mouse_pos` / `is_hovered` are set, matching the real input order.
    #[cfg(test)]
    fn right_click(ti: &mut TextInput, doc: &mut AutoCommit, x: f64, y: f64) -> bool {
        use winit::dpi::PhysicalPosition;
        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(x, y),
        };
        ti.handle_event(&moved, 1.0, &Modifiers::default(), doc, &automerge::ROOT, &mut handled, &mut focus_target, node, &taffy);
        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Right,
        };
        ti.handle_event(&press, 1.0, &Modifiers::default(), doc, &automerge::ROOT, &mut handled, &mut focus_target, node, &taffy);
        handled
    }

    /// A right-click with no live selection selects the word under the click
    /// (the clicked-word fallback) and opens a menu carrying the expected rows.
    #[test]
    fn right_click_selects_word_and_opens_menu_with_expected_labels() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        assert!(ti.selection_range().is_none(), "precondition: no selection");
        assert!(right_click(&mut ti, &mut doc, 20.0, 20.0), "right-click is consumed");

        // No char_x_positions yet → cursor_from_x resolves to byte 0, so the
        // clicked-word fallback selects the first word "hello".
        assert_eq!(ti.selected_str(), Some("hello"), "clicked word is selected");
        assert!(ti.context_menu.is_open(), "menu opened");

        // With a selection + an empty clipboard in the headless test env, Paste
        // is omitted, so the rows are Cut, Copy, (separator), Select All.
        let labels = ti.context_menu_labels();
        assert!(labels.contains(&"Cut".to_string()), "got {labels:?}");
        assert!(labels.contains(&"Copy".to_string()), "got {labels:?}");
        assert!(labels.contains(&"Select All".to_string()), "got {labels:?}");
    }

    /// `item_at` + dispatch maps the right row to Copy: clicking the Copy row
    /// runs the copy action (keeping the selection) and closes the menu.
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
        let copy_idx = labels.iter().position(|l| l == "Copy").expect("Copy row present");
        // Map the visible label index back to the real item index (labels skip
        // separators, but Cut/Copy precede any separator so indices align here).
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

        assert!(ti.dispatch_context_action(item_idx, &mut doc, None), "dispatch ran");
        assert!(!ti.context_menu.is_open(), "menu closed after dispatch");
        // Copy preserves the selection (it's a read-only action).
        assert_eq!(ti.selected_str(), Some("world"));
    }

    /// Cut dispatches to the cut action: it removes the selected text.
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

    /// A left-click outside the open menu dismisses it without placing a caret.
    #[test]
    fn left_click_outside_open_menu_dismisses_it() {
        use winit::dpi::PhysicalPosition;
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        ti.set_focus(true);
        let mut doc = AutoCommit::new();
        ti.open_context_menu([10.0, 18.0]);
        assert!(ti.context_menu.is_open());

        // Left-press far from the menu rect.
        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(280.0, 38.0),
        };
        ti.handle_event(&moved, 1.0, &Modifiers::default(), &mut doc, &automerge::ROOT, &mut handled, &mut focus_target, node, &taffy);
        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        ti.handle_event(&press, 1.0, &Modifiers::default(), &mut doc, &automerge::ROOT, &mut handled, &mut focus_target, node, &taffy);
        assert!(handled, "click consumed while menu open");
        assert!(!ti.context_menu.is_open(), "menu dismissed by outside click");
    }

    /// The placeholder shows ONLY when the input is empty AND unfocused.
    /// Typing text or gaining focus must hide it — native form semantics.
    #[test]
    fn placeholder_visible_only_when_empty_and_unfocused() {
        let mut ti = TextInput::local().with_placeholder("Search…");
        assert_eq!(ti.placeholder(), "Search…");
        // Empty + unfocused → shown.
        assert!(ti.shows_placeholder(), "empty unfocused input shows placeholder");

        // Focused → hidden even while empty.
        ti.set_focus(true);
        assert!(!ti.shows_placeholder(), "focused input hides placeholder");

        // Unfocused but non-empty → hidden.
        ti.set_focus(false);
        ti.set_text("hello");
        assert!(!ti.shows_placeholder(), "non-empty input hides placeholder");

        // Back to empty + unfocused → shown again.
        ti.set_text("");
        assert!(ti.shows_placeholder());

        // No placeholder set → never shown.
        let plain = TextInput::local();
        assert!(!plain.shows_placeholder(), "no placeholder set → never shows");
    }

    /// The placeholder draw path emits a glyph run when (and only when) it
    /// should be visible — proves the dim hint actually paints.
    #[test]
    fn placeholder_emits_glyphs_when_shown() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = crate::presence::PresenceCtx::empty();

        let mut shown = TextInput::local().with_placeholder("Type here");
        shown.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));
        let mut scene = Scene::new();
        Widget::draw(&mut shown, &mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        assert!(
            !scene.encoding().resources.glyph_runs.is_empty(),
            "placeholder must paint a glyph run when shown",
        );

        // Focused empty input: placeholder hidden → no glyph run.
        let mut hidden = TextInput::local().with_placeholder("Type here");
        hidden.set_focus(true);
        hidden.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));
        let mut scene2 = Scene::new();
        Widget::draw(&mut hidden, &mut scene2, Affine::IDENTITY, &mut tr, &theme, &presence);
        assert!(
            scene2.encoding().resources.glyph_runs.is_empty(),
            "focused input must not paint the placeholder",
        );
    }

    /// Walking back through a sentence with `prev_word_boundary` must
    /// land on the start of each word. Word membership uses
    /// `is_word_char`, so the spaces between words count as
    /// boundaries and the cursor doesn't get stuck mid-word.
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

    /// Mirror of the above for the forward direction. Note that
    /// `next_word_boundary` lands *past* the trailing space — that's
    /// how every native editor handles "Ctrl + Right".
    #[test]
    fn next_word_boundary_walks_word_ends() {
        let s = "hello world quick ui";
        // After "hello" the cursor lands at index 6 (start of "world").
        assert_eq!(next_word_boundary(s, 0), 6);
        assert_eq!(next_word_boundary(s, 6), 12);
        assert_eq!(next_word_boundary(s, 12), 18);
        assert_eq!(next_word_boundary(s, 18), s.len());
        assert_eq!(next_word_boundary(s, s.len()), s.len());
    }

    /// `word_boundaries` should snap to the word the cursor is sitting
    /// *inside* — and at a space immediately after a word, to the word
    /// on the left (matches every native editor's double-click-on-
    /// trailing-space behaviour). Used by double-click word selection.
    #[test]
    fn word_boundaries_snaps_to_word_bounds() {
        let s = "hello world";
        assert_eq!(word_boundaries(s, 2), (0, 5));   // mid-word
        assert_eq!(word_boundaries(s, 0), (0, 5));   // word start
        assert_eq!(word_boundaries(s, 5), (0, 5));   // space → snap left
        assert_eq!(word_boundaries(s, 6), (6, 11));  // start of next word
        assert_eq!(word_boundaries(s, 11), (6, 11)); // end → snap left
    }

    /// Mock the bare minimum to drive a selection delete through
    /// `delete_selection`: stub doc, no CRDT roundtrip required for
    /// the local-text contract.
    #[test]
    fn delete_selection_removes_range_and_collapses() {
        let mut ti = TextInput::new(Vec::new()).with_on_change_path("text_1");
        ti.text = "hello world".to_string();
        ti.selection_anchor = Some(6);
        ti.cursor_pos = 11;
        // Use a synthetic AutoCommit + Text object so `splice_text`
        // succeeds; the test's contract is local-text only.
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

    /// `insert_at_cursor` splices programmatic text at the caret of a
    /// local-mode field and marks it dirty — the path a host uses to insert
    /// non-keyboard text (e.g. a picked emoji) as if typed.
    #[test]
    fn insert_at_cursor_splices_and_marks_dirty() {
        let mut ti = TextInput::local().with_initial_value("hi world");
        ti.set_cursor_pos(2); // just after "hi"
        ti.insert_at_cursor(" there");
        assert_eq!(ti.value(), "hi there world");
        assert_eq!(ti.cursor_pos(), "hi there".len(), "caret advances past the insert");
        assert_eq!(ti.take_dirty().as_deref(), Some("hi there world"), "the edit is observable");
    }

    /// `insert_str` over an active selection should *replace* it,
    /// not append. This is the path Ctrl+V uses when pasting over
    /// selected text.
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

    /// Password masking: a `secret` input must render bullets (one per
    /// character) while the CRDT-backed `text` keeps the *real* value —
    /// auth code reads `self.text`, the screen shows `•`. Drives a real
    /// Automerge `Text` object through `sync` to prove the stored secret
    /// is intact end-to-end.
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

        // sync pulls the real value out of the CRDT.
        ti.sync(&mut doc, &automerge::ROOT);
        assert_eq!(ti.text, "hunter2", "stored value must be the real secret");

        // What gets painted/measured is a bullet per character.
        assert_eq!(ti.display_text(), "•••••••");
        // The masked prefix tracks character count, not bytes.
        assert_eq!(ti.display_prefix(3), "•••");

        // A non-secret input is unaffected (no behaviour change).
        let mut plain = TextInput::new(Vec::new());
        plain.text = "hunter2".to_string();
        assert_eq!(plain.display_text(), "hunter2");
        assert_eq!(plain.display_prefix(3), "hun");
    }

    #[test]
    fn accessibility_reports_text_input_role_and_value() {
        use crate::a11y::Role;
        let mut ti = TextInput::new(Vec::new());
        ti.text = "hello".to_string();
        let node = Widget::accessibility(&ti).unwrap();
        assert_eq!(node.role, Role::TextInput);
        assert_eq!(node.value.as_deref(), Some("hello"));
        assert!(node.state.focusable, "enabled input is focusable");
        assert!(!node.state.disabled);
        assert!(!node.state.protected);
    }

    #[test]
    fn accessibility_secret_input_masks_value_and_sets_protected() {
        let mut ti = TextInput::new(Vec::new()).with_secret(true);
        ti.text = "hunter2".to_string();
        let node = Widget::accessibility(&ti).unwrap();
        assert_eq!(node.value, None, "a secret value must not leak into the a11y tree");
        assert!(node.state.protected, "secret input is marked protected");
    }

    #[test]
    fn accessibility_disabled_input_reports_disabled_and_not_focusable() {
        let ti = TextInput::new(Vec::new()).disabled(true);
        let node = Widget::accessibility(&ti).unwrap();
        assert!(node.state.disabled);
        assert!(!node.state.focusable, "disabled input is dropped from focus order");
    }

    /// The fix for visual-gap "text below center" pins the text top
    /// against `line_height` (= `FONT_SIZE * LINE_HEIGHT_RATIO`),
    /// *not* against `FONT_SIZE` itself. Without this, the cosmic-
    /// text baseline shifts the glyphs ~3 px below the caret in a
    /// 40 px input.
    #[test]
    fn text_baseline_centred_with_caret() {
        let rect_h = 40.0_f32;
        let line_height = FONT_SIZE * LINE_HEIGHT_RATIO;
        let text_y_correct = (rect_h - line_height) / 2.0;
        let text_y_naive = (rect_h - FONT_SIZE) / 2.0;
        // The naive computation lands ~2 px below the correct one for
        // 16 px text → that's the "below center" the user reported.
        assert!(text_y_naive > text_y_correct);
        assert!(
            (text_y_naive - text_y_correct - (line_height - FONT_SIZE) / 2.0).abs() < 0.01
        );
    }

    // ---- selection on the production `handle_event` path --------------------
    //
    // The word-boundary helpers are unit-tested above; these prove the
    // *widget* wires them up correctly — every behaviour is driven through
    // `handle_event`, never by poking private fields, so a regression in the
    // event routing (not just the helper) is caught.

    /// Build a `Modifiers` with the requested ctrl / shift state.
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

    /// A `WindowEvent::KeyboardInput` for a named key (arrows, Home/End).
    fn named_key_event(k: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(k);
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
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

    /// Drive one event through `handle_event`, returning whether it was
    /// consumed. Uses a throwaway taffy node — the input embeds no children.
    fn drive(ti: &mut TextInput, doc: &mut AutoCommit, ev: &WindowEvent, m: &Modifiers) -> bool {
        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        ti.handle_event(
            ev, 1.0, m, doc, &automerge::ROOT, &mut handled, &mut focus_target, node, &taffy,
        );
        handled
    }

    /// Run one `draw` so `char_x_positions` is populated (mouse hit-testing
    /// needs it). Unfocused, so `scroll_x` stays 0 and click math is simple.
    fn draw_to_populate(ti: &mut TextInput) {
        let theme = Theme::dark();
        let presence = crate::presence::PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        Widget::draw(ti, &mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    /// Logical x that resolves (via `cursor_from_x`) to byte `b`. Mirrors the
    /// handler's own `padding_x = 10.0` and assumes `scroll_x == 0`.
    fn x_for_byte(ti: &TextInput, b: usize) -> f64 {
        ti.rect.x0 + 10.0 + ti.char_x_positions()[b]
    }

    /// Shift + Arrow extends the selection one character at a time.
    #[test]
    fn shift_arrow_extends_selection_by_char() {
        let mut ti = TextInput::local().with_initial_value("abcdef");
        ti.set_focus(true);
        ti.set_cursor_pos(3);
        let mut doc = AutoCommit::new();

        assert!(drive(&mut ti, &mut doc, &named_key_event(NamedKey::ArrowRight), &mods(false, true)));
        assert_eq!(ti.selection_range(), Some((3, 4)), "shift+Right extends by one char");
        drive(&mut ti, &mut doc, &named_key_event(NamedKey::ArrowRight), &mods(false, true));
        assert_eq!(ti.selection_range(), Some((3, 5)));
        drive(&mut ti, &mut doc, &named_key_event(NamedKey::ArrowLeft), &mods(false, true));
        assert_eq!(ti.selection_range(), Some((3, 4)), "shift+Left shrinks it back");
    }

    /// Ctrl + Arrow moves the caret a whole word without selecting.
    #[test]
    fn ctrl_arrow_moves_caret_by_word() {
        let mut ti = TextInput::local().with_initial_value("hello world foo");
        ti.set_focus(true);
        ti.set_cursor_pos(0);
        let mut doc = AutoCommit::new();

        drive(&mut ti, &mut doc, &named_key_event(NamedKey::ArrowRight), &mods(true, false));
        assert_eq!(ti.cursor_pos(), 6, "ctrl+Right jumps to start of the next word");
        assert_eq!(ti.selection_range(), None, "no shift → no selection");
        drive(&mut ti, &mut doc, &named_key_event(NamedKey::ArrowRight), &mods(true, false));
        assert_eq!(ti.cursor_pos(), 12, "ctrl+Right again → start of 'foo'");
        drive(&mut ti, &mut doc, &named_key_event(NamedKey::ArrowLeft), &mods(true, false));
        assert_eq!(ti.cursor_pos(), 6, "ctrl+Left walks back a word");
    }

    /// Shift + Ctrl + Arrow extends the selection by whole words.
    #[test]
    fn shift_ctrl_arrow_extends_selection_by_word() {
        let mut ti = TextInput::local().with_initial_value("hello world foo");
        ti.set_focus(true);
        ti.set_cursor_pos(0);
        let mut doc = AutoCommit::new();

        drive(&mut ti, &mut doc, &named_key_event(NamedKey::ArrowRight), &mods(true, true));
        assert_eq!(ti.selection_range(), Some((0, 6)), "shift+ctrl+Right selects the first word span");
        drive(&mut ti, &mut doc, &named_key_event(NamedKey::ArrowRight), &mods(true, true));
        assert_eq!(ti.selection_range(), Some((0, 12)), "extends by another word");
    }

    /// Home / End (and their shift variants) move / extend to the line ends.
    #[test]
    fn home_end_move_and_extend() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.set_focus(true);
        ti.set_cursor_pos(5);
        let mut doc = AutoCommit::new();

        drive(&mut ti, &mut doc, &named_key_event(NamedKey::End), &mods(false, false));
        assert_eq!(ti.cursor_pos(), 11, "End → end of field");
        assert_eq!(ti.selection_range(), None);
        drive(&mut ti, &mut doc, &named_key_event(NamedKey::Home), &mods(false, false));
        assert_eq!(ti.cursor_pos(), 0, "Home → start of field");

        // Shift+End from the start selects the whole line.
        drive(&mut ti, &mut doc, &named_key_event(NamedKey::End), &mods(false, true));
        assert_eq!(ti.selection_range(), Some((0, 11)), "shift+End selects to line end");
        // Shift+Home from the end selects the whole line too.
        ti.set_cursor_pos(11);
        drive(&mut ti, &mut doc, &named_key_event(NamedKey::Home), &mods(false, true));
        assert_eq!(ti.selection_range(), Some((0, 11)), "shift+Home selects to line start");
    }

    /// Ctrl + A selects the whole field, and Ctrl + X then cuts the selection
    /// (both via the real key path).
    #[test]
    fn ctrl_a_selects_all_and_ctrl_x_cuts() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        drive(&mut ti, &mut doc, &char_key_event("a"), &mods(true, false));
        assert_eq!(ti.selection_range(), Some((0, 11)), "ctrl+A selects everything");

        drive(&mut ti, &mut doc, &char_key_event("x"), &mods(true, false));
        assert_eq!(ti.value(), "", "ctrl+X cuts the selection");
        assert_eq!(ti.selection_range(), None, "selection collapsed after cut");
    }

    /// A `with_on_change_path` field bound to a key that holds a *scalar* string
    /// (a seeded value, not a Text object) must still persist a Backspace. The
    /// old edit path spliced nowhere — so the next `sync` reverted the local
    /// delete while the caret had already stepped back, which reads exactly as
    /// "Backspace jumps back a char without deleting". The field now seeds its
    /// own backing Text object on the first edit.
    #[test]
    fn crdt_bound_scalar_field_backspace_persists_across_sync() {
        use automerge::{transaction::Transactable, ReadDoc, ROOT};
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "q", "abc").unwrap(); // scalar string, NOT a Text object

        let mut ti = TextInput::new(Vec::new()).with_on_change_path("q");
        ti.set_focus(true);
        Widget::sync(&mut ti, &mut doc, &ROOT);
        assert_eq!(ti.value(), "abc", "reads the seeded scalar value");
        ti.set_cursor_pos(3);

        drive(&mut ti, &mut doc, &named_key_event(NamedKey::Backspace), &mods(false, false));
        // The next frame's sync must NOT revert the delete.
        Widget::sync(&mut ti, &mut doc, &ROOT);

        assert_eq!(ti.value(), "ab", "Backspace deleted the char and it survived sync");
        assert_eq!(ti.cursor_pos(), 2, "caret sits where the char was removed");
        let (v, tid) = doc.get(&ROOT, "q").unwrap().unwrap();
        assert!(
            matches!(v, automerge::Value::Object(automerge::ObjType::Text)),
            "the key was upgraded to a CRDT Text object so edits persist",
        );
        assert_eq!(doc.text(&tid).unwrap(), "ab", "the delete reached the CRDT");
    }

    /// Typed characters into a `with_on_change_path` field the host never seeded
    /// must reach the CRDT, so they survive a widget rebuild (a fresh instance's
    /// `sync` re-reads the same key). Without self-seeding the edits lived only
    /// in the widget's local buffer and were lost on the next structure rebuild.
    #[test]
    fn crdt_bound_unseeded_field_typing_survives_widget_rebuild() {
        use automerge::{ReadDoc, ROOT};
        let mut doc = AutoCommit::new();

        let mut ti = TextInput::new(Vec::new()).with_on_change_path("name");
        ti.set_focus(true);
        Widget::sync(&mut ti, &mut doc, &ROOT); // key absent → empty
        drive(&mut ti, &mut doc, &char_key_event("h"), &mods(false, false));
        drive(&mut ti, &mut doc, &char_key_event("i"), &mods(false, false));
        assert_eq!(ti.value(), "hi");

        // A brand-new widget instance (the "structure rebuild") reads the key.
        let mut fresh = TextInput::new(Vec::new()).with_on_change_path("name");
        Widget::sync(&mut fresh, &mut doc, &ROOT);
        assert_eq!(fresh.value(), "hi", "typed text persisted to the CRDT, not just the buffer");
        let (_, tid) = doc.get(&ROOT, "name").unwrap().unwrap();
        assert_eq!(doc.text(&tid).unwrap(), "hi");
    }

    /// Ctrl+Backspace deletes the whole word before the caret; Ctrl+Delete the
    /// whole word after — the word-granularity deletes, driven through the real
    /// key path (word boundaries from `text_selection`).
    #[test]
    fn ctrl_backspace_and_ctrl_delete_remove_whole_words() {
        let mut ti = TextInput::local().with_initial_value("hello world foo");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        // Ctrl+Delete from the start removes the whole word after the caret.
        ti.set_cursor_pos(0);
        drive(&mut ti, &mut doc, &named_key_event(NamedKey::Delete), &mods(true, false));
        assert_eq!(ti.value(), "world foo", "Ctrl+Delete removed the word after the caret");
        assert_eq!(ti.cursor_pos(), 0, "the caret stays at the deletion point");

        // Ctrl+Backspace from the end removes the whole word before the caret.
        ti.set_cursor_pos(ti.value().len());
        drive(&mut ti, &mut doc, &named_key_event(NamedKey::Backspace), &mods(true, false));
        assert_eq!(ti.value(), "world ", "Ctrl+Backspace removed the word before the caret");
    }

    /// A keystroke splices only the delta into the CRDT `Text` — the per-keystroke
    /// op count is independent of the field length, not an O(n) whole-Text rewrite.
    /// Typing one char into a 2000-char field must cost the same handful of
    /// Automerge ops as typing into a 1-char field (whether appended or inserted
    /// mid-string), or a typing session accumulates O(n^2) tombstones and freezes.
    #[test]
    fn keystroke_splices_only_the_delta_regardless_of_field_length() {
        use automerge::{transaction::Transactable, ObjType, ROOT};

        // Ops added to the CRDT by typing one 'z' at `caret` in a field seeded
        // with `initial`, driven through the real key path.
        fn ops_for_keystroke(initial: &str, caret: usize) -> usize {
            let mut doc = AutoCommit::new();
            let tid = doc.put_object(&ROOT, "t", ObjType::Text).unwrap();
            doc.splice_text(&tid, 0, 0, initial).unwrap();
            let mut ti = TextInput::new(Vec::new()).with_on_change_path("t");
            ti.set_focus(true);
            Widget::sync(&mut ti, &mut doc, &ROOT);
            ti.set_cursor_pos(caret);
            let heads = doc.get_heads(); // commit the setup so we measure only the keystroke
            drive(&mut ti, &mut doc, &char_key_event("z"), &mods(false, false));
            doc.get_changes(&heads).iter().map(|c| c.len()).sum()
        }

        let big = "abcd ".repeat(400); // 2000 chars
        let small_append = ops_for_keystroke("x", 1);
        let big_append = ops_for_keystroke(&big, big.len());
        let big_middle = ops_for_keystroke(&big, 1000);

        assert_eq!(
            big_append, small_append,
            "appending a char to a 2000-char field costs the same ops as a 1-char field \
             ({big_append} vs {small_append}) — the delta is spliced, not the whole Text",
        );
        assert_eq!(
            big_middle, small_append,
            "inserting mid-string is still a single-char delta ({big_middle} ops), not a rewrite",
        );
        assert!(big_append <= 4, "one typed char is a handful of ops, got {big_append}");
    }

    /// Typing over a selection replaces it — the insert path respects the
    /// live selection when driven through `handle_event`.
    #[test]
    fn typing_replaces_selection_via_handle_event() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();
        drive(&mut ti, &mut doc, &char_key_event("a"), &mods(true, false)); // select all
        drive(&mut ti, &mut doc, &char_key_event("Z"), &mods(false, false)); // type over it
        assert_eq!(ti.value(), "Z", "typing over a full selection replaces it");
    }

    /// Double-click selects the word under the pointer, via the real mouse
    /// event ladder (move → press → release → press).
    #[test]
    fn double_click_selects_word_via_handle_event() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        draw_to_populate(&mut ti);
        let mut doc = AutoCommit::new();

        let bx = x_for_byte(&ti, 8); // inside "world" (bytes 6..11)
        drive(&mut ti, &mut doc, &mouse_move(bx, 20.0), &mods(false, false));
        drive(&mut ti, &mut doc, &left_press(), &mods(false, false));
        drive(&mut ti, &mut doc, &left_release(), &mods(false, false));
        // Second press within the multi-click interval → double click.
        drive(&mut ti, &mut doc, &left_press(), &mods(false, false));

        assert_eq!(ti.selection_range(), Some((6, 11)), "double-click selected the word");
        assert_eq!(ti.selected_str(), Some("world"));
    }

    /// A press-drag selects a character range that tracks the pointer.
    #[test]
    fn drag_selects_range_via_handle_event() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        draw_to_populate(&mut ti);
        let mut doc = AutoCommit::new();

        let x_start = x_for_byte(&ti, 2);
        let x_end = x_for_byte(&ti, 9);
        drive(&mut ti, &mut doc, &mouse_move(x_start, 20.0), &mods(false, false));
        drive(&mut ti, &mut doc, &left_press(), &mods(false, false));
        assert!(ti.is_dragging(), "press starts a drag");
        drive(&mut ti, &mut doc, &mouse_move(x_end, 20.0), &mods(false, false));
        assert_eq!(ti.selection_range(), Some((2, 9)), "drag selected the swept range");
        drive(&mut ti, &mut doc, &left_release(), &mods(false, false));
        assert!(!ti.is_dragging(), "release ends the drag");
    }

    /// Triple-click selects the whole field.
    #[test]
    fn triple_click_selects_all_via_handle_event() {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        draw_to_populate(&mut ti);
        let mut doc = AutoCommit::new();

        let bx = x_for_byte(&ti, 8);
        drive(&mut ti, &mut doc, &mouse_move(bx, 20.0), &mods(false, false));
        drive(&mut ti, &mut doc, &left_press(), &mods(false, false));
        drive(&mut ti, &mut doc, &left_press(), &mods(false, false));
        drive(&mut ti, &mut doc, &left_press(), &mods(false, false));
        assert_eq!(ti.selection_range(), Some((0, 11)), "triple-click selects the whole field");
    }

    /// A `with_font_family` override shapes the input's glyphs in that named
    /// family: after the input draws its value once, re-shaping the same
    /// string+size in the SAME family is a cache hit (the input already shaped
    /// it there), while a DIFFERENT family is a distinct miss. This proves the
    /// override reaches the renderer's family-aware path, not the sans default.
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
        Widget::draw(&mut ti, &mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        // The input's draw shapes "Reading" at 16px in the named family. The
        // char-metric pass measures per-prefix, so the full string's shaped
        // glyph run is the one that lands in the shape cache.
        let _ = tr.take_shape_stats();
        // Same string+size+family as the input's body draw → a cache hit.
        tr.draw_text_font(
            &mut scene, "Reading", FONT_SIZE, theme.text_primary,
            Affine::IDENTITY, None, TextWeight::Regular, false, &FontChoice::Named(fam),
        );
        let (hits, _m, _ns) = tr.take_shape_stats();
        assert_eq!(hits, 1, "the input shaped its value in the named family");

        // A sans render of the same string is a distinct shape (different family).
        tr.draw_text_font(
            &mut scene, "Reading", FONT_SIZE, theme.text_primary,
            Affine::IDENTITY, None, TextWeight::Regular, false, &FontChoice::SansSerif,
        );
        let (_h, misses, _ns) = tr.take_shape_stats();
        assert_eq!(misses, 1, "sans is a separate shape from the named family");
    }

    // ---- multiline mode -----------------------------------------------------
    //
    // Every behaviour is driven through the real `handle_event` / `draw`
    // path — the visual-line layout the caret/nav math reads is the same one
    // `draw` builds and paints, so a regression in either is caught.

    /// A focused, laid-out multiline input drawn once so its wrapped
    /// visual-line layout (`lines`) is populated for the nav/selection math.
    fn ml_input(text: &str, w: f64, h: f64) -> TextInput {
        let mut ti = TextInput::local().multiline(true).with_initial_value(text);
        ti.set_focus(true);
        ti.apply_layout_external(Rect::new(0.0, 0.0, w, h));
        draw_to_populate(&mut ti);
        ti
    }

    /// Enter inserts a real newline in multiline mode, splitting at the caret.
    #[test]
    fn multiline_enter_inserts_newline() {
        let mut ti = ml_input("ab", 300.0, 120.0);
        ti.set_cursor_pos(1); // between 'a' and 'b'
        let mut doc = AutoCommit::new();
        assert!(drive(&mut ti, &mut doc, &named_key_event(NamedKey::Enter), &mods(false, false)));
        assert_eq!(ti.value(), "a\nb", "Enter inserted a newline at the caret");
        assert_eq!(ti.cursor_pos(), 2, "caret sits just after the inserted newline");
    }

    /// A single-line field with `enter_submits` fires a one-shot submit signal
    /// and inserts nothing; a multiline field with `enter_submits` still
    /// inserts a newline on Shift+Enter (the chat-composer split).
    #[test]
    fn enter_submits_signals_without_inserting_and_shift_enter_still_splits() {
        let mut ti = TextInput::local().with_enter_submits(true).with_initial_value("hi");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();
        assert!(drive(&mut ti, &mut doc, &named_key_event(NamedKey::Enter), &mods(false, false)));
        assert_eq!(ti.value(), "hi", "Enter must not insert a newline in submit mode");
        assert!(ti.take_submit(), "Enter fired the submit signal");
        assert!(!ti.take_submit(), "submit is one-shot — cleared on read");

        let mut ml = TextInput::local()
            .multiline(true)
            .with_enter_submits(true)
            .with_initial_value("hi");
        ml.set_focus(true);
        ml.set_cursor_pos(2);
        drive(&mut ml, &mut doc, &named_key_event(NamedKey::Enter), &mods(false, true)); // Shift+Enter
        assert_eq!(ml.value(), "hi\n", "Shift+Enter inserts a newline in multiline submit mode");
        assert!(!ml.take_submit(), "Shift+Enter must not submit");
    }

    /// Up / Down move the caret between visual lines (here, hard newlines).
    #[test]
    fn multiline_up_down_moves_between_visual_lines() {
        let mut ti = ml_input("hello\nworld\nfoo", 400.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(8); // inside "world" (row 1)
        assert_eq!(ti.locate_ml(ti.cursor_pos()).0, 1, "precondition: caret on row 1");

        drive(&mut ti, &mut doc, &named_key_event(NamedKey::ArrowUp), &mods(false, false));
        assert_eq!(ti.locate_ml(ti.cursor_pos()).0, 0, "Up moved to the first visual line");

        drive(&mut ti, &mut doc, &named_key_event(NamedKey::ArrowDown), &mods(false, false));
        assert_eq!(ti.locate_ml(ti.cursor_pos()).0, 1, "Down returned to row 1");
        drive(&mut ti, &mut doc, &named_key_event(NamedKey::ArrowDown), &mods(false, false));
        assert_eq!(ti.locate_ml(ti.cursor_pos()).0, 2, "Down reached the last visual line");
    }

    /// The vertical goal column survives a run of Up/Down over a short line:
    /// stepping Down through a 1-char middle line and onto a long third line
    /// lands the caret back near the original column, not at the short line's
    /// end.
    #[test]
    fn multiline_vertical_motion_preserves_goal_column() {
        let mut ti = ml_input("abcdefgh\nx\nabcdefgh", 500.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(8); // end of the first "abcdefgh" (row 0, high column)

        drive(&mut ti, &mut doc, &named_key_event(NamedKey::ArrowDown), &mods(false, false));
        assert_eq!(ti.locate_ml(ti.cursor_pos()).0, 1, "Down landed on the short middle row");

        drive(&mut ti, &mut doc, &named_key_event(NamedKey::ArrowDown), &mods(false, false));
        let (row, _) = ti.locate_ml(ti.cursor_pos());
        assert_eq!(row, 2, "Down reached the third row");
        assert!(
            ti.cursor_pos() >= 17,
            "goal column preserved across the short line — caret is near row 3's end, got {}",
            ti.cursor_pos()
        );
    }

    /// A selection dragged across a newline covers both lines, and the string a
    /// copy would emit (the selected slice) includes the newline.
    #[test]
    fn multiline_selection_spans_lines_and_copy_includes_newline() {
        let mut ti = ml_input("hello\nworld", 400.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(3); // inside "hello"
        drive(&mut ti, &mut doc, &named_key_event(NamedKey::ArrowDown), &mods(false, true)); // Shift+Down
        let (s, e) = ti.selection_range_public().expect("a cross-line selection is active");
        assert!(s <= 5 && e > 6, "selection crosses the newline at byte 5, got ({s},{e})");
        let selected = ti.selected_str().expect("selected slice");
        assert!(selected.contains('\n'), "copy would emit the newline: {selected:?}");
    }

    /// A long paragraph soft-wraps into multiple visual rows at word
    /// boundaries (the first row ends on a space, not mid-word), and Down
    /// steps into the wrapped continuation.
    #[test]
    fn multiline_soft_wraps_at_word_boundaries() {
        let text = "the quick brown fox jumps over the lazy dog again and again";
        let mut ti = ml_input(text, 170.0, 220.0); // narrow → must wrap
        assert!(ti.lines.len() > 1, "long paragraph wrapped into {} rows", ti.lines.len());
        let first_end = ti.lines[0].end;
        assert!(
            ti.value()[..first_end].ends_with(' '),
            "wrap happened at a word boundary (row ends on a space): {:?}",
            &ti.value()[..first_end]
        );

        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(0);
        drive(&mut ti, &mut doc, &named_key_event(NamedKey::ArrowDown), &mods(false, false));
        assert_eq!(ti.locate_ml(ti.cursor_pos()).0, 1, "Down entered the wrapped continuation");
        assert!(ti.cursor_pos() > 0, "caret advanced into the second visual row");
    }

    /// Home / End act on the current *visual* line in multiline mode.
    #[test]
    fn multiline_home_end_are_per_visual_line() {
        let mut ti = ml_input("hello\nworld", 400.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(8); // "wo|rld" on row 1
        drive(&mut ti, &mut doc, &named_key_event(NamedKey::Home), &mods(false, false));
        assert_eq!(ti.cursor_pos(), 6, "Home went to the start of the second line, not the field");
        drive(&mut ti, &mut doc, &named_key_event(NamedKey::End), &mods(false, false));
        assert_eq!(ti.cursor_pos(), 11, "End went to the end of the second line");
    }

    /// A multiline input reports `AccessState::multiline`; a single-line one
    /// does not.
    #[test]
    fn multiline_sets_accessibility_flag() {
        let ml = TextInput::local().multiline(true);
        assert!(Widget::accessibility(&ml).unwrap().state.multiline, "multiline flagged in a11y");
        let single = TextInput::local();
        assert!(!Widget::accessibility(&single).unwrap().state.multiline);
    }

    /// Backspace at the start of a wrapped-by-newline line merges it with the
    /// previous one (the newline is just a byte, so the shared delete path
    /// handles it) — proves multiline editing composes with the core.
    #[test]
    fn multiline_backspace_at_line_start_merges_lines() {
        let mut ti = ml_input("hello\nworld", 400.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(6); // start of "world", just after the newline
        drive(&mut ti, &mut doc, &named_key_event(NamedKey::Backspace), &mods(false, false));
        assert_eq!(ti.value(), "helloworld", "Backspace removed the newline, merging the lines");
        assert_eq!(ti.cursor_pos(), 5);
    }

    /// A focused multiline field follows its caret with vertical scroll: a caret
    /// below the viewport scrolls the body down to keep it visible, and a caret
    /// back at the top scrolls back up. This is the shared editable view's
    /// scroll-to-caret — the body never leaves the caret off-screen while typing.
    #[test]
    fn multiline_scrolls_to_keep_caret_visible() {
        let text = (0..20).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        // ~5 rows fit in 120 px; 20 rows total, so the body must scroll.
        let mut ti = ml_input(&text, 300.0, 120.0);
        assert!(ti.lines.len() >= 20, "the 20 lines laid out as rows");

        // Caret at the top pins the view to the top.
        ti.set_cursor_pos(0);
        draw_to_populate(&mut ti);
        assert!(ti.scroll_y() < 1.0, "caret at the top keeps the view at the top");

        // Caret at the end sits far below the viewport → the body scrolls down.
        ti.set_cursor_pos(ti.value().len());
        draw_to_populate(&mut ti);
        let scrolled = ti.scroll_y();
        assert!(scrolled > 1.0, "the view scrolled down to reveal the caret, got {scrolled}");
        let (caret_row, _) = ti.locate_ml(ti.cursor_pos());
        let caret_top = ti.lines[caret_row].y;
        let view_h = ti.rect.height() - ML_PAD_Y * 2.0;
        assert!(
            caret_top >= scrolled && caret_top <= scrolled + view_h,
            "the caret row {caret_top} is within the scrolled viewport [{scrolled}, {})",
            scrolled + view_h
        );

        // Caret back to the top scrolls the body back up.
        ti.set_cursor_pos(0);
        draw_to_populate(&mut ti);
        assert!(ti.scroll_y() < 1.0, "the view scrolled back to the caret at the top");
    }

    // ---- secret / masked mode ----------------------------------------------

    /// The reveal toggle shows the real glyphs on screen, but the plaintext is
    /// still kept out of the a11y tree (value `None`, `protected`) even when
    /// revealed.
    #[test]
    fn secret_reveal_toggle_shows_glyphs_but_never_leaks_to_a11y() {
        let mut ti = TextInput::new(Vec::new()).with_secret(true);
        ti.text = "hunter2".to_string();
        assert_eq!(ti.display_text(), "•••••••", "masked by default");

        ti.toggle_reveal();
        assert!(ti.is_revealed());
        assert_eq!(ti.display_text(), "hunter2", "revealed shows the real glyphs");
        let node = Widget::accessibility(&ti).unwrap();
        assert_eq!(node.value, None, "revealed value still must not reach the a11y tree");
        assert!(node.state.protected, "still marked protected while revealed");

        ti.toggle_reveal();
        assert_eq!(ti.display_text(), "•••••••", "toggles back to masked");
    }

    /// A secret field masks the render but the model holds the real value, so
    /// copy emits the real secret and cut/caret still operate on real bytes.
    #[test]
    fn secret_masks_render_but_clipboard_and_caret_use_the_real_value() {
        let mut ti = TextInput::local().with_secret(true).with_initial_value("s3cr3t");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        // Caret navigation walks the real characters (not the bullet run).
        ti.set_cursor_pos(0);
        drive(&mut ti, &mut doc, &named_key_event(NamedKey::ArrowRight), &mods(false, false));
        assert_eq!(ti.cursor_pos(), 1, "Right moved one real char boundary");

        // Ctrl+A → the slice a copy would send is the real secret, not bullets.
        drive(&mut ti, &mut doc, &char_key_event("a"), &mods(true, false));
        assert_eq!(ti.selected_str(), Some("s3cr3t"), "copy sends the real value, never the mask");

        // Ctrl+X cuts the real selection.
        drive(&mut ti, &mut doc, &char_key_event("x"), &mods(true, false));
        assert_eq!(ti.value(), "", "Ctrl+X cut the real selection");
    }
}
