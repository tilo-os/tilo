//! A workspace's model asked from a device ([`model_providers::ASK`]), as the
//! workspace's policy lets the caller, answered as it is written.
//!
//! The ask is a result that moves ([`Answered::Live`]): while the model writes, its newest
//! item holds the answer so far (its words and the reasoning it shows), each newer than the
//! last; a device that follows the invocation reads the newest one and shows what it has not
//! shown yet, so an item it never read loses it nothing. The turn's whole answer is the
//! invocation's output once it ends, as for any other action.

use agent_provider_config::protocol::StreamEvent;
use serde_json::json;
use tilo_actions::remote::{ActionCallError, InvokeRequest};
use tilo_workspace_records::models as model_providers;
use tilo_workspace_service::workspaces::providers::{Answered, Live, WorkspaceCall};
use tilo_workspace_service::workspaces::workspace_stores::Store;

use crate::PersonServices;

/// Whether `request` is an ask of the workspace's model, which
/// [`PersonServices::ask_model`] answers.
pub(crate) fn is_model_ask(request: &InvokeRequest) -> bool {
    request.target == model_providers::target(model_providers::ASK)
}

/// What a running ask has written so far, as its item.
fn written(text: &str, reasoning: &str) -> Live {
    Live::Item(json!({"text": text, "reasoning": reasoning}))
}

impl PersonServices {
    /// Answer `request`, an ask of `call`'s caller's own model, as it is written: who may ask
    /// is decided first, and the answer so far is its newest item until the turn is its
    /// output ([`model_providers::ASK`]).
    pub(crate) async fn ask_model(
        &self,
        call: WorkspaceCall<'_>,
        request: &InvokeRequest,
    ) -> Result<Answered, ActionCallError> {
        self.reaching_allowed(
            (call.workspace, call.caller, call.certificate),
            Store::MODELS,
            model_providers::ASK,
            tilo_action_policy::ActionKind::Read,
            request,
        )
        .await?;
        let (live, follows) = tokio::sync::watch::channel(written("", ""));
        let (services, caller, certificate, workspace, request) = (
            self.clone(),
            call.caller.clone(),
            call.certificate.cloned(),
            call.workspace,
            request.clone(),
        );
        tokio::spawn(async move {
            let (mut text, mut reasoning) = (String::new(), String::new());
            let mut events = |event: StreamEvent| {
                match event {
                    StreamEvent::TextDelta { text: more } => text.push_str(&more),
                    StreamEvent::ReasoningDelta { text: more } => reasoning.push_str(&more),
                    _ => return,
                }
                live.send_replace(written(&text, &reasoning));
            };
            let answered = services
                .ask_streamed(
                    workspace,
                    &caller,
                    certificate.as_ref(),
                    &request,
                    &mut events,
                )
                .await;
            live.send_replace(Live::Ended(answered.map_err(|error| error.to_string())));
        });
        Ok(Answered::Live(follows))
    }
}
