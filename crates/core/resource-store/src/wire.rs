//! What an app asks of the device's one store, and what it is answered, as they cross the app's
//! connection. An app holds no store of its own: it writes and reads resources by kind and id,
//! and the device answers from its store, as the person the app acts for may read.
//!
//! The store carries a command out itself: [`write`] makes a write and answers each document's
//! change, which the device publishes as its own; [`read`] answers a read of only what the
//! reader check admits. Redo is the undo of an undo: the undo's own invocation, taken back.

use std::collections::BTreeSet;

use automerge::transaction::{Transactable as _, Transaction};
use automerge::{ObjId, ObjType, ReadDoc as _, ScalarValue};
use serde_json::Value as Json;

use collab_types::store::wire::{Command, Edit, PastChildren, Reach, Reply};
use collab_types::store::{Query, Resource};

use crate::Stamp;
use crate::store::{Store, StoreError, Written};

/// A write the store refused partway: a store written directly ([`write`]) holds what the batch
/// wrote before the refusal. A workspace keeps none of a refused batch's steps.
#[derive(Debug)]
pub struct Refused {
    pub error: StoreError,
    /// Each document's change the batch made before it was refused.
    pub written: Vec<Written>,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for Refused {}

/// Makes the write `command` in `store`, and answers each document's change.
///
/// # Errors
/// When `command` is a read, or the store refused the write.
pub fn write(store: &mut Store, command: Command) -> Result<Vec<Written>, Box<Refused>> {
    let command = placed_by_rules(store, command)?;
    write_placed(store, command)
}

/// [`write`] of a command whose resources are placed already.
fn write_placed(store: &mut Store, command: Command) -> Result<Vec<Written>, Box<Refused>> {
    let mut written = Vec::new();
    match write_into(store, command, &mut written) {
        Ok(()) => Ok(written),
        Err(error) => Err(Box::new(Refused { error, written })),
    }
}

/// `command` with each resource it puts placed with the readers its type's rule gives it.
fn placed_by_rules(store: &Store, command: Command) -> Result<Command, Box<Refused>> {
    store.placed_by_rules(command).map_err(|error| {
        Box::new(Refused {
            error,
            written: Vec::new(),
        })
    })
}

/// Draft the writes a command makes, keeping the live store untouched.
///
/// # Errors
/// When the command is a read or the store refused a step.
pub fn draft(store: &mut Store, command: Command) -> Result<Vec<Written>, Box<Refused>> {
    let command = placed_by_rules(store, command)?;
    let mut scope = DraftScope::default();
    scope.add(&command);
    store.stage(&scope, |staged| write_placed(staged, command))
}

/// The writes that found the resource `id` of `kind`, read by `readers`, under `stamp`, holding
/// `content`: its placement, then each change of its content, in order. The store applies none of
/// them; kept as one batch, they found the resource whole or not at all.
///
/// # Errors
/// The store refuses the placement.
pub fn found(
    store: &mut Store,
    (kind, id): (&str, &str),
    readers: &str,
    stamp: Stamp,
    content: Vec<automerge::Change>,
) -> Result<Vec<Written>, Box<Refused>> {
    // Placed first: readers its type's rule names stand for the empty set a caller passes, and
    // its content is read by the same people as its placement.
    let placed = placed_by_rules(
        store,
        Command::Put {
            kind: kind.to_owned(),
            id: id.to_owned(),
            resource: Resource {
                readers: readers.to_owned(),
                parent: None,
                order: None,
                fields: std::collections::BTreeMap::new(),
            },
            stamp,
        },
    )?;
    let readers = match &placed {
        Command::Put { resource, .. } => resource.readers.clone(),
        _ => readers.to_owned(),
    };
    let mut scope = DraftScope::default();
    scope.add(&placed);
    let mut writes = store.stage(&scope, |staged| write_placed(staged, placed))?;
    let doc = crate::store::content_id(&readers, kind, id);
    writes.extend(content.into_iter().map(|change| Written {
        doc: doc.clone(),
        readers: readers.clone(),
        change,
    }));
    Ok(writes)
}

/// The live index rows and placement counters a command may consult while staging.
#[derive(Default)]
pub(crate) struct DraftScope {
    pub(crate) resources: BTreeSet<(String, String)>,
    pub(crate) placements: BTreeSet<(String, String)>,
    pub(crate) undo: bool,
    /// A check of writes already made: the index rows it reads are `resources`, and it keeps
    /// every placement and nesting the store knows.
    pub(crate) check: bool,
}

impl DraftScope {
    fn add(&mut self, command: &Command) {
        match command {
            Command::Put {
                kind, id, resource, ..
            } => {
                self.resources.insert((kind.clone(), id.clone()));
                self.placements.insert((
                    resource.readers.clone(),
                    resource.parent.as_ref().unwrap_or(kind).clone(),
                ));
            }
            Command::Remove { kind, id, .. }
            | Command::Edit { kind, id, .. }
            | Command::Checkpoint { kind, id, .. } => {
                self.resources.insert((kind.clone(), id.clone()));
            }
            // A restore writes resources it does not name: those nested in what it names, and an
            // undo of several steps whichever documents those steps touched.
            Command::Undo { .. } | Command::Restore { .. } | Command::UndoMany { .. } => {
                self.undo = true;
            }
            Command::Batch(commands) => {
                for step in commands {
                    self.add(step);
                }
            }
            _ => {}
        }
    }
}

fn write_into(
    store: &mut Store,
    command: Command,
    written: &mut Vec<Written>,
) -> Result<(), StoreError> {
    match command {
        Command::Put {
            kind,
            id,
            resource,
            stamp,
        } => written.extend(store.put(&kind, &id, resource, &stamp)?),
        Command::Remove { kind, id, stamp } => written.push(store.remove(&kind, &id, &stamp)?),
        Command::Undo { invocation, stamp } => written.extend(store.undo(&invocation, &stamp)?),
        Command::UndoMany { invocations, stamp } => {
            for invocation in &invocations {
                written.extend(store.undo(invocation, &stamp)?);
            }
        }
        Command::Checkpoint {
            kind,
            id,
            name,
            stamp,
        } => written.push(store.checkpoint(&kind, &id, &name, &stamp)?),
        Command::Restore {
            resources,
            change,
            only,
            live,
            stamp,
        } => written.extend(store.restore_version(
            &resources,
            &change,
            only.as_deref(),
            live.as_deref(),
            &stamp,
        )?),
        Command::Edit {
            kind,
            id,
            edits,
            stamp,
        } => {
            let ((), change) = store.edit_content(&kind, &id, &stamp, |tx, root| {
                edits.iter().try_for_each(|edit| apply(tx, root, edit))
            })?;
            written.extend(change);
        }
        Command::Batch(commands) => {
            for command in commands {
                write_into(store, command, written)?;
            }
        }
        // Every other command makes no store write: the store's `read` answers the reads among
        // them, and its host answers the rest.
        _ => return Err(StoreError::NotAWrite),
    }
    Ok(())
}

/// Answers [`Command::Steps`] from `store`: the invocations of `invoker`'s last `steps` steps
/// still in effect, newest first (with `redo`, the undos of theirs still in effect), read from the
/// store's own change graph, in the documents holding records of `kinds` (every document when it
/// names none). What an undo or redo is carried out by, also where the store was opened for the
/// call that asked.
#[must_use]
pub fn steps(store: &mut Store, steps: u32, invoker: &str, kinds: &[String], redo: bool) -> Reply {
    Reply::Ids(store.steps(steps as usize, invoker, kinds, redo))
}

/// Whether the person `reader` (their key as a reader set names them) reads what the reader set
/// `readers` names: `*` names every member, and nobody reads as no one. What every host of the
/// store answers a read by.
pub fn reads_as(reader: &str) -> impl Fn(&str) -> bool + '_ {
    move |readers| {
        !reader.is_empty() && (readers == "*" || readers.split(',').any(|person| person == reader))
    }
}

