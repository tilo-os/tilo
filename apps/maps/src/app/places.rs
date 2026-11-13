//! Finding places: the geocoder's answers and the local matches around them, how results rank, and
//! the viewport POIs a person can select.

use super::{
    Instant, MapsApp, NEARBY_MAX, NetMsg, POI_DEBOUNCE, POI_MIN_ZOOM, Panel, Place, Point,
    Workspace, bounds_contains, canvas, failure, geo, net, spawn_net,
};

impl MapsApp {
    pub(super) fn accept_places(&mut self, places: Vec<Place>) {
        for place in &places {
            self.model.upsert_place(place.clone());
        }
        self.perform(Workspace::ACCEPT_PLACES.with(places));
    }

    pub(super) fn commit_search(&mut self, query: &str) {
        crate::workspace::remember_search(&mut self.model.recents, query);
        self.perform(Workspace::COMMIT_SEARCH.with(query));
    }

    pub(super) fn select_place(&mut self, id: &str) {
        let changed = self.model.selected.as_deref() != Some(id);
        self.model.selected = Some(id.to_string());
        // Selecting a place targets it for routing. A re-route reads `dest`, not the selection, so
        // `swap_ends` can move it to the origin without being clobbered.
        if let Some(p) = self.model.place(id).cloned() {
            self.center_on(p.pos);
            self.model.dest = Some(p.pos);
            self.model.dest_name = p.name;
        }
        if changed {
            self.model.transit.clear();
            self.model.transit_choice = 0;
        }
        self.nav_open = true;
    }

    pub(super) fn set_dest_to_selection(&mut self) {
        if let Some(p) = self.model.selected_place().cloned() {
            self.model.dest = Some(p.pos);
            self.model.dest_name = p.name;
        }
    }

    pub(super) fn set_panel(&mut self, panel: Panel) {
        self.model.panel = panel;
        self.scroll = 0.0;
        self.nav_open = true;
        self.endpoints.dismiss();
        if panel == Panel::Directions {
            if self.model.mode.is_transit() {
                let has_target = self.model.dest.is_some() || self.model.selected_place().is_some();
                if self.model.transit.is_empty() && self.pending == 0 && has_target {
                    self.recompute_transit();
                }
            } else if self.model.route.is_none() {
                self.recompute_route();
            }
        }
    }

    pub(super) fn local_matches(&self, q: &str) -> Vec<crate::model::Place> {
        let needle = q.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut results: Vec<_> = self
            .model
            .places
            .iter()
            .filter(|p| {
                p.name.to_lowercase().contains(&needle)
                    || p.address.to_lowercase().contains(&needle)
                    || p.category.label().to_lowercase().contains(&needle)
            })
            .cloned()
            .collect();
        if results.is_empty() {
            results = self
                .model
                .places
                .iter()
                .filter(|p| crate::place_search::keyword_match(&needle, p.category))
                .cloned()
                .collect();
        }
        for poi in &self.pois {
            if results.iter().any(|r| r.id == poi.id) {
                continue;
            }
            let hit = poi.name.to_lowercase().contains(&needle)
                || poi.address.to_lowercase().contains(&needle)
                || poi.category.label().to_lowercase().contains(&needle)
                || poi
                    .kind
                    .as_deref()
                    .is_some_and(|k| k.to_lowercase().contains(&needle))
                || crate::place_search::keyword_match(&needle, poi.category);
            if hit {
                results.push(crate::model::Place::new(
                    &poi.id,
                    &poi.name,
                    poi.category,
                    &poi.address,
                    poi.pos,
                ));
            }
        }
        results
    }

    pub(super) fn search_anchor(&self) -> crate::model::LatLon {
        self.model.origin.unwrap_or(self.model.center)
    }

    /// Rank by relevance to `query`, distance to the search anchor tie-breaking, collapsing
    /// co-located duplicates: a far namesake must not outrank the near place meant (see
    /// [`crate::rank`]).
    pub(super) fn rank_results(&self, places: &mut Vec<crate::model::Place>, query: &str) {
        crate::rank::rank(places, self.search_anchor(), query);
    }

