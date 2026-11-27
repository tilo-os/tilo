//! The shells an app's own connection carries ([`tilo_actions::remote::shell`]): an app whose
//! signed manifest grants it shells asks the daemon for one, the daemon opens it on its one
//! connection to the shell runtime ([`crate::control_line`]) in the workspace the app was started
//! in, keeps the terminal the runtime hands it, and carries the session's bytes on the app's
//! connection both ways until it ends.
//!
//! The app's process holds no descriptor of the session: it types, resizes and closes by the
//! shell's id, and hears what the shell printed and, last, how it ended. Its connection closing
//! lets every shell it opened go.

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use runtime_portal::pending_connection::{LeaseState, SessionLifetime};
use serde_json::{Value, json};
use tilo_action_services::{CallLanes, Take, frame_line};
use tilo_actions::remote::shell::{
    MAX_SHELL_BYTES, ShellBytes, ShellEnded, ShellOpen, ShellOpened,
};
use tilo_actions::remote::{ActionCallError, RemoteActionMessage};

use crate::control_line::ControlLine;

/// How many shells one app's connection keeps open at once, as one app's connection to the
/// runtime was held to.
const MAX_SHELLS: usize = 8;

/// How long a terminal whose shell has not started yet is waited on before it is read again: a
/// workspace a server hosts hands its shell over after the terminal opened.
const HANDED_OVER_WITHIN: Duration = Duration::from_millis(20);

/// Where a line written for the app goes: its connection.
pub type Out = Arc<dyn Fn(String) + Send + Sync>;

/// Whether the app may open a shell now, or why not: asked on each open, since a workspace's
/// people change while the app runs.
pub type Allowed = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// The shells one app's connection carries.
pub struct AppShells(Arc<Lane>);

struct Lane {
    /// The workspace the app was started in: every shell it opens runs there.
    workspace: String,
    /// Whether the app's signed manifest grants it shells, or why not.
    granted: Result<(), String>,
    /// Whether this computer runs the workspace's programs, or why not.
    allowed: Allowed,
    out: Out,
    ush: &'static ControlLine,
    shells: Arc<Mutex<HashMap<u64, Held>>>,
    next: AtomicU64,
}

/// One open shell: its terminal, written to as the app types, and the lease that keeps its
/// session; letting both go ends it.
struct Held {
    terminal: std::fs::File,
    _lease: SessionLifetime,
}

impl AppShells {
    /// The shells of the app started in `workspace`, which its manifest grants when `granted`
    /// and the computer runs while it is `allowed`, asked of the runtime on `ush` and written to
    /// the app with `out`.
    pub fn new(
        workspace: &str,
        granted: Result<(), String>,
        allowed: Allowed,
        ush: &'static ControlLine,
        out: Out,
    ) -> Self {
        Self(Arc::new(Lane {
            workspace: workspace.to_owned(),
            granted,
            allowed,
            out,
            ush,
            shells: Arc::default(),
            next: AtomicU64::new(1),
        }))
    }
}

