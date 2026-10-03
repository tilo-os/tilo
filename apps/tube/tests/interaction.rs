//! Click-driven interaction tests.
//!
//! Each test mounts the real app in the toolkit's headless host
//! (`ui::headless::Framed`), lays it out with the real passes, finds controls
//! by their accessible names or test ids, and clicks at their real
//! coordinates. Action-bound controls go through the host's resource door,
//! exactly as in the live app.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Arc;

use tube::TubeApp;
use tube::model::Route;
use ui::headless::Framed;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Rect;

const VIEWPORT: (u32, u32) = (1280, 800);

fn mount(app: TubeApp) -> Framed<TubeApp> {
    mount_at(app, VIEWPORT, Theme::dark())
}

fn mount_at(app: TubeApp, viewport: (u32, u32), theme: Theme) -> Framed<TubeApp> {
    let (w, h) = viewport;
    let app = app.with_viewport(f64::from(w), f64::from(h));
    let mut framed = Framed::new(ui::State::new(), app, viewport, theme);
    Framed::harness_mut(&mut framed).relayout();
    framed
}

/// Click the control the host exports under `name`, then lay out again.
fn click_control(framed: &mut Framed<TubeApp>, name: &str) {
    let harness = Framed::harness_mut(framed);
    let centre = harness
        .controls()
        .into_iter()
        .find(|control| control.name.as_deref() == Some(name))
        .and_then(|control| control.bounds)
        .map_or_else(
            || panic!("no control named {name:?}"),
            |bounds| bounds.center(),
        );
    harness.click_at(centre.x, centre.y);
    harness.relayout();
}

/// Click the widget with test id `id`, then lay out again.
fn click_id(framed: &mut Framed<TubeApp>, id: &str) {
    let harness = Framed::harness_mut(framed);
    harness
        .click_named(id)
        .unwrap_or_else(|error| panic!("{error}"));
    harness.relayout();
}

fn find<'a>(w: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
    if w.test_id() == Some(id) {
        return Some(w);
    }
    w.children().into_iter().find_map(|c| find(c, id))
}

fn rects_named(w: &dyn Widget, name: &str, out: &mut Vec<Rect>) {
    if w.name() == name {
        out.push(w.get_rect());
    }
    for c in w.children() {
        rects_named(c, name, out);
    }
}

fn png(rgba: [u8; 4]) -> Vec<u8> {
    let pixels: Vec<u8> = (0..64).flat_map(|_| rgba).collect();
    ui::png::encode_rgba8(8, 8, &pixels).unwrap()
}

fn image_draws(framed: &Framed<TubeApp>) -> usize {
    use vello_encoding::DrawTag;
    Framed::harness(framed)
        .scene()
        .encoding()
        .draw_tags
        .iter()
        .filter(|tag| **tag == DrawTag::IMAGE)
        .count()
}

#[test]
fn the_rail_and_cards_navigate_and_back_returns() {
    // Case `rail_items_open_their_destinations`.
    {
        let mut framed = mount(TubeApp::new());
        assert_eq!(
            framed.route_for_test(),
            &Route::Home,
            "the app boots on Home"
        );
        click_control(&mut framed, "Trending");
        assert_eq!(framed.route_for_test(), &Route::Trending);
        click_control(&mut framed, "Queue");
        assert_eq!(framed.route_for_test(), &Route::Queue);
        click_control(&mut framed, "Settings");
        assert_eq!(framed.route_for_test(), &Route::Settings);
    }
    // Case `a_card_opens_its_video_and_escape_goes_back`.
    {
        let mut framed = mount(TubeApp::new());
        click_id(&mut framed, "video_card_vid_donut");
        assert_eq!(
            framed.route_for_test(),
            &Route::Watch("vid_donut".into()),
            "clicking a card opens its watch page"
        );
        Framed::harness_mut(&mut framed).key("Escape");
        Framed::harness_mut(&mut framed).relayout();
        assert_eq!(framed.route_for_test(), &Route::Home, "Escape goes back");
    }
    // Case `a_creator_tile_opens_the_creator`.
    {
        let mut framed = mount(TubeApp::new().with_route_shot(Route::Subscriptions));
        click_id(&mut framed, "creator_ch_kurz");
        assert_eq!(framed.route_for_test(), &Route::Channel("ch_kurz".into()));
    }
    // Case `a_browsed_page_shows_back_and_it_returns`.
    for route in [
        Route::Watch("vid_donut".into()),
        Route::Channel("ch_kurz".into()),
        Route::Playlist("pl_systems".into()),
    ] {
        let mut framed = mount(TubeApp::new().with_route_shot(route.clone()));
        click_control(&mut framed, "Back");
        assert_eq!(
            framed.route_for_test(),
            &Route::Home,
            "Back on {route:?} returns to the page it was opened from"
        );
    }
    // Case `the_watch_page_gear_opens_settings`.
    {
        let mut framed = mount(TubeApp::new().with_route_shot(Route::Watch("vid_donut".into())));
        click_control(&mut framed, "Playback settings");
        assert_eq!(framed.route_for_test(), &Route::Settings);
    }
}

