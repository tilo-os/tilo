//! Root widget for the Maps app.
//!
//! `MapsApp` owns the in-memory [`MapModel`] and the static vector [`Basemap`].
//! [`crate::doc::MapsDoc`] synchronizes its explicitly durable resources; the
//! rest is per-window presentation state. It
//! is a single full-window custom widget: the map is full-bleed and the UI
//! (search bar, side panel, zoom controls, place card) floats on top, drawn
//! straight into the Vello scene with theme-derived colours. Interaction —
//! pan, zoom-to-cursor, marker hit-testing, panel buttons, search typing —
//! is handled directly in `handle_event`.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use ui::time::Instant;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::chrome::{SelectionState, selection_tier};
use ui::format::bytes as format_bytes;
use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::tilo_actions::{
    ActionContract, ActionFailure, ActionOutput, ActionSpec, Availability, DiscoveryScope,
    ExecutionMode, InvocationId, InvocationUpdate, OperationKind, PermissionRequirement, Resource,
    ResultSpec, RetryPolicy,
};
use ui::tokens::Metrics;
use ui::tokens::spacing::Spacing;
use ui::tokens::typography::TypeScale;
use ui::widget::{Button, ButtonFace, ContextMenu, MenuItem};
use ui::widget::{FloatingSurfaceProvider, ResourceOperationStart, Widget};

use crate::actions::{MAPS_ACTION_TYPE, MapsAction};
use crate::canvas::{self, MapPalette, MapView};
use crate::doc::MapsDoc;
use crate::draw;
use crate::endpoint_editor::{Endpoint, EndpointEditor, EndpointEffect, EndpointFrame};
use crate::geo::{self, Projection};
use crate::map_controls::{MapControlAction, MapControls};
use crate::map_info_sheet::{MapInfo, MapInfoSheet};
use crate::model::{
    Category, LatLon, MapModel, Panel, Place, TransitMode, TransitTimeMode, TravelMode,
};
use crate::net::{self, NetMsg};
use crate::offline_panel::{OFFLINE_AREAS_REFUSED, OfflineAction, OfflineFrame, OfflinePanel};
use crate::sample::{self, Basemap};
use crate::search_box::{SearchBox, SearchEffect, SearchHit, SearchKeyEffect, SearchOverlay};
use crate::tiles::{GeoBounds, TileFault, TileId, TileLayer, TileStyle};
use crate::traffic::{Congestion, TrafficData};
use ui::location::{self, LocationChannel, LocationOutcome};

mod interaction;

mod resource_surface;

