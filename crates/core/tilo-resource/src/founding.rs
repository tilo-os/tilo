//! The founding an app's record is derived from: what every holder makes of it alike.
//!
//! An app's record holds its content alone. Two devices that make the same record before they
//! sync (a person's vault opened on a phone and a laptop at once) would each make the containers
//! its fields keep their items in, and the items one of them adds first would land in a
//! container the merge leaves behind. So the containers come first, in one change every holder
//! derives from the record's workspace, kind and id alone ([`Scope`]): an actor derived from them
//! makes it at time zero. The store admits that change only as the one it derives itself
//! ([`structure`]); the app's own first values follow as any change of its.
//!
//! The workspace is part of every derivation. A record's id can be the same in every workspace
//! (a workspace's own accounts record), and a change two workspaces both held would be one
//! change in both: a host keeping both would refuse the second, and the hash would link records
//! the workspaces keep apart.

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{AutoCommit, Change, ChangeHash, ChangeMetadata, ObjType, ROOT};
use tilo_actions::surface::{FieldType, Surface};
use tilo_automerge::ApplyIncoming as _;

pub mod split;

/// The record a founding and its steps are derived for: the record `id` of `kind` in the
/// workspace `workspace`. A record is always one workspace's, so two workspaces holding a record
/// of the same kind and id still derive changes of their own, and the holders within one derive
/// the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scope<'a> {
    pub workspace: &'a str,
    pub kind: &'a str,
    pub id: &'a str,
}

impl Scope<'_> {
    /// The actor the record's founding is made under ([`structure`]). It is salted apart from
    /// the actor of the record's identity genesis, which is derived from the same id, kind and
    /// workspace: two different changes under one actor and sequence number would not merge.
    #[must_use]
    pub fn founding_actor(self) -> automerge::ActorId {
        crate::genesis::actor(
            self.id,
            self.kind,
            &format!("founding in {}", self.workspace),
        )
    }
}

/// The containers at the root of `surface`'s record that its top-level fields keep their items
/// in, by field name: a collection, map, set or outbox in a map, a list in a list, a record of
/// its own in a map: what [`structure`] makes.
///
/// An outbox is one of them: it is a unit the kind declares, empty when the record is founded,
/// and a record whose every field is read-only takes no write to make it. Built by its creator
/// instead, it would be the one change such a kind could not take, from the app or anyone.
#[must_use]
pub fn containers(surface: &Surface) -> Vec<(String, ObjType)> {
    let mut containers: Vec<(String, ObjType)> = surface
        .fields
        .iter()
        .filter(|field| !field.path.contains('.'))
        .filter_map(|field| {
            let container = match &field.ty {
                // A set is a map whose keys are its members, as the resource types keep one.
                FieldType::Collection(_)
                | FieldType::Map(_)
                | FieldType::Set(_)
                | FieldType::Outbox(_) => ObjType::Map,
                FieldType::List(_) => ObjType::List,
                FieldType::Named(_) if field.stored == "record" => ObjType::Map,
                _ => return None,
            };
            Some((field.path.clone(), container))
        })
        .collect();
    containers.sort_by(|a, b| a.0.cmp(&b.0));
    containers.dedup_by(|a, b| a.0 == b.0);
    containers
}

/// The containers a record of `surface`'s kind founds with: its own record's ([`containers`]),
/// as the store reads the kind's type from the surface. A surface naming its record by its label
/// declares no type of its kind, and an enum of its kind is no record: either founds with none.
#[must_use]
pub fn of_kind(surface: &Surface) -> Vec<(String, ObjType)> {
    let named = |name: &str| surface.types.iter().find(|ty| ty.name == name);
    let own = match named(&surface.kind) {
        Some(ty) => !ty.is_enum(),
        None => named(&surface.label).is_none(),
    };
    if own { containers(surface) } else { Vec::new() }
}

