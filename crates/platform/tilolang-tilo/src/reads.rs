//! Reading one field of one resource straight from the workspace record, and naming the fields a
//! set of Automerge changes wrote.
//!
//! A read walks from the record's root to the resource's map (`tl/<app>/<id>`), to the item's
//! map by its id ([`crate::items`]), and reads the one field: its cost does not grow with the
//! size of the resource. Materializing the resource as a [`crate::DynRecord`] instead
//! would read every item of every collection in it for each field a view reads.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;

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
/// before (`previously:`), where its older records stay. Every copy of the levels above it is
/// read ([`crate::containers::reach`]): a record made before two replicas met may be in the map
/// the document does not keep, and it is as much the app's record as any.
pub fn resource_obj(doc: &Doc, app: &str, id: &str) -> Option<ObjId> {
    crate::schema::stored_under(app).iter().find_map(|stored| {
        crate::containers::reach(doc, &automerge::ROOT, &[RECORDS_KEY, stored])
            .iter()
            .rev()
            .find_map(|held| map_at(doc, held, id))
    })
}

/// The ids of the resources the document `doc` holds: the keys below the levels above the
/// records, under every copy of them a replica made. A chunk of a record's items holds the items
/// of a few resources of the app, never all of them, so a step of a build reads the items of the
/// ones a chunk names rather than look each resource of the store up in each chunk.
#[must_use]
pub fn held_ids(doc: &Doc) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for records in crate::containers::reach(doc, &automerge::ROOT, &[RECORDS_KEY]) {
        for stored in doc.keys(&records) {
            for app in crate::containers::maps(doc, &records, &stored) {
                out.extend(doc.keys(&app));
            }
        }
    }
    out
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
    /// The chunk its map is read in: the item's, or the resource's own.
    pub chunk: crate::chunk::ChunkNo,
}

/// Locate the resource `at`, given the app and type of its document: the record's map, or the
/// map of its item `at.item` wherever that is stored ([`crate::items`]). Whether the item is
/// in a collection now is the index's to say.
///
/// # Errors
/// `NotFound` for a document the record does not hold, or an item it holds none of.
pub fn locate_ref(
    doc: &Doc,
    chunks: Option<&crate::chunk::Chunks>,
    (app, ty): (&AppId, &TypeId),
    at: &ResourceRef,
    nested: &crate::items::Nested,
) -> Result<Located, StoreError> {
    let root =
        resource_obj(doc, app, &at.doc).ok_or_else(|| not_found(format!("resource {}", at.doc)))?;
    let schema = lookup(ty).ok_or_else(|| not_found(format!("type {ty}")))?;
    let Some(item) = &at.item else {
        let chunk = chunks.map_or(crate::chunk::FIRST, |chunks| chunks.chunk(&at.doc));
        return Ok(Located {
            obj: Some(root),
            schema,
            item: None,
            chunk,
        });
    };
    // An item is stored in the chunk of the run it is in: found across the store's chunks, so a
    // read of its fields goes to the chunk that holds its map.
    let found = match chunks {
        Some(chunks) => crate::items::find_across(chunks, app, &at.doc, schema, nested, item),
        None => crate::items::find(doc, (&at.doc, &root, schema), nested, item),
    }
    .ok_or_else(|| not_found(format!("item `{item}`")))?;
    let item_schema = found
        .ty
        .as_deref()
        .and_then(lookup)
        .ok_or_else(|| not_found(format!("the type of item `{item}`")))?;
    Ok(Located {
        obj: found.obj.clone(),
        schema: item_schema,
        chunk: found.chunk,
        item: Some(found),
    })
}

