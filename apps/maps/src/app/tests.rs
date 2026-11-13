use super::*;
use crate::model::Category;
use automerge::AutoCommit;

#[test]
fn effects_answer_from_the_window_state() {
    use crate::resource::MapsEffect;
    use ui::framework::EffectStatus;

    {
        let mut app = MapsApp::offline();
        assert!(
            app.effect_statuses().contains(&EffectStatus::unavailable(
                MapsEffect::LOCATE,
                "location_service_offline",
                "Location is unavailable right now",
            )),
            "a capture or test map has no location service"
        );
        app.online = true;
        assert!(app.effect_statuses().is_empty());
        app.locating = true;
        app.location_rx = Some(host_location(LocationOutcome::Unavailable(
            "permission denied".into(),
        )));
        assert!(app.effect_statuses().contains(&EffectStatus::unavailable(
            MapsEffect::LOCATE,
            "request_in_progress",
            "A location request is already in progress",
        )));
        app.drain_location();
        assert_eq!(app.model.status, "Location unavailable — permission denied");
        assert!(app.effect_statuses().is_empty(), "locate is offered again");
    }

    {
        let mut app = MapsApp::offline();
        let recents_before = app.model.recents.clone();
        app.model.query = "Marienplatz".to_string();
        app.run_search(false);
        assert!(!app.model.results.is_empty());
        assert_eq!(app.model.recents, recents_before);
        app.run_search(true);
        assert_eq!(
            app.model.recents.first().map(String::as_str),
            Some("Marienplatz")
        );
    }

    {
        let mut a = app_at(1_280.0, 800.0);
        for index in 0..20 {
            let mut place = a.model.places[0].clone();
            place.id = format!("saved-{index}");
            place.saved = true;
            a.model.places.push(place);
        }
        let rows: Vec<_> = a
            .published_rows()
            .into_iter()
            .map(LiveRowParts::of)
            .collect();
        let mut ids: Vec<_> = rows.iter().map(|row| row.id.clone()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), rows.len(), "row ids are unique");
        let result = rows
            .iter()
            .find(|row| row.id.starts_with("maps.search-result:"))
            .expect("a ranked result row");
        assert_eq!(result.actions, ["open-place", "directions"]);
        assert!(
            rows.iter()
                .filter(|row| row.id.starts_with("maps.place:"))
                .count()
                <= 12,
            "saved places are a bounded page"
        );
        let omitted = a
            .published_row("maps.place:saved-19")
            .map(LiveRowParts::of)
            .expect("an omitted saved place resolves");
        assert_eq!(omitted.actions, ["open-place", "directions"]);
    }
}

struct LiveRowParts {
    id: String,
    actions: Vec<String>,
}

impl LiveRowParts {
    fn of(row: ui::framework::LiveRow) -> Self {
        let resource = row.into_resource();
        Self {
            id: resource.id.as_str().to_owned(),
            actions: resource
                .actions
                .iter()
                .map(|action| action.id.clone())
                .collect(),
        }
    }
}

#[test]
fn swapping_transit_endpoints_replans_transit_not_a_road_route() {
    let mut app = app_at(1280.0, 800.0);
    app.model.mode = TravelMode::Transit;
    app.model.route = None;
    let origin = app.model.origin.expect("transit origin");
    let dest = app.model.dest.expect("transit destination");
    let origin_name = app.model.origin_name.clone();
    let dest_name = app.model.dest_name.clone();
    assert!(
        !app.model.transit.is_empty(),
        "a journey is shown before swapping"
    );
    let pending = app.pending;

    app.apply_endpoint_effect(EndpointEffect::Swap);

    assert_eq!(app.model.origin, Some(dest));
    assert_eq!(app.model.dest, Some(origin));
    assert_eq!(app.model.origin_name, dest_name);
    assert_eq!(app.model.dest_name, origin_name);
    assert_eq!(app.model.mode, TravelMode::Transit);
    assert!(
        app.model.route.is_none(),
        "transit must not fabricate a road route"
    );
    assert!(
        app.model.transit.is_empty(),
        "the old direction's departures are stale"
    );
    assert_eq!(app.pending, pending + 1);
    assert_eq!(app.model.status, "Finding departures…");
}

#[test]
fn scaled_host_keeps_the_map_full_bleed_and_frames_the_plan() {
    use ui::framework::testing::TestApp;
    for scenario in ["directions", "transit"] {
        let mut hosted = TestApp::<crate::MapsShell>::capture_at(Some(scenario), (1440, 810))
            .expect("hosted map");
        for scale in [1.0, 1.5, 2.0] {
            hosted
                .harness()
                .set_theme(Theme::dark().with_ui_scale(scale));
            let app = hosted.surface_mut::<MapWindow>().app();
            assert_eq!(
                app.rect,
                Rect::new(0.0, 0.0, 1440.0, 810.0),
                "History must not allocate a toolbar above the immersive surface"
            );
            let points = app.model.route.as_ref().map_or_else(
                || app.selected_journey_geometry(),
                |route| route.geometry.clone(),
            );
            assert!(!points.is_empty());
            let area = app.visible_map_area();
            for point in points {
                let projected = app.projection().project(point);
                assert!(
                    area.contains(projected),
                    "{scenario} at {scale}×: {projected:?} is outside {area:?}"
                );
            }
        }
        hosted.harness().close_components();
    }
}

#[test]
fn scaled_marker_hit_regions_follow_the_painted_heads() {
    let theme = Theme::dark().with_ui_scale(1.5);
    let mut app = app_at(1440.0, 810.0);
    app.ui_scale = f64::from(theme.ui_scale);
    let place = app.model.places[0].clone();
    app.model.places = vec![place.clone()];
    app.model.selected = Some(place.id.clone());
    let anchor = app.projection().project(place.pos);
    let (radius, height) = canvas::pin_size(true, app.ui_scale);
    let head = Point::new(anchor.x + radius * 0.8, anchor.y - height);
    assert_eq!(app.marker_at(head), Some(place.id));

    app.pois = vec![demo_poi(
        "scaled-poi",
        app.model.center.lat,
        app.model.center.lon,
    )];
    app.model.zoom = 17.0;
    let anchor = app.projection().project(app.pois[0].pos);
    let radius = canvas::poi_radius(true, false) * app.ui_scale;
    assert_eq!(
        app.poi_at(Point::new(anchor.x + radius, anchor.y)),
        Some("scaled-poi".into())
    );
}

#[test]
fn clipped_departures_are_not_pointer_or_accessibility_targets() {
    use ui::framework::testing::TestApp;
    let mut hosted =
        TestApp::<crate::MapsShell>::capture_at(Some("transit"), (1440, 810)).expect("transit map");
    hosted.harness().set_theme(Theme::dark().with_ui_scale(1.5));
    {
        let mut app = hosted.surface_mut::<MapWindow>().app();
        assert!(
            !app.click_targets
                .iter()
                .any(|(_, hit)| matches!(hit, Hit::Journey(1))),
            "the next departure is below the whole-row clip"
        );
        assert!(
            !app.accessibility_targets()
                .iter()
                .any(|target| target.target == "maps.journey:1")
        );
        app.scroll_panel(-1.0);
    }
    hosted.harness().relayout();
    let app = hosted.surface_mut::<MapWindow>().app();
    assert!(
        app.click_targets
            .iter()
            .any(|(_, hit)| matches!(hit, Hit::Journey(1))),
        "scrolling reveals the next departure as an actionable whole row"
    );
    let panel = app.panel_rect();
    for target in app
        .accessibility_targets()
        .iter()
        .filter(|target| target.target.starts_with("maps.journey:"))
    {
        assert!(target.rect.y0 >= panel.y0 && target.rect.y1 <= panel.y1);
    }
    drop(app);
    hosted.harness().close_components();
}

#[test]
fn zoom_buttons_keep_the_unobscured_map_center_stationary() {
    let theme = Theme::dark().with_ui_scale(1.5);
    let mut app = app_at(1440.0, 810.0);
    app.spacing = theme.tokens.spacing;
    app.metrics = theme.tokens.metrics;
    app.nav_card_size = navigation_card_size(&theme);
    app.map_controls.set_metrics(ColumnMetrics::of(&theme));
    let anchor = app.visible_map_area().center();
    let geographic = app.projection().unproject(anchor);
    for action in [MapControlAction::ZoomIn, MapControlAction::ZoomOut] {
        app.dispatch_map_control(action);
        assert!(
            app.projection().project(geographic).distance(anchor) < 0.01,
            "zoom must center on the visible map, not beneath the panel"
        );
    }
}

#[test]
fn road_mode_changes_keep_the_new_estimate_not_the_old_accepted_duration() {
    use ui::framework::testing::TestApp;
    let mut hosted =
        TestApp::<crate::MapsShell>::capture_at(Some("directions"), (1440, 810)).expect("road map");
    hosted
        .surface_mut::<MapWindow>()
        .app()
        .set_mode(TravelMode::Bike);
    hosted.harness().relayout();
    assert_eq!(hosted.state().mode, TravelMode::Bike);
    assert_eq!(
        hosted.state().route().unwrap().mode,
        TravelMode::Car,
        "a mode change does not destroy the previously accepted plan"
    );
    assert_eq!(
        hosted
            .surface_mut::<MapWindow>()
            .app()
            .model
            .route
            .as_ref()
            .unwrap()
            .mode,
        TravelMode::Bike,
        "the pending route summary must use the chosen mode"
    );
    hosted.invoke(Workspace::CLEAR_ROUTE.invoke()).unwrap();
    hosted.harness().relayout();
    assert!(
        hosted
            .surface_mut::<MapWindow>()
            .app()
            .model
            .route
            .is_none(),
        "clearing the route must also discard its local estimate"
    );
    hosted.harness().close_components();
}

fn app_at(w: f64, h: f64) -> MapsApp {
    let mut a = MapsApp::offline();
    a.rect = Rect::new(0.0, 0.0, w, h);
    a.map_controls.set_viewport(a.rect);
    a
}

fn tap(a: &mut MapsApp, hit: &Hit) {
    a.dispatch(hit);
}

fn tap_offline(a: &mut MapsApp, action: OfflineAction) {
    a.dispatch_offline(action);
}

fn tap_control(a: &mut MapsApp, action: MapControlAction) {
    a.dispatch_map_control(action);
}

