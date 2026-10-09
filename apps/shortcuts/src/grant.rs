//! The least authority a shortcut needs.
//!
//! A shortcut acts with the intersection of its grant and the authority of
//! whoever runs it. The grant the editor proposes names exactly the actions
//! the steps invoke, each at the weakest verb its contract requires, so the
//! person reviewing it sees everything the shortcut may do and nothing more.

use collab_types::{PermissionProfileRule, Scope, Verb};
use tilo_automation::{
    AGENT_TRANSFORM_KIND, ActionCatalog, Binding, ResourceTarget, Selector, Step, StepKind,
    required_verb,
};
use ui::tilo_actions::{ActionKey, PermissionRequirement};

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
    pub verb: Verb,
    /// How the catalogue names it: "List notes in Inbox".
    pub label: String,
}

impl Permission {
    #[must_use]
    pub fn rule(&self) -> PermissionProfileRule {
        let scope = match &self.reach {
            Reach::Resource(resource) => Scope::Action {
                app: self.app_id.clone(),
                resource: resource.clone(),
                action: self.action.clone(),
            },
            Reach::Kind(kind) => Scope::ActionOnKind {
                app: self.app_id.clone(),
                kind: kind.clone(),
                action: self.action.clone(),
            },
        };
        PermissionProfileRule::grant(scope, self.verb)
    }
}

/// Every permission `steps` use, in step order, each once at the strongest
/// verb any step needs it at.
#[must_use]
pub fn least_privilege(steps: &[Step], catalog: &ActionCatalog) -> Vec<Permission> {
    let mut out = Vec::new();
    collect(steps, catalog, &mut out);
    out
}

/// The grant rules for `permissions`.
#[must_use]
pub fn rules(permissions: &[Permission]) -> Vec<PermissionProfileRule> {
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
    let requirement = row
        .and_then(|row| row.action.as_ref())
        .map_or(PermissionRequirement::Write, |descriptor| {
            descriptor.contract.permission
        });
    let verb = required_verb(requirement);
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
        if verb.implies(existing.verb) {
            existing.verb = verb;
        }
        return;
    }
    out.push(Permission {
        app_id: app_id.to_owned(),
        reach,
        action: action.to_owned(),
        verb,
        label,
    });
}