pub(crate) fn scalar_of_json(value: &Json, composite: bool) -> Scalar {
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

pub(crate) fn element_key(value: &Json) -> String {
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

/// Whether `field` of the located resource holds anything: a value other than what a record
/// that never wrote it reads. What a migration asks of the fields an earlier version wrote,
/// one field at a time.
#[must_use]
pub fn holds_something(doc: &Doc, located: &Located, field: &FieldSchema) -> bool {
    read_field(doc, located, field).is_ok_and(|value| value != default_value(field))
}

/// Whether the collection `field` of the record whose map is `root` holds an item: an item of a
/// record written before items were flat, nested in the field's map, or one of the record's own
/// items placed in it. Read from the placements, never from the items' values.
#[must_use]
pub fn collection_holds(doc: &Doc, root: &ObjId, field: &FieldSchema) -> bool {
    if map_at(doc, root, &field.id).is_some_and(|coll| doc.length(&coll) > 0) {
        return true;
    }
    crate::containers::maps(doc, root, tilolang_runtime::place::ITEMS_KEY)
        .iter()
        .any(|items| {
            doc.keys(items).any(|id| {
                let Some(obj) = map_at(doc, items, &id) else {
                    return false;
                };
                tilolang_runtime::place::held_at(doc, &obj).is_some_and(|held| {
                    match held.placement {
                        tilolang_runtime::place::Placement::At { place, .. } => {
                            place.parent.is_none() && place.field.as_str() == field.id
                        }
                        tilolang_runtime::place::Placement::Removed => false,
                    }
                })
            })
        })
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
    /// Per document, the person the record's change names as the one who made it: `None` for a
    /// document several people changed, and for a change that names nobody.
    pub by: By,
}

/// Per document, who made the record's change to it.
pub type By = BTreeMap<DocId, Option<String>>;

/// Per document, the items whose placement changes wrote, each with whether they made it.
pub type Placed = BTreeMap<DocId, BTreeMap<String, bool>>;

impl Changed {
    /// Whether the change wrote anything but where the record places its resources (`$chunks`):
    /// whether it names a resource at all, and so whether its message is read for one.
    ///
    /// A run's tail in the manifest is written once an item of a collection, names no resource,
    /// makes no version of one and places no item the item's own chunk does not
    /// ([`written_by`]), so a workspace of a hundred thousand items holds a hundred thousand
    /// changes this says nothing of.
    #[must_use]
    pub fn wrote(&self) -> bool {
        !self.docs.is_empty()
            || !self.placed.is_empty()
            || !self.nested.is_empty()
            || self.structure
            || self.unplaced
            || self.layout
    }

    /// What `other` changed as well: the changes of another document of the record, read on
    /// that document.
    pub fn take_in(&mut self, other: Self) {
        for (doc, fields) in other.docs {
            match self.docs.get_mut(&doc) {
                // A document as a whole stays whole.
                Some(held) if held.is_empty() => {}
                Some(held) => held.extend(fields),
                None => {
                    self.docs.insert(doc, fields);
                }
            }
        }
        self.structure = self.structure || other.structure;
        self.unplaced = self.unplaced || other.unplaced;
        self.layout = self.layout || other.layout;
        for (doc, items) in other.placed {
            self.placed.entry(doc).or_default().extend(items);
        }
        self.nested.extend(other.nested);
        for (doc, who) in other.by {
            match self.by.get(&doc) {
                // Nobody said yet.
                None => {
                    self.by.insert(doc, who);
                }
                // Known, and this change names nobody, or someone else.
                Some(Some(seen)) if who.as_deref() != Some(seen.as_str()) => {
                    self.by.insert(doc, None);
                }
                // Already unknown, or known and named again.
                Some(None | Some(_)) => {}
            }
        }
    }
}

/// Name the (resource, field) pairs `changes` wrote, once `doc` holds them, and the items they
/// placed. `index` gives each document's app and type, so the walk knows which fields are
/// collections whose items are resources of their own.
///
/// Each op names its object and key; the object's place in the record is found by walking up
/// its parents ([`Places`]). (Automerge's own patches would name the same places, but building
/// one walks every entry of each map above the change, which in a collection of 10,000 items
/// costs milliseconds per keystroke.) Ops overwritten by a concurrent op are named as well: a
/// view re-reading a field that did not change shows the same thing.
#[must_use]
pub fn changed_by(
    changes: &[Change],
    doc: &Doc,
    index: &BTreeMap<DocId, (AppId, TypeId)>,
) -> Changed {
    let mut out = Changed::default();
    let mut places = Places::of(doc);
    for change in changes {
        out.take_in(places.written(change, index));
    }
    out
}

/// The changes of `doc` that wrote a resource of the record, and whose messages are read for
/// one: a change that wrote nothing but where the record places its resources (`$chunks`) names
/// no resource and makes no version of one ([`Places::wrote`]).
#[must_use]
pub fn naming<'c>(changes: &'c [Change], doc: &Doc) -> Vec<&'c Change> {
    let mut places = Places::of(doc);
    changes
        .iter()
        .filter(|change| places.wrote(change))
        .collect()
}

