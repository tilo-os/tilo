//! A call a workspace's broker delivers to a hosted window: the app runs it, and the answer its
//! caller reads carries what the action made — the record as data, with its id, and the address
//! the app publishes it under.

use super::packages::{package, package_of};
use tilo_actions::InvocationUpdate;
use ui::headless::Driver;

const CALENDAR: &str = "so.tilo.calendar";

// USER REQUIREMENT: an action that returns a record answers its caller with that record, so a
// caller reaches what it made — as the shots' plans seed a task and then check it off.
#[test]
fn a_delivered_call_answers_the_record_the_app_made() {
    let route = collaboration::RouteSpec::app(CALENDAR, uuid::Uuid::new_v4());
    let spec =
        tilolang_process::hosted::Spec::new(CALENDAR, route.cluster_id.to_string(), 1, "ada")
            .in_language("en-US");
    let window = tilolang_process::window::hosted::HostedWindow::in_process(
        spec,
        route,
        package(package_of(CALENDAR)),
        None,
    )
    .expect("the app's instance starts");
    let mut driver = Driver::new(ui::State::new(), Box::new(window), (1280, 800));
    driver.connect_peer();
    let resource = format!("tl:{CALENDAR}:root-53uahg6d");
    let caller = tilo_actions::Actor::Member(tilo_actions::MemberId::person("ada"));
    let invocation = driver.deliver_call(
        &resource,
        "new_task",
        &serde_json::json!({ "title": "Collect feedback" }),
        caller,
        None,
    );
    assert!(
        driver.wait_until(std::time::Duration::from_secs(10), |driver| {
            driver
                .call_updates(invocation)
                .iter()
                .any(InvocationUpdate::is_terminal)
        }),
        "the app answers: {:?}",
        driver.call_updates(invocation)
    );
    let updates = driver.call_updates(invocation);
    let [InvocationUpdate::Succeeded { output, .. }] = updates.as_slice() else {
        panic!("the app succeeded: {updates:?}");
    };
    let value = output.value.clone().expect("the task as data");
    let id = value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("the task's id: {value}"));
    assert_eq!(
        output.resources,
        vec![format!("tl:{CALENDAR}:{id}")],
        "the record it made is named by the address the app publishes it under"
    );
}
