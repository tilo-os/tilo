//! Chat lists its conversations by their latest activity, the latest first, from the index's
//! versions: a new conversation and a post are a conversation's change; an edit of a message
//! inside it, and typing (each person's own draft), are not. Replicas holding the same changes
//! list them alike. A device's own actions count at once, before the record holds them.

use automerge::AutoCommit;
use serde_json::Value as Json;
use tilo_resource::{Edit, EditOp, Id, Segment};
use tilolang_runtime::meta::FieldKind;
use tilolang_runtime::store::{FieldSpec, Store as _};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{InvokeOutcome, Patch, Principal, UiNode, Value, ViewInstance};
use tilolang_tilo::action::{Authorship, DynAction, DynEdit};

use crate::fixtures::{Replica, field_id};

const CHAT: &str = include_str!("../../../../apps/tl/chat/app.tl");
const CHAT_IDS: &str = include_str!("../../../../apps/tl/chat/app.ids");
const APP: &str = "so.tilo.lang.chat";

fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    let meta = r.host.package(APP).unwrap().meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap()
        .id
        .clone();
    match r
        .host
        .runtime()
        .invoke_action(&Principal::new(&r.name), (id.as_str(), args))
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("{action}: {other:?}"),
    }
}

/// Run what `r` does next at `time`.
fn at(r: &Replica, time: &'static str) {
    r.host.runtime().set_clock(move || time.to_owned());
}

fn texts(node: &UiNode, out: &mut Vec<String>) {
    out.extend(node.props.values().filter_map(|value| match value {
        Value::Text(text) => Some(text.clone()),
        _ => None,
    }));
    for child in &node.children {
        texts(child, out);
    }
}

fn find<'a>(node: &'a UiNode, kind: &str) -> Option<&'a UiNode> {
    if node.kind == kind {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, kind))
}

/// The conversations Chat's window lists, in order, as its virtual list shows its rows: the
/// first of `titles` each row says.
fn listed(view: &mut ViewInstance, titles: &[&str]) -> Vec<String> {
    view.poll_patches().unwrap();
    let list = find(view.tree(), "ui.VirtualList")
        .expect("a list")
        .key
        .clone();
    let len = view.list_len(&list).unwrap_or(0);
    let mut out = Vec::new();
    for i in 0..len {
        if let Some(row) = view.row(&list, i).unwrap() {
            let mut said = Vec::new();
            texts(row, &mut said);
            out.extend(said.into_iter().find(|t| titles.contains(&t.as_str())));
        }
    }
    out
}

fn target(value: &Value) -> ResourceRef {
    match value {
        Value::Ref(r) => (*r.target).clone(),
        other => panic!("a resource: {other:?}"),
    }
}

/// `r` rewords the first part of the message `message`, an edit of the message alone.
fn reword(r: &mut Replica, message: &Value, text: &str) {
    let meta = r.host.package(APP).unwrap().meta;
    let message = target(message);
    let parts = field_id(&meta, "Message", "parts");
    let spec = FieldSpec {
        id: parts.clone(),
        kind: FieldKind::Collection,
    };
    let part = r.host.store().listing(&message, &spec).unwrap().ids()[0].clone();
    let mut path: Vec<Segment> = message
        .path
        .iter()
        .flat_map(|step| {
            [
                Segment::Field(step.field.clone()),
                Segment::Item(Id::from_raw(step.item.clone())),
            ]
        })
        .collect();
    path.extend([
        Segment::Field(parts),
        Segment::Item(Id::from_raw(part)),
        Segment::Field(field_id(&meta, "Part", "body")),
    ]);
    r.apply(&DynAction {
        app: APP.to_owned(),
        kind: format!("tl.{APP}.edit"),
        action_id: "$edit".to_owned(),
        args: Json::Null,
        authorship: Authorship::default(),
        edits: vec![DynEdit::Edit {
            id: message.doc,
            edit: Edit::new(
                path,
                EditOp::Replace {
                    text: text.to_owned(),
                },
            ),
        }],
    });
}

