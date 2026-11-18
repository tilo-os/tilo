//! What a workspace keeps with the removal of a device: where each document's
//! history stood.
//!
//! A person's root revokes a device ([`tilo_user_identity::RevocationEntry`])
//! and the workspace's governance carries the entry in its device rows. That
//! says the device is gone; it does not say where the device's past ends. A
//! clock cannot: the entry's time is its signer's, and a write's time is its
//! author's. So the device that writes the removal into the governance writes
//! its *cut* beside it: for every document, the number of the removed device's
//! last write the workspace had taken. Every write carries its author's own
//! running number per document, so the cut is a place in each document's
//! history that every replica reads the same, and the removed device has no
//! say in it.
//!
//! - A write of the removed device at or below its document's cut is history:
//!   taken by everyone, for good ([`Place::Before`]).
//! - A write above it, or to a document the cut does not name, is refused by
//!   everyone, whoever hands it over and whatever time it claims
//!   ([`Place::After`]).
//! - While a replica holds no cut for a removal it knows of, it decides
//!   nothing about that device's writes ([`Place::Unrecorded`]): it holds
//!   them.
//!
//! The cuts are one map of the workspace document, [`FIELD_V1`], which the
//! workspace is founded with: a key that is a device (hex) says its removal's
//! cut is written, and a key of a device and a route holds that route's
//! number. Two devices writing the cut of the same removal each say what they
//! had taken, and the larger number stands: a write either of them took
//! before it knew of the removal is history. A cut is never lowered and never
//! erased ([`monotonic`]).
//!
//! A compaction restates history under a number its author used before, so
//! its number says nothing of when it was made. The cut therefore names each
//! compaction of the removed device that the workspace had taken, by the id
//! of that very write, and no other compaction of its is ever history.

use std::collections::{BTreeMap, BTreeSet};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};

/// The workspace document's key for the cuts of its devices' removals.
pub const FIELD_V1: &str = "device_removals_v1";

/// Where a write of a removed device stands to the cut of its removal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// No cut is written for the device's removal here yet.
    Unrecorded,
    /// At or below its document's cut: the workspace had taken it when the
    /// removal was written down.
    Before,
    /// Above its document's cut, or in a document the cut does not name.
    After,
}

/// What a removed device had written into one document when its removal was
/// written down.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Held {
    /// The number of its last write the workspace had taken.
    pub(crate) last: u64,
    /// Its compactions in the document's history, each by that write's id.
    pub(crate) compactions: Vec<[u8; 32]>,
}

/// One value of the map, as stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Stored {
    /// Under a device's key: the cut of its removal is written. Under the key
    /// of one of its compactions: the workspace had taken that compaction.
    Recorded,
    /// Under a device's and a route's key: the number of the device's last
    /// write to the route that the workspace had taken.
    Taken(u64),
}

fn device_key(device: &[u8; 32]) -> String {
    hex::encode(device)
}

fn route_key(device: &[u8; 32], route: &str) -> String {
    format!("{}\0{route}", hex::encode(device))
}

fn compaction_key(device: &[u8; 32], id: &[u8; 32], route: &str) -> String {
    format!("c\0{}\0{}\0{route}", hex::encode(device), hex::encode(id))
}

const NOT_A_MAP: &str = "the cuts of device removals are not a map";

/// The map the workspace is founded with, which holds the cuts of its
/// devices' removals: none until this build founds a workspace, and exactly
/// one since.
fn maps(doc: &AutoCommit) -> Result<Vec<ObjId>, String> {
    match doc.get(ROOT, FIELD_V1).map_err(|error| error.to_string())? {
        None => Ok(Vec::new()),
        Some((Value::Object(ObjType::Map), id)) => Ok(vec![id]),
        Some(_) => Err(NOT_A_MAP.to_owned()),
    }
}

/// Every value `key` holds in `maps`, those of writers who wrote it at the
/// same time included.
fn stored(doc: &AutoCommit, maps: &[ObjId], key: &str) -> Result<Vec<Stored>, String> {
    let mut values = Vec::new();
    for map in maps {
        for (value, _) in doc.get_all(map, key).map_err(|error| error.to_string())? {
            let Value::Scalar(value) = value else {
                return Err("a cut of a device removal is not bytes".into());
            };
            let ScalarValue::Bytes(bytes) = value.as_ref() else {
                return Err("a cut of a device removal is not bytes".into());
            };
            values.push(
                crate::governed_routes::exact::<Stored>(bytes)
                    .ok_or("a cut of a device removal does not decode")?,
            );
        }
    }
    Ok(values)
}

