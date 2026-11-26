//! Bringing together a record whose root containers its devices each made of their own: what a
//! record founded before every holder derived its founding ([`super::structure`]) needs
//! once, and what a merge of such records leaves to do.

use std::collections::BTreeMap;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
use tilo_automerge::ApplyIncoming;

/// Bring together a record whose root containers its devices each made of their own before its
/// founding was the same everywhere: of every container `founding` names, the copies a merge left
/// conflicting at the root, where it shows one and hides the rest with the items added to them.
/// The items of each hidden copy are copied into the shown one, which every device picks alike: a
/// map's entries it lacks by key (an item's id), and a list's elements it holds fewer of than the
/// hidden copy does, an element naming an `id` held once by it.
///
/// The change is the same on every device that holds the same history: made at time zero by an
/// actor derived from the record (`kind`, `id`), the containers it brings together and the heads
/// it is made on, which are its only dependencies, and named as the app `app`'s own edit of its
/// record, the same everywhere. Two devices doing it at once from one history
/// make one change, which a merge keeps once; two doing it from different histories make two, of
/// which a list keeps an element naming an `id` once all the same. Opened again, a record brought
/// together is not changed. Whether this changed anything.
///
/// # Errors
/// The record could not be written.
pub fn merge_split_containers(
    doc: &mut AutoCommit,
    kind: &str,
    id: &str,
    founding: &[(String, ObjType)],
    app: &str,
) -> Result<bool, automerge::AutomergeError> {
    let mut split = Vec::new();
    for (field, container) in founding {
        let Some((Value::Object(_), shown)) = doc.get(ROOT, field.as_str())? else {
            continue;
        };
        let hidden: Vec<automerge::ObjId> = doc
            .get_all(ROOT, field.as_str())?
            .into_iter()
            .filter(|(value, held)| {
                matches!(value, Value::Object(ty) if ty == container) && *held != shown
            })
            .map(|(_, held)| held)
            .collect();
        if !hidden.is_empty() {
            split.push((*container, shown, hidden));
        }
    }
    if split.is_empty() {
        return Ok(false);
    }
    let heads = doc.get_heads();
    let mut named: Vec<String> = split
        .iter()
        .flat_map(|(_, shown, hidden)| std::iter::once(shown).chain(hidden))
        .map(ToString::to_string)
        .collect();
    named.sort();
    let mut on: Vec<String> = heads.iter().map(ToString::to_string).collect();
    on.sort();
    let actor = crate::genesis::actor(
        id,
        kind,
        &format!("merge {} on {}", named.join(","), on.join(",")),
    );
    let mut merging = doc.fork();
    merging.set_actor(actor);
    for (container, shown, hidden) in &split {
        for stray in hidden {
            match container {
                ObjType::Map | ObjType::Table => {
                    for key in merging.keys(stray).collect::<Vec<_>>() {
                        if merging.get(shown, key.as_str())?.is_none() {
                            copy_value(
                                &mut merging,
                                stray,
                                key.as_str().into(),
                                shown,
                                Place::Key(&key),
                            )?;
                        }
                    }
                }
                ObjType::List => {
                    let mut held: BTreeMap<String, usize> = BTreeMap::new();
                    for at in 0..merging.length(shown) {
                        let count = held
                            .entry(fingerprint(&merging, shown, at.into())?)
                            .or_default();
                        *count = count.saturating_add(1);
                    }
                    for at in 0..merging.length(stray) {
                        let print = fingerprint(&merging, stray, at.into())?;
                        match held.get_mut(&print) {
                            Some(count) if *count > 0 => *count = count.saturating_sub(1),
                            _ => {
                                let end = merging.length(shown);
                                copy_value(
                                    &mut merging,
                                    stray,
                                    at.into(),
                                    shown,
                                    Place::Index(end),
                                )?;
                            }
                        }
                    }
                }
                ObjType::Text => {}
            }
        }
        if *container == ObjType::List {
            once_by_id(&mut merging, shown)?;
        }
    }
    if merging.pending_ops() == 0 {
        return Ok(false);
    }
    merging.commit_with(
        automerge::transaction::CommitOptions::default()
            // The store derives it for the app whose record this is, on every device alike: the
            // same bytes made apart merge as one change. Naming the app keeps that, and names
            // who made it.
            .with_message(tilo_actions::ActionContext::whole_edit_by(
                &tilo_actions::Acting::by_app(app),
            ))
            .with_time(0),
    );
    let merged = merging
        .get_last_local_change()
        .ok_or(automerge::AutomergeError::Fail)?;
    ApplyIncoming::apply_incoming(doc, vec![merged])?;
    Ok(true)
}

/// The `id` the list element at `at` of `list` names, when it is a map naming one.
pub fn element_id(doc: &AutoCommit, list: &automerge::ObjId, at: usize) -> Option<String> {
    let Ok(Some((Value::Object(ObjType::Map), element))) = doc.get(list, at) else {
        return None;
    };
    match doc.get(&element, "id") {
        Ok(Some((Value::Scalar(id), _))) => Some(
            id.as_str()
                .map_or_else(|| format!("{:?}", id.as_ref()), str::to_owned),
        ),
        _ => None,
    }
}

