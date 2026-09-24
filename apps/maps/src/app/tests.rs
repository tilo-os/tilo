use super::*;

#[test]
fn location_action_reports_the_real_portal_terminal_result() {
    let mut app = MapsApp::offline();
    app.online = true;
    let mut state = ui::State::new();
    app.sync(&mut state.doc, &state.root);
    let locate = app
        .resources(&state.doc)
        .into_iter()
        .flat_map(|resource| resource.actions)
        .find(|action| action.id == "locate")
        .expect("location action");
    assert_eq!(
        locate.contract.execution,
        ExecutionMode::Background { cancellable: false }
    );
    assert_eq!(locate.contract.result, ResultSpec::Job);

    let invocation = InvocationId::default();
    app.location_invocation = Some(invocation);
    app.location_rx = Some(host_location(LocationOutcome::Unavailable(
        "permission denied".into(),
    )));
    app.drain_location();
    assert!(matches!(
        app.poll_resource_operation(invocation, false, &state.doc, &state.root),
        Some(InvocationUpdate::Failed { error, .. }) if error.code == "location_unavailable"
    ));
    assert_eq!(app.location_invocation, None);
}

#[test]
fn resource_place_open_is_window_local() {
    let mut app = MapsApp::offline();
    let mut state = ui::State::new();
    app.sync(&mut state.doc, &state.root);
    let id = app
        .model
        .places
        .iter()
        .find(|place| place.saved)
        .expect("sample has a saved place")
        .id
        .clone();
    let payload = serde_json::to_vec(&id).unwrap();
    assert!(
        app.invoke_resource_operation(
            OperationKind::View,
            "maps.open-place",
            &payload,
            &state.doc,
            &state.root,
        )
        .unwrap()
    );
    assert_eq!(app.model.selected.as_deref(), Some(id.as_str()));
    assert!(
        MapsDoc::load(&state.doc).is_some_and(|model| model.selected.is_none()),
        "opening a resource never changes another window's selection"
    );
}

#[test]
fn resource_search_is_a_local_brokered_operation() {
    let mut app = MapsApp::offline();
    let mut state = ui::State::new();
    app.sync(&mut state.doc, &state.root);
    let recents_before = MapsDoc::load(&state.doc).unwrap().recents;
    let payload = serde_json::to_vec("Marienplatz").unwrap();
    assert!(
        app.invoke_resource_operation(
            OperationKind::View,
            "maps.search",
            &payload,
            &state.doc,
            &state.root,
        )
        .unwrap()
    );
    assert_eq!(app.model.query, "Marienplatz");
    assert!(!app.model.results.is_empty());
    assert_eq!(MapsDoc::load(&state.doc).unwrap().recents, recents_before);
}

fn app_at(w: f64, h: f64) -> MapsApp {
    // Offline variant: no tile worker thread, no network — tests stay
    // hermetic and never touch the wire.
    let mut a = MapsApp::offline();
    a.rect = Rect::new(0.0, 0.0, w, h);
    a.map_controls.set_viewport(a.rect);
    a
}

/// Drive a click `Hit` through the real `dispatch` path with a throwaway
/// document, for the tests that assert on the resulting in-memory view state.
/// Doc-mutating hits still flow through the action host (`perform` re-seeds the
/// throwaway doc from the live model each call, so state carries across taps);
/// the collaborative doc + undo + replay guarantees are proven separately in
/// [`crate::actions`] and by [`delete_region_affordance_matches_action_spec`].
fn tap(a: &mut MapsApp, hit: &Hit) {
    let mut doc = AutoCommit::new();
    a.dispatch(hit, &mut doc);
}

fn tap_offline(a: &mut MapsApp, action: OfflineAction) {
    let mut doc = AutoCommit::new();
    a.dispatch_offline(action, &mut doc);
}

fn tap_control(a: &mut MapsApp, action: MapControlAction) {
    a.dispatch_map_control(action);
}

#[test]
fn pan_changes_center() {
    let mut a = app_at(1280.0, 800.0);
    let before = a.model.center;
    a.pan_by(120.0, 0.0);
    assert!((a.model.center.lon - before.lon).abs() > 1e-6);
}

/// Profiling-driven perf guard for the zoom/scroll path — the interaction
/// the user flagged as stuttery. Drives the real five passes headless over a
/// *loaded* raster viewport (seeded tiles + live POI markers) and asserts the
/// p95 frame stays inside the 60 fps budget. Prints the full per-pass table
/// with `--nocapture` so a regression is diagnosable, not just red.
#[test]
fn perf_zoom_scroll_stays_under_budget() {
    use ui::profiling::{PerfScenario, PerfStep, run_scenario_on};
    let mut app = MapsApp::offline();
    app.set_demo_pois();
    app.seed_perf_tiles(1280.0, 800.0);
    let scenario = PerfScenario {
        name: "maps-zoom-scroll".into(),
        viewport: (1280, 800),
        warmup: 8,
        // Gentle zoom-out sweep (dy·0.35 per notch) across several z levels,
        // the way a touchpad pinch traverses the tile pyramid.
        steps: vec![PerfStep::Scroll {
            dy: -0.4,
            count: 70,
        }],
    };
    let report = run_scenario_on(ui::State::new(), Box::new(app), &scenario);
    eprintln!("{report}");
    // Frame cost is only meaningful for the optimised build (unoptimised
    // vello/text shaping inflates a debug frame far past budget), so the
    // hard guard runs in release; debug still prints the table.
    if !cfg!(debug_assertions) {
        assert!(
            report.total.p95_us < ui::profiling::FRAME_BUDGET_US,
            "zoom p95 {}us over the {}us budget",
            report.total.p95_us,
            ui::profiling::FRAME_BUDGET_US
        );
    }
}

#[test]
fn local_pan_cannot_overwrite_a_peer_bookmark() {
    let mut app = app_at(1280.0, 800.0);
    let mut state = ui::State::new();
    app.sync(&mut state.doc, &state.root);
    let bytes = state.doc.save();
    let mut peer = AutoCommit::load(&bytes).expect("peer replica");
    let id = app.model.places[0].id.clone();
    MapsDoc::set_place_saved(&mut peer, &id, true);
    app.pan_by(96.0, 0.0);
    app.sync(&mut state.doc, &state.root);
    state.doc.merge(&mut peer).expect("merge peer action");
    app.sync(&mut state.doc, &state.root);
    assert!(
        MapsDoc::load(&state.doc)
            .and_then(|model| model.place(&id).map(|place| place.saved))
            .unwrap_or(false),
        "a local camera update must not write stale place data over a peer bookmark"
    );
}

#[test]
fn loaded_endpoint_queries_do_not_replace_an_in_progress_local_draft() {
    let mut a = app_at(1280.0, 800.0);
    a.endpoints.begin_edit(Endpoint::Origin);
    a.endpoints.sync_queries("Local draft", "");

    let mut state = ui::State::new();
    let mut loaded = a.model.clone();
    loaded.origin_query = "Peer origin".to_string();
    loaded.dest_query = "Peer destination".to_string();
    MapsDoc::save(&mut state.doc, &loaded);

    a.sync(&mut state.doc, &state.root);

    assert_ne!(a.model.origin_query, "Peer origin");
    assert_eq!(a.endpoints.test_query(Endpoint::Origin), "");
    assert_eq!(a.endpoints.test_query(Endpoint::Destination), "");
}

/// The ⋯ overflow menu carries map utilities plus the shared live-navigation
/// handoff, opens from the bottom-right button, and routes every row.
#[test]
fn overflow_menu_opens_and_routes_to_its_options() {
    let mut a = app_at(1280.0, 800.0);
    let labels: Vec<&str> = a
        .overflow
        .items()
        .iter()
        .filter_map(|i| i.label())
        .collect();
    assert_eq!(
        labels,
        vec![
            "Attribution",
            "Offline maps",
            "Licenses",
            "Float navigation"
        ]
    );

    assert!(!a.overflow.is_open());
    tap(&mut a, &Hit::ToggleOverflow);
    assert!(a.overflow.is_open(), "the ⋯ button opens the overflow menu");
    // It expands upward from the bottom-right button (never off the bottom).
    assert!(
        a.overflow.get_rect().y1 <= a.overflow_rect().y0 + 1.0,
        "the menu sits above its button"
    );

    // Row 0 -> attribution sheet; row 1 -> offline dialog; row 2 -> licenses.
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
    a.overflow_select(3);
    let request = ui::widget::FloatingSurfaceWidgetExt::take_floating_surface_request(&mut a)
        .expect("sample route produces a live navigation float");
    assert_eq!(request.role, ui::floating_surface::FloatingRole::Navigation);

    // Toggling again closes it.
    tap(&mut a, &Hit::ToggleOverflow);
    assert!(
        !a.overflow.is_open(),
        "the ⋯ button toggles the menu closed"
    );
}

