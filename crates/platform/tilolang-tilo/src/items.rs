//! A record's items, by id (docs/designs/app-language/convergence.md, "Storage and sync").
//!
//! A record (`tl/<app>/<id>`) stores its items flat under `$items`, each under its id: a map
//! holding `$type`, `$app`, the item's placement `$at` ([`tilolang_runtime::place`]) and its
//! fields. A collection is derived from the placements.
//!
//! A record written before items were flat holds each item inside the collection it was
//! inserted into, nested in its holder: an entry `{ k: order key, v: the item's map }` (or the
//! order key alone, for an item whose every field reads as its default) in the collection
//! field's map. Such an item is read where it is, as an item of the flat form: under its id,
//! placed in that collection at that order key, until it is moved or removed, which writes its
//! placement `$at` into its map. Nothing is copied or rewritten, so no edit made anywhere is
//! lost to a migration, and every id stays. An entry that does not read (an id held twice, an
//! entry without an order key) is set aside: left where it is, and in no collection.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use automerge::{ObjId, ObjType, ReadDoc, Value as AmValue};
use serde_json::Value as Json;
use tilo_resource::Model as _;
use tilo_resource::ModelError;
use tilo_resource::collection::{collection_entries, read_entry};
use tilolang_runtime::ids::{DocId, ItemId, TypeId};
use tilolang_runtime::place::{self, Held, ITEMS_KEY, Place, Placement, Resolved};

use crate::chunk::{ChunkNo, Chunks, FIRST};
use crate::reads::{map_at, type_at};
use crate::schema::{MergeKind, TypeSchema, lookup};

type Doc = tilo_resource::Doc;

/// Where an item of a record written before items were flat is: the steps from the record's
/// map down to it, each a collection field and the item in it.
pub type Nesting = Vec<(String, ItemId)>;

/// An item's map and type, wherever it is stored.
#[derive(Clone, Debug)]
pub struct Found {
    /// Its map: `None` for an item stored as its order key alone (every field reads as its
    /// default).
    pub obj: Option<ObjId>,
    /// Its type: the one its map holds, else its collection's item type.
    pub ty: Option<TypeId>,
    /// Its placement, as stored or (an item of an older record that never moved) where it is.
    pub held: Option<Held>,
    /// Where an item of an older record is nested; empty for a flat item.
    pub nesting: Nesting,
    /// The chunk its map is stored in: the document a read of its fields goes to.
    pub chunk: ChunkNo,
}

/// The copy of an item that is later in the record's order: of two chunks holding an item of the
/// same id (a move made on two replicas at once), the one whose placing change comes later.
pub(crate) fn later(held: Option<Found>, next: Found) -> Found {
    fn written(found: &Found) -> (u64, &[u8]) {
        found.held.as_ref().map_or((0, &[][..]), |held| {
            (held.written.0, held.written.1.as_slice())
        })
    }
    match held {
        Some(held) if written(&held) > written(&next) => held,
        _ => next,
    }
}

/// Where the items of records written before items were flat are, per record: found by one
/// walk of the record's nested collections the first time one is wanted, and kept, as those
/// items never move in the record (a move writes their placement, not their place). Forgotten
/// when an entry of a nested collection is put or deleted.
#[derive(Debug, Default)]
pub struct Nested {
    docs: RefCell<HashMap<DocId, Rc<HashMap<ItemId, Nesting>>>>,
}

impl Nested {
    /// Forget where the nested items of `doc` are.
    pub fn forget(&self, doc: &str) {
        self.docs.borrow_mut().remove(doc);
    }

    /// Forget every record's.
    pub fn clear(&self) {
        self.docs.borrow_mut().clear();
    }

