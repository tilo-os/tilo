//! The screen as a person meets it: where things sit at each window size.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use speed_test::SpeedTestShell;
use ui::framework::testing::HeadlessApp;
use ui::vello::kurbo::Rect;

fn staged(scenario: &str, viewport: (u32, u32)) -> HeadlessApp<SpeedTestShell> {
    let mut app = HeadlessApp::<SpeedTestShell>::new().unwrap();
    app.harness().resize(viewport).unwrap();
    app.stage(Some(scenario)).unwrap();
    app
}

fn target(app: &mut HeadlessApp<SpeedTestShell>, name: &str) -> Rect {
    *app.harness()
        .targets()
        .get(name)
        .unwrap_or_else(|| panic!("no target named {name}"))
}

#[test]
fn a_wide_window_puts_the_results_beside_the_test() {
    let mut app = staged("complete", (1280, 800));
    let run = target(&mut app, "run");
    let latest = target(&mut app, "history-0");
    assert!(
        latest.x0 >= run.x1,
        "results start right of the action: {latest:?} vs {run:?}"
    );
    assert!(latest.y0 < run.y1 + 200.0, "and share its top part");
}

#[test]
fn a_narrow_window_stacks_the_results_below_the_test() {
    let mut app = staged("complete", (800, 600));
    let run = target(&mut app, "run");
    let latest = target(&mut app, "history-0");
    assert!(
        latest.y0 >= run.y1,
        "results sit below the action: {latest:?} vs {run:?}"
    );
    assert!(
        latest.x0 < run.x1,
        "and the page is one column: {latest:?} vs {run:?}"
    );
}

#[test]
fn the_action_stays_centred_under_the_dial_axis() {
    let mut app = staged("complete", (1280, 800));
    let run = target(&mut app, "run");
    let stages = app
        .harness()
        .targets()
        .into_iter()
        .find(|(name, _)| name.ends_with("/Stepper"))
        .map(|(_, rect)| rect)
        .unwrap();
    let axis = |rect: Rect| f64::midpoint(rect.x0, rect.x1);
    assert!(
        (axis(run) - axis(stages)).abs() <= 1.0,
        "{run:?} {stages:?}"
    );
}

#[test]
fn nothing_has_been_measured_before_a_test_runs() {
    let mut app = staged("idle", (1280, 800));
    let targets = app.harness().targets();
    assert!(targets.contains_key("run"));
    assert!(
        !targets.contains_key("history-0"),
        "no recent tests are invented"
    );
    assert!(
        !targets.keys().any(|name| name.ends_with("/Stepper")),
        "no stage is reached at idle"
    );
}
