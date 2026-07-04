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

use ui::presence::PresenceCtx;
use ui::permissions::PermissionContext;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;

use crate::canvas::{self, MapPalette};
use crate::doc::MapsDoc;
use crate::draw;
use crate::geo::{self, Projection};
use crate::location::{self, LocationChannel, LocationOutcome};
use crate::model::{
    Category, LatLon, MapModel, Panel, TransitMode, TransitTimeMode, TravelMode,
};
use crate::net::{self, NetMsg};
use crate::sample::{self, Basemap};
use crate::tiles::{GeoBounds, TileLayer};

/// Which Directions endpoint field (From / To) currently has keyboard focus for
/// typed geocoding. `None` when neither is being edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EndField {
    Origin,
    Dest,
}

// --- Layout metrics (logical px) -------------------------------------------
const MARGIN: f64 = 16.0;
const SEARCH_H: f64 = 48.0;
const GAP: f64 = 10.0;
const TAB_H: f64 = 34.0;
const CTRL_SIZE: f64 = 40.0;
const ROW_H: f64 = 56.0;
/// Below this window width the side panel stops being docked and becomes a
/// closed overlay drawer that the user opens over the map (Adwaita flap).
const NAV_OVERLAY_BELOW: f64 = 720.0;
/// Widest the panel is ever drawn (docked, or as an overlay drawer).
const PANEL_MAX_W: f64 = 384.0;
const PANEL_MIN_W: f64 = 300.0;

/// A clickable hot-region recorded during draw and hit-tested on click.
#[derive(Clone, Debug)]
enum Hit {
    /// Swallow the click (panel chrome) — don't pan the map.
    Consume,
    FocusSearch,
    ClearSearch,
    Tab(Panel),
    Mode(TravelMode),
    SelectPlace(String),
    Recent(String),
    ZoomIn,
    ZoomOut,
    Locate,
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
    /// Fetch the device's real location (portal / GeoClue) and use it as origin.
    UseMyLocation,
    /// Set the Transit depart/arrive anchor.
    TransitTimeMode(TransitTimeMode),
    /// Shift the Transit time anchor by ±minutes.
    TransitTimeDelta(i32),
    /// Reset the Transit time anchor to "leave now".
    TransitTimeNow,
    Journey(usize),
    /// Open the offline-download / manage-cache overlay.
    OfflineOpen,
    /// Close the overlay.
    OfflineClose,
    /// Adjust how many extra zoom levels the offline region covers (±1).
    OfflineZoomDelta(i8),
    /// Begin downloading the current viewport region for offline use.
    OfflineDownload,
    /// Delete the on-disk tile cache.
    OfflineClearCache,
}

pub struct MapsApp {
    model: MapModel,
    basemap: Basemap,
    rect: Rect,
    node: Option<NodeId>,

    // interaction state (render/UI caches only — app state lives in `model`)
    last_mouse: Point,
    search_focused: bool,
    /// Which Directions endpoint field is being typed into, if any.
    edit_field: Option<EndField>,
    /// Geocoded dropdown results for the "From" field (render cache).
    origin_results: Vec<crate::model::Place>,
    /// Geocoded dropdown results for the "To" field (render cache).
    dest_results: Vec<crate::model::Place>,
    left_down: bool,
    dragging: bool,
    drag_last: Point,
    drag_moved: bool,
    click_targets: Vec<(Rect, Hit)>,
    scroll: f64,
    content_h: f64,
    content_view_h: f64,
    start: Instant,

    // CRDT bridge
    loaded: bool,
    dirty: bool,

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

    // offline-download / cache-management overlay
    offline_open: bool,
    offline_zoom_extra: u8,
    cache_bytes: u64,

