//! A resource's genesis. An app's record holds its content alone, and the workspace's store keeps
//! who it is ([`content_genesis`]); a record the device keeps for the workspace itself names its
//! id, kind, home and workspace in its meta map ([`collab_types::document::identity`]).

#[cfg(any(test, feature = "test-support"))]
pub mod conformance;
mod documents;
pub mod transfer;
pub use documents::*;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
pub use collab_types::document::{
    DocumentCheckout, DocumentId, DocumentRef, MAX_DOCUMENT_RELATION_BYTES, MAX_FORMER_IDS,
    ResourceType, valid_label,
};
use tilo_automerge::ApplyIncoming;

use crate::document::fields::{META, ObjectAt, StringAt, meta_ro, object_at};
pub use collab_types::document::identity::{
    FORMERLY, SURFACE, WORKSPACE, commit_genesis, former_identities, home, identity, init,
    init_scoped, meta_obj, primary_document, stamp_scoped, stamp_surface, stored_workspace, str_at,
    workspace_scope,
};

/// Key, in a workspace's index document, of the map of resource references.
pub const INDEX: &str = "tilo:records";

#[derive(Debug, thiserror::Error)]
pub enum ResourceOpenError {
    #[error("record identity does not match its binding")]
    IdentityMismatch,
    #[error("record does not exist")]
    MissingResource,
    #[error("record is reserved or already initialized")]
    Busy,
    #[error("the new record identity already exists or is reserved")]
    AlreadyExists,
    #[error("record initialization failed: {0}")]
    Initialization(String),
    /// The workspace's one store, which holds every record, did not answer the read: the service
    /// on this device is down or slow. Nothing opens in the record's place, so nothing anyone
    /// types is lost.
    #[error("the workspace did not answer: {0}")]
    Unreachable(String),
    /// A resource the workspace sends only once asked is not here yet: it was asked for, and the
    /// store wakes its readers when it arrives or when the workspace's devices say they have none of
    /// it.
    #[error("the record is on its way from the workspace")]
    Arriving,
    /// The resource is another person's own: the workspace's devices send it to that person's devices
    /// alone, and nobody else opens it.
    #[error("this is someone else's own record, which only they open")]
    NotYours,
}

/// Whether `doc` is the live resource `want` names in `workspace`.
///
/// # Errors
/// It is another resource, or of another workspace.
pub fn validate_identity(
    doc: &AutoCommit,
    want: &DocumentCheckout,
    workspace: &str,
) -> Result<(), ResourceOpenError> {
    match object_at(doc, &ROOT, META, ObjType::Map) {
        ObjectAt::Object(metadata) => {
            if matches!(
                object_at(doc, &metadata, "refs", ObjType::List),
                ObjectAt::Invalid
            ) {
                return Err(ResourceOpenError::IdentityMismatch);
            }
        }
        ObjectAt::Missing if want.id.is_package_primary() => {}
        ObjectAt::Missing | ObjectAt::Invalid => {
            return Err(ResourceOpenError::IdentityMismatch);
        }
    }
    if want.id.is_package_primary() {
        return if primary_document(doc)
            && !want.kind.as_str().is_empty()
            && !workspace.is_empty()
            && workspace_scope(doc).is_none_or(|scope| scope == workspace)
        {
            Ok(())
        } else {
            Err(ResourceOpenError::IdentityMismatch)
        };
    }
    if identity(doc).is_none_or(|found| found.id != want.id || found.kind != want.kind)
        || want.kind.as_str().is_empty()
        || home(doc).is_none_or(|home| home.is_empty())
        || workspace_scope(doc).as_deref() != Some(workspace)
    {
        return Err(ResourceOpenError::IdentityMismatch);
    }
    Ok(())
}

impl From<crate::native_initial::ResourceReadError> for ResourceOpenError {
    fn from(error: crate::native_initial::ResourceReadError) -> Self {
        use crate::native_initial::ResourceReadError;
        match error {
            ResourceReadError::Arriving => Self::Arriving,
            ResourceReadError::Initialization(error) => Self::Initialization(error),
        }
    }
}

/// published under, as one container resource beside the app's own.
#[must_use]
pub fn children_resource_id(parent: &DocumentId, kind: &str) -> String {
    format!("children:{kind}@{}", parent.as_str())
}

