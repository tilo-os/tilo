//! "Make from this": a task made from a chat message through the message's own menu, the title
//! it takes from what the message says, the link each way afterwards, and what the two views
//! show of it: the task's row naming the message it came from, the message drawing the task in
//! its own row view.

use automerge::AutoCommit;
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::store::{LinkHow, Store as _};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::view::BindTarget;
use tilolang_runtime::{InvokeOutcome, Principal, UiNode, Value, ViewInstance};

use crate::fixtures::{Replica, target};

const CHAT: &str = include_str!("../../../../apps/tl/chat/app.tl");
const CHAT_IDS: &str = include_str!("../../../../apps/tl/chat/app.ids");
const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");
const DISCUSSION_IDS: &str = include_str!("../../../../apps/tl/discussion/app.ids");
const TASKS: &str = crate::fixtures::TASKS_SOURCE;
const TASKS_IDS: &str = include_str!("../../../../apps/tl/tasks/app.ids");
const CHAT_APP: &str = "so.tilo.lang.chat";
const TASKS_APP: &str = "so.tilo.lang.tasks";

/// What Bo writes, and what the task made from it is called.
const SAID: &str = "can someone book the van for Saturday?";

fn call(r: &Replica, app: &str, action: &str, args: Vec<Value>) -> Value {
    let meta = r.host.package(app).unwrap().meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("no action {action}"))
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

fn find<'a>(node: &'a UiNode, kind: &str) -> Option<&'a UiNode> {
    if node.kind == kind {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, kind))
}

fn find_all<'a>(node: &'a UiNode, kind: &str, out: &mut Vec<&'a UiNode>) {
    if node.kind == kind {
        out.push(node);
    }
    for child in &node.children {
        find_all(child, kind, out);
    }
}

/// Every node of `kind`, anywhere in the view.
fn all(view: &ViewInstance, kind: &str) -> Vec<UiNode> {
    let mut found = Vec::new();
    find_all(view.tree(), kind, &mut found);
    found.into_iter().cloned().collect()
}

/// The resource the `prop` of `node` names.
fn ref_of(node: &UiNode, prop: &str) -> Option<ResourceRef> {
    match node.props.get(prop) {
        Some(Value::Ref(r)) => Some((*r.target).clone()),
        _ => None,
    }
}

/// The transcript's rows as the list builds them: each row's node.
fn rows(view: &mut ViewInstance) -> Vec<UiNode> {
    view.poll_patches().unwrap();
    let list = find(view.tree(), "ui.VirtualList")
        .expect("a transcript")
        .key
        .clone();
    let len = view.list_len(&list).unwrap_or(0);
    (0..len)
        .filter_map(|i| view.row(&list, i).unwrap().cloned())
        .collect()
}

/// The items the transcript's rows stand for: the messages, in order.
fn messages(view: &mut ViewInstance) -> Vec<ResourceRef> {
    rows(view)
        .iter()
        .map(|row| {
            let item = find(row, "ui.ListItem").expect("a row of a message");
            ref_of(item, "item").expect("a row of a message")
        })
        .collect()
}

/// The row the message draws for the task `task`: the task's own row view, its checkbox bound to
/// that task's state. Rows are built as they show, so the row is looked for in each row's own
/// tree.
fn task_row(view: &mut ViewInstance, task: &ResourceRef) -> UiNode {
    let rows = rows(view);
    for row in &rows {
        let mut drawn = Vec::new();
        find_all(row, "ui.ListRow", &mut drawn);
        if let Some(drawn) = drawn.into_iter().find(|list_row| {
            list_row.props.get("title") == Some(&Value::text(SAID))
                && list_row.props.get("bind") == Some(&Value::text("checkbox"))
                && list_row.binding.as_ref().is_some_and(|binding| {
                    matches!(
                        &binding.target,
                        BindTarget::Field { target, .. }
                            if target.id_string() == task.id_string()
                    )
                })
        }) {
            return drawn.clone();
        }
    }
    panic!(
        "no row draws the task {}: {:?}",
        task.id_string(),
        rows.len()
    );
}

/// The menu item of `row` in the group `group` labelled `label`.
fn menu(row: &UiNode, group: &str, label: &str) -> String {
    let mut found = Vec::new();
    find_all(row, "ui.MenuItem", &mut found);
    found
        .iter()
        .find(|item| {
            item.props.get("group") == Some(&Value::text(group))
                && item.props.get("label") == Some(&Value::text(label))
        })
        .map_or_else(
            || {
                let all: Vec<_> = found
                    .iter()
                    .map(|item| (item.props.get("group"), item.props.get("label")))
                    .collect();
                panic!("no {group} → {label} in the row's {all:?}")
            },
            |item| item.key.clone(),
        )
}

