//! The seeded workspace: the record a workspace of `n` items holds ([`seeded_record`]), and the
//! seed that makes it. What it fills and how is the package's [`Shape`]: the root's collection,
//! inside its holders when it stands inside a record of its own (a mail account, a chat's
//! sidebar).
//!
//! The seed adds items as a person does, in actions a change records whole: an action larger
//! than a change holds is recorded with its large parts kept as content, and its change is
//! history's step on the whole record rather than a version of each item it made.

use serde_json::{Value as Json, json};
use tilo_actions::Action as _;
use tilo_resource::{Collection, Id};
use tilolang::Compiled;
use tilolang_runtime::meta::{Meta, Ty};
use tilolang_runtime::value::{ResourceRef, Value};

use super::{
    BENCH_DAY, BENCH_TIME, Doc, List, MergeKind, PLACES, Shape, execute, item_id, lookup, root_id,
    shape_of, unwritten,
};
use crate::action::{Authorship, DynAction, DynEdit};
use crate::chunk::{self, Chunks};
use crate::record::{DynRecord, DynValue};

/// The item's optional dates (its `Option<Date>` fields).
fn optional_dates(ty: &str) -> Result<Vec<String>, String> {
    let schema = lookup(ty).ok_or_else(|| format!("unregistered type {ty}"))?;
    Ok(schema
        .fields
        .iter()
        .filter(|f| f.kind == MergeKind::Register)
        .filter(|f| matches!(&f.ty, Some(Ty::Option(inner)) if matches!(**inner, Ty::Date)))
        .map(|f| f.id.clone())
        .collect())
}

/// `n` items of [`List`], as its collection holds them, outside any holder.
fn seeded_items(list: &List, n: usize) -> Result<Collection<DynRecord>, String> {
    let n = if list.places { n.min(PLACES) } else { n };
    let dates = optional_dates(&list.item_ty)?;
    let mut items = Collection::default();
    for i in 0..n {
        let mut item = unwritten(&list.item_ty)?;
        for (k, field) in list.texts.iter().enumerate() {
            if let Some(DynValue::Text(text)) = item.get_mut(field) {
                *text = if k == 0 {
                    format!("Item {i}")
                } else {
                    format!("Text of item {i}")
                };
            }
        }
        for field in &list.flags {
            if let Some(DynValue::Register(flag)) = item.get_mut(field) {
                *flag = json!(i % 3 == 0);
            }
        }
        for field in &list.sets {
            if let Some(DynValue::Set(slot)) = item.get_mut(field) {
                slot.insert(json!("inbox"));
            }
        }
        // Every second item is dated the day the bench's clock reads, as a workspace a person adds
        // work to day by day holds it: a filter on the day holds of some items and not of others,
        // and a window opening on a place that filters by it shows rows of the workspace.
        if i % 2 == 0 {
            for field in &dates {
                if let Some(DynValue::Register(date)) = item.get_mut(field) {
                    *date = json!({ "some": BENCH_DAY });
                }
            }
        }
        items.push(Id::from_raw(item_id(i)), item);
    }
    Ok(items)
}

/// The list's collection, empty, wrapped in the records the collection is inside: each holder is
/// a record of its own holding the one below it, the topmost in the root.
pub(crate) fn with_holders(list: &List) -> Result<Collection<DynRecord>, String> {
    let mut items = Collection::default();
    // Each holder's own collection holds the one below it, its own id naming it: a mail
    // account's conversations.
    for (level, holder) in list.holders.iter().enumerate().rev() {
        let mut record = unwritten(&holder.ty)?;
        for (k, field) in holder.texts.iter().enumerate() {
            if let Some(DynValue::Text(text)) = record.get_mut(field) {
                *text = if k == 0 {
                    "Holder".to_owned()
                } else {
                    format!("Text {k}")
                };
            }
        }
        // The holder holds the record below it: the next holder, or the list itself.
        let field = list
            .holders
            .get(level.saturating_add(1))
            .map_or_else(|| list.field.clone(), |next| next.field.clone());
        match record.get_mut(&field) {
            Some(DynValue::Collection(slot)) => *slot = items,
            _ => return Err("the holder's field is not a collection".into()),
        }
        let mut held = Collection::default();
        held.push(Id::from_raw(holder_id(level)), record);
        items = held;
    }
    Ok(items)
}

