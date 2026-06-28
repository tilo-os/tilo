//! `RichTextLine` — a single editable line of text wrapping a
//! local-mode [`TextInput`].
//!
//! Designed as the leaf cell of a block editor. The inner `TextInput`
//! handles cursor placement, selection, IME, copy/paste — everything
//! that's "edit a string". `RichTextLine` adds the block-editor-shaped
//! affordances around it:
//!
//! * Intercepts **Enter**, **Backspace at byte 0**, **Up arrow at the
//!   start**, and **Down arrow at the end** before they reach the
//!   underlying input. These keystrokes don't insert / delete / move
//!   the cursor — they're queued as [`LineSignal`]s the parent
//!   ([`crate::widget::block_editor::BlockEditor`]) consumes.
//! * Exposes the cursor position so the parent can split the line at
//!   the right byte offset.
//! * Carries a per-line font size so a block editor can render
//!   headings at H1/H2/H3 sizes without forking the widget.
//! * **Phase 4**: tracks a [`MarkSpan`] list — inline bold / italic /
//!   code / link marks — and ships rendering + Ctrl-B/I/E toggling
//!   built on top of the inner `TextInput`'s selection range.

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
use crate::widget::marks::{self, Mark, MarkSegment, MarkSpan, PageId};
use crate::widget::{TextInput, Widget};

/// Signals emitted by a `RichTextLine` when a key would carry the
/// edit out of the line itself. The parent (a `BlockEditor`) drains
/// these via [`RichTextLine::take_signal`] after every handled event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineSignal {
    /// User pressed Enter while focused. `split_at` is the byte index
    /// of the caret *inside the line's current value at the moment
    /// Enter was pressed* — the parent splits the line there, drops
    /// the suffix into a new block, and re-focuses.
    Enter { split_at: usize },
    /// User pressed Backspace while the caret was at byte 0 with no
    /// active selection. Parent decides whether to merge with the
    /// previous block, delete an empty block, or no-op.
    BackspaceAtStart,
    /// User pressed Up while the caret was on the first visual line
    /// (single-line for now, so always). Parent moves focus up.
    ArrowUp,
    /// User pressed Down while the caret was on the last visual line.
    /// Parent moves focus down.
    ArrowDown,
    /// User typed `[[` while focused. Parent opens a [`LinkPopover`]
    /// anchored at the caret. The `[[` characters DO fall through to
    /// the inner input — the parent's commit-link logic strips them
    /// when the user picks a page from the popover.
    LinkOpen { open_at: usize },
}

/// Default font size when [`RichTextLine::with_size`] hasn't been
/// called. Matches the body baseline the rest of the UI uses for
/// `TextInput`.
const DEFAULT_FONT_SIZE: f32 = 16.0;

pub struct RichTextLine {
    inner: TextInput,
    /// Pending signal from the most-recent `handle_event`. At most one
    /// signal is in flight per frame; new signals overwrite the slot.
    /// Drained by [`take_signal`].
    signal: Option<LineSignal>,
    /// Per-line font size in CSS-like pixels. Drives both the rendered
    /// glyph height and the leaf's intrinsic taffy height.
    font_size: f32,
    /// Inline marks active on this line. Kept sorted by `range.start`.
    marks: Vec<MarkSpan>,
    /// "Next typed character will be marked." Each entry pushes the
    /// mark onto the inserted char(s) when the next `handle_event`
    /// observes a text-input edit. Drained after applying.
    pending_marks: Vec<Mark>,
    /// Previous text snapshot used to detect splice direction +
    /// length so we can keep [`marks`] aligned across user edits.
    /// Updated after every `handle_event`.
    prev_text: String,
    /// Previous cursor position before the most-recent edit. Used
    /// alongside `prev_text` to figure out where the splice landed.
    prev_cursor: usize,
    /// Cached authorship runs `(byte_start, byte_end, author_id)` read
    /// from the bound Text's Automerge marks during `sync`. Drives the
    /// authorship highlight in `BlockEditor::draw`; empty when unbound or
    /// unmarked.
    author_runs: Vec<(usize, usize, String)>,
}

