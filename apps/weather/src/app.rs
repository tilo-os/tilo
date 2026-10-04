//! Root widget for the weather app.
//!
//! Weather is an **overview + detail** pair of content panels (HIG §1b), not
//! a sidebar app. The responsive layout is delegated to the shared
//! [`ui::widget::NavShell`] primitive, constructed content-only (no genuine
//! sidebar): the *contentful city list* is the overview, the forecast is the
//! detail.
//!
//! * **Wide** (window ≥ the shell breakpoint): the two panels sit **side by
//!   side**.
//! * **Narrow**: the city list is full-screen; tapping a city **pushes** the
//!   forecast in from the right with a **Back** button. The shell owns that
//!   transition, the Back affordance and the divider.
//!
//! [`WeatherApp`] itself is thin: it holds the [`NavShell`] (whose two child
//! panes render from a shared [`Core`]), shows the saved places the framework
//! presents, submits edits through its dispatcher, and pumps the background
//! [`WeatherService`]. Measurement units come from the shared Settings store
//! (see [`crate::region`]) — this app reads the preference and never hosts a
//! toggle for it.
//!
//! ## Live, captures and past versions
//! Only a live window fetches: its draw/event pump asks Open-Meteo for the
//! presented places' current observations and never renders invented data.
//! A capture shows the bundled [`crate::sample`] forecasts its fixture
//! provides, and a past version shows the places without forecasts.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use ui::time::Instant;
use ui::widget::{AppModel, ModelPath};

use taffy::{NodeId, TaffyTree};
use ui::a11y::{AccessNode, Politeness, Role};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::framework::{EffectCx, EffectStatus, Effected, LiveRow, RunMode, SurfaceCx};
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Affordance, NavShell, ShellMode, Widget};

use crate::actions::WeatherEffect;
use crate::core::{Core, NavIntent};
use crate::model::LocationId;
use crate::net::WeatherService;
use crate::panes::{DetailPane, OverviewPane};
use crate::resource::{SampleForecasts, Weather};

/// Width of the city-list (overview) panel in the side-by-side layout.
const OVERVIEW_WIDTH: f64 = 340.0;
/// Width of the collapsed overview **rail** (glanceable icon column). It sits
/// below [`crate::core`]'s rail threshold, so allocating this width switches
/// the overview to its rail rendering with no separate flag to keep in sync.
const OVERVIEW_RAIL_WIDTH: f64 = 68.0;

/// A refresh someone invoked, waiting for the requests it sent.
struct PendingRefresh {
    /// The requests that belong to this refresh and have not settled.
    tickets: std::collections::HashSet<u64>,
    /// The first failure among them.
    failure: Option<String>,
    /// Answers the invocation once every request settled.
    answer: Box<dyn FnOnce(Result<(), String>)>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, orthogonal flag (rail-collapsed overview, a \
              pending shell rebuild, live network vs. bundled sample data, and the \
              one-shot initial fetch); they aren't a state machine, and grouping them \
              would mean churning every field access across this widget for no \
              behavioural gain"
)]
pub struct WeatherApp {
    core: Rc<RefCell<Core>>,
    nav: NavShell,

    /// Whether the city list is collapsed to a rail in the side-by-side
    /// layout. Drives the overview width the shell is rebuilt with.
    overview_collapsed: bool,
    /// Set when the shell is rebuilt (collapse toggle) so the event loop
    /// re-runs `build_layout` on the fresh shell before the next draw.
    structure_dirty: bool,

    /// Whether this window fetches: a live one, never a capture or a past
    /// version.
    live: bool,
    service: Option<WeatherService>,
    initial_fetch_done: bool,
    /// Whether the framework has presented the saved places yet.
    presented: bool,
    last_activity: Instant,
    /// The refresh an invocation waits for, if any.
    refresh: Option<PendingRefresh>,
}

