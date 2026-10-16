//! The clock Clocks keeps floating above other windows, as its live
//! activity over the app's channel: the shell presents it, and hands back
//! the controls pressed on it.

use serde::{Deserialize, Serialize};

/// The floating clock as the shell presents it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the activity the shell presents; each flag is independent"
)]
pub struct ShellSnapshot {
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
    /// Stable identity for the latest due event: a due event is announced
    /// exactly once even if the shell takes it in after the timer has
    /// already left the active set.
    #[serde(default)]
    pub notification_id: Option<String>,
    #[serde(default)]
    pub notification_title: String,
    #[serde(default)]
    pub notification_body: String,
}

/// A control pressed on the shell's floating clock.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ShellCommand {
    pub command: String,
    /// The timer it names; `None` for the stopwatch.
    #[serde(default)]
    pub activity_id: Option<String>,
}

/// Show `snapshot` as Clocks' live activity, or end it when there is nothing
/// running and nothing to announce.
pub fn publish(snapshot: &ShellSnapshot) {
    let shown = (snapshot.active || snapshot.notification_id.is_some())
        .then(|| serde_json::to_value(snapshot).ok())
        .flatten();
    ui::collaboration::channel_live_activity::show(shown);
}

/// The controls pressed on the floating clock since the last call.
#[must_use]
pub fn take_commands() -> Vec<ShellCommand> {
    ui::collaboration::channel_live_activity::take_controls()
        .into_iter()
        .filter_map(|control| serde_json::from_value(control).ok())
        .collect()
}
