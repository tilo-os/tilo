//! The transcript scenarios: one item of the package's list holding N items of its own (a
//! conversation of N messages), shown in the item's window as a transcript shows it: its last
//! page of rows, the rest rendered as they scroll into view.
//!
//! * `thread_open`: opening the item's window and rendering its last page of rows;
//! * `thread_keystroke`: one character typed into the window's first text input (the draft);
//! * `thread_post`: another replica appends a row (a message), which the transcript shows;
//! * `thread_scroll`: the transcript shows the page of rows before the one it shows.

use std::collections::HashSet;
use std::time::Instant;

use serde_json::{Value as Json, json};
use tilo_automerge::ApplyIncoming as _;
use tilo_resource::{Collection, Edit, EditOp, Id, Segment};
use tilolang::Compiled;
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_runtime::{Principal, ViewInstance};

use super::{
    Doc, List, Nested, PAGE, Record, Room, SCROLL_STEP, Shape, Sized, content_lists, execute,
    item_id, ms, root_id, text_input,
};
use crate::action::{Authorship, DynAction, DynEdit};
use crate::record::{DynRecord, DynValue};
use crate::schema::{MergeKind, lookup};

/// The fields of the packages' types that hold a principal (a message's author), by type and
/// field id: a register a seeded record has no default for.
type Principals = HashSet<(String, String)>;

fn principals(packages: &[Compiled]) -> Principals {
    packages
        .iter()
        .flat_map(|package| &package.meta.types)
        .flat_map(|ty| {
            ty.fields
                .iter()
                .filter(|field| field.ty == "Principal")
                .map(|field| (ty.id.clone(), field.id.clone()))
        })
        .collect()
}

/// A record of type `ty` with `label` in every text field, a principal of its own in every
/// field that holds one, and, `depth` collections down, one item in each collection (a
/// message's text part).
fn filled(
    ty: &str,
    label: &str,
    depth: usize,
    principals: &Principals,
) -> Result<DynRecord, String> {
    let schema = lookup(ty).ok_or_else(|| format!("unregistered type {ty}"))?;
    let mut record = DynRecord::empty(schema).map_err(|e| e.to_string())?;
    let author = tilolang_runtime::value::to_json(&Value::Principal(Principal::new("bench")))?;
    for field in &schema.fields {
        let principal = principals.contains(&(ty.to_owned(), field.id.clone()));
        match (field.kind, record.get_mut(&field.id)) {
            (MergeKind::Text, Some(DynValue::Text(text))) => label.clone_into(text),
            (MergeKind::Register, Some(DynValue::Register(slot))) if principal => {
                slot.clone_from(&author);
            }
            (MergeKind::Collection, Some(DynValue::Collection(slot))) if depth > 0 => {
                if let Some(inner) = field.record.as_deref() {
                    let mut items = Collection::default();
                    items.push(
                        Id::from_raw("part-0"),
                        filled(inner, label, depth.saturating_sub(1), principals)?,
                    );
                    *slot = items;
                }
            }
            _ => {}
        }
    }
    Ok(record)
}

/// A room whose root's list holds one item, holding `n` items of its own.
///
/// # Errors
/// A type of the package is not registered, or the seed does not apply.
pub(super) fn seeded(
    packages: &[Compiled],
    shape: &Shape,
    list: &List,
    nested: &Nested,
    n: usize,
) -> Result<Doc, String> {
    let principals = principals(packages);
    let root_schema = lookup(&shape.root_ty).ok_or("unregistered root type")?;
    let mut root = DynRecord::empty(root_schema).map_err(|e| e.to_string())?;
    let mut item = filled(&list.item_ty, "Item 0", 0, &principals)?;
    let mut inner = Collection::default();
    for i in 0..n {
        inner.push(
            Id::from_raw(item_id(i)),
            filled(&nested.item_ty, &format!("Message {i}"), 1, &principals)?,
        );
    }
    match item.get_mut(&nested.field) {
        Some(DynValue::Collection(slot)) => *slot = inner,
        _ => return Err("the item's collection is not one".into()),
    }
    let mut items = Collection::default();
    items.push(Id::from_raw(item_id(0)), item);
    match root.get_mut(&list.field) {
        Some(DynValue::Collection(slot)) => *slot = items,
        _ => return Err("the list field is not a collection".into()),
    }
    let mut doc = Doc::new();
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
    execute(&mut doc, &action, "seed")?;
    Doc::load(&doc.save()).map_err(|e| e.to_string())
}

/// The item's window, its transcript's list, and the rows it shows.
struct Thread {
    view: ViewInstance,
    list: String,
    shown: std::ops::Range<usize>,
}

impl Thread {
    /// Open `item`'s window in `room` and render its transcript's last page of rows, as the
    /// transcript does when it opens at its end.
    fn open(room: &Room, item: &ResourceRef) -> Result<Self, String> {
        let mut view = room
            .host
            .open_resource(item, None)
            .map_err(|e| e.to_string())?;
        let mut lists = Vec::new();
        content_lists(view.tree(), &mut lists);
        let list = lists
            .into_iter()
            .next()
            .ok_or("the item's window shows no list")?;
        let len = view.list_len(&list).unwrap_or(0);
        let shown = len.saturating_sub(PAGE)..len;
        for i in shown.clone() {
            view.row(&list, i).map_err(|e| e.to_string())?;
        }
        Ok(Self { view, list, shown })
    }

