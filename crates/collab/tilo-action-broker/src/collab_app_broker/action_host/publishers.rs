//! Which connection of an app may take an action that shows something: only
//! a window of the app, never an instance with no window.

use tilo_actions::remote::APPLICATION_OPEN_ACTION;

use super::super::AppBrokerState;

impl AppBrokerState {
    pub(crate) fn is_windowless(&self, peer_id: u64) -> bool {
        self.launchable
            .windowless_peers
            .lock()
            .is_ok_and(|windowless| windowless.contains(&peer_id))
    }

    /// Whether `address` in `app_id` shows something, and so is taken only
    /// by a window of the app: its `open`, and any action of the view kind
    /// it declares (an issue's page, its chat), as published in `room` or
    /// offered by its manifest. An instance with no window has nothing to
    /// show it in.
    pub(crate) fn action_shows(
        &self,
        app_id: &str,
        room: &str,
        address: &tilo_actions::ActionAddress,
    ) -> bool {
        if address.action == APPLICATION_OPEN_ACTION {
            return true;
        }
        let logical = tilo_actions::selection::logical_resource_id(&address.resource);
        self.published_surfaces()
            .into_iter()
            .chain(self.launchable.surfaces())
            .filter(|surface| surface.app_id == app_id && surface.route.cluster_id == room)
            .flat_map(|surface| surface.actions)
            .find(|action| {
                action.action == address.action
                    && tilo_actions::selection::logical_resource_id(&action.resource) == logical
            })
            .or_else(|| self.manifest_offer(app_id, address))
            .is_some_and(|action| action.operation_kind == tilo_actions::OperationKind::View)
    }
}
