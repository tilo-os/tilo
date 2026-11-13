//! Directions: the endpoints and where they come from, the route and its transit variants, when it
//! is replanned, and the framing that fits it.

use super::{
    Endpoint, EndpointEffect, LatLon, MapsApp, NetMsg, Panel, Place, Point, Projection, Rect,
    TravelMode, Workspace, failure, geo, location, net, spawn_net, workspace,
};

impl MapsApp {
    /// The origin: a set origin, else the map centre labelled as such. Never a specific location
    /// without a real fix.
    pub(super) fn resolve_origin(&mut self) -> crate::model::LatLon {
        if let Some(o) = self.model.origin {
            o
        } else {
            self.model.origin_name = failure::t("maps-map-centre");
            self.model.center
        }
    }

    pub(super) fn apply_endpoint_effect(&mut self, effect: EndpointEffect) {
        match effect {
            EndpointEffect::Changed { endpoint, text } => {
                match endpoint {
                    Endpoint::Origin => text.clone_into(&mut self.model.origin_query),
                    Endpoint::Destination => text.clone_into(&mut self.model.dest_query),
                }
                let results = self.local_matches(&text);
                self.endpoints.set_results(endpoint, results);
            }
            EndpointEffect::Search { endpoint, query } => {
                self.run_endpoint_search(endpoint, query);
            }
            EndpointEffect::Pick { endpoint, place } => self.pick_endpoint(endpoint, &place),
            EndpointEffect::Swap => self.swap_ends(),
            EndpointEffect::Handled => {}
        }
    }

    pub(super) fn run_endpoint_search(&mut self, endpoint: Endpoint, query: String) {
        let local_results = self.local_matches(&query);
        self.endpoints.set_results(endpoint, local_results.clone());
        if query.trim().is_empty() {
            return;
        }
        if self.online {
            self.pending += 1;
            let tx = self.net_tx.clone();
            let origin = endpoint == Endpoint::Origin;
            spawn_net(async move {
                let results = net::geocode(&query).await;
                let _ = tx.send(NetMsg::EndpointSearch {
                    origin,
                    query,
                    results,
                });
            });
        } else {
            self.endpoints.finish_search(endpoint, local_results);
        }
    }

    pub(super) fn pick_endpoint(&mut self, endpoint: Endpoint, p: &Place) {
        self.model.upsert_place(p.clone());
        if endpoint == Endpoint::Origin {
            self.model.origin = Some(p.pos);
            p.name.clone_into(&mut self.model.origin_name);
            self.model.origin_query.clear();
        } else {
            self.model.dest = Some(p.pos);
            p.name.clone_into(&mut self.model.dest_name);
            self.model.selected = Some(p.id.clone());
            self.model.dest_query.clear();
        }
        self.model.center = p.pos;
        if self.model.mode.is_transit() {
            self.recompute_transit();
        } else {
            self.recompute_route();
        }
    }

    /// Ask the portal (`GeoClue2`) for a real fix, off the UI thread. Honest states only:
    /// `Locating…`, then a fix or an unavailable message.
    pub(super) fn use_my_location(&mut self) {
        if self.locating {
            return;
        }
        self.locating = true;
        self.model.status = failure::t("maps-locating");
        if self.online {
            self.location_rx = Some(location::request_location());
        }
    }

    /// The picker's anchor: `Leave now` sends no time; otherwise an RFC-3339 wall clock at `now +
    /// offset`, flagged depart or arrive.
    pub(super) fn transit_query(&self) -> net::TransitQuery {
        let tt = self.model.transit_time;
        let time = if tt.is_now() {
            None
        } else {
            let when = ui::clock::local_now() + chrono::Duration::minutes(i64::from(tt.offset_min));
            Some(when.to_rfc3339())
        };
        net::TransitQuery {
            arrive_by: tt.arrive_by(),
            time,
        }
    }

    pub(super) fn enter_directions(&mut self) {
        self.set_dest_to_selection();
        self.model.panel = Panel::Directions;
        if self.model.mode.is_transit() {
            self.recompute_transit();
        } else {
            self.recompute_route();
        }
    }

