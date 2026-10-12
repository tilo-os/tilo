//! Completion of a launch admitted on an application's original channel.
use super::{BrokerActor, CapnpChannelHandle, InvocationOwner, app_node};
use crate::services::LaunchReadiness;
use tilo_actions::InvocationId;

impl CapnpChannelHandle {
    pub(super) fn begin_original_launch(
        &self,
        app: &str,
        root: Option<&str>,
        workspace: &tilo_desktop_model::Workspace,
        author: Option<&str>,
        parent: Option<InvocationId>,
    ) -> Result<LaunchReadiness, String> {
        let original = self.launch_original_context(parent, app, root)?;
        self.state
            .services
            .launcher
            .as_ref()
            .ok_or_else(|| "application launch unsupported".to_string())?
            .begin_launch_from_original(app, root, workspace, author, original)
    }

    pub(super) fn launch_original_context(
        &self,
        parent: Option<InvocationId>,
        app: &str,
        root: Option<&str>,
    ) -> Result<Option<crate::services::ObservedApplicationLaunch>, String> {
        let Some(parent) = parent else {
            return Ok(None);
        };
        // Agent launch authority remains its existing continuation path; it
        // cannot become a Person delegation merely by requesting authorship.
        if self.launch_actor(Some(parent))?.is_agent() {
            return Ok(None);
        }
        let original = {
            let pending = self
                .state
                .pending_invocations
                .lock()
                .map_err(|_| "invocation registry unavailable")?;
            let mut matches = pending.values().filter(|entry| {
                entry.peer_id == self.peer_id
                    && entry.update.invocation() == parent
                    && entry.cluster_id == self.route().cluster_id
            });
            let entry = matches
                .next()
                .ok_or("original launch invocation disappeared")?;
            if matches.next().is_some()
                || entry.update.is_terminal()
                || entry.cancellation_requested_at.is_some()
            {
                return Err("original launch invocation is no longer active".into());
            }
            let original = entry
                .original
                .clone()
                .ok_or("original certified launch caller unavailable")?;
            drop(matches);
            drop(pending);
            original
        };
        let grant_original = original.clone();
        let state = std::sync::Arc::downgrade(&self.state);
        let route = self.route();
        let identity = self.verified_identity.clone();
        let peer = self.peer_id;
        let check = move || {
            let state = state.upgrade().ok_or("application launch broker stopped")?;
            state
                .revalidate_admission(&route, &identity)
                .map_err(|error| error.to_string())?;
            let pending = state
                .pending_invocations
                .lock()
                .map_err(|_| "invocation registry unavailable")?;
            let active = pending
                .values()
                .filter(|entry| entry.peer_id == peer && entry.update.invocation() == parent)
                .collect::<Vec<_>>();
            let valid = matches!(active.as_slice(), [entry]
                if entry.cluster_id == route.cluster_id && !entry.update.is_terminal()
                && entry.cancellation_requested_at.is_none()
                && entry.original.as_ref().is_some_and(|value| std::sync::Arc::ptr_eq(value, &original)));
            drop(active);
            drop(pending);
            if !valid {
                return Err("original launch invocation is no longer active".into());
            }
            let result = original.launch_person_certificate(&state, &route.cluster_id);
            drop(state);
            result
        };
        let check = std::sync::Arc::new(check);
        check()?;
        let grant_check = check.clone();
        let state = std::sync::Arc::downgrade(&self.state);
        let workspace_id = self.route().cluster_id;
        let grant_room = workspace_id.clone();
        let grant_app = app.to_owned();
        let grant = std::sync::Arc::new(move |certificate: &tilo_user_identity::DeviceCert| {
            if grant_check()? != *certificate {
                return Err("original launch certificate changed".into());
            }
            let state = state.upgrade().ok_or("application launch broker stopped")?;
            grant_original.launch_administrator_certificate(&state, &grant_room)?;
            let result = state.invoke_workspace_membership(&grant_room,
                    &tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::GrantAppAuthorship {
                        app: grant_app.clone(),
                    }, certificate);
            drop(state);
            result
        });
        Ok(Some(crate::services::ObservedApplicationLaunch::new(
            workspace_id,
            app.to_owned(),
            root.map(str::to_owned),
            check,
            grant,
        )))
    }