/// The derived founding of the record `scope` names, whose fields keep their items in the
/// `containers` named at its root. A kind that keeps none founds with a change that holds nothing:
/// every record has its founding as its first head, so a record nothing was written to yet is kept
/// and read back like any other, and what follows the founding names it as its dependency.
///
/// # Errors
/// When a container cannot be put at the root under its field's name (an empty name).
pub fn structure(
    scope: Scope<'_>,
    containers: &[(String, ObjType)],
) -> Result<automerge::Change, automerge::AutomergeError> {
    let mut doc = AutoCommit::new().with_actor(scope.founding_actor());
    for (field, container) in containers {
        doc.put_object(ROOT, field.as_str(), *container)?;
    }
    let options = || {
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message())
            .with_time(0)
    };
    // A kind keeping no container commits nothing, so its founding is an empty change.
    let hash = match doc.commit_with(options()) {
        Some(hash) => hash,
        None => doc.empty_change(options()),
    };
    doc.get_change_by_hash(&hash)
        .ok_or(automerge::AutomergeError::MissingHash(hash))
}

/// The derived founding of the record `scope` names ([`structure`]) followed by the steps that
/// bring it to each version up to `version` ([`crate::evolution::step`]): the chain every
/// holder of a record at `version` stands on. Each step follows the one before it, the first the
/// founding, so two holders that wrote their own changes meanwhile still make it byte for byte
/// alike.
///
/// # Errors
/// When the founding or a step cannot be made ([`structure`]).
pub fn chain(
    scope: Scope<'_>,
    version: u32,
    containers: &[(String, ObjType)],
) -> Result<Vec<automerge::Change>, automerge::AutomergeError> {
    let founding = structure(scope, containers)?;
    chain_on(&founding, scope, version)
}

/// The steps that bring `founding`, the founding a record `scope` names stands on, to `version`.
/// Derived on the founding the record itself holds, so a record founded by an older type — its
/// containers those its type then declared — steps to the same version on every holder of it, and
/// the steps a newer type derives on top of a founding it no longer has are never applied over it.
///
/// # Errors
/// When a step cannot be made.
pub fn chain_on(
    founding: &automerge::Change,
    scope: Scope<'_>,
    version: u32,
) -> Result<Vec<automerge::Change>, automerge::AutomergeError> {
    let mut doc = AutoCommit::new();
    doc.apply_incoming([founding.clone()])?;
    let mut chain = vec![founding.clone()];
    for version in 1..=version {
        let step = crate::evolution::step(scope, version, &doc)?;
        doc.apply_incoming([step.clone()])?;
        chain.push(step);
    }
    Ok(chain)
}

/// A document whose founding and steps a holder reads: a [`AutoCommit`] a record is written
/// through, or the [`automerge::Automerge`] a store keeps.
pub trait FoundingDoc {
    /// The metadata of every change it holds.
    fn change_metas(&mut self) -> Vec<ChangeMetadata<'_>>;

    /// The change `hash` names.
    fn change_by_hash(&mut self, hash: &ChangeHash) -> Option<Change>;
}

impl FoundingDoc for AutoCommit {
    fn change_metas(&mut self) -> Vec<ChangeMetadata<'_>> {
        self.get_changes_meta(&[])
    }

    fn change_by_hash(&mut self, hash: &ChangeHash) -> Option<Change> {
        self.get_change_by_hash(hash)
    }
}

impl FoundingDoc for automerge::Automerge {
    fn change_metas(&mut self) -> Vec<ChangeMetadata<'_>> {
        self.get_changes_meta(&[])
    }

    fn change_by_hash(&mut self, hash: &ChangeHash) -> Option<Change> {
        automerge::ReadDoc::get_change_by_hash(self, hash)
    }
}

/// The founding change `doc` holds for `scope`: the one the record's derived founding actor made,
/// whatever containers its type declared then. `None` for a record founded before foundings were
/// derived (its own devices' changes alone), which takes the one its type derives now ([`found`]).
#[must_use]
pub fn founding_of(doc: &mut impl FoundingDoc, scope: Scope<'_>) -> Option<automerge::Change> {
    let actor = scope.founding_actor();
    let hash = doc
        .change_metas()
        .into_iter()
        .find(|change| change.actor.as_ref() == &actor && change.seq == 1)?
        .hash;
    doc.change_by_hash(&hash)
}