    pub(super) fn enter_transit(&mut self) {
        self.set_dest_to_selection();
        self.choose_mode(TravelMode::Transit);
        self.recompute_transit();
    }

    pub(super) fn recompute_route(&mut self) {
        if self.model.dest.is_none() {
            self.set_dest_to_selection();
        }
        let Some(dest) = self.model.dest else {
            return;
        };
        let origin = self.resolve_origin();
        self.model.origin = Some(origin);
        self.nav_open = true;
        let mode = self.model.mode;
        self.model.route = Some(crate::basemap::fallback_route(origin, dest, mode));
        self.fit_route();

        self.pending += 1;
        self.model.status = failure::t("maps-routing");
        if self.online {
            let tx = self.net_tx.clone();
            spawn_net(async move {
                let r = net::routes(origin, dest, mode).await;
                let _ = tx.send(NetMsg::Route(r));
            });
        }
    }

    pub(super) fn replan_for_time(&mut self) {
        if self.model.mode.is_transit() {
            self.recompute_transit();
        }
    }

    pub(super) fn recompute_transit(&mut self) {
        if self.model.dest.is_none() {
            self.set_dest_to_selection();
        }
        let Some(dest) = self.model.dest else {
            return;
        };
        let origin = self.resolve_origin();
        self.model.origin = Some(origin);
        self.model.panel = Panel::Directions;
        self.model.mode = TravelMode::Transit;
        self.model.transit_choice = 0;
        self.model.route = None;
        self.nav_open = true;
        self.model.transit.clear();
        self.pending += 1;
        self.model.status = failure::t("maps-finding-departures");
        self.fit_points(&[origin, dest]);
        if self.online {
            let tx = self.net_tx.clone();
            let query = self.transit_query();
            spawn_net(async move {
                let r = net::transit(origin, dest, &query).await;
                let _ = tx.send(NetMsg::Transit(r));
            });
        }
    }

    pub(super) fn set_mode(&mut self, mode: TravelMode) {
        if self.model.mode != mode {
            self.choose_mode(mode);
            self.model.panel = Panel::Directions;
            if mode.is_transit() {
                self.recompute_transit();
            } else {
                self.recompute_route();
            }
        }
    }

    pub(super) fn next_route(&mut self) {
        if self.model.route_alternatives.len() < 2 {
            return;
        }
        let shown = self
            .model
            .route
            .as_ref()
            .and_then(|route| {
                self.model
                    .route_alternatives
                    .iter()
                    .position(|offered| offered.is_same_trip(route))
            })
            .unwrap_or(0);
        let next = (shown + 1) % self.model.route_alternatives.len();
        let Some(route) = self.model.route_alternatives.get(next).cloned() else {
            return;
        };
        self.model.route = Some(route.clone());
        self.perform(Workspace::ACCEPT_ROUTE.with(workspace::AcceptRoute {
            route,
            origin: self.model.origin,
            origin_name: self.model.origin_name.clone(),
            dest: self.model.dest,
            dest_name: self.model.dest_name.clone(),
            mode: self.model.mode,
        }));
        self.fit_route();
        self.model.status = failure::t_args(
            "maps-route-number",
            &[
                ("number", i64::try_from(next + 1).unwrap_or(1).into()),
                (
                    "count",
                    i64::try_from(self.model.route_alternatives.len())
                        .unwrap_or(1)
                        .into(),
                ),
            ],
        );
    }

    pub(super) fn clear_route(&mut self) {
        if self.model.route.take().is_some() {
            self.perform(Workspace::CLEAR_ROUTE.invoke());
        }
        self.navigation_placement.reset_attached();
        self.navigation_drag_handle = Rect::ZERO;
        self.navigation_end_rect = Rect::ZERO;
        self.navigation_end_hovered = false;
    }

    pub(super) fn swap_ends(&mut self) {
        if self.model.origin.is_none() || self.model.dest.is_none() {
            return;
        }
        std::mem::swap(&mut self.model.origin, &mut self.model.dest);
        std::mem::swap(&mut self.model.origin_name, &mut self.model.dest_name);
        if self.model.mode.is_transit() {
            self.recompute_transit();
        } else {
            self.recompute_route();
        }
    }

