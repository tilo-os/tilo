//! Click-driven interaction tests.
//!
//! These build the real widget tree, run the real five-pass layout, resolve
//! affordances by their laid-out rect, and inject real winit pointer events
//! through `Widget::handle_event` — the exact production path (`ActionBus` drain →
//! `TubeApp::apply`). No app-level shortcuts: a passing test means a real click
//! at the real coordinates did the real thing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

mod harness;

use harness::Harness;
use tube::TubeApp;
use tube::model::Route;

const VIEWPORT: (f32, f32) = (1280.0, 800.0);

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn navigation_tabs_and_queue_are_real_destinations() {
    // Case `clicking_a_nav_tab_switches_route`.
    {
        // The app boots on Home (never into a fake/placeholder video). Click the
        // "Trending" rail item and prove the route follows. Seed the viewport so
        // the wide layout resolves its docked rail on the first frame (as the live
        // app does once its width lands) rather than the unknown-width overlay.
        let mut h = Harness::new(
            TubeApp::new().with_viewport(f64::from(VIEWPORT.0), f64::from(VIEWPORT.1)),
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
    // Case `shared_queue_is_a_real_navigation_destination`.
    {
        // The queue has both a visible rail destination and a canonical navigation
        // action, rather than being an agent-only collection hidden behind Ctrl+K.
        let mut h = Harness::new(
            TubeApp::new().with_viewport(f64::from(VIEWPORT.0), f64::from(VIEWPORT.1)),
            VIEWPORT,
        );
        h.click_id("nav_queue");
        assert_eq!(h.app.route_for_test(), &Route::Queue);
        assert!(
            h.has_id("queue_page"),
            "the shared queue screen is laid out"
        );
    }
    // Case `shared_queue_200_percent_text_reserves_a_distinct_rect_for_every_line`.
    {
        // Regression for the 200%-text screenshot: wrapped prose used to paint
        // beneath a one-line flex allocation, causing the heading and empty state
        // to collide. The real layout tree must give every rendered line a later
        // y-origin than the one before it.
        let app = TubeApp::new()
            .with_viewport(800.0, 720.0)
            .with_route_shot(Route::Queue);
        let h = Harness::with_theme(
            app,
            (800.0, 720.0),
            ui::theme::Theme::dark().with_text_scale(2.0),
        );
        let ids = [
            "queue_heading_title",
            "queue_heading_count",
            "queue_heading_membership",
            "queue_heading_order",
            "queue_empty_title",
            "queue_empty_add",
            "queue_empty_sync",
        ];
        let rects: Vec<_> = ids
            .iter()
            .map(|id| h.rect_by_id(id).unwrap_or_else(|| panic!("missing {id}")))
            .collect();
        assert!(
            rects[0].height() >= 40.0,
            "200% title must reserve scaled line height, got {:?}",
            rects[0]
        );
        assert!(
            rects[1].height() >= 25.0,
            "200% metadata must reserve scaled line height, got {:?}",
            rects[1]
        );
        for pair in rects.windows(2) {
            assert!(
                pair[0].y1 <= pair[1].y0,
                "large-text lines overlap: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
    }
    // Case `selected_nav_tab_is_visible_and_addressable_after_switch`.
    {
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
    // Case `transport_settings_gear_navigates_to_settings`.
    {
        let mut h = Harness::new(watch_app(), VIEWPORT);
        assert!(matches!(h.app.route_for_test(), Route::Watch(_)));
        h.click_id("watch_settings");
        assert_eq!(
            h.app.route_for_test(),
            &Route::Settings,
            "the transport gear opens Settings"
        );
    }
    // Case `hamburger_toggles_the_overlay_rail_through_a_real_click`.
    {
        // At a narrow width the rail is a collapsed overlay flap. The top-bar
        // hamburger must summon and dismiss it — via a real click through the menu
        // button's `handle_event`, not app.apply.
        let narrow = (560.0f32, 800.0f32);
        let app = TubeApp::new().with_viewport(f64::from(narrow.0), f64::from(narrow.1));
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
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn palette_and_search_inputs_are_present_and_navigate() {
    // Case `palette_result_click_navigates_to_video`.
    {
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
        assert!(
            !h.app.palette_open_for_test(),
            "activating a result closes the palette"
        );
    }
    // Case `clicking_scrim_closes_palette`.
    {
        let app = TubeApp::new().with_palette_open_for_test("");
        let mut h = Harness::new(app, VIEWPORT);
        assert!(h.app.palette_open_for_test());
        // Click near the top-left corner — inside the scrim, outside the centred
        // card — which must dismiss the palette.
        h.click_at(vello::kurbo::Point::new(20.0, 20.0));
        assert!(
            !h.app.palette_open_for_test(),
            "scrim click closes the palette"
        );
    }
    // Case `top_bar_search_input_is_present_even_when_query_is_empty`.
    {
        let h = Harness::new(
            TubeApp::new().with_viewport(f64::from(VIEWPORT.0), f64::from(VIEWPORT.1)),
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
    // Case `command_palette_input_is_present_even_when_query_is_empty`.
    {
        let app = TubeApp::new().with_palette_open_for_test("");
        let h = Harness::new(app, VIEWPORT);
        assert!(h.app.palette_open_for_test(), "palette starts open");
        assert!(h.has_id("palette_card"), "palette overlay is laid out");
        assert!(
            h.has_id("palette_input"),
            "the palette TextInput must be present even when the palette query is empty"
        );
    }
    // Case `clicking_the_search_field_focuses_the_input`:
    // A real click on the top search field must move keyboard focus to the
    // `search_input` `TextInput` — drives the actual hit-test + event-routing path
    // (`handle_event` → `focus_target`).
    {
        let app = TubeApp::new().with_viewport(f64::from(VIEWPORT.0), f64::from(VIEWPORT.1));
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
    // Case `search_open_video_then_back_returns_to_the_same_results_with_scroll_preserved`.
    {
        // From a results list, opening a video and pressing Back must land on the
        // EXACT same search results, with the scroll position preserved — all
        // through real clicks on real widgets.
        let query = "rust";
        // Use several sample video ids so the results list is long enough to scroll.
        let sample = tube::sample::model();
        let ids: Vec<String> = sample.videos.iter().map(|v| v.id.clone()).collect();
        assert!(
            ids.len() >= 3,
            "need a few sample videos to build a results list"
        );
        let app = tube::TubeApp::new()
            .with_viewport(f64::from(VIEWPORT.0), f64::from(VIEWPORT.1))
            .with_model_for_test(sample)
            .with_search_for_test(query, ids.clone());
        let mut h = Harness::new(app, VIEWPORT);
        assert_eq!(h.app.route_for_test(), &Route::Search(query.into()));
        assert!(
            h.app.can_go_back_for_test(),
            "arriving via search leaves Home to go back to"
        );

        // Scroll the results down, then record where the grid sits.
        h.wheel_at(vello::kurbo::Point::new(640.0, 500.0), -6.0);
        let grid_after_scroll = h
            .rect_by_id("video_grid")
            .expect("results grid laid out")
            .y0;

        // Open a *visible* result via a real click. The first result is now above
        // the clipped viewport, so activating it would be a synthetic off-screen
        // click rather than a user-visible search interaction.
        let opened = ids
            .iter()
            .find(|id| {
                h.rect_by_id(&format!("video_card_{id}"))
                    .is_some_and(|rect| {
                        let center = rect.center();
                        center.x >= 0.0
                            && center.x < f64::from(VIEWPORT.0)
                            && center.y >= 0.0
                            && center.y < f64::from(VIEWPORT.1)
                    })
            })
            .expect("a scrolled search result remains visible and clickable")
            .clone();
        h.click_id(&format!("video_card_{opened}"));
        assert!(
            matches!(h.app.route_for_test(), Route::Watch(id) if *id == opened),
            "clicking a result opens its watch page"
        );

        // Press Back (a real click on the top-bar Back button).
        assert!(
            h.has_id("back_btn"),
            "the Back affordance is present on the watch page"
        );
        h.click_id("back_btn");
        assert_eq!(
            h.app.route_for_test(),
            &Route::Search(query.into()),
            "Back returns to the exact same search results",
        );
        assert!(h.has_id("video_grid"), "the results list is rendered again");

        // Scroll position preserved: the grid resumes where it was, not reset to top.
        let grid_after_back = h
            .rect_by_id("video_grid")
            .expect("results grid re-laid out")
            .y0;
        assert!(
            (grid_after_back - grid_after_scroll).abs() < 2.0,
            "scroll preserved on Back: grid y0 {grid_after_back} vs pre-nav {grid_after_scroll}",
        );
    }
}

#[test]
fn watch_page_lays_out_player_pills_and_timeline() {
    // Case `watch_view_lays_out_the_shared_video_player_surface`.
    {
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
    // Case `non_watch_views_have_no_video_player_surface`.
    {
        // The dual: off a watch route (Home) the player surface must not be in the
        // tree — the surface is scoped to the watch page, not always-on.
        let app = TubeApp::new().with_viewport(f64::from(VIEWPORT.0), f64::from(VIEWPORT.1));
        let h = Harness::new(app, VIEWPORT);
        assert_eq!(h.app.route_for_test(), &Route::Home);
        assert!(
            !h.has_id("watch_video_surface"),
            "the video-player surface only exists on the watch page"
        );
    }
    // Case `watch_player_is_height_driven_16x9_centered_and_clear_of_the_scrollbar`.
    {
        // The player is sized by available HEIGHT (a true 16:9 box capped near ~65%
        // of the viewport height), centered horizontally in the content lane, and
        // clear of the reserved scrollbar gutter on the right — never a wide, short
        // strip that overlaps the bar.
        let h = Harness::new(watch_app(), VIEWPORT);
        let player = h
            .rect_by_id("watch_video_surface")
            .expect("the watch page lays out the shared video-player surface");

        let content_left = tube::screens::NAV_RAIL_W; // docked rail width (86)
        let viewport_h = f64::from(VIEWPORT.1);
        // The content lane: the split's content pane minus the reserved scrollbar
        // gutter on the right.
        let lane_left = content_left;
        let lane_right = f64::from(VIEWPORT.0) - f64::from(tube::app::SCROLL_GUTTER);

        // 16:9 aspect.
        let aspect = player.width() / player.height();
        assert!(
            (aspect - 16.0 / 9.0).abs() < 0.03,
            "player must be 16:9 (got {aspect:.3}: {}x{})",
            player.width(),
            player.height()
        );
        // Height-driven: ~65% of the viewport height, not a fixed pixel cap.
        assert!(
            player.height() > 0.55 * viewport_h && player.height() <= 0.66 * viewport_h + 2.0,
            "player height {} should be ~65% of the {viewport_h}px viewport (bigger video)",
            player.height()
        );
        // Centered in the content lane.
        let lane_center = f64::midpoint(lane_left, lane_right);
        assert!(
            (player.center().x - lane_center).abs() < 3.0,
            "player centered in the content lane (center.x={}, lane center {lane_center})",
            player.center().x
        );
        // Clear of the scrollbar gutter: the player's right edge never reaches the
        // reserved bar lane.
        assert!(
            player.x1 <= lane_right + 0.5,
            "player right edge {} must stay left of the scrollbar lane ({lane_right})",
            player.x1
        );
        // And it stays inset from the rail on the left.
        assert!(
            player.x0 >= lane_left - 0.5,
            "player left edge {} stays right of the nav rail ({lane_left})",
            player.x0
        );
    }
    // Case `watch_page_lays_out_dislikes_pill_and_sponsorblock_timeline`.
    {
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
}

// ---------------------------------------------------------------------------
// text-entry surfaces must be reachable
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// watch-page affordances
// ---------------------------------------------------------------------------

const WATCH_ID: &str = "vid_rust_async";

fn watch_app() -> TubeApp {
    TubeApp::new()
        .with_viewport(f64::from(VIEWPORT.0), f64::from(VIEWPORT.1))
        .with_route_shot(Route::Watch(WATCH_ID.into()))
}

#[test]
fn watch_and_settings_controls_toggle_state_through_real_clicks() {
    // Case `share_button_copies_a_link_and_is_not_wired_to_like`.
    {
        // Clicking Share must record a "link copied" notice (the visible
        // confirmation of the real clipboard write) and must NOT like the video —
        // proving the two are distinct.
        let mut h = Harness::new(watch_app(), VIEWPORT);
        assert!(
            !h.app.model_for_test().is_liked(WATCH_ID),
            "not liked to start"
        );

        h.click_id("share_btn");

        assert!(
            !h.app.model_for_test().is_liked(WATCH_ID),
            "Share must not like the video"
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
        assert!(
            h.has_id("notices"),
            "the share confirmation shows in the toast strip"
        );
    }
    // Case `like_button_toggles_liked_state_through_a_real_click`.
    {
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
    // Case `settings_category_mode_pill_cycles_the_skip_mode_through_a_real_click`.
    {
        use tube::model::{Category, SkipMode};
        let app = TubeApp::new()
            .with_viewport(f64::from(VIEWPORT.0), f64::from(VIEWPORT.1))
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
    // Case `subscribe_pill_toggles_subscription_through_a_real_click`.
    {
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
    // Case `autoplay_switch_toggles_the_setting_through_a_real_switch_click`.
    {
        // Exercise `Switch::handle_event` for real (not app.apply): click the
        // autoplay switch on the Settings page and prove the setting flips.
        let app = TubeApp::new()
            .with_viewport(f64::from(VIEWPORT.0), f64::from(VIEWPORT.1))
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

// ---------------------------------------------------------------------------
// responsive nav rail — hamburger via a real click
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Scroll-performance + responsive-grid + real-image regressions
// ---------------------------------------------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn home_grid_fills_rows_renders_avatars_and_stays_laid_out() {
    // Case `home_grid_fills_the_row_with_no_right_gutter_across_widths`:
    // The responsive grid fills each row edge-to-edge — no blank gutter on the
    // right — and adds columns as the window widens. Driven at three widths.
    {
        let mut columns_per_width = Vec::new();
        for w in [1040.0_f32, 1360.0, 1760.0] {
            let vp = (w, 900.0);
            let app = TubeApp::new().with_viewport(f64::from(w), 900.0);
            let h = Harness::new(app, vp);

            let grid = h
                .rect_by_id("video_grid")
                .unwrap_or_else(|| panic!("home grid laid out at width {w}"));
            let cards = h.child_rects("video_grid");
            assert!(
                cards.len() >= 4,
                "grid has cards at width {w} (got {})",
                cards.len()
            );

            // The top row: cards sharing the smallest y0.
            let top_y = cards.iter().map(|r| r.y0).fold(f64::INFINITY, f64::min);
            let mut top: Vec<&vello::kurbo::Rect> = cards
                .iter()
                .filter(|r| (r.y0 - top_y).abs() < 1.0)
                .collect();
            top.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap());
            assert!(
                top.len() >= 2,
                "the grid has >= 2 columns at width {w} (got {})",
                top.len()
            );
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
    // Case `home_channel_avatars_render_the_fetched_photo`:
    // A real creator avatar (fetched + decoded through the `ImageStore`) is
    // painted on the Home cards — not the initials-only fallback. Drives the
    // whole Home screen with a seeded store and asserts an IMAGE draw reaches
    // the scene.
    {
        use std::sync::Arc;

        // A solid-green PNG the store's decoder accepts.
        fn png_bytes() -> Vec<u8> {
            let (w, h) = (8u32, 8u32);
            let mut rgba = Vec::new();
            for _ in 0..(w * h) {
                rgba.extend_from_slice(&[20, 200, 60, 255]);
            }
            ui::png::encode_rgba8(w, h, &rgba).unwrap()
        }

        let avatar_url = "http://test.local/avatar.png";
        let bytes = png_bytes();
        let fetch: ui::Fetcher = Arc::new(move |_url: &str| Some(bytes.clone()));
        let store = ui::ImageStore::new(&fetch);
        // Pre-warm so the single sync in the first frame resolves it immediately.
        let _ = store.get(avatar_url);
        store.block_until_idle(tilo_test_env::hang_bound());

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
    // Case `reconciled_avatars_render_on_the_majority_of_home_cards`:
    // The yt-dlp/rustypipe channel-record mismatch: most cards are uploaded by
    // one creator whose *primary* channel record carries NO avatar, while a
    // second record (a creator row, different id, same name) does. After
    // `reconcile_channel_avatars` every card for that creator must resolve the
    // real avatar — proving the reconciliation fixes the MAJORITY, not just one
    // row.
    {
        use std::sync::Arc;
        use tube::model::{Channel, TubeModel, Video};

        fn png_bytes() -> Vec<u8> {
            let (w, h) = (8u32, 8u32);
            let mut rgba = Vec::new();
            for _ in 0..(w * h) {
                rgba.extend_from_slice(&[200, 60, 20, 255]);
            }
            ui::png::encode_rgba8(w, h, &rgba).unwrap()
        }

        let avatar_url = "http://test.local/creator.png";
        let bytes = png_bytes();
        let fetch: ui::Fetcher = Arc::new(move |_u: &str| Some(bytes.clone()));
        let store = ui::ImageStore::new(&fetch);
        let _ = store.get(avatar_url);
        store.block_until_idle(tilo_test_env::hang_bound());

        // 12 videos all uploaded by "Creator X" via its PRIMARY record (id UC_A),
        // which has NO avatar (the yt-dlp record). No thumbnails, so the only image a
        // card can paint is the avatar.
        let mut model = TubeModel::default();
        for i in 0..12 {
            model.videos.push(Video {
                id: format!("v{i}"),
                title: format!("Video {i}"),
                dearrow_title: None,
                channel_id: "UC_A".into(),
                channel_name: "Creator X".into(),
                channel_seed: "UC_A".into(),
                channel_avatar_url: String::new(),
                views: 1000,
                published: "1 day ago".into(),
                duration: 60,
                description: String::new(),
                likes: 0,
                seed: format!("v{i}"),
                thumbnail_url: String::new(),
                category: "Search".into(),
                dearrow_thumb: false,
                sponsor: vec![],
                comments: vec![],
                live: false,
            });
        }
        // The primary uploader record (id UC_A) — NO avatar.
        model.channels.push(Channel {
            id: "UC_A".into(),
            name: "Creator X".into(),
            handle: "UC_A".into(),
            subscribers: 0,
            verified: false,
            description: String::new(),
            seed: "UC_A".into(),
            avatar_url: String::new(),
            banner_url: String::new(),
        });
        // A SECOND record for the same creator (different id, same name) that DOES
        // carry the avatar — the rustypipe creator row.
        model.channels.push(Channel {
            id: "UC_B".into(),
            name: "Creator X".into(),
            handle: "@creatorx".into(),
            subscribers: 500,
            verified: true,
            description: String::new(),
            seed: "UC_B".into(),
            avatar_url: avatar_url.to_string(),
            banner_url: String::new(),
        });

        // Before reconciliation: the cards read UC_A (empty) → initials only.
        let app_before = TubeApp::new()
            .with_viewport(1280.0, 900.0)
            .with_model_for_test(model.clone())
            .with_image_store(store.clone());
        let mut hb = Harness::new(app_before, (1280.0, 900.0));
        let avatars_before = hb.image_draw_count();

        // The production reconciliation pass bridges UC_B's avatar onto UC_A.
        model.reconcile_channel_avatars();
        assert_eq!(
            model.channel("UC_A").unwrap().avatar_url,
            avatar_url,
            "reconcile stamps the same-name record's avatar onto the primary record",
        );

        let app = TubeApp::new()
            .with_viewport(1280.0, 900.0)
            .with_model_for_test(model)
            .with_image_store(store);
        let mut h = Harness::new(app, (1280.0, 900.0));
        let avatars_after = h.image_draw_count();

        // A 1280×900 Home shows several rows of cards; after reconciliation the
        // MAJORITY of the visible cards must paint a real avatar (many IMAGE draws),
        // not the single row that happened to carry one.
        assert!(
            avatars_after >= 4,
            "the majority of visible cards must show a real avatar after reconcile \
         (before={avatars_before}, after={avatars_after})",
        );
        assert!(
            avatars_after > avatars_before,
            "reconciliation must increase real-avatar coverage (before={avatars_before}, after={avatars_after})",
        );
    }
    // Case `laid_out_home_does_not_demand_relayout_a_scroll_perf_guard`:
    // Scroll-perf guard: after a fully laid-out + drawn frame, NO widget in the
    // Home tree may still demand a layout re-measure. A widget that reports "dirty"
    // forever — e.g. an avatar keyed off a draw-time cache while it's culled
    // off-screen — forces the enclosing `NavSplit` to rebuild a cosmic-text
    // `FontSystem` + re-solve the whole content pane on EVERY scroll frame, measured
    // at ~26 ms/frame when this regresses.
    {
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

        let app = TubeApp::new()
            .with_viewport(1280.0, 900.0)
            .with_model_for_test(model);
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
}
