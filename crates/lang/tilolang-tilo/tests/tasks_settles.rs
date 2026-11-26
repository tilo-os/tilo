//! A task's fields that an action settles (`settles:`) are set by that action, however they are
//! set: the status chip on a task's page closes its steps with it as the board does, the day chip
//! moves its reminder as the date picker does, the assignee chip assigns as `assign` does, and an
//! agent's field edit of the status is the same `set_status`.

use tilolang_runtime::{Actor, Value};
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::publish::PlanRequest;

use crate::fixtures::Replica;
use crate::tasks::{
    APP, bound_key, call, count, create, done, read, status_of, task_page, workspace,
};

/// The key of the control on `page` bound to the task's field `field`.
fn chip(ada: &Replica, page: &tilolang_runtime::ViewInstance, field: &str) -> String {
    let id = crate::fixtures::field_id(&ada.host.package(APP).unwrap().meta, "Task", field);
    bound_key(page.tree(), &id).unwrap_or_else(|| panic!("the page binds the task's {field}"))
}

/// The action that made the task's last change, by name.
fn last_action(ada: &Replica, task: &Value) -> String {
    let Value::Ref(at) = task else {
        panic!("a task: {task:?}")
    };
    let id = ada
        .versions(&at.target)
        .last()
        .expect("a change")
        .action
        .clone();
    let meta = ada.host.package(APP).unwrap().meta;
    meta.actions
        .iter()
        .find(|action| action.id == id)
        .map_or(id, |action| action.name.clone())
}

/// A task with a step under it.
fn a_task_with_a_step() -> (Replica, Value, Value) {
    let (ada, root) = workspace();
    let post = call(&ada, "new_task", create(&root, "Write the launch post"));
    let outline = call(
        &ada,
        "add_subtask",
        vec![post.clone(), Value::text("Outline")],
    );
    ada.commit();
    (ada, post, outline)
}

/// Choosing Done on the status chip of a task's page is `set_status`: the task is done, the step
/// under it is closed with it, and choosing To do opens the task again.
#[test]
fn the_status_chip_closes_a_task_with_its_steps() {
    let (ada, post, outline) = a_task_with_a_step();
    let mut page = task_page(&ada, &post);
    let status = chip(&ada, &page, "status");

    page.edit(&status, &Value::text("done")).unwrap();
    ada.commit();
    page.poll_patches().unwrap();
    assert_eq!(status_of(&ada, &post), "done");
    assert_eq!(
        read(&ada, &post, "done"),
        Value::Bool(true),
        "done keeps step with the status"
    );
    assert!(done(&ada, &outline), "the step is closed with its task");
    assert_eq!(
        last_action(&ada, &post),
        "set_status",
        "the change is the action's"
    );

    page.edit(&status, &Value::text("todo")).unwrap();
    ada.commit();
    page.poll_patches().unwrap();
    assert_eq!(status_of(&ada, &post), "todo");
    assert_eq!(
        read(&ada, &post, "done"),
        Value::Bool(false),
        "opened again"
    );

    // Choosing what the chip already holds is no change, and no refusal.
    let before = last_action(&ada, &post);
    page.edit(&status, &Value::text("todo")).unwrap();
    assert_eq!(last_action(&ada, &post), before);
}

/// The box beside a task's title on its page is the shared checkbox, bound to `done`: marking it
/// is `check`, which closes the task through `set_status` and the step under it with it, and unmarking it opens the task
/// again. The page never writes `done` itself.
#[test]
fn the_pages_box_checks_a_task_off_with_its_steps() {
    let (ada, post, outline) = a_task_with_a_step();
    let mut page = task_page(&ada, &post);
    let done_box = chip(&ada, &page, "done");
    let kind = |tree: &tilolang_runtime::UiNode| -> Option<String> {
        fn find(node: &tilolang_runtime::UiNode, key: &str) -> Option<String> {
            if node.key == key {
                return Some(node.kind.clone());
            }
            node.children.iter().find_map(|child| find(child, key))
        }
        find(tree, &done_box)
    };
    assert_eq!(
        kind(page.tree()).as_deref(),
        Some("ui.Checkbox"),
        "the page's box is the shared checkbox"
    );

    page.edit(&done_box, &Value::Bool(true)).unwrap();
    ada.commit();
    page.poll_patches().unwrap();
    assert_eq!(status_of(&ada, &post), "done");
    assert!(done(&ada, &outline), "the step is closed with its task");
    // `check` moves the task through the board's own `set_status`, which records the change.
    assert_eq!(
        last_action(&ada, &post),
        "set_status",
        "the change is the action's, not a write of done"
    );

    page.edit(&done_box, &Value::Bool(false)).unwrap();
    ada.commit();
    page.poll_patches().unwrap();
    assert_eq!(status_of(&ada, &post), "todo", "opened again");
    assert_eq!(read(&ada, &post, "done"), Value::Bool(false));
}

