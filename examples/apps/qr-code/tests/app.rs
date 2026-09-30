use example_qr_code::{App, Code};
use ui::a11y::{Action, Role};
use ui::headless::SemanticExpectation;
use ui::prelude::TestApp;

/// The app opens empty, explaining what to do instead of encoding a sample.
#[test]
fn the_app_starts_empty() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    assert_eq!(app.state(), Code::default());
    assert!(
        !app.harness().targets().contains_key("qr-output"),
        "no code is drawn"
    );
    let snapshot = app.harness().inspection_json().expect("snapshot");
    assert!(
        snapshot.contains("Nothing to encode yet"),
        "the empty state explains"
    );
}

/// Typed text becomes a code whose alternative text names the payload.
#[test]
fn the_code_follows_the_payload_and_is_described() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.harness()
        .perform_named("qr-input", &Action::SetValue("你好 Rust GUI".into()))
        .expect("the input is addressable");
    app.harness()
        .expect(
            &SemanticExpectation::named("qr-output")
                .role(Role::Image)
                .name_contains("你好 Rust GUI"),
        )
        .expect("the image has payload-specific alternative text");

    app.invoke(Code::CLEAR.invoke()).expect("clear");
    assert!(!app.harness().targets().contains_key("qr-output"));
    assert!(app.undo(), "clearing is undoable");
    assert_eq!(app.state().payload, "你好 Rust GUI");
}

#[test]
fn the_staged_scenario_shows_a_code() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.stage(Some("encoded")).expect("stage the scenario");
    app.harness()
        .expect(&SemanticExpectation::named("qr-output").name_contains("tilo.dev"))
        .expect("the scenario encodes a link");
}

#[test]
fn initial_native_app_contract_is_certified() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}
