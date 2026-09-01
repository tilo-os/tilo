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
//! panes render from a shared [`Core`]), seeds/persists the CRDT, and pumps
//! the background [`WeatherService`]. Measurement units come from the shared
//! Settings store (see [`crate::region`]) — this app reads the preference and
//! never hosts a toggle for it.
//!
//! ## The `--shots` / live split
//! `--shots` and tests build [`WeatherApp::new`]: seeded with bundled
//! [`crate::sample`] snapshots, `live == false`, so the network worker is
//! never spawned and the frame is fully populated and offline. The live
//! binary builds [`WeatherApp::live`]: the same default places (real
//! coordinates) but with the synthesized snapshots dropped, fetching real
//! Open-Meteo forecasts on the first frame — it never renders invented data.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::qos_actions::{ActionSpec, OperationKind, Resource};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Affordance, NavShell, ShellMode, Widget};

use crate::core::{Core, NavIntent};
use crate::doc::WeatherDoc;
use crate::net::WeatherService;
use crate::panes::{DetailPane, OverviewPane};

/// Width of the city-list (overview) panel in the side-by-side layout.
const OVERVIEW_WIDTH: f64 = 340.0;
/// Width of the collapsed overview **rail** (glanceable icon column). It sits
/// below [`crate::core`]'s rail threshold, so allocating this width switches
/// the overview to its rail rendering with no separate flag to keep in sync.
const OVERVIEW_RAIL_WIDTH: f64 = 68.0;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, orthogonal flag (rail-collapsed overview, a \
              pending shell rebuild, live network vs. bundled sample data, the one-shot \
              initial fetch, and whether the shared doc has been seeded yet); they aren't a \
              state machine, and grouping them would mean churning every field access \
              across this widget for no behavioural gain"
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

    live: bool,
    service: Option<WeatherService>,
    initial_fetch_done: bool,
    seeded_doc: bool,
    last_activity: Instant,
}

impl WeatherApp {
    /// Build the responsive shell at the overview width implied by `collapsed`
    /// (full list vs. rail). Both pane widgets share the one [`Core`].
    fn build_nav(core: Rc<RefCell<Core>>, collapsed: bool) -> NavShell {
        let overview: Box<dyn Widget> = Box::new(OverviewPane::new(core.clone()));
        let detail: Box<dyn Widget> = Box::new(DetailPane::new(core));
        let width = if collapsed {
            OVERVIEW_RAIL_WIDTH
        } else {
            OVERVIEW_WIDTH
        };
        NavShell::new(overview, detail).with_overview_width(width)
    }

