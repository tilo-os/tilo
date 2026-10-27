//! What the index holds, derived from the room record: a package's records whole, or what a
//! set of changes reached, field by field.
//!
//! The same derivation keeps the index on disk ([`super::Writer`]) and the store's open writes
//! ([`super::overlay::Overlay`]) as the record holds them: both are a [`Sink`].

use std::collections::{BTreeMap, BTreeSet};

use automerge::{ObjId, ObjType, ReadDoc, Value as AmValue};
use tilo_resource::collection::{collection_entries, in_stored_order, read_entry};
use tilolang_runtime::ids::{AppId, DocId, TypeId};
use tilolang_runtime::store::{ANY_FIELD, FieldKey, FieldValue, Scalar};
use tilolang_runtime::value::ResourceRef;

use super::lists::Entry;
use super::text::{NAMING, OTHER, words};
use super::{IndexError, KEY, TYPE, Writer};
use crate::reads::{default_value, field_of_slot, locate_ref, map_at, type_at};
use crate::schema::{FieldSchema, MergeKind, TypeSchema, lookup};

type Doc = tilo_resource::Doc;

/// The list of the items of the collection `field` of the resource `resource`.
#[must_use]
pub fn member_list(resource: &str, field: &str) -> String {
    format!("m\u{1}{resource}\u{1}{field}")
}

/// What starts the names of the filter columns of the items of the collection `field` of
/// `resource`, for the item field `item_field`.
#[must_use]
pub fn column_prefix(resource: &str, field: &str, item_field: &str) -> String {
    format!("c\u{1}{resource}\u{1}{field}\u{1}{item_field}\u{1}")
}

