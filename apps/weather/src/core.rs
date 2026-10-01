//! Shared weather state and the two **content panels** it renders.
//!
//! Per the HIG §1b, weather is *not* a sidebar+content app — it is an
//! **overview + detail** pair of content panels:
//!
//! * the **overview** is a *contentful* city list (each row carries quick
//!   per-city info — condition icon + current temperature); and
//! * the **detail** is the full forecast for the selected city.
//!
//! The responsive shell ([`ui::widget::NavShell`]) owns the breakpoint,
//! the side-by-side↔push transition, the Back affordance, and the divider.
//! This module owns the *content* of the two panes plus the state they
//! share (the model, the search box, direct-manipulation of the list) and
//! the network request queue the app pumps.
//!
//! [`Core`] is held once behind an `Rc<RefCell<…>>`; the two pane widgets
//! (see [`crate::panes`]) borrow it to draw and to route input, and the root
//! [`crate::app::WeatherApp`] borrows it to seed the CRDT, pump the network
//! worker, and drive the shell.

use std::collections::HashMap;
use ui::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Brush, Color, Fill, Gradient};
use winit::event::WindowEvent;

use ui::a11y::{AccessNode, AccessState, Role};
use ui::charts::{BarGauge, GaugeStyle, TrendRange};
use ui::color_compat::ColorCompat;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::tokens::Metrics;
use ui::tokens::spacing::Spacing;
use ui::tokens::typography::TypeScale;
use ui::widget::{
    Autocomplete, AutocompleteOutcome, DragController, ListMetrics, PressOutcome, ScrollMetrics,
    Scrollbar, Suggestion, Widget,
};
use ui::widget::{Button, ButtonFace};

use crate::actions::WeatherAction;
use crate::api;
use crate::chart;
use crate::icon;
use crate::interaction::{ContextMenu, EntryAction, PointerPress, SWIPE_COMMIT_DX};
use crate::model::{
    AirQuality, CurrentConditions, DayForecast, LocationId, SavedLocation, WeatherModel,
    WeatherSnapshot,
};
use crate::net::{Request, RequestKind, Response};
use crate::presentation::SavedPlaceFace;
use crate::units::{self, Units};
use crate::wxcode::Sky;

/// Page gutter of the detail pane — the theme's `s6` (HIG §7b), the same
/// step the overview keeps, so both panes share one edge rhythm.
const PAD: f64 = Spacing::BASE.s6 as f64;
/// Overview inner padding — the same `s6` page gutter.
const RP: f64 = Spacing::BASE.s6 as f64;
const CHIP_H: f64 = 62.0; // authored 1× saved-place chip height (see `ChipMetrics`)
/// Base text size of a saved-place chip's city name (15pt sits between the
/// type scale's `base` and `lg`); its condition line reads `sm`, the trailing
/// temperature `x2l`, and the gap between chips is the theme's `s2`.
pub(crate) const CHIP_NAME_TEXT: f32 = 15.0;
/// Authored 1× condition glyph size inside a chip.
const CHIP_ICON: f64 = 40.0;
/// Authored 1× height of the detail header (city, region, date/time, refresh)
/// measured from its top to the body's first section.
const DETAIL_HEADER_H: f64 = 74.0;
/// Text sizes the detail header stacks: the city name and its region line.
const DETAIL_TITLE_TEXT: f32 = 26.0;
const DETAIL_SUBTITLE_TEXT: f32 = 13.0;

/// Height of one line of `size`-pt text at `text_scale` — the same value
/// `TextRenderer::measure` reports for a single line, so the row grid, the hit
/// rects, the reorder metrics and the painter agree at every scale. The
/// crate's one copy of the line-height formula.
fn line_h(size: f32, text_scale: f32) -> f64 {
    ui::text_metrics::line_height_px(size, text_scale)
}

/// How much taller one line of `size`-pt text is at `text_scale` than at 1×;
/// zero at or below 1×, so nothing ever shrinks under its authored design.
fn line_growth(size: f32, text_scale: f32) -> f64 {
    ui::text_metrics::line_growth(size, text_scale)
}

/// The authored (or token) 1× minimum, or the measured line plus its theme
/// pad rounded up to a whole pixel once scaled type needs more. Reserve
/// (layout) and paint read the same tokens, so a row is never a fraction
/// short of the text plus the gap the painter leaves.
fn at_least_whole(minimum: f64, measured: f64) -> f64 {
    ui::text_metrics::at_least_whole(minimum, measured)
}

/// Base (unscaled) text sizes handed to the legacy renderer, read from the
/// default type scale so the app states no sizes of its own; the renderer
/// applies the accessibility scale. Sizes off the scale are named beside the
/// surfaces that use them (or in the body's design table).
fn text_xs() -> f32 {
    TypeScale::default().xs
}
pub(crate) fn text_sm() -> f32 {
    TypeScale::default().sm
}
fn text_base() -> f32 {
    TypeScale::default().base
}
pub(crate) fn text_x2l() -> f32 {
    TypeScale::default().x2l
}

/// Height of the detail header (city, region, date/time, refresh) from its
/// top to the body's first section: the authored 74px at 1× (the name and
/// region lines with their `s1` gap plus an `s4` below sit inside it), or
/// that measured stack rounded up once scaled type needs more.
fn detail_header_h(spacing: &Spacing, text_scale: f32) -> f64 {
    at_least_whole(
        DETAIL_HEADER_H,
        line_h(DETAIL_TITLE_TEXT, text_scale)
            + f64::from(spacing.s1)
            + line_h(DETAIL_SUBTITLE_TEXT, text_scale)
            + f64::from(spacing.s4),
    )
}

/// An authored 1× height grown by exactly the extra room its text lines need
/// at `text_scale`, snapped up to a whole pixel. The authored value already
/// holds its 1× lines plus the padding the design gave them, so at 1× this is
/// the design exactly (the 74px header stays 74, not `32.5 + 4 + 16.25 + 24 =
/// 76.75`) and at 2× the row grows by the lines' growth alone.
fn authored_h(authored: f64, sizes: &[f32], text_scale: f32) -> f64 {
    ui::text_metrics::authored_h(authored, sizes, text_scale)
}

/// Saved-place row geometry for the current text scale: the authored 62px
/// chip at 1×, growing once the measured name + condition lines need more.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ChipMetrics {
    pub(crate) chip_h: f64,
    /// Gap between consecutive chips — the theme's `s2`.
    pub(crate) gap: f64,
    pub(crate) name_h: f64,
    pub(crate) sub_h: f64,
    pub(crate) temp_h: f64,
    /// Condition glyph size, never taller than the text block beside it.
    pub(crate) icon: f64,
}

impl ChipMetrics {
    pub(crate) fn new(spacing: &Spacing, text_scale: f32) -> Self {
        let name_h = line_h(CHIP_NAME_TEXT, text_scale);
        let sub_h = line_h(text_sm(), text_scale);
        let temp_h = line_h(text_x2l(), text_scale);
        // The authored 62px row at 1× (33.75 + 24 sits inside it), or the two
        // lines plus the `s3` the painter leaves above and below them,
        // rounded up — always a whole pixel (92 at 2×).
        let chip_h = at_least_whole(CHIP_H, name_h + sub_h + 2.0 * f64::from(spacing.s3));
        Self {
            chip_h,
            gap: f64::from(spacing.s2),
            name_h,
            sub_h,
            temp_h,
            icon: (name_h + sub_h).max(CHIP_ICON),
        }
    }

    pub(crate) fn pitch(self) -> f64 {
        self.chip_h + self.gap
    }
}
/// Minimum width of one metric tile before the row wraps.
const TILE_MIN_W: f64 = 118.0;
/// Minimum width of one hour column; below this the strip scrolls horizontally
/// rather than squeezing all 24 hours into the card width.
const HOUR_COL_MIN_W: f64 = 58.0;
/// Extra top padding reserved in the detail header for the shell's Back
/// affordance (chip + label) when the detail is pushed on a narrow layout.
const BACK_RESERVE: f64 = 30.0;
/// How long a recently-typed query stays "fresh" before we fire a geocode.
const GEOCODE_DEBOUNCE: Duration = Duration::from_millis(220);
/// Below this allocated width the overview renders as a collapsed **rail**
/// (glanceable icon column) instead of the full city list. The rail width
/// (see [`crate::app`]) and the side-by-side overview width sit either side
/// of this threshold, so the render mode follows the allocation with no
/// separate cached flag.
const RAIL_MAX_WIDTH: f64 = 120.0;
/// Height of one city slot in the collapsed rail.
const RAIL_SLOT_H: f64 = 62.0;

/// An intent the overview raises for the shell to act on (push the detail
/// in). Consumed by the root widget after routing an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavIntent {
    ShowDetail,
}

/// A click target in the overview, rebuilt each draw and hit-tested on input.
struct Hotspot {
    rect: Rect,
    action: OverAction,
}

#[derive(Clone, Copy)]
enum OverAction {
    SelectLocation(usize),
    MenuItem(usize, EntryAction),
    /// Begin a potential press on a chip (click / reorder / swipe).
    PressChip(usize),
    /// Collapse the full city list to a rail, or expand the rail back.
    ToggleCollapse,
}

/// All mutable weather state shared by the two panes and the root widget.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, orthogonal flag (a geocode request in flight, the \
              detail pane reserving space for the Back affordance, the collapse chip offered \
              this frame, a queued collapse toggle, and a pending CRDT write); they aren't a \
              state machine, and grouping them would mean churning every field access across \
              this widget and its panes for no behavioural gain"
)]
pub(crate) struct OverviewEventCtx<'a> {
    pub(crate) event: &'a WindowEvent,
    pub(crate) scale_factor: f64,
    pub(crate) modifiers: winit::event::Modifiers,
    pub(crate) state: &'a mut AutoCommit,
    pub(crate) path: &'a ObjId,
    pub(crate) focus_target: &'a mut Option<NodeId>,
    pub(crate) node_id: NodeId,
    pub(crate) taffy: &'a TaffyTree<()>,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent view flags of the app root, not a state machine"
)]
pub struct Core {
    pub model: WeatherModel,
    /// Display units — read from the shared Settings store (`region`), never
    /// toggled inside this app.
    pub units: Units,

    last_mouse: Point,
    /// The renderer's text scale, captured at the top of each paint so the
    /// saved-place rows, their hit rects, the reorder metrics and the scroll
    /// extent all size from the same measured lines.
    text_scale: f32,
    /// The theme's spacing and density metrics, cached from the app's layout
    /// pass so the renderer-less row grid reads the same tokens the painters
    /// do.
    spacing: Spacing,
    metrics: Metrics,

    // --- overview: saved-place direct manipulation ---
    /// Press classifier: distinguishes a click from a vertical reorder-drag
    /// from a horizontal delete-swipe once the pointer moves.
    press: Option<PointerPress>,
    menu: Option<ContextMenu>,
    /// The reusable drag-to-reorder model (live gap + reflow + floating
    /// ghost). Armed once a press classifies as a vertical reorder; commits
    /// the `(from, to)` move on drop and restores the order on cancel.
    reorder: DragController<LocationId>,
    over_rect: Rect,
    over_list_top: f64,
    over_scroll: f64,
    over_max_scroll: f64,
    over_hotspots: Vec<Hotspot>,

    // --- overview: search (shared Autocomplete widget) ---
    /// The shared type-ahead field + suggestion overlay. Owns the query text,
    /// caret / selection / clipboard, and the arrow-key / Enter / Esc
    /// navigation of the geocoded matches. Weather feeds it results reactively
    /// (the geocode is a debounced network side effect), never a synchronous
    /// suggester closure.
    search: Autocomplete,
    /// The field's on-screen rect (set each draw) so a press can begin an edit.
    search_field_rect: Rect,
    /// The last query text folded into the geocode machinery, for change
    /// detection against the field's live text.
    search_query: String,
    search_results: Vec<SavedLocation>,
    search_seq: u64,
    pending_query_at: Option<Instant>,
    geocode_cache: HashMap<String, Vec<SavedLocation>>,
    geocode_inflight: bool,

    // --- detail: forecast ---
    detail_rect: Rect,
    detail_reserve_back: bool,
    refresh_btn: Rect,
    scroll: f64,
    max_scroll: f64,

    // --- detail: the horizontally-scrollable 24h strip ---
    hourly_scroll_x: f64,
    hourly_max_scroll: f64,
    /// The strip's visible band (absolute coords, updated each draw).
    hourly_viewport: Rect,
    /// The strip's scrollbar track (absolute coords, updated each draw).
    hourly_track: Rect,
    /// Full width of all 24 columns (for the scrollbar metrics).
    hourly_content_w: f64,
    /// Shared scrollbar interaction model for the strip's horizontal axis.
    hourly_bar: Scrollbar,

    // --- network coordination (drained by the app's net pump) ---
    outbox: Vec<Request>,
    inflight: usize,
    content_generation: u64,
    next_ticket: u64,
    dispatched: HashMap<u64, (u64, RequestKind)>,
    pub status: String,

    // --- overview: collapse-to-rail (wide layout only) ---
    /// Whether the app currently offers the collapse affordance (set each
    /// frame by the root widget; true only in the side-by-side layout).
    overview_collapsible: bool,
    /// Raised when the collapse / expand chip is clicked; the root widget
    /// consumes it and rebuilds the shell at the new overview width.
    collapse_toggle: bool,

    // --- shell intent (consumed by the root widget) ---
    nav_request: Option<NavIntent>,

    /// User-owned CRDT changes awaiting root-level `ActionHost` dispatch.
    /// Network data stays local to this process and never writes the document.
    durable_actions: Vec<WeatherAction>,
}

impl Core {
    pub fn new(model: WeatherModel, units: Units) -> Self {
        Self {
            model,
            units,
            last_mouse: Point::ZERO,
            text_scale: 1.0,
            spacing: Spacing::default(),
            metrics: Metrics::BASE,
            press: None,
            menu: None,
            reorder: DragController::vertical(),
            over_rect: Rect::ZERO,
            over_list_top: 0.0,
            over_scroll: 0.0,
            over_max_scroll: 0.0,
            over_hotspots: Vec::new(),
            search: Autocomplete::new()
                .with_id("weather.city_search")
                .with_placeholder("Search city…")
                .with_font_size(13.0),
            search_field_rect: Rect::ZERO,
            search_query: String::new(),
            search_results: Vec::new(),
            search_seq: 0,
            pending_query_at: None,
            geocode_cache: HashMap::new(),
            geocode_inflight: false,
            detail_rect: Rect::ZERO,
            detail_reserve_back: false,
            refresh_btn: Rect::ZERO,
            scroll: 0.0,
            max_scroll: 0.0,
            hourly_scroll_x: 0.0,
            hourly_max_scroll: 0.0,
            hourly_viewport: Rect::ZERO,
            hourly_track: Rect::ZERO,
            hourly_content_w: 0.0,
            hourly_bar: Scrollbar::horizontal(),
            outbox: Vec::new(),
            inflight: 0,
            content_generation: 0,
            next_ticket: 0,
            dispatched: HashMap::new(),
            status: String::new(),
            overview_collapsible: false,
            collapse_toggle: false,
            nav_request: None,
            durable_actions: Vec::new(),
        }
    }

    // ---- app-facing coordination ------------------------------------

