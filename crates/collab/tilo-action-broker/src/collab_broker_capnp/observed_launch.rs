//! Completion of a launch admitted on an application's original channel.
use super::{BrokerActor, CapDecision, CapnpChannelHandle, InvocationOwner, ResourceRef, Verb};
use crate::services::LaunchReadiness;
use tilo_actions::InvocationId;

impl CapnpChannelHandle {
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
        let tree = membership.snapshot()?;
        let workspace = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == self.route().cluster_id)
            .ok_or_else(|| "the application workspace is no longer hosted".to_string())?;
        let now = self.state.policy_now_ns()? / 1_000_000_000;
        let caller = actor.policy_in(workspace, now);
        if workspace.decide_at(&caller, &ResourceRef::App(app.to_owned()), Verb::Write, now)
            != CapDecision::Allow
        {
            return Err("the caller is no longer allowed to launch that application".into());
        }
        Ok(())
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
    (!actor.is_agent()
        && !actor.id().starts_with("app:")
        && workspace.decide_at(
            &actor.policy_in(workspace, now),
            &ResourceRef::Workspace,
            Verb::Write,
            now,
        ) == CapDecision::Allow)
        .then(|| actor.id().to_owned())
}