// --- Layout metrics (logical px) -------------------------------------------
// The window margin is the theme's `s4`, the search bar an `s12` strip, the
// panel header the theme's bar height and the docked/drawer breakpoint its
// compact breakpoint — read through `MapsApp::margin` & co. from the cached
// theme tokens. The values below are the deliberately authored ones.
/// Gap between adjacent floating elements — the theme's `s3`, the sibling
/// step every floating map surface keeps from its neighbour (HIG §7b), so
/// the search bar, panel, controls and cards share one rhythm.
const GAP: f64 = Spacing::BASE.s3 as f64;
const CTRL_SIZE: f64 = crate::map_controls::SIZE;
const ROW_H: f64 = 56.0;
/// Widest the panel is ever drawn (docked, or as an overlay drawer).
const PANEL_MAX_W: f64 = 384.0;
const PANEL_MIN_W: f64 = 300.0;
/// Authored 1× height of the floating navigation card; its width is the
/// theme's list-panel width. The card stacks a title (`sm`), the next
/// instruction, its distance (`sm`) and the summary row (`lg`); the card's
/// intrinsic size grows from these, see [`navigation_card_size`].
const NAV_CARD_H: f64 = 156.0;
/// Base size of the next-instruction line (15pt sits between the type
/// scale's `base` and `lg`).
const NAV_INSTR_TEXT: f32 = 15.0;
/// Authored 1× width of the card's End pill and its label size (11pt sits
/// between `xs` and `sm`); the pill is an `s6` tall.
const NAV_END_W: f64 = 50.0;
const NAV_END_TEXT: f32 = 11.0;
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
    actions: ui::ActionHost,

    // interaction state (render/UI caches only — app state lives in `model`)
    last_mouse: Point,
    /// Floating zoom/location/traffic stack. Owns geometry, drawing,
    /// hit-testing, traffic UI state, and shared tooltip timing.
    map_controls: MapControls,
    /// Top-level place search: text input, focus, debounce, results overlay,
    /// drawing, and hot regions are one component.
    search: SearchBox,
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
    /// How much wider the End pill's label measures at the last seen text
    /// scale than at 1× — `(scale, growth)`, re-measured only when the scale
    /// changes rather than every frame.
    nav_end_growth: (f32, f64),
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
                preferred_size: navigation_card_size(&self.metrics, self.text_scale),
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

    /// The area the floating map controls (zoom, locate, traffic) lay
    /// themselves out in. Normally the whole window; while the navigation
    /// card floats over the top-right corner, the cluster starts beneath it
    /// so the controls stay visible and reachable rather than being covered.
    /// Paint, pointer hit-testing and the accessibility rects all read the
    /// controls' geometry from this one viewport.
    /// Top of a `size`-pt line centred in a band, at the captured text scale.
    fn vmid(&self, y: f64, h: f64, size: f32) -> f64 {
        text_vmid(y, h, size, self.text_scale)
    }

    /// The window margin every floating element keeps — the theme's `s4`.
    fn margin(&self) -> f64 {
        f64::from(self.spacing.s4)
    }

    /// The search bar's height — the theme's `s12` strip.
    fn search_h(&self) -> f64 {
        f64::from(self.spacing.s12)
    }

    /// The side panel's header bar (back / title / collapse toggle) — the
    /// theme's bar height.
    fn panel_header_h(&self) -> f64 {
        f64::from(self.metrics.bar_height)
    }

    /// How much wider the End pill's label measures at the renderer's scale
    /// than at 1×, re-measured only when the scale changes.
    fn nav_end_label_growth(&mut self, tr: &mut TextRenderer) -> f64 {
        let scale = tr.text_scale();
        if self.nav_end_growth.0.to_bits() != scale.to_bits() {
            let growth = if scale > 1.0 {
                let scaled = tr
                    .measure_weighted("End", NAV_END_TEXT, None, TextWeight::Medium)
                    .0;
                // Measuring at `size / scale` yields the 1× width on the same
                // renderer, which multiplies every size by its scale.
                let one_x = tr
                    .measure_weighted("End", NAV_END_TEXT / scale, None, TextWeight::Medium)
                    .0;
                f64::from(scaled - one_x).max(0.0)
            } else {
                0.0
            };
            self.nav_end_growth = (scale, growth);
        }
        self.nav_end_growth.1
    }

    fn map_controls_viewport(&self) -> Rect {
        let viewport = self.rect;
        if !self.navigation_surface_visible_in_app() {
            return viewport;
        }
        let margin = self.margin();
        let nav = self.navigation_rect();
        let column_h = margin + crate::map_controls::column_height();
        let column = Rect::new(
            self.rect.x1 - margin - CTRL_SIZE,
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
        let floor = self.overflow_rect().y0 - GAP;
        if nav.y1 < self.rect.y1 && nav.y1 + column_h <= floor {
            return Rect::new(viewport.x0, nav.y1, viewport.x1, viewport.y1);
        }
        let panel_right = if self.panel_docked_expanded() {
            self.panel_rect().x1 + GAP
        } else {
            self.rect.x0 + margin
        };
        if nav.x0 - GAP - CTRL_SIZE >= panel_right {
            return Rect::new(viewport.x0, viewport.y0, nav.x0 - GAP + margin, viewport.y1);
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
        let c = &theme.tokens.colors;
        let shadow =
            ui::surface::soft_outline(rect.inflate(5.0, 5.0) + vello::kurbo::Vec2::new(0.0, 2.0));
        scene.fill(
            Fill::NonZero,
            visual,
            draw::alpha(c.neutral[0], 0.30),
            None,
            &shadow,
        );
        let body = ui::surface::soft_outline(rect);
        scene.fill(Fill::NonZero, visual, c.surfaces.raised, None, &body);
        // The card's rest edge derives from its own surface (HIG §9.5): a
        // declared `border_standard` is three shades off dark `raised` and
        // vanished against the map.
        scene.stroke(
            &Stroke::new(1.0),
            visual,
            ui::surface::edge_on(c.surfaces.raised),
            None,
            &body,
        );
        let pad = self.margin();
        // The End pill is its authored 50px wide and an `s6` tall at 1×,
        // grown by the label's measured extra width (cached per scale) and
        // its line plus an `s1` above and below once scaled type needs more,
        // so "End" never spills out of its own face.
        let end_scale = tr.text_scale();
        let end_label_growth = self.nav_end_label_growth(tr);
        let end_w = (NAV_END_W + end_label_growth).ceil();
        let end_h = (line_h(NAV_END_TEXT, end_scale) + 2.0 * f64::from(self.spacing.s1))
            .ceil()
            .max(f64::from(self.spacing.s6));
        let end = Rect::new(
            rect.x1 - 8.0 - end_w,
            rect.y0 + 6.0,
            rect.x1 - 8.0,
            rect.y0 + 6.0 + end_h,
        );
        self.navigation_end_rect = end;
        let handle = Rect::new(rect.x0, rect.y0, end.x0 - 4.0, rect.y0 + 34.0);
        self.navigation_drag_handle = handle;
        PaintCtx::new(scene, visual, tr, theme).text(
            "Navigation",
            &TextStyle::new(text_sm()).with_weight(TextWeight::Medium),
            theme.text_dim,
            Point::new(rect.x0 + pad, rect.y0 + 10.0),
            None,
        );
        ui::surface::floating_drag_grip(scene, visual, rect, theme.text_dim);

        // Ending a live activity is intentionally explicit. At rest this reads
        // as quiet chrome; hover adds a soft danger wash without changing the
        // button's geometry (and therefore without making the title jump).
        let end_text = if self.navigation_end_hovered {
            c.semantic.danger
        } else {
            theme.text_dim
        };
        if self.navigation_end_hovered {
            draw::fill_round(
                scene,
                visual,
                end,
                10.0,
                draw::alpha(c.semantic.danger, 0.16),
            );
            draw::stroke_rrect(
                scene,
                visual,
                end,
                10.0,
                1.0,
                draw::alpha(c.semantic.danger, 0.58),
            );
        } else {
            // Quiet chrome at rest: the base tier seated in the raised card
            // (a contrast step on both themes) with its derived edge.
            draw::edged(scene, visual, end, 10.0, c.surfaces.base);
        }
        draw::fill_round(
            scene,
            visual,
            Rect::new(
                end.x0 + 9.0,
                end.center().y - 2.5,
                end.x0 + 14.0,
                end.center().y + 2.5,
            ),
            1.5,
            end_text,
        );
        PaintCtx::new(scene, visual, tr, theme).text(
            "End",
            &TextStyle::new(NAV_END_TEXT).with_weight(TextWeight::Medium),
            end_text,
            Point::new(
                end.x0 + 19.0,
                text_vmid(end.y0, end.height(), NAV_END_TEXT, end_scale),
            ),
            None,
        );

        let instruction = route.steps.first();
        let glyph = instruction.map_or("↑", |step| step.maneuver.glyph());
        let text = instruction.map_or("Continue on the route", |step| step.instruction.as_str());
        // The instruction row starts where the design put it plus the growth
        // of the title line above it, so a scaled "Navigation" never runs
        // into the badge and instruction beneath.
        let title_growth = line_growth(text_sm(), tr.text_scale());
        let badge = Rect::new(
            rect.x0 + pad,
            rect.y0 + 48.0 + title_growth,
            rect.x0 + pad + 36.0,
            rect.y0 + 84.0 + title_growth,
        );
        draw::fill_circle(
            scene,
            visual,
            badge.center(),
            18.0,
            draw::alpha(theme.accent_primary, 0.18),
        );
        draw::text_center(
            &mut PaintCtx::new(scene, visual, tr, theme),
            glyph,
            Point::new(badge.center().x, badge.y0 + 8.0),
            17.0,
            theme.accent_primary,
        );
        // Instruction, then its distance directly beneath on the measured
        // line; the rule and summary row follow the stack instead of sitting
        // at fixed offsets a scaled instruction runs into. (The card's own
        // height is fixed by the floating-surface placement — see the review
        // notes — so at large scales the stack can still exceed the card.)
        let instr_y = rect.y0 + 47.0 + title_growth;
        let instr_h = f64::from(tr.measure("Ag", NAV_INSTR_TEXT, None).1);
        let dist_h = f64::from(tr.measure("Ag", text_sm(), None).1);
        let dist_y = instr_y + instr_h + 2.0;
        let rule_y = (dist_y + dist_h + 12.0).max(rect.y0 + 98.0 + title_growth);
        let shift = rule_y - (rect.y0 + 98.0);
        draw::text_clip(
            &mut PaintCtx::new(scene, visual, tr, theme),
            text,
            Point::new(rect.x0 + 64.0, instr_y),
            15.0,
            theme.text_primary,
            rect.width() - 82.0,
        );
        let next_distance = instruction.map_or(route.distance_m, |step| step.distance_m);
        draw::text(
            scene,
            tr,
            visual,
            &geo::fmt_distance(next_distance),
            rect.x0 + 64.0,
            dist_y,
            12.0,
            theme.text_dim,
        );
        // The rule between the next step and the summary derives against the
        // card and stops at the content gutter, never running into the
        // corner radius.
        draw::separator(
            scene,
            visual,
            rect.x0 + pad,
            rect.x1 - pad,
            rule_y,
            c.surfaces.raised,
        );
        PaintCtx::new(scene, visual, tr, theme).text(
            &geo::fmt_duration(route.duration_s),
            &TextStyle::new(16.0).with_weight(TextWeight::Bold),
            theme.accent_primary,
            Point::new(rect.x0 + pad, rect.y0 + 114.0 + shift),
            None,
        );
        draw::text_right(
            scene,
            tr,
            visual,
            &format!("{} remaining", geo::fmt_distance(route.distance_m)),
            rect.x1 - pad,
            rect.y0 + 116.0 + shift,
            12.0,
            theme.text_dim,
        );
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
        Self {
            model,
            basemap,
            rect: Rect::ZERO,
            node: None,
            actions: ui::ActionHost::new(),
            last_mouse: Point::ZERO,
            map_controls: MapControls::default(),
            search: SearchBox::new(),
            endpoints: EndpointEditor::new(),
            left_down: false,
            dragging: false,
            drag_last: Point::ZERO,
            drag_moved: false,
            click_targets: Vec::new(),
            text_scale: 1.0,
            spacing: Spacing::default(),
            metrics: Metrics::BASE,
            nav_end_growth: (1.0, 0.0),
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
                m.push(MenuItem::action("Float navigation", || {}));
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
            navigation_placement: ui::floating_surface::FloatingPlacement::attached(
                navigation_card_size(&Metrics::BASE, 1.0),
            ),
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
        self.model.status = "Map imagery unavailable offline — reconnect and try again".into();
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

    /// Headless visual-review hook for the production navigation renderer and
    /// its real shell → app ownership edge. Called after layout so the
    /// `Suspended` host path uses authoritative app-local coordinates.
    #[doc(hidden)]
    pub fn debug_stage_navigation_for_shot(&mut self, stage: &str) {
        if stage == "attached" {
            self.navigation_placement
                .pin_handoff_for_visual_review(1_000);
            return;
        }
        let detached = Rect::new(820.0, 120.0, 1_180.0, 276.0);
        self.navigation_placement.detach_to_desktop(detached);
        self.navigation_placement.set_desktop_transfer_covered(true);
        self.navigation_placement.confirm_desktop_adoption();

        if stage == "hover" {
            self.navigation_end_hovered = true;
        }

        if matches!(stage, "returning" | "returned") {
            let pointer = Point::new(detached.center().x, detached.y0 + 16.0);
            self.navigation_placement.begin_drag(
                pointer,
                self.rect,
                self.desktop_floating_bounds(),
            );
            let _ = self.navigation_placement.update_drag(
                Point::new(720.0, 360.0),
                self.rect,
                self.desktop_floating_bounds(),
            );
        }

        if stage == "returned"
            && let Some(id) = self.active_navigation_request().map(|request| request.id)
        {
            <Self as FloatingSurfaceProvider>::on_floating_surface_host_message(
                self,
                &ui::floating_surface::FloatingSurfaceHostMessage::Suspended { id },
            );
        }

        let handoff_phase = if matches!(stage, "returning" | "returned") {
            500
        } else {
            1_000
        };
        self.navigation_placement
            .pin_handoff_for_visual_review(handoff_phase);
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

    /// Panel width for the current window: a docked column proportional to the
    /// window when wide, or a near-full-width drawer when narrow.
    fn panel_w(&self) -> f64 {
        // The panel carries text, so its authored bounds grow with the text
        // scale (never shrink): a 2× row keeps the character budget a 1× row
        // has instead of eliding inside a 1× column. Bounds are combined with
        // `max`/`min` rather than `clamp`: a degenerate rect can put them out
        // of order, and `clamp` panics where these just pick the finite one.
        let grow = f64::from(self.text_scale.clamp(1.0, 4.0));
        let avail = (self.rect.width() - 2.0 * self.margin()).max(1.0);
        if self.overlay_mode() {
            // The 1× drawer is exactly what it was; only the *extra* width a
            // scaled drawer gains stops short of the trailing zoom/locate
            // column (as the search bar does), so growth never slides the
            // drawer under the map controls.
            let one_x = avail.min(PANEL_MAX_W);
            let scaled = (PANEL_MAX_W * grow).min((avail - CTRL_SIZE - GAP).max(1.0));
            scaled.max(one_x)
        } else {
            // Docked: 42 % of the window inside the scaled authored bounds,
            // and never more than 60 % so the map keeps room beside it.
            let max_w = (PANEL_MAX_W * grow).min(avail).min(self.rect.width() * 0.6);
            let min_w = (PANEL_MIN_W * grow).min(max_w);
            (self.rect.width() * 0.42).max(min_w).min(max_w)
        }
    }

    /// The leading menu button that toggles the drawer — narrow layouts only.
    fn menu_rect(&self) -> Option<Rect> {
        if !self.overlay_mode() {
            return None;
        }
        let x0 = self.rect.x0 + self.margin();
        let y0 = self.rect.y0 + self.margin();
        Some(Rect::new(x0, y0, x0 + CTRL_SIZE, y0 + self.search_h()))
    }

    fn search_rect(&self) -> Rect {
        let y0 = self.rect.y0 + self.margin();
        if self.overlay_mode() {
            // Sits between the leading menu button and the trailing zoom column
            // so the bar spans the full top edge without colliding.
            let x0 = self.rect.x0 + self.margin() + CTRL_SIZE + GAP;
            let x1 = self.rect.x1 - self.margin() - CTRL_SIZE - GAP;
            Rect::new(x0, y0, x1.max(x0 + 120.0), y0 + self.search_h())
        } else {
            let x0 = self.rect.x0 + self.margin();
            Rect::new(x0, y0, x0 + self.panel_w(), y0 + self.search_h())
        }
    }

    fn panel_rect(&self) -> Rect {
        let x0 = self.rect.x0 + self.margin();
        let top = self.rect.y0 + self.margin() + self.search_h() + GAP;
        Rect::new(x0, top, x0 + self.panel_w(), self.rect.y1 - self.margin())
    }

    /// The bottom-right ⋯ overflow button (attribution / offline maps / licenses).
    /// Downloads and the credit line moved off the map into this menu.
    fn overflow_rect(&self) -> Rect {
        let x1 = self.rect.x1 - self.margin();
        let y1 = self.rect.y1 - self.margin();
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
        self.model.status = format!("Removed “{}” · {} freed", region.name, format_bytes(freed));
    }

    // --- actions -----------------------------------------------------------
    /// Dispatch one collaborative [`MapsAction`] against the shared document.
    ///
    /// The host applies the same canonical binding used by resources and
    /// on-screen controls. Local presentation state is never pre-flushed.
    fn perform(&mut self, doc: &mut AutoCommit, action: &MapsAction) {
        // Test/standalone callers may invoke an action before the widget's
        // first sync. Seed once only; ordinary interactions never mirror local
        // presentation back into an existing document.
        if MapsDoc::load(doc).is_none() {
            MapsDoc::save(doc, &self.model);
        }
        self.actions.dispatch_with_context(
            doc,
            MAPS_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.maps", "Maps", action),
        );
        MapsDoc::apply_durable(doc, &mut self.model);
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
        self.search.query_changed(&q, matches, ui::time::now());
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
            spawn_net(async move {
                let results = net::geocode(&q).await;
                let _ = tx.send(NetMsg::Search { query: q, results });
            });
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
        // Enter commits the search: dismiss the floating overlay and stop the
        // pending debounce (the live augmentation below supersedes it).
        self.search.dismiss_results();
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

    /// Frame the map so `pts` fit the visible map area (the part not covered by
    /// the docked panel). Used to fit a road route's geometry and, for transit
    /// where there is no road polyline, the origin/destination pair.
    fn fit_points(&mut self, pts: &[LatLon]) {
        // In the docked layout the map shares the width with the panel; in the
        // overlay layout the drawer floats over a full-width map.
        let w = if self.overlay_mode() {
            self.rect.width() - self.margin() * 2.0
        } else {
            self.rect.width() - (self.panel_w() + self.margin() * 2.0 + GAP)
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
        // Reveal the drawer on narrow layouts so the tapped result is visible.
        self.nav_open = true;
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
                        // Rank the From/To dropdown the same way the top search
                        // is ranked — relevance first, nearest-first within a
                        // tier — so a distant namesake never sits above the
                        // near place the user meant.
                        let mut ranked = found;
                        self.rank_results(&mut ranked, &query);
                        self.endpoints.set_results(
                            if origin {
                                Endpoint::Origin
                            } else {
                                Endpoint::Destination
                            },
                            ranked,
                        );
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
            Hit::Search(hit) => match self.search.handle_hit(hit) {
                SearchEffect::None => {}
                SearchEffect::Clear => {
                    self.model.query.clear();
                    self.model.results.clear();
                }
                SearchEffect::Pick(place) => {
                    self.perform(
                        doc,
                        &MapsAction::UpsertPlaces {
                            places: vec![place.clone()],
                        },
                    );
                    place.name.clone_into(&mut self.model.query);
                    self.perform(
                        doc,
                        &MapsAction::CommitSearch {
                            query: place.name.clone(),
                        },
                    );
                    // Selection is intentionally window-local; only accepting
                    // the geocoded place crossed the shared action boundary.
                    self.select_place(&place.id);
                }
            },
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
                q.clone_into(&mut self.model.query);
                self.run_search(Some(doc));
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

/// Top of a `size`-pt line vertically centred in a band of height `h` at `y`,
/// at the renderer's `text_scale`: the renderer draws one line as
/// `size × text_scale × 1.25`, so centring on the unscaled size would drop the
/// text below centre at every scale above 1×.
fn text_vmid(y: f64, h: f64, size: f32, text_scale: f32) -> f64 {
    y + (h - line_h(size, text_scale)) / 2.0
}

/// Height of one line of `size`-pt text at `text_scale` — what the renderer
/// measures for a single line. The crate's one copy of the line formula;
/// `text_vmid` and the authored row heights below all read it.
fn line_h(size: f32, text_scale: f32) -> f64 {
    ui::text_metrics::line_height_px(size, text_scale)
}

/// How much taller one line of `size`-pt text is at `text_scale` than at 1×;
/// zero at or below 1×.
fn line_growth(size: f32, text_scale: f32) -> f64 {
    ui::text_metrics::line_growth(size, text_scale)
}

/// Base (unscaled) text sizes handed to the legacy renderer, read from the
/// default type scale so the app states no sizes of its own; the renderer
/// applies the accessibility scale. Sizes off the scale are named beside the
/// surfaces that use them.
fn text_sm() -> f32 {
    TypeScale::default().sm
}
fn text_lg() -> f32 {
    TypeScale::default().lg
}

/// An authored 1× height grown by exactly the extra room its text lines need
/// at `text_scale`, snapped up to a whole pixel — the design exactly at 1×
/// (a 28px row stays 28, not `16.25 + 12 = 28.25`), and never a fraction.
fn authored_h(authored: f64, sizes: &[f32], text_scale: f32) -> f64 {
    ui::text_metrics::authored_h(authored, sizes, text_scale)
}

/// Intrinsic size of the floating navigation card at `text_scale`: the
/// authored 360×156 at 1×; the height grows by the title, instruction,
/// distance and summary lines it stacks, the width with the scale so the
/// instruction keeps its 1× character budget instead of eliding at 2×. The
/// placement clamps it to the window it floats in.
fn navigation_card_size(metrics: &Metrics, text_scale: f32) -> vello::kurbo::Size {
    let grow = f64::from(text_scale.clamp(1.0, 4.0));
    vello::kurbo::Size::new(
        (f64::from(metrics.panel_width_list) * grow).ceil(),
        authored_h(
            NAV_CARD_H,
            &[text_sm(), NAV_INSTR_TEXT, text_sm(), text_lg()],
            text_scale,
        ),
    )
}

// ===========================================================================
// Drawing
// ===========================================================================
impl MapsApp {
    fn draw_app(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        self.click_targets.clear();
        self.text_scale = tr.text_scale();
        self.map_controls.set_viewport(self.map_controls_viewport());
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
            let left = (self.panel_rect().x1 + GAP).min(clip.x1);
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
        self.draw_panel_and_scale(scene, t, tr, theme, overlay, clip, painted_tiles);
        self.draw_chrome(scene, t, tr, theme);
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
        // available (headless `--shots`, or the very first offline frame).
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
        // nothing here — the honest "No traffic data" pill is drawn with the
        // chrome below.
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

    /// Side panel (docked or overlay drawer + scrim), the scale bar, and the
    /// offline/loading hint that only shows while no real imagery has painted.
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
            draw::fill_rect(
                scene,
                t,
                clip,
                draw::alpha(theme.tokens.colors.surfaces.scrim, 0.5),
            );
            self.click_targets.push((clip, Hit::CloseNav));
            if show_panel {
                self.draw_panel(scene, t, tr, theme);
            }
        } else if !overlay && show_panel {
            self.draw_panel(scene, t, tr, theme);
        }

        // Scale bar, bottom-right, ending just left of the ⋯ overflow button,
        // with the tile credit beside it; the full attribution and licenses
        // live in that menu.
        let scale_x1 = self.overflow_rect().x0 - GAP;
        let scale_x0 = canvas::draw_scale_bar(scene, t, tr, clip, scale_x1, &self.model, theme);
        canvas::draw_attribution(scene, t, tr, clip, scale_x0, scale_x1, theme);

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
        if matches!(self.model.panel, Panel::Directions) {
            let anchor = self.search_anchor();
            self.endpoints.draw_overlay(scene, t, tr, theme, anchor);
        }
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
                    downloads_allowed: self.tiles.allows_prefetch(),
                },
            );
        }
        let uncovered_desktop_copy = self.model.route.is_some()
            && matches!(
                self.navigation_placement.attachment(),
                ui::floating_surface::Attachment::Desktop { .. }
            )
            && !self.navigation_placement.is_desktop_covered();
        if self.navigation_surface_visible_in_app() || uncovered_desktop_copy {
            self.draw_navigation_surface(scene, t, tr, theme);
        } else if self.overlay_mode() && self.nav_open {
            // Do not leave stale hit/a11y geometry behind when the drawer
            // suppresses the attached card on this frame.
            self.navigation_drag_handle = Rect::ZERO;
            self.navigation_end_rect = Rect::ZERO;
            self.navigation_end_hovered = false;
        }

        // The ⋯ overflow button (attribution / offline / licenses) sits bottom-
        // right; its menu and the info sheet draw last, on top of everything.
        self.draw_overflow(scene, t, tr, theme);
        if self.overflow.is_open() {
            self.overflow
                .draw(scene, t, tr, theme, &PresenceCtx::empty());
        }
        self.info_sheet.draw(scene, t, tr, theme, self.rect);
        self.map_controls.draw_tooltip(scene, t, tr, theme);
    }

    /// The bottom-right ⋯ button. When the overflow menu is open (or a download
    /// is running) it wears an accent border, so an in-progress offline download
    /// stays visible even with the dialog closed.
    fn draw_overflow(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let r = self.overflow_rect();
        let active = self.overflow.is_open() || self.tiles.progress().is_active();
        // The same Neutral face as the map controls column; the ⋯ glyph
        // paints on top. An open menu or a running download is live state,
        // so it wears an accent ring over the rest edge (HIG §9.4).
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(r, "").radius(10.0).accessible_name("More"),
        );
        if active {
            draw::stroke_rrect(scene, t, r, 10.0, 1.0, theme.accent_primary);
        }
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

    /// The leading menu button in the narrow overlay layout: toggles the panel
    /// drawer. Renders nothing when the panel is docked.
    fn draw_menu(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let Some(r) = self.menu_rect() else { return };
        // Same Neutral face as every floating map button; the open state is
        // carried by the accent glyph below, not a second border colour.
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(r, "").radius(12.0).accessible_name("Menu"),
        );
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
            &SearchOverlay {
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
        matches!(self.model.panel, Panel::Directions) && self.endpoints.overlay_active()
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
            self.rect.x0 + self.margin()
        };
        let area_x1 = self.rect.x1 - self.margin();
        let w = 360.0_f64.min((area_x1 - area_x0).max(200.0));
        // One rhythm from the theme: `s4` card gutter, `s1` between the
        // title and its category line, `s2` between detail rows, `s3`
        // before the action; every line sits on its measured box.
        let ts = self.text_scale;
        let line = |size: f32| line_h(size, ts);
        let (s1, s2, s3, pad) = (
            f64::from(self.spacing.s1),
            f64::from(self.spacing.s2),
            f64::from(self.spacing.s3),
            f64::from(self.spacing.s4),
        );
        let (title_h, cat_h, row_line) = (line(18.0), line(12.0), line(12.0));
        let bh = 36.0;
        let card_h =
            (pad + title_h + s1 + cat_h + rows.len() as f64 * (s2 + row_line) + s3 + bh + pad)
                .ceil();
        let cx = f64::midpoint(area_x0, area_x1);
        let x0 = (cx - w / 2.0).max(area_x0);
        let y1 = self.rect.y1 - 54.0; // above the scale bar + attribution
        let y0 = y1 - card_h;
        let card = Rect::new(x0, y0, x0 + w, y1);
        draw::card(scene, t, theme, card, 16.0, c.surfaces.raised);
        self.click_targets.push((card, Hit::Consume));

        // Category colour bar down the leading edge.
        draw::fill_round(
            scene,
            t,
            Rect::new(card.x0, card.y0, card.x0 + 5.0, card.y1),
            16.0,
            canvas::category_color(theme, poi.category),
        );

        let x = card.x0 + pad;
        let right = card.x1 - pad;
        let mut y = card.y0 + pad;
        // Close (×): a 26 px target seated `s2` inside the corner, its glyph
        // centred on the title line; the title elides before it.
        let close_edge = 26.0;
        let close = Rect::new(
            card.x1 - s2 - close_edge,
            card.y0 + s2,
            card.x1 - s2,
            card.y0 + s2 + close_edge,
        );
        draw::text_center(
            &mut PaintCtx::new(scene, t, tr, theme),
            "✕",
            Point::new(close.center().x, self.vmid(close.y0, close_edge, 14.0)),
            14.0,
            theme.text_dim,
        );
        self.click_targets.push((close, Hit::ClosePoi));
        let name = tr.elide_to_width(&poi.name, 18.0, ui::num::f32_of_f64(close.x0 - s2 - x));
        PaintCtx::new(scene, t, tr, theme).text(
            &name,
            &TextStyle::new(18.0).with_weight(TextWeight::Bold),
            theme.text_primary,
            Point::new(x, y),
            None,
        );
        y += title_h + s1;
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
                &mut PaintCtx::new(scene, t, tr, theme),
                &format!("· {k}"),
                Point::new(x + cw + 10.0, y),
                12.0,
                theme.text_dim,
                right - x - cw - 10.0,
            );
        }
        y += cat_h;
        for (glyph, text, link) in &rows {
            y += s2;
            draw::text(scene, tr, t, glyph, x, y, 12.0, theme.text_dim);
            let col = if *link {
                theme.accent_primary
            } else {
                theme.text_primary
            };
            draw::text_clip(
                &mut PaintCtx::new(scene, t, tr, theme),
                text,
                Point::new(x + 20.0, y),
                12.0,
                col,
                right - x - 20.0,
            );
            y += row_line;
        }
        y += s3;
        // Directions (primary action): the shared button face.
        let btn = Rect::new(x, y, right, y + bh);
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(btn, "Directions")
                .primary()
                .radius(9.0)
                .text_size(13.0),
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
            self.rect.x0 + self.margin()
        };
        let bottom = self.rect.y1 - self.margin();

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
                theme,
                pill,
                h / 2.0,
                draw::alpha(c.surfaces.raised, 0.96),
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
                self.vmid(pill.y0, h, fs),
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
            theme,
            card,
            12.0,
            draw::alpha(c.surfaces.raised, 0.96),
        );

        let x = card.x0 + pad;
        let mut y = card.y0 + pad;
        PaintCtx::new(scene, t, tr, theme).text(
            "Traffic",
            &TextStyle::new(title_fs).with_weight(TextWeight::Semibold),
            c.text_primary,
            Point::new(x, y),
            None,
        );
        if let Some(src) = &self.traffic.source {
            let tw = draw::measure_w(tr, "Traffic", title_fs) + 8.0;
            draw::text_clip(
                &mut PaintCtx::new(scene, t, tr, theme),
                &format!("· {src}"),
                Point::new(x + tw, y),
                10.0,
                c.text_dim,
                card.x1 - pad - (x + tw),
            );
        }
        y += header_h;
        for level in Congestion::ALL {
            let sw = Rect::new(x, y + 3.0, x + 18.0, y + 3.0 + 6.0);
            draw::fill_round(scene, t, sw, 3.0, canvas::traffic_color(&pal, level));
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
        // Surface transient progress and recoverable failures. Steady success
        // messages stay out of the way, but hiding a failure merely because
        // its worker finished strands a user on a silent map.
        let status_lower = self.model.status.to_ascii_lowercase();
        let is_error = ["failed", "unavailable", "denied", "error"]
            .iter()
            .any(|word| status_lower.contains(word));
        if self.pending == 0 && !self.model.status.contains('…') && !is_error {
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
        let top = self.rect.y0 + self.margin() + self.search_h() + GAP;
        let r = Rect::new(cx - w / 2.0, top, cx + w / 2.0, top + 30.0);
        draw::card(
            scene,
            t,
            theme,
            r,
            15.0,
            theme.tokens.colors.surfaces.raised,
        );
        // Small spinner dot.
        let dot = Point::new(r.x0 + 16.0, r.center().y);
        draw::fill_circle(
            scene,
            t,
            dot,
            4.0,
            if is_error {
                theme.tokens.colors.semantic.danger
            } else {
                theme.accent_primary
            },
        );
        draw::text(
            scene,
            tr,
            t,
            label,
            r.x0 + 28.0,
            self.vmid(r.y0, r.height(), size),
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
        let fault = self.tiles.failure().filter(|_| !self.tiles.has_inflight());
        let offline = fault.is_some();
        let msg = match fault {
            Some(TileFault::Rejected) => "The map tile server refused the request",
            Some(TileFault::Unreachable) => "Map imagery unavailable offline",
            None => "Loading map…",
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
        draw::card(scene, t, theme, r, 17.0, c.surfaces.raised);
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
            self.vmid(r.y0, r.height(), size),
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
        // The panel's one inset — `s3` between its edge and every child.
        let inset = f64::from(self.spacing.s3);
        let s1 = f64::from(self.spacing.s1);
        let docked = !self.overlay_mode();
        let directions = matches!(self.model.panel, Panel::Directions);

        // Collapsed (docked only): show just a header bar with an expand
        // affordance so the map reclaims the column. A plain collapse/expand
        // toggle — not a mode/tab.
        if self.panel_collapsed && docked {
            let bar = Rect::new(
                pr.x0,
                pr.y0,
                pr.x1,
                pr.y0 + self.panel_header_h() + 2.0 * inset,
            );
            draw::card(scene, t, theme, bar, 14.0, c.surfaces.raised);
            self.click_targets.push((bar, Hit::Consume));
            let by = self.vmid(bar.y0 + inset, self.panel_header_h(), 15.0);
            PaintCtx::new(scene, t, tr, theme).text(
                "Places",
                &TextStyle::new(15.0).with_weight(TextWeight::Semibold),
                theme.text_primary,
                Point::new(bar.x0 + inset + s1, by),
                None,
            );
            let btn = Rect::new(
                bar.x1 - inset - self.panel_header_h(),
                bar.y0 + inset,
                bar.x1 - inset,
                bar.y1 - inset,
            );
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::new(btn, "»")
                    .text_size(16.0)
                    .accessible_name("Expand panel"),
            );
            self.click_targets.push((btn, Hit::ToggleCollapse));
            self.content_view_h = 0.0;
            self.content_h = 0.0;
            return;
        }

        draw::card(scene, t, theme, pr, 14.0, c.surfaces.raised);
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
            pr.y0 + inset + self.panel_header_h(),
        );
        let mut hx = header.x0 + s1;
        if directions {
            let back = Rect::new(
                header.x0,
                header.y0,
                header.x0 + self.panel_header_h(),
                header.y1,
            );
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::new(back, "‹")
                    .text_size(18.0)
                    .accessible_name("Back"),
            );
            self.click_targets.push((back, Hit::PanelBack));
            hx = back.x1 + f64::from(self.spacing.s2);
        }
        let title = if directions { "Directions" } else { "Places" };
        PaintCtx::new(scene, t, tr, theme).text(
            title,
            &TextStyle::new(16.0).with_weight(TextWeight::Bold),
            theme.text_primary,
            Point::new(hx, self.vmid(header.y0, header.height(), 16.0)),
            None,
        );
        if docked {
            let btn = Rect::new(
                header.x1 - self.panel_header_h(),
                header.y0,
                header.x1,
                header.y1,
            );
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::new(btn, "«")
                    .text_size(16.0)
                    .accessible_name("Collapse panel"),
            );
            self.click_targets.push((btn, Hit::ToggleCollapse));
        }

        // Content region (below the header), clipped + scrolled.
        let content = Rect::new(
            pr.x0 + inset,
            header.y1 + inset,
            pr.x1 - inset,
            pr.y1 - inset,
        );
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
        // The panel's rhythm: `s1` under a title, `s2` between related rows
        // and after a section label, `s3` between siblings, `s4` between
        // sections — every line on its measured box.
        let ts = self.text_scale;
        let line = |size: f32| line_h(size, ts);
        let (s1, s2, s3, s4) = (
            f64::from(self.spacing.s1),
            f64::from(self.spacing.s2),
            f64::from(self.spacing.s3),
            f64::from(self.spacing.s4),
        );
        let label_advance = line(11.0) + s2;

        // First-run empty state: nothing selected, searched, saved, recent, or
        // fetched for the viewport. An honest hint toward the primary action
        // rather than a blank panel.
        let empty = self.model.selected_place().is_none()
            && self.model.results.is_empty()
            && self.model.saved_places().is_empty()
            && self.model.recents.is_empty()
            && self.pois.is_empty();
        if empty {
            let (title_h, body_h) = (line(16.0), line(13.0));
            PaintCtx::new(scene, t, tr, theme).text(
                "Find your way",
                &TextStyle::new(16.0).with_weight(TextWeight::Bold),
                theme.text_primary,
                Point::new(x, y),
                None,
            );
            let body_y = y + title_h + s2;
            draw::text(
                scene,
                tr,
                t,
                "Search for a place or address",
                x,
                body_y,
                13.0,
                theme.text_dim,
            );
            draw::text(
                scene,
                tr,
                t,
                "to route and plan transit.",
                x,
                body_y + body_h,
                13.0,
                theme.text_dim,
            );
            return body_y + 2.0 * body_h + s4;
        }

        // Selected place card.
        if let Some(sel) = self.model.selected_place().cloned() {
            let (name_h, meta_h) = (line(18.0), line(12.0));
            let bh = 34.0;
            let name_y = y + s3;
            let cat_y = name_y + name_h + s1;
            let addr_y = cat_y + meta_h;
            let by = addr_y + meta_h + s3;
            let card_h = (by + bh + s3 - y).ceil();
            let cr = Rect::new(x, y, right, y + card_h);
            // The base tier seated in the raised panel, with its derived
            // edge — a contrast step on both themes, never a bare fill.
            draw::edged(scene, t, cr, 12.0, c.surfaces.base);
            // Accent identifies the current selection. Category is written
            // directly below, so it does not need a competing colour stripe.
            draw::fill_round(
                scene,
                t,
                Rect::new(cr.x0, cr.y0, cr.x0 + 5.0, cr.y1),
                12.0,
                theme.accent_primary,
            );
            let name =
                tr.elide_to_width(&sel.name, 18.0, ui::num::f32_of_f64(right - x - 2.0 * s4));
            PaintCtx::new(scene, t, tr, theme).text(
                &name,
                &TextStyle::new(18.0).with_weight(TextWeight::Bold),
                theme.text_primary,
                Point::new(x + s4, name_y),
                None,
            );
            draw::text(
                scene,
                tr,
                t,
                sel.category.label(),
                x + s4,
                cat_y,
                12.0,
                theme.accent_primary,
            );
            draw::text_clip(
                &mut PaintCtx::new(scene, t, tr, theme),
                &sel.address,
                Point::new(x + s4, addr_y),
                12.0,
                theme.text_dim,
                right - x - 2.0 * s4,
            );

            // Action buttons row: the shared button face, three siblings an
            // `s3` apart on the card's `s4` gutter (aligned with the text).
            let bw = (right - x - 2.0 * s4 - 2.0 * s3) / 3.0;
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
                let bx = x + s4 + i as f64 * (bw + s3);
                let br = Rect::new(bx, by, bx + bw, by + bh);
                let mut face = ButtonFace::new(br, label).text_size(12.0);
                if primary {
                    face = face.primary();
                }
                Button::paint(scene, t, tr, theme, &face);
                if matches!(hit, Hit::ToggleSave) && saved {
                    // Chosen state is a lift with a derived edge, not an
                    // accent wash (HIG §9.3).
                    let tier = selection_tier(theme, SelectionState::active(true));
                    draw::fill_round(scene, t, br, 8.0, tier.fill);
                    if let Some(border) = tier.border {
                        draw::stroke_rrect(scene, t, br, 8.0, 1.0, border);
                    }
                    draw::text_center(
                        &mut PaintCtx::new(scene, t, tr, theme),
                        label,
                        Point::new(br.center().x, self.vmid(br.y0, bh, 12.0)),
                        12.0,
                        theme.text_primary,
                    );
                }
                self.click_targets.push((br, hit));
            }
            y += card_h + s4;
        }

        // Results / recents / saved sections.
        let results = self.model.results.clone();
        if !results.is_empty() {
            PaintCtx::new(scene, t, tr, theme).text(
                "RESULTS",
                &TextStyle::new(11.0).with_weight(TextWeight::Semibold),
                theme.text_dim,
                Point::new(x, y),
                None,
            );
            y += label_advance;
            for p in &results {
                y = self.draw_place_row(scene, t, tr, theme, content, y, p);
            }
            y += s2;
        }

        // Nearby POIs fetched for the current viewport (Overpass), nearest-first
        // and linked to the marker layer: tapping a row centres the map and
        // opens the POI's info card, and the selected marker highlights its row.
        let nearby = self.nearby_pois();
        if !nearby.is_empty() {
            PaintCtx::new(scene, t, tr, theme).text(
                "NEARBY",
                &TextStyle::new(11.0).with_weight(TextWeight::Semibold),
                theme.text_dim,
                Point::new(x, y),
                None,
            );
            y += label_advance;
            let anchor = self.search_anchor();
            for poi in &nearby {
                y = self.draw_poi_row(
                    &mut PaintCtx::new(scene, t, tr, theme),
                    content,
                    y,
                    poi,
                    anchor,
                );
            }
            y += s2;
        }

        let saved = self
            .model
            .saved_places()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        if !saved.is_empty() {
            PaintCtx::new(scene, t, tr, theme).text(
                "SAVED",
                &TextStyle::new(11.0).with_weight(TextWeight::Semibold),
                theme.text_dim,
                Point::new(x, y),
                None,
            );
            y += label_advance;
            for p in &saved {
                y = self.draw_place_row(scene, t, tr, theme, content, y, p);
            }
            y += s2;
        }

        if !self.model.recents.is_empty() {
            let clear_label = "Clear";
            let clear_w = draw::measure_w(tr, clear_label, 11.0) + 12.0;
            let clear = Rect::new(right - clear_w, y - 5.0, right, y + 15.0);
            PaintCtx::new(scene, t, tr, theme).text(
                "RECENT SEARCHES",
                &TextStyle::new(11.0).with_weight(TextWeight::Semibold),
                theme.text_dim,
                Point::new(x, y),
                None,
            );
            draw::text_right(
                scene,
                tr,
                t,
                clear_label,
                right,
                y,
                11.0,
                theme.accent_primary,
            );
            self.click_targets.push((clear, Hit::ClearRecents));
            y += label_advance;
            let recents = self.model.recents.clone();
            let recent_h = f64::from(self.spacing.s8);
            for q in &recents {
                let rr = Rect::new(x, y, right, y + recent_h);
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
                    self.vmid(rr.y0, recent_h, 12.0),
                    12.0,
                    theme.text_primary,
                );
                self.click_targets.push((rr, Hit::Recent(q.clone())));
                y += recent_h + s1;
            }
        }
        y
    }

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
        let s2 = f64::from(self.spacing.s2);
        let r = Rect::new(content.x0, y, content.x1, y + ROW_H - s2);
        let selected = self.model.selected.as_deref() == Some(p.id.as_str());
        if selected {
            draw::selected_row(scene, t, r, 8.0, theme);
        }
        // Lists use one quiet marker treatment; the category label carries the
        // information without turning saved places into a rainbow of competing
        // priorities. Saturated category colours stay on actual map markers,
        // where they help scan spatially distinct places.
        let marker = draw::mix(theme.accent_primary, c.surfaces.base, 0.78);
        draw::fill_circle(scene, t, Point::new(r.x0 + 18.0, r.center().y), 9.0, marker);
        draw::fill_circle(
            scene,
            t,
            Point::new(r.x0 + 18.0, r.center().y),
            3.2,
            theme.accent_primary,
        );
        // Name and subtitle stack on their measured lines, the block centred
        // on the row like the marker beside it.
        let (name_h, sub_h) = (line_h(13.0, self.text_scale), line_h(11.0, self.text_scale));
        let name_y = r.y0 + (r.height() - name_h - sub_h) / 2.0;
        let tx = r.x0 + 36.0;
        draw::text_clip(
            &mut PaintCtx::new(scene, t, tr, theme),
            &p.name,
            Point::new(tx, name_y),
            13.0,
            theme.text_primary,
            r.x1 - tx - 50.0,
        );
        let sub = format!("{} · {}", p.category.label(), p.address);
        draw::text_clip(
            &mut PaintCtx::new(scene, t, tr, theme),
            &sub,
            Point::new(tx, name_y + name_h),
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
                r.x1 - s2,
                self.vmid(r.y0, r.height(), 12.0),
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
    fn draw_poi_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        content: Rect,
        y: f64,
        poi: &crate::model::Poi,
        anchor: LatLon,
    ) -> f64 {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let s2 = f64::from(self.spacing.s2);
        let r = Rect::new(content.x0, y, content.x1, y + ROW_H - s2);
        let selected = self.selected_poi.as_deref() == Some(poi.id.as_str());
        if selected {
            draw::selected_row(ctx.scene, ctx.transform, r, 8.0, theme);
        }
        // The pictogram, category label and map marker already identify this
        // POI. Keeping list decoration neutral makes distance and selection
        // easier to scan across mixed categories.
        let marker = draw::mix(theme.accent_primary, c.surfaces.base, 0.78);
        let cc = Point::new(r.x0 + 18.0, r.center().y);
        draw::fill_circle(ctx.scene, ctx.transform, cc, 11.0, marker);
        canvas::draw_category_icon(
            ctx.scene,
            ctx.transform,
            cc,
            11.0,
            poi.category,
            theme.text_primary,
            marker,
        );

        let dist = geo::fmt_distance(geo::haversine_m(poi.pos, anchor));
        let dw = draw::measure_w(ctx.text, &dist, 11.0) + 10.0;
        let (name_h, sub_h) = (line_h(13.0, self.text_scale), line_h(11.0, self.text_scale));
        let name_y = r.y0 + (r.height() - name_h - sub_h) / 2.0;
        let tx = r.x0 + 38.0;
        draw::text_clip(
            ctx,
            &poi.name,
            Point::new(tx, name_y),
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
            ctx,
            &sub,
            Point::new(tx, name_y + name_h),
            11.0,
            theme.text_dim,
            r.x1 - tx - dw,
        );
        // The distance chip sits on the name line's centre.
        draw::text_right(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &dist,
            r.x1 - s2,
            name_y + (name_h - sub_h) / 2.0,
            11.0,
            theme.accent_primary,
        );
        self.click_targets.push((r, Hit::SelectPoi(poi.id.clone())));
        y + ROW_H
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

        let selected_destination = self.model.selected_place().map(|place| place.name.as_str());
        let frame = EndpointFrame {
            content,
            top: y,
            origin: self.model.origin.and_then(|_| {
                (!self.model.origin_name.is_empty()).then_some(self.model.origin_name.as_str())
            }),
            destination: (!self.model.dest_name.is_empty())
                .then_some(self.model.dest_name.as_str())
                .or(selected_destination),
        };
        y = self.endpoints.draw_fields(scene, t, tr, theme, &frame);

        // "Use my location" — a real device-location fix for the origin. A
        // compact, content-sized chip (not a full-width bar): it is a secondary
        // helper, so it stays quiet and lets the fields + mode read as the
        // primary controls.
        // Row heights in this panel are the authored 1× values or the measured
        // label line plus its padding, whichever is taller, so scaled type
        // never spills out of a chip, a segment or a step band.
        let line = |tr: &mut TextRenderer, size: f32| f64::from(tr.measure("Ag", size, None).1);
        // Authored 1× rows grown by their line's extra height (whole pixels;
        // see `authored_h`), never `line + pad` floored at the design.
        let loc_h = authored_h(30.0, &[13.0], tr.text_scale());
        let loc_label = if self.locating {
            "Locating…"
        } else {
            "Use my location"
        };
        let loc_w = draw::measure_w(tr, loc_label, 13.0) + 44.0;
        let loc_r = Rect::new(x, y, x + loc_w, y + loc_h);
        let loc_fg = theme.accent_primary;
        if self.locating {
            // A translucent accent wash: the surface the eye sees is the
            // panel beneath it, so the edge derives from that ground, not
            // from the wash's accent RGB (which would vanish on light).
            draw::fill_round(
                scene,
                t,
                loc_r,
                8.0,
                draw::alpha(theme.accent_primary, 0.16),
            );
            scene.stroke(
                &Stroke::new(1.0),
                t,
                ui::surface::edge_on(c.surfaces.raised),
                None,
                &ui::surface::edge_outline(loc_r, 8.0),
            );
        } else {
            draw::edged(scene, t, loc_r, 8.0, c.surfaces.base);
        }
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
            self.vmid(loc_r.y0, loc_h, 13.0),
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
        y += loc_h + f64::from(self.spacing.s4);

        // NOTE: the geocoded results for the focused endpoint are NOT drawn
        // inline here — that reflowed the mode control and route below every
        // keystroke. They render in `draw_endpoint_overlay` as a floating
        // dropdown anchored under the field, over the map, so the panel below
        // stays put whether or not the dropdown is showing.

        // Mode segmented control — Drive / Transit / Cycle / Walk. Transit is
        // a mode here, not a separate tab: selecting it swaps the route summary
        // below for the public-transport departures.
        let mode_h = authored_h(40.0, &[12.0], tr.text_scale());
        let mode_rect = Rect::new(x, y, right, y + mode_h);
        // The track is the base tier seated in the panel with its derived
        // edge; the active segment sits an `s1` inside it.
        let track_radius = 9.0;
        let seg_inset = f64::from(self.spacing.s1);
        draw::edged(scene, t, mode_rect, track_radius, c.surfaces.base);
        let seg_w = mode_rect.width() / TravelMode::ALL.len() as f64;
        let active_fg = c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST);
        for (i, m) in TravelMode::ALL.iter().enumerate() {
            let sx = mode_rect.x0 + i as f64 * seg_w;
            let seg = Rect::new(sx, mode_rect.y0, sx + seg_w, mode_rect.y1);
            let active = self.model.mode == *m;
            if active {
                draw::fill_round(
                    scene,
                    t,
                    seg.inflate(-seg_inset, -seg_inset),
                    track_radius - seg_inset,
                    theme.accent_primary,
                );
            }
            let fg = if active { active_fg } else { theme.text_dim };
            draw::text_center(
                &mut PaintCtx::new(scene, t, tr, theme),
                m.label(),
                Point::new(seg.center().x, self.vmid(seg.y0, seg.height(), 12.0)),
                12.0,
                fg,
            );
            self.click_targets.push((seg, Hit::Mode(*m)));
        }
        y += mode_h + f64::from(self.spacing.s3);

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
            PaintCtx::new(scene, t, tr, theme).text(
                &big,
                &TextStyle::new(26.0).with_weight(TextWeight::Bold),
                theme.accent_primary,
                Point::new(x, y),
                None,
            );
            // The distance and the Clear link sit on the duration's line box,
            // each centred from its own measured height; the row then advances
            // by that measured line so the arrival clock never overprints it.
            let big_h = line(tr, 26.0);
            let dist = geo::fmt_distance(route.distance_m);
            let bw = draw::measure_w(tr, &big, 26.0);
            draw::text(
                scene,
                tr,
                t,
                &format!("· {dist}"),
                x + bw + 12.0,
                self.vmid(y, big_h, 14.0),
                14.0,
                theme.text_dim,
            );
            // Clear route link.
            let clr_w = draw::measure_w(tr, "Clear", 12.0) + 24.0;
            let clr = Rect::new(right - clr_w, y, right, y + big_h);
            draw::text_right(
                scene,
                tr,
                t,
                "Clear",
                right - 4.0,
                self.vmid(y, big_h, 12.0),
                12.0,
                theme.tokens.colors.semantic.danger,
            );
            self.click_targets.push((clr, Hit::ClearRoute));
            y += big_h + f64::from(self.spacing.s2);

            // Planned departure → arrival clock, derived from the time picker.
            // Leaving now, a single ETA reads cleaner than "Leave x → Arrive y";
            // the full pair only shows once a specific departure/arrival is set.
            let (dep_label, arr_label) = self.road_trip_clock(route.duration_s);
            if self.model.transit_time.is_now() {
                PaintCtx::new(scene, t, tr, theme).text(
                    &arr_label,
                    &TextStyle::new(13.0).with_weight(TextWeight::Medium),
                    theme.text_dim,
                    Point::new(x, y),
                    None,
                );
            } else {
                PaintCtx::new(scene, t, tr, theme).text(
                    &dep_label,
                    &TextStyle::new(13.0).with_weight(TextWeight::Medium),
                    theme.text_primary,
                    Point::new(x, y),
                    None,
                );
                let dep_w = draw::measure_w(tr, &dep_label, 13.0);
                draw::text(scene, tr, t, "→", x + dep_w + 10.0, y, 13.0, theme.text_dim);
                PaintCtx::new(scene, t, tr, theme).text(
                    &arr_label,
                    &TextStyle::new(13.0).with_weight(TextWeight::Medium),
                    theme.text_primary,
                    Point::new(x + dep_w + 30.0, y),
                    None,
                );
            }
            y += authored_h(28.0, &[13.0], tr.text_scale());

            // Summary and steps are two groups: a derived rule, then `s4`.
            draw::separator(scene, t, x, right, y, c.surfaces.raised);
            y += f64::from(self.spacing.s4);

            // Each step band holds an instruction line and a distance line on
            // its own measured row beneath it — at 1× the authored 46px band,
            // taller once the type grows so the two never overprint.
            let instr_h = line(tr, 13.0);
            let rh = authored_h(46.0, &[13.0, 11.0], tr.text_scale());
            for (i, step) in route.steps.iter().enumerate() {
                // Glyph badge, centred on the instruction line.
                let bc = Point::new(x + 16.0, y + instr_h / 2.0 + 2.0);
                draw::fill_circle(scene, t, bc, 13.0, draw::alpha(theme.accent_primary, 0.16));
                draw::text_center(
                    &mut PaintCtx::new(scene, t, tr, theme),
                    step.maneuver.glyph(),
                    Point::new(bc.x, self.vmid(bc.y - 13.0, 26.0, 15.0)),
                    15.0,
                    theme.accent_primary,
                );
                draw::text_clip(
                    &mut PaintCtx::new(scene, t, tr, theme),
                    &step.instruction,
                    Point::new(x + 38.0, y),
                    13.0,
                    theme.text_primary,
                    right - x - 38.0,
                );
                if step.distance_m >= 1.0 {
                    // The distance line sits on its own measured box directly
                    // under the instruction's.
                    draw::text_right(
                        scene,
                        tr,
                        t,
                        &geo::fmt_distance(step.distance_m),
                        right - f64::from(self.spacing.s1),
                        y + instr_h,
                        11.0,
                        theme.text_dim,
                    );
                }
                // Connector line between steps.
                if i + 1 < route.steps.len() {
                    // Down to the top of the next step's badge.
                    draw::line(
                        scene,
                        t,
                        Point::new(bc.x, bc.y + 13.0),
                        Point::new(bc.x, y + rh + instr_h / 2.0 + 2.0 - 13.0),
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
            y += line(tr, 13.0) + f64::from(self.spacing.s2);
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
        // The authored 36px row, or the tallest label it carries (the 18pt
        // stepper glyphs) plus padding once the type has grown.
        let row_h = authored_h(36.0, &[18.0], tr.text_scale());
        let (s1, s2) = (f64::from(self.spacing.s1), f64::from(self.spacing.s2));
        let toggle_w = 150.0_f64.min((right - x - s2) * 0.5);

        // Travel mode above is the person's first decision. Timing is a
        // refinement, so its selector is deliberately quieter: a soft active
        // state with a focused edge rather than a second full-accent control.
        let seg_rect = Rect::new(x, y, x + toggle_w, y + row_h);
        let track_radius = 9.0;
        draw::edged(scene, t, seg_rect, track_radius, c.surfaces.base);
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
                let active_rect = seg.inflate(-s1, -s1);
                draw::fill_round(
                    scene,
                    t,
                    active_rect,
                    track_radius - s1,
                    draw::alpha(theme.accent_primary, 0.14),
                );
                draw::stroke_rrect(
                    scene,
                    t,
                    active_rect,
                    track_radius - s1,
                    1.0,
                    theme.accent_primary,
                );
            }
            let fg = if active {
                theme.accent_primary
            } else {
                theme.text_dim
            };
            draw::text_center(
                &mut PaintCtx::new(scene, t, tr, theme),
                label,
                Point::new(seg.center().x, self.vmid(seg.y0, seg.height(), 12.0)),
                12.0,
                fg,
            );
            self.click_targets.push((seg, Hit::TransitTimeMode(*m)));
        }

        // Stepper: [–] <clock> [+], sharing the row with the toggle.
        let btn_w = 38.0;
        let minus = Rect::new(seg_rect.x1 + s2, y, seg_rect.x1 + s2 + btn_w, y + row_h);
        let plus = Rect::new(right - btn_w, y, right, y + row_h);
        for (rect, label, name) in [(minus, "–", "Earlier"), (plus, "+", "Later")] {
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::new(rect, label)
                    .text_size(18.0)
                    .accessible_name(name),
            );
        }
        let target = self.clock_anchor() + chrono::Duration::minutes(i64::from(tt.offset_min));
        let label = if tt.is_now() {
            "Now".to_string()
        } else {
            target.format("%H:%M").to_string()
        };
        draw::text_center(
            &mut PaintCtx::new(scene, t, tr, theme),
            &label,
            Point::new((minus.x1 + plus.x0) / 2.0, self.vmid(y, row_h, 15.0)),
            15.0,
            theme.accent_primary,
        );
        self.click_targets.push((minus, Hit::TransitTimeDelta(-15)));
        self.click_targets.push((plus, Hit::TransitTimeDelta(15)));
        y += row_h;

        // A compact "Reset to now" appears only when the anchor is off default —
        // the common leave-now case stays a single, quiet row.
        if !tt.is_now() {
            y += s2;
            let reset_h = authored_h(26.0, &[12.0], tr.text_scale());
            let reset_w =
                draw::measure_w(tr, "Reset to now", 12.0) + 2.0 * f64::from(self.spacing.s3);
            let reset = Rect::new(x, y, x + reset_w, y + reset_h);
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::new(reset, "Reset to now").text_size(12.0),
            );
            self.click_targets.push((reset, Hit::TransitTimeNow));
            y += reset_h;
        }

        // Breathing space rather than a hairline — groups separate by space, not
        // boxes-within-boxes (HIG §7): `s4` before the next group.
        y + f64::from(self.spacing.s4)
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
        let (s1, s2, s3) = (
            f64::from(self.spacing.s1),
            f64::from(self.spacing.s2),
            f64::from(self.spacing.s3),
        );
        PaintCtx::new(scene, t, tr, theme).text(
            &format!("Departures to {to}"),
            &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
            theme.text_primary,
            Point::new(x, y),
            None,
        );
        y += line_h(14.0, self.text_scale) + s2;

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
            return y + line_h(13.0, self.text_scale) + s2;
        }

        let journeys = self.model.transit.clone();
        for (i, j) in journeys.iter().enumerate() {
            let selected = self.model.transit_choice == i;
            let base_h = 86.0;
            let jr = Rect::new(x, y, right, y + base_h);
            // Each itinerary is the base tier in the panel with its derived
            // edge; the chosen one is a translucent accent wash whose edge
            // derives from the panel it sits on (the surface actually seen),
            // then the accent ring.
            if selected {
                draw::fill_round(scene, t, jr, 10.0, draw::alpha(theme.accent_primary, 0.12));
                scene.stroke(
                    &Stroke::new(1.0),
                    t,
                    ui::surface::edge_on(c.surfaces.raised),
                    None,
                    &ui::surface::edge_outline(jr, 10.0),
                );
                draw::stroke_rrect(scene, t, jr, 10.0, 1.5, theme.accent_primary);
            } else {
                draw::edged(scene, t, jr, 10.0, c.surfaces.base);
            }
            // Header: dep–arr  ·  duration  ·  transfers.
            PaintCtx::new(scene, t, tr, theme).text(
                &format!("{} – {}", j.dep, j.arr),
                &TextStyle::new(15.0).with_weight(TextWeight::Bold),
                theme.text_primary,
                Point::new(x + 12.0, y + 10.0),
                None,
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
                        .unwrap_or_else(|| theme.tokens.colors.on_surface(col).ink);
                    let lbl = if leg.line.is_empty() {
                        leg.mode.label().to_string()
                    } else {
                        leg.line.clone()
                    };
                    let w = draw::measure_w(tr, &lbl, 12.0) + 16.0;
                    let pill = Rect::new(px, py, px + w, py + 22.0);
                    draw::fill_round(scene, t, pill, 6.0, col);
                    draw::text_center(
                        &mut PaintCtx::new(scene, t, tr, theme),
                        &lbl,
                        Point::new(pill.center().x, self.vmid(pill.y0, 22.0, 12.0)),
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
            y += base_h + s3;

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
                        &mut PaintCtx::new(scene, t, tr, theme),
                        &head,
                        Point::new(x + 32.0, y),
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
                        &mut PaintCtx::new(scene, t, tr, theme),
                        &sub,
                        Point::new(x + 32.0, y + 16.0),
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
                    y += lh + s1;
                }
                y += s2;
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
        draw::separator(scene, t, x, right, y, c.surfaces.raised);
        y += s3;
        draw::text_clip(
            &mut PaintCtx::new(scene, t, tr, theme),
            &src,
            Point::new(x, y),
            10.0,
            theme.text_dim,
            right - x,
        );
        y += line_h(10.0, self.text_scale) + s2;
        y
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
            Hit::Consume | Hit::Search(SearchHit::Consume) | Hit::CloseNav => return None,
            Hit::Search(SearchHit::Field) => (
                "Search places".to_string(),
                "maps.search".to_string(),
                Role::TextInput,
                AccessState::new().focusable(true),
            ),
            Hit::Search(SearchHit::Clear) => (
                "Clear search".to_string(),
                "maps.search.clear".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::Search(SearchHit::Result(index)) => {
                let id = self
                    .search
                    .result_id(*index)
                    .map_or_else(|| index.to_string(), str::to_string);
                (
                    self.search.result_name(*index).map_or_else(
                        || format!("Search result {}", index + 1),
                        |name| format!("Open {name}"),
                    ),
                    format!("maps.search-result:{id}"),
                    Role::Button,
                    AccessState::new().focusable(true),
                )
            }
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
                    "Close map navigation"
                } else {
                    "Open map navigation"
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
                "Leave now".to_string(),
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
                target.target == "maps.navigation" || panel.contains(target.rect.center())
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
        dyn ui::resources::PublishesResources
    );

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
        // Fire a debounced search-as-you-type geocode once typing settles.
        self.tick_search_debounce(ui::time::now());
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
            if MapsDoc::load(state).is_some() {
                // Workspace resources merge into this window; the camera,
                // panel, draft, selection and status remain local.
                MapsDoc::apply_durable(state, &mut self.model);
            } else {
                MapsDoc::save(state, &self.model);
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
        self.navigation_placement
            .set_preferred_size(navigation_card_size(&self.metrics, theme.text_scale));
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
                    self.search.dismiss();
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
                    // A result popover is transient. Keep it open for its own
                    // field/padding, but dismiss it before any other action.
                    if !matches!(hit, Hit::Search(SearchHit::Field | SearchHit::Consume)) {
                        self.search.dismiss();
                    }
                    self.endpoints.dismiss();
                    self.dispatch(&hit, ctx.state);
                    // A click that focused a text field forwards the press so the
                    // shared TextInput places its caret at the click point (the
                    // preceding CursorMoved already set its hover flag).
                    if matches!(hit, Hit::Search(SearchHit::Field)) {
                        self.forward_press_to_field(&hit, event, ctx.scale_factor, *ctx.modifiers);
                    }
                    consumed = true;
                    *ctx.handled = true;
                }
                if !consumed {
                    self.search.dismiss();
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
                    && self.last_mouse.y > self.panel_rect().y0 + 50.0;
                if over_panel {
                    let max = (self.content_h - self.content_view_h).max(0.0);
                    self.scroll = (self.scroll - dy * 36.0).clamp(0.0, max);
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
                let field_focused = self.endpoints.is_focused() || self.search.is_focused();
                if field_focused {
                    if self.route_field_key(event, ctx.scale_factor, *ctx.modifiers, ctx.state) {
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
        // The TextInput needs a genuine pointer press to place its caret, so
        // leave it to the framework's exact-rect fallback below. Every other
        // painted action is dispatched directly by its stable semantic target.
        if target == "maps.search" {
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
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        [
            self.search.query_deadline(),
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
    /// Publish the resource tree the shell's one Ctrl+K palette walks — the saved
    /// places, the active route, and the offline regions currently in the shared
    /// document. Delegates to the
    /// [`ResourceProvider`](tilo_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        let mut resources = <Self as tilo_actions::ResourceProvider>::resources(self, doc);
        if resources.is_empty() {
            resources.push(Resource::new("maps.workspace", "maps.workspace", "Maps"));
        }
        if let Some(workspace) = resources.first_mut() {
            workspace
                .children
                .extend(self.model.results.iter().take(12).map(|place| {
                    Resource::new(
                        format!("maps.search-result:{}", place.id),
                        "maps.search-result",
                        place.name.clone(),
                    )
                }));
            workspace.children.extend(
                self.nearby_pois()
                    .into_iter()
                    .take(12)
                    .map(|poi| Resource::new(format!("maps.poi:{}", poi.id), "maps.poi", poi.name)),
            );
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
        let mut resource = <Self as tilo_actions::ResourceProvider>::resource(self, doc, id)
            .or_else(|| {
                (id == "maps.workspace")
                    .then(|| Resource::new("maps.workspace", "maps.workspace", "Maps"))
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