    pub(super) fn ranked_matches(&self, q: &str) -> Vec<crate::model::Place> {
        let mut v = self.local_matches(q);
        self.rank_results(&mut v, q);
        v
    }

    pub(super) fn pump_place_search(&mut self) {
        let Some(query) = self.place_search.take_query() else {
            return;
        };
        if !self.online {
            self.place_search.answer(&query, Vec::new());
            return;
        }
        self.pending += 1;
        let tx = self.net_tx.clone();
        spawn_net(async move {
            let results = net::geocode(&query).await;
            let _ = tx.send(NetMsg::PaletteSearch { query, results });
        });
    }

    pub(crate) fn open_place(&mut self, id: &str) -> Result<(), String> {
        if self.model.place(id).is_none() {
            let place = self
                .place_search
                .geocoded(id)
                .cloned()
                .ok_or_else(|| failure::t("maps-place-unavailable"))?;
            let query = place.name.clone();
            self.accept_places(vec![place]);
            self.commit_search(&query);
        }
        self.select_place(id);
        Ok(())
    }

    pub(crate) fn search_for(&mut self, query: String) {
        self.model.query = query;
        self.run_search(false);
    }

    pub(super) fn run_search(&mut self, commit: bool) {
        let q = self.model.query.trim().to_string();
        if q.is_empty() {
            return;
        }
        let results = self.ranked_matches(&q);
        self.results_anchor = Some(self.search_anchor());
        if let Some(first) = results.first() {
            let id = first.id.clone();
            self.model.center = first.pos;
            self.model.selected = Some(id);
        }
        self.model.results = results;
        if commit {
            self.commit_search(&q);
        } else {
            crate::workspace::remember_search(&mut self.model.recents, &q);
        }
        self.model.panel = Panel::Explore;
        self.scroll = 0.0;
        self.nav_open = true;

        self.spawn_search(q);
    }

    pub(super) fn spawn_search(&mut self, query: String) {
        if !self.online {
            return;
        }
        self.pending += 1;
        self.search_asked = Some((query.clone(), ui::time::now()));
        let tx = self.net_tx.clone();
        spawn_net(async move {
            let results = net::geocode(&query).await;
            let _ = tx.send(NetMsg::Search { query, results });
        });
    }

    #[cfg(test)]
    pub(super) fn endpoint_overlay_active(&self) -> bool {
        matches!(self.model.panel, Panel::Directions) && self.endpoints.overlay_active()
    }

    pub(super) fn poi_icons_shown(&self) -> bool {
        self.model.zoom >= canvas::POI_ICON_ZOOM
    }

    pub(super) fn poi_declutter_px(&self) -> f64 {
        (if self.poi_icons_shown() { 26.0 } else { 15.0 }) * self.ui_scale
    }

    pub(super) fn visible_poi_indices(&self) -> Vec<usize> {
        canvas::declutter_indices(
            &self.projection(),
            &self.pois,
            self.poi_declutter_px(),
            self.selected_poi.as_deref(),
        )
    }