impl WeatherApp {
    /// Show the saved places the framework presents. Refreshes waiting for
    /// requests not yet sent fail when the places they were for changed.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::model::WeatherModel) {
        if self.core.borrow().model.locations != state.locations
            && let Some(refresh) = self.refresh.as_mut()
        {
            let unsent = self.core.borrow().queued_weather_tickets_after(0);
            let mut invalidated = false;
            for ticket in unsent {
                invalidated |= refresh.tickets.remove(&ticket);
            }
            if invalidated {
                refresh.failure =
                    Some("Saved places changed before the refresh was dispatched".into());
            }
        }
        let mut persisted = state.clone();
        self.core.borrow_mut().adopt_saved_places(&mut persisted);
        self.presented = true;
    }

    /// Take the framework's dispatcher, and the forecasts this window shows:
    /// fetched in a live window, the fixture's in a capture, none in a past
    /// version.
    pub(crate) fn attach_framework(&mut self, cx: &SurfaceCx<Weather>) {
        self.core.borrow_mut().attach(cx.dispatcher());
        match cx.mode() {
            RunMode::Live => {
                self.live = true;
                self.core.borrow_mut().units = crate::region::load_units();
            }
            RunMode::Capture => {
                if let Some(sample) = cx.fixture::<SampleForecasts>() {
                    let mut core = self.core.borrow_mut();
                    core.units = sample.0.units;
                    core.show_fixture_forecasts(Rc::clone(&sample.0));
                }
            }
            RunMode::Past => {}
        }
    }

    /// Answer a weather effect, from a control, the palette or an agent.
    pub(crate) fn answer_effect(
        &mut self,
        effect: WeatherEffect,
        cx: &mut EffectCx<Weather, Self>,
    ) -> Result<Effected, String> {
        match effect {
            WeatherEffect::OpenLocation { location } => {
                let id = parse_location(&location)?;
                if !self.core.borrow_mut().open_location(id) {
                    return Err("Weather location is no longer saved".into());
                }
                if let Some(NavIntent::ShowDetail) = self.core.borrow_mut().take_nav_request() {
                    self.nav.show_detail();
                }
                Ok(Effected::done())
            }
            WeatherEffect::Refresh => {
                let reply = cx.reply();
                self.admit_refresh(None, move |outcome| {
                    reply.finish(outcome.map(|()| tilo_actions::ActionOutput::default()));
                })?;
                Ok(Effected::Pending)
            }
            WeatherEffect::RefreshLocation { location } => {
                let id = parse_location(&location)?;
                let reply = cx.reply();
                self.admit_refresh(Some(id), move |outcome| {
                    reply.finish(outcome.map(|()| tilo_actions::ActionOutput::default()));
                })?;
                Ok(Effected::Pending)
            }
        }
    }

    /// How the weather effects differ from their declarations now.
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
        if self.core.borrow().model.locations.is_empty() {
            vec![EffectStatus::unavailable(
                WeatherEffect::REFRESH,
                "no_locations",
                "Save a weather location before refreshing",
            )]
        } else {
            Vec::new()
        }
    }

    /// Each saved place, offering to open and refresh its forecast.
    pub(crate) fn location_rows(&self) -> Vec<LiveRow> {
        self.core
            .borrow()
            .model
            .locations
            .iter()
            .map(crate::actions::location_row)
            .collect()
    }

    /// Start a refresh an invocation waits for: of every saved place, or of
    /// `location`. `answer` hears its outcome once every request it sent
    /// settled; unrelated fetches neither delay nor change it.
    fn admit_refresh(
        &mut self,
        location: Option<LocationId>,
        answer: impl FnOnce(Result<(), String>) + 'static,
    ) -> Result<(), String> {
        if !self.live {
            return Err("network refresh is disabled for this weather view".into());
        }
        if self.refresh.is_some() || self.core.borrow().is_fetching() {
            return Err("a weather refresh is already running".into());
        }
        if self.core.borrow().model.locations.is_empty() {
            return Err("save a weather location before refreshing".into());
        }
        // Startup refreshes are background work. Capture the ticket boundary
        // after they are enqueued, immediately before this refresh's enqueue.
        self.ensure_service();
        let marker = self.core.borrow().request_marker();
        match location {
            Some(id) => {
                if !self.core.borrow_mut().refresh_location_by_id(id) {
                    return Err("Weather location is no longer saved".into());
                }
            }
            None => self.core.borrow_mut().refresh_all(),
        }
        self.last_activity = ui::time::now();
        self.refresh = Some(PendingRefresh {
            tickets: self.core.borrow().queued_weather_tickets_after(marker),
            failure: None,
            answer: Box::new(answer),
        });
        Ok(())
    }

    /// Answer the refresh in flight once every request it sent settled.
    fn settle_refresh(&mut self) {
        if self
            .refresh
            .as_ref()
            .is_some_and(|refresh| refresh.tickets.is_empty())
            && let Some(refresh) = self.refresh.take()
        {
            (refresh.answer)(refresh.failure.map_or(Ok(()), Err));
        }
    }

    /// Build the responsive shell at the overview width implied by `collapsed`
    /// (full list vs. rail). Both pane widgets share the one [`Core`].
    fn build_nav(core: Rc<RefCell<Core>>, collapsed: bool) -> NavShell {
        let overview: Box<dyn Widget> = Box::new(OverviewPane::new(core.clone()));
        let detail: Box<dyn Widget> = Box::new(DetailPane::new(core));
        // Authored 1× geometry. The shell resolves it against the theme it is
        // built with (F1, round two): the text-sized city list and the
        // breakpoint grow by the theme's responsive scale, the collapsed
        // icon rail follows `ui_scale` only — scaling them here as well
        // doubled both at 2×.
        let nav = NavShell::new(overview, detail)
            .with_breakpoint(ui::widget::DEFAULT_NAVSHELL_BREAKPOINT);
        if collapsed {
            nav.with_icon_overview_width(OVERVIEW_RAIL_WIDTH)
        } else {
            nav.with_overview_width(OVERVIEW_WIDTH)
        }
    }

    fn assemble(core: Rc<RefCell<Core>>) -> Self {
        let nav = Self::build_nav(core.clone(), false);
        Self {
            core,
            nav,
            overview_collapsed: false,
            structure_dirty: false,
            live: false,
            service: None,
            initial_fetch_done: false,
            presented: false,
            last_activity: ui::time::now(),
            refresh: None,
        }
    }

    /// Rebuild the shell after the collapse state changed, flagging a
    /// structure change so the event loop re-lays-out the new shell.
    fn set_overview_collapsed(&mut self, collapsed: bool) {
        if self.overview_collapsed == collapsed {
            return;
        }
        self.overview_collapsed = collapsed;
        self.nav = Self::build_nav(self.core.clone(), collapsed);
        self.structure_dirty = true;
    }

    /// Tell the core whether the collapse affordance applies this frame. It is
    /// offered only in the side-by-side layout — in the narrow push layout the
    /// list is already the full screen, so collapsing it would be meaningless.
    fn sync_overview_collapsible(&self) {
        let collapsible = self.nav.mode() == ShellMode::Wide;
        self.core.borrow_mut().set_overview_collapsible(collapsible);
    }

    /// A window showing the bundled sample places and their forecasts, with
    /// no network: what a capture shows, for driving the widget directly.
    #[cfg(test)]
    pub(crate) fn sample() -> Self {
        let app = Self::new();
        app.core.borrow_mut().model = crate::sample::sample_model();
        let mut app = app;
        app.presented = true;
        app
    }

    /// A window with no places yet: the framework presents them, and
    /// [`ResourceSurface::attach`] says whether it fetches.
    ///
    /// [`ResourceSurface::attach`]: ui::framework::ResourceSurface::attach
    pub fn new() -> Self {
        let core = Rc::new(RefCell::new(Core::new(
            crate::model::WeatherModel::default(),
            crate::units::Units::METRIC,
        )));
        Self::assemble(core)
    }

    /// The id of the currently selected city, if any. Lets a test observe where
    /// a Ctrl+K activation navigated.
    pub fn selected_city(&self) -> Option<crate::model::LocationId> {
        self.core.borrow().model.selected_location().map(|l| l.id)
    }

    /// Spawn the worker (idempotent) and queue the initial forecast refresh
    /// for every saved location. No-op for the offline `--shots`/test path.
    fn ensure_service(&mut self) {
        if !self.live {
            return;
        }
        if self.service.is_none() {
            self.service = Some(WeatherService::spawn());
        }
        if !self.initial_fetch_done
            && self.presented
            && !self.core.borrow().model.locations.is_empty()
        {
            // The record may arrive after the first empty sync. Do not spend
            // its initial refresh before its saved locations are available.
            // A freshly selected search result already has a fetch queued.
            if !self.core.borrow().is_fetching() {
                self.core.borrow_mut().refresh_all();
            }
            self.initial_fetch_done = true;
        }
    }

    /// Send queued requests to the worker and fold ready responses back into
    /// the model. Also fires any due geocode. Offline path is a no-op.
    fn tick_network(&mut self) {
        if !self.live {
            return;
        }
        self.ensure_service();
        self.core.borrow_mut().maybe_fire_geocode();

        let reqs = self.core.borrow_mut().take_tracked_outbox();
        let mut completions = Vec::new();
        let mut disconnected = false;
        if let Some(svc) = self.service.as_ref() {
            if !reqs.is_empty() {
                self.last_activity = ui::time::now();
            }
            for r in reqs {
                if let Some(failed) = svc.request(r) {
                    completions.push(failed);
                }
            }
            let (ready, closed) = svc.drain_with_status();
            completions.extend(ready);
            disconnected = closed;
        }
        for response in completions {
            self.apply_network_response(response);
        }
        if disconnected {
            // Real buffered replies settle first. Only the remaining accepted
            // work fails; no result is replaced and no request is retried.
            let failures = self.core.borrow().disconnected_responses();
            for response in failures {
                self.apply_network_response(response);
            }
        }
        self.settle_refresh();
    }

    fn apply_network_response(&mut self, response: crate::net::Response) {
        if let crate::net::Response::Tracked { ticket, .. } = &response
            && let Some(refresh) = self.refresh.as_mut()
            && refresh.tickets.remove(ticket)
            && let Some(message) = response.weather_error()
        {
            refresh.failure.get_or_insert_with(|| message.to_string());
        }
        self.core.borrow_mut().apply_response(response);
    }

    /// Set the detail pane's Back-affordance reservation from the shell state
    /// (only the shell knows whether a Back chip occupies the corner).
    fn sync_reserve_back(&self) {
        let reserve = matches!(self.nav.top_left_affordance(), Affordance::Back);
        self.core.borrow_mut().set_detail_reserve_back(reserve);
    }

    /// Stage a screenshot scenario's view state, once its places are
    /// presented.
    ///
    /// Recognised scenarios:
    /// * `"detail"` — push the forecast in (pair with a narrow `--viewport`
    ///   to capture the pushed detail + the Back button).
    /// * `"drag"` — a reorder drag of the second saved place is in flight.
    /// * `"scrolled"` — the forecast parked at the bottom.
    /// * `"detail_scrolled"` — narrow pushed forecast parked at the bottom.
    /// * `"collapsed"` — the city list collapsed to its rail (wide viewport).
    /// * `"search"` — the live-search dropdown showing geocoded matches.
    /// * `"search_loading"` — the dropdown's in-flight loading state.
    /// * `"search_empty"` — the dropdown's "no matches" state.
    /// * `"offline"` — cached conditions remain readable while connectivity
    ///   is unavailable.
    /// * `"error"` — a refresh error with a clear recovery message.
    ///
    /// `"onboarding"`, the first-run state with no saved places, stages
    /// nothing: its scenario saves no places.
    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        match scenario {
            "detail" => {
                self.nav.show_detail();
                self.nav.snap();
            }
            "drag" => self.core.borrow_mut().seed_drag(),
            "scrolled" => self.core.borrow_mut().seed_scrolled(),
            "detail_scrolled" => {
                self.nav.show_detail();
                self.nav.snap();
                self.core.borrow_mut().seed_scrolled();
            }
            "collapsed" => {
                self.set_overview_collapsed(true);
                self.core.borrow_mut().set_overview_collapsible(true);
            }
            "search" => self
                .core
                .borrow_mut()
                .seed_search_highlighted("Par", crate::sample::sample_geocode_results()),
            "search_loading" => self.core.borrow_mut().seed_search_loading("Reykjav"),
            "search_empty" => self.core.borrow_mut().seed_search_empty("Zzqx"),
            "offline" => {
                self.core.borrow_mut().status = "Offline — showing last saved forecast".into();
            }
            "error" => {
                self.core.borrow_mut().status =
                    "Couldn't update — check your connection and try again".into();
            }
            _ => {}
        }
    }
}

/// A saved place's id, as an effect names it.
fn parse_location(location: &str) -> Result<LocationId, String> {
    location
        .parse()
        .map_err(|_| "Invalid weather location id".to_string())
}

