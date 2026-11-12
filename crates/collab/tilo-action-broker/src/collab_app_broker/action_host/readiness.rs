//! What a decision here waits for when the session has not said it yet.
//!
//! A desk opens a workspace in one pass and claims it in the same; the copy of the desk the broker
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
use tilo_actions::remote::{ActionHostError, Caller, CallerPrincipal, InvokeRequest};
use tilo_desktop_model::Workspace;

/// How long an action waits for the desk to say who the workspace it names is whose, when the desk
/// has not said yet: a desk claims the workspace it has just opened in the pass that shows it, and
/// a pass takes milliseconds once the session runs.
const WORKSPACE_OWNER_WAIT: Duration = Duration::from_secs(5);

impl AppBrokerState {
    /// `workspace_id` as the desk names its owner, waiting at most [`WORKSPACE_OWNER_WAIT`] for the
    /// desk to say whose it is, and `None` when it never does: a workspace joined through a door
    /// whose host has not said whose it is here is nobody's, and stays so.
    pub(super) fn workspace_naming_its_owner(
        &self,
        caller: &Caller,
        workspace_id: &str,
    ) -> Option<(Workspace, BrokerActor)> {
        let until = tilo_clock::now().checked_add(WORKSPACE_OWNER_WAIT)?;
        let slice = WORKSPACE_OWNER_WAIT.div_f32(25.0);
        let mut sample = None;
        while tilo_clock::now() < until {
            if let Some((workspace, actor, _)) =
                self.decide_for(caller, workspace_id, |workspace, policy, now| {
                    (workspace.clone(), policy.clone(), now)
                })
            {
                if workspace.owner().is_some() {
                    return Some((workspace, actor));
                }
                sample = Some((workspace, actor));
            }
            wait_for_a_change(slice);
        }
        sample
    }
}

/// Wait at most `wait` for something every decision here reads to change: the desk's publications,
/// the identity store and the device fleet all announce, so nothing is polled.
pub(super) fn wait_for_a_change(wait: Duration) {
    let (woken, wake) = std::sync::mpsc::channel();
    let watch = tilo_identity_host::policy_change::Watch::new(move || {
        let _ = woken.send(());
    });
    let seen = watch.arm();
    // A change that arrived between reading the desk and arming the watch is not woken for: it is
    // already there to read.
    if tilo_identity_host::policy_change::epoch() == seen {
        let _ = wake.recv_timeout(wait);
    }
}

impl AppBrokerState {
    /// The workspace's `open` of `app`, started in `workspace`, admitted under the caller's custody,
    /// and `None` when the action is not an app's `open` at all.
    pub(super) fn start_app_open(
        self: &Arc<Self>,
        caller: &Caller,
        workspace_id: &str,
        launch: Option<&LaunchScope>,
        origin: Option<&super::super::InvocationCaller>,
        request: &InvokeRequest,
        argument: Option<&str>,
        (mut workspace, mut policy, now): (Workspace, BrokerActor, u64),
    ) -> Result<Option<u64>, ActionHostError> {
        let address = &request.target.address;
        let Some(app_id) = address
            .resource
            .strip_prefix(APPLICATION_RESOURCE_PREFIX)
            .filter(|_| address.action == APPLICATION_OPEN_ACTION)
        else {
            return Ok(None);
        };
        // A desk claims a workspace it has just opened in the pass that shows it, and says so on
        // the way. An action arriving in the moment before waits for that pass — the person is
        // never refused in a workspace that is about to be theirs — and decides on the desk's
        // next word.
        if workspace.owner().is_none()
            && let Some((named, actor)) = self.workspace_naming_its_owner(caller, workspace_id)
        {
            workspace = named;
            policy = actor;
        }
        if caller.workspace().is_none() {
            tracing::warn!("opening an app was refused: the caller names no workspace");
            return Err(ActionHostError::UnknownAction);
        }
        // An app whose model this session runs has no window to open: answer its `open` at
        // once, naming the resource the session places
        // (`docs/designs/app-language/one-app-model.md` §3).
        if let Some(reason) = self.served_open_refusal(app_id) {
            return Err(ActionHostError::Unavailable { reason });
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
                caller = %policy.id,
                owner = workspace.owner().map_or("none", |owner| owner.as_str()),
                direct = policy.direct(),
                answer = ?policy.answer_in(&workspace, &node, tilo_action_policy::ActionKind::Undoable, now),
                "opening an app was refused: the workspace does not let the caller open it"
            );
            return Err(ActionHostError::NotPermitted);
        }
        if !launch_content_hash(argument).is_empty() && launch.is_none() {
            return Err(refusal(
                "launch content requires an application the launcher authenticated".into(),
            ));
        }
        let author = match caller.principal() {
            CallerPrincipal::Person(person)
                if policy.may_in(
                    &workspace,
                    &workspace.node(),
                    tilo_action_policy::ActionKind::Undoable,
                    now,
                ) =>
            {
                Some(person.as_str())
            }
            _ => None,
        };
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
            .ok_or(ActionHostError::UnknownAction)?;
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
        self.launch_application(
            (caller, origin, workspace_id),
            admitted,
            app_id,
            crate::services::LaunchContent {
                hash: launch_content_hash(argument),
                caller: launch,
                author,
            },
            &self.launch_place(caller, workspace_id, launch),
        )
        .map(Some)
    }
}