    /// Take raw requests for offline consumers. Live dispatch uses the ticketed
    /// path below, preserving the identity assigned at enqueue time.
    pub fn take_outbox(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.outbox)
            .into_iter()
            .map(|request| match request {
                Request::Tracked { request, .. } => *request,
                request => request,
            })
            .collect()
    }

    fn enqueue_request(&mut self, request: Request) {
        self.next_ticket += 1;
        self.outbox.push(Request::Tracked {
            ticket: self.next_ticket,
            generation: self.content_generation,
            request: Box::new(request),
        });
    }

    pub(crate) fn request_marker(&self) -> u64 {
        self.next_ticket
    }

    /// Capture only the synchronous enqueue operation following this marker.
    /// Later dispatch batches cannot change an invocation's request ownership.
    pub(crate) fn queued_weather_tickets_after(
        &self,
        marker: u64,
    ) -> std::collections::HashSet<u64> {
        self.outbox
            .iter()
            .filter_map(|r| match r {
                Request::Tracked {
                    ticket, request, ..
                } if *ticket > marker && request.kind() != RequestKind::Geocode => Some(*ticket),
                _ => None,
            })
            .collect()
    }

    /// Dispatch retains each request's original ticket and content incarnation.
    pub(crate) fn take_tracked_outbox(&mut self) -> Vec<Request> {
        let requests = std::mem::take(&mut self.outbox);
        for request in &requests {
            if let Request::Tracked {
                ticket,
                generation,
                request,
            } = request
            {
                self.dispatched
                    .insert(*ticket, (*generation, request.kind()));
            }
        }
        requests
    }

    /// Fail only requests still awaiting a response after the worker channel
    /// closes. These flow through the normal ticket reducer, exactly once.
    pub(crate) fn disconnected_responses(&self) -> Vec<Response> {
        self.dispatched
            .iter()
            .map(|(&ticket, &(generation, kind))| Response::Tracked {
                ticket,
                generation,
                response: Box::new(Response::Error {
                    kind,
                    message: "Weather worker disconnected".into(),
                }),
            })
            .collect()
    }

    /// Adopt restored/remote authored places, invalidating stale gestures and
    /// unsent effects. Dispatched effects retain their real settlement count.
    pub(crate) fn adopt_saved_places(&mut self, persisted: &mut WeatherModel) {
        let changed = self.model.locations != persisted.locations;
        for l in &persisted.locations {
            if self.model.locations.iter().any(|old| old == l) {
                if let Some(s) = self.model.snapshots.get(&l.id) {
                    persisted.snapshots.insert(l.id, s.clone());
                }
                if let Some(a) = self.model.air_quality.get(&l.id) {
                    persisted.air_quality.insert(l.id, a.clone());
                }
            }
        }
        if changed {
            self.content_generation += 1;
            self.press = None;
            self.menu = None;
            self.reorder.cancel();
            self.over_hotspots.clear();
            self.durable_actions.clear();
            self.nav_request = None;
            let unsent = self
                .outbox
                .iter()
                .filter(|r| r.kind() != RequestKind::Geocode)
                .count();
            self.inflight = self.inflight.saturating_sub(unsent);
            self.outbox.clear();
            self.search_seq += 1;
            self.pending_query_at = None;
            self.geocode_inflight = false;
            self.search_results.clear();
            self.search.set_suggestions(Vec::new());
            self.search.set_status(None);
            self.search.close();
            self.status.clear();
        }
        if let Some(id) = self.model.selected_location().map(|location| location.id)
            && let Some(index) = persisted
                .locations
                .iter()
                .position(|location| location.id == id)
        {
            persisted.selected = index;
        }
        persisted.units = self.units;
        self.model = persisted.clone();
    }

    /// Take a pending shell navigation intent, if any.
    pub fn take_nav_request(&mut self) -> Option<NavIntent> {
        self.nav_request.take()
    }

    pub fn take_durable_actions(&mut self) -> Vec<WeatherAction> {
        std::mem::take(&mut self.durable_actions)
    }

    fn queue_durable_action(&mut self, action: WeatherAction) {
        self.durable_actions.push(action);
    }

    /// Set whether the collapse affordance is offered (side-by-side only).
    /// Called once per frame by the root widget, which alone knows the layout.
    pub fn set_overview_collapsible(&mut self, v: bool) {
        self.overview_collapsible = v;
    }

    /// Take a pending collapse/expand toggle raised by the chip. The root
    /// widget rebuilds the shell at the new overview width when this is set.
    pub fn take_collapse_toggle(&mut self) -> bool {
        std::mem::take(&mut self.collapse_toggle)
    }

    /// Whether a fetch is currently in flight (drives frame pumping).
    pub fn is_fetching(&self) -> bool {
        self.inflight > 0
    }

    /// Whether a geocode is armed or in flight. The app keeps pumping frames
    /// while this holds so the result is drained and shown promptly (rather
    /// than the loop parking on "Searching…" until the next input event).
    pub fn search_pending(&self) -> bool {
        self.geocode_inflight || self.pending_query_at.is_some()
    }

    pub fn search_deadline(&self) -> Option<Instant> {
        self.pending_query_at.map(|at| at + GEOCODE_DEBOUNCE)
    }

    /// Whether a geocode debounce is armed (so the app keeps pumping frames
    /// until it fires).
    pub fn debounce_pending(&self) -> bool {
        self.pending_query_at.is_some()
    }

    /// Queue a forecast + air-quality fetch for every saved location.
    pub fn refresh_all(&mut self) {
        let units = self.units;
        let mut reqs: Vec<Request> = Vec::new();
        for loc in &self.model.locations {
            reqs.push(Request::Forecast {
                location_id: loc.id,
                lat: loc.latitude,
                lon: loc.longitude,
                units,
            });
            reqs.push(Request::AirQuality {
                location_id: loc.id,
                lat: loc.latitude,
                lon: loc.longitude,
            });
        }
        self.inflight += reqs.len();
        for request in reqs {
            self.enqueue_request(request);
        }
        if !self.model.locations.is_empty() {
            self.status = "Updating…".to_string();
        }
    }

    /// Open a saved city by id: select it, reset the forecast scroll, refresh
    /// its forecast, and raise the show-detail push intent — the same path a
    /// tap on the city list takes (`OverAction::SelectLocation`). Returns
    /// `false` (touching nothing) for an id that is not a saved city. Used by
    /// the app's Ctrl+K search activation.
    pub fn open_location(&mut self, id: LocationId) -> bool {
        let Some(pos) = self.model.locations.iter().position(|l| l.id == id) else {
            return false;
        };
        self.model.select(pos);
        self.scroll = 0.0;
        self.refresh_selected();
        self.nav_request = Some(NavIntent::ShowDetail);
        true
    }

    /// Queue an update for exactly one durable saved location. This powers the
    /// per-location resource operation, so agent and keyboard-oriented flows
    /// can refresh the forecast in view without re-fetching every city.
    pub fn refresh_location_by_id(&mut self, id: LocationId) -> bool {
        let Some(index) = self
            .model
            .locations
            .iter()
            .position(|location| location.id == id)
        else {
            return false;
        };
        self.refresh_location(index);
        true
    }

    /// Refresh the city currently displayed in the detail pane.
    pub fn refresh_selected_forecast(&mut self) {
        self.refresh_selected();
    }

    fn refresh_selected(&mut self) {
        if let Some(loc) = self.model.selected_location() {
            let (id, lat, lon) = (loc.id, loc.latitude, loc.longitude);
            self.enqueue_request(Request::Forecast {
                location_id: id,
                lat,
                lon,
                units: self.units,
            });
            self.enqueue_request(Request::AirQuality {
                location_id: id,
                lat,
                lon,
            });
            self.inflight += 2;
            self.status = "Updating…".to_string();
        }
    }

    /// Move the overview selection by `delta` (arrow-key navigation of the
    /// city list), re-fetch the newly selected city's forecast and reset the
    /// forecast scroll. Returns true when the selection actually moved, so the
    /// caller can persist the new `selected` to the shared document.
    fn move_selection(&mut self, delta: i64) -> bool {
        if self.model.locations.is_empty() {
            return false;
        }
        let before = self.model.selected;
        self.model.select_relative(delta);
        let moved = self.model.selected != before;
        if moved {
            self.scroll = 0.0;
            self.refresh_selected();
        }
        moved
    }

    fn refresh_location(&mut self, idx: usize) {
        if let Some(loc) = self.model.locations.get(idx) {
            let (id, lat, lon) = (loc.id, loc.latitude, loc.longitude);
            self.enqueue_request(Request::Forecast {
                location_id: id,
                lat,
                lon,
                units: self.units,
            });
            self.enqueue_request(Request::AirQuality {
                location_id: id,
                lat,
                lon,
            });
            self.inflight += 2;
            self.status = "Updating…".to_string();
        }
    }

    /// Apply a completed network response to the model / search results.
    pub fn apply_response(&mut self, resp: Response) {
        match resp {
            Response::Tracked {
                ticket,
                generation,
                response,
            } => {
                if self.dispatched.remove(&ticket).is_none() {
                    return;
                }
                if generation != self.content_generation {
                    if response.is_weather() {
                        self.inflight = self.inflight.saturating_sub(1);
                    }
                    return;
                }
                self.apply_response(*response);
            }
            Response::Forecast(snap) => {
                self.inflight = self.inflight.saturating_sub(1);
                if !self
                    .model
                    .locations
                    .iter()
                    .any(|l| l.id == snap.location_id)
                {
                    return;
                }
                self.model.snapshots.insert(snap.location_id, snap);
                self.status = "Updated just now".to_string();
            }
            Response::AirQuality(aq) => {
                self.inflight = self.inflight.saturating_sub(1);
                if !self.model.locations.iter().any(|l| l.id == aq.location_id) {
                    return;
                }
                self.model.air_quality.insert(aq.location_id, aq);
            }
            Response::Geocode { seq, results } => {
                if seq == self.search_seq {
                    self.geocode_inflight = false;
                    let key = self.search_query.trim().to_lowercase();
                    if !key.is_empty() {
                        self.geocode_cache.insert(key, results.clone());
                    }
                    // The dropdown surfaces match/no-match state; the forecast
                    // status line is reserved for the forecast itself.
                    self.search_results = results;
                    self.sync_search_view();
                }
            }
            Response::Error { kind, .. } => match kind {
                RequestKind::Forecast | RequestKind::AirQuality => {
                    self.inflight = self.inflight.saturating_sub(1);
                    self.status =
                        "Couldn't update — check your connection and try again".to_string();
                }
                RequestKind::Geocode => {
                    self.geocode_inflight = false;
                    // A failed geocode must resolve the dropdown's spinner into
                    // an honest state rather than spin forever.
                    self.search_results.clear();
                    self.search.set_status(Some(
                        "Search unavailable — check your connection and try again".into(),
                    ));
                    self.search.open();
                }
            },
        }
    }

    /// Push the current search state into the shared [`Autocomplete`]: real
    /// suggestion rows when there are matches, else an honest loading or
    /// "no matches" status line — all inside the shared overlay.
    fn sync_search_view(&mut self) {
        if !self.search.is_editing() {
            return;
        }
        let q = self.search_query.trim().to_string();
        if q.is_empty() {
            self.search.set_suggestions(Vec::new());
            self.search.set_status(None);
            self.search.close();
            return;
        }
        if !self.search_results.is_empty() {
            let sugg: Vec<Suggestion> = self
                .search_results
                .iter()
                // A compact field can show either a long place name or a
                // right-aligned region, but not both reliably: a long name
                // would paint into the detail column. Keep the useful
                // disambiguator in one bounded label instead, so Paris,
                // France and Paris, Texas remain distinguishable at every
                // responsive width.
                .map(|r| Suggestion::labelled(result_value(r), result_label(r)))
                .collect();
            self.search.set_suggestions(sugg);
            self.search.set_status(None);
        } else if self.search_pending() {
            self.search.set_suggestions(Vec::new());
            self.search.set_status(Some("Searching…".to_string()));
        } else {
            self.search.set_suggestions(Vec::new());
            self.search
                .set_status(Some(format!("No matches for “{q}”")));
        }
        self.search.open();
    }

    // ---- geocoding (debounced + cached) -----------------------------

    /// React to the search field's text changing: cache lookup for instant
    /// matches, arm the debounce, and refresh the dropdown's view.
    fn on_query_changed(&mut self, text: &str) {
        self.search_query = text.to_string();
        let key = text.trim().to_lowercase();
        if key.is_empty() {
            self.search_results.clear();
            self.pending_query_at = None;
            self.sync_search_view();
            return;
        }
        // Show cached matches instantly; otherwise the dropdown shows its
        // loading state until the debounced geocode lands.
        if let Some(hit) = self.geocode_cache.get(&key) {
            self.search_results = hit.clone();
        } else {
            self.search_results.clear();
        }
        self.pending_query_at = Some(ui::time::now());
        self.sync_search_view();
    }

    /// If the debounce window elapsed, queue the geocode request now. Called
    /// once per frame by the app.
    pub fn maybe_fire_geocode(&mut self) {
        let Some(at) = self.pending_query_at else {
            return;
        };
        if at.elapsed() < GEOCODE_DEBOUNCE {
            return;
        }
        self.pending_query_at = None;
        let q = self.search_query.trim().to_string();
        if q.is_empty() {
            return;
        }
        let key = q.to_lowercase();
        if self.geocode_cache.contains_key(&key) {
            return; // already shown from cache
        }
        self.search_seq += 1;
        self.geocode_inflight = true;
        self.enqueue_request(Request::Geocode {
            seq: self.search_seq,
            query: q,
        });
        // The spinner in the dropdown reflects the now-in-flight geocode.
        self.sync_search_view();
    }

    // ---- search lifecycle (begin / commit / end) --------------------

    /// Focus the shared search field and open an empty edit. Called from a
    /// press on the field or the `/` shortcut.
    fn begin_search(&mut self) {
        self.search.set_text("");
        self.search.begin_edit();
        self.search_query.clear();
        self.search_results.clear();
        self.pending_query_at = None;
        self.geocode_inflight = false;
    }

    /// Blur the search field and clear its transient state.
    fn end_search(&mut self) {
        self.search.close();
        self.search.set_focus(false);
        self.search.set_text("");
        self.search_query.clear();
        self.search_results.clear();
        self.pending_query_at = None;
        self.geocode_inflight = false;
    }

    /// Resolve an accepted autocomplete value back to a geocoded place and add
    /// it. The encoded "Name, Region, Country" value identifies the exact match
    /// (so arrowing to the second "Paris" adds the right one); a bare typed
    /// query (Enter with no highlight) falls back to the top match. Returns
    /// true when a place was actually added.
    fn commit_search_pick(&mut self, value: &str) -> bool {
        let picked = self
            .search_results
            .iter()
            .find(|r| result_value(r) == value)
            .cloned()
            .or_else(|| {
                (value.trim() == self.search_query.trim())
                    .then(|| self.search_results.first().cloned())
                    .flatten()
            });
        if let Some(loc) = picked {
            self.model.add_location(loc);
            // Persist the retained identity/rank after coordinate deduplication,
            // and the selection that add_location made in the live model.
            let Some(location) = self.model.selected_location().cloned() else {
                return false;
            };
            let id = location.id.to_string();
            self.queue_durable_action(WeatherAction::AddLocation { location });
            self.queue_durable_action(WeatherAction::SelectLocation { id });
            self.scroll = 0.0;
            self.refresh_selected();
            self.nav_request = Some(NavIntent::ShowDetail);
            return true;
        }
        // Enter while still loading: arm an immediate geocode so the field
        // resolves to real matches rather than doing nothing. If the clock's
        // too young for the debounce to backdate (never in practice outside
        // a fresh boot), just start the debounce fresh from now instead of
        // panicking.
        if !self.search_query.trim().is_empty() {
            self.pending_query_at = Some(
                ui::time::now()
                    .checked_sub(GEOCODE_DEBOUNCE)
                    .unwrap_or_else(ui::time::now),
            );
        }
        false
    }

    // ---- per-entry actions (swipe ⇔ menu parity) --------------------

    fn apply_entry_action(&mut self, index: usize, action: EntryAction) -> bool {
        match action {
            EntryAction::Delete => {
                if let Some(id) = self.model.locations.get(index).map(|location| location.id) {
                    self.model.remove_location(index);
                    self.queue_durable_action(WeatherAction::RemoveLocation { id: id.to_string() });
                    true
                } else {
                    false
                }
            }
            EntryAction::MoveToTop => {
                let changed = self.model.move_location(index, 0);
                if changed && let Some(location) = self.model.locations.first() {
                    self.queue_durable_action(WeatherAction::SetLocationRank {
                        id: location.id.to_string(),
                        rank: location.order_rank,
                    });
                }
                changed
            }
            EntryAction::Refresh => {
                self.refresh_location(index);
                false
            }
        }
    }

    // ---- shared visual helpers --------------------------------------

    fn accent(&self, theme: &Theme) -> Color {
        let day = self.is_day();
        if day {
            theme.tokens.colors.accent[5]
        } else {
            theme.tokens.colors.accent[6]
        }
    }

    fn is_day(&self) -> bool {
        self.model
            .selected_snapshot()
            .is_none_or(|s| s.current.is_day)
    }

    // =================================================================
    // OVERVIEW pane — the contentful city list (master)
    // =================================================================

    /// Set the detail pane's reserved top space for the shell's Back chip.
    /// Called by the app before drawing (only the shell knows the affordance).
    pub fn set_detail_reserve_back(&mut self, reserve: bool) {
        self.detail_reserve_back = reserve;
    }

    /// Semantic projection of the painted saved-place list. The visual rows
    /// are custom Vello drawing, so expose their stable model identity and
    /// current readings explicitly instead of leaving screen-reader users with
    /// an unnamed canvas.
    pub fn overview_accessibility(&self, rect: Rect) -> AccessNode {
        let total = self.model.locations.len();
        let items = self
            .model
            .locations
            .iter()
            .enumerate()
            .map(|(index, location)| {
                let reading = self.model.snapshot_for(location.id).map_or_else(
                    || "Current conditions are loading".to_string(),
                    |snapshot| {
                        let temp = snapshot
                            .temp_in(snapshot.current.temperature, self.units)
                            .round();
                        format!("{temp:.0}°, {}", Sky::label(snapshot.current.weather_code))
                    },
                );
                let bounds = self
                    .over_hotspots
                    .iter()
                    .find_map(|hotspot| match hotspot.action {
                        OverAction::PressChip(i) | OverAction::SelectLocation(i) if i == index => {
                            Some(hotspot.rect)
                        }
                        _ => None,
                    })
                    .unwrap_or(rect);
                AccessNode::new(Role::ListItem)
                    .with_name(location.name.clone())
                    .with_description(format!("{}; {reading}", location.subtitle()))
                    .with_position(index + 1, total)
                    .with_hashed_key(location.id)
                    .with_bounds(bounds)
                    .with_state(
                        AccessState::new()
                            .selected(index == self.model.selected)
                            .focusable(true),
                    )
            })
            .collect();
        let places = AccessNode::new(Role::List)
            .with_name("Saved places")
            .with_description("Use Up and Down Arrow to change city, Enter to open its forecast, or slash to search for another city.")
            .with_bounds(rect)
            .with_children(items);
        let mut children = Vec::new();
        if let Some(search) = self.search.accessibility() {
            children.push(search);
        }
        children.push(places);
        AccessNode::new(Role::Group)
            .with_name("Weather")
            .with_bounds(rect)
            .with_children(children)
            .with_keyed_action_routes("weather-overview")
            .with_keyed_action_routes("weather-overview")
    }

    /// Semantic summary of the currently painted forecast. This mirrors the
    /// detail card's key reading while leaving the long forecast visually
    /// scannable and avoids forcing assistive technology through decorative
    /// icons and chart marks.
    pub fn detail_accessibility(&self, rect: Rect) -> AccessNode {
        let mut node = AccessNode::new(Role::Group)
            .with_name("Forecast")
            .with_bounds(rect);
        let Some(location) = self.model.selected_location() else {
            return node.with_description(if self.model.locations.is_empty() {
                "No cities yet. Search for a city to see its forecast."
            } else {
                "Choose a saved city to see its forecast."
            });
        };
        let detail = self.model.snapshot_for(location.id).map_or_else(
            || "Current conditions are loading.".to_string(),
            |snapshot| {
                let temp = snapshot.temp_in(snapshot.current.temperature, self.units).round();
                let feels = snapshot.temp_in(snapshot.current.apparent, self.units).round();
                format!(
                    "{temp:.0}°, {}, feels like {feels:.0}°. {} hourly readings and {} daily forecasts are available.",
                    Sky::label(snapshot.current.weather_code),
                    snapshot.hourly.len(),
                    snapshot.daily.len()
                )
            },
        );
        node = node
            .with_name(format!("{} forecast", location.name))
            .with_description(detail);
        if self.refresh_btn != Rect::ZERO {
            node.children.push(
                AccessNode::new(Role::Button)
                    .with_name("Refresh forecast")
                    .with_description("Get the latest weather and air-quality readings.")
                    .with_hashed_key("weather.refresh-forecast")
                    .with_bounds(self.refresh_btn)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .disabled(self.is_fetching()),
                    ),
            );
        }
        node.with_keyed_action_routes("weather-detail")
    }

    pub fn draw_overview(
        &mut self,
        rect: Rect,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.over_rect = rect;
        self.text_scale = tr.text_scale();
        // Below the rail threshold the overview collapses to a glanceable
        // icon column. The mode is re-derived from the allocated width every
        // frame, so a resize between side-by-side and rail reflows on its own.
        if rect.width() < RAIL_MAX_WIDTH {
            self.draw_overview_rail(rect, scene, t, tr, theme);
            return;
        }

        let c = &theme.tokens.colors;
        let mut hot: Vec<Hotspot> = Vec::new();

        // Solid surface — this is a content panel, not a floating drawer.
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &rect);

        let (left, width) = overview_column(rect);
        let list_top = self.draw_overview_search(scene, t, tr, theme, left, width, &mut hot);
        self.draw_overview_list(
            &mut PaintCtx::new(scene, t, tr, theme),
            rect,
            list_top,
            &mut hot,
        );
        self.over_hotspots = hot;
    }

    /// The collapse chip (side-by-side only) + the shared search field. No
    /// self-drawn app-name header — the compositor owns window chrome, so
    /// this panel opens straight into its content. Returns the y the saved-
    /// place list starts at.
    fn draw_overview_search(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        left: f64,
        width: f64,
        hot: &mut Vec<Hotspot>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let sp = &theme.tokens.spacing;
        let sy = self.over_rect.y0 + f64::from(sp.s4);
        let sh = 38.0;
        let mut field_w = width;

        // Collapse chip (side-by-side only) — tucks the list into a rail so
        // the forecast can use the full width. It sits at the search field's
        // right edge, an `s3` from it, which the field leaves room for. The
        // shared button face, so its rest edge and hover match every other
        // control.
        if self.overview_collapsible {
            let cb = 30.0;
            let cy = sy + (sh - cb) / 2.0;
            let chip = Rect::new(left + width - cb, cy, left + width, cy + cb);
            let hovered = chip.contains(self.last_mouse);
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::new(chip, "")
                    .icon(Icon::ChevronLeft)
                    .hovered(hovered)
                    .accessible_name("Collapse city list"),
            );
            hot.push(Hotspot {
                rect: chip,
                action: OverAction::ToggleCollapse,
            });
            field_w = (width - cb - f64::from(sp.s3)).max(80.0);
        }

        // Search field — the shared Autocomplete (a real text field, its
        // suggestion overlay floats last). No hand-rolled buffer / caret / hit
        // rect here; the widget owns selection, clipboard, IME and theming.
        let field_rect = Rect::new(left, sy, left + field_w, sy + sh);
        self.search_field_rect = field_rect;
        self.search.apply_layout_external(field_rect);
        let presence = PresenceCtx::empty();
        self.search.draw(scene, t, tr, theme, &presence);

        // The saved-place list keeps a fixed position — the live search
        // matches render as a floating dropdown over it (drawn last), so the
        // list never jumps around while typing.
        let mut y = sy + sh + f64::from(sp.s4);
        text_left(
            scene,
            tr,
            "SAVED PLACES",
            left,
            y,
            10.0,
            TextWeight::Semibold,
            c.text_dim,
        );
        y += (line_h(10.0, self.text_scale) + f64::from(sp.s2)).ceil();
        y
    }

    /// The scrolling chip list below the search field, its empty state, the
    /// delete drop zone while reordering, the scroll indicator, and finally
    /// the context menu + suggestion overlay (which must paint last so they
    /// sit above the chips).
    fn draw_overview_list(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        list_top: f64,
        hot: &mut Vec<Hotspot>,
    ) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let (left, width) = overview_column(rect);
        let c = &theme.tokens.colors;

        // The chip list scrolls within the region below the header. While a
        // reorder is in flight the foot shows the Delete drop zone.
        let reorder = self.is_reorder_active();
        let list_bottom = if reorder {
            rect.y1 - 96.0 - 2.0 * RP
        } else {
            rect.y1 - RP
        };
        let view_h = (list_bottom - list_top).max(0.0);
        let count = self.model.locations.len();
        let content_h = count as f64 * self.chip_metrics().pitch();
        self.over_max_scroll = (content_h - view_h).max(0.0);
        self.over_scroll = self.over_scroll.clamp(0.0, self.over_max_scroll);
        self.over_list_top = list_top - self.over_scroll;

        // Re-pin the live insertion gap to the cursor against this frame's
        // geometry so a held drag reflows correctly even as the list scrolls.
        let metrics = self.list_metrics();
        self.reorder.sync_insertion(&metrics);

        // Clip the scrolling list to its region.
        let clip = Rect::new(rect.x0, list_top - 2.0, rect.x1, list_bottom + 2.0);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &clip);
        self.draw_chips(scene, t, tr, theme, left, width, hot);
        scene.pop_layer();

        if self.model.locations.is_empty() {
            text_left(
                scene,
                tr,
                "No places yet — search above",
                left,
                list_top + 8.0,
                13.0,
                TextWeight::Regular,
                c.text_dim,
            );
        }

        // Foot: Delete drop zone while reordering.
        if reorder {
            self.draw_delete_dropzone(scene, t, tr, theme, rect);
        }

        // Scroll indicator.
        if self.over_max_scroll > 1.0 {
            let track_x = rect.x1 - 6.0;
            let track_h = list_bottom - list_top;
            let thumb_h = (track_h * (view_h / content_h)).clamp(24.0, track_h);
            let thumb_y =
                list_top + (self.over_scroll / self.over_max_scroll) * (track_h - thumb_h);
            scene.fill(
                Fill::NonZero,
                t,
                c.lift(1.0),
                None,
                &RoundedRect::new(track_x, list_top, track_x + 3.0, list_bottom, 1.5),
            );
            scene.fill(
                Fill::NonZero,
                t,
                c.text_dim.with_alpha_factor(0.7),
                None,
                &RoundedRect::new(track_x, thumb_y, track_x + 3.0, thumb_y + thumb_h, 1.5),
            );
        }

        // Context menu paints last so it sits above the chips.
        if self.menu.is_some() {
            self.draw_context_menu(scene, t, tr, theme, hot);
        }

        // The shared suggestion overlay floats over the list, topmost.
        if self.search.is_editing() {
            let presence = PresenceCtx::empty();
            self.search.draw_overlay(scene, t, tr, theme, &presence);
        }
    }

    /// The collapsed **rail**: a slim, glanceable column of the saved cities
    /// (condition icon + temperature) plus an expand chip at the top. Clicking
    /// a city selects it; clicking the chip restores the full list. Rendered
    /// whenever the overview is allocated less than [`RAIL_MAX_WIDTH`].
    fn draw_overview_rail(
        &mut self,
        rect: Rect,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let mut hot: Vec<Hotspot> = Vec::new();
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &rect);
        // The rail has no search field; drop any stale rect so a press can't
        // land on a phantom field from the previous (wide) layout.
        self.search_field_rect = Rect::ZERO;

        let list_top = self.draw_rail_header(scene, t, tr, theme, rect, &mut hot);
        self.draw_rail_list(scene, t, tr, theme, rect, list_top, &mut hot);
        self.over_hotspots = hot;
    }

    /// The rail's expand chip + the separator beneath it. Returns the y the
    /// scrolling city column starts at.
    fn draw_rail_header(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        hot: &mut Vec<Hotspot>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let s3 = f64::from(theme.tokens.spacing.s3);
        let cx = f64::midpoint(rect.x0, rect.x1);

        // Expand chip: the shared button face with the chevron icon.
        let cs = 36.0;
        let chip = Rect::new(
            cx - cs / 2.0,
            rect.y0 + s3,
            cx + cs / 2.0,
            rect.y0 + s3 + cs,
        );
        let hovered = chip.contains(self.last_mouse);
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(chip, "")
                .icon(Icon::ChevronRight)
                .hovered(hovered)
                .accessible_name("Expand city list"),
        );
        hot.push(Hotspot {
            rect: chip,
            action: OverAction::ToggleCollapse,
        });

        // Separator under the chip: derived against the rail, inset `s3`,
        // an `s3` below the chip and above the first city.
        let sep_y = chip.y1 + s3;
        ui::surface::separator(scene, t, rect.x0, rect.x1, sep_y + 0.5, s3, c.surfaces.base);
        sep_y + 1.0 + s3
    }

    /// The scrolling city column: one slot per saved place (icon + temp),
    /// the selection/hover highlight, and the scroll indicator.
    fn draw_rail_list(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        list_top: f64,
        hot: &mut Vec<Hotspot>,
    ) {
        let c = &theme.tokens.colors;
        let list_bottom = rect.y1 - 10.0;
        let view_h = (list_bottom - list_top).max(0.0);
        let count = self.model.locations.len();
        let content_h = count as f64 * RAIL_SLOT_H;
        self.over_max_scroll = (content_h - view_h).max(0.0);
        self.over_scroll = self.over_scroll.clamp(0.0, self.over_max_scroll);
        let scrolled_top = list_top - self.over_scroll;
        self.over_list_top = scrolled_top;

        let clip = Rect::new(rect.x0, list_top - 2.0, rect.x1, list_bottom + 2.0);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &clip);
        for (i, loc) in self.model.locations.iter().enumerate() {
            let sy = scrolled_top + i as f64 * RAIL_SLOT_H;
            let band = Rect::new(rect.x0, sy, rect.x1, sy + RAIL_SLOT_H);
            self.draw_rail_slot(&mut PaintCtx::new(scene, t, tr, theme), band, i, loc, hot);
        }
        scene.pop_layer();

        if self.over_max_scroll > 1.0 && view_h >= 20.0 {
            let track_x = rect.x1 - 4.0;
            let track_h = view_h;
            let thumb_h = (track_h * (view_h / content_h)).clamp(20.0, track_h);
            let thumb_y =
                list_top + (self.over_scroll / self.over_max_scroll) * (track_h - thumb_h);
            scene.fill(
                Fill::NonZero,
                t,
                c.text_dim.with_alpha_factor(0.6),
                None,
                &RoundedRect::new(track_x, thumb_y, track_x + 2.5, thumb_y + thumb_h, 1.25),
            );
        }
    }

    /// One rail slot in its `band` of the rail: the selection/hover
    /// highlight, the condition icon (or a dash before the first fetch), the
    /// rounded temperature, and its hit region.
    fn draw_rail_slot(
        &self,
        ctx: &mut PaintCtx<'_>,
        band: Rect,
        i: usize,
        loc: &SavedLocation,
        hot: &mut Vec<Hotspot>,
    ) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let c = &theme.tokens.colors;
        let cx = f64::midpoint(band.x0, band.x1);
        let slot = Rect::new(band.x0 + 6.0, band.y0 + 4.0, band.x1 - 6.0, band.y1 - 6.0);
        let selected = i == self.model.selected;
        let hov = slot.contains(self.last_mouse);
        if selected {
            let bg = lerp_color(c.surfaces.base, self.accent(theme), 0.18);
            scene.fill(
                Fill::NonZero,
                t,
                bg,
                None,
                &RoundedRect::from_rect(slot, 8.0),
            );
            scene.fill(
                Fill::NonZero,
                t,
                self.accent(theme),
                None,
                &RoundedRect::new(slot.x0, slot.y0 + 6.0, slot.x0 + 3.0, slot.y1 - 6.0, 1.5),
            );
        } else if hov {
            scene.fill(
                Fill::NonZero,
                t,
                hover_wash(c.surfaces.base),
                None,
                &RoundedRect::from_rect(slot, 8.0),
            );
        }
        if let Some(snap) = self.model.snapshot_for(loc.id) {
            let sky = Sky::from_code(snap.current.weather_code);
            icon::draw_icon(
                scene,
                t,
                sky,
                snap.current.is_day,
                Rect::new(cx - 15.0, slot.y0 + 4.0, cx + 15.0, slot.y0 + 34.0),
                theme,
            );
            let temp = snap.temp_in(snap.current.temperature, self.units).round() as i64;
            text_center(
                scene,
                tr,
                &format!("{temp}°"),
                cx,
                slot.y1 - 18.0,
                14.0,
                TextWeight::Bold,
                c.text_primary,
            );
        } else {
            text_center(
                scene,
                tr,
                "—",
                cx,
                slot.y0 + 14.0,
                15.0,
                TextWeight::Regular,
                c.text_dim,
            );
        }
        hot.push(Hotspot {
            rect: slot,
            action: OverAction::SelectLocation(i),
        });
    }

    fn draw_chips(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        w: f64,
        hot: &mut Vec<Hotspot>,
    ) {
        let metrics = self.list_metrics();
        let chip_m = self.chip_metrics();
        let dragging = self.reorder.from_index();
        let slot_y = |i: usize| self.over_list_top + i as f64 * chip_m.pitch();

        // While a reorder is live the toolkit's `DragController` reflows the
        // list: every non-dragged chip slides by `item_shift` to close the
        // vacated hole and open a one-slot gap exactly where the dragged chip
        // will land — so the list visibly rearranges under the cursor rather
        // than only on release. A faint accent caret sits at the top of the
        // reflowed gap (the live target slot) to mark where the chip will drop.
        if let Some(gap) = self.reorder.insertion() {
            let gy = slot_y(gap) - chip_m.gap / 2.0;
            scene.fill(
                Fill::NonZero,
                t,
                self.accent(theme),
                None,
                &RoundedRect::new(x, gy - 1.5, x + w, gy + 1.5, 1.5),
            );
        }

        for (i, loc) in self.model.locations.iter().enumerate() {
            if Some(i) == dragging {
                continue;
            }
            let y = slot_y(i) + self.reorder.item_shift(i, &metrics);
            let chip = Rect::new(x, y, x + w, y + chip_m.chip_h);
            let swipe_dx = self.swipe_dx_for(i);
            if swipe_dx > 0.0 {
                Self::draw_swipe_backing(scene, t, theme, chip);
            }
            let chip_t = t * Affine::translate((-swipe_dx, 0.0));
            self.draw_chip(&mut PaintCtx::new(scene, chip_t, tr, theme), chip, loc, i);
            hot.push(Hotspot {
                rect: chip,
                action: OverAction::PressChip(i),
            });
        }

        // The dragged chip floats free as a ghost at its grab offset under the
        // cursor, drawn last so it rides above the reflowing list.
        if let Some(i) = dragging
            && let (Some(loc), Some(chip)) = (
                self.model.locations.get(i),
                self.reorder.ghost_rect(x, x + w, &metrics),
            )
        {
            let sh =
                RoundedRect::from_rect(chip.inset(-2.0), f64::from(theme.tokens.radii.md) + 2.0);
            // The dragged chip's seat is an elevation shadow, so a light
            // theme softens it and high contrast keeps the ladder's own hue.
            scene.fill(Fill::NonZero, t, theme.tokens.elevation.e3.color, None, &sh);
            self.draw_chip(&mut PaintCtx::new(scene, t, tr, theme), chip, loc, i);
        }
    }

    /// The reorderable saved-place list's geometry this frame, for the
    /// toolkit's [`DragController`]. `over_list_top` is the scroll-adjusted
    /// leading edge of the first chip, set during `draw_overview`.
    fn list_metrics(&self) -> ListMetrics {
        let m = self.chip_metrics();
        ListMetrics::uniform(
            self.over_list_top,
            m.chip_h,
            m.gap,
            self.model.locations.len(),
        )
    }

    /// Cache the theme's spacing and density metrics from the app's layout
    /// pass, so the row grid solved without a renderer reads the same tokens
    /// the painters do.
    pub fn set_layout_tokens(&mut self, theme: &Theme) {
        self.spacing = theme.tokens.spacing;
        self.metrics = theme.tokens.metrics;
    }

    /// Saved-place row geometry at the captured text scale — one description
    /// for the row grid, the hit rects, the reorder model and the painter.
    fn chip_metrics(&self) -> ChipMetrics {
        ChipMetrics::new(&self.spacing, self.text_scale)
    }

    /// The rounded current temperature a saved-place chip shows, or `None`
    /// before the city's first snapshot has arrived.
    fn draw_chip(&self, ctx: &mut PaintCtx<'_>, chip: Rect, loc: &SavedLocation, index: usize) {
        let accent = self.accent(ctx.theme);
        crate::presentation::draw_saved_place(
            ctx,
            &SavedPlaceFace {
                chip,
                loc,
                snapshot: self.model.snapshot_for(loc.id),
                units: self.units,
                selected: index == self.model.selected,
                hovered: chip.contains(self.last_mouse),
                accent,
            },
        );
    }

    fn draw_swipe_backing(scene: &mut Scene, t: Affine, theme: &Theme, chip: Rect) {
        let c = &theme.tokens.colors;
        let shape = RoundedRect::from_rect(chip, f64::from(theme.tokens.radii.md));
        scene.fill(Fill::NonZero, t, c.semantic.danger, None, &shape);
        let icon_r = Rect::new(
            chip.x1 - 42.0,
            chip.y0 + 17.0,
            chip.x1 - 14.0,
            chip.y0 + 45.0,
        );
        Icon::Trash.draw(scene, t, icon_r, c.on_surface(c.semantic.danger).ink);
    }

    fn draw_delete_dropzone(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        let c = &theme.tokens.colors;
        let zone = self.delete_zone_rect();
        let w = (rect.width() - 2.0 * RP).max(80.0);
        let armed = self.is_reorder_active() && zone.contains(self.last_mouse);
        let shape = RoundedRect::from_rect(zone, f64::from(theme.tokens.radii.lg));
        let fill = if armed {
            c.semantic.danger
        } else {
            c.semantic.danger.with_alpha_factor(0.16)
        };
        scene.fill(Fill::NonZero, t, fill, None, &shape);
        let dash = Stroke::new(2.0).with_dashes(0.0, [6.0, 5.0]);
        scene.stroke(&dash, t, c.semantic.danger, None, &shape);
        let icon_color = if armed {
            c.on_surface(c.semantic.danger).ink
        } else {
            c.semantic.danger
        };
        let icon_r = Rect::new(
            zone.x0 + w / 2.0 - 16.0,
            zone.y0 + 14.0,
            zone.x0 + w / 2.0 + 16.0,
            zone.y0 + 46.0,
        );
        Icon::Trash.draw_with_stroke(scene, t, icon_r, icon_color, 2.0);
        let label_color = if armed {
            c.on_surface(c.semantic.danger).ink
        } else {
            c.semantic.danger
        };
        text_center(
            scene,
            tr,
            "Delete",
            zone.x0 + w / 2.0,
            zone.y1 - 26.0,
            14.0,
            TextWeight::Bold,
            label_color,
        );
    }

    fn draw_context_menu(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hot: &mut Vec<Hotspot>,
    ) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        let c = &theme.tokens.colors;
        let mw = 184.0;
        let row_h = 34.0;
        let pad = 6.0;
        let mh = pad * 2.0 + menu.items.len() as f64 * row_h;
        let area = self.over_rect;
        let mx = menu.anchor.x.min(area.x1 - mw - 4.0).max(area.x0 + 4.0);
        let my = menu.anchor.y.min(area.y1 - mh - 4.0).max(area.y0 + 4.0);
        let panel = Rect::new(mx, my, mx + mw, my + mh);
        let shape = RoundedRect::from_rect(panel, f64::from(theme.tokens.radii.md));
        scene.fill(
            Fill::NonZero,
            t,
            theme.tokens.elevation.e4.color,
            None,
            &RoundedRect::from_rect(panel.inset(-3.0), f64::from(theme.tokens.radii.md) + 3.0),
        );
        // A floating menu is a Panel: the chrome's panel ground and its
        // derived panel border (HIG §9.5), so the edge registers on both
        // themes.
        let ground = ui::chrome::panel_ground(theme);
        scene.fill(Fill::NonZero, t, ground, None, &shape);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::chrome::panel_border(theme),
            None,
            &shape,
        );
        for (i, action) in menu.items.iter().enumerate() {
            let ry = my + pad + i as f64 * row_h;
            let row = Rect::new(mx + pad, ry, mx + mw - pad, ry + row_h);
            if row.contains(self.last_mouse) {
                let hl = if action.is_destructive() {
                    c.semantic.danger.with_alpha_factor(0.22)
                } else {
                    hover_wash(ground)
                };
                scene.fill(
                    Fill::NonZero,
                    t,
                    hl,
                    None,
                    &RoundedRect::from_rect(row, 6.0),
                );
            }
            let text_color = if action.is_destructive() {
                c.semantic.danger
            } else {
                c.text_primary
            };
            let icon_r = Rect::new(row.x0 + 6.0, row.y0 + 8.0, row.x0 + 24.0, row.y0 + 26.0);
            match action {
                EntryAction::Delete => Icon::Trash.draw(scene, t, icon_r, text_color),
                EntryAction::MoveToTop => Icon::ChevronUp.draw(scene, t, icon_r, text_color),
                EntryAction::Refresh => Icon::Refresh.draw(scene, t, icon_r, text_color),
            }
            text_left(
                scene,
                tr,
                action.label(),
                row.x0 + 32.0,
                row.y0 + row_h / 2.0 - 8.0,
                13.0,
                TextWeight::Medium,
                text_color,
            );
            hot.push(Hotspot {
                rect: row,
                action: OverAction::MenuItem(menu.index, *action),
            });
        }
    }

    // ---- overview drag / swipe helpers ------------------------------

    fn is_reorder_active(&self) -> bool {
        self.reorder.is_active()
    }

    fn swipe_dx_for(&self, i: usize) -> f64 {
        match self.press {
            Some(p) if p.is_swipe() && p.index == i => p.swipe_dx(),
            _ => 0.0,
        }
    }

    fn delete_zone_rect(&self) -> Rect {
        let rect = self.over_rect;
        let x = rect.x0 + RP;
        let w = (rect.width() - 2.0 * RP).max(80.0);
        let h = 96.0;
        Rect::new(x, rect.y1 - h - RP, x + w, rect.y1 - RP)
    }

    fn chip_at(&self, p: Point) -> Option<usize> {
        let rect = self.over_rect;
        let x = rect.x0 + RP;
        let w = (rect.width() - 2.0 * RP).max(80.0);
        let metrics = self.chip_metrics();
        for i in 0..self.model.locations.len() {
            let y = self.over_list_top + i as f64 * metrics.pitch();
            if Rect::new(x, y, x + w, y + metrics.chip_h).contains(p) {
                return Some(i);
            }
        }
        None
    }

    fn apply_over_action(&mut self, action: OverAction) -> bool {
        match action {
            OverAction::SelectLocation(i) => {
                self.model.select(i);
                self.scroll = 0.0;
                self.refresh_selected();
                self.nav_request = Some(NavIntent::ShowDetail);
                true
            }
            OverAction::MenuItem(index, action) => {
                self.menu = None;
                self.apply_entry_action(index, action)
            }
            OverAction::ToggleCollapse => {
                // Raise the intent; the root widget rebuilds the shell at the
                // new overview width. Not a document mutation.
                self.collapse_toggle = true;
                false
            }
            OverAction::PressChip(_) => false,
        }
    }

    /// Handle an event routed to the overview pane. Returns true if consumed.
    pub(crate) fn handle_overview_event(&mut self, mut ctx: OverviewEventCtx<'_>) -> bool {
        // Keep the shared field placed so its overlay hit-tests correctly.
        self.search.apply_layout_external(self.search_field_rect);

        if let Some(consumed) = self.route_overview_search_field(&mut ctx) {
            return consumed;
        }

        let (handled, _changed) = self.dispatch_overview_event(ctx.event, ctx.scale_factor);
        handled
    }

    /// The search field's first refusal: a press over the field begins an
    /// edit, and while editing the shared Autocomplete owns typing and
    /// arrow/Enter/Esc navigation of the geocoded matches. Returns
    /// `Some(consumed)` when the field fully handled the event (the caller
    /// should return immediately), or `None` to fall through to the list's
    /// own gesture handling.
    fn route_overview_search_field(&mut self, ctx: &mut OverviewEventCtx<'_>) -> Option<bool> {
        use winit::event::{ElementState, MouseButton};

        // A press over the field while not editing begins an edit.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = ctx.event
            && !self.search.is_editing()
            && self.search_field_rect.contains(self.last_mouse)
        {
            self.begin_search();
            return Some(true);
        }

        if !self.search.is_editing() {
            return None;
        }

        let before = self.search.text().to_string();
        let mut ac_handled = false;
        self.search.handle_event(
            ctx.event,
            &mut ui::widget::EventCtx {
                scale_factor: ctx.scale_factor,
                modifiers: &ctx.modifiers,
                state: ctx.state,
                path: ctx.path,
                handled: &mut ac_handled,
                focus_target: ctx.focus_target,
                node_id: ctx.node_id,
                taffy: ctx.taffy,
            },
        );
        self.finish_search_edit(&before, ac_handled)
    }

    pub fn perform_search_action(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        if !self.search.is_editing() {
            self.begin_search();
        }
        let before = self.search.text().to_string();
        let handled = self.search.perform_action_at(action, key_path, state, path);
        self.finish_search_edit(&before, handled).unwrap_or(false)
    }

    fn finish_search_edit(&mut self, before: &str, ac_handled: bool) -> Option<bool> {
        match self.search.take_outcome() {
            AutocompleteOutcome::Accepted(value) => {
                if self.commit_search_pick(&value) {
                    self.end_search();
                } else {
                    // Nothing to pick yet (Enter while loading): keep the
                    // edit open and show the honest loading state.
                    self.search.open();
                    self.sync_search_view();
                }
                return Some(true);
            }
            AutocompleteOutcome::Cancelled => {
                // A click-outside cancel is not consumed — fall through so
                // the same press can still engage a chip beneath the field.
                self.end_search();
            }
            AutocompleteOutcome::None => {}
        }
        let after = self.search.text().to_string();
        let text_changed = after != before;
        if text_changed {
            self.on_query_changed(&after);
        }
        // Consume anything the field acted on — a handled key/click or a
        // text edit — so a keystroke never leaks past the focused field.
        if ac_handled || text_changed {
            Some(true)
        } else {
            None
        }
    }

    /// The list's own gesture handling once the search field has declined
    /// the event. Returns `(handled, changed)`.
    fn dispatch_overview_event(&mut self, event: &WindowEvent, scale_factor: f64) -> (bool, bool) {
        use winit::event::{ElementState, KeyEvent, MouseButton};

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_overview_cursor_moved(*position, scale_factor);
                (false, false)
            }
            WindowEvent::MouseWheel { delta, .. } => {
                (self.handle_overview_wheel(*delta, scale_factor), false)
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => (self.handle_overview_right_click(), false),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.handle_overview_left_press(),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.handle_overview_left_release(),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => self.handle_overview_key(logical_key),
            _ => (false, false),
        }
    }

    fn handle_overview_cursor_moved(
        &mut self,
        position: winit::dpi::PhysicalPosition<f64>,
        scale_factor: f64,
    ) {
        self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
        if let Some(press) = self.press.as_mut() {
            press.update(self.last_mouse);
        }
        // Drive the reusable reorder model only while the gesture is a
        // vertical reorder. A horizontal delete-swipe drops any pending
        // drag so the two gestures never fight for the same pointer.
        if let Some(press) = self.press {
            if press.is_reorder() {
                let metrics = self.list_metrics();
                if !self.reorder.is_pending()
                    && let Some(loc) = self.model.locations.get(press.index)
                {
                    // Lazily arm the drag the moment it classifies as a
                    // reorder, grabbing the chip at the press point.
                    let grab = press.origin.y - metrics.slot_lead(press.index);
                    self.reorder.begin(loc.id, press.index, grab, press.origin);
                }
                self.reorder.pointer_moved(self.last_mouse, &metrics);
            } else if press.is_swipe() {
                self.reorder.cancel();
            }
        }
    }

    fn handle_overview_wheel(
        &mut self,
        delta: winit::event::MouseScrollDelta,
        scale_factor: f64,
    ) -> bool {
        use winit::event::MouseScrollDelta;
        if self.over_rect.contains(self.last_mouse) && self.over_max_scroll > 0.0 {
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => f64::from(y) * 48.0,
                MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
            };
            self.over_scroll = (self.over_scroll - dy).clamp(0.0, self.over_max_scroll);
            true
        } else {
            false
        }
    }

    fn handle_overview_right_click(&mut self) -> bool {
        if let Some(idx) = self.chip_at(self.last_mouse) {
            self.menu = Some(ContextMenu::for_entry(idx, self.last_mouse));
            true
        } else if self.menu.is_some() {
            self.menu = None;
            true
        } else {
            false
        }
    }

    fn handle_overview_left_press(&mut self) -> (bool, bool) {
        let mut changed = false;
        if self.menu.is_some() {
            let hit = self
                .over_hotspots
                .iter()
                .rev()
                .find(|h| {
                    matches!(h.action, OverAction::MenuItem(..)) && h.rect.contains(self.last_mouse)
                })
                .map(|h| h.action);
            if let Some(action) = hit {
                changed = self.apply_over_action(action);
            } else {
                self.menu = None;
            }
            (true, changed)
        } else {
            let hit = self
                .over_hotspots
                .iter()
                .rev()
                .find(|h| h.rect.contains(self.last_mouse))
                .map(|h| h.action);
            match hit {
                Some(OverAction::PressChip(i)) => {
                    self.press = Some(PointerPress::new(i, self.last_mouse));
                    self.reorder.cancel();
                    (true, changed)
                }
                Some(action) => {
                    changed = self.apply_over_action(action);
                    (true, changed)
                }
                // A press on empty list space (the search field's own
                // click-outside blur is handled by the Autocomplete
                // routing above).
                None => (false, changed),
            }
        }
    }

    fn handle_overview_left_release(&mut self) -> (bool, bool) {
        let mut changed = false;
        let Some(press) = self.press.take() else {
            return (false, changed);
        };
        if press.is_reorder() {
            if self.delete_zone_rect().contains(self.last_mouse) {
                // Drop onto the trash: abandon the reorder, delete.
                self.reorder.cancel();
                changed = self.apply_entry_action(press.index, EntryAction::Delete);
            } else if let Some((from, to)) = self.reorder.commit() {
                // Commit the live insertion the controller tracked.
                changed = self.model.move_location(from, to);
                if changed && let Some(location) = self.model.locations.get(to) {
                    self.queue_durable_action(WeatherAction::SetLocationRank {
                        id: location.id.to_string(),
                        rank: location.order_rank,
                    });
                }
            } else {
                // Dropped in place (or never activated): no change.
                self.reorder.cancel();
            }
        } else if press.is_swipe() {
            if press.swipe_dx() >= SWIPE_COMMIT_DX {
                changed = self.apply_entry_action(press.index, EntryAction::swipe_left());
            }
        } else {
            changed = self.apply_over_action(OverAction::SelectLocation(press.index));
        }
        (true, changed)
    }

    /// Typing / arrow / Enter / Esc while the search field is focused is
    /// owned by the shared Autocomplete in [`Self::route_overview_search_field`];
    /// this only runs when the list itself has focus.
    fn handle_overview_key(&mut self, logical_key: &winit::keyboard::Key) -> (bool, bool) {
        use winit::keyboard::{Key, NamedKey};

        let mut handled = false;
        let mut changed = false;
        match logical_key {
            Key::Named(NamedKey::Escape) => {
                // Escape abandons an in-flight reorder, snapping the list
                // back to its original order (the model was never touched —
                // the drag is purely visual until drop).
                if self.reorder.cancel() {
                    self.press = None;
                    handled = true;
                } else if self.menu.is_some() {
                    self.menu = None;
                    handled = true;
                }
            }
            // Arrow up/down move the highlighted city; the detail pane
            // follows the selection live (side-by-side) and a fresh
            // forecast is fetched for it.
            Key::Named(NamedKey::ArrowDown) => {
                self.menu = None;
                changed = self.move_selection(1);
                handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.menu = None;
                changed = self.move_selection(-1);
                handled = true;
            }
            // Enter opens the forecast for the highlighted city — the only
            // way to reach the detail by keyboard in the narrow push
            // layout, where it isn't shown yet.
            Key::Named(NamedKey::Enter) => {
                if self.model.selected_location().is_some() {
                    self.refresh_selected();
                    self.nav_request = Some(NavIntent::ShowDetail);
                    handled = true;
                }
            }
            Key::Character(ch) if ch.as_str() == "/" => {
                self.begin_search();
                handled = true;
            }
            _ => {}
        }
        (handled, changed)
    }

    // =================================================================
    // DETAIL pane — the forecast for the selected city
    // =================================================================

    pub fn draw_detail(
        &mut self,
        rect: Rect,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.detail_rect = rect;
        self.draw_sky_background(scene, t, theme, rect);

        let Some(loc) = self.model.selected_location().cloned() else {
            self.draw_detail_zero_state(rect, scene, t, tr, theme);
            return;
        };
        let snap_opt = self.model.selected_snapshot().cloned();
        let body_top = self.draw_detail_header(rect, scene, t, tr, theme, &loc, snap_opt.as_ref());

        let c = &theme.tokens.colors;
        let mx = rect.x0 + PAD;
        let right = rect.x1 - PAD;
        // The rule under the header derives against the pane's base tier.
        ui::surface::separator(scene, t, mx, right, body_top - 9.5, 0.0, c.surfaces.base);

        let Some(snap) = snap_opt else {
            self.max_scroll = 0.0;
            text_left(
                scene,
                tr,
                "Fetching current conditions…",
                mx,
                body_top + 24.0,
                15.0,
                TextWeight::Medium,
                c.text_dim,
            );
            return;
        };

        let mw = (rect.width() - 2.0 * PAD).max(140.0);
        let layout = DetailBodyLayout::measure(
            mw,
            &snap,
            &theme.tokens.spacing,
            &theme.tokens.metrics,
            tr.text_scale(),
        );
        let view_h = (rect.y1 - PAD) - body_top;
        self.max_scroll = (layout.body_h - view_h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, self.max_scroll);

        let clip = Rect::new(rect.x0, body_top - 8.0, rect.x1, rect.y1);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &clip);
        let mut y = body_top - self.scroll;
        let body = DetailBody {
            rect,
            snap: &snap,
            layout: &layout,
        };
        let ctx = &mut PaintCtx::new(scene, t, tr, theme);
        y = self.draw_detail_hero(ctx, &body, y);
        y = self.draw_detail_tiles(ctx, &body, y);
        y = self.draw_detail_hourly(ctx, &body, y);
        y = self.draw_detail_days(ctx, &body, y);
        self.draw_detail_air_quality_section(ctx, &body, y);
        ctx.scene.pop_layer();

        self.draw_detail_footer(ctx, rect, body_top, layout.body_h);
    }

    /// Composed zero-state: a friendly weather glyph over an inviting
    /// prompt, centred in the detail pane — never a bare label on a big
    /// empty gradient (HIG §7). The copy differs by whether the user has
    /// any saved places: a fresh vault points at the search; an existing
    /// roster asks them to pick a city.
    fn draw_detail_zero_state(
        &mut self,
        rect: Rect,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let cx = rect.center().x;
        let anchor = rect.y0 + rect.height() * 0.34;
        let glyph = 92.0;
        icon::draw_icon(
            scene,
            t,
            Sky::PartlyCloudy,
            true,
            Rect::new(cx - glyph / 2.0, anchor, cx + glyph / 2.0, anchor + glyph),
            theme,
        );
        let (headline, sub) = if self.model.locations.is_empty() {
            (
                "No cities yet",
                "Search for a city to see its current conditions and 7-day forecast.",
            )
        } else {
            (
                "Pick a city",
                "Choose a saved place on the left to see its forecast.",
            )
        };
        text_center(
            scene,
            tr,
            headline,
            cx,
            anchor + glyph + 20.0,
            22.0,
            TextWeight::Bold,
            c.text_primary,
        );
        text_center(
            scene,
            tr,
            sub,
            cx,
            anchor + glyph + 54.0,
            14.0,
            TextWeight::Regular,
            c.text_dim,
        );
        self.refresh_btn = Rect::ZERO;
        self.max_scroll = 0.0;
    }

    /// Place name/subtitle, the refresh button, and (once a snapshot has
    /// landed) its fetch date/local time. Returns the y the divider under
    /// the header sits at.
    /// The header's refresh button: a raised square with the refresh glyph,
    /// hover-lifted, and recorded as this frame's hit rect.
    fn draw_refresh_button(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        refresh: Rect,
    ) {
        let rhover = refresh.contains(self.last_mouse);
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(refresh, "")
                .icon(Icon::Refresh)
                .hovered(rhover)
                .accessible_name("Refresh forecast"),
        );
        self.refresh_btn = refresh;
    }

    fn draw_detail_header(
        &mut self,
        rect: Rect,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        loc: &SavedLocation,
        snap: Option<&WeatherSnapshot>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let top_pad = if self.detail_reserve_back {
            PAD + BACK_RESERVE
        } else {
            PAD
        };
        let mx = rect.x0 + PAD;
        let right = rect.x1 - PAD;
        let hy = rect.y0 + top_pad;
        let sp = &theme.tokens.spacing;

        // The header stacks from measured lines: city name, then its region
        // one small gap below; the right-hand date/time column stacks the same
        // way. The title elides before the right column instead of running
        // under it, and the header's height (returned as the body top) grows
        // with the lines rather than staying the authored 74px.
        let rb = 34.0;
        let sub_h = f64::from(tr.measure("Ag", DETAIL_SUBTITLE_TEXT, None).1);
        let date_h = f64::from(tr.measure_weighted("Ag", 14.0, None, TextWeight::Medium).1);
        let (date_str, time_str) = snap.map_or((String::new(), String::new()), |snap| {
            (
                fmt_date(&snap.current.time),
                format!("Local time {}", hhmm(&snap.current.time)),
            )
        });
        let right_col_w = f64::from(
            tr.measure_weighted(&date_str, 14.0, None, TextWeight::Medium)
                .0
                .max(tr.measure(&time_str, 12.0, None).0),
        );
        let right_col_x = right - rb - f64::from(sp.s3);
        let title_max = (right_col_x - right_col_w - f64::from(sp.s4) - mx).max(40.0);
        let sub_y = crate::presentation::draw_identity(scene, tr, theme, loc, mx, hy, title_max);
        // The title's measured line, as `draw_identity` lays it out, so the
        // refresh button centres on the city name rather than on an
        // authored offset.
        let name_h = f64::from(
            tr.measure_weighted("Ag", DETAIL_TITLE_TEXT, None, TextWeight::Bold)
                .1,
        );
        // The authored 74px at 1× (not the 76.75 that stacking the measured
        // lines plus token gaps gives), grown by the two lines' extra height.
        let header_h = detail_header_h(sp, tr.text_scale());

        // Refresh button (top-right) — re-fetches this city's forecast,
        // centred on the city name's line.
        let refresh_y = hy + (name_h - rb) / 2.0;
        let refresh = Rect::new(right - rb, refresh_y, right, refresh_y + rb);
        self.draw_refresh_button(scene, t, tr, theme, refresh);

        if snap.is_some() {
            let date_y = hy + 2.0;
            text_right(
                scene,
                tr,
                &date_str,
                right_col_x,
                date_y,
                14.0,
                TextWeight::Medium,
                c.text_primary,
            );
            text_right(
                scene,
                tr,
                &time_str,
                right_col_x,
                date_y + date_h + f64::from(sp.s1),
                12.0,
                TextWeight::Regular,
                c.text_dim,
            );
        }
        // A connection problem is a third line under the region, an `s1`
        // below it on its own measured box; the header grows by that line so
        // the rule beneath never runs through it.
        let mut status_extra = 0.0;
        if self.status.starts_with("Couldn't") || self.status.starts_with("Offline") {
            let status_y = sub_y + sub_h + f64::from(sp.s1);
            text_left(
                scene,
                tr,
                &self.status,
                mx,
                status_y,
                11.0,
                TextWeight::Medium,
                c.semantic.warning,
            );
            status_extra = line_h(11.0, tr.text_scale()) + f64::from(sp.s1);
        }
        hy + header_h + status_extra
    }

    /// The hero card: condition glyph, big temperature, condition label,
    /// "feels like", and the sunrise/sunset arc (or a compact fallback on a
    /// narrow pane). Returns the y cursor for the next section.
    fn draw_detail_hero(&self, ctx: &mut PaintCtx<'_>, body: &DetailBody<'_>, y: f64) -> f64 {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let DetailBody { rect, snap, layout } = *body;
        let c = &theme.tokens.colors;
        let mx = rect.x0 + PAD;
        let mw = (rect.width() - 2.0 * PAD).max(140.0);
        let units = self.units;
        let cur = &snap.current;

        let hero = Rect::new(mx, y, mx + mw, y + layout.hero_h);
        card(scene, t, hero, theme);
        let sky = Sky::from_code(cur.weather_code);
        icon::draw_icon(
            scene,
            t,
            sky,
            cur.is_day,
            Rect::new(
                hero.x0 + 24.0,
                hero.y0 + 36.0,
                hero.x0 + 116.0,
                hero.y0 + 128.0,
            ),
            theme,
        );
        let temp = snap.temp_in(cur.temperature, units).round() as i64;
        let temp_str = format!("{temp}");
        let temp_x = hero.x0 + 132.0;
        tr.draw_text_weighted(
            scene,
            &temp_str,
            layout.text.hero_temp,
            c.text_primary,
            t * Affine::translate((temp_x, hero.y0 + 30.0)),
            None,
            TextWeight::Bold,
        );
        let (tnw, _) =
            tr.measure_weighted(&temp_str, layout.text.hero_temp, None, TextWeight::Bold);
        text_left(
            scene,
            tr,
            units.degree_glyph(),
            temp_x + f64::from(tnw) + 4.0,
            hero.y0 + 40.0,
            32.0,
            TextWeight::Regular,
            c.text_dim,
        );
        let cond = Sky::label(cur.weather_code);
        text_left(
            scene,
            tr,
            cond,
            temp_x + 2.0,
            hero.y0 + layout.hero_cond_dy,
            layout.text.hero_cond,
            TextWeight::Semibold,
            c.text_primary,
        );
        let feels = snap.temp_in(cur.apparent, units).round() as i64;
        text_left(
            scene,
            tr,
            &format!("Feels like {feels}{}", units.temp_suffix()),
            temp_x + 2.0,
            hero.y0 + layout.hero_feels_dy,
            layout.text.hero_feels,
            TextWeight::Regular,
            c.text_dim,
        );

        if let Some(today) = snap.daily.first() {
            Self::draw_hero_sun(
                &mut PaintCtx::new(scene, t, tr, theme),
                hero,
                mw,
                cur,
                today,
            );
        }

        y + layout.hero_h + layout.gap
    }

    /// The sunrise/sunset arc on a wide pane, or the compact Sunrise/Sunset
    /// label fallback on a narrow one.
    fn draw_hero_sun(
        ctx: &mut PaintCtx<'_>,
        hero: Rect,
        mw: f64,
        cur: &CurrentConditions,
        today: &DayForecast,
    ) {
        let sr = hhmm(&today.sunrise);
        let ss = hhmm(&today.sunset);
        if mw >= 560.0 {
            Self::draw_sun_arc(
                ctx,
                Rect::new(
                    hero.x1 - 226.0,
                    hero.y0 + 22.0,
                    hero.x1 - 22.0,
                    hero.y1 - 22.0,
                ),
                cur,
                &sr,
                &ss,
            );
        } else {
            let (scene, tr, theme) = (&mut *ctx.scene, &mut *ctx.text, ctx.theme);
            let c = &theme.tokens.colors;
            // Two label/value pairs stacked up from the card's `s3` bottom
            // inset on their measured lines: `s1` inside a pair, `s2`
            // between the pairs — nothing runs past the card.
            let sp = &theme.tokens.spacing;
            let scale = tr.text_scale();
            let (lh_label, lh_value) = (line_h(10.0, scale), line_h(14.0, scale));
            let (s1, s2, s3, s4) = (
                f64::from(sp.s1),
                f64::from(sp.s2),
                f64::from(sp.s3),
                f64::from(sp.s4),
            );
            let sx = hero.x1 - s4;
            let sunset_value_y = hero.y1 - s3 - lh_value;
            let sunset_label_y = sunset_value_y - s1 - lh_label;
            let sunrise_value_y = sunset_label_y - s2 - lh_value;
            let sunrise_label_y = sunrise_value_y - s1 - lh_label;
            for (label, value, label_y, value_y) in [
                ("Sunrise", sr.as_str(), sunrise_label_y, sunrise_value_y),
                ("Sunset", ss.as_str(), sunset_label_y, sunset_value_y),
            ] {
                text_right(
                    scene,
                    tr,
                    label,
                    sx,
                    label_y,
                    10.0,
                    TextWeight::Semibold,
                    c.text_dim,
                );
                text_right(
                    scene,
                    tr,
                    value,
                    sx,
                    value_y,
                    14.0,
                    TextWeight::Medium,
                    c.text_primary,
                );
            }
        }
    }

    /// The metric-tile grid (wind/humidity/precip/pressure/UV). Returns the
    /// y cursor for the next section.
    fn draw_detail_tiles(&self, ctx: &mut PaintCtx<'_>, body: &DetailBody<'_>, y: f64) -> f64 {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let DetailBody { rect, snap, layout } = *body;
        let c = &theme.tokens.colors;
        let mx = rect.x0 + PAD;
        let mw = (rect.width() - 2.0 * PAD).max(140.0);
        let units = self.units;
        let cur = &snap.current;
        let gap = layout.gap;
        let cols = layout.tile_cols;

        let wind = snap.wind_in(cur.wind_speed, units).round() as i64;
        let tiles: [(&str, String, String); 5] = [
            (
                "WIND",
                format!("{wind}"),
                format!(
                    "{} {}",
                    units::compass(cur.wind_direction),
                    units.wind_suffix()
                ),
            ),
            (
                "HUMIDITY",
                format!("{}", cur.humidity.round() as i64),
                "%".to_string(),
            ),
            (
                "PRECIP",
                format!("{:.1}", cur.precipitation),
                "mm now".to_string(),
            ),
            (
                "PRESSURE",
                format!("{}", cur.pressure.round() as i64),
                "hPa".to_string(),
            ),
            (
                "UV INDEX",
                format!("{}", cur.uv_index.round() as i64),
                uv_word(cur.uv_index).to_string(),
            ),
        ];
        // Full rows use even columns; a partial last row stretches its tiles to
        // fill the width, so there's never an orphaned tile with a dead gap.
        let full_rows = tiles.len() / cols;
        let rem = tiles.len() % cols;
        for (i, (label, value, sub)) in tiles.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let row_count = if row < full_rows { cols } else { rem };
            let tw = (mw - (row_count as f64 - 1.0) * gap) / row_count as f64;
            let tx = mx + col as f64 * (tw + gap);
            let ty = y + row as f64 * (layout.tile_h + gap);
            let tile = Rect::new(tx, ty, tx + tw, ty + layout.tile_h);
            card(scene, t, tile, theme);
            text_left(
                scene,
                tr,
                label,
                tile.x0 + 12.0,
                tile.y0 + 12.0,
                layout.text.tile_label,
                TextWeight::Semibold,
                c.text_dim,
            );
            text_left(
                scene,
                tr,
                value,
                tile.x0 + 12.0,
                tile.y0 + layout.tile_value_dy,
                layout.text.tile_value,
                TextWeight::Bold,
                c.text_primary,
            );
            text_left(
                scene,
                tr,
                sub,
                tile.x0 + 12.0,
                tile.y1 - layout.tile_sub_inset,
                layout.text.tile_unit,
                TextWeight::Regular,
                c.text_dim,
            );
        }

        y + layout.tiles_block_h + layout.gap
    }

    /// The 24-hour scrollable strip card. Returns the y cursor for the
    /// next section.
    fn draw_detail_hourly(&mut self, ctx: &mut PaintCtx<'_>, body: &DetailBody<'_>, y: f64) -> f64 {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let DetailBody { rect, snap, layout } = *body;
        let c = &theme.tokens.colors;
        let mx = rect.x0 + PAD;
        let mw = (rect.width() - 2.0 * PAD).max(140.0);
        let units = self.units;
        let accent = self.accent(theme);

        let hcard = Rect::new(mx, y, mx + mw, y + layout.hourly_h);
        card(scene, t, hcard, theme);
        text_left(
            scene,
            tr,
            "Next 24 hours",
            hcard.x0 + 16.0,
            hcard.y0 + 12.0,
            14.0,
            TextWeight::Semibold,
            c.text_primary,
        );
        if self.hourly_max_scroll > 1.0 {
            text_right(
                scene,
                tr,
                "scroll →",
                hcard.x1 - 16.0,
                hcard.y0 + 14.0,
                11.0,
                TextWeight::Regular,
                c.text_dim,
            );
        }

        let cols: Vec<chart::HourColumn> = snap
            .hourly
            .iter()
            .enumerate()
            .map(|(i, h)| chart::HourColumn {
                hour: h.hour_label(),
                temp: snap.temp_in(h.temperature, units),
                precip: h.precip_probability,
                code: h.weather_code,
                is_day: h.is_day,
                is_now: i == 0,
            })
            .collect();

        // Each hour gets at least HOUR_COL_MIN_W; the strip scrolls when the 24
        // columns are wider than the band rather than squeezing them. The band
        // sits on the card's `s4` gutter and leaves an `s6` below for the
        // scrollbar (`s2` gap, `s2` track, `s2` to the card edge).
        let sp = &theme.tokens.spacing;
        let band = Rect::new(
            hcard.x0 + f64::from(sp.s4),
            hcard.y0 + layout.hourly_band_top,
            hcard.x1 - f64::from(sp.s4),
            hcard.y1 - f64::from(sp.s6),
        );
        let n = cols.len().max(1);
        let col_w = (band.width() / n as f64).max(HOUR_COL_MIN_W);
        let content_w = col_w * n as f64;
        self.hourly_viewport = band;
        self.hourly_content_w = content_w;
        self.hourly_max_scroll = (content_w - band.width()).max(0.0);
        self.hourly_scroll_x = self.hourly_scroll_x.clamp(0.0, self.hourly_max_scroll);

        chart::HourlyStrip {
            cols: &cols,
            col_w,
            scroll_x: self.hourly_scroll_x,
            accent,
            temp_suffix: "°",
        }
        .draw(&mut PaintCtx::new(scene, t, tr, theme), band);

        self.draw_hourly_scrollbar(scene, t, theme, band, content_w);

        y + layout.hourly_h + layout.gap
    }

    /// The shared horizontal scrollbar for the hourly strip, shown only
    /// when the 24 columns overflow the band.
    fn draw_hourly_scrollbar(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        band: Rect,
        content_w: f64,
    ) {
        let c = &theme.tokens.colors;
        let s2 = f64::from(theme.tokens.spacing.s2);
        let hmetrics = ScrollMetrics::new(content_w, band.width(), self.hourly_scroll_x);
        // An `s2` under the band, an `s2` tall, an `s2` above the card edge.
        let track = Rect::new(band.x0, band.y1 + s2, band.x1, band.y1 + 2.0 * s2);
        self.hourly_track = track;
        if hmetrics.overflows()
            && let Some(thumb) = self.hourly_bar.thumb_rect(track, hmetrics)
        {
            // The track derives against the card at track strength — visible
            // on both themes, never the faint hover wash.
            scene.fill(
                Fill::NonZero,
                t,
                track_fill(c.surfaces.raised),
                None,
                &RoundedRect::from_rect(track, s2 / 2.0),
            );
            let thumb_color = if self.hourly_bar.is_active() {
                c.text_dim
            } else {
                c.text_dim.with_alpha_factor(0.7)
            };
            scene.fill(
                Fill::NonZero,
                t,
                thumb_color,
                None,
                &RoundedRect::from_rect(thumb, 3.0),
            );
        }
    }

    /// The 7-day forecast card: header + one row per day. Returns the y
    /// cursor for the next section.
    fn draw_detail_days(&self, ctx: &mut PaintCtx<'_>, body: &DetailBody<'_>, y: f64) -> f64 {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let DetailBody { rect, snap, layout } = *body;
        let c = &theme.tokens.colors;
        let mx = rect.x0 + PAD;
        let mw = (rect.width() - 2.0 * PAD).max(140.0);
        let units = self.units;

        let dcard = Rect::new(mx, y, mx + mw, y + layout.days_block_h);
        card(scene, t, dcard, theme);
        text_left(
            scene,
            tr,
            "7-day forecast",
            dcard.x0 + 16.0,
            dcard.y0 + 12.0,
            14.0,
            TextWeight::Semibold,
            c.text_primary,
        );

        let (mut wmin, mut wmax) = (f64::INFINITY, f64::NEG_INFINITY);
        for d in &snap.daily {
            wmin = wmin.min(d.temp_min);
            wmax = wmax.max(d.temp_max);
        }
        if (wmax - wmin).abs() < 1.0 {
            wmax += 1.0;
            wmin -= 1.0;
        }

        // The temperature column is as wide as the widest lo / hi label this
        // forecast actually paints (a three-digit or negative reading grows
        // it), so the hi label never runs past the card's gutter.
        let temp_w = snap
            .daily
            .iter()
            .flat_map(|d| {
                let lo = snap.temp_in(d.temp_min, units).round() as i64;
                let hi = snap.temp_in(d.temp_max, units).round() as i64;
                [
                    (format!("{lo}°"), TextWeight::Medium),
                    (format!("{hi}°"), TextWeight::Semibold),
                ]
            })
            .map(|(label, weight)| f64::from(tr.measure_weighted(&label, 14.0, None, weight).0))
            .fold(0.0_f64, f64::max)
            .ceil();
        let rows = ForecastRows {
            geo: DayRowGeometry::new(dcard, &theme.tokens.spacing, temp_w),
            snap,
            units,
            week: TrendRange { lo: wmin, hi: wmax },
        };
        let rows_top = dcard.y0 + layout.days_header_h;
        let ctx = &mut PaintCtx::new(scene, t, tr, theme);
        for (i, d) in snap.daily.iter().enumerate() {
            let ry = rows_top + i as f64 * layout.day_row_h;
            rows.draw_row(
                ctx,
                i,
                d,
                Rect::new(
                    rows.geo.inner_l,
                    ry,
                    rows.geo.inner_r,
                    ry + layout.day_row_h,
                ),
            );
        }

        y + layout.days_block_h + layout.gap
    }

    /// The air-quality card: header + the AQI hero and pollutant tiles.
    fn draw_detail_air_quality_section(
        &self,
        ctx: &mut PaintCtx<'_>,
        body: &DetailBody<'_>,
        y: f64,
    ) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let DetailBody { rect, layout, .. } = *body;
        let c = &theme.tokens.colors;
        let mx = rect.x0 + PAD;
        let mw = (rect.width() - 2.0 * PAD).max(140.0);
        // Air-quality block: header + AQI hero + a row (or two) of pollutant
        // tiles. Reserved whether or not a reading is available yet, so the
        // scroll geometry stays stable while it loads.
        let aq = self.model.selected_air_quality().cloned();

        let acard = Rect::new(mx, y, mx + mw, y + layout.aq_block_h);
        card(scene, t, acard, theme);
        text_left(
            scene,
            tr,
            "Air quality",
            acard.x0 + 16.0,
            acard.y0 + 12.0,
            14.0,
            TextWeight::Semibold,
            c.text_primary,
        );
        self.draw_air_quality(scene, t, tr, theme, acard, aq.as_ref(), layout);
    }

    /// The status line + the body's vertical scroll indicator.
    fn draw_detail_footer(&self, ctx: &mut PaintCtx<'_>, rect: Rect, body_top: f64, body_h: f64) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let mx = rect.x0 + PAD;
        let view_h = (rect.y1 - PAD) - body_top;
        let c = &theme.tokens.colors;
        // Connection/update problems are called out beside the city name,
        // where they remain visible even when the forecast body scrolls.
        // Keep the footer for transient success/progress text only.
        if !self.status.is_empty()
            && !self.status.starts_with("Couldn't")
            && !self.status.starts_with("Offline")
        {
            text_left(
                scene,
                tr,
                &self.status,
                mx,
                rect.y1 - 20.0,
                11.0,
                TextWeight::Regular,
                c.text_dim,
            );
        }
        if self.max_scroll > 1.0 {
            let tx = rect.x1 - 6.0;
            let track_top = body_top + 2.0;
            let track_bot = rect.y1 - 6.0;
            let track_h = track_bot - track_top;
            let thumb_h = (track_h * (view_h / body_h)).clamp(28.0, track_h);
            let thumb_y = track_top + (self.scroll / self.max_scroll) * (track_h - thumb_h);
            scene.fill(
                Fill::NonZero,
                t,
                c.lift(1.0),
                None,
                &RoundedRect::new(tx, track_top, tx + 3.0, track_bot, 1.5),
            );
            scene.fill(
                Fill::NonZero,
                t,
                c.text_dim.with_alpha_factor(0.7),
                None,
                &RoundedRect::new(tx, thumb_y, tx + 3.0, thumb_y + thumb_h, 1.5),
            );
        }
    }

    fn draw_sky_background(&self, scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect) {
        let c = &theme.tokens.colors;
        let base = c.surfaces.base;
        let top = if self.is_day() {
            lerp_color(base, c.accent[6], 0.20)
        } else {
            lerp_color(base, c.neutral[0], 0.55)
        };
        let grad = Gradient::new_linear(
            Point::new(rect.x0, rect.y0),
            Point::new(rect.x0, rect.y0 + rect.height() * 0.65),
        )
        .with_stops([(0.0_f32, top), (1.0_f32, base)]);
        scene.fill(Fill::NonZero, t, &Brush::Gradient(grad), None, &rect);
    }

    /// The sun's arc across `area`, its dot placed by `cur`'s observation
    /// time between `sunrise` and `sunset`, with both times labelled.
    fn draw_sun_arc(
        ctx: &mut PaintCtx<'_>,
        area: Rect,
        cur: &CurrentConditions,
        sunrise: &str,
        sunset: &str,
    ) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        // Current observation time ("YYYY-MM-DDTHH:MM"); positions the sun dot.
        let (now_iso, is_day) = (cur.time.as_str(), cur.is_day);
        let c = &theme.tokens.colors;
        let sp = &theme.tokens.spacing;
        let scale = tr.text_scale();
        let (lh_label, lh_value) = (line_h(10.0, scale), line_h(13.0, scale));
        let (s1, s2) = (f64::from(sp.s1), f64::from(sp.s2));
        let cx = f64::midpoint(area.x0, area.x1);
        // The horizon sits above the label/value pair it carries: `s2`
        // under the rule, `s1` between label and time, both measured.
        let label_y = area.y1 - lh_value - s1 - lh_label;
        let value_y = label_y + lh_label + s1;
        let baseline = label_y - s2;
        let radius = (area.width() / 2.0 - 16.0).min(70.0);
        // The horizon rule derives against the hero card, inset `s2`.
        ui::surface::separator(
            scene,
            t,
            area.x0,
            area.x1,
            baseline + 0.5,
            f64::from(theme.tokens.spacing.s2),
            c.surfaces.raised,
        );
        let mut arc = BezPath::new();
        let steps = 28;
        for i in 0..=steps {
            let frac = f64::from(i) / f64::from(steps);
            let ang = std::f64::consts::PI * frac;
            let px = cx - radius * ang.cos();
            let py = baseline - radius * ang.sin();
            if i == 0 {
                arc.move_to(Point::new(px, py));
            } else {
                arc.line_to(Point::new(px, py));
            }
        }
        scene.stroke(
            &Stroke::new(1.5),
            t,
            c.text_dim.with_alpha_factor(0.7),
            None,
            &arc,
        );
        // Position the sun/moon dot from the real observation time relative to
        // today's sunrise/sunset (0 = sunrise at the left horizon, 0.5 = solar
        // noon at the apex, 1 = sunset at the right horizon). Falls back to a
        // neutral position only when the times can't be parsed.
        let now_hhmm = hhmm(now_iso);
        let frac =
            sun_arc_fraction(&now_hhmm, sunrise, sunset).unwrap_or(if is_day { 0.5 } else { 0.08 });
        let ang = std::f64::consts::PI * frac;
        let sx = cx - radius * ang.cos();
        let sy = baseline - radius * ang.sin();
        let sun_color = if is_day {
            c.semantic.warning
        } else {
            c.neutral[7]
        };
        scene.fill(
            Fill::NonZero,
            t,
            sun_color,
            None,
            &Circle::new(Point::new(sx, sy), 7.0),
        );
        text_left(
            scene,
            tr,
            "Sunrise",
            area.x0 + s1,
            label_y,
            10.0,
            TextWeight::Semibold,
            c.text_dim,
        );
        text_left(
            scene,
            tr,
            sunrise,
            area.x0 + s1,
            value_y,
            13.0,
            TextWeight::Medium,
            c.text_primary,
        );
        text_right(
            scene,
            tr,
            "Sunset",
            area.x1 - s1,
            label_y,
            10.0,
            TextWeight::Semibold,
            c.text_dim,
        );
        text_right(
            scene,
            tr,
            sunset,
            area.x1 - s1,
            value_y,
            13.0,
            TextWeight::Medium,
            c.text_primary,
        );
    }

    /// The air-quality section: an AQI severity hero + a row (or two) of key
    /// pollutant tiles, or an honest loading / unavailable message when there
    /// is no reading yet. Real data only — a missing value shows an em-dash,
    /// never a fabricated zero.
    fn draw_air_quality(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        aq: Option<&AirQuality>,
        layout: &DetailBodyLayout,
    ) {
        let c = &theme.tokens.colors;
        let ix = area.x0 + 16.0;
        let iw = area.width() - 32.0;
        let hero_top = area.y0 + layout.aq_header_h;

        let Some(aq) = aq.filter(|a| a.has_any()) else {
            let msg = if self.is_fetching() {
                "Loading air quality…"
            } else {
                "Air quality unavailable"
            };
            text_center(
                scene,
                tr,
                msg,
                area.center().x,
                hero_top + 22.0,
                14.0,
                TextWeight::Medium,
                c.text_dim,
            );
            return;
        };

        // AQI hero: a severity-tinted chip with the big index number, the band
        // word, and a caption naming the scale.
        let hero = Rect::new(ix, hero_top, area.x1 - 16.0, hero_top + layout.aq_hero_h);
        Self::draw_aqi_hero(scene, t, tr, theme, hero, aq);
        Self::draw_aq_tiles(
            &mut PaintCtx::new(scene, t, tr, theme),
            hero,
            iw,
            aq,
            layout,
        );
    }

    /// The AQI hero: a severity-tinted chip with the big index number, the
    /// band word and a caption naming the scale (European AQI when
    /// available, else a rough US-AQI fallback).
    fn draw_aqi_hero(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hero: Rect,
        aq: &AirQuality,
    ) {
        let c = &theme.tokens.colors;
        if let Some(aqi) = aq.european_aqi {
            let sev = aqi_band_color(aqi, theme);
            let chip = RoundedRect::from_rect(
                Rect::new(hero.x0, hero.y0, hero.x0 + 116.0, hero.y1),
                f64::from(theme.tokens.radii.lg),
            );
            scene.fill(Fill::NonZero, t, sev.with_alpha_factor(0.16), None, &chip);
            scene.stroke(
                &Stroke::new(1.0),
                t,
                sev.with_alpha_factor(0.55),
                None,
                &chip,
            );
            let num = format!("{}", aqi.round() as i64);
            tr.draw_text_weighted(
                scene,
                &num,
                34.0,
                c.text_primary,
                t * Affine::translate((hero.x0 + 16.0, hero.y0 + 12.0)),
                None,
                TextWeight::Bold,
            );
            let (nw, _) = tr.measure_weighted(&num, 34.0, None, TextWeight::Bold);
            text_left(
                scene,
                tr,
                "AQI",
                hero.x0 + 24.0 + f64::from(nw),
                hero.y0 + 30.0,
                12.0,
                TextWeight::Semibold,
                c.text_dim,
            );
            text_left(
                scene,
                tr,
                api::european_aqi_band(aqi),
                hero.x0 + 132.0,
                hero.y0 + 12.0,
                19.0,
                TextWeight::Semibold,
                sev,
            );
            text_left(
                scene,
                tr,
                "European AQI · now",
                hero.x0 + 132.0,
                hero.y0 + 40.0,
                12.0,
                TextWeight::Regular,
                c.text_dim,
            );
        } else if let Some(us) = aq.us_aqi {
            let sev = aqi_band_color(us / 2.0, theme); // rough EU-equivalent tint
            text_left(
                scene,
                tr,
                &format!("{} US AQI", us.round() as i64),
                hero.x0,
                hero.y0 + 18.0,
                20.0,
                TextWeight::Bold,
                sev,
            );
        }
    }

    /// The key pollutant tiles (µg/m³) below the AQI hero, spread across
    /// `iw` from its left edge.
    fn draw_aq_tiles(
        ctx: &mut PaintCtx<'_>,
        hero: Rect,
        iw: f64,
        aq: &AirQuality,
        layout: &DetailBodyLayout,
    ) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let ix = hero.x0;
        let c = &theme.tokens.colors;
        let (cols, gap) = (layout.aq_cols, layout.gap);
        let tiles: [(&str, Option<f64>); 4] = [
            ("PM2.5", aq.pm2_5),
            ("PM10", aq.pm10),
            ("NO₂", aq.nitrogen_dioxide),
            ("O₃", aq.ozone),
        ];
        let tiles_top = hero.y1 + 12.0;
        let cols = cols.max(1);
        let tw = (iw - (cols as f64 - 1.0) * gap) / cols as f64;
        for (i, (label, val)) in tiles.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let tx = ix + col as f64 * (tw + gap);
            let ty = tiles_top + row as f64 * (layout.aq_tile_h + gap);
            let tile = Rect::new(tx, ty, tx + tw, ty + layout.aq_tile_h);
            card(scene, t, tile, theme);
            text_left(
                scene,
                tr,
                label,
                tile.x0 + 12.0,
                tile.y0 + 10.0,
                layout.text.aq_tile_label,
                TextWeight::Semibold,
                c.text_dim,
            );
            match val {
                Some(v) => {
                    text_left(
                        scene,
                        tr,
                        &format!("{}", v.round() as i64),
                        tile.x0 + 12.0,
                        tile.y0 + layout.aq_tile_value_dy,
                        layout.text.aq_tile_value,
                        TextWeight::Bold,
                        c.text_primary,
                    );
                    text_left(
                        scene,
                        tr,
                        "µg/m³",
                        tile.x0 + 12.0,
                        tile.y1 - layout.aq_tile_sub_inset,
                        layout.text.aq_tile_label,
                        TextWeight::Regular,
                        c.text_dim,
                    );
                }
                None => {
                    text_left(
                        scene,
                        tr,
                        "—",
                        tile.x0 + 12.0,
                        tile.y0 + 26.0,
                        20.0,
                        TextWeight::Regular,
                        c.text_dim,
                    );
                }
            }
        }
    }

    /// Whether the strip's shared scrollbar is mid-drag (for pointer capture).
    pub fn hourly_scrollbar_dragging(&self) -> bool {
        self.hourly_bar.is_dragging()
    }

    fn hourly_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(
            self.hourly_content_w,
            self.hourly_viewport.width(),
            self.hourly_scroll_x,
        )
    }

    /// Handle an event routed to the detail pane. Returns true if consumed.
    pub fn handle_detail_event(&mut self, event: &WindowEvent, scale_factor: f64) -> bool {
        use winit::event::{ElementState, MouseButton, MouseScrollDelta};

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
                // A live scrollbar drag tracks the cursor.
                if let Some(off) = self.hourly_bar.drag_to(
                    self.hourly_track,
                    self.hourly_metrics(),
                    self.last_mouse,
                ) {
                    self.hourly_scroll_x = off;
                    return true;
                }
                self.hourly_bar.update_hover(
                    self.hourly_track,
                    self.hourly_metrics(),
                    self.last_mouse,
                );
                false
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        (f64::from(*x) * 48.0, f64::from(*y) * 48.0)
                    }
                    MouseScrollDelta::PixelDelta(p) => (p.x / scale_factor, p.y / scale_factor),
                };
                // Over the strip: scroll it horizontally. A horizontal delta
                // maps directly; a vertical wheel nudges it too, falling
                // through to the page scroll once it reaches an end.
                if self.hourly_viewport.contains(self.last_mouse) && self.hourly_max_scroll > 0.0 {
                    let want = if dx.abs() > f64::EPSILON { -dx } else { -dy };
                    let before = self.hourly_scroll_x;
                    self.hourly_scroll_x =
                        (self.hourly_scroll_x + want).clamp(0.0, self.hourly_max_scroll);
                    if (self.hourly_scroll_x - before).abs() > f64::EPSILON {
                        return true;
                    }
                }
                if self.detail_rect.contains(self.last_mouse) && self.max_scroll > 0.0 {
                    self.scroll = (self.scroll - dy).clamp(0.0, self.max_scroll);
                    return true;
                }
                false
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // The strip's scrollbar wins the press when it overflows.
                if self.hourly_max_scroll > 0.0 {
                    match self.hourly_bar.press(
                        self.hourly_track,
                        self.hourly_metrics(),
                        self.last_mouse,
                    ) {
                        PressOutcome::Thumb => return true,
                        PressOutcome::Page(off) => {
                            self.hourly_scroll_x = off;
                            return true;
                        }
                        PressOutcome::Miss => {}
                    }
                }
                if self.refresh_btn.contains(self.last_mouse) {
                    self.refresh_selected();
                    true
                } else {
                    false
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.hourly_bar.release(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                self.handle_detail_key(&event.logical_key)
            }
            _ => false,
        }
    }

    // ---- test / scenario seams --------------------------------------

    /// Stage a live mid-drag reorder of the second saved place for a headless
    /// shot: the chip floats free as a ghost under the cursor while the rest
    /// of the list has reflowed to open a gap near the tail where it will
    /// land. Insertion is finalised by `sync_insertion` against real geometry
    /// during the shot's draw pass.
    fn handle_detail_key(&mut self, key: &winit::keyboard::Key) -> bool {
        use winit::keyboard::{Key, NamedKey};

        let page = (self.hourly_viewport.width() * 0.8).max(80.0);
        match key {
            Key::Named(NamedKey::ArrowLeft) if self.hourly_max_scroll > 0.0 => {
                self.hourly_scroll_x = (self.hourly_scroll_x - page).max(0.0);
                true
            }
            Key::Named(NamedKey::ArrowRight) if self.hourly_max_scroll > 0.0 => {
                self.hourly_scroll_x = (self.hourly_scroll_x + page).min(self.hourly_max_scroll);
                true
            }
            Key::Named(NamedKey::Home) if self.hourly_max_scroll > 0.0 => {
                self.hourly_scroll_x = 0.0;
                true
            }
            Key::Named(NamedKey::End) if self.hourly_max_scroll > 0.0 => {
                self.hourly_scroll_x = self.hourly_max_scroll;
                true
            }
            Key::Character(text) if text.as_str().eq_ignore_ascii_case("r") => {
                self.refresh_selected();
                true
            }
            _ => false,
        }
    }

    pub fn detail_tooltip(&self) -> Option<&'static str> {
        if self.refresh_btn.contains(self.last_mouse) {
            Some("Refresh forecast (R)")
        } else if self.hourly_viewport.contains(self.last_mouse) && self.hourly_max_scroll > 0.0 {
            Some("Hourly forecast — use Left and Right Arrow to scroll")
        } else {
            None
        }
    }

    pub fn seed_drag(&mut self) {
        if self.model.locations.is_empty() {
            return;
        }
        let from = 1.min(self.model.locations.len() - 1);
        let Some(id) = self.model.locations.get(from).map(|loc| loc.id) else {
            return;
        };
        let start = Point::new(200.0, 232.0);
        let chip_m = self.chip_metrics();
        let target = Point::new(200.0, start.y + 2.4 * chip_m.pitch());
        // Grab the chip at its vertical centre so the ghost sits on the cursor.
        self.reorder.begin(id, from, CHIP_H / 2.0, start);
        self.last_mouse = target;
        // A rough metrics is enough to clear the drag-start threshold; the draw
        // pass re-pins the insertion index against the real list geometry.
        let m = ListMetrics::uniform(0.0, chip_m.chip_h, chip_m.gap, self.model.locations.len());
        self.reorder.pointer_moved(target, &m);
        // Keep the classifier in the reorder state so the delete drop-zone and
        // list-bottom inset match a real in-flight drag.
        let mut press = PointerPress::new(from, start);
        press.update(target);
        self.press = Some(press);
    }

    /// Park the forecast scrolled to the bottom for a short-window shot.
    pub fn seed_scrolled(&mut self) {
        self.scroll = f64::MAX;
    }

    /// Stage the search field + suggestion overlay with a query and a set of
    /// geocoded matches (headless shot / test — the offscreen path can't await
    /// a live geocode). Drives the real shared [`Autocomplete`].
    pub fn seed_search(&mut self, query: &str, results: Vec<SavedLocation>) {
        self.search.begin_edit();
        self.search.set_text(query); // after begin_edit: caret at end, no select
        self.search_query = query.to_string();
        self.search_results = results;
        self.geocode_inflight = false;
        self.pending_query_at = None;
        self.sync_search_view();
    }

    /// Like [`seed_search`](Self::seed_search) but with the top match already
    /// highlighted — for a headless shot of the overlay mid-navigation.
    pub fn seed_search_highlighted(&mut self, query: &str, results: Vec<SavedLocation>) {
        self.seed_search(query, results);
        self.search.set_highlight(Some(0));
    }

    /// Stage the overlay's loading state (a geocode in flight).
    pub fn seed_search_loading(&mut self, query: &str) {
        self.search.begin_edit();
        self.search.set_text(query);
        self.search_query = query.to_string();
        self.search_results.clear();
        self.geocode_inflight = true;
        self.pending_query_at = None;
        self.sync_search_view();
    }

    /// Stage the overlay's "no matches" state.
    pub fn seed_search_empty(&mut self, query: &str) {
        self.search.begin_edit();
        self.search.set_text(query);
        self.search_query = query.to_string();
        self.search_results.clear();
        self.geocode_inflight = false;
        self.pending_query_at = None;
        self.sync_search_view();
    }

    #[cfg(test)]
    pub fn over_list_top(&self) -> f64 {
        self.over_list_top
    }
    #[cfg(test)]
    pub fn max_scroll(&self) -> f64 {
        self.max_scroll
    }
    #[cfg(test)]
    pub fn scroll(&self) -> f64 {
        self.scroll
    }
    #[cfg(test)]
    pub fn over_rect(&self) -> Rect {
        self.over_rect
    }
    #[cfg(test)]
    pub fn menu_is_open(&self) -> bool {
        self.menu.is_some()
    }
    /// The live hotspot rect the production hit-test would match for a given
    /// context-menu action, so tests click the *real* target rather than a
    /// recomputed guess. Requires a `draw_overview` pass with the menu open.
    #[cfg(test)]
    pub fn menu_item_rect(&self, action: EntryAction) -> Option<Rect> {
        self.over_hotspots
            .iter()
            .rev()
            .find_map(|h| match &h.action {
                OverAction::MenuItem(_, a) if *a == action => Some(h.rect),
                _ => None,
            })
    }
    #[cfg(test)]
    pub fn search_is_focused(&self) -> bool {
        self.search.is_editing()
    }
    #[cfg(test)]
    pub fn search_query(&self) -> &str {
        self.search.text()
    }
    /// Test seam: the number of suggestion rows the shared overlay currently
    /// offers (proves the autocomplete is populated from real geocode data).
    #[cfg(test)]
    pub fn search_suggestion_count(&self) -> usize {
        self.search.suggestions().len()
    }
    /// Test seam: the keyboard-highlighted suggestion's display label, if any.
    #[cfg(test)]
    pub fn search_highlight_label(&self) -> Option<String> {
        self.search.highlighted().map(|s| s.label.clone())
    }
    #[cfg(test)]
    pub fn hourly_max_scroll(&self) -> f64 {
        self.hourly_max_scroll
    }
    #[cfg(test)]
    pub fn hourly_scroll_x(&self) -> f64 {
        self.hourly_scroll_x
    }
    #[cfg(test)]
    pub fn hourly_viewport_rect(&self) -> Rect {
        self.hourly_viewport
    }
    #[cfg(test)]
    pub fn delete_zone_rect_test(&self) -> Rect {
        self.delete_zone_rect()
    }
    #[cfg(test)]
    pub fn refresh_btn_rect(&self) -> Rect {
        self.refresh_btn
    }
    /// Test seam: the live insertion boundary the reorder controller tracks
    /// this frame, or `None` when no reorder is in flight.
    #[cfg(test)]
    pub fn reorder_insertion_for_test(&self) -> Option<usize> {
        self.reorder.insertion()
    }
    /// Test seam: index of the chip currently being dragged, if any.
    #[cfg(test)]
    pub fn reorder_from_for_test(&self) -> Option<usize> {
        self.reorder.from_index()
    }
    /// Test seam: the reflow shift (logical px) the controller applies to the
    /// non-dragged chip at `i` while a drag is live.
    #[cfg(test)]
    pub fn reorder_item_shift_for_test(&self, i: usize) -> f64 {
        self.reorder.item_shift(i, &self.list_metrics())
    }
}

