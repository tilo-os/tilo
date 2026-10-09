use example_bound_controls::{App, Preferences};
use ui::a11y::{Action, Role};
use ui::headless::SemanticExpectation;
use ui::prelude::{Replica, TestApp};

/// Validation is in the actions, so it holds for callers with no control.
#[test]
fn actions_refuse_out_of_range_values() {
    let mut prefs = Replica::<Preferences>::new();
    assert!(prefs.apply(Preferences::SET_VOLUME.with(2.0)).is_err());
    assert!(prefs.apply(Preferences::SET_REFRESH.with(0.0)).is_err());
    assert!(prefs.apply(Preferences::SET_REGION.with("Mars")).is_err());
    assert_eq!(prefs.state().expect("preferences"), Preferences::default());
}

/// Each control invokes its action, and the resource holds what was chosen.
#[test]
fn every_control_edits_its_field() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let harness = app.harness();
    harness
        .perform_named("name-input", &Action::SetValue("Ada".into()))
        .expect("the name input is addressable");
    harness
        .perform_named("updates-checkbox", &Action::ToggleCheck)
        .expect("the checkbox is addressable");
    harness
        .perform_named("refresh-input", &Action::SetValue("30".into()))
        .expect("the number input is addressable");
    harness
        .perform_named("volume-slider", &Action::SetValue("75%".into()))
        .expect("the slider is addressable");
    harness
        .perform_named("region-select", &Action::SetValue("Asia/Tokyo".into()))
        .expect("the dropdown is addressable");

    let prefs = app.state();
    assert_eq!(prefs.name, "Ada");
    assert!(!prefs.updates);
    assert!((prefs.refresh_minutes - 30.0).abs() < f64::EPSILON);
    assert!((prefs.volume - 0.75).abs() < 1e-9);
    assert_eq!(prefs.region.0, "Asia/Tokyo");
    assert!(app.undo(), "every edit is one undo step");
    assert_eq!(app.state().region.0, "Europe/Berlin");
}

/// Controls follow the resource: an edit through the action door, as an
/// agent makes it, shows up in the control.
#[test]
fn controls_follow_edits_from_elsewhere() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(Preferences::SET_REFRESH.with(45.0))
        .expect("set refresh");
    app.invoke(Preferences::SET_UPDATES.with(false))
        .expect("set updates");
    app.harness()
        .expect(
            &SemanticExpectation::named("refresh-input")
                .role(Role::SpinButton)
                .value("45 min"),
        )
        .expect("the number input follows");
    app.harness()
        .expect(
            &SemanticExpectation::named("updates-checkbox")
                .role(Role::Checkbox)
                .checked(false),
        )
        .expect("the checkbox follows");
}

/// A viewer's controls are disabled and its edits are refused.
#[test]
fn a_viewer_cannot_change_preferences() {
    let mut app = TestApp::<App>::with_role(ui::Role::Viewer).expect("the app opens");
    app.harness()
        .expect(&SemanticExpectation::named("name-input").enabled(false))
        .expect("the input disables itself");
    assert!(app.invoke(Preferences::SET_NAME.with("Eve")).is_err());
}

/// Every control that causes an effect is bound to a published action.
#[test]
fn initial_native_app_contract_is_certified() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}