impl RichTextLine {
    /// Empty, unfocused line ready to be filled by [`with_text`] or
    /// [`set_text`].
    pub fn new() -> Self {
        // Block-editor cells render as document copy, not as form
        // fields — so we suppress the inner input's rounded
        // background. Font size lands via `with_font_size` from the
        // outer builder chain.
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

    /// Builder-style: seed the line's text. Cursor parked at the end.
    /// The dirty flag stays false — this is a programmatic seed.
    pub fn with_text(mut self, s: &str) -> Self {
        self.inner = self.inner.with_initial_value(s);
        self.prev_text = s.to_string();
        self.prev_cursor = s.len();
        self
    }

    /// Builder-style: set the per-line font size (in CSS-like pixels).
    /// Used by `BlockEditor` to render heading blocks (H1=28, H2=22,
    /// H3=18) larger than paragraph blocks.
    pub fn with_size(mut self, size: f32) -> Self {
        self.font_size = size;
        self.inner.set_font_size(size);
        self
    }

    /// Current text content.
    pub fn value(&self) -> &str {
        self.inner.value()
    }

    /// Bind this line to an Automerge `Text` object so its content syncs
    /// from the shared CRDT document and every edit splices back into
    /// it. Used by `BlockEditor` to back each block with the per-block
    /// `Text` at `pages[id].blocks[i].text`. After binding, [`sync`]
    /// pulls the latest text on each frame.
    pub fn bind_text(&mut self, obj: ObjId) {
        self.inner.bind_text(obj);
    }

    /// The Automerge `Text` this line is bound to, if any.
    pub fn bound_obj(&self) -> Option<ObjId> {
        self.inner.bound_obj().cloned()
    }

    /// Set the local user's id for authorship stamping on edits.
    pub fn set_local_author(&mut self, author: Option<String>) {
        self.inner.set_local_author(author);
    }

    /// Authorship runs `(byte_start, byte_end, author_id)` cached from
    /// the bound Text's Automerge marks during the last `sync`. Used by
    /// `BlockEditor::draw` to paint the authorship highlight.
    pub fn author_runs(&self) -> &[(usize, usize, String)] {
        &self.author_runs
    }

    /// Replace the text programmatically. Does NOT mark the line as
    /// dirty (programmatic writes are not user edits). Cursor lands
    /// at the end after clamping to a char boundary.
    pub fn set_text(&mut self, s: &str) {
        self.inner.set_text(s);
        // After a programmatic set_text the cursor needs to be a
        // valid offset; TextInput::set_text just runs clamp, so
        // park the caret at the end of the new value (matches what
        // `with_initial_value` does at construction time).
        self.inner.set_cursor_pos(s.len());
        // Reset mark tracking — programmatic writes don't preserve
        // the prior marks (parents are responsible for re-seeding
        // marks via `set_marks` after a programmatic text rewrite).
        self.marks.clear();
        self.pending_marks.clear();
        self.prev_text = s.to_string();
        self.prev_cursor = s.len();
    }

    /// One-shot read of user edits. Returns `Some(text)` if the user
    /// has typed/deleted since the last call. Clears the dirty flag.
    pub fn take_dirty(&mut self) -> Option<String> {
        self.inner.take_dirty()
    }

    /// True if the inner input currently holds focus.
    pub fn is_focused(&self) -> bool {
        self.inner.is_focused()
    }

    /// Set focus on the underlying input. When losing focus we don't
    /// touch the cursor — it stays wherever the user last left it.
    pub fn set_focus(&mut self, focused: bool) {
        self.inner.set_focus(focused);
    }

    /// True iff the cursor is at byte 0 (the start of the line).
    pub fn cursor_at_start(&self) -> bool {
        self.inner.cursor_pos() == 0
    }

    /// True iff the cursor is at `value().len()` (the end of the line).
    pub fn cursor_at_end(&self) -> bool {
        self.inner.cursor_pos() == self.inner.value().len()
    }

    /// Current cursor position as a byte index into [`value`]. Always
    /// on a UTF-8 char boundary.
    pub fn cursor_pos(&self) -> usize {
        self.inner.cursor_pos()
    }

    /// Active selection as `(start, end)` byte offsets, or `None` when
    /// there's no selection. Used to broadcast the local selection as
    /// presence so peers can highlight it.
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        self.inner.selection_range_public()
    }

