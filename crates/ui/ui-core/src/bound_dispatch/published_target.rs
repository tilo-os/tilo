//! The resource a local invocation resolves to: the node the component's shared tree keeps, with
//! the status its surfaces lay over the action it names.
use super::{AutoCommit, CallbackBinding, ResourceSpec, SharedResource, Widget};
use tilo_ui_base::widget::capability::capability;

/// The resource a local invocation of `address` resolves to in `widget`'s
/// filtered tree, found in the component's shared tree without building the
/// rest of it ([`shared_target`]). What a surface says of a row now (its
/// status: whether the effect can be used there, how it is named, the
/// parameters it narrows) is not kept in the shared tree, so the target action
/// is laid over as the widget publishes it
/// ([`tilo_ui_base::widget::resources::PublishesResources::published_action`]): a caller
/// cannot invoke what the palette refuses, and no invocation builds a
/// publication. `None` defers to the whole tree: the tree does not resolve the
/// address itself (the unbound and provider fallbacks), or the action is not
/// permitted there.
pub(super) fn resolve_published_action(
    widget: &dyn Widget,
    doc: &AutoCommit,
    binding: &CallbackBinding,
    address: &tilo_actions::ActionAddress,
) -> Option<ResourceSpec> {
    if tilo_ui_base::widget::component_walk::has_separate_document_owner(widget) {
        return None;
    }
    let forest = tilo_ui_base::widget::resources::shared_resources(widget, doc);
    let mut found = shared_target(&forest, Some(binding), address)?;
    let Some(publisher) =
        capability::<dyn tilo_ui_base::widget::resources::PublishesResources>(widget)
    else {
        return Some(found);
    };
    for spec in found
        .actions
        .iter_mut()
        .filter(|spec| spec.id == address.action)
    {
        *spec = publisher.published_action(found.id.as_str(), spec.clone());
    }
    Some(found)
}

/// The node `address` names in `forest`, exactly as `resolve_resource_invocation` finds it in
/// the tree `filter_resource` keeps for `binding` (or in the whole tree, with no binding): the
/// first, depth first, with the address's resource id that filtering does not drop. Answered as
/// its head alone, with its permitted actions and without its children, found without building,
/// copying or filtering the rest of the tree: a resource of thousands of items resolves one of them,
/// or itself, for the cost of finding it. `None` when the forest does not resolve the address, or
/// the node found does not offer the action.
pub(super) fn shared_target(
    forest: &[SharedResource],
    binding: Option<&CallbackBinding>,
    address: &tilo_actions::ActionAddress,
) -> Option<ResourceSpec> {
    fn resolves(node: &SharedResource, address: &tilo_actions::ActionAddress) -> bool {
        (node.id() == address.resource
            && node
                .head()
                .actions
                .iter()
                .any(|action| action.id == address.action))
            || node.children().iter().any(|child| resolves(child, address))
    }
    fn kept(decider: &tilo_ui_base::permissions::ActionDecider<'_>, node: &SharedResource) -> bool {
        let head = node.head();
        head.actions
            .iter()
            .any(|action| decider.permits(&head.kind, head.id.as_str(), action))
            || node.children().iter().any(|child| kept(decider, child))
    }
    /// The first node with resource id `id` that filtering keeps (any, with no decider). A node
    /// filtering drops has no kept node beneath it.
    fn first<'a>(
        decider: Option<&tilo_ui_base::permissions::ActionDecider<'_>>,
        node: &'a SharedResource,
        id: &str,
    ) -> Option<&'a SharedResource> {
        if node.id() == id && decider.is_none_or(|decider| kept(decider, node)) {
            return Some(node);
        }
        node.children()
            .iter()
            .find_map(|child| first(decider, child, id))
    }
    if !forest.iter().any(|node| resolves(node, address)) {
        return None;
    }
    let permissions = binding.map(|binding| binding.permissions.borrow());
    let decider = binding
        .zip(permissions.as_ref())
        .map(|(binding, permissions)| permissions.action_decider(&binding.app_id));
    let node = forest
        .iter()
        .find_map(|node| first(decider.as_ref(), node, &address.resource))?;
    let mut resource = node.head().clone();
    if let Some(decider) = &decider {
        resource
            .actions
            .retain(|action| decider.permits(&node.head().kind, node.head().id.as_str(), action));
    }
    resource
        .actions
        .iter()
        .any(|action| action.id == address.action)
        .then_some(resource)
}
