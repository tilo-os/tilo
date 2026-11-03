//! The Tasks app (`apps/tl/tasks`) on two replicas of one workspace: a task made, filed and
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

/// Ada's replica of an empty workspace at 10:00 on Thursday 2026-10-29, and the tasks root.
fn workspace() -> (Replica, Value) {
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
    let (mut ada, root) = workspace();
    let task = call(
        &ada,
        "new_task",
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
    let (mut ada, root) = workspace();
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
    let (mut ada, root) = workspace();
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
    let (mut ada, root) = workspace();
    let project = call(&ada, "new_project", vec![root.clone()]);
    ada.commit();
    let dated = |title: &str| call(&ada, "new_task", vec![root.clone(), Value::text(title)]);
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

/// The count the tasks window shows over its list.
fn overview_count(view: &ViewInstance) -> Option<i64> {
    fn walk(node: &UiNode, out: &mut Option<i64>) {
        if node.kind == "ui.NavShell"
            && let Some(Value::Int(count)) = node.props.get("overview_count")
        {
            *out = Some(*count);
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = None;
    walk(view.tree(), &mut out);
    out
}

/// The home list, sized before it reads its rows, counts the inbox just written: what the
/// writes put in the run it is drawn from is in its count before the index holds it as well
/// as after.
#[test]
fn the_home_list_counts_the_inbox_that_was_just_written() {
    let (mut ada, root) = workspace();
    let dated = call(
        &ada,
        "new_task",
        vec![root.clone(), Value::text("Buy oat milk and eggs")],
    );
    call(
        &ada,
        "schedule",
        vec![dated, Value::some(Value::text("2026-10-30"))],
    );
    call(&ada, "new_task", vec![root, Value::text("Book a haircut")]);
    assert_eq!(
        overview_count(&ada.view(APP)),
        Some(2),
        "both inbox tasks count before they are indexed"
    );
    ada.commit();
    assert_eq!(overview_count(&ada.view(APP)), Some(2), "and after");
}

#[test]
fn a_window_open_over_midnight_moves_what_today_lists_without_the_record_changing() {
    let (mut ada, root) = workspace();
    let dated = |title: &str| call(&ada, "new_task", vec![root.clone(), Value::text(title)]);
    let today = dated("Buy oat milk");
    let next = dated("Order a present");
    ada.commit();
    for (task, day) in [(&today, "2026-10-29"), (&next, "2026-10-30")] {
        call(
            &ada,
            "schedule",
            vec![task.clone(), Value::some(Value::text(day))],
        );
    }
    ada.commit();
    let mut view = ada.view(APP);
    assert_eq!(count(&view, "Today"), 1, "{:?}", places(&view));
    assert_eq!(count(&view, "Upcoming"), 1, "{:?}", places(&view));
    assert!(
        view.wakes_at().is_some(),
        "the view read the day it is: it asks to be run again when that reads differently"
    );
    // The record did not change; the clock did. The same window runs again and lists the day
    // it is now.
    ada.host
        .runtime()
        .set_clock(|| "2026-10-30T10:00:00Z".to_owned());
    assert!(
        !view.poll_patches().unwrap().is_empty(),
        "the window woke for the time it read"
    );
    assert_eq!(count(&view, "Today"), 2, "{:?}", places(&view));
    assert_eq!(count(&view, "Upcoming"), 0, "{:?}", places(&view));
}

#[test]
fn two_people_change_one_task_at_once_and_both_changes_stay() {
    let (mut ada, root) = workspace();
    let task = call(
        &ada,
        "new_task",
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
    let (mut ada, root) = workspace();
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
        "Measure the bedworkspace",
    ] {
        let t = call(&ada, "new_task", vec![root.clone(), Value::text(title)]);
        call(&ada, "file", vec![t.clone(), project.clone()]);
        made.push(t);
    }
    ada.commit();
    let [boxes, van, books, bedworkspace] = [0, 1, 2, 3].map(|i| made[i].clone());
    let Value::Ref(at) = &project else {
        panic!("a resource: {project:?}")
    };
    let mut view = ada.host.open_resource(&at.target, None).unwrap();
    view.poll_patches().unwrap();
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
                ids(&[&boxes, &van, &books, &bedworkspace])
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
    drop(&mut view, "No section", &bedworkspace, 0);
    // Below a lane's last card.
    drop(&mut view, "Doing", &boxes, 2);
    ada.commit();
    assert_eq!(
        lanes(&view),
        [
            ("No section".to_owned(), ids(&[&bedworkspace])),
            ("Doing".to_owned(), ids(&[&van, &books, &boxes])),
        ]
    );
    assert_eq!(target(&read(&ada, &van, "section")), target(&doing));
    assert_eq!(read(&ada, &bedworkspace, "section"), Value::none());
}

/// A checklist written before a task's steps were tasks of their own opens with each step a
/// subtask of the task it was a step of, in the checklist's order, and the migration runs once.
#[test]
fn a_checklist_written_before_subtasks_opens_as_subtasks() {
    const BEFORE: &str = include_str!("tasks_before_subtasks/app.tl");
    const BEFORE_IDS: &str = include_str!("tasks_before_subtasks/app.ids");
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[(BEFORE, BEFORE_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let copy = call(
        &ada,
        "add",
        vec![all.clone(), Value::text("Write the launch post")],
    );
    ada.commit();
    for title in ["Outline", "First draft", "Screenshots"] {
        call(
            &ada,
            "add_step",
            vec![all.clone(), copy.clone(), Value::text(title)],
        );
        ada.commit();
    }

    // The same workspace, opened by the version that keeps subtasks.
    let (mut later, _) = Replica::new("ada", ada.doc.fork(), &[(TASKS, TASKS_IDS)]);
    later.host.sync_from(&later.doc);
    assert_eq!(later.host.migrate(), 1, "the checklist migration runs");
    for done in later.host.take_committed() {
        later.apply(&done.action);
    }
    let tasks = items(&later);
    let titles: Vec<Value> = tasks
        .iter()
        .map(|item| read(&later, item, "title"))
        .collect();
    assert_eq!(
        titles,
        [
            Value::text("Write the launch post"),
            Value::text("Outline"),
            Value::text("First draft"),
            Value::text("Screenshots"),
        ],
        "the task, then its checklist in order"
    );
    let of = target(&copy).expect("the task");
    for step in &tasks[1..] {
        assert_eq!(
            target(&read(&later, step, "parent")),
            Some(of.clone()),
            "a step's task is under the task it was a step of"
        );
    }
    assert_eq!(
        read(&later, &tasks[1], "done"),
        Value::Bool(false),
        "a step keeps what it had"
    );
    assert_eq!(later.host.migrate(), 0, "the migration runs once");
}

/// A step outlived the task it was a step of: it opens as a task of its own, keeping what it
/// said, rather than being lost or failing the migration.
#[test]
fn a_step_whose_task_was_removed_becomes_a_task_of_its_own() {
    const BEFORE: &str = include_str!("tasks_before_subtasks/app.tl");
    const BEFORE_IDS: &str = include_str!("tasks_before_subtasks/app.ids");
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[(BEFORE, BEFORE_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let gone = call(&ada, "add", vec![all.clone(), Value::text("Old task")]);
    ada.commit();
    call(
        &ada,
        "add_step",
        vec![all.clone(), gone.clone(), Value::text("Its step")],
    );
    ada.commit();
    call(&ada, "remove", vec![all, gone]);
    ada.commit();

    let (mut later, _) = Replica::new("ada", ada.doc.fork(), &[(TASKS, TASKS_IDS)]);
    later.host.sync_from(&later.doc);
    assert_eq!(later.host.migrate(), 1, "the steps migration runs");
    for done in later.host.take_committed() {
        later.apply(&done.action);
    }
    let tasks = items(&later);
    assert_eq!(tasks.len(), 1, "the step stayed, as a task of its own");
    assert_eq!(read(&later, &tasks[0], "title"), Value::text("Its step"));
    assert_eq!(read(&later, &tasks[0], "parent"), Value::none());
}

/// The drawn row of the task held by `item`.
fn row_of(view: &ViewInstance, item: &Value) -> String {
    let want = target(item);
    key_of(view, "ui.TreeViewNode", &|node| {
        node.props.get("item").and_then(target) == want
    })
}

/// The drawn node with this key, at any depth.
fn node_of<'a>(node: &'a UiNode, key: &str) -> Option<&'a UiNode> {
    if node.key == key {
        return Some(node);
    }
    node.children.iter().find_map(|child| node_of(child, key))
}

/// The task with this title.
fn task(r: &Replica, title: &str) -> Value {
    items(r)
        .into_iter()
        .find(|item| read(r, item, "title") == Value::text(title))
        .unwrap_or_else(|| panic!("{title}"))
}

/// Every task's title, in the order the tasks stand.
fn titles(r: &Replica) -> Vec<Value> {
    items(r).iter().map(|item| read(r, item, "title")).collect()
}

/// A launch post with the subtasks "Outline", "First draft" and "Screenshots".
fn launch_post() -> (Replica, Value) {
    let (mut ada, root) = workspace();
    let parent = call(
        &ada,
        "new_task",
        vec![root.clone(), Value::text("Write the launch post")],
    );
    ada.commit();
    for title in ["Outline", "First draft", "Screenshots"] {
        call(
            &ada,
            "add_subtask",
            vec![root.clone(), parent.clone(), Value::text(title)],
        );
        ada.commit();
    }
    (ada, parent)
}

/// The page a task opens, drawn.
fn task_page(ada: &Replica, task: &Value) -> ViewInstance {
    let Value::Ref(at) = task else {
        panic!("a task: {task:?}")
    };
    let mut view = ada.host.open_resource(&at.target, None).unwrap();
    view.poll_patches().unwrap();
    view
}

/// A drag in a task's outline is the one action that names both the new place and the parent it
/// comes to stand under; a row's checkbox asks the outline to toggle that task.
#[test]
fn a_subtask_moves_and_is_checked_from_the_outline() {
    let (mut ada, parent) = launch_post();
    let outline = task(&ada, "Outline");
    let shots = task(&ada, "Screenshots");
    let mut view = task_page(&ada, &parent);
    assert_eq!(
        titles(&ada),
        [
            Value::text("Write the launch post"),
            Value::text("Outline"),
            Value::text("First draft"),
            Value::text("Screenshots"),
        ]
    );
    let key = row_of(&view, &outline);
    view.fire_with(&key, "drop", Some(&shots)).unwrap();
    ada.commit();
    view.poll_patches().unwrap();
    assert_eq!(
        titles(&ada),
        [
            Value::text("Write the launch post"),
            Value::text("Screenshots"),
            Value::text("Outline"),
            Value::text("First draft"),
        ]
    );
    let key = row_of(&view, &outline);
    view.fire_with(&key, "check", None).unwrap();
    ada.commit();
    view.poll_patches().unwrap();
    assert_eq!(read(&ada, &outline, "done"), Value::Bool(true));
}

/// Dropped inside another task's row, a task becomes its subtask, and the outline draws it there.
#[test]
fn a_subtask_dropped_inside_another_is_nested() {
    let (mut ada, parent) = launch_post();
    let outline = task(&ada, "Outline");
    let draft = task(&ada, "First draft");
    let mut view = task_page(&ada, &parent);
    let key = row_of(&view, &draft);
    view.fire_with(&key, "into", Some(&outline)).unwrap();
    ada.commit();
    view.poll_patches().unwrap();
    assert_eq!(
        target(&read(&ada, &outline, "parent")),
        target(&draft),
        "the task is inside the row it was dropped on"
    );
    assert_eq!(
        titles(&ada),
        [
            Value::text("Write the launch post"),
            Value::text("First draft"),
            Value::text("Outline"),
            Value::text("Screenshots"),
        ],
        "it stands where it landed, under its new parent"
    );
    let key = row_of(&view, &outline);
    let node = node_of(view.tree(), &key).unwrap();
    assert_eq!(
        node.props.get("parent").and_then(target),
        target(&draft),
        "the row is drawn under its new parent"
    );
}

/// The menu's Indent and Outdent are the moves `Tab` and `Shift-Tab` make: under the task over
/// it, and out beside the one over it.
#[test]
fn a_subtask_indents_over_itself_and_outdents_beside_its_parent() {
    let (mut ada, parent) = launch_post();
    let draft = task(&ada, "First draft");
    let shots = task(&ada, "Screenshots");
    call(&ada, "indent", vec![shots.clone()]);
    ada.commit();
    assert_eq!(
        target(&read(&ada, &shots, "parent")),
        target(&draft),
        "Tab puts it under the task just over it"
    );
    let view = task_page(&ada, &parent);
    let key = row_of(&view, &shots);
    let node = node_of(view.tree(), &key).unwrap();
    assert_eq!(
        node.props.get("parent").and_then(target),
        target(&draft),
        "the row is drawn one step deeper"
    );
    call(&ada, "outdent", vec![shots.clone()]);
    ada.commit();
    assert_eq!(
        target(&read(&ada, &shots, "parent")),
        target(&parent),
        "Shift-Tab puts it back beside the task over it"
    );
    assert_eq!(
        titles(&ada),
        [
            Value::text("Write the launch post"),
            Value::text("Outline"),
            Value::text("First draft"),
            Value::text("Screenshots"),
        ],
        "out again it stands after the task it was under"
    );
}

/// A task is done exactly when everything under it is: checking it off checks off every step
/// under it, however deep, and opening a step opens the tasks it is under while the rest stay.
#[test]
fn checking_a_task_off_checks_every_task_under_it() {
    let (mut ada, parent) = launch_post();
    let tasks = Value::resource(&ada.host.root_of(APP).unwrap());
    let outline = task(&ada, "Outline");
    call(
        &ada,
        "add_subtask",
        vec![tasks, outline.clone(), Value::text("Gather facts")],
    );
    ada.commit();
    let facts = task(&ada, "Gather facts");

    call(&ada, "check", vec![parent.clone()]);
    ada.commit();
    for t in [
        &parent,
        &outline,
        &task(&ada, "First draft"),
        &task(&ada, "Screenshots"),
        &facts,
    ] {
        assert_eq!(
            read(&ada, t, "done"),
            Value::Bool(true),
            "{:?} is done with the task over it",
            read(&ada, t, "title")
        );
    }

    // Opening a step at the bottom opens everything over it, and nothing beside it.
    call(&ada, "check", vec![facts.clone()]);
    ada.commit();
    assert_eq!(read(&ada, &facts, "done"), Value::Bool(false));
    assert_eq!(
        read(&ada, &outline, "done"),
        Value::Bool(false),
        "the task it is a step of opens too"
    );
    assert_eq!(
        read(&ada, &parent, "done"),
        Value::Bool(false),
        "and the task that one is under"
    );
    for title in ["First draft", "Screenshots"] {
        assert_eq!(
            read(&ada, &task(&ada, title), "done"),
            Value::Bool(true),
            "{title} stays done"
        );
    }
}

/// A list row's checkbox asks the app's check, so a press on it takes the tasks under the one
/// it stands for, not the one field the box shows.
#[test]
fn a_rows_checkbox_checks_the_tasks_under_it() {
    let (mut ada, parent) = launch_post();
    assert_eq!(
        target(&read(&ada, &parent, "project")),
        None,
        "the task is in the inbox, so the home list shows it"
    );
    let mut view = ada.view(APP);
    // Every row, not only the ones the window has room for.
    view.set_lazy_rows(false).unwrap();
    let key = key_of(&view, "ui.ListRow", &|node| {
        node.props.get("title") == Some(&Value::text("Write the launch post"))
    });
    view.fire_with(&key, "check", None).unwrap();
    ada.commit();
    view.poll_patches().unwrap();
    for title in ["Write the launch post", "Outline", "First draft"] {
        assert_eq!(
            read(&ada, &task(&ada, title), "done"),
            Value::Bool(true),
            "{title} is done from the row's box"
        );
    }
}
