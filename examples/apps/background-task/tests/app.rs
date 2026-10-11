use std::time::Duration;

use example_background_task::{App, MAX_LIMIT, Search, Task, count_primes, is_prime};
use ui::a11y::Action;
use ui::headless::SemanticExpectation;
use ui::prelude::{Invoke, Replica, TestApp};

/// The limit, edited at its path as the number field edits it.
#[expect(clippy::expect_used, reason = "a test helper fails the test loudly")]
fn limit(limit: u64) -> Invoke<Search> {
    Invoke::edit(&Search::paths().limit.set(limit)).expect("an edit")
}

#[test]
fn the_work_is_plain_testable_code() {
    let primes = (0..30).filter(|n| is_prime(*n)).collect::<Vec<_>>();
    assert_eq!(primes, [2, 3, 5, 7, 11, 13, 17, 19, 23, 29]);
    assert_eq!(count_primes(100, || true), Some(25));
    assert_eq!(
        count_primes(100_000, || false),
        None,
        "the worker stops when told"
    );
}

#[test]
fn the_limit_is_validated_and_durable() {
    let mut search = Replica::<Search>::new();
    assert!(search.apply(limit(1)).is_err());
    assert!(search.apply(limit(MAX_LIMIT + 1)).is_err());

    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(limit(100)).expect("set the limit");
    let app = app.reopen().expect("the app reopens");
    assert_eq!(app.state().limit, 100);
}

/// Run is an action anyone can start; its result replaces the spinner.
#[test]
fn counting_runs_in_the_background_on_the_resource_input() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    assert!(
        app.harness().resources().iter().any(|resource| {
            resource.kind == "example.prime-search"
                && ["run", "cancel"]
                    .iter()
                    .all(|id| resource.actions.iter().any(|action| action.id == *id))
        }),
        "the palette, agents and scripts can start and cancel the task"
    );
    app.invoke(limit(100)).expect("set the limit");
    app.harness()
        .perform_named("run-task", &Action::Activate)
        .expect("the run button is addressable");
    app.harness()
        .wait_for_named("task-result", Duration::from_secs(5))
        .expect("the count completes");
    app.harness()
        .expect(&SemanticExpectation::named("task-result").name("Found 25 primes below 100."))
        .expect("the result replaces the spinner");
}

/// Cancelling discards the running count and returns to idle.
#[test]
fn a_running_count_can_be_cancelled() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(limit(MAX_LIMIT)).expect("set the limit");
    app.harness()
        .perform_named("run-task", &Action::Activate)
        .expect("start");
    app.harness()
        .wait_for_named("cancel-task", Duration::from_secs(1))
        .expect("the running state offers Cancel");
    app.harness()
        .perform_named("cancel-task", &Action::Activate)
        .expect("cancel");
    app.harness()
        .expect(&SemanticExpectation::named("task-status").name("Press Count to start."))
        .expect("the task is idle again");
}

/// An agent's Run answers with the count once the worker finishes.
#[test]
fn an_agent_runs_the_count_as_a_job() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(limit(100)).expect("set the limit");
    let output = app.effect(&Task::Run).expect("the count finishes");
    assert_eq!(output.value, Some(ui::serde_json::json!(25)));
    app.harness()
        .expect(&SemanticExpectation::named("task-result").name("Found 25 primes below 100."))
        .expect("the view shows the agent's run too");
}

#[test]
fn initial_native_app_contract_is_certified() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}
