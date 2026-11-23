//! Tasks on a person's devices: a task an agent's step makes with no Tasks window open reaches
//! the person's devices and stays reachable once it is closed; a second device reads and edits the
//! same tasks; and a workspace founded again keeps the tasks the hosted one holds.
//!
//! Nothing here opens a window to make the tasks: each step runs the action the app publishes, so
//! what is read back is the workspace's own record, on each device's copy of it.

use tilolang_runtime::Value;

use crate::tasks::{
    APP, PACKAGES, call, create, done, go_to, home_list_titles, items, read, target, workspace,
};

/// `task` is in `r`'s copy of the workspace, titled `title`.
fn holds(r: &crate::fixtures::Replica, task: &Value, title: &str) {
    assert!(
        items(r).iter().any(|held| target(held) == target(task)),
        "{} holds the task made with no window open: {:?}",
        r.name,
        items(r)
    );
    assert_eq!(
        read(r, task, "title"),
        Value::text(title),
        "{} reads the task's title",
        r.name
    );
}

#[test]
fn an_agents_step_makes_a_task_with_tasks_closed_and_the_persons_devices_read_it() {
    let (ada, root) = workspace();
    let phone = ada.fork("ada", PACKAGES);
    // The step is the task list's own published action: no Tasks window is open anywhere.
    let task = call(&ada, "new_task", create(&root, "Book the ferry"));
    ada.commit();
    holds(&ada, &task, "Book the ferry");

    // Another device of the person reads the same task.
    ada.sync_with(&phone);
    holds(&phone, &task, "Book the ferry");

    // A person who opens Tasks afterwards sees it listed, on the device the step ran on.
    let mut view = ada.view(APP);
    go_to(&mut view, "Inbox");
    assert_eq!(home_list_titles(&ada, &mut view), ["Book the ferry"]);

    // Closing the task with no window open keeps it reachable: Done holds it, Inbox does not.
    call(&phone, "check", vec![task.clone(), Value::Bool(true)]);
    phone.commit();
    assert!(done(&phone, &task), "the phone closed the task");
    ada.sync_with(&phone);
    assert!(done(&ada, &task), "the device that made it reads it closed");
    let mut view = ada.view(APP);
    go_to(&mut view, "Inbox");
    assert_eq!(home_list_titles(&ada, &mut view), Vec::<String>::new());
    go_to(&mut view, "Done");
    assert_eq!(home_list_titles(&ada, &mut view), ["Book the ferry"]);
}

#[test]
fn a_second_device_sees_and_edits_the_same_tasks() {
    let (ada, root) = workspace();
    let task = call(&ada, "new_task", create(&root, "Book the ferry"));
    ada.commit();
    let phone = ada.fork("ada", PACKAGES);
    ada.sync_with(&phone);
    holds(&phone, &task, "Book the ferry");

    // The phone edits the task the laptop made: its own change reaches both.
    call(&phone, "check", vec![task.clone(), Value::Bool(true)]);
    let made = call(&phone, "new_task", create(&root, "Pack the tent"));
    phone.commit();
    ada.sync_with(&phone);
    for r in [&ada, &phone] {
        assert!(done(r, &task), "{} reads the task closed", r.name);
        assert_eq!(items(r).len(), 2, "both tasks are on {}", r.name);
    }
    for r in [&ada, &phone] {
        let mut view = r.view(APP);
        go_to(&mut view, "Inbox");
        assert_eq!(
            home_list_titles(r, &mut view),
            ["Pack the tent"],
            "{} lists the task the phone made, and not the one closed",
            r.name
        );
        go_to(&mut view, "Done");
        assert_eq!(
            home_list_titles(r, &mut view),
            ["Book the ferry"],
            "{} lists the closed one under Done",
            r.name
        );
    }
    assert_eq!(read(&ada, &made, "title"), Value::text("Pack the tent"));
}

#[test]
fn a_workspace_founded_again_keeps_the_tasks_the_hosted_one_holds() {
    let (site, root) = workspace();
    let ferry = call(&site, "new_task", create(&root, "Book the ferry"));
    let tent = call(&site, "new_task", create(&root, "Pack the tent"));
    site.commit();
    let before = items(&site);

    // A computer restored from the person's words holds none of the workspace: it makes one of
    // its own under the same app, and asks for the hosted one by its id.
    let (restored, _own) = workspace();
    assert!(
        items(&restored).is_empty(),
        "the restored computer holds nothing of the workspace"
    );
    restored.sync_with(&site);

    // What the host held is what the restored computer comes to read, and the host keeps every
    // task it had.
    assert_eq!(
        items(&restored).len(),
        2,
        "the restored computer reads the hosted tasks"
    );
    assert_eq!(read(&restored, &ferry, "title"), Value::text("Book the ferry"));
    for task in &before {
        assert!(
            items(&site).iter().any(|held| target(held) == target(task)),
            "the host still holds {task:?}"
        );
    }

    // The host carries on: what its people write next lands beside every task before it.
    let tickets = call(&site, "new_task", create(&root, "Buy the tickets"));
    site.commit();
    assert_eq!(items(&site).len(), 3, "the host made the task beside the others");
    assert_eq!(read(&site, &tickets, "title"), Value::text("Buy the tickets"));
    let mut view = site.view(APP);
    go_to(&mut view, "Inbox");
    assert_eq!(
        home_list_titles(&site, &mut view),
        ["Book the ferry", "Pack the tent", "Buy the tickets"]
    );
}
