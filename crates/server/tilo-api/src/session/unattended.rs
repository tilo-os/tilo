//! Whether a workspace runs commands unattended, as the path
//! `workspace/unattended` of every workspace a key acts in.
//!
//! - `tilo_read workspace/unattended` answers `{workspace, unattended}`;
//! - `workspace/unattended#set {on}` turns it on or off.
//!
//! On, a key for that workspace alone runs commands in its Linux machine
//! (`terminal#run`) without the person being asked each time; a key reaching
//! all their workspaces is still asked, and so is everything else that asks.
//! A branch made from the workspace starts with it on too. It is the
//! workspace's own governance (`tilo.workspace`'s `commands.unattended`),
//! asked of its host over the key's session, and only the workspace's owner
//! has it. Turning it on asks the person on their approving device, whichever
//! key of theirs asks, and answers `pending` until they decide; denied, it
//! stays off. Turning it off asks nobody and holds for the next command.

use super::Session;
use crate::account::{Outcome, answer, failed, refused};
use serde_json::{Map, Value, json};
use tilo_actions::remote::ActionProvider as _;
use tilo_actions::remote::{InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget};
use tilo_server_session::workspaces::RUN_UNATTENDED;

/// The path of a workspace's setting.
pub const ROOT: &str = "workspace/unattended";
/// Turning it on or off.
const SET: &str = "workspace/unattended#set";
/// How long the call waits for the host before it answers `pending`: off is
/// answered at once, on once the person decided, which takes longer.
const ANSWERING_MS: u32 = 1_000;

/// Whether `tool` with `arguments` is about the workspace's setting.
pub(crate) fn addresses(tool: &str, arguments: &Value) -> bool {
    matches!(tool, "tilo_do" | "tilo_read")
        && arguments
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.split('#').next() == Some(ROOT))
}

/// The answer of a call that waits for the person: `tilo_status` follows
/// `invocation`.
fn pending(structured: bool, path: &str, invocation: &str) -> Value {
    let typed = json!({"status": "pending", "path": path, "invocation_id": invocation,
                       "ask": {"invocation_id": invocation}});
    if structured {
        return json!({
            "content": [{"type": "text", "text": typed.to_string()}],
            "structuredContent": typed,
            "isError": false,
        });
    }
    let text = format!(
        "pending {invocation}: the person is asked on their approving device; \
         tilo_status {{invocation_id}} answers once they decided"
    );
    json!({"content": [{"type": "text", "text": text}], "isError": false})
}

impl Session {
    /// Answer `tool` with `arguments` on `workspace`'s setting, typed when
    /// `structured`.
    pub(super) async fn unattended(
        &self,
        workspace: &str,
        tool: &str,
        arguments: &Value,
        structured: bool,
    ) -> Value {
        let path = arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let args = arguments.get("args").cloned().unwrap_or_else(|| json!({}));
        let invocation = arguments
            .get("invocation_id")
            .and_then(Value::as_str)
            .and_then(|id| id.parse::<uuid::Uuid>().ok());
        let outcome = match (tool, path) {
            ("tilo_do", SET) => match self.set_unattended(workspace, &args, invocation).await {
                Ok(Set::Done(value)) => Outcome::Ok(value),
                Ok(Set::Asking(invocation)) => return pending(structured, path, &invocation),
                Err(outcome) => outcome,
            },
            ("tilo_read", ROOT) => self.read_unattended(workspace).await,
            (_, other) => failed(
                "not_found",
                format!("the workspace has no action {other}"),
                "tilo_read workspace/unattended reads it, and workspace/unattended#set {on} \
                 turns it on or off",
            ),
        };
        answer(structured, path, outcome)
    }

    /// Whether `workspace` runs commands unattended, as its listing says.
    async fn read_unattended(&self, workspace: &str) -> Outcome {
        match self.workspace(workspace).await {
            Ok(Some(listed)) => Outcome::Ok(json!({
                "workspace": workspace,
                "unattended": listed.unattended,
            })),
            Ok(None) => refused(&crate::server::ServerError::NotFound),
            Err(error) => refused(&error),
        }
    }

    /// Ask `workspace`'s host to turn it on or off as `args` says
    /// (`{on}`), under `invocation` when the caller named one: done, or
    /// still waiting for the person.
    async fn set_unattended(
        &self,
        workspace: &str,
        args: &Value,
        invocation: Option<uuid::Uuid>,
    ) -> Result<Set, Outcome> {
        let Some(on) = args.get("on").and_then(Value::as_bool) else {
            return Err(failed(
                "invalid_arguments",
                "`on` is true or false",
                "pass args {on: true} to ask the person, {on: false} to turn it off",
            ));
        };
        let connection = self.connection().map_err(|error| refused(&error))?;
        let id = connection
            .server()
            .enter(connection, workspace)
            .await
            .map_err(|error| refused(&error))?;
        let mut arguments = Map::new();
        arguments.insert("on".into(), json!(on));
        let request = InvokeRequest {
            invocation: invocation.map_or_else(tilo_actions::InvocationId::new, |id| {
                tilo_actions::InvocationId(id)
            }),
            target: RemoteActionTarget::workspace_governance(workspace, RUN_UNATTENDED),
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: Some(ANSWERING_MS),
            appearance: None,
        };
        let receipt = connection
            .device
            .actions(id)
            .invoke(&self.caller(), request)
            .await
            .ok_or_else(|| failed("unavailable", "the workspace did not answer", "try again"))?
            .map_err(|error| refusal(&error.to_string()))?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(Set::Done(output.value.unwrap_or_default())),
            ReceiptStatus::Accepted => Ok(Set::Asking(receipt.invocation.0.to_string())),
            ReceiptStatus::Failed { error } => Err(refusal(&error.message)),
            ReceiptStatus::Rejected { error } => Err(refusal(&error.to_string())),
            ReceiptStatus::Cancelled { .. } => Err(failed(
                "unavailable",
                "the workspace stopped it",
                "try again",
            )),
        }
    }
}

/// What a turn of the setting answered.
enum Set {
    /// Its new state, `{workspace, unattended}`.
    Done(Value),
    /// The person is asked; the invocation follows their answer.
    Asking(String),
}

/// The workspace's refusal, `why`, as the caller is told it.
fn refusal(why: &str) -> Outcome {
    failed(
        "not_permitted",
        format!("the workspace refused: {why}"),
        "only the workspace's owner, from a key of theirs reaching it, has it; \
         turning it on waits for them to allow it",
    )
}
