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
use tilo_resource::collection::{collection_entries, read_entry};
use tilolang_runtime::ids::{DocId, ItemId, TypeId};
use tilolang_runtime::place::{self, Held, ITEMS_KEY, Place, Placement, Resolved};

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

/// The map of a flat item `id` of the record map `root`.
fn flat(doc: &Doc, root: &ObjId, id: &str) -> Option<ObjId> {
    let items = map_at(doc, root, ITEMS_KEY)?;
    map_at(doc, &items, id)
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
    })
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
    /// first time it is met; the others are set aside.
    #[must_use]
    pub fn scan(doc: &Doc, root: &ObjId, schema: &TypeSchema) -> Self {
        let mut found = BTreeMap::new();
        if let Some(items) = map_at(doc, root, ITEMS_KEY) {
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
                },
            );
        });
        Self { found }
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

/// Whether the collection `field` of a resource of `holder` may hold an item of `item`.
#[must_use]
pub fn fits(holder: Option<&TypeSchema>, field: &str, item: &str) -> bool {
    holder
        .and_then(|schema| schema.field(field))
        .filter(|f| f.kind == MergeKind::Collection)
        .is_some_and(|f| f.record.as_deref().is_none_or(|record| record == item))
        && lookup(item).is_some()
}
