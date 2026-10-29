//! The transcript scenarios: one item of the package's list holding N items of its own (a
//! conversation of N messages), shown in the item's window as a transcript shows it: its last
//! page of rows, the rest rendered as they scroll into view.
//!
//! * `thread_open`: opening the item's window and rendering its last page of rows;
//! * `thread_keystroke`: one character typed into the window's first text input (the composer);
//! * `thread_post`: another replica appends a row (a message), which the transcript shows;
//! * `thread_send`: a message typed into the composer is sent with the button beside it, and
//!   the transcript shows it;
//! * `thread_scroll`: the transcript shows the page of rows before the one it shows.

use std::collections::HashSet;
use std::time::Instant;

use serde_json::{Value as Json, json};
use tilo_automerge::ApplyIncoming as _;
use tilo_resource::{Collection, Id};
use tilolang::Compiled;
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_runtime::{Principal, UiNode, ViewInstance};

use super::{
    Changes, Doc, List, Nested, PAGE, Record, Room, SCROLL_STEP, Shape, Sized, Work,
    composer_input, execute, frame_list, item_id, ms, root_id, text_input, typed, with_work,
};
use crate::action::{Authorship, DynAction, DynEdit};
use crate::record::{DynRecord, DynValue};
use crate::schema::{MergeKind, lookup};

/// The id of the one item of the root's list: the thread whose transcript the scenarios show.
fn thread_id() -> String {
    "thread".to_owned()
}

/// The fields of the packages' types that hold a principal (a message's author), by type and
/// field id: a register a seeded record has no default for.
pub(super) type Principals = HashSet<(String, String)>;