/// The largest number among `values`: none is zero.
fn largest(values: &[Stored]) -> u64 {
    values
        .iter()
        .filter_map(|value| match value {
            Stored::Taken(sequence) => Some(*sequence),
            Stored::Recorded => None,
        })
        .max()
        .unwrap_or(0)
}

/// Where the removed `device`'s write number `sequence` to `document` of
/// `app` stands to the cut of its removal, as `doc` holds it. `compaction` is
/// the write's id when it is a compaction: then that id decides, never the
/// number, which a compaction shares with a write it folds.
///
/// # Errors
///
/// The map does not read: nothing can be said of the write.
pub fn place(
    doc: &AutoCommit,
    device: &[u8; 32],
    (app, document): (&str, &str),
    sequence: u64,
    compaction: Option<&[u8; 32]>,
) -> Result<Place, String> {
    let maps = maps(doc)?;
    let written = |key: &str| -> Result<bool, String> {
        Ok(stored(doc, &maps, key)?.contains(&Stored::Recorded))
    };
    if !written(&device_key(device))? {
        return Ok(Place::Unrecorded);
    }
    let route = crate::coverage::route_key(app, document);
    let before = match compaction {
        Some(id) => written(&compaction_key(device, id, &route))?,
        None => sequence <= largest(&stored(doc, &maps, &route_key(device, &route))?),
    };
    Ok(if before { Place::Before } else { Place::After })
}

/// The devices whose removal has its cut written in `doc`.
///
/// # Errors
///
/// The map does not read.
pub fn recorded(doc: &AutoCommit) -> Result<BTreeSet<[u8; 32]>, String> {
    Ok(rows(doc)?
        .into_iter()
        .filter(|(key, (written, _))| *written && !key.contains('\0'))
        .filter_map(|(key, _)| hex::decode(key).ok()?.try_into().ok())
        .collect())
}

/// Write the cut of `device`'s removal into `doc`: `held`, what the writer had
/// taken of the device, by route ([`crate::coverage::route_key`]). A removal
/// whose cut is written keeps it: nothing is written, and the answer is
/// `false`. The caller commits.
///
/// # Errors
///
/// The map does not read, or the workspace document refuses the write.
pub(crate) fn write(
    doc: &mut AutoCommit,
    device: &[u8; 32],
    held: &BTreeMap<String, Held>,
) -> Result<bool, String> {
    if stored(doc, &maps(doc)?, &device_key(device))?.contains(&Stored::Recorded) {
        return Ok(false);
    }
    let map = crate::state::governed_map(doc, FIELD_V1)?;
    let mut put = |key: String, value: Stored| -> Result<(), String> {
        let bytes = postcard::to_allocvec(&value).map_err(|error| error.to_string())?;
        doc.put(&map, key, ScalarValue::Bytes(bytes))
            .map_err(|error| error.to_string())
    };
    for (route, held) in held {
        if held.last > 0 {
            put(route_key(device, route), Stored::Taken(held.last))?;
        }
        for id in &held.compactions {
            put(compaction_key(device, id, route), Stored::Recorded)?;
        }
    }
    put(device_key(device), Stored::Recorded)?;
    Ok(true)
}

/// What the map of `doc` says, key by key: whether a device's cut or one of
/// its compactions is written, and the number each route's key holds.
fn rows(doc: &AutoCommit) -> Result<BTreeMap<String, (bool, u64)>, String> {
    let maps = maps(doc)?;
    let mut rows = BTreeMap::new();
    for key in maps.iter().flat_map(|map| doc.keys(map)) {
        if rows.contains_key(&key) {
            continue;
        }
        let values = stored(doc, &maps, &key)?;
        rows.insert(key, (values.contains(&Stored::Recorded), largest(&values)));
    }
    Ok(rows)
}

