//! Which connection of an app may take an action that shows something: only
//! a window of the app, never an instance with no window.

use tilo_actions::remote::{APPLICATION_OPEN_ACTION, APPLICATION_OPEN_LINK_ACTION};

use super::super::AppBrokerState;

impl AppBrokerState {
    /// Whether `address` in `app_id` shows something, and so is taken only
    /// by a window of the app: its `open` and `open-link`, and any action its
    /// surface declares it shows (Chat's conversation with a person, an
    /// issue's page), on whatever resource it is invoked. An app with no
    /// surface here says so by the kind of the action it publishes in
    /// `workspace`. An instance with no window has nothing to show it in.
    pub(crate) fn action_shows(
        &self,
        app_id: &str,
        workspace: &str,
        address: &tilo_actions::ActionAddress,
    ) -> bool {
        if address.action == APPLICATION_OPEN_ACTION
            || address.action == APPLICATION_OPEN_LINK_ACTION
            || collab_types::app_surface::shows(app_id, &address.action)
        {
            return true;
        }
        let logical = tilo_actions::selection::logical_resource_id(&address.resource);
        self.published_surfaces()
            .into_iter()
            .chain(self.launchable.surfaces())
            .filter(|surface| surface.app_id == app_id && surface.route.cluster_id == workspace)
            .flat_map(|surface| surface.actions)
            .any(|action| {
                action.action == address.action
                    && tilo_actions::selection::logical_resource_id(&action.resource) == logical
                    && action.operation_kind == tilo_actions::OperationKind::View
            })
    }
}
