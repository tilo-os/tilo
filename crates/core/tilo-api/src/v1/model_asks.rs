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
use crate::account::{MODELS, PROVIDER};
use crate::session::Session;
use hyper::body::{Bytes, Frame};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use tilo_actions::remote::{
    ActionProvider, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};

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
        asks: false,
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
        let status = ReceiptStatus::Accepted;
        match crate::calls::follow(
            &account,
            &caller,
            invocation,
            status,
            "the server",
            &mut told,
        )
        .await
        {
            Ok(Some(answer)) => {
                told("answer", answer);
            }
            Ok(None) => {}
            Err((code, why)) => {
                told("error", ApiError::new(code, why).json());
            }
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
