//! The Today place is one filter of the app's own: what is due today or late, and what the
//! person looking holds, whenever it is due.

use tilolang_runtime::{Principal, Value};

use crate::fixtures::{Replica, field_id};
use crate::tasks::{
    APP, PACKAGES, call, count, create, go_to, home_list_titles, places, workspace,
};
use automerge::AutoCommit;

/// Ada's week and Ben's copy of it: a task each of them holds with no day of its own, a bill due
/// today nobody holds, a task due today that is done, and one due later.
fn week() -> (Replica, Replica) {
    let (mut ada, root) = workspace();
    let held = call(&ada, "new_task", create(&root, "Call the landlord"));
    let bill = call(&ada, "new_task", create(&root, "Pay the electricity bill"));
    let report = call(&ada, "new_task", create(&root, "Send the report"));
    let bins = call(&ada, "new_task", create(&root, "Take the bins out"));
    let present = call(&ada, "new_task", create(&root, "Order a present"));
    ada.commit();
    for (task, who) in [(&held, "ada"), (&report, "ben")] {
        call(
            &ada,
            "assign",
            vec![
                task.clone(),
                Value::some(Value::Principal(Principal::new(who))),
            ],
        );
    }
    for (task, day) in [(&bill, "2026-10-29"), (&bins, "2026-10-29")] {
        call(
            &ada,
            "schedule",
            vec![task.clone(), Value::some(Value::text(day))],
        );
    }
    call(
        &ada,
        "schedule",
        vec![present, Value::some(Value::text("2026-10-30"))],
    );
    call(&ada, "check", vec![bins]);
    ada.commit();
    let ben = ada.fork("ben", PACKAGES);
    ben.host.sync_from(&ben.doc);
    ben.host.set_clock(|| "2026-10-29T10:00:00Z".to_owned());
    (ada, ben)
}

#[test]
fn today_is_what_is_due_and_what_the_person_looking_holds() {
    let (ada, ben) = week();
    let mut view = ada.view(APP);
    view.set_lazy_rows(false).unwrap();
    go_to(&mut view, "Today");
    assert_eq!(
        home_list_titles(&ada, &mut view),
        ["Pay the electricity bill", "Call the landlord"],
        "the day's work stands under its day, then the undated task Ada holds: not the one she \
         finished, not the one due later, and not the one Ben holds"
    );
    assert_eq!(count(&view, "Today"), Some(2), "{:?}", places(&view));

    // Ben's Today holds the same days, with the task he holds in place of hers.
    let mut theirs = ben.view(APP);
    theirs.set_lazy_rows(false).unwrap();
    go_to(&mut theirs, "Today");
    assert_eq!(
        home_list_titles(&ben, &mut theirs),
        ["Pay the electricity bill", "Send the report"],
        "a task nobody holds is due for everyone; what each holds stands in their own Today"
    );
    assert_eq!(count(&theirs, "Today"), Some(2), "{:?}", places(&theirs));
}

/// A workspace of `n` tasks: every tenth due today, every tenth held by Ada, the rest due later.
fn seeded(n: usize) -> Replica {
    use serde_json::json;
    use tilo_resource::{Collection, Id, Model as _};
    use tilolang_tilo::action::{DynAction, DynEdit};
    use tilolang_tilo::record::{DynRecord, DynValue};

    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), PACKAGES);
    ada.host.set_clock(|| "2026-10-29T10:00:00Z".to_owned());
    let meta = packages[1].meta.clone();
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    let task_ty = meta.types.iter().find(|t| t.name == "Task").unwrap();
    let schema = tilolang_tilo::schema::lookup(&task_ty.id).unwrap();
    let mut items = Collection::default();
    for i in 0..n {
        let mut item = DynRecord::empty(schema).unwrap();
        if let Some(DynValue::Text(title)) = item.get_mut(&field_id(&meta, "Task", "title")) {
            *title = format!("Task {i:05}");
        }
        let day = if i < 3 { "2026-10-29" } else { "2027-01-01" };
        if let Some(DynValue::Register(due)) = item.get_mut(&field_id(&meta, "Task", "due")) {
            *due = json!({"some": day});
        }
        if (3..5).contains(&i)
            && let Some(DynValue::Register(who)) =
                item.get_mut(&field_id(&meta, "Task", "assignee"))
        {
            *who = json!({"some": {"principal": "ada"}});
        }
        items.push(Id::from_raw(format!("task-{i:05}")), item);
    }
    let holding = meta.types.iter().find(|t| t.name == "Tasks").unwrap();
    let mut root_record =
        DynRecord::empty(tilolang_tilo::schema::lookup(&holding.id).unwrap()).unwrap();
    if let Some(DynValue::Collection(slot)) =
        root_record.get_mut(&field_id(&meta, "Tasks", "items"))
    {
        *slot = items;
    }
    let create = DynAction {
        app: APP.to_owned(),
        kind: "so.tilo.lang.tasks.seed".to_owned(),
        action_id: "$create".to_owned(),
        args: serde_json::Value::Null,
        authorship: tilolang_tilo::Authorship::default(),
        edits: vec![DynEdit::Create {
            id: root.id_string(),
            record: root_record.to_json(),
        }],
    };
    ada.apply(&create);
    ada.commit();
    ada
}

