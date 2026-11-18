//! `POST …/model/ask`: one turn of the person's own model, its answer streamed
//! as server-sent events while the model writes it ([`crate::session::models`]).
//! The events and what each carries are [`super::answers::ModelAsk`]'s.

use super::errors::ApiError;
use super::{Answer, events};
use crate::session::Session;
use crate::session::models::Written;
use hyper::body::{Bytes, Frame};
use serde_json::{Map, Value, json};
use std::sync::Arc;

/// Ask the model in `workspace` with `body`, a [`super::answers::ModelAsk`]:
/// the stream of its answer once the server admitted the ask.
pub(super) async fn ask(
    session: &Arc<Session>,
    workspace: &str,
    body: &[u8],
) -> Result<Answer, ApiError> {
    let argument: Map<String, Value> = serde_json::from_slice(body)
        .map_err(|error| ApiError::new("invalid_arguments", format!("the body: {error}")))?;
    serde_json::from_value::<super::answers::ModelAsk>(Value::Object(argument.clone()))
        .map_err(|error| ApiError::new("invalid_arguments", format!("the body: {error}")))?;
    let asked = session.ask_model(workspace, argument).await?;
    // One turn's words are few: a reader slower than the model is told the
    // rest as it reads, and only a reader gone stops the ask.
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    tokio::spawn(async move {
        let mut told = |written: Written| {
            let (kind, data) = match written {
                Written::Text(text) => ("text", json!({"text": text})),
                Written::Reasoning(text) => ("reasoning", json!({"text": text})),
                Written::Answer(answer) => ("answer", answer),
            };
            sender.send(events::frame(None, kind, &data)).is_ok()
        };
        if let Err(refused) = asked.follow(&mut told).await {
            let refused = ApiError::from(refused);
            let _ = sender.send(events::frame(None, "error", &refused.json()));
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
