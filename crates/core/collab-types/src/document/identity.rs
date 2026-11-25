//! A resource's identity, as its document holds it: the meta map naming its id, kind, home and
//! workspace, the surface its changes are decided against, and the stamp idempotent enough that a
//! reopen authors nothing. This is model state, not UI state: a server, a peer, a window and a
//! mounted folder's daemon read and stamp it alike, so it lives beside the documents it describes
//! rather than in the store that keeps them.

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::document::fields::{META, ObjectAt, StringAt, meta_ro, object_at, string_at};
use crate::document::{DocumentCheckout, DocumentId, ResourceType};

/// Key in a resource document for the workspace that owns this local replica.
pub const WORKSPACE: &str = "workspace";

/// Key, in a resource's meta map, of the identities it was adopted from.
pub const FORMERLY: &str = "formerly";

/// Select the document's metadata map, or create it only when it is absent.
pub fn meta_obj(doc: &mut AutoCommit) -> Option<ObjId> {
    match object_at(doc, &ROOT, META, ObjType::Map) {
        ObjectAt::Missing => doc.put_object(&ROOT, META, ObjType::Map).ok(),
        ObjectAt::Object(object) => Some(object),
        ObjectAt::Invalid => None,
    }
}

pub fn str_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match string_at(doc, obj, key) {
        StringAt::String(value) => Some(value),
        StringAt::Missing | StringAt::Invalid => None,
    }
}

/// The workspace a resource document is stamped with. A value that is not a name is refused.
pub fn stored_workspace(doc: &AutoCommit, meta: &ObjId) -> StringAt {
    string_at(doc, meta, WORKSPACE)
}

/// Stamp a document with the identity of the resource it holds; idempotent, so a reopen does not
/// author a change graph entry that says nothing new.
pub fn init(doc: &mut AutoCommit, id: &DocumentId, kind: &ResourceType, home: &str) {
    let _ = init_scoped(doc, id, kind, home, None);
}

/// Bind a resource document to `workspace`, never rewriting an existing identity or workspace: a
/// document stamped for another workspace would turn a matching resource id into an alias across
/// workspaces. A document naming none may be adopted once, after its identity is checked.
pub fn init_scoped(
    doc: &mut AutoCommit,
    id: &DocumentId,
    kind: &ResourceType,
    home: &str,
    workspace: Option<&str>,
) -> bool {
    // Primary ownership is the checked app/workspace namespace, never a fake
    // resource stamp. Keep this serialized internal id stable across devices.
    if id.is_package_primary() {
        return primary_document(doc);
    }
    if DocumentId::parse(id.as_str()).is_none() || !kind.is_valid() {
        return false;
    }
    // A stamp written on its own is the resource's genesis and is committed as one; with other
    // pending operations it is part of the change those make, which names itself.
    let alone = doc.pending_ops() == 0;
    let stamped = stamp_scoped(doc, id, kind, home, workspace);
    if alone && stamped && doc.pending_ops() != 0 {
        commit_genesis(doc);
    } else if alone && stamped && stamp_surface(doc).is_some() {
        // The app opens a resource its surface did not name yet (a newer package, or a
        // resource from before resources named one): its surface decides the resource's changes from here
        // on.
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Upgrade.change_message()),
        );
    }
    stamped
}

pub fn stamp_scoped(
    doc: &mut AutoCommit,
    id: &DocumentId,
    kind: &ResourceType,
    home: &str,
    workspace: Option<&str>,
) -> bool {
    let Some(m) = meta_obj(doc) else {
        // The document's root object is gone — nothing to stamp.
        return false;
    };
    let existing_id = string_at(doc, &m, "id");
    let existing_kind = string_at(doc, &m, "kind");
    let existing_home = string_at(doc, &m, "home");
    let existing_workspace = stored_workspace(doc, &m);
    let existing_refs = object_at(doc, &m, "refs", ObjType::List);
    if matches!(&existing_id, StringAt::String(existing) if existing != id.as_str())
        || matches!(&existing_id, StringAt::Invalid)
        || matches!(&existing_kind, StringAt::String(existing) if !kind.names(existing))
        || matches!(&existing_kind, StringAt::Invalid)
        || matches!(&existing_home, StringAt::String(existing) if existing.is_empty())
        || matches!(&existing_home, StringAt::Invalid)
        || matches!(&existing_workspace, StringAt::Invalid)
        || workspace.is_some_and(
            |scope| matches!(&existing_workspace, StringAt::String(existing) if existing != scope),
        )
        || matches!(&existing_refs, ObjectAt::Invalid)
    {
        return false;
    }
    if matches!(&existing_id, StringAt::Missing) {
        let _ = doc.put(&m, "id", id.as_str());
    }
    if matches!(&existing_kind, StringAt::Missing) {
        let _ = doc.put(&m, "kind", kind.as_str());
    }
    // Home is the workspace that minted the resource; never rewritten on a
    // later open, or the last workspace to open it would become the owner.
    if matches!(&existing_home, StringAt::Missing) {
        let _ = doc.put(&m, "home", home);
    }
    // A document that already names a workspace under either key keeps it: nothing here rewrites a
    // resource's workspace, or moves it to the key this build writes.
    if let Some(workspace) = workspace
        && matches!(&existing_workspace, StringAt::Missing)
    {
        let _ = doc.put(&m, WORKSPACE, workspace);
    }
    if matches!(&existing_refs, ObjectAt::Missing) {
        let _ = doc.put_object(&m, "refs", ObjType::List);
    }
    true
}

