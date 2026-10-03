use example_counter::{App, Counter, LIMIT};
use ui::headless::SemanticExpectation;
use ui::prelude::{Replica, ResourceApp as _, TestApp};

/// A `Replica` applies actions to the resource with no window: the fastest
/// way to test what an action allows and refuses.
#[test]
fn actions_validate_and_a_refusal_changes_nothing() {
    let mut counter = Replica::<Counter>::new();
    counter.apply(Counter::ADD.with(10)).expect("add ten");
    counter
        .apply(Counter::ADD.with(-3))
        .expect("subtract three");
    assert_eq!(counter.state().expect("counter").count, 7);

    assert!(counter.apply(Counter::SET.with(LIMIT + 1)).is_err());
    counter
        .apply(Counter::SET.with(LIMIT))
        .expect("set to the limit");
    assert!(counter.apply(Counter::ADD.with(1)).is_err());
    assert!(counter.apply(Counter::ADD.with(i64::MIN)).is_err());
    assert_eq!(counter.state().expect("counter").count, LIMIT);
}

/// The published surface is what the palette, agents and scripts see: each
/// action with its label and typed parameters.
#[test]
fn every_action_is_published_with_its_parameters() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let resources = app.harness().resources();
    let counter = resources
        .iter()
        .find(|resource| resource.kind == "example.counter")
        .expect("the counter is published");
    let actions: Vec<_> = counter
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    // The framework adds its own generic actions after the app's.
    assert_eq!(actions.get(..3), Some(["add", "set", "reset"].as_slice()));
    let add = counter.actions.first().expect("add");
    let params: Vec<_> = add
        .params()
        .iter()
        .map(|param| param.name.as_ref())
        .collect();
    assert_eq!(params, ["amount"]);
}

/// Buttons invoke the action with their own arguments; the count follows;
/// each press is one undo step.
#[test]
fn buttons_invoke_the_action_and_undo_reverts_each_press() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.harness().click_named("add-ten").expect("+10");
    app.harness().click_named("subtract-one").expect("-1");
    app.harness()
        .expect(&SemanticExpectation::named("count").name("9"))
        .expect("the count follows the resource");
    assert_eq!(App::label(&app.state()), "Counter at 9");
    assert!(app.undo());
    assert_eq!(app.state().count, 10);
    assert!(app.redo());
    assert_eq!(app.state().count, 9);
}

/// A destructive action is refused until it is confirmed, from any surface.
#[test]
fn reset_needs_confirmation() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(Counter::SET.with(5)).expect("set");
    assert!(app.invoke(Counter::RESET.invoke()).is_err());
    assert_eq!(app.state().count, 5);
    app.invoke_confirmed(Counter::RESET.invoke())
        .expect("a confirmed reset");
    assert_eq!(app.state().count, 0);
}

/// Permissions are enforced on every action: a viewer can read but not edit.
#[test]
fn a_viewer_cannot_count() {
    let mut app = TestApp::<App>::with_role(ui::Role::Viewer).expect("the app opens");
    assert!(app.invoke(Counter::ADD.with(1)).is_err());
    assert_eq!(app.state().count, 0);
}

/// The screenshot scenario is staged through the same actions.
#[test]
fn the_counted_scenario_stages_through_actions() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.stage(Some("counted")).expect("stage");
    assert_eq!(app.state().count, 42);
}

/// Every control that causes an effect is bound to a published action.
#[test]
fn initial_native_app_contract_is_certified() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}
