//! Rich text shown, not edited: a sent message, a comment reply.
//!
//! [`RichTextView`] draws blocks with the same editor that writes them, read
//! only, so what someone wrote looks the same once it is sent. It is laid
//! out by the widget that draws it, at the width that widget gives it.

use qdoc::snapshot::BlockView;
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
        Self::with_editor(
            BlockEditor::new()
                .with_wrap_lines(true)
                .with_compact_paragraph_spacing(true),
        )
    }

    /// A view drawn by `editor`, configured by the caller; it is made read
    /// only.
    #[must_use]
    pub fn with_editor(editor: BlockEditor) -> Self {
        Self {
            host: Embedded::new(editor.with_read_only(true)),
        }
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
}
