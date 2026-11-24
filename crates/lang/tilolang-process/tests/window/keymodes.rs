//! A view's own state answers a key: a control's `key:` fires its press while the window has the
//! keyboard, exactly as a click does, so a mode switch is as reachable as pressing its button.
//! Only the controls the view draws now answer.

use tilo_app_test_host::TestApp;
use tilolang_process::window::app::TlApp;

const DESKTOP: (u32, u32) = (1280, 800);

fn staged() -> TestApp<TlApp> {
    crate::support::installed("keymodes");
    let mut app =
        TestApp::<TlApp>::capture_at(Some("two modes"), DESKTOP).expect("the scenario stages");
    settle(&mut app);
    app
}

fn settle(app: &mut TestApp<TlApp>) {
    crate::change_marks::captured(app, 8);
}

fn said(app: &mut TestApp<TlApp>) -> Vec<String> {
    crate::change_marks::said(app)
}

#[test]
fn a_controls_key_sets_the_views_own_state_as_its_press_does() {
    let mut app = staged();
    let start = said(&mut app);
    assert!(
        start.iter().any(|text| text == "Basic mode"),
        "the view starts in its basic mode: {start:?}"
    );
    app.harness().key("Ctrl+2");
    settle(&mut app);
    let scientific = said(&mut app);
    assert!(
        scientific.iter().any(|text| text == "Scientific mode"),
        "Ctrl+2 presses the scientific button: {scientific:?}"
    );
    app.harness().key("Ctrl+1");
    settle(&mut app);
    let basic = said(&mut app);
    assert!(
        basic.iter().any(|text| text == "Basic mode"),
        "Ctrl+1 presses the basic button: {basic:?}"
    );
}

#[test]
fn a_control_that_is_not_drawn_takes_no_key() {
    let mut app = staged();
    app.harness().key("Ctrl+3");
    settle(&mut app);
    let after = said(&mut app);
    assert!(
        !after.iter().any(|text| text == "Graph mode"),
        "the graph button stands behind a false `if`, so its key answers nothing: {after:?}"
    );
    assert!(
        after.iter().any(|text| text == "Basic mode"),
        "the view stays in its basic mode: {after:?}"
    );
}
