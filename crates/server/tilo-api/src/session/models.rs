//! The person's own model, asked of the server that keeps their workspace
//! ([`person_models::ASK`]) over the session's own connection, and followed
//! as the model writes: the one way a program of this computer asks a
//! model, which the computer's model gateway takes too.
//!
//! The server admits the ask at once. While the model writes, the
//! invocation's newest item holds the answer so far (`{"text", "reasoning"}`,
//! never a delta), so a follower that misses an item loses nothing: what it
//! has not told yet of the newest one is its next delta. The output is the
//! whole turn, `{"Ok": …}` or `{"Err": …}` as serde writes a `Result`.

use super::{Reach, Session};
use crate::server::ServerError;
use serde_json::{Map, Value};
use tilo_actions::remote::{
    ActionProvider as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus, UpdateMark,
    person_models, settle_with,
};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_clock::time::Duration;

/// The longest one turn is followed.
const TURN: Duration = Duration::from_secs(150);

/// What a turn wrote since it was last told, or how it ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Written {
    /// More of the answer's words.
    Text(String),
    /// More of the reasoning the model shows.
    Reasoning(String),
    /// The whole turn, as the server answered it: `{"Ok": response}` or
    /// `{"Err": {"kind", "message"}}`.
    Answer(Value),
}

/// An ask the server admitted, to follow ([`Self::follow`]).
pub struct ModelAsk {
    actions: tilo_session_actions::SessionActions,
    caller: Caller,
    invocation: InvocationId,
}

