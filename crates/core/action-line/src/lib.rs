//! One frame per line: how a local socket carries the action wire. A
//! session's control socket, a console app's socket and its virtio-serial
//! channel each read a line and write the line that answers it.
//!
//! This is the caller's side: [`LineClient`], the one client of the line wire, the connection a
//! launcher hands the programs it starts, and how a frame is written as a line. Answering a line
//! as a provider is `tilo-action-line-serve`'s, which only the hosts that answer depend on.

use serde_json::json;

use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};

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
pub mod handed;

#[cfg(not(target_arch = "wasm32"))]
mod carry;
#[cfg(not(target_arch = "wasm32"))]
mod client;
#[cfg(not(target_arch = "wasm32"))]
pub use carry::{Carry, CarryAnswer, answer_line as carry_answer_line, carry_line, carry_request};
#[cfg(not(target_arch = "wasm32"))]
pub use client::{ApiHeard, LineClient, LinePut, ShellHeard};
