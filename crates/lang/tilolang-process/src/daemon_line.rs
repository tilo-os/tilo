//! This process's connection to the device's daemon, when it runs one app's own instances.
//!
//! The process's sandbox holds its link and nothing else: no socket of the daemon's. The daemon
//! that started it hands it this line over the link (`ToProcess::Daemon`), and only when the
//! process runs one app's own instances, never a task's host or a page's window. It is the one
//! path from a tilolang app's process to the daemon, which serves it exactly as the connection of
//! a sandboxed app it started: as that app, bound to this process. Its Rust host calls on it what
//! a sandboxed app calls on its held connection for itself: keeping and reading back its own
//! secrets, and the key the device holds for it. Nothing the app's tilolang code holds reaches
//! it, and no answer of it reaches that code.

use std::sync::OnceLock;
use std::time::Duration;

use runtime_portal::process_link::Descriptor;
use serde_json::{Map, Value};
use tilo_action_services::LineClient;
use tilo_actions::remote::native_media::{Request as CallMediaRequest, Snapshot as CallSnapshot};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeRequest, ReceiptStatus, RemoteActionTarget,
};

/// How long a call on the line is waited for before it is given up on.
const ANSWERED_WITHIN: Duration = Duration::from_secs(30);

/// What a process the daemon handed no line is told.
const NO_LINE: &str = "This process has no connection to your computer's daemon: only the \
                       process that runs an app's own instances is handed one";

fn line() -> &'static OnceLock<LineClient> {
    static LINE: OnceLock<LineClient> = OnceLock::new();
    &LINE
}

/// Keep `line`, the connection the daemon handed this process, for its Rust host's calls: the
/// first one handed, which the daemon sends once.
pub fn keep(line: Descriptor) {
    #[cfg(unix)]
    {
        let stream = std::os::unix::net::UnixStream::from(line);
        match LineClient::of_stream(stream) {
            Ok(client) => {
                // The shells the app opens run at the device, which carries them on this line.
                #[cfg(feature = "app-launch")]
                let shells = shells_over(client.clone());
                if self::line().set(client).is_err() {
                    tracing::warn!("the daemon handed this process a second line: it is dropped");
                    return;
                }
                #[cfg(feature = "app-launch")]
                tilo_terminal::ops::shells_run_at(shells);
            }
            Err(error) => tracing::error!(%error, "the daemon's line does not read"),
        }
    }
    #[cfg(not(unix))]
    {
        drop(line);
        tracing::warn!("the daemon's line is not taken on this platform");
    }
}

/// Whether the daemon handed this process its line.
#[must_use]
pub fn held() -> bool {
    line().get().is_some()
}

/// Call `target` with `argument` on the daemon's line as this process's app, waiting for its
/// answer: the value it answered, or what refused it, in words a person reads.
///
/// # Errors
/// The process has no line, the daemon refused the call, or did not answer in time.
pub fn call(target: RemoteActionTarget, argument: Map<String, Value>) -> Result<Value, String> {
    let line = line().get().ok_or_else(|| NO_LINE.to_owned())?;
    // The daemon knows the caller by the line: the call names nobody.
    let caller = Caller::local_person("");
    let said = |error: ActionCallError| match error {
        ActionCallError::Refused { reason } => reason,
        other => other.to_string(),
    };
    let answer = line
        .invoke(&caller, InvokeRequest::plain(target, argument))
        .wait_timeout(ANSWERED_WITHIN)
        .map_err(|_| "Your computer's daemon did not answer in time".to_owned())?
        .ok_or_else(|| "Your computer's daemon did not answer".to_owned())?
        .map_err(said)?;
    match answer.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Failed { error } => Err(error.message),
        ReceiptStatus::Rejected { error } => Err(said(error)),
        ReceiptStatus::Cancelled { .. } => Err("The call was stopped".to_owned()),
        ReceiptStatus::Accepted => Err("Your computer's daemon did not answer".to_owned()),
    }
}

/// The key the device holds for this process's app's own `purpose`
/// (`tilo.vault` `vault#device-key`).
///
/// # Errors
/// As [`call`], and an answer that holds no key.
pub fn device_key(purpose: &str) -> Result<[u8; 32], String> {
    use tilo_actions::remote::{VAULT_DEVICE_KEY, vault_target};
    const NO_KEY: &str = "Your computer's daemon answered no key";
    let mut argument = Map::new();
    argument.insert("purpose".to_owned(), Value::String(purpose.to_owned()));
    let answered = call(vault_target(VAULT_DEVICE_KEY), argument)?;
    let digits = answered
        .get("key")
        .and_then(Value::as_str)
        .ok_or(NO_KEY)?
        .as_bytes();
    if digits.len() != 64 {
        return Err(NO_KEY.to_owned());
    }
    let mut bytes = [0_u8; 32];
    for (byte, pair) in bytes.iter_mut().zip(digits.chunks_exact(2)) {
        let text = std::str::from_utf8(pair).map_err(|_| NO_KEY.to_owned())?;
        *byte = u8::from_str_radix(text, 16).map_err(|_| NO_KEY.to_owned())?;
    }
    Ok(bytes)
}

/// Ask one command of the call this app's views take part in, without waiting for its answer.
/// The correlation it answers is what [`call_media_answers`] hands the answer back by, so a held
/// watch or picture collection never stops the frames this process runs.
///
/// # Errors
/// The process has no line, or it closed.
pub fn call_media_ask(request: CallMediaRequest) -> Result<u64, String> {
    line()
        .get()
        .ok_or_else(|| NO_LINE.to_owned())?
        .call_media_ask(request)
        .map_err(|error| error.own_words())
}

