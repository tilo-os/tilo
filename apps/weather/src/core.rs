//! Shared weather state and the two **content panels** it renders.
//!
//! Per the HIG §1b, weather is *not* a sidebar+content app — it is an
//! **overview + detail** pair of content panels:
//!
//! * the **overview** is a *contentful* city list (each row carries quick
//!   per-city info — the current condition and temperature); and
//! * the **detail** is the full forecast for the selected city
//!   ([`detail`]).
//!
//! The window shell ([`ui::widget::NavShell`]) owns the header row, the
//! breakpoint, the side-by-side↔push transition and the Back button. This
//! module owns the *content* of the two panes plus the state they share (the
//! model, the add-place search, direct manipulation of the list) and the
//! network request queue the app pumps.
//!
//! [`Core`] is held once behind an `Rc<RefCell<…>>`; the two pane widgets
//! (see [`crate::panes`]) borrow it to draw and to route input, and the root
//! [`crate::app::WeatherApp`] borrows it to present the document, pump the network
//! worker, and drive the shell.

#[cfg(test)]
use automerge::AutoCommit;
use std::collections::HashMap;
use ui::time::{Duration, Instant};

use automerge::ObjId;
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Fill};
use winit::event::WindowEvent;

use ui::a11y::{AccessNode, AccessState, Role};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    Autocomplete, AutocompleteOutcome, ButtonFace, ContextMenu, ControlSize, DragController,
    EmptyState, EmptyStateContent, Interaction, ListMetrics, MenuItem, PressOutcome, ScrollMetrics,
    Scrollbar, Suggestion, SwipeAction, Widget,
};

use crate::icon;
use crate::interaction::{EntryAction, PointerPress, swipe_commit_dx};
use crate::model::{LocationId, SavedLocation, WeatherModel};
use crate::net::{Request, RequestKind, Response};
use crate::presentation::PlaceRowText;
use ui::framework::Submit;
mod detail;
use crate::resource::Weather;
use crate::units::Units;
use crate::wxcode::Sky;
pub use detail::RETRY_KEY;

/// How long a recently-typed query stays "fresh" before we fire a geocode.
const GEOCODE_DEBOUNCE: Duration = Duration::from_millis(220);

/// An intent the overview raises for the shell to act on (push the detail
/// in). Consumed by the root widget after routing an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavIntent {
    ShowDetail,
}

/// What the forecast is telling the person about its own freshness. Only
/// the two lasting problems are shown; progress is the refresh button's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ForecastNotice {
    /// The last refresh failed; the forecast on screen is older.
    UpdateFailed,
    /// No connection; the forecast on screen is the last one saved.
    Offline,
}

/// The parts of an event the overview's routing needs.
pub(crate) struct OverviewEventCtx<'a> {
    pub(crate) event: &'a WindowEvent,
    pub(crate) scale_factor: f64,
    pub(crate) modifiers: winit::event::Modifiers,
    pub(crate) state: ui::widget::Edits<'a>,
    pub(crate) path: &'a ObjId,
    pub(crate) focus_target: &'a mut Option<NodeId>,
    pub(crate) node_id: NodeId,
    pub(crate) taffy: &'a TaffyTree<()>,
}

impl OverviewEventCtx<'_> {
    /// Run `f` with a toolkit [`ui::widget::EventCtx`] over this event's
    /// document and focus, returning whether the widget handled it.
    fn with_widget_ctx(&mut self, f: impl FnOnce(&mut ui::widget::EventCtx<'_>)) -> bool {
        let mut handled = false;
        f(&mut ui::widget::EventCtx {
            scale_factor: self.scale_factor,
            modifiers: &self.modifiers,
            state: self.state.reborrow(),
            path: self.path,
            handled: &mut handled,
            focus_target: self.focus_target,
            node_id: self.node_id,
            taffy: self.taffy,
        });
        handled
    }
}

/// All mutable weather state shared by the two panes and the root widget.
pub struct Core {
    pub model: WeatherModel,
    /// Display units — read from the shared Settings store (`region`), never
    /// toggled inside this app.
    pub units: Units,

    last_mouse: Point,

    // --- overview: saved-place direct manipulation ---
    /// Press classifier: distinguishes a click from a vertical reorder-drag
    /// from a horizontal delete-swipe once the pointer moves.
    press: Option<PointerPress>,
    /// The per-place context menu and, by item index, the action each row
    /// runs (`None` for a separator).
    menu: ContextMenu,
    menu_actions: Vec<Option<EntryAction>>,
    menu_target: Option<usize>,
    /// The reusable drag-to-reorder model (live gap + reflow + floating
    /// ghost). Armed once a press classifies as a vertical reorder; commits
    /// the `(from, to)` move on drop and restores the order on cancel.
    reorder: DragController<LocationId>,
    over_rect: Rect,
    /// The scrolling band the saved-place rows stand in.
    list_rect: Rect,
    /// One row's height, measured from the row face at the last paint.
    row_h: f64,
    over_list_top: f64,
    over_scroll: f64,
    over_max_scroll: f64,
    over_bar: Scrollbar,
    /// The track the list's scroll thumb runs in, as last painted.
    over_track: Rect,
    /// How far left a row must be swiped to remove it, from the theme.
    swipe_commit: f64,
    /// How far one notch of a scroll wheel moves a list: three lines of
    /// body text, from the theme at the last paint.
    wheel_step: f64,
    /// The empty state's "Add place" action, while it shows.
    empty_action: Option<Rect>,

    // --- add a place: the shared Autocomplete, shown while adding ---
    /// The shared type-ahead field + suggestion overlay. Owns the query text,
    /// caret / selection / clipboard, and the arrow-key / Enter / Esc
    /// navigation of the geocoded matches. Weather feeds it results reactively
    /// (the geocode is a debounced network side effect), never a synchronous
    /// suggester closure. It shows only while a place is being added.
    search: Autocomplete,
    /// The field's on-screen rect (set each draw).
    search_field_rect: Rect,
    /// The add-place popover around the field, while a place is added.
    add_panel: Rect,
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
    /// The scrolling band of the forecast body.
    body_rect: Rect,
    scroll: f64,
    max_scroll: f64,
    body_bar: Scrollbar,
    body_track: Rect,
    body_content_h: f64,
    /// Where the out-of-date notice's Retry button takes a pointer, while
    /// it shows.
    retry_rect: Option<Rect>,

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
    notice: Option<ForecastNotice>,

