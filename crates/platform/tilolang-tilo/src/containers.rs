//! The maps a record keeps beside its fields: its items ([`crate::items`], `$items`), the
//! outside effects waiting on it (`$outbox`), the marks its migrations and triggers left, and
//! the manifest that places its resources in chunks (`$chunks`).
//!
//! Each is a map at a key of the record's own map, made with the first thing put in it. Two
//! replicas that each put the first thing into a record before they met make a map of their
//! own: Automerge keeps one of the two, and everything in the other would be gone with it. So
//! two things hold, and both are needed:
//!
//! * a record made now makes its `$items` and `$outbox` with it ([`crate::action`] at its
//!   creation): the first thing put in afterwards goes into the one map there is;
//! * a record that already holds two reads whole ([`maps`]: a read reads every copy, so the
//!   things in the copy the record does not keep are never lost), and its next write folds the
//!   copies into the one it keeps ([`single`]), the same way on every replica, so one map holds
//!   everything again from that write on.
//!
//! The levels above the resources are maps of the same kind: the workspace's `tl` map, each
//! app's map under it, and the workspace record's `$personal` with the maps below it. Two
//! replicas that each write the first record of an app (or of a workspace) before they meet
//! make a map of their own, and everything in the map Automerge does not keep would be out of
//! every read: a person's first task would be gone. The same two things hold for them: a read
//! reaches through every copy at every step ([`reach`]), and a write makes and folds those maps
//! ([`single`]), so a record that already holds two reads the union, and its next write
//! converges on every replica.

use automerge::transaction::Transactable as _;
use automerge::{ObjId, ObjType, Prop, ReadDoc, ScalarValue, Value as AmValue};
use tilo_actions::ActionError;

type Doc = tilo_resource::Doc;

