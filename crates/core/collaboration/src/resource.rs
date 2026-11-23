//! A resource's references and its genesis, over the identity its document holds: the meta map
//! naming its id, kind, home and workspace is [`collab_types::document::identity`], read and
//! stamped alike by a server, a peer, a window and a mounted folder's daemon.

mod store;
pub mod transfer;
pub use store::*;

use std::rc::Rc;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
pub use collab_types::document::{
    DocumentCheckout, DocumentId, DocumentRef, MAX_DOCUMENT_RELATION_BYTES, MAX_FORMER_IDS,
    ResourceType, valid_label,
};
use durable_store::{DurableStore, StorageKey};
use tilo_automerge::ApplyIncoming;

use crate::document::fields::{META, ObjectAt, StringAt, meta_ro, object_at};
pub use collab_types::document::identity::{
    FORMERLY, SURFACE, WORKSPACE, commit_genesis, former_identities, home, identity, init,
    init_scoped, meta_obj, primary_document, stamp_scoped, stamp_surface, stored_workspace, str_at,
    workspace_scope,
};

/// Key, in a workspace's index document, of the map of resource references.
pub const INDEX: &str = "tilo:records";

/// Give an adopted document `want`'s identity: the stamped id becomes the new one, the old is
/// remembered, and references to a former identity of a declared reference target now name that
/// target. Untouched when it isn't `former`'s document of `want`'s kind.
pub fn restamp(doc: &mut AutoCommit, former: &DocumentId, want: &DocumentCheckout) -> bool {
    if identity(doc).is_none_or(|found| found.id != *former || found.kind != want.kind) {
        return false;
    }
    let Some(m) = meta_obj(doc) else {
        return false;
    };
    let _ = doc.put(&m, "id", want.id.as_str());
    let list = match doc.get(&m, FORMERLY) {
        Ok(Some((Value::Object(ObjType::List), list))) => Some(list),
        _ => doc.put_object(&m, FORMERLY, ObjType::List).ok(),
    };
    if let Some(list) = list {
        let n = doc.length(&list);
        let _ = doc.insert(&list, n, former.as_str());
    }
    let Some(refs) = refs_obj(doc) else {
        return true;
    };
    for i in (0..doc.length(&refs)).rev() {
        let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&refs, i) else {
            continue;
        };
        let (Some(target), Some(relation)) = (
            str_at(doc, &entry, "target"),
            str_at(doc, &entry, "relation"),
        ) else {
            continue;
        };
        let Some((successor, _)) = want
            .references
            .iter()
            .find(|(declared, declared_relation)| {
                *declared_relation == relation
                    && declared.formerly.iter().any(|f| f.as_str() == target)
            })
        else {
            continue;
        };
        let successor_ref = DocumentRef::new(successor.id.clone(), relation);
        if references(doc).contains(&successor_ref) {
            let _ = doc.delete(&refs, i);
        } else {
            let _ = doc.put(&entry, "target", successor.id.as_str());
        }
    }
    true
}

/// Adopt the stored document of the first former identity of `want` that exists, belongs to its
/// kind and workspace, and whose ownership this successor can claim. The former document is only
/// read: the copy is restamped and returned to persist under the new id.
pub fn adopt_former(
    store: &Rc<dyn DurableStore>,
    want: &DocumentCheckout,
    workspace: &str,
    key: impl Fn(&DocumentId) -> StorageKey,
    mut load: impl FnMut(&StorageKey) -> Result<Option<AutoCommit>, app_state::StoreError>,
) -> Result<Option<AutoCommit>, app_state::StoreError> {
    for former in &want.formerly {
        let former_key = key(former);
        if store
            .read(&former_key)?
            .is_none_or(|stored| stored.bytes.is_empty())
        {
            continue;
        }
        let Some(mut doc) = load(&former_key)? else {
            continue;
        };
        if workspace_scope(&doc).is_some_and(|scope| scope != workspace)
            || identity(&doc).is_none_or(|found| found.id != *former || found.kind != want.kind)
        {
            tracing::warn!(resource = %want.id, former = %former, "former resource does not match; not adopting it");
            continue;
        }
        // First successor wins, durably: another resource naming the same former identity later must
        // never receive these edits as well.
        if !store.claim(&former_key, &want.id.storage_key())? {
            tracing::info!(resource = %want.id, former = %former, "former resource already adopted by another resource");
            continue;
        }
        if !restamp(&mut doc, former, want) {
            continue;
        }
        tracing::info!(resource = %want.id, former = %former, workspace, "adopting a resource under its new identity");
        return Ok(Some(doc));
    }
    Ok(None)
}