    // --- shell intent (consumed by the root widget) ---
    nav_request: Option<NavIntent>,

    /// Where saved-place edits go: the framework's dispatcher, which applies
    /// them to the document and presents the result back. Network data stays
    /// local to this process and never reaches the document.
    dispatcher: Option<ui::framework::Dispatcher<Weather>>,
    /// The default place the document named when last presented.
    default_location: Option<LocationId>,
    /// The forecasts a capture shows in place of the network's.
    fixture_forecasts: Option<std::rc::Rc<WeatherModel>>,
}

impl Core {
    pub fn new(model: WeatherModel, units: Units) -> Self {
        Self {
            model,
            units,
            last_mouse: Point::ZERO,
            press: None,
            menu: ContextMenu::new(),
            menu_actions: Vec::new(),
            menu_target: None,
            reorder: DragController::vertical(),
            over_rect: Rect::ZERO,
            list_rect: Rect::ZERO,
            row_h: 0.0,
            over_list_top: 0.0,
            over_scroll: 0.0,
            over_max_scroll: 0.0,
            over_bar: Scrollbar::vertical(),
            over_track: Rect::ZERO,
            swipe_commit: swipe_commit_dx(&ui::tokens::Metrics::BASE),
            wheel_step: 0.0,
            empty_action: None,
            search: Autocomplete::new()
                .with_id("weather.city_search")
                .with_placeholder("City name"),
            search_field_rect: Rect::ZERO,
            add_panel: Rect::ZERO,
            search_query: String::new(),
            search_results: Vec::new(),
            search_seq: 0,
            pending_query_at: None,
            geocode_cache: HashMap::new(),
            geocode_inflight: false,
            detail_rect: Rect::ZERO,
            body_rect: Rect::ZERO,
            scroll: 0.0,
            max_scroll: 0.0,
            body_bar: Scrollbar::vertical(),
            body_track: Rect::ZERO,
            body_content_h: 0.0,
            retry_rect: None,
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
            notice: None,
            nav_request: None,
            dispatcher: None,
            default_location: None,
            fixture_forecasts: None,
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

    /// Submit saved-place edits through `dispatcher`.
    pub(crate) fn attach(&mut self, dispatcher: ui::framework::Dispatcher<Weather>) {
        self.dispatcher = Some(dispatcher);
    }

    /// Show `forecasts` for the places a capture stages, in place of fetched
    /// ones.
    pub(crate) fn show_fixture_forecasts(&mut self, forecasts: std::rc::Rc<WeatherModel>) {
        self.fixture_forecasts = Some(forecasts);
    }

    /// Submit one saved-place edit or verb. The model already previews it;
    /// the document presents the result back, or the stored state when the
    /// edit is refused.
    fn submit(&self, submit: impl Submit<Weather>) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.submit(submit);
        }
    }

    /// Adopt the presented saved places, invalidating stale gestures and
    /// unsent effects. Dispatched effects retain their real settlement count.
    /// The place on view stays this window's, unless the document's default
    /// place changed.
    pub(crate) fn adopt_saved_places(&mut self, persisted: &mut WeatherModel) {
        let changed = self.model.locations != persisted.locations;
        for l in &persisted.locations {
            // A cached reading belongs to a place's coordinates: one that
            // moved must be fetched again.
            if self.model.locations.iter().any(|old| same_place(old, l)) {
                if let Some(s) = self.model.snapshots.get(&l.id) {
                    persisted.snapshots.insert(l.id, s.clone());
                }
                if let Some(a) = self.model.air_quality.get(&l.id) {
                    persisted.air_quality.insert(l.id, a.clone());
                }
            }
            if let Some(fixture) = &self.fixture_forecasts {
                if let Some(s) = fixture.snapshots.get(&l.id) {
                    persisted.snapshots.entry(l.id).or_insert_with(|| s.clone());
                }
                if let Some(a) = fixture.air_quality.get(&l.id) {
                    persisted
                        .air_quality
                        .entry(l.id)
                        .or_insert_with(|| a.clone());
                }
            }
        }
        if changed {
            self.content_generation += 1;
            self.press = None;
            self.close_menu();
            self.reorder.cancel();

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
            self.notice = None;
        }
        let presented_default = persisted.selected_location().map(|location| location.id);
        let viewed = (presented_default == self.default_location)
            .then(|| self.model.selected_location().map(|location| location.id))
            .flatten();
        self.default_location = presented_default;
        if let Some(id) = viewed
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

    /// Whether the city list holds the pointer and the keyboard right now:
    /// a place is being added, its menu is open, or a row is being dragged.
    /// The forecast beside it takes no input meanwhile, so a key typed into
    /// the field or a click on a menu row that overlaps the forecast never
    /// reaches the forecast too.
    pub(crate) fn overview_owns_input(&self) -> bool {
        self.search.is_editing() || self.menu.is_open() || self.reorder.is_active()
    }

    /// The lasting problem the forecast shows above itself, if any.
    #[cfg(test)]
    pub(crate) fn notice(&self) -> Option<ForecastNotice> {
        self.notice
    }

    /// Show `notice` above the forecast (a scenario, or the live pump).
    pub(crate) fn set_notice(&mut self, notice: Option<ForecastNotice>) {
        self.notice = notice;
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
            self.status = "Updating forecasts".to_string();
        }
    }

    /// Open a saved city by id: select it, reset the forecast scroll, refresh
    /// its forecast, and raise the show-detail push intent — the same path a
    /// tap on the city list takes. Returns `false` (touching nothing) for an
    /// id that is not a saved city. Used by the platform search's activation.
    pub fn open_location(&mut self, id: LocationId) -> bool {
        let Some(pos) = self.model.locations.iter().position(|l| l.id == id) else {
            return false;
        };
        self.select_index(pos);
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
        if let Some(index) = self
            .model
            .selected_location()
            .and_then(|loc| self.model.locations.iter().position(|l| l.id == loc.id))
        {
            self.refresh_location(index);
        }
    }

