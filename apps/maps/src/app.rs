//! Root widget for the Maps app.
//!
//! `MapsApp` owns the in-memory [`MapModel`] (mirrored into the CRDT every
//! frame via [`crate::doc::MapsDoc`]) and the static vector [`Basemap`]. It
//! is a single full-window custom widget: the map is full-bleed and the UI
//! (search bar, side panel, zoom controls, place card) floats on top, drawn
//! straight into the Vello scene with theme-derived colours. Interaction —
//! pan, zoom-to-cursor, marker hit-testing, panel buttons, search typing —
//! is handled directly in `handle_event`.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;
use ui::widget::{ContextMenu, MenuItem};

use crate::actions::{MAPS_ACTION_TYPE, MapsAction};
use crate::canvas::{self, MapPalette};
use crate::doc::MapsDoc;
use crate::draw;
use crate::geo::{self, Projection};
use crate::location::{self, LocationChannel, LocationOutcome};
use crate::map_controls::{MapControlAction, MapControls};
use crate::model::{Category, LatLon, MapModel, Panel, TransitMode, TransitTimeMode, TravelMode};
use crate::net::{self, NetMsg};
use crate::offline_panel::{OfflineAction, OfflineFrame, OfflinePanel};
use crate::sample::{self, Basemap};
use crate::search_box::{
    SearchBox, SearchEffect, SearchHit, SearchKeyEffect, SearchOverlay, drive_text_input,
};
use crate::tiles::{GeoBounds, TileId, TileLayer, TileStyle};
use crate::traffic::{Congestion, TrafficData};
use ui::widget::TextInput;

/// Which Directions endpoint field (From / To) currently has keyboard focus for
/// typed geocoding. `None` when neither is being edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EndField {
    Origin,
    Dest,
}

/// Which map-data info sheet is open (reached from the ⋯ overflow menu). The
/// attribution + licences the OSM/CARTO data require live here, one tap away,
/// rather than pasted permanently over the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MapInfo {
    Attribution,
    Licenses,
}

// --- Layout metrics (logical px) -------------------------------------------
const MARGIN: f64 = 16.0;
const SEARCH_H: f64 = 48.0;
const GAP: f64 = 10.0;
/// Height of the side panel's header bar (back / title / collapse toggle).
const PANEL_HEADER_H: f64 = 40.0;
const CTRL_SIZE: f64 = crate::map_controls::SIZE;
const ROW_H: f64 = 56.0;
/// Below this window width the side panel stops being docked and becomes a
/// closed overlay drawer that the user opens over the map (Adwaita flap).
const NAV_OVERLAY_BELOW: f64 = 720.0;
/// Widest the panel is ever drawn (docked, or as an overlay drawer).
const PANEL_MAX_W: f64 = 384.0;
const PANEL_MIN_W: f64 = 300.0;
const ENDPOINT_SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(280);
const ENDPOINT_OVERLAY_MAX: usize = 6;
/// Idle gap after the last pan/zoom before refetching viewport POIs (Overpass).
const POI_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(450);
/// Only fetch POIs at city-block scale and closer — a whole-country viewport
/// would return an unbounded, meaningless marker set.
const POI_MIN_ZOOM: f64 = 14.0;
/// Cap on POI markers drawn so a dense area stays legible.
const POI_MAX: usize = 140;
/// How many nearby POIs the Explore panel lists (the closest to the anchor).
const NEARBY_MAX: usize = 10;
/// Quiet period after the last view change (pan / zoom / in-progress typing)
/// before the whole model is re-serialised into the CRDT. A continuous gesture
/// touches `center`/`zoom` every frame; without this, `sync` would
/// `serde_json::to_string` the entire model (route + transit geometry, places,
/// regions) 60×/second — the measured hot pass. Coalescing collapses that to one
/// write once the gesture settles; the view still converges for peers and
/// survives a restart, just not per-frame.
const PERSIST_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(120);

/// A clickable hot-region recorded during draw and hit-tested on click.
#[derive(Clone, Debug)]
enum Hit {
    /// Swallow the click (panel chrome) — don't pan the map.
    Consume,
    Search(SearchHit),
    /// Collapse / expand the side panel (docked layouts).
    ToggleCollapse,
    /// Return from the Directions view to the plain places list.
    PanelBack,
    Mode(TravelMode),
    SelectPlace(String),
    /// Select a nearby POI from the Explore list: centre the map on it and open
    /// its info card, linking the list to the marker layer.
    SelectPoi(String),
    /// Dismiss the open POI info card.
    ClosePoi,
    /// Route to the POI whose info card is open.
    PoiDirections,
    Recent(String),
    /// Toggle the overlay drawer (narrow layouts only).
    ToggleNav,
    /// Dismiss the overlay drawer (scrim tap).
    CloseNav,
    Directions,
    Transit,
    ToggleSave,
    ClearRoute,
    SwapEnds,
    /// Focus the Directions "From" field for typed origin geocoding.
    EditOrigin,
    /// Focus the Directions "To" field for typed destination geocoding.
    EditDest,
    /// Choose the Nth geocoded result for the origin field.
    PickOrigin(usize),
    /// Choose the Nth geocoded result for the destination field.
    PickDest(usize),
    /// Fetch the device's real location (portal / `GeoClue`) and use it as origin.
    UseMyLocation,
    /// Set the Transit depart/arrive anchor.
    TransitTimeMode(TransitTimeMode),
    /// Shift the Transit time anchor by ±minutes.
    TransitTimeDelta(i32),
    /// Reset the Transit time anchor to "leave now".
    TransitTimeNow,
    Journey(usize),
    /// Toggle the bottom-right ⋯ overflow menu (attribution / offline / licenses).
    ToggleOverflow,
    /// Dismiss the open map-data info sheet.
    CloseMapInfo,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent render/interaction cache (drag state, focus, \
              debounce/CRDT bookkeeping), not a state machine that would collapse into an enum"
)]
pub struct MapsApp {
    model: MapModel,
    basemap: Basemap,
    rect: Rect,
    node: Option<NodeId>,

    /// Framework-owned action dispatch: every collaborative doc mutation (save a
    /// place, clear the route, add/remove an offline region) goes through this so
    /// it brackets one undo boundary and replays byte-identically on peers — the
    /// same path a Ctrl+K palette dispatch takes.
    actions: ui::ActionHost,

    // interaction state (render/UI caches only — app state lives in `model`)
    last_mouse: Point,
    /// Floating zoom/location/traffic stack. Owns geometry, drawing,
    /// hit-testing, traffic UI state, and shared tooltip timing.
    map_controls: MapControls,
    /// Top-level place search: text input, focus, debounce, results overlay,
    /// drawing, and hot regions are one component.
    search: SearchBox,
    /// The Directions From / To endpoint fields, likewise shared `TextInput`s
    /// (Enter geocodes that endpoint). While editing, each mirrors its
    /// `model.*_query`; otherwise it shows the resolved endpoint name.
    origin_input: TextInput,
    dest_input: TextInput,
    /// Which Directions endpoint field is being typed into, if any.
    edit_field: Option<EndField>,
    /// Geocoded dropdown results for the "From" field (render cache).
    origin_results: Vec<crate::model::Place>,
    /// Geocoded dropdown results for the "To" field (render cache).
    dest_results: Vec<crate::model::Place>,
    /// Debounce deadline for the live endpoint (From/To) geocode. Same shape as
    /// Same debounce pattern as the top-level search: armed on each keystroke,
    /// the start/end fields search-as-you-type (no Enter) just like the top bar.
    endpoint_debounce: Option<Instant>,
    /// Screen rects of the last-drawn From / To endpoint fields, so the floating
    /// endpoint results overlay can anchor directly under whichever is focused
    /// (a dropdown over the map — it never reflows the panel). Render caches.
    origin_field_rect: Option<Rect>,
    dest_field_rect: Option<Rect>,
    left_down: bool,
    dragging: bool,
    drag_last: Point,
    drag_moved: bool,
    click_targets: Vec<(Rect, Hit)>,
    scroll: f64,
    content_h: f64,
    content_view_h: f64,

    // CRDT bridge
    loaded: bool,
    dirty: bool,
    /// When the coalesced view-persist should flush. Armed by [`mark_dirty`] and
    /// pushed forward on every subsequent change, so a continuous pan/zoom writes
    /// the model to the CRDT once — after [`PERSIST_DEBOUNCE`] of quiet — rather
    /// than serialising the whole model every frame.
    persist_deadline: Option<Instant>,

    // networking
    net_tx: Sender<NetMsg>,
    net_rx: Receiver<NetMsg>,
    pending: u32,
    /// Whether background network/portal work may be spawned. False for the
    /// offline `--shots`/test app so it never touches the wire or D-Bus.
    online: bool,

    // device location (portal / GeoClue), off the UI thread
    location_rx: Option<LocationChannel>,
    /// True while a location fix is being acquired (drives the "Locating…" UI).
    locating: bool,

    // raster basemap (non-blocking OSM tiles)
    tiles: TileLayer,

    // live POI marker layer (Overpass), our own themed layer over the basemap
    /// POIs for the current viewport (ephemeral render cache — re-fetched on
    /// pan/zoom, never persisted in the CRDT).
    pois: Vec<crate::model::Poi>,
    /// The POI whose info card is open, if any. Held apart from `model.selected`
    /// (a persisted Place) so opening a POI never disturbs the saved pins.
    selected_poi: Option<String>,
    /// Debounce deadline for the viewport POI refetch (armed on pan/zoom).
    poi_debounce: Option<Instant>,
    /// The padded bbox last fetched for POIs, so small pans reuse the cache.
    poi_fetched_bounds: Option<GeoBounds>,
    /// Monotonic query generation so a stale Overpass reply for an old viewport
    /// is discarded rather than drawn over the current one.
    poi_generation: u64,

    // bottom-right ⋯ overflow menu (attribution / offline maps / licenses) — a
    // shared ContextMenu so it is themed, keyboard-navigable and a11y-correct.
    overflow: ContextMenu,
    /// The open map-data info sheet (attribution or licenses), if any.
    map_info: Option<MapInfo>,

    /// Offline download/cache-management component. It owns its widget state,
    /// drawing, and hit-testing; this app only performs requested tile effects.
    offline_panel: OfflinePanel,

    /// Whether the side panel drawer is open. Only consulted in the narrow
    /// overlay layout; when docked the panel is always shown. Re-evaluated
    /// against the live window width every frame — never cached at startup.
    nav_open: bool,

    /// Whether the docked side panel is collapsed to its header bar (an
    /// expand/collapse toggle, not a tab). Only meaningful in the docked
    /// layout; the narrow layout hides/shows the whole drawer instead.
    panel_collapsed: bool,

    /// The current traffic-congestion picture. Empty until real peer reports
    /// arrive — the honest live state (see [`crate::traffic`]).
    traffic: TrafficData,
}

