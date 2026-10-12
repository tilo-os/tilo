//! The least authority a shortcut needs.
//!
//! A shortcut acts with the intersection of its grant and the authority of
//! whoever runs it. The grant the editor proposes names exactly the actions
//! the steps invoke, each for the kind of action its contract is, so the
//! person reviewing it sees everything the shortcut may do and nothing more.

use tilo_action_policy::{ActionKind, Answer, KindSet, Level, Rule, TreePath};
use tilo_automation::{
    AGENT_TRANSFORM_KIND, ActionCatalog, Binding, ResourceTarget, Selector, Step, StepKind,
};
use ui::tilo_actions::ActionKey;

/// The resources a permission reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    /// The one resource a step names.
    Resource(String),
    /// Every resource of a kind, for a step that picks its resource when it
    /// runs.
    Kind(String),
}

/// One permission the shortcut will use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permission {
    pub app_id: String,
    pub reach: Reach,
    pub action: String,
    /// The kind of action it is: reading, an undoable change, or one that
    /// reaches outside.
    pub kind: ActionKind,
    /// How the catalogue names it: "List notes in Inbox".
    pub label: String,
}

impl Permission {
    /// The rule allowing it in any room: this action, on the one resource
    /// or on every resource of the kind, for its kind of action and the
    /// weaker ones.
    #[must_use]
    pub fn rule(&self) -> Rule {
        let app = TreePath::root().any(Level::Room).app(self.app_id.clone());
        let node = match &self.reach {
            Reach::Resource(resource) => app.any(Level::Kind).resource(resource.clone()),
            Reach::Kind(kind) => app.kind(kind.clone()).any(Level::Resource),
        };
        let kinds = ActionKind::ALL
            .into_iter()
            .filter(|kind| *kind <= self.kind)
            .fold(KindSet::NONE, KindSet::with);
        Rule::new(node.action(self.action.clone()), kinds, Answer::Allow)
    }
}

/// Every permission `steps` use, in step order, each once as the strongest
/// kind any step takes it as.
#[must_use]
pub fn least_privilege(steps: &[Step], catalog: &ActionCatalog) -> Vec<Permission> {
    let mut out = Vec::new();
    collect(steps, catalog, &mut out);
    out
}

/// The grant rules for `permissions`.
#[must_use]
pub fn rules(permissions: &[Permission]) -> Vec<Rule> {
    permissions.iter().map(Permission::rule).collect()
}

fn collect(steps: &[Step], catalog: &ActionCatalog, out: &mut Vec<Permission>) {
    for step in steps {
        match &step.kind {
            StepKind::Invoke {
                action, resource, ..
            } => {
                let target = match resource {
                    Selector::Fixed(id)
                    | Selector::FromBinding(Binding::Literal(serde_json::Value::String(id))) => {
                        ResourceTarget::Id(id)
                    }
                    Selector::FirstOfKind(kind) => ResourceTarget::Kind(kind),
                    Selector::FromBinding(_) => ResourceTarget::Any,
                };
                add(catalog, action, target, out);
            }
            StepKind::Ai { transform, .. } => add(
                catalog,
                &transform.key(),
                ResourceTarget::Kind(AGENT_TRANSFORM_KIND),
                out,
            ),
            StepKind::If {
                then, otherwise, ..
            } => {
                collect(then, catalog, out);
                collect(otherwise, catalog, out);
            }
            StepKind::ForEach { body, .. } => collect(body, catalog, out),
            StepKind::SetVar { .. } | StepKind::Ask { .. } => {}
        }
    }
}

fn add(
    catalog: &ActionCatalog,
    key: &ActionKey,
    target: ResourceTarget<'_>,
    out: &mut Vec<Permission>,
) {
    let (app_id, action) = (key.app.as_str(), key.action.as_str());
    let row = catalog.find(key, target);
    let descriptor = row.and_then(|row| row.action.as_ref());
    let reach = match target {
        ResourceTarget::Id(id) => Reach::Resource(id.to_owned()),
        // The kind of the resource the step finds, which may be a sub-kind
        // of the one it names: a grant covers exactly that kind.
        ResourceTarget::Kind(kind) => Reach::Kind(descriptor.map_or_else(
            || kind.to_owned(),
            |descriptor| descriptor.resource_kind.clone(),
        )),
        // A bound resource is only known when the shortcut runs; the kind
        // of the resource the catalogue offers the action on is what it
        // may reach. Without one there is nothing to grant.
        ResourceTarget::Any => match descriptor {
            Some(descriptor) => Reach::Kind(descriptor.resource_kind.clone()),
            None => return,
        },
    };
    let kind = descriptor.map_or(ActionKind::Undoable, |descriptor| descriptor.contract.kind);
    let label = match (&reach, descriptor) {
        (Reach::Resource(_), Some(descriptor)) => format!(
            "{} in {}",
            descriptor.action_label, descriptor.resource_label
        ),
        (Reach::Kind(kind), Some(descriptor)) => {
            format!("{} in {kind}", descriptor.action_label)
        }
        (Reach::Resource(resource) | Reach::Kind(resource), None) => {
            format!("{action} in {resource}")
        }
    };
    if let Some(existing) = out.iter_mut().find(|existing| {
        existing.app_id == app_id && existing.reach == reach && existing.action == action
    }) {
        existing.kind = existing.kind.max(kind);
        return;
    }
    out.push(Permission {
        app_id: app_id.to_owned(),
        reach,
        action: action.to_owned(),
        kind,
        label,
    });
}
