//! Root widget for the weather app.
//!
//! Weather is an **overview + detail** pair of content panels (HIG §1b), not
//! a sidebar app. The window is the shared [`ui::widget::NavShell`] with no
//! sidebar: the *contentful city list* is the overview, the forecast is the
//! content, and each has its header in the shell's one header row (the
//! list's "Places" with its Add button, the forecast's place name with its
//! Refresh and History buttons).
//!
//! * **Wide** (both columns fit): the two panels sit **side by side**.
//! * **Narrow**: the city list is full-screen; tapping a city **pushes** the
//!   forecast in from the right with a **Back** button. The shell owns that
//!   transition and the Back button.
//!
//! [`WeatherApp`] itself is thin: it holds the [`NavShell`] (whose two child
//! panes render from a shared [`Core`]), seeds/persists the CRDT, and pumps
//! the background [`WeatherService`]. Measurement units come from the shared
//! Settings store (see [`crate::region`]) — this app reads the preference and
//! never hosts a toggle for it.
//!
//! ## The capture / live split
//! The live surface is [`WeatherApp::live`]: an empty model that adopts the
//! actual saved places without rewriting their document, and fetches their
//! current Open-Meteo observations — it never renders invented data. A
//! `--shots` capture stages the surface ([`ResourceSurface::stage`]) onto
//! the bundled [`crate::sample`] places and readings instead, with the
//! network off, so the frame is fully populated, offline and deterministic.
//! Tests build [`WeatherApp::new`], which starts that way.
//!
//! [`ResourceSurface::stage`]: ui::framework::ResourceSurface::stage

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use ui::time::Instant;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ChangeHash, ObjId};
use taffy::{NodeId, TaffyTree};
use ui::a11y::{AccessNode, Politeness, Role};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::icons::Icon;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::{OperationKind, Resource};
use ui::widget::{Button, Element, NavPane, NavShell, Widget};

use crate::actions::WeatherAction;
use crate::core::{Core, ForecastNotice, NavIntent};
use crate::doc::WeatherDoc;
use crate::net::WeatherService;
use crate::panes::{DetailPane, OverviewPane};

/// The screenshot scenarios a capture can stage (`--shots --scenario NAME`).
pub const SCENARIOS: &[&str] = &[
    "onboarding",
    "detail",
    "drag",
    "menu",
    "scrolled",
    "detail_scrolled",
    "search",
    "search_loading",
    "search_empty",
    "offline",
    "error",
];

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, orthogonal flag (live network vs. bundled sample \
              data, the one-shot initial fetch, whether the shared doc has been seeded yet, \
              and whether a capture seeds it with the sample places); they aren't a state \
              machine"
)]
pub struct WeatherApp {
    core: Rc<RefCell<Core>>,
    nav: NavShell,
    /// Whether the forecast's header holds the History button.
    history_in_header: bool,
    /// Whether the shell was built with a place in the forecast column.
    shows_forecast: bool,

    /// The shell was rebuilt; the host lays the window out again.
    structure_dirty: bool,

    live: bool,
    service: Option<WeatherService>,
    initial_fetch_done: bool,
    seeded_doc: bool,
    seed_sample_document: bool,
    last_activity: Instant,
    resource_refresh_invocation: Option<tilo_actions::InvocationId>,
    resource_refresh_failure: Option<String>,
    resource_refresh_tickets: std::collections::HashSet<u64>,
    actions: crate::resource::ActionSink,
    /// Last document state folded into the render model. `ActionHost` updates
    /// this watermark eagerly; palette/agent actions are adopted on sync.
    doc_heads: Vec<ChangeHash>,
    /// A capture scenario staged after the next paint (it stands on the
    /// painted rows).
    pending_scenario: Option<String>,
    /// A scenario was staged during the last paint; it shows on the next.
    repaint_staged: bool,
}

impl WeatherApp {
    /// An empty, offline root for the History tests. The shared History
    /// conformance driver opens History through the window's own History
    /// bar, so this root leaves the History button out of its header and
    /// lets the window draw that bar.
    #[cfg(test)]
    pub(crate) fn history_test_root() -> Self {
        let mut app = Self::assemble(
            Rc::new(RefCell::new(Core::new(
                crate::model::WeatherModel::default(),
                crate::units::Units::METRIC,
            ))),
            false,
        );
        app.history_in_header = false;
        app.nav = Self::build_nav(&app.core, false);
        app
    }
    fn dispatch_durable_action(&mut self, doc: &mut AutoCommit, action: &WeatherAction) {
        let dispatched = action.clone();
        self.actions.dispatch_with_context(
            doc,
            crate::actions::WEATHER_ACTION_TYPE,
            &dispatched,
            tilo_actions::ActionContext::for_action("so.tilo.weather", "Weather", action),
        );
    }

    /// Replay every user-owned mutation reported by the core through the
    /// framework action host. The core has already updated its render model;
    /// this is the authoritative history/collaboration write.
    fn flush_durable_actions(&mut self, doc: &mut AutoCommit) -> bool {
        self.adopt_document_changes(doc);
        let actions = self.core.borrow_mut().take_durable_actions();
        if actions.is_empty() {
            return false;
        }
        for action in actions {
            self.dispatch_durable_action(doc, &action);
        }
        self.doc_heads = doc.get_heads();
        true
    }

    /// Create first-run state through the canonical action route, including
    /// the empty schema marker for a fresh live profile. Synchronisation never
    /// writes durable document fields itself.
    fn bootstrap_document(&mut self, doc: &mut AutoCommit) {
        let model = self.core.borrow().model.clone();
        self.dispatch_durable_action(doc, &WeatherAction::Initialize);
        for location in &model.locations {
            self.dispatch_durable_action(
                doc,
                &WeatherAction::AddLocation {
                    location: location.clone(),
                },
            );
        }
        if let Some(location) = model.selected_location() {
            self.dispatch_durable_action(
                doc,
                &WeatherAction::SelectLocation {
                    id: location.id.to_string(),
                },
            );
        }
        self.doc_heads = doc.get_heads();
    }

