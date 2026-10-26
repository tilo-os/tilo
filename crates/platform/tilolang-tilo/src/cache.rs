//! What the store has read from its mirror, kept until a change touches it.
//!
//! Reading one field of one resource out of the mirror opens Automerge objects: the record's
//! maps down to the resource, each collection entry on the way, then the resource's map. Each
//! opening is a seek in the document's op columns, about 3 µs at 10k items, so a view that
//! filters or counts 10k items paid 20 µs per item for the walk and the field. The store keeps
//! what it read instead:
//!
//! * where each resource is (its map), so a read walks no further than the resource;
//! * each field value read, so the second pass of a filter (a list and its filter counts, a
//!   card's count and its rows) reads nothing;
//! * a collection read with the scalar fields (registers and counters) of its items, every
//!   entry and item map opened once, on several threads for a large collection: a view that
//!   lists a collection mostly filters, counts or sorts it by those.
//!
//! A change forgets exactly what it touched (the store's own writes by the field they wrote,
//! the record's changes by the [`FieldKey`]s [`crate::reads::changed_by`] names), and a change
//! the store cannot place forgets everything. What is forgotten is read again when next asked.

use std::collections::HashMap;

/// A map keyed by resource ids or field names, read per field a view reads: hashed fast.
type Map<K, V> = HashMap<K, V, foldhash::fast::RandomState>;

use automerge::{ObjId, ObjType, ReadDoc, Value as AmValue};
use tilo_resource::collection::{
    Stored, StoredEntry, collection_entries, in_stored_order, read_entry,
};
use tilo_resource::model::MapSlot;
use tilolang_runtime::ids::{AppId, DocId, TypeId};
use tilolang_runtime::store::{ANY_FIELD, FieldKey, FieldValue, StoreError};
use tilolang_runtime::value::ResourceRef;

use crate::reads::{default_value, field_of_slot, resource_obj};
use crate::schema::{FieldSchema, MergeKind, TypeSchema, lookup};

type Doc = tilo_resource::Doc;

/// Collections of at least this many items are read on several threads: below it, starting
/// the threads costs about what they save.
const PARALLEL_FROM: usize = 1024;

/// The most threads one collection read takes.
const MAX_THREADS: usize = 8;

/// What the store read of every document.
#[derive(Default)]
pub struct ReadCache {
    docs: Map<DocId, DocCache>,
}

/// What the store read of one document: per resource (the document's root, or an item, keyed
/// by [`ResourceRef::id_string`]).
#[derive(Default)]
struct DocCache {
    resources: Map<String, Cached>,
}

/// One resource: where it is and the fields read so far, keyed by the field ids its schema
/// holds.
struct Cached {
    /// The resource's map; `None` for an item stored as its order key alone.
    obj: Option<ObjId>,
    schema: &'static TypeSchema,
    fields: Map<&'static str, FieldValue>,
}

/// An item of a collection as one thread of a collection read found it.
struct Listed {
    stored: Stored,
    obj: Option<ObjId>,
    fields: Vec<(&'static str, FieldValue)>,
}

fn not_found(what: impl Into<String>) -> StoreError {
    StoreError::NotFound(what.into())
}

/// The map a slot holds, if it holds one.
fn map_of(slot: &MapSlot) -> Option<ObjId> {
    match slot {
        Some((AmValue::Object(ObjType::Map), obj)) => Some(obj.clone()),
        _ => None,
    }
}

impl ReadCache {
    /// The value of `field` of the resource `at` in `doc`, whose document is of the app and
    /// type `(app, ty)`.
    ///
    /// # Errors
    /// `NotFound` for a resource or field that is not there; `Backend` for a stored value of
    /// the wrong shape.
    pub fn read(
        &mut self,
        doc: &Doc,
        (app, ty): (&AppId, &TypeId),
        at: &ResourceRef,
        field: &str,
    ) -> Result<FieldValue, StoreError> {
        let key = at.id_string();
        if let Some(value) = self
            .docs
            .get(&at.doc)
            .and_then(|d| d.resources.get(&key))
            .and_then(|r| r.fields.get(field))
        {
            return Ok(value.clone());
        }
        let (obj, schema) = self.locate(doc, (app, ty), at, &key)?;
        let spec = schema
            .field(field)
            .ok_or_else(|| not_found(format!("field {field}")))?;
        let value = match obj.as_ref() {
            None => default_value(spec),
            Some(obj) if spec.kind == MergeKind::Collection => self.list(doc, obj, &key, spec)?,
            Some(obj) => {
                let slot = doc
                    .get(obj, spec.id.as_str())
                    .map_err(|e| StoreError::Backend(e.to_string()))?
                    .map(|(value, id)| (value.into_owned(), id));
                field_of_slot(doc, slot, spec)?
            }
        };
        if let Some(cached) = self
            .docs
            .get_mut(&at.doc)
            .and_then(|d| d.resources.get_mut(&key))
        {
            cached.fields.insert(spec.id.as_str(), value.clone());
        }
        Ok(value)
    }

