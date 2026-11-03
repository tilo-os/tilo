//! What the index holds, derived from the workspace record: a package's records whole, or what a
//! set of changes reached, field by field.
//!
//! The same derivation keeps the index on disk ([`super::Writer`]) and the store's open writes
//! ([`super::overlay::Overlay`]) as the record holds them: both are a [`Sink`].

use std::collections::{BTreeMap, BTreeSet};

use automerge::{ObjId, ReadDoc};
use tilolang_runtime::ids::{AppId, DocId, TypeId};
use tilolang_runtime::place::{Place, Resolved};
use tilolang_runtime::store::{ANY_FIELD, FieldKey, FieldValue, doc_of};

use super::columns::{
    self, reversed, scope_entry, scope_list, scope_prefix, type_entry, type_list,
};
use super::derived;
use super::lists::Entry;
use super::reflect::{self, CREATED, Stamp, UPDATED};
use super::{CYCLES, IN, IndexError, KEY, TYPE, Writer};
use crate::items::{Items, Nested, fits};
use crate::reads::{Placed, default_value, field_of_slot, type_at};
use crate::schema::{FieldSchema, MergeKind, TypeSchema, lookup};

type Doc = tilo_resource::Doc;

/// The list of the items of the collection `field` of the resource `resource`.
#[must_use]
pub fn member_list(resource: &str, field: &str) -> String {
    format!("m\u{1}{resource}\u{1}{field}")
}

/// The pseudo-field holding the values a resource's column `column` holds, as the column's
/// lists have them: what a change replaces there, and a removal takes out.
#[must_use]
pub fn column_field(column: &str) -> String {
    format!("$c\u{1}{column}")
}

/// The values held under a [`column_field`].
fn split_values(held: &str) -> Vec<String> {
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
///
/// # Errors
/// What it holds cannot be read.
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
    ///
    /// # Errors
    /// What it holds cannot be read.
    fn field(&mut self, resource: &str, field: &str) -> Result<Option<FieldValue>, IndexError>;

    /// The type (`$type`) or order key (`$key`) of `resource`.
    ///
    /// # Errors
    /// What it holds cannot be read.
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

    /// Drop `resource` and everything inside it: fields, lists, and what search finds them by.
    ///
    /// # Errors
    /// It cannot be written.
    fn remove(&mut self, resource: &str) -> Result<(), IndexError>;

    /// Drop `resource` and everything inside it to derive it anew: as [`Sink::remove`], but
    /// the versions kept of each ([`reflect`]) stay, as they are not in the record.
    ///
    /// # Errors
    /// It cannot be written.
    fn clear(&mut self, resource: &str) -> Result<(), IndexError> {
        self.remove(resource)
    }

    /// Drop what is held of the resources within the record `doc` that hold no type: the
    /// versions [`Sink::clear`] kept of items a derivation anew finds in no collection.
    ///
    /// # Errors
    /// It cannot be written.
    fn prune(&mut self, doc: &str) -> Result<(), IndexError> {
        let _ = doc;
        Ok(())
    }

    /// That the package's record `doc` is of the type `ty`, or (`None`) is gone.
    ///
    /// # Errors
    /// It cannot be written.
    fn doc(&mut self, doc: &str, ty: Option<&str>) -> Result<(), IndexError>;

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
    ///
    /// # Errors
    /// What it holds cannot be read.
    fn entries(&mut self, list: &str) -> Result<Vec<Entry>, IndexError>;

    /// How many entries `list` holds.
    ///
    /// # Errors
    /// What it holds cannot be read.
    fn len(&mut self, list: &str) -> Result<usize, IndexError>;

    /// Add `delta` to how many entries of the column list `list` hold `value`
    /// ([`columns::counted`]).
    ///
    /// # Errors
    /// It cannot be written.
    fn count(&mut self, list: &str, value: &str, delta: i64) -> Result<(), IndexError>;

    /// Drop every list whose name starts with `prefix`.
    ///
    /// # Errors
    /// It cannot be written.
    fn drop_lists(&mut self, prefix: &str) -> Result<(), IndexError>;

    /// Whether it keeps filter columns, or (the store's open writes) fields and membership
    /// alone.
    fn keeps_columns(&self) -> bool;
}