    /// Show the saved places the framework resource already holds.
    /// An empty projection does not wipe a library that has not been published yet.
    pub(crate) fn adopt_resource_state(&self, state: &crate::model::WeatherModel) {
        if state.locations.is_empty() && !self.core.borrow().model.locations.is_empty() {
            return;
        }
        let mut persisted = state.clone();
        self.core.borrow_mut().adopt_saved_places(&mut persisted);
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    ///
    /// [`ResourceSurface`]: ui::framework::ResourceSurface
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Weather>,
    ) {
        self.actions.attach(dispatcher);
    }

    /// Standalone / unit-test tree via [`tilo_resource::publish`].
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        let from_doc = WeatherDoc::read_model(doc);
        let widget = self.core.borrow().model.clone();
        // Saved places live in the document once sync has written them.
        // Forecasts stay device-local and are not part of this door.
        let model = if from_doc.locations.is_empty() && !widget.locations.is_empty() {
            widget
        } else {
            from_doc
        };
        crate::actions::provide_resources(&model, self.live)
    }

    #[cfg(test)]
    pub(crate) fn actions_are_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    #[cfg(test)]
    pub(crate) fn mark_actions_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// Adopt an action/document change while retaining device-local provider
    /// cache data. Forecasts and air-quality readings are never collaborative
    /// records, but survive a durable location update in this process.
    fn adopt_document_changes(&mut self, state: &mut AutoCommit) {
        let current_heads = state.get_heads();
        if current_heads == self.doc_heads {
            return;
        }
        let mut persisted = WeatherDoc::read_model(state);
        if self.core.borrow().model.locations != persisted.locations {
            let unsent = self.core.borrow().queued_weather_tickets_after(0);
            let mut invalidated = false;
            for ticket in unsent {
                invalidated |= self.resource_refresh_tickets.remove(&ticket);
            }
            if invalidated {
                self.resource_refresh_failure =
                    Some("Saved places changed before the refresh was dispatched".into());
            }
        }
        self.core.borrow_mut().adopt_saved_places(&mut persisted);
        self.doc_heads = current_heads;
    }

    /// The window: the city list as the overview and the forecast as the
    /// content, both rendering from the one shared [`Core`], each with its
    /// header in the shell's header row.
    /// The forecast's header offers Refresh only while it shows a place, and
    /// History when `history` (the window then draws no History bar).
    fn build_nav(core: &Rc<RefCell<Core>>, history: bool) -> NavShell {
        let add: Element = Box::new(
            Button::icon_only("weather.add-place", Icon::Plus, "Add place").on_view({
                let core = Rc::clone(core);
                move || core.borrow_mut().begin_search()
            }),
        );
        let mut actions: Vec<Element> = Vec::new();
        if Self::shows_forecast(&core.borrow()) {
            actions.push(Box::new(
                Button::icon_only("weather.refresh", Icon::Refresh, "Refresh forecast").on_effect(
                    {
                        let core = Rc::clone(core);
                        move || core.borrow_mut().refresh_selected_forecast()
                    },
                ),
            ));
        }
        if history {
            actions.push(Box::new(ui::widget::history_button()));
        }
        let title = Self::content_title(&core.borrow());
        NavShell::new()
            .with_id("weather-shell")
            .overview(Box::new(OverviewPane::new(Rc::clone(core))))
            .content(Box::new(DetailPane::new(Rc::clone(core))))
            .overview_header("Places", vec![add])
            .content_header(title, actions)
    }

    /// Whether the forecast column shows a place (and so can refresh it).
    fn shows_forecast(core: &Core) -> bool {
        core.model.selected_location().is_some()
    }

    /// The forecast column's title: the selected place's name.
    fn content_title(core: &Core) -> String {
        core.model
            .selected_location()
            .map(|location| location.name.clone())
            .unwrap_or_default()
    }

    /// Keep the forecast column's header on the selected place: its name as
    /// the title, and Refresh only while there is a place to refresh.
    fn sync_title(&mut self) {
        let shows = Self::shows_forecast(&self.core.borrow());
        if shows != self.shows_forecast {
            self.nav = Self::build_nav(&self.core, self.history_in_header);
            self.shows_forecast = shows;
            self.structure_dirty = true;
        }
        let title = Self::content_title(&self.core.borrow());
        self.nav.set_content_title(title);
    }

    fn assemble(core: Rc<RefCell<Core>>, live: bool) -> Self {
        let nav = Self::build_nav(&core, true);
        let shows_forecast = Self::shows_forecast(&core.borrow());
        Self {
            core,
            nav,
            history_in_header: true,
            shows_forecast,
            structure_dirty: false,
            live,
            service: None,
            initial_fetch_done: false,
            seeded_doc: false,
            seed_sample_document: false,
            last_activity: ui::time::now(),
            resource_refresh_invocation: None,
            resource_refresh_failure: None,
            resource_refresh_tickets: std::collections::HashSet::new(),
            actions: crate::resource::ActionSink::default(),
            doc_heads: Vec::new(),
            pending_scenario: None,
            repaint_staged: false,
        }
    }

    /// Offscreen / test configuration: bundled sample data, metric units, no
    /// network. Deterministic and fully offline.
    pub fn new() -> Self {
        let core = Rc::new(RefCell::new(Core::new(
            Self::sample_model(),
            crate::units::Units::METRIC,
        )));
        let mut app = Self::assemble(core, false);
        app.seed_sample_document = true;
        app
    }

    fn sample_model() -> crate::model::WeatherModel {
        let mut model = crate::sample::sample_model();
        model.units = crate::units::Units::METRIC;
        model
    }

    /// Switch a live surface to the bundled sample places and readings with
    /// the network off, and have the next sync publish those places through
    /// the ordinary actions — what a capture shows.
    fn use_sample_data(&mut self) {
        self.live = false;
        self.service = None;
        self.initial_fetch_done = false;
        self.seed_sample_document = true;
        self.seeded_doc = false;
        let mut core = self.core.borrow_mut();
        core.units = crate::units::Units::METRIC;
        core.model = Self::sample_model();
        core.status.clear();
        drop(core);
        self.sync_title();
    }

    /// The id of the currently selected city, if any. Lets a test observe where
    /// a search activation navigated.
    pub fn selected_city(&self) -> Option<crate::model::LocationId> {
        self.core.borrow().model.selected_location().map(|l| l.id)
    }

    /// Live event-loop configuration: the user's own saved places, real
    /// Open-Meteo fetches, and display units read from the shared Settings
    /// store. Nothing invented ever shows on a fresh launch — both the bundled
    /// sample cities and their synthesized readings are dropped, so a first run
    /// lands on the "No places yet" empty state and the user adds real cities
    /// through the Add place search. Any places they add persist to the shared
    /// document and are re-adopted by `sync` on the next launch.
    pub fn live() -> Self {
        let mut model = crate::sample::sample_model();
        model.locations.clear();
        model.snapshots.clear();
        model.air_quality.clear();
        model.selected = 0;
        let units = crate::region::load_units();
        model.units = units;
        let core = Rc::new(RefCell::new(Core::new(model, units)));
        let app = Self::assemble(core, true);
        // Only announce loading when there is actually something to fetch;
        // a place-less first run shows the empty state, not a phantom load.
        // (The persisted doc's real cities, if any, are adopted in `sync`.)
        if !app.core.borrow().model.locations.is_empty() {
            app.core.borrow_mut().status = "Loading forecasts".to_string();
        }
        app
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
            && self.seeded_doc
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
    }

    fn apply_network_response(&mut self, response: crate::net::Response) {
        if let crate::net::Response::Tracked { ticket, .. } = &response
            && self.resource_refresh_tickets.remove(ticket)
            && let Some(message) = response.weather_error()
        {
            self.resource_refresh_failure = Some(message.to_string());
        }
        self.core.borrow_mut().apply_response(response);
    }

    /// Follow the panes' navigation intent and keep the forecast's title on
    /// the selected place.
    fn follow_core(&mut self) -> bool {
        let navigate = self.core.borrow_mut().take_nav_request();
        if let Some(NavIntent::ShowDetail) = navigate {
            self.nav.show(NavPane::Content);
        }
        self.sync_title();
        navigate.is_some()
    }

    /// Seed a deterministic interaction state for a capture or a test.
    ///
    /// Recognised scenarios ([`SCENARIOS`]):
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
    /// * `"error"` — a refresh failed, with what to do about it.
    /// * `"onboarding"` — the fresh-launch empty state a live first run
    ///   shows: no saved places.
    pub fn seed_shot_scenario(&mut self, scenario: &str) {
        match scenario {
            "onboarding" => {
                let mut core = self.core.borrow_mut();
                core.model.locations.clear();
                core.model.snapshots.clear();
                core.model.air_quality.clear();
                core.model.selected = 0;
                core.status.clear();
            }
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
        self.sync_title();
    }

    /// Stage a capture: the sample places with the network off, then the
    /// named scenario's view state.
    pub(crate) fn stage_capture(&mut self, scenario: Option<&str>) {
        self.use_sample_data();
        match scenario {
            // A drag or a menu stands on the painted rows: stage it after
            // the next paint has laid them out.
            Some(name @ ("drag" | "menu")) => self.pending_scenario = Some(name.to_string()),
            Some(name) => self.seed_shot_scenario(name),
            None => {}
        }
    }

    /// Stage a scenario that waited for the rows to be painted, and ask for
    /// one more frame so the capture shows it.
    fn stage_pending_scenario(&mut self) {
        self.repaint_staged = false;
        if let Some(name) = self.pending_scenario.take() {
            self.seed_shot_scenario(&name);
            self.repaint_staged = true;
        }
    }
}