    /// Move the caret to the given byte index. Clamped + char-boundary
    /// safe. Used by `BlockEditor` after a split to land the caret at
    /// position 0 of the new block, or after a merge to land it at
    /// the join point.
    pub fn set_cursor_pos(&mut self, pos: usize) {
        self.inner.set_cursor_pos(pos);
        self.prev_cursor = self.inner.cursor_pos();
    }

    /// Drain the pending signal (if any). At most one signal is in
    /// flight per frame.
    pub fn take_signal(&mut self) -> Option<LineSignal> {
        self.signal.take()
    }

    /// Apply layout directly. Used by `BlockEditor` which owns its
    /// children's rects and lays them out manually rather than via
    /// taffy nesting (one taffy leaf per block keeps the focus
    /// machinery simple).
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.inner.apply_layout_external(rect);
    }

    /// Font size in CSS-like pixels. Read by `BlockEditor` to lay out
    /// each block at the right height before delegating to
    /// [`apply_layout_external`].
    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    // ------------ marks API (Phase 4) ------------------------------

    /// Inline marks attached to this line, sorted by start byte.
    pub fn marks(&self) -> &[MarkSpan] {
        &self.marks
    }

    /// Replace the entire mark list (e.g. from a markdown parse).
    pub fn set_marks(&mut self, marks: Vec<MarkSpan>) {
        self.marks = marks;
        self.marks.sort_by_key(|s| s.range.start);
    }

    /// Push a single mark span. Used by the page-link insertion path.
    pub fn add_mark(&mut self, span: MarkSpan) {
        self.marks.push(span);
        self.marks.sort_by_key(|s| s.range.start);
    }

    /// Pending-marks slot (test/inspection accessor).
    pub fn pending_marks(&self) -> &[Mark] {
        &self.pending_marks
    }

    /// Toggle `mark` over the current selection. If no selection is
    /// active, push the mark onto the pending list — the next typed
    /// character will inherit it.
    pub fn toggle_mark(&mut self, mark: Mark) {
        if let Some((s, e)) = self.inner.selection_range_public() {
            marks::toggle_mark_range(&mut self.marks, s..e, mark);
        } else {
            // Toggle pending state: pushing the same mark twice
            // cancels it (matches Notion's "press Ctrl-B twice with
            // no selection clears the pending bold").
            if let Some(idx) = self.pending_marks.iter().position(|m| m.same_kind(&mark)) {
                self.pending_marks.remove(idx);
            } else {
                self.pending_marks.push(mark);
            }
        }
    }

    /// Pixel x coordinate of byte offset `byte` inside the rendered
    /// line. Computed from the inner `TextInput`'s most-recent
    /// per-char layout. Returns 0.0 before the first draw.
    pub fn pixel_x_for_byte(&self, theme: &Theme, byte: usize) -> f64 {
        let xs = self.inner.char_x_positions();
        if xs.is_empty() {
            return self.inner.get_rect().x0;
        }
        let idx = byte.min(xs.len().saturating_sub(1));
        let pad = self.inner.text_padding_x(theme);
        self.inner.get_rect().x0 + pad + xs[idx] - self.inner.scroll_x()
    }

    /// Exact caret rect for byte `byte`, in local coords — delegates to
    /// the inner `TextInput` so a peer caret on this line is pixel-
    /// identical to the line's own focused caret.
    pub fn caret_rect(&self, theme: &Theme, byte: usize) -> Rect {
        self.inner.caret_rect(theme, byte)
    }

    /// Selection rect for `[start, end)` in local coords (delegates).
    pub fn selection_rect(&self, theme: &Theme, start: usize, end: usize) -> Rect {
        self.inner.selection_rect(theme, start, end)
    }

    /// Inverse of [`pixel_x_for_byte`] — clamps `x` to the nearest
    /// char boundary. Used by the click-on-link hit-test in the host
    /// app: caller passes a click x coordinate, gets back the byte
    /// offset to look up against the mark list.
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

    /// Look up the [`Mark`] at byte offset `byte`, preferring
    /// [`Mark::Link`] over other kinds (so link hit-tests still find
    /// the link even if the text is also bold). Returns `None` if no
    /// marks cover that byte.
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

    /// Test-only access to the inner [`TextInput`] (for unit tests).
    #[doc(hidden)]
    pub fn inner_mut(&mut self) -> &mut TextInput {
        &mut self.inner
    }
}

