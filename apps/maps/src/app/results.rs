//! What arrives from the network and the location service: a tile, a geocoder reply or a position, drained on the app's own thread and applied to the map.

use super::*;

impl MapsApp {
    #[allow(
        clippy::too_many_lines,
        reason = "one match arm per NetMsg variant; the length is the number of \
                  message kinds, and splitting arms out would relocate the code, not \
                  shrink what a reader must trace for one variant"
    )]
    /// Take the network results that arrived since the last frame. A result
    /// that becomes the map's — a geocoded place, a route, transit options —
    /// is accepted with the map action every peer replays.
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
                            // A committed search: list the results in the panel
                            // and centre on the top (nearest) match.
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
                            self.model.status = failure::say(failure::SEARCH, &e.to_string());
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
                        // Rank the From/To suggestions the same way a search
                        // is ranked — relevance first, nearest-first within a
                        // tier — so a distant namesake never sits above the
                        // near place the user meant.
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
                        self.model.status = format!("Search failed: {e}");
                    }
                },
                NetMsg::ReverseGeocode(r) => {
                    if let Ok(label) = r
                        && !label.is_empty()
                    {
                        // Refine the device-location origin's label in place.
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
                        // Every route the service offered, this one first: the
                        // person can look at another without retyping the
                        // destination.
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
                        self.model.status = format!(
                            "{} The offline estimate is shown.",
                            failure::say(failure::ROUTE, &e.to_string())
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
                        // Frame the map on the itinerary now that we have real
                        // leg geometry (the same courtesy the road route gets).
                        self.fit_current();
                        self.model.status = failure::t("maps-departures-updated");
                    }
                    Ok(_) => self.model.status = failure::t("maps-no-transit"),
                    Err(e) => {
                        self.model.status = failure::say(failure::TRANSIT, &e.to_string());
                    }
                },
                NetMsg::Pois { generation, pois } => {
                    // Ignore a stale reply for a viewport we've since panned off.
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
                                if self.model.status.contains("Loading places") {
                                    self.model.status = format!("{n} places nearby");
                                }
                            }
                            Err(e) => {
                                self.model.status = failure::say(failure::NEARBY, &e.to_string());
                            }
                        }
                    }
                }
                NetMsg::RegionSearch { query, results } => {
                    // Only adopt a reply that still matches the current query and
                    // while the download dialog is open.
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

    /// Drain a pending device-location result (non-blocking, once per frame).
    /// On success the coordinate becomes the route origin, the map recentres,
    /// a reverse-geocode refines the label, and the route/transit re-plans. On
    /// failure an honest "Location unavailable" message is shown — never a
    /// fabricated position.
    pub(super) fn drain_location(&mut self) {
        let Some(rx) = &self.location_rx else { return };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.locating = false;
                self.location_rx = None;
                self.model.status = "Location unavailable".to_string();
                if let Some(reply) = self.location_reply.take() {
                    reply.finish(Err(
                        "The location service disconnected before returning a fix".into(),
                    ));
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
                self.model.origin_name = "Current location".to_string();
                self.model.center = pos;
                self.model.status = "Located".to_string();
                // Refine the raw coordinate into a human place label.
                if self.online {
                    self.pending += 1;
                    let tx = self.net_tx.clone();
                    spawn_net(async move {
                        let r = net::reverse_geocode(pos.lat, pos.lon).await;
                        let _ = tx.send(NetMsg::ReverseGeocode(r));
                    });
                }
                // If a destination is known, re-plan from the real location.
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
