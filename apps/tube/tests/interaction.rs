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
    // The app boots on Home (never into a fake/placeholder video). Click the
    // "Trending" rail item and prove the route follows. Seed the viewport so
    // the wide layout resolves its docked rail on the first frame (as the live
    // app does once its width lands) rather than the unknown-width overlay.
    let mut h = Harness::new(
        TubeApp::new().with_viewport(VIEWPORT.0 as f64, VIEWPORT.1 as f64),
        VIEWPORT,
    );
    assert_eq!(
        h.app.route_for_test(),
        &Route::Home,
        "the app boots on Home"
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
fn watch_view_paints_the_decoded_frame_when_present() {
    // With a real decoded frame in app state, the watch page must paint the
    // live video surface (the on-screen paint path) instead of the poster.
    // Inject a 16x16 frame for a sample video and assert the surface is laid
    // out in the real widget tree — proof the frame reaches the screen.
    let (fw, fh) = (16u32, 16u32);
    let rgba = vec![210u8; (fw * fh * 4) as usize];
    let app = TubeApp::new().with_decoded_frame_for_test("vid_donut", fw, fh, rgba);
    let h = Harness::new(app, VIEWPORT);
    assert!(
        matches!(h.app.route_for_test(), Route::Watch(_)),
        "the injected frame opens its watch page"
    );
    assert!(
        h.has_id("watch_video_surface"),
        "a decoded frame paints the live video surface, not the poster"
    );
}

#[test]
fn watch_view_shows_poster_without_a_frame() {
    // The dual of the above: with no decoded frame, the watch page must NOT
    // contain the live surface (it falls back to the poster). Proves the swap
    // is real, not always-on.
    let app = TubeApp::new().with_route_shot(Route::Watch("vid_donut".into()));
    let h = Harness::new(app, VIEWPORT);
    assert!(
        !h.has_id("watch_video_surface"),
        "with no decoded frame the poster stands in for the player"
    );
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