/// The actor [`genesis`] makes resource `id` of `workspace` under.
pub fn genesis_actor_of(
    id: &DocumentId,
    kind: &ResourceType,
    workspace: &str,
) -> automerge::ActorId {
    // One derivation of a genesis actor for every writer of it: the store
    // here, the model modules a device executes, and a device that founds a
    // document it keeps for the workspace.
    collab_types::document::identity::genesis_actor(id, kind, workspace)
}

/// The actor `author`'s mount edits a resource as, when the resource's writer is `resource_writer` and
/// the device asked for per-author writers; mounts of one profile and device share a document, so
/// this keeps each author's edits apart. Unique per writer, and the same after a reopen, so history
/// names the same author.
pub fn author_writer(
    resource_writer: &automerge::ActorId,
    author: impl AsRef<[u8]>,
) -> automerge::ActorId {
    use sha2::Digest as _;
    let digest = sha2::Sha256::new()
        .chain_update(b"tilo.record.author-writer\0")
        .chain_update(resource_writer.to_bytes())
        .chain_update(b"\0")
        .chain_update(author.as_ref())
        .finalize();
    automerge::ActorId::from(digest.get(..16).unwrap_or_default())
}

/// The actor that made the genesis of the resource `doc` holds: nobody edits as it, and its changes
/// carry no time, so history lists none of them.
pub fn genesis_actor(doc: &AutoCommit) -> Option<automerge::ActorId> {
    let checkout = identity(doc)?;
    let home = str_at(doc, &meta_ro(doc)?, "home")?;
    Some(genesis_actor_of(&checkout.id, &checkout.kind, &home))
}

/// Bind a package-primary document to `workspace`, the namespace that owns it, as a resource's
/// identity binds it: the primary's genesis then holds a change whatever the app's `initialize`
/// writes. A primary whose state starts as its defaults would otherwise be founded as a document
/// with no change at all, which every other device takes for a resource nobody made: a window of
/// a peer that names it finds nothing to open.
fn scope_primary(doc: &mut AutoCommit, workspace: &str) -> bool {
    if workspace.is_empty() {
        return false;
    }
    let Some(m) = meta_obj(doc) else {
        return false;
    };
    match stored_workspace(doc, &m) {
        StringAt::Missing => doc.put(&m, WORKSPACE, workspace).is_ok(),
        StringAt::String(existing) => existing == workspace,
        StringAt::Invalid => false,
    }
}

/// Make `doc` an app's record as it is created: its derived founding, the containers its kind
/// keeps its items in ([`collab_types::document::founding`]), which every member creating the same
/// record makes alike and the store derives for the record its route names, then what the app's
/// `initialize` writes, as any change of the writer's.
/// Nothing of who the record is goes in: the workspace's store keeps that.
///
/// # Errors
/// The initializer failed.
pub fn content_genesis(
    doc: &mut AutoCommit,
    checkout: &DocumentCheckout,
    initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
) -> Result<(), ResourceOpenError> {
    let kind = checkout.kind.as_str();
    let containers = collab_types::app_surface::resource_surface(kind)
        .map(|surface| collab_types::document::founding::of_kind(&surface))
        .unwrap_or_default();
    let mut made = AutoCommit::new().with_actor(doc.get_actor().clone());
    if let Some(founding) =
        collab_types::document::founding::structure(kind, checkout.id.route(), &containers)
    {
        ApplyIncoming::apply_incoming(&mut made, vec![founding])
            .map_err(|error| ResourceOpenError::Initialization(error.to_string()))?;
    }
    initialize(&mut made).map_err(ResourceOpenError::Initialization)?;
    if made.pending_ops() != 0 {
        made.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Native.change_message()),
        );
    }
    *doc = made;
    Ok(())
}

