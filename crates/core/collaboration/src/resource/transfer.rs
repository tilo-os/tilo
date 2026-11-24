//! Transfer: the explicit verb that takes a resource across a workspace boundary.
//!
//! A resource id names a document only inside the workspace whose replica hydrates it
//! ([`super::workspace_scope`]), so nothing crosses workspaces by reference. Transfer is the
//! counterpart of duplicating a resource in a workspace index for a document that already has content: the
//! destination workspace receives the resource's whole change graph under a fresh id of its own,
//! scoped to that workspace, with the source id kept in its `formerly` list as provenance. Nothing
//! ever looks a resource up through that list.
//!
//! Moving is transfer followed by [`stamp_retired`]: the source resource gets one last change saying it
//! left, which stays in its history the way a deletion does, and it is never opened as a live
//! document again ([`super::ResourceOpenError::Retired`]). Copying is transfer alone: the source
//! keeps going and the two histories diverge from here. An app that keeps one document for itself
//! in a workspace (its resource index) transfers the same way ([`Origin::AppDocument`]).
//!
//! Every function here works on documents alone: a caller reads the source from its workspace and
//! writes what comes out to the destination's, both through the workspace's one store.

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{AutoCommit, ChangeHash, ObjType, ReadDoc, ScalarValue, Value};
use tilo_automerge::ApplyIncoming as _;

use super::{
    DocumentCheckout, DocumentId, FORMERLY, ObjectAt, ResourceOpenError, ResourceType, identity,
    meta_obj, meta_ro, object_at, validate_identity,
};

use crate::document::retirement::RETIRED;
pub use crate::document::retirement::{Retirement, Successor, is_retired, retirement};

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error(transparent)]
    Open(#[from] ResourceOpenError),
    #[error("{0}")]
    NotTransferable(&'static str),
    #[error("automerge: {0}")]
    Automerge(String),
}

/// Stamp `doc` retired, in one change of its own, unless it already is.
/// The shell does this to a resource at rest; an app does it to the live
/// document it shows, so the stamp travels to everyone holding that resource
/// over its normal route.
///
/// # Errors
/// A document whose metadata cannot hold the stamp.
pub fn stamp_retired(doc: &mut AutoCommit, why: &Retirement) -> Result<(), TransferError> {
    if is_retired(doc) {
        return Ok(());
    }
    let automerge = |error: automerge::AutomergeError| TransferError::Automerge(error.to_string());
    let meta = meta_obj(doc).ok_or(ResourceOpenError::IdentityMismatch)?;
    let stamp = doc
        .put_object(&meta, RETIRED, ObjType::Map)
        .map_err(automerge)?;
    doc.put(&stamp, "reason", why.reason()).map_err(automerge)?;
    if let Retirement::Moved(Some(successor)) = why {
        doc.put(&stamp, "workspace", successor.workspace.as_str())
            .map_err(automerge)?;
        doc.put(&stamp, "document", successor.document.as_str())
            .map_err(automerge)?;
        doc.put(&stamp, "by", successor.by.as_str())
            .map_err(automerge)?;
    }
    doc.commit_with(
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Transfer.change_message()),
    );
    Ok(())
}

/// What a transfer takes out of the source workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Record(DocumentCheckout),
    /// The document an app keeps for itself in a workspace, its resource index.
    /// It has no resource identity of its own there, and arrives in the
    /// destination as a resource of this kind.
    AppDocument(ResourceType),
}

impl Origin {
    /// Refuse anything but this origin's live document in `workspace`.
    pub fn validate(&self, doc: &AutoCommit, workspace: &str) -> Result<(), TransferError> {
        match self {
            Self::Record(checkout) => Ok(validate_identity(doc, checkout, workspace)?),
            Self::AppDocument(_) if is_retired(doc) => Err(ResourceOpenError::Retired.into()),
            Self::AppDocument(_) if identity(doc).is_some() => {
                Err(ResourceOpenError::IdentityMismatch.into())
            }
            Self::AppDocument(_) => Ok(()),
        }
    }

