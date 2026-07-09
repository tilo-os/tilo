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
fn watch_view_lays_out_the_shared_video_player_surface() {
    // The watch page mounts the app's persistent, media-engine-backed
    // `ui::widget::VideoPlayer` (via `PlayerHost`) as its player surface. Assert
    // it is laid out in the real widget tree on a watch route — proof the shared
    // player reaches the screen.
    let app = TubeApp::new().with_route_shot(Route::Watch("vid_donut".into()));
    let h = Harness::new(app, VIEWPORT);
    assert!(
        matches!(h.app.route_for_test(), Route::Watch(_)),
        "the app is on a watch page"
    );
    assert!(
        h.has_id("watch_video_surface"),
        "the watch page lays out the shared video-player surface"
    );
}

#[test]
fn non_watch_views_have_no_video_player_surface() {
    // The dual: off a watch route (Home) the player surface must not be in the
    // tree — the surface is scoped to the watch page, not always-on.
    let app = TubeApp::new().with_viewport(VIEWPORT.0 as f64, VIEWPORT.1 as f64);
    let h = Harness::new(app, VIEWPORT);
    assert_eq!(h.app.route_for_test(), &Route::Home);
    assert!(
        !h.has_id("watch_video_surface"),
        "the video-player surface only exists on the watch page"
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
fn watch_player_is_flush_to_the_content_edges() {
    // Issue 4: the embedded player must sit flush to the top/left/right of the
    // content area — no page-padding gap to the window edge or the nav rail. At
    // 1280px the rail is docked (86px), so the content area is x∈[86,1280],
    // y starting just below the 66px top bar; the player must fill that exactly.
    let h = Harness::new(watch_app(), VIEWPORT);
    let player = h
        .rect_by_id("watch_video_surface")
        .expect("the watch page lays out the shared video-player surface");

    let content_left = tube::screens::NAV_RAIL_W; // docked rail width
    let top_bar_h = 66.0; // top_bar fixed height
    let content_w = VIEWPORT.0 as f64 - content_left;

    assert!(
        (player.x0 - content_left).abs() < 1.5,
        "player flush-left to the content area (x0={}, expected {content_left})",
        player.x0
    );
    assert!(
        (player.x1 - VIEWPORT.0 as f64).abs() < 1.5,
        "player flush-right to the window edge (x1={}, expected {})",
        player.x1,
        VIEWPORT.0
    );
    assert!(
        player.y0 < top_bar_h + 6.0,
        "player flush to the top — no padding gap (y0={}, top bar {top_bar_h})",
        player.y0
    );
    assert!(
        (player.width() - content_w).abs() < 1.5,
        "player spans the full content width, not inset by page padding (w={}, expected {content_w})",
        player.width()
    );
}

#[test]
fn watch_page_lays_out_dislikes_pill_and_sponsorblock_timeline() {
    // The Return YouTube Dislike pill and the SponsorBlock segment timeline are
    // both real widgets on the watch page (WATCH_ID carries sponsor segments and
    // a seeded dislike count). Assert they're laid out in the real tree.
    let h = Harness::new(watch_app(), VIEWPORT);
    assert!(
        h.has_id("dislike_pill"),
        "the watch page lays out the Return YouTube Dislike pill"
    );
    assert!(
        h.has_id("sb_timeline"),
        "the watch page lays out the SponsorBlock segment timeline"
    );
}

#[test]
fn settings_category_mode_pill_cycles_the_skip_mode_through_a_real_click() {
    use tube::model::{Category, SkipMode};
    let app = TubeApp::new()
        .with_viewport(VIEWPORT.0 as f64, VIEWPORT.1 as f64)
        .with_route_shot(Route::Settings);
    let mut h = Harness::new(app, VIEWPORT);
    // Sponsor defaults to Auto-skip; clicking its mode pill cycles it to Show.
    assert_eq!(
        h.app.model_for_test().settings.mode_of(Category::Sponsor),
        SkipMode::Skip
    );
    h.click_id(&format!("mode_{}", Category::Sponsor.api_key()));
    assert_eq!(
        h.app.model_for_test().settings.mode_of(Category::Sponsor),
        SkipMode::Show,
        "clicking the mode pill cycles Auto-skip → Show"
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

// The play/pause and scrub transport now live in the shared `VideoPlayer`'s
// controls overlay (driven by the `media::MediaEngine`), and SponsorBlock
// auto-skip runs against the engine playhead each frame. Those paths are proven
// against the real engine in `app.rs`'s `toggle_play_drives_the_engine` and
// `auto_skip_seeks_the_engine_past_a_sponsor_segment` unit tests — the headless
// click harness can't address the player's in-surface controls by id.

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

// ---------------------------------------------------------------------------
// Scroll-performance + responsive-grid + real-image regressions
// ---------------------------------------------------------------------------

/// REGRESSION (task 4): a real click on the top search field must move keyboard
/// focus to the `search_input` TextInput. Drives the actual hit-test +
/// event-routing path (`handle_event` → `focus_target`), the thing that broke.
#[test]
fn clicking_the_search_field_focuses_the_input() {
    let app = TubeApp::new().with_viewport(VIEWPORT.0 as f64, VIEWPORT.1 as f64);
    let mut h = Harness::new(app, VIEWPORT);

    // The field is present and has a real, clickable rect (not zero-sized /
    // collapsed — an earlier failure mode).
    let r = h
        .rect_by_id("search_input")
        .expect("the top-bar search input is in the laid-out tree");
    assert!(
        r.width() > 40.0 && r.height() > 10.0,
        "the search field has a real clickable rect: {r:?}",
    );

    // A click at its rect focuses it — the exact production path.
    assert!(
        h.click_focuses("search_input"),
        "clicking the search field must focus the search_input TextInput",
    );
}

/// Task 2: the responsive grid fills each row edge-to-edge — no blank gutter on
/// the right — and adds columns as the window widens. Driven at three widths.
#[test]
fn home_grid_fills_the_row_with_no_right_gutter_across_widths() {
    let mut columns_per_width = Vec::new();
    for w in [1040.0_f32, 1360.0, 1760.0] {
        let vp = (w, 900.0);
        let app = TubeApp::new().with_viewport(w as f64, 900.0);
        let h = Harness::new(app, vp);

        let grid = h
            .rect_by_id("video_grid")
            .unwrap_or_else(|| panic!("home grid laid out at width {w}"));
        let cards = h.child_rects("video_grid");
        assert!(cards.len() >= 4, "grid has cards at width {w} (got {})", cards.len());

        // The top row: cards sharing the smallest y0.
        let top_y = cards.iter().map(|r| r.y0).fold(f64::INFINITY, f64::min);
        let mut top: Vec<&vello::kurbo::Rect> =
            cards.iter().filter(|r| (r.y0 - top_y).abs() < 1.0).collect();
        top.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap());
        assert!(top.len() >= 2, "the grid has >= 2 columns at width {w} (got {})", top.len());
        columns_per_width.push(top.len());

        // Equal-width flex tracks.
        let w0 = top[0].width();
        for c in &top {
            assert!(
                (c.width() - w0).abs() < 2.0,
                "grid columns are equal width at {w}: {} vs {}",
                c.width(),
                w0,
            );
        }
        // No right gutter: the rightmost card reaches the grid's right edge.
        let right = top.last().unwrap().x1;
        assert!(
            (grid.x1 - right).abs() < 2.0,
            "no right gutter at width {w}: grid right {} vs last card right {} (gap {})",
            grid.x1,
            right,
            grid.x1 - right,
        );
    }
    // Responsive: a wider window fits strictly more columns than a narrow one.
    assert!(
        columns_per_width.last() > columns_per_width.first(),
        "columns must grow with width: {columns_per_width:?}",
    );
}

/// Task 3: a real creator avatar (fetched + decoded through the ImageStore) is
/// painted on the Home cards — not the initials-only fallback. Drives the whole
/// Home screen with a seeded store and asserts an IMAGE draw reaches the scene.
#[test]
fn home_channel_avatars_render_the_fetched_photo() {
    use std::sync::Arc;
    use std::time::Duration;

    // A solid-green PNG the store's decoder accepts.
    fn png_bytes() -> Vec<u8> {
        let (w, h) = (8u32, 8u32);
        let mut rgba = Vec::new();
        for _ in 0..(w * h) {
            rgba.extend_from_slice(&[20, 200, 60, 255]);
        }
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&rgba).unwrap();
        }
        out
    }

    let avatar_url = "http://test.local/avatar.png";
    let bytes = png_bytes();
    let fetch: ui::Fetcher = Arc::new(move |_url: &str| Some(bytes.clone()));
    let store = ui::ImageStore::new(fetch);
    // Pre-warm so the single sync in the first frame resolves it immediately.
    let _ = store.get(avatar_url);
    store.block_until_idle(Duration::from_secs(5));

    // A Home model whose videos carry NO thumbnail URL (so the only image that
    // can paint is the avatar) and whose uploader has a real avatar URL.
    let mut model = tube::sample::model();
    model.route = Route::Home;
    for v in &mut model.videos {
        v.thumbnail_url.clear();
    }
    let cid = model.videos[0].channel_id.clone();
    // Set the avatar URL on that uploader's channel record.
    let mut found = false;
    // `channels` is walked via the model's public accessor; set through a fresh
    // channel upsert so the avatar URL lands on the record the card reads.
    if let Some(ch) = model.channels.iter_mut().find(|c| c.id == cid) {
        ch.avatar_url = avatar_url.to_string();
        found = true;
    }
    assert!(found, "seeded an avatar URL on the first video's uploader");

    let app = TubeApp::new()
        .with_viewport(1280.0, 900.0)
        .with_model_for_test(model)
        .with_image_store(store);
    let mut h = Harness::new(app, (1280.0, 900.0));

    assert!(
        h.image_draw_count() >= 1,
        "a real decoded creator avatar must paint (IMAGE draw), got initials only",
    );
}

/// Scroll-perf guard: after a fully laid-out + drawn frame, NO widget in the
/// Home tree may still demand a layout re-measure. A widget that reports "dirty"
/// forever — e.g. an avatar keyed off a draw-time cache while it's culled
/// off-screen — forces the enclosing NavSplit to rebuild a cosmic-text
/// FontSystem + re-solve the whole content pane on EVERY scroll frame (measured
/// at ~26 ms/frame before the fix). This is the load-bearing invariant.
#[test]
fn laid_out_home_does_not_demand_relayout_a_scroll_perf_guard() {
    // A large catalogue so most cards (and their avatars) are culled off-screen.
    let mut model = tube::sample::model();
    model.route = Route::Home;
    let base = model.videos.clone();
    let mut i = 0usize;
    while model.videos.len() < 60 && !base.is_empty() {
        let mut v = base[i % base.len()].clone();
        v.id = format!("{}_{i}", v.id);
        v.seed = v.id.clone();
        model.videos.push(v);
        i += 1;
    }

    let app = TubeApp::new().with_viewport(1280.0, 900.0).with_model_for_test(model);
    let mut h = Harness::new(app, (1280.0, 900.0));
    // One real drawn frame (visible widgets warm their caches, off-screen ones
    // are culled — exactly the state a scroll leaves the tree in).
    let _ = h.image_draw_count();

    assert!(
        !h.layout_needs_measure(),
        "a laid-out, drawn Home must not report layout_needs_measure — an \
         off-screen widget forcing a re-solve is the scroll-jank regression",
    );
}
