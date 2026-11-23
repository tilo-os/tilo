//! A control whose event changes path when the window's published actions change is built again.

use super::UiNode;

/// The tree `node` an app built while the actions `before` were published, with each control in it
/// that would take the other path now that the window offers the actions `after` named as another
/// kind; `None` when none would. An event that runs an action the app publishes is bound to the
/// action itself, and one that runs an action it does not publish is the view's own.
pub(crate) fn rebinding(
    before: &crate::adapter::Published,
    after: &crate::adapter::Adapter,
    node: &UiNode,
) -> Option<UiNode> {
    let own = node.events.values().any(|event| {
        let Some(invoked) = event.invokes.as_ref() else {
            return false;
        };
        before.holds(&invoked.resource, &invoked.action)
            != after.holds(&invoked.resource, &invoked.action)
    });
    let children: Vec<Option<UiNode>> = node
        .children
        .iter()
        .map(|child| rebinding(before, after, child))
        .collect();
    if !own && children.iter().all(Option::is_none) {
        return None;
    }
    Some(UiNode {
        kind: if own {
            format!("{}~rebound", node.kind)
        } else {
            node.kind.clone()
        },
        key: node.key.clone(),
        props: node.props.clone(),
        binding: node.binding.clone(),
        events: node.events.clone(),
        anchor: node.anchor.clone(),
        children: node
            .children
            .iter()
            .zip(children)
            .map(|(child, rebound)| rebound.unwrap_or_else(|| child.clone()))
            .collect(),
    })
}
