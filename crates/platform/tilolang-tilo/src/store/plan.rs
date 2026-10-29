//! Planning a mutation against the store as it is: the edit it records, and where the items
//! it inserts, moves or removes are ([`crate::items`]).

use tilo_resource::{Id, Model as _, Segment};
use tilolang_runtime::place::{Place, Placement};
use tilolang_runtime::store::{ANY_FIELD, FieldKey, Mutation, StoreError};
use tilolang_runtime::value::ResourceRef;

use super::{
    DynEdit, DynRecord, Inner, ItemAt, Planned, backend, edit_of, field_of, idx, invalid, lookup,
    not_found, sanitize_key,
};
use crate::items::Items;
use crate::store::{AppId, DocReads};

impl Inner {
    /// What the mutation `m` of `at` writes, planned against the store as it is now.
    pub(super) fn plan(&mut self, at: &ResourceRef, m: &Mutation) -> Result<Planned, StoreError> {
        let doc = at.doc.clone();
        match m {
            Mutation::OutboxPush {
                capability,
                payload,
                key,
            } => {
                let entry = key.as_deref().map_or_else(|| self.fresh("o"), sanitize_key);
                Ok(Planned {
                    edit: DynEdit::OutboxPush {
                        id: doc,
                        entry,
                        capability: capability.clone(),
                        payload: payload.clone(),
                        owner: self.device.clone(),
                    },
                    item: None,
                    touched: Vec::new(),
                    items: Vec::new(),
                    placed: Vec::new(),
                })
            }
            // A new item, stored by its id, placed before the item the program saw at the
            // place it named (else at the end), so a concurrent insert, removal or move
            // elsewhere does not shift where it lands.
            Mutation::CollInsert { field, item, index } => {
                let key = self.key_for(at, field, *index, None)?;
                let schema = lookup(&item.ty)
                    .ok_or_else(|| not_found(format!("type {} is not registered", item.ty)))?;
                let record = DynRecord::empty(schema).map_err(backend)?.to_json();
                let id = self.fresh("i");
                let placement = Placement::At {
                    place: Place {
                        parent: at.item.clone(),
                        field: field.clone(),
                        key,
                    },
                    was: None,
                };
                let coll = FieldKey::new(at, field.clone());
                Ok(Planned {
                    edit: DynEdit::Insert {
                        id: doc.clone(),
                        item: id.clone(),
                        record,
                        at: placement.encode(),
                    },
                    item: Some(id.clone()),
                    touched: vec![coll.clone()],
                    items: vec![(coll, id.clone())],
                    placed: vec![(doc, id, true)],
                })
            }
            Mutation::CollMove { field, item, index } => self.plan_move(at, field, (item, *index)),
            Mutation::CollRemove { item, .. } => self.plan_remove(&doc, item),
            _ => {
                let prefix = self.prefix_of(at)?;
                Ok(Planned {
                    edit: DynEdit::Edit {
                        id: doc,
                        edit: edit_of(&prefix, m)?,
                    },
                    item: None,
                    touched: vec![FieldKey::new(at, field_of(m))],
                    items: Vec::new(),
                    placed: Vec::new(),
                })
            }
        }
    }

    /// An item, from wherever it is in the document, to the place `index` of the collection
    /// `field` of `at`: its placement written, its id kept.
    fn plan_move(
        &self,
        at: &ResourceRef,
        field: &str,
        (item, index): (&str, usize),
    ) -> Result<Planned, StoreError> {
        let from = self.item_at(&at.doc, item)?;
        let mut holder = Some(at.clone());
        while let Some(held) = holder {
            if held.item.as_deref() == Some(item) {
                return Err(invalid("an item cannot go inside itself"));
            }
            holder = self.container(&held)?.map(|(parent, _)| parent);
        }
        let key = self.key_for(at, field, Some(index), Some(item))?;
        let was = Place {
            parent: crate::index::derive::id_of(&from.holder).map(str::to_owned),
            field: from.field.clone(),
            key: from.key.clone(),
        };
        let placement = Placement::At {
            place: Place {
                parent: at.item.clone(),
                field: field.to_owned(),
                key,
            },
            was: Some(was),
        };
        let to = FieldKey::new(at, field.to_owned());
        let left = FieldKey {
            resource: from.holder.clone(),
            field: from.field.clone(),
        };
        let mut holders = vec![from.holder.clone()];
        if to.resource != from.holder {
            holders.push(to.resource.clone());
        }
        Ok(Planned {
            edit: DynEdit::Place {
                id: at.doc.clone(),
                item: item.to_owned(),
                at: placement.encode(),
                nested: from.nesting,
                ty: from.ty,
                holders,
            },
            item: None,
            touched: vec![to.clone(), left.clone()],
            items: vec![(to, item.to_owned()), (left, item.to_owned())],
            placed: vec![(at.doc.clone(), item.to_owned(), false)],
        })
    }

