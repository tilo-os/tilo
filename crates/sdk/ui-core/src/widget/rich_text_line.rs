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

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use tilo_sort::CompactSort as _;

use crate::color_compat::ColorCompat;
use crate::richtext::{RichSpan, inline_emoji};
use crate::widget::text_input::SingleLineAttributes;
use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::code_editor::TokenKind;
use crate::widget::markdown_input;
use crate::widget::marks::{self, Mark, MarkSegment, MarkSpan, PageId};
use crate::widget::{TextInput, Widget};

/// A presentation-only syntax colour over a byte range of the line, painted
/// from the theme's [`SyntaxPalette`](crate::tokens::SyntaxPalette) and never
/// written to the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransientColorSpan {
    pub range: std::ops::Range<usize>,
    pub kind: TokenKind,
}

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
    /// Mirror of `SelectBlockUp` extending down.
    SelectBlockDown {
        caret: usize,
    },
    /// User typed `[[`; parent opens a [`LinkPopover`](crate::widget::link_popover::LinkPopover) anchored at
    /// `open_at`. Brackets fall through to the inner input — the popover's
    /// commit-link logic strips them.
    LinkOpen {
        open_at: usize,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ImagePreview {
    #[default]
    Absent,
    Present {
        dimensions: Option<(u32, u32)>,
        editing: bool,
        /// The tallest the preview may be, in logical pixels: a share of the
        /// host's visible height, so a tall picture never fills the editor.
        max_height: Option<u32>,
        /// The width the person sized the picture to, in logical pixels at
        /// 1× text; `None` fills the column.
        width: Option<u32>,
    },
}

impl ImagePreview {
    fn is_present(self) -> bool {
        matches!(self, Self::Present { .. })
    }
}

/// The inputs a stacking host measured a line under, and the result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct StackMeasure {
    pub(super) width: f32,
    pub(super) height: f32,
}

fn default_measurement_theme() -> Arc<Theme> {
    static DEFAULT: LazyLock<Arc<Theme>> = LazyLock::new(|| Arc::new(Theme::dark()));
    Arc::clone(&DEFAULT)
}

#[derive(Default)]
struct RichTextDirty {
    marks: bool,
    styled_layout: bool,
}

pub struct RichTextLine {
    inner: TextInput,
    /// At most one signal in flight per frame; new signals overwrite the slot.
    signal: Option<LineSignal>,
    /// The line's 1× type size: its role's, or a document editor's own
    /// (a heading block under the page zoom).
    font_size: f32,
    wrap: bool,
    inline_formatting: bool,
    /// Shared by every line of one editor: the editor hands out one `Arc`
    /// per theme, so preparing an unchanged theme is a pointer comparison.
    measurement_theme: Arc<Theme>,
    /// The height a stacking host last measured for this line, valid while
    /// the line reports no pending measurement and its layout inputs match.
    stack_measure: Option<StackMeasure>,
    layout_inset: f32,
    layout_header_height: f32,
    table_preview: Option<bool>,
    table_source_height: f32,
    /// An image attachment uses the leaf's actual Taffy width for its preview.
    image_preview: ImagePreview,
    image_source_height: f32,
    /// Kept sorted by `range.start`.
    marks: Vec<MarkSpan>,
    /// Paint-only semantic colours (for example fenced-code syntax). These
    /// never enter Automerge or the Markdown mark model.
    transient_colors: Vec<TransientColorSpan>,
    /// What the owner derived `transient_colors` from, if it said: an owner
    /// that derives them (a code run's syntax) compares this rather than
    /// deriving them again. Any other change of the colours forgets it.
    transient_key: Option<u64>,
    /// Formatting-only edits must notify the owning document just like typing.
    dirty: RichTextDirty,
    /// Cached theme colours used by unwrapped attributed single-line shaping.
    accent_rgb: Option<[u8; 3]>,
    /// The theme's syntax palette, captured when attributes are prepared.
    syntax: Option<crate::tokens::SyntaxPalette>,
    /// Drained once `handle_event` observes a text-input edit.
    pending_marks: Vec<Mark>,
    /// Snapshot used to detect splice direction + length so `marks` stays
    /// aligned across user edits.
    prev_text: String,
    /// Cursor position before the most-recent edit; used with `prev_text` to
    /// figure out where the splice landed.
    prev_cursor: usize,
    /// Marks that end at this byte for good: text typed there next does not
    /// continue them (a span closed by its typed markdown delimiter).
    closed: Option<(usize, Vec<Mark>)>,
    /// `(byte_start, byte_end, author_id)` read from the bound Text's marks
    /// during `sync`; drives the authorship highlight in `BlockEditor::draw`.
    author_runs: Vec<(usize, usize, String)>,
    /// The colours mentions are painted in; accent ink when unset.
    mention_paint: Option<marks::MentionPaint>,
    /// The revision of the window's workspace emoji the line's runs were
    /// last built against: when it moves, a `:name:` may have become a
    /// picture or stopped being one.
    emoji_revision: Option<u64>,
}

impl RichTextLine {
    pub fn new() -> Self {
        // Block-editor cells render as document copy, not a form field.
        let mut inner = TextInput::local().with_plain_chrome();
        inner.keep_document_inset();
        // The block editor writes an autocorrection after the keystroke
        // that typed its word boundary, as an undo step of its own.
        inner.set_deferred_autocorrection(true);
        Self {
            inner,
            signal: None,
            font_size: crate::tokens::typography::stock().size(TextRole::Body),
            wrap: false,
            inline_formatting: true,
            measurement_theme: default_measurement_theme(),
            stack_measure: None,
            layout_inset: 0.0,
            layout_header_height: 0.0,
            table_preview: None,
            table_source_height: 0.0,
            image_preview: ImagePreview::Absent,
            image_source_height: 0.0,
            marks: Vec::new(),
            transient_colors: Vec::new(),
            transient_key: None,
            dirty: RichTextDirty::default(),
            accent_rgb: None,
            syntax: None,
            pending_marks: Vec::new(),
            prev_text: String::new(),
            prev_cursor: 0,
            closed: None,
            author_runs: Vec::new(),
            mention_paint: None,
            emoji_revision: None,
        }
    }

    /// Paint mentions in the colours `paint` gives each participant.
    pub fn set_mention_paint(&mut self, paint: Option<marks::MentionPaint>) {
        self.mention_paint = paint;
        self.refresh_attributes();
    }

