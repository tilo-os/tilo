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
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        let start = self.rect.origin();
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
        fn start_operation(
            &mut self,
            op: &crate::widget::ResourceOperation<'_>,
            doc: &mut crate::widget::Edits<'_>,
            path: &ObjId,
        ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
            crate::widget::start_in_children(self, op, doc, path)
        }
        fn update(
            &mut self,
            state: &crate::widget::AppModel,
            path: &crate::widget::ModelPath,
        ) -> bool {
            let mut changed = false;
            for child in &mut self.parts.children {
                changed |= child.update(state, path);
            }
            changed
        }
        fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
            for child in &mut self.parts.children {
                child.sync(state, path);
            }
        }
        fn handle_event(
            &mut self,
            event: &winit::event::WindowEvent,
            ctx: &mut crate::widget::EventCtx<'_>,
        ) {
            for (child, &node) in self.parts.children.iter_mut().zip(&self.parts.nodes) {
                let path = ctx.path;
                child.handle_event(event, &mut ctx.reborrow(path, node));
                if *ctx.handled {
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
        // A pure layout container: what changed is what its parts changed,
        // not the whole box, so a hover or a caret elsewhere on the page
        // does not repaint the page.
        fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
            for child in &self.parts.children {
                child.collect_damage(regions);
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
    use crate::widget::{FormSection, Manual, ResourceOperation, ResourceOperationStart};
    use automerge::{AutoCommit, ObjId, ReadDoc, transaction::Transactable};
    use std::{cell::RefCell, rc::Rc};
    use tilo_actions::{InvocationId, OperationKind};
    use vello::kurbo::Point;

    struct MutableAdmission {
        calls: Rc<RefCell<Vec<InvocationId>>>,
        accepts: bool,
    }
    impl Widget for MutableAdmission {
        fn name(&self) -> &str {
            "MutableAdmission"
        }
        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }
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
        fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
            &["mutable.effect"]
        }
        fn start_operation(
            &mut self,
            op: &ResourceOperation<'_>,
            doc: &mut crate::widget::Edits<'_>,
            path: &ObjId,
        ) -> Result<Option<ResourceOperationStart>, String> {
            let request = op.unscoped().expect("a local operation");
            assert_eq!(request.kind, OperationKind::Effect);
            assert_eq!(request.type_id, "mutable.effect");
            assert_eq!(request.payload, b"payload");
            self.calls.borrow_mut().push(request.invocation);
            if self.accepts {
                doc.document_mut()
                    .put(path, "admitted", "mutable follow-up")
                    .unwrap();
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
            let result = ResourceOperation::local(
                invocation,
                OperationKind::Effect,
                "mutable.effect",
                b"payload",
            )
            .start(
                root.as_mut(),
                &mut crate::widget::Edits::lend(&mut doc),
                &path,
            )
            .unwrap();
            assert_eq!(
                &*calls.borrow(),
                &[invocation],
                "the handler must be offered the operation exactly once"
            );
            assert_eq!(
                matches!(result, Some(ResourceOperationStart::Accepted)),
                accepts
            );
            assert_eq!(doc.get(&path, "admitted").unwrap().is_some(), accepts);
        }
    }
    #[test]
    fn compositions_forward_mutable_admission_exactly_once() {
        fn manual_forwards_mutable_admission_exactly_once() {
            check_mutable_admission(false);
        }

        fn nested_composition_forwards_mutable_admission_exactly_once() {
            check_mutable_admission(true);
        }

        manual_forwards_mutable_admission_exactly_once();
        nested_composition_forwards_mutable_admission_exactly_once();
    }
}
