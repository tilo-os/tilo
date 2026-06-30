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
use crate::text_render::TextRenderer;
use crate::text_selection::{
    clipboard_copy, clipboard_paste, next_char_boundary, next_word_boundary,
    prev_char_boundary, prev_word_boundary, word_boundaries,
};
use crate::theme::Theme;
use crate::presence::{PathSegment, PresenceKey};

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
        }
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
    /// bound `ObjId` (via [`bind_text`]) wins over the string-path mode.
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
    /// the local one, only recoloured.
    pub fn caret_rect(&self, theme: &Theme, byte: usize) -> Rect {
        let x = self.caret_x(theme, byte);
        let (text_y, line_height) = self.text_band();
        Rect::new(x, text_y, x + CARET_WIDTH, text_y + line_height)
    }

    /// The selection highlight rect for the `[start, end)` byte range, in
    /// local coordinates — same vertical band as the caret.
    pub fn selection_rect(&self, theme: &Theme, start: usize, end: usize) -> Rect {
        let x0 = self.caret_x(theme, start);
        let x1 = self.caret_x(theme, end);
        let (text_y, line_height) = self.text_band();
        Rect::new(x0, text_y, x1, text_y + line_height)
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

    /// The string actually painted / measured. For secret inputs this is
    /// one bullet per *character* of the real text; otherwise it's the
    /// real text itself.
    fn display_text(&self) -> String {
        if self.secret {
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
        if self.secret {
            "•".repeat(self.text[..byte_idx].chars().count())
        } else {
            self.text[..byte_idx].to_string()
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
        self.last_edit_time = Some(Instant::now());
        self.dirty = true;
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
        let node = taffy.new_leaf(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(40.0),
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
            let (w, _) = text_renderer.measure(&self.display_prefix(byte_idx), text_size, None);
            self.char_x_positions[byte_idx] = w as f64;
        }
        let (total_w, _) = text_renderer.measure(&display, text_size, None);
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
        text_renderer.draw_text(
            scene,
            &display,
            text_size,
            text_color,
            transform * Affine::translate((self.rect.x0 + padding_x - self.scroll_x, text_y)),
            None,
        );

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
            WindowEvent::CursorMoved { position, .. } => {
                let logical_pos = Point::new(position.x / scale_factor, position.y / scale_factor);
                let hover = self.rect.contains(logical_pos);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *handled = true;
                }
                self.last_mouse_pos = logical_pos;

                if self.is_dragging {
                    let pos = self.cursor_from_x(logical_pos.x);
                    if pos != self.cursor_pos {
                        if self.selection_anchor.is_none() {
                            self.selection_anchor = Some(self.cursor_pos);
                        }
                        self.cursor_pos = pos;
                        *handled = true;
                    }
                }
            }
            WindowEvent::MouseInput { state: element_state, button: MouseButton::Left, .. } => {
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

                        let pos = self.cursor_from_x(self.last_mouse_pos.x);
                        match self.click_count {
                            1 => {
                                if modifiers.state().shift_key() {
                                    self.set_cursor(pos, true);
                                } else {
                                    self.selection_anchor = None;
                                    self.cursor_pos = pos;
                                    self.is_dragging = true;
                                }
                            }
                            2 => {
                                let (start, end) = word_boundaries(&self.text, pos);
                                self.selection_anchor = Some(start);
                                self.cursor_pos = end;
                            }
                            _ => {
                                self.selection_anchor = Some(0);
                                self.cursor_pos = self.text.len();
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
                if !self.is_focused || *element_state != ElementState::Pressed {
                    return;
                }
                let obj_id_opt: Option<ObjId> = self.resolve_text_obj(state, root);
                let shift = modifiers.state().shift_key();
                let ctrl = modifiers.state().control_key();

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
                    Key::Named(NamedKey::Home) => {
                        self.set_cursor(0, shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::End) => {
                        self.set_cursor(self.text.len(), shift);
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
                                if !self.text.is_empty() {
                                    self.selection_anchor = Some(0);
                                    self.cursor_pos = self.text.len();
                                }
                                *handled = true;
                            }
                            "c" => {
                                if let Some(s) = self.selected_str() {
                                    clipboard_copy(s);
                                }
                                *handled = true;
                            }
                            "x" => {
                                if let Some(s) = self.selected_str() {
                                    clipboard_copy(&s.to_string());
                                    self.delete_selection(state, obj_id_opt.as_ref());
                                }
                                *handled = true;
                            }
                            "v" => {
                                if let Some(paste) = clipboard_paste() {
                                    // Strip any newlines / nulls — single-line input.
                                    let cleaned: String = paste
                                        .chars()
                                        .filter(|c| !c.is_control())
                                        .collect();
                                    if !cleaned.is_empty() {
                                        self.insert_str(&cleaned, state, obj_id_opt.as_ref());
                                    }
                                }
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
                let obj_id_opt: Option<ObjId> = self.resolve_text_obj(state, root);
                if !t.chars().all(|c| c.is_control()) {
                    self.insert_str(t.as_str(), state, obj_id_opt.as_ref());
                    *handled = true;
                }
            }
            _ => {}
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
            if let Some(list) = presence.carets.get(&key) {
                for (user, caret) in list.iter() {
                    presence.draw_peer_caret(
                        scene,
                        theme,
                        transform,
                        user,
                        self.caret_rect(theme, *caret),
                    );
                }
            }

            if let Some(list) = presence.selections.get(&key) {
                for (user, range) in list.iter() {
                    presence.draw_peer_selection(
                        scene,
                        theme,
                        transform,
                        user,
                        self.selection_rect(theme, range.start, range.end),
                    );
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
}
