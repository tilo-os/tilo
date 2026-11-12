//! The Tasks app (`apps/tl/tasks`) on two replicas of one workspace: a task made, filed and
//! checked off; Inbox, Today, Upcoming and Done as the places its own filters make, read when
//! they are asked; a quick add that adds what was typed and nothing for an empty entry; and two
//! people changing one task at once.

use automerge::AutoCommit;
use tilolang_runtime::{InvokeOutcome, Principal, UiNode, Value, ViewInstance};

use crate::fixtures::Replica;

pub(crate) const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");
pub(crate) const DISCUSSION_IDS: &str = include_str!("../../../../apps/tl/discussion/app.ids");
pub(crate) const TASKS: &str = crate::fixtures::TASKS_SOURCE;
pub(crate) const TASKS_IDS: &str = include_str!("../../../../apps/tl/tasks/app.ids");
/// What a task's page is drawn from: the tasks and the discussion under them.
pub(crate) const PACKAGES: &[(&str, &str)] = &[(DISCUSSION, DISCUSSION_IDS), (TASKS, TASKS_IDS)];
pub(crate) const APP: &str = "so.tilo.lang.tasks";

pub(crate) fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
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
pub(crate) fn workspace() -> (Replica, Value) {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), PACKAGES);
    ada.host.set_clock(|| "2026-10-29T10:00:00Z".to_owned());
    let root = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    (ada, root)
}

pub(crate) fn read(r: &Replica, item: &Value, field: &str) -> Value {
    let Value::Ref(item) = item else {
        panic!("a resource: {item:?}")
    };
    r.host.runtime().read(&item.target, field).unwrap()
}

/// The resource a field refers to, whoever's code made the reference.
pub(crate) fn target(v: &Value) -> Option<String> {
    match v {
        Value::Opt(Some(inner)) => target(inner),
        Value::Ref(r) => Some(r.target.id_string()),
        _ => None,
    }
}

/// The arguments `new_task` takes for a task added with only its title: the list it is added
/// to, nothing for the project, section and place it is typed in, nothing for the day, time and
/// assignee, and what was typed.
pub(crate) fn create(root: &Value, title: &str) -> Vec<Value> {
    let mut args = vec![root.clone()];
    args.resize(6, Value::none());
    args.push(Value::text(title));
    args
}

fn items(r: &Replica) -> Vec<Value> {
    let root = r.host.root_of(APP).unwrap();
    match r.host.runtime().read(&root, "items").unwrap() {
        Value::List(items) => items,
        other => panic!("a list: {other:?}"),
    }
}

/// A place as the sidebar draws it: its label, and the count the sidebar itself works out for
/// the places it counts (the inbox's and today's).
pub(crate) type ShownPlace = (String, Option<i64>);

/// A heading of the sidebar with the places under it, in the order they stand.
pub(crate) type ShownSection = (String, Vec<ShownPlace>);

