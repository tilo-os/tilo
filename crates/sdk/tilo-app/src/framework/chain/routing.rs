//! Where a chain's published resources send their document actions: to the chain.

use tilo_actions::OperationKind;

use super::CHAIN_ACTION_TYPE;

/// `resource`'s document actions, and those of its children, carried out on
/// the chain `chain`, each keeping the need it declares. An item's views
/// and the app's other effects on it stay as the part publishes them,
/// answered by the app's window: an item of a part opens at the address a
/// workspace's host knows it by. The chain itself offers only document actions;
/// the app's own record offers its effects.
pub(super) fn route(
    resource: tilo_actions::ResourceSpec,
    chain: &str,
) -> tilo_actions::ResourceSpec {
    route_at(resource, chain, false)
}

pub(super) fn route_at(
    mut resource: tilo_actions::ResourceSpec,
    chain: &str,
    item: bool,
) -> tilo_actions::ResourceSpec {
    resource.actions = std::mem::take(&mut resource.actions)
        .into_iter()
        .filter_map(|spec| {
            if spec.kind == OperationKind::Document {
                Some(super::super::resources::route_operation(
                    spec,
                    CHAIN_ACTION_TYPE,
                    chain,
                ))
            } else {
                item.then_some(spec)
            }
        })
        .collect();
    resource.children = std::mem::take(&mut resource.children)
        .into_iter()
        .map(|child| route_at(child, chain, true))
        .collect();
    resource
}
