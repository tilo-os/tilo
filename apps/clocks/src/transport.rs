//! The session-private channel between Clocks and the shell: the app
//! publishes the clock floating above other windows, and the shell hands
//! back the controls pressed on it. The runtime directory is already
//! per-login; owner and mode checks on the shell side prevent forged status.

use std::io;

use serde::{Deserialize, Serialize};
use tilo_files::{Dir, VPath, blocking};

/// The floating clock as the shell reads it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "mirrors the JSON wire format the shell reads; each flag is independent"
)]
pub struct ShellSnapshot {
    pub version: u8,
    pub active: bool,
    /// The timer shown; `None` is the stopwatch.
    #[serde(default)]
    pub activity_id: Option<String>,
    pub label: String,
    pub remaining_secs: u64,
    pub stopwatch: bool,
    #[serde(default)]
    pub paused: bool,
    pub overlay: bool,
    pub generation: u64,
    pub producer_pid: u32,
    /// Stable identity for the latest due event. It is deliberately separate
    /// from `generation`: live snapshots are refreshed often, while a due
    /// event must be announced exactly once even if the shell observes it
    /// after the timer has already left the active set.
    #[serde(default)]
    pub notification_id: Option<String>,
    #[serde(default)]
    pub notification_title: String,
    #[serde(default)]
    pub notification_body: String,
}

/// A control pressed on the shell's floating clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellCommand {
    pub command: String,
    /// The timer it names; `None` for the stopwatch.
    pub activity_id: Option<String>,
}

/// `$XDG_RUNTIME_DIR/tilo`, which the shell reads and writes too.
fn runtime() -> io::Result<Dir> {
    let location = tilo_paths::AppPaths::for_app("tilo")
        .runtime()
        .map_err(|error| io::Error::new(io::ErrorKind::NotFound, error))?;
    Dir::open(location).map_err(io::Error::other)
}

/// Atomically writes `snapshot` to the private transport file.
///
/// # Errors
///
/// Fails when the session has no runtime directory, serialization fails, or
/// the file cannot be written.
pub fn publish(snapshot: &ShellSnapshot) -> io::Result<()> {
    let name = VPath::new("clocks.json").map_err(io::Error::other)?;
    blocking::write_private(&runtime()?, &name, &serde_json::to_vec(snapshot)?)
        .map_err(io::Error::other)
}

#[derive(Deserialize)]
struct Command {
    version: u8,
    #[expect(
        clippy::struct_field_names,
        reason = "the field name is the JSON key the shell writes"
    )]
    command: String,
    #[serde(default)]
    activity_id: Option<String>,
}

/// The control the shell handed over since the last call, if any.
#[must_use]
pub fn take_command() -> Option<ShellCommand> {
    let dir = runtime().ok()?;
    let name = VPath::new("clocks-command.json").ok()?;
    let bytes = blocking::read(&dir, &name).ok()?;
    let _ = blocking::remove(&dir, &name);
    if bytes.len() > 1024 {
        return None;
    }
    let command: Command = serde_json::from_slice(&bytes).ok()?;
    (command.version == 1).then_some(ShellCommand {
        command: command.command,
        activity_id: command.activity_id,
    })
}
