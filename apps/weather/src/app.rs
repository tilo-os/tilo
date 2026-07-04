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
        let width = if collapsed { OVERVIEW_RAIL_WIDTH } else { OVERVIEW_WIDTH };
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

    /// Live event-loop configuration: real saved places (coordinates), real
    /// Open-Meteo fetches, and display units read from the shared Settings
    /// store. The synthesized readings are dropped so nothing invented ever
    /// shows.
    pub fn live() -> Self {
        let mut model = crate::sample::sample_model();
        model.snapshots.clear();
        let units = crate::region::load_units();
        model.units = units;
        let core = Rc::new(RefCell::new(Core::new(model, units)));
        let app = Self::assemble(core, true);
        app.core.borrow_mut().status = "Loading…".to_string();
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
        if !reqs.is_empty() {
            if let Some(svc) = self.service.as_ref() {
                for r in reqs {
                    svc.request(r);
                }
                self.last_activity = Instant::now();
            }
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
    pub fn seed_shot_scenario(&mut self, scenario: &str) {
        match scenario {
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
                .seed_search("Par", crate::sample::sample_geocode_results()),
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
        self.nav.draw(scene, transform, text_renderer, theme, presence);
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
        self.nav
            .draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.nav.draw_overlay(scene, transform, text_renderer, theme, presence);
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
            event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy,
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

    fn name(&self) -> &str {
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
mod interaction_tests {
    //! Drive the real `handle_event` / draw path (the same code the live loop
    //! runs) and assert the model + shell view actually changed. Built with
    //! `WeatherApp::new()` so `live == false`: no network, deterministic.
    use super::*;
    use ui::widget::ContentLayout;
    use ui::widget::ShellMode;
    use vello::kurbo::Point as KPoint;
    use winit::dpi::PhysicalPosition;
    use winit::event::{
        DeviceId, ElementState, Modifiers, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent,
    };

    fn synth_device() -> DeviceId {
        unsafe { std::mem::zeroed() }
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
            let child = self.app.build_layout(&mut taffy, &self.theme, &mut node_map);
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
            self.app
                .draw(&mut scene, Affine::IDENTITY, &mut self.tr, &self.theme, &presence);
        }

        fn dispatch(&mut self, ev: WindowEvent) {
            let mods = Modifiers::default();
            let mut handled = false;
            let mut focus = None;
            self.app.handle_event(
                &ev,
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
            self.dispatch(WindowEvent::CursorMoved {
                device_id: synth_device(),
                position: PhysicalPosition::new(x, y),
            });
        }

        fn click(&mut self, x: f64, y: f64) {
            self.move_to(x, y);
            self.dispatch(WindowEvent::MouseInput {
                device_id: synth_device(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            });
            self.dispatch(WindowEvent::MouseInput {
                device_id: synth_device(),
                state: ElementState::Released,
                button: MouseButton::Left,
            });
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
        assert_eq!(h.app.nav.mode(), ShellMode::Narrow, "narrow width must reflow to push");
        assert_eq!(h.app.nav.content_layout(), ContentLayout::Push);

        // And back up — proves it isn't frozen after the first resize.
        h.layout(1280.0, 720.0);
        assert_eq!(h.app.nav.mode(), ShellMode::Wide, "widening must reflow back to side-by-side");
    }

    #[test]
    fn tapping_a_city_pushes_detail_then_back_returns() {
        let mut h = Harness::new(560.0, 720.0);
        assert_eq!(h.app.nav.content_layout(), ContentLayout::Push);
        assert!(!h.app.nav.is_detail_shown(), "starts on the full-screen city list");

        // Tap the SECOND city in the overview list.
        h.redraw(); // populate hotspots + list geometry
        let top = h.app.core.borrow().over_list_top();
        let cy = top + (62.0 + 8.0) + 62.0 / 2.0; // second chip centre
        h.click(150.0, cy);
        assert_eq!(h.app.core.borrow().model.selected, 1, "tap selects the city");
        assert!(h.app.nav.is_detail_shown(), "tap pushes the forecast in");

        // Settle the push so the Back affordance resolves, then press Back.
        h.app.nav.snap();
        assert_eq!(h.app.nav.top_left_affordance(), Affordance::Back);
        let back = h.app.nav.back_button_rect().center();
        h.click(back.x, back.y);
        assert!(!h.app.nav.is_detail_shown(), "Back returns to the city list");
    }

    #[test]
    fn wheel_scrolls_the_forecast_body() {
        let mut h = Harness::new(1280.0, 620.0);
        h.redraw();
        assert!(h.app.core.borrow().max_scroll() > 0.0, "sample forecast is taller than 620px");
        assert_eq!(h.app.core.borrow().scroll(), 0.0);
        // Point over the detail (right) panel, not the overview list.
        h.move_to(900.0, 400.0);
        h.dispatch(WindowEvent::MouseWheel {
            device_id: synth_device(),
            delta: MouseScrollDelta::LineDelta(0.0, -1.0),
            phase: TouchPhase::Moved,
        });
        assert!(h.app.core.borrow().scroll() > 0.0, "wheel scrolls the forecast body down");
    }

    #[test]
    fn live_starts_without_fabricated_snapshots() {
        let live = WeatherApp::live();
        assert!(live.live);
        {
            let core = live.core.borrow();
            assert!(!core.model.locations.is_empty(), "keeps default saved places");
            assert!(core.model.snapshots.is_empty(), "never shows invented readings on startup");
        }
        let sample = WeatherApp::new();
        assert!(!sample.core.borrow().model.snapshots.is_empty());
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
        assert!(has_forecast_req, "selecting a city queues a real forecast fetch");
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
        assert!(h.app.overview_collapsed, "the collapse chip collapses the list");

        // Re-lay-out the rebuilt shell (the real loop does this on the
        // structure-dirty signal) and confirm the forecast reclaimed width.
        h.layout(1280.0, 720.0);
        let over_w = h.app.nav.overview_rect().width();
        let detail_w = h.app.nav.detail_rect().width();
        assert!(over_w < 120.0, "collapsed overview becomes a rail (got {over_w})");
        assert!(
            detail_w > detail_w_open + 100.0,
            "collapsing reclaims width for the forecast ({detail_w_open} → {detail_w})"
        );

        // Expanding via the rail's chip restores the full list.
        h.redraw();
        let rail = h.app.nav.overview_rect();
        h.click((rail.x0 + rail.x1) / 2.0, rail.y0 + 30.0);
        assert!(!h.app.overview_collapsed, "the rail chip expands the list back");
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
        assert!(h.app.nav.overview_rect().width() < 120.0, "wide + collapsed is a rail");

        // Resize to narrow on the SAME instance: the push layout ignores the
        // rail width, so the overview is full-screen (not a frozen rail).
        h.layout(560.0, 720.0);
        assert_eq!(h.app.nav.mode(), ShellMode::Narrow);
        let ow = h.app.nav.overview_rect().width();
        assert!((ow - 560.0).abs() < 1.0, "narrow overview is full-screen even when collapsed ({ow})");

        // Back to wide → the rail returns (state preserved, still reflowing).
        h.layout(1280.0, 720.0);
        assert!(h.app.nav.overview_rect().width() < 120.0, "widening restores the rail");
    }

    #[test]
    fn picking_a_search_result_adds_and_selects_it() {
        let mut h = Harness::new(1280.0, 720.0);
        let before = h.app.core.borrow().model.locations.len();
        h.app
            .core
            .borrow_mut()
            .seed_search("Par", crate::sample::sample_geocode_results());
        h.redraw(); // build the dropdown + its result-row hotspots

        // First result row, anchored below the search field.
        let over = h.app.nav.overview_rect();
        let rx = over.x0 + 60.0;
        let ry = over.y0 + 56.0 + 38.0 + 6.0 + 6.0 + 23.0; // field + gap + pad + half row
        h.click(rx, ry);

        let model = &h.app.core.borrow().model;
        assert_eq!(model.locations.len(), before + 1, "picking a geocoded result adds the city");
        assert_eq!(
            model.selected_location().unwrap().name,
            "Paris",
            "the added city becomes selected"
        );
    }
}
