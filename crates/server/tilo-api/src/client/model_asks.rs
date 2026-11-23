//! One turn of the person's own model through the API (`POST …/model/ask`),
//! its events read as the server sends them.

use super::{Answer, Client, Method, fresh_key};
use anyhow::{Result, bail};
use serde_json::Value;
use std::ops::ControlFlow;
use wire_frame::sse::{Reader, Utf8};

impl Client {
    /// The client a program of this computer asks the person's model with,
    /// as this computer's own device: the process holding the session `tilo
    /// login` made, where one serves it, else this login's Tilo desktop.
    /// `None` when neither serves here, or this computer's login is
    /// damaged.
    #[must_use]
    pub fn this_computer() -> Option<Self> {
        let held = tilo_device_identity::login::held_api(super::device_keys())
            .ok()
            .flatten()
            .filter(|socket| is_socket(socket));
        held.or_else(super::desktop_socket)
            .map(|socket| Self::session(&socket))
    }

    /// The workspace a program of this computer asks the person's model in:
    /// the one this computer's login entered, else the first of the
    /// person's own, else the first the key reaches.
    ///
    /// # Errors
    /// The workspaces could not be listed.
    pub async fn model_workspace(&self) -> Result<String> {
        if let Some((entered, _)) =
            tilo_device_identity::login::entered_workspace(super::device_keys())
                .ok()
                .flatten()
        {
            return Ok(entered);
        }
        let listed = self.workspaces().await?;
        let workspaces: Vec<&Value> = listed
            .get("workspaces")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .collect();
        let own = workspaces
            .iter()
            .find(|workspace| workspace.get("role").and_then(Value::as_str) == Some("owner"));
        own.or_else(|| workspaces.first())
            .and_then(|workspace| workspace.get("id").and_then(Value::as_str))
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("no workspace of yours to ask your model in"))
    }

    /// One turn of the person's own model in `workspace`: `request` is the
    /// conversation as `ModelAsk` describes it. `told` hears each `text` and
    /// `reasoning` event with its data as it comes; the answer is the whole
    /// turn the `answer` event carries, `{"Ok": …}` or `{"Err": …}`.
    ///
    /// # Errors
    /// The API was not reached, refused the ask, or the stream ended before
    /// the turn did.
    pub async fn ask_model(
        &self,
        workspace: &str,
        request: &Value,
        told: &mut (dyn FnMut(&str, &Value) + Send),
    ) -> Result<Value> {
        let path = format!("/v1/workspaces/{workspace}/model/ask");
        let said = request.to_string();
        let headers = [
            ("accept", "text/event-stream".to_owned()),
            ("content-type", "application/json".to_owned()),
            ("idempotency-key", fresh_key()),
        ];
        let mut opened = self
            .opening(Method::Post, &path, &headers, said.as_bytes())
            .await?;
        let mut events = Events::default();
        let mut whole = Vec::new();
        while let Some(piece) = opened.piece().await? {
            if !(200..300).contains(&opened.status) {
                whole.extend_from_slice(&piece);
                continue;
            }
            if let Some(done) = events.push(&piece, |kind, data| match kind {
                "answer" => Some(Ok(data)),
                "error" => {
                    let body = serde_json::to_vec(&data).unwrap_or_default();
                    Some(Err(Answer { status: 503, body }.refused()))
                }
                _ => {
                    told(kind, &data);
                    None
                }
            }) {
                return done;
            }
        }
        if !(200..300).contains(&opened.status) {
            let (status, body) = (opened.status, whole);
            return Err(Answer { status, body }.refused());
        }
        bail!("the model's answer ended before the turn did")
    }
}

/// Whether `path` is a socket something may listen at.
fn is_socket(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|found| std::os::unix::fs::FileTypeExt::is_socket(&found.file_type()))
}

/// Server-sent events, read from pieces cut anywhere.
pub(crate) struct Events(Reader);

impl Default for Events {
    fn default() -> Self {
        // Lenient UTF-8, unbounded: an `answer` event wrapping a 1 MiB turn exceeds 1 MiB.
        Self(Reader::new(Utf8::Lenient, None))
    }
}

impl Events {
    /// Feed `piece` into the stream, calling `on` for each JSON event it completes.
    pub(crate) fn push<B>(
        &mut self,
        piece: &[u8],
        mut on: impl FnMut(&str, Value) -> Option<B>,
    ) -> Option<B> {
        let flow = self.0.push(piece, |event| {
            serde_json::from_str(event.data)
                .ok()
                .and_then(|data| on(event.kind(), data))
                .map_or(ControlFlow::Continue(()), ControlFlow::Break)
        });
        flow.ok().and_then(ControlFlow::break_value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An event cut across pieces is read once it is whole, and a comment
    /// that keeps the stream alive is none.
    #[test]
    fn events_are_read_whole_from_any_cut() {
        let mut events = Events::default();
        assert!(events.read(b"event: text\ndata: {\"te").is_empty());
        let read = events.read(b"xt\":\"Hi\"}\n\n: keep-alive\n\nevent: answer\ndata: {}\n\n");
        assert_eq!(
            read,
            [
                ("text".to_owned(), json!({"text": "Hi"})),
                ("answer".to_owned(), json!({})),
            ]
        );
    }
}