impl Default for MapsApp {
    fn default() -> Self {
        Self::new()
    }
}

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

    /// Headless/offline variant used by `--shots` and tests: no worker thread,
    /// no network. Renders the bundled sample city (places, a route, transit
    /// options) and the stylised vector basemap so the screenshot is fully
    /// populated without ever touching the wire. The overlay drawer starts
    /// open so narrow screenshots show the panel content.
    pub fn offline() -> Self {
        Self::build(
            sample::default_model(),
            sample::basemap(),
            TileLayer::offline(),
            true,
            false,
        )
    }

    fn build(
        model: MapModel,
        basemap: Basemap,
        tiles: TileLayer,
        nav_open: bool,
        online: bool,
    ) -> Self {
        let (net_tx, net_rx) = std::sync::mpsc::channel();
        let cache_bytes = tiles.cache_bytes();
        Self {
            model,
            basemap,
            rect: Rect::ZERO,
            node: None,
            actions: ui::ActionHost::new(),
            last_mouse: Point::ZERO,
            map_controls: MapControls::default(),
            search: SearchBox::new(),
            origin_input: {
                let mut t = TextInput::local()
                    .with_enter_submits(true)
                    .with_font_size(13.0);
                t.set_plain_chrome(true);
                t.set_placeholder("Choose starting point");
                t
            },
            dest_input: {
                let mut t = TextInput::local()
                    .with_enter_submits(true)
                    .with_font_size(13.0);
                t.set_plain_chrome(true);
                t.set_placeholder("Choose destination");
                t
            },
            edit_field: None,
            origin_results: Vec::new(),
            dest_results: Vec::new(),
            endpoint_debounce: None,
            origin_field_rect: None,
            dest_field_rect: None,
            left_down: false,
            dragging: false,
            drag_last: Point::ZERO,
            drag_moved: false,
            click_targets: Vec::new(),
            scroll: 0.0,
            content_h: 0.0,
            content_view_h: 0.0,
            loaded: false,
            dirty: true,
            persist_deadline: None,
            net_tx,
            net_rx,
            pending: 0,
            online,
            location_rx: None,
            locating: false,
            tiles,
            overflow: {
                let mut m = ContextMenu::new();
                // Indices are mapped in `dispatch` via `take_selection`.
                m.push(MenuItem::action("Attribution", || {}));
                m.push(MenuItem::action("Offline maps", || {}));
                m.push(MenuItem::action("Licenses", || {}));
                m
            },
            map_info: None,
            offline_panel: OfflinePanel::new(cache_bytes),
            nav_open,
            pois: Vec::new(),
            selected_poi: None,
            poi_debounce: None,
            poi_fetched_bounds: None,
            poi_generation: 0,
            panel_collapsed: false,
            traffic: TrafficData::default(),
        }
    }

    /// Screenshot-only hook: force the overlay drawer open/closed so both the
    /// map-first and panel-open states can be captured headlessly.
    pub fn set_nav_open(&mut self, open: bool) {
        self.nav_open = open;
    }

    /// Screenshot-only hook: force the active side panel so each tab can be
    /// captured headlessly. Offline sample data already populates every panel,
    /// so this never triggers a fetch.
    pub fn set_active_panel(&mut self, panel: Panel) {
        self.model.panel = panel;
    }

    /// Screenshot-only hook: force the Directions "From" field into its editing
    /// state with a populated geocoded dropdown, so the typeable-origin flow can
    /// be captured headlessly. Uses offline local matches only — no network.
    pub fn set_demo_edit_origin(&mut self) {
        self.model.panel = Panel::Directions;
        self.edit_field = Some(EndField::Origin);
        self.model.origin_query = "hauptbahnhof".to_string();
        self.origin_results = self.local_matches("hauptbahnhof");
    }

    /// Screenshot-only hook: seed the live POI marker layer from bundled sample
    /// data and open the info card for one of them, so the themed POI markers and
    /// the info card can be captured headlessly. No network.
    pub fn set_demo_pois(&mut self) {
        self.pois = sample::pois();
        self.selected_poi = Some("osm-node-203".to_string()); // Hofbräuhaus
        // Frame the map so the POI cluster sits in the upper map area and the
        // bottom-anchored info card is clear of it. Zoom past the icon + label
        // threshold so every marker carries its category pictogram and name.
        self.model.center = LatLon::new(48.1381, 11.5770);
        self.model.zoom = 16.7;
        self.model.selected = None;
        self.model.route = None;
        // Focus the Explore panel on the live NEARBY list for the shot: drop the
        // stale committed search/results so the nearby places lead the panel and
        // the selected POI's row is the highlighted one.
        self.model.results.clear();
        self.model.query.clear();
        self.model.panel = Panel::Explore;
    }

    /// Screenshot-only hook: open the offline-maps overlay with a couple of
    /// already-saved regions and a plausible on-disk size, so the download UI
    /// AND the managed-region list render headlessly. No network, no disk I/O —
    /// the regions are bundled sample metadata, not real cache entries.
    pub fn set_demo_offline(&mut self) {
        self.model.panel = Panel::Explore;
        self.model.selected = None;
        self.model.route = None;
        self.model.regions = sample::regions();
        // Demonstrate the download-by-place search: a typed city with candidate
        // results in the shared dropdown (offline sample data — no network).
        self.offline_panel
            .set_demo(47_542_000, "Mün", sample::download_regions());
    }

    /// Screenshot-only hook: open the ⋯ overflow menu so its Attribution /
    /// Offline maps / Licenses options render expanded over the map. Seeds the
    /// rect (the shot renders at the default viewport) so the menu anchors above
    /// its bottom-right button before layout runs.
    pub fn set_demo_overflow(&mut self) {
        self.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let b = self.overflow_rect();
        self.overflow.open_at([b.x1 as f32, b.y0 as f32]);
        let mr = self.overflow.get_rect();
        self.overflow.open_at([
            (b.x1 - mr.width()) as f32,
            (b.y0 - mr.height() - 6.0) as f32,
        ]);
    }

    /// Screenshot-only hook: open the floating search-results overlay populated
    /// with proximity-ranked matches, so the search-as-you-type dropdown can be
    /// captured headlessly. Offline local data only — no network.
    pub fn set_demo_search(&mut self) {
        self.model.panel = Panel::Explore;
        self.search.set_focused(true);
        self.model.query = "Marienplatz".to_string();
        let mut results = self.model.places.clone();
        self.rank_by_proximity(&mut results);
        self.search.replace_results(results);
    }

    /// Screenshot-only hook: open Directions in Transit mode anchored to an
    /// "Arrive by" time an hour out, so the departures list, the stepped time
    /// label and the "Now" reset chip all render.
    pub fn set_demo_transit_arrive(&mut self) {
        self.model.panel = Panel::Directions;
        self.model.mode = TravelMode::Transit;
        self.model.route = None;
        self.model.transit_time = crate::model::TransitTime {
            mode: TransitTimeMode::Arrive,
            offset_min: 60,
        };
    }

    /// Screenshot-only hook: select a travel mode in the Directions panel so
    /// each mode (including public Transit) can be captured headlessly. Offline
    /// sample data already populates the route/departures, so nothing fetches.
    pub fn set_travel_mode(&mut self, mode: TravelMode) {
        self.model.mode = mode;
        self.model.panel = Panel::Directions;
        // A transit journey draws no road polyline; drop the sample road route
        // so the map under the departures stays clean.
        if mode.is_transit() {
            self.model.route = None;
        }
    }

    /// Screenshot/test helper: an offline (no-worker, no-network) app that
    /// mirrors the live first-run state — an empty model and a plain themed
    /// basemap — so the empty Explore hint can be captured hermetically.
    pub fn offline_empty() -> Self {
        Self::build(
            MapModel::default(),
            Basemap::default(),
            TileLayer::offline(),
            true,
            false,
        )
    }

    /// Live-verify helper: a fully live app (real tile worker, real network,
    /// `online == true`) whose on-disk tile cache is rooted at `cache_dir`
    /// instead of the shared default. Identical production path to [`new`] —
    /// the only difference is a redirectable cache — so a live integration
    /// test can start from a clean cache and inspect exactly the PNGs the
    /// worker writes. Not used by the live app or `--shots`.
    pub fn new_with_cache_dir(cache_dir: std::path::PathBuf) -> Self {
        Self::build(
            MapModel::default(),
            Basemap::default(),
            // Style-scoped like the real `new()` — the cache is rooted here and
            // the active theme's style subdir is chosen on the first draw.
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
        self.mark_dirty();
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

    // --- geometry helpers --------------------------------------------------
    fn view_center(&self) -> Point {
        Point::new(
            f64::midpoint(self.rect.x0, self.rect.x1),
            f64::midpoint(self.rect.y0, self.rect.y1),
        )
    }

    fn projection(&self) -> Projection {
        Projection::new(self.model.center, self.model.zoom, self.view_center())
    }

    /// Whether the layout is narrow enough that the panel becomes an overlay
    /// drawer. Derived from the *current* allocated width every call, so a
    /// resize between breakpoints reflows immediately — never cached.
    fn overlay_mode(&self) -> bool {
        self.rect.width() < NAV_OVERLAY_BELOW
    }

    /// Is the side panel currently on screen? Always in docked layouts; only
    /// when the drawer is open in the overlay layout.
    fn panel_visible(&self) -> bool {
        !self.overlay_mode() || self.nav_open
    }

    /// Whether the docked panel column is occupying space with content (docked
    /// and expanded). Bottom map cards keep clear of it; when collapsed the map
    /// reclaims the column so they may use the full width.
    fn panel_docked_expanded(&self) -> bool {
        !self.overlay_mode() && !self.panel_collapsed
    }

    /// Panel width for the current window: a docked column proportional to the
    /// window when wide, or a near-full-width drawer when narrow.
    fn panel_w(&self) -> f64 {
        if self.overlay_mode() {
            (self.rect.width() - 2.0 * MARGIN).min(PANEL_MAX_W)
        } else {
            (self.rect.width() * 0.42).clamp(PANEL_MIN_W, PANEL_MAX_W)
        }
    }

    /// The leading menu button that toggles the drawer — narrow layouts only.
    fn menu_rect(&self) -> Option<Rect> {
        if !self.overlay_mode() {
            return None;
        }
        let x0 = self.rect.x0 + MARGIN;
        let y0 = self.rect.y0 + MARGIN;
        Some(Rect::new(x0, y0, x0 + CTRL_SIZE, y0 + SEARCH_H))
    }

    fn search_rect(&self) -> Rect {
        let y0 = self.rect.y0 + MARGIN;
        if self.overlay_mode() {
            // Sits between the leading menu button and the trailing zoom column
            // so the bar spans the full top edge without colliding.
            let x0 = self.rect.x0 + MARGIN + CTRL_SIZE + GAP;
            let x1 = self.rect.x1 - MARGIN - CTRL_SIZE - GAP;
            Rect::new(x0, y0, x1.max(x0 + 120.0), y0 + SEARCH_H)
        } else {
            let x0 = self.rect.x0 + MARGIN;
            Rect::new(x0, y0, x0 + self.panel_w(), y0 + SEARCH_H)
        }
    }

    fn panel_rect(&self) -> Rect {
        let x0 = self.rect.x0 + MARGIN;
        let top = self.rect.y0 + MARGIN + SEARCH_H + GAP;
        Rect::new(x0, top, x0 + self.panel_w(), self.rect.y1 - MARGIN)
    }

    /// The bottom-right ⋯ overflow button (attribution / offline maps / licenses).
    /// Downloads and the credit line moved off the map into this menu.
    fn overflow_rect(&self) -> Rect {
        let x1 = self.rect.x1 - MARGIN;
        let y1 = self.rect.y1 - MARGIN;
        Rect::new(x1 - 40.0, y1 - 30.0, x1, y1)
    }

    /// The geographic bounds currently visible in the map viewport.
    fn viewport_bounds(&self) -> GeoBounds {
        let proj = self.projection();
        let nw = proj.unproject(Point::new(self.rect.x0, self.rect.y0));
        let se = proj.unproject(Point::new(self.rect.x1, self.rect.y1));
        GeoBounds::new(nw, se)
    }

    // --- offline regions ---------------------------------------------------

    /// The tile ids a saved region covers — used for delete/keep set arithmetic
    /// so removing one area doesn't drop imagery another still needs.
    fn region_tile_ids(region: &crate::model::DownloadedRegion) -> Vec<TileId> {
        GeoBounds::new(region.min, region.max).tiles_in_range(region.z0, region.z1)
    }

    /// A human label for a downloaded area: a known place inside it when there
    /// is one (reads nicely, e.g. "Marienplatz"), else its centre coordinate.
    fn region_name(&self, bounds: GeoBounds) -> String {
        if let Some(p) = self
            .model
            .places
            .iter()
            .find(|p| bounds_contains(bounds, GeoBounds::new(p.pos, p.pos)))
        {
            return p.name.clone();
        }
        let lat = f64::midpoint(bounds.min.lat, bounds.max.lat);
        let lon = f64::midpoint(bounds.min.lon, bounds.max.lon);
        format!("{lat:.3}, {lon:.3}")
    }

    /// Save (or refresh) the just-requested download as a manageable region.
    /// The region **row** is a collaborative doc mutation dispatched through the
    /// action host (its dedupe of the same footprint lives in the action's
    /// `apply`); the tiles themselves were fetched externally by the caller.
    /// `name` overrides the derived label (the searched place's name).
    fn record_region(
        &mut self,
        doc: &mut AutoCommit,
        bounds: GeoBounds,
        z0: u8,
        z1: u8,
        tiles: u32,
        name: Option<String>,
    ) {
        // Id + name + tile count are resolved here, at authoring time, and carried
        // in the action so `apply` stays free of the RNG / wall-clock.
        let name = name.unwrap_or_else(|| self.region_name(bounds));
        let id = format!(
            "region-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_micros())
        );
        let region = crate::model::DownloadedRegion {
            id,
            name,
            min: bounds.min,
            max: bounds.max,
            z0,
            z1,
            tiles,
        };
        self.perform(doc, &MapsAction::AddRegion(region));
    }

    /// Remove a saved region: dispatch the row removal as an action, and delete
    /// the tiles no *other* saved region still needs as an external side effect.
    /// Shared imagery is kept — the tile arithmetic reads the live region set
    /// before the row is dropped.
    fn delete_region(&mut self, doc: &mut AutoCommit, id: &str) {
        let Some(region) = self.model.regions.iter().find(|r| r.id == id).cloned() else {
            return;
        };
        // Tiles any *other* saved region still needs must survive.
        let mut keep: std::collections::HashSet<TileId> = std::collections::HashSet::new();
        for r in self.model.regions.iter().filter(|r| r.id != id) {
            keep.extend(Self::region_tile_ids(r));
        }
        let doomed: Vec<TileId> = Self::region_tile_ids(&region)
            .into_iter()
            .filter(|t| !keep.contains(t))
            .collect();
        let freed = self.tiles.delete_tiles(&doomed);
        self.offline_panel.set_cache_bytes(self.tiles.cache_bytes());
        // The metadata row is the action; the tile deletion above is external.
        self.perform(doc, &MapsAction::DeleteRegion { id: id.to_string() });
        self.model.status = format!("Removed “{}” · {} freed", region.name, fmt_bytes(freed));
        self.mark_dirty();
    }

    // --- actions -----------------------------------------------------------
    fn mark_dirty(&mut self) {
        self.dirty = true;
        // Push the persist deadline forward so a continuous gesture coalesces
        // into a single serialise once it settles (see `PERSIST_DEBOUNCE`).
        self.persist_deadline = Some(Instant::now() + PERSIST_DEBOUNCE);
    }

    /// Dispatch one collaborative [`MapsAction`] against the shared document.
    ///
    /// Maps holds its state as an in-memory [`MapModel`] mirrored into a single
    /// CRDT blob, so this first flushes the live model to `doc` (usually a no-op —
    /// `sync` already did — but it guarantees the action mutates current state and
    /// that the [`ActionHost`](ui::ActionHost) brackets *only* the action). The
    /// host encodes + applies it via the executor (one undo boundary, byte-
    /// identical replay on peers); we then re-adopt the mutated model so the UI
    /// and the document agree on a single source of truth.
    fn perform(&mut self, doc: &mut AutoCommit, action: &MapsAction) {
        MapsDoc::save(doc, &self.model);
        self.actions.dispatch(doc, MAPS_ACTION_TYPE, action);
        if let Some(model) = MapsDoc::load(doc) {
            self.model = model;
        }
        self.dirty = false;
    }

    /// Toggle the traffic overlay. Turning it on triggers a refresh so the
    /// current congestion picture is loaded (or the honest empty state is set).
    fn set_traffic(&mut self, on: bool) {
        self.map_controls.set_traffic_enabled(on);
        if on {
            self.refresh_traffic();
        }
        self.mark_dirty();
    }

    /// Populate the traffic overlay.
    ///
    /// qOS traffic is a crowdsourced, peer-shared layer (see `plan/product.md`);
    /// no such live feed is wired yet, so the LIVE app has no reports and the
    /// overlay stays empty — the map then shows "No traffic data" rather than
    /// inventing congestion. When the bundled offline cartography is loaded
    /// (the `--shots` / sample path, distinguished by a non-empty vector
    /// basemap), we colour its arterials from the sample congestion set so the
    /// overlay is demonstrable — the same sample-only gating every other piece
    /// of this app's offline data already uses. Nothing is fabricated on the
    /// live path.
    fn refresh_traffic(&mut self) {
        if self.basemap.is_empty() {
            self.traffic = TrafficData::default();
        } else {
            self.traffic = TrafficData::sample(&self.basemap);
        }
    }

    /// Screenshot-only hook: turn the traffic overlay on so the coloured
    /// congestion segments (or, with `--empty`, the honest "No traffic data"
    /// state) can be captured headlessly.
    pub fn set_demo_traffic(&mut self) {
        self.set_traffic(true);
    }

    fn select_place(&mut self, id: &str) {
        let changed = self.model.selected.as_deref() != Some(id);
        self.model.selected = Some(id.to_string());
        // Selecting a place expresses destination intent: it becomes the
        // routing/transit target. A later swap can move it to the origin side
        // without this clobbering it (see `swap_ends`), because subsequent
        // re-routes read `dest`, not the selection.
        if let Some(p) = self.model.place(id).cloned() {
            self.model.center = p.pos;
            self.model.dest = Some(p.pos);
            self.model.dest_name = p.name;
        }
        if changed {
            // Departures are destination-specific: drop stale itineraries so
            // the Transit view re-fetches for the newly selected place.
            self.model.transit.clear();
            self.model.transit_choice = 0;
        }
        // Selecting a place (e.g. tapping a map marker) reveals its card.
        self.nav_open = true;
        self.mark_dirty();
    }

    /// Point the destination at the currently-selected place. The explicit
    /// entry points (tapping Directions/Transit on a place card, the keyboard
    /// shortcuts) call this so choosing a new place re-targets the route.
    fn set_dest_to_selection(&mut self) {
        if let Some(p) = self.model.selected_place().cloned() {
            self.model.dest = Some(p.pos);
            self.model.dest_name = p.name;
        }
    }

    fn set_panel(&mut self, panel: Panel) {
        self.model.panel = panel;
        self.scroll = 0.0;
        self.nav_open = true;
        // Leaving/entering a tab drops any in-progress endpoint editing so
        // keystrokes never land in a field that isn't on screen.
        self.edit_field = None;
        // Entering Directions actually performs the lookup for the current
        // place + travel mode — the tab is not a passive view. Transit is a
        // mode of Directions, so which lookup runs depends on the active mode.
        if panel == Panel::Directions {
            if self.model.mode.is_transit() {
                let has_target = self.model.dest.is_some() || self.model.selected_place().is_some();
                if self.model.transit.is_empty() && self.pending == 0 && has_target {
                    self.recompute_transit();
                }
            } else if self.model.route.is_none() {
                self.recompute_route();
            }
        }
        self.mark_dirty();
    }

    /// Resolve the route/transit origin. Uses a prior origin if one is set,
    /// otherwise the current map centre (an honest "from here"), labelled as
    /// such — we never invent a specific "your location" without a real fix.
    fn resolve_origin(&mut self) -> crate::model::LatLon {
        if let Some(o) = self.model.origin {
            o
        } else {
            self.model.origin_name = "Map centre".to_string();
            self.model.center
        }
    }

    /// Substring/keyword match over the known places for a query. Shared by the
    /// top Explore search and the Directions endpoint fields so all three give
    /// the same instant offline results before any network response lands.
    fn local_matches(&self, q: &str) -> Vec<crate::model::Place> {
        let needle = q.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut results: Vec<_> = self
            .model
            .places
            .iter()
            .filter(|p| {
                p.name.to_lowercase().contains(&needle)
                    || p.address.to_lowercase().contains(&needle)
                    || p.category.label().to_lowercase().contains(&needle)
            })
            .cloned()
            .collect();
        if results.is_empty() {
            // Fall back to category keyword matches so a term like "coffee"
            // still surfaces cafés.
            results = self
                .model
                .places
                .iter()
                .filter(|p| keyword_match(&needle, p.category))
                .cloned()
                .collect();
        }
        // Also match the live viewport POIs (Overpass) by name, address,
        // category label, cuisine/kind, or a category keyword, so a query like
        // "coffee" or "museum" instantly surfaces what's on the map right now.
        for poi in &self.pois {
            if results.iter().any(|r| r.id == poi.id) {
                continue;
            }
            let hit = poi.name.to_lowercase().contains(&needle)
                || poi.address.to_lowercase().contains(&needle)
                || poi.category.label().to_lowercase().contains(&needle)
                || poi
                    .kind
                    .as_deref()
                    .is_some_and(|k| k.to_lowercase().contains(&needle))
                || keyword_match(&needle, poi.category);
            if hit {
                results.push(crate::model::Place::new(
                    &poi.id,
                    &poi.name,
                    poi.category,
                    &poi.address,
                    poi.pos,
                ));
            }
        }
        results
    }

    /// The reference point search results are ranked against: the user's real
    /// location when known, otherwise the current map centre.
    fn search_anchor(&self) -> crate::model::LatLon {
        self.model.origin.unwrap_or(self.model.center)
    }

    /// Order places by increasing great-circle distance from the search anchor,
    /// so the nearest match to where the user is looking ranks first.
    fn rank_by_proximity(&self, places: &mut [crate::model::Place]) {
        let anchor = self.search_anchor();
        places.sort_by(|a, b| {
            geo::haversine_m(a.pos, anchor).total_cmp(&geo::haversine_m(b.pos, anchor))
        });
    }

    /// Rank geocoded results by relevance to `query` with distance to the search
    /// anchor as the tie-breaker, and collapse co-located duplicates. This is
    /// the fix for far-away namesakes (a 511 km hit) outranking the near place
    /// the user meant — see [`crate::rank`].
    fn rank_results(&self, places: &mut Vec<crate::model::Place>, query: &str) {
        crate::rank::rank(places, self.search_anchor(), query);
    }

    /// Instant offline matches for `q`, ranked by proximity — the zero-latency
    /// content shown in the search overlay before any network reply lands.
    fn ranked_matches(&self, q: &str) -> Vec<crate::model::Place> {
        let mut v = self.local_matches(q);
        self.rank_results(&mut v, q);
        v
    }

    /// Handle one edit to the top search query: refresh the instant overlay
    /// matches and (re)arm the debounce so a live geocode fires once typing
    /// settles. This is the search-as-you-type entry point — no Enter needed.
    fn on_search_input(&mut self) {
        let q = self.model.query.trim().to_string();
        let matches = self.ranked_matches(&q);
        self.search.query_changed(&q, matches, Instant::now());
        self.mark_dirty();
    }

    /// Fire the debounced live geocode if the quiet period has elapsed. Called
    /// once per frame from `sync` (the search field keeps the frame loop awake).
    fn tick_search_debounce(&mut self, now: Instant) {
        let Some(q) = self.search.take_due_query(now, &self.model.query) else {
            return;
        };
        // Best-effort live augmentation; instant local matches already show.
        self.pending += 1;
        self.model.status = "Searching…".to_string();
        if self.online {
            let tx = self.net_tx.clone();
            std::thread::spawn(move || {
                let results = net::geocode(&q);
                let _ = tx.send(NetMsg::Search { query: q, results });
            });
        }
    }

    /// Local (offline) search: substring match over known places. Also kicks
    /// off a live Nominatim request whose results fold in later.
    fn run_search(&mut self) {
        let q = self.model.query.trim().to_string();
        if q.is_empty() {
            return;
        }
        let results = self.ranked_matches(&q);
        if let Some(first) = results.first() {
            let id = first.id.clone();
            self.model.center = first.pos;
            self.model.selected = Some(id);
        }
        self.model.results = results;
        self.model.push_recent(&q);
        self.model.panel = Panel::Explore;
        self.scroll = 0.0;
        // Enter commits the search: dismiss the floating overlay and stop the
        // pending debounce (the live augmentation below supersedes it).
        self.search.dismiss_results();
        // Results just landed — reveal the drawer on narrow layouts.
        self.nav_open = true;

        // Live augmentation (best effort).
        self.spawn_search(q);
        self.mark_dirty();
    }

    fn spawn_search(&mut self, query: String) {
        self.pending += 1;
        self.model.status = "Searching…".to_string();
        if !self.online {
            return;
        }
        let tx = self.net_tx.clone();
        std::thread::spawn(move || {
            let results = net::geocode(&query);
            let _ = tx.send(NetMsg::Search { query, results });
        });
    }

    /// Focus a Directions endpoint field for typed geocoding: clear its query so
    /// the user types fresh (a placeholder shows), and drop the top-search focus.
    fn focus_endpoint(&mut self, field: EndField) {
        self.edit_field = Some(field);
        self.search.set_focused(false);
        self.endpoint_debounce = None;
        match field {
            EndField::Origin => {
                self.model.origin_query.clear();
                self.origin_results.clear();
            }
            EndField::Dest => {
                self.model.dest_query.clear();
                self.dest_results.clear();
            }
        }
        self.mark_dirty();
    }

    fn endpoint_query_mut(&mut self, origin: bool) -> &mut String {
        if origin {
            &mut self.model.origin_query
        } else {
            &mut self.model.dest_query
        }
    }

    /// Refresh the instant offline dropdown for an endpoint field from the
    /// current query text (no network — runs on every keystroke).
    fn update_endpoint_local(&mut self, origin: bool) {
        let q = if origin {
            self.model.origin_query.clone()
        } else {
            self.model.dest_query.clone()
        };
        let local = self.local_matches(&q);
        if origin {
            self.origin_results = local;
        } else {
            self.dest_results = local;
        }
    }

    /// Handle one edit to an endpoint (From/To) field: refresh the instant
    /// offline dropdown and (re)arm the debounce so a live geocode fires once
    /// typing settles. This is the endpoint search-as-you-type entry point —
    /// no Enter required, mirroring the top search field.
    fn on_endpoint_input(&mut self, origin: bool) {
        self.update_endpoint_local(origin);
        let q = if origin {
            self.model.origin_query.trim().to_string()
        } else {
            self.model.dest_query.trim().to_string()
        };
        if q.is_empty() {
            self.endpoint_debounce = None;
        } else {
            self.endpoint_debounce = Some(Instant::now() + ENDPOINT_SEARCH_DEBOUNCE);
        }
        self.mark_dirty();
    }

    /// Fire the debounced live endpoint geocode once the quiet period elapses.
    /// Called once per frame from `sync`. Targets whichever endpoint field is
    /// currently focused; a focus change or commit clears the debounce.
    fn tick_endpoint_debounce(&mut self, now: Instant) {
        let Some(deadline) = self.endpoint_debounce else {
            return;
        };
        if now < deadline {
            return;
        }
        self.endpoint_debounce = None;
        let Some(field) = self.edit_field else { return };
        self.run_endpoint_search(field == EndField::Origin);
    }

    /// Run a live Nominatim geocode for an endpoint field, folding in the
    /// instant offline matches first so the dropdown is never briefly empty.
    /// Reached both from the settled debounce and from an explicit Enter.
    fn run_endpoint_search(&mut self, origin: bool) {
        self.endpoint_debounce = None;
        self.update_endpoint_local(origin);
        let q = if origin {
            self.model.origin_query.trim().to_string()
        } else {
            self.model.dest_query.trim().to_string()
        };
        if q.is_empty() {
            self.mark_dirty();
            return;
        }
        self.pending += 1;
        self.model.status = "Searching…".to_string();
        if self.online {
            let tx = self.net_tx.clone();
            std::thread::spawn(move || {
                let results = net::geocode(&q);
                let _ = tx.send(NetMsg::EndpointSearch {
                    origin,
                    query: q,
                    results,
                });
            });
        }
        self.mark_dirty();
    }

    /// Adopt a geocoded dropdown result as the chosen endpoint: set its coord +
    /// label and re-route (or re-plan transit). Mirrors selecting a destination
    /// so From and To behave symmetrically.
    fn pick_endpoint(&mut self, origin: bool, idx: usize) {
        let picked = if origin {
            self.origin_results.get(idx).cloned()
        } else {
            self.dest_results.get(idx).cloned()
        };
        let Some(p) = picked else { return };
        self.model.upsert_place(p.clone());
        if origin {
            self.model.origin = Some(p.pos);
            p.name.clone_into(&mut self.model.origin_name);
            self.origin_results.clear();
            self.model.origin_query.clear();
        } else {
            self.model.dest = Some(p.pos);
            p.name.clone_into(&mut self.model.dest_name);
            self.model.selected = Some(p.id.clone());
            self.dest_results.clear();
            self.model.dest_query.clear();
        }
        self.edit_field = None;
        self.model.center = p.pos;
        if self.model.mode.is_transit() {
            self.recompute_transit();
        } else {
            self.recompute_route();
        }
        self.mark_dirty();
    }

    /// Kick off a real device-location fix via the location portal (`GeoClue2`
    /// behind it), off the UI thread. Honest states only: "Locating…" now, then
    /// either a real origin or a clear unavailable/denied message on drain.
    fn use_my_location(&mut self) {
        if self.locating {
            return;
        }
        self.locating = true;
        self.model.status = "Locating…".to_string();
        if self.online {
            self.location_rx = Some(location::request_location());
        }
        self.mark_dirty();
    }

    /// The wall-clock instant the trip clocks are anchored to. Live sessions use
    /// the real local time; the offline demo / `--shots` / test path uses a
    /// fixed base (see [`sample::demo_clock_base`]) so the Drive summary and the
    /// Transit departures read from one coherent time rather than a
    /// nondeterministic wall clock. Only affects the demo path — never live data.
    fn clock_anchor(&self) -> chrono::DateTime<chrono::Local> {
        if self.online {
            chrono::Local::now()
        } else {
            sample::demo_clock_base()
        }
    }

    /// Build the transit query time anchor from the picker. "Leave now" sends no
    /// time (the planner uses its own now); otherwise an RFC-3339 wall clock at
    /// `now + offset`, flagged depart/arrive.
    fn transit_query(&self) -> net::TransitQuery {
        let tt = self.model.transit_time;
        let time = if tt.is_now() {
            None
        } else {
            let when = self.clock_anchor() + chrono::Duration::minutes(i64::from(tt.offset_min));
            Some(when.to_rfc3339())
        };
        net::TransitQuery {
            arrive_by: tt.arrive_by(),
            time,
        }
    }

    /// Open Directions to the selected place, forcing the destination to the
    /// current selection. Tapping "Directions" on a place card or pressing `d`
    /// re-targets the route at whatever is selected now, honouring the active
    /// travel mode (a road route, or a transit journey if Transit is selected).
    fn enter_directions(&mut self) {
        self.set_dest_to_selection();
        self.model.panel = Panel::Directions;
        if self.model.mode.is_transit() {
            self.recompute_transit();
        } else {
            self.recompute_route();
        }
    }

    /// Open Directions in Transit mode to the selected place, forcing the
    /// destination to the current selection. The place-card "Transit" shortcut
    /// and the `t` key land here.
    fn enter_transit(&mut self) {
        self.set_dest_to_selection();
        self.model.mode = TravelMode::Transit;
        self.recompute_transit();
    }

    fn recompute_route(&mut self) {
        // Adopt the selected place as destination if none has been chosen yet
        // (e.g. the user changed travel mode straight after selecting a place).
        // A destination set by a prior route or a swap is preserved so the
        // endpoints don't collapse back together.
        if self.model.dest.is_none() {
            self.set_dest_to_selection();
        }
        let Some(dest) = self.model.dest else {
            return;
        };
        let origin = self.resolve_origin();
        self.model.origin = Some(origin);
        self.nav_open = true;
        let mode = self.model.mode;
        // Immediate offline fallback so the panel is never empty.
        self.model.route = Some(sample::fallback_route(origin, dest, mode));
        self.fit_route();

        // Live OSRM request.
        self.pending += 1;
        self.model.status = "Routing…".to_string();
        if self.online {
            let tx = self.net_tx.clone();
            std::thread::spawn(move || {
                let r = net::route(origin, dest, mode);
                let _ = tx.send(NetMsg::Route(r));
            });
        }
        self.mark_dirty();
    }

    /// React to a change of the leave/arrive time anchor. Transit re-plans the
    /// journey against the new time; the road modes just re-derive the computed
    /// departure/arrival clock (OSRM is not time-dependent), so a repaint is
    /// enough — no needless refetch.
    fn replan_for_time(&mut self) {
        if self.model.mode.is_transit() {
            self.recompute_transit();
        }
        self.mark_dirty();
    }

    fn recompute_transit(&mut self) {
        if self.model.dest.is_none() {
            self.set_dest_to_selection();
        }
        let Some(dest) = self.model.dest else {
            return;
        };
        let origin = self.resolve_origin();
        self.model.origin = Some(origin);
        // Transit is a mode of Directions, not a separate surface.
        self.model.panel = Panel::Directions;
        self.model.mode = TravelMode::Transit;
        self.model.transit_choice = 0;
        // A transit journey has no road polyline — drop any road route so the
        // map doesn't draw a car line beneath the departures.
        self.model.route = None;
        self.nav_open = true;
        // Real departures only: clear any stale itineraries and show a loading
        // state until the planner responds — we never fabricate departures.
        self.model.transit.clear();
        self.pending += 1;
        self.model.status = "Finding departures…".to_string();
        // No route geometry to fit; frame the map on the endpoints instead.
        self.fit_points(&[origin, dest]);
        if self.online {
            let tx = self.net_tx.clone();
            let query = self.transit_query();
            std::thread::spawn(move || {
                let r = net::transit(origin, dest, &query);
                let _ = tx.send(NetMsg::Transit(r));
            });
        }
        self.mark_dirty();
    }

    fn set_mode(&mut self, mode: TravelMode) {
        if self.model.mode != mode {
            self.model.mode = mode;
            self.model.panel = Panel::Directions;
            // Transit plans a public-transport journey; the road modes route.
            if mode.is_transit() {
                self.recompute_transit();
            } else {
                self.recompute_route();
            }
        }
    }

    fn toggle_save_selected(&mut self, doc: &mut AutoCommit) {
        if let Some(id) = self.model.selected.clone() {
            // Resolve the target state here (authoring time) so the action carries
            // an explicit `saved`, never a read-and-toggle that could diverge on
            // replay.
            let now_saved = self.model.place(&id).is_some_and(|p| p.saved);
            self.perform(
                doc,
                &MapsAction::SetPlaceSaved {
                    id,
                    saved: !now_saved,
                },
            );
        }
    }

    fn clear_route(&mut self, doc: &mut AutoCommit) {
        self.perform(doc, &MapsAction::ClearRoute);
    }

    /// Exchange the route's origin and destination (both the coordinates and
    /// their labels), then re-route so the line and steps reflect the reversal.
    fn swap_ends(&mut self) {
        // Need a concrete point on both ends to have something to swap.
        if self.model.origin.is_none() || self.model.dest.is_none() {
            return;
        }
        std::mem::swap(&mut self.model.origin, &mut self.model.dest);
        std::mem::swap(&mut self.model.origin_name, &mut self.model.dest_name);
        // Re-route: `recompute_route` reads `dest` (now the old origin) and
        // `resolve_origin` reads `origin` (now the old destination), so the
        // journey is genuinely reversed rather than routed to itself.
        self.recompute_route();
    }

    fn fit_route(&mut self) {
        if let Some(route) = self.model.route.clone() {
            self.fit_points(&route.geometry);
        }
    }

    /// Flatten the currently-selected transit journey's leg geometry into a
    /// single polyline (empty if no journey is selected or none has geometry).
    fn selected_journey_geometry(&self) -> Vec<LatLon> {
        self.model
            .transit
            .get(self.model.transit_choice)
            .map(|j| {
                j.legs
                    .iter()
                    .flat_map(|l| l.geometry.iter().copied())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Frame the map on whatever journey is currently shown — a road route's
    /// polyline, else the selected transit itinerary's leg geometry, else the
    /// origin/destination pair. This is what the `f` key and journey selection
    /// use so "fit" works in Transit mode (which has no road route), not only
    /// for road routes.
    fn fit_current(&mut self) {
        if let Some(route) = self.model.route.clone()
            && route.geometry.len() >= 2
        {
            self.fit_points(&route.geometry);
            return;
        }
        let journey = self.selected_journey_geometry();
        if journey.len() >= 2 {
            self.fit_points(&journey);
            return;
        }
        // No line to fit — frame the endpoints if we have them.
        let mut ends = Vec::new();
        if let Some(o) = self.model.origin {
            ends.push(o);
        }
        if let Some(d) = self.model.dest {
            ends.push(d);
        }
        if ends.len() >= 2 {
            self.fit_points(&ends);
        }
    }

    /// Frame the map so `pts` fit the visible map area (the part not covered by
    /// the docked panel). Used to fit a road route's geometry and, for transit
    /// where there is no road polyline, the origin/destination pair.
    fn fit_points(&mut self, pts: &[LatLon]) {
        // In the docked layout the map shares the width with the panel; in the
        // overlay layout the drawer floats over a full-width map.
        let w = if self.overlay_mode() {
            self.rect.width() - MARGIN * 2.0
        } else {
            self.rect.width() - (self.panel_w() + MARGIN * 2.0 + GAP)
        };
        let h = self.rect.height();
        if let Some((c, z)) = geo::fit_bounds(pts, w.max(200.0), h, 70.0) {
            self.model.center = c;
            self.model.zoom = z.min(16.5);
        }
    }

    fn zoom_by(&mut self, dz: f64, anchor: Point) {
        let proj = self.projection();
        let under = proj.unproject(anchor);
        self.model.zoom = (self.model.zoom + dz).clamp(3.0, 19.0);
        let proj2 = Projection::new(self.model.center, self.model.zoom, self.view_center());
        let q = proj2.project(under);
        let cw = geo::lonlat_to_world(self.model.center, self.model.zoom);
        self.model.center = geo::world_to_lonlat(
            cw.0 + (q.x - anchor.x),
            cw.1 + (q.y - anchor.y),
            self.model.zoom,
        );
        self.schedule_pois();
        self.mark_dirty();
    }

    fn pan_by(&mut self, dx: f64, dy: f64) {
        let cw = geo::lonlat_to_world(self.model.center, self.model.zoom);
        self.model.center = geo::world_to_lonlat(cw.0 - dx, cw.1 - dy, self.model.zoom);
        self.schedule_pois();
        self.mark_dirty();
    }

    // --- POI layer (Overpass) ----------------------------------------------

    /// Arm the viewport POI refetch. Debounced so a drag/zoom gesture fires one
    /// Overpass query when it settles, not per pointer event.
    fn schedule_pois(&mut self) {
        self.poi_debounce = Some(Instant::now() + POI_DEBOUNCE);
    }

    /// Fire the debounced POI refetch if the quiet period has elapsed. Called
    /// once per frame from `sync` (a live viewport keeps the loop awake).
    fn tick_poi_debounce(&mut self, now: Instant) {
        let Some(deadline) = self.poi_debounce else {
            return;
        };
        if now < deadline {
            return;
        }
        self.poi_debounce = None;
        self.fetch_viewport_pois();
    }

    /// Expand the viewport bbox by a margin so a small pan reuses the cache.
    fn padded_viewport_bounds(&self) -> GeoBounds {
        let v = self.viewport_bounds();
        let dlat = (v.max.lat - v.min.lat) * 0.25;
        let dlon = (v.max.lon - v.min.lon) * 0.25;
        GeoBounds::new(
            LatLon::new(v.min.lat - dlat, v.min.lon - dlon),
            LatLon::new(v.max.lat + dlat, v.max.lon + dlon),
        )
    }

    /// Issue the Overpass query for the current viewport, unless the map is
    /// zoomed too far out (clears markers) or the viewport is still inside the
    /// last fetched bbox (reuses the cache). Sets a fresh generation so a stale
    /// reply for a previous viewport is discarded on drain.
    fn fetch_viewport_pois(&mut self) {
        if self.rect.width() <= 1.0 {
            // No valid viewport yet — re-arm so the first real frame fetches.
            self.poi_debounce = Some(Instant::now() + POI_DEBOUNCE);
            return;
        }
        if self.model.zoom < POI_MIN_ZOOM {
            // Zoomed out past the useful range: drop the markers rather than
            // leaving a stale, too-sparse scatter from a closer view.
            self.pois.clear();
            self.poi_fetched_bounds = None;
            return;
        }
        let view = self.viewport_bounds();
        if let Some(prev) = self.poi_fetched_bounds
            && bounds_contains(prev, view)
        {
            return; // still inside the last fetch — reuse the cache
        }
        let bounds = self.padded_viewport_bounds();
        self.poi_fetched_bounds = Some(bounds);
        self.poi_generation += 1;
        let generation = self.poi_generation;
        if self.online {
            self.pending += 1;
            self.model.status = "Loading places…".to_string();
            let tx = self.net_tx.clone();
            std::thread::spawn(move || {
                let pois = net::overpass(
                    bounds.min.lat,
                    bounds.min.lon,
                    bounds.max.lat,
                    bounds.max.lon,
                );
                let _ = tx.send(NetMsg::Pois { generation, pois });
            });
        }
    }

    /// Whether POI markers are drawn as labelled icon markers (close zoom) vs.
    /// small category dots. Kept in step with [`canvas::draw_pois`].
    fn poi_icons_shown(&self) -> bool {
        self.model.zoom >= canvas::POI_ICON_ZOOM
    }

    /// Minimum screen spacing between kept POI markers for the declutter pass —
    /// wider when icon markers are drawn (they are physically larger).
    fn poi_declutter_px(&self) -> f64 {
        if self.poi_icons_shown() { 26.0 } else { 15.0 }
    }

    /// The decluttered POI index set drawn this frame. The draw pass and the
    /// click hit-test both read it, so a click always resolves to a marker the
    /// user can actually see (never one hidden behind a denser neighbour).
    fn visible_poi_indices(&self) -> Vec<usize> {
        canvas::declutter_indices(
            &self.projection(),
            &self.pois,
            self.poi_declutter_px(),
            self.selected_poi.as_deref(),
        )
    }

    /// Hit-test a click against the *visible* POI markers; returns the POI id if
    /// any. Uses the same decluttered set and marker radius as the draw pass.
    fn poi_at(&self, p: Point) -> Option<String> {
        let proj = self.projection();
        let icons = self.poi_icons_shown();
        let mut best: Option<(f64, String)> = None;
        for i in self.visible_poi_indices() {
            let Some(poi) = self.pois.get(i) else {
                continue;
            };
            let c = proj.project(poi.pos);
            let selected = self.selected_poi.as_deref() == Some(poi.id.as_str());
            let base = if icons { 9.0 } else { 4.5 };
            let r = if selected { base + 4.0 } else { base };
            let d = c.distance(p);
            if d <= r + 6.0 && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                best = Some((d, poi.id.clone()));
            }
        }
        best.map(|(_, id)| id)
    }

    /// Open the info card for a POI marker.
    fn select_poi(&mut self, id: &str) {
        self.selected_poi = Some(id.to_string());
        self.mark_dirty();
    }

    /// The viewport POIs the Explore panel lists, nearest-first. The selected
    /// POI is always included even if it would fall past the cap, so the list
    /// row and the open info card stay in sync.
    fn nearby_pois(&self) -> Vec<crate::model::Poi> {
        let anchor = self.search_anchor();
        let mut v = self.pois.clone();
        v.sort_by(|a, b| {
            geo::haversine_m(a.pos, anchor).total_cmp(&geo::haversine_m(b.pos, anchor))
        });
        v.truncate(NEARBY_MAX);
        if let Some(sel) = self.selected_poi.clone()
            && !v.iter().any(|p| p.id == sel)
            && let Some(p) = self.pois.iter().find(|p| p.id == sel).cloned()
        {
            v.pop();
            v.insert(0, p);
        }
        v
    }

    /// Adopt a nearby POI chosen from the Explore list: centre the map on it and
    /// open its info card. Links the list selection to the map marker layer.
    fn select_poi_from_list(&mut self, id: &str) {
        let Some(poi) = self.pois.iter().find(|p| p.id == id).cloned() else {
            return;
        };
        self.model.center = poi.pos;
        self.selected_poi = Some(id.to_string());
        // Reveal the drawer on narrow layouts so the tapped result is visible.
        self.nav_open = true;
        self.mark_dirty();
    }

    /// Route to the POI whose info card is open: adopt it as the destination
    /// (and as a Place so its pin/label show), open Directions, and plan.
    fn poi_directions(&mut self) {
        let Some(id) = self.selected_poi.clone() else {
            return;
        };
        let Some(poi) = self.pois.iter().find(|p| p.id == id).cloned() else {
            return;
        };
        self.model.upsert_place(crate::model::Place::new(
            &poi.id,
            &poi.name,
            poi.category,
            &poi.address,
            poi.pos,
        ));
        self.model.selected = Some(poi.id.clone());
        self.model.dest = Some(poi.pos);
        self.model.dest_name = poi.name;
        self.model.panel = Panel::Directions;
        self.selected_poi = None;
        if self.model.mode.is_transit() {
            self.recompute_transit();
        } else {
            self.recompute_route();
        }
    }

    /// Hit-test a click against the markers; returns the place id if any.
    fn marker_at(&self, p: Point) -> Option<String> {
        let proj = self.projection();
        let mut best: Option<(f64, String)> = None;
        for place in &self.model.places {
            let anchor = proj.project(place.pos);
            let selected = self.model.selected.as_deref() == Some(place.id.as_str());
            let h = if selected { 30.0 } else { 22.0 };
            let r = if selected { 13.0 } else { 9.0 };
            let center = Point::new(anchor.x, anchor.y - h);
            let d = center.distance(p);
            if d <= r + 5.0 && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                best = Some((d, place.id.clone()));
            }
        }
        best.map(|(_, id)| id)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one match arm per NetMsg variant; the length is the number of \
                  message kinds, and splitting arms out would relocate the code, not \
                  shrink what a reader must trace for one variant"
    )]
    fn drain_net(&mut self) {
        while let Ok(msg) = self.net_rx.try_recv() {
            self.pending = self.pending.saturating_sub(1);
            match msg {
                NetMsg::Search { query, results } => match results {
                    Ok(found) if !found.is_empty() => {
                        let mut ranked = found;
                        for p in &ranked {
                            self.model.upsert_place(p.clone());
                        }
                        self.rank_results(&mut ranked, &query);
                        if self.search.is_focused() {
                            // Search-as-you-type: fill the floating overlay only.
                            // Never reflow the panel or recentre the map while
                            // the user is still typing.
                            self.search.replace_results(ranked.clone());
                        } else {
                            // A committed search (Enter): populate the panel and
                            // centre on the top (nearest) match.
                            if let Some(first) = ranked.first() {
                                self.model.center = first.pos;
                                self.model.selected = Some(first.id.clone());
                            }
                            self.model.results = ranked.clone();
                        }
                        self.model.status = format!("{} results for “{}”", ranked.len(), query);
                    }
                    Ok(_) => self.model.status = format!("No results for “{query}”"),
                    Err(e) => self.model.status = format!("Search failed: {e}"),
                },
                NetMsg::EndpointSearch {
                    origin,
                    query,
                    results,
                } => match results {
                    Ok(found) if !found.is_empty() => {
                        for p in &found {
                            self.model.upsert_place(p.clone());
                        }
                        // Rank the From/To dropdown the same way the top search
                        // is ranked — relevance first, nearest-first within a
                        // tier — so a distant namesake never sits above the
                        // near place the user meant.
                        let mut ranked = found;
                        self.rank_results(&mut ranked, &query);
                        if origin {
                            self.origin_results = ranked;
                        } else {
                            self.dest_results = ranked;
                        }
                        self.model.status.clear();
                    }
                    Ok(_) => self.model.status = "No matching places".to_string(),
                    Err(e) => self.model.status = format!("Search failed: {e}"),
                },
                NetMsg::ReverseGeocode(r) => {
                    if let Ok(label) = r
                        && !label.is_empty()
                    {
                        // Refine the device-location origin's label in place.
                        self.model.origin_name = label;
                    }
                }
                NetMsg::Route(r) => match r {
                    Ok(route) => {
                        self.model.route = Some(route);
                        self.fit_route();
                        self.model.status = "Route updated".to_string();
                    }
                    Err(e) => {
                        self.model.status = format!("Routing failed (offline estimate shown): {e}");
                    }
                },
                NetMsg::Transit(t) => match t {
                    Ok(j) if !j.is_empty() => {
                        self.model.transit = j;
                        // Frame the map on the itinerary now that we have real
                        // leg geometry (the same courtesy the road route gets).
                        self.fit_current();
                        self.model.status = "Departures updated".to_string();
                    }
                    Ok(_) => self.model.status = "No transit options found".to_string(),
                    Err(e) => self.model.status = format!("Transit lookup failed: {e}"),
                },
                NetMsg::Pois { generation, pois } => {
                    // Ignore a stale reply for a viewport we've since panned off.
                    if generation == self.poi_generation {
                        match pois {
                            Ok(mut found) => {
                                found.truncate(POI_MAX);
                                let n = found.len();
                                self.pois = found;
                                // Drop the open card if its POI is gone.
                                if let Some(sel) = &self.selected_poi
                                    && !self.pois.iter().any(|p| &p.id == sel)
                                {
                                    self.selected_poi = None;
                                }
                                if self.model.status.contains("Loading places") {
                                    self.model.status = format!("{n} places nearby");
                                }
                            }
                            Err(e) => self.model.status = format!("Places unavailable: {e}"),
                        }
                    }
                }
                NetMsg::RegionSearch { query, results } => {
                    // Only adopt a reply that still matches the current query and
                    // while the download dialog is open.
                    if self.offline_panel.query_matches(&query) {
                        self.offline_panel.adopt_results(results);
                    }
                }
            }
            if self.pending == 0 && self.model.status.contains('…') {
                self.model.status.clear();
            }
            self.mark_dirty();
        }
    }

    /// Drain a pending device-location result (non-blocking, once per frame).
    /// On success the coordinate becomes the route origin, the map recentres,
    /// a reverse-geocode refines the label, and the route/transit re-plans. On
    /// failure an honest "Location unavailable" message is shown — never a
    /// fabricated position.
    fn drain_location(&mut self) {
        let Some(rx) = &self.location_rx else { return };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.locating = false;
                self.location_rx = None;
                self.model.status = "Location unavailable".to_string();
                self.mark_dirty();
                return;
            }
        };
        self.locating = false;
        self.location_rx = None;
        match outcome {
            LocationOutcome::Located { lat, lon, .. } => {
                let pos = LatLon::new(lat, lon);
                self.model.origin = Some(pos);
                self.model.origin_name = "Current location".to_string();
                self.model.center = pos;
                self.model.status = "Located".to_string();
                // Refine the raw coordinate into a human place label.
                if self.online {
                    self.pending += 1;
                    let tx = self.net_tx.clone();
                    std::thread::spawn(move || {
                        let r = net::reverse_geocode(pos.lat, pos.lon);
                        let _ = tx.send(NetMsg::ReverseGeocode(r));
                    });
                }
                // If a destination is known, re-plan from the real location.
                if self.model.dest.is_some() {
                    if self.model.mode.is_transit() {
                        self.recompute_transit();
                    } else {
                        self.recompute_route();
                    }
                }
            }
            LocationOutcome::Unavailable(reason) => {
                self.model.status = format!("Location unavailable — {reason}");
            }
        }
        self.mark_dirty();
    }

    // --- click dispatch ----------------------------------------------------
    #[allow(
        clippy::too_many_lines,
        reason = "one match arm per Hit variant; the length is the number of click \
                  targets the map surface exposes, not accidental complexity in any one arm"
    )]
    fn dispatch(&mut self, hit: &Hit, doc: &mut AutoCommit) {
        match hit {
            Hit::Consume => {}
            Hit::Search(hit) => match self.search.handle_hit(hit) {
                SearchEffect::None => {}
                SearchEffect::Clear => {
                    self.model.query.clear();
                    self.model.results.clear();
                    self.mark_dirty();
                }
                SearchEffect::Pick(place) => {
                    self.model.upsert_place(place.clone());
                    place.name.clone_into(&mut self.model.query);
                    self.model.push_recent(&place.name);
                    self.select_place(&place.id);
                }
            },
            Hit::ToggleCollapse => {
                self.panel_collapsed = !self.panel_collapsed;
                self.mark_dirty();
            }
            Hit::PanelBack => {
                // Return from the Directions view to the plain places list. The
                // route stays in the model (it keeps drawing on the map); only
                // the panel view changes — this is navigation, not a tab.
                self.model.panel = Panel::Explore;
                self.edit_field = None;
                self.endpoint_debounce = None;
                self.scroll = 0.0;
                self.mark_dirty();
            }
            Hit::Mode(m) => self.set_mode(*m),
            Hit::SelectPlace(id) => {
                self.select_place(id);
            }
            Hit::SelectPoi(id) => self.select_poi_from_list(id),
            Hit::ClosePoi => {
                self.selected_poi = None;
                self.mark_dirty();
            }
            Hit::PoiDirections => self.poi_directions(),
            Hit::Recent(q) => {
                q.clone_into(&mut self.model.query);
                self.run_search();
            }
            Hit::ToggleNav => {
                self.nav_open = !self.nav_open;
                self.mark_dirty();
            }
            Hit::CloseNav => {
                self.nav_open = false;
                self.mark_dirty();
            }
            Hit::Directions => self.enter_directions(),
            Hit::Transit => self.enter_transit(),
            Hit::ToggleSave => self.toggle_save_selected(doc),
            Hit::ClearRoute => self.clear_route(doc),
            Hit::SwapEnds => self.swap_ends(),
            Hit::EditOrigin => self.focus_endpoint(EndField::Origin),
            Hit::EditDest => self.focus_endpoint(EndField::Dest),
            Hit::PickOrigin(i) => self.pick_endpoint(true, *i),
            Hit::PickDest(i) => self.pick_endpoint(false, *i),
            Hit::UseMyLocation => self.use_my_location(),
            Hit::TransitTimeMode(m) => {
                self.model.transit_time.mode = *m;
                self.replan_for_time();
            }
            Hit::TransitTimeDelta(d) => {
                let v = (self.model.transit_time.offset_min + *d).clamp(0, 1440);
                self.model.transit_time.offset_min = v;
                self.replan_for_time();
            }
            Hit::TransitTimeNow => {
                self.model.transit_time = crate::model::TransitTime::default();
                self.replan_for_time();
            }
            Hit::Journey(i) => {
                self.model.transit_choice = *i;
                // Frame the newly-selected itinerary on the map (its legs are
                // what draws now), not just expand the panel card.
                self.fit_current();
                self.mark_dirty();
            }
            Hit::ToggleOverflow => {
                if self.overflow.is_open() {
                    self.overflow.close();
                } else {
                    let b = self.overflow_rect();
                    // Open provisionally to measure the menu, then re-anchor it
                    // above-left of the button so it expands up from the corner.
                    self.overflow.open_at([b.x1 as f32, b.y0 as f32]);
                    let mr = self.overflow.get_rect();
                    self.overflow.open_at([
                        (b.x1 - mr.width()) as f32,
                        (b.y0 - mr.height() - 6.0) as f32,
                    ]);
                }
            }
            Hit::CloseMapInfo => {
                self.map_info = None;
            }
        }
    }

    fn dispatch_map_control(&mut self, action: MapControlAction) {
        match action {
            MapControlAction::ZoomIn => self.zoom_by(1.0, self.view_center()),
            MapControlAction::ZoomOut => self.zoom_by(-1.0, self.view_center()),
            MapControlAction::Locate => self.use_my_location(),
            MapControlAction::ToggleTraffic => {
                self.set_traffic(!self.map_controls.traffic_enabled());
            }
        }
    }

    /// Apply an effect requested by the self-contained offline panel.
    fn dispatch_offline(&mut self, action: OfflineAction, doc: &mut AutoCommit) {
        match action {
            OfflineAction::Close => self.offline_panel.close(),
            OfflineAction::ZoomDelta(delta) => self.offline_panel.adjust_zoom(delta),
            OfflineAction::Download => {
                let request = self.offline_panel.download_request(
                    self.viewport_bounds(),
                    TileLayer::tile_zoom(self.model.zoom),
                    self.rect,
                );
                let count = self
                    .tiles
                    .start_download(request.bounds, request.z0, request.z1);
                self.record_region(
                    doc,
                    request.bounds,
                    request.z0,
                    request.z1,
                    count,
                    request.name.clone(),
                );
                self.model.status = request.name.map_or_else(
                    || format!("Downloading {count} tiles for offline use…"),
                    |name| format!("Downloading {count} tiles for {name}…"),
                );
                self.mark_dirty();
            }
            OfflineAction::DeleteRegion(id) => self.delete_region(doc, &id),
            OfflineAction::ClearCache => {
                let freed = self.tiles.clear_cache();
                self.offline_panel.set_cache_bytes(0);
                self.perform(doc, &MapsAction::ClearRegions);
                self.model.status = format!("Cleared {} of offline tiles", fmt_bytes(freed));
                self.mark_dirty();
            }
            OfflineAction::Consume => {}
        }
    }
}

