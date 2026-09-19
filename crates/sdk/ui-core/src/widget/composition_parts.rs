//! Shared retained-child lifecycle only; each composition owns its policy.
use super::{Element, TaffyTreeExt, Widget};
use crate::{presence::PresenceCtx, text_render::TextRenderer, theme::Theme};
use std::collections::HashMap;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

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

    pub(super) fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let start = Point::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
        );
        self.rect = Rect::from_origin_size(
            start,
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
        for (child, &node) in self.children.iter_mut().zip(&self.nodes) {
            child.apply_layout(taffy, node, start);
        }
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
        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            self.parts.apply_layout(taffy, node, origin);
        }
        retained_parts!($name, custom_apply_layout);
    };
    ($name:literal, custom_apply_layout) => {
        fn start_resource_operation_with_document(
            &mut self,
            invocation: tilo_actions::InvocationId,
            kind: tilo_actions::OperationKind,
            type_id: &str,
            payload: &[u8],
            doc: &mut AutoCommit,
            path: &ObjId,
        ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
            for child in &mut self.parts.children {
                if let Some(started) = child.start_resource_operation_with_document(
                    invocation, kind, type_id, payload, doc, path,
                )? {
                    return Ok(Some(started));
                }
            }
            // A second read-only fallback would visit declining handlers twice.
            Ok(None)
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{FormSection, Manual, ResourceOperationStart};
    use automerge::{AutoCommit, ObjId, ReadDoc, transaction::Transactable};
    use std::{cell::RefCell, rc::Rc};
    use tilo_actions::{InvocationId, OperationKind};
    use vello::kurbo::Point;

    struct MutableAdmission {
        calls: Rc<RefCell<Vec<(InvocationId, bool)>>>,
        accepts: bool,
    }
    impl Widget for MutableAdmission {
        fn name(&self) -> &str {
            "MutableAdmission"
        }
        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }
        fn sync(&mut self, _: &mut AutoCommit, _: &ObjId) {}
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            _: &Theme,
            _: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            tree.leaf_node(Style::default())
        }
        fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: Point) {}
        fn draw(
            &mut self,
            _: &mut Scene,
            _: Affine,
            _: &mut TextRenderer,
            _: &Theme,
            _: &PresenceCtx,
        ) {
        }
        fn handle_event(
            &mut self,
            _: &winit::event::WindowEvent,
            _: f64,
            _: &winit::event::Modifiers,
            _: &mut AutoCommit,
            _: &ObjId,
            _: &mut bool,
            _: &mut Option<NodeId>,
            _: NodeId,
            _: &TaffyTree<()>,
        ) {
        }
        fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
            &["mutable.effect"]
        }
        fn start_resource_operation(
            &mut self,
            invocation: InvocationId,
            _: OperationKind,
            _: &str,
            _: &[u8],
            _: &AutoCommit,
            _: &ObjId,
        ) -> Result<Option<ResourceOperationStart>, String> {
            self.calls.borrow_mut().push((invocation, false));
            Ok(None)
        }
        fn start_resource_operation_with_document(
            &mut self,
            invocation: InvocationId,
            kind: OperationKind,
            type_id: &str,
            payload: &[u8],
            doc: &mut AutoCommit,
            path: &ObjId,
        ) -> Result<Option<ResourceOperationStart>, String> {
            assert_eq!(kind, OperationKind::Effect);
            assert_eq!(type_id, "mutable.effect");
            assert_eq!(payload, b"payload");
            self.calls.borrow_mut().push((invocation, true));
            if self.accepts {
                doc.put(path, "admitted", "mutable follow-up").unwrap();
                Ok(Some(ResourceOperationStart::Accepted))
            } else {
                Ok(None)
            }
        }
    }

    fn check_mutable_admission(nested: bool) {
        for accepts in [true, false] {
            let calls = Rc::new(RefCell::new(Vec::new()));
            let child = Box::new(MutableAdmission {
                calls: calls.clone(),
                accepts,
            });
            let manual = Manual::new(|ctx| {
                Ok(taffy::Size {
                    width: ctx.available().width,
                    height: 40.0,
                })
            })
            .child(child);
            let mut root: Element = if nested {
                Box::new(FormSection::new("Nested").content(Box::new(manual)))
            } else {
                Box::new(manual)
            };
            let mut doc = AutoCommit::new();
            let path = doc
                .put_object(automerge::ROOT, "document", automerge::ObjType::Map)
                .unwrap();
            let invocation = InvocationId::new();
            let result = root
                .start_resource_operation_with_document(
                    invocation,
                    OperationKind::Effect,
                    "mutable.effect",
                    b"payload",
                    &mut doc,
                    &path,
                )
                .unwrap();
            assert_eq!(
                &*calls.borrow(),
                &[(invocation, true)],
                "mutable admission must be visited exactly once without read-only fallback"
            );
            assert_eq!(
                matches!(result, Some(ResourceOperationStart::Accepted)),
                accepts
            );
            assert_eq!(doc.get(&path, "admitted").unwrap().is_some(), accepts);
        }
    }
    #[test]
    fn manual_forwards_mutable_admission_exactly_once() {
        check_mutable_admission(false);
    }
    #[test]
    fn nested_composition_forwards_mutable_admission_exactly_once() {
        check_mutable_admission(true);
    }
}