    fn assemble(core: Rc<RefCell<Core>>, live: bool) -> Self {
        let nav = Self::build_nav(core.clone(), false);
        Self {
            core,
            nav,
            overview_collapsed: false,
            structure_dirty: false,
            live,
            service: None,
            initial_fetch_done: false,
            seeded_doc: false,
            last_activity: Instant::now(),
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

    /// Offscreen / test configuration: bundled sample data, metric units, no
    /// network. Deterministic and fully offline.
    pub fn new() -> Self {
        let mut model = crate::sample::sample_model();
        let units = crate::units::Units::METRIC;
        model.units = units;
        let core = Rc::new(RefCell::new(Core::new(model, units)));
        Self::assemble(core, false)
    }

    /// The id of the currently selected city, if any. Lets a test observe where
    /// a Ctrl+K activation navigated.
    pub fn selected_city(&self) -> Option<crate::model::LocationId> {
        self.core.borrow().model.selected_location().map(|l| l.id)
    }

    /// Live event-loop configuration: the user's own saved places, real
    /// Open-Meteo fetches, and display units read from the shared Settings
    /// store. Nothing invented ever shows on a fresh launch — both the bundled
    /// sample cities and their synthesized readings are dropped, so a first run
    /// lands on the "No places yet — search above" onboarding and the user adds
    /// real cities through the geocoding search. Any places they add persist to
    /// the shared document and are re-adopted by `sync` on the next launch.
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
        // Only advertise "Loading…" when there is actually something to fetch;
        // a place-less first run shows the onboarding, not a phantom spinner.
        // (The persisted doc's real cities, if any, are adopted in `sync`.)
        if !app.core.borrow().model.locations.is_empty() {
            app.core.borrow_mut().status = "Loading…".to_string();
        }
        app
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
        if !self.initial_fetch_done {
            self.core.borrow_mut().refresh_all();
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

        let reqs = self.core.borrow_mut().take_outbox();
        if !reqs.is_empty()
            && let Some(svc) = self.service.as_ref()
        {
            for r in reqs {
                svc.request(r);
            }
            self.last_activity = Instant::now();
        }
        if let Some(svc) = self.service.as_ref() {
            let resps = svc.drain();
            if !resps.is_empty() {
                let mut core = self.core.borrow_mut();
                for r in resps {
                    core.apply_response(r);
                }
            }
        }
    }

    /// Set the detail pane's Back-affordance reservation from the shell state
    /// (only the shell knows whether a Back chip occupies the corner).
    fn sync_reserve_back(&self) {
        let reserve = matches!(self.nav.top_left_affordance(), Affordance::Back);
        self.core.borrow_mut().set_detail_reserve_back(reserve);
    }

    /// Seed a deterministic interaction state for a headless screenshot.
    ///
    /// Recognised scenarios:
    /// * `"detail"` — push the forecast in (pair with a narrow `--viewport`
    ///   to capture the pushed detail + the Back button).
    /// * `"drag"` — a reorder drag of the second saved place is in flight.
    /// * `"scrolled"` — the forecast parked at the bottom.
    /// * `"collapsed"` — the city list collapsed to its rail (wide viewport).
    /// * `"search"` — the live-search dropdown showing geocoded matches.
    /// * `"search_loading"` — the dropdown's in-flight loading state.
    /// * `"search_empty"` — the dropdown's "no matches" state.
    /// * `"onboarding"` — the fresh-launch empty state a live first run shows:
    ///   no saved places (the sample cities + readings dropped), inviting the
    ///   user to search. Mirrors what [`WeatherApp::live`] renders on first run.
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
                self.nav.show_detail();
                self.nav.snap();
            }
            "drag" => self.core.borrow_mut().seed_drag(),
            "scrolled" => self.core.borrow_mut().seed_scrolled(),
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
            _ => {}
        }
    }
}

