//! `RichTextEditor` — a shared CRDT-native WYSIWYG rich-text editor widget.
//!
//! Ties together the shared caret core ([`EditCore`])
//! and the rich-run layout primitive ([`RichTextLayout`]) into an *editable*
//! widget with shared caret motion, selection, splicing, and run styling.
//!
//! Design, following the five-pass [`Widget`] model:
//!
//! * **State lives in the CRDT.** The characters are an Automerge `Text`; the
//!   inline formatting is stored as Automerge **native marks** (`bold` / `italic`
//!   / `underline`), which shift automatically as text is spliced and merge
//!   collaboratively — the same mechanism [`TextInput`] uses for authorship. The
//!   widget holds only render / caret caches.
//! * **Caret + selection are [`EditCore`].** Motion, shift-select, select-all,
//!   and the insert/delete splices come from the shared core, so this editor
//!   and any future adopter get identical, tested behaviour.
//! * **Runs render through [`RichTextLayout`].** Marks become [`RichSpan`]s via
//!   the shared [`marks::segments_for`] partition, so bold/italic/underline
//!   shape as attributed runs.
//! * **IME-friendly.** Committed IME text is inserted; an in-progress preedit is
//!   rendered inline at the caret without touching the CRDT until it commits.
//!
//! Scope: this is a **single-paragraph** rich field that *word-wraps* — one
//! Automerge `Text`, no hard line breaks (Enter is a no-op and pasted newlines
//! fold to spaces), because [`RichTextLayout`] indexes bytes per paragraph. A
//! multi-paragraph document composes a column of these, one editor per block.
//!
//! [`text_selection`]: crate::text_selection
//! [`TextInput`]: crate::widget::TextInput

use crate::time::{Duration, Instant};
use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Fill;
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::gesture::{DragSelect, Granularity};
use crate::presence::PresenceCtx;
use crate::richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign};
use crate::text_edit::{EditCore, Motion};
use crate::text_render::TextRenderer;
use crate::text_selection::{
    TextAffinity, TextPosition, clipboard_copy, clipboard_paste, extend_span_drag, line_boundaries,
    word_boundaries,
};
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::marks::{self, Mark, MarkSpan};

/// Inner padding of the editor frame (logical px).
const PAD: f64 = 12.0;
/// Extra left indent for a bulleted block, leaving room for the marker.
const BULLET_INDENT: f64 = 22.0;

/// Narrows a screen-space `f64` (rect extent or coordinate) to the `f32`
/// vello/`RichTextLayout` expect.
#[allow(
    clippy::cast_possible_truncation,
    reason = "on-screen layout coordinates never approach f32::MAX; this narrowing is exact for every practical UI extent"
)]
fn px(v: f64) -> f32 {
    v as f32
}

/// Block-level style of the editor's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RichBlockStyle {
    /// Regular paragraph text.
    #[default]
    Body,
    /// A large, bold heading.
    Heading,
    /// A medium, bold subheading.
    Subheading,
    /// A bulleted list item (leading marker + indent).
    Bullet,
}

impl RichBlockStyle {
    fn as_str(self) -> &'static str {
        match self {
            RichBlockStyle::Body => "body",
            RichBlockStyle::Heading => "heading",
            RichBlockStyle::Subheading => "subheading",
            RichBlockStyle::Bullet => "bullet",
        }
    }
    fn from_str(s: &str) -> Self {
        match s {
            "heading" => RichBlockStyle::Heading,
            "subheading" => RichBlockStyle::Subheading,
            "bullet" => RichBlockStyle::Bullet,
            _ => RichBlockStyle::Body,
        }
    }
    /// Font-size multiplier over the editor's base body size.
    fn size_mult(self) -> f32 {
        match self {
            RichBlockStyle::Heading => 1.7,
            RichBlockStyle::Subheading => 1.3,
            RichBlockStyle::Body | RichBlockStyle::Bullet => 1.0,
        }
    }
    /// Whether the whole block shapes bold.
    fn base_bold(self) -> bool {
        matches!(self, RichBlockStyle::Heading | RichBlockStyle::Subheading)
    }
    /// The next style in the toolbar/keyboard rotation.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            RichBlockStyle::Body => RichBlockStyle::Heading,
            RichBlockStyle::Heading => RichBlockStyle::Subheading,
            RichBlockStyle::Subheading => RichBlockStyle::Bullet,
            RichBlockStyle::Bullet => RichBlockStyle::Body,
        }
    }
}

/// A CRDT-native WYSIWYG rich-text editor. Bind it to a field key under the
/// widget's sync path; it stores `{ text: Text, style }` there.
///
/// `frame`, `layout_dirty`, `redraw`, `focused`, drag selection and `dirty`
/// are independent state axes (frame chrome, layout cache, paint cache,
/// focus, selection interaction, unsaved-edit) rather than one state machine, and
/// every field is set through named struct-literal / method assignment, never
/// a positional bool argument — so bundling them into a bitflags type would
/// touch every read/write site in this widget for no behavioural change.
#[allow(
    clippy::struct_excessive_bools,
    reason = "six independent state axes, all set/read by name, not positional bool args"
)]
pub struct RichTextEditor {
    key: String,
    placeholder: String,
    base_font_size: f32,
    frame: bool,

    // --- resolved CRDT handles (from `sync`) ---
    field_obj: Option<ObjId>,
    text_obj: Option<ObjId>,

    // --- caches pulled from the CRDT each `sync` ---
    text: String,
    /// Inline marks in BYTE ranges, rebuilt from the Text's native marks.
    marks: Vec<MarkSpan>,
    style: RichBlockStyle,

    // --- caret / selection ---
    core: EditCore,
    goal_x: Option<f32>,

    // --- render caches ---
    rect: Rect,
    layout: Option<RichTextLayout>,
    layout_width: f32,
    layout_dirty: bool,
    layout_epoch: Option<Arc<()>>,
    redraw: bool,

    // --- interaction ---
    focused: bool,
    last_cursor: Point,
    drag_select: DragSelect,
    drag_anchor_span: (usize, usize),

    // --- IME ---
    preedit: String,
    preedit_cursor: Option<(usize, usize)>,
    last_caret_width: f64,

    // --- persistence / undo ---
    dirty: bool,
    last_edit_time: Option<Instant>,
}

