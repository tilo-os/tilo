//! What the index holds, derived from the workspace record: a package's records whole, or what a
//! set of changes reached, field by field.
//!
//! The same derivation keeps the index on disk ([`super::Writer`]) and the store's open writes
//! ([`super::overlay::Overlay`]) as the record holds them: both are a [`Sink`].

use std::collections::{BTreeMap, BTreeSet};

use automerge::{ObjId, ReadDoc};
use tilolang_runtime::ids::{AppId, DocId, ItemId, TypeId};
use tilolang_runtime::place::{Place, Resolved};
use tilolang_runtime::store::{
    ANY_FIELD, FieldKey, FieldValue, LINKS, Left, LinkHow, Scalar, Version, VersionKind, doc_of,
};

use super::columns::{
    self, reversed, scope_entry, scope_list, scope_prefix, type_entry, type_list,
};
use super::lists::Entry;
use super::reflect::{self, CREATED, Ends, Stamp, UPDATED};
use super::{CHUNK, CYCLES, IN, IndexError, KEY, LEFT, TYPE, Writer, removed_field};
use crate::chunk::{ChunkNo, Chunks};
use crate::items::{Found, Items, Nested, fits};
use crate::reads::{Placed, default_value, field_of_slot, type_at};
use crate::schema::{FieldSchema, MergeKind, TypeSchema, lookup};

type Doc = tilo_resource::Doc;

/// The list of the items of the collection `field` of the resource `resource`.
#[must_use]
pub fn member_list(resource: &str, field: &str) -> String {
    format!("m\u{1}{resource}\u{1}{field}")
}

/// The list of what links to the resource `target`, by how and by the resource linking to it:
/// where [`Derive`] keeps the answer of [`tilolang_runtime::store::Store::linked`], one list per
/// target, read as one list rather than by walking the workspace.
#[must_use]
pub fn linked_list(target: &str) -> String {
    format!("l\u{1}{target}")
}

/// The entry of the resource `source` in the [`linked_list`] of a target it is linked to `how`.
fn link_entry(how: &str, source: &str) -> Entry {
    (format!("{how}{}{source}", columns::SEP), source.to_owned())
}

/// The how a [`linked_list`] entry's key names, and the resource it is of.
#[must_use]
pub fn split_link(key: &str) -> Option<(LinkHow, &str)> {
    let (how, source) = key.split_once(columns::SEP)?;
    Some((LinkHow::of_word(how)?, source))
}

/// The links the field value `value` makes to other resources: those its rich text mentions, or
/// those the value a reference register holds names.
fn linked_by(value: &FieldValue) -> Vec<String> {
    match value {
        FieldValue::Rich(rich) => rich
            .references()
            .iter()
            .filter_map(|reference| {
                tilolang_runtime::rich::resource_id(reference).map(str::to_owned)
            })
            .collect(),
        FieldValue::Scalar(Scalar::Json(json)) => tilolang_runtime::value::from_json(json)
            .map_or_else(
                |_| Vec::new(),
                |value| tilolang_runtime::value::references_in(&value),
            ),
        _ => Vec::new(),
    }
}

/// Take the links the resource `resource` makes out of the [`linked_list`]s of their targets,
/// before what it holds goes.
pub(super) fn unlink(sink: &mut impl Sink, resource: &str) -> Result<(), IndexError> {
    if let Some(held) = sink.text(resource, LINKS)? {
        unlink_held(sink, resource, &held)?;
    }
    Ok(())
}

/// [`unlink`] for the `$links` text `held` of `resource`, already read.
fn unlink_held(sink: &mut impl Sink, resource: &str, held: &str) -> Result<(), IndexError> {
    for pair in split_values(held).chunks(2) {
        let [how, target] = pair else {
            continue;
        };
        sink.remove_entry(&linked_list(target), &link_entry(how, resource))?;
    }
    Ok(())
}

/// The pseudo-field holding the values a resource's column `column` holds, as the column's
/// lists have them: what a change replaces there, and a removal takes out.
#[must_use]
pub fn column_field(column: &str) -> String {
    format!("$c\u{1}{column}")
}

/// The values held under a [`column_field`].
pub(super) fn split_values(held: &str) -> Vec<String> {
    held.split(columns::SEP).map(str::to_owned).collect()
}

fn join_values(values: &[String]) -> String {
    let mut out = String::new();
    for (at, value) in values.iter().enumerate() {
        if at > 0 {
            out.push(columns::SEP);
        }
        out.push_str(value);
    }
    out
}

/// The prefixes of the lists of the resource `resource` and of everything inside it.
fn lists_within(resource: &str) -> [String; 4] {
    [
        format!("m\u{1}{resource}\u{1}"),
        format!("m\u{1}{resource}/"),
        format!("c\u{1}{resource}\u{1}"),
        format!("c\u{1}{resource}/"),
    ]
}

/// The resource of the item `id` of the record `doc` (or of the record a resource `doc/...` is
/// in): `doc/id`, wherever the item is.
#[must_use]
pub fn item_path(doc: &str, id: &str) -> String {
    format!("{}/{id}", doc_of(doc))
}

/// The id of the item at `path`; `None` for a record.
#[must_use]
pub fn id_of(path: &str) -> Option<&str> {
    path.split_once('/').map(|(_, id)| id)
}

/// What [`IN`] holds of an item in the collection `field` of `holder`.
#[must_use]
pub fn in_of(holder: &str, field: &str) -> String {
    format!("{holder}\u{1}{field}")
}

/// The holder and field an [`IN`] holds.
#[must_use]
pub fn split_in(held: &str) -> Option<(&str, &str)> {
    held.split_once('\u{1}')
}

/// Where the item at `path` is, as `sink` holds it: the resource holding its collection, the
/// collection's field and its order key there. `None` for an item in no collection.
pub fn placed_in(
    sink: &mut impl Sink,
    path: &str,
) -> Result<Option<(String, String, String)>, IndexError> {
    let (Some(held), Some(key)) = (sink.text(path, IN)?, sink.text(path, KEY)?) else {
        return Ok(None);
    };
    Ok(split_in(&held).map(|(holder, field)| (holder.to_owned(), field.to_owned(), key)))
}

/// Where what is derived goes: the index being written, or the store's open writes.
pub trait Sink {
    /// The value the sink holds of `field` of `resource`.
    fn field(&mut self, resource: &str, field: &str) -> Result<Option<FieldValue>, IndexError>;

    /// The type (`$type`) or order key (`$key`) of `resource`.
    fn text(&mut self, resource: &str, key: &str) -> Result<Option<String>, IndexError>;

    /// # Errors
    /// It cannot be written.
    fn put_field(
        &mut self,
        resource: &str,
        field: &str,
        value: &FieldValue,
    ) -> Result<(), IndexError>;

    /// # Errors
    /// It cannot be written.
    fn put_text(&mut self, resource: &str, key: &str, text: &str) -> Result<(), IndexError>;

    /// Drop the text `key` of `resource`: what the index holds there is no more. A sink that
    /// holds this pass's writes over the index keeps the empty text, which reads as none.
    fn drop_text(&mut self, resource: &str, key: &str) -> Result<(), IndexError> {
        self.put_text(resource, key, "")
    }

    /// Keep `stamp`'s version as the newest version of `resource`, unless one it holds comes
    /// after it ([`super::Snapshot::newest`]): what a mark of it, or of the document holding
    /// it, reads without the resource's log.
    fn put_newest(&mut self, resource: &str, stamp: &Stamp) -> Result<(), IndexError>;

    /// Drop `resource` and everything inside it: fields, lists, and what search finds them by.
    /// The newest version kept of it stays, as the record's log keeps its versions.
    fn remove(&mut self, resource: &str) -> Result<(), IndexError>;

    /// Drop `resource` and everything inside it to derive it anew: as [`Sink::remove`], but
    /// the versions kept of each ([`reflect`]) stay, as they are not in the record.
    fn clear(&mut self, resource: &str) -> Result<(), IndexError> {
        self.remove(resource)
    }

    /// Drop what is held of the resources within the record `doc` that hold no type: the
    /// versions [`Sink::clear`] kept of items a derivation anew finds in no collection.
    fn prune(&mut self, doc: &str) -> Result<(), IndexError> {
        let _ = doc;
        Ok(())
    }

    /// That the package's record `doc` is of the type `ty` (else `None`) and stored in the chunk
    /// `chunk`.
    fn doc(&mut self, doc: &str, ty: Option<&str>, chunk: ChunkNo) -> Result<(), IndexError>;

    /// # Errors
    /// It cannot be written.
    fn insert(&mut self, list: &str, entry: Entry) -> Result<(), IndexError>;

    /// # Errors
    /// It cannot be written.
    fn remove_entry(&mut self, list: &str, entry: &Entry) -> Result<(), IndexError>;

    /// # Errors
    /// It cannot be written.
    fn replace(&mut self, list: &str, entries: Vec<Entry>) -> Result<(), IndexError>;

    /// Every entry of `list`.
    fn entries(&mut self, list: &str) -> Result<Vec<Entry>, IndexError>;

    /// How many entries `list` holds.
    fn len(&mut self, list: &str) -> Result<usize, IndexError>;

    /// Add `delta` to how many entries of the column list `list` hold `value`
    /// ([`columns::counted`]).
    fn count(&mut self, list: &str, value: &str, delta: i64) -> Result<(), IndexError>;

    /// Drop every list whose name starts with `prefix`.
    fn drop_lists(&mut self, prefix: &str) -> Result<(), IndexError>;

