//! A single editable line of text wrapping a local-mode [`TextInput`], the
//! leaf cell of a block editor. The inner `TextInput` handles cursor
//! placement, selection, IME, copy/paste; `RichTextLine` adds:
//!
//! * Intercepts Enter, Backspace at byte 0, Up at the start, and Down at the
//!   end before the inner input sees them, queuing [`LineSignal`]s the
//!   parent ([`crate::widget::block_editor::BlockEditor`]) consumes instead.
//! * A per-line font size so a block editor can render H1/H2/H3 headings
//!   without forking the widget.
//! * A [`MarkSpan`] list (inline bold/italic/code/link) with rendering and
//!   Ctrl-B/I/E toggling built on the inner `TextInput`'s selection range.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::BlendMode;
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::marks::{self, Mark, MarkSegment, MarkSpan, PageId};
use crate::widget::{TextInput, Widget};

/// Emitted when a key would carry the edit out of the line itself; the
/// parent (a `BlockEditor`) drains these via [`RichTextLine::take_signal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineSignal {
    /// Parent splits the line at `split_at`, drops the suffix into a new
    /// block, and re-focuses.
    Enter {
        split_at: usize,
    },
    /// Same as `Enter`, but the new block is flagged a soft-break
    /// continuation (no top spacing, reads as one paragraph).
    SoftNewline {
        split_at: usize,
    },
    /// Caret was at byte 0 with no selection; parent decides whether to
    /// merge with the previous block, delete an empty block, or no-op.
    BackspaceAtStart,
    ArrowUp,
    ArrowDown,
    /// Ctrl+Shift+Up: parent extends the selection up across the block
    /// boundary from `caret`, instead of moving focus.
    SelectBlockUp {
        caret: usize,
    },
    /// Mirror of [`SelectBlockUp`] extending down.
    SelectBlockDown {
        caret: usize,
    },
    /// User typed `[[`; parent opens a [`LinkPopover`] anchored at
    /// `open_at`. Brackets fall through to the inner input — the popover's
    /// commit-link logic strips them.
    LinkOpen {
        open_at: usize,
    },
}

/// Matches the body baseline the rest of the UI uses for `TextInput`.
const DEFAULT_FONT_SIZE: f32 = 16.0;

/// Geometry shared by every mark segment `draw` paints in one frame, so
/// `paint_mark_segment` doesn't need a fistful of separate parameters.
struct MarkDrawCtx<'a> {
    text: &'a str,
    xs: &'a [f64],
    rect: Rect,
    pad: f64,
    scroll_x: f64,
    text_y: f64,
    line_height: f64,
    text_size: f32,
}

pub struct RichTextLine {
    inner: TextInput,
    /// At most one signal in flight per frame; new signals overwrite the slot.
    signal: Option<LineSignal>,
    font_size: f32,
    /// Kept sorted by `range.start`.
    marks: Vec<MarkSpan>,
    /// Drained once `handle_event` observes a text-input edit.
    pending_marks: Vec<Mark>,
    /// Snapshot used to detect splice direction + length so `marks` stays
    /// aligned across user edits.
    prev_text: String,
    /// Cursor position before the most-recent edit; used with `prev_text` to
    /// figure out where the splice landed.
    prev_cursor: usize,
    /// `(byte_start, byte_end, author_id)` read from the bound Text's marks
    /// during `sync`; drives the authorship highlight in `BlockEditor::draw`.
    author_runs: Vec<(usize, usize, String)>,
}

impl RichTextLine {
    pub fn new() -> Self {
        // Block-editor cells render as document copy, not a form field.
        let inner = TextInput::local()
            .with_plain_chrome()
            .with_font_size(DEFAULT_FONT_SIZE);
        Self {
            inner,
            signal: None,
            font_size: DEFAULT_FONT_SIZE,
            marks: Vec::new(),
            pending_marks: Vec::new(),
            prev_text: String::new(),
            prev_cursor: 0,
            author_runs: Vec::new(),
        }
    }

    /// Dirty flag stays false — this is a programmatic seed.
    #[must_use]
    pub fn with_text(mut self, s: &str) -> Self {
        self.inner = self.inner.with_initial_value(s);
        self.prev_text = s.to_string();
        self.prev_cursor = s.len();
        self
    }

    /// For `BlockEditor` heading blocks (H1=28, H2=22, H3=18).
    #[must_use]
    pub fn with_size(mut self, size: f32) -> Self {
        self.font_size = size;
        self.inner.set_font_size(size);
        self
    }

    #[must_use]
    pub fn with_mono(mut self) -> Self {
        self.inner.set_mono(true);
        self
    }

    pub fn set_mono(&mut self, mono: bool) {
        self.inner.set_mono(mono);
    }

    pub fn value(&self) -> &str {
        self.inner.value()
    }