    /// Select the saved place `id` (a rail row's id), as a click on its row
    /// in the list does.
    pub(crate) fn select_place(&mut self, id: &str) {
        if let Some(index) = self
            .model
            .locations
            .iter()
            .position(|loc| loc.id.to_string() == id)
        {
            self.select_index(index);
        }
    }

    /// The saved places as the overview's rail shows them: each one's id,
    /// name and current condition glyph, and the selected one's id.
    pub(crate) fn rail_places(&self) -> (Vec<(String, String, Icon)>, Option<String>) {
        let places = self
            .model
            .locations
            .iter()
            .map(|loc| {
                let icon = self
                    .model
                    .snapshot_for(loc.id)
                    .map_or(Icon::MapPin, |snap| {
                        crate::wxcode::Sky::from_code(snap.current.weather_code)
                            .rail_icon(snap.current.is_day)
                    });
                (loc.id.to_string(), loc.name.clone(), icon)
            })
            .collect();
        let selected = self.model.selected_location().map(|loc| loc.id.to_string());
        (places, selected)
    }

    /// Select the place at `index`, reset the forecast scroll, refresh it,
    /// and ask the shell to show its forecast.
    fn select_index(&mut self, index: usize) {
        self.model.select(index);
        self.scroll = 0.0;
        self.hourly_scroll_x = 0.0;
        self.refresh_selected();
        self.nav_request = Some(NavIntent::ShowDetail);
    }

    /// Move the overview selection by `delta` (arrow-key navigation of the
    /// city list), re-fetch the newly selected city's forecast and reset the
    /// forecast scroll. Returns true when the selection actually moved.
    fn move_selection(&mut self, delta: i64) -> bool {
        if self.model.locations.is_empty() {
            return false;
        }
        let before = self.model.selected;
        self.model.select_relative(delta);
        let moved = self.model.selected != before;
        if moved {
            self.scroll = 0.0;
            self.hourly_scroll_x = 0.0;
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
            self.status = "Updating forecast".to_string();
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
                self.status = "Forecast updated".to_string();
                self.notice = None;
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
                    // notice is reserved for the forecast itself.
                    self.search_results = results;
                    self.sync_search_view();
                }
            }
            Response::Error { kind, .. } => match kind {
                RequestKind::Forecast | RequestKind::AirQuality => {
                    self.inflight = self.inflight.saturating_sub(1);
                    self.status = "Couldn't update the forecast".to_string();
                    self.notice = Some(ForecastNotice::UpdateFailed);
                }
                RequestKind::Geocode => {
                    self.geocode_inflight = false;
                    // A failed geocode must resolve the dropdown's spinner into
                    // an honest state rather than spin forever.
                    self.search_results.clear();
                    self.search.set_busy(false);
                    self.search.set_status(Some(
                        "Couldn't search for places. Check your connection.".into(),
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
        let pending = self.search_pending();
        self.search
            .set_busy(!q.is_empty() && self.search_results.is_empty() && pending);
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
                // The place's name leads; its region and country trail as the
                // row's detail, so Paris, France and Paris, Texas stay
                // distinguishable at every width.
                .map(|r| Suggestion::labelled(result_value(r), &r.name).with_detail(r.subtitle()))
                .collect();
            self.search.set_suggestions(sugg);
            self.search.set_status(None);
        } else if pending {
            self.search.set_suggestions(Vec::new());
            self.search.set_status(Some("Searching…".to_string()));
        } else {
            self.search.set_suggestions(Vec::new());
            self.search
                .set_status(Some(format!("No places match “{q}”")));
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

    // ---- adding a place (begin / commit / end) ----------------------

    /// Show and focus the add-place field with an empty query. Called by the
    /// header's Add button and the empty state's action.
    pub(crate) fn begin_search(&mut self) {
        self.close_menu();
        self.search.set_text("");
        self.search.begin_edit();
        self.search_query.clear();
        self.search_results.clear();
        self.pending_query_at = None;
        self.geocode_inflight = false;
    }

    /// Hide the add-place field and clear its transient state.
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
            self.submit(Weather::save(&location));
            self.scroll = 0.0;
            self.hourly_scroll_x = 0.0;
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
                    self.submit(Weather::REMOVE.with(Weather::item(id)));
                    true
                } else {
                    false
                }
            }
            EntryAction::MoveToTop => {
                let changed = self.model.move_location(index, 0);
                if changed && let Some(location) = self.model.locations.first() {
                    self.submit(Weather::MOVE_TO_TOP.with(Weather::item(location.id)));
                }
                changed
            }
            EntryAction::Refresh => {
                self.refresh_location(index);
                false
            }
        }
    }

    /// Open the per-place menu for the row at `index`, anchored at the
    /// pointer, the row marked as its target.
    fn open_menu(&mut self, index: usize, at: Point) {
        let mut menu = ContextMenu::new();
        let mut actions = Vec::new();
        for action in EntryAction::menu_for_entry() {
            if action.is_destructive() {
                menu.push(MenuItem::separator());
                actions.push(None);
            }
            let item = if action.is_destructive() {
                MenuItem::destructive_selection(action.label())
            } else {
                MenuItem::selection(action.label())
            }
            .with_icon(action.icon())
            .with_enabled(action != EntryAction::MoveToTop || index > 0);
            menu.push(item);
            actions.push(Some(action));
        }
        if let Some(row) = self.row_rect(index) {
            menu.anchor_row(row);
        }
        menu.open_anchored(Rect::from_origin_size(at, (0.0, 0.0)), self.over_rect);
        self.menu = menu;
        self.menu_actions = actions;
        self.menu_target = Some(index);
    }

    fn close_menu(&mut self) {
        self.menu.close();
        self.menu_target = None;
    }

    /// Apply a row the menu reported chosen.
    fn apply_menu_selection(&mut self, item: usize) -> bool {
        let action = self.menu_actions.get(item).copied().flatten();
        let target = self.menu_target;
        self.close_menu();
        match (action, target) {
            (Some(action), Some(index)) => self.apply_entry_action(index, action),
            _ => false,
        }
    }

    // =================================================================
    // OVERVIEW pane — the contentful city list (master)
    // =================================================================

