//! Shared retained-child lifecycle only; each composition owns its policy.
use super::{Element, TaffyTreeExt, Widget};
use crate::{presence::PresenceCtx, text_render::TextRenderer, theme::Theme};
use std::collections::HashMap;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

#[derive(Default)]
pub(super) struct Parts {
    pub(super) children: Vec<Element>,
    pub(super) nodes: Vec<NodeId>,
    pub(super) rect: Rect,
}

impl Parts {
    pub(super) fn new(children: Vec<Element>) -> Self {
        Self {
            children,
            nodes: Vec::new(),
            rect: Rect::ZERO,
        }
    }

    pub(super) fn build(
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

    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for child in &mut self.children {
            child.draw(scene, transform, text, theme, presence);
        }
    }
}

macro_rules! retained_parts {
    ($name:literal) => {
        fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
            let mut changed = false;
            for child in &mut self.parts.children {
                changed |= child.update(state, path);
            }
            changed
        }
        fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
            for child in &mut self.parts.children {
                child.sync(state, path);
            }
        }
        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let layout = taffy.layout(node).copied().unwrap_or_default();
            let start = Point::new(
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
            );
            self.parts.rect = Rect::from_origin_size(
                start,
                (f64::from(layout.size.width), f64::from(layout.size.height)),
            );
            for (child, &node) in self.parts.children.iter_mut().zip(&self.parts.nodes) {
                child.apply_layout(taffy, node, start);
            }
        }
        fn handle_event(
            &mut self,
            event: &winit::event::WindowEvent,
            scale: f64,
            modifiers: &winit::event::Modifiers,
            state: &mut AutoCommit,
            path: &ObjId,
            handled: &mut bool,
            focus: &mut Option<NodeId>,
            _node: NodeId,
            taffy: &TaffyTree<()>,
        ) {
            for (child, &node) in self.parts.children.iter_mut().zip(&self.parts.nodes) {
                child.handle_event(
                    event, scale, modifiers, state, path, handled, focus, node, taffy,
                );
                if *handled {
                    break;
                }
            }
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.parts
                .children
                .iter()
                .map(|child| child.as_ref())
                .collect()
        }
        fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
            for child in &self.parts.children {
                visitor(child.as_ref());
            }
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.parts
                .children
                .iter_mut()
                .map(|child| child.as_mut() as &mut (dyn Widget + '_))
                .collect()
        }
        fn mark_drawn(&mut self) {
            for child in &mut self.parts.children {
                child.mark_drawn();
            }
        }
        fn needs_commit(&self) -> bool {
            self.parts.children.iter().any(|child| child.needs_commit())
        }
        fn on_commit(&mut self) {
            for child in &mut self.parts.children {
                child.on_commit();
            }
        }
        fn take_structure_dirty(&mut self) -> bool {
            let mut dirty = false;
            for child in &mut self.parts.children {
                dirty |= child.take_structure_dirty();
            }
            dirty
        }
        fn wants_pointer_capture(&self) -> bool {
            self.parts
                .children
                .iter()
                .any(|child| child.wants_pointer_capture())
        }
        fn get_rect(&self) -> Rect {
            self.parts.rect
        }
        fn name(&self) -> &'static str {
            $name
        }
    };
}

pub(super) use retained_parts;