impl Default for WeatherApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for WeatherApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            crate::actions::OPEN_LOCATION_OPERATION,
            crate::actions::REFRESH_OPERATION,
            crate::actions::REFRESH_LOCATION_OPERATION,
        ]
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
        invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if self.resource_refresh_invocation != Some(invocation) {
            return None;
        }
        self.tick_network();
        if !self.resource_refresh_tickets.is_empty() {
            return None;
        }
        self.resource_refresh_invocation = None;
        Some(
            if let Some(message) = self.resource_refresh_failure.take() {
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "weather.refresh_failed".into(),
                        message,
                        details: None,
                        retryable: true,
                    },
                }
            } else {
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                }
            },
        )
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        let (state, _) = ui::doc::document(state, path);
        // Semantic actions reach the owning pane directly, so finish their
        // canonical writes and navigation here as well as on pointer input.
        let changed = self.flush_durable_actions(state);
        let navigated = self.follow_core();
        changed || navigated
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document(state, path);
        if !self.seeded_doc {
            // Only a capture or a test seeds examples. A live blank document
            // is valid; its first deliberate add creates the schema.
            if self.seed_sample_document && WeatherDoc::read_model(state).locations.is_empty() {
                self.bootstrap_document(state);
            }
            let mut persisted = WeatherDoc::read_model(state);
            self.core.borrow_mut().adopt_saved_places(&mut persisted);
            self.seeded_doc = true;
            self.doc_heads = state.get_heads();
        }

        // Direct palette/agent dispatches mutate the same document through the
        // registered WeatherAction. This also preserves a pending local fetch
        // result rather than letting it hide an unseen remote durable change.
        self.adopt_document_changes(state);
        self.sync_title();

        self.nav.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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
        self.nav
            .draw(scene, transform, text_renderer, theme, presence);
        self.stage_pending_scenario();
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
        self.nav.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
        self.stage_pending_scenario();
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
        self.adopt_document_changes(ctx.state);
        self.ensure_service();
        self.nav.handle_event(event, ctx);
        self.flush_durable_actions(ctx.state);
        self.follow_core();
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
        core_busy
            || self.pending_scenario.is_some()
            || self.repaint_staged
            || self.nav.needs_animation_frame()
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
        let rebuilt = std::mem::take(&mut self.structure_dirty);
        self.nav.take_structure_dirty() || rebuilt
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.nav.modal_capture()
    }
}