impl Default for WeatherApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for WeatherApp {
    fn update(&mut self, _state: &mut AppModel, _path: &ModelPath) -> bool {
        // Semantic actions reach the owning pane directly, so finish their
        // navigation here as well as on pointer input.
        let navigate = self.core.borrow_mut().take_nav_request();
        if let Some(NavIntent::ShowDetail) = navigate {
            self.nav.show_detail();
        }
        navigate.is_some()
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.nav.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The core solves its row grid without a renderer; hand it the
        // theme's spacing and metrics here so reserve and paint agree.
        self.core.borrow_mut().set_layout_tokens(theme);
        self.nav.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.nav.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.tick_network();
        self.sync_reserve_back();
        self.sync_overview_collapsible();
        self.nav
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &PermissionContext,
    ) {
        self.tick_network();
        self.sync_reserve_back();
        self.sync_overview_collapsible();
        self.nav.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.nav
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.ensure_service();
        self.nav.handle_event(event, ctx);

        // The overview raises a push intent when a city is selected.
        if let Some(NavIntent::ShowDetail) = self.core.borrow_mut().take_nav_request() {
            self.nav.show_detail();
        }

        // The collapse / expand chip rebuilds the shell at the new width.
        if self.core.borrow_mut().take_collapse_toggle() {
            self.set_overview_collapsed(!self.overview_collapsed);
        }

        self.tick_network();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.nav]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.nav]
    }

    fn name(&self) -> &'static str {
        "WeatherApp"
    }

    fn get_rect(&self) -> Rect {
        self.nav.get_rect()
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let core = self.core.borrow();
        let mut root = AccessNode::new(Role::Window)
            .with_name("Weather")
            .with_bounds(self.nav.get_rect());
        if !core.status.is_empty() {
            root.children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(core.status.clone())
                    .with_live(Politeness::Polite),
            );
        }
        Some(root)
    }

    fn needs_animation_frame(&self) -> bool {
        let core_busy = {
            let core = self.core.borrow();
            (core.is_fetching() && self.last_activity.elapsed().as_secs() < 12)
                || core.search_pending()
        };
        core_busy || self.nav.needs_animation_frame()
    }

    fn needs_idle_sync(&self) -> bool {
        let core_busy = {
            let core = self.core.borrow();
            // Keep pumping while a geocode is armed OR in flight, so its
            // result is drained and the dropdown updates promptly rather than
            // parking on the loading state until the next input event.
            core.is_fetching() || core.search_pending()
        };
        core_busy || self.nav.needs_idle_sync()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.core
            .borrow()
            .search_deadline()
            .into_iter()
            .chain(self.nav.next_wake_deadline())
            .min()
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mine = std::mem::take(&mut self.structure_dirty);
        self.nav.take_structure_dirty() || mine
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.nav.modal_capture()
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod interaction_tests {
    //! Drive the real `handle_event` / draw path (the same code the live loop
    //! runs) and assert the model + shell view actually changed. Built with
    //! `WeatherApp::new()` so `live == false`: no network, deterministic.
    use super::*;
    use crate::interaction::{EntryAction, SWIPE_COMMIT_DX};
    use crate::net::Request;
    use ui::headless::Framed;
    use ui::widget::ContentLayout;
    use ui::widget::ShellMode;
    use vello::kurbo::Point as KPoint;
    use winit::dpi::PhysicalPosition;
    use winit::event::{
        DeviceId, ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, TouchPhase,
        WindowEvent,
    };
    use winit::keyboard::{Key, KeyLocation, NamedKey, NativeKeyCode, PhysicalKey, SmolStr};

    fn synth_device() -> DeviceId {
        unsafe { std::mem::zeroed() }
    }

    /// Build a real `WindowEvent::KeyboardInput`. winit 0.30 keeps
    /// `KeyEvent::platform_specific` `pub(crate)`, so the struct is
    /// zero-initialised (every public field tolerates the all-zero bit
    /// pattern) and the fields the handler reads are overwritten — the same
    /// trick the ui/shell and sibling-app test harnesses use.
    fn key_event(logical_key: Key, text: Option<&str>) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = text.map(SmolStr::new);
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }

    struct Harness {
        app: Framed<WeatherApp>,
        tr: TextRenderer,
    }

    impl Harness {
        fn new(w: f64, h: f64) -> Self {
            let mut harness = Self {
                app: Framed::new(
                    ui::State::new(),
                    WeatherApp::sample(),
                    ui::headless::viewport((w, h)),
                    Theme::dark(),
                ),
                tr: TextRenderer::new(),
            };
            harness.redraw();
            harness
        }

        /// The production frame — sync, layout, draw — at a size, so the same
        /// instance can be re-laid-out to simulate a window resize.
        fn layout(&mut self, w: f64, h: f64) {
            Framed::harness_mut(&mut self.app)
                .relayout_at(ui::headless::viewport((w, h)))
                .expect("layout settles");
        }

        /// The app and its live document, for calls that take both.
        fn parts(&mut self) -> (&mut WeatherApp, &mut ui::State) {
            Framed::split(&mut self.app)
        }

        fn redraw(&mut self) {
            let presence = PresenceCtx::empty();
            let mut scene = Scene::new();
            let theme = Framed::harness(&self.app).theme().clone();
            self.app.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut self.tr,
                &theme,
                &presence,
            );
        }

        fn dispatch(&mut self, ev: &WindowEvent) {
            Framed::harness_mut(&mut self.app).dispatch_to_root_with(ev, &Modifiers::default());
        }

        fn move_to(&mut self, x: f64, y: f64) {
            self.dispatch(&WindowEvent::CursorMoved {
                device_id: synth_device(),
                position: PhysicalPosition::new(x, y),
            });
        }

        fn click(&mut self, x: f64, y: f64) {
            self.move_to(x, y);
            self.dispatch(&WindowEvent::MouseInput {
                device_id: synth_device(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            });
            self.dispatch(&WindowEvent::MouseInput {
                device_id: synth_device(),
                state: ElementState::Released,
                button: MouseButton::Left,
            });
        }

        fn press(&mut self, x: f64, y: f64) {
            self.move_to(x, y);
            self.dispatch(&WindowEvent::MouseInput {
                device_id: synth_device(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            });
        }

        fn release(&mut self) {
            self.dispatch(&WindowEvent::MouseInput {
                device_id: synth_device(),
                state: ElementState::Released,
                button: MouseButton::Left,
            });
        }

        fn right_click(&mut self, x: f64, y: f64) {
            self.move_to(x, y);
            self.dispatch(&WindowEvent::MouseInput {
                device_id: synth_device(),
                state: ElementState::Pressed,
                button: MouseButton::Right,
            });
        }

        fn key(&mut self, logical_key: Key, text: Option<&str>) {
            self.dispatch(&key_event(logical_key, text));
        }

        /// Centre-y of the saved-place chip at `i`, from the live list origin.
        /// Mirrors `CHIP_H` (62) + `CHIP_GAP` (8) from `core`.
        fn chip_center_y(&self, i: usize) -> f64 {
            let top = self.app.core.borrow().over_list_top();
            top + i as f64 * (62.0 + 8.0) + 62.0 / 2.0
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn layout_reflows_scrolls_and_collapses_the_city_list() {
        // Case `resize_reflows_between_breakpoints`.
        {
            // The breakpoint MUST be re-derived from the current allocated width
            // on every layout — not cached from the first frame.
            let mut h = Harness::new(1280.0, 720.0);
            assert_eq!(h.app.nav.mode(), ShellMode::Wide);
            assert_eq!(h.app.nav.content_layout(), ContentLayout::SideBySide);
            assert!(h.app.nav.overview_rect().width() > 1.0);
            assert!(h.app.nav.detail_rect().width() > 1.0);

            // Simulate a resize down to a narrow width on the SAME instance.
            h.layout(560.0, 720.0);
            assert_eq!(
                h.app.nav.mode(),
                ShellMode::Narrow,
                "narrow width must reflow to push"
            );
            assert_eq!(h.app.nav.content_layout(), ContentLayout::Push);

            // And back up — proves it isn't frozen after the first resize.
            h.layout(1280.0, 720.0);
            assert_eq!(
                h.app.nav.mode(),
                ShellMode::Wide,
                "widening must reflow back to side-by-side"
            );
        }
        // Case `wheel_scrolls_the_forecast_body`.
        {
            let mut h = Harness::new(1280.0, 620.0);
            h.redraw();
            assert!(
                h.app.core.borrow().max_scroll() > 0.0,
                "sample forecast is taller than 620px"
            );
            assert_eq!(h.app.core.borrow().scroll(), 0.0);
            // Point over the detail (right) panel's hero card — not the overview
            // list, and not the hourly strip, whose band owns the wheel for its
            // own horizontal scroll.
            h.move_to(900.0, 200.0);
            h.dispatch(&WindowEvent::MouseWheel {
                device_id: synth_device(),
                delta: MouseScrollDelta::LineDelta(0.0, -1.0),
                phase: TouchPhase::Moved,
            });
            assert!(
                h.app.core.borrow().scroll() > 0.0,
                "wheel scrolls the forecast body down"
            );
        }
        // Case `collapsing_the_city_list_reclaims_forecast_width`.
        {
            let mut h = Harness::new(1280.0, 720.0);
            assert_eq!(h.app.nav.content_layout(), ContentLayout::SideBySide);
            let detail_w_open = h.app.nav.detail_rect().width();
            let over = h.app.nav.overview_rect();
            assert!(over.width() > 300.0, "expanded list is full-width");

            // The draw pass sets `overview_collapsible` and builds the chip's
            // hotspot; click the collapse chip (top-right of the overview panel).
            h.redraw();
            let cx = over.x1 - 39.0; // chip spans x+w-30..x+w, RP=s6 → over.x1-54..-24
            let cy = over.y0 + 28.0;
            h.click(cx, cy);
            assert!(
                h.app.overview_collapsed,
                "the collapse chip collapses the list"
            );

            // Re-lay-out the rebuilt shell (the real loop does this on the
            // structure-dirty signal) and confirm the forecast reclaimed width.
            h.layout(1280.0, 720.0);
            let over_w = h.app.nav.overview_rect().width();
            let detail_w = h.app.nav.detail_rect().width();
            assert!(
                over_w < 120.0,
                "collapsed overview becomes a rail (got {over_w})"
            );
            assert!(
                detail_w > detail_w_open + 100.0,
                "collapsing reclaims width for the forecast ({detail_w_open} → {detail_w})"
            );

            // Expanding via the rail's chip restores the full list.
            h.redraw();
            let rail = h.app.nav.overview_rect();
            h.click(f64::midpoint(rail.x0, rail.x1), rail.y0 + 30.0);
            assert!(
                !h.app.overview_collapsed,
                "the rail chip expands the list back"
            );
        }
        // Case `collapsed_rail_is_not_frozen_across_a_resize`.
        {
            // The rail vs. full-list render is derived from the allocated width on
            // every pass, never cached — so a resize reflows even while collapsed.
            let mut h = Harness::new(1280.0, 720.0);
            h.redraw();
            let over = h.app.nav.overview_rect();
            h.click(over.x1 - 35.0, over.y0 + 28.0); // collapse
            h.layout(1280.0, 720.0);
            assert!(
                h.app.nav.overview_rect().width() < 120.0,
                "wide + collapsed is a rail"
            );

            // Resize to narrow on the SAME instance: the push layout ignores the
            // rail width, so the overview is full-screen (not a frozen rail).
            h.layout(560.0, 720.0);
            assert_eq!(h.app.nav.mode(), ShellMode::Narrow);
            let ow = h.app.nav.overview_rect().width();
            assert!(
                (ow - 560.0).abs() < 1.0,
                "narrow overview is full-screen even when collapsed ({ow})"
            );

            // Back to wide → the rail returns (state preserved, still reflowing).
            h.layout(1280.0, 720.0);
            assert!(
                h.app.nav.overview_rect().width() < 120.0,
                "widening restores the rail"
            );
        }
        // Case `hourly_strip_scrolls_when_it_overflows`.
        {
            // The 24 hours are laid out at a minimum per-hour width, so they
            // overflow the card and the strip scrolls horizontally via the shared
            // scrollbar primitive.
            let mut h = Harness::new(1280.0, 720.0);
            h.redraw();
            assert!(
                h.app.core.borrow().hourly_max_scroll() > 0.0,
                "24 hours at the min column width overflow the card"
            );
            assert_eq!(h.app.core.borrow().hourly_scroll_x(), 0.0);

            // A horizontal wheel over the strip scrolls it.
            let vp = h.app.core.borrow().hourly_viewport_rect();
            let p = vp.center();
            h.move_to(p.x, p.y);
            h.dispatch(&WindowEvent::MouseWheel {
                device_id: synth_device(),
                delta: MouseScrollDelta::LineDelta(-2.0, 0.0),
                phase: TouchPhase::Moved,
            });
            assert!(
                h.app.core.borrow().hourly_scroll_x() > 0.0,
                "a horizontal wheel scrolls the overflowing strip"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn city_navigation_by_pointer_keyboard_and_accessibility() {
        // Case `tapping_a_city_pushes_detail_then_back_returns`.
        {
            let mut h = Harness::new(560.0, 720.0);
            assert_eq!(h.app.nav.content_layout(), ContentLayout::Push);
            assert!(
                !h.app.nav.is_detail_shown(),
                "starts on the full-screen city list"
            );

            // Tap the SECOND city in the overview list.
            h.redraw(); // populate hotspots + list geometry
            let top = h.app.core.borrow().over_list_top();
            let cy = top + (62.0 + 8.0) + 62.0 / 2.0; // second chip centre
            h.click(150.0, cy);
            assert_eq!(
                h.app.core.borrow().model.selected,
                1,
                "tap selects the city"
            );
            assert!(h.app.nav.is_detail_shown(), "tap pushes the forecast in");

            // Settle the push so the Back affordance resolves, then press Back.
            h.app.nav.snap();
            assert_eq!(h.app.nav.top_left_affordance(), Affordance::Back);
            let back = h.app.nav.back_button_rect().center();
            h.click(back.x, back.y);
            assert!(
                !h.app.nav.is_detail_shown(),
                "Back returns to the city list"
            );
        }
        // Case `accessible_city_selection_commits_and_navigates_without_pointer_input`.
        {
            let mut h = Harness::new(420.0, 720.0);
            let results = crate::sample::sample_geocode_results();
            let chosen = results[1].id;
            h.app
                .core
                .borrow_mut()
                .seed_search_highlighted("Par", results);
            let tree = h.app.nav.children()[0].accessibility().unwrap();
            let target = tree.children[0].children[1]
                .action_target
                .as_deref()
                .unwrap();
            let (app, state) = h.parts();
            assert!(app.nav.children_mut()[0].perform_semantic_action(
                target,
                &ui::a11y::Action::Activate,
                &mut state.doc,
                &state.root,
            ));
            assert!(app.update(&mut state.doc, &state.root));
            assert_eq!(
                app.core.borrow().model.selected_location().unwrap().id,
                chosen
            );
            assert!(app.nav.is_detail_shown());
            assert!(!app.update(&mut state.doc, &state.root));
        }
        // Case `arrow_keys_navigate_cities_and_fetch`.
        {
            let mut h = Harness::new(1280.0, 720.0); // side-by-side; detail follows
            h.redraw();
            assert_eq!(h.app.core.borrow().model.selected, 0);
            h.app.core.borrow_mut().take_outbox(); // drain startup

            h.key(Key::Named(NamedKey::ArrowDown), None);
            assert_eq!(
                h.app.core.borrow().model.selected,
                1,
                "Arrow Down advances the selection"
            );
            let fetched = h
                .app
                .core
                .borrow_mut()
                .take_outbox()
                .iter()
                .any(|r| matches!(r, Request::Forecast { .. }));
            assert!(fetched, "moving to a city queues its forecast fetch");

            h.key(Key::Named(NamedKey::ArrowUp), None);
            assert_eq!(
                h.app.core.borrow().model.selected,
                0,
                "Arrow Up moves it back"
            );
        }
        // Case `detail_refresh_button_queues_a_fetch`.
        {
            let mut h = Harness::new(1280.0, 720.0);
            h.redraw();
            let btn = h.app.core.borrow().refresh_btn_rect();
            assert!(btn.width() > 0.0, "the refresh button is laid out");
            h.app.core.borrow_mut().take_outbox(); // drain startup
            let c = btn.center();
            h.click(c.x, c.y);
            let queued = h
                .app
                .core
                .borrow_mut()
                .take_outbox()
                .iter()
                .any(|r| matches!(r, Request::Forecast { .. }));
            assert!(queued, "clicking Refresh queues a forecast fetch");
        }
        // Case `detail_keyboard_controls_scroll_hourly_forecast_and_refresh`.
        {
            let mut h = Harness::new(1280.0, 720.0);
            h.redraw();
            assert!(
                h.app.core.borrow().hourly_max_scroll() > 0.0,
                "the populated hourly strip overflows at the standard detail width"
            );
            h.app.core.borrow_mut().take_outbox(); // drain startup requests

            h.key(Key::Named(NamedKey::ArrowRight), None);
            assert!(
                h.app.core.borrow().hourly_scroll_x() > 0.0,
                "Right Arrow pages the overflowing hourly forecast"
            );
            h.key(Key::Named(NamedKey::Home), None);
            assert_eq!(h.app.core.borrow().hourly_scroll_x(), 0.0);

            h.key(Key::Character(SmolStr::new("r")), Some("r"));
            assert!(
                h.app
                    .core
                    .borrow_mut()
                    .take_outbox()
                    .iter()
                    .any(|request| matches!(request, Request::Forecast { .. })),
                "R refreshes the forecast through the same queued network path as the button"
            );
        }
        // Case `root_accessibility_announces_weather_status`.
        {
            let mut h = Harness::new(1280.0, 720.0);
            h.redraw();
            h.app.core.borrow_mut().status = "Updated just now".into();
            let tree = h.app.accessibility().expect("weather exports a window");
            assert_eq!(tree.role, Role::Window);
            assert_eq!(tree.name.as_deref(), Some("Weather"));
            assert!(tree.children.iter().any(|node| {
                node.name.as_deref() == Some("Updated just now")
                    && node.live == Some(Politeness::Polite)
            }));
        }
    }

    #[test]
    fn live_weather_starts_empty_and_fetches_only_restored_or_selected_places() {
        // Case `onboarding_scenario_matches_the_live_empty_state`.
        {
            // The onboarding capture shows the SAME place-less empty state a
            // live first run shows, not a half-cleared sample.
            let capture =
                ui::prelude::TestApp::<crate::WeatherShell>::capture(Some("onboarding")).unwrap();
            assert!(capture.state().locations.is_empty());
            let mut detail =
                ui::prelude::TestApp::<crate::WeatherShell>::capture(Some("detail")).unwrap();
            assert_eq!(detail.state().locations.len(), 4);
            assert!(
                detail
                    .harness()
                    .resources()
                    .iter()
                    .flat_map(|resource| &resource.children)
                    .any(|child| child.actions.iter().any(|action| action.id == "open")),
                "each staged place offers its forecast"
            );
        }
        // Case `initial_refresh_waits_for_restored_locations`.
        {
            let mut app = WeatherApp::new();
            app.live = true;
            app.presented = true;
            app.ensure_service();
            assert!(!app.initial_fetch_done);
            assert!(app.core.borrow_mut().take_outbox().is_empty());
            app.core.borrow_mut().model = crate::sample::sample_model();
            app.ensure_service();
            assert!(app.initial_fetch_done);
            let requests = app.core.borrow_mut().take_outbox();
            assert_eq!(
                requests
                    .iter()
                    .filter(|r| matches!(r, Request::Forecast { .. }))
                    .count(),
                4
            );
            app.ensure_service();
            assert!(app.core.borrow_mut().take_outbox().is_empty());
        }
        // Case `a_new_window_starts_empty_with_no_fabricated_places_or_readings`.
        {
            // A window invents nothing: neither cities the user never added nor
            // readings for them. It shows the places the framework presents.
            let app = WeatherApp::new();
            let core = app.core.borrow();
            assert!(core.model.locations.is_empty());
            assert!(core.model.snapshots.is_empty());
            assert!(core.model.air_quality.is_empty());
            assert!(
                core.status.is_empty(),
                "no phantom Loading spinner with nothing to fetch"
            );
        }
        // Case `selecting_a_city_queues_a_real_fetch`.
        {
            // Proves the select path issues a network request (not a mock): the
            // outbox holds a Forecast request the app would pump to the worker.
            let mut h = Harness::new(560.0, 720.0);
            h.redraw();
            let top = h.app.core.borrow().over_list_top();
            let cy = top + (62.0 + 8.0) + 62.0 / 2.0;
            h.click(150.0, cy);
            let has_forecast_req = h
                .app
                .core
                .borrow_mut()
                .take_outbox()
                .iter()
                .any(|r| matches!(r, crate::net::Request::Forecast { .. }));
            assert!(
                has_forecast_req,
                "selecting a city queues a real forecast fetch"
            );
            let _ = KPoint::ZERO;
        }
        // Case `detail_renders_air_quality_from_real_data`.
        {
            // The sample carries a real air-quality reading; the detail render path
            // must consume it (parsed fields → section) without panicking, and the
            // selected place must actually have a reading to show.
            let mut h = Harness::new(1280.0, 720.0);
            {
                let aq = h.app.core.borrow().model.selected_air_quality().cloned();
                let aq = aq.expect("the selected place has an air-quality reading");
                assert!(aq.has_any() && aq.european_aqi.is_some());
            }
            h.redraw(); // exercises draw_air_quality on the real reading
            // The new section extends the scrollable body.
            assert!(h.app.core.borrow().max_scroll() > 0.0);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn city_search_autocomplete_picks_and_geocodes() {
        // Case `city_search_uses_the_shared_autocomplete_overlay`.
        {
            // Seeding the search stages the shared `ui::Autocomplete`; its overlay
            // is populated from the geocode results (not a bespoke dropdown).
            let mut h = Harness::new(1280.0, 720.0);
            h.app
                .core
                .borrow_mut()
                .seed_search("Par", crate::sample::sample_geocode_results());
            h.redraw();
            assert!(
                h.app.core.borrow().search_is_focused(),
                "the shared field is editing"
            );
            assert_eq!(
                h.app.core.borrow().search_suggestion_count(),
                crate::sample::sample_geocode_results().len(),
                "the overlay lists every geocoded match"
            );
        }
        // Case `picking_a_search_result_adds_and_selects_it`.
        {
            let mut h = Harness::new(1280.0, 720.0);
            let before = h.app.core.borrow().model.locations.len();
            h.app
                .core
                .borrow_mut()
                .seed_search("Par", crate::sample::sample_geocode_results());
            h.redraw(); // place the field + populate the shared overlay

            // Arrow-key navigation of the shared overlay, then Enter to accept —
            // no bespoke dropdown hit-test.
            h.key(Key::Named(NamedKey::ArrowDown), None);
            assert_eq!(
                h.app.core.borrow().search_highlight_label().as_deref(),
                Some("FR · Paris, Île-de-France"),
                "Arrow Down exposes the first suggestion's place context"
            );
            h.key(Key::Named(NamedKey::Enter), None);

            let model = &h.app.core.borrow().model;
            assert_eq!(
                model.locations.len(),
                before + 1,
                "picking a geocoded result adds the city"
            );
            assert_eq!(
                model.selected_location().unwrap().name,
                "Paris",
                "the added city becomes selected"
            );
        }
        // Case `arrow_nav_picks_the_disambiguated_result`.
        {
            // Two cities named "Paris" (France, then Texas): arrowing to the SECOND
            // and accepting must add THAT one — proving the shared overlay's
            // keyboard selection is bound to the real geocoded place, not matched by
            // display name.
            let mut h = Harness::new(1280.0, 720.0);
            h.app
                .core
                .borrow_mut()
                .seed_search("Par", crate::sample::sample_geocode_results());
            h.redraw();
            h.key(Key::Named(NamedKey::ArrowDown), None); // Paris, France
            h.key(Key::Named(NamedKey::ArrowDown), None); // Paris, Texas
            h.key(Key::Named(NamedKey::Enter), None);
            let model = &h.app.core.borrow().model;
            let added = model.selected_location().unwrap();
            assert_eq!(added.name, "Paris");
            assert_eq!(
                added.region, "Texas",
                "arrowing to the 2nd match adds Paris, Texas — not France"
            );
        }
        // Case `slash_focuses_search_and_escape_clears_it`.
        {
            let mut h = Harness::new(1280.0, 720.0);
            h.redraw();
            assert!(!h.app.core.borrow().search_is_focused());

            h.key(Key::Character(SmolStr::new("/")), None);
            assert!(
                h.app.core.borrow().search_is_focused(),
                "'/' focuses the search field"
            );

            for ch in ["P", "a", "r"] {
                h.key(Key::Character(SmolStr::new(ch)), Some(ch));
            }
            assert_eq!(
                h.app.core.borrow().search_query(),
                "Par",
                "typed chars build the query"
            );

            h.key(Key::Named(NamedKey::Escape), None);
            assert!(
                !h.app.core.borrow().search_is_focused(),
                "Escape blurs the search field"
            );
            assert_eq!(
                h.app.core.borrow().search_query(),
                "",
                "Escape clears the query"
            );
        }
        // Case `typing_then_enter_queues_a_debounced_geocode`.
        {
            let mut h = Harness::new(1280.0, 720.0);
            h.redraw();
            h.key(Key::Character(SmolStr::new("/")), None);
            for ch in ["P", "a", "r"] {
                h.key(Key::Character(SmolStr::new(ch)), Some(ch));
            }
            assert_eq!(h.app.core.borrow().search_query(), "Par");

            // Enter arms an immediate geocode (bypasses the wall-clock debounce);
            // the app pumps `maybe_fire_geocode` every frame.
            h.key(Key::Named(NamedKey::Enter), None);
            h.app.core.borrow_mut().maybe_fire_geocode();

            let queued =
                h.app.core.borrow_mut().take_outbox().iter().any(
                    |r| matches!(r, Request::Geocode { query, .. } if query.as_str() == "Par"),
                );
            assert!(
                queued,
                "typing then Enter queues a real geocode for the typed query"
            );
        }
    }

    // ---- right-click context menu (open → dispatch item) ----------------

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn context_menu_and_swipe_gestures_edit_the_city_list() {
        // Case `right_click_menu_delete_removes_the_city_and_persists`.
        {
            let mut h = Harness::new(560.0, 900.0);
            h.redraw();
            let before = h.app.core.borrow().model.locations.len();
            let victim = h.app.core.borrow().model.locations[1].id;

            // Open the per-entry menu on the second chip via right-click.
            let y1 = h.chip_center_y(1);
            h.right_click(150.0, y1);
            assert!(
                h.app.core.borrow().menu_is_open(),
                "right-click opens the context menu"
            );

            // Draw so the menu's item hotspots are built, then click Delete on the
            // real hotspot the production hit-test would match.
            h.redraw();
            let del = h
                .app
                .core
                .borrow()
                .menu_item_rect(EntryAction::Delete)
                .expect("Delete row hotspot present");
            let c = del.center();
            h.click(c.x, c.y);

            let model = &h.app.core.borrow().model;
            assert_eq!(
                model.locations.len(),
                before - 1,
                "Delete removes the entry"
            );
            assert!(
                !model.locations.iter().any(|l| l.id == victim),
                "the right city was removed"
            );
            assert!(
                !h.app.core.borrow().menu_is_open(),
                "the menu closes after an action"
            );
        }
        // Case `right_click_menu_move_to_top_reorders`.
        {
            let mut h = Harness::new(560.0, 900.0);
            h.redraw();
            let second = h.app.core.borrow().model.locations[1].id;
            let y1 = h.chip_center_y(1);
            h.right_click(150.0, y1);
            h.redraw();
            let mv = h
                .app
                .core
                .borrow()
                .menu_item_rect(EntryAction::MoveToTop)
                .expect("Move-to-top row hotspot present");
            let c = mv.center();
            h.click(c.x, c.y);
            assert_eq!(
                h.app.core.borrow().model.locations[0].id,
                second,
                "Move-to-top floats the entry to index 0"
            );
        }
        // Case `right_click_menu_refresh_queues_a_fetch_for_that_city`.
        {
            let mut h = Harness::new(560.0, 900.0);
            h.redraw();
            let target = h.app.core.borrow().model.locations[1].id;
            h.app.core.borrow_mut().take_outbox(); // drain any startup requests
            let y1 = h.chip_center_y(1);
            h.right_click(150.0, y1);
            h.redraw();
            let rf = h
                .app
                .core
                .borrow()
                .menu_item_rect(EntryAction::Refresh)
                .expect("Refresh row hotspot present");
            let c = rf.center();
            h.click(c.x, c.y);
            let queued = h.app.core.borrow_mut().take_outbox().iter().any(
                |r| matches!(r, Request::Forecast { location_id, .. } if *location_id == target),
            );
            assert!(
                queued,
                "menu Refresh queues a real forecast fetch for that city"
            );
        }
        // Case `swipe_left_past_threshold_deletes_the_city`.
        {
            let mut h = Harness::new(560.0, 900.0);
            h.redraw();
            let before = h.app.core.borrow().model.locations.len();
            let victim = h.app.core.borrow().model.locations[0].id;

            // Press the first chip, drag left past the commit distance, release.
            let y0 = h.chip_center_y(0);
            h.press(250.0, y0);
            h.move_to(250.0 - (SWIPE_COMMIT_DX + 20.0), y0 + 2.0); // mostly-horizontal → swipe
            h.release();

            let model = &h.app.core.borrow().model;
            assert_eq!(
                model.locations.len(),
                before - 1,
                "a committed swipe deletes the entry"
            );
            assert!(
                !model.locations.iter().any(|l| l.id == victim),
                "the swiped city is gone"
            );
        }
        // Case `short_swipe_does_not_delete`.
        {
            let mut h = Harness::new(560.0, 900.0);
            h.redraw();
            let before = h.app.core.borrow().model.locations.len();
            let y0 = h.chip_center_y(0);
            h.press(250.0, y0);
            h.move_to(250.0 - (SWIPE_COMMIT_DX - 30.0), y0 + 2.0); // under the commit distance
            h.release();
            assert_eq!(
                h.app.core.borrow().model.locations.len(),
                before,
                "a swipe that doesn't clear the threshold is a no-op"
            );
        }
    }

    // ---- swipe-to-delete + drag-to-reorder through handle_event ---------

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn drag_reorder_reflows_live_commits_on_drop_and_cancels() {
        // Case `vertical_drag_reorders_the_city`.
        {
            let mut h = Harness::new(560.0, 900.0);
            h.redraw();
            let first = h.app.core.borrow().model.locations[0].id;
            let n = h.app.core.borrow().model.locations.len();

            // Press the first chip and drag it straight down past the last chip.
            let y0 = h.chip_center_y(0);
            let target_y = h.chip_center_y(n - 1) + 80.0;
            h.press(200.0, y0);
            h.move_to(200.0, target_y); // mostly-vertical → reorder
            h.release();

            let model = &h.app.core.borrow().model;
            assert_ne!(
                model.locations[0].id, first,
                "the dragged city left index 0"
            );
            assert_eq!(
                model.locations.len(),
                n,
                "reorder preserves the entry count"
            );
            assert_eq!(
                model.locations.last().unwrap().id,
                first,
                "dropping below the list lands it at the end"
            );
        }
        // Case `drag_reflows_the_list_live_and_commits_only_on_drop`.
        {
            let mut h = Harness::new(560.0, 900.0);
            h.redraw();
            let order_before: Vec<_> = h
                .app
                .core
                .borrow()
                .model
                .locations
                .iter()
                .map(|l| l.id)
                .collect();
            let first = order_before[0];

            // Press the first chip and drag it down about two slots — mid-drag.
            let y0 = h.chip_center_y(0);
            h.press(200.0, y0);
            h.move_to(200.0, h.chip_center_y(2));

            {
                let core = h.app.core.borrow();
                // The toolkit controller is dragging item 0 and tracking a live
                // insertion index further down the list (the gap follows the
                // cursor, not the release).
                assert_eq!(core.reorder_from_for_test(), Some(0));
                let ins = core
                    .reorder_insertion_for_test()
                    .expect("a live insertion index is tracked while dragging");
                assert!(
                    ins >= 2,
                    "the gap opened down the list under the cursor, got {ins}"
                );
                // A non-dragged chip has physically reflowed to open that gap —
                // proving the list rearranges in real time.
                assert!(
                    core.reorder_item_shift_for_test(1).abs() > 0.0,
                    "the list reflows live during the drag"
                );
                // ...yet the model is untouched: the reorder only commits on drop.
                assert_eq!(
                    core.model
                        .locations
                        .iter()
                        .map(|l| l.id)
                        .collect::<Vec<_>>(),
                    order_before,
                    "no premature mutation — the drag is purely visual until release"
                );
            }

            // Releasing commits the tracked insertion into the model.
            h.release();
            let model = &h.app.core.borrow().model;
            assert_ne!(model.locations[0].id, first, "the drop reordered the model");
            assert_eq!(model.locations.len(), order_before.len());
        }
        // Case `escape_cancels_an_in_flight_reorder_and_restores_order`.
        {
            let mut h = Harness::new(560.0, 900.0);
            h.redraw();
            let order_before: Vec<_> = h
                .app
                .core
                .borrow()
                .model
                .locations
                .iter()
                .map(|l| l.id)
                .collect();

            // Start a vertical reorder of the first chip.
            let y0 = h.chip_center_y(0);
            h.press(200.0, y0);
            h.move_to(200.0, h.chip_center_y(2));
            assert!(
                h.app.core.borrow().reorder_from_for_test().is_some(),
                "a reorder is in flight before Escape"
            );

            // Escape abandons the drag and snaps the list back.
            h.key(Key::Named(NamedKey::Escape), None);
            {
                let core = h.app.core.borrow();
                assert_eq!(
                    core.reorder_from_for_test(),
                    None,
                    "escape cleared the drag"
                );
                assert_eq!(core.reorder_insertion_for_test(), None);
                assert_eq!(
                    core.model
                        .locations
                        .iter()
                        .map(|l| l.id)
                        .collect::<Vec<_>>(),
                    order_before,
                    "the original order is restored — the model was never mutated"
                );
            }

            // A release after the cancel is a no-op; nothing commits.
            h.release();
            assert_eq!(
                h.app
                    .core
                    .borrow()
                    .model
                    .locations
                    .iter()
                    .map(|l| l.id)
                    .collect::<Vec<_>>(),
                order_before,
                "no stale drag commits after Escape"
            );
        }
        // Case `dragging_onto_the_delete_zone_deletes`.
        {
            let mut h = Harness::new(560.0, 900.0);
            h.redraw();
            let before = h.app.core.borrow().model.locations.len();
            let victim = h.app.core.borrow().model.locations[0].id;

            // Begin a vertical reorder so the delete drop-zone appears, then drop
            // onto it.
            let y0 = h.chip_center_y(0);
            h.press(200.0, y0);
            h.move_to(200.0, y0 + 30.0); // classify as a reorder → zone shows
            let zone = h.app.core.borrow().delete_zone_rect_test().center();
            h.move_to(zone.x, zone.y);
            h.release();

            let model = &h.app.core.borrow().model;
            assert_eq!(
                model.locations.len(),
                before - 1,
                "dropping on the trash deletes"
            );
            assert!(!model.locations.iter().any(|l| l.id == victim));
        }
    }

    // ---- keyboard: arrow nav, '/' focus, Escape, geocode-on-typing ------

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn provider_cache_is_device_local_across_remote_and_local_changes() {
        // Case `remote_location_change_preserves_device_local_provider_cache`.
        {
            let mut h = Harness::new(1280.0, 720.0);
            let oslo = crate::model::SavedLocation::new(
                "Oslo",
                "Oslo",
                "Norway",
                "NO",
                59.9139,
                10.7522,
                "Europe/Oslo",
            );
            let mut presented = crate::sample::sample_model();
            presented.snapshots.clear();
            presented.air_quality.clear();
            presented.locations.push(oslo.clone());

            let local_id = h.app.core.borrow().model.locations[0].id;
            let local_temperature = 37.5;
            {
                let mut core = h.app.core.borrow_mut();
                core.model
                    .snapshots
                    .get_mut(&local_id)
                    .unwrap()
                    .current
                    .temperature = local_temperature;
            }

            h.app.adopt_resource_state(&presented);

            let core = h.app.core.borrow();
            assert!(
                core.model
                    .locations
                    .iter()
                    .any(|location| location.id == oslo.id),
                "remote saved location was adopted alongside local provider cache"
            );
            assert_eq!(
                core.model.snapshots[&local_id].current.temperature, local_temperature,
                "local pending provider cache survived the durable-state adoption"
            );
        }
        // Case `location_rows_offer_views_and_refreshes_beside_document_actions`.
        {
            let h = Harness::new(1280.0, 720.0);
            let rows = h.app.location_rows();
            assert_eq!(rows.len(), 4);
            for row in rows {
                let row = row.into_resource();
                for id in ["open", "refresh"] {
                    let action = row.actions.iter().find(|action| action.id == id).unwrap();
                    assert_eq!(action.kind, tilo_actions::OperationKind::Effect);
                }
                let refresh = row
                    .actions
                    .iter()
                    .find(|action| action.id == "refresh")
                    .unwrap();
                assert_eq!(refresh.contract.result, tilo_actions::ResultSpec::Job);
            }
        }
        // Case `local_navigation_does_not_drop_device_local_provider_cache`.
        {
            let mut h = Harness::new(1280.0, 720.0);
            let id = h.app.core.borrow().model.locations[0].id;
            {
                let mut core = h.app.core.borrow_mut();
                core.model
                    .snapshots
                    .get_mut(&id)
                    .unwrap()
                    .current
                    .temperature = 31.25;
            }

            h.key(Key::Named(NamedKey::ArrowDown), None);

            assert_eq!(
                h.app.core.borrow().model.snapshots[&id].current.temperature,
                31.25,
                "navigating must not discard an already-renderable local forecast"
            );
        }
    }

    // ---- detail Refresh button ------------------------------------------
}

#[cfg(test)]
mod refresh_tests {
    use super::*;
    use crate::model::WeatherModel;
    use crate::net::{Request, RequestKind, Response};

    /// What a refresh's invocation heard, in order.
    type Heard = Rc<RefCell<Vec<Result<(), String>>>>;

    /// A live window showing `model`'s places, with a worker the test drives.
    fn fixture(
        model: &WeatherModel,
    ) -> (
        WeatherApp,
        std::sync::mpsc::Receiver<Request>,
        std::sync::mpsc::Sender<Response>,
    ) {
        let mut app = WeatherApp::new();
        app.adopt_resource_state(model);
        let (service, requests, responses) = crate::net::test_service();
        app.service = Some(service);
        app.live = true;
        (app, requests, responses)
    }

    fn admit(app: &mut WeatherApp, location: Option<LocationId>) -> Heard {
        let heard = Heard::default();
        let into = Rc::clone(&heard);
        app.admit_refresh(location, move |outcome| into.borrow_mut().push(outcome))
            .unwrap();
        heard
    }

    /// Pump the network, then what the invocation heard since last asked.
    fn poll(app: &mut WeatherApp, heard: &Heard) -> Option<Result<(), String>> {
        app.tick_network();
        let mut heard = heard.borrow_mut();
        (!heard.is_empty()).then(|| heard.remove(0))
    }

    fn completion(request: &Request, failure: Option<&str>) -> Response {
        let Request::Tracked {
            ticket,
            generation,
            request,
        } = request
        else {
            panic!("production request must have a ticket")
        };
        let sample = crate::sample::sample_model();
        let (kind, response) = match request.as_ref() {
            Request::Forecast { location_id, .. } => {
                let mut snap = sample.snapshots.values().next().unwrap().clone();
                snap.location_id = *location_id;
                (RequestKind::Forecast, Response::Forecast(snap))
            }
            Request::AirQuality { location_id, .. } => {
                let mut aq = sample.air_quality.values().next().unwrap().clone();
                aq.location_id = *location_id;
                (RequestKind::AirQuality, Response::AirQuality(aq))
            }
            _ => panic!("weather request"),
        };
        Response::Tracked {
            ticket: *ticket,
            generation: *generation,
            response: Box::new(failure.map_or(response, |message| Response::Error {
                kind,
                message: message.into(),
            })),
        }
    }

    fn two_places() -> WeatherModel {
        let mut model = crate::sample::sample_model();
        model.locations.truncate(2);
        model
    }

    #[test]
    fn a_refresh_settles_with_its_own_requests_across_presented_changes() {
        // Its real result, even after the places it fetched were removed and
        // an unrelated refresh started.
        for failed in [false, true] {
            let sample = crate::sample::sample_model();
            let (mut app, requests, responses) = fixture(&sample);
            app.initial_fetch_done = true;
            let heard = admit(&mut app, None);
            app.tick_network();
            let sent: Vec<_> = requests.try_iter().collect();
            assert!(!sent.is_empty());
            app.adopt_resource_state(&WeatherModel::default());
            assert!(poll(&mut app, &heard).is_none());
            app.core.borrow_mut().model = crate::sample::sample_model();
            app.core.borrow_mut().refresh_all();
            app.tick_network();
            assert!(requests.try_iter().next().is_some());
            for request in &sent {
                responses
                    .send(completion(
                        request,
                        failed.then_some("actual provider failure"),
                    ))
                    .unwrap();
            }
            let outcome = poll(&mut app, &heard).unwrap();
            if failed {
                assert_eq!(outcome, Err("actual provider failure".to_owned()));
            } else {
                assert_eq!(outcome, Ok(()));
            }
            assert!(app.core.borrow().is_fetching());
            assert!(poll(&mut app, &heard).is_none(), "answered exactly once");
        }

        // A worker that died is a failure, not a success or a refresh that
        // never ends.
        {
            let (mut app, requests, _responses) = fixture(&crate::sample::sample_model());
            drop(requests);
            app.initial_fetch_done = true;
            let heard = admit(&mut app, None);
            assert!(matches!(poll(&mut app, &heard), Some(Err(_))));
        }

        // Places changing before its requests were sent fail it, and nothing
        // is sent for them.
        {
            let sample = crate::sample::sample_model();
            let (mut app, requests, _responses) = fixture(&sample);
            app.initial_fetch_done = true;
            let heard = admit(&mut app, None);
            let mut fewer = sample;
            fewer.locations.remove(0);
            app.adopt_resource_state(&fewer);
            assert!(matches!(
                poll(&mut app, &heard),
                Some(Err(message)) if message.contains("before the refresh was dispatched")
            ));
            assert!(requests.try_iter().next().is_none());
        }

        // Nothing to refresh, or a refresh already running, is refused.
        {
            let (mut app, _requests, _responses) = fixture(&WeatherModel::default());
            assert!(app.admit_refresh(None, |_| {}).is_err());
            assert!(!app.effect_statuses().is_empty());
            let (mut app, _requests, _responses) = fixture(&two_places());
            app.initial_fetch_done = true;
            let _heard = admit(&mut app, None);
            assert!(app.admit_refresh(None, |_| {}).is_err());
        }
    }

    fn accepted_worker_death(restore: bool) {
        let (mut app, requests, responses) = fixture(&two_places());
        app.initial_fetch_done = true;
        let heard = admit(&mut app, None);
        app.tick_network();
        let accepted: Vec<_> = requests.try_iter().collect();
        assert_eq!(accepted.len(), 4);
        assert!(
            accepted
                .iter()
                .all(|request| matches!(request, Request::Tracked { .. }))
        );
        assert!(poll(&mut app, &heard).is_none());
        if restore {
            app.adopt_resource_state(&WeatherModel::default());
        }
        drop(requests);
        drop(responses);
        assert!(
            matches!(poll(&mut app, &heard), Some(Err(message)) if message.contains("disconnected")),
            "accepted request must fail when its worker dies; restore={restore}"
        );
        assert!(!app.core.borrow().is_fetching());
        assert!(app.refresh.is_none());
        assert!(app.core.borrow().disconnected_responses().is_empty());
        assert!(
            poll(&mut app, &heard).is_none(),
            "terminal outcome emitted exactly once"
        );
        assert!(
            app.core.borrow_mut().take_outbox().is_empty(),
            "disconnected requests are not retried"
        );
    }

    #[test]
    fn worker_death_and_batches_settle_per_place_refreshes_once() {
        accepted_worker_death(false);
        accepted_worker_death(true);

        // Real completions buffered before the channel closed survive it.
        for complete_all in [false, true] {
            let (mut app, requests, responses) = fixture(&two_places());
            app.initial_fetch_done = true;
            let heard = admit(&mut app, None);
            app.tick_network();
            let sent: Vec<_> = requests.try_iter().collect();
            assert_eq!(sent.len(), 4);
            let target = app.core.borrow().model.locations[0].id;
            let count = if complete_all { sent.len() } else { 1 };
            for request in &sent[..count] {
                responses.send(completion(request, None)).unwrap();
            }
            drop(requests);
            drop(responses);
            let outcome = poll(&mut app, &heard);
            if complete_all {
                assert_eq!(outcome, Some(Ok(())));
            } else {
                assert!(matches!(outcome, Some(Err(message)) if message.contains("disconnected")));
            }
            assert!(
                app.core.borrow().model.snapshots.contains_key(&target),
                "real buffered forecast retained"
            );
            assert!(!app.core.borrow().is_fetching());
            assert!(app.core.borrow().disconnected_responses().is_empty());
            assert!(poll(&mut app, &heard).is_none());
        }

        startup_ownership(true, false);
        startup_ownership(false, false);
        startup_ownership(false, true);
    }

    /// A refresh of one place answers with that place's requests only:
    /// startup fetches and later work neither delay nor change it.
    fn startup_ownership(background_failure: bool, own_failure: bool) {
        let (mut app, requests, responses) = fixture(&two_places());
        assert!(!app.initial_fetch_done);
        let target = app.core.borrow().model.locations[0].id;
        let other = app.core.borrow().model.locations[1].id;
        let heard = admit(&mut app, Some(target));
        // Ordinary interactive work queued after acceptance but before the
        // first dispatch, in addition to the initial four startup requests.
        assert!(app.core.borrow_mut().refresh_location_by_id(other));
        app.tick_network();
        let sent: Vec<_> = requests.try_iter().collect();
        assert_eq!(sent.len(), 8);
        for (index, r) in sent.iter().enumerate() {
            let Request::Tracked { request, .. } = r else {
                panic!("ticket")
            };
            let id = match request.as_ref() {
                Request::Forecast { location_id, .. } | Request::AirQuality { location_id, .. } => {
                    *location_id
                }
                _ => panic!("weather"),
            };
            match index {
                0 | 1 => {
                    assert_eq!(id, target);
                    responses.send(completion(r, None)).unwrap();
                }
                2 | 3 => {
                    assert_eq!(id, other);
                    if background_failure {
                        responses
                            .send(completion(r, Some("unrelated startup failure")))
                            .unwrap();
                    }
                }
                4 | 5 => {
                    assert_eq!(id, target);
                    responses
                        .send(completion(r, own_failure.then_some("own provider failure")))
                        .unwrap();
                }
                6 | 7 => assert_eq!(id, other),
                _ => unreachable!(),
            }
        }
        let outcome = poll(&mut app, &heard);
        if own_failure {
            assert_eq!(outcome, Some(Err("own provider failure".to_owned())));
        } else {
            assert_eq!(
                outcome,
                Some(Ok(())),
                "only the requested place's requests belong to this refresh"
            );
        }
        assert!(
            app.core.borrow().is_fetching(),
            "unrelated queued/sent work remains outstanding"
        );
        // Duplicate owned responses must not settle background work or answer
        // a second time. Real background completions still apply.
        for r in &sent[4..6] {
            responses.send(completion(r, None)).unwrap();
        }
        assert!(poll(&mut app, &heard).is_none());
        assert!(app.core.borrow().is_fetching());
        for (i, r) in sent.iter().enumerate() {
            if i >= 6 || (i == 2 || i == 3) && !background_failure {
                responses.send(completion(r, None)).unwrap();
            }
        }
        app.tick_network();
        assert!(!app.core.borrow().is_fetching());
        assert!(app.core.borrow().model.snapshots.contains_key(&other));
    }
}

#[cfg(test)]
mod framework_tests {
    use crate::actions::location_resource_id;
    use crate::model::LocationId;
    use ui::prelude::TestApp;

    /// The default place is the document's; which place a window shows is
    /// its own. Edits to places go through the resource's actions.
    #[test]
    fn places_are_edited_through_the_resource_and_the_default_follows_the_document() {
        let mut app = TestApp::<crate::WeatherShell>::capture(Some("detail")).unwrap();
        let state = app.state();
        assert_eq!(state.locations.len(), 4);
        assert_eq!(state.selected, 0);
        let tokyo: LocationId = state.locations[1].id;
        app.call_on(
            &location_resource_id(tokyo),
            "use-as-default",
            serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(app.state().selected_location().unwrap().id, tokyo);
        app.call_on(
            &location_resource_id(tokyo),
            "move-to-top",
            serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(app.state().locations[0].id, tokyo);
        let removed = app.state().locations[3].id;
        assert!(
            app.call_on(
                &location_resource_id(removed),
                "remove",
                serde_json::json!({})
            )
            .is_err(),
            "removing a place asks for confirmation"
        );
        assert_eq!(app.state().locations.len(), 4);
    }

    /// A live window answers the weather effects through the door: with no
    /// saved places there is nothing to refresh, and a place it does not hold
    /// cannot be opened.
    #[test]
    fn a_live_window_answers_weather_effects_through_the_door() {
        let mut app = TestApp::<crate::WeatherShell>::new().unwrap();
        let refused = app
            .effect(&crate::actions::WeatherEffect::Refresh)
            .unwrap_err();
        assert!(
            matches!(&refused, tilo_actions::DoorError::Unavailable { code, .. } if code == "no_locations"),
            "{refused:?}"
        );
        assert!(
            app.effect(&crate::actions::WeatherEffect::OpenLocation {
                location: LocationId::from_u128(9).to_string(),
            })
            .is_err()
        );
    }
}
