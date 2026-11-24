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
    /// The resource `id` of `kind` as it stood just before the change `change` (a version's
    /// change, its hash in hex), or just after it with `after`.
    At {
        kind: String,
        id: String,
        change: String,
        after: bool,
    },
    /// Every version stamped as a step of the group `group`, of every resource it touched.
    Grouped { group: String },
    /// The ids of the resources of `kind` removed, nested in `parent` when it names one.
    Removed {
        kind: String,
        parent: Option<String>,
    },
    /// The resources made by a change stamped as made from the resource `id`, wherever they
    /// were placed.
    MadeFrom { id: String },
    /// The newest version of each resource `within` names (it, and those whose ids are under
    /// it), of at most `limit` of them, most recent first.
    Newest { within: String, limit: usize },
    /// Where the resource `id` stands in the listing of `query`, read from the index's order
    /// rather than by paging to it: where a reader showing the listing puts a row that came,
    /// and finds one that left.
    Position { query: Query, id: String },
    /// The page of `query` that starts at `at` in its listing, its cursor ignored: the rows a
    /// reader shows far down a listing, read from the index's counts rather than by paging to
    /// them.
    PageAt { query: Query, at: usize },
    /// The first version of the resource `id` of `kind`, or its last with `last`: what a view
    /// showing when it was made or last changed reads, without the rest of its history.
    VersionEnd {
        kind: String,
        id: String,
        last: bool,
    },
    /// The content of each resource `ids` names, of `kind`, in their order: what [`Self::Content`]
    /// answers of each, in one read, for a reader taking the contents of a page of resources.
    Contents { kind: String, ids: Vec<String> },
    /// One page of `query`, each resource holding only the indexed fields `fields` names: what
    /// a reader taking a few fields of every resource of a kind (search, its titles) asks for,
    /// rather than each resource whole.
    Fields { query: Query, fields: Vec<String> },
    /// Take the turn to run the action `address` (`<app>/<resource>#<action>`) on `inputs`,
    /// which runs once in the workspace whichever device runs it; or, with an `answer`, keep that
    /// this device ran it and what it answered, and let the turn go. Answered as [`Reply::Turn`].
    /// No store holds turns: the device's workspace service places them, never [`read`].
    Turn {
        address: String,
        inputs: Json,
        answer: Option<Json>,
    },
    /// This device's changes of the record `record` that no other device keeping the workspace
    /// has taken in yet, and those the workspace refused. Answered as [`Reply::Reach`]. The
    /// device's sync keeps them and its workspace service answers; a store answers none
    /// ([`read`]), as a store on one device alone has no other device to reach.
    Reach { record: String },
    /// The changes of the record the asking app's process opened, as Automerge encodes them, past
    /// the heads `after` (hex; none asks for every change): what the process merges into its own
    /// copy of the record. Answered as [`Reply::RecordChanges`] by the device's workspace service,
    /// which keeps the record and decides the read for the caller; a store answers none
    /// ([`read`]).
    ///
    /// The answer comes in pages: `from` is where in it this page starts, `0` first, and each
    /// page names where the next one starts until the last.
    RecordChanges { after: Vec<String>, from: u64 },
    /// The asking app's own changes of the record its process opened, as Automerge encodes them:
    /// the device's workspace service reads them with the confined parser, decides the write for
    /// the caller, and records them as its own change of the record. Answered as
    /// [`Reply::Written`]; a store makes none ([`write`]).
    ///
    /// `changes` is a run of the changes, concatenated as Automerge saves them incrementally; a
    /// write too large for one command goes as several runs in order, each but the last with
    /// `more`, and the service takes them as one write once the last arrives. `commit` names the
    /// action this write is one part of, when it wrote more than one record: every part carries
    /// it, and every device applies the parts together or not at all.
    RecordChange {
        changes: Bytes,
        more: bool,
        commit: Option<collab_types::WorkspaceCommit>,
    },
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
    /// The resource and its content as they stood at a change, `None` where the store holds no
    /// such change of a resource the person may read.
    At(Option<(Option<Indexed>, Option<Json>)>),
    /// Versions, each with the kind and id of the resource it is a version of.
    Grouped(Vec<(String, String, Version)>),
    /// Ids of resources.
    Ids(Vec<String>),
    /// Resources, by kind and id.
    Resources(Vec<(String, String)>),
    /// Versions, each with the kind and id of the resource it is the newest version of, and
    /// whether there were more than were asked for.
    Newest {
        versions: Vec<(String, String, Version)>,
        more: bool,
    },
    /// Where a resource stands in a listing, `None` where the listing holds none the person may
    /// read.
    Position(Option<usize>),
    /// The content of each resource asked for, in the order asked: as [`Self::Content`] answers
    /// one.
    Contents(Vec<Option<Json>>),
    /// Whether this device runs an action that runs once ([`Command::Turn`]).
    Turn(Turn),
    /// How far this device's changes of a record have gone ([`Command::Reach`]).
    Reach(Reach),
    /// The changes of a record past what the asker holds, and the heads they bring it to
    /// ([`Command::RecordChanges`]).
    ///
    /// `changes` is this page's part of the changes, concatenated as Automerge saves them
    /// incrementally; the asker joins the pages and loads them once `next` is `None`. `next` is
    /// where the next page starts. `heads` is where the whole answer brings the record, the same
    /// on every page of one answer: a page naming other heads belongs to a newer answer, and the
    /// asker starts again from its first page.
    RecordChanges {
        changes: Bytes,
        heads: Vec<String>,
        next: Option<u64>,
    },
}