/// The step that brings the record `scope` names to `version`, derived on the founding the record
/// `doc` holds (or the one its type derives, for a record founded before foundings were derived):
/// [`migrate`], for a holder that has the record.
///
/// # Errors
/// When the founding or the step cannot be made, or `version` is no version a record steps to (0).
pub fn migrate_on(
    doc: &mut impl FoundingDoc,
    scope: Scope<'_>,
    version: u32,
    containers: &[(String, ObjType)],
) -> Result<automerge::Change, automerge::AutomergeError> {
    let founding = match founding_of(doc, scope) {
        Some(founding) => founding,
        None => structure(scope, containers)?,
    };
    chain_on(&founding, scope, version)?
        .pop()
        .filter(|_| version > 0)
        .ok_or(automerge::AutomergeError::Fail)
}

/// The step that brings the record `scope` names to `version`, on the chain [`chain`] derives:
/// the one change a store admits under the migration verb for that version, and the one every
/// holder writes.
///
/// # Errors
/// When the chain cannot be made ([`chain`]), or `version` is no version a record steps to (0).
pub fn migrate(
    scope: Scope<'_>,
    version: u32,
    containers: &[(String, ObjType)],
) -> Result<automerge::Change, automerge::AutomergeError> {
    chain(scope, version, containers)?
        .pop()
        .filter(|_| version > 0)
        .ok_or(automerge::AutomergeError::Fail)
}