#[test]
fn pan_and_zoom_follow_pointer_wheel_and_keys() {
    {
        let mut a = app_at(1280.0, 800.0);
        let before = a.model.center;
        a.pan_by(120.0, 0.0);
        assert!((a.model.center.lon - before.lon).abs() > 1e-6);
    }

    {
        let mut a = app_at(1280.0, 800.0);
        for _ in 0..50 {
            a.zoom_by(1.0, a.view_center());
        }
        assert!(a.model.zoom <= 19.0);
        for _ in 0..80 {
            a.zoom_by(-1.0, a.view_center());
        }
        assert!(a.model.zoom >= 3.0);
    }

    {
        let mut a = app_at(1280.0, 720.0);
        let before = a.model.zoom;
        move_to(&mut a, 1100.0, 360.0);
        let handled = feed(&mut a, &ui::synthetic_input::mouse_wheel(0.0, 1.0));
        assert!(handled, "wheel over the map must be handled");
        assert!(a.model.zoom > before, "scroll up should zoom in");
    }

    {
        let mut a = app_at(1280.0, 720.0);
        a.model.places.clear();
        let before = a.model.center;
        move_to(&mut a, 1000.0, 360.0);
        assert!(
            press_left(&mut a),
            "pressing open map begins a pan and is handled"
        );
        let moved_handled = move_to(&mut a, 1040.0, 372.0);
        assert!(
            (a.model.center.lon - before.lon).abs() > 1e-6
                || (a.model.center.lat - before.lat).abs() > 1e-6,
            "drag should move the map centre"
        );
        assert!(
            moved_handled,
            "a drag-move must be handled so the map repaints"
        );
    }

    {
        let mut a = app_at(1280.0, 720.0);
        assert!(a.panel_visible(), "docked panel is visible");
        a.content_h = 2000.0;
        a.content_view_h = 400.0;
        let zoom_before = a.model.zoom;
        let scroll_before = a.scroll;
        let pr = a.panel_rect();
        move_to(&mut a, pr.center().x, pr.y0 + 120.0);
        let handled = feed(&mut a, &ui::synthetic_input::mouse_wheel(0.0, -1.0));
        assert!(handled, "wheel over the panel must be handled");
        assert!(a.scroll > scroll_before, "panel content should scroll");
        assert_eq!(
            a.model.zoom, zoom_before,
            "scrolling the panel must not zoom the map"
        );
    }

    {
        let mut a = app_at(1280.0, 720.0);
        let z0 = a.model.zoom;
        key_char(&mut a, "+");
        assert!(a.model.zoom > z0, "'+' zooms in");
        let z1 = a.model.zoom;
        key_char(&mut a, "-");
        assert!(a.model.zoom < z1, "'-' zooms out");
    }

    {
        let mut a = app_at(1280.0, 720.0);
        let before = a.model.center;
        key_named(&mut a, NamedKey::ArrowRight);
        assert!(
            (a.model.center.lon - before.lon).abs() > 1e-9,
            "ArrowRight pans east"
        );
        let mid = a.model.center;
        key_named(&mut a, NamedKey::ArrowDown);
        assert!(
            (a.model.center.lat - mid.lat).abs() > 1e-9,
            "ArrowDown pans south"
        );
    }
}

#[test]
fn zoom_scroll_and_search_typing_stay_under_frame_budget() {
    {
        use ui::profiling::{PerfScenario, PerfStep, run_scenario_on};
        let mut app = MapsApp::offline();
        app.stage_pois();
        app.seed_perf_tiles(1280.0, 800.0);
        let scenario = PerfScenario {
            name: "maps-zoom-scroll".into(),
            viewport: (1280, 800),
            warmup: 8,
            steps: vec![PerfStep::Scroll {
                dy: -0.4,
                count: 70,
            }],
        };
        let report = run_scenario_on(ui::State::new(), Box::new(app), &scenario);
        eprintln!("{report}");
        if !cfg!(debug_assertions) {
            assert!(
                report.total.p95_us < ui::profiling::FRAME_BUDGET_US,
                "zoom p95 {}us over the {}us budget",
                report.total.p95_us,
                ui::profiling::FRAME_BUDGET_US
            );
        }
    }

    {
        use ui::profiling::{PerfScenario, PerfStep, run_scenario_on};
        let mut app = MapsApp::offline();
        app.seed_perf_tiles(1280.0, 800.0);
        app.stage_edit_origin();
        let scenario = PerfScenario {
            name: "maps-endpoint-typing".into(),
            viewport: (1280, 800),
            warmup: 8,
            steps: vec![PerfStep::Type("Marienplatz coffee".into())],
        };
        let report = run_scenario_on(ui::State::new(), Box::new(app), &scenario);
        eprintln!("{report}");
        if !cfg!(debug_assertions) {
            assert!(
                report.total.p95_us < ui::profiling::FRAME_BUDGET_US,
                "typing p95 {}us over the {}us budget",
                report.total.p95_us,
                ui::profiling::FRAME_BUDGET_US
            );
        }
    }
}

