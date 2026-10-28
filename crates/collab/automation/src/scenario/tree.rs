//! The published action a scenario step names, found in what the app
//! publishes.
//!
//! A Shortcut resolves a step against catalogue rows a host returned; a
//! scenario standing next to the running app has the app's own resource tree
//! instead. Both end at one address, so a step and a click reach the same
//! door. Here that is the only thing the tree is asked: which address
//! `invocation` names, the first publication of that kind when the step does
//! not fix a resource id.

use tilo_actions::door;
use tilo_actions::{ActionKey, Resource};

use crate::{Binding, Invocation, Selector, kind_matches};

/// The published address `invocation` names, or `None` when the tree
/// publishes no such action.
#[must_use]
pub fn resolve(resources: &[Resource], invocation: &Invocation) -> Option<door::ActionAddress> {
    let wanted = SelectorTarget::of(&invocation.resource)?;
    let mut best: Option<(usize, door::ActionAddress)> = None;
    walk(resources, &mut |resource| {
        if !matching(resource, &invocation.action, &wanted) {
            return;
        }
        for spec in &resource.actions {
            if spec.id != invocation.action.action {
                continue;
            }
            // An exact id beats the first publication of the kind, and an
            // earlier publication beats a later one: one tree, one answer,
            // whatever order the walk takes.
            let rank = usize::from(!wanted.is_exactly(resource.id.as_str()));
            if best.as_ref().is_none_or(|(held, _)| rank < *held) {
                best = Some((
                    rank,
                    door::ActionAddress::new(resource.id.as_str(), spec.id.clone()),
                ));
            }
        }
    });
    best.map(|(_, address)| address)
}

/// The resource a step names, once any literal in it is read.
enum SelectorTarget<'a> {
    /// Exactly this id.
    Fixed(&'a str),
    /// The first publication of this kind, or a sub-kind of it.
    Kind(&'a str),
}

impl<'a> SelectorTarget<'a> {
    /// Whether this names the resource `id` exactly, rather than the first
    /// publication of a kind.
    fn is_exactly(&self, id: &str) -> bool {
        matches!(self, Self::Fixed(fixed) if *fixed == id)
    }

    fn of(selector: &'a Selector) -> Option<Self> {
        match selector {
            Selector::Fixed(id) => Some(Self::Fixed(id)),
            Selector::FirstOfKind(kind) => Some(Self::Kind(kind)),
            // A binding is part of a Shortcut's program, and its value at run
            // time is the run's business, not the tree's.
            Selector::FromBinding(Binding::Literal(serde_json::Value::String(id))) => {
                Some(Self::Fixed(id))
            }
            Selector::FromBinding(_) => None,
        }
    }
}

fn matching(resource: &Resource, action: &ActionKey, target: &SelectorTarget<'_>) -> bool {
    kind_matches(&action.kind, &resource.kind)
        && match target {
            SelectorTarget::Fixed(id) => resource.id.as_str() == *id,
            SelectorTarget::Kind(kind) => kind_matches(kind, &resource.kind),
        }
}

/// `resource` and every resource under it.
fn walk(resources: &[Resource], visit: &mut impl FnMut(&Resource)) {
    for resource in resources {
        visit(resource);
        walk(&resource.children, visit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StepPath;
    use crate::scenario::Invocation;
    use std::collections::BTreeMap;

    fn action(id: &str) -> tilo_actions::ActionSpec {
        tilo_actions::ActionSpec::typed(
            id,
            id,
            "does it",
            tilo_actions::ExecutionLocus::ClientLocal,
            Vec::new(),
            |_| tilo_actions::EncodedAction {
                type_id: "test.rename".into(),
                payload: Vec::new(),
            },
        )
    }

    fn note(id: &str) -> Resource {
        let mut resource = Resource::new(id, "notes.note", "Note");
        resource.actions = vec![action("rename"), action("delete")];
        resource
    }

    fn step(resource: Selector) -> Invocation {
        Invocation {
            path: String::new(),
            step: "rename".into(),
            action: ActionKey::new("so.tilo.notes", "notes.note", "rename"),
            resource,
            args: BTreeMap::new(),
            appearance: None,
            expect_error: None,
            bind: None,
        }
    }

    #[test]
    fn a_step_finds_the_action_the_tree_publishes() {
        let folder = Resource::new("notes.folder:inbox", "notes.folder", "Inbox");
        let mut folder = folder;
        folder.children = vec![note("notes.note:1")];
        let mut nested = Resource::new("notes.note:2", "notes.note", "Second");
        nested.actions = vec![action("rename")];
        folder.children.push(nested);
        let resources = vec![folder];

        // A fixed id is that resource's, never the first of its kind, and a
        // child is in the tree as much as a top-level resource is.
        assert_eq!(
            resolve(&resources, &step(Selector::Fixed("notes.note:2".into()))),
            Some(door::ActionAddress::new("notes.note:2", "rename"))
        );
        // No such resource: nothing is resolved, never a guess.
        assert_eq!(
            resolve(&resources, &step(Selector::Fixed("notes.note:9".into()))),
            None
        );
        // A kind takes the first publication of it or a sub-kind of it.
        assert_eq!(
            resolve(
                &resources,
                &step(Selector::FirstOfKind("notes.note".into()))
            ),
            Some(door::ActionAddress::new("notes.note:1", "rename"))
        );
        // A parent kind takes what is below it: `notes`, `notes.note`.
        assert_eq!(
            resolve(&resources, &step(Selector::FirstOfKind("notes".into()))),
            Some(door::ActionAddress::new("notes.note:1", "rename"))
        );
        // A kind is not a prefix of any spelling: `not` names nothing.
        assert_eq!(
            resolve(&resources, &step(Selector::FirstOfKind("not".into()))),
            None
        );
        // An action the resource does not publish stays unresolved.
        let mut missing = step(Selector::Fixed("notes.note:1".into()));
        missing.action = ActionKey::new("so.tilo.notes", "notes.note", "publish");
        assert_eq!(resolve(&resources, &missing), None);
        // A value only the run knows names nothing before it runs.
        assert_eq!(
            resolve(
                &resources,
                &step(Selector::FromBinding(Binding::Input("note".into())))
            ),
            None
        );
        let _ = StepPath::root();
    }
}
