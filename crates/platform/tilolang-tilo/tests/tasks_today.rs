//! A pick for today is a value each person has their own of, on a task everyone shares: it
//! stands in their Today and nowhere else's, another person reads on the task's page that they
//! picked it, and letting it go takes it out of theirs alone.

use tilolang_runtime::{Principal, Value};

use crate::fixtures::Replica;
use crate::tasks::{
    APP, PACKAGES, call, count, go_to, home_list_titles, page_commands, places, task_page, texts,
    workspace,
};

/// Ada's week and Ben's copy of it: a bill due today nobody holds, a report due today that is
/// Ben's, and a call she picked for her Today, with no day of its own.
fn week() -> (Replica, Replica, Value) {
    let (mut ada, root) = workspace();
    let called = call(
        &ada,
        "new_task",
        vec![
            root.clone(),
            Value::none(),
            Value::none(),
            Value::none(),
            Value::none(),
            Value::none(),
            Value::text("Call the landlord"),
        ],
    );
    let bill = call(
        &ada,
        "new_task",
        vec![
            root.clone(),
            Value::none(),
            Value::none(),
            Value::none(),
            Value::none(),
            Value::none(),
            Value::text("Pay the electricity bill"),
        ],
    );
    let report = call(
        &ada,
        "new_task",
        vec![
            root,
            Value::none(),
            Value::none(),
            Value::none(),
            Value::none(),
            Value::none(),
            Value::text("Send the report"),
        ],
    );
    ada.commit();
    for task in [&bill, &report] {
        call(
            &ada,
            "schedule",
            vec![task.clone(), Value::some(Value::text("2026-10-29"))],
        );
    }
    call(
        &ada,
        "assign",
        vec![report, Value::some(Value::Principal(Principal::new("ben")))],
    );
    call(&ada, "pick", vec![called.clone(), Value::Bool(true)]);
    ada.commit();
    let ben = ada.fork("ben", PACKAGES);
    ben.host.sync_from(&ben.doc);
    (ada, ben, called)
}

#[test]
fn a_pick_stands_in_the_pickers_today_and_in_no_one_elses() {
    let (mut ada, mut ben, called) = week();
    let mut view = ada.view(APP);
    view.set_lazy_rows(false).unwrap();
    go_to(&mut view, "Today");
    assert_eq!(
        home_list_titles(&ada, &mut view),
        ["Pay the electricity bill", "Call the landlord"],
        "what is due stands under its day; her pick with no day after it, and the report is Ben's"
    );
    assert_eq!(count(&view, "Today"), Some(2), "{:?}", places(&view));
    let mut theirs = ben.view(APP);
    theirs.set_lazy_rows(false).unwrap();
    go_to(&mut theirs, "Today");
    assert_eq!(
        home_list_titles(&ben, &mut theirs),
        ["Pay the electricity bill", "Send the report"],
        "Ben's Today holds what is due and his; Ada's pick is not in it"
    );
    assert_eq!(count(&theirs, "Today"), Some(2), "{:?}", places(&theirs));

    // Letting it go takes it out of hers, and leaves his Today as it was.
    call(&ada, "pick", vec![called, Value::Bool(false)]);
    ada.commit();
    ada.sync_with(&mut ben);
    go_to(&mut view, "Inbox");
    go_to(&mut view, "Today");
    assert_eq!(
        home_list_titles(&ada, &mut view),
        ["Pay the electricity bill"],
        "the pick is gone from her Today"
    );
    assert_eq!(count(&view, "Today"), Some(1), "{:?}", places(&view));
    go_to(&mut theirs, "Inbox");
    go_to(&mut theirs, "Today");
    assert_eq!(
        home_list_titles(&ben, &mut theirs),
        ["Pay the electricity bill", "Send the report"],
        "his Today never held it"
    );
}

#[test]
fn everyone_reads_who_picked_a_shared_task() {
    let (ada, ben, called) = week();
    // The command follows the person looking's value: hers offers to let it go, his to pick it.
    let page = task_page(&ada, &called);
    assert!(
        page_commands(&page)
            .iter()
            .any(|command| command.label == "Let go"),
        "Ada picked it, and the page offers to let it go: {:?}",
        page_commands(&page)
    );
    assert!(
        !texts(page.tree())
            .iter()
            .any(|text| text.ends_with("'s today")),
        "Ada reads no one else's pick on her own: {:?}",
        texts(page.tree())
    );
    let page = task_page(&ben, &called);
    assert!(
        page_commands(&page)
            .iter()
            .any(|command| command.label == "Pick for today"),
        "Ben did not pick it, and his page offers to pick it: {:?}",
        page_commands(&page)
    );
    assert!(
        texts(page.tree()).iter().any(|text| text == "ada's today"),
        "Ben reads that Ada picked it: {:?}",
        texts(page.tree())
    );
}