/// Whether `outer` fully contains `inner` (both lat/lon extents).
fn bounds_contains(outer: GeoBounds, inner: GeoBounds) -> bool {
    outer.min.lat <= inner.min.lat
        && outer.min.lon <= inner.min.lon
        && outer.max.lat >= inner.max.lat
        && outer.max.lon >= inner.max.lon
}

/// Loose keyword → category synonyms for offline search.
fn keyword_match(needle: &str, cat: Category) -> bool {
    let syn: &[&str] = match cat {
        Category::Cafe => &["coffee", "cafe", "café", "espresso", "breakfast"],
        Category::Restaurant => &["food", "eat", "lunch", "dinner", "restaurant"],
        Category::Bar => &["beer", "drink", "pub", "bar", "biergarten"],
        Category::Museum => &["art", "museum", "gallery", "history"],
        Category::Park => &["park", "green", "garden", "nature"],
        Category::Transit => &["station", "train", "metro", "transit", "bahnhof"],
        Category::Hotel => &["hotel", "stay", "sleep", "room"],
        Category::Shop => &["shop", "market", "store", "buy"],
        Category::Landmark => &["sight", "landmark", "tourist"],
        _ => &[],
    };
    syn.iter()
        .any(|s| s.contains(needle) || needle.contains(*s))
}