/// Bring the record `scope` names, the app `app`'s, to `version`, in the one way every holder
/// does: a record made before its founding was derived (two devices' own root containers) takes
/// the founding and has what its devices made apart brought together
/// ([`split::merge_split_containers`]); then the chain of steps up to `version`
/// ([`chain`]) is written where it is not yet, and last the app's own `rewrite` of its content
/// from the version the record stood at ([`crate::evolution::rewrite`]). Whether this
/// changed anything.
///
/// # Errors
/// When the founding, a step or the merge cannot be made, or the app's rewrite fails; `doc` is
/// left with what was written before it.
pub fn bring_up(
    (scope, app): (Scope<'_>, &str),
    version: u32,
    containers: &[(String, ObjType)],
    rewrite: &dyn Fn(&mut AutoCommit, u32) -> Result<(), crate::ModelError>,
    doc: &mut AutoCommit,
) -> Result<bool, crate::ModelError> {
    let from = crate::evolution::stored_version(doc);
    let mut changed = found((scope, app), containers, doc)?;
    // The chain follows the founding this record stands on, not the one its type derives now:
    // a newer type's founding names containers an older record never had, and applying it over
    // the record's own would be a second change under one founding's actor and number. The
    // common case is the founding its type derives now, found by one lookup: only a record an
    // older type founded is walked for the founding it holds.
    let current = structure(scope, containers)?;
    let chain = if doc.get_change_by_hash(&current.hash()).is_some() {
        chain_on(&current, scope, version)?
    } else {
        let founding = founding_of(doc, scope).ok_or(automerge::AutomergeError::Fail)?;
        chain_on(&founding, scope, version)?
    };
    let missing: Vec<automerge::Change> = chain
        .into_iter()
        .filter(|link| doc.get_change_by_hash(&link.hash()).is_none())
        .collect();
    changed |= !missing.is_empty();
    doc.apply_incoming(missing)?;
    if from < version || crate::evolution::stored_version(doc) < version {
        changed |= crate::evolution::rewrite((scope, version), from, rewrite, doc)?;
    }
    Ok(changed)
}

/// Give the record `scope` names, the app `app`'s, its derived founding when it was made without
/// one (two devices' own root containers, before every holder derived it), and bring together
/// what its devices made apart ([`split::merge_split_containers`]): once, the first part
/// of [`bring_up`], and all a store that reads no type's version does on opening a record.
/// Whether this changed anything.
///
/// # Errors
/// When the founding or the merge cannot be made.
pub fn found(
    (scope, app): (Scope<'_>, &str),
    containers: &[(String, ObjType)],
    doc: &mut AutoCommit,
) -> Result<bool, automerge::AutomergeError> {
    let founding = structure(scope, containers)?;
    // The common case is one lookup, never a walk of the record's changes.
    if doc.get_change_by_hash(&founding.hash()).is_some() {
        return Ok(false);
    }
    // A record already founded by an older derivation of its founding keeps it: a later type's
    // founding holds another container set, under the same actor and number, and applying it
    // over the record's own is a conflicting change.
    if founding_of(doc, scope).is_some() {
        return Ok(false);
    }
    doc.apply_incoming([founding])?;
    split::merge_split_containers(doc, scope, containers, app)?;
    Ok(true)
}

/// [`bring_up`] of the record `id` of the resource type `R` in `workspace`, by the containers and
/// version its surface declares and its own migration (`#[resource(migrate = ..)]`).
///
/// # Errors
/// As [`bring_up`].
pub fn bring_up_resource<R: crate::Resource>(
    workspace: &str,
    id: &str,
    doc: &mut AutoCommit,
) -> Result<bool, crate::ModelError> {
    let scope = Scope {
        workspace,
        kind: R::KIND,
        id,
    };
    bring_up(
        (scope, R::APP_ID),
        R::VERSION,
        &of_kind(&crate::surface::surface_of::<R>()),
        &|doc: &mut AutoCommit, from: u32| R::migrate(doc, from),
        doc,
    )
}

/// A new record `id` of `R` in `workspace`: its derived founding and the steps to `R`'s version
/// ([`chain`]), then `R`'s first values, left uncommitted for the caller to commit under its own
/// message. Its version comes from the chain alone, as every holder of the record derives it.
///
/// # Errors
/// When the chain cannot be made, or `R`'s initializer fails.
pub fn new_record<R: crate::Resource>(
    workspace: &str,
    id: &str,
) -> Result<AutoCommit, crate::ModelError> {
    let mut doc = AutoCommit::new();
    let containers = of_kind(&crate::surface::surface_of::<R>());
    let scope = Scope {
        workspace,
        kind: R::KIND,
        id,
    };
    doc.apply_incoming(chain(scope, R::VERSION, &containers)?)?;
    R::initialize(&mut doc)?;
    Ok(doc)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]

    use crate::evolution::{VERSION_KEY, stored_version};
    use automerge::transaction::Transactable as _;
    use automerge::{AutoCommit, ObjType, ROOT, ReadDoc as _};
    use tilo_automerge::ApplyIncoming as _;

    const KIND: &str = "test.founding.chain";
    const ID: &str = "groceries";
    const APP: &str = "test.app";
    const SCOPE: super::Scope<'static> = super::Scope {
        workspace: "6f1c1b62-4f5e-4b8a-9b52-1f8a3f0f3a11",
        kind: KIND,
        id: ID,
    };

    fn containers() -> Vec<(String, ObjType)> {
        vec![("items".to_owned(), ObjType::Map)]
    }

    /// A record founded at `version` on a device of its own, with `wrote` written under `key` of
    /// its items as that device's change.
    fn device(version: u32, actor: u8, key: &str, wrote: &str) -> AutoCommit {
        let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from([actor; 16]));
        doc.apply_incoming(super::chain(SCOPE, version, &containers()).unwrap())
            .unwrap();
        let (_, items) = doc.get(ROOT, "items").unwrap().unwrap();
        doc.put(&items, key, wrote).unwrap();
        doc.commit();
        doc
    }

    /// A type whose versions change nothing but the version itself.
    fn unchanged() -> impl Fn(&mut AutoCommit, u32) -> Result<(), crate::ModelError> {
        |_, _| Ok(())
    }

    fn items(doc: &AutoCommit) -> Vec<String> {
        let (_, items) = doc.get(ROOT, "items").unwrap().unwrap();
        doc.keys(&items).collect()
    }

    /// A kind's outbox is one of its containers, founded empty on every holder alike: a record
    /// whose every field is read-only would take no change to make it, from the app or anyone.
    #[test]
    fn a_kinds_queue_is_founded_with_its_containers() {
        let surface: crate::Surface = serde_json::from_value(serde_json::json!({
            "kind": "terminal.session",
            "label": "Terminal",
            "version": 1,
            "fields": [
                {
                    "path": "requests",
                    "type": {"outbox": {"named": "CommandRequest"}},
                    "readonly": true,
                    "unit": true
                }
            ],
            "verbs": [],
            "types": []
        }))
        .unwrap();
        assert_eq!(
            super::containers(&surface),
            vec![("requests".to_owned(), ObjType::Map)],
            "the outbox is founded like a map the kind keeps its items in"
        );
        let founding = super::structure(SCOPE, &super::containers(&surface)).unwrap();
        let mut doc = AutoCommit::new();
        doc.apply_incoming([founding]).unwrap();
        assert_eq!(
            doc.get(ROOT, "requests").unwrap().map(|(value, _)| value),
            Some(automerge::Value::Object(ObjType::Map)),
            "founded, and empty"
        );
    }

    #[test]
    fn two_devices_with_their_own_writes_step_a_record_up_byte_for_byte_alike() {
        let mut here = device(1, 1, "milk", "2 l");
        let mut there = device(1, 2, "eggs", "6");
        let (own_heads, peer_heads) = (here.get_heads(), there.get_heads());
        assert!(super::bring_up((SCOPE, APP), 3, &containers(), &unchanged(), &mut here).unwrap());
        assert!(super::bring_up((SCOPE, APP), 3, &containers(), &unchanged(), &mut there).unwrap());
        let bytes = |changes: Vec<automerge::Change>| -> Vec<Vec<u8>> {
            changes
                .into_iter()
                .map(|change| change.raw_bytes().to_vec())
                .collect()
        };
        let ours = bytes(here.get_changes(&own_heads));
        let theirs = bytes(there.get_changes(&peer_heads));
        // Each device wrote the steps to versions 2 and 3 and nothing else, and both wrote the
        // same bytes: the derived ones, whatever each device's own writes were.
        assert_eq!(ours, theirs);
        assert_eq!(
            ours,
            bytes(super::chain(SCOPE, 3, &containers()).unwrap().split_off(2))
        );
        for (to, step) in (2..=3).zip(super::chain(SCOPE, 3, &containers()).unwrap().split_off(2)) {
            assert_eq!(
                super::migrate(SCOPE, to, &containers()).unwrap().hash(),
                step.hash()
            );
        }
        tilo_automerge::ApplyIncoming::merge_incoming(&mut here, &mut there).unwrap();
        assert_eq!(stored_version(&here), 3);
        assert_eq!(items(&here), ["eggs", "milk"]);
        // Brought up already: nothing more.
        assert!(!super::bring_up((SCOPE, APP), 3, &containers(), &unchanged(), &mut here).unwrap());
    }

    #[test]
    fn a_rewrite_two_devices_make_apart_keeps_one_default() {
        // Version 2 adds the record's tags, and each device's rewrite fills the default in on its
        // own history: the two rewrites make the same key once, and each device's items stay.
        let mut here = device(1, 1, "milk", "2 l");
        let mut there = here.fork().with_actor(automerge::ActorId::from([2; 16]));
        let (_, kept) = there.get(ROOT, "items").unwrap().unwrap();
        there.put(&kept, "eggs", "6").unwrap();
        there.commit();
        let add_tags = |doc: &mut AutoCommit, from: u32| {
            assert_eq!(from, 1);
            let tags = doc.put_object(ROOT, "tags", ObjType::Map)?;
            doc.put(&tags, "default", "kept")?;
            Ok(())
        };
        super::bring_up((SCOPE, APP), 2, &containers(), &add_tags, &mut here).unwrap();
        super::bring_up((SCOPE, APP), 2, &containers(), &add_tags, &mut there).unwrap();
        tilo_automerge::ApplyIncoming::merge_incoming(&mut here, &mut there).unwrap();
        let (_, tags) = here.get(ROOT, "tags").unwrap().unwrap();
        assert_eq!(here.keys(&tags).collect::<Vec<_>>(), ["default"]);
        assert_eq!(items(&here), ["eggs", "milk"]);
        assert_eq!(stored_version(&here), 2);
    }

    #[test]
    fn a_record_made_before_its_founding_was_derived_is_founded_once_and_brought_together() {
        // Two devices each made the record's container of their own, and added to it.
        let made = |actor: u8, key: &str| {
            let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from([actor; 16]));
            let items = doc.put_object(ROOT, "items", ObjType::Map).unwrap();
            doc.put(&items, key, "kept").unwrap();
            doc.commit();
            doc
        };
        let mut here = made(1, "milk");
        let mut there = made(2, "eggs");
        tilo_automerge::ApplyIncoming::merge_incoming(&mut here, &mut there).unwrap();
        let mut again = here.fork();
        assert!(super::bring_up((SCOPE, APP), 2, &containers(), &unchanged(), &mut here).unwrap());
        assert!(super::bring_up((SCOPE, APP), 2, &containers(), &unchanged(), &mut again).unwrap());
        // Every holder converts it alike.
        assert_eq!(here.get_heads(), again.get_heads());
        let founding = super::structure(SCOPE, &containers()).unwrap();
        assert!(here.get_change_by_hash(&founding.hash()).is_some());
        let mut held = items(&here);
        held.sort();
        assert_eq!(held, ["eggs", "milk"]);
        assert_eq!(stored_version(&here), 2);
        assert!(here.get(ROOT, VERSION_KEY).unwrap().is_some());
        assert!(!super::bring_up((SCOPE, APP), 2, &containers(), &unchanged(), &mut here).unwrap());
    }

    #[test]
    fn the_same_record_in_two_workspaces_shares_no_change() {
        // A workspace's own records have the same id in every workspace; a host holding two
        // workspaces keeps each one's apart, so neither the founding nor a step may be the same
        // change in both.
        let other = super::Scope {
            workspace: "0b9d7a3e-2c41-4f6b-8e15-7a9c3d2e1f40",
            ..SCOPE
        };
        let hashes = |scope| -> Vec<automerge::ChangeHash> {
            super::chain(scope, 3, &containers())
                .unwrap()
                .iter()
                .map(automerge::Change::hash)
                .collect()
        };
        let (here, there) = (hashes(SCOPE), hashes(other));
        assert_eq!(here.len(), 4);
        assert!(here.iter().all(|hash| !there.contains(hash)));
        assert_ne!(
            super::structure(SCOPE, &containers()).unwrap().hash(),
            super::structure(other, &containers()).unwrap().hash()
        );
        // Nor is its actor the one the record's identity genesis in its workspace is made under,
        // which a record founded beside that genesis would hold two first changes of.
        assert_ne!(
            SCOPE.founding_actor(),
            crate::genesis::actor(ID, KIND, SCOPE.workspace)
        );
        // Brought up from records each workspace's devices made apart, neither shares a change
        // with the other either.
        let mut here_doc = device(1, 1, "milk", "2 l");
        let mut there_doc = AutoCommit::new().with_actor(automerge::ActorId::from([1; 16]));
        there_doc
            .apply_incoming(super::chain(other, 1, &containers()).unwrap())
            .unwrap();
        super::bring_up((SCOPE, APP), 3, &containers(), &unchanged(), &mut here_doc).unwrap();
        super::bring_up((other, APP), 3, &containers(), &unchanged(), &mut there_doc).unwrap();
        let held = |doc: &mut AutoCommit| -> Vec<automerge::ChangeHash> {
            doc.get_changes(&[])
                .iter()
                .map(automerge::Change::hash)
                .collect()
        };
        let there_held = held(&mut there_doc);
        assert!(
            held(&mut here_doc)
                .iter()
                .all(|hash| !there_held.contains(hash))
        );
    }
}