/// The entry of a branch's founding step on the record `id` of `kind`: the framework copies a
/// record with its history into a branch under it, naming the store's built-in branch verb and
/// nobody's action, so a holder judges the step as the fork's, and history reads it as one.
#[must_use]
pub fn branch_step(id: &str, kind: &str, label: &str) -> tilo_actions::ActionContext {
    tilo_actions::ActionContext {
        change: tilo_actions::ChangeKind::Housekeeping,
        ..tilo_actions::ActionContext::for_resource(id, kind, label).with_described(
            tilo_actions::Described::new(tilo_actions::Did::verb(
                tilo_actions::ActionKey::new(
                    String::new(),
                    kind,
                    tilo_actions::stamp::Stamp::BRANCH_VERB,
                ),
                [],
            )),
        )
    }
}

/// Commit what a brand-new resource starts with (its identity, references and
/// first content) as its genesis, which names itself as such.
pub fn commit_genesis(doc: &mut AutoCommit) {
    let _ = stamp_surface(doc);
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
    );
}

/// The actor every founder of the resource `id` of `kind` in `home` makes its genesis under:
/// one derivation, so a page's Files and the device keeping the workspace make the same change.
#[must_use]
pub fn genesis_actor(id: &DocumentId, kind: &ResourceType, home: &str) -> automerge::ActorId {
    tilo_resource::genesis::actor(id.as_str(), kind.as_str(), home)
}

/// Stamp `doc` with what founds `id` of `kind` in `workspace`: its identity, its workspace scope
/// and its app's surface. The caller commits it as the document's genesis ([`founding`]), or as
/// part of the one change a resource's creation is. Whether it is a resource that may be founded.
pub fn stamp_founding(
    doc: &mut AutoCommit,
    id: &DocumentId,
    kind: &ResourceType,
    home: &str,
    workspace: &str,
) -> bool {
    DocumentId::parse(id.as_str()).is_some()
        && kind.is_valid()
        && stamp_scoped(doc, id, kind, home, Some(workspace))
}

/// The document that founds `id` of `kind` in `workspace`: its identity and its app's surface
/// in one change, made under [`genesis_actor`] at time zero with nothing before it. Every
/// founder of a resource derives the same change, so a page's window and the device that starts
/// it as it writes the first content make one document; a founder with a genesis of its own
/// wrote the identity a second time, and the merged document then holds two of them, which reads
/// as none at all. What a founder puts there follows as its own change.
#[must_use]
pub fn founding(
    id: &DocumentId,
    kind: &ResourceType,
    home: &str,
    workspace: &str,
) -> Option<AutoCommit> {
    let surface = kind_surface(kind);
    founding_naming(id, kind, home, workspace, surface.as_deref())
}

/// The surface hash of the app that publishes `kind` here, as a founding names it.
fn kind_surface(kind: &ResourceType) -> Option<String> {
    crate::app_surface::hash_of_kind(kind.as_str()).map(hex::encode)
}

/// [`founding`], naming `surface` as its app's surface: the one a founder named, which a
/// receiver checking the founding reads from the change rather than from what it runs.
fn founding_naming(
    id: &DocumentId,
    kind: &ResourceType,
    home: &str,
    workspace: &str,
    surface: Option<&str>,
) -> Option<AutoCommit> {
    if DocumentId::parse(id.as_str()).is_none() || !kind.is_valid() {
        return None;
    }
    let mut doc = AutoCommit::new().with_actor(genesis_actor(id, kind, home));
    if !stamp_founding(&mut doc, id, kind, home, workspace) {
        return None;
    }
    if let Some(surface) = surface {
        let m = meta_ro(&doc)?;
        doc.put(&m, SURFACE, surface).ok()?;
    }
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message())
            .with_time(0),
    );
    Some(doc)
}

