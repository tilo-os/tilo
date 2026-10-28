//! Source-scoped accessibility for app-owned secondary surfaces.

use super::{
    AccessNode, AkNode, AkNodeId, AkRect, AkRole, ROOT_ID, TreeInfo, TreeUpdate,
    apply_semantics_common, map_role, synthetic_node_id, synthetic_text_run_node_id,
    text_run_content, text_run_for,
};
use vello::kurbo::Point;

struct SurfaceWalk {
    nodes: Vec<(AkNodeId, AkNode)>,
    focus: Option<AkNodeId>,
    origin: Point,
    scale: f64,
}

impl SurfaceWalk {
    fn visit(&mut self, access: &AccessNode, parent: AkNodeId, ordinal: usize) -> AkNodeId {
        let id = synthetic_node_id(parent, access.key.unwrap_or(ordinal as u64));
        let mut local = access.clone();
        local.bounds = access
            .bounds
            .map(|bounds| (bounds.to_rect() - self.origin.to_vec2()).into());
        let mut node = AkNode::new(map_role(local.role));
        apply_semantics_common(&mut node, &local);
        if let Some(bounds) = local.bounds {
            node.set_bounds(AkRect::new(
                bounds.x0 * self.scale,
                bounds.y0 * self.scale,
                bounds.x1 * self.scale,
                bounds.y1 * self.scale,
            ));
        }
        if local.state.focused && self.focus.is_none() {
            self.focus = Some(id);
        }
        let mut children: Vec<_> = access
            .children
            .iter()
            .enumerate()
            .map(|(index, child)| self.visit(child, id, index))
            .collect();
        let run_id = synthetic_text_run_node_id(id);
        if let Some(run) = text_run_for(
            &mut node,
            &local,
            run_id,
            local.bounds.map(crate::a11y::AccessBounds::to_rect),
            self.scale,
        ) {
            children.insert(0, run_id);
            self.nodes.push((run_id, run));
        }
        node.set_children(children);
        self.nodes.push((id, node));
        id
    }
}

pub(crate) fn surface_tree_update(
    access: Option<&AccessNode>,
    origin: Point,
    scale: f64,
) -> TreeUpdate {
    let mut walk = SurfaceWalk {
        nodes: Vec::new(),
        focus: None,
        origin,
        scale,
    };
    let mut window = AkNode::new(AkRole::Window);
    if let Some(access) = access {
        if let Some(name) = &access.name {
            window.set_label(name);
        }
        window.set_children(vec![walk.visit(access, ROOT_ID, 0)]);
    }
    walk.nodes.push((ROOT_ID, window));
    TreeUpdate {
        nodes: walk.nodes,
        tree: Some(TreeInfo::new(ROOT_ID)),
        tree_id: accesskit::TreeId::ROOT,
        focus: walk.focus.unwrap_or(ROOT_ID),
    }
}

/// Publish `part`, a part of the window its host draws rather than any
/// widget (a window of its own's row of slots), in the window's `update`:
/// first among the window's children, in logical pixels times `scale`. A
/// node of it that holds the keyboard holds the window's focus. Its nodes'
/// ids are the ones [`surface_action_target`] resolves against `part`; give
/// `part` a [`key`](AccessNode::key) no other part of the window uses.
pub(crate) fn publish_host_part(update: &mut TreeUpdate, part: &AccessNode, scale: f64) {
    let mut walk = SurfaceWalk {
        nodes: Vec::new(),
        focus: None,
        origin: Point::ZERO,
        scale,
    };
    let id = walk.visit(part, ROOT_ID, 0);
    let Some((_, window)) = update.nodes.iter_mut().find(|(node, _)| *node == ROOT_ID) else {
        return;
    };
    let mut children = Vec::with_capacity(window.children().len() + 1);
    children.push(id);
    children.extend_from_slice(window.children());
    window.set_children(children);
    update.nodes.extend(walk.nodes);
    if let Some(focus) = walk.focus {
        update.focus = focus;
    }
}

pub(crate) fn surface_action_target(access: &AccessNode, requested: AkNodeId) -> Option<&str> {
    fn find(
        access: &AccessNode,
        parent: AkNodeId,
        ordinal: usize,
        requested: AkNodeId,
    ) -> Option<&str> {
        let id = synthetic_node_id(parent, access.key.unwrap_or(ordinal as u64));
        if id == requested
            || (text_run_content(access).is_some() && synthetic_text_run_node_id(id) == requested)
        {
            return access.action_target.as_deref();
        }
        access
            .children
            .iter()
            .enumerate()
            .find_map(|(index, child)| find(child, id, index, requested))
    }
    find(access, ROOT_ID, 0, requested)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::{AccessState, Role};

    #[test]
    fn secondary_tree_is_rebased_and_routes_only_its_source() {
        let composer = AccessNode::new(Role::MultilineTextInput)
            .with_key(12)
            .with_name("Message Tide")
            .with_bounds(vello::kurbo::Rect::new(-700.0, 500.0, -300.0, 550.0))
            .with_state(AccessState::new().focusable(true).focused(true))
            .with_action_target("surface:tide|chat.composer");
        let source = AccessNode::new(Role::Group)
            .with_key(7)
            .with_name("Tide")
            .with_children(vec![composer]);
        let update = surface_tree_update(Some(&source), Point::new(-800.0, 200.0), 2.0);
        let (id, node) = update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Message Tide"))
            .expect("the actual composer is exported");
        assert_eq!(
            node.bounds(),
            Some(AkRect::new(200.0, 600.0, 1000.0, 700.0))
        );
        assert_eq!(update.focus, *id);
        assert_eq!(
            surface_action_target(&source, *id),
            Some("surface:tide|chat.composer")
        );
        let other = AccessNode::new(Role::Group)
            .with_key(8)
            .with_name("Other agent");
        assert!(surface_action_target(&other, *id).is_none());
    }
}
