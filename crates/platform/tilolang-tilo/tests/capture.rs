//! Capture: an app's create action with one `Text` parameter run from
//! anywhere with one line a person typed. The door reads the words for the action's own field
//! types — a day, a moment, a person, a record — fills them, and titles the rest, so a palette
//! makes what the app's entry would. What it is made from, when it is made from something, is
//! the change's.

use automerge::AutoCommit;
use serde_json::{Value as Json, json};
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::store::Store as _;
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{InvokeOutcome, Principal, Value};
use tilolang_tilo::DynAction;
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::publish::PlanRequest;

use crate::fixtures::Replica;

const TASKS: &str = crate::fixtures::TASKS_SOURCE;
const TASKS_IDS: &str = include_str!("../../../../apps/tl/tasks/app.ids");
const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");
const DISCUSSION_IDS: &str = include_str!("../../../../apps/tl/discussion/app.ids");
const CHAT: &str = include_str!("../../../../apps/tl/chat/app.tl");
const CHAT_IDS: &str = include_str!("../../../../apps/tl/chat/app.ids");
const TASKS_APP: &str = "so.tilo.lang.tasks";
const CHAT_APP: &str = "so.tilo.lang.chat";

/// A Wednesday morning, so "fri" is a day still to come.
const NOW: &str = "2026-11-04T09:00:00Z";

/// `action` of `app` as the door plans it on the app's root, made from `from` when one is given.
fn planned(
    replica: &Replica,
    app: &str,
    action: &str,
    args: Json,
    from: Option<&ResourceRef>,
) -> DynAction {
    let package = replica.host.package(app).unwrap();
    let meta = &package.meta;
    let declared = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("no action {action}"));
    let root = replica.host.root_of(app).unwrap().id_string();
    let kind = meta.kind_of("Tasks");
    replica
        .host
        .plan(&PlanRequest {
            app,
            kind: &kind,
            action_id: &declared.id,
            resource: Some(&root),
            args: args.as_object().unwrap(),
            caller: None,
            from,
        })
        .unwrap()
}

/// The items of the Tasks root, in the order they were added.
fn tasks(replica: &Replica) -> Vec<ResourceRef> {
    let root = replica.host.root_of(TASKS_APP).unwrap();
    match replica.host.runtime().read(&root, "items").unwrap() {
        Value::List(items) => items
            .into_iter()
            .map(|item| match item {
                Value::Ref(r) => std::rc::Rc::unwrap_or_clone(r.target),
                other => panic!("not a resource: {other:?}"),
            })
            .collect(),
        other => panic!("not a list of tasks: {other:?}"),
    }
}

fn field(replica: &Replica, task: &ResourceRef, name: &str) -> Value {
    replica.host.runtime().read(task, name).unwrap()
}

/// The text an optional field holds, when it holds one.
fn some_text(value: Value) -> Option<String> {
    match value {
        Value::Opt(Some(inner)) => match *inner {
            Value::Text(text) => Some(text),
            other => panic!("not text: {other:?}"),
        },
        Value::Opt(None) => None,
        other => panic!("not an optional: {other:?}"),
    }
}

/// Runs `action` of `app` with `args`, as the app's own code does.
fn invoke(replica: &Replica, app: &str, action: &str, args: Vec<Value>) -> Value {
    let id = replica
        .host
        .package(app)
        .unwrap()
        .meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("no action {action}"))
        .id
        .clone();
    match replica
        .host
        .runtime()
        .invoke_action(&Principal::new("ada"), (id.as_str(), args))
        .unwrap()
    {
        InvokeOutcome::Done(value) => value,
        other => panic!("{action}: {other:?}"),
    }
}

/// The id of the Tasks entry action: the create action that takes the line a person types.
fn create(replica: &Replica) -> String {
    let meta = replica.host.package(TASKS_APP).unwrap().meta;
    let action = meta
        .actions
        .iter()
        .find(|a| a.name == "new_task")
        .expect("Tasks has a create action");
    action.id.clone()
}

