//! What a maps window answers and shows beside the shared map: its effects,
//! with their availability now, the live rows it publishes, and what it
//! adds to the platform search.

use ui::framework::{EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow};

use super::MapsApp;
use crate::resource::MapsEffect;
use crate::workspace::Workspace;

/// How many rows of one kind a published page holds; the rest resolve by id.
const ROWS_PER_KIND: usize = 12;

impl MapsApp {
    /// Answer `effect`, from a control, the palette, an agent or a script.
    pub(crate) fn answer_effect(
        &mut self,
        effect: MapsEffect,
        cx: &mut EffectCx<Workspace, Self>,
    ) -> EffectAnswer {
        match effect {
            MapsEffect::Search { query } => self.search_for(query),
            MapsEffect::Locate => {
                if let Some(reason) = self.locate_unavailable() {
                    return Err(reason.1.into());
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
                return Err("Map point of interest is no longer in this viewport".into());
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
    fn locate_unavailable(&self) -> Option<(&'static str, &'static str)> {
        if !self.online {
            Some((
                "location_service_offline",
                "The location service is unavailable in this session",
            ))
        } else if self.location_rx.is_some() || self.location_reply.is_some() {
            Some((
                "request_in_progress",
                "A location request is already in progress",
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
                "Hide traffic",
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
