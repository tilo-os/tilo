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
use crate::session::Session;
use hyper::body::{Bytes, Frame};
use serde_json::{Map, Value};
use std::sync::Arc;

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
    let (invocation, status) = crate::calls::start_ask(
        &account,
        &caller,
        workspace,
        Value::Object(request),
        "the server",
    )
    .await
    .map_err(|(code, why)| ApiError::new(code, why))?;
    // One turn's words are few: a reader slower than the model is told the
    // rest as it reads, and only a reader gone stops the ask.
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    tokio::spawn(async move {
        let mut told =
            |kind: &str, data: Value| sender.send(events::frame(None, kind, &data)).is_ok();
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