#[test]
fn one_line_from_anywhere_makes_the_task_its_words_say() {
    let (mut ada, _) = Replica::new(
        "ada",
        AutoCommit::new(),
        &[
            (DISCUSSION, DISCUSSION_IDS),
            (CHAT, CHAT_IDS),
            (TASKS, TASKS_IDS),
        ],
    );
    // Both roots exist, as they do once each app's window was opened in the workspace.
    let _ = ada.host.root_of(TASKS_APP).unwrap();
    ada.commit();
    ada.host.set_clock(|| NOW.to_owned());

    // The palette's one line, given to the action's entry parameter: the door reads the day and
    // the moment out of it, and what is left titles the task.
    let line = "Call the landlord fri 10am";
    let made = planned(&ada, TASKS_APP, "new_task", json!({ "title": line }), None);
    ada.apply(&made);
    ada.commit();

    let today = tilolang::calendar::instant(NOW)
        .unwrap()
        .div_euclid(tilolang::calendar::DAY);
    let list = tasks(&ada);
    let [task] = list.as_slice() else {
        panic!("one task: {list:?}");
    };
    assert_eq!(field(&ada, task, "title"), Value::text("Call the landlord"));
    let due = some_text(field(&ada, task, "due")).expect("a day said: fri");
    let days =
        tilolang::calendar::instant(&format!("{due}T00:00:00Z")).unwrap() / tilolang::calendar::DAY;
    assert_eq!((days + 4).rem_euclid(7), 5, "{due} is a Friday");
    assert!(days >= today, "{due} is still to come");
    let at = some_text(field(&ada, task, "at")).expect("a moment said: 10am");
    assert_eq!(
        at,
        tilolang::calendar::time_text(days * tilolang::calendar::DAY + 10 * 3600),
        "ten in the morning of that Friday"
    );

    // What the invocation records is the same, so a replay makes the same task without reading
    // the line again.
    assert_eq!(made.args.get("title"), Some(&json!("Call the landlord")));
    assert_eq!(made.args.get("due"), Some(&json!(due)));
    assert_eq!(made.args.get("at"), Some(&json!(at)));

    // A line of nothing the task says adds no task: capture never blocks on a field.
    let empty = planned(&ada, TASKS_APP, "new_task", json!({ "title": "" }), None);
    ada.apply(&empty);
    ada.commit();
    assert_eq!(tasks(&ada).len(), 1);
}

#[test]
fn a_task_captured_from_a_message_says_what_it_was_made_from() {
    let (mut ada, _) = Replica::new(
        "ada",
        AutoCommit::new(),
        &[
            (DISCUSSION, DISCUSSION_IDS),
            (CHAT, CHAT_IDS),
            (TASKS, TASKS_IDS),
        ],
    );
    let _ = ada.host.root_of(TASKS_APP).unwrap();
    // Bo says something in the channel `design`, and the palette captures a task from it.
    let chat = ada.host.root_of(CHAT_APP).unwrap();
    let category = invoke(
        &ada,
        CHAT_APP,
        "new_category",
        vec![Value::resource(&chat), Value::text("Team")],
    );
    let channel = invoke(
        &ada,
        CHAT_APP,
        "add_channel",
        vec![category, Value::text("design")],
    );
    let message = invoke(
        &ada,
        CHAT_APP,
        "send",
        vec![
            channel,
            Value::Rich(RichValue::from_text("can someone book the van?")),
            Value::none(),
        ],
    );
    ada.commit();
    let message = match message {
        Value::Ref(r) => std::rc::Rc::unwrap_or_clone(r.target),
        other => panic!("the message: {other:?}"),
    };

    // The palette's row names what it is made from with the arguments it sends (`$from`), and
    // the door reads it as the invocation's origin.
    let mut arguments = serde_json::Map::new();
    arguments.insert("title".to_owned(), json!("Book the van"));
    arguments.insert(
        tilo_actions::params::FROM_ARGUMENT.to_owned(),
        tilo_actions::params::MadeFrom {
            resource: message.id_string(),
            kind: message.ty.clone(),
        }
        .to_json(),
    );
    // What the published action's builder does with the arguments the palette sends: the
    // resource it is made from, parsed from the one argument that names it.
    let from =
        tilo_actions::params::MadeFrom::from_json(&arguments[tilo_actions::params::FROM_ARGUMENT])
            .and_then(|from| ResourceRef::parse_id(&from.resource, from.kind))
            .expect("the arguments name what the task is made from");
    let root = ada.host.root_of(TASKS_APP).unwrap();
    let root = root.id_string();
    let kind = ada.host.package(TASKS_APP).unwrap().meta.kind_of("Tasks");
    let made = ada
        .host
        .plan(&PlanRequest {
            app: TASKS_APP,
            kind: &kind,
            action_id: &create(&ada),
            resource: Some(&root),
            args: &arguments,
            caller: None,
            from: Some(&from),
        })
        .unwrap();
    ada.apply(&made);
    ada.commit();
    let list = tasks(&ada);
    let [task] = list.as_slice() else {
        panic!("one task: {list:?}");
    };
    assert_eq!(field(&ada, task, "title"), Value::text("Book the van"));
    // The change that made it says what it was made from, as "Make from this" does.
    assert_eq!(
        ada.host
            .store()
            .version_end(task, false)
            .unwrap()
            .unwrap()
            .from
            .map(|r| r.id_string()),
        Some(message.id_string())
    );
}