/// Ada's workspace with Chat and Tasks, a channel `design`, and Bo's message in it.
fn workspace() -> (Replica, ResourceRef, ResourceRef) {
    let (mut ada, _) = Replica::new(
        "ada",
        AutoCommit::new(),
        &[
            (CHAT, CHAT_IDS),
            (DISCUSSION, DISCUSSION_IDS),
            (TASKS, TASKS_IDS),
        ],
    );
    // Both roots exist, as they do once each app's window was opened in the workspace.
    let all = Value::resource(&ada.host.root_of(CHAT_APP).unwrap());
    let _ = ada.host.root_of(TASKS_APP).unwrap();
    ada.commit();
    let team = call(
        &ada,
        CHAT_APP,
        "new_category",
        vec![all, Value::text("Team")],
    );
    let design = call(
        &ada,
        CHAT_APP,
        "add_channel",
        vec![team, Value::text("design")],
    );
    ada.commit();
    let said = Value::Rich(RichValue::from_text(SAID));
    let message = call(
        &ada,
        CHAT_APP,
        "send",
        vec![design.clone(), said, Value::none()],
    );
    ada.commit();
    (ada, target(&design), target(&message))
}

#[test]
fn a_task_made_from_a_message_links_both_ways() {
    let (mut ada, design, message) = workspace();

    // The message's own menu offers making a task of it, and running that carries the message
    // through the door: the action it runs is made from the message, its title its text.
    let mut channel = ada.host.open_resource(&design, None).unwrap();
    assert_eq!(messages(&mut channel), std::slice::from_ref(&message));
    let row = rows(&mut channel).remove(0);
    let key = menu(&row, "Make", "Task");
    let chosen = channel
        .event_action(&key, "choose")
        .expect("Make → Task runs on the message");
    assert_eq!(
        chosen.from.as_ref().map(ResourceRef::id_string),
        Some(message.id_string()),
        "the task is made from the message the menu was opened on"
    );
    let made = match ada
        .host
        .runtime()
        .invoke_action(&Principal::new("ada"), chosen)
        .unwrap()
    {
        InvokeOutcome::Done(made) => made,
        other => panic!("made: {other:?}"),
    };
    ada.commit();
    let task = target(&made);

    // What it was made from is a fact of the change that made it, and the task is what was made
    // from the message.
    assert_eq!(
        ada.host
            .store()
            .version_end(&task, false)
            .unwrap()
            .unwrap()
            .from
            .map(|r| r.id_string()),
        Some(message.id_string())
    );
    assert_eq!(
        ada.host.runtime().read(&task, "title").unwrap(),
        Value::text(SAID)
    );
    assert_eq!(
        ada.host
            .store()
            .linked(&message)
            .unwrap()
            .into_iter()
            .map(|link| (link.of.id_string(), link.how))
            .collect::<Vec<_>>(),
        [(task.id_string(), LinkHow::CreatedFrom)]
    );

    // The task's page names the message it came from, as a quiet row under the title that opens
    // it.
    let page = ada.host.open_resource(&task, None).unwrap();
    let origin = all(&page, "ui.ListRow")
        .into_iter()
        .find(|row| ref_of(row, "item").map(|r| r.id_string()) == Some(message.id_string()))
        .expect("the task's page says what it was made from");
    assert_eq!(origin.props.get("title"), Some(&Value::text(SAID)));
    assert!(
        origin.props.contains_key("prefix"),
        "the row says that the task was made from the message"
    );

    // The message's view draws the task in its own row, as the task's row view draws it: not done
    // yet.
    let done = |view: &mut ViewInstance| {
        let row = task_row(view, &task);
        view.bound_value(&row.key)
    };
    let mut channel = ada.host.open_resource(&design, None).unwrap();
    assert_eq!(done(&mut channel), Some(Value::Bool(false)));

    // Checking the task off shows on the message: the same row, drawn done.
    call(&ada, TASKS_APP, "check", vec![Value::resource(&task)]);
    ada.commit();
    let mut channel = ada.host.open_resource(&design, None).unwrap();
    assert_eq!(done(&mut channel), Some(Value::Bool(true)));
}
