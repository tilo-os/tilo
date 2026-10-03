//! Rich text shown, not edited: a sent message, a comment reply.
//!
//! [`RichTextView`] draws blocks with the same editor that writes them, read
//! only, so what someone wrote looks the same once it is sent. It is laid
//! out by the widget that draws it, at the width that widget gives it.

use tilo_doc::snapshot::BlockView;
use ui::kurbo::{Affine, Rect};
use ui::presence::PresenceCtx;
use ui::vello::Scene;
use ui::widget::{BlockContent, BlockEditor, Embedded};
use ui::{TextRenderer, Theme};

use crate::convert::content_of;

/// Blocks of rich text, drawn read-only at a given width.
pub struct RichTextView {
    host: Embedded<BlockEditor>,
}

impl Default for RichTextView {
    fn default() -> Self {
        Self::new()
    }
}

impl RichTextView {
    /// An empty view with compact paragraph spacing, for rows and bubbles.
    #[must_use]
    pub fn new() -> Self {
        Self::with_editor(crate::compact_editor())
    }

    /// A view drawn by `editor`, configured by the caller; it is made read
    /// only.
    #[must_use]
    pub fn with_editor(editor: BlockEditor) -> Self {
        Self {
            host: Embedded::new(editor.with_read_only(true)),
        }
    }

    /// The editor that draws the view, for its alignment and body size.
    pub fn editor_mut(&mut self) -> &mut BlockEditor {
        self.host.widget_mut()
    }

    /// Lay the view out in its own space and draw it through `transform`
    /// (a slide's text box, laid out at the slide's size, drawn scaled to a
    /// thumbnail or the presentation).
    pub fn set_transform(&mut self, transform: Affine) {
        self.host.set_transform(transform);
    }

    /// Paint the text in `color` rather than the theme's.
    pub fn set_text_color(&mut self, color: Option<ui::peniko::Color>) {
        self.host.set_text_color(color);
    }

    /// Show `blocks`. Returns whether anything changed.
    pub fn show(&mut self, blocks: &[BlockContent]) -> bool {
        self.host.widget_mut().present_blocks(blocks)
    }

    /// Show the paragraphs a document holds (a comment reply's blocks).
    pub fn show_views(&mut self, views: &[BlockView]) -> bool {
        let blocks: Vec<BlockContent> = views.iter().map(content_of).collect();
        self.show(&blocks)
    }

    /// Paint mentions in the colours `paint` gives each participant.
    pub fn set_mention_paint(&mut self, paint: ui::widget::marks::MentionPaint) {
        self.host.widget_mut().set_mention_paint(Some(paint));
    }

    /// What is shown.
    #[must_use]
    pub fn blocks(&self) -> Vec<BlockContent> {
        self.host
            .widget()
            .blocks()
            .iter()
            .map(ui::widget::Block::content)
            .collect()
    }

    /// Lay the view out at `rect`'s origin and width; returns its height.
    pub fn layout(&mut self, rect: Rect, theme: &Theme, text: &mut TextRenderer) -> f64 {
        self.host.layout(rect, theme, text)
    }

    /// Where the view was last laid out.
    #[must_use]
    pub fn rect(&self) -> Rect {
        self.host.rect()
    }

    /// Paint the view where it was last laid out.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.host
            .draw(scene, transform, text, theme, &PresenceCtx::empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::marks::{Mark, MarkSpan};

    #[test]
    fn a_view_shows_blocks_read_only_and_grows_with_them() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut view = RichTextView::new();
        let mut bold = BlockContent::paragraph("a", "bold words");
        bold.marks = vec![MarkSpan::new(0..4, Mark::Bold)];
        assert!(view.show(&[bold.clone()]));
        let one = view.layout(Rect::new(10.0, 20.0, 310.0, 20.0), &theme, &mut text);
        assert!(one > 0.0);
        assert!((view.rect().y0 - 20.0).abs() < f64::EPSILON);
        assert!(
            !view.show(&[bold.clone()]),
            "the same blocks change nothing"
        );
        assert!(view.show(&[bold, BlockContent::paragraph("b", "and a second line")]));
        let two = view.layout(Rect::new(10.0, 20.0, 310.0, 20.0), &theme, &mut text);
        assert!(two > one, "{two} > {one}");
        assert_eq!(view.blocks().len(), 2);
        assert!(view.host.widget().is_read_only());
    }

    #[test]
    fn a_view_in_its_own_space_is_sized_by_its_body_text_and_drawn_scaled() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut small = RichTextView::new();
        let mut large = RichTextView::new();
        large.editor_mut().set_body_size(48.0);
        large.editor_mut().set_text_align(ui::TextAlign::Center);
        for view in [&mut small, &mut large] {
            view.show(&[BlockContent::paragraph("a", "Title")]);
        }
        let box_rect = Rect::new(100.0, 50.0, 700.0, 50.0);
        let small_h = small.layout(box_rect, &theme, &mut text);
        large.set_transform(Affine::scale(0.5));
        let large_h = large.layout(box_rect, &theme, &mut text);
        assert!(large_h > small_h * 2.0, "{large_h} vs {small_h}");
        // Laid out at its own size, reported where it is drawn.
        assert!((large.rect().x0 - 50.0).abs() < 1e-9);
        assert!((large.rect().width() - 300.0).abs() < 1e-9);
        assert_eq!(large.editor_mut().text_align(), ui::TextAlign::Center);
    }
}