impl ui::search::capability::SearchSource for WeatherApp {
    /// Fold the complete saved-location catalogue into the unified Ctrl+K
    /// palette. [`CitySearchProvider`](crate::search_provider::CitySearchProvider)
    /// keeps every city findable by name, region, or country through bounded
    /// pages, without touching the geocoder.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(crate::search_provider::CitySearchProvider::new(
            &self.core.borrow().model.locations,
        ))]
    }
}

impl ui::resources::PublishesResources for WeatherApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// saved places, open/select/reorder/remove and refresh — no second door.
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        if self.actions.is_hosted() {
            return Vec::new();
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        if self.actions.is_hosted() && id == "weather.locations" {
            return None;
        }
        find_resource(&self.publish_resources(doc), id)
    }
}

impl ui::history_mode::capability::DocumentHistory for WeatherApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }
}

impl ui::undo::CommittedActions for WeatherApp {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl WeatherApp {
    fn answer_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == OperationKind::View && type_id == crate::actions::OPEN_LOCATION_OPERATION {
            let raw: String = serde_json::from_slice(payload).map_err(|error| error.to_string())?;
            let id = raw
                .parse()
                .map_err(|_| "Invalid weather location id".to_string())?;
            if !self.core.borrow_mut().open_location(id) {
                return Err("Weather location is no longer saved".into());
            }
            self.follow_core();
            return Ok(true);
        }
        if kind != OperationKind::Effect
            || !matches!(
                type_id,
                crate::actions::REFRESH_OPERATION | crate::actions::REFRESH_LOCATION_OPERATION
            )
        {
            return Ok(false);
        }
        if !self.live {
            return Err("network refresh is disabled for this weather view".into());
        }
        self.ensure_service();
        if type_id == crate::actions::REFRESH_LOCATION_OPERATION {
            let raw: String = serde_json::from_slice(payload).map_err(|error| error.to_string())?;
            let id = raw
                .parse()
                .map_err(|_| "Invalid weather location id".to_string())?;
            if !self.core.borrow_mut().refresh_location_by_id(id) {
                return Err("Weather location is no longer saved".into());
            }
        } else {
            self.core.borrow_mut().refresh_all();
        }
        self.last_activity = ui::time::now();
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != OperationKind::Effect
            || !matches!(
                type_id,
                crate::actions::REFRESH_OPERATION | crate::actions::REFRESH_LOCATION_OPERATION
            )
        {
            return Ok(None);
        }
        if self.resource_refresh_invocation.is_some() || self.core.borrow().is_fetching() {
            return Err("a weather refresh is already running".into());
        }
        if self.core.borrow().model.locations.is_empty() {
            return Err("save a weather location before refreshing".into());
        }
        // Startup refreshes are background work. Capture the ticket boundary
        // after they are enqueued, immediately before this operation's enqueue.
        self.ensure_service();
        let marker = self.core.borrow().request_marker();
        if !self.answer_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        self.resource_refresh_invocation = Some(invocation);
        self.resource_refresh_failure = None;
        self.resource_refresh_tickets = self.core.borrow().queued_weather_tickets_after(marker);
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }
}

