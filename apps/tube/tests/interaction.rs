//! Click-driven interaction tests.
//!
//! These build the real widget tree, run the real five-pass layout, resolve
//! affordances by their laid-out rect, and inject real winit pointer events
//! through `Widget::handle_event` — the exact production path (`ActionBus` drain →
//! `TubeApp::apply`). No app-level shortcuts: a passing test means a real click
//! at the real coordinates did the real thing. Controls that change the shared
//! library go through the framework's door and are driven through the
//! framework app in `resource.rs`.

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
use ui::widget::{MediaSize, Widget};
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
    if w.test_id() == Some(id) || (id == "video_grid" && w.name() == "MediaGrid") {
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

/// Paint the actual persistent player mounted by the watch page, including
/// its shared controls. Window and poster material caches are outside this
/// measurement, so only the timeline's additional segment fills differ.
fn player_draws(app: TubeApp) -> usize {
    fn paint_player(widget: &mut dyn Widget, scene: &mut vello::Scene) -> bool {
        if widget.name() == "PlayerHost" {
            widget.draw(
                scene,
                vello::kurbo::Affine::IDENTITY,
                &mut ui::TextRenderer::new(),
                &Theme::dark(),
                &ui::presence::PresenceCtx::default(),
            );
            return true;
        }
        widget
            .children_mut()
            .into_iter()
            .any(|child| paint_player(child, scene))
    }
    let mut framed = mount(app);
    let player = find(&*framed, tube::widgets::player_host::PLAYER_HOST_ID)
        .expect("the production watch page mounts its player")
        .get_rect();
    assert!(player.width() > 0.0 && player.height() > 0.0);
    Framed::harness_mut(&mut framed).send(&winit::event::WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(player.center().x, player.center().y),
    });
    let mut scene = vello::Scene::new();
    assert!(paint_player(&mut *framed, &mut scene));
    scene.encoding().draw_tags.len()
}

struct Harness {
    app: Framed<TubeApp>,
}
impl Harness {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "test viewport dimensions are positive integral values below 2000"
    )]
    fn new<T: Into<f64> + Copy>(app: TubeApp, viewport: (T, T)) -> Self {
        Self {
            app: mount_at(
                app,
                (viewport.0.into() as u32, viewport.1.into() as u32),
                Theme::dark(),
            ),
        }
    }
    fn rect_by_id(&self, id: &str) -> Option<Rect> {
        find(&*self.app, id).map(Widget::get_rect)
    }
    fn has_id(&self, id: &str) -> bool {
        self.rect_by_id(if id == "back_btn" {
            "nav-shell-back"
        } else {
            id
        })
        .is_some()
    }
    fn click_id(&mut self, id: &str) {
        if id == "back_btn" {
            click_control(&mut self.app, "Back");
        } else {
            click_id(&mut self.app, id);
        }
    }
    fn child_rects(&self, id: &str) -> Vec<Rect> {
        let root = find(&*self.app, id).expect("the grid is retained");
        let mut rects = Vec::new();
        rects_named(root, "MediaCard", &mut rects);
        rects
    }
    fn wheel_at(&mut self, at: vello::kurbo::Point, delta: f64) {
        let frame = Framed::harness_mut(&mut self.app);
        frame.send(&winit::event::WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(at.x, at.y),
        });
        frame.scroll(0.0, delta);
        frame.relayout();
    }
    fn image_draw_count(&mut self) -> usize {
        Framed::harness_mut(&mut self.app).relayout();
        image_draws(&self.app)
    }
    fn layout_needs_measure(&self) -> bool {
        Widget::layout_needs_measure(&*self.app)
    }
}

/// A live Tube window, wide enough for the docked rail: a capture's controls
/// start no effects.
fn live_app() -> ui::framework::testing::TestApp<tube::TubeShell> {
    let mut app = ui::framework::testing::TestApp::<tube::TubeShell>::new().unwrap();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a fixed viewport"
    )]
    app.harness().resize(VIEWPORT).unwrap();
    app.harness().relayout();
    app
}

/// A hermetic catalogue in the actual live host, retaining typed effects.
fn live_catalogue() -> ui::framework::testing::TestApp<tube::TubeShell> {
    let mut app = live_app();
    app.with_surface::<TubeApp, _>(|surface| {
        surface.stage_model_for_test(tube::sample::model());
    })
    .expect("the canonical Tube surface is mounted");
    app.settle();
    app
}