    /// The kind the document has, or takes once it arrives as a resource.
    pub fn kind(&self) -> &ResourceType {
        match self {
            Self::Record(checkout) => &checkout.kind,
            Self::AppDocument(kind) => kind,
        }
    }
}

/// Identity keys a resource's meta map may hold; anything else could claim
/// more than the one resource the document is.
const IMPORTABLE_META: [&str; 7] = [
    "id",
    "kind",
    "home",
    super::WORKSPACE,
    FORMERLY,
    "refs",
    "blobs",
];

/// Check `bytes` offered to become resource `id` of `workspace` with their whole
/// history, before anything keeps them: at most `max_bytes`, a complete
/// document whose identity is exactly that freshly minted resource scoped to
/// that workspace, live, holding no workspace index and no references to other
/// resources, and naming earlier ids only as provenance.
///
/// # Errors
/// Bytes that are too large, malformed, or not exactly that resource.
pub fn validate_import(
    bytes: &[u8],
    workspace: &str,
    id: &DocumentId,
    max_bytes: usize,
) -> Result<(AutoCommit, DocumentCheckout), TransferError> {
    if bytes.len() > max_bytes {
        return Err(TransferError::NotTransferable("the document is too large"));
    }
    let minted = id.as_str().strip_prefix("uuid:").is_some_and(|text| {
        uuid::Uuid::parse_str(text).is_ok_and(|uuid| uuid.hyphenated().to_string() == text)
    });
    if !minted || DocumentId::parse(id.as_str()).as_ref() != Some(id) {
        return Err(TransferError::NotTransferable(
            "a document joins a workspace only as a newly minted resource",
        ));
    }
    let doc =
        AutoCommit::load(bytes).map_err(|error| TransferError::Automerge(error.to_string()))?;
    let checkout = identity(&doc)
        .filter(|found| &found.id == id)
        .ok_or(ResourceOpenError::IdentityMismatch)?;
    validate_identity(&doc, &checkout, workspace)?;
    if super::workspace_scope(&doc).as_deref() != Some(workspace) {
        return Err(ResourceOpenError::IdentityMismatch.into());
    }
    if doc
        .get(automerge::ROOT, super::INDEX)
        .ok()
        .flatten()
        .is_some()
    {
        return Err(TransferError::NotTransferable(
            "a document listing other resources cannot join a workspace",
        ));
    }
    let meta = meta_ro(&doc).ok_or(ResourceOpenError::IdentityMismatch)?;
    if doc
        .keys(&meta)
        .any(|key| !IMPORTABLE_META.contains(&key.as_str()))
    {
        return Err(ResourceOpenError::IdentityMismatch.into());
    }
    if matches!(object_at(&doc, &meta, "refs", ObjType::List), ObjectAt::Object(list) if doc.length(&list) != 0)
    {
        return Err(TransferError::NotTransferable(
            "a document referring to other resources cannot join a workspace",
        ));
    }
    let formerly = match object_at(&doc, &meta, FORMERLY, ObjType::List) {
        ObjectAt::Object(list) => doc.length(&list),
        ObjectAt::Missing => 0,
        ObjectAt::Invalid => return Err(ResourceOpenError::IdentityMismatch.into()),
    };
    let provenance = provenance(&doc);
    if formerly > super::MAX_FORMER_IDS
        || provenance.len() != formerly
        || provenance.iter().any(|former| former == id)
    {
        return Err(ResourceOpenError::IdentityMismatch.into());
    }
    Ok((doc, checkout))
}