    /// Whether the side panel drawer is open. Only consulted in the narrow
    /// overlay layout; when docked the panel is always shown. Re-evaluated
    /// against the live window width every frame — never cached at startup.
    nav_open: bool,
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
        Self::build(MapModel::default(), Basemap::default(), TileLayer::new(), false, true)
    }

    /// Headless/offline variant used by `--shots` and tests: no worker thread,
    /// no network. Renders the bundled sample city (places, a route, transit
    /// options) and the stylised vector basemap so the screenshot is fully
    /// populated without ever touching the wire. The overlay drawer starts
    /// open so narrow screenshots show the panel content.
    pub fn offline() -> Self {
        Self::build(sample::default_model(), sample::basemap(), TileLayer::offline(), true, false)
    }

    fn build(model: MapModel, basemap: Basemap, tiles: TileLayer, nav_open: bool, online: bool) -> Self {
        let (net_tx, net_rx) = std::sync::mpsc::channel();
        let cache_bytes = tiles.cache_bytes();
        Self {
            model,
            basemap,
            rect: Rect::ZERO,
            node: None,
            last_mouse: Point::ZERO,
            search_focused: false,
            edit_field: None,
            origin_results: Vec::new(),
            dest_results: Vec::new(),
            left_down: false,
            dragging: false,
            drag_last: Point::ZERO,
            drag_moved: false,
            click_targets: Vec::new(),
            scroll: 0.0,
            content_h: 0.0,
            content_view_h: 0.0,
            start: Instant::now(),
            loaded: false,
            dirty: true,
            net_tx,
            net_rx,
            pending: 0,
            online,
            location_rx: None,
            locating: false,
            tiles,
            offline_open: false,
            offline_zoom_extra: 2,
            cache_bytes,
            nav_open,
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

    /// Screenshot-only hook: anchor the Transit picker to an "Arrive by" time an
    /// hour out, so the stepped time label and the "Now" reset chip render.
    pub fn set_demo_transit_arrive(&mut self) {
        self.model.panel = Panel::Transit;
        self.model.transit_time = crate::model::TransitTime {
            mode: TransitTimeMode::Arrive,
            offset_min: 60,
        };
    }

    /// Screenshot/test helper: an offline (no-worker, no-network) app that
    /// mirrors the live first-run state — an empty model and a plain themed
    /// basemap — so the empty Explore hint can be captured hermetically.
    pub fn offline_empty() -> Self {
        Self::build(MapModel::default(), Basemap::default(), TileLayer::offline(), true, false)
    }

    // --- geometry helpers --------------------------------------------------
    fn view_center(&self) -> Point {
        Point::new(
            (self.rect.x0 + self.rect.x1) / 2.0,
            (self.rect.y0 + self.rect.y1) / 2.0,
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

    fn zoom_in_rect(&self) -> Rect {
        let x1 = self.rect.x1 - MARGIN;
        let y0 = self.rect.y0 + MARGIN;
        Rect::new(x1 - CTRL_SIZE, y0, x1, y0 + CTRL_SIZE)
    }
    fn zoom_out_rect(&self) -> Rect {
        let r = self.zoom_in_rect();
        Rect::new(r.x0, r.y1, r.x1, r.y1 + CTRL_SIZE)
    }
    fn locate_rect(&self) -> Rect {
        let r = self.zoom_out_rect();
        Rect::new(r.x0, r.y1 + GAP, r.x1, r.y1 + GAP + CTRL_SIZE)
    }
    fn download_rect(&self) -> Rect {
        let r = self.locate_rect();
        Rect::new(r.x0, r.y1 + GAP, r.x1, r.y1 + GAP + CTRL_SIZE)
    }

    /// The geographic bounds currently visible in the map viewport.
    fn viewport_bounds(&self) -> GeoBounds {
        let proj = self.projection();
        let nw = proj.unproject(Point::new(self.rect.x0, self.rect.y0));
        let se = proj.unproject(Point::new(self.rect.x1, self.rect.y1));
        GeoBounds::new(nw, se)
    }

    // --- actions -----------------------------------------------------------
    fn mark_dirty(&mut self) {
        self.dirty = true;
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
        // Switching to a routing tab actually performs the lookup for the
        // current place — the tab is not a passive view. Without this the
        // Transit tab would render its empty state as if a lookup had run and
        // come back empty, when in fact nothing was ever requested.
        match panel {
            Panel::Directions if self.model.route.is_none() => {
                self.recompute_route();
            }
            Panel::Transit
                if self.model.transit.is_empty()
                    && self.pending == 0
                    && self.model.selected_place().is_some() =>
            {
                self.recompute_transit();
            }
            _ => {}
        }
        self.mark_dirty();
    }

    /// Resolve the route/transit origin. Uses a prior origin if one is set,
    /// otherwise the current map centre (an honest "from here"), labelled as
    /// such — we never invent a specific "your location" without a real fix.
    fn resolve_origin(&mut self) -> crate::model::LatLon {
        match self.model.origin {
            Some(o) => o,
            None => {
                self.model.origin_name = "Map centre".to_string();
                self.model.center
            }
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
        results
    }

    /// Local (offline) search: substring match over known places. Also kicks
    /// off a live Nominatim request whose results fold in later.
    fn run_search(&mut self) {
        let q = self.model.query.trim().to_string();
        if q.is_empty() {
            return;
        }
        let results = self.local_matches(&q);
        if let Some(first) = results.first() {
            let id = first.id.clone();
            self.model.center = first.pos;
            self.model.selected = Some(id);
        }
        self.model.results = results;
        self.model.push_recent(&q);
        self.model.panel = Panel::Explore;
        self.scroll = 0.0;
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
        self.search_focused = false;
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

    /// Run a live Nominatim geocode for an endpoint field (on Enter), folding in
    /// the instant offline matches first so the dropdown is never briefly empty.
    fn run_endpoint_search(&mut self, origin: bool) {
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
                let _ = tx.send(NetMsg::EndpointSearch { origin, query: q, results });
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
            self.model.origin_name = p.name.clone();
            self.origin_results.clear();
            self.model.origin_query.clear();
        } else {
            self.model.dest = Some(p.pos);
            self.model.dest_name = p.name.clone();
            self.model.selected = Some(p.id.clone());
            self.dest_results.clear();
            self.model.dest_query.clear();
        }
        self.edit_field = None;
        self.model.center = p.pos;
        if self.model.panel == Panel::Transit {
            self.recompute_transit();
        } else {
            self.recompute_route();
        }
        self.mark_dirty();
    }

    /// Kick off a real device-location fix via the location portal (GeoClue2
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

    /// Build the transit query time anchor from the picker. "Leave now" sends no
    /// time (the planner uses its own now); otherwise an RFC-3339 wall clock at
    /// `now + offset`, flagged depart/arrive.
    fn transit_query(&self) -> net::TransitQuery {
        let tt = self.model.transit_time;
        let time = if tt.is_now() {
            None
        } else {
            let when = chrono::Local::now() + chrono::Duration::minutes(tt.offset_min as i64);
            Some(when.to_rfc3339())
        };
        net::TransitQuery {
            arrive_by: tt.arrive_by(),
            time,
        }
    }

    /// Start (or restart) routing to the selected place, forcing the
    /// destination to the current selection. Tapping "Directions" on a place
    /// card or pressing `d` re-targets the route at whatever is selected now.
    fn enter_directions(&mut self) {
        self.set_dest_to_selection();
        self.model.panel = Panel::Directions;
        self.recompute_route();
    }

    /// Start (or restart) a transit lookup to the selected place, forcing the
    /// destination to the current selection.
    fn enter_transit(&mut self) {
        self.set_dest_to_selection();
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
        let dest = match self.model.dest {
            Some(d) => d,
            None => return,
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

    fn recompute_transit(&mut self) {
        if self.model.dest.is_none() {
            self.set_dest_to_selection();
        }
        let dest = match self.model.dest {
            Some(d) => d,
            None => return,
        };
        let origin = self.resolve_origin();
        self.model.origin = Some(origin);
        self.model.panel = Panel::Transit;
        self.model.transit_choice = 0;
        self.nav_open = true;
        // Real departures only: clear any stale itineraries and show a loading
        // state until the planner responds — we never fabricate departures.
        self.model.transit.clear();
        self.pending += 1;
        self.model.status = "Finding departures…".to_string();
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
            self.recompute_route();
        }
    }

    fn toggle_save_selected(&mut self) {
        if let Some(id) = self.model.selected.clone() {
            self.model.toggle_saved(&id);
            self.mark_dirty();
        }
    }

    fn clear_route(&mut self) {
        self.model.route = None;
        self.mark_dirty();
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
        if let Some(route) = &self.model.route {
            // In the docked layout the map shares the width with the panel; in
            // the overlay layout the drawer floats over a full-width map.
            let w = if self.overlay_mode() {
                self.rect.width() - MARGIN * 2.0
            } else {
                self.rect.width() - (self.panel_w() + MARGIN * 2.0 + GAP)
            };
            let h = self.rect.height();
            if let Some((c, z)) = geo::fit_bounds(&route.geometry, w.max(200.0), h, 70.0) {
                self.model.center = c;
                self.model.zoom = z.min(16.5);
            }
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
        self.mark_dirty();
    }

    fn pan_by(&mut self, dx: f64, dy: f64) {
        let cw = geo::lonlat_to_world(self.model.center, self.model.zoom);
        self.model.center = geo::world_to_lonlat(cw.0 - dx, cw.1 - dy, self.model.zoom);
        self.mark_dirty();
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
            if d <= r + 5.0 {
                if best.as_ref().map(|(bd, _)| d < *bd).unwrap_or(true) {
                    best = Some((d, place.id.clone()));
                }
            }
        }
        best.map(|(_, id)| id)
    }

    fn drain_net(&mut self) {
        while let Ok(msg) = self.net_rx.try_recv() {
            self.pending = self.pending.saturating_sub(1);
            match msg {
                NetMsg::Search { query, results } => match results {
                    Ok(found) if !found.is_empty() => {
                        for p in &found {
                            self.model.upsert_place(p.clone());
                        }
                        self.model.results = found;
                        if let Some(first) = self.model.results.first() {
                            self.model.center = first.pos;
                            self.model.selected = Some(first.id.clone());
                        }
                        self.model.status = format!("{} results for “{}”", self.model.results.len(), query);
                    }
                    Ok(_) => self.model.status = format!("No results for “{query}”"),
                    Err(e) => self.model.status = format!("Search failed: {e}"),
                },
                NetMsg::EndpointSearch { origin, query: _, results } => match results {
                    Ok(found) if !found.is_empty() => {
                        for p in &found {
                            self.model.upsert_place(p.clone());
                        }
                        if origin {
                            self.origin_results = found;
                        } else {
                            self.dest_results = found;
                        }
                        self.model.status.clear();
                    }
                    Ok(_) => self.model.status = "No matching places".to_string(),
                    Err(e) => self.model.status = format!("Search failed: {e}"),
                },
                NetMsg::ReverseGeocode(r) => {
                    if let Ok(label) = r {
                        if !label.is_empty() {
                            // Refine the device-location origin's label in place.
                            self.model.origin_name = label;
                        }
                    }
                }
                NetMsg::Route(r) => match r {
                    Ok(route) => {
                        self.model.route = Some(route);
                        self.fit_route();
                        self.model.status = "Route updated".to_string();
                    }
                    Err(e) => self.model.status = format!("Routing failed (offline estimate shown): {e}"),
                },
                NetMsg::Transit(t) => match t {
                    Ok(j) if !j.is_empty() => {
                        self.model.transit = j;
                        self.model.status = "Departures updated".to_string();
                    }
                    Ok(_) => self.model.status = "No transit options found".to_string(),
                    Err(e) => self.model.status = format!("Transit lookup failed: {e}"),
                },
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
                    if self.model.panel == Panel::Transit {
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
    fn dispatch(&mut self, hit: &Hit) {
        match hit {
            Hit::Consume => {}
            Hit::FocusSearch => self.search_focused = true,
            Hit::ClearSearch => {
                self.model.query.clear();
                self.model.results.clear();
                self.mark_dirty();
            }
            Hit::Tab(p) => self.set_panel(*p),
            Hit::Mode(m) => self.set_mode(*m),
            Hit::SelectPlace(id) => {
                self.select_place(id);
            }
            Hit::Recent(q) => {
                self.model.query = q.clone();
                self.run_search();
            }
            Hit::ZoomIn => self.zoom_by(1.0, self.view_center()),
            Hit::ZoomOut => self.zoom_by(-1.0, self.view_center()),
            Hit::Locate => {
                // Fetch the device's real position (portal / GeoClue) off the UI
                // thread. `drain_location` recentres on the fix, or reports it
                // unavailable — we never invent a location.
                self.use_my_location();
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
            Hit::ToggleSave => self.toggle_save_selected(),
            Hit::ClearRoute => self.clear_route(),
            Hit::SwapEnds => self.swap_ends(),
            Hit::EditOrigin => self.focus_endpoint(EndField::Origin),
            Hit::EditDest => self.focus_endpoint(EndField::Dest),
            Hit::PickOrigin(i) => self.pick_endpoint(true, *i),
            Hit::PickDest(i) => self.pick_endpoint(false, *i),
            Hit::UseMyLocation => self.use_my_location(),
            Hit::TransitTimeMode(m) => {
                self.model.transit_time.mode = *m;
                self.recompute_transit();
                self.mark_dirty();
            }
            Hit::TransitTimeDelta(d) => {
                let v = (self.model.transit_time.offset_min + *d).clamp(0, 1440);
                self.model.transit_time.offset_min = v;
                self.recompute_transit();
                self.mark_dirty();
            }
            Hit::TransitTimeNow => {
                self.model.transit_time = crate::model::TransitTime::default();
                self.recompute_transit();
                self.mark_dirty();
            }
            Hit::Journey(i) => {
                self.model.transit_choice = *i;
                self.mark_dirty();
            }
            Hit::OfflineOpen => {
                self.offline_open = true;
                self.cache_bytes = self.tiles.cache_bytes();
            }
            Hit::OfflineClose => self.offline_open = false,
            Hit::OfflineZoomDelta(d) => {
                let v = self.offline_zoom_extra as i16 + *d as i16;
                self.offline_zoom_extra = v.clamp(0, 6) as u8;
            }
            Hit::OfflineDownload => {
                let base_z = TileLayer::tile_zoom(self.model.zoom);
                let z1 = (base_z + self.offline_zoom_extra).min(19);
                let bounds = self.viewport_bounds();
                let n = self.tiles.start_download(bounds, base_z, z1);
                self.model.status = format!("Downloading {n} tiles for offline use…");
            }
            Hit::OfflineClearCache => {
                let freed = self.tiles.clear_cache();
                self.cache_bytes = 0;
                self.model.status = format!("Cleared {} of offline tiles", fmt_bytes(freed));
            }
        }
    }
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
    syn.iter().any(|s| s.contains(needle) || needle.contains(*s))
}

fn vmid(y: f64, h: f64, size: f32) -> f64 {
    y + (h - size as f64 * 1.25) / 2.0
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

fn transit_color(theme: &Theme, mode: TransitMode) -> Color {
    let c = &theme.tokens.colors;
    match mode {
        TransitMode::Walk => c.text_dim,
        TransitMode::Bus => c.semantic.info,
        TransitMode::Tram => c.semantic.danger,
        TransitMode::Subway => theme.accent_primary,
        TransitMode::Rail => c.semantic.success,
        TransitMode::Ferry => c.accent[6],
    }
}

// ===========================================================================
// Drawing
// ===========================================================================
impl MapsApp {
    fn draw_app(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.click_targets.clear();
        let pal = MapPalette::from_theme(theme);
        let proj = self.projection();
        let clip = self.rect;

        // Map layers. Real OSM raster tiles paint first (non-blocking); the
        // stylised vector basemap is the fallback shown only until imagery is
        // available (headless `--shots`, or the very first offline frame).
        let view_center = self.view_center();
        let painted_tiles =
            canvas::draw_tiles(scene, t, clip, &mut self.tiles, &self.model, view_center, &pal);
        if !painted_tiles {
            if self.basemap.is_empty() {
                // Live first-run / offline gap: no imagery and no vector
                // cartography yet. Draw a reference grid so the map reads as a
                // map rather than a black void, with a loading affordance.
                canvas::draw_grid_substrate(scene, t, clip, &self.model, view_center, &pal);
            } else {
                canvas::draw_basemap(scene, t, clip, &proj, &self.basemap, &pal);
            }
        }
        if let Some(route) = &self.model.route {
            canvas::draw_route(scene, t, &proj, route, &pal);
        }
        canvas::draw_markers(scene, t, tr, &proj, &self.model, theme, &pal, clip);

        // Side panel: docked when wide, an overlay drawer (with a dimming
        // scrim over the map) when narrow.
        let overlay = self.overlay_mode();
        if overlay && self.nav_open {
            draw::fill_rect(scene, t, clip, draw::alpha(Color::BLACK, 0.42));
            self.click_targets.push((clip, Hit::CloseNav));
            self.draw_panel(scene, t, tr, theme);
        } else if !overlay {
            self.draw_panel(scene, t, tr, theme);
        }

        // Scale bar + OSM attribution on top of the drawer scrim so the credit
        // stays fully legible and is never dimmed or clipped by an open drawer
        // (in docked layouts it sits at the far right, clear of the panel).
        canvas::draw_scale_and_attribution(scene, t, tr, clip, &self.model, theme);

        // Loading / offline affordance while the substrate grid is showing —
        // above the map but below the chrome, and skipped once real imagery or
        // the drawer scrim is up (the drawer is the focus then).
        if !painted_tiles && self.basemap.is_empty() && !(overlay && self.nav_open) {
            self.draw_imagery_hint(scene, t, tr, theme);
        }

        // Chrome on top of the scrim so it stays bright and usable.
        self.draw_controls(scene, t, tr, theme);
        self.draw_menu(scene, t, tr, theme);
        self.draw_search(scene, t, tr, theme);
        self.draw_status(scene, t, tr, theme);
        if self.offline_open {
            self.draw_offline_overlay(scene, t, tr, theme);
        }
    }

    /// The leading menu button in the narrow overlay layout: toggles the panel
    /// drawer. Renders nothing when the panel is docked.
    fn draw_menu(&mut self, scene: &mut Scene, t: Affine, _tr: &mut TextRenderer, theme: &Theme) {
        let Some(r) = self.menu_rect() else { return };
        let c = &theme.tokens.colors;
        let border = if self.nav_open { theme.border_focus } else { c.border_standard };
        draw::card(scene, t, r, 12.0, c.surfaces.raised, border);
        // Three-line hamburger, or a back-chevron feel when open (rotate hint
        // via colour). Kept as clean strokes — no icon font dependency.
        let cx = r.center().x;
        let cy = r.center().y;
        let hw = 8.0;
        let col = if self.nav_open { theme.accent_primary } else { theme.text_primary };
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
        // Focus target first so it sits above the drawer scrim but below the
        // clear (×) button, which is pushed later and therefore wins its area.
        self.click_targets.push((r, Hit::FocusSearch));
        let c = &theme.tokens.colors;
        let border = if self.search_focused { theme.border_focus } else { c.border_standard };
        draw::card(scene, t, r, 12.0, c.surfaces.raised, border);
        // Search glyph (a magnifier drawn as a circle + handle).
        let gx = r.x0 + 22.0;
        let gy = r.y0 + r.height() / 2.0;
        draw::stroke_circle(scene, t, Point::new(gx, gy - 1.0), 6.0, 2.0, theme.text_dim);
        draw::line(
            scene,
            t,
            Point::new(gx + 4.5, gy + 3.5),
            Point::new(gx + 9.0, gy + 8.0),
            2.0,
            theme.text_dim,
        );

        let text_x = r.x0 + 40.0;
        let avail = r.x1 - text_x - 34.0;
        let size = 15.0_f32;
        let ty = vmid(r.y0, r.height(), size);
        if self.model.query.is_empty() {
            draw::text(scene, tr, t, "Search places, addresses…", text_x, ty, size, theme.text_dim);
        } else {
            draw::text_clip(scene, tr, t, &self.model.query, text_x, ty, size, theme.text_primary, avail);
            // Caret when focused.
            if self.search_focused && self.caret_on() {
                let w = draw::measure_w(tr, &self.model.query, size).min(avail);
                draw::line(
                    scene,
                    t,
                    Point::new(text_x + w + 2.0, r.y0 + 12.0),
                    Point::new(text_x + w + 2.0, r.y1 - 12.0),
                    1.5,
                    theme.accent_primary,
                );
            }
            // Clear (×) button.
            let cb = Rect::new(r.x1 - 32.0, r.y0 + 10.0, r.x1 - 10.0, r.y1 - 10.0);
            draw::fill_circle(scene, t, cb.center(), 10.0, c.surfaces.base);
            let cc = cb.center();
            draw::line(scene, t, Point::new(cc.x - 4.0, cc.y - 4.0), Point::new(cc.x + 4.0, cc.y + 4.0), 1.8, theme.text_dim);
            draw::line(scene, t, Point::new(cc.x + 4.0, cc.y - 4.0), Point::new(cc.x - 4.0, cc.y + 4.0), 1.8, theme.text_dim);
            self.click_targets.push((cb, Hit::ClearSearch));
        }
    }

    fn caret_on(&self) -> bool {
        (self.start.elapsed().as_secs_f64() * 1.6) as u64 % 2 == 0
    }

    fn draw_controls(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        // Zoom in/out as one stacked card.
        let zi = self.zoom_in_rect();
        let zo = self.zoom_out_rect();
        let combined = Rect::new(zi.x0, zi.y0, zi.x1, zo.y1);
        draw::card(scene, t, combined, 10.0, c.surfaces.raised, c.border_standard);
        draw::line(scene, t, Point::new(zi.x0 + 8.0, zi.y1), Point::new(zi.x1 - 8.0, zi.y1), 1.0, c.border_standard);
        draw::text_center(scene, tr, t, "+", zi.x0, zi.x1, vmid(zi.y0, zi.height(), 22.0), 22.0, theme.text_primary);
        draw::text_center(scene, tr, t, "–", zo.x0, zo.x1, vmid(zo.y0, zo.height(), 22.0), 22.0, theme.text_primary);
        self.click_targets.push((zi, Hit::ZoomIn));
        self.click_targets.push((zo, Hit::ZoomOut));

        // Locate button.
        let lc = self.locate_rect();
        draw::card(scene, t, lc, 10.0, c.surfaces.raised, c.border_standard);
        let ctr = lc.center();
        draw::stroke_circle(scene, t, ctr, 6.0, 2.0, theme.accent_primary);
        draw::fill_circle(scene, t, ctr, 2.0, theme.accent_primary);
        for (dx, dy) in [(0.0, -9.0), (0.0, 9.0), (-9.0, 0.0), (9.0, 0.0)] {
            draw::line(scene, t, Point::new(ctr.x + dx * 0.7, ctr.y + dy * 0.7), Point::new(ctr.x + dx, ctr.y + dy), 2.0, theme.accent_primary);
        }
        self.click_targets.push((lc, Hit::Locate));

        // Offline-download button: a downward arrow into a tray.
        let dl = self.download_rect();
        let dl_active = self.tiles.progress().is_active();
        let dl_border = if dl_active { theme.accent_primary } else { c.border_standard };
        draw::card(scene, t, dl, 10.0, c.surfaces.raised, dl_border);
        let dc = dl.center();
        draw::line(scene, t, Point::new(dc.x, dc.y - 8.0), Point::new(dc.x, dc.y + 3.0), 2.0, theme.text_primary);
        draw::line(scene, t, Point::new(dc.x - 4.0, dc.y - 1.0), Point::new(dc.x, dc.y + 3.0), 2.0, theme.text_primary);
        draw::line(scene, t, Point::new(dc.x + 4.0, dc.y - 1.0), Point::new(dc.x, dc.y + 3.0), 2.0, theme.text_primary);
        draw::line(scene, t, Point::new(dc.x - 6.0, dc.y + 7.0), Point::new(dc.x + 6.0, dc.y + 7.0), 2.0, theme.text_primary);
        if dl_active {
            // A small accent ring tick as a busy hint.
            draw::stroke_circle(scene, t, Point::new(dl.x1 - 7.0, dl.y0 + 7.0), 4.0, 2.0, theme.accent_primary);
        }
        self.click_targets.push((dl, Hit::OfflineOpen));
    }

    /// The offline-download + manage-cache overlay: a centred card with a
    /// zoom-depth stepper, an estimated-tile/progress readout, and
    /// download / clear-cache / close actions.
    fn draw_offline_overlay(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        // Scrim — also swallows background clicks.
        draw::fill_rect(scene, t, self.rect, draw::alpha(Color::BLACK, 0.45));
        self.click_targets.push((self.rect, Hit::OfflineClose));

        let w = 420.0_f64.min(self.rect.width() - 40.0);
        let h = 318.0_f64;
        let cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        let card = Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
        draw::card(scene, t, card, 16.0, c.surfaces.raised, c.border_standard);
        // Swallow clicks on the card itself.
        self.click_targets.push((card, Hit::Consume));

        let x = card.x0 + 22.0;
        let right = card.x1 - 22.0;
        let mut y = card.y0 + 20.0;
        draw::text_w(scene, tr, t, "Offline maps", x, y, 18.0, theme.text_primary, TextWeight::Bold);
        // Close (×).
        let close = Rect::new(right - 26.0, card.y0 + 16.0, right, card.y0 + 42.0);
        draw::text_center(scene, tr, t, "✕", close.x0, close.x1, vmid(close.y0, 26.0, 14.0), 14.0, theme.text_dim);
        self.click_targets.push((close, Hit::OfflineClose));
        y += 30.0;

        draw::text(scene, tr, t, "Download the current map area for navigation without a connection.", x, y, 12.0, theme.text_dim);
        y += 28.0;

        // Zoom-depth stepper.
        let base_z = TileLayer::tile_zoom(self.model.zoom);
        let z0 = base_z;
        let z1 = (base_z + self.offline_zoom_extra).min(19);
        draw::text_w(scene, tr, t, "Detail (zoom levels)", x, y, 13.0, theme.text_primary, TextWeight::Semibold);
        let minus = Rect::new(right - 84.0, y - 4.0, right - 58.0, y + 22.0);
        let plus = Rect::new(right - 26.0, y - 4.0, right, y + 22.0);
        draw::fill_rrect(scene, t, minus, 7.0, c.surfaces.base);
        draw::fill_rrect(scene, t, plus, 7.0, c.surfaces.base);
        draw::text_center(scene, tr, t, "–", minus.x0, minus.x1, vmid(minus.y0, 26.0, 18.0), 18.0, theme.text_primary);
        draw::text_center(scene, tr, t, "+", plus.x0, plus.x1, vmid(plus.y0, 26.0, 18.0), 18.0, theme.text_primary);
        draw::text_center(scene, tr, t, &format!("z{z0}–z{z1}"), minus.x1, plus.x0, vmid(y - 4.0, 26.0, 13.0), 13.0, theme.accent_primary);
        self.click_targets.push((minus, Hit::OfflineZoomDelta(-1)));
        self.click_targets.push((plus, Hit::OfflineZoomDelta(1)));
        y += 34.0;

        // Estimate / progress readout.
        let bounds = self.viewport_bounds();
        let est = TileLayer::estimate_download(bounds, z0, z1);
        let prog = self.tiles.progress();
        let readout = if prog.is_active() {
            format!("Downloading {}/{} tiles…", prog.done, prog.total)
        } else if prog.total > 0 && prog.done >= prog.total {
            let failed = if prog.failed > 0 { format!(" ({} failed)", prog.failed) } else { String::new() };
            format!("Downloaded {} tiles{failed}", prog.total)
        } else {
            format!("≈ {est} tiles ({})", fmt_bytes(est as u64 * 18_000))
        };
        draw::text(scene, tr, t, &readout, x, y, 12.0, theme.text_dim);
        y += 22.0;

        // Progress bar.
        let bar = Rect::new(x, y, right, y + 8.0);
        draw::fill_rrect(scene, t, bar, 4.0, c.surfaces.base);
        let frac = if prog.total > 0 { prog.fraction() } else { 0.0 };
        if frac > 0.0 {
            let fill_w = (bar.width() * frac).max(4.0);
            draw::fill_rrect(scene, t, Rect::new(bar.x0, bar.y0, bar.x0 + fill_w, bar.y1), 4.0, theme.accent_primary);
        }
        y += 24.0;

        // Cache size line.
        draw::text(scene, tr, t, &format!("Cached on disk: {}", fmt_bytes(self.cache_bytes)), x, y, 12.0, theme.text_primary);
        y += 30.0;

        // Action buttons: Download (primary) + Clear cache.
        let bh = 38.0;
        let gap = 10.0;
        let bw = (right - x - gap) / 2.0;
        let dl = Rect::new(x, y, x + bw, y + bh);
        let clr = Rect::new(x + bw + gap, y, right, y + bh);
        draw::fill_rrect(scene, t, dl, 9.0, theme.accent_primary);
        draw::text_center(scene, tr, t, "Download this area", dl.x0, dl.x1, vmid(dl.y0, bh, 13.0), 13.0, Color::WHITE);
        self.click_targets.push((dl, Hit::OfflineDownload));
        draw::fill_rrect(scene, t, clr, 9.0, c.surfaces.base);
        draw::stroke_rrect(scene, t, clr, 9.0, 1.0, c.border_standard);
        draw::text_center(scene, tr, t, "Clear cache", clr.x0, clr.x1, vmid(clr.y0, bh, 13.0), 13.0, c.semantic.danger);
        self.click_targets.push((clr, Hit::OfflineClearCache));
    }

    fn draw_status(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
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
        let cx = (self.rect.x0 + self.rect.x1) / 2.0;
        // Sit below the top search row so it never overlaps the search field
        // (which spans most of the top edge in the narrow layout).
        let top = self.rect.y0 + MARGIN + SEARCH_H + GAP;
        let r = Rect::new(cx - w / 2.0, top, cx + w / 2.0, top + 30.0);
        draw::card(scene, t, r, 15.0, theme.tokens.colors.surfaces.raised, theme.tokens.colors.border_standard);
        // Small spinner dot.
        let dot = Point::new(r.x0 + 16.0, r.center().y);
        draw::fill_circle(scene, t, dot, 4.0, theme.accent_primary);
        draw::text(scene, tr, t, label, r.x0 + 28.0, vmid(r.y0, r.height(), size), size, theme.text_primary);
    }

    /// A centred pill over the substrate grid telling the user the imagery is
    /// loading (or unreachable), so a first-run / offline map is never a silent
    /// void. Positioned in the visible map area, clear of the docked panel.
    fn draw_imagery_hint(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let offline = self.tiles.has_failures() && !self.tiles.has_inflight();
        let msg = if offline {
            "Map imagery unavailable offline"
        } else {
            "Loading map…"
        };
        // Centre within the map area (right of the docked panel when present).
        let area_x0 = if self.panel_visible() && !self.overlay_mode() {
            self.panel_rect().x1 + GAP
        } else {
            self.rect.x0
        };
        let cx = (area_x0 + self.rect.x1) / 2.0;
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        let size = 13.0_f32;
        let w = draw::measure_w(tr, msg, size) + 44.0;
        let r = Rect::new(cx - w / 2.0, cy - 17.0, cx + w / 2.0, cy + 17.0);
        let c = &theme.tokens.colors;
        draw::card(scene, t, r, 17.0, c.surfaces.raised, c.border_standard);
        let dot = Point::new(r.x0 + 18.0, r.center().y);
        let dot_col = if offline { c.semantic.warning } else { theme.accent_primary };
        draw::fill_circle(scene, t, dot, 4.0, dot_col);
        draw::text(scene, tr, t, msg, r.x0 + 30.0, vmid(r.y0, r.height(), size), size, theme.text_primary);
    }

    fn draw_panel(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let pr = self.panel_rect();
        if pr.height() < 80.0 {
            return;
        }
        let c = &theme.tokens.colors;
        draw::card(scene, t, pr, 14.0, c.surfaces.raised, c.border_standard);
        // Swallow background clicks.
        self.click_targets.push((pr, Hit::Consume));

        // Segmented tabs.
        let inset = 10.0;
        let tabs_rect = Rect::new(pr.x0 + inset, pr.y0 + inset, pr.x1 - inset, pr.y0 + inset + TAB_H);
        draw::fill_rrect(scene, t, tabs_rect, 9.0, c.surfaces.base);
        let seg_w = tabs_rect.width() / 3.0;
        for (i, p) in Panel::ALL.iter().enumerate() {
            let x0 = tabs_rect.x0 + i as f64 * seg_w;
            let seg = Rect::new(x0, tabs_rect.y0, x0 + seg_w, tabs_rect.y1);
            let active = self.model.panel == *p;
            if active {
                draw::fill_rrect(scene, t, seg.inflate(-3.0, -3.0), 7.0, theme.accent_primary);
            }
            let col = if active { Color::WHITE } else { theme.text_dim };
            draw::text_center(scene, tr, t, p.label(), seg.x0, seg.x1, vmid(seg.y0, seg.height(), 13.0), 13.0, col);
            self.click_targets.push((seg, Hit::Tab(*p)));
        }

        // Content region (below tabs), clipped + scrolled.
        let content = Rect::new(pr.x0 + inset, tabs_rect.y1 + 10.0, pr.x1 - inset, pr.y1 - 10.0);
        self.content_view_h = content.height();
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &content);
        let cy = content.y0 - self.scroll;
        let drawn = match self.model.panel {
            Panel::Explore => self.draw_explore(scene, t, tr, theme, content, cy),
            Panel::Directions => self.draw_directions(scene, t, tr, theme, content, cy),
            Panel::Transit => self.draw_transit(scene, t, tr, theme, content, cy),
        };
        scene.pop_layer();
        self.content_h = drawn - content.y0 + self.scroll;
    }

    // Returns the y of the bottom of the drawn content (absolute).
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

        // First-run empty state: nothing selected, searched, saved or recent.
        // An honest hint toward the primary action rather than a blank panel.
        let empty = self.model.selected_place().is_none()
            && self.model.results.is_empty()
            && self.model.saved_places().is_empty()
            && self.model.recents.is_empty();
        if empty {
            draw::text_w(scene, tr, t, "Find your way", x, y, 16.0, theme.text_primary, TextWeight::Bold);
            draw::text(scene, tr, t, "Search for a place or address", x, y + 28.0, 13.0, theme.text_dim);
            draw::text(scene, tr, t, "to route and plan transit.", x, y + 46.0, 13.0, theme.text_dim);
            return y + 78.0;
        }

        // Selected place card.
        if let Some(sel) = self.model.selected_place().cloned() {
            let card_h = 132.0;
            let cr = Rect::new(x, y, right, y + card_h);
            draw::fill_rrect(scene, t, cr, 12.0, c.surfaces.base);
            // Category swatch.
            let cat_col = canvas::category_color(theme, sel.category);
            draw::fill_rrect(scene, t, Rect::new(cr.x0, cr.y0, cr.x0 + 5.0, cr.y1), 12.0, cat_col);
            draw::text_w(scene, tr, t, &sel.name, x + 16.0, y + 12.0, 18.0, theme.text_primary, TextWeight::Bold);
            let mut meta = sel.category.label().to_string();
            if let Some(rating) = sel.rating {
                meta = format!("{meta}  ·  ★ {rating:.1}");
            }
            draw::text(scene, tr, t, &meta, x + 16.0, y + 38.0, 12.0, theme.accent_primary);
            draw::text_clip(scene, tr, t, &sel.address, x + 16.0, y + 58.0, 12.0, theme.text_dim, right - x - 28.0);

            // Action buttons row.
            let by = y + 86.0;
            let bw = (right - x - 16.0 - 16.0) / 3.0;
            let bh = 34.0;
            let saved = sel.saved;
            let actions: [(&str, Hit, bool); 3] = [
                ("Directions", Hit::Directions, true),
                ("Transit", Hit::Transit, false),
                (if saved { "Saved ✓" } else { "Save" }, Hit::ToggleSave, false),
            ];
            for (i, (label, hit, primary)) in actions.into_iter().enumerate() {
                let bx = x + 8.0 + i as f64 * (bw + 4.0);
                let br = Rect::new(bx, by, bx + bw, by + bh);
                let (fill, fg) = if primary {
                    (theme.accent_primary, Color::WHITE)
                } else if matches!(hit, Hit::ToggleSave) && saved {
                    (draw::alpha(theme.accent_primary, 0.18), theme.accent_primary)
                } else {
                    (c.surfaces.raised, theme.text_primary)
                };
                draw::fill_rrect(scene, t, br, 8.0, fill);
                if !primary {
                    draw::stroke_rrect(scene, t, br, 8.0, 1.0, c.border_standard);
                }
                draw::text_center(scene, tr, t, label, br.x0, br.x1, vmid(br.y0, bh, 12.0), 12.0, fg);
                self.click_targets.push((br, hit));
            }
            y += card_h + 14.0;
        }

        // Results / recents / saved sections.
        let results = self.model.results.clone();
        if !results.is_empty() {
            draw::text_w(scene, tr, t, "RESULTS", x, y, 11.0, theme.text_dim, TextWeight::Semibold);
            y += 20.0;
            for p in &results {
                y = self.draw_place_row(scene, t, tr, theme, content, y, p);
            }
            y += 8.0;
        }

        let saved = self.model.saved_places().into_iter().cloned().collect::<Vec<_>>();
        if !saved.is_empty() {
            draw::text_w(scene, tr, t, "SAVED", x, y, 11.0, theme.text_dim, TextWeight::Semibold);
            y += 20.0;
            for p in &saved {
                y = self.draw_place_row(scene, t, tr, theme, content, y, p);
            }
            y += 8.0;
        }

        if !self.model.recents.is_empty() {
            draw::text_w(scene, tr, t, "RECENT SEARCHES", x, y, 11.0, theme.text_dim, TextWeight::Semibold);
            y += 20.0;
            let recents = self.model.recents.clone();
            for q in &recents {
                let rr = Rect::new(x, y, right, y + 30.0);
                draw::stroke_circle(scene, t, Point::new(x + 11.0, rr.center().y), 5.0, 1.5, theme.text_dim);
                draw::text(scene, tr, t, q, x + 26.0, vmid(rr.y0, 30.0, 12.0), 12.0, theme.text_primary);
                self.click_targets.push((rr, Hit::Recent(q.clone())));
                y += 32.0;
            }
        }
        y
    }

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
        draw::fill_circle(scene, t, Point::new(r.x0 + 18.0, r.center().y), 9.0, cat_col);
        draw::fill_circle(scene, t, Point::new(r.x0 + 18.0, r.center().y), 3.2, draw::mix(c.surfaces.base, Color::WHITE, 0.7));
        let tx = r.x0 + 36.0;
        draw::text_clip(scene, tr, t, &p.name, tx, r.y0 + 6.0, 13.0, theme.text_primary, r.x1 - tx - 50.0);
        let sub = format!("{} · {}", p.category.label(), p.address);
        draw::text_clip(scene, tr, t, &sub, tx, r.y0 + 24.0, 11.0, theme.text_dim, r.x1 - tx - 50.0);
        if let Some(rt) = p.rating {
            draw::text_right(scene, tr, t, &format!("★ {rt:.1}"), r.x1 - 8.0, r.y0 + 6.0, 11.0, theme.accent_primary);
        }
        if p.saved {
            draw::text_right(scene, tr, t, "♥", r.x1 - 8.0, r.y0 + 24.0, 12.0, theme.tokens.colors.semantic.danger);
        }
        self.click_targets.push((r, Hit::SelectPlace(p.id.clone())));
        y + ROW_H
    }

    /// One typeable endpoint field (a rounded input with a leading dot, the
    /// current text or a placeholder, and a blinking caret while focused).
    #[allow(clippy::too_many_arguments)]
    fn draw_endpoint_field(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
        dot: Color,
        text: &str,
        editing: bool,
        placeholder: &str,
        hit: Hit,
    ) {
        let c = &theme.tokens.colors;
        let border = if editing { theme.border_focus } else { c.border_standard };
        draw::fill_rrect(scene, t, r, 9.0, c.surfaces.base);
        draw::stroke_rrect(scene, t, r, 9.0, 1.0, border);
        draw::fill_circle(scene, t, Point::new(r.x0 + 16.0, r.center().y), 4.0, dot);
        let tx = r.x0 + 30.0;
        let size = 13.0_f32;
        let ty = vmid(r.y0, r.height(), size);
        let avail = r.x1 - tx - 12.0;
        if text.is_empty() {
            draw::text(scene, tr, t, placeholder, tx, ty, size, theme.text_dim);
        } else {
            draw::text_clip(scene, tr, t, text, tx, ty, size, theme.text_primary, avail);
        }
        if editing && self.caret_on() {
            let w = draw::measure_w(tr, text, size).min(avail);
            draw::line(
                scene,
                t,
                Point::new(tx + w + 2.0, r.y0 + 10.0),
                Point::new(tx + w + 2.0, r.y1 - 10.0),
                1.5,
                theme.accent_primary,
            );
        }
        self.click_targets.push((r, hit));
    }

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
        self.draw_endpoint_field(
            scene, t, tr, theme, from_r, theme.accent_primary, &from_text, from_editing,
            "Choose starting point", Hit::EditOrigin,
        );

        let to_y = y + field_h + 8.0;
        let to_r = Rect::new(x, to_y, fields_right, to_y + field_h);
        let to_editing = self.edit_field == Some(EndField::Dest);
        let to_text = if to_editing {
            self.model.dest_query.clone()
        } else if !self.model.dest_name.is_empty() {
            self.model.dest_name.clone()
        } else {
            self.model.selected_place().map(|p| p.name.clone()).unwrap_or_default()
        };
        self.draw_endpoint_field(
            scene, t, tr, theme, to_r, c.semantic.danger, &to_text, to_editing,
            "Choose destination", Hit::EditDest,
        );

        // Swap button spanning both fields on the trailing edge.
        let swap = Rect::new(right - swap_w, y + field_h / 2.0, right, to_r.y1 - field_h / 2.0);
        draw::fill_rrect(scene, t, swap, 8.0, c.surfaces.raised);
        draw::stroke_rrect(scene, t, swap, 8.0, 1.0, c.border_standard);
        draw::text_center(scene, tr, t, "⇅", swap.x0, swap.x1, vmid(swap.center().y - 8.0, 16.0, 16.0), 16.0, theme.text_primary);
        self.click_targets.push((swap, Hit::SwapEnds));
        y = to_r.y1 + 10.0;

        // "Use my location" — a real device-location fix for the origin.
        let loc_h = 32.0;
        let loc_r = Rect::new(x, y, fields_right, y + loc_h);
        let (loc_fill, loc_fg) = if self.locating {
            (draw::alpha(theme.accent_primary, 0.16), theme.accent_primary)
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
            draw::line(scene, t, Point::new(gx + dx * 0.7, gy + dy * 0.7), Point::new(gx + dx, gy + dy), 1.4, loc_fg);
        }
        let loc_label = if self.locating { "Locating…" } else { "Use my location" };
        draw::text(scene, tr, t, loc_label, gx + 16.0, vmid(loc_r.y0, loc_h, 13.0), 13.0, loc_fg);
        self.click_targets.push((loc_r, if self.locating { Hit::Consume } else { Hit::UseMyLocation }));
        y += loc_h + 12.0;

        // Geocoded dropdown for whichever endpoint field is being edited.
        if let Some(field) = self.edit_field {
            let results = match field {
                EndField::Origin => self.origin_results.clone(),
                EndField::Dest => self.dest_results.clone(),
            };
            if results.is_empty() {
                draw::text(scene, tr, t, "Type a place or address, then press Enter.", x, y, 12.0, theme.text_dim);
                y += 22.0;
            } else {
                for (i, p) in results.iter().enumerate().take(6) {
                    let rr = Rect::new(x, y, right, y + ROW_H - 12.0);
                    let cat_col = canvas::category_color(theme, p.category);
                    draw::fill_circle(scene, t, Point::new(rr.x0 + 14.0, rr.center().y), 6.0, cat_col);
                    let tx = rr.x0 + 28.0;
                    draw::text_clip(scene, tr, t, &p.name, tx, rr.y0 + 4.0, 13.0, theme.text_primary, right - tx - 8.0);
                    draw::text_clip(scene, tr, t, &p.address, tx, rr.y0 + 22.0, 11.0, theme.text_dim, right - tx - 8.0);
                    let hit = match field {
                        EndField::Origin => Hit::PickOrigin(i),
                        EndField::Dest => Hit::PickDest(i),
                    };
                    self.click_targets.push((rr, hit));
                    y += ROW_H - 8.0;
                }
            }
            y += 8.0;
        }

        // Mode segmented control.
        let mode_rect = Rect::new(x, y, right, y + 40.0);
        draw::fill_rrect(scene, t, mode_rect, 9.0, c.surfaces.base);
        let seg_w = mode_rect.width() / 3.0;
        for (i, m) in TravelMode::ALL.iter().enumerate() {
            let sx = mode_rect.x0 + i as f64 * seg_w;
            let seg = Rect::new(sx, mode_rect.y0, sx + seg_w, mode_rect.y1);
            let active = self.model.mode == *m;
            if active {
                draw::fill_rrect(scene, t, seg.inflate(-3.0, -3.0), 7.0, theme.accent_primary);
            }
            let fg = if active { Color::WHITE } else { theme.text_dim };
            draw::text_center(scene, tr, t, m.label(), seg.x0, seg.x1, vmid(seg.y0, seg.height(), 13.0), 13.0, fg);
            self.click_targets.push((seg, Hit::Mode(*m)));
        }
        y += 52.0;

        // Route summary + steps.
        if let Some(route) = self.model.route.clone() {
            let big = geo::fmt_duration(route.duration_s);
            draw::text_w(scene, tr, t, &big, x, y, 26.0, theme.accent_primary, TextWeight::Bold);
            let dist = geo::fmt_distance(route.distance_m);
            let bw = draw::measure_w(tr, &big, 26.0);
            draw::text(scene, tr, t, &format!("· {dist}"), x + bw + 12.0, y + 8.0, 14.0, theme.text_dim);
            // Clear route link.
            let clr = Rect::new(right - 64.0, y + 2.0, right, y + 28.0);
            draw::text_right(scene, tr, t, "Clear", right - 4.0, y + 8.0, 12.0, theme.tokens.colors.semantic.danger);
            self.click_targets.push((clr, Hit::ClearRoute));
            y += 40.0;

            draw::line(scene, t, Point::new(x, y), Point::new(right, y), 1.0, c.border_standard);
            y += 12.0;

            for (i, step) in route.steps.iter().enumerate() {
                let rh = 46.0;
                // Glyph badge.
                let bc = Point::new(x + 16.0, y + rh / 2.0 - 4.0);
                draw::fill_circle(scene, t, bc, 13.0, draw::alpha(theme.accent_primary, 0.16));
                draw::text_center(scene, tr, t, step.maneuver.glyph(), bc.x - 13.0, bc.x + 13.0, bc.y - 9.0, 15.0, theme.accent_primary);
                draw::text_clip(scene, tr, t, &step.instruction, x + 38.0, y, 13.0, theme.text_primary, right - x - 80.0);
                if step.distance_m >= 1.0 {
                    draw::text_right(scene, tr, t, &geo::fmt_distance(step.distance_m), right - 4.0, y + 18.0, 11.0, theme.text_dim);
                }
                // Connector line between steps.
                if i + 1 < route.steps.len() {
                    draw::line(scene, t, Point::new(bc.x, bc.y + 13.0), Point::new(bc.x, y + rh + 9.0), 1.5, draw::alpha(theme.accent_primary, 0.4));
                }
                y += rh;
            }
        } else {
            draw::text(scene, tr, t, "Select a place, then tap Directions.", x, y, 13.0, theme.text_dim);
            y += 24.0;
        }
        y
    }

    /// Depart/arrive time picker for the Transit tab. Defaults to "leave now";
    /// a segmented Leave/Arrive toggle plus a ±15-minute stepper (and a "Now"
    /// reset) let the user anchor the query to a specific time.
    fn draw_transit_time_picker(
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

        // Leave / Arrive segmented control.
        let seg_rect = Rect::new(x, y, right, y + 34.0);
        draw::fill_rrect(scene, t, seg_rect, 9.0, c.surfaces.base);
        let modes = [(TransitTimeMode::Depart, "Leave"), (TransitTimeMode::Arrive, "Arrive")];
        let seg_w = seg_rect.width() / 2.0;
        for (i, (m, label)) in modes.iter().enumerate() {
            let sx = seg_rect.x0 + i as f64 * seg_w;
            let seg = Rect::new(sx, seg_rect.y0, sx + seg_w, seg_rect.y1);
            let active = tt.mode == *m;
            if active {
                draw::fill_rrect(scene, t, seg.inflate(-3.0, -3.0), 7.0, theme.accent_primary);
            }
            let fg = if active { Color::WHITE } else { theme.text_dim };
            draw::text_center(scene, tr, t, label, seg.x0, seg.x1, vmid(seg.y0, seg.height(), 13.0), 13.0, fg);
            self.click_targets.push((seg, Hit::TransitTimeMode(*m)));
        }
        y += 42.0;

        // Time stepper: [–] <clock> [+].
        let row_h = 34.0;
        let minus = Rect::new(x, y, x + 40.0, y + row_h);
        let plus = Rect::new(right - 40.0, y, right, y + row_h);
        draw::fill_rrect(scene, t, minus, 8.0, c.surfaces.base);
        draw::fill_rrect(scene, t, plus, 8.0, c.surfaces.base);
        draw::text_center(scene, tr, t, "–", minus.x0, minus.x1, vmid(minus.y0, row_h, 18.0), 18.0, theme.text_primary);
        draw::text_center(scene, tr, t, "+", plus.x0, plus.x1, vmid(plus.y0, row_h, 18.0), 18.0, theme.text_primary);
        let target = chrono::Local::now() + chrono::Duration::minutes(tt.offset_min as i64);
        let clock = target.format("%H:%M").to_string();
        let label = if tt.is_now() {
            "Now".to_string()
        } else {
            clock
        };
        draw::text_center(scene, tr, t, &label, minus.x1, plus.x0, vmid(y, row_h, 15.0), 15.0, theme.accent_primary);
        self.click_targets.push((minus, Hit::TransitTimeDelta(-15)));
        self.click_targets.push((plus, Hit::TransitTimeDelta(15)));
        y += row_h + 8.0;

        // "Now" reset chip when the anchor has moved off the default.
        if !tt.is_now() {
            let now_r = Rect::new(x, y, x + 72.0, y + 26.0);
            draw::fill_rrect(scene, t, now_r, 8.0, c.surfaces.raised);
            draw::stroke_rrect(scene, t, now_r, 8.0, 1.0, c.border_standard);
            draw::text_center(scene, tr, t, "Now", now_r.x0, now_r.x1, vmid(now_r.y0, 26.0, 12.0), 12.0, theme.text_primary);
            self.click_targets.push((now_r, Hit::TransitTimeNow));
            y += 34.0;
        }

        draw::line(scene, t, Point::new(x, y), Point::new(right, y), 1.0, c.border_standard);
        y + 12.0
    }

    fn draw_transit(
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
        // Time picker first so the depart/arrive anchor is set before reading
        // the departures below it.
        y = self.draw_transit_time_picker(scene, t, tr, theme, content, y);
        let to = if !self.model.dest_name.is_empty() {
            self.model.dest_name.clone()
        } else {
            self.model
                .selected_place()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "destination".to_string())
        };
        draw::text_w(scene, tr, t, &format!("Departures to {to}"), x, y, 14.0, theme.text_primary, TextWeight::Semibold);
        y += 26.0;

        if self.model.transit.is_empty() {
            let msg = if self.pending > 0 {
                "Finding departures…"
            } else if self.model.selected_place().is_none() {
                "Pick a destination to see departures."
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
            draw::fill_rrect(scene, t, jr, 10.0, if selected { draw::alpha(theme.accent_primary, 0.12) } else { c.surfaces.base });
            if selected {
                draw::stroke_rrect(scene, t, jr, 10.0, 1.5, theme.accent_primary);
            }
            // Header: dep–arr  ·  duration  ·  transfers.
            draw::text_w(scene, tr, t, &format!("{} – {}", j.dep, j.arr), x + 12.0, y + 10.0, 15.0, theme.text_primary, TextWeight::Bold);
            draw::text_right(scene, tr, t, &format!("{} min", j.duration_min), right - 12.0, y + 10.0, 14.0, theme.accent_primary);
            let xfer = if j.transfers == 0 { "direct".to_string() } else { format!("{} change{}", j.transfers, if j.transfers == 1 { "" } else { "s" }) };
            draw::text_right(scene, tr, t, &xfer, right - 12.0, y + 30.0, 11.0, theme.text_dim);

            // Leg pills.
            let mut px = x + 12.0;
            let py = y + 38.0;
            for (li, leg) in j.legs.iter().enumerate() {
                if li > 0 {
                    draw::text(scene, tr, t, "›", px, py + 2.0, 13.0, theme.text_dim);
                    px += 14.0;
                }
                let col = transit_color(theme, leg.mode);
                if leg.mode == TransitMode::Walk {
                    // Walk shown as a dashed segment with minutes.
                    let lbl = format!("walk {}m", leg.duration_min.max(1));
                    let w = draw::measure_w(tr, &lbl, 11.0) + 6.0;
                    draw::text(scene, tr, t, &lbl, px, py + 3.0, 11.0, theme.text_dim);
                    px += w + 4.0;
                } else {
                    let lbl = if leg.line.is_empty() { leg.mode.label().to_string() } else { leg.line.clone() };
                    let w = draw::measure_w(tr, &lbl, 12.0) + 16.0;
                    let pill = Rect::new(px, py, px + w, py + 22.0);
                    draw::fill_rrect(scene, t, pill, 6.0, col);
                    draw::text_center(scene, tr, t, &lbl, pill.x0, pill.x1, vmid(pill.y0, 22.0, 12.0), 12.0, Color::WHITE);
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
                    let lh = 34.0;
                    let col = transit_color(theme, leg.mode);
                    draw::line(scene, t, Point::new(x + 18.0, y + 4.0), Point::new(x + 18.0, y + lh), 2.0, col);
                    draw::fill_circle(scene, t, Point::new(x + 18.0, y + 8.0), 4.0, col);
                    let head = if leg.line.is_empty() {
                        format!("Walk · {} min", leg.duration_min.max(1))
                    } else {
                        format!("{} {} {}", leg.mode.label(), leg.line, leg.headsign)
                    };
                    draw::text_clip(scene, tr, t, head.trim(), x + 32.0, y, 12.0, theme.text_primary, right - x - 90.0);
                    let sub = if leg.line.is_empty() {
                        format!("to {}", leg.to_name)
                    } else {
                        format!("{} → {} · {} stops", leg.from_name, leg.to_name, leg.num_stops)
                    };
                    draw::text_clip(scene, tr, t, &sub, x + 32.0, y + 16.0, 11.0, theme.text_dim, right - x - 90.0);
                    draw::text_right(scene, tr, t, &format!("{}–{}", leg.dep, leg.arr), right - 8.0, y + 4.0, 11.0, theme.text_dim);
                    y += lh + 2.0;
                }
                y += 6.0;
            }
        }
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
        // Fold in any tiles / download progress the worker finished. This is a
        // non-blocking try_recv drain — it never waits on the network.
        let before = self.tiles.progress();
        self.tiles.drain();
        let after = self.tiles.progress();
        if before != after && !after.is_active() && after.total > 0 {
            // A download just finished — refresh the on-disk size readout.
            self.cache_bytes = self.tiles.cache_bytes();
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
        } else if self.dirty {
            MapsDoc::save(state, &self.model);
            self.dirty = false;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse = p;
                if self.dragging && self.left_down {
                    let dx = p.x - self.drag_last.x;
                    let dy = p.y - self.drag_last.y;
                    if dx.abs() > 0.5 || dy.abs() > 0.5 {
                        self.drag_moved = true;
                        self.pan_by(dx, dy);
                    }
                    self.drag_last = p;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let p = self.last_mouse;
                self.left_down = true;
                self.drag_moved = false;
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
                    if !matches!(hit, Hit::FocusSearch) {
                        self.search_focused = false;
                    }
                    // Clicking anything other than an endpoint field (or its
                    // dropdown) ends endpoint editing.
                    if !matches!(
                        hit,
                        Hit::EditOrigin | Hit::EditDest | Hit::PickOrigin(_) | Hit::PickDest(_)
                    ) {
                        self.edit_field = None;
                    }
                    self.dispatch(&hit);
                    consumed = true;
                    *handled = true;
                }
                if !consumed {
                    self.search_focused = false;
                    self.edit_field = None;
                    // Map interaction: marker select or begin pan.
                    if let Some(id) = self.marker_at(p) {
                        self.select_place(&id);
                        *handled = true;
                    } else {
                        self.dragging = true;
                        self.drag_last = p;
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                self.left_down = false;
                self.dragging = false;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64,
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
            WindowEvent::KeyboardInput { event: key_event, .. } => {
                if key_event.state != ElementState::Pressed {
                    return;
                }
                if self.handle_key(&key_event.logical_key, modifiers) {
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "MapsApp"
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
            || self.locating
            || self.location_rx.is_some()
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
            || self.search_focused
            || self.edit_field.is_some()
            || self.locating
            || self.location_rx.is_some()
            || self.tiles.has_inflight()
            || self.tiles.progress().is_active()
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
    fn handle_key(&mut self, key: &Key, modifiers: &winit::event::Modifiers) -> bool {
        // A Directions endpoint field (From / To) has keyboard focus: typed
        // characters geocode that endpoint, mirroring the top search field.
        if let Some(field) = self.edit_field {
            let origin = field == EndField::Origin;
            return match key {
                Key::Named(NamedKey::Escape) => {
                    self.edit_field = None;
                    self.mark_dirty();
                    true
                }
                Key::Named(NamedKey::Enter) => {
                    self.run_endpoint_search(origin);
                    true
                }
                Key::Named(NamedKey::Backspace) => {
                    self.endpoint_query_mut(origin).pop();
                    self.update_endpoint_local(origin);
                    self.mark_dirty();
                    true
                }
                Key::Named(NamedKey::Space) => {
                    self.endpoint_query_mut(origin).push(' ');
                    self.update_endpoint_local(origin);
                    self.mark_dirty();
                    true
                }
                Key::Character(s) => {
                    self.endpoint_query_mut(origin).push_str(s);
                    self.update_endpoint_local(origin);
                    self.mark_dirty();
                    true
                }
                _ => false,
            };
        }
        // Search field has keyboard focus.
        if self.search_focused {
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.search_focused = false;
                    true
                }
                Key::Named(NamedKey::Enter) => {
                    self.search_focused = false;
                    self.run_search();
                    true
                }
                Key::Named(NamedKey::Backspace) => {
                    self.model.query.pop();
                    self.mark_dirty();
                    true
                }
                Key::Named(NamedKey::Space) => {
                    self.model.query.push(' ');
                    self.mark_dirty();
                    true
                }
                Key::Character(s) => {
                    self.model.query.push_str(s);
                    self.mark_dirty();
                    true
                }
                _ => false,
            }
        } else {
            let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
            match key {
                Key::Character(s) if s.as_str() == "/" => {
                    self.search_focused = true;
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
                    self.fit_route();
                    true
                }
                Key::Character(s) if s.eq_ignore_ascii_case("s") && !ctrl => {
                    self.toggle_save_selected();
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
                Key::Character(s) if s.as_str() == "3" => {
                    self.set_panel(Panel::Transit);
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
                    if self.offline_open {
                        self.offline_open = false;
                    } else if self.overlay_mode() && self.nav_open {
                        self.nav_open = false;
                    } else {
                        self.model.selected = None;
                    }
                    self.mark_dirty();
                    true
                }
                Key::Character(s) if s.eq_ignore_ascii_case("m") => {
                    self.offline_open = !self.offline_open;
                    if self.offline_open {
                        self.cache_bytes = self.tiles.cache_bytes();
                    }
                    true
                }
                _ => false,
            }
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
        a
    }

    #[test]
    fn pan_changes_center() {
        let mut a = app_at(1280.0, 800.0);
        let before = a.model.center;
        a.pan_by(120.0, 0.0);
        assert!((a.model.center.lon - before.lon).abs() > 1e-6);
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
        assert!(a.model.recents.iter().any(|r| r.eq_ignore_ascii_case("viktualien")));
    }

    #[test]
    fn keyword_search_finds_cafe() {
        let mut a = app_at(1280.0, 800.0);
        a.model.query = "coffee".to_string();
        a.run_search();
        assert!(a.model.results.iter().any(|p| p.category == Category::Cafe));
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
        assert!(!a.click_targets.iter().any(|(_, h)| matches!(h, Hit::ToggleNav)));

        // Narrow: SAME instance → overlay drawer, menu button appears, and the
        // search bar reflows to make room for the leading menu button.
        a.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        assert!(a.overlay_mode());
        let mut scene2 = Scene::new();
        a.draw_app(&mut scene2, Affine::IDENTITY, &mut tr, &theme);
        assert!(a.menu_rect().is_some());
        assert!(a.click_targets.iter().any(|(_, h)| matches!(h, Hit::ToggleNav)));
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
        a.dispatch(&Hit::ToggleNav);
        assert!(a.panel_visible(), "menu opens the drawer");
        a.dispatch(&Hit::CloseNav);
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
            matches!(topmost(&a, sc), Some(Hit::FocusSearch)),
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
        assert!(ba.distance_m > 1.0, "swapped route must not be a degenerate 0 m");
    }

    /// Switching to the Transit tab for a selected place must actually start
    /// the departures lookup, not merely flip the view — otherwise the panel
    /// renders a false "no itineraries found" for a lookup that never ran.
    #[test]
    fn transit_tab_triggers_fetch_for_selected_place() {
        let mut a = app_at(1280.0, 800.0);
        // Select a place other than the sample's default so the stale sample
        // departures are dropped (destination-specific).
        a.select_place("hofgarten");
        assert!(a.model.transit.is_empty(), "selecting a new place drops stale departures");
        a.model.panel = Panel::Explore;

        a.set_panel(Panel::Transit);
        assert_eq!(a.model.panel, Panel::Transit);
        assert!(a.pending > 0, "transit tab must kick off a departures lookup");
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
        assert_eq!(a.model.origin, Some(picked.pos), "origin coord set from the result");
        assert_eq!(a.model.origin_name, picked.name, "origin label set from the result");
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
                results: Ok(vec![p.clone()]),
            })
            .unwrap();
        a.drain_net();
        assert!(
            a.dest_results.iter().any(|r| r.id == "osm-n-1"),
            "geocoded result populates the To dropdown"
        );
        assert!(a.model.place("osm-n-1").is_some(), "result upserted so its marker shows");
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
        assert_eq!(a.model.origin, Some(LatLon::new(48.11, 11.53)), "origin set from the fix");
        let route = a.model.route.clone().expect("route re-planned from the fix");
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
        assert_eq!(a.model.origin, before, "a failed fix must not fabricate an origin");
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
        assert!(ts.contains('T') && ts.len() >= 19, "rfc3339 timestamp, got {ts}");

        let url = net::transit_url(LatLon::new(48.10, 11.55), LatLon::new(48.14, 11.58), &q);
        assert!(url.contains("arriveBy=true"), "{url}");
        assert!(url.contains("&time="), "picked time must reach the query: {url}");
    }

    /// Stepping the time anchor (via the real dispatch path) shifts the offset
    /// and "Now" resets it — proving the picker controls are wired.
    #[test]
    fn transit_time_controls_adjust_the_anchor() {
        let mut a = app_at(1280.0, 800.0);
        a.dispatch(&Hit::TransitTimeDelta(15));
        a.dispatch(&Hit::TransitTimeDelta(15));
        assert_eq!(a.model.transit_time.offset_min, 30, "two +15 steps advance 30 min");
        // Never steps below now.
        for _ in 0..5 {
            a.dispatch(&Hit::TransitTimeDelta(-15));
        }
        assert_eq!(a.model.transit_time.offset_min, 0, "offset floors at now");
        a.dispatch(&Hit::TransitTimeMode(TransitTimeMode::Arrive));
        a.dispatch(&Hit::TransitTimeDelta(15));
        a.dispatch(&Hit::TransitTimeNow);
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
    fn feed(a: &mut MapsApp, ev: WindowEvent) -> bool {
        let modifiers = winit::event::Modifiers::default();
        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        a.handle_event(
            &ev, 1.0, &modifiers, &mut state, &root, &mut handled, &mut focus, node, &taffy,
        );
        handled
    }

    fn move_to(a: &mut MapsApp, x: f64, y: f64) -> bool {
        feed(
            a,
            WindowEvent::CursorMoved {
                device_id: dummy_device_id(),
                position: winit::dpi::PhysicalPosition::new(x, y),
            },
        )
    }

    fn press_left(a: &mut MapsApp) -> bool {
        feed(
            a,
            WindowEvent::MouseInput {
                device_id: dummy_device_id(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        )
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
            WindowEvent::MouseWheel {
                device_id: dummy_device_id(),
                delta: MouseScrollDelta::LineDelta(0.0, 1.0),
                phase: winit::event::TouchPhase::Moved,
            },
        );
        assert!(handled, "wheel over the map must be handled");
        assert!(a.model.zoom > before, "scroll up should zoom in");
    }

    /// Clicking the Directions tab through the real event path (draw records
    /// the tab rects; a press hit-tests them) switches the active panel.
    #[test]
    fn tab_click_switches_panel_via_handle_event() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut a = app_at(1280.0, 720.0);
        a.model.panel = Panel::Explore;
        // Draw once so the tab hot-regions are recorded.
        let mut scene = Scene::new();
        a.draw_app(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        let tab = a
            .click_targets
            .iter()
            .find(|(_, h)| matches!(h, Hit::Tab(Panel::Directions)))
            .map(|(r, _)| r.center())
            .expect("Directions tab hot-region recorded");

        move_to(&mut a, tab.x, tab.y);
        let handled = press_left(&mut a);
        assert!(handled, "tab click must be handled");
        assert_eq!(a.model.panel, Panel::Directions, "panel switched to Directions");
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
        press_left(&mut a);
        // Move the cursor: with the button down this pans, not hovers.
        move_to(&mut a, 1040.0, 372.0);
        assert!(
            (a.model.center.lon - before.lon).abs() > 1e-6
                || (a.model.center.lat - before.lat).abs() > 1e-6,
            "drag should move the map centre"
        );
    }
}
