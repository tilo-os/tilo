//! Directions: the two endpoints and where they come from, the route and its transit variants, when it is replanned, and the framing that fits it in the viewport.

use super::*;

impl MapsApp {
    /// Resolve the route/transit origin. Uses a prior origin if one is set,
    /// otherwise the current map centre (an honest "from here"), labelled as
    /// such — we never invent a specific "your location" without a real fix.
    pub(super) fn resolve_origin(&mut self) -> crate::model::LatLon {
        if let Some(o) = self.model.origin {
            o
        } else {
            self.model.origin_name = "Map centre".to_string();
            self.model.center
        }
    }

    /// Apply a component effect to the route model or the geocoding worker.
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
            // Nothing asks the geocoder: what is here is all there is.
            self.endpoints.finish_search(endpoint, local_results);
        }
    }

    /// Adopt a geocoded dropdown result as the chosen endpoint: set its coord +
    /// label and re-route (or re-plan transit). Mirrors selecting a destination
    /// so From and To behave symmetrically.
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

    /// Kick off a real device-location fix via the location portal (`GeoClue2`
    /// behind it), off the UI thread. Honest states only: "Locating…" now, then
    /// either a real origin or a clear unavailable/denied message on drain.
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

    /// Build the transit query time anchor from the picker. "Leave now" sends no
    /// time (the planner uses its own now); otherwise an RFC-3339 wall clock at
    /// `now + offset`, flagged depart/arrive.
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

    /// Open Directions to the selected place, forcing the destination to the
    /// current selection. Tapping "Directions" on a place card or pressing `d`
    /// re-targets the route at whatever is selected now, honouring the active
    /// travel mode (a road route, or a transit journey if Transit is selected).
    pub(super) fn enter_directions(&mut self) {
        self.set_dest_to_selection();
        self.model.panel = Panel::Directions;
        if self.model.mode.is_transit() {
            self.recompute_transit();
        } else {
            self.recompute_route();
        }
    }

    /// Open Directions in Transit mode to the selected place, forcing the
    /// destination to the current selection. The place-card "Transit" shortcut
    /// and the `t` key land here.
    pub(super) fn enter_transit(&mut self) {
        self.set_dest_to_selection();
        self.choose_mode(TravelMode::Transit);
        self.recompute_transit();
    }

    pub(super) fn recompute_route(&mut self) {
        // Adopt the selected place as destination if none has been chosen yet
        // (e.g. the user changed travel mode straight after selecting a place).
        // A destination set by a prior route or a swap is preserved so the
        // endpoints don't collapse back together.
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
        // Immediate offline fallback so the panel is never empty.
        self.model.route = Some(sample::fallback_route(origin, dest, mode));
        self.fit_route();

        // Live OSRM request.
        self.pending += 1;
        self.model.status = "Routing…".to_string();
        if self.online {
            let tx = self.net_tx.clone();
            spawn_net(async move {
                let r = net::routes(origin, dest, mode).await;
                let _ = tx.send(NetMsg::Route(r));
            });
        }
    }

    /// React to a change of the leave/arrive time anchor. Transit re-plans the
    /// journey against the new time; the road modes just re-derive the computed
    /// departure/arrival clock (OSRM is not time-dependent), so a repaint is
    /// enough — no needless refetch.
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
        // Transit is a mode of Directions, not a separate surface.
        self.model.panel = Panel::Directions;
        self.model.mode = TravelMode::Transit;
        self.model.transit_choice = 0;
        // A transit journey has no road polyline — drop any road route so the
        // map doesn't draw a car line beneath the departures.
        self.model.route = None;
        self.nav_open = true;
        // Real departures only: clear any stale itineraries and show a loading
        // state until the planner responds — we never fabricate departures.
        self.model.transit.clear();
        self.pending += 1;
        self.model.status = "Finding departures…".to_string();
        // No route geometry to fit; frame the map on the endpoints instead.
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
            // Transit plans a public-transport journey; the road modes route.
            if mode.is_transit() {
                self.recompute_transit();
            } else {
                self.recompute_route();
            }
        }
    }

    /// Show the next route the service offered for this trip: the one after
    /// the route shown, or the first again when it was the last. The choice is
    /// a plan like any other, so it is taken in the workspace and framed.
    pub(super) fn next_route(&mut self) {
        if self.model.route_alternatives.len() < 2 {
            return;
        }
        let shown = self
            .model
            .route
            .as_ref()
            .and_then(|route| {
                self.model.route_alternatives.iter().position(|offered| {
                    offered.duration_s == route.duration_s && offered.distance_m == route.distance_m
                })
            })
            .unwrap_or(0);
        let next = (shown + 1) % self.model.route_alternatives.len();
        let route = self.model.route_alternatives[next].clone();
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

    /// Exchange the route's origin and destination (both the coordinates and
    /// their labels), then re-route so the line and steps reflect the reversal.
    pub(super) fn swap_ends(&mut self) {
        // Need a concrete point on both ends to have something to swap.
        if self.model.origin.is_none() || self.model.dest.is_none() {
            return;
        }
        std::mem::swap(&mut self.model.origin, &mut self.model.dest);
        std::mem::swap(&mut self.model.origin_name, &mut self.model.dest_name);
        // Both planners read the exchanged endpoints. Transit must discard
        // the old direction's departures rather than request a road route.
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

    /// Flatten the currently-selected transit journey's leg geometry into a
    /// single polyline (empty if no journey is selected or none has geometry).
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

    /// Frame the map on whatever journey is currently shown — a road route's
    /// polyline, else the selected transit itinerary's leg geometry, else the
    /// origin/destination pair. This is what the `f` key and journey selection
    /// use so "fit" works in Transit mode (which has no road route), not only
    /// for road routes.
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
        // No line to fit — frame the endpoints if we have them.
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

    /// Frame the map so `pts` fit the visible map area — beside the docked
    /// panel, under the turn card, clear of the controls — and centre them
    /// there rather than on the window. Used to fit a road route's geometry
    /// and, for transit where there is no road polyline, the endpoints.
    pub(super) fn fit_points(&mut self, pts: &[LatLon]) {
        let area = self.visible_map_area();
        // Room for a pin and its name above an end of the line.
        let pad = f64::from(self.spacing.s12);
        let Some((center, zoom)) = geo::fit_bounds(pts, area.width(), area.height(), pad) else {
            return;
        };
        let zoom = zoom.min(16.5);
        // Shift the camera so the fitted centre lands on the area's centre.
        let world = geo::lonlat_to_world(center, zoom);
        let offset = area.center() - self.view_center();
        self.model.center = geo::world_to_lonlat(world.0 - offset.x, world.1 - offset.y, zoom);
        self.model.zoom = zoom;
    }

    /// Bring `pos` to the centre of the visible map area — beside the docked
    /// panel, clear of the controls and under the turn card — rather than
    /// the window's centre, where chrome may cover it. Before the window has
    /// its size the camera centres on it once the size is known.
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

    /// Bring the place whose card is open to the middle of the map above
    /// its card, so neither the card nor the panel covers it.
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