    /// Whether it keeps filter columns, or (the store's open writes) fields and membership
    /// alone.
    fn keeps_columns(&self) -> bool;

    /// Record the instant the columns of `ty` that read the clock are written at
    /// ([`super::clock`]), unless the index records one: the first one written is the one every
    /// later one is past. A sink that keeps no columns of its own (`keeps_columns`) records
    /// nothing.
    fn set_written_at(&mut self, ty: &str, clock: super::Clock) -> Result<(), IndexError> {
        let _ = (ty, clock);
        Ok(())
    }
}

impl Sink for Writer<'_> {
    fn put_newest(&mut self, resource: &str, stamp: &Stamp) -> Result<(), IndexError> {
        Writer::put_newest(self, resource, stamp)
    }

    fn field(&mut self, resource: &str, field: &str) -> Result<Option<FieldValue>, IndexError> {
        Writer::field(self, resource, field)
    }

    fn text(&mut self, resource: &str, key: &str) -> Result<Option<String>, IndexError> {
        Writer::text(self, resource, key)
    }

    fn put_field(
        &mut self,
        resource: &str,
        field: &str,
        value: &FieldValue,
    ) -> Result<(), IndexError> {
        Writer::put_field(self, resource, field, value)
    }

    fn put_text(&mut self, resource: &str, key: &str, text: &str) -> Result<(), IndexError> {
        Writer::put_text(self, resource, key, text)
    }

    fn drop_text(&mut self, resource: &str, key: &str) -> Result<(), IndexError> {
        self.remove_field(resource, key)
    }

    fn remove(&mut self, resource: &str) -> Result<(), IndexError> {
        let held_fields = self.subtree(resource)?;
        // Out of the type's columns across the workspace, as each resource holds them.
        let mut types: BTreeMap<&str, String> = BTreeMap::new();
        for (held, field, bytes) in &held_fields {
            if field == TYPE {
                types.insert(held.as_str(), super::codec::get_text(bytes)?);
            }
        }
        for (held, field, bytes) in &held_fields {
            if field == LINKS {
                unlink_held(self, held, &super::codec::get_text(bytes)?)?;
            }
        }
        for (held, field, bytes) in &held_fields {
            let Some(name) = field.strip_prefix("$c\u{1}") else {
                continue;
            };
            let Some(column) = types
                .get(held.as_str())
                .and_then(|ty| lookup(ty))
                .and_then(|schema| schema.column(name))
                .filter(|column| column.filter)
            else {
                continue;
            };
            let ty = types.get(held.as_str()).cloned().unwrap_or_default();
            for value in split_values(&super::codec::get_text(bytes)?) {
                self.list_remove(&type_list(&ty, name), &type_entry(&value, held))?;
                if column.kind == crate::schema::ColumnKind::Ident {
                    self.list_remove(
                        &super::columns::reversed_list(&ty, name),
                        &type_entry(&reversed(&value), held),
                    )?;
                }
            }
        }
        for (held, field, _) in held_fields {
            self.remove_field(&held, &field)?;
        }
        for prefix in lists_within(resource) {
            self.drop_lists(&prefix)?;
        }
        Ok(())
    }

    fn clear(&mut self, resource: &str) -> Result<(), IndexError> {
        let kept: Vec<(String, String, Vec<u8>)> = self
            .subtree(resource)?
            .into_iter()
            .filter(|(_, field, _)| reflect::is_reflected(field) || field == LEFT)
            .collect();
        Sink::remove(self, resource)?;
        for (held, field, bytes) in kept {
            Writer::put_field(self, &held, &field, &super::codec::get_value(&bytes)?)?;
        }
        Ok(())
    }

    fn prune(&mut self, doc: &str) -> Result<(), IndexError> {
        let held = self.subtree(doc)?;
        let typed: BTreeSet<&str> = held
            .iter()
            .filter(|(_, field, _)| field == TYPE)
            .map(|(resource, _, _)| resource.as_str())
            .collect();
        // An item the record places in no collection keeps where it left from, which the next
        // removal of it reads: its other fields go.
        let stale: Vec<(String, String)> = held
            .iter()
            .filter(|(resource, field, _)| !typed.contains(resource.as_str()) && field != LEFT)
            .map(|(resource, field, _)| (resource.clone(), field.clone()))
            .collect();
        for (resource, field) in stale {
            self.remove_field(&resource, &field)?;
        }
        Ok(())
    }

    fn doc(&mut self, doc: &str, ty: Option<&str>, chunk: ChunkNo) -> Result<(), IndexError> {
        match ty {
            Some(ty) => self.put_doc(doc, ty, chunk),
            None => self.remove_doc(doc),
        }
    }

    fn insert(&mut self, list: &str, entry: Entry) -> Result<(), IndexError> {
        self.list_insert(list, entry)
    }

    fn remove_entry(&mut self, list: &str, entry: &Entry) -> Result<(), IndexError> {
        self.list_remove(list, entry)
    }

    fn replace(&mut self, list: &str, entries: Vec<Entry>) -> Result<(), IndexError> {
        self.list_replace(list, entries)
    }

    fn entries(&mut self, list: &str) -> Result<Vec<Entry>, IndexError> {
        self.list(list)
    }

    fn len(&mut self, list: &str) -> Result<usize, IndexError> {
        self.list_len(list)
    }

    fn count(&mut self, list: &str, value: &str, delta: i64) -> Result<(), IndexError> {
        Writer::count(self, list, value, delta)
    }

    fn drop_lists(&mut self, prefix: &str) -> Result<(), IndexError> {
        Writer::drop_lists(self, prefix)
    }

    fn keeps_columns(&self) -> bool {
        true
    }

    fn set_written_at(&mut self, ty: &str, clock: super::Clock) -> Result<(), IndexError> {
        Writer::note_written_at(self, ty, clock)
    }
}

/// What derivation read of the record, counted: work that does not depend on the machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DocReads {
    /// Resources found by walking the record.
    pub located: u64,
    /// Field values read.
    pub fields: u64,
    /// Collections listed by walking their entries.
    pub listings: u64,
    /// Collection entries read.
    pub entries: u64,
}

impl DocReads {
    /// These reads and `other`'s.
    pub fn take_in(&mut self, other: Self) {
        self.located = self.located.saturating_add(other.located);
        self.fields = self.fields.saturating_add(other.fields);
        self.listings = self.listings.saturating_add(other.listings);
        self.entries = self.entries.saturating_add(other.entries);
    }
}

/// Derivation over one record.
pub struct Derive<'d> {
    /// The documents the store holds: a resource is read in the chunk the record's manifest
    /// places it in.
    chunks: &'d Chunks,
    /// Every record of the workspace: its app and type.
    docs: &'d BTreeMap<DocId, (AppId, TypeId)>,
    /// Where the items of records written before items were flat are.
    nested: &'d Nested,
    pub reads: DocReads,
    /// Resources derived whole in this pass: a change naming them again is done.
    done: BTreeSet<String>,
    /// The columns written as the fields are, and the time a column reading the clock is
    /// written at.
    columns: Columns,
    clock: super::Clock,
    /// The collections whose items it added, removed or moved, with those items: what a
    /// listing of each is brought in line by.
    pub moved: BTreeMap<FieldKey, BTreeSet<String>>,
    /// The records it derived anew whole while deriving changes, where a change could not be
    /// taken in item by item: everything read of them may have changed.
    pub wholes: BTreeSet<DocId>,
    /// Fields a change moved on resources it did not name, per document: a link target's
    /// [`LINKS`] when what links to it moved, so a view reading them runs again.
    pub notified: BTreeMap<DocId, Vec<FieldKey>>,
    /// The items the changes removed from a collection, with where they left from and the
    /// version that removed them: what a row of what left it is written from once the changes
    /// are derived, when the item is in no collection.
    left: Vec<(String, FieldKey, Stamp, Option<String>)>,
    /// The chunk each item's map is stored in, by its path in the index: a collection derives
    /// where the items of its runs are, and each item's fields are read there.
    chunks_of: BTreeMap<String, ChunkNo>,
    /// The chunks the step derives, when it derives some: a change is read of the chunks it is
    /// in, and each item it names is read where those chunks hold it.
    part: Option<&'d [ChunkNo]>,
    /// Where the items of the chunks the step derives are, by record and id: read once, so that
    /// deriving each item a change names is a lookup rather than a search of every chunk the
    /// store holds (which, an item at a time, is the workspace read once an item).
    found: BTreeMap<DocId, BTreeMap<ItemId, Found>>,
    /// The chunks the step derives a few changes of, not read whole: an item read from `found`
    /// is looked up in these as well, as one of their changes may have moved a copy of it there.
    looked: Vec<ChunkNo>,
    /// Whether a record derives only the items of the chunks the step reads: the first step of
    /// a build, which is where the window's first rows are, the rest of the record derived as
    /// the chunks holding it are read.
    opening: bool,
}

/// The collection an item is in, and its order key: where its columns' entries go.
type Scope<'a> = (&'a str, &'a str, &'a str);

/// What a set of changes reached: per document the fields (none: the document as a whole),
/// and per document the items whose placement was written, each with whether the changes
/// made it.
#[derive(Clone, Debug, Default)]
pub struct Reached {
    pub docs: BTreeMap<DocId, Vec<FieldKey>>,
    pub placed: Placed,
}

impl Reached {
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty() && self.placed.is_empty()
    }
}

