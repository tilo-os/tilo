//! What a document editor drives on the text input each of its lines is. A rich text line, a
//! block of a document and a cell of its table are each a [`TextInput`] the editor lays out,
//! measures, moves the caret through and paints in parts of its own; this is that contract, kept
//! apart from what an app does with a field ([`TextInput`]'s own methods).

use automerge::AutoCommit;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::richtext::RichSpan;
use crate::text_render::{TextRenderer, TextStyle};
use crate::text_selection::TextPosition;
use crate::theme::Theme;
use crate::time::Instant;

use super::TextInput;
pub use super::text_input::{SingleLineAttributes, TextAccessibilityState};

/// The horizontal padding a multiline field keeps between its edge and its text.
pub const TEXT_PAD_X: f64 = super::text_input::ML_PAD_X;

/// A text input as one line of a document.
pub trait DocumentLine {
    /// Keep the document's text inset (a block, a table cell), or drop it (a line shown on its
    /// own).
    fn set_document_inset(&mut self, kept: bool);
    /// Lay out as one compact multiline paragraph, or as a document block.
    fn set_compact_multiline(&mut self, compact: bool);
    /// The text size the line's role gives it, before the person's text scale.
    fn set_base_size(&mut self, size: f32);
    /// The text scale the next layout is measured at.
    fn capture_layout_text_scale(&mut self, text_scale: f32);
    /// The formatting of the line's spans (bold, links, mentions); `None` for plain text. Whether it
    /// changed what is drawn.
    fn set_rich_spans(&mut self, spans: Option<Vec<RichSpan>>) -> bool;
    /// The spans and decorations of a single-line field.
    fn set_single_line_attributes(&mut self, attributes: SingleLineAttributes);
    /// Where a chip popover beside the line may open.
    fn set_chip_bounds(&mut self, bounds: Option<Rect>);
    /// Take the bound text from `state`, where the document keeps it.
    ///
    /// # Errors
    /// When the bound text cannot be read from `state`.
    fn adopt_bound_text(&mut self, state: &AutoCommit) -> Result<(), crate::view::ThawError>;

    /// The height of the wrapped paragraph and its padding at `width`, as painting lays it out.
    fn measure_wrapped_content(&mut self, width: f32, tr: &mut TextRenderer, theme: &Theme) -> f32;
    /// Lay the wrapped paragraph out for the current width, before it is measured or hit.
    fn prepare_wrapped_layout(&mut self, tr: &mut TextRenderer, theme: &Theme);
    /// Adopt the paragraph just measured for hit-testing and carets, as painting would: what a
    /// line left unpainted off screen does to keep its geometry current.
    fn adopt_measured_layout(&mut self);
    /// The editor took the line's last measurement.
    fn acknowledge_layout_measure(&mut self);
    /// The top and bottom of the line's first visual row.
    fn first_row_band(&self) -> (f64, f64);
    /// The style its text is set in at `scale`.
    fn line_style(&self, scale: f32) -> TextStyle;

    /// The x the caret keeps while moving up and down rows.
    fn vertical_goal_x(&self) -> f64;
    /// Keep `x` as the caret's x while moving up and down rows.
    fn set_vertical_goal_x(&mut self, x: f64);
    /// Whether the caret has a visual row below (`down`) or above it in this line.
    fn has_visual_row(&self, down: bool) -> bool;
    /// Move the caret one visual row down or up within this line.
    fn move_visual_row(&mut self, down: bool);
    /// Select from `anchor` to `cursor`.
    fn set_selection_at(&mut self, anchor: usize, cursor: TextPosition);
    /// Delete the selection here, without the clipboard.
    fn delete_local_selection(&mut self);
    /// Move the pointer to `point` without pressing: what an editor routing input to one line at
    /// a time hands a line before a press. Whether the hover changed.
    fn update_pointer_position(&mut self, point: Point) -> bool;
    /// Settle the spelling suggestion shown at `now`. Whether it changed.
    fn settle_suggestion_at(&mut self, now: Instant) -> bool;