    /// The nested items of the record `id` (its map `root`, of type `schema`).
    fn of(
        &self,
        doc: &Doc,
        id: &str,
        root: &ObjId,
        schema: &TypeSchema,
    ) -> Rc<HashMap<ItemId, Nesting>> {
        if let Some(held) = self.docs.borrow().get(id) {
            return Rc::clone(held);
        }
        let mut found = HashMap::new();
        walk_nested(doc, root, schema, &mut Vec::new(), &mut |nesting, _| {
            if let Some((_, item)) = nesting.last() {
                found
                    .entry(item.clone())
                    .or_insert_with(|| nesting.to_vec());
            }
        });
        let found = Rc::new(found);
        self.docs
            .borrow_mut()
            .insert(id.to_owned(), Rc::clone(&found));
        found
    }
}

/// One nested entry as a walk meets it.
struct Entry {
    key: String,
    obj: Option<ObjId>,
    ty: Option<TypeId>,
}

/// What a walk of nested entries does with each: given where it is nested, and the entry.
type Visit<'a> = dyn FnMut(&[(String, ItemId)], &Entry) + 'a;

/// Visit every nested entry below the map `obj` (of type `schema`), in field order and each
/// collection's stored order, parents before their items.
fn walk_nested(
    doc: &Doc,
    obj: &ObjId,
    schema: &TypeSchema,
    at: &mut Nesting,
    visit: &mut Visit<'_>,
) {
    for field in schema
        .fields
        .iter()
        .filter(|f| f.kind == MergeKind::Collection)
    {
        let Some(coll) = map_at(doc, obj, &field.id) else {
            continue;
        };
        let mut stored: Vec<_> = collection_entries(doc, &coll)
            .into_iter()
            .filter_map(|entry| read_entry(doc, entry))
            .collect();
        tilo_resource::collection::in_stored_order(&mut stored);
        for item in stored {
            let item_obj = item.value.as_ref().and_then(|(value, id)| {
                matches!(value, AmValue::Object(ObjType::Map)).then(|| id.clone())
            });
            let ty = item_obj
                .as_ref()
                .and_then(|o| type_at(doc, o))
                .or_else(|| field.record.clone());
            at.push((field.id.clone(), item.id.as_str().to_owned()));
            visit(
                at,
                &Entry {
                    key: item.key.clone(),
                    obj: item_obj.clone(),
                    ty: ty.clone(),
                },
            );
            if let (Some(item_obj), Some(item_schema)) = (&item_obj, ty.as_deref().and_then(lookup))
            {
                walk_nested(doc, item_obj, item_schema, at, visit);
            }
            at.pop();
        }
    }
}

/// The map of a flat item `id` of the record map `root`: an item of a record that holds two
/// maps of its items ([`crate::containers`]) is in one of them.
fn flat(doc: &Doc, root: &ObjId, id: &str) -> Option<ObjId> {
    // The copy the record keeps answers first, then those it replaced: an item of either is
    // the item, and a write goes where the record keeps its items.
    crate::containers::maps(doc, root, ITEMS_KEY)
        .iter()
        .rev()
        .find_map(|items| map_at(doc, items, id))
}

/// The placement a nested item reads as while it holds none: the collection it is in.
fn nested_place(nesting: &[(String, ItemId)], key: String) -> Option<Place> {
    let (field, _) = nesting.last()?;
    let parent = nesting
        .len()
        .checked_sub(2)
        .and_then(|at| nesting.get(at))
        .map(|(_, parent)| parent.clone());
    Some(Place {
        parent,
        field: field.clone(),
        key,
    })
}

/// The nested entry at `nesting` below the record map `root`: its order key and map.
pub fn nested_entry(
    doc: &Doc,
    root: &ObjId,
    nesting: &[(String, ItemId)],
) -> Option<(String, Option<ObjId>)> {
    let mut obj = Some(root.clone());
    let mut key = None;
    for (field, item) in nesting {
        let coll = map_at(doc, obj.as_ref()?, field)?;
        let stored = tilo_resource::stored_entry(doc, &coll, item)?;
        key = Some(stored.key.clone());
        obj = stored.value.as_ref().and_then(|(value, id)| {
            matches!(value, AmValue::Object(ObjType::Map)).then(|| id.clone())
        });
    }
    Some((key?, obj))
}