// =====================================================================
// Free drawing helpers
// =====================================================================

/// Precomputed section geometry for [`Core::draw_detail`]'s scrollable body,
/// measured once per frame so every section (and the scroll geometry) agree
/// on where it starts.
///
/// Every card, tile and row is its authored 1× height grown by exactly the
/// extra height the text lines inside it need at the renderer's text scale
/// (see [`authored_h`]), and the inner text offsets shift by the growth of
/// the lines above them, so at 1× the design is reproduced exactly and at 2×
/// no line runs into its neighbour or out of its card.
/// What one detail-body pass paints: the pane, the selected city's
/// forecast, and the body layout solved for it.
#[derive(Clone, Copy)]
struct DetailBody<'a> {
    rect: Rect,
    snap: &'a WeatherSnapshot,
    layout: &'a DetailBodyLayout,
}

struct DetailBodyLayout {
    /// Gap between sections and between tiles — the theme's `s3`.
    gap: f64,
    /// Base text sizes the section painters set their lines in.
    text: BodyText,
    /// Hero card: height, and the tops of the condition and "feels like"
    /// lines beneath the big temperature.
    hero_h: f64,
    hero_cond_dy: f64,
    hero_feels_dy: f64,
    /// Metric tiles: height, the value line's top, and the inset of the unit
    /// line from the tile bottom.
    tile_cols: usize,
    tile_h: f64,
    tile_value_dy: f64,
    tile_sub_inset: f64,
    tiles_block_h: f64,
    /// Hourly strip card: height and the chart band's top inset.
    hourly_h: f64,
    hourly_band_top: f64,
    /// Daily card: title row height, one day row, and the whole card.
    days_header_h: f64,
    day_row_h: f64,
    days_block_h: f64,
    /// Air-quality card: title row, AQI hero, pollutant tiles and the whole card.
    aq_cols: usize,
    aq_header_h: f64,
    aq_hero_h: f64,
    aq_tile_h: f64,
    aq_tile_value_dy: f64,
    aq_tile_sub_inset: f64,
    aq_block_h: f64,
    body_h: f64,
}