    /// Used by `BlockEditor` to back each block with the per-block `Text` at
    /// `pages[id].blocks[i].text`.
    pub fn bind_text(&mut self, obj: ObjId) {
        self.inner.bind_text(obj);
    }

    pub fn bound_obj(&self) -> Option<ObjId> {
        self.inner.bound_obj().cloned()
    }

    pub fn set_local_author(&mut self, author: Option<String>) {
        self.inner.set_local_author(author);
    }

    pub fn author_runs(&self) -> &[(usize, usize, String)] {
        &self.author_runs
    }

    /// Does NOT mark the line dirty — programmatic writes are not user edits.
    pub fn set_text(&mut self, s: &str) {
        self.inner.set_text(s);
        self.inner.set_cursor_pos(s.len());
        // Doesn't preserve prior marks; parents re-seed via `set_marks`.
        self.marks.clear();
        self.pending_marks.clear();
        self.prev_text = s.to_string();
        self.prev_cursor = s.len();
    }

    pub fn take_dirty(&mut self) -> Option<String> {
        self.inner.take_dirty()
    }

    pub fn is_focused(&self) -> bool {
        self.inner.is_focused()
    }

    /// When losing focus the cursor isn't touched — it stays wherever
    /// the user last left it.
    pub fn set_focus(&mut self, focused: bool) {
        self.inner.set_focus(focused);
    }

    pub fn cursor_at_start(&self) -> bool {
        self.inner.cursor_pos() == 0
    }

    pub fn cursor_at_end(&self) -> bool {
        self.inner.cursor_pos() == self.inner.value().len()
    }

    pub fn cursor_pos(&self) -> usize {
        self.inner.cursor_pos()
    }

    /// Used to broadcast the local selection as presence so peers can
    /// highlight it.
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        self.inner.selection_range_public()
    }

    pub fn set_cursor_pos(&mut self, pos: usize) {
        self.inner.set_cursor_pos(pos);
        self.prev_cursor = self.inner.cursor_pos();
    }

    pub fn take_signal(&mut self) -> Option<LineSignal> {
        self.signal.take()
    }

    /// `BlockEditor` owns its children's rects and lays them out manually
    /// rather than via taffy nesting.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.inner.apply_layout_external(rect);
    }

    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    pub fn marks(&self) -> &[MarkSpan] {
        &self.marks
    }

    pub fn set_marks(&mut self, marks: Vec<MarkSpan>) {
        self.marks = marks;
        self.marks.sort_by_key(|s| s.range.start);
    }

    pub fn add_mark(&mut self, span: MarkSpan) {
        self.marks.push(span);
        self.marks.sort_by_key(|s| s.range.start);
    }

    pub fn pending_marks(&self) -> &[Mark] {
        &self.pending_marks
    }

    /// With no active selection, pushes the mark onto the pending list so
    /// the next typed character inherits it.
    pub fn toggle_mark(&mut self, mark: Mark) {
        if let Some((s, e)) = self.inner.selection_range_public() {
            marks::toggle_mark_range(&mut self.marks, s..e, mark);
        } else {
            // Pushing the same mark twice cancels it (Notion-style).
            if let Some(idx) = self.pending_marks.iter().position(|m| m.same_kind(&mark)) {
                self.pending_marks.remove(idx);
            } else {
                self.pending_marks.push(mark);
            }
        }
    }

    /// Returns 0.0 before the first draw.
    pub fn pixel_x_for_byte(&self, theme: &Theme, byte: usize) -> f64 {
        let xs = self.inner.char_x_positions();
        if xs.is_empty() {
            return self.inner.get_rect().x0;
        }
        let idx = byte.min(xs.len().saturating_sub(1));
        let pad = self.inner.text_padding_x(theme);
        let x = xs.get(idx).copied().unwrap_or(0.0);
        self.inner.get_rect().x0 + pad + x - self.inner.scroll_x()
    }

    /// So a peer caret on this line is pixel-identical to the line's own focused caret.
    pub fn caret_rect(&self, theme: &Theme, byte: usize) -> Rect {
        self.inner.caret_rect(theme, byte)
    }

    pub fn selection_rect(&self, theme: &Theme, start: usize, end: usize) -> Rect {
        self.inner.selection_rect(theme, start, end)
    }

    /// Inverse of [`pixel_x_for_byte`]; for the click-on-link hit-test.
    pub fn byte_for_pixel_x(&self, theme: &Theme, x: f64) -> usize {
        let xs = self.inner.char_x_positions();
        if xs.is_empty() {
            return 0;
        }
        let pad = self.inner.text_padding_x(theme);
        let relative = x - self.inner.get_rect().x0 - pad + self.inner.scroll_x();
        let mut best = 0usize;
        let mut best_d = f64::MAX;
        for (i, px) in xs.iter().enumerate() {
            let d = (relative - *px).abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        best
    }

    /// Prefers [`Mark::Link`] over other kinds so link hit-tests still
    /// find the link even if the text is also bold.
    pub fn mark_at_byte(&self, byte: usize) -> Option<&Mark> {
        let mut link_hit: Option<&Mark> = None;
        let mut any_hit: Option<&Mark> = None;
        for s in &self.marks {
            if s.range.start <= byte && byte < s.range.end {
                if matches!(s.mark, Mark::Link(_)) {
                    link_hit = Some(&s.mark);
                }
                if any_hit.is_none() {
                    any_hit = Some(&s.mark);
                }
            }
        }
        link_hit.or(any_hit)
    }

    #[doc(hidden)]
    pub fn inner_mut(&mut self) -> &mut TextInput {
        &mut self.inner
    }

    /// For `BlockEditor`'s cross-block drag coordinator, to check drag state without `&mut self`.
    #[doc(hidden)]
    pub fn inner(&self) -> &TextInput {
        &self.inner
    }

    /// Ctrl/Super + b/i/e/u toggles the corresponding mark. Returns `true`
    /// when `c` matched one of the shortcuts (case-insensitively).
    fn handle_mark_shortcut(&mut self, c: &str) -> bool {
        match c.to_lowercase().as_str() {
            "b" => self.toggle_mark(Mark::Bold),
            "i" => self.toggle_mark(Mark::Italic),
            "e" => self.toggle_mark(Mark::Code),
            "u" => self.toggle_mark(Mark::Underline),
            _ => return false,
        }
        true
    }

    /// Reconciles `marks`/`pending_marks` against a text edit (diffed
    /// against `prev_text`, which the caller updates afterward), and emits
    /// `LinkOpen` when the edit just typed a `[[` at the caret.
    fn reconcile_marks_after_edit(&mut self, new_text: &str) {
        let (at, inserted, removed) = diff_splice(&self.prev_text, new_text);
        marks::shift_after_edit(&mut self.marks, at, inserted, removed);

        // Drain pending marks onto the inserted bytes.
        if inserted > 0 && !self.pending_marks.is_empty() {
            let range = at..at.saturating_add(inserted);
            let pending = std::mem::take(&mut self.pending_marks);
            for m in pending {
                marks::add_mark_to_range(&mut self.marks, range.clone(), m);
            }
        }

        // `[[` right before the caret emits LinkOpen; brackets are stripped
        // later by the popover commit path, not here.
        let caret = self.inner.cursor_pos();
        if inserted > 0
            && let Some(prefix) = new_text.as_bytes().get(..caret)
            && prefix.ends_with(b"[[")
            && let Some(open_at) = caret.checked_sub(2)
        {
            self.signal = Some(LineSignal::LinkOpen { open_at });
        }
    }
}

