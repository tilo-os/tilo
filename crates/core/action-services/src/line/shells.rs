//! The shells an app's own connection carries ([`tilo_actions::remote::shell`]): an app whose
//! signed manifest grants it shells asks the device for one, the device opens it from its
//! [`ShellSource`] in the workspace the app was started in, keeps the terminal, and carries the
//! session's bytes on the app's connection both ways until it ends.
//!
//! The app's process holds no descriptor of the session: it types, resizes and closes by the
//! shell's id, and hears what the shell printed and, last, how it ended. Its connection closing
//! lets every shell it opened go. Where a shell comes from is the device's: Linux's shell
//! runtime, or the computer's own login shell where that runtime does not run.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use tilo_actions::remote::shell::{
    MAX_SHELL_BYTES, ShellBytes, ShellEnded, ShellOpen, ShellOpened,
};
use tilo_actions::remote::{ActionCallError, RemoteActionMessage};

use super::{CallLanes, Take, frame_line};

/// How many shells one app's connection keeps open at once.
const MAX_SHELLS: usize = 8;

/// Where a line written for the app goes: its connection.
pub type Out = Arc<dyn Fn(String) + Send + Sync>;

/// Whether the app may open a shell now, or why not: asked on each open, since a workspace's
/// people change while the app runs.
pub type Allowed = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// A shell a [`ShellSource`] opened, held by the lane until it ends or the app closes it.
pub struct OpenedShell {
    /// Where what the app types goes.
    pub input: Box<dyn Write + Send>,
    /// What the shell prints, until it ends (a read of nothing).
    pub output: Box<dyn Read + Send>,
    /// Tell the terminal its window is this many columns by rows.
    pub resize: Box<dyn Fn(u16, u16) + Send + Sync>,
    /// How the session ended, asked once its output ended: empty when the shell exited and
    /// nothing more was said.
    pub report: Box<dyn FnOnce() -> String + Send>,
    /// What keeps the session open; letting it go ends it.
    pub keep: Box<dyn Send>,
    /// What the shell is, as the app names it.
    pub name: String,
    /// The session on another device this one relays, when it does.
    pub relay_session: Option<String>,
    /// Whether the shell marks its command lines with the key the open named.
    pub commands: bool,
}

/// Where a device's shells come from.
pub trait ShellSource: Send + Sync {
    /// Open `open` in `workspace`.
    ///
    /// # Errors
    /// Why no shell opened: refused, in words a person reads, or nothing here runs it.
    fn open(&self, workspace: &str, open: &ShellOpen) -> Result<OpenedShell, ActionCallError>;
}

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
    source: Arc<dyn ShellSource>,
    shells: Arc<Mutex<HashMap<u64, Held>>>,
    next: AtomicU64,
}

/// One open shell: its input, its window and what keeps its session; letting them go ends it.
struct Held {
    input: Box<dyn Write + Send>,
    resize: Box<dyn Fn(u16, u16) + Send + Sync>,
    _keep: Box<dyn Send>,
}

impl AppShells {
    /// The shells of the app started in `workspace`, which its manifest grants when `granted`
    /// and the computer runs while it is `allowed`, opened from `source` and written to the app
    /// with `out`.
    pub fn new(
        workspace: &str,
        granted: Result<(), String>,
        allowed: Allowed,
        source: Arc<dyn ShellSource>,
        out: Out,
    ) -> Self {
        Self(Arc::new(Lane {
            workspace: workspace.to_owned(),
            granted,
            allowed,
            out,
            source,
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
                // The source may wait for a person to allow it: the connection's other frames
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
                    let _ = held.input.write_all(bytes);
                    let _ = held.input.flush();
                }
                Take::Silent
            }
            RemoteActionMessage::ShellResize { shell, cols, rows } => {
                if let Some(held) = held(&lane.shells).get(shell) {
                    (held.resize)((*cols).max(1), (*rows).max(1));
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
    /// Open `open` from the source, keep it and carry what it prints to the app.
    fn open(&self, open: &ShellOpen) -> Result<ShellOpened, ActionCallError> {
        (self.allowed)().map_err(|reason| ActionCallError::Refused { reason })?;
        let OpenedShell {
            input,
            output,
            resize,
            report,
            keep,
            name,
            relay_session,
            commands,
        } = self.source.open(&self.workspace, open)?;
        let shell = self.next.fetch_add(1, Ordering::Relaxed);
        held(&self.shells).insert(
            shell,
            Held {
                input,
                resize,
                _keep: keep,
            },
        );
        let (out, shells) = (Arc::clone(&self.out), Arc::downgrade(&self.shells));
        std::thread::Builder::new()
            .name("tilo-shell-output".into())
            .spawn(move || carry_output(shell, output, report, &out, &shells))
            .map_err(|error| ActionCallError::Unavailable {
                reason: error.to_string(),
            })?;
        Ok(ShellOpened {
            shell,
            name,
            relay_session,
            commands,
        })
    }
}

/// Carry what the shell `shell` prints to the app until it ends, then how it ended.
fn carry_output(
    shell: u64,
    mut output: Box<dyn Read + Send>,
    report: Box<dyn FnOnce() -> String + Send>,
    out: &Out,
    shells: &Weak<Mutex<HashMap<u64, Held>>>,
) {
    let mut buffer = vec![0_u8; MAX_SHELL_BYTES];
    loop {
        match output.read(&mut buffer) {
            Ok(read) if read > 0 => out(frame_line(RemoteActionMessage::ShellOutput(ShellBytes {
                shell,
                bytes: buffer.get(..read).map(<[u8]>::to_vec).unwrap_or_default(),
            }))),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            _ => break,
        }
    }
    if let Some(shells) = shells.upgrade() {
        held(&shells).remove(&shell);
    }
    out(frame_line(RemoteActionMessage::ShellEnded(ShellEnded {
        shell,
        report: report(),
    })));
}

fn opened(correlation: u64, result: Result<ShellOpened, ActionCallError>) -> String {
    frame_line(RemoteActionMessage::ShellOpenResponse {
        correlation,
        result,
    })
}

fn held(shells: &Mutex<HashMap<u64, Held>>) -> std::sync::MutexGuard<'_, HashMap<u64, Held>> {
    shells.lock().unwrap_or_else(PoisonError::into_inner)
}
