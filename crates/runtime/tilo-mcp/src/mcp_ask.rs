//! `tilo_ask`: the agent asks the person it works for something only they
//! can answer, a decision or something only they know, and waits for their
//! answer ([`tilo_actions::remote::ASK`]). The question waits on the person's
//! own devices, beside every other question their agents put to them, and
//! the call answers with what they said. A question still unanswered when
//! the call's wait ends is answered `pending`: `tilo_status` with its
//! invocation id answers it once the person did.

use serde::Deserialize;
use serde_json::{Value, json};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{InvokeArgument, InvokeRequest, PersonAnswer, PersonAsk, ReceiptStatus};

use super::{ActionProvider, Failure, McpServer, Reply};

/// How long a call waits for the person's answer when it names no wait: as
/// long as the question waits for them.
const ASK_PATIENCE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// What `tilo_ask` takes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AskArgs {
    question: String,
    #[serde(default)]
    choices: Vec<String>,
    #[serde(default)]
    typed: bool,
    #[serde(default)]
    wait_ms: Option<u64>,
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// Put [`AskArgs`] to the person and answer what they said.
    pub(super) async fn agent_ask(
        &self,
        args: AskArgs,
        structured: bool,
    ) -> Result<Reply, Failure> {
        let ask = PersonAsk {
            question: args.question,
            choices: args.choices,
            typed: args.typed,
        }
        .checked()?;
        let argument = match serde_json::to_value(&ask) {
            Ok(Value::Object(fields)) => InvokeArgument::Json(fields),
            _ => return Err("the question could not be written".to_owned().into()),
        };
        let invocation = tilo_actions::InvocationId::new();
        let request = InvokeRequest {
            invocation,
            target: tilo_actions::remote::ask_target(),
            argument,
            context: self.context(),
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        let receipt = self
            .host()
            .invoke(&self.caller(), request)
            .await
            .ok_or_else(|| "the workspace did not answer".to_owned())?
            .map_err(Failure::from)?;
        if let ReceiptStatus::Rejected { error } = receipt.status {
            return Err(Failure::from(error));
        }
        let wait = args
            .wait_ms
            .map_or(ASK_PATIENCE, std::time::Duration::from_millis)
            .min(ASK_PATIENCE);
        let value = self.settled(invocation, crate::Hold::For(wait)).await?;
        Ok(asked_reply(&value, structured))
    }
}

/// The call's answer, from how its invocation stands (`value`, as
/// [`McpServer::settled`] answers it).
fn asked_reply(value: &Value, structured: bool) -> Reply {
    let id = value["invocation_id"].as_str().unwrap_or_default();
    let update: Option<InvocationUpdate> = serde_json::from_value(value["update"].clone()).ok();
    let mut reply = match update {
        Some(InvocationUpdate::Succeeded { output, .. }) => {
            match output
                .value
                .and_then(|answer| serde_json::from_value(answer).ok())
            {
                Some(PersonAnswer::Answered { answer }) => {
                    let mut reply = Reply::ok(format!("answered: {answer}"));
                    reply.structured =
                        structured.then(|| json!({"answered": answer, "invocation_id": id}));
                    // What the person typed is theirs, never an instruction from elsewhere;
                    // it is still text to reason about.
                    return reply;
                }
                Some(PersonAnswer::Declined) => Reply::ok(
                    "declined: your person chose not to answer; go on without it, or ask \
                     differently later"
                        .to_owned(),
                ),
                None => Reply::failed("the workspace answered no answer".to_owned()),
            }
        }
        Some(InvocationUpdate::Failed { error, .. }) => Reply::failed(error.message),
        Some(InvocationUpdate::Cancelled { .. }) => {
            Reply::failed("the question was taken back".to_owned())
        }
        _ => Reply::ok(format!(
            "pending {id}: your person has not answered yet; the question waits for them. \
             tilo_status with this invocation_id answers once they did"
        )),
    };
    if structured {
        reply.structured = Some(value.clone());
    }
    reply
}
