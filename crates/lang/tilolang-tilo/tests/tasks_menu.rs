//! A task's row when the window asks for its menu: the commands the app offers on the task, in
//! the app's order, and the one a person chooses running as the app publishes it. A row the window
//! never asks about decides none of its commands, so a list of thousands costs the one row opened.

use tilolang_runtime::{Actor, InvokeOutcome, UiNode};

use crate::tasks::{
    APP, call, create, done, flag, go_to, home_list_titles, home_row, labels, row_command_key,
    row_commands, text_of, workspace,
};

/// The commands a row node offers from its menus, as a person reads them.
fn offered_in(row: &UiNode) -> Vec<String> {
    row.of_kind("ui.MenuItem")
        .into_iter()
        .filter_map(|item| text_of(item, "label"))
        .collect()
}

#[test]
fn a_tasks_row_asked_for_its_menu_holds_the_tasks_commands_and_one_chosen_runs() {
    let (ada, root) = workspace();
    let task = call(&ada, "new_task", create(&root, "Book the ferry"));
    ada.commit();
    let mut view = ada.view(APP);
    go_to(&mut view, "Inbox");

    // Before the window asks, the row carries only the way to the task's page its view placed:
    // the commands the task itself offers wait for the ask, so a list of thousands decides none
    // of its rows' actions until a person opens one.
    let row = home_row(&mut view, &task, false);
    assert!(
        flag(row, "offers_menu"),
        "the row says its task offers a menu: {row:?}"
    );
    let before = offered_in(row);
    assert_eq!(
        before,
        ["Open as page"],
        "the row carries what its view placed and nothing of its task's own yet: {before:?}"
    );

    // What the window asks when the person opens the row's menu: the row again, with what its
    // task offers, in the order the app offers them.
    let commands = row_commands(&mut view, &task);
    let offered = labels(&commands);
    for wanted in ["Complete", "Pick date…", "Assign…", "Delete"] {
        assert!(
            offered.contains(&wanted),
            "the row's menu offers {wanted}: {offered:?}"
        );
    }
    let row = home_row(&mut view, &task, true);
    assert!(
        !flag(row, "offers_menu"),
        "the row carries what its task offers now: {row:?}"
    );

    // Choosing Complete runs the task's own action, as its declaration publishes it.
    let key = row_command_key(&mut view, &task, "Complete");
    let choose = view
        .event_action(&key, "choose")
        .expect("Complete runs where it is offered");
    let ran = ada
        .host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person(&ada.name)),
            (choose.action.as_str(), choose.args.clone()),
        )
        .expect("Complete ran");
    assert!(matches!(ran, InvokeOutcome::Done(_)), "{ran:?}");
    ada.commit();
    assert!(done(&ada, &task), "Complete marked the task done");

    // The list no longer shows it, and Done does: the command did what its row offered.
    go_to(&mut view, "Inbox");
    assert_eq!(home_list_titles(&ada, &mut view), Vec::<String>::new());
    go_to(&mut view, "Done");
    assert_eq!(home_list_titles(&ada, &mut view), ["Book the ferry"]);
}