/// Answers the read `command` from `store`, of only what `may_read` admits; `None` for a write.
#[must_use]
pub fn read(store: &Store, command: &Command, may_read: &dyn Fn(&str) -> bool) -> Option<Reply> {
    Some(match command {
        Command::Get { kind, id } => Reply::Resource(store.get(kind, id, may_read).cloned()),
        Command::Query(query) => Reply::Page(store.query(query, may_read)),
        Command::Fields { query, fields } => fields_reply(store, query, fields, may_read),
        Command::Count { kind, filters } => Reply::Count(store.count(kind, filters, may_read)),
        Command::Values {
            kind,
            filters,
            field,
        } => Reply::Values(store.values(kind, filters, field, may_read)),
        Command::Content { kind, id } => Reply::Content(
            contents(store, kind, std::slice::from_ref(id), may_read)
                .pop()
                .flatten(),
        ),
        Command::Contents { kind, ids } => Reply::Contents(contents(store, kind, ids, may_read)),
        // A resource's history stays readable after its removal, by whoever could read it.
        Command::Versions { kind, id } => Reply::Versions(
            store
                .readers_of(kind, id)
                .filter(|readers| may_read(readers))
                .map(|_| store.versions(kind, id))
                .unwrap_or_default(),
        ),
        Command::VersionEnd { kind, id, last } => Reply::Versions(
            store
                .readers_of(kind, id)
                .filter(|readers| may_read(readers))
                .and_then(|_| store.version_end(kind, id, *last))
                .into_iter()
                .collect(),
        ),
        Command::At {
            kind,
            id,
            change,
            after,
        } => Reply::At(
            store
                .readers_of(kind, id)
                .filter(|readers| may_read(readers))
                .and_then(|_| store.at_change(kind, id, change, *after)),
        ),
        Command::Grouped { group } => grouped_reply(store, group, may_read),
        Command::Removed { kind, parent } => {
            Reply::Ids(store.removed(kind, parent.as_deref(), may_read))
        }
        Command::MadeFrom { ids } => made_from_reply(store, ids, may_read),
        Command::Newest { within, limit } => {
            let (mut versions, more) = store.newest(within, *limit, may_read);
            // A page of the newest versions of many resources says who changed each, and how,
            // never with what: one call stamps every resource it made with its argument, of up to
            // `MAX_RECORDED_ARGUMENT_BYTES`, so the page would carry it once for each of them, and
            // a window reads the page whenever it draws. The call's signature stays; a reader
            // after the argument reads the resource's own versions.
            for (_, _, version) in &mut versions {
                if let Some(call) = version
                    .stamp
                    .as_mut()
                    .and_then(|stamp| stamp.signed.as_mut())
                {
                    call.argument = None;
                }
            }
            Reply::Newest { versions, more }
        }
        Command::Positions { query, ids } => Reply::Positions(
            ids.iter()
                .map(|id| store.position(query, id, may_read))
                .collect(),
        ),
        Command::PageAt { query, at } => Reply::Page(store.at(query, *at, may_read)),
        Command::ChildrenAt {
            parent,
            change,
            from,
        } => Reply::ChildrenAt(
            store
                .children_at(parent, change, may_read)
                .map(|children| PastChildren::page(children, *from)),
        ),
        Command::Reach { .. } => Reply::Reach(Reach::default()),
        Command::ReviewRestore { resources, change } => Reply::Review(
            store
                .review_restore_held(&readable(store, resources, may_read), change)
                .map_err(|error| error.to_string()),
        ),
        // Every other command makes no store write, or reads a change graph that only its host
        // can: a store answers no read of it.
        _ => return None,
    })
}

