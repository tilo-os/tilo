//! A workspace's model asked from a device ([`person_models::ASK`]), as the
//! workspace's policy lets the caller, answered as it is written.
//!
//! The ask is admitted at once and answered on the server's runtime. While the model
//! writes, the invocation's state is an item holding the answer so far (its
//! words and the reasoning it shows), each newer than the last; a device that
//! follows the invocation reads the newest one and shows what it has not shown
//! yet, so an item it never read loses it nothing. The turn's whole answer is
//! the invocation's output once it ends, as for any other action.

use serde_json::json;
use tilo_actions::remote::{
    ActionCallError, Caller, InvocationReceipt, InvocationState, InvokeRequest, Pending,
    person_models,
};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::{FOLLOW_WAKE, ServiceWorkspaceActions as Actions, failed, succeeded};
use crate::workspaces::Workspaces;
use crate::workspaces::workspace_stores::Store;
use agent_provider_config::protocol::StreamEvent;

/// What a running ask has written so far.
#[derive(Default)]
struct Written {
    text: String,
    reasoning: String,
}

/// Whether `request` is an ask of the workspace's model, which
/// [`Actions::ask_model`] answers.
pub(super) fn is_model_ask(request: &InvokeRequest) -> bool {
    request.target == person_models::target(person_models::ASK)
}

impl super::InWorkspace {
    /// [`Actions::ask_model`] for this workspace and caller's certificate.
    pub(super) fn ask_model(
        &self,
        workspaces: Workspaces,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (host, caller, workspace_id, certificate) = (
            self.server.clone(),
            caller.clone(),
            self.workspace_id,
            self.certificate.clone(),
        );
        self.server.spawn(async move {
            // Who may ask is decided before anything is admitted.
            workspaces
                .reaching_allowed(
                    (workspace_id, &caller, certificate.as_ref()),
                    Store::MODELS,
                    person_models::ASK,
                    tilo_action_policy::ActionKind::Read,
                )
                .await?;
            host.ask_model(
                &workspaces,
                &caller,
                certificate.as_ref(),
                workspace_id,
                &request,
            )
        })
    }
}

impl Actions {
    /// Admit `request`, an ask of `caller`'s own model, and answer it: the
    /// receipt says it is accepted, its items carry the answer so far, and its output
    /// is the turn ([`person_models::ASK`]).
    pub(super) fn ask_model(
        &self,
        workspaces: &Workspaces,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        workspace_id: Option<WorkspaceId>,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionCallError> {
        let invocation = request.invocation;
        let mut log = self.log();
        if log.reserve(caller, request)? == tilo_actions::remote::Reservation::Admitted {
            return log
                .get(caller, &invocation)
                .map(InvocationState::receipt)
                .ok_or(ActionCallError::UnknownInvocation);
        }
        let accepted = InvocationState::new(invocation, InvocationUpdate::Accepted { invocation });
        let receipt = accepted.receipt();
        log.complete(&invocation, accepted);
        drop(log);
        let moved = tokio::sync::watch::Sender::new(0_u64);
        let follows = moved.subscribe();
        self.shared
            .model_asks
            .lock_recover()
            .insert(invocation, follows);
        let (actions, workspaces, caller, certificate, request, writer) = (
            self.clone(),
            workspaces.clone(),
            caller.clone(),
            certificate.cloned(),
            request.clone(),
            caller.clone(),
        );
        self.shared.runtime.spawn(async move {
            let mut written = Written::default();
            let mut events = |event: StreamEvent| {
                match event {
                    StreamEvent::TextDelta { text } => written.text.push_str(&text),
                    StreamEvent::ReasoningDelta { text } => written.reasoning.push_str(&text),
                    _ => return,
                }
                actions.write_item(&writer, &invocation, &written);
                moved.send_modify(|count| *count = count.wrapping_add(1));
            };
            let answered = workspaces
                .ask_streamed(
                    workspace_id,
                    &caller,
                    certificate.as_ref(),
                    &request,
                    &mut events,
                )
                .await;
            let state = match answered {
                Ok(value) => succeeded(invocation, value),
                Err(error) => failed(invocation, &error.to_string()),
            };
            actions.log().complete(&invocation, state);
            actions.shared.model_asks.lock_recover().remove(&invocation);
            moved.send_modify(|count| *count = count.wrapping_add(1));
        });
        Ok(receipt)
    }

    /// Keep the answer so far as `invocation`'s newest item.
    fn write_item(&self, caller: &Caller, invocation: &InvocationId, written: &Written) {
        let mut log = self.log();
        let Some(entry) = log.get_mut(caller, invocation) else {
            return;
        };
        let sequence = match &entry.update {
            InvocationUpdate::Item { sequence, .. } => sequence.saturating_add(1),
            _ => 1,
        };
        *entry = InvocationState::new(
            *invocation,
            InvocationUpdate::Item {
                invocation: *invocation,
                sequence,
                value: json!({"text": written.text, "reasoning": written.reasoning}),
            },
        );
        drop(log);
    }

    /// Resolves when the ask `id` of `caller` wrote more or ended; `None` for
    /// an invocation that is no running ask.
    pub(super) fn next_ask_change(
        &self,
        caller: &Caller,
        id: &InvocationId,
    ) -> Option<Pending<()>> {
        let mut follows = self.shared.model_asks.lock_recover().get(id).cloned()?;
        if !self.admitted(caller, id) {
            return None;
        }
        // What it wrote until now is the caller's to observe already.
        follows.borrow_and_update();
        Some(self.spawn(async move {
            // Bounded, as a waiter's deadline is only checked when woken.
            let _ = tokio::time::timeout(FOLLOW_WAKE, follows.changed()).await;
        }))
    }
}