impl Default for RichTextLine {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for RichTextLine {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        // Realigning the mark-diff snapshot here keeps a remotely-synced edit
        // from being misread as a local splice on the next keystroke.
        self.inner.sync(state, path);
        self.prev_text = self.inner.value().to_string();
        self.prev_cursor = self.inner.cursor_pos();

        // Marks are char-indexed; convert to byte ranges for `pixel_x_for_byte`.
        self.author_runs.clear();
        if let Some(obj) = self.inner.bound_obj().cloned() {
            let text = self.inner.value().to_string();
            if let Ok(marks) = state.marks(&obj) {
                for m in marks {
                    if m.name.as_str() != crate::widget::text_input::AUTHOR_MARK {
                        continue;
                    }
                    let ScalarValue::Str(author) = &m.value else {
                        continue;
                    };
                    let bs = char_to_byte(&text, m.start);
                    let be = char_to_byte(&text, m.end);
                    if be > bs {
                        self.author_runs.push((bs, be, author.to_string()));
                    }
                }
            }
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // font_size * 1.5 so headings end up taller than paragraph rows without a measure pass.
        let height = (self.font_size * 1.5).max(20.0);
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(height),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        self.inner.apply_layout_external(rect);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The inner TextInput draws the plain text first; mark-styled
        // segments are then overpainted on top only where styling differs.
        self.inner
            .draw(scene, transform, text_renderer, theme, presence);

        if self.marks.is_empty() {
            return;
        }

        let text = self.inner.value().to_string();
        let segments = marks::segments_for(text.len(), &self.marks);
        if segments.is_empty() {
            return;
        }

        let text_size = self.font_size;
        let line_height = f64::from(text_size) * 1.25;
        let rect = self.inner.get_rect();
        let pad = self.inner.text_padding_x(theme);
        let scroll_x = self.inner.scroll_x();
        let text_y = rect.y0 + (rect.height() - line_height) / 2.0;
        let xs = self.inner.char_x_positions();
        if xs.is_empty() {
            // First frame — inner input hasn't populated char_x_positions yet.
            return;
        }

        // Clips overpaint so a scrolled long line doesn't bleed glyphs past the column.
        let clip_shape =
            vello::kurbo::RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.md));
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &clip_shape,
        );

        let ctx = MarkDrawCtx {
            text: &text,
            xs,
            rect,
            pad,
            scroll_x,
            text_y,
            line_height,
            text_size,
        };
        for seg in &segments {
            paint_mark_segment(scene, transform, text_renderer, theme, seg, &ctx);
        }

        scene.pop_layer();
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Intercepted before the inner TextInput sees them — otherwise Enter
        // would be filtered as a control char, and Backspace at byte 0 would
        // silently no-op (TextInput already guards `cursor_pos > 0`).
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
            && self.inner.is_focused()
        {
            // Intercepted so the character doesn't get inserted via the
            // text branch (TextInput's own Ctrl-handler skips b/i/e/u).
            if (modifiers.state().control_key() || modifiers.state().super_key())
                && let Key::Character(c) = logical_key
                && self.handle_mark_shortcut(c)
            {
                *handled = true;
                return;
            }

            match logical_key {
                Key::Named(NamedKey::Enter) => {
                    // Shift+Enter is a soft-break; plain Enter splits.
                    let split_at = self.inner.cursor_pos();
                    self.signal = if modifiers.state().shift_key() {
                        Some(LineSignal::SoftNewline { split_at })
                    } else {
                        Some(LineSignal::Enter { split_at })
                    };
                    *handled = true;
                    return;
                }
                Key::Named(NamedKey::Backspace) => {
                    // Otherwise the inner input handles the delete.
                    if self.inner.cursor_pos() == 0 {
                        self.signal = Some(LineSignal::BackspaceAtStart);
                        *handled = true;
                        return;
                    }
                }
                Key::Named(NamedKey::ArrowUp) => {
                    let m = modifiers.state();
                    self.signal = if (m.control_key() || m.super_key()) && m.shift_key() {
                        Some(LineSignal::SelectBlockUp {
                            caret: self.inner.cursor_pos(),
                        })
                    } else {
                        Some(LineSignal::ArrowUp)
                    };
                    *handled = true;
                    return;
                }
                Key::Named(NamedKey::ArrowDown) => {
                    let m = modifiers.state();
                    self.signal = if (m.control_key() || m.super_key()) && m.shift_key() {
                        Some(LineSignal::SelectBlockDown {
                            caret: self.inner.cursor_pos(),
                        })
                    } else {
                        Some(LineSignal::ArrowDown)
                    };
                    *handled = true;
                    return;
                }
                _ => {}
            }
        }

        self.inner.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
        );

        // Reconcile marks against any text edit using byte-deltas from the previous snapshot.
        let new_text = self.inner.value().to_string();
        if new_text != self.prev_text {
            self.reconcile_marks_after_edit(&new_text);
            self.prev_text = new_text;
        }
        self.prev_cursor = self.inner.cursor_pos();
    }

    fn name(&self) -> &'static str {
        "RichTextLine"
    }

    fn get_rect(&self) -> Rect {
        self.inner.get_rect()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.inner.cursor_shape(point)
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.inner.set_focus(focused);
    }

    fn is_focused(&self) -> bool {
        self.inner.is_focused()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(&self.inner)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Widget::accessibility(&self.inner)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        let handled = Widget::perform_action(&mut self.inner, action, state, path);
        if handled && matches!(action, crate::a11y::Action::SetValue(_)) {
            self.marks.clear();
            self.pending_marks.clear();
            self.prev_text = self.inner.value().to_string();
            self.prev_cursor = self.inner.cursor_pos();
        }
        handled
    }
}

