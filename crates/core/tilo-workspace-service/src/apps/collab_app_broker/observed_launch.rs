//! An application's open admitted on its original channel: who asked, what they may open, and
//! how the view it starts is reported.
use super::host_action_admission::HostActionAdmission;
use crate::apps::BrokerError;
use tilo_actions::InvocationId;

fn launch_descriptor(app: &str) -> tilo_actions::ActionDescriptor {
    tilo_actions::ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(app, "launch"),
        resource_kind: "application".into(),
        resource_label: app.into(),
        action_label: "Open".into(),
        keywords: Vec::new(),
        icon: None,
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
    }
}

/// The one running invocation `parent` in `workspace` that may launch an
/// application: the peer it runs for and what it was invoked with.
fn launching_invocation(
    pending: &std::collections::HashMap<u64, super::PendingInvocation>,
    parent: InvocationId,
    workspace: &str,
) -> Result<(u64, std::sync::Arc<super::OriginalInvocationData>), String> {
    let matches = pending
        .values()
        .filter(|entry| entry.update.invocation() == parent)
        .take(2)
        .collect::<Vec<_>>();
    let [entry] = matches.as_slice() else {
        return Err("the invocation the open is for is not running".into());
    };
    if entry.cluster_id != workspace
        || entry.update.is_terminal()
        || entry.cancellation_requested_at.is_some()
    {
        return Err("the invocation the open is for is not running here".into());
    }
    let original = entry
        .original
        .clone()
        .filter(|original| original.permits_launch_in(workspace))
        .ok_or("the invocation the open is for may not launch an application")?;
    Ok((entry.peer_id, original))
}

/// The host-launch request `invocation` makes for `descriptor`, continuing
/// `parent` when it has one.
fn launch_request(
    invocation: InvocationId,
    parent: Option<InvocationId>,
    descriptor: &tilo_actions::ActionDescriptor,
) -> tilo_actions::remote::InvokeRequest {
    use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
    InvokeRequest {
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
        signed: None,
    }
}

/// What an app's open for an invocation it names comes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppOpen {
    /// The invocation's caller admits it, and it is journaled as its child.
    Admitted,
    /// The app asked that invocation of itself: only the person's press in
    /// one of its windows opens for it.
    NeedsPress,
}

impl super::AppBrokerState {
    /// Admit app `caller_app`'s request, made in `workspace` through the shell's
    /// tools service, to open `target` for `parent`: an invocation the
    /// broker delivered to one of `caller_app`'s connections in `workspace` that
    /// is still running and may launch there, whose caller (an agent, say)
    /// may still open `target` in that workspace. An invocation the app made
    /// of itself through its own connection is no one else's: an open for it
    /// [`AppOpen::NeedsPress`]. An admitted open is journaled as
    /// a child of `parent`, attributed to that caller, as a launch the app
    /// asks the broker for is (`JoinedChannel::admit_observed_launch`).
    ///
    /// # Errors
    ///
    /// Why the open is not `parent`'s to make: no such running invocation,
    /// another app's or another workspace's, a caller no longer
    /// admitted or not allowed to open `target`, or a journal that could not
    /// record it.
    pub fn admit_app_open(
        &self,
        caller_app: &str,
        workspace: &str,
        parent: InvocationId,
        target: &str,
    ) -> Result<AppOpen, String> {
        let pending = self
            .pending_invocations
            .lock()
            .map_err(|_| "invocation registry unavailable")?;
        let launching = launching_invocation(&pending, parent, workspace);
        drop(pending);
        let (peer_id, original) = launching?;
        if !original.delivered_to(caller_app, workspace) {
            return Err("the invocation the open is for is another app's".into());
        }
        if original.through_app_api() {
            return Ok(AppOpen::NeedsPress);
        }
        let caller = self
            .continuing_caller(peer_id, parent)
            .ok_or("the invocation's caller is no longer admitted")?;
        let actor = self.caller_actor(&caller, workspace);
        let membership = self
            .membership
            .as_ref()
            .ok_or_else(|| "the action broker has no workspace authority".to_string())?;
        let now = self.policy_now_ns()? / 1_000_000_000;
        membership
            .decide_in_workspace(workspace, |workspace| {
                let workspace = workspace.ok_or("the workspace is no longer hosted")?;
                if !actor.may_in(
                    workspace,
                    &super::app_node(workspace, target),
                    tilo_action_policy::ActionKind::Undoable,
                    now,
                ) {
                    return Err("the caller may not open that application");
                }
                Ok(())
            })?
            .map_err(str::to_owned)?;
        let invocation = InvocationId::new();
        let descriptor = launch_descriptor(target);
        let request = launch_request(invocation, Some(parent), &descriptor);
        let admitted = self
            .admit_host_action(
                &caller,
                Some(&original.launch_child_caller(invocation)),
                HostActionAdmission {
                    workspace,
                    app_id: target,
                    descriptor: &descriptor,
                    request: &request,
                },
            )
            .map_err(|error| error.to_string())?;
        self.complete_host_action(
            &admitted,
            tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
        )
        .map_err(|error| error.to_string())?;
        Ok(AppOpen::Admitted)
    }
}

pub(super) fn package_root(root: &str) -> Result<Option<String>, BrokerError> {
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
    Err(BrokerError::failed("invalid installed package root".into()))
}

pub(super) fn launch_parent(bytes: &[u8]) -> Result<Option<InvocationId>, BrokerError> {
    if bytes.is_empty() {
        return Ok(None);
    }
    uuid::Uuid::from_slice(bytes)
        .map(|id| Some(InvocationId(id)))
        .map_err(|_| BrokerError::failed("invalid original launch invocation".into()))
}
