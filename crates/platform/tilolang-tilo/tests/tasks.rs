//! The Tasks app (`apps/tl/tasks`) on two replicas of one room: a task made, filed and
//! checked off; Inbox, Today and Upcoming as the filters of those names, read when they are
//! asked; a quick add that adds what was typed and nothing for an empty entry; and two people
//! changing one task at once.

use automerge::AutoCommit;
use tilolang_runtime::{InvokeOutcome, Principal, UiNode, Value, ViewInstance};

use crate::fixtures::Replica;

const TASKS: &str = include_str!("../../../../apps/tl/tasks/app.tl");
const TASKS_IDS: &str = include_str!("../../../../apps/tl/tasks/app.ids");
const APP: &str = "so.tilo.lang.tasks";

fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    let meta = r.host.package(APP).unwrap().meta;
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

/// Ada's replica of an empty room at 10:00 on Thursday 2026-10-29, and the tasks root.
fn room() -> (Replica, Value) {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[(TASKS, TASKS_IDS)]);
    ada.host
        .runtime()
        .set_clock(|| "2026-10-29T10:00:00Z".to_owned());
    let root = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    (ada, root)
}

fn read(r: &Replica, item: &Value, field: &str) -> Value {
    let Value::Ref(item) = item else {
        panic!("a resource: {item:?}")
    };
    r.host.runtime().read(&item.target, field).unwrap()
}

/// The resource a field refers to, whoever's code made the reference.
fn target(v: &Value) -> Option<String> {
    match v {
        Value::Opt(Some(inner)) => target(inner),
        Value::Ref(r) => Some(r.target.id_string()),
        _ => None,
    }
}

fn items(r: &Replica) -> Vec<Value> {
    let root = r.host.root_of(APP).unwrap();
    match r.host.runtime().read(&root, "items").unwrap() {
        Value::List(items) => items,
        other => panic!("a list: {other:?}"),
    }
}

