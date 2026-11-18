//! How a message or a log names a resource: by its app, and by the id the app
//! keeps it under when it has one.

use tilo_peer_state::document_frame::DocumentRoute;

/// Whether the wire id `document` is a part of a chain an app keeps beside
/// its own ([`ui::resource::DocumentId::part`]): held with that document,
/// never on its own.
pub(super) fn is_part(document: &str) -> bool {
    ui::resource::DocumentId::parse(document).is_some_and(|id| id.chain_part().is_some())
}

/// How a person reads the resource `document` of `app` in a message or a
/// log: an app's own resource of the workspace travels with no id
/// ([`ui::resource::DocumentId::package_primary`]), so it is named for its app
/// rather than left blank.
pub(super) fn named_document(app: &str, document: &str) -> String {
    if document.is_empty() {
        format!("{app}'s own resource")
    } else {
        format!("{app} resource {document}")
    }
}

/// [`named_document`] of the document at `route`.
pub(super) fn named(route: &DocumentRoute) -> String {
    named_document(&route.app_id, &route.document)
}