/// What was made from each of `ids`, in their order, of only the resources `may_read` admits: the
/// answer [`Command::MadeFrom`] reads.
fn made_from_reply(store: &Store, ids: &[String], may_read: &dyn Fn(&str) -> bool) -> Reply {
    Reply::MadeFrom(
        store
            .made_from_many(ids)
            .into_iter()
            .map(|made| {
                made.into_iter()
                    .filter(|(kind, id)| store.get(kind, id, may_read).is_some())
                    .collect()
            })
            .collect(),
    )
}

/// The content of each of `ids` of `kind`, in their order, of only the resources `may_read`
/// admits: each as [`Reply::Content`] answers one.
fn contents(
    store: &Store,
    kind: &str,
    ids: &[String],
    may_read: &dyn Fn(&str) -> bool,
) -> Vec<Option<Json>> {
    ids.iter()
        .map(|id| {
            store
                .get(kind, id, may_read)
                .and_then(|_| store.content_json(kind, id))
        })
        .collect()
}

/// The page `query` finds, each resource cut down to `fields`.
fn fields_reply(
    store: &Store,
    query: &Query,
    fields: &[String],
    may_read: &dyn Fn(&str) -> bool,
) -> Reply {
    let mut page = store.query(query, may_read);
    for (_, resource) in &mut page.resources {
        resource.fields.retain(|name, _| fields.contains(name));
    }
    Reply::Page(page)
}

