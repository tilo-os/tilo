//! Planning a mutation against the store as it is: the edit it records, and where the items
//! it inserts, moves or removes are ([`crate::items`]).

use serde_json::Value as Json;
use tilo_doc::snapshot::BlockView;
use tilo_document::ClipBlock;
use tilo_resource::{Edit, EditOp, Id, Model as _, Segment};
use tilolang_runtime::place::{Place, Placement};
use tilolang_runtime::store::{ANY_FIELD, FieldKey, FieldValue, Mutation, StoreError};
use tilolang_runtime::value::ResourceRef;

use super::reading::{json_of_scalar, path_to};
use super::{
    DynEdit, DynRecord, Inner, ItemAt, Planned, backend, idx, invalid, lookup, not_found,
    sanitize_key,
};
use crate::chunk::{ChunkNo, Limits};
use crate::items::{Found, Items};
use crate::store::{AppId, DocReads};

impl Inner {
    /// What the mutation `m` of `at` writes, planned against the store as it is now.
    pub(super) fn plan(&mut self, at: &ResourceRef, m: &Mutation) -> Result<Planned, StoreError> {
        let doc = at.doc.clone();
        match m {
            Mutation::OutboxPush {
                capability,
                payload,
                held_until,
                key,
            } => {
                let entry = key.as_deref().map_or_else(|| self.fresh("o"), sanitize_key);
                Ok(Planned {
                    edits: vec![DynEdit::OutboxPush {
                        id: doc,
                        entry,
                        capability: capability.clone(),
                        payload: payload.clone(),
                        owner: self.device.clone(),
                        held_until: held_until.clone(),
                    }],
                    item: None,
                    touched: Vec::new(),
                    items: Vec::new(),
                    placed: Vec::new(),
                    reshapes: false,
                })
            }
            // A new item, stored by its id, placed before the item the program saw at the
            // place it named (else at the end), so a concurrent insert, removal or move
            // elsewhere does not shift where it lands.
            Mutation::CollInsert {
                field,
                item,
                index,
                id,
            } => {
                let key = self.key_for(at, field, *index, None)?;
                let schema = lookup(&item.ty)
                    .ok_or_else(|| not_found(format!("type {} is not registered", item.ty)))?;
                let record = DynRecord::empty(schema).map_err(backend)?.to_json();
                let id = id.clone().unwrap_or_else(|| self.fresh("i"));
                let placement = Placement::At {
                    place: Place {
                        parent: at.item.clone(),
                        field: field.clone(),
                        key: key.clone(),
                    },
                    was: None,
                };
                let coll = FieldKey::new(at, field.clone());
                // The item goes to the chunk of the collection's newest run, which the same
                // action records in the record's manifest.
                let (chunk, run) = self.run_for(at, field, &key)?;
                let mut edits = Vec::new();
                if let Some(run) = run {
                    edits.push(run.in_chunk(crate::chunk::FIRST));
                }
                edits.push(DynEdit::routed_at(
                    &self.chunks,
                    DynEdit::Insert {
                        id: doc.clone(),
                        item: id.clone(),
                        record,
                        at: placement.encode(),
                    },
                    chunk,
                ));
                Ok(Planned {
                    edits,
                    item: Some(id.clone()),
                    touched: vec![coll.clone()],
                    items: vec![(coll, id.clone())],
                    placed: vec![(doc, id, true)],
                    reshapes: false,
                })
            }
            Mutation::CollMove { field, item, index } => self.plan_move(at, field, (item, *index)),
            Mutation::CollRemove { item, .. } => self.plan_remove(&doc, item),
            _ => {
                let prefix = self.prefix_of(at)?;
                let chunk = self.at_chunk(at)?;
                let (edit, reshapes) = match m {
                    Mutation::SetRich { field, value } => {
                        let (op, reshapes) = self.rich_edit_of(at, field, value)?;
                        (Edit::new(path_to(&prefix, field), op), reshapes)
                    }
                    _ => (edit_of(&prefix, m)?, false),
                };
                Ok(Planned {
                    edits: vec![DynEdit::routed_at(
                        &self.chunks,
                        DynEdit::Edit { id: doc, edit },
                        chunk,
                    )],
                    item: None,
                    touched: vec![FieldKey::new(at, written_field(m))],
                    items: Vec::new(),
                    placed: Vec::new(),
                    reshapes,
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
                key: key.clone(),
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
        let (chunk, run) = self.run_for(at, field, &key)?;
        let edits = if chunk == from.chunk {
            vec![DynEdit::routed_at(
                &self.chunks,
                DynEdit::Place {
                    id: at.doc.clone(),
                    item: item.to_owned(),
                    at: placement.encode(),
                    nested: from.nesting,
                    ty: from.ty,
                    holders,
                },
                chunk,
            )]
        } else {
            // The item's map is made in the chunk of the collection it joins, with what it
            // holds, and the copy in the chunk it leaves goes: one action, one item.
            let found = self.found_item(&at.doc, item)?;
            let record = crate::items::item_json(&self.chunks, &found).map_err(backend)?;
            let mut edits = Vec::new();
            if let Some(run) = run {
                edits.push(run.in_chunk(crate::chunk::FIRST));
            }
            edits.push(DynEdit::routed_at(
                &self.chunks,
                DynEdit::RemoveItem {
                    id: at.doc.clone(),
                    item: item.to_owned(),
                },
                from.chunk,
            ));
            edits.push(DynEdit::routed_at(
                &self.chunks,
                DynEdit::Insert {
                    id: at.doc.clone(),
                    item: item.to_owned(),
                    record,
                    at: placement.encode(),
                },
                chunk,
            ));
            edits
        };
        Ok(Planned {
            edits,
            item: None,
            touched: vec![to.clone(), left.clone()],
            items: vec![(to, item.to_owned()), (left, item.to_owned())],
            placed: vec![(at.doc.clone(), item.to_owned(), false)],
            reshapes: false,
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
            edits: vec![DynEdit::routed_at(
                &self.chunks,
                DynEdit::Place {
                    id: doc.to_owned(),
                    item: item.to_owned(),
                    at: Placement::Removed.encode(),
                    nested: from.nesting,
                    ty: from.ty,
                    holders: vec![from.holder],
                },
                from.chunk,
            )],
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
            reshapes: false,
        })
    }

    /// The map of the record of `at`, and the record's type.
    pub(super) fn record_root(
        &self,
        at: &ResourceRef,
    ) -> Result<(AppId, automerge::ObjId, &'static crate::schema::TypeSchema), StoreError> {
        let (app, ty) = self.locate(at)?;
        let mirror = self.held_doc(&at.doc)?;
        let root = crate::reads::resource_obj(mirror, &app, &at.doc)
            .ok_or_else(|| not_found(format!("resource {}", at.doc)))?;
        let schema = lookup(&ty).ok_or_else(|| not_found(format!("type {ty}")))?;
        Ok((app, root, schema))
    }

    /// The path from the record's map to the map of `at`: none for the record, the item's id
    /// for an item stored by it, the steps it is nested at for one of a record written before
    /// items were flat.
    pub(super) fn prefix_of(&self, at: &ResourceRef) -> Result<Vec<Segment>, StoreError> {
        let Some(item) = &at.item else {
            return Ok(Vec::new());
        };
        let found = self.found_item(&at.doc, item)?;
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
        let found = self.found_item(doc, item)?;
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
            chunk: found.chunk,
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
        let (app, _, schema) = self.record_root(at)?;
        let items = Items::across(&self.chunks, &app, &at.doc, schema);
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

impl Inner {
    /// The chunk the item `item` of the record `doc` is stored in, and its map there.
    fn found_item(&self, doc: &str, item: &str) -> Result<Found, StoreError> {
        let at = ResourceRef::root(doc, String::new()).item(item, String::new());
        let (app, _, schema) = self.record_root(&at)?;
        crate::items::find_across(&self.chunks, &app, doc, schema, &self.nested, item)
            .ok_or_else(|| not_found(format!("item `{item}`")))
    }

    /// The chunk the index routes the item at `at` to, and `None` while it names none: no step
    /// has derived a row of it, so where its map is read from is the record's to say.
    pub(super) fn routed_chunk(&self, at: &ResourceRef) -> Option<ChunkNo> {
        let (app, _) = self.locate(at).ok()?;
        let item = at.item.as_ref()?;
        let view = self.view(&app)?;
        let held = view
            .text(&crate::index::derive::item_path(&at.doc, item), idx::CHUNK)
            .ok()??;
        tilo_failure::ignored!(
            "a row the index wrote before chunks says no chunk, and the record says where the item is",
            held.parse::<ChunkNo>().ok()
        )
    }

    /// Whether the chunks the store holds hold the item `item` of the record `doc`, in a run of
    /// one of its collections.
    pub(super) fn holds_item(&self, doc: &str, item: &str) -> bool {
        self.found_item(doc, item).is_ok()
    }

    /// The chunk the resource at `at` is read and written in: of an item, the chunk its map is
    /// stored in (from the index, or the item's own chunk when the index does not name it), else
    /// the chunk the record's manifest places the resource in.
    pub(super) fn at_chunk(&self, at: &ResourceRef) -> Result<ChunkNo, StoreError> {
        let Some(item) = &at.item else {
            return Ok(self.chunks.chunk(&at.doc));
        };
        if let Some(chunk) = self.routed_chunk(at) {
            return Ok(chunk);
        }
        Ok(self.found_item(&at.doc, item)?.chunk)
    }

    /// How many items the collection `field` of `at` holds now.
    fn list_count(&self, at: &ResourceRef, field: &str) -> Result<u64, StoreError> {
        let (app, _) = self.locate(at)?;
        if let Some(view) = self.view(&app) {
            let list = view.list(&at.id_string(), field).map_err(backend)?;
            return Ok(u64::try_from(list.len()).unwrap_or(u64::MAX));
        }
        let (_, resolved) = self.resolved(at)?;
        let count = resolved
            .children
            .get(&(at.item.clone(), field.to_owned()))
            .map_or(0, Vec::len);
        Ok(u64::try_from(count).unwrap_or(u64::MAX))
    }

    /// Where the item placed at the order key `key` of the collection `field` of `at` goes: the
    /// chunk of the collection's newest run, with the edit that records the run in the record's
    /// manifest. A full run starts the next chunk, so an item's place never changes with size.
    fn run_for(
        &self,
        at: &ResourceRef,
        field: &str,
        key: &str,
    ) -> Result<(ChunkNo, Option<DynEdit>), StoreError> {
        let doc = at.doc.clone();
        let holder = at
            .item
            .as_ref()
            .map_or_else(|| doc.clone(), |parent| format!("{doc}/{parent}"));
        let placement = self.chunks.placement();
        let runs = placement.runs(&holder, field);
        let (chunk, starts) = placement.for_new(
            &holder,
            field,
            self.chunks.chunk(&holder),
            Limits::default(),
        );
        let (last, len) = match (starts, runs.last()) {
            (true, _) => (key.to_owned(), 1),
            (false, Some((held, run))) if *held == chunk => (
                run.last.clone().max(key.to_owned()),
                run.len.saturating_add(1),
            ),
            // A collection whose items were placed before its runs were recorded: the run it
            // starts with holds what the collection holds.
            (false, _) => (
                key.to_owned(),
                self.list_count(at, field)?.saturating_add(1),
            ),
        };
        Ok((
            chunk,
            Some(DynEdit::RunChunk {
                holder,
                field: field.to_owned(),
                chunk,
                last,
                len,
            }),
        ))
    }
}

/// The field a mutation writes.
fn written_field(m: &Mutation) -> String {
    match m {
        Mutation::SetRegister { field, .. }
        | Mutation::AddCounter { field, .. }
        | Mutation::SetText { field, .. }
        | Mutation::SetRich { field, .. }
        | Mutation::CollInsert { field, .. }
        | Mutation::CollRemove { field, .. }
        | Mutation::CollMove { field, .. }
        | Mutation::SetAdd { field, .. }
        | Mutation::SetRemove { field, .. }
        | Mutation::MapPut { field, .. }
        | Mutation::MapRemove { field, .. } => field.clone(),
        Mutation::OutboxPush { .. } | Mutation::OutboxWithdraw => String::new(),
    }
}

/// The edit a mutation of a field makes below the resource whose map `prefix` reaches.
pub(super) fn edit_of(prefix: &[Segment], m: &Mutation) -> Result<Edit<DynRecord>, StoreError> {
    let (field, op) = match m {
        Mutation::SetRegister { field, value } => (
            field,
            EditOp::Set {
                value: json_of_scalar(value),
            },
        ),
        Mutation::AddCounter { field, delta } => (field, EditOp::Add { by: *delta }),
        Mutation::SetText { field, text } => (field, EditOp::Replace { text: text.clone() }),
        Mutation::SetRich { .. } => {
            return Err(invalid(
                "a rich text is written as an edit of what it holds",
            ));
        }
        Mutation::SetAdd { field, key } => (
            field,
            EditOp::Insert {
                value: Json::String(key.clone()),
            },
        ),
        Mutation::SetRemove { field, key } => (
            field,
            EditOp::Remove {
                value: Json::String(key.clone()),
            },
        ),
        Mutation::MapPut { field, key, value } => {
            let mut path = path_to(prefix, field);
            path.push(Segment::Key(key.clone()));
            let op = EditOp::Set {
                value: json_of_scalar(value),
            };
            return Ok(Edit::new(path, op));
        }
        Mutation::MapRemove { field, key } => (field, EditOp::RemoveKey { key: key.clone() }),
        Mutation::CollInsert { .. } | Mutation::CollRemove { .. } | Mutation::CollMove { .. } => {
            return Err(invalid("a collection's items are placed, not edited"));
        }
        Mutation::OutboxPush { .. } | Mutation::OutboxWithdraw => {
            return Err(invalid(
                "outside effects are not stored in the workspace record yet",
            ));
        }
    };
    Ok(Edit::new(path_to(prefix, field), op))
}

/// The edit that brings a rich text field to read as `blocks`: the whole of it, or with
/// `replace` the blocks it names, the first of them at `at` ([`tilo_document::RichTextAction::SyncClips`]).
pub(crate) fn rich_edit(
    blocks: Vec<ClipBlock>,
    replace: Option<Vec<String>>,
    at: usize,
) -> Result<EditOp, serde_json::Error> {
    let action = tilo_document::RichTextAction::SyncClips {
        blocks,
        replace,
        at,
    };
    Ok(EditOp::Rich {
        rich: serde_json::to_value(&action)?,
    })
}

/// The most characters one keystroke writes: a word an input method or autocorrect hands over.
/// More at once is a paste or a dictation, a step of the person's on its own.
const MOST_TYPED_AT_ONCE: usize = 64;

/// How many characters `now` holds that `was` did not, between what both start and end with.
fn typed(was: &str, now: &str) -> usize {
    let start = was
        .chars()
        .zip(now.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let end = was
        .chars()
        .rev()
        .zip(now.chars().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (was, now) = (was.chars().count(), now.chars().count());
    now.saturating_sub(start.saturating_add(end).min(was).min(now))
}

impl Inner {
    /// The edit that brings the rich text `field` of `at` from what it holds to read as `value`:
    /// the paragraphs that differ, in their place among those it keeps. A keystroke records the
    /// paragraph it typed into, however long the text. Also whether the edit gives the text a
    /// structure rather than words — a paragraph more or fewer, or one whose kind or attributes
    /// changed (`# ` making a line a heading) — or more words than a keystroke types, a step of
    /// the person's on its own ([`super::burst`]).
    fn rich_edit_of(
        &self,
        at: &ResourceRef,
        field: &str,
        value: &tilolang_runtime::rich::RichValue,
    ) -> Result<(EditOp, bool), StoreError> {
        let (app, ty) = self.locate(at)?;
        let held = match self.read_mirror((&app, &ty), at, field)? {
            FieldValue::Rich(held) => held.blocks,
            _ => Vec::new(),
        };
        let want = &value.blocks;
        let same = |(a, b): &(&BlockView, &BlockView)| {
            a.kind == b.kind && a.text == b.text && a.marks == b.marks && a.attrs == b.attrs
        };
        let start = held.iter().zip(want).take_while(same).count();
        let end = held
            .iter()
            .rev()
            .zip(want.iter().rev())
            .take_while(same)
            .count();
        let end = end.min(held.len().min(want.len()).saturating_sub(start));
        let was = held
            .get(start..held.len().saturating_sub(end))
            .unwrap_or_default();
        let now = want
            .get(start..want.len().saturating_sub(end))
            .unwrap_or_default();
        let reshapes = was.len() != now.len()
            || was.iter().zip(now).any(|(was, now)| {
                was.kind != now.kind
                    || was.attrs != now.attrs
                    || typed(&was.text, &now.text) > MOST_TYPED_AT_ONCE
            });
        // A text not written yet holds what a new one starts with, which the value replaces.
        let replace =
            (!held.is_empty()).then(|| was.iter().map(|block| block.id.clone()).collect());
        let blocks = now.iter().map(ClipBlock::from_view).collect();
        let op = rich_edit(blocks, replace, start).map_err(backend)?;
        Ok((op, reshapes))
    }
}
