//! The instances of apps the host runs with no window: started for an
//! action that shows nothing, or for an app's `start`, told apart from
//! windows, and never handed an `open`.

use tilo_actions::remote::{
    APPLICATION_RESOURCE_PREFIX, APPLICATION_START_ACTION, ActionCallError, Caller, InvokeRequest,
    provider_workspace,
};

use super::AppBrokerState;
use super::published_invocation::InvocationCaller;

/// The workspace and app `request` starts, when it is a workspace's `start`
/// of one of its apps ([`APPLICATION_START_ACTION`]).
pub(super) fn started_app(request: &InvokeRequest) -> Option<(&str, &str)> {
    let workspace = provider_workspace(&request.target.provider)?;
    let app_id = request
        .target
        .address
        .resource
        .strip_prefix(APPLICATION_RESOURCE_PREFIX)?;
    (request.target.address.action == APPLICATION_START_ACTION).then_some((workspace, app_id))
}

impl AppBrokerState {
    /// Answer `request`, a workspace's `start` of `app_id` ([`started_app`]),
    /// with `peer_id`, the instance that runs now: admitted and settled as
    /// the workspace's other host actions on its apps are. Blocks on the
    /// journal, so it runs on a blocking thread.
    pub(super) fn answer_start(
        &self,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        request: &InvokeRequest,
        (app_id, workspace): (&str, &str),
        peer_id: u64,
    ) -> Result<u64, ActionCallError> {
        let name = self
            .services
            .launcher
            .as_ref()
            .and_then(|launcher| launcher.applications_for(app_id).into_iter().next())
            .map_or_else(|| app_id.to_owned(), |app| app.display_name);
        let admitted = self.admit_host_action(
            caller,
            origin,
            super::host_action_admission::HostActionAdmission {
                workspace,
                app_id,
                descriptor: &super::action_provider::app_start_descriptor(app_id, &name),
                request,
            },
        )?;
        let id = admitted.id;
        self.complete_host_action(
            &admitted,
            tilo_actions::InvocationUpdate::Succeeded {
                invocation: admitted.invocation,
                output: started_output(peer_id),
            },
        )?;
        Ok(id)
    }

    /// Note whether peer `peer_id`, the connection of `local`, is an
    /// instance of its app the host runs with no window.
    pub(super) fn note_windowless(
        &self,
        peer_id: u64,
        local: Option<&super::app_process::LocalAppConnection>,
    ) {
        if local.is_some_and(|local| local.windowless)
            && let Ok(mut windowless) = self.publishers.windowless.lock()
        {
            windowless.insert(peer_id);
        }
    }

    /// Peer `peer_id` disconnected: forget the window it was for, and that
    /// it had none.
    pub(super) fn forget_windows_of(&self, peer_id: u64) {
        let window = self
            .window_instances
            .lock()
            .ok()
            .and_then(|mut windows| windows.remove(&peer_id));
        if let Ok(mut windowless) = self.publishers.windowless.lock() {
            windowless.remove(&peer_id);
        }
        // The window its app held is gone: the device's daemon publishes that
        // it shows no record now.
        if window.is_some() {
            self.windows_changed();
        }
    }
}

/// What an app's `start` answers: the instance that runs, by peer id, as
/// its `open` names the one it started.
pub(super) fn started_output(peer_id: u64) -> tilo_actions::ActionOutput {
    tilo_actions::ActionOutput {
        value: Some(serde_json::json!({ super::workspace_open::OPENED_PEER_FIELD: peer_id })),
        ..tilo_actions::ActionOutput::default()
    }
}
