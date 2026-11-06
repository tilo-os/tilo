//! The Today place is one filter of the app's own: what is due today or late, and what the
//! person looking holds, whenever it is due.

use tilolang_runtime::{Principal, Value};

use crate::fixtures::Replica;
use crate::tasks::{
    APP, PACKAGES, call, count, create, go_to, home_list_titles, places, workspace,
};

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
    ben.host
        .runtime()
        .set_clock(|| "2026-10-29T10:00:00Z".to_owned());
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