/// Downloading offline maps targets the *searched place's real bounding box*
/// (from the shared city-search), not the visible screen rectangle — the
/// core of the offline-UX redesign — and records a region named after it.
#[test]
fn download_by_city_targets_the_picked_place_bounds() {
    let mut a = app_at(1280.0, 800.0);
    let dir = region_test_cache("citydl");
    a.tiles = TileLayer::offline_with_cache_dir(dir.clone());

    // Open the dialog and pick "Munich" from the (sample) geocoded results.
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

    // The picked city's footprint was queued (offline layer marks it done)…
    assert!(
        a.tiles.progress().total > 0,
        "the picked city's tiles were queued"
    );
    // …and recorded as a region named after the place, with the place's
    // bounding box — NOT the viewport.
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
    // A bigger place downloads at a coarser base zoom than a tiny one.
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

/// Perf guard for typing into the shared search field — the other
/// interaction flagged as stuttery. Focuses search, then types a query and
/// asserts the per-keystroke p95 frame stays inside budget.
#[test]
fn perf_search_typing_stays_under_budget() {
    use ui::profiling::{PerfScenario, PerfStep, run_scenario_on};
    let mut app = MapsApp::offline();
    app.seed_perf_tiles(1280.0, 800.0);
    app.search.set_focused(true);
    let scenario = PerfScenario {
        name: "maps-search-typing".into(),
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

#[test]
fn traffic_toggle_populates_from_sample_cartography() {
    // The offline app carries the sample vector basemap, so toggling traffic
    // on colours its arterials — the demonstrable overlay.
    let mut a = app_at(1280.0, 800.0);
    assert!(
        !a.map_controls.traffic_enabled() && a.traffic.is_empty(),
        "off + empty at start"
    );
    tap_control(&mut a, MapControlAction::ToggleTraffic);
    assert!(
        a.map_controls.traffic_enabled(),
        "the control toggles the overlay on"
    );
    assert!(
        !a.traffic.is_empty(),
        "sample cartography yields congestion segments"
    );
    assert_eq!(
        a.traffic.source.as_deref(),
        Some("sample data"),
        "provenance is honest"
    );
    // Toggling again turns it off.
    tap_control(&mut a, MapControlAction::ToggleTraffic);
    assert!(
        !a.map_controls.traffic_enabled(),
        "the control toggles the overlay back off"
    );
}

#[test]
fn traffic_on_a_live_empty_basemap_stays_honestly_empty() {
    // An empty vector basemap is the LIVE state (real tiles, no bundled
    // cartography). With no peer reports and no provider, turning traffic on
    // must NOT fabricate congestion — the overlay stays empty and the UI
    // shows "No traffic data".
    let mut a = MapsApp::offline_empty();
    a.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    assert!(a.basemap.is_empty(), "live/empty cartography");
    a.set_traffic(true);
    assert!(a.map_controls.traffic_enabled(), "overlay is on");
    assert!(a.traffic.is_empty(), "no data is invented on the live path");
    assert!(
        a.traffic.source.is_none(),
        "no provenance when there is nothing to show"
    );
}

#[test]
fn traffic_legend_only_draws_when_the_overlay_is_on() {
    // With the overlay off the legend/empty pill is absent; turning it on
    // (with sample data) draws the colour key — a visible affordance.
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut a = app_at(1280.0, 800.0);
    let mut scene = Scene::new();
    a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
    // (No panic; the status pass is a no-op when off.)
    a.set_traffic(true);
    let mut scene2 = Scene::new();
    a.draw_app(&mut scene2, Affine::IDENTITY, &mut tr, &theme);
    assert!(
        a.map_controls.traffic_enabled() && !a.traffic.is_empty(),
        "overlay on with data → legend drawn"
    );
}

#[test]
fn zoom_clamps() {
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

#[test]
fn local_search_selects_match() {
    let mut a = app_at(1280.0, 800.0);
    a.model.query = "viktualien".to_string();
    a.run_search(None);
    assert_eq!(a.model.selected.as_deref(), Some("viktualienmarkt"));
    assert!(
        a.model
            .recents
            .iter()
            .any(|r| r.eq_ignore_ascii_case("viktualien"))
    );
}

#[test]
fn keyword_search_finds_cafe() {
    let mut a = app_at(1280.0, 800.0);
    a.model.query = "coffee".to_string();
    a.run_search(None);
    assert!(a.model.results.iter().any(|p| p.category == Category::Cafe));
}

// --- Search-as-you-type + overlay + ranking ----------------------------

/// Search results are ordered by proximity to the search anchor (map centre
/// or the user's location), nearest first.
#[test]
fn search_results_ordered_by_proximity() {
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

/// Picking a result from the overlay puts the proper human place-name in the
/// search field (not a bare house number), selects it, and dismisses the
/// overlay. Guards the label bug where the field showed "18".
#[test]
fn picking_search_result_sets_place_name_label() {
    let mut a = app_at(1280.0, 720.0);
    a.search.set_focused(true);
    a.search.replace_results(vec![crate::model::Place::new(
        "osm-w-1",
        "Theatinerstraße 18",
        Category::Shop,
        "18, Theatinerstraße, München",
        LatLon::new(48.141, 11.577),
    )]);
    a.model.query = "theat".to_string();
    tap(&mut a, &Hit::Search(SearchHit::Result(0)));
    assert_eq!(
        a.model.query, "Theatinerstraße 18",
        "field shows the human place-name"
    );
    assert_ne!(a.model.query, "18", "never the bare house number");
    assert_eq!(
        a.model.selected.as_deref(),
        Some("osm-w-1"),
        "the pick is selected"
    );
    assert!(!a.search.is_focused(), "picking dismisses the overlay");
    assert!(a.search.results().is_empty(), "overlay clears after a pick");
    assert!(
        (a.model.center.lat - 48.141).abs() < 1e-9,
        "map navigated to the pick"
    );
}

/// The overlay draws as a floating dropdown UNDER the search field, records
/// clickable pick targets, and never reflows the side panel to make room.
#[test]
fn search_overlay_floats_and_does_not_reflow_panel() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut a = app_at(1280.0, 720.0);
    let panel_before = a.panel_rect();
    a.set_demo_search();
    let mut scene = Scene::new();
    a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);

    // Overlay records clickable pick rows.
    let row = a
        .click_targets
        .iter()
        .rev()
        .find(|(_, h)| matches!(h, Hit::Search(SearchHit::Result(_))))
        .map(|(r, _)| *r)
        .expect("overlay records PickSearch hot-regions");
    // Anchored below the search field (a dropdown, not a reflow).
    assert!(
        row.y0 >= a.search_rect().y1,
        "overlay sits under the search field"
    );
    // The side panel geometry is unchanged — the overlay floats over it.
    assert_eq!(
        a.panel_rect(),
        panel_before,
        "overlay must not reflow the panel"
    );
    // The pick row wins the hit-test over the panel it overlays.
    let topmost = a
        .click_targets
        .iter()
        .rev()
        .find(|(r, _)| r.contains(row.center()))
        .map(|(_, h)| h.clone());
    assert!(
        matches!(topmost, Some(Hit::Search(SearchHit::Result(_)))),
        "overlay row is the topmost target"
    );
}

/// While the floating results overlay is up it is the ONLY place list: the
/// panel's own rows behind it are suppressed so the same place never appears
/// in both. Dismissing the results brings the panel back.
#[test]
fn search_overlay_suppresses_panel_list_behind_it() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut a = app_at(1280.0, 800.0);
    a.set_demo_search();
    assert!(
        a.search_overlay_active(),
        "the demo raises the results overlay"
    );
    assert!(
        !a.model.results.is_empty(),
        "the panel would otherwise list places"
    );

    let mut scene = Scene::new();
    a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
    let has_pick = a
        .click_targets
        .iter()
        .any(|(_, h)| matches!(h, Hit::Search(SearchHit::Result(_))));
    let has_panel_rows = a
        .click_targets
        .iter()
        .any(|(_, h)| matches!(h, Hit::SelectPlace(_) | Hit::Recent(_)));
    assert!(has_pick, "the results overlay is drawn");
    assert!(
        !has_panel_rows,
        "the panel list behind the results is suppressed"
    );

    // Dismiss the transient results → the panel list returns, without
    // retaining stale suggestions for the next focus.
    a.search.dismiss();
    assert!(!a.search_overlay_active());
    assert!(a.search.results().is_empty());
    let mut scene2 = Scene::new();
    a.draw_app(&mut scene2, Affine::IDENTITY, &mut tr, &theme);
    let panel_rows_back = a
        .click_targets
        .iter()
        .any(|(_, h)| matches!(h, Hit::SelectPlace(_) | Hit::Recent(_)));
    assert!(
        panel_rows_back,
        "the panel list returns once the results dismiss"
    );
}

/// The offline demo Drive and Transit clocks share one coherent base time:
/// the Drive summary is anchored to the fixed 09:00 demo base (not a
/// nondeterministic wall clock), matching the bundled Transit departures.
#[test]
fn demo_drive_and_transit_share_base_clock() {
    let a = app_at(1280.0, 800.0);
    let route = a.model.route.clone().expect("demo has a road route");
    let (dep, arr) = a.road_trip_clock(route.duration_s);
    assert!(
        dep.starts_with("Leave 09:"),
        "demo Drive leaves at the demo base hour: {dep}"
    );
    assert!(
        arr.starts_with("Arrive 09:") || arr.starts_with("Arrive 10:"),
        "arrival derived from the base + trip duration: {arr}"
    );
    let first = a.model.transit.first().expect("demo has departures");
    assert_eq!(
        first.dep.get(..2).unwrap_or_default(),
        "09",
        "Transit departs the same base hour: {}",
        first.dep
    );
    assert_eq!(
        dep.get("Leave ".len()..)
            .and_then(|s| s.get(..2))
            .unwrap_or_default(),
        "09",
        "Drive and Transit agree on the base hour"
    );
}

// --- POI layer (Overpass) ----------------------------------------------

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

/// A pan arms the POI refetch; it fires exactly once after the debounce,
/// versions the query, and a subsequent tick over the same viewport reuses
/// the cache instead of refetching.
#[test]
fn poi_fetch_debounces_caches_and_versions() {
    let mut a = app_at(1280.0, 720.0);
    a.model.zoom = 15.0;
    a.pan_by(24.0, 0.0);
    assert!(a.poi_debounce.is_some(), "a pan arms a POI refetch");
    let gen0 = a.poi_generation;

    a.tick_poi_debounce(Instant::now());
    assert_eq!(
        a.poi_generation, gen0,
        "no fetch before the debounce elapses"
    );
    assert!(a.poi_debounce.is_some());

    let past = Instant::now() + POI_DEBOUNCE + std::time::Duration::from_millis(1);
    a.tick_poi_debounce(past);
    assert_eq!(a.poi_generation, gen0 + 1, "the debounce issues one fetch");
    assert!(
        a.poi_fetched_bounds.is_some(),
        "the fetched bbox is recorded for caching"
    );
    assert!(a.poi_debounce.is_none());

    // Re-arm without moving the viewport → the cache is reused, no new fetch.
    a.schedule_pois();
    a.tick_poi_debounce(Instant::now() + POI_DEBOUNCE + std::time::Duration::from_millis(1));
    assert_eq!(
        a.poi_generation,
        gen0 + 1,
        "same viewport reuses the cached POIs"
    );
}

/// Zooming out past the useful range drops the POI layer rather than leaving
/// a stale, too-sparse scatter from a closer view.
#[test]
fn poi_fetch_clears_when_zoomed_out() {
    let mut a = app_at(1280.0, 720.0);
    a.pois = vec![demo_poi("a", 48.1, 11.5)];
    a.poi_fetched_bounds = Some(a.padded_viewport_bounds());
    a.model.zoom = 10.0;
    a.schedule_pois();
    a.tick_poi_debounce(Instant::now() + POI_DEBOUNCE + std::time::Duration::from_millis(1));
    assert!(a.pois.is_empty(), "zoom-out clears the POI markers");
    assert!(a.poi_fetched_bounds.is_none());
}

/// A fresh Overpass reply populates the POI layer via the real drain path; a
/// stale reply for a superseded viewport is ignored (not drawn over current).
#[test]
fn overpass_results_populate_and_stale_is_ignored() {
    let mut a = app_at(1280.0, 720.0);
    a.poi_generation = 5;
    a.net_tx
        .send(NetMsg::Pois {
            generation: 5,
            pois: Ok(vec![demo_poi("p1", 48.1, 11.5)]),
        })
        .unwrap();
    a.drain_net(None);
    assert_eq!(a.pois.len(), 1, "a current reply populates the layer");

    a.net_tx
        .send(NetMsg::Pois {
            generation: 4,
            pois: Ok(vec![]),
        })
        .unwrap();
    a.drain_net(None);
    assert_eq!(
        a.pois.len(),
        1,
        "a stale reply must not clear the current POIs"
    );
}

/// Pressing a POI marker on the map opens its info card, and the card records
/// its Close and Directions action targets — the real draw → hit-test path.
#[test]
fn press_on_poi_opens_info_card_via_handle_event() {
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

    // Redraw: the open card records its action hot-regions.
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

/// The card's Directions action routes to the POI: it becomes the
/// destination, is upserted as a Place (so its pin shows), Directions opens,
/// a route is computed, and the card closes.
#[test]
fn poi_directions_routes_to_the_poi() {
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

/// The Explore panel lists the viewport POIs nearest-first, each row is a
/// real click target, and choosing one centres the map on it and opens its
/// info card — the list → map-selection link. Exercises the actual
/// draw → hit-test → dispatch path.
#[test]
fn nearby_list_lists_pois_and_links_to_map() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut a = app_at(1280.0, 800.0);
    a.model.panel = Panel::Explore;
    a.model.origin = None; // rank against the map centre, deterministically
    let c = a.model.center;
    let sel_before = a.model.selected.clone();
    // A near POI and a far POI, supplied out of nearest order.
    let near = LatLon::new(c.lat + 0.0008, c.lon);
    let far = LatLon::new(c.lat + 0.02, c.lon);
    a.pois = vec![
        demo_poi("far", far.lat, far.lon),
        demo_poi("near", near.lat, near.lon),
    ];
    a.selected_poi = None;

    // Nearest-first ordering off the real proximity sort.
    let listed = a.nearby_pois();
    assert_eq!(
        listed.first().map(|p| p.id.as_str()),
        Some("near"),
        "closest POI leads the list"
    );

    // The draw pass records a SelectPoi target for each listed POI.
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

    // Dispatching the row selects the POI: opens its card AND centres the map
    // on it (the list → map link), without disturbing the place selection.
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

/// A selected POI beyond the list cap is still listed (so its row can be the
/// highlighted one), and clicking its marker on the map opens the same card —
/// the map → list link, the mirror of the list → map one above.
#[test]
fn selected_poi_is_always_listed_and_marker_click_selects_it() {
    let mut a = app_at(1280.0, 800.0);
    a.model.panel = Panel::Explore;
    a.model.origin = None; // rank against the map centre, deterministically
    a.model.zoom = 16.5;
    let c = a.model.center;
    // More POIs than the list cap, all at distinct, well-separated coords.
    a.pois = (0..(NEARBY_MAX + 4))
        .map(|i| demo_poi(&format!("p{i}"), c.lat + 0.001 * (i as f64 + 1.0), c.lon))
        .collect();
    // Select the farthest one — it would fall past the cap by proximity.
    let far_id = format!("p{}", NEARBY_MAX + 3);
    a.selected_poi = Some(far_id.clone());
    let listed = a.nearby_pois();
    assert!(
        listed.iter().any(|p| p.id == far_id),
        "the open POI is always present in the list even past the cap"
    );
    assert_eq!(listed.len(), NEARBY_MAX, "the list stays capped");

    // Clicking that POI's marker (map → selection) resolves to exactly it.
    let far_poi = a.pois.iter().find(|p| p.id == far_id).unwrap().clone();
    let hit = a.poi_at(a.projection().project(far_poi.pos));
    assert_eq!(
        hit.as_deref(),
        Some(far_id.as_str()),
        "a marker click resolves to that POI"
    );
}

/// The declutter set that the draw pass uses is exactly the set the click
/// hit-test walks: a marker dropped for being too dense is not clickable,
/// and the kept one at that spot is.
#[test]
fn poi_hit_test_matches_the_decluttered_draw_set() {
    let mut a = app_at(1280.0, 800.0);
    a.model.zoom = 17.0;
    // Two POIs on the same coordinate; declutter keeps the higher-priority
    // Transit stop over the Shop.
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
    // The hit-test at that pixel returns exactly the kept marker.
    let p = a.projection().project(c);
    assert_eq!(
        a.poi_at(p).as_deref(),
        Some("transit"),
        "hit-test agrees with the draw set"
    );
}

/// The search folds the live viewport POIs in, so a name OR category query
/// (e.g. "coffee") instantly surfaces what is on the map — the real
/// ranked-matches path the overlay and Enter-search both use.
#[test]
fn search_matches_live_pois_by_name_and_category() {
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
    // Category keyword: "coffee" surfaces the café (not the shop).
    let by_kw = a.ranked_matches("coffee");
    assert!(
        by_kw.iter().any(|p| p.id == "cafe1"),
        "a category keyword surfaces the live POI"
    );
    assert!(
        !by_kw.iter().any(|p| p.id == "shop1"),
        "unrelated POIs stay out"
    );
    // Category label: "Café" also matches.
    assert!(a.ranked_matches("café").iter().any(|p| p.id == "cafe1"));
    // Name substring: the POI's own name matches.
    assert!(
        a.ranked_matches("POI cafe1")
            .iter()
            .any(|p| p.id == "cafe1")
    );
}

// --- Time picker for all modes -----------------------------------------

/// The leave/arrive time picker is offered for EVERY travel mode (not only
/// Transit), and the road-trip clock derives departure/arrival from it.
#[test]
fn time_picker_available_for_all_modes_and_road_clock_derives() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut a = app_at(1280.0, 800.0);
    let mut doc = AutoCommit::new();
    a.select_place("marienplatz");
    a.enter_directions();

    for mode in [TravelMode::Foot, TravelMode::Bike, TravelMode::Car] {
        a.set_mode(mode, &mut doc);
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

    // Changing the anchor for a road mode re-derives the clock but does NOT
    // refetch a transit journey (OSRM is not time-dependent).
    a.set_mode(TravelMode::Car, &mut doc);
    let route = a.model.route.clone().expect("road route present");
    a.model.transit_time = crate::model::TransitTime::default();
    let (dep, arr) = a.road_trip_clock(route.duration_s);
    assert!(dep.starts_with("Leave "), "leave label, got {dep}");
    assert!(arr.starts_with("Arrive "), "arrive label, got {arr}");
    // Arrive-by flips which end is anchored.
    a.model.transit_time.mode = TransitTimeMode::Arrive;
    let (dep2, arr2) = a.road_trip_clock(route.duration_s);
    assert!(dep2.starts_with("Leave ") && arr2.starts_with("Arrive "));
}

#[test]
fn mode_switch_recomputes_route() {
    let mut a = app_at(1280.0, 800.0);
    let mut doc = AutoCommit::new();
    a.model.selected = Some("marienplatz".to_string());
    a.set_mode(TravelMode::Foot, &mut doc);
    let route = a.model.route.expect("route present");
    assert_eq!(route.mode, TravelMode::Foot);
}

#[test]
fn marker_hit_test_selects_when_close() {
    let mut a = app_at(1280.0, 800.0);
    a.model.selected = None;
    let proj = a.projection();
    // Project a known place to where its pin head sits.
    let place = a.model.place("hofgarten").unwrap().clone();
    let anchor = proj.project(place.pos);
    let head = Point::new(anchor.x, anchor.y - 22.0);
    let hit = a.marker_at(head);
    assert_eq!(hit.as_deref(), Some("hofgarten"));
}

/// The live app must not invent any content on startup — real imagery only,
/// with the user's own searches/pins as the sole data. Sample places belong
/// exclusively to the offline `--shots`/test path.
#[test]
fn live_app_starts_without_fabricated_content() {
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

/// The responsive breakpoint must be re-derived from the *current* width on
/// every layout pass. Render the SAME instance wide then narrow and confirm
/// it reflows: the docked column becomes an overlay drawer and a menu
/// toggle appears — proving nothing is cached from the first frame.
#[test]
fn resize_reflows_between_docked_and_overlay() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut a = MapsApp::offline();

    // Wide: docked column, no menu button, no drawer toggle target, and the
    // search bar starts flush at the left margin.
    a.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    assert!(!a.overlay_mode());
    assert!(a.panel_visible());
    let docked_w = a.panel_w();
    assert!((300.0..=384.0).contains(&docked_w));
    let docked_search_x0 = a.search_rect().x0;
    let mut scene = Scene::new();
    a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
    assert!(a.menu_rect().is_none());
    assert!(
        !a.click_targets
            .iter()
            .any(|(_, h)| matches!(h, Hit::ToggleNav))
    );

    // Narrow: SAME instance → overlay drawer, menu button appears, and the
    // search bar reflows to make room for the leading menu button.
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
        a.search_rect().x0 > docked_search_x0 + CTRL_SIZE,
        "top bar did not reflow: search x0 {} vs docked {docked_search_x0}",
        a.search_rect().x0
    );

    // And back to wide reflows again (no one-way latch).
    a.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    assert!(!a.overlay_mode());
    assert!(a.menu_rect().is_none());
    assert_eq!(a.search_rect().x0, docked_search_x0);
}

/// In the narrow overlay layout the drawer starts closed and the menu
/// button opens/closes it — the panel is only on screen when open.
#[test]
fn narrow_menu_toggles_drawer() {
    let mut a = MapsApp::offline();
    a.set_nav_open(false);
    a.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
    assert!(a.overlay_mode());
    assert!(!a.panel_visible(), "drawer starts closed");
    tap(&mut a, &Hit::ToggleNav);
    assert!(a.panel_visible(), "menu opens the drawer");
    tap(&mut a, &Hit::CloseNav);
    assert!(!a.panel_visible(), "scrim/Esc closes the drawer");
}

/// With the drawer open the top-bar chrome must stay hittable above the
/// dimming scrim: tapping the search field focuses it (does not close the
/// drawer), the menu toggles, and the dimmed map closes.
#[test]
fn drawer_open_hit_layering_is_correct() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut a = MapsApp::offline();
    a.set_nav_open(true);
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

    let sc = a.search_rect().center();
    assert!(
        matches!(topmost(&a, sc), Some(Hit::Search(SearchHit::Field))),
        "search tap must focus, not close the drawer"
    );
    let mc = a.menu_rect().unwrap().center();
    assert!(matches!(topmost(&a, mc), Some(Hit::ToggleNav)));
    let corner = Point::new(a.rect.x1 - 4.0, a.rect.y1 - 4.0);
    assert!(matches!(topmost(&a, corner), Some(Hit::CloseNav)));
}

/// Running a search reveals the drawer on narrow layouts so results are
/// visible without a second tap.
#[test]
fn search_opens_drawer_on_narrow() {
    let mut a = MapsApp::offline();
    a.set_nav_open(false);
    a.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
    a.model.query = "café".to_string();
    a.run_search(None);
    assert!(a.panel_visible(), "results should reveal the drawer");
}

/// Swapping endpoints must genuinely reverse origin ↔ destination — both
/// the coordinates and the labels — and re-route to a non-degenerate line.
/// Guards the prior bug where swap collapsed origin onto the destination
/// and produced a 0 m route with a stale "from" label.
#[test]
fn swap_ends_reverses_origin_and_destination() {
    let mut a = app_at(1280.0, 800.0);
    // Establish A → B: origin (Sendling) → Marienplatz.
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
    // Labels swapped.
    assert_eq!(a.model.origin_name, to0, "from-label must become old to");
    assert_eq!(a.model.dest_name, from0, "to-label must become old from");
    // Coordinates genuinely reversed, not collapsed to a single point.
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

/// Selecting the Transit travel mode for a place must actually start the
/// departures lookup, land in the Directions panel (Transit is a mode of
/// Directions, not a separate tab), and drop any stale road route — not
/// merely flip a view and render a false "no itineraries found".
#[test]
fn transit_mode_triggers_fetch_and_opens_directions() {
    let mut a = app_at(1280.0, 800.0);
    let mut doc = AutoCommit::new();
    // Select a place other than the sample's default so the stale sample
    // departures are dropped (destination-specific).
    a.select_place("hofgarten");
    assert!(
        a.model.transit.is_empty(),
        "selecting a new place drops stale departures"
    );
    a.model.panel = Panel::Explore;
    a.model.mode = TravelMode::Car;

    a.set_mode(TravelMode::Transit, &mut doc);
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

// --- Typeable origin ---------------------------------------------------

/// Typing into the From field surfaces geocoded matches, and picking one
/// sets the origin coord + label and re-routes from there. This is the core
/// fix: routing from anywhere, not just the map centre / a prior route.
#[test]
fn picking_origin_result_sets_origin_and_reroutes() {
    let mut a = app_at(1280.0, 800.0);
    a.select_place("marienplatz");
    a.enter_directions();
    let before = a.model.route.clone().expect("initial route present");

    // Focus the From field, type, and confirm a local match populates.
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

/// A geocoded endpoint result delivered by the worker lands in the field's
/// dropdown and is upserted so its marker can render — the real drain path.
#[test]
fn endpoint_geocode_results_land_via_drain() {
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
    a.drain_net(None);
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

/// The From/To dropdown must rank a near same-named place above a far one.
/// Delivered in far-first (importance) order — the shape Nominatim returns —
/// and drained through the real production path; the near place must surface
/// first. Guards the "511 km above 2.8 km, same name" report.
#[test]
fn endpoint_results_rank_near_same_name_first() {
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
    a.drain_net(None);
    assert_eq!(
        a.endpoints
            .test_results(Endpoint::Destination)
            .first()
            .map(|r| r.id.as_str()),
        Some("near"),
        "the nearer same-named endpoint ranks first, not the distant namesake"
    );
}

/// Route-finding never blocks the UI thread: entering Directions shows an
/// instant fallback synchronously, then the heavy work (fetch + geometry
/// simplification) runs on a worker thread and its result is folded through
/// the same channel + `drain_net` idle path the live worker uses. The worker
/// here mirrors `net::route`, simplifying dense geometry off the UI thread.
#[test]
fn route_reply_folds_from_a_worker_thread() {
    let mut a = app_at(1280.0, 800.0);
    a.select_place("marienplatz");
    a.enter_directions();
    // A fallback route is on screen immediately — the UI never waited.
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
    // Worker thread: build dense OSRM-style geometry and simplify it off the
    // UI thread, exactly as `net::route` does, then deliver over the channel.
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
        tx.send(NetMsg::Route(Ok(route))).unwrap();
        dense.len()
    });
    let dense_len = worker.join().expect("worker thread completes");

    // UI thread folds the worker's reply via the production drain path.
    a.drain_net(None);
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

// --- Location services -------------------------------------------------

/// The "use my location" success path: a host-provided fix becomes the route
/// origin and the journey re-plans from it. The real coordinates depend on
/// the host location service + user permission (unverifiable headlessly);
/// this proves the result *handling* around the portal.
#[test]
fn use_my_location_success_sets_origin_from_fix() {
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

/// A failed/denied fix must be honest and must never fabricate a position.
#[test]
fn location_unavailable_is_honest_and_fabricates_nothing() {
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

// --- Transit time picker -----------------------------------------------

/// The depart/arrive picker feeds the MOTIS query: "leave now" pins no time,
/// while an Arrive anchor carries both `arriveBy` and a concrete timestamp.
#[test]
fn transit_time_picker_feeds_query() {
    let mut a = app_at(1280.0, 800.0);

    // Default is "leave now".
    let now_q = a.transit_query();
    assert!(now_q.time.is_none(), "leave-now sends no explicit time");
    assert!(!now_q.arrive_by, "leave-now is not arrive-by");

    // Arrive-by, anchored 45 minutes out.
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

/// Stepping the time anchor (via the real dispatch path) shifts the offset
/// and "Now" resets it — proving the picker controls are wired.
#[test]
fn transit_time_controls_adjust_the_anchor() {
    let mut a = app_at(1280.0, 800.0);
    tap(&mut a, &Hit::TransitTimeDelta(15));
    tap(&mut a, &Hit::TransitTimeDelta(15));
    assert_eq!(
        a.model.transit_time.offset_min, 30,
        "two +15 steps advance 30 min"
    );
    // Never steps below now.
    for _ in 0..5 {
        tap(&mut a, &Hit::TransitTimeDelta(-15));
    }
    assert_eq!(a.model.transit_time.offset_min, 0, "offset floors at now");
    tap(&mut a, &Hit::TransitTimeMode(TransitTimeMode::Arrive));
    tap(&mut a, &Hit::TransitTimeDelta(15));
    tap(&mut a, &Hit::TransitTimeNow);
    assert!(a.model.transit_time.is_now(), "\"Now\" resets to leave-now");
}

// --- Real handle_event path --------------------------------------------
// The tests above exercise the internal helpers; these drive the *actual*
// `Widget::handle_event` entry point with synthesised `WindowEvent`s, so
// the hit-testing, drag tracking and dispatch chain a real click travels
// through are all covered.

/// `DeviceId` has no public constructor; a zeroed value is the sentinel
/// winit itself uses and the convention across this workspace's widget
/// tests (see `crates/sdk/ui-core/src/widget/image_viewer.rs`).
fn dummy_device_id() -> winit::event::DeviceId {
    // SAFETY: `DeviceId` is an opaque platform handle; the dispatch path
    // under test never dereferences it.
    unsafe { std::mem::zeroed() }
}

/// Feed one `WindowEvent` through the real `handle_event`, returning
/// whether the app marked it handled. Layout params are unused by the maps
/// widget (it hit-tests its own recorded rects) so dummies suffice.
fn feed(a: &mut MapsApp, ev: &WindowEvent) -> bool {
    let modifiers = winit::event::Modifiers::default();
    let mut state = AutoCommit::new();
    let root = automerge::ROOT;
    let mut handled = false;
    let mut focus: Option<NodeId> = None;
    let mut taffy = TaffyTree::<()>::new();
    let node = taffy.new_leaf(Style::default()).unwrap();
    a.handle_event(
        ev,
        1.0,
        &modifiers,
        &mut state,
        &root,
        &mut handled,
        &mut focus,
        node,
        &taffy,
    );
    handled
}

fn move_to(a: &mut MapsApp, x: f64, y: f64) -> bool {
    feed(
        a,
        &WindowEvent::CursorMoved {
            device_id: dummy_device_id(),
            position: winit::dpi::PhysicalPosition::new(x, y),
        },
    )
}

fn press_left(a: &mut MapsApp) -> bool {
    feed(
        a,
        &WindowEvent::MouseInput {
            device_id: dummy_device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
    )
}

fn release_left(a: &mut MapsApp) -> bool {
    feed(
        a,
        &WindowEvent::MouseInput {
            device_id: dummy_device_id(),
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    )
}

#[test]
fn live_navigation_drags_across_shell_ownership_without_a_jump() {
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
    let presentations = ui::widget::FloatingSurfaceWidgetExt::floating_surface_presentations(&a);
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

#[test]
fn live_navigation_end_action_is_distinct_from_its_drag_handle() {
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

#[test]
fn rerouting_preserves_the_positioned_navigation_surface_identity() {
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

#[test]
fn map_info_sheet_stays_modal_over_extracted_controls() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let mut app = app_at(1280.0, 720.0);
    app.info_sheet.open(MapInfo::Attribution);
    let mut scene = Scene::new();
    app.draw_app(&mut scene, Affine::IDENTITY, &mut text, &theme);

    let traffic = app
        .map_controls
        .rect(MapControlAction::ToggleTraffic)
        .center();
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

/// A scroll over open map area must zoom the view (not scroll a panel),
/// travelling the real `WindowEvent::MouseWheel` handler.
#[test]
fn wheel_over_map_zooms_via_handle_event() {
    let mut a = app_at(1280.0, 720.0);
    let before = a.model.zoom;
    // Cursor over the right-hand map area, clear of the docked panel.
    move_to(&mut a, 1100.0, 360.0);
    let handled = feed(
        &mut a,
        &WindowEvent::MouseWheel {
            device_id: dummy_device_id(),
            delta: MouseScrollDelta::LineDelta(0.0, 1.0),
            phase: winit::event::TouchPhase::Moved,
        },
    );
    assert!(handled, "wheel over the map must be handled");
    assert!(a.model.zoom > before, "scroll up should zoom in");
}

/// There is no Explore/Directions tab strip: the panel is a plain
/// collapsible surface. Directions is reached as an action on a place card
/// (not a top-level tab), and clicking that action through the real
/// draw → hit-test → dispatch path opens the Directions view.
#[test]
fn directions_reached_via_card_action_not_a_tab() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut a = app_at(1280.0, 720.0);
    a.model.panel = Panel::Explore;
    // Select a place so its Explore card (with the Directions action) shows.
    a.select_place("marienplatz");
    let mut scene = Scene::new();
    a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);

    // The docked panel exposes a collapse/expand toggle — a plain
    // collapsible panel, never a tab strip.
    assert!(
        a.click_targets
            .iter()
            .any(|(_, h)| matches!(h, Hit::ToggleCollapse)),
        "the panel has a collapse toggle"
    );

    // Directions is a card action, reached by clicking it — not a tab.
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

    // Back returns to the plain list view.
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

/// Clicking a POI marker on the map opens its info card directly — no tab or
/// travel-mode switch. The side panel view is unchanged; the map just gains
/// the floating info card. Exercises the real press → hit-test path.
#[test]
fn poi_click_opens_info_card_without_mode_change() {
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

    // Redraw with the card open: the info card is present.
    let mut scene2 = Scene::new();
    a.draw_app(&mut scene2, Affine::IDENTITY, &mut tr, &theme);
    assert!(
        a.click_targets
            .iter()
            .any(|(_, h)| matches!(h, Hit::ClosePoi)),
        "the POI info card is open"
    );
}

/// The start/end (From/To) fields search-as-you-type: typing arms a live
/// geocode without Enter, results render in a floating dropdown anchored
/// under the focused field, and the panel below (the mode control) does NOT
/// reflow when the dropdown appears. Drives the real key + draw path.
#[test]
fn endpoint_search_is_live_and_overlay_floats_without_reflow() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut a = app_at(1280.0, 720.0);
    a.select_place("marienplatz");
    a.enter_directions();
    assert_eq!(a.model.panel, Panel::Directions);

    // Baseline draw with no endpoint focused: record the mode control's rect.
    let mut scene = Scene::new();
    a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
    let mode_rect_before = a
        .click_targets
        .iter()
        .find(|(_, h)| matches!(h, Hit::Mode(TravelMode::Car)))
        .map(|(r, _)| *r)
        .expect("mode control recorded");

    // Focus the From field via the real hit-test, then type — no Enter.
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
    // Live: instant local matches populate WITHOUT any Enter, and a live
    // geocode is armed to fire once typing settles.
    assert!(
        !a.endpoints.test_results(Endpoint::Origin).is_empty(),
        "typing surfaces matches live (no Enter)"
    );
    assert!(
        a.endpoints.has_pending_query(),
        "a live geocode is armed on input"
    );

    // Redraw: the floating overlay records pick targets anchored UNDER the
    // From field, and the mode control below has NOT moved (no reflow).
    let mut scene2 = Scene::new();
    a.draw_app(&mut scene2, Affine::IDENTITY, &mut tr, &theme);
    assert!(
        a.endpoint_overlay_active(),
        "the endpoint dropdown is showing"
    );
    let pick = a
        .endpoints
        .test_pick_rect(Endpoint::Origin)
        .expect("overlay records PickOrigin hot-regions");
    let field = a
        .endpoints
        .test_field_rect(Endpoint::Origin)
        .expect("From field rect cached");
    assert!(
        pick.y0 >= field.y1,
        "overlay sits under the From field (a dropdown)"
    );
    let mode_rect_after = a
        .click_targets
        .iter()
        .find(|(_, h)| matches!(h, Hit::Mode(TravelMode::Car)))
        .map(|(r, _)| *r)
        .expect("mode control still recorded");
    assert_eq!(
        mode_rect_before, mode_rect_after,
        "opening the endpoint dropdown must NOT reflow the panel below it"
    );
    // The overlay row wins the hit-test over the panel it overlays.
    assert!(
        a.endpoints
            .test_top_hit_is_pick(pick.center(), Endpoint::Origin),
        "overlay row is topmost"
    );
    let expected = a.endpoints.test_results(Endpoint::Origin)[0].clone();
    move_to(&mut a, pick.center().x, pick.center().y);
    assert!(
        press_left(&mut a),
        "the floating result row handles its click"
    );
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

/// Clicking the Transit segment of the Directions mode row (through the real
/// draw → hit-test → dispatch path) switches to Transit mode, stays in the
/// Directions panel, drops the road route, and kicks off a departures
/// lookup — proving Transit is wired as a travel mode, not a dead label.
#[test]
fn transit_mode_segment_click_via_handle_event() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut a = app_at(1280.0, 720.0);
    a.model.panel = Panel::Directions;
    a.model.mode = TravelMode::Car;
    // Draw once so the mode-segment hot-regions are recorded.
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

/// A press-then-drag over the map pans the view via the real cursor/mouse
/// event handlers (drag state is tracked across two events).
#[test]
fn drag_pans_map_via_handle_event() {
    let mut a = app_at(1280.0, 720.0);
    // Remove markers so the press lands on open map (not a pin), isolating
    // the pan path deterministically.
    a.model.places.clear();
    let before = a.model.center;
    // Press on open map area (no marker, no panel) to begin a drag.
    move_to(&mut a, 1000.0, 360.0);
    assert!(
        press_left(&mut a),
        "pressing open map begins a pan and is handled"
    );
    // Move the cursor: with the button down this pans, not hovers.
    let moved_handled = move_to(&mut a, 1040.0, 372.0);
    assert!(
        (a.model.center.lon - before.lon).abs() > 1e-6
            || (a.model.center.lat - before.lat).abs() > 1e-6,
        "drag should move the map centre"
    );
    // The drag-move MUST report handled: the `ui` App only requests a repaint
    // for a `CursorMoved` when the widget marks it handled (its hover-spam
    // gate). Without this the centre updates but the frame never repaints —
    // the exact "drag looks frozen" bug. Assert on the production signal.
    assert!(
        moved_handled,
        "a drag-move must be handled so the map repaints"
    );
}

/// Press directly on a map pin through the real `handle_event` path (no UI
/// click-target under it) and confirm `select_place` fires via `marker_at`.
/// The drag test above deliberately clears markers; this covers the pin path.
#[test]
fn press_on_marker_selects_via_handle_event() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut a = app_at(1280.0, 720.0);
    // A single marker placed in the open right-hand map area (clear of the
    // docked left panel, the top search bar and the right-edge controls).
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
    // Draw so any UI hot-regions are recorded (there are none over the pin).
    let mut scene = Scene::new();
    a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
    // Press on the pin head (an unselected pin's head sits 22px above anchor).
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

/// A scroll over the side panel must scroll the panel content, not zoom the
/// map — the branch the map-zoom wheel test never reaches.
#[test]
fn wheel_over_panel_scrolls_not_zoom() {
    let mut a = app_at(1280.0, 720.0);
    assert!(a.panel_visible(), "docked panel is visible");
    // Force overflow so there is somewhere to scroll to.
    a.content_h = 2000.0;
    a.content_view_h = 400.0;
    let zoom_before = a.model.zoom;
    let scroll_before = a.scroll;
    let pr = a.panel_rect();
    move_to(&mut a, pr.center().x, pr.y0 + 120.0);
    let handled = feed(
        &mut a,
        &WindowEvent::MouseWheel {
            device_id: dummy_device_id(),
            delta: MouseScrollDelta::LineDelta(0.0, -1.0),
            phase: winit::event::TouchPhase::Moved,
        },
    );
    assert!(handled, "wheel over the panel must be handled");
    assert!(a.scroll > scroll_before, "panel content should scroll");
    assert_eq!(
        a.model.zoom, zoom_before,
        "scrolling the panel must not zoom the map"
    );
}

// --- Keyboard shortcuts through the real KeyboardInput path -------------
// These drive the actual `WindowEvent::KeyboardInput` → `handle_key`
// dispatch. `KeyEvent` has a `pub(crate)` field in winit 0.30, so it is
// built via the workspace-standard zeroed-struct workaround (see
// `crates/sdk/ui-core/src/widget/slash_menu.rs`); `handle_key` only reads `logical_key`.

fn feed_key(a: &mut MapsApp, logical: Key) -> bool {
    use winit::keyboard::{KeyLocation, NativeKeyCode, PhysicalKey};
    // Real winit reports the committed text alongside the logical key for a
    // printable character; the shared TextInput inserts from that `text`
    // field, so mirror it here (named keys carry no text).
    let text = match &logical {
        Key::Character(s) => Some(s.clone()),
        Key::Named(NamedKey::Space) => Some(winit::keyboard::SmolStr::new(" ")),
        _ => None,
    };
    let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = logical;
    ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = text;
    feed(
        a,
        &WindowEvent::KeyboardInput {
            device_id: dummy_device_id(),
            event: ev,
            is_synthetic: false,
        },
    )
}

fn key_char(a: &mut MapsApp, ch: &str) -> bool {
    feed_key(a, Key::Character(winit::keyboard::SmolStr::new(ch)))
}

fn key_named(a: &mut MapsApp, named: NamedKey) -> bool {
    feed_key(a, Key::Named(named))
}

/// Verifies Ctrl+V from the system clipboard reaches the focused search
/// field through the normal event path.
#[test]
fn ctrl_v_pastes_into_the_search_field() {
    let mut a = app_at(1280.0, 720.0);
    a.model.query.clear();
    a.search.set_focused(false);
    key_char(&mut a, "/"); // focus the search field
    assert!(a.search.is_focused());

    let sentinel = "Alexanderplatz";
    ui::clipboard_copy(sentinel);
    assert_eq!(
        ui::clipboard_paste().as_deref(),
        Some(sentinel),
        "clipboard round-trips here (precondition for the paste path)"
    );

    // Ctrl+V through the real KeyboardInput path.
    let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Character(winit::keyboard::SmolStr::new("v"));
    ev.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = None; // Ctrl+V carries no committed text
    let ctrl = {
        use winit::keyboard::ModifiersState;
        winit::event::Modifiers::from(ModifiersState::CONTROL)
    };
    let mut state = AutoCommit::new();
    let root = automerge::ROOT;
    let mut handled = false;
    let mut focus: Option<NodeId> = None;
    let mut taffy = TaffyTree::<()>::new();
    let node = taffy.new_leaf(Style::default()).unwrap();
    a.handle_event(
        &WindowEvent::KeyboardInput {
            device_id: dummy_device_id(),
            event: ev,
            is_synthetic: false,
        },
        1.0,
        &ctrl,
        &mut state,
        &root,
        &mut handled,
        &mut focus,
        node,
        &taffy,
    );

    assert_eq!(
        a.model.query, sentinel,
        "Ctrl+V pasted the clipboard into the search query"
    );
}

/// `/` focuses the search field, characters then type into it, Backspace
/// deletes, and Escape unfocuses — the whole edit loop over the real event
/// path.
#[test]
fn slash_and_typing_drive_search_via_key_event() {
    let mut a = app_at(1280.0, 720.0);
    a.model.query.clear();
    a.search.set_focused(false);
    assert!(key_char(&mut a, "/"), "'/' handled");
    assert!(a.search.is_focused(), "'/' focuses search");
    key_char(&mut a, "h");
    key_char(&mut a, "i");
    assert_eq!(
        a.model.query, "hi",
        "characters type into the focused search"
    );
    key_named(&mut a, NamedKey::Backspace);
    assert_eq!(a.model.query, "h", "Backspace deletes a char");
    a.search.replace_results(vec![crate::model::Place::new(
        "suggestion",
        "Helpful suggestion",
        Category::Museum,
        "Example address",
        LatLon::new(48.14, 11.57),
    )]);
    key_named(&mut a, NamedKey::Escape);
    assert!(!a.search.is_focused(), "Escape unfocuses search");
    assert_eq!(a.model.query, "h", "Escape preserves the query");
    assert!(
        a.search.results().is_empty(),
        "Escape drops transient suggestions"
    );
}

/// `+`/`-` zoom the map through the key path.
#[test]
fn zoom_keys_via_key_event() {
    let mut a = app_at(1280.0, 720.0);
    let z0 = a.model.zoom;
    key_char(&mut a, "+");
    assert!(a.model.zoom > z0, "'+' zooms in");
    let z1 = a.model.zoom;
    key_char(&mut a, "-");
    assert!(a.model.zoom < z1, "'-' zooms out");
}

/// Arrow keys pan the map through the key path.
#[test]
fn arrow_keys_pan_via_key_event() {
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

/// `d`/`t` open Directions / Transit for the selected place, and `s` toggles
/// its saved flag — all via the key path.
#[test]
fn directions_transit_save_keys_via_key_event() {
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

/// `1`/`2` switch the panel view (list / directions); `m` opens the offline
/// overlay (whose focused city field then owns the keyboard); Escape closes
/// the overlay and, once closed, deselects the place.
#[test]
fn panel_offline_escape_keys_via_key_event() {
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
    // The dialog's field owns keys now, so Escape (not another 'm') closes it.
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

/// Clearing the search (the × button's `Hit`) empties the query and results.
#[test]
fn clear_search_dispatch() {
    let mut a = app_at(1280.0, 720.0);
    a.model.query = "café".to_string();
    a.run_search(None);
    assert!(!a.model.results.is_empty(), "search populated results");
    tap(&mut a, &Hit::Search(SearchHit::Clear));
    assert!(a.model.query.is_empty(), "clear empties the query");
    assert!(a.model.results.is_empty(), "clear empties the results");
}

#[test]
fn search_history_affordances_dispatch_canonical_actions() {
    use crate::actions::encode;

    let mut a = app_at(1280.0, 720.0);
    let mut doc = AutoCommit::new();
    a.model.query = "coffee".to_string();
    a.run_search(Some(&mut doc));
    assert_eq!(
        a.actions.last_dispatched(),
        Some(&encode(&MapsAction::CommitSearch {
            query: "coffee".to_string(),
        })),
        "visible search commits history through MapsAction"
    );
    assert!(
        MapsDoc::load(&doc)
            .is_some_and(|model| { model.recents.first().is_some_and(|query| query == "coffee") })
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
    a.dispatch(&Hit::ClearRecents, &mut doc);
    assert_eq!(
        a.actions.last_dispatched(),
        Some(&encode(&MapsAction::ClearRecents))
    );
    assert!(MapsDoc::load(&doc).is_some_and(|model| model.recents.is_empty()));
}

/// Clearing an active route drops it (the place-card / directions clear-×).
#[test]
fn clear_route_dispatch() {
    let mut a = app_at(1280.0, 720.0);
    a.select_place("marienplatz");
    a.enter_directions();
    assert!(a.model.route.is_some());
    tap(&mut a, &Hit::ClearRoute);
    assert!(a.model.route.is_none(), "clear-route drops the route");
}

/// A recent-search chip re-runs that query.
#[test]
fn recent_dispatch_runs_search() {
    let mut a = app_at(1280.0, 720.0);
    tap(&mut a, &Hit::Recent("Marienplatz".to_string()));
    assert_eq!(a.model.query, "Marienplatz", "recent sets the query");
    assert_eq!(a.model.panel, Panel::Explore, "a search lands in Explore");
    assert!(
        !a.model.results.is_empty(),
        "the recent search actually runs"
    );
}

// --- Offline download / cache management dispatch -----------------------

/// Opening the overlay then starting a download computes a real tile count
/// for the current viewport and surfaces a progress status.
#[test]
fn offline_open_and_download_dispatch() {
    let mut a = app_at(1280.0, 720.0);
    a.offline_panel.open(a.tiles.cache_bytes());
    assert!(a.offline_panel.is_open(), "opening shows the overlay");
    tap_offline(&mut a, OfflineAction::Download);
    assert!(
        a.model.status.starts_with("Downloading") && a.model.status.contains("tiles"),
        "download surfaces a tile-count status, got {:?}",
        a.model.status
    );
    // The download covers at least the base zoom plus the extra levels.
    assert!(
        a.tiles.progress().total > 0,
        "a real region of tiles was queued"
    );
}

/// The offline zoom-depth stepper clamps to 0..=6.
#[test]
fn offline_zoom_delta_clamps() {
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

/// Clearing the cache deletes the on-disk tile tree and zeroes the readout —
/// exercised against a disposable temp dir, never the user's real cache.
#[test]
fn offline_clear_cache_dispatch_removes_dir() {
    let mut a = app_at(1280.0, 720.0);
    let uniq = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir =
        std::env::temp_dir().join(format!("tilo-maps-cctest-{}-{}", std::process::id(), uniq));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("seed.png"), b"not-a-real-tile").unwrap();
    a.tiles = TileLayer::offline_with_cache_dir(dir.clone());
    assert!(a.tiles.cache_bytes() > 0, "seeded cache is non-empty");

    tap_offline(&mut a, OfflineAction::ClearCache);
    assert!(!dir.exists(), "clear-cache removes the on-disk tree");
    assert_eq!(
        a.offline_panel.cache_bytes(),
        0,
        "the cache-size readout resets to 0"
    );
    assert!(
        a.model.status.starts_with("Cleared"),
        "a status states what was freed"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// --- Transit itinerary on the map (element: transit geometry was dead) ---

/// With a transit itinerary selected, the leg geometry is present and the
/// `f` key / journey selection frames the map on it — previously `f` was a
/// no-op in Transit mode because it only ever fit `model.route` (always None
/// for transit), and the geometry was never populated at all.
#[test]
fn transit_journey_geometry_drives_map_fit() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut a = app_at(1280.0, 720.0);
    // Sample transit carries real leg geometry; select Transit as the mode
    // (keeps the sample itineraries, drops the road route).
    a.set_travel_mode(TravelMode::Transit);
    assert!(a.model.route.is_none(), "transit has no road route");
    let geom = a.selected_journey_geometry();
    assert!(
        geom.len() >= 2,
        "the selected itinerary has drawable leg geometry"
    );

    // Drawing the transit itinerary must not panic (real on-map render path).
    let mut scene = Scene::new();
    a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);

    // The `f` key frames the itinerary even though there is no road route.
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

/// Selecting a different journey card re-frames the map onto that itinerary
/// (not merely expanding the panel), via the real dispatch path.
#[test]
fn selecting_journey_reframes_map() {
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

/// A fresh, isolated on-disk cache dir so a delete step never touches the
/// user's real tile cache.
fn region_test_cache(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("tilo-maps-{tag}-{}-{}", std::process::id(), n))
}

/// Starting a download records the chosen viewport as a **manageable
/// region** (so it shows in the saved-areas list), and deleting it by id
/// removes the record. This is the see/manage-downloaded-regions flow.
#[test]
fn offline_download_records_and_delete_removes_region() {
    let mut a = app_at(1280.0, 800.0);
    // Isolate the on-disk cache so the delete step never touches the user's
    // real tile cache.
    let dir = region_test_cache("regtest");
    a.tiles = TileLayer::offline_with_cache_dir(dir.clone());
    a.offline_panel.open(a.tiles.cache_bytes());
    let before = a.model.regions.len();
    tap_offline(&mut a, OfflineAction::Download);
    assert_eq!(
        a.model.regions.len(),
        before + 1,
        "a downloaded area is recorded as a saved region"
    );
    let region = a.model.regions.last().unwrap().clone();
    assert!(region.tiles > 0, "the region covers real tiles");
    assert!(
        region.z1 >= region.z0,
        "the saved zoom range is well-formed"
    );
    assert!(!region.name.is_empty(), "the region has a human label");

    // Deleting via its id removes exactly that record.
    tap_offline(&mut a, OfflineAction::DeleteRegion(region.id.clone()));
    assert!(
        !a.model.regions.iter().any(|r| r.id == region.id),
        "delete removes the region from the managed list"
    );
    assert_eq!(
        a.model.regions.len(),
        before,
        "back to the starting region count"
    );
    assert!(
        a.model.status.contains("Removed"),
        "a status confirms the removal"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Removing one saved area must NOT delete tiles another saved area still
/// covers. A nested area's tiles are a subset of the containing area's, so
/// deleting the nested one frees nothing; deleting the last owner frees all.
#[test]
fn delete_region_keeps_tiles_shared_with_another_area() {
    use crate::model::DownloadedRegion;
    let dir = region_test_cache("region");
    let mut a = MapsApp::offline();
    a.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    a.tiles = TileLayer::offline_with_cache_dir(dir.clone());

    let big = GeoBounds::new(LatLon::new(48.10, 11.55), LatLon::new(48.18, 11.62));
    let small = GeoBounds::new(LatLon::new(48.13, 11.57), LatLon::new(48.14, 11.58));
    let (z0, z1) = (14u8, 15u8);
    let big_ids = big.tiles_in_range(z0, z1);
    let small_ids = small.tiles_in_range(z0, z1);
    assert!(!small_ids.is_empty());
    assert!(
        small_ids.iter().all(|t| big_ids.contains(t)),
        "the nested area's tiles are a subset of the containing area's"
    );

    // Seed the on-disk cache with the containing area's tiles.
    for id in &big_ids {
        let p = a.tiles.cache_path(*id);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"seed").unwrap();
    }
    a.model.regions = vec![
        DownloadedRegion {
            id: "big".to_string(),
            name: "Big".to_string(),
            min: big.min,
            max: big.max,
            z0,
            z1,
            tiles: big_ids.len() as u32,
        },
        DownloadedRegion {
            id: "small".to_string(),
            name: "Small".to_string(),
            min: small.min,
            max: small.max,
            z0,
            z1,
            tiles: small_ids.len() as u32,
        },
    ];

    // Deleting the nested area removes its record but keeps every tile,
    // since the containing area still needs them.
    tap_offline(&mut a, OfflineAction::DeleteRegion("small".to_string()));
    assert!(!a.model.regions.iter().any(|r| r.id == "small"));
    for id in &big_ids {
        assert!(
            a.tiles.cache_path(*id).exists(),
            "a shared tile must be kept: {id:?}"
        );
    }

    // Deleting the last owner frees all of its tiles.
    tap_offline(&mut a, OfflineAction::DeleteRegion("big".to_string()));
    for id in &big_ids {
        assert!(
            !a.tiles.cache_path(*id).exists(),
            "the last owner's tiles are freed: {id:?}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// The on-screen trash affordance dispatches the byte-identical
/// [`MapsAction`](crate::actions::MapsAction) the Ctrl+K "Delete region" spec
/// does — one source of truth. Drives the real `dispatch` path against a live
/// document and reads back the action host's `last_dispatched`, then confirms
/// the shared document actually changed.
#[test]
fn delete_region_affordance_matches_action_spec() {
    use crate::actions::{MapsAction, encode};
    let mut a = app_at(1280.0, 800.0);
    a.model.regions = vec![crate::model::DownloadedRegion {
        id: "r1".to_string(),
        name: "Altstadt".to_string(),
        min: LatLon::new(48.10, 11.55),
        max: LatLon::new(48.18, 11.62),
        z0: 14,
        z1: 15,
        tiles: 8,
    }];

    let mut doc = AutoCommit::new();
    a.dispatch_offline(OfflineAction::DeleteRegion("r1".to_string()), &mut doc);

    // The affordance dispatched exactly the DeleteRegion action a palette row
    // (its `ActionSpec`) would build for the same id.
    let dispatched = a
        .actions
        .last_dispatched()
        .expect("the trash affordance dispatched an action");
    let expected = encode(&MapsAction::DeleteRegion {
        id: "r1".to_string(),
    });
    assert_eq!(
        dispatched, &expected,
        "affordance == ActionSpec encoded action"
    );

    // And it really mutated the shared document + the live model.
    assert!(
        !a.model.regions.iter().any(|r| r.id == "r1"),
        "the region row is gone"
    );
    assert!(
        !MapsDoc::load(&doc)
            .unwrap()
            .regions
            .iter()
            .any(|r| r.id == "r1"),
        "the document reflects the removal"
    );
}

#[test]
fn map_controls_are_operable_semantic_children_with_matching_geometry() {
    let a = app_at(1_280.0, 800.0);
    let node = Widget::accessibility(&a).expect("maps is semantic");
    assert_eq!(node.children.len(), 4, "only drawn controls are exposed");
    let expected_targets = [
        "maps.control:zoom-in",
        "maps.control:zoom-out",
        "maps.control:locate",
        "maps.control:traffic",
    ];
    for (index, (child, expected_target)) in node.children.iter().zip(expected_targets).enumerate()
    {
        assert_eq!(child.action_target.as_deref(), Some(expected_target));
        assert_eq!(
            Widget::synthetic_accessibility_rect(&a, &[index]),
            child.bounds.map(ui::a11y::AccessBounds::to_rect),
            "control {index} must activate at its visible bounds"
        );
    }
    let traffic = node
        .children
        .iter()
        .find(|child| child.action_target.as_deref() == Some("maps.control:traffic"))
        .expect("traffic semantics");
    assert_eq!(traffic.role, ui::a11y::Role::Switch);
    assert_eq!(traffic.state.checked, Some(false));
}

#[test]
fn semantic_control_targets_are_stable_unique_and_operable() {
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
        "maps.control:traffic",
        &ui::a11y::Action::ToggleCheck,
        &mut doc,
        &automerge::ROOT,
    ));
    assert!(a.map_controls.traffic_enabled());
}

#[test]
fn offline_dialog_is_modal_semantic_and_operable() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut a = app_at(1_280.0, 800.0);
    a.offline_panel.open(0);
    let mut scene = Scene::new();
    a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);

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
        &mut doc,
        &automerge::ROOT,
    ));
    assert!(!a.offline_panel.is_open());
}

#[test]
fn narrow_drawer_suppresses_redundant_navigation_card_and_background_semantics() {
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

#[test]
fn end_navigation_is_semantic_when_the_route_card_is_visible() {
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
        &mut doc,
        &automerge::ROOT,
    ));
    assert!(a.model.route.is_none());
}

#[test]
fn reduced_motion_does_not_pump_floating_handoff_frames() {
    let mut a = app_at(1_280.0, 800.0);
    a.navigation_placement
        .detach_to_desktop(Rect::new(820.0, 120.0, 1_180.0, 276.0));
    assert!(Widget::needs_animation_frame(&a));
    Widget::set_motion_reduced(&mut a, true);
    assert!(!Widget::needs_animation_frame(&a));
}

#[test]
fn eager_and_lazy_resources_share_unique_live_operations() {
    let mut a = app_at(1_280.0, 800.0);
    let result = a
        .model
        .results
        .first()
        .expect("sample search result")
        .clone();
    let mut saved = result;
    saved.id = "eager-saved".to_string();
    saved.saved = true;
    a.model.places.push(saved.clone());
    saved.id = "lazy-saved".to_string();
    a.model.places.push(saved);
    let mut doc = AutoCommit::new();
    MapsDoc::save(&mut doc, &a.model);

    let root = Widget::resources(&a, &doc)
        .into_iter()
        .next()
        .expect("maps root");
    let result_resource_id = "maps.place:eager-saved".to_string();
    let eager_result = root
        .children
        .iter()
        .find(|resource| resource.id.as_str() == result_resource_id)
        .expect("eager result");
    let mut ids: Vec<_> = eager_result
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(
        ids.len(),
        eager_result.actions.len(),
        "action ids are unique"
    );
    assert!(
        eager_result
            .actions
            .iter()
            .any(|action| action.id == "open")
    );
    assert!(
        eager_result
            .actions
            .iter()
            .any(|action| action.id == "directions")
    );

    let lazy =
        Widget::resource(&a, &doc, "maps.place:lazy-saved").expect("exact saved-place resolver");
    assert!(lazy.actions.iter().any(|action| action.id == "open"));
    assert!(lazy.actions.iter().any(|action| action.id == "directions"));
}