    pub(super) fn fit_route(&mut self) {
        if let Some(route) = self.model.route.clone() {
            self.fit_points(&route.geometry);
        }
    }

    pub(super) fn selected_journey_geometry(&self) -> Vec<LatLon> {
        self.model
            .transit
            .get(self.model.transit_choice)
            .map(|j| {
                j.legs
                    .iter()
                    .flat_map(|l| l.geometry.iter().copied())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Frame what is shown: a road polyline, else the selected itinerary's legs, else the
    /// endpoints. The `f` key and selecting a journey both land here.
    pub(super) fn fit_current(&mut self) {
        if let Some(route) = self.model.route.clone()
            && route.geometry.len() >= 2
        {
            self.fit_points(&route.geometry);
            return;
        }
        let journey = self.selected_journey_geometry();
        if journey.len() >= 2 {
            self.fit_points(&journey);
            return;
        }
        let mut ends = Vec::new();
        if let Some(o) = self.model.origin {
            ends.push(o);
        }
        if let Some(d) = self.model.dest {
            ends.push(d);
        }
        if ends.len() >= 2 {
            self.fit_points(&ends);
        }
    }

    pub(super) fn fit_points(&mut self, pts: &[LatLon]) {
        let area = self.visible_map_area();
        let pad = f64::from(self.spacing.s12);
        let Some((center, zoom)) = geo::fit_bounds(pts, area.width(), area.height(), pad) else {
            return;
        };
        let zoom = zoom.min(16.5);
        let world = geo::lonlat_to_world(center, zoom);
        let offset = area.center() - self.view_center();
        self.model.center = geo::world_to_lonlat(world.0 - offset.x, world.1 - offset.y, zoom);
        self.model.zoom = zoom;
    }

    pub(super) fn center_on(&mut self, pos: LatLon) {
        if self.rect.area() <= 0.0 {
            self.model.center = pos;
            self.pending_focus = Some(pos);
            return;
        }
        let zoom = self.model.zoom;
        let world = geo::lonlat_to_world(pos, zoom);
        let offset = self.visible_map_area().center() - self.view_center();
        self.model.center = geo::world_to_lonlat(world.0 - offset.x, world.1 - offset.y, zoom);
    }

    pub(super) fn focus_selected_poi(&mut self) {
        let Some(pos) = self
            .selected_poi
            .as_ref()
            .and_then(|id| self.pois.iter().find(|poi| &poi.id == id))
            .map(|poi| poi.pos)
        else {
            return;
        };
        let mut area = self.visible_map_area();
        if self.poi_card_rect.area() > 0.0 {
            area.y1 = area.y1.min(self.poi_card_rect.y0 - self.gap()).max(area.y0);
        }
        let zoom = self.model.zoom;
        let world = geo::lonlat_to_world(pos, zoom);
        let offset = area.center() - self.view_center();
        self.model.center = geo::world_to_lonlat(world.0 - offset.x, world.1 - offset.y, zoom);
    }

    pub(super) fn zoom_by(&mut self, dz: f64, anchor: Point) {
        let proj = self.projection();
        let under = proj.unproject(anchor);
        self.model.zoom = (self.model.zoom + dz).clamp(3.0, 19.0);
        let proj2 = Projection::new(self.model.center, self.model.zoom, self.view_center());
        let q = proj2.project(under);
        let cw = geo::lonlat_to_world(self.model.center, self.model.zoom);
        self.model.center = geo::world_to_lonlat(
            cw.0 + (q.x - anchor.x),
            cw.1 + (q.y - anchor.y),
            self.model.zoom,
        );
        self.schedule_pois();
    }

    pub(super) fn pan_by(&mut self, dx: f64, dy: f64) {
        let cw = geo::lonlat_to_world(self.model.center, self.model.zoom);
        self.model.center = geo::world_to_lonlat(cw.0 - dx, cw.1 - dy, self.model.zoom);
        self.schedule_pois();
    }
}