/// The list of the items of the collection `field` of `resource` whose `item_field` holds
/// the value written `value` ([`column_value`]).
#[must_use]
pub fn column_list(resource: &str, field: &str, item_field: &str, value: &str) -> String {
    format!("{}{value}", column_prefix(resource, field, item_field))
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

/// A value a filter column holds of, as its list is named: `None` for one a filter does not
/// compare (a list of its own, `x`, which a filter over the field does not answer from).
#[must_use]
pub fn column_value(value: &FieldValue) -> String {
    match value {
        FieldValue::Absent => "a".to_owned(),
        FieldValue::Scalar(Scalar::Bool(flag)) => format!("b{}", u8::from(*flag)),
        FieldValue::Scalar(Scalar::Int(value)) | FieldValue::Counter(value) => format!("i{value}"),
        FieldValue::Scalar(Scalar::Text(text)) => format!("t{text}"),
        _ => "x".to_owned(),
    }
}

/// The value a filter column's list holds of: `None` for the list of values a filter does not
/// compare.
#[must_use]
pub fn value_of_column(written: &str) -> Option<FieldValue> {
    let mut chars = written.chars();
    let tag = chars.next()?;
    let rest = chars.as_str();
    Some(match tag {
        'a' => FieldValue::Absent,
        'b' => FieldValue::Scalar(Scalar::Bool(rest == "1")),
        'i' => FieldValue::Scalar(Scalar::Int(rest.parse().ok()?)),
        't' => FieldValue::Scalar(Scalar::Text(rest.to_owned())),
        _ => return None,
    })
}

/// Whether the index keeps a filter column of `field` of an item: a register of a flag, a
/// number or a text, or a counter, as a filter reads.
#[must_use]
pub fn columned(field: &FieldSchema) -> bool {
    match field.kind {
        MergeKind::Register => !field.composite,
        MergeKind::Counter => true,
        _ => false,
    }
}

/// The resource holding the item at `path`, its collection and the item's id.
#[must_use]
pub fn item_of(path: &str) -> Option<(&str, &str, &str)> {
    let (parent, step) = path.rsplit_once('/')?;
    let (field, id) = step.split_once(':')?;
    Some((parent, field, id))
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

    /// Drop `resource` and everything inside it: fields, lists, postings.
    ///
    /// # Errors
    /// It cannot be written.
    fn remove(&mut self, resource: &str) -> Result<(), IndexError>;

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

    /// Drop every list whose name starts with `prefix`.
    ///
    /// # Errors
    /// It cannot be written.
    fn drop_lists(&mut self, prefix: &str) -> Result<(), IndexError>;

    /// The words of a text of `resource` changed from `old` to `new`.
    ///
    /// # Errors
    /// It cannot be written.
    fn words(
        &mut self,
        resource: &str,
        old: Option<&str>,
        new: Option<&str>,
        weight: u8,
    ) -> Result<(), IndexError>;

    /// Whether it keeps filter columns and postings, or (the store's open writes) fields and
    /// membership alone.
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
        for (held, field, bytes) in self.subtree(resource)? {
            if !field.starts_with('$')
                && let Ok(FieldValue::Text(text)) = super::codec::get_value(&bytes)
            {
                for word in words(&text) {
                    self.unpost(&word, &held)?;
                }
            }
            self.remove_field(&held, &field)?;
        }
        for prefix in lists_within(resource) {
            self.drop_lists(&prefix)?;
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

    fn drop_lists(&mut self, prefix: &str) -> Result<(), IndexError> {
        Writer::drop_lists(self, prefix)
    }

    fn words(
        &mut self,
        resource: &str,
        old: Option<&str>,
        new: Option<&str>,
        weight: u8,
    ) -> Result<(), IndexError> {
        let before = old.map(words).unwrap_or_default();
        let after = new.map(words).unwrap_or_default();
        for gone in before.difference(&after) {
            self.unpost(gone, resource)?;
        }
        for word in &after {
            self.post(word, resource, weight)?;
        }
        Ok(())
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
    /// Every record of the room: its app and type.
    docs: &'d BTreeMap<DocId, (AppId, TypeId)>,
    pub reads: DocReads,
    /// Resources derived whole in this pass: a change naming them again is done.
    done: BTreeSet<String>,
}

/// What a set of changes reached: per document the fields (none: the document as a whole),
/// and per collection the items added, removed or moved.
#[derive(Clone, Debug, Default)]
pub struct Reached {
    pub docs: BTreeMap<DocId, Vec<FieldKey>>,
    pub items: BTreeMap<FieldKey, BTreeSet<String>>,
}

impl Reached {
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty() && self.items.is_empty()
    }
}

fn map_of(value: &AmValue<'_>, id: &ObjId) -> Option<ObjId> {
    matches!(value, AmValue::Object(ObjType::Map)).then(|| id.clone())
}

/// The weight of the words of the text field `field` of `schema`: the text that names a
/// resource is its first text field, in id order.
fn weight_of(schema: &TypeSchema, field: &FieldSchema) -> u8 {
    let first = schema
        .fields
        .iter()
        .filter(|f| f.kind == MergeKind::Text)
        .map(|f| f.id.as_str())
        .min();
    if first == Some(field.id.as_str()) {
        NAMING
    } else {
        OTHER
    }
}

impl<'d> Derive<'d> {
    pub fn new(doc: &'d Doc, docs: &'d BTreeMap<DocId, (AppId, TypeId)>) -> Self {
        Self {
            doc,
            docs,
            reads: DocReads::default(),
            done: BTreeSet::new(),
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
        for (doc, ty) in records {
            self.record(sink, app, &doc, &ty)?;
        }
        Ok(())
    }

    /// Derive the record `doc` of `app` anew.
    fn record(
        &mut self,
        sink: &mut impl Sink,
        app: &str,
        doc: &str,
        ty: &str,
    ) -> Result<(), IndexError> {
        sink.remove(doc)?;
        let obj = crate::reads::resource_obj(self.doc, app, doc);
        self.reads.located = self.reads.located.saturating_add(1);
        match (obj, lookup(ty)) {
            (Some(obj), Some(schema)) => {
                sink.doc(doc, Some(ty))?;
                self.tree(sink, doc, Some(&obj), schema)?;
            }
            _ => sink.doc(doc, None)?,
        }
        self.done.insert(doc.to_owned());
        Ok(())
    }

    /// Derive the resource at `path` (its map `obj`, of the type `schema`) and everything
    /// inside it; returns the values of its fields a filter column holds.
    fn tree(
        &mut self,
        sink: &mut impl Sink,
        path: &str,
        obj: Option<&ObjId>,
        schema: &'static TypeSchema,
    ) -> Result<Vec<(&'static FieldSchema, FieldValue)>, IndexError> {
        let ty = obj
            .and_then(|obj| type_at(self.doc, obj))
            .unwrap_or_else(|| schema.id.clone());
        sink.put_text(path, TYPE, &ty)?;
        let mut columns = Vec::new();
        for field in &schema.fields {
            match field.kind {
                MergeKind::Collection => self.collection(sink, path, field, obj)?,
                MergeKind::Record => {}
                _ => {
                    let value = self.value(obj, field);
                    if field.kind == MergeKind::Text
                        && let FieldValue::Text(text) = &value
                    {
                        sink.words(path, None, Some(text), weight_of(schema, field))?;
                    }
                    sink.put_field(path, &field.id, &value)?;
                    if columned(field) {
                        columns.push((field, value));
                    }
                }
            }
        }
        Ok(columns)
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

    /// The items of the collection `field` of the resource map `obj`, in order: their order
    /// keys, ids and maps.
    fn stored(&mut self, obj: Option<&ObjId>, field: &str) -> Vec<(String, String, Option<ObjId>)> {
        let Some(coll) = obj.and_then(|obj| map_at(self.doc, obj, field)) else {
            return Vec::new();
        };
        self.reads.listings = self.reads.listings.saturating_add(1);
        let entries = collection_entries(self.doc, &coll);
        self.reads.entries = self
            .reads
            .entries
            .saturating_add(u64::try_from(entries.len()).unwrap_or(u64::MAX));
        let mut stored: Vec<_> = entries
            .into_iter()
            .filter_map(|entry| read_entry(self.doc, entry))
            .collect();
        in_stored_order(&mut stored);
        stored
            .into_iter()
            .map(|s| {
                let obj = if s.entry.is_some() {
                    s.value.as_ref().and_then(|(value, id)| map_of(value, id))
                } else {
                    None
                };
                (s.key, s.id.as_str().to_owned(), obj)
            })
            .collect()
    }

    /// Derive the collection `field` of the resource at `path` whole: its list, its items
    /// and their filter columns.
    fn collection(
        &mut self,
        sink: &mut impl Sink,
        path: &str,
        field: &FieldSchema,
        obj: Option<&ObjId>,
    ) -> Result<(), IndexError> {
        let item_schema = field.record.as_deref().and_then(lookup);
        let stored = self.stored(obj, &field.id);
        let mut members = Vec::with_capacity(stored.len());
        let mut columns: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
        if sink.keeps_columns() {
            sink.drop_lists(&format!("c\u{1}{path}\u{1}{}\u{1}", field.id))?;
        }
        for (key, id, item_obj) in stored {
            members.push((key.clone(), id.clone()));
            let child = format!("{path}/{}:{id}", field.id);
            sink.put_text(&child, KEY, &key)?;
            let Some(item_schema) = item_schema else {
                continue;
            };
            let values = self.tree(sink, &child, item_obj.as_ref(), item_schema)?;
            if sink.keeps_columns() {
                for (item_field, value) in values {
                    columns
                        .entry(column_list(
                            path,
                            &field.id,
                            &item_field.id,
                            &column_value(&value),
                        ))
                        .or_default()
                        .push((key.clone(), id.clone()));
                }
            }
        }
        sink.replace(&member_list(path, &field.id), members)?;
        for (list, entries) in columns {
            sink.replace(&list, entries)?;
        }
        Ok(())
    }

    /// Where the resource at `path` is in the record, and its type: `None` when it is not
    /// there.
    fn locate(&mut self, path: &str) -> Option<(Option<ObjId>, &'static TypeSchema)> {
        let doc = tilolang_runtime::store::doc_of(path);
        let (app, ty) = self.docs.get(doc)?;
        let at = ResourceRef::parse_id(path, ty.clone())?;
        self.reads.located = self.reads.located.saturating_add(1);
        let located = locate_ref(self.doc, (app, ty), &at).ok()?;
        Some((located.obj, located.schema))
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
        for (coll, ids) in &reached.items {
            if !of_app(coll.doc(), self.docs) || self.within_done(&coll.resource) {
                continue;
            }
            self.items(sink, coll, ids)?;
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
                } else if !reached.items.contains_key(key) {
                    self.field(sink, &key.resource, &key.field)?;
                }
            }
        }
        Ok(())
    }

    fn within_done(&self, path: &str) -> bool {
        let mut at = path;
        loop {
            if self.done.contains(at) {
                return true;
            }
            match at.rsplit_once('/') {
                Some((parent, _)) => at = parent,
                None => return false,
            }
        }
    }

    /// The items `ids` of the collection `coll` were added, removed or moved.
    fn items(
        &mut self,
        sink: &mut impl Sink,
        coll: &FieldKey,
        ids: &BTreeSet<String>,
    ) -> Result<(), IndexError> {
        let Some((obj, schema)) = self.locate(&coll.resource) else {
            // The resource holding it is gone: so is what it held.
            return Ok(());
        };
        let Some(field) = schema.field(&coll.field) else {
            return Ok(());
        };
        let map = obj
            .as_ref()
            .and_then(|obj| map_at(self.doc, obj, &field.id));
        for id in ids {
            let now = map
                .as_ref()
                .and_then(|map| tilo_resource::stored_entry(self.doc, map, id))
                .map(|stored| {
                    let obj = if stored.entry.is_some() {
                        stored
                            .value
                            .as_ref()
                            .and_then(|(value, at)| map_of(value, at))
                    } else {
                        None
                    };
                    (stored.key, obj)
                });
            self.reads.entries = self.reads.entries.saturating_add(1);
            self.item(sink, (&coll.resource, field), id, now)?;
        }
        Ok(())
    }

    /// Bring the item `id` of the collection `field` of `resource` in line with the record,
    /// where it now has the order key and map `now` (or is gone).
    fn item(
        &mut self,
        sink: &mut impl Sink,
        (resource, field): (&str, &FieldSchema),
        id: &str,
        now: Option<(String, Option<ObjId>)>,
    ) -> Result<(), IndexError> {
        let child = format!("{resource}/{}:{id}", field.id);
        let list = member_list(resource, &field.id);
        let before = sink.text(&child, KEY)?;
        let item_schema = field.record.as_deref().and_then(lookup);
        match (before, now) {
            (Some(key), None) => {
                sink.remove_entry(&list, &(key.clone(), id.to_owned()))?;
                if let Some(item_schema) = item_schema {
                    Self::columns(sink, (resource, field), &child, item_schema, (&key, false))?;
                }
                sink.remove(&child)?;
                self.done.insert(child);
            }
            (None, Some((key, obj))) => {
                sink.remove(&child)?;
                if let Some(item_schema) = item_schema {
                    self.tree(sink, &child, obj.as_ref(), item_schema)?;
                }
                sink.put_text(&child, KEY, &key)?;
                if let Some(item_schema) = item_schema {
                    Self::columns(sink, (resource, field), &child, item_schema, (&key, true))?;
                }
                sink.insert(&list, (key, id.to_owned()))?;
                self.done.insert(child);
            }
            (Some(old), Some((key, _))) if old != key => {
                sink.remove_entry(&list, &(old.clone(), id.to_owned()))?;
                sink.insert(&list, (key.clone(), id.to_owned()))?;
                if let Some(item_schema) = item_schema {
                    Self::columns(sink, (resource, field), &child, item_schema, (&old, false))?;
                }
                sink.put_text(&child, KEY, &key)?;
                if let Some(item_schema) = item_schema {
                    Self::columns(sink, (resource, field), &child, item_schema, (&key, true))?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Take the item at `child` (order key `key`) out of its filter columns (`into` false), or
    /// put it in them as its fields now hold.
    fn columns(
        sink: &mut impl Sink,
        (resource, field): (&str, &FieldSchema),
        child: &str,
        item_schema: &TypeSchema,
        (key, into): (&str, bool),
    ) -> Result<(), IndexError> {
        if !sink.keeps_columns() {
            return Ok(());
        }
        let Some((_, _, id)) = item_of(child) else {
            return Ok(());
        };
        for item_field in item_schema.fields.iter().filter(|f| columned(f)) {
            let value = sink
                .field(child, &item_field.id)?
                .unwrap_or_else(|| default_value(item_field));
            let list = column_list(resource, &field.id, &item_field.id, &column_value(&value));
            let entry = (key.to_owned(), id.to_owned());
            if into {
                sink.insert(&list, entry)?;
            } else {
                sink.remove_entry(&list, &entry)?;
            }
        }
        Ok(())
    }

    /// The resource at `path` was put, replaced or removed as a whole: derive it anew, where
    /// its collection keeps it.
    fn resource(&mut self, sink: &mut impl Sink, path: &str) -> Result<(), IndexError> {
        let Some((parent, coll, id)) = item_of(path) else {
            // A record: derived whole.
            let doc = path.to_owned();
            if let Some((app, ty)) = self.docs.get(&doc).cloned() {
                self.record(sink, &app, &doc, &ty)?;
            }
            return Ok(());
        };
        let Some((parent_obj, parent_schema)) = self.locate(parent) else {
            return Ok(());
        };
        let Some(field) = parent_schema.field(coll) else {
            return Ok(());
        };
        let now = parent_obj
            .as_ref()
            .and_then(|obj| map_at(self.doc, obj, coll))
            .and_then(|map| tilo_resource::stored_entry(self.doc, &map, id))
            .map(|stored| {
                let obj = if stored.entry.is_some() {
                    stored
                        .value
                        .as_ref()
                        .and_then(|(value, at)| map_of(value, at))
                } else {
                    None
                };
                (stored.key, obj)
            });
        if let Some(key) = sink.text(path, KEY)? {
            // Out of the columns as it was, and out of its list: put back below as it is.
            if let Some(item_schema) = field.record.as_deref().and_then(lookup) {
                Self::columns(sink, (parent, field), path, item_schema, (&key, false))?;
            }
            sink.remove_entry(&member_list(parent, coll), &(key, id.to_owned()))?;
            sink.remove(path)?;
        }
        self.item(sink, (parent, field), id, now)?;
        self.done.insert(path.to_owned());
        Ok(())
    }

    /// The field `field` of the resource at `path` changed.
    fn field(&mut self, sink: &mut impl Sink, path: &str, field: &str) -> Result<(), IndexError> {
        let Some((obj, schema)) = self.locate(path) else {
            return Ok(());
        };
        let Some(spec) = schema.field(field) else {
            return Ok(());
        };
        match spec.kind {
            MergeKind::Record => Ok(()),
            MergeKind::Collection => self.membership(sink, path, spec, obj.as_ref()),
            _ => {
                let now = self.value(obj.as_ref(), spec);
                let before = sink.field(path, field)?;
                if before.as_ref() == Some(&now) {
                    return Ok(());
                }
                if columned(spec)
                    && sink.keeps_columns()
                    && let Some((parent, coll, id)) = item_of(path)
                    && let Some(key) = sink.text(path, KEY)?
                {
                    let old = before.clone().unwrap_or_else(|| default_value(spec));
                    let entry = (key, id.to_owned());
                    sink.remove_entry(
                        &column_list(parent, coll, field, &column_value(&old)),
                        &entry,
                    )?;
                    sink.insert(
                        &column_list(parent, coll, field, &column_value(&now)),
                        entry,
                    )?;
                }
                if spec.kind == MergeKind::Text {
                    let old = match &before {
                        Some(FieldValue::Text(text)) => Some(text.as_str()),
                        _ => None,
                    };
                    let new = match &now {
                        FieldValue::Text(text) => Some(text.as_str()),
                        _ => None,
                    };
                    sink.words(path, old, new, weight_of(schema, spec))?;
                }
                sink.put_field(path, field, &now)
            }
        }
    }

    /// The collection `field` of the resource at `path` changed in a way no item names: its
    /// items as the record holds them now, against those the sink holds.
    fn membership(
        &mut self,
        sink: &mut impl Sink,
        path: &str,
        field: &FieldSchema,
        obj: Option<&ObjId>,
    ) -> Result<(), IndexError> {
        let now = self.stored(obj, &field.id);
        let before: BTreeMap<String, String> = sink
            .entries(&member_list(path, &field.id))?
            .into_iter()
            .map(|(key, id)| (id, key))
            .collect();
        let mut seen = BTreeSet::new();
        for (key, id, item_obj) in now {
            seen.insert(id.clone());
            if before.get(&id) != Some(&key) {
                self.item(sink, (path, field), &id, Some((key, item_obj)))?;
            }
        }
        for id in before.keys().filter(|id| !seen.contains(*id)) {
            self.item(sink, (path, field), id, None)?;
        }
        Ok(())
    }
}
