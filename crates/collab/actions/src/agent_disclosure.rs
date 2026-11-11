//! Actions an agent is never shown, because the resource they sit on is
//! disclosed only through a private view.
//!
//! The action directory discloses a resource through its first view (a
//! document read first), else through a descendant's. A view that changes
//! only what its caller sees, and is neither a document read nor the
//! caller's own participant view, is private: its resource's label and
//! presentation are the publisher's, and the broker never lists that
//! resource, nor anything below it, to an agent. A resource without a view
//! is not a listing of its own: its actions stand in its place, each listed
//! as the caller's policy allows.
//!
//! So a view an app adds for its own window (Stop on a page, Show history on
//! the history) makes everything on that resource private: actions an agent
//! is meant to take there are never offered, whatever it is granted. That is
//! a declaration mistake, and [`hidden_from_agents`] names it.

use crate::protocol::ContractViolation;
use crate::{ActionSpec, OperationKind, ResourceSpec};

/// Every action in `resources` that agents' listings would offer
/// ([`crate::ActionContract::reaches_agents`]) yet sits on, or below, a
/// resource disclosed only through a private view. Top-level resources are
/// exempt: an agent addresses an app's root by the app itself.
#[must_use]
pub fn hidden_from_agents(resources: &[ResourceSpec]) -> Vec<ContractViolation> {
    let mut hidden = Vec::new();
    for resource in resources {
        for child in &resource.children {
            walk(child, &mut hidden);
        }
    }
    hidden
}

fn walk(resource: &ResourceSpec, hidden: &mut Vec<ContractViolation>) {
    if let Some(view) = disclosing_view(resource).filter(|view| is_private(view)) {
        hide(resource, resource, view, hidden);
        return;
    }
    for child in &resource.children {
        walk(child, hidden);
    }
}

/// The view the directory discloses `resource` through: its own first view,
/// a document read before any other, else its first child's.
fn disclosing_view(resource: &ResourceSpec) -> Option<&ActionSpec> {
    let mut first: Option<&ActionSpec> = None;
    for action in resource
        .actions
        .iter()
        .filter(|action| action.kind == OperationKind::View)
    {
        if first.is_none_or(|first| {
            action.contract.reads_document() && !first.contract.reads_document()
        }) {
            first = Some(action);
        }
    }
    first.or_else(|| resource.children.iter().find_map(disclosing_view))
}

/// Whether `view` keeps its resource from everyone but the publisher.
fn is_private(view: &ActionSpec) -> bool {
    !view.contract.is_participant_view() && !view.contract.reads_document()
}

fn hide(
    owner: &ResourceSpec,
    resource: &ResourceSpec,
    view: &ActionSpec,
    hidden: &mut Vec<ContractViolation>,
) {
    for action in resource
        .actions
        .iter()
        .filter(|action| action.kind != OperationKind::View && action.contract.reaches_agents())
    {
        hidden.push(ContractViolation {
            address: format!("{}\u{1f}{}", resource.id.as_str(), action.id),
            reason: format!(
                "an agent is never shown it: {} is disclosed only through the private view \
                 `{}`, which keeps the resource and everything on it from agents. Disclose it \
                 through a document read or a participant view, or move that view off it",
                owner.id.as_str(),
                view.id
            ),
        });
    }
    for child in &resource.children {
        hide(owner, child, view, hidden);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionContract, OperationKind};

    fn action(id: &str, kind: OperationKind, contract: ActionContract) -> ActionSpec {
        let mut spec = ActionSpec::typed_operation(id, id, "", kind, id, Vec::new(), |_| {
            crate::EncodedAction {
                type_id: "test".into(),
                payload: Vec::new(),
            }
        });
        spec.type_mut().contract = contract;
        spec
    }

    fn view(id: &str, tags: &[&str]) -> ActionSpec {
        let mut contract = ActionContract::view();
        contract.tags = tags.iter().map(|tag| (*tag).to_owned()).collect();
        action(id, OperationKind::View, contract)
    }

    fn effect(id: &str) -> ActionSpec {
        action(id, OperationKind::Effect, ActionContract::default())
    }

    fn app(children: Vec<ResourceSpec>) -> Vec<ResourceSpec> {
        let mut root = ResourceSpec::new("app", "app", "App");
        root.actions.push(view("show", &[]));
        root.children = children;
        vec![root]
    }

    fn row(id: &str, actions: Vec<ActionSpec>) -> ResourceSpec {
        let mut row = ResourceSpec::new(id, id, id);
        row.actions = actions;
        row
    }

    #[test]
    fn a_private_view_hides_every_agent_action_on_its_resource_and_below() {
        let mut page = row(
            "page",
            vec![effect("read"), view("stop", &[]), effect("click")],
        );
        page.children.push(row("frame", vec![effect("frame-read")]));
        let hidden = hidden_from_agents(&app(vec![page]));
        let addresses: Vec<&str> = hidden.iter().map(|v| v.address.as_str()).collect();
        assert_eq!(
            addresses,
            ["page\u{1f}read", "page\u{1f}click", "frame\u{1f}frame-read"]
        );
        assert!(hidden[0].reason.contains("`stop`"), "{}", hidden[0].reason);
    }

    #[test]
    fn a_resource_without_a_private_disclosure_hides_nothing() {
        let resources = app(vec![
            // No view: its actions stand in its place.
            row("page", vec![effect("read"), effect("click")]),
            // A document read discloses under the caller's own policy.
            row(
                "document",
                vec![
                    view("ui", &[]),
                    view("read-document", &[crate::DOCUMENT_READ_TAG]),
                    effect("edit"),
                ],
            ),
            row(
                "mine",
                vec![view("look", &[crate::PARTICIPANT_VIEW_TAG]), effect("act")],
            ),
            // Only private views: nothing an agent could take is hidden.
            row("panel", vec![view("show-panel", &[])]),
        ]);
        assert_eq!(hidden_from_agents(&resources), []);
        // The app's own root is the app's: an agent addresses it by name.
        let mut root = row("app", vec![view("show", &[]), effect("act")]);
        root.children.clear();
        assert_eq!(hidden_from_agents(&[root]), []);
    }
}