/// A record's items, as a whole derivation reads them.
struct Whole<'a> {
    doc: &'a str,
    items: &'a Items,
    resolved: &'a Resolved,
}

/// Write what `end` holds of the resource's versions into an empty sink: the first and the
/// last version, and when each field last changed ([`reflect::Ends`]).
fn write_ends(sink: &mut impl Sink, resource: &str, end: &Ends) -> Result<(), IndexError> {
    sink.put_field(resource, CREATED, &end.first.value())?;
    sink.put_field(resource, UPDATED, &end.last.value())?;
    for (field, stamp) in &end.fields {
        sink.put_field(resource, &reflect::changed_field(field), &stamp.value())?;
    }
    Ok(())
}

/// A row of what left a collection: the item, and the version that removed it, which says
/// when that happened and where the change stands causally.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LeftRow {
    pub path: String,
    pub version: Version,
}

impl LeftRow {
    /// What orders it among the rows of what left a collection, as [`Version`] orders versions.
    fn order(&self) -> (&str, u64, &str) {
        self.version.order()
    }

    /// Whether it left before `other`.
    fn before(&self, other: &Self) -> bool {
        self.order() < other.order()
    }
}

/// The items that left each collection, by the resource holding the collection and its field,
/// in the order they left, each with the version that removed it ([`super::REMOVED`]).
pub type Removed = BTreeMap<(String, String), Vec<LeftRow>>;

/// The items that left the collection `field` of `holder`, as `sink` holds them.
fn read_removed(
    sink: &mut impl Sink,
    holder: &str,
    field: &str,
) -> Result<Vec<LeftRow>, IndexError> {
    let Some(json) = sink.text(holder, &removed_field(field))? else {
        return Ok(Vec::new());
    };
    serde_json::from_str(&json).map_err(|_| IndexError::Corrupt)
}

/// Keep `left` as the items that left the collection `field` of `holder` ([`super::REMOVED`]).
fn write_removed(
    sink: &mut impl Sink,
    holder: &str,
    field: &str,
    left: &[LeftRow],
) -> Result<(), IndexError> {
    let mut left = left.to_vec();
    left.sort_by(|one, other| one.order().cmp(&other.order()));
    let json = serde_json::to_string(&left).map_err(|_| IndexError::Corrupt)?;
    sink.put_text(holder, &removed_field(field), &json)
}

/// The type an item of the collection `key` is, as the version that placed one recorded it:
/// what a row of what left it shows is.
fn collection_ty(sink: &mut impl Sink, key: &FieldKey) -> Result<Option<String>, IndexError> {
    Ok(sink
        .text(&key.resource, TYPE)?
        .and_then(|held| lookup(&held))
        .and_then(|schema| {
            schema
                .fields
                .iter()
                .find(|field| field.id == key.field)
                .and_then(|field| field.record.clone())
        }))
}

/// Keep the item at `path` a row of what left the collection `key` names, with the version
/// `end` holds of it; the later removal of it stays.
fn note_left(
    sink: &mut impl Sink,
    key: &FieldKey,
    path: &str,
    stamp: &Stamp,
    ty: Option<&str>,
) -> Result<(), IndexError> {
    let mut left = read_removed(sink, &key.resource, &key.field)?;
    let row = left_version(stamp, path, key, ty.map(str::to_owned));
    match left.iter().position(|held| held.path == path) {
        Some(at) => {
            if let Some(held) = left.get_mut(at)
                && held.before(&row)
            {
                *held = row;
            }
        }
        None => left.push(row),
    }
    write_removed(sink, &key.resource, &key.field, &left)?;
    sink.put_text(path, LEFT, &in_of(&key.resource, &key.field))
}

/// Drop the item at `path` from what left the collection `key` names, when it is a row of the
/// collection again.
fn drop_left(sink: &mut impl Sink, key: &FieldKey, path: &str) -> Result<(), IndexError> {
    let mut left = read_removed(sink, &key.resource, &key.field)?;
    let Some(at) = left.iter().position(|held| held.path == path) else {
        return Ok(());
    };
    left.remove(at);
    write_removed(sink, &key.resource, &key.field, &left)?;
    sink.drop_text(path, LEFT)
}

/// The item at `path` is in a collection again: the row of what left the collection it left
/// from goes, and where it left from is forgotten ([`LEFT`]).
fn returned(sink: &mut impl Sink, path: &str) -> Result<(), IndexError> {
    let Some(held) = sink.text(path, LEFT)? else {
        return Ok(());
    };
    if let Some((holder, field)) = split_in(&held) {
        let key = FieldKey {
            resource: holder.to_owned(),
            field: field.to_owned(),
        };
        drop_left(sink, &key, path)?;
    }
    sink.drop_text(path, LEFT)
}

/// The items that left the collections `schema` holds in `resource`, as `sink` keeps them:
/// what a derivation anew of it writes back, as the record holds where an item is now, never
/// where it was.
fn kept_removed(
    sink: &mut impl Sink,
    resource: &str,
    schema: &TypeSchema,
) -> Result<Vec<(String, String)>, IndexError> {
    let mut held = Vec::new();
    for field in schema
        .fields
        .iter()
        .filter(|field| field.kind == MergeKind::Collection)
    {
        let name = removed_field(&field.id);
        if let Some(json) = sink.text(resource, &name)? {
            held.push((name, json));
        }
    }
    Ok(held)
}

/// Write back what [`kept_removed`] read off a resource before it was derived anew.
fn restore_removed(
    sink: &mut impl Sink,
    resource: &str,
    held: &[(String, String)],
) -> Result<(), IndexError> {
    for (field, json) in held {
        sink.put_text(resource, field, json)?;
    }
    Ok(())
}

/// The value of `field` of the resource at `path` as the sink holds it, its default when it
/// holds none; a collection as the [`FieldValue::Counter`] of its items.
pub(super) fn field_value(
    sink: &mut impl Sink,
    path: &str,
    field: &FieldSchema,
) -> Result<FieldValue, IndexError> {
    if field.kind == MergeKind::Collection {
        let len = sink.len(&member_list(path, &field.id))?;
        return Ok(FieldValue::Counter(i64::try_from(len).unwrap_or(i64::MAX)));
    }
    Ok(sink
        .field(path, &field.id)?
        .unwrap_or_else(|| default_value(field)))
}

/// Writing the columns of a resource as its fields are: what a derivation does, and what the
/// index does again for the items a later clock changes the answer of a filter for
/// ([`super::clock`]).
#[derive(Default)]
pub(super) struct Columns {
    /// While a whole package is derived into an empty index: the entries of its types'
    /// columns across the workspace, each list written whole at the end rather than entry by
    /// entry.
    bulk: Option<BTreeMap<String, Vec<Entry>>>,
}

impl Columns {
    /// Bring the entries of one column of the resource at `path` (of the type `schema`) in line
    /// with the values `now` it holds: the column's across the workspace, the collection's, the
    /// open counts a place reads, and what it held.
    pub(super) fn write(
        &mut self,
        sink: &mut impl Sink,
        path: &str,
        schema: &TypeSchema,
        column: &crate::schema::ColumnSchema,
        scope: Option<Scope<'_>>,
        now: &[String],
    ) -> Result<(), IndexError> {
        let held = sink
            .text(path, &column_field(&column.name))?
            .map(|h| split_values(&h));
        if held.as_deref() == Some(now) {
            return Ok(());
        }
        let before = held.unwrap_or_default();
        if column.filter {
            let ty = &schema.id;
            for value in before.iter().filter(|v| !now.contains(v)) {
                sink.remove_entry(&type_list(ty, &column.name), &type_entry(value, path))?;
                if column.kind == crate::schema::ColumnKind::Ident {
                    sink.remove_entry(
                        &columns::reversed_list(ty, &column.name),
                        &type_entry(&reversed(value), path),
                    )?;
                }
            }
            for value in now.iter().filter(|v| !before.contains(v)) {
                self.add(sink, type_list(ty, &column.name), type_entry(value, path))?;
                if column.kind == crate::schema::ColumnKind::Ident {
                    self.add(
                        sink,
                        columns::reversed_list(ty, &column.name),
                        type_entry(&reversed(value), path),
                    )?;
                }
            }
        }
        if let Some((parent, coll, key)) = scope
            && let Some(id) = id_of(path)
        {
            let list = scope_list(parent, coll, &column.name);
            let counted = columns::counted(column);
            for value in before.iter().filter(|v| !now.contains(v)) {
                sink.remove_entry(&list, &scope_entry(value, key, id))?;
                if counted {
                    sink.count(&list, value, -1)?;
                }
            }
            for value in now.iter().filter(|v| !before.contains(v)) {
                sink.insert(&list, scope_entry(value, key, id))?;
                if counted {
                    sink.count(&list, value, 1)?;
                }
            }
        }
        sink.put_text(path, &column_field(&column.name), &join_values(now))?;
        Ok(())
    }

    /// Put `entry` in `list`: now, or with the rest of a whole derivation's.
    fn add(&mut self, sink: &mut impl Sink, list: String, entry: Entry) -> Result<(), IndexError> {
        match &mut self.bulk {
            Some(bulk) => {
                bulk.entry(list).or_default().push(entry);
                Ok(())
            }
            None => sink.insert(&list, entry),
        }
    }
}