    /// The ink and band of a mention of `id`.
    fn mention_rgb(&self, id: &str) -> (Option<[u8; 3]>, Option<[u8; 3]>) {
        let rgb = |color: vello::peniko::Color| [color.r(), color.g(), color.b()];
        match &self.mention_paint {
            Some(paint) => {
                let colors = paint(id);
                (Some(rgb(colors.ink)), colors.band.map(rgb))
            }
            None => (self.accent_rgb, None),
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

    /// Set the line in `role`: `Body` for interface copy, `Reading` for
    /// long-form text. The default is `Body`.
    #[must_use]
    pub fn with_text_role(mut self, role: TextRole) -> Self {
        self.set_text_role(role);
        self
    }

    /// Change the line's role; its height is measured again.
    pub fn set_text_role(&mut self, role: TextRole) {
        self.inner.set_text_role(role);
        self.set_size(crate::tokens::typography::stock().size(role));
    }

    /// A document editor's own 1× size for this line (a heading block, the
    /// page zoom).
    #[must_use]
    pub(crate) fn with_size(mut self, size: f32) -> Self {
        self.set_size(size);
        self
    }

    #[must_use]
    pub fn with_mono(mut self) -> Self {
        self.inner.set_mono(true);
        self
    }

    pub fn set_mono(&mut self, mono: bool) {
        if self.inner.is_mono() != mono {
            self.stack_measure = None;
        }
        self.inner.set_mono(mono);
    }

    /// Source-code cells retain literal text rather than prose formatting or
    /// note-link shortcuts. Monospace alone does not imply these semantics.
    pub fn set_inline_formatting(&mut self, enabled: bool) {
        if self.inline_formatting != enabled {
            self.dirty.styled_layout = true;
            self.stack_measure = None;
        }
        self.inline_formatting = enabled;
        self.inner.set_compact_multiline(!enabled);
        if !enabled {
            self.pending_marks.clear();
        }
    }

    /// Whether the line shows inline marks (prose) rather than plain
    /// source (code).
    pub fn inline_formatting(&self) -> bool {
        self.inline_formatting
    }

    /// Whether the caret types into inline code: inside a code span, at its
    /// end (where the document extends the span over what is typed), or
    /// with code set for the text typed next.
    pub fn caret_in_code(&self) -> bool {
        let caret = self.inner.cursor_pos();
        self.inline_formatting
            && (self
                .pending_marks
                .iter()
                .any(|mark| matches!(mark, Mark::Code))
                || self.marks.iter().any(|span| {
                    matches!(span.mark, Mark::Code)
                        && span.range.start < caret
                        && caret <= span.range.end
                }))
    }

    /// Whether `event` types the backtick that closes inline code.
    fn closes_inline_code(&self, event: &WindowEvent) -> bool {
        let typed = match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        text: Some(text),
                        ..
                    },
                ..
            } => text.as_str(),
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => text.as_str(),
            _ => return false,
        };
        if !self.inline_formatting || !typed.ends_with('`') {
            return false;
        }
        let text = self.inner.value();
        text.get(..self.inner.cursor_pos().min(text.len()))
            .and_then(|head| markdown_input::detect_inline(&format!("{head}{typed}")))
            .is_some_and(|found| found.kind == markdown_input::InlineKind::Code)
    }

    pub fn set_wrap(&mut self, wrap: bool) {
        if self.wrap != wrap {
            self.stack_measure = None;
        }
        self.wrap = wrap;
        self.inner.set_multiline(wrap);
    }

    pub(super) fn set_compact_wrapped(&mut self, compact: bool) {
        self.inner.set_compact_multiline(compact);
    }

    pub(super) fn set_layout_inset(&mut self, inset: f32) {
        if self.layout_inset.to_bits() != inset.to_bits() {
            self.stack_measure = None;
        }
        self.layout_inset = inset;
    }

    pub(super) fn set_layout_header_height(&mut self, height: f32) {
        if self.layout_header_height.to_bits() != height.to_bits() {
            self.stack_measure = None;
        }
        self.layout_header_height = height;
    }

    pub(super) fn set_table_preview(&mut self, visual: Option<bool>) {
        if self.table_preview != visual {
            self.stack_measure = None;
        }
        self.table_preview = visual;
    }

    pub(super) fn table_source_height(&self) -> f32 {
        self.table_source_height
    }

    pub(super) fn set_image_preview(&mut self, preview: ImagePreview) {
        if self.image_preview != preview {
            self.stack_measure = None;
        }
        self.image_preview = preview;
    }

    pub(super) fn image_source_height(&self) -> f32 {
        self.image_source_height
    }

    pub fn value(&self) -> &str {
        self.inner.value()
    }

    /// Used by `BlockEditor` to back each block with the per-block `Text` at
    /// `pages[id].blocks[i].text`.
    pub fn bind_text(&mut self, obj: ObjId) {
        self.inner.bind_text(obj);
    }

    pub(super) fn adopt_bound_text(
        &mut self,
        state: &AutoCommit,
    ) -> Result<(), crate::view::ThawError> {
        self.inner.adopt_bound_text(state)?;
        self.align_bound_projection(state);
        self.refresh_attributes();
        Ok(())
    }

    fn align_bound_projection(&mut self, state: &AutoCommit) {
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

    pub fn bound_obj(&self) -> Option<ObjId> {
        self.inner.bound_obj().cloned()
    }

    pub fn set_local_author(&mut self, author: Option<String>) {
        self.inner.set_local_author(author);
    }

    /// Align the line's text within its width.
    pub fn set_align(&mut self, align: crate::richtext::TextAlign) {
        self.inner.set_align(align);
    }

    pub fn align(&self) -> crate::richtext::TextAlign {
        self.inner.align()
    }

    pub fn author_runs(&self) -> &[(usize, usize, String)] {
        &self.author_runs
    }

    /// Show who wrote which run of an unbound line: byte ranges and the
    /// writer's id, as the model owning the text records them.
    pub fn set_author_runs(&mut self, runs: Vec<(usize, usize, String)>) {
        self.author_runs = runs;
    }

    /// Does NOT mark the line dirty — programmatic writes are not user edits.
    pub fn set_text(&mut self, s: &str) {
        self.closed = None;
        self.inner.set_text(s);
        self.inner.set_cursor_pos(s.len());
        // Doesn't preserve prior marks; parents re-seed via `set_marks`.
        self.dirty.styled_layout |= !self.marks.is_empty();
        self.marks.clear();
        self.dirty.marks = false;
        self.pending_marks.clear();
        self.prev_text = s.to_string();
        self.prev_cursor = s.len();
        self.refresh_attributes();
    }

    /// A user edit to text or rich spans. Programmatic projection stays clean.
    pub fn take_dirty(&mut self) -> Option<String> {
        let text = self.inner.take_dirty();
        if std::mem::take(&mut self.dirty.marks) {
            Some(text.unwrap_or_else(|| self.inner.value().to_owned()))
        } else {
            text
        }
    }

    pub fn is_focused(&self) -> bool {
        self.inner.is_focused()
    }

    /// Whether the underlying input currently refuses editing.
    pub fn is_disabled(&self) -> bool {
        self.inner.is_disabled()
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

    /// A document editor's own 1× size; its height is measured again.
    pub(crate) fn set_size(&mut self, size: f32) {
        if self.font_size.to_bits() != size.to_bits() {
            self.stack_measure = None;
            self.font_size = size;
            self.inner.set_base_size(size);
        }
    }

    pub fn marks(&self) -> &[MarkSpan] {
        &self.marks
    }

    /// What the line's accessibility node is built from besides its text:
    /// [`TextInput::accessibility_state`].
    pub(crate) fn accessibility_state(&self) -> crate::widget::text_input::TextAccessibilityState {
        self.inner.accessibility_state()
    }

    pub fn set_marks(&mut self, mut marks: Vec<MarkSpan>) {
        marks.compact_sort_by_key(|s| s.range.start);
        self.dirty.styled_layout |= self.marks != marks;
        self.marks = marks;
    }

    pub fn set_transient_colors(&mut self, colors: Vec<TransientColorSpan>) {
        self.transient_key = None;
        self.replace_transient_colors(colors);
    }

    /// [`Self::set_transient_colors`], remembering `key` as what they were
    /// derived from; [`Self::transient_colors_key`] returns it until the
    /// colours are set again.
    pub fn set_keyed_transient_colors(&mut self, key: u64, colors: Vec<TransientColorSpan>) {
        self.transient_key = Some(key);
        self.replace_transient_colors(colors);
    }

    /// The key the current transient colours were set with, if any.
    pub fn transient_colors_key(&self) -> Option<u64> {
        self.transient_key
    }

    fn replace_transient_colors(&mut self, colors: Vec<TransientColorSpan>) {
        if self.transient_colors == colors {
            return;
        }
        self.dirty.styled_layout = true;
        self.transient_colors = colors;
        self.refresh_attributes();
    }

    #[doc(hidden)]
    pub fn transient_colors(&self) -> &[TransientColorSpan] {
        &self.transient_colors
    }

    /// Whether the last typed word boundary left a correction to apply.
    pub fn has_pending_autocorrection(&self) -> bool {
        self.inner.has_pending_autocorrection()
    }

    /// Make the correction the last typed word boundary left pending (see
    /// [`TextInput::apply_pending_autocorrection`]), carrying the marks over
    /// it as typing does.
    pub fn apply_pending_autocorrection(&mut self) -> bool {
        if !self.inner.apply_pending_autocorrection() {
            return false;
        }
        let new_text = self.inner.value().to_string();
        if new_text != self.prev_text {
            self.reconcile_marks_after_edit(&new_text);
            self.prev_text = new_text;
            self.refresh_attributes();
        }
        self.prev_cursor = self.inner.cursor_pos();
        true
    }

    /// Text typed next at byte `at` does not continue `marks`, though a
    /// span of each ends there: the person ended them on purpose. Moving
    /// the caret or editing elsewhere forgets this.
    pub fn close_marks_at(&mut self, at: usize, marks: Vec<Mark>) {
        self.closed = Some((at, marks));
    }

    pub fn add_mark(&mut self, span: MarkSpan) {
        self.marks.push(span);
        self.marks.compact_sort_by_key(|s| s.range.start);
        self.dirty.styled_layout = true;
    }

    pub fn pending_marks(&self) -> &[Mark] {
        &self.pending_marks
    }

    /// With no active selection, pushes the mark onto the pending list so
    /// the next typed character inherits it.
    pub fn toggle_mark(&mut self, mark: Mark) {
        if !self.inline_formatting {
            return;
        }
        if let Some((s, e)) = self.inner.selection_range_public() {
            marks::toggle_mark_range(&mut self.marks, s..e, mark);
            self.dirty.marks = true;
            self.dirty.styled_layout = true;
        } else {
            // Pushing the same mark twice cancels it (Notion-style).
            if let Some(idx) = self.pending_marks.iter().position(|m| m.same_kind(&mark)) {
                self.pending_marks.remove(idx);
            } else {
                self.pending_marks.push(mark);
            }
        }
    }

    /// How far the line's text starts inside its rect: the text field's
    /// own horizontal inset (none for the editor's plain lines).
    pub fn text_inset(&self, theme: &Theme) -> f64 {
        self.inner.text_inset_x(theme)
    }

    /// The first visual row's band, `(top, height)`, where a block's
    /// prefix (a bullet, an ordinal, a checkbox) sits.
    pub(super) fn first_row_band(&self) -> (f64, f64) {
        self.inner.first_row_band()
    }

    pub(super) fn line_style(&self, text_scale: f32) -> crate::text_render::TextStyle {
        self.inner.line_style(text_scale)
    }

    /// Returns 0.0 before the first draw.
    pub fn pixel_x_for_byte(&self, theme: &Theme, byte: usize) -> f64 {
        let xs = self.inner.char_x_positions();
        if xs.is_empty() {
            return self.inner.get_rect().x0;
        }
        let idx = byte.min(xs.len().saturating_sub(1));
        // The same inset the line's text is laid out at, multiline or not.
        let pad = self.text_inset(theme);
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

    /// Inverse of `pixel_x_for_byte`, using the shaped caret stops.
    pub fn byte_for_pixel_x(&self, _theme: &Theme, x: f64) -> usize {
        self.inner
            .byte_at_point(Point::new(x, self.inner.get_rect().center().y))
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

    /// What the last draw painted outside the line's rect
    /// ([`TextInput::overhang`]).
    #[must_use]
    pub fn overhang(&self) -> Option<Rect> {
        self.inner.overhang()
    }

    fn refresh_attributes(&mut self) {
        let text = self.inner.value();
        // Ignore malformed mark endpoints while still retaining every UTF-8
        // byte in the complete run list.
        let marks: Vec<_> = self
            .marks
            .iter()
            .filter(|mark| {
                mark.range.start < mark.range.end
                    && mark.range.end <= text.len()
                    && text.is_char_boundary(mark.range.start)
                    && text.is_char_boundary(mark.range.end)
            })
            .cloned()
            .collect();
        let mut attributes = SingleLineAttributes::default();
        for segment in marks::segments_for(text.len(), &marks) {
            let range = (segment.range.start, segment.range.end);
            // Split the complete shaped run at paint-only syntax boundaries.
            // Styling and hit geometry continue to use the same TextInput layout.
            let mut boundaries = vec![segment.range.start, segment.range.end];
            for color in &self.transient_colors {
                for boundary in [color.range.start, color.range.end] {
                    if segment.range.contains(&boundary) && text.is_char_boundary(boundary) {
                        boundaries.push(boundary);
                    }
                }
            }
            boundaries.compact_sort();
            boundaries.dedup();
            for endpoints in boundaries.windows(2) {
                let &[start, end] = endpoints else { continue };
                let Some(slice) = text.get(start..end) else {
                    continue;
                };
                let syntax = self
                    .transient_colors
                    .iter()
                    .rev()
                    .find(|color| color.range.start <= start && color.range.end >= end)
                    .and_then(|color| {
                        let rgb = self.syntax.as_ref()?.color(color.kind);
                        Some([rgb.r(), rgb.g(), rgb.b()])
                    });
                let (mention_ink, mention_band) = segment
                    .mention
                    .as_deref()
                    .map_or((None, None), |id| self.mention_rgb(id));
                let color = if mention_ink.is_some() {
                    mention_ink
                } else if segment.code || segment.link.is_some() || segment.external_link {
                    self.accent_rgb
                } else {
                    syntax
                };
                let mut span = RichSpan::new(slice)
                    .bold(segment.bold || segment.mention.is_some())
                    .italic(segment.italic)
                    .color(color)
                    .background(mention_band);
                span.kind = mark_kind(&segment);
                // Prose draws its workspace emoji; code keeps `:name:`.
                if segment.code || !self.inline_formatting || !slice.contains(':') {
                    attributes.spans.push(span);
                } else {
                    attributes
                        .spans
                        .extend(inline_emoji::split_spans_in_window(vec![span]));
                }
            }
            if segment.underline {
                attributes.underlines.push(range);
            }
            if segment.strikethrough {
                attributes.strikes.push(range);
            }
            if segment.code {
                attributes.chips.push(range);
            }
            if (segment.link.is_some() || segment.external_link) && !segment.underline {
                attributes.links.push(range);
            }
        }
        self.inner.set_single_line_attributes(attributes);
    }

    fn prepare_attributes(&mut self, theme: &Theme) {
        let color = theme.tokens.colors.text_primary;
        self.accent_rgb = Some([color.r(), color.g(), color.b()]);
        self.syntax = Some(theme.tokens.syntax.clone());
        self.refresh_attributes();
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
        marks::carry_through_edit(&mut self.marks, at, inserted, removed);
        if let Some((closed_at, closed)) = self.closed.take()
            && closed_at == at
            && inserted > 0
        {
            for mark in &closed {
                marks::remove_mark_from_range(
                    &mut self.marks,
                    at..at.saturating_add(inserted),
                    mark,
                );
            }
        }

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
        if self.inline_formatting
            && inserted > 0
            && let Some(prefix) = new_text.as_bytes().get(..caret)
            && prefix.ends_with(b"[[")
            && let Some(open_at) = caret.checked_sub(2)
        {
            self.signal = Some(LineSignal::LinkOpen { open_at });
        }
    }

    /// Give the input attributed text before it measures or paints. Overpainting
    /// plain glyphs cannot preserve wrapping or pointer/caret geometry.
    fn sync_wrapped_spans(&mut self, theme: &Theme) {
        if self.wrap && !self.inline_formatting && !self.transient_colors.is_empty() {
            let text = self.value();
            let mut boundaries = vec![0, text.len()];
            for color in &self.transient_colors {
                for boundary in [color.range.start, color.range.end] {
                    if text.is_char_boundary(boundary) {
                        boundaries.push(boundary);
                    }
                }
            }
            boundaries.compact_sort();
            boundaries.dedup();
            let spans = boundaries
                .windows(2)
                .filter_map(|range| {
                    let &[start, end] = range else {
                        return None;
                    };
                    let mut span = crate::richtext::RichSpan::new(text.get(start..end)?);
                    if self.inner.is_mono() {
                        span.family = Some(crate::richtext::FontFamily::Mono);
                    }
                    if let Some(color) = self
                        .transient_colors
                        .iter()
                        .rev()
                        .find(|color| color.range.contains(&start))
                    {
                        let color = transient_color(theme, color.kind);
                        span.color = Some([color.r(), color.g(), color.b()]);
                    }
                    Some(span)
                })
                .collect();
            self.inner.set_rich_spans(Some(spans));
            return;
        }
        self.emoji_revision = window_emoji_revision();
        if !self.wrap || !self.inline_formatting {
            self.inner.set_rich_spans(None);
            return;
        }
        if self.marks.is_empty() {
            let text = self.value();
            let spans = inline_emoji::writes_window_emoji(text)
                .then(|| inline_emoji::split_spans_in_window(vec![RichSpan::new(text)]));
            self.inner.set_rich_spans(spans);
            return;
        }
        let text = self.value();
        let spans: Vec<RichSpan> = marks::segments_for(text.len(), &self.marks)
            .into_iter()
            .filter_map(|segment| {
                let source = text.get(segment.range.clone())?;
                let mut span = crate::richtext::RichSpan::new(source)
                    .bold(segment.bold || segment.mention.is_some())
                    .italic(segment.italic);
                span.kind = mark_kind(&segment);
                if let Some(id) = segment.mention.as_deref() {
                    let (ink, band) = self.mention_rgb(id);
                    span.color = ink;
                    span.background = band;
                }
                if segment.code {
                    span.family = Some(crate::richtext::FontFamily::Mono);
                    let background = theme.tokens.colors.surfaces.raised;
                    span.background = Some([background.r(), background.g(), background.b()]);
                }
                if segment.code
                    || segment.link.is_some()
                    || segment.external_link
                    || segment.resource_link.is_some()
                {
                    let color = theme.tokens.colors.text_primary;
                    span.color = Some([color.r(), color.g(), color.b()]);
                }
                Some(span)
            })
            .collect();
        let spans = if text.contains(':') {
            inline_emoji::split_spans_in_window(spans)
        } else {
            spans
        };
        self.inner.set_rich_spans(Some(spans));
    }

    /// Whether the window's workspace emoji changed since the line's runs
    /// were built, for a line that writes a `:name:` at all. Asked where no
    /// window's code runs, nothing moved.
    fn emoji_moved(&self) -> bool {
        self.inline_formatting
            && self.value().contains(':')
            && window_emoji_revision().is_some_and(|now| self.emoji_revision != Some(now))
    }

    fn draw_wrapped_mark_decorations(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if !self.inline_formatting {
            return;
        }
        for span in &self.marks {
            let (color, strike) = match span.mark {
                Mark::Link(_)
                | Mark::Transclusion(_)
                | Mark::ExternalLink(_)
                | Mark::ResourceLink(_)
                | Mark::Underline => (theme.tokens.colors.text_primary, false),
                Mark::Strikethrough => (theme.tokens.colors.text_primary, true),
                _ => continue,
            };
            for bounds in self
                .inner
                .selection_rects(theme, span.range.start, span.range.end)
            {
                let y = if strike {
                    bounds.y0 + bounds.height() * 0.52
                } else {
                    bounds.y1 - 1.0
                };
                let rule = Rect::new(bounds.x0, y - 1.0, bounds.x1, y + 1.0);
                scene.fill(vello::peniko::Fill::NonZero, transform, color, None, &rule);
            }
        }
    }

    pub fn mark_rects(&self, theme: &Theme, range: std::ops::Range<usize>) -> Vec<Rect> {
        // Selection geometry is also the painted geometry for a marked run.
        // In particular it preserves right-to-left ranges (whose logical end
        // can be left of their start) and disjoint bands in mixed-direction
        // text. Collapsing those carets into one `x0..x1` interval either
        // drops the RTL run or makes an unmarked visual gap clickable.
        self.inner.selection_rects(theme, range.start, range.end)
    }

    /// [`Self::mark_rects`] under the theme the line was last laid out in,
    /// for a host that scrolls a range into view between frames.
    #[must_use]
    pub fn prepared_range_rects(&self, range: std::ops::Range<usize>) -> Vec<Rect> {
        self.mark_rects(&self.measurement_theme, range)
    }
}

impl RichTextLine {
    /// Resolve everything layout reads from the theme. A host that lays out
    /// many lines shares one `Arc` per theme, so an unchanged theme costs a
    /// pointer comparison instead of re-deriving attributes for every line.
    pub(super) fn prepare_layout(&mut self, theme: &Arc<Theme>) {
        if !Arc::ptr_eq(&self.measurement_theme, theme) {
            // Font overrides remain base values. TextInput's shaped layout
            // applies the theme scale exactly once; the row height is
            // resolved by the owner of the surrounding document geometry.
            self.prepare_attributes(theme);
            self.inner.capture_layout_text_scale(theme.text_scale);
            self.measurement_theme = Arc::clone(theme);
            self.stack_measure = None;
        }
        self.inner.set_base_size(self.font_size);
        self.inner.set_multiline(self.wrap);
        self.sync_wrapped_spans(theme);
    }

    /// The fixed row height of a line that is not measured, and the minimum
    /// of one that is: the line's size at its role's leading (Body 1.45,
    /// Heading 1.3, a title tighter), so headings stand taller than
    /// paragraph rows without a measure pass and the rhythm is the one
    /// every other surface uses.
    pub(super) fn row_height(&self, text_scale: f32) -> f32 {
        let style = crate::tokens::typography::stock().style(self.inner.text_role());
        let leading = (style.line_height_px() / style.size.max(1.0)).max(1.0);
        (self.font_size * text_scale * leading).max(20.0 * text_scale)
    }

    /// Whether the line's height comes from measuring its content rather
    /// than from [`Self::row_height`].
    pub(super) fn measures_height(&self) -> bool {
        self.wrap || self.table_preview.is_some() || self.image_preview.is_present()
    }

    /// The height last measured at `width`, while nothing that feeds the
    /// measurement has changed since.
    pub(super) fn stacked_height(&self, width: f32) -> Option<f32> {
        self.stack_measure
            .filter(|measure| measure.width.to_bits() == width.to_bits())
            .filter(|_| !self.layout_needs_measure())
            .map(|measure| measure.height)
    }

    pub(super) fn forget_stacked_height(&mut self) {
        self.stack_measure = None;
    }

    /// Measure the line's content at `width` for a host that stacks lines
    /// itself, remembering the result for [`Self::stacked_height`].
    pub(super) fn measure_stacked(&mut self, width: f32, text_renderer: &mut TextRenderer) -> f32 {
        let height = self
            .measure(
                Size {
                    width: Some(width),
                    height: None,
                },
                Size {
                    width: taffy::AvailableSpace::Definite(width),
                    height: taffy::AvailableSpace::MaxContent,
                },
                text_renderer,
            )
            .height;
        if self.wrap && self.table_preview.is_none() && !self.image_preview.is_present() {
            self.inner.adopt_measured_layout();
        }
        self.stack_measure = Some(StackMeasure { width, height });
        height
    }

    /// The line's size for `known` / `available`, shaped with the
    /// measurement theme; [`Widget::measure`] acknowledges it.
    fn measure_content(
        &mut self,
        known: Size<Option<f32>>,
        available: Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> Size<f32> {
        let theme = Arc::clone(&self.measurement_theme);
        self.sync_wrapped_spans(&theme);
        if let Some(visual) = self.table_preview {
            let width = known
                .width
                .unwrap_or(match available.width {
                    taffy::AvailableSpace::Definite(width) => width,
                    _ => 400.0,
                })
                .max(1.0);
            self.table_source_height = if visual {
                0.0
            } else {
                self.inner
                    .measure_wrapped_content(width, text_renderer, &self.measurement_theme)
            };
            let preview_height = super::block_editor::measure_table_preview_height(
                self.value(),
                width,
                text_renderer,
                &self.measurement_theme,
            );
            return Size {
                width,
                height: self.table_source_height
                    + preview_height
                    + super::block_editor::table_toolbar_height(
                        width,
                        self.measurement_theme.text_scale,
                    ),
            };
        }
        if let ImagePreview::Present {
            dimensions,
            editing,
            max_height,
            width: sized,
        } = self.image_preview
        {
            let width = known
                .width
                .unwrap_or(match available.width {
                    taffy::AvailableSpace::Definite(width) => width,
                    _ => 400.0,
                })
                .max(1.0);
            self.image_source_height = if editing {
                self.inner.measure_wrapped_content(
                    (width - self.layout_inset).max(1.0),
                    text_renderer,
                    &self.measurement_theme,
                )
            } else {
                0.0
            };
            // The image stands on the editor's content edge, inside the
            // line's text inset on both sides, at its native aspect, and
            // never taller than its share of the visible height: a capped
            // picture is painted narrower, its aspect kept.
            let pad = self.text_inset(&self.measurement_theme) as f32;
            let image_width =
                (width - 2.0 * pad)
                    .max(1.0)
                    .min(sized.map_or(f32::INFINITY, |sized| {
                        crate::num::f32_of_u32(sized) * self.measurement_theme.text_scale
                    }));
            let preview_height = dimensions
                .map_or(
                    220.0 * self.measurement_theme.text_scale,
                    |(image_w, image_h)| {
                        image_width * image_h.max(1) as f32 / image_w.max(1) as f32
                    },
                )
                .min(max_height.map_or(f32::INFINITY, |max| crate::num::f32_of_u32(max.max(1))));
            return Size {
                width,
                height: self.image_source_height
                    + if editing {
                        6.0 * self.measurement_theme.text_scale
                    } else {
                        0.0
                    }
                    + preview_height,
            };
        }
        if !self.wrap {
            self.prepare_attributes(&self.measurement_theme.clone());
            return self.inner.measure(known, available, text_renderer);
        }
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(width) => width,
            _ => 400.0,
        });
        let height = self.inner.measure_wrapped_content(
            (width - self.layout_inset).max(1.0),
            text_renderer,
            &self.measurement_theme,
        );
        Size {
            width,
            height: height + self.layout_header_height,
        }
    }
}

impl Default for RichTextLine {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for RichTextLine {
    /// The inner field's timers (a settling squiggle, a suggestion for the
    /// word under the caret) must wake the host; the field is not a child.
    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        self.inner.next_wake_deadline()
    }

    fn layout_needs_measure(&self) -> bool {
        (self.wrap && (self.dirty.styled_layout || self.emoji_moved()))
            || self.inner.layout_needs_measure()
    }

    /// A line's height follows its text and its inline marks, both of which
    /// `measure` re-applies; the childless default would dirty every line.
    fn is_layout_dirty(&self) -> bool {
        self.dirty.styled_layout || self.emoji_moved() || self.inner.is_layout_dirty()
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = crate::doc::document_ref(state, path);
        // Realigning the mark-diff snapshot here keeps a remotely-synced edit
        // from being misread as a local splice on the next keystroke.
        self.inner.sync(state, path);
        self.align_bound_projection(state);
        self.refresh_attributes();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.prepare_layout(&Arc::new(theme.clone()));
        let height = self.row_height(theme.text_scale);
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: if self.measures_height() {
                    Dimension::auto()
                } else {
                    Dimension::length(height)
                },
            },
            min_size: Size {
                width: Dimension::auto(),
                height: Dimension::length(height),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = crate::widget::layout_rect(taffy, node, origin);
        self.inner.apply_layout_external(rect);
    }

    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        available: Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> Size<f32> {
        let size = self.measure_content(known, available, text_renderer);
        // Acknowledged once the measurement is done: measuring adopts the
        // theme's field geometry, which marks the input's layout dirty the
        // first time, yet the size just measured already reflects it.
        self.dirty.styled_layout = false;
        self.inner.acknowledge_layout_measure();
        size
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        #[cfg(test)]
        let attributes_probe = crate::profiling::frame_probe::span("line_attributes");
        self.prepare_attributes(theme);
        self.sync_wrapped_spans(theme);
        #[cfg(test)]
        drop(attributes_probe);
        #[cfg(test)]
        let input_probe = crate::profiling::frame_probe::span("text_input_draw");
        // TextInput owns glyph shaping and painting. This wrapper supplies
        // attributes and draws only mark decorations that are not glyphs.
        self.inner
            .draw(scene, transform, text_renderer, theme, presence);
        #[cfg(test)]
        drop(input_probe);

        if self.marks.is_empty() && self.transient_colors.is_empty() {
            return;
        }

        let text = self.inner.value().to_string();
        let segments = if self.inline_formatting {
            marks::segments_for(text.len(), &self.marks)
        } else {
            Vec::new()
        };
        if segments.is_empty() && self.transient_colors.is_empty() {
            return;
        }

        if self.wrap {
            self.draw_wrapped_mark_decorations(scene, transform, theme);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
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
            if (ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key())
                && let Key::Character(c) = logical_key
                && self.handle_mark_shortcut(c)
            {
                *ctx.handled = true;
                return;
            }

            match logical_key {
                // Wrapped prose owns real newlines in its Text object. Let
                // TextInput splice Shift+Enter so marks, undo, and peers see
                // the same line break, without creating another list item.
                Key::Named(NamedKey::Enter)
                    if self.wrap && self.inline_formatting && ctx.modifiers.state().shift_key() => {
                }
                Key::Named(NamedKey::Enter) => {
                    // Shift+Enter is a soft-break; plain Enter splits.
                    let split_at = self.inner.cursor_pos();
                    self.signal = if ctx.modifiers.state().shift_key() {
                        Some(LineSignal::SoftNewline { split_at })
                    } else {
                        Some(LineSignal::Enter { split_at })
                    };
                    *ctx.handled = true;
                    return;
                }
                Key::Named(NamedKey::Backspace) => {
                    // Otherwise the inner input handles the delete.
                    if self.inner.cursor_pos() == 0 && self.selection_range().is_none() {
                        self.signal = Some(LineSignal::BackspaceAtStart);
                        *ctx.handled = true;
                        return;
                    }
                }
                Key::Named(NamedKey::ArrowUp)
                    if !self.wrap
                        || !self.inner.has_visual_row(false)
                        || ((ctx.modifiers.state().control_key()
                            || ctx.modifiers.state().super_key())
                            && ctx.modifiers.state().shift_key()) =>
                {
                    let m = ctx.modifiers.state();
                    self.signal = if m.shift_key() {
                        Some(LineSignal::SelectBlockUp {
                            caret: self.inner.cursor_pos(),
                        })
                    } else {
                        Some(LineSignal::ArrowUp)
                    };
                    *ctx.handled = true;
                    return;
                }
                Key::Named(NamedKey::ArrowDown)
                    if !self.wrap
                        || !self.inner.has_visual_row(true)
                        || ((ctx.modifiers.state().control_key()
                            || ctx.modifiers.state().super_key())
                            && ctx.modifiers.state().shift_key()) =>
                {
                    let m = ctx.modifiers.state();
                    self.signal = if m.shift_key() {
                        Some(LineSignal::SelectBlockDown {
                            caret: self.inner.cursor_pos(),
                        })
                    } else {
                        Some(LineSignal::ArrowDown)
                    };
                    *ctx.handled = true;
                    return;
                }
                _ => {}
            }
        }

        // A word typed in inline code is code: it is never autocorrected,
        // nor is the word a typed backtick closes into code.
        let in_code = self.caret_in_code() || self.closes_inline_code(event);
        self.inner.set_autocorrect(in_code.then_some(false));
        self.inner.handle_event(event, ctx);

        // Reconcile marks against any text edit using byte-deltas from the previous snapshot.
        let new_text = self.inner.value().to_string();
        if new_text != self.prev_text {
            self.reconcile_marks_after_edit(&new_text);
            self.prev_text = new_text;
            self.refresh_attributes();
        }
        self.prev_cursor = self.inner.cursor_pos();
        if self
            .closed
            .as_ref()
            .is_some_and(|(at, _)| *at != self.prev_cursor)
        {
            self.closed = None;
        }
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

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        Some(crate::ime::InputContent::TEXT)
    }

    /// `multiline` is cleared — a block editor stacks many of these to form a document.
    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.inner.ime_edit_fingerprint(key)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Widget::accessibility(&self.inner)
    }

    fn accessibility_hidden(&self) -> bool {
        Widget::accessibility_hidden(&self.inner)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let (state, path) = crate::doc::document(state, path);
        let handled = Widget::perform_action(&mut self.inner, action, &[], state, path);
        if handled && matches!(action, crate::a11y::Action::SetValue(_)) {
            self.marks.clear();
            self.pending_marks.clear();
            self.prev_text = self.inner.value().to_string();
            self.prev_cursor = self.inner.cursor_pos();
            self.refresh_attributes();
        }
        handled
    }
}

/// The revision of the window's workspace emoji, if its host registered any.
fn window_emoji_revision() -> Option<u64> {
    crate::workspace_emoji::current().map(|source| source.revision())
}

fn transient_color(theme: &Theme, kind: TokenKind) -> vello::peniko::Color {
    theme.tokens.syntax.color(kind)
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
    let mut prefix = oa.iter().zip(na.iter()).take_while(|(o, n)| o == n).count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix = prefix.saturating_sub(1);
    }

    let o_rest = oa.len().saturating_sub(prefix);
    let n_rest = na.len().saturating_sub(prefix);

    // Longest common suffix within the bytes past the prefix. `zip` already
    // stops at the shorter of the two (reversed, prefix-skipped) iterators,
    // matching the original bound of `min(o_rest, n_rest)`.
    let mut suffix = oa
        .iter()
        .skip(prefix)
        .rev()
        .zip(na.iter().skip(prefix).rev())
        .take_while(|(o, n)| o == n)
        .count();

    while !old.is_char_boundary(old.len().saturating_sub(suffix))
        || !new.is_char_boundary(new.len().saturating_sub(suffix))
    {
        suffix = suffix.saturating_sub(1);
    }
    let removed = o_rest.saturating_sub(suffix);
    let inserted = n_rest.saturating_sub(suffix);
    (prefix, inserted, removed)
}

