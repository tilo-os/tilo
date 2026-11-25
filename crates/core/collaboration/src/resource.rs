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

/// The record types a store founds records by: for each kind, the containers its records keep
/// their items in ([`collab_types::document::founding::of_kind`]), read from the type an app
/// holds of it. What any holder of the type derives alike, so every device founds a record of the
/// kind the same: never what surfaces this process happens to have been named. Clones share it.
#[derive(Clone, Default)]
pub struct RecordTypes(
    std::rc::Rc<
        std::cell::RefCell<std::collections::BTreeMap<String, (Vec<(String, ObjType)>, u32)>>,
    >,
);

impl RecordTypes {
    /// Found the records of `surface`'s kind by the containers its type declares.
    pub fn declare(&self, surface: &tilo_actions::surface::Surface) {
        self.0.borrow_mut().insert(
            surface.kind.clone(),
            (
                collab_types::document::founding::of_kind(surface),
                surface.version,
            ),
        );
    }

    /// [`Self::declare`] of the type `surface` describes, unless one of `kind` is declared
    /// already: a type is read once, however often its records are opened.
    pub fn declare_kind(
        &self,
        kind: &str,
        surface: impl FnOnce() -> tilo_actions::surface::Surface,
    ) {
        if !self.0.borrow().contains_key(kind) {
            self.declare(&surface());
        }
    }

    /// The containers records of `kind` found with, once its type was declared here.
    #[must_use]
    pub fn founding(&self, kind: &str) -> Option<Vec<(String, ObjType)>> {
        self.0
            .borrow()
            .get(kind)
            .map(|(containers, _)| containers.clone())
    }

    /// The containers records of `kind` found with and the storage version their chain of steps
    /// reaches ([`collab_types::document::founding::chain`]), once its type was declared here.
    #[must_use]
    pub fn chain(&self, kind: &str) -> Option<(Vec<(String, ObjType)>, u32)> {
        self.0.borrow().get(kind).cloned()
    }
}

/// Make `doc` an app's record as it is created: its derived founding, the containers its kind
/// keeps its items in (`founding`, as the record's type declares them,
/// [`collab_types::document::founding`]), which every member creating the same record makes alike
/// and the store derives for the record its route names, then what the app's `initialize` writes,
/// as the edit of `by`, who creates the record. Nothing of who the record is goes in: the
/// workspace's store keeps that.
///
/// A kind whose type is not declared (`founding` is `None`) founds nothing: its record is made
/// only while what `initialize` writes keeps no container at its root. One it would make there
/// is refused, since two devices would each make their own and a merge keeps one of them, with
/// the items the other's held.
///
/// # Errors
/// The initializer failed, or it makes a container of a kind whose type is not declared.
pub fn content_genesis(
    doc: &mut AutoCommit,
    checkout: &DocumentCheckout,
    founding: Option<(&[(String, ObjType)], u32)>,
    (by, initialize): (
        &tilo_action_policy::Acting,
        impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ),
) -> Result<(), ResourceOpenError> {
    let kind = checkout.kind.as_str();
    let failed =
        |error: automerge::AutomergeError| ResourceOpenError::Initialization(error.to_string());
    // What `initialize` writes is written into the founding's containers, never beside them:
    // an initializer that makes a container the founding made already fills the founded one.
    let mut first = AutoCommit::new();
    initialize(&mut first).map_err(ResourceOpenError::Initialization)?;
    if founding.is_none()
        && let Some(container) = first.keys(ROOT).find(|key| {
            matches!(
                first.get(ROOT, key.as_str()),
                Ok(Some((Value::Object(_), _)))
            )
        })
    {
        return Err(ResourceOpenError::Initialization(format!(
            "no type of `{kind}` is declared here, so its `{container}` cannot be founded alike on every device"
        )));
    }
    let mut made = AutoCommit::new().with_actor(doc.get_actor().clone());
    // The founding and the steps to the type's version, which every holder derives alike; a kind
    // whose type is not declared founds with no container and steps to no version.
    let (containers, version) = founding.unwrap_or_default();
    let chain =
        collab_types::document::founding::chain(kind, checkout.id.route(), version, containers)
            .map_err(|error| ResourceOpenError::Initialization(error.to_string()))?;
    ApplyIncoming::apply_incoming(&mut made, chain)
        .map_err(|error| ResourceOpenError::Initialization(error.to_string()))?;
    // What the record starts with is made alike wherever its creator makes it: under an actor
    // derived from the record and who creates it, at time zero, on the chain. Two of a person's
    // devices creating the same record write one change; two people write one each.
    let writer = made.get_actor().clone();
    made.set_actor(tilo_resource::genesis::actor(
        checkout.id.route(),
        kind,
        &format!(
            "initial content by {}",
            tilo_actions::stamp::Stamp::by(&by.by)
        ),
    ));
    for key in first.keys(ROOT).collect::<Vec<_>>() {
        // A record's version is its chain's alone, wherever its type is declared: what the
        // initializer says of it is no second writer of it.
        if founding.is_some() && key == tilo_resource::evolution::VERSION_KEY {
            continue;
        }
        let Some((value, from)) = first.get(ROOT, key.as_str()).map_err(failed)? else {
            continue;
        };
        match value {
            Value::Scalar(value) => {
                made.put(ROOT, key.as_str(), value.into_owned())
                    .map_err(failed)?;
            }
            Value::Object(ty) => {
                let into = match made.get(ROOT, key.as_str()).map_err(failed)? {
                    Some((Value::Object(held), into)) if held == ty => into,
                    _ => made.put_object(ROOT, key.as_str(), ty).map_err(failed)?,
                };
                collab_types::document::split::copy_across(&first, &from, &mut made, &into)
                    .map_err(failed)?;
            }
        }
    }
    if made.pending_ops() != 0 {
        made.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(initial_content_by(by))
                .with_time(0),
        );
    }
    made.set_actor(writer);
    *doc = made;
    Ok(())
}

