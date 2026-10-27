//! Root widget for the Maps app.
//!
//! `MapsApp` owns the in-memory [`MapModel`] and the static vector [`Basemap`].
//! [`crate::workspace::Workspace`] holds its explicitly durable resources;
//! the rest is per-window presentation state. It
//! is a single full-window custom widget: the map is full-bleed and the
//! chrome (the places panel, the control column, the place card) floats on
//! top as floating panels built from the toolkit's faces. Interaction —
//! pan, zoom-to-cursor, marker hit-testing, panel buttons — is handled
//! directly in `handle_event`; finding places is the platform search, which
//! [`crate::place_search`] answers.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use ui::time::Instant;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::TextKind;
use ui::chrome::SelectionState;
use ui::format::memory as format_bytes;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::ActionOutput;
use ui::tokens::spacing::Spacing;
use ui::tokens::{Metrics, TextRole};
use ui::widget::{
    Button, ButtonFace, ButtonRole, CardFace, ChipStyle, ContextMenu, ControlSize, EmptyState,
    EmptyStateContent, Interaction, ListRowFace, MenuItem, NumberStepperFace, PropertyListFace,
    PropertyRow, RowLeading, SegmentedFace, SegmentedStyle, SegmentedTouch, SegmentedTouchGroup,
    SegmentedTouchResult, ToggleGroup, Tone,
};
use ui::widget::{FloatingSurfaceProvider, Widget};

use crate::canvas::{self, MapPalette, MapView};
use crate::draw;
use crate::endpoint_editor::GEOCODER;
use crate::endpoint_editor::{Endpoint, EndpointEditor, EndpointEffect, EndpointFrame};
use crate::failure;
use crate::geo::{self, Projection};
use crate::map_controls::{ColumnMetrics, MapControlAction, MapControls};
use crate::map_info_sheet::{MapInfo, MapInfoSheet};
use crate::model::{LatLon, MapModel, Panel, Place, TransitMode, TransitTimeMode, TravelMode};
use crate::net::{self, NetMsg};
use crate::offline_panel::{OfflineAction, OfflineFrame, OfflinePanel, areas_refused};
use crate::place_search::PlaceSearch;
use crate::sample::{self, Basemap};
use crate::tiles::{GeoBounds, TileFault, TileId, TileLayer, TileStyle};
use crate::traffic::{Congestion, TrafficData};
use crate::workspace::{Workspace, workspace};
use ui::location::{self, LocationChannel, LocationOutcome};

