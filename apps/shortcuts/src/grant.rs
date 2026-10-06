//! The least authority a shortcut needs.
//!
//! A shortcut acts with the intersection of its grant and the authority of
//! whoever runs it. The grant the editor proposes names exactly the actions
//! the steps invoke, each at the weakest verb its contract requires, so the
//! person reviewing it sees everything the shortcut may do and nothing more.

use collab_types::{PermissionProfileRule, Scope, Verb};
use tilo_automation::{
    AGENT_APP_ID, AGENT_TRANSFORM_KIND, ActionCatalog, ResourceTarget, Selector, Step, StepKind,
    required_verb,
};
use ui::tilo_actions::PermissionRequirement;

/// One permission the shortcut will use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permission {
    pub app_id: String,
    pub action: String,
    pub verb: Verb,
    /// How the catalogue names it: "List notes in Inbox".
    pub label: String,
}

impl Permission {
    #[must_use]
    pub fn rule(&self) -> PermissionProfileRule {
        PermissionProfileRule::grant(
            Scope::Action {
                app: self.app_id.clone(),
                action: self.action.clone(),
            },
            self.verb,
        )
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
                app_id,
                resource,
                action,
                ..
            } => {
                let target = match resource {
                    Selector::Fixed(id) => ResourceTarget::Id(id),
                    Selector::FirstOfKind(kind) => ResourceTarget::Kind(kind),
                    Selector::FromBinding(_) => ResourceTarget::Any,
                };
                add(catalog, app_id, target, action, out);
            }
            StepKind::Ai { transform, .. } => add(
                catalog,
                AGENT_APP_ID,
                ResourceTarget::Kind(AGENT_TRANSFORM_KIND),
                transform.action(),
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
    app_id: &str,
    target: ResourceTarget<'_>,
    action: &str,
    out: &mut Vec<Permission>,
) {
    let row = catalog.find(app_id, target, action);
    let requirement = row
        .and_then(|row| row.action.as_ref())
        .map_or(PermissionRequirement::Write, |descriptor| {
            descriptor.contract.permission
        });
    let verb = required_verb(requirement);
    let label = row.and_then(|row| row.action.as_ref()).map_or_else(
        || format!("{action} in {}", crate::editor::app_label(app_id)),
        |descriptor| {
            format!(
                "{} in {}",
                descriptor.action_label, descriptor.resource_label
            )
        },
    );
    if let Some(existing) = out
        .iter_mut()
        .find(|existing| existing.app_id == app_id && existing.action == action)
    {
        if verb.implies(existing.verb) {
            existing.verb = verb;
        }
        return;
    }
    out.push(Permission {
        app_id: app_id.to_owned(),
        action: action.to_owned(),
        verb,
        label,
    });
}
