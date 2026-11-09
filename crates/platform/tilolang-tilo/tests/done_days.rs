//! Done as a log of the week: the tasks finished under the day each was finished on, newest
//! first, and a task finished before the app kept that moment under the day the log says.

use automerge::AutoCommit;
use tilolang_runtime::{Value, ViewInstance};

use crate::fixtures::Replica;
use crate::tasks::{all, call, go_to, home_list_titles, workspace};

const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");
const DISCUSSION_IDS: &str = include_str!("../../../../apps/tl/discussion/app.ids");
const TASKS: &str = crate::fixtures::TASKS_SOURCE;
const TASKS_IDS: &str = include_str!("../../../../apps/tl/tasks/app.ids");
const APP: &str = "so.tilo.lang.tasks";

/// Each row of the home list as the list groups it: the heading its run stands under, and
/// whether that heading says the day the task was finished rather than the day it is due.
fn home_row_groups(view: &mut ViewInstance) -> Vec<(String, bool)> {
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
        let group = match item.props.get("group") {
            Some(Value::Text(at)) => at.clone(),
            Some(Value::Opt(Some(at))) => match at.as_ref() {
                Value::Text(at) => at.clone(),
                other => panic!("a row's heading: {other:?}"),
            },
            other => panic!("a row's heading: {other:?}"),
        };
        let done = match item.props.get("group_done") {
            Some(Value::Int(on)) => *on != 0,
            Some(Value::Bool(on)) => *on,
            None => false,
            other => panic!("a heading says whether the task was finished: {other:?}"),
        };
        out.push((group, done));
    }
    out
}

/// Done stands what was finished under the day it was finished on, newest first, as a log of
/// the week: the day a task is due never orders it, and a task opened again keeps no day there.
#[test]
fn done_stands_what_was_finished_under_the_day_it_was_finished() {
    let (mut ada, root) = workspace();
    let finish = |ada: &mut Replica, title: &str, at: &str| {
        let at = at.to_owned();
        ada.host.set_clock(move || at.clone());
        let made = call(ada, "new_task", crate::tasks::create(&root, title));
        call(ada, "check", vec![made.clone()]);
        ada.commit();
        made
    };
    // Two finished on Monday, one on Wednesday, and one of them due on a day of its own.
    finish(&mut ada, "Pay the electricity bill", "2026-10-26T09:00:00Z");
    finish(&mut ada, "Book a haircut", "2026-10-26T15:00:00Z");
    let milk = finish(&mut ada, "Buy oat milk", "2026-10-28T18:00:00Z");
    call(
        &ada,
        "schedule",
        vec![milk.clone(), Value::some(Value::text("2026-10-30"))],
    );
    ada.commit();

    ada.host.set_clock(|| "2026-10-29T10:00:00Z".to_owned());
    let mut view = ada.view(APP);
    go_to(&mut view, "Done");
    assert_eq!(
        home_list_titles(&ada, &mut view),
        ["Buy oat milk", "Book a haircut", "Pay the electricity bill"],
        "what was finished last stands first"
    );
    assert_eq!(
        home_row_groups(&mut view),
        [
            ("2026-10-28".to_owned(), true),
            ("2026-10-26".to_owned(), true),
            ("2026-10-26".to_owned(), true),
        ],
        "each run of a day finished under that day, never under the day it is due"
    );

    // A task opened again is not done, and keeps no day Done could group it by: the version
    // that last changed its box is the one that opened it.
    call(&ada, "check", vec![milk, Value::Bool(false)]);
    ada.commit();
    let mut view = ada.view(APP);
    go_to(&mut view, "Done");
    assert_eq!(
        home_list_titles(&ada, &mut view),
        ["Book a haircut", "Pay the electricity bill"]
    );
    assert_eq!(
        home_row_groups(&mut view),
        [
            ("2026-10-26".to_owned(), true),
            ("2026-10-26".to_owned(), true),
        ],
        "the tasks still finished keep the days they were finished on"
    );
}

/// A task finished by a version that kept no moment of it stands under the day of the change
/// that finished it, read from the log, rather than under no day at all.
#[test]
fn a_task_finished_before_the_app_kept_its_moment_keeps_its_day() {
    const BEFORE: &str = include_str!("tasks_before_done_at/app.tl");
    const BEFORE_IDS: &str = include_str!("tasks_before_done_at/app.ids");
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[(BEFORE, BEFORE_IDS)]);
    ada.host.set_clock(|| "2026-10-26T09:00:00Z".to_owned());
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let old = call(&ada, "add", vec![all, Value::text("Send the invoice")]);
    ada.commit();
    call(&ada, "check", vec![old]);
    ada.commit();

    // The same workspace, opened by the version that keeps nothing of it but the box.
    let (mut later, _) = Replica::new(
        "ada",
        ada.doc.fork(),
        &[(DISCUSSION, DISCUSSION_IDS), (TASKS, TASKS_IDS)],
    );
    later.host.sync_from(&later.doc);
    later.host.migrate();
    for done in later.host.take_committed() {
        later.apply(&done.action);
    }
    later.host.set_clock(|| "2026-10-29T10:00:00Z".to_owned());
    let mut view = later.view(APP);
    go_to(&mut view, "Done");
    assert_eq!(
        home_row_groups(&mut view),
        [("2026-10-26".to_owned(), true)],
        "the day the log says it was finished on"
    );
}