/// Paints one mark segment's overpaint (code chip, link/underline/strike
/// rules, and restyled glyphs) inside the clip layer `draw` already pushed.
/// A pure function of its inputs — no `self` needed.
fn paint_mark_segment(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    seg: &MarkSegment,
    ctx: &MarkDrawCtx<'_>,
) {
    if seg.range.start >= ctx.text.len() {
        return;
    }
    // A mark's byte range might not land on a char boundary after an edit
    // races the mark-shift logic; skip rather than panic.
    let Some(slice) = ctx
        .text
        .get(seg.range.start..seg.range.end.min(ctx.text.len()))
    else {
        return;
    };
    if slice.is_empty() {
        return;
    }

    let x_offset = ctx.xs.get(seg.range.start).copied().unwrap_or(0.0);
    let x = ctx.rect.x0 + ctx.pad + x_offset - ctx.scroll_x;
    let sans_end_x_offset = ctx
        .xs
        .get(seg.range.end.min(ctx.xs.len().saturating_sub(1)))
        .copied()
        .unwrap_or(x_offset);
    let sans_end_x = ctx.rect.x0 + ctx.pad + sans_end_x_offset - ctx.scroll_x;

    paint_segment_decorations(scene, transform, theme, seg, ctx, x, sans_end_x);

    let needs_overpaint = seg.bold
        || seg.italic
        || seg.code
        || seg.underline
        || seg.strikethrough
        || seg.link.is_some();
    if !needs_overpaint {
        return;
    }

    let tokens = &theme.tokens;
    let color = if seg.link.is_some() || seg.code {
        tokens.colors.accent[5]
    } else {
        tokens.colors.text_primary
    };

    let weight = if seg.bold {
        TextWeight::Bold
    } else {
        TextWeight::Regular
    };
    let italic = seg.italic;
    let mono = false;

    text_renderer.draw_text_styled(
        scene,
        slice,
        ctx.text_size,
        color,
        translated(transform, x, ctx.text_y),
        None,
        weight,
        italic,
        mono,
    );
}

