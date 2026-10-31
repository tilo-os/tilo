//! Reading one field of one resource straight from the room record, and naming the fields a
//! set of Automerge changes wrote.
//!
//! A read walks from the record's root to the resource's map (`tl/<app>/<id>`), to the item's
//! map by its id ([`crate::items`]), and reads the one field: its cost does not grow with the
//! size of the resource. Materializing the resource as a [`crate::DynRecord`] instead
//! would read every item of every collection in it for each field a view reads.

use std::collections::{BTreeMap, BTreeSet};

use automerge::legacy::Key as LegacyKey;
use automerge::{Change, ObjId, ObjType, Prop, ReadDoc, ScalarValue, Value as AmValue};
use serde_json::Value as Json;
use tilo_resource::model::{MapSlot, load_slot};
use tilo_resource::{Counter, Map, Set, read_key};
use tilolang_runtime::ids::{AppId, DocId, TypeId};
use tilolang_runtime::place::{AT_KEY, ITEMS_KEY};
use tilolang_runtime::store::{ANY_FIELD, FieldKey, FieldValue, Scalar, StoreError};
use tilolang_runtime::value::ResourceRef;

use crate::action::RECORDS_KEY;
use crate::schema::{FieldSchema, MergeKind, TYPE_KEY, TypeSchema, lookup};

type Doc = tilo_resource::Doc;

fn not_found(what: impl Into<String>) -> StoreError {
    StoreError::NotFound(what.into())
}

fn backend(error: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(error.to_string())
}