    /// Whether the line changed what it paints since it was last painted.
    fn paint_changed(&self) -> bool;
    /// Whether the line paints its caret into the shaped text.
    fn paints_shaped_caret(&self) -> bool;
    /// Paint the caret the last draw left out, so the editor keeps it in a damage part of its
    /// own. Where it painted; `None` when no caret was shown.
    fn draw_caret_apart(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
    ) -> Option<Rect>;
    /// What assistive technology reads of the line.
    fn accessibility_state(&self) -> TextAccessibilityState;
}

impl DocumentLine for TextInput {
    fn set_document_inset(&mut self, kept: bool) {
        if kept {
            self.keep_document_inset();
        } else {
            self.drop_document_inset();
        }
    }

    fn set_compact_multiline(&mut self, compact: bool) {
        TextInput::set_compact_multiline(self, compact);
    }

    fn set_base_size(&mut self, size: f32) {
        TextInput::set_base_size(self, size);
    }

    fn capture_layout_text_scale(&mut self, text_scale: f32) {
        TextInput::capture_layout_text_scale(self, text_scale);
    }

    fn set_rich_spans(&mut self, spans: Option<Vec<RichSpan>>) -> bool {
        TextInput::set_rich_spans(self, spans)
    }

    fn set_single_line_attributes(&mut self, attributes: SingleLineAttributes) {
        TextInput::set_single_line_attributes(self, attributes);
    }

    fn set_chip_bounds(&mut self, bounds: Option<Rect>) {
        self.chip_room = bounds;
    }

    fn adopt_bound_text(&mut self, state: &AutoCommit) -> Result<(), crate::view::ThawError> {
        TextInput::adopt_bound_text(self, state)
    }

    fn measure_wrapped_content(&mut self, width: f32, tr: &mut TextRenderer, theme: &Theme) -> f32 {
        TextInput::measure_wrapped_content(self, width, tr, theme)
    }

    fn prepare_wrapped_layout(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        TextInput::prepare_wrapped_layout(self, tr, theme);
    }

    fn adopt_measured_layout(&mut self) {
        TextInput::adopt_measured_layout(self);
    }

    fn acknowledge_layout_measure(&mut self) {
        TextInput::acknowledge_layout_measure(self);
    }

    fn first_row_band(&self) -> (f64, f64) {
        TextInput::first_row_band(self)
    }

    fn line_style(&self, scale: f32) -> TextStyle {
        TextInput::line_style(self, scale)
    }

    fn vertical_goal_x(&self) -> f64 {
        TextInput::vertical_goal_x(self)
    }

    fn set_vertical_goal_x(&mut self, x: f64) {
        TextInput::set_vertical_goal_x(self, x);
    }

    fn has_visual_row(&self, down: bool) -> bool {
        TextInput::has_visual_row(self, down)
    }

    fn move_visual_row(&mut self, down: bool) {
        TextInput::move_visual_row(self, down);
    }

    fn set_selection_at(&mut self, anchor: usize, cursor: TextPosition) {
        TextInput::set_selection_at(self, anchor, cursor);
    }

    fn delete_local_selection(&mut self) {
        TextInput::delete_local_selection(self);
    }

    fn update_pointer_position(&mut self, point: Point) -> bool {
        TextInput::update_pointer_position(self, point)
    }

    fn settle_suggestion_at(&mut self, now: Instant) -> bool {
        TextInput::settle_suggestion_at(self, now)
    }

    fn paint_changed(&self) -> bool {
        TextInput::paint_changed(self)
    }

    fn paints_shaped_caret(&self) -> bool {
        TextInput::paints_shaped_caret(self)
    }

    fn draw_caret_apart(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
    ) -> Option<Rect> {
        TextInput::draw_caret_apart(self, scene, transform, theme)
    }

    fn accessibility_state(&self) -> TextAccessibilityState {
        TextInput::accessibility_state(self)
    }
}