/// Keep each element of `list` that names an `id` once: the first holding it.
fn once_by_id(
    doc: &mut AutoCommit,
    list: &automerge::ObjId,
) -> Result<(), automerge::AutomergeError> {
    let mut seen = std::collections::BTreeSet::new();
    let mut at = 0;
    while at < doc.length(list) {
        match element_id(doc, list, at) {
            Some(id) if seen.contains(&id) => doc.delete(list, at)?,
            Some(id) => {
                seen.insert(id);
                at = at.saturating_add(1);
            }
            _ => at = at.saturating_add(1),
        }
    }
    Ok(())
}

/// Where [`copy_value`] puts what it copies.
enum Place<'a> {
    Key(&'a str),
    Index(usize),
}

/// What the value at `prop` of `obj` holds, the same wherever it is held: what tells one list
/// element from another when two copies of a list are brought together.
fn fingerprint(
    doc: &AutoCommit,
    obj: &automerge::ObjId,
    prop: automerge::Prop,
) -> Result<String, automerge::AutomergeError> {
    Ok(match doc.get(obj, prop)? {
        Some((Value::Scalar(value), _)) => format!("{:?}", value.as_ref()),
        Some((Value::Object(ObjType::Text), id)) => format!("text:{:?}", doc.text(&id)?),
        Some((Value::Object(ObjType::List), id)) => {
            let items = (0..doc.length(&id))
                .map(|at| fingerprint(doc, &id, at.into()))
                .collect::<Result<Vec<String>, _>>()?;
            format!("list:{items:?}")
        }
        Some((Value::Object(_), id)) => {
            if let Some((Value::Scalar(named), _)) = doc.get(&id, "id")? {
                return Ok(format!("id:{:?}", named.as_ref()));
            }
            let entries = doc
                .keys(&id)
                .map(|key| {
                    let print = fingerprint(doc, &id, key.as_str().into())?;
                    Ok((key, print))
                })
                .collect::<Result<Vec<(String, String)>, automerge::AutomergeError>>()?;
            format!("map:{entries:?}")
        }
        None => String::new(),
    })
}

/// Copy the value at `prop` of `from` to `place` in `to`, whole: a container is made anew with
/// everything it holds.
fn copy_value(
    doc: &mut AutoCommit,
    from: &automerge::ObjId,
    prop: automerge::Prop,
    to: &automerge::ObjId,
    place: Place<'_>,
) -> Result<(), automerge::AutomergeError> {
    let Some((value, id)) = doc.get(from, prop)? else {
        return Ok(());
    };
    let made = match (value, place) {
        (Value::Scalar(value), Place::Key(key)) => {
            doc.put(to, key, value.into_owned())?;
            return Ok(());
        }
        (Value::Scalar(value), Place::Index(at)) => {
            doc.insert(to, at, value.into_owned())?;
            return Ok(());
        }
        (Value::Object(ty), Place::Key(key)) => doc.put_object(to, key, ty)?,
        (Value::Object(ty), Place::Index(at)) => doc.insert_object(to, at, ty)?,
    };
    match doc.object_type(&id)? {
        ObjType::Text => {
            let text = doc.text(&id)?;
            doc.splice_text(&made, 0, 0, &text)?;
        }
        ObjType::List => {
            for at in 0..doc.length(&id) {
                copy_value(doc, &id, at.into(), &made, Place::Index(at))?;
            }
        }
        ObjType::Map | ObjType::Table => {
            for key in doc.keys(&id).collect::<Vec<_>>() {
                copy_value(doc, &id, key.as_str().into(), &made, Place::Key(&key))?;
            }
        }
    }
    Ok(())
}

/// Copy what the container `from` of `source` holds into the container `into` of `doc`, of the
/// same type: a map's entries, a list's elements after those `into` holds, a text's characters.
pub fn copy_across(
    source: &AutoCommit,
    from: &automerge::ObjId,
    doc: &mut AutoCommit,
    into: &automerge::ObjId,
) -> Result<(), automerge::AutomergeError> {
    let made = |doc: &mut AutoCommit, value: Value<'_>, place: Place<'_>| {
        Ok::<_, automerge::AutomergeError>(match (value, place) {
            (Value::Scalar(value), Place::Key(key)) => {
                doc.put(into, key, value.into_owned())?;
                None
            }
            (Value::Scalar(value), Place::Index(at)) => {
                doc.insert(into, at, value.into_owned())?;
                None
            }
            (Value::Object(ty), Place::Key(key)) => Some(doc.put_object(into, key, ty)?),
            (Value::Object(ty), Place::Index(at)) => Some(doc.insert_object(into, at, ty)?),
        })
    };
    match source.object_type(from)? {
        ObjType::Text => {
            let text = source.text(from)?;
            let end = doc.length(into);
            doc.splice_text(into, end, 0, &text)?;
        }
        ObjType::List => {
            for at in 0..source.length(from) {
                if let Some((value, held)) = source.get(from, at)? {
                    let end = doc.length(into);
                    if let Some(child) = made(doc, value, Place::Index(end))? {
                        copy_across(source, &held, doc, &child)?;
                    }
                }
            }
        }
        ObjType::Map | ObjType::Table => {
            for key in source.keys(from).collect::<Vec<_>>() {
                if let Some((value, held)) = source.get(from, key.as_str())?
                    && let Some(child) = made(doc, value, Place::Key(&key))?
                {
                    copy_across(source, &held, doc, &child)?;
                }
            }
        }
    }
    Ok(())
}
