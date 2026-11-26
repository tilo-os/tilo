//! The founding an app's record is derived from: what every holder makes of it alike.
//!
//! An app's record holds its content alone. Two devices that make the same record before they
//! sync (a person's vault opened on a phone and a laptop at once) would each make the containers
//! its fields keep their items in, and the items one of them adds first would land in a
//! container the merge leaves behind. So the containers come first, in one change every holder
//! derives from the record's kind and id alone: an actor derived from them makes it at time zero.
//! The store admits that change only as the one it derives itself ([`structure`]); the app's
//! own first values follow as any change of its.

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{AutoCommit, ObjType, ROOT};
use tilo_automerge::ApplyIncoming as _;
use tilo_actions::surface::{FieldType, Surface};

pub mod split;

/// The containers at the root of `surface`'s record that its top-level fields keep their items
/// in, by field name: a collection, map or set in a map, a list in a list, a record of its own
/// in a map: what [`structure`] makes.
#[must_use]
pub fn containers(surface: &Surface) -> Vec<(String, ObjType)> {
    let mut containers: Vec<(String, ObjType)> = surface
        .fields
        .iter()
        .filter(|field| !field.path.contains('.'))
        .filter_map(|field| {
            let container = match &field.ty {
                // A set is a map whose keys are its members, as the resource types keep one.
                FieldType::Collection(_) | FieldType::Map(_) | FieldType::Set(_) => ObjType::Map,
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

/// The derived founding of the record `id` of `kind`, whose fields keep their items in the
/// `containers` named at its root. A kind that keeps none founds with a change that holds nothing:
/// every record has its founding as its first head, so a record nothing was written to yet is kept
/// and read back like any other, and what follows the founding names it as its dependency.
///
/// # Errors
/// When a container cannot be put at the root under its field's name (an empty name).
pub fn structure(
    kind: &str,
    id: &str,
    containers: &[(String, ObjType)],
) -> Result<automerge::Change, automerge::AutomergeError> {
    let mut doc = AutoCommit::new().with_actor(crate::genesis::actor(id, kind, ""));
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

/// The derived founding of the record `id` of `kind` ([`structure`]) followed by the steps that
/// bring it to each version up to `version` ([`crate::evolution::step`]): the chain every
/// holder of a record at `version` stands on. Each step follows the one before it, the first the
/// founding, so two holders that wrote their own changes meanwhile still make it byte for byte
/// alike.
///
/// # Errors
/// When the founding or a step cannot be made ([`structure`]).
pub fn chain(
    kind: &str,
    id: &str,
    version: u32,
    containers: &[(String, ObjType)],
) -> Result<Vec<automerge::Change>, automerge::AutomergeError> {
    let founding = structure(kind, id, containers)?;
    let mut doc = AutoCommit::new();
    doc.apply_incoming([founding.clone()])?;
    let mut chain = vec![founding];
    for version in 1..=version {
        let step = crate::evolution::step(kind, id, version, &doc)?;
        doc.apply_incoming([step.clone()])?;
        chain.push(step);
    }
    Ok(chain)
}

/// The step that brings the record `id` of `kind` to `version`, on the chain [`chain`] derives:
/// the one change a store admits under the migration verb for that version, and the one every
/// holder writes.
///
/// # Errors
/// When the chain cannot be made ([`chain`]), or `version` is no version a record steps to (0).
pub fn migrate(
    kind: &str,
    id: &str,
    version: u32,
    containers: &[(String, ObjType)],
) -> Result<automerge::Change, automerge::AutomergeError> {
    chain(kind, id, version, containers)?
        .pop()
        .filter(|_| version > 0)
        .ok_or(automerge::AutomergeError::Fail)
}

/// Bring the record `id` of `kind`, the app `app`'s, to `version`, in the one way every holder
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
    (kind, id, app): (&str, &str, &str),
    version: u32,
    containers: &[(String, ObjType)],
    rewrite: &dyn Fn(&mut AutoCommit, u32) -> Result<(), crate::ModelError>,
    doc: &mut AutoCommit,
) -> Result<bool, crate::ModelError> {
    let from = crate::evolution::stored_version(doc);
    let mut changed = found((kind, id, app), containers, doc)?;
    let chain = chain(kind, id, version, containers)?;
    let missing: Vec<automerge::Change> = chain
        .into_iter()
        .filter(|link| doc.get_change_by_hash(&link.hash()).is_none())
        .collect();
    changed |= !missing.is_empty();
    doc.apply_incoming(missing)?;
    if from < version || crate::evolution::stored_version(doc) < version {
        changed |= crate::evolution::rewrite((kind, id, version), from, rewrite, doc)?;
    }
    Ok(changed)
}

/// Give the record `id` of `kind`, the app `app`'s, its derived founding when it was made without
/// one (two devices' own root containers, before every holder derived it), and bring together
/// what its devices made apart ([`split::merge_split_containers`]): once, the first part
/// of [`bring_up`], and all a store that reads no type's version does on opening a record.
/// Whether this changed anything.
///
/// # Errors
/// When the founding or the merge cannot be made.
pub fn found(
    (kind, id, app): (&str, &str, &str),
    containers: &[(String, ObjType)],
    doc: &mut AutoCommit,
) -> Result<bool, automerge::AutomergeError> {
    let founding = structure(kind, id, containers)?;
    if doc.get_change_by_hash(&founding.hash()).is_some() {
        return Ok(false);
    }
    doc.apply_incoming([founding])?;
    split::merge_split_containers(doc, kind, id, containers, app)?;
    Ok(true)
}

/// [`bring_up`] of the record `id` of the resource type `R`, by the containers and version its
/// surface declares and its own migration (`#[resource(migrate = ..)]`).
///
/// # Errors
/// As [`bring_up`].
pub fn bring_up_resource<R: crate::Resource>(
    id: &str,
    doc: &mut AutoCommit,
) -> Result<bool, crate::ModelError> {
    bring_up(
        (R::KIND, id, R::APP_ID),
        R::VERSION,
        &of_kind(&crate::surface::surface_of::<R>()),
        &|doc: &mut AutoCommit, from: u32| R::migrate(doc, from),
        doc,
    )
}

/// A new record `id` of `R`: its derived founding and the steps to `R`'s version ([`chain`]),
/// then `R`'s first values, left uncommitted for the caller to commit under its own message.
/// Its version comes from the chain alone, as every holder of the record derives it.
///
/// # Errors
/// When the chain cannot be made, or `R`'s initializer fails.
pub fn new_record<R: crate::Resource>(id: &str) -> Result<AutoCommit, crate::ModelError> {
    let mut doc = AutoCommit::new();
    let containers = of_kind(&crate::surface::surface_of::<R>());
    doc.apply_incoming(chain(R::KIND, id, R::VERSION, &containers)?)?;
    R::initialize(&mut doc)?;
    Ok(doc)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]

    use std::collections::BTreeMap;

    use automerge::transaction::Transactable as _;
    use automerge::{AutoCommit, ObjType, ROOT, ReadDoc as _};
    use tilo_automerge::ApplyIncoming as _;
    use crate::evolution::{VERSION_KEY, stored_version};

    const KIND: &str = "test.founding.chain";
    const ID: &str = "groceries";
    const APP: &str = "test.app";

    fn containers() -> Vec<(String, ObjType)> {
        vec![("items".to_owned(), ObjType::Map)]
    }

    /// A record founded at `version` on a device of its own, with `wrote` written under `key` of
    /// its items as that device's change.
    fn device(version: u32, actor: u8, key: &str, wrote: &str) -> AutoCommit {
        let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from([actor; 16]));
        doc.apply_incoming(super::chain(KIND, ID, version, &containers()).unwrap())
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

    #[test]
    fn two_devices_with_their_own_writes_step_a_record_up_byte_for_byte_alike() {
        let mut here = device(1, 1, "milk", "2 l");
        let mut there = device(1, 2, "eggs", "6");
        let (own_heads, peer_heads) = (here.get_heads(), there.get_heads());
        assert!(super::bring_up((KIND, ID, APP), 3, &containers(), &unchanged(), &mut here).unwrap());
        assert!(
            super::bring_up((KIND, ID, APP), 3, &containers(), &unchanged(), &mut there).unwrap()
        );
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
            bytes(
                super::chain(KIND, ID, 3, &containers())
                    .unwrap()
                    .split_off(2)
            )
        );
        for (to, step) in (2..=3).zip(
            super::chain(KIND, ID, 3, &containers())
                .unwrap()
                .split_off(2),
        ) {
            assert_eq!(
                super::migrate(KIND, ID, to, &containers()).unwrap().hash(),
                step.hash()
            );
        }
        tilo_automerge::ApplyIncoming::merge_incoming(&mut here, &mut there).unwrap();
        assert_eq!(stored_version(&here), 3);
        assert_eq!(items(&here), ["eggs", "milk"]);
        // Brought up already: nothing more.
        assert!(
            !super::bring_up((KIND, ID, APP), 3, &containers(), &unchanged(), &mut here).unwrap()
        );
    }

    #[test]
    fn a_rewrite_two_devices_make_apart_keeps_each_item_once() {
        // Version 1 kept the record's tags as one JSON string; version 2 keeps them as a map. Each
        // device wrote an item of its own on the version 1 record before either brought it up.
        let mut here = device(1, 1, "milk", "2 l");
        here.put(ROOT, "tags", r#"{"red":"warm","blue":"cold"}"#)
            .unwrap();
        here.commit();
        let mut there = here.fork().with_actor(automerge::ActorId::from([2; 16]));
        let (_, kept) = there.get(ROOT, "items").unwrap().unwrap();
        there.put(&kept, "eggs", "6").unwrap();
        there.commit();
        let retype = |doc: &mut AutoCommit, from: u32| {
            assert_eq!(from, 1);
            crate::evolution::retype_json::<BTreeMap<String, String>>(doc, &ROOT, "tags")
                .map(drop)
        };
        super::bring_up((KIND, ID, APP), 2, &containers(), &retype, &mut here).unwrap();
        super::bring_up((KIND, ID, APP), 2, &containers(), &retype, &mut there).unwrap();
        tilo_automerge::ApplyIncoming::merge_incoming(&mut here, &mut there).unwrap();
        let (_, tags) = here.get(ROOT, "tags").unwrap().unwrap();
        let mut held: Vec<String> = here.keys(&tags).collect();
        held.sort();
        assert_eq!(held, ["blue", "red"]);
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
        assert!(super::bring_up((KIND, ID, APP), 2, &containers(), &unchanged(), &mut here).unwrap());
        assert!(
            super::bring_up((KIND, ID, APP), 2, &containers(), &unchanged(), &mut again).unwrap()
        );
        // Every holder converts it alike.
        assert_eq!(here.get_heads(), again.get_heads());
        let founding = super::structure(KIND, ID, &containers()).unwrap();
        assert!(here.get_change_by_hash(&founding.hash()).is_some());
        let mut held = items(&here);
        held.sort();
        assert_eq!(held, ["eggs", "milk"]);
        assert_eq!(stored_version(&here), 2);
        assert!(here.get(ROOT, VERSION_KEY).unwrap().is_some());
        assert!(
            !super::bring_up((KIND, ID, APP), 2, &containers(), &unchanged(), &mut here).unwrap()
        );
    }
}
