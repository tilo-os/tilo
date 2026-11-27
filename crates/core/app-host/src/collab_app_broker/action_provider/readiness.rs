//! What a decision here waits for when the session has not said it yet.
//!
//! A shell opens a workspace in one pass and claims it in the same; the copy of the shell the broker
//! decides against is published on the way. An action that arrives before the pass has said whose
//! the workspace is waits for that pass rather than being refused for a workspace that is about to
//! be the person's own.

use std::time::Duration;

use super::super::app_node;
use super::{
    AppBrokerState, BrokerActor, LaunchScope, app_open_descriptor, launch_content_hash, refusal,
};
use crate::collab_app_broker::Arc;
use tilo_actions::remote::{APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX};
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest};
use tilo_desktop_model::Workspace;

/// How long an action waits for the shell to say who the workspace it names is whose, when the shell
/// has not said yet: a shell claims the workspace it has just opened in the pass that shows it, and
/// a pass takes milliseconds once the session runs.
const WORKSPACE_OWNER_WAIT: Duration = Duration::from_secs(5);

impl AppBrokerState {
    /// `workspace_id` decided once this dispatch holds it, waiting [`WORKSPACE_OWNER_WAIT`] for it.
    pub(super) async fn workspace_once_joined<R>(
        &self,
        caller: &Caller,
        workspace_id: &str,
        decide: impl Fn(&Workspace, &BrokerActor, u64) -> R,
    ) -> Option<R> {
        let until = tilo_clock::now().checked_add(WORKSPACE_OWNER_WAIT)?;
        let slice = WORKSPACE_OWNER_WAIT.div_f32(25.0);
        loop {
            if let Some(answer) = self.decide_for(caller, workspace_id, &decide) {
                return Some(answer);
            }
            if tilo_clock::now() >= until {
                return None;
            }
            tokio::time::sleep(slice).await;
        }
    }
}

impl AppBrokerState {
    /// The workspace's `open` of `app`, started in `workspace`, admitted under the caller's ownership,
    /// and `None` when the action is not an app's `open` at all.
    pub(super) fn start_app_open(
        self: &Arc<Self>,
        caller: &Caller,
        workspace_id: &str,
        launch: Option<&LaunchScope>,
        origin: Option<&super::super::InvocationCaller>,
        request: &InvokeRequest,
        argument: Option<&str>,
        (workspace, policy, now): (Workspace, BrokerActor, u64),
    ) -> Result<Option<u64>, ActionCallError> {
        let address = &request.target.address;
        let Some(app_id) = address
            .resource
            .strip_prefix(APPLICATION_RESOURCE_PREFIX)
            .filter(|_| address.action == APPLICATION_OPEN_ACTION)
        else {
            return Ok(None);
        };
        if caller.workspace().is_none() {
            tracing::warn!("opening an app was refused: the caller names no workspace");
            return Err(ActionCallError::UnknownAction);
        }
        if !policy.may_in(
            &workspace,
            &app_node(&workspace, app_id),
            tilo_action_policy::ActionKind::Undoable,
            now,
        ) {
            let node = app_node(&workspace, app_id);
            tracing::warn!(
                %app_id,
                caller = %policy.actor(),
                owner = workspace.policy.owner().unwrap_or("none"),
                direct = policy.direct(),
                answer = ?policy.answer_in(&workspace, &node, tilo_action_policy::ActionKind::Undoable, now),
                "opening an app was refused: the workspace does not let the caller open it"
            );
            return Err(ActionCallError::NotPermitted);
        }
        if !launch_content_hash(argument).is_empty() && launch.is_none() {
            return Err(refusal(
                "launch content requires an application the launcher authenticated".into(),
            ));
        }
        let author = caller.acting().person_id().filter(|_| {
            policy.may_in(
                &workspace,
                &workspace.policy.node(),
                tilo_action_policy::ActionKind::Undoable,
                now,
            )
        });
        let app = self
            .services
            .launcher
            .as_ref()
            .and_then(|launcher| {
                launcher
                    .applications()
                    .into_iter()
                    .find(|app| app.package_id == app_id)
            })
            .ok_or(ActionCallError::UnknownAction)?;
        let admitted = self.admit_host_action(
            caller,
            origin,
            super::super::host_action_admission::HostActionAdmission {
                workspace: workspace_id,
                app_id,
                descriptor: &app_open_descriptor(&app),
                request,
            },
        )?;
        let open = super::install_on_open::HeldOpen {
            caller: caller.clone(),
            origin: origin.cloned(),
            workspace: workspace_id.to_owned(),
            app: app_id.to_owned(),
            hash: launch_content_hash(argument).into_owned(),
            scope: launch.cloned(),
            author: author.map(str::to_owned),
            place: self.launch_place(caller, workspace_id, launch),
        };
        // Opening an app the workspace runs no version of is the person asking to use it here:
        // it is installed as them first, and the open answers once it launched or was refused.
        if let Some(release) = Self::install_before_open(caller, &workspace, &app) {
            let id = admitted.id;
            self.install_then_open(&release, admitted, open, app.display_name);
            return Ok(Some(id));
        }
        open.launch(self, admitted).map(Some)
    }
}
