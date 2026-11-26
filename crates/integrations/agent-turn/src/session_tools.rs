//! The tools of a turn whose process holds no tool server: asked of the device that serves its
//! session, as two steps of its agent session ([`tilo_action_services::agent_tools`]).
//!
//! That device's session takes each call as the agent, in its agent session, through the same MCP
//! tools every other agent works through: what it may do, what it asks its person first and how
//! History names it are decided there. A call is an invocation the turn follows until it ends; one
//! that takes long holds nothing else up, since every request travels on its own.

use agent_provider_config::protocol::ToolDefinition;
use serde_json::Value;
use tilo_action_services::agent_tools as wire;
use tilo_actions::remote::{
    ActionProvider, Caller, InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};

use crate::{Answer, TOOL_TIME, Tools};

/// How long one request waits at the session for the step to end before it is asked again.
const WAIT_MS: u32 = 5_000;
/// The longest a follow waits for the provider to say the step moved before it looks again.
const FOLLOW_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
/// How long a follow sleeps when the provider cannot say when the step moves.
const POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// The tools the session behind `provider` answers, asked as `caller`.
pub struct SessionTools<H> {
    provider: H,
    caller: Caller,
}

impl<H: ActionProvider + Send + Sync> SessionTools<H> {
    /// The tools the session behind `provider` answers. The session decides every step as the
    /// turn the agent answers, whatever `caller` says.
    #[must_use]
    pub const fn new(provider: H, caller: Caller) -> Self {
        Self { provider, caller }
    }

    /// Take step `action` with `argument` and follow it until it ends: what it made, or why not,
    /// in words its person reads.
    async fn step(&self, action: &str, argument: InvokeArgument) -> Result<Value, String> {
        let invocation = InvocationId::new();
        let request = InvokeRequest {
            invocation,
            target: wire::target(action),
            argument,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            appearance: None,
            wait_ms: Some(WAIT_MS),
            signed: None,
        };
        let receipt: Option<Result<InvocationReceipt, _>> =
            self.provider.invoke(&self.caller, request).await;
        let update = match receipt {
            None => return Err("the session did not answer".to_owned()),
            Some(Err(error)) => return Err(error.to_string()),
            Some(Ok(receipt)) => match receipt.status {
                ReceiptStatus::Completed { output } => return ended(output),
                ReceiptStatus::Failed { error } => return Err(error.message),
                ReceiptStatus::Rejected { error } => return Err(error.to_string()),
                ReceiptStatus::Cancelled { message } => {
                    return Err(message.unwrap_or_else(|| "it was stopped".to_owned()));
                }
                ReceiptStatus::Accepted => self.follow(invocation).await,
            },
        };
        match update {
            Some(InvocationUpdate::Succeeded { output, .. }) => ended(output),
            Some(InvocationUpdate::Failed { error, .. }) => Err(error.message),
            Some(InvocationUpdate::Cancelled { message, .. }) => {
                Err(message.unwrap_or_else(|| "it was stopped".to_owned()))
            }
            _ => {
                // Out of time: the session is asked to stop it rather than let it go on unseen.
                let _ = self.provider.cancel(&self.caller, &invocation).await;
                Err("it did not end in time".to_owned())
            }
        }
    }

    /// How `invocation` ended, followed as the provider says it moves; `None` when it did not
    /// within [`TOOL_TIME`].
    async fn follow(&self, invocation: InvocationId) -> Option<InvocationUpdate> {
        let deadline = tokio::time::Instant::now().checked_add(TOOL_TIME)?;
        loop {
            // Asked before looking, so a change in between still wakes it.
            let change = self.provider.next_change(&self.caller, &invocation);
            if let Some(state) = self.provider.observe(&self.caller, &invocation)
                && state.update.is_terminal()
            {
                return Some(state.update);
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            let until = tokio::time::Instant::now()
                .checked_add(FOLLOW_WAIT)
                .map_or(deadline, |until| until.min(deadline));
            match change {
                Some(change) => {
                    let _ = tokio::time::timeout_at(until, change).await;
                }
                None => {
                    tokio::time::sleep_until(
                        tokio::time::Instant::now()
                            .checked_add(POLL)
                            .map_or(until, |next| next.min(until)),
                    )
                    .await;
                }
            }
        }
    }
}

/// What a step that ended made, as its value.
fn ended(output: ActionOutput) -> Result<Value, String> {
    output
        .value
        .ok_or_else(|| "the session answered nothing".to_owned())
}

impl<H: ActionProvider + Send + Sync> Tools for SessionTools<H> {
    async fn list(&self) -> Result<Vec<ToolDefinition>, String> {
        let listed = self
            .step(wire::LIST, InvokeArgument::Json(serde_json::Map::new()))
            .await
            .map_err(|why| format!("the agent's tools did not say what they are: {why}"))?;
        let listed: wire::ToolList = serde_json::from_value(listed)
            .map_err(|error| format!("the agent's tools answered something else: {error}"))?;
        Ok(listed
            .tools
            .into_iter()
            .map(|tool| ToolDefinition {
                name: tool.name,
                description: tool.description,
                input_schema: tool.input_schema,
            })
            .collect())
    }

    async fn call(&self, name: &str, arguments: Value) -> Answer {
        let asked = wire::ToolCall {
            name: name.to_owned(),
            arguments,
        };
        let answered = match self.step(wire::CALL, wire::argument(&asked)).await {
            Ok(answered) => answered,
            Err(why) => return Answer::failed(format!("{name} was not taken: {why}")),
        };
        match serde_json::from_value::<wire::ToolAnswer>(answered) {
            Ok(answer) => Answer {
                text: answer.text,
                pictures: answer
                    .pictures
                    .into_iter()
                    .map(|picture| (picture.media_type, picture.base64))
                    .collect(),
                is_error: answer.failed,
            },
            Err(error) => Answer::failed(format!("{name} answered something else: {error}")),
        }
    }
}
