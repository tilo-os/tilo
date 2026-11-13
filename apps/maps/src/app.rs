//! Root widget for the Maps app.
//!
//! `MapsApp` owns the in-memory [`MapModel`] and the vector [`Basemap`];
//! [`crate::workspace::Workspace`] holds the durable resources, and the rest is this window's
//! state.

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

use crate::basemap::Basemap;
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
#[cfg(feature = "samples")]
use crate::sample;
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
mod results;
mod route;
mod sheets;
#[cfg(feature = "samples")]
mod stage;

mod state;
mod window;

#[cfg(test)]
use window::MapWindow;
pub(crate) use window::window;

/// One semantic live-activity identity for the current turn-by-turn session. Rerouting changes its
/// content, not the surface the user positioned.
const NAVIGATION_FLOAT_ID: &str = "maps.active-navigation";
/// Idle gap after the last pan/zoom before refetching viewport POIs (Overpass).
const POI_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(450);
/// Only fetch POIs at city-block scale and closer — a whole-country viewport would return an
/// unbounded, meaningless marker set.
const POI_MIN_ZOOM: f64 = 14.0;
/// Cap on POI markers drawn so a dense area stays legible.
const POI_MAX: usize = 140;
/// How many nearby POIs the Explore panel lists (the closest to the anchor).
const NEARBY_MAX: usize = 10;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MapMenu {
    More,
    Layers,
}
#[derive(Clone, Debug)]
enum Hit {
    Consume,
    OpenSearch,
    OpenEmptySearch,
    ToggleCollapse,
    PanelBack,
    Mode(TravelMode),
    SelectPlace(String),
    SelectPoi(String),
    ClosePoi,
    PoiDirections,
    Recent(String),
    ClearRecents,
    ToggleNav,
    CloseNav,
    Directions,
    Transit,
    ToggleSave,
    ClearRoute,
    NextRoute,
    UseMyLocation,
    TransitTimeMode(TransitTimeMode),
    TransitTimeDelta(i32),
    TransitTimeNow,
    Journey(usize),
    ToggleOverflow,
}

struct AccessibilityTarget {
    rect: Rect,
    label: String,
    target: String,
    role: ui::a11y::Role,
    state: ui::a11y::AccessState,
}

fn stable_access_key(id: &str) -> u64 {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, id.as_bytes());
    std::hash::Hasher::finish(&hasher)
}

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