fn vmid(y: f64, h: f64, size: f32) -> f64 {
    y + (h - f64::from(size) * 1.25) / 2.0
}

/// Human-readable byte count for the cache-size readouts.
fn fmt_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if n >= GB {
        format!("{:.1} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{} KB", n / KB)
    } else {
        format!("{n} B")
    }
}

// ===========================================================================
// Drawing
// ===========================================================================
impl MapsApp {
    fn draw_app(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        self.click_targets.clear();
        self.map_controls.set_viewport(self.rect);
        // Point the raster layer at the basemap style for the active theme (dark
        // tiles under a dark theme, light under light) before any tile is drawn,
        // so imagery and the drawn cartography never mix tones. A no-op on the
        // steady state; on a theme change it re-points the worker + cache.
        self.tiles.set_style(TileStyle::from_theme(theme));
        let pal = MapPalette::from_theme(theme);
        let proj = self.projection();
        let clip = self.rect;

        // When the narrow overlay drawer is open it floats over the map. Clip the
        // marker/label layers to the map area *beside* it, so pins and their name
        // plates are never sliced at the drawer's edge into unreadable fragments
        // (docked/closed layouts keep the full clip).
        let overlay = self.overlay_mode();
        let map_clip = if overlay && self.nav_open {
            let left = (self.panel_rect().x1 + GAP).min(clip.x1);
            Rect::new(left, clip.y0, clip.x1, clip.y1)
        } else {
            clip
        };

        let painted_tiles = self.draw_map_layers(scene, t, tr, theme, &pal, proj, clip, map_clip);
        self.draw_panel_and_scale(scene, t, tr, theme, overlay, clip, painted_tiles);
        self.draw_chrome(scene, t, tr, theme);
    }