impl RichTextEditor {
    /// A body-styled editor bound to `key` under the widget's sync path.
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            placeholder: String::new(),
            base_font_size: 16.0,
            frame: true,
            field_obj: None,
            text_obj: None,
            text: String::new(),
            marks: Vec::new(),
            style: RichBlockStyle::Body,
            core: EditCore::new(),
            goal_x: None,
            rect: Rect::ZERO,
            layout: None,
            layout_width: 0.0,
            layout_dirty: true,
            layout_epoch: None,
            redraw: true,
            focused: false,
            last_cursor: Point::ZERO,
            drag_select: DragSelect::new(),
            drag_anchor_span: (0, 0),
            preedit: String::new(),
            preedit_cursor: None,
            last_caret_width: 2.0,
            dirty: false,
            last_edit_time: None,
        }
    }

    /// Placeholder shown (dimmed) while the editor is empty and unfocused.
    #[must_use]
    pub fn with_placeholder(mut self, text: impl Into<String>) -> Self {
        self.placeholder = text.into();
        self
    }
    /// Override the base body font size (logical px). Block styles scale off it.
    #[must_use]
    pub fn with_font_size(mut self, px: f32) -> Self {
        self.base_font_size = px;
        self
    }
    /// Draw the editor without its rounded frame (for an inline title field).
    #[must_use]
    pub fn without_frame(mut self) -> Self {
        self.frame = false;
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn block_style(&self) -> RichBlockStyle {
        self.style
    }
    /// The caret byte offset.
    pub fn caret(&self) -> usize {
        self.core.caret()
    }

    /// The caret's document offset and visual side at an ambiguous boundary.
    pub fn caret_position(&self) -> TextPosition {
        self.core.caret_position()
    }
    /// The selected byte range (empty when collapsed).
    pub fn selection(&self) -> Range<usize> {
        self.core.range()
    }

    /// One-shot read of whether a user edit landed since the last call.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    // --- host-facing formatting actions (call from the app's event handler) ---

    pub fn toggle_bold(&mut self, state: &mut AutoCommit) {
        self.toggle_mark(state, Mark::Bold, "bold");
    }
    pub fn toggle_italic(&mut self, state: &mut AutoCommit) {
        self.toggle_mark(state, Mark::Italic, "italic");
    }
    pub fn toggle_underline(&mut self, state: &mut AutoCommit) {
        self.toggle_mark(state, Mark::Underline, "underline");
    }
    pub fn set_block_style(&mut self, state: &mut AutoCommit, style: RichBlockStyle) {
        if let Some(f) = self.field_obj.clone() {
            let _ = state.put(&f, "style", style.as_str());
        }
        self.style = style;
        self.mark_dirty();
    }
    pub fn cycle_block_style(&mut self, state: &mut AutoCommit) {
        let next = self.style.next();
        self.set_block_style(state, next);
    }

    // --- internal helpers -------------------------------------------------

    fn mark_dirty(&mut self) {
        self.dirty = true;
        self.redraw = true;
        self.layout_dirty = true;
        self.last_edit_time = Some(crate::time::now());
    }

    /// `None` when the document write itself fails (e.g. `parent` turned out
    /// not to be an object) — the caller skips this sync rather than trust an
    /// id that was never actually written.
    fn ensure_map(state: &mut AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
        match state.get(parent, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => state.put_object(parent, key, ObjType::Map).ok(),
        }
    }
    /// See [`Self::ensure_map`].
    fn ensure_text(state: &mut AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
        match state.get(parent, key) {
            Ok(Some((Value::Object(ObjType::Text), id))) => Some(id),
            _ => state.put_object(parent, key, ObjType::Text).ok(),
        }
    }

    fn rebuild_marks(&mut self, state: &AutoCommit, tid: &ObjId) {
        self.marks.clear();
        let Ok(ms) = state.marks(tid) else { return };
        for m in ms {
            let kind = match m.name.as_str() {
                "bold" => Mark::Bold,
                "italic" => Mark::Italic,
                "underline" => Mark::Underline,
                _ => continue,
            };
            if matches!(m.value, ScalarValue::Boolean(false)) {
                continue;
            }
            let bs = char_to_byte(&self.text, m.start);
            let be = char_to_byte(&self.text, m.end);
            if be > bs {
                self.marks.push(MarkSpan::new(bs..be, kind));
            }
        }
        self.marks.sort_by_key(|s| s.range.start);
    }

    /// Apply a byte-space edit from [`EditCore`] to the CRDT `Text` (converting
    /// to char offsets) and keep the local caches aligned. Native marks shift
    /// themselves in the doc; the local byte-range copy is shifted to match so
    /// the current frame stays correct before the next `sync` re-reads them.
    fn apply_edit(&mut self, state: &mut AutoCommit, e: &crate::text_edit::Edit) {
        if e.is_noop() {
            return;
        }
        let end = e.at.saturating_add(e.removed);
        // `e.at`/`end` must land on `self.text`'s char boundaries — `EditCore`
        // guarantees this, but a slice that didn't would panic, so an edit
        // that doesn't land cleanly is dropped rather than trusted blindly.
        let (Some(before), Some(removed_slice)) = (self.text.get(..e.at), self.text.get(e.at..end))
        else {
            return;
        };
        if let Some(tid) = self.text_obj.clone() {
            let char_pos = before.chars().count();
            let char_del = removed_slice.chars().count();
            let char_del = isize::try_from(char_del).unwrap_or(isize::MAX);
            let _ = state.splice_text(&tid, char_pos, char_del, &e.inserted);
        }
        self.text.replace_range(e.at..end, &e.inserted);
        marks::shift_after_edit(&mut self.marks, e.at, e.inserted.len(), e.removed);
        self.goal_x = None;
        self.mark_dirty();
    }

    fn toggle_mark(&mut self, state: &mut AutoCommit, mark: Mark, name: &str) {
        if self.core.selection().is_collapsed() {
            return;
        }
        let Some(tid) = self.text_obj.clone() else {
            return;
        };
        let r = self.core.range();
        let covered = marks::range_fully_covered(&self.marks, &r, &mark);
        // `r` must land on `self.text`'s char boundaries — `EditCore` guarantees
        // this, but a slice that didn't would panic, so an out-of-alignment
        // range is dropped rather than trusted blindly.
        let (Some(before_start), Some(before_end)) =
            (self.text.get(..r.start), self.text.get(..r.end))
        else {
            return;
        };
        let cs = before_start.chars().count();
        let ce = before_end.chars().count();
        if covered {
            let _ = state.unmark(&tid, name, cs, ce, automerge::marks::ExpandMark::None);
        } else {
            let m = automerge::marks::Mark::new(name.to_string(), true, cs, ce);
            let _ = state.mark(&tid, m, automerge::marks::ExpandMark::None);
        }
        marks::toggle_mark_range(&mut self.marks, r, mark);
        self.mark_dirty();
    }

    fn insert_str(&mut self, state: &mut AutoCommit, s: &str) {
        // Single-paragraph field: fold any newlines (typed or pasted) into
        // spaces so the buffer stays one paragraph — `RichTextLayout` indexes
        // bytes per paragraph, so a hard `\n` would desync caret geometry.
        let cleaned: String = s
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .collect();
        if cleaned.is_empty() {
            return;
        }
        let edit = self.core.insert(&cleaned);
        self.apply_edit(state, &edit);
    }

    /// The text + marks + caret to display, folding in an active IME preedit
    /// (rendered but not yet committed). Returns the preedit byte range too.
    fn display_state(&self) -> (String, Vec<MarkSpan>, usize, Option<Range<usize>>) {
        let no_preedit = || {
            (
                self.text.clone(),
                self.marks.clone(),
                self.core.caret(),
                None,
            )
        };
        if self.preedit.is_empty() {
            return no_preedit();
        }
        let c = self.core.caret().min(self.text.len());
        // `c` must land on `self.text`'s char boundaries — `EditCore` keeps the
        // caret aligned, but a slice that didn't would panic, so a misaligned
        // caret reports the committed text with no preedit splice rather than
        // build a string that doesn't match the coordinates below.
        let Some((head, tail)) = self.text.get(..c).zip(self.text.get(c..)) else {
            return no_preedit();
        };
        let dt_capacity = self.text.len().saturating_add(self.preedit.len());
        let mut dt = String::with_capacity(dt_capacity);
        dt.push_str(head);
        dt.push_str(&self.preedit);
        dt.push_str(tail);
        let mut dm = self.marks.clone();
        marks::shift_after_edit(&mut dm, c, self.preedit.len(), 0);
        let within = self
            .preedit_cursor
            .map_or(self.preedit.len(), |(_, end)| end.min(self.preedit.len()));
        let within = crate::text_selection::grapheme_boundary_at_or_before(&self.preedit, within);
        let caret = c.saturating_add(within);
        let preedit_end = c.saturating_add(self.preedit.len());
        (dt, dm, caret, Some(c..preedit_end))
    }

    /// Map a byte offset in the display text back to the committed text.
    fn display_to_text_byte(&self, disp: usize) -> usize {
        if self.preedit.is_empty() {
            return disp.min(self.text.len());
        }
        let c = self.core.caret().min(self.text.len());
        let plen = self.preedit.len();
        let preedit_end = c.saturating_add(plen);
        if disp <= c {
            disp
        } else if disp >= preedit_end {
            disp.saturating_sub(plen)
        } else {
            c
        }
    }

    fn display_caret_position(&self) -> TextPosition {
        let (_, _, byte, _) = self.display_state();
        if self.preedit.is_empty() {
            TextPosition::with_affinity(byte, self.core.caret_position().affinity)
        } else {
            TextPosition::new(byte)
        }
    }

    fn display_to_text_position(&self, position: TextPosition) -> TextPosition {
        TextPosition::with_affinity(self.display_to_text_byte(position.byte), position.affinity)
    }

    fn content_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PAD,
            self.rect.y0 + PAD,
            self.rect.x1 - PAD,
            self.rect.y1 - PAD,
        )
    }
    fn text_origin(&self) -> Point {
        let indent = if self.style == RichBlockStyle::Bullet {
            BULLET_INDENT
        } else {
            0.0
        };
        let inner = self.content_rect();
        Point::new(inner.x0 + indent, inner.y0)
    }
    fn text_width(&self) -> f32 {
        let indent = if self.style == RichBlockStyle::Bullet {
            BULLET_INDENT
        } else {
            0.0
        };
        px((self.content_rect().width() - indent).max(1.0))
    }

    fn shape(&self, tr: &mut TextRenderer, width: f32) -> RichTextLayout {
        let (display_text, display_marks, _caret, preedit_range) = self.display_state();
        let size = self.base_font_size * self.style.size_mult();
        let base_bold = self.style.base_bold();

        let mut spans: Vec<RichSpan> = Vec::new();
        let mut underlines: Vec<(usize, usize)> = Vec::new();
        let mut strikes: Vec<(usize, usize)> = Vec::new();
        let segs = marks::segments_for(display_text.len(), &display_marks);
        if segs.is_empty() {
            spans.push(RichSpan::new(String::new()));
        }
        for seg in &segs {
            // `segments_for` promises byte ranges into `display_text`, but a
            // range that somehow didn't land on a char boundary would panic
            // here; fall back to an empty run rather than trust it blindly.
            let s = display_text
                .get(seg.range.clone())
                .unwrap_or_default()
                .to_string();
            spans.push(RichSpan::new(s).bold(seg.bold).italic(seg.italic));
            if seg.underline {
                underlines.push((seg.range.start, seg.range.end));
            }
            if seg.strikethrough {
                strikes.push((seg.range.start, seg.range.end));
            }
        }
        if let Some(pr) = preedit_range {
            underlines.push((pr.start, pr.end));
        }
        let browser_emoji = tr.browser_color_emoji_installed();
        let layout = RichTextLayout::shape_styled_browser(
            &mut tr.font_system,
            &spans,
            &RichTextLayout::block_style(base_bold, size, FontFamily::Sans, 1.0),
            Some(width),
            TextAlign::Left,
            underlines,
            strikes,
            browser_emoji,
        );
        tr.note_browser_rich_layout(&layout);
        layout
    }

    fn ensure_layout(&mut self, tr: &mut TextRenderer, width: f32) {
        let epoch_stale = self
            .layout_epoch
            .as_ref()
            .is_none_or(|epoch| !Arc::ptr_eq(epoch, tr.layout_epoch()));
        let stale = self.layout.is_none()
            || self.layout_dirty
            || (self.layout_width - width).abs() > 0.5
            || epoch_stale;
        if stale {
            self.layout = Some(self.shape(tr, width.max(1.0)));
            self.layout_width = width;
            self.layout_dirty = false;
            self.layout_epoch = Some(Arc::clone(tr.layout_epoch()));
        }
    }

    /// Exact committed-text position under a logical point.
    fn position_at(&self, pt: Point) -> TextPosition {
        let Some(layout) = &self.layout else {
            return self.core.caret_position();
        };
        let origin = self.text_origin();
        let x = px(pt.x - origin.x);
        let y = px(pt.y - origin.y);
        let disp = layout.hit_position(x.max(0.0), y.max(0.0));
        self.display_to_text_position(disp)
    }

    fn move_vertical(&mut self, down: bool, extend: bool) {
        let Some(layout) = &self.layout else { return };
        let caret_disp = self.display_caret_position();
        let (cx, top, h) = layout.caret_geometry_at(caret_disp);
        let goal = self.goal_x.unwrap_or(cx);
        let ty = if down {
            top + h + h * 0.5
        } else {
            top - h * 0.5
        };
        let disp = layout.hit_position(goal, ty.max(0.0));
        let target = self.display_to_text_position(disp);
        self.core.place_position(target, extend);
        self.goal_x = Some(goal);
        self.redraw = true;
    }

    fn move_horizontal(&mut self, right: bool, extend: bool, word: bool) {
        if word {
            self.core.motion(
                &self.text,
                if right {
                    Motion::WordRight
                } else {
                    Motion::WordLeft
                },
                extend,
            );
        } else if let Some(layout) = &self.layout {
            if !extend && self.core.has_selection() {
                let range = self.core.range();
                let left = layout.caret_geometry(range.start).0;
                let right_x = layout.caret_geometry(range.end).0;
                let target = if right == (right_x >= left) {
                    range.end
                } else {
                    range.start
                };
                self.core.place(target, false);
            } else {
                let target = layout.visual_motion_position(self.core.caret_position(), right);
                self.core.place_position(target, extend);
            }
        } else {
            self.core.motion(
                &self.text,
                if right { Motion::Right } else { Motion::Left },
                extend,
            );
        }
        self.goal_x = None;
        self.redraw = true;
    }

    fn copy_selection(&self) {
        let r = self.core.range();
        if r.start < r.end
            && let Some(sel) = self.text.get(r)
        {
            clipboard_copy(sel);
        }
    }

    /// Handles a focused `KeyboardInput` press: motion, editing, and
    /// formatting shortcuts. Split out of `handle_event` to keep that under
    /// the line cap.
    fn handle_key_press(
        &mut self,
        logical_key: &Key,
        text: Option<&str>,
        ctrl: bool,
        shift: bool,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        match logical_key {
            Key::Named(NamedKey::ArrowLeft) => {
                self.move_horizontal(false, shift, ctrl);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.move_horizontal(true, shift, ctrl);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.move_vertical(false, shift);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.move_vertical(true, shift);
                *handled = true;
            }
            Key::Named(NamedKey::Home) => {
                if ctrl {
                    self.core.motion(&self.text, Motion::DocStart, shift);
                } else if let Some(layout) = &self.layout {
                    let (home, _) = layout.visual_line_edges(self.core.caret());
                    self.core.place(home, shift);
                } else {
                    self.core.motion(&self.text, Motion::LineStart, shift);
                }
                self.goal_x = None;
                self.redraw = true;
                *handled = true;
            }
            Key::Named(NamedKey::End) => {
                if ctrl {
                    self.core.motion(&self.text, Motion::DocEnd, shift);
                } else if let Some(layout) = &self.layout {
                    let (_, end) = layout.visual_line_edges(self.core.caret());
                    self.core.place(end, shift);
                } else {
                    self.core.motion(&self.text, Motion::LineEnd, shift);
                }
                self.goal_x = None;
                self.redraw = true;
                *handled = true;
            }
            Key::Named(NamedKey::Backspace) => {
                let edit = self.core.delete_backward(&self.text, ctrl);
                self.apply_edit(state, &edit);
                *handled = true;
            }
            Key::Named(NamedKey::Delete) => {
                let edit = self.core.delete_forward(&self.text, ctrl);
                self.apply_edit(state, &edit);
                *handled = true;
            }
            Key::Named(NamedKey::Enter) => {
                // Single-paragraph field: Enter is a no-op here (a host
                // can treat it as submit), never a hard line break.
            }
            Key::Named(NamedKey::Space) if !ctrl => {
                self.insert_str(state, " ");
                *handled = true;
            }
            Key::Character(c) if ctrl => self.apply_ctrl_shortcut(c.as_str(), state, handled),
            Key::Character(c) if !ctrl => {
                self.insert_str(state, c.as_str());
                *handled = true;
            }
            _ => {
                // Fall back to any committed text winit attached (dead
                // keys, layout composition) when it isn't a control combo.
                if !ctrl
                    && let Some(t) = text
                    && !t.is_empty()
                    && !t.chars().all(char::is_control)
                {
                    self.insert_str(state, t);
                    *handled = true;
                }
            }
        }
    }

    /// The `Key::Character` + Ctrl arm of [`Self::handle_key_press`], split
    /// out to keep that under the line cap: select-all, copy/cut/paste, and
    /// bold/italic/underline toggles.
    fn apply_ctrl_shortcut(&mut self, c: &str, state: &mut AutoCommit, handled: &mut bool) {
        match c {
            "a" | "A" => {
                self.core.select_all(&self.text);
                self.redraw = true;
                *handled = true;
            }
            "c" | "C" => {
                self.copy_selection();
                *handled = true;
            }
            "x" | "X" => {
                self.copy_selection();
                let edit = self.core.delete_backward(&self.text, false);
                self.apply_edit(state, &edit);
                *handled = true;
            }
            "v" | "V" => {
                if let Some(clip) = clipboard_paste() {
                    self.insert_str(state, &clip);
                }
                *handled = true;
            }
            "b" | "B" => {
                self.toggle_bold(state);
                *handled = true;
            }
            "i" | "I" => {
                self.toggle_italic(state);
                *handled = true;
            }
            "u" | "U" => {
                self.toggle_underline(state);
                *handled = true;
            }
            _ => {}
        }
    }

    fn handle_cursor_move(&mut self, point: Point, handled: &mut bool) {
        self.last_cursor = point;
        let Some(granularity) = self.drag_select.drag() else {
            return;
        };
        let position = self.position_at(point);
        let raw = position.byte;
        let (anchor_start, anchor_end) = self.drag_anchor_span;
        let (anchor, caret) = match granularity {
            Granularity::Char => (anchor_start, raw),
            Granularity::Word => {
                let (start, end) = word_boundaries(&self.text, raw);
                extend_span_drag(anchor_start, anchor_end, raw, start, end)
            }
            Granularity::Line => {
                let (start, end) = line_boundaries(&self.text, raw);
                extend_span_drag(anchor_start, anchor_end, raw, start, end)
            }
        };
        self.core.set_selection(anchor, caret);
        self.redraw = true;
        *handled = true;
    }

    fn handle_primary_press(
        &mut self,
        shift: bool,
        node_id: NodeId,
        focus_target: &mut Option<NodeId>,
        handled: &mut bool,
    ) {
        let point = self.last_cursor;
        if !self.rect.contains(point) {
            return;
        }
        // Clear any in-flight preedit before hit-testing so byte offsets are
        // in committed-text space.
        self.preedit.clear();
        let intent = self.drag_select.press(point, shift);
        let position = self.position_at(point);
        let byte = position.byte;
        match intent.granularity {
            Granularity::Char => self.core.place_position(position, intent.extend),
            Granularity::Word => {
                let (start, end) = word_boundaries(&self.text, byte);
                self.core.set_selection(start, end);
            }
            Granularity::Line => {
                let (start, end) = line_boundaries(&self.text, byte);
                self.core.set_selection(start, end);
            }
        }
        let selection = self.core.selection();
        self.drag_anchor_span = if intent.granularity == Granularity::Char {
            // Preserve the true (possibly right-hand) anchor for Shift-click.
            (selection.anchor, selection.anchor)
        } else {
            (selection.min(), selection.max())
        };
        self.focused = true;
        *focus_target = Some(node_id);
        self.goal_x = None;
        self.redraw = true;
        *handled = true;
    }
}

fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map_or(text.len(), |(b, _)| b)
}

impl Widget for RichTextEditor {
    crate::provides!(dyn crate::undo::UndoPoints);

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = crate::doc::document(state, path);
        let Some(field) = Self::ensure_map(state, path, &self.key) else {
            return;
        };
        let Some(text_obj) = Self::ensure_text(state, &field, "text") else {
            return;
        };
        self.text = state.text(&text_obj).unwrap_or_default();
        self.style = match state.get(&field, "style") {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Str(v) => RichBlockStyle::from_str(v.as_str()),
                _ => RichBlockStyle::Body,
            },
            _ => RichBlockStyle::Body,
        };
        self.rebuild_marks(state, &text_obj);
        self.field_obj = Some(field);
        self.text_obj = Some(text_obj);
        self.core.clamp_to(&self.text);
        self.layout_dirty = true;
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let width = known.width.unwrap_or(match available.width {
            AvailableSpace::Definite(w) => w,
            _ => 400.0,
        });
        let indent = if self.style == RichBlockStyle::Bullet {
            px(BULLET_INDENT)
        } else {
            0.0
        };
        let inner_w = (width - 2.0 * px(PAD) - indent).max(1.0);
        self.ensure_layout(text_renderer, inner_w);
        let text_h = self.layout.as_ref().map_or(
            self.base_font_size * 1.4,
            super::super::richtext::RichTextLayout::height,
        );
        taffy::Size {
            width,
            height: text_h + 2.0 * px(PAD),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        self.layout_dirty = true;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.last_caret_width = theme.caret_width;
        let radius = f64::from(theme.tokens.radii.control);
        if self.frame {
            let body = RoundedRect::from_rect(self.rect, radius);
            scene.fill(Fill::NonZero, transform, theme.bg_primary, None, &body);
            let border = if self.focused {
                theme.border_focus
            } else {
                crate::surface::edge_on(theme.bg_primary)
            };
            scene.stroke(&Stroke::new(1.0), transform, border, None, &body);
        }

        let width = self.text_width();
        self.ensure_layout(text_renderer, width);
        let origin = self.text_origin();
        let t = transform.pre_translate(Vec2::new(origin.x, origin.y));
        // `ensure_layout` just shaped it (or found it already fresh); bail
        // rather than trust that blindly if it somehow didn't.
        let Some(layout) = self.layout.as_ref() else {
            self.redraw = false;
            return;
        };

        // Bullet marker.
        if self.style == RichBlockStyle::Bullet {
            let size = f64::from(self.base_font_size);
            let cy = self.content_rect().y0 + size * 0.7;
            let cx = self.content_rect().x0 + BULLET_INDENT * 0.4;
            scene.fill(
                Fill::NonZero,
                transform,
                theme.text_primary,
                None,
                &Circle::new(Point::new(cx, cy), size * 0.16),
            );
        }

        // Selection (only in the committed, non-preedit state).
        if self.preedit.is_empty() {
            let r = self.core.range();
            if r.start < r.end {
                let sel = theme.tokens.colors.intent.selection_tint;
                for (x0, y0, x1, y1) in layout.selection_rects(r.start, r.end) {
                    let rect =
                        Rect::new(f64::from(x0), f64::from(y0), f64::from(x1), f64::from(y1));
                    scene.fill(Fill::NonZero, t, sel, None, &rect);
                }
            }
        }

        // Placeholder when empty and unfocused.
        if self.text.is_empty()
            && self.preedit.is_empty()
            && !self.focused
            && !self.placeholder.is_empty()
        {
            text_renderer.draw_text(
                scene,
                &self.placeholder,
                self.base_font_size,
                theme.text_dim,
                t,
                None,
            );
        } else {
            text_renderer.draw_rich(scene, layout, t, theme.text_primary);
        }

        // Caret (solid; no blink → no idle redraw).
        if self.focused {
            let (cx, top, h) = layout.caret_geometry_at(self.display_caret_position());
            let caret = Rect::new(
                f64::from(cx),
                f64::from(top),
                f64::from(cx) + theme.caret_width,
                f64::from(top + h),
            );
            scene.fill(
                Fill::NonZero,
                t,
                theme.tokens.colors.intent.you,
                None,
                &caret,
            );
        }
        self.redraw = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let ctrl = ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key();
        let shift = ctx.modifiers.state().shift_key();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.handle_cursor_move(point, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_primary_press(shift, ctx.node_id, ctx.focus_target, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.drag_select.release() {
                    *ctx.handled = true;
                }
            }
            WindowEvent::Ime(Ime::Commit(s)) => {
                if !self.focused {
                    return;
                }
                self.preedit.clear();
                self.preedit_cursor = None;
                self.insert_str(ctx.state, s);
                *ctx.handled = true;
            }
            WindowEvent::Ime(Ime::Preedit(s, cursor)) => {
                if !self.focused {
                    return;
                }
                s.clone_into(&mut self.preedit);
                self.preedit_cursor = *cursor;
                self.redraw = true;
                self.layout_dirty = true;
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        text,
                        ..
                    },
                ..
            } => {
                if !self.focused {
                    return;
                }
                self.handle_key_press(
                    logical_key,
                    text.as_deref(),
                    ctrl,
                    shift,
                    ctx.state,
                    ctx.handled,
                );
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "RichTextEditor"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.content_rect()
            .contains(point)
            .then_some(winit::window::CursorIcon::Text)
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            if !focused {
                self.preedit.clear();
                self.preedit_cursor = None;
            }
            self.redraw = true;
        }
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn ime_cursor_rect(&self) -> Option<Rect> {
        if !self.focused {
            return None;
        }
        let layout = self.layout.as_ref()?;
        let (x, top, height) = layout.caret_geometry_at(self.display_caret_position());
        let origin = self.text_origin();
        Some(Rect::new(
            origin.x + f64::from(x),
            origin.y + f64::from(top),
            origin.x + f64::from(x) + self.last_caret_width,
            origin.y + f64::from(top + height),
        ))
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Some(winit::window::ImePurpose::Normal)
    }

    fn needs_redraw(&self) -> bool {
        self.redraw
    }
    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::TextInput)
                .with_optional_value(Some(self.text.clone()))
                .with_text_selection(self.core.selection().anchor, self.core.selection().cursor)
                .with_state(AccessState {
                    focusable: true,
                    focused: self.focused,
                    ..Default::default()
                }),
        )
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        let (state, _) = crate::doc::document(state, path);

        match action {
            Action::SetValue(value) => {
                let cleaned = value
                    .chars()
                    .filter_map(|c| match c {
                        '\n' | '\r' => Some(' '),
                        c if c.is_control() => None,
                        c => Some(c),
                    })
                    .collect::<String>();
                self.core.set_selection(0, self.text.len());
                let edit = self.core.insert(&cleaned);
                self.apply_edit(state, &edit);
                true
            }
            Action::SetSelection { anchor, focus } => {
                let byte_at = |character_index: usize| {
                    crate::text_break::grapheme_indices(&self.text)
                        .nth(character_index)
                        .map_or(self.text.len(), |(byte, _)| byte)
                };
                self.core.set_selection(byte_at(*anchor), byte_at(*focus));
                self.goal_x = None;
                self.redraw = true;
                true
            }
            _ => false,
        }
    }
}