/// The field of the root the list's topmost collection is in: the first holder's, or the
/// list's own when it stands in the root.
pub(crate) fn top_field(list: &List) -> String {
    list.holders
        .first()
        .map_or_else(|| list.field.clone(), |h| h.field.clone())
}

/// The id of the holder at `level` (the record the list's collection is inside).
fn holder_id(level: usize) -> String {
    format!("hold-{level:03}")
}

/// The root record with its collections empty, referring to the roots it links.
///
/// A list inside holders comes with its holders: the holders hold one record each (the
/// workspace's mail accounts; a workspace has a few), and what grows is the collection in the
/// deepest of them (`add_in_runs`).
fn seeded_root(shape: &Shape) -> Result<DynRecord, String> {
    let mut root = unwritten(&shape.root_ty)?;
    if let Some(list) = &shape.list {
        let items = with_holders(list)?;
        // The topmost collection is the root's own field: the holder chain's first field, or
        // the list's when it stands in the root.
        let field = top_field(list);
        match root.get_mut(&field) {
            Some(DynValue::Collection(slot)) => *slot = items,
            _ => return Err("the list field is not a collection".into()),
        }
    }
    for (field, linked) in &shape.links {
        let Some(field) = field else { continue };
        let target = ResourceRef::root(root_id(linked), linked.root_ty.clone());
        let value = tilolang_runtime::value::to_json(&Value::some(Value::resource(&target)))?;
        match root.get_mut(field) {
            Some(DynValue::Register(slot)) => *slot = value,
            _ => return Err(format!("the field {field} does not hold a reference")),
        }
    }
    Ok(root)
}

/// Create the root of `shape` in `doc`, its collections empty, and the roots it links first.
pub(super) fn create_root(doc: &mut Doc, shape: &Shape, who: &str) -> Result<(), String> {
    let root = seeded_root(shape)?;
    let action = DynAction {
        app: shape.app.clone(),
        kind: format!("tl.{}.seed", shape.app),
        action_id: "$create".into(),
        args: Json::Null,
        authorship: Authorship::default(),
        edits: vec![DynEdit::Create {
            id: root_id(shape),
            record: tilo_resource::Model::to_json(&root),
        }],
    };
    execute(doc, &action, who)
}

/// Create the root of `shape`, seeded with `n` items, in `doc`: the roots it links first, and
/// the root's items `batch` an action.
fn seed(doc: &mut Doc, shape: &Shape, n: usize, who: &str, batch: usize) -> Result<(), String> {
    for (_, linked) in &shape.links {
        seed(doc, linked, n, who, batch)?;
    }
    create_root(doc, shape, who)?;
    let Some(list) = &shape.list else {
        return Ok(());
    };
    let items = seeded_items(list, n)?;
    let items: Vec<_> = items
        .iter()
        .map(|(id, item)| (id.clone(), item.clone()))
        .collect();
    add_in_runs(
        doc,
        shape,
        &list.field,
        &list.item_ty,
        holder_of(list).as_deref(),
        &items,
        who,
        batch,
    )
}

/// The id of the record the list's collection is inside: the deepest of its holders (a mail
/// account's conversations), or none when the root holds the collection itself.
pub(super) fn holder_of(list: &List) -> Option<String> {
    list.holders
        .last()
        .map(|_| holder_id(list.holders.len().saturating_sub(1)))
}

