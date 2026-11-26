//! The shells an app's own connection carries ([`tilo_action_line::shells`]), where
//! the shell runtime runs them: the daemon opens each on its one connection to the runtime
//! ([`crate::control_line`]) in the workspace the app was started in, and keeps the terminal and
//! the session lease the runtime hands back.

use std::io::Read;
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use runtime_portal::pending_connection::{LeaseState, ReaderLifetime, SessionLifetime};
use serde_json::{Value, json};
use tilo_action_line::shells::{OpenedShell, ShellSource};
use tilo_actions::remote::ActionCallError;
use tilo_actions::remote::shell::ShellOpen;

use crate::control_line::ControlLine;

/// How long a terminal whose shell has not started yet is waited on before it is read again: a
/// workspace a server hosts hands its shell over after the terminal opened.
const HANDED_OVER_WITHIN: Duration = Duration::from_millis(20);

/// The shell runtime, as the daemon reaches it on `0`.
pub struct Runtime(pub &'static ControlLine);

impl ShellSource for Runtime {
    fn open(&self, workspace: &str, open: &ShellOpen) -> Result<OpenedShell, ActionCallError> {
        let mut request = json!({
            "op": "open-session",
            "cols": open.cols.max(1),
            "rows": open.rows.max(1),
            "term": "xterm-256color",
            "target": open.target,
            "requested_by": if open.agent { "agent" } else { "person" },
            // The lease says how the session ended: a lost connection to a remote shell is not
            // an exit.
            "end_report": true,
        });
        if let Some(fields) = request.as_object_mut() {
            if !open.command.is_empty() {
                fields.insert("command".into(), json!(open.command));
            }
            if let Some(session) = &open.relay_session {
                fields.insert("relay_session".into(), json!(session));
            }
            if let Some(record) = &open.record {
                fields.insert("record".into(), json!(record));
            }
            if let Some(key) = &open.commands {
                fields.insert("commands".into(), json!(key));
            }
        }
        let answer =
            self.0
                .ask(workspace, &request)
                .map_err(|error| ActionCallError::Unavailable {
                    reason: error.to_string(),
                })?;
        let said: Value = serde_json::from_slice(&answer.line).unwrap_or_default();
        if answer.descriptors.is_empty() {
            return Err(ActionCallError::Refused {
                reason: refusal(&said, &answer.line),
            });
        }
        let [terminal, lease]: [OwnedFd; 2] =
            answer
                .descriptors
                .try_into()
                .map_err(|_| ActionCallError::Unavailable {
                    reason: "the shell runtime handed back another session than was asked".into(),
                })?;
        if said.get("result").and_then(Value::as_str) != Some("session") {
            return Err(ActionCallError::Unavailable {
                reason: "the shell runtime handed back another session than was asked".into(),
            });
        }
        let lease = SessionLifetime::adopt(lease).map_err(|error| unavailable(&error))?;
        let reading = lease
            .clone_for_reader()
            .map_err(|error| unavailable(&error))?;
        let terminal = std::fs::File::from(terminal);
        let ended = Arc::new(Mutex::new(String::new()));
        let output = Terminal {
            pty: terminal.try_clone().map_err(|error| unavailable(&error))?,
            lease: reading,
            ended: Arc::clone(&ended),
        };
        let window = terminal.try_clone().map_err(|error| unavailable(&error))?;
        Ok(OpenedShell {
            input: Box::new(terminal),
            output: Box::new(output),
            resize: Box::new(move |cols, rows| resize(&window, cols, rows)),
            report: Box::new(move || {
                std::mem::take(&mut *ended.lock().unwrap_or_else(PoisonError::into_inner))
            }),
            keep: Box::new(lease),
            name: if open.target.get("kind").and_then(Value::as_str) == Some("host") {
                "host".into()
            } else {
                "ush".into()
            },
            relay_session: said
                .get("host_session")
                .and_then(Value::as_str)
                .map(str::to_owned),
            commands: said.get("commands").and_then(Value::as_bool) == Some(true),
        })
    }
}

/// A session's terminal as its output is read: it ends when the session's lease says the
/// session ended, and keeps how it did.
struct Terminal {
    pty: std::fs::File,
    lease: ReaderLifetime,
    ended: Arc<Mutex<String>>,
}

impl Read for Terminal {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match self.pty.read(buffer) {
                Ok(read) if read > 0 => return Ok(read),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                _ => match self.lease.state() {
                    // A terminal whose shell is not there yet: a hosted workspace hands it over.
                    LeaseState::Open => std::thread::sleep(HANDED_OVER_WITHIN),
                    LeaseState::Closed => return Ok(0),
                    LeaseState::Reported(line) => {
                        *self.ended.lock().unwrap_or_else(PoisonError::into_inner) =
                            String::from_utf8_lossy(&line).into_owned();
                        return Ok(0);
                    }
                },
            }
        }
    }
}

/// Why the runtime opened no session, in its own words where it gave them.
fn refusal(said: &Value, line: &[u8]) -> String {
    match said.get("reason").and_then(Value::as_str) {
        Some(reason) if !reason.is_empty() => reason.to_owned(),
        _ => format!(
            "Tilo's shell runtime refused this shell: {}",
            String::from_utf8_lossy(line).trim()
        ),
    }
}

fn unavailable(error: &std::io::Error) -> ActionCallError {
    ActionCallError::Unavailable {
        reason: error.to_string(),
    }
}

/// Tell the terminal its window is `cols` by `rows` cells: the shell's programs reflow.
fn resize(terminal: &std::fs::File, cols: u16, rows: u16) {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `terminal` is an open pty master for the call's duration, and `size` is a valid
    // `winsize` that `TIOCSWINSZ` only reads.
    let _ = unsafe { libc::ioctl(terminal.as_raw_fd(), libc::TIOCSWINSZ, &raw const size) };
}

#[cfg(test)]
#[path = "app_shells_tests.rs"]
mod tests;
