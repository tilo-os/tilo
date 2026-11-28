//! The resource a local invocation resolves to: the node the component's shared tree keeps, with
//! the status its surfaces lay over the action it names.
use super::AutoCommit;
use super::CallbackBinding;
use super::ResourceSpec;
use super::SharedResource;
use super::Widget;
use tilo_ui_base::widget::capability::capability;

/// The resource a local invocation of `address` resolves to in `widget`'s
/// filtered tree, found in the component's shared tree without building the
/// rest of it ([`shared_target`]). What a surface says of a row now (its
/// status: whether the effect can be used there, how it is named, the
/// parameters it narrows) is not kept in the shared tree, so the target action
/// is laid over as the widget publishes it
/// ([`tilo_ui::widget::resources::PublishesResources::published_action`]): a caller
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
    let forest = tilo_ui::widget::resources::shared_resources(widget, doc);
    let mut found = shared_target(&forest, Some(binding), address)?;
    let Some(publisher) = capability::<dyn tilo_ui::widget::resources::PublishesResources>(widget)
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
    type Shows<'a> = (&'a tilo_ui_base::permissions::Verdicts, &'a str);
    fn kept(shows: Shows<'_>, node: &SharedResource) -> bool {
        let head = node.head();
        head.actions
            .iter()
            .any(|action| shows.0.shows_published(shows.1, head, action))
            || node.children().iter().any(|child| kept(shows, child))
    }
    /// The first node with resource id `id` that filtering keeps (any, with no binding). A node
    /// filtering drops has no kept node beneath it.
    fn first<'a>(
        shows: Option<Shows<'_>>,
        node: &'a SharedResource,
        id: &str,
    ) -> Option<&'a SharedResource> {
        if node.id() == id && shows.is_none_or(|shows| kept(shows, node)) {
            return Some(node);
        }
        node.children()
            .iter()
            .find_map(|child| first(shows, child, id))
    }
    if !forest.iter().any(|node| resolves(node, address)) {
        return None;
    }
    let permissions = binding.map(|binding| binding.permissions.borrow());
    let shows = binding
        .zip(permissions.as_ref())
        .map(|(binding, permissions)| (&**permissions, binding.app_id.as_str()));
    let node = forest
        .iter()
        .find_map(|node| first(shows, node, &address.resource))?;
    let mut resource = node.head().clone();
    if let Some((permissions, app)) = shows {
        resource
            .actions
            .retain(|action| permissions.shows_published(app, node.head(), action));
    }
    resource
        .actions
        .iter()
        .any(|action| action.id == address.action)
        .then_some(resource)
}