pub(crate) fn map_at(doc: &Doc, obj: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(obj, key) {
        Ok(Some((AmValue::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}

/// The map of the resource `id` of `app`: under the app's id, or under an id it was published as
/// before (`previously:`), where its older records stay.
pub fn resource_obj(doc: &Doc, app: &str, id: &str) -> Option<ObjId> {
    let records = map_at(doc, &automerge::ROOT, RECORDS_KEY)?;
    if let Some(found) = map_at(doc, &records, app).and_then(|held| map_at(doc, &held, id)) {
        return Some(found);
    }
    crate::schema::previously(app)
        .iter()
        .find_map(|old| map_at(doc, &map_at(doc, &records, old)?, id))
}

/// The type stored in `$type` of the map `obj`.
pub fn type_at(doc: &Doc, obj: &ObjId) -> Option<TypeId> {
    match doc.get(obj, TYPE_KEY) {
        Ok(Some((AmValue::Scalar(scalar), _))) => match scalar.as_ref() {
            ScalarValue::Str(ty) => Some(ty.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// Where a resource's fields are: its map, or `None` for an item stored as its order key alone
/// (every field reads as its default); and its type. For an item, where it is stored.
pub struct Located {
    pub obj: Option<ObjId>,
    pub schema: &'static TypeSchema,
    pub item: Option<crate::items::Found>,
}

/// Locate the resource `at`, given the app and type of its document: the record's map, or the
/// map of its item `at.item` wherever that is stored ([`crate::items`]). Whether the item is
/// in a collection now is the index's to say.
///
/// # Errors
/// `NotFound` for a document the record does not hold, or an item it holds none of.
pub fn locate_ref(
    doc: &Doc,
    (app, ty): (&AppId, &TypeId),
    at: &ResourceRef,
    nested: &crate::items::Nested,
) -> Result<Located, StoreError> {
    let root =
        resource_obj(doc, app, &at.doc).ok_or_else(|| not_found(format!("resource {}", at.doc)))?;
    let schema = lookup(ty).ok_or_else(|| not_found(format!("type {ty}")))?;
    let Some(item) = &at.item else {
        return Ok(Located {
            obj: Some(root),
            schema,
            item: None,
        });
    };
    let found = crate::items::find(doc, (&at.doc, &root, schema), nested, item)
        .ok_or_else(|| not_found(format!("item `{item}`")))?;
    let item_schema = found
        .ty
        .as_deref()
        .and_then(lookup)
        .ok_or_else(|| not_found(format!("the type of item `{item}`")))?;
    Ok(Located {
        obj: found.obj.clone(),
        schema: item_schema,
        item: Some(found),
    })
}

fn scalar_of_json(value: &Json, composite: bool) -> Scalar {
    if composite {
        return Scalar::Json(value.clone());
    }
    match value {
        Json::Bool(flag) => Scalar::Bool(*flag),
        Json::Number(number) => number.as_i64().map_or_else(
            || Scalar::Float(number.as_f64().unwrap_or(0.0)),
            Scalar::Int,
        ),
        Json::String(text) => Scalar::Text(text.clone()),
        other => Scalar::Json(other.clone()),
    }
}

fn element_key(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The value of `field` in the located resource.
///
/// # Errors
/// `Backend` when the stored value does not have the field's shape.
pub fn read_field(
    doc: &Doc,
    located: &Located,
    field: &FieldSchema,
) -> Result<FieldValue, StoreError> {
    let Some(obj) = located.obj.as_ref() else {
        return Ok(default_value(field));
    };
    let slot = doc
        .get(obj, field.id.as_str())
        .map_err(backend)?
        .map(|(value, id)| (value.into_owned(), id));
    field_of_slot(doc, slot, field)
}

/// The value of `field` from what its key holds (`None` when nothing): [`read_field`] once
/// the field's key is read, or a walk of the resource's map has read it with the others.
///
/// # Errors
/// `Backend` when the stored value does not have the field's shape.
pub fn field_of_slot(
    doc: &Doc,
    slot: MapSlot,
    field: &FieldSchema,
) -> Result<FieldValue, StoreError> {
    let id = field.id.as_str();
    Ok(match field.kind {
        MergeKind::Register => {
            let value: Json = load_slot(doc, slot, id).map_err(backend)?;
            let value = if value.is_null() {
                field.default.clone()
            } else {
                value
            };
            if value.is_null() {
                FieldValue::Absent
            } else {
                FieldValue::Scalar(scalar_of_json(&value, field.composite))
            }
        }
        MergeKind::Counter => {
            FieldValue::Counter(load_slot::<Counter>(doc, slot, id).map_err(backend)?.get())
        }
        MergeKind::Text => FieldValue::Text(load_slot::<String>(doc, slot, id).map_err(backend)?),
        MergeKind::RichText => FieldValue::Rich(match slot {
            Some((AmValue::Object(ObjType::Map), obj)) => {
                tilolang_runtime::rich::read_under(doc, &obj)
            }
            _ => tilolang_runtime::rich::RichValue::default(),
        }),
        MergeKind::Set => {
            let set: Set<Json> = load_slot(doc, slot, id).map_err(backend)?;
            let mut keys: Vec<String> = set.iter().map(element_key).collect();
            keys.sort();
            FieldValue::Keys(keys)
        }
        MergeKind::Map => {
            let map: Map<Json> = load_slot(doc, slot, id).map_err(backend)?;
            FieldValue::Entries(
                map.iter()
                    .map(|(key, value)| (key.clone(), scalar_of_json(value, false)))
                    .collect(),
            )
        }
        MergeKind::Collection => {
            return Err(StoreError::Invalid(format!(
                "collection `{id}` is read from its items' placements"
            )));
        }
        MergeKind::Record => {
            return Err(StoreError::Invalid(
                "a nested record is not a field the runtime reads".into(),
            ));
        }
    })
}

/// The value of `field` from `json`, what its key held as JSON at some version
/// ([`tilo_resource::history::held_at`]): [`field_of_slot`] of a value read from history.
///
/// # Errors
/// `Backend` when the value does not have the field's shape.
pub fn field_of_json(json: &Json, field: &FieldSchema) -> Result<FieldValue, StoreError> {
    let shape = |what: &str| {
        StoreError::Backend(format!(
            "field `{}` held {json} where it holds {what}",
            field.id
        ))
    };
    Ok(match field.kind {
        MergeKind::Register => {
            let value = if json.is_null() { &field.default } else { json };
            if value.is_null() {
                FieldValue::Absent
            } else {
                FieldValue::Scalar(scalar_of_json(value, field.composite))
            }
        }
        MergeKind::Counter => match json {
            Json::Null => FieldValue::Counter(0),
            other => FieldValue::Counter(other.as_i64().ok_or_else(|| shape("a count"))?),
        },
        MergeKind::Text => match json {
            Json::Null => FieldValue::Text(String::new()),
            Json::String(text) => FieldValue::Text(text.clone()),
            _ => return Err(shape("text")),
        },
        MergeKind::Set => match json {
            Json::Null => FieldValue::Keys(Vec::new()),
            Json::Object(elements) => {
                let mut keys: Vec<String> = elements.keys().cloned().collect();
                keys.sort();
                FieldValue::Keys(keys)
            }
            _ => return Err(shape("a set")),
        },
        MergeKind::Map => match json {
            Json::Null => FieldValue::Entries(Vec::new()),
            Json::Object(entries) => FieldValue::Entries(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), scalar_of_json(value, false)))
                    .collect(),
            ),
            _ => return Err(shape("a map")),
        },
        MergeKind::RichText | MergeKind::Collection | MergeKind::Record => {
            return Err(StoreError::Invalid(format!(
                "field `{}` is not read from what its key held",
                field.id
            )));
        }
    })
}

/// A text field's value as the store holds it, when it holds text: a text, or rich text. What
/// it says depends on who reads it, as a rich text's references read for each reader
/// ([`crate::reading::said`]).
#[must_use]
pub fn text_value(value: FieldValue) -> Option<FieldValue> {
    match value {
        FieldValue::Text(_) | FieldValue::Rich(_) => Some(value),
        _ => None,
    }
}

/// Whether a text value ([`text_value`]) says nothing but spaces, whoever reads it: a reference
/// reads as more than spaces to everyone.
#[must_use]
pub fn is_blank(value: &FieldValue) -> bool {
    match value {
        FieldValue::Text(text) => text.trim().is_empty(),
        FieldValue::Rich(rich) => rich.written_text().trim().is_empty(),
        _ => true,
    }
}

/// Whether a text value ([`text_value`]) holds no character at all.
#[must_use]
pub fn is_empty(value: &FieldValue) -> bool {
    match value {
        FieldValue::Text(text) => text.is_empty(),
        FieldValue::Rich(rich) => rich.blocks.iter().all(|b| b.text.is_empty()),
        _ => true,
    }
}

/// What `field` of an item stored as its order key alone reads as.
#[must_use]
pub fn default_value(field: &FieldSchema) -> FieldValue {
    match field.kind {
        MergeKind::Register if field.default.is_null() => FieldValue::Absent,
        MergeKind::Register => FieldValue::Scalar(scalar_of_json(&field.default, field.composite)),
        MergeKind::Counter => FieldValue::Counter(0),
        MergeKind::Text => FieldValue::Text(String::new()),
        MergeKind::RichText => FieldValue::Rich(tilolang_runtime::rich::RichValue::default()),
        MergeKind::Set => FieldValue::Keys(Vec::new()),
        MergeKind::Map => FieldValue::Entries(Vec::new()),
        MergeKind::Collection | MergeKind::Record => FieldValue::Items(Vec::new()),
    }
}

/// What a set of patches changed.
#[derive(Debug, Default)]
pub struct Changed {
    /// Per document, the fields that changed; an empty list for a document changed as a whole.
    pub docs: BTreeMap<DocId, Vec<FieldKey>>,
    /// Whether a resource was created or removed, or the record's layout above the resources
    /// changed: the index of resources must be read again.
    pub structure: bool,
    /// Whether an op's object could not be placed in the record: every document must be read
    /// again.
    pub unplaced: bool,
    /// Whether the record's layout above the resources changed (not just one resource):
    /// anything read of any resource may be stale.
    pub layout: bool,
    /// Per document, the items whose placement was written (moved, removed, inserted), each
    /// with whether the change made it.
    pub placed: Placed,
    /// The documents an entry of a nested collection (a record written before items were
    /// flat) was put in or deleted from: where their nested items are is read again.
    pub nested: BTreeSet<DocId>,
}

/// Per document, the items whose placement changes wrote, each with whether they made it.
pub type Placed = BTreeMap<DocId, BTreeMap<String, bool>>;

/// Name the (resource, field) pairs `changes` wrote, once `doc` holds them, and the items they
/// placed. `index` gives each document's app and type, so the walk knows which fields are
/// collections whose items are resources of their own.
///
/// Each op names its object and key; the object's place in the record is found by walking up
/// its parents, one lookup per level. (Automerge's own patches would name the same places, but
/// building one walks every entry of each map above the change, which in a collection of 10,000
/// items costs milliseconds per keystroke.) Ops overwritten by a concurrent op are named as
/// well: a view re-reading a field that did not change shows the same thing.
#[must_use]
pub fn changed_by(
    changes: &[Change],
    doc: &Doc,
    index: &BTreeMap<DocId, (AppId, TypeId)>,
) -> Changed {
    // Per object, the keys written and whether an object was made under each.
    let mut writes: BTreeMap<String, BTreeMap<Option<String>, bool>> = BTreeMap::new();
    for change in changes {
        for op in change.decode().operations {
            let key = match op.key {
                LegacyKey::Map(key) => Some(key.to_string()),
                LegacyKey::Seq(_) => None,
            };
            let made = matches!(op.action, automerge::legacy::OpType::Make(_));
            let held = writes
                .entry(op.obj.to_string())
                .or_default()
                .entry(key)
                .or_default();
            *held = *held || made;
        }
    }
    let mut out = ChangedBuilder::default();
    for (obj, keys) in writes {
        let Some(path) = path_of(doc, &obj) else {
            out.unplaced = true;
            continue;
        };
        for (key, made) in keys {
            let mut props: Vec<&Prop> = path.iter().collect();
            let own = key.map(Prop::Map);
            if let Some(own) = own.as_ref() {
                props.push(own);
            }
            out.patch(&props, made, (doc, index));
        }
    }
    out.finish()
}

/// The keys from the record's root down to the object `obj` (as Automerge prints its id).
fn path_of(doc: &Doc, obj: &str) -> Option<Vec<Prop>> {
    let obj = doc.import_obj(obj).ok()?;
    if obj == automerge::ROOT {
        return Some(Vec::new());
    }
    let mut path: Vec<Prop> = doc.parents(&obj).ok()?.map(|parent| parent.prop).collect();
    path.reverse();
    Some(path)
}

#[derive(Default)]
struct ChangedBuilder {
    docs: BTreeMap<DocId, Vec<FieldKey>>,
    whole: BTreeSet<DocId>,
    structure: bool,
    unplaced: bool,
    layout: bool,
    placed: Placed,
    nested: BTreeSet<DocId>,
}

fn key_of(prop: &Prop) -> Option<&str> {
    match prop {
        Prop::Map(key) => Some(key.as_str()),
        Prop::Seq(_) => None,
    }
}

impl ChangedBuilder {
    fn patch(
        &mut self,
        props: &[&Prop],
        made: bool,
        (record, index): (&Doc, &BTreeMap<DocId, (AppId, TypeId)>),
    ) {
        // tl / <app> / <id> / <field> ...
        let keys: Vec<Option<&str>> = props.iter().map(|p| key_of(p)).collect();
        if keys.first().copied().flatten() != Some(RECORDS_KEY) {
            return;
        }
        let Some(Some(doc)) = keys.get(2).copied() else {
            self.structure = true;
            self.layout = true;
            return;
        };
        if keys.len() <= 3 {
            self.structure = true;
            self.whole(doc);
            return;
        }
        let Some((app, ty)) = index.get(doc) else {
            // A resource the index does not know yet: it was just created.
            self.structure = true;
            self.whole(doc);
            return;
        };
        let Some(schema) = lookup(ty) else {
            self.whole(doc);
            return;
        };
        if keys.get(3).copied().flatten() == Some(ITEMS_KEY) {
            self.flat_item(record, (app, doc), keys.get(4..).unwrap_or_default(), made);
            return;
        }
        self.within(doc, doc, schema, keys.get(3..).unwrap_or_default());
    }

    /// A write below `$items` of the record `doc`: `below` are the keys from the item's id on.
    fn flat_item(
        &mut self,
        record: &Doc,
        (app, doc): (&str, &str),
        below: &[Option<&str>],
        made: bool,
    ) {
        let Some(Some(item)) = below.first().copied() else {
            self.whole(doc);
            return;
        };
        let resource = format!("{doc}/{item}");
        match below.get(1).copied() {
            // The item's map was put or deleted: made, or (a replica's concurrent copy) replaced.
            None => {
                self.place(doc, item, made);
                self.any(doc, &resource);
            }
            Some(Some(AT_KEY)) => self.place(doc, item, false),
            Some(Some(TYPE_KEY)) => self.any(doc, &resource),
            Some(Some(key)) if key.starts_with('$') => {}
            Some(_) => {
                let schema = resource_obj(record, app, doc)
                    .and_then(|root| map_at(record, &root, ITEMS_KEY))
                    .and_then(|items| map_at(record, &items, item))
                    .and_then(|obj| type_at(record, &obj))
                    .and_then(|ty| lookup(&ty));
                match schema {
                    Some(schema) => {
                        self.within(doc, &resource, schema, below.get(1..).unwrap_or_default());
                    }
                    None => self.any(doc, &resource),
                }
            }
        }
    }

    /// A write below the map of `resource` (of type `schema`) of the record `doc`: `keys` are
    /// the keys from the field on. A collection field here is one of a record written before
    /// items were flat, holding its items nested.
    fn within(
        &mut self,
        doc: &str,
        resource: &str,
        schema: &'static TypeSchema,
        keys: &[Option<&str>],
    ) {
        let mut resource = resource.to_owned();
        let mut schema = schema;
        let mut at = 0usize;
        loop {
            let Some(Some(field)) = keys.get(at).copied() else {
                self.any(doc, &resource);
                return;
            };
            if field == AT_KEY {
                // A nested item's placement, written when it moved.
                if let Some((_, item)) = resource.rsplit_once('/') {
                    let item = item.to_owned();
                    self.place(doc, &item, false);
                }
                return;
            }
            if field == TYPE_KEY || field.starts_with('$') {
                // `$type`, `$app` and the outbox are not fields views read.
                if field == TYPE_KEY {
                    self.any(doc, &resource);
                }
                return;
            }
            let Some(spec) = schema.field(field) else {
                self.any(doc, &resource);
                return;
            };
            let below = keys.get(at.saturating_add(1)..).unwrap_or_default();
            let item_record = spec.record.as_deref().and_then(lookup);
            match (spec.kind, below) {
                // A nested item's value changed below its `v`: descend into the item, named by
                // its id.
                (MergeKind::Collection, [Some(item), Some("v"), _, ..]) => {
                    resource = format!("{doc}/{item}");
                    // An item of a collection of several types: read again whole.
                    let Some(item_schema) = item_record else {
                        self.any(doc, &resource);
                        return;
                    };
                    schema = item_schema;
                    at = at.saturating_add(3);
                }
                // A nested entry or its map was put, replaced or removed: where its items are,
                // its placement and every field.
                (MergeKind::Collection, [Some(item), Some("v")] | [Some(item)]) => {
                    if below.len() == 1 {
                        self.nested.insert(doc.to_owned());
                    }
                    self.place(doc, item, false);
                    self.any(doc, &format!("{doc}/{item}"));
                    return;
                }
                // A nested item's order key.
                (MergeKind::Collection, [Some(item), ..]) => {
                    self.place(doc, item, false);
                    return;
                }
                _ => {
                    self.field(doc, &resource, field);
                    return;
                }
            }
        }
    }

    fn whole(&mut self, doc: &str) {
        self.whole.insert(doc.to_owned());
        self.docs.insert(doc.to_owned(), Vec::new());
    }

    fn field(&mut self, doc: &str, resource: &str, field: &str) {
        if self.whole.contains(doc) {
            return;
        }
        self.docs.entry(doc.to_owned()).or_default().push(FieldKey {
            resource: resource.to_owned(),
            field: field.to_owned(),
        });
    }

    fn any(&mut self, doc: &str, resource: &str) {
        self.field(doc, resource, ANY_FIELD);
    }

    /// The placement of the item `item` of `doc` was written; `made` when this made the item.
    fn place(&mut self, doc: &str, item: &str, made: bool) {
        let held = self
            .placed
            .entry(doc.to_owned())
            .or_default()
            .entry(item.to_owned())
            .or_default();
        *held = *held || made;
    }

    fn finish(self) -> Changed {
        let mut docs = self.docs;
        for fields in docs.values_mut() {
            fields.sort();
            fields.dedup();
        }
        Changed {
            docs,
            structure: self.structure,
            unplaced: self.unplaced,
            layout: self.layout,
            placed: self.placed,
            nested: self.nested,
        }
    }
}

/// Whether a resource of type `schema` can hold a text at all: a text field of its own, or one
/// in a record or a collection's items it holds. Naming a resource skips a collection whose
/// items cannot, rather than open each of its items to find none.
#[must_use]
pub fn may_hold_text(schema: &TypeSchema) -> bool {
    fn holds(schema: &TypeSchema, seen: &mut Vec<String>) -> bool {
        if seen.contains(&schema.id) {
            return false;
        }
        seen.push(schema.id.clone());
        schema.fields.iter().any(|field| match field.kind {
            MergeKind::Text | MergeKind::RichText => true,
            MergeKind::Record | MergeKind::Collection => field
                .record
                .as_deref()
                .and_then(lookup)
                .is_some_and(|inner| holds(inner, seen)),
            _ => false,
        })
    }
    holds(schema, &mut Vec::new())
}

/// The items of a record, found and resolved into their collections: what
/// [`first_text`] reads a collection's items from.
pub struct RecordItems<'a> {
    pub doc: &'a str,
    pub items: &'a crate::items::Items,
    pub resolved: &'a tilolang_runtime::place::Resolved,
}

/// The first non-empty text of the resource map `obj` of type `schema` at `path`, and the path
/// of the resource that holds it, in the order [`crate::DynRecord`]'s text collection visits:
/// fields by id, a collection's items in order, nested records in place. It stops at the first,
/// so naming a resource reads the few fields before its first text, not the whole resource.
#[must_use]
pub fn first_text(
    doc: &Doc,
    obj: &ObjId,
    schema: &TypeSchema,
    (path, items): (&str, &RecordItems<'_>),
) -> Option<(String, String)> {
    let mut fields: Vec<&FieldSchema> = schema.fields.iter().collect();
    fields.sort_by(|a, b| a.id.cmp(&b.id));
    for field in fields {
        let id = field.id.as_str();
        match field.kind {
            MergeKind::Text => {
                let text: String = read_key(doc, obj, id).ok()?;
                if !text.is_empty() {
                    return Some((text, path.to_owned()));
                }
            }
            MergeKind::Record => {
                let nested = field.record.as_deref().and_then(lookup);
                if let (Some(map), Some(nested)) = (map_at(doc, obj, id), nested)
                    && let Some(found) = first_text(doc, &map, nested, (path, items))
                {
                    return Some(found);
                }
            }
            MergeKind::Collection => {
                if field
                    .record
                    .as_deref()
                    .and_then(lookup)
                    .is_some_and(|item| !may_hold_text(item))
                {
                    continue;
                }
                let holder = path
                    .strip_prefix(items.doc)
                    .and_then(|rest| rest.strip_prefix('/'))
                    .map(str::to_owned);
                for item in items.resolved.items(holder.as_deref(), id) {
                    let Some(found) = items.items.found.get(&item) else {
                        continue;
                    };
                    let (Some(value), Some(item_schema)) =
                        (&found.obj, found.ty.as_deref().and_then(lookup))
                    else {
                        continue;
                    };
                    let at = format!("{}/{item}", items.doc);
                    if let Some(found) = first_text(doc, value, item_schema, (&at, items)) {
                        return Some(found);
                    }
                }
            }
            _ => {}
        }
    }
    None
}
