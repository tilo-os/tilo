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
use tilo_actions::remote::native_media::{Request as CallMediaRequest, Snapshot as CallSnapshot};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeRequest, LineClient, ReceiptStatus,
    RemoteActionTarget,
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
                if self::line().set(client).is_err() {
                    tracing::warn!("the daemon handed this process a second line: it is dropped");
                }
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
