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

/// The lanes of a board as its window shows them: each lane's title and its cards' tasks.
fn lanes(view: &ViewInstance) -> Vec<(String, Vec<Option<String>>)> {
    fn walk(node: &UiNode, out: &mut Vec<(String, Vec<Option<String>>)>) {
        if node.kind == "ui.BoardLane" {
            let title = match node.props.get("title") {
                Some(Value::Text(title)) => title.clone(),
                other => panic!("a lane's title: {other:?}"),
            };
            let mut cards = Vec::new();
            let mut stack: Vec<&UiNode> = node.children.iter().rev().collect();
            while let Some(n) = stack.pop() {
                if n.kind == "ui.BoardCard" {
                    cards.push(n.props.get("item").and_then(target));
                } else {
                    stack.extend(n.children.iter().rev());
                }
            }
            out.push((title, cards));
            return;
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(view.tree(), &mut out);
    out
}

/// The key of the node of `kind` that `holds` holds of, in the window `view` shows.
fn key_of(view: &ViewInstance, kind: &str, holds: &dyn Fn(&UiNode) -> bool) -> String {
    fn walk(node: &UiNode, kind: &str, holds: &dyn Fn(&UiNode) -> bool) -> Option<String> {
        if node.kind == kind && holds(node) {
            return Some(node.key.clone());
        }
        node.children.iter().find_map(|c| walk(c, kind, holds))
    }
    walk(view.tree(), kind, holds).unwrap_or_else(|| panic!("no {kind}"))
}

#[test]
fn on_a_projects_board_a_dropped_task_lands_in_its_lane_where_it_was_dropped() {
    let (mut ada, root) = room();
    let project = call(&ada, "new_project", vec![root.clone()]);
    ada.commit();
    let doing = call(&ada, "new_section", vec![project.clone()]);
    call(
        &ada,
        "rename_section",
        vec![doing.clone(), Value::text("Doing")],
    );
    let mut made = Vec::new();
    for title in [
        "Order boxes",
        "Book the van",
        "Pack the books",
        "Measure the bedroom",
    ] {
        let t = call(&ada, "add", vec![root.clone(), Value::text(title)]);
        call(&ada, "file", vec![t.clone(), project.clone()]);
        made.push(t);
    }
    ada.commit();
    let [boxes, van, books, bedroom] = [0, 1, 2, 3].map(|i| made[i].clone());
    let Value::Ref(at) = &project else {
        panic!("a resource: {project:?}")
    };
    let mut view = ada.host.open_resource(&at.target, None).unwrap();
    // The window opens as the project's list; its Board button switches it to the board.
    let board = key_of(&view, "ui.Button", &|b| {
        b.props.get("label") == Some(&Value::text("Board"))
    });
    view.press(&board).unwrap();
    view.poll_patches().unwrap();
    let ids = |ts: &[&Value]| ts.iter().map(|t| target(t)).collect::<Vec<_>>();
    assert_eq!(
        lanes(&view),
        [
            (
                "No section".to_owned(),
                ids(&[&boxes, &van, &books, &bedroom])
            ),
            ("Doing".to_owned(), vec![]),
        ]
    );
    let drop = |view: &mut ViewInstance, lane: &str, task: &Value, at: i64| {
        let key = key_of(view, "ui.BoardLane", &|l| {
            l.props.get("title") == Some(&Value::text(lane))
        });
        let carried = Value::List(vec![task.clone(), Value::Int(at)]);
        view.fire_with(&key, "drop", Some(&carried)).unwrap();
        view.poll_patches().unwrap();
    };
    // Into the empty lane, then a second card above it.
    drop(&mut view, "Doing", &books, 0);
    drop(&mut view, "Doing", &van, 0);
    // Within a lane: the last card to the top.
    drop(&mut view, "No section", &bedroom, 0);
    // Below a lane's last card.
    drop(&mut view, "Doing", &boxes, 2);
    ada.commit();
    assert_eq!(
        lanes(&view),
        [
            ("No section".to_owned(), ids(&[&bedroom])),
            ("Doing".to_owned(), ids(&[&van, &books, &boxes])),
        ]
    );
    assert_eq!(target(&read(&ada, &van, "section")), target(&doing));
    assert_eq!(read(&ada, &bedroom, "section"), Value::none());
}