fn imagery_hint(can_fetch: bool, fault: Option<TileFault>) -> String {
    failure::t(match fault {
        _ if !can_fetch => "maps-imagery-unavailable",
        Some(TileFault::Rejected) => "maps-imagery-refused",
        Some(TileFault::Unreachable) => "maps-imagery-unreachable",
        None => "maps-loading-map",
    })
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

    dispatch: Option<ui::framework::Dispatcher<Workspace>>,
    offline_areas: Option<(
        ui::framework::PersonRecord<crate::offline_areas::OfflineAreas>,
        String,
    )>,

    last_mouse: Point,
    segmented_touch: SegmentedTouch,
    map_controls: MapControls,
    place_search: PlaceSearch,
    search_opener: ui::search::SearchOpener,
    history: ui::widget::HistoryOpener,
    results_anchor: Option<LatLon>,
    chrome_rects: Vec<Rect>,
    status_notice: ui::widget::toast::Transient<String>,
    status_said: String,
    endpoints: EndpointEditor,
    left_down: bool,
    dragging: bool,
    drag_last: Point,
    drag_moved: bool,
    click_targets: Vec<(Rect, Hit)>,
    text_scale: f32,
    ui_scale: f64,
    spacing: Spacing,
    metrics: Metrics,
    pending_fit: bool,
    pending_focus: Option<LatLon>,
    pending_poi_focus: bool,
    poi_card_rect: Rect,
    nav_card_size: vello::kurbo::Size,
    panel_edges: Vec<f64>,
    panel_origin: f64,
    scroll: f64,
    content_h: f64,
    content_view_h: f64,

    net_tx: NetSender,
    net_rx: Receiver<NetMsg>,
    pending: u32,
    search_asked: Option<(String, Instant)>,
    online: bool,
    offline_downloading: bool,

    location_rx: Option<LocationChannel>,
    locating: bool,
    location_reply: Option<ui::framework::EffectReply>,

    tiles: TileLayer,

    pois: Vec<crate::model::Poi>,
    selected_poi: Option<String>,
    poi_debounce: Option<Instant>,
    poi_fetched_bounds: Option<GeoBounds>,
    poi_generation: u64,

    overflow: ContextMenu,
    menu_for: MapMenu,
    info_sheet: MapInfoSheet,

    offline_panel: OfflinePanel,

    nav_open: bool,

    panel_collapsed: bool,

    traffic: TrafficData,
    pending_floating: Option<ui::floating_surface::FloatingSurfaceRequest>,
    presented_navigation: bool,
    navigation_placement: ui::floating_surface::FloatingPlacement,
    navigation_drag_handle: Rect,
    navigation_end_rect: Rect,
    navigation_end_hovered: bool,
    motion_reduced: bool,
    floating_surface_bounds: Rect,

    dirty: bool,
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

fn endpoint_of(origin: bool) -> Endpoint {
    if origin {
        Endpoint::Origin
    } else {
        Endpoint::Destination
    }
}

fn bounds_contains(outer: GeoBounds, inner: GeoBounds) -> bool {
    outer.min.lat <= inner.min.lat
        && outer.min.lon <= inner.min.lon
        && outer.max.lat >= inner.max.lat
        && outer.max.lon >= inner.max.lon
}

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

fn on_panel(theme: &Theme) -> Theme {
    theme.on_ground(ui::chrome::panel_ground(theme))
}

fn clock_label(at: chrono::DateTime<chrono::Local>) -> String {
    ui::clock::time_of_day(&at)
}

impl MapsApp {
    fn publish_navigation(&mut self) {
        crate::live::publish(&crate::live::published(self.model.route.as_ref()));
        self.present_navigation_card();
    }

    fn present_navigation_card(&mut self) {
        if self.model.route.is_none() {
            self.presented_navigation = false;
            return;
        }
        if self.presented_navigation {
            return;
        }
        let card = self.navigation_rect();
        if card.width() <= 0.0 || card.height() <= 0.0 {
            return;
        }
        self.pending_floating = self.active_navigation_request_from(card.inflate(8.0, 8.0));
        self.presented_navigation = true;
    }
}

impl MapsApp {}

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
            if matches!(
                message,
                ui::floating_surface::FloatingSurfaceHostMessage::Reattach { .. }
            ) {
                self.presented_navigation = false;
            }
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
        dyn ui::widget::controls::ActionControls
    );

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        for command in crate::live::take_commands() {
            match command {
                crate::live::Command::End => self.clear_route(),
            }
        }
        self.drain_net();
        self.drain_location();
        self.pump_place_search();
        self.tick_poi_debounce(ui::time::now());
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
        let before = self.tiles.progress();
        self.tiles.drain();
        let after = self.tiles.progress();
        if before != after && !after.is_active() && after.total > 0 {
            self.offline_panel.set_cache_bytes(self.tiles.cache_bytes());
            if self.offline_downloading {
                self.offline_downloading = false;
                self.model.status =
                    failure::t_args("maps-offline-ready", &[("count", after.total.into())]);
            }
        }

        self.follow_offline_areas();
        self.endpoints
            .sync_queries(&self.model.origin_query, &self.model.dest_query);
        if let Some(effect) = self.endpoints.take_due_search(ui::time::now()) {
            self.apply_endpoint_effect(effect);
        }
        self.publish_status();
        self.publish_navigation();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let scale_changed = (self.ui_scale - f64::from(theme.ui_scale)).abs() > f64::EPSILON
            || (self.text_scale - theme.text_scale).abs() > f32::EPSILON;
        if scale_changed {
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
                self.forward_move_to_field_inputs(
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    &mut ctx.state,
                );
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
                        *ctx.handled = true;
                    }
                    self.drag_last = p;
                }
                let extra_targets = self
                    .menu_rect()
                    .map(|menu| (menu, failure::t("maps-menu")))
                    .into_iter()
                    .chain(std::iter::once((
                        self.overflow_rect(),
                        failure::t("maps-more"),
                    )));
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
                if self.overflow.is_open() {
                    self.drive_overflow(event, ctx.scale_factor, *ctx.modifiers, &mut ctx.state);
                    if let Some(idx) = self.overflow.take_selection() {
                        self.overflow_select(idx);
                    }
                    *ctx.handled = true;
                    return;
                }
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
                    if let Some(action) = self.map_controls.hit_test(p) {
                        self.dispatch_map_control(action);
                    } else if let Some(id) = self.marker_at(p) {
                        self.select_place(&id);
                    } else if let Some(pid) = self.poi_at(p) {
                        self.select_poi(&pid);
                    } else {
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
                if self.overflow.is_open() {
                    self.drive_overflow(event, ctx.scale_factor, *ctx.modifiers, &mut ctx.state);
                    if let Some(idx) = self.overflow.take_selection() {
                        self.overflow_select(idx);
                    }
                    *ctx.handled = true;
                    return;
                }
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
    fn needs_redraw(&self) -> bool {
        ui::widget::redraw_or_children(self.dirty, self)
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        ui::widget::Widget::mark_children_drawn(self);
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
                .with_name(failure::t("maps-app-name"))
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
        self.pending > 0
            || self.place_search.is_busy()
            || self.locating
            || self.location_rx.is_some()
            || self.poi_debounce.is_some()
            || self.tiles.has_inflight()
            || self.tiles.progress().is_active()
    }

    fn needs_animation_frame(&self) -> bool {
        self.locating
            || self.location_rx.is_some()
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
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.history.take().or_else(|| self.search_opener.take())
    }
}

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