    /// An item removed, with every item in it: its placement written as removed.
    fn plan_remove(&self, doc: &str, item: &str) -> Result<Planned, StoreError> {
        let from = self.item_at(doc, item)?;
        let left = FieldKey {
            resource: from.holder.clone(),
            field: from.field.clone(),
        };
        Ok(Planned {
            edit: DynEdit::Place {
                id: doc.to_owned(),
                item: item.to_owned(),
                at: Placement::Removed.encode(),
                nested: from.nesting,
                ty: from.ty,
                holders: vec![from.holder],
            },
            item: None,
            touched: vec![
                left.clone(),
                // The item is gone: whatever was read of it.
                FieldKey {
                    resource: format!("{doc}/{item}"),
                    field: ANY_FIELD.to_owned(),
                },
            ],
            items: vec![(left, item.to_owned())],
            placed: vec![(doc.to_owned(), item.to_owned(), false)],
        })
    }

    /// The map of the record of `at`, and the record's type.
    fn record_root(
        &self,
        at: &ResourceRef,
    ) -> Result<(AppId, automerge::ObjId, &'static crate::schema::TypeSchema), StoreError> {
        let (app, ty) = self.locate(at)?;
        let mirror = self.mirror()?;
        let root = crate::reads::resource_obj(mirror, &app, &at.doc)
            .ok_or_else(|| not_found(format!("resource {}", at.doc)))?;
        let schema = lookup(&ty).ok_or_else(|| not_found(format!("type {ty}")))?;
        Ok((app, root, schema))
    }

    /// The path from the record's map to the map of `at`: none for the record, the item's id
    /// for an item stored by it, the steps it is nested at for one of a record written before
    /// items were flat.
    fn prefix_of(&self, at: &ResourceRef) -> Result<Vec<Segment>, StoreError> {
        let Some(item) = &at.item else {
            return Ok(Vec::new());
        };
        let (_, root, schema) = self.record_root(at)?;
        let found =
            crate::items::find(self.mirror()?, (&at.doc, &root, schema), &self.nested, item)
                .ok_or_else(|| not_found(format!("item `{item}`")))?;
        if found.nesting.is_empty() {
            return Ok(vec![Segment::Item(Id::from_raw(item.clone()))]);
        }
        Ok(found
            .nesting
            .iter()
            .flat_map(|(field, id)| {
                [
                    Segment::Field(field.clone()),
                    Segment::Item(Id::from_raw(id.clone())),
                ]
            })
            .collect())
    }

    /// Where the item `item` of the record `doc` is now, and how it is stored.
    fn item_at(&self, doc: &str, item: &str) -> Result<ItemAt, StoreError> {
        let at = ResourceRef::root(doc, String::new()).item(item, String::new());
        let (_, root, schema) = self.record_root(&at)?;
        let found = crate::items::find(self.mirror()?, (doc, &root, schema), &self.nested, item)
            .ok_or_else(|| not_found(format!("item `{item}`")))?;
        let (holder, field) = self
            .container(&at)?
            .map(|(holder, field)| (holder.id_string(), field))
            .ok_or_else(|| not_found(format!("item `{item}` is in no collection")))?;
        let key = self.key_of(doc, item)?.unwrap_or_default();
        Ok(ItemAt {
            holder,
            field,
            key,
            nesting: found.nesting,
            ty: found.ty.unwrap_or_default(),
        })
    }