mod a11y;
mod dispatch;
mod frame;
mod interaction;
mod layout;
mod nav_surface;
mod offline;
mod places;
mod places_panel;
mod resource_surface;
mod results;
mod route;
mod sheets;
mod stage;
mod state;

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
/// The menu the map's one [`ContextMenu`] shows: the ⋯ button's utilities or
/// the layers button's layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MapMenu {
    More,
    Layers,
}
/// A clickable hot-region recorded during draw and hit-tested on click.
#[derive(Clone, Debug)]
enum Hit {
    /// Swallow the click (panel chrome) — don't pan the map.
    Consume,
    /// Open the platform search over this app.
    OpenSearch,
    /// The separate search action in the empty places panel.
    OpenEmptySearch,
    /// Collapse / expand the side panel (docked layouts).
    ToggleCollapse,
    /// Return from the Directions view to the plain places list.
    PanelBack,
    Mode(TravelMode),
    SelectPlace(String),
    /// Select a nearby POI from the Explore list: centre the map on it and open
    /// its info card, linking the list to the marker layer.
    SelectPoi(String),
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
    /// Look at the next route the service offered for this trip.
    NextRoute,
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
/// What the map says while no imagery shows. It never claims to be loading
/// what nothing is fetching, and an unreachable tile server is not blamed on
/// being offline, which a page cannot tell apart from a blocked request.
fn imagery_hint(can_fetch: bool, fault: Option<TileFault>) -> &'static str {
    match fault {
        _ if !can_fetch => "Map imagery can't be fetched here",
        Some(TileFault::Rejected) => "The map tile server refused the request",
        Some(TileFault::Unreachable) => {
            "The map tile server couldn't be reached. Trying again shortly."
        }
        None => "Loading map…",
    }
}

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

    /// The framework's door for the map's edits (save a place, clear the
    /// route): one undo step each, replayed byte-identically on peers — the
    /// same path a Ctrl+K palette dispatch takes. `None` until the surface is
    /// attached; an unattached map only shows its edits.
    dispatch: Option<ui::framework::Dispatcher<Workspace>>,
    /// This device's offline areas, as stored; `None` when the map has no
    /// host to keep them.
    offline_areas: Option<ui::framework::Setting<crate::offline_areas::OfflineAreas>>,

    // interaction state (render/UI caches only — app state lives in `model`)
    last_mouse: Point,
    segmented_touch: SegmentedTouch,
    /// Floating zoom/location/traffic stack. Owns geometry, drawing,
    /// hit-testing, traffic UI state, and shared tooltip timing.
    map_controls: MapControls,
    /// The geocoder lookups behind the platform search: the query to send,
    /// the ones on their way, and the places that came back.
    place_search: PlaceSearch,
    /// Opens the platform search from the painted search buttons.
    search_opener: ui::search::SearchOpener,
    /// The window's History control, which heads the map control column:
    /// the full-bleed map is the window's header row.
    history: ui::widget::HistoryOpener,
    /// Where the listed search results were ranked from: their distances
    /// read from there, so moving the camera to a result never changes them.
    results_anchor: Option<LatLon>,
    /// The floating surfaces drawn over the map this frame (the panel, the
    /// controls, the cards, the footer); the markers keep clear of them.
    chrome_rects: Vec<Rect>,
    /// Transient status (searching, routing, a download, a failure).
    /// The model's status (searching, routing, a download's progress, a
    /// recoverable failure), said in the window's bar while it is news.
    status_notice: ui::widget::toast::Transient<String>,
    /// The status last said in the bar, so only a change is news.
    status_said: String,
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
    /// Whole-interface scale, also used by marker hit-testing and decluttering.
    ui_scale: f64,
    /// The theme's spacing and density metrics, cached in `build_layout` so
    /// the renderer-less geometry reads the same tokens the painters do.
    spacing: Spacing,
    metrics: Metrics,
    /// Frame the route (or journey) once the window has its size: a staged
    /// or restored route fits the map beside the panel and the turn card.
    pending_fit: bool,
    /// A place to bring to the centre of the visible map once the window has
    /// its size.
    pending_focus: Option<LatLon>,
    /// Bring the place whose card is open clear of the card on the next
    /// frame, once the card is laid out.
    pending_poi_focus: bool,
    /// Where the open place card was drawn this frame.
    poi_card_rect: Rect,
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

    // networking
    net_tx: NetSender,
    net_rx: Receiver<NetMsg>,
    pending: u32,
    /// The places of a committed search on their way from the geocoder:
    /// the query and when it was asked. Their placeholder rows stand under
    /// the results until they land.
    search_asked: Option<(String, Instant)>,
    /// Whether background network/portal work may be spawned. False for the
    /// offline `--shots`/test app so it never touches the wire or D-Bus.
    online: bool,

    // device location (portal / GeoClue), off the UI thread
    location_rx: Option<LocationChannel>,
    /// True while a location fix is being acquired (drives the "Locating…" UI).
    locating: bool,
    /// The answer owed to whoever invoked the locate effect: the portal's
    /// result, rather than its admission, finishes it.
    location_reply: Option<ui::framework::EffectReply>,

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

    // bottom-right ⋯ overflow menu (attribution / offline maps / licenses), or
    // the layers button's menu of layers — a shared ContextMenu so it is
    // themed, keyboard-navigable and a11y-correct.
    overflow: ContextMenu,
    /// Which menu `overflow` shows now: the ⋯ utilities or the map's layers.
    menu_for: MapMenu,
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

