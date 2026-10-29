//! The maps window: the [`MapsApp`] the view shares with its effect
//! handler, following the shared map, with what it answers and shows beside
//! it: its effects, with their availability now, the live rows it publishes,
//! and what it adds to the platform search.

use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, Live, LiveRow, Publishes, ViewCx,
};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{AppModel, Element, FloatingSurfaceProvider, ModelPath, Widget};

use super::MapsApp;
use crate::failure;
use crate::resource::MapsEffect;
use crate::workspace::Workspace;

/// The maps window over the shared [`MapsApp`]: the map and its chrome,
/// drawn by the map, which the view's effect handler and what the view
/// publishes reach too.
pub(crate) struct MapWindow {
    app: Rc<RefCell<MapsApp>>,
    /// The shared map as the document holds it, shown as it changes.
    live: Live<Workspace>,
    /// The `(revision, loads)` of the shared map last shown.
    presented: Option<(u64, u64)>,
}

impl MapWindow {
    /// What the window shows, shared with the view's effect handler.
    pub(crate) fn app(&self) -> RefMut<'_, MapsApp> {
        self.app.borrow_mut()
    }

    /// Show the shared map the document holds now, when it changed since.
    fn follow_document(&mut self) {
        if let Some(workspace) = self.live.changed_since(&mut self.presented) {
            self.app().adopt_resource_state(&workspace);
        }
    }
}

/// The view over `app`: the map attached to the shared map's dispatcher and
/// this device's offline areas, answering the maps effects, publishing the
/// rows it shows and looking up what the platform search asks for.
pub(crate) fn window(cx: &ViewCx<Workspace>, mut app: MapsApp) -> Element {
    // A live window keeps this device's offline areas; a capture and a
    // past version show what they were staged with.
    let offline_areas = cx
        .mode()
        .is_live()
        .then(|| cx.person().zip(cx.device().map(str::to_owned)))
        .flatten();
    app.attach_framework(cx.dispatcher(), offline_areas);
    // The window opens on the shared map as it is.
    let mut presented = None;
    if let Some(workspace) = cx.live().changed_since(&mut presented) {
        app.adopt_resource_state(&workspace);
    }
    let app = Rc::new(RefCell::new(app));
    {
        let (answer, status) = (Rc::clone(&app), Rc::clone(&app));
        cx.effects(move |effect: MapsEffect, cx| answer.borrow_mut().answer_effect(effect, cx))
            .status(move || status.borrow().effect_statuses());
    }
    #[cfg(feature = "samples")]
    {
        let app = Rc::clone(&app);
        cx.on_stage(move |scenario| app.borrow_mut().stage_scenario(scenario));
    }
    let (rows, row, entries, query) = (
        Rc::clone(&app),
        Rc::clone(&app),
        Rc::clone(&app),
        Rc::clone(&app),
    );
    let window = MapWindow {
        app,
        live: cx.live().clone(),
        presented,
    };
    cx.publishing(
        Publishes::new()
            // Ranked search hits, nearby points of interest, the saved places
            // and recent searches to open again, and this device's offline
            // areas: what the window shows, with what it can do there. Not
            // document state.
            .live_rows(move || rows.borrow().published_rows())
            .live_row(move |id| row.borrow().published_row(id))
            // The places, saved first, the points of interest around the
            // view, the recent searches and the geocoder's places for the
            // latest query, each opening its row.
            .search_entries(move || entries.borrow().place_search_entries())
            // The geocoder looks up what the platform search asks for, in
            // the background.
            .search_query(move |asked| query.borrow_mut().queue_place_search(asked)),
        Box::new(window),
    )
}

/// How many rows of one kind a published page holds; the rest resolve by id.
const ROWS_PER_KIND: usize = 12;

impl MapsApp {
    /// Answer `effect`, from a control, the palette, an agent or a script.
    pub(crate) fn answer_effect(
        &mut self,
        effect: MapsEffect,
        cx: &mut EffectCx<Workspace>,
    ) -> EffectAnswer {
        match effect {
            MapsEffect::Search { query } => self.search_for(query),
            MapsEffect::Locate => {
                if let Some(reason) = self.locate_unavailable() {
                    return Err(reason.1);
                }
                self.location_reply = Some(cx.reply());
                self.use_my_location();
                return Ok(Effected::Pending);
            }
            MapsEffect::ToggleTraffic => {
                self.set_traffic(!self.map_controls.traffic_enabled());
            }
            MapsEffect::OpenPlace { place } => self.open_place(&place)?,
            // A place the geocoder found is taken in as opening it would.
            MapsEffect::Directions { place } => {
                self.open_place(&place)?;
                self.enter_directions();
            }
            MapsEffect::OpenPoi { poi } | MapsEffect::PoiDirections { poi }
                if !self.pois.iter().any(|known| known.id == poi) =>
            {
                return Err(failure::t("maps-poi-unavailable"));
            }
            MapsEffect::OpenPoi { poi } => self.select_poi(&poi),
            MapsEffect::PoiDirections { poi } => {
                self.select_poi(&poi);
                self.poi_directions();
            }
            MapsEffect::DeleteOfflineArea { area } => self.delete_region(&area)?,
            MapsEffect::ClearOfflineAreas => self.clear_offline_areas(),
        }
        Ok(Effected::done())
    }

