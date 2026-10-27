//! The map's life: what it is built from, the tile cache it reads, the menus and panels it opens, and the durable resource it attaches to and adopts.

use super::*;

impl MapsApp {
    /// Live app: spawns the background tile worker (network + decode off the
    /// UI thread) and starts from an empty model — real OSM imagery, no
    /// fabricated places. The user's own searches, routes and saved pins are
    /// the only content; nothing is invented on startup.
    pub fn new() -> Self {
        // Empty model + empty vector basemap: before the first real tiles
        // arrive the map shows a plain themed land colour (an honest loading
        // state) rather than an invented street grid.
        Self::build(
            MapModel::default(),
            Basemap::default(),
            TileLayer::new(),
            false,
            true,
        )
    }

    /// A capture of `sample`: no worker thread, no network, the bundled
    /// sample's basemap standing in for raster imagery. The scenario's
    /// places and plans arrive as the resource's state.
    pub(crate) fn sample(sample: crate::scenario::Sample) -> Self {
        match sample {
            crate::scenario::Sample::City => Self::offline(),
            crate::scenario::Sample::Empty => Self::offline_empty(),
        }
    }

    /// A past version in History mode: drawn from the version's places and
    /// plans over a plain basemap, without a tile worker or the network.
    pub(crate) fn past() -> Self {
        Self::build(
            MapModel::default(),
            Basemap::default(),
            TileLayer::offline(),
            false,
            false,
        )
    }

    /// Headless/offline variant used by captures and tests: no worker thread,
    /// no network. Renders the bundled sample city (places, a route, transit
    /// options) and the stylised vector basemap so the screenshot is fully
    /// populated without ever touching the wire. The overlay drawer starts
    /// open so narrow screenshots show the panel content.
    pub(crate) fn offline() -> Self {
        Self::build(
            sample::default_model(),
            sample::basemap(),
            TileLayer::offline(),
            true,
            false,
        )
    }

    pub(super) fn build(
        model: MapModel,
        basemap: Basemap,
        tiles: TileLayer,
        nav_open: bool,
        online: bool,
    ) -> Self {
        let (tx, net_rx) = std::sync::mpsc::channel();
        let net_tx = NetSender {
            tx,
            wake: ui::wake::WakeHandle::new(),
        };
        let cache_bytes = tiles.cache_bytes();
        let nav_card_size = navigation_card_size(&Theme::dark());
        Self {
            model,
            basemap,
            rect: Rect::ZERO,
            node: None,
            dispatch: None,
            offline_areas: None,
            last_mouse: Point::ZERO,
            segmented_touch: SegmentedTouch::default(),
            map_controls: MapControls::default(),
            place_search: PlaceSearch::default(),
            search_opener: ui::search::SearchOpener::new(),
            history: ui::widget::HistoryOpener::new(),
            results_anchor: None,
            chrome_rects: Vec::new(),
            status_notice: ui::widget::toast::Transient::new(),
            status_said: String::new(),
            endpoints: EndpointEditor::new(),
            left_down: false,
            dragging: false,
            drag_last: Point::ZERO,
            drag_moved: false,
            click_targets: Vec::new(),
            text_scale: 1.0,
            ui_scale: 1.0,
            spacing: Spacing::default(),
            metrics: Metrics::BASE,
            pending_fit: false,
            pending_focus: None,
            pending_poi_focus: false,
            poi_card_rect: Rect::ZERO,
            nav_card_size,
            panel_edges: Vec::new(),
            panel_origin: 0.0,
            scroll: 0.0,
            content_h: 0.0,
            content_view_h: 0.0,
            net_tx,
            net_rx,
            pending: 0,
            search_asked: None,
            online,
            location_rx: None,
            locating: false,
            location_reply: None,
            tiles,
            overflow: {
                let mut m = ContextMenu::new();
                // Indices are mapped in `dispatch` via `take_selection`.
                m.push(MenuItem::action("Attribution", || {}));
                m.push(MenuItem::action("Offline maps", || {}));
                m.push(MenuItem::action("Licenses", || {}));
                m
            },
            menu_for: MapMenu::More,
            info_sheet: MapInfoSheet::default(),
            offline_panel: OfflinePanel::new(cache_bytes),
            nav_open,
            pois: Vec::new(),
            selected_poi: None,
            poi_debounce: None,
            poi_fetched_bounds: None,
            poi_generation: 0,
            panel_collapsed: false,
            traffic: TrafficData::default(),
            pending_floating: None,
            navigation_placement: ui::floating_surface::FloatingPlacement::attached(nav_card_size),
            navigation_drag_handle: Rect::ZERO,
            navigation_end_rect: Rect::ZERO,
            navigation_end_hovered: false,
            motion_reduced: false,
            floating_surface_bounds: Rect::ZERO,
        }
    }

