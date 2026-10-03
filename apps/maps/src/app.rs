//! Root widget for the Maps app.
//!
//! `MapsApp` owns the in-memory [`MapModel`] and the static vector [`Basemap`].
//! [`crate::doc::MapsDoc`] synchronizes its explicitly durable resources; the
//! rest is per-window presentation state. It
//! is a single full-window custom widget: the map is full-bleed and the
//! chrome (the places panel, the control column, the place card) floats on
//! top as floating panels built from the toolkit's faces. Interaction —
//! pan, zoom-to-cursor, marker hit-testing, panel buttons — is handled
//! directly in `handle_event`; finding places is the platform search, which
//! [`crate::place_search`] answers.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use ui::time::Instant;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::TextKind;
use ui::chrome::SelectionState;
use ui::format::bytes as format_bytes;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::{
    ActionFailure, ActionOutput, InvocationId, InvocationUpdate, OperationKind, Resource,
};
use ui::tokens::spacing::Spacing;
use ui::tokens::{Metrics, TextRole};
use ui::widget::{
    BadgeFace, Button, ButtonFace, ButtonRole, CardFace, ChipStyle, ContextMenu, ControlSize,
    EmptyState, EmptyStateContent, Interaction, ListRowFace, MenuItem, PropertyListFace,
    PropertyRow, RowLeading, SegmentedFace, SegmentedStyle, ToggleGroup, Tone,
};
use ui::widget::{FloatingSurfaceProvider, ResourceOperationStart, Widget};

use crate::actions::{MAPS_ACTION_TYPE, MapsAction};
use crate::canvas::{self, MapPalette, MapView};
use crate::doc::MapsDoc;
use crate::draw;
use crate::endpoint_editor::{Endpoint, EndpointEditor, EndpointEffect, EndpointFrame};
use crate::geo::{self, Projection};
use crate::map_controls::{ColumnMetrics, MapControlAction, MapControls};
use crate::map_info_sheet::{MapInfo, MapInfoSheet};
use crate::model::{LatLon, MapModel, Panel, Place, TransitMode, TransitTimeMode, TravelMode};
use crate::net::{self, NetMsg};
use crate::offline_panel::{OFFLINE_AREAS_REFUSED, OfflineAction, OfflineFrame, OfflinePanel};
use crate::place_search::{LocalPlaces, PlaceHit, PlaceSearch};
use crate::sample::{self, Basemap};
use crate::tiles::{GeoBounds, TileFault, TileId, TileLayer, TileStyle};
use crate::traffic::{Congestion, TrafficData};
use ui::location::{self, LocationChannel, LocationOutcome};

mod interaction;

mod resource_surface;

// Geometry comes from the theme: the window margin is `s4`, the gap between
// floating elements `s3`, the panel the list-panel width, the controls the
// Regular control height — read through `MapsApp::margin` & co. from the
// cached theme tokens.

/// One semantic live-activity identity for the current turn-by-turn session.
/// Rerouting changes its content, not the surface the user positioned.
const NAVIGATION_FLOAT_ID: &str = "maps.active-navigation";
/// Idle gap after the last pan/zoom before refetching viewport POIs (Overpass).
const POI_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(450);
/// Only fetch POIs at city-block scale and closer — a whole-country viewport
/// would return an unbounded, meaningless marker set.
const POI_MIN_ZOOM: f64 = 14.0;
/// Cap on POI markers drawn so a dense area stays legible.
const POI_MAX: usize = 140;
/// How many nearby POIs the Explore panel lists (the closest to the anchor).
const NEARBY_MAX: usize = 10;
/// A clickable hot-region recorded during draw and hit-tested on click.
#[derive(Clone, Debug)]
enum Hit {
    /// Swallow the click (panel chrome) — don't pan the map.
    Consume,
    /// Open the platform search over this app.
    OpenSearch,
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
    /// Search for a recent query again, in the platform search.
    Recent(String),
    /// Clear collaborative recent-search history through its canonical action.
    ClearRecents,
    /// Toggle the overlay drawer (narrow layouts only).
    ToggleNav,
    /// Dismiss the overlay drawer (scrim tap).
    CloseNav,
    Directions,
    Transit,
    ToggleSave,
    ClearRoute,
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
}

/// One immediately-painted control exposed to assistive technology.  `target`
/// is deliberately derived from a durable command/id rather than its frame
/// position: result ordering and responsive reflow must not change what an
/// agent or screen reader has addressed.
struct AccessibilityTarget {
    rect: Rect,
    label: String,
    target: String,
    role: ui::a11y::Role,
    state: ui::a11y::AccessState,
}

fn stable_access_key(id: &str) -> u64 {
    // FNV-1a keeps accessibility child identity stable across responsive
    // reordering without depending on a process-random hash seed.
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, id.as_bytes());
    std::hash::Hasher::finish(&hasher)
}

/// The app's end of its network fetches: a result is queued and the app woken
/// to take it, so nothing polls for it frame by frame while a slow or failing
/// service keeps a request open.
#[derive(Clone)]
struct NetSender {
    tx: Sender<NetMsg>,
    wake: ui::wake::WakeHandle,
}

impl NetSender {
    fn send(&self, msg: NetMsg) -> Result<(), std::sync::mpsc::SendError<NetMsg>> {
        self.tx.send(msg)?;
        self.wake.wake();
        Ok(())
    }
}

/// Run a network fetch as a task on the app runtime; its result arrives on
/// the app's channel.
fn spawn_net(fetch: impl Future<Output = ()> + Send + 'static) {
    match ui::rt::try_app() {
        Ok(runtime) => drop(runtime.spawn(fetch)),
        Err(error) => tracing::warn!("maps: no app runtime for a network fetch: {error}"),
    }
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
    actions: crate::resource::ActionSink,

    // interaction state (render/UI caches only — app state lives in `model`)
    last_mouse: Point,
    /// Floating zoom/location/traffic stack. Owns geometry, drawing,
    /// hit-testing, traffic UI state, and shared tooltip timing.
    map_controls: MapControls,
    /// The platform search's view of the map: the provider it asks, and the
    /// geocoder queries it is waiting on.
    place_search: PlaceSearch,
    /// Opens the platform search from the painted search buttons.
    search_opener: ui::search::SearchOpener,
    /// The History button, in the panel's header (the top row when narrow),
    /// so the window draws no History bar over the map.
    history: Button,
    /// Where the History button was drawn this frame.
    history_rect: Rect,
    /// Transient status (searching, routing, a download, a failure).
    status_bubble: ui::StatusBubble,
    /// Directions From/To fields, dropdown, focus, debounce and hit routing.
    endpoints: EndpointEditor,
    left_down: bool,
    dragging: bool,
    drag_last: Point,
    drag_moved: bool,
    click_targets: Vec<(Rect, Hit)>,
    /// The renderer's text scale, captured at the top of each paint so the
    /// panel's hand-laid rows size and centre their text from the same lines
    /// the renderer draws.
    text_scale: f32,
    /// The theme's spacing and density metrics, cached in `build_layout` so
    /// the renderer-less geometry reads the same tokens the painters do.
    spacing: Spacing,
    metrics: Metrics,
    /// Frame the route (or journey) once the window has its size: a staged
    /// or restored route fits the map beside the panel and the turn card.
    pending_fit: bool,
    /// The turn card's intrinsic size in the current theme.
    nav_card_size: vello::kurbo::Size,
    /// Where each block of the panel's content ends (rows, cards, groups),
    /// measured from the content's top: the panel clips and scrolls on these
    /// so no row shows as a sliver.
    panel_edges: Vec<f64>,
    /// The y of the panel content's top this frame, scroll included.
    panel_origin: f64,
    scroll: f64,
    content_h: f64,
    content_view_h: f64,

    // CRDT bridge
    loaded: bool,

    // networking
    net_tx: NetSender,
    net_rx: Receiver<NetMsg>,
    pending: u32,
    /// Whether background network/portal work may be spawned. False for the
    /// offline `--shots`/test app so it never touches the wire or D-Bus.
    online: bool,

    // device location (portal / GeoClue), off the UI thread
    location_rx: Option<LocationChannel>,
    /// True while a location fix is being acquired (drives the "Locating…" UI).
    locating: bool,
    /// Broker identity for a location request admitted through the canonical
    /// resource/action surface. The portal result, rather than admission, is
    /// the operation's terminal answer.
    location_invocation: Option<InvocationId>,
    location_terminal: Option<InvocationUpdate>,

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
    /// Attribution/licence modal. Owns state, drawing geometry, and hit-testing.
    info_sheet: MapInfoSheet,

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
    /// One-shot request emitted by the overflow action and drained by the
    /// shared shell floating-surface host.
    pending_floating: Option<ui::floating_surface::FloatingSurfaceRequest>,
    /// One geometry/drag state for the attached and shell-presented live turn
    /// card. Ownership changes; the Maps widget and route data never do.
    navigation_placement: ui::floating_surface::FloatingPlacement,
    navigation_drag_handle: Rect,
    /// Compact, always-available end-navigation action in the live turn card.
    /// Kept separate from the title-bar drag target so a quick stop can never
    /// accidentally begin an app -> shell ownership handoff.
    navigation_end_rect: Rect,
    navigation_end_hovered: bool,
    /// Effective accessibility motion policy supplied by the host. Floating
    /// handoff geometry already draws settled under reduced motion; retaining
    /// this bit also stops its decorative timer from requesting idle frames.
    motion_reduced: bool,
    /// Authoritative combined-output bounds supplied by the shell. Standalone
    /// app/test hosts leave this empty and fall back to the app rect.
    floating_surface_bounds: Rect,
}

impl Default for MapsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl MapsApp {
    /// Compact live-navigation surface for the shell floating host. The source
    /// stays app-owned so route changes immediately update an already-detached
    /// overlay instead of snapshotting stale directions.
    pub fn active_navigation_request(
        &self,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.active_navigation_request_from(self.navigation_rect())
    }

    fn active_navigation_request_from(
        &self,
        source_region: Rect,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.model
            .route
            .as_ref()
            .map(|_route| ui::floating_surface::FloatingSurfaceRequest {
                id: NAVIGATION_FLOAT_ID.into(),
                title: "Active navigation".into(),
                role: ui::floating_surface::FloatingRole::Navigation,
                source: ui::floating_surface::FloatingSource::AppElement {
                    app_id: "so.tilo.maps".into(),
                    element_id: "active-navigation".into(),
                    region: source_region,
                },
                preferred_size: self.nav_card_size,
                aspect_ratio: None,
                initial_rect: Some(self.navigation_rect()),
                presentation_rect: Some(self.navigation_rect().inflate(8.0, 8.0)),
                tracking_source: None,
            })
    }

    fn navigation_rect(&self) -> Rect {
        self.navigation_placement
            .rect(self.rect, self.desktop_floating_bounds())
    }

    /// The window margin every floating element keeps — the theme's `s4`.
    fn margin(&self) -> f64 {
        f64::from(self.spacing.s4)
    }

    /// The gap between neighbouring floating elements — the theme's `s3`,
    /// the sibling step (HIG §7b), so the panel, the controls and the cards
    /// share one rhythm.
    fn gap(&self) -> f64 {
        f64::from(self.spacing.s3)
    }

    /// The control column's geometry (the zoom, locate and traffic buttons).
    fn controls(&self) -> ColumnMetrics {
        self.map_controls.metrics()
    }

    /// The area the floating map controls (zoom, locate, traffic) lay
    /// themselves out in. Normally the whole window; while the navigation
    /// card floats over the top-right corner, the cluster starts beneath it
    /// so the controls stay visible and reachable rather than being covered.
    /// Paint, pointer hit-testing and the accessibility rects all read the
    /// controls' geometry from this one viewport.
    fn map_controls_viewport(&self) -> Rect {
        let viewport = self.rect;
        if !self.navigation_surface_visible_in_app() {
            return viewport;
        }
        let margin = self.margin();
        let gap = self.gap();
        let controls = self.controls();
        let nav = self.navigation_rect();
        let column_h = margin + controls.column_height();
        let column = Rect::new(
            self.rect.x1 - margin - controls.size,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + column_h,
        );
        if nav.intersect(column).area() <= 0.0 {
            return viewport;
        }
        // Beneath the card when the whole column still clears the ⋯ overflow
        // button at the bottom; otherwise beside the card's leading edge when
        // that clears the docked side panel; otherwise the corner stays as it
        // was (the card covers it) rather than laying the cluster out off the
        // bottom of a short window or under the overflow button.
        let floor = self.overflow_rect().y0 - gap;
        if nav.y1 < self.rect.y1 && nav.y1 + column_h <= floor {
            return Rect::new(viewport.x0, nav.y1, viewport.x1, viewport.y1);
        }
        let panel_right = if self.panel_docked_expanded() {
            self.panel_rect().x1 + gap
        } else {
            self.rect.x0 + margin
        };
        if nav.x0 - gap - controls.size >= panel_right {
            return Rect::new(viewport.x0, viewport.y0, nav.x0 - gap + margin, viewport.y1);
        }
        viewport
    }

    /// The route card is useful over the map, but duplicates the route summary
    /// and next steps inside the narrow navigation drawer. Hiding it while that
    /// drawer is open avoids covering the fields the user is actively editing.
    fn navigation_surface_visible_in_app(&self) -> bool {
        self.model.route.is_some()
            && !self.navigation_placement.is_return_pending()
            && matches!(
                self.navigation_placement.attachment(),
                ui::floating_surface::Attachment::App { .. }
            )
            && !(self.overlay_mode() && self.nav_open)
    }

