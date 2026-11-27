//! A workspace of a person's own that a host keeping their workspaces makes for a key acting for
//! them (an agent's), which it founds nothing for: their device founds it.
//!
//! The host hands the key's own call, as the key signed it for no workspace, to the device that
//! founds the person's workspaces, over the action service both serve
//! ([`Workspaces::found_through_their_device`]). That device decides it as the key's call (the
//! signature proves the key, fresh and once), asks the person there where the key's grants leave
//! it to them, founds the workspace on their yes, places it at the host that carried the call and
//! invites the key in at the address the host signed for it. The host founds nothing, issues
//! nothing and takes no one's word: it keeps the workspace it was handed as the person's own and
//! answers the key with what their device answered, a wait for their yes and a no among it.

use std::sync::Arc;

use tilo_actions::InvocationUpdate;
use tilo_actions::remote::asked::RecordRef;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvocationState, InvokeRequest, ReceiptStatus,
    RemoteActionMessage, UpdateMark,
};
use tilo_peer_state::actions::{CHANGE_WAIT_MS, RemoteDevice};
use tilo_server_protocol::workspaces::{CreatedWorkspace, WorkspaceLabels};
use tilo_user_identity::DeviceCert;

use super::persons_own;
use crate::workspaces::providers::{Answered, Live};
use crate::workspaces::{WeakWorkspaces, Workspaces};

/// How long the host waits for the person's founding device to say how the make stands at all.
const ANSWERED_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);

impl Workspaces {
    /// Have the device that founds `device`'s person's workspaces make the one `request` asks
    /// for, as `caller` asked it here, where this host founds nothing for that key
    /// ([`Self::found_for`]): how that device answered, once it made it or while it waits for
    /// the person's yes there. `None` where this host founds it itself, keeps no workspaces for
    /// the person, the call carries no signature of the key's own, or no workspace kept here
    /// names the person's founding device: it is then decided here as any other.
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
        let Some(own) = self.own() else {
            return Ok(None);
        };
        let name = args.text("name")?;
        let labels: Option<WorkspaceLabels> = args.object("labels")?;
        let labels = labels.as_ref();
        // What is theirs already is answered here: a workspace the ask names, the first of
        // theirs where it names none, or the one a retried ask labelled.
        if args.text("workspace")?.is_some()
            || (name.is_none() && own.any_of(&person))
            || self
                .own_labelled_for(person, (name, None), labels, own)
                .await
                .is_some()
        {
            return Ok(None);
        }
        let signed_by_the_key = request
            .signed
            .as_ref()
            .is_some_and(|signed| signed.statement().signer == device.device_public_key);
        if caller.forwarder().is_some()
            || !signed_by_the_key
            || caller.is_in_process()
            || persons_own((caller, device))
            || self.founding_here(&person).await.is_ok()
            || !self.let_in(&person, now).await
        {
            return Ok(None);
        }
        let Some((founding, addr)) = self.persons_founding_device(&person).await else {
            return Ok(None);
        };
        let remote = Arc::new(
            self.action_service()
                .remote(&self.inner.host.endpoint, &addr, ""),
        );
        if remote.device() != founding {
            return Ok(None);
        }
        let mut carried = request.clone();
        carried.wait_ms = Some(CHANGE_WAIT_MS);
        let invocation = carried.invocation;
        let unanswered = || ActionCallError::Unavailable {
            reason: "the device that makes your workspaces didn't answer".into(),
        };
        let receipt = tokio::time::timeout(ANSWERED_WITHIN, remote.invoke(caller, carried))
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
        // Followed until it ends there, or waits there for the person's yes: the key then follows
        // it here, for as long as they take.
        loop {
            if InvocationState::new(invocation, update.clone()).is_terminal() {
                return match self.took_in(person, labels, update).await {
                    InvocationUpdate::Succeeded { output, .. } => {
                        Ok(Some(Answered::Value(output.value.unwrap_or_default())))
                    }
                    ended => Err(refusal(ended)),
                };
            }
            if RecordRef::waited_in(&update).is_some() {
                break;
            }
            update = tokio::time::timeout(
                ANSWERED_WITHIN,
                next_update(&remote, invocation, UpdateMark::of(&update)),
            )
            .await
            .map_err(|_| unanswered())??;
        }
        let (moved, following) = tokio::sync::watch::channel(Live::There(update.clone()));
        let workspaces = self.downgrade();
        let labels = labels.cloned();
        tokio::spawn(async move {
            follow_there(
                (&remote, invocation, update),
                &moved,
                (&workspaces, person, labels.as_ref()),
            )
            .await;
        });
        Ok(Some(Answered::Live(following)))
    }

    /// `ended`, how the person's founding device ended a make it was handed: one that made the
    /// workspace has it kept here as the person's own, labelled as asked, once it was placed
    /// here.
    async fn took_in(
        &self,
        person: [u8; 32],
        labels: Option<&WorkspaceLabels>,
        ended: InvocationUpdate,
    ) -> InvocationUpdate {
        let InvocationUpdate::Succeeded { invocation, output } = &ended else {
            return ended;
        };
        let failed = |message: String| InvocationUpdate::Failed {
            invocation: *invocation,
            error: tilo_actions::ActionFailure {
                code: "refused".into(),
                message,
                details: None,
                retryable: false,
            },
        };
        let made = match serde_json::from_value::<CreatedWorkspace>(
            output.value.clone().unwrap_or_default(),
        ) {
            Ok(made) => made,
            Err(error) => {
                return failed(format!(
                    "the device that makes your workspaces answered no workspace: {error}"
                ));
            }
        };
        let workspace = made.workspace.workspace;
        if self.state(&workspace).await.is_none() {
            return failed(format!(
                "the workspace {workspace} was made, and not placed on this host"
            ));
        }
        if let Some(own) = self.own()
            && let Err(error) = own.record(&person, &workspace)
        {
            return failed(error);
        }
        if let Some(labels) = labels
            && let Err(error) = self.label(&workspace, labels).await
        {
            return failed(error.to_string());
        }
        ended
    }
}

/// Follow `invocation`, which the person's founding device `remote` stood at `update` for, there
/// until it ends, telling `moved` each change: once it made the workspace, `workspaces` keeps it
/// as `person`'s own ([`Workspaces::took_in`]). Stops when no one follows it here any more.
async fn follow_there(
    (remote, invocation, mut update): (
        &Arc<RemoteDevice>,
        tilo_actions::InvocationId,
        InvocationUpdate,
    ),
    moved: &tokio::sync::watch::Sender<Live>,
    (workspaces, person, labels): (&WeakWorkspaces, [u8; 32], Option<&WorkspaceLabels>),
) {
    loop {
        let next = tokio::select! {
            () = moved.closed() => return,
            next = next_update(remote, invocation, UpdateMark::of(&update)) => next,
        };
        update = match next {
            Ok(next) => next,
            Err(error) => {
                moved.send_replace(Live::Ended(Err(format!(
                    "the device that makes your workspaces stopped answering: {error}"
                ))));
                return;
            }
        };
        if InvocationState::new(invocation, update.clone()).is_terminal() {
            let ended = match workspaces.upgrade() {
                Some(workspaces) => workspaces.took_in(person, labels, update).await,
                None => return,
            };
            moved.send_replace(Live::There(ended));
            return;
        }
        moved.send_replace(Live::There(update.clone()));
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
