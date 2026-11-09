//! Running a workspace's shell commands on the member device that takes its
//! work (`docs/app-model/environments.md`, "Members that take work").
//!
//! A workspace whose host serves it has no device of the person's to run its
//! Terminal sessions. A member device with the take-work appetite does
//! instead: it holds the workspace and its files, and runs a session's
//! command line there, a Linux program in the workspace's own VM when it
//! needs one. The host asks it over the device plane, as the device it is,
//! the way any device of a workspace asks another:
//!
//! - [`target`] names the run, an action of the workspace's environment
//!   ([`RESOURCE`]); the argument is a [`RunCommand`].
//! - While the command runs, each [`InvocationUpdate::Item`](crate::InvocationUpdate)
//!   is a [`RunOutput`]: everything printed so far, up to its last
//!   [`LIVE_TAIL`] bytes, and how much that is in all, so a follower that
//!   missed an update shows what is there now and never a gap.
//! - It succeeds with a [`CommandRan`], whatever the command's exit status:
//!   the run happened. A refusal the program needed a VM for comes back typed,
//!   for the shell's words to the person or agent who ran it.
//!
//! Only the workspace's host may ask, as a device of the operator the
//! member's own identity belongs to: a run is paid for by the account the
//! request names, which only the host knows.

use serde::{Deserialize, Serialize};

use super::RemoteActionTarget;
use super::host_account::Refusal;
use crate::door::ActionAddress;

/// The provider the taking member answers runs under.
pub const PROVIDER: &str = "tilo.environment";
/// The resource a run is an action of: the workspace's environment.
pub const RESOURCE: &str = "environment";
/// Run one command line in a session's shell.
pub const RUN: &str = "run";

/// The most of a running command's output one live update carries.
pub const LIVE_TAIL: usize = 32 * 1024;

/// What [`RUN`] is invoked on.
#[must_use]
pub fn target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, RUN),
    }
}

/// One command line to run in one session's shell of a workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCommand {
    /// The session whose shell runs it: its directory and variables carry
    /// over from the session's earlier commands.
    pub session: String,
    pub command: String,
    /// Render for an agent: plain words, no colour, no prompts.
    #[serde(default)]
    pub agent: bool,
    /// Who pays for the workspace's VM, as the host knows it.
    pub account: String,
    /// The size of the workspace's VM (`small`, `default`, `large`, `xlarge`).
    pub size: String,
}

/// What a running command printed so far.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOutput {
    /// How many bytes it printed in all.
    pub total: u64,
    /// The last of them, up to [`LIVE_TAIL`], as text.
    pub tail: String,
}

impl RunOutput {
    /// What `printed` shows a follower: its length and its last
    /// [`LIVE_TAIL`] bytes, cut at a character boundary.
    #[must_use]
    pub fn of(printed: &[u8]) -> Self {
        let from = printed.len().saturating_sub(LIVE_TAIL);
        Self {
            total: u64::try_from(printed.len()).unwrap_or(u64::MAX),
            tail: String::from_utf8_lossy(printed.get(from..).unwrap_or_default()).into_owned(),
        }
    }
}

/// How one command line went.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRan {
    /// The command line, as run.
    pub command: String,
    /// The directory it ran in.
    pub cwd: String,
    /// Its exit status: 126 when the host refused what it needed.
    pub exit: i32,
    /// What it printed, standard output and error as written.
    pub output: String,
    /// It printed more than the run keeps.
    #[serde(default)]
    pub truncated: bool,
    pub started_ms: i64,
    pub ended_ms: i64,
    /// Why the host would not run a program it needed, typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_output_keeps_the_newest_bytes_and_counts_them_all() {
        let printed = "x".repeat(LIVE_TAIL + 10) + "é end";
        let live = RunOutput::of(printed.as_bytes());
        assert_eq!(live.total, u64::try_from(printed.len()).unwrap());
        assert!(live.tail.ends_with("é end"));
        assert!(
            live.tail.len() <= LIVE_TAIL + 3,
            "a cut character is replaced, not kept"
        );
    }
}