#[test]
fn local_view_state_never_fabricates_or_overwrites_shared_state() {
    {
        let mut app = app_at(1280.0, 800.0);
        app.pan_by(96.0, 0.0);
        let center = app.model.center;
        let mut peer = crate::workspace::Workspace {
            places: app.model.places.clone().into(),
            ..crate::workspace::Workspace::default()
        };
        let id = peer.places[0].id.clone();
        peer.places[0].saved = true;
        app.adopt_resource_state(&peer);
        assert!(app.model.place(&id).is_some_and(|place| place.saved));
        assert_eq!(app.model.center, center, "the camera stays this window's");
    }

    {
        let mut a = app_at(1280.0, 800.0);
        a.endpoints.begin_edit(Endpoint::Origin);
        a.endpoints.sync_queries("Local draft", "");
        let draft = (a.model.origin_query.clone(), a.model.dest_query.clone());
        let presented = crate::workspace::Workspace {
            places: a.model.places.clone().into(),
            ..crate::workspace::Workspace::default()
        };
        a.adopt_resource_state(&presented);
        assert_eq!(
            (a.model.origin_query.clone(), a.model.dest_query.clone()),
            draft,
            "the draft stays this window's"
        );
    }

    {
        let a = MapsApp::new();
        assert!(a.model.places.is_empty(), "no invented places");
        assert!(a.model.recents.is_empty(), "no invented recent searches");
        assert!(a.model.results.is_empty());
        assert!(a.model.selected.is_none());
        assert!(a.model.route.is_none(), "no invented route");
        assert!(a.model.transit.is_empty(), "no invented departures");
        assert!(a.model.query.is_empty());
        assert!(a.basemap.roads.is_empty(), "no invented street grid");
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn overflow_menu_and_offline_download_dispatch_target_the_right_area() {
    {
        let mut a = app_at(1280.0, 800.0);
        let labels: Vec<&str> = a
            .overflow
            .items()
            .iter()
            .filter_map(|i| i.label())
            .collect();
        assert_eq!(labels, vec!["Attribution", "Offline maps", "Licenses"]);

        assert!(!a.overflow.is_open());
        tap(&mut a, &Hit::ToggleOverflow);
        assert!(a.overflow.is_open(), "the ⋯ button opens the overflow menu");
        assert!(
            a.overflow.get_rect().y1 <= a.overflow_rect().y0 + 1.0,
            "the menu sits above its button"
        );

        a.overflow_select(0);
        assert_eq!(a.info_sheet.current(), Some(MapInfo::Attribution));
        a.info_sheet.close();
        a.overflow_select(1);
        assert!(
            a.offline_panel.is_open(),
            "‘Offline maps’ opens the download dialog"
        );
        a.offline_panel.close();
        a.overflow_select(2);
        assert_eq!(a.info_sheet.current(), Some(MapInfo::Licenses));
        a.info_sheet.close();

        tap(&mut a, &Hit::ToggleOverflow);
        assert!(
            !a.overflow.is_open(),
            "the ⋯ button toggles the menu closed"
        );
    }

    {
        let mut a = app_at(1280.0, 800.0);
        let dir = region_test_cache("citydl");
        a.tiles = TileLayer::offline_with_cache_dir(dir.clone());
        a.tiles.allow_prefetch_for_test();

        a.offline_panel.open(a.tiles.cache_bytes());
        assert!(
            a.offline_panel.is_open() && a.offline_panel.is_editing(),
            "the city field is focused on open"
        );
        a.offline_panel
            .set_demo(a.tiles.cache_bytes(), "Mun", sample::download_regions());
        let munich = a.offline_panel.select_result(0).unwrap();

        let before = a.model.regions.len();
        tap_offline(&mut a, OfflineAction::Download);

        assert!(
            a.tiles.progress().total > 0,
            "the picked city's tiles were queued"
        );
        assert_eq!(a.model.regions.len(), before + 1);
        let region = a.model.regions.last().unwrap();
        assert_eq!(
            region.name, "Munich",
            "the region is named after the searched place"
        );
        assert!(
            (region.min.lat - munich.sw.lat).abs() < 1e-6
                && (region.max.lon - munich.ne.lon).abs() < 1e-6,
            "the download footprint is the place's bounding box, not the screen rect"
        );
        let city_z = a
            .offline_panel
            .download_request(
                a.viewport_bounds(),
                TileLayer::tile_zoom(a.model.zoom),
                a.rect,
            )
            .z0;
        a.offline_panel.select_result(2).unwrap();
        let plaza_z = a
            .offline_panel
            .download_request(
                a.viewport_bounds(),
                TileLayer::tile_zoom(a.model.zoom),
                a.rect,
            )
            .z0;
        assert!(
            plaza_z > city_z,
            "a small place fits at a deeper zoom than a whole city"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    {
        let mut a = app_at(1280.0, 720.0);
        a.tiles.allow_prefetch_for_test();
        a.offline_panel.open(a.tiles.cache_bytes());
        assert!(a.offline_panel.is_open(), "opening shows the overlay");
        tap_offline(&mut a, OfflineAction::Download);
        assert!(
            a.model.status.starts_with("Downloading") && a.model.status.contains("tiles"),
            "download surfaces a tile-count status, got {:?}",
            a.model.status
        );
        assert!(
            a.tiles.progress().total > 0,
            "a real region of tiles was queued"
        );
    }

    {
        let mut a = app_at(1280.0, 720.0);
        assert!(!a.tiles.allows_prefetch());
        a.offline_panel.open(a.tiles.cache_bytes());
        a.draw_app(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
        );
        let mut doc = AutoCommit::new();
        assert!(
            !Widget::perform_semantic_action(
                &mut a,
                "maps.offline.download",
                &ui::a11y::Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ),
            "no actionable Download target while saving areas is refused"
        );
        let before = a.model.regions.len();
        tap_offline(&mut a, OfflineAction::Download);
        assert_eq!(a.tiles.progress().total, 0, "nothing was queued");
        assert_eq!(a.model.regions.len(), before, "no region was recorded");
        assert_eq!(a.model.status, crate::offline_panel::areas_refused());
    }

    {
        let mut a = app_at(1280.0, 720.0);
        for _ in 0..10 {
            tap_offline(&mut a, OfflineAction::ZoomDelta(1));
        }
        assert_eq!(a.offline_panel.zoom_extra(), 6, "zoom-depth caps at 6");
        for _ in 0..10 {
            tap_offline(&mut a, OfflineAction::ZoomDelta(-1));
        }
        assert_eq!(a.offline_panel.zoom_extra(), 0, "zoom-depth floors at 0");
    }
}

#[test]
fn traffic_overlay_populates_honestly_and_draws_its_legend_only_when_on() {
    {
        let mut a = app_at(1280.0, 800.0);
        assert!(
            !a.map_controls.traffic_enabled() && a.traffic.is_empty(),
            "off + empty at start"
        );
        tap_control(&mut a, MapControlAction::Layers);
        assert!(
            a.menu_open(MapMenu::Layers),
            "the layers button opens the menu of layers"
        );
        let rows: Vec<_> = a
            .overflow
            .items()
            .iter()
            .map(|item| (item.label().map(str::to_owned), item.checked()))
            .collect();
        assert_eq!(rows, [(Some("Traffic".to_owned()), Some(false))]);
        assert!(
            !a.map_controls.traffic_enabled(),
            "opening it changes nothing"
        );
        a.overflow_select(0);
        a.overflow.close();
        assert!(
            a.map_controls.traffic_enabled(),
            "choosing Traffic turns the overlay on"
        );
        assert!(
            !a.traffic.is_empty(),
            "sample cartography yields congestion segments"
        );
        tap_control(&mut a, MapControlAction::Layers);
        assert_eq!(
            a.overflow.items().first().and_then(MenuItem::checked),
            Some(true),
            "Traffic shows as on"
        );
        a.overflow_select(0);
        assert!(
            !a.map_controls.traffic_enabled(),
            "choosing it again turns the overlay back off"
        );
    }

    {
        let mut a = MapsApp::offline_empty();
        a.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        assert!(a.basemap.is_empty(), "live/empty cartography");
        a.set_traffic(true);
        assert!(a.map_controls.traffic_enabled(), "overlay is on");
        assert!(a.traffic.is_empty(), "no data is invented on the live path");
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 800.0);
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        a.set_traffic(true);
        let mut scene2 = Scene::new();
        a.draw_app(&mut scene2, Affine::IDENTITY, &mut tr, &theme);
        assert!(
            a.map_controls.traffic_enabled() && !a.traffic.is_empty(),
            "overlay on with data → legend drawn"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn place_search_matches_ranks_and_labels_results() {
    {
        let mut a = app_at(1280.0, 800.0);
        a.model.query = "viktualien".to_string();
        a.run_search(false);
        assert_eq!(a.model.selected.as_deref(), Some("viktualienmarkt"));
        assert!(
            a.model
                .recents
                .iter()
                .any(|r| r.eq_ignore_ascii_case("viktualien"))
        );
    }

    {
        let mut a = app_at(1280.0, 800.0);
        a.model.query = "coffee".to_string();
        a.run_search(false);
        assert!(a.model.results.iter().any(|p| p.category == Category::Cafe));
    }

    {
        let mut a = app_at(1280.0, 720.0);
        a.model.origin = None;
        a.model.center = LatLon::new(48.10, 11.50);
        a.model.places = vec![
            crate::model::Place::new(
                "far",
                "Far Cafe",
                Category::Cafe,
                "",
                LatLon::new(48.30, 11.90),
            ),
            crate::model::Place::new(
                "near",
                "Near Cafe",
                Category::Cafe,
                "",
                LatLon::new(48.101, 11.501),
            ),
            crate::model::Place::new(
                "mid",
                "Mid Cafe",
                Category::Cafe,
                "",
                LatLon::new(48.15, 11.60),
            ),
        ];
        let ranked = a.ranked_matches("cafe");
        let ids: Vec<_> = ranked.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["near", "mid", "far"],
            "results ordered nearest-first"
        );
    }

    {
        let mut a = app_at(1280.0, 720.0);
        a.place_search.answer(
            "theat",
            vec![crate::model::Place::new(
                "osm-w-1",
                "Theatinerstraße 18",
                Category::Shop,
                "18, Theatinerstraße, München",
                LatLon::new(48.141, 11.577),
            )],
        );
        a.open_place("osm-w-1").expect("a found place opens");
        assert_eq!(
            a.model.selected.as_deref(),
            Some("osm-w-1"),
            "the hit is selected"
        );
        assert_eq!(
            a.model.selected_place().map(|p| p.name.as_str()),
            Some("Theatinerstraße 18"),
            "the place keeps its human name"
        );
        let at = a.projection().project(LatLon::new(48.141, 11.577));
        assert!(
            (at - a.visible_map_area().center()).hypot() < 1.0,
            "map navigated to the hit, in the middle of the visible map {at:?}"
        );
        assert!(
            a.model.recents.iter().any(|q| q == "Theatinerstraße 18"),
            "opening a hit records it as a recent search"
        );
    }

    {
        let mut a = app_at(1280.0, 800.0);
        a.model.places.clear(); // isolate: only the live POIs can match
        let c = a.model.center;
        a.pois = vec![
            crate::model::Poi {
                category: Category::Cafe,
                kind: Some("coffee".to_string()),
                ..demo_poi("cafe1", c.lat + 0.001, c.lon)
            },
            crate::model::Poi {
                category: Category::Shop,
                ..demo_poi("shop1", c.lat, c.lon + 0.001)
            },
        ];
        let by_kw = a.ranked_matches("coffee");
        assert!(
            by_kw.iter().any(|p| p.id == "cafe1"),
            "a category keyword surfaces the live POI"
        );
        assert!(
            !by_kw.iter().any(|p| p.id == "shop1"),
            "unrelated POIs stay out"
        );
        assert!(a.ranked_matches("café").iter().any(|p| p.id == "cafe1"));
        assert!(
            a.ranked_matches("POI cafe1")
                .iter()
                .any(|p| p.id == "cafe1")
        );
    }
}

#[test]
fn the_platform_search_finds_and_opens_places_in_a_window() {
    use ui::search::SearchQuery;

    let mut app = ui::framework::testing::TestApp::<crate::MapsShell>::new().unwrap();
    app.with_surface(|window: &mut MapWindow| window.app().online = false)
        .unwrap();
    let mut cafe = crate::model::Place::new(
        "cafe-glueck",
        "Café Glück",
        Category::Cafe,
        "Munich",
        LatLon::new(48.137, 11.575),
    );
    cafe.saved = true;
    app.invoke(crate::workspace::Workspace::ACCEPT_PLACES.with(vec![cafe]))
        .unwrap();
    app.settle();
    let providers = ui::search::capability::search_providers(app.harness().root_widget());
    assert_eq!(providers.len(), 1, "the platform owns Maps search");
    let search = &providers[0];
    let hits = |query: &str| {
        search
            .search_page(&SearchQuery::new(query), None, 50, 50)
            .results
    };

    let cafe = hits("coffee")
        .into_iter()
        .find(|hit| hit.title == "Café Glück")
        .expect("a keyword finds the saved café");
    assert_eq!(cafe.kind, "Saved places");
    assert!(
        search
            .search_scopes()
            .iter()
            .any(|scope| scope.anchor == crate::place_search::SAVED_SCOPE),
        "the search narrows to the saved places"
    );
    let saved = SearchQuery::new("coffee").within(crate::place_search::SAVED_SCOPE);
    assert_eq!(search.search_page(&saved, None, 50, 50).results.len(), 1);
    let address = cafe.action_address().expect("the hit opens its row");
    app.call_on(&address.resource, &address.action, serde_json::json!({}))
        .unwrap();
    app.settle();
    assert_eq!(
        app.with_surface(|window: &mut MapWindow| window.app().model.selected.clone()),
        Some(Some("cafe-glueck".to_owned()))
    );

    search.search_page(&SearchQuery::new("Theatiner"), None, 50, 50);
    app.settle();
    app.with_surface(|window: &mut MapWindow| {
        let mut maps = window.app();
        maps.place_search.answer(
            "Theatiner",
            vec![crate::model::Place::new(
                "osm-w-1",
                "Theatinerstraße 18",
                Category::Shop,
                "18, Theatinerstraße, München",
                LatLon::new(48.141, 11.577),
            )],
        );
    })
    .unwrap();
    app.settle();
    let providers = ui::search::capability::search_providers(app.harness().root_widget());
    let found = providers[0]
        .search_page(&SearchQuery::new("Theatiner"), None, 50, 50)
        .results
        .into_iter()
        .find(|hit| hit.kind == "Addresses and places")
        .expect("the geocoded place is found by its query");
    assert_eq!(found.title, "Theatinerstraße 18");
    let address = found.action_address().expect("the hit opens its row");
    app.call_on(&address.resource, &address.action, serde_json::json!({}))
        .unwrap();
    app.settle();
    assert!(
        app.state().places.iter().any(|place| place.id == "osm-w-1"),
        "opening a found place takes it into the map"
    );
}

#[test]
fn directions_to_a_found_place_route_there_from_its_row() {
    let mut app = ui::framework::testing::TestApp::<crate::MapsShell>::new().unwrap();
    app.with_surface(|window: &mut MapWindow| {
        let mut maps = window.app();
        maps.online = false;
        maps.place_search.answer(
            "Theatiner",
            vec![crate::model::Place::new(
                "osm-w-1",
                "Theatinerstraße 18",
                Category::Shop,
                "18, Theatinerstraße, München",
                LatLon::new(48.141, 11.577),
            )],
        );
    })
    .unwrap();
    app.settle();
    app.call_on("maps.place:osm-w-1", "directions", serde_json::json!({}))
        .expect("directions to the found place");
    app.settle();
    app.with_surface(|window: &mut MapWindow| {
        let maps = window.app();
        assert_eq!(maps.model.panel, Panel::Directions);
        assert_eq!(maps.model.selected.as_deref(), Some("osm-w-1"));
        assert_eq!(maps.model.dest_name, "Theatinerstraße 18");
        assert!(maps.model.dest.is_some(), "the route has its destination");
    })
    .unwrap();
    assert!(
        app.state().places.iter().any(|place| place.id == "osm-w-1"),
        "the found place is taken into the map"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn platform_search_opens_finds_and_activates_places() {
    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 720.0);
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        assert!(
            a.click_targets
                .iter()
                .any(|(_, hit)| matches!(hit, Hit::OpenSearch)),
            "the docked panel's header carries the search button"
        );
        let mut empty = MapsApp::offline_empty();
        empty.rect = a.rect;
        empty.map_controls.set_viewport(empty.rect);
        empty.draw_app(&mut Scene::new(), Affine::IDENTITY, &mut tr, &theme);
        let targets = empty.accessibility_targets();
        let keys: std::collections::HashSet<_> = targets
            .iter()
            .map(|target| target.target.as_str())
            .collect();
        assert_eq!(keys.len(), targets.len(), "visible targets must be unique");
        assert!(keys.contains("maps.search"));
        assert!(keys.contains("maps.empty-search"));
        tap(&mut empty, &Hit::OpenEmptySearch);
        assert!(empty.search_opener.is_pending());
        assert!(
            ui::widget::controls::ActionControls::take_control_invocation(&mut empty).is_some()
        );
        assert!(!empty.search_opener.is_pending());
        tap(&mut a, &Hit::OpenSearch);
        assert!(a.search_opener.is_pending());
        assert!(
            ui::widget::controls::ActionControls::take_control_invocation(&mut a).is_some(),
            "the button's request reaches the host as one invocation"
        );
        assert!(!a.search_opener.is_pending());

        a.rect = Rect::new(0.0, 0.0, 520.0, 820.0);
        a.nav_open = true;
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        let search = a.search_button_rect().expect("narrow search button");
        let topmost = a
            .click_targets
            .iter()
            .rev()
            .find(|(r, _)| r.contains(search.center()))
            .map(|(_, h)| h.clone());
        assert!(matches!(topmost, Some(Hit::OpenSearch)));
    }

    {
        use ui::search::{SearchProvider as _, SearchQuery};

        let mut a = app_at(1280.0, 720.0);
        a.pois = vec![crate::model::Poi {
            category: Category::Bar,
            ..demo_poi("bar1", a.model.center.lat, a.model.center.lon)
        }];
        let source = ui::framework::testing::search_source::<crate::workspace::Workspace>(
            a.place_search_entries(),
        );
        let hits = |query: &str| {
            source
                .search_page(&SearchQuery::new(query), None, 50, 50)
                .results
        };
        assert!(
            hits("Marienplatz")
                .iter()
                .any(|hit| hit.title == "Marienplatz" && hit.kind == "Recent searches"),
            "the recent searches are found"
        );
        let coffee = hits("coffee");
        let cafe = coffee
            .iter()
            .find(|hit| hit.title == "Café Glück" && hit.kind == "Saved places")
            .expect("a keyword finds a saved café");
        assert!(hits("beer").iter().any(|hit| hit.kind == "Nearby"));
        a.queue_place_search(&SearchQuery::new("coffee"));
        assert!(
            a.place_search.take_query().is_none(),
            "an offline map asks no geocoder"
        );

        let address = cafe.action_address().expect("the hit opens its row");
        assert_eq!(
            (address.resource.as_str(), address.action.as_str()),
            ("maps.place:cafe-glueck", "open-place")
        );
        assert!(
            ui::search::scope_contains(crate::place_search::SAVED_SCOPE, &cafe.anchor()),
            "a saved place lies in the saved places"
        );
        assert_eq!(
            crate::place_search::PlaceHit::opened("maps.place:cafe-glueck"),
            Some(crate::resource::MapsEffect::OpenPlace {
                place: "cafe-glueck".into()
            })
        );
        a.open_place("cafe-glueck").expect("the café opens");
        assert_eq!(a.model.selected.as_deref(), Some("cafe-glueck"));
    }

    {
        let mut a = app_at(1280.0, 720.0);
        tap(&mut a, &Hit::Recent("Marienplatz".to_string()));
        assert!(a.search_opener.is_pending(), "a recent opens the search");
    }

    {
        let mut a = app_at(1280.0, 720.0);
        a.stage_edit_origin();
        a.model.origin_query.clear();
        a.endpoints
            .sync_queries(&a.model.origin_query, &a.model.dest_query);

        let sentinel = "Alexanderplatz";
        ui::clipboard_copy(sentinel);
        assert_eq!(
            ui::clipboard_paste().as_deref(),
            Some(sentinel),
            "clipboard round-trips here (precondition for the paste path)"
        );

        let ev = ui::synthetic_input::key_event(
            Key::Character(winit::keyboard::SmolStr::new("v")),
            None,
            ElementState::Pressed,
        );
        let ctrl = {
            use winit::keyboard::ModifiersState;
            winit::event::Modifiers::from(ModifiersState::CONTROL)
        };
        ui::testing::Dispatch::new()
            .modifiers(ctrl)
            .send(&mut a, &ui::synthetic_input::keyboard_input(ev));

        assert_eq!(
            a.model.origin_query, sentinel,
            "Ctrl+V pasted the clipboard into the From field"
        );
    }

    {
        let mut a = app_at(1280.0, 720.0);
        a.model.query = "coffee".to_string();
        a.run_search(true);
        assert_eq!(
            a.model.recents.first().map(String::as_str),
            Some("coffee"),
            "visible search commits history through the map's verb"
        );

        let theme = Theme::dark();
        let mut renderer = TextRenderer::new();
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut renderer, &theme);
        assert!(
            a.click_targets
                .iter()
                .any(|(_, hit)| matches!(hit, Hit::ClearRecents)),
            "durable clear-history operation has an on-screen affordance"
        );
        a.dispatch(&Hit::ClearRecents);
        assert!(a.model.recents.is_empty());
    }

    {
        let mut a = app_at(1280.0, 720.0);
        assert_eq!(
            crate::place_search::PlaceHit::Recent("Marienplatz".into()).effect(),
            crate::resource::MapsEffect::Search {
                query: "Marienplatz".into()
            }
        );
        a.search_for("Marienplatz".to_string());
        assert_eq!(a.model.query, "Marienplatz", "the recent sets the query");
        assert_eq!(a.model.panel, Panel::Explore, "a search lands in Explore");
        assert!(
            !a.model.results.is_empty(),
            "the recent search actually runs"
        );
    }
}

fn clock_minutes(label: &str) -> Option<u32> {
    let (clock, half) = match label.split_once(' ') {
        Some((clock, half)) => (clock, Some(half)),
        None => (label, None),
    };
    let (hour, minute) = clock.split_once(':')?;
    if minute.len() != 2 {
        return None;
    }
    let (hour, minute): (u32, u32) = (hour.parse().ok()?, minute.parse().ok()?);
    if minute >= 60 {
        return None;
    }
    let hour = match half {
        None if hour < 24 => hour,
        Some("AM") if (1..=12).contains(&hour) => hour % 12,
        Some("PM") if (1..=12).contains(&hour) => hour % 12 + 12,
        _ => return None,
    };
    Some(hour * 60 + minute)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn transit_time_and_journeys_drive_queries_and_map_fit() {
    {
        let at = ui::clock::capture_wall().expect("the capture time exists in the local zone");
        ui::time::testing::with_wall(at, || {
            let a = app_at(1280.0, 800.0);
            let route = a.model.route.clone().expect("demo has a road route");
            let (dep, arr) = a.road_trip_clock(route.duration_s);
            assert_eq!(
                dep,
                ui::clock::hour_minute(10, 42),
                "a capture leaves at its pinned time"
            );
            assert!(
                clock_minutes(&arr) > clock_minutes(&dep),
                "arrival derived from now + trip duration: {arr}"
            );
            let first = a.model.transit.first().expect("demo has departures");
            assert!(
                clock_minutes(&first.dep) > clock_minutes(&dep),
                "the departures lie ahead of now"
            );
        });
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 800.0);
        a.select_place("marienplatz");
        a.enter_directions();

        for mode in [TravelMode::Foot, TravelMode::Bike, TravelMode::Car] {
            a.set_mode(mode);
            let mut scene = Scene::new();
            a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
            assert!(
                a.click_targets
                    .iter()
                    .any(|(_, h)| matches!(h, Hit::TransitTimeMode(_))),
                "{mode:?} must offer the leave/arrive picker"
            );
            assert!(
                a.click_targets
                    .iter()
                    .any(|(_, h)| matches!(h, Hit::TransitTimeDelta(_))),
                "{mode:?} must offer the time stepper"
            );
        }

        a.set_mode(TravelMode::Car);
        let route = a.model.route.clone().expect("road route present");
        a.model.transit_time = crate::model::TransitTime::default();
        let is_clock = |s: &str| clock_minutes(s).is_some();
        let (dep, arr) = a.road_trip_clock(route.duration_s);
        assert!(is_clock(&dep) && is_clock(&arr), "clocks, got {dep} {arr}");
        a.model.transit_time.mode = TransitTimeMode::Arrive;
        let (dep2, arr2) = a.road_trip_clock(route.duration_s);
        assert_eq!(arr2, dep, "arrive-by anchors the arrival");
        assert!(is_clock(&dep2));
    }

    {
        let mut a = app_at(1280.0, 800.0);

        let now_q = a.transit_query();
        assert!(now_q.time.is_none(), "leave-now sends no explicit time");
        assert!(!now_q.arrive_by, "leave-now is not arrive-by");

        a.model.transit_time.mode = TransitTimeMode::Arrive;
        a.model.transit_time.offset_min = 45;
        let q = a.transit_query();
        assert!(q.arrive_by, "arrive mode sets arriveBy");
        let ts = q.time.clone().expect("a non-now anchor pins a timestamp");
        assert!(
            ts.contains('T') && ts.len() >= 19,
            "rfc3339 timestamp, got {ts}"
        );

        let url = net::transit_url(LatLon::new(48.10, 11.55), LatLon::new(48.14, 11.58), &q);
        assert!(url.contains("arriveBy=true"), "{url}");
        assert!(
            url.contains("&time="),
            "picked time must reach the query: {url}"
        );
    }

    {
        let mut a = app_at(1280.0, 800.0);
        tap(&mut a, &Hit::TransitTimeDelta(15));
        tap(&mut a, &Hit::TransitTimeDelta(15));
        assert_eq!(
            a.model.transit_time.offset_min, 30,
            "two +15 steps advance 30 min"
        );
        for _ in 0..5 {
            tap(&mut a, &Hit::TransitTimeDelta(-15));
        }
        assert_eq!(a.model.transit_time.offset_min, 0, "offset floors at now");
        tap(&mut a, &Hit::TransitTimeMode(TransitTimeMode::Arrive));
        tap(&mut a, &Hit::TransitTimeDelta(15));
        tap(&mut a, &Hit::TransitTimeNow);
        assert!(a.model.transit_time.is_now(), "\"Now\" resets to leave-now");
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 720.0);
        a.set_travel_mode(TravelMode::Transit);
        assert!(a.model.route.is_none(), "transit has no road route");
        let geom = a.selected_journey_geometry();
        assert!(
            geom.len() >= 2,
            "the selected itinerary has drawable leg geometry"
        );

        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);

        a.model.center = LatLon::new(0.0, 0.0);
        a.model.zoom = 3.0;
        assert!(key_char(&mut a, "f"), "'f' handled in transit mode");
        assert!(
            a.model.center.lat.abs() > 1.0 && a.model.center.lon.abs() > 1.0,
            "'f' reframed the map onto the itinerary, not left at (0,0): {:?}",
            a.model.center
        );
        assert!(a.model.zoom > 3.0, "'f' zoomed in to fit the itinerary");
    }

    {
        let mut a = app_at(1280.0, 720.0);
        a.set_travel_mode(TravelMode::Transit);
        assert!(
            a.model.transit.len() >= 3,
            "sample has multiple itineraries"
        );
        tap(&mut a, &Hit::Journey(0));
        let center0 = a.model.center;
        tap(&mut a, &Hit::Journey(2));
        assert_eq!(a.model.transit_choice, 2, "the third journey is selected");
        assert!(
            (a.model.center.lat - center0.lat).abs() > 1e-6
                || (a.model.center.lon - center0.lon).abs() > 1e-6,
            "a different itinerary reframes the map"
        );
    }
}

fn demo_poi(id: &str, lat: f64, lon: f64) -> crate::model::Poi {
    crate::model::Poi {
        id: id.to_string(),
        name: format!("POI {id}"),
        category: Category::Cafe,
        pos: LatLon::new(lat, lon),
        address: "Some Street 1".to_string(),
        opening_hours: Some("09:00–17:00".to_string()),
        website: None,
        phone: None,
        kind: None,
    }
}

#[test]
fn poi_fetch_debounces_caches_versions_and_clears() {
    ui::time::testing::with_monotonic_time(ui::time::now(), || {
        let mut a = app_at(1280.0, 720.0);
        a.model.zoom = 15.0;
        a.pan_by(24.0, 0.0);
        assert!(a.poi_debounce.is_some(), "a pan arms a POI refetch");
        let gen0 = a.poi_generation;

        a.tick_poi_debounce(ui::time::now());
        assert_eq!(
            a.poi_generation, gen0,
            "no fetch before the debounce elapses"
        );
        assert!(a.poi_debounce.is_some());

        let past = ui::time::now() + POI_DEBOUNCE + std::time::Duration::from_millis(1);
        a.tick_poi_debounce(past);
        assert_eq!(a.poi_generation, gen0 + 1, "the debounce issues one fetch");
        assert!(
            a.poi_fetched_bounds.is_some(),
            "the fetched bbox is recorded for caching"
        );
        assert!(a.poi_debounce.is_none());

        a.schedule_pois();
        a.tick_poi_debounce(ui::time::now() + POI_DEBOUNCE + std::time::Duration::from_millis(1));
        assert_eq!(
            a.poi_generation,
            gen0 + 1,
            "same viewport reuses the cached POIs"
        );
    });

    {
        let mut a = app_at(1280.0, 720.0);
        a.pois = vec![demo_poi("a", 48.1, 11.5)];
        a.poi_fetched_bounds = Some(a.padded_viewport_bounds());
        a.model.zoom = 10.0;
        a.schedule_pois();
        a.tick_poi_debounce(ui::time::now() + POI_DEBOUNCE + std::time::Duration::from_millis(1));
        assert!(a.pois.is_empty(), "zoom-out clears the POI markers");
        assert!(a.poi_fetched_bounds.is_none());
    }

    {
        let mut a = app_at(1280.0, 720.0);
        a.poi_generation = 5;
        a.net_tx
            .send(NetMsg::Pois {
                generation: 5,
                pois: Ok(vec![demo_poi("p1", 48.1, 11.5)]),
            })
            .unwrap();
        a.drain_net();
        assert_eq!(a.pois.len(), 1, "a current reply populates the layer");

        a.net_tx
            .send(NetMsg::Pois {
                generation: 4,
                pois: Ok(vec![]),
            })
            .unwrap();
        a.drain_net();
        assert_eq!(
            a.pois.len(),
            1,
            "a stale reply must not clear the current POIs"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn poi_and_marker_presses_select_open_cards_and_route() {
    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 720.0);
        a.model.places.clear(); // no place pins to intercept the press
        let anchor = Point::new(900.0, 380.0);
        let pos = a.projection().unproject(anchor);
        a.pois = vec![demo_poi("p1", pos.lat, pos.lon)];
        a.selected_poi = None;
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);

        move_to(&mut a, anchor.x, anchor.y);
        let handled = press_left(&mut a);
        assert!(handled, "a press on a POI is handled");
        assert_eq!(
            a.selected_poi.as_deref(),
            Some("p1"),
            "pressing a POI opens its card"
        );

        let mut scene2 = Scene::new();
        a.draw_app(&mut scene2, Affine::IDENTITY, &mut tr, &theme);
        assert!(
            a.click_targets
                .iter()
                .any(|(_, h)| matches!(h, Hit::ClosePoi)),
            "card has a close target"
        );
        assert!(
            a.click_targets
                .iter()
                .any(|(_, h)| matches!(h, Hit::PoiDirections)),
            "card has a Directions target"
        );
    }

    {
        let mut a = app_at(1280.0, 720.0);
        a.pois = vec![demo_poi("pd", 48.16, 11.60)];
        a.selected_poi = Some("pd".to_string());
        a.model.mode = TravelMode::Car;
        tap(&mut a, &Hit::PoiDirections);
        assert_eq!(a.model.panel, Panel::Directions);
        assert_eq!(
            a.model.dest,
            Some(LatLon::new(48.16, 11.60)),
            "POI becomes the destination"
        );
        assert_eq!(a.model.dest_name, "POI pd");
        assert!(a.model.route.is_some(), "a route to the POI is computed");
        assert!(
            a.selected_poi.is_none(),
            "the card closes when directions start"
        );
        assert!(
            a.model.place("pd").is_some(),
            "the POI is upserted as a Place so its pin shows"
        );
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 800.0);
        a.model.panel = Panel::Explore;
        a.model.origin = None; // rank against the map centre, deterministically
        let c = a.model.center;
        let sel_before = a.model.selected.clone();
        let near = LatLon::new(c.lat + 0.0008, c.lon);
        let far = LatLon::new(c.lat + 0.02, c.lon);
        a.pois = vec![
            demo_poi("far", far.lat, far.lon),
            demo_poi("near", near.lat, near.lon),
        ];
        a.selected_poi = None;

        let listed = a.nearby_pois();
        assert_eq!(
            listed.first().map(|p| p.id.as_str()),
            Some("near"),
            "closest POI leads the list"
        );

        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        let has = |id: &str| {
            a.click_targets
                .iter()
                .any(|(_, h)| matches!(h, Hit::SelectPoi(s) if s == id))
        };
        assert!(
            has("near") && has("far"),
            "every nearby row is a click target"
        );

        tap(&mut a, &Hit::SelectPoi("far".to_string()));
        assert_eq!(
            a.selected_poi.as_deref(),
            Some("far"),
            "the row opens the POI card"
        );
        assert_eq!(a.model.center, far, "the map re-centres on the chosen POI");
        assert_eq!(
            a.model.selected, sel_before,
            "selecting a POI never disturbs the place selection"
        );
    }

    {
        let mut a = app_at(1280.0, 800.0);
        a.model.panel = Panel::Explore;
        a.model.origin = None; // rank against the map centre, deterministically
        a.model.zoom = 16.5;
        let c = a.model.center;
        a.pois = (0..(NEARBY_MAX + 4))
            .map(|i| demo_poi(&format!("p{i}"), c.lat + 0.001 * (i as f64 + 1.0), c.lon))
            .collect();
        let far_id = format!("p{}", NEARBY_MAX + 3);
        a.selected_poi = Some(far_id.clone());
        let listed = a.nearby_pois();
        assert!(
            listed.iter().any(|p| p.id == far_id),
            "the open POI is always present in the list even past the cap"
        );
        assert_eq!(listed.len(), NEARBY_MAX, "the list stays capped");

        let far_poi = a.pois.iter().find(|p| p.id == far_id).unwrap().clone();
        let hit = a.poi_at(a.projection().project(far_poi.pos));
        assert_eq!(
            hit.as_deref(),
            Some(far_id.as_str()),
            "a marker click resolves to that POI"
        );
    }

    {
        let mut a = app_at(1280.0, 800.0);
        a.model.zoom = 17.0;
        let c = a.model.center;
        a.pois = vec![
            crate::model::Poi {
                category: Category::Shop,
                ..demo_poi("shop", c.lat, c.lon)
            },
            crate::model::Poi {
                category: Category::Transit,
                ..demo_poi("transit", c.lat, c.lon)
            },
        ];
        let vis = a.visible_poi_indices();
        assert_eq!(vis.len(), 1, "overlapping markers collapse to one");
        let kept = a.pois[vis[0]].id.clone();
        assert_eq!(kept, "transit", "the salient category is the kept marker");
        let p = a.projection().project(c);
        assert_eq!(
            a.poi_at(p).as_deref(),
            Some("transit"),
            "hit-test agrees with the draw set"
        );
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 720.0);
        a.model.places.clear(); // no place pins to intercept the press
        let panel_before = a.model.panel;
        let mode_before = a.model.mode;
        let anchor = Point::new(900.0, 380.0);
        let pos = a.projection().unproject(anchor);
        a.pois = vec![demo_poi("p1", pos.lat, pos.lon)];
        a.selected_poi = None;
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);

        move_to(&mut a, anchor.x, anchor.y);
        assert!(press_left(&mut a), "a POI press is handled");
        assert_eq!(
            a.selected_poi.as_deref(),
            Some("p1"),
            "clicking a POI opens its info card"
        );
        assert_eq!(
            a.model.panel, panel_before,
            "no tab/view switch on a POI click"
        );
        assert_eq!(
            a.model.mode, mode_before,
            "no travel-mode switch on a POI click"
        );

        let mut scene2 = Scene::new();
        a.draw_app(&mut scene2, Affine::IDENTITY, &mut tr, &theme);
        assert!(
            a.click_targets
                .iter()
                .any(|(_, h)| matches!(h, Hit::ClosePoi)),
            "the POI info card is open"
        );
    }

    {
        let mut a = app_at(1280.0, 800.0);
        a.model.selected = None;
        let proj = a.projection();
        let place = a.model.place("hofgarten").unwrap().clone();
        let anchor = proj.project(place.pos);
        let head = Point::new(anchor.x, anchor.y - 22.0);
        let hit = a.marker_at(head);
        assert_eq!(hit.as_deref(), Some("hofgarten"));
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 720.0);
        let anchor = Point::new(900.0, 460.0);
        let pos = a.projection().unproject(anchor);
        a.model.places = vec![crate::model::Place::new(
            "pin",
            "Pin",
            Category::Cafe,
            "",
            pos,
        )];
        a.model.selected = None;
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        let head = Point::new(anchor.x, anchor.y - 22.0);
        move_to(&mut a, head.x, head.y);
        let handled = press_left(&mut a);
        assert!(handled, "a press on a pin must be handled");
        assert_eq!(
            a.model.selected.as_deref(),
            Some("pin"),
            "pressing the pin selects it"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn directions_modes_and_route_replies_recompute_the_route() {
    {
        let mut a = app_at(1280.0, 800.0);
        a.model.selected = Some("marienplatz".to_string());
        a.set_mode(TravelMode::Foot);
        let route = a.model.route.expect("route present");
        assert_eq!(route.mode, TravelMode::Foot);
    }

    {
        let mut a = app_at(1280.0, 800.0);
        a.select_place("marienplatz");
        a.enter_directions();
        let ab = a.model.route.clone().expect("route A→B present");
        let from0 = a.model.origin_name.clone();
        let to0 = a.model.dest_name.clone();
        assert!(ab.distance_m > 1.0, "initial route must be non-degenerate");
        assert_ne!(from0, to0, "endpoints must start distinct");

        a.endpoints.begin_edit(Endpoint::Origin);
        let mut scene = Scene::new();
        a.draw_app(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
        );
        let swap = a.endpoints.test_swap_rect().expect("swap control drawn");
        move_to(&mut a, swap.center().x, swap.center().y);
        assert!(press_left(&mut a), "swap control handles its click");
        assert!(
            !a.endpoints.is_focused(),
            "swap dismisses the endpoint draft and its overlay"
        );
        let ba = a.model.route.clone().expect("route B→A present");
        assert_eq!(a.model.origin_name, to0, "from-label must become old to");
        assert_eq!(a.model.dest_name, from0, "to-label must become old from");
        assert!(
            (ba.from.lat - ab.to.lat).abs() < 1e-9 && (ba.from.lon - ab.to.lon).abs() < 1e-9,
            "new origin must be the old destination"
        );
        assert!(
            (ba.to.lat - ab.from.lat).abs() < 1e-9 && (ba.to.lon - ab.from.lon).abs() < 1e-9,
            "new destination must be the old origin"
        );
        assert!(
            ba.distance_m > 1.0,
            "swapped route must not be a degenerate 0 m"
        );
    }

    {
        let mut a = app_at(1280.0, 800.0);
        a.select_place("hofgarten");
        assert!(
            a.model.transit.is_empty(),
            "selecting a new place drops stale departures"
        );
        a.model.panel = Panel::Explore;
        a.model.mode = TravelMode::Car;

        a.set_mode(TravelMode::Transit);
        assert_eq!(
            a.model.panel,
            Panel::Directions,
            "transit is a mode of Directions"
        );
        assert!(a.model.mode.is_transit());
        assert!(
            a.model.route.is_none(),
            "a transit journey draws no road route"
        );
        assert!(
            a.pending > 0,
            "transit mode must kick off a departures lookup"
        );
        assert!(
            a.model.status.contains("departures") || a.model.status.contains('…'),
            "a lookup should surface a loading status, got {:?}",
            a.model.status
        );
    }

    {
        let mut a = app_at(1280.0, 800.0);
        a.select_place("marienplatz");
        a.enter_directions();
        let fallback = a
            .model
            .route
            .clone()
            .expect("instant fallback route shown synchronously");
        assert!(
            fallback.distance_m > 0.0,
            "fallback is a real, non-empty estimate"
        );

        let origin = a.model.origin.expect("origin set by entering directions");
        let dest = a.model.dest.expect("destination set from the selection");
        let tx = a.net_tx.clone();
        let worker = std::thread::spawn(move || {
            let dense: Vec<LatLon> = (0..500)
                .map(|i| {
                    let f = f64::from(i) / 499.0;
                    LatLon::new(
                        origin.lat + (dest.lat - origin.lat) * f,
                        origin.lon + (dest.lon - origin.lon) * f,
                    )
                })
                .collect();
            let geometry = crate::geo::simplify_polyline(&dense, 3.0);
            let route = crate::model::Route {
                mode: TravelMode::Car,
                distance_m: 4200.0,
                duration_s: 600.0,
                geometry,
                steps: Vec::new(),
                from: origin,
                to: dest,
            };
            tx.send(NetMsg::Route(Ok(vec![route]))).unwrap();
            dense.len()
        });
        let dense_len = worker.join().expect("worker thread completes");

        a.drain_net();
        let landed = a.model.route.clone().expect("route present after drain");
        assert_eq!(
            landed.distance_m, 4200.0,
            "the worker's route replaced the fallback"
        );
        assert!(
            landed.geometry.len() < dense_len,
            "geometry simplified on the worker: {} < {dense_len}",
            landed.geometry.len()
        );
        assert!(landed.geometry.len() >= 2, "endpoints retained");
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 720.0);
        a.model.panel = Panel::Directions;
        a.model.mode = TravelMode::Car;
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        let seg = a
            .click_targets
            .iter()
            .find(|(_, h)| matches!(h, Hit::Mode(TravelMode::Transit)))
            .map(|(r, _)| r.center())
            .expect("Transit mode segment hot-region recorded");

        move_to(&mut a, seg.x, seg.y);
        let handled = press_left(&mut a);
        assert!(handled, "mode-segment click must be handled");
        assert!(
            a.model.mode.is_transit(),
            "clicking Transit selects transit mode"
        );
        assert_eq!(a.model.panel, Panel::Directions, "stays in Directions");
        assert!(a.model.route.is_none(), "transit clears the road route");
        assert!(a.pending > 0, "transit mode starts a departures lookup");
    }

    {
        let mut a = app_at(1280.0, 720.0);
        a.select_place("marienplatz");
        a.enter_directions();
        assert!(a.model.route.is_some());
        tap(&mut a, &Hit::ClearRoute);
        assert!(a.model.route.is_none(), "clear-route drops the route");
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 720.0);
        a.model.panel = Panel::Explore;
        a.select_place("marienplatz");
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);

        assert!(
            a.click_targets
                .iter()
                .any(|(_, h)| matches!(h, Hit::ToggleCollapse)),
            "the panel has a collapse toggle"
        );

        let dir = a
            .click_targets
            .iter()
            .find(|(_, h)| matches!(h, Hit::Directions))
            .map(|(r, _)| r.center())
            .expect("the selected place card exposes a Directions action");
        move_to(&mut a, dir.x, dir.y);
        let handled = press_left(&mut a);
        assert!(handled, "the Directions action is handled");
        assert_eq!(
            a.model.panel,
            Panel::Directions,
            "the card action opens the Directions view (no tab toggle)"
        );

        let mut scene2 = Scene::new();
        a.draw_app(&mut scene2, Affine::IDENTITY, &mut tr, &theme);
        let back = a
            .click_targets
            .iter()
            .find(|(_, h)| matches!(h, Hit::PanelBack))
            .map(|(r, _)| r.center())
            .expect("Directions view has a back button");
        move_to(&mut a, back.x, back.y);
        assert!(press_left(&mut a), "back is handled");
        assert_eq!(
            a.model.panel,
            Panel::Explore,
            "back returns to the list view"
        );
    }

    {
        let mut a = app_at(1280.0, 720.0);
        a.select_place("marienplatz");
        a.model.panel = Panel::Explore;
        a.model.mode = TravelMode::Car;

        key_char(&mut a, "d");
        assert_eq!(a.model.panel, Panel::Directions, "'d' opens Directions");
        assert!(
            a.model.route.is_some(),
            "'d' computes a route for the selection"
        );

        key_char(&mut a, "t");
        assert!(a.model.mode.is_transit(), "'t' switches to Transit mode");
        assert!(a.model.route.is_none(), "transit drops the road route");

        let saved_before = a.model.place("marienplatz").unwrap().saved;
        key_char(&mut a, "s");
        assert_eq!(
            a.model.place("marienplatz").unwrap().saved,
            !saved_before,
            "'s' toggles the selected place's saved flag"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn narrow_layout_drawer_reflows_layers_and_toggles() {
    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = MapsApp::offline();

        a.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        assert!(!a.overlay_mode());
        assert!(a.panel_visible());
        let docked_w = a.panel_w();
        assert_eq!(docked_w, f64::from(a.metrics.panel_width_list));
        let docked_top = a.panel_rect().y0;
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        assert!(a.menu_rect().is_none());
        assert!(
            !a.click_targets
                .iter()
                .any(|(_, h)| matches!(h, Hit::ToggleNav))
        );

        a.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        assert!(a.overlay_mode());
        let mut scene2 = Scene::new();
        a.draw_app(&mut scene2, Affine::IDENTITY, &mut tr, &theme);
        assert!(a.menu_rect().is_some());
        assert!(
            a.click_targets
                .iter()
                .any(|(_, h)| matches!(h, Hit::ToggleNav))
        );
        assert!(
            a.panel_rect().y0 > docked_top + a.controls().size,
            "the drawer did not reflow under the top row"
        );

        a.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        assert!(!a.overlay_mode());
        assert!(a.menu_rect().is_none());
        assert_eq!(a.panel_rect().y0, docked_top);
    }

    {
        let mut a = MapsApp::offline();
        a.nav_open = false;
        a.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        assert!(a.overlay_mode());
        assert!(!a.panel_visible(), "drawer starts closed");
        tap(&mut a, &Hit::ToggleNav);
        assert!(a.panel_visible(), "menu opens the drawer");
        tap(&mut a, &Hit::CloseNav);
        assert!(!a.panel_visible(), "scrim/Esc closes the drawer");
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = MapsApp::offline();
        a.nav_open = true;
        a.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        assert!(a.overlay_mode());
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);

        let topmost = |a: &MapsApp, p: Point| -> Option<Hit> {
            a.click_targets
                .iter()
                .rev()
                .find(|(r, _)| r.contains(p))
                .map(|(_, h)| h.clone())
        };

        let sc = a.search_button_rect().unwrap().center();
        assert!(
            matches!(topmost(&a, sc), Some(Hit::OpenSearch)),
            "the search button opens search, not close the drawer"
        );
        let mc = a.menu_rect().unwrap().center();
        assert!(matches!(topmost(&a, mc), Some(Hit::ToggleNav)));
        let corner = Point::new(a.rect.x1 - 4.0, a.rect.y1 - 4.0);
        assert!(matches!(topmost(&a, corner), Some(Hit::CloseNav)));
    }

    {
        let mut a = MapsApp::offline();
        a.nav_open = false;
        a.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        a.model.query = "café".to_string();
        a.run_search(false);
        assert!(a.panel_visible(), "results should reveal the drawer");
    }

    {
        let mut a = app_at(1280.0, 720.0);
        key_char(&mut a, "1");
        assert_eq!(a.model.panel, Panel::Explore, "'1' selects the list view");
        key_char(&mut a, "2");
        assert_eq!(
            a.model.panel,
            Panel::Directions,
            "'2' selects the directions view"
        );

        assert!(!a.offline_panel.is_open());
        key_char(&mut a, "m");
        assert!(a.offline_panel.is_open(), "'m' opens the offline overlay");
        assert!(
            a.offline_panel.is_editing(),
            "the city search field takes focus on open"
        );
        key_named(&mut a, NamedKey::Escape);
        assert!(
            !a.offline_panel.is_open(),
            "Escape closes the offline overlay"
        );

        a.select_place("hofgarten");
        assert!(a.model.selected.is_some());
        key_named(&mut a, NamedKey::Escape);
        assert!(a.model.selected.is_none(), "Escape deselects the place");
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(520.0, 840.0);
        a.nav_open = true;
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);

        assert_eq!(a.navigation_end_rect, Rect::ZERO);
        let node = Widget::accessibility(&a).expect("drawer remains semantic");
        assert!(
            node.children.iter().all(|child| {
                !child
                    .action_target
                    .as_deref()
                    .is_some_and(|target| target.starts_with("maps.control:"))
            }),
            "covered map controls must not remain in the drawer's accessibility tree"
        );
        assert!(
            node.children
                .iter()
                .any(|child| { child.action_target.as_deref() == Some("maps.navigation") }),
            "the visible drawer toggle remains operable"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn route_endpoints_come_from_ranked_search_or_an_honest_location_fix() {
    {
        let mut a = app_at(1280.0, 800.0);
        a.select_place("marienplatz");
        a.enter_directions();
        let before = a.model.route.clone().expect("initial route present");

        a.endpoints.begin_edit(Endpoint::Origin);
        a.apply_endpoint_effect(EndpointEffect::Changed {
            endpoint: Endpoint::Origin,
            text: "hauptbahnhof".to_string(),
        });
        assert!(
            !a.endpoints.test_results(Endpoint::Origin).is_empty(),
            "typing surfaces a From match"
        );

        let picked = a.endpoints.test_results(Endpoint::Origin)[0].clone();
        a.pick_endpoint(Endpoint::Origin, &picked);
        assert_eq!(
            a.model.origin,
            Some(picked.pos),
            "origin coord set from the result"
        );
        assert_eq!(
            a.model.origin_name, picked.name,
            "origin label set from the result"
        );
        let after = a.model.route.clone().expect("route present after pick");
        assert!(
            (after.from.lat - picked.pos.lat).abs() < 1e-9
                && (after.from.lon - picked.pos.lon).abs() < 1e-9,
            "route must start from the newly-typed origin"
        );
        assert_ne!(after.from, before.from, "the origin actually changed");
    }

    {
        let mut a = app_at(1280.0, 800.0);
        a.endpoints.begin_edit(Endpoint::Destination);
        let p = crate::model::Place::new(
            "osm-n-1",
            "Englischer Garten",
            Category::Park,
            "München",
            LatLon::new(48.16, 11.60),
        );
        a.net_tx
            .send(NetMsg::EndpointSearch {
                origin: false,
                query: "english garden".into(),
                results: Ok(vec![p]),
            })
            .unwrap();
        a.drain_net();
        assert!(
            a.endpoints
                .test_results(Endpoint::Destination)
                .iter()
                .any(|r| r.id == "osm-n-1"),
            "geocoded result populates the To dropdown"
        );
        assert!(
            a.model.place("osm-n-1").is_some(),
            "result upserted so its marker shows"
        );
    }

    {
        let mut a = app_at(1280.0, 800.0);
        a.model.origin = None;
        a.model.center = LatLon::new(48.137, 11.575); // Munich → the search anchor
        a.endpoints.begin_edit(Endpoint::Destination);
        let far = crate::model::Place::new(
            "far",
            "Hauptbahnhof",
            Category::Landmark,
            "Hamburg",
            LatLon::new(53.553, 10.006), // ~610 km away
        );
        let near = crate::model::Place::new(
            "near",
            "Hauptbahnhof",
            Category::Landmark,
            "München",
            LatLon::new(48.140, 11.558), // ~2 km away
        );
        a.net_tx
            .send(NetMsg::EndpointSearch {
                origin: false,
                query: "hauptbahnhof".into(),
                results: Ok(vec![far, near]), // far listed first, as importance order would
            })
            .unwrap();
        a.drain_net();
        assert_eq!(
            a.endpoints
                .test_results(Endpoint::Destination)
                .first()
                .map(|r| r.id.as_str()),
            Some("near"),
            "the nearer same-named endpoint ranks first, not the distant namesake"
        );
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 720.0);
        a.select_place("marienplatz");
        a.enter_directions();
        assert_eq!(a.model.panel, Panel::Directions);

        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        let mode_rect_before = a
            .click_targets
            .iter()
            .find(|(_, h)| matches!(h, Hit::Mode(TravelMode::Car)))
            .map(|(r, _)| *r)
            .expect("mode control recorded");

        let from = a
            .endpoints
            .test_field_rect(Endpoint::Origin)
            .map(|rect| rect.center())
            .expect("From field recorded");
        move_to(&mut a, from.x, from.y);
        assert!(press_left(&mut a), "focusing the From field is handled");
        assert_eq!(
            a.endpoints.focused_endpoint(),
            Some(Endpoint::Origin),
            "the From field is focused"
        );

        for ch in ["h", "a", "u", "p", "t", "b", "a", "h", "n", "h", "o", "f"] {
            assert!(
                key_char(&mut a, ch),
                "typing into the From field is handled"
            );
        }
        assert!(
            !a.endpoints.test_results(Endpoint::Origin).is_empty(),
            "typing surfaces matches live (no Enter)"
        );
        assert!(
            a.endpoints.has_pending_query(),
            "a live geocode is armed on input"
        );

        let mut scene2 = Scene::new();
        a.draw_app(&mut scene2, Affine::IDENTITY, &mut tr, &theme);
        assert!(
            a.endpoint_overlay_active(),
            "the endpoint suggestions are showing"
        );
        let mode_rect_after = a
            .click_targets
            .iter()
            .find(|(_, h)| matches!(h, Hit::Mode(TravelMode::Car)))
            .map(|(r, _)| *r)
            .expect("mode control still recorded");
        assert_eq!(
            mode_rect_before, mode_rect_after,
            "opening the endpoint suggestions must NOT reflow the panel below it"
        );
        let expected = a.endpoints.test_results(Endpoint::Origin)[0].clone();
        assert!(key_named(&mut a, NamedKey::ArrowDown));
        assert!(key_named(&mut a, NamedKey::Enter));
        assert_eq!(
            a.model.origin,
            Some(expected.pos),
            "the editor emits the selected place to the route model"
        );
        assert!(
            !a.endpoints.is_focused(),
            "selecting a result closes the editor"
        );
    }

    {
        let mut a = app_at(1280.0, 800.0);
        a.select_place("marienplatz");
        a.enter_directions();

        a.locating = true;
        a.location_rx = Some(host_location(LocationOutcome::Located {
            lat: 48.11,
            lon: 11.53,
            accuracy_m: 8.0,
        }));
        a.drain_location();

        assert!(!a.locating, "a delivered fix clears the locating state");
        assert_eq!(
            a.model.origin,
            Some(LatLon::new(48.11, 11.53)),
            "origin set from the fix"
        );
        let route = a
            .model
            .route
            .clone()
            .expect("route re-planned from the fix");
        assert!(
            (route.from.lat - 48.11).abs() < 1e-9 && (route.from.lon - 11.53).abs() < 1e-9,
            "route starts at the device location"
        );
        assert!(
            !a.model.status.to_lowercase().contains("unavailable"),
            "a success must not read as an error, got {:?}",
            a.model.status
        );
    }

    {
        let mut a = app_at(1280.0, 800.0);
        let before = a.model.origin;
        a.locating = true;
        a.location_rx = Some(host_location(LocationOutcome::Unavailable(
            "permission denied".into(),
        )));
        a.drain_location();

        assert!(!a.locating, "the locating state clears on failure");
        assert_eq!(
            a.model.origin, before,
            "a failed fix must not fabricate an origin"
        );
        assert!(
            a.model.status.to_lowercase().contains("unavailable"),
            "the failure must be stated plainly, got {:?}",
            a.model.status
        );
    }
}

fn feed(a: &mut MapsApp, ev: &WindowEvent) -> bool {
    ui::testing::dispatch(a, ev)
}

fn move_to(a: &mut MapsApp, x: f64, y: f64) -> bool {
    feed(a, &ui::synthetic_input::cursor_moved(x, y))
}

fn press_left(a: &mut MapsApp) -> bool {
    feed(a, &ui::synthetic_input::mouse_press())
}

fn release_left(a: &mut MapsApp) -> bool {
    feed(a, &ui::synthetic_input::mouse_release())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn live_navigation_surface_keeps_identity_modality_and_motion_rules() {
    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 720.0);
        ui::widget::FloatingSurfaceWidgetExt::set_floating_surface_bounds(
            &mut a,
            Rect::new(0.0, 0.0, 1680.0, 900.0),
        );
        assert!(
            a.model.route.is_some(),
            "offline fixture has an active route"
        );
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        let origin = a.navigation_drag_handle.center();
        let before = a.navigation_rect();
        move_to(&mut a, origin.x, origin.y);
        assert!(press_left(&mut a));
        assert!(Widget::wants_pointer_capture(&a));

        let outside = Point::new(1400.0, origin.y + 32.0);
        assert!(move_to(&mut a, outside.x, outside.y));
        assert!(matches!(
            a.navigation_placement.attachment(),
            ui::floating_surface::Attachment::Desktop { .. }
        ));
        assert_eq!(
            a.navigation_rect(),
            before + (outside - origin),
            "navigation's transfer frame preserves exact geometry"
        );
        let request = ui::widget::FloatingSurfaceWidgetExt::take_floating_surface_request(&mut a)
            .expect("navigation publishes one native-surface handoff");
        let prepared_content = a.navigation_rect();
        ui::widget::FloatingSurfaceWidgetExt::on_floating_surface_host_message(
            &mut a,
            &ui::floating_surface::FloatingSurfaceHostMessage::PrepareSurface {
                id: request.id.clone(),
                token: "0123456789abcdef0123456789abcdef".into(),
                rect: ui::floating_surface::protocol_rect_array(prepared_content.inflate(8.0, 8.0)),
                content_rect: ui::floating_surface::protocol_rect_array(prepared_content),
                transfer_covered: true,
                coordinate_offset: [0.0, 0.0],
            },
        );
        assert!(!a.navigation_placement.is_desktop_adopted());
        ui::widget::FloatingSurfaceWidgetExt::on_floating_surface_host_message(
            &mut a,
            &ui::floating_surface::FloatingSurfaceHostMessage::Adopted {
                id: request.id,
                rect: ui::floating_surface::protocol_rect_array(prepared_content),
            },
        );
        assert!(a.navigation_placement.is_desktop_adopted());
        assert_eq!(a.navigation_rect(), prepared_content);
        let returned_pointer = Point::new(980.0, 310.0);
        assert!(move_to(&mut a, returned_pointer.x, returned_pointer.y));
        assert!(a.navigation_placement.is_return_pending());
        let final_shell_rect = a.navigation_rect();
        let presentations =
            ui::widget::FloatingSurfaceWidgetExt::floating_surface_presentations(&a);
        assert_eq!(presentations.len(), 1);
        assert!(presentations[0].returning_to_app);
        let request_id = a.active_navigation_request().unwrap().id;
        ui::widget::FloatingSurfaceWidgetExt::on_floating_surface_host_message(
            &mut a,
            &ui::floating_surface::FloatingSurfaceHostMessage::Suspended { id: request_id },
        );
        assert!(matches!(
            a.navigation_placement.attachment(),
            ui::floating_surface::Attachment::App { .. }
        ));
        assert_eq!(
            a.navigation_rect(),
            final_shell_rect,
            "the first app-owned navigation frame exactly covers the final shell frame"
        );
        assert!(Widget::wants_pointer_capture(&a));

        let continued_pointer = returned_pointer + vello::kurbo::Vec2::new(-16.0, 9.0);
        assert!(move_to(&mut a, continued_pointer.x, continued_pointer.y));
        assert_eq!(
            a.navigation_rect(),
            final_shell_rect + (continued_pointer - returned_pointer),
            "the first primary-window motion continues the same drag"
        );
        assert!(release_left(&mut a));
        assert!(!Widget::wants_pointer_capture(&a));
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 720.0);
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);

        assert!(a.model.route.is_some(), "fixture starts navigating");
        assert!(a.navigation_end_rect.area() > 0.0);
        assert!(
            a.navigation_drag_handle
                .intersect(a.navigation_end_rect)
                .area()
                == 0.0,
            "ending navigation must never begin a drag"
        );

        let end = a.navigation_end_rect.center();
        assert!(move_to(&mut a, end.x, end.y));
        assert!(a.navigation_end_hovered);
        assert!(press_left(&mut a));
        assert!(
            a.model.route.is_none(),
            "the visible End action clears the route"
        );
        assert!(!a.navigation_placement.is_dragging());
        assert_eq!(a.navigation_end_rect, Rect::ZERO);
    }

    {
        let mut a = app_at(1280.0, 720.0);
        let first = a
            .active_navigation_request()
            .expect("offline fixture has active navigation");
        let route = a.model.route.as_mut().unwrap();
        route.to.lat += 0.01;
        route.steps[0].instruction = "Turn right after the reroute".into();
        let rerouted = a.active_navigation_request().unwrap();

        assert_eq!(first.id, NAVIGATION_FLOAT_ID);
        assert_eq!(rerouted.id, first.id);
        assert!(matches!(
            rerouted.source,
            ui::floating_surface::FloatingSource::AppElement {
                ref app_id,
                ref element_id,
                ..
            } if app_id == "so.tilo.maps" && element_id == "active-navigation"
        ));
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1_280.0, 800.0);
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        assert!(a.navigation_end_rect.area() > 0.0);
        let node = Widget::accessibility(&a).expect("maps is semantic");
        assert!(node.children.iter().any(|child| {
            child.action_target.as_deref() == Some("maps.navigation.end")
                && child.name.as_deref() == Some("End navigation")
        }));

        let mut doc = AutoCommit::new();
        assert!(Widget::perform_semantic_action(
            &mut a,
            "maps.navigation.end",
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(a.model.route.is_none());
    }

    {
        let mut a = app_at(1_280.0, 800.0);
        a.navigation_placement
            .detach_to_desktop(Rect::new(820.0, 120.0, 1_180.0, 276.0));
        assert!(Widget::needs_animation_frame(&a));
        Widget::set_motion_reduced(&mut a, true);
        assert!(!Widget::needs_animation_frame(&a));
    }

    {
        let mut a = app_at(1280.0, 720.0);
        a.pending = 3;
        assert!(!Widget::needs_animation_frame(&a));
        assert_eq!(Widget::next_wake_deadline(&a), None);

        a.schedule_pois();
        let deadline = Widget::next_wake_deadline(&a).expect("the POI debounce wakes the app");
        assert!(deadline > ui::time::now());
        assert!(!Widget::needs_animation_frame(&a));
    }

    {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut app = app_at(1280.0, 720.0);
        app.info_sheet.open(MapInfo::Attribution);
        let mut scene = Scene::new();
        app.draw_app(&mut scene, Affine::IDENTITY, &mut text, &theme);

        let traffic = app.map_controls.rect(MapControlAction::Layers).center();
        move_to(&mut app, traffic.x, traffic.y);
        assert!(press_left(&mut app));
        assert!(
            !app.info_sheet.is_open(),
            "the modal sheet consumed the press"
        );
        assert!(
            !app.map_controls.traffic_enabled(),
            "the covered traffic control did not activate"
        );
    }
}

