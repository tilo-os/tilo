//! Click-driven interaction tests.
//!
//! These build the real widget tree, run the real five-pass layout, resolve
//! affordances by their laid-out rect, and inject real winit pointer events
//! through `Widget::handle_event` — the exact production path (ActionBus drain →
//! `TubeApp::apply`). No app-level shortcuts: a passing test means a real click
//! at the real coordinates did the real thing.

mod harness;

use harness::Harness;
use tube::model::Route;
use tube::TubeApp;

const VIEWPORT: (f32, f32) = (1280.0, 800.0);

#[test]
fn clicking_a_nav_tab_switches_route() {
    // Start on the sample's default (Watch). Click the "Trending" rail item.
    let mut h = Harness::new(TubeApp::new(), VIEWPORT);
    assert!(
        matches!(h.app.route_for_test(), Route::Watch(_)),
        "sample starts on the watch page"
    );

    h.click_id("nav_trending");
    assert_eq!(
        h.app.route_for_test(),
        &Route::Trending,
        "clicking the Trending tab navigates to Trending"
    );

    // And a second tab, to prove it isn't a one-off.
    h.click_id("nav_settings");
    assert_eq!(h.app.route_for_test(), &Route::Settings);
}

#[test]
fn selected_nav_tab_is_visible_and_addressable_after_switch() {
    // After navigating, the selected tab must still resolve (it's re-rendered
    // with the accent fill, same test_id) — i.e. selection is real, not a
    // rendering dead-end.
    let mut h = Harness::new(TubeApp::new(), VIEWPORT);
    h.click_id("nav_home");
    assert_eq!(h.app.route_for_test(), &Route::Home);
    assert!(
        h.has_id("nav_home"),
        "the now-selected Home tab is still in the laid-out tree"
    );
}

#[test]
fn palette_result_click_navigates_to_video() {
    // Open the palette pre-seeded with a query (headless can't press Ctrl+K —
    // see harness note), then click the first result and assert it opened a
    // watch page. This drives the real palette overlay + ActivateHit path.
    let app = TubeApp::new().with_palette_open_for_test("rust");
    let mut h = Harness::new(app, VIEWPORT);
    assert!(h.app.palette_open_for_test(), "palette starts open");
    assert!(h.has_id("palette_card"), "palette overlay is laid out");
    assert!(
        h.has_id("palette_row_0"),
        "query 'rust' should surface at least one result row"
    );

    // Click the first result row at its computed rect — the real ActivateHit
    // path — and assert it opened a watch page.
    h.click_id("palette_row_0");
    assert!(
        matches!(h.app.route_for_test(), Route::Watch(_)),
        "clicking a video result opens its watch page"
    );
    assert!(!h.app.palette_open_for_test(), "activating a result closes the palette");
}

#[test]
fn clicking_scrim_closes_palette() {
    let app = TubeApp::new().with_palette_open_for_test("");
    let mut h = Harness::new(app, VIEWPORT);
    assert!(h.app.palette_open_for_test());
    // Click near the top-left corner — inside the scrim, outside the centred
    // card — which must dismiss the palette.
    h.click_at(vello::kurbo::Point::new(20.0, 20.0));
    assert!(!h.app.palette_open_for_test(), "scrim click closes the palette");
}