/// The versions in `group`, of only the resources `may_read` admits.
fn grouped_reply(store: &Store, group: &str, may_read: &dyn Fn(&str) -> bool) -> Reply {
    Reply::Grouped(
        store
            .grouped(group)
            .into_iter()
            .filter(|(kind, id, _)| {
                store
                    .readers_of(kind, id)
                    .is_some_and(|readers| may_read(&readers))
            })
            .collect(),
    )
}

/// Those of `resources` (by kind and id) that `may_read` admits: what a review of a restore
/// reads.
#[must_use]
pub fn readable(
    store: &Store,
    resources: &[(String, String)],
    may_read: &dyn Fn(&str) -> bool,
) -> Vec<(String, String)> {
    resources
        .iter()
        .filter(|(kind, id)| store.get(kind, id, may_read).is_some())
        .cloned()
        .collect()
}

/// Makes `edit` in the content whose root is `root`.
fn apply(
    tx: &mut Transaction<'_>,
    root: &ObjId,
    edit: &Edit,
) -> Result<(), automerge::AutomergeError> {
    match edit {
        Edit::Set { field, value } => put_json(tx, root, field, value),
        Edit::Splice {
            field,
            at,
            delete,
            insert,
        } => {
            let text = match tx.get(root, field.as_str())? {
                Some((automerge::Value::Object(ObjType::Text), text)) => text,
                _ => tx.put_object(root, field.as_str(), ObjType::Text)?,
            };
            let delete = isize::try_from(*delete).map_err(|_| automerge::AutomergeError::Fail)?;
            tx.splice_text(&text, *at, delete, insert)
        }
        Edit::Add { field, by } => {
            if !matches!(
                tx.get(root, field.as_str())?,
                Some((automerge::Value::Scalar(ref held), _)) if matches!(held.as_ref(), ScalarValue::Counter(_))
            ) {
                tx.put(root, field.as_str(), ScalarValue::counter(0))?;
            }
            tx.increment(root, field.as_str(), *by)
        }
        Edit::MapPut { field, key, value } => {
            let map = match tx.get(root, field.as_str())? {
                Some((automerge::Value::Object(ObjType::Map), map)) => map,
                _ => tx.put_object(root, field.as_str(), ObjType::Map)?,
            };
            put_json(tx, &map, key, value)
        }
        Edit::MapDelete { field, key } => {
            if let Some((automerge::Value::Object(ObjType::Map), map)) =
                tx.get(root, field.as_str())?
                && tx.get(&map, key.as_str())?.is_some()
            {
                tx.delete(&map, key.as_str())?;
            }
            Ok(())
        }
        Edit::Remove { field } => {
            if tx.get(root, field.as_str())?.is_some() {
                tx.delete(root, field.as_str())?;
            }
            Ok(())
        }
    }
}

/// Puts `value` at `key` of the map `obj`: a scalar as itself, an object as a map, an array as a
/// list.
fn put_json(
    tx: &mut Transaction<'_>,
    obj: &ObjId,
    key: &str,
    value: &Json,
) -> Result<(), automerge::AutomergeError> {
    match value {
        Json::Object(fields) => {
            let map = tx.put_object(obj, key, ObjType::Map)?;
            for (name, value) in fields {
                put_json(tx, &map, name, value)?;
            }
            Ok(())
        }
        Json::Array(items) => {
            let list = tx.put_object(obj, key, ObjType::List)?;
            for (at, item) in items.iter().enumerate() {
                insert_json(tx, &list, at, item)?;
            }
            Ok(())
        }
        scalar => tx.put(obj, key, scalar_of(scalar)),
    }
}

