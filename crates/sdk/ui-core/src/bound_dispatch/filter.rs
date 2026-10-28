//! The permission filter over published resources: what the participant
//! may invoke stays, the rest is withheld.

use super::*;

pub(crate) fn filter_resource(
    binding: &CallbackBinding,
    resource: ResourceSpec,
) -> Option<ResourceSpec> {
    binding
        .permissions
        .borrow()
        .permitted_resource(&binding.app_id, resource)
}

/// [`filter_resource`] of a shared tree, for the permission context `token`
/// names: a subtree whose result under `token` is kept on it is not
/// filtered again, and the result shares every subtree filtering left
/// unchanged. A token stands for one component's permissions, so the
/// result is exactly [`filter_resource`]'s.
pub(crate) fn filter_shared(
    binding: &CallbackBinding,
    token: u64,
    resource: &SharedResource,
) -> Option<SharedResource> {
    use crate::published::Filtered;
    let filtered = resource.filtered(token).unwrap_or_else(|| {
        let filtered = filter_shared_node(binding, token, resource);
        resource.remember_filtered(token, filtered.clone());
        filtered
    });
    match filtered {
        Filtered::Same => Some(resource.clone()),
        Filtered::Pruned => None,
        Filtered::Kept(kept) => Some(kept),
    }
}

/// Let `next` take `last`'s filtering under `token` when every action of
/// `last` was permitted and `next` offers the same (a root head rebuilt as
/// it was for a new publication): only its children are filtered, each
/// from what it kept, and nobody is asked about its actions again.
pub(super) fn inherit_filtered(
    binding: &CallbackBinding,
    token: u64,
    last: &SharedResource,
    next: &SharedResource,
) {
    use crate::published::Filtered;
    if next.filtered(token).is_some()
        || !matches!(last.filtered(token), Some(Filtered::Same))
        || !same_offer(last, next)
        || (next.head().actions.is_empty() && next.children().is_empty())
    {
        return;
    }
    if matches!(
        filter_shared_children(binding, token, next.children()),
        Filtered::Same
    ) {
        next.remember_filtered(token, Filtered::Same);
    }
}

/// What filtering `resource` under `token` gives, its children's results
/// kept on them.
fn filter_shared_node(
    binding: &CallbackBinding,
    token: u64,
    resource: &SharedResource,
) -> crate::published::Filtered<SharedResource> {
    use crate::published::{Filtered, SharedChildren};
    let _phase = tilo_resource::profiling::Phase::start("ui.filtered_resource");
    let head = resource.head();
    let permissions = binding.permissions.borrow();
    let decider = permissions.action_decider(&binding.app_id);
    let every_action = head
        .actions
        .iter()
        .all(|action| decider.permits(&head.kind, head.id.as_str(), action));
    let children = filter_shared_children(binding, token, resource.children());
    if every_action
        && matches!(children, Filtered::Same)
        && (!head.actions.is_empty() || !resource.children().is_empty())
    {
        return Filtered::Same;
    }
    let children = match children {
        Filtered::Same => resource.children().clone(),
        Filtered::Kept(children) => children,
        Filtered::Pruned => SharedChildren::default(),
    };
    let mut head = head.clone();
    head.retain_actions(|action| {
        decider.permits(&resource.head().kind, resource.head().id.as_str(), action)
    });
    if head.actions.is_empty() && children.is_empty() {
        Filtered::Pruned
    } else {
        Filtered::Kept(SharedResource::new(head, children))
    }
}

/// The filtered `children` under `token`: [`Filtered::Same`] when every
/// child stands as it is, else the children kept, sharing those unchanged.
fn filter_shared_children(
    binding: &CallbackBinding,
    token: u64,
    children: &crate::published::SharedChildren,
) -> crate::published::Filtered<crate::published::SharedChildren> {
    use crate::published::{Filtered, SharedChildren};
    if let Some(filtered) = children.filtered(token) {
        return filtered;
    }
    let mut kept: Option<Vec<SharedResource>> = None;
    for (index, child) in children.iter().enumerate() {
        let filtered = filter_shared(binding, token, child);
        let same = filtered
            .as_ref()
            .is_some_and(|filtered| filtered.ptr_eq(child));
        match (&mut kept, same) {
            (None, true) => {}
            (None, false) => {
                let mut list = children.iter().take(index).cloned().collect::<Vec<_>>();
                list.extend(filtered);
                kept = Some(list);
            }
            (Some(list), _) => list.extend(filtered),
        }
    }
    let filtered = match kept {
        None => Filtered::Same,
        Some(list) if list.is_empty() => Filtered::Pruned,
        Some(list) => Filtered::Kept(SharedChildren::new(list)),
    };
    children.remember_filtered(token, filtered.clone());
    filtered
}

/// Add to `allowed` the address of every available action in `resources`
/// that `binding` permits: what [`tilo_actions::door::survey`] lists as
/// available in the trees [`filter_resource`] keeps, read in place rather
/// than through a filtered copy and a descriptor per action.
pub(super) fn insert_available_permitted(
    binding: &CallbackBinding,
    resources: &[ResourceSpec],
    allowed: &mut std::collections::HashSet<tilo_actions::ActionAddress>,
) {
    let permissions = binding.permissions.borrow();
    let decider = permissions.action_decider(&binding.app_id);
    insert_available(&decider, resources, allowed);
}

fn insert_available(
    decider: &crate::permissions::ActionDecider<'_>,
    resources: &[ResourceSpec],
    allowed: &mut std::collections::HashSet<tilo_actions::ActionAddress>,
) {
    for resource in resources {
        for action in &resource.actions {
            if action.contract.availability.is_available()
                && decider.permits(&resource.kind, resource.id.as_str(), action)
            {
                allowed.insert(tilo_actions::ActionAddress::new(
                    resource.id.as_str(),
                    &action.id,
                ));
            }
        }
        insert_available(decider, &resource.children, allowed);
    }
}