    pub(super) fn launch_actor(&self, parent: Option<InvocationId>) -> Result<BrokerActor, String> {
        let current = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
            .map_err(|error| error.to_string())?;
        let Some(parent) = parent else {
            return Ok(current.broker_actor());
        };
        let permitted = {
            let pending = self
                .state
                .pending_invocations
                .lock()
                .map_err(|_| "invocation registry unavailable")?;
            let matches = pending
                .values()
                .filter(|pending| {
                    pending.peer_id == self.peer_id && pending.update.invocation() == parent
                })
                .take(2)
                .cloned()
                .collect::<Vec<_>>();
            drop(pending);
            match matches.as_slice() {
                [invocation] => {
                    invocation.cluster_id == self.route().cluster_id
                        && invocation.cancellation_requested_at.is_none()
                        && invocation.original.as_ref().is_some_and(|original| {
                            original.permits_launch_in(&self.route().cluster_id)
                        })
                }
                _ => false,
            }
        };
        if !permitted {
            return Err("the original launch invocation is no longer permitted".into());
        }
        let caller = self
            .state
            .continuing_caller(self.peer_id, parent)
            .ok_or("the original launch caller is no longer admitted")?;
        Ok(self.state.caller_actor(&caller, &self.route().cluster_id))
    }

    fn current_launch_authority(
        &self,
        app: &str,
        parent: Option<InvocationId>,
    ) -> Result<(), String> {
        let actor = self.launch_actor(parent)?;
        let membership = self
            .state
            .membership
            .as_ref()
            .ok_or_else(|| "the action broker has no workspace authority".to_string())?;
        let now = self.state.policy_now_ns()? / 1_000_000_000;
        membership
            .decide_in_room(&self.route().cluster_id, |workspace| {
                let workspace = workspace.ok_or("the application workspace is no longer hosted")?;
                if !actor.may_in(
                    workspace,
                    &app_node(workspace, app),
                    tilo_action_policy::ActionKind::Undoable,
                    now,
                ) {
                    return Err("the caller is no longer allowed to launch that application");
                }
                Ok(())
            })?
            .map_err(str::to_owned)
    }

    pub(super) fn finish_observed_launch(
        &self,
        app: &str,
        actor: String,
        parent: Option<InvocationId>,
        id: u64,
        outcome: Result<(), String>,
    ) -> tilo_actions::InvocationUpdate {
        let outcome = outcome.and_then(|()| self.current_launch_authority(app, parent));
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
        let update = match outcome {
            Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
            Err(message) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "launch_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
        };
        self.state
            .record_completion(id, InvocationOwner::Local { actor }, update.clone());
        update
    }
}

pub(super) async fn wait_for_startup(
    started: Result<LaunchReadiness, String>,
) -> Result<(), String> {
    match started {
        Ok(readiness) => tokio::time::timeout(
            crate::services::APPLICATION_STARTUP_TIMEOUT,
            readiness.wait(),
        )
        .await
        .unwrap_or_else(|_| Err("the application did not open a window in time".into())),
        Err(error) => Err(error),
    }
}

pub(super) fn package_root(root: &str) -> Result<Option<String>, capnp::Error> {
    if root.is_empty() {
        return Ok(None);
    }
    if root.strip_prefix("tapp:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) {
        return Ok(Some(root.to_owned()));
    }
    Err(capnp::Error::failed(
        "invalid installed package root".into(),
    ))
}

pub(super) fn launch_parent(bytes: &[u8]) -> Result<Option<InvocationId>, capnp::Error> {
    if bytes.is_empty() {
        return Ok(None);
    }
    uuid::Uuid::from_slice(bytes)
        .map(|id| Some(InvocationId(id)))
        .map_err(|_| capnp::Error::failed("invalid original launch invocation".into()))
}

pub(super) fn launch_author(
    actor: &BrokerActor,
    workspace: &tilo_desktop_model::Workspace,
    now: u64,
) -> Option<String> {
    (actor.direct()
        && actor.may_in(
            workspace,
            &workspace.node(),
            tilo_action_policy::ActionKind::Undoable,
            now,
        ))
    .then(|| actor.id().to_owned())
}
