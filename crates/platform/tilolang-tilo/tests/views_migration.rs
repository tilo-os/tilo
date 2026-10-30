//! Views kept its answers in the workspace's record, one `kept` for everyone; it now keeps each
//! person's views in their own record (`records: .own`). A person's own record, opened for the
//! first time on a device that holds the workspace's record of Views, starts as that record
//! (`collaboration::record::save::open_stored_adopting`): every answer they saw, kept ones kept,
//! under the ids it had, drawn as the view it was. The workspace's record is left as it was.

use std::rc::Rc;

use automerge::AutoCommit;
use collaboration::record::save::{open_stored_adopting, record_document, room_document};
use durable_store::{DurableStore, MemoryStore};
use tilolang_runtime::{InvokeOutcome, Principal, Value};

use crate::fixtures::Replica;

const VIEWS: &str = "so.tilo.lang.views";
const ROOM: &str = "7f0c2b9e-4a51-4c1e-9d6b-3a2f1e0d9c8b";
const ADA: &str = "abababababababababababababababababababababababababababababababab";

macro_rules! app {
    ($name:literal) => {
        (
            include_str!(concat!("../../../../apps/tl/", $name, "/app.tl")),
            include_str!(concat!("../../../../apps/tl/", $name, "/app.ids")),
        )
    };
}

/// Views as it was: answers in the workspace's record, over four apps it named.
const BEFORE: (&str, &str) = (
    include_str!("legacy/views/app.tl"),
    include_str!("legacy/views/app.ids"),
);

const RELEASE: &str = "card(tasks.tasks, count: tasks.tasks.items.count(t -> not t.done)) {\n    list(tasks.tasks.items.filter(t -> not t.done)) { t ->\n        row(t)\n    }\n}";

fn invoke(r: &Replica, who: &str, action: &str, args: Vec<Value>) -> Value {
    let meta = r.host.package(VIEWS).unwrap().meta;
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
        .invoke_action(&Principal::new(who), (id.as_str(), args))
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("{action}: {other:?}"),
    }
}

fn reference(v: &Value) -> tilolang_runtime::ResourceRef {
    match v {
        Value::Ref(r) => (*r.target).clone(),
        other => panic!("not a resource: {other:?}"),
    }
}

fn of_kind<'a>(
    node: &'a tilolang_runtime::UiNode,
    kind: &str,
) -> Vec<&'a tilolang_runtime::UiNode> {
    let mut out = Vec::new();
    if node.kind == kind {
        out.push(node);
    }
    for child in &node.children {
        out.extend(of_kind(child, kind));
    }
    out
}

#[test]
fn a_kept_answer_moves_into_its_keepers_own_views_with_its_id() {
    // The workspace's record of Views as it was: an agent answered, the answer was kept.
    let (mut before, _) = Replica::new(
        "ada",
        AutoCommit::new(),
        &[
            app!("board"),
            app!("tasks"),
            app!("chat"),
            app!("notes"),
            BEFORE,
        ],
    );
    let answers = before.host.root_of(VIEWS).unwrap();
    before.commit();
    let answer = invoke(
        &before,
        "agent-task:release",
        "answer",
        vec![
            Value::resource(&answers),
            Value::text("Left for the release"),
            Value::text("What's left for the release?"),
            Value::text(RELEASE),
        ],
    );
    before.commit();
    invoke(&before, "ada", "keep", vec![answer.clone()]);
    before.commit();
    let kept = reference(&answer);

    // This device's copy of it, where a room this device governs keeps it.
    let documents: Rc<dyn DurableStore> = Rc::new(MemoryStore::new());
    let mut workspace = room_document(&documents, VIEWS, ROOM).unwrap();
    workspace.save(&mut before.doc).unwrap();

    // Ada's own record of Views has no copy here yet: it starts as the workspace's record.
    let own = tilolang_tilo::member_record_of(ADA).unwrap();
    let key = collaboration::record::RecordId::parse(&own)
        .unwrap()
        .storage_key();
    let (doc, _writer) = open_stored_adopting(
        record_document(&documents, VIEWS, ROOM, &key).unwrap(),
        Some(room_document(&documents, VIEWS, ROOM).unwrap()),
    );
    let (after, _) = Replica::new("ada", doc, &[app!("tasks"), app!("chat"), app!("views")]);

    // The answer is hers, under the id it had, kept, written by the task that wrote it.
    let root = after.host.root_of(VIEWS).unwrap();
    assert_eq!(root.doc, answers.doc, "the root keeps its id");
    let rt = after.host.runtime();
    let Value::List(items) = rt.read(&root, "items").unwrap() else {
        panic!("items is a list");
    };
    let items: Vec<_> = items.iter().map(reference).collect();
    assert_eq!(items, vec![kept.clone()], "the answer keeps its id");
    assert_eq!(rt.read(&kept, "kept").unwrap(), Value::Bool(true));
    assert_eq!(
        rt.read(&kept, "definition").unwrap(),
        Value::text(RELEASE),
        "what was written is what it is"
    );
    assert_eq!(
        rt.read(&kept, "author").unwrap(),
        Value::Principal(Principal::new("agent-task:release"))
    );
    // Drawn as the view it was: a body of a view that takes nothing.
    let meta = after.host.package(VIEWS).unwrap().meta;
    let page = meta
        .views
        .iter()
        .find(|v| v.name == "view_page")
        .unwrap()
        .id
        .clone();
    let view = rt
        .open_view(&Principal::new("ada"), &page, vec![Value::resource(&kept)])
        .unwrap();
    let embed = of_kind(view.tree(), "embed");
    assert_eq!(
        embed[0].props.get("state"),
        Some(&Value::text("view")),
        "{:#?}",
        view.tree()
    );
}