#[test]
fn conversations_are_listed_latest_activity_first_alike_on_every_replica() {
    let titles = ["Plans", "Books", "New chat"];
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    at(&ada, "2026-10-28T09:00:00Z");
    let plans = call(&ada, "start", vec![all.clone(), Value::text("Plans")]);
    ada.commit();
    at(&ada, "2026-10-28T09:10:00Z");
    let first = call(
        &ada,
        "post",
        vec![
            plans.clone(),
            Value::text("Walk on Saturday?"),
            Value::text(""),
        ],
    );
    ada.commit();
    at(&ada, "2026-10-28T09:20:00Z");
    let books = call(&ada, "start", vec![all.clone(), Value::text("Books")]);
    ada.commit();
    let mut ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
    ben.host.sync_from(&ben.doc);
    let mut ada_list = ada.view(APP);
    let mut ben_list = ben.view(APP);
    assert_eq!(listed(&mut ada_list, &titles), ["Books", "Plans"]);
    assert_eq!(listed(&mut ben_list, &titles), ["Books", "Plans"]);

    // Posting is a change of the conversation: Ben's post brings Plans first.
    at(&ben, "2026-10-28T09:30:00Z");
    call(&ben, "send", vec![plans, Value::text("Count me in")]);
    ben.commit();
    assert_eq!(listed(&mut ben_list, &titles), ["Plans", "Books"]);

    // At the same time Ada starts a conversation, later than Ben's post: it comes first.
    at(&ada, "2026-10-28T09:40:00Z");
    call(&ada, "new_chat", vec![all]);
    ada.commit();
    assert_eq!(
        listed(&mut ada_list, &titles),
        ["New chat", "Books", "Plans"]
    );
    ada.sync_with(&mut ben);
    let want = ["New chat", "Plans", "Books"];
    assert_eq!(listed(&mut ada_list, &titles), want);
    assert_eq!(listed(&mut ben_list, &titles), want);

    // Editing an old message changes the message, not its conversation: Plans stays where it
    // is, though the edit is the room's latest change.
    reword(&mut ada, &first, "Walk on Sunday?");
    ada.sync_with(&mut ben);
    assert_eq!(listed(&mut ada_list, &titles), want);
    assert_eq!(listed(&mut ben_list, &titles), want);

    // Typing is the person's own draft: nothing is written, nothing moves.
    let thread = ben.host.open_resource(&target(&books), None).unwrap();
    let input = find(thread.tree(), "ui.TextInput")
        .expect("the composer")
        .key
        .clone();
    thread
        .edit(&input, &Value::text("Halfway through"))
        .unwrap();
    assert!(
        ben.host.take_committed().is_empty(),
        "a draft is not a change"
    );
    assert_eq!(listed(&mut ben_list, &titles), want);

    // Sending it is a post: Books comes first, on both.
    at(&ben, "2026-10-28T09:50:00Z");
    call(&ben, "send", vec![books, Value::text("Halfway through")]);
    ben.commit();
    ada.sync_with(&mut ben);
    let want = ["Books", "New chat", "Plans"];
    assert_eq!(listed(&mut ada_list, &titles), want);
    assert_eq!(listed(&mut ben_list, &titles), want);
}

#[test]
fn a_conversation_started_offline_is_first_at_once_and_stays_first_once_synced() {
    let titles = ["Plans", "Books", "Trip"];
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    at(&ada, "2026-10-28T09:00:00Z");
    call(&ada, "start", vec![all.clone(), Value::text("Plans")]);
    ada.commit();
    at(&ada, "2026-10-28T09:10:00Z");
    call(&ada, "start", vec![all.clone(), Value::text("Books")]);
    ada.commit();
    let mut ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
    ben.host.sync_from(&ben.doc);
    let mut ada_list = ada.view(APP);
    let mut ben_list = ben.view(APP);
    assert_eq!(listed(&mut ada_list, &titles), ["Books", "Plans"]);

    // Offline: the runtime takes both actions and the record holds neither yet. Their versions
    // are provisional, the invoker's at the times the actions were invoked: the new
    // conversation is first at once, and its post keeps it there.
    at(&ada, "2026-10-28T09:20:00Z");
    let trip = call(&ada, "start", vec![all, Value::text("Trip")]);
    let want = ["Trip", "Books", "Plans"];
    assert_eq!(listed(&mut ada_list, &titles), want);
    at(&ada, "2026-10-28T09:30:00Z");
    call(&ada, "send", vec![trip, Value::text("Who is in?")]);
    assert_eq!(listed(&mut ada_list, &titles), want);

    // Back online: the record's versions replace the provisional ones. They carry the same
    // times, so nothing moves.
    ada.commit();
    ada.sync_with(&mut ben);
    let patches = ada_list.poll_patches().unwrap();
    assert!(
        !patches.iter().any(|p| matches!(p, Patch::Move { .. })),
        "no row moves once the record holds the actions: {patches:?}"
    );
    assert_eq!(listed(&mut ada_list, &titles), want);
    assert_eq!(listed(&mut ben_list, &titles), want);
}