/// What a first render of the Tasks home reads of the index: the pages and directories, the
/// fields, and the items it evaluated, and the count the Today place shows.
fn rendered(replica: &Replica) -> ((u64, u64, u64, u64), i64) {
    let before = replica.host.store().read_stats();
    let view = replica.view(APP);
    let after = replica.host.store().read_stats();
    let today = count(&view, "Today").expect("the Today place shows how many it holds");
    (
        (
            after.index.pages.saturating_sub(before.index.pages),
            after.index.dirs.saturating_sub(before.index.dirs),
            after.index.fields.saturating_sub(before.index.fields),
            after.evaluated.saturating_sub(before.evaluated),
        ),
        today,
    )
}

#[test]
fn today_reads_the_same_at_any_size_and_each_person_their_own() {
    // Three tasks are due today whoever looks, and two more are Ada's whenever they are due.
    let mut measured = Vec::new();
    for n in [1_000usize, 10_000] {
        let ada = seeded(n);
        let (read, today) = rendered(&ada);
        assert_eq!(
            today, 5,
            "{n} tasks: the three due today and the two Ada holds"
        );
        measured.push(read);
    }
    assert_eq!(
        measured[0], measured[1],
        "the same today costs the same whether the workspace holds a thousand tasks or ten \
         thousand: the filter's run is read, not the tasks"
    );

    // Each person's Today is their own, read from the same column: the tasks due today are
    // everyone's, and the ones Ada holds are not Ben's.
    let mut ada = seeded(1_000);
    let (hers, today) = rendered(&ada);
    assert_eq!(today, 5);
    let ben = ada.fork("ben", PACKAGES);
    ben.host.sync_from(&ben.doc);
    ben.host.set_clock(|| "2026-10-29T10:00:00Z".to_owned());
    let (theirs, today) = rendered(&ben);
    assert_eq!(
        today, 3,
        "what Ada holds stands in her own Today: Ben's is the day's tasks alone"
    );
    assert_eq!(
        theirs.0, hers.0,
        "the assignee column is one column, read for the run of the person looking"
    );
}

#[test]
fn the_day_turns_at_the_persons_midnight_and_the_view_waits_for_it() {
    use std::cell::RefCell;
    use std::rc::Rc;

    // Ada's week in Tokyo: 19:00 on Thursday the 29th, so her day turns at 15:00 UTC.
    let clock = Rc::new(RefCell::new("2026-10-29T10:00:00Z".to_owned()));
    let read = Rc::clone(&clock);
    let (ada, _) = week();
    ada.host.set_clock(move || read.borrow().clone());
    ada.host.set_zone(|| 9 * 3600);
    let mut view = ada.view(APP);
    view.set_lazy_rows(false).unwrap();
    go_to(&mut view, "Today");
    assert_eq!(count(&view, "Today"), Some(2), "{:?}", places(&view));

    // Nothing else moves the view: it waits for her midnight, not for the next minute.
    let midnight = tilolang::calendar::instant("2026-10-29T15:00:00Z");
    assert_eq!(view.wakes_at(), midnight, "her day turns at her midnight");

    // A minute before it, nothing runs: the day it read has not turned, and the view is the
    // same one it drew.
    *clock.borrow_mut() = "2026-10-29T14:59:00Z".to_owned();
    assert!(
        !view.is_stale(),
        "the view woke before the person's day turned"
    );
    assert!(
        view.poll_patches().unwrap().is_empty(),
        "a frame ran a minute before the midnight it waits for"
    );
    assert_eq!(count(&view, "Today"), Some(2), "{:?}", places(&view));

    // Her midnight: the day turns, and what is due on it stands in Today, which is now the
    // 30th.
    *clock.borrow_mut() = "2026-10-29T15:00:00Z".to_owned();
    assert!(view.is_stale(), "her midnight opened the next day");
    view.poll_patches().unwrap();
    assert_eq!(
        count(&view, "Today"),
        Some(3),
        "the task due on the 30th stands in Today: {:?}",
        places(&view)
    );
    assert_eq!(
        view.wakes_at(),
        tilolang::calendar::instant("2026-10-30T15:00:00Z"),
        "it waits for the next midnight"
    );
}