/// Every map the record `parent` holds at `key`: the copies a replica made before it met the
/// others, oldest first, and last the one [`ReadDoc::get`] answers with, which the record keeps.
/// A record that holds one map answers with that one.
#[must_use]
pub fn maps(doc: &Doc, parent: &ObjId, key: &str) -> Vec<ObjId> {
    doc.get_all(parent, key)
        .map(|held| {
            held.into_iter()
                .filter_map(|(value, id)| match value {
                    AmValue::Object(ObjType::Map) => Some(id),
                    AmValue::Object(_) | AmValue::Scalar(_) => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every map `path` reaches below `parent`, reading every copy a replica made at each step: the
/// maps a level holds are each read for the maps of the level below, so a level the document
/// keeps twice answers with the maps under both, and each level's copies come oldest first,
/// last the one the document keeps.
///
/// This is how a read of the levels above the resources (`tl`, `tl/<app>`, `$personal` and the
/// maps below) reaches what a replica wrote before it met the others: a record of an app whose
/// map another replica made is under that map, which [`ReadDoc::get`] does not answer with.
#[must_use]
pub fn reach(doc: &Doc, parent: &ObjId, path: &[&str]) -> Vec<ObjId> {
    let mut at = vec![parent.clone()];
    for key in path {
        at = at.iter().flat_map(|held| maps(doc, held, key)).collect();
    }
    at
}

/// The map at `key` of `parent`, made when the record holds none; the copies of it a replica
/// made are folded into the one the record keeps.
///
/// # Errors
/// A copy of the map could not be folded into the one the record keeps.
pub fn single(doc: &mut Doc, parent: &ObjId, key: &str) -> Result<ObjId, ActionError> {
    match fold(doc, parent, key)? {
        Some(kept) => Ok(kept),
        None => Ok(doc.put_object(parent, key, ObjType::Map)?),
    }
}

/// Fold every copy of the map at `key` of `parent` into the one the record keeps, and answer
/// with it: every thing a copy holds that the record's map does not is copied into it, a map
/// both hold is folded in turn, and the copy is emptied. `None` when there is no map at all, and
/// nothing to fold.
///
/// The record keeps the same copy on every replica ([`ReadDoc::get`] of the same changes
/// answers the same opid), so every replica folds the same way. A copy is only written when the
/// record holds two maps, so a record that never had two never pays for this.
///
/// # Errors
/// A thing a copy holds could not be read or written into the map the record keeps.
pub fn fold(doc: &mut Doc, parent: &ObjId, key: &str) -> Result<Option<ObjId>, ActionError> {
    let held = maps(doc, parent, key);
    let Some((kept, copies)) = held.split_last() else {
        return Ok(None);
    };
    let kept = kept.clone();
    for copy in copies {
        merge(doc, copy, &kept)?;
    }
    Ok(Some(kept))
}

/// Fold the map `from` into the map `into`, entry by entry, and empty it: an entry `into` does
/// not hold is copied in, a map both hold is folded in turn, and an entry both hold otherwise is
/// the one `into` holds, as a read of `into`'s key answers with it.
fn merge(doc: &mut Doc, from: &ObjId, into: &ObjId) -> Result<(), ActionError> {
    let entries: Vec<String> = doc.keys(from).collect();
    for entry in entries {
        let prop = Prop::Map(entry.clone());
        match read(doc, from, &prop)? {
            Copy::Map(nested) => match read(doc, into, &prop)? {
                Copy::Map(held) => merge(doc, &nested, &held)?,
                _ => copy_value(doc, from, &prop, into, &prop)?,
            },
            Copy::Nothing => {}
            _ => {
                if doc.get(into, entry.as_str())?.is_none() {
                    copy_value(doc, from, &prop, into, &prop)?;
                }
            }
        }
        doc.delete(from, entry.as_str())?;
    }
    Ok(())
}

/// A value to copy, read whole so that the document is free to be written to while it is.
enum Copy {
    Map(ObjId),
    List(ObjId),
    Text(ObjId),
    Scalar(ScalarValue),
    Nothing,
}

/// The value at `prop` of `from`, read whole.
fn read(doc: &Doc, from: &ObjId, prop: &Prop) -> Result<Copy, ActionError> {
    Ok(match doc.get(from, prop.clone())? {
        Some((AmValue::Object(ObjType::Map | ObjType::Table), id)) => Copy::Map(id),
        Some((AmValue::Object(ObjType::List), id)) => Copy::List(id),
        Some((AmValue::Object(ObjType::Text), id)) => Copy::Text(id),
        Some((AmValue::Scalar(scalar), _)) => Copy::Scalar(scalar.into_owned()),
        None => Copy::Nothing,
    })
}

/// Copy the value at `prop` of `from` to `to` of `into`: every entry of a map, every element of
/// a list, the text of a text with its marks, or the scalar itself.
///
/// A copy keeps what the value holds, not its history: a record's history is its action log,
/// and the ids of the things in it are their own.
fn copy_value(
    doc: &mut Doc,
    from: &ObjId,
    prop: &Prop,
    into: &ObjId,
    to: &Prop,
) -> Result<(), ActionError> {
    match read(doc, from, prop)? {
        Copy::Nothing => Ok(()),
        Copy::Scalar(scalar) => {
            match to {
                Prop::Map(key) => doc.put(into, key.as_str(), scalar)?,
                Prop::Seq(index) => doc.insert(into, *index, scalar)?,
            }
            Ok(())
        }
        Copy::Map(from) => {
            let made = make(doc, into, to, ObjType::Map)?;
            let entries: Vec<String> = doc.keys(&from).collect();
            for entry in entries {
                let prop = Prop::Map(entry);
                copy_value(doc, &from, &prop, &made, &prop)?;
            }
            Ok(())
        }
        Copy::List(from) => {
            let made = make(doc, into, to, ObjType::List)?;
            for index in 0..doc.length(&from) {
                let prop = Prop::Seq(index);
                copy_value(doc, &from, &prop, &made, &prop)?;
            }
            Ok(())
        }
        Copy::Text(from) => {
            let made = make(doc, into, to, ObjType::Text)?;
            tilo_resource::text::copy_whole(doc, &from, &made)?;
            Ok(())
        }
    }
}

/// A new object of `kind` at `prop` of `into`.
fn make(doc: &mut Doc, into: &ObjId, prop: &Prop, kind: ObjType) -> Result<ObjId, ActionError> {
    Ok(match prop {
        Prop::Map(key) => doc.put_object(into, key.as_str(), kind)?,
        Prop::Seq(index) => doc.insert_object(into, *index, kind)?,
    })
}
