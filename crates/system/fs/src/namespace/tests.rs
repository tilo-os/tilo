//! The catalogue a rebuild gives, compared with the one a change kept.

use super::*;

pub(super) fn assert_indexes_match_rebuild(ns: &Namespace) {
    let mut rebuilt = ns.clone();
    rebuilt.rebuild_document_index();
    assert_eq!(ns.tree.placement, rebuilt.tree.placement, "placement");
    assert_eq!(ns.tree.children, rebuilt.tree.children, "children");
    assert_eq!(ns.tree.records, rebuilt.tree.records, "record bindings");
    assert_eq!(
        ns.tree.resource_of, rebuilt.tree.resource_of,
        "record owners"
    );
    assert_eq!(ns.document_index, rebuilt.document_index, "rows");
    assert_eq!(
        ns.document_index_digest, rebuilt.document_index_digest,
        "digest"
    );
    assert_eq!(
        ns.document_index_revision, rebuilt.document_index_revision,
        "revision"
    );
    assert_eq!(
        ns.document_index_ambiguous, rebuilt.document_index_ambiguous,
        "ambiguity"
    );
}