    /// Where the resource `at` (written `key`) is, and its schema: from what was read before,
    /// else walked to from the nearest resource above it that was.
    ///
    /// # Errors
    /// `NotFound` when a step names no collection or no item.
    pub fn locate(
        &mut self,
        doc: &Doc,
        (app, ty): (&AppId, &TypeId),
        at: &ResourceRef,
        key: &str,
    ) -> Result<(Option<ObjId>, &'static TypeSchema), StoreError> {
        let cache = self.docs.entry(at.doc.clone()).or_default();
        if let Some(found) = cache.resources.get(key) {
            return Ok((found.obj.clone(), found.schema));
        }
        let Some((last, above)) = at.path.split_last() else {
            let schema = lookup(ty).ok_or_else(|| not_found(format!("type {ty}")))?;
            let obj = resource_obj(doc, app, &at.doc)
                .ok_or_else(|| not_found(format!("resource {}", at.doc)))?;
            cache.resources.insert(
                key.to_owned(),
                Cached {
                    obj: Some(obj.clone()),
                    schema,
                    fields: Map::default(),
                },
            );
            return Ok((Some(obj), schema));
        };
        let parent = ResourceRef {
            doc: at.doc.clone(),
            path: above.to_vec(),
            ty: String::new(),
        };
        let (parent_obj, parent_schema) =
            self.locate(doc, (app, ty), &parent, &parent.id_string())?;
        let spec = parent_schema
            .field(&last.field)
            .filter(|f| f.kind == MergeKind::Collection)
            .ok_or_else(|| not_found(format!("collection `{}`", last.field)))?;
        let item_ty = spec
            .record
            .as_deref()
            .ok_or_else(|| not_found(format!("item type of `{}`", last.field)))?;
        let schema = lookup(item_ty).ok_or_else(|| not_found(format!("type {item_ty}")))?;
        let coll = parent_obj
            .as_ref()
            .and_then(|o| doc.get(o, last.field.as_str()).ok().flatten())
            .and_then(|(value, id)| map_of(&Some((value.into_owned(), id))))
            .ok_or_else(|| not_found(format!("item `{}`", last.item)))?;
        let obj = tilo_resource::stored_item(doc, &coll, &last.item)
            .ok_or_else(|| not_found(format!("item `{}`", last.item)))?;
        self.docs
            .entry(at.doc.clone())
            .or_default()
            .resources
            .insert(
                key.to_owned(),
                Cached {
                    obj: obj.clone(),
                    schema,
                    fields: Map::default(),
                },
            );
        Ok((obj, schema))
    }