impl crate::undo::UndoPoints for RichTextEditor {
    fn needs_commit(&self) -> bool {
        self.last_edit_time
            .is_some_and(|t| t.elapsed() >= Duration::from_secs(1))
    }
    fn on_commit(&mut self) {
        self.last_edit_time = None;
    }
    fn get_state(&self) -> Option<String> {
        let affinity = match self.core.caret_position().affinity {
            TextAffinity::Before => "b",
            TextAffinity::After => "a",
        };
        Some(format!(
            "{}:{}:{affinity}",
            self.core.selection().anchor,
            self.core.selection().cursor
        ))
    }
    fn set_state(&mut self, s: &str) {
        let mut parts = s.split(':');
        if let (Some(a), Some(c)) = (parts.next(), parts.next())
            && let (Ok(a), Ok(c)) = (a.parse::<usize>(), c.parse::<usize>())
        {
            let affinity = if parts.next() == Some("a") {
                TextAffinity::After
            } else {
                TextAffinity::Before
            };
            self.core.set_selection(a, c);
            self.core
                .place_position(TextPosition::with_affinity(c, affinity), true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::State;
    use vello::kurbo::Affine;
    use winit::event::Modifiers;
    use winit::keyboard::{ModifiersState, SmolStr};

    fn setup() -> (State, RichTextEditor) {
        let mut state = State::new();
        let mut ed = RichTextEditor::new("doc");
        ed.sync(&mut state.doc, &state.root);
        ed.set_focus(true);
        (state, ed)
    }

    fn mods(ctrl: bool, shift: bool) -> Modifiers {
        let mut s = ModifiersState::empty();
        if ctrl {
            s |= ModifiersState::CONTROL;
        }
        if shift {
            s |= ModifiersState::SHIFT;
        }
        Modifiers::from(s)
    }

    fn char_ev(s: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(SmolStr::new(s));
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(SmolStr::new(s));
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }
    fn named_ev(k: NamedKey) -> WindowEvent {
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

    fn press(ed: &mut RichTextEditor, state: &mut State, ev: &WindowEvent, m: Modifiers) {
        let taffy = TaffyTree::<()>::new();
        let node = NodeId::from(0usize);
        let mut handled = false;
        let mut ft = None;
        ed.handle_event(
            ev,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &m,
                state: &mut state.doc,
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut ft,
                node_id: node,
                taffy: &taffy,
            },
        );
    }

    fn type_str(ed: &mut RichTextEditor, state: &mut State, s: &str) {
        for ch in s.chars() {
            let mut buf = [0u8; 4];
            press(
                ed,
                state,
                &char_ev(ch.encode_utf8(&mut buf)),
                mods(false, false),
            );
        }
    }

    /// Run the real five passes so `self.layout` is shaped (for hit-testing /
    /// vertical motion). Uses a real `TextRenderer` (cosmic-text, no GPU).
    fn layout(ed: &mut RichTextEditor, w: f32, h: f32) {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut taffy = TaffyTree::<()>::new();
        let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = ed.build_layout(&mut taffy, &theme, &mut nm);
        taffy
            .compute_layout_with_measure(
                node,
                Size {
                    width: AvailableSpace::Definite(w),
                    height: AvailableSpace::Definite(h),
                },
                |known, available, nid, _, _| {
                    nm.get(&nid).map_or(Size::ZERO, |&p| unsafe {
                        (*p).measure(known, available, &mut tr)
                    })
                },
            )
            .unwrap();
        ed.apply_layout(&taffy, node, Point::ZERO);
        // Build the layout cache at the final content width (what draw does).
        let mut scene = Scene::new();
        ed.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::default(),
        );
    }

    fn crdt_text_obj(state: &State) -> ObjId {
        let (_, field) = state.doc.get(&state.root, "doc").unwrap().unwrap();
        let (_, tid) = state.doc.get(&field, "text").unwrap().unwrap();
        tid
    }

    /// A shaped test layout's width, floored to a pixel column count. Fixture
    /// geometry only — always small and non-negative, never anywhere near
    /// `usize`'s range.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a rendered test layout's width is always small and non-negative"
    )]
    fn width_columns(width: f32) -> usize {
        width.max(0.0).ceil() as usize
    }