/// The answers to the call commands this process sent since the last time, each by the
/// correlation [`call_media_ask`] answered.
#[must_use]
pub fn call_media_answers() -> Vec<(u64, Result<CallSnapshot, ActionCallError>)> {
    line()
        .get()
        .map(LineClient::call_media_answers)
        .unwrap_or_default()
}

/// Say `said`, one word of this app's person in its workspace, on the daemon's line: the
/// device's live presence says it in the workspace's live session for this process's app, and
/// the workspace's other devices hear it.
///
/// # Errors
/// The process has no line, the device refused the word, or it did not answer in time.
pub fn say_presence(said: &collab_types::presence::Said) -> Result<(), String> {
    let line = line().get().ok_or_else(|| NO_LINE.to_owned())?;
    let said = serde_json::to_value(said).map_err(|error| error.to_string())?;
    line.say_presence(&said).map_err(|error| error.own_words())
}

/// What the device's live session heard in this process's workspace since the last look, oldest
/// first.
#[must_use]
pub fn presence_heard() -> Vec<collab_types::presence::Heard> {
    line()
        .get()
        .map(LineClient::presence_heard)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|heard| match serde_json::from_str(&heard) {
            Ok(heard) => Some(heard),
            Err(error) => {
                tracing::warn!(%error, "a presence word does not read");
                None
            }
        })
        .collect()
}

/// The shells an app's own process opens: the device opens each in its shell runtime, if the
/// app's signed manifest grants it shells, and carries it on this process's line. The process
/// never reaches the runtime itself.
#[cfg(feature = "app-launch")]
struct Shells(LineClient);

/// The shells an app's process opens at the device at the far end of `line`: what this process
/// opens once the daemon handed it its line.
#[cfg(feature = "app-launch")]
#[must_use]
pub fn shells_over(line: LineClient) -> std::sync::Arc<dyn tilo_terminal::ops::FarShells> {
    std::sync::Arc::new(Shells(line))
}

#[cfg(feature = "app-launch")]
impl tilo_terminal::ops::FarShells for Shells {
    fn open(
        &self,
        open: &tilo_terminal::ops::FarOpen<'_>,
    ) -> Result<tilo_terminal::FarSide, String> {
        use std::sync::{Arc, Mutex};
        use tilo_actions::remote::shell::ShellOpen;
        let line = self.0.clone();
        let (opened, heard) = line
            .open_shell(ShellOpen {
                target: open.target.clone(),
                cols: open.columns,
                rows: open.rows,
                agent: open.agent,
                command: open.program.to_vec(),
                relay_session: open.relay_session.map(str::to_owned),
                record: open.record.map(str::to_owned),
                commands: Some(open.commands.clone()),
            })
            .map_err(|error| match error {
                ActionCallError::Refused { reason } | ActionCallError::Unavailable { reason } => {
                    reason
                }
                other => other.to_string(),
            })?;
        let shell = opened.shell;
        let ended = Arc::new(Mutex::new(None));
        let (resizing, closing) = (line.clone(), line.clone());
        Ok(tilo_terminal::FarSide {
            writer: Box::new(Typed { line, shell }),
            reader: Box::new(Printed {
                heard,
                ended: Arc::clone(&ended),
                pending: Vec::new(),
                at: 0,
            }),
            resize: Box::new(move |cols, rows| {
                let _ = resizing.resize_shell(shell, cols, rows);
            }),
            close: Box::new(move || {
                let _ = closing.close_shell(shell);
            }),
            shell_name: opened.name,
            ended,
            relay_session: opened.relay_session,
            commands: opened.commands.then(|| open.commands.clone()),
        })
    }
}

/// What the grid types into a shell the device carries.
#[cfg(feature = "app-launch")]
struct Typed {
    line: LineClient,
    shell: u64,
}

#[cfg(feature = "app-launch")]
impl std::io::Write for Typed {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.line
            .type_into_shell(self.shell, bytes)
            .map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, error.to_string())
            })?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// What a shell the device carries printed, until it ended: its end is told before its output
/// stops, so a lost one is never taken for a shell that exited.
#[cfg(feature = "app-launch")]
struct Printed {
    heard: std::sync::mpsc::Receiver<tilo_action_services::ShellHeard>,
    ended: std::sync::Arc<std::sync::Mutex<Option<tilo_terminal::SessionEnd>>>,
    pending: Vec<u8>,
    at: usize,
}

#[cfg(feature = "app-launch")]
impl std::io::Read for Printed {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        use tilo_action_services::ShellHeard;
        while self.at >= self.pending.len() {
            match self.heard.recv() {
                Ok(ShellHeard::Output(bytes)) => {
                    self.pending = bytes;
                    self.at = 0;
                }
                Ok(ShellHeard::Ended(report)) => {
                    if !report.is_empty() {
                        *self
                            .ended
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some(tilo_terminal::SessionEnd::from_report(report.as_bytes()));
                    }
                    return Ok(0);
                }
                // Let go here: the session closed it.
                Err(_) => return Ok(0),
            }
        }
        let taken = (self.pending.len() - self.at).min(buffer.len());
        buffer[..taken].copy_from_slice(&self.pending[self.at..self.at + taken]);
        self.at += taken;
        Ok(taken)
    }
}