/// The item `id` of the record `doc_id` (its map `root`, of type `schema`), wherever it is
/// stored: `None` when the record holds none.
pub fn find(
    doc: &Doc,
    (doc_id, root, schema): (&str, &ObjId, &TypeSchema),
    nested: &Nested,
    id: &str,
) -> Option<Found> {
    if let Some(obj) = flat(doc, root, id) {
        return Some(Found {
            ty: type_at(doc, &obj),
            held: place::held_at(doc, &obj),
            obj: Some(obj),
            nesting: Vec::new(),
            chunk: FIRST,
        });
    }
    let nesting = nested.of(doc, doc_id, root, schema).get(id)?.clone();
    let (key, obj) = nested_entry(doc, root, &nesting)?;
    let ty = obj
        .as_ref()
        .and_then(|o| type_at(doc, o))
        .or_else(|| nested_item_type(schema, &nesting));
    let held = obj
        .as_ref()
        .and_then(|o| place::held_at(doc, o))
        .or_else(|| {
            Some(Held {
                placement: Placement::At {
                    place: nested_place(&nesting, key)?,
                    was: None,
                },
                written: (0, Vec::new()),
            })
        });
    Some(Found {
        obj,
        ty,
        held,
        nesting,
        chunk: FIRST,
    })
}

/// The item `id` of the record `doc` (of `app`), in whichever chunk the store holds holding it:
/// the copy that is later in the record's order, as [`Items::across`] reads it.
#[must_use]
pub fn find_across(
    chunks: &Chunks,
    app: &str,
    doc: &str,
    schema: &TypeSchema,
    nested: &Nested,
    id: &str,
) -> Option<Found> {
    let mut found: Option<Found> = None;
    for (chunk, held) in chunks.docs() {
        let Some(root) = crate::reads::resource_obj(held, app, doc) else {
            continue;
        };
        // An item of a record written before items were flat is nested in the record's own map.
        let next = if chunk == FIRST {
            find(held, (doc, &root, schema), nested, id)
        } else {
            flat(held, &root, id).map(|obj| Found {
                ty: type_at(held, &obj),
                held: place::held_at(held, &obj),
                obj: Some(obj),
                nesting: Vec::new(),
                chunk,
            })
        };
        if let Some(next) = next {
            found = Some(later(found.take(), next));
        }
    }
    found
}

/// The item type of the collection the last step of `nesting` is in.
pub fn nested_item_type(schema: &TypeSchema, nesting: &[(String, ItemId)]) -> Option<TypeId> {
    let mut schema = schema;
    let mut ty = None;
    for (field, _) in nesting {
        let record = schema.field(field)?.record.clone()?;
        schema = lookup(&record)?;
        ty = Some(record);
    }
    ty
}

/// Every item of a record, by id: where it is stored and its placement.
#[derive(Debug, Default)]
pub struct Items {
    pub found: BTreeMap<ItemId, Found>,
}

impl Items {
    /// Every item of the record map `root` (of type `schema`): its flat items, then those
    /// nested in a record written before items were flat. An id held twice is read once, the
    /// first time it is met; the others are set aside. Of a flat id held in two copies of the
    /// map of items, the copy the record keeps is read.
    #[must_use]
    pub fn scan(doc: &Doc, root: &ObjId, schema: &TypeSchema, chunk: ChunkNo) -> Self {
        let mut found = BTreeMap::new();
        // Every copy of the map the record keeps its items in, the one it keeps last: the
        // items of a copy it does not keep a replica made before it met the others, and they
        // are the record's just the same.
        for items in crate::containers::maps(doc, root, ITEMS_KEY) {
            for id in doc.keys(&items) {
                let Some(obj) = map_at(doc, &items, &id) else {
                    continue;
                };
                found.insert(
                    id,
                    Found {
                        ty: type_at(doc, &obj),
                        held: place::held_at(doc, &obj),
                        obj: Some(obj),
                        nesting: Vec::new(),
                        chunk,
                    },
                );
            }
        }
        walk_nested(doc, root, schema, &mut Vec::new(), &mut |nesting, entry| {
            let Some((_, id)) = nesting.last() else {
                return;
            };
            if found.contains_key(id) {
                return;
            }
            let held = entry
                .obj
                .as_ref()
                .and_then(|o| place::held_at(doc, o))
                .or_else(|| {
                    Some(Held {
                        placement: Placement::At {
                            place: nested_place(nesting, entry.key.clone())?,
                            was: None,
                        },
                        written: (0, Vec::new()),
                    })
                });
            found.insert(
                id.clone(),
                Found {
                    obj: entry.obj.clone(),
                    ty: entry.ty.clone(),
                    held,
                    nesting: nesting.to_vec(),
                    chunk,
                },
            );
        });
        Self { found }
    }