impl CallLanes for AppShells {
    fn take(&self, message: &RemoteActionMessage) -> Take {
        let lane = &self.0;
        match message {
            RemoteActionMessage::ShellOpenRequest { correlation, open } => {
                if let Err(reason) = &lane.granted {
                    return Take::Answer(opened(
                        *correlation,
                        Err(ActionCallError::Refused {
                            reason: reason.clone(),
                        }),
                    ));
                }
                if held(&lane.shells).len() >= MAX_SHELLS {
                    return Take::Answer(opened(
                        *correlation,
                        Err(ActionCallError::Refused {
                            reason: "This app has as many shells open as it may: close one first"
                                .into(),
                        }),
                    ));
                }
                // The runtime may wait for a person to allow it: the connection's other frames
                // are answered meanwhile.
                let (lane, correlation, open) = (Arc::clone(lane), *correlation, open.clone());
                let started = std::thread::Builder::new()
                    .name("tilo-shell-open".into())
                    .spawn(move || {
                        let result = lane.open(&open);
                        (lane.out)(opened(correlation, result));
                    });
                if let Err(error) = started {
                    return Take::Answer(opened(
                        correlation,
                        Err(ActionCallError::Unavailable {
                            reason: error.to_string(),
                        }),
                    ));
                }
                Take::Silent
            }
            RemoteActionMessage::ShellInput(ShellBytes { shell, bytes }) => {
                if let Some(held) = held(&lane.shells).get_mut(shell) {
                    let _ = held.terminal.write_all(bytes);
                }
                Take::Silent
            }
            RemoteActionMessage::ShellResize { shell, cols, rows } => {
                if let Some(held) = held(&lane.shells).get(shell) {
                    resize(&held.terminal, *cols, *rows);
                }
                Take::Silent
            }
            RemoteActionMessage::ShellClose { shell } => {
                held(&lane.shells).remove(shell);
                Take::Silent
            }
            // What the device itself says: an app that sends one back is saying nothing.
            RemoteActionMessage::ShellOpenResponse { .. }
            | RemoteActionMessage::ShellOutput(_)
            | RemoteActionMessage::ShellEnded(_) => Take::Silent,
            _ => Take::NotCall,
        }
    }
}

impl Lane {
    /// Open `open` on the runtime, keep its terminal and carry what it prints to the app.
    fn open(&self, open: &ShellOpen) -> Result<ShellOpened, ActionCallError> {
        (self.allowed)().map_err(|reason| ActionCallError::Refused { reason })?;
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
        let answer = self.ush.ask(&self.workspace, &request).map_err(|error| {
            ActionCallError::Unavailable {
                reason: error.to_string(),
            }
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
        let output = terminal.try_clone().map_err(|error| unavailable(&error))?;
        let shell = self.next.fetch_add(1, Ordering::Relaxed);
        held(&self.shells).insert(
            shell,
            Held {
                terminal,
                _lease: lease,
            },
        );
        let (out, shells) = (Arc::clone(&self.out), Arc::downgrade(&self.shells));
        std::thread::Builder::new()
            .name("tilo-shell-output".into())
            .spawn(move || carry_output(shell, output, &reading, &out, &shells))
            .map_err(|error| unavailable(&error))?;
        Ok(ShellOpened {
            shell,
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

/// Carry what the shell `shell` prints to the app until its terminal closes, then how it ended.
fn carry_output(
    shell: u64,
    mut terminal: std::fs::File,
    lease: &runtime_portal::pending_connection::ReaderLifetime,
    out: &Out,
    shells: &Weak<Mutex<HashMap<u64, Held>>>,
) {
    let mut buffer = vec![0_u8; MAX_SHELL_BYTES];
    let report = loop {
        match terminal.read(&mut buffer) {
            Ok(read) if read > 0 => out(frame_line(RemoteActionMessage::ShellOutput(ShellBytes {
                shell,
                bytes: buffer.get(..read).map(<[u8]>::to_vec).unwrap_or_default(),
            }))),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            _ => match lease.state() {
                // A terminal whose shell is not there yet: a hosted workspace hands it over.
                LeaseState::Open => std::thread::sleep(HANDED_OVER_WITHIN),
                LeaseState::Closed => break String::new(),
                LeaseState::Reported(line) => break String::from_utf8_lossy(&line).into_owned(),
            },
        }
    };
    if let Some(shells) = shells.upgrade() {
        held(&shells).remove(&shell);
    }
    out(frame_line(RemoteActionMessage::ShellEnded(ShellEnded {
        shell,
        report,
    })));
}

fn opened(correlation: u64, result: Result<ShellOpened, ActionCallError>) -> String {
    frame_line(RemoteActionMessage::ShellOpenResponse {
        correlation,
        result,
    })
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

fn held(shells: &Mutex<HashMap<u64, Held>>) -> std::sync::MutexGuard<'_, HashMap<u64, Held>> {
    shells.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Tell the terminal its window is `cols` by `rows` cells: the shell's programs reflow.
fn resize(terminal: &std::fs::File, cols: u16, rows: u16) {
    let size = libc::winsize {
        ws_row: rows.max(1),
        ws_col: cols.max(1),
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