    /// Why the location can't be asked for now, as a code and a reason.
    fn locate_unavailable(&self) -> Option<(&'static str, String)> {
        if !self.online {
            Some((
                "location_service_offline",
                failure::t("maps-location-service-offline"),
            ))
        } else if self.location_rx.is_some() || self.location_reply.is_some() {
            Some((
                "request_in_progress",
                failure::t("maps-location-request-in-progress"),
            ))
        } else {
            None
        }
    }

    /// The effects that differ from their declaration now.
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
        let mut statuses = Vec::new();
        if let Some((code, reason)) = self.locate_unavailable() {
            statuses.push(EffectStatus::unavailable(MapsEffect::LOCATE, code, reason));
        }
        if self.map_controls.traffic_enabled() {
            statuses.push(EffectStatus::labelled(
                MapsEffect::TOGGLE_TRAFFIC,
                failure::t("maps-hide-traffic"),
            ));
        }
        statuses
    }

    /// Take in the platform search's latest query: one that means a place
    /// is looked up with the geocoder in the background.
    pub(crate) fn queue_place_search(&mut self, query: &ui::search::SearchQuery) {
        self.place_search.ask(query, self.online);
    }

    /// What the window shows that the platform search finds, beside the
    /// shared map's own: see [`crate::place_search`].
    pub(crate) fn place_search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        crate::place_search::entries(&crate::place_search::Shown {
            places: &self.model.places,
            pois: &self.pois,
            recents: &self.model.recents,
            found: self.place_search.found(),
            anchor: self.search_anchor(),
        })
    }

    /// The rows the window publishes now, a bounded page of each kind.
    pub(crate) fn published_rows(&self) -> Vec<LiveRow> {
        let mut rows = Vec::new();
        rows.extend(
            self.model
                .results
                .iter()
                .take(ROWS_PER_KIND)
                .map(|place| search_result_row(&place.id, &place.name)),
        );
        rows.extend(
            self.nearby_pois()
                .into_iter()
                .take(ROWS_PER_KIND)
                .map(|poi| poi_row(&poi.id, &poi.name)),
        );
        rows.extend(
            self.model
                .places
                .iter()
                .filter(|place| place.saved)
                .take(ROWS_PER_KIND)
                .map(|place| place_row(&place.id, &place.name)),
        );
        rows.extend(
            self.model
                .recents
                .iter()
                .take(ROWS_PER_KIND)
                .map(|query| history_row(query)),
        );
        rows.extend(self.model.regions.iter().take(ROWS_PER_KIND).map(|area| {
            area_row(
                &area.id,
                &format!("{}, zoom {}–{}", area.name, area.z0, area.z1),
            )
        }));
        rows
    }

    /// The row `id`, wherever it falls in the window's state.
    pub(crate) fn published_row(&self, id: &str) -> Option<LiveRow> {
        if let Some(place_id) = id.strip_prefix("maps.search-result:") {
            return self
                .model
                .results
                .iter()
                .find(|place| place.id == place_id)
                .map(|place| search_result_row(&place.id, &place.name));
        }
        if let Some(poi_id) = id.strip_prefix("maps.poi:") {
            return self
                .pois
                .iter()
                .find(|poi| poi.id == poi_id)
                .map(|poi| poi_row(&poi.id, &poi.name));
        }
        // Any place the map knows, or the geocoder found for the platform
        // search, opens by its row; the page lists the saved ones.
        if let Some(place_id) = id.strip_prefix("maps.place:") {
            return self
                .model
                .place(place_id)
                .or_else(|| self.place_search.geocoded(place_id))
                .map(|place| place_row(&place.id, &place.name));
        }
        if let Some(query) = id.strip_prefix("maps.search-history:") {
            return self
                .model
                .recents
                .iter()
                .find(|recent| recent.as_str() == query)
                .map(|recent| history_row(recent));
        }
        if let Some(area_id) = id.strip_prefix("maps.region:") {
            return self
                .model
                .regions
                .iter()
                .find(|area| area.id == area_id)
                .map(|area| {
                    area_row(
                        &area.id,
                        &format!("{}, zoom {}–{}", area.name, area.z0, area.z1),
                    )
                });
        }
        None
    }
}

