use super::*;

impl MapsApp {
    /// Add operations whose availability depends on this live app instance to
    /// both eager and lazily resolved resources. Durable mutations stay on the
    /// encoded `MapsAction` path in `actions`; these are presentation/external
    /// effects and therefore use local operation handlers.
    pub(super) fn decorate_live_resource(&self, resource: &mut Resource) {
        if resource.kind == "maps.workspace" {
            if let Ok(spec) = ActionSpec::operation(
                "locate",
                "Use my location",
                OperationKind::Effect,
                "maps.locate",
                &"",
            ) {
                resource.actions.push(spec.with_contract(ActionContract {
                    availability: if !self.online {
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
                    },
                    permission: PermissionRequirement::Read,
                    retry: RetryPolicy::AtMostOnce,
                    execution: ExecutionMode::Background { cancellable: false },
                    discovery: DiscoveryScope::Launchable,
                    result: ResultSpec::Job,
                    ..ActionContract::default()
                }));
            }
            if let Ok(spec) = ActionSpec::operation(
                "toggle-traffic",
                if self.map_controls.traffic_enabled() {
                    "Hide traffic"
                } else {
                    "Show traffic"
                },
                OperationKind::View,
                "maps.toggle-traffic",
                &"",
            ) {
                resource
                    .actions
                    .push(spec.with_contract(ActionContract::view()));
            }
        }

        let prefix = match resource.kind.as_str() {
            "maps.place" => Some("maps.place:"),
            "maps.search-result" => Some("maps.search-result:"),
            _ => None,
        };
        if let Some(prefix) = prefix
            && let Some(place_id) = resource.id.as_str().strip_prefix(prefix)
            && self.model.place(place_id).is_some()
        {
            // Saved places expose a local presentation-only opener.
            if resource.kind == "maps.place"
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
            if let Ok(spec) = ActionSpec::operation(
                "directions",
                "Get directions",
                OperationKind::View,
                "maps.directions",
                &place_id,
            ) {
                resource
                    .actions
                    .insert(0, spec.with_contract(ActionContract::view()));
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