    fn desktop_floating_bounds(&self) -> Rect {
        if self.floating_surface_bounds.area() > 0.0 {
            self.floating_surface_bounds
        } else {
            self.rect
        }
    }

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
        let (tx, net_rx) = std::sync::mpsc::channel();
        let net_tx = NetSender {
            tx,
            wake: ui::wake::WakeHandle::new(),
        };
        let cache_bytes = tiles.cache_bytes();
        let nav_card_size = navigation_card_size(&Theme::dark());
        let place_search = PlaceSearch::default();
        place_search.set_reachable(online);
        Self {
            model,
            basemap,
            rect: Rect::ZERO,
            node: None,
            actions: crate::resource::ActionSink::default(),
            last_mouse: Point::ZERO,
            map_controls: MapControls::default(),
            place_search,
            search_opener: ui::search::SearchOpener::new(),
            history: {
                let mut history = ui::widget::history_button();
                history.set_compact_toolbar_presentation(true);
                history
            },
            history_rect: Rect::ZERO,
            status_bubble: ui::StatusBubble::new(),
            endpoints: EndpointEditor::new(),
            left_down: false,
            dragging: false,
            drag_last: Point::ZERO,
            drag_moved: false,
            click_targets: Vec::new(),
            text_scale: 1.0,
            spacing: Spacing::default(),
            metrics: Metrics::BASE,
            pending_fit: false,
            nav_card_size,
            panel_edges: Vec::new(),
            panel_origin: 0.0,
            scroll: 0.0,
            content_h: 0.0,
            content_view_h: 0.0,
            loaded: false,
            net_tx,
            net_rx,
            pending: 0,
            online,
            location_rx: None,
            locating: false,
            location_invocation: None,
            location_terminal: None,
            tiles,
            overflow: {
                let mut m = ContextMenu::new();
                // Indices are mapped in `dispatch` via `take_selection`.
                m.push(MenuItem::action("Attribution", || {}));
                m.push(MenuItem::action("Offline maps", || {}));
                m.push(MenuItem::action("Licenses", || {}));
                m
            },
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

    /// Screenshot-only hook: force the overlay drawer open/closed so both the
    /// map-first and panel-open states can be captured headlessly.
    pub fn set_nav_open(&mut self, open: bool) {
        self.nav_open = open;
    }

    /// Screenshot-only hook for the shared attribution dialog.
    pub fn set_demo_attribution(&mut self) {
        self.info_sheet.open(MapInfo::Attribution);
    }

    /// Screenshot-only hook for an honest recoverable network failure. This
    /// keeps visual regression review from treating loading and error states as
    /// interchangeable, while the production app still obtains this text only
    /// from a real worker failure.
    pub fn set_demo_error(&mut self) {
        self.model.status = "Routing failed, so the route shown is an estimate".into();
        let now = ui::time::now();
        self.status_bubble.show(&self.model.status, now);
        self.status_bubble.set_hover(true, now);
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
        self.model.origin_query = "hauptbahnhof".to_string();
        self.endpoints.begin_edit(Endpoint::Origin);
        self.endpoints
            .sync_queries(&self.model.origin_query, &self.model.dest_query);
        self.endpoints
            .set_results(Endpoint::Origin, self.local_matches("hauptbahnhof"));
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
            .set_demo(47_542_000, "Munich", sample::download_regions());
        self.offline_panel.select_result(0);
    }

    /// Screenshot-only hook: open the ⋯ overflow menu so its Attribution /
    /// Offline maps / Licenses options render expanded over the map. Seeds the
    /// rect (the shot renders at the default viewport) so the menu anchors above
    /// its bottom-right button before layout runs.
    pub fn set_demo_overflow(&mut self) {
        if self.rect.area() <= 0.0 {
            self.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        }
        self.open_overflow();
    }

    /// Open the overflow menu above the button, its trailing edge on the
    /// button's, a token gap above it. Opened once to measure, then
    /// re-anchored so it grows up from the corner.
    fn open_overflow(&mut self) {
        let b = self.overflow_rect();
        self.overflow.open_at([b.x1 as f32, b.y0 as f32]);
        let mr = self.overflow.get_rect();
        self.overflow.open_at([
            (b.x1 - mr.width()) as f32,
            (b.y0 - mr.height() - f64::from(self.spacing.s2)) as f32,
        ]);
    }

    /// Screenshot-only hook: the panel after a committed search ("café"):
    /// its results listed nearest first, the first one selected. Offline
    /// local data only — no network.
    pub fn set_demo_search(&mut self) {
        self.model.panel = Panel::Explore;
        self.model.route = None;
        self.model.query = "café".to_string();
        let mut results = self.ranked_matches("café");
        results.extend(self.ranked_matches("museum"));
        self.model.selected = results.first().map(|place| place.id.clone());
        self.model.results = results;
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

    /// Stage a capture: the bundled sample city, drawn offline (no tile
    /// worker, no network), with the view state the scenario shows. The
    /// sample's places, route and journeys arrive through the scenario's
    /// actions (`MapsShell::scenario`); this sets what is this window's own —
    /// the camera, the panel, the selection, the open sheet.
    pub(crate) fn stage_capture(&mut self, scenario: crate::resource::Scenario) {
        use crate::resource::Scenario;
        self.online = false;
        self.place_search.set_reachable(false);
        self.tiles = TileLayer::offline();
        self.locating = false;
        self.location_rx = None;
        if scenario != Scenario::Empty {
            self.basemap = sample::basemap();
        }
        let sample = sample::default_model();
        self.model.center = sample.center;
        self.model.zoom = sample.zoom;
        self.model.selected.clone_from(&sample.selected);
        self.model.panel = Panel::Directions;
        self.nav_open = true;
        match scenario {
            Scenario::Directions | Scenario::Error | Scenario::Collapsed => {
                self.pending_fit = true;
                if scenario == Scenario::Error {
                    self.set_demo_error();
                }
                if scenario == Scenario::Collapsed {
                    self.panel_collapsed = true;
                }
            }
            Scenario::EditOrigin => self.set_demo_edit_origin(),
            Scenario::Explore => self.model.panel = Panel::Explore,
            Scenario::Search => self.set_demo_search(),
            Scenario::Pois => {
                self.set_demo_pois();
                // A narrow window shows the place card over the map, the
                // place centred above it.
                if self.overlay_mode()
                    && let Some(id) = self.selected_poi.clone()
                {
                    self.select_poi_from_list(&id);
                }
            }
            Scenario::Navigation => {
                self.pending_fit = true;
                self.nav_open = false;
            }
            Scenario::Transit | Scenario::TransitArrive => {
                self.model.panel = Panel::Directions;
                self.pending_fit = true;
            }
            Scenario::Traffic => {
                self.model.panel = Panel::Explore;
                self.set_demo_traffic();
            }
            Scenario::Offline => self.set_demo_offline(),
            Scenario::Menu => {
                self.model.panel = Panel::Explore;
                self.set_demo_overflow();
            }
            Scenario::Attribution => {
                self.model.panel = Panel::Explore;
                self.set_demo_attribution();
            }
            Scenario::Empty => {
                self.model.panel = Panel::Explore;
                self.model.selected = None;
            }
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
    /// instead of the shared default. Identical production path to `new` —
    /// the only difference is a redirectable cache — so a live integration
    /// test can start from a clean cache and inspect exactly the PNGs the
    /// worker writes. Not used by the live app or `--shots`.
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
        // The theme's compact breakpoint; scaling breakpoints with the text
        // is the shell family's policy (NavShell / SidebarDrawer), not an
        // app's — this hand-rolled drawer reads the token and leaves it.
        self.rect.width() < f64::from(self.metrics.compact_breakpoint)
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

    /// Panel width for the current window: the list-panel width when docked,
    /// a drawer that stops short of the control column when narrow.
    fn panel_w(&self) -> f64 {
        // The panel carries text, so its width grows with the text scale
        // (never shrinks): a 2× row keeps the character budget a 1× row has
        // instead of eliding inside a 1× column. Bounds are combined with
        // `max`/`min` rather than `clamp`: a degenerate rect can put them out
        // of order, and `clamp` panics where these just pick the finite one.
        let grow = f64::from(self.text_scale.clamp(1.0, 4.0));
        let list = f64::from(self.metrics.panel_width_list);
        let avail = (self.rect.width() - 2.0 * self.margin()).max(1.0);
        if self.overlay_mode() {
            // The drawer never slides under the control column.
            let beside_controls = (avail - self.controls().size - self.gap()).max(1.0);
            (list * grow).min(beside_controls)
        } else {
            // Docked: never more than 60 % so the map keeps room beside it.
            (list * grow).min(avail).min(self.rect.width() * 0.6)
        }
    }

    /// The narrow window's top row: the drawer toggle — narrow layouts only.
    fn menu_rect(&self) -> Option<Rect> {
        if !self.overlay_mode() {
            return None;
        }
        let side = self.controls().size;
        let x0 = self.rect.x0 + self.margin();
        let y0 = self.rect.y0 + self.margin();
        Some(Rect::new(x0, y0, x0 + side, y0 + side))
    }

    /// The narrow window's search button, beside the drawer toggle.
    fn search_button_rect(&self) -> Option<Rect> {
        let menu = self.menu_rect()?;
        let x0 = menu.x1 + f64::from(self.spacing.s2);
        Some(Rect::new(x0, menu.y0, x0 + menu.width(), menu.y1))
    }

    fn panel_rect(&self) -> Rect {
        let x0 = self.rect.x0 + self.margin();
        let top = self
            .menu_rect()
            .map_or_else(|| self.rect.y0 + self.margin(), |menu| menu.y1 + self.gap());
        Rect::new(x0, top, x0 + self.panel_w(), self.rect.y1 - self.margin())
    }

    /// The overflow button (attribution, offline maps, licences) in the
    /// bottom-trailing corner.
    fn overflow_rect(&self) -> Rect {
        let side = self.controls().size;
        let x1 = self.rect.x1 - self.margin();
        let y1 = self.rect.y1 - self.margin();
        Rect::new(x1 - side, y1 - side, x1, y1)
    }

    /// The leading edge of the map the floating cards keep to: beside the
    /// docked panel, or the window margin.
    fn map_left_edge(&self) -> f64 {
        if self.panel_docked_expanded() {
            self.panel_rect().x1 + self.gap()
        } else {
            self.rect.x0 + self.margin()
        }
    }

    /// The part of the map nothing floats over: beside the docked panel, clear
    /// of the control column and the footer row, and under the turn card when
    /// it spans the top. Fitting a route frames it here, and the place card
    /// and the loading text centre on it.
    fn visible_map_area(&self) -> Rect {
        let gap = self.gap();
        let x0 = self.map_left_edge();
        let mut x1 = self.rect.x1 - self.margin() - self.controls().size - gap;
        let mut y0 = self
            .menu_rect()
            .map_or_else(|| self.rect.y0 + self.margin(), |menu| menu.y1 + gap);
        if self.navigation_surface_visible_in_app() {
            let nav = self.navigation_rect();
            // A card across the top pushes the area down; one in the corner
            // beside a wide map narrows it instead.
            if nav.x0 - gap - x0 >= (x1 - x0) * 0.5 {
                x1 = x1.min(nav.x0 - gap);
            } else {
                y0 = y0.max(nav.y1 + gap);
            }
        }
        let y1 = self.overflow_rect().y0 - gap;
        Rect::new(x0, y0, x1.max(x0 + 1.0), y1.max(y0 + 1.0))
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
        // The footprint itself is the stable resource identity. Two peers that
        // independently download the same area therefore address one row after
        // merge instead of creating timestamp-named duplicates.
        let name = name.unwrap_or_else(|| self.region_name(bounds));
        let id = format!(
            "region:{:.6}:{:.6}:{:.6}:{:.6}:z{z0}-{z1}",
            bounds.min.lat, bounds.min.lon, bounds.max.lat, bounds.max.lon,
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
        self.model.status = format!("Removed {}, freeing {}", region.name, format_bytes(freed));
    }

    // --- actions -----------------------------------------------------------
    /// Dispatch one collaborative [`MapsAction`] against the shared document.
    ///
    /// The host applies the same canonical binding used by resources and
    /// on-screen controls. Local presentation state is never pre-flushed.
    fn has_durable_map(&self) -> bool {
        !self.model.places.is_empty()
            || !self.model.regions.is_empty()
            || !self.model.recents.is_empty()
            || !self.model.transit.is_empty()
            || self.model.route.is_some()
    }

    /// Publish a sample or offline map through the existing map actions.
    /// Live empty maps stay unpublished until a later action.
    fn publish_durable(&mut self, doc: &mut AutoCommit) {
        let places = self.model.places.clone();
        let recents = self.model.recents.clone();
        let regions = self.model.regions.clone();
        let route = self.model.route.clone();
        let transit = self.model.transit.clone();
        let mode = self.model.mode;
        let transit_time = self.model.transit_time;
        let origin = self.model.origin;
        let origin_name = self.model.origin_name.clone();
        let dest = self.model.dest;
        let dest_name = self.model.dest_name.clone();
        if !places.is_empty() {
            self.dispatch_map(doc, &MapsAction::UpsertPlaces { places });
        }
        for query in recents.into_iter().rev() {
            self.dispatch_map(doc, &MapsAction::CommitSearch { query });
        }
        self.dispatch_map(doc, &MapsAction::SetTravelMode { mode });
        self.dispatch_map(
            doc,
            &MapsAction::SetTransitTime {
                value: transit_time,
            },
        );
        for region in regions {
            self.dispatch_map(doc, &MapsAction::AddRegion(region));
        }
        if let Some(route) = route {
            self.dispatch_map(
                doc,
                &MapsAction::SetRoute {
                    route,
                    origin,
                    origin_name: origin_name.clone(),
                    dest,
                    dest_name: dest_name.clone(),
                    mode,
                },
            );
        }
        if !transit.is_empty() {
            self.dispatch_map(
                doc,
                &MapsAction::SetTransit {
                    transit,
                    origin,
                    origin_name,
                    dest,
                    dest_name,
                },
            );
        }
    }

    fn dispatch_map(&mut self, doc: &mut AutoCommit, action: &MapsAction) {
        self.actions.dispatch_with_context(
            doc,
            MAPS_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.maps", "Maps", action),
        );
    }

    fn perform(&mut self, doc: &mut AutoCommit, action: &MapsAction) {
        // Test/standalone callers may invoke an action before the widget's
        // first sync. Seed once only; ordinary interactions never mirror local
        // presentation back into an existing document.
        if MapsDoc::load(doc).is_none() && !self.actions.is_hosted() && self.has_durable_map() {
            self.publish_durable(doc);
        }
        self.actions.dispatch_with_context(
            doc,
            MAPS_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.maps", "Maps", action),
        );
        MapsDoc::apply_durable(doc, &mut self.model);
    }

    /// Show the places, routes and regions the framework resource already holds.
    pub(crate) fn adopt_resource_state(&mut self, state: &MapModel) {
        self.model.places.clone_from(&state.places);
        self.model.recents.clone_from(&state.recents);
        self.model.route.clone_from(&state.route);
        self.model.transit.clone_from(&state.transit);
        self.model.mode = state.mode;
        self.model.origin = state.origin;
        self.model.origin_name.clone_from(&state.origin_name);
        self.model.dest = state.dest;
        self.model.dest_name.clone_from(&state.dest_name);
        self.model.transit_time = state.transit_time;
        self.model.regions.clone_from(&state.regions);
        self.loaded = true;
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Maps>,
    ) {
        self.actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// Test helper: stage ranked search hits that are not document state.
    #[cfg(test)]
    pub(crate) fn seed_search_results_for_test(&mut self, results: Vec<crate::model::Place>) {
        self.model.results = results;
    }

    /// Toggle the traffic overlay. Turning it on triggers a refresh so the
    /// current congestion picture is loaded (or the honest empty state is set).
    fn set_traffic(&mut self, on: bool) {
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
        self.endpoints.dismiss();
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
                .filter(|p| crate::place_search::keyword_match(&needle, p.category))
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
                || crate::place_search::keyword_match(&needle, poi.category);
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

    /// Rank geocoded results by relevance to `query` with distance to the search
    /// anchor as the tie-breaker, and collapse co-located duplicates. This is
    /// the fix for far-away namesakes (a 511 km hit) outranking the near place
    /// the user meant — see [`crate::rank`].
    fn rank_results(&self, places: &mut Vec<crate::model::Place>, query: &str) {
        crate::rank::rank(places, self.search_anchor(), query);
    }

    /// Instant offline matches for `q`, ranked by relevance and proximity —
    /// what a committed search lists before any network reply lands.
    fn ranked_matches(&self, q: &str) -> Vec<crate::model::Place> {
        let mut v = self.local_matches(q);
        self.rank_results(&mut v, q);
        v
    }

    /// Whether the floating endpoint (From/To) suggestions are on screen.
    #[cfg(test)]
    fn endpoint_overlay_active(&self) -> bool {
        matches!(self.model.panel, Panel::Directions) && self.endpoints.overlay_active()
    }

    /// What the platform search finds at once: the places the map knows,
    /// the points of interest around the view, and the recent searches an
    /// empty query lists.
    fn local_places(&self) -> LocalPlaces {
        LocalPlaces {
            places: self.model.places.clone(),
            pois: self
                .pois
                .iter()
                .map(|poi| Place::new(&poi.id, &poi.name, poi.category, &poi.address, poi.pos))
                .collect(),
            recents: self.model.recents.clone(),
            anchor: self.search_anchor(),
        }
    }

    /// Send the newest query the platform search is waiting on to the
    /// geocoder, once. The reply answers the palette's pending page.
    fn pump_place_search(&mut self) {
        let Some(query) = self.place_search.take_query() else {
            return;
        };
        if !self.online {
            self.place_search.answer(&query, Vec::new());
            return;
        }
        self.pending += 1;
        let tx = self.net_tx.clone();
        spawn_net(async move {
            let results = net::geocode(&query).await;
            let _ = tx.send(NetMsg::PaletteSearch { query, results });
        });
    }

    /// Open a hit the platform search found.
    fn open_search_hit(&mut self, hit: PlaceHit, doc: &mut AutoCommit) {
        match hit {
            PlaceHit::Place(id) => {
                if self.model.place(&id).is_some() {
                    self.select_place(&id);
                }
            }
            PlaceHit::Poi(id) => {
                if self.pois.iter().any(|poi| poi.id == id) {
                    self.select_poi_from_list(&id);
                }
            }
            PlaceHit::Geocoded(id) => {
                let Some(place) = self.place_search.geocoded(&id) else {
                    return;
                };
                self.perform(
                    doc,
                    &MapsAction::UpsertPlaces {
                        places: vec![place.clone()],
                    },
                );
                self.perform(
                    doc,
                    &MapsAction::CommitSearch {
                        query: place.name.clone(),
                    },
                );
                self.select_place(&place.id);
            }
            PlaceHit::Recent(query) => {
                self.model.query = query;
                self.run_search(Some(doc));
            }
        }
    }

    /// Local (offline) search: substring match over known places. Also kicks
    /// off a live Nominatim request whose results fold in later.
    fn run_search(&mut self, doc: Option<&mut AutoCommit>) {
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
        if let Some(doc) = doc {
            self.perform(doc, &MapsAction::CommitSearch { query: q.clone() });
        } else {
            self.model.push_recent(&q);
        }
        self.model.panel = Panel::Explore;
        self.scroll = 0.0;
        // Results just landed — reveal the drawer on narrow layouts.
        self.nav_open = true;

        // Live augmentation (best effort).
        self.spawn_search(q);
    }

    fn spawn_search(&mut self, query: String) {
        self.pending += 1;
        self.model.status = "Searching…".to_string();
        if !self.online {
            return;
        }
        let tx = self.net_tx.clone();
        spawn_net(async move {
            let results = net::geocode(&query).await;
            let _ = tx.send(NetMsg::Search { query, results });
        });
    }

    /// Apply a component effect to the route model or the geocoding worker.
    fn apply_endpoint_effect(&mut self, effect: EndpointEffect) {
        match effect {
            EndpointEffect::Changed { endpoint, text } => {
                match endpoint {
                    Endpoint::Origin => text.clone_into(&mut self.model.origin_query),
                    Endpoint::Destination => text.clone_into(&mut self.model.dest_query),
                }
                let results = self.local_matches(&text);
                self.endpoints.set_results(endpoint, results);
            }
            EndpointEffect::Search { endpoint, query } => {
                self.run_endpoint_search(endpoint, query);
            }
            EndpointEffect::Pick { endpoint, place } => self.pick_endpoint(endpoint, &place),
            EndpointEffect::Swap => self.swap_ends(),
            EndpointEffect::Handled => {}
        }
    }

    fn run_endpoint_search(&mut self, endpoint: Endpoint, query: String) {
        let local_results = self.local_matches(&query);
        self.endpoints.set_results(endpoint, local_results);
        if query.trim().is_empty() {
            return;
        }
        self.pending += 1;
        self.model.status = "Searching…".to_string();
        if self.online {
            let tx = self.net_tx.clone();
            let origin = endpoint == Endpoint::Origin;
            spawn_net(async move {
                let results = net::geocode(&query).await;
                let _ = tx.send(NetMsg::EndpointSearch {
                    origin,
                    query,
                    results,
                });
            });
        }
    }

    /// Adopt a geocoded dropdown result as the chosen endpoint: set its coord +
    /// label and re-route (or re-plan transit). Mirrors selecting a destination
    /// so From and To behave symmetrically.
    fn pick_endpoint(&mut self, endpoint: Endpoint, p: &Place) {
        self.model.upsert_place(p.clone());
        if endpoint == Endpoint::Origin {
            self.model.origin = Some(p.pos);
            p.name.clone_into(&mut self.model.origin_name);
            self.model.origin_query.clear();
        } else {
            self.model.dest = Some(p.pos);
            p.name.clone_into(&mut self.model.dest_name);
            self.model.selected = Some(p.id.clone());
            self.model.dest_query.clear();
        }
        self.model.center = p.pos;
        if self.model.mode.is_transit() {
            self.recompute_transit();
        } else {
            self.recompute_route();
        }
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
    }

    /// Build the transit query time anchor from the picker. "Leave now" sends no
    /// time (the planner uses its own now); otherwise an RFC-3339 wall clock at
    /// `now + offset`, flagged depart/arrive.
    fn transit_query(&self) -> net::TransitQuery {
        let tt = self.model.transit_time;
        let time = if tt.is_now() {
            None
        } else {
            let when = ui::clock::local_now() + chrono::Duration::minutes(i64::from(tt.offset_min));
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
    fn enter_transit(&mut self, doc: &mut AutoCommit) {
        self.set_dest_to_selection();
        self.perform(
            doc,
            &MapsAction::SetTravelMode {
                mode: TravelMode::Transit,
            },
        );
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
            spawn_net(async move {
                let r = net::route(origin, dest, mode).await;
                let _ = tx.send(NetMsg::Route(r));
            });
        }
    }

    /// React to a change of the leave/arrive time anchor. Transit re-plans the
    /// journey against the new time; the road modes just re-derive the computed
    /// departure/arrival clock (OSRM is not time-dependent), so a repaint is
    /// enough — no needless refetch.
    fn replan_for_time(&mut self) {
        if self.model.mode.is_transit() {
            self.recompute_transit();
        }
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
            spawn_net(async move {
                let r = net::transit(origin, dest, &query).await;
                let _ = tx.send(NetMsg::Transit(r));
            });
        }
    }

    fn set_mode(&mut self, mode: TravelMode, doc: &mut AutoCommit) {
        if self.model.mode != mode {
            self.perform(doc, &MapsAction::SetTravelMode { mode });
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
            let saved = !now_saved;
            self.perform(
                doc,
                &MapsAction::SetPlaceSaved {
                    id: id.clone(),
                    saved,
                },
            );
            // The selection is local; reflect the explicit action payload in
            // this window even when the selected search result is not yet a
            // durable bookmark row.
            if let Some(place) = self.model.places.iter_mut().find(|place| place.id == id) {
                place.saved = saved;
            }
        }
    }

    fn clear_route(&mut self, doc: &mut AutoCommit) {
        self.perform(doc, &MapsAction::ClearRoute);
        self.navigation_placement.reset_attached();
        self.navigation_drag_handle = Rect::ZERO;
        self.navigation_end_rect = Rect::ZERO;
        self.navigation_end_hovered = false;
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

    /// Frame the map so `pts` fit the visible map area — beside the docked
    /// panel, under the turn card, clear of the controls — and centre them
    /// there rather than on the window. Used to fit a road route's geometry
    /// and, for transit where there is no road polyline, the endpoints.
    fn fit_points(&mut self, pts: &[LatLon]) {
        let area = self.visible_map_area();
        // Room for a pin and its name above an end of the line.
        let pad = f64::from(self.spacing.s12);
        let Some((center, zoom)) = geo::fit_bounds(pts, area.width(), area.height(), pad) else {
            return;
        };
        let zoom = zoom.min(16.5);
        // Shift the camera so the fitted centre lands on the area's centre.
        let world = geo::lonlat_to_world(center, zoom);
        let offset = area.center() - self.view_center();
        self.model.center = geo::world_to_lonlat(world.0 - offset.x, world.1 - offset.y, zoom);
        self.model.zoom = zoom;
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
    }

    fn pan_by(&mut self, dx: f64, dy: f64) {
        let cw = geo::lonlat_to_world(self.model.center, self.model.zoom);
        self.model.center = geo::world_to_lonlat(cw.0 - dx, cw.1 - dy, self.model.zoom);
        self.schedule_pois();
    }

    // --- POI layer (Overpass) ----------------------------------------------

    /// Arm the viewport POI refetch. Debounced so a drag/zoom gesture fires one
    /// Overpass query when it settles, not per pointer event.
    fn schedule_pois(&mut self) {
        self.poi_debounce = Some(ui::time::now() + POI_DEBOUNCE);
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
            // No valid viewport yet: `apply_layout` arms the fetch once one
            // exists, rather than a timer polling for it.
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
            spawn_net(async move {
                let pois = net::overpass(
                    bounds.min.lat,
                    bounds.min.lon,
                    bounds.max.lat,
                    bounds.max.lon,
                )
                .await;
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
        // The place card floats over the map: a narrow window closes the
        // drawer so the card and the place behind it show.
        self.nav_open = !self.overlay_mode();
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
    fn drain_net(&mut self, mut doc: Option<&mut AutoCommit>) {
        while let Ok(msg) = self.net_rx.try_recv() {
            self.pending = self.pending.saturating_sub(1);
            match msg {
                NetMsg::Search { query, results } => match results {
                    Ok(found) if !found.is_empty() => {
                        let mut ranked = found;
                        if let Some(state) = doc.as_deref_mut() {
                            self.perform(
                                state,
                                &MapsAction::UpsertPlaces {
                                    places: ranked.clone(),
                                },
                            );
                        } else {
                            for p in &ranked {
                                self.model.upsert_place(p.clone());
                            }
                        }
                        self.rank_results(&mut ranked, &query);
                        // A committed search: list the results in the panel
                        // and centre on the top (nearest) match.
                        if let Some(first) = ranked.first() {
                            self.model.center = first.pos;
                            self.model.selected = Some(first.id.clone());
                        }
                        self.model.results = ranked.clone();
                        self.model.status = format!("{} results for “{}”", ranked.len(), query);
                    }
                    Ok(_) => self.model.status = format!("No results for “{query}”"),
                    Err(e) => self.model.status = format!("Search failed: {e}"),
                },
                NetMsg::PaletteSearch { query, results } => {
                    let mut found = results.unwrap_or_default();
                    self.rank_results(&mut found, &query);
                    self.place_search.answer(&query, found);
                }
                NetMsg::EndpointSearch {
                    origin,
                    query,
                    results,
                } => match results {
                    Ok(found) if !found.is_empty() => {
                        if let Some(state) = doc.as_deref_mut() {
                            self.perform(
                                state,
                                &MapsAction::UpsertPlaces {
                                    places: found.clone(),
                                },
                            );
                        } else {
                            for p in &found {
                                self.model.upsert_place(p.clone());
                            }
                        }
                        // Rank the From/To suggestions the same way a search
                        // is ranked — relevance first, nearest-first within a
                        // tier — so a distant namesake never sits above the
                        // near place the user meant.
                        let mut ranked = found;
                        self.rank_results(&mut ranked, &query);
                        self.endpoints.finish_search(endpoint_of(origin), ranked);
                        self.model.status.clear();
                    }
                    Ok(_) => self
                        .endpoints
                        .finish_search(endpoint_of(origin), Vec::new()),
                    Err(e) => {
                        self.endpoints
                            .finish_search(endpoint_of(origin), Vec::new());
                        self.model.status = format!("Search failed: {e}");
                    }
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
                        if let Some(state) = doc.as_deref_mut() {
                            self.perform(
                                state,
                                &MapsAction::SetRoute {
                                    route: route.clone(),
                                    origin: self.model.origin,
                                    origin_name: self.model.origin_name.clone(),
                                    dest: self.model.dest,
                                    dest_name: self.model.dest_name.clone(),
                                    mode: self.model.mode,
                                },
                            );
                        } else {
                            self.model.route = Some(route);
                        }
                        self.fit_route();
                        self.model.status = "Route updated".to_string();
                    }
                    Err(e) => {
                        self.model.status = format!("Routing failed (offline estimate shown): {e}");
                    }
                },
                NetMsg::Transit(t) => match t {
                    Ok(j) if !j.is_empty() => {
                        if let Some(state) = doc.as_deref_mut() {
                            self.perform(
                                state,
                                &MapsAction::SetTransit {
                                    transit: j.clone(),
                                    origin: self.model.origin,
                                    origin_name: self.model.origin_name.clone(),
                                    dest: self.model.dest,
                                    dest_name: self.model.dest_name.clone(),
                                },
                            );
                        } else {
                            self.model.transit = j;
                        }
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
                if let Some(invocation) = self.location_invocation {
                    self.location_terminal = Some(InvocationUpdate::Failed {
                        invocation,
                        error: ActionFailure {
                            code: "location_channel_closed".into(),
                            message: "The location service disconnected before returning a fix"
                                .into(),
                            details: None,
                            retryable: true,
                        },
                    });
                }
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
                    spawn_net(async move {
                        let r = net::reverse_geocode(pos.lat, pos.lon).await;
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
                if let Some(invocation) = self.location_invocation {
                    self.location_terminal = Some(InvocationUpdate::Succeeded {
                        invocation,
                        output: ActionOutput::default(),
                    });
                }
            }
            LocationOutcome::Unavailable(reason) => {
                self.model.status = format!("Location unavailable — {reason}");
                if let Some(invocation) = self.location_invocation {
                    self.location_terminal = Some(InvocationUpdate::Failed {
                        invocation,
                        error: ActionFailure {
                            code: "location_unavailable".into(),
                            message: reason,
                            details: None,
                            retryable: true,
                        },
                    });
                }
            }
        }
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
            Hit::OpenSearch => {
                self.search_opener
                    .open(ui::search::SearchRequest::this_app());
            }
            Hit::ToggleCollapse => {
                self.panel_collapsed = !self.panel_collapsed;
            }
            Hit::PanelBack => {
                // Return from the Directions view to the plain places list. The
                // route stays in the model (it keeps drawing on the map); only
                // the panel view changes — this is navigation, not a tab.
                self.model.panel = Panel::Explore;
                self.endpoints.dismiss();
                self.scroll = 0.0;
            }
            Hit::Mode(m) => self.set_mode(*m, doc),
            Hit::SelectPlace(id) => {
                self.select_place(id);
            }
            Hit::SelectPoi(id) => self.select_poi_from_list(id),
            Hit::ClosePoi => {
                self.selected_poi = None;
            }
            Hit::PoiDirections => self.poi_directions(),
            Hit::Recent(q) => {
                self.search_opener
                    .open(ui::search::SearchRequest::this_app().with_query(q.clone()));
            }
            Hit::ClearRecents => self.perform(doc, &MapsAction::ClearRecents),
            Hit::ToggleNav => {
                self.nav_open = !self.nav_open;
            }
            Hit::CloseNav => {
                self.nav_open = false;
            }
            Hit::Directions => self.enter_directions(),
            Hit::Transit => self.enter_transit(doc),
            Hit::ToggleSave => self.toggle_save_selected(doc),
            Hit::ClearRoute => self.clear_route(doc),
            Hit::UseMyLocation => self.use_my_location(),
            Hit::TransitTimeMode(m) => {
                let mut value = self.model.transit_time;
                value.mode = *m;
                self.perform(doc, &MapsAction::SetTransitTime { value });
                self.replan_for_time();
            }
            Hit::TransitTimeDelta(d) => {
                let mut value = self.model.transit_time;
                value.offset_min = (value.offset_min + *d).clamp(0, 1440);
                self.perform(doc, &MapsAction::SetTransitTime { value });
                self.replan_for_time();
            }
            Hit::TransitTimeNow => {
                self.perform(
                    doc,
                    &MapsAction::SetTransitTime {
                        value: crate::model::TransitTime::default(),
                    },
                );
                self.replan_for_time();
            }
            Hit::Journey(i) => {
                self.model.transit_choice = *i;
                // Frame the newly-selected itinerary on the map (its legs are
                // what draws now), not just expand the panel card.
                self.fit_current();
            }
            Hit::ToggleOverflow => {
                if self.overflow.is_open() {
                    self.overflow.close();
                } else {
                    self.open_overflow();
                }
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
            OfflineAction::Download if !self.tiles.allows_prefetch() => {
                self.model.status = OFFLINE_AREAS_REFUSED.to_string();
            }
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
            }
            OfflineAction::DeleteRegion(id) => self.delete_region(doc, &id),
            OfflineAction::ClearCache => {
                let freed = self.tiles.clear_cache();
                self.offline_panel.set_cache_bytes(0);
                self.perform(doc, &MapsAction::ClearRegions);
                self.model.status = format!("Cleared {} of offline tiles", format_bytes(freed));
            }
            OfflineAction::Consume => {}
        }
    }
}

/// The endpoint a geocoder reply is for.
fn endpoint_of(origin: bool) -> Endpoint {
    if origin {
        Endpoint::Origin
    } else {
        Endpoint::Destination
    }
}

/// Whether `outer` fully contains `inner` (both lat/lon extents).
fn bounds_contains(outer: GeoBounds, inner: GeoBounds) -> bool {
    outer.min.lat <= inner.min.lat
        && outer.min.lon <= inner.min.lon
        && outer.max.lat >= inner.max.lat
        && outer.max.lon >= inner.max.lon
}

/// Intrinsic size of the floating navigation card in `theme`: the list-panel
/// width (grown with the text scale so the instruction keeps its character
/// budget), and the stack it holds — a title row with the End button, the
/// next instruction and its distance, a rule, and the trip summary.
fn navigation_card_size(theme: &Theme) -> vello::kurbo::Size {
    let sp = &theme.tokens.spacing;
    let line = |role: TextRole| f64::from(theme.text(role).line_height_px());
    let title_row = ButtonFace::height(ControlSize::Small, theme).max(line(TextRole::Caption));
    let height = f64::from(sp.s3) * 2.0
        + title_row
        + f64::from(sp.s2)
        + line(TextRole::Heading)
        + line(TextRole::Body)
        + f64::from(sp.s3) * 2.0
        + line(TextRole::Heading);
    let grow = f64::from(theme.text_scale.clamp(1.0, 4.0));
    vello::kurbo::Size::new(
        (f64::from(theme.tokens.metrics.panel_width_list) * grow).ceil(),
        height.ceil(),
    )
}

/// The theme for what is drawn on floating chrome: ink re-measured against
/// the panel ground, the selection lift stepped from it.
fn on_panel(theme: &Theme) -> Theme {
    theme.on_ground(ui::chrome::panel_ground(theme))
}

/// A travel time read as a clock: `10:57`.
fn clock_label(at: chrono::DateTime<chrono::Local>) -> String {
    at.format("%H:%M").to_string()
}

// ===========================================================================
// Drawing
// ===========================================================================
impl MapsApp {
    /// Whether the pointer rests on `rect` (and is not panning the map).
    fn hovered(&self, rect: Rect) -> bool {
        !self.dragging && rect.contains(self.last_mouse)
    }

    /// Paint an icon-only button and record what it does.
    fn icon_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        (icon, name): (Icon, &str),
        role: ButtonRole,
        on: bool,
        hit: Hit,
    ) {
        let face = ButtonFace::icon_only(rect, icon, name)
            .role(role)
            .hovered(self.hovered(rect))
            .pressed(on);
        Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
        self.click_targets.push((face.hit_rect(ctx.theme), hit));
    }

    /// Paint a labelled button and record what it does.
    fn text_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        face: ButtonFace<'_>,
        rect: Rect,
        hit: Option<Hit>,
    ) {
        let face = face.hovered(self.hovered(rect) && hit.is_some());
        Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
        if let Some(hit) = hit {
            self.click_targets.push((rect, hit));
        }
    }

    fn draw_app(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        self.click_targets.clear();
        self.history_rect = Rect::ZERO;
        self.text_scale = theme.text_scale;
        self.map_controls.set_metrics(ColumnMetrics::of(theme));
        self.map_controls.set_viewport(self.map_controls_viewport());
        if self.pending_fit && self.rect.area() > 0.0 {
            self.pending_fit = false;
            self.fit_current();
        }
        // Point the raster layer at the basemap style for the active theme (dark
        // tiles under a dark theme, light under light) before any tile is drawn,
        // so imagery and the drawn cartography never mix tones. A no-op on the
        // steady state; on a theme change it re-points the worker's decode.
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
            let left = (self.panel_rect().x1 + self.gap()).min(clip.x1);
            Rect::new(left, clip.y0, clip.x1, clip.y1)
        } else {
            clip
        };

        let view = MapView {
            proj: &proj,
            pal: &pal,
            clip,
        };
        let painted_tiles =
            self.draw_map_layers(&mut PaintCtx::new(scene, t, tr, theme), view, map_clip);
        // Loading / offline affordance while the substrate grid is showing —
        // above the map but below the chrome, and skipped while the drawer
        // covers the map (the drawer is the focus then).
        if !painted_tiles && self.basemap.is_empty() && !(overlay && self.nav_open) {
            self.draw_imagery_hint(scene, t, tr, theme);
        }
        self.draw_chrome(scene, t, tr, theme, overlay);
    }

    /// Map layers only: real tiles or their vector/grid fallback, traffic,
    /// route, transit, POIs and markers. Returns whether real imagery painted,
    /// which the caller needs to decide whether the offline/loading hint is due.
    fn draw_map_layers(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        view: MapView<'_>,
        map_clip: Rect,
    ) -> bool {
        let MapView { proj, pal, clip } = view;
        let theme = ctx.theme;
        // Map layers. Real OSM raster tiles paint first (non-blocking); the
        // stylised vector basemap is the fallback shown only until imagery is
        // available (an offline capture, or the very first offline frame).
        let view_center = self.view_center();
        let painted_tiles = canvas::draw_tiles(
            ctx.scene,
            ctx.transform,
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
                canvas::draw_grid_substrate(
                    ctx.scene,
                    ctx.transform,
                    clip,
                    &self.model,
                    view_center,
                    pal,
                );
            } else {
                canvas::draw_basemap(ctx.scene, ctx.transform, clip, proj, &self.basemap, pal);
            }
        }
        // Traffic overlay: coloured congestion over the roads, under the active
        // route + markers so navigation stays legible. Empty data draws
        // nothing here — the honest "No traffic data" note is chrome.
        if self.map_controls.traffic_enabled() {
            canvas::draw_traffic(ctx.scene, ctx.transform, proj, &self.traffic, pal);
        }
        if let Some(route) = &self.model.route {
            canvas::draw_route(ctx.scene, ctx.transform, proj, route, pal);
        }
        // Public-transport itinerary: the selected journey's legs draw on the
        // map (mode-coloured lines + transfer pucks) when Transit is the active
        // mode. Only the highlighted option is drawn to keep the map legible.
        if self.model.mode.is_transit()
            && let Some(j) = self.model.transit.get(self.model.transit_choice)
        {
            canvas::draw_transit(ctx.scene, ctx.transform, proj, j, theme, pal, clip);
        }
        // Live POI marker layer (Overpass) — our own themed category markers
        // over the basemap, decluttered and under the user's saved/selected
        // pins.
        let visible_pois = self.visible_poi_indices();
        canvas::draw_pois(
            ctx,
            MapView {
                clip: map_clip,
                ..view
            },
            &self.pois,
            &visible_pois,
            self.selected_poi.as_deref(),
        );
        canvas::draw_markers(
            ctx.scene,
            ctx.transform,
            ctx.text,
            proj,
            &self.model,
            theme,
            pal,
            map_clip,
        );
        painted_tiles
    }

    /// Chrome over the map: the places panel (docked, or a drawer over a
    /// scrim when narrow), the control column, the floating cards, the
    /// footer with the scale and the credit, the overflow menu, and the
    /// transient status bubble and tooltip on top. Modal sheets paint in
    /// the window's top layer (`draw_overlay`).
    fn draw_chrome(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        overlay: bool,
    ) {
        let drawer_open = overlay && self.nav_open;
        // The chrome that belongs to the map: under the drawer's scrim when
        // the drawer is open.
        self.map_controls.draw(scene, t, tr, theme);
        self.draw_map_footer(scene, t, tr, theme);
        self.draw_overflow(scene, t, tr, theme);
        if drawer_open {
            ui::overlay::scrim::modal(scene, t, self.rect, 1.0, theme);
            self.click_targets.push((self.rect, Hit::CloseNav));
            // Stale hit and accessibility geometry must not outlive the turn
            // card the open drawer hides.
            self.navigation_drag_handle = Rect::ZERO;
            self.navigation_end_rect = Rect::ZERO;
            self.navigation_end_hovered = false;
        } else {
            self.draw_traffic_status(scene, t, tr, theme);
            // The place card floats over the map, its actions above the
            // controls in the hit-test.
            self.draw_poi_card(scene, t, tr, theme);
            let uncovered_desktop_copy = self.model.route.is_some()
                && matches!(
                    self.navigation_placement.attachment(),
                    ui::floating_surface::Attachment::Desktop { .. }
                )
                && !self.navigation_placement.is_desktop_covered();
            if self.navigation_surface_visible_in_app() || uncovered_desktop_copy {
                self.draw_navigation_surface(scene, t, tr, theme);
            }
        }
        if self.panel_visible() {
            self.draw_panel(scene, t, tr, theme);
            // The Directions From/To suggestions float under the focused
            // field so the panel below never reflows.
            if matches!(self.model.panel, Panel::Directions) {
                self.endpoints.draw_overlay(scene, t, tr, theme);
            }
        }
        self.draw_top_row(scene, t, tr, theme);
        if self.overflow.is_open() {
            self.overflow
                .draw(scene, t, tr, theme, &PresenceCtx::empty());
        }
        self.draw_status(scene, t, tr, theme);
        self.map_controls.draw_tooltip(scene, t, tr, theme);
    }

    /// The overflow button (attribution, offline maps, licences) in the
    /// bottom-trailing corner; pressed while its menu is open.
    fn draw_overflow(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let r = self.overflow_rect();
        let open = self.overflow.is_open();
        self.icon_button(
            &mut PaintCtx::new(scene, t, tr, theme),
            r,
            (Icon::MoreHorizontal, "More"),
            ButtonRole::Secondary,
            open,
            Hit::ToggleOverflow,
        );
    }

    /// The narrow window's top row: the button that shows and hides the
    /// places drawer, and the search button beside it. A docked panel
    /// carries search in its own header instead.
    fn draw_top_row(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let Some(menu) = self.menu_rect() else { return };
        let name = if self.nav_open {
            "Hide places"
        } else {
            "Show places"
        };
        let open = self.nav_open;
        self.icon_button(
            &mut PaintCtx::new(scene, t, tr, theme),
            menu,
            (Icon::PanelLeft, name),
            ButtonRole::Secondary,
            open,
            Hit::ToggleNav,
        );
        if let Some(search) = self.search_button_rect() {
            self.icon_button(
                &mut PaintCtx::new(scene, t, tr, theme),
                search,
                (Icon::Search, "Search places"),
                ButtonRole::Secondary,
                false,
                Hit::OpenSearch,
            );
            let history = Rect::new(
                search.x1 + f64::from(self.spacing.s2),
                search.y0,
                search.x1 + f64::from(self.spacing.s2) + search.width(),
                search.y1,
            );
            self.draw_history_button(scene, t, tr, theme, history, ButtonRole::Secondary);
        }
    }

    /// The scale and the tile credit, on one floating pill left of the
    /// overflow button: the credit, then the scale's label with its bar a
    /// token gap after it, so the label never sits on the line. When the map
    /// is too narrow for both, the credit floats on its own pill above.
    fn draw_map_footer(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let sp = &theme.tokens.spacing;
        let (s2, s3) = (f64::from(sp.s2), f64::from(sp.s3));
        let ink = on_panel(theme);
        let caption = theme.text(TextRole::Caption);
        let numeric = caption.clone().numeric();
        let overflow = self.overflow_rect();
        let right = overflow.x0 - self.gap();
        let bar_max = f64::from(sp.s16) + f64::from(sp.s8);
        let (label, bar_w) = geo::scale_bar(self.model.center, self.model.zoom, bar_max);
        let label_w = draw::measure_w(tr, &label, &numeric);
        let credit = crate::tiles::TILE_ATTRIBUTION;
        let credit_w = draw::measure_w(tr, credit, &caption);
        let scale_w = s3 + label_w + s2 + bar_w + s3;
        let both_w = s3 + credit_w + s3 + label_w + s2 + bar_w + s3;
        let left = self.map_left_edge();
        let together = right - both_w >= left;
        let scale_x0 = if together {
            right - both_w
        } else {
            right - scale_w
        };
        let pill = Rect::new(scale_x0, overflow.y0, right, overflow.y1);
        ui::widget::floating_surface::paint(scene, t, pill, theme);
        let mut ctx = PaintCtx::new(scene, t, tr, &ink);
        let top = draw::vmid(ctx.text, pill.y0, pill.height(), &caption);
        let mut x = pill.x0 + s3;
        if together {
            draw::text(&mut ctx, credit, Point::new(x, top), &caption, ink.text_dim);
            x += credit_w + s3;
        }
        draw::text(&mut ctx, &label, Point::new(x, top), &numeric, ink.text_dim);
        x += label_w + s2;
        // The bar itself is map content: a line the length of the labelled
        // distance with a tick at each end.
        let cy = pill.center().y;
        let tick = f64::from(sp.s1);
        let stroke = f64::from(theme.tokens.metrics.chrome_rule_px) * 1.5;
        draw::line(
            ctx.scene,
            t,
            Point::new(x, cy),
            Point::new(x + bar_w, cy),
            stroke,
            ink.text_primary,
        );
        for end in [x, x + bar_w] {
            draw::line(
                ctx.scene,
                t,
                Point::new(end, cy - tick),
                Point::new(end, cy + tick),
                stroke,
                ink.text_primary,
            );
        }
        if !together {
            let h = pill.height();
            let credit_pill = Rect::new(
                (right - (credit_w + 2.0 * s3)).max(left),
                pill.y0 - self.gap() - h,
                right,
                pill.y0 - self.gap(),
            );
            ui::widget::floating_surface::paint(ctx.scene, t, credit_pill, theme);
            let top = draw::vmid(ctx.text, credit_pill.y0, h, &caption);
            draw::text_clip(
                &mut ctx,
                credit,
                Point::new(credit_pill.x0 + s3, top),
                &caption,
                ink.text_dim,
                credit_pill.width() - 2.0 * s3,
            );
        }
    }

    /// The transient status (searching, routing, a download's progress, a
    /// recoverable failure) in the status bubble, above the overflow button.
    fn draw_status(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let now = ui::time::now();
        self.status_bubble.show(&self.model.status, now);
        let area = self.status_area(theme);
        self.status_bubble.paint(scene, t, tr, theme, area, now);
    }

    /// Where the status bubble floats: the map beside the panel, ending just
    /// above the footer row.
    fn status_area(&self, theme: &Theme) -> Rect {
        let inset = f64::from(theme.tokens.spacing.s4);
        let bottom = self.overflow_rect().y0 - self.gap() + inset;
        Rect::new(
            self.map_left_edge() - inset,
            self.rect.y0,
            self.rect.x1 - self.margin() + inset,
            bottom,
        )
    }

    /// Text over the substrate grid telling the person the imagery is
    /// loading (or unreachable), so a first-run / offline map is never a
    /// silent void. Text only, centred in the visible map area.
    fn draw_imagery_hint(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let fault = self.tiles.failure().filter(|_| !self.tiles.has_inflight());
        let msg = match fault {
            Some(TileFault::Rejected) => "The map tile server refused the request",
            Some(TileFault::Unreachable) => "Map imagery is unavailable offline",
            None => "Loading map…",
        };
        let area = self.visible_map_area();
        let style = theme.text(TextRole::Body);
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let h = draw::line_h(ctx.text, &style);
        draw::text_center(
            &mut ctx,
            msg,
            Point::new(area.center().x, area.center().y - h / 2.0),
            &style,
            theme.text_dim,
        );
    }

    /// The live turn card: a floating panel with the next instruction and
    /// the trip summary, dragged out to the desktop by its title row.
    #[allow(
        clippy::too_many_lines,
        reason = "the navigation card is one cohesive painted surface with shared geometry for its drag and end-navigation targets"
    )]
    fn draw_navigation_surface(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(route) = self.model.route.clone() else {
            self.navigation_drag_handle = Rect::ZERO;
            self.navigation_end_rect = Rect::ZERO;
            self.navigation_end_hovered = false;
            return;
        };
        let rect = self.navigation_rect();
        let visual =
            self.navigation_placement
                .presentation_transform(transform, rect, theme.reduce_motion);
        ui::widget::floating_surface::paint(scene, visual, rect, theme);
        let ink = on_panel(theme);
        let sp = &theme.tokens.spacing;
        let (s1, s2, s3) = (f64::from(sp.s1), f64::from(sp.s2), f64::from(sp.s3));
        let pad = s3;
        let caption = theme.text(TextRole::Caption);
        let heading = theme.text(TextRole::Heading);
        let body_numeric = theme.text(TextRole::Body).numeric();

        // Title row: "Navigation" (the drag handle) and the End button.
        let end_face = ButtonFace::new(Rect::ZERO, "End").control_size(ControlSize::Small);
        let end_w = end_face.intrinsic_width(tr, theme);
        let end_h = ButtonFace::height(ControlSize::Small, theme);
        let end = Rect::new(
            rect.x1 - pad - end_w,
            rect.y0 + pad,
            rect.x1 - pad,
            rect.y0 + pad + end_h,
        );
        self.navigation_end_rect = end;
        let handle = Rect::new(rect.x0, rect.y0, end.x0 - s1, end.y1 + s1);
        self.navigation_drag_handle = handle;
        Button::paint(
            scene,
            visual,
            tr,
            &ink,
            &ButtonFace::new(end, "End")
                .control_size(ControlSize::Small)
                .hovered(self.navigation_end_hovered),
        );
        let mut ctx = PaintCtx::new(scene, visual, tr, &ink);
        let title_top = draw::vmid(ctx.text, end.y0, end.height(), &caption);
        draw::text(
            &mut ctx,
            "Navigation",
            Point::new(rect.x0 + pad, title_top),
            &caption,
            ink.text_dim,
        );
        ui::surface::floating_drag_grip(ctx.scene, visual, rect, ink.text_tertiary);

        // The next instruction: its manoeuvre icon, the instruction, and the
        // distance to it on the line below.
        let instruction = route.steps.first();
        let icon = instruction.map_or(Icon::ArrowUp, |step| step.maneuver.icon());
        let text = instruction.map_or("Continue on the route", |step| step.instruction.as_str());
        let icon_side = f64::from(theme.tokens.metrics.icon_lg);
        let instr_y = end.y1 + s2;
        let heading_h = draw::line_h(ctx.text, &heading);
        let icon_rect = Rect::new(
            rect.x0 + pad,
            instr_y + (heading_h - icon_side) / 2.0,
            rect.x0 + pad + icon_side,
            instr_y + (heading_h + icon_side) / 2.0,
        );
        icon.draw(ctx.scene, visual, icon_rect, ink.text_primary);
        let text_x = icon_rect.x1 + s3;
        draw::text_clip(
            &mut ctx,
            text,
            Point::new(text_x, instr_y),
            &heading,
            ink.text_primary,
            rect.x1 - pad - text_x,
        );
        let next_distance = instruction.map_or(route.distance_m, |step| step.distance_m);
        let dist_y = instr_y + heading_h;
        draw::text(
            &mut ctx,
            &geo::fmt_distance(next_distance),
            Point::new(text_x, dist_y),
            &body_numeric,
            ink.text_dim,
        );
        let rule_y = dist_y + draw::line_h(ctx.text, &body_numeric) + s3;
        ui::surface::separator(
            ctx.scene,
            visual,
            rect.x0 + pad,
            rect.x1 - pad,
            rule_y,
            0.0,
            ink.tokens.colors.surfaces.content,
        );

        // The summary: the time to go and the distance left.
        let summary_y = rule_y + s3;
        let duration_style = heading.clone().numeric();
        draw::text(
            &mut ctx,
            &geo::fmt_duration(route.duration_s),
            Point::new(rect.x0 + pad, summary_y),
            &duration_style,
            ink.text_primary,
        );
        let remaining = format!("{} to go", geo::fmt_distance(route.distance_m));
        let numeric_caption = caption.clone().numeric();
        let rem_top = summary_y + (heading_h - draw::line_h(ctx.text, &numeric_caption)) / 2.0;
        draw::text_right(
            &mut ctx,
            &remaining,
            rect.x1 - pad,
            rem_top,
            &numeric_caption,
            ink.text_dim,
        );
    }

    /// Floating info card for the selected POI, anchored at the bottom of the
    /// visible map area (clear of the docked panel and the control column):
    /// its name, what it is, the facts OpenStreetMap has for it, and
    /// Directions.
    #[allow(
        clippy::too_many_lines,
        reason = "a single linear draw pass over this card's fixed regions; splitting it would only \
              relocate the line count, not the amount a reader must hold in mind"
    )]
    #[allow(
        clippy::many_single_char_names,
        reason = "the shared drawing names (t: transform, x/y: the pen, s1–s4: spacing steps) \
              used consistently across every draw_* method in this file"
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
        let ink = on_panel(theme);
        let sp = &theme.tokens.spacing;
        let (s1, s2, s4) = (f64::from(sp.s1), f64::from(sp.s2), f64::from(sp.s4));
        let pad = s4;

        // The facts OpenStreetMap has for this place.
        let mut facts: Vec<PropertyRow<'_>> = Vec::new();
        if !poi.address.is_empty() {
            facts.push(PropertyRow::new("Address", &poi.address));
        }
        if let Some(hours) = &poi.opening_hours {
            facts.push(PropertyRow::new("Hours", hours));
        }
        if let Some(phone) = &poi.phone {
            facts.push(PropertyRow::new("Phone", phone).with_kind(TextKind::Numeric));
        }
        if let Some(web) = &poi.website {
            facts.push(PropertyRow::new("Website", web).with_kind(TextKind::Identifier));
        }
        let properties = PropertyListFace::new(&facts);

        let area = self.visible_map_area();
        let w = f64::from(theme.tokens.metrics.panel_width_list)
            .min((area.width()).max(f64::from(sp.s16) * 3.0));
        let inner_w = w - 2.0 * pad;
        let title = theme.text(TextRole::Title);
        let caption = theme.text(TextRole::Caption);
        let title_h = draw::line_h(tr, &title);
        let caption_h = draw::line_h(tr, &caption);
        let facts_h = if facts.is_empty() {
            0.0
        } else {
            s4 + properties.height(tr, &ink, inner_w)
        };
        let button_h = ButtonFace::height(ControlSize::Regular, theme);
        let card_h = (pad + title_h + s1 + caption_h + facts_h + s4 + button_h + pad).ceil();
        let x0 = (area.center().x - w / 2.0).max(area.x0);
        let y1 = area.y1;
        let card = Rect::new(x0, y1 - card_h, x0 + w, y1);
        ui::widget::floating_surface::paint(scene, t, card, theme);
        self.click_targets.push((card, Hit::Consume));

        let x = card.x0 + pad;
        let right = card.x1 - pad;
        let mut y = card.y0 + pad;
        // Close: an icon button seated on the title line.
        let side = ButtonFace::height(ControlSize::Regular, theme);
        let close = Rect::new(
            card.x1 - s2 - side,
            y + (title_h - side) / 2.0,
            card.x1 - s2,
            y + (title_h + side) / 2.0,
        );
        self.icon_button(
            &mut PaintCtx::new(scene, t, tr, &ink),
            close,
            (Icon::X, "Close"),
            ButtonRole::Plain,
            false,
            Hit::ClosePoi,
        );
        let mut ctx = PaintCtx::new(scene, t, tr, &ink);
        draw::text_clip(
            &mut ctx,
            &poi.name,
            Point::new(x, y),
            &title,
            ink.text_primary,
            close.x0 - s2 - x,
        );
        y += title_h + s1;
        // What it is: OpenStreetMap's kind when tagged, else the category.
        draw::text_clip(
            &mut ctx,
            &poi.kind_label(),
            Point::new(x, y),
            &caption,
            ink.text_dim,
            right - x,
        );
        y += caption_h;
        if !facts.is_empty() {
            y += s4;
            let h = properties.height(ctx.text, &ink, inner_w);
            properties.paint(&mut ctx, Rect::new(x, y, right, y + h));
            y += h;
        }
        y += s4;
        let btn = Rect::new(x, y, right, y + button_h);
        self.text_button(
            &mut PaintCtx::new(scene, t, tr, &ink),
            ButtonFace::new(btn, "Directions").role(ButtonRole::Primary),
            btn,
            Some(Hit::PoiDirections),
        );
    }

    /// The traffic key, bottom-leading in the map area: with data, the
    /// congestion levels with their counts and where the reports come from;
    /// without, the honest "No traffic data". Nothing while the overlay is
    /// off.
    #[allow(
        clippy::many_single_char_names,
        reason = "the shared drawing names (t: transform, x/y: the pen, s1–s4: spacing steps) \
              used consistently across every draw_* method in this file"
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
        // the focus — don't stack a key over it.
        if self.overlay_mode() && self.nav_open {
            return;
        }
        let ink = on_panel(theme);
        let pal = MapPalette::from_theme(theme);
        let sp = &theme.tokens.spacing;
        let (s1, s2, s3) = (f64::from(sp.s1), f64::from(sp.s2), f64::from(sp.s3));
        let left = self.map_left_edge();
        let bottom = self.overflow_rect().y1;
        let label = theme.text(TextRole::Label);
        let caption = theme.text(TextRole::Caption);
        let numeric = caption.clone().numeric();

        if self.traffic.is_empty() {
            let text = "No traffic data";
            let w = draw::measure_w(tr, text, &label) + 2.0 * s3;
            let h = self.overflow_rect().height();
            let pill = Rect::new(left, bottom - h, left + w, bottom);
            ui::widget::floating_surface::paint(scene, t, pill, theme);
            let mut ctx = PaintCtx::new(scene, t, tr, &ink);
            let top = draw::vmid(ctx.text, pill.y0, h, &label);
            draw::text(
                &mut ctx,
                text,
                Point::new(pill.x0 + s3, top),
                &label,
                ink.text_primary,
            );
            return;
        }

        let counts = self.traffic.counts();
        let heading = theme.text(TextRole::Subheading);
        let heading_h = draw::line_h(tr, &heading);
        let caption_h = draw::line_h(tr, &caption);
        let row_h = caption_h + s1;
        let source_h = if self.traffic.source.is_some() {
            caption_h
        } else {
            0.0
        };
        let w = f64::from(sp.s16) * 3.0;
        let h = s3 + heading_h + source_h + s2 + Congestion::ALL.len() as f64 * row_h + s2;
        let card = Rect::new(left, bottom - h, left + w, bottom);
        ui::widget::floating_surface::paint(scene, t, card, theme);
        let mut ctx = PaintCtx::new(scene, t, tr, &ink);
        let x = card.x0 + s3;
        let right = card.x1 - s3;
        let mut y = card.y0 + s3;
        draw::text(
            &mut ctx,
            "Traffic",
            Point::new(x, y),
            &heading,
            ink.text_primary,
        );
        y += heading_h;
        if let Some(src) = &self.traffic.source {
            draw::text_clip(
                &mut ctx,
                &ui::format::capitalize_first(src),
                Point::new(x, y),
                &caption,
                ink.text_dim,
                right - x,
            );
            y += caption_h;
        }
        y += s2;
        let swatch_w = f64::from(sp.s4);
        for level in Congestion::ALL {
            let mid = y + caption_h / 2.0;
            let swatch = Rect::new(x, mid - s1 / 2.0 - 1.0, x + swatch_w, mid + s1 / 2.0 + 1.0);
            draw::fill_round(
                ctx.scene,
                t,
                swatch,
                f64::from(theme.tokens.radii.detail),
                canvas::traffic_color(&pal, level),
            );
            draw::text(
                &mut ctx,
                level.label(),
                Point::new(x + swatch_w + s2, y),
                &caption,
                ink.text_primary,
            );
            let n = counts.get(level as usize).copied().unwrap_or(0).to_string();
            draw::text_right(&mut ctx, &n, right, y, &numeric, ink.text_dim);
            y += row_h;
        }
    }

    /// The places panel: a floating panel down the leading edge (a drawer in
    /// a narrow window) with a header row — Back in Directions, the title,
    /// search and the collapse toggle — over the scrolled content. Collapsed
    /// it is just its header.
    #[allow(
        clippy::too_many_lines,
        reason = "one draw pass over the panel's header and its clipped, scrolled content"
    )]
    fn draw_panel(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let pr = self.panel_rect();
        let sp = &theme.tokens.spacing;
        let (s1, s2) = (f64::from(sp.s1), f64::from(sp.s2));
        let header_h = f64::from(theme.tokens.metrics.bar_height);
        if pr.height() < header_h + 2.0 * s2 {
            return;
        }
        let docked = !self.overlay_mode();
        let collapsed = self.panel_collapsed && docked;
        let directions = matches!(self.model.panel, Panel::Directions) && !collapsed;
        let body = if collapsed {
            Rect::new(pr.x0, pr.y0, pr.x1, pr.y0 + header_h + 2.0 * s1)
        } else {
            pr
        };
        ui::widget::floating_surface::paint(scene, t, body, theme);
        self.click_targets.push((body, Hit::Consume));
        let ink = on_panel(theme);

        // Header row.
        let header = Rect::new(
            body.x0 + s1,
            body.y0 + s1,
            body.x1 - s1,
            body.y0 + s1 + header_h,
        );
        let side = ButtonFace::height(ControlSize::Regular, theme);
        let button_at = |x0: f64| {
            Rect::new(
                x0,
                header.center().y - side / 2.0,
                x0 + side,
                header.center().y + side / 2.0,
            )
        };
        let mut title_x = header.x0 + f64::from(sp.s3);
        if directions {
            let back = button_at(header.x0 + s1);
            self.icon_button(
                &mut PaintCtx::new(scene, t, tr, &ink),
                back,
                (Icon::ChevronLeft, "Back to places"),
                ButtonRole::Plain,
                false,
                Hit::PanelBack,
            );
            title_x = back.x1 + s1;
        }
        let mut trailing = header.x1 - s1;
        if docked {
            let toggle = button_at(trailing - side);
            let (icon, name) = if collapsed {
                (Icon::ChevronDown, "Expand panel")
            } else {
                (Icon::ChevronUp, "Collapse panel")
            };
            self.icon_button(
                &mut PaintCtx::new(scene, t, tr, &ink),
                toggle,
                (icon, name),
                ButtonRole::Plain,
                false,
                Hit::ToggleCollapse,
            );
            trailing = toggle.x0;
            let search = button_at(trailing - side);
            self.icon_button(
                &mut PaintCtx::new(scene, t, tr, &ink),
                search,
                (Icon::Search, "Search places"),
                ButtonRole::Plain,
                false,
                Hit::OpenSearch,
            );
            trailing = search.x0;
            let history = button_at(trailing - side);
            self.draw_history_button(scene, t, tr, &ink, history, ButtonRole::Plain);
            trailing = history.x0;
        }
        let title = if matches!(self.model.panel, Panel::Directions) {
            "Directions"
        } else {
            "Places"
        };
        let heading = theme.text(TextRole::Heading);
        let mut ctx = PaintCtx::new(scene, t, tr, &ink);
        let top = draw::vmid(ctx.text, header.y0, header_h, &heading);
        draw::text_clip(
            &mut ctx,
            title,
            Point::new(title_x, top),
            &heading,
            ink.text_primary,
            trailing - s2 - title_x,
        );
        if collapsed {
            self.content_view_h = 0.0;
            self.content_h = 0.0;
            return;
        }

        // Content region (below the header), scrolled, and clipped at the
        // last block that fits whole, so the bottom never shows a sliver.
        let content = Rect::new(pr.x0 + s2, header.y1 + s1, pr.x1 - s2, pr.y1 - s2);
        self.content_view_h = content.height();
        let visible = self.scroll + content.height();
        let clip_bottom = if self.content_h > visible + 0.5 {
            self.panel_edges
                .iter()
                .copied()
                .filter(|edge| *edge > self.scroll && *edge <= visible + 0.5)
                .fold(None, |best: Option<f64>, edge| {
                    Some(best.map_or(edge, |b| b.max(edge)))
                })
                .map_or(content.y1, |edge| content.y0 + edge - self.scroll)
        } else {
            content.y1
        };
        let clip = Rect::new(content.x0, content.y0, content.x1, clip_bottom);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &clip);
        let top = content.y0 - self.scroll;
        self.panel_origin = top;
        self.panel_edges.clear();
        let drawn = match self.model.panel {
            Panel::Explore => self.draw_explore(scene, t, tr, &ink, content, top),
            Panel::Directions => self.draw_directions(scene, t, tr, &ink, content, top),
        };
        scene.pop_layer();
        self.content_h = drawn - content.y0 + self.scroll;
    }