    /// The items of the collection `field` of the resource map `obj` (written `key`), in
    /// order; each item that was not read before is kept with its scalar fields.
    fn list(
        &mut self,
        doc: &Doc,
        obj: &ObjId,
        key: &str,
        field: &FieldSchema,
    ) -> Result<FieldValue, StoreError> {
        let coll = doc
            .get(obj, field.id.as_str())
            .map_err(|e| StoreError::Backend(e.to_string()))?
            .and_then(|(value, id)| map_of(&Some((value.into_owned(), id))));
        let Some(coll) = coll else {
            return Ok(FieldValue::Items(Vec::new()));
        };
        let item_schema = field.record.as_deref().and_then(lookup);
        let doc_id = tilolang_runtime::store::doc_of(key).to_owned();
        let cache = self.docs.entry(doc_id).or_default();
        let prefix = format!("{key}/{}:", field.id);
        let entries: Vec<(StoredEntry, bool)> = collection_entries(doc, &coll)
            .into_iter()
            .map(|entry| {
                // An item kept already needs only its entry read; the others their scalars too.
                let kept = cache
                    .resources
                    .contains_key(&format!("{prefix}{}", entry.id().as_str()));
                (entry, !kept)
            })
            .collect();
        let listed = read_listing(doc, entries, item_schema);
        let mut stored: Vec<Stored> = Vec::with_capacity(listed.len());
        for item in listed {
            let id = format!("{prefix}{}", item.stored.id.as_str());
            if let Some(schema) = item_schema {
                let cached = cache.resources.entry(id).or_insert_with(|| Cached {
                    obj: item.obj.clone(),
                    schema,
                    fields: Map::default(),
                });
                for (name, value) in item.fields {
                    cached.fields.entry(name).or_insert(value);
                }
            }
            stored.push(item.stored);
        }
        in_stored_order(&mut stored);
        Ok(FieldValue::Items(
            stored
                .into_iter()
                .map(|s| s.id.as_str().to_owned())
                .collect(),
        ))
    }

