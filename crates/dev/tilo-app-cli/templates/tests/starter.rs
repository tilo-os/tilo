use generated_app::{App, Profile};
use ui::a11y::{Action, Role};
use ui::headless::SemanticExpectation;
use ui::prelude::{Replica, TestApp};

/// The resource on its own: actions applied the way every surface applies
/// them, no window needed.
#[test]
fn profile_actions_validate_their_values() {
    let mut profile = Replica::<Profile>::new();
    profile
        .apply(Profile::SET_NAME.with("Ada".into()))
        .expect("rename");
    assert_eq!(profile.state().expect("profile").name, "Ada");
    assert!(profile.apply(Profile::SET_VOLUME.with(2.0)).is_err());
    assert!(
        profile
            .apply(Profile::SET_REGION.with("Mars".into()))
            .is_err()
    );
}

/// The app as it runs: controls invoke the actions, the view follows the
/// resource, and every edit is one undo step.
#[test]
fn app_controls_edit_the_durable_profile() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let harness = app.harness();
    harness
        .perform_named("name-input", &Action::SetValue("Ada".into()))
        .expect("the name input remains addressable");
    harness
        .expect(
            &SemanticExpectation::named("greeting")
                .role(Role::StaticText)
                .name_contains("Ada"),
        )
        .expect("the greeting follows the profile");
    harness
        .perform_named("updates-checkbox", &Action::ToggleCheck)
        .expect("the checkbox remains addressable");
    harness
        .expect(
            &SemanticExpectation::named("updates-checkbox")
                .role(Role::Checkbox)
                .checked(false),
        )
        .expect("checkbox semantics follow the profile");
    harness
        .perform_named("refresh-input", &Action::SetValue("30".into()))
        .expect("the number input remains addressable");
    harness
        .expect(
            &SemanticExpectation::named("refresh-input")
                .role(Role::SpinButton)
                .value("30 min"),
        )
        .expect("spinbutton semantics follow the profile");
    harness
        .perform_named("volume-slider", &Action::SetValue("75%".into()))
        .expect("the slider remains addressable");
    harness
        .perform_named("region-select", &Action::SetValue("Asia/Tokyo".into()))
        .expect("the dropdown remains addressable");

    let profile = app.state();
    assert_eq!(profile.name, "Ada");
    assert!(!profile.updates);
    assert!((profile.refresh_minutes - 30.0).abs() < f64::EPSILON);
    assert!((profile.volume - 0.75).abs() < 1e-9);
    assert_eq!(profile.region.0, "Asia/Tokyo");
    assert!(app.undo(), "every edit is one undo step");
    assert_eq!(app.state().region.0, "Europe/Berlin");
}

/// Permissions are enforced on every action: a viewer's edit is refused.
#[test]
fn a_viewer_cannot_edit_the_profile() {
    let mut app = TestApp::<App>::with_role(ui::Role::Viewer).expect("the app opens");
    assert!(
        app.invoke(Profile::SET_NAME.with("not mine".into()))
            .is_err()
    );
    assert_eq!(app.state().name, "world");
}

/// The profile is stored: it survives a relaunch with no save step.
#[test]
fn the_profile_survives_a_relaunch() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(Profile::SET_NAME.with("Grace".into()))
        .expect("rename");
    let app = app.reopen().expect("the app reopens");
    assert_eq!(app.state().name, "Grace");
}
