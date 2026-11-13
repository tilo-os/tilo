//! Staging a screenshot scenario's view state — the camera, an open panel, a menu or sheet, a
//! search, a detached navigation card — once the scenario's places and plans are the resource's
//! ([`crate::scenario`]).

use vello::kurbo::{Point, Rect};

#[cfg(test)]
use super::TravelMode;
use super::{Endpoint, FloatingSurfaceProvider, LatLon, MapInfo, MapsApp, Panel, sample};
use crate::scenario::Scenario;

impl MapsApp {
    pub(crate) fn stage_scenario(&mut self, name: Option<&str>) {
        let scenario = Scenario::of(name);
        let sample = sample::default_model();
        self.model.center = sample.center;
        self.model.zoom = sample.zoom;
        self.model.selected.clone_from(&sample.selected);
        self.model.panel = Panel::Directions;
        self.nav_open = true;
        match scenario {
            Scenario::Directions | Scenario::Error | Scenario::Collapsed => {
                self.pending_fit = true;
                if scenario == Scenario::Error {
                    self.stage_error();
                }
                if scenario == Scenario::Collapsed {
                    self.panel_collapsed = true;
                }
            }
            Scenario::EditOrigin => self.stage_edit_origin(),
            Scenario::Explore => {
                self.model.panel = Panel::Explore;
                if let Some(place) = self.model.selected_place().cloned() {
                    self.center_on(place.pos);
                }
            }
            Scenario::Search => self.stage_search(),
            Scenario::Pois => {
                self.stage_pois();
                self.pending_poi_focus = true;
                if self.overlay_mode()
                    && let Some(id) = self.selected_poi.clone()
                {
                    self.select_poi_from_list(&id);
                }
            }
            Scenario::Navigation => {
                self.nav_open = false;
                self.panel_collapsed = true;
                self.model.zoom = 15.5;
                if let Some(start) = self.model.origin {
                    self.center_on(start);
                }
            }
            Scenario::FloatingNavigation => {
                self.pending_fit = true;
                let stage = name
                    .and_then(|name| name.strip_prefix("floating-navigation-"))
                    .unwrap_or("detached");
                self.stage_navigation(stage);
            }
            Scenario::Transit | Scenario::TransitArrive => {
                self.pending_fit = true;
            }
            Scenario::Traffic => {
                self.model.panel = Panel::Explore;
                self.set_traffic(true);
            }
            Scenario::Offline => self.stage_offline(),
            Scenario::Menu => {
                self.model.panel = Panel::Explore;
                self.open_overflow();
            }
            Scenario::Attribution => {
                self.model.panel = Panel::Explore;
                self.info_sheet.open(MapInfo::Attribution);
            }
            Scenario::Empty => {
                self.model.panel = Panel::Explore;
                self.model.selected = None;
            }
        }
    }

    pub(super) fn stage_error(&mut self) {
        self.model.status = "Routing failed, so the route shown is an estimate".into();
    }

    pub(super) fn stage_edit_origin(&mut self) {
        self.model.panel = Panel::Directions;
        self.model.origin_query = "hauptbahnhof".to_string();
        self.endpoints.begin_edit(Endpoint::Origin);
        self.endpoints
            .sync_queries(&self.model.origin_query, &self.model.dest_query);
        self.endpoints
            .set_results(Endpoint::Origin, self.local_matches("hauptbahnhof"));
    }

    pub(super) fn stage_pois(&mut self) {
        self.pois = sample::pois();
        self.selected_poi = Some("osm-node-203".to_string()); // Hofbräuhaus
        self.model.center = LatLon::new(48.1381, 11.5770);
        self.model.zoom = 16.7;
        self.model.selected = None;
        self.model.results.clear();
        self.model.query.clear();
        self.model.panel = Panel::Explore;
    }

    pub(super) fn stage_offline(&mut self) {
        self.model.panel = Panel::Explore;
        self.model.selected = None;
        self.offline_panel.open(47_542_000);
    }

    pub(super) fn stage_search(&mut self) {
        self.model.panel = Panel::Explore;
        self.model.query = "café".to_string();
        let results = self.ranked_matches("café");
        self.results_anchor = Some(self.search_anchor());
        self.model.selected = results.first().map(|place| place.id.clone());
        if let Some(first) = results.first() {
            self.center_on(first.pos);
        }
        self.model.results = results;
        self.search_opener
            .open(ui::search::SearchRequest::this_app().with_query("café".to_string()));
    }

    pub(super) fn stage_navigation(&mut self, stage: &str) {
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

    #[cfg(test)]
    pub(super) fn set_travel_mode(&mut self, mode: TravelMode) {
        self.model.mode = mode;
        self.model.panel = Panel::Directions;
        if mode.is_transit() {
            self.model.route = None;
        }
    }
}
