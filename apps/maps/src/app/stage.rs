//! Staging a screenshot scenario's view state: the camera, an open panel, a
//! menu or sheet, a search on screen, a detached navigation card. The
//! scenario's places and plans are already the resource's, written by its
//! actions ([`crate::scenario`]); what is staged here is this window's alone.

use vello::kurbo::{Point, Rect};

#[cfg(test)]
use super::TravelMode;
use super::{Endpoint, FloatingSurfaceProvider, LatLon, MapInfo, MapsApp, Panel, sample};
use crate::scenario::Scenario;

impl MapsApp {
    /// Stage `name`'s view state (`None` for a plain `--shots`, the drive
    /// route), once its actions are laid out.
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
                // A narrow window shows the place card over the map, the
                // place centred above it.
                if self.overlay_mode()
                    && let Some(id) = self.selected_poi.clone()
                {
                    self.select_poi_from_list(&id);
                }
            }
            Scenario::Navigation => {
                // Under way: the map follows the start of the route at street
                // level, and the turn card leads — the panel folds to its
                // header (the drawer closes when narrow).
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

    /// An honest recoverable network failure, so visual review never treats
    /// loading and error states as interchangeable; the live app obtains
    /// this text only from a real worker failure.
    pub(super) fn stage_error(&mut self) {
        self.model.status = "Routing failed, so the route shown is an estimate".into();
        let now = ui::time::now();
        self.status_bubble.show(&self.model.status, now);
        self.status_bubble.set_hover(true, now);
    }

    /// The Directions "From" field editing, with a populated geocoded
    /// dropdown. Offline local matches only — no network.
    pub(super) fn stage_edit_origin(&mut self) {
        self.model.panel = Panel::Directions;
        self.model.origin_query = "hauptbahnhof".to_string();
        self.endpoints.begin_edit(Endpoint::Origin);
        self.endpoints
            .sync_queries(&self.model.origin_query, &self.model.dest_query);
        self.endpoints
            .set_results(Endpoint::Origin, self.local_matches("hauptbahnhof"));
    }

    /// The themed POI markers from bundled sample data, with the place card
    /// of one of them open. No network.
    pub(super) fn stage_pois(&mut self) {
        self.pois = sample::pois();
        self.selected_poi = Some("osm-node-203".to_string()); // Hofbräuhaus
        // Frame the map so the POI cluster sits in the upper map area and the
        // bottom-anchored info card is clear of it. Zoom past the icon + label
        // threshold so every marker carries its category pictogram and name.
        self.model.center = LatLon::new(48.1381, 11.5770);
        self.model.zoom = 16.7;
        self.model.selected = None;
        // Focus the Explore panel on the live NEARBY list: drop the committed
        // search/results so the nearby places lead the panel and the selected
        // POI's row is the highlighted one.
        self.model.results.clear();
        self.model.query.clear();
        self.model.panel = Panel::Explore;
    }

    /// The offline-maps sheet as the shipped map source shows it — what
    /// stays on the device and a plausible on-disk size. No network, no disk
    /// I/O.
    pub(super) fn stage_offline(&mut self) {
        self.model.panel = Panel::Explore;
        self.model.selected = None;
        self.offline_panel.open(47_542_000);
    }

    /// The panel after a committed search ("café"): its results listed
    /// nearest first, the first one selected and brought to the middle of the
    /// map, and the platform search open on the query. Offline local data
    /// only — no network.
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

    /// The production navigation renderer and its real shell → app ownership
    /// edge. Staged after layout so the `Suspended` host path uses
    /// authoritative app-local coordinates.
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

    /// Select a travel mode in the Directions panel of this window only, as
    /// tests drive it. Offline sample data already populates the
    /// route/departures, so nothing fetches.
    #[cfg(test)]
    pub(super) fn set_travel_mode(&mut self, mode: TravelMode) {
        self.model.mode = mode;
        self.model.panel = Panel::Directions;
        // A transit journey draws no road polyline; drop the sample road route
        // so the map under the departures stays clean.
        if mode.is_transit() {
            self.model.route = None;
        }
    }
}