fn insert_json(
    tx: &mut Transaction<'_>,
    list: &ObjId,
    at: usize,
    value: &Json,
) -> Result<(), automerge::AutomergeError> {
    match value {
        Json::Object(fields) => {
            let map = tx.insert_object(list, at, ObjType::Map)?;
            for (name, value) in fields {
                put_json(tx, &map, name, value)?;
            }
            Ok(())
        }
        Json::Array(items) => {
            let inner = tx.insert_object(list, at, ObjType::List)?;
            for (index, item) in items.iter().enumerate() {
                insert_json(tx, &inner, index, item)?;
            }
            Ok(())
        }
        scalar => tx.insert(list, at, scalar_of(scalar)),
    }
}

fn scalar_of(value: &Json) -> ScalarValue {
    match value {
        Json::Bool(flag) => ScalarValue::Boolean(*flag),
        Json::Number(number) => number.as_i64().map_or_else(
            || ScalarValue::F64(number.as_f64().unwrap_or_default()),
            ScalarValue::Int,
        ),
        Json::String(text) => ScalarValue::Str(text.as_str().into()),
        Json::Null | Json::Array(_) | Json::Object(_) => ScalarValue::Null,
    }
}

/// What the map `obj` of `doc` holds, as JSON.
pub(crate) fn json_of(doc: &automerge::Automerge, obj: &ObjId) -> Json {
    AsOf { doc, heads: None }.json(obj)
}

/// A document read as it stood at `heads`, or as it stands with none.
pub(crate) struct AsOf<'a> {
    pub doc: &'a automerge::Automerge,
    pub heads: Option<&'a [automerge::ChangeHash]>,
}

impl AsOf<'_> {
    pub(crate) fn get(
        &self,
        obj: &ObjId,
        prop: impl Into<automerge::Prop>,
    ) -> Option<(automerge::Value<'_>, ObjId)> {
        match self.heads {
            Some(heads) => self.doc.get_at(obj, prop, heads),
            None => self.doc.get(obj, prop),
        }
        .ok()
        .flatten()
    }

    pub(crate) fn keys(&self, obj: &ObjId) -> Vec<String> {
        match self.heads {
            Some(heads) => self.doc.keys_at(obj, heads).collect(),
            None => self.doc.keys(obj).collect(),
        }
    }

    pub(crate) fn length(&self, obj: &ObjId) -> usize {
        match self.heads {
            Some(heads) => self.doc.length_at(obj, heads),
            None => self.doc.length(obj),
        }
    }

    fn text(&self, obj: &ObjId) -> String {
        match self.heads {
            Some(heads) => self.doc.text_at(obj, heads),
            None => self.doc.text(obj),
        }
        .unwrap_or_default()
    }

    /// The object `obj` as JSON: texts as strings, counters as numbers.
    pub(crate) fn json(&self, obj: &ObjId) -> Json {
        let value_at = |prop: automerge::Prop| -> Json {
            match self.get(obj, prop) {
                Some((automerge::Value::Object(_), inner)) => self.json(&inner),
                Some((automerge::Value::Scalar(scalar), _)) => match scalar.as_ref() {
                    ScalarValue::Str(text) => Json::String(text.to_string()),
                    ScalarValue::Int(number) => Json::from(*number),
                    ScalarValue::Uint(number) => Json::from(*number),
                    ScalarValue::F64(number) => Json::from(*number),
                    ScalarValue::Counter(counter) => Json::from(i64::from(counter)),
                    ScalarValue::Boolean(flag) => Json::Bool(*flag),
                    _ => Json::Null,
                },
                None => Json::Null,
            }
        };
        match self.doc.object_type(obj) {
            Ok(ObjType::Text) => Json::String(self.text(obj)),
            Ok(ObjType::List) => Json::Array(
                (0..self.length(obj))
                    .map(|at| value_at(automerge::Prop::Seq(at)))
                    .collect(),
            ),
            Ok(ObjType::Map | ObjType::Table) => Json::Object(
                self.keys(obj)
                    .into_iter()
                    .map(|key| {
                        let value = value_at(automerge::Prop::Map(key.clone()));
                        (key, value)
                    })
                    .collect(),
            ),
            Err(_) => Json::Null,
        }
    }
}