fn find_resource(resources: &[Resource], id: &str) -> Option<Resource> {
    for resource in resources {
        if resource.id.as_str() == id {
            return Some(resource.clone());
        }
        if let Some(found) = find_resource(&resource.children, id) {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod interaction_tests {
    //! Drive the real `handle_event` / draw path (the same code the live loop
    //! runs) and assert the model + shell view actually changed. Built with
    //! `WeatherApp::new()` so `live == false`: no network, deterministic.
    use super::*;
    use crate::interaction::EntryAction;
    use crate::net::Request;
    use tilo_automerge::ApplyIncoming;
    use ui::headless::Framed;
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
                    WeatherApp::new(),
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

        /// The app's live document, read-only.
        fn doc(&self) -> &AutoCommit {
            &Framed::harness(&self.app).state().doc
        }

        /// The app's live document.
        fn state(&mut self) -> &mut ui::State {
            Framed::harness_mut(&mut self.app).state_mut()
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

        /// Centre-y of the saved-place row at `i`, from the live list.
        fn row_center_y(&self, i: usize) -> f64 {
            self.app.core.borrow().row_rect_for_test(i).center().y
        }

        /// Centre-x of the saved-place rows.
        fn row_center_x(&self) -> f64 {
            self.app.core.borrow().row_rect_for_test(0).center().x
        }

        /// Click the named control the way a person would.
        fn click_named(&mut self, name: &str) {
            Framed::harness_mut(&mut self.app)
                .click_named(name)
                .expect("the control is on screen");
        }

        /// How far a row must be swiped to be removed.
        fn swipe_commit() -> f64 {
            crate::interaction::swipe_commit_dx(&Theme::dark().tokens.metrics)
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn layout_reflows_and_scrolls() {
        // Case `resize_reflows_between_side_by_side_and_push`.
        {
            // The layout MUST be re-derived from the current allocated width
            // on every layout — not cached from the first frame.
            let mut h = Harness::new(1280.0, 720.0);
            let g = h.app.nav.geometry();
            assert!(g.side_by_side && g.stack.is_empty());
            assert!(g.overview_column.width() > 1.0);
            assert!(g.content_column.width() > 1.0);

            // Simulate a resize down to a narrow width on the SAME instance.
            h.layout(560.0, 720.0);
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
        // Case `wheel_scrolls_the_forecast_body`.
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
        // Case `hourly_strip_scrolls_when_it_overflows`.
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
        // Case `forecast_title_follows_the_selected_place`.
        {
            let mut h = Harness::new(1280.0, 720.0);
            h.key(Key::Named(NamedKey::ArrowDown), None);
            let second = h.app.core.borrow().model.locations[1].name.clone();
            let tree = h
                .app
                .nav
                .accessibility()
                .expect("the header row has titles");
            assert!(
                tree.children
                    .iter()
                    .any(|heading| heading.name.as_deref() == Some(second.as_str())),
                "the forecast's header names the selected place"
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
            assert!(!h.app.nav.geometry().stack.is_empty());
            assert_ne!(
                h.app.nav.front_pane(),
                NavPane::Content,
                "starts on the full-screen city list"
            );

            // Tap the SECOND city in the overview list.
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

            // Settle the push so the Back button is placed, then press Back.
            h.app.nav.snap();
            h.layout(560.0, 720.0);
            let back = h.app.nav.geometry().back;
            assert!(back.width() > 0.0, "a pushed forecast has a Back button");
            h.click_named("nav-shell-back");
            assert_ne!(
                h.app.nav.front_pane(),
                NavPane::Content,
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
            let mut pane = crate::panes::OverviewPane::new(h.app.core.clone());
            let tree = pane.accessibility().unwrap();
            let target = tree.children[0].children[1].action_target.clone().unwrap();
            let (app, state) = h.parts();
            assert!(pane.perform_semantic_action(
                &target,
                &ui::a11y::Action::Activate,
                &mut state.doc,
                &state.root,
            ));
            assert!(app.update(&mut state.doc, &state.root));
            let persisted = WeatherDoc::read_model(&state.doc);
            assert_eq!(persisted.selected_location().unwrap().id, chosen);
            assert_eq!(app.nav.front_pane(), NavPane::Content);
            assert!(!app.update(&mut state.doc, &state.root));
        }
        // Case `arrow_keys_navigate_cities_and_fetch`.
        {
            let mut h = Harness::new(1280.0, 720.0); // side-by-side; detail follows
            h.redraw();
            assert_eq!(h.app.core.borrow().model.selected, 0);
            h.app.core.borrow_mut().take_outbox(); // drain startup
            let _ = ui::undo::take_committed_action(&mut *h.app); // drain first-run bootstrap

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
            assert_eq!(
                WeatherDoc::read_model(h.doc()).selected,
                0,
                "navigating this window does not move a collaborator's durable default"
            );
            assert!(
                ui::undo::take_committed_action(&mut *h.app).is_none(),
                "transient keyboard navigation does not create shared history noise"
            );

            h.key(Key::Named(NamedKey::ArrowUp), None);
            assert_eq!(
                h.app.core.borrow().model.selected,
                0,
                "Arrow Up moves it back"
            );
        }
        // Case `header_refresh_button_queues_a_fetch`.
        {
            let mut h = Harness::new(1280.0, 720.0);
            h.redraw();
            h.app.core.borrow_mut().take_outbox(); // drain startup
            h.click_named("weather.refresh");
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
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn live_weather_starts_empty_and_fetches_only_restored_or_selected_places() {
        // Case `onboarding_scenario_matches_the_live_empty_state`.
        {
            // The `--shots onboarding` capture must exercise the SAME place-less
            // empty state a live first run shows — not a half-cleared sample.
            let mut app = WeatherApp::new();
            assert!(
                !app.core.borrow().model.locations.is_empty(),
                "sample seeds cities"
            );
            app.seed_shot_scenario("onboarding");
            let core = app.core.borrow();
            assert!(
                core.model.locations.is_empty(),
                "onboarding clears saved places"
            );
            assert!(
                core.model.snapshots.is_empty(),
                "onboarding clears readings"
            );
            assert!(
                core.model.air_quality.is_empty(),
                "onboarding clears air quality"
            );
            assert!(core.status.is_empty(), "onboarding announces no load");
        }
        // Case `a_capture_stages_the_sample_offline`.
        {
            let mut app = WeatherApp::live();
            app.stage_capture(Some("error"));
            assert!(!app.live, "a capture never touches the network");
            let core = app.core.borrow();
            assert_eq!(core.model.locations.len(), 4);
            assert!(core.model.selected_snapshot().is_some());
            assert_eq!(
                core.notice(),
                Some(crate::core::ForecastNotice::UpdateFailed)
            );
        }
        // Case `initial_refresh_waits_for_restored_locations`.
        {
            let mut app = WeatherApp::live();
            app.seeded_doc = true;
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
        // Case `live_starts_empty_with_no_fabricated_places_or_readings`.
        {
            // A fresh live launch must invent nothing: neither cities the user never
            // added nor readings for them. It lands on the empty state and adopts
            // the user's real, persisted places in `sync`.
            let live = WeatherApp::live();
            assert!(live.live);
            {
                let core = live.core.borrow();
                assert!(
                    core.model.locations.is_empty(),
                    "no fabricated saved places on startup"
                );
                assert!(
                    core.model.snapshots.is_empty(),
                    "no invented readings on startup"
                );
                assert!(
                    core.model.air_quality.is_empty(),
                    "no invented air quality on startup"
                );
                assert!(
                    core.status.is_empty(),
                    "no phantom load announced with nothing to fetch"
                );
            }
            // The bundled sample (used only by captures/tests) still carries demo
            // cities + readings, so the offline screenshot path stays populated.
            let sample = WeatherApp::new();
            assert!(!sample.core.borrow().model.locations.is_empty());
            assert!(!sample.core.borrow().model.snapshots.is_empty());
            assert!(!sample.core.borrow().model.air_quality.is_empty());
        }
        // Case `selecting_a_city_queues_a_real_fetch`.
        {
            // Proves the select path issues a network request (not a mock): the
            // outbox holds a Forecast request the app would pump to the worker.
            let mut h = Harness::new(560.0, 720.0);
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
        // Case `detail_renders_air_quality_from_real_data`.
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
            // The section extends the scrollable body.
            assert!(h.app.core.borrow().max_scroll() > 0.0);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn adding_a_place_picks_geocoded_results() {
        // Case `the_add_button_opens_the_shared_autocomplete`.
        {
            let mut h = Harness::new(1280.0, 720.0);
            assert!(!h.app.core.borrow().search_is_focused());
            h.click_named("weather.add-place");
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
        // Case `picking_a_search_result_adds_and_selects_it`.
        {
            let mut h = Harness::new(1280.0, 720.0);
            let before = h.app.core.borrow().model.locations.len();
            h.app
                .core
                .borrow_mut()
                .seed_search("Par", crate::sample::sample_geocode_results());
            h.redraw(); // place the field + populate the shared overlay

            // Arrow-key navigation of the shared overlay, then Enter to accept.
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
        // Case `arrow_nav_picks_the_disambiguated_result`.
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
        // Case `typing_builds_the_query_and_escape_ends_it`.
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
        // Case `typing_then_enter_queues_a_debounced_geocode`.
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
        // Case `right_click_menu_remove_deletes_the_city_and_persists`.
        {
            let mut h = Harness::new(560.0, 900.0);
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
            // The deletion is written through to the shared document.
            assert_eq!(
                WeatherDoc::read_model(h.doc()).locations.len(),
                before - 1,
                "the mutation persisted to the CRDT"
            );
        }
        // Case `right_click_menu_move_to_top_reorders`.
        {
            let mut h = Harness::new(560.0, 900.0);
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
        // Case `right_click_menu_refresh_queues_a_fetch_for_that_city`.
        {
            let mut h = Harness::new(560.0, 900.0);
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
        // Case `swipe_left_past_threshold_deletes_the_city`.
        {
            let mut h = Harness::new(560.0, 900.0);
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
        // Case `short_swipe_does_not_delete`.
        {
            let mut h = Harness::new(560.0, 900.0);
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

    // ---- drag-to-reorder through handle_event ---------------------------

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

            let x = h.row_center_x();
            let y0 = h.row_center_y(0);
            h.press(x, y0);
            let y2 = h.row_center_y(2);
            h.move_to(x, y2);
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
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn provider_cache_is_device_local_across_remote_and_local_changes() {
        // Case `remote_location_change_preserves_device_local_provider_cache`.
        {
            let mut h = Harness::new(1280.0, 720.0);
            let mut remote = AutoCommit::load(&h.state().doc.save()).unwrap();
            let oslo = crate::model::SavedLocation::new(
                "Oslo",
                "Oslo",
                "Norway",
                "NO",
                59.9139,
                10.7522,
                "Europe/Oslo",
            );
            WeatherDoc::upsert_location(&mut remote, &oslo);
            remote.commit();
            let changes = remote.get_changes(&h.state().doc.get_heads());
            h.state().doc.apply_incoming(changes).unwrap();

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

            let (app, state) = h.parts();
            app.sync(&mut state.doc, &state.root);

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
            drop(core);
            let persisted = WeatherDoc::read_model(h.doc());
            assert!(
                persisted
                    .locations
                    .iter()
                    .any(|location| location.id == oslo.id)
            );
            assert!(persisted.snapshots.is_empty());
            assert!(persisted.air_quality.is_empty());
        }
        // Case `location_resources_separate_view_default_and_reorder_actions`.
        {
            let mut h = Harness::new(1280.0, 720.0);
            let (app, state) = h.parts();
            let resources = ui::resources::resources(&*app, &state.doc);
            let handlers: std::collections::HashSet<String> = h
                .app
                .local_resource_operation_handlers()
                .iter()
                .map(|handler| (*handler).to_string())
                .collect();
            let mut registry = tilo_actions::ActionRegistry::default();
            crate::actions::register(&mut registry);
            let violations = tilo_actions::validate_surface(&resources, &registry, &handlers);
            assert!(violations.is_empty(), "{violations:?}");
            let root = &resources[0];
            for resource in std::iter::once(root).chain(root.children.iter()) {
                let refresh = resource
                    .actions
                    .iter()
                    .find(|action| action.id == "refresh")
                    .unwrap();
                assert_eq!(refresh.contract.result, tilo_actions::ResultSpec::Job);
                assert!(
                    refresh
                        .contract
                        .result
                        .validate(&tilo_actions::ActionOutput::default())
                        .is_ok()
                );
            }
            let first = &root.children[0];
            let second = &root.children[1];
            assert!(first.actions.iter().any(|action| action.id == "open"));
            assert!(
                first.actions.iter().any(|action| {
                    action.id == "refresh"
                        && action.operation_type.as_deref()
                            == Some(crate::actions::REFRESH_LOCATION_OPERATION)
                }),
                "each saved forecast exports a bounded per-location refresh operation"
            );
            assert!(
                first
                    .actions
                    .iter()
                    .any(|action| action.id == "use-as-default"),
                "durable default selection is not conflated with opening a view"
            );
            assert!(
                !first
                    .actions
                    .iter()
                    .any(|action| action.id == "move-to-top"),
                "the first location is already at the top"
            );
            assert!(
                second
                    .actions
                    .iter()
                    .any(|action| action.id == "move-to-top"),
                "every existing non-first UI reorder is also agent-discoverable"
            );
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

            assert_eq!(WeatherDoc::read_model(h.doc()).selected, 0);
            assert_eq!(
                h.app.core.borrow().model.snapshots[&id].current.temperature,
                31.25,
                "an action dispatch must not discard an already-renderable local forecast"
            );
            assert!(WeatherDoc::read_model(h.doc()).snapshots.is_empty());
        }
    }
}

#[cfg(test)]
mod history_regressions {
    use super::*;
    use automerge::transaction::Transactable;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn history_invocations_settle_exactly_across_restore_and_sync() {
        // Case `history_dispatched_invocation_settles_exact_real_result_after_restore`.
        {
            for failed in [false, true] {
                let mut app = WeatherApp::history_test_root();
                let mut d = AutoCommit::new();
                WeatherDoc::write_model(&mut d, &crate::sample::sample_model());
                app.sync(&mut d, &automerge::ROOT);
                let dispatched_model = crate::doc::WeatherDoc::read_model(&d);
                let sample = crate::sample::sample_model();
                let (service, requests, responses) = crate::net::test_service();
                app.service = Some(service);
                app.live = true;
                app.initial_fetch_done = true;
                let invocation = tilo_actions::InvocationId::new();
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        OperationKind::Effect,
                        crate::actions::REFRESH_OPERATION,
                        b"",
                    ),
                    &mut d,
                    &automerge::ROOT,
                )
                .unwrap();
                app.tick_network();
                let sent: Vec<_> = requests.try_iter().collect();
                assert!(!sent.is_empty());
                d.delete(automerge::ROOT, "saved_locations").unwrap();
                d.delete(automerge::ROOT, "location_order").unwrap();
                d.delete(automerge::ROOT, "location_tombstones").unwrap();
                d.get_heads();
                app.sync(&mut d, &automerge::ROOT);
                assert!(
                    app.poll_resource_operation(invocation, false, &d, &automerge::ROOT)
                        .is_none()
                );
                // A later unrelated refresh must neither delay this invocation nor alter its result.
                app.core.borrow_mut().model = crate::sample::sample_model();
                app.core.borrow_mut().refresh_all();
                app.tick_network();
                assert!(requests.try_iter().next().is_some());
                for request in sent {
                    let crate::net::Request::Tracked {
                        ticket,
                        generation,
                        request,
                    } = request
                    else {
                        panic!("ticket")
                    };
                    let response = if failed {
                        crate::net::Response::Error {
                            kind: crate::net::RequestKind::Forecast,
                            message: "actual provider failure".into(),
                        }
                    } else {
                        match *request {
                            crate::net::Request::Forecast { location_id, .. } => {
                                crate::net::Response::Forecast({
                                    let mut snap =
                                        sample.snapshots.values().next().unwrap().clone();
                                    assert!(
                                        dispatched_model
                                            .locations
                                            .iter()
                                            .any(|l| l.id == location_id)
                                    );
                                    snap.location_id = location_id;
                                    snap
                                })
                            }
                            crate::net::Request::AirQuality { location_id, .. } => {
                                crate::net::Response::AirQuality({
                                    let mut aq =
                                        sample.air_quality.values().next().unwrap().clone();
                                    assert!(
                                        dispatched_model
                                            .locations
                                            .iter()
                                            .any(|l| l.id == location_id)
                                    );
                                    aq.location_id = location_id;
                                    aq
                                })
                            }
                            _ => panic!("weather request"),
                        }
                    };
                    responses
                        .send(crate::net::Response::Tracked {
                            ticket,
                            generation,
                            response: Box::new(response),
                        })
                        .unwrap();
                }
                let outcome = app
                    .poll_resource_operation(invocation, true, &d, &automerge::ROOT)
                    .unwrap();
                if failed {
                    assert!(
                        matches!(outcome,tilo_actions::InvocationUpdate::Failed{invocation:id,error} if id==invocation && error.message=="actual provider failure")
                    );
                } else {
                    assert!(
                        matches!(outcome,tilo_actions::InvocationUpdate::Succeeded{invocation:id,..} if id==invocation)
                    );
                }
                assert!(app.core.borrow().is_fetching());
            }
        }
        // Case `history_disconnected_worker_is_failure_not_success_or_permanent_pending`.
        {
            let mut app = WeatherApp::history_test_root();
            let mut d = AutoCommit::new();
            WeatherDoc::write_model(&mut d, &crate::sample::sample_model());
            app.sync(&mut d, &automerge::ROOT);
            let (service, requests, _responses) = crate::net::test_service();
            drop(requests);
            app.service = Some(service);
            app.live = true;
            app.initial_fetch_done = true;
            let invocation = tilo_actions::InvocationId::new();
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    OperationKind::Effect,
                    crate::actions::REFRESH_OPERATION,
                    b"",
                ),
                &mut d,
                &automerge::ROOT,
            )
            .unwrap();
            assert!(matches!(
                app.poll_resource_operation(invocation, false, &d, &automerge::ROOT),
                Some(tilo_actions::InvocationUpdate::Failed { .. })
            ));
        }
        // Case `history_unsent_invocation_and_stale_durable_selection_are_invalidated_on_actual_root_sync`.
        {
            let mut app = WeatherApp::history_test_root();
            let mut d = AutoCommit::new();
            WeatherDoc::write_model(&mut d, &crate::sample::sample_model());
            app.sync(&mut d, &automerge::ROOT);
            let target = app.core.borrow().model.locations[1].id;
            app.core.borrow_mut().open_location(target);
            WeatherDoc::remove_location(&mut d, target);
            let heads = d.get_heads();
            app.flush_durable_actions(&mut d);
            assert_eq!(d.get_heads(), heads);
            assert!(
                !app.core
                    .borrow()
                    .model
                    .locations
                    .iter()
                    .any(|l| l.id == target)
            );
            let (service, requests, _responses) = crate::net::test_service();
            app.service = Some(service);
            app.live = true;
            app.initial_fetch_done = true;
            let invocation = tilo_actions::InvocationId::new();
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    OperationKind::Effect,
                    crate::actions::REFRESH_OPERATION,
                    b"",
                ),
                &mut d,
                &automerge::ROOT,
            )
            .unwrap();
            let target = app.core.borrow().model.locations[0].id;
            WeatherDoc::remove_location(&mut d, target);
            d.get_heads();
            app.sync(&mut d, &automerge::ROOT);
            assert!(
                matches!(app.poll_resource_operation(invocation,false,&d,&automerge::ROOT),Some(tilo_actions::InvocationUpdate::Failed{error,..}) if error.message.contains("before the refresh was dispatched"))
            );
            assert!(requests.try_iter().next().is_none());
        }
        // Case `history_root_declares_registration`.
        {
            assert!(
                ui::history_mode::capability::document_history_definition(&WeatherApp::new())
                    .is_some()
            );
        }
        // Case `history_blank_sync_preserves_graph`.
        {
            let mut app = WeatherApp::assemble(
                Rc::new(RefCell::new(Core::new(
                    crate::model::WeatherModel::default(),
                    crate::units::Units::METRIC,
                ))),
                false,
            );
            let mut doc = AutoCommit::new();
            doc.put(automerge::ROOT, "runtime_metadata", "keep")
                .unwrap();
            let heads = doc.get_heads();
            app.sync(&mut doc, &automerge::ROOT);
            assert_eq!(
                doc.get_heads(),
                heads,
                "ordinary sync preserves a blank graph"
            );
        }
    }
}

#[cfg(test)]
mod history_round1_tests {
    use super::*;
    use crate::net::{Request, RequestKind, Response};
    use automerge::{ROOT, transaction::Transactable};

    fn fixture() -> (
        WeatherApp,
        AutoCommit,
        std::sync::mpsc::Receiver<Request>,
        std::sync::mpsc::Sender<Response>,
    ) {
        let mut model = crate::sample::sample_model();
        model.locations.truncate(2);
        let mut doc = AutoCommit::new();
        WeatherDoc::write_model(&mut doc, &model);
        let mut app = WeatherApp::history_test_root();
        app.sync(&mut doc, &ROOT);
        let (service, requests, responses) = crate::net::test_service();
        app.service = Some(service);
        app.live = true;
        (app, doc, requests, responses)
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
    fn accepted_worker_death(restore: bool) {
        let (mut app, mut doc, requests, responses) = fixture();
        app.initial_fetch_done = true;
        let invocation = tilo_actions::InvocationId::new();
        app.start_operation(
            &ui::widget::ResourceOperation::local(
                invocation,
                OperationKind::Effect,
                crate::actions::REFRESH_OPERATION,
                b"",
            ),
            &mut doc,
            &ROOT,
        )
        .unwrap();
        app.tick_network();
        let accepted: Vec<_> = requests.try_iter().collect();
        assert_eq!(accepted.len(), 4);
        assert!(
            accepted
                .iter()
                .all(|request| matches!(request, Request::Tracked { .. }))
        );
        assert!(
            app.poll_resource_operation(invocation, false, &doc, &ROOT)
                .is_none()
        );
        if restore {
            for key in ["saved_locations", "location_order", "location_tombstones"] {
                doc.delete(ROOT, key).unwrap();
            }
            doc.get_heads();
            app.sync(&mut doc, &ROOT);
        }
        drop(requests);
        drop(responses);
        assert!(
            matches!(app.poll_resource_operation(invocation,false,&doc,&ROOT),Some(tilo_actions::InvocationUpdate::Failed{invocation:id,error}) if id==invocation && error.message.contains("disconnected")),
            "accepted request must fail when its worker dies; restore={restore}"
        );
        assert!(!app.core.borrow().is_fetching());
        assert!(app.resource_refresh_tickets.is_empty());
        assert!(app.core.borrow().disconnected_responses().is_empty());
        assert!(
            app.poll_resource_operation(invocation, false, &doc, &ROOT)
                .is_none(),
            "terminal outcome emitted exactly once"
        );
        app.tick_network();
        assert!(
            app.core.borrow_mut().take_outbox().is_empty(),
            "disconnected requests are not retried"
        );
    }
    #[test]
    fn worker_death_and_batches_settle_per_place_invocations_once() {
        // Case `accepted_worker_death_settles_once`.
        {
            accepted_worker_death(false);
        }
        // Case `accepted_worker_death_after_restore_settles_once`.
        {
            accepted_worker_death(true);
        }
        // Case `buffered_real_completions_survive_terminal_channel_close`.
        {
            for complete_all in [false, true] {
                let (mut app, mut doc, requests, responses) = fixture();
                app.initial_fetch_done = true;
                let invocation = tilo_actions::InvocationId::new();
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        OperationKind::Effect,
                        crate::actions::REFRESH_OPERATION,
                        b"",
                    ),
                    &mut doc,
                    &ROOT,
                )
                .unwrap();
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
                let outcome = app.poll_resource_operation(invocation, false, &doc, &ROOT);
                if complete_all {
                    assert!(
                        matches!(outcome, Some(tilo_actions::InvocationUpdate::Succeeded { invocation:id, .. }) if id==invocation)
                    );
                } else {
                    assert!(
                        matches!(outcome, Some(tilo_actions::InvocationUpdate::Failed { invocation:id, error }) if id==invocation && error.message.contains("disconnected"))
                    );
                }
                assert!(
                    app.core.borrow().model.snapshots.contains_key(&target),
                    "real buffered forecast retained"
                );
                assert!(!app.core.borrow().is_fetching());
                assert!(app.core.borrow().disconnected_responses().is_empty());
                assert!(
                    app.poll_resource_operation(invocation, false, &doc, &ROOT)
                        .is_none()
                );
            }
        }
        // Case `startup_failure_and_later_batch_do_not_own_per_place_invocation`.
        {
            startup_ownership(true, false);
        }
        // Case `startup_pending_and_later_batch_do_not_delay_per_place_invocation`.
        {
            startup_ownership(false, false);
        }
        // Case `own_failure_remains_failure_with_unrelated_pending_work`.
        {
            startup_ownership(false, true);
        }
    }

    fn startup_ownership(background_failure: bool, own_failure: bool) {
        let (mut app, mut doc, requests, responses) = fixture();
        assert!(!app.initial_fetch_done);
        let target = app.core.borrow().model.locations[0].id;
        let other = app.core.borrow().model.locations[1].id;
        let payload = serde_json::to_vec(&target.to_string()).unwrap();
        let invocation = tilo_actions::InvocationId::new();
        app.start_operation(
            &ui::widget::ResourceOperation::local(
                invocation,
                OperationKind::Effect,
                crate::actions::REFRESH_LOCATION_OPERATION,
                &payload,
            ),
            &mut doc,
            &ROOT,
        )
        .unwrap();
        // This is ordinary interactive work queued after acceptance but before
        // the first dispatch, in addition to the initial four startup requests.
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
        let outcome = app.poll_resource_operation(invocation, false, &doc, &ROOT);
        if own_failure {
            assert!(
                matches!(outcome,Some(tilo_actions::InvocationUpdate::Failed{invocation:id,error}) if id==invocation && error.message=="own provider failure")
            );
        } else {
            assert!(
                matches!(outcome,Some(tilo_actions::InvocationUpdate::Succeeded{invocation:id,..}) if id==invocation),
                "only the requested place's requests belong to this invocation"
            );
        }
        assert!(
            app.core.borrow().is_fetching(),
            "unrelated queued/sent work remains outstanding"
        );
        // Duplicate owned responses must not settle background work or emit a
        // second terminal outcome. Real background completions still apply.
        for r in &sent[4..6] {
            responses.send(completion(r, None)).unwrap();
        }
        assert!(
            app.poll_resource_operation(invocation, false, &doc, &ROOT)
                .is_none()
        );
        app.tick_network();
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
