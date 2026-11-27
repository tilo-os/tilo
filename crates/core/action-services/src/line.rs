//! One frame per line: how a local socket carries the action wire. A
//! session's control socket, a console app's socket and its virtio-serial
//! channel each read a line, answer it here as the provider they serve, and
//! write the line this answers.
//!
//! The daemon's socket answers a whole connection here ([`serve_lines`]): its uploads stay open
//! between lines, and a request that waits is answered beside the ones after it. Its callers ask
//! through [`LineClient`], the one client of the line wire.

use std::time::Duration;

use serde_json::json;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, MAX_REMOTE_WAIT, RemoteActionFrame,
    RemoteActionMessage, refuse, serve,
};

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

/// The line that answers `message` from `caller` once `provider` answers it, however long that
/// takes: a call that still runs is never cut short. A look at how a call stands is the one
/// answered within a bound, as unanswered past it. An answer the provider abandons is refused,
/// as [`serve`] refuses it. A message that asks nothing is refused as `parse_error`.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn answer_whenever<H: ActionProvider + Clone + Send + 'static>(
    provider: &H,
    caller: &Caller,
    message: RemoteActionMessage,
) -> String {
    let Some(unanswered) = refuse(&message, ActionCallError::Unanswered) else {
        return line_refusal("parse_error", "a frame sent here asks something");
    };
    // A look at how a call stands is answered within the longest remote wait: asked again then,
    // it goes on from where it was, while the call it looks at runs on.
    let looks = matches!(message, RemoteActionMessage::Observe { .. });
    let answer = serve(provider, caller, message)
        .and_then(|pending| {
            if looks {
                pending
                    .wait_timeout(MAX_REMOTE_WAIT.saturating_add(LOOK_SLACK))
                    .ok()
                    .flatten()
            } else {
                pending.wait()
            }
        })
        .unwrap_or(unanswered);
    frame_line(answer)
}

/// What a look at a call ([`answer_whenever`]) is given past the longest remote wait before it
/// is answered as unanswered, for its provider to answer it itself.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const LOOK_SLACK: Duration = Duration::from_secs(2);

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

#[cfg(not(target_arch = "wasm32"))]
mod carry;
#[cfg(not(target_arch = "wasm32"))]
mod client;
#[cfg(not(target_arch = "wasm32"))]
mod lines;
#[cfg(not(target_arch = "wasm32"))]
pub mod shells;
#[cfg(not(target_arch = "wasm32"))]
pub use carry::{Carry, CarryAnswer, answer_line as carry_answer_line, carry_line, carry_request};
#[cfg(not(target_arch = "wasm32"))]
pub use client::{LineClient, LinePut, ShellHeard};
#[cfg(not(target_arch = "wasm32"))]
pub use lines::{CallLanes, Ending, Take, serve_lines, serve_lines_with};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, Value};
    use std::sync::Arc;
    use tilo_actions::api::ActionAddress;
    use tilo_actions::remote::{InvokeRequest, RemoteActionTarget};
    use {crate::TestProvider, tilo_actions::remote::ReceiptStatus};

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