fn click(app: &mut ui::framework::testing::TestApp<tube::TubeShell>, id: &str) {
    if let Some(label) = id.strip_prefix("nav_").filter(|name| *name != "toggle") {
        let expected = format!("{}{}", label.get(..1).unwrap().to_uppercase(), &label[1..]);
        let centre = app
            .harness()
            .controls()
            .into_iter()
            .find(|control| control.name.as_deref() == Some(expected.as_str()))
            .and_then(|control| control.bounds)
            .expect("the navigation item is actionable")
            .center();
        app.harness().click_at(centre.x, centre.y);
    } else {
        app.harness()
            .click_named(if id == "nav_toggle" {
                "nav-shell-sidebar"
            } else {
                id
            })
            .unwrap();
    }
    app.settle();
    app.harness().relayout();
}

fn route(app: &mut ui::framework::testing::TestApp<tube::TubeShell>) -> Route {
    app.with_surface::<TubeApp, _>(|tube| tube.route_for_test().clone())
        .expect("the window shows Tube")
}

#[test]
fn the_rail_and_cards_navigate_and_back_returns() {
    // Case `rail_items_open_their_destinations`.
    {
        // The app boots on Home (never into a fake/placeholder video). Click the
        // "Trending" rail item and prove the route follows: the rail item is
        // bound to the show-screen effect the window's host answers.
        let mut app = live_app();
        assert_eq!(route(&mut app), Route::Home, "the app boots on Home");

        click(&mut app, "nav_trending");
        assert_eq!(
            route(&mut app),
            Route::Trending,
            "clicking the Trending tab navigates to Trending"
        );
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
    for destination in [
        Route::Watch("vid_donut".into()),
        Route::Channel("ch_kurz".into()),
        Route::Playlist("pl_systems".into()),
    ] {
        let mut app = live_catalogue();
        let effect = match &destination {
            Route::Watch(video) => tube::actions::TubeEffect::OpenVideo {
                video: video.clone(),
            },
            Route::Channel(creator) => tube::actions::TubeEffect::OpenCreator {
                creator: creator.clone(),
            },
            Route::Playlist(playlist) => tube::actions::TubeEffect::OpenPlaylist {
                playlist: playlist.clone(),
            },
            _ => unreachable!("the cases are browsable content"),
        };
        app.effect(&effect).unwrap();
        app.settle();
        let centre = app
            .harness()
            .controls()
            .into_iter()
            .find(|control| control.name.as_deref() == Some("Back"))
            .and_then(|control| control.bounds)
            .expect("browsed content exposes Back")
            .center();
        app.harness().click_at(centre.x, centre.y);
        app.settle();
        assert_eq!(
            route(&mut app),
            Route::Home,
            "Back returns to the page the content was opened from"
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
fn another_rail_destination_navigates() {
    {
        let mut app = live_app();
        // And a second tab, to prove it isn't a one-off.
        click(&mut app, "nav_settings");
        assert_eq!(route(&mut app), Route::Settings);
    }
    // Case `shared_queue_is_a_real_navigation_destination`.
    {
        // The queue has both a visible rail destination and a canonical navigation
        // action, rather than being an agent-only collection hidden behind Ctrl+K.
        let mut app = live_app();
        click(&mut app, "nav_queue");
        assert_eq!(route(&mut app), Route::Queue);
        assert!(
            app.harness().targets().contains_key("queue_page"),
            "the shared queue screen is laid out"
        );
    }
}

/// A real viewport refresh between motion and press keeps the rail's hit state.
#[test]
fn rail_click_survives_a_layout_refresh_between_motion_and_press() {
    let mut app = live_app();
    app.with_surface::<TubeApp, _>(|surface| {
        surface.stage_model_for_test(tube::model::TubeModel::default());
    })
    .expect("the actual live surface is mounted");
    app.settle();
    let queue = app
        .harness()
        .controls()
        .into_iter()
        .find(|control| control.name.as_deref() == Some("Queue"))
        .and_then(|control| control.bounds)
        .expect("the Queue rail destination is laid out");
    assert!(queue.width() > 0.0 && queue.height() > 0.0);
    let centre = queue.center();
    app.harness().send(&winit::event::WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(centre.x, centre.y),
    });
    // An actual viewport change forces the host's composition/layout passes,
    // unlike a fixture-only model flag that cached layout may never observe.
    app.harness().resize((VIEWPORT.0 + 1, VIEWPORT.1)).unwrap();
    assert!(
        app.harness()
            .root_widget()
            .get_rect()
            .width()
            .total_cmp(&f64::from(VIEWPORT.0 + 1))
            .is_eq(),
        "the host actually laid out the intermediate viewport"
    );
    app.harness().resize(VIEWPORT).unwrap();
    let refreshed = app
        .harness()
        .controls()
        .into_iter()
        .find(|control| control.name.as_deref() == Some("Queue"))
        .and_then(|control| control.bounds)
        .expect("Queue remains laid out after the refresh");
    assert_eq!(refreshed, queue, "the destination did not move");
    assert_eq!(route(&mut app), Route::Home, "refresh does not navigate");
    for state in [
        winit::event::ElementState::Pressed,
        winit::event::ElementState::Released,
    ] {
        app.harness().send(&winit::event::WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state,
            button: winit::event::MouseButton::Left,
        });
    }
    app.settle();
    assert_eq!(route(&mut app), Route::Queue);
    assert!(
        app.harness().targets().contains_key("queue_page"),
        "the physical click opens the laid-out shared queue"
    );
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
            let mut grids = Vec::new();
            rects_named(&*framed, "MediaGrid", &mut grids);
            let width = cards
                .first()
                .map(Rect::width)
                .expect("the page shows cards");
            let extent = MediaSize::Regular.extent(&Theme::dark());
            (width, grids, extent)
        };
        let (home, home_grids, home_extent) = width_on(Route::Home);
        let (channel, channel_grids, channel_extent) =
            width_on(Route::Channel("ch_lowlevel".into()));
        assert!(
            (home - channel).abs() <= 1.0,
            "a creator's three videos are as wide as Home's at {viewport:?}: {home} vs {channel}; Home grids={home_grids:?}, extent={home_extent}; creator grids={channel_grids:?}, extent={channel_extent}"
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
    // Case `selected_nav_tab_is_visible_and_addressable_after_switch`.
    {
        // After navigating, the selected tab must still resolve (it's re-rendered
        // with the accent fill, same test_id) — i.e. selection is real, not a
        // rendering dead-end.
        let mut app = live_app();
        click(&mut app, "nav_home");
        assert_eq!(route(&mut app), Route::Home);
        assert!(
            app.harness().a11y_node_named("Home").is_some(),
            "the now-selected Home tab is still in the laid-out tree"
        );
    }
    // Case `hamburger_toggles_the_overlay_rail_through_a_real_click`.
    {
        // At a narrow width the rail is a collapsed overlay flap. The top-bar
        // hamburger must summon and dismiss it — via a real click through the menu
        // button's `handle_event`, not app.apply.
        let mut app = live_app();
        app.harness().resize((560, 800)).unwrap();
        app.harness().relayout();
        let open = |app: &mut ui::framework::testing::TestApp<tube::TubeShell>| {
            app.with_surface::<TubeApp, _>(|tube| tube.sidebar_open_for_test())
                .expect("the window shows Tube")
        };
        assert!(
            !open(&mut app),
            "a narrow window starts with the rail collapsed"
        );

        click(&mut app, "nav_toggle");
        assert!(open(&mut app), "the hamburger summons the overlay rail");

        click(&mut app, "nav_toggle");
        assert!(!open(&mut app), "a second click dismisses it");
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn search_inputs_are_present_and_navigate() {
    for (viewport, scale) in [((1280, 800), 1.0), ((420, 800), 1.5)] {
        let mut app =
            ui::framework::testing::TestApp::<tube::TubeShell>::capture_at(None, viewport).unwrap();
        app.harness()
            .set_theme(Theme::dark().with_text_scale(scale));
        let search = app
            .harness()
            .controls()
            .into_iter()
            .find(|control| {
                control.name.as_deref()
                    == Some(ui::i18n::I18nManager::lookup_plain("standalone-search-title").as_str())
            })
            .expect("system search is present even with an empty query");
        let bounds = search.bounds.expect("search has measured geometry");
        assert!(
            bounds.width() > 10.0 && bounds.height() > 10.0,
            "search is reachable: {bounds:?}"
        );
        let centre = bounds.center();
        app.harness().click_at(centre.x, centre.y);
        app.settle();
        let (_, focused) = app
            .harness()
            .a11y_focus()
            .expect("pointer activation focuses the platform search input");
        assert_eq!(focused.role(), ui::accesskit::Role::TextInput);
        assert_eq!(focused.label(), Some("Search"));
        assert_eq!(focused.description(), Some("Search actions and results"));
        assert_eq!(
            focused.value(),
            Some(""),
            "the actual platform palette starts with an empty query"
        );
        app.harness().type_text("compiler");
        app.settle();
        let (_, focused) = app
            .harness()
            .a11y_focus()
            .expect("the platform query retains input focus");
        assert_eq!(
            focused.value(),
            Some("compiler"),
            "typing reaches the actual shared query field"
        );
        app.harness().key("Escape");
        app.settle();
        assert!(
            app.harness()
                .controls()
                .iter()
                .all(|control| !(control.role == ui::accesskit::Role::TextInput
                    && control.name.as_deref() == Some("Search"))),
            "Escape closes the actual platform query field"
        );
        assert!(
            app.harness()
                .controls()
                .iter()
                .any(|control| control.role == ui::accesskit::Role::Button
                    && control.name.as_deref() == search.name.as_deref()),
            "Escape restores the platform Search button"
        );
        assert_eq!(
            route(&mut app),
            Route::Home,
            "search does not replace the app's page"
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

        let content_left = find(&*h.app, "tube_rail")
            .expect("the rail is laid out")
            .get_rect()
            .x1; // docked rail width (86)
        let viewport_h = f64::from(VIEWPORT.1);
        // The content lane: the split's content pane minus the reserved scrollbar
        // gutter on the right.
        let lane_left = content_left;
        let lane_right =
            f64::from(VIEWPORT.0) - f64::from(Theme::dark().tokens.metrics.scrollbar_gutter);

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
            h.has_id("tube-dislikes"),
            "the watch page lays out the shared Return YouTube Dislike count"
        );
        let mut model = watch_app().model_for_test().clone();
        let segments = model
            .video(WATCH_ID)
            .unwrap()
            .sponsor
            .iter()
            .filter(|segment| model.settings.category_enabled(segment.category))
            .count();
        assert!(
            segments > 0,
            "the actual video has enabled sponsor segments"
        );
        let with_marks = player_draws(watch_app());
        model.settings.sponsorblock_enabled = false;
        let without_marks = player_draws(TubeApp::new().with_model_for_test(model));
        assert!(
            with_marks >= without_marks + segments,
            "the shared player paints each SponsorBlock segment on its timeline: {with_marks} vs {without_marks}, {segments} segments"
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

fn share_copies_a_link_through_a_real_click() {
    let mut app = live_catalogue();
    app.effect(&tube::actions::TubeEffect::OpenVideo {
        video: WATCH_ID.into(),
    })
    .unwrap();
    app.settle();
    assert!(
        app.with_surface::<TubeApp, _>(|viewer| !viewer.model_for_test().is_liked(WATCH_ID))
            .unwrap(),
        "not liked to start"
    );
    // with_surface runs another host tick and relayout, which can start new
    // presentation work. Settle after that final inspection so click_named's
    // target rectangle stays valid through the real hover/press/release path.
    // settle() above flushes effects but does not await the NavShell entrance.
    assert!(
        app.harness()
            .settle_for_capture(ui::time::Duration::from_secs(5)),
        "the watch page must finish its entrance before clicking Share"
    );
    // Observe the real effect through the published accessibility notice.
    // Under a busy shard, the click can enqueue work beyond the next host
    // tick, so inspect the model only after that effect has appeared.
    app.harness().click_named("watch_copy_link").unwrap();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(5), |h| {
                h.a11y_nodes().iter().any(|(_, node)| {
                    node.label() == Some("Notifications")
                        && node.value().is_some_and(|value| {
                            value.contains("Link copied") && value.contains(WATCH_ID)
                        })
                })
            }),
        "the real click must publish the copied-link notice before inspecting the model"
    );
    app.with_surface::<TubeApp, _>(|viewer| {
        assert!(
            viewer
                .model_for_test()
                .notices
                .iter()
                .any(|notice| { notice.contains("Link copied") && notice.contains(WATCH_ID) }),
            "the real pointer sequence must submit Share on the settled watch page"
        );
    })
    .unwrap();

    app.with_surface::<TubeApp, _>(|viewer| {
        assert!(
            !viewer.model_for_test().is_liked(WATCH_ID),
            "Share must not like the video"
        );
        assert!(
            viewer
                .model_for_test()
                .notices
                .iter()
                .any(|notice| notice.contains("Link copied") && notice.contains(WATCH_ID)),
            "Share records a link copied notice for this video"
        );
    })
    .unwrap();
    assert!(
        app.harness().a11y_nodes().iter().any(|(_, node)| {
            node.label() == Some("Notifications")
                && node.live() == Some(ui::accesskit::Live::Assertive)
                && node
                    .value()
                    .is_some_and(|value| value.contains("Link copied") && value.contains(WATCH_ID))
        }),
        "the share confirmation is actually published by the toast strip"
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

/// The shared settings switch keeps its named, stateful semantic surface;
/// activation crosses the real typed effect boundary and pointer hit-testing
/// stays within the switch rather than its whole form row.
#[test]
fn shared_settings_toggle_exposes_state_and_applies_the_preference() {
    let mut app = live_app();
    click(&mut app, "nav_settings");
    let name = "Autoplay next video";
    let id = app
        .harness()
        .a11y_node_named(name)
        .expect("named shared switch");
    let node = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(key, _)| *key == id)
        .unwrap()
        .1;
    assert_eq!(node.role(), ui::accesskit::Role::Switch);
    let before = node.toggled();
    let bounds = node.bounds().unwrap();
    app.harness()
        .perform_a11y_action(id, &ui::a11y::Action::Focus)
        .unwrap();
    assert_eq!(
        app.harness().focused_access_node().unwrap().name.as_deref(),
        Some(name)
    );
    app.harness()
        .perform_a11y_action(id, &ui::a11y::Action::ToggleCheck)
        .unwrap();
    app.settle();
    let id = app.harness().a11y_node_named(name).unwrap();
    assert_ne!(
        app.harness()
            .a11y_nodes()
            .into_iter()
            .find(|(key, _)| *key == id)
            .unwrap()
            .1
            .toggled(),
        before,
        "actual hosted effect changes the preference"
    );
    let rect = app
        .with_surface::<TubeApp, _>(|surface| {
            let toggle = find(surface, "switch_autoplay").expect("shared switch identity");
            toggle.get_rect()
        })
        .unwrap();
    assert!(rect.area() > 0.0 && bounds.width() > 0.0);
    app.harness().click_at(rect.x0 - 1.0, rect.center().y);
    app.settle();
    let after_outside = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(name) && node.role() == ui::accesskit::Role::Switch)
        .unwrap()
        .1
        .toggled();
    assert_ne!(
        after_outside, before,
        "outside press does not undo the semantic change"
    );
    app.harness().click_at(rect.center().x, rect.center().y);
    app.settle();
    let after_inside = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(name) && node.role() == ui::accesskit::Role::Switch)
        .unwrap()
        .1
        .toggled();
    assert_eq!(
        after_inside, before,
        "inside press crosses the same typed preference route"
    );
}

fn draw_cards(
    widget: &mut dyn Widget,
    scene: &mut vello::Scene,
    text: &mut ui::text_render::TextRenderer,
    theme: &Theme,
    presence: &ui::presence::PresenceCtx,
) -> usize {
    if widget.name() == "MediaCard" {
        widget.draw(scene, vello::kurbo::Affine::IDENTITY, text, theme, presence);
        return 1;
    }
    widget
        .children_mut()
        .into_iter()
        .map(|child| draw_cards(child, scene, text, theme, presence))
        .sum()
}

/// Missing fetched media still paints the actual shared card and banner
/// placeholders, without submitting any decoded IMAGE draw.
#[test]
fn unfetched_media_paints_shared_placeholders() {
    for route in [Route::Home, Route::Channel("ch_kurz".into())] {
        let mut framed = mount(TubeApp::new().with_route_shot(route));
        let theme = Framed::harness(&framed).theme().clone();
        let mut scene = vello::Scene::new();
        let mut text = ui::text_render::TextRenderer::new();
        let presence = ui::presence::PresenceCtx::empty();
        let observed = draw_cards(&mut *framed, &mut scene, &mut text, &theme, &presence);
        assert!(
            observed > 0,
            "the mounted catalogue supplies actual shared cards"
        );
        assert_eq!(
            scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|tag| **tag == vello_encoding::DrawTag::IMAGE)
                .count(),
            0,
            "unfetched card art submits no decoded image; window material caches are separate"
        );
        assert!(
            !scene.encoding().draw_tags.is_empty(),
            "actual fallback cards paint"
        );
    }
}
