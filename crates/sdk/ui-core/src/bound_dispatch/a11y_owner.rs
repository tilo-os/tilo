//! Host-local AT aliases for legacy live record owners. Taffy arena identity
//! alone is insufficient when a replacement owner occupies a rebuilt slot.
#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use crate::accesskit_bridge::A11yTarget;

const ALIAS_START: u64 = 1 << 47;
const ALIAS_LIMIT: u64 = (1 << 48) - 2;
const MAX_ALIASES: usize = 131_072;
#[derive(Default)]
pub(super) struct OwnerAliases {
    next: u64,
    aliases: HashMap<(uuid::Uuid, NodeId), NodeId>,
}
impl OwnerAliases {
    pub(super) fn admit(&mut self, nodes: &HashMap<NodeId, BoundNode>) {
        for (&node, entry) in nodes {
            let Some(lifetime) = entry
                .legacy
                .as_ref()
                .and_then(crate::history_mode::HistoryOwner::lifetime_id)
            else {
                continue;
            };
            let key = (lifetime, node);
            if self.aliases.contains_key(&key) || self.aliases.len() >= MAX_ALIASES {
                continue;
            }
            self.next = self.next.max(ALIAS_START);
            while self.next < ALIAS_LIMIT {
                self.next = self.next.saturating_add(1);
                let alias = NodeId::from(self.next);
                if nodes.contains_key(&alias) {
                    continue;
                }
                self.aliases.insert(key, alias);
                break;
            }
        }
    }
}
/// Virtual node keys are used only by the existing AT bridge, never Taffy or
/// widget callbacks. The inverse resolves back through the current admission.
pub(crate) struct ExportedNodes {
    pub(crate) nodes: HashMap<NodeId, *mut dyn Widget>,
    pub(crate) names: crate::a11y_tree::AdmittedNodeNames,
    actual: HashMap<NodeId, NodeId>,
    exported: HashMap<NodeId, NodeId>,
}
impl ExportedNodes {
    pub(crate) fn focus(&self, node: Option<NodeId>) -> Option<NodeId> {
        self.exported.get(&node?).copied()
    }
    pub(crate) fn resolve(&self, root: &dyn Widget, id: accesskit::NodeId) -> Option<A11yTarget> {
        let target = crate::accesskit_bridge::resolve_node_id_bound(root, &self.nodes, id)?;
        Some(match target {
            A11yTarget::Root => A11yTarget::Root,
            A11yTarget::Widget(node) => A11yTarget::Widget(*self.actual.get(&node)?),
            A11yTarget::Synthetic {
                owner,
                key_path,
                ordinal_path,
                semantic_target,
            } => A11yTarget::Synthetic {
                owner: *self.actual.get(&owner)?,
                key_path,
                ordinal_path,
                semantic_target,
            },
        })
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
            if !std::ptr::eq(admitted.pointer, pointer)
                || admitted.inactive
                || admitted
                    .legacy
                    .as_ref()
                    .is_some_and(|owner| owner.lifetime_id().is_none())
            {
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
            actual: HashMap::new(),
            exported: HashMap::new(),
        };
        let mut collisions = std::collections::HashSet::new();
        for (&node, &pointer) in nodes {
            let Some(admitted) = self.nodes.get(&node) else {
                continue;
            };
            if !std::ptr::eq(admitted.pointer, pointer) || admitted.inactive {
                continue;
            }
            let alias = if let Some(owner) = &admitted.legacy {
                let Some(lifetime) = owner.lifetime_id() else {
                    continue;
                };
                let Some(alias) = self.a11y_aliases.aliases.get(&(lifetime, node)) else {
                    continue;
                };
                *alias
            } else {
                node
            };
            if exported.nodes.insert(alias, pointer).is_some() {
                collisions.insert(alias);
            }
            exported
                .names
                .insert(alias, pointer, admitted.semantic_name.clone());
            exported.actual.insert(alias, node);
            exported.exported.insert(node, alias);
        }
        // Alias exhaustion/collision is unavailable, never a raw-id fallback.
        for alias in collisions {
            exported.nodes.remove(&alias);
            exported.names.remove(alias);
            exported.actual.remove(&alias);
            exported.exported.retain(|_, candidate| *candidate != alias);
        }
        exported
    }
}