    /// The first non-empty text of the resource `at`, and the path of the resource holding
    /// it, as [`crate::reads::first_text`] finds it, through what was read before: naming an
    /// item of a large collection lists the collection once, not on every change elsewhere.
    pub fn first_text(
        &mut self,
        doc: &Doc,
        (app, ty): (&AppId, &TypeId),
        at: &ResourceRef,
    ) -> Option<(String, String)> {
        let key = at.id_string();
        let (obj, schema) = self.locate(doc, (app, ty), at, &key).ok()?;
        let obj = obj?;
        let mut fields: Vec<&'static FieldSchema> = schema.fields.iter().collect();
        fields.sort_by(|a, b| a.id.cmp(&b.id));
        for field in fields {
            match field.kind {
                MergeKind::Text => {
                    if let Ok(FieldValue::Text(text)) = self.read(doc, (app, ty), at, &field.id)
                        && !text.is_empty()
                    {
                        return Some((text, key));
                    }
                }
                MergeKind::Record => {
                    let nested = field.record.as_deref().and_then(lookup);
                    if let (Some(map), Some(nested)) =
                        (crate::reads::map_at(doc, &obj, &field.id), nested)
                        && let Some(found) = crate::reads::first_text(doc, &map, nested, &key)
                    {
                        return Some(found);
                    }
                }
                MergeKind::Collection => {
                    let Some(item_ty) = field
                        .record
                        .as_deref()
                        .filter(|item| lookup(item).is_some_and(crate::reads::may_hold_text))
                    else {
                        continue;
                    };
                    let Ok(FieldValue::Items(ids)) = self.read(doc, (app, ty), at, &field.id)
                    else {
                        continue;
                    };
                    for id in ids {
                        let item = at.item(&field.id, &id, item_ty);
                        if let Some(found) = self.first_text(doc, (app, ty), &item) {
                            return Some(found);
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Forget what a change to `key` may have changed: the field, or with [`ANY_FIELD`] the
    /// resource and everything inside it.
    pub fn forget(&mut self, key: &FieldKey) {
        let Some(cache) = self.docs.get_mut(key.doc()) else {
            return;
        };
        if key.field == ANY_FIELD {
            let inside = format!("{}/", key.resource);
            cache
                .resources
                .retain(|r, _| r != &key.resource && !r.starts_with(&inside));
        } else if let Some(cached) = cache.resources.get_mut(&key.resource) {
            cached.fields.remove(key.field.as_str());
        }
    }

    /// Forget everything read of the document `doc`.
    pub fn forget_doc(&mut self, doc: &str) {
        self.docs.remove(doc);
    }

    /// Forget everything.
    pub fn clear(&mut self) {
        self.docs.clear();
    }
}

/// Read the entries of a collection, and each item's scalar fields: on several threads when
/// there are many.
fn read_listing(
    doc: &Doc,
    entries: Vec<(StoredEntry, bool)>,
    item: Option<&'static TypeSchema>,
) -> Vec<Listed> {
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(MAX_THREADS);
    if entries.len() < PARALLEL_FROM || threads < 2 {
        return read_items(doc, entries, item);
    }
    // Chunks a few times smaller than a thread's share, taken from one queue by the threads
    // and by this one: a thread the system does not give, or a slow one, costs no items.
    let per = entries.len().div_ceil(threads.saturating_mul(4));
    let mut chunks: Vec<Vec<(StoredEntry, bool)>> = Vec::new();
    let mut rest = entries.into_iter();
    loop {
        let chunk: Vec<_> = rest.by_ref().take(per).collect();
        if chunk.is_empty() {
            break;
        }
        chunks.push(chunk);
    }
    let queue = std::sync::Mutex::new(chunks);
    let run = || {
        let mut out = Vec::new();
        loop {
            let chunk = queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop();
            let Some(chunk) = chunk else {
                break out;
            };
            out.extend(read_items(doc, chunk, item));
        }
    };
    std::thread::scope(|scope| {
        let workers: Vec<_> = (1..threads)
            .filter_map(|_| {
                std::thread::Builder::new()
                    .name("tilolang-list".to_owned())
                    .spawn_scoped(scope, run)
                    .ok()
            })
            .collect();
        // The order does not matter: the caller sorts the items into stored order.
        let mut out = run();
        for worker in workers {
            match worker.join() {
                Ok(items) => out.extend(items),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
        out
    })
}

/// Read entries, and the scalar fields of the items marked to be read, in order.
fn read_items(
    doc: &Doc,
    entries: Vec<(StoredEntry, bool)>,
    item: Option<&'static TypeSchema>,
) -> Vec<Listed> {
    let wanted: Vec<&'static FieldSchema> = item.map_or_else(Vec::new, |schema| {
        schema
            .fields
            .iter()
            .filter(|f| matches!(f.kind, MergeKind::Register | MergeKind::Counter))
            .collect()
    });
    entries
        .into_iter()
        .filter_map(|(entry, read)| read_entry(doc, entry).map(|stored| (stored, read)))
        .map(|(stored, read)| {
            let obj = if stored.entry.is_some() {
                map_of(&stored.value)
            } else {
                None
            };
            let fields = match obj.as_ref() {
                _ if !read => Vec::new(),
                Some(obj) => scalars(doc, obj, &wanted),
                None if stored.entry.is_none() => wanted
                    .iter()
                    .map(|f| (f.id.as_str(), default_value(f)))
                    .collect(),
                None => Vec::new(),
            };
            Listed {
                stored,
                obj,
                fields,
            }
        })
        .collect()
}

/// The fields `wanted` (registers and counters) of the resource map `obj`, read in one walk of
/// the map.
fn scalars(
    doc: &Doc,
    obj: &ObjId,
    wanted: &[&'static FieldSchema],
) -> Vec<(&'static str, FieldValue)> {
    if wanted.is_empty() {
        return Vec::new();
    }
    let mut slots: Vec<MapSlot> = vec![None; wanted.len()];
    for entry in doc.map_range(obj, ..) {
        if let Some(slot) = wanted
            .iter()
            .position(|f| f.id == entry.key)
            .and_then(|at| slots.get_mut(at))
        {
            *slot = Some((entry.value.clone().into_value(), entry.id()));
        }
    }
    wanted
        .iter()
        .zip(slots)
        .filter_map(|(field, slot)| {
            // A value of the wrong shape is left to the read that wants it, which reports it.
            field_of_slot(doc, slot, field)
                .ok()
                .map(|value| (field.id.as_str(), value))
        })
        .collect()
}