/// Check that `after` keeps every cut `before` holds: none is erased and
/// none is lowered, since either would take back history the workspace
/// already called valid.
///
/// # Errors
///
/// A cut is erased or lowered, or `after`'s map does not read.
pub fn monotonic(before: Option<&AutoCommit>, after: &AutoCommit) -> Result<(), String> {
    let Some(before) = before else {
        return rows(after).map(drop);
    };
    // Most workspaces removed no device: nothing to compare.
    if maps(before)?.is_empty() && maps(after)?.is_empty() {
        return Ok(());
    }
    let next = rows(after)?;
    for (key, (written, taken)) in rows(before)? {
        let (still_written, still_taken) = next.get(&key).copied().unwrap_or((false, 0));
        if written && !still_written {
            return Err("the cut of a device's removal cannot be erased".into());
        }
        if still_taken < taken {
            return Err("the cut of a device's removal cannot be lowered".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_automerge::ApplyIncoming as _;

    const DEVICE: [u8; 32] = [7; 32];

    fn cut(routes: &[(&str, &str, u64)]) -> BTreeMap<String, Held> {
        routes
            .iter()
            .map(|(app, document, last)| {
                let held = Held {
                    last: *last,
                    compactions: Vec::new(),
                };
                (crate::coverage::route_key(app, document), held)
            })
            .collect()
    }

    /// Where `device`'s plain write number `sequence` to `document` stands.
    fn placed(doc: &AutoCommit, device: &[u8; 32], document: &str, sequence: u64) -> Place {
        place(doc, device, ("app", document), sequence, None).unwrap()
    }

    fn only_map(doc: &AutoCommit) -> ObjId {
        let mut maps = maps(doc).unwrap();
        assert_eq!(maps.len(), 1);
        maps.remove(0)
    }

    /// A workspace document as its founding leaves it: the cuts' map exists
    /// before any device writes its own.
    fn founded() -> AutoCommit {
        let mut doc = AutoCommit::new();
        doc.put_object(ROOT, FIELD_V1, ObjType::Map).unwrap();
        doc
    }

    #[test]
    fn a_cut_places_a_removed_devices_writes_and_is_never_lowered_or_erased() {
        let mut doc = founded();
        // No cut written: nothing is said of the device's writes.
        assert_eq!(placed(&doc, &DEVICE, "note", 1), Place::Unrecorded);
        assert!(
            write(
                &mut doc,
                &DEVICE,
                &cut(&[("app", "note", 3), ("app", "empty", 0)])
            )
            .unwrap()
        );
        doc.commit();
        assert_eq!(recorded(&doc).unwrap(), BTreeSet::from([DEVICE]));
        for (sequence, expected) in [(1, Place::Before), (3, Place::Before), (4, Place::After)] {
            assert_eq!(placed(&doc, &DEVICE, "note", sequence), expected);
        }
        // A document the cut does not name holds nothing of the device.
        assert_eq!(placed(&doc, &DEVICE, "other", 1), Place::After);
        assert_eq!(placed(&doc, &DEVICE, "empty", 1), Place::After);
        // Another device's removal is not written by this one's.
        assert_eq!(placed(&doc, &[8; 32], "note", 1), Place::Unrecorded);

        // Written once: a second writing changes nothing.
        let held = doc.clone();
        assert!(!write(&mut doc, &DEVICE, &cut(&[("app", "note", 9)])).unwrap());
        assert_eq!(placed(&doc, &DEVICE, "note", 4), Place::After);
        assert_eq!(monotonic(Some(&held), &doc), Ok(()));

        // Lowered or erased, a cut would take history back.
        let map = only_map(&doc);
        let key = route_key(&DEVICE, &crate::coverage::route_key("app", "note"));
        let mut lowered = doc.clone();
        lowered
            .put(
                &map,
                key.as_str(),
                ScalarValue::Bytes(postcard::to_allocvec(&Stored::Taken(2)).unwrap()),
            )
            .unwrap();
        assert!(monotonic(Some(&doc), &lowered).is_err());
        let mut erased = doc.clone();
        erased.delete(&map, key.as_str()).unwrap();
        assert!(monotonic(Some(&doc), &erased).is_err());
        let mut forgotten = doc.clone();
        forgotten
            .delete(&map, device_key(&DEVICE).as_str())
            .unwrap();
        assert!(monotonic(Some(&doc), &forgotten).is_err());
        let mut gone = doc.clone();
        gone.delete(ROOT, FIELD_V1).unwrap();
        assert!(monotonic(Some(&doc), &gone).is_err());
    }

    #[test]
    fn two_writers_of_one_removals_cut_keep_what_either_had_taken() {
        // One map, founded with the workspace: both write their own devices'
        // cuts into it.
        let mut base = founded();
        base.put(ROOT, "name", "Workspace").unwrap();
        base.commit();
        let mut first = base.fork();
        let mut second = base.fork();
        assert!(write(&mut first, &DEVICE, &cut(&[("app", "note", 3)])).unwrap());
        assert!(write(&mut first, &[9; 32], &cut(&[("app", "note", 1)])).unwrap());
        assert!(
            write(
                &mut second,
                &DEVICE,
                &cut(&[("app", "note", 5), ("app", "other", 1)])
            )
            .unwrap()
        );
        first.commit();
        second.commit();
        // Whichever of them takes the other's in, both stand.
        let merged = |into: &AutoCommit, from: &AutoCommit| {
            let (mut into, mut from) = (into.clone(), from.clone());
            let before = into.clone();
            into.merge_incoming(&mut from).unwrap();
            assert_eq!(placed(&into, &DEVICE, "note", 5), Place::Before);
            assert_eq!(placed(&into, &DEVICE, "note", 6), Place::After);
            assert_eq!(placed(&into, &DEVICE, "other", 1), Place::Before);
            // What only one of them wrote is kept whichever map the key shows.
            assert_eq!(placed(&into, &[9; 32], "note", 1), Place::Before);
            assert_eq!(recorded(&into).unwrap(), BTreeSet::from([DEVICE, [9; 32]]));
            assert_eq!(monotonic(Some(&before), &into), Ok(()));
            // Written by both already: nothing more to write.
            assert!(!write(&mut into, &DEVICE, &cut(&[("app", "note", 9)])).unwrap());
        };
        merged(&first, &second);
        merged(&second, &first);
    }

    #[test]
    fn a_compaction_is_history_only_as_the_very_one_the_workspace_had_taken() {
        let (taken, other) = ([1; 32], [2; 32]);
        let mut doc = founded();
        let held = BTreeMap::from([(
            crate::coverage::route_key("app", "note"),
            Held {
                last: 3,
                compactions: vec![taken],
            },
        )]);
        assert!(write(&mut doc, &DEVICE, &held).unwrap());
        doc.commit();
        let folded = |document: &str, sequence, id: &[u8; 32]| {
            place(&doc, &DEVICE, ("app", document), sequence, Some(id)).unwrap()
        };
        // The compaction the workspace had taken, under the number it took.
        assert_eq!(folded("note", 3, &taken), Place::Before);
        // Another one under that number, which a number alone would let in.
        assert_eq!(folded("note", 3, &other), Place::After);
        assert_eq!(folded("note", 1, &other), Place::After);
        // The taken one names its own document, and nothing beside it.
        assert_eq!(folded("other", 3, &taken), Place::After);
        assert_eq!(placed(&doc, &DEVICE, "note", 3), Place::Before);
        // Erased, it would take the document's checkpoint back.
        let map = only_map(&doc);
        let key = compaction_key(&DEVICE, &taken, &crate::coverage::route_key("app", "note"));
        let mut erased = doc.clone();
        erased.delete(&map, key.as_str()).unwrap();
        assert!(monotonic(Some(&doc), &erased).is_err());
    }

    #[test]
    fn a_map_that_does_not_read_says_nothing() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, FIELD_V1, "not a map").unwrap();
        assert!(place(&doc, &DEVICE, ("app", "note"), 1, None).is_err());
        assert!(monotonic(None, &doc).is_err());
        let mut doc = AutoCommit::new();
        let map = doc.put_object(ROOT, FIELD_V1, ObjType::Map).unwrap();
        doc.put(
            &map,
            device_key(&DEVICE),
            ScalarValue::Bytes(vec![0xff, 0xff]),
        )
        .unwrap();
        assert!(place(&doc, &DEVICE, ("app", "note"), 1, None).is_err());
        assert!(recorded(&doc).is_err());
    }
}