/// Base text sizes of the lines the body painters set, carried on the layout
/// so a painter and the reservation that holds its line read one source.
#[derive(Clone, Copy, Debug)]
struct BodyText {
    hero_temp: f32,
    hero_cond: f32,
    hero_feels: f32,
    tile_label: f32,
    tile_value: f32,
    tile_unit: f32,
    aq_tile_label: f32,
    aq_tile_value: f32,
}

/// The authored 1× design of the detail body: card heights, the inner offsets
/// of their stacked lines, and the base sizes of the lines that sit off the
/// type scale. One table, grown by [`DetailBodyLayout::measure`] and read by
/// the section painters through the layout, so every number the body is drawn
/// from lives here once. Values a theme token names — the card title rows
/// (`metrics.bar_height`), section and tile gaps (`s3`), the hourly band inset
/// (`s8`), the AQI hero (`s16`), `xs`/`sm`/`base` text — are read from the
/// theme in `measure`, not restated here.
struct BodyDesign {
    hero_h: f64,
    hero_temp_text: f32,
    hero_cond_text: f32,
    hero_feels_text: f32,
    /// Tops of the condition and "feels like" lines under the temperature.
    hero_cond_dy: f64,
    hero_feels_dy: f64,
    tile_h: f64,
    tile_value_text: f32,
    tile_unit_text: f32,
    /// Top of the value line; inset of the unit line from the tile bottom.
    tile_value_dy: f64,
    tile_unit_inset: f64,
    hourly_h: f64,
    day_row_h: f64,
    day_label_text: f32,
    aq_band_text: f32,
    aq_tile_h: f64,
    aq_tile_value_text: f32,
    aq_tile_value_dy: f64,
    aq_tile_unit_inset: f64,
    /// Bottom pad of the air-quality card, under its tiles.
    aq_bottom_pad: f64,
}

