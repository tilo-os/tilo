//! The instances of apps the host runs with no window: started for an
//! action that shows nothing, or for an app's `start`, told apart from
//! windows, and never handed an `open`.

use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, APPLICATION_START_ACTION,
    ActionHostError, Caller, InvokeRequest, provider_workspace,
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
    /// Have the host run `app_id` with no window in `workspace`, for an action
    /// that shows nothing: how its start ends, where it does.
    pub(super) fn run_windowless(
        &self,
        app_id: &str,
        workspace: &str,
    ) -> Result<Option<crate::services::WindowlessStart>, ActionHostError> {
        let started = match (
            self.launchable.windowless(),
            self.services.launcher.as_ref(),
        ) {
            (Some(stand_in), _) => stand_in(app_id, workspace),
            (None, Some(launcher)) => launcher.windowless(app_id, workspace),
            (None, None) => Ok(None),
        };
        started.map_err(|why| ActionHostError::Unavailable {
            reason: format!("{app_id} could not run without a window: {why}"),
        })
    }

    /// The running instance of `app_id` in `workspace` that may take `action`
    /// and takes invocations now: what an app's `open` or `start` answers
    /// with once one runs.
    pub(super) fn running_instance(
        &self,
        app_id: &str,
        workspace: &str,
        action: &str,
    ) -> Option<u64> {
        let subscribers = self.invocation_subscribers.lock().ok()?;
        self.surfaces
            .lock()
            .ok()?
            .values()
            .filter(|surface| {
                surface.live
                    && self.shows(surface.peer_id, action)
                    && surface.app_id == app_id
                    && surface.route.cluster_id == workspace
                    && subscribers.contains_key(&surface.peer_id)
                    && !surface.actions.is_empty()
            })
            .map(|surface| surface.peer_id)
            .min()
    }

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
    ) -> Result<u64, ActionHostError> {
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
                descriptor: &super::action_host::app_start_descriptor(app_id, &name),
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
        local: Option<&super::native_media::LocalAppConnection>,
    ) {
        if local.is_some_and(|local| local.windowless)
            && let Ok(mut windowless) = self.launchable.windowless_peers.lock()
        {
            windowless.insert(peer_id);
        }
    }

    /// Peer `peer_id` disconnected: forget the window it was for, and that
    /// it had none.
    pub(super) fn forget_windows_of(&self, peer_id: u64) {
        if let Ok(mut windows) = self.window_instances.lock() {
            windows.remove(&peer_id);
        }
        if let Ok(mut windowless) = self.launchable.windowless_peers.lock() {
            windowless.remove(&peer_id);
        }
    }

    /// Whether peer `peer_id` may take an invocation of `action`: an
    /// instance with no window takes every action but `open`, which shows
    /// what it opens and so goes to a window.
    pub(super) fn shows(&self, peer_id: u64, action: &str) -> bool {
        action != APPLICATION_OPEN_ACTION
            || self
                .launchable
                .windowless_peers
                .lock()
                .is_ok_and(|windowless| !windowless.contains(&peer_id))
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

/// Why the start of an instance with no window failed, once it does;
/// pending while there is none to wait on, or once it runs. Cancel-safe: a
/// wait dropped before the start ended leaves `starting` as it was.
pub(super) async fn start_failed(
    starting: &mut Option<crate::services::WindowlessStart>,
    app_id: &str,
) -> ActionHostError {
    let Some(start) = starting.as_mut() else {
        return std::future::pending().await;
    };
    let ended = start.await;
    *starting = None;
    let why = match ended {
        Ok(Ok(())) => return std::future::pending().await,
        Ok(Err(why)) => why,
        Err(error) => {
            tracing::warn!(%error, app = app_id, "starting the app without a window did not finish");
            format!("the host stopped starting it ({error})")
        }
    };
    ActionHostError::Unavailable {
        reason: format!("{app_id} could not run without a window: {why}"),
    }
}