/// Whether `change`, loaded alone as `doc`, is exactly the [`founding`] of the resource it
/// names: the identity, home, workspace and surface it writes, founded as every founder founds
/// them. A change that writes anything else, or is made by another writer, is not.
#[must_use]
pub fn is_derived_founding(doc: &AutoCommit, change: &automerge::Change) -> bool {
    let Some(m) = meta_ro(doc) else {
        return false;
    };
    let (Some(id), Some(kind), Some(home), Some(workspace)) = (
        str_at(doc, &m, "id").and_then(|id| DocumentId::parse(&id)),
        str_at(doc, &m, "kind").map(ResourceType::new),
        str_at(doc, &m, "home"),
        workspace_scope(doc),
    ) else {
        return false;
    };
    let surface = str_at(doc, &m, SURFACE);
    founding_naming(&id, &kind, &home, &workspace, surface.as_deref())
        .is_some_and(|mut founded| founded.get_heads() == [change.hash()])
}

/// The [`founding`] of `id` of `kind` in `workspace` that `doc` lacks, when `doc` holds that
/// resource as a build wrote it before resources named themselves: content, and no meta map at
/// all. Merged into `doc`, it names the document exactly as a founder on this build names it.
/// Every holder derives the same change, so two holders naming one document at once write one
/// identity: two meta maps made apart conflict, and a conflicting meta map names nothing
/// ([`object_at`]). `None` for an empty document, and for one that has a meta map, which this
/// never rewrites.
#[must_use]
pub fn founding_for_unnamed(
    doc: &AutoCommit,
    id: &DocumentId,
    kind: &ResourceType,
    workspace: &str,
) -> Option<AutoCommit> {
    if doc.keys(&ROOT).next().is_none()
        || !matches!(object_at(doc, &ROOT, META, ObjType::Map), ObjectAt::Missing)
    {
        return None;
    }
    founding(id, kind, workspace, workspace)
}

/// The key of the resource identity naming the surface its changes are
/// decided against: the content hash of its app's surface in the form it is
/// kept by.
pub const SURFACE: &str = "surface";

/// Name, in the resource's identity, the surface of its app running here: `Some(true)` when it
/// replaced another app surface, `Some(false)` when it named the first, `None` when nothing
/// changed.
pub fn stamp_surface(doc: &mut AutoCommit) -> Option<bool> {
    let m = meta_ro(doc)?;
    let kind = str_at(doc, &m, "kind")?;
    let hash = hex::encode(crate::app_surface::hash_of_kind(&kind)?);
    match str_at(doc, &m, SURFACE) {
        Some(named) if named == hash => None,
        named => {
            doc.put(&m, SURFACE, hash).ok()?;
            Some(named.is_some())
        }
    }
}

/// The identity stamped into `doc`, if it holds a resource.
pub fn identity(doc: &AutoCommit) -> Option<DocumentCheckout> {
    let m = meta_ro(doc)?;
    let id = DocumentId::parse(&str_at(doc, &m, "id")?)?;
    let kind = ResourceType::new(str_at(doc, &m, "kind")?);
    let checkout = DocumentCheckout::new(id, kind);
    checkout.is_valid().then_some(checkout)
}

/// Identities this resource's document was known by before adoption, oldest
/// first (see [`DocumentCheckout::formerly`]).
pub fn former_identities(doc: &AutoCommit) -> Vec<DocumentId> {
    let Some(m) = meta_ro(doc) else {
        return Vec::new();
    };
    let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&m, FORMERLY) else {
        return Vec::new();
    };
    (0..doc.length(&list))
        .filter_map(|i| match doc.get(&list, i) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) => DocumentId::parse(s),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The workspace this resource was minted in.
pub fn home(doc: &AutoCommit) -> Option<String> {
    let m = meta_ro(doc)?;
    str_at(doc, &m, "home")
}

/// The workspace this local replica is bound to, under either key, absent only for resources written
/// before resources were scoped to a workspace; callers may adopt those once with [`init_scoped`],
/// but must never treat an explicit other workspace as interchangeable.
pub fn workspace_scope(doc: &AutoCommit) -> Option<String> {
    let m = meta_ro(doc)?;
    match stored_workspace(doc, &m) {
        StringAt::String(workspace) => Some(workspace),
        StringAt::Missing | StringAt::Invalid => None,
    }
}

pub fn primary_document(doc: &AutoCommit) -> bool {
    match object_at(doc, &ROOT, META, ObjType::Map) {
        ObjectAt::Missing => true,
        ObjectAt::Object(object) => {
            matches!(string_at(doc, &object, "id"), StringAt::Missing)
                && matches!(string_at(doc, &object, "kind"), StringAt::Missing)
                && !matches!(string_at(doc, &object, "home"), StringAt::Invalid)
                && !matches!(stored_workspace(doc, &object), StringAt::Invalid)
                && !matches!(
                    object_at(doc, &object, "refs", ObjType::List),
                    ObjectAt::Invalid
                )
        }
        ObjectAt::Invalid => false,
    }
}