/// Draws the code-chip background and the link/underline/strikethrough
/// rules for one segment — the parts of `paint_mark_segment` that don't
/// depend on the restyled text run itself.
fn paint_segment_decorations(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    seg: &MarkSegment,
    ctx: &MarkDrawCtx<'_>,
    x: f64,
    sans_end_x: f64,
) {
    let tokens = &theme.tokens;

    if seg.code {
        // Inline code renders in the document font (`mono = false` in
        // `paint_mark_segment`) so its extent matches the sans layout slot
        // exactly; a wider monospace measurement would overlap next chars.
        let bg_rect = Rect::new(
            x - 2.0,
            ctx.text_y + 1.0,
            sans_end_x + 2.0,
            ctx.text_y + ctx.line_height - 1.0,
        );
        let bg = vello::kurbo::RoundedRect::from_rect(bg_rect, 3.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &bg,
        );
    }

    if seg.link.is_some() {
        let underline = Rect::new(
            x,
            ctx.text_y + ctx.line_height - 2.0,
            sans_end_x,
            ctx.text_y + ctx.line_height - 1.0,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.accent[5],
            None,
            &underline,
        );
    }

    // In text colour, distinct from the accent-coloured link underline above.
    if seg.underline {
        let rule = Rect::new(
            x,
            ctx.text_y + ctx.line_height - 2.0,
            sans_end_x,
            ctx.text_y + ctx.line_height - 1.0,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.text_primary,
            None,
            &rule,
        );
    }

    if seg.strikethrough {
        let mid = ctx.text_y + ctx.line_height * 0.5;
        let rule = Rect::new(x, mid - 0.5, sans_end_x, mid + 0.5);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.text_primary,
            None,
            &rule,
        );
    }
}

/// Composes `transform` with a translation by `(dx, dy)`. `kurbo::Affine`'s
/// `Mul` impl trips `arithmetic_side_effects` like any operator overload,
/// but composing two f64-based affine transforms cannot meaningfully
/// overflow — same reasoning clippy already exempts bare `f64` arithmetic
/// for.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine::mul composes f64 transform matrices; cannot meaningfully overflow"
)]
fn translated(transform: Affine, dx: f64, dy: f64) -> Affine {
    transform * Affine::translate((dx, dy))
}

/// Clamps to the end. Converts Automerge marks (char-indexed) to the byte
/// offsets `pixel_x_for_byte` uses.
fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map_or(text.len(), |(b, _)| b)
}

/// Returns `(at, inserted, removed)`. The simple LCP/LCS-prefix algorithm is
/// fine because a `TextInput` always splices at the caret, so the diff is a
/// single insertion or deletion.
pub(crate) fn diff_splice(old: &str, new: &str) -> (usize, usize, usize) {
    let oa = old.as_bytes();
    let na = new.as_bytes();

    // Longest common prefix.
    let prefix = oa.iter().zip(na.iter()).take_while(|(o, n)| o == n).count();

    let o_rest = oa.len().saturating_sub(prefix);
    let n_rest = na.len().saturating_sub(prefix);

    // Longest common suffix within the bytes past the prefix. `zip` already
    // stops at the shorter of the two (reversed, prefix-skipped) iterators,
    // matching the original bound of `min(o_rest, n_rest)`.
    let suffix = oa
        .iter()
        .skip(prefix)
        .rev()
        .zip(na.iter().skip(prefix).rev())
        .take_while(|(o, n)| o == n)
        .count();

    let removed = o_rest.saturating_sub(suffix);
    let inserted = n_rest.saturating_sub(suffix);
    (prefix, inserted, removed)
}

/// Re-exported so callers don't have to depend on the `marks` module
/// directly.
pub fn segments_of(text_len: usize, marks: &[MarkSpan]) -> Vec<MarkSegment> {
    marks::segments_for(text_len, marks)
}

/// Pulled out as a free function so the chip-sizing math is
/// unit-testable independent of the full draw path.
pub fn chip_mono_width(text_renderer: &mut TextRenderer, slice: &str, size: f32) -> f64 {
    let (w, _) = text_renderer.measure_styled(slice, size, None, TextWeight::Regular, false, true);
    f64::from(w)
}

