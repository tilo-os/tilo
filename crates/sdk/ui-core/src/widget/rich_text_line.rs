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

use crate::color_compat::ColorCompat;
use crate::richtext::RichSpan;
use crate::widget::text_input::SingleLineAttributes;
use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::{FontChoice, TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::code_editor::TokenKind;
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

/// Matches the body baseline the rest of the UI uses for `TextInput`.
const DEFAULT_FONT_SIZE: f32 = 16.0;

/// Geometry shared by every mark segment `draw` paints in one frame, so
/// `paint_mark_segment` doesn't need a fistful of separate parameters.
#[allow(dead_code, reason = "used by feature-gated rich text rendering")]
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ImagePreview {
    #[default]
    Absent,
    Present {
        dimensions: Option<(u32, u32)>,
        editing: bool,
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
    /// `(byte_start, byte_end, author_id)` read from the bound Text's marks
    /// during `sync`; drives the authorship highlight in `BlockEditor::draw`.
    author_runs: Vec<(usize, usize, String)>,
    /// The colours mentions are painted in; accent ink when unset.
    mention_paint: Option<marks::MentionPaint>,
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
            dirty: RichTextDirty::default(),
            accent_rgb: None,
            syntax: None,
            pending_marks: Vec::new(),
            prev_text: String::new(),
            prev_cursor: 0,
            author_runs: Vec::new(),
            mention_paint: None,
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

    /// For `BlockEditor` heading blocks (H1=28, H2=22, H3=18).
    #[must_use]
    pub fn with_size(mut self, size: f32) -> Self {
        self.stack_measure = None;
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

    /// Retain document reconciliation while routing edits through an owning
    /// action bridge instead of allowing `TextInput` to splice Automerge.
    pub fn set_direct_crdt_writes(&mut self, enabled: bool) {
        self.inner.set_direct_crdt_writes(enabled);
    }

    pub fn set_local_author(&mut self, author: Option<String>) {
        self.inner.set_local_author(author);
    }

    /// Align the line's text within its width.
    pub fn set_align(&mut self, align: crate::richtext::TextAlign) {
        self.inner.set_align(align);
    }

    /// How the line's text is aligned.
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

    /// Change the line's type size; its height is measured again.
    pub fn set_size(&mut self, size: f32) {
        if self.font_size.to_bits() != size.to_bits() {
            self.stack_measure = None;
            self.font_size = size;
            self.inner.set_font_size(size);
        }
    }

    pub fn marks(&self) -> &[MarkSpan] {
        &self.marks
    }

    pub fn set_marks(&mut self, mut marks: Vec<MarkSpan>) {
        marks.sort_by_key(|s| s.range.start);
        self.dirty.styled_layout |= self.marks != marks;
        self.marks = marks;
    }

    pub fn set_transient_colors(&mut self, colors: Vec<TransientColorSpan>) {
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

    pub fn add_mark(&mut self, span: MarkSpan) {
        self.marks.push(span);
        self.marks.sort_by_key(|s| s.range.start);
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

    /// Inverse of `pixel_x_for_byte`, using the shaped caret stops.
    pub fn byte_for_pixel_x(&self, _theme: &Theme, x: f64) -> usize {
        self.inner
            .byte_at_point(Point::new(x, self.inner.get_rect().center().y))
    }

    /// Prefers [`Mark::Link`] over other kinds so link hit-tests still
    /// find the link even if the text is also bold.
    pub fn mark_at_byte(&self, byte: usize) -> Option<&Mark> {
        let mut link_hit: Option<&Mark> = None;
        let mut any_hit: Option<&Mark> = None;
        for s in &self.marks {
            if s.range.start <= byte && byte < s.range.end {
                if matches!(
                    s.mark,
                    Mark::Link(_) | Mark::Transclusion(_) | Mark::ExternalLink(_)
                ) {
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
            boundaries.sort_unstable();
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
                attributes.spans.push(
                    RichSpan::new(slice)
                        .bold(segment.bold || segment.mention.is_some())
                        .italic(segment.italic)
                        .color(color)
                        .background(mention_band),
                );
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
            boundaries.sort_unstable();
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
        if !self.wrap || !self.inline_formatting || self.marks.is_empty() {
            self.inner.set_rich_spans(None);
            return;
        }
        let text = self.value();
        let spans = marks::segments_for(text.len(), &self.marks)
            .into_iter()
            .filter_map(|segment| {
                let source = text.get(segment.range)?;
                let mut span = crate::richtext::RichSpan::new(source)
                    .bold(segment.bold || segment.mention.is_some())
                    .italic(segment.italic);
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
                if segment.code || segment.link.is_some() || segment.external_link {
                    let color = theme.tokens.colors.text_primary;
                    span.color = Some([color.r(), color.g(), color.b()]);
                }
                Some(span)
            })
            .collect();
        self.inner.set_rich_spans(Some(spans));
    }

    fn draw_wrapped_mark_decorations(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if !self.inline_formatting {
            return;
        }
        for span in &self.marks {
            let (color, strike) = match span.mark {
                Mark::Link(_) | Mark::Transclusion(_) | Mark::ExternalLink(_) => {
                    (theme.tokens.colors.text_primary, false)
                }
                Mark::Underline => (theme.tokens.colors.text_primary, false),
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
        self.inner.set_font_size(self.font_size);
        self.inner.set_multiline(self.wrap);
        self.sync_wrapped_spans(theme);
    }

    /// The fixed row height of a line that is not measured, and the minimum
    /// of one that is: `font_size * 1.5`, so headings stand taller than
    /// paragraph rows without a measure pass.
    pub(super) fn row_height(&self, text_scale: f32) -> f32 {
        (self.font_size * text_scale * 1.5).max(20.0 * text_scale)
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
}

impl Default for RichTextLine {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for RichTextLine {
    fn layout_needs_measure(&self) -> bool {
        (self.wrap && self.dirty.styled_layout) || self.inner.layout_needs_measure()
    }

    /// A line's height follows its text and its inline marks, both of which
    /// `measure` re-applies; the childless default would dirty every line.
    fn is_layout_dirty(&self) -> bool {
        self.dirty.styled_layout || self.inner.is_layout_dirty()
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = crate::doc::document(state, path);
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
        let node = taffy.leaf_node(Style {
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

    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        available: Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> Size<f32> {
        let theme = Arc::clone(&self.measurement_theme);
        self.sync_wrapped_spans(&theme);
        self.dirty.styled_layout = false;
        self.inner.acknowledge_layout_measure();
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
            #[allow(
                clippy::cast_precision_loss,
                reason = "bounded image pixel dimensions are converted to the f32 layout coordinate space"
            )]
            let preview_height = dimensions.map_or(
                220.0 * self.measurement_theme.text_scale,
                |(image_width, image_height)| {
                    width * image_height.max(1) as f32 / image_width.max(1) as f32
                },
            );
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

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.prepare_attributes(theme);
        self.sync_wrapped_spans(theme);
        // TextInput owns glyph shaping and painting. This wrapper supplies
        // attributes and draws only mark decorations that are not glyphs.
        self.inner
            .draw(scene, transform, text_renderer, theme, presence);

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

        self.inner.handle_event(event, ctx);

        // Reconcile marks against any text edit using byte-deltas from the previous snapshot.
        let new_text = self.inner.value().to_string();
        if new_text != self.prev_text {
            self.reconcile_marks_after_edit(&new_text);
            self.prev_text = new_text;
            self.refresh_attributes();
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

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Some(winit::window::ImePurpose::Normal)
    }

    /// `multiline` is cleared — a block editor stacks many of these to form a document.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Widget::accessibility(&self.inner)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, path) = crate::doc::document(state, path);
        let handled = Widget::perform_action(&mut self.inner, action, state, path);
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

fn transient_color(theme: &Theme, kind: TokenKind) -> vello::peniko::Color {
    theme.tokens.syntax.color(kind)
}

#[allow(dead_code, reason = "used by feature-gated rich text rendering")]
fn paint_transient_color(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    span: &TransientColorSpan,
    ctx: &MarkDrawCtx<'_>,
) {
    let end = span.range.end.min(ctx.text.len());
    let Some(slice) = ctx.text.get(span.range.start..end) else {
        return;
    };
    if slice.is_empty() {
        return;
    }
    let x_offset = ctx.xs.get(span.range.start).copied().unwrap_or(0.0);
    let x = ctx.rect.x0 + ctx.pad + x_offset - ctx.scroll_x;
    let color = transient_color(theme, span.kind);
    text_renderer.draw_style(
        scene,
        slice,
        color,
        translated(transform, x, ctx.text_y),
        None,
        &TextStyle::new(ctx.text_size).with_family(FontChoice::Monospace),
    );
}

/// Paints one mark segment's overpaint (code chip, link/underline/strike
/// rules, and restyled glyphs) inside the clip layer `draw` already pushed.
/// A pure function of its inputs — no `self` needed.
#[allow(dead_code, reason = "used by feature-gated rich text rendering")]
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
        || seg.link.is_some()
        || seg.external_link;
    if !needs_overpaint {
        return;
    }

    let tokens = &theme.tokens;
    let color = tokens.colors.text_primary;

    let weight = if seg.bold {
        TextWeight::Bold
    } else {
        TextWeight::Regular
    };
    let italic = seg.italic;
    let mono = false;

    text_renderer.draw_style(
        scene,
        slice,
        color,
        translated(transform, x, ctx.text_y),
        None,
        &TextStyle::new(ctx.text_size)
            .with_weight(weight)
            .with_italic(italic)
            .with_family(if mono {
                FontChoice::Monospace
            } else {
                FontChoice::SansSerif
            }),
    );
}

/// Draws the code-chip background and the link/underline/strikethrough
/// rules for one segment — the parts of `paint_mark_segment` that don't
/// depend on the restyled text run itself.
#[allow(dead_code, reason = "used by feature-gated rich text rendering")]
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

    if seg.link.is_some() || seg.external_link {
        let underline = Rect::new(
            x,
            ctx.text_y + ctx.line_height - 3.0,
            sans_end_x,
            ctx.text_y + ctx.line_height - 1.0,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.text_primary,
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
    dead_code,
    clippy::arithmetic_side_effects,
    reason = "feature-gated rendering helper; Affine::mul cannot meaningfully overflow"
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
        if let Mark::Link(id) | Mark::Transclusion(id) = &s.mark
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
    use crate::widget::marks::Mark;
    use winit::event::{KeyEvent, Modifiers};
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
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: &mut state,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: fake_node,
                taffy: &taffy,
            },
        );
    }

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
        // Case `large_text_shapes_once_not_geometry_scale_squared`.
        {
            let normal = rendered_advance_at_scale(1.0);
            let doubled = rendered_advance_at_scale(2.0);
            assert!(
                (doubled - normal * 2.0).abs() < 0.5,
                "2× text must have 2× advance, not 4×: {normal} -> {doubled}"
            );
        }

        // Case `base_font_override_scales_once_across_relayout_caret_selection_and_pointer`.
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

        // Case `chip_mono_width_uses_monospace_face_not_sans`.
        {
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
    }

    #[test]
    fn rich_mark_geometry_is_authoritative_for_paint_selection_and_pointer() {
        // Case `rich_marks_utf8_replacement_keeps_complete_boundaries`.
        {
            assert_eq!(diff_splice("éx", "êx"), (0, 2, 2));
            assert_eq!(diff_splice("xé", "xê"), (1, 2, 2));
        }

        // Case `rich_marks_paint_each_glyph_once`.
        {
            let mut line = RichTextLine::new().with_text("aBz");
            line.set_marks(vec![MarkSpan::new(1..2, Mark::Bold)]);
            let scene = layout_and_draw(&mut line, &Theme::dark(), 320.0);
            assert_eq!(scene.encoding().resources.glyphs.len(), 3);
        }

        // Case `rich_marks_style_only_changes_refresh_authoritative_geometry`.
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

        // Case `rich_marks_scrolled_selection_and_pointer_share_the_painted_end`.
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
    fn bound_line_reads_and_splices_the_document() {
        // Case `bound_line_reads_text_from_doc`.
        {
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

        // Case `editing_bound_line_splices_into_doc`.
        {
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
                    &mut crate::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &Modifiers::default(),
                        state: &mut doc,
                        path: &ROOT,
                        handled: &mut handled,
                        focus_target: &mut focus_target,
                        node_id: NodeId::new(0),
                        taffy: &taffy,
                    },
                );
            }

            assert_eq!(line.value(), "hi");
            assert_eq!(
                doc.text(&tid).unwrap(),
                "hi",
                "edits must splice into the bound Text"
            );
        }

        // Case `authored_edit_stamps_mark_caches_run_and_survives_round_trip`.
        {
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
                    &mut crate::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &Modifiers::default(),
                        state: &mut doc,
                        path: &ROOT,
                        handled: &mut handled,
                        focus_target: &mut focus_target,
                        node_id: NodeId::new(0),
                        taffy: &taffy,
                    },
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

        // Case `diff_splice_detects_simple_insertion`.
        {
            assert_eq!(diff_splice("abc", "abXc"), (2, 1, 0));
            assert_eq!(diff_splice("abc", "abc"), (3, 0, 0));
            assert_eq!(diff_splice("abc", "ab"), (2, 0, 1));
            assert_eq!(diff_splice("", "hi"), (0, 2, 0));
        }
    }

    #[test]
    fn construction_and_programmatic_state_do_not_count_as_edits() {
        // Case `new_starts_empty_unfocused`.
        {
            let l = RichTextLine::new();
            assert_eq!(l.value(), "");
            assert!(!l.is_focused());
            assert!(l.cursor_at_start());
            assert!(l.cursor_at_end());
        }

        // Case `with_text_seeds_value_and_parks_cursor_at_end`.
        {
            let l = RichTextLine::new().with_text("hello");
            assert_eq!(l.value(), "hello");
            assert!(l.cursor_at_end());
            assert!(!l.cursor_at_start());
        }

        // Case `set_text_does_not_mark_dirty`.
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

        // Case `cursor_at_start_after_set_cursor_pos_zero`.
        {
            let mut l = RichTextLine::new().with_text("abc");
            l.set_cursor_pos(0);
            assert!(l.cursor_at_start());
            assert!(!l.cursor_at_end());
            l.set_cursor_pos(3);
            assert!(l.cursor_at_end());
        }

        // Case `accessibility_reports_single_line_text_input_with_value`.
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

        // Case `accessibility_focus_tracks_inner_input`.
        {
            let mut l = RichTextLine::new().with_text("x");
            Widget::set_focus(&mut l, true);
            let node = Widget::accessibility(&l).unwrap();
            assert!(node.state.focused);
        }
    }

    #[test]
    fn boundary_keys_yield_block_signals() {
        // Case `enter_key_yields_signal_with_cursor_byte`.
        {
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

        // Case `enter_at_end_yields_signal_with_full_length`.
        {
            let mut l = RichTextLine::new().with_text("hi");
            l.set_focus(true);
            dispatch(&mut l, &key_press(NamedKey::Enter));
            match l.take_signal() {
                Some(LineSignal::Enter { split_at }) => assert_eq!(split_at, 2),
                other => panic!("expected Enter@2, got {other:?}"),
            }
        }

        // Case `backspace_at_byte_zero_yields_signal`.
        {
            let mut l = RichTextLine::new().with_text("abc");
            l.set_focus(true);
            l.set_cursor_pos(0);
            dispatch(&mut l, &key_press(NamedKey::Backspace));
            assert_eq!(l.take_signal(), Some(LineSignal::BackspaceAtStart));
            assert_eq!(l.value(), "abc");
        }

        // Case `backspace_after_byte_zero_falls_through_to_inner`.
        {
            let mut l = RichTextLine::new().with_text("abc");
            l.set_focus(true);
            l.set_cursor_pos(3);
            dispatch(&mut l, &key_press(NamedKey::Backspace));
            assert!(l.take_signal().is_none());
            assert_eq!(l.value(), "ab");
            assert!(l.take_dirty().is_some());
        }

        // Case `arrow_up_down_emit_signals`.
        {
            let mut l = RichTextLine::new().with_text("x");
            l.set_focus(true);
            dispatch(&mut l, &key_press(NamedKey::ArrowUp));
            assert_eq!(l.take_signal(), Some(LineSignal::ArrowUp));
            dispatch(&mut l, &key_press(NamedKey::ArrowDown));
            assert_eq!(l.take_signal(), Some(LineSignal::ArrowDown));
        }

        // Case `typing_double_bracket_emits_link_open_signal`.
        {
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
    }

    #[test]
    fn typing_edits_only_when_focused_and_reports_one_dirty_edge() {
        // Case `typing_marks_dirty_and_appends`.
        {
            let mut l = RichTextLine::new();
            l.set_focus(true);
            dispatch(&mut l, &char_press("h"));
            dispatch(&mut l, &char_press("i"));
            assert_eq!(l.value(), "hi");
            let dirty = l.take_dirty();
            assert_eq!(dirty.as_deref(), Some("hi"));
            assert!(l.take_dirty().is_none());
        }

        // Case `unfocused_line_ignores_keys`.
        {
            let mut l = RichTextLine::new().with_text("abc");
            dispatch(&mut l, &key_press(NamedKey::Enter));
            assert!(l.take_signal().is_none(), "unfocused line must not emit");
            dispatch(&mut l, &char_press("z"));
            assert_eq!(l.value(), "abc");
        }

        // Case `user_rich_mark_edit_emits_one_dirty_edge_but_projection_and_pending_marks_do_not`.
        {
            let mut line = RichTextLine::new().with_text("text");
            let mut doc = AutoCommit::new();
            assert!(Widget::perform_action(
                line.inner_mut(),
                &crate::a11y::Action::SetSelection {
                    anchor: 0,
                    focus: 4
                },
                &mut doc,
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
                &mut doc,
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
        // Case `toggle_mark_on_selection_wraps_it`.
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

        // Case `selected_formatting_reports_one_edit_without_changing_text`.
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

        // Case `pending_mark_applies_to_next_typed_char`.
        {
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

        // Case `marks_shift_on_insert_before`.
        {
            let mut l = RichTextLine::new().with_text("hello");
            l.set_marks(vec![MarkSpan::new(2..4, Mark::Bold)]);
            l.set_focus(true);
            l.set_cursor_pos(0);
            dispatch(&mut l, &char_press("X"));
            assert_eq!(l.value(), "Xhello");
            assert_eq!(l.marks()[0].range, 3..5);
        }

        // Case `marks_shift_on_delete_before`.
        {
            let mut l = RichTextLine::new().with_text("Xhello");
            l.set_marks(vec![MarkSpan::new(3..5, Mark::Bold)]);
            l.set_focus(true);
            l.set_cursor_pos(1);
            dispatch(&mut l, &key_press(NamedKey::Backspace));
            assert_eq!(l.value(), "hello");
            assert_eq!(l.marks()[0].range, 2..4);
        }

        // Case `link_at_byte_finds_mark`.
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
        // Case `literal_code_rejects_formatting_shortcuts_and_restores_prose_behavior`.
        {
            for modifier in [ModifiersState::CONTROL, ModifiersState::SUPER] {
                for shortcut in ["b", "i", "e", "u"] {
                    let mut line = RichTextLine::new().with_text("source").with_mono();
                    line.set_focus(true);
                    line.inner_mut().set_selection(0, 6);
                    line.set_inline_formatting(false);
                    dispatch_with_mods(&mut line, &char_press(shortcut), Modifiers::from(modifier));
                    assert_eq!(line.value(), "source");
                    assert!(line.marks().is_empty());
                    assert!(line.pending_marks().is_empty());
                    assert!(line.take_dirty().is_none());

                    line.set_inline_formatting(true);
                    dispatch_with_mods(&mut line, &char_press(shortcut), Modifiers::from(modifier));
                    assert_eq!(line.marks().len(), 1, "prose shortcut {shortcut} restored");
                }
            }
        }

        // Case `literal_code_brackets_do_not_open_note_links_or_inherit_pending_marks`.
        {
            let mut line = RichTextLine::new();
            line.set_focus(true);
            dispatch_with_mods(
                &mut line,
                &char_press("b"),
                Modifiers::from(ModifiersState::CONTROL),
            );
            assert_eq!(line.pending_marks(), &[Mark::Bold]);
            line.set_inline_formatting(false);
            dispatch(&mut line, &char_press("["));
            dispatch(&mut line, &char_press("["));
            assert_eq!(line.value(), "[[");
            assert!(line.take_signal().is_none());
            assert!(line.marks().is_empty());
            line.set_inline_formatting(true);
            dispatch(&mut line, &char_press("["));
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
}