/// `None` only if the containing document's root/meta object is gone; see
/// [`meta_obj`].
fn refs_obj(doc: &mut AutoCommit) -> Option<ObjId> {
    let m = meta_obj(doc)?;
    match object_at(doc, &m, "refs", ObjType::List) {
        ObjectAt::Missing => doc.put_object(&m, "refs", ObjType::List).ok(),
        ObjectAt::Object(object) => Some(object),
        ObjectAt::Invalid => None,
    }
}

/// Idempotent on `(target, relation)`: a reference is a fact, not an
/// event.
pub fn add_reference(doc: &mut AutoCommit, r: &DocumentRef) -> bool {
    if DocumentId::parse(r.target.as_str()).is_none()
        || !valid_label(&r.relation, MAX_DOCUMENT_RELATION_BYTES)
    {
        return false;
    }
    if references(doc).iter().any(|e| e == r) {
        return false;
    }
    let Some(list) = refs_obj(doc) else {
        return false;
    };
    let n = doc.length(&list);
    let Ok(entry) = doc.insert_object(&list, n, ObjType::Map) else {
        return false;
    };
    let _ = doc.put(&entry, "target", r.target.as_str());
    let _ = doc.put(&entry, "relation", r.relation.as_str());
    true
}

/// Every resource this one references.
pub fn references(doc: &AutoCommit) -> Vec<DocumentRef> {
    let Some(m) = meta_ro(doc) else {
        return Vec::new();
    };
    let ObjectAt::Object(list) = object_at(doc, &m, "refs", ObjType::List) else {
        return Vec::new();
    };
    (0..doc.length(&list))
        .filter_map(|i| {
            let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&list, i) else {
                return None;
            };
            let target = DocumentId::parse(&str_at(doc, &entry, "target")?)?;
            let relation = str_at(doc, &entry, "relation")?;
            if !valid_label(&relation, MAX_DOCUMENT_RELATION_BYTES) {
                return None;
            }
            Some(DocumentRef { target, relation })
        })
        .collect()
}

/// A content-addressed blob a resource depends on: immutable bytes its content is about or embeds
/// (the PDF an annotation set marks up, an image a document shows). Kept in the resource's metadata,
/// so sync, retention and history see which blobs a resource keeps alive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobRef {
    /// Lowercase hex BLAKE3 of the bytes — the blob store's key.
    pub hash: String,
    pub len: u64,
    pub mime: String,
    /// What the blob is to the resource — `source`, `embedded`, `import`.
    pub relation: String,
}

fn valid_blob_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn blobs_obj(doc: &mut AutoCommit) -> Option<ObjId> {
    let m = meta_obj(doc)?;
    match doc.get(&m, "blobs") {
        Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
        _ => doc.put_object(&m, "blobs", ObjType::List).ok(),
    }
}