/// How many bytes of changes one page or run carries: under [`MAX_WIRE_BYTES`] once encoded.
pub const RECORD_PAGE_BYTES: usize = 2 << 20;

/// Bytes carried in a command or reply, encoded as base64 text rather than a list of numbers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

/// Standard base64, written padded and read with or without its padding.
const BASE64: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    base64::engine::GeneralPurposeConfig::new()
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
);

impl Serialize for Bytes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::Engine::encode(&BASE64, &self.0))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        base64::Engine::decode(&BASE64, text)
            .map(Self)
            .map_err(|error| serde::de::Error::custom(format!("bytes are not base64: {error}")))
    }
}

/// This device's changes of one record that no other device keeping the workspace has taken in
/// yet, and those the workspace refused for good, each by its hash in hex. Any other change of
/// the record reached another device, or came from one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reach {
    pub pending: Vec<String>,
    pub refused: Vec<String>,
}

/// Whether this device runs an action that runs once in the workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Turn {
    /// It is this device's turn: run it, then say it is done.
    Take,
    /// Another device runs it, or none may yet: ask again later.
    Later,
    /// A device ran it already, and answered this.
    Done(Json),
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
                | Self::RecordChange { .. }
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

/// Draft the writes a command makes, keeping the live store untouched. A refused batch returns
/// the same prefix as [`write`].
///
/// # Errors
/// When the command is a read or the store refused a step.
pub fn draft(store: &mut Store, command: Command) -> Result<Vec<Written>, Box<Refused>> {
    let command = placed_by_rules(store, command)?;
    let mut scope = DraftScope::default();
    scope.add(&command);
    store.stage(&scope, |staged| write_placed(staged, command))
}

/// The live index rows and placement counters a command may consult while staging.
#[derive(Default)]
pub(crate) struct DraftScope {
    pub(crate) resources: BTreeSet<(String, String)>,
    pub(crate) placements: BTreeSet<(String, String)>,
    pub(crate) undo: bool,
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
            Command::Remove { kind, id, .. } | Command::Edit { kind, id, .. } => {
                self.resources.insert((kind.clone(), id.clone()));
            }
            Command::Undo { .. } => self.undo = true,
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
        | Command::Newest { .. }
        | Command::Position { .. }
        | Command::PageAt { .. }
        | Command::Versions { .. }
        | Command::VersionEnd { .. }
        | Command::At { .. }
        | Command::Grouped { .. }
        | Command::Removed { .. }
        | Command::MadeFrom { .. }
        | Command::Contents { .. }
        | Command::Fields { .. }
        | Command::Turn { .. }
        | Command::Reach { .. }
        | Command::RecordChanges { .. }
        | Command::RecordChange { .. } => return Err(StoreError::NotAWrite),
    }
    Ok(())
}

