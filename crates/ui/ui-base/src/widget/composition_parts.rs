//! Shared retained-child lifecycle only; each composition owns its policy.
use super::{Element, TaffyTreeExt, Widget};
use crate::theme::Theme;
use crate::widget::DrawChild;
use std::collections::HashMap;
use taffy::{NodeId, Style, TaffyTree};
use vello::kurbo::{Point, Rect};

#[derive(Default)]
pub struct Parts {
    pub children: Vec<Element>,
    pub nodes: Vec<NodeId>,
    pub rect: Rect,
}

impl Parts {
    pub fn new(children: Vec<Element>) -> Self {
        Self {
            children,
            nodes: Vec::new(),
            rect: Rect::ZERO,
        }
    }

    pub fn build(
        &mut self,
        style: Style,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.nodes = self
            .children
            .iter_mut()
            .map(|child| child.build_layout(taffy, theme, map))
            .collect();
        taffy.parent_node(style, &self.nodes)
    }

    pub fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        let start = self.rect.origin();
        for (child, &node) in self.children.iter_mut().zip(&self.nodes) {
            child.apply_layout(taffy, node, start);
        }
    }

    pub fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        for child in &mut self.children {
            child.draw_child(scene, transform, text, theme, &env);
        }
    }
}

#[doc(hidden)]
#[macro_export]
macro_rules! retained_parts {
    ($name:literal) => {
        $crate::retained_parts!(lifecycle: parts);
        fn name(&self) -> &'static str {
            $name
        }
    };
    ($name:literal, custom_apply_layout) => {
        $crate::retained_parts!(children: parts);
        fn name(&self) -> &'static str {
            $name
        }
    };
    (lifecycle: $($path:ident).+) => {
        fn apply_layout(&mut self, taffy: &$crate::taffy::TaffyTree<()>, node: $crate::taffy::NodeId, origin: $crate::kurbo::Point) {
            self.$($path).+.apply_layout(taffy, node, origin);
        }
        $crate::retained_parts!(children: $($path).+);
    };
    (children: $($path:ident).+) => {
        fn update(
            &mut self,
            state: &$crate::widget::AppModel,
            path: &$crate::widget::ModelPath,
        ) -> bool {
            let mut changed = false;
            for child in &mut self.$($path).+.children {
                changed |= child.update(state, path);
            }
            changed
        }
        fn sync(&mut self, state: &$crate::widget::AppModel, path: &$crate::widget::ModelPath) {
            for child in &mut self.$($path).+.children {
                child.sync(state, path);
            }
        }
        fn handle_event(
            &mut self,
            event: &$crate::winit::event::WindowEvent,
            ctx: &mut $crate::widget::EventCtx<'_>,
        ) {
            let parts = &mut self.$($path).+;
            for (child, &node) in parts.children.iter_mut().zip(&parts.nodes) {
                let path = ctx.path;
                child.handle_event(event, &mut ctx.reborrow(path, node));
                if *ctx.handled {
                    break;
                }
            }
        }
        fn children(&self) -> Vec<&dyn $crate::widget::Widget> {
            self.$($path).+
                .children
                .iter()
                .map(|child| child.as_ref())
                .collect()
        }
        fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn $crate::widget::Widget)) {
            for child in &self.$($path).+.children {
                visitor(child.as_ref());
            }
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn $crate::widget::Widget +  '_)> {
            self.$($path).+
                .children
                .iter_mut()
                .map(|child| child.as_mut() as &mut (dyn $crate::widget::Widget +  '_))
                .collect()
        }
        fn mark_drawn(&mut self) {
            for child in &mut self.$($path).+.children {
                child.mark_drawn();
            }
        }
        // A pure layout container: what changed is what its parts changed,
        // not the whole box, so a hover or a caret elsewhere on the page
        // does not repaint the page.
        fn collect_damage(&self, regions: &mut Vec<$crate::kurbo::Rect>) {
            for child in &self.$($path).+.children {
                child.collect_damage(regions);
            }
        }
        fn take_structure_dirty(&mut self) -> bool {
            let mut dirty = false;
            for child in &mut self.$($path).+.children {
                dirty |= child.take_structure_dirty();
            }
            dirty
        }
        fn take_structure_change(&mut self) -> $crate::widget::StructureChange {
            let mut change = $crate::widget::StructureChange::None;
            for child in &mut self.$($path).+.children {
                change = change.join(child.take_structure_change());
            }
            change
        }
        fn wants_pointer_capture(&self) -> bool {
            self.$($path).+
                .children
                .iter()
                .any(|child| child.wants_pointer_capture())
        }
        fn get_rect(&self) -> $crate::kurbo::Rect {
            self.$($path).+.rect
        }
    };
}

