//! The navigation rail through the real host loop: a click reaches the
//! action-bound rail item, which the host routes back to the app.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use ui::headless::FrameHarness;

fn names(harness: &FrameHarness) -> Vec<String> {
    harness
        .controls()
        .into_iter()
        .filter_map(|control| control.name)
        .collect()
}

fn opens_settings_from(root: Box<dyn ui::widget::Widget>) {
    let mut harness = FrameHarness::new(ui::State::new(), root, (1280, 800));
    harness.relayout();
    let home = names(&harness);
    let (x, y) = harness
        .controls()
        .into_iter()
        .find(|control| control.name.as_deref() == Some("Settings"))
        .and_then(|control| control.bounds)
        .map(|b| b.center())
        .map(|centre| (centre.x, centre.y))
        .expect("the rail's Settings item is placed");
    harness.click_at(x, y);
    harness.relayout();
    let settings = names(&harness);
    assert_ne!(home, settings, "Settings did not open: {settings:?}");
}

#[test]
fn clicking_settings_in_the_rail_opens_settings() {
    opens_settings_from(tube::app_root());
}

#[test]
fn the_rail_works_from_a_watch_page() {
    opens_settings_from(tube::app_root_route_shot(tube::model::Route::Watch(
        "vid_donut".into(),
    )));
}
