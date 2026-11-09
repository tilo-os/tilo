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
    /// it declares (an issue's page, its chat), as published in `workspace` or
    /// offered by its manifest. An instance with no window has nothing to
    /// show it in.
    pub(crate) fn action_shows(
        &self,
        app_id: &str,
        workspace: &str,
        address: &tilo_actions::ActionAddress,
    ) -> bool {
        if address.action == APPLICATION_OPEN_ACTION {
            return true;
        }
        let logical = tilo_actions::selection::logical_resource_id(&address.resource);
        self.published_surfaces()
            .into_iter()
            .chain(self.launchable.surfaces())
            .filter(|surface| surface.app_id == app_id && surface.route.cluster_id == workspace)
            .flat_map(|surface| surface.actions)
            .find(|action| {
                action.action == address.action
                    && tilo_actions::selection::logical_resource_id(&action.resource) == logical
            })
            .or_else(|| self.manifest_offer(app_id, address))
            .is_some_and(|action| action.operation_kind == tilo_actions::OperationKind::View)
    }

    /// Why the plain `open` of `app_id` is refused before anything launches, when the app is one
    /// this session runs itself: it has no window process, and its `open` places its root instead
    /// (`docs/designs/app-language/one-app-model.md` §3). Answering at once is the difference
    /// between a refusal the caller can act on and a wait for a process that never publishes.
    /// `None` for an app a window of starts.
    pub(super) fn served_open_refusal(&self, app_id: &str) -> Option<String> {
        let root = self
            .services
            .launcher
            .as_ref()
            .and_then(|launcher| launcher.served(app_id))?;
        Some(format!(
            "{app_id} is served by this session: it has no window to open; open its resource {root} \
             instead"
        ))
    }
}