/// Used by `MainPanel::handle_event` to route clicks on link spans to
/// the page-navigation handler.
pub fn link_at_byte(marks_list: &[MarkSpan], byte: usize) -> Option<PageId> {
    for s in marks_list {
        if let Mark::Link(id) | Mark::Transclusion(id) = &s.mark
            && s.range.start <= byte
            && byte < s.range.end
        {
            return Some(*id);
        }
    }
    None
}

/// What a marked stretch is: a mention names a person
/// ([`TextKind::Identifier`](crate::text_render::TextKind::Identifier)) and
/// an external link is a [`TextKind::Url`](crate::text_render::TextKind::Url),
/// whatever its label says. Everything else is found in the prose.
fn mark_kind(segment: &MarkSegment) -> Option<crate::text_render::TextKind> {
    if segment.code {
        None
    } else if segment.mention.is_some() {
        Some(crate::text_render::TextKind::Identifier)
    } else if segment.external_link {
        Some(crate::text_render::TextKind::Url)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Dispatch, dispatch};
    use crate::widget::marks::Mark;
    use winit::keyboard::{ModifiersKeyState, ModifiersState, NamedKey};

    use crate::synthetic_input::named_key;

    use crate::synthetic_input::char_key;

    fn layout_and_draw(line: &mut RichTextLine, theme: &Theme, width: f32) -> Scene {
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map = HashMap::new();
        let node = line.build_layout(&mut taffy, theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(width),
                    height: taffy::AvailableSpace::MaxContent,
                },
            )
            .expect("rich line layout");
        line.apply_layout(&taffy, node, Point::ZERO);

        let mut renderer = TextRenderer::new();
        renderer.set_text_scale(theme.text_scale);
        let mut scene = Scene::new();
        line.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut renderer,
            theme,
            &PresenceCtx::empty(),
        );
        scene
    }

    fn rendered_advance_at_scale(scale: f32) -> f64 {
        let mut line = RichTextLine::new().with_text("Calendar").with_size(16.0);
        let theme = Theme::dark().with_text_scale(scale);
        let mut renderer = TextRenderer::new();
        renderer.set_text_scale(scale);
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let node = line.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(640.0),
                    height: taffy::AvailableSpace::MaxContent,
                },
            )
            .expect("line layout");
        line.apply_layout(&taffy, node, Point::ZERO);
        line.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut renderer,
            &theme,
            &PresenceCtx::empty(),
        );
        line.inner()
            .char_x_positions()
            .last()
            .copied()
            .expect("terminal caret advance")
    }

    #[test]
    fn rich_line_shapes_once_at_every_scale() {
        {
            let normal = rendered_advance_at_scale(1.0);
            let doubled = rendered_advance_at_scale(2.0);
            assert!(
                (doubled - normal * 2.0).abs() < 0.5,
                "2× text must have 2× advance, not 4×: {normal} -> {doubled}"
            );
        }

        {
            let mut line = RichTextLine::new().with_text("alpha beta").with_size(16.0);
            line.set_marks(vec![MarkSpan::new(0..5, Mark::Code)]);
            line.inner_mut().set_selection(0, 5);

            let normal = Theme::dark();
            layout_and_draw(&mut line, &normal, 320.0);
            assert_eq!(line.inner().shaped_font_size_for_test(), Some(16.0));
            assert!((line.caret_rect(&normal, 5).height() - 20.0).abs() < 0.1);
            assert!((line.selection_rect(&normal, 0, 5).height() - 20.0).abs() < 0.1);

            let large = Theme::dark().with_text_scale(2.0);
            layout_and_draw(&mut line, &large, 320.0);
            assert_eq!(line.inner().shaped_font_size_for_test(), Some(32.0));
            let caret = line.caret_rect(&large, 5);
            assert!(
                (caret.height() - 40.0).abs() < 0.1,
                "scaled caret height was {}",
                caret.height()
            );
            let selection = line.selection_rect(&large, 0, 5);
            assert!(
                (selection.height() - 40.0).abs() < 0.1,
                "scaled selection height was {}",
                selection.height()
            );

            let target = 6;
            let x = line.pixel_x_for_byte(&large, target);
            assert_eq!(
                line.byte_for_pixel_x(&large, x),
                target,
                "pointer inverse lands on the scaled shaped caret stop"
            );
        }
    }

    #[test]
    fn rich_mark_geometry_is_authoritative_for_paint_selection_and_pointer() {
        // Case `rich_marks_utf8_replacement_keeps_complete_boundaries`.
        {
            assert_eq!(diff_splice("éx", "êx"), (0, 2, 2));
            assert_eq!(diff_splice("xé", "xê"), (1, 2, 2));
        }

        {
            let mut line = RichTextLine::new().with_text("aBz");
            line.set_marks(vec![MarkSpan::new(1..2, Mark::Bold)]);
            let scene = layout_and_draw(&mut line, &Theme::dark(), 320.0);
            assert_eq!(scene.encoding().resources.glyphs.len(), 3);
        }

        {
            for scale in [1.0, 2.0] {
                let theme = Theme::dark().with_text_scale(scale);
                let mut line = RichTextLine::new().with_text("WWW end");
                layout_and_draw(&mut line, &theme, 400.0);
                let plain = line.pixel_x_for_byte(&theme, 3);
                line.set_marks(vec![MarkSpan::new(0..3, Mark::Bold)]);
                layout_and_draw(&mut line, &theme, 400.0);
                let bold = line.pixel_x_for_byte(&theme, 3);
                assert!(
                    (bold - plain).abs() > 0.1,
                    "bold must change the authoritative advances"
                );
                assert!((line.caret_rect(&theme, 3).x0 - bold).abs() < 0.01);
                assert!((line.selection_rect(&theme, 0, 3).x1 - bold).abs() < 0.01);
                assert_eq!(line.byte_for_pixel_x(&theme, bold), 3);
                line.set_marks(vec![]);
                layout_and_draw(&mut line, &theme, 400.0);
                assert!((line.pixel_x_for_byte(&theme, 3) - plain).abs() < 0.01);
            }
        }

        {
            for scale in [1.0, 2.0] {
                let theme = Theme::dark().with_text_scale(scale);
                let mut line = RichTextLine::new().with_text("prefix WWW tail");
                line.set_marks(vec![MarkSpan::new(7..10, Mark::Bold)]);
                line.set_focus(true);
                line.inner_mut().set_selection(7, 10);
                layout_and_draw(&mut line, &theme, 80.0);
                assert!(line.inner().scroll_x() > 0.0);
                let x = line.pixel_x_for_byte(&theme, 10);
                assert!(x <= line.get_rect().x1);
                assert!((line.caret_rect(&theme, 10).x0 - x).abs() < 0.01);
                assert!((line.selection_rect(&theme, 7, 10).x1 - x).abs() < 0.01);
                assert_eq!(line.byte_for_pixel_x(&theme, x), 10);
            }
        }
    }

    #[test]
    fn bound_line_reads_the_document_and_keeps_edits_for_its_owner() {
        {
            use automerge::{ObjType, ROOT, transaction::Transactable};
            let mut doc = automerge::AutoCommit::new();
            let tid = doc.put_object(&ROOT, "t", ObjType::Text).unwrap();
            doc.splice_text(&tid, 0, 0, "hello").unwrap();

            let mut line = RichTextLine::new();
            line.bind_text(tid.clone());
            line.sync(&doc, &ROOT);
            assert_eq!(line.value(), "hello");
        }

        {
            use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
            let mut doc = automerge::AutoCommit::new();
            let tid = doc.put_object(&ROOT, "t", ObjType::Text).unwrap();

            let mut line = RichTextLine::new();
            line.bind_text(tid.clone());
            line.set_focus(true);

            // Type against OUR doc, not the throwaway one `dispatch` would make.
            let mut on_doc = Dispatch::new().doc(&mut doc);
            for c in ["h", "i"] {
                on_doc.send(&mut line, &char_key(c));
            }

            assert_eq!(line.value(), "hi");
            assert_eq!(doc.text(&tid).unwrap(), "", "the owner writes the edit");
            assert_eq!(line.take_dirty().as_deref(), Some("hi"));
        }

        {
            use automerge::{ObjType, ROOT, transaction::Transactable};
            let mut doc = automerge::AutoCommit::new();
            let tid = doc.put_object(&ROOT, "t", ObjType::Text).unwrap();
            doc.splice_text(&tid, 0, 0, "hi").unwrap();
            doc.mark(
                &tid,
                automerge::marks::Mark::new(
                    crate::widget::text_input::AUTHOR_MARK.to_string(),
                    "alice",
                    0,
                    2,
                ),
                automerge::marks::ExpandMark::None,
            )
            .unwrap();

            let mut line = RichTextLine::new();
            line.bind_text(tid.clone());
            line.sync(&doc, &ROOT);
            assert!(
                line.author_runs().iter().any(|(_, _, a)| a == "alice"),
                "expected an alice author run, got {:?}",
                line.author_runs()
            );
        }

        {
            assert_eq!(diff_splice("abc", "abXc"), (2, 1, 0));
            assert_eq!(diff_splice("abc", "abc"), (3, 0, 0));
            assert_eq!(diff_splice("abc", "ab"), (2, 0, 1));
            assert_eq!(diff_splice("", "hi"), (0, 2, 0));
        }
    }

    #[test]
    fn construction_and_programmatic_state_do_not_count_as_edits() {
        {
            let l = RichTextLine::new();
            assert_eq!(l.value(), "");
            assert!(!l.is_focused());
            assert!(l.cursor_at_start());
            assert!(l.cursor_at_end());
        }

        {
            let l = RichTextLine::new().with_text("hello");
            assert_eq!(l.value(), "hello");
            assert!(l.cursor_at_end());
            assert!(!l.cursor_at_start());
        }

        {
            let mut l = RichTextLine::new();
            l.set_text("seeded");
            assert!(
                l.take_dirty().is_none(),
                "programmatic set_text is not dirty"
            );
            assert_eq!(l.value(), "seeded");
            assert!(l.cursor_at_end());
        }

        {
            let mut l = RichTextLine::new().with_text("abc");
            l.set_cursor_pos(0);
            assert!(l.cursor_at_start());
            assert!(!l.cursor_at_end());
            l.set_cursor_pos(3);
            assert!(l.cursor_at_end());
        }

        {
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

        {
            let mut l = RichTextLine::new().with_text("x");
            Widget::set_focus(&mut l, true);
            let node = Widget::accessibility(&l).unwrap();
            assert!(node.state.focused);
        }
    }

    #[test]
    fn boundary_keys_yield_block_signals() {
        {
            let mut l = RichTextLine::new().with_text("abcdef");
            l.set_focus(true);
            l.set_cursor_pos(3);
            dispatch(&mut l, &named_key(NamedKey::Enter));
            match l.take_signal() {
                Some(LineSignal::Enter { split_at: 3 }) => {}
                other => panic!("expected Enter@3, got {other:?}"),
            }
            assert_eq!(l.value(), "abcdef");
        }

        {
            let mut l = RichTextLine::new().with_text("hi");
            l.set_focus(true);
            dispatch(&mut l, &named_key(NamedKey::Enter));
            match l.take_signal() {
                Some(LineSignal::Enter { split_at }) => assert_eq!(split_at, 2),
                other => panic!("expected Enter@2, got {other:?}"),
            }
        }

        {
            let mut l = RichTextLine::new().with_text("abc");
            l.set_focus(true);
            l.set_cursor_pos(0);
            dispatch(&mut l, &named_key(NamedKey::Backspace));
            assert_eq!(l.take_signal(), Some(LineSignal::BackspaceAtStart));
            assert_eq!(l.value(), "abc");
        }

        {
            let mut l = RichTextLine::new().with_text("abc");
            l.set_focus(true);
            l.set_cursor_pos(3);
            dispatch(&mut l, &named_key(NamedKey::Backspace));
            assert!(l.take_signal().is_none());
            assert_eq!(l.value(), "ab");
            assert!(l.take_dirty().is_some());
        }

        {
            let mut l = RichTextLine::new().with_text("x");
            l.set_focus(true);
            dispatch(&mut l, &named_key(NamedKey::ArrowUp));
            assert_eq!(l.take_signal(), Some(LineSignal::ArrowUp));
            dispatch(&mut l, &named_key(NamedKey::ArrowDown));
            assert_eq!(l.take_signal(), Some(LineSignal::ArrowDown));
        }

        {
            let mut l = RichTextLine::new();
            l.set_focus(true);
            dispatch(&mut l, &char_key("["));
            // First `[` shouldn't emit.
            assert!(l.take_signal().is_none());
            dispatch(&mut l, &char_key("["));
            match l.take_signal() {
                Some(LineSignal::LinkOpen { open_at: 0 }) => {}
                other => panic!("expected LinkOpen{{open_at:0}}, got {other:?}"),
            }
            assert_eq!(l.value(), "[[");
        }
    }

    #[test]
    fn typing_edits_only_when_focused_and_reports_one_dirty_edge() {
        {
            let mut l = RichTextLine::new();
            l.set_focus(true);
            dispatch(&mut l, &char_key("h"));
            dispatch(&mut l, &char_key("i"));
            assert_eq!(l.value(), "hi");
            let dirty = l.take_dirty();
            assert_eq!(dirty.as_deref(), Some("hi"));
            assert!(l.take_dirty().is_none());
        }

        {
            let mut l = RichTextLine::new().with_text("abc");
            dispatch(&mut l, &named_key(NamedKey::Enter));
            assert!(l.take_signal().is_none(), "unfocused line must not emit");
            dispatch(&mut l, &char_key("z"));
            assert_eq!(l.value(), "abc");
        }

        {
            let mut line = RichTextLine::new().with_text("text");
            let mut doc = AutoCommit::new();
            assert!(Widget::perform_action(
                line.inner_mut(),
                &crate::a11y::Action::SetSelection {
                    anchor: 0,
                    focus: 4
                },
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            let _ = line.take_dirty(); // Selection has its own presentation edge.
            line.toggle_mark(Mark::Bold);
            assert_eq!(line.take_dirty().as_deref(), Some("text"));
            assert!(line.take_dirty().is_none());
            line.set_marks(Vec::new());
            assert!(line.take_dirty().is_none());
            assert!(Widget::perform_action(
                line.inner_mut(),
                &crate::a11y::Action::SetSelection {
                    anchor: 4,
                    focus: 4
                },
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            let _ = line.take_dirty(); // Selection has its own presentation edge.
            line.toggle_mark(Mark::Bold);
            assert!(
                line.take_dirty().is_none(),
                "pending typing style changes no content yet"
            );
        }
    }

    #[test]
    fn marks_toggle_apply_pending_and_shift_with_edits() {
        {
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

        {
            let mut line = RichTextLine::new().with_text("hello");
            line.inner_mut().set_selection(0, 5);
            line.toggle_mark(Mark::Bold);
            assert_eq!(line.take_dirty().as_deref(), Some("hello"));
            assert!(line.take_dirty().is_none());
            assert_eq!(line.marks(), &[MarkSpan::new(0..5, Mark::Bold)]);
            line.toggle_mark(Mark::Italic);
            line.set_text("replacement");
            assert!(
                line.take_dirty().is_none(),
                "programmatic replacement discards the old edit"
            );
        }

        {
            let mut l = RichTextLine::new();
            l.set_focus(true);
            l.toggle_mark(Mark::Bold);
            assert_eq!(l.pending_marks().len(), 1);
            dispatch(&mut l, &char_key("x"));
            assert_eq!(l.value(), "x");
            assert!(l.pending_marks().is_empty());
            assert_eq!(l.marks().len(), 1);
            assert_eq!(l.marks()[0].range, 0..1);
            assert_eq!(l.marks()[0].mark, Mark::Bold);
        }

        {
            let mut l = RichTextLine::new().with_text("hello");
            l.set_marks(vec![MarkSpan::new(2..4, Mark::Bold)]);
            l.set_focus(true);
            l.set_cursor_pos(0);
            dispatch(&mut l, &char_key("X"));
            assert_eq!(l.value(), "Xhello");
            assert_eq!(l.marks()[0].range, 3..5);
        }

        {
            let mut l = RichTextLine::new().with_text("Xhello");
            l.set_marks(vec![MarkSpan::new(3..5, Mark::Bold)]);
            l.set_focus(true);
            l.set_cursor_pos(1);
            dispatch(&mut l, &named_key(NamedKey::Backspace));
            assert_eq!(l.value(), "hello");
            assert_eq!(l.marks()[0].range, 2..4);
        }

        {
            let id = uuid::Uuid::from_u128(7);
            let marks = vec![MarkSpan::new(2..6, Mark::Link(id))];
            assert_eq!(link_at_byte(&marks, 3), Some(id));
            assert_eq!(link_at_byte(&marks, 1), None);
            assert_eq!(link_at_byte(&marks, 6), None);
        }
    }

    #[test]
    fn literal_code_is_immune_to_formatting_and_note_links() {
        {
            for modifier in [ModifiersState::CONTROL, ModifiersState::SUPER] {
                for shortcut in ["b", "i", "e", "u"] {
                    let mut line = RichTextLine::new().with_text("source").with_mono();
                    line.set_focus(true);
                    line.inner_mut().set_selection(0, 6);
                    line.set_inline_formatting(false);
                    Dispatch::new()
                        .modifiers(modifier)
                        .send(&mut line, &char_key(shortcut));
                    assert_eq!(line.value(), "source");
                    assert!(line.marks().is_empty());
                    assert!(line.pending_marks().is_empty());
                    assert!(line.take_dirty().is_none());

                    line.set_inline_formatting(true);
                    Dispatch::new()
                        .modifiers(modifier)
                        .send(&mut line, &char_key(shortcut));
                    assert_eq!(line.marks().len(), 1, "prose shortcut {shortcut} restored");
                }
            }
        }

        {
            let mut line = RichTextLine::new();
            line.set_focus(true);
            Dispatch::new()
                .modifiers(ModifiersState::CONTROL)
                .send(&mut line, &char_key("b"));
            assert_eq!(line.pending_marks(), &[Mark::Bold]);
            line.set_inline_formatting(false);
            dispatch(&mut line, &char_key("["));
            dispatch(&mut line, &char_key("["));
            assert_eq!(line.value(), "[[");
            assert!(line.take_signal().is_none());
            assert!(line.marks().is_empty());
            line.set_inline_formatting(true);
            dispatch(&mut line, &char_key("["));
            assert!(matches!(
                line.take_signal(),
                Some(LineSignal::LinkOpen { .. })
            ));
        }
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

    /// A line in a window whose workspace has an emoji draws its picture,
    /// wrapped or not, marked or not; it stops once the workspace drops it.
    #[test]
    fn a_workspace_emoji_draws_as_its_picture_until_the_workspace_drops_it() {
        use crate::richtext::inline_emoji::tests::{Fixed, workspace};
        let draws_picture = |scene: &Scene| {
            scene
                .encoding()
                .draw_tags
                .contains(&vello_encoding::DrawTag::IMAGE)
        };
        let theme = Theme::dark();
        let text = "nice :party_parrot: work";
        let at = text.find(':').unwrap();
        let emoji = at..at + ":party_parrot:".len();

        assert!(
            !draws_picture(&layout_and_draw(
                &mut RichTextLine::new().with_text(text),
                &theme,
                400.0
            )),
            "outside a window the name is text"
        );

        let scope = crate::wake::WakeScope::new();
        let _entry = scope.enter();
        let source = std::rc::Rc::new(Fixed::new(workspace()));
        crate::workspace_emoji::register(&scope.handle(), source.clone());
        for (wrap, marked) in [(false, false), (true, false), (true, true), (false, true)] {
            let mut line = RichTextLine::new().with_text(text);
            line.set_wrap(wrap);
            if marked {
                line.set_marks(vec![MarkSpan {
                    range: 0..4,
                    mark: Mark::Bold,
                }]);
            }
            assert!(
                draws_picture(&layout_and_draw(&mut line, &theme, 400.0)),
                "wrap {wrap}, marked {marked}"
            );
            let rects = line.mark_rects(&theme, emoji.clone());
            let advance = f64::from(crate::richtext::inline_emoji::ADVANCE_EM * line.font_size);
            assert_eq!(rects.len(), 1);
            assert!(
                (rects[0].width() - advance).abs() < 0.5,
                "one emoji wide: {} vs {advance}",
                rects[0].width()
            );
            assert_eq!(line.value(), text, "the text is unchanged");
            assert!(!line.emoji_moved(), "drawn against the emoji now");

            source.set(crate::workspace_emoji::WorkspaceEmoji::default());
            assert!(
                line.emoji_moved() && line.is_layout_dirty(),
                "the workspace's emoji moved"
            );
            assert!(!draws_picture(&layout_and_draw(&mut line, &theme, 400.0)));
            let rects = line.mark_rects(&theme, emoji.clone());
            assert!(
                rects[0].width() > advance * 2.0,
                "`:party_parrot:` is text again"
            );
            source.set(workspace());
        }

        let mut code = RichTextLine::new().with_text(":tilo:");
        code.set_marks(vec![MarkSpan {
            range: 0..6,
            mark: Mark::Code,
        }]);
        for wrap in [false, true] {
            code.set_wrap(wrap);
            assert!(
                !draws_picture(&layout_and_draw(&mut code, &theme, 400.0)),
                "inline code keeps `:tilo:`, wrap {wrap}"
            );
        }
    }
}