    /// The inverse of scanning [`width_columns`]: a pixel column back to the
    /// `f32` `hit_position` expects. Fixture geometry only — always far below
    /// `f32`'s exact-integer range.
    #[allow(
        clippy::cast_precision_loss,
        reason = "a scanned test pixel column is always small, not a large count"
    )]
    fn column_px(x: usize) -> f32 {
        x as f32
    }

    #[test]
    fn typing_and_deletion_edit_the_crdt_text() {
        // Case `typing_inserts_into_the_crdt_text`.
        {
            let (mut state, mut ed) = setup();
            type_str(&mut ed, &mut state, "hello");
            assert_eq!(ed.text(), "hello");
            assert_eq!(ed.caret(), 5);
            let tid = crdt_text_obj(&state);
            assert_eq!(
                state.doc.text(&tid).unwrap(),
                "hello",
                "characters land in the CRDT Text"
            );
        }

        // Case `backspace_char_and_word_and_delete`.
        {
            let (mut state, mut ed) = setup();
            type_str(&mut ed, &mut state, "one two");
            press(
                &mut ed,
                &mut state,
                &named_ev(NamedKey::Backspace),
                mods(false, false),
            );
            assert_eq!(ed.text(), "one tw");
            // Ctrl+Backspace deletes the rest of the word.
            press(
                &mut ed,
                &mut state,
                &named_ev(NamedKey::Backspace),
                mods(true, false),
            );
            assert_eq!(ed.text(), "one ");
            press(
                &mut ed,
                &mut state,
                &named_ev(NamedKey::Home),
                mods(false, false),
            );
            press(
                &mut ed,
                &mut state,
                &named_ev(NamedKey::Delete),
                mods(false, false),
            );
            assert_eq!(ed.text(), "ne ");
        }

        // Case `enter_is_a_noop_and_pasted_newlines_fold_to_spaces`.
        {
            let (mut state, mut ed) = setup();
            type_str(&mut ed, &mut state, "a");
            press(
                &mut ed,
                &mut state,
                &named_ev(NamedKey::Enter),
                mods(false, false),
            );
            type_str(&mut ed, &mut state, "b");
            // Single-paragraph: Enter inserted nothing; the buffer stays one line.
            assert_eq!(ed.text(), "ab");
            // A commit carrying a newline (e.g. a multi-line paste) folds to a space.
            press(
                &mut ed,
                &mut state,
                &WindowEvent::Ime(Ime::Commit("x\ny".to_string())),
                mods(false, false),
            );
            assert_eq!(ed.text(), "abx y");
        }

        // Case `rich_editor_preedit_honours_internal_caret_and_candidate_anchor`.
        {
            let (mut state, mut ed) = setup();
            type_str(&mut ed, &mut state, "ab");
            ed.core.set_caret(1);
            let composition = "漢字";
            press(
                &mut ed,
                &mut state,
                &WindowEvent::Ime(Ime::Preedit(
                    composition.into(),
                    Some(("漢".len(), "漢".len())),
                )),
                mods(false, false),
            );
            assert_eq!(ed.text(), "ab", "preedit is display-only");
            layout(&mut ed, 400.0, 80.0);
            let (_, _, display_caret, preedit) = ed.display_state();
            assert_eq!(display_caret, 1 + "漢".len());
            assert_eq!(preedit, Some(1..1 + composition.len()));
            assert!(Widget::ime_cursor_rect(&ed).is_some());

            press(
                &mut ed,
                &mut state,
                &WindowEvent::Ime(Ime::Commit(composition.into())),
                mods(false, false),
            );
            assert_eq!(ed.text(), "a漢字b");
            assert!(ed.preedit.is_empty());
        }
    }

    #[test]
    fn caret_motion_and_selection_follow_words_lines_and_visual_bidi() {
        // Case `arrow_and_word_motion_walk_the_caret`.
        {
            let (mut state, mut ed) = setup();
            type_str(&mut ed, &mut state, "hi there");
            press(
                &mut ed,
                &mut state,
                &named_ev(NamedKey::ArrowLeft),
                mods(false, false),
            );
            assert_eq!(ed.caret(), 7);
            // Ctrl+Left → previous word boundary ("there" starts at byte 3).
            press(
                &mut ed,
                &mut state,
                &named_ev(NamedKey::ArrowLeft),
                mods(true, false),
            );
            assert_eq!(ed.caret(), 3);
            press(
                &mut ed,
                &mut state,
                &named_ev(NamedKey::Home),
                mods(false, false),
            );
            assert_eq!(ed.caret(), 0);
            press(
                &mut ed,
                &mut state,
                &named_ev(NamedKey::End),
                mods(false, false),
            );
            assert_eq!(ed.caret(), 8);
        }

        // Case `shift_arrow_selects_then_typing_replaces_selection`.
        {
            let (mut state, mut ed) = setup();
            type_str(&mut ed, &mut state, "abcdef");
            press(
                &mut ed,
                &mut state,
                &named_ev(NamedKey::Home),
                mods(false, false),
            );
            for _ in 0..3 {
                press(
                    &mut ed,
                    &mut state,
                    &named_ev(NamedKey::ArrowRight),
                    mods(false, true),
                );
            }
            assert_eq!(ed.selection(), 0..3, "shift+right extends the selection");
            type_str(&mut ed, &mut state, "X");
            assert_eq!(ed.text(), "Xdef", "typing over a selection replaces it");
        }

        // Case `select_all_via_ctrl_a`.
        {
            let (mut state, mut ed) = setup();
            type_str(&mut ed, &mut state, "pick me");
            press(&mut ed, &mut state, &char_ev("a"), mods(true, false));
            assert_eq!(ed.selection(), 0..7);
        }

        // Case `rich_editor_arrows_follow_rtl_visual_direction`.
        {
            let (mut state, mut ed) = setup();
            let text = "שלום";
            type_str(&mut ed, &mut state, text);
            layout(&mut ed, 400.0, 80.0);
            ed.core.set_caret(0);
            press(
                &mut ed,
                &mut state,
                &named_ev(NamedKey::ArrowLeft),
                mods(false, false),
            );
            assert_eq!(ed.caret(), text.chars().next().unwrap().len_utf8());
            press(
                &mut ed,
                &mut state,
                &named_ev(NamedKey::ArrowRight),
                mods(false, false),
            );
            assert_eq!(ed.caret(), 0);
        }

        // Case `rich_editor_arrows_retain_mixed_bidi_affinity`.
        {
            let (mut state, mut ed) = setup();
            type_str(&mut ed, &mut state, "abc אבג xyz");
            layout(&mut ed, 400.0, 80.0);
            let shaped = ed.layout.as_ref().unwrap();
            let (_, top, height) = shaped.caret_geometry(0);
            let y = top + height * 0.5;
            let mut positions = Vec::new();
            for x in 0..=width_columns(shaped.width()) {
                let position = shaped.hit_position(column_px(x), y);
                if positions.last() != Some(&position) {
                    positions.push(position);
                }
            }
            let (a, b) = positions
                .iter()
                .find_map(|a| {
                    let ax = shaped.caret_geometry_at(*a).0;
                    positions
                        .iter()
                        .find(|b| {
                            a.byte == b.byte
                                && a.affinity != b.affinity
                                && (ax - shaped.caret_geometry_at(**b).0).abs() > 1.0
                        })
                        .map(|b| (*a, *b))
                })
                .expect("mixed BiDi exposes an ambiguous boundary");
            let (left, right) = if shaped.caret_geometry_at(a).0 < shaped.caret_geometry_at(b).0 {
                (a, b)
            } else {
                (b, a)
            };

            ed.core.set_caret_position(left);
            for _ in 0..16 {
                if ed.caret_position() == right {
                    break;
                }
                press(
                    &mut ed,
                    &mut state,
                    &named_ev(NamedKey::ArrowRight),
                    mods(false, false),
                );
            }
            assert_eq!(ed.caret_position(), right);
            assert_eq!(left.byte, right.byte);
            assert_ne!(left.affinity, right.affinity);
        }

        // Case `arrow_down_up_move_across_wrapped_lines`.
        {
            let (mut state, mut ed) = setup();
            // A single paragraph long enough to wrap at a narrow width.
            type_str(
                &mut ed,
                &mut state,
                "alpha bravo charlie delta echo foxtrot golf hotel india",
            );
            layout(&mut ed, 160.0, 240.0);
            press(
                &mut ed,
                &mut state,
                &named_ev(NamedKey::Home),
                mods(true, false),
            );
            assert_eq!(ed.caret(), 0);
            press(
                &mut ed,
                &mut state,
                &named_ev(NamedKey::ArrowDown),
                mods(false, false),
            );
            let down = ed.caret();
            assert!(
                down > 0,
                "ArrowDown moves to a lower wrapped line, got {down}"
            );
            press(
                &mut ed,
                &mut state,
                &named_ev(NamedKey::ArrowUp),
                mods(false, false),
            );
            assert!(
                ed.caret() < down,
                "ArrowUp moves back toward the first line, got {}",
                ed.caret()
            );
        }
    }

    #[test]
    fn marks_block_style_and_caret_state_persist() {
        // Case `bold_toggle_writes_and_clears_an_automerge_mark`.
        {
            let (mut state, mut ed) = setup();
            type_str(&mut ed, &mut state, "abcdef");
            // Select "abc" (Home + 3× shift-right) and bold it via the production
            // toolbar method (the same one Ctrl+B invokes).
            press(
                &mut ed,
                &mut state,
                &named_ev(NamedKey::Home),
                mods(false, false),
            );
            for _ in 0..3 {
                press(
                    &mut ed,
                    &mut state,
                    &named_ev(NamedKey::ArrowRight),
                    mods(false, true),
                );
            }
            ed.toggle_bold(&mut state.doc);
            let tid = crdt_text_obj(&state);
            let marks = state.doc.marks(&tid).unwrap();
            let bold: Vec<_> = marks.iter().filter(|m| m.name.as_str() == "bold").collect();
            assert_eq!(bold.len(), 1, "one bold mark written");
            assert_eq!(
                (bold[0].start, bold[0].end),
                (0, 3),
                "bold covers the selected chars"
            );

            // A fresh editor reads the mark back as a byte-range MarkSpan.
            let mut ed2 = RichTextEditor::new("doc");
            ed2.sync(&mut state.doc, &state.root);
            assert!(
                ed2.marks
                    .iter()
                    .any(|s| s.mark == Mark::Bold && s.range == (0..3)),
                "the bold span round-trips through the CRDT: {:?}",
                ed2.marks
            );

            // Toggling the same selection again removes the mark.
            ed.toggle_bold(&mut state.doc);
            let marks = state.doc.marks(&tid).unwrap();
            assert!(
                marks
                    .iter()
                    .all(|m| m.name.as_str() != "bold" || m.value == ScalarValue::Boolean(false)),
                "toggling the covered range again clears bold"
            );
        }

        // Case `block_style_persists_to_the_crdt`.
        {
            let (mut state, mut ed) = setup();
            ed.set_block_style(&mut state.doc, RichBlockStyle::Heading);
            assert_eq!(ed.block_style(), RichBlockStyle::Heading);
            // A fresh editor reads the persisted style.
            let mut ed2 = RichTextEditor::new("doc");
            ed2.sync(&mut state.doc, &state.root);
            assert_eq!(
                ed2.block_style(),
                RichBlockStyle::Heading,
                "style survives via the CRDT"
            );
        }

        // Case `caret_state_round_trips_for_undo_restore`.
        {
            let (mut state, mut ed) = setup();
            type_str(&mut ed, &mut state, "abcdef");
            press(
                &mut ed,
                &mut state,
                &named_ev(NamedKey::Home),
                mods(false, false),
            );
            for _ in 0..2 {
                press(
                    &mut ed,
                    &mut state,
                    &named_ev(NamedKey::ArrowRight),
                    mods(false, true),
                );
            }
            let saved = crate::undo::get_state(&ed).unwrap();
            // A rebuild resets the caret; set_state restores it.
            let mut ed2 = RichTextEditor::new("doc");
            ed2.sync(&mut state.doc, &state.root);
            crate::undo::set_state(&mut ed2, &saved);
            assert_eq!(
                ed2.selection(),
                0..2,
                "caret + selection restore across a rebuild"
            );
        }
    }

    #[test]
    fn pointer_clicks_seat_the_caret_and_select_words_and_lines() {
        // Case `click_seats_caret_and_double_click_selects_a_word`.
        {
            let (mut state, mut ed) = setup();
            type_str(&mut ed, &mut state, "hello world");
            layout(&mut ed, 400.0, 80.0);
            let origin = ed.text_origin();
            // A click at the very start seats the caret at the beginning.
            press(
                &mut ed,
                &mut state,
                &mouse_move(origin.x, origin.y + 4.0),
                mods(false, false),
            );
            press(&mut ed, &mut state, &left_press(), mods(false, false));
            press(&mut ed, &mut state, &left_release(), mods(false, false));
            assert!(
                ed.caret() <= 1,
                "click at the start seats the caret near 0, got {}",
                ed.caret()
            );
            // A double-click inside "hello" selects the whole word.
            let p = mouse_move(origin.x + 12.0, origin.y + 4.0);
            press(&mut ed, &mut state, &p, mods(false, false));
            press(&mut ed, &mut state, &left_press(), mods(false, false));
            press(&mut ed, &mut state, &left_release(), mods(false, false));
            press(&mut ed, &mut state, &p, mods(false, false));
            press(&mut ed, &mut state, &left_press(), mods(false, false));
            assert_eq!(
                ed.selection(),
                0..5,
                "double-click selects the word under the pointer"
            );
        }

        // Case `double_click_drag_expands_by_words_and_triple_click_selects_the_line`.
        {
            let (mut state, mut ed) = setup();
            type_str(&mut ed, &mut state, "hello world");
            layout(&mut ed, 400.0, 80.0);
            let origin = ed.text_origin();
            let hello = mouse_move(origin.x + 12.0, origin.y + 4.0);
            let world = mouse_move(origin.x + 90.0, origin.y + 4.0);

            press(&mut ed, &mut state, &hello, mods(false, false));
            press(&mut ed, &mut state, &left_press(), mods(false, false));
            press(&mut ed, &mut state, &left_release(), mods(false, false));
            press(&mut ed, &mut state, &left_press(), mods(false, false));
            press(&mut ed, &mut state, &world, mods(false, false));
            assert_eq!(
                ed.selection(),
                0..11,
                "dragging from a double-click expands in whole-word units"
            );

            press(&mut ed, &mut state, &left_release(), mods(false, false));
            press(&mut ed, &mut state, &hello, mods(false, false));
            press(&mut ed, &mut state, &left_press(), mods(false, false));
            assert_eq!(
                ed.selection(),
                0..11,
                "triple-click selects the paragraph's whole logical line"
            );
        }

        // Case `text_cursor_over_the_body_and_focusable`.
        {
            let (mut state, mut ed) = setup();
            type_str(&mut ed, &mut state, "x");
            layout(&mut ed, 400.0, 80.0);
            assert!(Widget::is_focusable(&ed));
            let inside = ed.content_rect().center();
            assert_eq!(
                Widget::cursor_shape(&ed, inside),
                Some(winit::window::CursorIcon::Text)
            );
        }

        // Case `accessibility_reports_a_text_input`.
        {
            use crate::a11y::{AccessibleTextSelection, Action, Role};
            let (mut state, mut ed) = setup();
            type_str(&mut ed, &mut state, "a👩‍👩‍👧‍👦b");
            ed.core.set_selection(1, 1 + "👩‍👩‍👧‍👦".len());
            let node = Widget::accessibility(&ed).unwrap();
            assert_eq!(node.role, Role::TextInput);
            assert_eq!(node.value.as_deref(), Some("a👩‍👩‍👧‍👦b"));
            assert_eq!(
                node.text_selection,
                Some(AccessibleTextSelection {
                    anchor: 1,
                    focus: 1 + "👩‍👩‍👧‍👦".len(),
                })
            );
            assert!(node.state.focusable);

            assert!(Widget::perform_action(
                &mut ed,
                &Action::SetSelection {
                    anchor: 1,
                    focus: 2,
                },
                &mut state.doc,
                &state.root,
            ));
            assert_eq!(ed.selection(), 1..1 + "👩‍👩‍👧‍👦".len());
            assert!(Widget::perform_action(
                &mut ed,
                &Action::SetValue("replacement".into()),
                &mut state.doc,
                &state.root,
            ));
            assert_eq!(ed.text(), "replacement");
            assert_eq!(
                state.doc.text(ed.text_obj.as_ref().unwrap()).unwrap(),
                "replacement"
            );
        }
    }

    #[test]
    fn unchanged_browser_rich_text_reshapes_after_font_generation_changes() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
        let fonts = crate::font_catalog::FontCatalog::product("", "en-US")
            .load_browser_startup_bytes(|name| std::fs::read(root.join(name)).ok())
            .unwrap();
        let mut renderer = TextRenderer::with_browser_font_system(fonts);
        let mut editor = RichTextEditor::new("doc");
        editor.text = "مَرْحَبًا".to_owned();
        editor.ensure_layout(&mut renderer, 300.0);
        assert_eq!(
            renderer.take_browser_font_demands(),
            ["NotoSansArabic[wght].ttf"]
        );
        renderer.browser_font_failed();
        editor.ensure_layout(&mut renderer, 300.0);
        assert_eq!(
            renderer.take_browser_font_demands(),
            ["NotoSansArabic[wght].ttf"]
        );
        renderer
            .install_browser_font(
                "NotoSansArabic[wght].ttf",
                std::fs::read(root.join("NotoSansArabic[wght].ttf")).unwrap(),
            )
            .unwrap();
        editor.ensure_layout(&mut renderer, 300.0);
        assert!(
            editor
                .layout
                .as_ref()
                .unwrap()
                .buffer
                .layout_runs()
                .flat_map(|run| run.glyphs)
                .all(|glyph| glyph.glyph_id != 0)
        );
    }
}