fn feed_key(a: &mut MapsApp, logical: Key) -> bool {
    let text = match &logical {
        Key::Character(s) => Some(s.clone()),
        Key::Named(NamedKey::Space) => Some(winit::keyboard::SmolStr::new(" ")),
        _ => None,
    };
    let ev = ui::synthetic_input::key_event(logical, text, ElementState::Pressed);
    feed(a, &ui::synthetic_input::keyboard_input(ev))
}

fn key_char(a: &mut MapsApp, ch: &str) -> bool {
    feed_key(a, Key::Character(winit::keyboard::SmolStr::new(ch)))
}

fn key_named(a: &mut MapsApp, named: NamedKey) -> bool {
    feed_key(a, Key::Named(named))
}

fn region_test_cache(tag: &str) -> std::path::PathBuf {
    let n = ui::time::try_unix_now_ns().unwrap();
    std::env::temp_dir().join(format!("tilo-maps-{tag}-{}-{}", std::process::id(), n))
}

#[test]
fn semantic_controls_are_stable_modal_and_operable() {
    {
        let a = app_at(1_280.0, 800.0);
        let node = Widget::accessibility(&a).expect("maps is semantic");
        assert_eq!(node.children.len(), 4, "only drawn controls are exposed");
        let expected_targets = [
            "maps.control:zoom-in",
            "maps.control:zoom-out",
            "maps.control:locate",
            "maps.control:layers",
        ];
        for (index, (child, expected_target)) in
            node.children.iter().zip(expected_targets).enumerate()
        {
            assert_eq!(child.action_target.as_deref(), Some(expected_target));
            assert_eq!(
                Widget::synthetic_accessibility_rect(&a, &[index]),
                child.bounds.map(ui::a11y::AccessBounds::to_rect),
                "control {index} must activate at its visible bounds"
            );
        }
        let layers = node
            .children
            .iter()
            .find(|child| child.action_target.as_deref() == Some("maps.control:layers"))
            .expect("layers semantics");
        assert_eq!(layers.role, ui::a11y::Role::ToggleButton);
        assert_eq!(layers.state.checked, Some(false));
    }

    {
        let mut a = app_at(1_280.0, 800.0);
        let mut doc = AutoCommit::new();
        let node = Widget::accessibility(&a).expect("maps is semantic");
        let mut targets: Vec<_> = node
            .children
            .iter()
            .filter_map(|child| child.action_target.as_deref())
            .collect();
        targets.sort_unstable();
        targets.dedup();
        assert_eq!(
            targets.len(),
            node.children.len(),
            "no duplicate semantic ids"
        );
        assert!(Widget::perform_semantic_action(
            &mut a,
            "maps.control:layers",
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(a.menu_open(MapMenu::Layers));
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1_280.0, 800.0);
        a.offline_panel.open(0);
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        Widget::draw_overlay(
            &mut a,
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::empty(),
        );

        let node = Widget::accessibility(&a).expect("offline dialog is semantic");
        assert_eq!(node.role, ui::a11y::Role::Dialog);
        assert!(node.state.modal);
        assert_eq!(node.name.as_deref(), Some("Offline maps"));
        assert!(node.children.iter().all(|child| {
            child
                .action_target
                .as_deref()
                .is_some_and(|target| target.starts_with("maps.offline."))
        }));
        let close_index = node
            .children
            .iter()
            .position(|child| child.action_target.as_deref() == Some("maps.offline.close"))
            .expect("close control");
        assert_eq!(
            Widget::synthetic_accessibility_rect(&a, &[close_index]),
            node.children[close_index]
                .bounds
                .map(ui::a11y::AccessBounds::to_rect)
        );

        let mut doc = AutoCommit::new();
        assert!(Widget::perform_semantic_action(
            &mut a,
            "maps.offline.close",
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(!a.offline_panel.is_open());
    }
}

#[test]
fn authored_geometry_keeps_gaps_and_scales_with_text() {
    {
        let theme = Theme::dark();
        let one = navigation_card_size(&theme);
        assert_eq!(one.width, f64::from(theme.tokens.metrics.panel_width_list));
        let two = navigation_card_size(&Theme::dark().with_text_scale(2.0));
        assert_eq!(two.height.fract(), 0.0);
        assert!(two.height > one.height && two.width > one.width);
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 800.0);
        a.model.panel = Panel::Explore;
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        let mut rows: Vec<Rect> = a
            .click_targets
            .iter()
            .filter(|(_, hit)| matches!(hit, Hit::SelectPlace(_) | Hit::SelectPoi(_)))
            .map(|(rect, _)| *rect)
            .collect();
        assert!(rows.len() >= 2, "the offline demo lists several places");
        rows.sort_by(|a, b| a.y0.total_cmp(&b.y0));
        for pair in rows.windows(2) {
            let gap = pair[1].y0 - pair[0].y1;
            assert!(
                gap >= -1e-6,
                "rows {:?} and {:?} overlap by {}px",
                pair[0],
                pair[1],
                -gap
            );
        }
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 800.0);
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        let sp = theme.tokens.spacing;
        assert_eq!(a.gap(), f64::from(sp.s3));
        let panel = a.panel_rect();
        assert_eq!(panel.y0, f64::from(sp.s4), "the panel keeps the margin");
        assert_eq!(panel.x0, f64::from(sp.s4));
        let overflow = a.overflow_rect();
        assert_eq!(a.rect.x1 - overflow.x1, f64::from(sp.s4));
        assert_eq!(a.rect.y1 - overflow.y1, f64::from(sp.s4));
        assert!(
            overflow.x0 - panel.x1 >= a.gap(),
            "the overflow button clears the panel by a sibling gap"
        );
    }

    {
        let mut a = app_at(1600.0, 900.0);
        let list = f64::from(a.metrics.panel_width_list);
        a.text_scale = 1.0;
        assert!(!a.overlay_mode());
        assert_eq!(a.panel_w(), list);
        a.text_scale = 2.0;
        assert!(!a.overlay_mode(), "the breakpoint is the theme's, unscaled");
        let two = a.panel_w();
        assert!(two > list, "a 2× panel is wider than the 1× column");
        assert!(
            two <= 1600.0 * 0.6,
            "the map keeps at least 40 % of a docked window"
        );

        let mut mid = app_at(880.0, 900.0);
        mid.text_scale = 2.0;
        assert!(!mid.overlay_mode());
        assert_eq!(f64::from(mid.metrics.compact_breakpoint), 840.0);

        let mut narrow = app_at(400.0, 800.0);
        narrow.text_scale = 1.0;
        assert!(narrow.overlay_mode());
        assert_eq!(
            narrow.panel_w(),
            400.0 - 2.0 * narrow.margin() - narrow.controls().size - narrow.gap(),
            "the drawer stops short of the controls column"
        );

        let mut degenerate = app_at(0.0, 0.0);
        degenerate.text_scale = 1.0;
        let _ = degenerate.panel_w();
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn map_controls_stay_on_screen_clear_of_panel_and_navigation_card() {
    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 800.0);
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        assert!(
            a.navigation_surface_visible_in_app(),
            "the offline demo starts with an active route and its card"
        );
        let nav = a.navigation_rect();
        let zoom_in = a.map_controls.rect(MapControlAction::ZoomIn);
        let traffic = a.map_controls.rect(MapControlAction::Layers);
        assert!(
            zoom_in.y0 >= nav.y1,
            "zoom-in {zoom_in:?} must start below the navigation card {nav:?}"
        );
        for action in [
            MapControlAction::ZoomIn,
            MapControlAction::ZoomOut,
            MapControlAction::Locate,
            MapControlAction::Layers,
        ] {
            let rect = a.map_controls.rect(action);
            assert!(
                rect.intersect(nav).area() <= 0.0,
                "{action:?} {rect:?} overlaps the navigation card {nav:?}"
            );
            assert_eq!(
                a.map_controls.hit_test(rect.center()),
                Some(action),
                "hit rect must equal the painted rect"
            );
        }
        assert!(traffic.y1 <= a.rect.y1, "the whole cluster stays on screen");

        let mut plain = MapsApp::offline_empty();
        plain.rect = a.rect;
        plain.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        assert!(!plain.navigation_surface_visible_in_app());
        assert!(
            plain.map_controls.rect(MapControlAction::ZoomIn).y0 < nav.y1,
            "with no card the cluster sits at the top-right corner"
        );
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 360.0);
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        assert!(a.navigation_surface_visible_in_app());
        let nav = a.navigation_rect();
        let overflow = a.overflow_rect();
        for action in [
            MapControlAction::ZoomIn,
            MapControlAction::ZoomOut,
            MapControlAction::Locate,
            MapControlAction::Layers,
        ] {
            let rect = a.map_controls.rect(action);
            assert!(
                rect.y0 >= a.rect.y0 && rect.y1 <= a.rect.y1 && rect.x0 >= a.rect.x0,
                "{action:?} {rect:?} must stay inside the window {:?}",
                a.rect
            );
            assert!(
                rect.intersect(nav).area() <= 0.0,
                "{action:?} {rect:?} overlaps the navigation card {nav:?}"
            );
            assert!(
                rect.intersect(overflow).area() <= 0.0,
                "{action:?} {rect:?} overlaps the overflow button {overflow:?}"
            );
            assert_eq!(a.map_controls.hit_test(rect.center()), Some(action));
        }
        assert!(
            a.map_controls.rect(MapControlAction::ZoomIn).x1 <= nav.x0,
            "the cluster sits beside the card when it cannot fit beneath it"
        );
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let controls = [
            MapControlAction::ZoomIn,
            MapControlAction::ZoomOut,
            MapControlAction::Locate,
            MapControlAction::Layers,
        ];

        let mut docked = app_at(840.0, 360.0);
        let mut scene = Scene::new();
        docked.draw_app(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme.clone().with_text_scale(1.25),
        );
        assert!(docked.navigation_surface_visible_in_app() && !docked.overlay_mode());
        let panel = docked.panel_rect();
        let overflow = docked.overflow_rect();
        for action in controls {
            let rect = docked.map_controls.rect(action);
            assert!(
                rect.intersect(panel).area() <= 0.0,
                "{action:?} {rect:?} must not cover the docked panel {panel:?}"
            );
            assert!(
                rect.intersect(overflow).area() <= 0.0,
                "{action:?} {rect:?} must not cover the overflow button {overflow:?}"
            );
            assert!(rect.x0 >= docked.rect.x0 && rect.x1 <= docked.rect.x1);
        }
        assert_eq!(
            docked.map_controls_viewport(),
            docked.rect,
            "with no fitting placement the corner keeps HEAD's layout"
        );

        let mut tiny = app_at(380.0, 330.0);
        tiny.nav_open = false;
        tiny.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        assert!(tiny.overlay_mode());
        if tiny.navigation_surface_visible_in_app() {
            let overflow = tiny.overflow_rect();
            for action in controls {
                let rect = tiny.map_controls.rect(action);
                assert!(rect.y0 >= tiny.rect.y0 && rect.x1 <= tiny.rect.x1);
                assert!(
                    rect.intersect(overflow).area() <= 0.0,
                    "{action:?} {rect:?} must not cover the overflow button {overflow:?}"
                );
            }
            assert_eq!(tiny.map_controls_viewport(), tiny.rect);
        }
    }
}

#[test]
fn the_imagery_hint_never_waits_on_nothing() {
    use crate::tiles::{TileFault, TileLayer};
    let inert = TileLayer::offline();
    assert!(!inert.can_fetch());
    assert_eq!(
        imagery_hint(inert.can_fetch(), None),
        "Map imagery can't be fetched here"
    );
    assert_eq!(imagery_hint(true, None), "Loading map…");
    let unreachable = imagery_hint(true, Some(TileFault::Unreachable));
    assert!(!unreachable.contains("offline"), "{unreachable}");
}

#[path = "offline_region_tests.rs"]
mod offline_region_tests;
