//! Completion of a launch admitted on an application's original channel.
use super::host_action_admission::{AdmittedHostAction, HostActionAdmission};
use super::{BrokerActor, CapnpChannelHandle, InvocationCaller, app_node};
use crate::services::LaunchReadiness;
use tilo_actions::InvocationId;

#[cfg(test)]
thread_local! {
    static EFFECT_BARRIER: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
impl super::CapnpBrokerState {
    pub(crate) fn set_observed_launch_effect_hook(&self, hook: impl FnOnce() + 'static) {
        EFFECT_BARRIER.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }
}

impl CapnpChannelHandle {
    #[cfg(test)]
    pub(super) fn observed_launch_effect_barrier(&self) {
        if let Some(hook) = EFFECT_BARRIER.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
    }

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
                        && !invocation.update.is_terminal()
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

    pub(super) fn admit_observed_launch(
        &self,
        app: &str,
        root: Option<&str>,
        parent: Option<InvocationId>,
    ) -> Result<AdmittedHostAction, String> {
        use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
        let invocation = InvocationId::new();
        // A continuation belongs to its starter, not the Store worker that
        // happens to carry this RPC. Never reconstruct custody from an actor.
        let (caller, origin) = if let Some(parent) = parent {
            self.launch_actor(Some(parent))?;
            let caller = self
                .state
                .continuing_caller(self.peer_id, parent)
                .ok_or("original launch caller unavailable")?;
            let pending = self
                .state
                .pending_invocations
                .lock()
                .map_err(|_| "invocation registry unavailable")?;
            let mut entries = pending.values().filter(|entry| {
                entry.peer_id == self.peer_id && entry.update.invocation() == parent
            });
            let entry = entries
                .next()
                .ok_or("original launch invocation disappeared")?;
            if entries.next().is_some()
                || entry.update.is_terminal()
                || entry.cancellation_requested_at.is_some()
            {
                return Err("original launch invocation is no longer active".into());
            }
            let origin = entry
                .original
                .as_ref()
                .ok_or("original certified launch caller unavailable")?
                .launch_child_caller(invocation);
            (caller, origin)
        } else {
            let route = self.route();
            let current = self
                .state
                .revalidate_admission(&route, &self.verified_identity)
                .map_err(|error| error.to_string())?;
            let caller = if current.certificate.is_some() {
                current.certified_caller(&route.cluster_id, false)
            } else {
                current.caller(&route.cluster_id)
            };
            (
                caller,
                InvocationCaller::Local {
                    route,
                    admitted: Box::new(self.verified_identity.clone()),
                },
            )
        };
        let descriptor = tilo_actions::ActionDescriptor {
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new(app, "launch"),
            resource_kind: "application".into(),
            resource_label: app.into(),
            action_label: "Open".into(),
            description: None,
            locus: tilo_actions::ExecutionLocus::default(),
            kind: tilo_actions::OperationKind::Effect,
            contract: tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Undoable,
                ..Default::default()
            },
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        };
        let request = InvokeRequest {
            invocation,
            on_behalf_of: parent,
            target: RemoteActionTarget {
                provider: "host-launch".into(),
                address: descriptor.address.clone(),
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        };
        self.state
            .admit_host_action_with_start(
                &caller,
                Some(&origin),
                HostActionAdmission {
                    room: &self.route().cluster_id,
                    app_id: app,
                    descriptor: &descriptor,
                    request: &request,
                },
                Some(serde_json::json!({"package_root": root})),
            )
            .map_err(|error| error.to_string())
    }

    pub(super) fn current_launch_authority(
        &self,
        app: &str,
        parent: Option<InvocationId>,
    ) -> Result<(tilo_desktop_model::Workspace, BrokerActor, u64), String> {
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
                Ok(workspace.clone())
            })?
            .map_err(str::to_owned)
            .map(|workspace| (workspace, actor, now))
    }

    pub(super) fn finish_observed_launch(
        &self,
        app: &str,
        parent: Option<InvocationId>,
        admitted: &AdmittedHostAction,
        outcome: Result<Option<crate::services::LaunchAdoption>, String>,
    ) -> Result<tilo_actions::InvocationUpdate, capnp::Error> {
        let outcome = outcome.and_then(|adoption| {
            self.current_launch_authority(app, parent)?;
            if let Some(window) = &adoption
                && (window.package_id != app
                    || window.room != self.route().cluster_id
                    || window.surface_id == 0
                    || window.adoption_id.is_nil())
            {
                return Err("the adopted application window has foreign custody".into());
            }
            Ok(adoption)
        });
        let invocation = admitted.invocation;
        let update = match outcome {
            Ok(adoption) => tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput {
                    value: adoption.map(|window| serde_json::json!({"adopted_window": window})),
                    ..tilo_actions::ActionOutput::default()
                },
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
            .complete_host_action(admitted, update.clone())
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
        Ok(update)
    }
}

pub(super) async fn wait_for_startup(
    started: Result<LaunchReadiness, String>,
) -> Result<Option<crate::services::LaunchAdoption>, String> {
    match started {
        Ok(readiness) => tokio::time::timeout(
            crate::services::APPLICATION_STARTUP_TIMEOUT,
            readiness.wait_with_adoption(),
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