    /// Map layers only: real tiles or their vector/grid fallback, traffic,
    /// route, transit, POIs and markers. Returns whether real imagery painted,
    /// which the caller needs to decide whether the offline/loading hint is due.
    #[allow(
        clippy::too_many_arguments,
        reason = "the frame's shared draw context (scene/transform/theme/palette/projection/clip \
                  rects), computed once in draw_app and threaded through unchanged"
    )]
    fn draw_map_layers(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        pal: &MapPalette,
        proj: Projection,
        clip: Rect,
        map_clip: Rect,
    ) -> bool {
        // Map layers. Real OSM raster tiles paint first (non-blocking); the
        // stylised vector basemap is the fallback shown only until imagery is
        // available (headless `--shots`, or the very first offline frame).
        let view_center = self.view_center();
        let painted_tiles = canvas::draw_tiles(
            scene,
            t,
            clip,
            &mut self.tiles,
            &self.model,
            view_center,
            pal,
        );
        if !painted_tiles {
            if self.basemap.is_empty() {
                // Live first-run / offline gap: no imagery and no vector
                // cartography yet. Draw a reference grid so the map reads as a
                // map rather than a black void, with a loading affordance.
                canvas::draw_grid_substrate(scene, t, clip, &self.model, view_center, pal);
            } else {
                canvas::draw_basemap(scene, t, clip, &proj, &self.basemap, pal);
            }
        }
        // Traffic overlay: coloured congestion over the roads, under the active
        // route + markers so navigation stays legible. Empty data draws
        // nothing here — the honest "No traffic data" pill is drawn with the
        // chrome below.
        if self.map_controls.traffic_enabled() {
            canvas::draw_traffic(scene, t, &proj, &self.traffic, pal);
        }
        if let Some(route) = &self.model.route {
            canvas::draw_route(scene, t, &proj, route, pal);
        }
        // Public-transport itinerary: the selected journey's legs draw on the
        // map (mode-coloured lines + transfer pucks) when Transit is the active
        // mode. Only the highlighted option is drawn to keep the map legible.
        if self.model.mode.is_transit()
            && let Some(j) = self.model.transit.get(self.model.transit_choice)
        {
            canvas::draw_transit(scene, t, &proj, j, theme, pal, clip);
        }
        // Live POI marker layer (Overpass) — our own themed category markers
        // over the basemap, decluttered and under the user's saved/selected
        // pins.
        let visible_pois = self.visible_poi_indices();
        canvas::draw_pois(
            scene,
            t,
            tr,
            &proj,
            &self.pois,
            &visible_pois,
            self.selected_poi.as_deref(),
            theme,
            pal,
            map_clip,
        );
        canvas::draw_markers(scene, t, tr, &proj, &self.model, theme, pal, map_clip);
        painted_tiles
    }

    /// Side panel (docked or overlay drawer + scrim), the scale bar, and the
    /// offline/loading hint that only shows while no real imagery has painted.
    #[allow(
        clippy::too_many_arguments,
        reason = "the frame's shared draw context plus the two map-layer outcomes \
                  (`overlay`, `painted_tiles`) this section's branches depend on"
    )]
    fn draw_panel_and_scale(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        overlay: bool,
        clip: Rect,
        painted_tiles: bool,
    ) {
        // Side panel: docked when wide, an overlay drawer (with a dimming
        // scrim over the map) when narrow. While the floating search-results
        // overlay is up it stands in for the panel — suppress the list behind it
        // so the same place never appears twice (dropdown + panel). The panel
        // returns as soon as the results dismiss.
        let show_panel = !self.search_overlay_active();
        if overlay && self.nav_open {
            draw::fill_rect(scene, t, clip, draw::alpha(Color::BLACK, 0.5));
            self.click_targets.push((clip, Hit::CloseNav));
            if show_panel {
                self.draw_panel(scene, t, tr, theme);
            }
        } else if !overlay && show_panel {
            self.draw_panel(scene, t, tr, theme);
        }

        // Scale bar, bottom-right, ending just left of the ⋯ overflow button.
        // Attribution/licenses moved into that menu, so nothing overlaps it.
        canvas::draw_scale_bar(
            scene,
            t,
            tr,
            clip,
            self.overflow_rect().x0 - GAP,
            &self.model,
            theme,
        );

        // Loading / offline affordance while the substrate grid is showing —
        // above the map but below the chrome, and skipped once real imagery or
        // the drawer scrim is up (the drawer is the focus then).
        if !painted_tiles && self.basemap.is_empty() && !(overlay && self.nav_open) {
            self.draw_imagery_hint(scene, t, tr, theme);
        }
    }

    /// Chrome: controls, menus, search, floating cards/dropdowns, the offline
    /// panel, the overflow menu + info sheet, and the hover tooltip on top.
    fn draw_chrome(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        // Chrome on top of the scrim so it stays bright and usable.
        self.map_controls.draw(scene, t, tr, theme);
        self.draw_traffic_status(scene, t, tr, theme);
        self.draw_menu(scene, t, tr, theme);
        self.draw_search(scene, t, tr, theme);
        self.draw_status(scene, t, tr, theme);
        // Floating POI info card (map overlay) above the chrome so its actions
        // win the hit-test.
        self.draw_poi_card(scene, t, tr, theme);
        // Floating search-results dropdown, above the chrome so it overlays the
        // map/panel rather than reflowing them.
        self.draw_search_overlay(scene, t, tr, theme);
        // Floating Directions From/To results dropdown — same idiom, anchored
        // under the focused endpoint field so the panel below never reflows.
        self.draw_endpoint_overlay(scene, t, tr, theme);
        if self.offline_panel.is_open() {
            let viewport_bounds = self.viewport_bounds();
            self.offline_panel.draw(
                scene,
                t,
                tr,
                theme,
                OfflineFrame {
                    viewport: self.rect,
                    viewport_bounds,
                    viewport_zoom: TileLayer::tile_zoom(self.model.zoom),
                    regions: &self.model.regions,
                    progress: self.tiles.progress(),
                },
            );
        }
        // The ⋯ overflow button (attribution / offline / licenses) sits bottom-
        // right; its menu and the info sheet draw last, on top of everything.
        self.draw_overflow(scene, t, tr, theme);
        if let Some(info) = self.map_info {
            self.draw_map_info_sheet(scene, t, tr, theme, info);
        }
        if self.overflow.is_open() {
            self.overflow
                .draw(scene, t, tr, theme, &PresenceCtx::empty());
        }
        self.map_controls.draw_tooltip(scene, t, tr, theme);
    }

    /// The bottom-right ⋯ button. When the overflow menu is open (or a download
    /// is running) it wears an accent border, so an in-progress offline download
    /// stays visible even with the dialog closed.
    fn draw_overflow(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        _tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let r = self.overflow_rect();
        let c = &theme.tokens.colors;
        let active = self.overflow.is_open() || self.tiles.progress().is_active();
        let border = if active {
            theme.accent_primary
        } else {
            c.border_standard
        };
        draw::card(scene, t, r, 10.0, c.surfaces.raised, border);
        let ctr = r.center();
        let col = if self.overflow.is_open() {
            theme.accent_primary
        } else {
            theme.text_primary
        };
        for dx in [-6.0, 0.0, 6.0] {
            draw::fill_circle(scene, t, Point::new(ctr.x + dx, ctr.y), 1.9, col);
        }
        self.click_targets.push((r, Hit::ToggleOverflow));
    }

    /// A small centred sheet with the map-data attribution or licenses, reached
    /// from the ⋯ menu. Honest, one line per credit/term — the OSM/CARTO
    /// requirement met by being reachable rather than pasted over the map.
    #[allow(
        clippy::many_single_char_names,
        reason = "conventional short names for the shared drawing context (scene, t: transform, \
              tr: text renderer, c: theme colours, r: rect) used consistently across every \
              draw_* method in this file"
    )]
    fn draw_map_info_sheet(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        info: MapInfo,
    ) {
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, self.rect, draw::alpha(Color::BLACK, 0.45));
        self.click_targets.push((self.rect, Hit::CloseMapInfo));

        let (title, lines): (&str, &[&str]) = match info {
            MapInfo::Attribution => (
                "Attribution",
                &[
                    "Map data © OpenStreetMap contributors",
                    "Basemap tiles © CARTO",
                ],
            ),
            MapInfo::Licenses => (
                "Licenses",
                &[
                    "OpenStreetMap data — Open Database License (ODbL)",
                    "CARTO basemaps — under CARTO's basemap terms",
                    "© OpenStreetMap contributors · © CARTO",
                ],
            ),
        };

        let w = 440.0_f64.min(self.rect.width() - 40.0);
        let row_h = 26.0;
        let h = 64.0 + lines.len() as f64 * row_h + 16.0;
        let cx = f64::midpoint(self.rect.x0, self.rect.x1);
        let cy = f64::midpoint(self.rect.y0, self.rect.y1);
        let card = Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
        draw::card(scene, t, card, 16.0, c.surfaces.raised, c.border_standard);
        self.click_targets.push((card, Hit::Consume));

        let x = card.x0 + 22.0;
        let right = card.x1 - 22.0;
        let mut y = card.y0 + 20.0;
        draw::text_w(
            scene,
            tr,
            t,
            title,
            x,
            y,
            17.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        let close = Rect::new(right - 26.0, card.y0 + 16.0, right, card.y0 + 42.0);
        draw::text_center(
            scene,
            tr,
            t,
            "✕",
            close.x0,
            close.x1,
            vmid(close.y0, 26.0, 14.0),
            14.0,
            theme.text_dim,
        );
        self.click_targets.push((close, Hit::CloseMapInfo));
        y += 36.0;
        for line in lines {
            draw::text_clip(
                scene,
                tr,
                t,
                line,
                x,
                y,
                12.5,
                theme.text_primary,
                right - x,
            );
            y += row_h;
        }
    }

    /// The leading menu button in the narrow overlay layout: toggles the panel
    /// drawer. Renders nothing when the panel is docked.
    fn draw_menu(&mut self, scene: &mut Scene, t: Affine, _tr: &mut TextRenderer, theme: &Theme) {
        let Some(r) = self.menu_rect() else { return };
        let c = &theme.tokens.colors;
        let border = if self.nav_open {
            theme.border_focus
        } else {
            c.border_standard
        };
        draw::card(scene, t, r, 12.0, c.surfaces.raised, border);
        // Three-line hamburger, or a back-chevron feel when open (rotate hint
        // via colour). Kept as clean strokes — no icon font dependency.
        let cx = r.center().x;
        let cy = r.center().y;
        let hw = 8.0;
        let col = if self.nav_open {
            theme.accent_primary
        } else {
            theme.text_primary
        };
        for dy in [-5.0, 0.0, 5.0] {
            draw::line(
                scene,
                t,
                Point::new(cx - hw, cy + dy),
                Point::new(cx + hw, cy + dy),
                2.0,
                col,
            );
        }
        self.click_targets.push((r, Hit::ToggleNav));
    }

    fn draw_search(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let r = self.search_rect();
        self.search
            .draw_field(scene, t, tr, theme, r, &self.model.query);
        self.click_targets.extend(
            self.search
                .take_targets()
                .into_iter()
                .map(|(rect, hit)| (rect, Hit::Search(hit))),
        );
    }

    /// Whether the floating search-results overlay is currently on screen.
    fn search_overlay_active(&self) -> bool {
        self.search.overlay_active(&self.model.query)
    }

    /// Floating results overlay anchored directly under the search field. It
    /// draws OVER the map (and, in docked layouts, over the top of the side
    /// panel) as a dropdown — it deliberately never reflows the panel or the
    /// map to make room. Rows are proximity-ordered with a distance chip, so
    /// the nearest match to where the user is looking reads first.
    fn draw_search_overlay(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let rect = self.search_rect();
        let anchor = self.search_anchor();
        self.search.draw_overlay(
            scene,
            t,
            tr,
            theme,
            SearchOverlay {
                rect,
                query: &self.model.query,
                selected_id: self.model.selected.as_deref(),
                anchor,
            },
        );
        self.click_targets.extend(
            self.search
                .take_targets()
                .into_iter()
                .map(|(rect, hit)| (rect, Hit::Search(hit))),
        );
    }

    /// Whether the floating endpoint (From/To) results overlay is on screen.
    #[cfg(test)]
    fn endpoint_overlay_active(&self) -> bool {
        let Some(field) = self.edit_field else {
            return false;
        };
        if !matches!(self.model.panel, Panel::Directions) {
            return false;
        }
        let (anchor, results) = match field {
            EndField::Origin => (self.origin_field_rect, &self.origin_results),
            EndField::Dest => (self.dest_field_rect, &self.dest_results),
        };
        anchor.is_some() && !results.is_empty()
    }

    /// Floating results dropdown for the focused Directions endpoint, anchored
    /// directly under its From/To field and drawn OVER the map — the exact same
    /// dropdown idiom as the top search overlay. It deliberately never reflows
    /// the panel below (mode control, route, steps stay put whether or not the
    /// dropdown is open), which was the old inline-list bug.
    #[allow(
        clippy::many_single_char_names,
        reason = "conventional short names for the shared drawing context (scene, t: transform, \
              tr: text renderer, c: theme colours, r: rect) used consistently across every \
              draw_* method in this file"
    )]
    fn draw_endpoint_overlay(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(field) = self.edit_field else { return };
        if !matches!(self.model.panel, Panel::Directions) {
            return;
        }
        let (anchor, results, origin) = match field {
            EndField::Origin => (self.origin_field_rect, self.origin_results.clone(), true),
            EndField::Dest => (self.dest_field_rect, self.dest_results.clone(), false),
        };
        let Some(fr) = anchor else { return };
        let c = &theme.tokens.colors;
        let x = fr.x0;
        let w = fr.width();
        let top = fr.y1 + 6.0;

        if results.is_empty() {
            // While the live geocode is still resolving, show a "Searching…"
            // hint so the field reads as actively searching — no Enter needed.
            let q = if origin {
                &self.model.origin_query
            } else {
                &self.model.dest_query
            };
            if q.trim().is_empty() {
                return;
            }
            let card = Rect::new(x, top, x + w, top + 34.0);
            draw::card(scene, t, card, 12.0, c.surfaces.raised, c.border_standard);
            self.click_targets.push((card, Hit::Consume));
            draw::text(
                scene,
                tr,
                t,
                "Searching…",
                card.x0 + 12.0,
                vmid(card.y0, card.height(), 12.0),
                12.0,
                theme.text_dim,
            );
            return;
        }

        let row_h = 46.0;
        let pad = 6.0;
        let n = results.len().min(ENDPOINT_OVERLAY_MAX);
        let h = pad * 2.0 + n as f64 * row_h;
        let card = Rect::new(x, top, x + w, top + h);
        draw::card(scene, t, card, 12.0, c.surfaces.raised, c.border_standard);
        self.click_targets.push((card, Hit::Consume));

        let anchor_pt = self.search_anchor();
        let mut y = top + pad;
        for (i, p) in results.iter().enumerate().take(n) {
            let r = Rect::new(x + pad, y, x + w - pad, y + row_h - 2.0);
            let cat_col = canvas::category_color(theme, p.category);
            draw::fill_circle(
                scene,
                t,
                Point::new(r.x0 + 14.0, r.center().y),
                6.0,
                cat_col,
            );
            let tx = r.x0 + 30.0;
            let dist = geo::fmt_distance(geo::haversine_m(p.pos, anchor_pt));
            let dw = draw::measure_w(tr, &dist, 11.0) + 8.0;
            draw::text_clip(
                scene,
                tr,
                t,
                &p.name,
                tx,
                r.y0 + 6.0,
                13.0,
                theme.text_primary,
                r.x1 - tx - dw,
            );
            draw::text_clip(
                scene,
                tr,
                t,
                &p.address,
                tx,
                r.y0 + 24.0,
                11.0,
                theme.text_dim,
                r.x1 - tx - dw,
            );
            draw::text_right(
                scene,
                tr,
                t,
                &dist,
                r.x1 - 8.0,
                r.y0 + 8.0,
                11.0,
                theme.accent_primary,
            );
            let hit = if origin {
                Hit::PickOrigin(i)
            } else {
                Hit::PickDest(i)
            };
            self.click_targets.push((r, hit));
            y += row_h;
        }
    }

    /// Floating info card for the selected POI, anchored at the bottom of the
    /// visible map area (clear of the docked panel and the right-hand controls).
    /// Surfaces the readable OSM tags — address, opening hours, phone, website —
    /// with a Directions action. Drawn late so it overlays the map and its
    /// controls win the hit-test.
    #[allow(
        clippy::too_many_lines,
        reason = "a single linear draw pass over this panel's fixed regions; splitting it would only \
              relocate the line count, not the amount a reader must hold in mind"
    )]
    #[allow(
        clippy::many_single_char_names,
        reason = "conventional short names for the shared drawing context (scene, t: transform, \
              tr: text renderer, c: theme colours, r: rect) used consistently across every \
              draw_* method in this file"
    )]
    fn draw_poi_card(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(id) = self.selected_poi.clone() else {
            return;
        };
        let Some(poi) = self.pois.iter().find(|p| p.id == id).cloned() else {
            return;
        };
        let c = &theme.tokens.colors;

        // Detail rows we actually have tags for (glyph, text, is-link).
        let mut rows: Vec<(&str, String, bool)> = Vec::new();
        if !poi.address.is_empty() {
            rows.push(("◎", poi.address.clone(), false));
        }
        if let Some(oh) = &poi.opening_hours {
            rows.push(("◷", oh.clone(), false));
        }
        if let Some(ph) = &poi.phone {
            rows.push(("✆", ph.clone(), false));
        }
        if let Some(web) = &poi.website {
            rows.push(("⚓", web.clone(), true));
        }

        let area_x0 = if self.panel_docked_expanded() {
            self.panel_rect().x1 + GAP
        } else {
            self.rect.x0 + MARGIN
        };
        let area_x1 = self.rect.x1 - MARGIN;
        let w = 360.0_f64.min((area_x1 - area_x0).max(200.0));
        let row_h = 24.0;
        let card_h = 118.0 + rows.len() as f64 * row_h;
        let cx = f64::midpoint(area_x0, area_x1);
        let x0 = (cx - w / 2.0).max(area_x0);
        let y1 = self.rect.y1 - 54.0; // above the scale bar + attribution
        let y0 = y1 - card_h;
        let card = Rect::new(x0, y0, x0 + w, y1);
        draw::card(scene, t, card, 16.0, c.surfaces.raised, c.border_standard);
        self.click_targets.push((card, Hit::Consume));

        // Category colour bar down the leading edge.
        draw::fill_rrect(
            scene,
            t,
            Rect::new(card.x0, card.y0, card.x0 + 5.0, card.y1),
            16.0,
            canvas::category_color(theme, poi.category),
        );

        let pad = 16.0;
        let x = card.x0 + pad;
        let right = card.x1 - pad;
        let mut y = card.y0 + 14.0;
        // Close (×).
        let close = Rect::new(right - 20.0, card.y0 + 12.0, right + 6.0, card.y0 + 38.0);
        draw::text_center(
            scene,
            tr,
            t,
            "✕",
            close.x0,
            close.x1,
            y,
            14.0,
            theme.text_dim,
        );
        self.click_targets.push((close, Hit::ClosePoi));
        draw::text_w(
            scene,
            tr,
            t,
            &poi.name,
            x,
            y,
            18.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        y += 26.0;
        draw::text(
            scene,
            tr,
            t,
            poi.category.label(),
            x,
            y,
            12.0,
            theme.accent_primary,
        );
        if let Some(k) = &poi.kind {
            let cw = draw::measure_w(tr, poi.category.label(), 12.0);
            draw::text_clip(
                scene,
                tr,
                t,
                &format!("· {k}"),
                x + cw + 10.0,
                y,
                12.0,
                theme.text_dim,
                right - x - cw - 10.0,
            );
        }
        y += 24.0;
        for (glyph, text, link) in &rows {
            draw::text(scene, tr, t, glyph, x, y, 12.0, theme.text_dim);
            let col = if *link {
                theme.accent_primary
            } else {
                theme.text_primary
            };
            draw::text_clip(scene, tr, t, text, x + 20.0, y, 12.0, col, right - x - 20.0);
            y += row_h;
        }
        y += 6.0;
        // Directions (primary action).
        let bh = 36.0;
        let btn = Rect::new(x, y, right, y + bh);
        draw::fill_rrect(scene, t, btn, 9.0, theme.accent_primary);
        draw::text_center(
            scene,
            tr,
            t,
            "Directions",
            btn.x0,
            btn.x1,
            vmid(btn.y0, bh, 13.0),
            13.0,
            Color::WHITE,
        );
        self.click_targets.push((btn, Hit::PoiDirections));
    }

    /// The traffic legend / status, bottom-left of the map area (clear of the
    /// docked panel and the bottom-right scale bar). With data it's a colour
    /// key with per-level counts + provenance; with none it's the honest
    /// "No traffic data" pill. Draws nothing when the overlay is off.
    #[allow(
        clippy::too_many_lines,
        reason = "a single linear draw pass over this panel's fixed regions; splitting it would only \
              relocate the line count, not the amount a reader must hold in mind"
    )]
    #[allow(
        clippy::many_single_char_names,
        reason = "conventional short names for the shared drawing context (scene, t: transform, \
              tr: text renderer, c: theme colours, r: rect) used consistently across every \
              draw_* method in this file"
    )]
    fn draw_traffic_status(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.map_controls.traffic_enabled() {
            return;
        }
        // While the narrow drawer is open the map is dimmed and the drawer is
        // the focus — don't stack a legend over it.
        if self.overlay_mode() && self.nav_open {
            return;
        }
        let c = &theme.tokens.colors;
        let pal = MapPalette::from_theme(theme);
        let map_left = if self.panel_docked_expanded() {
            self.panel_rect().x1 + GAP
        } else {
            self.rect.x0 + MARGIN
        };
        let bottom = self.rect.y1 - MARGIN;

        if self.traffic.is_empty() {
            // Honest empty state — no fabricated congestion.
            let label = "No traffic data";
            let fs = 12.0;
            let h = 32.0;
            let w = draw::measure_w(tr, label, fs) + 40.0;
            let pill = Rect::new(map_left, bottom - h, map_left + w, bottom);
            draw::card(
                scene,
                t,
                pill,
                h / 2.0,
                draw::alpha(c.surfaces.raised, 0.96),
                c.border_standard,
            );
            draw::fill_circle(
                scene,
                t,
                Point::new(pill.x0 + 16.0, pill.center().y),
                3.6,
                c.text_dim,
            );
            draw::text(
                scene,
                tr,
                t,
                label,
                pill.x0 + 27.0,
                vmid(pill.y0, h, fs),
                fs,
                c.text_primary,
            );
            return;
        }

        // Populated legend: a compact vertical colour key.
        let counts = self.traffic.counts();
        let title_fs = 12.0;
        let row_fs = 11.0;
        let row_h = 19.0;
        let pad = 12.0;
        let header_h = 22.0;
        let w = 180.0_f64;
        let h = pad + header_h + Congestion::ALL.len() as f64 * row_h + pad - 4.0;
        let card = Rect::new(map_left, bottom - h, map_left + w, bottom);
        draw::card(
            scene,
            t,
            card,
            12.0,
            draw::alpha(c.surfaces.raised, 0.96),
            c.border_standard,
        );

        let x = card.x0 + pad;
        let mut y = card.y0 + pad;
        draw::text_w(
            scene,
            tr,
            t,
            "Traffic",
            x,
            y,
            title_fs,
            c.text_primary,
            TextWeight::Semibold,
        );
        if let Some(src) = &self.traffic.source {
            let tw = draw::measure_w(tr, "Traffic", title_fs) + 8.0;
            draw::text_clip(
                scene,
                tr,
                t,
                &format!("· {src}"),
                x + tw,
                y,
                10.0,
                c.text_dim,
                card.x1 - pad - (x + tw),
            );
        }
        y += header_h;
        for level in Congestion::ALL {
            let sw = Rect::new(x, y + 3.0, x + 18.0, y + 3.0 + 6.0);
            draw::fill_rrect(scene, t, sw, 3.0, canvas::traffic_color(&pal, level));
            draw::text(
                scene,
                tr,
                t,
                level.label(),
                x + 26.0,
                y,
                row_fs,
                c.text_primary,
            );
            let n = counts.get(level as usize).copied().unwrap_or(0).to_string();
            let nw = draw::measure_w(tr, &n, row_fs);
            draw::text(scene, tr, t, &n, card.x1 - pad - nw, y, row_fs, c.text_dim);
            y += row_h;
        }
    }

    /// The offline-download + manage-areas overlay: a centred card with a
    /// zoom-depth stepper, an estimated-tile/progress readout, a list of the
    /// areas already saved for offline use (each removable), and
    fn draw_status(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        // Suppress the top-centre pill while the search dropdown is open — the
        // overlay sits in the same spot and the search feedback lives there.
        if self.search_overlay_active() {
            return;
        }
        // Only surface transient/loading status as a top-centre pill — steady
        // attribution lives in the bottom bar, so we don't duplicate it here.
        if self.pending == 0 && !self.model.status.contains('…') {
            return;
        }
        let label = &self.model.status;
        if label.is_empty() {
            return;
        }
        let size = 12.0_f32;
        let w = draw::measure_w(tr, label, size) + 36.0;
        let cx = f64::midpoint(self.rect.x0, self.rect.x1);
        // Sit below the top search row so it never overlaps the search field
        // (which spans most of the top edge in the narrow layout).
        let top = self.rect.y0 + MARGIN + SEARCH_H + GAP;
        let r = Rect::new(cx - w / 2.0, top, cx + w / 2.0, top + 30.0);
        draw::card(
            scene,
            t,
            r,
            15.0,
            theme.tokens.colors.surfaces.raised,
            theme.tokens.colors.border_standard,
        );
        // Small spinner dot.
        let dot = Point::new(r.x0 + 16.0, r.center().y);
        draw::fill_circle(scene, t, dot, 4.0, theme.accent_primary);
        draw::text(
            scene,
            tr,
            t,
            label,
            r.x0 + 28.0,
            vmid(r.y0, r.height(), size),
            size,
            theme.text_primary,
        );
    }

    /// A centred pill over the substrate grid telling the user the imagery is
    /// loading (or unreachable), so a first-run / offline map is never a silent
    /// void. Positioned in the visible map area, clear of the docked panel.
    fn draw_imagery_hint(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let offline = self.tiles.has_failures() && !self.tiles.has_inflight();
        let msg = if offline {
            "Map imagery unavailable offline"
        } else {
            "Loading map…"
        };
        // Centre within the map area (right of the docked panel when present).
        let area_x0 = if self.panel_docked_expanded() {
            self.panel_rect().x1 + GAP
        } else {
            self.rect.x0
        };
        let cx = f64::midpoint(area_x0, self.rect.x1);
        let cy = f64::midpoint(self.rect.y0, self.rect.y1);
        let size = 13.0_f32;
        let w = draw::measure_w(tr, msg, size) + 44.0;
        let r = Rect::new(cx - w / 2.0, cy - 17.0, cx + w / 2.0, cy + 17.0);
        let c = &theme.tokens.colors;
        draw::card(scene, t, r, 17.0, c.surfaces.raised, c.border_standard);
        let dot = Point::new(r.x0 + 18.0, r.center().y);
        let dot_col = if offline {
            c.semantic.warning
        } else {
            theme.accent_primary
        };
        draw::fill_circle(scene, t, dot, 4.0, dot_col);
        draw::text(
            scene,
            tr,
            t,
            msg,
            r.x0 + 30.0,
            vmid(r.y0, r.height(), size),
            size,
            theme.text_primary,
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "a single linear draw pass over this panel's fixed regions; splitting it would only \
              relocate the line count, not the amount a reader must hold in mind"
    )]
    fn draw_panel(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let pr = self.panel_rect();
        if pr.height() < 80.0 {
            return;
        }
        let c = &theme.tokens.colors;
        let inset = 10.0;
        let docked = !self.overlay_mode();
        let directions = matches!(self.model.panel, Panel::Directions);

        // Collapsed (docked only): show just a header bar with an expand
        // affordance so the map reclaims the column. A plain collapse/expand
        // toggle — not a mode/tab.
        if self.panel_collapsed && docked {
            let bar = Rect::new(pr.x0, pr.y0, pr.x1, pr.y0 + PANEL_HEADER_H + 2.0 * inset);
            draw::card(scene, t, bar, 14.0, c.surfaces.raised, c.border_standard);
            self.click_targets.push((bar, Hit::Consume));
            let by = vmid(bar.y0 + inset, PANEL_HEADER_H, 15.0);
            draw::text_w(
                scene,
                tr,
                t,
                "Places",
                bar.x0 + inset + 4.0,
                by,
                15.0,
                theme.text_primary,
                TextWeight::Semibold,
            );
            let btn = Rect::new(
                bar.x1 - inset - PANEL_HEADER_H,
                bar.y0 + inset,
                bar.x1 - inset,
                bar.y1 - inset,
            );
            draw::fill_rrect(scene, t, btn, 8.0, c.surfaces.base);
            draw::text_center(
                scene,
                tr,
                t,
                "»",
                btn.x0,
                btn.x1,
                vmid(btn.y0, btn.height(), 16.0),
                16.0,
                theme.text_primary,
            );
            self.click_targets.push((btn, Hit::ToggleCollapse));
            self.content_view_h = 0.0;
            self.content_h = 0.0;
            return;
        }

        draw::card(scene, t, pr, 14.0, c.surfaces.raised, c.border_standard);
        // Swallow background clicks.
        self.click_targets.push((pr, Hit::Consume));

        // Header row: an optional back button (Directions view), the title, and
        // — when docked — a collapse toggle. This replaces the old
        // Explore/Directions tab strip: the panel is a plain collapsible
        // surface, and Directions is an action reached from a place/POI card,
        // never a top-level tab the user toggles.
        let header = Rect::new(
            pr.x0 + inset,
            pr.y0 + inset,
            pr.x1 - inset,
            pr.y0 + inset + PANEL_HEADER_H,
        );
        let mut hx = header.x0;
        if directions {
            let back = Rect::new(hx, header.y0, hx + PANEL_HEADER_H, header.y1);
            draw::fill_rrect(scene, t, back, 8.0, c.surfaces.base);
            draw::text_center(
                scene,
                tr,
                t,
                "‹",
                back.x0,
                back.x1,
                vmid(back.y0, back.height(), 18.0),
                18.0,
                theme.text_primary,
            );
            self.click_targets.push((back, Hit::PanelBack));
            hx = back.x1 + 8.0;
        }
        let title = if directions { "Directions" } else { "Places" };
        draw::text_w(
            scene,
            tr,
            t,
            title,
            hx + 2.0,
            vmid(header.y0, header.height(), 16.0),
            16.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        if docked {
            let btn = Rect::new(header.x1 - PANEL_HEADER_H, header.y0, header.x1, header.y1);
            draw::fill_rrect(scene, t, btn, 8.0, c.surfaces.base);
            draw::text_center(
                scene,
                tr,
                t,
                "«",
                btn.x0,
                btn.x1,
                vmid(btn.y0, btn.height(), 16.0),
                16.0,
                theme.text_dim,
            );
            self.click_targets.push((btn, Hit::ToggleCollapse));
        }

        // Content region (below the header), clipped + scrolled.
        let content = Rect::new(pr.x0 + inset, header.y1 + 10.0, pr.x1 - inset, pr.y1 - 10.0);
        self.content_view_h = content.height();
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &content);
        let cy = content.y0 - self.scroll;
        let drawn = match self.model.panel {
            Panel::Explore => self.draw_explore(scene, t, tr, theme, content, cy),
            Panel::Directions => self.draw_directions(scene, t, tr, theme, content, cy),
        };
        scene.pop_layer();
        self.content_h = drawn - content.y0 + self.scroll;
    }

    // Returns the y of the bottom of the drawn content (absolute).
    #[allow(
        clippy::too_many_lines,
        reason = "a single linear draw pass over this panel's fixed regions; splitting it would only \
              relocate the line count, not the amount a reader must hold in mind"
    )]
    fn draw_explore(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        mut y: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let x = content.x0;
        let right = content.x1;

        // First-run empty state: nothing selected, searched, saved, recent, or
        // fetched for the viewport. An honest hint toward the primary action
        // rather than a blank panel.
        let empty = self.model.selected_place().is_none()
            && self.model.results.is_empty()
            && self.model.saved_places().is_empty()
            && self.model.recents.is_empty()
            && self.pois.is_empty();
        if empty {
            draw::text_w(
                scene,
                tr,
                t,
                "Find your way",
                x,
                y,
                16.0,
                theme.text_primary,
                TextWeight::Bold,
            );
            draw::text(
                scene,
                tr,
                t,
                "Search for a place or address",
                x,
                y + 28.0,
                13.0,
                theme.text_dim,
            );
            draw::text(
                scene,
                tr,
                t,
                "to route and plan transit.",
                x,
                y + 46.0,
                13.0,
                theme.text_dim,
            );
            return y + 78.0;
        }

        // Selected place card.
        if let Some(sel) = self.model.selected_place().cloned() {
            let card_h = 132.0;
            let cr = Rect::new(x, y, right, y + card_h);
            draw::fill_rrect(scene, t, cr, 12.0, c.surfaces.base);
            // Category swatch.
            let cat_col = canvas::category_color(theme, sel.category);
            draw::fill_rrect(
                scene,
                t,
                Rect::new(cr.x0, cr.y0, cr.x0 + 5.0, cr.y1),
                12.0,
                cat_col,
            );
            draw::text_w(
                scene,
                tr,
                t,
                &sel.name,
                x + 16.0,
                y + 12.0,
                18.0,
                theme.text_primary,
                TextWeight::Bold,
            );
            draw::text(
                scene,
                tr,
                t,
                sel.category.label(),
                x + 16.0,
                y + 38.0,
                12.0,
                theme.accent_primary,
            );
            draw::text_clip(
                scene,
                tr,
                t,
                &sel.address,
                x + 16.0,
                y + 58.0,
                12.0,
                theme.text_dim,
                right - x - 28.0,
            );

            // Action buttons row.
            let by = y + 86.0;
            let bw = (right - x - 16.0 - 16.0) / 3.0;
            let bh = 34.0;
            let saved = sel.saved;
            let actions: [(&str, Hit, bool); 3] = [
                ("Directions", Hit::Directions, true),
                ("Transit", Hit::Transit, false),
                (
                    if saved { "Saved ✓" } else { "Save" },
                    Hit::ToggleSave,
                    false,
                ),
            ];
            for (i, (label, hit, primary)) in actions.into_iter().enumerate() {
                let bx = x + 8.0 + i as f64 * (bw + 4.0);
                let br = Rect::new(bx, by, bx + bw, by + bh);
                let (fill, fg) = if primary {
                    (theme.accent_primary, Color::WHITE)
                } else if matches!(hit, Hit::ToggleSave) && saved {
                    (
                        draw::alpha(theme.accent_primary, 0.18),
                        theme.accent_primary,
                    )
                } else {
                    (c.surfaces.raised, theme.text_primary)
                };
                draw::fill_rrect(scene, t, br, 8.0, fill);
                if !primary {
                    draw::stroke_rrect(scene, t, br, 8.0, 1.0, c.border_standard);
                }
                draw::text_center(
                    scene,
                    tr,
                    t,
                    label,
                    br.x0,
                    br.x1,
                    vmid(br.y0, bh, 12.0),
                    12.0,
                    fg,
                );
                self.click_targets.push((br, hit));
            }
            y += card_h + 14.0;
        }

        // Results / recents / saved sections.
        let results = self.model.results.clone();
        if !results.is_empty() {
            draw::text_w(
                scene,
                tr,
                t,
                "RESULTS",
                x,
                y,
                11.0,
                theme.text_dim,
                TextWeight::Semibold,
            );
            y += 20.0;
            for p in &results {
                y = self.draw_place_row(scene, t, tr, theme, content, y, p);
            }
            y += 8.0;
        }

        // Nearby POIs fetched for the current viewport (Overpass), nearest-first
        // and linked to the marker layer: tapping a row centres the map and
        // opens the POI's info card, and the selected marker highlights its row.
        let nearby = self.nearby_pois();
        if !nearby.is_empty() {
            draw::text_w(
                scene,
                tr,
                t,
                "NEARBY",
                x,
                y,
                11.0,
                theme.text_dim,
                TextWeight::Semibold,
            );
            y += 20.0;
            let anchor = self.search_anchor();
            for poi in &nearby {
                y = self.draw_poi_row(scene, t, tr, theme, content, y, poi, anchor);
            }
            y += 8.0;
        }

        let saved = self
            .model
            .saved_places()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        if !saved.is_empty() {
            draw::text_w(
                scene,
                tr,
                t,
                "SAVED",
                x,
                y,
                11.0,
                theme.text_dim,
                TextWeight::Semibold,
            );
            y += 20.0;
            for p in &saved {
                y = self.draw_place_row(scene, t, tr, theme, content, y, p);
            }
            y += 8.0;
        }

        if !self.model.recents.is_empty() {
            draw::text_w(
                scene,
                tr,
                t,
                "RECENT SEARCHES",
                x,
                y,
                11.0,
                theme.text_dim,
                TextWeight::Semibold,
            );
            y += 20.0;
            let recents = self.model.recents.clone();
            for q in &recents {
                let rr = Rect::new(x, y, right, y + 30.0);
                draw::stroke_circle(
                    scene,
                    t,
                    Point::new(x + 11.0, rr.center().y),
                    5.0,
                    1.5,
                    theme.text_dim,
                );
                draw::text(
                    scene,
                    tr,
                    t,
                    q,
                    x + 26.0,
                    vmid(rr.y0, 30.0, 12.0),
                    12.0,
                    theme.text_primary,
                );
                self.click_targets.push((rr, Hit::Recent(q.clone())));
                y += 32.0;
            }
        }
        y
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "shares the standard (scene, t, tr, theme) drawing-context params with every \
                  other draw_* method in this file; bundling them into a context struct is a \
                  file-wide refactor out of scope here"
    )]
    #[allow(
        clippy::many_single_char_names,
        reason = "conventional short names for the shared drawing context (scene, t: transform, \
              tr: text renderer, c: theme colours, r: rect) used consistently across every \
              draw_* method in this file"
    )]
    fn draw_place_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        y: f64,
        p: &crate::model::Place,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let r = Rect::new(content.x0, y, content.x1, y + ROW_H - 8.0);
        let selected = self.model.selected.as_deref() == Some(p.id.as_str());
        if selected {
            draw::fill_rrect(scene, t, r, 8.0, draw::alpha(theme.accent_primary, 0.14));
        }
        let cat_col = canvas::category_color(theme, p.category);
        draw::fill_circle(
            scene,
            t,
            Point::new(r.x0 + 18.0, r.center().y),
            9.0,
            cat_col,
        );
        draw::fill_circle(
            scene,
            t,
            Point::new(r.x0 + 18.0, r.center().y),
            3.2,
            draw::mix(c.surfaces.base, Color::WHITE, 0.7),
        );
        let tx = r.x0 + 36.0;
        draw::text_clip(
            scene,
            tr,
            t,
            &p.name,
            tx,
            r.y0 + 6.0,
            13.0,
            theme.text_primary,
            r.x1 - tx - 50.0,
        );
        let sub = format!("{} · {}", p.category.label(), p.address);
        draw::text_clip(
            scene,
            tr,
            t,
            &sub,
            tx,
            r.y0 + 24.0,
            11.0,
            theme.text_dim,
            r.x1 - tx - 50.0,
        );
        if p.saved {
            draw::text_right(
                scene,
                tr,
                t,
                "♥",
                r.x1 - 8.0,
                r.y0 + 24.0,
                12.0,
                theme.tokens.colors.semantic.danger,
            );
        }
        self.click_targets.push((r, Hit::SelectPlace(p.id.clone())));
        y + ROW_H
    }

    /// One nearby-POI row: a category marker chip (disc + white pictogram,
    /// matching the map markers), the name, a "Category · detail" subtitle, and a
    /// trailing distance chip. The whole row selects the POI (centre + info card)
    /// and highlights while its marker is the open one.
    #[allow(clippy::too_many_arguments)]
    fn draw_poi_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        y: f64,
        poi: &crate::model::Poi,
        anchor: LatLon,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let r = Rect::new(content.x0, y, content.x1, y + ROW_H - 8.0);
        let selected = self.selected_poi.as_deref() == Some(poi.id.as_str());
        if selected {
            draw::fill_rrect(scene, t, r, 8.0, draw::alpha(theme.accent_primary, 0.14));
        }
        let cat_col = canvas::category_color(theme, poi.category);
        let ring = draw::mix(c.surfaces.base, Color::WHITE, 0.85);
        let cc = Point::new(r.x0 + 18.0, r.center().y);
        draw::fill_circle(scene, t, cc, 11.0, cat_col);
        canvas::draw_category_icon(scene, t, cc, 11.0, poi.category, ring, cat_col);

        let dist = geo::fmt_distance(geo::haversine_m(poi.pos, anchor));
        let dw = draw::measure_w(tr, &dist, 11.0) + 10.0;
        let tx = r.x0 + 38.0;
        draw::text_clip(
            scene,
            tr,
            t,
            &poi.name,
            tx,
            r.y0 + 6.0,
            13.0,
            theme.text_primary,
            r.x1 - tx - dw,
        );
        let sub = if !poi.address.is_empty() {
            format!("{} · {}", poi.category.label(), poi.address)
        } else if let Some(k) = &poi.kind {
            format!("{} · {}", poi.category.label(), k)
        } else {
            poi.category.label().to_string()
        };
        draw::text_clip(
            scene,
            tr,
            t,
            &sub,
            tx,
            r.y0 + 24.0,
            11.0,
            theme.text_dim,
            r.x1 - tx - dw,
        );
        draw::text_right(
            scene,
            tr,
            t,
            &dist,
            r.x1 - 8.0,
            r.y0 + 8.0,
            11.0,
            theme.accent_primary,
        );
        self.click_targets.push((r, Hit::SelectPoi(poi.id.clone())));
        y + ROW_H
    }

    /// One typeable endpoint field: draw the rounded box + leading dot, record
    /// the click target, and return the inner rect the caller lays the endpoint
    /// [`TextInput`] into (the widget owns the text, caret, and placeholder).
    #[allow(clippy::too_many_arguments)]
    fn draw_endpoint_field(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &TextRenderer,
        theme: &Theme,
        r: Rect,
        dot: Color,
        editing: bool,
        hit: Hit,
    ) -> Rect {
        let _ = tr;
        let c = &theme.tokens.colors;
        let border = if editing {
            theme.border_focus
        } else {
            c.border_standard
        };
        draw::fill_rrect(scene, t, r, 9.0, c.surfaces.base);
        draw::stroke_rrect(scene, t, r, 9.0, 1.0, border);
        draw::fill_circle(scene, t, Point::new(r.x0 + 16.0, r.center().y), 4.0, dot);
        self.click_targets.push((r, hit));
        let tx = r.x0 + 30.0;
        Rect::new(tx - 4.0, r.y0, r.x1 - 12.0, r.y1)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "a single linear draw pass over this panel's fixed regions; splitting it would only \
              relocate the line count, not the amount a reader must hold in mind"
    )]
    fn draw_directions(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        mut y: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let x = content.x0;
        let right = content.x1;

        // Typeable endpoint fields: From and To are symmetric real search
        // inputs. Editing one shows its geocoded dropdown; a swap flips them.
        let field_h = 40.0;
        let swap_w = 34.0;
        let fields_right = right - swap_w - 8.0;
        let from_editing = self.edit_field == Some(EndField::Origin);
        let from_text = if from_editing {
            self.model.origin_query.clone()
        } else if self.model.origin.is_some() && !self.model.origin_name.is_empty() {
            self.model.origin_name.clone()
        } else {
            String::new()
        };
        let from_r = Rect::new(x, y, fields_right, y + field_h);
        self.origin_field_rect = Some(from_r);
        let from_inner = self.draw_endpoint_field(
            scene,
            t,
            tr,
            theme,
            from_r,
            theme.accent_primary,
            from_editing,
            Hit::EditOrigin,
        );
        self.origin_input.set_text(&from_text);
        self.origin_input.set_focus(from_editing);
        self.origin_input.apply_layout_external(from_inner);
        self.origin_input
            .draw(scene, t, tr, theme, &PresenceCtx::empty());

        let to_y = y + field_h + 8.0;
        let to_r = Rect::new(x, to_y, fields_right, to_y + field_h);
        self.dest_field_rect = Some(to_r);
        let to_editing = self.edit_field == Some(EndField::Dest);
        let to_text = if to_editing {
            self.model.dest_query.clone()
        } else if !self.model.dest_name.is_empty() {
            self.model.dest_name.clone()
        } else {
            self.model
                .selected_place()
                .map(|p| p.name.clone())
                .unwrap_or_default()
        };
        let to_inner = self.draw_endpoint_field(
            scene,
            t,
            tr,
            theme,
            to_r,
            c.semantic.danger,
            to_editing,
            Hit::EditDest,
        );
        self.dest_input.set_text(&to_text);
        self.dest_input.set_focus(to_editing);
        self.dest_input.apply_layout_external(to_inner);
        self.dest_input
            .draw(scene, t, tr, theme, &PresenceCtx::empty());

        // Swap button spanning both fields on the trailing edge.
        let swap = Rect::new(
            right - swap_w,
            y + field_h / 2.0,
            right,
            to_r.y1 - field_h / 2.0,
        );
        draw::fill_rrect(scene, t, swap, 8.0, c.surfaces.raised);
        draw::stroke_rrect(scene, t, swap, 8.0, 1.0, c.border_standard);
        draw::text_center(
            scene,
            tr,
            t,
            "⇅",
            swap.x0,
            swap.x1,
            vmid(swap.center().y - 8.0, 16.0, 16.0),
            16.0,
            theme.text_primary,
        );
        self.click_targets.push((swap, Hit::SwapEnds));
        y = to_r.y1 + 10.0;

        // "Use my location" — a real device-location fix for the origin. A
        // compact, content-sized chip (not a full-width bar): it is a secondary
        // helper, so it stays quiet and lets the fields + mode read as the
        // primary controls.
        let loc_h = 30.0;
        let loc_label = if self.locating {
            "Locating…"
        } else {
            "Use my location"
        };
        let loc_w = draw::measure_w(tr, loc_label, 13.0) + 44.0;
        let loc_r = Rect::new(x, y, x + loc_w, y + loc_h);
        let (loc_fill, loc_fg) = if self.locating {
            (
                draw::alpha(theme.accent_primary, 0.16),
                theme.accent_primary,
            )
        } else {
            (c.surfaces.base, theme.accent_primary)
        };
        draw::fill_rrect(scene, t, loc_r, 8.0, loc_fill);
        draw::stroke_rrect(scene, t, loc_r, 8.0, 1.0, c.border_standard);
        let gx = loc_r.x0 + 18.0;
        let gy = loc_r.center().y;
        draw::stroke_circle(scene, t, Point::new(gx, gy), 5.0, 1.6, loc_fg);
        draw::fill_circle(scene, t, Point::new(gx, gy), 1.6, loc_fg);
        for (dx, dy) in [(0.0, -8.0), (0.0, 8.0), (-8.0, 0.0), (8.0, 0.0)] {
            draw::line(
                scene,
                t,
                Point::new(gx + dx * 0.7, gy + dy * 0.7),
                Point::new(gx + dx, gy + dy),
                1.4,
                loc_fg,
            );
        }
        draw::text(
            scene,
            tr,
            t,
            loc_label,
            gx + 16.0,
            vmid(loc_r.y0, loc_h, 13.0),
            13.0,
            loc_fg,
        );
        self.click_targets.push((
            loc_r,
            if self.locating {
                Hit::Consume
            } else {
                Hit::UseMyLocation
            },
        ));
        y += loc_h + 14.0;

        // NOTE: the geocoded results for the focused endpoint are NOT drawn
        // inline here — that reflowed the mode control and route below every
        // keystroke. They render in `draw_endpoint_overlay` as a floating
        // dropdown anchored under the field, over the map, so the panel below
        // stays put whether or not the dropdown is showing.

        // Mode segmented control — Drive / Transit / Cycle / Walk. Transit is
        // a mode here, not a separate tab: selecting it swaps the route summary
        // below for the public-transport departures.
        let mode_rect = Rect::new(x, y, right, y + 40.0);
        draw::fill_rrect(scene, t, mode_rect, 9.0, c.surfaces.base);
        let seg_w = mode_rect.width() / TravelMode::ALL.len() as f64;
        for (i, m) in TravelMode::ALL.iter().enumerate() {
            let sx = mode_rect.x0 + i as f64 * seg_w;
            let seg = Rect::new(sx, mode_rect.y0, sx + seg_w, mode_rect.y1);
            let active = self.model.mode == *m;
            if active {
                draw::fill_rrect(scene, t, seg.inflate(-3.0, -3.0), 7.0, theme.accent_primary);
            }
            let fg = if active { Color::WHITE } else { theme.text_dim };
            draw::text_center(
                scene,
                tr,
                t,
                m.label(),
                seg.x0,
                seg.x1,
                vmid(seg.y0, seg.height(), 12.0),
                12.0,
                fg,
            );
            self.click_targets.push((seg, Hit::Mode(*m)));
        }
        y += 52.0;

        // Depart / arrive time control — offered for EVERY travel mode. It is
        // most meaningful for Transit (it anchors the planner query) but the
        // road modes get it too, where it sets the planned departure and the
        // computed arrival clock below.
        y = self.draw_time_picker(scene, t, tr, theme, content, y);

        // Transit mode plans a public-transport journey (departures) instead of
        // a road route.
        if self.model.mode.is_transit() {
            return self.draw_departures(scene, t, tr, theme, content, y);
        }

        // Route summary + steps for the road modes.
        if let Some(route) = self.model.route.clone() {
            let big = geo::fmt_duration(route.duration_s);
            draw::text_w(
                scene,
                tr,
                t,
                &big,
                x,
                y,
                26.0,
                theme.accent_primary,
                TextWeight::Bold,
            );
            let dist = geo::fmt_distance(route.distance_m);
            let bw = draw::measure_w(tr, &big, 26.0);
            draw::text(
                scene,
                tr,
                t,
                &format!("· {dist}"),
                x + bw + 12.0,
                y + 8.0,
                14.0,
                theme.text_dim,
            );
            // Clear route link.
            let clr = Rect::new(right - 64.0, y + 2.0, right, y + 28.0);
            draw::text_right(
                scene,
                tr,
                t,
                "Clear",
                right - 4.0,
                y + 8.0,
                12.0,
                theme.tokens.colors.semantic.danger,
            );
            self.click_targets.push((clr, Hit::ClearRoute));
            y += 40.0;

            // Planned departure → arrival clock, derived from the time picker.
            // Leaving now, a single ETA reads cleaner than "Leave x → Arrive y";
            // the full pair only shows once a specific departure/arrival is set.
            let (dep_label, arr_label) = self.road_trip_clock(route.duration_s);
            if self.model.transit_time.is_now() {
                draw::text_w(
                    scene,
                    tr,
                    t,
                    &arr_label,
                    x,
                    y,
                    13.0,
                    theme.text_dim,
                    TextWeight::Medium,
                );
            } else {
                draw::text_w(
                    scene,
                    tr,
                    t,
                    &dep_label,
                    x,
                    y,
                    13.0,
                    theme.text_primary,
                    TextWeight::Medium,
                );
                let dep_w = draw::measure_w(tr, &dep_label, 13.0);
                draw::text(scene, tr, t, "→", x + dep_w + 10.0, y, 13.0, theme.text_dim);
                draw::text_w(
                    scene,
                    tr,
                    t,
                    &arr_label,
                    x + dep_w + 30.0,
                    y,
                    13.0,
                    theme.text_primary,
                    TextWeight::Medium,
                );
            }
            y += 28.0;

            draw::line(
                scene,
                t,
                Point::new(x, y),
                Point::new(right, y),
                1.0,
                c.border_standard,
            );
            y += 14.0;

            for (i, step) in route.steps.iter().enumerate() {
                let rh = 46.0;
                // Glyph badge.
                let bc = Point::new(x + 16.0, y + rh / 2.0 - 4.0);
                draw::fill_circle(scene, t, bc, 13.0, draw::alpha(theme.accent_primary, 0.16));
                draw::text_center(
                    scene,
                    tr,
                    t,
                    step.maneuver.glyph(),
                    bc.x - 13.0,
                    bc.x + 13.0,
                    bc.y - 9.0,
                    15.0,
                    theme.accent_primary,
                );
                draw::text_clip(
                    scene,
                    tr,
                    t,
                    &step.instruction,
                    x + 38.0,
                    y,
                    13.0,
                    theme.text_primary,
                    right - x - 80.0,
                );
                if step.distance_m >= 1.0 {
                    draw::text_right(
                        scene,
                        tr,
                        t,
                        &geo::fmt_distance(step.distance_m),
                        right - 4.0,
                        y + 18.0,
                        11.0,
                        theme.text_dim,
                    );
                }
                // Connector line between steps.
                if i + 1 < route.steps.len() {
                    draw::line(
                        scene,
                        t,
                        Point::new(bc.x, bc.y + 13.0),
                        Point::new(bc.x, y + rh + 9.0),
                        1.5,
                        draw::alpha(theme.accent_primary, 0.4),
                    );
                }
                y += rh;
            }
        } else {
            draw::text(
                scene,
                tr,
                t,
                "Select a place, then tap Directions.",
                x,
                y,
                13.0,
                theme.text_dim,
            );
            y += 24.0;
        }
        y
    }

    /// Compute the planned departure and arrival clock labels for a road trip
    /// of `duration_s`, honouring the leave/arrive time picker. "Leave" anchors
    /// the departure (arrival = departure + duration); "Arrive" anchors the
    /// arrival (departure = arrival − duration).
    fn road_trip_clock(&self, duration_s: f64) -> (String, String) {
        let tt = self.model.transit_time;
        let dur = chrono::Duration::seconds(duration_s.max(0.0) as i64);
        let anchor = self.clock_anchor() + chrono::Duration::minutes(i64::from(tt.offset_min));
        let (dep, arr) = if tt.arrive_by() {
            (anchor - dur, anchor)
        } else {
            (anchor, anchor + dur)
        };
        (
            format!("Leave {}", dep.format("%H:%M")),
            format!("Arrive {}", arr.format("%H:%M")),
        )
    }

    /// Depart/arrive time picker, offered for every travel mode. Defaults to
    /// "leave now"; a segmented Leave/Arrive toggle plus a ±15-minute stepper
    /// (and a "Now" reset) let the user anchor the trip to a specific time. For
    /// Transit it anchors the planner query; for the road modes it drives the
    /// computed departure/arrival clock.
    #[allow(
        clippy::too_many_lines,
        reason = "a single linear draw pass over this panel's fixed regions; splitting it would only \
              relocate the line count, not the amount a reader must hold in mind"
    )]
    fn draw_time_picker(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        mut y: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let x = content.x0;
        let right = content.x1;
        let tt = self.model.transit_time;

        // One calm row: a Leave/Arrive toggle on the left, a [– clock +] stepper
        // on the right. The old picker stacked two full-width segmented controls
        // plus a separate reset chip and a hairline — a lot of chrome for a
        // control that stays at "Leave now" most of the time.
        let row_h = 36.0;
        let toggle_w = 150.0_f64.min((right - x - 8.0) * 0.5);

        // Leave / Arrive segmented toggle.
        let seg_rect = Rect::new(x, y, x + toggle_w, y + row_h);
        draw::fill_rrect(scene, t, seg_rect, 9.0, c.surfaces.base);
        let modes = [
            (TransitTimeMode::Depart, "Leave"),
            (TransitTimeMode::Arrive, "Arrive"),
        ];
        let seg_w = seg_rect.width() / 2.0;
        for (i, (m, label)) in modes.iter().enumerate() {
            let sx = seg_rect.x0 + i as f64 * seg_w;
            let seg = Rect::new(sx, seg_rect.y0, sx + seg_w, seg_rect.y1);
            let active = tt.mode == *m;
            if active {
                draw::fill_rrect(scene, t, seg.inflate(-3.0, -3.0), 7.0, theme.accent_primary);
            }
            let fg = if active { Color::WHITE } else { theme.text_dim };
            draw::text_center(
                scene,
                tr,
                t,
                label,
                seg.x0,
                seg.x1,
                vmid(seg.y0, seg.height(), 12.0),
                12.0,
                fg,
            );
            self.click_targets.push((seg, Hit::TransitTimeMode(*m)));
        }

        // Stepper: [–] <clock> [+], sharing the row with the toggle.
        let btn_w = 38.0;
        let minus = Rect::new(seg_rect.x1 + 8.0, y, seg_rect.x1 + 8.0 + btn_w, y + row_h);
        let plus = Rect::new(right - btn_w, y, right, y + row_h);
        draw::fill_rrect(scene, t, minus, 8.0, c.surfaces.base);
        draw::fill_rrect(scene, t, plus, 8.0, c.surfaces.base);
        draw::text_center(
            scene,
            tr,
            t,
            "–",
            minus.x0,
            minus.x1,
            vmid(minus.y0, row_h, 18.0),
            18.0,
            theme.text_primary,
        );
        draw::text_center(
            scene,
            tr,
            t,
            "+",
            plus.x0,
            plus.x1,
            vmid(plus.y0, row_h, 18.0),
            18.0,
            theme.text_primary,
        );
        let target = self.clock_anchor() + chrono::Duration::minutes(i64::from(tt.offset_min));
        let label = if tt.is_now() {
            "Now".to_string()
        } else {
            target.format("%H:%M").to_string()
        };
        draw::text_center(
            scene,
            tr,
            t,
            &label,
            minus.x1,
            plus.x0,
            vmid(y, row_h, 15.0),
            15.0,
            theme.accent_primary,
        );
        self.click_targets.push((minus, Hit::TransitTimeDelta(-15)));
        self.click_targets.push((plus, Hit::TransitTimeDelta(15)));
        y += row_h;

        // A compact "Reset to now" appears only when the anchor is off default —
        // the common leave-now case stays a single, quiet row.
        if !tt.is_now() {
            y += 8.0;
            let reset_w = draw::measure_w(tr, "Reset to now", 12.0) + 20.0;
            let reset = Rect::new(x, y, x + reset_w, y + 26.0);
            draw::fill_rrect(scene, t, reset, 8.0, c.surfaces.raised);
            draw::stroke_rrect(scene, t, reset, 8.0, 1.0, c.border_standard);
            draw::text_center(
                scene,
                tr,
                t,
                "Reset to now",
                reset.x0,
                reset.x1,
                vmid(reset.y0, 26.0, 12.0),
                12.0,
                theme.text_primary,
            );
            self.click_targets.push((reset, Hit::TransitTimeNow));
            y += 26.0;
        }

        // Breathing space rather than a hairline — groups separate by space, not
        // boxes-within-boxes (HIG §7).
        y + 18.0
    }

    /// The public-transport departures list for Transit mode. The depart/arrive
    /// time picker is drawn by [`draw_directions`] just above this; here we
    /// render the destination header and the itinerary cards.
    #[allow(
        clippy::too_many_lines,
        reason = "a single linear draw pass over this panel's fixed regions; splitting it would only \
              relocate the line count, not the amount a reader must hold in mind"
    )]
    #[allow(
        clippy::many_single_char_names,
        reason = "conventional short names for the shared drawing context (scene, t: transform, \
              tr: text renderer, c: theme colours, r: rect) used consistently across every \
              draw_* method in this file"
    )]
    fn draw_departures(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        mut y: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let x = content.x0;
        let right = content.x1;
        let to = if self.model.dest_name.is_empty() {
            self.model
                .selected_place()
                .map_or_else(|| "destination".to_string(), |p| p.name.clone())
        } else {
            self.model.dest_name.clone()
        };
        draw::text_w(
            scene,
            tr,
            t,
            &format!("Departures to {to}"),
            x,
            y,
            14.0,
            theme.text_primary,
            TextWeight::Semibold,
        );
        y += 26.0;

        if self.model.transit.is_empty() {
            let has_target = self.model.dest.is_some() || self.model.selected_place().is_some();
            let msg = if self.pending > 0 {
                "Finding departures…"
            } else if !has_target {
                "Choose a destination to see departures."
            } else {
                "No public-transport itineraries found."
            };
            draw::text(scene, tr, t, msg, x, y, 13.0, theme.text_dim);
            return y + 24.0;
        }

        let journeys = self.model.transit.clone();
        for (i, j) in journeys.iter().enumerate() {
            let selected = self.model.transit_choice == i;
            let base_h = 86.0;
            let jr = Rect::new(x, y, right, y + base_h);
            draw::fill_rrect(
                scene,
                t,
                jr,
                10.0,
                if selected {
                    draw::alpha(theme.accent_primary, 0.12)
                } else {
                    c.surfaces.base
                },
            );
            if selected {
                draw::stroke_rrect(scene, t, jr, 10.0, 1.5, theme.accent_primary);
            }
            // Header: dep–arr  ·  duration  ·  transfers.
            draw::text_w(
                scene,
                tr,
                t,
                &format!("{} – {}", j.dep, j.arr),
                x + 12.0,
                y + 10.0,
                15.0,
                theme.text_primary,
                TextWeight::Bold,
            );
            draw::text_right(
                scene,
                tr,
                t,
                &format!("{} min", j.duration_min),
                right - 12.0,
                y + 10.0,
                14.0,
                theme.accent_primary,
            );
            let xfer = if j.transfers == 0 {
                "direct".to_string()
            } else {
                format!(
                    "{} change{}",
                    j.transfers,
                    if j.transfers == 1 { "" } else { "s" }
                )
            };
            draw::text_right(
                scene,
                tr,
                t,
                &xfer,
                right - 12.0,
                y + 30.0,
                11.0,
                theme.text_dim,
            );

            // Leg pills — coloured in each line's authentic feed colour.
            let mut px = x + 12.0;
            let py = y + 38.0;
            for (li, leg) in j.legs.iter().enumerate() {
                if li > 0 {
                    draw::text(scene, tr, t, "›", px, py + 2.0, 13.0, theme.text_dim);
                    px += 14.0;
                }
                if leg.mode == TransitMode::Walk {
                    // Walk shown as a dashed segment with minutes.
                    let lbl = format!("walk {}m", leg.duration_min.max(1));
                    let w = draw::measure_w(tr, &lbl, 11.0) + 6.0;
                    draw::text(scene, tr, t, &lbl, px, py + 3.0, 11.0, theme.text_dim);
                    px += w + 4.0;
                } else {
                    let col = canvas::leg_color(theme, leg);
                    let fg = leg
                        .text_color
                        .as_deref()
                        .and_then(draw::hex_color)
                        .unwrap_or(Color::WHITE);
                    let lbl = if leg.line.is_empty() {
                        leg.mode.label().to_string()
                    } else {
                        leg.line.clone()
                    };
                    let w = draw::measure_w(tr, &lbl, 12.0) + 16.0;
                    let pill = Rect::new(px, py, px + w, py + 22.0);
                    draw::fill_rrect(scene, t, pill, 6.0, col);
                    draw::text_center(
                        scene,
                        tr,
                        t,
                        &lbl,
                        pill.x0,
                        pill.x1,
                        vmid(pill.y0, 22.0, 12.0),
                        12.0,
                        fg,
                    );
                    px += w + 6.0;
                }
                if px > right - 40.0 {
                    break;
                }
            }
            self.click_targets.push((jr, Hit::Journey(i)));
            y += base_h + 10.0;

            // Expanded leg detail for the selected journey.
            if selected {
                for leg in &j.legs {
                    let lh = 36.0;
                    let col = canvas::leg_color(theme, leg);
                    draw::line(
                        scene,
                        t,
                        Point::new(x + 18.0, y + 4.0),
                        Point::new(x + 18.0, y + lh),
                        3.0,
                        col,
                    );
                    draw::fill_circle(scene, t, Point::new(x + 18.0, y + 8.0), 4.0, col);
                    let head = if leg.line.is_empty() {
                        format!("Walk · {} min", leg.duration_min.max(1))
                    } else {
                        format!("{} {} {}", leg.mode.label(), leg.line, leg.headsign)
                    };
                    let head = head.trim().to_string();
                    draw::text_clip(
                        scene,
                        tr,
                        t,
                        &head,
                        x + 32.0,
                        y,
                        12.0,
                        theme.text_primary,
                        right - x - 120.0,
                    );
                    // "live" badge for a real-time leg — honest freshness signal.
                    if leg.realtime {
                        let bx = x + 32.0 + draw::measure_w(tr, &head, 12.0) + 8.0;
                        draw::fill_circle(
                            scene,
                            t,
                            Point::new(bx + 3.0, y + 7.0),
                            3.0,
                            c.semantic.success,
                        );
                        draw::text(
                            scene,
                            tr,
                            t,
                            "live",
                            bx + 9.0,
                            y + 1.0,
                            10.0,
                            c.semantic.success,
                        );
                    }
                    let sub = if leg.line.is_empty() {
                        format!("to {}", leg.to_name)
                    } else {
                        // Board stop → alight stop, so "where to get off" is explicit.
                        format!(
                            "{} → alight {} · {} stops",
                            leg.from_name, leg.to_name, leg.num_stops
                        )
                    };
                    draw::text_clip(
                        scene,
                        tr,
                        t,
                        &sub,
                        x + 32.0,
                        y + 16.0,
                        11.0,
                        theme.text_dim,
                        right - x - 90.0,
                    );
                    draw::text_right(
                        scene,
                        tr,
                        t,
                        &format!("{}–{}", leg.dep, leg.arr),
                        right - 8.0,
                        y + 4.0,
                        11.0,
                        theme.text_dim,
                    );
                    y += lh + 2.0;
                }
                y += 6.0;
            }
        }

        // Data-source + freshness footer. Transitous runs MOTIS over the
        // aggregated operator GTFS(-RT) feeds; per-leg "live" badges above
        // reflect real-time data. Munich (MVV/MVG + DB) publishes GTFS-RT, so
        // departures are live when the leg is flagged; where a feed is
        // schedule-only this reads "Scheduled" — honest about freshness rather
        // than implying live data the feed didn't provide.
        let agency = journeys
            .iter()
            .flat_map(|j| j.legs.iter())
            .find(|l| !l.agency.is_empty())
            .map(|l| l.agency.clone());
        let any_live = journeys
            .iter()
            .flat_map(|j| j.legs.iter())
            .any(|l| l.realtime);
        let freshness = if any_live {
            "Live times"
        } else {
            "Scheduled times"
        };
        let src = match agency {
            Some(a) => format!("{freshness} · via Transitous (MOTIS) · {a}"),
            None => format!("{freshness} · via Transitous (MOTIS)"),
        };
        draw::line(
            scene,
            t,
            Point::new(x, y),
            Point::new(right, y),
            1.0,
            c.border_standard,
        );
        y += 10.0;
        draw::text_clip(scene, tr, t, &src, x, y, 10.0, theme.text_dim, right - x);
        y += 18.0;
        y
    }
}

