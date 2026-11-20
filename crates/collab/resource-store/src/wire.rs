//! What an app asks of the device's one store, and what it is answered, as they cross the app's
//! connection. An app holds no store of its own: it writes and reads resources by kind and id,
//! and the device answers from its store, as the person the app acts for may read.
//!
//! The store carries a command out itself: [`write`] makes a write and answers each document's
//! change, which the device publishes as its own; [`read`] answers a read of only what the
//! reader check admits. Redo is the undo of an undo: the undo's own invocation, taken back.

use automerge::transaction::{Transactable as _, Transaction};
use automerge::{ObjId, ObjType, ReadDoc as _, ScalarValue};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::admit::Stamp;
use crate::history::Version;
use crate::index::Indexed;
use crate::query::{Filter, Page, Query};
use crate::store::{Resource, Store, StoreError, Written};

/// The most a command or its answer takes on the wire.
pub const MAX_WIRE_BYTES: usize = 4 << 20;

/// One thing an app asks of the store.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Command {
    /// Write the resource `id` of `kind`, as the action `stamp` names.
    Put {
        kind: String,
        id: String,
        resource: Resource,
        stamp: Stamp,
    },
    /// Remove the resource `id` of `kind`, as `stamp` names.
    Remove {
        kind: String,
        id: String,
        stamp: Stamp,
    },
    /// Take back the invocation `invocation`, as `stamp` names.
    Undo { invocation: String, stamp: Stamp },
    /// The resource `id` of `kind`.
    Get { kind: String, id: String },
    /// One page of `query`.
    Query(Query),
    /// How many resources of `kind` hold to `filters`.
    Count { kind: String, filters: Vec<Filter> },
    /// Edit the content of the resource `id` of `kind` (its texts, counters, lists and maps), in
    /// one change, as `stamp` names: two writers' edits of it merge.
    Edit {
        kind: String,
        id: String,
        edits: Vec<Edit>,
        stamp: Stamp,
    },
    /// The content of the resource `id` of `kind`, as JSON: texts as strings, counters as
    /// numbers.
    Content { kind: String, id: String },
    /// Several writes, as one: each is made in turn, and the first that fails stops them.
    Batch(Vec<Command>),
    /// The versions of the resource `id` of `kind`, oldest first.
    Versions { kind: String, id: String },
}

/// One edit of a resource's content.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Edit {
    /// `field` holds `value` whole: a scalar, or maps and lists of them.
    Set { field: String, value: Json },
    /// In the text `field`, made when it holds none, `delete` characters at `at` give way to
    /// `insert`.
    Splice {
        field: String,
        at: usize,
        delete: usize,
        insert: String,
    },
    /// The counter `field`, made at zero when it holds none, grows by `by`.
    Add { field: String, by: i64 },
    /// In the map `field`, made when it holds none, `key` holds `value`.
    MapPut {
        field: String,
        key: String,
        value: Json,
    },
    /// In the map `field`, `key` holds nothing.
    MapDelete { field: String, key: String },
    /// `field` holds nothing.
    Remove { field: String },
}

/// What the store answered.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Reply {
    /// The write was made.
    Written,
    /// The resource, `None` where there is none the person may read.
    Resource(Option<Indexed>),
    Page(Page),
    Count(usize),
    /// The content, `None` where there is none the person may read or none was written.
    Content(Option<Json>),
    Versions(Vec<Version>),
}

impl Command {
    /// Whether it writes: what [`write`] carries out, and [`read`] does not.
    #[must_use]
    pub const fn writes(&self) -> bool {
        matches!(
            self,
            Self::Put { .. }
                | Self::Remove { .. }
                | Self::Undo { .. }
                | Self::Edit { .. }
                | Self::Batch(_)
        )
    }
}

/// A write the store refused partway: a batch keeps what it wrote before the refusal, which is
/// in the store and is carried to the other devices as any write.
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
    let mut written = Vec::new();
    match write_into(store, command, &mut written) {
        Ok(()) => Ok(written),
        Err(error) => Err(Box::new(Refused { error, written })),
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
        Command::Get { .. }
        | Command::Query(_)
        | Command::Count { .. }
        | Command::Content { .. }
        | Command::Versions { .. } => return Err(StoreError::NotAWrite),
    }
    Ok(())
}

/// Answers the read `command` from `store`, of only what `may_read` admits; `None` for a write.
#[must_use]
pub fn read(store: &Store, command: &Command, may_read: &dyn Fn(&str) -> bool) -> Option<Reply> {
    Some(match command {
        Command::Get { kind, id } => Reply::Resource(store.get(kind, id, may_read).cloned()),
        Command::Query(query) => Reply::Page(store.query(query, may_read)),
        Command::Count { kind, filters } => Reply::Count(store.count(kind, filters, may_read)),
        Command::Content { kind, id } => Reply::Content(
            store
                .get(kind, id, may_read)
                .and_then(|_| store.content_json(kind, id)),
        ),
        Command::Versions { kind, id } => Reply::Versions(
            store
                .get(kind, id, may_read)
                .map(|_| store.versions(kind, id))
                .unwrap_or_default(),
        ),
        Command::Put { .. }
        | Command::Remove { .. }
        | Command::Undo { .. }
        | Command::Edit { .. }
        | Command::Batch(_) => return None,
    })
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
    let value_at = |prop: automerge::Prop| -> Json {
        match doc.get(obj, prop).ok().flatten() {
            Some((automerge::Value::Object(_), inner)) => json_of(doc, &inner),
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
    match doc.object_type(obj) {
        Ok(ObjType::Text) => Json::String(doc.text(obj).unwrap_or_default()),
        Ok(ObjType::List) => Json::Array(
            (0..doc.length(obj))
                .map(|at| value_at(automerge::Prop::Seq(at)))
                .collect(),
        ),
        Ok(ObjType::Map | ObjType::Table) => Json::Object(
            doc.keys(obj)
                .map(|key| {
                    let value = value_at(automerge::Prop::Map(key.clone()));
                    (key, value)
                })
                .collect(),
        ),
        Err(_) => Json::Null,
    }
}