/// Moving a task's day on its page's day chip moves its moment and its reminder with it, at the
/// same time of day, as the date picker does.
#[test]
fn the_day_chip_moves_the_reminder_with_the_day() {
    let (ada, root) = workspace();
    let call_mum = call(&ada, "new_task", create(&root, "Call mum"));
    call(
        &ada,
        "schedule",
        vec![call_mum.clone(), Value::some(Value::text("2026-11-18"))],
    );
    call(
        &ada,
        "set_time",
        vec![
            call_mum.clone(),
            Value::some(Value::text("2026-11-18T17:00:00Z")),
        ],
    );
    call(
        &ada,
        "remind_at",
        vec![call_mum.clone(), Value::text("2026-11-18T17:00:00Z")],
    );
    ada.commit();
    let mut page = task_page(&ada, &call_mum);
    let due = chip(&ada, &page, "due");

    page.edit(&due, &Value::text("2026-11-20")).unwrap();
    ada.commit();
    page.poll_patches().unwrap();
    assert_eq!(
        read(&ada, &call_mum, "due"),
        Value::some(Value::text("2026-11-20"))
    );
    assert_eq!(
        read(&ada, &call_mum, "remind"),
        Value::some(Value::text("2026-11-20T17:00:00Z")),
        "the reminder moved with the day"
    );
    assert_eq!(
        read(&ada, &call_mum, "at"),
        Value::some(Value::text("2026-11-20T17:00:00Z")),
        "the moment moved with the day"
    );
}

/// An agent setting a task's status through the field edit every resource publishes runs
/// `set_status` too: the step under the task closes with it.
#[test]
fn an_agents_field_edit_of_the_status_is_set_status() {
    let (ada, post, outline) = a_task_with_a_step();
    let Value::Ref(at) = &post else {
        panic!("a task")
    };
    let meta = ada.host.package(APP).unwrap().meta;
    let kind = meta.kind_of("Task");
    let resource = at.target.id_string();
    let args = serde_json::Map::from_iter([
        ("field".to_owned(), serde_json::json!("status")),
        ("value".to_owned(), serde_json::json!("done")),
    ]);
    ada.carry_out(&PlanRequest {
        app: APP,
        kind: &kind,
        action_id: tilolang_tilo::action::EDIT_TEXT_ACTION,
        resource: Some(&resource),
        args: &args,
        caller: None,
        from: None,
    })
    .expect("the edit runs the action that settles the status");
    ada.commit();
    assert_eq!(status_of(&ada, &post), "done");
    assert!(done(&ada, &outline), "the step is closed with its task");
    assert_eq!(last_action(&ada, &post), "set_status");
}

/// Agent work lists what agents are given and nothing else: a task made for an agent is there
/// from the start, a person's task is not, and a task handed from the agent to a person on its
/// page's assignee chip leaves it, since the chip assigns as `assign` does.
#[test]
fn agent_work_lists_the_tasks_agents_are_given() {
    use tilolang_tilo::runtime::ids::{MemberId, actor_text};
    let (ada, root) = workspace();
    let scout = Value::Actor(Actor::from(MemberId::agent("scout", "ada")));
    let me = Actor::from(MemberId::person("ada"));
    let mut args = create(&root, "Sort the inbox");
    args[5] = Value::some(scout);
    let sorted = call(&ada, "new_task", args);
    let rent = call(&ada, "new_task", create(&root, "Pay the rent"));
    call(
        &ada,
        "assign",
        vec![rent, Value::some(Value::Actor(me.clone()))],
    );
    ada.commit();
    assert_eq!(
        count(&ada.view(APP), "Agent work"),
        Some(1),
        "the agent's task alone"
    );

    let page = task_page(&ada, &sorted);
    let assignee = chip(&ada, &page, "assignee");
    page.edit(&assignee, &Value::text(actor_text(&me))).unwrap();
    ada.commit();
    assert_eq!(
        read(&ada, &sorted, "assigned_by"),
        Value::none(),
        "no agent was given it"
    );
    assert_eq!(last_action(&ada, &sorted), "assign");
    assert_eq!(
        count(&ada.view(APP), "Agent work"),
        None,
        "nothing waits on an agent"
    );
}