    /// Open the overflow menu at its button; the toolkit flips it up from
    /// the bottom corner.
    pub(super) fn open_overflow(&mut self) {
        self.menu_for = MapMenu::More;
        self.overflow.set_items(vec![
            MenuItem::action("Attribution", || {}),
            MenuItem::action("Offline maps", || {}),
            MenuItem::action("Licenses", || {}),
        ]);
        self.overflow.open_anchored(self.overflow_rect(), self.rect);
    }

    /// The map's layers, each checked while it shows, under the layers
    /// button. Traffic is the one layer the map draws over its basemap.
    pub(super) fn open_layers(&mut self) {
        self.menu_for = MapMenu::Layers;
        self.overflow.set_items(vec![
            MenuItem::action("Traffic", || {}).with_checked(self.map_controls.traffic_enabled()),
        ]);
        self.overflow
            .open_anchored(self.map_controls.rect(MapControlAction::Layers), self.rect);
    }

    /// Whether the menu of `which` is open.
    pub(super) fn menu_open(&self, which: MapMenu) -> bool {
        self.overflow.is_open() && self.menu_for == which
    }

    /// An offline (no-worker, no-network) app that mirrors the live first-run
    /// state — an empty model and a plain themed basemap — so the empty
    /// Explore hint can be captured hermetically.
    pub(crate) fn offline_empty() -> Self {
        Self::build(
            MapModel::default(),
            Basemap::default(),
            TileLayer::offline(),
            true,
            false,
        )
    }

    /// A fully live app (real tile worker, real network, `online == true`)
    /// whose on-disk tile cache is rooted at `cache_dir`: the app's cache
    /// area in a live window, a clean directory in a live integration test
    /// that inspects exactly the PNGs the worker writes.
    pub fn new_with_cache_dir(cache_dir: std::path::PathBuf) -> Self {
        Self::build(
            MapModel::default(),
            Basemap::default(),
            // Rooted here like the real `new()`; the active theme's style is
            // chosen on the first draw.
            TileLayer::live_with_root(cache_dir),
            false,
            true,
        )
    }

    // --- live-verify accessors ---------------------------------------------
    // Read-only windows onto the tile layer for the live-render harness and
    // the full-plumbing integration test. They assert on the *production*
    // request→worker→fetch→decode→drain path, not a mock.

    /// Decoded OSM raster tiles resident in memory. `> 0` proves the live path
    /// fetched AND decoded real imagery (the thing the mock/sample tests never
    /// exercised).
    pub fn tiles_mem_len(&self) -> usize {
        self.tiles.mem_len()
    }

    /// Whether any tile fetch failed and is in the negative-cache backoff —
    /// i.e. imagery is unreachable (egress blocked / offline).
    pub fn tiles_has_failures(&self) -> bool {
        self.tiles.has_failures()
    }

    /// Why tile fetches are failing, if they are: a refusing server
    /// ([`TileFault::Rejected`]) or unreachable imagery.
    pub fn tiles_failure(&self) -> Option<TileFault> {
        self.tiles.failure()
    }

    /// Whether any tile request is still awaiting the background worker.
    pub fn tiles_has_inflight(&self) -> bool {
        self.tiles.has_inflight()
    }

    /// On-disk tile cache root (`z/x/y.png` beneath it) — so a test can prove
    /// real PNGs were persisted.
    pub fn tiles_cache_dir(&self) -> &std::path::Path {
        self.tiles.cache_dir()
    }

    /// Live-verify hook: jump the camera to `zoom` (clamped), leaving the centre
    /// put, so the render harness can capture a *mid-zoom* frame — the
    /// target-level tiles not yet fetched — and prove the map reuses the
    /// resident tiles scaled under an affine instead of blanking.
    pub fn set_zoom_for_verify(&mut self, zoom: f64) {
        self.model.zoom = zoom.clamp(3.0, 19.0);
    }

    /// Live-verify read-back of the current camera zoom.
    pub fn zoom_for_verify(&self) -> f64 {
        self.model.zoom
    }

    /// Test-only: seed the resident tile store with solid tiles covering the
    /// current viewport across a few zoom levels, so a perf scenario measures
    /// the *loaded* raster path (exact tiles + ancestor/descendant reuse under
    /// zoom) instead of the empty first-frame substrate. No worker, no network.
    #[cfg(test)]
    pub(crate) fn seed_perf_tiles(&mut self, w: f64, h: f64) {
        self.rect = Rect::new(0.0, 0.0, w, h);
        let vc = self.view_center();
        let base_z = TileLayer::tile_zoom(self.model.zoom);
        // Seed one level below through one level above so both zoom-in
        // (ancestors) and zoom-out (descendants) reuse resident imagery.
        for z in base_z.saturating_sub(1)..=(base_z + 1).min(19) {
            let zoom = f64::from(z);
            for (id, _) in TileLayer::visible_tiles(self.model.center, zoom, vc, self.rect) {
                self.tiles.insert_solid_tile(id);
            }
        }
    }