#[test]
fn watch_and_settings_controls_change_state_through_real_clicks() {
    // Case `like_toggles_from_the_watch_toolbar`.
    {
        let mut framed = mount(TubeApp::new().with_route_shot(Route::Watch("vid_donut".into())));
        assert!(!framed.model_for_test().is_liked("vid_donut"));
        click_id(&mut framed, "watch_like");
        assert!(
            framed.model_for_test().is_liked("vid_donut"),
            "clicking Like likes the video"
        );
        click_id(&mut framed, "watch_like");
        assert!(!framed.model_for_test().is_liked("vid_donut"));
    }
    // Case `subscribe_toggles_from_the_creator_row`.
    {
        let mut framed = mount(TubeApp::new().with_route_shot(Route::Watch("vid_synth".into())));
        assert!(!framed.model_for_test().is_subscribed("ch_synth"));
        click_id(&mut framed, "subscribe_btn");
        assert!(framed.model_for_test().is_subscribed("ch_synth"));
    }
    // Case `a_settings_switch_flips_its_setting`.
    {
        let mut framed = mount(TubeApp::new().with_route_shot(Route::Settings));
        let before = framed.model_for_test().settings.show_comments;
        click_id(&mut framed, "switch_comments");
        assert_eq!(
            framed.model_for_test().settings.show_comments,
            !before,
            "the switch flips Show comments"
        );
    }
}

#[test]
fn pages_lay_out_without_overlap_at_every_size() {
    // Case `watch_page_parts_stack_in_reading_order`.
    for (viewport, scale) in [((1280, 800), 1.0), ((800, 600), 1.0), ((1280, 800), 2.0)] {
        let framed = mount_at(
            TubeApp::new().with_route_shot(Route::Watch("vid_rust_async".into())),
            viewport,
            Theme::dark().with_text_scale(scale),
        );
        let player = find(&*framed, "watch_video_surface")
            .expect("the player is laid out")
            .get_rect();
        assert!(player.width() > 0.0 && player.height() > 0.0);
        let creator = find(&*framed, "watch_creator")
            .expect("the creator row is laid out")
            .get_rect();
        assert!(
            creator.y0 >= player.y1,
            "the details sit under the player at {viewport:?} ×{scale}: {player:?} / {creator:?}"
        );
    }
    // Case `a_short_grid_keeps_the_card_size_of_a_full_one`.
    for viewport in [(1280, 800), (800, 600)] {
        let width_on = |route: Route| {
            let framed = mount_at(
                TubeApp::new().with_route_shot(route),
                viewport,
                Theme::dark(),
            );
            let mut cards = Vec::new();
            rects_named(&*framed, "MediaCard", &mut cards);
            cards
                .first()
                .map(Rect::width)
                .expect("the page shows cards")
        };
        let home = width_on(Route::Home);
        let channel = width_on(Route::Channel("ch_lowlevel".into()));
        assert!(
            (home - channel).abs() <= 1.0,
            "a creator's three videos are as wide as Home's at {viewport:?}: {home} vs {channel}"
        );
    }
    // Case `home_cards_never_overlap`.
    for (viewport, scale) in [((1280, 800), 1.0), ((560, 800), 1.0), ((1280, 800), 2.0)] {
        let framed = mount_at(
            TubeApp::new(),
            viewport,
            Theme::dark().with_text_scale(scale),
        );
        let mut cards = Vec::new();
        rects_named(&*framed, "MediaCard", &mut cards);
        assert!(cards.len() >= 4, "Home shows its cards at {viewport:?}");
        for (i, a) in cards.iter().enumerate() {
            for b in cards.iter().skip(i + 1) {
                let overlap = a.intersect(*b);
                assert!(
                    overlap.width() <= 0.5 || overlap.height() <= 0.5,
                    "cards overlap at {viewport:?} ×{scale}: {a:?} and {b:?}"
                );
            }
        }
    }
}