    pub(super) fn poi_at(&self, p: Point) -> Option<String> {
        let proj = self.projection();
        let icons = self.poi_icons_shown();
        let mut best: Option<(f64, String)> = None;
        for i in self.visible_poi_indices() {
            let Some(poi) = self.pois.get(i) else {
                continue;
            };
            let c = proj.project(poi.pos);
            let selected = self.selected_poi.as_deref() == Some(poi.id.as_str());
            let r = canvas::poi_radius(icons, selected) * self.ui_scale;
            let d = c.distance(p);
            if d <= r + 6.0 * self.ui_scale && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                best = Some((d, poi.id.clone()));
            }
        }
        best.map(|(_, id)| id)
    }

    pub(super) fn select_poi(&mut self, id: &str) {
        self.selected_poi = Some(id.to_string());
    }

    pub(super) fn nearby_pois(&self) -> Vec<crate::model::Poi> {
        let anchor = self.search_anchor();
        let mut v = self.pois.clone();
        v.sort_by(|a, b| {
            geo::haversine_m(a.pos, anchor).total_cmp(&geo::haversine_m(b.pos, anchor))
        });
        v.truncate(NEARBY_MAX);
        if let Some(sel) = self.selected_poi.clone()
            && !v.iter().any(|p| p.id == sel)
            && let Some(p) = self.pois.iter().find(|p| p.id == sel).cloned()
        {
            v.pop();
            v.insert(0, p);
        }
        v
    }

    pub(super) fn select_poi_from_list(&mut self, id: &str) {
        let Some(poi) = self.pois.iter().find(|p| p.id == id).cloned() else {
            return;
        };
        self.model.center = poi.pos;
        self.pending_poi_focus = true;
        self.selected_poi = Some(id.to_string());
        self.nav_open = !self.overlay_mode();
    }

    pub(super) fn poi_directions(&mut self) {
        let Some(id) = self.selected_poi.clone() else {
            return;
        };
        let Some(poi) = self.pois.iter().find(|p| p.id == id).cloned() else {
            return;
        };
        self.model.upsert_place(crate::model::Place::new(
            &poi.id,
            &poi.name,
            poi.category,
            &poi.address,
            poi.pos,
        ));
        self.model.selected = Some(poi.id.clone());
        self.model.dest = Some(poi.pos);
        self.model.dest_name = poi.name;
        self.model.panel = Panel::Directions;
        self.selected_poi = None;
        if self.model.mode.is_transit() {
            self.recompute_transit();
        } else {
            self.recompute_route();
        }
    }

    pub(super) fn marker_at(&self, p: Point) -> Option<String> {
        let proj = self.projection();
        let mut best: Option<(f64, String)> = None;
        for place in &self.model.places {
            let anchor = proj.project(place.pos);
            let selected = self.model.selected.as_deref() == Some(place.id.as_str());
            let (r, h) = canvas::pin_size(selected, self.ui_scale);
            let center = Point::new(anchor.x, anchor.y - h);
            let d = center.distance(p);
            if d <= r + 5.0 * self.ui_scale && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                best = Some((d, place.id.clone()));
            }
        }
        best.map(|(_, id)| id)
    }

    pub(super) fn schedule_pois(&mut self) {
        self.poi_debounce = Some(ui::time::now() + POI_DEBOUNCE);
    }

    pub(super) fn tick_poi_debounce(&mut self, now: Instant) {
        let Some(deadline) = self.poi_debounce else {
            return;
        };
        if now < deadline {
            return;
        }
        self.poi_debounce = None;
        self.fetch_viewport_pois();
    }

    pub(super) fn fetch_viewport_pois(&mut self) {
        if self.rect.width() <= 1.0 {
            return;
        }
        if self.model.zoom < POI_MIN_ZOOM {
            self.pois.clear();
            self.poi_fetched_bounds = None;
            return;
        }
        let view = self.viewport_bounds();
        if let Some(prev) = self.poi_fetched_bounds
            && bounds_contains(prev, view)
        {
            return; // still inside the last fetch — reuse the cache
        }
        let bounds = self.padded_viewport_bounds();
        self.poi_fetched_bounds = Some(bounds);
        self.poi_generation += 1;
        let generation = self.poi_generation;
        if self.online {
            self.pending += 1;
            self.model.status = failure::t("maps-loading-places");
            let tx = self.net_tx.clone();
            spawn_net(async move {
                let pois = net::overpass(
                    bounds.min.lat,
                    bounds.min.lon,
                    bounds.max.lat,
                    bounds.max.lon,
                )
                .await;
                let _ = tx.send(NetMsg::Pois { generation, pois });
            });
        }
    }
}
