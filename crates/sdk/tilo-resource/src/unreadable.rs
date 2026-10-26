//! What a resource publishes when the document it should read from does not
//! read: every action that would change it, and every action it declares,
//! says why it does not apply, instead of the surface showing a hole or the
//! last state that read as if it were what the record holds.

use crate::Resource;
use crate::resource::{apply_spec, declaration_spec, field_edit_spec, group_spec};
use crate::vocabulary::vocabulary;

/// The tree published for the resource `R` with id `id` and label `label`
/// whose document did not read, for `reason`: its declared actions, `apply`,
/// `group` and the field edit, each unavailable for that reason, so a caller
/// learns why they do not apply instead of finding a hole in the surface.
/// What a state would offer — its children, its views, which actions can be
/// taken now — is not published: there is no state to read it from.
pub(crate) fn parts<R: Resource>(id: &str, label: &str, reason: &str) -> tilo_actions::Resource {
    let mut resource = tilo_actions::Resource::new(id, R::KIND, label);
    let mut add = |spec: tilo_actions::ActionSpec| {
        resource.actions.push(withhold(spec, reason));
    };
    for decl in R::declarations() {
        add(declaration_spec(&decl));
    }
    let vocabulary = vocabulary::<R>();
    add(apply_spec(vocabulary));
    add(group_spec(vocabulary));
    if vocabulary.edits {
        add(field_edit_spec(vocabulary, Vec::new()));
    }
    resource
}

/// Every document action of `resource` and its children, unavailable for
/// `reason`: the record does not read as the resource, so none of them could
/// apply nor say what it holds.
pub fn withhold_document_actions(resource: &mut tilo_actions::Resource, reason: &str) {
    for spec in &mut resource.actions {
        if spec.kind == tilo_actions::OperationKind::Document {
            spec.contract.availability = unavailable(reason);
        }
    }
    for child in &mut resource.children {
        withhold_document_actions(child, reason);
    }
}

/// `spec` unavailable for `reason`, whatever it would otherwise do: the
/// record it changes does not read, so invoking it could neither apply the
/// change nor say what the record holds.
pub(crate) fn withhold(
    mut spec: tilo_actions::ActionSpec,
    reason: &str,
) -> tilo_actions::ActionSpec {
    spec.contract.availability = unavailable(reason);
    spec
}

fn unavailable(reason: &str) -> tilo_actions::Availability {
    tilo_actions::Availability::Unavailable {
        code: "unreadable".into(),
        reason: reason.to_owned(),
    }
}
