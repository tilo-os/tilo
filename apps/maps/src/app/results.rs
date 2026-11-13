//! What arrives from the network and the location service, drained on the app's own thread and
//! applied to the map.

use super::{
    ActionOutput, LatLon, LocationOutcome, MapsApp, NetMsg, POI_MAX, TravelMode, Workspace,
    endpoint_of, failure, net, spawn_net, workspace,
};

impl MapsApp {
    #[allow(
        clippy::too_many_lines,
        reason = "one match arm per NetMsg variant; the length is the number of \
                  message kinds, and splitting arms out would relocate the code, not \
                  shrink what a reader must trace for one variant"
    )]
    pub(super) fn drain_net(&mut self) {
        while let Ok(msg) = self.net_rx.try_recv() {
            self.pending = self.pending.saturating_sub(1);
            match msg {
                NetMsg::Search { query, results } => {
                    if self
                        .search_asked
                        .as_ref()
                        .is_some_and(|(asked, _)| *asked == query)
                    {
                        self.search_asked = None;
                    }
                    match results {
                        Ok(found) if !found.is_empty() => {
                            let mut ranked = found;
                            self.accept_places(ranked.clone());
                            self.rank_results(&mut ranked, &query);
                            self.results_anchor = Some(self.search_anchor());
                            if let Some(first) = ranked.first() {
                                self.model.center = first.pos;
                                self.model.selected = Some(first.id.clone());
                            }
                            self.model.results = ranked.clone();
                            self.model.status = failure::t_args(
                                "maps-search-results",
                                &[
                                    ("count", i64::try_from(ranked.len()).unwrap_or(0).into()),
                                    ("query", query.clone().into()),
                                ],
                            );
                        }
                        Ok(_) => {
                            self.model.status = failure::t_args(
                                "maps-search-no-results",
                                &[("query", query.clone().into())],
                            );
                        }
                        Err(e) => {
                            self.model.status = failure::say(failure::SEARCH, &e);
                        }
                    }
                }
                NetMsg::PaletteSearch { query, results } => {
                    let mut found = results.unwrap_or_default();
                    self.rank_results(&mut found, &query);
                    self.place_search.answer(&query, found);
                }
                NetMsg::EndpointSearch {
                    origin,
                    query,
                    results,
                } => match results {
                    Ok(found) if !found.is_empty() => {
                        self.accept_places(found.clone());
                        let mut ranked = found;
                        self.rank_results(&mut ranked, &query);
                        self.endpoints.finish_search(endpoint_of(origin), ranked);
                        self.model.status.clear();
                    }
                    Ok(_) => self
                        .endpoints
                        .finish_search(endpoint_of(origin), Vec::new()),
                    Err(e) => {
                        self.endpoints
                            .finish_search(endpoint_of(origin), Vec::new());
                        self.model.status = failure::say(failure::SEARCH, &e);
                    }
                },
                NetMsg::ReverseGeocode(r) => {
                    if let Ok(label) = r
                        && !label.is_empty()
                    {
                        self.model.origin_name = label;
                    }
                }
                NetMsg::Route(r) => match r {
                    Ok(routes) => {
                        let Some(route) = routes.first().cloned() else {
                            self.model.status =
                                failure::say(failure::ROUTE, "the service offered no route");
                            return;
                        };
                        self.model.route_alternatives = routes;

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
                        self.model.status = failure::t("maps-route-updated");
                    }
                    Err(e) => {
                        self.model.status = failure::t_args(
                            "maps-route-estimate",
                            &[("reason", failure::say(failure::ROUTE, &e).into())],
                        );
                    }
                },
                NetMsg::Transit(t) => match t {
                    Ok(j) if !j.is_empty() => {
                        self.model.transit.clone_from(&j);
                        self.model.route = None;
                        self.model.mode = TravelMode::Transit;
                        self.perform(Workspace::ACCEPT_TRANSIT.with(workspace::AcceptTransit {
                            transit: j,
                            origin: self.model.origin,
                            origin_name: self.model.origin_name.clone(),
                            dest: self.model.dest,
                            dest_name: self.model.dest_name.clone(),
                        }));
                        self.fit_current();
                        self.model.status = failure::t("maps-departures-updated");
                    }
                    Ok(_) => self.model.status = failure::t("maps-no-transit"),
                    Err(e) => {
                        self.model.status = failure::say(failure::TRANSIT, &e);
                    }
                },
                NetMsg::Pois { generation, pois } => {
                    if generation == self.poi_generation {
                        match pois {
                            Ok(mut found) => {
                                found.truncate(POI_MAX);
                                let n = found.len();
                                self.pois = found;
                                if let Some(sel) = &self.selected_poi
                                    && !self.pois.iter().any(|p| &p.id == sel)
                                {
                                    self.selected_poi = None;
                                }
                                if self.model.status == failure::t("maps-loading-places") {
                                    self.model.status = failure::t_args(
                                        "maps-nearby-count",
                                        &[("count", n.into())],
                                    );
                                }
                            }
                            Err(e) => {
                                self.model.status = failure::say(failure::NEARBY, &e);
                            }
                        }
                    }
                }
                NetMsg::RegionSearch { query, results } => {
                    if self.offline_panel.query_matches(&query) {
                        self.offline_panel.adopt_results(results);
                    }
                }
            }
            if self.pending == 0 && self.model.status.contains('…') {
                self.model.status.clear();
            }
        }
    }

    pub(super) fn drain_location(&mut self) {
        let Some(rx) = &self.location_rx else { return };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.locating = false;
                self.location_rx = None;
                self.model.status = failure::t("maps-location-unavailable");
                if let Some(reply) = self.location_reply.take() {
                    reply.finish(Err(failure::t("maps-location-disconnected")));
                }
                return;
            }
        };
        self.locating = false;
        self.location_rx = None;
        match outcome {
            LocationOutcome::Located { lat, lon, .. } => {
                let pos = LatLon::new(lat, lon);
                self.model.origin = Some(pos);
                self.model.origin_name = failure::t("maps-current-location");
                self.model.center = pos;
                self.model.status = failure::t("maps-located");
                if self.online {
                    self.pending += 1;
                    let tx = self.net_tx.clone();
                    spawn_net(async move {
                        let r = net::reverse_geocode(pos.lat, pos.lon).await;
                        let _ = tx.send(NetMsg::ReverseGeocode(r));
                    });
                }
                if self.model.dest.is_some() {
                    if self.model.mode.is_transit() {
                        self.recompute_transit();
                    } else {
                        self.recompute_route();
                    }
                }
                if let Some(reply) = self.location_reply.take() {
                    reply.finish(Ok(ActionOutput::default()));
                }
            }
            LocationOutcome::Unavailable(reason) => {
                self.model.status =
                    format!("{} — {reason}", failure::t("maps-location-unavailable"));
                if let Some(reply) = self.location_reply.take() {
                    reply.finish(Err(reason));
                }
            }
        }
    }
}