/// The version that removed the item at `path` from the collection `key` names, with where it
/// stood: what a list shows of it struck ([`super::REMOVED`]).
fn left_version(stamp: &Stamp, path: &str, key: &FieldKey, ty: Option<String>) -> LeftRow {
    let mut version = stamp.version.clone();
    path.clone_into(&mut version.resource);
    version.kind = VersionKind::Removed;
    version.left = Some(Left {
        holder: key.resource.clone(),
        field: key.field.clone(),
        ty,
    });
    LeftRow {
        path: path.to_owned(),
        version,
    }
}

impl<'d> Derive<'d> {
    pub fn new(
        chunks: &'d Chunks,
        docs: &'d BTreeMap<DocId, (AppId, TypeId)>,
        nested: &'d Nested,
        clock: super::Clock,
    ) -> Self {
        Self {
            chunks,
            docs,
            nested,
            reads: DocReads::default(),
            done: BTreeSet::new(),
            columns: Columns::default(),
            clock,
            moved: BTreeMap::new(),
            wholes: BTreeSet::new(),
            chunks_of: BTreeMap::new(),
            notified: BTreeMap::new(),
            left: Vec::new(),
            part: None,
            found: BTreeMap::new(),
            looked: Vec::new(),
            opening: false,
        }
    }

    /// [`Self::new`] for a step that derives the chunks `part`, of which it derives the chunks
    /// `whole` from their first change: the items of those are read once, where they are, and
    /// each item a change of them names is looked up rather than searched for in every chunk the
    /// store holds.
    ///
    /// A chunk the step takes a few changes of is not read whole: an edit of one task names one
    /// item, which is searched for, and reading every item of its chunk to find it would cost
    /// each edit what the chunk holds.
    pub fn step(
        chunks: &'d Chunks,
        docs: &'d BTreeMap<DocId, (AppId, TypeId)>,
        nested: &'d Nested,
        clock: super::Clock,
        (part, whole): (&'d [ChunkNo], &[ChunkNo]),
    ) -> Self {
        let mut derive = Self::new(chunks, docs, nested, clock);
        derive.read_part(whole);
        derive.looked = part
            .iter()
            .copied()
            .filter(|chunk| !whole.contains(chunk))
            .collect();
        derive.part = Some(part);
        derive
    }

    /// [`Self::step`] for the first step of a build: a record derives only the items of the
    /// chunks it reads, which is what a window's first rows are made of, the rest of the record
    /// derived as the chunks holding it are read.
    pub fn opening(
        chunks: &'d Chunks,
        docs: &'d BTreeMap<DocId, (AppId, TypeId)>,
        nested: &'d Nested,
        clock: super::Clock,
        part: &'d [ChunkNo],
    ) -> Self {
        let mut derive = Self::step(chunks, docs, nested, clock, (part, part));
        derive.opening = true;
        derive
    }

    /// Read where the items of the chunks `part` hold them, by record and id.
    fn read_part(&mut self, part: &[ChunkNo]) {
        for chunk in part {
            let Some(doc) = self.chunks.mirror(*chunk) else {
                continue;
            };
            // The records this chunk holds, never every record of the store: a chunk holds the
            // items of a few of them.
            for record in crate::reads::held_ids(doc) {
                let Some((app, ty)) = self.docs.get(&record) else {
                    continue;
                };
                let (app, ty) = (app.clone(), ty.clone());
                let Some(schema) = lookup(&ty) else {
                    continue;
                };
                let Some(root) = crate::reads::resource_obj(doc, &app, &record) else {
                    continue;
                };
                let found = self.found.entry(record).or_default();
                for (id, item) in Items::scan(doc, &root, schema, *chunk).found {
                    let was = found.remove(&id);
                    found.insert(id, crate::items::later(was, item));
                }
            }
        }
    }

    /// The item `id` of the record `doc` (of `app`, of type `schema`): where the chunks the step
    /// reads hold it, and the index reads it from there, else searched for across the chunks the
    /// store holds, and kept for the items the step names again.
    fn item(
        &mut self,
        sink: &mut impl Sink,
        (app, doc): (&str, &str),
        schema: &TypeSchema,
        id: &str,
    ) -> Result<Option<Found>, IndexError> {
        let derived: Option<ChunkNo> = sink
            .text(&item_path(doc, id), CHUNK)?
            .map(|held| held.parse::<ChunkNo>().map_err(|_| IndexError::Corrupt))
            .transpose()?;
        let held = self.found.get(doc).and_then(|found| found.get(id));
        if let Some(found) = held
            && derived.is_none_or(|derived| derived == found.chunk)
        {
            let mut found = found.clone();
            for chunk in &self.looked {
                let Some(held) = self.chunks.mirror(*chunk) else {
                    continue;
                };
                let copy = crate::items::find_in((*chunk, held), app, doc, schema, self.nested, id);
                if let Some(copy) = copy {
                    found = crate::items::later(Some(found), copy);
                }
            }
            return Ok(Some(found));
        }
        // An item the store holds two copies of (a move made on two replicas at once) is read
        // from the copy that is later in the record's order, wherever it stands.
        let found = crate::items::find_across(self.chunks, app, doc, schema, self.nested, id);
        if let Some(found) = &found {
            self.found
                .entry(doc.to_owned())
                .or_default()
                .insert(id.to_owned(), found.clone());
        }
        Ok(found)
    }

    /// The document the resource `id` is stored in, when the store holds its chunk.
    fn source(&self, id: &str) -> Option<&'d Doc> {
        self.chunks.doc(id)
    }

    /// The chunk the map at `path` is stored in: the item's own, or the record's.
    fn chunk_of(&self, path: &str) -> ChunkNo {
        self.chunks_of
            .get(path)
            .copied()
            .unwrap_or_else(|| self.chunks.chunk(doc_of(path)))
    }