/// Answers the read `command` from `store`, of only what `may_read` admits; `None` for a write.
#[must_use]
pub fn read(store: &Store, command: &Command, may_read: &dyn Fn(&str) -> bool) -> Option<Reply> {
    Some(match command {
        Command::Get { kind, id } => Reply::Resource(store.get(kind, id, may_read).cloned()),
        Command::Query(query) => Reply::Page(store.query(query, may_read)),
        Command::Fields { query, fields } => {
            let mut page = store.query(query, may_read);
            for (_, resource) in &mut page.resources {
                resource.fields.retain(|name, _| fields.contains(name));
            }
            Reply::Page(page)
        }
        Command::Count { kind, filters } => Reply::Count(store.count(kind, filters, may_read)),
        Command::Content { kind, id } => Reply::Content(
            store
                .get(kind, id, may_read)
                .and_then(|_| store.content_json(kind, id)),
        ),
        Command::Contents { kind, ids } => Reply::Contents(
            ids.iter()
                .map(|id| {
                    store
                        .get(kind, id, may_read)
                        .and_then(|_| store.content_json(kind, id))
                })
                .collect(),
        ),
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
        Command::Grouped { group } => Reply::Grouped(
            store
                .grouped(group)
                .into_iter()
                .filter(|(kind, id, _)| {
                    store
                        .readers_of(kind, id)
                        .is_some_and(|readers| may_read(&readers))
                })
                .collect(),
        ),
        Command::Removed { kind, parent } => {
            Reply::Ids(store.removed(kind, parent.as_deref(), may_read))
        }
        Command::MadeFrom { id } => Reply::Resources(
            store
                .made_from(id)
                .into_iter()
                .filter(|(kind, id)| store.get(kind, id, may_read).is_some())
                .collect(),
        ),
        Command::Newest { within, limit } => {
            let (versions, more) = store.newest(within, *limit, may_read);
            Reply::Newest { versions, more }
        }
        Command::Position { query, id } => Reply::Position(store.position(query, id, may_read)),
        Command::PageAt { query, at } => Reply::Page(store.at(query, *at, may_read)),
        Command::Reach { .. } => Reply::Reach(Reach::default()),
        Command::Put { .. }
        | Command::Remove { .. }
        | Command::Undo { .. }
        | Command::Edit { .. }
        | Command::Batch(_)
        | Command::Turn { .. }
        | Command::RecordChanges { .. }
        | Command::RecordChange { .. } => return None,
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

#[cfg(test)]
mod bytes_tests {
    use super::Bytes;

    /// Every length's bytes come back from their text whole, and the text is base64's.
    #[test]
    fn bytes_travel_as_base64_text() {
        assert_eq!(
            serde_json::to_string(&Bytes(b"Man".to_vec())).unwrap(),
            "\"TWFu\""
        );
        assert_eq!(
            serde_json::to_string(&Bytes(b"Ma".to_vec())).unwrap(),
            "\"TWE=\""
        );
        for len in 0..300 {
            let bytes = Bytes(
                (0..len)
                    .map(|at: usize| u8::try_from(at * 7 % 256).unwrap())
                    .collect(),
            );
            let text = serde_json::to_string(&bytes).unwrap();
            assert_eq!(serde_json::from_str::<Bytes>(&text).unwrap(), bytes);
        }
        assert!(serde_json::from_str::<Bytes>("\"T!==\"").is_err());
    }
}