fn search_result_row(id: &str, name: &str) -> LiveRow {
    LiveRow::new(
        format!("maps.search-result:{id}"),
        "maps.search-result",
        name,
    )
    .effect(&MapsEffect::OpenPlace { place: id.into() })
    .effect(&MapsEffect::Directions { place: id.into() })
}

fn poi_row(id: &str, name: &str) -> LiveRow {
    LiveRow::new(format!("maps.poi:{id}"), "maps.poi", name)
        .effect(&MapsEffect::OpenPoi { poi: id.into() })
        .effect(&MapsEffect::PoiDirections { poi: id.into() })
}

/// A place the map knows or the geocoder found: open it, or get directions.
fn place_row(id: &str, name: &str) -> LiveRow {
    LiveRow::new(format!("maps.place:{id}"), "maps.place", name)
        .effect(&MapsEffect::OpenPlace { place: id.into() })
        .effect(&MapsEffect::Directions { place: id.into() })
}

/// Searching again joins the document's row of the same recent search.
fn history_row(query: &str) -> LiveRow {
    LiveRow::new(
        format!("maps.search-history:{query}"),
        "maps.search-history",
        query,
    )
    .effect(&MapsEffect::Search {
        query: query.into(),
    })
}

fn area_row(id: &str, label: &str) -> LiveRow {
    LiveRow::new(format!("maps.region:{id}"), "maps.region", label)
        .effect(&MapsEffect::DeleteOfflineArea { area: id.into() })
}

impl FloatingSurfaceProvider for MapWindow {
    fn take_floating_surface_request(
        &mut self,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.app().take_floating_surface_request()
    }

    fn draw_floating_surfaces(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.app()
            .draw_floating_surfaces(scene, transform, text_renderer, theme, presence);
    }

    fn floating_surface_presentations(
        &self,
    ) -> Vec<ui::floating_surface::FloatingSurfacePresentation> {
        self.app.borrow().floating_surface_presentations()
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        self.app.borrow().active_floating_surface_ids()
    }

    fn set_floating_surface_bounds(&mut self, bounds: Rect) {
        self.app().set_floating_surface_bounds(bounds);
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &ui::floating_surface::FloatingSurfaceHostMessage,
    ) {
        self.app().on_floating_surface_host_message(message);
    }
}

impl ui::widget::controls::ActionControls for MapWindow {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        ui::widget::controls::ActionControls::take_control_invocation(&mut *self.app())
    }
}

impl ui::widget::WindowHeader for MapWindow {
    fn set_history_control(&mut self, control: Option<ui::widget::HistoryControl>) {
        ui::widget::WindowHeader::set_history_control(&mut *self.app(), control);
    }
}

/// Every pass the tree runs on the window reaches the map in the shared
/// state, where the view's effect handler drives it too.
impl Widget for MapWindow {
    ui::provides!(
        dyn ui::widget::FloatingSurfaceProvider,
        dyn ui::widget::WindowHeader,
        dyn ui::widget::controls::ActionControls
    );

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.follow_document();
        self.app().sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The tree reaches the map through this window, never past the
        // shared state's borrow.
        let node = self.app().build_layout(taffy, theme, &mut HashMap::new());
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.app().apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.app()
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.app()
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.app().handle_event(event, ctx);
    }

    fn name(&self) -> &'static str {
        "MapsApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("maps_app")
    }

    fn get_rect(&self) -> Rect {
        self.app.borrow().get_rect()
    }

    fn claims_raw_touch(&self, point: Point) -> bool {
        self.app.borrow().claims_raw_touch(point)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.app.borrow().wants_pointer_capture()
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.app.borrow().modal_capture()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.app.borrow().accessibility()
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        self.app.borrow().synthetic_accessibility_rect(path)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.app()
            .perform_semantic_action(target, action, state, path)
    }

    fn needs_idle_sync(&self) -> bool {
        self.app.borrow().needs_idle_sync()
    }

    fn needs_animation_frame(&self) -> bool {
        self.app.borrow().needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.app.borrow().next_wake_deadline()
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.app().set_motion_reduced(reduced);
    }

    // Tests reach the map through the framework's view.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}
