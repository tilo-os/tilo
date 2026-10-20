//! The element tree a widget rebuilds from its own state at each layout.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use super::{AppModel, Element, EventCtx, ModelPath, Widget, layout_rect};
use crate::i18n::I18nManager;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

/// A widget's composition: the element tree it rebuilds at every layout, or
/// keeps across layouts until it is invalidated ([`Composed::build_layout_kept`]), and
/// lays out as the only child of its own node, plus the box it was given and
/// whether the owner asked for a rebuild. The owning widget forwards its tree
/// methods here.
#[derive(Default)]
pub struct Composed {
    rect: Rect,
    element: Option<Element>,
    invalidated: bool,
    /// The owner asked for a fresh composition since the last was built.
    /// Unlike `invalidated`, reading the structure flag leaves it set.
    stale: bool,
    /// The language the composition was built in ([`I18nManager::revision`]).
    language: u64,
}

impl Composed {
    /// Lay out a freshly built `element`, replacing the previous
    /// composition. Returns its node, the only child of the owner's.
    pub fn build_layout(
        &mut self,
        element: Element,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.invalidated = false;
        self.stale = false;
        self.language = I18nManager::revision();
        self.element
            .insert(element)
            .build_layout(taffy, theme, node_map)
    }

    /// Lay out the composition kept from the last layout, or what `build`
    /// makes where there is none, the owner invalidated it or the language
    /// changed since. A relayout (a resize, a theme, a parent's reflow) then
    /// keeps the controls, and the hover, press and focus they hold; the
    /// owner invalidates when what it builds the composition from changes.
    pub fn build_layout_kept(
        &mut self,
        build: impl FnOnce() -> Element,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let element = match self.element.take() {
            Some(element) if !self.stale && self.language == I18nManager::revision() => element,
            _ => build(),
        };
        self.build_layout(element, taffy, theme, node_map)
    }

    /// Swap in `element` without laying it out.
    pub fn replace(&mut self, element: Element) -> &mut (dyn Widget + 'static) {
        self.invalidated = false;
        self.stale = false;
        self.language = I18nManager::revision();
        self.element.insert(element).as_mut()
    }

    /// Take the composition out, for an owner that keeps it across layouts
    /// and lays it out again with [`Self::build_layout`].
    pub fn take(&mut self) -> Option<Element> {
        self.element.take()
    }

    /// Rebuild the composition at the next layout: the owner's state that it
    /// is built from changed. A fresh composition settles it.
    pub fn invalidate(&mut self) {
        self.invalidated = true;
        self.stale = true;
    }

    /// Drop the composition until the next layout builds a fresh one.
    pub fn clear(&mut self) {
        self.element = None;
    }

    /// Store the owner's box and place the composition inside it.
    pub fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = layout_rect(taffy, node, origin);
        if let Some(element) = &mut self.element
            && let Ok(child) = taffy.child_at_index(node, 0)
        {
            element.apply_layout(taffy, child, self.rect.origin());
        }
    }

    /// The owner's laid-out box.
    #[must_use]
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// The current composition, once built.
    #[must_use]
    pub fn element(&self) -> Option<&(dyn Widget + 'static)> {
        self.element.as_deref()
    }

    /// The current composition, once built.
    pub fn element_mut(&mut self) -> Option<&mut (dyn Widget + 'static)> {
        self.element.as_deref_mut()
    }

    /// Sync the composition; one built in another language is rebuilt at
    /// the next layout.
    pub fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if self.element.is_some() && self.language != I18nManager::revision() {
            self.invalidate();
        }
        if let Some(element) = &mut self.element {
            element.sync(state, path);
        }
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(element) = &mut self.element {
            element.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    pub fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let Some(element) = &mut self.element
            && let Ok(child) = ctx.taffy.child_at_index(ctx.node_id, 0)
        {
            element.handle_event(event, &mut ctx.reborrow(ctx.path, child));
        }
    }

    #[must_use]
    pub fn children(&self) -> Vec<&dyn Widget> {
        self.element
            .as_deref()
            .map(|element| element as &dyn Widget)
            .into_iter()
            .collect()
    }

    pub fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.element
            .as_deref_mut()
            .map(|element| element as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    /// Whether the owner invalidated the composition or the composition
    /// asked to be rebuilt. Reading clears both.
    pub fn take_structure_dirty(&mut self) -> bool {
        let invalidated = std::mem::take(&mut self.invalidated);
        let child = self
            .element
            .as_mut()
            .is_some_and(|element| element.take_structure_dirty());
        invalidated || child
    }
}