/// Commit `items` into the collection `field` of the record `parent` holds (`None` for the
/// root's own), each under the id it comes with, in the actions a change records whole: one
/// action of them all outgrows [`HISTORY_BUDGET`] past a few thousand items, and its change is
/// read as one whose action does not decode, a record of another kind than at a smaller size.
pub(super) fn add_in_runs(
    doc: &mut Doc,
    shape: &Shape,
    field: &str,
    kind: &str,
    parent: Option<&str>,
    items: &[(Id, DynRecord)],
    who: &str,
    batch: usize,
) -> Result<(), String> {
    let mut edits: Vec<DynEdit> = Vec::new();
    for (at, (id, item)) in items.iter().enumerate() {
        let record = tilo_resource::Model::to_json(item);
        let place = tilolang_runtime::place::Placement::At {
            place: tilolang_runtime::place::Place {
                parent: parent.map(str::to_owned),
                field: field.to_owned(),
                key: format!("{at:010}"),
            },
            was: None,
        };
        edits.push(DynEdit::Insert {
            id: root_id(shape),
            item: id.as_str().to_owned(),
            record,
            at: place.encode(),
        });
        if edits.len() >= batch.max(1) {
            commit(doc, shape, kind, &mut edits, who)?;
        }
    }
    commit(doc, shape, kind, &mut edits, who)
}

/// Commit the edits gathered so far as one action of the seed, leaving `edits` empty.
fn commit(
    doc: &mut Doc,
    shape: &Shape,
    kind: &str,
    edits: &mut Vec<DynEdit>,
    who: &str,
) -> Result<(), String> {
    if edits.is_empty() {
        return Ok(());
    }
    let action = DynAction {
        app: shape.app.clone(),
        kind: kind.to_owned(),
        action_id: "add".into(),
        args: Json::Null,
        authorship: Authorship::default(),
        edits: std::mem::take(edits),
    };
    execute(doc, &action, who)
}

/// How many items one action of the seed adds, as a person adds them: what a change records
/// whole holds them.
pub(super) const SEED_BATCH: usize = 256;

/// A workspace seeded with `n` items, all made by `who`: the record, ready for a host.
pub(super) fn seeded_workspace(
    shape: &Shape,
    n: usize,
    who: &str,
    batch: usize,
) -> Result<Doc, String> {
    let mut doc = Doc::new();
    seed(&mut doc, shape, n, who, batch)?;
    // A window opens a workspace it loads, from storage or a peer: the seeded record, saved and
    // loaded again.
    Doc::load(&doc.save()).map_err(|e| e.to_string())
}

/// The workspace record a window of the last of `packages` opens with `n` items, as the bench seeds
/// it: what a measurement of the real window installs as the workspace's document.
///
/// With no items it is the workspace before anything was made, whatever the root holds.
///
/// # Errors
/// The package's root has nothing to seed, or seeding fails.
pub fn seeded_record(packages: &[Compiled], n: usize) -> Result<Doc, String> {
    seeded_record_by(packages, n, "seed")
}

/// As [`seeded_record`], its items made by `who`: a workspace as the person `who` left it, so
/// that what another person changed of it afterwards is what they have not seen.
///
/// # Errors
///
/// As [`seeded_record`].
pub fn seeded_record_by(packages: &[Compiled], n: usize, who: &str) -> Result<Doc, String> {
    seeded(packages, n, who, SEED_BATCH)
}

/// As [`seeded_record`], every item added by one action, as an import adds them: past a few
/// thousand items it is more than a change records whole, and keeps its large parts as content.
///
/// # Errors
///
/// As [`seeded_record`].
pub fn seeded_record_in_one_action(packages: &[Compiled], n: usize) -> Result<Doc, String> {
    seeded(packages, n, "seed", n.max(1))
}