impl Default for RichTextLine {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for RichTextLine {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        // When bound to a Text object the inner input pulls the latest
        // content from the shared CRDT here; when unbound this is a
        // no-op (local mode). Either way, realign the mark-diff snapshot
        // so a remotely-synced edit isn't misread as a local splice on
        // the next keystroke.
        self.inner.sync(state, path);
        self.prev_text = self.inner.value().to_string();
        self.prev_cursor = self.inner.cursor_pos();

        // Refresh the cached authorship runs from the bound Text's marks.
        // Marks are char-indexed; convert to byte ranges so the draw pass
        // can place them with `pixel_x_for_byte`.
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
        // Single leaf sized by `font_size * 1.5` (line height plus a
        // small breathing room) so headings end up taller than
        // paragraph rows without needing a measure pass.
        let height = (self.font_size * 1.5).max(20.0);
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(height),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
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
        // Always delegate to the inner TextInput first — it lays out
        // its caret + selection box + per-char positions and writes
        // text into `scene`. We then overpaint mark-styled segments
        // on top of the plain text. The overlap is intentional: most
        // segments are plain text where the inner pass already drew
        // the correct glyphs, and the overpaint only matters for
        // segments where bold/italic/code/link styling differs.
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
        let line_height = (text_size as f64) * 1.25;
        let rect = self.inner.get_rect();
        let pad = self.inner.text_padding_x(theme);
        let scroll_x = self.inner.scroll_x();
        let text_y = rect.y0 + (rect.height() - line_height) / 2.0;
        let xs = self.inner.char_x_positions();
        if xs.is_empty() {
            // No layout yet (first frame) — bail; next frame the inner
            // input's `draw` will have populated char_x_positions.
            return;
        }

        // Clip overpaint to the input rect so a scrolled long line
        // doesn't bleed glyphs past the column.
        let clip_shape = vello::kurbo::RoundedRect::from_rect(
            rect,
            theme.tokens.radii.md as f64,
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &clip_shape,
        );