pub(super) fn principals(packages: &[Compiled]) -> Principals {
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

/// A record of type `ty` with `label` in every text and rich text field, a principal of its own in every
/// field that holds one, and, `depth` collections down, one item in each collection (a
/// message's text part, when it has no rich text), whose id is the record's `id` and `-part`.
pub(super) fn filled(
    (id, ty): (&str, &str),
    label: &str,
    depth: usize,
    principals: &Principals,
) -> Result<DynRecord, String> {
    let schema = lookup(ty).ok_or_else(|| format!("unregistered type {ty}"))?;
    let mut record = DynRecord::empty(schema).map_err(|e| e.to_string())?;
    let author = tilolang_runtime::value::to_json(&Value::Principal(Principal::new("bench")))?;
    // What a record says in rich text it says there alone: a message with a rich text body
    // has no parts.
    let rich = schema.fields.iter().any(|f| f.kind == MergeKind::RichText);
    for field in &schema.fields {
        let principal = principals.contains(&(ty.to_owned(), field.id.clone()));
        match (field.kind, record.get_mut(&field.id)) {
            (MergeKind::Text, Some(DynValue::Text(text))) => label.clone_into(text),
            (MergeKind::RichText, Some(DynValue::RichText(rich))) => {
                **rich = tilo_document::RichText::from_markdown(label);
            }
            (MergeKind::Register, Some(DynValue::Register(slot))) if principal => {
                slot.clone_from(&author);
            }
            (MergeKind::Collection, Some(DynValue::Collection(slot))) if depth > 0 && !rich => {
                if let Some(inner) = field.record.as_deref() {
                    let mut items = Collection::default();
                    let part = format!("{id}-part");
                    items.push(
                        Id::from_raw(&part),
                        filled((&part, inner), label, depth.saturating_sub(1), principals)?,
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
    let mut item = filled((&thread_id(), &list.item_ty), "Item 0", 0, &principals)?;
    let mut inner = Collection::default();
    for i in 0..n {
        inner.push(
            Id::from_raw(item_id(i)),
            filled(
                (&item_id(i), &nested.item_ty),
                &format!("Message {i}"),
                1,
                &principals,
            )?,
        );
    }
    match item.get_mut(&nested.field) {
        Some(DynValue::Collection(slot)) => *slot = inner,
        _ => return Err("the item's collection is not one".into()),
    }
    let mut items = Collection::default();
    items.push(Id::from_raw(thread_id()), item);
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
        let list = frame_list(view.tree()).ok_or("the item's window shows no list")?;
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
    let item = ResourceRef::root(root_id(shape), shape.root_ty.clone())
        .item(&thread_id(), list.item_ty.clone());
    let mut out = vec![open(sized, &item)?];
    if sized.config.open_only {
        return Ok(out);
    }
    let mut room = sized.open()?;
    room.settle();
    let mut thread = Thread::open(&room, &item)?;
    room.frame(&mut [&mut thread.view])?;
    room.phases.take();
    out.push(keystroke(sized, &mut room, &mut thread)?);
    out.push(post(sized, nested, &mut room, &mut thread)?);
    out.extend(send(sized, &mut room, &mut thread)?);
    out.push(scroll(sized, &room, &mut thread)?);
    Ok(out)
}

/// Opening the item's window and rendering its last page.
fn open(sized: &Sized<'_>, item: &ResourceRef) -> Result<Record, String> {
    let mut samples = Vec::new();
    let mut nodes = 0;
    let mut work = None;
    for _ in 0..sized.config.samples {
        let room = sized.open()?;
        let before = Work::of(&room, &[]);
        let start = Instant::now();
        let thread = Thread::open(&room, item)?;
        samples.push(ms(start.elapsed()));
        nodes = thread.view.tree().count();
        let mut opened = Work::of(&room, &[&thread.view]).since(before);
        // The window's own boundaries: the room's window rendered none of them.
        opened.renders = thread.view.stats().renders;
        work.get_or_insert(opened);
    }
    let extra = json!({"nodes": nodes, "shown": PAGE});
    Ok(sized.record(
        "thread_open",
        "ms",
        samples,
        with_work(extra, work.unwrap_or_default()),
    ))
}

/// Typing into the window's first text input, one letter a frame.
fn keystroke(sized: &Sized<'_>, room: &mut Room, thread: &mut Thread) -> Result<Record, String> {
    let (input, mut text) = text_input(&room.host, thread.view.tree())
        .or_else(|| composer_input(&thread.view, thread.view.tree()))
        .ok_or("the item's window has no text input")?;
    let mut changes = Changes::default();
    for _ in 0..sized.config.samples {
        text.push('x');
        let value = typed(&thread.view, &input, &text);
        let before = Work::of(room, &[&thread.view]);
        let start = Instant::now();
        thread
            .view
            .edit(&input, &value)
            .map_err(|e| e.to_string())?;
        let patches = room.frame(&mut [&mut thread.view])?;
        let took = ms(start.elapsed());
        let mut done = Work::of(room, &[&thread.view]).since(before);
        done.patches = patches as u64;
        changes.push(took, done);
    }
    let (samples, extra) = changes.into_record(room.phases.take());
    Ok(sized.record("thread_keystroke", "ms", samples, extra))
}

/// Another replica appending a row, which the transcript, following its end, shows.
fn post(
    sized: &Sized<'_>,
    nested: &Nested,
    room: &mut Room,
    thread: &mut Thread,
) -> Result<Record, String> {
    let shape = sized.shape;
    let principals = principals(sized.packages);
    let mut peer = room.doc.fork();
    let mut changes = Changes::default();
    for sample in 0..sized.config.samples {
        let id = format!("peer-{sample:06}");
        let message = filled(
            (&id, &nested.item_ty),
            &format!("Peer message {sample}"),
            1,
            &principals,
        )?;
        // After the seeded messages and the peer's earlier ones.
        let key = crate::action::keys_in_order(sized.n.saturating_add(sample).saturating_add(1))
            .pop()
            .unwrap_or_default();
        let at = tilolang_runtime::place::Placement::At {
            place: tilolang_runtime::place::Place {
                parent: Some(thread_id()),
                field: nested.field.clone(),
                key,
            },
            was: None,
        };
        let action = DynAction {
            app: shape.app.clone(),
            kind: format!("tl.{}.edit", shape.app),
            action_id: "$edit".into(),
            args: Json::Null,
            authorship: Authorship::default(),
            edits: vec![DynEdit::Insert {
                id: root_id(shape),
                item: id,
                record: tilo_resource::Model::to_json(&message),
                at: at.encode(),
            }],
        };
        execute(&mut peer, &action, "peer")?;
        let before = Work::of(room, &[&thread.view]);
        let start = Instant::now();
        room.doc
            .merge_incoming(&mut peer)
            .map_err(|e| e.to_string())?;
        let patches = room.frame(&mut [&mut thread.view])?;
        // The transcript follows its end: the new last row comes into view.
        let len = thread.view.list_len(&thread.list).unwrap_or(0);
        thread.show(len.saturating_sub(PAGE)..len)?;
        let took = ms(start.elapsed());
        let mut done = Work::of(room, &[&thread.view]).since(before);
        done.patches = patches as u64;
        changes.push(took, done);
    }
    let (samples, extra) = changes.into_record(room.phases.take());
    Ok(sized.record("thread_post", "ms", samples, extra))
}

/// The key of the button beside the input `input` in `node`: what sends what the composer
/// holds.
fn button_beside(node: &UiNode, input: &str) -> Option<String> {
    if let Some(at) = node.children.iter().position(|c| c.key == input) {
        return node
            .children
            .iter()
            .skip(at)
            .find(|c| c.kind == "ui.Button")
            .map(|c| c.key.clone());
    }
    node.children.iter().find_map(|c| button_beside(c, input))
}

/// Typing a message into the composer and sending it with the button beside it: the message
/// is posted, and the transcript, following its end, shows it. Only for a window whose composer
/// has a send button.
fn send(sized: &Sized<'_>, room: &mut Room, thread: &mut Thread) -> Result<Option<Record>, String> {
    let Some((input, _)) = composer_input(&thread.view, thread.view.tree()) else {
        return Ok(None);
    };
    let Some(button) = button_beside(thread.view.tree(), &input) else {
        return Ok(None);
    };
    let mut changes = Changes::default();
    for sample in 0..sized.config.samples {
        let value = typed(&thread.view, &input, &format!("Sent message {sample}"));
        thread
            .view
            .edit(&input, &value)
            .map_err(|e| e.to_string())?;
        room.frame(&mut [&mut thread.view])?;
        let before = Work::of(room, &[&thread.view]);
        let start = Instant::now();
        thread.view.press(&button).map_err(|e| e.to_string())?;
        let patches = room.frame(&mut [&mut thread.view])?;
        let len = thread.view.list_len(&thread.list).unwrap_or(0);
        thread.show(len.saturating_sub(PAGE)..len)?;
        let took = ms(start.elapsed());
        let mut done = Work::of(room, &[&thread.view]).since(before);
        done.patches = patches as u64;
        changes.push(took, done);
    }
    let (samples, extra) = changes.into_record(room.phases.take());
    Ok(Some(sized.record("thread_send", "ms", samples, extra)))
}

/// The transcript scrolled back a few rows a frame.
fn scroll(sized: &Sized<'_>, room: &Room, thread: &mut Thread) -> Result<Record, String> {
    let mut samples = Vec::new();
    let mut work = Work::default();
    for _ in 0..sized.config.samples {
        let end = match thread.shown.start.checked_sub(SCROLL_STEP) {
            Some(start) if start > 0 => thread.shown.end.saturating_sub(SCROLL_STEP),
            _ => thread.view.list_len(&thread.list).unwrap_or(0),
        };
        let before = Work::of(room, &[&thread.view]);
        let start = Instant::now();
        thread.show(end.saturating_sub(PAGE)..end)?;
        samples.push(ms(start.elapsed()));
        work = work.max(Work::of(room, &[&thread.view]).since(before));
    }
    let extra = json!({"rows_per_frame": SCROLL_STEP, "shown": PAGE});
    Ok(sized.record("thread_scroll", "ms", samples, with_work(extra, work)))
}