/// The sidebar's places as the window shows them.
pub(crate) fn places(view: &ViewInstance) -> Vec<ShownPlace> {
    fn walk(node: &UiNode, out: &mut Vec<ShownPlace>) {
        if node.kind == "ui.NavItem"
            && let Some(Value::Text(label)) = node.props.get("label")
        {
            let count = match node.props.get("count") {
                Some(Value::Int(count)) => Some(*count),
                _ => None,
            };
            out.push((label.clone(), count));
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(view.tree(), &mut out);
    out
}

/// What the sidebar counts at `place`: a count only where the place's filter asks for one.
pub(crate) fn count(view: &ViewInstance, place: &str) -> Option<i64> {
    places(view)
        .into_iter()
        .find(|(label, _)| label == place)
        .unwrap_or_else(|| panic!("no place {place} in {:?}", places(view)))
        .1
}

#[test]
fn a_task_is_made_filed_under_a_project_and_checked_off() {
    let (mut ada, root) = workspace();
    let task = call(&ada, "new_task", create(&root, "Book a haircut"));
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
fn an_entry_adds_what_was_typed_and_nothing_for_an_empty_entry() {
    let (ada, _) = workspace();
    let mut view = ada.view(APP);
    // An entry with nothing in it adds nothing.
    enter(&mut view, "");
    assert!(items(&ada).is_empty(), "an empty entry adds nothing");
    enter(&mut view, "Water the plants");
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
    assert_eq!(
        view.bound_value(&entry(&view)),
        Some(Value::text("")),
        "the entry is cleared once the task it made is listed"
    );
}

#[test]
fn an_entry_reads_when_it_is_due_in_the_persons_zone() {
    // 20:00 UTC on Thursday 2026-10-29 is 05:00 on Friday the 30th in Tokyo.
    let (mut ada, _) = workspace();
    ada.host.set_clock(|| "2026-10-29T20:00:00Z".to_owned());
    ada.host.set_zone(|| 9 * 3600);
    let mut view = ada.view(APP);
    // The inbox's entry reads what is typed: what a task says of when it is due is the app's
    // reading of the words, and today's entry is the day's, typed in as it stands.
    go_to(&mut view, "Inbox");
    for text in [
        "Call the landlord tomorrow 9am",
        "Pay rent fri",
        "Plan the trip next week",
        "Renew the passport in 3 days",
        "Water the plants",
    ] {
        enter(&mut view, text);
    }
    // On Today, what does not say when is due today.
    go_to(&mut view, "Today");
    enter(&mut view, "Buy oat milk");
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
    // Today is Tokyo's Friday, though it is still Thursday in UTC: what is due today is the
    // day's place, what is due later stands in its own under the days it is due on, and neither
    // the plants nor it is counted beyond the count of what waits.
    let mut view = ada.view(APP);
    assert_eq!(count(&view, "Inbox"), Some(1), "{:?}", places(&view));
    assert_eq!(count(&view, "Today"), Some(1), "{:?}", places(&view));
    assert_eq!(
        count(&view, "Upcoming"),
        None,
        "a place of what is due later counts nothing: {:?}",
        places(&view)
    );
    go_to(&mut view, "Upcoming");
    assert_eq!(
        home_list_titles(&ada, &mut view),
        [
            "Call the landlord",
            "Plan the trip",
            "Renew the passport",
            "Pay rent"
        ],
        "every dated entry after today, soonest first"
    );
}

#[test]
fn an_entry_in_today_reads_the_same_words_as_the_inbox() {
    let (mut ada, _) = workspace();
    ada.host.runtime().set_members(
        vec![
            tilolang_runtime::members::Member::person("ada", "owner"),
            tilolang_runtime::members::Member::person("bo", "editor"),
        ],
        None,
    );
    // Thursday 2026-10-29: Friday is the next day. Today's entry takes the same words the
    // inbox's does, a day, a moment and who it is for, and a task that names no day is today's.
    let mut view = ada.view(APP);
    go_to(&mut view, "Today");
    enter(&mut view, "Call the bank fri 10am @bo");
    enter(&mut view, "Water the plants");
    ada.commit();
    let day = |d: &str| Value::some(Value::text(d));
    let made: Vec<(Value, Value, Value, Value)> = items(&ada)
        .iter()
        .map(|t| {
            (
                read(&ada, t, "title"),
                read(&ada, t, "due"),
                read(&ada, t, "at"),
                read(&ada, t, "assignee"),
            )
        })
        .collect();
    assert_eq!(
        made,
        vec![
            (
                Value::text("Call the bank"),
                day("2026-10-30"),
                day("2026-10-30T10:00:00Z"),
                Value::some(Value::Principal(Principal::new("bo"))),
            ),
            (
                Value::text("Water the plants"),
                day("2026-10-29"),
                Value::none(),
                Value::none(),
            ),
        ],
        "the words fill the task the same way here: fri 10am is Friday at ten, and no day is today"
    );
    let mut view = ada.view(APP);
    assert_eq!(count(&view, "Today"), Some(1), "{:?}", places(&view));
    go_to(&mut view, "Today");
    assert_eq!(
        home_list_titles(&ada, &mut view),
        ["Water the plants"],
        "the task that named no day stands in Today; the one for Friday waits"
    );
}

#[test]
fn inbox_today_and_upcoming_are_read_when_they_are_asked() {
    let (mut ada, root) = workspace();
    let project = call(&ada, "new_project", vec![root.clone()]);
    ada.commit();
    let dated = |title: &str| call(&ada, "new_task", create(&root, title));
    let late = dated("Pay the electricity bill");
    let today = dated("Buy oat milk");
    let next = dated("Order a present");
    dated("Water the plants");
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
    let mut view = ada.view(APP);
    assert_eq!(
        count(&view, "Inbox"),
        Some(1),
        "the one nobody sorted: not the filed task, not the dated ones, not what is done: {:?}",
        places(&view)
    );
    assert_eq!(
        count(&view, "Today"),
        Some(2),
        "late and today, not what is done"
    );
    assert_eq!(
        count(&view, "Upcoming"),
        None,
        "what is due later counts nothing"
    );
    assert_eq!(
        count(&view, "Done"),
        None,
        "Done counts nothing: {:?}",
        places(&view)
    );
    // Each place holds what its filter says of the tasks, read when it is asked.
    go_to(&mut view, "Inbox");
    assert_eq!(home_list_titles(&ada, &mut view), ["Water the plants"]);
    go_to(&mut view, "Today");
    assert_eq!(
        home_list_titles(&ada, &mut view),
        ["Pay the electricity bill", "Buy oat milk"],
        "the day gone by first, then today, what is due soonest first"
    );
    go_to(&mut view, "Upcoming");
    assert_eq!(home_list_titles(&ada, &mut view), ["Order a present"]);
    go_to(&mut view, "Done");
    assert_eq!(
        home_list_titles(&ada, &mut view),
        ["Take the bins out"],
        "Done holds what is done with"
    );
    // A day later, what was upcoming is today's: the window opened then asks for that day.
    ada.host.set_clock(|| "2026-10-30T10:00:00Z".to_owned());
    let mut view = ada.view(APP);
    assert_eq!(count(&view, "Today"), Some(3), "{:?}", places(&view));
    go_to(&mut view, "Today");
    assert_eq!(
        home_list_titles(&ada, &mut view),
        [
            "Pay the electricity bill",
            "Buy oat milk",
            "Order a present"
        ],
        "the day the window woke on is the day's place"
    );
    go_to(&mut view, "Upcoming");
    assert!(
        home_list_titles(&ada, &mut view).is_empty(),
        "nothing is left for later"
    );
}

/// The count the tasks window shows over its list: the list is the window's content, so the
/// count stands beside the content's title.
fn list_count(view: &ViewInstance) -> Option<i64> {
    fn walk(node: &UiNode, out: &mut Option<i64>) {
        if node.kind == "ui.NavShell"
            && let Some(Value::Int(count)) = node.props.get("count")
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
    for title in ["Buy oat milk and eggs", "Book a haircut"] {
        call(&ada, "new_task", create(&root, title));
    }
    let mut view = ada.view(APP);
    go_to(&mut view, "Inbox");
    assert_eq!(
        list_count(&view),
        Some(2),
        "both unsorted tasks count before they are indexed"
    );
    ada.commit();
    let mut after = ada.view(APP);
    go_to(&mut after, "Inbox");
    assert_eq!(list_count(&after), Some(2), "and after");
}

#[test]
fn a_window_open_over_midnight_moves_what_today_lists_without_the_record_changing() {
    let (mut ada, root) = workspace();
    let dated = |title: &str| call(&ada, "new_task", create(&root, title));
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
    assert_eq!(count(&view, "Today"), Some(1), "{:?}", places(&view));
    assert_eq!(
        count(&view, "Upcoming"),
        None,
        "the place of what is due later counts nothing: {:?}",
        places(&view)
    );
    assert!(
        view.wakes_at().is_some(),
        "the view read the day it is: it asks to be run again when that reads differently"
    );
    go_to(&mut view, "Upcoming");
    assert_eq!(home_list_titles(&ada, &mut view), ["Order a present"]);
    // The record did not change; the clock did. The same window runs again and lists the day
    // it is now.
    ada.host.set_clock(|| "2026-10-30T10:00:00Z".to_owned());
    assert!(
        !view.poll_patches().unwrap().is_empty(),
        "the window woke for the time it read"
    );
    assert_eq!(count(&view, "Today"), Some(2), "{:?}", places(&view));
    assert!(
        home_list_titles(&ada, &mut view).is_empty(),
        "the day's place took the task that was due later: {:?}",
        places(&view)
    );
}

#[test]
fn two_people_change_one_task_at_once_and_both_changes_stay() {
    let (mut ada, root) = workspace();
    let task = call(&ada, "new_task", create(&root, "Call the landlord"));
    ada.commit();
    let mut ben = ada.fork("ben", PACKAGES);
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

/// A lane's title as its node carries it: none for the lane of the tasks in no section.
fn lane_title(node: &UiNode) -> Option<String> {
    match node.props.get("title") {
        Some(Value::Text(title)) => Some(title.clone()),
        None | Some(Value::Unit | Value::Opt(None)) => None,
        other => panic!("a lane's title: {other:?}"),
    }
}

/// A lane as a board's window shows it: what it is called (none for the lane of the tasks in no
/// section), the count its header shows (none where it has no heading), and its cards' tasks.
type Lane = (Option<String>, Option<i64>, Vec<Option<String>>);

/// The lanes of a board as its window shows them, in the order they stand.
fn lanes(view: &ViewInstance) -> Vec<Lane> {
    fn walk(node: &UiNode, out: &mut Vec<Lane>) {
        if node.kind == "ui.BoardLane" {
            let title = lane_title(node);
            let count = match node.props.get("count") {
                Some(Value::Int(count)) => Some(*count),
                None | Some(Value::Unit | Value::Opt(None)) => None,
                other => panic!("a lane's count: {other:?}"),
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
            out.push((title, count, cards));
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
pub(crate) fn key_of(view: &ViewInstance, kind: &str, holds: &dyn Fn(&UiNode) -> bool) -> String {
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
        let t = call(&ada, "new_task", create(&root, title));
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
    // The project's window opens as its list and switches to the board from the bar's tabs.
    let tabs = key_of(&view, "ui.ViewTabs", &|_| true);
    view.fire_with(&tabs, "change", Some(&Value::Int(1)))
        .unwrap();
    view.poll_patches().unwrap();
    let ids = |ts: &[&Value]| ts.iter().map(|t| target(t)).collect::<Vec<_>>();
    assert_eq!(
        lanes(&view),
        [
            (None, None, ids(&[&boxes, &van, &books, &bedworkspace])),
            (Some("Doing".to_owned()), Some(0), vec![]),
        ]
    );
    let drop = |view: &mut ViewInstance, lane: Option<&str>, task: &Value, at: i64| {
        let key = key_of(view, "ui.BoardLane", &|l| match lane {
            Some(lane) => lane_title(l).as_deref() == Some(lane),
            None => lane_title(l).is_none(),
        });
        let carried = Value::List(vec![task.clone(), Value::Int(at)]);
        view.fire_with(&key, "drop", Some(&carried)).unwrap();
    };
    // Into the empty lane, then a second card above it.
    drop(&mut view, Some("Doing"), &books, 0);
    drop(&mut view, Some("Doing"), &van, 0);
    // Within a lane: the last card to the top.
    drop(&mut view, None, &bedworkspace, 0);
    // Below a lane's last card.
    drop(&mut view, Some("Doing"), &boxes, 2);
    ada.commit();
    // A window hears of a change once, after the record holds it: it runs again now, and shows
    // the cards where they landed.
    view.poll_patches().unwrap();
    assert_eq!(
        lanes(&view),
        [
            (None, None, ids(&[&bedworkspace])),
            (
                Some("Doing".to_owned()),
                Some(3),
                ids(&[&van, &books, &boxes])
            ),
        ]
    );
    assert_eq!(target(&read(&ada, &van, "section")), target(&doing));
    assert_eq!(read(&ada, &bedworkspace, "section"), Value::none());
}

/// A lane's header counts the cards the lane holds: the open tasks filed in it, not the
/// project's tasks, the ones it leaves out (done, or a step under a task), or another
/// project's.
#[test]
fn a_lanes_header_counts_the_cards_it_holds() {
    let (mut ada, root) = workspace();
    let project = call(&ada, "new_project", vec![root.clone()]);
    ada.commit();
    let packing = call(&ada, "new_section", vec![project.clone()]);
    call(
        &ada,
        "rename_section",
        vec![packing.clone(), Value::text("Packing")],
    );
    let booked = call(&ada, "new_section", vec![project.clone()]);
    call(&ada, "rename_section", vec![booked, Value::text("Booked")]);
    // Two in no section, one in Packing, and in Booked nothing yet.
    let boxes = call(
        &ada,
        "add_to",
        vec![project.clone(), Value::text("Order boxes"), Value::none()],
    );
    let tape = call(
        &ada,
        "add_to",
        vec![project.clone(), Value::text("Order tape"), Value::none()],
    );
    let kitchen = call(
        &ada,
        "add_to",
        vec![
            project.clone(),
            Value::text("Clean the kitchen"),
            Value::some(packing.clone()),
        ],
    );
    // A step under the kitchen's task is no card of its own, a done task is no card, and a
    // task of another project is none here.
    call(
        &ada,
        "add_subtask",
        vec![
            root.clone(),
            kitchen.clone(),
            Value::text("Wipe the shelves"),
        ],
    );
    let books = call(
        &ada,
        "add_to",
        vec![
            project.clone(),
            Value::text("Pack the books"),
            Value::some(packing),
        ],
    );
    call(&ada, "check", vec![books, Value::Bool(true)]);
    let van = call(&ada, "new_project", vec![root]);
    call(
        &ada,
        "add_to",
        vec![van, Value::text("Book the van"), Value::none()],
    );
    ada.commit();
    let Value::Ref(at) = &project else {
        panic!("a resource: {project:?}")
    };
    let mut view = ada.host.open_resource(&at.target, None).unwrap();
    view.poll_patches().unwrap();
    // The project's window switches to its board from the bar's labelled tabs.
    let tabs = key_of(&view, "ui.ViewTabs", &|_| true);
    view.fire_with(&tabs, "change", Some(&Value::Int(1)))
        .unwrap();
    view.poll_patches().unwrap();
    let ids = |ts: &[&Value]| ts.iter().map(|t| target(t)).collect::<Vec<_>>();
    assert_eq!(
        lanes(&view),
        [
            (None, None, ids(&[&boxes, &tape])),
            (Some("Packing".to_owned()), Some(1), ids(&[&kitchen])),
            (Some("Booked".to_owned()), Some(0), vec![]),
        ]
    );
    for (title, count, cards) in lanes(&view) {
        let Some(count) = count else {
            assert!(title.is_none(), "a lane with a heading has a count");
            continue;
        };
        assert_eq!(
            usize::try_from(count).unwrap_or(usize::MAX),
            cards.len(),
            "the header of `{title:?}` counts the cards it holds"
        );
    }
}

/// The drawn row of the task held by `item`.
fn row_of(view: &ViewInstance, item: &Value) -> String {
    let want = target(item);
    key_of(view, "ui.TreeViewNode", &|node| {
        node.props.get("item").and_then(target) == want
    })
}

/// The tree a page's row stands in: the outline the page draws, whose moves are the tree's.
fn outline_of(view: &ViewInstance, row: &str) -> String {
    fn holds(node: &UiNode, row: &str) -> bool {
        node.key == row || node.children.iter().any(|child| holds(child, row))
    }
    fn walk(node: &UiNode, row: &str) -> Option<String> {
        if node.kind == "ui.TreeView" && node.children.iter().any(|child| holds(child, row)) {
            return Some(node.key.clone());
        }
        node.children.iter().find_map(|child| walk(child, row))
    }
    walk(view.tree(), row).unwrap_or_else(|| panic!("no tree holding {row}"))
}

/// Move a task in a page's outline, as the tree reports it: `event` is how it lands — `drop`
/// just before the row `at`, `into` at the end of its own — carrying the task that moved and the
/// row it lands at.
fn move_in_outline(view: &ViewInstance, moved: &Value, event: &str, at: &Value) {
    let tree = outline_of(view, &row_of(view, at));
    let carried = Value::List(vec![moved.clone(), Value::some(at.clone())]);
    view.fire_with(&tree, event, Some(&carried)).unwrap();
}

/// The drawn node with this key, at any depth.
pub(crate) fn node_of<'a>(node: &'a UiNode, key: &str) -> Option<&'a UiNode> {
    if node.key == key {
        return Some(node);
    }
    node.children.iter().find_map(|child| node_of(child, key))
}

/// The task with this title.
pub(crate) fn task(r: &Replica, title: &str) -> Value {
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
    let parent = call(&ada, "new_task", create(&root, "Write the launch post"));
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

/// The page a task opens, drawn: its outline's rows built as a window that shows them all
/// builds them, so the test reads the rows a person sees rather than the ones a scroll would
/// have to reach.
pub(crate) fn task_page(ada: &Replica, task: &Value) -> ViewInstance {
    let Value::Ref(at) = task else {
        panic!("a task: {task:?}")
    };
    let mut view = ada.host.open_resource(&at.target, None).unwrap();
    view.set_lazy_rows(false).unwrap();
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
    move_in_outline(&view, &shots, "drop", &outline);
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
    move_in_outline(&view, &outline, "into", &draft);
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
    assert_eq!(
        node.props.get("depth"),
        Some(&Value::Int(1)),
        "one step in from the top of the page's outline"
    );
}

/// A task's page draws its steps as a tree whose rows the window fills as it shows them: the
/// tree holds every step, and a step's row says how deep it stands without the row it stands
/// under having been read, so a step indented under another draws one step in even where it is
/// the only row on screen.
#[test]
fn a_task_pages_outline_tells_the_window_where_each_step_stands() {
    let (mut ada, parent) = launch_post();
    call(
        &ada,
        "indent",
        vec![
            task(&ada, "Screenshots"),
            Value::some(task(&ada, "First draft")),
        ],
    );
    ada.commit();
    let Value::Ref(at) = &parent else {
        panic!("a task: {parent:?}")
    };
    let mut page = ada.host.open_resource(&at.target, None).unwrap();
    page.poll_patches().unwrap();
    let tree = key_of(&page, "ui.TreeView", &|node| {
        node.props.get("name") == Some(&Value::text("Subtasks"))
    });
    assert_eq!(
        page.list_len(&tree),
        Some(3),
        "the outline holds every step of the task"
    );
    assert!(
        all(page.tree(), "ui.TreeViewNode").is_empty(),
        "no step is drawn before the window shows it"
    );
    // The indented step, read first: neither the step over it nor its parent has been read.
    let row = page.row(&tree, 2).unwrap().expect("shown").clone();
    let node = all(&row, "ui.TreeViewNode").first().copied().unwrap();
    assert_eq!(node.props.get("label"), Some(&Value::text("Screenshots")));
    assert_eq!(
        node.props.get("depth"),
        Some(&Value::Int(1)),
        "one step in, of the row it stands under"
    );
    assert!(!flag(node, "branch"), "it holds no step of its own");
    let row = page.row(&tree, 1).unwrap().expect("shown").clone();
    let node = all(&row, "ui.TreeViewNode").first().copied().unwrap();
    assert_eq!(node.props.get("label"), Some(&Value::text("First draft")));
    assert_eq!(
        node.props.get("depth"),
        Some(&Value::Int(0)),
        "a step of the task itself stands at the top of its outline"
    );
    assert!(
        flag(node, "branch"),
        "it says it holds a step, so the window draws it foldable"
    );
}

/// The menu's Indent and Outdent are the moves `Ctrl+]` and `Ctrl+[` make: under the task over
/// it, and out beside the one over it.
#[test]
fn a_subtask_indents_over_itself_and_outdents_beside_its_parent() {
    let (mut ada, parent) = launch_post();
    let draft = task(&ada, "First draft");
    let shots = task(&ada, "Screenshots");
    // Under the task the list shows over it, which the row's menu carries as the move's place.
    call(
        &ada,
        "indent",
        vec![shots.clone(), Value::some(draft.clone())],
    );
    ada.commit();
    assert_eq!(
        target(&read(&ada, &shots, "parent")),
        target(&draft),
        "Ctrl+] puts it under the task just over it"
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
        "Ctrl+[ puts it back beside the task over it"
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

/// Every row of the home list, in the order it stands: its place, the key of the row the host
/// draws, the task it presents, and whether its box is drawn checked. A place that holds nothing
/// draws no list, and no rows.
pub(crate) fn home_list_rows(view: &mut ViewInstance) -> Vec<(usize, String, String, bool)> {
    view.poll_patches().unwrap();
    let lists = all(view.tree(), "ui.VirtualList");
    let Some(list) = lists.first() else {
        return Vec::new();
    };
    let list = list.key.clone();
    let mut out = Vec::new();
    for index in 0..view.list_len(&list).unwrap_or(0) {
        let Some(row) = view.row(&list, index).unwrap().cloned() else {
            continue;
        };
        let Some(item) = all(&row, "ui.ListItem").first().copied() else {
            continue;
        };
        let Some(Value::Ref(at)) = item.props.get("item") else {
            panic!("a row presents its task: {:?}", item.props.get("item"));
        };
        let Some(drawn) = all(&row, "ui.ListRow").first().copied() else {
            continue;
        };
        // The box is bound to whether the task is done: what a person sees it filled with.
        let checked = match view.bound_value(&drawn.key) {
            Some(Value::Bool(checked)) => checked,
            other => panic!("a row's box follows whether its task is done: {other:?}"),
        };
        out.push((index, drawn.key.clone(), at.target.id_string(), checked));
    }
    out
}

/// The titles of the tasks the home list shows, in the order it shows them.
pub(crate) fn home_list_titles(r: &Replica, view: &mut ViewInstance) -> Vec<String> {
    let titles: std::collections::HashMap<String, String> = items(r)
        .iter()
        .filter_map(|task| {
            let Value::Text(title) = read(r, task, "title") else {
                return None;
            };
            Some((target(task)?, title))
        })
        .collect();
    home_list_rows(view)
        .into_iter()
        .map(|(_, _, of, _)| {
            titles
                .get(&of)
                .cloned()
                .unwrap_or_else(|| panic!("the title of {of}"))
        })
        .collect()
}

/// The home list's row that presents `item`: its place, its key, and what its box draws.
fn home_row_of(view: &mut ViewInstance, item: &str) -> (usize, String, bool) {
    let row = home_list_rows(view)
        .into_iter()
        .find(|(_, _, of, _)| of == item);
    match row {
        Some((place, key, _, checked)) => (place, key, checked),
        None => panic!("no row presents {item}"),
    }
}

/// The row the home list draws for `item`, as a tree.
fn home_row_node(view: &mut ViewInstance, item: &str) -> UiNode {
    let (place, _, _) = home_row_of(view, item);
    let list = all(view.tree(), "ui.VirtualList")
        .first()
        .expect("the home list")
        .key
        .clone();
    view.row(&list, place)
        .unwrap()
        .expect("the row is drawn")
        .clone()
}

/// Switch the window's place, as a press on the sidebar's item does.
pub(crate) fn go_to(view: &mut ViewInstance, place: &str) {
    let key = key_of(view, "ui.NavItem", &|node| {
        node.props.get("label") == Some(&Value::text(place))
    });
    view.fire(&key, "select").unwrap();
    view.poll_patches().unwrap();
}

/// The entry at the foot of the list the window shows: where a person types what to add.
fn entry(view: &ViewInstance) -> String {
    key_of(view, "ui.TextInput", &|node| {
        matches!(
            node.props.get("entry"),
            Some(Value::Bool(true) | Value::Int(1))
        )
    })
}

/// Type `text` in the list's entry and press Enter, as a person adds a task.
fn enter(view: &mut ViewInstance, text: &str) {
    let key = entry(view);
    view.edit(&key, &Value::text(text)).unwrap();
    view.poll_patches().unwrap();
    view.fire_with(&key, "submit", Some(&Value::text(text)))
        .unwrap();
    view.poll_patches().unwrap();
}

/// Checking a task off in the place it stands keeps its row where it is, drawn checked, for as
/// long as the person stays; leaving the place settles the list. Pressing the box again while
/// the row lingers reopens the task where it stands.
#[test]
fn a_checked_task_keeps_its_row_until_the_place_is_left() {
    let (mut ada, root) = workspace();
    for title in ["Buy oat milk", "Pay the electricity bill", "Book a haircut"] {
        let made = call(&ada, "new_task", create(&root, title));
        call(
            &ada,
            "schedule",
            vec![made, Value::some(Value::text("2026-10-29"))],
        );
        ada.commit();
    }
    let mut view = ada.view(APP);
    go_to(&mut view, "Today");
    let bill = task(&ada, "Pay the electricity bill");
    let of = target(&bill).expect("a task");
    let before = home_list_rows(&mut view);
    assert_eq!(before.len(), 3, "three tasks are due today: {before:?}");
    let (place, key, checked) = home_row_of(&mut view, &of);
    assert!(!checked, "the box is empty");
    // A press on the row's box, as the person makes it.
    view.fire_with(&key, "check", None).unwrap();
    ada.commit();
    assert_eq!(read(&ada, &bill, "done"), Value::Bool(true), "it is done");
    let rows = home_list_rows(&mut view);
    assert_eq!(rows.len(), 3, "nothing left the list: {rows:?}");
    let (now, _, drawn) = home_row_of(&mut view, &of);
    assert_eq!(now, place, "in the place it stood");
    assert!(drawn, "its box drawn checked");
    // Typing the next task into the entry is not leaving the place: the row stays.
    view.set_view_state("draft", Value::text("Buy bread"));
    let rows = home_list_rows(&mut view);
    assert_eq!(rows.len(), 3, "typing settles nothing: {rows:?}");
    // Pressing the box again opens it where it is.
    let (place, key, _) = home_row_of(&mut view, &of);
    view.fire_with(&key, "check", None).unwrap();
    ada.commit();
    assert_eq!(read(&ada, &bill, "done"), Value::Bool(false), "reopened");
    let (again, _, drawn) = home_row_of(&mut view, &of);
    assert_eq!(again, place, "in the place it stood");
    assert!(!drawn, "its box drawn empty again");
    // Checked off once more, the row waits for the person to leave the place.
    let (_, key, _) = home_row_of(&mut view, &of);
    view.fire_with(&key, "check", None).unwrap();
    ada.commit();
    assert_eq!(home_list_rows(&mut view).len(), 3, "the row is still here");
    go_to(&mut view, "Inbox");
    go_to(&mut view, "Today");
    let rows = home_list_rows(&mut view);
    assert_eq!(rows.len(), 2, "the checked task is no longer today's");
    assert!(rows.iter().all(|(_, _, row, _)| *row != of));
}

/// Filing a task out of the inbox keeps its row in the list, drawn with the project it went
/// to, until the person leaves the view; coming back shows the inbox without it.
#[test]
fn a_task_filed_out_of_the_inbox_keeps_its_row_until_the_view_is_left() {
    let (mut ada, root) = workspace();
    let project = call(&ada, "new_project", vec![root.clone()]);
    call(
        &ada,
        "rename",
        vec![project.clone(), Value::text("Move flat")],
    );
    let packed = call(&ada, "new_task", create(&root, "Pack the books"));
    call(&ada, "new_task", create(&root, "Buy oat milk"));
    ada.commit();
    let mut view = ada.view(APP);
    go_to(&mut view, "Inbox");
    let of = target(&packed).expect("a task");
    let (place, _, _) = home_row_of(&mut view, &of);
    call(&ada, "file", vec![packed.clone(), project]);
    ada.commit();
    assert!(
        target(&read(&ada, &packed, "project")).is_some(),
        "it is in the project now"
    );
    let rows = home_list_rows(&mut view);
    assert_eq!(rows.len(), 2, "nothing left the list: {rows:?}");
    let (now, _, _) = home_row_of(&mut view, &of);
    assert_eq!(now, place, "in the place it stood");
    let row = home_row_node(&mut view, &of);
    assert_eq!(
        all(&row, "ui.ListRow")
            .first()
            .and_then(|drawn| drawn.props.get("secondary")),
        Some(&Value::text("Move flat")),
        "the row says where it went"
    );
    go_to(&mut view, "Today");
    go_to(&mut view, "Inbox");
    let rows = home_list_rows(&mut view);
    assert_eq!(rows.len(), 1, "the inbox no longer holds it");
    assert!(rows.iter().all(|(_, _, row, _)| *row != of));
}

/// A task somebody else completes leaves the list as any change of theirs does: it waits for
/// no one to leave the place.
#[test]
fn a_peers_completion_takes_its_row_away() {
    let (mut ada, root) = workspace();
    for title in ["Buy oat milk", "Pay the electricity bill"] {
        let made = call(&ada, "new_task", create(&root, title));
        call(
            &ada,
            "schedule",
            vec![made, Value::some(Value::text("2026-10-29"))],
        );
        ada.commit();
    }
    let mut ben = ada.fork("ben", PACKAGES);
    ben.host.sync_from(&ben.doc);
    let mut view = ada.view(APP);
    go_to(&mut view, "Today");
    assert_eq!(home_list_rows(&mut view).len(), 2);
    let of = target(&task(&ada, "Pay the electricity bill")).expect("a task");
    call(&ben, "check", vec![task(&ben, "Pay the electricity bill")]);
    ben.commit();
    ada.sync_with(&mut ben);
    let rows = home_list_rows(&mut view);
    assert_eq!(rows.len(), 1, "the row left: {rows:?}");
    assert!(rows.iter().all(|(_, _, row, _)| *row != of));
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
    go_to(&mut view, "Inbox");
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

/// The assignee a task holds, as its page reads it: who, or nobody.
#[cfg(feature = "record")]
fn assignee(r: &Replica, task: &Value) -> Option<String> {
    match read(r, task, "assignee") {
        Value::Opt(Some(held)) => match *held {
            Value::Principal(who) => Some(who.0),
            other => panic!("an assignee: {other:?}"),
        },
        Value::Opt(None) => None,
        other => panic!("an assignee: {other:?}"),
    }
}

/// The case a task's priority holds, by the case's name.
#[cfg(feature = "record")]
fn priority(r: &Replica, task: &Value) -> String {
    match read(r, task, "priority") {
        Value::Enum(case) => case.variant,
        other => panic!("a priority: {other:?}"),
    }
}

/// The key of the node bound to `field`.
#[cfg(feature = "record")]
pub(crate) fn bound_key(node: &UiNode, field: &str) -> Option<String> {
    if node
        .binding
        .as_ref()
        .and_then(|binding| binding.field())
        .is_some_and(|(_, bound)| bound == field)
    {
        return Some(node.key.clone());
    }
    node.children
        .iter()
        .find_map(|child| bound_key(child, field))
}

/// Every text the tree shows.
#[cfg(feature = "record")]
pub(crate) fn texts(node: &UiNode) -> Vec<String> {
    fn walk(node: &UiNode, out: &mut Vec<String>) {
        if let Some(Value::Text(text)) = node.props.get("label").or_else(|| node.props.get("value"))
        {
            out.push(text.clone());
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(node, &mut out);
    out
}

/// A task made by Ada in a workspace of Ada and Ben, and the page showing it: the list the
/// assignee's chip opens offers the workspace's members, and the key the page binds it to.
#[cfg(feature = "record")]
fn a_task_whose_assignee_the_page_shows() -> (Replica, Value, ViewInstance, String) {
    let (mut ada, root) = workspace();
    ada.host.runtime().set_members(
        vec![
            tilolang_runtime::members::Member::person("ada", "owner"),
            tilolang_runtime::members::Member::person("ben", "editor"),
        ],
        None,
    );
    let task = call(&ada, "new_task", create(&root, "Write the launch post"));
    ada.commit();
    let page = task_page(&ada, &task);
    let field = {
        let meta = &ada.host.package(APP).unwrap().meta;
        crate::fixtures::field_id(meta, "Task", "assignee")
    };
    let key = bound_key(page.tree(), &field).expect("the page binds its assignee");
    (ada, task, page, key)
}

/// The person picks `pick` for the task's assignee in the page's chip, and the page redraws.
#[cfg(feature = "record")]
fn pick_the_assignee(ada: &mut Replica, page: &mut ViewInstance, key: &str, pick: &Value) {
    page.edit(key, pick).unwrap();
    ada.commit();
    page.poll_patches().unwrap();
}

/// A task's assignee is one person field: a chip of who holds it that opens a list of "Me",
/// "No one" and the workspace's people, and shows each pick.
#[cfg(feature = "record")]
#[test]
fn the_assignee_is_one_person_field_whose_chip_shows_each_pick() {
    let (mut ada, task, mut page, key) = a_task_whose_assignee_the_page_shows();
    let node = node_of(page.tree(), &key).expect("the bound node is drawn");
    assert_eq!(
        node.kind, "ui.PersonField",
        "the assignee is one person field, not two buttons"
    );
    assert_eq!(
        node.props.get("people"),
        Some(&Value::List(vec![
            Value::Principal(Principal::new("ada")),
            Value::Principal(Principal::new("ben")),
        ])),
        "the list offers the workspace's members"
    );
    assert_eq!(
        node.props.get("me"),
        Some(&Value::Principal(Principal::new("ada"))),
        "the list leads with the person the page is shown to"
    );
    assert!(
        !texts(page.tree())
            .iter()
            .any(|text| text.starts_with("Assign to")),
        "the page no longer spells the assignee out as buttons: {:?}",
        texts(page.tree())
    );
    assert_eq!(assignee(&ada, &task), None, "nobody holds it yet");

    // Me, then another member, then nobody, as the person picks each from the list.
    pick_the_assignee(&mut ada, &mut page, &key, &Value::text("ada"));
    assert_eq!(assignee(&ada, &task), Some("ada".to_owned()));
    assert_eq!(
        page.bound_value(&key),
        Some(Value::some(Value::Principal(Principal::new("ada")))),
        "the chip shows the person it is bound to"
    );
    pick_the_assignee(&mut ada, &mut page, &key, &Value::text("ben"));
    assert_eq!(assignee(&ada, &task), Some("ben".to_owned()));
    pick_the_assignee(&mut ada, &mut page, &key, &Value::Text(String::new()));
    assert_eq!(assignee(&ada, &task), None, "No one clears it");
    assert_eq!(
        page.bound_value(&key),
        Some(Value::none()),
        "the chip shows nobody"
    );

    // The picks fold into one step of the task's history, the way one person's changes made in
    // a row do, and the step reads with what its last change did. The page itself draws the
    // assignee as a chip, with no label beside it.
    let shown = texts(page.tree());
    assert!(
        shown.join(" ").contains("You unassigned it"),
        "clearing the assignee last, the step says so: {shown:?}"
    );
    pick_the_assignee(&mut ada, &mut page, &key, &Value::text("ben"));
    let shown = texts(page.tree());
    assert!(
        shown.join(" ").contains("You assigned it to"),
        "assigned last, the step says that instead: {shown:?}"
    );
    assert!(
        !shown.iter().any(|text| text == "Assignee"),
        "the assignee is a chip, not a labelled field: {shown:?}"
    );
}

/// Picking the person the page shows, another member, and nobody is one change of the task's
/// history each, so one undo takes back one pick.
#[cfg(feature = "record")]
#[test]
fn each_pick_of_the_assignee_is_one_step_of_history() {
    use collaboration::history_source::verbs::HistoryEdit;

    let (mut ada, task, mut page, key) = a_task_whose_assignee_the_page_shows();
    for pick in [
        Value::text("ada"),
        Value::text("ben"),
        Value::Text(String::new()),
    ] {
        pick_the_assignee(&mut ada, &mut page, &key, &pick);
    }
    // One undo takes back one pick: the clearing, then Ben, then Me.
    let undo = |r: &mut Replica| {
        r.history(&HistoryEdit::Undo {
            steps: None,
            run: None,
            by: None,
            to: None,
            step: None,
        });
    };
    undo(&mut ada);
    assert_eq!(
        assignee(&ada, &task),
        Some("ben".to_owned()),
        "clearing it is one step"
    );
    undo(&mut ada);
    assert_eq!(
        assignee(&ada, &task),
        Some("ada".to_owned()),
        "picking Ben is one step"
    );
    undo(&mut ada);
    assert_eq!(assignee(&ada, &task), None, "picking Me is one step");
}

/// A task's priority is one case field: the page draws it as a chip of the case it holds, the
/// chip offers every case the type declares in that case's own glyph and tone, and a pick
/// writes the case into the task.
#[cfg(feature = "record")]
#[test]
fn the_priority_is_one_case_field_and_each_pick_writes_the_case() {
    let (mut ada, root) = workspace();
    let task = call(&ada, "new_task", create(&root, "Write the launch post"));
    ada.commit();
    let mut page = task_page(&ada, &task);
    let field = {
        let meta = &ada.host.package(APP).unwrap().meta;
        crate::fixtures::field_id(meta, "Task", "priority")
    };
    let key = bound_key(page.tree(), &field).expect("the page binds its priority");
    let node = node_of(page.tree(), &key).expect("the bound node is drawn");
    assert_eq!(node.kind, "ui.EnumField", "the priority is one case field");
    let declared = ada
        .host
        .package(APP)
        .unwrap()
        .meta
        .enums
        .iter()
        .find(|declared| declared.name == "Priority")
        .expect("the type declares its priority")
        .variants
        .clone();
    assert_eq!(
        declared
            .iter()
            .map(|case| case.label.as_str())
            .collect::<Vec<_>>(),
        ["No priority", "Low", "Medium", "High", "Urgent"],
        "the type's cases read as the app writes them"
    );
    let cases: Vec<(&str, &str)> = node
        .children
        .iter()
        .filter(|child| child.kind == "ui.EnumOption")
        .map(|child| {
            let text = |name: &str| match child.props.get(name) {
                Some(Value::Text(text)) => text.as_str(),
                other => panic!("a case's {name}: {other:?}"),
            };
            (text("name"), text("label"))
        })
        .collect();
    assert_eq!(
        cases,
        declared
            .iter()
            .map(|case| (case.id.as_str(), case.label.as_str()))
            .collect::<Vec<_>>(),
        "the chip offers every case the type declares, in its order"
    );
    let case = |name: &str| {
        node.children
            .iter()
            .find(|child| child.props.get("name") == Some(&Value::text(name)))
            .unwrap_or_else(|| panic!("the case {name}"))
    };
    let high = declared
        .iter()
        .find(|case| case.name == "high")
        .expect("the type declares its high priority");
    assert_eq!(
        case(&high.id).props.get("icon"),
        Some(&Value::text("flag")),
        "a case says the glyph it reads as"
    );
    assert_eq!(
        case(&high.id).props.get("tone"),
        Some(&Value::text("warning")),
        "a case says the tone it reads in"
    );
    assert_eq!(
        case(&declared[0].id).props.get("tone"),
        None,
        "a case that names no tone reads plainly"
    );

    // A pick writes the case into the task, held by the id the enum declares it by, and the
    // chip shows what the task holds.
    for picked in ["high", "urgent", "none"] {
        let held = declared
            .iter()
            .find(|case| case.name == picked)
            .expect("the type declares the case")
            .id
            .clone();
        page.edit(&key, &Value::text(picked)).unwrap();
        ada.commit();
        page.poll_patches().unwrap();
        assert_eq!(priority(&ada, &task), held, "a pick writes {picked}");
        assert_eq!(
            page.bound_value(&key),
            Some(read(&ada, &task, "priority")),
            "the chip shows the case the task holds"
        );
    }
}

/// A step added while someone else checks the task off leaves that task done with the step
/// open under it. A clear takes the finished branch and keeps the step: unfinished work is
/// never taken with a task that was checked off before it existed.
#[test]
fn a_step_added_while_a_task_is_checked_off_is_never_cleared_with_it() {
    let (mut ada, root) = workspace();
    let parent = call(&ada, "new_task", create(&root, "Write the launch post"));
    ada.commit();
    call(
        &ada,
        "add_subtask",
        vec![root.clone(), parent.clone(), Value::text("Outline")],
    );
    ada.commit();
    let mut ben = ada.fork("ben", PACKAGES);
    ben.host.sync_from(&ben.doc);
    // Ada checks the task off while Ben, not yet seeing that, puts a step under it.
    call(&ada, "check", vec![parent.clone()]);
    ada.commit();
    call(
        &ben,
        "add_subtask",
        vec![
            root.clone(),
            parent.clone(),
            Value::text("Gather the quotes"),
        ],
    );
    ben.commit();
    ada.sync_with(&mut ben);
    assert_eq!(
        read(&ada, &parent, "done"),
        Value::Bool(true),
        "checked off before the step was there"
    );
    assert_eq!(
        read(&ada, &task(&ada, "Gather the quotes"), "done"),
        Value::Bool(false),
        "the step came after the check"
    );

    // Outline is finished with nothing under it, so it goes; the task whose step is still open
    // stays with that step.
    assert_eq!(
        call(&ada, "clear_done", vec![root]),
        Value::Int(1),
        "one finished task went"
    );
    ada.commit();
    assert_eq!(
        titles(&ada),
        [
            Value::text("Write the launch post"),
            Value::text("Gather the quotes"),
        ],
        "the step that is not done stays under its task"
    );
}

/// Two people check the last two open steps of a branch at once: the branch folds once both
/// changes are in, though neither change names the task they are under, and opening one of them
/// unfolds it again.
#[test]
fn a_branch_folds_when_two_people_check_its_last_steps_at_once() {
    let (mut ada, root) = workspace();
    // In a project, whose list folds what is done with under the list: the place where a
    // finished branch stands in the Done tail.
    let project = call(&ada, "new_project", vec![root.clone()]);
    call(
        &ada,
        "rename",
        vec![project.clone(), Value::text("Website launch")],
    );
    let post = call(&ada, "new_task", create(&root, "Write the launch post"));
    call(&ada, "file", vec![post.clone(), project]);
    ada.commit();
    for title in ["Outline", "Screenshots"] {
        call(
            &ada,
            "add_subtask",
            vec![root.clone(), post.clone(), Value::text(title)],
        );
        ada.commit();
    }
    let outline = task(&ada, "Outline");
    let screenshots = task(&ada, "Screenshots");
    let mut ben = ada.fork("ben", PACKAGES);
    ben.host.sync_from(&ben.doc);
    // Each checks one step, neither seeing the other: one step under the task is still open
    // when each writes, so the branch is not finished in either of them.
    call(&ada, "check", vec![outline.clone()]);
    ada.commit();
    call(&ben, "check", vec![screenshots.clone()]);
    ben.commit();
    let of = |item: &Value| target(item).expect("a task");
    let at = |rows: &[(String, Option<String>, Option<String>, bool)], item: &Value| {
        let of = of(item);
        rows.iter()
            .position(|(row, _, _, _)| *row == of)
            .unwrap_or_else(|| panic!("the row of {of}"))
    };
    let mut view = ada.view(APP);
    go_to(&mut view, "Website launch");
    let rows = home_rows(&mut view);
    assert!(
        !rows[at(&rows, &post)].3,
        "Ada's copy still has a step open under the task"
    );

    ada.sync_with(&mut ben);
    let mut view = ada.view(APP);
    go_to(&mut view, "Website launch");
    let rows = home_rows(&mut view);
    assert!(
        rows[at(&rows, &post)].3,
        "the branch is finished once both steps are, so its row folds away into the Done group"
    );
    // The copy that took the change has both steps checked, so its own view code works out the
    // same folded branch: nothing of the fold travelled with the change.
    for step in [&outline, &screenshots] {
        assert_eq!(
            read(&ben, step, "done"),
            Value::Bool(true),
            "the other copy took the step the first one checked"
        );
    }

    // Opening one step unfinishes the branch, and the row leaves the tail.
    call(&ada, "check", vec![outline, Value::Bool(false)]);
    ada.commit();
    let mut view = ada.view(APP);
    go_to(&mut view, "Website launch");
    let rows = home_rows(&mut view);
    assert!(
        !rows[at(&rows, &post)].3,
        "a branch with an open step under it does not fold"
    );
    let rows = home_rows(&mut view);
    assert!(
        !rows[at(&rows, &post)].3,
        "and its row stands where it was, out of the Done group"
    );
}

/// A scenario's branch reads its counts and its fold from the tasks it staged, as soon as the
/// record has them: no migration runs after a record.
#[test]
fn a_scenario_reads_the_keys_of_the_branch_it_stages() {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), PACKAGES);
    let scenario = loaded
        .last()
        .unwrap()
        .meta
        .scenarios
        .iter()
        .find(|scenario| scenario.name == "a task's subtasks")
        .expect("Tasks stages a task's subtasks")
        .action
        .clone();
    // The packages' record holds no tasks yet: the app's root makes them.
    ada.host.root_of(APP).unwrap();
    ada.commit();
    ada.host
        .runtime()
        .invoke_action(&Principal::new("ada"), (scenario.as_str(), vec![]))
        .unwrap();
    ada.commit();
    let post = task(&ada, "Write the launch post");
    let outline = task(&ada, "Outline");
    let quotes = task(&ada, "Collect the quotes from the beta testers");
    let mut view = ada.view(APP);
    go_to(&mut view, "Website launch");
    let rows = home_rows(&mut view);
    let of = |item: &Value| target(item).expect("a task");
    let at = |item: &Value| {
        rows.iter()
            .position(|(row, _, _, _)| *row == of(item))
            .unwrap_or_else(|| panic!("the row of {:?}", of(item)))
    };
    assert_eq!(
        rows[at(&post)].2.as_deref(),
        Some("1/3"),
        "three steps under it, one of them done"
    );
    assert!(
        !rows[at(&post)].3,
        "it is not done, so the branch is unfinished"
    );
    assert_eq!(
        rows[at(&outline)].1,
        target(&post),
        "a done step with an open step beside it stands under its task"
    );
    assert!(
        !rows[at(&outline)].3,
        "a done task with an open step beside it does not fold"
    );
    assert_eq!(rows[at(&quotes)].2, None, "a leaf counts nothing");
}

/// A step names the day it is due only when that is not the day of the task it stands under:
/// the day the page around it already says, the step leaves unsaid.
#[test]
fn a_step_names_its_day_only_when_its_task_does_not() {
    let (mut ada, _) = workspace();
    let scenario = ada
        .host
        .package(APP)
        .unwrap()
        .meta
        .scenarios
        .iter()
        .find(|scenario| scenario.name == "a task that is late")
        .expect("Tasks stages a task that is late")
        .action
        .clone();
    let page = match ada
        .host
        .runtime()
        .invoke_action(&Principal::new("ada"), (scenario.as_str(), vec![]))
        .unwrap()
    {
        InvokeOutcome::Done(page) => page,
        other => panic!("{other:?}"),
    };
    ada.commit();
    let Value::Ref(task) = page else {
        panic!("the scenario hands back the task: {page:?}");
    };
    let mut window = ada
        .host
        .open_resource(&task.target, None)
        .expect("a task's page");
    let rows = step_rows(&mut window);
    let day = |title: &str| {
        rows.iter()
            .find(|(label, _, _)| label == title)
            .unwrap_or_else(|| panic!("{title}'s row"))
            .1
            .clone()
    };
    assert_eq!(
        day("Find the last service date"),
        None,
        "a step due the day its task is due repeats nothing the page says"
    );
    assert_eq!(
        day("Book the engineer's visit").as_deref(),
        Some("2026-07-02"),
        "a step due another day names it"
    );
    let kind = |title: &str| {
        rows.iter()
            .find(|(label, _, _)| label == title)
            .unwrap_or_else(|| panic!("{title}'s row"))
            .2
            .clone()
    };
    assert_eq!(
        kind("Book the engineer's visit").as_deref(),
        Some("date"),
        "the day it names is a day the host writes, not a string the app formats"
    );
}

/// What each step's row of a task's page says of the day it is due, by the step's title: the day
/// it gives, which kind of fact that is, and none where it leaves the day to the page.
fn step_rows(view: &mut ViewInstance) -> Vec<(String, Option<String>, Option<String>)> {
    view.set_lazy_rows(false).unwrap();
    view.poll_patches().unwrap();
    let tree = all(view.tree(), "ui.TreeView")
        .into_iter()
        .find(|tree| tree.props.get("name") == Some(&Value::text("Subtasks")))
        .expect("the page's subtasks");
    all(tree, "ui.TreeViewNode")
        .into_iter()
        .filter_map(|node| {
            let Some(Value::Text(label)) = node.props.get("label") else {
                return None;
            };
            let row = all(node, "ui.TreeViewRow").first().copied()?;
            let text = |name: &str| match row.props.get(name) {
                Some(Value::Text(text)) if !text.is_empty() => Some(text.clone()),
                _ => None,
            };
            Some((label.clone(), text("trailing"), text("trailing:as")))
        })
        .collect()
}

/// Every node of this kind in the drawn view.
pub(crate) fn all<'a>(node: &'a UiNode, kind: &str) -> Vec<&'a UiNode> {
    fn walk<'a>(node: &'a UiNode, kind: &str, out: &mut Vec<&'a UiNode>) {
        if node.kind == kind {
            out.push(node);
        }
        for child in &node.children {
            walk(child, kind, out);
        }
    }
    let mut out = Vec::new();
    walk(node, kind, &mut out);
    out
}

/// What each row of the home list presents: the task, the task it stands under, what its row says
/// of the task's subtasks, and whether it stands in the folded tail.
fn home_rows(view: &mut ViewInstance) -> Vec<(String, Option<String>, Option<String>, bool)> {
    let _ = view.set_lazy_rows(false);
    view.poll_patches().unwrap();
    let list = all(view.tree(), "ui.VirtualList")
        .first()
        .expect("the home list")
        .key
        .clone();
    let len = view.list_len(&list).unwrap_or(0);
    let mut out = Vec::new();
    for index in 0..len {
        let Some(row) = view.row(&list, index).unwrap() else {
            continue;
        };
        let Some(item) = all(row, "ui.ListItem").first().copied() else {
            continue;
        };
        let Some(Value::Ref(at)) = item.props.get("item") else {
            panic!("a row presents its task: {:?}", item.props.get("item"));
        };
        let count = match item.props.get("progress") {
            Some(Value::Text(count)) => Some(count.clone()),
            _ => None,
        };
        // A flag prop is the node's 0 or 1.
        let tail = match item.props.get("tail") {
            Some(Value::Bool(tail)) => *tail,
            Some(Value::Int(tail)) => *tail != 0,
            _ => false,
        };
        out.push((
            at.target.id_string(),
            item.props.get("parent").and_then(target),
            count,
            tail,
        ));
    }
    out
}

/// The home list is a tree: a task's subtasks stand under it, and its row says how many of them
/// are done, of all of them.
#[test]
fn the_home_list_nests_a_tasks_subtasks_under_its_row() {
    let (ada, parent) = launch_post();
    let mut view = ada.view(APP);
    go_to(&mut view, "Inbox");
    assert_eq!(
        all(view.tree(), "ui.VirtualList")
            .first()
            .and_then(|list| list.props.get("tree")),
        Some(&Value::Int(1)),
        "the home list draws the tasks as a tree"
    );
    let rows = home_rows(&mut view);
    let of = |item: &Value| target(item).expect("a task");
    let (_, above, count, _) = rows
        .iter()
        .find(|(item, _, _, _)| *item == of(&parent))
        .expect("the task's row");
    assert_eq!(above, &None, "the task stands at the top");
    assert_eq!(
        count.as_deref(),
        Some("0/3"),
        "its row says none of the three subtasks it has is done"
    );
    for title in ["Outline", "First draft", "Screenshots"] {
        let subtask = task(&ada, title);
        let (_, under, count, _) = rows
            .iter()
            .find(|(item, _, _, _)| *item == of(&subtask))
            .unwrap_or_else(|| panic!("{title}'s row"));
        assert_eq!(
            under,
            &target(&parent),
            "{title} stands under the task it belongs to"
        );
        assert_eq!(count, &None, "{title} has no subtasks of its own");
    }
}

/// A subtask done with under a task still open stands under it, checked and muted, rather than
/// folding away into the Done group: the list and the task's outline say the same count of a
/// row, so both read "1/3", and they hold the same subtasks in the same order.
#[test]
fn a_tasks_row_and_its_outline_count_what_is_done_alike() {
    let (mut ada, root) = workspace();
    // The task is in a project, whose list is where a step done with under an open task stands
    // under it: the list stands each step under the task it belongs to, and Done holds what is
    // done with.
    let project = call(&ada, "new_project", vec![root.clone()]);
    call(
        &ada,
        "rename",
        vec![project.clone(), Value::text("Website launch")],
    );
    let post = call(&ada, "new_task", create(&root, "Write the launch post"));
    call(&ada, "file", vec![post.clone(), project]);
    ada.commit();
    call(
        &ada,
        "add_subtask",
        vec![root.clone(), post.clone(), Value::text("Draft the post")],
    );
    ada.commit();
    let draft = task(&ada, "Draft the post");
    for title in ["Outline", "First draft", "Screenshots"] {
        call(
            &ada,
            "add_subtask",
            vec![root.clone(), draft.clone(), Value::text(title)],
        );
        ada.commit();
    }
    call(&ada, "check", vec![task(&ada, "Outline")]);
    ada.commit();

    let mut view = ada.view(APP);
    go_to(&mut view, "Website launch");
    let rows = home_rows(&mut view);
    let of = |item: &Value| target(item).expect("a task");
    let at = |item: &Value| {
        rows.iter()
            .position(|(row, _, _, _)| *row == of(item))
            .unwrap_or_else(|| panic!("the row of {:?}", of(item)))
    };
    assert_eq!(
        rows[at(&draft)].2.as_deref(),
        Some("1/3"),
        "the row says one of the three subtasks of the task is done"
    );
    let done = at(&task(&ada, "Outline"));
    assert_eq!(
        done,
        at(&draft) + 1,
        "the subtask done with stands right under the task it is under"
    );
    assert_eq!(
        rows[done].1,
        target(&draft),
        "under it, not at the top where a folded row stands"
    );
    assert!(!rows[done].3, "and not folded away into the Done group");

    let view = task_page(&ada, &post);
    let node = node_of(view.tree(), &row_of(&view, &draft)).expect("the draft's node");
    assert_eq!(
        node.props.get("progress"),
        Some(&Value::text("1/3")),
        "the outline says the same count of the same subtasks"
    );
    let subtask = node_of(view.tree(), &row_of(&view, &task(&ada, "Outline"))).expect("its node");
    assert_eq!(
        subtask.props.get("parent").and_then(target),
        target(&draft),
        "the subtask done with stands under the task it is under, in the outline too"
    );
    // Both hold the same subtasks of the step, in the same order.
    let listed: Vec<String> = rows
        .iter()
        .filter(|(_, above, _, _)| above == &target(&draft))
        .map(|(row, _, _, _)| row.clone())
        .collect();
    let outlined: Vec<String> = all(view.tree(), "ui.TreeViewNode")
        .iter()
        .filter(|node| node.props.get("parent").and_then(target) == target(&draft))
        .filter_map(|node| node.props.get("item").and_then(target))
        .collect();
    assert_eq!(listed, outlined, "the list and the outline agree");
}

/// One command a menu offers, as the person reads it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Command {
    pub(crate) label: String,
    pub(crate) key: Option<String>,
    pub(crate) group: Option<String>,
    pub(crate) reason: Option<String>,
    pub(crate) enabled: bool,
    pub(crate) destructive: bool,
}

fn flag(node: &UiNode, name: &str) -> bool {
    match node.props.get(name) {
        Some(Value::Bool(on)) => *on,
        Some(Value::Int(on)) => *on != 0,
        _ => false,
    }
}

fn text_of(node: &UiNode, name: &str) -> Option<String> {
    match node.props.get(name) {
        Some(Value::Text(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

/// A described `ui.MenuItem` as a command: it runs unless it says it is disabled, and says why
/// beside its label.
fn menu_command(node: &UiNode) -> Command {
    Command {
        label: text_of(node, "label").unwrap_or_default(),
        key: text_of(node, "key"),
        group: text_of(node, "group"),
        reason: text_of(node, "reason"),
        enabled: !flag(node, "disabled"),
        destructive: flag(node, "destructive"),
    }
}

/// The commands a task's menus offer, in the order a person reads them: every command, and of
/// the commands that need something the task may not have, the ones this task has. This is a
/// task in the inbox, with nothing standing over it and nothing under it: it is on no board to be
/// placed on, so it offers none of that, and it can still be moved under another task and copied
/// with the steps it has.
fn spec_commands() -> Vec<&'static str> {
    vec![
        "Complete",
        // It asks for the title it makes the task under: a task is never made unnamed.
        "Add subtask…",
        "Today",
        "Tomorrow",
        "Next week",
        "Pick date…",
        "No date",
        // It sets the moment the person is told of the task, once — the form it opens asks
        // when, so the command says it has a question — and clears it. They stand with the
        // commands that say when a task is due, before how much it matters.
        "Remind me…",
        "No reminder",
        "Remind",
        "No priority",
        "Low",
        "Medium",
        "High",
        "Urgent",
        "Project…",
        "Inbox",
        "Assign…",
        "Move to…",
        "Duplicate",
        "Delete",
    ]
}

fn labels(commands: &[Command]) -> Vec<&str> {
    commands
        .iter()
        .map(|command| command.label.as_str())
        .collect()
}

/// The home list's drawn row for a task.
fn home_row<'a>(view: &'a mut ViewInstance, want: &Value) -> &'a UiNode {
    view.poll_patches().unwrap();
    let list = all(view.tree(), "ui.VirtualList")
        .first()
        .expect("the home list")
        .key
        .clone();
    let len = view.list_len(&list).unwrap_or(0);
    let mut found = None;
    for index in 0..len {
        let stands = match view.row(&list, index) {
            Ok(Some(row)) => {
                all(row, "ui.ListItem")
                    .first()
                    .and_then(|item| item.props.get("item"))
                    .and_then(target)
                    == target(want)
            }
            _ => false,
        };
        if stands {
            found = Some(index);
            break;
        }
    }
    let index = found.unwrap_or_else(|| panic!("no drawn row for the task"));
    view.row(&list, index)
        .expect("the row it listed")
        .expect("the row it listed")
}

/// The menu a task's row offers: the slot of the item the row presents, not one of a row drawn
/// inside it — a task opened in the row holds rows of its own.
fn row_menu<'a>(view: &'a mut ViewInstance, want: &Value) -> &'a UiNode {
    let row = home_row(view, want);
    let item = all(row, "ui.ListItem")
        .into_iter()
        .find(|item| item.props.get("item").and_then(target) == target(want))
        .unwrap_or_else(|| panic!("the row presents the task: {row:?}"));
    let menu = |node: &'a UiNode| {
        node.kind == "slot" && node.props.get("name") == Some(&Value::text("menu"))
    };
    item.children
        .iter()
        .find(|child| menu(child))
        .unwrap_or_else(|| panic!("the row offers a menu: {row:?}"))
}

/// The commands a task's row offers from its own menu.
fn row_commands(view: &mut ViewInstance, want: &Value) -> Vec<Command> {
    all(row_menu(view, want), "ui.MenuItem")
        .into_iter()
        .map(menu_command)
        .collect()
}

/// The command a task's row runs when it is swiped, as its label reads.
fn row_swipe(view: &mut ViewInstance, want: &Value) -> Option<String> {
    let row = home_row(view, want);
    let swipe = all(row, "slot")
        .into_iter()
        .find(|slot| slot.props.get("name") == Some(&Value::text("swipe")))?;
    let action = all(swipe, "ui.SwipeAction").into_iter().next()?;
    text_of(action, "label")
}

/// The key of the command `label` of a task's row menu: the node running it names.
fn row_command_key(view: &mut ViewInstance, want: &Value, label: &str) -> String {
    all(row_menu(view, want), "ui.MenuItem")
        .into_iter()
        .find(|item| text_of(item, "label").as_deref() == Some(label))
        .unwrap_or_else(|| panic!("the row offers {label}"))
        .key
        .clone()
}

/// The commands a page offers behind its `...` door.
pub(crate) fn page_commands(view: &ViewInstance) -> Vec<Command> {
    let more = all(view.tree(), "ui.MenuButton")
        .into_iter()
        .find(|button| button.key.ends_with("~more"))
        .unwrap_or_else(|| {
            panic!(
                "the page keeps its commands behind a \"…\": {:#?}",
                view.tree()
            )
        });
    all(more, "ui.MenuItem")
        .into_iter()
        .map(menu_command)
        .collect()
}

/// The commands a task's row places before any other: the row's own way to the task's page,
/// which no other surface offers, then the view's own order of what a person opens the row for.
fn placed_commands() -> Vec<&'static str> {
    vec![
        "Open as page",
        "Complete",
        "Pick date…",
        "Assign…",
        "Delete",
    ]
}

/// A task's row opens with the commands its view places for it — checking it off, when it is
/// due, who is doing it, and taking it away — and keeps every other command the task takes
/// behind its "More", worded once and in the order the app declares them.
#[test]
fn a_tasks_row_opens_with_the_commands_its_view_places() {
    let (mut ada, root) = workspace();
    let booked = call(&ada, "new_task", create(&root, "Book a haircut"));
    ada.commit();
    let mut view = ada.view(APP);
    go_to(&mut view, "Inbox");
    let commands = row_commands(&mut view, &booked);
    let placed = placed_commands();
    let rest: Vec<&str> = spec_commands()
        .into_iter()
        .filter(|label| !placed.contains(label))
        .collect();
    let mut expected = placed.clone();
    expected.extend(rest.iter().copied());
    assert_eq!(labels(&commands), expected, "as a person reads them");
    assert_eq!(
        commands[1].key.as_deref(),
        Some("Ctrl+enter"),
        "the keys the app declares show as the keyboard spells them"
    );
    assert_eq!(
        commands[1].group, None,
        "a command its view places on its own stands on its own"
    );
    assert_eq!(
        commands[2].group.as_deref(),
        Some("Plan"),
        "the view puts the date picker in a run of its own"
    );
    assert!(
        commands[placed.len()..]
            .iter()
            .all(|command| command.group.as_deref() == Some("More")),
        "what the view left over stands behind its \"More\": {commands:?}"
    );
    let at = |label: &str| {
        commands
            .iter()
            .position(|command| command.label == label)
            .unwrap_or_else(|| panic!("the menu offers {label}"))
    };
    assert!(
        !commands[at("No date")].enabled
            && commands[at("No date")].reason.as_deref() == Some("It has no date"),
        "a command that cannot run stays listed and says why: {:?}",
        commands[at("No date")]
    );
    assert!(
        !commands[at("No priority")].enabled
            && commands[at("No priority")].reason.as_deref() == Some("It has no priority"),
        "what it already is cannot be chosen: {:?}",
        commands[at("No priority")]
    );
    assert_eq!(
        commands[at("Duplicate")].key.as_deref(),
        Some("Ctrl+Shift+d"),
        "Duplicate"
    );
    let delete = &commands[at("Delete")];
    assert!(
        delete.destructive,
        "delete is drawn as taking something away"
    );
    assert_eq!(delete.key.as_deref(), Some("delete"));
}

/// Every command is still offered, once: a row places the ones its view names, keeps the rest
/// behind its "More" in the order the app declares them, and the page of the task holds them
/// all behind its "…" in that declared order.
#[test]
fn the_commands_a_task_takes_beside_every_other_stand_in_the_one_order() {
    let (mut ada, root) = workspace();
    let flat = call(&ada, "new_project", vec![root.clone()]);
    let booked = call(&ada, "new_task", create(&root, "Book a haircut"));
    call(
        &ada,
        "add_subtask",
        vec![root, booked.clone(), Value::text("Find the number")],
    );
    call(&ada, "file", vec![booked.clone(), flat]);
    ada.commit();
    let mut home = ada.view(APP);
    go_to(&mut home, "New project");
    let commands = row_commands(&mut home, &booked);
    let row = labels(&commands);
    let placed = placed_commands();
    assert_eq!(
        &row[..placed.len()],
        placed.as_slice(),
        "the row opens with what its view places: {row:?}"
    );
    let mut rest: Vec<&str> = spec_commands()
        .into_iter()
        .filter(|label| !placed.contains(label))
        .collect();
    rest.push("Place in board…");
    assert_eq!(
        &row[placed.len()..],
        rest.as_slice(),
        "and the rest keeps the order the app declares: {row:?}"
    );
    assert_eq!(
        row.last().copied(),
        Some("Place in board…"),
        "the board's place stands among the rest, where the app declares it: {row:?}"
    );
    let page = page_commands(&task_page(&ada, &booked));
    let mut declared = spec_commands();
    declared.push("Place in board…");
    assert_eq!(
        labels(&page),
        declared,
        "the page offers every command, in the order the app declares"
    );
}

/// A window whose type declares more than one view offers them as one labeled switch, in the
/// order it declares them, the view it shows chosen; the commands beside the switch run once and
/// no action is drawn as a switch of its own.
#[test]
fn the_declared_views_are_the_bars_labeled_switch() {
    let (mut ada, root) = workspace();
    let project = call(&ada, "new_project", vec![root]);
    ada.commit();
    let Value::Ref(at) = &project else {
        panic!("a project: {project:?}")
    };
    let mut view = ada.host.open_resource(&at.target, None).unwrap();
    view.poll_patches().unwrap();
    let switch = all(view.tree(), "ui.ViewTabs")
        .into_iter()
        .next()
        .expect("the project's window switches between the views it declares");
    let segments: Vec<String> = all(switch, "ui.Segment")
        .into_iter()
        .filter_map(|segment| text_of(segment, "label"))
        .collect();
    assert_eq!(
        segments,
        ["List", "Board"],
        "the views it declares, in the order it declares them, each saying what it adds to the type's name"
    );
    let icons: Vec<Option<String>> = all(switch, "ui.Segment")
        .into_iter()
        .map(|segment| text_of(segment, "icon"))
        .collect();
    assert_eq!(
        icons,
        [Some("list".to_owned()), Some("columns-2".to_owned())],
        "each view says the glyph it reads as"
    );
    assert_eq!(
        switch.props.get("selected"),
        Some(&Value::Opt(Some(Box::new(Value::Int(0))))),
        "the view the window shows is the one chosen"
    );
    let mut toggles = Vec::new();
    let mut commands = Vec::new();
    for button in all(view.tree(), "ui.Button") {
        let Some(label) = text_of(button, "label") else {
            continue;
        };
        if button.props.contains_key("toggled") {
            toggles.push(label);
        } else {
            commands.push(label);
        }
    }
    assert!(
        toggles.is_empty(),
        "no action is drawn as a switch: {toggles:?}"
    );
    assert!(
        ["New task", "New section", "Rename…"]
            .iter()
            .all(|label| commands.contains(&label.to_string())),
        "and the commands beside it are buttons: {commands:?}"
    );
}

/// A command whose place this view does not have is not among the commands it offers: a task
/// with nothing over it in its day cannot be indented under it, though the store holds another
/// day's task before it, and the one under a task it can see is. Running the one offered nests
/// it under the task the person sees over it.
#[test]
fn the_moves_a_row_offers_follow_the_rows_the_view_shows() {
    let (mut ada, root) = workspace();
    // What is overdue and what is due today, in the day's place: the list shows the day gone
    // by first, and the time before today stands over today's tasks in the store.
    let van = call(&ada, "new_task", create(&root, "Book the van"));
    let tent = call(&ada, "new_task", create(&root, "Pack the tent"));
    let tank = call(&ada, "new_task", create(&root, "Fill the tank"));
    ada.commit();
    for (task, day) in [
        (&van, "2026-10-27"),
        (&tent, "2026-10-29"),
        (&tank, "2026-10-29"),
    ] {
        call(
            &ada,
            "schedule",
            vec![task.clone(), Value::some(Value::text(day))],
        );
    }
    ada.commit();

    let mut view = ada.view(APP);
    go_to(&mut view, "Today");
    let offered = |view: &mut ViewInstance, task: &Value| {
        labels(&row_commands(view, task))
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<String>>()
    };
    assert!(
        !offered(&mut view, &van).contains(&"Indent".to_owned()),
        "the first task of its day has nothing over it to go under"
    );
    assert!(
        !offered(&mut view, &tent).contains(&"Indent".to_owned()),
        "and neither has the first of another day, though a task of the day before stands \
         over it in the store"
    );
    assert!(
        offered(&mut view, &tank).contains(&"Indent".to_owned()),
        "the one under a task of its own day does: {:?}",
        offered(&mut view, &tank)
    );
    assert!(
        !offered(&mut view, &van).contains(&"Outdent".to_owned()),
        "and a task at the top level has none to move out of"
    );
    assert_ne!(
        row_swipe(&mut view, &van).as_deref(),
        Some("Indent"),
        "and the swipe over the row runs no move it has no place for either"
    );

    // Running it nests it under the task over it, which is the one of its own day.
    let key = row_command_key(&mut view, &tank, "Indent");
    let indent = view
        .event_action(&key, "choose")
        .expect("Indent runs where it is offered");
    ada.host
        .runtime()
        .invoke_action(
            &Principal::new("ada"),
            (indent.action.as_str(), indent.args.clone()),
        )
        .expect("indented");
    ada.commit();
    assert_eq!(
        target(&read(&ada, &tank, "parent")),
        target(&tent),
        "under the task the list shows over it, never one of another day"
    );
    let mut view = ada.view(APP);
    go_to(&mut view, "Today");
    assert!(
        offered(&mut view, &tank).contains(&"Outdent".to_owned()),
        "and now it has a parent to move out of: {:?}",
        offered(&mut view, &tank)
    );

    // The task's own window shows no row over it, so it offers no move that needs one.
    let page = page_commands(&task_page(&ada, &tank));
    assert!(
        !labels(&page).contains(&"Indent"),
        "the window offers no Indent: {:?}",
        labels(&page)
    );
}

/// A name in the code, as a control's binding spells it: lowercase words joined by underscores,
/// or one lowercase word (`draft`, `step`). A person hears it read out as though the app's code
/// were the word for what they are typing in.
fn identifier_shaped(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A control that takes typing is announced by what the view says it is for — its placeholder —
/// never by the name in the code it binds: the list's entry says "New task", the entry over a
/// task's subtasks "Add a subtask". A control left without a placeholder is announced by the
/// binding's own name, so every one of them says what it is for.
#[test]
fn a_typing_control_is_named_by_a_word_a_person_says() {
    let (mut ada, root) = workspace();
    let task = call(&ada, "new_task", create(&root, "Book the van"));
    ada.commit();
    let mut home = ada.view(APP);
    go_to(&mut home, "Inbox");
    let mut page = task_page(&ada, &task);
    let mut said: Vec<String> = Vec::new();
    for (where_, view) in [("the list", &mut home), ("a task", &mut page)] {
        for field in all(view.tree(), "ui.TextInput") {
            let placeholder = text_of(field, "placeholder").unwrap_or_default();
            assert!(
                !placeholder.is_empty(),
                "the field in {where_} says what it takes typing for: {field:?}"
            );
            assert!(
                !identifier_shaped(&placeholder),
                "the field in {where_} is named `{placeholder}`, which is a name in the code \
                 rather than a word a person says: {field:?}"
            );
            said.push(placeholder);
        }
    }
    assert!(
        said.contains(&"New task".to_owned()),
        "the list's entry says what it adds: {said:?}"
    );
    assert!(
        said.contains(&"Add a subtask".to_owned()),
        "and a task's entry says what it adds: {said:?}"
    );
}

/// A task's page offers every command the task takes, in the order the app declares them, with
/// the same keys and the same reasons its row gives them: the row's view places a few of them up
/// front, and neither surface loses one.
#[test]
fn a_tasks_page_menu_offers_what_its_row_does() {
    let (mut ada, root) = workspace();
    let booked = call(&ada, "new_task", create(&root, "Book a haircut"));
    ada.commit();
    let mut home = ada.view(APP);
    go_to(&mut home, "Inbox");
    let row = row_commands(&mut home, &booked);
    let page = task_page(&ada, &booked);
    let behind_the_door = page_commands(&page);
    assert_eq!(labels(&behind_the_door), spec_commands());
    let mut offered = labels(&behind_the_door);
    offered.sort_unstable();
    // The row also offers its own way to the page, which a page has no need of: what the two
    // surfaces are held to is the commands the task takes.
    let mut placed: Vec<&str> = labels(&row)
        .into_iter()
        .filter(|label| spec_commands().contains(label))
        .collect();
    placed.sort_unstable();
    assert_eq!(
        offered, placed,
        "the row and the page offer the same commands"
    );
}

/// Adding a subtask from a menu asks for its title and makes it right under the task, after the
/// steps it already has, named: no task stands unnamed for a person to leave behind.
#[test]
fn adding_a_subtask_from_a_menu_asks_for_its_title_and_names_it_under_its_row() {
    let (mut ada, root) = workspace();
    let parent = call(&ada, "new_task", create(&root, "Write the launch post"));
    call(
        &ada,
        "add_subtask",
        vec![root.clone(), parent.clone(), Value::text("Outline")],
    );
    ada.commit();
    // The menu reads as asking for the title the task is made under.
    let mut view = ada.view(APP);
    go_to(&mut view, "Inbox");
    let add = row_commands(&mut view, &parent)
        .into_iter()
        .find(|command| command.label.starts_with("Add subtask"))
        .unwrap_or_else(|| panic!("the task's menu offers the add"));
    assert_eq!(
        add.label, "Add subtask…",
        "the title is asked for, not left for a row to be named in"
    );
    // What the form collects is the title it adds: named, under the task, after the steps it
    // already has.
    let made = call(
        &ada,
        "add_subtask",
        vec![root, parent.clone(), Value::text("Collect the quotes")],
    );
    ada.commit();
    assert_eq!(
        read(&ada, &made, "title"),
        Value::text("Collect the quotes"),
        "it stands under the title given"
    );
    assert_eq!(
        target(&read(&ada, &made, "parent")),
        target(&parent),
        "it stands under the task the menu was opened on"
    );
    assert_eq!(
        titles(&ada),
        [
            Value::text("Write the launch post"),
            Value::text("Outline"),
            Value::text("Collect the quotes"),
        ],
        "and after the steps it already has"
    );
}

/// Copying a task copies every task under it, level after level, as one step of history: one
/// undo takes the copy and the steps it brought away together.
#[cfg(feature = "record")]
#[test]
fn duplicating_a_task_copies_every_task_under_it_in_one_step() {
    use collaboration::history_source::verbs::HistoryEdit;

    let (mut ada, root) = workspace();
    let parent = call(&ada, "new_task", create(&root, "Plan the trip"));
    let step = call(
        &ada,
        "add_subtask",
        vec![root.clone(), parent.clone(), Value::text("Book a room")],
    );
    call(
        &ada,
        "add_subtask",
        vec![root.clone(), step, Value::text("Check the prices")],
    );
    ada.commit();
    let copy = call(&ada, "duplicate", vec![root, parent.clone()]);
    ada.commit();
    let made = items(&ada);
    assert_eq!(
        titles(&ada),
        [
            Value::text("Plan the trip"),
            Value::text("Book a room"),
            Value::text("Check the prices"),
            Value::text("Plan the trip"),
            Value::text("Book a room"),
            Value::text("Check the prices"),
        ],
        "the copy stands after the task it copies, with every task under it"
    );
    assert_eq!(
        target(&read(&ada, &copy, "parent")),
        target(&read(&ada, &parent, "parent")),
        "the copy stands where the task it copies stands"
    );
    assert_eq!(
        target(&read(&ada, &made[5], "parent")),
        target(&made[4]),
        "a copied step stands under the copy of the task it stood under"
    );
    assert_eq!(
        target(&read(&ada, &made[4], "parent")),
        target(&copy),
        "and the first of them under the copy itself"
    );
    ada.history(&HistoryEdit::Undo {
        steps: None,
        run: None,
        by: None,
        to: None,
        step: None,
    });
    ada.commit();
    assert_eq!(
        titles(&ada),
        [
            Value::text("Plan the trip"),
            Value::text("Book a room"),
            Value::text("Check the prices"),
        ],
        "one undo takes the copy and every task it brought with it"
    );
}

/// Every action the app declares is offered wherever actions are listed: a person chooses it from
/// a menu or the palette, an agent from its choices, and a program names it. Nothing is reserved
/// for a gesture alone.
#[test]
fn every_declared_action_is_offered() {
    let (ada, _) = workspace();
    let meta = &ada.host.package(APP).unwrap().meta;
    let offered: Vec<&str> = meta
        .offered_actions()
        .map(|action| action.name.as_str())
        .collect();
    for wanted in [
        "add_subtask",
        "duplicate",
        "due_today",
        "due_tomorrow",
        "due_next_week",
        "due_clear",
        "set_priority_low",
        "assign",
        "file",
        "indent",
        "outdent",
        "remove",
        "place",
        "move_to",
    ] {
        assert!(
            offered.contains(&wanted),
            "{wanted} is offered: {offered:?}"
        );
    }
    let meta = &ada.host.package(APP).unwrap().meta;
    let declared: Vec<&str> = meta
        .actions
        .iter()
        .filter(|a| !a.scenario)
        .map(|a| a.name.as_str())
        .collect();
    assert_eq!(
        offered, declared,
        "every action is offered, in declaration order"
    );
}

/// An action run on a task names the kind it is published on, a task's, not the record's that
/// holds the task: the workspace decides the change against the actions of that kind, and a
/// change naming the record's kind with a task's action is refused.
#[test]
fn a_tasks_action_names_the_kind_of_a_task() {
    let (ada, root) = workspace();
    let made = call(&ada, "new_task", create(&root, "Water the plants"));
    let _ = ada.host.take_committed();
    call(&ada, "check", vec![made]);
    let kinds: Vec<String> = ada
        .host
        .take_committed()
        .into_iter()
        .map(|committed| committed.action.kind)
        .collect();
    assert_eq!(kinds, ["tl.so.tilo.lang.tasks.Task"], "{kinds:?}");
}
