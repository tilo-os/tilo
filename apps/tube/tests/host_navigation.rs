//! The navigation rail through the real host loop: a click reaches the
//! action-bound rail item, which the host routes back to the app.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use tube::TubeShell;
use ui::framework::testing::TestApp;
use ui::headless::FrameHarness;

fn names(harness: &FrameHarness) -> Vec<String> {
    harness
        .controls()
        .into_iter()
        .filter_map(|control| control.name)
        .collect()
}

/// Open the rail's Settings from `screen` in a live window (a capture's
/// controls start no effects).
fn opens_settings_from(screen: &str) {
    let mut app = TestApp::<TubeShell>::new().unwrap();
    app.effect(&tube::actions::TubeEffect::ShowScreen {
        screen: screen.into(),
    })
    .unwrap();
    let harness = app.harness();
    harness.relayout();
    let home = names(harness);
    let (x, y) = harness
        .controls()
        .into_iter()
        .find(|control| control.name.as_deref() == Some("Settings"))
        .and_then(|control| control.bounds)
        .map(|b| b.center())
        .map(|centre| (centre.x, centre.y))
        .expect("the rail's Settings item is placed");
    harness.click_at(x, y);
    app.settle();
    let harness = app.harness();
    harness.relayout();
    let settings = names(harness);
    assert_ne!(home, settings, "Settings did not open: {settings:?}");
}

#[test]
fn clicking_settings_in_the_rail_opens_settings() {
    opens_settings_from("home");
}

#[test]
fn the_rail_works_from_another_screen() {
    opens_settings_from("trending");
}