/// A search result's row: the place's name, its address or kind under it.
const RESULT_ROW: ui::widget::RowShape = ui::widget::RowShape::TITLE.with_secondary(1);
/// How many rows a committed search reserves for the geocoder's places.
const ARRIVING_RESULTS: usize = 3;

impl MapsApp {}

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
    let summary_row = ButtonFace::height(ControlSize::Small, theme).max(line(TextRole::Heading));
    let height = f64::from(sp.s3) * 2.0
        + line(TextRole::Heading)
        + line(TextRole::Body)
        + f64::from(sp.s3) * 2.0
        + summary_row;
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

/// A travel time read as a clock: `10:57` or `10:57 AM`.
fn clock_label(at: chrono::DateTime<chrono::Local>) -> String {
    ui::clock::time_of_day(&at)
}

// ===========================================================================
// Drawing
// ===========================================================================
impl MapsApp {}

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
            input_rect: Some(self.navigation_rect()),
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
        dyn ui::widget::WindowHeader,
        dyn ui::widget::controls::ActionControls
    );

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        // Pull any background network results into the model first.
        self.drain_net();
        // Fold in a device-location fix if the portal worker has one.
        self.drain_location();
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

        // Another window may have downloaded or deleted an offline area.
        self.follow_offline_areas();
        self.endpoints
            .sync_queries(&self.model.origin_query, &self.model.dest_query);
        if let Some(effect) = self.endpoints.take_due_search(ui::time::now()) {
            self.apply_endpoint_effect(effect);
        }
        self.publish_status();
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
        let scale_changed = (self.ui_scale - f64::from(theme.ui_scale)).abs() > f64::EPSILON
            || (self.text_scale - theme.text_scale).abs() > f32::EPSILON;
        if scale_changed {
            // New chrome dimensions change the unobscured map area. Fit the
            // existing plan again rather than leaving its ends under a panel.
            self.pending_fit = self.model.route.is_some() || self.model.mode.is_transit();
            self.pending_poi_focus = self.selected_poi.is_some();
            self.scroll = 0.0;
        }
        self.text_scale = theme.text_scale;
        self.ui_scale = f64::from(theme.ui_scale);
        self.spacing = theme.tokens.spacing;
        self.metrics = theme.tokens.metrics;
        self.map_controls.set_metrics(ColumnMetrics::of(theme));
        self.nav_card_size = navigation_card_size(theme);
        self.navigation_placement
            .set_preferred_size(self.nav_card_size);
        let node = taffy.fill_leaf(node_map, self);
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
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
        if self.overflow.is_open() {
            self.overflow
                .draw_overlay(scene, transform, text_renderer, theme, presence);
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
            .handle_event(event, ctx.scale_factor, *ctx.modifiers, &mut ctx.state)
        {
            *ctx.handled = true;
            return;
        }
        if let WindowEvent::Touch(touch) = event
            && self.handle_segmented_touch(touch, ctx.scale_factor)
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
                let p = ctx.logical(position);
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
                self.forward_move_to_field_inputs(
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    &mut ctx.state,
                );
                // Track hover in the open overflow menu / offline city dropdown.
                if self.overflow.is_open() {
                    self.drive_overflow(event, ctx.scale_factor, *ctx.modifiers, &mut ctx.state);
                }
                if self.offline_panel.is_open() {
                    self.offline_panel.route_field_event(
                        event,
                        ctx.scale_factor,
                        *ctx.modifiers,
                        self.last_mouse,
                        &mut ctx.state,
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
                    self.clear_route();
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
                    self.drive_overflow(event, ctx.scale_factor, *ctx.modifiers, &mut ctx.state);
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
                        &mut ctx.state,
                    ) {
                        *ctx.handled = true;
                        return;
                    }
                    if let Some(action) = self.offline_panel.action_at(p) {
                        self.dispatch_offline(action);
                        *ctx.handled = true;
                        return;
                    }
                }
                if matches!(self.model.panel, Panel::Directions)
                    && let Some(effect) = self.endpoints.pointer_press(
                        p,
                        event,
                        ctx.scale_factor,
                        *ctx.modifiers,
                        &mut ctx.state,
                    )
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
                    self.dispatch(&hit);
                    consumed = true;
                    *ctx.handled = true;
                }
                if !consumed {
                    self.endpoints.dismiss();
                    // The standalone control stack is below modal/panel targets
                    // above, but above the actual map surface.
                    if let Some(action) = self.map_controls.hit_test(p) {
                        self.dispatch_map_control(action);
                    } else if let Some(id) = self.marker_at(p) {
                        // Map interaction: marker select, POI select, or begin pan.
                        self.select_place(&id);
                    } else if let Some(pid) = self.poi_at(p) {
                        self.select_poi(&pid);
                    } else {
                        // Empty map: close any open POI card and begin a pan.
                        // Claim the press so the compositor/ui App treats the map
                        // as the interaction target for the gesture that follows.
                        self.selected_poi.take();
                        self.dragging = true;
                        self.drag_last = p;
                    }
                    *ctx.handled = true;
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
                    self.drive_overflow(event, ctx.scale_factor, *ctx.modifiers, &mut ctx.state);
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
                            &mut ctx.state,
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
                    if self.route_field_key(event, ctx.scale_factor, *ctx.modifiers, &mut ctx.state)
                    {
                        *ctx.handled = true;
                    }
                } else if self.handle_key(&key_event.logical_key, *ctx.modifiers) {
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "MapsApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("maps_app")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn claims_raw_touch(&self, point: Point) -> bool {
        let (modes, times) = self.segmented_rects();
        modes.iter().chain(&times).any(|rect| rect.contains(point))
    }

    fn wants_pointer_capture(&self) -> bool {
        self.navigation_placement.is_dragging() || self.segmented_touch.is_active()
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
        _state: &mut ui::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
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
                self.dispatch_offline(action);
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
        if self.history.perform_semantic_action(target, action) {
            return true;
        }
        let control = match target {
            "maps.control:zoom-in" => Some(MapControlAction::ZoomIn),
            "maps.control:zoom-out" => Some(MapControlAction::ZoomOut),
            "maps.control:locate" => Some(MapControlAction::Locate),
            "maps.control:layers" => Some(MapControlAction::Layers),
            _ => None,
        };
        if let Some(control) = control {
            self.dispatch_map_control(control);
            return true;
        }
        if target == "maps.navigation.end" {
            self.clear_route();
            return true;
        }
        let hit = self.click_targets.iter().find_map(|(_, hit)| {
            (self
                .accessibility_target_for_hit(hit)
                .is_some_and(|candidate| candidate.target == target))
            .then(|| hit.clone())
        });
        if let Some(hit) = hit {
            self.dispatch(&hit);
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
            || self.navigation_placement.is_handoff_animating()
            || self.map_controls.needs_frame()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        [
            self.endpoints.query_deadline(),
            self.offline_panel.query_deadline(),
            self.poi_debounce,
            self.status_notice.deadline(),
            self.search_clock()
                .and_then(|clock| clock.next_step_at(ui::time::now())),
            self.endpoints.arriving_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.motion_reduced = reduced;
        self.navigation_placement.set_reduce_motion(reduced);
    }
}

impl ui::widget::controls::ActionControls for MapsApp {
    /// History opened from the control column; search opened from the
    /// painted search buttons and recent searches.
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.history.take().or_else(|| self.search_opener.take())
    }
}

/// The map is the window's header row: the History control the host offers
/// heads the map control column, so the window draws no History bar above
/// the map.
impl ui::widget::WindowHeader for MapsApp {
    fn set_history_control(&mut self, control: Option<ui::widget::HistoryControl>) {
        self.history.set_control(control);
        self.map_controls.set_history(control);
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

#[cfg(test)]
mod search_arrival_tests;
