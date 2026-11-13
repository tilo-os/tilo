//! What happens in a workspace, as it happens: its changes, the key's runs
//! starting and ending, and who is there. One watch ([`Watch`]) reads them,
//! with the same `tilo_watch` an agent holds, and both the event stream
//! (`…/events`, `…/runs/{run}/output`) and webhooks ([`super::webhooks`])
//! carry what it reads.

use super::answers::Event;
use super::errors::ApiError;
use crate::session::{Call, Session};
use hyper::body::Bytes;
use serde_json::{Map, Value, json};
use std::sync::Arc;

/// How long one look waits for something to happen; an idle stream says it
/// is alive this often.
const LOOK_MS: u64 = 15_000;
/// The phases a run ends in.
const ENDED: [&str; 3] = ["succeeded", "failed", "cancelled"];

/// One workspace's events, read as a session's run.
pub struct Watch {
    session: Arc<Session>,
    workspace: String,
    path: Option<String>,
    cursor: Option<String>,
}

impl Watch {
    /// The events of `workspace` (under `path`) after `cursor`; without one,
    /// from now.
    #[must_use]
    pub const fn new(
        session: Arc<Session>,
        workspace: String,
        path: Option<String>,
        cursor: Option<String>,
    ) -> Self {
        Self {
            session,
            workspace,
            path,
            cursor,
        }
    }

    /// The next events, waiting up to `wait_ms` for one: none when nothing
    /// happened. The cursor moves past them.
    ///
    /// # Errors
    /// The workspace could not be watched.
    pub async fn next(&mut self, wait_ms: u64) -> Result<Vec<Event>, ApiError> {
        let mut arguments = Map::new();
        arguments.insert("wait_ms".into(), json!(wait_ms));
        if let Some(cursor) = &self.cursor {
            arguments.insert("cursor".into(), json!(cursor));
        }
        if let Some(path) = &self.path {
            arguments.insert("path".into(), json!(path));
        }
        let answer = self
            .session
            .call(Call {
                workspace: Some(self.workspace.clone()),
                tool: "tilo_watch",
                arguments: Value::Object(arguments),
                run: None,
            })
            .await?;
        let typed = super::typed_answer(&answer)?;
        if let Some(cursor) = typed.get("cursor").and_then(Value::as_str) {
            self.cursor = Some(cursor.to_owned());
        }
        let cursor = self.cursor.clone().unwrap_or_default();
        Ok(typed
            .get("events")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|event| Event {
                id: cursor.clone(),
                kind: event
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("change")
                    .to_owned(),
                workspace: self.workspace.clone(),
                data: data_of(event),
            })
            .collect())
    }

    /// Where the watch has read up to.
    #[must_use]
    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }
}

/// An event's data as the API names it: a run by its invocation (`run`),
/// with its title (`title`) and the run it is a step of (`group`).
fn data_of(event: &Value) -> Value {
    let mut data = event.as_object().cloned().unwrap_or_default();
    data.remove("kind");
    if let Some(title) = data.remove("run") {
        data.insert("title".into(), title);
    }
    if let Some(run) = data.remove("invocation_id") {
        data.insert("run".into(), run);
    }
    if let Some(group) = data.remove("run_id") {
        data.insert("group".into(), group);
    }
    Value::Object(data)
}

/// The workspace's events as server-sent events, or only `run`'s, ending
/// when it ends. The stream reads on while someone reads it.
#[must_use]
pub fn stream(mut watch: Watch, run: Option<String>) -> crate::Body {
    let (sender, receiver) = tokio::sync::mpsc::channel::<Bytes>(16);
    tokio::spawn(async move {
        loop {
            let events = match watch.next(LOOK_MS).await {
                Ok(events) => events,
                Err(refused) => {
                    let _ = sender.send(frame(None, "error", &refused.json())).await;
                    return;
                }
            };
            if events.is_empty()
                && sender
                    .send(Bytes::from_static(b": keep-alive\n\n"))
                    .await
                    .is_err()
            {
                return;
            }
            let last = events.len().saturating_sub(1);
            for (index, event) in events.into_iter().enumerate() {
                let theirs = run.as_deref().is_none_or(|run| {
                    event.kind == "run"
                        && event.data.get("run").and_then(Value::as_str) == Some(run)
                });
                if !theirs {
                    continue;
                }
                let ended = run.is_some()
                    && event
                        .data
                        .get("phase")
                        .and_then(Value::as_str)
                        .is_some_and(|phase| ENDED.contains(&phase));
                // The cursor resumes after the whole batch: only its last
                // event carries it, so a reader that reconnects reads a
                // batch it did not finish again rather than losing its end.
                let id = (index == last).then_some(event.id.as_str());
                if sender
                    .send(frame(id, &event.kind, &event.data))
                    .await
                    .is_err()
                    || ended
                {
                    return;
                }
            }
        }
    });
    let frames = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver
            .recv()
            .await
            .map(|bytes| (Ok(hyper::body::Frame::data(bytes)), receiver))
    });
    http_body_util::BodyExt::boxed(http_body_util::StreamBody::new(frames))
}

fn frame(id: Option<&str>, kind: &str, data: &Value) -> Bytes {
    let mut text = String::new();
    if let Some(id) = id.filter(|id| !id.is_empty()) {
        text.push_str("id: ");
        text.push_str(id);
        text.push('\n');
    }
    text.push_str("event: ");
    text.push_str(kind);
    text.push_str("\ndata: ");
    text.push_str(&data.to_string());
    text.push_str("\n\n");
    Bytes::from(text)
}