    /// Semantic projection of the painted saved-place list. The visual rows
    /// are painted faces, so expose their stable model identity and current
    /// readings explicitly instead of leaving screen-reader users with an
    /// unnamed canvas.
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
                AccessNode::new(Role::ListItem)
                    .with_name(location.name.clone())
                    .with_description(format!("{}; {reading}", location.subtitle()))
                    .with_position(index + 1, total)
                    .with_hashed_key(location.id)
                    .with_bounds(self.row_rect(index).unwrap_or(rect))
                    .with_state(
                        AccessState::new()
                            .selected(index == self.model.selected)
                            .focusable(true),
                    )
            })
            .collect();
        let places = AccessNode::new(Role::List)
            .with_name("Saved places")
            .with_hashed_key("weather.places")
            .with_description(
                "Use Up and Down Arrow to change place, and Enter to open its forecast.",
            )
            .with_bounds(rect)
            .with_children(items);
        let mut children = Vec::new();
        if self.search.is_editing()
            && let Some(search) = self.search.accessibility()
        {
            children.push(search);
        }
        children.push(places);
        AccessNode::new(Role::Group)
            .with_name("Places")
            .with_bounds(rect)
            .with_children(children)
            .with_keyed_action_routes("weather-overview")
    }

    /// The rect of the saved-place row at `index` as last painted, or
    /// `None` before the first paint and for an index past the list.
    fn row_rect(&self, index: usize) -> Option<Rect> {
        (index < self.model.locations.len() && self.row_h > 0.0).then(|| {
            let y = self.over_list_top + index as f64 * self.row_h;
            Rect::new(self.list_rect.x0, y, self.list_rect.x1, y + self.row_h)
        })
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
        self.swipe_commit = swipe_commit_dx(&theme.tokens.metrics);
        self.wheel_step = wheel_step(theme);
        let ctx = &mut PaintCtx::new(scene, t, tr, theme);
        let sp = &theme.tokens.spacing;
        let inset = f64::from(sp.s2);
        // The field and the rows' text start on the column's content edge,
        // where the column's title starts; a row's lift bleeds past it.
        let gutter = ui::chrome::column_gutter(theme);
        let edge = gutter - ui::chrome::row_bleed(theme);
        let top = rect.y0 + inset;

        // While a place is being added, its field stands in a popover from
        // the header's Add button (drawn in the overlay).
        if !self.search.is_editing() {
            self.search_field_rect = Rect::ZERO;
            self.add_panel = Rect::ZERO;
        }

        if self.model.locations.is_empty() {
            self.list_rect = Rect::ZERO;
            self.row_h = 0.0;
            self.empty_action = if self.search.is_editing() || !self.model.locations.is_empty() {
                None
            } else {
                let glyph = |ctx: &mut PaintCtx<'_>, r: Rect| {
                    crate::icon::draw_icon(
                        ctx.scene,
                        ctx.transform,
                        Sky::PartlyCloudy,
                        true,
                        r,
                        ctx.theme,
                    );
                };
                EmptyState::paint(
                    ctx,
                    Rect::new(rect.x0, top, rect.x1, rect.y1),
                    &EmptyStateContent {
                        illustration: Some(&glyph),
                        action_label: Some("Add place"),
                        ..EmptyStateContent::new("No places yet")
                    },
                    self.last_mouse,
                )
            };
            return;
        }
        self.empty_action = None;
        self.list_rect = Rect::new(rect.x0 + edge, top, rect.x1 - edge, rect.y1 - inset);
        self.row_h = crate::presentation::row_height(ctx.text, theme);
        self.draw_place_list(ctx);
    }

    /// The scrolling saved-place rows: the toolkit's list rows, a live gap
    /// and a floating ghost while one is dragged, and the list's scroll bar.
    fn draw_place_list(&mut self, ctx: &mut PaintCtx<'_>) {
        let list = self.list_rect;
        let count = self.model.locations.len();
        let content_h = count as f64 * self.row_h;
        self.over_max_scroll = (content_h - list.height()).max(0.0);
        self.over_scroll = self.over_scroll.clamp(0.0, self.over_max_scroll);
        self.over_list_top = list.y0 - self.over_scroll;

        // Re-pin the live insertion gap to the cursor against this frame's
        // geometry so a held drag reflows correctly even as the list scrolls.
        let metrics = self.list_metrics();
        self.reorder.sync_insertion(&metrics);
        let dragging = self.reorder.from_index();
        let selected = self.model.selected;
        let hovered = (!self.overview_owns_input())
            .then(|| self.row_at(self.last_mouse))
            .flatten();

        // Where the place at `index` stands in the list as it shows: its own
        // slot, the drop slot while it is dragged, or its reflowed slot.
        let slot = |index: usize| -> usize {
            if Some(index) == dragging {
                return self.reorder.insertion().unwrap_or(index);
            }
            let rows = (self.reorder.item_shift(index, &metrics) / self.row_h).round() as isize;
            index.saturating_add_signed(rows)
        };
        let t = ctx.transform;
        ctx.scene
            .push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &list);
        for (i, loc) in self.model.locations.iter().enumerate() {
            if Some(i) == dragging {
                continue;
            }
            let y =
                self.over_list_top + i as f64 * self.row_h + self.reorder.item_shift(i, &metrics);
            let row = Rect::new(list.x0, y, list.x1, y + self.row_h);
            if row.y1 < list.y0 || row.y0 > list.y1 {
                continue;
            }
            let text = PlaceRowText::of(loc, self.model.snapshot_for(loc.id), self.units);
            let interaction = Interaction {
                hovered: hovered == Some(i) || self.menu_target == Some(i),
                ..Interaction::default()
            };
            let mut face = text.face(loc, i == selected, interaction);
            // Separators follow the order the rows stand in, so a held drag
            // keeps them between its reflowed neighbours.
            let below = slot(i) + 1;
            if below < count {
                face = face.separator(slot(selected) == below);
            }
            let swipe = self.swipe_dx_for(i);
            if swipe > 0.0 {
                face = face.swiped(
                    SwipeAction {
                        icon: Icon::Trash2,
                        label: EntryAction::swipe_left().label(),
                        destructive: true,
                    },
                    swipe,
                );
            }
            face.paint(ctx, row);
        }

        // The live drop target: a rule in your colour where the dragged
        // place will land.
        if let Some(gap) = self.reorder.insertion() {
            let rule = f64::from(ctx.theme.tokens.metrics.chrome_rule_px) * 2.0;
            let y = self.over_list_top + gap as f64 * self.row_h;
            ctx.fill_rect(
                Rect::new(list.x0, y - rule / 2.0, list.x1, y + rule / 2.0),
                ctx.theme.tokens.colors.intent.you,
            );
        }
        ctx.scene.pop_layer();

        // The dragged place floats free under the cursor, drawn last so it
        // rides above the reflowing list.
        if let Some(i) = dragging
            && let (Some(loc), Some(ghost)) = (
                self.model.locations.get(i),
                self.reorder.ghost_rect(list.x0, list.x1, &metrics),
            )
        {
            let theme = ctx.theme;
            let radius = f64::from(theme.tokens.radii.item);
            ui::surface::shadow(
                ctx.scene,
                t,
                ghost,
                radius,
                &theme.tokens.elevation.floating,
            );
            ui::surface::panel(ctx.scene, t, ghost, radius, theme);
            let on_panel = theme.on_ground(ui::chrome::panel_ground(theme));
            let text = PlaceRowText::of(loc, self.model.snapshot_for(loc.id), self.units);
            text.face(loc, false, Interaction::default())
                .paint(&mut PaintCtx::new(ctx.scene, t, ctx.text, &on_panel), ghost);
        }

        self.over_track = scroll_track(list, ctx.theme);
        let track = self.over_track;
        let metrics = ScrollMetrics::new(content_h, list.height(), self.over_scroll);
        if metrics.overflows() {
            self.over_bar
                .draw(ctx.scene, t, track, metrics, ctx.theme, false);
        }
    }

    /// The overview's floating parts, painted in the window's top layer:
    /// the add-place suggestions and the per-place menu.
    pub fn draw_overview_overlay(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.search.is_editing() {
            self.draw_add_popover(scene, t, tr, theme, presence);
        }
        if self.menu.is_open() {
            self.menu.draw_overlay(scene, t, tr, theme, presence);
        }
    }

    /// The add-place form: the shared anchored floating Panel, opening below
    /// the header's Add button on its trailing edge, with its caption over
    /// the search field. The field's matches float below it.
    fn draw_add_popover(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use ui::widget::floating_surface;
        let over = self.over_rect;
        let spacing = &theme.tokens.spacing;
        let pad = f64::from(spacing.s4);
        let caption_style = theme.text(TextRole::Label);
        let caption_h = f64::from(caption_style.line_height_px());
        let field_h = ButtonFace::height(ControlSize::Regular, theme);
        // It stays over the column it adds to, which routes its input.
        let window = presence.viewport.get().unwrap_or(over);
        let room = floating_surface::panel_room(window, theme)
            .intersect(floating_surface::panel_room(over, theme).inflate(0.0, over.height()));
        let w = f64::from(theme.tokens.metrics.panel_width_list).min(room.width());
        let h = pad + caption_h + f64::from(spacing.s2) + field_h + pad;
        let anchor = Rect::new(room.x1 - field_h, over.y0 - field_h, room.x1, over.y0);
        let (panel, _) = floating_surface::place_aligned(
            anchor,
            (w, h),
            ui::widget::PopoverSide::Below,
            ui::widget::PanelAlign::Trailing,
            f64::from(spacing.s1),
            room,
        );
        self.add_panel = panel;
        floating_surface::paint(scene, t, panel, theme);
        let on_panel = theme.on_ground(ui::chrome::panel_ground(theme));
        tr.draw_style(
            scene,
            "Add a place",
            on_panel.text_primary,
            t * Affine::translate((panel.x0 + pad, panel.y0 + pad)),
            None,
            &caption_style,
        );
        let top = panel.y0 + pad + caption_h + f64::from(spacing.s2);
        let field = Rect::new(panel.x0 + pad, top, panel.x1 - pad, top + field_h);
        self.search_field_rect = field;
        self.search.apply_layout_external(field);
        self.search.draw(scene, t, tr, &on_panel, presence);
        self.search.draw_overlay(scene, t, tr, theme, presence);
    }

    /// The open menu's panel, which captures the pointer while it shows.
    pub fn overview_modal_capture(&self) -> Option<Rect> {
        self.menu.modal_capture()
    }

    /// The thin vertical track the list's scroll thumb runs in, at the
    /// list's trailing edge, as last painted.
    fn list_track(&self) -> Rect {
        self.over_track
    }

    /// The reorderable saved-place list's geometry this frame, for the
    /// toolkit's [`DragController`]. `over_list_top` is the scroll-adjusted
    /// leading edge of the first row, set during `draw_overview`.
    fn list_metrics(&self) -> ListMetrics {
        ListMetrics::uniform(
            self.over_list_top,
            self.row_h,
            0.0,
            self.model.locations.len(),
        )
    }

    // ---- overview drag / swipe helpers ------------------------------

    fn swipe_dx_for(&self, i: usize) -> f64 {
        match self.press {
            Some(p) if p.is_swipe() && p.index == i => p.swipe_dx(),
            _ => 0.0,
        }
    }

    fn row_at(&self, p: Point) -> Option<usize> {
        if !self.list_rect.contains(p) {
            return None;
        }
        (0..self.model.locations.len()).find(|&i| self.row_rect(i).is_some_and(|r| r.contains(p)))
    }

    /// Handle an event routed to the overview pane. Returns true if consumed.
    pub(crate) fn handle_overview_event(&mut self, mut ctx: OverviewEventCtx<'_>) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = ctx.event {
            self.last_mouse =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        // Keep the shared field placed so its overlay hit-tests correctly.
        if self.search.is_editing() {
            // A press on the popover around the field keeps it open.
            if matches!(
                ctx.event,
                WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    ..
                }
            ) && self.add_panel.contains(self.last_mouse)
                && !self.search_field_rect.contains(self.last_mouse)
            {
                return true;
            }
            self.search.apply_layout_external(self.search_field_rect);
            if let Some(consumed) = self.route_search_field(&mut ctx) {
                return consumed;
            }
        }
        if self.menu.is_open() {
            let event = ctx.event;
            let menu = &mut self.menu;
            ctx.with_widget_ctx(|wctx| menu.handle_event(event, wctx));
            return self.finish_menu_event(event);
        }
        let (handled, _changed) = self.dispatch_overview_event(ctx.event);
        handled
    }

    /// While a place is being added the shared Autocomplete owns typing and
    /// arrow/Enter/Esc navigation of the geocoded matches. Returns
    /// `Some(consumed)` when the field fully handled the event (the caller
    /// should return immediately), or `None` to fall through to the list's
    /// own gesture handling.
    fn route_search_field(&mut self, ctx: &mut OverviewEventCtx<'_>) -> Option<bool> {
        let before = self.search.text().to_string();
        let mut ac_handled = false;
        self.search.handle_event(
            ctx.event,
            &mut ui::widget::EventCtx {
                scale_factor: ctx.scale_factor,
                modifiers: &ctx.modifiers,
                state: ctx.state.reborrow(),
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
        state: &mut ui::widget::Edits<'_>,
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
                // the same press can still engage a row beneath the field.
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

    /// After the open menu saw an event: run what it chose. An open menu
    /// keeps the event (a press outside it only closes it).
    fn finish_menu_event(&mut self, event: &WindowEvent) -> bool {
        if let Some(item) = self.menu.take_selection() {
            self.apply_menu_selection(item);
            return true;
        }
        if !self.menu.is_open() {
            self.menu_target = None;
        }
        !matches!(event, WindowEvent::CursorMoved { .. })
    }

    /// The list's own gesture handling. Returns `(handled, changed)`.
    fn dispatch_overview_event(&mut self, event: &WindowEvent) -> (bool, bool) {
        use winit::event::{ElementState, KeyEvent, MouseButton};

        match event {
            WindowEvent::CursorMoved { .. } => {
                self.handle_overview_cursor_moved();
                (false, false)
            }
            WindowEvent::MouseWheel { delta, .. } => (self.handle_overview_wheel(*delta), false),
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

    fn handle_overview_cursor_moved(&mut self) {
        if let Some(press) = self.press.as_mut() {
            press.update(self.last_mouse);
        }
        if let Some(off) = self.over_bar.drag_to(
            self.list_track(),
            ScrollMetrics::new(
                self.model.locations.len() as f64 * self.row_h,
                self.list_rect.height(),
                self.over_scroll,
            ),
            self.last_mouse,
        ) {
            self.over_scroll = off;
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
                    // reorder, grabbing the row at the press point.
                    let grab = press.origin.y - metrics.slot_lead(press.index);
                    self.reorder.begin(loc.id, press.index, grab, press.origin);
                }
                self.reorder.pointer_moved(self.last_mouse, &metrics);
            } else if press.is_swipe() {
                self.reorder.cancel();
            }
        }
    }

    fn handle_overview_wheel(&mut self, delta: winit::event::MouseScrollDelta) -> bool {
        if self.over_rect.contains(self.last_mouse) && self.over_max_scroll > 0.0 {
            let dy = wheel_dy(delta, self.wheel_step);
            self.over_scroll = (self.over_scroll - dy).clamp(0.0, self.over_max_scroll);
            true
        } else {
            false
        }
    }

    fn handle_overview_right_click(&mut self) -> bool {
        if let Some(idx) = self.row_at(self.last_mouse) {
            self.open_menu(idx, self.last_mouse);
            true
        } else {
            false
        }
    }

    fn handle_overview_left_press(&mut self) -> (bool, bool) {
        if let Some(action) = self.empty_action
            && action.contains(self.last_mouse)
        {
            self.begin_search();
            return (true, false);
        }
        let track = self.list_track();
        if self.over_max_scroll > 0.0 {
            let metrics = ScrollMetrics::new(
                self.model.locations.len() as f64 * self.row_h,
                self.list_rect.height(),
                self.over_scroll,
            );
            match self.over_bar.press(track, metrics, self.last_mouse) {
                PressOutcome::Thumb => return (true, false),
                PressOutcome::Page(off) => {
                    self.over_scroll = off;
                    return (true, false);
                }
                PressOutcome::Miss => {}
            }
        }
        match self.row_at(self.last_mouse) {
            Some(i) => {
                self.press = Some(PointerPress::new(i, self.last_mouse));
                self.reorder.cancel();
                (true, false)
            }
            None => (false, false),
        }
    }

    fn handle_overview_left_release(&mut self) -> (bool, bool) {
        self.over_bar.release();
        let mut changed = false;
        let Some(press) = self.press.take() else {
            return (false, changed);
        };
        if press.is_reorder() {
            if let Some((from, to)) = self.reorder.commit() {
                // Commit the live insertion the controller tracked.
                changed = self.model.move_location(from, to);
                if changed && let Some(location) = self.model.locations.get(to) {
                    self.submit(
                        Weather::paths()
                            .places
                            .move_to(&Weather::item(location.id), to),
                    );
                }
            } else {
                // Dropped in place (or never activated): no change.
                self.reorder.cancel();
            }
        } else if press.is_swipe() {
            if press.swipe_dx() >= self.swipe_commit {
                changed = self.apply_entry_action(press.index, EntryAction::swipe_left());
            }
        } else {
            self.select_index(press.index);
            changed = true;
        }
        (true, changed)
    }

    /// Keys while the list itself has the keyboard (typing into the
    /// add-place field is the field's, routed above).
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
                }
            }
            // Arrow up/down move the highlighted city; the detail pane
            // follows the selection live (side-by-side) and a fresh
            // forecast is fetched for it.
            Key::Named(NamedKey::ArrowDown) => {
                changed = self.move_selection(1);
                handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                changed = self.move_selection(-1);
                handled = true;
            }
            // Enter opens the forecast for the highlighted city — the only
            // way to reach the detail by keyboard in the narrow push
            // layout, where it isn't shown yet.
            Key::Named(NamedKey::Enter) if self.model.selected_location().is_some() => {
                self.refresh_selected();
                self.nav_request = Some(NavIntent::ShowDetail);
                handled = true;
            }
            _ => {}
        }
        (handled, changed)
    }

    // ---- test / scenario seams --------------------------------------

    /// Stage a live mid-drag reorder of the second saved place for a headless
    /// shot: the row floats free as a ghost under the cursor while the rest
    /// of the list has reflowed to open a gap near the tail where it will
    /// land. Insertion is finalised by `sync_insertion` against real geometry
    /// during the shot's draw pass.
    pub fn seed_drag(&mut self) {
        if self.model.locations.is_empty() {
            return;
        }
        let from = 1.min(self.model.locations.len() - 1);
        let Some(id) = self.model.locations.get(from).map(|loc| loc.id) else {
            return;
        };
        let row_h = if self.row_h > 0.0 {
            self.row_h
        } else {
            // Before the first paint: the row at the default theme.
            crate::presentation::row_height(&mut TextRenderer::new(), &Theme::dark())
        };
        let top = if self.row_h > 0.0 {
            self.over_list_top
        } else {
            0.0
        };
        let x = self.list_rect.center().x.max(row_h);
        let start = Point::new(x, top + (from as f64 + 0.5) * row_h);
        // Past the last place, the drop rule showing just above the ghost.
        let target = Point::new(x, start.y + 2.2 * row_h);
        // Grab the row at its vertical centre so the ghost sits on the cursor.
        self.reorder.begin(id, from, row_h / 2.0, start);
        self.last_mouse = target;
        let m = ListMetrics::uniform(top, row_h, 0.0, self.model.locations.len());
        self.reorder.pointer_moved(target, &m);
        // Keep the classifier in the reorder state so a release commits it
        // like a real in-flight drag.
        let mut press = PointerPress::new(from, start);
        press.update(target);
        self.press = Some(press);
    }

    /// Stage the per-place menu open on the second place for a headless
    /// shot.
    pub fn seed_menu(&mut self) {
        let Some(row) = self.row_rect(1.min(self.model.locations.len().saturating_sub(1))) else {
            return;
        };
        let at = Point::new(row.x0 + row.width() * 0.6, row.center().y);
        self.last_mouse = at;
        self.open_menu(1.min(self.model.locations.len() - 1), at);
    }

    /// Stage the add-place field with a query and a set of geocoded matches
    /// (headless shot / test — the offscreen path can't await a live
    /// geocode). Drives the real shared [`Autocomplete`].
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
    pub fn row_rect_for_test(&self, index: usize) -> Rect {
        self.row_rect(index).unwrap_or(Rect::ZERO)
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
        self.menu.is_open()
    }
    /// The live rect the production hit-test would match for a given
    /// context-menu action, so tests click the *real* target rather than a
    /// recomputed guess.
    #[cfg(test)]
    pub fn menu_item_rect(&self, action: EntryAction) -> Option<Rect> {
        let index = self.menu_actions.iter().position(|a| *a == Some(action))?;
        self.menu.item_rect(index)
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
    /// Test seam: the live insertion boundary the reorder controller tracks
    /// this frame, or `None` when no reorder is in flight.
    #[cfg(test)]
    pub fn reorder_insertion_for_test(&self) -> Option<usize> {
        self.reorder.insertion()
    }
    /// Test seam: index of the row currently being dragged, if any.
    #[cfg(test)]
    pub fn reorder_from_for_test(&self) -> Option<usize> {
        self.reorder.from_index()
    }
    /// Test seam: the reflow shift (logical px) the controller applies to the
    /// non-dragged row at `i` while a drag is live.
    #[cfg(test)]
    pub fn reorder_item_shift_for_test(&self, i: usize) -> f64 {
        self.reorder.item_shift(i, &self.list_metrics())
    }
}

