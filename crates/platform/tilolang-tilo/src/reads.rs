//! Reading one field of one resource straight from the room record, and naming the fields a
//! set of Automerge changes wrote.
//!
//! A read walks from the record's root to the resource's map (`tl/<app>/<id>`), down one
//! collection entry per step of the reference, and reads the one field: its cost does not grow
//! with the size of the resource. Materializing the resource as a [`crate::DynRecord`] instead
//! would read every item of every collection in it for each field a view reads.

use std::collections::{BTreeMap, BTreeSet};

use automerge::legacy::Key as LegacyKey;
use automerge::{Change, ObjId, ObjType, Prop, ReadDoc, ScalarValue, Value as AmValue};
use serde_json::Value as Json;
use tilo_resource::{Counter, Map, Set, read_key, stored_ids, stored_item};
use tilolang_runtime::ids::{AppId, DocId, TypeId};
use tilolang_runtime::store::{ANY_FIELD, FieldKey, FieldValue, Scalar, StoreError};
use tilolang_runtime::value::{ResourceRef, Step};

use crate::action::RECORDS_KEY;
use crate::schema::{FieldSchema, MergeKind, TYPE_KEY, TypeSchema, lookup};

type Doc = tilo_resource::Doc;

fn not_found(what: impl Into<String>) -> StoreError {
    StoreError::NotFound(what.into())
}

fn backend(error: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(error.to_string())
}

