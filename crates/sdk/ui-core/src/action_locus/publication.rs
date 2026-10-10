//! Geometry from an exact live publication owner, never an alias guessed by label.

use std::collections::HashMap;

use crate::a11y_tree::{AdmittedNodeNames, SemanticTree};
use crate::bound_dispatch::BoundActionTarget;
use crate::widget::Widget;
use vello::kurbo::Rect;

pub(crate) struct PublicationGeometry<'a> {
    pub root: &'a dyn Widget,
    pub nodes: &'a HashMap<taffy::NodeId, *mut dyn Widget>,
    pub names: &'a AdmittedNodeNames,
    pub focused: Option<taffy::NodeId>,
}

impl PublicationGeometry<'_> {
    pub(crate) fn interactions(
        &self,
        doc: &automerge::AutoCommit,
        publication: &crate::bound_dispatch::BoundPublication,
        interactions: &[tilo_actions::SemanticInteraction],
    ) -> Vec<(tilo_actions::InvocationId, Option<Rect>)> {
        interactions
            .iter()
            .filter(|interaction| {
                interaction
                    .resource
                    .id
                    .starts_with(crate::bound_dispatch::PUBLICATION_PREFIX)
            })
            .map(|interaction| {
                let address = tilo_actions::ActionAddress::new(
                    &interaction.resource.id,
                    &interaction.action.id,
                );
                let bounds = publication
                    .target(&address)
                    .ok()
                    .flatten()
                    .and_then(|target| {
                        let app = interaction.action.app.as_ref()?;
                        if app.id != target.binding.app_id
                            || interaction.anchor != tilo_actions::InteractionAnchor::Resource
                        {
                            return None;
                        }
                        self.bounds(doc, &target)
                    });
                // Retain refused aliases as unresolved; never fall back to
                // a same-named unbound resource or another mounted owner.
                (interaction.invocation, bounds)
            })
            .collect()
    }

    pub(crate) fn bounds(
        &self,
        doc: &automerge::AutoCommit,
        target: &BoundActionTarget,
    ) -> Option<Rect> {
        let _entry = target.binding.enter().ok()?;
        let snapshots =
            crate::bound_dispatch::discover_bound_resources(self.root, doc, None).ok()?;
        let snapshot = snapshots
            .iter()
            .find(|snapshot| snapshot.binding.same_component(&target.binding))?;
        let mut result = Vec::new();
        self.visit(self.root, target, snapshot.resources(), &mut result);
        if result.len() == 1 {
            result.pop().flatten()
        } else {
            None
        }
    }

    fn visit(
        &self,
        widget: &dyn Widget,
        target: &BoundActionTarget,
        resources: &[tilo_actions::Resource],
        result: &mut Vec<Option<Rect>>,
    ) {
        if widget
            .callback_binding()
            .is_some_and(|binding| binding.same_component(&target.binding))
        {
            // A foreign nested mount cannot supply a same-named control.
            if foreign_owner(widget, &target.binding) {
                result.push(None);
                return;
            }
            let mut tree = SemanticTree::new();
            tree.refresh_bound(widget, self.nodes, self.names, self.focused, resources);
            let bindings = crate::widget::controls::control_action_bindings(widget);
            result.push(
                super::resource_bounds(&tree, &bindings, &target.address.address)
                    .and_then(|(bounds, node)| self.clipped_bounds(bounds, node)),
            );
            return;
        }
        widget.for_each_child(&mut |child| self.visit(child, target, resources, result));
    }

    fn clipped_bounds(&self, bounds: Rect, node: u64) -> Option<Rect> {
        let pointer = *self.nodes.get(&taffy::NodeId::from(node))?;
        let mut matches = Vec::new();
        visit_clipped(self.root, pointer, bounds, &mut matches);
        (matches.len() == 1).then(|| matches[0])
    }
}

fn foreign_owner(widget: &dyn Widget, owner: &crate::bound_dispatch::CallbackBinding) -> bool {
    if widget
        .callback_binding()
        .is_some_and(|binding| !binding.same_component(owner))
    {
        return true;
    }
    let mut foreign = false;
    widget.for_each_child(&mut |child| foreign |= foreign_owner(child, owner));
    foreign
}

fn visit_clipped(
    widget: &dyn Widget,
    pointer: *const dyn Widget,
    bounds: Rect,
    matches: &mut Vec<Rect>,
) {
    if std::ptr::addr_eq(std::ptr::from_ref(widget), pointer) {
        matches.push(bounds);
        return;
    }
    // Include non-accessible ancestors: they clip painting too.
    let bounds = if let Some(clip) = widget.child_clip_rect() {
        if ![clip.x0, clip.y0, clip.x1, clip.y1]
            .into_iter()
            .all(f64::is_finite)
            || clip.width() <= 0.0
            || clip.height() <= 0.0
        {
            return;
        }
        bounds.intersect(clip)
    } else {
        bounds
    };
    if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
        return;
    }
    widget.for_each_child(&mut |child| visit_clipped(child, pointer, bounds, matches));
}