// =====================================================================
// The sun's arc: content drawing beside the forecast's hero
// =====================================================================

/// The sun's arc across the day, its dot placed by the observation time
/// between sunrise and sunset, with both times under the horizon.
pub(crate) struct SunArc {
    sunrise: String,
    sunset: String,
    /// Where the sun stands: 0 at sunrise, 1 at sunset.
    fraction: f64,
    is_day: bool,
    width: f64,
    radius: f64,
    height: f64,
}

impl SunArc {
    pub(crate) fn new(
        ctx: &mut PaintCtx<'_>,
        now: &str,
        is_day: bool,
        today: &crate::model::DayForecast,
    ) -> Self {
        let sp = &ctx.theme.tokens.spacing;
        let sunrise = detail::hhmm(&today.sunrise);
        let sunset = detail::hhmm(&today.sunset);
        let fraction = detail::sun_arc_fraction(&detail::hhmm(now), &sunrise, &sunset)
            .unwrap_or(if is_day { 0.5 } else { 0.0 });
        let radius = f64::from(sp.s16);
        let labels =
            ctx.line_box(TextRole::Caption) + f64::from(sp.s1) + ctx.line_box(TextRole::Label);
        Self {
            sunrise,
            sunset,
            fraction,
            is_day,
            width: 2.0 * radius + 2.0 * f64::from(sp.s6),
            radius,
            height: radius + f64::from(sp.s2) + labels + f64::from(sp.s2),
        }
    }

