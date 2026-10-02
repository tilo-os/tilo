use ui::tilo_actions::{ActionContract, ActionSpec, Availability, OperationKind, Resource};

use super::MapsApp;

impl MapsApp {
    /// Ranked search hits and nearby POIs — machine-local presentation cache,
    /// not document state. Hosted [`PublishesResources`] returns only these.
    pub(crate) fn live_search_and_poi_resources(&self) -> Vec<Resource> {
        let mut extras = self.live_search_and_poi_resources_undecorated();
        for resource in &mut extras {
            self.decorate_live_resource(resource);
        }
        extras
    }

    pub(crate) fn live_search_and_poi_resources_undecorated(&self) -> Vec<Resource> {
        let mut extras: Vec<Resource> = self
            .model
            .results
            .iter()
            .take(12)
            .map(|place| {
                Resource::new(
                    format!("maps.search-result:{}", place.id),
                    "maps.search-result",
                    place.name.clone(),
                )
            })
            .collect();
        extras.extend(
            self.nearby_pois()
                .into_iter()
                .take(12)
                .map(|poi| Resource::new(format!("maps.poi:{}", poi.id), "maps.poi", poi.name)),
        );
        extras
    }

    /// Overlay live availability / presentation ops on a published resource.
    /// Durable mutations stay on the encoded `MapsAction` path in `actions`;
    /// locate/traffic already live on [`crate::resource::Maps::local_operations`]
    /// and are replaced here with this surface's contracts.
    pub(crate) fn decorate_live_resource(&self, resource: &mut Resource) {
        if resource.kind == "maps.workspace" {
            resource
                .actions
                .retain(|action| action.id != "locate" && action.id != "toggle-traffic");
            let locate_availability = if !self.online {
                Availability::Unavailable {
                    code: "location_service_offline".into(),
                    reason: "The location service is unavailable in this session".into(),
                }
            } else if self.location_rx.is_none() {
                Availability::Available
            } else {
                Availability::Unavailable {
                    code: "request_in_progress".into(),
                    reason: "A location request is already in progress".into(),
                }
            };
            if let Some(spec) = crate::resource::locate_spec(locate_availability) {
                resource.actions.push(spec);
            }
            if let Some(spec) =
                crate::resource::toggle_traffic_spec(self.map_controls.traffic_enabled())
            {
                resource.actions.push(spec);
            }
        }

        let prefix = match resource.kind.as_str() {
            "maps.place" => Some("maps.place:"),
            "maps.search-result" => Some("maps.search-result:"),
            _ => None,
        };
        if let Some(prefix) = prefix
            && let Some(place_id) = resource.id.as_str().strip_prefix(prefix)
        {
            let known = self.model.place(place_id).is_some()
                || self.model.results.iter().any(|place| place.id == place_id);
            // Saved places already carry open/directions from document publish.
            if known && resource.kind == "maps.search-result" {
                if !resource.actions.iter().any(|action| action.id == "open")
                    && let Ok(spec) = ActionSpec::operation(
                        "open",
                        "Open place",
                        OperationKind::View,
                        "maps.open-place",
                        &place_id,
                    )
                {
                    resource
                        .actions
                        .insert(0, spec.with_contract(ActionContract::view()));
                }
                if !resource
                    .actions
                    .iter()
                    .any(|action| action.id == "directions")
                    && let Ok(spec) = ActionSpec::operation(
                        "directions",
                        "Get directions",
                        OperationKind::View,
                        "maps.directions",
                        &place_id,
                    )
                {
                    resource
                        .actions
                        .insert(0, spec.with_contract(ActionContract::view()));
                }
            }
        }
        if resource.kind == "maps.poi"
            && let Some(poi_id) = resource.id.as_str().strip_prefix("maps.poi:")
            && self.pois.iter().any(|poi| poi.id == poi_id)
        {
            for (id, label, type_id) in [
                ("open", "Open place", "maps.open-poi"),
                ("directions", "Get directions", "maps.directions-poi"),
            ] {
                if resource.actions.iter().any(|action| action.id == id) {
                    continue;
                }
                if let Ok(spec) =
                    ActionSpec::operation(id, label, OperationKind::View, type_id, &poi_id)
                {
                    resource
                        .actions
                        .push(spec.with_contract(ActionContract::view()));
                }
            }
        }
        for child in &mut resource.children {
            self.decorate_live_resource(child);
        }
    }
}
