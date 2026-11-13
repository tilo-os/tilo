//! The element tree a widget rebuilds from its own state at each layout.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use super::DrawChild;
use super::{AppModel, Element, EventCtx, ModelPath, Widget, layout_rect};
use crate::i18n::I18nManager;
use crate::interaction::InteractionEnvironment;
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
    stale: bool,
    language: u64,
    /// The host's input capabilities, as last told: each composition built
    /// afresh starts with them, not with a mouse's defaults.
    environment: Option<InteractionEnvironment>,
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
        let element = self.element.insert(element);
        if let Some(environment) = self.environment {
            element.set_interaction_environment(environment);
        }
        element.build_layout(taffy, theme, node_map)
    }

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

    pub fn replace(&mut self, element: Element) -> &mut (dyn Widget + 'static) {
        self.invalidated = false;
        self.stale = false;
        self.language = I18nManager::revision();
        let element = self.element.insert(element).as_mut();
        if let Some(environment) = self.environment {
            element.set_interaction_environment(environment);
        }
        element
    }

    /// The host's input capabilities changed: the composition takes them,
    /// and so does every one built after it. An owner forwards
    /// [`Widget::set_interaction_environment`] here.
    pub fn set_interaction_environment(&mut self, environment: InteractionEnvironment) {
        self.environment = Some(environment);
        if let Some(element) = &mut self.element {
            element.set_interaction_environment(environment);
        }
    }

    pub fn take(&mut self) -> Option<Element> {
        self.element.take()
    }

    pub fn invalidate(&mut self) {
        self.invalidated = true;
        self.stale = true;
    }

    /// Drop the composition until the next layout builds a fresh one.
    pub fn clear(&mut self) {
        self.element = None;
    }

    pub fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = layout_rect(taffy, node, origin);
        if let Some(element) = &mut self.element
            && let Ok(child) = taffy.child_at_index(node, 0)
        {
            element.apply_layout(taffy, child, self.rect.origin());
        }
    }

    #[must_use]
    pub fn rect(&self) -> Rect {
        self.rect
    }

    #[must_use]
    pub fn element(&self) -> Option<&(dyn Widget + 'static)> {
        self.element.as_deref()
    }

    pub fn element_mut(&mut self) -> Option<&mut (dyn Widget + 'static)> {
        self.element.as_deref_mut()
    }

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
            element.draw_child(scene, transform, text_renderer, theme, presence);
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
            $crate::widget::DrawChild::draw_child(
                &mut self.$child,
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
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

    struct Probe(std::rc::Rc<std::cell::Cell<Option<InteractionEnvironment>>>);

    impl Widget for Probe {
        fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            _node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.new_leaf(taffy::Style::default()).unwrap()
        }
        fn name(&self) -> &'static str {
            "Probe"
        }
        fn children(&self) -> Vec<&dyn Widget> {
            Vec::new()
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            Vec::new()
        }
        fn set_interaction_environment(&mut self, environment: InteractionEnvironment) {
            self.0.set(Some(environment));
        }
    }

    #[test]
    fn a_composition_built_afresh_keeps_the_input_in_use() {
        let mut composed = Composed::default();
        let first = std::rc::Rc::default();
        composed.replace(Box::new(Probe(std::rc::Rc::clone(&first))));
        assert_eq!(first.get(), None, "nothing told yet, nothing passed on");
        composed.set_interaction_environment(InteractionEnvironment::TOUCH);
        assert_eq!(first.get(), Some(InteractionEnvironment::TOUCH));

        let rebuilt = std::rc::Rc::default();
        composed.replace(Box::new(Probe(std::rc::Rc::clone(&rebuilt))));
        assert_eq!(rebuilt.get(), Some(InteractionEnvironment::TOUCH));

        let laid_out = std::rc::Rc::default();
        let mut taffy = TaffyTree::new();
        composed.build_layout(
            Box::new(Probe(std::rc::Rc::clone(&laid_out))),
            &mut taffy,
            &Theme::light(),
            &mut HashMap::new(),
        );
        assert_eq!(laid_out.get(), Some(InteractionEnvironment::TOUCH));
    }
}