// ===========================================================================
// Widget impl
// ===========================================================================
impl Widget for MapsApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        // Pull any background network results into the model first.
        self.drain_net();
        // Fold in a device-location fix if the portal worker has one.
        self.drain_location();
        // Fire a debounced search-as-you-type geocode once typing settles.
        self.tick_search_debounce(Instant::now());
        // Same for the Directions From/To fields (start/end search-as-you-type).
        self.tick_endpoint_debounce(Instant::now());
        // Refetch viewport POIs once a pan/zoom settles.
        self.tick_poi_debounce(Instant::now());
        // Fire the offline panel's live city geocode once its field settles.
        if let Some(query) = self.offline_panel.take_due_query(Instant::now()) {
            self.pending += 1;
            if self.online {
                let tx = self.net_tx.clone();
                std::thread::spawn(move || {
                    let results = net::geocode_regions(&query);
                    let _ = tx.send(NetMsg::RegionSearch { query, results });
                });
            }
        }
        // Fold in any tiles / download progress the worker finished. This is a
        // non-blocking try_recv drain — it never waits on the network.
        let before = self.tiles.progress();
        self.tiles.drain();
        let after = self.tiles.progress();
        if before != after && !after.is_active() && after.total > 0 {
            // A download just finished — refresh the on-disk size readout.
            self.offline_panel.set_cache_bytes(self.tiles.cache_bytes());
            if self.model.status.contains("Downloading") {
                self.model.status = format!("Offline area ready ({} tiles)", after.total);
            }
        }

        if !self.loaded {
            if let Some(loaded) = MapsDoc::load(state) {
                // Adopt an already-seeded (possibly peer-provided) document,
                // but keep our generated basemap.
                self.model = loaded;
            } else {
                MapsDoc::save(state, &self.model);
            }
            self.loaded = true;
            self.dirty = false;
            self.persist_deadline = None;
        } else if self.dirty && self.persist_deadline.is_none_or(|d| Instant::now() >= d) {
            // The view has settled (or a non-gesture change had no deadline):
            // flush the whole model once. Coalescing lives in `mark_dirty`.
            MapsDoc::save(state, &self.model);
            self.dirty = false;
            self.persist_deadline = None;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        #[allow(
            clippy::unwrap_used,
            reason = "a fresh leaf on a live taffy tree cannot fail short of OOM, and \
                      `Widget::build_layout` returns `NodeId` directly with no fallible path"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        // Arm the first POI fetch once a real viewport exists (live app only).
        if self.online
            && self.poi_fetched_bounds.is_none()
            && self.poi_debounce.is_none()
            && self.rect.width() > 1.0
            && self.model.zoom >= POI_MIN_ZOOM
        {
            self.poi_debounce = Some(Instant::now() + POI_DEBOUNCE);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.draw_app(scene, transform, text_renderer, theme);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the Widget::handle_event trait method fans out on WindowEvent kind; each \
                  branch is a short, independent dispatch to an already-extracted helper"
    )]
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // A click, scroll or keypress dismisses any shown control tooltip.
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } | WindowEvent::MouseWheel { .. }
                | WindowEvent::KeyboardInput { .. }
        ) {
            self.map_controls.dismiss_tooltip();
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse = p;
                // Keep the field inputs' hover state current so a click lands a
                // caret and a press-drag extends a selection.
                self.forward_move_to_field_inputs(event, scale_factor, *modifiers);
                // Track hover in the open overflow menu / offline city dropdown.
                if self.overflow.is_open() {
                    self.drive_overflow(event, scale_factor, *modifiers);
                }
                if self.offline_panel.is_open() {
                    self.offline_panel.route_field_event(
                        event,
                        scale_factor,
                        *modifiers,
                        self.last_mouse,
                    );
                }
                if self.dragging && self.left_down {
                    let dx = p.x - self.drag_last.x;
                    let dy = p.y - self.drag_last.y;
                    if dx.abs() > 0.5 || dy.abs() > 0.5 {
                        self.drag_moved = true;
                        self.pan_by(dx, dy);
                        // Mark the move handled so the `ui` App requests a repaint
                        // for this pointer event. Its per-frame redraw is gated on
                        // `CursorMoved` events being `handled` (the hover-spam
                        // guard, see ui::app), so without this the map centre
                        // updates but the screen never repaints — drag-to-pan
                        // reads as frozen. This mirrors the wheel-zoom path, which
                        // already marks its events handled for the same reason.
                        *handled = true;
                    }
                    self.drag_last = p;
                }
                // Retarget the control hover tooltip. Suppressed while dragging
                // (the pointer is panning, not resting on a control). A state
                // change marks the move handled so the `ui` App repaints it,
                // clearing or updating the chip promptly.
                let extra_targets = self
                    .menu_rect()
                    .map(|menu| (menu, "Menu"))
                    .into_iter()
                    .chain(std::iter::once((self.overflow_rect(), "More")));
                if self
                    .map_controls
                    .on_pointer(p, self.left_down, extra_targets, Instant::now())
                {
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_mouse;
                self.left_down = true;
                self.drag_moved = false;
                // The open ⋯ overflow menu owns the pointer: a row press selects,
                // an outside press dismisses. Route to it before the map/chrome.
                if self.overflow.is_open() {
                    self.drive_overflow(event, scale_factor, *modifiers);
                    if let Some(idx) = self.overflow.take_selection() {
                        self.overflow_select(idx);
                    }
                    *handled = true;
                    return;
                }
                // The offline dialog's city field / dropdown takes its own clicks
                // first; a click elsewhere in the dialog falls through to the
                // buttons below (download / clear / delete / close / scrim).
                if self.offline_panel.is_open() {
                    if self.offline_panel.route_field_event(
                        event,
                        scale_factor,
                        *modifiers,
                        self.last_mouse,
                    ) {
                        *handled = true;
                        return;
                    }
                    if let Some(action) = self.offline_panel.action_at(p) {
                        self.dispatch_offline(action, state);
                        *handled = true;
                        return;
                    }
                }
                // Hit-test recorded UI targets (topmost = last drawn).
                let mut consumed = false;
                if let Some(hit) = self
                    .click_targets
                    .iter()
                    .rev()
                    .find(|(r, _)| r.contains(p))
                    .map(|(_, h)| h.clone())
                {
                    // Clicking outside the search bar unfocuses it.
                    if !matches!(hit, Hit::Search(SearchHit::Field)) {
                        self.search.set_focused(false);
                    }
                    // Clicking anything other than an endpoint field (or its
                    // dropdown) ends endpoint editing.
                    if !matches!(
                        hit,
                        Hit::EditOrigin | Hit::EditDest | Hit::PickOrigin(_) | Hit::PickDest(_)
                    ) {
                        self.edit_field = None;
                    }
                    self.dispatch(&hit, state);
                    // A click that focused a text field forwards the press so the
                    // shared TextInput places its caret at the click point (the
                    // preceding CursorMoved already set its hover flag).
                    if matches!(
                        hit,
                        Hit::Search(SearchHit::Field) | Hit::EditOrigin | Hit::EditDest
                    ) {
                        self.forward_press_to_field(&hit, event, scale_factor, *modifiers);
                    }
                    consumed = true;
                    *handled = true;
                }
                if !consumed {
                    self.search.set_focused(false);
                    self.edit_field = None;
                    // The standalone control stack is below modal/panel targets
                    // above, but above the actual map surface.
                    if let Some(action) = self.map_controls.hit_test(p) {
                        self.dispatch_map_control(action);
                        *handled = true;
                    } else if let Some(id) = self.marker_at(p) {
                        // Map interaction: marker select, POI select, or begin pan.
                        self.select_place(&id);
                        *handled = true;
                    } else if let Some(pid) = self.poi_at(p) {
                        self.select_poi(&pid);
                        *handled = true;
                    } else {
                        // Empty map: close any open POI card and begin a pan.
                        // Claim the press so the compositor/ui App treats the map
                        // as the interaction target for the gesture that follows.
                        if self.selected_poi.take().is_some() {
                            self.mark_dirty();
                        }
                        self.dragging = true;
                        self.drag_last = p;
                        *handled = true;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.left_down = false;
                self.dragging = false;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    MouseScrollDelta::PixelDelta(pos) => pos.y / 40.0,
                };
                let over_panel = self.panel_visible()
                    && self.panel_rect().contains(self.last_mouse)
                    && self.last_mouse.y > self.panel_rect().y0 + 50.0;
                if over_panel {
                    let max = (self.content_h - self.content_view_h).max(0.0);
                    self.scroll = (self.scroll - dy * 36.0).clamp(0.0, max);
                } else {
                    self.zoom_by(dy * 0.35, self.last_mouse);
                }
                *handled = true;
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                if key_event.state != ElementState::Pressed {
                    return;
                }
                // The open ⋯ overflow menu takes keyboard nav (arrows / Enter /
                // Esc) via its own handling before the map shortcuts.
                if self.overflow.is_open() {
                    self.drive_overflow(event, scale_factor, *modifiers);
                    if let Some(idx) = self.overflow.take_selection() {
                        self.overflow_select(idx);
                    }
                    *handled = true;
                    return;
                }
                // While the offline dialog is open, Escape closes it and every
                // other key types into its city search field.
                if self.offline_panel.is_open() {
                    if matches!(key_event.logical_key, Key::Named(NamedKey::Escape)) {
                        self.offline_panel.close();
                    } else {
                        self.offline_panel.route_field_event(
                            event,
                            scale_factor,
                            *modifiers,
                            self.last_mouse,
                        );
                    }
                    *handled = true;
                    return;
                }
                // A focused text field (search or an endpoint) owns the
                // keystroke via the shared TextInput; otherwise the map-level
                // shortcuts apply.
                let field_focused = self.edit_field.is_some() || self.search.is_focused();
                if field_focused {
                    if self.route_field_key(event, scale_factor, *modifiers) {
                        *handled = true;
                    }
                } else if self.handle_key(&key_event.logical_key, *modifiers, state) {
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "MapsApp"
    }

    /// Publish the resource tree the shell's one Ctrl+K palette walks — the saved
    /// places, the active route, and the offline regions currently in the shared
    /// document. Delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Drain the undo boundary the [`ActionHost`](ui::ActionHost) bracketed for
    /// this event so the shell records exactly one undo point per dispatched
    /// action — the same path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    fn test_id(&self) -> Option<&str> {
        Some("maps_app")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_idle_sync(&self) -> bool {
        // Keep idling only while real work is outstanding: net requests, the
        // initial load, tiles genuinely in flight, or an active offline
        // download. This lets the app fold in worker results and repaint as
        // imagery arrives without the user touching the input device. A tile
        // whose fetch failed is *not* counted — it is negative-cached with a
        // backoff (see `TileLayer::request`), so `has_inflight()` drops to
        // false and the loop settles instead of re-requesting every frame.
        self.pending > 0
            || !self.loaded
            // A coalesced view-persist is still pending — keep idling until it
            // flushes, so a settled pan/zoom always reaches the CRDT even if
            // nothing else is in flight.
            || self.dirty
            || self.locating
            || self.location_rx.is_some()
            || self.poi_debounce.is_some()
            || self.tiles.has_inflight()
            || self.tiles.progress().is_active()
    }

    fn needs_animation_frame(&self) -> bool {
        // Same rule as `needs_idle_sync`: only ask for another frame while
        // something is legitimately in flight (net, tiles, download) or the
        // search caret is blinking. Failed tiles back off and clear
        // `has_inflight()`, so a viewport of unfetchable tiles no longer pins
        // the loop at 60 Hz.
        self.pending > 0
            || self.search.is_focused()
            || self.search.has_pending_query()
            || self.endpoint_debounce.is_some()
            || self.poi_debounce.is_some()
            || self.edit_field.is_some()
            || self.locating
            || self.location_rx.is_some()
            || self.tiles.has_inflight()
            || self.tiles.progress().is_active()
            // Keep pumping frames while a control tooltip counts toward its
            // reveal, so the dwell elapses even when the map is otherwise idle.
            || self.map_controls.needs_frame()
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        // Root fills the window — never cull.
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

impl MapsApp {
    fn endpoint_input_mut(&mut self, origin: bool) -> &mut TextInput {
        if origin {
            &mut self.origin_input
        } else {
            &mut self.dest_input
        }
    }

    /// Route a keystroke into the focused text field (search or an endpoint).
    /// Enter runs the corresponding search; any text edit is mirrored into the
    /// model (`query` / `*_query`) so the existing geocode / dropdown logic runs
    /// unchanged. Returns whether the key was consumed.
    fn route_field_key(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let is_escape = matches!(
            event,
            WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape),
                    ..
                },
                ..
            }
        );
        if let Some(field) = self.edit_field {
            let origin = field == EndField::Origin;
            if is_escape {
                self.edit_field = None;
                self.mark_dirty();
                return true;
            }
            let (submitted, dirty) = {
                let input = self.endpoint_input_mut(origin);
                // The widget only acts on keys while focused; the app's
                // `edit_field` is the source of truth, so assert focus here (a
                // draw may not have run since the field was focused).
                input.set_focus(true);
                drive_text_input(input, event, scale, modifiers);
                let submitted = input.take_submit();
                (submitted, if submitted { None } else { input.take_dirty() })
            };
            if submitted {
                self.run_endpoint_search(origin);
            } else if let Some(text) = dirty {
                *self.endpoint_query_mut(origin) = text;
                self.on_endpoint_input(origin);
            }
            return true;
        }
        if self.search.is_focused() {
            match self.search.handle_key(event, scale, modifiers) {
                SearchKeyEffect::None => {}
                SearchKeyEffect::Changed(text) => {
                    self.model.query = text;
                    self.on_search_input();
                }
                SearchKeyEffect::Submit => self.run_search(),
                SearchKeyEffect::Dismissed => self.mark_dirty(),
            }
            return true;
        }
        false
    }

    /// Drive one raw event into the ⋯ overflow [`ContextMenu`] (hand-laid, so a
    /// throwaway leaf + doc satisfy `Widget::handle_event`; the menu hit-tests
    /// against its own `open_at` rect). Poll `take_selection` afterwards.
    fn drive_overflow(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
    ) {
        let mut doc = AutoCommit::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let Ok(node) = taffy.new_leaf(Style::default()) else {
            return;
        };
        let mut handled = false;
        let mut focus_target: Option<NodeId> = None;
        self.overflow.handle_event(
            event,
            scale,
            &modifiers,
            &mut doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
    }

    /// Map an overflow-menu row selection to app state: attribution / licenses
    /// sheets, or the offline-maps dialog. Indices match the push order in
    /// [`build`].
    fn overflow_select(&mut self, idx: usize) {
        match idx {
            0 => self.map_info = Some(MapInfo::Attribution),
            1 => self.offline_panel.open(self.tiles.cache_bytes()),
            2 => self.map_info = Some(MapInfo::Licenses),
            _ => {}
        }
        self.mark_dirty();
    }

    /// Forward a `CursorMoved` to all field inputs so their hover state tracks
    /// (a click then lands a caret; a press-drag extends a selection).
    fn forward_move_to_field_inputs(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
    ) {
        self.search.forward_pointer(event, scale, modifiers);
        drive_text_input(&mut self.origin_input, event, scale, modifiers);
        drive_text_input(&mut self.dest_input, event, scale, modifiers);
    }

    /// Forward a press to the field input `origin`/dest/search just focused so
    /// it places its caret at the click point.
    fn forward_press_to_field(
        &mut self,
        which: &Hit,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
    ) {
        if matches!(which, Hit::Search(SearchHit::Field)) {
            self.search.forward_press(event, scale, modifiers);
            return;
        }
        let input = match which {
            Hit::EditOrigin => &mut self.origin_input,
            Hit::EditDest => &mut self.dest_input,
            _ => return,
        };
        input.set_focus(true);
        drive_text_input(input, event, scale, modifiers);
    }

    fn handle_key(
        &mut self,
        key: &Key,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
    ) -> bool {
        // A Directions endpoint field (From / To) has keyboard focus: typed
        // characters geocode that endpoint, mirroring the top search field.
        // Focused text fields (search / endpoints) are handled by
        // `route_field_key`; `handle_key` runs only when no field has keyboard
        // focus, so it dispatches the map-level shortcuts.
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
        match key {
            Key::Character(s) if s.as_str() == "/" => {
                self.search.set_focused(true);
                true
            }
            Key::Character(s) if s.as_str() == "+" || s.as_str() == "=" => {
                self.zoom_by(1.0, self.view_center());
                true
            }
            Key::Character(s) if s.as_str() == "-" || s.as_str() == "_" => {
                self.zoom_by(-1.0, self.view_center());
                true
            }
            Key::Character(s) if s.eq_ignore_ascii_case("d") => {
                self.enter_directions();
                true
            }
            Key::Character(s) if s.eq_ignore_ascii_case("t") => {
                self.enter_transit();
                true
            }
            Key::Character(s) if s.eq_ignore_ascii_case("f") => {
                // Fit whatever is shown: a road route, or — in Transit mode
                // — the selected itinerary's geometry, or the endpoints.
                self.fit_current();
                true
            }
            Key::Character(s) if s.eq_ignore_ascii_case("s") && !ctrl => {
                self.toggle_save_selected(doc);
                true
            }
            Key::Character(s) if s.as_str() == "1" => {
                self.set_panel(Panel::Explore);
                true
            }
            Key::Character(s) if s.as_str() == "2" => {
                self.set_panel(Panel::Directions);
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.pan_by(-80.0, 0.0);
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.pan_by(80.0, 0.0);
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.pan_by(0.0, -80.0);
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.pan_by(0.0, 80.0);
                true
            }
            Key::Named(NamedKey::Escape) => {
                if self.overflow.is_open() {
                    self.overflow.close();
                } else if self.map_info.is_some() {
                    self.map_info = None;
                } else if self.offline_panel.is_open() {
                    self.offline_panel.close();
                } else if self.selected_poi.is_some() {
                    self.selected_poi = None;
                } else if self.overlay_mode() && self.nav_open {
                    self.nav_open = false;
                } else {
                    self.model.selected = None;
                }
                self.mark_dirty();
                true
            }
            Key::Character(s) if s.eq_ignore_ascii_case("m") => {
                // Only reachable while the dialog is closed (once open, its city
                // field owns the keyboard), so this opens it — focusing the
                // search field. Escape / ✕ / the scrim close it.
                if !self.offline_panel.is_open() {
                    self.offline_panel.open(self.tiles.cache_bytes());
                }
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The coalesced view-persist must not write every frame during a gesture,
    /// yet must reliably flush once the view settles — the correctness half of
    /// the perf fix (the perf guards above prove the cost half).
    #[test]
    fn view_persist_is_coalesced_then_flushes() {
        use crate::doc::MapsDoc;
        let mut a = app_at(1280.0, 800.0);
        let mut state = ui::State::new();
        // First sync seeds the CRDT from the model.
        a.sync(&mut state.doc, &state.root);
        assert!(!a.dirty, "loaded model is clean after the seeding sync");

        // A zoom marks the model dirty and arms the debounce in the future.
        a.zoom_by(1.0, a.view_center());
        assert!(
            a.dirty && a.persist_deadline.is_some(),
            "a view change arms a deferred flush"
        );

        // Immediately after, sync must NOT serialise+write — this is the
        // per-frame churn we removed. The doc still holds the pre-zoom view.
        a.sync(&mut state.doc, &state.root);
        assert!(
            a.dirty,
            "a fresh gesture is not persisted on the very next frame"
        );
        let mid = MapsDoc::load(&state.doc).expect("doc seeded");
        assert_ne!(
            mid.zoom, a.model.zoom,
            "the in-flight view has not yet reached the doc"
        );

        // Once the quiet period elapses, the next sync flushes exactly once.
        a.persist_deadline = Some(
            Instant::now()
                .checked_sub(std::time::Duration::from_millis(1))
                .unwrap(),
        );
        a.sync(&mut state.doc, &state.root);
        assert!(
            !a.dirty && a.persist_deadline.is_none(),
            "the settled view flushed and disarmed"
        );
        let after = MapsDoc::load(&state.doc).expect("doc seeded");
        assert!(
            (after.zoom - a.model.zoom).abs() < 1e-9,
            "the settled zoom reached the doc"
        );
    }

    /// The ⋯ overflow menu carries exactly the attribution / offline-maps /
    /// licenses options, opens from the bottom-right button, and each row routes
    /// to its surface — the permanent copyright line no longer sits over the map.
    #[test]
    fn overflow_menu_opens_and_routes_to_its_options() {
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
        // It expands upward from the bottom-right button (never off the bottom).
        assert!(
            a.overflow.get_rect().y1 <= a.overflow_rect().y0 + 1.0,
            "the menu sits above its button"
        );

        // Row 0 -> attribution sheet; row 1 -> offline dialog; row 2 -> licenses.
        a.overflow_select(0);
        assert_eq!(a.map_info, Some(MapInfo::Attribution));
        a.map_info = None;
        a.overflow_select(1);
        assert!(
            a.offline_panel.is_open(),
            "‘Offline maps’ opens the download dialog"
        );
        a.offline_panel.close();
        a.overflow_select(2);
        assert_eq!(a.map_info, Some(MapInfo::Licenses));

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
        a.run_search();
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
        a.run_search();
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

        // Dismiss the results → the panel list returns.
        a.search.set_focused(false);
        assert!(!a.search_overlay_active());
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

        // Changing the anchor for a road mode re-derives the clock but does NOT
        // refetch a transit journey (OSRM is not time-dependent).
        a.set_mode(TravelMode::Car);
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
        a.model.selected = Some("marienplatz".to_string());
        a.set_mode(TravelMode::Foot);
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
        a.run_search();
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

        a.swap_ends();
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
        // Select a place other than the sample's default so the stale sample
        // departures are dropped (destination-specific).
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
        a.focus_endpoint(EndField::Origin);
        a.model.origin_query = "hauptbahnhof".to_string();
        a.update_endpoint_local(true);
        assert!(!a.origin_results.is_empty(), "typing surfaces a From match");

        let picked = a.origin_results[0].clone();
        a.pick_endpoint(true, 0);
        assert_eq!(
            a.model.origin,
            Some(picked.pos),
            "origin coord set from the result"
        );
        assert_eq!(
            a.model.origin_name, picked.name,
            "origin label set from the result"
        );
        assert!(a.edit_field.is_none(), "picking a result ends editing");

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
        a.focus_endpoint(EndField::Dest);
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
            a.dest_results.iter().any(|r| r.id == "osm-n-1"),
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
        a.focus_endpoint(EndField::Dest);
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
            a.dest_results.first().map(|r| r.id.as_str()),
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

    // --- Location services -------------------------------------------------

    /// The "use my location" success path: a mocked portal fix becomes the route
    /// origin and the journey re-plans from it. The real coordinates depend on
    /// the host location service + user permission (unverifiable headlessly);
    /// this proves the result *handling* around the portal.
    #[test]
    fn use_my_location_success_sets_origin_from_fix() {
        let mut a = app_at(1280.0, 800.0);
        a.select_place("marienplatz");
        a.enter_directions();

        a.locating = true;
        a.location_rx = Some(location::inject_for_test(LocationOutcome::Located {
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
        a.location_rx = Some(location::inject_for_test(LocationOutcome::Unavailable(
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
    /// tests (see `ui/src/widget/image_viewer.rs`).
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

    #[test]
    fn map_info_sheet_stays_modal_over_extracted_controls() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut app = app_at(1280.0, 720.0);
        app.map_info = Some(MapInfo::Attribution);
        let mut scene = Scene::new();
        app.draw_app(&mut scene, Affine::IDENTITY, &mut text, &theme);

        let traffic = app
            .map_controls
            .rect(MapControlAction::ToggleTraffic)
            .center();
        move_to(&mut app, traffic.x, traffic.y);
        assert!(press_left(&mut app));
        assert_eq!(app.map_info, None, "the modal sheet consumed the press");
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
            .click_targets
            .iter()
            .find(|(_, h)| matches!(h, Hit::EditOrigin))
            .map(|(r, _)| r.center())
            .expect("From field recorded");
        move_to(&mut a, from.x, from.y);
        assert!(press_left(&mut a), "focusing the From field is handled");
        assert_eq!(
            a.edit_field,
            Some(EndField::Origin),
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
            !a.origin_results.is_empty(),
            "typing surfaces matches live (no Enter)"
        );
        assert!(
            a.endpoint_debounce.is_some(),
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
            .click_targets
            .iter()
            .rev()
            .find(|(_, h)| matches!(h, Hit::PickOrigin(_)))
            .map(|(r, _)| *r)
            .expect("overlay records PickOrigin hot-regions");
        let field = a.origin_field_rect.expect("From field rect cached");
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
        let topmost = a
            .click_targets
            .iter()
            .rev()
            .find(|(r, _)| r.contains(pick.center()))
            .map(|(_, h)| h.clone());
        assert!(
            matches!(topmost, Some(Hit::PickOrigin(_))),
            "overlay row is topmost"
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
    // `ui/src/widget/slash_menu.rs`); `handle_key` only reads `logical_key`.

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

    /// Ctrl+V pastes the system clipboard into the focused search field via the
    /// shared `TextInput` — the hand-rolled field had no clipboard at all. Primes
    /// the real clipboard, checks it round-trips, then drives Ctrl+V through the
    /// real event path and asserts the pasted text reached `model.query`.
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
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
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
        key_named(&mut a, NamedKey::Escape);
        assert!(!a.search.is_focused(), "Escape unfocuses search");
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

    // --- Click-dispatch coverage for previously-untested Hits ---------------

    /// Clearing the search (the × button's `Hit`) empties the query and results.
    #[test]
    fn clear_search_dispatch() {
        let mut a = app_at(1280.0, 720.0);
        a.model.query = "café".to_string();
        a.run_search();
        assert!(!a.model.results.is_empty(), "search populated results");
        tap(&mut a, &Hit::Search(SearchHit::Clear));
        assert!(a.model.query.is_empty(), "clear empties the query");
        assert!(a.model.results.is_empty(), "clear empties the results");
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
            std::env::temp_dir().join(format!("qos-maps-cctest-{}-{}", std::process::id(), uniq));
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
        std::env::temp_dir().join(format!("qos-maps-{tag}-{}-{}", std::process::id(), n))
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
}
