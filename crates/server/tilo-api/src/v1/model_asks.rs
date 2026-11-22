//! `POST …/model/ask`: one turn of the person's own model (`account/models#ask`), its answer
//! streamed as server-sent events while the model writes it. The events and what each carries
//! are [`super::answers::ModelAsk`]'s.
//!
//! While the model writes, the invocation's newest item holds the answer so far (`{"text",
//! "reasoning"}`, never a delta), so a follower that misses an item loses nothing: what it has
//! not told yet of the newest one is its next delta. The output is the whole turn, `{"Ok": …}`
//! or `{"Err": …}` as serde writes a `Result`.

use super::errors::ApiError;
use super::{Answer, events};
use crate::account::{Account, MODELS, PROVIDER};
use crate::session::Session;
use hyper::body::{Bytes, Frame};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use tilo_actions::remote::{
    ActionProvider, Caller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
    UpdateMark, settle_with,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use tilo_clock::time::Duration;

/// The longest one turn is followed.
const TURN: Duration = Duration::from_secs(150);

/// Ask the model in `workspace` with `body`, a [`super::answers::ModelAsk`]:
/// the stream of its answer once the server admitted the ask.
pub(super) async fn ask(
    session: &Arc<Session>,
    workspace: &str,
    body: &[u8],
) -> Result<Answer, ApiError> {
    let request: Map<String, Value> = serde_json::from_slice(body)
        .map_err(|error| ApiError::new("invalid_arguments", format!("the body: {error}")))?;
    serde_json::from_value::<super::answers::ModelAsk>(Value::Object(request.clone()))
        .map_err(|error| ApiError::new("invalid_arguments", format!("the body: {error}")))?;
    let account = session.account().clone();
    let caller = session.caller();
    let invocation = InvocationId::new();
    let mut arguments = Map::new();
    arguments.insert("workspace".into(), json!(workspace));
    arguments.insert("request".into(), Value::Object(request));
    let asked = InvokeRequest {
        invocation,
        target: RemoteActionTarget {
            provider: PROVIDER.into(),
            address: ActionAddress::new(MODELS, "ask"),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let receipt = account
        .invoke(&caller, asked)
        .await
        .ok_or_else(|| ApiError::new("unavailable", "the account did not answer"))?
        .map_err(|refused| ApiError::new("refused", refused.to_string()))?;
    match receipt.status {
        ReceiptStatus::Rejected { error } => {
            return Err(ApiError::new("refused", error.to_string()));
        }
        ReceiptStatus::Failed { error } => return Err(ApiError::new(&error.code, error.message)),
        _ => {}
    }
    // One turn's words are few: a reader slower than the model is told the
    // rest as it reads, and only a reader gone stops the ask.
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    tokio::spawn(async move {
        let mut told =
            |kind: &str, data: Value| sender.send(events::frame(None, kind, &data)).is_ok();
        if let Err(refused) = follow(&account, &caller, invocation, &mut told).await {
            told("error", refused.json());
        }
    });
    let frames = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|bytes| {
            (
                Ok::<_, std::convert::Infallible>(Frame::data(bytes)),
                receiver,
            )
        })
    });
    Ok(Answer::Stream {
        content_type: "text/event-stream",
        body: http_body_util::BodyExt::boxed(http_body_util::StreamBody::new(frames)),
    })
}

/// Follow the ask `invocation` to its end, telling `told` each `text` and `reasoning` delta as
/// the model writes it and then the whole turn (`answer`). Stops early once `told` answers
/// `false` (nobody reads any more).
async fn follow(
    account: &Account,
    caller: &Caller,
    invocation: InvocationId,
    told: &mut (dyn FnMut(&str, Value) -> bool + Send),
) -> Result<(), ApiError> {
    let now = tilo_clock::now();
    let until = now.checked_add(TURN).unwrap_or(now);
    let mut seen = UpdateMark::Accepted;
    let mut shown = Shown::default();
    let stopped = |why: &str| ApiError::new("unavailable", why);
    loop {
        let state = settle_with(account, caller, invocation, Some(seen), until)
            .await
            .flatten()
            .ok_or_else(|| stopped("the server stopped answering"))?;
        seen = UpdateMark::of(&state.update);
        if let InvocationUpdate::Item { value, .. } = &state.update
            && !shown.tell(value, told)
        {
            return Ok(());
        }
        match state.receipt().status {
            ReceiptStatus::Accepted if tilo_clock::now() >= until => {
                return Err(stopped("the model took too long"));
            }
            ReceiptStatus::Accepted => {}
            ReceiptStatus::Completed { output } => {
                let answer = output.value.unwrap_or_default();
                // The whole turn replaces the last item: what it wrote
                // after the newest item read is told before it.
                if shown.tell(&written_in(&answer), told) {
                    told("answer", answer);
                }
                return Ok(());
            }
            ReceiptStatus::Failed { error } => return Err(ApiError::new("refused", error.message)),
            ReceiptStatus::Rejected { error } => {
                return Err(ApiError::new("refused", error.to_string()));
            }
            ReceiptStatus::Cancelled { .. } => {
                return Err(ApiError::new("refused", "the ask was stopped"));
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
    json!({"text": text, "reasoning": reasoning})
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
    fn tell(&mut self, value: &Value, told: &mut (dyn FnMut(&str, Value) -> bool + Send)) -> bool {
        for kind in ["reasoning", "text"] {
            let shown = if kind == "text" {
                &mut self.text
            } else {
                &mut self.reasoning
            };
            let Some(said) = value.get(kind).and_then(Value::as_str) else {
                continue;
            };
            let Some(more) = said.get(*shown..).filter(|more| !more.is_empty()) else {
                continue;
            };
            *shown = said.len();
            if !told(kind, json!({"text": more})) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heard() -> (Vec<(String, Value)>, Shown) {
        (Vec::new(), Shown::default())
    }

    /// The whole turn tells what its last item did not.
    #[test]
    fn the_whole_turn_tells_the_rest() {
        let (mut heard, mut shown) = heard();
        let mut told = |kind: &str, data: Value| {
            heard.push((kind.to_owned(), data));
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
            [
                ("text".to_owned(), json!({"text": "on "})),
                ("text".to_owned(), json!({"text": "Tuesday"})),
            ]
        );
    }

    /// Each snapshot tells only what it adds, so a missed item loses nothing.
    #[test]
    fn a_snapshot_tells_only_what_it_adds() {
        let (mut heard, mut shown) = heard();
        let mut told = |kind: &str, data: Value| {
            heard.push((kind.to_owned(), data));
            true
        };
        assert!(shown.tell(&json!({"text": "Hel", "reasoning": ""}), &mut told));
        assert!(shown.tell(&json!({"text": "Hello wor", "reasoning": "hm"}), &mut told));
        assert!(shown.tell(&json!({"text": "Hello wor", "reasoning": "hm"}), &mut told));
        assert_eq!(
            heard,
            [
                ("text".to_owned(), json!({"text": "Hel"})),
                ("reasoning".to_owned(), json!({"text": "hm"})),
                ("text".to_owned(), json!({"text": "lo wor"})),
            ]
        );
    }
}