const BODY: BodyDesign = BodyDesign {
    hero_h: 168.0,
    hero_temp_text: 72.0,
    hero_cond_text: 19.0,
    hero_feels_text: 13.0,
    hero_cond_dy: 114.0,
    hero_feels_dy: 140.0,
    tile_h: 78.0,
    tile_value_text: 25.0,
    tile_unit_text: 11.0,
    tile_value_dy: 30.0,
    tile_unit_inset: 22.0,
    hourly_h: 190.0,
    day_row_h: 42.0,
    day_label_text: 13.0,
    aq_band_text: 19.0,
    aq_tile_h: 66.0,
    aq_tile_value_text: 22.0,
    aq_tile_value_dy: 26.0,
    aq_tile_unit_inset: 18.0,
    aq_bottom_pad: 14.0,
};

impl DetailBodyLayout {
    /// Solve the body for `text_scale` with the theme's `spacing` and
    /// `metrics`: token-sized rows (card titles, day rows) are the token, or
    /// the measured line plus the same token pad the painter leaves, rounded
    /// up; design-table cards grow by exactly their lines' extra height, and
    /// their inner offsets shift by the growth of the lines above them.
    #[expect(
        clippy::too_many_lines,
        reason = "solves the whole body layout table in one pass, top to bottom"
    )]
    fn measure(
        mw: f64,
        snap: &WeatherSnapshot,
        spacing: &Spacing,
        metrics: &Metrics,
        text_scale: f32,
    ) -> Self {
        let g = |size: f32| line_growth(size, text_scale);
        let (s2, s3) = (f64::from(spacing.s2), f64::from(spacing.s3));
        let gap = s3;
        let d = &BODY;
        let text = BodyText {
            hero_temp: d.hero_temp_text,
            hero_cond: d.hero_cond_text,
            hero_feels: d.hero_feels_text,
            tile_label: text_xs(),
            tile_value: d.tile_value_text,
            tile_unit: d.tile_unit_text,
            aq_tile_label: text_xs(),
            aq_tile_value: d.aq_tile_value_text,
        };

        // Hero: the temperature line, then the condition and "feels like"
        // lines beneath it; each starts where the design put it plus the
        // growth of the lines above.
        let hero_h = authored_h(
            d.hero_h,
            &[text.hero_temp, text.hero_cond, text.hero_feels],
            text_scale,
        );
        let hero_cond_dy = d.hero_cond_dy + g(text.hero_temp);
        let hero_feels_dy = d.hero_feels_dy + g(text.hero_temp) + g(text.hero_cond);

        // Metric tiles: label, value, unit stacked top to bottom.
        let n_tiles = 5usize;
        let large_text = text_scale >= 1.5;
        let tile_cols = if large_text {
            if mw >= 760.0 { 3 } else { 2 }
        } else {
            balanced_tile_columns(mw, n_tiles)
        };
        let tile_rows = n_tiles.div_ceil(tile_cols);
        let tile_h = authored_h(
            d.tile_h,
            &[text.tile_label, text.tile_value, text.tile_unit],
            text_scale,
        );
        let tile_value_dy = d.tile_value_dy + g(text.tile_label);
        let tile_sub_inset = d.tile_unit_inset + g(text.tile_unit);
        let tiles_block_h = tile_rows as f64 * tile_h + (tile_rows as f64 - 1.0) * gap;

        // Hourly strip: the card title (`base`) above a band, an `s8` down,
        // whose chart stacks its rows from measured lines
        // (`chart::strip_rows`); an `s6` under the band holds the shared
        // scrollbar. The authored 190 px at 1×, or that stack once scaled type
        // needs more — so the hour labels never run into the precipitation
        // line.
        let hourly_band_top = f64::from(spacing.s8) + g(text_base());
        let hourly_h = at_least_whole(
            if large_text {
                d.hourly_h.max(286.0)
            } else {
                d.hourly_h
            },
            hourly_band_top + chart::strip_height(spacing, text_scale) + f64::from(spacing.s6),
        );

        // A card's title row is the theme's bar height, or the title line
        // plus its `s3` above and `s2` below, rounded up.
        let card_header_h = at_least_whole(
            f64::from(metrics.bar_height),
            line_h(text_base(), text_scale) + s3 + s2,
        );

        // Daily rows under the card title: the authored 42px, or the day
        // label plus an `s3` above and below.
        let n_days = snap.daily.len().max(1);
        let day_row_h = at_least_whole(
            if large_text {
                d.day_row_h.max(72.0)
            } else {
                d.day_row_h
            },
            line_h(d.day_label_text, text_scale) + 2.0 * s3,
        );
        let days_block_h = card_header_h + n_days as f64 * day_row_h + s3;

        // Air-quality block: header + AQI hero (an `s16` band holding the band
        // word and its caption) + a row (or two) of pollutant tiles. Reserved
        // whether or not a reading is available yet, so the scroll geometry
        // stays stable while it loads.
        let aq_cols = if text_scale >= 1.5 {
            2
        } else if mw >= 560.0 {
            4
        } else {
            2
        };
        let aq_rows = 4usize.div_ceil(aq_cols);
        let aq_hero_h = authored_h(
            f64::from(spacing.s16),
            &[d.aq_band_text, text_sm()],
            text_scale,
        );
        let aq_tile_h = authored_h(
            d.aq_tile_h,
            &[text.aq_tile_label, text.aq_tile_value, text.aq_tile_label],
            text_scale,
        );
        let aq_tile_value_dy = d.aq_tile_value_dy + g(text.aq_tile_label);
        let aq_tile_sub_inset = d.aq_tile_unit_inset + g(text.aq_tile_label);
        let aq_tile_grid_h = aq_rows as f64 * aq_tile_h + (aq_rows as f64 - 1.0) * gap;
        let aq_block_h = card_header_h + aq_hero_h + s3 + aq_tile_grid_h + d.aq_bottom_pad;

        let body_h =
            hero_h + gap + tiles_block_h + gap + hourly_h + gap + days_block_h + gap + aq_block_h;
        Self {
            gap,
            text,
            hero_h,
            hero_cond_dy,
            hero_feels_dy,
            tile_cols,
            tile_h,
            tile_value_dy,
            tile_sub_inset,
            tiles_block_h,
            hourly_h,
            hourly_band_top,
            days_header_h: card_header_h,
            day_row_h,
            days_block_h,
            aq_cols,
            aq_header_h: card_header_h,
            aq_hero_h,
            aq_tile_h,
            aq_tile_value_dy,
            aq_tile_sub_inset,
            aq_block_h,
            body_h,
        }
    }
}