impl Sink for Writer<'_> {
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
            .filter(|(_, field, _)| reflect::is_reflected(field))
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
        let stale: Vec<(String, String)> = held
            .iter()
            .filter(|(resource, _, _)| !typed.contains(resource.as_str()))
            .map(|(resource, field, _)| (resource.clone(), field.clone()))
            .collect();
        for (resource, field) in stale {
            self.remove_field(&resource, &field)?;
        }
        Ok(())
    }

    fn doc(&mut self, doc: &str, ty: Option<&str>) -> Result<(), IndexError> {
        match ty {
            Some(ty) => self.put_doc(doc, ty),
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
    doc: &'d Doc,
    /// Every record of the workspace: its app and type.
    docs: &'d BTreeMap<DocId, (AppId, TypeId)>,
    /// Where the items of records written before items were flat are.
    nested: &'d Nested,
    pub reads: DocReads,
    /// Resources derived whole in this pass: a change naming them again is done.
    done: BTreeSet<String>,
    /// While a whole package is derived into an empty index: the entries of its types'
    /// columns across the workspace, each list written whole at the end rather than entry by
    /// entry.
    bulk: Option<BTreeMap<String, Vec<Entry>>>,
    /// The collections whose items it added, removed or moved, with those items: what a
    /// listing of each is brought in line by.
    pub moved: BTreeMap<FieldKey, BTreeSet<String>>,
    /// The records it derived anew whole while deriving changes, where a change could not be
    /// taken in item by item: everything read of them may have changed.
    pub wholes: BTreeSet<DocId>,
    /// The derived keys a change moved, per document: a value a view reads as a field of an
    /// item whose own field the change did not name, so whoever reads is told of it
    /// ([`crate::index::derived`]).
    pub derived_keys: BTreeMap<DocId, Vec<FieldKey>>,
    /// The items of the record being derived whole, with their types: what the derived keys
    /// are worked out over once it is.
    whole_items: Option<Vec<(String, &'static TypeSchema)>>,
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

impl<'d> Derive<'d> {
    pub fn new(
        doc: &'d Doc,
        docs: &'d BTreeMap<DocId, (AppId, TypeId)>,
        nested: &'d Nested,
    ) -> Self {
        Self {
            doc,
            docs,
            nested,
            reads: DocReads::default(),
            done: BTreeSet::new(),
            bulk: None,
            moved: BTreeMap::new(),
            wholes: BTreeSet::new(),
            derived_keys: BTreeMap::new(),
            whole_items: None,
        }
    }

    /// Derive every record of `app` into `sink`, which holds none of them.
    ///
    /// # Errors
    /// The sink cannot be written.
    pub fn all(&mut self, sink: &mut impl Sink, app: &str) -> Result<(), IndexError> {
        let records: Vec<(String, TypeId)> = self
            .docs
            .iter()
            .filter(|(_, (a, _))| a == app)
            .map(|(doc, (_, ty))| (doc.clone(), ty.clone()))
            .collect();
        self.bulk = Some(BTreeMap::new());
        for (doc, ty) in records {
            self.record(sink, app, &doc, &ty)?;
        }
        for (list, mut entries) in self.bulk.take().unwrap_or_default() {
            entries.sort();
            entries.dedup();
            sink.replace(&list, entries)?;
        }
        Ok(())
    }

    /// Derive every record of `app` into `sink`, which holds none of them, with the versions
    /// `ends` holds of its resources: the first and last of the record's whole log
    /// ([`reflect::ends_of`]).
    ///
    /// # Errors
    /// The sink cannot be written.
    pub fn all_with_versions(
        &mut self,
        sink: &mut impl Sink,
        app: &str,
        ends: &BTreeMap<String, (Stamp, Stamp)>,
    ) -> Result<(), IndexError> {
        // Written first, so the columns of each resource are derived with its versions; those
        // of resources the record no longer holds go again once the records are derived.
        let of_app: Vec<(&String, &(Stamp, Stamp))> = ends
            .iter()
            .filter(|(resource, _)| self.of_app(resource, app))
            .collect();
        for (resource, (first, last)) in &of_app {
            sink.put_field(resource, CREATED, &first.value())?;
            sink.put_field(resource, UPDATED, &last.value())?;
        }
        self.all(sink, app)?;
        for (resource, _) in of_app {
            if sink.text(resource, TYPE)?.is_none() {
                sink.remove(resource)?;
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
    ///
    /// # Errors
    /// The sink cannot be read or written.
    pub fn versions(
        &mut self,
        sink: &mut impl Sink,
        app: &str,
        ends: &BTreeMap<String, (Stamp, Stamp)>,
    ) -> Result<(), IndexError> {
        for (resource, (first, last)) in ends {
            if !self.of_app(resource, app) {
                continue;
            }
            let Some(schema) = sink.text(resource, TYPE)?.and_then(|ty| lookup(&ty)) else {
                continue;
            };
            let held = |sink: &mut _, field| -> Result<Option<Stamp>, IndexError> {
                Ok(Sink::field(sink, resource, field)?
                    .as_ref()
                    .and_then(Stamp::of_value))
            };
            let mut moved = Vec::new();
            if held(sink, CREATED)?.is_none_or(|held| first.before(&held)) {
                sink.put_field(resource, CREATED, &first.value())?;
                moved.push(CREATED);
            }
            if held(sink, UPDATED)?.is_none_or(|held| held.before(last)) {
                sink.put_field(resource, UPDATED, &last.value())?;
                moved.push(UPDATED);
            }
            if moved.is_empty() || !sink.keeps_columns() {
                continue;
            }
            let placed = placed_in(sink, resource)?;
            let scope = placed
                .as_ref()
                .map(|(holder, coll, key)| (holder.as_str(), coll.as_str(), key.as_str()));
            for field in moved {
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
        sink.clear(doc)?;
        let obj = crate::reads::resource_obj(self.doc, app, doc);
        self.reads.located = self.reads.located.saturating_add(1);
        match (obj, lookup(ty)) {
            (Some(obj), Some(schema)) => {
                sink.doc(doc, Some(ty))?;
                let items = Items::scan(self.doc, &obj, schema);
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
                self.whole_items = Some(Vec::new());
                self.tree(sink, doc, Some(&obj), schema, (&whole, None))?;
                sink.prune(doc)?;
                let visited = self.whole_items.take().unwrap_or_default();
                self.derive_whole(sink, &visited)?;
            }
            _ => sink.doc(doc, None)?,
        }
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
        if let Some(items) = self.whole_items.as_mut() {
            items.push((path.to_owned(), schema));
        }
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
        let ty = obj
            .and_then(|obj| type_at(self.doc, obj))
            .unwrap_or_else(|| schema.id.clone());
        sink.put_text(path, TYPE, &ty)?;
        // An item of a collection of several types is its stored type's.
        let schema = lookup(&ty).unwrap_or(schema);
        for field in &schema.fields {
            if matches!(field.kind, MergeKind::Collection | MergeKind::Record) {
                continue;
            }
            // A derived key is worked out from the collection, not read from the record.
            if field.derived.is_some() {
                continue;
            }
            let value = self.value(obj, field);
            sink.put_field(path, &field.id, &value)?;
        }
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
            let now = columns::values(column, schema, &mut |field| field_value(sink, path, field))?;
            let held = sink
                .text(path, &column_field(&column.name))?
                .map(|h| split_values(&h));
            if held.as_ref() != Some(&now) {
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
                sink.put_text(path, &column_field(&column.name), &join_values(&now))?;
            }
            out.push((column.name.clone(), now));
        }
        Ok(out)
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
    fn value(&mut self, obj: Option<&ObjId>, field: &FieldSchema) -> FieldValue {
        let Some(obj) = obj else {
            return default_value(field);
        };
        self.reads.fields = self.reads.fields.saturating_add(1);
        let slot = self
            .doc
            .get(obj, field.id.as_str())
            .ok()
            .flatten()
            .map(|(value, id)| (value.into_owned(), id));
        field_of_slot(self.doc, slot, field).unwrap_or_else(|_| default_value(field))
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
            let Some(found) = whole.items.found.get(&id) else {
                continue;
            };
            let Some(item_schema) = found.ty.as_deref().and_then(lookup) else {
                continue;
            };
            let values = self.tree(
                sink,
                &child,
                found.obj.as_ref(),
                item_schema,
                (whole, Some(&id)),
            )?;
            for (column, values) in values {
                let counted = item_schema.column(&column).is_some_and(columns::counted);
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
        let obj = crate::reads::resource_obj(self.doc, app, doc)?;
        Some((obj, lookup(ty)?))
    }

    /// Where the resource at `path` is in the record, and its type: `None` when it is not
    /// there.
    fn locate(&mut self, path: &str) -> Option<(Option<ObjId>, &'static TypeSchema)> {
        let doc = doc_of(path);
        let (root, schema) = self.record_of(doc)?;
        let Some(item) = id_of(path) else {
            return Some((Some(root), schema));
        };
        let found = crate::items::find(self.doc, (doc, &root, schema), self.nested, item)?;
        Some((found.obj, found.ty.as_deref().and_then(lookup)?))
    }

    /// Apply what changes to the record reached, of the records of `app`, to `sink`, which
    /// held the record as it was before them.
    ///
    /// # Errors
    /// The sink cannot be read or written.
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
        for (doc, fields) in &reached.docs {
            if !fields.is_empty() {
                continue;
            }
            match self.docs.get(doc) {
                Some((a, ty)) if a == app => {
                    let ty = ty.clone();
                    self.record(sink, app, doc, &ty)?;
                }
                // Gone from the record (or never this package's): what the index held of it
                // goes.
                None if sink.text(doc, TYPE)?.is_some() => {
                    sink.remove(doc)?;
                    sink.doc(doc, None)?;
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
        if sink.keeps_columns() {
            let ends = reflect::ends_of(&self.doc.committed_document().get_changes(&[]));
            for (resource, (first, last)) in &ends {
                if doc_of(resource) == doc {
                    sink.put_field(resource, CREATED, &first.value())?;
                    sink.put_field(resource, UPDATED, &last.value())?;
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
        let Some((root, schema)) = self.record_of(doc) else {
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
                let found = crate::items::find(self.doc, (doc, &root, schema), self.nested, &id);
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
                let before = placed_in(sink, &path)?;
                match (before, target) {
                    (None, None) => {}
                    (Some(before), None) => {
                        self.leave(sink, &path, &before)?;
                        self.remove_tree(sink, &path)?;
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
                        sink.clear(&path)?;
                        let own = self.own_fields(sink, &path, found.obj.as_ref(), item_schema)?;
                        for field in own
                            .fields
                            .iter()
                            .filter(|f| f.kind == MergeKind::Collection)
                        {
                            sink.replace(&member_list(&path, &field.id), Vec::new())?;
                        }
                        self.arrive(sink, (&path, own), &place)?;
                        self.derive_one(sink, &path, own, Pass::All)?;
                        self.place_under(sink, &path, own)?;
                        self.done.insert(path);
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
                        self.arrive(sink, (&path, own), &place)?;
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
    ) -> Result<(), IndexError> {
        let Some(id) = id_of(path) else {
            return Ok(());
        };
        let holder = place_holder(doc_of(path), place);
        sink.put_text(path, KEY, &place.key)?;
        sink.put_text(path, IN, &in_of(&holder, &place.field))?;
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
        // The items under an item that goes count it no more; one whose parent goes with it
        // needs nothing, as its parent's list goes with it.
        let mut above: BTreeSet<String> = BTreeSet::new();
        for resource in &found {
            let Some(schema) = sink.text(resource, TYPE)?.and_then(|ty| lookup(&ty)) else {
                continue;
            };
            let Some(parent) = schema.parent() else {
                continue;
            };
            let Some(stands) = derived::parent_of(sink, resource, parent)? else {
                continue;
            };
            if found.contains(&stands) {
                continue;
            }
            derived::standing(sink, resource, parent, Some(&stands), false)?;
            above.insert(stands);
        }
        for resource in found.into_iter().rev() {
            sink.remove(&resource)?;
            self.done.insert(resource);
        }
        for stands in above {
            self.derive_item_and_up(sink, &stands)?;
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
        let Some((obj, schema)) = self.locate(path) else {
            return Ok(());
        };
        let parent = schema.parent();
        let was = match parent {
            Some(parent) => sink.field(path, parent)?,
            None => None,
        };
        let own = self.own_fields(sink, path, obj.as_ref(), schema)?;
        let (holder, field, key) = &placed;
        self.index_columns(sink, path, own, Some((holder, field, key)), None)?;
        if let Some(parent) = parent {
            let before = was.as_ref().and_then(derived::parent_in);
            let now = sink
                .field(path, parent)?
                .as_ref()
                .and_then(derived::parent_in);
            derived::reparent(sink, path, parent, before.as_deref(), now.as_deref())?;
            for above in [before.as_deref(), now.as_deref()].into_iter().flatten() {
                self.derive_item_and_up(sink, above)?;
            }
        }
        self.derive_item_and_up(sink, path)?;
        self.done.insert(path.to_owned());
        Ok(())
    }

    /// The item at `path`, new to the index, stands under the item its parent field names: that
    /// item counts it from now, as do the items above it.
    fn place_under(
        &mut self,
        sink: &mut impl Sink,
        path: &str,
        schema: &'static TypeSchema,
    ) -> Result<(), IndexError> {
        let Some(parent) = schema.parent() else {
            return Ok(());
        };
        let Some(above) = derived::parent_of(sink, path, parent)? else {
            return Ok(());
        };
        // The list is the parent's, and holds the item whether or not the parent is indexed
        // yet: one that arrives after its items reads them when it does.
        derived::standing(sink, path, parent, Some(&above), true)?;
        if sink.text(&above, TYPE)?.is_none() {
            return Ok(());
        }
        self.derive_item_and_up(sink, &above)
    }

    /// The field `field` of the resource at `path` changed.
    fn field(&mut self, sink: &mut impl Sink, path: &str, field: &str) -> Result<(), IndexError> {
        // An item in no collection is in no list: nothing of it is held.
        if id_of(path).is_some() && sink.text(path, TYPE)?.is_none() {
            return Ok(());
        }
        let Some((obj, schema)) = self.locate(path) else {
            return Ok(());
        };
        let Some(spec) = schema.field(field) else {
            return Ok(());
        };
        if matches!(spec.kind, MergeKind::Record | MergeKind::Collection) {
            // A collection is its items' placements, which are derived as they are written.
            return Ok(());
        }
        if spec.derived.is_some() {
            // Worked out from the collection, never from the record.
            return Ok(());
        }
        let now = self.value(obj.as_ref(), spec);
        let before = sink.field(path, field)?;
        if before.as_ref() == Some(&now) {
            return Ok(());
        }
        sink.put_field(path, field, &now)?;
        // The columns reading the field, in the collection the resource is in and across the
        // workspace.
        if sink.keeps_columns() && schema.columns.iter().any(|c| columns::reads(c, field)) {
            let placed = placed_in(sink, path)?;
            let scope = placed
                .as_ref()
                .map(|(holder, coll, key)| (holder.as_str(), coll.as_str(), key.as_str()));
            self.index_columns(sink, path, schema, scope, Some(field))?;
        }
        // The values derived from the field: on the item, and up the items it stands under.
        if schema.parent() == Some(field) {
            let before = before.as_ref().and_then(derived::parent_in);
            let now = derived::parent_in(&now);
            derived::reparent(sink, path, field, before.as_deref(), now.as_deref())?;
            for above in [before.as_deref(), now.as_deref()].into_iter().flatten() {
                self.derive_item_and_up(sink, above)?;
            }
        } else if derived::reads(schema, field) {
            self.derive_item_and_up(sink, path)?;
        }
        Ok(())
    }

    /// Work out again the values the type `schema` derives for the item at `path`, those of the
    /// `pass` it asks for, and answer whether one of them reads the items above the item.
    fn derive_one(
        &mut self,
        sink: &mut impl Sink,
        path: &str,
        schema: &'static TypeSchema,
        pass: Pass,
    ) -> Result<bool, IndexError> {
        let mut above = false;
        for field in &schema.fields {
            let Some(expr) = &field.derived else {
                continue;
            };
            let reach = derived::reach(schema, field);
            let worked = match pass {
                Pass::All => true,
                Pass::Under => reach == derived::Reach::Under,
                Pass::Above => reach == derived::Reach::Above,
            };
            if !worked {
                continue;
            }
            let now = derived::eval(sink, path, schema, expr)?;
            let before = field_value(sink, path, field)?;
            if now.is(&before) {
                continue;
            }
            sink.put_field(path, &field.id, &now.value())?;
            self.derived_keys
                .entry(tilolang_runtime::store::doc_of(path).to_owned())
                .or_default()
                .push(FieldKey {
                    resource: path.to_owned(),
                    field: field.id.clone(),
                });
            above |= reach == derived::Reach::Above;
            // A column reading the value, in the collection the item is in and across the
            // workspace.
            if sink.keeps_columns() && schema.columns.iter().any(|c| columns::reads(c, &field.id)) {
                let held = placed_in(sink, path)?;
                let scope = held
                    .as_ref()
                    .map(|(holder, coll, key)| (holder.as_str(), coll.as_str(), key.as_str()));
                self.index_columns(sink, path, schema, scope, Some(&field.id))?;
            }
        }
        Ok(above)
    }

    /// Work out again the values the item at `path` derives, and those of the items it stands
    /// under, which read what it holds; a key reading the items above the item moves every item
    /// under it with it, from the one that stands highest.
    fn derive_item_and_up(&mut self, sink: &mut impl Sink, path: &str) -> Result<(), IndexError> {
        let mut moved: Option<String> = None;
        for at in derived::chain(sink, path)? {
            let Some(schema) = derived::type_of(sink, &at)? else {
                continue;
            };
            // The chain comes deepest first, so the last one an item above it reads stands
            // highest, and its items under it are read from it.
            if self.derive_one(sink, &at, schema, Pass::All)? {
                moved = Some(at);
            }
        }
        match moved {
            Some(at) => self.derive_below(sink, &at),
            None => Ok(()),
        }
    }

    /// Work out again the values the items under `path` derive from `path` and the items above
    /// it: one that did not move leaves the items under it as they were, as a key reading the
    /// items above an item that is not finished is answered by the next one that is.
    fn derive_below(&mut self, sink: &mut impl Sink, path: &str) -> Result<(), IndexError> {
        let Some(schema) = derived::type_of(sink, path)? else {
            return Ok(());
        };
        for child in derived::children(sink, path, schema)? {
            let Some(child_schema) = derived::type_of(sink, &child)? else {
                continue;
            };
            if self.derive_one(sink, &child, child_schema, Pass::Above)? {
                self.derive_below(sink, &child)?;
            }
        }
        Ok(())
    }

    /// Work out the values a record derived whole holds, over every item it visited: the items
    /// under each item are read from the parent field each item names, the values that read
    /// them worked out from the deepest item up, and then the values that read the items above
    /// an item, from the top item down.
    fn derive_whole(
        &mut self,
        sink: &mut impl Sink,
        visited: &[(String, &'static TypeSchema)],
    ) -> Result<(), IndexError> {
        for (path, schema) in visited {
            let Some(parent) = schema.parent() else {
                continue;
            };
            let Some(above) = derived::parent_of(sink, path, parent)? else {
                continue;
            };
            if sink.text(&above, TYPE)?.is_none() {
                continue;
            }
            if let Some(id) = id_of(path) {
                self.add(
                    sink,
                    derived::children_list(&above, parent),
                    derived::child_entry(id),
                )?;
            }
        }
        let mut order: Vec<(usize, &String, &'static TypeSchema)> =
            Vec::with_capacity(visited.len());
        for (path, schema) in visited {
            order.push((derived::depth(sink, path)?, path, schema));
        }
        order.sort_by_key(|(depth, ..)| std::cmp::Reverse(*depth));
        for (_, path, schema) in &order {
            self.derive_one(sink, path, schema, Pass::Under)?;
        }
        order.sort_by_key(|(depth, ..)| *depth);
        for (_, path, schema) in order {
            self.derive_one(sink, path, schema, Pass::Above)?;
        }
        Ok(())
    }
}

/// Which of a type's derived keys a pass works out: those that read the items under an item,
/// those that read the items above it, or every one of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pass {
    Under,
    Above,
    All,
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
