//! Running a workspace's shell commands on the member device that takes its
//! work (`docs/app-model/environments.md`, "Members that take work").
//!
//! A workspace whose host serves it has no device of the person's to run its
//! Terminal sessions. A member device with the take-work appetite does
//! instead: it holds the workspace and its files, and runs a session's
//! command line there, a Linux program in the workspace's own VM when it
//! needs one. The host asks it over the device action service, as the device it is,
//! the way any device of a workspace asks another:
//!
//! - [`target`] names the run, an action of the workspace's environment
//!   ([`RESOURCE`]); the argument is a [`CommandRequest`].
//! - While the command runs, each [`InvocationUpdate::Item`](crate::InvocationUpdate)
//!   is a [`CommandOutput`]: everything printed so far, up to its last
//!   [`LIVE_TAIL`] bytes, and how much that is in all, so a follower that
//!   missed an update shows what is there now and never a gap.
//! - It succeeds with a [`CommandRan`], whatever the command's exit status:
//!   the run happened. A refusal the program needed a VM for comes back typed,
//!   for the shell's words to the person or agent who ran it.
//! - It can be stopped: a cancel of the invocation ends the command in the
//!   session's shell, and the [`CommandRan`] says it was
//!   ([`cancelled`](CommandRan::cancelled)).
//!
//! - A command may carry secrets beside its command line ([`VmSecret`]): items of
//!   the workspace's vault its VM may use, resolved by the device holding the workspace
//!   for this command.
//!   Each reaches the environment of every program the command starts in the
//!   VM, and nothing else: not the shell's own variables, not the command
//!   line, not the command's answer.
//!
//! Only the workspace's host may ask, as a device of the operator the
//! member's own identity belongs to: a run is paid for by the account the
//! request names, which only the host knows.

use serde::{Deserialize, Serialize};

use super::RemoteActionTarget;
use super::server_account::Refusal;
use crate::api::ActionAddress;

/// The provider the taking member answers runs under.
pub const PROVIDER: &str = "tilo.environment";
/// The resource a run is an action of: the workspace's environment.
pub const RESOURCE: &str = "environment";
/// Run one command line in a session's shell.
pub const RUN: &str = "run";

/// The record's verbs an app, agent or script may ask of a workspace's environment, as the
/// published surface names them: everything the record offers except `make` (the runtime makes a
/// record with the runtime of the one it copies, so nothing asks it of an existing one) and
/// `type` (the outbox's own read of what it queued, not a verb of the environment). The shell
/// runtime's supervisor applies exactly these to a record, each by its own rules (`run` above is
/// the one verb answered by the member that takes the workspace's Linux work).
pub const APP_ACTIONS: &[&str] = &[
    "add-tool",
    "remove-tool",
    "grant-dir",
    "set-home",
    "allow-host",
    "revoke-grant",
    "set-runtime",
    "rename",
    "copy",
    "move",
    "remove",
    "make-dir",
    "remove-dir",
    "link",
    "truncate",
    "touch",
    "append",
    "write",
    "restore",
    "revert",
];

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
pub struct CommandRequest {
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
    /// What the programs the command starts in the VM get in their
    /// environment besides the shell's variables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<VmSecret>,
    /// Who the command runs as: the person or agent the host runs it for,
    /// when it names them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<Person>,
}

/// One secret a program in the workspace's VM starts with: an environment
/// variable whose value a vault item keeps. It prints nothing of the value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmSecret {
    /// The variable's name (`GITHUB_TOKEN`).
    pub variable: String,
    /// What it is set to.
    pub value: String,
}

impl std::fmt::Debug for VmSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VmSecret")
            .field("variable", &self.variable)
            .finish_non_exhaustive()
    }
}

/// Who a command runs as in a workspace's VM: an agent's or a person's name,
/// as the workspace names them, and their address there. A program that
/// records who made a change — git's author and committer — records them, so
/// one VM and one shell serving a workspace's people and agents in turn never
/// put one actor's name on another's work.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub name: String,
    pub address: String,
}