/// Make `doc` the resource as it is created: its identity and the app's `initialize`d content in one
/// change, byte for byte the same on every peer. An actor derived from the resource's identity makes
/// it at time zero, so two members creating the same resource hash the same change.
pub fn genesis(
    doc: &mut AutoCommit,
    checkout: &DocumentCheckout,
    workspace: &str,
    initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
) -> Result<(), ResourceOpenError> {
    let actor = genesis_actor_of(&checkout.id, &checkout.kind, workspace);
    let mut made = AutoCommit::new().with_actor(actor.clone());
    for key in doc.keys(&ROOT).collect::<Vec<_>>() {
        if let Ok(Some((Value::Scalar(value), _))) = doc.get(&ROOT, key.as_str()) {
            made.put(&ROOT, key.as_str(), value.into_owned())
                .map_err(|error| ResourceOpenError::Initialization(error.to_string()))?;
        }
    }
    // Stamped, not committed: the identity is part of the one genesis change.
    let stamped = if checkout.id.is_package_primary() {
        init_scoped(
            &mut made,
            &checkout.id,
            &checkout.kind,
            workspace,
            Some(workspace),
        ) && scope_primary(&mut made, workspace)
    } else {
        collab_types::document::identity::stamp_founding(
            &mut made,
            &checkout.id,
            &checkout.kind,
            workspace,
            workspace,
        )
    };
    if !stamped {
        return Err(ResourceOpenError::IdentityMismatch);
    }
    initialize(&mut made).map_err(ResourceOpenError::Initialization)?;
    // The app's surface is the same wherever its package is installed: named
    // in the genesis, not in a change of this device's afterwards.
    let _ = stamp_surface(&mut made);
    // Every member must derive the same genesis, so the initializer may not
    // author it under an actor of its own.
    if made.get_actor() != &actor {
        return Err(ResourceOpenError::IdentityMismatch);
    }
    made.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message())
            .with_time(0),
    );
    let changes = made.get_changes(&[]);
    let mut created = AutoCommit::new().with_actor(doc.get_actor().clone());
    ApplyIncoming::apply_incoming(&mut created, changes)
        .map_err(|error| ResourceOpenError::Initialization(error.to_string()))?;
    *doc = created;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "6f1c1b62-4f5e-4b8a-9b52-1f8a3f0f3a11";
    const KIND: &str = "files.tree";

    /// An app's own record founds on the change the store derives for the record its route
    /// names, the empty one: the store refuses any other founding of it.
    #[test]
    fn an_apps_own_record_founds_as_the_store_derives_it_for_its_route() {
        const SURFACE: &[u8] = br#"format = 2

[[resources]]
kind = "test.founding.board"
label = "Board"
version = 1
fields = [{ path = "items", type = { list = "text" } }]
"#;
        collab_types::app_surface::register_package("test.founding", SURFACE);
        let kind = ResourceType::new("test.founding.board");
        let mut made = AutoCommit::new();
        content_genesis(&mut made, &DocumentCheckout::package_primary(kind), |_| {
            Ok(())
        })
        .expect("the record is founded");
        let derived = collab_types::document::founding::structure(
            "test.founding.board",
            "",
            &[("items".to_owned(), ObjType::List)],
        )
        .expect("the board keeps its items in a list");
        assert_eq!(made.get_heads(), [derived.hash()]);
    }

    fn tree() -> (DocumentId, ResourceType) {
        (
            DocumentId::parse(&collab_types::person_document_id(KIND)).expect("a document id"),
            ResourceType::new(KIND),
        )
    }

    #[test]
    fn a_resources_founding_is_the_genesis_its_first_content_is_made_with() {
        let (id, kind) = tree();
        let mut made = AutoCommit::new();
        genesis(
            &mut made,
            &DocumentCheckout::new(id.clone(), kind.clone()),
            WORKSPACE,
            |_| Ok(()),
        )
        .expect("the resource is founded");
        let mut founding =
            collab_types::document::identity::founding(&id, &kind, WORKSPACE, WORKSPACE)
                .expect("a document is founded");
        assert_eq!(
            made.get_heads(),
            founding.get_heads(),
            "a founder and an app making the resource of its first content make one change"
        );
    }

    #[test]
    fn every_founder_of_a_resource_derives_the_same_founding() {
        let (id, kind) = tree();
        let mut one = collab_types::document::identity::founding(&id, &kind, WORKSPACE, WORKSPACE)
            .expect("a document is founded");
        let mut other =
            collab_types::document::identity::founding(&id, &kind, WORKSPACE, WORKSPACE)
                .expect("a document is founded");
        assert_eq!(one.get_heads(), other.get_heads());
        assert_eq!(one.save(), other.save());
        let mut elsewhere = collab_types::document::identity::founding(
            &id,
            &kind,
            "0b8f7b53-5a6e-4f7b-9d7a-1c2f3b4e5d60",
            "0b8f7b53-5a6e-4f7b-9d7a-1c2f3b4e5d60",
        )
        .expect("a document is founded");
        assert_ne!(
            one.get_heads(),
            elsewhere.get_heads(),
            "another workspace's resource of the same kind is another identity"
        );
    }
}