    pub(crate) fn paint(&self, ctx: &mut PaintCtx<'_>, area: Rect) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let sp = &theme.tokens.spacing;
        let (s1, s2) = (f64::from(sp.s1), f64::from(sp.s2));
        let label_h = ctx.line_box(TextRole::Caption);
        let value_h = ctx.line_box(TextRole::Label);
        let bottom = area.y0 + f64::midpoint(area.height(), self.height);
        let value_y = bottom - value_h;
        let label_y = value_y - s1 - label_h;
        let baseline = label_y - s2;
        let cx = area.center().x;
        let radius = self.radius;
        // The horizon, on the ground the arc stands on.
        ui::surface::separator(
            ctx.scene,
            ctx.transform,
            area.x0,
            area.x1,
            baseline + 0.5,
            0.0,
            c.surfaces.content,
        );
        let mut arc = BezPath::new();
        let steps = 28;
        for i in 0..=steps {
            let ang = std::f64::consts::PI * f64::from(i) / f64::from(steps);
            let p = Point::new(cx - radius * ang.cos(), baseline - radius * ang.sin());
            if i == 0 {
                arc.move_to(p);
            } else {
                arc.line_to(p);
            }
        }
        ctx.scene.stroke(
            &Stroke::new(f64::from(theme.tokens.metrics.chrome_rule_px) * 1.5),
            ctx.transform,
            c.text_tertiary,
            None,
            &arc,
        );
        let ang = std::f64::consts::PI * self.fraction;
        let sun = Point::new(cx - radius * ang.cos(), baseline - radius * ang.sin());
        let ink = if self.is_day {
            icon::sun_colour(theme)
        } else {
            c.text_dim
        };
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            ink,
            None,
            &Circle::new(sun, s1 + s1 / 2.0),
        );
        let value = theme.text(TextRole::Label).numeric();
        for (label, time, left) in [
            ("Sunrise", &self.sunrise, true),
            ("Sunset", &self.sunset, false),
        ] {
            let lw = ctx.measure_role(label, TextRole::Caption, None).width;
            let (tw, _) = ctx.text.measure_style(time, None, &value);
            let lx = if left { area.x0 } else { area.x1 - lw };
            let tx = if left {
                area.x0
            } else {
                area.x1 - f64::from(tw)
            };
            ctx.text_role(
                label,
                TextRole::Caption,
                c.text_dim,
                Point::new(lx, label_y),
                None,
            );
            ctx.text(time, &value, c.text_primary, Point::new(tx, value_y), None);
        }
    }
}