    /// Show `range` instead of the rows shown.
    fn show(&mut self, range: std::ops::Range<usize>) -> Result<(), String> {
        self.view.hide_rows(&self.list, range.clone());
        for i in range.clone() {
            self.view.row(&self.list, i).map_err(|e| e.to_string())?;
        }
        self.shown = range;
        Ok(())
    }
}

/// Run the transcript scenarios on `sized`, whose room [`seeded`] made.
///
/// # Errors
/// A scenario fails.
pub(super) fn run(sized: &Sized<'_>, list: &List, nested: &Nested) -> Result<Vec<Record>, String> {
    let shape = sized.shape;
    let item = ResourceRef::root(root_id(shape), shape.root_ty.clone()).item(
        &list.field,
        &item_id(0),
        list.item_ty.clone(),
    );
    let mut out = vec![open(sized, &item)?];
    let mut room = sized.open()?;
    room.settle();
    let mut thread = Thread::open(&room, &item)?;
    room.frame(&mut [&mut thread.view])?;
    room.phases.take();
    out.push(keystroke(sized, &mut room, &mut thread)?);
    out.push(post(sized, (list, nested), &mut room, &mut thread)?);
    out.push(scroll(sized, &mut thread)?);
    Ok(out)
}

/// Opening the item's window and rendering its last page.
fn open(sized: &Sized<'_>, item: &ResourceRef) -> Result<Record, String> {
    let mut samples = Vec::new();
    let mut nodes = 0;
    for _ in 0..sized.config.samples {
        let room = sized.open()?;
        let start = Instant::now();
        let thread = Thread::open(&room, item)?;
        samples.push(ms(start.elapsed()));
        nodes = thread.view.tree().count();
    }
    Ok(sized.record(
        "thread_open",
        "ms",
        samples,
        json!({"nodes": nodes, "shown": PAGE}),
    ))
}

/// Typing into the window's first text input, one letter a frame.
fn keystroke(sized: &Sized<'_>, room: &mut Room, thread: &mut Thread) -> Result<Record, String> {
    let (input, mut text) =
        text_input(&room.host, thread.view.tree()).ok_or("the item's window has no text input")?;
    let mut samples = Vec::new();
    for _ in 0..sized.config.samples {
        text.push('x');
        let start = Instant::now();
        thread
            .view
            .edit(&input, &Value::text(text.clone()))
            .map_err(|e| e.to_string())?;
        room.frame(&mut [&mut thread.view])?;
        samples.push(ms(start.elapsed()));
    }
    Ok(sized.record("thread_keystroke", "ms", samples, room.phases.take()))
}

/// Another replica appending a row, which the transcript, following its end, shows.
fn post(
    sized: &Sized<'_>,
    (list, nested): (&List, &Nested),
    room: &mut Room,
    thread: &mut Thread,
) -> Result<Record, String> {
    let shape = sized.shape;
    let principals = principals(sized.packages);
    let mut peer = room.doc.fork();
    let mut samples = Vec::new();
    for sample in 0..sized.config.samples {
        let message = filled(
            &nested.item_ty,
            &format!("Peer message {sample}"),
            1,
            &principals,
        )?;
        let action = DynAction {
            app: shape.app.clone(),
            kind: format!("tl.{}.edit", shape.app),
            action_id: "$edit".into(),
            args: Json::Null,
            authorship: Authorship::default(),
            edits: vec![DynEdit::Edit {
                id: root_id(shape),
                edit: Edit::new(
                    vec![
                        Segment::Field(list.field.clone()),
                        Segment::Item(Id::from_raw(item_id(0))),
                        Segment::Field(nested.field.clone()),
                    ],
                    EditOp::InsertItem {
                        id: Some(Id::from_raw(format!("peer-{sample:06}"))),
                        at: sized.n.saturating_add(sample),
                        value: tilo_resource::Model::to_json(&message),
                    },
                ),
            }],
        };
        execute(&mut peer, &action, "peer")?;
        let start = Instant::now();
        room.doc
            .merge_incoming(&mut peer)
            .map_err(|e| e.to_string())?;
        room.frame(&mut [&mut thread.view])?;
        // The transcript follows its end: the new last row comes into view.
        let len = thread.view.list_len(&thread.list).unwrap_or(0);
        thread.show(len.saturating_sub(PAGE)..len)?;
        samples.push(ms(start.elapsed()));
    }
    Ok(sized.record("thread_post", "ms", samples, room.phases.take()))
}

/// The transcript scrolled back a few rows a frame.
fn scroll(sized: &Sized<'_>, thread: &mut Thread) -> Result<Record, String> {
    let mut samples = Vec::new();
    for _ in 0..sized.config.samples {
        let end = match thread.shown.start.checked_sub(SCROLL_STEP) {
            Some(start) if start > 0 => thread.shown.end.saturating_sub(SCROLL_STEP),
            _ => thread.view.list_len(&thread.list).unwrap_or(0),
        };
        let start = Instant::now();
        thread.show(end.saturating_sub(PAGE)..end)?;
        samples.push(ms(start.elapsed()));
    }
    Ok(sized.record(
        "thread_scroll",
        "ms",
        samples,
        json!({"rows_per_frame": SCROLL_STEP, "shown": PAGE}),
    ))
}