/// The destination's copy of `source`, read from `origin` in `from_workspace`:
/// its whole history, one more change giving it `target`'s id and
/// `to_workspace`'s scope, and a resource's origin id kept as provenance. `source`
/// is not changed.
pub fn rehome(
    source: &AutoCommit,
    origin: &Origin,
    from_workspace: &str,
    to_workspace: &str,
    target: &DocumentId,
) -> Result<AutoCommit, TransferError> {
    if target.is_package_primary()
        || matches!(origin, Origin::Record(checkout) if checkout.id.is_package_primary())
    {
        return Err(TransferError::NotTransferable(
            "a workspace's own package document never leaves its workspace",
        ));
    }
    if DocumentId::parse(target.as_str()).as_ref() != Some(target)
        || matches!(origin, Origin::Record(checkout) if &checkout.id == target)
    {
        return Err(ResourceOpenError::IdentityMismatch.into());
    }
    origin.validate(source, from_workspace)?;
    if from_workspace == to_workspace || to_workspace.is_empty() || from_workspace.is_empty() {
        return Err(TransferError::NotTransferable(
            "the resource is already in that workspace",
        ));
    }
    let automerge = |error: automerge::AutomergeError| TransferError::Automerge(error.to_string());
    // A fork continues the same change graph under a writer of its own.
    let mut doc = source.clone().fork();
    match origin {
        Origin::Record(checkout) => {
            let meta = meta_obj(&mut doc).ok_or(ResourceOpenError::IdentityMismatch)?;
            doc.put(&meta, "id", target.as_str()).map_err(automerge)?;
            doc.put(&meta, super::WORKSPACE, to_workspace)
                .map_err(automerge)?;
            let formerly = match object_at(&doc, &meta, FORMERLY, ObjType::List) {
                ObjectAt::Object(list) => list,
                ObjectAt::Missing => doc
                    .put_object(&meta, FORMERLY, ObjType::List)
                    .map_err(automerge)?,
                ObjectAt::Invalid => return Err(ResourceOpenError::IdentityMismatch.into()),
            };
            let len = doc.length(&formerly);
            doc.insert(&formerly, len, checkout.id.as_str())
                .map_err(automerge)?;
        }
        // The app's own document becomes a resource the moment it leaves: it
        // was minted in the source workspace and is now scoped to the destination.
        Origin::AppDocument(kind) => {
            if !super::init_scoped(&mut doc, target, kind, from_workspace, Some(to_workspace)) {
                return Err(ResourceOpenError::IdentityMismatch.into());
            }
        }
    }
    doc.commit_with(
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Transfer.change_message()),
    );
    let arrived = DocumentCheckout::new(target.clone(), origin.kind().clone());
    validate_identity(&doc, &arrived, to_workspace)?;
    Ok(doc)
}

/// A branch's copy of `source`, a document of a workspace branched into `to_workspace`: its whole
/// history, under a writer of its own, with one more change naming `to_workspace` as the workspace
/// it is in when it names one at all. Its id stays: a resource id names a document only inside its
/// workspace ([`super::workspace_scope`]), and a branch is a whole workspace forked, so every
/// reference one of its resources makes to another still names the same one. Where it was minted
/// (`home`) stays too, as its provenance, but for a resource any app makes from its kind alone,
/// which is minted again in the branch ([`minted_again`]). `source` is not changed.
///
/// # Errors
/// `to_workspace` is empty, or the document's metadata cannot name it.
pub fn branch(source: &AutoCommit, to_workspace: &str) -> Result<AutoCommit, TransferError> {
    if to_workspace.is_empty() {
        return Err(TransferError::NotTransferable(
            "a branch is made into a workspace",
        ));
    }
    // A fork continues the same change graph under a writer of its own, so a change made in the
    // branch never takes a number a change in the source already has.
    let mut doc = match minted_again(source, to_workspace)? {
        Some(mut minted) => minted.fork(),
        None => source.clone().fork(),
    };
    let Some(scoped) = super::workspace_scope(&doc) else {
        return Ok(doc);
    };
    if scoped == to_workspace {
        return Ok(doc);
    }
    let automerge = |error: automerge::AutomergeError| TransferError::Automerge(error.to_string());
    let meta = meta_obj(&mut doc).ok_or(ResourceOpenError::IdentityMismatch)?;
    doc.put(&meta, super::WORKSPACE, to_workspace)
        .map_err(automerge)?;
    doc.commit_with(
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Transfer.change_message()),
    );
    Ok(doc)
}