/// Precomputed x-geometry for one row of the 7-day forecast card, shared by
/// every row so they align pixel-for-pixel and agree on which optional
/// columns (condition label, UV/wind extras, precip) fit.
struct DayRowGeometry {
    inner_l: f64,
    inner_r: f64,
    icon_x0: f64,
    icon_x1: f64,
    cond_x: f64,
    bar_x0: f64,
    bar_x1: f64,
    lo_x: f64,
    precip_left: f64,
    extras_x: f64,
    extras_right: f64,
    show_precip: bool,
    show_extras: bool,
    show_cond: bool,
    /// Radius of the precipitation / UV severity dots.
    dot_r: f64,
}

/// The 7-day card's row painter: the shared column geometry, the forecast
/// and units its labels read, and the week's temperature range each day's
/// bar spans.
struct ForecastRows<'a> {
    geo: DayRowGeometry,
    snap: &'a WeatherSnapshot,
    units: Units,
    week: TrendRange,
}

impl ForecastRows<'_> {
    /// One row of the 7-day forecast: day label, condition icon (+ label
    /// when there's room), UV/wind extras (when there's room), precip
    /// probability (when there's room), and the lo/hi temperature bar.
    fn draw_row(&self, ctx: &mut PaintCtx<'_>, i: usize, d: &DayForecast, row: Rect) {
        // The row rect comes from the shared body layout, so its height is
        // the authored 42px grown by the scaled label line; everything in
        // the row centres on that rect, not on a fixed constant.
        let cy = row.center().y;
        self.draw_label(ctx, i, d, row);
        if self.geo.show_extras {
            self.draw_extras(ctx, d, cy);
        }
        self.draw_temps(ctx, d, cy);
    }

    /// The row divider, the "Today"/"Tomorrow"/weekday label, the condition
    /// icon, and (when there's room) the condition text.
    fn draw_label(&self, ctx: &mut PaintCtx<'_>, i: usize, d: &DayForecast, row: Rect) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let geo = &self.geo;
        let (ry, cy) = (row.y0, row.center().y);
        let c = &theme.tokens.colors;
        if i > 0 {
            // Row rules derive against the card they sit on.
            ui::surface::separator(
                scene,
                t,
                geo.inner_l,
                geo.inner_r,
                ry + 0.5,
                0.0,
                c.surfaces.raised,
            );
        }
        let day_label = match i {
            0 => "Today".to_string(),
            1 => "Tomorrow".to_string(),
            _ => weekday(&d.date),
        };
        text_left(
            scene,
            tr,
            &day_label,
            geo.inner_l,
            cy - 8.0,
            14.0,
            TextWeight::Medium,
            c.text_primary,
        );
        let dsky = Sky::from_code(d.weather_code);
        icon::draw_icon(
            scene,
            t,
            dsky,
            true,
            Rect::new(geo.icon_x0, cy - 14.0, geo.icon_x1, cy + 14.0),
            theme,
        );
        if geo.show_cond {
            text_left(
                scene,
                tr,
                Sky::label(d.weather_code),
                geo.cond_x,
                cy - 7.0,
                13.0,
                TextWeight::Regular,
                c.text_dim,
            );
        }
    }

    /// The UV-index and wind-max extras column, shown only when the row is
    /// wide enough to fit them without crowding the condition text.
    fn draw_extras(&self, ctx: &mut PaintCtx<'_>, d: &DayForecast, cy: f64) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let (geo, snap, units) = (&self.geo, self.snap, self.units);
        let c = &theme.tokens.colors;
        let (uv_label, wind_label) = day_extra_labels(d, snap, units);
        // UV dot tinted by severity so the number reads at a glance.
        let uv_dot = if d.uv_index_max < 3.0 {
            c.semantic.info
        } else if d.uv_index_max < 8.0 {
            c.semantic.warning
        } else {
            c.semantic.danger
        };
        scene.fill(
            Fill::NonZero,
            t,
            uv_dot,
            None,
            &Circle::new(Point::new(geo.extras_x + geo.dot_r + 1.0, cy), geo.dot_r),
        );
        text_left(
            scene,
            tr,
            &uv_label,
            geo.extras_x + 12.0,
            cy - 7.0,
            12.0,
            TextWeight::Medium,
            c.text_dim,
        );
        text_right(
            scene,
            tr,
            &wind_label,
            geo.extras_right,
            cy - 7.0,
            12.0,
            TextWeight::Regular,
            c.text_dim,
        );
    }

    /// The precip-probability read-out (when there's room) and the lo/hi
    /// temperature bar.
    fn draw_temps(&self, ctx: &mut PaintCtx<'_>, d: &DayForecast, cy: f64) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let (geo, snap, units) = (&self.geo, self.snap, self.units);
        let c = &theme.tokens.colors;
        if geo.show_precip {
            scene.fill(
                Fill::NonZero,
                t,
                c.semantic.info,
                None,
                &Circle::new(Point::new(geo.precip_left, cy), geo.dot_r),
            );
            text_left(
                scene,
                tr,
                &format!("{}%", d.precip_probability.round() as i64),
                geo.precip_left + 8.0,
                cy - 7.0,
                12.0,
                TextWeight::Medium,
                c.semantic.info,
            );
        }
        let lo = snap.temp_in(d.temp_min, units).round() as i64;
        let hi = snap.temp_in(d.temp_max, units).round() as i64;
        text_right(
            scene,
            tr,
            &format!("{lo}°"),
            geo.lo_x,
            cy - 8.0,
            14.0,
            TextWeight::Medium,
            c.text_dim,
        );
        // The day's low–high on the week's range. The track runs at track
        // strength, so an empty stretch of range still reads as a bar on both
        // themes; one cool→warm wash spans the whole track and each day shows
        // its slice of it.
        let range = self.week;
        let grad = Gradient::new_linear(Point::new(geo.bar_x0, cy), Point::new(geo.bar_x1, cy))
            .with_stops([(0.0_f32, c.semantic.info), (1.0_f32, c.semantic.warning)]);
        BarGauge::draw_span(
            scene,
            t,
            Rect::new(geo.bar_x0, cy - 3.0, geo.bar_x1, cy + 3.0),
            theme,
            (
                range.fraction(d.temp_min) as f32,
                range.fraction(d.temp_max) as f32,
            ),
            &Brush::Gradient(grad),
            &GaugeStyle::default().with_track_alpha(TRACK_ALPHA),
        );
        text_left(
            scene,
            tr,
            &format!("{hi}°"),
            geo.bar_x1 + f64::from(theme.tokens.spacing.s2),
            cy - 8.0,
            14.0,
            TextWeight::Semibold,
            c.text_primary,
        );
    }
}