// =====================================================================
// Free helpers
// =====================================================================

/// A thin scroll track along the trailing edge of `area`: the thumb is the
/// theme's `s1` wide, an `s1` in from the edge.
fn scroll_track(area: Rect, theme: &Theme) -> Rect {
    let s1 = f64::from(theme.tokens.spacing.s1);
    Rect::new(area.x1 - 2.0 * s1, area.y0 + s1, area.x1 - s1, area.y1 - s1)
}

/// How far one notch of a scroll wheel moves: three lines of body text.
fn wheel_step(theme: &Theme) -> f64 {
    f64::from(theme.text(TextRole::Body).line_height_px()) * 3.0
}

/// A wheel delta in logical pixels: each notch moves `step` pixels.
fn wheel_dy(delta: winit::event::MouseScrollDelta, step: f64) -> f64 {
    match delta {
        winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(y) * step,
        winit::event::MouseScrollDelta::PixelDelta(p) => p.y,
    }
}

/// The horizontal part of a wheel delta in logical pixels.
fn wheel_dx(delta: winit::event::MouseScrollDelta, step: f64) -> f64 {
    match delta {
        winit::event::MouseScrollDelta::LineDelta(x, _) => f64::from(x) * step,
        winit::event::MouseScrollDelta::PixelDelta(p) => p.x,
    }
}