    /// Paint the History button at `rect`.
    fn draw_history_button(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        role: ButtonRole,
    ) {
        self.history.set_role(role);
        self.history.apply_layout_external(rect);
        self.history_rect = rect;
        self.history
            .draw(scene, t, tr, theme, &PresenceCtx::empty());
    }

    /// Route a pointer event to the History button; whether it took it.
    fn drive_history(&mut self, event: &WindowEvent, ctx: &ui::widget::EventCtx<'_>) -> bool {
        let mut doc = AutoCommit::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let Ok(node) = taffy.new_leaf(Style::default()) else {
            return false;
        };
        let mut handled = false;
        let mut focus_target: Option<NodeId> = None;
        self.history.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: ctx.scale_factor,
                modifiers: ctx.modifiers,
                state: &mut doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    /// A group label inside the panel, with an optional trailing text action
    /// on its line. Returns the y below it.
    fn draw_group_header(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        content: Rect,
        y: f64,
        title: &str,
        action: Option<(&str, Hit)>,
    ) -> f64 {
        let theme = ctx.theme;
        let sp = &theme.tokens.spacing;
        let (s1, s3) = (f64::from(sp.s1), f64::from(sp.s3));
        let style = theme.text(TextRole::Subheading);
        let line = draw::line_h(ctx.text, &style);
        let action_h = ButtonFace::height(ControlSize::Small, theme);
        let band_h = line.max(if action.is_some() { action_h } else { 0.0 });
        let band = Rect::new(content.x0, y, content.x1, y + band_h);
        let top = draw::vmid(ctx.text, band.y0, band_h, &style);
        draw::text(
            ctx,
            title,
            Point::new(content.x0 + s3, top),
            &style,
            theme.text_primary,
        );
        if let Some((label, hit)) = action {
            let face = ButtonFace::new(Rect::ZERO, label)
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small);
            let w = face.intrinsic_width(ctx.text, theme);
            let rect = Rect::new(
                content.x1 - w,
                band.center().y - action_h / 2.0,
                content.x1,
                band.center().y + action_h / 2.0,
            );
            self.text_button(
                ctx,
                ButtonFace::new(rect, label)
                    .role(ButtonRole::Plain)
                    .control_size(ControlSize::Small),
                rect,
                Some(hit),
            );
        }
        band.y1 + s1
    }

