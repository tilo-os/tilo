//! The shared map: the places it knows, the searches it made, how
//! directions travel and the plans it accepted.
//!
//! Saving a place, renaming it, choosing how directions travel and the
//! transit planner's depart/arrive anchor are field edits at their paths
//! (`Workspace::paths().mode.set(TravelMode::Bike)`). The methods below are
//! the operations that are more than setting a field: accepting what a
//! geocoder or a planner answered, and the capped search history.
//!
//! Search results and viewport points of interest are each window's,
//! derived caches; the areas a device keeps for offline use are that
//! device's ([`crate::offline_areas`]).

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, JsonSchema, Keyed, Model, Refusal, resource};

use crate::model::{LatLon, MapModel, Place, Route, TransitJourney, TransitTime, TravelMode};

/// How many recent searches the map remembers.
pub const MAX_RECENTS: usize = 8;

/// One accepted plan with the endpoints that give it meaning. Stored whole,
/// so when two people accept different planner replies at once one complete
/// plan wins rather than a route to one place labelled as a route to
/// another.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum ActivePlan {
    #[default]
    None,
    Route {
        route: Route,
        origin: Option<LatLon>,
        origin_name: String,
        dest: Option<LatLon>,
        dest_name: String,
        mode: TravelMode,
    },
    Transit {
        transit: Vec<TransitJourney>,
        origin: Option<LatLon>,
        origin_name: String,
        dest: Option<LatLon>,
        dest_name: String,
    },
}

tilo_resource::json_model!(ActivePlan);

/// The shared map.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Workspace {
    /// Places
    ///
    /// Every place the map knows: what searches accepted, pinned and saved.
    pub places: Keyed<Place>,
    /// Recent searches, newest first.
    #[model(readonly)]
    pub recents: Vec<String>,
    /// Travel mode
    pub mode: TravelMode,
    /// Transit time
    ///
    /// When a transit journey departs or arrives, relative to now.
    pub transit_time: TransitTime,
    /// The road route accepted last.
    #[model(readonly)]
    pub route_plan: ActivePlan,
    /// The transit journeys accepted last.
    #[model(readonly)]
    pub transit_plan: ActivePlan,
}

#[resource(kind = "maps.workspace", label = "Maps", scopes = Workspace::saved_scope)]
impl Workspace {
    /// Accept places.
    ///
    /// Places a search or the map found become places of the shared map;
    /// one already known is updated and keeps being saved.
    #[action(housekeeping)]
    pub fn accept_places(&mut self, #[param("The places")] places: Vec<Place>) {
        for place in places {
            match self.places.find_mut(&tilo_resource::Identified::id(&place)) {
                Some(known) => {
                    let saved = known.saved || place.saved;
                    *known = Place { saved, ..place };
                }
                None => self.places.push(place),
            }
        }
        self.places.settle();
    }

    /// Search maps.
    ///
    /// Remembers the search in the shared history; each window looks it up
    /// on its own.
    #[action]
    pub fn commit_search(&mut self, #[param("What was searched")] query: String) {
        let query = query.trim();
        if query.is_empty() {
            return;
        }
        self.recents
            .retain(|recent| !recent.eq_ignore_ascii_case(query));
        self.recents.insert(0, query.to_owned());
        self.recents.truncate(MAX_RECENTS);
    }

    /// Clear recent searches.
    #[action]
    pub fn clear_recents(&mut self) {
        self.recents.clear();
    }

    /// Accept a route.
    ///
    /// The road route a planner answered, with its endpoints.
    #[action]
    pub fn accept_route(
        &mut self,
        #[param("The route")] route: Route,
        #[param("Where it starts")] origin: Option<LatLon>,
        #[param("What the start is called")] origin_name: String,
        #[param("Where it ends")] dest: Option<LatLon>,
        #[param("What the end is called")] dest_name: String,
        #[param("How it travels")] mode: TravelMode,
    ) {
        self.mode = mode;
        self.route_plan = ActivePlan::Route {
            route,
            origin,
            origin_name,
            dest,
            dest_name,
            mode,
        };
    }