/// The sidebar's places as the window shows them: label and count.
fn places(view: &ViewInstance) -> Vec<(String, i64)> {
    fn walk(node: &UiNode, out: &mut Vec<(String, i64)>) {
        if node.kind == "ui.NavItem"
            && let (Some(Value::Text(label)), Some(Value::Int(count))) =
                (node.props.get("label"), node.props.get("count"))
        {
            out.push((label.clone(), *count));
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(view.tree(), &mut out);
    out
}

fn count(view: &ViewInstance, place: &str) -> i64 {
    places(view)
        .into_iter()
        .find(|(label, _)| label == place)
        .unwrap_or_else(|| panic!("no place {place} in {:?}", places(view)))
        .1
}

#[test]
fn a_task_is_made_filed_under_a_project_and_checked_off() {
    let (mut ada, root) = room();
    let task = call(
        &ada,
        "add",
        vec![root.clone(), Value::text("Book a haircut")],
    );
    ada.commit();
    let project = call(&ada, "new_project", vec![root.clone()]);
    ada.commit();
    call(
        &ada,
        "rename",
        vec![project.clone(), Value::text("Move flat")],
    );
    call(&ada, "file", vec![task.clone(), project.clone()]);
    ada.commit();
    assert_eq!(target(&read(&ada, &task, "project")), target(&project));
    call(&ada, "check", vec![task.clone()]);
    ada.commit();
    assert_eq!(read(&ada, &task, "done"), Value::Bool(true));
    // Deleting the project sends its tasks back to the inbox; the task stays.
    call(&ada, "remove_project", vec![root, project]);
    ada.commit();
    assert_eq!(read(&ada, &task, "project"), Value::none());
    assert_eq!(items(&ada).len(), 1);
}

#[test]
fn a_quick_add_adds_what_was_typed_and_nothing_for_an_empty_entry() {
    let (mut ada, root) = room();
    call(&ada, "quick_add", vec![root.clone(), Value::text("")]);
    ada.commit();
    assert!(items(&ada).is_empty(), "an empty entry adds nothing");
    call(
        &ada,
        "quick_add",
        vec![root, Value::text("Water the plants")],
    );
    ada.commit();
    let made = items(&ada);
    assert_eq!(made.len(), 1);
    assert_eq!(
        read(&ada, &made[0], "title"),
        Value::text("Water the plants")
    );
    assert_eq!(
        read(&ada, &made[0], "project"),
        Value::none(),
        "into the inbox"
    );
}

#[test]
fn a_quick_add_reads_when_it_is_due_in_the_persons_zone() {
    // 20:00 UTC on Thursday 2026-10-29 is 05:00 on Friday the 30th in Tokyo.
    let (mut ada, root) = room();
    ada.host
        .runtime()
        .set_clock(|| "2026-10-29T20:00:00Z".to_owned());
    ada.host.runtime().set_zone(|| 9 * 3600);
    for entry in [
        "Call the landlord tomorrow 9am",
        "Pay rent fri",
        "Plan the trip next week",
        "Renew the passport in 3 days",
        "Water the plants",
    ] {
        call(&ada, "quick_add", vec![root.clone(), Value::text(entry)]);
    }
    // On Today, what does not say when is due today.
    call(
        &ada,
        "quick_add",
        vec![
            root,
            Value::text("Buy oat milk"),
            Value::none(),
            Value::Bool(true),
        ],
    );
    ada.commit();
    let day = |d: &str| Value::some(Value::text(d));
    let made: Vec<(Value, Value, Value)> = items(&ada)
        .iter()
        .map(|t| {
            (
                read(&ada, t, "title"),
                read(&ada, t, "due"),
                read(&ada, t, "at"),
            )
        })
        .collect();
    assert_eq!(
        made,
        vec![
            (
                Value::text("Call the landlord"),
                day("2026-10-31"),
                day("2026-10-31T00:00:00Z"),
            ),
            (Value::text("Pay rent"), day("2026-11-06"), Value::none()),
            (
                Value::text("Plan the trip"),
                day("2026-11-02"),
                Value::none()
            ),
            (
                Value::text("Renew the passport"),
                day("2026-11-02"),
                Value::none()
            ),
            (
                Value::text("Water the plants"),
                Value::none(),
                Value::none()
            ),
            (
                Value::text("Buy oat milk"),
                day("2026-10-30"),
                Value::none()
            ),
        ],
        "Friday is today in Tokyo, so `fri` is the next one"
    );
    // Today is Tokyo's Friday, though it is still Thursday in UTC.
    let view = ada.view(APP);
    assert_eq!(count(&view, "Today"), 1, "{:?}", places(&view));
    assert_eq!(count(&view, "Upcoming"), 4, "{:?}", places(&view));
}

#[test]
fn inbox_today_and_upcoming_are_read_when_they_are_asked() {
    let (mut ada, root) = room();
    let project = call(&ada, "new_project", vec![root.clone()]);
    ada.commit();
    let dated = |title: &str| call(&ada, "add", vec![root.clone(), Value::text(title)]);
    let late = dated("Pay the electricity bill");
    let today = dated("Buy oat milk");
    let next = dated("Order a present");
    let filed = dated("Pack the books");
    let done = dated("Take the bins out");
    ada.commit();
    for (task, day) in [
        (&late, "2026-10-28"),
        (&today, "2026-10-29"),
        (&next, "2026-10-30"),
        (&done, "2026-10-29"),
    ] {
        call(
            &ada,
            "schedule",
            vec![task.clone(), Value::some(Value::text(day))],
        );
    }
    call(&ada, "file", vec![filed, project]);
    call(&ada, "check", vec![done]);
    ada.commit();
    let view = ada.view(APP);
    assert_eq!(
        count(&view, "Inbox"),
        4,
        "all but the filed one, done or not: {:?}",
        places(&view)
    );
    assert_eq!(count(&view, "Today"), 2, "late and today, not what is done");
    assert_eq!(count(&view, "Upcoming"), 1);
    assert_eq!(count(&view, "Done"), 1);
    // A day later, what was upcoming is today's: the window opened then asks for that day.
    ada.host
        .runtime()
        .set_clock(|| "2026-10-30T10:00:00Z".to_owned());
    let view = ada.view(APP);
    assert_eq!(count(&view, "Today"), 3, "{:?}", places(&view));
    assert_eq!(count(&view, "Upcoming"), 0);
}

#[test]
fn two_people_change_one_task_at_once_and_both_changes_stay() {
    let (mut ada, root) = room();
    let task = call(
        &ada,
        "add",
        vec![root.clone(), Value::text("Call the landlord")],
    );
    ada.commit();
    let mut ben = ada.fork("ben", &[(TASKS, TASKS_IDS)]);
    ben.host.sync_from(&ben.doc);
    // Ada checks it off while Ben files it under a project he just made.
    call(&ada, "check", vec![task.clone()]);
    ada.commit();
    let project = call(&ben, "new_project", vec![root]);
    ben.commit();
    call(&ben, "file", vec![task.clone(), project.clone()]);
    call(&ben, "check", vec![task.clone(), Value::Bool(true)]);
    ben.commit();
    ada.sync_with(&mut ben);
    for r in [&ada, &ben] {
        assert_eq!(read(r, &task, "done"), Value::Bool(true), "{}", r.name);
        assert_eq!(
            target(&read(r, &task, "project")),
            target(&project),
            "{}",
            r.name
        );
        assert_eq!(items(r).len(), 1, "one task on {}", r.name);
    }
}