    /// Scroll the panel one block per wheel step, so its first visible row
    /// is always whole.
    fn scroll_panel(&mut self, dy: f64) {
        let max = (self.content_h - self.content_view_h).max(0.0);
        let edges = std::iter::once(0.0).chain(self.panel_edges.iter().copied());
        let next = if dy < 0.0 {
            edges
                .filter(|edge| *edge > self.scroll + 0.5)
                .min_by(f64::total_cmp)
                .unwrap_or(max)
        } else if dy > 0.0 {
            edges
                .filter(|edge| *edge < self.scroll - 0.5)
                .max_by(f64::total_cmp)
                .unwrap_or(0.0)
        } else {
            self.scroll
        };
        self.scroll = next.clamp(0.0, max);
    }

    /// Note that a block of the panel's content ends at `y`.
    fn mark_edge(&mut self, y: f64) {
        self.panel_edges.push(y - self.panel_origin);
    }

    /// One list row in the panel. Returns the y below it.
    fn draw_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        content: Rect,
        y: f64,
        face: ListRowFace<'_>,
        selected: bool,
        separator: bool,
        hit: Hit,
    ) -> f64 {
        let h = face.height(ctx.text, ctx.theme);
        let rect = Rect::new(content.x0, y, content.x1, y + h);
        let face = face.selected(selected).interaction(Interaction {
            hovered: self.hovered(rect),
            ..Interaction::default()
        });
        let face = if separator {
            face.separator(false)
        } else {
            face
        };
        face.paint(ctx, rect);
        self.click_targets.push((rect, hit));
        self.mark_edge(rect.y1);
        rect.y1
    }

    /// The Explore view: the selected place, then results, nearby places,
    /// saved places and recent searches — or, with nothing to show, one
    /// sentence and the way to search. Returns the y below the content.
    #[allow(
        clippy::too_many_lines,
        reason = "a single linear draw pass over this panel's sections; splitting it would only \
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
        let sp = &theme.tokens.spacing;
        let (s2, s3, s4) = (f64::from(sp.s2), f64::from(sp.s3), f64::from(sp.s4));
        let anchor = self.search_anchor();
        let distance = |pos: LatLon| geo::fmt_distance(geo::haversine_m(pos, anchor));

        // First run: nothing selected, searched, saved, recent, or fetched for
        // the viewport — one sentence and the one action that changes that.
        let empty = self.model.selected_place().is_none()
            && self.model.results.is_empty()
            && self.model.saved_places().is_empty()
            && self.model.recents.is_empty()
            && self.pois.is_empty();
        if empty {
            let rect = Rect::new(content.x0, y, content.x1, content.y1);
            let empty_state = EmptyStateContent {
                icon: Some(Icon::Search),
                action_label: Some("Search places"),
                ..EmptyStateContent::new("Search for a place or an address to see it here.")
            };
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            if let Some(action) = EmptyState::paint(&mut ctx, rect, &empty_state, self.last_mouse) {
                self.click_targets.push((action, Hit::OpenSearch));
            }
            return content.y1;
        }

        // The selected place: an object card with what it is, where it is,
        // and what to do with it.
        if let Some(sel) = self.model.selected_place().cloned() {
            let card_rect_w = content.width() - 2.0 * s2;
            let face = CardFace::new().title(&sel.name);
            let caption = theme.text(TextRole::Caption);
            let caption_h = draw::line_h(tr, &caption);
            let button_h = ButtonFace::height(ControlSize::Regular, theme);
            let body_h = caption_h * 2.0 + s4 + button_h;
            let card_h = face.height_for_body(tr, theme, card_rect_w, body_h);
            let card = Rect::new(content.x0 + s2, y + s2, content.x1 - s2, y + s2 + card_h);
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            face.paint(&mut ctx, card);
            let body = face.body_rect(ctx.text, theme, card);
            draw::text(
                &mut ctx,
                sel.category.label(),
                Point::new(body.x0, body.y0),
                &caption,
                theme.text_dim,
            );
            draw::text_clip(
                &mut ctx,
                &sel.address,
                Point::new(body.x0, body.y0 + caption_h),
                &caption,
                theme.text_dim,
                body.width(),
            );
            // The actions: Directions is the card's one primary action.
            let by = body.y0 + caption_h * 2.0 + s4;
            let bw = (body.width() - 2.0 * s2) / 3.0;
            let saved = sel.saved;
            let actions: [(&str, Hit, ButtonRole); 3] = [
                ("Directions", Hit::Directions, ButtonRole::Primary),
                ("Transit", Hit::Transit, ButtonRole::Secondary),
                (
                    if saved { "Saved" } else { "Save" },
                    Hit::ToggleSave,
                    ButtonRole::Secondary,
                ),
            ];
            for (i, (label, hit, role)) in actions.into_iter().enumerate() {
                let bx = body.x0 + i as f64 * (bw + s2);
                let br = Rect::new(bx, by, bx + bw, by + button_h);
                let mut face = ButtonFace::new(br, label).role(role);
                if matches!(hit, Hit::ToggleSave) {
                    face = face.icon(Icon::Star).pressed(saved);
                }
                self.text_button(&mut PaintCtx::new(scene, t, tr, theme), face, br, Some(hit));
            }
            self.mark_edge(card.y1);
            y = card.y1 + s4;
        }

        // Results of a committed search.
        let results = self.model.results.clone();
        if !results.is_empty() {
            y = self.draw_group_header(
                &mut PaintCtx::new(scene, t, tr, theme),
                content,
                y,
                "Results",
                None,
            );
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            let last = results.len() - 1;
            for (i, p) in results.iter().enumerate() {
                let meta = distance(p.pos);
                let secondary = if p.address.is_empty() {
                    p.category.label()
                } else {
                    p.address.as_str()
                };
                let selected = self.model.selected.as_deref() == Some(p.id.as_str());
                y = self.draw_row(
                    &mut ctx,
                    content,
                    y,
                    ListRowFace::new(&p.name).secondary(secondary).meta(&meta),
                    selected,
                    i < last,
                    Hit::SelectPlace(p.id.clone()),
                );
            }
            y += s3;
        }

        // Nearby POIs fetched for the current viewport (Overpass), nearest-first
        // and linked to the marker layer: tapping a row centres the map and
        // opens the POI's info card, and the selected marker selects its row.
        let nearby = self.nearby_pois();
        if !nearby.is_empty() {
            y = self.draw_group_header(
                &mut PaintCtx::new(scene, t, tr, theme),
                content,
                y,
                "Nearby",
                None,
            );
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            let last = nearby.len() - 1;
            for (i, poi) in nearby.iter().enumerate() {
                let meta = distance(poi.pos);
                let secondary = poi.kind_label();
                let selected = self.selected_poi.as_deref() == Some(poi.id.as_str());
                y = self.draw_row(
                    &mut ctx,
                    content,
                    y,
                    ListRowFace::new(&poi.name)
                        .secondary(&secondary)
                        .meta(&meta),
                    selected,
                    i < last,
                    Hit::SelectPoi(poi.id.clone()),
                );
            }
            y += s3;
        }

        let saved: Vec<Place> = self.model.saved_places().into_iter().cloned().collect();
        if !saved.is_empty() {
            y = self.draw_group_header(
                &mut PaintCtx::new(scene, t, tr, theme),
                content,
                y,
                "Saved places",
                None,
            );
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            let last = saved.len() - 1;
            for (i, p) in saved.iter().enumerate() {
                let meta = distance(p.pos);
                let selected = self.model.selected.as_deref() == Some(p.id.as_str());
                y = self.draw_row(
                    &mut ctx,
                    content,
                    y,
                    ListRowFace::new(&p.name)
                        .secondary(p.category.label())
                        .meta(&meta),
                    selected,
                    i < last,
                    Hit::SelectPlace(p.id.clone()),
                );
            }
            y += s3;
        }

        if !self.model.recents.is_empty() {
            y = self.draw_group_header(
                &mut PaintCtx::new(scene, t, tr, theme),
                content,
                y,
                "Recent searches",
                Some(("Clear", Hit::ClearRecents)),
            );
            let recents = self.model.recents.clone();
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            let last = recents.len() - 1;
            for (i, q) in recents.iter().enumerate() {
                y = self.draw_row(
                    &mut ctx,
                    content,
                    y,
                    ListRowFace::new(q).leading(RowLeading::Icon(Icon::History)),
                    false,
                    i < last,
                    Hit::Recent(q.clone()),
                );
            }
            y += s3;
        }
        y
    }

    /// The Directions view: the From/To fields, the travel mode, the
    /// departure time, then the route (or, in Transit, the departures).
    /// Returns the y below the content.
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
        let sp = &theme.tokens.spacing;
        let (s2, s3, s4) = (f64::from(sp.s2), f64::from(sp.s3), f64::from(sp.s4));
        // Controls sit an `s2` in from the content edge, the rows' own inset.
        let inner = Rect::new(content.x0 + s2, content.y0, content.x1 - s2, content.y1);
        let x = inner.x0;
        let right = inner.x1;

        let selected_destination = self.model.selected_place().map(|place| place.name.as_str());
        let frame = EndpointFrame {
            content: inner,
            top: y + s2,
            origin: self.model.origin.and_then(|_| {
                (!self.model.origin_name.is_empty()).then_some(self.model.origin_name.as_str())
            }),
            destination: (!self.model.dest_name.is_empty())
                .then_some(self.model.dest_name.as_str())
                .or(selected_destination),
        };
        y = self.endpoints.draw_fields(scene, t, tr, theme, &frame);

        // "Use my location" — a real device-location fix for the origin; a
        // small secondary action under the fields.
        let small_h = ButtonFace::height(ControlSize::Small, theme);
        let (loc_label, loc_hit) = if self.locating {
            ("Locating…", None)
        } else {
            ("Use my location", Some(Hit::UseMyLocation))
        };
        let loc_face = ButtonFace::new(Rect::ZERO, loc_label).control_size(ControlSize::Small);
        let loc_w = loc_face.intrinsic_width(tr, theme);
        let loc = Rect::new(x, y, x + loc_w, y + small_h);
        self.text_button(
            &mut PaintCtx::new(scene, t, tr, theme),
            ButtonFace::new(loc, loc_label)
                .control_size(ControlSize::Small)
                .disabled(self.locating),
            loc,
            loc_hit,
        );
        y = loc.y1 + s4;

        // Travel mode: the primary choice of the view.
        let seg_h = SegmentedFace::height(ControlSize::Regular, theme);
        let seg_w = (right - x) / TravelMode::ALL.len() as f64;
        let segs: Vec<Rect> = (0..TravelMode::ALL.len())
            .map(|i| {
                let sx = x + i as f64 * seg_w;
                Rect::new(sx, y, sx + seg_w, y + seg_h)
            })
            .collect();
        let labels: Vec<&str> = TravelMode::ALL.iter().map(|m| m.label()).collect();
        let current = TravelMode::ALL
            .iter()
            .position(|m| *m == self.model.mode)
            .unwrap_or(0);
        let hovered = segs.iter().position(|r| self.hovered(*r));
        ToggleGroup::paint(
            scene,
            t,
            tr,
            theme,
            &SegmentedFace::new(&segs, &labels, current).hovered(hovered),
        );
        for (rect, mode) in segs.iter().zip(TravelMode::ALL) {
            self.click_targets.push((*rect, Hit::Mode(mode)));
        }
        y += seg_h + s3;

        // Departure time: a quiet refinement under the mode.
        y = self.draw_time_picker(scene, t, tr, theme, inner, y);

        // Transit plans a public-transport journey instead of a road route.
        if self.model.mode.is_transit() {
            return self.draw_departures(scene, t, tr, theme, content, y);
        }

        let Some(route) = self.model.route.clone() else {
            let body = theme.text(TextRole::Body);
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            draw::text_clip(
                &mut ctx,
                "Choose a destination to see the route.",
                Point::new(x, y),
                &body,
                theme.text_dim,
                right - x,
            );
            return y + draw::line_h(ctx.text, &body) + s2;
        };

        // The trip: how long it takes (the view's one hero number), how far,
        // and when it arrives.
        let display = theme.text(TextRole::Display).numeric();
        let body_numeric = theme.text(TextRole::Body).numeric();
        let duration = geo::fmt_duration(route.duration_s);
        let display_h = draw::line_h(tr, &display);
        let clear_face = ButtonFace::new(Rect::ZERO, "Clear route")
            .role(ButtonRole::Plain)
            .control_size(ControlSize::Small);
        let clear_w = clear_face.intrinsic_width(tr, theme);
        let clear = Rect::new(
            right - clear_w,
            y + (display_h - small_h) / 2.0,
            right,
            y + (display_h + small_h) / 2.0,
        );
        self.text_button(
            &mut PaintCtx::new(scene, t, tr, theme),
            ButtonFace::new(clear, "Clear route")
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small),
            clear,
            Some(Hit::ClearRoute),
        );
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        draw::text(
            &mut ctx,
            &duration,
            Point::new(x, y),
            &display,
            theme.text_primary,
        );
        let duration_w = draw::measure_w(ctx.text, &duration, &display);
        let body_h = draw::line_h(ctx.text, &body_numeric);
        draw::text_clip(
            &mut ctx,
            &geo::fmt_distance(route.distance_m),
            Point::new(x + duration_w + s3, y + (display_h - body_h) / 2.0),
            &body_numeric,
            theme.text_dim,
            clear.x0 - s2 - (x + duration_w + s3),
        );
        y += display_h;
        let (departure, arrival) = self.road_trip_clock(route.duration_s);
        let trip = if self.model.transit_time.is_now() {
            format!("Arrive at {arrival}")
        } else {
            format!("Leave at {departure}, arrive at {arrival}")
        };
        draw::text(
            &mut ctx,
            &trip,
            Point::new(x, y),
            &body_numeric,
            theme.text_dim,
        );
        y += body_h + s3;

        // Summary and steps are two groups: a derived rule, then the steps.
        ui::surface::separator(
            ctx.scene,
            t,
            x,
            right,
            y,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
        y += s2;
        let last = route.steps.len().saturating_sub(1);
        for (i, step) in route.steps.iter().enumerate() {
            let meta = if step.distance_m >= 1.0 {
                geo::fmt_distance(step.distance_m)
            } else {
                String::new()
            };
            let mut face =
                ListRowFace::new(&step.instruction).leading(RowLeading::Icon(step.maneuver.icon()));
            if !meta.is_empty() {
                face = face.meta(&meta);
            }
            let h = face.height(ctx.text, theme);
            let rect = Rect::new(content.x0, y, content.x1, y + h);
            let face = if i < last {
                face.separator(false)
            } else {
                face
            };
            face.paint(&mut ctx, rect);
            self.panel_edges.push(rect.y1 - self.panel_origin);
            y = rect.y1;
        }
        y + s3
    }

    /// The planned departure and arrival clocks for a road trip of
    /// `duration_s`, honouring the leave/arrive picker: "Leave" anchors the
    /// departure (arrival = departure + duration); "Arrive" anchors the
    /// arrival (departure = arrival − duration).
    fn road_trip_clock(&self, duration_s: f64) -> (String, String) {
        let tt = self.model.transit_time;
        let dur = chrono::Duration::seconds(duration_s.max(0.0) as i64);
        let anchor = ui::clock::local_now() + chrono::Duration::minutes(i64::from(tt.offset_min));
        let (dep, arr) = if tt.arrive_by() {
            (anchor - dur, anchor)
        } else {
            (anchor, anchor + dur)
        };
        (clock_label(dep), clock_label(arr))
    }

    /// The departure time: a quiet Leave/Arrive choice and a stepper that
    /// moves the time in quarter hours, with "Now" to go back. Defaults to
    /// leaving now. Returns the y below it.
    #[allow(
        clippy::too_many_lines,
        reason = "one row of related controls laid out against each other"
    )]
    fn draw_time_picker(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        inner: Rect,
        y: f64,
    ) -> f64 {
        let sp = &theme.tokens.spacing;
        let (s1, s4) = (f64::from(sp.s1), f64::from(sp.s4));
        let x = inner.x0;
        let right = inner.x1;
        let tt = self.model.transit_time;
        let row_h = SegmentedFace::height(ControlSize::Regular, theme);

        // Leave / Arrive: quiet, so the travel mode above reads first.
        let labels = ["Leave", "Arrive"];
        let toggle_w = SegmentedFace::intrinsic_width(&labels, ControlSize::Regular, tr, theme);
        let half = toggle_w / 2.0;
        let segs = [
            Rect::new(x, y, x + half, y + row_h),
            Rect::new(x + half, y, x + toggle_w, y + row_h),
        ];
        let current = usize::from(tt.mode == TransitTimeMode::Arrive);
        let hovered = segs.iter().position(|r| self.hovered(*r));
        ToggleGroup::paint(
            scene,
            t,
            tr,
            theme,
            &SegmentedFace::new(&segs, &labels, current)
                .style(SegmentedStyle::Quiet)
                .hovered(hovered),
        );
        self.click_targets
            .push((segs[0], Hit::TransitTimeMode(TransitTimeMode::Depart)));
        self.click_targets
            .push((segs[1], Hit::TransitTimeMode(TransitTimeMode::Arrive)));

        // The time stepper: earlier, the time, later.
        let side = ButtonFace::height(ControlSize::Regular, theme);
        let plus = Rect::new(
            right - side,
            y + (row_h - side) / 2.0,
            right,
            y + (row_h + side) / 2.0,
        );
        let target = ui::clock::local_now() + chrono::Duration::minutes(i64::from(tt.offset_min));
        let label = if tt.is_now() {
            "Now".to_string()
        } else {
            clock_label(target)
        };
        let style = theme.text(TextRole::Body).numeric();
        let label_w = draw::measure_w(tr, "00:00", &style).max(draw::measure_w(tr, &label, &style));
        let minus_x1 = plus.x0 - s1 - label_w - s1;
        let minus = Rect::new(minus_x1 - side, plus.y0, minus_x1, plus.y1);
        self.icon_button(
            &mut PaintCtx::new(scene, t, tr, theme),
            minus,
            (Icon::Minus, "Earlier"),
            ButtonRole::Plain,
            false,
            Hit::TransitTimeDelta(-15),
        );
        self.icon_button(
            &mut PaintCtx::new(scene, t, tr, theme),
            plus,
            (Icon::Plus, "Later"),
            ButtonRole::Plain,
            false,
            Hit::TransitTimeDelta(15),
        );
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let top = draw::vmid(ctx.text, y, row_h, &style);
        draw::text_center(
            &mut ctx,
            &label,
            Point::new((minus.x1 + plus.x0) / 2.0, top),
            &style,
            theme.text_primary,
        );
        let mut y = y + row_h;

        // A way back to "now" appears only when the time is off it.
        if !tt.is_now() {
            y += s1;
            let small_h = ButtonFace::height(ControlSize::Small, theme);
            let face = ButtonFace::new(Rect::ZERO, "Reset to now")
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small);
            let w = face.intrinsic_width(tr, theme);
            let reset = Rect::new(x, y, x + w, y + small_h);
            self.text_button(
                &mut PaintCtx::new(scene, t, tr, theme),
                ButtonFace::new(reset, "Reset to now")
                    .role(ButtonRole::Plain)
                    .control_size(ControlSize::Small),
                reset,
                Some(Hit::TransitTimeNow),
            );
            y += small_h;
        }
        y + s4
    }

    /// The public-transport departures for Transit: one row per journey (its
    /// times, length, changes and lines), the chosen one opened into its
    /// legs, and where the times come from.
    #[allow(
        clippy::too_many_lines,
        reason = "a single linear draw pass over this panel's fixed regions; splitting it would only \
              relocate the line count, not the amount a reader must hold in mind"
    )]
    #[allow(
        clippy::many_single_char_names,
        reason = "the shared drawing names (t: transform, x/y: the pen, s1–s4: spacing steps) \
              used consistently across every draw_* method in this file"
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
        let sp = &theme.tokens.spacing;
        let (s1, s2, s3) = (f64::from(sp.s1), f64::from(sp.s2), f64::from(sp.s3));
        let to = if self.model.dest_name.is_empty() {
            self.model
                .selected_place()
                .map_or_else(|| "your destination".to_string(), |p| p.name.clone())
        } else {
            self.model.dest_name.clone()
        };
        y = self.draw_group_header(
            &mut PaintCtx::new(scene, t, tr, theme),
            content,
            y,
            &format!("Departures to {to}"),
            None,
        );

        if self.model.transit.is_empty() {
            let has_target = self.model.dest.is_some() || self.model.selected_place().is_some();
            let msg = if self.pending > 0 {
                "Finding departures…"
            } else if !has_target {
                "Choose a destination to see departures."
            } else {
                "No public transport goes there at this time."
            };
            let body = theme.text(TextRole::Body);
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            draw::text_clip(
                &mut ctx,
                msg,
                Point::new(content.x0 + s2 + s3, y),
                &body,
                theme.text_dim,
                content.width() - 2.0 * (s2 + s3),
            );
            return y + draw::line_h(ctx.text, &body) + s2;
        }

        let title_style = theme.text(TextRole::Body).strong().numeric();
        let caption = theme.text(TextRole::Caption);
        let numeric = caption.clone().numeric();
        let badge = theme.text(TextRole::Caption).strong();
        let title_h = draw::line_h(tr, &title_style);
        let caption_h = draw::line_h(tr, &caption);
        let chip_h = caption_h + s1;
        let pad_x = s3;
        let journeys = self.model.transit.clone();
        let ground = theme.tokens.colors.surfaces.content;
        for (i, j) in journeys.iter().enumerate() {
            let selected = self.model.transit_choice == i;
            let row_h = s2 + title_h + s1 + chip_h + s2;
            let row = Rect::new(content.x0, y, content.x1, y + row_h);
            ui::chrome::paint_row_selection(
                scene,
                t,
                row,
                SelectionState::new(selected, self.hovered(row), false),
                ground,
                theme,
            );
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            let x = row.x0 + pad_x;
            let right = row.x1 - pad_x;
            let ty = row.y0 + s2;
            draw::text(
                &mut ctx,
                &format!("{}–{}", j.dep, j.arr),
                Point::new(x, ty),
                &title_style,
                theme.text_primary,
            );
            let meta_top = ty + (title_h - caption_h) / 2.0;
            draw::text_right(
                &mut ctx,
                &format!("{} min", j.duration_min),
                right,
                meta_top,
                &numeric,
                theme.text_dim,
            );
            // The journey's lines in their own colours, walks as plain text.
            let chips_y = ty + title_h + s1;
            let changes = match j.transfers {
                0 => "Direct".to_string(),
                1 => "1 change".to_string(),
                n => format!("{n} changes"),
            };
            let changes_w = draw::measure_w(ctx.text, &changes, &caption);
            draw::text_right(
                &mut ctx,
                &changes,
                right,
                chips_y + (chip_h - caption_h) / 2.0,
                &caption,
                theme.text_dim,
            );
            let chips_right = right - changes_w - s3;
            let mut px = x;
            let sep = f64::from(theme.tokens.metrics.icon_sm);
            for (li, leg) in j.legs.iter().enumerate() {
                let lead = if li > 0 { sep + s1 } else { 0.0 };
                if leg.mode == TransitMode::Walk {
                    let label = format!("Walk {} min", leg.duration_min.max(1));
                    let w = draw::measure_w(ctx.text, &label, &numeric);
                    if px + lead + w > chips_right {
                        break;
                    }
                    if li > 0 {
                        let r = Rect::new(
                            px,
                            chips_y + (chip_h - sep) / 2.0,
                            px + sep,
                            chips_y + (chip_h + sep) / 2.0,
                        );
                        Icon::ChevronRight.draw(ctx.scene, t, r, theme.text_tertiary);
                        px += lead;
                    }
                    draw::text(
                        &mut ctx,
                        &label,
                        Point::new(px, chips_y + (chip_h - caption_h) / 2.0),
                        &numeric,
                        theme.text_dim,
                    );
                    px += w + s1;
                } else {
                    let col = canvas::leg_color(theme, leg);
                    let fg = leg
                        .text_color
                        .as_deref()
                        .and_then(draw::hex_color)
                        .unwrap_or_else(|| theme.tokens.colors.on_surface(col).ink);
                    let label = if leg.line.is_empty() {
                        leg.mode.label().to_string()
                    } else {
                        leg.line.clone()
                    };
                    let w = draw::measure_w(ctx.text, &label, &badge) + 2.0 * s2;
                    if px + lead + w > chips_right {
                        break;
                    }
                    if li > 0 {
                        let r = Rect::new(
                            px,
                            chips_y + (chip_h - sep) / 2.0,
                            px + sep,
                            chips_y + (chip_h + sep) / 2.0,
                        );
                        Icon::ChevronRight.draw(ctx.scene, t, r, theme.text_tertiary);
                        px += lead;
                    }
                    let pill = Rect::new(px, chips_y, px + w, chips_y + chip_h);
                    // A line badge in the operator's own colour is content,
                    // like a calendar's event colour.
                    draw::fill_round(
                        ctx.scene,
                        t,
                        pill,
                        f64::from(theme.tokens.radii.detail),
                        col,
                    );
                    draw::text_center(
                        &mut ctx,
                        &label,
                        Point::new(pill.center().x, chips_y + (chip_h - caption_h) / 2.0),
                        &badge,
                        fg,
                    );
                    px += w + s1;
                }
            }
            self.click_targets.push((row, Hit::Journey(i)));
            self.mark_edge(row.y1);
            y = row.y1 + s1;

            // The chosen journey opened into its legs.
            if selected {
                let mut ctx = PaintCtx::new(scene, t, tr, theme);
                for leg in &j.legs {
                    let col = canvas::leg_color(theme, leg);
                    let (title, secondary) = if leg.mode == TransitMode::Walk {
                        (
                            format!("Walk to {}", leg.to_name),
                            format!("{} min", leg.duration_min.max(1)),
                        )
                    } else {
                        let line = format!("{} {}", leg.mode.label(), leg.line);
                        let title = if leg.headsign.is_empty() {
                            line
                        } else {
                            format!("{line} towards {}", leg.headsign)
                        };
                        let stops = if leg.num_stops == 1 {
                            "1 stop".to_string()
                        } else {
                            format!("{} stops", leg.num_stops)
                        };
                        (
                            title,
                            format!("{} to {}, {stops}", leg.from_name, leg.to_name),
                        )
                    };
                    let times = format!("{}–{}", leg.dep, leg.arr);
                    let face = ListRowFace::new(&title).secondary(&secondary).meta(&times);
                    let h = face.height(ctx.text, theme);
                    let rect = Rect::new(content.x0, y, content.x1, y + h);
                    // The leg's line colour down its leading edge ties the
                    // row to the line drawn on the map.
                    let stripe_x = rect.x0 + s1;
                    draw::line(
                        ctx.scene,
                        t,
                        Point::new(stripe_x, rect.y0 + s1),
                        Point::new(stripe_x, rect.y1 - s1),
                        f64::from(theme.tokens.metrics.chrome_rule_px) * 3.0,
                        col,
                    );
                    face.paint(&mut ctx, rect);
                    if leg.realtime {
                        let label = "Live";
                        let bw = BadgeFace::intrinsic_width(label, ctx.text, theme);
                        let bh = BadgeFace::height(theme);
                        let title_w =
                            draw::measure_w(ctx.text, &title, &theme.text(TextRole::Body));
                        let bx = (rect.x0 + s3 + title_w + s2).min(rect.x1 - bw - s3);
                        let by = rect.y0 + s2;
                        BadgeFace::new(Rect::new(bx, by, bx + bw, by + bh), label)
                            .style(ChipStyle::Tinted(Tone::Success))
                            .paint(ctx.scene, t, ctx.text, theme);
                    }
                    self.panel_edges.push(rect.y1 - self.panel_origin);
                    y = rect.y1;
                }
                y += s2;
            }
        }

        // Where the times come from, and whether they are live: Transitous
        // runs over the operators' aggregated GTFS(-RT) feeds; a leg flagged
        // real-time is live, a schedule-only feed reads "Scheduled".
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
        let source = match agency {
            Some(a) => format!("{freshness} from {a} via Transitous"),
            None => format!("{freshness} via Transitous"),
        };
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        y += s2;
        draw::text_clip(
            &mut ctx,
            &source,
            Point::new(content.x0 + s3, y),
            &caption,
            theme.text_dim,
            content.width() - 2.0 * s3,
        );
        y + caption_h + s2
    }

    /// Immediate-mode controls exported as one semantic list. Keeping this
    /// derived from the same hit targets used by pointer routing prevents the
    /// accessibility tree from advertising controls that are not on screen.
    #[allow(
        clippy::too_many_lines,
        reason = "the exhaustive mapping keeps each painted Maps command next to its stable semantic id, role, and derived state"
    )]
    fn accessibility_target_for_hit(&self, hit: &Hit) -> Option<AccessibilityTarget> {
        use ui::a11y::{AccessState, Role};

        let (label, target, role, state) = match hit {
            Hit::Consume | Hit::CloseNav => return None,
            Hit::OpenSearch => (
                "Search places".to_string(),
                "maps.search".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::ToggleCollapse => (
                if self.panel_collapsed {
                    "Expand panel"
                } else {
                    "Collapse panel"
                }
                .to_string(),
                "maps.panel-collapse".to_string(),
                Role::ToggleButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.panel_collapsed),
            ),
            Hit::PanelBack => (
                "Back to places".to_string(),
                "maps.panel.back".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::Mode(mode) => (
                format!("Travel by {}", mode.label()),
                format!("maps.mode:{}", mode.label().to_lowercase()),
                Role::RadioButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.model.mode == *mode),
            ),
            Hit::SelectPlace(id) => (
                self.model.place(id).map_or_else(
                    || "Open place".to_string(),
                    |place| format!("Open {}", place.name),
                ),
                format!("maps.place:{id}"),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::SelectPoi(id) => (
                self.pois.iter().find(|poi| poi.id == *id).map_or_else(
                    || "Open nearby place".to_string(),
                    |poi| format!("Open {}", poi.name),
                ),
                format!("maps.poi:{id}"),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::ClosePoi => (
                "Close place details".to_string(),
                "maps.poi.close".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::PoiDirections => (
                "Directions to this place".to_string(),
                "maps.poi.directions".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::Recent(query) => (
                format!("Search again for {query}"),
                format!("maps.recent:{query}"),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::ClearRecents => (
                "Clear search history".to_string(),
                "maps.recent.clear".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::ToggleNav => (
                if self.nav_open {
                    "Hide places"
                } else {
                    "Show places"
                }
                .to_string(),
                "maps.navigation".to_string(),
                Role::ToggleButton,
                AccessState::new().focusable(true).checked(self.nav_open),
            ),
            Hit::Directions => (
                "Directions".to_string(),
                "maps.directions".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::Transit => (
                "Transit".to_string(),
                "maps.transit".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::ToggleSave => (
                "Save or unsave place".to_string(),
                "maps.save".to_string(),
                Role::ToggleButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.model.selected_place().is_some_and(|place| place.saved)),
            ),
            Hit::ClearRoute => (
                "Clear route".to_string(),
                "maps.route.clear".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::UseMyLocation => (
                "Use my location".to_string(),
                "maps.location".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::TransitTimeMode(TransitTimeMode::Depart) => (
                "Leave at".to_string(),
                "maps.transit-time:depart".to_string(),
                Role::RadioButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.model.transit_time.mode == TransitTimeMode::Depart),
            ),
            Hit::TransitTimeMode(TransitTimeMode::Arrive) => (
                "Arrive by".to_string(),
                "maps.transit-time:arrive".to_string(),
                Role::RadioButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.model.transit_time.mode == TransitTimeMode::Arrive),
            ),
            Hit::TransitTimeDelta(minutes) => (
                if *minutes < 0 { "Earlier" } else { "Later" }.to_string(),
                format!("maps.transit-time:delta:{minutes}"),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::TransitTimeNow => (
                "Reset to now".to_string(),
                "maps.transit-time:now".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::Journey(index) => (
                format!("Journey option {}", index + 1),
                format!("maps.journey:{index}"),
                Role::RadioButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.model.transit_choice == *index),
            ),
            Hit::ToggleOverflow => (
                "More map options".to_string(),
                "maps.overflow".to_string(),
                Role::ToggleButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.overflow.is_open()),
            ),
        };
        Some(AccessibilityTarget {
            rect: Rect::ZERO,
            label,
            target,
            role,
            state,
        })
    }

    fn accessibility_targets(&self) -> Vec<AccessibilityTarget> {
        use crate::map_controls::MapControlAction;
        use ui::a11y::{AccessState, Role};

        let mut targets = vec![
            AccessibilityTarget {
                rect: self.map_controls.rect(MapControlAction::ZoomIn),
                label: "Zoom in".to_string(),
                target: "maps.control:zoom-in".to_string(),
                role: Role::Button,
                state: AccessState::new().focusable(true),
            },
            AccessibilityTarget {
                rect: self.map_controls.rect(MapControlAction::ZoomOut),
                label: "Zoom out".to_string(),
                target: "maps.control:zoom-out".to_string(),
                role: Role::Button,
                state: AccessState::new().focusable(true),
            },
            AccessibilityTarget {
                rect: self.map_controls.rect(MapControlAction::Locate),
                label: "Use my location".to_string(),
                target: "maps.control:locate".to_string(),
                role: Role::Button,
                state: AccessState::new().focusable(true),
            },
            AccessibilityTarget {
                rect: self.map_controls.rect(MapControlAction::ToggleTraffic),
                label: "Traffic".to_string(),
                target: "maps.control:traffic".to_string(),
                role: Role::Switch,
                state: AccessState::new()
                    .focusable(true)
                    .checked(self.map_controls.traffic_enabled()),
            },
        ];
        if self.history_rect.area() > 0.0 {
            targets.push(AccessibilityTarget {
                rect: self.history_rect,
                label: "History".to_string(),
                target: "maps.history".to_string(),
                role: Role::Button,
                state: AccessState::new().focusable(true),
            });
        }
        targets.retain(|target| target.rect.area() > 0.0);
        if self.navigation_end_rect.area() > 0.0 && self.model.route.is_some() {
            targets.push(AccessibilityTarget {
                rect: self.navigation_end_rect,
                label: "End navigation".to_string(),
                target: "maps.navigation.end".to_string(),
                role: Role::Button,
                state: AccessState::new().focusable(true),
            });
        }
        targets.extend(self.click_targets.iter().filter_map(|(rect, hit)| {
            self.accessibility_target_for_hit(hit)
                .and_then(|mut target| {
                    (rect.area() > 0.0).then(|| {
                        target.rect = *rect;
                        target
                    })
                })
        }));
        if self.overlay_mode() && self.nav_open {
            let panel = self.panel_rect();
            targets.retain(|target| {
                matches!(
                    target.target.as_str(),
                    "maps.navigation" | "maps.search" | "maps.history"
                ) || panel.contains(target.rect.center())
            });
        }
        targets
    }
}

// ===========================================================================
// Widget impl
// ===========================================================================
impl FloatingSurfaceProvider for MapsApp {
    fn take_floating_surface_request(
        &mut self,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.pending_floating.take()
    }

    fn draw_floating_surfaces(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _: &PresenceCtx,
    ) {
        if self.model.route.is_some()
            && matches!(
                self.navigation_placement.attachment(),
                ui::floating_surface::Attachment::Desktop { .. }
            )
        {
            self.draw_navigation_surface(scene, transform, text_renderer, theme);
        }
    }

    fn floating_surface_presentations(
        &self,
    ) -> Vec<ui::floating_surface::FloatingSurfacePresentation> {
        let Some(id) = self.active_navigation_request().map(|request| request.id) else {
            return Vec::new();
        };
        let returning_to_app = self.navigation_placement.is_return_pending();
        if !returning_to_app
            && !matches!(
                self.navigation_placement.attachment(),
                ui::floating_surface::Attachment::Desktop { .. }
            )
        {
            return Vec::new();
        }
        vec![ui::floating_surface::FloatingSurfacePresentation {
            id,
            content_rect: self.navigation_rect(),
            window_rect: self.navigation_rect().inflate(8.0, 8.0),
            returning_to_app,
        }]
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        self.active_navigation_request()
            .map(|request| request.id)
            .into_iter()
            .collect()
    }

    fn set_floating_surface_bounds(&mut self, bounds: Rect) {
        self.floating_surface_bounds = bounds;
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &ui::floating_surface::FloatingSurfaceHostMessage,
    ) {
        let Some(id) = self.active_navigation_request().map(|request| request.id) else {
            return;
        };
        if let ui::floating_surface::FloatingSurfaceHostMessage::PrepareSurface {
            id: request_id,
            content_rect,
            transfer_covered,
            coordinate_offset,
            ..
        } = message
            && request_id == &id
            && let Ok(rect) = ui::floating_surface::protocol_rect(*content_rect)
        {
            self.navigation_placement.prepare_desktop_rect_with_offset(
                rect,
                vello::kurbo::Vec2::new(coordinate_offset[0], coordinate_offset[1]),
            );
            self.navigation_placement
                .set_desktop_transfer_covered(*transfer_covered);
            return;
        }
        if let ui::floating_surface::FloatingSurfaceHostMessage::Adopted {
            id: request_id,
            rect,
        } = message
            && request_id == &id
            && ui::floating_surface::protocol_rect(*rect).is_ok()
        {
            self.navigation_placement.confirm_desktop_adoption();
            return;
        }
        if matches!(
            message,
            ui::floating_surface::FloatingSurfaceHostMessage::Reattach { id: request_id }
                | ui::floating_surface::FloatingSurfaceHostMessage::Closed { id: request_id }
                if request_id == &id
        ) {
            let app = self.rect;
            let desktop = self.desktop_floating_bounds();
            self.navigation_placement.reattach_to_app(app, desktop);
            return;
        }
        if matches!(
            message,
            ui::floating_surface::FloatingSurfaceHostMessage::Suspended { id: request_id }
                if request_id == &id
        ) {
            self.navigation_placement.confirm_app_adoption(self.rect);
        }
    }
}

impl Widget for MapsApp {
    ui::provides!(
        dyn ui::widget::FloatingSurfaceProvider,
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource,
        dyn ui::widget::controls::ActionControls
    );

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.history]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.history);
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.history]
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            "maps.locate",
            "maps.search",
            "maps.toggle-traffic",
            "maps.open-place",
            "maps.directions",
            "maps.open-poi",
            "maps.directions-poi",
        ]
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
        // Pull any background network results into the model first.
        self.drain_net(Some(state));
        // Fold in a device-location fix if the portal worker has one.
        self.drain_location();
        // Send the platform search's newest place query to the geocoder.
        self.pump_place_search();
        // Refetch viewport POIs once a pan/zoom settles.
        self.tick_poi_debounce(ui::time::now());
        // Fire the offline panel's live city geocode once its field settles.
        if let Some(query) = self.offline_panel.take_due_query(ui::time::now()) {
            self.pending += 1;
            if self.online {
                let tx = self.net_tx.clone();
                spawn_net(async move {
                    let results = net::geocode_regions(&query).await;
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

        if self.loaded {
            // A palette/agent/peer action mutates the CRDT outside this
            // widget's pointer path. Adopt durable resources; local camera,
            // panel, selection and ranked results never mirror back into it.
            MapsDoc::apply_durable(state, &mut self.model);
        } else {
            if MapsDoc::load(state).is_some() || self.actions.is_hosted() || !self.has_durable_map()
            {
                // The hosted record is the library. An empty live map stays
                // empty until a map action writes it. Sample places still
                // publish on the offline path.
                MapsDoc::apply_durable(state, &mut self.model);
            } else {
                self.publish_durable(state);
            }
            self.loaded = true;
        }
        self.endpoints
            .sync_queries(&self.model.origin_query, &self.model.dest_query);
        if let Some(effect) = self.endpoints.take_due_search(ui::time::now()) {
            self.apply_endpoint_effect(effect);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The text scale is read here, in the layout pass, so the panel
        // width, the navigation card's intrinsic size and every row solved
        // before the first paint already follow the type they will carry.
        self.text_scale = theme.text_scale;
        self.spacing = theme.tokens.spacing;
        self.metrics = theme.tokens.metrics;
        self.map_controls.set_metrics(ColumnMetrics::of(theme));
        self.nav_card_size = navigation_card_size(theme);
        self.navigation_placement
            .set_preferred_size(self.nav_card_size);
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
            self.poi_debounce = Some(ui::time::now() + POI_DEBOUNCE);
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

    /// The window's top layer: the offline-maps sheet and the information
    /// sheet, over the whole window and its chrome.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.offline_panel.is_open() {
            let window = presence.viewport.get().unwrap_or(self.rect);
            let viewport_bounds = self.viewport_bounds();
            self.offline_panel.draw(
                scene,
                transform,
                text_renderer,
                theme,
                OfflineFrame {
                    window,
                    viewport: self.rect,
                    viewport_bounds,
                    viewport_zoom: TileLayer::tile_zoom(self.model.zoom),
                    regions: &self.model.regions,
                    progress: self.tiles.progress(),
                    downloads_allowed: self.tiles.allows_prefetch(),
                    pointer: self.last_mouse,
                },
            );
        }
        self.info_sheet
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the Widget::handle_event trait method fans out on WindowEvent kind; each \
                  branch is a short, independent dispatch to an already-extracted helper"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // The shared information dialog is modal and owns its event routing.
        // Covered map controls must never observe the same event.
        if self
            .info_sheet
            .handle_event(event, ctx.scale_factor, *ctx.modifiers, ctx.state)
        {
            *ctx.handled = true;
            return;
        }
        // The History button takes its own pointer input.
        if matches!(
            event,
            WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
        ) && self.drive_history(event, ctx)
            && matches!(event, WindowEvent::MouseInput { .. })
        {
            *ctx.handled = true;
            return;
        }
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
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_mouse = p;
                if self.navigation_placement.is_dragging() {
                    let source_region = self.navigation_rect().inflate(8.0, 8.0);
                    let handoff = self.navigation_placement.update_drag(
                        p,
                        self.rect,
                        self.desktop_floating_bounds(),
                    );
                    if handoff == ui::floating_surface::Handoff::Detached {
                        self.pending_floating = self.active_navigation_request_from(source_region);
                    }
                    *ctx.handled = true;
                    return;
                }
                let end_hovered =
                    self.model.route.is_some() && self.navigation_end_rect.contains(p);
                if self.navigation_end_hovered != end_hovered {
                    self.navigation_end_hovered = end_hovered;
                    *ctx.handled = true;
                }
                // Keep the field inputs' hover state current so a click lands a
                // caret and a press-drag extends a selection.
                self.forward_move_to_field_inputs(event, ctx.scale_factor, *ctx.modifiers);
                // Track hover in the open overflow menu / offline city dropdown.
                if self.overflow.is_open() {
                    self.drive_overflow(event, ctx.scale_factor, *ctx.modifiers);
                }
                if self.offline_panel.is_open() {
                    self.offline_panel.route_field_event(
                        event,
                        ctx.scale_factor,
                        *ctx.modifiers,
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
                        *ctx.handled = true;
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
                    .on_pointer(p, self.left_down, extra_targets, ui::time::now())
                {
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_mouse;
                if self.model.route.is_some() && self.navigation_end_rect.contains(p) {
                    self.clear_route(ctx.state);
                    self.navigation_end_hovered = false;
                    *ctx.handled = true;
                    return;
                }
                if self.model.route.is_some() && self.navigation_drag_handle.contains(p) {
                    let rect = self.navigation_rect();
                    self.navigation_placement.set_preferred_size(rect.size());
                    self.navigation_placement.begin_drag(
                        p,
                        self.rect,
                        self.desktop_floating_bounds(),
                    );
                    *ctx.handled = true;
                    return;
                }
                self.left_down = true;
                self.drag_moved = false;
                // The open ⋯ overflow menu owns the pointer: a row press selects,
                // an outside press dismisses. Route to it before the map/chrome.
                if self.overflow.is_open() {
                    self.drive_overflow(event, ctx.scale_factor, *ctx.modifiers);
                    if let Some(idx) = self.overflow.take_selection() {
                        self.overflow_select(idx);
                    }
                    *ctx.handled = true;
                    return;
                }
                // The offline dialog's city field / dropdown takes its own clicks
                // first; a click elsewhere in the dialog falls through to the
                // buttons below (download / clear / delete / close / scrim).
                if self.offline_panel.is_open() {
                    if self.offline_panel.route_field_event(
                        event,
                        ctx.scale_factor,
                        *ctx.modifiers,
                        self.last_mouse,
                    ) {
                        *ctx.handled = true;
                        return;
                    }
                    if let Some(action) = self.offline_panel.action_at(p) {
                        self.dispatch_offline(action, ctx.state);
                        *ctx.handled = true;
                        return;
                    }
                }
                if matches!(self.model.panel, Panel::Directions)
                    && let Some(effect) =
                        self.endpoints
                            .pointer_press(p, event, ctx.scale_factor, *ctx.modifiers)
                {
                    self.apply_endpoint_effect(effect);
                    *ctx.handled = true;
                    return;
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
                    self.endpoints.dismiss();
                    self.dispatch(&hit, ctx.state);
                    consumed = true;
                    *ctx.handled = true;
                }
                if !consumed {
                    self.endpoints.dismiss();
                    // The standalone control stack is below modal/panel targets
                    // above, but above the actual map surface.
                    if let Some(action) = self.map_controls.hit_test(p) {
                        self.dispatch_map_control(action);
                        *ctx.handled = true;
                    } else if let Some(id) = self.marker_at(p) {
                        // Map interaction: marker select, POI select, or begin pan.
                        self.select_place(&id);
                        *ctx.handled = true;
                    } else if let Some(pid) = self.poi_at(p) {
                        self.select_poi(&pid);
                        *ctx.handled = true;
                    } else {
                        // Empty map: close any open POI card and begin a pan.
                        // Claim the press so the compositor/ui App treats the map
                        // as the interaction target for the gesture that follows.
                        self.selected_poi.take();
                        self.dragging = true;
                        self.drag_last = p;
                        *ctx.handled = true;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let navigation_dragging = self.navigation_placement.is_dragging();
                self.navigation_placement
                    .end_drag(self.desktop_floating_bounds());
                self.left_down = false;
                self.dragging = false;
                if navigation_dragging {
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    MouseScrollDelta::PixelDelta(pos) => pos.y / 40.0,
                };
                let over_panel = self.panel_visible()
                    && self.panel_rect().contains(self.last_mouse)
                    && self.last_mouse.y
                        > self.panel_rect().y0 + f64::from(self.metrics.bar_height);
                if over_panel {
                    self.scroll_panel(dy);
                } else {
                    self.zoom_by(dy * 0.35, self.last_mouse);
                }
                *ctx.handled = true;
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
                    self.drive_overflow(event, ctx.scale_factor, *ctx.modifiers);
                    if let Some(idx) = self.overflow.take_selection() {
                        self.overflow_select(idx);
                    }
                    *ctx.handled = true;
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
                            ctx.scale_factor,
                            *ctx.modifiers,
                            self.last_mouse,
                        );
                    }
                    *ctx.handled = true;
                    return;
                }
                // A focused text field (search or an endpoint) owns the
                // keystroke via the shared TextInput; otherwise the map-level
                // shortcuts apply.
                let field_focused = self.endpoints.is_focused();
                if field_focused {
                    if self.route_field_key(event, ctx.scale_factor, *ctx.modifiers) {
                        *ctx.handled = true;
                    }
                } else if self.handle_key(&key_event.logical_key, *ctx.modifiers, ctx.state) {
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "MapsApp"
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<InvocationUpdate> {
        if self.location_invocation != Some(invocation) {
            return None;
        }
        let terminal = self.location_terminal.take()?;
        self.location_invocation = None;
        Some(terminal)
    }

    fn test_id(&self) -> Option<&str> {
        Some("maps_app")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn wants_pointer_capture(&self) -> bool {
        self.navigation_placement.is_dragging()
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.info_sheet
            .modal_capture()
            .or_else(|| self.offline_panel.modal_capture())
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        if let Some(dialog) = self.info_sheet.accessibility() {
            return Some(dialog);
        }
        if let Some(dialog) = self.offline_panel.accessibility() {
            return Some(dialog);
        }
        let children = self
            .accessibility_targets()
            .into_iter()
            .map(|target| {
                let key = stable_access_key(&target.target);
                ui::a11y::AccessNode::new(target.role)
                    .with_name(target.label)
                    .with_state(target.state)
                    .with_key(key)
                    .with_action_target(target.target)
                    .with_bounds(target.rect)
            })
            .collect();
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Group)
                .with_name("Maps")
                .with_children(children),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        if self.offline_panel.is_open() {
            return self.offline_panel.accessibility_rect(path);
        }
        let [index] = path else {
            return None;
        };
        self.accessibility_targets()
            .get(*index)
            .map(|target| target.rect)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, _) = ui::doc::document(state, path);
        if self.offline_panel.is_open() {
            // Text input needs the framework's exact-rectangle pointer
            // fallback so it can place its caret. All buttons dispatch through
            // the same effect path as pointer input.
            if target == "maps.offline.search" {
                return false;
            }
            if matches!(action, ui::a11y::Action::Activate)
                && let Some(action) = self.offline_panel.semantic_action(target)
            {
                self.dispatch_offline(action, state);
                return true;
            }
            return false;
        }
        if !matches!(
            action,
            ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck
        ) {
            return false;
        }
        let control = match target {
            "maps.control:zoom-in" => Some(MapControlAction::ZoomIn),
            "maps.control:zoom-out" => Some(MapControlAction::ZoomOut),
            "maps.control:locate" => Some(MapControlAction::Locate),
            "maps.control:traffic" => Some(MapControlAction::ToggleTraffic),
            _ => None,
        };
        if let Some(control) = control {
            self.dispatch_map_control(control);
            return true;
        }
        if target == "maps.navigation.end" {
            self.clear_route(state);
            return true;
        }
        // The History button is a real toolkit button: the framework's
        // exact-rect press reaches it through `handle_event`.
        if target == "maps.history" {
            return false;
        }
        let hit = self.click_targets.iter().find_map(|(_, hit)| {
            (self
                .accessibility_target_for_hit(hit)
                .is_some_and(|candidate| candidate.target == target))
            .then(|| hit.clone())
        });
        if let Some(hit) = hit {
            self.dispatch(&hit, state);
            true
        } else {
            false
        }
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
            || self.place_search.is_busy()
            || !self.loaded
            || self.locating
            || self.location_rx.is_some()
            || self.poi_debounce.is_some()
            || self.tiles.has_inflight()
            || self.tiles.progress().is_active()
    }

    fn needs_animation_frame(&self) -> bool {
        // Only motion takes a frame every vsync. Network replies, tiles and
        // download ticks wake the app when they land, however long a slow or
        // failing service keeps them open; debounces are wake deadlines; the
        // search caret does not blink.
        self.locating
            || self.location_rx.is_some()
            // Keep pumping frames while a control tooltip counts toward its
            // reveal, so the dwell elapses even when the map is otherwise idle.
            || self.navigation_placement.is_dragging()
            || (!self.motion_reduced && self.navigation_placement.is_handoff_animating())
            || self.map_controls.needs_frame()
            || self.status_bubble.needs_frame(ui::time::now())
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        [
            self.endpoints.query_deadline(),
            self.offline_panel.query_deadline(),
            self.poi_debounce,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.motion_reduced = reduced;
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

impl ui::resources::PublishesResources for MapsApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the workspace door — keep only live search hits and POIs.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        use ui::tilo_resource::Resource as _;
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        let mut resources = self.publish_resources(doc);
        if resources.is_empty() {
            let state = crate::resource::Maps::read(doc).unwrap_or_default();
            resources.push(ui::tilo_resource::publish::<crate::resource::Maps>(
                "maps.workspace",
                "Maps",
                &state,
            ));
        }
        if let Some(workspace) = resources.first_mut() {
            workspace
                .children
                .extend(self.live_search_and_poi_resources_undecorated());
        }
        for resource in &mut resources {
            self.decorate_live_resource(resource);
        }
        resources
    }
    fn action_directory_resource_page(
        &self,
        doc: &AutoCommit,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<ui::widget::ActionDirectoryResourcePage> {
        let resources = if cursor.is_empty() && max_results > 0 {
            self.resources(doc)
        } else {
            Vec::new()
        };
        Some(ui::widget::ActionDirectoryResourcePage {
            resources,
            next_cursor: Vec::new(),
            exhausted: true,
        })
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        use ui::tilo_resource::Resource as _;
        // The framework root already owns the workspace door.
        if self.is_hosted() && id == "maps.workspace" {
            return None;
        }
        let mut resource = self
            .publish_resource(doc, id)
            .or_else(|| {
                (id == "maps.workspace").then(|| {
                    let state = crate::resource::Maps::read(doc).unwrap_or_default();
                    ui::tilo_resource::publish::<crate::resource::Maps>(
                        "maps.workspace",
                        "Maps",
                        &state,
                    )
                })
            })
            .or_else(|| {
                id.strip_prefix("maps.search-result:").and_then(|place_id| {
                    self.model
                        .results
                        .iter()
                        .find(|place| place.id == place_id)
                        .map(|place| Resource::new(id, "maps.search-result", place.name.clone()))
                })
            })
            .or_else(|| {
                id.strip_prefix("maps.poi:").and_then(|poi_id| {
                    self.pois
                        .iter()
                        .find(|poi| poi.id == poi_id)
                        .map(|poi| Resource::new(id, "maps.poi", poi.name.clone()))
                })
            })?;
        self.decorate_live_resource(&mut resource);
        Some(resource)
    }
}

impl ui::search::capability::SearchSource for MapsApp {
    /// The places the map knows, the points of interest around the view and
    /// the recent searches answer at once; the geocoder answers later.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.place_search.provider(self.local_places()))]
    }

    /// Show the place a hit names, as a click on it in the panel would.
    fn activate_search_result(&mut self, action_id: &str, doc: &mut AppModel, path: &ModelPath) {
        let Some(hit) = PlaceHit::parse(action_id) else {
            return;
        };
        let (state, _) = ui::doc::document(doc, path);
        self.open_search_hit(hit, state);
    }
}

impl ui::widget::controls::ActionControls for MapsApp {
    /// Search opened from the painted search buttons and recent searches.
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.search_opener
            .take()
            .or_else(|| ui::widget::controls::take_control_invocation(&mut self.history))
    }
}

impl ui::undo::CommittedActions for MapsApp {
    /// Drain the undo boundary the [`ActionHost`](ui::ActionHost) bracketed for
    /// this event so the shell records exactly one undo point per dispatched
    /// action — the same path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl MapsApp {
    fn answer_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        match (kind, type_id) {
            (OperationKind::View, "maps.search") => {
                let query: String =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.model.query = query;
                self.run_search(None);
            }
            (OperationKind::Effect, "maps.locate") => {
                if self.location_rx.is_some() {
                    return Err("A location request is already in progress".into());
                }
                self.use_my_location();
            }
            (OperationKind::View, "maps.toggle-traffic") => {
                self.set_traffic(!self.map_controls.traffic_enabled());
            }
            (OperationKind::View, "maps.open-place" | "maps.directions") => {
                let id: String =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                if self.model.place(&id).is_none() {
                    return Err("Map place is no longer available".into());
                }
                self.select_place(&id);
                if type_id == "maps.directions" {
                    self.enter_directions();
                }
            }
            (OperationKind::View, "maps.open-poi" | "maps.directions-poi") => {
                let id: String =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                if !self.pois.iter().any(|poi| poi.id == id) {
                    return Err("Map point of interest is no longer in this viewport".into());
                }
                self.select_poi(&id);
                if type_id == "maps.directions-poi" {
                    self.poi_directions();
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: InvocationId,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        if (kind, type_id) != (OperationKind::Effect, "maps.locate") {
            return self
                .answer_resource_operation(kind, type_id, payload, doc, path)
                .map(|handled| {
                    handled.then(|| {
                        ResourceOperationStart::Completed(ui::tilo_actions::ActionOutput::default())
                    })
                });
        }
        if !self.online {
            return Err("The location service is unavailable in this session".into());
        }
        if self.location_invocation.is_some() || self.location_rx.is_some() {
            return Err("A location request is already in progress".into());
        }
        self.location_invocation = Some(invocation);
        self.location_terminal = None;
        self.use_my_location();
        Ok(Some(ResourceOperationStart::Accepted))
    }
}

/// A location request answered by a recording host with `outcome` — the
/// production request path, without the device's location service.
#[cfg(test)]
fn host_location(outcome: LocationOutcome) -> LocationChannel {
    let (services, recorder) = ui::host::HostServices::recording();
    recorder.respond_to_location(outcome);
    let channel = ui::host::scoped(&services, location::request_location);
    assert_eq!(recorder.location_requests(), 1);
    channel
}

#[cfg(test)]
mod tests;