/// Where the objects of a document stand, walked once: a step of a build reads a hundred
/// thousand changes on one document, and they write few of its objects — every item of a
/// collection writes the tail of its run in the manifest ([`Changed::wrote`]) — so each object
/// is walked up to its place once, and every change after it reads that place from here.
///
/// Automerge answers an object's parents by finding the op that made it, which searches the
/// document's op ids: walking up from each op's object cost about a tenth of a millisecond per
/// change, and a hundred thousand of them half a minute.
pub struct Places<'d> {
    doc: &'d Doc,
    /// The place from the record's root of each object walked, by the id Automerge prints for
    /// it; `None` for one the document does not hold.
    at: HashMap<String, Option<Rc<[Prop]>>>,
}

impl<'d> Places<'d> {
    /// Where the objects of `doc` stand, none walked yet.
    #[must_use]
    pub fn of(doc: &'d Doc) -> Self {
        Self {
            doc,
            at: HashMap::new(),
        }
    }

    /// What `change` wrote of the record, and whether it wrote more than where the record
    /// places its resources (`$chunks`) at all ([`Changed::wrote`]).
    ///
    /// A run's tail in the manifest is written once an item of a collection, so a workspace of a
    /// hundred thousand items has a hundred thousand changes that name no resource and make no
    /// version of one: their ops all point into the manifest, the walk of each op says so, and
    /// their messages are never read.
    pub fn written(
        &mut self,
        change: &Change,
        index: &BTreeMap<DocId, (AppId, TypeId)>,
    ) -> Changed {
        let doc = self.doc;
        let expanded = change.decode();
        let mut out = ChangedBuilder::default();
        // The objects the change wrote, each where it stands in the record and the keys written
        // of it: a change on a workspace of a hundred thousand items writes one key of one
        // object, and the record's own document holds a hundred thousand such changes.
        let mut writes: Vec<Writes<'_>> = Vec::new();
        // Where each object the change wrote stands, once walked, by its id: most changes write
        // one object, but the change that adds ten thousand items writes ten thousand, and a
        // search of the ones seen for each op would read them once an op.
        let mut known: HashMap<&automerge::legacy::ObjectId, Option<usize>> = HashMap::new();
        for op in &expanded.operations {
            let at = if let Some(at) = known.get(&op.obj) {
                *at
            } else {
                let at = writes.len();
                let walked = match self.path(&op.obj.to_string()) {
                    // Where the record places its resources: written once an item, and no
                    // resource's fields, place or version is read of it.
                    Some(path) if places_only(&path) => None,
                    path => {
                        writes.push(Writes {
                            path,
                            keys: Vec::new(),
                            seen: HashMap::new(),
                        });
                        Some(at)
                    }
                };
                known.insert(&op.obj, walked);
                walked
            };
            let Some(at) = at else {
                continue;
            };
            let Some(wrote) = writes.get_mut(at) else {
                continue;
            };
            let key = match &op.key {
                LegacyKey::Map(key) => Some(key.as_str()),
                LegacyKey::Seq(_) => None,
            };
            let made = matches!(op.action, automerge::legacy::OpType::Make(_));
            wrote.note(key, made);
        }
        if writes.is_empty() {
            return out.finish();
        }
        out.maker = maker_of(change);
        for wrote in writes {
            let Some(path) = wrote.path else {
                out.unplaced = true;
                continue;
            };
            for (key, made) in wrote.keys {
                let mut props: Vec<&Prop> = path.iter().collect();
                let own = key.map(str::to_owned).map(Prop::Map);
                if let Some(own) = own.as_ref() {
                    props.push(own);
                }
                out.patch(&props, made, (doc, index));
            }
        }
        out.finish()
    }