/// `source` minted again in `to_workspace`, when it is a resource any app makes from its kind alone
/// where it holds none ([`collab_types::workspace_document_id`]: the workspace's emoji, its agents)
/// and its genesis made it: its whole history, but with the genesis an app of the branch makes
/// ([`super::genesis`]) as its first change, and every change after it built on that one. An app of
/// the branch that opens the resource before the branch's copy reached it makes the same genesis, so
/// the two meet as one resource. `None` for any other document, which keeps where it was minted.
fn minted_again(
    source: &AutoCommit,
    to_workspace: &str,
) -> Result<Option<AutoCommit>, TransferError> {
    use automerge::legacy::{ElementId, Key, ObjectId, OpId, OpType};

    let Some(checkout) = identity(source) else {
        return Ok(None);
    };
    if checkout.id.as_str() != collab_types::workspace_document_id(checkout.kind.as_str()) {
        return Ok(None);
    }
    let Some(home) = super::home(source).filter(|home| home != to_workspace) else {
        return Ok(None);
    };
    let from = super::genesis_actor_of(&checkout.id, &checkout.kind, &home);
    let to = super::genesis_actor_of(&checkout.id, &checkout.kind, to_workspace);
    let changes = source.clone().get_changes(&[]);
    // Its genesis made it: the genesis actor's one change comes first and depends on nothing.
    let made_by_genesis = changes
        .first()
        .is_some_and(|first| first.actor_id() == &from && first.deps().is_empty())
        && changes
            .iter()
            .filter(|change| change.actor_id() == &from)
            .count()
            == 1;
    if !made_by_genesis {
        return Ok(None);
    }
    let rename = |id: &mut OpId| {
        if id.1 == from {
            id.1 = to.clone();
        }
    };
    let mut hashes = std::collections::HashMap::new();
    let mut minted = Vec::with_capacity(changes.len());
    for change in changes {
        let was = change.hash();
        let mut expanded = change.decode();
        expanded.hash = None;
        expanded.deps = expanded
            .deps
            .iter()
            .map(|dep| hashes.get(dep).copied())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| TransferError::Automerge(format!("change {was} before its base")))?;
        let genesis = expanded.actor_id == from;
        if genesis {
            expanded.actor_id = to.clone();
        }
        for op in &mut expanded.operations {
            if let ObjectId::Id(id) = &mut op.obj {
                rename(id);
            }
            if let Key::Seq(ElementId::Id(id)) = &mut op.key {
                rename(id);
            }
            op.pred = op
                .pred
                .iter()
                .cloned()
                .map(|mut pred| {
                    rename(&mut pred);
                    pred
                })
                .collect();
            // The genesis names the workspace that minted it, and the one it is in.
            let names_home = match (&op.key, &op.action) {
                (Key::Map(key), OpType::Put(ScalarValue::Str(named))) => {
                    genesis
                        && ["home", super::WORKSPACE].contains(&key.as_str())
                        && named.as_str() == home
                }
                _ => false,
            };
            if names_home {
                op.action = OpType::Put(ScalarValue::from(to_workspace));
            }
        }
        let change = automerge::Change::from(expanded);
        hashes.insert(was, change.hash());
        minted.push(change);
    }
    let mut doc = AutoCommit::new();
    doc.apply_incoming(minted)
        .map_err(|error| TransferError::Automerge(error.to_string()))?;
    if super::genesis_actor(&doc).as_ref() != Some(&to) {
        return Err(ResourceOpenError::IdentityMismatch.into());
    }
    validate_identity(&doc, &checkout, to_workspace)?;
    Ok(Some(doc))
}

/// The changes `source` holds beyond the `known` heads of an earlier read:
/// what its writer committed after that read.
pub fn changes_since(source: &AutoCommit, known: &[ChangeHash]) -> usize {
    source.clone().get_changes(known).len()
}

/// The origin ids a transferred resource came from, oldest first.
pub fn provenance(doc: &AutoCommit) -> Vec<DocumentId> {
    let Some(meta) = meta_ro(doc) else {
        return Vec::new();
    };
    let ObjectAt::Object(list) = object_at(doc, &meta, FORMERLY, ObjType::List) else {
        return Vec::new();
    };
    (0..doc.length(&list))
        .filter_map(|index| match doc.get(&list, index) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Str(id) => DocumentId::parse(id),
                _ => None,
            },
            _ => None,
        })
        .collect()
}
