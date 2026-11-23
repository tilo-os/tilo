//! The person's model, asked through the server that keeps their workspace.
//!
//! This computer keeps no provider key. Its apps' turns go where a page's
//! do: to the person's own model on their workspace's server
//! (`tilo.models` `ask`), which asks it with the key their vault keeps there.
//! The way there is the account's `account/models#ask` action, called through
//! this computer's daemon over its socket as the person's `tilo` commands are
//! ([`tilo_api::calls::ask_model`]), and the answer streams back word by word
//! as the model writes it. With no daemon answering, or no model serving the
//! workspace, an app is told there is no model while offline.

use std::time::Duration;

use agent_provider_config::protocol::{ErrorKind, ModelError, ModelRequest, ModelResponse};
use serde_json::Value;

use crate::Response;

/// The error an app is told when no server can be asked.
fn offline() -> ModelError {
    ModelError::new(ErrorKind::Offline)
}

/// One turn of the person's model, its words told to `emit` as frames of
/// the gateway's own stream while the model writes them: the whole turn.
pub(super) async fn ask(
    request: &ModelRequest,
    timeout: Duration,
    emit: super::Emit<'_>,
) -> Result<ModelResponse, ModelError> {
    let asking = async {
        let argument = serde_json::to_value(request)
            .map_err(|_| ModelError::new(ErrorKind::InvalidRequest))?;
        let mut told = |kind: &str, data: &Value| {
            let event = match kind {
                "text" => "text_delta",
                "reasoning" => "reasoning_delta",
                _ => return,
            };
            let Some(text) = data.get("text").and_then(Value::as_str) else {
                return;
            };
            emit(&Response {
                ok: true,
                output: Some(Value::String(text.to_owned()).to_string()),
                configured: None,
                error: None,
                model_error: None,
                event: Some(event.into()),
            });
        };
        let answer = tilo_api::calls::ask_model(&argument, &mut told)
            .await
            .map_err(|why| refusal(&why))?;
        serde_json::from_value::<Result<ModelResponse, ModelError>>(answer)
            .map_err(|_| ModelError::new(ErrorKind::InvalidResponse))?
    };
    tokio::time::timeout(timeout, asking)
        .await
        .map_err(|_| ModelError::new(ErrorKind::TimedOut))?
}

/// What an app is told of `why` the ask did not answer: no model while
/// offline when nothing answered, else the refusal's own words.
fn refusal(why: &anyhow::Error) -> ModelError {
    match tilo_api::calls::Refusal::of(why) {
        Some(refused) if refused.code() != "unavailable" => ModelError {
            kind: ErrorKind::Unavailable,
            message: refused.to_string(),
        },
        _ => offline(),
    }
}