impl DayRowGeometry {
    /// One column grid for every row, laid out from the card's `s4` gutters
    /// with the theme's steps between neighbours: `s3` between siblings
    /// (label column, glyph, extras, precip, temperatures), `s2` between a
    /// value and its own bar. `temp_w` is the measured width of the widest
    /// lo / hi label painted this frame. Optional columns show only when they
    /// keep that gap — the precip cluster never lands on the glyph (HIG §7).
    fn new(dcard: Rect, spacing: &Spacing, temp_w: f64) -> Self {
        let (s2, s3, s4) = (
            f64::from(spacing.s2),
            f64::from(spacing.s3),
            f64::from(spacing.s4),
        );
        // The row's authored column widths: the widest day label
        // ("Tomorrow", 14pt Medium), the precipitation cluster (dot, gap,
        // "63%") and the radius of its severity dot — dimensions of the
        // row's own content, not offsets; the gaps between them are the
        // theme's steps.
        let (label_w, precip_w, dot_r) = (72.0, 46.0, 3.0);
        let (icon_edge, extras_w, cond_min_w) = (28.0, 132.0, 96.0);
        let inner_l = dcard.x0 + s4;
        let inner_r = dcard.x1 - s4;
        let dw = inner_r - inner_l;
        let hi_w = s2 + temp_w;
        let bar_w = (dw * 0.22).clamp(56.0, 120.0);
        let bar_x1 = inner_r - hi_w;
        let bar_x0 = bar_x1 - bar_w;
        let lo_x = bar_x0 - s2;
        let precip_right = lo_x - temp_w - s3;
        let precip_left = precip_right - precip_w;
        let icon_x0 = inner_l + label_w + s3;
        let icon_x1 = icon_x0 + icon_edge;
        let cond_x = icon_x1 + s2;
        let show_precip = dw >= 300.0 && precip_left - dot_r >= icon_x1 + s3;
        // Per-day extras (UV-index max + wind max) get a fixed-width column to
        // the left of the precip/lo cluster, shown only when the row is wide
        // enough to fit them without crowding the condition text.
        let extras_right = if show_precip {
            precip_left - dot_r - s3
        } else {
            lo_x - temp_w - s3
        };
        let extras_x = extras_right - extras_w;
        let show_extras = (extras_x - cond_x) >= cond_min_w + s3 + 10.0;
        let cond_limit = if show_extras {
            extras_x - s3
        } else if show_precip {
            precip_left - dot_r - s3
        } else {
            lo_x - temp_w - s3
        };
        let show_cond = (cond_limit - cond_x) >= cond_min_w;
        Self {
            inner_l,
            inner_r,
            icon_x0,
            icon_x1,
            cond_x,
            bar_x0,
            bar_x1,
            lo_x,
            precip_left,
            extras_x,
            extras_right,
            show_precip,
            show_extras,
            show_cond,
            dot_r,
        }
    }
}

fn tile_columns(mw: f64) -> usize {
    let gap = f64::from(Spacing::BASE.s3);
    (((mw + gap) / (TILE_MIN_W + gap)).floor() as usize).clamp(2, 5)
}

/// Column count for `n` metric tiles at the given content width, adjusted to
/// avoid a single orphaned tile on the last row. For the five weather tiles a
/// raw fit of four columns (4 + 1) is dropped to three (a balanced 3 + 2).
fn balanced_tile_columns(mw: f64, n: usize) -> usize {
    let cols = tile_columns(mw);
    if cols > 2 && n % cols == 1 {
        cols - 1
    } else {
        cols
    }
}

/// A raised card of the detail body with its rest edge *derived* from its
/// own surface (HIG §9.5) — a declared `border_standard` at 70 % was three
/// shades off dark `raised` and read as no edge at all.
fn card(scene: &mut Scene, t: Affine, rect: Rect, theme: &Theme) {
    ui::surface::card(
        scene,
        t,
        rect,
        f64::from(theme.tokens.radii.lg),
        theme.tokens.colors.surfaces.raised,
    );
}

/// The hover wash of a chip or row on `ground`: a hairline-strength lift
/// derived against that ground, white on a dark panel and black on a light
/// one, instead of a ramp slot that only reads on one side.
pub(crate) fn hover_wash(ground: Color) -> Color {
    ui::tokens::colors::Colors::hairline_on(ground, ui::chrome::HAIRLINE_ALPHA)
}

/// Alpha of a track's fill: two steps up `ui::surface::EDGE_ALPHA_STEPS` from
/// the hover hairline, so an empty stretch of range or scrollbar reads as a
/// bar on both themes rather than a wash.
const TRACK_ALPHA: u8 = 56;

/// The fill of a range or scrollbar track on `ground`, derived against it
/// (white over a dark card, black over a light one — `glass_film` lifts
/// toward white on a paper palette and vanished on the white card).
fn track_fill(ground: Color) -> Color {
    ui::tokens::colors::Colors::hairline_on(ground, TRACK_ALPHA)
}

/// The saved-place column inside the overview panel `rect`: its left edge
/// and width.
fn overview_column(rect: Rect) -> (f64, f64) {
    (rect.x0 + RP, (rect.width() - 2.0 * RP).max(80.0))
}

pub(crate) fn text_left(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    s: &str,
    x: f64,
    y_top: f64,
    size: f32,
    weight: TextWeight,
    color: Color,
) {
    tr.draw_text_weighted(
        scene,
        s,
        size,
        color,
        Affine::translate((x, y_top)),
        None,
        weight,
    );
}

pub(crate) fn text_right(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    s: &str,
    x_right: f64,
    y_top: f64,
    size: f32,
    weight: TextWeight,
    color: Color,
) {
    let (w, _) = tr.measure_weighted(s, size, None, weight);
    tr.draw_text_weighted(
        scene,
        s,
        size,
        color,
        Affine::translate((x_right - f64::from(w), y_top)),
        None,
        weight,
    );
}

fn text_center(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    s: &str,
    x_center: f64,
    y_top: f64,
    size: f32,
    weight: TextWeight,
    color: Color,
) {
    let (w, _) = tr.measure_weighted(s, size, None, weight);
    tr.draw_text_weighted(
        scene,
        s,
        size,
        color,
        Affine::translate((x_center - f64::from(w) / 2.0, y_top)),
        None,
        weight,
    );
}

pub(crate) fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let a = a.to_rgba8();
    let b = b.to_rgba8();
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t) as u8;
    Color::from_rgba8(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b), mix(a.a, b.a))
}

fn hhmm(iso: &str) -> String {
    iso.get(11..16).unwrap_or("--:--").to_string()
}

/// Parse an "HH:MM" clock string into minutes since local midnight.
fn hhmm_to_minutes(s: &str) -> Option<i64> {
    let (h, m) = s.split_once(':')?;
    let h: i64 = h.trim().parse().ok()?;
    let m: i64 = m.trim().parse().ok()?;
    if !(0..24).contains(&h) || !(0..60).contains(&m) {
        return None;
    }
    Some(h * 60 + m)
}

/// Fraction along the daytime arc for the sun's current position: 0 at
/// sunrise (left horizon), 0.5 at solar noon (apex), 1 at sunset (right
/// horizon). Clamps to the nearest horizon before sunrise / after sunset.
/// Returns `None` when any clock string is unparseable or the day has no
/// length, so the caller can fall back rather than divide by zero.
fn sun_arc_fraction(now_hhmm: &str, sunrise_hhmm: &str, sunset_hhmm: &str) -> Option<f64> {
    let now = hhmm_to_minutes(now_hhmm)?;
    let sr = hhmm_to_minutes(sunrise_hhmm)?;
    let ss = hhmm_to_minutes(sunset_hhmm)?;
    if ss <= sr {
        return None;
    }
    Some(((now - sr) as f64 / (ss - sr) as f64).clamp(0.0, 1.0))
}

/// The per-day "extras" labels shown at the right of a wide 7-day row: the
/// UV-index max and the wind max converted to the display units. Kept pure so
/// the exact strings the row renders are unit-testable.
fn day_extra_labels(d: &DayForecast, snap: &WeatherSnapshot, units: Units) -> (String, String) {
    let uv = d.uv_index_max.round().max(0.0) as i64;
    let wind = snap.wind_in(d.wind_max, units).round() as i64;
    (
        format!("UV {uv}"),
        format!("{wind} {}", units.wind_suffix()),
    )
}

fn weekday(date: &str) -> String {
    use chrono::NaiveDate;
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_or_else(|_| date.to_string(), |d| d.format("%a").to_string())
}

fn fmt_date(iso: &str) -> String {
    use chrono::NaiveDate;
    let date_part = iso.get(0..10).unwrap_or(iso);
    NaiveDate::parse_from_str(date_part, "%Y-%m-%d").map_or_else(
        |_| date_part.to_string(),
        |d| d.format("%a, %b %d").to_string(),
    )
}

fn uv_word(uv: f64) -> &'static str {
    match uv.round() as i64 {
        0..=2 => "Low",
        3..=5 => "Moderate",
        6..=7 => "High",
        8..=10 => "Very high",
        _ => "Extreme",
    }
}

/// Severity tint for a European AQI value, drawn from the theme's semantic
/// palette so it tracks light/dark and high-contrast.
fn aqi_band_color(aqi: f64, theme: &Theme) -> Color {
    let c = &theme.tokens.colors;
    match aqi.round() as i64 {
        i64::MIN..=20 => c.semantic.success,
        21..=40 => c.semantic.info,
        41..=60 => c.semantic.warning,
        _ => c.semantic.danger,
    }
}

/// The encoded, unambiguous value for a geocoded search result — its display
/// name plus subtitle — so the accepted [`Autocomplete`] value maps back to the
/// exact place (two cities named "Paris" stay distinct).
fn result_value(loc: &SavedLocation) -> String {
    format!("{}, {}", loc.name, loc.subtitle())
}

/// The single visible autocomplete label. Keeping location context in the
/// same text run prevents a long city name from colliding with a separate
/// right-aligned detail column in a narrow search overlay.
fn result_label(loc: &SavedLocation) -> String {
    let place = if loc.region.is_empty() || loc.region == loc.name {
        loc.name.clone()
    } else {
        format!("{}, {}", loc.name, loc.region)
    };
    if loc.country_code.is_empty() {
        format!("{} · {place}", loc.country)
    } else {
        format!("{} · {place}", loc.country_code)
    }
}

#[cfg(test)]
mod layout_tests {
    use super::balanced_tile_columns;

    #[test]
    fn five_tiles_never_orphan_a_single_on_the_last_row() {
        // Across representative content widths the column count never leaves a
        // lonely single tile (rem == 1) when more than two columns fit.
        for mw in [404.0_f64, 504.0, 604.0, 884.0, 1200.0] {
            let cols = balanced_tile_columns(mw, 5);
            if cols > 2 {
                assert_ne!(5 % cols, 1, "mw {mw}: {cols} columns orphans one tile");
            }
        }
        // The specific awkward case: a raw 4-wide fit becomes a balanced 3 + 2.
        assert_eq!(balanced_tile_columns(604.0, 5), 3);
    }
}

