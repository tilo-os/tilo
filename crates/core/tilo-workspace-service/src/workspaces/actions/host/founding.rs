//! A workspace a person's own device asks a server of theirs to make, where that server is not
//! the device that founds their workspaces: their founding device founds it.
//!
//! The server hands the call to the device that founds the person's workspaces, over the action
//! service both serve ([`Workspaces::found_through_their_device`]). The call carries no signature
//! that device could check, so the server asks it as itself, a device of theirs, in the person's
//! own workspace: the founding device founds it, places it at their server and lists it with
//! their others. The server founds nothing: it keeps the workspace it was handed as the person's
//! own and answers with what their device answered. An agent's make goes the other way, kept
//! for the person's root in the asked-calls record ([`crate::workspaces::at_root`]).

use std::sync::Arc;

use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvocationState, InvokeRequest, ReceiptStatus,
    RemoteActionMessage, UpdateMark,
};
use tilo_peer_state::actions::{CHANGE_WAIT_MS, RemoteDevice};
use tilo_server_protocol::workspaces::WorkspaceLabels;
use tilo_user_identity::DeviceCert;

use super::persons_own;
use crate::workspaces::Workspaces;
use crate::workspaces::providers::Answered;

/// How long the host waits for the person's founding device to say how the make stands at all.
const ANSWERED_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);

impl Workspaces {
    /// Have the device that founds `device`'s person's workspaces make the one `request` asks
    /// for, as `caller` asked it here, where this host does not found it ([`Self::found_for`]):
    /// how that device answered. `None` where this host founds it itself, the call is not
    /// asked by a device of this host's own person, or no workspace kept here names the
    /// person's founding device: it is then decided here as any other.
    ///
    /// # Errors
    /// The founding device did not answer, refused, or the person said no there.
    pub(super) async fn found_through_their_device(
        &self,
        (caller, device): (&Caller, &DeviceCert),
        request: &InvokeRequest,
        (args, now): (&super::super::Args, u64),
    ) -> Result<Option<Answered>, ActionCallError> {
        let person = device.user_public_key;
        let labels: Option<WorkspaceLabels> = args.object("labels")?;
        let labels = labels.as_ref();
        if self.theirs_already(person, args, labels).await? {
            return Ok(None);
        }
        let here = self.device_cert().device_public_key;
        let as_here = persons_own((caller, device)) && self.device_cert().user_public_key == person;
        if caller.forwarder().is_some()
            || !as_here
            || caller.is_in_process()
            || self.founding_here(&person).await.is_ok()
            || !self.let_in(&person, now).await
        {
            return Ok(None);
        }
        let Some((founding, addr)) = self.persons_founding_device(&person).await else {
            return Ok(None);
        };
        // This host proves itself in the person's own workspace, which both keep, as a device of
        // theirs.
        let own_workspace = tilo_server_protocol::servers::person_workspace_id(&person);
        let remote = Arc::new(self.action_service().remote(
            &self.inner.host.endpoint,
            &addr,
            own_workspace.as_str(),
        ));
        if remote.device() != founding {
            return Ok(None);
        }
        let mut carried = request.clone();
        carried.wait_ms = Some(CHANGE_WAIT_MS);
        let invocation = carried.invocation;
        carried.signed = None;
        let caller = Caller::authenticated(person, here).in_workspace(own_workspace.clone());
        let unanswered = || ActionCallError::Unavailable {
            reason: "the device that makes your workspaces didn't answer".into(),
        };
        let receipt = tokio::time::timeout(ANSWERED_WITHIN, remote.invoke(&caller, carried))
            .await
            .map_err(|_| unanswered())?
            .ok_or_else(unanswered)??;
        let mut update = match receipt.status {
            ReceiptStatus::Completed { output } => {
                InvocationUpdate::Succeeded { invocation, output }
            }
            ReceiptStatus::Failed { error } => InvocationUpdate::Failed { invocation, error },
            ReceiptStatus::Cancelled { message } => InvocationUpdate::Cancelled {
                invocation,
                message,
            },
            ReceiptStatus::Rejected { error } => return Err(error),
            ReceiptStatus::Accepted => InvocationUpdate::Accepted { invocation },
        };
        // Followed until it ends there: a device of theirs asking founds without a question.
        loop {
            if InvocationState::new(invocation, update.clone()).is_terminal() {
                return match self.took_in(person, request, update).await {
                    InvocationUpdate::Succeeded { output, .. } => {
                        Ok(Some(Answered::Value(output.value.unwrap_or_default())))
                    }
                    ended => Err(refusal(ended)),
                };
            }
            update = tokio::time::timeout(
                ANSWERED_WITHIN,
                next_update(&remote, invocation, UpdateMark::of(&update)),
            )
            .await
            .map_err(|_| unanswered())??;
        }
    }

    /// Whether what `args` asks of `person` is theirs already, and answered here: a workspace
    /// the ask names, the first of theirs where it names none, or the one a retried ask labelled.
    async fn theirs_already(
        &self,
        person: [u8; 32],
        args: &super::super::Args,
        labels: Option<&WorkspaceLabels>,
    ) -> Result<bool, ActionCallError> {
        let name = args.text("name")?;
        if args.text("workspace")?.is_some() {
            return Ok(true);
        }
        let Some(own) = self.own() else {
            return Ok(false);
        };
        Ok((name.is_none() && own.any_of(&person))
            || self
                .own_labelled_for(person, (name, None), labels, own)
                .await
                .is_some())
    }
}

/// The next update of `invocation` on `remote` after the one `seen` marks.
async fn next_update(
    remote: &RemoteDevice,
    invocation: tilo_actions::InvocationId,
    seen: UpdateMark,
) -> Result<InvocationUpdate, ActionCallError> {
    loop {
        let answer = remote
            .ask_message(RemoteActionMessage::Observe {
                correlation: 0,
                invocation,
                wait_ms: Some(CHANGE_WAIT_MS),
                seen: Some(seen),
            })
            .await
            .unwrap_or(Err(ActionCallError::Unanswered))?;
        match answer {
            RemoteActionMessage::Update {
                state: Some(state), ..
            } if UpdateMark::of(&state.update) != seen => return Ok(state.update),
            RemoteActionMessage::Update { state: Some(_), .. } => {}
            RemoteActionMessage::Update { state: None, .. } => {
                return Err(ActionCallError::UnknownInvocation);
            }
            _ => return Err(ActionCallError::Unanswered),
        }
    }
}

/// What the key hears of a make the person's founding device ended without making it.
fn refusal(ended: InvocationUpdate) -> ActionCallError {
    match ended {
        InvocationUpdate::Failed { error, .. } => ActionCallError::refused(error.message),
        InvocationUpdate::Cancelled { message, .. } => {
            ActionCallError::refused(message.unwrap_or_default())
        }
        other => ActionCallError::refused(format!("{other:?}")),
    }
}