/// The encoded, unambiguous value for a geocoded search result — its display
/// name plus subtitle — so the accepted [`Autocomplete`] value maps back to the
/// exact place (two cities named "Paris" stay distinct).
fn result_value(loc: &SavedLocation) -> String {
    format!("{}, {}", loc.name, loc.subtitle())
}

fn same_place(a: &SavedLocation, b: &SavedLocation) -> bool {
    a.id == b.id
        && a.latitude.to_bits() == b.latitude.to_bits()
        && a.longitude.to_bits() == b.longitude.to_bits()
}
#[cfg(test)]
mod search_tests {
    use super::*;

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
                core.notice().is_none(),
                "the forecast notice stays clear while only the place search failed"
            );
        }
        // Case `accessible_place_search_selects_a_result_and_queues_its_forecast`.
        {
            use std::{cell::RefCell, rc::Rc};
            use ui::a11y::Action;
            let mut core = Core::new(WeatherModel::default(), Units::METRIC);
            let results = crate::sample::sample_model().locations;
            let chosen = results[1].clone();
            core.geocode_cache.insert("city".into(), results);
            core.begin_search();
            let core = Rc::new(RefCell::new(core));
            let mut pane = crate::panes::OverviewPane::new(core.clone());
            let mut state = AutoCommit::new();
            let search = pane.accessibility().unwrap().children.remove(0);
            assert!(pane.perform_semantic_action(
                search.action_target.as_deref().unwrap(),
                &Action::SetValue("city".into()),
                &mut ui::widget::Edits::lend(&mut state),
                &automerge::ROOT,
            ));
            assert_eq!(core.borrow().search_query, "city");
            assert!(core.borrow().pending_query_at.is_some());
            let search = pane.accessibility().unwrap().children.remove(0);
            let row = &search.children[1];
            assert_eq!(row.name.as_deref(), Some(chosen.name.as_str()));
            assert!(pane.perform_semantic_action(
                row.action_target.as_deref().unwrap(),
                &Action::Activate,
                &mut ui::widget::Edits::lend(&mut state),
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
            let overview = core.overview_accessibility(Rect::new(0.0, 0.0, 300.0, 700.0));
            assert_eq!(overview.role, Role::Group);
            assert_eq!(
                overview.children.len(),
                1,
                "no place is being added, so the list is the only child"
            );
            let places = &overview.children[0];
            assert_eq!(places.role, Role::List);
            assert_eq!(places.children.len(), 4);
            assert!(places.children[0].state.selected);
            assert!(places.children.iter().all(|item| item.state.focusable));
            assert!(places.key.is_some());
            let detail = core.detail_accessibility(Rect::new(300.0, 0.0, 1200.0, 700.0));
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
            let dispatched_count = core.inflight - core.outbox.len();
            let mut restored = core.model.clone();
            restored.locations[0].latitude += 1.;
            restored.snapshots.clear();
            restored.air_quality.clear();
            core.adopt_saved_places(&mut restored);
            assert_eq!(core.search_query(), "unsaved query");
            assert!(core.search_results.is_empty());
            assert!(core.search_seq > seq);
            assert!(core.press.is_none() && !core.menu.is_open());
            assert!(!core.reorder.is_active());
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
            assert!(core.model.locations.iter().any(|l| l.id == next.id));
            assert!(core.open_location(loc.id));
            assert!(!core.take_outbox().is_empty());
        }
        // Case `history_stale_autocomplete_acceptance_cannot_select_new_unrelated_results`.
        {
            let mut core = Core::new(WeatherModel::default(), Units::METRIC);
            let old = SavedLocation::new("Old", "", "", "", 1., 2., "auto");
            let new = SavedLocation::new("New", "", "", "", 3., 4., "auto");
            core.seed_search_highlighted("New", vec![new]);
            assert!(!core.commit_search_pick(&result_value(&old)));
            assert!(core.model.locations.is_empty());
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
