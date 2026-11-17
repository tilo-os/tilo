//! A caret on a surface (canvas.md §1): a field with no well and no fill, only as wide
//! as what it holds.

use super::TextInput;
use crate::text_render::TextRenderer;

impl TextInput {
    /// A caret on a surface rather than a field in a form: it draws no well and no
    /// fill — the caret and the words are all there is of it — and it is only as wide as what
    /// it holds, growing as the words are typed (the prompt's width while it holds none). The
    /// canvas's caret, where the person pointed (canvas.md §1).
    #[must_use]
    pub fn as_caret(mut self) -> Self {
        self.set_caret(true);
        self
    }

    /// See [`Self::as_caret`].
    pub fn set_caret(&mut self, caret: bool) {
        if self.caret != caret {
            self.caret = caret;
            self.caret_measured = None;
            self.repaint = true;
        }
        self.plain = caret || self.plain;
    }

    /// What a caret field is as wide as: its words, else its prompt.
    pub(super) fn caret_shown(&self) -> &str {
        if self.value().is_empty() {
            &self.placeholder
        } else {
            self.value()
        }
    }

    /// How wide a caret lays out: as wide as its words (or its prompt), and the caret's own width
    /// past them, never wider than the room it stands in; its height is the band the field's
    /// layout set.
    pub(super) fn measure_caret(
        &mut self,
        text_renderer: &mut TextRenderer,
        known: Option<f32>,
        available: taffy::AvailableSpace,
    ) -> taffy::Size<f32> {
        let shown = self.caret_shown().to_owned();
        let scale = self.scale_for(text_renderer.text_scale());
        let style = self.line_style(scale);
        let (words, _) = text_renderer.measure_style(&shown, None, &style);
        // The room past the words is the caret's own width, as the theme draws it.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a caret's width is a few logical pixels"
        )]
        let room = self.last_caret_width as f32;
        let width = (words + room).ceil();
        self.caret_measured = Some(shown);
        let width = match available {
            taffy::AvailableSpace::Definite(room) => width.min(room),
            _ => width,
        };
        taffy::Size {
            width: known.unwrap_or(width),
            height: 0.0,
        }
    }
}