/// The message of what a brand-new record starts with, which `by` creates it with: their edit,
/// since a change that names no one is refused, but what the record starts as rather than a step
/// of theirs, so no undo takes it back and workspace activity leaves it out
/// ([`tilo_actions::ChangeKind::External`]).
pub(crate) fn initial_content_by(by: &tilo_action_policy::Acting) -> String {
    tilo_actions::ActionContext::default()
        .with_invoker(tilo_actions::ActionInvoker::admitted(by))
        .with_change(tilo_actions::ChangeKind::External)
        .change_message()
        .unwrap_or_default()
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
    use collab_types::document::split::{element_id, merge_split_containers};

    const WORKSPACE: &str = "6f1c1b62-4f5e-4b8a-9b52-1f8a3f0f3a11";
    const KIND: &str = "files.tree";

    /// A list whose items two devices add to: the kind no app of this process names a surface
    /// for.
    const LIST: &str = "test.founding.list";

    fn list_type() -> RecordTypes {
        let types = RecordTypes::default();
        types.declare(
            &serde_json::from_value(serde_json::json!({
                "kind": LIST,
                "label": "List",
                "version": 1,
                "fields": [{"path": "items", "type": {"list": "text"}}],
            }))
            .expect("a surface"),
        );
        types
    }

    /// `checkout` founded by `writer` as a store declaring `types` founds it, its first value
    /// written by `initialize`.
    fn founded(
        writer: &automerge::ActorId,
        checkout: &DocumentCheckout,
        types: &RecordTypes,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<AutoCommit, ResourceOpenError> {
        let mut doc = AutoCommit::new().with_actor(writer.clone());
        let founding = types.chain(checkout.kind.as_str());
        content_genesis(
            &mut doc,
            checkout,
            founding
                .as_ref()
                .map(|(containers, version)| (containers.as_slice(), *version)),
            (
                &tilo_action_policy::Acting::by_app("test.founding"),
                initialize,
            ),
        )?;
        Ok(doc)
    }

    fn items(doc: &AutoCommit) -> Vec<String> {
        let Some((_, list)) = doc.get(ROOT, "items").expect("a readable record") else {
            return Vec::new();
        };
        let mut items: Vec<String> = (0..doc.length(&list))
            .filter_map(|at| doc.get(&list, at).ok().flatten())
            .filter_map(|(value, _)| value.into_string().ok())
            .collect();
        items.sort();
        items
    }

    fn add(doc: &mut AutoCommit, item: &str) {
        let (_, list) = doc
            .get(ROOT, "items")
            .expect("a readable record")
            .expect("the founding made the list");
        let at = doc.length(&list);
        doc.insert(&list, at, item).expect("an item is added");
        doc.commit();
    }

    /// Two devices that make the same record before they sync, each adding items of its own,
    /// keep every item once they merge, whichever of their writers sorts first: both added to
    /// the one list the founding made, never to a list of their own that the merge leaves
    /// behind.
    #[test]
    fn two_devices_founding_one_record_keep_each_others_items() {
        let types = list_type();
        let checkout = DocumentCheckout::new(DocumentId::for_tab(7), ResourceType::new(LIST));
        let ordered = [
            ([0x01_u8; 16], [0xfe_u8; 16]),
            ([0xfe_u8; 16], [0x01_u8; 16]),
        ]
        .map(|(one, two)| {
            (
                automerge::ActorId::from(one.as_slice()),
                automerge::ActorId::from(two.as_slice()),
            )
        });
        let random = (0..64).map(|_| (automerge::ActorId::random(), automerge::ActorId::random()));
        for (here, there) in ordered.into_iter().chain(random) {
            let initialize = |doc: &mut AutoCommit| {
                doc.put(ROOT, "title", "Shopping")
                    .map(drop)
                    .map_err(|error| error.to_string())
            };
            let mut one = founded(&here, &checkout, &types, initialize).expect("founded here");
            let mut two = founded(&there, &checkout, &types, initialize).expect("founded there");
            add(&mut one, "milk");
            add(&mut two, "eggs");
            add(&mut one, "bread");
            one.merge(&mut two.clone()).expect("the devices merge");
            two.merge(&mut one.clone()).expect("the devices merge");
            assert_eq!(items(&one), ["bread", "eggs", "milk"], "{here} and {there}");
            assert_eq!(items(&two), items(&one), "{here} and {there}");
        }
    }

    /// A record's founding is what its kind and route derive with the type a store declares,
    /// byte for byte, whatever surfaces the process was named: the same before an app of the
    /// kind named its surface here as after.
    #[test]
    fn a_records_founding_does_not_depend_on_the_surfaces_the_process_was_named() {
        let types = list_type();
        let checkout = DocumentCheckout::package_primary(ResourceType::new(LIST));
        let found = || {
            founded(&automerge::ActorId::random(), &checkout, &types, |_| Ok(()))
                .expect("founded")
                .get_changes(&[])
                .iter()
                .map(|change| change.raw_bytes().to_vec())
                .collect::<Vec<_>>()
        };
        let derived = collab_types::document::founding::chain(
            LIST,
            "",
            1,
            &[("items".to_owned(), ObjType::List)],
        )
        .expect("a list founds with its items");
        let unnamed = found();
        assert_eq!(
            unnamed,
            derived
                .iter()
                .map(|change| change.raw_bytes().to_vec())
                .collect::<Vec<_>>(),
            "the store's own derivation: the founding and its step to the type's version"
        );
        collab_types::app_surface::register_built_in(
            "so.tilo.test-founding",
            r#"{"resources":[{"fields":[{"path":"items","type":{"list":"text"}}],"kind":"test.founding.list","label":"List","version":1}]}"#,
        );
        assert!(
            collab_types::app_surface::resource_surface(LIST).is_some(),
            "the surface is named here now"
        );
        assert_eq!(found(), unnamed, "the same founding once it is");
    }

    /// The same record founded on two devices, one whose process was named the app's surface
    /// and one whose process was not, each adding items of its own and then syncing both ways:
    /// the two foundings are the same bytes, the devices converge, and no item is dropped.
    #[test]
    fn a_record_founded_with_and_without_the_surface_named_converges_with_every_item() {
        const BOARD: &str = "test.founding.board";
        let types = RecordTypes::default();
        types.declare(
            &serde_json::from_value(serde_json::json!({
                "kind": BOARD,
                "label": "Board",
                "version": 1,
                "fields": [{"path": "items", "type": {"list": "text"}}],
            }))
            .expect("a surface"),
        );
        let checkout = DocumentCheckout::new(DocumentId::for_tab(9), ResourceType::new(BOARD));
        let founding = |doc: &mut AutoCommit| -> Vec<Vec<u8>> {
            doc.get_changes(&[])
                .iter()
                .filter(|change| change.timestamp() == 0)
                .map(|change| change.raw_bytes().to_vec())
                .collect()
        };
        assert!(
            collab_types::app_surface::resource_surface(BOARD).is_none(),
            "no app of the kind named its surface here yet"
        );
        let mut unnamed = founded(&automerge::ActorId::random(), &checkout, &types, |_| Ok(()))
            .expect("founded where the surface was never named");
        collab_types::app_surface::register_built_in(
            "so.tilo.test-founding-board",
            r#"{"resources":[{"fields":[{"path":"items","type":{"list":"text"}}],"kind":"test.founding.board","label":"Board","version":1}]}"#,
        );
        assert!(
            collab_types::app_surface::resource_surface(BOARD).is_some(),
            "the surface is named here now"
        );
        let mut named = founded(&automerge::ActorId::random(), &checkout, &types, |_| Ok(()))
            .expect("founded where the surface is named");
        assert_eq!(
            founding(&mut named),
            founding(&mut unnamed),
            "one founding, byte for byte"
        );
        assert_eq!(
            founding(&mut named).len(),
            2,
            "the founding is one change, and its step to the type's version another"
        );

        add(&mut unnamed, "eggs");
        add(&mut named, "milk");
        add(&mut unnamed, "tea");
        add(&mut named, "bread");
        named
            .merge(&mut unnamed.clone())
            .expect("the named device takes the other's");
        unnamed
            .merge(&mut named.clone())
            .expect("the other takes the named device's");
        assert_eq!(items(&named), ["bread", "eggs", "milk", "tea"]);
        assert_eq!(items(&unnamed), items(&named));
        assert_eq!(named.get_heads(), unnamed.get_heads(), "they converged");
    }

    /// A record two devices founded apart, as one was founded where its type was not known:
    /// each made its own `items` list and `notes` map at the root, then added items of its own.
    fn founded_apart() -> (AutoCommit, AutoCommit) {
        let device = |actor: &[u8], items: &[&str], notes: &[(&str, &str)]| {
            let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(actor));
            let list = doc.put_object(ROOT, "items", ObjType::List).unwrap();
            let map = doc.put_object(ROOT, "notes", ObjType::Map).unwrap();
            doc.commit();
            for (at, item) in items.iter().enumerate() {
                doc.insert(&list, at, *item).unwrap();
            }
            for (id, text) in notes {
                let note = doc.put_object(&map, *id, ObjType::Map).unwrap();
                doc.put(&note, "text", *text).unwrap();
            }
            doc.commit();
            doc
        };
        (
            device(
                b"device-one",
                &["milk", "bread"],
                &[("n1", "call the plumber")],
            ),
            device(b"device-two", &["eggs"], &[("n2", "water the plants")]),
        )
    }

    fn split_founding() -> Vec<(String, ObjType)> {
        vec![
            ("items".to_owned(), ObjType::List),
            ("notes".to_owned(), ObjType::Map),
        ]
    }

    fn notes(doc: &AutoCommit) -> Vec<String> {
        let Some((_, map)) = doc.get(ROOT, "notes").unwrap() else {
            return Vec::new();
        };
        let mut notes: Vec<String> = doc.keys(&map).collect();
        notes.sort();
        notes
    }

    /// Merging two devices' records that were founded apart shows one device's containers and
    /// hides the other's with its items: the founding being the same from now on does not bring
    /// them back, so the record is brought together when it opens. Every item is there after,
    /// opening it again changes nothing, and two devices doing it at once converge.
    #[test]
    fn a_record_founded_apart_is_brought_together_with_every_item() {
        let (mut one, mut two) = founded_apart();
        one.merge(&mut two.clone()).unwrap();
        two.merge(&mut one.clone()).unwrap();
        assert_eq!(
            one.get_heads(),
            two.get_heads(),
            "the devices hold one history"
        );
        let shown = items(&one);
        assert!(
            shown == ["bread", "milk"] || shown == ["eggs"],
            "a merge shows one device's list and hides the other's: {shown:?}"
        );
        assert_eq!(
            one.get_all(ROOT, "items").unwrap().len(),
            2,
            "both lists are held, one hidden"
        );

        let mut alone = one.clone();
        assert!(
            merge_split_containers(&mut alone, LIST, "apart", &split_founding(), "test.app")
                .unwrap()
        );
        assert_eq!(items(&alone), ["bread", "eggs", "milk"]);
        assert_eq!(notes(&alone), ["n1", "n2"]);
        let heads = alone.get_heads();
        assert!(
            !merge_split_containers(&mut alone, LIST, "apart", &split_founding(), "test.app")
                .unwrap(),
            "a second open changes nothing"
        );
        assert_eq!(alone.get_heads(), heads);

        // Both devices bring it together from the one history they hold, before they sync
        // again: each makes the same change, which the merge keeps once.
        let (mut here, mut there) = (one.fork(), two.fork());
        here.set_actor(automerge::ActorId::from(b"device-one-again".as_slice()));
        there.set_actor(automerge::ActorId::from(b"device-two-again".as_slice()));
        assert!(
            merge_split_containers(&mut here, LIST, "apart", &split_founding(), "test.app")
                .unwrap()
        );
        assert!(
            merge_split_containers(&mut there, LIST, "apart", &split_founding(), "test.app")
                .unwrap()
        );
        assert_eq!(
            here.get_heads(),
            there.get_heads(),
            "one change, byte for byte"
        );
        here.merge(&mut there.clone()).unwrap();
        there.merge(&mut here.clone()).unwrap();
        assert_eq!(here.get_heads(), there.get_heads(), "they converge");
        assert_eq!(items(&here), ["bread", "eggs", "milk"], "each item once");
        assert_eq!(notes(&here), ["n1", "n2"], "each note once, by its id");
        assert_eq!(items(&there), items(&here));
    }

    /// Two devices that bring a split list together from different histories (one wrote
    /// something else first) make different changes; an element naming an `id` is kept once all
    /// the same, once either opens the merged record.
    #[test]
    fn a_list_brought_together_from_different_histories_keeps_each_item_once() {
        let device = |actor: &[u8], entries: &[(&str, &str)]| {
            let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(actor));
            let list = doc.put_object(ROOT, "entries", ObjType::List).unwrap();
            doc.commit();
            for (at, (id, text)) in entries.iter().enumerate() {
                let entry = doc.insert_object(&list, at, ObjType::Map).unwrap();
                doc.put(&entry, "id", *id).unwrap();
                doc.put(&entry, "text", *text).unwrap();
            }
            doc.commit();
            doc
        };
        let founding = [("entries".to_owned(), ObjType::List)];
        let ids = |doc: &AutoCommit| -> Vec<String> {
            let (_, list) = doc.get(ROOT, "entries").unwrap().unwrap();
            let mut ids: Vec<String> = (0..doc.length(&list))
                .filter_map(|at| element_id(doc, &list, at))
                .collect();
            ids.sort();
            ids
        };
        let mut one = device(b"device-one", &[("a", "milk"), ("b", "bread")]);
        let mut two = device(b"device-two", &[("c", "eggs")]);
        one.merge(&mut two.clone()).unwrap();
        two.merge(&mut one.clone()).unwrap();
        two.put(ROOT, "title", "Shopping").unwrap();
        two.commit();
        assert!(merge_split_containers(&mut one, LIST, "entries", &founding, "test.app").unwrap());
        assert!(merge_split_containers(&mut two, LIST, "entries", &founding, "test.app").unwrap());
        one.merge(&mut two.clone()).unwrap();
        two.merge(&mut one.clone()).unwrap();
        merge_split_containers(&mut one, LIST, "entries", &founding, "test.app").unwrap();
        merge_split_containers(&mut two, LIST, "entries", &founding, "test.app").unwrap();
        one.merge(&mut two.clone()).unwrap();
        two.merge(&mut one.clone()).unwrap();
        assert_eq!(one.get_heads(), two.get_heads(), "they converge");
        assert_eq!(ids(&one), ["a", "b", "c"], "each entry once");
        let heads = one.get_heads();
        assert!(!merge_split_containers(&mut one, LIST, "entries", &founding, "test.app").unwrap());
        assert_eq!(one.get_heads(), heads, "opened again, nothing changes");
    }

    /// A kind whose type nobody declared is not founded by guesswork: a first write that makes a
    /// container at its root is refused, as each device would make one of its own.
    #[test]
    fn a_record_of_an_undeclared_type_is_not_founded_with_a_container() {
        let checkout = DocumentCheckout::new(DocumentId::for_tab(8), ResourceType::new(LIST));
        let undeclared = RecordTypes::default();
        let refused = founded(
            &automerge::ActorId::random(),
            &checkout,
            &undeclared,
            |doc| {
                doc.put_object(ROOT, "items", ObjType::List)
                    .map(drop)
                    .map_err(|error| error.to_string())
            },
        );
        assert!(
            matches!(&refused, Err(ResourceOpenError::Initialization(reason)) if reason.contains("`items`")),
            "founded: {}",
            refused.is_ok()
        );
        let scalar = founded(
            &automerge::ActorId::random(),
            &checkout,
            &undeclared,
            |doc| {
                doc.put(ROOT, "title", "Untitled")
                    .map(drop)
                    .map_err(|error| error.to_string())
            },
        );
        assert!(
            scalar.is_ok(),
            "nothing at its root two devices could make apart"
        );
    }

    /// An app's own record founds on the changes the store derives for the record its route
    /// names, the empty one, and its step to the type's version: the store refuses any other
    /// founding or step of it.
    #[test]
    fn an_apps_own_record_founds_as_the_store_derives_it_for_its_route() {
        let kind = ResourceType::new(LIST);
        let mut made = founded(
            &automerge::ActorId::random(),
            &DocumentCheckout::package_primary(kind),
            &list_type(),
            |_| Ok(()),
        )
        .expect("the record is founded");
        let derived = collab_types::document::founding::migrate(
            LIST,
            "",
            1,
            &[("items".to_owned(), ObjType::List)],
        )
        .expect("a list founds with its items and steps to its version");
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