/// Resource that this resource depends on `blob`. Idempotent on
/// `(hash, relation)`. Returns whether a reference was added.
pub fn add_blob_reference(doc: &mut AutoCommit, blob: &BlobRef) -> bool {
    if !valid_blob_hash(&blob.hash)
        || !valid_label(&blob.relation, MAX_DOCUMENT_RELATION_BYTES)
        || blob.mime.len() > MAX_DOCUMENT_RELATION_BYTES
    {
        return false;
    }
    if blob_references(doc)
        .iter()
        .any(|b| b.hash == blob.hash && b.relation == blob.relation)
    {
        return false;
    }
    let Some(list) = blobs_obj(doc) else {
        return false;
    };
    let n = doc.length(&list);
    let Ok(entry) = doc.insert_object(&list, n, ObjType::Map) else {
        return false;
    };
    let _ = doc.put(&entry, "hash", blob.hash.as_str());
    let _ = doc.put(&entry, "len", i64::try_from(blob.len).unwrap_or(i64::MAX));
    let _ = doc.put(&entry, "mime", blob.mime.as_str());
    let _ = doc.put(&entry, "relation", blob.relation.as_str());
    true
}

/// Every blob this resource depends on.
pub fn blob_references(doc: &AutoCommit) -> Vec<BlobRef> {
    let Some(m) = meta_ro(doc) else {
        return Vec::new();
    };
    let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&m, "blobs") else {
        return Vec::new();
    };
    (0..doc.length(&list))
        .filter_map(|i| {
            let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&list, i) else {
                return None;
            };
            let hash = str_at(doc, &entry, "hash").filter(|h| valid_blob_hash(h))?;
            let len = match doc.get(&entry, "len") {
                Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                    ScalarValue::Int(n) => u64::try_from(*n).ok(),
                    ScalarValue::Uint(n) => Some(*n),
                    _ => None,
                },
                _ => None,
            }?;
            Some(BlobRef {
                hash,
                len,
                mime: str_at(doc, &entry, "mime").unwrap_or_default(),
                relation: str_at(doc, &entry, "relation")?,
            })
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum ResourceOpenError {
    #[error(transparent)]
    Store(app_state::StoreError),
    #[error("record identity does not match its binding")]
    IdentityMismatch,
    #[error("record does not exist")]
    MissingResource,
    #[error("record is reserved or already initialized")]
    Busy,
    /// The store's own thread is still opening the resource's document for a thread that must not
    /// wait on storage; that thread's wake is called when it is done.
    #[error("the document is still opening")]
    Opening,
    #[error("the new record identity already exists or is reserved")]
    AlreadyExists,
    #[error("record initialization failed: {0}")]
    Initialization(String),
    /// A resource the workspace sends only once asked is not here yet: it was asked for, and the
    /// store wakes its readers when it arrives or when the workspace's devices say they have none of
    /// it.
    #[error("the record is on its way from the workspace")]
    Arriving,
    /// The resource is another person's own: the workspace's devices send it to that person's devices
    /// alone, and nobody else opens it.
    #[error("this is someone else's own record, which only they open")]
    NotYours,
    /// The resource left this workspace (see [`transfer`]); its history stays, but
    /// it is never opened as a live document again.
    #[error("record was moved out of this workspace")]
    Retired,
}

impl ResourceOpenError {
    /// Whether another process was writing the store at this moment: the resource is not one this
    /// device cannot open, just one it did not open this time, and opening it again once that write
    /// is done succeeds.
    #[must_use]
    pub fn writer_busy(&self) -> bool {
        matches!(self, Self::Store(app_state::StoreError::WriterBusy { .. }))
    }
}

impl From<app_state::StoreError> for ResourceOpenError {
    fn from(error: app_state::StoreError) -> Self {
        match error {
            app_state::StoreError::Opening(_) => Self::Opening,
            other => Self::Store(other),
        }
    }
}

impl From<durable_store::StoreError> for ResourceOpenError {
    fn from(error: durable_store::StoreError) -> Self {
        Self::from(app_state::StoreError::from(error))
    }
}

/// The stable namespace string of a portable cache, shared by resource sets and

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
    if crate::document::retirement::is_retired(doc) {
        return Err(ResourceOpenError::Retired);
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
