//! Root widget for the weather app.
//!
//! Weather is an **overview + detail** pair of content panels (HIG §1b), not
//! a sidebar app. The window is the shared [`ui::widget::NavShell`] with no
//! sidebar: the *contentful city list* is the overview, the forecast is the
//! content. The window bar names the place shown and offers Add place and
//! Refresh (`window_bar`); the shell draws no header row.
//!
//! * **Wide** (both columns fit): the two panels sit **side by side**.
//! * **Narrow**: the city list is full-screen; tapping a city **pushes** the
//!   forecast in from the right with a **Back** button. The shell owns that
//!   transition and the Back button.
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
use std::rc::Rc;
use ui::time::Instant;
use ui::widget::{AppModel, ModelPath};

use ui::a11y::{AccessNode, Politeness, Role};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::WindowEvent;

use ui::framework::{EffectCx, EffectStatus, Effected, LiveRow, RunMode, SurfaceCx};
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Element, NavPane, NavShell, Widget};

use crate::actions::WeatherEffect;
use crate::core::{Core, ForecastNotice, NavIntent};
use crate::model::LocationId;
use crate::net::WeatherService;
use crate::panes::{DetailPane, OverviewPane};
use crate::resource::{SampleForecasts, Weather};

/// The overview rail's places (id, name, condition glyph) and the selected one.
type RailPlaces = (Vec<(String, String, Icon)>, Option<String>);

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
    /// The places the overview's rail was last built from.
    rail: RailPlaces,
    pending_scenario: Option<String>,
    repaint_staged: bool,

    /// Whether this window fetches: a live one, never a capture or a past
    /// version.
    live: bool,
    service: Option<WeatherService>,
    initial_fetch_done: bool,
    /// Whether the framework has presented the saved places yet.
    presented: bool,
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
        let changed = self.core.borrow().model.locations != state.locations;
        let mut persisted = state.clone();
        self.core.borrow_mut().adopt_saved_places(&mut persisted);
        self.presented = true;
        // Adopting changed places discards the replies still on their way;
        // fetch again what that left without a forecast. Before the first
        // refresh, `ensure_service` fetches every place anyway.
        if changed && self.live && self.initial_fetch_done {
            self.core.borrow_mut().fetch_missing_forecasts();
        }
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
                if self.core.borrow_mut().take_nav_request() == Some(NavIntent::ShowDetail) {
                    self.nav.show(NavPane::Content);
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
        let core = self.core.borrow();
        core.model
            .locations
            .iter()
            .map(|location| {
                let row = crate::actions::location_row(location);
                core.model.snapshot_for(location.id).map_or_else(
                    || row.clone(),
                    |snapshot| {
                        row.clone()
                            .presentation(crate::presentation::morning_preview(
                                snapshot,
                                core.units,
                                core.notice(),
                            ))
                    },
                )
            })
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

    /// The window: the city list as the overview and the forecast as the
    /// content, both rendering from the one shared [`Core`]; their actions
    /// are the window bar's.
    fn build_nav(core: &Rc<RefCell<Core>>) -> NavShell {
        let rail = core.borrow().rail_places();
        NavShell::new()
            .content_kind(ui::widget::ContentKind::Reading)
            .with_id("weather-shell")
            .with_window_toolbar()
            .overview(Box::new(OverviewPane::new(Rc::clone(core))))
            .overview_rail(Self::place_rail(core, &rail))
            .content(Box::new(DetailPane::new(Rc::clone(core))))
    }

    /// What the city list collapses into where it no longer fits beside the
    /// forecast: a rail of the same places, each by its current condition.
    fn place_rail(core: &Rc<RefCell<Core>>, (places, selected): &RailPlaces) -> Element {
        let items = places
            .iter()
            .map(|(id, name, icon)| ui::widget::NavItem::new(id.clone(), name.clone()).icon(*icon))
            .collect();
        let mut rail = ui::widget::NavList::new(vec![ui::widget::NavSection::untitled(
            "weather.places",
            items,
        )])
        .with_id("weather.rail")
        .with_accessible_name("Places")
        .rail()
        .on_select({
            let core = Rc::clone(core);
            move |id| core.borrow_mut().select_place(&id)
        });
        if let Some(selected) = selected {
            rail = rail.selected(selected);
        }
        Box::new(rail)
    }

    /// Whether the forecast column shows a place (and so can refresh it).
    fn shows_forecast(core: &Core) -> bool {
        core.model.selected_location().is_some()
    }

    /// Keep the overview's rail on the saved places.
    fn sync_rail(&mut self) {
        let rail = self.core.borrow().rail_places();
        if rail != self.rail {
            self.nav
                .set_overview_rail(Some(Self::place_rail(&self.core, &rail)));
            self.rail = rail;
        }
    }

    /// With no place chosen the forecast column is only a placeholder, so
    /// the places (and their Add place) are the screen to be on: a window
    /// narrowed to one screen at a time then keeps them in front rather than
    /// pushing a blank forecast over them. Choosing a place shows its forecast.
    fn keep_places_in_front_without_a_forecast(&mut self) {
        if !Self::shows_forecast(&self.core.borrow()) && self.nav.front_pane() != NavPane::Overview
        {
            self.nav.show(NavPane::Overview);
        }
    }

    fn assemble(core: Rc<RefCell<Core>>) -> Self {
        let nav = Self::build_nav(&core);
        let rail = core.borrow().rail_places();
        Self {
            core,
            nav,
            rail,
            pending_scenario: None,
            repaint_staged: false,
            live: false,
            service: None,
            initial_fetch_done: false,
            presented: false,
            refresh: None,
        }
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

    /// Spawn the worker (idempotent) and queue the initial forecast refresh
    /// for every saved location. No-op for the offline capture/test path.
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
        let disconnected = if let Some(svc) = self.service.as_ref() {
            for r in reqs {
                if let Some(failed) = svc.request(r) {
                    completions.push(failed);
                }
            }
            let (ready, closed) = svc.drain_with_status();
            completions.extend(ready);
            closed
        } else {
            false
        };
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

    /// Follow the panes' navigation intent and keep the forecast's title on
    /// the selected place.
    fn follow_core(&mut self) -> bool {
        let navigate = self.core.borrow_mut().take_nav_request();
        if navigate == Some(NavIntent::ShowDetail) {
            self.nav.show(NavPane::Content);
        }
        self.sync_rail();
        navigate.is_some()
    }

    /// Stage a screenshot scenario's view state, once its places are
    /// presented.
    ///
    /// Recognised scenarios:
    /// * `"detail"` — push the forecast in (a narrow window shows the
    ///   pushed forecast and its Back button).
    /// * `"drag"` — a reorder drag of the second saved place is in flight.
    /// * `"menu"` — the second place's menu is open.
    /// * `"scrolled"` — the forecast parked at the bottom.
    /// * `"detail_scrolled"` — narrow pushed forecast parked at the bottom.
    /// * `"search"` — adding a place, the geocoded matches listed.
    /// * `"search_loading"` — adding a place, the geocode in flight.
    /// * `"search_empty"` — adding a place, nothing matches.
    /// * `"offline"` — cached conditions remain readable while connectivity
    ///   is unavailable.
    /// * `"error"` — a refresh error with a clear recovery message.
    ///
    /// `"onboarding"`, the first-run state with no saved places, stages
    /// nothing: its scenario saves no places.
    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        match scenario {
            "detail" => {
                self.nav.show(NavPane::Content);
                self.nav.snap();
            }
            "drag" => self.core.borrow_mut().seed_drag(),
            "menu" => self.core.borrow_mut().seed_menu(),
            "scrolled" => self.core.borrow_mut().seed_scrolled(),
            "detail_scrolled" => {
                self.nav.show(NavPane::Content);
                self.nav.snap();
                self.core.borrow_mut().seed_scrolled();
            }
            "search" => self
                .core
                .borrow_mut()
                .seed_search_highlighted("Par", crate::sample::sample_geocode_results()),
            "search_loading" => self.core.borrow_mut().seed_search_loading("Reykjav"),
            "search_empty" => self.core.borrow_mut().seed_search_empty("Zzqx"),
            "offline" => self
                .core
                .borrow_mut()
                .set_notice(Some(ForecastNotice::Offline)),
            "error" => self
                .core
                .borrow_mut()
                .set_notice(Some(ForecastNotice::UpdateFailed)),
            _ => {}
        }
        self.sync_rail();
    }

    /// Stage a capture: the sample places with the network off, then the
    /// named scenario's view state.
    pub(crate) fn stage_capture(&mut self, scenario: Option<&str>) {
        self.live = false;
        self.service = None;
        match scenario {
            // A drag or a menu stands on the painted rows: stage it after
            // the next paint has laid them out.
            Some(name @ ("drag" | "menu")) => self.pending_scenario = Some(name.to_string()),
            Some(name) => self.stage_scenario(name),
            None => {}
        }
    }

    /// Stage a scenario that waited for the rows to be painted, and ask for
    /// one more frame so the capture shows it.
    fn stage_pending_scenario(&mut self) {
        self.repaint_staged = false;
        if let Some(name) = self.pending_scenario.take() {
            self.stage_scenario(&name);
            self.repaint_staged = true;
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

impl ui::widget::Decorator for WeatherApp {
    type Inner = NavShell;

    fn inner(&self) -> &NavShell {
        &self.nav
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.nav
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.take_window_toolbar_events();
        self.follow_core();
        self.declare_window_toolbar();
        self.nav.sync(state, path);
    }

    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        // Semantic actions reach the owning pane directly, so finish their
        // navigation here as well as on pointer input.
        let asked = self.take_window_toolbar_events();
        let followed = self.follow_core();
        self.declare_window_toolbar();
        asked || followed
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
        self.keep_places_in_front_without_a_forecast();
        self.core.borrow_mut().side_by_side = self.nav.geometry().side_by_side;
        self.nav
            .draw(scene, transform, text_renderer, theme, presence);
        self.stage_pending_scenario();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.ensure_service();
        self.nav.handle_event(event, ctx);

        // The overview raises a push intent when a city is selected.
        if self.core.borrow_mut().take_nav_request() == Some(NavIntent::ShowDetail) {
            self.nav.show(NavPane::Content);
        }

        self.tick_network();
    }

    fn name(&self) -> &'static str {
        "WeatherApp"
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
        // A forecast or a place on its way wakes the app when it lands
        // (the worker's wake handle), and a debounced search at its
        // deadline: neither runs frames while it waits.
        self.pending_scenario.is_some() || self.repaint_staged || self.nav.needs_animation_frame()
    }

    fn needs_idle_sync(&self) -> bool {
        self.nav.needs_idle_sync()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.core
            .borrow()
            .search_deadline()
            .into_iter()
            .chain(self.nav.next_wake_deadline())
            .min()
    }
}

mod window_bar;

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod interaction_tests {
    //! Drive the real `handle_event` / draw path (the same code the live loop
    //! runs) and assert the model + shell view actually changed. Built with
    //! `WeatherApp::new()` so `live == false`: no network, deterministic.
    use super::*;
    use crate::interaction::EntryAction;
    use crate::net::Request;
    use ui::headless::Framed;
    use vello::kurbo::Point as KPoint;

    use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
    use winit::keyboard::{Key, NamedKey, SmolStr};

    use ui::synthetic_input::key_press;

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
            self.dispatch(&ui::synthetic_input::cursor_moved(x, y));
        }

        fn click(&mut self, x: f64, y: f64) {
            self.move_to(x, y);
            self.dispatch(&ui::synthetic_input::mouse_press());
            self.dispatch(&ui::synthetic_input::mouse_release());
        }

        fn press(&mut self, x: f64, y: f64) {
            self.move_to(x, y);
            self.dispatch(&ui::synthetic_input::mouse_press());
        }

        fn release(&mut self) {
            self.dispatch(&ui::synthetic_input::mouse_release());
        }

        fn right_click(&mut self, x: f64, y: f64) {
            self.move_to(x, y);
            self.dispatch(&ui::synthetic_input::mouse_input(
                ElementState::Pressed,
                MouseButton::Right,
            ));
        }

        fn key(&mut self, logical_key: Key, text: Option<&str>) {
            self.dispatch(&key_press(logical_key, text));
        }

        /// Centre-y of the saved-place row at `i`, from the live list.
        fn row_center_y(&self, i: usize) -> f64 {
            self.app.core.borrow().row_rect_for_test(i).center().y
        }

        /// Centre-x of the saved-place rows.
        fn row_center_x(&self) -> f64 {
            self.app.core.borrow().row_rect_for_test(0).center().x
        }

        /// Press one of the app's actions in the window bar.
        fn press_bar(&mut self, id: &str) {
            Framed::harness_mut(&mut self.app)
                .press_window_slot(ui::window_toolbar::ToolbarEvent::Action { id: id.to_owned() });
            self.redraw();
        }

        /// How far a row must be swiped to be removed.
        fn swipe_commit() -> f64 {
            crate::interaction::swipe_commit_dx(&Theme::dark().tokens.metrics)
        }
    }

    #[test]
    fn with_no_places_the_empty_state_takes_the_wide_column() {
        let empty = |w: f64, h: f64| {
            let mut harness = Harness {
                app: Framed::new(
                    ui::State::new(),
                    WeatherApp::new(),
                    ui::headless::viewport((w, h)),
                    Theme::dark(),
                ),
                tr: TextRenderer::new(),
            };
            harness.redraw();
            harness
        };
        // Side by side, the forecast's column carries it, not the list.
        let mut h = empty(1708.0, 1400.0);
        let geometry = h.app.nav.geometry().clone();
        assert!(geometry.side_by_side);
        let action = h
            .app
            .core
            .borrow()
            .empty_action_for_test()
            .expect("Add place shows");
        assert!(
            geometry.content_column.contains(action.center()),
            "{action:?} in {:?}",
            geometry.content_column
        );
        h.click(action.center().x, action.center().y);
        assert!(
            h.app.core.borrow().search_is_focused(),
            "its Add place adds a place"
        );

        // A narrow window shows only the list, which then carries it.
        let mut h = empty(420.0, 800.0);
        assert!(!h.app.nav.geometry().side_by_side);
        let action = h
            .app
            .core
            .borrow()
            .empty_action_for_test()
            .expect("Add place shows");
        h.click(action.center().x, action.center().y);
        assert!(h.app.core.borrow().search_is_focused());
    }

    #[test]
    fn narrowing_an_empty_window_keeps_its_add_place_working() {
        let mut h = Harness {
            app: Framed::new(
                ui::State::new(),
                WeatherApp::new(),
                ui::headless::viewport((1708.0, 1400.0)),
                Theme::dark(),
            ),
            tr: TextRenderer::new(),
        };
        h.redraw();
        h.layout(420.0, 800.0);
        h.redraw();
        // The list stays in front: no place is chosen, so the forecast
        // would be empty.
        assert_eq!(h.app.nav.front_pane(), NavPane::Overview);
        let action = h
            .app
            .core
            .borrow()
            .empty_action_for_test()
            .expect("Add place shows");
        h.click(action.center().x, action.center().y);
        assert!(
            h.app.core.borrow().search_is_focused(),
            "the visible Add place adds a place"
        );
    }

    #[test]
    fn layout_reflows_and_scrolls() {
        {
            // The layout MUST be re-derived from the current allocated width
            // on every layout — not cached from the first frame.
            let mut h = Harness::new(1280.0, 720.0);
            let g = h.app.nav.geometry();
            assert!(g.side_by_side && g.stack.is_empty());
            assert!(g.overview_column.width() > 1.0);
            assert!(g.content_column.width() > 1.0);

            // Between: the list collapses into its rail of places beside
            // the forecast before anything pushes.
            h.layout(560.0, 720.0);
            let g = h.app.nav.geometry();
            assert_eq!(g.overview, ui::widget::OverviewPlacement::Rail, "{g:?}");
            assert!(g.stack.is_empty(), "the rail keeps the forecast beside it");

            h.layout(420.0, 720.0);
            assert!(
                !h.app.nav.geometry().stack.is_empty(),
                "a narrow width pushes the forecast over the list"
            );

            // And back up — proves it isn't frozen after the first resize.
            h.layout(1280.0, 720.0);
            assert!(
                h.app.nav.geometry().side_by_side,
                "widening reflows back to side by side"
            );
        }
        {
            let mut h = Harness::new(1280.0, 620.0);
            h.redraw();
            assert!(
                h.app.core.borrow().max_scroll() > 0.0,
                "sample forecast is taller than 620px"
            );
            assert_eq!(h.app.core.borrow().scroll(), 0.0);
            // Point over the forecast's hero — not the city list, and not the
            // hourly strip, whose band owns the wheel for its own scroll.
            let content = h.app.nav.geometry().content_column;
            h.move_to(content.center().x, content.y0 + 120.0);
            h.dispatch(&ui::synthetic_input::mouse_wheel(0.0, -1.0));
            assert!(
                h.app.core.borrow().scroll() > 0.0,
                "wheel scrolls the forecast body down"
            );
        }
        {
            // The 24 hours are laid out at a minimum per-hour width, so they
            // overflow the band and the strip scrolls horizontally via the
            // shared scrollbar primitive.
            let mut h = Harness::new(1280.0, 720.0);
            h.redraw();
            assert!(
                h.app.core.borrow().hourly_max_scroll() > 0.0,
                "24 hours at the min column width overflow the band"
            );
            assert_eq!(h.app.core.borrow().hourly_scroll_x(), 0.0);

            let vp = h.app.core.borrow().hourly_viewport_rect();
            let p = vp.center();
            h.move_to(p.x, p.y);
            h.dispatch(&ui::synthetic_input::mouse_wheel(-2.0, 0.0));
            assert!(
                h.app.core.borrow().hourly_scroll_x() > 0.0,
                "a horizontal wheel scrolls the overflowing strip"
            );
        }
        {
            let mut h = Harness::new(1280.0, 720.0);
            h.key(Key::Named(NamedKey::ArrowDown), None);
            h.layout(1280.0, 720.0);
            let second = h.app.core.borrow().model.locations[1].name.clone();
            assert_eq!(
                h.app
                    .window_toolbar()
                    .location
                    .map(|location| location.label),
                Some(second),
                "the window bar names the selected place"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn city_navigation_by_pointer_keyboard_and_accessibility() {
        {
            let mut h = Harness::new(420.0, 720.0);
            assert!(!h.app.nav.geometry().stack.is_empty());
            assert_ne!(
                h.app.nav.front_pane(),
                NavPane::Content,
                "starts on the full-screen city list"
            );

            h.redraw(); // lay out the rows
            let (x, y) = (h.row_center_x(), h.row_center_y(1));
            h.click(x, y);
            assert_eq!(
                h.app.core.borrow().model.selected,
                1,
                "tap selects the city"
            );
            assert_eq!(
                h.app.nav.front_pane(),
                NavPane::Content,
                "tap pushes the forecast in"
            );

            // Settle the push, then press the window bar's Back.
            h.app.nav.snap();
            h.layout(420.0, 720.0);
            assert!(
                !h.app.window_toolbar().leading.is_empty(),
                "a pushed forecast has Back in the window bar"
            );
            Framed::harness_mut(&mut h.app)
                .press_window_slot(ui::window_toolbar::ToolbarEvent::Back);
            h.redraw();
            assert_ne!(
                h.app.nav.front_pane(),
                NavPane::Content,
                "Back returns to the city list"
            );
        }
        {
            let mut h = Harness::new(420.0, 720.0);
            let results = crate::sample::sample_geocode_results();
            let chosen = results[1].id;
            h.app
                .core
                .borrow_mut()
                .seed_search_highlighted("Par", results);
            let mut pane = crate::panes::OverviewPane::new(h.app.core.clone());
            let tree = pane.accessibility().unwrap();
            let target = tree.children[0].children[1].action_target.clone().unwrap();
            let (app, state) = h.parts();
            assert!(pane.perform_semantic_action(
                &target,
                &ui::a11y::Action::Activate,
                &mut ui::widget::Edits::lend(&mut state.doc),
                &state.root,
            ));
            assert!(app.update(&state.doc, &state.root));
            assert_eq!(
                app.core.borrow().model.selected_location().unwrap().id,
                chosen
            );
            assert_eq!(app.nav.front_pane(), ui::widget::NavPane::Content);
            assert!(!app.update(&state.doc, &state.root));
        }
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
        {
            let mut h = Harness::new(1280.0, 720.0);
            h.redraw();
            h.app.core.borrow_mut().take_outbox(); // drain startup
            h.press_bar(window_bar::REFRESH);
            let queued = h
                .app
                .core
                .borrow_mut()
                .take_outbox()
                .iter()
                .any(|r| matches!(r, Request::Forecast { .. }));
            assert!(queued, "clicking Refresh queues a forecast fetch");
        }
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
        {
            let mut h = Harness::new(1280.0, 720.0);
            h.redraw();
            h.app.core.borrow_mut().status = "Forecast updated".into();
            let tree = h.app.accessibility().expect("weather exports a window");
            assert_eq!(tree.role, Role::Window);
            assert_eq!(tree.name.as_deref(), Some("Weather"));
            assert!(tree.children.iter().any(|node| {
                node.name.as_deref() == Some("Forecast updated")
                    && node.live == Some(Politeness::Polite)
            }));
        }
    }

    #[test]
    fn live_weather_starts_empty_and_fetches_only_restored_or_selected_places() {
        {
            // The onboarding capture shows the SAME place-less empty state a
            // live first run shows, not a half-cleared sample.
            let capture =
                ui::prelude::TestApp::<crate::WeatherShell>::capture(Some("onboarding")).unwrap();
            assert!(capture.state().model().locations.is_empty());
            let mut detail =
                ui::prelude::TestApp::<crate::WeatherShell>::capture(Some("detail")).unwrap();
            assert_eq!(detail.state().model().locations.len(), 4);
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
        {
            // Proves the select path issues a network request (not a mock): the
            // outbox holds a Forecast request the app would pump to the worker.
            let mut h = Harness::new(420.0, 720.0);
            h.redraw();
            let (x, y) = (h.row_center_x(), h.row_center_y(1));
            h.click(x, y);
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
        {
            // The sample carries a real air-quality reading; the detail render path
            // must consume it without panicking, and the selected place must
            // actually have a reading to show.
            let mut h = Harness::new(1280.0, 720.0);
            {
                let aq = h.app.core.borrow().model.selected_air_quality().cloned();
                let aq = aq.expect("the selected place has an air-quality reading");
                assert!(aq.has_any() && aq.european_aqi.is_some());
            }
            h.redraw(); // paints the air-quality tiles from the real reading
            assert!(h.app.core.borrow().max_scroll() > 0.0);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn adding_a_place_picks_geocoded_results() {
        {
            let mut h = Harness::new(1280.0, 720.0);
            assert!(!h.app.core.borrow().search_is_focused());
            h.press_bar(window_bar::ADD_PLACE);
            assert!(
                h.app.core.borrow().search_is_focused(),
                "Add place shows and focuses the field"
            );
            h.app
                .core
                .borrow_mut()
                .seed_search("Par", crate::sample::sample_geocode_results());
            h.redraw();
            assert_eq!(
                h.app.core.borrow().search_suggestion_count(),
                crate::sample::sample_geocode_results().len(),
                "the overlay lists every geocoded match"
            );
        }
        {
            let mut h = Harness::new(1280.0, 720.0);
            let before = h.app.core.borrow().model.locations.len();
            h.app
                .core
                .borrow_mut()
                .seed_search("Par", crate::sample::sample_geocode_results());
            h.redraw(); // place the field + populate the shared overlay

            h.key(Key::Named(NamedKey::ArrowDown), None);
            assert_eq!(
                h.app.core.borrow().search_highlight_label().as_deref(),
                Some("Paris"),
                "Arrow Down highlights the first match"
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
        {
            // Two cities named "Paris" (France, then Texas): arrowing to the SECOND
            // and accepting must add THAT one.
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
        {
            let mut h = Harness::new(1280.0, 720.0);
            h.redraw();
            h.app.core.borrow_mut().begin_search();
            h.redraw();
            for ch in ["P", "a", "r"] {
                h.key(Key::Character(SmolStr::new(ch)), Some(ch));
            }
            assert_eq!(
                h.app.core.borrow().search_query(),
                "Par",
                "typed chars build the query"
            );
            assert!(
                h.app
                    .core
                    .borrow_mut()
                    .take_outbox()
                    .iter()
                    .all(|r| !matches!(r, Request::Forecast { .. })),
                "typing never reaches the forecast's R shortcut"
            );

            h.key(Key::Named(NamedKey::Escape), None);
            assert!(
                !h.app.core.borrow().search_is_focused(),
                "Escape ends adding a place"
            );
            assert_eq!(
                h.app.core.borrow().search_query(),
                "",
                "Escape clears the query"
            );
        }
        {
            let mut h = Harness::new(1280.0, 720.0);
            h.redraw();
            h.app.core.borrow_mut().begin_search();
            h.redraw();
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

    // ---- the per-place menu (open → choose a row) -----------------------

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn place_menu_and_swipe_gestures_edit_the_city_list() {
        {
            let mut h = Harness::new(420.0, 900.0);
            h.redraw();
            let before = h.app.core.borrow().model.locations.len();
            let victim = h.app.core.borrow().model.locations[1].id;

            // Open the per-place menu on the second row via right-click.
            let (x, y1) = (h.row_center_x(), h.row_center_y(1));
            h.right_click(x, y1);
            assert!(
                h.app.core.borrow().menu_is_open(),
                "right-click opens the menu"
            );

            let del = h
                .app
                .core
                .borrow()
                .menu_item_rect(EntryAction::Delete)
                .expect("the remove row is laid out");
            let c = del.center();
            h.click(c.x, c.y);

            let model = &h.app.core.borrow().model;
            assert_eq!(
                model.locations.len(),
                before - 1,
                "Remove place removes the entry"
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
        {
            let mut h = Harness::new(420.0, 900.0);
            h.redraw();
            let second = h.app.core.borrow().model.locations[1].id;
            let (x, y1) = (h.row_center_x(), h.row_center_y(1));
            h.right_click(x, y1);
            let mv = h
                .app
                .core
                .borrow()
                .menu_item_rect(EntryAction::MoveToTop)
                .expect("the move-to-top row is laid out");
            let c = mv.center();
            h.click(c.x, c.y);
            assert_eq!(
                h.app.core.borrow().model.locations[0].id,
                second,
                "Move to top floats the entry to index 0"
            );
        }
        {
            let mut h = Harness::new(420.0, 900.0);
            h.redraw();
            let target = h.app.core.borrow().model.locations[1].id;
            h.app.core.borrow_mut().take_outbox(); // drain any startup requests
            let (x, y1) = (h.row_center_x(), h.row_center_y(1));
            h.right_click(x, y1);
            let rf = h
                .app
                .core
                .borrow()
                .menu_item_rect(EntryAction::Refresh)
                .expect("the refresh row is laid out");
            let c = rf.center();
            h.click(c.x, c.y);
            let queued = h.app.core.borrow_mut().take_outbox().iter().any(
                |r| matches!(r, Request::Forecast { location_id, .. } if *location_id == target),
            );
            assert!(
                queued,
                "the menu's Refresh queues a real forecast fetch for that city"
            );
        }
        {
            let mut h = Harness::new(420.0, 900.0);
            h.redraw();
            let before = h.app.core.borrow().model.locations.len();
            let victim = h.app.core.borrow().model.locations[0].id;

            // Press the first row, drag left past the commit distance, release.
            let (x, y0) = (h.row_center_x() + 100.0, h.row_center_y(0));
            h.press(x, y0);
            h.move_to(x - (Harness::swipe_commit() + 20.0), y0 + 2.0); // mostly-horizontal → swipe
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
        {
            let mut h = Harness::new(420.0, 900.0);
            h.redraw();
            let before = h.app.core.borrow().model.locations.len();
            let (x, y0) = (h.row_center_x() + 100.0, h.row_center_y(0));
            h.press(x, y0);
            h.move_to(x - (Harness::swipe_commit() - 30.0), y0 + 2.0); // under the commit distance

            h.release();
            assert_eq!(
                h.app.core.borrow().model.locations.len(),
                before,
                "a swipe that doesn't clear the threshold is a no-op"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn drag_reorder_reflows_live_commits_on_drop_and_cancels() {
        {
            let mut h = Harness::new(420.0, 900.0);
            h.redraw();
            let first = h.app.core.borrow().model.locations[0].id;
            let n = h.app.core.borrow().model.locations.len();

            // Press the first row and drag it straight down past the last row.
            let x = h.row_center_x();
            let y0 = h.row_center_y(0);
            let target_y = h.row_center_y(n - 1) + 80.0;
            h.press(x, y0);
            h.move_to(x, target_y); // mostly-vertical → reorder
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
        {
            let mut h = Harness::new(420.0, 900.0);
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

            // Press the first row and drag it down about two slots — mid-drag.
            let x = h.row_center_x();
            let y0 = h.row_center_y(0);
            h.press(x, y0);
            let y2 = h.row_center_y(2);
            h.move_to(x, y2);

            {
                let core = h.app.core.borrow();
                assert_eq!(core.reorder_from_for_test(), Some(0));
                let ins = core
                    .reorder_insertion_for_test()
                    .expect("a live insertion index is tracked while dragging");
                assert!(
                    ins >= 2,
                    "the gap opened down the list under the cursor, got {ins}"
                );
                assert!(
                    core.reorder_item_shift_for_test(1).abs() > 0.0,
                    "the list reflows live during the drag"
                );
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

            h.release();
            let model = &h.app.core.borrow().model;
            assert_ne!(model.locations[0].id, first, "the drop reordered the model");
            assert_eq!(model.locations.len(), order_before.len());
        }
        {
            let mut h = Harness::new(420.0, 900.0);
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

            let x = h.row_center_x();
            let y0 = h.row_center_y(0);
            h.press(x, y0);
            let y2 = h.row_center_y(2);
            h.move_to(x, y2);
            assert!(
                h.app.core.borrow().reorder_from_for_test().is_some(),
                "a reorder is in flight before Escape"
            );

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
    }

    #[test]
    fn provider_cache_is_device_local_across_remote_and_local_changes() {
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

    #[test]
    fn a_place_added_while_the_saved_places_change_still_gets_its_forecast() {
        // The place was added and its forecast requested; then the saved
        // places arrived from the document a little different (another
        // window reordered them). That fences the reply already on its way,
        // so the window must ask again rather than load forever.
        let (mut app, requests, responses) = fixture(&WeatherModel::default());
        app.initial_fetch_done = true;
        let place = crate::model::SavedLocation::new(
            "Amsterdam",
            "North Holland",
            "The Netherlands",
            "NL",
            52.37,
            4.89,
            "auto",
        );
        let id = place.id;
        app.core.borrow_mut().model.add_location(place);
        app.core.borrow_mut().refresh_selected_forecast();
        app.tick_network();
        let first: Vec<_> = requests.try_iter().collect();
        assert!(!first.is_empty());

        let mut presented = app.core.borrow().model.clone();
        presented.locations[0].order_rank += 512.0;
        app.adopt_resource_state(&presented);
        app.tick_network();
        let again: Vec<_> = requests.try_iter().collect();
        assert!(
            again.iter().any(|request| matches!(
                request,
                Request::Tracked { request, .. }
                    if matches!(request.as_ref(), Request::Forecast { location_id, .. } if *location_id == id)
            )),
            "the fenced forecast is requested again"
        );

        for request in first.iter().chain(&again) {
            responses.send(completion(request, None)).unwrap();
        }
        app.tick_network();
        assert!(app.core.borrow().model.snapshots.contains_key(&id));
        assert!(!app.core.borrow().is_fetching());

        // A place that already has a forecast is not fetched again.
        let mut presented = app.core.borrow().model.clone();
        presented.locations[0].order_rank += 512.0;
        app.adopt_resource_state(&presented);
        app.tick_network();
        assert!(requests.try_iter().next().is_none());
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
        let state = app.state().model();
        assert_eq!(state.locations.len(), 4);
        assert_eq!(state.selected, 0);
        let tokyo: LocationId = state.locations[1].id;
        app.call_on(
            &location_resource_id(tokyo),
            "use-as-default",
            serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(app.state().model().selected_location().unwrap().id, tokyo);
        app.call_on(
            &location_resource_id(tokyo),
            "move-to-top",
            serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(app.state().model().locations[0].id, tokyo);
        let removed = app.state().model().locations[3].id;
        app.call_on(
            &location_resource_id(removed),
            "remove",
            serde_json::json!({}),
        )
        .expect("Undo takes a removed place back, so removing asks nothing");
        assert_eq!(app.state().model().locations.len(), 3);
        assert!(app.undo(), "removing a place is one undo step");
        let restored = app.state().model();
        assert_eq!(restored.locations.len(), 4);
        assert_eq!(restored.locations[3].id, removed, "back where it was");
    }

    /// What an agent does to the places is history a person can read.
    #[test]
    fn history_says_what_happened_to_each_place() {
        let mut app = TestApp::<crate::WeatherShell>::capture(Some("detail")).unwrap();
        let tokyo: LocationId = app.state().model().locations[1].id;
        app.call_on(
            &location_resource_id(tokyo),
            "move-to-top",
            serde_json::json!({}),
        )
        .unwrap();
        app.call(
            "save-location",
            serde_json::json!({
                "place": { "name": "Auckland", "country": "New Zealand",
                           "latitude": -36.85, "longitude": 174.76 }
            }),
        )
        .unwrap();
        let catalogue =
            tilo_history::Catalogue::new("en-US", &[include_str!("../locales/en-US.ftl")]);
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        let sentences: Vec<String> = app
            .versions()
            .into_iter()
            .filter_map(|version| version.context)
            .filter(|entry| entry.what.is_some())
            .map(|entry| reader.sentence(&entry))
            .collect();
        assert_eq!(
            sentences.get(sentences.len().saturating_sub(2)..),
            Some(
                ["Moved Tokyo to the top", "Saved a place to follow"]
                    .map(String::from)
                    .as_slice()
            )
        );
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

#[cfg(test)]
mod morning_preview_regression {
    use super::*;
    #[test]
    fn morning_preview_is_published_from_the_real_cached_forecast() {
        let app = WeatherApp::sample();
        let rows = app.location_rows();
        assert!(!rows.is_empty());
        let row = rows[0].clone().into_resource();
        let presentation = row
            .presentation
            .expect("cached forecast publishes a bounded morning preview");
        assert!(presentation.is_valid());
        assert!(presentation.preview.unwrap().contains("observation"));
        assert!(presentation.detail.unwrap().contains("Open-Meteo"));
        assert_eq!(presentation.badge.as_deref(), Some("Capture fixture"));
    }
}
