//! The admitted nodes the accessibility bridge may name and address.
#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use crate::accesskit_bridge::A11yTarget;

/// The live, admitted nodes exported to the AT bridge. A target resolves back
/// only through the current admission.
pub(crate) struct ExportedNodes {
    pub(crate) nodes: HashMap<NodeId, *mut dyn Widget>,
    pub(crate) names: crate::a11y_tree::AdmittedNodeNames,
}
impl ExportedNodes {
    pub(crate) fn focus(&self, node: Option<NodeId>) -> Option<NodeId> {
        node.filter(|node| self.nodes.contains_key(node))
    }
    pub(crate) fn resolve(&self, root: &dyn Widget, id: accesskit::NodeId) -> Option<A11yTarget> {
        let target = crate::accesskit_bridge::resolve_node_id_bound(root, &self.nodes, id)?;
        let exported = |node: &NodeId| self.nodes.contains_key(node);
        match &target {
            A11yTarget::Root => {}
            A11yTarget::Widget(node) | A11yTarget::Synthetic { owner: node, .. } => {
                if !exported(node) {
                    return None;
                }
            }
        }
        Some(target)
    }
}
impl BoundNodeMap {
    pub(crate) fn semantic_node_names(
        &self,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
    ) -> crate::a11y_tree::AdmittedNodeNames {
        let mut names = crate::a11y_tree::AdmittedNodeNames::default();
        for (&node, &pointer) in nodes {
            let Some(admitted) = self.nodes.get(&node) else {
                continue;
            };
            if !std::ptr::eq(admitted.pointer, pointer) || admitted.inactive {
                continue;
            }
            names.insert(node, pointer, admitted.semantic_name.clone());
        }
        names
    }
    pub(crate) fn exported_nodes(&self, nodes: &HashMap<NodeId, *mut dyn Widget>) -> ExportedNodes {
        let mut exported = ExportedNodes {
            nodes: HashMap::new(),
            names: crate::a11y_tree::AdmittedNodeNames::default(),
        };
        for (&node, &pointer) in nodes {
            let Some(admitted) = self.nodes.get(&node) else {
                continue;
            };
            if !std::ptr::eq(admitted.pointer, pointer) || admitted.inactive {
                continue;
            }
            exported.nodes.insert(node, pointer);
            exported
                .names
                .insert(node, pointer, admitted.semantic_name.clone());
        }
        exported
    }
}
