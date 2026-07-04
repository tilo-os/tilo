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

// ---------------------------------------------------------------------------
// text-entry surfaces must be reachable (the headline defect)
// ---------------------------------------------------------------------------

#[test]
fn top_bar_search_input_is_present_even_when_query_is_empty() {
    // Regression: the search box used to hide its real TextInput behind a
    // `!query.is_empty()` gate and show a static label, so with an empty query
    // there was NO input in the tree to focus — free-text search was
    // unreachable. The input must now always be laid out (placeholder-backed),
    // so a user can click into it and type from a cold start.
    let h = Harness::new(
        TubeApp::new().with_viewport(VIEWPORT.0 as f64, VIEWPORT.1 as f64),
        VIEWPORT,
    );
    assert_eq!(h.app.route_for_test(), &Route::Home);
    assert!(
        h.app.model_for_test().search_query.is_empty(),
        "precondition: the boot query is empty"
    );
    assert!(
        h.has_id("search_input"),
        "the search TextInput must be present (and focusable) even with an empty query"
    );
}

#[test]
fn command_palette_input_is_present_even_when_query_is_empty() {
    // Same defect on the Ctrl+K palette: the input was gated on a non-empty
    // query, so the freshly-opened (empty) palette had no field to type into.
    let app = TubeApp::new().with_palette_open_for_test("");
    let h = Harness::new(app, VIEWPORT);
    assert!(h.app.palette_open_for_test(), "palette starts open");
    assert!(h.has_id("palette_card"), "palette overlay is laid out");
    assert!(
        h.has_id("palette_input"),
        "the palette TextInput must be present even when the palette query is empty"
    );
}

// ---------------------------------------------------------------------------
// watch-page affordances
// ---------------------------------------------------------------------------

const WATCH_ID: &str = "vid_rust_async";

fn watch_app() -> TubeApp {
    TubeApp::new()
        .with_viewport(VIEWPORT.0 as f64, VIEWPORT.1 as f64)
        .with_route_shot(Route::Watch(WATCH_ID.into()))
}

#[test]
fn share_button_copies_a_link_and_is_not_wired_to_like() {
    // The Share pill was mis-wired to `Action::Like`. Clicking it must record a
    // "link copied" notice (the visible confirmation of the real clipboard
    // write) and must NOT like the video — proving the two are distinct.
    let mut h = Harness::new(watch_app(), VIEWPORT);
    assert!(!h.app.model_for_test().is_liked(WATCH_ID), "not liked to start");

    h.click_id("share_btn");

    assert!(
        !h.app.model_for_test().is_liked(WATCH_ID),
        "Share must not like the video (the old mis-wire did)"
    );
    assert!(
        h.app
            .model_for_test()
            .notices
            .iter()
            .any(|n| n.contains("Link copied") && n.contains(WATCH_ID)),
        "Share records a 'link copied' notice for this video"
    );
    // And the confirmation toast is actually rendered.
    assert!(h.has_id("notices"), "the share confirmation shows in the toast strip");
}

#[test]
fn like_button_toggles_liked_state_through_a_real_click() {
    let mut h = Harness::new(watch_app(), VIEWPORT);
    assert!(!h.app.model_for_test().is_liked(WATCH_ID));

    h.click_id("like_btn");
    assert!(
        h.app.model_for_test().is_liked(WATCH_ID),
        "clicking Like marks the video liked"
    );

    h.click_id("like_btn");
    assert!(
        !h.app.model_for_test().is_liked(WATCH_ID),
        "clicking Like again unlikes it"
    );
}

#[test]
fn transport_settings_gear_navigates_to_settings() {
    // The gear in the watch transport bar was a dead glyph. It must now open
    // the Settings screen.
    let mut h = Harness::new(watch_app(), VIEWPORT);
    assert!(matches!(h.app.route_for_test(), Route::Watch(_)));
    h.click_id("watch_settings");
    assert_eq!(
        h.app.route_for_test(),
        &Route::Settings,
        "the transport gear opens Settings"
    );
}

#[test]
fn play_pause_button_toggles_playing_through_a_real_click() {
    let mut h = Harness::new(watch_app(), VIEWPORT);
    let before = h.app.model_for_test().playing;
    h.click_id("play_pause");
    assert_ne!(
        before,
        h.app.model_for_test().playing,
        "the play/pause button flips the playing state"
    );
}

