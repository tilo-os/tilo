//! `Section` — an intrinsic vertical stack with reliable, explicit spacing.
//!
//! `Section` owns its spacing through explicit spacer widgets, independently
//! of the underlying layout mode, while retaining `Column` traversal,
//! focus, accessibility, actions, and document sync.

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{Column, Container, Element, Widget};

/// Default vertical rhythm for related rows, in logical pixels.
pub const DEFAULT_SECTION_GAP: f32 = 12.0;

/// An intrinsic-height vertical stack whose gap works consistently.
pub struct Section {
    inner: Element,
}

impl Section {
    /// Stack `items` using [`DEFAULT_SECTION_GAP`]. Empty and one-item
    /// sections do not create leading or trailing spacer nodes.
    pub fn new(items: Vec<Element>) -> Self {
        Self::with_gap(items, DEFAULT_SECTION_GAP)
    }

    /// Stack `items` with an explicit logical-pixel gap.
    pub fn with_gap(items: Vec<Element>, gap: f32) -> Self {
        let gap = gap.max(0.0);
        let last = items.len().saturating_sub(1);
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(0.0);
        for (index, item) in items.into_iter().enumerate() {
            column = column.push(item);
            if index != last && gap > 0.0 {
                column = column.push(Box::new(
                    Container::new().with_no_padding().with_height(gap),
                ));
            }
        }
        Self {
            inner: Box::new(column),
        }
    }
}

impl Widget for Section {
    crate::provides!(dyn crate::undo::UndoPoints);

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        self.inner.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.inner.build_layout(taffy, theme, node_map)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        self.inner.measure(known, available, text_renderer)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.inner.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.inner
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        self.inner.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        self.inner.handle_event(event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.inner.as_ref()]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(self.inner.as_ref());
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.inner.as_mut()]
    }

    fn mark_drawn(&mut self) {
        self.inner.mark_drawn();
    }

    fn get_rect(&self) -> Rect {
        self.inner.get_rect()
    }

    fn name(&self) -> &'static str {
        "Section"
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.inner.take_structure_dirty()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.inner.wants_pointer_capture()
    }
}

impl crate::undo::UndoPoints for Section {
    fn needs_commit(&self) -> bool {
        crate::undo::needs_commit(self.inner.as_ref())
    }
    fn on_commit(&mut self) {
        crate::undo::on_commit(self.inner.as_mut());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::Text;

    #[test]
    fn stack_spacing_uses_one_clamped_gap_and_no_edge_spacers() {
        // Case `stack_has_no_edge_spacers_and_one_gap_between_items`.
        {
            let empty = Section::new(Vec::new());
            assert_eq!(empty.inner.as_ref().children().len(), 0);

            let one = Section::new(vec![Box::new(Text::new("one"))]);
            assert_eq!(one.inner.as_ref().children().len(), 1);

            let three = Section::with_gap(
                vec![
                    Box::new(Text::new("one")),
                    Box::new(Text::new("two")),
                    Box::new(Text::new("three")),
                ],
                20.0,
            );
            assert_eq!(three.inner.as_ref().children().len(), 5);
        }

        // Case `negative_gap_is_clamped_and_does_not_add_spacers`.
        {
            let section = Section::with_gap(
                vec![Box::new(Text::new("one")), Box::new(Text::new("two"))],
                -4.0,
            );
            assert_eq!(section.inner.as_ref().children().len(), 2);
        }
    }
}