    /// Show the places, searches and plans the framework resource holds.
    pub(crate) fn adopt_resource_state(&mut self, state: &Workspace) {
        let estimate = self.model.route.take();
        state.show_in(&mut self.model);
        // A mode edit can arrive before its planner response. Keep the local
        // estimate for those same endpoints, not the previous mode's accepted
        // route. Clearing the accepted plan must still remove the estimate.
        if self.model.route.is_none()
            && !state.mode.is_transit()
            && self.pending > 0
            && state.route().is_some()
            && let Some(route) = estimate.filter(|route| {
                route.mode == state.mode
                    && Some(route.from) == self.model.origin
                    && Some(route.to) == self.model.dest
            })
        {
            self.model.route = Some(route);
        }
    }

    /// Hosted as the resource's surface: edits go through `dispatch`, and the
    /// offline areas are this device's `offline_areas`.
    pub(crate) fn attach_framework(
        &mut self,
        dispatch: ui::framework::Dispatcher<Workspace>,
        offline_areas: Option<ui::framework::Setting<crate::offline_areas::OfflineAreas>>,
    ) {
        self.dispatch = Some(dispatch);
        self.offline_areas = offline_areas;
        self.follow_offline_areas();
    }

    /// Toggle the traffic overlay. Turning it on triggers a refresh so the
    /// current congestion picture is loaded (or the honest empty state is set).
    pub(super) fn set_traffic(&mut self, on: bool) {
        self.map_controls.set_traffic_enabled(on);
        if on {
            self.refresh_traffic();
        }
    }

    /// Populate the traffic overlay.
    ///
    /// Tilo traffic is a crowdsourced, peer-shared layer (see `docs/architecture/product.md`);
    /// no such live feed is wired yet, so the LIVE app has no reports and the
    /// overlay stays empty — the map then shows "No traffic data" rather than
    /// inventing congestion. When the bundled offline cartography is loaded
    /// (the `--shots` / sample path, distinguished by a non-empty vector
    /// basemap), we colour its arterials from the sample congestion set so the
    /// overlay is demonstrable — the same sample-only gating every other piece
    /// of this app's offline data already uses. Nothing is fabricated on the
    /// live path.
    pub(super) fn refresh_traffic(&mut self) {
        if self.basemap.is_empty() {
            self.traffic = TrafficData::default();
        } else {
            self.traffic = TrafficData::sample(&self.basemap);
        }
    }

    /// Submit one verb or field edit of the shared map through the
    /// framework's door. Callers show its result in this window right away;
    /// the door checks, applies, records and replicates it, and the state it
    /// stores is presented back, so an edit the door refuses is undone here
    /// too.
    pub(super) fn perform(&self, submit: impl ui::framework::Submit<Workspace>) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.submit(submit);
        }
    }

    /// Choose how directions travel, here and for everyone.
    pub(super) fn choose_mode(&mut self, mode: TravelMode) {
        self.model.mode = mode;
        self.perform(Workspace::paths().mode.set(mode));
    }

    /// Anchor the transit planner at `value`, here and for everyone.
    pub(super) fn set_transit_time(&mut self, value: crate::model::TransitTime) {
        self.model.transit_time = value;
        self.perform(Workspace::paths().transit_time.set(value));
    }

    pub(super) fn toggle_save_selected(&mut self) {
        if let Some(id) = self.model.selected.clone() {
            // Resolve the target state here (authoring time) so the action carries
            // an explicit `saved`, never a read-and-toggle that could diverge on
            // replay.
            let now_saved = self.model.place(&id).is_some_and(|p| p.saved);
            let saved = !now_saved;
            if self.model.places.iter().any(|place| place.id == id) {
                self.perform(Workspace::save(&id, saved));
            } else if let Some(found) = self.model.results.iter().find(|place| place.id == id) {
                // A search result becomes a place of the shared map as it is
                // saved.
                let place = Place {
                    saved,
                    ..found.clone()
                };
                self.accept_places(vec![place]);
            }
            // The selection is local; reflect the explicit action payload in
            // this window even when the selected search result is not yet a
            // durable bookmark row.
            if let Some(place) = self.model.places.iter_mut().find(|place| place.id == id) {
                place.saved = saved;
            }
        }
    }
}