    /// Clear route.
    #[action]
    pub fn clear_route(&mut self) -> Result<(), Refusal> {
        if self.route_plan == ActivePlan::None {
            return Err(Refusal::new("there is no route"));
        }
        self.route_plan = ActivePlan::None;
        Ok(())
    }

    /// Accept transit journeys.
    ///
    /// The journeys a transit planner answered, with their endpoints.
    #[action]
    pub fn accept_transit(
        &mut self,
        #[param("The journeys")] transit: Vec<TransitJourney>,
        #[param("Where they start")] origin: Option<LatLon>,
        #[param("What the start is called")] origin_name: String,
        #[param("Where they end")] dest: Option<LatLon>,
        #[param("What the end is called")] dest_name: String,
    ) {
        self.mode = TravelMode::Transit;
        self.transit_plan = ActivePlan::Transit {
            transit,
            origin,
            origin_name,
            dest,
            dest_name,
        };
    }

    /// Clear transit options.
    #[action]
    pub fn clear_transit(&mut self) -> Result<(), Refusal> {
        if self.transit_plan == ActivePlan::None {
            return Err(Refusal::new("there are no transit options"));
        }
        self.transit_plan = ActivePlan::None;
        Ok(())
    }
}

impl Workspace {
    /// The saved places, a part of the map search can be narrowed to once
    /// there are any.
    fn saved_scope(&self) -> Vec<ui::search::ScopeDecl> {
        self.places
            .iter()
            .any(|place| place.saved)
            .then(|| ui::search::ScopeDecl::new(crate::place_search::SAVED_SCOPE, "Saved places"))
            .into_iter()
            .collect()
    }

    /// The place `id`.
    pub fn place(&self, id: &str) -> Option<&Place> {
        self.places.iter().find(|place| place.id == id)
    }

    /// The accepted road route.
    pub fn route(&self) -> Option<&Route> {
        match &self.route_plan {
            ActivePlan::Route { route, .. } => Some(route),
            _ => None,
        }
    }

    /// The accepted transit journeys.
    pub fn transit(&self) -> &[TransitJourney] {
        match &self.transit_plan {
            ActivePlan::Transit { transit, .. } => transit,
            _ => &[],
        }
    }

    /// The endpoints of the plan the travel mode shows: the transit
    /// journeys' for transit, the route's otherwise.
    pub fn endpoints(&self) -> Option<(Option<LatLon>, &str, Option<LatLon>, &str)> {
        let plan = if self.mode.is_transit() {
            &self.transit_plan
        } else {
            &self.route_plan
        };
        match plan {
            ActivePlan::Route {
                origin,
                origin_name,
                dest,
                dest_name,
                ..
            }
            | ActivePlan::Transit {
                origin,
                origin_name,
                dest,
                dest_name,
                ..
            } => Some((*origin, origin_name, *dest, dest_name)),
            ActivePlan::None => None,
        }
    }

    /// Show what the shared map holds in a window's `model`.
    pub fn show_in(&self, model: &mut MapModel) {
        model.places = self.places.to_vec();
        model.recents.clone_from(&self.recents);
        model.mode = self.mode;
        model.transit_time = self.transit_time;
        // Keep the accepted road plan in the document for switching back,
        // but never paint it (or its turn card) beneath a transit itinerary.
        model.route = if self.mode.is_transit() {
            None
        } else {
            self.route()
                .filter(|route| route.mode == self.mode)
                .cloned()
        };
        model.transit = self.transit().to_vec();
        if let Some((origin, origin_name, dest, dest_name)) = self.endpoints() {
            model.origin = origin;
            origin_name.clone_into(&mut model.origin_name);
            model.dest = dest;
            dest_name.clone_into(&mut model.dest_name);
        }
    }

    /// The edit that saves the place `id`, or takes it off the saved
    /// places.
    pub fn save(id: &str, saved: bool) -> tilo_resource::Edit<Self> {
        Self::paths()
            .places
            .item(&tilo_resource::Id::from_raw(id))
            .saved
            .set(saved)
    }
}

#[cfg(test)]
mod tests;