/// The [`Widget`] methods a view owning a [`Composed`] in `self.$field`
/// forwards to it unchanged: layout placement, painting, events, child
/// walks, its box and whether it must be rebuilt.
#[macro_export]
macro_rules! forward_to_composed {
    ($field:ident) => {
        fn apply_layout(
            &mut self,
            taffy: &$crate::taffy::TaffyTree<()>,
            node: $crate::taffy::NodeId,
            origin: $crate::kurbo::Point,
        ) {
            self.$field.apply_layout(taffy, node, origin);
        }

        fn draw(
            &mut self,
            scene: &mut $crate::vello::Scene,
            transform: $crate::kurbo::Affine,
            text_renderer: &mut $crate::text_render::TextRenderer,
            theme: &$crate::theme::Theme,
            presence: &$crate::presence::PresenceCtx,
        ) {
            self.$field
                .draw(scene, transform, text_renderer, theme, presence);
        }

        fn handle_event(
            &mut self,
            event: &$crate::winit::event::WindowEvent,
            ctx: &mut $crate::widget::EventCtx<'_>,
        ) {
            self.$field.handle_event(event, ctx);
        }

        fn children(&self) -> Vec<&dyn $crate::widget::Widget> {
            self.$field.children()
        }

        fn children_mut(&mut self) -> Vec<&mut (dyn $crate::widget::Widget + '_)> {
            self.$field.children_mut()
        }

        fn get_rect(&self) -> $crate::kurbo::Rect {
            self.$field.rect()
        }

        fn take_structure_dirty(&mut self) -> bool {
            self.$field.take_structure_dirty()
        }
    };
}

/// The [`Widget`] methods a wrapper that keeps one child [`Element`] in
/// `self.$child` — laid out as the only child of its own node, whose box it
/// stores in `self.$rect` — forwards to it unchanged: placement, painting,
/// events, child walks and its box.
#[macro_export]
macro_rules! forward_to_child {
    ($child:ident, $rect:ident) => {
        fn apply_layout(
            &mut self,
            taffy: &$crate::taffy::TaffyTree<()>,
            node: $crate::taffy::NodeId,
            origin: $crate::kurbo::Point,
        ) {
            self.$rect = $crate::widget::layout_rect(taffy, node, origin);
            if let Ok(child) = taffy.child_at_index(node, 0) {
                self.$child.apply_layout(taffy, child, self.$rect.origin());
            }
        }

        fn draw(
            &mut self,
            scene: &mut $crate::vello::Scene,
            transform: $crate::kurbo::Affine,
            text_renderer: &mut $crate::text_render::TextRenderer,
            theme: &$crate::theme::Theme,
            presence: &$crate::presence::PresenceCtx,
        ) {
            self.$child
                .draw(scene, transform, text_renderer, theme, presence);
        }

        fn handle_event(
            &mut self,
            event: &$crate::winit::event::WindowEvent,
            ctx: &mut $crate::widget::EventCtx<'_>,
        ) {
            if let Ok(child) = ctx.taffy.child_at_index(ctx.node_id, 0) {
                self.$child
                    .handle_event(event, &mut ctx.reborrow(ctx.path, child));
            }
        }

        fn children(&self) -> Vec<&dyn $crate::widget::Widget> {
            vec![self.$child.as_ref()]
        }

        fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn $crate::widget::Widget)) {
            visitor(self.$child.as_ref());
        }

        fn children_mut(&mut self) -> Vec<&mut (dyn $crate::widget::Widget + '_)> {
            vec![self.$child.as_mut()]
        }

        fn get_rect(&self) -> $crate::kurbo::Rect {
            self.$rect
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::Spacer;

    #[test]
    fn invalidation_is_read_once_and_settled_by_a_fresh_composition() {
        let mut composed = Composed::default();
        assert!(!composed.take_structure_dirty());
        composed.invalidate();
        assert!(composed.take_structure_dirty());
        assert!(!composed.take_structure_dirty(), "reading clears it");

        composed.invalidate();
        composed.replace(Box::new(Spacer::new()));
        assert!(
            !composed.take_structure_dirty(),
            "the rebuild it asked for happened"
        );
    }
}