impl Session {
    /// Ask the person's model in `workspace` with `argument` (a model
    /// request, as the server's model service takes one): the ask, once the
    /// server admitted it.
    ///
    /// # Errors
    /// Not a workspace of the person's, the server was not reached, or it
    /// refused the ask; a desktop serving its own workspaces asks no model
    /// itself.
    pub async fn ask_model(
        &self,
        workspace: &str,
        argument: Map<String, Value>,
    ) -> Result<ModelAsk, ServerError> {
        let connection = match &self.reach {
            Reach::Forwarded(connection) => connection,
            Reach::Here(_) => return Err(ServerError::NoAccountHere),
        };
        let id = connection.server().enter(connection, workspace).await?;
        let actions = connection.device.actions(id);
        let caller = self.caller();
        let invocation = InvocationId::new();
        let asked = InvokeRequest {
            invocation,
            target: person_models::target(person_models::ASK),
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        let receipt = actions
            .invoke(&caller, asked)
            .await
            .ok_or_else(|| ServerError::Unavailable("the session with the server closed".into()))?
            .map_err(|refused| ServerError::Refused(refused.to_string()))?;
        if let ReceiptStatus::Rejected { error } = receipt.status {
            return Err(ServerError::Refused(error.to_string()));
        }
        Ok(ModelAsk {
            actions,
            caller,
            invocation,
        })
    }
}

impl ModelAsk {
    /// Follow the ask to its end, telling `told` each delta as the model
    /// writes it and then the whole turn. Stops early once `told` answers
    /// `false` (nobody reads any more).
    ///
    /// # Errors
    /// The server stopped answering, the turn took longer than it may, or it
    /// failed or was stopped there.
    pub async fn follow(
        self,
        told: &mut (dyn FnMut(Written) -> bool + Send),
    ) -> Result<(), ServerError> {
        let now = tilo_clock::now();
        let until = now.checked_add(TURN).unwrap_or(now);
        let mut seen = UpdateMark::Accepted;
        let mut shown = Shown::default();
        loop {
            let state = settle_with(
                &self.actions,
                &self.caller,
                self.invocation,
                Some(seen),
                until,
            )
            .await
            .flatten()
            .ok_or_else(|| ServerError::Unavailable("the server stopped answering".into()))?;
            seen = UpdateMark::of(&state.update);
            if let InvocationUpdate::Item { value, .. } = &state.update
                && !shown.tell(value, told)
            {
                return Ok(());
            }
            match state.receipt().status {
                ReceiptStatus::Accepted if tilo_clock::now() >= until => {
                    return Err(ServerError::Unavailable("the model took too long".into()));
                }
                ReceiptStatus::Accepted => {}
                ReceiptStatus::Completed { output } => {
                    let answer = output.value.unwrap_or_default();
                    // The whole turn replaces the last item: what it wrote
                    // after the newest item read is told before it.
                    if shown.tell(&written_in(&answer), told) {
                        told(Written::Answer(answer));
                    }
                    return Ok(());
                }
                ReceiptStatus::Failed { error } => return Err(ServerError::Refused(error.message)),
                ReceiptStatus::Rejected { error } => {
                    return Err(ServerError::Refused(error.to_string()));
                }
                ReceiptStatus::Cancelled { .. } => {
                    return Err(ServerError::Refused("the ask was stopped".into()));
                }
            }
        }
    }
}

/// The words and reasoning of a whole turn `answer`, `{"Ok": {"message":
/// {"content": [...]}}}`, as an item holds an answer so far.
fn written_in(answer: &Value) -> Value {
    let (mut text, mut reasoning) = (String::new(), String::new());
    let parts = answer
        .pointer("/Ok/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for part in parts {
        let said = part.get("text").and_then(Value::as_str).unwrap_or_default();
        match part.get("type").and_then(Value::as_str) {
            Some("text") => text.push_str(said),
            Some("reasoning") => reasoning.push_str(said),
            _ => {}
        }
    }
    serde_json::json!({"text": text, "reasoning": reasoning})
}

/// How much of the answer so far was told.
#[derive(Default)]
struct Shown {
    text: usize,
    reasoning: usize,
}

impl Shown {
    /// Tell `told` what the answer so far `value` adds to what was told:
    /// whether it still listens.
    fn tell(&mut self, value: &Value, told: &mut (dyn FnMut(Written) -> bool + Send)) -> bool {
        let newer = |key: &str, shown: &mut usize| {
            let text = value.get(key).and_then(Value::as_str)?;
            let more = text.get(*shown..).filter(|more| !more.is_empty())?;
            *shown = text.len();
            Some(more.to_owned())
        };
        if let Some(more) = newer("reasoning", &mut self.reasoning)
            && !told(Written::Reasoning(more))
        {
            return false;
        }
        if let Some(more) = newer("text", &mut self.text)
            && !told(Written::Text(more))
        {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The whole turn tells what its last item did not.
    #[test]
    fn the_whole_turn_tells_the_rest() {
        let mut shown = Shown::default();
        let mut heard = Vec::new();
        let mut told = |written: Written| {
            heard.push(written);
            true
        };
        shown.tell(&json!({"text": "on ", "reasoning": ""}), &mut told);
        let whole = json!({"Ok": {"message": {"role": "assistant", "content": [
            {"type": "text", "text": "on Tuesday"},
            {"type": "tool_call", "id": "1", "name": "x", "input": {}},
        ]}}});
        shown.tell(&written_in(&whole), &mut told);
        assert_eq!(
            heard,
            [Written::Text("on ".into()), Written::Text("Tuesday".into())]
        );
    }

    /// Each snapshot tells only what it adds, so a missed item loses nothing.
    #[test]
    fn a_snapshot_tells_only_what_it_adds() {
        let mut shown = Shown::default();
        let mut heard = Vec::new();
        let mut told = |written: Written| {
            heard.push(written);
            true
        };
        assert!(shown.tell(&json!({"text": "Hel", "reasoning": ""}), &mut told));
        assert!(shown.tell(&json!({"text": "Hello wor", "reasoning": "hm"}), &mut told));
        assert!(shown.tell(&json!({"text": "Hello wor", "reasoning": "hm"}), &mut told));
        assert_eq!(
            heard,
            [
                Written::Text("Hel".into()),
                Written::Reasoning("hm".into()),
                Written::Text("lo wor".into()),
            ]
        );
    }
}