    /// Whether `change` wrote anything but where the record places its resources (`$chunks`),
    /// for a change read only for that: the first object it wrote elsewhere answers, and what
    /// it wrote there is not read. A change that adds ten thousand items is answered by its
    /// first op, not walked to each item's place.
    pub fn wrote(&mut self, change: &Change) -> bool {
        let expanded = change.decode();
        let mut seen: HashSet<&automerge::legacy::ObjectId> = HashSet::new();
        for op in &expanded.operations {
            if !seen.insert(&op.obj) {
                continue;
            }
            match self.path(&op.obj.to_string()) {
                Some(path) if places_only(&path) => {}
                // Written elsewhere in the record, or where no place could be walked.
                _ => return true,
            }
        }
        false
    }

    /// The keys from the record's root down to the object `obj` (as Automerge prints its id),
    /// walking no further than the first object this already walked.
    fn path(&mut self, obj: &str) -> Option<Rc<[Prop]>> {
        if let Some(held) = self.at.get(obj) {
            return held.clone();
        }
        let place = self.walk(obj);
        self.at.insert(obj.to_owned(), place.clone());
        place
    }

    fn walk(&mut self, obj: &str) -> Option<Rc<[Prop]>> {
        let mut here = self.doc.import_obj(obj).ok()?;
        if here == automerge::ROOT {
            return Some(Rc::from(Vec::new() as Vec<Prop>));
        }
        // The keys from the object up to the first one already walked (or to the root), the
        // object each led to, and the place of that one.
        let mut above: Vec<Prop> = Vec::new();
        let mut led: Vec<String> = Vec::new();
        let base = loop {
            let parent = self
                .doc
                .parents(&here)
                .ok()
                .and_then(|mut parents| parents.next())?;
            above.push(parent.prop);
            here = parent.obj;
            if here == automerge::ROOT {
                break Some(Rc::from(Vec::new() as Vec<Prop>));
            }
            let id = here.to_string();
            if let Some(held) = self.at.get(&id) {
                break held.clone();
            }
            led.push(id);
        };
        let Some(base) = base else {
            // An object whose parents lead nowhere is not in the tree: nothing stands above it.
            return None;
        };
        // Each key on the way up stands in the one above it: the object `led[i]` is the
        // `i`th this walked up to, and its place is the base and the keys from it on.
        for (at, id) in led.iter().enumerate() {
            let mut path: Vec<Prop> = base.iter().cloned().collect();
            path.extend(above.iter().skip(at.saturating_add(1)).rev().cloned());
            self.at.insert(id.clone(), Some(Rc::from(path)));
        }
        let mut path: Vec<Prop> = base.iter().cloned().collect();
        path.extend(above.iter().rev().cloned());
        Some(Rc::from(path))
    }
}

/// The keys a change wrote of one object of the record, and whether it made an object under
/// each: a key written twice (made, then a value put in it) is read once, as made.
struct Writes<'a> {
    /// Where the object stands in the record; `None` when its place could not be walked.
    path: Option<Rc<[Prop]>>,
    keys: Vec<(Option<&'a str>, bool)>,
    /// Where each key stands in `keys`: a map the change put ten thousand keys in is read a
    /// key at a time, not searched once a key.
    seen: HashMap<Option<&'a str>, usize>,
}

impl<'a> Writes<'a> {
    /// The key `key` written, with an object made under it when `made`.
    fn note(&mut self, key: Option<&'a str>, made: bool) {
        if let Some((_, held)) = self.seen.get(&key).and_then(|at| self.keys.get_mut(*at)) {
            *held = *held || made;
        } else {
            self.seen.insert(key, self.keys.len());
            self.keys.push((key, made));
        }
    }
}

/// Whether an object stands where the record places its resources (`$chunks`): a write there
/// names no resource, and makes no version of one.
fn places_only(path: &[Prop]) -> bool {
    path.first()
        .is_some_and(|prop| key_of(prop) == Some(crate::chunk::CHUNKS_KEY))
}