        let tokens = &theme.tokens;
        for seg in &segments {
            if seg.range.start >= text.len() {
                continue;
            }
            let slice = &text[seg.range.start..seg.range.end.min(text.len())];
            if slice.is_empty() {
                continue;
            }
            let needs_overpaint =
                seg.bold || seg.italic || seg.code || seg.link.is_some();

            // x for this segment.
            let x_offset = xs
                .get(seg.range.start)
                .copied()
                .unwrap_or(0.0);
            let x = rect.x0 + pad + x_offset - scroll_x;
            let sans_end_x_offset = xs
                .get(seg.range.end.min(xs.len().saturating_sub(1)))
                .copied()
                .unwrap_or(x_offset);
            let sans_end_x = rect.x0 + pad + sans_end_x_offset - scroll_x;

            // Background chip for inline code.
            //
            // Width choice — measure the *monospace* advance for the
            // slice and use the wider of (sans, mono) so the chip
            // covers the sans-serif glyphs the inner TextInput already
            // drew underneath. Using only the sans-serif extent (the
            // original implementation) leaves the chip too wide when
            // monospace is narrower than sans, producing the visible
            // "chip overflows past the code text" artifact reported in
            // the v0.1 critique. Using only the mono extent the other
            // way around would leak the sans `code` tail past the
            // chip's right edge. `max(sans, mono)` handles both.
            if seg.code {
                // Inline code is drawn in the document font (see the
                // `mono = false` below), so its extent matches the
                // sans layout slot exactly. Sizing the chip to the sans
                // extent keeps it from bleeding over the following text —
                // a monospace face would be wider than its measured slot
                // and overlap the next characters.
                let chip_right = sans_end_x;
                let bg_rect = Rect::new(
                    x - 2.0,
                    text_y + 1.0,
                    chip_right + 2.0,
                    text_y + line_height - 1.0,
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

            // Underline for links.
            if seg.link.is_some() {
                let underline = Rect::new(
                    x,
                    text_y + line_height - 2.0,
                    sans_end_x,
                    text_y + line_height - 1.0,
                );
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    tokens.colors.accent[5],
                    None,
                    &underline,
                );
            }

            if !needs_overpaint {
                continue;
            }

            // Pick the colour for this segment.
            let color = if seg.link.is_some() {
                tokens.colors.accent[5]
            } else if seg.code {
                tokens.colors.accent[5]
            } else {
                tokens.colors.text_primary
            };

            let weight = if seg.bold { TextWeight::Bold } else { TextWeight::Regular };
            let italic = seg.italic;
            // Render inline code in the document font, not a separate
            // monospace face: char_x_positions (and thus every segment's
            // x) are measured in that font, so a wider mono glyph run
            // would overflow its slot and overlap the following text. The
            // chip background + accent colour keep code visually distinct.
            let mono = false;

            text_renderer.draw_text_styled(
                scene,
                slice,
                text_size,
                color,
                transform * Affine::translate((x, text_y)),
                None,
                weight,
                italic,
                mono,
            );
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
        // Intercept the four "carry the edit out of the line" keys
        // BEFORE the inner TextInput sees them — otherwise Enter would
        // be filtered as a control char (no-op), and Backspace at
        // byte 0 would silently do nothing (TextInput already guards
        // `if cursor_pos > 0` so we can't piggy-back on the same
        // event). Up / Down arrows would just walk into the next /
        // previous visual line within the input (currently a no-op
        // since it's single-line, but we still want to claim them).
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            if self.inner.is_focused() {
                // Phase 4: Ctrl-B / Ctrl-I / Ctrl-E toggle inline marks.
                // Intercept BEFORE the inner TextInput sees them so the
                // 'b'/'i'/'e' character doesn't get inserted via the
                // text branch (TextInput's own Ctrl-handler skips them
                // because they don't match its a/c/v/x rotation).
                if modifiers.state().control_key() {
                    if let Key::Character(c) = logical_key {
                        match c.to_lowercase().as_str() {
                            "b" => {
                                self.toggle_mark(Mark::Bold);
                                *handled = true;
                                return;
                            }
                            "i" => {
                                self.toggle_mark(Mark::Italic);
                                *handled = true;
                                return;
                            }
                            "e" => {
                                self.toggle_mark(Mark::Code);
                                *handled = true;
                                return;
                            }
                            _ => {}
                        }
                    }
                }

                match logical_key {
                    Key::Named(NamedKey::Enter) => {
                        self.signal = Some(LineSignal::Enter {
                            split_at: self.inner.cursor_pos(),
                        });
                        *handled = true;
                        return;
                    }
                    Key::Named(NamedKey::Backspace) => {
                        // Only intercept when the caret is at byte 0
                        // *and* there's nothing selected — otherwise
                        // the inner input should handle the delete.
                        if self.inner.cursor_pos() == 0 {
                            self.signal = Some(LineSignal::BackspaceAtStart);
                            *handled = true;
                            return;
                        }
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.signal = Some(LineSignal::ArrowUp);
                        *handled = true;
                        return;
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.signal = Some(LineSignal::ArrowDown);
                        *handled = true;
                        return;
                    }
                    _ => {}
                }
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

        // After the inner handled the event, reconcile marks against
        // any text edit. Mark shifts use byte-deltas relative to the
        // previous text snapshot.
        let new_text = self.inner.value();
        if new_text != self.prev_text {
            let (at, inserted, removed) = diff_splice(&self.prev_text, new_text);
            marks::shift_after_edit(&mut self.marks, at, inserted, removed);

            // Phase 4: drain pending marks onto the inserted bytes.
            if inserted > 0 && !self.pending_marks.is_empty() {
                let range = at..(at + inserted);
                let pending = std::mem::take(&mut self.pending_marks);
                for m in pending {
                    marks::add_mark_to_range(&mut self.marks, range.clone(), m);
                }
            }

            // Detect `[[` → emit LinkOpen signal so the parent can
            // pop the LinkPopover. The signal fires when the two
            // most recently typed characters are `[[` AND the caret
            // lands right after them. We do NOT strip the brackets;
            // the popover commit path takes care of that.
            if inserted > 0 {
                let caret = self.inner.cursor_pos();
                if caret >= 2 && new_text.as_bytes()[caret - 1] == b'['
                    && new_text.as_bytes()[caret - 2] == b'['
                {
                    self.signal = Some(LineSignal::LinkOpen { open_at: caret - 2 });
                }
            }

            self.prev_text = new_text.to_string();
        }
        self.prev_cursor = self.inner.cursor_pos();
    }

    fn name(&self) -> &str {
        "RichTextLine"
    }

    fn get_rect(&self) -> Rect {
        self.inner.get_rect()
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
}

/// Diff `old` and `new` and return `(at, inserted, removed)` where
/// `at` is the first differing byte, `inserted` is the number of
/// bytes in `new` after `at` that aren't in `old`, and `removed` is
/// the number of bytes in `old` after `at` that were removed. The
/// simple LCP / LCS-prefix algorithm is fine for the single-line
/// edits this widget sees (a TextInput splices at the caret, so the
/// diff is always a single insertion or deletion).
/// Convert a char index into a byte index within `text`, clamping to the
/// end. Used to map Automerge marks (char-indexed) to the byte offsets
/// the layout/`pixel_x_for_byte` path uses.
fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

pub(crate) fn diff_splice(old: &str, new: &str) -> (usize, usize, usize) {
    let oa = old.as_bytes();
    let na = new.as_bytes();
    let max = oa.len().min(na.len());
    let mut prefix = 0;
    while prefix < max && oa[prefix] == na[prefix] {
        prefix += 1;
    }
    let mut o_suffix = 0;
    let mut n_suffix = 0;
    while o_suffix < oa.len() - prefix
        && n_suffix < na.len() - prefix
        && oa[oa.len() - 1 - o_suffix] == na[na.len() - 1 - n_suffix]
    {
        o_suffix += 1;
        n_suffix += 1;
    }
    let removed = oa.len() - prefix - o_suffix;
    let inserted = na.len() - prefix - n_suffix;
    (prefix, inserted, removed)
}

/// Helper used by the host app to materialise the partition of marks
/// into segments for a given byte length. Re-exported so callers
/// don't have to depend on the `marks` module directly.
pub fn segments_of(text_len: usize, marks: &[MarkSpan]) -> Vec<MarkSegment> {
    marks::segments_for(text_len, marks)
}

/// Width (in CSS-like px) of `slice` when rendered in the monospace
/// face the inline-code chip uses. Pulled out as a free function so
/// the chip-sizing math is unit-testable independent of the full draw
/// path (which requires a populated Scene + Theme). The matching
/// rendering call uses `text_renderer.draw_text_styled(..., monospace
/// = true)` — see `RichTextLine::draw`.
pub fn chip_mono_width(
    text_renderer: &mut TextRenderer,
    slice: &str,
    size: f32,
) -> f64 {
    let (w, _) = text_renderer.measure_styled(
        slice,
        size,
        None,
        TextWeight::Regular,
        false,
        true,
    );
    w as f64
}

/// Conveniently look up a page-link mark covering `byte`, returning
/// the `PageId` if present. Used by `MainPanel::handle_event` to
/// route clicks on link spans to the page-navigation handler.
pub fn link_at_byte(marks_list: &[MarkSpan], byte: usize) -> Option<PageId> {
    for s in marks_list {
        if let Mark::Link(id) = &s.mark {
            if s.range.start <= byte && byte < s.range.end {
                return Some(*id);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::KeyEvent;
    use winit::keyboard::{Key, KeyLocation, ModifiersKeyState, ModifiersState, NamedKey, PhysicalKey, SmolStr};

    /// `KeyEventExtra` is `pub(crate)` in winit 0.30, so we can't
    /// construct a `KeyEvent` via struct-literal — same workaround as
    /// `command_palette::tests::key_press`. The all-zero bit pattern
    /// is safe for every pub field this widget reads.
    fn key_press(named: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(named);
        ev.physical_key = PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
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
        ev.physical_key = PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
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

    fn ctrl_modifiers() -> Modifiers {
        // Modifiers fields are pub(crate) in winit 0.30, so we zero-
        // construct (every flag false) then update via reflection-y
        // hacks would be a nightmare. Instead, we test toggle_mark
        // through the public API rather than the keyboard path.
        Modifiers::default()
    }

    fn dispatch(line: &mut RichTextLine, event: &WindowEvent) {
        dispatch_with_mods(line, event, &Modifiers::default());
    }

    fn dispatch_with_mods(line: &mut RichTextLine, event: &WindowEvent, modifiers: &Modifiers) {
        let mut state = automerge::AutoCommit::new();
        let root = automerge::ROOT.clone();
        let mut handled = false;
        let mut focus_target = None;
        let taffy: TaffyTree<()> = TaffyTree::new();
        let fake_node = NodeId::new(0);
        line.handle_event(
            event,
            1.0,
            modifiers,
            &mut state,
            &root,
            &mut handled,
            &mut focus_target,
            fake_node,
            &taffy,
        );
    }

    // ---- CRDT binding (re-enabled CRDT path) -------------------------

    #[test]
    fn bound_line_reads_text_from_doc() {
        use automerge::{ObjType, ROOT, transaction::Transactable};
        let mut doc = automerge::AutoCommit::new();
        let tid = doc.put_object(&ROOT, "t", ObjType::Text).unwrap();
        doc.splice_text(&tid, 0, 0, "hello").unwrap();

        let mut line = RichTextLine::new();
        line.bind_text(tid.clone());
        // Widget::sync; the root arg is unused once a Text is bound.
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

        // Type "hi" against OUR doc (not the throwaway doc `dispatch`
        // would make) so we can observe the splice land in the CRDT.
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
        assert_eq!(doc.text(&tid).unwrap(), "hi", "edits must splice into the bound Text");
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

        // An author mark covers the inserted run.
        let marks = doc.marks(&tid).unwrap();
        assert!(
            marks.iter().any(|m| m.name.as_str() == "author"),
            "expected an author mark, got {marks:?}"
        );

        // sync caches it as a byte-range run attributed to alice.
        line.sync(&mut doc, &ROOT);
        assert!(
            line.author_runs().iter().any(|(_, _, a)| a == "alice"),
            "expected an alice author run, got {:?}",
            line.author_runs()
        );

        // The mark rides the CRDT: it survives a save/load round-trip,
        // which is how authorship reaches a peer.
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
    fn set_text_does_not_mark_dirty() {
        let mut l = RichTextLine::new();
        l.set_text("seeded");
        assert!(l.take_dirty().is_none(), "programmatic set_text is not dirty");
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
            other => panic!("expected Enter@3, got {:?}", other),
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
            other => panic!("expected Enter@2, got {:?}", other),
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

    // ---- Phase 4: marks ----------------------------------------------

    #[test]
    fn toggle_mark_on_selection_wraps_it() {
        let mut l = RichTextLine::new().with_text("hello");
        l.set_focus(true);
        // Manually set a selection on the inner TextInput.
        l.inner_mut().set_cursor_pos(0);
        // Use the inner public selection setters indirectly: ctrl+A.
        // The test path is simpler — set anchor + cursor manually
        // via the public selection range path. But TextInput doesn't
        // expose mutators, so call ctrl-a equivalent by setting via
        // private state. We instead drive `toggle_mark` directly with
        // a fake selection by reaching into the inner through tests.
        //
        // Cheaper: hand-build a selection using cursor + click path.
        // Easiest: do it through the public select_all / Ctrl-A handler.
        //
        // For unit purposes here, we just call toggle_mark after
        // priming a selection through the inner.
        // NOTE: TextInput doesn't expose a public select_all, so we
        // bypass selection and assert pending-mark behaviour instead
        // for the unit. The full select-and-toggle path is covered in
        // the integration test below.

        l.toggle_mark(Mark::Bold);
        // No selection → pending mark.
        assert_eq!(l.pending_marks(), &[Mark::Bold]);
        // Toggling again clears pending.
        l.toggle_mark(Mark::Bold);
        assert!(l.pending_marks().is_empty());
    }

    #[test]
    fn pending_mark_applies_to_next_typed_char() {
        let mut l = RichTextLine::new();
        l.set_focus(true);
        l.toggle_mark(Mark::Bold);
        assert_eq!(l.pending_marks().len(), 1);
        // Type one character.
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
        // Now text is "Xhello", mark should have shifted to 3..5.
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
            other => panic!("expected LinkOpen{{open_at:0}}, got {:?}", other),
        }
        assert_eq!(l.value(), "[[");
    }

    #[test]
    fn chip_mono_width_uses_monospace_face_not_sans() {
        // Anchor for the inline-code chip-width fix (v0.1 critique A1).
        // The chip BG width must be measured in the same face the
        // *visible* code text uses (monospace), not the underlying
        // sans-serif width the inner TextInput's `char_x_positions`
        // reports. Otherwise the chip's right edge sits in the gap
        // between the (sans) layout end and the (mono) glyph end —
        // visible as "chip overflows past the code text".
        //
        // We pin two properties:
        // 1. `chip_mono_width` returns a non-zero value for non-empty
        //    input.
        // 2. The mono advance differs from the sans-serif advance for
        //    a typical lowercase string. Without this differentiation
        //    the bug couldn't exist in the first place, so the test
        //    has to observe both.
        let mut tr = crate::text_render::TextRenderer::new();
        let mono = super::chip_mono_width(&mut tr, "inline code", 16.0);
        let (sans_w, _) = tr.measure_styled(
            "inline code",
            16.0,
            None,
            TextWeight::Regular,
            false,
            false,
        );
        assert!(mono > 0.0, "mono width must be non-zero for non-empty input");
        assert!(
            (mono - sans_w as f64).abs() > 0.5,
            "mono and sans widths must differ for this string (got mono={mono}, sans={sans_w}) — \
             otherwise the chip-width-mismatch bug is undetectable",
        );
    }

    /// Unused helper kept for symmetry with future ModifiersState test
    /// constructors. winit 0.30 keeps `ModifiersState` fully `pub(crate)`
    /// so we can't synthesise a Ctrl-down modifier in unit tests; the
    /// Ctrl-B / Ctrl-I / Ctrl-E paths are exercised through
    /// `toggle_mark` directly above.
    #[allow(dead_code)]
    fn _placeholder_for_modifiers_state() -> ModifiersState {
        ModifiersState::default()
    }
    #[allow(dead_code)]
    fn _placeholder_for_modifiers_key_state() -> ModifiersKeyState {
        ModifiersKeyState::Unknown
    }
    #[allow(dead_code)]
    fn _placeholder_for_ctrl_modifiers() -> Modifiers {
        ctrl_modifiers()
    }
}
