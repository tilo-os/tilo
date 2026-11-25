//! What may be published outside a document's own actions: views and
//! effects, never a document mutation, which reaches the document through
//! the resource's own action.

use crate::{ActionSpec, OperationKind};

/// Whether `operation` may be offered outside the document's own actions: a
/// view or effect operation, never a document mutation, which is the
/// resource's `Action` and reaches the document through it.
///
/// # Panics
/// In a debug build, when `operation` is a document mutation.
#[must_use]
pub fn admits_outside_document(operation: &ActionSpec, on: &str) -> bool {
    let admitted = operation.kind != OperationKind::Document;
    debug_assert!(
        admitted,
        "`{on}` offers document action `{}` outside the resource's actions; \
         a document change is the resource's Action through its Dispatcher",
        operation.id
    );
    admitted
}

/// `resource` without document actions, at any depth: what a widget below its
/// component's root may publish.
#[must_use]
pub fn outside_document(mut resource: crate::ResourceSpec) -> crate::ResourceSpec {
    let on = resource.id.as_str().to_owned();
    resource
        .actions
        .retain(|action| admits_outside_document(action, &on));
    resource.children = resource
        .children
        .into_iter()
        .map(outside_document)
        .collect();
    resource
}

/// `resource`, resolved by id below its component's root, without document
/// actions.
#[must_use]
pub fn resolved_outside_document(mut resource: crate::ResourceSpec) -> crate::ResourceSpec {
    resource
        .actions
        .retain(|action| action.kind != OperationKind::Document);
    resource.children = resource
        .children
        .into_iter()
        .map(resolved_outside_document)
        .collect();
    resource
}