impl Person {
    /// The actor called `name` in the workspace whose host is `host`: their
    /// address there is what their name makes at the host, `<name>@<host>`.
    /// An actor the workspace has not named is a Tilo member, and a host with
    /// no name of its own is `tilo`, so a program that signs its work is never
    /// refused an identity and never signs it with a key.
    #[must_use]
    pub fn of(name: &str, host: &str) -> Self {
        let name = name.trim();
        let name = if name.is_empty() {
            UNNAMED_ACTOR.to_owned()
        } else {
            name.to_owned()
        };
        // A name written in a script that makes no address leaves a plain
        // local part: the address is for reading, not for finding anyone.
        let local = slug(&name, &['.', '_']);
        let local = if local.is_empty() {
            "member".to_owned()
        } else {
            local
        };
        let host = slug(host_name(host), &['.']);
        let host = if host.is_empty() {
            "tilo".to_owned()
        } else {
            host
        };
        Self {
            name,
            address: format!("{local}@{host}"),
        }
    }
}

/// What someone the workspace has not named signs as: a person's word, never a
/// key, since what a program records of its author is read by people.
const UNNAMED_ACTOR: &str = "Tilo member";

/// The host part of `host`, which may be written as a URL (`https://eu1.tilo.sh/`,
/// `http://eu1.tilo.sh:8080`).
fn host_name(host: &str) -> &str {
    let host = host.trim();
    let host = host.split_once("://").map_or(host, |(_, rest)| rest);
    let host = host.split('/').next().unwrap_or(host);
    match host.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    }
}

/// `text` as the part of an address: lowercased, its words joined by `-`,
/// only letters, digits and `keep` left (up to 63 characters).
fn slug(text: &str, keep: &[char]) -> String {
    let mut out = String::new();
    for c in text.trim().chars().flat_map(char::to_lowercase) {
        if out.len() >= 63 {
            break;
        }
        if c.is_ascii_alphanumeric() || keep.contains(&c) {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

/// What a running command printed so far.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutput {
    /// How many bytes it printed in all.
    pub total: u64,
    /// The last of them, up to [`LIVE_TAIL`], as text.
    pub tail: String,
}

impl CommandOutput {
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
    /// It was stopped before it ended: the person's Ctrl+C, or the agent's
    /// cancel of its invocation. What it printed until then is all here.
    #[serde(default)]
    pub cancelled: bool,
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
    fn a_secret_prints_its_name_and_never_its_value() {
        let secret = VmSecret {
            variable: "GITHUB_TOKEN".into(),
            value: "sk-the-value".into(),
        };
        let printed = format!("{secret:?}");
        assert!(printed.contains("GITHUB_TOKEN"), "{printed}");
        assert!(!printed.contains("sk-the-value"), "{printed}");
    }

    #[test]
    fn live_output_keeps_the_newest_bytes_and_counts_them_all() {
        let printed = "x".repeat(LIVE_TAIL + 10) + "é end";
        let live = CommandOutput::of(printed.as_bytes());
        assert_eq!(live.total, u64::try_from(printed.len()).unwrap());
        assert!(live.tail.ends_with("é end"));
        assert!(
            live.tail.len() <= LIVE_TAIL + 3,
            "a cut character is replaced, not kept"
        );
    }

    #[test]
    fn a_person_is_addressed_by_their_name_at_their_workspaces_host() {
        let ada = Person::of("Ada Lovelace", "https://eu1.tilo.sh/");
        assert_eq!(ada.name, "Ada Lovelace");
        assert_eq!(ada.address, "ada-lovelace@eu1.tilo.sh");
        let with_port = Person::of("Ada", "http://eu1.tilo.sh:8080");
        assert_eq!(with_port.address, "ada@eu1.tilo.sh");
        // A person the workspace has not named signs as a member, never as a
        // key, and a host with no name is the default one: a program is never
        // without an identity to sign its work with.
        let unnamed = Person::of("  ", "");
        assert_eq!(unnamed.name, "Tilo member");
        assert_eq!(unnamed.address, "tilo-member@tilo");
        let odd = Person::of("Ada  O'Neil!!", "Ada's server");
        assert_eq!(odd.address, "ada-o-neil@ada-s-server");
        // A name in a script that spells nothing of an address still signs.
        let written = Person::of("Мария", "team.tilo");
        assert_eq!(written.name, "Мария");
        assert_eq!(written.address, "member@team.tilo");
    }
}