    /// The document the map at `path` is stored in, when the store holds its chunk.
    fn source_of(&self, path: &str) -> Option<&'d Doc> {
        self.chunks.mirror(self.chunk_of(path))
    }

    /// Derive every record of `app` into `sink`, which holds none of them.
    pub fn all(&mut self, sink: &mut impl Sink, app: &str) -> Result<(), IndexError> {
        let records: Vec<(String, TypeId)> = self
            .docs
            .iter()
            .filter(|(_, (a, _))| a == app)
            .map(|(doc, (_, ty))| (doc.clone(), ty.clone()))
            .collect();
        self.columns.bulk = Some(BTreeMap::new());
        for (doc, ty) in records {
            self.record(sink, app, &doc, &ty)?;
        }
        for (list, mut entries) in self.columns.bulk.take().unwrap_or_default() {
            entries.sort();
            entries.dedup();
            sink.replace(&list, entries)?;
        }
        Ok(())
    }

    /// Derive every record of `app` into `sink`, which holds none of them, with the versions
    /// `ends` holds of its resources: the first and last of the record's whole log and the
    /// last change of each field ([`reflect::ends_of`]), and the items `removed` holds that
    /// left their collections ([`super::REMOVED`]).
    pub fn all_with_versions(
        &mut self,
        sink: &mut impl Sink,
        app: &str,
        ends: &BTreeMap<String, Ends>,
        removed: &Removed,
    ) -> Result<(), IndexError> {
        // Written first, so the columns of each resource are derived with its versions; those
        // of resources the record no longer holds go again once the records are derived.
        let of_app: Vec<(&String, &Ends)> = ends
            .iter()
            .filter(|(resource, _)| self.of_app(resource, app))
            .collect();
        for (resource, end) in &of_app {
            write_ends(sink, resource, end)?;
        }
        self.all(sink, app)?;
        // What a mark reads of a resource without its log, once the records are derived: of a
        // resource the record no longer holds, the version that removed it.
        for (resource, end) in &of_app {
            if sink.text(resource, TYPE)?.is_none() {
                sink.remove(resource)?;
            }
            sink.put_newest(resource, &end.newest())?;
        }
        for ((holder, field), left) in removed {
            if left.is_empty() || !self.of_app(holder, app) {
                continue;
            }
            let mut rows = Vec::new();
            for row in left {
                // An item in a collection again is no row of what left one: a later move it
                // is not, when the collection it moved into is gone too.
                if sink.text(&row.path, IN)?.is_some() {
                    sink.drop_text(&row.path, LEFT)?;
                    continue;
                }
                rows.push(row.clone());
            }
            write_removed(sink, holder, field, &rows)?;
            for row in &rows {
                sink.put_text(&row.path, LEFT, &in_of(holder, field))?;
            }
        }
        Ok(())
    }

    /// Keep the items that left each collection the changes moved items of, with the version
    /// that removed each, and drop those that came back ([`super::REMOVED`]): what a list shows
    /// struck until the person has seen them go. Called after [`Self::changes`]: what a row is
    /// is where the item is now.
    pub fn removed(
        &mut self,
        sink: &mut impl Sink,
        app: &str,
        ends: &BTreeMap<String, Ends>,
    ) -> Result<(), IndexError> {
        // Where each item the changes removed left from, read before they were derived: what
        // the record held of its place is gone with them.
        for (path, key, stamp, ty) in std::mem::take(&mut self.left) {
            // An item in a collection again is no row of what left one, whatever the change
            // that returned it was derived with.
            if sink.text(&path, IN)?.is_some() {
                returned(sink, &path)?;
                drop_left(sink, &key, &path)?;
                continue;
            }
            note_left(sink, &key, &path, &stamp, ty.as_deref())?;
        }
        // What came back is no row of what left one, whether or not the changes derived it.
        let back: Vec<String> = ends
            .keys()
            .filter(|path| self.of_app(path, app))
            .filter(|path| matches!(sink.text(path, IN), Ok(Some(_))))
            .cloned()
            .collect();
        for path in &back {
            returned(sink, path)?;
        }
        let moved = std::mem::take(&mut self.moved);
        let written = self.removed_of(sink, app, ends, &moved);
        self.moved = moved;
        written
    }

    /// Take in what `ends` holds as removed of each collection, read before the changes that
    /// removed it are derived: where an item was is in the record the changes take away, and a
    /// record derived anew holds no item it lost. [`Self::removed`] writes the rows afterwards,
    /// when each item's place is read again.
    pub fn removed_before(
        &mut self,
        sink: &mut impl Sink,
        app: &str,
        ends: &BTreeMap<String, Ends>,
        placed: &reflect::Placed,
    ) -> Result<(), IndexError> {
        for (path, end) in ends {
            let Some((stamp, VersionKind::Removed)) = &end.moved else {
                continue;
            };
            if !self.of_app(path, app) {
                continue;
            }
            let anywhere = |held: Option<String>| {
                held.as_deref()
                    .and_then(split_in)
                    .map(|(holder, field)| (holder.to_owned(), field.to_owned()))
            };
            // Where it is, or where it left from when a removal already took it out of the
            // index.
            let mut held = anywhere(sink.text(path, IN)?);
            if held.is_none() {
                held = anywhere(sink.text(path, LEFT)?);
            }
            // An item placed and removed in the same batch was never in the index: the change
            // that placed it says where it left from.
            let Some((holder, field)) = held.or_else(|| {
                placed
                    .get(path)
                    .map(|left| (left.holder.clone(), left.field.clone()))
            }) else {
                continue;
            };
            let key = FieldKey {
                resource: holder,
                field,
            };
            let ty = collection_ty(sink, &key)?;
            self.left.push((path.clone(), key, stamp.clone(), ty));
        }
        Ok(())
    }

    /// [`Self::removed`], for the collections `moved` names: what a local write's plan placed
    /// and took out, derived into the overlay before the record holds its change.
    pub fn removed_of(
        &mut self,
        sink: &mut impl Sink,
        app: &str,
        ends: &BTreeMap<String, Ends>,
        moved: &BTreeMap<FieldKey, BTreeSet<String>>,
    ) -> Result<(), IndexError> {
        for (key, items) in moved {
            if !self.of_app(&key.resource, app) {
                continue;
            }
            let within = in_of(&key.resource, &key.field);
            let doc = doc_of(&key.resource).to_owned();
            let ty = collection_ty(sink, key)?;
            for item in items {
                let path = item_path(&doc, item);
                let back = sink.text(&path, IN)?.as_deref() == Some(within.as_str());
                match ends.get(&path).and_then(|end| end.moved.as_ref()) {
                    // It left the collection, in a batch that removed it.
                    Some((stamp, VersionKind::Removed)) if !back => {
                        note_left(sink, key, &path, stamp, ty.as_deref())?;
                    }
                    // It is in the collection again, or alive elsewhere: it is no row of what
                    // left this one.
                    Some(_) => drop_left(sink, key, &path)?,
                    // The batch did not move it: what the index holds of it stands.
                    None => {}
                }
            }
        }
        Ok(())
    }

    /// Whether the resource at `path` is in a record of `app`.
    fn of_app(&self, path: &str, app: &str) -> bool {
        self.docs.get(doc_of(path)).is_some_and(|(a, _)| a == app)
    }
    /// Take in the versions `ends` holds (the first and last versions of the resources new
    /// changes changed, [`reflect::ends_of`]) of the resources of `app` the sink holds, after
    /// the changes themselves were derived: each resource's first version stays unless one
    /// came before it, its last is replaced by a later one, and the columns of the collection
    /// it is in follow.
    pub fn versions(
        &mut self,
        sink: &mut impl Sink,
        app: &str,
        ends: &BTreeMap<String, Ends>,
    ) -> Result<(), IndexError> {
        for (resource, end) in ends {
            if !self.of_app(resource, app) {
                continue;
            }
            // The newest version of it, whatever the record holds of it now: a mark of an item
            // the record no longer has reads the version that took it out of its collection.
            sink.put_newest(resource, &end.newest())?;
            let Some(schema) = sink.text(resource, TYPE)?.and_then(|ty| lookup(&ty)) else {
                continue;
            };
            let held = |sink: &mut _, field: &str| -> Result<Option<Stamp>, IndexError> {
                Ok(Sink::field(sink, resource, field)?
                    .as_ref()
                    .and_then(Stamp::of_value))
            };
            // A step took a change to it back: its versions are its whole log read anew, so
            // they replace what the index holds, and each field it was made with says so.
            if end.restored {
                let mut end = end.clone();
                let made = end.first.clone();
                for field in &schema.fields {
                    end.fields
                        .entry(field.id.clone())
                        .or_insert_with(|| made.clone());
                }
                write_ends(sink, resource, &end)?;
                self.links(sink, resource, schema)?;
                if sink.keeps_columns() {
                    let placed = placed_in(sink, resource)?;
                    let scope = placed
                        .as_ref()
                        .map(|(holder, coll, key)| (holder.as_str(), coll.as_str(), key.as_str()));
                    let mut moved = vec![CREATED.to_owned(), UPDATED.to_owned()];
                    moved.extend(
                        schema
                            .fields
                            .iter()
                            .map(|field| reflect::changed_field(&field.id)),
                    );
                    for field in &moved {
                        self.index_columns(sink, resource, schema, scope, Some(field))?;
                    }
                }
                continue;
            }
            let mut moved: Vec<String> = Vec::new();
            if held(sink, CREATED)?.is_none_or(|held| end.first.before(&held)) {
                sink.put_field(resource, CREATED, &end.first.value())?;
                moved.push(CREATED.to_owned());
            }
            if held(sink, UPDATED)?.is_none_or(|held| held.before(&end.last)) {
                sink.put_field(resource, UPDATED, &end.last.value())?;
                moved.push(UPDATED.to_owned());
            }
            // When each field last changed, which a list groups or sorts by.
            for (field, stamp) in &end.fields {
                let id = reflect::changed_field(field);
                if held(sink, &id)?.is_none_or(|held| held.before(stamp)) {
                    sink.put_field(resource, &id, &stamp.value())?;
                    moved.push(id);
                }
            }
            if moved.is_empty() {
                continue;
            }
            // A resource made from another links to it from its first version on.
            if moved.iter().any(|field| field == CREATED) {
                self.links(sink, resource, schema)?;
            }
            if !sink.keeps_columns() {
                continue;
            }
            let placed = placed_in(sink, resource)?;
            let scope = placed
                .as_ref()
                .map(|(holder, coll, key)| (holder.as_str(), coll.as_str(), key.as_str()));
            for field in &moved {
                self.index_columns(sink, resource, schema, scope, Some(field))?;
            }
        }
        Ok(())
    }

    /// Derive the record `doc` of `app` anew, with every item placed in it.
    fn record(
        &mut self,
        sink: &mut impl Sink,
        app: &str,
        doc: &str,
        ty: &str,
    ) -> Result<(), IndexError> {
        // A record of a chunk the store was not sent is derived when that chunk arrives.
        let (Some(source), chunk) = (self.chunks.doc(doc), self.chunks.chunk(doc)) else {
            return Ok(());
        };
        let schema = lookup(ty);
        let held = match schema {
            Some(schema) => kept_removed(sink, doc, schema)?,
            None => Vec::new(),
        };
        sink.clear(doc)?;
        let obj = crate::reads::resource_obj(source, app, doc);
        self.reads.located = self.reads.located.saturating_add(1);
        match (obj, schema) {
            (Some(obj), Some(schema)) => {
                sink.doc(doc, Some(ty), chunk)?;
                // A collection's items are spread over the chunks of its runs: all of them, or
                // — the first step of a build — the ones this step reads, where the window's
                // first rows are, the rest of the items derived as their chunks are read.
                let items = Items::across_in(
                    self.chunks,
                    app,
                    doc,
                    schema,
                    self.opening.then_some(self.part).flatten(),
                );
                self.reads.listings = self.reads.listings.saturating_add(1);
                self.reads.entries = self
                    .reads
                    .entries
                    .saturating_add(u64::try_from(items.found.len()).unwrap_or(u64::MAX));
                let resolved = items.resolve(schema);
                if !resolved.held_back.is_empty() {
                    sink.put_text(doc, CYCLES, "1")?;
                }
                let whole = Whole {
                    doc,
                    items: &items,
                    resolved: &resolved,
                };
                self.tree(sink, doc, Some(&obj), schema, (&whole, None))?;
                sink.prune(doc)?;
            }
            _ => sink.doc(doc, None, chunk)?,
        }
        restore_removed(sink, doc, &held)?;
        self.done.insert(doc.to_owned());
        Ok(())
    }

    /// Derive the resource at `path` (its map `obj`, of the type `schema`; the item `holder`
    /// of the record, or the record for `None`) and every item placed in it; returns the values
    /// of its columns, for the lists of the collection it is in.
    fn tree(
        &mut self,
        sink: &mut impl Sink,
        path: &str,
        obj: Option<&ObjId>,
        schema: &'static TypeSchema,
        (whole, holder): (&Whole<'_>, Option<&str>),
    ) -> Result<Vec<(String, Vec<String>)>, IndexError> {
        let schema = self.own_fields(sink, path, obj, schema)?;
        for field in schema
            .fields
            .iter()
            .filter(|f| f.kind == MergeKind::Collection)
        {
            self.collection(sink, path, field, (whole, holder))?;
        }
        self.index_columns(sink, path, schema, None, None)
    }

    /// Derive the type and every field but a collection of the resource at `path` (its map
    /// `obj`, of the type `schema` unless the map holds another): returns its schema.
    fn own_fields(
        &mut self,
        sink: &mut impl Sink,
        path: &str,
        obj: Option<&ObjId>,
        schema: &'static TypeSchema,
    ) -> Result<&'static TypeSchema, IndexError> {
        let source = self.source_of(path);
        let chunk = self.chunk_of(path);
        let ty = obj
            .and_then(|obj| source.and_then(|source| type_at(source, obj)))
            .unwrap_or_else(|| schema.id.clone());
        sink.put_text(path, TYPE, &ty)?;
        // Where a read or a write of the resource goes: the chunk that holds its map.
        sink.put_text(path, CHUNK, &chunk.to_string())?;
        // An item of a collection of several types is its stored type's.
        let schema = lookup(&ty).unwrap_or(schema);
        for field in &schema.fields {
            if matches!(field.kind, MergeKind::Collection | MergeKind::Record) {
                continue;
            }
            let value = self.value(source, obj, field);
            sink.put_field(path, &field.id, &value)?;
        }
        // What it links to, once its fields are as they are now.
        self.links(sink, path, schema)?;
        Ok(schema)
    }

    /// Bring the columns of the resource at `path` (of the type `schema`) in line with its
    /// fields: those reading `changed`, or all of them. Its entries across the workspace move
    /// with its values; so do those of the collection it is in, at the order key `scope` gives.
    /// Returns the columns' values.
    fn index_columns(
        &mut self,
        sink: &mut impl Sink,
        path: &str,
        schema: &TypeSchema,
        scope: Option<Scope<'_>>,
        changed: Option<&str>,
    ) -> Result<Vec<(String, Vec<String>)>, IndexError> {
        let mut out = Vec::new();
        if !sink.keeps_columns() {
            return Ok(out);
        }
        for column in &schema.columns {
            if changed.is_some_and(|field| !columns::reads(column, field)) {
                continue;
            }
            let now = columns::values(column, schema, self.clock, &mut |field| {
                field_value(sink, path, field)
            })?;
            self.columns
                .write(sink, path, schema, column, scope, &now)?;
            // The index says what it wrote the column at, so that it writes again only what the
            // clock passes later ([`super::clock`]). The first item written fixes it: every
            // item written after it holds of the filter at its own instant, which is past this
            // one, and what the clock passes between them is written again either way.
            if column
                .pred
                .as_ref()
                .is_some_and(tilolang::pred::Pred::reads_clock)
            {
                sink.set_written_at(&schema.id, self.clock)?;
            }
            out.push((column.name.clone(), now));
        }
        Ok(out)
    }

    /// Take the item at `child` (order key `key`) out of the lists of the columns of the
    /// collection `field` of `resource` (`into` false), or put it in them, as its columns hold.
    fn scope_columns(
        sink: &mut impl Sink,
        (resource, field): (&str, &str),
        child: &str,
        item_schema: &TypeSchema,
        (key, into): (&str, bool),
    ) -> Result<(), IndexError> {
        if !sink.keeps_columns() {
            return Ok(());
        }
        let Some(id) = id_of(child) else {
            return Ok(());
        };
        for column in &item_schema.columns {
            let Some(held) = sink.text(child, &column_field(&column.name))? else {
                continue;
            };
            let list = scope_list(resource, field, &column.name);
            let counted = columns::counted(column);
            for value in split_values(&held) {
                let entry = scope_entry(&value, key, id);
                if into {
                    sink.insert(&list, entry)?;
                } else {
                    sink.remove_entry(&list, &entry)?;
                }
                if counted {
                    sink.count(&list, &value, if into { 1 } else { -1 })?;
                }
            }
        }
        Ok(())
    }

    /// The collection `field` of `resource` gained or lost items: the columns of `resource`
    /// that read how many it holds.
    fn holder_columns(
        &mut self,
        sink: &mut impl Sink,
        resource: &str,
        field: &str,
    ) -> Result<(), IndexError> {
        let Some(schema) = sink.text(resource, TYPE)?.and_then(|ty| lookup(&ty)) else {
            return Ok(());
        };
        if !schema
            .columns
            .iter()
            .any(|c| c.pred.is_some() && columns::reads(c, field))
        {
            return Ok(());
        }
        let placed = placed_in(sink, resource)?;
        let scope = placed
            .as_ref()
            .map(|(holder, coll, key)| (holder.as_str(), coll.as_str(), key.as_str()));
        self.index_columns(sink, resource, schema, scope, Some(field))?;
        Ok(())
    }

    /// The value of `field` in the resource map `obj`, as the store reads it; its default
    /// when the map holds none, or one of the wrong shape.
    fn value(
        &mut self,
        source: Option<&'d Doc>,
        obj: Option<&ObjId>,
        field: &FieldSchema,
    ) -> FieldValue {
        let (Some(source), Some(obj)) = (source, obj) else {
            return default_value(field);
        };
        self.reads.fields = self.reads.fields.saturating_add(1);
        let slot = source
            .get(obj, field.id.as_str())
            .ok()
            .flatten()
            .map(|(value, id)| (value.into_owned(), id));
        field_of_slot(source, slot, field).unwrap_or_else(|_| default_value(field))
    }

    /// Derive the collection `field` of the resource at `path` (the item `holder`, or the
    /// record) whole: its list, its items and their columns.
    fn collection(
        &mut self,
        sink: &mut impl Sink,
        path: &str,
        field: &FieldSchema,
        (whole, holder): (&Whole<'_>, Option<&str>),
    ) -> Result<(), IndexError> {
        let placed: Vec<(String, String)> = whole
            .resolved
            .children
            .get(&(holder.map(str::to_owned), field.id.clone()))
            .cloned()
            .unwrap_or_default();
        let mut members = Vec::with_capacity(placed.len());
        let mut lists: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
        let mut counts: BTreeMap<(String, String), i64> = BTreeMap::new();
        if sink.keeps_columns() {
            sink.drop_lists(&scope_prefix(path, &field.id))?;
        }
        for (key, id) in placed {
            members.push((key.clone(), id.clone()));
            let child = item_path(whole.doc, &id);
            sink.put_text(&child, KEY, &key)?;
            sink.put_text(&child, IN, &in_of(path, &field.id))?;
            returned(sink, &child)?;
            let Some(found) = whole.items.found.get(&id) else {
                continue;
            };
            let Some(item_schema) = found.ty.as_deref().and_then(lookup) else {
                continue;
            };
            self.chunks_of.insert(child.clone(), found.chunk);
            let values = self.tree(
                sink,
                &child,
                found.obj.as_ref(),
                item_schema,
                (whole, Some(&id)),
            )?;
            for (column, values) in values {
                let schema = item_schema.column(&column);
                let counted = schema.is_some_and(columns::counted);
                let name = scope_list(path, &field.id, &column);
                for value in values {
                    if counted {
                        let n = counts.entry((name.clone(), value.clone())).or_default();
                        *n = n.saturating_add(1);
                    }
                    lists
                        .entry(name.clone())
                        .or_default()
                        .push(scope_entry(&value, &key, &id));
                }
            }
        }
        sink.replace(&member_list(path, &field.id), members)?;
        for (list, mut entries) in lists {
            entries.sort();
            sink.replace(&list, entries)?;
        }
        for ((list, value), n) in counts {
            sink.count(&list, &value, n)?;
        }
        Ok(())
    }

    /// The map of the record `doc` and its type.
    fn record_of(&mut self, doc: &str) -> Option<(ObjId, &'static TypeSchema)> {
        let (app, ty) = self.docs.get(doc)?;
        self.reads.located = self.reads.located.saturating_add(1);
        let obj = crate::reads::resource_obj(self.source(doc)?, app, doc)?;
        Some((obj, lookup(ty)?))
    }

    /// Where the resource at `path` is in the record, and its type: `None` when it is not
    /// there. The chunk holding the item's map is kept with it: it is where its fields are read
    /// and its writes go, and an item of a run is in a chunk above the record's.
    fn locate(
        &mut self,
        sink: &mut impl Sink,
        path: &str,
    ) -> Result<Option<(Option<ObjId>, &'static TypeSchema)>, IndexError> {
        let doc = doc_of(path);
        let Some((root, schema)) = self.record_of(doc) else {
            return Ok(None);
        };
        let Some(item) = id_of(path) else {
            return Ok(Some((Some(root), schema)));
        };
        let Some((app, _)) = self.docs.get(doc) else {
            return Ok(None);
        };
        let app = app.clone();
        let Some(found) = self.item(sink, (&app, doc), schema, item)? else {
            return Ok(None);
        };
        self.chunks_of.insert(path.to_owned(), found.chunk);
        let Some(item_schema) = found.ty.as_deref().and_then(lookup) else {
            return Ok(None);
        };
        Ok(Some((found.obj, item_schema)))
    }

    /// Apply what changes to the record reached, of the records of `app`, to `sink`, which
    /// held the record as it was before them.
    pub fn changes(
        &mut self,
        sink: &mut impl Sink,
        app: &str,
        reached: &Reached,
    ) -> Result<(), IndexError> {
        let of_app = |doc: &str, docs: &BTreeMap<DocId, (AppId, TypeId)>| {
            docs.get(doc).is_some_and(|(a, _)| a == app)
        };
        // Records as a whole, first: what is inside them is derived with them.
        //
        // In a step of a build or a catch-up ([`Self::step`]), a record whose own map the changes
        // named whole (any field of it) is derived whole here too, rather than after its items'
        // placements: each item the step places would otherwise be derived once where it
        // arrives and again with the record, which for the rest of a build is every item of the
        // workspace twice. A step reports no item that moved (the store takes it in as a change
        // to everything, [`Self::moved`] unread); a change derived on the frame does, for the
        // lists it keeps, and places its items first.
        for (doc, fields) in &reached.docs {
            let named_whole = self.part.is_some()
                && fields
                    .iter()
                    .any(|key| key.resource == *doc && key.field == ANY_FIELD);
            if !fields.is_empty() && !named_whole {
                continue;
            }
            match self.docs.get(doc) {
                Some((a, ty)) if a == app => {
                    let ty = ty.clone();
                    self.record(sink, app, doc, &ty)?;
                }
                // Gone from the record (or never this package's): what the index held of it
                // goes.
                None if fields.is_empty() && sink.text(doc, TYPE)?.is_some() => {
                    sink.remove(doc)?;
                    sink.doc(doc, None, self.chunks.chunk(doc))?;
                    self.done.insert(doc.clone());
                }
                _ => {}
            }
        }
        for (doc, items) in &reached.placed {
            if !of_app(doc, self.docs) || self.done.contains(doc) {
                continue;
            }
            self.placements(sink, app, doc, items)?;
        }
        for (doc, fields) in &reached.docs {
            if !of_app(doc, self.docs) && sink.text(doc, TYPE)?.is_none() {
                continue;
            }
            for key in fields {
                if self.within_done(&key.resource) {
                    continue;
                }
                if key.field == ANY_FIELD {
                    self.resource(sink, &key.resource)?;
                } else {
                    self.field(sink, &key.resource, &key.field)?;
                }
            }
        }
        Ok(())
    }

    fn within_done(&self, path: &str) -> bool {
        self.done.contains(path) || self.done.contains(doc_of(path))
    }

    /// Derive the record `doc` anew whole, as a change to it could not be taken in item by
    /// item.
    fn whole(&mut self, sink: &mut impl Sink, app: &str, doc: &str) -> Result<(), IndexError> {
        let Some((_, ty)) = self.docs.get(doc).cloned() else {
            return Ok(());
        };
        // An item back in a collection was dropped with its versions when it left: they are
        // read again from the whole log, as a derivation anew reads them.
        if sink.keeps_columns()
            && let Some(source) = self.source(doc)
        {
            let ends = reflect::ends_of(
                &source.committed_document().get_changes(&[]),
                source,
                self.docs,
            );
            for (resource, end) in &ends {
                if doc_of(resource) == doc {
                    write_ends(sink, resource, end)?;
                }
            }
        }
        self.record(sink, app, doc, &ty)?;
        self.wholes.insert(doc.to_owned());
        Ok(())
    }

    /// The placements of the items `items` of the record `doc` were written (each `true` when
    /// the changes made it): move each to the collection it is in now, out of the one it was
    /// in, and take out those in none, with everything in them.
    fn placements(
        &mut self,
        sink: &mut impl Sink,
        app: &str,
        doc: &str,
        items: &BTreeMap<String, bool>,
    ) -> Result<(), IndexError> {
        // A record where a move closed a cycle is derived whole, as which move holds back
        // depends on the others.
        if sink.text(doc, CYCLES)?.is_some() {
            return self.whole(sink, app, doc);
        }
        let Some((_, schema)) = self.record_of(doc) else {
            return Ok(());
        };
        let Some((held_app, _)) = self.docs.get(doc).cloned() else {
            return Ok(());
        };
        let mut pending: Vec<(String, bool)> =
            items.iter().map(|(id, made)| (id.clone(), *made)).collect();
        while !pending.is_empty() {
            let waiting: BTreeSet<String> = pending.iter().map(|(id, _)| id.clone()).collect();
            let mut deferred = Vec::new();
            let mut progressed = false;
            for (id, made) in std::mem::take(&mut pending) {
                let path = item_path(doc, &id);
                if self.done.contains(&path) {
                    progressed = true;
                    continue;
                }
                self.reads.located = self.reads.located.saturating_add(1);
                let found = self.item(sink, (&held_app, doc), schema, &id)?;
                let item_schema = found
                    .as_ref()
                    .and_then(|found| found.ty.as_deref())
                    .and_then(lookup);
                let target = found
                    .as_ref()
                    .and_then(|found| found.held.as_ref())
                    .and_then(|held| held.placement.place())
                    .cloned();
                let target = match (target, item_schema) {
                    (Some(place), Some(item_schema)) => {
                        // The item holding it, when it is placed in another item's collection.
                        if let Some(parent) = &place.parent {
                            if parent != &id && waiting.contains(parent) {
                                deferred.push((id, made));
                                continue;
                            }
                            if Self::holds(sink, doc, parent, &id)? == Holds::Cycle {
                                return self.whole(sink, app, doc);
                            }
                        }
                        Self::fitting(sink, doc, place, item_schema)?
                    }
                    _ => None,
                };
                progressed = true;
                // The chunk that holds its map now: where its fields are read and written.
                let chunk = found
                    .as_ref()
                    .map_or_else(|| self.chunk_of(&path), |found| found.chunk);
                self.chunks_of.insert(path.clone(), chunk);
                let before = placed_in(sink, &path)?;
                match (before, target) {
                    (None, None) => {}
                    (Some(before), None) => {
                        self.leave(sink, &path, &before)?;
                        self.remove_tree(sink, &path)?;
                        // It went with the item that held it, as a card goes with its column:
                        // `leave` keeps the row of what left for the item holding it, and
                        // where it left from is not kept for an item the record no longer
                        // holds. A derivation of the record anew writes it the same way, and
                        // the index reads the same however it was derived.
                    }
                    (None, Some(place)) => {
                        // An item that was in no collection, and that the changes did not
                        // make, may hold items of its own: its whole record is derived again.
                        if !made {
                            return self.whole(sink, app, doc);
                        }
                        let (Some(found), Some(item_schema)) = (found, item_schema) else {
                            continue;
                        };
                        self.made_into(sink, &path, &found, item_schema, &place, chunk)?;
                    }
                    (Some(before), Some(place)) => {
                        let holder = place_holder(doc, &place);
                        if before == (holder, place.field.clone(), place.key.clone()) {
                            continue;
                        }
                        let Some(own) = sink.text(&path, TYPE)?.and_then(|ty| lookup(&ty)) else {
                            continue;
                        };
                        self.leave(sink, &path, &before)?;
                        self.arrive(sink, (&path, own), &place, chunk)?;
                    }
                }
            }
            if !progressed {
                // Items waiting on each other: placed inside each other.
                return self.whole(sink, app, doc);
            }
            pending = deferred;
        }
        Ok(())
    }

    /// The row of an item the changes made, as it arrives in the collection `place` names: its
    /// own fields derived where it now stands, and what left its collections kept.
    fn made_into(
        &mut self,
        sink: &mut impl Sink,
        path: &str,
        found: &crate::items::Found,
        item_schema: &'static TypeSchema,
        place: &Place,
        chunk: ChunkNo,
    ) -> Result<(), IndexError> {
        let held = kept_removed(sink, path, item_schema)?;
        sink.clear(path)?;
        let own = self.own_fields(sink, path, found.obj.as_ref(), item_schema)?;
        for field in own
            .fields
            .iter()
            .filter(|field| field.kind == MergeKind::Collection)
        {
            sink.replace(&member_list(path, &field.id), Vec::new())?;
        }
        self.arrive(sink, (path, own), place, chunk)?;
        restore_removed(sink, path, &held)?;
        self.done.insert(path.to_owned());
        Ok(())
    }

    /// Whether the item `parent` of `doc` holds items now (is in a collection) and is not the
    /// item `item` or inside it.
    fn holds(
        sink: &mut impl Sink,
        doc: &str,
        parent: &str,
        item: &str,
    ) -> Result<Holds, IndexError> {
        let mut at = item_path(doc, parent);
        let mine = item_path(doc, item);
        if sink.text(&at, TYPE)?.is_none() {
            return Ok(Holds::Not);
        }
        // Up from the holder: the item met on the way would hold itself.
        let mut steps = 0usize;
        loop {
            if at == mine {
                return Ok(Holds::Cycle);
            }
            let Some((holder, _, _)) = placed_in(sink, &at)? else {
                return Ok(Holds::Yes);
            };
            if holder == doc {
                return Ok(Holds::Yes);
            }
            at = holder;
            steps = steps.saturating_add(1);
            if steps > 100_000 {
                return Ok(Holds::Cycle);
            }
        }
    }

    /// The place `place` of an item of `item_schema` in the record `doc`, if its collection
    /// may hold the item and its holder is in a collection itself.
    fn fitting(
        sink: &mut impl Sink,
        doc: &str,
        place: Place,
        item_schema: &TypeSchema,
    ) -> Result<Option<Place>, IndexError> {
        let holder = place_holder(doc, &place);
        let Some(holder_schema) = sink.text(&holder, TYPE)?.and_then(|ty| lookup(&ty)) else {
            return Ok(None);
        };
        Ok(fits(Some(holder_schema), &place.field, &item_schema.id).then_some(place))
    }

    /// Take the item at `path` out of the collection it was in (`before`: holder, field, order
    /// key).
    fn leave(
        &mut self,
        sink: &mut impl Sink,
        path: &str,
        (holder, field, key): &(String, String, String),
    ) -> Result<(), IndexError> {
        let Some(id) = id_of(path) else {
            return Ok(());
        };
        sink.remove_entry(&member_list(holder, field), &(key.clone(), id.to_owned()))?;
        if let Some(own) = sink.text(path, TYPE)?.and_then(|ty| lookup(&ty)) {
            Self::scope_columns(sink, (holder, field), path, own, (key, false))?;
        }
        self.holder_columns(sink, holder, field)?;
        self.moved
            .entry(FieldKey {
                resource: holder.clone(),
                field: field.clone(),
            })
            .or_default()
            .insert(id.to_owned());
        Ok(())
    }

    /// Put the item at `path` (of `schema`, its fields derived) in the collection `place` names.
    fn arrive(
        &mut self,
        sink: &mut impl Sink,
        (path, schema): (&str, &'static TypeSchema),
        place: &Place,
        chunk: ChunkNo,
    ) -> Result<(), IndexError> {
        let Some(id) = id_of(path) else {
            return Ok(());
        };
        let holder = place_holder(doc_of(path), place);
        sink.put_text(path, KEY, &place.key)?;
        sink.put_text(path, IN, &in_of(&holder, &place.field))?;
        // An item moved into a run of another chunk is read and written there now.
        sink.put_text(path, CHUNK, &chunk.to_string())?;
        returned(sink, path)?;
        sink.insert(
            &member_list(&holder, &place.field),
            (place.key.clone(), id.to_owned()),
        )?;
        if sink.text(path, &first_column(schema))?.is_some() {
            Self::scope_columns(
                sink,
                (&holder, &place.field),
                path,
                schema,
                (&place.key, true),
            )?;
        } else {
            self.index_columns(
                sink,
                path,
                schema,
                Some((&holder, &place.field, &place.key)),
                None,
            )?;
        }
        self.holder_columns(sink, &holder, &place.field)?;
        self.moved
            .entry(FieldKey {
                resource: holder,
                field: place.field.clone(),
            })
            .or_default()
            .insert(id.to_owned());
        Ok(())
    }

    /// Drop the item at `path` and every item in it from `sink`.
    fn remove_tree(&mut self, sink: &mut impl Sink, path: &str) -> Result<(), IndexError> {
        let doc = doc_of(path).to_owned();
        let mut found = vec![path.to_owned()];
        let mut at = 0usize;
        while let Some(resource) = found.get(at).cloned() {
            at = at.saturating_add(1);
            let Some(schema) = sink.text(&resource, TYPE)?.and_then(|ty| lookup(&ty)) else {
                continue;
            };
            for field in schema
                .fields
                .iter()
                .filter(|f| f.kind == MergeKind::Collection)
            {
                for (_, id) in sink.entries(&member_list(&resource, &field.id))? {
                    found.push(item_path(&doc, &id));
                }
            }
        }
        for resource in found.into_iter().rev() {
            sink.remove(&resource)?;
            self.done.insert(resource);
        }
        Ok(())
    }

    /// The resource at `path` was put, replaced or removed as a whole: its own fields derived
    /// anew, where it is.
    fn resource(&mut self, sink: &mut impl Sink, path: &str) -> Result<(), IndexError> {
        if id_of(path).is_none() {
            // A record: derived whole.
            let doc = path.to_owned();
            if let Some((app, ty)) = self.docs.get(&doc).cloned() {
                self.record(sink, &app, &doc, &ty)?;
            }
            return Ok(());
        }
        // An item in no collection is in no list: nothing of it is held.
        let Some(placed) = placed_in(sink, path)? else {
            return Ok(());
        };
        let Some((obj, schema)) = self.locate(sink, path)? else {
            return Ok(());
        };
        let own = self.own_fields(sink, path, obj.as_ref(), schema)?;
        let (holder, field, key) = &placed;
        self.index_columns(sink, path, own, Some((holder, field, key)), None)?;
        self.done.insert(path.to_owned());
        Ok(())
    }

    /// Derive the links the resource at `path` (of the type `schema`) makes, into the
    /// [`linked_list`] of each resource it links to: the resource its first version says it was
    /// made from, the resources its reference fields name, those its rich text mentions
    /// ([`tilolang_runtime::store::Store::linked`]). The set it made before is kept under
    /// [`LINKS`], so a change takes out of the lists exactly what it no longer links to; a target
    /// whose links moved is told of it, so a view reading them runs again.
    fn links(
        &mut self,
        sink: &mut impl Sink,
        path: &str,
        schema: &TypeSchema,
    ) -> Result<(), IndexError> {
        let mut now: Vec<(LinkHow, String)> = Vec::new();
        if let Some(from) = sink
            .field(path, CREATED)?
            .as_ref()
            .and_then(Stamp::of_value)
            .and_then(|stamp| stamp.version.from)
        {
            now.push((LinkHow::CreatedFrom, from.id_string()));
        }
        for field in &schema.fields {
            let how = match field.kind {
                MergeKind::RichText => LinkHow::Mention,
                MergeKind::Register if field.refers => LinkHow::Field,
                _ => continue,
            };
            let Some(value) = sink.field(path, &field.id)? else {
                continue;
            };
            for target in linked_by(&value) {
                now.push((how, target));
            }
        }
        // Nothing links to itself: rich text mentioning what holds it, or a field naming it.
        now.retain(|(_, target)| !target.is_empty() && target != path);
        now.sort_by(|a, b| (a.0.rank(), &a.1).cmp(&(b.0.rank(), &b.1)));
        now.dedup();
        let now_flat: Vec<String> = now
            .iter()
            .flat_map(|(how, target)| [how.word().to_owned(), target.clone()])
            .collect();
        let held = sink.text(path, LINKS)?;
        if held.as_deref().map(split_values).as_deref() == Some(now_flat.as_slice()) {
            return Ok(());
        }
        let pairs = |flat: Vec<String>| -> BTreeSet<(String, String)> {
            flat.chunks(2)
                .filter_map(|pair| match pair {
                    [how, target] => Some((how.clone(), target.clone())),
                    _ => None,
                })
                .collect()
        };
        let before = pairs(held.map(|held| split_values(&held)).unwrap_or_default());
        let after: BTreeSet<(String, String)> = now
            .iter()
            .map(|(how, target)| (how.word().to_owned(), target.clone()))
            .collect();
        for (how, target) in before.difference(&after) {
            sink.remove_entry(&linked_list(target), &link_entry(how, path))?;
        }
        for (how, target) in after.difference(&before) {
            sink.insert(&linked_list(target), link_entry(how, path))?;
        }
        sink.put_text(path, LINKS, &join_values(&now_flat))?;
        let mut moved: BTreeSet<&str> = BTreeSet::new();
        for (_, target) in before.symmetric_difference(&after) {
            moved.insert(target);
        }
        for target in moved {
            self.notified
                .entry(doc_of(target).to_owned())
                .or_default()
                .push(FieldKey {
                    resource: target.to_owned(),
                    field: LINKS.to_owned(),
                });
        }
        Ok(())
    }

    /// The field `field` of the resource at `path` changed.
    fn field(&mut self, sink: &mut impl Sink, path: &str, field: &str) -> Result<(), IndexError> {
        // An item in no collection is in no list: nothing of it is held.
        if id_of(path).is_some() && sink.text(path, TYPE)?.is_none() {
            return Ok(());
        }
        let Some((obj, schema)) = self.locate(sink, path)? else {
            return Ok(());
        };
        let Some(spec) = schema.field(field) else {
            return Ok(());
        };
        if matches!(spec.kind, MergeKind::Record | MergeKind::Collection) {
            // A collection is its items' placements, which are derived as they are written.
            return Ok(());
        }
        let now = self.value(self.source_of(path), obj.as_ref(), spec);
        let before = sink.field(path, field)?;
        if before.as_ref() == Some(&now) {
            return Ok(());
        }
        sink.put_field(path, field, &now)?;
        // What it links to, when the field is one that links.
        if matches!(spec.kind, MergeKind::RichText)
            || (spec.kind == MergeKind::Register && spec.refers)
        {
            self.links(sink, path, schema)?;
        }
        // The columns reading the field, in the collection the resource is in and across the
        // workspace.
        if sink.keeps_columns() && schema.columns.iter().any(|c| columns::reads(c, field)) {
            let placed = placed_in(sink, path)?;
            let scope = placed
                .as_ref()
                .map(|(holder, coll, key)| (holder.as_str(), coll.as_str(), key.as_str()));
            self.index_columns(sink, path, schema, scope, Some(field))?;
        }
        Ok(())
    }
}

/// Whether an item may be placed in another's collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Holds {
    /// The holder is in a collection.
    Yes,
    /// It is in none.
    Not,
    /// It is the item, or inside it.
    Cycle,
}

/// The resource holding the collection `place` names, in the record `doc`.
fn place_holder(doc: &str, place: &Place) -> String {
    match &place.parent {
        Some(parent) => item_path(doc, parent),
        None => doc.to_owned(),
    }
}

/// The pseudo-field of the first column of `schema`: present once the item's columns were
/// derived.
fn first_column(schema: &TypeSchema) -> String {
    schema
        .columns
        .first()
        .map_or_else(|| TYPE.to_owned(), |column| column_field(&column.name))
}
