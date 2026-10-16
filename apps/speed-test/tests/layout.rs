//! The screen as a person meets it: where things sit at each window size.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use speed_test::SpeedTestShell;
use ui::framework::testing::HeadlessApp;
use ui::vello::kurbo::Rect;

fn staged(scenario: &str, viewport: (u32, u32)) -> HeadlessApp<SpeedTestShell> {
    HeadlessApp::<SpeedTestShell>::capture_at(Some(scenario), viewport).unwrap()
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
    assert!(latest.y0 < run.y1 + 300.0, "and share its top part");
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

#[test]
fn the_run_button_stops_a_running_test_through_its_published_action() {
    use ui::framework::{ResourceApp, ResourceSurface};
    let mut app = HeadlessApp::<SpeedTestShell>::new().unwrap();
    for invoke in SpeedTestShell::scenario(Some("running")) {
        app.invoke(invoke).unwrap();
    }
    app.with_widget(|surface: &mut speed_test::app::SpeedTestApp| surface.stage(Some("running")))
        .expect("the production surface stages its running fixture");
    app.harness().resize((1280, 800)).unwrap();
    let harness = app.harness();
    let stop = harness
        .a11y_node_named("Stop test")
        .expect("a running test offers Stop");
    harness
        .perform_a11y_action(stop, &ui::a11y::Action::Activate)
        .unwrap();
    let _ = harness.poll_frame();
    let run = harness
        .a11y_node_named("Run speed test")
        .expect("stopping returns the button to Run");
    assert!(harness.a11y_node_named("Stop test").is_none());
    let (_, node) = harness
        .a11y_nodes()
        .into_iter()
        .find(|(id, _)| *id == run)
        .unwrap();
    assert!(
        !node.is_disabled(),
        "Run is bound to the resource's published start"
    );
}

/// Inspect the actual shared widgets, including the last result tile;
/// stacking below the action alone does not prove they fit horizontally.
#[test]
fn phone_width_contains_every_result_and_centres_the_test() {
    fn collect(widget: &dyn ui::widget::Widget, name: &str, bounds: &mut Vec<Rect>) {
        if widget.name() == name {
            bounds.push(widget.get_rect());
        }
        for child in widget.children() {
            collect(child, name, bounds);
        }
    }
    for viewport in [(390, 844), (480, 820)] {
        let mut app = staged("complete", viewport);
        let harness = app.harness();
        let mut tiles = Vec::new();
        collect(harness.root_widget(), "StatTile", &mut tiles);
        assert_eq!(tiles.len(), 3, "all measured results, including Upload");
        let mut dial = Vec::new();
        collect(harness.root_widget(), "ProgressRing", &mut dial);
        let mut tracker = Vec::new();
        collect(harness.root_widget(), "Stepper", &mut tracker);
        assert_eq!(dial.len(), 1);
        assert_eq!(tracker.len(), 1);
        for rect in tiles.iter().chain(&dial).chain(&tracker) {
            assert!(
                rect.width() > 0.0 && rect.x0 >= 0.0 && rect.x1 <= f64::from(viewport.0),
                "actual content leaves {viewport:?}: {rect:?}"
            );
        }
        let run = harness.targets()["run"];
        let axis = |rect: Rect| f64::midpoint(rect.x0, rect.x1);
        assert!((axis(dial[0]) - axis(run)).abs() <= 1.0);
        assert!((axis(dial[0]) - axis(tracker[0])).abs() <= 1.0);
    }
}