fn seeded(packages: &[Compiled], n: usize, who: &str, batch: usize) -> Result<Doc, String> {
    for compiled in packages {
        crate::meta::MetaExt::register_schemas(&compiled.meta);
    }
    if n == 0 {
        return Ok(Doc::new());
    }
    let main = packages.last().ok_or("no package")?;
    let others: Vec<&Meta> = packages
        .iter()
        .map(|p| &p.meta)
        .filter(|m| m.app != main.meta.app)
        .collect();
    seeded_workspace(&shape_of(&main.meta, &others)?, n, who, batch)
}

/// The workspace the last of `packages` seeds `n` items of, held as the chunks the store writes:
/// the record's own document ([`chunk::FIRST`]) names in its manifest the run each chunk of items
/// is, and the chunks above it hold the runs' items — [`chunk::ITEMS`] items an action, one
/// action an item, as a person adding them makes.
///
/// The items are those of the root's own collection. A collection inside holders (a mail
/// account's conversations) is seeded as one document, in the record.
///
/// # Errors
/// The package's root has nothing to seed, seeding fails, or the collection grows inside holders.
pub fn seeded_chunks(packages: &[Compiled], n: usize) -> Result<Chunks, String> {
    for compiled in packages {
        crate::meta::MetaExt::register_schemas(&compiled.meta);
    }
    let main = packages.last().ok_or("no package")?;
    let others: Vec<&Meta> = packages
        .iter()
        .map(|p| &p.meta)
        .filter(|m| m.app != main.meta.app)
        .collect();
    let shape = shape_of(&main.meta, &others)?;
    // The root and the roots it links, their collections empty: the items come as the action
    // that adds each does, one change an item in the chunk its run is in.
    let mut record = Doc::new();
    seed(&mut record, &shape, 0, "seed", SEED_BATCH)?;
    let mut chunks = Chunks::of(record);
    let Some(list) = &shape.list else {
        return Ok(chunks);
    };
    if !list.holders.is_empty() {
        return Err("the items of a collection inside holders are seeded as one document".into());
    }
    let items = seeded_items(list, n)?;
    let root = root_id(&shape);
    let field = list.field.clone();
    let per = usize::try_from(chunk::ITEMS).map_err(|e| e.to_string())?;
    for (at, (id, item)) in items.iter().enumerate() {
        let run = at
            .checked_div(per)
            .and_then(|run| run.checked_add(1))
            .ok_or("the items do not fit their chunks")?;
        let number = chunk::FIRST.saturating_add(u64::try_from(run).map_err(|e| e.to_string())?);
        let in_run = at.checked_rem(per).ok_or("a chunk holds no items")?;
        let key = format!("{at:010}");
        let at_place = tilolang_runtime::place::Placement::At {
            place: tilolang_runtime::place::Place {
                parent: None,
                field: field.clone(),
                key: key.clone(),
            },
            was: None,
        };
        let action = DynAction {
            app: shape.app.clone(),
            kind: list.item_ty.clone(),
            action_id: "add".into(),
            args: Json::Null,
            authorship: Authorship {
                at: Some(BENCH_TIME.to_owned()),
                ..Authorship::default()
            },
            edits: vec![
                // The run's entry in the record, which the insert into its chunk goes with.
                DynEdit::RunChunk {
                    holder: root.clone(),
                    field: field.clone(),
                    chunk: number,
                    last: key,
                    len: u64::try_from(in_run.saturating_add(1)).map_err(|e| e.to_string())?,
                },
                DynEdit::Insert {
                    id: root.clone(),
                    item: id.as_str().to_owned(),
                    record: tilo_resource::Model::to_json(item),
                    at: at_place.encode(),
                }
                .in_chunk(number),
            ],
        };
        for edit in &action.edits {
            let target = edit.chunk().unwrap_or(chunk::FIRST);
            let one = DynAction {
                edits: vec![edit.clone()],
                ..action.clone()
            };
            one.apply(chunks.open(target)).map_err(|e| e.to_string())?;
            chunks.open(target).commit();
        }
    }
    chunks.reread_placement();
    Ok(chunks)
}
