//! What a window's catalogue offers of an app's actions: what it is called there and whether it
//! can run, as the document the app publishes now says it.

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
/// reaches it: the app runs it as that caller's step and publishes what the document is now.
fn call(
    session: &mut Session,
    address: &ui::tilo_actions::ActionAddress,
    argument: Option<String>,
) {
    session.deliver(
        &ui::tilo_actions::MemberId::person("ada").into(),
        address.resource.clone(),
        address.action.clone(),
        argument,
        None,
    );
}

#[test]
fn a_task_checked_off_offers_reopen_and_the_finished_ones_can_be_cleared() {
    use ui::tilo_actions::Availability;
    use ui::winit::keyboard::NamedKey;
    let _settled = ui::motion::testing::settled();
    let mut session = Session::live_tasks();
    // A project's list folds the finished work under its foot: the task stays where the person
    // reads it once it is checked off, so what the window shows is unchanged and only the
    // document moved.
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
    // ones can be cleared: both are the app's answer for the document as it is now.
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
    // And what was offered runs: choosing it clears the task the app says is finished. A
    // destructive offer asks the person first, as every destructive press does, and their no
    // runs nothing of it.
    choose(&mut session, "Delete finished tasks");
    session.settle();
    assert_eq!(
        session.titles(),
        vec!["Water the plants".to_owned()],
        "nothing runs before the person answers"
    );
    assert_eq!(
        session.decline_asked(),
        1,
        "clearing the finished tasks asks the person first"
    );
    session.settle();
    assert_eq!(
        session.titles(),
        vec!["Water the plants".to_owned()],
        "the person said no: the task stays"
    );
    // Their yes runs the same choice.
    choose(&mut session, "Delete finished tasks");
    session.settle();
    assert_eq!(
        session.allow_asked(),
        1,
        "clearing the finished tasks asks the person first"
    );
    assert!(session.titles().is_empty(), "{:?}", session.titles());
}

/// A person seated at the Poker table from their window is offered the way up, by the window's
/// control and by the catalogue an agent and the palette read: both are decided for the person
/// at the window, whose id is their key.
#[test]
fn a_seat_taken_at_the_window_offers_leaving_the_table() {
    use ui::tilo_actions::Availability;
    let _settled = ui::motion::testing::settled();
    let person = "c9f492a76c47c7cbe0281d39135c53f68826a275dd763343da2baff59926bb1a";
    let mut session = Session::open("so.tilo.poker", person, Window::live);
    let sit = session.identity_named("Take a seat");
    assert!(
        session
            .window
            .app()
            .press_window_control(&sit, ui::window_targets::TargetAction::Activate),
        "the window takes the press"
    );
    session.settle();
    let leaving = session
        .hosted()
        .view()
        .expect("the window is connected")
        .actions()
        .iter()
        .find(|action| action.action_label == "Leave the table")
        .expect("the window offers leaving the table")
        .contract
        .availability
        .clone();
    assert_eq!(leaving, Availability::Available);
    let leave = session
        .window
        .node_named("Leave the table")
        .expect("seated, the window offers the way up")
        .1;
    assert!(
        leave.supports_action(ui::accesskit::Action::Click),
        "leaving is open to the person seated: {:?}",
        leave.description()
    );
}