#[test]
fn fetched_images_reach_the_scene() {
    // Case `home_cards_paint_fetched_thumbnails`.
    {
        let thumb = "http://test.local/thumb.png";
        let bytes = png([20, 200, 60, 255]);
        let fetch: ui::Fetcher = Arc::new(move |_url: &str| Some(bytes.clone()));
        let store = ui::ImageStore::new(&fetch);
        let _ = store.get(thumb);
        store.block_until_idle(tilo_test_env::hang_bound());

        let mut model = tube::sample::model();
        for v in &mut model.videos {
            v.thumbnail_url = thumb.into();
        }
        let mut framed = mount(
            TubeApp::new()
                .with_model_for_test(model)
                .with_image_store(store),
        );
        Framed::harness_mut(&mut framed).relayout();
        assert!(
            image_draws(&framed) >= 4,
            "the decoded thumbnails paint on the Home cards"
        );
    }
    // Case `the_watch_page_paints_the_creator_avatar`.
    {
        let avatar = "http://test.local/avatar.png";
        let bytes = png([200, 60, 20, 255]);
        let fetch: ui::Fetcher = Arc::new(move |_url: &str| Some(bytes.clone()));
        let store = ui::ImageStore::new(&fetch);
        let _ = store.get(avatar);
        store.block_until_idle(tilo_test_env::hang_bound());

        let mut model = tube::sample::model();
        if let Some(channel) = model.channels.iter_mut().find(|c| c.id == "ch_blender") {
            channel.avatar_url = avatar.into();
        }
        model.route = Route::Watch("vid_donut".into());
        let mut framed = mount(
            TubeApp::new()
                .with_model_for_test(model)
                .with_image_store(store)
                .with_route_shot(Route::Watch("vid_donut".into())),
        );
        Framed::harness_mut(&mut framed).relayout();
        assert!(
            image_draws(&framed) >= 1,
            "the creator's decoded avatar paints, not its initials"
        );
    }
}

/// Leaf widgets that still ask for a layout re-measure.
fn measure_demands(w: &dyn Widget, path: &str, out: &mut Vec<String>) {
    let here = format!("{path}/{}", w.name());
    let children = w.children();
    if children.is_empty() {
        if w.layout_needs_measure() {
            out.push(here);
        }
    } else {
        for child in children {
            measure_demands(child, &here, out);
        }
    }
}

#[test]
fn laid_out_pages_do_not_demand_a_relayout_while_scrolling() {
    // A widget that reports itself dirty forever makes the host re-solve the
    // whole page on every scroll frame, which is the scroll-jank regression.
    for route in [
        Route::Home,
        Route::Subscriptions,
        Route::History,
        Route::Channel("ch_kurz".into()),
        Route::Playlist("pl_systems".into()),
    ] {
        let framed = mount(TubeApp::new().with_route_shot(route.clone()));
        let mut demands = Vec::new();
        measure_demands(&*framed, "", &mut demands);
        assert!(
            demands.is_empty(),
            "{route:?}: laid-out widgets still demand a measure: {demands:?}"
        );
    }
}
