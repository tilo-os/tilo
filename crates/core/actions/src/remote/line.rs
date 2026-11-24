//! One frame per line: how a local socket carries the action wire. A
//! session's control socket, a console app's socket and its virtio-serial
//! channel each read a line, answer it here as the provider they serve, and
//! write the line this answers.
//!
//! The daemon's socket answers a whole connection here ([`serve_lines`]): its uploads stay open
//! between lines, and a request that waits is answered beside the ones after it. Its callers ask
//! through [`LineClient`], the one client of the line wire.

use std::time::Duration;

use serde_json::{Map, Value, json};

use super::{
    ActionCallError, ActionProvider, Caller, InvokeArgument, InvokeRequest, MAX_REMOTE_WAIT,
    RemoteActionFrame, RemoteActionMessage, RemoteActionTarget, refuse, serve,
};
use crate::outcome::InvocationId;

/// The line that answers the frame `line` holds, as [`answer_message`] answers it; a line that
/// holds no frame is refused as `parse_error`.
#[must_use]
pub fn answer_line<H: ActionProvider + Clone + Send + 'static>(
    provider: &H,
    caller: &Caller,
    line: &[u8],
    slack: Duration,
) -> String {
    match RemoteActionFrame::decode(line) {
        Ok(frame) => answer_message(provider, caller, frame.message, slack),
        Err(error) => line_refusal("parse_error", &error.to_string()),
    }
}

/// The line that answers `message` from `caller`: what `provider` answers ([`serve`]), waited for
/// as long as the longest remote wait and `slack` together, else the refusal that says it went
/// unanswered. A message that asks nothing is refused as `parse_error`.
#[must_use]
pub fn answer_message<H: ActionProvider + Clone + Send + 'static>(
    provider: &H,
    caller: &Caller,
    message: RemoteActionMessage,
    slack: Duration,
) -> String {
    let Some(unanswered) = refuse(&message, ActionCallError::Unanswered) else {
        return line_refusal("parse_error", "a frame sent here asks something");
    };
    let answer = serve(provider, caller, message)
        .and_then(|pending| {
            pending
                .wait_timeout(MAX_REMOTE_WAIT.saturating_add(slack))
                .ok()
                .flatten()
        })
        .unwrap_or(unanswered);
    frame_line(answer)
}

/// `message` as the line a socket writes.
#[must_use]
pub fn frame_line(message: RemoteActionMessage) -> String {
    frame_line_in(message, None)
}

/// `message`, asked in `workspace` (none: the device's own), as the line a socket writes.
fn frame_line_in(message: RemoteActionMessage, workspace: Option<&str>) -> String {
    let frame = RemoteActionFrame::new(message).in_workspace(workspace.map(str::to_owned));
    match frame.encode() {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => line_refusal("unavailable", &error.to_string()),
    }
}

/// The line that refuses a line which is no request at all: `{"err": {"code", "message"}}`.
#[must_use]
pub fn line_refusal(code: &str, message: &str) -> String {
    json!({ "err": { "code": code, "message": message } }).to_string()
}

impl InvokeRequest {
    /// An invocation of `target` with `arguments` and nothing else: no context, revision or
    /// signature, answered once it is admitted.
    #[must_use]
    pub fn plain(target: RemoteActionTarget, arguments: Map<String, Value>) -> Self {
        Self {
            invocation: InvocationId::new(),
            target,
            argument: if arguments.is_empty() {
                InvokeArgument::None
            } else {
                InvokeArgument::Json(arguments)
            },
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod client;
#[cfg(not(target_arch = "wasm32"))]
mod lines;
#[cfg(not(target_arch = "wasm32"))]
pub use client::{LineClient, LinePut};
#[cfg(not(target_arch = "wasm32"))]
pub use lines::{CallLanes, Take, serve_lines, serve_lines_with};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ActionAddress;
    use crate::remote::{ReceiptStatus, TestProvider};
    use std::sync::Arc;

    #[test]
    fn a_line_is_answered_by_the_provider_or_refused() {
        let provider = Arc::new(TestProvider::new());
        let caller = Caller::local_app("console");
        let request = InvokeRequest::plain(
            RemoteActionTarget {
                provider: "installer".into(),
                address: ActionAddress::new("installer", "next"),
            },
            Map::new(),
        );
        let invocation = request.invocation;
        let line = frame_line(RemoteActionMessage::Invoke(request));
        let answer = answer_line(&provider, &caller, line.as_bytes(), Duration::ZERO);
        let RemoteActionMessage::Receipt(receipt) = RemoteActionFrame::decode(answer.as_bytes())
            .unwrap()
            .message
        else {
            panic!("no receipt: {answer}");
        };
        assert_eq!(receipt.invocation, invocation);
        assert!(matches!(receipt.status, ReceiptStatus::Completed { .. }));
        assert_eq!(provider.asked().len(), 1);

        let refused: Value =
            serde_json::from_str(&answer_line(&provider, &caller, b"{}", Duration::ZERO)).unwrap();
        assert_eq!(refused["err"]["code"], "parse_error");
        let answered = frame_line(RemoteActionMessage::Receipt(receipt));
        let refused: Value = serde_json::from_str(&answer_line(
            &provider,
            &caller,
            answered.as_bytes(),
            Duration::ZERO,
        ))
        .unwrap();
        assert_eq!(refused["err"]["code"], "parse_error");
    }
}