fn map_at(doc: &Doc, obj: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(obj, key) {
        Ok(Some((AmValue::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}

/// The map of the resource `id` of `app`.
pub fn resource_obj(doc: &Doc, app: &str, id: &str) -> Option<ObjId> {
    let records = map_at(doc, &automerge::ROOT, RECORDS_KEY)?;
    let app = map_at(doc, &records, app)?;
    map_at(doc, &app, id)
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
/// (every field reads as its default); and its type.
pub struct Located {
    pub obj: Option<ObjId>,
    pub schema: &'static TypeSchema,
}

/// Walk from the resource map `root` (of type `root_ty`) down the steps of `at`.
///
/// # Errors
/// `NotFound` when a step names no collection or no item.
pub fn locate(
    doc: &Doc,
    root: ObjId,
    root_ty: &str,
    steps: &[Step],
) -> Result<Located, StoreError> {
    let mut schema = lookup(root_ty).ok_or_else(|| not_found(format!("type {root_ty}")))?;
    let mut obj = Some(root);
    for step in steps {
        let field = schema
            .field(&step.field)
            .filter(|f| f.kind == MergeKind::Collection)
            .ok_or_else(|| not_found(format!("collection `{}`", step.field)))?;
        let item_ty = field
            .record
            .as_deref()
            .ok_or_else(|| not_found(format!("item type of `{}`", step.field)))?;
        schema = lookup(item_ty).ok_or_else(|| not_found(format!("type {item_ty}")))?;
        let coll = obj
            .as_ref()
            .and_then(|o| map_at(doc, o, &step.field))
            .ok_or_else(|| not_found(format!("item `{}`", step.item)))?;
        obj = stored_item(doc, &coll, &step.item)
            .ok_or_else(|| not_found(format!("item `{}`", step.item)))?;
    }
    Ok(Located { obj, schema })
}

/// Locate the resource `at`, given the app and type of its document.
///
/// # Errors
/// As [`locate`]; `NotFound` for a document the record does not hold.
pub fn locate_ref(
    doc: &Doc,
    (app, ty): (&AppId, &TypeId),
    at: &ResourceRef,
) -> Result<Located, StoreError> {
    let root =
        resource_obj(doc, app, &at.doc).ok_or_else(|| not_found(format!("resource {}", at.doc)))?;
    locate(doc, root, ty, &at.path)
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
    let id = field.id.as_str();
    let Some(obj) = located.obj.as_ref() else {
        return Ok(default_value(field));
    };
    Ok(match field.kind {
        MergeKind::Register => {
            let value: Json = read_key(doc, obj, id).map_err(backend)?;
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
            FieldValue::Counter(read_key::<Counter>(doc, obj, id).map_err(backend)?.get())
        }
        MergeKind::Text => FieldValue::Text(read_key::<String>(doc, obj, id).map_err(backend)?),
        MergeKind::Set => {
            let set: Set<Json> = read_key(doc, obj, id).map_err(backend)?;
            let mut keys: Vec<String> = set.iter().map(element_key).collect();
            keys.sort();
            FieldValue::Keys(keys)
        }
        MergeKind::Map => {
            let map: Map<Json> = read_key(doc, obj, id).map_err(backend)?;
            FieldValue::Entries(
                map.iter()
                    .map(|(key, value)| (key.clone(), scalar_of_json(value, false)))
                    .collect(),
            )
        }
        MergeKind::Collection => FieldValue::Items(
            map_at(doc, obj, id)
                .map(|coll| {
                    stored_ids(doc, &coll)
                        .into_iter()
                        .map(|item| item.as_str().to_owned())
                        .collect()
                })
                .unwrap_or_default(),
        ),
        MergeKind::Record => {
            return Err(StoreError::Invalid(
                "a nested record is not a field the runtime reads".into(),
            ));
        }
    })
}

fn default_value(field: &FieldSchema) -> FieldValue {
    match field.kind {
        MergeKind::Register if field.default.is_null() => FieldValue::Absent,
        MergeKind::Register => FieldValue::Scalar(scalar_of_json(&field.default, field.composite)),
        MergeKind::Counter => FieldValue::Counter(0),
        MergeKind::Text => FieldValue::Text(String::new()),
        MergeKind::Set => FieldValue::Keys(Vec::new()),
        MergeKind::Map => FieldValue::Entries(Vec::new()),
        MergeKind::Collection | MergeKind::Record => FieldValue::Items(Vec::new()),
    }
}

/// How many items the collection `field` of the located resource holds.
#[must_use]
pub fn collection_len(doc: &Doc, located: &Located, field: &str) -> usize {
    located
        .obj
        .as_ref()
        .and_then(|obj| map_at(doc, obj, field))
        .map_or(0, |coll| stored_ids(doc, &coll).len())
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
}

/// Name the (resource, field) pairs `changes` wrote, once `doc` holds them. `index` gives each
/// document's app and type, so the walk knows which fields are collections whose items are
/// resources of their own.
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
    let mut writes: BTreeMap<String, BTreeSet<Option<String>>> = BTreeMap::new();
    for change in changes {
        for op in change.decode().operations {
            let key = match op.key {
                LegacyKey::Map(key) => Some(key.to_string()),
                LegacyKey::Seq(_) => None,
            };
            writes.entry(op.obj.to_string()).or_default().insert(key);
        }
    }
    let mut out = ChangedBuilder::default();
    for (obj, keys) in writes {
        let Some(path) = path_of(doc, &obj) else {
            out.unplaced = true;
            continue;
        };
        for key in keys {
            let mut props: Vec<&Prop> = path.iter().collect();
            let own = key.map(Prop::Map);
            if let Some(own) = own.as_ref() {
                props.push(own);
            }
            out.patch(&props, index);
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
}

fn key_of(prop: &Prop) -> Option<&str> {
    match prop {
        Prop::Map(key) => Some(key.as_str()),
        Prop::Seq(_) => None,
    }
}

impl ChangedBuilder {
    fn patch(&mut self, props: &[&Prop], index: &BTreeMap<DocId, (AppId, TypeId)>) {
        // tl / <app> / <id> / <field> ...
        let keys: Vec<Option<&str>> = props.iter().map(|p| key_of(p)).collect();
        if keys.first().copied().flatten() != Some(RECORDS_KEY) {
            return;
        }
        let Some(Some(doc)) = keys.get(2).copied() else {
            self.structure = true;
            return;
        };
        if keys.len() <= 3 {
            self.structure = true;
            self.whole(doc);
            return;
        }
        let Some((_, ty)) = index.get(doc) else {
            // A resource the index does not know yet: it was just created.
            self.structure = true;
            self.whole(doc);
            return;
        };
        let Some(mut schema) = lookup(ty) else {
            self.whole(doc);
            return;
        };
        let mut resource = doc.to_owned();
        let mut at = 3;
        loop {
            let Some(Some(field)) = keys.get(at).copied() else {
                self.any(doc, &resource);
                return;
            };
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
            let below = &keys[at + 1..];
            let item_record = spec.record.as_deref().and_then(lookup);
            match (spec.kind, below, item_record) {
                // An item's value changed below its `v`: descend into the item.
                (MergeKind::Collection, [Some(item), Some("v"), _, ..], Some(item_schema)) => {
                    resource = format!("{resource}/{field}:{item}");
                    schema = item_schema;
                    at += 3;
                }
                // The item itself was put or replaced: the collection and every field of the item.
                (MergeKind::Collection, [Some(item), Some("v")], _) => {
                    self.field(doc, &resource, field);
                    self.any(doc, &format!("{resource}/{field}:{item}"));
                    return;
                }
                // Membership or order.
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
        }
    }
}