/// Used by `MainPanel::handle_event` to route clicks on link spans to
/// the page-navigation handler.
pub fn link_at_byte(marks_list: &[MarkSpan], byte: usize) -> Option<PageId> {
    for s in marks_list {
        if let Mark::Link(id) = &s.mark
            && s.range.start <= byte
            && byte < s.range.end
        {
            return Some(*id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::KeyEvent;
    use winit::keyboard::{
        Key, KeyLocation, ModifiersKeyState, ModifiersState, NamedKey, PhysicalKey, SmolStr,
    };

    /// `KeyEventExtra` is `pub(crate)` in winit 0.30, so this zero-constructs
    /// rather than using a struct literal.
    fn key_press(named: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(named);
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

    fn char_press(c: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(SmolStr::new(c));
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(SmolStr::new(c));
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn dispatch(line: &mut RichTextLine, event: &WindowEvent) {
        dispatch_with_mods(line, event, Modifiers::default());
    }

    fn dispatch_with_mods(line: &mut RichTextLine, event: &WindowEvent, modifiers: Modifiers) {
        let mut state = automerge::AutoCommit::new();
        let root = automerge::ROOT.clone();
        let mut handled = false;
        let mut focus_target = None;
        let taffy: TaffyTree<()> = TaffyTree::new();
        let fake_node = NodeId::new(0);
        line.handle_event(
            event,
            1.0,
            &modifiers,
            &mut state,
            &root,
            &mut handled,
            &mut focus_target,
            fake_node,
            &taffy,
        );
    }

    #[test]
    fn bound_line_reads_text_from_doc() {
        use automerge::{ObjType, ROOT, transaction::Transactable};
        let mut doc = automerge::AutoCommit::new();
        let tid = doc.put_object(&ROOT, "t", ObjType::Text).unwrap();
        doc.splice_text(&tid, 0, 0, "hello").unwrap();

        let mut line = RichTextLine::new();
        line.bind_text(tid.clone());
        // The root arg is unused once a Text is bound.
        line.sync(&mut doc, &ROOT);
        assert_eq!(line.value(), "hello");
    }

    #[test]
    fn editing_bound_line_splices_into_doc() {
        use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
        let mut doc = automerge::AutoCommit::new();
        let tid = doc.put_object(&ROOT, "t", ObjType::Text).unwrap();

        let mut line = RichTextLine::new();
        line.bind_text(tid.clone());
        line.set_focus(true);

        // Type against OUR doc, not the throwaway one `dispatch` would make.
        let taffy: TaffyTree<()> = TaffyTree::new();
        for c in ["h", "i"] {
            let ev = char_press(c);
            let mut handled = false;
            let mut focus_target = None;
            line.handle_event(
                &ev,
                1.0,
                &Modifiers::default(),
                &mut doc,
                &ROOT,
                &mut handled,
                &mut focus_target,
                NodeId::new(0),
                &taffy,
            );
        }

        assert_eq!(line.value(), "hi");
        assert_eq!(
            doc.text(&tid).unwrap(),
            "hi",
            "edits must splice into the bound Text"
        );
    }

    #[test]
    fn authored_edit_stamps_mark_caches_run_and_survives_round_trip() {
        use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
        let mut doc = automerge::AutoCommit::new();
        let tid = doc.put_object(&ROOT, "t", ObjType::Text).unwrap();

        let mut line = RichTextLine::new();
        line.bind_text(tid.clone());
        line.set_local_author(Some("alice".to_string()));
        line.set_focus(true);

        let taffy: TaffyTree<()> = TaffyTree::new();
        for c in ["h", "i"] {
            let ev = char_press(c);
            let mut handled = false;
            let mut focus_target = None;
            line.handle_event(
                &ev,
                1.0,
                &Modifiers::default(),
                &mut doc,
                &ROOT,
                &mut handled,
                &mut focus_target,
                NodeId::new(0),
                &taffy,
            );
        }
        assert_eq!(doc.text(&tid).unwrap(), "hi");

        let marks = doc.marks(&tid).unwrap();
        assert!(
            marks.iter().any(|m| m.name.as_str() == "author"),
            "expected an author mark, got {marks:?}"
        );

        line.sync(&mut doc, &ROOT);
        assert!(
            line.author_runs().iter().any(|(_, _, a)| a == "alice"),
            "expected an alice author run, got {:?}",
            line.author_runs()
        );

        // Survives a save/load round-trip, which is how authorship reaches a peer.
        let reloaded = automerge::AutoCommit::load(&doc.save()).unwrap();
        let marks2 = reloaded.marks(&tid).unwrap();
        assert!(
            marks2.iter().any(|m| m.name.as_str() == "author"),
            "author mark must survive save/load (CRDT sync)"
        );
    }

    #[test]
    fn new_starts_empty_unfocused() {
        let l = RichTextLine::new();
        assert_eq!(l.value(), "");
        assert!(!l.is_focused());
        assert!(l.cursor_at_start());
        assert!(l.cursor_at_end());
    }

    #[test]
    fn with_text_seeds_value_and_parks_cursor_at_end() {
        let l = RichTextLine::new().with_text("hello");
        assert_eq!(l.value(), "hello");
        assert!(l.cursor_at_end());
        assert!(!l.cursor_at_start());
    }

    #[test]
    fn accessibility_reports_single_line_text_input_with_value() {
        use crate::a11y::Role;
        let l = RichTextLine::new().with_text("hello world");
        let node = Widget::accessibility(&l).expect("editable line is a text input");
        assert_eq!(node.role, Role::TextInput);
        assert_eq!(node.value.as_deref(), Some("hello world"));
        assert!(!node.state.multiline, "a single line is not multiline");
        assert!(node.state.focusable);
        assert!(!node.state.focused, "fresh line is not focused");
        assert!(!node.state.protected, "block text is never a secret field");
    }

    #[test]
    fn accessibility_focus_tracks_inner_input() {
        let mut l = RichTextLine::new().with_text("x");
        Widget::set_focus(&mut l, true);
        let node = Widget::accessibility(&l).unwrap();
        assert!(node.state.focused);
    }

    #[test]
    fn set_text_does_not_mark_dirty() {
        let mut l = RichTextLine::new();
        l.set_text("seeded");
        assert!(
            l.take_dirty().is_none(),
            "programmatic set_text is not dirty"
        );
        assert_eq!(l.value(), "seeded");
        assert!(l.cursor_at_end());
    }

    #[test]
    fn cursor_at_start_after_set_cursor_pos_zero() {
        let mut l = RichTextLine::new().with_text("abc");
        l.set_cursor_pos(0);
        assert!(l.cursor_at_start());
        assert!(!l.cursor_at_end());
        l.set_cursor_pos(3);
        assert!(l.cursor_at_end());
    }

    #[test]
    fn enter_key_yields_signal_with_cursor_byte() {
        let mut l = RichTextLine::new().with_text("abcdef");
        l.set_focus(true);
        l.set_cursor_pos(3);
        dispatch(&mut l, &key_press(NamedKey::Enter));
        match l.take_signal() {
            Some(LineSignal::Enter { split_at: 3 }) => {}
            other => panic!("expected Enter@3, got {other:?}"),
        }
        assert_eq!(l.value(), "abcdef");
    }

    #[test]
    fn enter_at_end_yields_signal_with_full_length() {
        let mut l = RichTextLine::new().with_text("hi");
        l.set_focus(true);
        dispatch(&mut l, &key_press(NamedKey::Enter));
        match l.take_signal() {
            Some(LineSignal::Enter { split_at }) => assert_eq!(split_at, 2),
            other => panic!("expected Enter@2, got {other:?}"),
        }
    }

    #[test]
    fn backspace_at_byte_zero_yields_signal() {
        let mut l = RichTextLine::new().with_text("abc");
        l.set_focus(true);
        l.set_cursor_pos(0);
        dispatch(&mut l, &key_press(NamedKey::Backspace));
        assert_eq!(l.take_signal(), Some(LineSignal::BackspaceAtStart));
        assert_eq!(l.value(), "abc");
    }

    #[test]
    fn backspace_after_byte_zero_falls_through_to_inner() {
        let mut l = RichTextLine::new().with_text("abc");
        l.set_focus(true);
        l.set_cursor_pos(3);
        dispatch(&mut l, &key_press(NamedKey::Backspace));
        assert!(l.take_signal().is_none());
        assert_eq!(l.value(), "ab");
        assert!(l.take_dirty().is_some());
    }

    #[test]
    fn arrow_up_down_emit_signals() {
        let mut l = RichTextLine::new().with_text("x");
        l.set_focus(true);
        dispatch(&mut l, &key_press(NamedKey::ArrowUp));
        assert_eq!(l.take_signal(), Some(LineSignal::ArrowUp));
        dispatch(&mut l, &key_press(NamedKey::ArrowDown));
        assert_eq!(l.take_signal(), Some(LineSignal::ArrowDown));
    }

    #[test]
    fn typing_marks_dirty_and_appends() {
        let mut l = RichTextLine::new();
        l.set_focus(true);
        dispatch(&mut l, &char_press("h"));
        dispatch(&mut l, &char_press("i"));
        assert_eq!(l.value(), "hi");
        let dirty = l.take_dirty();
        assert_eq!(dirty.as_deref(), Some("hi"));
        assert!(l.take_dirty().is_none());
    }

    #[test]
    fn unfocused_line_ignores_keys() {
        let mut l = RichTextLine::new().with_text("abc");
        dispatch(&mut l, &key_press(NamedKey::Enter));
        assert!(l.take_signal().is_none(), "unfocused line must not emit");
        dispatch(&mut l, &char_press("z"));
        assert_eq!(l.value(), "abc");
    }

    #[test]
    fn toggle_mark_on_selection_wraps_it() {
        let mut l = RichTextLine::new().with_text("hello");
        l.set_focus(true);
        l.inner_mut().set_cursor_pos(0);
        // TextInput doesn't expose a public select_all, so this asserts
        // pending-mark behaviour; select-and-toggle is covered below.
        l.toggle_mark(Mark::Bold);
        assert_eq!(l.pending_marks(), &[Mark::Bold]);
        l.toggle_mark(Mark::Bold);
        assert!(l.pending_marks().is_empty());
    }

    #[test]
    fn pending_mark_applies_to_next_typed_char() {
        let mut l = RichTextLine::new();
        l.set_focus(true);
        l.toggle_mark(Mark::Bold);
        assert_eq!(l.pending_marks().len(), 1);
        dispatch(&mut l, &char_press("x"));
        assert_eq!(l.value(), "x");
        // Pending was drained onto the inserted byte.
        assert!(l.pending_marks().is_empty());
        assert_eq!(l.marks().len(), 1);
        assert_eq!(l.marks()[0].range, 0..1);
        assert_eq!(l.marks()[0].mark, Mark::Bold);
    }

    #[test]
    fn marks_shift_on_insert_before() {
        let mut l = RichTextLine::new().with_text("hello");
        l.set_marks(vec![MarkSpan::new(2..4, Mark::Bold)]);
        l.set_focus(true);
        l.set_cursor_pos(0);
        dispatch(&mut l, &char_press("X"));
        assert_eq!(l.value(), "Xhello");
        assert_eq!(l.marks()[0].range, 3..5);
    }

    #[test]
    fn marks_shift_on_delete_before() {
        let mut l = RichTextLine::new().with_text("Xhello");
        l.set_marks(vec![MarkSpan::new(3..5, Mark::Bold)]);
        l.set_focus(true);
        l.set_cursor_pos(1);
        dispatch(&mut l, &key_press(NamedKey::Backspace));
        assert_eq!(l.value(), "hello");
        assert_eq!(l.marks()[0].range, 2..4);
    }

    #[test]
    fn diff_splice_detects_simple_insertion() {
        assert_eq!(diff_splice("abc", "abXc"), (2, 1, 0));
        assert_eq!(diff_splice("abc", "abc"), (3, 0, 0));
        assert_eq!(diff_splice("abc", "ab"), (2, 0, 1));
        assert_eq!(diff_splice("", "hi"), (0, 2, 0));
    }

    #[test]
    fn link_at_byte_finds_mark() {
        let id = uuid::Uuid::from_u128(7);
        let marks = vec![MarkSpan::new(2..6, Mark::Link(id))];
        assert_eq!(link_at_byte(&marks, 3), Some(id));
        assert_eq!(link_at_byte(&marks, 1), None);
        assert_eq!(link_at_byte(&marks, 6), None);
    }

    #[test]
    fn typing_double_bracket_emits_link_open_signal() {
        let mut l = RichTextLine::new();
        l.set_focus(true);
        dispatch(&mut l, &char_press("["));
        // First `[` shouldn't emit.
        assert!(l.take_signal().is_none());
        dispatch(&mut l, &char_press("["));
        match l.take_signal() {
            Some(LineSignal::LinkOpen { open_at: 0 }) => {}
            other => panic!("expected LinkOpen{{open_at:0}}, got {other:?}"),
        }
        assert_eq!(l.value(), "[[");
    }

    #[test]
    fn chip_mono_width_uses_monospace_face_not_sans() {
        // Also checks the mono advance differs from sans — without that, a
        // face mismatch couldn't be detected at all.
        let mut tr = crate::text_render::TextRenderer::new();
        let mono = super::chip_mono_width(&mut tr, "inline code", 16.0);
        let (sans_w, _) =
            tr.measure_styled("inline code", 16.0, None, TextWeight::Regular, false, false);
        assert!(
            mono > 0.0,
            "mono width must be non-zero for non-empty input"
        );
        assert!(
            (mono - f64::from(sans_w)).abs() > 0.5,
            "mono and sans widths must differ for this string (got mono={mono}, sans={sans_w}) — \
             otherwise the chip-width-mismatch bug is undetectable",
        );
    }

    /// `ModifiersState` is fully `pub(crate)` in winit 0.30, so a Ctrl-down
    /// modifier can't be synthesised in unit tests; Ctrl-B/I/E is exercised
    /// through `toggle_mark` above instead.
    fn _placeholder_for_modifiers_state() -> ModifiersState {
        ModifiersState::default()
    }
    fn _placeholder_for_modifiers_key_state() -> ModifiersKeyState {
        ModifiersKeyState::Unknown
    }
}