#[test]
fn subscribe_pill_toggles_subscription_through_a_real_click() {
    let mut h = Harness::new(watch_app(), VIEWPORT);
    let channel = h
        .app
        .model_for_test()
        .video(WATCH_ID)
        .unwrap()
        .channel_id
        .clone();
    let before = h.app.model_for_test().is_subscribed(&channel);
    h.click_id("subscribe_btn");
    assert_ne!(
        before,
        h.app.model_for_test().is_subscribed(&channel),
        "clicking the subscribe pill flips the subscription"
    );
}

#[test]
fn seek_bar_drag_seeks_and_auto_skips_a_sponsor_segment() {
    // Drive the scrubber's real drag handling: drag the playhead into the
    // SponsorBlock sponsor segment (1440–1570s of a 1620s video). Auto-skip
    // must fast-forward past it and record a skip notice — the exact
    // production seek → auto-skip path, through the widget (not app.apply).
    let mut h = Harness::new(watch_app(), VIEWPORT);
    let dur = h.app.model_for_test().video(WATCH_ID).unwrap().duration as f64;
    // Target ~1505s (inside the 1440–1570 sponsor band) as a fraction of width.
    h.drag_scrubber_to("seek_bar", 1505.0 / dur);

    let playhead = h.app.model_for_test().playhead;
    assert!(
        playhead >= 1570.0,
        "auto-skip must jump the playhead past the sponsor segment end (was {playhead})"
    );
    assert!(
        h.app
            .model_for_test()
            .notices
            .iter()
            .any(|n| n.contains("Sponsor")),
        "the auto-skip is surfaced as a notice"
    );
    assert!(
        matches!(h.app.route_for_test(), Route::Watch(id) if id == WATCH_ID),
        "a mid-video skip does not trigger autoplay-next"
    );
}

// ---------------------------------------------------------------------------
// settings-page controls
// ---------------------------------------------------------------------------

#[test]
fn autoplay_switch_toggles_the_setting_through_a_real_switch_click() {
    // Exercise `Switch::handle_event` for real (not app.apply): click the
    // autoplay switch on the Settings page and prove the setting flips.
    let app = TubeApp::new()
        .with_viewport(VIEWPORT.0 as f64, VIEWPORT.1 as f64)
        .with_route_shot(Route::Settings);
    let mut h = Harness::new(app, VIEWPORT);
    let before = h.app.model_for_test().settings.autoplay;
    h.click_id("switch_autoplay");
    assert_ne!(
        before,
        h.app.model_for_test().settings.autoplay,
        "clicking the autoplay switch flips the setting"
    );
}

#[test]
fn invidious_instance_row_cycles_the_configured_instance() {
    // The "Invidious instance" row was a dead affordance. Clicking it must
    // cycle to a different instance (the one live search actually queries).
    let app = TubeApp::new()
        .with_viewport(VIEWPORT.0 as f64, VIEWPORT.1 as f64)
        .with_route_shot(Route::Settings);
    let mut h = Harness::new(app, VIEWPORT);
    let before = h.app.model_for_test().settings.invidious_instance.clone();
    h.click_id("instance_row");
    assert_ne!(
        before,
        h.app.model_for_test().settings.invidious_instance,
        "clicking the instance row cycles to a different instance"
    );
}

// ---------------------------------------------------------------------------
// responsive nav rail — hamburger via a real click
// ---------------------------------------------------------------------------

#[test]
fn hamburger_toggles_the_overlay_rail_through_a_real_click() {
    // At a narrow width the rail is a collapsed overlay flap. The top-bar
    // hamburger must summon and dismiss it — via a real click through the menu
    // button's `handle_event`, not app.apply.
    let narrow = (560.0f32, 800.0f32);
    let app = TubeApp::new().with_viewport(narrow.0 as f64, narrow.1 as f64);
    let mut h = Harness::new(app, narrow);
    assert!(
        !h.app.sidebar_open_for_test(),
        "a narrow window starts with the rail collapsed"
    );

    h.click_id("nav_toggle");
    assert!(
        h.app.sidebar_open_for_test(),
        "the hamburger summons the overlay rail"
    );

    h.click_id("nav_toggle");
    assert!(
        !h.app.sidebar_open_for_test(),
        "a second click dismisses it"
    );
}