    /// Every item of the record `doc` (of `app`) across the chunks the store holds: the items
    /// of a collection are stored in the chunk of the run they are in, so a record's items are
    /// read from each chunk that holds some. An id held in two chunks (a move made on two
    /// replicas at once) reads as the copy that is later in the record's order.
    #[must_use]
    pub fn across(chunks: &Chunks, app: &str, doc: &str, schema: &TypeSchema) -> Self {
        Self::across_in(chunks, app, doc, schema, None)
    }

    /// [`Self::across`] over the chunks `only` names (`None`: every chunk the store holds): what
    /// a record holds of the chunks a step of a build reads, where a window's first rows are,
    /// the chunks of the rest arriving as they are read.
    #[must_use]
    pub fn across_in(
        chunks: &Chunks,
        app: &str,
        doc: &str,
        schema: &TypeSchema,
        only: Option<&[ChunkNo]>,
    ) -> Self {
        let mut items = Self::default();
        for (chunk, held) in chunks.docs() {
            if only.is_some_and(|only| !only.contains(&chunk)) {
                continue;
            }
            let Some(root) = crate::reads::resource_obj(held, app, doc) else {
                continue;
            };
            for (id, found) in Self::scan(held, &root, schema, chunk).found {
                let was = items.found.remove(&id);
                items.found.insert(id, later(was, found));
            }
        }
        items
    }

    /// The items resolved into the collections they are in, the record being of `schema`.
    #[must_use]
    pub fn resolve(&self, schema: &TypeSchema) -> Resolved {
        let held: BTreeMap<ItemId, Held> = self
            .found
            .iter()
            .filter_map(|(id, found)| Some((id.clone(), found.held.clone()?)))
            .collect();
        let type_of = |id: &str| -> Option<&'static TypeSchema> {
            self.found.get(id)?.ty.as_deref().and_then(lookup)
        };
        place::resolve(&held, &|parent, field, item| {
            let holder = match parent {
                Some(parent) => type_of(parent),
                None => lookup(&schema.id),
            };
            let Some(item_ty) = self.found.get(item).and_then(|f| f.ty.as_deref()) else {
                return false;
            };
            fits(holder, field, item_ty)
        })
    }
}

/// The item `found` as JSON, for the edit that makes the item's copy in another chunk: every
/// field of it as stored, without the items of its collections, which stay where they are.
pub fn item_json(chunks: &Chunks, found: &Found) -> Result<Json, ModelError> {
    let doc = chunks
        .mirror(found.chunk)
        .ok_or_else(|| ModelError::invalid("the chunk holding the item is not loaded"))?;
    let schema = found
        .ty
        .as_deref()
        .and_then(lookup)
        .ok_or_else(|| ModelError::invalid("the item's type is not registered"))?;
    let record = match &found.obj {
        Some(obj) => crate::record::DynRecord::read_own_fields(doc, obj)?,
        None => crate::record::DynRecord::empty(schema)?,
    };
    Ok(record.to_json())
}

/// Whether the collection `field` of a resource of `holder` may hold an item of `item`.
#[must_use]
pub fn fits(holder: Option<&TypeSchema>, field: &str, item: &str) -> bool {
    holder
        .and_then(|schema| schema.field(field))
        .filter(|f| f.kind == MergeKind::Collection)
        .is_some_and(|f| f.record.as_deref().is_none_or(|record| record == item))
        && lookup(item).is_some()
}