/// The person the change `change` names as the one who made it, when its message names one.
fn maker_of(change: &Change) -> Option<String> {
    let context = tilo_actions::ActionContext::from_change_message(change.message()?)?;
    context.invoker.map(|invoker| invoker.actor)
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
    /// Who the change being read names as its maker, when it names one.
    maker: Option<String>,
    by: By,
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
        let above = keys.first().copied().flatten();
        if above == Some(crate::chunk::CHUNKS_KEY) {
            // Where the record places its resources: written once an item of a collection, and
            // no resource's fields, place or version is read of it.
            return;
        }
        if above != Some(RECORDS_KEY) {
            return;
        }
        let Some(Some(doc)) = keys.get(2).copied() else {
            // `tl` and `tl/<app>`, the maps above the resources: every chunk makes them with
            // its first write. Making one creates no resource and moves none, so nothing read of
            // any resource changed; removing one takes away every resource it held, and so does
            // a write that folds the copies a replica made into the one the document keeps
            // ([`crate::containers`]), which empties the copies it leaves.
            if !made {
                self.structure = true;
                self.layout = true;
            }
            return;
        };
        if keys.len() <= 3 {
            // A resource's own map, made when it is created or moved into this chunk. What it
            // holds is written below it in this same change, and named there; a resource the
            // index does not know yet is read again whole.
            if index.get(doc).is_none() {
                self.structure = true;
                self.whole(doc);
                return;
            }
            if !made {
                // Its map was removed: the resource is gone, and with it everything it held.
                self.structure = true;
                self.whole(doc);
            }
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
            // The map the record's items sit in, made with the first of them and removed with
            // the last: the items it gains or loses are named by their own writes.
            if !made {
                self.whole(doc);
            }
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
                    .map(|root| crate::containers::maps(record, &root, ITEMS_KEY))
                    .and_then(|items| {
                        items
                            .iter()
                            .rev()
                            .find_map(|items| map_at(record, items, item))
                    })
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
        self.made_by(doc);
        self.whole.insert(doc.to_owned());
        self.docs.insert(doc.to_owned(), Vec::new());
    }

    fn field(&mut self, doc: &str, resource: &str, field: &str) {
        if self.whole.contains(doc) {
            return;
        }
        self.made_by(doc);
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
        self.made_by(doc);
        let held = self
            .placed
            .entry(doc.to_owned())
            .or_default()
            .entry(item.to_owned())
            .or_default();
        *held = *held || made;
    }

    /// Note that the document `doc` changed by the change being read, and who made it: a
    /// document two people's changes reached in one batch names nobody.
    fn made_by(&mut self, doc: &str) {
        let Some(who) = self.maker.as_deref() else {
            // The change names nobody: it says nothing of who made it.
            self.by.entry(doc.to_owned()).or_insert(None);
            return;
        };
        match self.by.get(doc) {
            None => {
                self.by.insert(doc.to_owned(), Some(who.to_owned()));
            }
            Some(Some(seen)) if seen != who => {
                self.by.insert(doc.to_owned(), None);
            }
            _ => {}
        }
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
            by: self.by,
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
/// of the resource that holds it: its own texts name it before its items' do, its first text
/// field first; a nested record in place and a collection's items in order only where the
/// resource holds no text of its own. It stops at the first, so naming a resource reads the few
/// fields before its first text, not the whole resource. A text names it by its first non-blank
/// line.
///
/// A channel called "design" whose messages hold text is "design", not the first of its
/// messages (a milestone's tasks are not its name either); only a record that is nothing but
/// its list is named by its first item.
#[must_use]
pub fn first_text(
    doc: &Doc,
    obj: &ObjId,
    schema: &TypeSchema,
    (path, items): (&str, &RecordItems<'_>),
) -> Option<(String, String)> {
    // Its own texts, in the order the type declares them, then the records and collections it
    // holds ([`TypeSchema::naming_scan`], the one order naming uses).
    let fields = schema.naming_scan();
    if let Some(own) = own_text(doc, obj, &fields, path) {
        return Some(own);
    }
    for field in fields {
        let id = field.id.as_str();
        match field.kind {
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

/// The first non-empty text `obj` holds in its own [`MergeKind::Text`] fields, and where it
/// is: its first non-blank line. `None` when it holds none of its own, where [`first_text`]
/// falls back to its items.
fn own_text(
    doc: &Doc,
    obj: &ObjId,
    fields: &[&FieldSchema],
    path: &str,
) -> Option<(String, String)> {
    for field in fields {
        if field.kind != MergeKind::Text {
            continue;
        }
        let text: String = read_key(doc, obj, field.id.as_str()).ok()?;
        let text = text
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .to_owned();
        if !text.trim().is_empty() {
            return Some((text, path.to_owned()));
        }
    }
    None
}
