//! What a window's catalogue offers of an app's actions: what it is called there and whether it
//! can run, as the record the app publishes now says it.

use super::*;

/// The address the window's catalogue publishes the action labelled `label` at, of a resource
/// `pick` takes: what the app offers on that resource now.
fn offered_at(
    session: &mut Session,
    label: &str,
    pick: &dyn Fn(&str) -> bool,
) -> ui::tilo_actions::ActionAddress {
    session
        .hosted()
        .view()
        .expect("the window is connected")
        .actions()
        .iter()
        .find(|action| action.action_label == label && pick(action.address.resource.as_str()))
        .unwrap_or_else(|| panic!("the app offers {label}"))
        .address
        .clone()
}

/// Deliver an invocation of `address` on the window's app, as another app's call or an agent's
/// reaches it: the app runs it as that caller's step and publishes what the record is now.
fn call(
    session: &mut Session,
    address: &ui::tilo_actions::ActionAddress,
    argument: Option<String>,
) {
    session
        .inbox()
        .borrow_mut()
        .push_back(CollabEvent::Invocation {
            id: 1,
            invocation: ui::tilo_actions::InvocationId::new(),
            resource: address.resource.clone(),
            action: address.action.clone(),
            argument,
            actor: "ada".to_owned(),
            context: None,
        });
    session.settle();
}

#[test]
fn a_task_checked_off_offers_reopen_and_the_finished_ones_can_be_cleared() {
    use ui::tilo_actions::Availability;
    use ui::winit::keyboard::NamedKey;
    let _settled = ui::motion::testing::settled();
    let mut session = Session::live_tasks();
    // A project's list folds the finished work under its foot: the task stays where the person
    // reads it once it is checked off, so what the window shows is unchanged and only the
    // record moved.
    choose(&mut session, "New project");
    session.settle();
    let (px, py) = shown_at(&mut session, "New project");
    session.window.click_at(px, py);
    session.settle();
    choose(&mut session, "New task");
    session.window.type_text("Water the plants");
    session.window.press_key(NamedKey::Enter);
    session.settle();
    // What the window's catalogue offers of the task's own action, and of the finished ones.
    let labels = |session: &mut Session| -> Vec<String> {
        session
            .hosted()
            .view()
            .expect("the window is connected")
            .actions()
            .iter()
            .map(|action| action.action_label.clone())
            .collect()
    };
    let clearing = |session: &mut Session| -> Availability {
        session
            .hosted()
            .view()
            .expect("the window is connected")
            .actions()
            .iter()
            .find(|action| action.action_label == "Delete finished tasks")
            .expect("the window offers to clear what is finished")
            .contract
            .availability
            .clone()
    };
    assert!(
        labels(&mut session).iter().any(|label| label == "Complete"),
        "the task is not finished yet"
    );
    assert!(
        matches!(clearing(&mut session), Availability::Unavailable { .. }),
        "nothing is finished yet"
    );

    // Checking it off at the app moves what the window offers of it, and whether the finished
    // ones can be cleared: both are the app's answer for the record as it is now.
    let complete = offered_at(&mut session, "Complete", &|resource| resource.contains('/'));
    call(&mut session, &complete, None);
    let now = labels(&mut session);
    assert!(
        now.iter().any(|label| label == "Reopen"),
        "the finished task is offered opened again: {now:?}"
    );
    assert!(
        !now.iter().any(|label| label == "Complete"),
        "the finished task is no longer offered finished: {now:?}"
    );
    assert_eq!(clearing(&mut session), Availability::Available);
    // And what was offered runs: choosing it clears the task the app says is finished.
    choose(&mut session, "Delete finished tasks");
    session.settle();
    assert!(session.titles().is_empty(), "{:?}", session.titles());
}