    /// The order key of the item `item` of `doc` in the collection it is in.
    fn key_of(&self, doc: &str, item: &str) -> Result<Option<String>, StoreError> {
        let path = crate::index::derive::item_path(doc, item);
        if let Some((app, _)) = self.index.get(doc)
            && let Some(view) = self.view(app)
        {
            return view.text(&path, idx::KEY).map_err(backend);
        }
        let at = ResourceRef::root(doc, String::new()).item(item, String::new());
        Ok(self
            .resolved(&at)?
            .1
            .places
            .get(item)
            .map(|place| place.key.clone()))
    }

    /// The items of the record of `at`, read and resolved from the record: for a package whose
    /// index is not read from yet.
    pub(super) fn resolved(
        &self,
        at: &ResourceRef,
    ) -> Result<(Items, tilolang_runtime::place::Resolved), StoreError> {
        let (_, root, schema) = self.record_root(at)?;
        let items = Items::scan(self.mirror()?, &root, schema);
        self.count(DocReads {
            located: 1,
            fields: 0,
            listings: 1,
            entries: u64::try_from(items.found.len()).unwrap_or(u64::MAX),
        });
        let resolved = items.resolve(schema);
        Ok((items, resolved))
    }

    /// The order key for the place `index` (the end for `None`) of the collection `field` of
    /// `at`, among its items but `moving`: before the item at that place, after the one before.
    fn key_for(
        &self,
        at: &ResourceRef,
        field: &str,
        index: Option<usize>,
        moving: Option<&str>,
    ) -> Result<String, StoreError> {
        let resource = at.id_string();
        let (app, _) = self.locate(at)?;
        let Some(view) = self.view(&app) else {
            let (_, resolved) = self.resolved(at)?;
            let others: Vec<(String, String)> = resolved
                .children
                .get(&(at.item.clone(), field.to_owned()))
                .into_iter()
                .flatten()
                .filter(|(_, id)| Some(id.as_str()) != moving)
                .cloned()
                .collect();
            return Ok(tilolang_runtime::place::key_at(
                &others,
                index.unwrap_or(others.len()),
            ));
        };
        let list = view.list(&resource, field).map_err(backend)?;
        let skipped = match moving {
            Some(item) => view.position(&resource, field, item).map_err(backend)?,
            None => None,
        };
        let count = list.len().saturating_sub(usize::from(skipped.is_some()));
        let index = index.unwrap_or(count).min(count);
        let key_at = |k: usize| -> Result<Option<String>, StoreError> {
            let at = if skipped.is_some_and(|p| k >= p) {
                k.saturating_add(1)
            } else {
                k
            };
            let Some(id) = list.get(at) else {
                return Ok(None);
            };
            view.text(&crate::index::view::child(&resource, &id), idx::KEY)
                .map_err(backend)
        };
        let low = match index.checked_sub(1) {
            Some(before) => key_at(before)?,
            None => None,
        };
        let high = if index < count { key_at(index)? } else { None };
        Ok(tilolang_runtime::place::key_between(
            low.as_deref(),
            high.as_deref(),
        ))
    }

    /// Where the item `at` is: the resource holding its collection, and the field. `None` for a
    /// record.
    pub(super) fn container(
        &self,
        at: &ResourceRef,
    ) -> Result<Option<(ResourceRef, String)>, StoreError> {
        let (app, _) = self.locate(at)?;
        let Some(item) = &at.item else {
            return Ok(None);
        };
        let missing = || not_found(format!("item `{item}` is in no collection"));
        if let Some(view) = self.view(&app) {
            let held = view
                .text(&at.id_string(), idx::IN)
                .map_err(backend)?
                .ok_or_else(missing)?;
            let (holder, field) = crate::index::derive::split_in(&held).ok_or_else(missing)?;
            let ty = view.type_of(holder).map_err(backend)?.unwrap_or_default();
            let holder = ResourceRef::parse_id(holder, ty).ok_or_else(missing)?;
            return Ok(Some((holder, field.to_owned())));
        }
        let (items, resolved) = self.resolved(at)?;
        let place = resolved.places.get(item).ok_or_else(missing)?;
        let holder = if let Some(parent) = &place.parent {
            let ty = items
                .found
                .get(parent)
                .and_then(|found| found.ty.clone())
                .unwrap_or_default();
            at.item(parent, ty)
        } else {
            let (_, ty) = self.locate(at)?;
            ResourceRef::root(at.doc.clone(), ty)
        };
        Ok(Some((holder, place.field.clone())))
    }
}
