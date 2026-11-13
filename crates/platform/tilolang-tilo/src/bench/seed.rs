//! The workspaces the bench measures, seeded: the root of a package and its items, made as
//! people make them, through the executor.

use serde_json::Value as Json;
use tilo_actions::{Action as _, ActionContext, ActionInvoker};
use tilolang::Compiled;
use tilolang_runtime::meta::Meta;

use super::{BENCH_TIME, Doc, Shape, seeded_items, seeded_root, shape_of};
use crate::action::{Authorship, DynAction, DynEdit};
use crate::chunk::{self, Chunks};

/// Commit `action` on `doc` through the executor, as a window's host does: recorded at
/// [`BENCH_TIME`] unless it says when it was made, so what the windows show of when things
/// changed is the same in every run.
pub(super) fn execute(doc: &mut Doc, action: &DynAction, who: &str) -> Result<(), String> {
    let mut action = action.clone();
    if action.authorship.at.is_none() {
        action.authorship.at = Some(BENCH_TIME.to_owned());
    }
    let action = &action;
    let context = ActionContext::for_resource(
        action.resources().first().copied().unwrap_or("workspace"),
        &action.kind,
        &action.kind,
    )
    .with_app("so.tilo.lang", "tilolang")
    .with_invoker(ActionInvoker::actor(who.to_owned()));
    tilo_actions::apply_local_with_context(
        doc,
        action,
        action.encode().map_err(|e| e.to_string())?,
        Some(who.as_bytes()),
        context,
    )
    .map_err(|e| e.to_string())?;
    doc.commit();
    Ok(())
}

/// Create the root of `shape`, seeded with `n` items, in `doc`: the roots it links first. The
/// root's own items come as adding them does, `batch` an action.
pub(super) fn seed(
    doc: &mut Doc,
    shape: &Shape,
    n: usize,
    who: &str,
    batch: usize,
) -> Result<(), String> {
    for (_, linked) in &shape.links {
        seed(doc, linked, n, who, batch)?;
    }
    // A collection inside holders comes with the root.
    let added = shape.list.as_ref().filter(|list| list.holders.is_empty());
    let root = seeded_root(shape, if added.is_some() { 0 } else { n })?;
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
    execute(doc, &action, who)?;
    let Some(list) = added else {
        return Ok(());
    };
    let items = seeded_items(list, n)?;
    let items: Vec<_> = items.iter().collect();
    for (at, run) in items.chunks(batch.max(1)).enumerate() {
        let first = at.saturating_mul(batch);
        let edits = run
            .iter()
            .enumerate()
            .map(|(k, (id, item))| {
                let place = tilolang_runtime::place::Placement::At {
                    place: tilolang_runtime::place::Place {
                        parent: None,
                        field: list.field.clone(),
                        key: format!("{:010}", first.saturating_add(k)),
                    },
                    was: None,
                };
                DynEdit::Insert {
                    id: root_id(shape),
                    item: id.as_str().to_owned(),
                    record: tilo_resource::Model::to_json(*item),
                    at: place.encode(),
                }
            })
            .collect();
        let action = DynAction {
            app: shape.app.clone(),
            kind: list.item_ty.clone(),
            action_id: "add".into(),
            args: Json::Null,
            authorship: Authorship::default(),
            edits,
        };
        execute(doc, &action, who)?;
    }
    Ok(())
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
    seeded(packages, n, "seed", n)
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

pub(super) fn root_id(shape: &Shape) -> String {
    format!("root-{}", shape.root_ty)
}