#[cfg(test)]
mod day_row_geometry_tests {
    use super::DayRowGeometry;
    use ui::tokens::spacing::Spacing;
    use vello::kurbo::Rect;

    #[test]
    fn day_row_columns_keep_token_gaps_and_clear_the_glyph() {
        // Case `day_row_columns_keep_token_gaps_at_every_width`:
        // Every neighbouring pair in a 7-day row keeps a token-step gap at
        // every card width the app lays out (HIG §7 "nothing touches"): the
        // precip dot clears the condition glyph by `s3`, the extras column
        // clears the precip cluster, the lo label clears its bar by `s2`.
        {
            let sp = Spacing::default();
            let (s2, s3) = (f64::from(sp.s2), f64::from(sp.s3));
            // Two-digit and three-digit / negative readings ("-12°", "104°").
            for (card_w, temp_w) in [
                (372.0_f64, 30.0_f64),
                (440.0, 30.0),
                (520.0, 30.0),
                (604.0, 30.0),
                (700.0, 30.0),
                (884.0, 30.0),
                (1200.0, 30.0),
                (372.0, 38.0),
                (604.0, 38.0),
                (884.0, 42.0),
            ] {
                let geo = DayRowGeometry::new(Rect::new(0.0, 0.0, card_w, 42.0), &sp, temp_w);
                assert!(
                    geo.icon_x0 - geo.inner_l >= s3,
                    "{card_w}: glyph touches the label column"
                );
                assert!(
                    geo.bar_x0 - geo.lo_x >= s2 - 1e-9,
                    "{card_w}: lo label touches its bar"
                );
                assert!(
                    geo.inner_r - (geo.bar_x1 + s2) >= temp_w - 1e-9,
                    "{card_w}: a {temp_w}px hi label runs past the card gutter"
                );
                if geo.show_precip {
                    assert!(
                        geo.precip_left - geo.dot_r - geo.icon_x1 >= s3 - 1e-9,
                        "{card_w}: precip dot {} lands on the glyph ending at {}",
                        geo.precip_left,
                        geo.icon_x1
                    );
                }
                if geo.show_extras {
                    let next = if geo.show_precip {
                        geo.precip_left - geo.dot_r
                    } else {
                        geo.lo_x - temp_w
                    };
                    assert!(
                        next - geo.extras_right >= s3 - 1e-9,
                        "{card_w}: extras run into the next column"
                    );
                    assert!(geo.extras_x - geo.cond_x >= s3);
                }
            }
        }
        // Case `tablet_width_keeps_precipitation_clear_of_the_glyph`:
        // The tablet-width card (768 px window) keeps the precipitation column
        // instead of dropping it, and keeps it clear of the glyph.
        {
            let sp = Spacing::default();
            let geo = DayRowGeometry::new(Rect::new(0.0, 0.0, 372.0, 42.0), &sp, 30.0);
            assert!(geo.show_precip);
            assert!(geo.precip_left - geo.dot_r - geo.icon_x1 >= f64::from(sp.s3));
        }
    }
}

#[cfg(test)]
mod chip_metrics_tests {
    use super::{
        BODY, CHIP_H, CHIP_ICON, ChipMetrics, DETAIL_HEADER_H, DETAIL_SUBTITLE_TEXT,
        DETAIL_TITLE_TEXT, DetailBodyLayout, detail_header_h, line_h, text_base,
    };
    use ui::tokens::Metrics;
    use ui::tokens::spacing::Spacing;

    #[test]
    fn rows_and_detail_are_authored_at_1x_and_scale_to_whole_pixels() {
        // Case `one_x_rows_keep_the_authored_chip`.
        #[expect(
            clippy::float_cmp,
            reason = "1x chips must equal the authored constants exactly"
        )]
        {
            let sp = Spacing::default();
            let m = ChipMetrics::new(&sp, 1.0);
            assert_eq!(m.chip_h, CHIP_H);
            assert_eq!(m.icon, CHIP_ICON);
            assert_eq!(m.pitch(), CHIP_H + f64::from(sp.s2));
        }
        // Case `scaled_rows_land_on_whole_pixels`.
        #[expect(
            clippy::float_cmp,
            reason = "whole-pixel snapping is exact by construction (`ceil`)"
        )]
        {
            let sp = Spacing::default();
            let m = ChipMetrics::new(&sp, 2.0);
            assert_eq!(m.chip_h.fract(), 0.0);
            // Both scaled lines plus the `s3` the painter leaves above and below.
            assert_eq!(
                m.chip_h,
                (m.name_h + m.sub_h + 2.0 * f64::from(sp.s3)).ceil()
            );
        }
        // Case `detail_header_is_authored_at_one_x`:
        // The detail header is the authored 74px at 1× — not the 76.75 that
        // stacking measured lines plus token gaps produced — and a whole pixel
        // that holds both scaled lines and their gaps at 2×.
        #[expect(
            clippy::float_cmp,
            reason = "1x header must equal the authored constant and 2x must be a whole pixel"
        )]
        {
            let sp = Spacing::default();
            assert_eq!(detail_header_h(&sp, 1.0), DETAIL_HEADER_H);
            let two = detail_header_h(&sp, 2.0);
            assert_eq!(two.fract(), 0.0);
            assert!(
                two >= line_h(DETAIL_TITLE_TEXT, 2.0)
                    + f64::from(sp.s1)
                    + line_h(DETAIL_SUBTITLE_TEXT, 2.0)
                    + f64::from(sp.s4)
            );
        }
        // Case `detail_body_layout_is_authored_at_one_x_and_grows_whole`:
        // The scrollable body reproduces every authored section height at 1×
        // and, at 2×, grows each card on whole pixels with the inner stacks
        // still inside their cards; token-sized rows hold their line plus the
        // same theme pad the painter leaves.
        #[expect(
            clippy::float_cmp,
            reason = "1x sections must equal the authored heights and 2x ones whole pixels exactly"
        )]
        {
            let model = crate::sample::sample_model();
            let snap = model
                .selected_snapshot()
                .cloned()
                .expect("the sample model selects a place with a forecast");
            let (sp, mx) = (Spacing::default(), Metrics::BASE);
            let (s2, s3) = (f64::from(sp.s2), f64::from(sp.s3));
            let one = DetailBodyLayout::measure(800.0, &snap, &sp, &mx, 1.0);
            assert_eq!(one.hero_h, BODY.hero_h);
            assert_eq!(one.tile_h, BODY.tile_h);
            assert_eq!(one.hourly_h, BODY.hourly_h);
            assert_eq!(one.day_row_h, BODY.day_row_h);
            assert_eq!(one.days_header_h, f64::from(mx.bar_height));
            assert_eq!(one.aq_header_h, f64::from(mx.bar_height));
            assert_eq!(one.aq_hero_h, f64::from(sp.s16));
            assert_eq!(one.aq_tile_h, BODY.aq_tile_h);
            assert_eq!(one.hero_cond_dy, BODY.hero_cond_dy);
            assert_eq!(one.hero_feels_dy, BODY.hero_feels_dy);
            assert_eq!(one.tile_value_dy, BODY.tile_value_dy);
            assert_eq!(one.hourly_band_top, f64::from(sp.s8));
            assert_eq!(one.gap, s3);

            let two = DetailBodyLayout::measure(800.0, &snap, &sp, &mx, 2.0);
            assert!(two.days_header_h >= line_h(text_base(), 2.0) + s3 + s2);
            assert!(two.day_row_h >= line_h(BODY.day_label_text, 2.0) + 2.0 * s3);
            for value in [
                two.hero_h,
                two.tile_h,
                two.hourly_h,
                two.day_row_h,
                two.days_header_h,
                two.aq_header_h,
                two.aq_hero_h,
                two.aq_tile_h,
            ] {
                assert_eq!(value.fract(), 0.0, "{value} is not a whole pixel");
            }
            assert!(two.hero_h > one.hero_h && two.tile_h > one.tile_h);
            assert!(two.hourly_h > one.hourly_h && two.day_row_h > one.day_row_h);
            assert!(two.aq_tile_h > one.aq_tile_h && two.aq_hero_h > one.aq_hero_h);
            assert!(two.body_h > one.body_h);
            // The stacked hero lines keep their authored relation and the last
            // one still ends inside the card.
            assert!(two.hero_cond_dy > one.hero_cond_dy);
            assert!(two.hero_feels_dy > two.hero_cond_dy);
            assert!(two.hero_feels_dy + line_h(BODY.hero_feels_text, 2.0) <= two.hero_h);
            assert!(two.hourly_band_top > one.hourly_band_top);
            assert!(two.tile_value_dy > one.tile_value_dy);
        }
        // Case `scaled_rows_hold_the_name_and_condition_lines`.
        {
            let sp = Spacing::default();
            let m = ChipMetrics::new(&sp, 2.0);
            // Both text lines plus the row padding must fit, and the glyph must
            // not outgrow the text block it sits beside.
            assert!(m.chip_h >= m.name_h + m.sub_h + 2.0 * f64::from(sp.s3));
            assert!(m.chip_h > CHIP_H);
            assert!(m.icon >= CHIP_ICON && m.icon <= m.chip_h);
            assert!(m.temp_h <= m.chip_h);
        }
    }
}

#[cfg(test)]
mod helper_tests {
    use super::{Core, day_extra_labels, hhmm_to_minutes, sun_arc_fraction};
    use crate::net::{RequestKind, Response};
    use crate::units::Units;

    #[test]
    fn sun_arc_and_day_extras_map_real_values() {
        // Case `sun_arc_fraction_maps_time_to_arc_position`.
        {
            // The dot the hero renders is driven by this: sunrise → 0 (left
            // horizon), solar noon → 0.5 (apex), sunset → 1 (right horizon).
            assert_eq!(sun_arc_fraction("06:00", "06:00", "18:00"), Some(0.0));
            assert_eq!(sun_arc_fraction("12:00", "06:00", "18:00"), Some(0.5));
            assert_eq!(sun_arc_fraction("18:00", "06:00", "18:00"), Some(1.0));
            assert_eq!(sun_arc_fraction("09:00", "06:00", "18:00"), Some(0.25));
        }
        // Case `sun_arc_fraction_clamps_outside_daylight`.
        {
            // Before sunrise pins to the left horizon, after sunset to the right.
            assert_eq!(sun_arc_fraction("03:00", "06:00", "18:00"), Some(0.0));
            assert_eq!(sun_arc_fraction("23:30", "06:00", "18:00"), Some(1.0));
        }
        // Case `sun_arc_fraction_rejects_unusable_input`.
        {
            assert_eq!(sun_arc_fraction("--:--", "06:00", "18:00"), None);
            assert_eq!(
                sun_arc_fraction("12:00", "18:00", "06:00"),
                None,
                "sunset before sunrise"
            );
            assert_eq!(
                sun_arc_fraction("12:00", "12:00", "12:00"),
                None,
                "zero-length day"
            );
            assert_eq!(hhmm_to_minutes("25:00"), None);
            assert_eq!(hhmm_to_minutes("07:30"), Some(450));
        }
        // Case `day_extra_labels_render_real_uv_and_wind_in_display_units`.
        {
            // The values the 7-day rows now surface come straight from the parsed
            // per-day fields (previously stored but never rendered).
            let m = crate::sample::sample_model();
            let snap = m.selected_snapshot().unwrap(); // San Francisco, metric source
            let day0 = &snap.daily[0];
            assert!(day0.wind_max > 0.0 && day0.uv_index_max > 0.0);
            let (uv, wind) = day_extra_labels(day0, snap, Units::METRIC);
            assert_eq!(uv, "UV 6");
            assert_eq!(wind, "24 km/h");
            // Imperial converts the wind (24 km/h ≈ 15 mph); the UV index is unitless.
            let (uv_i, wind_i) = day_extra_labels(day0, snap, Units::IMPERIAL);
            assert_eq!(uv_i, "UV 6");
            assert_eq!(wind_i, "15 mph");
        }
    }

    #[test]
    fn core_search_and_accessibility_drive_real_fetches() {
        // Case `geocode_failure_does_not_complete_a_forecast_refresh`.
        {
            let model = crate::sample::sample_model();
            let mut core = Core::new(model, Units::METRIC);
            core.refresh_all();
            let before = core.inflight;
            core.apply_response(Response::Error {
                kind: RequestKind::Geocode,
                message: "network unreachable".into(),
            });
            assert_eq!(
                core.inflight, before,
                "a geocode error must not settle a separate forecast request"
            );
            assert!(
                !core.status.starts_with("Couldn't"),
                "the forecast status stays intact while only search failed"
            );
        }
        // Case `accessible_city_search_selects_a_result_and_queues_its_forecast`.
        {
            use super::*;
            use std::{cell::RefCell, rc::Rc};
            use ui::a11y::Action;
            let mut core = Core::new(WeatherModel::default(), Units::METRIC);
            let results = crate::sample::sample_model().locations;
            let chosen = results[1].clone();
            core.geocode_cache.insert("city".into(), results);
            let core = Rc::new(RefCell::new(core));
            let mut pane = crate::panes::OverviewPane::new(core.clone());
            let mut state = AutoCommit::new();
            let search = pane.accessibility().unwrap().children.remove(0);
            assert!(pane.perform_semantic_action(
                search.action_target.as_deref().unwrap(),
                &Action::SetValue("city".into()),
                &mut state,
                &automerge::ROOT,
            ));
            assert_eq!(core.borrow().search_query, "city");
            assert!(core.borrow().pending_query_at.is_some());
            let deadline = core.borrow().search_deadline().unwrap();
            assert_eq!(core.borrow().search_deadline(), Some(deadline));
            let search = pane.accessibility().unwrap().children.remove(0);
            let row = &search.children[1];
            assert_eq!(row.name.as_deref(), Some(result_label(&chosen).as_str()));
            assert!(pane.perform_semantic_action(
                row.action_target.as_deref().unwrap(),
                &Action::Activate,
                &mut state,
                &automerge::ROOT,
            ));
            let mut core = core.borrow_mut();
            assert_eq!(core.model.selected_location().unwrap().id, chosen.id);
            assert!(
                core.take_outbox()
                    .iter()
                    .any(|request| matches!(request, Request::Forecast { .. }))
            );
            assert!(!core.search.is_editing());
            assert!(core.search_query.is_empty());
            assert!(core.search_deadline().is_none());
        }
        // Case `weather_accessibility_summarises_saved_places_and_forecast`.
        {
            let model = crate::sample::sample_model();
            let core = Core::new(model, Units::METRIC);
            let overview =
                core.overview_accessibility(vello::kurbo::Rect::new(0.0, 0.0, 300.0, 700.0));
            assert_eq!(overview.role, ui::a11y::Role::Group);
            assert_eq!(overview.children[0].role, ui::a11y::Role::TextInput);
            assert_eq!(overview.children[0].name.as_deref(), Some("Search city…"));
            let places = &overview.children[1];
            assert_eq!(places.role, ui::a11y::Role::List);
            assert_eq!(places.children.len(), 4);
            assert!(places.children[0].state.selected);
            assert!(places.children.iter().all(|item| item.state.focusable));
            let detail =
                core.detail_accessibility(vello::kurbo::Rect::new(300.0, 0.0, 1200.0, 700.0));
            assert_eq!(detail.name.as_deref(), Some("San Francisco forecast"));
            assert!(detail.description.unwrap().contains("hourly readings"));
        }
    }
}

#[cfg(test)]
mod history_regressions {
    use super::*;

    #[test]
    fn history_restore_fences_stale_gestures_searches_and_snapshots() {
        // Case `history_restore_fences_gestures_queue_search_and_changed_coordinate_responses`.
        {
            let mut core = Core::new(crate::sample::sample_model(), Units::IMPERIAL);
            let loc = core.model.locations[0].clone();
            let old_snapshot = core.model.snapshots.get(&loc.id).unwrap().clone();
            core.refresh_all();
            let requests = core.take_tracked_outbox();
            let (ticket, generation) = match &requests[0] {
                Request::Tracked {
                    ticket, generation, ..
                } => (*ticket, *generation),
                _ => panic!("tracked"),
            };
            core.refresh_all();
            core.seed_drag();
            core.seed_search_highlighted("unsaved query", crate::sample::sample_geocode_results());
            let seq = core.search_seq;
            core.queue_durable_action(WeatherAction::SetLocationOrder {
                ids: vec![loc.id.to_string()],
            });
            let dispatched_count = core.inflight - core.outbox.len();
            let mut restored = core.model.clone();
            restored.locations[0].latitude += 1.;
            restored.snapshots.clear();
            restored.air_quality.clear();
            core.adopt_saved_places(&mut restored);
            assert_eq!(core.search_query(), "unsaved query");
            assert!(core.search_results.is_empty());
            assert!(core.search_seq > seq);
            assert!(core.press.is_none() && core.menu.is_none());
            assert!(!core.reorder.is_active());
            assert!(core.take_durable_actions().is_empty());
            assert!(core.take_outbox().is_empty());
            assert_eq!(core.inflight, dispatched_count);
            assert_eq!(core.units, Units::IMPERIAL);
            assert!(!core.model.snapshots.contains_key(&loc.id));
            core.apply_response(Response::Tracked {
                ticket,
                generation,
                response: Box::new(Response::Forecast(old_snapshot.clone())),
            });
            assert!(!core.model.snapshots.contains_key(&loc.id));
            assert_eq!(core.inflight, dispatched_count - 1);
            core.apply_response(Response::Tracked {
                ticket,
                generation,
                response: Box::new(Response::Forecast(old_snapshot.clone())),
            });
            assert_eq!(
                core.inflight,
                dispatched_count - 1,
                "duplicate settlement ignored"
            );
            let mut empty = WeatherModel::default();
            core.adopt_saved_places(&mut empty);
            let mut recreated = WeatherModel::default();
            recreated.locations.push(loc.clone());
            core.adopt_saved_places(&mut recreated);
            let (ticket, generation) = match &requests[2] {
                Request::Tracked {
                    ticket, generation, ..
                } => (*ticket, *generation),
                _ => panic!("tracked"),
            };
            core.apply_response(Response::Tracked {
                ticket,
                generation,
                response: Box::new(Response::Forecast(old_snapshot)),
            });
            assert!(core.model.snapshots.is_empty());
            let next = SavedLocation::new("Next", "", "Country", "CC", 1., 2., "auto");
            core.seed_search_highlighted("Next", vec![next.clone()]);
            assert!(core.commit_search_pick(&result_value(&next)));
            assert!(core.open_location(loc.id));
            assert!(!core.take_durable_actions().is_empty());
            assert!(!core.take_outbox().is_empty());
        }
        // Case `history_stale_autocomplete_acceptance_cannot_select_new_unrelated_results`.
        {
            let mut core = Core::new(WeatherModel::default(), Units::METRIC);
            let old = SavedLocation::new("Old", "", "", "", 1., 2., "auto");
            let new = SavedLocation::new("New", "", "", "", 3., 4., "auto");
            core.seed_search_highlighted("New", vec![new]);
            assert!(!core.commit_search_pick(&result_value(&old)));
            assert!(core.take_durable_actions().is_empty());
        }
        // Case `history_deleted_location_rejects_delayed_provider_snapshot`.
        {
            let mut model = crate::sample::sample_model();
            let snapshot = model.snapshots.values().next().unwrap().clone();
            model.locations.clear();
            model.snapshots.clear();
            let mut core = Core::new(model, Units::METRIC);
            core.apply_response(Response::Forecast(snapshot));
            assert!(core.model.snapshots.is_empty());
        }
    }
}

#[cfg(test)]
mod incoming_responsive_tests {
    use super::*;
    #[test]
    fn large_text_allocates_intrinsic_vertical_space_and_fewer_columns() {
        let model = crate::sample::sample_model();
        let snapshot = model.selected_snapshot().unwrap();
        let normal =
            DetailBodyLayout::measure(334.0, snapshot, &Spacing::BASE, &Metrics::BASE, 1.0);
        let large = DetailBodyLayout::measure(334.0, snapshot, &Spacing::BASE, &Metrics::BASE, 2.0);
        assert!(large.body_h > normal.body_h * 1.5);
        assert!(large.tile_h > normal.tile_h);
        assert!(large.day_row_h > normal.day_row_h);
        assert_eq!(large.tile_cols, 2);
        assert_eq!(large.aq_cols, 2);
    }
}