impl Default for WeatherApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for WeatherApp {
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        let core = self.core.borrow();
        let mut root = Resource::new("weather", "weather.locations", "Weather");
        if let Ok(spec) = ActionSpec::operation(
            "refresh",
            "Refresh forecasts",
            OperationKind::Effect,
            "weather.refresh",
            &"",
        ) {
            root.actions.push(spec);
        }
        root.children
            .extend(core.model.locations.iter().map(|location| {
                Resource::new(
                    format!("location:{}", location.id),
                    "weather.location",
                    location.name.clone(),
                )
            }));
        vec![root]
    }

    fn invoke_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        _payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind != OperationKind::Effect || type_id != "weather.refresh" {
            return Ok(false);
        }
        if !self.live {
            return Err("network refresh is disabled for this weather view".into());
        }
        self.ensure_service();
        self.core.borrow_mut().refresh_all();
        self.last_activity = Instant::now();
        Ok(true)
    }

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.seeded_doc {
            {
                let mut core = self.core.borrow_mut();
                if WeatherDoc::is_initialised(state) {
                    let persisted = WeatherDoc::read_model(state);
                    if !persisted.locations.is_empty() {
                        let mut merged = persisted;
                        for (id, snap) in core.model.snapshots.drain() {
                            merged.snapshots.entry(id).or_insert(snap);
                        }
                        for (id, aq) in core.model.air_quality.drain() {
                            merged.air_quality.entry(id).or_insert(aq);
                        }
                        // Units are a system preference (Settings), not doc
                        // state — keep the region-derived value.
                        merged.units = core.units;
                        core.model = merged;
                    }
                } else {
                    WeatherDoc::write_model(state, &core.model);
                }
            }
            self.seeded_doc = true;
            if self.live {
                self.ensure_service();
            }
        }

        self.tick_network();
        {
            let mut core = self.core.borrow_mut();
            if core.pending_doc_write {
                WeatherDoc::write_model(state, &core.model);
                core.pending_doc_write = false;
            }
        }
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        self.ensure_service();
        self.nav.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
        );

        // The overview raises a push intent when a city is selected.
        if let Some(NavIntent::ShowDetail) = self.core.borrow_mut().take_nav_request() {
            self.nav.show_detail();
        }

        // The collapse / expand chip rebuilds the shell at the new width.
        if self.core.borrow_mut().take_collapse_toggle() {
            self.set_overview_collapsed(!self.overview_collapsed);
        }

        self.tick_network();
        let mut core = self.core.borrow_mut();
        if core.pending_doc_write {
            WeatherDoc::write_model(state, &core.model);
            core.pending_doc_write = false;
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.nav]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.nav]
    }

    /// Fold the saved cities into the unified Ctrl+K palette: a snapshot
    /// [`CitySearchProvider`](crate::search_provider::CitySearchProvider) makes
    /// every saved city findable there by name, region, or country, without
    /// touching the geocoder.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(crate::search_provider::CitySearchProvider::new(
            &self.core.borrow().model.locations,
        ))]
    }

    /// Open the saved city a chosen palette row identifies: select it and push
    /// its forecast into view, the same path a tap on the city list takes.
    fn activate_search_result(&mut self, action_id: &str, _doc: &mut AutoCommit, _path: &ObjId) {
        if let Some(id) = crate::search_provider::CitySearchProvider::parse_action_id(action_id)
            && self.core.borrow_mut().open_location(id)
            && let Some(NavIntent::ShowDetail) = self.core.borrow_mut().take_nav_request()
        {
            self.nav.show_detail();
        }
    }

    fn name(&self) -> &'static str {
        "WeatherApp"
    }

    fn get_rect(&self) -> Rect {
        self.nav.get_rect()
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
        app: WeatherApp,
        taffy: TaffyTree<()>,
        node: NodeId,
        state: ui::State,
        theme: Theme,
        tr: TextRenderer,
        w: f64,
        h: f64,
    }

    impl Harness {
        fn new(w: f64, h: f64) -> Self {
            let app = WeatherApp::new();
            let mut harness = Self {
                app,
                taffy: TaffyTree::new(),
                node: NodeId::new(0),
                state: ui::State::new(),
                theme: Theme::dark(),
                tr: TextRenderer::new(),
                w,
                h,
            };
            harness.layout(w, h);
            harness.redraw();
            harness
        }

        /// Full sync → build → compute → apply cycle at a size, so the same
        /// instance can be re-laid-out to simulate a window resize.
        fn layout(&mut self, w: f64, h: f64) {
            use taffy::prelude::*;
            self.w = w;
            self.h = h;
            self.app.sync(&mut self.state.doc, &self.state.root);
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let child = self
                .app
                .build_layout(&mut taffy, &self.theme, &mut node_map);
            let root = taffy
                .new_with_children(
                    Style {
                        size: Size {
                            width: Dimension::length(w as f32),
                            height: Dimension::length(h as f32),
                        },
                        ..Default::default()
                    },
                    &[child],
                )
                .unwrap();
            let measure_map = node_map.clone();
            let tr = &mut self.tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    Size {
                        width: AvailableSpace::Definite(w as f32),
                        height: AvailableSpace::Definite(h as f32),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, tr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
            self.app.apply_layout(&taffy, child, Point::ORIGIN);
            self.taffy = taffy;
            self.node = child;
        }

        fn redraw(&mut self) {
            let presence = PresenceCtx::empty();
            let mut scene = Scene::new();
            self.app.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut self.tr,
                &self.theme,
                &presence,
            );
        }

        fn dispatch(&mut self, ev: &WindowEvent) {
            let mods = Modifiers::default();
            let mut handled = false;
            let mut focus = None;
            self.app.handle_event(
                ev,
                1.0,
                &mods,
                &mut self.state.doc,
                &self.state.root,
                &mut handled,
                &mut focus,
                self.node,
                &self.taffy,
            );
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
    fn resize_reflows_between_breakpoints() {
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

    #[test]
    fn tapping_a_city_pushes_detail_then_back_returns() {
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

    #[test]
    fn wheel_scrolls_the_forecast_body() {
        let mut h = Harness::new(1280.0, 620.0);
        h.redraw();
        assert!(
            h.app.core.borrow().max_scroll() > 0.0,
            "sample forecast is taller than 620px"
        );
        assert_eq!(h.app.core.borrow().scroll(), 0.0);
        // Point over the detail (right) panel, not the overview list.
        h.move_to(900.0, 400.0);
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

    #[test]
    fn onboarding_scenario_matches_the_live_empty_state() {
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
        assert!(core.status.is_empty(), "onboarding shows no spinner");
    }

    #[test]
    fn live_starts_empty_with_no_fabricated_places_or_readings() {
        // A fresh live launch must invent nothing: neither cities the user never
        // added nor readings for them. It lands on the onboarding empty state and
        // adopts the user's real, persisted places in `sync`.
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
                "no phantom Loading spinner with nothing to fetch"
            );
        }
        // The bundled sample (used only by `--shots`/tests) still carries demo
        // cities + readings, so the offline screenshot path stays populated.
        let sample = WeatherApp::new();
        assert!(!sample.core.borrow().model.locations.is_empty());
        assert!(!sample.core.borrow().model.snapshots.is_empty());
        assert!(!sample.core.borrow().model.air_quality.is_empty());
    }

    #[test]
    fn selecting_a_city_queues_a_real_fetch() {
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

    #[test]
    fn collapsing_the_city_list_reclaims_forecast_width() {
        let mut h = Harness::new(1280.0, 720.0);
        assert_eq!(h.app.nav.content_layout(), ContentLayout::SideBySide);
        let detail_w_open = h.app.nav.detail_rect().width();
        let over = h.app.nav.overview_rect();
        assert!(over.width() > 300.0, "expanded list is full-width");

        // The draw pass sets `overview_collapsible` and builds the chip's
        // hotspot; click the collapse chip (top-right of the overview panel).
        h.redraw();
        let cx = over.x1 - 35.0; // chip spans x+w-30..x+w, RP=20 → over.x1-50..-20
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

    #[test]
    fn collapsed_rail_is_not_frozen_across_a_resize() {
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

    #[test]
    fn city_search_uses_the_shared_autocomplete_overlay() {
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

    #[test]
    fn picking_a_search_result_adds_and_selects_it() {
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
            Some("Paris"),
            "Arrow Down highlights the first suggestion"
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

    #[test]
    fn arrow_nav_picks_the_disambiguated_result() {
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

    #[test]
    fn hourly_strip_scrolls_when_it_overflows() {
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

    #[test]
    fn detail_renders_air_quality_from_real_data() {
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

    // ---- right-click context menu (open → dispatch item) ----------------

    #[test]
    fn right_click_menu_delete_removes_the_city_and_persists() {
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
        // The deletion is written through to the shared document.
        assert_eq!(
            WeatherDoc::read_model(&h.state.doc).locations.len(),
            before - 1,
            "the mutation persisted to the CRDT"
        );
    }

    #[test]
    fn right_click_menu_move_to_top_reorders() {
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

    #[test]
    fn right_click_menu_refresh_queues_a_fetch_for_that_city() {
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
        let queued =
            h.app.core.borrow_mut().take_outbox().iter().any(
                |r| matches!(r, Request::Forecast { location_id, .. } if *location_id == target),
            );
        assert!(
            queued,
            "menu Refresh queues a real forecast fetch for that city"
        );
    }

    // ---- swipe-to-delete + drag-to-reorder through handle_event ---------

    #[test]
    fn swipe_left_past_threshold_deletes_the_city() {
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

    #[test]
    fn short_swipe_does_not_delete() {
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

    #[test]
    fn vertical_drag_reorders_the_city() {
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

    #[test]
    fn drag_reflows_the_list_live_and_commits_only_on_drop() {
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

    #[test]
    fn escape_cancels_an_in_flight_reorder_and_restores_order() {
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

    #[test]
    fn dragging_onto_the_delete_zone_deletes() {
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

    // ---- keyboard: arrow nav, '/' focus, Escape, geocode-on-typing ------

    #[test]
    fn arrow_keys_navigate_cities_and_fetch() {
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
        assert_eq!(
            WeatherDoc::read_model(&h.state.doc).selected,
            1,
            "the new selection persisted to the CRDT"
        );

        h.key(Key::Named(NamedKey::ArrowUp), None);
        assert_eq!(
            h.app.core.borrow().model.selected,
            0,
            "Arrow Up moves it back"
        );
    }

    #[test]
    fn slash_focuses_search_and_escape_clears_it() {
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

    #[test]
    fn typing_then_enter_queues_a_debounced_geocode() {
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

        let queued = h
            .app
            .core
            .borrow_mut()
            .take_outbox()
            .iter()
            .any(|r| matches!(r, Request::Geocode { query, .. } if query.as_str() == "Par"));
        assert!(
            queued,
            "typing then Enter queues a real geocode for the typed query"
        );
    }

    // ---- detail Refresh button ------------------------------------------

    #[test]
    fn detail_refresh_button_queues_a_fetch() {
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
}
